use crate::db;
use crate::handlers;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub fn handler_timeout() -> Duration {
    std::env::var("CUBA_HANDLER_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(30))
}

const REM_INTERVAL: Duration = Duration::from_secs(4 * 3600);
const REM_FIRST_DELAY_DEFAULT_SECS: u64 = 300;

pub fn rem_first_delay() -> Duration {
    std::env::var("CUBA_REM_FIRST_DELAY_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(REM_FIRST_DELAY_DEFAULT_SECS))
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

static CLIENT_SUPPORTS_SAMPLING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

static HANDSHAKE_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn handshake_timeout() -> Option<Duration> {
    match std::env::var("CUBA_HANDSHAKE_TIMEOUT_SECS") {
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("off") => None,
        Ok(v) => v.parse::<u64>().ok().map(Duration::from_secs),
        Err(_) => Some(Duration::from_secs(60)),
    }
}

fn spawn_handshake_watchdog() {
    let Some(limit) = handshake_timeout() else {
        return;
    };
    tokio::spawn(async move {
        tokio::time::sleep(limit).await;
        if HANDSHAKE_SEEN.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        tracing::error!(
            secs = limit.as_secs(),
            "no MCP handshake — the client gave up before the models finished loading. \
             Exiting instead of holding them for nobody (set CUBA_HANDSHAKE_TIMEOUT_SECS=0 \
             to disable, or run `cuba-memorys serve` so the models load once and stay warm)"
        );
        std::process::exit(1);
    });
}

pub fn client_supports_sampling() -> bool {
    CLIENT_SUPPORTS_SAMPLING.load(std::sync::atomic::Ordering::Relaxed)
}

use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use tokio::sync::{Mutex, mpsc, oneshot};

static OUTBOUND: OnceLock<mpsc::UnboundedSender<Value>> = OnceLock::new();

static PENDING: OnceLock<Mutex<std::collections::HashMap<u64, oneshot::Sender<Value>>>> =
    OnceLock::new();

static NEXT_SERVER_ID: AtomicU64 = AtomicU64::new(1);

static CANCEL_TOKENS: OnceLock<std::sync::Mutex<std::collections::HashMap<String, CancelToken>>> =
    OnceLock::new();

#[derive(Clone, Default)]
pub struct CancelToken {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl CancelToken {
    pub fn cancelled(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn cancel(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub struct CancelRegistration {
    key: String,
    token: CancelToken,
}

impl CancelRegistration {
    pub fn token(&self) -> CancelToken {
        self.token.clone()
    }
}

impl Drop for CancelRegistration {
    fn drop(&mut self) {
        cancel_tokens().remove(&self.key);
    }
}

fn outbound() -> Option<&'static mpsc::UnboundedSender<Value>> {
    OUTBOUND.get()
}

fn send_outbound(msg: Value) {
    match outbound() {
        Some(tx) => {
            let _ = tx.send(msg);
        }
        None => tracing::debug!("no stdio channel — dropping server-initiated message"),
    }
}

fn pending() -> &'static Mutex<std::collections::HashMap<u64, oneshot::Sender<Value>>> {
    PENDING.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn cancel_tokens() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, CancelToken>>
{
    CANCEL_TOKENS
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub async fn request_sampling(prompt: &str) -> anyhow::Result<String> {
    request_sampling_max(prompt, 256).await
}

pub async fn request_sampling_max(prompt: &str, max_tokens: u32) -> anyhow::Result<String> {
    if !client_supports_sampling() {
        anyhow::bail!(
            "client does not advertise capabilities.sampling — \
             set CUBA_JUDGE=claude_cli or rely on auto fallback"
        );
    }

    let channel = outbound().ok_or_else(|| {
        anyhow::anyhow!(
            "no server->client channel: sampling needs the stdio transport. \
             Under the HTTP daemon the judge falls back to the local NLI model"
        )
    })?;

    let id = NEXT_SERVER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = oneshot::channel::<Value>();
    pending().lock().await.insert(id, tx);

    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": format!("srv_{id}"),
        "method": "sampling/createMessage",
        "params": {
            "messages": [
                {
                    "role": "user",
                    "content": { "type": "text", "text": prompt }
                }
            ],
            "maxTokens": max_tokens,
            "modelPreferences": {
                "intelligencePriority": 0.6,
                "speedPriority": 0.4
            }
        }
    });
    if let Err(e) = channel.send(req) {
        pending().lock().await.remove(&id);
        return Err(anyhow::anyhow!("outbound channel closed: {e}"));
    }

    let response = match tokio::time::timeout(handler_timeout(), rx).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => anyhow::bail!("sampling response channel dropped"),
        Err(_) => {
            pending().lock().await.remove(&id);
            anyhow::bail!("sampling timed out after 30s");
        }
    };

    let text = response
        .get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .or_else(|| {
            response
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
        })
        .ok_or_else(|| anyhow::anyhow!("malformed sampling response: {response}"))?;
    Ok(text.to_string())
}

pub fn notify_progress(token: &str, progress: f64, total: Option<f64>, message: Option<&str>) {
    let mut params = serde_json::json!({
        "progressToken": token,
        "progress": progress,
    });
    if let Some(t) = total {
        params["total"] = serde_json::json!(t);
    }
    if let Some(m) = message {
        params["message"] = serde_json::json!(m);
    }
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": params,
    });
    if let Some(tx) = OUTBOUND.get() {
        let _ = tx.send(notif);
    }
}

pub fn register_cancel_token(request_id: &Value) -> CancelRegistration {
    let token = CancelToken::default();
    let key = request_id.to_string();
    cancel_tokens().insert(key.clone(), token.clone());
    CancelRegistration { key, token }
}

const SUPPORTED_PROTOCOL_VERSIONS: [&str; 5] = [
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

const FALLBACK_PROTOCOL_VERSION: &str = "2024-11-05";

fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    let Some(requested) = requested else {
        return FALLBACK_PROTOCOL_VERSION;
    };
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|supported| *supported == requested)
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
}

fn server_info(params: Option<&Value>) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    serde_json::json!({
        "protocolVersion": negotiate_protocol_version(requested),
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "listChanged": false, "subscribe": false }
        },
        "serverInfo": {
            "name": "MemoryIndustry",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

pub async fn run_mcp() -> Result<()> {
    let database_url = crate::setup::resolve_database_url().await;

    let (pool, connected) = match db::create_pool(&database_url).await {
        Ok(pool) => {
            db::assert_embedding_dim(&pool).await?;
            (pool, true)
        }
        Err(why) => {
            tracing::warn!(
                error = %format!("{why:#}"),
                "starting without PostgreSQL — tools will fail until it is reachable"
            );
            (db::create_lazy_pool(&database_url), false)
        }
    };

    let rem_handle = connected.then(|| {
        let rem_pool = pool.clone();
        tokio::spawn(async move {
            rem_daemon(rem_pool).await;
        })
    });

    let listen_handle = connected.then(|| {
        let listen_pool = pool.clone();
        let listen_url = database_url.clone();
        tokio::spawn(async move {
            sync_listener(listen_pool, listen_url).await;
        })
    });

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    OUTBOUND
        .set(out_tx)
        .map_err(|_| anyhow::anyhow!("OUTBOUND already initialized"))?;

    let writer_handle = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(msg) = out_rx.recv().await {
            let mut bytes = match serde_json::to_vec(&msg) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(error = %e, "failed to serialize outbound");
                    continue;
                }
            };
            bytes.push(b'\n');
            if let Err(e) = stdout.write_all(&bytes).await {
                tracing::error!(error = %e, "stdout write failed — terminating writer");
                break;
            }
            if let Err(e) = stdout.flush().await {
                tracing::error!(error = %e, "stdout flush failed");
                break;
            }
        }
    });

    let stdin = tokio::io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();
    let mut in_flight: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    if crate::search::rerank::is_configured() {
        tokio::spawn(async {
            let started = tokio::time::Instant::now();
            if crate::search::rerank::warm_up().await {
                tracing::info!(
                    secs = started.elapsed().as_secs_f32(),
                    "reranker warm — the first search no longer pays for the load"
                );
            } else {
                tracing::warn!("reranker configured but failed to warm up — identity fallback");
            }
        });
    }

    spawn_handshake_watchdog();

    tracing::info!("MCP protocol ready on stdin/stdout (V0.9.2 correlator)");

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "invalid JSON-RPC");
                send_outbound(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": "Parse error", "data": e.to_string() }
                }));
                continue;
            }
        };

        let has_method = parsed.get("method").and_then(|v| v.as_str()).is_some();
        let id_value = parsed.get("id").cloned();

        if !has_method
            && let Some(idv) = id_value.as_ref()
            && let Some(srv_id) = idv
                .as_str()
                .and_then(|s| s.strip_prefix("srv_"))
                .and_then(|s| s.parse::<u64>().ok())
        {
            if let Some(tx) = pending().lock().await.remove(&srv_id) {
                let _ = tx.send(parsed);
            } else {
                tracing::warn!(srv_id, "stale sampling response — no pending entry");
            }
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_value(parsed) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "invalid JSON-RPC envelope");
                send_outbound(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id_value.unwrap_or(Value::Null),
                    "error": { "code": -32600, "message": "Invalid request", "data": e.to_string() }
                }));
                continue;
            }
        };

        let is_notification = request.id.is_none();
        let req_id = request.id.clone().unwrap_or(Value::Null);
        let pool_clone = pool.clone();

        in_flight.spawn(async move {
            let response = handle_request(&pool_clone, request).await;
            if is_notification {
                if let Err(e) = &response {
                    tracing::warn!(error = %e, "notification handler error (suppressed)");
                }
                return;
            }
            let envelope = match response {
                Ok(v) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": v,
                }),
                Err(e) => {
                    let chain = format!("{e:#}");
                    tracing::error!(error = %chain, "handler failed");
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": { "code": -32603, "message": chain }
                    })
                }
            };
            send_outbound(envelope);
        });
    }

    tracing::info!(
        "stdin closed — draining {} in-flight handlers",
        in_flight.len()
    );
    let drain = async { while in_flight.join_next().await.is_some() {} };
    let _ = tokio::time::timeout(handler_timeout() * 2, drain).await;

    if let Some(tx) = OUTBOUND.get() {
        let _ = tx;
    }

    if let Some(handle) = rem_handle {
        handle.abort();
    }
    if let Some(handle) = listen_handle {
        handle.abort();
    }
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), writer_handle).await;
    tracing::info!("REM daemon + writer drained, shutting down");

    Ok(())
}

