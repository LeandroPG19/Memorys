//! The UDP announcement: how a client on the same LAN learns where the host is without
//! knowing its address. Both sides of the protocol live here, the host's responder and the
//! client's `probe`, so the shape of a request and of a reply is written once.
//!
//! An answer is only ever a candidate. Whoever reads it still has to make the host prove
//! itself on `/identity` before a credential goes anywhere: a reply is cheap to forge.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::UdpSocket;
use uuid::Uuid;

const SERVICE: &str = "memory-industry";
const VERSION: u32 = 1;

/// The request is padded to at least this, so an answer is never many times the size of the
/// question that caused it: a forged source address buys an attacker no amplification.
const MIN_REQUEST: usize = 64;
const MAX_REQUEST: usize = 512;

/// What `probe` sends: inside the bounds, and about the size of a reply.
const PROBE_REQUEST: usize = 256;

/// Bigger than `MAX_REQUEST` on purpose: reading into exactly 512 bytes truncates a longer
/// datagram to 512, and a truncated one whose first 512 bytes parse would be answered.
const READ_BUFFER: usize = 2048;

const PER_ORIGIN: usize = 4;
const PER_ORIGIN_WINDOW: Duration = Duration::from_secs(10);
const GLOBAL: usize = 50;
const GLOBAL_WINDOW: Duration = Duration::from_secs(1);

/// Past this many tracked origins the expired ones are swept. The global ceiling keeps the
/// live ones near 500 (50 a second for a 10 s window), so the sweep is rare.
const SWEEP_AT: usize = 1024;

/// What the host says about itself: the same `host_id` and name `/identity` gives, and the
/// TCP port its daemon serves on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub service: String,
    pub v: u32,
    pub host_id: Uuid,
    pub host_name: String,
    pub port: u16,
}

impl Reply {
    pub fn new(host_id: Uuid, host_name: String, port: u16) -> Self {
        Self {
            service: SERVICE.to_string(),
            v: VERSION,
            host_id,
            host_name,
            port,
        }
    }
}

/// RFC 1918 or loopback. Everything else is either not the LAN or a forged source.
fn may_ask(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

fn is_request(packet: &[u8]) -> bool {
    if !(MIN_REQUEST..=MAX_REQUEST).contains(&packet.len()) {
        return false;
    }
    let Ok(asked) = serde_json::from_slice::<Value>(packet) else {
        return false;
    };
    asked.get("q").and_then(Value::as_str) == Some(SERVICE)
        && asked.get("v").and_then(Value::as_u64) == Some(u64::from(VERSION))
}

/// The reply `packet` deserves when it came from `from`, or `None` when it deserves none:
/// a request of the wrong shape or size, or an origin outside the private ranges and loopback.
///
/// The decision without the socket, because an origin like 8.8.8.8 cannot be produced on a
/// loopback test. A nil `host_id` is the database not read yet: no client could verify that
/// host, so it is not announced.
pub fn answer(packet: &[u8], from: SocketAddr, me: &Reply) -> Option<Vec<u8>> {
    if me.host_id.is_nil() || !may_ask(from.ip()) || !is_request(packet) {
        return None;
    }
    serde_json::to_vec(me).ok()
}

/// How many answers each origin and the whole host may get, so the responder is no amplifier.
/// A sliding window of answer times; only answers count, so junk spends nobody's budget.
#[derive(Debug, Default)]
pub struct Limiter {
    per_origin: HashMap<IpAddr, VecDeque<Instant>>,
    global: VecDeque<Instant>,
}

fn forget_before(times: &mut VecDeque<Instant>, now: Instant, window: Duration) {
    while times
        .front()
        .is_some_and(|first| now.saturating_duration_since(*first) >= window)
    {
        times.pop_front();
    }
}

impl Limiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether one more answer to `origin` fits at `now`; when it does, it is counted.
    pub fn allow(&mut self, origin: IpAddr, now: Instant) -> bool {
        forget_before(&mut self.global, now, GLOBAL_WINDOW);
        if self.global.len() >= GLOBAL {
            return false;
        }
        let mine = self.per_origin.entry(origin).or_default();
        forget_before(mine, now, PER_ORIGIN_WINDOW);
        if mine.len() >= PER_ORIGIN {
            return false;
        }
        mine.push_back(now);
        self.global.push_back(now);
        if self.per_origin.len() > SWEEP_AT {
            self.per_origin.retain(|_, times| {
                forget_before(times, now, PER_ORIGIN_WINDOW);
                !times.is_empty()
            });
        }
        true
    }
}

