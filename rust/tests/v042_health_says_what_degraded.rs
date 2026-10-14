//! `/health` says what is degraded, and says it to whoever is allowed to know.
//!
//! Two failures meet here. One is that a daemon which serves lexical search
//! perfectly well answered `503 SERVICE UNAVAILABLE` because PostgreSQL was
//! unreachable, and every monitor reads a 503 as "dead, stop sending traffic" —
//! so the one endpoint that could have said what was wrong was also the one
//! that got the daemon taken out of rotation. The other is that an operator
//! could not ask the daemon where its reranker was running and reconstructed
//! the answer by reading the source.
//!
//! Every test here runs in the ordinary suite and none of them needs
//! `DATABASE_URL`. The pool points at a listener that hangs up at once, which
//! is not a limitation: "the database is down" is one of the two states being
//! pinned, and every other assertion is about what this process says about
//! *itself*. That also keeps this file inside the plain `cargo test` run, where
//! a test that needed a database could not go.

use std::ffi::OsString;
use std::time::{Duration, Instant};

use memory_industry::http::{ModelState, RuntimeReport, overall_status};
use memory_industry::llm_cli::LlmSummary;
use serde_json::Value;

// A database that is certainly not there, and whose password is a canary.
mod dead_db;
// The port of each daemon, asked of the OS.
mod ephemeral;

/// Under HOST_IDENTITY_BUDGET (2 s), the shortest budget a dead database used to eat.
const DEAD_DB_CEILING: Duration = Duration::from_secs(1);

/// Long enough to be a real bearer token, so nothing in the bind guard has an
/// opinion about it and the assertions are about `/health` alone.
const TOKEN: &str = "canary-token-3f8a1c77b204e95d6ab0f1c2";

/// The daemon's own ceiling on loading models before it opens for business.
///
/// The number the wait below is a wait for, so it lives next to it rather than
/// as a literal in `quiet_machine()`, where nothing tied it to the budget that
/// has to outlast it. Past the ceiling `serve_pool` detaches the warm-up and
/// serves with `ready:false`, which is a state this file reads on purpose.
const WARM_CEILING: Duration = Duration::from_secs(10);

/// How long a test here waits for its daemon to open the port.
///
/// `serve_pool` binds the port, *then* warms the models, and only then serves.
/// A connection that arrives in between is accepted by the kernel and parked in
/// the backlog, so "the port took my connection" does not mean "something is
/// answering".
const OPEN_BUDGET: Duration = Duration::from_secs(30);

// The invariant, as a build failure rather than as a sentence somebody can read
// past: the daemon is guaranteed to open within its warm ceiling, so a budget
// below that ceiling fails a daemon which did exactly what it was told. The two
// halves used to be a coincidence — a literal "10" in the environment and a
// hundred sleeps of ten milliseconds down here — and a coincidence is not an
// invariant: either number could move alone. Now the smaller one does not
// compile.
const _: () = assert!(
    OPEN_BUDGET.as_secs() > WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the daemon is failed for opening on time"
);

/// Per probe, so a request parked in the backlog is cut and retried instead of
/// swallowing the whole budget in one silent wait. `/connect` is a page
/// compiled into the binary and answers as fast as the socket allows, so this
/// budget only ever cuts a connection that nothing is serving yet.
const PROBE_BUDGET: Duration = Duration::from_secs(5);

/// Between probes: short enough to add nothing visible to a fast start, long
/// enough not to spin against a socket nobody is serving yet.
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// Process-global environment, one test at a time.
static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Puts a variable back the way it was when the guard drops, panic or not.
///
/// `envs::ScopedEnv` in the library does exactly this and is behind
/// `cfg(test)`, so it does not exist for a test compiled against the library.
/// Restoring in `Drop` rather than after the assertions is the whole point: an
/// assertion that fires unwinds, and a leaked `CUBA_RERANKER_PATH` would then
/// follow every later test in this binary.
struct Env {
    name: &'static str,
    previous: Option<OsString>,
}

impl Env {
    fn set(name: &'static str, value: &str) -> Self {
        Self::swap(name, Some(value))
    }

    fn cleared(name: &'static str) -> Self {
        Self::swap(name, None)
    }