pub(crate) async fn handle_request(pool: &PgPool, request: JsonRpcRequest) -> Result<Value> {
    match request.method.as_str() {
        "initialize" => {
            HANDSHAKE_SEEN.store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(params) = &request.params {
                let sampling_advertised = params
                    .get("capabilities")
                    .and_then(|c| c.get("sampling"))
                    .is_some();
                if crate::session::daemon_mode() {
                    if sampling_advertised {
                        tracing::debug!(
                            "client advertises sampling, but the HTTP daemon cannot \
                             call back — judge stays on the local NLI model"
                        );
                    }
                } else {
                    CLIENT_SUPPORTS_SAMPLING
                        .store(sampling_advertised, std::sync::atomic::Ordering::Relaxed);
                    if sampling_advertised {
                        tracing::info!("client supports MCP sampling — judge auto-prefers it");
                    }
                }
            }
            Ok(server_info(request.params.as_ref()))
        }
        "initialized" | "notifications/initialized" => Ok(Value::Null),
        "notifications/cancelled" => {
            if let Some(params) = &request.params
                && let Some(req_id) = params.get("requestId")
            {
                let key = req_id.to_string();
                if let Some(token) = cancel_tokens().get(&key).cloned() {
                    token.cancel();
                    tracing::info!(req_id = %key, "client requested cancellation");
                }
            }
            Ok(Value::Null)
        }
        "ping" => Ok(serde_json::json!({})),

        "tools/list" => Ok(serde_json::json!({
            "tools": crate::constants::tools_for_profile()
        })),

        "resources/list" => list_resources(pool).await,
        "resources/read" => {
            let params = request.params.unwrap_or(Value::Null);
            let uri = params.get("uri").and_then(|v| v.as_str()).unwrap_or("");
            read_resource(pool, uri).await
        }

        "tools/call" => {
            let req_id = request.id.clone().unwrap_or(Value::Null);
            let params = request.params.unwrap_or(Value::Null);
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));

            tracing::info!(tool = %tool_name, "executing tool");

            let registration = register_cancel_token(&req_id);
            let token = registration.token();
            let cancel_fut = async {
                while !token.cancelled() {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            };

            tokio::select! {
                result = tokio::time::timeout(
                    handler_timeout(),
                    handlers::dispatch(pool, &tool_name, arguments),
                ) => match result {
                    Ok(r) => r,
                    Err(_) => {
                        tracing::error!(tool = %tool_name, "handler timed out after 30s");
                        Err(anyhow::anyhow!("Handler timed out after 30 seconds"))
                    }
                },
                _ = cancel_fut => {
                    tracing::warn!(tool = %tool_name, req_id = %req_id, "handler cancelled by client");
                    Err(anyhow::anyhow!("Handler cancelled by client"))
                }
            }
        }

        _ => {
            tracing::warn!(method = %request.method, "unknown method");
            anyhow::bail!("Unknown method: {}", request.method)
        }
    }
}

