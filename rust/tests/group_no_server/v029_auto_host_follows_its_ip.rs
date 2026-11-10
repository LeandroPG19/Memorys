//! The host in automatic LAN mode (0.29, slices A1-A3): it serves on loopback the whole time,
//! serves on its private IP while it has one, and follows that IP when it changes.
//!
//! Deterministic on Linux without touching the network: 127.0.0.2 and 127.0.0.3 are both
//! local, so they stand for the old and the new LAN address; the IP comes from an injected
//! provider and the watcher's sleep is a channel the test ticks. 127.0.0.1 is never the
//! «LAN» address here, because the loopback listener already holds that port.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use memory_industry::http::{self, LanMode, LanStatus};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

/// A guard on waits that end as soon as the thing happens. Reaching it is the failure.
const GUARD: Duration = Duration::from_secs(5);

const OLD_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
const NEW_IP: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 3);

/// The two waits the specification fixes for the watcher.
const WITHOUT_IP: Duration = Duration::from_secs(5);
const WITH_IP: Duration = Duration::from_secs(10);

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn at(ip: Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(ip), port)
}

/// A value the watcher has to overwrite, so a status that never moved cannot pass for one
/// that did.
fn placeholder() -> LanStatus {
    LanStatus {
        mode: LanMode::Fixed,
        address: Some("192.0.2.1:9".parse().unwrap()),
        since: chrono::Utc::now(),
    }
}

type Nap = Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// The watcher's sleep, driven by the test: every call reports how long it asked to sleep
/// and returns only when the test ticks.
struct Ticker {
    asked: mpsc::UnboundedReceiver<Duration>,
    tick: mpsc::UnboundedSender<()>,
}

impl Ticker {
    async fn next_nap(&mut self) -> Duration {
        tokio::time::timeout(GUARD, self.asked.recv())
            .await
            .expect("the watcher never went back to sleep: it has no loop, or it is stuck")
            .expect("the watcher's sleep was dropped")
    }

    fn wake(&self) {
        self.tick.send(()).unwrap();
    }
}

fn fake_sleep() -> (impl FnMut(Duration) -> Nap + Send + 'static, Ticker) {
    let (asked_tx, asked) = mpsc::unbounded_channel();
    let (tick, tick_rx) = mpsc::unbounded_channel::<()>();
    let tick_rx = Arc::new(tokio::sync::Mutex::new(tick_rx));
    let sleep = move |nap: Duration| -> Nap {
        let _ = asked_tx.send(nap);
        let tick_rx = tick_rx.clone();
        Box::pin(async move {
            tick_rx.lock().await.recv().await;
        })
    };
    (sleep, Ticker { asked, tick })
}

/// What the machine's private IP is right now, as far as the host can tell.
fn provider(
    ip: &Arc<Mutex<Option<Ipv4Addr>>>,
) -> impl FnMut() -> Option<Ipv4Addr> + Send + 'static {
    let ip = ip.clone();
    move || *ip.lock().unwrap()
}

/// `/` answers at once; `/slow` says it started and then waits for the test, so a request
/// can be held in flight across a change of address.
fn app(entered: Arc<Notify>, release: Arc<Notify>) -> Router {
    Router::new().route("/", get(|| async { "ok" })).route(
        "/slow",
        get(move || {
            let entered = entered.clone();
            let release = release.clone();
            async move {
                entered.notify_one();
                release.notified().await;
                "slow done"
            }
        }),
    )
}

/// One HTTP/1.1 GET over a fresh connection: the whole response, or why there was none.
async fn get_text(addr: SocketAddr, path: &str) -> Result<String, String> {
    let exchange = async {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect {addr}: {e}"))?;
        let ask = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(ask.as_bytes())
            .await
            .map_err(|e| format!("write to {addr}: {e}"))?;
        let mut body = Vec::new();
        stream
            .read_to_end(&mut body)
            .await
            .map_err(|e| format!("read from {addr}: {e}"))?;
        Ok::<_, String>(String::from_utf8_lossy(&body).into_owned())
    };
    tokio::time::timeout(GUARD, exchange)
        .await
        .unwrap_or_else(|_| Err(format!("{addr}{path} did not answer within {GUARD:?}")))
}

