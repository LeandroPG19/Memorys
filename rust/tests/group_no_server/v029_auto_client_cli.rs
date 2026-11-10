#![cfg(unix)]
//! `memory-industry bridge reconnect [--host X]` and `bridge status` as the client's notice
//! window runs them (autoconexión 0.29, B5 and B6): the real binary, with HOME in a scratch
//! directory this file creates and removes, so the state file is one this test wrote.
//!
//! No database and no `needs-a-server` line. The only host is a loopback listener this file
//! starts, answering `GET /identity` the way the daemon does. No case here lets the binary reach
//! the resolver or the LAN sweep: each one gives it an address that verifies first (`--host`, or
//! the last good one), and a verified answer ends the search there.
//!
//! "No bridge running" is a state whose loopback port refuses connections: the port of a listener
//! this test bound and closed. A refused connect answers at once (ci-rapido.md).
//!
//! Unix only, like v029_bridge_cli.rs: the state file is `$HOME/.local/share/MemoryIndustry/bridge.json`.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use memory_industry::accounts::{Secret, identity_proof};
use serde_json::{Value, json};
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

const STATE_DIR: &str = ".local/share/MemoryIndustry";
const STATE_FILE: &str = "bridge.json";
const HOST_PORT: u16 = 8787;

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "memory-industry-auto-client-{tag}-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&dir).expect("a scratch HOME under the temp dir");
        Home(dir)
    }

    fn state_path(&self) -> PathBuf {
        self.0.join(STATE_DIR).join(STATE_FILE)
    }

    fn stored(&self) -> Value {
        let raw = fs::read(self.state_path()).expect("the state file is there");
        serde_json::from_slice(&raw).expect("the state file is JSON")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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

/// A loopback port nothing listens on: bound, read, closed. A connect to it is refused at once.
fn refused_port() -> u16 {
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free loopback port");
    held.local_addr().expect("its address").port()
}

/// `bridge.json` from the documented field names, not with the production writer.
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

/// Answers `GET /identity?cred=…&nonce=…` as the daemon does, one request per connection.
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
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    while seen.len() < 8192 && !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => seen.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&seen).into_owned();
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let response = match identity_reply(&target, host_id, credential_id, secret_hash) {
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
    let mut body = json!({
        "service": "memory-industry",
        "host_id": host_id.to_string(),
        "name": "madre",
        "credential": "unknown",
    });
    if cred == credential_id.simple().to_string() {
        let proof = identity_proof(secret_hash, &host_id, &credential_id, &nonce);
        body["credential"] = json!("active");
        body["proof"] = json!(hex(&proof));
    }
    Some(body.to_string())
}

/// B5, the button «Escribir nombre o IP de la madre…» with no bridge running: the binary looks
/// once itself, the typed address proves to be the host, it is saved, and the exit says so.
#[test]
fn reconnect_con_host_y_sin_puente_en_marcha_verifica_guarda_y_sale_0() {
    let home = Home::new("reconnect-host");
    let pairing = new_pairing();
    let host = FakeHost::start(&pairing);
    write_state(&home, &pairing, &[], None, refused_port());

    let out = bridge(&home, &["reconnect", "--host", &host.addr.to_string()])
        .output()
        .expect("running bridge reconnect");
    let stdout = text(&out.stdout);

    assert_eq!(
        out.status.code(),
        Some(0),
        "a host that proved itself is a success: exit 0. stdout: {stdout} stderr: {}",
        text(&out.stderr)
    );
    assert!(
        stdout.to_lowercase().contains("conectad")
            && stdout.contains("madre")
            && stdout.contains(&host.addr.to_string()),
        "the person reads, in Spanish, that it connected, to which host and where. stdout: {stdout}"
    );
    let stored = home.stored();
    assert_eq!(
        stored["last_good"],
        host.addr.to_string(),
        "the typed address is kept as the last good one, so the next start finds it first"
    );
    assert!(
        stored["addresses"]
            .as_array()
            .is_some_and(|all| all.iter().any(|a| a == &json!(host.addr.to_string()))),
        "and among the addresses: {stored}"
    );
    assert_eq!(
        stored["secret"],
        pairing.secret.to_hex(),
        "saving the address keeps the credential"
    );
}

/// B5, the button «Reintentar ahora» with no bridge running: one search from what is stored.
#[test]
fn reconnect_sin_host_y_sin_puente_en_marcha_encuentra_la_ultima_buena_y_sale_0() {
    let home = Home::new("reconnect-plain");
    let pairing = new_pairing();
    let host = FakeHost::start(&pairing);
    write_state(
        &home,
        &pairing,
        &[host.addr],
        Some(host.addr),
        refused_port(),
    );

    let out = bridge(&home, &["reconnect"])
        .output()
        .expect("running bridge reconnect");
    let stdout = text(&out.stdout);

    assert_eq!(
        out.status.code(),
        Some(0),
        "found at the last good address: exit 0. stdout: {stdout} stderr: {}",
        text(&out.stderr)
    );
    assert!(
        stdout.contains("madre") && stdout.contains(&host.addr.to_string()),
        "it names the host and the address. stdout: {stdout}"
    );
}

/// B6: status says whether a bridge is serving this pairing, besides the host it trusts.
#[test]
fn status_dice_que_no_hay_puente_en_marcha_cuando_su_puerto_no_contesta() {
    let home = Home::new("status-not-running");
    let pairing = new_pairing();
    let port = refused_port();
    write_state(&home, &pairing, &[], None, port);

    let out = bridge(&home, &["status"])
        .output()
        .expect("running bridge status");
    let stdout = text(&out.stdout);

    assert!(
        out.status.success(),
        "status is an answer, not an error. stderr: {}",
        text(&out.stderr)
    );
    assert!(
        stdout.contains(&format!("not running on 127.0.0.1:{port}")),
        "the person sees that nothing serves the bridge's port, and which port it is: that is \
         why Claude Code gets no answer. stdout: {stdout}"
    );
}
