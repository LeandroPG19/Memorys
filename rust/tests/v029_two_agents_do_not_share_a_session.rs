use std::time::{Duration, Instant};

use serde_json::json;
use uuid::Uuid;

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ADMIN: &str = "identity-admin-token";

/// The daemon's own ceiling on loading models before it opens for business.
///
/// Pinned here rather than left at its 180 s default because the wait below is
/// exactly a wait for that opening, and nothing in this file asks the daemon
/// anything a model could answer: it asks who owns a session. The models keep
/// loading past it — `serve_pool` detaches the warm-up and serves with
/// `ready:false` — and no assertion here reads that flag. Left at the default,
/// a file that needs no model still waits for one.
const WARM_CEILING: Duration = Duration::from_secs(10);

/// How long this test waits for the daemon to open the port.
///
/// `serve_pool` binds the port, *then* warms the models, and only then serves.
/// A connection that arrives in between is accepted by the kernel and parked in
/// the backlog, so "the port took my connection" does not mean "something is
/// answering". A fixed sleep cannot tell those two apart: the clients below have
/// no timeout, so the first request simply waited in the backlog until the
/// daemon got round to it, and the old wait could only ever make this file slow,
/// never red.
const OPEN_BUDGET: Duration = Duration::from_secs(30);

// The invariant, as a build failure rather than as a sentence somebody can read
// past: the daemon is guaranteed to open within its warm ceiling, so a budget
// below that ceiling fails a daemon which did exactly what it was told. The
// headroom pays for the bind, the runtime and the probe interval. Lowering the
// budget to make the suite feel faster is the obvious next edit, and it is the
// one that would quietly turn a working daemon into a red test — so it does not
// compile.
const _: () = assert!(
    OPEN_BUDGET.as_secs() > WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the daemon is failed for opening on time"
);

/// Per probe, so a request parked in the backlog is cut and retried instead of
/// swallowing the whole budget in one silent wait. `/health` asks the database
/// within its own five seconds, and that database is up here.
const PROBE_BUDGET: Duration = Duration::from_secs(5);

/// Between probes: short enough to add nothing visible to a fast start, long
/// enough not to spin against a socket nobody is serving yet.
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// One `GET /health`: `Ok` when something served the port, `Err` with why
/// nothing did.
///
/// Whatever came back settles it, status code and body included: the question
/// this probe asks is only whether the port is *served*. What it serves is what
/// the test below is for.
async fn probe(client: &reqwest::Client, url: &str) -> Result<(), String> {
    client
        .get(url)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| format!("no answer at all: {e}"))
}

/// Block until the daemon on `port` serves, or fail saying it never did.
///
/// Polling rather than sleeping a fixed number: any number is right on an idle
/// machine and wrong on the busy one that actually failed, which is the same
/// race wearing a calmer name.
///
/// The condition is that `/health` *answers*, not that it answers `ready:true`.
/// Waiting for the socket is not enough — the port is bound before anything
/// serves it — but `ready` is more than this file needs, and it is not a signal
/// guaranteed to arrive: a warm-up that overruns
/// `MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS` leaves a daemon that answers
/// everything and reports `ready:false` for as long as it takes, so a test
/// waiting on it would fail a daemon that was working.
async fn wait_until_it_serves(port: u16, daemon: &tokio::task::JoinHandle<()>) {
    let url = format!("http://127.0.0.1:{port}/health");
    let client = reqwest::Client::builder()
        .timeout(PROBE_BUDGET)
        .build()
        .expect("a probe client with a bounded attempt");
    let deadline = Instant::now() + OPEN_BUDGET;
    let mut probes = 0u32;
    let mut last = String::from(
        "it never answered: the port was bound, so the connection was queued rather than \
         refused, and nothing ever served it",
    );

    while Instant::now() < deadline {
        assert!(
            !daemon.is_finished(),
            "the daemon on 127.0.0.1:{port} ended before it served anything, so nothing below \
             can run. serve_pool printed why it stopped — with --nocapture that line is just \
             above this one; the port still held by an earlier run is the usual reason"
        );

        probes += 1;
        match probe(&client, &url).await {
            Ok(()) => return,
            Err(why) => last = why,
        }
        tokio::time::sleep(PROBE_EVERY).await;
    }

    panic!(
        "the daemon at {url} never answered within {}s, over {probes} probe(s). Last: {last}. \
         serve_pool binds the port before it loads its models, so the kernel queues the \
         connection instead of refusing it, and an accepted connection says nothing about \
         whether anything is serving. This daemon is pinned to open within {}s of warm-up, so \
         overrunning the budget means it never opened at all",
        OPEN_BUDGET.as_secs(),
        WARM_CEILING.as_secs()
    );
}