pub const SYNC_CLOCK_CHANNEL: &str = "brain_sync_clock";

const ANNOUNCE_DEBOUNCE: Duration = Duration::from_millis(500);
const LISTEN_RETRY_MIN: Duration = Duration::from_secs(2);
const LISTEN_RETRY_MAX: Duration = Duration::from_secs(300);

pub(crate) async fn sync_listener(pool: PgPool, url: String) {
    let mut backoff = LISTEN_RETRY_MIN;
    loop {
        let deaf_since = std::time::Instant::now();
        match listen_once(&pool, &url).await {
            Ok(()) => backoff = LISTEN_RETRY_MIN,
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    deaf_secs = deaf_since.elapsed().as_secs(),
                    retry_in_secs = backoff.as_secs(),
                    "the sync listener lost its connection; NOTIFY is not durable, so whatever \
                     was written while it was deaf will only travel on the next fetch"
                );
            }
        }
        let jitter = Duration::from_millis(u64::from(std::process::id() % 500));
        tokio::time::sleep(backoff + jitter).await;
        backoff = (backoff * 2).min(LISTEN_RETRY_MAX);
    }
}

async fn listen_once(pool: &PgPool, url: &str) -> anyhow::Result<()> {
    let mut listener = sqlx::postgres::PgListener::connect(url)
        .await
        .context("opening the sync listener connection")?;
    listener
        .listen(SYNC_CLOCK_CHANNEL)
        .await
        .context("subscribing to the sync clock channel")?;
    tracing::info!(
        channel = SYNC_CLOCK_CHANNEL,
        "listening for local writes worth telling a peer about"
    );

    loop {
        let first = listener
            .recv()
            .await
            .context("the listener connection ended")?;
        let mut burst = 1u32;
        let deadline = tokio::time::Instant::now() + ANNOUNCE_DEBOUNCE;
        loop {
            match tokio::time::timeout_at(deadline, listener.recv()).await {
                Ok(Ok(_)) => burst += 1,
                Ok(Err(e)) => return Err(anyhow::anyhow!("listener ended mid-burst: {e}")),
                Err(_) => break,
            }
        }

        tracing::debug!(first = %first.payload(), writes = burst, "announcing to peers");
        match crate::handlers::sync::announce_to_peers(pool).await {
            Ok(0) => {}
            Ok(reached) => tracing::info!(peers = reached, writes = burst, "peers told"),
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "could not tell the peers"),
        }
    }
}

async fn run_rem_cycle(pool: &PgPool) {
    tracing::info!("REM sleep cycle starting");

    let pool_clone = pool.clone();
    let result = tokio::spawn(async move { run_rem_consolidation(&pool_clone).await }).await;

    match result {
        Ok(Ok(())) => tracing::info!("REM sleep cycle completed"),
        Ok(Err(e)) => tracing::error!(error = %e, "REM consolidation error"),
        Err(e) => tracing::error!(error = %e, "REM task panicked"),
    }
}

pub async fn rem_daemon(pool: PgPool) {
    tokio::time::sleep(rem_first_delay()).await;
    run_rem_cycle(&pool).await;

    let mut interval = tokio::time::interval(REM_INTERVAL);
    interval.tick().await;

    loop {
        interval.tick().await;
        run_rem_cycle(&pool).await;
    }
}

pub const REM_LOCK: i64 = 0x0CBA_A0D1_7106_0034;

pub async fn run_rem_consolidation(pool: &PgPool) -> Result<()> {
    let mut lock_conn = pool
        .acquire()
        .await
        .context("acquiring a connection to hold the REM advisory lock")?;

    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(REM_LOCK)
        .fetch_one(&mut *lock_conn)
        .await
        .context("checking the REM advisory lock")?;

    if !acquired {
        tracing::info!(
            "REM consolidation: another cycle already holds the lock, skipping this one"
        );
        return Ok(());
    }

    let cycle_started_at = chrono::Utc::now();
    let result = run_rem_consolidation_locked(pool).await;

    if let Err(e) = &result {
        record_rem_cycle_failure(pool, cycle_started_at, e).await;
    }

    if let Err(e) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(REM_LOCK)
        .execute(&mut *lock_conn)
        .await
    {
        tracing::error!(
            error = %e,
            "failed to release the REM advisory lock — it stays held until this connection closes"
        );
    }

    result
}

