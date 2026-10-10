//! The UDP announcement over real sockets (autoconexión 0.29, C1 and C3): the host's real
//! responder (`beacon::respond`) on a loopback UDP port, the client's real `announced` asking
//! it, and `discover` taking what it heard as a candidate that still has to pass `/identity`.
//!
//! Nothing is broadcast. The responder listens on 127.0.0.1 and is asked there directly: what
//! the real `Network::beacon` adds on top is only which targets to ask (`broadcast_targets` of
//! the own address), and a test that sent to 255.255.255.255 would reach the developer's LAN.
//!
//! No database and no `needs-a-server` line. The host's `/identity` is a loopback listener
//! answering as the daemon does; the old address accepts and hangs up, which fails at once
//! (ci-rapido.md).

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use memory_industry::accounts::{Secret, identity_proof};
use memory_industry::beacon::{self, Reply};
use memory_industry::bridge::discovery::{Source, announced, discover};
use memory_industry::bridge::state::{BridgeState, STATE_VERSION};
use memory_industry::bridge::{Network, Timing};
use serde_json::json;
use tokio::net::UdpSocket;
use uuid::Uuid;

/// Long enough for a loopback round trip on a loaded machine, short enough for the suite.
const WAIT: Duration = Duration::from_millis(400);

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

impl Pairing {
    fn new() -> Self {
        Pairing {
            host_id: Uuid::new_v4(),
            credential_id: Uuid::new_v4(),
            secret: Secret::generate(),
        }
    }

    fn state(&self, host_port: u16, last_good: SocketAddr) -> BridgeState {
        BridgeState {
            version: STATE_VERSION,
            host_id: self.host_id,
            host_name: "madre".to_string(),
            account: "ana".to_string(),
            device_label: "pc-lab-1".to_string(),
            credential_id: self.credential_id,
            secret: Secret::from_hex(&self.secret.to_hex()).expect("a secret round-trips"),
            addresses: vec![last_good],
            last_good: Some(last_good),
            host_port,
            port: 8797,
            paired_at: Utc::now(),
        }
    }
}

/// A thread that accepts on a loopback listener and hands each connection to `serve`.
/// Dropping it wakes the accept loop and joins the thread.
struct Listener {
    addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Listener {
    fn start(serve: impl Fn(TcpStream) + Send + 'static) -> Self {
        let listener =
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback port for the listener");
        let addr = listener.local_addr().expect("its address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, flag) = (Arc::clone(&accepted), Arc::clone(&stop));
        let worker = thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                count.fetch_add(1, Ordering::SeqCst);
                if let Ok(stream) = stream {
                    serve(stream);
                }
            }
        });
        Listener {
            addr,
            accepted,
            stop,
            worker: Some(worker),
        }
    }

    /// The address a host left: something there accepts and hangs up at once.
    fn closer() -> Self {
        Self::start(drop)
    }

    /// The host's `/identity`, answered as the daemon does.
    fn host(pairing: &Pairing) -> Self {
        let (host_id, credential_id, secret_hash) = (
            pairing.host_id,
            pairing.credential_id,
            pairing.secret.hash(),
        );
        Self::start(move |stream| answer(stream, host_id, credential_id, &secret_hash))
    }
}

impl Drop for Listener {
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
    let body = identity_reply(&target, host_id, credential_id, secret_hash);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.shutdown(Shutdown::Both);
}

fn identity_reply(
    target: &str,
    host_id: Uuid,
    credential_id: Uuid,
    secret_hash: &[u8; 32],
) -> String {
    let query = target.strip_prefix("/identity?").unwrap_or_default();
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
    };
    let mut body = json!({
        "service": "memory-industry",
        "host_id": host_id.to_string(),
        "name": "madre",
        "credential": "unknown",
    });
    let nonce: Option<[u8; 16]> = param("nonce")
        .and_then(unhex)
        .and_then(|bytes| bytes.try_into().ok());
    if let (Some(cred), Some(nonce)) = (param("cred"), nonce)
        && cred == credential_id.simple().to_string()
    {
        let proof = identity_proof(secret_hash, &host_id, &credential_id, &nonce);
        body["credential"] = json!("active");
        body["proof"] = json!(hex(&proof));
    }
    body.to_string()
}

/// The host's real responder on a loopback UDP port, announcing `reply`.
async fn responder(reply: Reply) -> SocketAddr {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("a loopback UDP port for the responder");
    let at = socket.local_addr().expect("its address");
    tokio::spawn(beacon::respond(socket, reply, Instant::now));
    at
}

/// The client's view of the network with only loopback in it: no name resolves, no LAN to
/// sweep, and the announcement asked of `targets` instead of the broadcast addresses.
struct LoopbackBeacon {
    targets: Vec<SocketAddr>,
}

#[async_trait::async_trait]
impl Network for LoopbackBeacon {
    async fn resolve(&self, _name: &str) -> Vec<IpAddr> {
        Vec::new()
    }

    fn own_ipv4_towards(&self, _peer: IpAddr) -> Option<Ipv4Addr> {
        None
    }

    async fn beacon(&self, port: u16, wait: Duration) -> Vec<(SocketAddr, Uuid)> {
        announced(port, wait, &self.targets).await
    }
}

fn timing() -> Timing {
    Timing {
        resolve: Duration::from_millis(200),
        identity: Duration::from_secs(1),
        probe_total: Duration::from_millis(500),
        discovery_total: Duration::from_secs(5),
        beacon_wait: WAIT,
        ..Timing::production()
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a plain HTTP client")
}

/// C1: what the client keeps of each reply is the address it came from, with the TCP port it
/// announced, and the host id it claimed.
#[tokio::test]
async fn el_aviso_real_dice_de_donde_vino_la_respuesta_y_que_madre_dijo_ser() {
    let host_id = Uuid::new_v4();
    let udp = responder(Reply::new(host_id, "madre".to_string(), 8787)).await;

    let heard = announced(8787, WAIT, &[udp]).await;

    assert_eq!(
        heard,
        vec![(SocketAddr::from((Ipv4Addr::LOCALHOST, 8787)), host_id)],
        "the reply's source IP with the announced TCP port, not the UDP port it came from"
    );
}

/// C3 over real sockets: the host moved, the last good address only hangs up, and the
/// announcement names the new one, which then proves itself on `/identity`.
#[tokio::test]
async fn el_aviso_real_encuentra_a_la_madre_que_cambio_de_direccion() {
    let pairing = Pairing::new();
    let host = Listener::host(&pairing);
    let old = Listener::closer();
    let udp = responder(Reply::new(
        pairing.host_id,
        "madre".to_string(),
        host.addr.port(),
    ))
    .await;
    let state = pairing.state(host.addr.port(), old.addr);
    let net = LoopbackBeacon { targets: vec![udp] };

    let found = discover(&state, &net, &timing(), &client()).await;

    assert_eq!(
        found.verified,
        Some(host.addr),
        "the host the announcement named is found. Report: {}",
        found.report()
    );
    assert!(
        old.accepted.load(Ordering::SeqCst) >= 1,
        "anchor: the old address was tried first"
    );
    let last = found.attempts.last().expect("an attempt");
    assert_eq!(
        (last.candidate.addr, last.candidate.source),
        (host.addr, Source::Beacon),
        "and it was the announcement that found it. Report: {}",
        found.report()
    );
}
