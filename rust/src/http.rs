use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures::FutureExt;
use serde_json::Value;
use sqlx::PgPool;

use crate::protocol::{self, JsonRpcRequest};
use crate::session::Scope;

pub const DEFAULT_ADDR: &str = "127.0.0.1:8787";

const MAX_BODY: usize = 8 * 1024 * 1024;

const MAX_BATCH_ITEMS: usize = 256;

const CLIENT_TTL: Duration = Duration::from_secs(24 * 3600);
const REAP_INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    token: Option<Arc<String>>,
    peer_token: Option<Arc<String>>,
    started: Instant,
    served: Arc<AtomicU64>,
    seen: Arc<std::sync::RwLock<std::collections::HashMap<String, Instant>>>,
    last_activity: Arc<Mutex<Instant>>,
    /// False until the models finished loading. The port opens before that is
    /// true only when warming ran past its budget, and `/health` says so.
    ready: Arc<std::sync::atomic::AtomicBool>,
    /// The port this daemon bound, so an Origin can be checked against it.
    port: u16,
    /// Failed bearer tokens, per address.
    auth_failures:
        Arc<std::sync::RwLock<std::collections::HashMap<std::net::IpAddr, (u32, Instant)>>>,
    /// Which resource plan this process started under.
    ///
    /// Read once at boot instead of per request: `resources::probe()` shells
    /// out to `nvidia-smi` on a GPU build, and `/health` is what a monitor
    /// polls every few seconds. The answer cannot change anyway — `main`
    /// computes the plan once and `apply()` has already written it into the
    /// environment by the time this daemon binds.
    resource_tier: &'static str,
}

pub fn bind_addr() -> String {
    std::env::var("CUBA_HTTP_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string())
}

fn idle_shutdown_after() -> Option<Duration> {
    let secs: u64 = std::env::var("CUBA_IDLE_SHUTDOWN_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    (secs > 0).then(|| Duration::from_secs(secs))
}

#[cfg(unix)]
fn systemd_listener() -> Option<std::net::TcpListener> {
    use std::os::fd::{FromRawFd, RawFd};

    let pid: u32 = std::env::var("LISTEN_PID").ok()?.parse().ok()?;
    if pid != std::process::id() {
        return None;
    }
    let fds: u32 = std::env::var("LISTEN_FDS").ok()?.parse().ok()?;
    if fds == 0 {
        return None;
    }
    const SD_LISTEN_FDS_START: RawFd = 3;
    let listener = unsafe { std::net::TcpListener::from_raw_fd(SD_LISTEN_FDS_START) };
    listener.set_nonblocking(true).ok()?;
    Some(listener)
}

#[cfg(not(unix))]
fn systemd_listener() -> Option<std::net::TcpListener> {
    None
}

fn auth_token() -> Option<String> {
    std::env::var("CUBA_HTTP_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
}

fn peer_token() -> Option<String> {
    std::env::var("CUBA_PEER_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
}

fn ensure_tokens_differ() -> Result<()> {
    match (auth_token(), peer_token()) {
        (Some(admin), Some(peer)) if admin == peer => anyhow::bail!(
            "CUBA_PEER_TOKEN is the same string as CUBA_HTTP_TOKEN, so the restricted token \
             is not restricted: it matches the admin arm first and gets all 31 tools. The \
             point of a peer token is that handing it to the other machine — and to the \
             Cloudflare tunnel, which uses CUBA_HTTP_TOKEN — are different acts"
        ),
        _ => Ok(()),
    }
}

/// The shortest bearer token worth calling a secret on a routable address.
/// 32 characters of base64 is 192 bits; the point is only that it is not in
/// anybody's wordlist.
pub(crate) const MIN_ROUTABLE_TOKEN_CHARS: usize = 32;

/// Why this token is not good enough for this address, if it is not.
///
/// On loopback the token is a convenience and anything goes: whoever is asking
/// already has the machine. On an address other machines can reach it is the
/// only thing between the graph and anyone who can route a packet to the port,
/// and `same_secret` compares in constant time but nothing stops an attacker
/// on the LAN trying tokens as fast as the daemon answers.
/// `pub(crate)`, not `pub`: `service.rs` is in this crate and needs to agree
/// with this rule token for token, and there is no reason to widen the public
/// API for it.
pub(crate) fn token_too_weak(addr_is_loopback: bool, token: Option<&str>) -> Option<String> {
    if addr_is_loopback {
        return None;
    }
    let token = token?;
    if token.chars().count() >= MIN_ROUTABLE_TOKEN_CHARS {
        return None;
    }
    Some(format!(
        "CUBA_HTTP_TOKEN is {} characters. On an address other machines can reach it is the only thing standing between them and the entire graph, so it needs at least {MIN_ROUTABLE_TOKEN_CHARS}. Generate one: head -c 32 /dev/urandom | base64",
        token.chars().count()
    ))
}

fn ensure_loopback(addr: &SocketAddr) -> Result<()> {
    if let Some(why) = token_too_weak(addr.ip().is_loopback(), auth_token().as_deref()) {
        anyhow::bail!("refusing to bind {addr}: {why}");
    }
    if addr.ip().is_loopback() || auth_token().is_some() {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to bind {addr}: the daemon serves the whole brain with no auth. \
         Use a 127.0.0.1 address, or set CUBA_HTTP_TOKEN to require a bearer token"
    )
}

fn ensure_adopted_loopback(addr: &SocketAddr) -> Result<()> {
    if let Some(why) = token_too_weak(addr.ip().is_loopback(), auth_token().as_deref()) {
        anyhow::bail!("refusing the socket systemd handed over on {addr}: {why}");
    }
    if addr.ip().is_loopback() || auth_token().is_some() {
        return Ok(());
    }
    anyhow::bail!(
        "refusing the socket systemd handed over on {addr}: it is not loopback and \
         CUBA_HTTP_TOKEN is unset, so the whole brain would be readable and writable \
         from every interface. With socket activation the .socket unit picks the \
         address and CUBA_HTTP_ADDR is ignored — set ListenStream=127.0.0.1:8787 in \
         memory-industry.socket, or set CUBA_HTTP_TOKEN in the service unit"
    )
}

pub async fn serve(addr: &str) -> Result<()> {
    let database_url = crate::setup::resolve_database_url().await;
    let (pool, connected) = match crate::db::create_pool(&database_url).await {
        Ok(pool) => {
            crate::db::assert_embedding_dim(&pool).await?;
            (pool, true)
        }
        Err(why) => {
            tracing::warn!(
                error = %format!("{why:#}"),
                "starting without PostgreSQL — tools will fail until it is reachable"
            );
            (crate::db::create_lazy_pool(&database_url), false)
        }
    };
    serve_pool(addr, pool, connected).await
}

pub async fn serve_pool(addr: &str, pool: PgPool, connected: bool) -> Result<()> {
    let addr: SocketAddr = addr
        .parse()
        .with_context(|| format!("invalid listen address: {addr}"))?;
    ensure_loopback(&addr)?;
    ensure_tokens_differ()?;

    crate::session::enable_daemon_mode();

    if connected {
        let rem_pool = pool.clone();
        tokio::spawn(async move { protocol::rem_daemon(rem_pool).await });

        let listen_pool = pool.clone();
        let listen_url = crate::setup::resolve_database_url().await;
        tokio::spawn(async move { protocol::sync_listener(listen_pool, listen_url).await });
    }

    let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let state = AppState {
        pool,
        token: auth_token().map(Arc::new),
        peer_token: peer_token().map(Arc::new),
        started: Instant::now(),
        served: Arc::new(AtomicU64::new(0)),
        seen: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        last_activity: Arc::new(Mutex::new(Instant::now())),
        ready: ready.clone(),
        port: addr.port(),
        auth_failures: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        resource_tier: crate::resources::plan(&crate::resources::probe())
            .tier
            .as_str(),
    };

    let reaper_seen = state.seen.clone();
    tokio::spawn(async move { reap_idle_clients(reaper_seen).await });

    let idle_shutdown = Arc::new(tokio::sync::Notify::new());
    if let Some(idle_after) = idle_shutdown_after() {
        let last_activity = state.last_activity.clone();
        let notify = idle_shutdown.clone();
        tokio::spawn(async move { shutdown_when_idle(last_activity, idle_after, notify).await });
    }

    let mut app = Router::new()
        .route("/mcp", post(mcp_endpoint))
        .route("/health", get(health))
        .route("/", get(connect_page))
        .route("/connect", get(connect_page))
        .route("/events/ticket", post(events_ticket));
    if panel_route_enabled(
        addr.ip().is_loopback(),
        panel_enabled(),
        panel_allows_forwarded(),
    ) {
        tracing::info!("control panel at http://{addr}/panel");
        app = app.route("/panel", get(panel));
    } else if panel_enabled() {
        tracing::warn!(
            %addr,
            "CUBA_PANEL=1 pero esta dirección es alcanzable desde otras máquinas: /panel NO se registra. El panel muestra estado, clientes conectados y llamadas recientes. Si de verdad lo querés publicado, CUBA_PANEL_PUBLIC=1"
        );
    }
    tracing::info!("connect page at http://{addr}/connect");

    // The timeout is attached here, before `/events` is merged in, because
    // `Router::layer` only wraps the routes already registered. An SSE stream
    // is a response that deliberately never ends: any budget on it is a
    // guarantee that every subscriber is cut off at the budget, and the bell
    // would look flaky instead of looking absent.
    let app = app
        .layer(axum::middleware::from_fn(bound_every_request))
        .merge(Router::new().route("/events", get(events_sse)))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(state);

    let listener = bind_listener(addr).await?;

    // Bind first: a port already taken, a non-loopback address without a token
    // or a weak one all fail here, in a second, instead of after two minutes of
    // loading. Then warm, and only then say the daemon is listening.
    //
    // Warming before the bind was the other obvious order and it is worse: the
    // port stays shut while the models load, so clients get ECONNREFUSED and
    // mark the server dead, and a warm-up that hangs means the daemon never
    // binds at all and cannot be asked why.
    //
    // Between bind and serve the kernel queues connections, so a client that
    // arrives early waits and then gets a real answer rather than a 200 with an
    // unreranked ranking in it.
    warm_before_serving(ready.clone()).await;

    tracing::info!(
        %addr,
        auth = auth_token().is_some(),
        ready = ready.load(Ordering::Relaxed),
        "MemoryIndustry daemon listening — point clients at http://{addr}/mcp (connect: /connect)"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(idle_shutdown))
    .await
    .context("http server failed")?;

    tracing::info!("daemon shut down");
    Ok(())
}

/// Remember that this client is alive, for the idle reaper and for /health.
fn note_activity(state: &AppState, key: &str) {
    if let Ok(mut guard) = state.seen.write() {
        guard.insert(key.to_string(), Instant::now());
    }
    if let Ok(mut guard) = state.last_activity.lock() {
        *guard = Instant::now();
    }
}

/// The socket, either handed over by systemd or opened here.
///
/// Kept first in `serve_pool` on purpose: a port already taken, a routable
/// address with no token and a token too weak to be one all fail here in a
/// second, rather than after two minutes of loading models.
async fn bind_listener(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    let Some(std_listener) = systemd_listener() else {
        return tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot bind {addr} — is another daemon already running?"));
    };
    let adopted = std_listener
        .local_addr()
        .context("cannot read the address of the systemd-activated socket")?;
    ensure_adopted_loopback(&adopted)?;
    tokio::net::TcpListener::from_std(std_listener)
        .context("failed to adopt systemd-activated socket")
}

/// Load the models, but do not wait forever for them.
///
/// Dropping the handle on timeout detaches the task: it keeps loading and
/// flips `ready` when it lands. Serving before it does is the lesser evil,
/// because a daemon that never opens its port cannot even be asked what it is
/// doing.
async fn warm_before_serving(ready: Arc<std::sync::atomic::AtomicBool>) {
    let warming = tokio::spawn(async move {
        let started = Instant::now();
        warm_models().await;
        ready.store(true, Ordering::Relaxed);
        tracing::info!(secs = started.elapsed().as_secs_f32(), "models warm");
    });
    let budget = warm_before_serve_budget();
    if tokio::time::timeout(budget, warming).await.is_err() {
        tracing::warn!(
            secs = budget.as_secs(),
            "los modelos no terminaron de calentar dentro del presupuesto — se sirve igual, /health dice ready:false y las búsquedas con rerank salen marcadas como degradadas"
        );
    }
}

async fn shutdown_signal(idle: Arc<tokio::sync::Notify>) {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "cannot install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("SIGINT received"),
        _ = terminate => tracing::info!("SIGTERM received"),
        _ = idle.notified() => tracing::info!("idle shutdown requested"),
    }
}

/// How long the daemon waits for its models before it opens for business
/// anyway. Long enough for a cold cross-encoder, short enough that a broken
/// model does not leave the port shut with nobody able to ask why.
fn warm_before_serve_budget() -> std::time::Duration {
    let secs = crate::envs::alias(
        "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
        "CUBA_WARM_BEFORE_SERVE_SECS",
    )
    .ok()
    .and_then(|raw| raw.trim().parse().ok())
    .unwrap_or(180);
    std::time::Duration::from_secs(secs)
}

/// Opt-out, not opt-in.
///
/// Deferring the load does not save the cost, it moves it inside the first
/// search that asks for reranking — a request with a 20 s budget paying for a
/// 1.1 GB read. The daemon has time at startup and the search does not.
fn warm_reranker_eagerly() -> bool {
    warm_eagerly_from(
        crate::envs::alias("MEMORY_INDUSTRY_WARM_RERANKER", "CUBA_WARM_RERANKER")
            .ok()
            .as_deref(),
    )
}

fn warm_eagerly_from(raw: Option<&str>) -> bool {
    !matches!(raw, Some("0") | Some("off") | Some("false") | Some("no"))
}

async fn warm_models() {
    if crate::search::rerank::is_configured() {
        if !warm_reranker_eagerly() {
            tracing::info!(
                "reranker deferred — loads on its first batch (CUBA_WARM_RERANKER=1 to preload)"
            );
        } else if crate::search::rerank::warm_up().await {
            tracing::info!("reranker warm");
        } else {
            tracing::warn!("reranker configured but failed to warm up — identity fallback");
        }
    }
    match crate::embeddings::onnx::embed("warm up").await {
        Ok(_) => tracing::info!(
            model = %crate::embeddings::onnx::current_model(),
            loaded = crate::embeddings::onnx::is_model_loaded(),
            "embedding model warm"
        ),
        Err(e) => tracing::warn!(error = %format!("{e:#}"), "embedding warm-up failed"),
    }
}

async fn shutdown_when_idle(
    last_activity: Arc<Mutex<Instant>>,
    idle_after: Duration,
    notify: Arc<tokio::sync::Notify>,
) {
    const CHECK_INTERVAL: Duration = Duration::from_secs(30);
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    loop {
        ticker.tick().await;
        let elapsed = last_activity
            .lock()
            .map(|guard| guard.elapsed())
            .unwrap_or_default();
        if elapsed >= idle_after {
            tracing::info!(
                idle_secs = elapsed.as_secs(),
                "idle timeout — shutting down"
            );
            notify.notify_one();
            return;
        }
    }
}

async fn reap_idle_clients(
    seen: Arc<std::sync::RwLock<std::collections::HashMap<String, Instant>>>,
) {
    let mut ticker = tokio::time::interval(REAP_INTERVAL);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let stale: Vec<String> = match seen.read() {
            Ok(guard) => guard
                .iter()
                .filter(|(_, last)| last.elapsed() > CLIENT_TTL)
                .map(|(k, _)| k.clone())
                .collect(),
            Err(_) => continue,
        };
        if stale.is_empty() {
            continue;
        }
        if let Ok(mut guard) = seen.write() {
            for key in &stale {
                guard.remove(key);
            }
        }
        for key in &stale {
            crate::session::forget_client(key);
        }
        tracing::info!(count = stale.len(), "reaped idle clients");
    }
}

fn client_key(headers: &HeaderMap, payload: &Value) -> (String, bool) {
    if let Some(id) = headers
        .get("mcp-client-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return (id.to_string(), true);
    }

    let first = payload
        .as_array()
        .and_then(|a| a.first())
        .unwrap_or(payload);
    let params = first.get("params");

    if let Some(id) = params
        .and_then(|p| p.get("_meta"))
        .and_then(|m| {
            m.get("io.modelcontextprotocol/client-id")
                .or_else(|| m.get("clientId"))
        })
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return (id.to_string(), true);
    }

    if let Some(name) = params
        .and_then(|p| p.get("clientInfo"))
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return (name.to_string(), false);
    }

    ("anonymous".to_string(), false)
}

fn same_secret(presented: &str, expected: &str) -> bool {
    let a = presented.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(state: &AppState, headers: &HeaderMap) -> Option<Scope> {
    let presented = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    if let Some(peer) = state.peer_token.as_ref()
        && same_secret(presented, peer)
    {
        return Some(Scope::Peer);
    }

    match state.token.as_ref() {
        None => Some(Scope::Full),
        Some(expected) if same_secret(presented, expected) => Some(Scope::Full),
        Some(_) => None,
    }
}

fn request_deadline() -> Duration {
    protocol::handler_timeout() * 4
}

/// The last resort for a request that stops making progress.
///
/// Strictly longer than `request_deadline()` on purpose. `/mcp` already bounds
/// its own dispatch and answers with a JSON-RPC envelope that names what died;
/// that answer is worth more than this one, so this layer must not fire first
/// and take it away. What it covers is everything `/mcp` cannot: a request
/// that never reaches a handler, and the routes that have no budget of their
/// own (`/panel`, `/connect`, `/health`), which on a LAN bind are reachable by
/// anything that can route a packet to the port.
fn router_deadline() -> Duration {
    request_deadline() * 2
}

/// Give up on a request that outlived its budget, and free the connection.
///
/// `/events` is deliberately not behind this — see where the layer is
/// attached.
async fn bound_every_request(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let budget = router_deadline();
    let path = request.uri().path().to_string();
    match tokio::time::timeout(budget, next.run(request)).await {
        Ok(response) => response,
        Err(_) => {
            tracing::warn!(
                path = %path,
                secs = budget.as_secs(),
                "request outlived the router deadline and was dropped"
            );
            (
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "this daemon gave up on the request after {}s and closed it",
                    budget.as_secs()
                ),
            )
                .into_response()
        }
    }
}

fn batch_items(payload: Value) -> Result<(Vec<Value>, bool), Value> {
    match payload {
        Value::Array(items) if items.len() > MAX_BATCH_ITEMS => Err(error_envelope(
            Value::Null,
            -32600,
            format!(
                "batch carries {} requests; this daemon dispatches at most {MAX_BATCH_ITEMS} \
                 per POST. Split it — a truncated batch would look answered",
                items.len()
            ),
        )),
        Value::Array(items) => Ok((items, true)),
        single => Ok((vec![single], false)),
    }
}

const ROOTS_REQUEST_ID: &str = "cuba_roots";

#[cfg(test)]
fn asks_for_roots(items: &[Value]) -> bool {
    items.iter().any(|item| {
        item.get("method").and_then(Value::as_str) == Some("initialize")
            && item
                .get("params")
                .and_then(|p| p.get("capabilities"))
                .and_then(|c| c.get("roots"))
                .is_some()
    })
}