async fn answers(addr: SocketAddr, why: &str) {
    let got = get_text(addr, "/").await;
    assert!(
        got.as_deref()
            .is_ok_and(|r| r.starts_with("HTTP/1.1 200") && r.ends_with("ok")),
        "{why}: {got:?}"
    );
}

/// A refused connection, never a bind: a bind can fail because another test's client holds
/// the port, and that would read as «the host still listens».
async fn refused(addr: SocketAddr, why: &str) {
    match TcpStream::connect(addr).await {
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
        other => panic!("{why}: connecting to {addr} gave {other:?}, not a refused connection"),
    }
}

/// Wait until the published status is `want`, failing fast if the host stopped.
async fn until(
    status: &mut watch::Receiver<LanStatus>,
    host: &mut JoinHandle<anyhow::Result<()>>,
    want: (LanMode, Option<SocketAddr>),
    why: &str,
) -> LanStatus {
    let waited = tokio::time::timeout(GUARD, async {
        tokio::select! {
            seen = status.wait_for(|s| (s.mode, s.address) == want) => {
                Ok(seen.expect("the host dropped its status sender").clone())
            }
            ended = host => Err(format!("the host stopped serving: {ended:?}")),
        }
    })
    .await;
    match waited {
        Ok(Ok(seen)) => seen,
        Ok(Err(stopped)) => panic!("{stopped}"),
        Err(_) => panic!("{why}: status is {:?}", *status.borrow()),
    }
}

#[tokio::test]
async fn the_host_follows_its_ip_and_loopback_never_stops_answering() {
    let port = free_port();
    let loopback = at(Ipv4Addr::LOCALHOST, port);
    let ip = Arc::new(Mutex::new(Some(OLD_IP)));
    let (sleep, mut ticker) = fake_sleep();
    let (status_tx, mut status) = watch::channel(placeholder());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut host = tokio::spawn(http::serve_following(
        port,
        app(entered.clone(), release.clone()),
        provider(&ip),
        sleep,
        status_tx,
        std::future::pending(),
    ));

    let first = until(
        &mut status,
        &mut host,
        (LanMode::Auto, Some(at(OLD_IP, port))),
        "the host never published the address it was given",
    )
    .await;
    assert_eq!(
        ticker.next_nap().await,
        WITH_IP,
        "with an IP the watcher looks again every 10 s"
    );
    answers(loopback, "loopback answers from the start").await;
    answers(at(OLD_IP, port), "the private IP answers once published").await;

    // The same IP again: nothing to do, and `since` keeps saying when it last changed.
    ticker.wake();
    assert_eq!(ticker.next_nap().await, WITH_IP);
    assert_eq!(
        *status.borrow(),
        first,
        "an IP that did not change must not move the status"
    );

    let slow = tokio::spawn(get_text(at(OLD_IP, port), "/slow"));
    tokio::time::timeout(GUARD, entered.notified())
        .await
        .expect("the slow request never reached the handler");

    *ip.lock().unwrap() = Some(NEW_IP);
    ticker.wake();
    let moved = until(
        &mut status,
        &mut host,
        (LanMode::Auto, Some(at(NEW_IP, port))),
        "the IP changed and one watcher cycle later the host is not on the new one: the \
         watcher does not read the IP again, or waits for the requests in flight first",
    )
    .await;
    assert!(
        moved.since > first.since,
        "`since` says when the address last changed"
    );

    refused(
        at(OLD_IP, port),
        "the old address must be closed once the new one is published",
    )
    .await;
    answers(at(NEW_IP, port), "the new address answers once published").await;
    answers(loopback, "loopback keeps answering through the change").await;

    release.notify_one();
    let finished = tokio::time::timeout(GUARD, slow)
        .await
        .expect("the request in flight on the old address never finished")
        .unwrap();
    assert!(
        finished.as_deref().is_ok_and(|r| r.ends_with("slow done")),
        "a request in flight when the address changed finishes on the old connection: {finished:?}"
    );
    assert_eq!(ticker.next_nap().await, WITH_IP);
}

