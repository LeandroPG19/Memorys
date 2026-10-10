//! The UDP announcement (0.29, slice A5): the host answers a well-formed request from the
//! private LAN with who it is, answers nothing else, and is no amplifier.
//!
//! Every socket here is on loopback and nothing is broadcast: a broadcast from a test would
//! reach a real host on the plant LAN, and its answer would turn an exact count into a flake.
//! An origin like 8.8.8.8 cannot be produced on loopback, so the origin filter is asked of
//! `beacon::answer` directly and the sockets prove the rest.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use memory_industry::beacon::{self, Limiter, Reply};
use serde_json::Value;
use tokio::net::UdpSocket;
use uuid::Uuid;

/// Long enough for a datagram on loopback on a machine running the whole gate; it is a
/// ceiling on a wait for something that normally takes microseconds.
const PROBE_WAIT: Duration = Duration::from_millis(500);

/// A guard on waits that end as soon as the answer arrives. Reaching it is the failure.
const GUARD: Duration = Duration::from_secs(5);

const HOST_ID: &str = "6f1c1e2a-0d3b-4c5e-9f7a-1b2c3d4e5f60";

fn me(port: u16) -> Reply {
    Reply {
        service: "memory-industry".to_string(),
        v: 1,
        host_id: Uuid::parse_str(HOST_ID).unwrap(),
        host_name: "madre-de-prueba".to_string(),
        port,
    }
}

/// A request in the shape the specification fixes, padded to exactly `len` bytes.
fn request_with(q: &str, v: u32, len: usize) -> Vec<u8> {
    let bare = format!(r#"{{"q":"{q}","v":{v},"pad":""}}"#);
    assert!(
        len >= bare.len(),
        "a request of {len} bytes cannot hold {bare}"
    );
    let pad = "x".repeat(len - bare.len());
    let out = format!(r#"{{"q":"{q}","v":{v},"pad":"{pad}"}}"#).into_bytes();
    assert_eq!(out.len(), len);
    out
}

fn request(len: usize) -> Vec<u8> {
    request_with("memory-industry", 1, len)
}

fn from(ip: &str) -> SocketAddr {
    SocketAddr::new(ip.parse().unwrap(), 51000)
}

fn ip(raw: &str) -> IpAddr {
    raw.parse().unwrap()
}

// --- the decision: who gets an answer, and what it says ---------------------

#[test]
fn a_valid_request_from_the_private_lan_is_answered_with_who_the_host_is() {
    let host = me(8787);

    for origin in [
        "192.168.1.20",
        "10.0.0.5",
        "172.16.0.1",
        "172.31.255.254",
        "127.0.0.1",
    ] {
        let raw = beacon::answer(&request(64), from(origin), &host).unwrap_or_else(|| {
            panic!("a well-formed request from {origin} is the LAN asking for the host, and got nothing")
        });
        let reply: Value = serde_json::from_slice(&raw)
            .unwrap_or_else(|e| panic!("the answer to {origin} is not JSON ({e}): {raw:?}"));
        let mut keys: Vec<&str> = reply
            .as_object()
            .expect("the answer is a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["host_id", "host_name", "port", "service", "v"],
            "the answer carries exactly what /identity says plus the TCP port, nothing else: {reply}"
        );
        assert_eq!(reply["service"], "memory-industry");
        assert_eq!(reply["v"], 1);
        assert_eq!(reply["host_id"], HOST_ID);
        assert_eq!(reply["host_name"], "madre-de-prueba");
        assert_eq!(reply["port"], 8787);
    }
}

#[test]
fn an_origin_outside_the_private_ranges_and_loopback_gets_nothing() {
    let host = me(8787);
    assert!(
        beacon::answer(&request(64), from("192.168.1.20"), &host).is_some(),
        "anchor: the same request from the private LAN is answered, so a None below is the origin"
    );

    for origin in [
        "8.8.8.8",
        "172.32.0.1",
        "172.15.255.255",
        "192.169.0.1",
        "100.64.0.1",
        "169.254.10.10",
        "0.0.0.0",
        "255.255.255.255",
    ] {
        assert_eq!(
            beacon::answer(&request(64), from(origin), &host),
            None,
            "{origin} is not RFC 1918 nor loopback: answering it turns the host into a reflector \
             for anyone who can spoof a source address"
        );
    }
}

#[test]
fn a_request_of_the_wrong_shape_or_size_gets_nothing() {
    let host = me(8787);
    let lan = from("192.168.1.20");
    for len in [64, 512] {
        assert!(
            beacon::answer(&request(len), lan, &host).is_some(),
            "anchor: a request of {len} bytes is inside 64..=512 and is answered"
        );
    }

    // Valid JSON with trailing blanks: a responder that reads into a 512-byte buffer sees a
    // clean object and answers a datagram past the limit.
    let mut oversized_but_parses = request(100);
    oversized_but_parses.resize(600, b' ');

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("63 bytes, one short of the floor", request(63)),
        ("513 bytes, one past the ceiling", request(513)),
        ("600 bytes whose first 512 parse", oversized_but_parses),
        ("another service", request_with("otro-servicio", 1, 80)),
        (
            "another protocol version",
            request_with("memory-industry", 2, 80),
        ),
        ("not JSON", vec![b'z'; 80]),
        ("a JSON array", format!("[{}]", " ".repeat(78)).into_bytes()),
        ("nothing", Vec::new()),
    ];
    for (what, packet) in cases {
        assert_eq!(
            beacon::answer(&packet, lan, &host),
            None,
            "{what}: a malformed request must be ignored, not answered"
        );
    }
}

// --- the limit ---------------------------------------------------------------

#[test]
fn one_origin_gets_at_most_four_answers_every_ten_seconds() {
    let mut limiter = Limiter::new();
    let t0 = Instant::now();
    let asker = ip("192.168.1.20");

    // Twenty requests spread over one second, the Gherkin's «20 veces en 1 s».
    let allowed = (0..20u64)
        .filter(|i| limiter.allow(asker, t0 + Duration::from_millis(i * 50)))
        .count();
    assert_eq!(
        allowed, 4,
        "one origin asking 20 times in a second gets the first four answers and no more"
    );

    assert!(
        limiter.allow(ip("192.168.1.21"), t0 + Duration::from_secs(1)),
        "the limit is per origin: a neighbour that never asked is still answered"
    );
    assert!(
        !limiter.allow(asker, t0 + Duration::from_secs(9)),
        "nine seconds later the ten-second window of the first answers is still open"
    );
    assert!(
        limiter.allow(asker, t0 + Duration::from_secs(12)),
        "once the window has passed the same origin is answered again: a limit, not a ban"
    );
}

#[test]
fn the_whole_host_answers_at_most_fifty_times_a_second() {
    let mut limiter = Limiter::new();
    let t0 = Instant::now();

    let allowed = (1..=60u8)
        .filter(|last| limiter.allow(IpAddr::V4(Ipv4Addr::new(10, 0, 0, *last)), t0))
        .count();
    assert_eq!(
        allowed, 50,
        "sixty different origins in the same instant: the global ceiling is fifty a second"
    );
    assert!(
        limiter.allow(
            IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)),
            t0 + Duration::from_millis(1100)
        ),
        "a second later the global budget is back"
    );
}

