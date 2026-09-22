use std::time::{Duration, Instant};

use serde_json::json;

const ADMIN: &str = "panel-admin-token";
const PEER: &str = "panel-peer-token";

/// The daemon's own ceiling on loading models before it opens for business.
///
/// Pinned here rather than left at its 180 s default because every wait below
/// is exactly a wait for that opening, and nothing in this file asks the daemon
/// anything a model could answer: it asks about tokens, routes and forwarding
/// headers. The models keep loading past it — `serve_pool` detaches the warm-up
/// and serves with `ready:false` — and no assertion here reads that flag. The
/// default is what put two tests over a minute each in the gate log.
const WARM_CEILING: Duration = Duration::from_secs(10);

/// How long a test here waits for its daemon to open the port.
///
/// `serve_pool` binds the port, *then* warms the models, and only then serves.
/// A connection that arrives in between is accepted by the kernel and parked in
/// the backlog, so "the port took my connection" does not mean "something is
/// answering". A fixed sleep cannot tell those two apart: with a client that has
/// no timeout — which is every other client in this file — the request waits in
/// the backlog until the daemon gets round to it, so the old wait could only
/// ever make this file slow, never red.
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

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One `GET /health`: `Ok` when something served the port, `Err` with why
/// nothing did.
///
/// Whatever came back settles it, status code and body included: the question
/// this probe asks is only whether the port is *served*. What it serves is what
/// the tests below are for.
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

async fn daemon(port: u16, panel: bool, public: bool) -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    unsafe {
        std::env::set_var("CUBA_HTTP_TOKEN", ADMIN);
        std::env::set_var("CUBA_PEER_TOKEN", PEER);
        std::env::set_var(
            "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
            WARM_CEILING.as_secs().to_string(),
        );
        if panel {
            std::env::set_var("CUBA_PANEL", "1");
        } else {
            std::env::remove_var("CUBA_PANEL");
        }
        if public {
            std::env::set_var("CUBA_PANEL_PUBLIC", "1");
        } else {
            std::env::remove_var("CUBA_PANEL_PUBLIC");
        }
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

async fn rpc(port: u16, token: &str, method: &str) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let r = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}}))
        .send()
        .await
        .expect("the daemon answers");
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
#[ignore]
async fn a_peer_token_cannot_open_the_admin_surface() {
    let _env = ENV_GUARD.lock().await;
    let _pool = daemon(18811, true, false).await;

    for method in memory_industry::admin::METHODS {
        let (status, body) = rpc(18811, ADMIN, method).await;
        assert_eq!(status, 200, "{method} refused the admin token: {body}");
        assert!(
            body["result"].is_object(),
            "{method} answered without a result, so the control half of this test proves \
             nothing about the refusal half: {body}"
        );

        let (_, refused) = rpc(18811, PEER, method).await;
        assert!(
            refused["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("peer token")),
            "the peer scope is enforced inside handlers::dispatch, and admin/* does not go \
             through dispatch. Without its own check the read-only token — the one that exists \
             so the other machine cannot call cuba_forget — would read the whole diagnostic \
             surface, the client list and the failure log through a different door. \
             {method} answered: {refused}"
        );
    }

    let (_, unknown) = rpc(18811, ADMIN, "admin/whatever").await;
    assert!(
        unknown["error"].is_object(),
        "an unlisted admin method must not be served: {unknown}"
    );
}

#[tokio::test]
#[ignore]
async fn the_panel_route_stays_shut_unless_it_is_switched_on() {
    let _env = ENV_GUARD.lock().await;
    let _pool = daemon(18812, false, false).await;
    let client = reqwest::Client::new();

    let off = client
        .get("http://127.0.0.1:18812/panel")
        .send()
        .await
        .expect("the daemon answers");
    assert_eq!(
        off.status().as_u16(),
        404,
        "without CUBA_PANEL=1 the route must not even be registered. Default-on would publish \
         an administration page on every install that ever binds anything"
    );

    let health = client
        .get("http://127.0.0.1:18812/health")
        .send()
        .await
        .expect("health answers");
    assert_eq!(
        health.status().as_u16(),
        200,
        "and the rest of the daemon has to keep working — a 404 everywhere would make the \
         first assertion meaningless"
    );
}

#[tokio::test]
#[ignore]
async fn the_panel_refuses_a_request_that_arrived_through_a_tunnel() {
    let _env = ENV_GUARD.lock().await;
    let _pool = daemon(18813, true, false).await;
    let client = reqwest::Client::new();

    let direct = client
        .get("http://127.0.0.1:18813/panel")
        .send()
        .await
        .expect("the daemon answers");
    assert_eq!(
        direct.status().as_u16(),
        200,
        "from this machine, with the switch on, the page has to load"
    );
    let body = direct.text().await.expect("a body");
    assert!(
        body.contains("sessionStorage") && body.contains("/mcp"),
        "and it has to be the real page, not an error rendered with a 200"
    );

    assert!(
        memory_industry::http::FORWARDING_HEADERS.contains(&"forwarded"),
        "the list has to include RFC 7239 `Forwarded`, which is the standard header and the one \
         a proxy that follows the spec sends instead of the x- ones. The first version of this \
         check knew only three names and tested itself against exactly those three: it proved \
         the code matched its own list, not that the list matched what proxies send"
    );

    for header in memory_industry::http::FORWARDING_HEADERS {
        let forwarded = client
            .get("http://127.0.0.1:18813/panel")
            .header(header, "203.0.113.7")
            .send()
            .await
            .expect("the daemon answers");
        assert_eq!(
            forwarded.status().as_u16(),
            403,
            "the Cloudflare tunnel points at 127.0.0.1, so behind it the client address is \
             loopback too and checking the peer address proves nothing — a route registered on \
             'we are bound to loopback' would be published to the internet by a tunnel nobody \
             thought about. The forwarding header is the only thing that tells the two apart, \
             and {header} got through.\n\nAnd the honest limit, which belongs next to the \
             check rather than in a commit nobody re-reads: a raw TCP forward (ssh -L, socat) \
             sends no header at all and cannot be caught this way. This guard stops HTTP \
             proxies; it is not proof that a request is local"
        );
    }
}
