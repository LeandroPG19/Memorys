#![cfg(unix)]
//! `memory-industry bridge status|run|install --print|doctor`, as an operator on a client PC runs
//! them: the real binary, with HOME pointed at a scratch directory this file creates and removes,
//! so the state file the bridge reads is one this test wrote and never the developer's own.
//!
//! No database is needed, and this file does not carry the `needs-a-server` line: the no-server
//! job of ci.yml runs it. The only host that answers is a loopback listener this file starts on
//! 127.0.0.1, answering `GET /identity` the way the daemon does. The only child that waits is
//! `bridge run`, and this file stops it once it has read its start line.
//!
//! The doctor case that finds a host never reaches the resolver or the LAN sweep: discovery tries
//! the stored address first, and a verified answer ends the search there (src/bridge/discovery.rs,
//! `Search::run`). The cases that find no host are not here, because without a pairing the bridge
//! stops before discovery, and with one they would sweep the real /24 of the machine.
//!
//! Unix only: there the state file is `$HOME/.local/share/MemoryIndustry/bridge.json`. Windows keeps
//! it under `%LOCALAPPDATA%` and is not covered by this file.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use memory_industry::accounts::{Secret, fingerprint, identity_proof};
use serde_json::json;
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

/// Where `state_dir` puts the file on unix: `<home>/.local/share/MemoryIndustry`.
const STATE_DIR: &str = ".local/share/MemoryIndustry";
const STATE_FILE: &str = "bridge.json";

/// `DEFAULT_BRIDGE_PORT` (src/bridge/mod.rs): the loopback port a state file names by default.
const DEFAULT_PORT: u16 = 8797;
/// `DEFAULT_HOST_PORT` (src/bridge/mod.rs): the port the host listens on; only the sweep reads it.
const HOST_PORT: u16 = 8787;

/// The opening words of `NOT_PAIRED` (src/bridge/mod.rs), the one line that says what to type.
const NOT_PAIRED: &str = "no está emparejado: ejecuta bridge pair";

/// `BRIDGE_TASK_NAME` (src/bridge/mod.rs): the scheduled task `bridge install` registers.
const TASK: &str = "MemoryIndustry Bridge";

/// How long a test waits for a child to say its start line. A child that has nothing to load
/// answers in well under a second; the bound makes a hung child fail the test, not the run.
const CHILD_WAIT: Duration = Duration::from_secs(20);

/// A HOME this test owns: created under the temp dir and removed when the test ends, even when
/// the test panics.
struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "memory-industry-bridge-cli-{tag}-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).expect("a scratch HOME under the temp dir");
        Home(dir)
    }

    fn state_path(&self) -> PathBuf {
        self.0.join(STATE_DIR).join(STATE_FILE)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `memory-industry bridge <args>` with HOME in the scratch directory and no database URL, so
/// nothing the bridge does can reach a server the developer runs.
fn bridge(home: &Home, args: &[&str]) -> Command {
    let mut command = Command::new(BIN);
    command
        .arg("bridge")
        .args(args)
        .env("HOME", &home.0)
        .env_remove("DATABASE_URL")
        .stdin(Stdio::null());
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
        })
        .collect()
}

/// The identity of one paired device: the host it was paired with and the credential it holds.
struct Pairing {
    host_id: Uuid,
    credential_id: Uuid,
    secret: Secret,
}

fn new_pairing() -> Pairing {
    Pairing {
        host_id: Uuid::new_v4(),
        credential_id: Uuid::new_v4(),
        secret: Secret::generate(),
    }
}

/// Writes `bridge.json` from the documented field names, not with the production writer: the
/// reader is then judged against the file format, not against the code that wrote it.
fn write_state(
    home: &Home,
    pairing: &Pairing,
    addresses: &[SocketAddr],
    last_good: Option<SocketAddr>,
    port: u16,
) {
    let path = home.state_path();
    fs::create_dir_all(path.parent().expect("a state directory")).expect("the state directory");
    let document = json!({
        "version": 1,
        "host_id": pairing.host_id.to_string(),
        "host_name": "madre",
        "account": "ana",
        "device_label": "pc-lab-1",
        "credential_id": pairing.credential_id.to_string(),
        "secret": pairing.secret.to_hex(),
        "addresses": addresses.iter().map(SocketAddr::to_string).collect::<Vec<_>>(),
        "last_good": last_good.map(|addr| addr.to_string()),
        "host_port": HOST_PORT,
        "port": port,
        "paired_at": "2026-10-08T12:00:00Z",
    });
    fs::write(&path, document.to_string()).expect("the bridge state is written");
}