fn first_root_uri(item: &Value) -> Option<&str> {
    if item.get("id").and_then(Value::as_str) != Some(ROOTS_REQUEST_ID) {
        return None;
    }
    item.get("result")?
        .get("roots")?
        .as_array()?
        .first()?
        .get("uri")?
        .as_str()
}

async fn adopt_client_root(pool: &PgPool, key: &str, uri: &str) {
    let Some(name) = crate::session::root_to_project_name(uri) else {
        return;
    };
    match crate::project::upsert_project(pool, &name).await {
        Ok(id) => {
            crate::session::remember_client_root(key, id);
            tracing::info!(client = %key, project = %name, "adopted the client's root as its project");
        }
        Err(why) => {
            tracing::warn!(client = %key, project = %name, error = %why, "could not adopt the client's root")
        }
    }
}

fn error_envelope(id: Value, code: i64, message: impl Into<String>) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn panel_enabled() -> bool {
    std::env::var("CUBA_PANEL").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Whether to register `/panel` at all, given where the daemon is listening.
///
/// The panel has one existing guard, `came_through_a_proxy`, and its own
/// documentation says what it does not catch: a raw TCP forward adds no header
/// and looks exactly like a local request. A LAN bind is not a proxy either.
/// So on a routable address, `CUBA_PANEL=1` alone published a page that reads
/// the daemon's state, its connected clients and its recent calls to anybody
/// who could reach the port.
///
/// It is not registered there unless somebody says out loud that they meant
/// it. Refusing to serve a route is stronger than refusing a request: there is
/// no header to forge and no check to get wrong.
fn panel_route_enabled(addr_is_loopback: bool, enabled: bool, public: bool) -> bool {
    enabled && (addr_is_loopback || public)
}

fn panel_allows_forwarded() -> bool {
    std::env::var("CUBA_PANEL_PUBLIC").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

pub const FORWARDING_HEADERS: [&str; 9] = [
    "forwarded",
    "cf-connecting-ip",
    "cf-ray",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
    "x-client-ip",
    "true-client-ip",
];

fn came_through_a_proxy(headers: &HeaderMap) -> bool {
    FORWARDING_HEADERS.iter().any(|h| headers.contains_key(*h))
}

fn html_security_headers(response: &mut Response) {
    let h = response.headers_mut();
    h.insert(
        "content-security-policy",
        axum::http::HeaderValue::from_static(
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
             frame-ancestors 'none'",
        ),
    );
    h.insert(
        "x-frame-options",
        axum::http::HeaderValue::from_static("DENY"),
    );
    h.insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        "referrer-policy",
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
}

async fn connect_page() -> Response {
    let mut response = (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("panel/connect.html"),
    )
        .into_response();
    html_security_headers(&mut response);
    response
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    token: Option<String>,
    ticket: Option<String>,
}

fn authorized_events(state: &AppState, headers: &HeaderMap, query: &EventsQuery) -> bool {
    if state.token.is_none() {
        return true;
    }
    if authorized(state, headers).is_some() {
        return true;
    }
    if let Some(ticket) = query
        .ticket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        && crate::events::ticket_valid(ticket)
    {
        return true;
    }
    let Some(expected) = state.token.as_deref() else {
        return true;
    };
    let presented = query
        .token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    matches!(presented, Some(p) if same_secret(p, expected))
}

async fn events_ticket(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match authorized(&state, &headers) {
        Some(Scope::Full) => {
            let ticket = crate::events::issue_ticket();
            (
                StatusCode::OK,
                axum::Json(serde_json::json!({
                    "ticket": ticket,
                    "expires_in_secs": 120,
                    "events": "/events",
                })),
            )
                .into_response()
        }
        Some(Scope::Peer) => (
            StatusCode::FORBIDDEN,
            axum::Json(error_envelope(
                Value::Null,
                -32001,
                "peer token cannot mint SSE tickets",
            )),
        )
            .into_response(),
        None => (
            StatusCode::UNAUTHORIZED,
            axum::Json(error_envelope(Value::Null, -32001, "invalid bearer token")),
        )
            .into_response(),
    }
}

async fn events_sse(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> Response {
    if !authorized_events(&state, &headers, &query) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(error_envelope(Value::Null, -32001, "invalid bearer token")),
        )
            .into_response();
    }

    let rx = crate::events::subscribe();
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(ev) => {
                let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
                Some((
                    Ok::<_, std::convert::Infallible>(
                        axum::response::sse::Event::default().data(data),
                    ),
                    rx,
                ))
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Some((
                Ok(axum::response::sse::Event::default()
                    .event("lagged")
                    .data("{}")),
                rx,
            )),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    });

    axum::response::sse::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

async fn panel(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(refusal) = refuse_foreign_origin(&state, &headers) {
        return refusal;
    }
    if came_through_a_proxy(&headers) && !panel_allows_forwarded() {
        return (
            StatusCode::FORBIDDEN,
            "This request carries a forwarding header, so it reached the daemon through an \
             HTTP proxy or tunnel rather than from this machine. The panel drives the admin \
             token, which can call every tool. Set CUBA_PANEL_PUBLIC=1 if you meant to publish \
             it.\n\nWhat this check can and cannot do, so nobody trusts it further than it \
             goes: it catches HTTP proxies, which announce themselves in a header. A raw TCP \
             forward — ssh -L, socat, ngrok tcp — adds no header at all and is indistinguishable \
             from a local request. If the daemon is reachable that way, the bearer token is the \
             only thing between a stranger and this page.\n",
        )
            .into_response();
    }

    let mut response = (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("panel/index.html"),
    )
        .into_response();

    let h = response.headers_mut();
    h.insert(
        "content-security-policy",
        axum::http::HeaderValue::from_static(
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
             connect-src 'self'; img-src data:; base-uri 'none'; form-action 'none'; \
             frame-ancestors 'none'",
        ),
    );
    h.insert(
        "x-frame-options",
        axum::http::HeaderValue::from_static("DENY"),
    );
    h.insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        "referrer-policy",
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// Which machine a request came from.
///
/// Loopback is always one bucket: every caller on this machine is this
/// machine, and keying them apart would change the key shape for every
/// install running today. Off-box, an explicit `Mcp-Machine-Id` wins because a
/// client that names itself survives DHCP; otherwise the address it connected
/// from is the only thing that distinguishes it.
fn origin_of(peer_is_loopback: bool, machine: Option<&str>, peer: &str) -> crate::session::Origin {
    use crate::session::Origin;
    if peer_is_loopback {
        return Origin::Local;
    }
    match machine.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => Origin::Remote(id.to_string()),
        None => Origin::Remote(peer.to_string()),
    }
}

fn request_origin(peer: SocketAddr, headers: &HeaderMap) -> crate::session::Origin {
    let machine = headers.get("mcp-machine-id").and_then(|v| v.to_str().ok());
    origin_of(peer.ip().is_loopback(), machine, &peer.ip().to_string())
}

/// Whether a browser origin may talk to this daemon.
///
/// Requests without an `Origin` pass: that is every MCP client there is, and
/// none of them is a browser. A request *with* one came from a page, and the
/// only pages that have any business here are the daemon's own.
///
/// This matters most where it looks least necessary. On a routable bind the
/// bearer token already stands in the way. On loopback with no token — the
/// documented default — any page the operator happens to open can reach
/// `127.0.0.1:8787`, and DNS rebinding turns "any page" into "any site". The
/// token is not there to stop it because the daemon is local; the Origin is
/// the only thing that distinguishes the operator's own panel from a tab.
///
/// Deliberately no CORS headers anywhere: `Content-Type: application/json`
/// already forces a preflight, and with nothing answering it the browser
/// refuses the request on its own. Adding a permissive CORS layer would
/// *remove* that protection.
fn origin_allowed(origin: Option<&str>, port: u16) -> bool {
    origin_allowed_with(
        origin,
        port,
        &std::env::var("CUBA_HTTP_ALLOWED_ORIGINS").unwrap_or_default(),
    )
}

/// The allowlist is a parameter so it can be checked. Read straight from the
/// environment it was the one branch here no test could reach, and three
/// mutations of it survived the gate for exactly that reason.
fn origin_allowed_with(origin: Option<&str>, port: u16, allowlist: &str) -> bool {
    let Some(origin) = origin.map(str::trim).filter(|o| !o.is_empty()) else {
        return true;
    };
    if origin == "null" {
        return false;
    }
    if allowlist
        .split(',')
        .map(str::trim)
        .any(|o| !o.is_empty() && o == origin)
    {
        return true;
    }
    let host = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    let (name, declared_port) = host.rsplit_once(':').unwrap_or((host, ""));
    let local = matches!(name, "localhost" | "127.0.0.1" | "[::1]" | "::1");
    local && (declared_port.is_empty() || declared_port == port.to_string())
}

/// How many wrong tokens from one address before it has to wait, and for how
/// long the count is remembered.
const MAX_AUTH_FAILURES: u32 = 10;
const AUTH_FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// How long this address must wait, if at all.
///
/// `same_secret` compares in constant time, so there is no timing to leak —
/// but nothing was slowing an attacker on the LAN down between attempts, and
/// they could try tokens exactly as fast as the daemon could answer.
///
/// Only failures are counted. A successful call clears the address, so an
/// editor that batches `tools/call` is never throttled: rate-limiting real
/// traffic would make the daemon look broken to the one client that is
/// behaving.
fn auth_brake(failures: u32, window_age: std::time::Duration) -> Option<std::time::Duration> {
    if window_age >= AUTH_FAILURE_WINDOW || failures < MAX_AUTH_FAILURES {
        return None;
    }
    Some(AUTH_FAILURE_WINDOW - window_age)
}

/// Refuse a browser page that is not the daemon's own, if that is what this is.
fn refuse_foreign_origin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    if origin_allowed(origin, state.port) {
        return None;
    }
    Some(
        (
            StatusCode::FORBIDDEN,
            "este endpoint no acepta peticiones de una página web de otro origen",
        )
            .into_response(),
    )
}

/// The count this address should carry after one more wrong token.
///
/// A window older than the limit starts over rather than accumulating: one bad
/// afternoon must not follow an address for the life of the daemon.
fn next_failure(entry: Option<(u32, Instant)>) -> (u32, Instant) {
    match entry {
        Some((failures, since)) if window_still_open(since.elapsed()) => (failures + 1, since),
        _ => (1, Instant::now()),
    }
}

/// Whether a window that started this long ago still counts.
///
/// Its own function because the boundary is the whole question and real clocks
/// cannot be asked about it: an `Instant` built exactly one window ago is
/// already older than one by the time it is read, so `<` and `<=` are
/// indistinguishable through `next_failure`.
fn window_still_open(elapsed: std::time::Duration) -> bool {
    elapsed < AUTH_FAILURE_WINDOW
}

fn brake_for(state: &AppState, who: std::net::IpAddr) -> Option<std::time::Duration> {
    let (failures, since) = state.auth_failures.read().ok()?.get(&who).copied()?;
    auth_brake(failures, since.elapsed())
}

fn record_auth_failure(state: &AppState, who: std::net::IpAddr) {
    if let Ok(mut failures) = state.auth_failures.write() {
        let updated = next_failure(failures.get(&who).copied());
        failures.insert(who, updated);
    }
}

fn clear_auth_failures(state: &AppState, who: std::net::IpAddr) {
    if let Ok(mut failures) = state.auth_failures.write() {
        failures.remove(&who);
    }
}

async fn mcp_endpoint(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(refusal) = refuse_foreign_origin(&state, &headers) {
        return refusal;
    }
    let origin = request_origin(peer, &headers);

    let who = peer.ip();
    if let Some(wait) = brake_for(&state, who) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", wait.as_secs().max(1).to_string())],
            axum::Json(error_envelope(
                Value::Null,
                -32001,
                "demasiados tokens inválidos desde esta dirección",
            )),
        )
            .into_response();
    }

    let Some(scope) = authorized(&state, &headers) else {
        record_auth_failure(&state, who);
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(error_envelope(Value::Null, -32001, "invalid bearer token")),
        )
            .into_response();
    };
    clear_auth_failures(&state, who);

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(error_envelope(
                    Value::Null,
                    -32700,
                    format!("Parse error: {e}"),
                )),
            )
                .into_response();
        }
    };

    let (label, declared) = client_key(&headers, &payload);
    let mcp_sid = mcp_session_id(&headers);
    let key = crate::session::bind_key(&label, mcp_sid.as_deref(), &origin);
    note_activity(&state, &key);

    let (items, is_batch) = match batch_items(payload) {
        Ok(split) => split,
        Err(envelope) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, axum::Json(envelope)).into_response();
        }
    };

    let mut items = items;
    let mut carried_roots = false;
    for item in &items {
        if let Some(uri) = first_root_uri(item) {
            adopt_client_root(&state.pool, &label, uri).await;
            carried_roots = true;
        }
    }
    if carried_roots {
        items.retain(|item| first_root_uri(item).is_none());
        if items.is_empty() {
            return StatusCode::ACCEPTED.into_response();
        }
    }

    let dispatch = async {
        let mut responses: Vec<Value> = Vec::with_capacity(items.len());
        for item in items {
            state.served.fetch_add(1, Ordering::Relaxed);
            if let Some(reply) =
                dispatch_one(&state, &label, mcp_sid.clone(), declared, scope, item).await
            {
                responses.push(reply);
            }
        }
        responses
    };

    let deadline = request_deadline();
    let Ok(mut responses) = tokio::time::timeout(deadline, dispatch).await else {
        tracing::warn!(client = %key, secs = deadline.as_secs(), "request hit the deadline");
        return (
            StatusCode::GATEWAY_TIMEOUT,
            axum::Json(error_envelope(
                Value::Null,
                -32000,
                format!(
                    "the request was still running after {}s and was dropped; \
                     each call already has its own timeout, this bounds the whole POST",
                    deadline.as_secs()
                ),
            )),
        )
            .into_response();
    };

    if responses.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }

    // Cursor's Streamable HTTP client treats an SSE initialize body as the
    // session stream. When that short body ends it POSTs tools/call and
    // reports 405; Settings stays green because initialize "succeeded".
    // Roots are still adopted when the client POSTs the JSON-RPC reply
    // (first_root_uri). Do not piggy-back roots/list on initialize.

    if is_batch {
        axum::Json(Value::Array(responses)).into_response()
    } else {
        axum::Json(responses.remove(0)).into_response()
    }
}

fn mcp_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Bind task-local identity when the client declared itself *or* sent a
/// protocol session. `&&` here would drop Mcp-Client-Id-only Cursor chats
/// (no Mcp-Session-Id) into the anonymous pool and clobber every other chat.
fn should_bind_identity(declared: bool, mcp_session: Option<&str>) -> bool {
    declared || mcp_session.is_some()
}