#[tokio::test]
async fn without_a_private_ip_it_serves_loopback_and_takes_the_ip_when_it_appears() {
    let port = free_port();
    let ip = Arc::new(Mutex::new(None));
    let (sleep, mut ticker) = fake_sleep();
    let (status_tx, mut status) = watch::channel(placeholder());
    let mut host = tokio::spawn(http::serve_following(
        port,
        app(Arc::new(Notify::new()), Arc::new(Notify::new())),
        provider(&ip),
        sleep,
        status_tx,
        std::future::pending(),
    ));

    until(
        &mut status,
        &mut host,
        (LanMode::Auto, None),
        "with no private IP the host still publishes that it is automatic and has no address",
    )
    .await;
    assert_eq!(
        ticker.next_nap().await,
        WITHOUT_IP,
        "without an IP the watcher looks again every 5 s"
    );
    answers(
        at(Ipv4Addr::LOCALHOST, port),
        "loopback serves while there is no private IP",
    )
    .await;
    refused(
        at(OLD_IP, port),
        "nothing is bound on an IP the host was not given",
    )
    .await;

    *ip.lock().unwrap() = Some(OLD_IP);
    ticker.wake();
    until(
        &mut status,
        &mut host,
        (LanMode::Auto, Some(at(OLD_IP, port))),
        "the IP appeared and the host did not take it",
    )
    .await;
    answers(at(OLD_IP, port), "the IP that appeared answers").await;
    assert_eq!(ticker.next_nap().await, WITH_IP);
}

#[tokio::test]
async fn a_loopback_port_already_taken_is_an_error_that_names_it() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let (sleep, _ticker) = fake_sleep();
    let (status_tx, _status) = watch::channel(placeholder());

    let outcome = tokio::time::timeout(
        GUARD,
        http::serve_following(
            port,
            app(Arc::new(Notify::new()), Arc::new(Notify::new())),
            || Some(OLD_IP),
            sleep,
            status_tx,
            std::future::pending(),
        ),
    )
    .await
    .expect("a taken loopback port must fail at once, not be retried forever");

    let why = format!(
        "{:#}",
        outcome.expect_err("loopback is not optional in lan mode")
    );
    assert!(
        why.contains(&format!("127.0.0.1:{port}")),
        "the error names the address that could not be bound: {why}"
    );
}

#[test]
fn the_lan_ip_is_the_first_private_rfc_1918_candidate() {
    let ip = |raw: &str| -> IpAddr { raw.parse().unwrap() };
    let v4 = |raw: &str| -> Option<Ipv4Addr> { Some(raw.parse().unwrap()) };

    assert_eq!(
        http::private_lan_ip(&[ip("192.168.1.5")]),
        v4("192.168.1.5")
    );
    assert_eq!(
        http::private_lan_ip(&[ip("8.8.8.8"), ip("10.0.0.7")]),
        v4("10.0.0.7"),
        "a public source on the default route is skipped for the anchors' private one"
    );
    assert_eq!(
        http::private_lan_ip(&[
            ip("127.0.0.1"),
            ip("169.254.3.4"),
            ip("100.64.1.1"),
            ip("172.16.0.9"),
        ]),
        v4("172.16.0.9"),
        "loopback, link-local and carrier-grade NAT are never the LAN"
    );
    assert_eq!(
        http::private_lan_ip(&[ip("10.1.1.1"), ip("192.168.0.5")]),
        v4("10.1.1.1"),
        "the default route comes first and wins"
    );
    assert_eq!(
        http::private_lan_ip(&[ip("172.31.255.254")]),
        v4("172.31.255.254")
    );

    for outside in [
        "172.32.0.1",
        "172.15.255.255",
        "192.169.0.1",
        "0.0.0.0",
        "::1",
        "fd00::1",
    ] {
        assert_eq!(
            http::private_lan_ip(&[ip(outside)]),
            None,
            "{outside} is not RFC 1918 IPv4"
        );
    }
    assert_eq!(http::private_lan_ip(&[]), None);
}
