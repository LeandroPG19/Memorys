//! The real daemon in automatic LAN mode (0.29, slices A2, A4, A5): `CUBA_HTTP_ADDR=lan:<port>`
//! serves on loopback whatever the machine's network is, says in `/health` where its LAN
//! listener is, answers the UDP announcement, and still demands a real token.
//!
//! No database: the pool points at a port nothing listens on, and nothing here asks the
//! daemon anything a database would answer. The private IP is whatever this machine has, so
//! the assertions accept both «it has one» and «it has none» and pin what must hold in both.

use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use memory_industry::beacon;
use serde_json::Value;

const DEAD_DB: &str = "postgresql://cuba:canarypass-db@127.0.0.1:1/nowhere";

const TOKEN: &str = "lan-auto-token-0123456789abcdef0123456789";

const HOST_NAME: &str = "madre-de-prueba";

/// The daemon's own ceiling on loading models before it opens for business; nothing here
/// reads `ready`.
const WARM_CEILING: Duration = Duration::from_secs(10);

/// How long a test waits for its daemon to open the port.
const OPEN_BUDGET: Duration = Duration::from_secs(30);

const _: () = assert!(
    OPEN_BUDGET.as_secs() > WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the daemon is failed for opening on time"
);

const PROBE_BUDGET: Duration = Duration::from_secs(5);
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// A ceiling on a datagram's round trip over loopback.
const BEACON_WAIT: Duration = Duration::from_millis(500);

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

/// A machine with no models, no panel and no systemd socket, whatever this developer has.
fn quiet_machine(token: Option<&str>) -> Vec<Env> {
    let nowhere = std::env::temp_dir().join("v029-auto-host-no-such-model");
    let ceiling = WARM_CEILING.as_secs().to_string();
    vec![
        match token {
            Some(t) => Env::set("CUBA_HTTP_TOKEN", t),
            None => Env::cleared("CUBA_HTTP_TOKEN"),
        },
        Env::cleared("CUBA_PEER_TOKEN"),
        Env::set("CUBA_NODE_NAME", HOST_NAME),
        Env::set("CUBA_RERANKER_PATH", "disabled-by-resource-plan"),
        Env::set("CUBA_NLI_PATH", "disabled-by-resource-plan"),
        Env::set("ONNX_MODEL_PATH", &nowhere.to_string_lossy()),
        Env::set("CUBA_WARM_RERANKER", "0"),
        Env::set("MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS", &ceiling),
        Env::cleared("CUBA_PANEL"),
        Env::cleared("CUBA_IDLE_SHUTDOWN_SECS"),
        Env::cleared("LISTEN_FDS"),
        Env::cleared("LISTEN_PID"),
    ]
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(PROBE_BUDGET)
        .no_proxy()
        .build()
        .unwrap()
}

/// Start `serve_pool(addr)` and wait until `127.0.0.1:port` serves, or fail with the
/// daemon's own reason for stopping.
async fn daemon(addr: String, port: u16) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let pool = memory_industry::db::create_lazy_pool(DEAD_DB);
    let mut task =
        tokio::spawn(async move { memory_industry::http::serve_pool(&addr, pool, false).await });

    let url = format!("http://127.0.0.1:{port}/connect");
    let client = client();
    let deadline = Instant::now() + OPEN_BUDGET;
    let mut last = String::from("no probe ran");
    while Instant::now() < deadline {
        if task.is_finished() {
            let ended = (&mut task).await;
            panic!("the daemon stopped before 127.0.0.1:{port} served anything: {ended:?}");
        }
        match client.get(&url).send().await {
            Ok(_) => return task,
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(PROBE_EVERY).await;
    }
    panic!("127.0.0.1:{port} never served within {OPEN_BUDGET:?}. Last: {last}");
}