async fn dispatch_one(
    state: &AppState,
    label: &str,
    mcp_sid: Option<String>,
    declared: bool,
    scope: Scope,
    item: Value,
) -> Option<Value> {
    let id = item.get("id").cloned().unwrap_or(Value::Null);

    let request: JsonRpcRequest = match serde_json::from_value(item) {
        Ok(r) => r,
        Err(e) => {
            return Some(error_envelope(id, -32600, format!("Invalid request: {e}")));
        }
    };

    let is_notification = request.id.is_none();
    let req_id = request.id.clone().unwrap_or(Value::Null);
    let pool = state.pool.clone();
    let method = request.method.clone();

    if crate::admin::is_admin_method(&method) {
        if scope != Scope::Full {
            return Some(error_envelope(
                req_id,
                -32001,
                "a peer token cannot reach the admin surface. The read-only scope exists so the \
                 other machine cannot call cuba_forget, and admin/* would hand it the same \
                 answers through a different door",
            ));
        }
        let connected: Vec<Value> = state
            .seen
            .read()
            .map(|g| {
                g.iter()
                    .map(|(name, last)| {
                        serde_json::json!({
                            "client": name,
                            "idle_secs": last.elapsed().as_secs(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let uptime = state.started.elapsed().as_secs();
        return Some(
            match crate::admin::handle(&pool, &method, uptime, connected).await {
                Ok(result) => serde_json::json!({
                    "jsonrpc": "2.0", "id": req_id, "result": result
                }),
                Err(e) => error_envelope(req_id, -32603, format!("{e:#}")),
            },
        );
    }

    let served = async move {
        crate::session::with_scope(scope, protocol::handle_request(&pool, request)).await
    };
    let work = async {
        let rooted = crate::session::with_root_project(
            crate::session::client_root_project_for(label),
            served,
        );
        if should_bind_identity(declared, mcp_sid.as_deref()) {
            crate::session::with_identity(label.to_string(), mcp_sid, rooted).await
        } else {
            rooted.await
        }
    };

    let outcome = match std::panic::AssertUnwindSafe(work).catch_unwind().await {
        Ok(result) => result,
        Err(panic) => {
            let detail = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_string());
            tracing::error!(client = %label, method = %method, detail = %detail, "handler panicked");
            Err(anyhow::anyhow!("handler panicked: {detail}"))
        }
    };

    if is_notification {
        if let Err(e) = &outcome {
            tracing::warn!(error = %format!("{e:#}"), "notification handler error (suppressed)");
        }
        return None;
    }

    Some(match outcome {
        Ok(v) => serde_json::json!({ "jsonrpc": "2.0", "id": req_id, "result": v }),
        Err(e) => {
            let chain = format!("{e:#}");
            tracing::error!(client = %label, method = %method, error = %chain, "handler failed");
            error_envelope(req_id, -32603, chain)
        }
    })
}

/// What a caller who has proved nothing may see of the graph backend.
///
/// `graph_db::status_summary()` carries `last_error`, and a connection failure
/// names the host and port it could not reach. It also carries the graph name
/// and whether a URL is configured. On a loopback bind none of that matters,
/// but `/health` answers on a LAN bind too, where it was handing anyone who
/// could route a packet a piece of internal topology, unauthenticated.
///
/// What survives is what a monitor actually needs: which backend, and whether
/// it answers.
fn graph_summary_for(full_scope: bool, summary: Value) -> Value {
    if full_scope {
        return summary;
    }
    serde_json::json!({
        "backend": summary.get("backend").cloned().unwrap_or(Value::Null),
        "reachable": summary.get("reachable").cloned().unwrap_or(Value::Null),
    })
}

/// A monitor asks this every few seconds; it must answer or say why, never sit.
const HEALTH_DB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

// The three answers `/health` gives about the daemon as a whole, and the
// vocabulary below for one model. These words are the wire contract, not
// identifiers: a monitor matches on them, so they are written once here.
const STATUS_OK: &str = "ok";
const STATUS_STARTING: &str = "starting";
const STATUS_DEGRADED: &str = "degraded";

const STATE_LOADED: &str = "loaded";
/// There is a model and it did not open. The only state that means *broken*.
const STATE_FAILED: &str = "failed";
/// A model is on disk and nothing has needed it yet.
const STATE_CONFIGURED: &str = "configured";
/// Still inside the warm-up that ran past its budget.
const STATE_WARMING: &str = "warming";
/// Loading was attempted and the code fell back to something weaker.
const STATE_FALLBACK: &str = "fallback";
/// The resource plan switched this model off on purpose.
const STATE_OFF: &str = "off";
/// Nothing on disk to load.
const STATE_ABSENT: &str = "absent";

/// Where one model is and why it is not simply running.
///
/// `device` is where the session opened and is `None` for a model with no open
/// session, or with one nobody recorded: it is not where the model was asked
/// to go. `reason` is a sentence for a human and is `None` when the state says
/// everything. It never carries a filesystem path, with or without spaces in
/// it: a model path is inventory, and on Windows it has the operator's user
/// name inside it. The one case it cannot promise is a Linux directory whose
/// name has a `:` or a `"` (see `reason_without_a_model_path`). Every reason here
/// is a literal except three — the load failure of each model, which comes
/// from its loader and from ONNX Runtime — and each of those goes through
/// `reason_without_a_model_path` first, with the variable that names *its*
/// directory. Before it did, this paragraph was a promise the code did not
/// keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelState {
    pub state: &'static str,
    pub device: Option<&'static str>,
    pub reason: Option<String>,
}

/// Everything `/health` can say about this process without asking anything
/// outside it.
///
/// Built for every caller, served only to `Scope::Full`: the verdict in
/// `status` is for the monitor, the inventory behind it is for the operator.
/// That split is the whole point — a `last_error` naming an internal host went
/// out to anonymous callers once already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeReport {
    pub mode: &'static str,
    pub resource_tier: &'static str,
    /// False while the models are still loading behind an already-open port.
    pub ready: bool,
    pub embedder: ModelState,
    pub reranker: ModelState,
    pub nli: ModelState,
    /// The GPU provider this binary was compiled against, if any.
    pub gpu_build: Option<&'static str>,
    /// `gpu::status()` — asked for a device and not using one, including a
    /// session whose provider refused to start.
    pub gpu_degraded: bool,
    /// `embedder=… reranker=… nli=…`, as the placement path decided it: what was
    /// asked for. Where each session opened is `device` in its own row.
    pub gpu_placement: String,
    pub llm: crate::llm_cli::LlmSummary,
}

/// The one verdict, from the facts, as a function that can be read and mutated.
///
/// Deliberately not inlined into the handler: `quality-gate.sh` runs
/// `cargo mutants` over `rust/src`, and an async handler is exactly what it
/// skips — logic living inside `health()` is logic no second judge ever tests.
///
/// The precedence is `degraded` over `starting` because they ask for different
/// things from whoever reads them: `starting` will fix itself and wants
/// patience, `degraded` will not and wants a person. When both are true the
/// person is the one who is needed, and `ready` is in the same body for anyone
/// who wants to know which.
///
/// `STATE_FALLBACK` on the embedder is deliberately *not* degraded. This
/// daemon is supported on machines that never had an embedding model — that is
/// what `Tier::Minimal` is — and reporting every one of those as degraded
/// forever teaches the operator to ignore the field, which is the failure this
/// endpoint exists to prevent. `resource_tier`, in the same block, says which
/// models the plan expected.
///
/// `STATE_FAILED` on that same embedder *is* degraded, and the two must not be
/// folded back together: `fallback` is "there was nothing to load", `failed`
/// is "everything was there and the session would not open". For two releases
/// the embedder could only say the first, so a broken machine and a supported
/// one were the same word here.
pub fn overall_status(db_ok: bool, runtime: &RuntimeReport) -> &'static str {
    let a_model_is_broken = [&runtime.embedder, &runtime.reranker, &runtime.nli]
        .into_iter()
        .any(|model| model.state == STATE_FAILED);

    if !db_ok || runtime.gpu_degraded || a_model_is_broken {
        return STATUS_DEGRADED;
    }
    if !runtime.ready {
        return STATUS_STARTING;
    }
    STATUS_OK
}

/// The GPU provider compiled in, for the one field that has to name it.
///
/// This is the second reader of the feature flags after `gpu::compiled_provider`,
/// which is private. It is read to print a word, never to choose a device: the
/// placement decision stays in `gpu.rs` so there is still exactly one place
/// that can send a tensor to the wrong processor.
fn compiled_gpu_provider() -> Option<&'static str> {
    if cfg!(feature = "cuda") {
        Some("cuda")
    } else if cfg!(feature = "directml") {
        Some("directml")
    } else {
        None
    }
}

/// The embedder, asked only once it is safe to ask.
///
/// `onnx::is_model_loaded()` resolves a `OnceLock` that loads the model, so on
/// a cold process it is a multi-second call — inside `/health`, on the async
/// executor. `ready` is the exact guard: it is set after `warm_models()`
/// finished, and `warm_models()` is what resolves that cell. Before it flips,
/// the honest answer is that the model is still loading, and asking would be
/// the thing that makes a monitor's poll hang. That `&&` is the guard itself
/// and not an optimisation — it is the one short circuit in this block that
/// has to stay.
///
/// `failure_reason()` is read *after* `loaded`, and the order is load-bearing:
/// past the guard `is_model_loaded()` is what settled the cell, so asking it
/// first would read an unresolved cell and report `fallback` on a machine
/// whose session had actually failed.
fn embedder_state(ready: bool) -> ModelState {
    let placement = crate::gpu::placement(crate::gpu::Workload::Embedder);
    let loaded = ready && crate::embeddings::onnx::is_model_loaded();
    embedder_state_from(
        crate::embeddings::onnx::failure_reason(),
        placement.as_ref(),
        ready,
        loaded,
    )
}

/// The embedder's state, from the three facts that decide it and nothing else.
///
/// `fallback` and `failed` are the whole point of this function existing. A
/// machine that never had an embedding model is `Tier::Minimal` and is
/// supported — `overall_status` deliberately does not call it degraded. A
/// machine whose model file was there, whose runtime was there, and whose
/// session still would not open is a fault, and until this split existed
/// `/health` reported both of them with the same word.
///
/// The failure wins over `warming` for the same reason `overall_status` puts
/// `degraded` over `starting`: past the warm ceiling `serve_pool` detaches the
/// warm-up and serves with `ready:false`, so a session that already failed
/// would otherwise hide behind "still loading" for as long as the process
/// lives.
fn embedder_state_from(
    failure: Option<String>,
    placement: Option<&crate::gpu::Placement>,
    ready: bool,
    loaded: bool,
) -> ModelState {
    if let Some(reason) = failure {
        return ModelState {
            state: STATE_FAILED,
            device: None,
            reason: Some(reason_without_a_model_path(
                &reason,
                EMBEDDER_DIR_INSTEAD_OF_A_PATH,
            )),
        };
    }
    if !ready {
        return ModelState {
            state: STATE_WARMING,
            device: None,
            reason: None,
        };
    }
    if loaded {
        return loaded_on(placement);
    }
    ModelState {
        state: STATE_FALLBACK,
        // The hash fallback runs on this CPU whatever the registry holds: there
        // is no session for it to hold anything about.
        device: Some("cpu"),
        reason: Some(
            "no ONNX embedder opened — vectors come from the hash fallback and search is \
             lexical only"
                .to_string(),
        ),
    }
}

/// The reranker, read from the cell rather than forced into it.
///
/// Every accessor used here answers without loading: `resolved_model_dir` and
/// `is_configured` stat the disk, and `failure_reason` / `status_resolved` read
/// a cell that is already decided or is not. `rerank::enabled()` would load
/// 1,1 GB inside a health poll.
///
/// All four facts are read before the decision instead of on the way through
/// it. Short-circuiting saved two `stat` calls and cost the decision its only
/// judge: the first branch turns on a process-wide `OnceLock` that another
/// test in this binary resolves, so which branch a test even reached was
/// decided by the order libtest happened to schedule them in — and a test that
/// asserted here would go red on an unmutated tree half the time.
fn reranker_state() -> ModelState {
    reranker_state_from(
        crate::search::rerank::failure_reason(),
        crate::gpu::placement(crate::gpu::Workload::Reranker).as_ref(),
        crate::search::rerank::resolved_model_dir().is_some(),
        crate::search::rerank::is_configured(),
        crate::search::rerank::status_resolved(),
    )
}

/// The reranker's state, from the four facts that decide it and nothing else.
///
/// `dir_resolved` is whether there is a directory to look in at all — the
/// resource plan switches the model off by naming one it never creates.
/// `configured` is a model file inside it, and `status_resolved` is whether
/// something already tried to open a session.
fn reranker_state_from(
    failure: Option<String>,
    placement: Option<&crate::gpu::Placement>,
    dir_resolved: bool,
    configured: bool,
    status_resolved: bool,
) -> ModelState {
    if let Some(reason) = failure {
        return ModelState {
            state: STATE_FAILED,
            device: None,
            reason: Some(reason_without_a_model_path(
                &reason,
                RERANKER_DIR_INSTEAD_OF_A_PATH,
            )),
        };
    }
    if !dir_resolved {
        return ModelState {
            state: STATE_OFF,
            device: None,
            reason: Some(
                "the resource plan switched the reranker off on this machine; rankings come \
                 back in RRF order"
                    .to_string(),
            ),
        };
    }
    if !configured {
        return ModelState {
            state: STATE_ABSENT,
            device: None,
            reason: Some(
                "no reranker model where this daemon looks — `memory-industry models reranker` \
                 installs it"
                    .to_string(),
            ),
        };
    }
    if status_resolved {
        return loaded_on(placement);
    }
    ModelState {
        state: STATE_CONFIGURED,
        device: None,
        reason: Some("loads on its first batch".to_string()),
    }
}

/// What a model directory becomes in `/health`: the variable that sets it,
/// never the directory it resolved to.
///
/// One per model, because the sentence is the operator's next move. A failed
/// NLI load that came back pointing at `CUBA_RERANKER_PATH` would send them to
/// change the wrong variable, which is worse than saying nothing.
const RERANKER_DIR_INSTEAD_OF_A_PATH: &str = "the reranker directory (see CUBA_RERANKER_PATH)";
const NLI_DIR_INSTEAD_OF_A_PATH: &str = "the NLI directory (see CUBA_NLI_PATH)";
const EMBEDDER_DIR_INSTEAD_OF_A_PATH: &str = "the embedder directory (see ONNX_MODEL_PATH)";
/// The odd one out: the paths in a provider refusal are the runtime's and its
/// provider libraries', not any model's, so the variable has to be the
/// runtime's too.
const RUNTIME_DIR_INSTEAD_OF_A_PATH: &str = "the ONNX Runtime directory (see ORT_DYLIB_PATH)";

/// A model whose session is open, from where `gpu::configure` says it opened.
///
/// With nothing in the registry the device is `None`: the answer `/health` used
/// to give here was the placement *plan*, and on a machine where the provider
/// refused the plan said `gpu` for a session on the CPU. Only that last case
/// has something to explain, and the cause goes through the same redaction as
/// every other sentence in this block.
fn loaded_on(placement: Option<&crate::gpu::Placement>) -> ModelState {
    let (device, reason) = match placement {
        Some(crate::gpu::Placement::Gpu) => (Some("gpu"), None),
        Some(crate::gpu::Placement::Cpu) => (Some("cpu"), None),
        Some(crate::gpu::Placement::CpuAfterProviderFailed(cause)) => (
            Some("cpu"),
            Some(format!(
                "the GPU provider did not start; this session runs on the CPU: {}",
                reason_without_a_model_path(cause, RUNTIME_DIR_INSTEAD_OF_A_PATH)
            )),
        ),
        None => (None, None),
    };
    ModelState {
        state: STATE_LOADED,
        device,
        reason,
    }
}

/// A load failure with the path taken out and the variable that names it put
/// in its place.
///
/// The sentence comes from `init_session`, which formats the directory it
/// looked in, and from ONNX Runtime, which formats the file it could not open.
/// Either one is inventory, and on Windows either one carries the operator's
/// user name, which is what `C:\Users\…\.cache` is made of. `doctor` still
/// prints the sentence whole, and should: it runs on the machine the operator
/// is already standing on, while this one goes out over a socket that a LAN
/// bind makes reachable.
///
/// Three layers, in this order, because a space inside a path says nothing:
/// no rule that cuts on spaces can tell `Carlos` from a word of the sentence,
/// and `C:\Users\Juan Carlos` left `Carlos` in the body before this.
///
/// 1. A segment in `"`, `'` or `` ` `` is replaced whole, quotes included,
///    when what is inside has a separator and an alphanumeric. A quote opens
///    only after a non-alphanumeric and closes only before one, so the `'` of
///    `doesn't` and of `O'Brien` open and close nothing.
/// 2. A path with no quotes starts at an anchor (`C:\`, `\\`, `/x`, `~/`,
///    `./`, `../`) and runs to the end of its clause: a `:` not followed by
///    `\` or `/`, a `"`, a line break or the end of the text. The output then
///    does not depend on the path at all, which is the property that matters.
///    Whatever the sentence says between the path and that `:` goes with it
///    (`with error`, the runtime version): `doctor` still shows it, and this
///    block is the one that leaves the machine.
/// 3. What is left goes word by word. A word is a path when it has a separator
///    with a name around it, which leaves the actionable half of the sentence
///    alone: the file names it looked for, `model.onnx` and
///    `model_quantized.onnx`, and the bare `/` between them. It is also what
///    catches a relative path with no anchor and a URL.
///
/// Where it cannot be exact: on Linux a directory whose name has a `:` or a
/// `"` ends the clause early, and layer 3 only takes the words of the rest
/// that have a separator. Windows does not allow either in a name.
fn reason_without_a_model_path(reason: &str, instead: &'static str) -> String {
    let chars: Vec<char> = reason.chars().collect();
    let mut redacted = String::with_capacity(reason.len());
    let mut at = 0;
    // A step that works moves at least one character, so `chars.len()` steps
    // cover the text, and a step taken past the end writes nothing. The bound
    // is a count and not a comparison on `at` on purpose: it ends the loop
    // whatever a step returns, so a step that stopped moving would fail an
    // assertion on its output instead of growing `redacted` until the process
    // died, and there is no `at < len` to turn into `at <= len`, a mutant that
    // changes nothing because that extra step is the one that writes nothing.
    for _ in 0..chars.len() {
        at = redact_at(&chars, at, instead, &mut redacted);
    }
    redact_words(&redacted, instead)
}

/// Layers 1 and 2 at one position: writes what stands there into `out` and
/// returns where to go on from, which is past `at` for every position inside
/// the text and is never relied on to be.
fn redact_at(chars: &[char], at: usize, instead: &str, out: &mut String) -> usize {
    if let Some(close) = quoted_span_end(chars, at) {
        // The quotes stay in the span that is judged: they are neither a
        // separator nor alphanumeric, so the verdict is the one for what is
        // inside, and no `at + 1` has to cut the opening one off.
        let span: String = chars.get(at..=close).unwrap_or_default().iter().collect();
        out.push_str(if is_path_like(&span) {
            instead
        } else {
            span.as_str()
        });
        return close + 1;
    }
    if path_anchor_at(chars, at) {
        let end = clause_end(chars, at);
        out.push_str(instead);
        out.extend(trailing_blanks(chars.get(at..end).unwrap_or_default()));
        return end;
    }
    out.extend(chars.get(at));
    at + 1
}

/// Layer 3, the rule the function had before paths with spaces.
fn redact_words(text: &str, instead: &'static str) -> String {
    text.split_whitespace()
        .map(|word| if is_path_like(word) { instead } else { word })
        .collect::<Vec<&str>>()
        .join(" ")
}

/// A separator and an alphanumeric: the same predicate for a word and for the
/// inside of a quote, so `"/"` is left alone as `/` is.
fn is_path_like(text: &str) -> bool {
    text.contains(['/', '\\']) && text.chars().any(char::is_alphanumeric)
}

fn alphanumeric_at(chars: &[char], at: usize) -> bool {
    chars.get(at).is_some_and(|c| c.is_alphanumeric())
}

/// Where the quote that opens at `at` closes, if one does. It opens only after
/// a non-alphanumeric and closes only before one.
fn quoted_span_end(chars: &[char], at: usize) -> Option<usize> {
    let quote = *chars.get(at).filter(|c| matches!(**c, '"' | '\'' | '`'))?;
    if at
        .checked_sub(1)
        .is_some_and(|before| alphanumeric_at(chars, before))
    {
        return None;
    }
    (at + 1..chars.len())
        .find(|&close| chars.get(close) == Some(&quote) && !alphanumeric_at(chars, close + 1))
}

/// An unquoted path starts at the beginning of the text or after a blank or
/// one of the characters that punctuation puts in front of it.
fn path_anchor_at(chars: &[char], at: usize) -> bool {
    let after_boundary = at.checked_sub(1).is_none_or(|before| {
        chars
            .get(before)
            .is_some_and(|c| c.is_whitespace() || "([{=,;'\"`".contains(*c))
    });
    after_boundary && anchor_shape(chars.get(at..).unwrap_or_default())
}

/// A drive (`C:\`, `C:/`), a UNC or verbatim prefix, a POSIX root, or a
/// home-relative or dot-relative start. A lone `/` followed by a blank is not
/// one: it is the one between `model.onnx / model_quantized.onnx`.
fn anchor_shape(rest: &[char]) -> bool {
    match rest {
        [drive, ':', '\\' | '/', ..] => drive.is_ascii_alphabetic(),
        ['\\', '\\', next, ..] | ['/', next, ..] => !next.is_whitespace(),
        ['~' | '.', '\\' | '/', ..] | ['.', '.', '\\' | '/', ..] => true,
        _ => false,
    }
}

/// The clause of an unquoted path ends at a `:` that is not the one in `C:\`,
/// at a `"`, at a line break or at the end of the text.
fn clause_end(chars: &[char], start: usize) -> usize {
    (start..chars.len())
        .find(|&at| ends_clause(chars, at))
        .unwrap_or(chars.len())
}

fn ends_clause(chars: &[char], at: usize) -> bool {
    match chars.get(at) {
        Some('"' | '\n' | '\r') => true,
        Some(':') => !matches!(chars.get(at + 1), Some('\\' | '/')),
        _ => false,
    }
}

/// The blanks a path swallowed before its clause ended. They are the
/// sentence's, not the path's, and stay: `"` follows them in
/// `…dll which depends on "cublasLt64_12.dll"`.
fn trailing_blanks(path: &[char]) -> &[char] {
    let kept = path
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map_or(0, |last| last + 1);
    path.get(kept..).unwrap_or_default()
}

/// The NLI model, read from the cell rather than forced into it.
///
/// Every accessor here answers without loading: `deferred_by_resource_plan`
/// and `available` stat the disk, and `failure_reason` / `status_resolved`
/// read a cell that is already decided or is not. `nli::enabled()` would load
/// ~1 GB inside a health poll, which is the bug `doctor` was fixed for — and
/// for two releases it was also the reason this block could say nothing but
/// `configured` about a model that had already failed to open.
///
/// All four facts are read before the decision instead of on the way through
/// it, for the reason written out over `reranker_state`: the first branch of a
/// short-circuited version turns on a process-wide `OnceLock` that another
/// test in this binary resolves.
fn nli_state() -> ModelState {
    nli_state_from(
        crate::cognitive::nli::failure_reason(),
        crate::gpu::placement(crate::gpu::Workload::Nli).as_ref(),
        crate::cognitive::nli::deferred_by_resource_plan(),
        crate::cognitive::nli::available(),
        crate::cognitive::nli::status_resolved(),
    )
}

/// The NLI model's state, from the four facts that decide it and nothing else.
///
/// `deferred` is the resource plan switching the model off by pointing the
/// path at a sentinel it never creates; `available` is a model file in the
/// cache; and `status_resolved` is whether something already tried to open a
/// session — which is what tells `loaded` from `configured` without opening
/// one here.
fn nli_state_from(
    failure: Option<String>,
    placement: Option<&crate::gpu::Placement>,
    deferred: bool,
    available: bool,
    status_resolved: bool,
) -> ModelState {
    if let Some(reason) = failure {
        return ModelState {
            state: STATE_FAILED,
            device: None,
            reason: Some(reason_without_a_model_path(
                &reason,
                NLI_DIR_INSTEAD_OF_A_PATH,
            )),
        };
    }
    if deferred {
        return ModelState {
            state: STATE_OFF,
            device: None,
            reason: Some(
                "the resource plan switched NLI off on this machine; contradictions fall back \
                 to the judge"
                    .to_string(),
            ),
        };
    }
    if !available {
        return ModelState {
            state: STATE_ABSENT,
            device: None,
            reason: Some("no NLI model where this daemon looks".to_string()),
        };
    }
    if status_resolved {
        return loaded_on(placement);
    }
    ModelState {
        state: STATE_CONFIGURED,
        device: None,
        reason: Some("loads on its first use; this endpoint does not force it".to_string()),
    }
}

fn runtime_report(state: &AppState) -> RuntimeReport {
    let ready = state.ready.load(Ordering::Relaxed);
    let gpu = crate::gpu::status();
    RuntimeReport {
        mode: crate::mode::active().as_str(),
        resource_tier: state.resource_tier,
        ready,
        embedder: embedder_state(ready),
        reranker: reranker_state(),
        nli: nli_state(),
        gpu_build: compiled_gpu_provider(),
        gpu_degraded: gpu.degraded,
        gpu_placement: crate::gpu::placement_summary(),
        llm: crate::llm_cli::configured_summary(),
    }
}

fn model_json(model: &ModelState) -> Value {
    serde_json::json!({
        "state": model.state,
        "device": model.device,
        "reason": model.reason,
    })
}

fn runtime_json(runtime: &RuntimeReport) -> Value {
    serde_json::json!({
        "mode": runtime.mode,
        "resource_tier": runtime.resource_tier,
        "embedder": model_json(&runtime.embedder),
        "reranker": model_json(&runtime.reranker),
        "nli": model_json(&runtime.nli),
        "gpu": {
            "build": runtime.gpu_build,
            "degraded": runtime.gpu_degraded,
            "placement": runtime.gpu_placement,
        },
        "llm": {
            "configured": runtime.llm.configured,
            "backend": runtime.llm.backend,
            "model": runtime.llm.model,
            "base_url": runtime.llm.base_url,
        },
    })
}

async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    // Bounded on purpose. A health endpoint that hangs because the database
    // hangs is worse than one that says the database is unreachable: the
    // monitor polling it holds a connection open and learns nothing.
    let db_ok = tokio::time::timeout(
        HEALTH_DB_TIMEOUT,
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.pool),
    )
    .await
    .is_ok_and(|r| r.is_ok());

    let full_scope = authorized(&state, &headers) == Some(Scope::Full);
    let runtime = runtime_report(&state);
    let mut body = serde_json::json!({
        "status": overall_status(db_ok, &runtime),
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started.elapsed().as_secs(),
        "requests_served": state.served.load(Ordering::Relaxed),
        "database": if db_ok { "up" } else { "unreachable" },
        "graph_db": graph_summary_for(full_scope, crate::graph_db::status_summary()),
        "ready": runtime.ready,
        "connect": "/connect",
        "events": "/events",
    });

    if full_scope {
        let clients: Vec<String> = state
            .seen
            .read()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default();
        body["clients"] = serde_json::json!(clients);
        // Only here. The block is an inventory of the machine — which models,
        // on which device, talking to which provider — and an anonymous caller
        // gets the verdict in `status` without the list of what to attack.
        body["runtime"] = runtime_json(&runtime);
    } else {
        body["clients_count"] = serde_json::json!(state.seen.read().map(|g| g.len()).unwrap_or(0));
    }

    // Always 200, including `degraded`, and there is no state that changes
    // that.
    //
    // A 503 is read by every monitor and every proxy as "dead, take it out of
    // rotation". This daemon answers lexical search, `cuba_decreto` reads and
    // the whole graph surface from caches that do not need PostgreSQL, and it
    // serves `/health` itself — so the 503 it used to return for an
    // unreachable database removed a daemon that was still working, and the
    // operator lost the one endpoint that could have told them what was wrong.
    //
    // `starting` does not earn one either: taking the only instance out of
    // rotation while it warms is the ECONNREFUSED this release just stopped
    // causing, one layer higher. There is also no rotation to be taken out of
    // — this is a single daemon on a LAN, not one of N behind a balancer.
    //
    // The honest non-200 would be "this process cannot answer", and that is
    // not a response this function can produce. Whoever needs to decide reads
    // `status`.
    (StatusCode::OK, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test in this module speaks for something on this machine.
    pub(super) fn local_peer() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 55555))
    }

    pub(super) fn headers_with(name: &'static str, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(name, value.parse().unwrap());
        h
    }

    #[test]
    fn a_client_that_announces_roots_is_asked_for_them() {
        let announces = serde_json::json!({
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": { "roots": { "listChanged": true }, "elicitation": {} },
                "clientInfo": { "name": "claude-code" }
            }
        });
        assert!(
            asks_for_roots(std::slice::from_ref(&announces)),
            "Claude Code 2.1.233 announces exactly this — captured from a live handshake on \
             2026-08-16. Asking a client that never announced roots would leave a request \
             hanging in its stream that it has no handler for"
        );

        let silent = serde_json::json!({
            "method": "initialize",
            "params": { "capabilities": { "sampling": {} } }
        });
        assert!(!asks_for_roots(std::slice::from_ref(&silent)));

        let not_a_handshake = serde_json::json!({
            "method": "tools/call",
            "params": { "capabilities": { "roots": {} } }
        });
        assert!(!asks_for_roots(std::slice::from_ref(&not_a_handshake)));
    }

    #[test]
    fn the_answer_to_our_roots_request_is_recognised_and_nothing_else_is() {
        let answer = serde_json::json!({
            "jsonrpc": "2.0",
            "id": ROOTS_REQUEST_ID,
            "result": { "roots": [
                { "uri": "file:///home/leandro/proyectos/MCP/cuba-memorys" },
                { "uri": "file:///tmp" }
            ]}
        });
        assert_eq!(
            first_root_uri(&answer),
            Some("file:///home/leandro/proyectos/MCP/cuba-memorys"),
            "the client lists its primary working directory first and its extra ones after; \
             taking any other entry would file every write under /tmp"
        );

        let someone_elses = serde_json::json!({
            "jsonrpc": "2.0", "id": "srv_9", "result": { "roots": [{ "uri": "file:///x" }] }
        });
        assert_eq!(first_root_uri(&someone_elses), None);

        let ordinary_call = serde_json::json!({ "method": "tools/list", "id": 1 });
        assert_eq!(first_root_uri(&ordinary_call), None);
    }

    #[tokio::test]
    async fn initialize_that_announces_roots_is_still_json() {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": { "roots": { "listChanged": true } },
                "clientInfo": { "name": "cursor" }
            }
        });
        let body = Bytes::from(serde_json::to_vec(&payload).expect("payload serializes"));
        let response = mcp_endpoint(
            State(state_with_clients(None, &[])),
            axum::extract::ConnectInfo(local_peer()),
            HeaderMap::new(),
            body,
        )
        .await;
        let ctype = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            ctype.starts_with("application/json"),
            "Cursor's Streamable HTTP client treats text/event-stream initialize as the \
             session stream and 405s the next POST; got {ctype}"
        );
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY)
            .await
            .expect("response body");
        let parsed: Value = serde_json::from_slice(&bytes).expect("JSON initialize body");
        assert_eq!(parsed["id"], 1);
        assert!(parsed.get("result").is_some(), "{parsed}");
    }

    #[test]
    fn a_protocol_session_splits_two_chats_of_the_same_window() {
        let payload = serde_json::json!({
            "params": { "clientInfo": { "name": "cursor" } }
        });
        let mut headers = headers_with("mcp-client-id", "cursor");
        headers.insert(
            "mcp-session-id",
            axum::http::HeaderValue::from_static("chat-a"),
        );
        assert_eq!(
            crate::session::bind_key(
                "cursor",
                mcp_session_id(&headers).as_deref(),
                &crate::session::Origin::Local
            ),
            "cursor::chat-a"
        );
        let _ = payload;
    }

    #[test]
    fn a_declared_client_binds_even_without_a_protocol_session() {
        assert!(
            should_bind_identity(true, None),
            "Mcp-Client-Id alone must bind — Cursor chats often omit Mcp-Session-Id; \
             replacing || with && would dump them into the anonymous pool"
        );
        assert!(
            should_bind_identity(false, Some("chat-a")),
            "a protocol session alone must bind even when the client id was invented"
        );
        assert!(
            !should_bind_identity(false, None),
            "anonymous requests stay unbound"
        );
        assert!(should_bind_identity(true, Some("chat-a")));
    }

    #[test]
    fn header_wins_over_payload() {
        let payload = serde_json::json!({
            "params": { "clientInfo": { "name": "claude-code" } }
        });
        let h = headers_with("mcp-client-id", "window-3");
        assert_eq!(client_key(&h, &payload), ("window-3".to_string(), true));
    }

    #[test]
    fn falls_back_to_meta_then_client_info() {
        let meta = serde_json::json!({
            "params": { "_meta": { "io.modelcontextprotocol/client-id": "from-meta" } }
        });
        assert_eq!(
            client_key(&HeaderMap::new(), &meta),
            ("from-meta".to_string(), true)
        );

        let info = serde_json::json!({
            "params": { "clientInfo": { "name": "warp" } }
        });
        assert_eq!(
            client_key(&HeaderMap::new(), &info),
            ("warp".to_string(), false),
            "a name out of clientInfo is not a declared identity: every Claude Code instance \
             sends the same one, so two of them would share a session and inherit each \
             other's scratchpad"
        );

        let bare = serde_json::json!({ "method": "ping" });
        assert_eq!(
            client_key(&HeaderMap::new(), &bare),
            ("anonymous".to_string(), false)
        );
    }

    #[test]
    fn batch_identity_comes_from_the_first_entry() {
        let batch = serde_json::json!([
            { "params": { "clientInfo": { "name": "first" } } },
            { "params": { "clientInfo": { "name": "second" } } },
        ]);
        assert_eq!(
            client_key(&HeaderMap::new(), &batch),
            ("first".to_string(), false)
        );
    }

    #[test]
    fn blank_header_does_not_win() {
        let payload = serde_json::json!({
            "params": { "clientInfo": { "name": "real-client" } }
        });
        let h = headers_with("mcp-client-id", "   ");
        assert_eq!(client_key(&h, &payload), ("real-client".to_string(), false));
    }

    #[test]
    fn non_loopback_bind_is_refused_without_a_token() {
        let prev = std::env::var("CUBA_HTTP_TOKEN").ok();
        unsafe { std::env::remove_var("CUBA_HTTP_TOKEN") };

        let public: SocketAddr = "0.0.0.0:8787".parse().unwrap();
        assert!(ensure_loopback(&public).is_err());

        let local: SocketAddr = "127.0.0.1:8787".parse().unwrap();
        assert!(ensure_loopback(&local).is_ok());

        match prev {
            Some(v) => unsafe { std::env::set_var("CUBA_HTTP_TOKEN", v) },
            None => unsafe { std::env::remove_var("CUBA_HTTP_TOKEN") },
        }
    }

    #[test]
    fn a_systemd_socket_open_to_every_interface_is_refused_even_when_the_argument_was_loopback() {
        let prev = std::env::var("CUBA_HTTP_TOKEN").ok();
        unsafe { std::env::remove_var("CUBA_HTTP_TOKEN") };

        let from_argument: SocketAddr = DEFAULT_ADDR.parse().unwrap();
        assert!(
            ensure_loopback(&from_argument).is_ok(),
            "the address serve() validates is the default one, which is why the .socket \
             unit slipped past: fd 3 never went through this check"
        );

        let adopted: SocketAddr = "0.0.0.0:8787".parse().unwrap();
        let refusal = ensure_adopted_loopback(&adopted).expect_err(
            "ListenStream=0.0.0.0:8787 with no CUBA_HTTP_TOKEN publishes the whole brain \
             unauthenticated, and CUBA_HTTP_ADDR cannot take it back",
        );
        let text = format!("{refusal:#}");
        assert!(
            text.contains("ListenStream"),
            "the operator has to be told the fix lives in the .socket unit, not in the \
             environment they were staring at: {text}"
        );

        match prev {
            Some(v) => unsafe { std::env::set_var("CUBA_HTTP_TOKEN", v) },
            None => unsafe { std::env::remove_var("CUBA_HTTP_TOKEN") },
        }
    }

    #[test]
    fn a_systemd_socket_on_loopback_is_adopted() {
        let adopted: SocketAddr = "127.0.0.1:0".parse().unwrap();
        assert!(
            ensure_adopted_loopback(&adopted).is_ok(),
            "the shipped unit binds loopback; refusing it would break socket activation \
             for everyone who configured it correctly"
        );
    }

    fn ping(id: u64) -> Value {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "ping" })
    }

    async fn post_mcp(payload: Value) -> (StatusCode, Value) {
        let body = Bytes::from(serde_json::to_vec(&payload).expect("payload serializes"));
        let response = mcp_endpoint(
            State(state_with_clients(None, &[])),
            axum::extract::ConnectInfo(local_peer()),
            HeaderMap::new(),
            body,
        )
        .await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY)
            .await
            .expect("response body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn a_batch_past_the_limit_is_refused_whole_instead_of_dispatched() {
        let items: Vec<Value> = (0..=MAX_BATCH_ITEMS as u64).map(ping).collect();

        let (status, body) = post_mcp(Value::Array(items)).await;

        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "an 8 MiB body holds ~4,19 million two-byte entries, so an unbounded batch \
             is a free way to pin a worker; it answered {} of them instead",
            body.as_array().map(Vec::len).unwrap_or_default()
        );
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(&(MAX_BATCH_ITEMS + 1).to_string()),
            "the refusal has to name the size that was sent or the client cannot tell \
             how far over it went: {body}"
        );
        assert!(
            body.get("result").is_none() && !body.is_array(),
            "silently answering the first 256 would read as a complete batch: {body}"
        );
    }

    #[tokio::test]
    async fn a_batch_at_the_limit_is_dispatched_in_full() {
        let items: Vec<Value> = (0..MAX_BATCH_ITEMS as u64).map(ping).collect();

        let (status, body) = post_mcp(Value::Array(items)).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body.as_array().map(Vec::len),
            Some(MAX_BATCH_ITEMS),
            "the cap is the largest batch that still works, not the first one refused"
        );
    }

    #[tokio::test]
    async fn a_lone_request_is_answered_with_an_object_not_a_one_element_array() {
        let (status, body) = post_mcp(ping(7)).await;

        assert_eq!(status, StatusCode::OK);
        assert!(
            body.is_object(),
            "JSON-RPC says a single request gets a single response; wrapping it in an \
             array breaks every client that does not unwrap: {body}"
        );
        assert_eq!(body["id"], 7);
    }

    #[test]
    fn the_whole_request_deadline_leaves_room_for_a_full_handler_timeout() {
        assert!(
            request_deadline() > protocol::handler_timeout(),
            "the POST budget must exceed one call's budget, or a single tools/call that \
             uses its allowance dies of the request deadline instead"
        );
    }

    #[test]
    fn the_router_deadline_fires_after_the_one_that_can_name_what_died() {
        // The relation, not a number: the budget is read from the environment,
        // and an assertion on seconds would pass a layer that had stopped
        // depending on the handler timeout at all.
        assert!(
            router_deadline() > request_deadline(),
            "`/mcp` answers a blown budget with a JSON-RPC envelope naming the call that died, \
             and this layer can only close the socket. Firing first takes the better answer \
             away from every client that asked for one"
        );
    }

    /// The timeout layer hands back what the handler said, when the handler
    /// said it in time.
    ///
    /// This is the half that runs on every request there has ever been. The
    /// other half — the deadline actually firing — is the test below, and the
    /// two are a pair on purpose: they mount the same minimal router and demand
    /// different statuses from it, so a router assembled without the layer, or
    /// with it swallowing what the handler decided, cannot pass both.
    #[tokio::test]
    async fn a_request_answered_inside_the_budget_is_handed_back_untouched() {
        let app = Router::new()
            .route(
                "/answers",
                get(|| async { (StatusCode::IM_A_TEAPOT, "the handler's answer") }),
            )
            .layer(axum::middleware::from_fn(bound_every_request));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port");
        let port = listener.local_addr().expect("the bound address").port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let answer = reqwest::get(format!("http://127.0.0.1:{port}/answers"))
            .await
            .expect("the layered router answers");
        assert_eq!(
            answer.status().as_u16(),
            418,
            "a last resort is not a rewriter. A status this layer invented would be the daemon \
             reporting something its own handler never decided"
        );
        assert_eq!(
            answer.text().await.expect("a body"),
            "the handler's answer",
            "and the body has to survive with it: an empty 200 reads as success to every \
             monitor and as a blank page to every operator"
        );
    }

    /// A handler that stops making progress becomes a 504 that says how long
    /// the daemon waited.
    ///
    /// The budget is shrunk through `CUBA_HANDLER_TIMEOUT_SECS`, which
    /// `protocol::handler_timeout()` already reads and the README already
    /// documents — no second knob had to exist so that a test could go faster.
    /// At `1` the chain leaves eight seconds (×4 for the request, ×2 for the
    /// router), and that is the floor: `handler_timeout()` filters zero out.
    /// Eight seconds of wall clock is what this test costs, and it is the price
    /// of the only branch of this layer that had never been driven.
    ///
    /// Setting the variable here is enough because `bound_every_request` reads
    /// the budget per request, not when the router is built.
    #[tokio::test]
    async fn a_request_that_outlives_the_budget_is_closed_with_a_504_naming_the_seconds() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let _shrunk = crate::envs::ScopedEnv::set("CUBA_HANDLER_TIMEOUT_SECS", "1");

        // Read before anything hangs. If the variable ever stopped reaching
        // this layer the budget would be back to four minutes, and the only
        // symptom would be a suite that got slower — which no failure reports.
        let budget = router_deadline();
        assert!(
            budget <= Duration::from_secs(10),
            "CUBA_HANDLER_TIMEOUT_SECS has to reach this layer through \
             protocol::handler_timeout(), and it left the budget at {budget:?}"
        );

        let app = Router::new()
            .route(
                "/hangs",
                get(move || async move {
                    tokio::time::sleep(budget * 4).await;
                    (
                        StatusCode::IM_A_TEAPOT,
                        "an answer nobody is still waiting for",
                    )
                }),
            )
            .layer(axum::middleware::from_fn(bound_every_request));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port");
        let port = listener.local_addr().expect("the bound address").port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        // The client outlasts the router deliberately: with the two budgets the
        // other way round, a layer that never fired would be cut by this
        // client's own clock and read as the layer working.
        let client = reqwest::Client::builder()
            .timeout(budget * 2)
            .build()
            .expect("a client that outlasts the router's budget");
        let answer = client
            .get(format!("http://127.0.0.1:{port}/hangs"))
            .send()
            .await
            .expect(
                "nothing came back before this client gave up, which is what a request held \
                 open by a layer that never fired looks like from the outside",
            );

        assert_eq!(
            answer.status().as_u16(),
            504,
            "a handler that stopped making progress has to become a status. A socket held open \
             until the client tires of it tells a monitor nothing, and on a LAN bind the routes \
             with no budget of their own are reachable by anything that can route a packet here"
        );
        let said = answer.text().await.expect("a body");
        assert!(
            said.contains(&format!("{}s", budget.as_secs())),
            "the sentence has to name the budget that actually expired ({budget:?}): whoever \
             reads it is about to go and tune CUBA_HANDLER_TIMEOUT_SECS, and a body carrying the \
             handler's budget, or milliseconds read as seconds, sends them after the wrong knob. \
             It said: {said}"
        );
    }

    #[tokio::test]
    async fn token_comparison_rejects_wrong_and_short_tokens() {
        let mut state = state_with_clients(Some("s3cret"), &[]);

        assert_eq!(
            authorized(&state, &headers_with("authorization", "Bearer s3cret")),
            Some(Scope::Full)
        );
        assert_eq!(
            authorized(&state, &headers_with("authorization", "Bearer s3cre")),
            None
        );
        assert_eq!(
            authorized(&state, &headers_with("authorization", "Bearer wrongg")),
            None
        );
        assert_eq!(authorized(&state, &HeaderMap::new()), None);

        state.peer_token = Some(Arc::new("p33r".to_string()));
        assert_eq!(
            authorized(&state, &headers_with("authorization", "Bearer p33r")),
            Some(Scope::Peer),
            "the peer arm is checked first, and it has to be: matching the admin token first \
             and falling through would hand a peer the full surface the moment somebody set \
             both variables to the same string"
        );
        assert_eq!(
            authorized(&state, &headers_with("authorization", "Bearer s3cret")),
            Some(Scope::Full),
            "and adding a peer token must not demote the admin one"
        );
    }

    pub(super) fn state_with_clients(token: Option<&str>, clients: &[&str]) -> AppState {
        let seen = std::collections::HashMap::from_iter(
            clients.iter().map(|c| ((*c).to_string(), Instant::now())),
        );
        AppState {
            ready: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            port: 8787,
            auth_failures: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
            pool: crate::db::create_lazy_pool("postgres://unused/unused"),
            token: token.map(|t| Arc::new(t.to_string())),
            peer_token: None,
            started: Instant::now(),
            served: Arc::new(AtomicU64::new(0)),
            seen: Arc::new(std::sync::RwLock::new(seen)),
            last_activity: Arc::new(Mutex::new(Instant::now())),
            resource_tier: crate::resources::Tier::Minimal.as_str(),
        }
    }

    async fn health_body(state: AppState, headers: HeaderMap) -> Value {
        let response = health(State(state), headers).await;
        let bytes = axum::body::to_bytes(response.into_body(), MAX_BODY)
            .await
            .expect("health body");
        serde_json::from_slice(&bytes).expect("health returns JSON")
    }

    #[tokio::test]
    async fn health_names_the_clients_when_the_caller_proved_it_may_see_them() {
        let state = state_with_clients(Some("s3cret"), &["editor-a", "editor-b"]);

        let body = health_body(state, headers_with("authorization", "Bearer s3cret")).await;

        let names = body["clients"].as_array().expect("clients is a list");
        assert_eq!(names.len(), 2);
        assert!(body.get("clients_count").is_none());
    }

    #[tokio::test]
    async fn health_hides_who_is_connected_from_an_unauthenticated_caller() {
        let state = state_with_clients(Some("s3cret"), &["editor-a", "laptop-de-leandro"]);

        let body = health_body(state, HeaderMap::new()).await;

        assert!(
            body.get("clients").is_none(),
            "a client id names a machine and a person, and /health takes no auth: {body}"
        );
        assert_eq!(body["clients_count"], 2);
        assert_eq!(
            body["version"],
            env!("CARGO_PKG_VERSION"),
            "liveness must survive the redaction — that is what /health is for"
        );
    }

    #[tokio::test]
    async fn health_without_a_configured_token_still_names_them() {
        let state = state_with_clients(None, &["editor-a"]);

        let body = health_body(state, HeaderMap::new()).await;

        assert_eq!(
            body["clients"].as_array().map(Vec::len),
            Some(1),
            "with no token the daemon is loopback-only by ensure_loopback, and hiding \
             the list there would cost debugging for no security"
        );
    }
}

