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
//! No model is loaded or warmed by the first two tests: the model paths point at a directory of
//! the test's own, empty. The two of CI-E31 (`stuck_warm_up`) make the reranker try a load that
//! never ends, with no byte of model: a FIFO with no writer stands where `model.onnx` would be.

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

/// The binary with `args`, the database down on purpose, the embedding models in `onnx` and the
/// reranker in `reranker`, and its three pipes. The environment is set on the child with
/// `Command::env`; the test's own is never written. `kill_on_drop` is the guard that keeps a child
/// that hangs from outliving its test: dropping the `Child` (a panic included) sends it SIGKILL.
fn server_command(args: &[&str], database_url: &str, onnx: &Path, reranker: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memory-industry"));
    command
        .args(args)
        .env("DATABASE_URL", database_url)
        .env("MEMORY_INDUSTRY_REQUIRE_DATABASE_URL", "1")
        .env("RUST_LOG", "memory_industry=info")
        .env("ONNX_MODEL_PATH", onnx)
        .env("CUBA_RERANKER_PATH", reranker)
        .env_remove("CUBA_HTTP_ADDR")
        .env_remove("CUBA_HTTP_TOKEN")
        .env_remove("CUBA_PEER_TOKEN")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

/// Runs the binary with `args`, gives it `stdin` followed by the end of file, and waits for it.
async fn run(args: &[&str], stdin: &str, database_url: &str, models: &Path) -> Ran {
    let mut child = server_command(args, database_url, models, models)
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

/// CI-E31: a reranker warm-up that never finishes must not keep the server alive after its client
/// is gone. The load is made to hang with a FIFO where `model.onnx` would be: `commit_from_file`
/// opens it and waits for bytes that never come, inside the `spawn_blocking` that the end of
/// `main` used to wait for without limit.
///
/// Unix only. The FIFO (`mkfifo`), the open that does not block (`tokio::net::unix::pipe`) and the
/// signal (`kill`) have no Windows counterpart: a named pipe there lives in `\\.\pipe\` and cannot
/// be a file of a model directory. These tests do not exist on Windows; they are not skipped there.
#[cfg(unix)]
mod stuck_warm_up {
    use std::path::PathBuf;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use tokio::net::unix::pipe::{OpenOptions, Sender};
    use tokio::process::{Child, ChildStdin, ChildStdout};
    use tokio::task::JoinHandle;

    use super::*;

    const INITIALIZE: &str = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"e31\",\"version\":\"0\"}}}\n";

    /// The pause between two tries to open the FIFO. It spaces tries; nothing is measured with it.
    const RETRY: Duration = Duration::from_millis(20);

    /// Takes its directories away when the test ends, however it ends.
    struct Dirs(Vec<PathBuf>);

    impl Drop for Dirs {
        fn drop(&mut self) {
            for dir in &self.0 {
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }

    /// A stdio server whose reranker warm-up is stuck on a FIFO, with both writing ends of the
    /// FIFO held by the test: if they were dropped, ONNX Runtime would read the end of file, fail,
    /// and the warm-up thread would leave by itself, so the test would pass without the fix.
    struct StuckServer {
        _dirs: Dirs,
        fifo: PathBuf,
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: BufReader<ChildStdout>,
        stderr: JoinHandle<Vec<u8>>,
        writers: Vec<Sender>,
    }

    fn require_onnx_runtime() {
        let named = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from);
        assert!(
            named.as_ref().is_some_and(|path| path.is_file()),
            "this test needs ONNX Runtime: ORT_DYLIB_PATH must name a library that exists (the \
             judge sets it in `gate_model_env`), got {named:?}"
        );
    }

    fn open_for_writing(fifo: &Path) -> std::io::Result<Sender> {
        OpenOptions::new().open_sender(fifo)
    }

    fn is_the_answer(line: &str) -> bool {
        serde_json::from_str::<Value>(line).is_ok_and(|answer| answer["id"] == 1)
    }

    impl StuckServer {
        /// Starts the server, waits for its answer to `initialize` (the start-up is over and the
        /// SIGTERM handler is installed) and for the warm-up to be inside the FIFO.
        async fn start() -> Self {
            require_onnx_runtime();
            let onnx = empty_models_dir();
            let dir = std::env::temp_dir().join(format!("mi-e31-{}", uuid::Uuid::new_v4()));
            let dirs = Dirs(vec![onnx.clone(), dir.clone()]);
            std::fs::create_dir_all(&dir).expect("the reranker directory is created");
            let fifo = dir.join("model.onnx");
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("`mkfifo` runs");
            assert!(made.success(), "`mkfifo {}` failed: {made}", fifo.display());

            let database = hang_up_listener();
            let mut child = server_command(&[], &database.url, &onnx, &dir)
                .env("CUBA_RERANK_INTRA_THREADS", "1")
                .env("CUBA_RERANK_DEVICE", "cpu")
                .spawn()
                .expect("binary runs");
            let mut stdin = child.stdin.take().expect("stdin is piped");
            stdin
                .write_all(INITIALIZE.as_bytes())
                .await
                .expect("the request reaches the child");
            let stdout = child.stdout.take().expect("stdout is piped");
            let mut stderr = child.stderr.take().expect("stderr is piped");
            let stderr = tokio::spawn(async move {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.ok();
                bytes
            });
            let mut server = Self {
                _dirs: dirs,
                fifo,
                child,
                stdin: Some(stdin),
                stdout: BufReader::new(stdout),
                stderr,
                writers: Vec::new(),
            };
            server.read_the_answer().await;
            server.hold_the_fifo().await;
            server
        }

        /// Kills the child, and stops the test saying what happened and what the server logged.
        async fn fail(&mut self, what: &str) -> ! {
            self.child.start_kill().ok();
            self.child.wait().await.ok();
            let stderr = (&mut self.stderr).await.unwrap_or_default();
            panic!("{what}.\nstderr: {}", String::from_utf8_lossy(&stderr));
        }

        async fn read_the_answer(&mut self) {
            let stdout = &mut self.stdout;
            let found = tokio::time::timeout(GUARD, async {
                loop {
                    let mut line = String::new();
                    match stdout.read_line(&mut line).await {
                        Ok(read) if read > 0 && is_the_answer(&line) => return true,
                        Ok(read) if read > 0 => {}
                        _ => return false,
                    }
                }
            })
            .await;
            if !found.unwrap_or(false) {
                self.fail("stdout closed or stayed silent: no answer with id 1 to initialize")
                    .await;
            }
        }

        /// Opens the FIFO for writing without blocking, twice. That open fails with `ENXIO` unless
        /// somebody has the FIFO open for reading, so the first one proves the server opened it
        /// (ONNX Runtime is loading the model) and the second, made right before the test acts,
        /// proves it is still holding it: the warm-up is inside its `spawn_blocking`.
        async fn hold_the_fifo(&mut self) {
            let fifo = self.fifo.clone();
            let child = &mut self.child;
            let mut refused = String::from("no attempt was made");
            let first = tokio::time::timeout(GUARD, async {
                loop {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Err(format!(
                            "the server ended ({status}) before opening the FIFO"
                        ));
                    }
                    match open_for_writing(&fifo) {
                        Ok(sender) => return Ok(sender),
                        Err(why) => refused = why.to_string(),
                    }
                    tokio::time::sleep(RETRY).await;
                }
            })
            .await;
            match first {
                Ok(Ok(sender)) => self.writers.push(sender),
                Ok(Err(ended)) => self.fail(&ended).await,
                Err(_) => {
                    self.fail(&format!(
                        "the server never opened the FIFO that stands for the reranker model: \
                         ONNX Runtime did not load, or the reranker was not tried (the last error \
                         of the open was «{refused}»)"
                    ))
                    .await
                }
            }
            match open_for_writing(&self.fifo) {
                Ok(second) => self.writers.push(second),
                Err(why) => {
                    self.fail(&format!(
                        "the server let go of the FIFO before the test acted: a second open for \
                         writing failed with «{why}», so ONNX Runtime did not stay reading it and \
                         the warm-up is not stuck"
                    ))
                    .await
                }
            }
        }

        async fn terminate(&mut self) {
            let pid = self.child.id().expect("the child is still running");
            let status = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status()
                .await
                .expect("`kill` runs");
            assert!(status.success(), "`kill -TERM {pid}` failed: {status}");
        }

        /// Waits for the child to end and returns what it said. It is given `GUARD`, which
        /// protects against «never» and is not a verdict on how long the exit takes.
        async fn ends_by_itself(&mut self) -> Ran {
            let waited = tokio::time::timeout(GUARD, self.child.wait()).await;
            let status = match waited {
                Ok(Ok(status)) => status,
                Ok(Err(why)) => panic!("the child can be waited for: {why}"),
                Err(_) => {
                    self.fail("the binary did not end by itself within the guard")
                        .await
                }
            };
            let mut stdout = String::new();
            self.stdout.read_to_string(&mut stdout).await.ok();
            let stderr = (&mut self.stderr).await.unwrap_or_default();
            Ran {
                code: status.code(),
                stdout,
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            }
        }
    }

    #[tokio::test]
    async fn with_the_reranker_warm_up_stuck_the_end_of_stdin_ends_the_server_with_0() {
        let mut server = StuckServer::start().await;
        server.stdin.take();

        let ran = server.ends_by_itself().await;

        assert_eq!(
            ran.code,
            Some(0),
            "closing stdin while the warm-up is stuck must end the server with 0 (None: a signal \
             killed it).\nstderr: {}",
            ran.stderr
        );
        assert!(
            ran.stderr.contains("stdin closed — draining"),
            "the server did not log that stdin closed and it was draining.\nstderr: {}",
            ran.stderr
        );
    }

    #[tokio::test]
    async fn with_the_reranker_warm_up_stuck_a_sigterm_ends_the_server_with_0() {
        let mut server = StuckServer::start().await;

        server.terminate().await;
        let ran = server.ends_by_itself().await;

        assert_eq!(
            ran.code,
            Some(0),
            "a SIGTERM with stdin still open and the warm-up stuck must end the server with 0 \
             (None: the signal killed it, so its handler was not there).\nstderr: {}",
            ran.stderr
        );
        assert!(
            ran.stderr.contains("SIGTERM received"),
            "the server did not log that it received SIGTERM.\nstderr: {}",
            ran.stderr
        );
    }
}