/// A loopback listener that answers `GET /identity?cred=…&nonce=…` as the daemon does: its host
/// id, whether the credential in the query is this device's, and a proof over the nonce made with
/// the secret it was given. One request per connection, then the connection is closed. Dropping
/// it wakes the accept loop and joins the thread, so no listener outlives the test.
struct FakeHost {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl FakeHost {
    fn start(pairing: &Pairing) -> Self {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback port for the fake host");
        let addr = listener.local_addr().expect("the fake host's address");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let host_id = pairing.host_id;
        let credential_id = pairing.credential_id;
        let secret_hash = pairing.secret.hash();
        let worker = thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    answer(stream, host_id, credential_id, &secret_hash);
                }
            }
        });
        FakeHost {
            addr,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn answer(mut stream: TcpStream, host_id: Uuid, credential_id: Uuid, secret_hash: &[u8; 32]) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let head = read_head(&mut stream);
    let request_line = head.lines().next().unwrap_or_default();
    let target = request_line.split_whitespace().nth(1).unwrap_or_default();
    let response = match identity_reply(target, host_id, credential_id, secret_hash) {
        Some(body) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        ),
        None => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
        }
    };
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.shutdown(Shutdown::Both);
}

/// The request head up to the blank line. Bounded, so a client that never stops sending ends the
/// read instead of the test.
fn read_head(stream: &mut TcpStream) -> String {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    while seen.len() < 8192 && !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => seen.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&seen).into_owned()
}

/// The reply the daemon gives `/identity`: `active` with a proof when the credential in the query
/// is this device's, `unknown` otherwise. `None` for any other path.
fn identity_reply(
    target: &str,
    host_id: Uuid,
    credential_id: Uuid,
    secret_hash: &[u8; 32],
) -> Option<String> {
    let query = target.strip_prefix("/identity?")?;
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
    };
    let cred = param("cred")?;
    let nonce: [u8; 16] = unhex(param("nonce")?)?.try_into().ok()?;
    let body = if cred == credential_id.simple().to_string() {
        let proof = identity_proof(secret_hash, &host_id, &credential_id, &nonce);
        json!({
            "service": "memory-industry",
            "host_id": host_id.to_string(),
            "name": "madre",
            "credential": "active",
            "proof": hex(&proof),
        })
    } else {
        json!({
            "service": "memory-industry",
            "host_id": host_id.to_string(),
            "name": "madre",
            "credential": "unknown",
        })
    };
    Some(body.to_string())
}

/// A child that is killed when the test ends, whether or not the test got to stop it.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// --- status -----------------------------------------------------------------------------------

/// Kills the mutant that makes `cli_status` a silent `Ok(())`: no line means no answer.
#[test]
fn status_with_no_state_says_not_paired_and_names_the_file_it_looked_at() {
    let home = Home::new("status-unpaired");

    let out = bridge(&home, &["status"])
        .output()
        .expect("running bridge status");
    let stdout = text(&out.stdout);

    assert!(
        out.status.success(),
        "not being paired is an answer, not an error: `bridge status` exits 0 so a script can read \
         the line. stderr: {}",
        text(&out.stderr)
    );
    assert!(
        stdout.contains("not paired: no state at"),
        "the operator is told this device is not paired, in the words the guide uses to check. \
         stdout: {stdout}"
    );
    assert!(
        stdout.contains(&home.state_path().display().to_string()),
        "the operator is shown the file that is missing, so the path can be checked against the \
         one the bridge reads. stdout: {stdout}"
    );
}