async fn run_rem_consolidation_locked(pool: &PgPool) -> Result<()> {
    let started_at = chrono::Utc::now();
    let candidates: Vec<uuid::Uuid> = match crate::session::session_id() {
        Some(sid) => vec![sid],
        None if crate::session::daemon_mode() => {
            let from_memory: Vec<uuid::Uuid> = crate::session::all_active()
                .into_iter()
                .map(|s| s.session_id)
                .collect();
            if from_memory.is_empty() {
                sqlx::query_scalar("SELECT id FROM brain_sessions WHERE ended_at IS NULL")
                    .fetch_all(pool)
                    .await?
            } else {
                from_memory
            }
        }
        None => vec![],
    };

    let active_session: Option<(uuid::Uuid, Vec<String>)> = if candidates.is_empty() {
        None
    } else {
        let row: Option<(uuid::Uuid, serde_json::Value)> = sqlx::query_as(
            "SELECT id, goals FROM brain_sessions
             WHERE id = ANY($1) AND ended_at IS NULL
             ORDER BY started_at DESC LIMIT 1",
        )
        .bind(&candidates)
        .fetch_optional(pool)
        .await?;

        row.map(|(id, goals)| {
            let goal_list: Vec<String> = serde_json::from_value(goals).unwrap_or_default();
            (id, goal_list)
        })
    };

    let protected_entity_ids: Vec<uuid::Uuid> = if let Some((_session_id, _)) = &active_session {
        sqlx::query_scalar(
            "SELECT DISTINCT entity_id FROM brain_observations
             WHERE created_at > NOW() - INTERVAL '8 hours'",
        )
        .fetch_all(pool)
        .await?
    } else {
        vec![]
    };

    let stratified_decay_sql = "UPDATE brain_observations SET
        importance = GREATEST(
            importance * EXP(-0.693
                * EXTRACT(EPOCH FROM (NOW() - GREATEST(last_accessed, last_decayed_at))) / 86400.0
                / ((CASE observation_type
                        WHEN 'fact'       THEN 30.0
                        WHEN 'preference' THEN 30.0
                        WHEN 'error'      THEN 14.0
                        WHEN 'solution'   THEN 14.0
                        WHEN 'context'    THEN  7.0
                        WHEN 'tool_usage' THEN  7.0
                        ELSE 30.0
                    END) * (1.0 + LN(1.0 + access_count::float8)))
            ),
            0.01
        ),
        last_decayed_at = NOW(),
        updated_at = NOW()
     WHERE observation_type NOT IN ('decision', 'lesson', 'superseded')
       AND last_accessed < NOW() - INTERVAL '1 day'";

    let decayed = if protected_entity_ids.is_empty() {
        sqlx::query(stratified_decay_sql)
            .execute(pool)
            .await?
            .rows_affected()
    } else {
        let sql_with_protection = format!(
            "{} AND entity_id NOT IN (SELECT UNNEST($1::uuid[]))",
            stratified_decay_sql
        );
        sqlx::query(&sql_with_protection)
            .bind(&protected_entity_ids)
            .execute(pool)
            .await?
            .rows_affected()
    };
    tracing::info!(
        decayed_count = decayed,
        "stratified exponential decay applied"
    );

    let episode_decayed = sqlx::query(
        "UPDATE brain_episodes SET
            importance = GREATEST(
                0.5 / POWER(1.0 + 0.1 * EXTRACT(EPOCH FROM (NOW() - created_at)) / 86400.0, 0.5),
                0.01
            )
         WHERE created_at < NOW() - INTERVAL '1 hour'",
    )
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    tracing::info!(
        episode_decayed_count = episode_decayed,
        "episode power-law decay applied"
    );

    let linked = rem_autolink(pool).await;
    tracing::info!(edges_created = linked, "NPMI autolink applied");

    let backfilled = rem_backfill_embeddings(pool).await;
    tracing::info!(
        embedded = backfilled.embedded,
        failed = backfilled.failed,
        "missing embeddings backfilled"
    );

    let chunked = rem_backfill_chunks(pool).await;
    tracing::info!(observations_chunked = chunked, "long observations chunked");

    let scan = rem_scan_relations(pool).await;
    tracing::info!(
        entities_scanned = scan.scanned,
        edges_created = scan.linked,
        failed = scan.failed,
        "isolated entities scanned for relations"
    );

    let extraction = rem_extract_observations(pool).await;
    tracing::info!(
        observations_scanned = extraction.scanned,
        facts_added = extraction.added,
        relations_linked = extraction.relations_linked,
        failed = extraction.failed,
        "auto-extraction over unprocessed observations"
    );

    let quantized = rem_backfill_halfvec(pool).await;
    if quantized > 0 {
        tracing::info!(rows = quantized, "embeddings quantized to halfvec");
    }

    let analyzed = rem_refresh_planner_stats(pool).await;
    tracing::info!(tables_analyzed = analyzed, "planner statistics refreshed");

    let ranked = crate::graph::pagerank::compute_and_store(pool).await?;
    tracing::info!(ranked_count = ranked, "PageRank updated");

    let communities_found = match crate::graph::community::detect_and_persist(pool).await {
        Ok((communities, nodes_updated)) => {
            tracing::info!(
                communities = communities.len(),
                nodes_updated,
                "community detection updated"
            );
            communities.len() as i64
        }
        Err(e) => {
            tracing::warn!(error = %e, "REM: community detection failed");
            0
        }
    };

    let duplicate_candidates = rem_count_duplicate_candidates(pool).await;
    tracing::info!(
        duplicate_candidates,
        "duplicate entity candidates awaiting `cuba-memorys dedupe`"
    );

    record_rem_cycle(
        pool,
        RemCycleRecord {
            started_at,
            decayed: decayed as i64,
            autolink_edges: linked as i64,
            embeddings_backfilled: backfilled.embedded as i64,
            entities_scanned: scan.scanned as i64,
            facts_extracted: extraction.added as i64,
            communities: communities_found,
            duplicate_candidates,
            relation_scan_failed: scan.failed as i64,
            extraction_failed: extraction.failed as i64,
            error: None,
        },
    )
    .await;

    tracing::info!("REM consolidation complete");

    Ok(())
}