    fn swap(name: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var_os(name);
        unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        Self { name, previous }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

/// A machine with no models and no panel, whatever this developer has
/// installed.
///
/// Not tidiness: the daemon warms its models before it serves, and on a
/// machine with the reranker in the cache that is a minute of loading 1,1 GB
/// inside this test — and a different `runtime` block than the gate box would
/// produce. Pointing the paths at the resource-plan sentinel and at a
/// directory that does not exist makes both machines answer the same.
fn quiet_machine() -> Vec<Env> {
    let nowhere = std::env::temp_dir().join("v042-health-no-such-model");
    let ceiling = WARM_CEILING.as_secs().to_string();
    vec![
        Env::set("CUBA_HTTP_TOKEN", TOKEN),
        Env::cleared("CUBA_PEER_TOKEN"),
        Env::set("CUBA_RERANKER_PATH", "disabled-by-resource-plan"),
        Env::set("CUBA_NLI_PATH", "disabled-by-resource-plan"),
        Env::set("ONNX_MODEL_PATH", &nowhere.to_string_lossy()),
        Env::set("CUBA_WARM_RERANKER", "0"),
        // The preferred name, because it is the one that wins: leaving the
        // 180 s default in place would let one unexpected model on this
        // developer's disk hold the whole file for three minutes.
        Env::set("MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS", &ceiling),
        Env::cleared("CUBA_PANEL"),
        Env::cleared("CUBA_IDLE_SHUTDOWN_SECS"),
        Env::cleared("CUBA_HTTP_ALLOWED_ORIGINS"),
    ]
}

/// One `GET /connect`: `Ok` when something served the port, `Err` with why
/// nothing did.
///
/// Whatever came back settles it, status code and body included: the question
/// this probe asks is only whether the port is *served*.
async fn probe(client: &reqwest::Client, url: &str) -> Result<(), String> {
    client
        .get(url)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| format!("no answer at all: {e}"))
}

/// Start a daemon and wait until it answers, or fail saying it never did.
///
/// The readiness probe goes to `/connect`, not to `/health`: `/connect` is a
/// page compiled into the binary and needs nothing, so a probe that only asks
/// whether the port is served does not have to ask the database.
///
/// Polling rather than sleeping a fixed number: any number is right on an idle
/// machine and wrong on the busy one that actually failed. The condition is
/// that the port *answers*, not that the daemon is `ready`: past
/// `WARM_CEILING` it serves with `ready:false` for as long as the models take,
/// so a wait on that flag could outlive a daemon that was working — and two of
/// the tests below are written against exactly that daemon.
///
/// Returns the port the daemon was given. It is asked of the OS on the line
/// before the spawn, and callers hold `ENV_GUARD`, so no other daemon of this
/// binary can be handed the same one in between.
async fn daemon() -> u16 {
    let pool = memory_industry::db::create_lazy_pool(&dead_db::url());
    let port = ephemeral::port();
    let serve_task = tokio::spawn(async move {
        let addr = format!("127.0.0.1:{port}");
        // Printed rather than dropped: when it is the bind that failed, the
        // wait below only ever sees a connection nobody answers, and the
        // sentence naming the cause lives in this Err and nowhere else.
        if let Err(why) = memory_industry::http::serve_pool(&addr, pool, false).await {
            eprintln!("the daemon on 127.0.0.1:{port} stopped: {why:#}");
        }
    });

    let url = format!("http://127.0.0.1:{port}/connect");
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
            !serve_task.is_finished(),
            "the daemon on 127.0.0.1:{port} ended before it served anything, so nothing below \
             can run. serve_pool printed why it stopped — with --nocapture that line is just \
             above this one; `already in use` there means the OS gave this port to another \
             socket between `ephemeral::port()` and the bind, see tests/ephemeral/mod.rs"
        );

