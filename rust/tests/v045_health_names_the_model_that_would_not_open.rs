//! `/health` says which model would not open, and says it without the path.
//!
//! The three cells this reads — `embeddings::onnx::failure_reason`,
//! `cognitive::nli::failure_reason` and `search::rerank::failure_reason` — are
//! process-wide `OnceLock`s that `cargo mutants -- --lib` cannot put in a known
//! state, so the mutation judge excludes them, and until this file no test of
//! the local gate read them through `/health` either:
//! v042_health_says_what_degraded points every model at nothing on purpose,
//! which only ever reaches `fallback`, `off` and `absent`. Replacing any of the
//! three with `None` turned a broken model into a healthy-looking one and left
//! every test green.
//!
//! «Failed» is made here without downloading anything: ONNX Runtime is a file
//! of bytes that is not a library, the model is a file of bytes that is not a
//! graph, and every model directory points at them. The session fails to open
//! at the first step it takes, loading the runtime, which is the same `Failed`
//! arm a corrupt model reaches one step later. A real runtime with a corrupt
//! model would make this file depend on whatever runtime the machine has, and
//! would keep it out of every job that has none; this one runs anywhere.
//!
//! What it does not reach is `loaded`: that takes a session that opens, which
//! takes a real model, and a model loaded into this process would take the
//! cell this test needs broken. The local gate loads the real ones elsewhere —
//! v043_a_model_of_another_width_does_not_start asserts the embedder, and
//! nli_entailment the NLI model, behind `require_present` in
//! scripts/run-all-tests.sh — and neither of those reads `/health`.
//!
//! One test, and it has to stay one: the three cells belong to the process,
//! and a second test here would find them already resolved by the first.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;

/// A database that is certainly not there. Nothing asserted here is about the
/// database; `/health` just has to be able to ask one.
const DEAD_DB: &str = "postgresql://cuba:canarypass-db@127.0.0.1:1/nowhere";

/// Long enough to be a real bearer token, so nothing in the bind guard has an
/// opinion about it.
const TOKEN: &str = "canary-token-7d2e90c4a1b35f68e0c9d4b7";

const PORT: u16 = 18854;

/// The daemon's own ceiling on loading models before it opens for business.
/// Past it `serve_pool` serves with `ready:false`, and a failure still wins
/// over `warming`, so the assertions below hold on either side of it.
const WARM_CEILING: Duration = Duration::from_secs(10);

/// How long the test waits for the daemon to open the port.
const OPEN_BUDGET: Duration = Duration::from_secs(30);

const _: () = assert!(
    OPEN_BUDGET.as_secs() > WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the daemon is failed for opening on time"
);

const PROBE_BUDGET: Duration = Duration::from_secs(5);
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// Puts a variable back the way it was when the guard drops, panic or not.
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

/// A directory that holds a runtime and a model, neither of which is one.
///
/// Its name is the needle: the load failures format the path they could not
/// open, and none of it may reach `/health`.
struct BrokenInstall {
    dir: PathBuf,
    runtime: PathBuf,
}