/// Kills the mutant that makes `cli_status` a silent `Ok(())`, with a paired state on disk.
#[test]
fn status_with_a_pairing_prints_the_pinned_host_and_the_port_and_never_the_secret() {
    let home = Home::new("status-paired");
    let pairing = new_pairing();
    write_state(&home, &pairing, &[], None, DEFAULT_PORT);

    let out = bridge(&home, &["status"])
        .output()
        .expect("running bridge status");
    let stdout = text(&out.stdout);

    assert!(
        out.status.success(),
        "a paired device reads its state and exits 0. stderr: {}",
        text(&out.stderr)
    );
    let pinned = format!("madre (fingerprint {})", fingerprint(&pairing.host_id));
    assert!(
        stdout.contains(&pinned),
        "the operator checks the host's fingerprint against the one the host shows, so status \
         prints it. Expected `{pinned}`. stdout: {stdout}"
    );
    assert!(
        stdout.contains("local port:  8797"),
        "the port the bridge listens on is the one the client config names; status shows it so a \
         mismatch is visible. stdout: {stdout}"
    );
    assert!(
        !stdout.contains(&pairing.secret.to_hex()),
        "status is read by anyone at the desk: it must show the fingerprint and never the \
         credential secret. stdout: {stdout}"
    );
}

// --- run --------------------------------------------------------------------------------------

/// Kills the mutant that makes `cli_run` a silent `Ok(())`: an unpaired bridge must refuse.
#[test]
fn run_without_a_pairing_refuses_before_it_opens_a_port() {
    let home = Home::new("run-unpaired");

    let out = bridge(&home, &["run"])
        .output()
        .expect("running bridge run");

    assert!(
        !out.status.success(),
        "a bridge with no credential must not start: it would answer every client with an error \
         that looks like a broken daemon. stdout: {}",
        text(&out.stdout)
    );
    assert!(
        text(&out.stderr).contains(NOT_PAIRED),
        "the operator is told the one command that fixes it. stderr: {}",
        text(&out.stderr)
    );
}