#[cfg(test)]
mod lan_exposure_tests {
    use super::tests::{headers_with, local_peer, state_with_clients};
    use super::*;

    #[test]
    fn a_token_anybody_could_guess_is_refused_on_an_address_other_machines_can_reach() {
        for weak in ["1234", "memoria", "changeme", "cuba", ""] {
            assert!(
                token_too_weak(false, Some(weak)).is_some(),
                "CUBA_HTTP_TOKEN={weak:?} passes the bind guard today: it only asks whether a token exists, never whether it is worth anything. On a LAN that token is the whole boundary around every observation, decision and episode the graph holds."
            );
        }
    }

    #[test]
    fn a_real_token_and_a_loopback_bind_are_both_left_alone() {
        let strong = "k7Qx2mVb9LpZ4tRw8sNc1JyH6fDgEa0U";
        assert_eq!(strong.len(), MIN_ROUTABLE_TOKEN_CHARS);
        assert!(
            token_too_weak(false, Some(strong)).is_none(),
            "a token at the minimum length is acceptable: the boundary is inclusive"
        );
        assert!(
            token_too_weak(true, Some("1234")).is_none(),
            "on loopback the token is a convenience, not a boundary. Whoever is asking already has the machine, and refusing here would break every developer running the daemon locally."
        );
        assert!(
            token_too_weak(true, None).is_none(),
            "no token on loopback is the documented default and must stay silent"
        );
    }