struct RemCycleRecord {
    started_at: chrono::DateTime<chrono::Utc>,
    decayed: i64,
    autolink_edges: i64,
    embeddings_backfilled: i64,
    entities_scanned: i64,
    facts_extracted: i64,
    communities: i64,
    duplicate_candidates: i64,
    relation_scan_failed: i64,
    extraction_failed: i64,
    error: Option<String>,
}

async fn record_rem_cycle(pool: &PgPool, r: RemCycleRecord) {
    let finished_at = chrono::Utc::now();
    let duration_ms = (finished_at - r.started_at).num_milliseconds();
    let written = sqlx::query(
        "INSERT INTO brain_rem_cycles
            (started_at, finished_at, duration_ms, decayed_count, autolink_edges,
             embeddings_backfilled, entities_scanned, facts_extracted, communities,
             duplicate_candidates, relation_scan_failed, extraction_failed, error)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(r.started_at)
    .bind(finished_at)
    .bind(duration_ms)
    .bind(r.decayed)
    .bind(r.autolink_edges)
    .bind(r.embeddings_backfilled)
    .bind(r.entities_scanned)
    .bind(r.facts_extracted)
    .bind(r.communities)
    .bind(r.duplicate_candidates)
    .bind(r.relation_scan_failed)
    .bind(r.extraction_failed)
    .bind(r.error)
    .execute(pool)
    .await;
    if let Err(e) = written {
        tracing::warn!(error = %e, "could not record this REM cycle");
    }
}

async fn record_rem_cycle_failure(
    pool: &PgPool,
    started_at: chrono::DateTime<chrono::Utc>,
    error: &anyhow::Error,
) {
    record_rem_cycle(
        pool,
        RemCycleRecord {
            started_at,
            decayed: 0,
            autolink_edges: 0,
            embeddings_backfilled: 0,
            entities_scanned: 0,
            facts_extracted: 0,
            communities: 0,
            duplicate_candidates: 0,
            relation_scan_failed: 0,
            extraction_failed: 0,
            error: Some(format!("{error:#}")),
        },
    )
    .await;
}

const HALFVEC_BATCH: i64 = 500;

async fn rem_backfill_halfvec(pool: &PgPool) -> u64 {
    let has_column: Option<bool> = sqlx::query_scalar(
        "SELECT true FROM information_schema.columns
         WHERE table_name = 'brain_observations' AND column_name = 'embedding_half'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    if has_column.is_none() {
        return 0;
    }

    sqlx::query(
        "UPDATE brain_observations SET embedding_half = embedding::halfvec
         WHERE id IN (
             SELECT id FROM brain_observations
             WHERE embedding IS NOT NULL AND embedding_half IS NULL
             LIMIT $1
         )",
    )
    .bind(HALFVEC_BATCH)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or_else(|why| {
        tracing::warn!(error = %why, "halfvec backfill failed");
        0
    })
}

const PLANNER_STAT_TABLES: [&str; 4] = [
    "brain_observations",
    "brain_entities",
    "brain_relations",
    "brain_observation_chunks",
];

async fn rem_refresh_planner_stats(pool: &PgPool) -> usize {
    let mut analyzed = 0usize;
    for table in PLANNER_STAT_TABLES {
        let stale: Option<bool> = sqlx::query_scalar(
            "SELECT last_analyze IS NULL AND last_autoanalyze IS NULL
             FROM pg_stat_user_tables WHERE relname = $1",
        )
        .bind(table)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        let never_analyzed = stale.unwrap_or(false);
        let churned: bool = sqlx::query_scalar(
            "SELECT n_mod_since_analyze > GREATEST(50, n_live_tup * 0.05)
             FROM pg_stat_user_tables WHERE relname = $1",
        )
        .bind(table)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);

        if !never_analyzed && !churned {
            continue;
        }
        match sqlx::query(&format!("ANALYZE {table}")).execute(pool).await {
            Ok(_) => analyzed += 1,
            Err(why) => tracing::warn!(error = %why, table, "ANALYZE failed"),
        }
    }
    analyzed
}

const REM_RELATION_SCAN_DEFAULT_BATCH: usize = 5;
const REM_RELATION_SCAN_LARGE_BATCH: usize = 20;
const REM_RELATION_SCAN_QUEUE_THRESHOLD: i64 = 50;
const REM_RELATION_SCAN_MAX_FAILURES: usize = 2;

#[derive(Default)]
pub struct RelationScanReport {
    pub scanned: usize,
    pub linked: u32,
    pub failed: usize,
}

pub async fn rem_relation_scan_batch(pool: &PgPool) -> usize {
    if let Some(explicit) = std::env::var("CUBA_REM_RELATION_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        return explicit;
    }

    let queue_len = crate::handlers::ingesta::entities_awaiting_relation_scan(
        pool,
        REM_RELATION_SCAN_QUEUE_THRESHOLD,
    )
    .await
    .map(|ids| ids.len() as i64)
    .unwrap_or(0);

    if queue_len >= REM_RELATION_SCAN_QUEUE_THRESHOLD {
        REM_RELATION_SCAN_LARGE_BATCH
    } else {
        REM_RELATION_SCAN_DEFAULT_BATCH
    }
}

async fn rem_scan_relations(pool: &PgPool) -> RelationScanReport {
    let batch = rem_relation_scan_batch(pool).await;
    let mut report = RelationScanReport::default();
    if batch == 0 {
        return report;
    }

    let candidates =
        match crate::handlers::ingesta::entities_awaiting_relation_scan(pool, batch as i64).await {
            Ok(ids) => ids,
            Err(why) => {
                tracing::warn!(error = %why, "could not list entities for the relation scan");
                return report;
            }
        };

    let mut consecutive_failures = 0usize;
    for id in candidates {
        match crate::handlers::ingesta::scan_entity_relations(pool, id).await {
            Ok(linked) => {
                consecutive_failures = 0;
                report.scanned += 1;
                report.linked += linked;
            }
            Err(why) => {
                consecutive_failures += 1;
                report.failed += 1;
                crate::handlers::ingesta::mark_relation_scan_attempt_failed(pool, id).await;
                tracing::warn!(error = %why, entity = %id, "relation scan failed");
                if consecutive_failures >= REM_RELATION_SCAN_MAX_FAILURES {
                    tracing::warn!(
                        failures = consecutive_failures,
                        "giving up on this cycle's relation scan"
                    );
                    break;
                }
            }
        }
    }
    report
}

const REM_EXTRACTION_DEFAULT_BATCH: usize = 5;
const REM_EXTRACTION_MAX_FAILURES: usize = 2;

pub fn rem_extraction_batch() -> usize {
    std::env::var("CUBA_REM_EXTRACTION_BATCH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(REM_EXTRACTION_DEFAULT_BATCH)
}

#[derive(Default)]
pub struct ExtractionScanReport {
    pub scanned: usize,
    pub added: u32,
    pub relations_linked: u32,
    pub failed: usize,
}

async fn rem_extract_observations(pool: &PgPool) -> ExtractionScanReport {
    let batch = rem_extraction_batch();
    let mut report = ExtractionScanReport::default();
    if batch == 0 {
        return report;
    }

    let candidates = match crate::handlers::ingesta::observations_awaiting_extraction(
        pool,
        batch as i64,
    )
    .await
    {
        Ok(rows) => rows,
        Err(why) => {
            tracing::warn!(error = %why, "could not list observations for auto-extraction");
            return report;
        }
    };

    let mut consecutive_failures = 0usize;
    for (id, content) in candidates {
        match crate::handlers::ingesta::rem_extract_observation(pool, id, &content).await {
            Ok(outcome) => {
                consecutive_failures = 0;
                report.scanned += 1;
                report.added += outcome.added;
                report.relations_linked += outcome.relations_linked;
            }
            Err(why) => {
                consecutive_failures += 1;
                report.failed += 1;
                crate::handlers::ingesta::mark_extraction_attempt_failed(pool, id).await;
                tracing::warn!(error = %why, observation = %id, "REM auto-extraction failed");
                if consecutive_failures >= REM_EXTRACTION_MAX_FAILURES {
                    tracing::warn!(
                        failures = consecutive_failures,
                        "giving up on this cycle's auto-extraction"
                    );
                    break;
                }
            }
        }
    }
    report
}

const DUPLICATE_NAME_SIMILARITY_THRESHOLD: f64 = 0.70;

pub async fn rem_count_duplicate_candidates(pool: &PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM brain_entities a
         JOIN brain_entities b ON a.id < b.id
         WHERE similarity(lower(a.name), lower(b.name)) > $1",
    )
    .bind(DUPLICATE_NAME_SIMILARITY_THRESHOLD)
    .fetch_one(pool)
    .await
    .unwrap_or(0)
}

fn rem_autolink_enabled() -> bool {
    !matches!(
        std::env::var("CUBA_REM_AUTOLINK").as_deref(),
        Ok("0") | Ok("off") | Ok("false")
    )
}

async fn rem_autolink(pool: &PgPool) -> usize {
    if !rem_autolink_enabled() {
        return 0;
    }
    use crate::graph::autolink::{self, DEFAULT_NPMI_THRESHOLD, MIN_CO_SESSIONS};

    let candidates = match autolink::candidates(pool, MIN_CO_SESSIONS, DEFAULT_NPMI_THRESHOLD).await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "REM: autolink candidate search failed");
            return 0;
        }
    };
    if candidates.is_empty() {
        return 0;
    }
    match autolink::apply(pool, &candidates).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "REM: autolink apply failed");
            0
        }
    }
}

