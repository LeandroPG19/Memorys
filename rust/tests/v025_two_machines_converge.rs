use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0027;

/// One place owns the port, so the daemon, the address it is announced at and
/// the URL the fetch is pointed at cannot drift apart.
const PEER_ADDR: &str = "127.0.0.1:18797";

/// The peer's own ceiling on loading models before it opens for business.
///
/// Pinned here rather than left at its 180 s default because the wait below is
/// exactly a wait for that opening: with the ceiling floating, this test's
/// budget would depend on how loaded the machine is. The models keep loading
/// past it — `serve_pool` detaches the warm-up and serves with `ready:false` —
/// and that is fine here, because `cuba_sync` moves rows and does not rerank.
const PEER_WARM_CEILING: Duration = Duration::from_secs(60);

/// How long this test waits for the peer to open its port.
///
/// `serve_pool` binds the port, *then* warms the models, and only then serves,
/// so a connection that arrives in between is accepted by the kernel and parked
/// in the backlog: "the port took my connection" stopped meaning "something is
/// answering". The client half of this test cuts at `protocol::handler_timeout()`
/// — 30 s — and on a loaded box the warm-up outran it. That is what took down
/// the sibling test in `v027_the_bell_closes_itself`, which waited the same 400
/// milliseconds this one used to; the failure is the same one and only the port
/// differs.
const PEER_OPEN_BUDGET: Duration = Duration::from_secs(90);

// The invariant, as a build failure rather than as a sentence somebody can read
// past: the peer is guaranteed to open within its warm ceiling, so a budget
// below that ceiling fails a daemon which did exactly what it was told.
// Lowering the budget to make the suite feel faster is the obvious next edit,
// and it is the one that would quietly bring the flake back.
const _: () = assert!(
    PEER_OPEN_BUDGET.as_secs() > PEER_WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the peer is failed for opening on time"
);

/// Per probe, so a request parked in the backlog is cut and retried instead of
/// swallowing the whole budget in one silent wait.
const PROBE_BUDGET: Duration = Duration::from_secs(5);

/// Between probes: short enough to add nothing visible to a fast start, long
/// enough not to spin against a socket nobody is serving yet.
const PROBE_EVERY: Duration = Duration::from_millis(250);

async fn own_the_process(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_SYNC_DIR and CUBA_PEER_TOKEN are process-global");
    tx
}

async fn call(pool: &sqlx::PgPool, tool: &str, peer: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "{tool} failed while the peer was already answering /health ({peer}), so this \
                 is the tool and not a daemon still opening its port: {e:#}"
            )
        });
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

/// One `GET /health`: `Ok` with what the peer said about itself, `Err` with why
/// it said nothing at all.
///
/// A body that is not JSON is still an answer, and so lands in `Ok`: the
/// question this probe asks is whether the port is *served*, and anything that
/// came back over it settles that.
async fn probe(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("no answer at all: {e}"))?;
    let code = response.status().as_u16();
    Ok(match response.json::<Value>().await {
        Ok(body) => format!(
            "HTTP {code}, status={}, ready={}",
            body["status"], body["ready"]
        ),
        Err(e) => format!("HTTP {code} with a body that is not JSON: {e}"),
    })
}

/// Block until the peer serves, and hand back what it said about itself.
///
/// Polling rather than sleeping a fixed number: any number would be right on an
/// idle machine and wrong on the busy one that actually failed, which is the
/// same race wearing a calmer name.
///
/// The condition is that `/health` *answers*, not that it answers `ready:true`.
/// Waiting for the socket is not enough — the port is bound before anything
/// serves it — but `ready` is more than this test needs: it means the models
/// finished loading, and `cuba_sync` moves rows without them. Worse, it is not
/// a signal that is guaranteed to arrive: a warm-up that overruns
/// `MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS` leaves a daemon that answers
/// everything and reports `ready:false` for as long as it takes, so a test
/// waiting on it would fail a peer that was working. `ready` is still worth
/// carrying into the failure message of whatever fails next, which is why this
/// returns it instead of dropping it.
async fn wait_until_the_peer_serves(daemon: &tokio::task::JoinHandle<()>) -> String {
    let url = format!("http://{PEER_ADDR}/health");
    let client = reqwest::Client::builder()
        .timeout(PROBE_BUDGET)
        .build()
        .expect("a probe client with a bounded attempt");
    let deadline = Instant::now() + PEER_OPEN_BUDGET;
    let mut probes = 0u32;
    let mut last = String::from(
        "it never answered: the port was bound, so the connection was queued rather than \
         refused, and nothing ever served it",
    );

    while Instant::now() < deadline {
        assert!(
            !daemon.is_finished(),
            "the peer daemon on {PEER_ADDR} ended before it served anything, so nothing below \
             can run. serve_pool printed why it stopped — with --nocapture that line is just \
             above this one; the port still held by an earlier run is the usual reason"
        );

        probes += 1;
        match probe(&client, &url).await {
            Ok(said) => return said,
            Err(why) => last = why,
        }
        tokio::time::sleep(PROBE_EVERY).await;
    }

    panic!(
        "the peer at {url} never answered within {}s, over {probes} probe(s). Last: {last}. \
         This is the daemon still opening, not the sync call failing: serve_pool binds the \
         port before it loads its models, so the kernel queues the connection instead of \
         refusing it and an accepted connection says nothing about whether anything is \
         serving. This peer is pinned to open within {}s of warm-up, so overrunning the \
         budget means it never opened at all",
        PEER_OPEN_BUDGET.as_secs(),
        PEER_WARM_CEILING.as_secs()
    );
}

