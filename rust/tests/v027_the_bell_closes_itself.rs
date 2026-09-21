use std::time::{Duration, Instant};

use serde_json::{Value, json};
use uuid::Uuid;

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0027;

/// One place owns the port, so the daemon, the readiness probe and the URL the
/// sync call is pointed at cannot drift apart.
const PEER_ADDR: &str = "127.0.0.1:18821";

/// The peer's own ceiling on loading models before it opens for business.
///
/// Pinned here rather than left at its 180 s default because the wait below is
/// exactly a wait for that opening: with the ceiling floating, this test's
/// budget would once more depend on how loaded the machine is, which is the
/// thing that broke it. The models keep loading past it — `serve_pool` detaches
/// the warm-up and serves with `ready:false` — and that is fine here, because
/// `cuba_sync` moves rows and does not rerank.
const PEER_WARM_CEILING: Duration = Duration::from_secs(60);

/// How long this test waits for the peer to open its port.
///
/// `serve_pool` binds the port, *then* warms the models, and only then serves.
/// A connection that arrives in between is accepted by the kernel and parked in
/// the backlog, so "the port took my connection" stopped meaning "something is
/// answering". The client half of this test cuts at `protocol::handler_timeout()`
/// — 30 s — and on a loaded gate box the warm-up outran that: this test went red
/// at 42,5 s on a commit whose only change was a shell script it never runs. The
/// wait belongs on this side, which can afford it, not on the call, which cannot.
const PEER_OPEN_BUDGET: Duration = Duration::from_secs(90);

// The invariant, as a build failure rather than as a sentence somebody can read
// past: the peer is guaranteed to open within its warm ceiling, so a budget
// below that ceiling fails a daemon which did exactly what it was told. The
// headroom pays for the bind, the runtime and the probe interval. Lowering the
// budget to make the suite feel faster is the obvious next edit, and it is the
// one that would quietly bring the flake back — so it does not compile.
const _: () = assert!(
    PEER_OPEN_BUDGET.as_secs() > PEER_WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the peer is failed for opening on time"
);

/// Per probe, so a request parked in the backlog is cut and retried instead of
/// swallowing the whole budget in one silent wait. `/health` asks the database
/// within its own five seconds, and that database is up here.
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
        .expect("CUBA_SYNC_DIR and the tokens are process-global");
    tx
}

async fn call(pool: &sqlx::PgPool, peer: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, "cuba_sync", args)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "the peer was already answering /health when this call started ({peer}), so \
                 this is cuba_sync failing and not a daemon still opening its port: {e:#}"
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

#[tokio::test]
#[ignore]
async fn taking_what_the_peer_offered_silences_its_bell() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let remote_url = std::env::var("CUBA_PEER_DATABASE_URL").expect(
        "this needs a second database, provisioned by scripts/run-all-tests.sh. Skipping when it \
         is absent would report green for a machine that never ran two nodes",
    );

    let local = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to the local database");
    let _one_at_a_time = own_the_process(&local).await;
    let remote = memory_industry::db::create_pool(&remote_url)
        .await
        .expect("connect to the second node");

    let bundle = std::env::temp_dir().join(format!("cuba-bell-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("scratch");
    unsafe {
        std::env::set_var("CUBA_SYNC_DIR", &bundle);
        std::env::set_var("CUBA_PEER_TOKEN", "bell-secret");
        std::env::set_var("CUBA_HTTP_TOKEN", "bell-admin-secret");
        std::env::set_var(
            "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
            PEER_WARM_CEILING.as_secs().to_string(),
        );
    }
    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&local)
        .await
        .expect("clean inbox");
    sqlx::query("DELETE FROM brain_sync_peers")
        .execute(&local)
        .await
        .expect("clean cursor");

    let marker = format!("bell_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&remote)
        .await
        .expect("the other machine learns something");

    let peer_node: Uuid = sqlx::query_scalar("SELECT node_id FROM brain_node_identity")
        .fetch_one(&remote)
        .await
        .expect("the second node has an identity of its own");
    let local_node: Uuid = sqlx::query_scalar("SELECT node_id FROM brain_node_identity")
        .fetch_one(&local)
        .await
        .expect("and so does this one");
    assert_ne!(
        peer_node, local_node,
        "two installs must not share a node id, or closing a notice by origin would close this \
         machine's own bells as well. Migration 0046 generates it per database for exactly \
         this. Read straight from the table and not through db::node_id, which memoises in a \
         process-wide OnceCell: correct for a daemon that serves one database, wrong for a test \
         holding two pools, where it hands back whichever it saw first"
    );

    sqlx::query(
        "INSERT INTO brain_peer_notices (node_id, node_name, summary)
         VALUES ($1, 'la-otra', $2)",
    )
    .bind(peer_node)
    .bind(format!("{marker} encontré algo que no tenés"))
    .execute(&local)
    .await
    .expect("the peer rings the bell");

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

    let open_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_peer_notices WHERE resolved_at IS NULL")
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        open_before, 1,
        "the bell has to be ringing before we answer it"
    );

    let fetched = call(
        &local,
        &peer,
        json!({
            "action": "fetch",
            "peer": "la-otra",
            "url": format!("http://{PEER_ADDR}"),
            "conflict": "skip"
        }),
    )
    .await;

    assert_eq!(
        fetched["notices_closed"].as_u64(),
        Some(1),
        "a notice says «I have something»; taking it is what makes that stop being true. \
         Closing by origin rather than by manifest hash is what lets the sender ring the bell \
         without first exporting a bundle just to learn its own hash — at four writes a day \
         that export would cost more than the change it announces. Got: {fetched}"
    );

    let still_open: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_peer_notices WHERE resolved_at IS NULL")
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        still_open, 0,
        "and it has to be closed in the database, not merely counted in the answer"
    );

    let arrived: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE name = $1")
        .bind(&marker)
        .fetch_one(&local)
        .await
        .expect("count");
    assert_eq!(
        arrived, 1,
        "and the thing the bell was about has to have actually arrived, or the notice was \
         closed on a promise"
    );

    daemon.abort();
    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&marker)
        .execute(&local)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&local)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe {
        std::env::remove_var("CUBA_SYNC_DIR");
        std::env::remove_var("CUBA_PEER_TOKEN");
        std::env::remove_var("CUBA_HTTP_TOKEN");
        std::env::remove_var("MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS");
    }
}