async fn daemon(port: u16) -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    unsafe {
        std::env::set_var("CUBA_HTTP_TOKEN", ADMIN);
        std::env::remove_var("CUBA_PEER_TOKEN");
        std::env::remove_var("CUBA_PANEL");
        std::env::set_var(
            "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
            WARM_CEILING.as_secs().to_string(),
        );
    }
    let served = pool.clone();
    let addr = format!("127.0.0.1:{port}");
    let serve_task = tokio::spawn(async move {
        // Printed rather than dropped: when it is the bind that failed, the
        // wait below only ever sees a connection nobody answers, and the
        // sentence naming the cause lives in this Err and nowhere else.
        if let Err(why) = memory_industry::http::serve_pool(&addr, served, true).await {
            eprintln!("the daemon on 127.0.0.1:{port} stopped: {why:#}");
        }
    });
    wait_until_it_serves(port, &serve_task).await;
    pool
}

async fn open_a_session(port: u16, header: Option<&str>, name: &str) -> serde_json::Value {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .bearer_auth(ADMIN);
    if let Some(id) = header {
        request = request.header("mcp-client-id", id);
    }
    let body = request
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "cuba_jornada",
                "arguments": {"action": "start", "name": name},
                "clientInfo": {"name": "claude-code"}
            }
        }))
        .send()
        .await
        .expect("the daemon answers")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no envelope: {body}"));
    serde_json::from_str(text).expect("json")
}

async fn whose_session(port: u16, header: Option<&str>) -> Option<String> {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .bearer_auth(ADMIN);
    if let Some(id) = header {
        request = request.header("mcp-client-id", id);
    }
    let body = request
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "cuba_jornada",
                "arguments": {"action": "current"},
                "clientInfo": {"name": "claude-code"}
            }
        }))
        .send()
        .await
        .expect("the daemon answers")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let text = body["result"]["content"][0]["text"].as_str()?;
    let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
    parsed["session"]["id"]
        .as_str()
        .or_else(|| parsed["session_id"].as_str())
        .map(str::to_string)
}

#[tokio::test]
#[ignore]
async fn a_client_that_only_says_claude_code_does_not_inherit_another_agents_session() {
    let _env = ENV_GUARD.lock().await;
    let pool = daemon(18841).await;

    let started = open_a_session(18841, None, "el trabajo de la primera IA").await;
    let owned = started["session"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("jornada start returns the session: {started}"))
        .to_string();

    let inherited = whose_session(18841, None).await;
    assert_ne!(
        inherited.as_deref(),
        Some(owned.as_str()),
        "both calls declared no id, so both fall back to clientInfo.name — the same string for \
         every Claude Code instance. The second call picked up the session the first one \
         opened. Two AIs are connected to this daemon today and only avoid colliding because \
         somebody set Mcp-Client-Id by hand in each project; a third added with the default \
         `claude mcp add` would land here. Losing the session is honest, inheriting somebody \
         else's is not"
    );

    let declared = format!("agent-a-{}", &Uuid::new_v4().to_string()[..8]);
    let mine = open_a_session(18841, Some(&declared), "la que sí se identifica").await;
    let mine_id = mine["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();
    assert_eq!(
        whose_session(18841, Some(&declared)).await.as_deref(),
        Some(mine_id.as_str()),
        "and an agent that DOES declare an id has to keep its session across calls, or the fix \
         works by breaking sessions for everybody, which is the same loss with better manners"
    );

    sqlx::query("DELETE FROM brain_sessions WHERE id = ANY($1::uuid[])")
        .bind(vec![
            Uuid::parse_str(&owned).expect("uuid"),
            Uuid::parse_str(&mine_id).expect("uuid"),
        ])
        .execute(&pool)
        .await
        .ok();
    unsafe {
        std::env::remove_var("CUBA_HTTP_TOKEN");
        std::env::remove_var("MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS");
    }
}