async fn rem_backfill_chunks(pool: &PgPool) -> usize {
    use crate::embeddings::backfill;

    let limit = backfill::backfill_limit();
    if limit == 0 {
        return 0;
    }
    match backfill::backfill_chunks(pool, limit).await {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "REM: chunk backfill failed");
            0
        }
    }
}

async fn rem_backfill_embeddings(pool: &PgPool) -> crate::embeddings::backfill::BackfillReport {
    use crate::embeddings::backfill;

    let limit = backfill::backfill_limit();
    if limit == 0 {
        return backfill::BackfillReport::default();
    }
    match backfill::backfill_missing(pool, limit).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "REM: embedding backfill failed");
            backfill::BackfillReport::default()
        }
    }
}

async fn list_resources(pool: &PgPool) -> Result<Value> {
    let mut resources: Vec<Value> = Vec::new();

    resources.push(serde_json::json!({
        "uri": "memory://context",
        "name": "agent context",
        "description": "Working memory + notes + artifacts + top recall (budgeted)",
        "mimeType": "application/json"
    }));
    resources.push(serde_json::json!({
        "uri": "memory://status",
        "name": "daemon status",
        "description": "Same payload as cuba_whoami / memory_whoami",
        "mimeType": "application/json"
    }));
    resources.push(serde_json::json!({
        "uri": "memory://artifacts",
        "name": "artifacts index",
        "description": "Shared multi-agent artifact paths",
        "mimeType": "application/json"
    }));

    let entities: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, entity_type FROM brain_entities
         ORDER BY access_count DESC NULLS LAST, updated_at DESC NULLS LAST
         LIMIT 50",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (name, etype) in &entities {
        resources.push(serde_json::json!({
            "uri": format!("cuba://entity/{name}"),
            "name": name,
            "description": format!("{etype} entity with observations and relations"),
            "mimeType": "application/json"
        }));
    }

    let projects: Vec<(String,)> =
        sqlx::query_as("SELECT name FROM brain_projects ORDER BY last_active_at DESC LIMIT 100")
            .fetch_all(pool)
            .await
            .unwrap_or_default();
    for (name,) in &projects {
        resources.push(serde_json::json!({
            "uri": format!("cuba://project/{name}"),
            "name": format!("project: {name}"),
            "description": "project metadata + per-table counts",
            "mimeType": "application/json"
        }));
    }

    let scope = crate::project::current_project_id(pool)
        .await
        .ok()
        .flatten();
    let snapshots: Vec<(uuid::Uuid, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, created_at FROM brain_compaction_snapshots
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(scope)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (id, ts) in &snapshots {
        resources.push(serde_json::json!({
            "uri": format!("cuba://snapshot/{id}"),
            "name": format!("snapshot {}", &id.to_string()[..8]),
            "description": format!("compaction snapshot from {}", ts.to_rfc3339()),
            "mimeType": "text/markdown"
        }));
    }

    Ok(serde_json::json!({"resources": resources}))
}