/// A receive error that says something about one peer and nothing about the socket: on
/// Windows the ICMP «port unreachable» for an earlier send comes back as a reset on the next
/// receive, and an oversized datagram as WSAEMSGSIZE (10040).
fn about_one_peer(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused
    ) || (cfg!(windows) && e.raw_os_error() == Some(10040))
}

/// The responder's socket: every interface, because a socket bound to one IP is not
/// guaranteed to receive broadcasts (on Linux it does not). `answer` does the filtering.
pub async fn bind(port: u16) -> std::io::Result<UdpSocket> {
    UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).await
}

/// Answers every acceptable request on `socket`, one datagram at a time and in arrival
/// order, until the socket fails. `clock` is injected so a test freezes the rate window.
pub async fn respond<C>(socket: UdpSocket, me: Reply, mut clock: C) -> std::io::Result<()>
where
    C: FnMut() -> Instant + Send,
{
    let mut limiter = Limiter::new();
    let mut buf = vec![0u8; READ_BUFFER];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(got) => got,
            Err(e) if about_one_peer(&e) => continue,
            Err(e) => return Err(e),
        };
        let Some(reply) = answer(&buf[..n], from, &me) else {
            continue;
        };
        if limiter.allow(from.ip(), clock()) {
            // A send that fails is one peer gone; the next request is another.
            let _ = socket.send_to(&reply, from).await;
        }
    }
}

fn request() -> Vec<u8> {
    let bare = format!(r#"{{"q":"{SERVICE}","v":{VERSION},"pad":""}}"#);
    let pad = "x".repeat(PROBE_REQUEST.saturating_sub(bare.len()));
    format!(r#"{{"q":"{SERVICE}","v":{VERSION},"pad":"{pad}"}}"#).into_bytes()
}

/// A reply worth handing on: this protocol, a host that has an id, and the port that was
/// asked about. Another service answering on the same number is not the host.
fn valid_reply(packet: &[u8], port: u16) -> Option<Reply> {
    let reply: Reply = serde_json::from_slice(packet).ok()?;
    (reply.service == SERVICE
        && reply.v == VERSION
        && reply.port == port
        && !reply.host_id.is_nil())
    .then_some(reply)
}

/// Sends one request to each of `targets` and collects the valid replies that arrive within
/// `wait`, each with the address it came from. A target that cannot be sent to, or that
/// answers with an error, does not end the probe: the others may still answer.
pub async fn probe(port: u16, wait: Duration, targets: &[SocketAddr]) -> Vec<(SocketAddr, Reply)> {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
        return Vec::new();
    };
    // Without it a send to a broadcast address fails with «permission denied».
    let _ = socket.set_broadcast(true);
    let ask = request();
    for target in targets {
        let _ = socket.send_to(&ask, target).await;
    }

    let deadline = tokio::time::Instant::now() + wait;
    let mut found: Vec<(SocketAddr, Reply)> = Vec::new();
    let mut buf = vec![0u8; READ_BUFFER];
    loop {
        let (n, from) = match tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
            Err(_) => break,
            Ok(Err(_)) => continue,
            Ok(Ok(got)) => got,
        };
        if found.iter().any(|(seen, _)| *seen == from) {
            continue;
        }
        if let Some(reply) = valid_reply(&buf[..n], port) {
            found.push((from, reply));
        }
    }
    found
}

/// The limited broadcast and the directed broadcast of the own /24, on `port`.
pub fn broadcast_targets(own: Ipv4Addr, port: u16) -> Vec<SocketAddr> {
    let [a, b, c, _] = own.octets();
    vec![
        SocketAddr::from((Ipv4Addr::BROADCAST, port)),
        SocketAddr::from((Ipv4Addr::new(a, b, c, 255), port)),
    ]
}
