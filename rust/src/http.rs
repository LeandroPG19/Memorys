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
         cuba-memorys.socket, or set CUBA_HTTP_TOKEN in the service unit"
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
/// `reason` is a sentence for a human and is `None` when the state says
/// everything. It never carries a filesystem path: a model path is inventory,
/// and on Windows it has the operator's user name inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelState {
    pub state: &'static str,
    pub device: &'static str,
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
    /// `gpu::status()` — asked for a device and not using one.
    pub gpu_degraded: bool,
    /// `embedder=… reranker=… nli=…`, as the placement path decided it.
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

/// Where the placement path put this workload.
fn device_of(workload: crate::gpu::Workload) -> &'static str {
    if crate::gpu::wants_gpu(workload) {
        "gpu"
    } else {
        "cpu"
    }
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
/// the thing that makes a monitor's poll hang.
fn embedder_state(ready: bool) -> ModelState {
    let device = device_of(crate::gpu::Workload::Embedder);
    if !ready {
        return ModelState {
            state: STATE_WARMING,
            device,
            reason: None,
        };
    }
    if crate::embeddings::onnx::is_model_loaded() {
        return ModelState {
            state: STATE_LOADED,
            device,
            reason: None,
        };
    }
    ModelState {
        state: STATE_FALLBACK,
        device: "cpu",
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
fn reranker_state() -> ModelState {
    let device = device_of(crate::gpu::Workload::Reranker);
    if let Some(reason) = crate::search::rerank::failure_reason() {
        return ModelState {
            state: STATE_FAILED,
            device,
            reason: Some(reason),
        };
    }
    if crate::search::rerank::resolved_model_dir().is_none() {
        return ModelState {
            state: STATE_OFF,
            device,
            reason: Some(
                "the resource plan switched the reranker off on this machine; rankings come \
                 back in RRF order"
                    .to_string(),
            ),
        };
    }
    if !crate::search::rerank::is_configured() {
        return ModelState {
            state: STATE_ABSENT,
            device,
            reason: Some(
                "no reranker model where this daemon looks — `memory-industry models reranker` \
                 installs it"
                    .to_string(),
            ),
        };
    }
    if crate::search::rerank::status_resolved() {
        return ModelState {
            state: STATE_LOADED,
            device,
            reason: None,
        };
    }
    ModelState {
        state: STATE_CONFIGURED,
        device,
        reason: Some("loads on its first batch".to_string()),
    }
}

/// The NLI model, from the two questions that can be answered for free.
///
/// It can never report `loaded` or `failed` from here: `nli::enabled()` is the
/// only thing that knows, and it is also what loads the model. Saying
/// `configured` when it may in fact have failed is the lesser lie — the
/// alternative is a `/health` that loads a gigabyte the first time a monitor
/// polls it, which is the bug `doctor` was just fixed for.
fn nli_state() -> ModelState {
    let device = device_of(crate::gpu::Workload::Nli);
    if crate::cognitive::nli::deferred_by_resource_plan() {
        return ModelState {
            state: STATE_OFF,
            device,
            reason: Some(
                "the resource plan switched NLI off on this machine; contradictions fall back \
                 to the judge"
                    .to_string(),
            ),
        };
    }
    if !crate::cognitive::nli::available() {
        return ModelState {
            state: STATE_ABSENT,
            device,
            reason: Some("no NLI model where this daemon looks".to_string()),
        };
    }
    ModelState {
        state: STATE_CONFIGURED,
        device,
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