fn second_node_url() -> String {
    std::env::var("CUBA_PEER_DATABASE_URL").expect(
        "the two-node test needs a second database and the runtime role cannot create one, so \
         scripts/run-all-tests.sh provisions it and exports CUBA_PEER_DATABASE_URL. Skipping \
         when it is missing would report green for a machine that never ran two nodes",
    )
}

#[tokio::test]
#[ignore]
async fn what_one_machine_learns_offline_reaches_the_other_when_the_link_comes_back() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let remote_url = second_node_url();

    let local = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to the local database");
    let _one_at_a_time = own_the_process(&local).await;

    let remote = memory_industry::db::create_pool(&remote_url)
        .await
        .unwrap_or_else(|e| panic!("connecting to the second node at {remote_url}: {e:#}"));
    sqlx::query("DELETE FROM brain_sync_peers")
        .execute(&local)
        .await
        .expect("start from no cursor");

    let bundle = std::env::temp_dir().join(format!("cuba-2n-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch sync directory");
    unsafe {
        std::env::set_var("CUBA_SYNC_DIR", &bundle);
        std::env::set_var("CUBA_PEER_TOKEN", "two-node-secret");
        std::env::set_var("CUBA_HTTP_TOKEN", "admin-secret-that-differs");
        std::env::set_var("CUBA_HTTP_ADDR", PEER_ADDR);
        std::env::set_var(
            "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
            PEER_WARM_CEILING.as_secs().to_string(),
        );
    }

    let marker = format!("offline_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&remote)
        .await
        .expect("the other machine learns something while the link is down");
    sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
        .bind(entity_id)
        .bind(format!(
            "{marker} el pool se agota a 40 conexiones bajo carga real"
        ))
        .execute(&remote)
        .await
        .expect("seed the observation");

    let served = remote.clone();
    let daemon = tokio::spawn(async move {
        // Printed rather than dropped: when it is the bind that failed, the
        // readiness probe below only ever sees a refused connection, and the
        // sentence naming the cause lives in this Err and nowhere else.
        if let Err(why) = memory_industry::http::serve_pool(PEER_ADDR, served, true).await {
            eprintln!("the peer daemon on {PEER_ADDR} stopped: {why:#}");
        }
    });
    let peer = wait_until_the_peer_serves(&daemon).await;

    let before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE content LIKE $1")
            .bind(format!("%{marker}%"))
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        before, 0,
        "the local machine must not already have what the remote learned, or the test proves \
         nothing about it travelling"
    );

    let fetched = call(
        &local,
        "cuba_sync",
        &peer,
        json!({
            "action": "fetch",
            "peer": "the-other-one",
            "url": format!("http://{PEER_ADDR}"),
            "conflict": "skip"
        }),
    )
    .await;
    assert!(
        fetched["imported"]["rows_inserted"].as_u64().unwrap_or(0) > 0,
        "the fetch reported no rows, so either the link or the import did nothing: {fetched}"
    );

    let after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE content LIKE $1")
            .bind(format!("%{marker}%"))
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        after, 1,
        "what the other machine learned while the link was down has to be here now. This is the \
         whole promise: nothing is lost while disconnected, and reconnecting is what moves it"
    );

    let again = call(
        &local,
        "cuba_sync",
        &peer,
        json!({"action": "fetch", "peer": "the-other-one", "conflict": "skip"}),
    )
    .await;
    assert_eq!(
        again["unchanged"].as_bool(),
        Some(true),
        "a second fetch with nothing new has to stop at the cursor without opening a \
         transaction. Before the peer table existed the only loop breaker was the manifest \
         hash, and ordinary use moves access_count, so every export produced a new hash and \
         both sides re-imported forever: the cycle converged in data and never in work. \
         Got: {again}"
    );
    assert!(
        again["url"].as_str().is_some_and(|u| u.contains("18797")),
        "and the address has to be remembered, or every fetch needs it spelled out again: \
         {again}"
    );

    daemon.abort();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe {
        std::env::remove_var("CUBA_SYNC_DIR");
        std::env::remove_var("CUBA_PEER_TOKEN");
        std::env::remove_var("CUBA_HTTP_TOKEN");
        std::env::remove_var("CUBA_HTTP_ADDR");
        std::env::remove_var("MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS");
    }
}