    #[test]
    fn health_does_not_hand_an_anonymous_caller_the_graph_backend_error() {
        let full = serde_json::json!({
            "backend": "falkor",
            "graph_name": "memory_industry",
            "url_configured": true,
            "reachable": false,
            "projected_ops": 7,
            "last_error": "connection refused to redis://10.0.0.5:6379",
        });

        let public = graph_summary_for(false, full.clone()).to_string();
        for leaked in ["10.0.0.5", "6379", "memory_industry", "url_configured"] {
            assert!(
                !public.contains(leaked),
                "/health answers unauthenticated and this body reaches anyone who can route a packet to the port. It must not carry {leaked:?}: {public}"
            );
        }
        assert!(
            public.contains("falkor") && public.contains("reachable"),
            "a monitor still has to be able to see which backend it is and whether it answers: {public}"
        );

        assert_eq!(
            graph_summary_for(true, full.clone()),
            full,
            "a caller with the admin token is the operator; redacting from them would just send them to SSH"
        );
    }

    #[test]
    fn only_an_off_box_caller_gets_told_apart() {
        use crate::session::Origin;

        assert_eq!(
            origin_of(true, None, "127.0.0.1"),
            Origin::Local,
            "every caller on this machine is this machine"
        );
        assert_eq!(
            origin_of(true, Some("laptop"), "127.0.0.1"),
            Origin::Local,
            "loopback stays one bucket even when a client volunteers a machine id, or a developer who sets the header would silently start a new session"
        );
        assert_eq!(
            origin_of(false, Some("ws-7"), "10.0.0.2"),
            Origin::Remote("ws-7".into()),
            "a client that names itself beats an address, which DHCP can move"
        );
        assert_eq!(
            origin_of(false, Some("   "), "10.0.0.2"),
            Origin::Remote("10.0.0.2".into()),
            "a blank header is not an identity"
        );
        assert_eq!(
            origin_of(false, None, "10.0.0.3"),
            Origin::Remote("10.0.0.3".into()),
            "with nothing volunteered the address is the only thing that distinguishes two machines, and it is what protects an operator who copied one config to every workstation"
        );
    }

    #[test]
    fn the_port_opens_after_the_models_are_warm_and_the_announcement_comes_last() {
        // The daemon used to bind, announce itself as listening, and only
        // then spawn the warm-up detached. The announcement was a lie for as
        // long as the load took, and a search that arrived in that window got
        // 200 OK with an unreranked ranking in it.
        let source = include_str!("http.rs");
        let body = source
            .split_once("pub async fn serve_pool(")
            .expect("serve_pool is in this file")
            .1;
        let body = body
            .split_once(
                "
}",
            )
            .expect("the function ends")
            .0;

        let bind = body.find("bind_listener(").expect("the bind");
        let warm = body.find("warm_before_serving(").expect("the warm-up");
        let announce = body.find("daemon listening").expect("the announcement");
        let serve = body.find("axum::serve(").expect("the server");

        assert!(
            bind < warm,
            "bind first: a taken port, a missing token or a weak one all fail in a second, and finding that out after two minutes of loading helps nobody"
        );
        assert!(
            warm < announce && announce < serve,
            "the models have to be warm before the daemon claims to be listening, and the claim has to come before anything is served"
        );
    }

    #[test]
    fn a_warm_up_that_never_finishes_does_not_keep_the_port_shut() {
        let source = include_str!("http.rs");
        let body = source
            .split_once("pub async fn serve_pool(")
            .expect("serve_pool is in this file")
            .1;
        let body = body
            .split_once(
                "
}",
            )
            .expect("the function ends")
            .0;
        // The bound lives in warm_before_serving now, which is what serve_pool
        // calls; checking serve_pool's own text would only prove where a line
        // happens to sit.
        let warm_fn = source
            .split_once("async fn warm_before_serving(")
            .expect("warm_before_serving is in this file")
            .1;
        let warm_fn = warm_fn
            .split_once(
                "
}",
            )
            .expect("the function ends")
            .0;
        assert!(
            body.contains("warm_before_serving(")
                && warm_fn.contains("warm_before_serve_budget()")
                && warm_fn.contains("tokio::time::timeout("),
            "the wait for the models has to be bounded. Warming before the bind was the other obvious order and it is worse: a model that never loads leaves the daemon unbound, so nobody can even ask it what is wrong."
        );
    }

    #[test]
    fn the_warm_up_budget_falls_back_rather_than_becoming_zero() {
        assert_eq!(
            warm_before_serve_budget(),
            std::time::Duration::from_secs(180),
            "the default has to leave room for a cold cross-encoder"
        );
    }

    #[test]
    fn the_panel_is_not_published_on_an_address_other_machines_can_reach() {
        assert!(
            panel_route_enabled(true, true, false),
            "on loopback CUBA_PANEL=1 is enough: whoever can reach it already has the machine"
        );
        assert!(
            !panel_route_enabled(false, true, false),
            "CUBA_PANEL=1 on a routable bind used to publish a page showing the daemon state, the connected clients and the recent calls to anyone who could route a packet. came_through_a_proxy does not help: a LAN bind is not a proxy, and its own docs say a raw TCP forward carries no header either."
        );
        assert!(
            panel_route_enabled(false, true, true),
            "CUBA_PANEL_PUBLIC=1 is somebody saying out loud that they meant it"
        );
        assert!(
            !panel_route_enabled(true, false, true),
            "public does not turn the panel on by itself; it only lifts the address restriction"
        );
        assert!(!panel_route_enabled(false, false, false));
    }

    #[test]
    fn a_page_from_somewhere_else_does_not_get_to_talk_to_the_daemon() {
        assert!(
            origin_allowed(None, 8787),
            "every MCP client there is sends no Origin at all; refusing those would break all of them and protect nobody"
        );
        assert!(origin_allowed(Some("http://127.0.0.1:8787"), 8787));
        assert!(origin_allowed(Some("http://localhost:8787"), 8787));
        assert!(
            origin_allowed(Some("http://localhost"), 8787),
            "a bare localhost origin is still the machine itself"
        );

        assert!(
            !origin_allowed(Some("https://evil.example"), 8787),
            "this is the DNS-rebinding case, and it matters most on the documented default: loopback with no token. Any page the operator opens can reach 127.0.0.1:8787, and the token is not there to stop it because the daemon is local."
        );
        assert!(
            !origin_allowed(Some("null"), 8787),
            "a sandboxed iframe sends Origin: null; treating that as local would hand it the daemon"
        );
        assert!(
            !origin_allowed(Some("http://localhost:3000"), 8787),
            "another local service is not this one: a dev server on the same machine should not be able to drive the brain"
        );
    }

    #[test]
    fn no_cors_headers_are_ever_emitted() {
        // A fence, not a check. `Content-Type: application/json` already forces
        // a preflight, and with nothing answering it the browser refuses the
        // request by itself. Adding a permissive CORS layer to "fix" a blocked
        // fetch would remove the protection doing the work and leave only the
        // Origin check above.
        //
        // The needles are assembled at run time so this test does not match its
        // own source: the first attempt sliced at `#[cfg(test)]` to avoid that,
        // and there is one of those halfway up the file, so the slice stopped
        // before the handlers and the fence caught nothing at all.
        let source = include_str!("http.rs");
        for forbidden in [
            format!("{}{}", "access-control-", "allow-origin"),
            format!("{}{}", "Cors", "Layer"),
            format!("{}{}", "tower_http::", "cors"),
        ] {
            assert!(
                !source
                    .to_ascii_lowercase()
                    .contains(&forbidden.to_ascii_lowercase()),
                "{forbidden} reached http.rs. Nothing here needs CORS: no browser page is supposed to call this daemon except its own, and those are same-origin."
            );
        }
    }

    #[test]
    fn a_wrong_token_is_not_free_to_retry_forever() {
        use std::time::Duration;

        assert_eq!(
            auth_brake(0, Duration::from_secs(0)),
            None,
            "an address that has done nothing wrong waits for nothing"
        );
        assert_eq!(
            auth_brake(MAX_AUTH_FAILURES - 1, Duration::from_secs(1)),
            None,
            "a handful of wrong tokens is a misconfigured client, not an attack"
        );

        let wait = auth_brake(MAX_AUTH_FAILURES, Duration::from_secs(10))
            .expect("ten wrong tokens inside the window has to cost something");
        assert_eq!(
            wait,
            AUTH_FAILURE_WINDOW - Duration::from_secs(10),
            "the wait is what is left of the window: same_secret compares in constant time so there is no timing to leak, but nothing was slowing an attacker on the LAN between attempts"
        );

        assert_eq!(
            auth_brake(MAX_AUTH_FAILURES * 100, AUTH_FAILURE_WINDOW),
            None,
            "the window has to expire, or one burst would lock an address out for the life of the daemon"
        );
    }

    #[tokio::test]
    async fn the_warm_up_is_deferred_under_either_spelling_of_the_knob() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        {
            let _preferred = crate::envs::ScopedEnv::cleared("MEMORY_INDUSTRY_WARM_RERANKER");
            let _legacy = crate::envs::ScopedEnv::set("CUBA_WARM_RERANKER", "0");
            assert!(
                !warm_reranker_eagerly(),
                "every machine in the field is configured with the legacy name; reading the \
                 new one first must not stop the old one from working"
            );
        }
        {
            let _preferred = crate::envs::ScopedEnv::set("MEMORY_INDUSTRY_WARM_RERANKER", "0");
            let _legacy = crate::envs::ScopedEnv::cleared("CUBA_WARM_RERANKER");
            assert!(
                !warm_reranker_eagerly(),
                "this knob answered only to CUBA_WARM_RERANKER while README.md and \
                 .env.example offered the MEMORY_INDUSTRY_ name, so an operator deferring the \
                 load with the documented name got a daemon that loaded anyway"
            );
        }
        {
            let _preferred = crate::envs::ScopedEnv::set("MEMORY_INDUSTRY_WARM_RERANKER", "1");
            let _legacy = crate::envs::ScopedEnv::set("CUBA_WARM_RERANKER", "0");
            assert!(
                warm_reranker_eagerly(),
                "with both set the preferred name decides, or a stale line in a unit file \
                 would quietly override the one the operator edited"
            );
        }
    }

    #[test]
    fn warming_is_on_unless_somebody_turns_it_off() {
        assert!(
            warm_eagerly_from(None),
            "on by default: deferring the load does not save the cost, it moves it inside the first search that asks for reranking, which has a 20 s budget and a 1.1 GB read to pay for"
        );
        for off in ["0", "off", "false", "no"] {
            assert!(!warm_eagerly_from(Some(off)), "{off} has to switch it off");
        }
        for on in ["1", "on", "true", "yes", "", "maybe"] {
            assert!(
                warm_eagerly_from(Some(on)),
                "{on} is not one of the off words, and an unrecognised value must not silently defer the load"
            );
        }
    }

    #[test]
    fn a_stale_window_starts_the_count_over_instead_of_accumulating() {
        let (count, _) = next_failure(None);
        assert_eq!(count, 1, "the first wrong token is one, not zero");

        let fresh = Instant::now();
        let (count, since) = next_failure(Some((3, fresh)));
        assert_eq!(count, 4);
        assert_eq!(
            since, fresh,
            "the window keeps its start, or every failure would push the deadline and an address could never serve its wait"
        );
    }

    #[test]
    fn the_allowlist_matches_whole_origins_and_ignores_its_own_gaps() {
        assert!(
            origin_allowed_with(Some("https://ops.example"), 8787, "https://ops.example"),
            "an origin somebody put on the list is allowed; that is what the list is for"
        );
        assert!(
            origin_allowed_with(
                Some("https://ops.example"),
                8787,
                "http://a.example, https://ops.example ,"
            ),
            "entries are trimmed and a trailing comma is not an entry"
        );
        assert!(
            !origin_allowed_with(Some("https://evil.example"), 8787, "https://ops.example"),
            "being on a list of one does not admit everyone else"
        );
        assert!(
            !origin_allowed_with(
                Some("https://ops.example.evil.test"),
                8787,
                "https://ops.example"
            ),
            "a prefix is not a match: a whole origin or nothing, or any attacker who can register a longer name gets in"
        );
        assert!(
            !origin_allowed_with(Some("https://evil.example"), 8787, ""),
            "an empty list is an empty list. Splitting it yields one empty entry, and matching that against an origin would admit everything the moment nobody configured anything - which is the default."
        );
        assert!(
            origin_allowed_with(Some(""), 8787, ""),
            "a blank Origin header is treated as absent, like every non-browser client"
        );
    }

    #[tokio::test]
    async fn the_brake_counts_failures_and_a_good_token_clears_them() {
        let state = state_with_clients(Some("tok"), &[]);
        let who = local_peer().ip();

        assert_eq!(
            brake_for(&state, who),
            None,
            "an address nobody has heard of waits for nothing"
        );

        for _ in 0..MAX_AUTH_FAILURES {
            record_auth_failure(&state, who);
        }
        let wait = brake_for(&state, who)
            .expect("ten wrong tokens inside the window has to cost this address something");
        assert!(
            wait <= AUTH_FAILURE_WINDOW && !wait.is_zero(),
            "the wait is what is left of the window, not zero and not more than all of it: {wait:?}"
        );

        clear_auth_failures(&state, who);
        assert_eq!(
            brake_for(&state, who),
            None,
            "a correct token clears the address. Without this an editor that reconnects after one bad config would stay throttled with nothing it could do about it."
        );
    }

    #[test]
    fn a_window_that_has_already_expired_starts_over() {
        let stale = Instant::now()
            .checked_sub(AUTH_FAILURE_WINDOW + std::time::Duration::from_secs(1))
            .expect("the process started after the window length");

        let (count, since) = next_failure(Some((9, stale)));
        assert_eq!(
            count, 1,
            "the old window is spent, so this is the first failure of a new one. Carrying the count forward would let a bad afternoon a week ago decide today."
        );
        assert!(
            since > stale,
            "and the new window starts now, or the address would be judged against a clock that already ran out"
        );
    }

    #[tokio::test]
    async fn a_page_from_elsewhere_is_refused_and_a_client_with_no_origin_is_not() {
        let state = state_with_clients(None, &[]);

        assert!(
            refuse_foreign_origin(&state, &HeaderMap::new()).is_none(),
            "no Origin at all is every MCP client there is, and none of them is a browser"
        );
        assert!(
            refuse_foreign_origin(&state, &headers_with("origin", "https://evil.example"))
                .is_some(),
            "this is the DNS-rebinding case: on loopback with no token, which is the documented default, any page the operator opens can reach the daemon"
        );
        assert!(
            refuse_foreign_origin(&state, &headers_with("origin", "http://127.0.0.1:8787"))
                .is_none(),
            "the daemon own page has to keep working"
        );
    }

    #[tokio::test]
    async fn activity_is_remembered_for_the_reaper_and_for_health() {
        let state = state_with_clients(None, &[]);
        assert_eq!(state.seen.read().map(|g| g.len()).unwrap_or(9), 0);

        note_activity(&state, "cursor::chat-a");
        assert!(
            state
                .seen
                .read()
                .is_ok_and(|g| g.contains_key("cursor::chat-a")),
            "the idle reaper purges what it has not seen, and /health counts it. A client that never registers is reaped while it is working."
        );
    }

    #[test]
    fn the_window_closes_exactly_when_it_says_it_does() {
        assert!(
            window_still_open(AUTH_FAILURE_WINDOW - std::time::Duration::from_nanos(1)),
            "a nanosecond before the end is still inside the window"
        );
        assert!(
            !window_still_open(AUTH_FAILURE_WINDOW),
            "a window that has run its full length is over. Accepting it would keep every count alive one tick longer than advertised, and an address serving its wait would never see it end."
        );
        assert!(!window_still_open(
            AUTH_FAILURE_WINDOW + std::time::Duration::from_secs(1)
        ));
        assert!(window_still_open(std::time::Duration::ZERO));
    }
}