        probes += 1;
        match probe(&client, &url).await {
            Ok(()) => return port,
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

async fn health(port: u16, token: Option<&str>) -> (u16, Value) {
    let mut request = reqwest::Client::new().get(format!("http://127.0.0.1:{port}/health"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.expect("the daemon answers /health");
    let status = response.status().as_u16();
    (status, response.json().await.expect("/health returns JSON"))
}

#[tokio::test]
async fn health_without_a_token_does_not_inventory_the_machine() {
    let _one_at_a_time = ENV_GUARD.lock().await;
    let _env = quiet_machine();
    let port = daemon().await;

    // The control comes first and is not optional. Asserting that an
    // anonymous caller does not get these keys proves nothing at all against a
    // daemon that never emits them, which is exactly what this daemon was
    // doing before the block existed.
    let (_, authorized) = health(port, Some(TOKEN)).await;
    let runtime = authorized
        .get("runtime")
        .unwrap_or_else(|| panic!("a caller with the admin token gets the block: {authorized}"));
    for key in [
        "mode",
        "resource_tier",
        "embedder",
        "reranker",
        "nli",
        "gpu",
        "llm",
    ] {
        assert!(
            runtime.get(key).is_some(),
            "the operator asks this endpoint what this machine is running; {key} is missing: \
             {runtime}"
        );
    }
    for model in ["embedder", "reranker", "nli"] {
        for field in ["state", "device"] {
            assert!(
                runtime[model].get(field).is_some(),
                "«where is the reranker running» is the question that sent an operator to \
                 read the source; {model}.{field} is missing: {runtime}"
            );
        }
    }
    for field in ["build", "degraded", "placement"] {
        assert!(
            runtime["gpu"].get(field).is_some(),
            "gpu.{field} is missing: {runtime}"
        );
    }

    // The anchor first: `quiet_machine()` points both paths at the resource-plan
    // sentinel, so these two are `off`. The embedder is left alone on purpose —
    // whether it is `warming`, `fallback` or `loaded` depends on the clock and on
    // this developer's disk.
    for model in ["reranker", "nli"] {
        assert_eq!(
            runtime[model]["state"], "off",
            "the fixture is broken: {model} is not switched off by the plan, so the device \
             assertion below is about some other state: {runtime}"
        );
        assert_eq!(
            runtime[model].get("device"),
            Some(&Value::Null),
            "{model} never opened a session, so it runs on no device. The key has to be there \
             and hold `null`: what this field used to say was the device the placement path \
             would have chosen, and on a model switched off that is a word nobody measured: \
             {runtime}"
        );
    }

    let (_, anonymous) = health(port, None).await;
    assert!(
        anonymous.get("runtime").is_none(),
        "`/health` answers anyone who can route a packet to the port. The runtime block is an \
         inventory of the machine — which models, on which device, against which provider — \
         and it is the shopping list of whoever is looking for a way in: {anonymous}"
    );
    let public = anonymous.to_string();
    for leaked in ["reranker", "resource_tier", "placement", "llm", "gpu"] {
        assert!(
            !public.contains(leaked),
            "{leaked:?} reached an unauthenticated caller: {public}"
        );
    }

    assert_eq!(
        anonymous["version"], authorized["version"],
        "liveness has to survive the redaction — a monitor with no token is the reason this \
         endpoint exists"
    );
    assert_eq!(
        anonymous["status"], authorized["status"],
        "and the verdict is not a secret either. Hiding «degraded» from the caller without a \
         token would mean the monitor is the one component that cannot see the problem"
    );
}

#[tokio::test]
async fn health_never_prints_a_secret() {
    let _one_at_a_time = ENV_GUARD.lock().await;
    let mut env = quiet_machine();
    env.push(Env::set("DATABASE_URL", &dead_db::url()));
    env.push(Env::cleared("MEMORY_INDUSTRY_LLM_PROVIDER"));
    env.push(Env::cleared("CUBA_LLM_PROVIDER"));
    env.push(Env::set(
        "MEMORY_INDUSTRY_LLM_BASE_URL",
        "http://canaryuser:canarypass-llm@127.0.0.1:11434/v1?api-key=sk-canary-inquery",
    ));
    env.push(Env::set("MEMORY_INDUSTRY_LLM_API_KEY", "sk-canary-apikey"));
    env.push(Env::set("MEMORY_INDUSTRY_LLM_MODEL", "qwen2.5-canary"));
    let port = daemon().await;

    let (_, authorized) = health(port, Some(TOKEN)).await;
    let (_, anonymous) = health(port, None).await;

    // Positive control. Without it an empty `llm` block would pass every
    // assertion below while telling the operator nothing.
    let llm = authorized["runtime"]["llm"].to_string();
    assert!(
        llm.contains("127.0.0.1:11434") && llm.contains("qwen2.5-canary"),
        "the point of this block is that the operator can read which provider and which model \
         this daemon is set to; redacting it into uselessness passes the canaries and fails \
         the operator: {llm}"
    );

    for body in [&authorized, &anonymous] {
        let text = body.to_string();
        for canary in [
            "canarypass-llm",
            "sk-canary-inquery",
            "sk-canary-apikey",
            "canarypass-db",
            TOKEN,
        ] {
            assert!(
                !text.contains(canary),
                "{canary:?} was served by /health. An API key, a database password and the \
                 bearer token itself are the three things this endpoint may never print, on a \
                 LAN bind or anywhere else: {text}"
            );
        }
    }
}

#[tokio::test]
async fn the_dead_database_hangs_up_at_once_instead_of_timing_out() {
    let pool = memory_industry::db::create_lazy_pool(&dead_db::url());
    let started = Instant::now();
    let refused = pool
        .acquire()
        .await
        .map(drop)
        .expect_err("nothing serves PostgreSQL behind the dead database");
    let took = started.elapsed();

    assert!(
        matches!(&refused, sqlx::Error::Io(_))
            || matches!(&refused, sqlx::Error::Protocol(text) if text.contains("SSLRequest")),
        "the dead database must fail because the peer hung up, and got {refused:?} after \
         {took:?}. The hang-up has two shapes by system, both immediate and neither retried by \
         sqlx: on Windows the close arrives as a RST and the error is Io (os error 10054); on \
         Linux it arrives as an EOF while reading the answer to the SSLRequest and the error is \
         Protocol(\"unexpected response from SSLRequest: 0x00\") (measured 2026-10-13, 659 \
         us). What this must reject is PoolTimedOut: sqlx 0.8 retries a refused connection \
         until the pool's acquire_timeout (5 s, db.rs) and then returns it, so a port with no \
         listener, or a listener that never answers, is slow and ends in that error. A URL \
         that does not parse is no better: create_lazy_pool falls back to localhost:5432 and \
         the PG* variables, which could reach a real server"
    );
    assert!(
        took < DEAD_DB_CEILING,
        "the dead database took {took:?} to fail, over {DEAD_DB_CEILING:?}. Every daemon this \
         suite starts pays 2 s of it before it binds (host identity) and every /health pays \
         up to 5 s, on Linux as well as Windows: v042 alone measured 31 s of waiting for a \
         database that was never going to answer. Error: {refused:?}"
    );
}

#[tokio::test]
async fn a_daemon_whose_database_is_down_still_answers_200() {
    let _one_at_a_time = ENV_GUARD.lock().await;
    let _env = quiet_machine();
    let port = daemon().await;

    let (code, body) = health(port, Some(TOKEN)).await;

    assert_eq!(
        body["database"], "unreachable",
        "the control: if the database were somehow reachable the 200 below would be the easy \
         200 and would pin nothing: {body}"
    );
    assert_eq!(
        code, 200,
        "a 503 tells every monitor and every proxy to take this daemon out of rotation. It \
         still serves lexical search and it still serves this endpoint, so the 503 removed a \
         working daemon and took the explanation with it: {body}"
    );
    assert_eq!(
        body["status"], "degraded",
        "200 is not «fine». The status code is for the transport, the body is for the \
         decision: {body}"
    );
}

/// Every model in the report at one state, with the rest of the machine
/// nailed down, so a row varies in exactly the thing it names.
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
        llm: LlmSummary {
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
    //  state) → the one word a monitor matches on.
    //
    // Written out rather than computed: an expectation derived from the same
    // condition the function uses is a tautology, and this table is the whole
    // thing standing between «degraded» and a word that quietly changes.
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
         reranker alone would pass every row of the table above and still miss a model that is \
         on disk and will not open"
    );

    assert_eq!(
        overall_status(true, &report(true, false, ["fallback", "loaded", "loaded"])),
        "ok",
        "a machine that never had an embedding model is Tier::Minimal, not a fault, and this \
         daemon is supported there. Reporting every one of them as degraded forever is how a \
         field stops being read — which is the failure this endpoint exists to prevent"
    );

    assert_eq!(
        overall_status(true, &report(true, false, ["failed", "loaded", "loaded"])),
        "degraded",
        "and the other half of that same distinction: an embedder whose model was there and \
         whose session would not open is a fault. Until `failed` existed for this model the row \
         above was the only answer either machine could get, so the two were indistinguishable \
         from outside — which is what made the supported one look broken and the broken one \
         look supported"
    );

    assert_eq!(
        overall_status(true, &report(false, false, ["loaded", "loaded", "loaded"])),
        "starting",
        "and «still loading» is not «broken». It fixes itself; degraded does not, and the two \
         ask different things of whoever is reading"
    );
}