/// Kills the mutant that makes `cli_run` a silent `Ok(())`: a paired `run` prints where it listens
/// and keeps serving until it is stopped.
#[test]
fn run_with_a_pairing_prints_its_loopback_address_and_keeps_serving_until_stopped() {
    let home = Home::new("run-paired");
    // Port 0: the OS picks a free one and the start line says which, so this test never races
    // another process for 8797. `port` 0 is only reachable through the file, not `--port`.
    write_state(&home, &new_pairing(), &[], None, 0);

    let log_path = home.0.join("run.stderr");
    let log = File::create(&log_path).expect("a file for the child's stderr");
    let mut command = bridge(&home, &["run"]);
    command.stdout(Stdio::piped()).stderr(Stdio::from(log));
    let mut child = command.spawn().expect("starting bridge run");
    let stdout = child.stdout.take().expect("the child's stdout is piped");
    let mut running = Running(child);

    let (lines_tx, lines_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let first = match lines_rx.recv_timeout(CHILD_WAIT) {
        Ok(line) => line,
        Err(err) => panic!(
            "bridge run did not print its start line within {CHILD_WAIT:?} ({err}). stderr: {}",
            fs::read_to_string(&log_path).unwrap_or_default()
        ),
    };

    let printed = first
        .strip_prefix("bridge listening on ")
        .unwrap_or_else(|| panic!("the start line is `bridge listening on <addr>`, got: {first}"));
    let addr: SocketAddr = printed
        .parse()
        .unwrap_or_else(|e| panic!("the start line names an address, `{printed}`: {e}"));
    assert_eq!(
        addr.ip(),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "the bridge listens on loopback only: the credential it holds must not answer the LAN"
    );
    assert_ne!(
        addr.port(),
        0,
        "the OS gave the bridge a real port to print"
    );
    TcpStream::connect_timeout(&addr, CHILD_WAIT)
        .expect("the printed address accepts a connection while the bridge runs");
    assert!(
        running.0.try_wait().expect("polling bridge run").is_none(),
        "bridge run serves until Ctrl-C: a process that had already exited would have printed its \
         start line and stopped, and nothing would be listening"
    );
    // Dropping `running` kills the child: SIGKILL stands in for Ctrl-C, which needs a signal
    // delivered from outside the test.
}

// --- install ----------------------------------------------------------------------------------

/// Kills the mutant that makes `cli_install` a silent `Ok(())`: `--print` names what it would
/// register, and runs nothing.
#[test]
fn install_print_names_the_task_it_would_register() {
    let home = Home::new("install-print");

    let out = bridge(&home, &["install", "--print"])
        .output()
        .expect("running bridge install --print");
    let stdout = text(&out.stdout);

    assert!(
        out.status.success(),
        "`--print` is the dry run the guide tells the operator to read first. stderr: {}",
        text(&out.stderr)
    );
    assert!(
        stdout.contains("schtasks") && stdout.contains("/Create"),
        "the dry run shows the task creation command the operator would run. stdout: {stdout}"
    );
    assert!(
        stdout.contains(TASK),
        "the dry run names the task, so the operator can find it in Task Scheduler later. stdout: {stdout}"
    );
}

// --- doctor -----------------------------------------------------------------------------------

/// Kills the mutant that makes `cli_doctor` a silent `Ok(())`: an unpaired device has nothing to
/// check, and doctor says so with a failing exit.
#[test]
fn doctor_without_a_pairing_refuses_and_says_how_to_pair() {
    let home = Home::new("doctor-unpaired");

    let out = bridge(&home, &["doctor"])
        .output()
        .expect("running bridge doctor");

    assert!(
        !out.status.success(),
        "doctor with nothing to check must fail, so a script can tell. stdout: {}",
        text(&out.stdout)
    );
    assert!(
        text(&out.stderr).contains(NOT_PAIRED),
        "the operator is told the one command that fixes it. stderr: {}",
        text(&out.stderr)
    );
}

/// Kills the `!=` to `==` mutant of `cli_doctor` (it would fail a verified host) and the silent
/// `Ok(())` mutant (no report would be printed). A live host proves its identity, so doctor
/// exits 0 and names it.
#[test]
fn doctor_finds_a_live_host_on_loopback_and_exits_zero() {
    let home = Home::new("doctor-live");
    let pairing = new_pairing();
    let host = FakeHost::start(&pairing);
    write_state(&home, &pairing, &[host.addr], Some(host.addr), DEFAULT_PORT);

    let out = bridge(&home, &["doctor"])
        .output()
        .expect("running bridge doctor");
    let stdout = text(&out.stdout);

    assert!(
        out.status.success(),
        "a host that proves its identity is found, and doctor exits 0 when one is found (1 means \
         none was verified). stdout: {stdout} stderr: {}",
        text(&out.stderr)
    );
    let line = format!(
        "madre: {} (huella {})",
        host.addr,
        fingerprint(&pairing.host_id)
    );
    assert!(
        stdout.contains(&line),
        "doctor names the verified host and its fingerprint, the two things the operator compares \
         with the host's screen. Expected `{line}`. stdout: {stdout}"
    );
}

// --- uninstall --------------------------------------------------------------------------------

/// Kills the mutant that makes `cli_uninstall` a silent `Ok(())`: uninstall ends the task, then
/// deletes it, and prints what each call answered. A fake `schtasks` on PATH records the calls, so
/// no scheduled task of this machine is touched.
#[test]
fn uninstall_ends_then_deletes_the_task_and_says_what_it_did() {
    let home = Home::new("uninstall");
    let fake_dir = home.0.join("fake-bin");
    fs::create_dir_all(&fake_dir).expect("a directory for the fake schtasks");
    let calls = home.0.join("schtasks.calls");
    let fake = fake_dir.join("schtasks");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexit 0\n",
            calls.display()
        ),
    )
    .expect("the fake schtasks is written");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755))
        .expect("the fake schtasks can run");
    let path = format!(
        "{}:{}",
        fake_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let out = bridge(&home, &["uninstall"])
        .env("PATH", path)
        .output()
        .expect("running bridge uninstall");
    let stdout = text(&out.stdout);

    assert!(
        out.status.success(),
        "both calls answered 0, so uninstall exits 0. stderr: {}",
        text(&out.stderr)
    );
    assert!(
        stdout.contains(r#"ok: schtasks /End /TN "MemoryIndustry Bridge""#)
            && stdout.contains(r#"ok: schtasks /Delete /TN "MemoryIndustry Bridge" /F"#),
        "each call that worked is reported as ok, in order. stdout: {stdout}"
    );
    let logged = fs::read_to_string(&calls).expect("the fake schtasks logged its calls");
    assert_eq!(
        logged.lines().collect::<Vec<_>>(),
        [
            "/End /TN MemoryIndustry Bridge",
            "/Delete /TN MemoryIndustry Bridge /F"
        ],
        "the task is stopped and then deleted, in that order: a delete of a running task is refused"
    );
}