/// The `/health` verdict and the block it is computed from, where the second
/// judge can reach them.
///
/// `quality-gate.sh` runs `cargo mutants -- --lib`, which executes only the
/// `#[cfg(test)]` modules that live inside `rust/src`. The same truth table is
/// asserted from outside in `rust/tests/v042_health_says_what_degraded.rs`,
/// over the public API and against a live daemon: that file is the contract a
/// reader should start from, and this one is the copy the mutation judge
/// actually runs. They have to stay identical. A `||` quietly turned into `&&`
/// here — "every subsystem is broken" instead of "any of them is" — survives a
/// green integration suite, because the mutation never reaches `rust/tests`.
#[cfg(test)]
mod health_report_tests {
    use super::*;

    /// A report carrying nothing but the facts a row varies.
    fn report(ready: bool, gpu_degraded: bool, states: [&'static str; 3]) -> RuntimeReport {
        let at = |state: &'static str| ModelState {
            state,
            device: Some("cpu"),
            reason: None,
        };
        RuntimeReport {
            mode: "local",
            resource_tier: "full",
            ready,
            embedder: at(states[0]),
            reranker: at(states[1]),
            nli: at(states[2]),
            gpu_build: None,
            gpu_degraded,
            gpu_placement: "embedder=cpu reranker=cpu nli=cpu".to_string(),
            llm: crate::llm_cli::LlmSummary {
                configured: false,
                backend: None,
                model: None,
                base_url: None,
            },
        }
    }

    #[test]
    fn degraded_when_any_subsystem_is() {
        // (database answers, models warm, gpu asked for and missing, reranker
        //  state) -> the one word a monitor matches on.
        //
        // Written out rather than computed: an expectation derived from the
        // same condition the function uses is a tautology, and this table is
        // the whole thing standing between «degraded» and a word that quietly
        // changes. The words are the wire contract, so they are literals here
        // and not the constants the function returns.
        let table: [(bool, bool, bool, &'static str, &'static str); 16] = [
            (true, true, false, "loaded", "ok"),
            (true, false, false, "loaded", "starting"),
            (true, true, true, "loaded", "degraded"),
            (true, false, true, "loaded", "degraded"),
            (true, true, false, "failed", "degraded"),
            (true, false, false, "failed", "degraded"),
            (true, true, true, "failed", "degraded"),
            (true, false, true, "failed", "degraded"),
            (false, true, false, "loaded", "degraded"),
            (false, false, false, "loaded", "degraded"),
            (false, true, true, "loaded", "degraded"),
            (false, false, true, "loaded", "degraded"),
            (false, true, false, "failed", "degraded"),
            (false, false, false, "failed", "degraded"),
            (false, true, true, "failed", "degraded"),
            (false, false, true, "failed", "degraded"),
        ];

        for (db_ok, ready, gpu_degraded, reranker, expected) in table {
            let runtime = report(ready, gpu_degraded, ["loaded", reranker, "loaded"]);
            assert_eq!(
                overall_status(db_ok, &runtime),
                expected,
                "db_ok={db_ok} ready={ready} gpu_degraded={gpu_degraded} reranker={reranker}"
            );
        }

        assert_eq!(
            overall_status(true, &report(true, false, ["loaded", "loaded", "failed"])),
            "degraded",
            "the NLI model is in the same list as the other two. A check that looked at the \
             reranker alone would pass every row of the table above and still miss a model \
             that is on disk and will not open"
        );

        assert_eq!(
            overall_status(true, &report(true, false, ["fallback", "loaded", "loaded"])),
            "ok",
            "a machine that never had an embedding model is Tier::Minimal, not a fault, and \
             this daemon is supported there. Reporting every one of them as degraded forever \
             is how a field stops being read — which is the failure this endpoint exists to \
             prevent"
        );

        assert_eq!(
            overall_status(true, &report(true, false, ["failed", "loaded", "loaded"])),
            "degraded",
            "and the other half of that same distinction: an embedder whose model was there \
             and whose session would not open is a fault. Until `failed` existed for this \
             model the row above was the only answer either machine could get, so the two were \
             indistinguishable from outside — which is what made the supported one look broken \
             and the broken one look supported"
        );

        assert_eq!(
            overall_status(true, &report(false, false, ["loaded", "loaded", "loaded"])),
            "starting",
            "and «still loading» is not «broken». It fixes itself; degraded does not, and the \
             two ask different things of whoever is reading"
        );
    }

    #[test]
    fn the_gpu_build_field_names_a_provider_or_says_nothing() {
        // The vocabulary, not the `cfg!` chain: an expectation assembled from
        // the same flags the function reads would agree with any word it put
        // there.
        assert!(
            matches!(
                compiled_gpu_provider(),
                None | Some("cuda") | Some("directml")
            ),
            "an operator reads `gpu.build` to learn which provider this binary was compiled \
             against. A word ONNX Runtime has never heard of sends them hunting for a driver \
             that was never the problem"
        );
    }

    #[test]
    fn the_embedder_is_warming_while_the_models_are_still_loading() {
        assert_eq!(
            embedder_state(false).state,
            "warming",
            "`ready` is the guard that keeps `/health` from calling `is_model_loaded()`, which \
             resolves the cell that loads the model. Asking before the warm-up finished is the \
             thing that makes a monitor's poll hang for seconds on a cold process"
        );
    }