#[test]
fn the_broadcast_targets_are_the_limited_one_and_the_own_slash_24() {
    assert_eq!(
        beacon::broadcast_targets(Ipv4Addr::new(192, 168, 1, 37), 8787),
        vec![
            "255.255.255.255:8787".parse::<SocketAddr>().unwrap(),
            "192.168.1.255:8787".parse().unwrap(),
        ]
    );
    assert_eq!(
        beacon::broadcast_targets(Ipv4Addr::new(10, 0, 5, 7), 9000),
        vec![
            "255.255.255.255:9000".parse::<SocketAddr>().unwrap(),
            "10.0.5.255:9000".parse().unwrap(),
        ]
    );
}

// --- the responder on a real socket -----------------------------------------

/// A responder on an ephemeral loopback port whose clock never moves, so the rate window
/// cannot open in the middle of a test on a slow machine.
async fn responder(
    reply_port: Option<u16>,
) -> (SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let frozen = Instant::now();
    let task = tokio::spawn(beacon::respond(
        socket,
        me(reply_port.unwrap_or(addr.port())),
        move || frozen,
    ));
    (addr, task)
}

/// One answer on `socket`, or a failure naming why none came.
async fn one_answer(
    socket: &UdpSocket,
    task: &mut tokio::task::JoinHandle<std::io::Result<()>>,
    why: &str,
) -> Vec<u8> {
    let mut buf = vec![0u8; 2048];
    tokio::select! {
        got = socket.recv_from(&mut buf) => {
            let (n, _) = got.expect("a datagram on loopback");
            buf.truncate(n);
            buf
        }
        ended = task => panic!("the responder stopped instead of answering: {ended:?}"),
        _ = tokio::time::sleep(GUARD) => panic!("{why}"),
    }
}

/// Every datagram already waiting on `socket`, without waiting for more.
fn drain(socket: &UdpSocket) -> Vec<Vec<u8>> {
    let mut got = Vec::new();
    let mut buf = vec![0u8; 2048];
    while let Ok((n, _)) = socket.try_recv_from(&mut buf) {
        got.push(buf[..n].to_vec());
    }
    got
}

/// Waits for `count` answers on `socket`, or fails naming why they did not come.
async fn answers(
    socket: &UdpSocket,
    task: &mut tokio::task::JoinHandle<std::io::Result<()>>,
    count: usize,
    why: &str,
) -> Vec<Vec<u8>> {
    let mut got = Vec::new();
    while got.len() < count {
        got.push(one_answer(socket, task, why).await);
    }
    got
}

/// How long a test listens for an answer that must not come, once the expected ones arrived.
const QUIET: Duration = Duration::from_millis(300);

