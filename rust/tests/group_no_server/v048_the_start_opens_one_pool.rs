//! The start-up opens one pool (CI-E17). The stdio start used to open two: `main` opened a pool
//! only to check the width of the embedding columns and `run_mcp` opened another to serve, so every
//! start connected, migrated, wrote `cuba_app`'s password and stepped down twice, and with the
//! database down it tried twice and said so in two different warnings.
//!
//! What is observed is a count, never a clock: the connections a listener of the test's own
//! accepted, and the lines of the log. The listener accepts and hangs up, so a refused attempt
//! fails at once and for the right reason (`dead_db` says why not a port with nobody on it). It is
//! not `dead_db`'s: that one is shared by every test of the binary at the same time, and its count
//! would mix the processes. This one adds to its count *before* it drops the connection, and the
//! child only goes on after it reads the end of file that the drop causes, so the count is
//! complete when the child exits.
//!
//! No model is loaded or warmed: the model paths point at a directory of the test's own, empty.

use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// The wait for a child that has to end by itself. It guards against a hang; it is not a verdict.
const GUARD: Duration = Duration::from_secs(60);

const READY: &str = "MCP protocol ready";
const WITHOUT_DATABASE: &str =
    "starting without PostgreSQL — tools will fail until it is reachable";
const CONNECTED: &str = "connected to PostgreSQL";

/// A database that is down on purpose, and how many times something tried to reach it.
struct DownDatabase {
    url: String,
    attempts: Arc<AtomicUsize>,
}

impl DownDatabase {
    fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
}

fn hang_up_listener() -> DownDatabase {
    let listener = TcpListener::bind("127.0.0.1:0")
        .expect("the down database needs a free port on 127.0.0.1 to listen on");
    let port = listener
        .local_addr()
        .expect("the down database's listener has no local address")
        .port();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    DownDatabase {
        url: format!("postgresql://cuba:e17@127.0.0.1:{port}/nowhere"),
        attempts,
    }
}

struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the binary with `args`, gives it `stdin` followed by the end of file, and waits for it.
/// The environment is set on the child with `Command::env`; the test's own is never written.
async fn run(args: &[&str], stdin: &str, database_url: &str, models: &Path) -> Ran {
    let mut child = Command::new(env!("CARGO_BIN_EXE_memory-industry"))
        .args(args)
        .env("DATABASE_URL", database_url)
        .env("MEMORY_INDUSTRY_REQUIRE_DATABASE_URL", "1")
        .env("RUST_LOG", "memory_industry=info")
        .env("ONNX_MODEL_PATH", models)
        .env("CUBA_RERANKER_PATH", models)
        .env_remove("CUBA_HTTP_ADDR")
        .env_remove("CUBA_HTTP_TOKEN")
        .env_remove("CUBA_PEER_TOKEN")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("binary runs");
    let mut pipe = child.stdin.take().expect("stdin is piped");
    pipe.write_all(stdin.as_bytes())
        .await
        .expect("the request reaches the child");
    drop(pipe);
    let output = tokio::time::timeout(GUARD, child.wait_with_output())
        .await
        .expect("the binary did not end by itself within the guard")
        .expect("the child can be waited for");
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn empty_models_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mi-v048-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("the empty model directory is created");
    dir
}

/// The JSON lines of stderr that carry a `fields.message`: (the line, its message).
fn log_lines(stderr: &str) -> Vec<(Value, String)> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|event| {
            let message = event["fields"]["message"].as_str()?.to_string();
            Some((event, message))
        })
        .collect()
}

/// What the process logged before it said it was ready.
fn before_ready(stderr: &str) -> Vec<(Value, String)> {
    let lines = log_lines(stderr);
    let ready = lines
        .iter()
        .position(|(_, message)| message.starts_with(READY))
        .unwrap_or_else(|| panic!("stderr has no «{READY}» line.\nstderr: {stderr}"));
    lines.into_iter().take(ready).collect()
}

#[tokio::test]
async fn with_the_database_down_the_stdio_start_tries_once_and_says_so_once() {
    let database = hang_up_listener();
    let models = empty_models_dir();
    let request = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n";

    let ran = run(&[], request, &database.url, &models).await;
    std::fs::remove_dir_all(&models).ok();

    assert_eq!(
        ran.code,
        Some(0),
        "a start without a database serves with a lazy pool and exits 0 at the end of file.\nstderr: {}",
        ran.stderr
    );
    let answer: Value = serde_json::from_str(ran.stdout.trim())
        .unwrap_or_else(|why| panic!("stdout is not one JSON answer ({why}): {}", ran.stdout));
    let tools = answer["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("the answer has no result.tools: {answer}"));
    assert!(!tools.is_empty(), "tools/list answered an empty list");

    let attempts = database.attempts();
    assert_eq!(
        attempts, 1,
        "the listener counted {attempts} connection attempts, expected 1: the start opens one pool \
         and, with the database down, tries it once.\nstderr: {}",
        ran.stderr
    );

    let early = before_ready(&ran.stderr);
    let with_error: Vec<&str> = early
        .iter()
        .filter(|(event, _)| event["fields"].get("error").is_some())
        .map(|(_, message)| message.as_str())
        .collect();
    assert_eq!(
        with_error.len(),
        1,
        "exactly one line with an `error` field before «{READY}», got {with_error:?}"
    );
    assert!(
        with_error[0].starts_with(WITHOUT_DATABASE),
        "the one warning before «{READY}» must be «{WITHOUT_DATABASE}», got «{}»",
        with_error[0]
    );
    assert!(
        log_lines(&ran.stderr)
            .iter()
            .all(|(_, message)| !message.starts_with(CONNECTED)),
        "the database is down and the log says «{CONNECTED}».\nstderr: {}",
        ran.stderr
    );
}

#[tokio::test]
async fn with_the_database_down_the_daemon_says_so_once() {
    let database = hang_up_listener();
    let models = empty_models_dir();
    let taken = TcpListener::bind("127.0.0.1:0").expect("a port to hold so the bind fails");
    let port = taken
        .local_addr()
        .expect("the held port has an address")
        .port();
    let address = format!("127.0.0.1:{port}");

    let ran = run(&["serve", address.as_str()], "", &database.url, &models).await;
    drop(taken);
    std::fs::remove_dir_all(&models).ok();

    assert!(
        ran.code.is_some_and(|code| code != 0),
        "`serve {address}` on a port that is taken must fail to bind and exit non-zero, exit {:?}.\nstdout: {}\nstderr: {}",
        ran.code,
        ran.stdout,
        ran.stderr
    );
    let lines = log_lines(&ran.stderr);
    let without_database = lines
        .iter()
        .filter(|(_, message)| message.starts_with(WITHOUT_DATABASE))
        .count();
    assert_eq!(
        without_database, 1,
        "the daemon says «{WITHOUT_DATABASE}» once, got {without_database}.\nstderr: {}",
        ran.stderr
    );
    for unwanted in [CONNECTED, READY] {
        assert!(
            lines
                .iter()
                .all(|(_, message)| !message.starts_with(unwanted)),
            "the daemon never got to «{unwanted}» and the log says it did.\nstderr: {}",
            ran.stderr
        );
    }
}