    #[tokio::test]
    async fn the_nli_model_is_absent_only_when_there_is_nothing_to_load() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        // `nli::available()` answers from `$HOME/.cache`, so this developer's
        // disk and the gate box would otherwise run different branches of the
        // function. Both names move: the lookup tries HOME and falls back to
        // USERPROFILE.
        let root =
            std::env::temp_dir().join(format!("memory-industry-http-nli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the temp dir is writable");
        let home = root.display().to_string();
        let _home = crate::envs::ScopedEnv::set("HOME", &home);
        let _userprofile = crate::envs::ScopedEnv::set("USERPROFILE", &home);
        let _plan = crate::envs::ScopedEnv::cleared("CUBA_NLI_PATH");

        assert_eq!(
            nli_state().state,
            "absent",
            "nothing on disk is not the same as a model waiting to be opened, and only the \
             first of those tells an operator to go and install something"
        );

        let installed = root
            .join(".cache")
            .join("memory-industry")
            .join("models-nli");
        std::fs::create_dir_all(&installed).expect("the temp dir is writable");
        std::fs::write(installed.join("model.onnx"), b"not a real graph")
            .expect("the temp dir is writable");
        assert_eq!(
            nli_state().state,
            "configured",
            "and with a model in the cache the answer has to change. Both rows are here \
             because either one alone passes on an `nli_state` that never looks at the disk"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_model_block_carries_its_state_its_device_and_its_reason() {
        let broken = ModelState {
            state: "failed",
            device: Some("gpu"),
            reason: Some("the session did not open".to_string()),
        };
        let json = model_json(&broken);
        assert_eq!(json["state"], "failed");
        assert_eq!(json["device"], "gpu");
        assert_eq!(
            json["reason"], "the session did not open",
            "the reason is why this block exists at all: `doctor` used to guess at it, and \
             guessed wrong"
        );

        let quiet = ModelState {
            state: "loaded",
            device: Some("cpu"),
            reason: None,
        };
        assert_eq!(
            model_json(&quiet)["reason"],
            Value::Null,
            "a model that is simply running says nothing, and the key stays so a reader never \
             has to tell «no reason» from «this daemon is too old to report one»"
        );
    }

    #[test]
    fn a_model_with_no_measured_device_serves_the_key_as_null_and_does_not_drop_it() {
        let unmeasured = ModelState {
            state: "configured",
            device: None,
            reason: Some("loads on its first batch".to_string()),
        };
        let json = model_json(&unmeasured);

        assert!(
            json.as_object()
                .is_some_and(|block| block.contains_key("device")),
            "the key stays when nobody measured the device. A reader, and the monitor that \
             matches on `runtime.reranker.device`, must never have to tell «null» from «this \
             daemon is too old to say»: {json}"
        );
        assert_eq!(
            json["device"],
            Value::Null,
            "and what it holds is `null`, not a guess: a session that never opened runs on no \
             device at all"
        );
        assert_eq!(
            json["state"], "configured",
            "control: the rest of the block is still there, so the missing device is the \
             decision and not an empty block"
        );
    }

    #[test]
    fn the_runtime_block_names_every_subsystem_it_was_built_from() {
        let json = runtime_json(&report(true, true, ["loaded", "off", "absent"]));

        assert_eq!(json["mode"], "local");
        assert_eq!(json["resource_tier"], "full");
        assert_eq!(json["embedder"]["state"], "loaded");
        assert_eq!(
            json["reranker"]["state"], "off",
            "the three models are named separately on purpose: «something is degraded» with \
             no subject is the answer that sent an operator to read the source"
        );
        assert_eq!(json["nli"]["state"], "absent");
        assert_eq!(json["gpu"]["build"], Value::Null);
        assert_eq!(
            json["gpu"]["degraded"], true,
            "asked for a card and not using one is the state a placement summary alone does \
             not distinguish from never having asked for one"
        );
        assert_eq!(
            json["gpu"]["placement"],
            "embedder=cpu reranker=cpu nli=cpu"
        );
        assert_eq!(json["llm"]["configured"], false);
        assert_eq!(json["llm"]["backend"], Value::Null);
        assert_eq!(json["llm"]["base_url"], Value::Null);
    }

    #[test]
    fn four_facts_decide_what_health_says_about_the_reranker() {
        // (a load failure, a directory to look in, a model inside it, a
        //  session already attempted) -> the word `/health` prints.
        //
        // Written out rather than derived, and the whole reason the decision
        // was pulled out of the accessors: it used to read `failure_reason()`
        // itself, which is a process-wide `OnceLock` that another test in this
        // binary resolves. Whether a test here reached the second branch at
        // all depended on the order libtest happened to schedule them in.
        //
        // Two rows are combinations no machine produces — a model «inside» a
        // directory that did not resolve — and they are here on purpose: they
        // pin which check wins instead of leaving the order to whoever reads
        // the function next.
        let table: [(Option<&str>, bool, bool, bool, &'static str); 8] = [
            (Some("the session did not open"), true, true, true, "failed"),
            (
                Some("the session did not open"),
                false,
                false,
                false,
                "failed",
            ),
            (None, false, false, false, "off"),
            (None, false, true, true, "off"),
            (None, true, false, false, "absent"),
            (None, true, false, true, "absent"),
            (None, true, true, false, "configured"),
            (None, true, true, true, "loaded"),
        ];

        for (failure, dir_resolved, configured, status_resolved, expected) in table {
            let state = reranker_state_from(
                failure.map(String::from),
                None,
                dir_resolved,
                configured,
                status_resolved,
            );
            assert_eq!(
                state.state, expected,
                "failure={failure:?} dir_resolved={dir_resolved} configured={configured} \
                 status_resolved={status_resolved}"
            );
            assert_eq!(
                state.device, None,
                "no session was measured, so no branch may name a device. The word used to be \
                 the one the placement path *chose*, in every branch, which is where `cpu` on a \
                 model that never opened came from: failure={failure:?} \
                 dir_resolved={dir_resolved} configured={configured} \
                 status_resolved={status_resolved}"
            );

            let measured = reranker_state_from(
                failure.map(String::from),
                Some(&crate::gpu::Placement::Gpu),
                dir_resolved,
                configured,
                status_resolved,
            );
            assert_eq!(
                measured.device,
                if expected == "loaded" {
                    Some("gpu")
                } else {
                    None
                },
                "a placement that exists is read only by the row whose session is open; every \
                 other row keeps `null` even though the registry can already hold a value (it is \
                 written while `commit_from_file` is still loading): expected={expected}"
            );
        }

        assert_eq!(
            reranker_state_from(None, None, true, true, true).reason,
            None,
            "a reranker that is loaded says nothing more. «loads on its first batch» on a model \
             that already loaded leaves someone waiting for a load that has happened"
        );
    }

    #[test]
    fn four_facts_decide_what_health_says_about_the_nli_model() {
        // (a load failure, the resource plan deferring it, a model in the
        //  cache, a session already attempted) -> the word `/health` prints.
        //
        // The same shape as the reranker table above and for the same reason:
        // `nli::status_resolved()` is a process-wide `OnceLock`, so a test
        // that reached the live accessors would be asserting against whatever
        // order libtest scheduled this binary in.
        //
        // The two failure rows are combinations no machine produces: a reason
        // only exists once a session was attempted and found a model, so it
        // cannot arrive with the plan having deferred the load, nor with the
        // cell unresolved. They are here on purpose — they pin that a failure
        // outranks every other answer instead of leaving the order to whoever
        // reads the function next.
        let table: [(Option<&str>, bool, bool, bool, &'static str); 8] = [
            (Some("the session did not open"), true, true, true, "failed"),
            (
                Some("the session did not open"),
                false,
                false,
                false,
                "failed",
            ),
            (None, true, true, false, "off"),
            (None, true, true, true, "off"),
            (None, false, false, false, "absent"),
            (None, false, false, true, "absent"),
            (None, false, true, false, "configured"),
            (None, false, true, true, "loaded"),
        ];

        for (failure, deferred, available, status_resolved, expected) in table {
            let state = nli_state_from(
                failure.map(String::from),
                None,
                deferred,
                available,
                status_resolved,
            );
            assert_eq!(
                state.state, expected,
                "failure={failure:?} deferred={deferred} available={available} \
                 status_resolved={status_resolved}"
            );
            assert_eq!(
                state.device, None,
                "no session was measured, so no branch may name a device: failure={failure:?} \
                 deferred={deferred} available={available} status_resolved={status_resolved}"
            );

            let measured = nli_state_from(
                failure.map(String::from),
                Some(&crate::gpu::Placement::Gpu),
                deferred,
                available,
                status_resolved,
            );
            assert_eq!(
                measured.device,
                if expected == "loaded" {
                    Some("gpu")
                } else {
                    None
                },
                "only the row whose session is open reads the registry: expected={expected}"
            );
        }

        assert_eq!(
            nli_state_from(None, None, false, true, true).reason,
            None,
            "an NLI model that already loaded says nothing more. «loads on its first use» on a \
             model that has loaded leaves someone waiting for a load that has happened"
        );
    }

    #[test]
    fn three_facts_decide_what_health_says_about_the_embedder() {
        // (a load failure, the warm-up has finished, the session is open) ->
        // the word `/health` prints.
        //
        // `loaded` is `ready && is_model_loaded()` at the call site, so the
        // two rows with `ready:false, loaded:true` are combinations no machine
        // produces. They are here to pin that «still loading» wins over a flag
        // that cannot yet mean anything, rather than leaving the order to the
        // next reader.
        let table: [(Option<&str>, bool, bool, &'static str); 8] = [
            (Some("the session did not open"), true, true, "failed"),
            (Some("the session did not open"), true, false, "failed"),
            (Some("the session did not open"), false, true, "failed"),
            (Some("the session did not open"), false, false, "failed"),
            (None, false, false, "warming"),
            (None, false, true, "warming"),
            (None, true, true, "loaded"),
            (None, true, false, "fallback"),
        ];

        for (failure, ready, loaded, expected) in table {
            let measured = embedder_state_from(
                failure.map(String::from),
                Some(&crate::gpu::Placement::Gpu),
                ready,
                loaded,
            );
            assert_eq!(
                measured.state, expected,
                "failure={failure:?} ready={ready} loaded={loaded}"
            );
            assert_eq!(
                measured.device,
                match expected {
                    "fallback" => Some("cpu"),
                    "loaded" => Some("gpu"),
                    _ => None,
                },
                "the hash fallback runs on this CPU whatever the registry says, an open \
                 session is where it opened, and a model that failed or is still warming runs \
                 nowhere yet: failure={failure:?} ready={ready} loaded={loaded}"
            );

            let unmeasured = embedder_state_from(failure.map(String::from), None, ready, loaded);
            assert_eq!(
                unmeasured.device,
                if expected == "fallback" {
                    Some("cpu")
                } else {
                    None
                },
                "with nothing in the registry only the fallback names a device, because it is \
                 the one answer that does not depend on a session: failure={failure:?} \
                 ready={ready} loaded={loaded}"
            );
        }

        assert_eq!(
            embedder_state_from(None, Some(&crate::gpu::Placement::Gpu), true, true).reason,
            None,
            "an embedder that is running says nothing more"
        );
        assert!(
            embedder_state_from(None, None, true, false)
                .reason
                .is_some_and(|why| why.contains("hash fallback")),
            "and a machine on the hash fallback has to say so: «search is lexical only» is the \
             sentence that explains results an operator would otherwise file as a ranking bug"
        );
        assert_eq!(
            embedder_state_from(None, Some(&crate::gpu::Placement::Gpu), false, false).reason,
            None,
            "«warming» says everything there is to say; a reason here would be a sentence that \
             stops being true a second later"
        );
    }

    #[test]
    fn a_failed_nli_or_embedder_never_carries_its_path_either() {
        const CANARY_USER: &str = "canaryoperator";
        let nli_dir =
            std::path::PathBuf::from(format!("C:\\Users\\{CANARY_USER}\\.cache\\models-nli"));
        // `{dir:?}` is how `nli::init` writes it, quotes and all.
        let nli_failure = format!("no hay model.onnx ni model_quantized.onnx en {nli_dir:?}");
        let embedder_failure = format!(
            "tokenizer.json not found at C:\\Users\\{CANARY_USER}\\.cache\\models\\tokenizer.json"
        );

        // Positive controls. Without them «the canary is not in the body» also
        // passes on a body that never carried a reason at all.
        for fixture in [&nli_failure, &embedder_failure] {
            assert!(
                fixture.contains(CANARY_USER),
                "the fixture is broken: {fixture:?} never had a path in it"
            );
        }

        let mut runtime = report(true, false, ["failed", "loaded", "failed"]);
        runtime.nli = nli_state_from(Some(nli_failure), None, false, true, true);
        runtime.embedder = embedder_state_from(Some(embedder_failure), None, true, false);

        assert_eq!(
            runtime.nli.reason.as_deref(),
            Some(
                "no hay model.onnx ni model_quantized.onnx en the NLI directory (see \
                 CUBA_NLI_PATH)"
            ),
            "the path goes and the actionable half stays — and the variable it names is the \
             NLI one. A failed NLI that came back pointing at CUBA_RERANKER_PATH would send the \
             operator to change the wrong knob, which is worse than saying nothing"
        );
        assert!(
            runtime
                .embedder
                .reason
                .as_deref()
                .is_some_and(|why| why.contains("ONNX_MODEL_PATH")),
            "and the embedder names the variable that sets *its* directory: {:?}",
            runtime.embedder.reason
        );

        let served = runtime_json(&runtime).to_string();
        assert!(
            served.contains("model.onnx") && served.contains("tokenizer.json"),
            "positive control: both reasons have to reach the block at all, or every canary \
             below passes on a block that says nothing: {served}"
        );
        for canary in [CANARY_USER, ".cache"] {
            assert!(
                !served.contains(canary),
                "{canary:?} was served in the runtime block. A model path is inventory, and on \
                 Windows it carries the operator's user name; the sentence with the path in it \
                 belongs to `doctor`, which runs on their machine: {served}"
            );
        }
    }

    #[test]
    fn a_model_path_never_reaches_the_runtime_block() {
        const CANARY_USER: &str = "canaryoperator";
        let dir = std::path::PathBuf::from(format!(
            "C:\\Users\\{CANARY_USER}\\.cache\\memory-industry\\reranker"
        ));
        // `{model_dir:?}` is how `init_session` writes it, quotes and all.
        let failure = format!("no model.onnx / model_quantized.onnx found in {dir:?}");

        // Positive control. Without it «the canary is not in the body» also
        // passes on a body that never carried a reason at all.
        assert!(
            failure.contains(CANARY_USER),
            "the fixture is broken: the sentence this test scrubs never had a path in it"
        );

        let mut runtime = report(true, false, ["loaded", "failed", "loaded"]);
        runtime.reranker = reranker_state_from(Some(failure), None, true, true, false);

        assert_eq!(
            runtime.reranker.reason.as_deref(),
            Some(
                "no model.onnx / model_quantized.onnx found in the reranker directory (see \
                 CUBA_RERANKER_PATH)"
            ),
            "the path goes and the rest stays. The two file names it looked for and the \
             variable that sets the directory are what an operator can act on; a sentence cut \
             down to «no model found» would keep the promise and lose the operator"
        );

        let served = runtime_json(&runtime).to_string();
        assert!(
            served.contains("model.onnx"),
            "positive control: the reason has to reach the block at all, or every canary below \
             passes on a block that says nothing: {served}"
        );
        for canary in [CANARY_USER, ".cache"] {
            assert!(
                !served.contains(canary),
                "{canary:?} was served in the runtime block. A model path is inventory, and on \
                 Windows it carries the operator's user name; the sentence with the path in it \
                 belongs to `doctor`, which runs on their machine: {served}"
            );
        }
    }

    /// What ONNX Runtime said on this machine on 2026-10-12, word for word,
    /// with the operator's name left as a parameter so a test can demand that
    /// it is absent from whatever is served.
    fn provider_refusal(user: &str) -> String {
        format!(
            r#"E:\_work\1\s\onnxruntime\core\session\provider_bridge_ort.cc:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading "C:\Users\{user}\.cache\cuba-memorys\onnxruntime\onnxruntime_providers_cuda.dll" which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")"#
        )
    }

    #[derive(Debug, Clone, Copy)]
    enum Which {
        Embedder,
        Reranker,
        Nli,
    }

    /// The row of `which` that lands on `state`, with the facts that put a real
    /// machine there, and the registry entry the caller wants to try on it.
    fn row(which: Which, state: &str, placement: Option<&crate::gpu::Placement>) -> ModelState {
        let failure = || Some("the session did not open".to_string());
        match (which, state) {
            (Which::Embedder, "failed") => embedder_state_from(failure(), placement, true, false),
            (Which::Embedder, "warming") => embedder_state_from(None, placement, false, false),
            (Which::Embedder, "loaded") => embedder_state_from(None, placement, true, true),
            (Which::Embedder, "fallback") => embedder_state_from(None, placement, true, false),
            (Which::Reranker, "failed") => {
                reranker_state_from(failure(), placement, true, true, false)
            }
            (Which::Reranker, "off") => reranker_state_from(None, placement, false, false, false),
            (Which::Reranker, "absent") => reranker_state_from(None, placement, true, false, false),
            (Which::Reranker, "configured") => {
                reranker_state_from(None, placement, true, true, false)
            }
            (Which::Reranker, "loaded") => reranker_state_from(None, placement, true, true, true),
            (Which::Nli, "failed") => nli_state_from(failure(), placement, false, true, false),
            (Which::Nli, "off") => nli_state_from(None, placement, true, true, false),
            (Which::Nli, "absent") => nli_state_from(None, placement, false, false, false),
            (Which::Nli, "configured") => nli_state_from(None, placement, false, true, false),
            (Which::Nli, "loaded") => nli_state_from(None, placement, false, true, true),
            _ => panic!("the fixture is broken: {which:?} has no row that lands on {state:?}"),
        }
    }

    /// The device `/health` prints is where the session *opened*, and only a
    /// session that is open has one.
    ///
    /// The old word was the placement path's choice, copied into every branch:
    /// a reranker that never opened said `cpu`, a reranker that was asked for
    /// the card and opened on the CPU said `gpu`, and on the build the gate
    /// runs both were the same word. Every column below is a literal for that
    /// reason, and the table crosses all three models with every entry the
    /// registry can hold, because a row that only tried `Gpu` would pass a
    /// function that hard-coded it.
    #[test]
    fn the_device_is_where_the_session_opened_and_only_an_open_session_has_one() {
        let refused = crate::gpu::Placement::CpuAfterProviderFailed(provider_refusal("canary"));
        // none, Gpu, Cpu, CpuAfterProviderFailed — the order of the device columns.
        let placements: [(&str, Option<&crate::gpu::Placement>); 4] = [
            ("nothing recorded", None),
            ("Gpu", Some(&crate::gpu::Placement::Gpu)),
            ("Cpu", Some(&crate::gpu::Placement::Cpu)),
            ("CpuAfterProviderFailed", Some(&refused)),
        ];
        let devices: [(&str, [Option<&str>; 4]); 7] = [
            ("loaded", [None, Some("gpu"), Some("cpu"), Some("cpu")]),
            ("fallback", [Some("cpu"); 4]),
            ("failed", [None; 4]),
            ("warming", [None; 4]),
            ("configured", [None; 4]),
            ("off", [None; 4]),
            ("absent", [None; 4]),
        ];
        let models: [(Which, &[&str]); 3] = [
            (
                Which::Embedder,
                &["failed", "warming", "loaded", "fallback"],
            ),
            (
                Which::Reranker,
                &["failed", "off", "absent", "configured", "loaded"],
            ),
            (
                Which::Nli,
                &["failed", "off", "absent", "configured", "loaded"],
            ),
        ];

        for (which, states) in models {
            for &state in states {
                let (_, expected) = devices
                    .iter()
                    .find(|(name, _)| *name == state)
                    .unwrap_or_else(|| panic!("the table has no column for {state:?}"));
                let unrecorded = row(which, state, None);
                assert_eq!(
                    unrecorded.state, state,
                    "the fixture is broken: {which:?} does not land on {state:?} with the facts \
                     this row gives it, so every device assertion below would be about another row"
                );

                for (index, (recorded, placement)) in placements.iter().enumerate() {
                    let got = row(which, state, *placement);
                    assert_eq!(got.state, state, "{which:?} with {recorded}");
                    assert_eq!(
                        got.device, expected[index],
                        "{which:?} in `{state}` with {recorded} in the registry. `loaded` says \
                         where the session opened and `null` when nobody measured it; `fallback` \
                         is the hash on this CPU; `failed`, `warming`, `configured`, `off` and \
                         `absent` have no session and so no device, even when the registry \
                         already holds a value (it is written while the model is still loading)"
                    );
                    if state == "loaded" {
                        assert_eq!(
                            got.reason.is_some(),
                            index == 3,
                            "{which:?} loaded with {recorded}: only a session that opened on the \
                             CPU after the provider refused has something to explain, and a \
                             healthy one says nothing: {:?}",
                            got.reason
                        );
                    } else {
                        assert_eq!(
                            got.reason, unrecorded.reason,
                            "{which:?} in `{state}`: the registry must not change what a model \
                             that is not open says about itself. A `failed` model keeps the \
                             loader's reason, redacted with the variable that names *its* \
                             directory, and does not borrow the provider's"
                        );
                    }
                }
            }
        }
    }

    /// The caught case: the card, the runtime and the provider library are on
    /// disk, `cublasLt64_12.dll` is not, and the session opened on the CPU.
    ///
    /// The cause is the sentence ONNX Runtime wrote, and it is full of paths:
    /// the source file it was built from, and the provider DLL under the
    /// operator's home. They go the way every other path goes, but the variable
    /// that replaces them is the *runtime's*, not the model's — the directory
    /// in that error is where `onnxruntime.dll` and its providers live, and
    /// sending the operator to `CUBA_RERANKER_PATH` would be the wrong knob.
    #[test]
    fn a_session_that_opened_on_the_cpu_after_the_provider_refused_says_why_without_its_path() {
        assert_provider_refusal_redacted("canaryoperator", &["canaryoperator"]);
        // The same sentence with a user name that has spaces in it, which is what
        // `C:\Users\<name>` is made of for anyone whose Windows account carries
        // their full name. The canary above is one word, so a redaction that cut
        // the sentence at its spaces never had a word between two separators to
        // let through.
        assert_provider_refusal_redacted(
            "Canaryuno Canarydos Canarytres",
            &["Canaryuno", "Canarydos", "Canarytres"],
        );
    }

    /// One run of the caught case above for one operator name: `canaries` are the
    /// words of `user`, and none of them may be served.
    fn assert_provider_refusal_redacted(user: &str, canaries: &[&str]) {
        const RUNTIME_DIR: &str = "the ONNX Runtime directory (see ORT_DYLIB_PATH)";
        let cause = provider_refusal(user);

        // Positive controls. Without them «the canary is not in the reason» also
        // passes on a reason that never carried the cause.
        let fixed_needles = [
            ".cache",
            "onnxruntime_providers_cuda.dll",
            "cublasLt64_12.dll",
        ];
        for needle in canaries.iter().chain(fixed_needles.iter()) {
            assert!(
                cause.contains(needle),
                "the fixture is broken: the sentence this test scrubs has no {needle:?} in it"
            );
        }

        let refused = crate::gpu::Placement::CpuAfterProviderFailed(cause);
        let opened = [
            (
                "embedder",
                embedder_state_from(None, Some(&refused), true, true),
            ),
            (
                "reranker",
                reranker_state_from(None, Some(&refused), true, true, true),
            ),
            (
                "nli",
                nli_state_from(None, Some(&refused), false, true, true),
            ),
        ];

        // The first path is cut at the `:` of `.cc:1962`: an unquoted path ends
        // where its clause does, and what follows is the line number in ONNX
        // Runtime's own source, from the machine Microsoft built it on.
        let expected = format!(
            "the GPU provider did not start; this session runs on the CPU: {RUNTIME_DIR}:1962 \
             onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading \
             {RUNTIME_DIR} which depends on \"cublasLt64_12.dll\" which is missing. (Error 126: \
             \"No se puede encontrar el módulo especificado.\")"
        );

        // Beside the words of the operator's name: what is inventory about the
        // machine whatever the name is, and the variables of the other models.
        let fixed_leaks = [
            ".cache",
            "onnxruntime_providers_cuda.dll",
            "C:\\Users",
            "provider_bridge_ort.cc",
            "CUBA_RERANKER_PATH",
            "CUBA_NLI_PATH",
            "ONNX_MODEL_PATH",
        ];

        for (model, state) in &opened {
            assert_eq!(state.state, "loaded", "{model}");
            assert_eq!(
                state.device,
                Some("cpu"),
                "{model}: the session opened, and opened on the CPU. The registry says so even \
                 though the placement path asked for the card"
            );
            let reason = state.reason.as_deref().unwrap_or_else(|| {
                panic!(
                    "{model}: a session that fell to the CPU after the provider refused has to \
                     say why — `device: cpu` alone is indistinguishable from a machine that \
                     asked for it"
                )
            });

            assert_eq!(
                reason, expected,
                "{model}: the sentence is fixed — what happened, where it landed, then ONNX \
                 Runtime's own words with each path replaced by the runtime directory"
            );
            for kept in [
                "CPU",
                "(see ORT_DYLIB_PATH)",
                "cublasLt64_12.dll",
                "Error 126",
            ] {
                assert!(
                    reason.contains(kept),
                    "{model}: {kept:?} is the half an operator can act on, and it was cut: \
                     {reason}"
                );
            }
            for leaked in canaries.iter().chain(fixed_leaks.iter()) {
                assert!(
                    !reason.contains(leaked),
                    "{model}: {leaked:?} reached the reason. The paths in this error are the \
                     runtime's and `/health` goes out over a socket a LAN bind makes reachable; \
                     and a model-directory variable would send the operator to change the wrong \
                     one: {reason}"
                );
            }
        }

        let mut runtime = report(true, false, ["loaded", "loaded", "loaded"]);
        let [(_, embedder), (_, reranker), (_, nli)] = opened;
        runtime.embedder = embedder;
        runtime.reranker = reranker;
        runtime.nli = nli;
        let served = runtime_json(&runtime).to_string();
        assert!(
            served.contains("cublasLt64_12.dll"),
            "positive control: the cause has to reach the served block at all, or every canary \
             below passes on a block that says nothing: {served}"
        );
        let fixed_served = [".cache", "onnxruntime_providers_cuda.dll"];
        for canary in canaries.iter().chain(fixed_served.iter()) {
            assert!(
                !served.contains(canary),
                "{canary:?} was served in the runtime block: {served}"
            );
        }
    }

    /// Three rows, three answers: a reranker that fell to the CPU, an embedder
    /// that opened where it was asked, and an NLI nobody has used yet.
    #[test]
    fn each_model_row_tells_its_own_truth_in_the_served_block() {
        const CANARY_USER: &str = "canaryoperator";
        let refused = crate::gpu::Placement::CpuAfterProviderFailed(provider_refusal(CANARY_USER));

        let mut runtime = report(true, false, ["loaded", "loaded", "configured"]);
        runtime.reranker = reranker_state_from(None, Some(&refused), true, true, true);
        runtime.embedder = embedder_state_from(None, Some(&crate::gpu::Placement::Cpu), true, true);
        runtime.nli = nli_state_from(None, None, false, true, false);
        let json = runtime_json(&runtime);

        assert_eq!(json["reranker"]["state"], "loaded");
        assert_eq!(
            json["reranker"]["device"], "cpu",
            "the one that fell: {json}"
        );
        assert!(
            json["reranker"]["reason"]
                .as_str()
                .is_some_and(|why| why.contains("cublasLt64_12.dll")),
            "and it says why, in its own row: {json}"
        );

        assert_eq!(json["embedder"]["state"], "loaded");
        assert_eq!(json["embedder"]["device"], "cpu");
        assert_eq!(
            json["embedder"]["reason"],
            Value::Null,
            "an embedder that opened on the CPU because it was asked to has nothing to explain, \
             and must not inherit the reranker's cause: {json}"
        );

        assert_eq!(json["nli"]["state"], "configured");
        assert!(
            json["nli"]
                .as_object()
                .is_some_and(|block| block.contains_key("device")),
            "the key stays: {json}"
        );
        assert_eq!(
            json["nli"]["device"],
            Value::Null,
            "a model nobody has opened runs on no device, and saying `cpu` or `gpu` for it is \
             the guess this block used to make: {json}"
        );

        let served = json.to_string();
        for canary in [CANARY_USER, ".cache"] {
            assert!(
                !served.contains(canary),
                "{canary:?} was served in the runtime block: {served}"
            );
        }
    }

    /// What every row of the two tables below puts where a path was. It is the
    /// runtime's, which is what the cause of a provider refusal is built from.
    const TABLE_R: &str = "the ONNX Runtime directory (see ORT_DYLIB_PATH)";

    /// The rows of a table written as a line with the id, then `IN :` and
    /// `OUT:`, in the shape of the W6 specification. `<R>` in an output stands
    /// for `TABLE_R` and `<NL>` for a line break, which a table that is one raw
    /// string cannot carry any other way: a `\n` in it would be a backslash and
    /// an `n`, and the Windows paths in these rows have plenty of those.
    ///
    /// The tables are a string and not an array of tuples so that no formatter
    /// ever reflows a row: the exact text is the whole test.
    fn table_rows(table: &str) -> Vec<(&str, String, String)> {
        let mut rows = Vec::new();
        let (mut id, mut input) = ("", String::new());
        for line in table.lines() {
            if let Some(text) = line.strip_prefix("IN : ") {
                input = text.replace("<NL>", "\n");
            } else if let Some(text) = line.strip_prefix("OUT: ") {
                let output = text.replace("<NL>", "\n").replace("<R>", TABLE_R);
                rows.push((id, input.clone(), output));
            } else if !line.trim().is_empty() {
                id = line.trim();
            }
        }
        rows
    }

    /// A: the paths and the sentences that are redacted differently since W6.
    /// U: sentences with no path in them, which must not change by a character.
    /// K: what was already redacted well and must come out as it did.
    ///
    /// The ids are the specification's (§2.1 of the W6 handoff).
    const REDACTION_TABLE: &str = r#"
A1 ORT on Windows, the literal of 2026-10-12, a user name without spaces
IN : E:\_work\1\s\onnxruntime\core\session\provider_bridge_ort.cc:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading "C:\Users\canaryoperator\.cache\cuba-memorys\onnxruntime\onnxruntime_providers_cuda.dll" which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")
OUT: <R>:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading <R> which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")
A2 the same, user "Juan Carlos Pérez"
IN : E:\_work\1\s\onnxruntime\core\session\provider_bridge_ort.cc:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading "C:\Users\Juan Carlos Pérez\.cache\cuba-memorys\onnxruntime\onnxruntime_providers_cuda.dll" which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")
OUT: <R>:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading <R> which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")
A3 the same, under Program Files
IN : E:\_work\1\s\onnxruntime\core\session\provider_bridge_ort.cc:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading "C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6\bin\onnxruntime_providers_cuda.dll" which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")
OUT: <R>:1962 onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error loading <R> which depends on "cublasLt64_12.dll" which is missing. (Error 126: "No se puede encontrar el módulo especificado.")
A4 Display without quotes, onnx.rs, Windows
IN : loading ONNX Runtime from C:\Users\Juan Carlos Pérez\.cache\memory-industry\onnxruntime\onnxruntime.dll: LoadLibraryExW failed
OUT: loading ONNX Runtime from <R>: LoadLibraryExW failed
A5 Windows with forward slashes
IN : loading ONNX Runtime from C:/Users/Juan Carlos Pérez/.cache/memory-industry/onnxruntime/onnxruntime.dll: LoadLibraryExW failed
OUT: loading ONNX Runtime from <R>: LoadLibraryExW failed
A6 Program Files without quotes
IN : loading ONNX Runtime from C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6\bin\onnxruntime.dll: LoadLibraryExW failed
OUT: loading ONNX Runtime from <R>: LoadLibraryExW failed
A7 Linux, libloading plus dlerror, two paths with spaces
IN : loading ONNX Runtime from /home/juan carlos/.cache/memory-industry/onnxruntime/libonnxruntime.so: /home/juan carlos/.cache/memory-industry/onnxruntime/libonnxruntime.so: invalid ELF header
OUT: loading ONNX Runtime from <R>: <R>: invalid ELF header
A8 Linux, ORT, provider
IN : Failed to load library /home/juan carlos/.cache/memory-industry/onnxruntime/libonnxruntime_providers_cuda.so with error: libcublasLt.so.12: cannot open shared object file: No such file or directory
OUT: Failed to load library <R>: libcublasLt.so.12: cannot open shared object file: No such file or directory
A9 Linux, cannot open the file itself
IN : loading ONNX Runtime from /opt/memory industry/lib/libonnxruntime.so: /opt/memory industry/lib/libonnxruntime.so: cannot open shared object file: No such file or directory
OUT: loading ONNX Runtime from <R>: <R>: cannot open shared object file: No such file or directory
A10 UNC
IN : loading ONNX Runtime from \\servidor\recurso\Juan Carlos\onnxruntime.dll: LoadLibraryExW failed
OUT: loading ONNX Runtime from <R>: LoadLibraryExW failed
A11 verbatim \\?\
IN : resolving the ONNX Runtime path \\?\C:\Users\Juan Carlos\ort\onnxruntime.dll: El sistema no puede encontrar la ruta especificada. (os error 3)
OUT: resolving the ONNX Runtime path <R>: El sistema no puede encontrar la ruta especificada. (os error 3)
A12a ~ with a slash
IN : tokenizer.json not found at ~/Juan Carlos/models/tokenizer.json
OUT: tokenizer.json not found at <R>
A12b ~ with a backslash
IN : tokenizer.json not found at ~\Juan Carlos\models\tokenizer.json
OUT: tokenizer.json not found at <R>
A13 single quotes
IN : cannot open 'C:\Users\Juan Carlos\.cache\memory-industry\models\model.onnx': Acceso denegado. (os error 5)
OUT: cannot open <R>: Acceso denegado. (os error 5)
A14 backticks, ort lib.rs:107
IN : failed to load from `C:\Users\Juan Carlos\.cache\memory-industry\onnxruntime\onnxruntime.dll`: LoadLibraryExW failed
OUT: failed to load from <R>: LoadLibraryExW failed
A15 Debug with spaces, nli.rs
IN : no hay model.onnx ni model_quantized.onnx en "C:\\Users\\Juan Carlos Pérez\\.cache\\models-nli"
OUT: no hay model.onnx ni model_quantized.onnx en <R>
A16 an apostrophe in the name, no quotes
IN : loading ONNX Runtime from C:\Users\O'Brien Smith\ort\onnxruntime.dll: LoadLibraryExW failed
OUT: loading ONNX Runtime from <R>: LoadLibraryExW failed
A17 an apostrophe inside single quotes
IN : cannot open 'C:\Users\O'Brien\x.dll': denied
OUT: cannot open <R>: denied
A18 the sentence of its own after the path is lost
IN : the ONNX Runtime at C:\Users\Juan Carlos\ort\onnxruntime.dll is version 1.16.3, and this build needs 1.22 or newer: run `memory-industry models runtime` or point ORT_DYLIB_PATH at a newer library
OUT: the ONNX Runtime at <R>: run `memory-industry models runtime` or point ORT_DYLIB_PATH at a newer library
A19 ORT, Load model from ... failed: with no space after the colon
IN : load model: Load model from C:\Users\Juan Carlos\.cache\memory-industry\models\model.onnx failed:Protobuf parsing failed.
OUT: load model: Load model from <R>:Protobuf parsing failed.
A20 a line break ends the clause
IN : loading ONNX Runtime from /home/juan carlos/x/libonnxruntime.so<NL>second line
OUT: loading ONNX Runtime from <R> second line
A21 two paths in one clause
IN : copy C:\a b\x.dll to D:\c d\y.dll: failed
OUT: copy <R>: failed
A22 the apostrophe of a word opens nothing
IN : it doesn't load 'C:\x\y.dll' here
OUT: it doesn't load <R> here
A23 an unquoted path followed by a quoted name: the double quote ends the clause
IN : Error loading C:\Users\Juan Carlos\.cache\x\onnxruntime_providers_cuda.dll which depends on "cublasLt64_12.dll" which is missing.
OUT: Error loading <R> "cublasLt64_12.dll" which is missing.
A24 the old form of ORT, with an empty ""
IN : LoadLibrary failed with error 126 "" when trying to load "C:\Users\Juan Carlos\.cache\memory-industry\onnxruntime\onnxruntime_providers_cuda.dll"
OUT: LoadLibrary failed with error 126 "" when trying to load <R>
U1
IN : the session did not open
OUT: the session did not open
U2
IN : no model.onnx / model_quantized.onnx found
OUT: no model.onnx / model_quantized.onnx found
U3
IN : onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error 126: "No se puede encontrar el módulo especificado."
OUT: onnxruntime::ProviderLibrary::Get [ONNXRuntimeError] : 1 : FAIL : Error 126: "No se puede encontrar el módulo especificado."
U4
IN : which depends on "cublasLt64_12.dll" which is missing.
OUT: which depends on "cublasLt64_12.dll" which is missing.
U5
IN : run `memory-industry models runtime` or point ORT_DYLIB_PATH at a newer library
OUT: run `memory-industry models runtime` or point ORT_DYLIB_PATH at a newer library
U6
IN : it doesn't open: it's 3:00
OUT: it doesn't open: it's 3:00
U7 quoted text needs a separator and an alphanumeric to be a path
IN : see "/" here
OUT: see "/" here
K1 Debug without spaces, rerank.rs
IN : no model.onnx / model_quantized.onnx found in "C:\\Users\\canaryoperator\\.cache\\memory-industry\\reranker"
OUT: no model.onnx / model_quantized.onnx found in <R>
K2 Debug without spaces, nli.rs
IN : no hay model.onnx ni model_quantized.onnx en "C:\\Users\\canaryoperator\\.cache\\models-nli"
OUT: no hay model.onnx ni model_quantized.onnx en <R>
K3 Display at the end, onnx.rs
IN : tokenizer.json not found at C:\Users\canaryoperator\.cache\models\tokenizer.json
OUT: tokenizer.json not found at <R>
K4 a relative path with no anchor: the word layer
IN : load model: models/e5/model.onnx is truncated
OUT: load model: <R> is truncated
K5 a URL: the word layer
IN : download from https://huggingface.co/intfloat/e5 failed
OUT: download from <R> failed
"#;

    /// The redaction of one string as a table, so a row that moves is a row that
    /// names itself. Today every A row differs from what the function did before
    /// W6 except A22; the U and K rows are the guards that it did not start
    /// swallowing what it used to leave alone.
    #[test]
    fn a_path_is_redacted_whole_whatever_its_spaces_quotes_or_root() {
        let rows = table_rows(REDACTION_TABLE);
        assert_eq!(
            rows.len(),
            37,
            "the table lost a row: A1-A24 (A12 has two), U1-U7 and K1-K5. A table that quietly \
             shrinks passes with whatever is left"
        );

        for (id, input, expected) in rows {
            assert_eq!(
                reason_without_a_model_path(&input, TABLE_R),
                expected,
                "{id}: the redaction of {input:?}"
            );
        }
    }

    /// The templates of the sentences ONNX Runtime, `libloading` and this crate
    /// write around a path, and what each one must come out as. `{p}` is the
    /// path, and `{p:?}` the path as `nli.rs` writes it, with the quotes and the
    /// doubled backslashes of `Debug`.
    const TEMPLATE_TABLE: &str = r#"
P-T1
IN : Error loading "{p}" which depends on "cublasLt64_12.dll" which is missing.
OUT: Error loading <R> which depends on "cublasLt64_12.dll" which is missing.
P-T2
IN : cannot open '{p}': denied
OUT: cannot open <R>: denied
P-T3
IN : failed to load from `{p}`: LoadLibraryExW failed
OUT: failed to load from <R>: LoadLibraryExW failed
P-T4
IN : loading ONNX Runtime from {p}: LoadLibraryExW failed
OUT: loading ONNX Runtime from <R>: LoadLibraryExW failed
P-T5
IN : tokenizer.json not found at {p}
OUT: tokenizer.json not found at <R>
P-T6
IN : load model: Load model from {p} failed:Protobuf parsing failed.
OUT: load model: Load model from <R>:Protobuf parsing failed.
P-T7
IN : {p}: cannot open shared object file: No such file or directory
OUT: <R>: cannot open shared object file: No such file or directory
P-T8
IN : no hay model.onnx ni model_quantized.onnx en {p:?}
OUT: no hay model.onnx ni model_quantized.onnx en <R>
"#;

    /// The output may not depend on the path. Eight sentences by nine paths, and
    /// each of the 72 has to come out as the sentence does, with nothing of the
    /// path in it.
    ///
    /// Every component of every path carries `zq`, so «nothing of the path» is
    /// one check. The last path is the one a rule of «up to the last word with a
    /// separator» would let through: its final component has the space in it.
    /// It is a product and not `proptest` on purpose: the crate does not use it,
    /// and the part that varies is a handful of shapes, not a space of values.
    #[test]
    fn the_redacted_reason_does_not_depend_on_the_path() {
        let paths = [
            r"C:\Users\Zqa Zqb Zqc\.cache\zqd\onnxruntime_providers_cuda.dll",
            r"C:\Program Files\Zqa Zqb Zqc\Zqd Zqe\zq.dll",
            r"C:/Users/Zqa Zqb/zqc.dll",
            r"\\Zqserver\Zqa Zqb\zq.dll",
            r"\\?\C:\Users\Zqa Zqb\zq.dll",
            "/home/zqa zqb zqc/.cache/zqd/libzq.so",
            "~/Zqa Zqb/zq.so",
            r"C:\Users\O'Zqa Zqb\zq.dll",
            r"C:\Users\Zqa Zqb",
        ];
        let templates = table_rows(TEMPLATE_TABLE);
        assert_eq!(
            templates.len(),
            8,
            "the template table lost a row, and with it a third of what this test crosses"
        );

        for (name, template, expected) in &templates {
            for path in paths {
                let input = template
                    .replace("{p:?}", &format!("{path:?}"))
                    .replace("{p}", path);
                // Positive control. Without it «nothing of the path is left» also
                // passes on a template that never carried the path.
                assert!(
                    input.to_lowercase().contains("zq"),
                    "the fixture is broken: {name} with {path} has no path in it: {input:?}"
                );

                let got = reason_without_a_model_path(&input, TABLE_R);
                assert_eq!(
                    &got, expected,
                    "{name} with {path}: the output is the sentence's and not the path's"
                );
                assert!(
                    !got.to_lowercase().contains("zq"),
                    "{name} with {path}: a piece of the path was served: {got:?}"
                );
                assert_eq!(
                    reason_without_a_model_path(&got, TABLE_R),
                    got,
                    "{name} with {path}: redacting what was already redacted changed it"
                );
            }
        }
    }

    /// If a replacement had a separator in it, the word layer would take it for
    /// a path the next time the sentence went through, and the redaction would
    /// stop being idempotent.
    #[test]
    fn no_replacement_names_a_path_itself() {
        for constant in [
            RERANKER_DIR_INSTEAD_OF_A_PATH,
            NLI_DIR_INSTEAD_OF_A_PATH,
            EMBEDDER_DIR_INSTEAD_OF_A_PATH,
            RUNTIME_DIR_INSTEAD_OF_A_PATH,
        ] {
            assert!(
                constant.contains("(see "),
                "the fixture is broken: {constant:?} is not the sentence that names a variable"
            );
            assert!(
                !constant.contains(['/', '\\']),
                "{constant:?} has a path separator in it. The word layer of the redaction would \
                 swallow it the next time the sentence went through, and it is the variable an \
                 operator reads to know which knob to change"
            );
        }
    }

    /// A quote opens after a non-alphanumeric, and that character can be a
    /// separator: it is the sentence's, not what the quote holds. Here the
    /// quoted text is `a b`, which is not a path, so the quotes stay and only
    /// the word that the separator is in goes (layer 3), and `b'` is left. A
    /// verdict taken over the character before the quote as well would swallow
    /// the whole span and with it the `b`.
    #[test]
    fn what_a_quote_holds_is_judged_without_the_character_before_it() {
        for (input, expected) in [
            ("from/'a b' here", format!("{TABLE_R} b' here")),
            (r#"from\"a b" here"#, format!(r#"{TABLE_R} b" here"#)),
        ] {
            assert_eq!(
                reason_without_a_model_path(input, TABLE_R),
                expected,
                "{input:?}"
            );
        }
    }

    /// The text that starts or ends on a character each rule looks at: nothing,
    /// a quote with no partner, a lone separator, half a drive, a drive and no
    /// more, and a quote that never closes in front of a path. Every one is
    /// also an input where a step that stopped moving would sit on the same
    /// position until the end of the loop, so the output would be the wrong one
    /// and not just a slow one.
    #[test]
    fn the_edges_of_the_text_come_out_as_they_went_in_or_as_one_replacement() {
        let cases = [
            ("", String::new()),
            ("\"", "\"".to_string()),
            ("\"\"", "\"\"".to_string()),
            ("say \"", "say \"".to_string()),
            ("/", "/".to_string()),
            ("/ x", "/ x".to_string()),
            ("C:", "C:".to_string()),
            (r"C:\", TABLE_R.to_string()),
            (r"'C:\a b\x.dll", format!("'{TABLE_R}")),
        ];
        for (input, expected) in cases {
            assert_eq!(
                reason_without_a_model_path(input, TABLE_R),
                expected,
                "{input:?}"
            );
        }
    }

    /// What `onnx.rs` writes when the runtime will not load, on Windows: the
    /// path with no quotes around it, ending in the `:` before the cause.
    fn failed_load(user: &str) -> String {
        format!(
            r"loading ONNX Runtime from C:\Users\{user}\.cache\memory-industry\onnxruntime\onnxruntime.dll: LoadLibraryExW failed"
        )
    }

    /// `state` placed in the row of `which` in a report, as `/health` serves it.
    fn served_with(which: Which, state: ModelState) -> String {
        let mut runtime = report(true, false, ["loaded", "loaded", "loaded"]);
        match which {
            Which::Embedder => runtime.embedder = state,
            Which::Reranker => runtime.reranker = state,
            Which::Nli => runtime.nli = state,
        }
        runtime_json(&runtime).to_string()
    }

    /// The row of `which` with a failure, or, with none, with the session opened
    /// where the registry holds `placement`. The facts that put each model on
    /// `loaded` are the ones `row` uses.
    fn site(
        which: Which,
        failure: Option<String>,
        placement: Option<&crate::gpu::Placement>,
    ) -> ModelState {
        match which {
            Which::Embedder => embedder_state_from(failure, placement, true, true),
            Which::Reranker => reranker_state_from(failure, placement, true, true, true),
            Which::Nli => nli_state_from(failure, placement, false, true, true),
        }
    }

    /// Each of the six places that serve a reason calls the redaction on its
    /// own: three `failed` arms and the one `loaded_on` that the three models
    /// share, reached through each of them. A copy of the old redaction in any
    /// one of them leaves that model's row leaking while the others pass.
    #[test]
    fn every_site_that_serves_a_reason_redacts_paths_with_spaces() {
        let failed_reason = failed_load("Zqa Zqb Zqc");
        let refused =
            crate::gpu::Placement::CpuAfterProviderFailed(provider_refusal("Zqa Zqb Zqc"));

        for which in [Which::Embedder, Which::Reranker, Which::Nli] {
            let model_variable = match which {
                Which::Embedder => "ONNX_MODEL_PATH",
                Which::Reranker => "CUBA_RERANKER_PATH",
                Which::Nli => "CUBA_NLI_PATH",
            };
            let failed = site(which, Some(failed_reason.clone()), None);
            let loaded = site(which, None, Some(&refused));
            assert_eq!(failed.state, "failed", "{which:?}: the fixture is broken");
            assert_eq!(loaded.state, "loaded", "{which:?}: the fixture is broken");

            // The variable differs by site: a failed model names its own
            // directory, and a provider refusal names the runtime's.
            for (served, variable, control) in [
                (
                    served_with(which, failed),
                    model_variable,
                    "LoadLibraryExW failed",
                ),
                (
                    served_with(which, loaded),
                    "ORT_DYLIB_PATH",
                    "cublasLt64_12.dll",
                ),
            ] {
                // Positive controls. Without them «none of the words are in the
                // block» also passes on a block that never carried the reason.
                assert!(
                    served.contains(control),
                    "{which:?}: {control:?} has to reach the block, or every check below is \
                     about a block that says nothing: {served}"
                );
                assert!(
                    served.contains(variable),
                    "{which:?}: the replacement names {variable}: {served}"
                );
                for word in ["Zqa", "Zqb", "Zqc"] {
                    assert!(
                        !served.contains(word),
                        "{which:?}: {word:?} is a word of the operator's name, and it was \
                         served in a path with spaces in it: {served}"
                    );
                }
            }
        }
    }
}