#[tokio::test]
async fn one_origin_asking_twenty_times_in_a_second_gets_at_most_four_answers() {
    let (host, mut task) = responder(None).await;
    let asker = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    for _ in 0..20 {
        asker.send_to(&request(64), host).await.unwrap();
    }
    answers(
        &asker,
        &mut task,
        4,
        "one origin asking 20 times got fewer than four answers",
    )
    .await;

    // A barrier from a second origin proves nothing here: measured on this kernel, loopback
    // does not keep the order between different sockets. So the fifth answer gets a quiet window.
    let mut buf = vec![0u8; 2048];
    let fifth = tokio::time::timeout(QUIET, asker.recv_from(&mut buf)).await;
    assert!(
        fifth.is_err(),
        "twenty requests from one origin got a fifth answer; the limit is four every ten \
         seconds, and without it the host is an amplifier"
    );
}

/// The barrier is a second good request from the same socket: one flow keeps its order on
/// loopback, so when its answer is in, every earlier answer the asker will get is in too.
#[tokio::test]
async fn the_responder_skips_what_it_must_not_answer_and_answers_the_rest() {
    let (host, mut task) = responder(None).await;
    let asker = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    let mut oversized_but_parses = request(100);
    oversized_but_parses.resize(600, b' ');
    for junk in [
        request(63),
        vec![b'z'; 80],
        oversized_but_parses,
        request_with("otro-servicio", 1, 80),
    ] {
        asker.send_to(&junk, host).await.unwrap();
    }
    asker.send_to(&request(64), host).await.unwrap();
    asker.send_to(&request(64), host).await.unwrap();

    let got = answers(
        &asker,
        &mut task,
        2,
        "two good requests after four malformed ones did not both get an answer",
    )
    .await;
    for raw in &got {
        let reply: Reply = serde_json::from_slice(raw).expect("the answer is a Reply");
        assert_eq!(reply, me(host.port()));
    }
    assert!(
        drain(&asker).is_empty(),
        "four malformed requests and two good ones: exactly two answers, a third came"
    );
    assert!(
        !task.is_finished(),
        "malformed datagrams must not stop the responder"
    );
}

// --- probe ------------------------------------------------------------------

#[tokio::test]
async fn probe_finds_a_responder_and_says_where_the_answer_came_from() {
    let (host, _task) = responder(None).await;

    let found = beacon::probe(host.port(), PROBE_WAIT, &[host]).await;

    assert_eq!(found, vec![(host, me(host.port()))]);
}

#[tokio::test]
async fn probe_collects_every_host_that_answers_within_the_wait() {
    let (first, _a) = responder(None).await;
    let (second, _b) = responder(Some(first.port())).await;

    let mut found = beacon::probe(first.port(), PROBE_WAIT, &[first, second]).await;
    found.sort_by_key(|(addr, _)| *addr);
    let mut want = vec![(first, me(first.port())), (second, me(first.port()))];
    want.sort_by_key(|(addr, _)| *addr);

    assert_eq!(
        found, want,
        "two hosts answered; a probe that stops at the first one hides the second"
    );
}

#[tokio::test]
async fn probe_goes_on_past_a_target_that_does_not_answer() {
    // A port nobody listens on any more. On Windows the ICMP that comes back surfaces as
    // WSAECONNRESET on the next receive of an unconnected socket, and a probe that treats
    // that as the end hears nothing from the live host.
    let dead = {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap()
    };
    let (live, _task) = responder(None).await;

    let started = Instant::now();
    let found = beacon::probe(live.port(), PROBE_WAIT, &[dead, live]).await;

    assert_eq!(found, vec![(live, me(live.port()))]);
    assert!(
        started.elapsed() < PROBE_WAIT + GUARD,
        "probe is bounded by its wait; it took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn probe_keeps_only_well_formed_replies_and_sends_a_well_formed_request() {
    let fake = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = fake.local_addr().unwrap();
    let valid = me(addr.port());
    let mut wrong_service = serde_json::to_value(&valid).unwrap();
    wrong_service["service"] = "otro-servicio".into();
    let mut wrong_version = serde_json::to_value(&valid).unwrap();
    wrong_version["v"] = 2.into();
    let answers = vec![
        b"not json at all".to_vec(),
        serde_json::to_vec(&wrong_service).unwrap(),
        serde_json::to_vec(&wrong_version).unwrap(),
        serde_json::to_vec(&valid).unwrap(),
    ];
    let host = tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        let (n, asker) = fake.recv_from(&mut buf).await.unwrap();
        for answer in &answers {
            fake.send_to(answer, asker).await.unwrap();
        }
        buf[..n].to_vec()
    });

    let found = beacon::probe(addr.port(), PROBE_WAIT, &[addr]).await;

    assert_eq!(
        found,
        vec![(addr, valid)],
        "garbage, another service and another version are not the host"
    );
    let sent = tokio::time::timeout(GUARD, host)
        .await
        .expect("the fake host received the probe")
        .unwrap();
    assert!(
        (64..=512).contains(&sent.len()),
        "the request is 64..=512 bytes, or a host following the protocol ignores it: {} bytes",
        sent.len()
    );
    let asked: Value = serde_json::from_slice(&sent).expect("the request is JSON");
    assert_eq!(asked["q"], "memory-industry");
    assert_eq!(asked["v"], 1);
}