async fn lan_block(port: u16) -> Value {
    // Above HEALTH_DB_TIMEOUT (5 s): with the database dead, /health spends all of it first.
    let body: Value = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
        .expect("/health answers on loopback")
        .json()
        .await
        .expect("/health is JSON");
    let lan = body
        .get("lan")
        .unwrap_or_else(|| panic!("/health has no `lan` block: {body}"))
        .clone();
    let mut keys: Vec<&str> = lan
        .as_object()
        .unwrap_or_else(|| panic!("`lan` is an object: {lan}"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["address", "mode", "since"],
        "/health answers anyone who can route a packet: the block says where and since when, \
         nothing else: {lan}"
    );
    let since = lan["since"].as_str().expect("`since` is a string");
    chrono::DateTime::parse_from_rfc3339(since)
        .unwrap_or_else(|e| panic!("`since` is RFC 3339 ({e}): {since}"));
    lan
}

fn is_rfc1918(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
        }
        IpAddr::V6(_) => false,
    }
}

#[tokio::test]
async fn a_lan_daemon_serves_loopback_says_auto_in_health_and_stays_silent_without_a_host_id() {
    let _one_at_a_time = ENV_GUARD.lock().await;
    let _env = quiet_machine(Some(TOKEN));
    let port = free_port();
    let _daemon = daemon(format!("lan:{port}"), port).await;

    let lan = lan_block(port).await;
    assert_eq!(lan["mode"], "auto", "{lan}");
    match &lan["address"] {
        Value::Null => {}
        Value::String(raw) => {
            let addr: SocketAddr = raw
                .parse()
                .unwrap_or_else(|e| panic!("`address` is ip:port ({e}): {raw}"));
            assert_eq!(
                addr.port(),
                port,
                "the LAN listener is on the configured port"
            );
            assert!(
                is_rfc1918(addr.ip()),
                "automatic mode only ever picks a private RFC 1918 address: {addr}"
            );
        }
        other => panic!("`address` is null or ip:port, not {other}"),
    }

    // The database is dead, so the host id is nil and no client could verify this host: the
    // announcement holds its port and stays silent. The answer with a real id is pinned by
    // the beacon tests on sockets.
    let held = std::net::UdpSocket::bind(("0.0.0.0", port));
    assert!(
        held.as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::AddrInUse),
        "anchor: a lan daemon holds UDP {port} for the announcement, got {held:?}"
    );
    let loopback = SocketAddr::from(([127, 0, 0, 1], port));
    let heard = beacon::probe(port, BEACON_WAIT, &[loopback]).await;
    assert!(
        heard.is_empty(),
        "with a nil host id the announcement must not answer: {heard:?}"
    );
}

#[tokio::test]
async fn a_loopback_daemon_reports_a_fixed_address_and_announces_nothing() {
    let _one_at_a_time = ENV_GUARD.lock().await;
    let _env = quiet_machine(None);
    let port = free_port();
    let _daemon = daemon(format!("127.0.0.1:{port}"), port).await;

    let lan = lan_block(port).await;
    assert_eq!(lan["mode"], "fixed", "{lan}");
    assert_eq!(lan["address"], format!("127.0.0.1:{port}"), "{lan}");

    let heard = beacon::probe(
        port,
        BEACON_WAIT,
        &[SocketAddr::from(([127, 0, 0, 1], port))],
    )
    .await;
    assert!(
        heard.is_empty(),
        "a daemon only this machine can reach has nothing to announce: {heard:?}"
    );
}

#[tokio::test]
async fn lan_mode_still_demands_a_real_token_before_it_binds_anything() {
    let _one_at_a_time = ENV_GUARD.lock().await;
    let port = free_port();

    for (token, must_name) in [(None, "CUBA_HTTP_TOKEN"), (Some("corto"), "at least 32")] {
        let _env = quiet_machine(token);
        let pool = memory_industry::db::create_lazy_pool(DEAD_DB);
        let outcome = tokio::time::timeout(
            PROBE_BUDGET,
            memory_industry::http::serve_pool(&format!("lan:{port}"), pool, false),
        )
        .await
        .expect("a refused token fails at once instead of serving");
        let why = format!(
            "{:#}",
            outcome.expect_err("lan mode is reachable from other machines and needs a token")
        );
        assert!(
            why.contains(must_name),
            "with token {token:?} the refusal names {must_name}: {why}"
        );
        match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            other => panic!("a refused daemon bound nothing, yet 127.0.0.1:{port} gave {other:?}"),
        }
    }
}
