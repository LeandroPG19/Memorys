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

fn ensure_loopback(addr: &SocketAddr) -> Result<()> {
    if addr.ip().is_loopback() || auth_token().is_some() {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to bind {addr}: the daemon serves the whole brain with no auth. \
         Use a 127.0.0.1 address, or set CUBA_HTTP_TOKEN to require a bearer token"
    )
}

fn ensure_adopted_loopback(addr: &SocketAddr) -> Result<()> {
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

    let state = AppState {
        pool,
        token: auth_token().map(Arc::new),
        peer_token: peer_token().map(Arc::new),
        started: Instant::now(),
        served: Arc::new(AtomicU64::new(0)),
        seen: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        last_activity: Arc::new(Mutex::new(Instant::now())),
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
        .route("/events", get(events_sse))
        .route("/events/ticket", post(events_ticket));
    if panel_enabled() {
        tracing::info!("control panel at http://{addr}/panel");
        app = app.route("/panel", get(panel));
    }
    tracing::info!("connect page at http://{addr}/connect");
    let app = app.layer(DefaultBodyLimit::max(MAX_BODY)).with_state(state);

    let listener = match systemd_listener() {
        Some(std_listener) => {
            let adopted = std_listener
                .local_addr()
                .context("cannot read the address of the systemd-activated socket")?;
            ensure_adopted_loopback(&adopted)?;
            tokio::net::TcpListener::from_std(std_listener)
                .context("failed to adopt systemd-activated socket")?
        }
        None => tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("cannot bind {addr} — is another daemon already running?"))?,
    };

    tracing::info!(
        %addr,
        auth = auth_token().is_some(),
        "MemoryIndustry daemon listening — point clients at http://{addr}/mcp (connect: /connect)"
    );

    tokio::spawn(async {
        let started = Instant::now();
        warm_models().await;
        tracing::info!(secs = started.elapsed().as_secs_f32(), "models warm");
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(idle_shutdown))
        .await
        .context("http server failed")?;

    tracing::info!("daemon shut down");
    Ok(())
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

fn warm_reranker_eagerly() -> bool {
    matches!(
        std::env::var("CUBA_WARM_RERANKER").as_deref(),
        Ok("1") | Ok("on") | Ok("true") | Ok("yes")
    )
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

async fn panel(headers: HeaderMap) -> Response {
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

async fn mcp_endpoint(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(scope) = authorized(&state, &headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(error_envelope(Value::Null, -32001, "invalid bearer token")),
        )
            .into_response();
    };

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

    let (key, declared) = client_key(&headers, &payload);
    if let Ok(mut guard) = state.seen.write() {
        guard.insert(key.clone(), Instant::now());
    }
    if let Ok(mut guard) = state.last_activity.lock() {
        *guard = Instant::now();
    }

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
            adopt_client_root(&state.pool, &key, uri).await;
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
            if let Some(reply) = dispatch_one(&state, &key, declared, scope, item).await {
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

async fn dispatch_one(
    state: &AppState,
    key: &str,
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
        let rooted =
            crate::session::with_root_project(crate::session::client_root_project_for(key), served);
        if declared {
            crate::session::with_client(key.to_string(), rooted).await
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
            tracing::error!(client = %key, method = %method, detail = %detail, "handler panicked");
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
            tracing::error!(client = %key, method = %method, error = %chain, "handler failed");
            error_envelope(req_id, -32603, chain)
        }
    })
}

async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
        .is_ok();

    let mut body = serde_json::json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started.elapsed().as_secs(),
        "requests_served": state.served.load(Ordering::Relaxed),
        "database": if db_ok { "up" } else { "unreachable" },
        "graph_db": crate::graph_db::status_summary(),
        "connect": "/connect",
        "events": "/events",
    });

    if authorized(&state, &headers) == Some(Scope::Full) {
        let clients: Vec<String> = state
            .seen
            .read()
            .map(|g| g.keys().cloned().collect())
            .unwrap_or_default();
        body["clients"] = serde_json::json!(clients);
    } else {
        body["clients_count"] = serde_json::json!(state.seen.read().map(|g| g.len()).unwrap_or(0));
    }

    let code = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(name: &'static str, value: &str) -> HeaderMap {
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
        let response =
            mcp_endpoint(State(state_with_clients(None, &[])), HeaderMap::new(), body).await;
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
        let response =
            mcp_endpoint(State(state_with_clients(None, &[])), HeaderMap::new(), body).await;
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

    fn state_with_clients(token: Option<&str>, clients: &[&str]) -> AppState {
        let seen = std::collections::HashMap::from_iter(
            clients.iter().map(|c| ((*c).to_string(), Instant::now())),
        );
        AppState {
            pool: crate::db::create_lazy_pool("postgres://unused/unused"),
            token: token.map(|t| Arc::new(t.to_string())),
            peer_token: None,
            started: Instant::now(),
            served: Arc::new(AtomicU64::new(0)),
            seen: Arc::new(std::sync::RwLock::new(seen)),
            last_activity: Arc::new(Mutex::new(Instant::now())),
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