impl BrokenInstall {
    fn new() -> Self {
        // One test per process, so the process id is unique enough.
        let dir = std::env::temp_dir().join(format!("v045-broken-install-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the temp dir is writable");
        // The name the loader would look for on this platform, so the file is
        // a runtime that is there and will not load — not one that is missing.
        let runtime = dir.join(if cfg!(windows) {
            "onnxruntime.dll"
        } else {
            "libonnxruntime.so"
        });
        std::fs::write(&runtime, b"these bytes are not a shared library\n")
            .expect("the temp dir is writable");
        std::fs::write(
            dir.join("model.onnx"),
            b"these bytes are not an ONNX graph\n",
        )
        .expect("the temp dir is writable");
        Self { dir, runtime }
    }

    /// The part of the path no other directory on the machine has.
    fn needle(&self) -> String {
        self.dir
            .file_name()
            .expect("a directory under temp has a name")
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for BrokenInstall {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Every model pointed at the broken install, and the reranker told to warm
/// at startup so the daemon opens it itself.
fn broken_machine(install: &BrokenInstall) -> Vec<Env> {
    let dir = install.dir.to_string_lossy().into_owned();
    let ceiling = WARM_CEILING.as_secs().to_string();
    vec![
        Env::set("CUBA_HTTP_TOKEN", TOKEN),
        Env::cleared("CUBA_PEER_TOKEN"),
        Env::set("ORT_DYLIB_PATH", &install.runtime.to_string_lossy()),
        Env::set("ONNX_MODEL_PATH", &dir),
        Env::set("CUBA_NLI_PATH", &dir),
        Env::set("CUBA_RERANKER_PATH", &dir),
        // The preferred name wins over CUBA_WARM_RERANKER, so it is the one
        // that has to be pinned: a developer who set it to 0 would otherwise
        // leave the reranker unasked and its cell empty.
        Env::set("MEMORY_INDUSTRY_WARM_RERANKER", "1"),
        Env::set("MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS", &ceiling),
        Env::cleared("CUBA_PANEL"),
        Env::cleared("CUBA_IDLE_SHUTDOWN_SECS"),
        Env::cleared("CUBA_HTTP_ALLOWED_ORIGINS"),
    ]
}

/// Start a daemon and wait until it answers, or fail saying it never did.
async fn daemon(port: u16) {
    let pool = memory_industry::db::create_lazy_pool(DEAD_DB);
    let addr = format!("127.0.0.1:{port}");
    let serve_task = tokio::spawn(async move {
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
    let mut last = String::from("it never answered");

    while Instant::now() < deadline {
        assert!(
            !serve_task.is_finished(),
            "the daemon on 127.0.0.1:{port} ended before it served anything; serve_pool printed \
             why just above (with --nocapture). The port still held by an earlier run is the \
             usual reason"
        );
        match client.get(&url).send().await {
            Ok(_) => return,
            Err(e) => last = format!("no answer at all: {e}"),
        }
        tokio::time::sleep(PROBE_EVERY).await;
    }

    panic!(
        "the daemon at {url} never answered within {}s. Last: {last}",
        OPEN_BUDGET.as_secs()
    );
}

async fn health(port: u16, token: Option<&str>) -> Value {
    let mut request = reqwest::Client::new().get(format!("http://127.0.0.1:{port}/health"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request
        .send()
        .await
        .expect("the daemon answers /health")
        .json()
        .await
        .expect("/health returns JSON")
}

#[tokio::test]
async fn a_model_that_would_not_open_is_failed_in_health_and_its_path_stays_home() {
    let install = BrokenInstall::new();
    let _env = broken_machine(&install);
    daemon(PORT).await;

    // The daemon warms the embedder and, told to, the reranker. Nothing warms
    // NLI: its first use does, and a verify or a contradiction check calls
    // exactly this. The cell it resolves is the one `/health` reads, because
    // the daemon above runs in this same process.
    let nli_opened = tokio::task::spawn_blocking(memory_industry::cognitive::nli::enabled)
        .await
        .expect("the NLI load does not panic");
    assert!(
        !nli_opened,
        "a runtime that is not a library opened an NLI session, so nothing below is about a \
         failed model"
    );

    let body = health(PORT, Some(TOKEN)).await;
    let runtime = body
        .get("runtime")
        .unwrap_or_else(|| panic!("a caller with the admin token gets the runtime block: {body}"));
    let needle = install.needle();

    for model in ["embedder", "reranker", "nli"] {
        let report = &runtime[model];
        assert_eq!(
            report["state"], "failed",
            "{model}: its model directory holds a model and a runtime, and the session would not \
             open. That is the one state that sends somebody to look, and `fallback`, `absent` or \
             `loaded` here would tell them the machine is fine: {report}"
        );
        let reason = report["reason"]
            .as_str()
            .unwrap_or_else(|| panic!("{model}: a failed model says why: {report}"));
        assert!(
            !reason.trim().is_empty(),
            "{model}: an empty reason is a failure nobody can act on: {report}"
        );
        assert!(
            reason.contains("(see "),
            "{model}: the load failure named a path, and /health replaces it with the variable \
             that sets it. Without the replacement either the path went out whole or the \
             sentence lost the one thing the operator can change: {reason}"
        );
        assert!(
            !reason.contains(&needle),
            "{model}: the reason carries the path it could not open. On Windows that path has \
             the operator's user name in it, and /health goes out over a socket a LAN bind makes \
             reachable: {reason}"
        );
    }

    let anonymous = health(PORT, None).await;
    assert!(
        anonymous.get("runtime").is_none() && !anonymous.to_string().contains(&needle),
        "a caller without the token gets the verdict and nothing of the inventory: {anonymous}"
    );
}