async fn read_resource(pool: &PgPool, uri: &str) -> Result<Value> {
    if uri == "memory://context" || uri == "cuba://context" {
        let body = crate::handlers::contexto::handle(pool, serde_json::json!({})).await?;
        return Ok(serde_json::json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&body)?
            }]
        }));
    }
    if uri == "memory://status" || uri == "cuba://status" {
        let body = crate::handlers::whoami::handle(pool, serde_json::json!({})).await?;
        return Ok(serde_json::json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&body)?
            }]
        }));
    }
    if uri == "memory://artifacts" || uri == "cuba://artifacts" {
        let body = crate::handlers::artefacto::handle(
            pool,
            serde_json::json!({"action": "list", "limit": 50}),
        )
        .await?;
        return Ok(serde_json::json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&body)?
            }]
        }));
    }

    let stripped = uri
        .strip_prefix("cuba://")
        .or_else(|| uri.strip_prefix("memory://"))
        .ok_or_else(|| anyhow::anyhow!("URI must start with cuba:// or memory://"))?;

    if let Some(name) = stripped.strip_prefix("entity/") {
        let row: Option<(String, String, f64, i32)> = sqlx::query_as(
            "SELECT name, entity_type, importance::float8, access_count
             FROM brain_entities WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(pool)
        .await?;
        let entity = row.ok_or_else(|| anyhow::anyhow!("entity not found: {name}"))?;
        let observations: Vec<(uuid::Uuid, String, String, f64)> = sqlx::query_as(
            "SELECT o.id, o.content, o.observation_type, o.importance::float8
             FROM brain_observations o
             JOIN brain_entities e ON o.entity_id = e.id
             WHERE e.name = $1 AND o.observation_type != 'superseded'
             ORDER BY o.importance DESC, o.created_at DESC LIMIT 20",
        )
        .bind(name)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        let body = serde_json::json!({
            "name": entity.0,
            "entity_type": entity.1,
            "importance": entity.2,
            "access_count": entity.3,
            "observations": observations.iter().map(|(id, c, t, i)| serde_json::json!({
                "id": id.to_string(), "content": c, "type": t, "importance": i
            })).collect::<Vec<_>>(),
        });
        return Ok(serde_json::json!({
            "contents": [{"uri": uri, "mimeType": "application/json", "text": body.to_string()}]
        }));
    }

    if let Some(name) = stripped.strip_prefix("project/") {
        let pid: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM brain_projects WHERE name = $1")
                .bind(name)
                .fetch_optional(pool)
                .await?;
        let pid = pid
            .map(|(id,)| id)
            .ok_or_else(|| anyhow::anyhow!("project not found: {name}"))?;
        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT
                (SELECT COUNT(*) FROM brain_entities WHERE project_id = $1),
                (SELECT COUNT(*) FROM brain_observations WHERE project_id = $1),
                (SELECT COUNT(*) FROM brain_episodes WHERE project_id = $1),
                (SELECT COUNT(*) FROM brain_relations WHERE project_id = $1)",
        )
        .bind(pid)
        .fetch_one(pool)
        .await?;
        let body = serde_json::json!({
            "name": name,
            "id": pid.to_string(),
            "entities": counts.0,
            "observations": counts.1,
            "episodes": counts.2,
            "relations": counts.3,
        });
        return Ok(serde_json::json!({
            "contents": [{"uri": uri, "mimeType": "application/json", "text": body.to_string()}]
        }));
    }

    if let Some(id_str) = stripped.strip_prefix("snapshot/") {
        let id: uuid::Uuid = id_str
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid snapshot UUID"))?;
        let scope = crate::project::current_project_id(pool)
            .await
            .ok()
            .flatten();
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT summary_md FROM brain_compaction_snapshots
             WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2 OR project_id IS NULL)",
        )
        .bind(id)
        .bind(scope)
        .fetch_optional(pool)
        .await?;
        let md = row
            .map(|(m,)| m)
            .ok_or_else(|| anyhow::anyhow!("snapshot not found: {id}"))?;
        return Ok(serde_json::json!({
            "contents": [{"uri": uri, "mimeType": "text/markdown", "text": md}]
        }));
    }

    anyhow::bail!("Unknown cuba:// URI scheme: {uri}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_we_speak_is_echoed_back_unchanged() {
        for supported in SUPPORTED_PROTOCOL_VERSIONS {
            assert_eq!(
                negotiate_protocol_version(Some(supported)),
                supported,
                "the spec says answer with the client's version when we support it; \
                 answering something else makes a conforming client disconnect"
            );
        }
    }

    #[test]
    fn a_version_we_do_not_know_gets_our_newest_not_our_oldest() {
        assert_eq!(
            negotiate_protocol_version(Some("2027-01-01")),
            SUPPORTED_PROTOCOL_VERSIONS[0],
            "a client from the future is told the newest we speak, so it can decide; \
             replying 2024-11-05 to it was the bug"
        );
        assert_eq!(
            negotiate_protocol_version(Some("nonsense")),
            SUPPORTED_PROTOCOL_VERSIONS[0]
        );
    }

    #[test]
    fn a_client_that_states_no_version_gets_the_floor() {
        assert_eq!(
            negotiate_protocol_version(None),
            FALLBACK_PROTOCOL_VERSION,
            "protocolVersion is required, so a missing one means a client that predates \
             the field or is broken: the oldest version is the only safe answer"
        );
    }

    #[test]
    fn the_supported_list_is_ordered_newest_first_and_has_no_duplicates() {
        let mut sorted = SUPPORTED_PROTOCOL_VERSIONS;
        sorted.sort_unstable();
        sorted.reverse();
        assert_eq!(
            sorted, SUPPORTED_PROTOCOL_VERSIONS,
            "negotiation falls back to index 0, so the list being newest-first is what \
             makes that fallback the newest instead of an arbitrary entry"
        );

        let mut unique = SUPPORTED_PROTOCOL_VERSIONS.to_vec();
        unique.dedup();
        assert_eq!(unique.len(), SUPPORTED_PROTOCOL_VERSIONS.len());
    }

    fn pool_that_never_connects() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(5))
            .connect_lazy("postgres://cancel-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    fn tools_call(id: &str, tool: &str, arguments: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::String(id.to_string())),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({ "name": tool, "arguments": arguments })),
        }
    }

    #[tokio::test]
    async fn a_tools_call_dropped_mid_flight_leaves_no_cancel_token_behind() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let key = Value::String("drop-probe".to_string()).to_string();
        let pool = pool_that_never_connects();

        let in_flight = tokio::spawn(async move {
            handle_request(
                &pool,
                tools_call(
                    "drop-probe",
                    "cuba_pizarra",
                    serde_json::json!({ "action": "read" }),
                ),
            )
            .await
        });

        let mut registered = false;
        for _ in 0..200 {
            if cancel_tokens().contains_key(&key) {
                registered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            registered,
            "the call never reached the select!, so this test would prove nothing about \
             what happens when its future is dropped"
        );
        assert!(
            !in_flight.is_finished(),
            "the handler was supposed to still be blocked acquiring a connection that will \
             never come; if it returned early the drop below happens after the normal \
             cleanup and the leak stays invisible"
        );

        in_flight.abort();
        assert!(
            in_flight.await.is_err_and(|e| e.is_cancelled()),
            "aborting must drop the handler future, which is what axum does to the /mcp \
             handler when the client hangs up"
        );

        assert!(
            !cancel_tokens().contains_key(&key),
            "a request abandoned by the client leaked one entry in CANCEL_TOKENS for the \
             whole life of the daemon: request ids are a monotonic counter, so nothing ever \
             reuses the key and the map only grows"
        );
    }

    #[tokio::test]
    async fn a_tools_call_that_returns_normally_leaves_no_cancel_token_behind() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let key = Value::String("finish-probe".to_string()).to_string();
        let pool = pool_that_never_connects();

        let outcome = handle_request(
            &pool,
            tools_call("finish-probe", "no_such_tool", Value::Null),
        )
        .await;

        assert!(
            outcome.is_err(),
            "an unknown tool must fail without touching the pool, which is what keeps this \
             test free of a database"
        );
        assert!(
            !cancel_tokens().contains_key(&key),
            "the happy path must clean up too: moving the cleanup into Drop is worthless if \
             it stops running when the handler simply returns"
        );
    }

    #[tokio::test]
    async fn a_registered_call_is_cancellable_through_the_map_until_it_is_dropped() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let req_id = Value::String("cancel-probe".to_string());
        let key = req_id.to_string();

        let registration = register_cancel_token(&req_id);
        let published = cancel_tokens()
            .get(&key)
            .cloned()
            .expect("registering must publish the token before the handler starts");
        published.cancel();

        assert!(
            registration.token().cancelled(),
            "notifications/cancelled flips the flag it finds in the map; if that were a copy \
             instead of a shared handle, cancelling would silently do nothing"
        );

        drop(registration);
        assert!(
            !cancel_tokens().contains_key(&key),
            "cleanup must be tied to the registration's lifetime, not to a call the caller \
             may never reach"
        );
    }

    #[test]
    fn initialize_answers_with_the_version_the_client_asked_for() {
        let params = serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"}
        });

        let info = server_info(Some(&params));

        assert_eq!(info["protocolVersion"], "2025-06-18");
        assert_eq!(info["serverInfo"]["name"], "MemoryIndustry");
        assert!(info["capabilities"]["tools"].is_object());
    }

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");
        db::create_pool(&url)
            .await
            .expect("connect to test database")
    }

    #[tokio::test]
    #[ignore]
    async fn a_cycle_that_dies_mid_flight_leaves_a_row_carrying_its_own_error() {
        let pool = test_pool().await;
        let started_at = chrono::Utc::now() + chrono::Duration::minutes(12);
        let boom = anyhow::anyhow!("pagerank::compute_and_store: connection reset");

        record_rem_cycle_failure(&pool, started_at, &boom).await;

        let row: (Option<String>, i64, i64) = sqlx::query_as(
            "SELECT error, facts_extracted, extraction_failed FROM brain_rem_cycles
             WHERE started_at = $1",
        )
        .bind(started_at)
        .fetch_one(&pool)
        .await
        .expect(
            "a cycle that fails must still leave a row in brain_rem_cycles — the panel's \
             'last cycle' otherwise keeps showing an earlier, unrelated success as if it \
             were today's run",
        );

        sqlx::query("DELETE FROM brain_rem_cycles WHERE started_at = $1")
            .bind(started_at)
            .execute(&pool)
            .await
            .ok();

        assert_eq!(
            row.0.as_deref(),
            Some("pagerank::compute_and_store: connection reset"),
            "the error column exists since migration 0059 but the INSERT never named it, so \
             it landed NULL no matter what record_rem_cycle was told — a crashed cycle read \
             back exactly like a clean one with nothing extracted"
        );
        assert_eq!(row.1, 0, "a cycle that never finished extracted nothing");
        assert_eq!(
            row.2, 0,
            "a cycle that never reached extraction failed nothing there"
        );
    }
}
