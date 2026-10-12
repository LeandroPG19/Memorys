// needs-a-server: starts the real daemon on loopback ports (and, for one test, on 0.0.0.0) against scratch databases it creates and drops, and pairs, authenticates, revokes and restarts a real bridge through it
//
//! The bridge and the daemon, talking for real: `bridge::pair` against `/identity` and `/pair`,
//! the bridge's loopback listener forwarding to the real `/mcp`, and the daemon's brake counting
//! what the bridge sends it.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched. One daemon per test (two where the scenario needs two), each on its own port in
//! 18880..=18891, each over its own scratch database, so the per-address brake of one test is
//! never the brake of another.
//!
//! What stands in for the outside world, and why:
//! - the network the bridge asks the OS for (`LoopbackOnly`): a name never resolves and no LAN is
//!   swept, so a test that loses its daemon does not go looking for one on the machine's real /24;
//! - the monotonic clock (`JumpClock`): "30 seconds later" is an `advance`, not a sleep;
//! - the HTTP client of the bridge keeps no idle connections, so a daemon that is stopped is
//!   really gone for the next request and not still answering on a pooled socket;
//! - the state directory is under the temp dir, never the user's home or `%LOCALAPPDATA%`.
//!
//! `MEMORY_INDUSTRY_REQUIRE_ACCOUNTS` only bites on a bind other machines could reach, and
//! `0.0.0.0` is the one such address every machine has. J5 binds it for the length of the test
//! behind the shared token and over an empty scratch database, and stops it at the end.

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use common::in_a_scratch_database;
use memory_industry::accounts::{
    PairRequest, PairResponse, REQUIRE_ACCOUNTS_REFUSAL, Secret, code_key, fingerprint,
    identity_proof, normalize_code, pair_request_mac, pair_response_mac, random_bytes, verify_mac,
};
use memory_industry::accounts_db as db;
use memory_industry::bridge::client_config::{apply_client_config, bridge_block};
use memory_industry::bridge::discovery::{Verdict, check_identity};
use memory_industry::bridge::pair::{PairError, pair};
use memory_industry::bridge::proxy::BridgeHandle;
use memory_industry::bridge::state::{BridgeState, read_state, write_state};
use memory_industry::bridge::{self, BridgeDeps, Clock, Network, STATE_BASENAME, Timing};
use memory_industry::service::Target;
use memory_industry::service_apply::{CommandOutcome, CommandRunner};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use uuid::Uuid;

const SHARED: &str = "j-shared-token-for-the-lan-0123456789ab";
const PEER: &str = "j-peer-token-for-the-other-machine-0123";

const J1_PORT: u16 = 18880;
const J2_PORT: u16 = 18881;
const J3_PORT: u16 = 18882;
const J4_MOVED_FIRST: u16 = 18883;
const J4_MOVED_SECOND: u16 = 18884;
const J4_PINNED_HOST: u16 = 18885;
const J4_OTHER_HOST: u16 = 18886;
const J5_KNOB_ON: u16 = 18887;
const J5_KNOB_OFF: u16 = 18888;
const J6_PORT: u16 = 18889;
const J7_PORT: u16 = 18890;
const J2B_PORT: u16 = 18891;

const LOOPBACK: &str = "127.0.0.1";
const EVERY_INTERFACE: &str = "0.0.0.0";

/// The daemon's own ceiling on loading models before it opens for business. Short here because
/// nothing in this file asks the daemon anything a model could answer.
const WARM_CEILING: Duration = Duration::from_secs(2);

/// How long a test waits for its daemon to open the port. It must outlast the warm ceiling or a
/// daemon that did exactly what it was told is failed for it; the next line makes that a build
/// error instead of a sentence somebody can read past.
const OPEN_BUDGET: Duration = Duration::from_secs(30);

const _: () = assert!(
    OPEN_BUDGET.as_secs() > WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the daemon is failed for opening on time"
);

const PROBE_BUDGET: Duration = Duration::from_secs(5);
const PROBE_EVERY: Duration = Duration::from_millis(250);

/// No call of this file waits longer than this for an answer.
const CALL_BUDGET: Duration = Duration::from_secs(20);

/// The daemon brakes an address at its tenth refused credential (`MAX_AUTH_FAILURES`).
const BRAKE_AT: u32 = 10;

const ROUNDS_OF_THE_RACE: usize = 10;

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------------------------
// The daemon, started the way the other real-HTTP tests start it.
// ---------------------------------------------------------------------------------------------

/// A running daemon. Dropping it stops it, and so does `stop`, which also waits for the port to
/// be released.
struct Daemon {
    task: JoinHandle<()>,
}

impl Daemon {
    async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Block until `/health` answers on `port`, or fail saying it never did. Polling, not sleeping a
/// fixed number: any number is right on an idle machine and wrong on the busy one that failed.
async fn wait_until_it_serves(port: u16, daemon: &JoinHandle<()>) {
    let url = format!("http://127.0.0.1:{port}/health");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(PROBE_BUDGET)
        .build()
        .expect("a probe client with a bounded attempt");
    let deadline = Instant::now() + OPEN_BUDGET;
    let mut last = String::from("it never answered");

    while Instant::now() < deadline {
        assert!(
            !daemon.is_finished(),
            "the daemon on port {port} ended before it served anything, so nothing below can \
             run. serve_pool printed why it stopped; a port still held by an earlier run is the \
             usual reason"
        );
        match client.get(&url).send().await {
            Ok(_) => return,
            Err(why) => last = format!("no answer at all: {why}"),
        }
        tokio::time::sleep(PROBE_EVERY).await;
    }
    panic!(
        "the daemon at {url} never answered within {}s. Last: {last}. serve_pool binds the port \
         before it loads its models, and it is pinned to open within {}s of warm-up",
        OPEN_BUDGET.as_secs(),
        WARM_CEILING.as_secs()
    );
}

/// Starts a daemon bound to `bind:port` over `pool`. The environment is the process's and the
/// daemon reads it once, at boot, so the guard is held until the port answers.
async fn start_daemon(
    bind: &str,
    port: u16,
    pool: &PgPool,
    env: &[(&str, Option<&str>)],
) -> Daemon {
    let _env = ENV_GUARD.lock().await;
    unsafe {
        std::env::set_var("CUBA_HTTP_TOKEN", SHARED);
        std::env::set_var("CUBA_PEER_TOKEN", PEER);
        std::env::set_var(
            "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
            WARM_CEILING.as_secs().to_string(),
        );
        for name in [
            "CUBA_PANEL",
            "CUBA_HTTP_ALLOWED_ORIGINS",
            "MEMORY_INDUSTRY_REQUIRE_ACCOUNTS",
            "CUBA_NODE_NAME",
        ] {
            std::env::remove_var(name);
        }
        for (name, value) in env {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
    let addr = format!("{bind}:{port}");
    let pool = pool.clone();
    let task = tokio::spawn(async move {
        if let Err(why) = memory_industry::http::serve_pool(&addr, pool, false).await {
            eprintln!("the daemon on {addr} stopped: {why:#}");
        }
    });
    wait_until_it_serves(port, &task).await;
    Daemon { task }
}

async fn migrated(url: &str) -> PgPool {
    memory_industry::db::create_pool(url)
        .await
        .expect("migrating the scratch database")
}

fn addr_of(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, port))
}

// ---------------------------------------------------------------------------------------------
// What the bridge is given instead of the machine's own network, clock and shell.
// ---------------------------------------------------------------------------------------------

/// The bridge's view of the OS with nothing outside loopback in it: no name resolves, no
/// interface is known, so the sweep has nothing to sweep.
struct LoopbackOnly;

#[async_trait::async_trait]
impl Network for LoopbackOnly {
    async fn resolve(&self, _name: &str) -> Vec<IpAddr> {
        Vec::new()
    }

    fn own_ipv4_towards(&self, _peer: IpAddr) -> Option<Ipv4Addr> {
        None
    }
}

/// A monotonic clock the test moves by hand.
struct JumpClock {
    base: Instant,
    jumped_secs: AtomicU64,
}

impl JumpClock {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            base: Instant::now(),
            jumped_secs: AtomicU64::new(0),
        })
    }

    fn advance(&self, by: Duration) {
        self.jumped_secs.fetch_add(by.as_secs(), Ordering::SeqCst);
    }
}

impl Clock for JumpClock {
    fn now(&self) -> Instant {
        self.base + Duration::from_secs(self.jumped_secs.load(Ordering::SeqCst))
    }
}

/// Runs a command for real. On unix `write_state` never asks for one (the file is born 0600);
/// on Windows it asks for `whoami` and `icacls`, which this answers like the bridge's own runner.
struct RealCommands;

impl CommandRunner for RealCommands {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<CommandOutcome> {
        let done = std::process::Command::new(program).args(args).output()?;
        Ok(CommandOutcome {
            code: done.status.code(),
            stdout: String::from_utf8_lossy(&done.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&done.stderr).into_owned(),
        })
    }
}

/// The production timing with every wait that could hang a test given an explicit, short bound.
/// `reverify_after` and `revoked_recheck` stay the production 30 s and 60 s: the clock moves,
/// the numbers do not.
fn timing() -> Timing {
    Timing {
        resolve: Duration::from_secs(1),
        identity: Duration::from_secs(3),
        probe_connect: Duration::from_millis(400),
        probe_total: Duration::from_secs(3),
        discovery_total: Duration::from_secs(8),
        request_waits: Duration::from_secs(15),
        upstream_headers: Duration::from_secs(30),
        probe_parallelism: 4,
        ..Timing::production()
    }
}

fn bridge_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .connect_timeout(Duration::from_secs(2))
        .build()
        .expect("the bridge's HTTP client")
}

/// A scratch directory for one test's bridge state and client configs, gone with the test.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("memory-industry-j-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("a scratch directory under the temp dir");
        Self(dir)
    }

    fn state_path(&self) -> PathBuf {
        self.0.join(STATE_BASENAME)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn save(path: &Path, state: &BridgeState) {
    write_state(path, state, Target::host(), &mut RealCommands)
        .expect("the bridge state is written to the scratch directory");
}

/// The bridge, as `bridge run` starts it, listening on `port` (0 = any free loopback port).
async fn open_bridge(path: &Path, clock: &Arc<JumpClock>, port: Option<u16>) -> BridgeHandle {
    let deps = BridgeDeps {
        state_path: path.to_path_buf(),
        target: Target::host(),
        runner: Box::new(RealCommands),
        net: Arc::new(LoopbackOnly),
        clock: clock.clone(),
        timing: timing(),
        client: bridge_client(),
    };
    bridge::run(deps, port)
        .await
        .expect("a paired bridge starts and listens on loopback")
}

// ---------------------------------------------------------------------------------------------
// Accounts, codes and pairing.
// ---------------------------------------------------------------------------------------------

async fn account(pool: &PgPool, name: &str) {
    db::add_account(pool, name, Utc::now())
        .await
        .unwrap_or_else(|e| panic!("add_account({name}) must succeed: {e:#}"));
}

async fn code_for(pool: &PgPool, account_name: &str) -> String {
    db::issue_pairing_code(pool, account_name, None, Utc::now())
        .await
        .unwrap_or_else(|e| panic!("issue_pairing_code({account_name}) must succeed: {e:#}"))
        .code
}

/// `bridge pair <code> --host 127.0.0.1:<port>`, as the client types it.
async fn pair_against(port: u16, code: &str, label: &str) -> Result<BridgeState, PairError> {
    pair(
        code,
        Some(&format!("{LOOPBACK}:{port}")),
        label,
        &LoopbackOnly,
        &timing(),
        &bridge_client(),
    )
    .await
}

async fn paired_state(pool: &PgPool, port: u16, account_name: &str, label: &str) -> BridgeState {
    let code = code_for(pool, account_name).await;
    pair_against(port, &code, label)
        .await
        .unwrap_or_else(|e| panic!("a fresh code for {account_name} must pair: {e}"))
}

// ---------------------------------------------------------------------------------------------
// Talking HTTP.
// ---------------------------------------------------------------------------------------------

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("the body is not JSON ({e}): {}", self.body))
    }

    fn error_message(&self) -> String {
        self.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// What a tool answered: the JSON the daemon wraps in `content[0].text`.
    fn tool_result(&self) -> Value {
        let envelope = self.json();
        let text = envelope["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("no tool result in {envelope}"));
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("the tool result is not JSON ({e}): {text}"))
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(CALL_BUDGET)
        .build()
        .expect("a bounded HTTP client")
}

async fn send(request: reqwest::RequestBuilder) -> Reply {
    let response = request.send().await.expect("the server answers");
    let status = response.status().as_u16();
    let body = response.text().await.expect("a body");
    Reply { status, body }
}

fn ping() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})
}

fn whoami_call() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "cuba_whoami", "arguments": {}}
    })
}

/// A call straight to the daemon, with the bearer given (or none).
async fn to_the_daemon(port: u16, bearer: Option<&str>, body: Value) -> Reply {
    let mut request = http()
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .json(&body);
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    send(request).await
}

/// What a local MCP client does: POST to the bridge's `/mcp`, with no `Authorization` at all.
async fn through_the_bridge(bridge: &BridgeHandle, body: Value) -> Reply {
    let url = format!("http://{}/mcp", bridge.local_addr());
    send(http().post(url).json(&body)).await
}

fn assert_named(reply: &Reply, device: &str, why: &str) {
    assert_eq!(reply.status, 200, "{why}: {}", reply.body);
    let who = reply.tool_result();
    assert_eq!(who["account"], json!("ana"), "{why}: {who}");
    assert_eq!(who["device"], json!(device), "{why}: {who}");
}

/// How many refused credentials the daemon holds against 127.0.0.1, read the only way a client
/// can: send refusals until the brake answers 429. The brake trips at `BRAKE_AT`, so the number
/// of 401s received before it is `BRAKE_AT` minus what was already recorded. Spends the budget of
/// the address: call it last.
async fn failures_the_daemon_recorded(port: u16) -> u32 {
    let mut refused = 0;
    for _ in 0..=BRAKE_AT {
        match to_the_daemon(port, Some("not-the-token"), ping())
            .await
            .status
        {
            401 => refused += 1,
            429 => return BRAKE_AT - refused,
            other => panic!("a refused credential must be a 401 or a 429, got {other}"),
        }
    }
    panic!("{BRAKE_AT} refusals in a row and the daemon never braked: it is not counting them")
}

async fn refuses_connections(addr: SocketAddr) -> bool {
    // Windows takes ~2 s to refuse a connect to a port with no listener (measured 2047 ms), and
    // Linux refuses at once. 6 s gives Windows room to refuse on its own; a timer that fires first
    // still counts as "not refused", because only a real `Err` from connect matches below.
    const REFUSAL_WAIT: Duration = Duration::from_secs(6);
    matches!(
        tokio::time::timeout(REFUSAL_WAIT, TcpStream::connect(addr)).await,
        Ok(Err(_))
    )
}

fn sorted_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

// ---------------------------------------------------------------------------------------------
// A relay that writes down every byte the client sends upstream, standing where an address in
// the bridge's state points. A recorder in the test file because the bridge's own fakes are
// `#[cfg(test)]` inside the library and an integration test cannot reach them.
// ---------------------------------------------------------------------------------------------

struct Relay {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<u8>>>,
    task: JoinHandle<()>,
}

impl Relay {
    async fn in_front_of(upstream: SocketAddr) -> Relay {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a free loopback port for the relay");
        let addr = listener.local_addr().expect("the relay's address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(pipe(client, upstream, recorded.clone()));
            }
        });
        Relay { addr, seen, task }
    }

    /// Everything the clients sent, lowercased so a header name is found in any spelling.
    fn heard(&self) -> String {
        let bytes = self.seen.lock().expect("the relay's record").clone();
        String::from_utf8_lossy(&bytes).to_lowercase()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn pipe(mut client: TcpStream, upstream: SocketAddr, seen: Arc<Mutex<Vec<u8>>>) {
    let Ok(mut daemon) = TcpStream::connect(upstream).await else {
        return;
    };
    let (mut from_client, mut to_client) = client.split();
    let (mut from_daemon, mut to_daemon) = daemon.split();
    let upward = async {
        let mut buffer = [0u8; 4096];
        loop {
            let read = match from_client.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            seen.lock()
                .expect("the relay's record")
                .extend_from_slice(&buffer[..read]);
            if to_daemon.write_all(&buffer[..read]).await.is_err() {
                break;
            }
        }
        let _ = to_daemon.shutdown().await;
    };
    let downward = async {
        let _ = tokio::io::copy(&mut from_daemon, &mut to_client).await;
        let _ = to_client.shutdown().await;
    };
    tokio::join!(upward, downward);
}

// =============================================================================================
// J1 · from the account to the tool
// =============================================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j1_a_client_with_no_credential_is_named_by_the_daemon_through_the_bridge() {
    in_a_scratch_database("brain_j_whole", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let daemon = start_daemon(LOOPBACK, J1_PORT, &pool, &[]).await;
        let scratch = Scratch::new();

        let state = paired_state(&pool, J1_PORT, "ana", "pc-lab-1").await;
        let secret = state.secret.to_hex();
        let credential = state.credential_id.simple().to_string();
        save(&scratch.state_path(), &state);
        let bridge = open_bridge(&scratch.state_path(), &JumpClock::new(), Some(0)).await;

        let reply = through_the_bridge(&bridge, whoami_call()).await;
        assert_named(
            &reply,
            "pc-lab-1",
            "the client sent no Authorization: the bridge put the credential on the call, or \
             the daemon would have refused it",
        );

        let kept = std::fs::read_to_string(scratch.state_path()).expect("bridge.json");
        assert!(
            kept.contains(&secret),
            "anchor: bridge.json is where the secret lives, or the absence below proves nothing"
        );
        let client_file = scratch.0.join("claude.json");
        let block = bridge_block(bridge.local_addr().port(), None);
        apply_client_config(&client_file, &block, Utc::now(), true)
            .expect("setup client writes its config");
        let written = std::fs::read_to_string(&client_file).expect("the client's config");
        assert!(
            written.contains(&format!(
                "http://127.0.0.1:{}/mcp",
                bridge.local_addr().port()
            )),
            "anchor: the client config points at the bridge's port: {written}"
        );
        let daemon_port = J1_PORT.to_string();
        for hidden in [
            secret.as_str(),
            credential.as_str(),
            "mi1.",
            "Bearer",
            "authorization",
            daemon_port.as_str(),
        ] {
            assert!(
                !written.to_lowercase().contains(&hidden.to_lowercase()),
                "the client's config file is read by every tool on the PC and synced by some: \
                 {hidden:?} is the secret, the credential or the host's address, which stay in \
                 bridge.json. Config written: {written}"
            );
        }
        drop(bridge);
        daemon.stop().await;
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// J2 · revoking cuts the next call
// =============================================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j2_a_revoked_device_is_told_to_pair_again_and_the_bridge_stops_asking() {
    in_a_scratch_database("brain_j_revoke", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let daemon = start_daemon(LOOPBACK, J2_PORT, &pool, &[]).await;
        let scratch = Scratch::new();
        let clock = JumpClock::new();

        let state = paired_state(&pool, J2_PORT, "ana", "pc-lab-1").await;
        save(&scratch.state_path(), &state);
        let bridge = open_bridge(&scratch.state_path(), &clock, Some(0)).await;
        assert_named(
            &through_the_bridge(&bridge, whoami_call()).await,
            "pc-lab-1",
            "control: before the revocation the same call is served",
        );

        db::revoke_device(&pool, "ana", "pc-lab-1", Utc::now())
            .await
            .expect("revoking the device");
        clock.advance(Timing::production().reverify_after + Duration::from_secs(1));

        for round in 1..=5 {
            let reply = through_the_bridge(&bridge, whoami_call()).await;
            assert_eq!(
                reply.status, 401,
                "round {round}: a revoked device is refused: {}",
                reply.body
            );
            assert!(
                reply
                    .error_message()
                    .contains("revocada: empareja de nuevo"),
                "round {round}: the user must be told to pair again, not shown the daemon's \
                 'invalid bearer token'. Got: {}",
                reply.body
            );
        }

        let recorded = failures_the_daemon_recorded(J2_PORT).await;
        assert!(
            recorded <= 1,
            "five calls from a revoked device left {recorded} refused credentials on the \
             daemon's brake: a bridge that keeps sending the credential after it is revoked \
             spends the whole address's budget (10 per minute) and brakes the PC's other \
             clients, and the legitimate pairing of its replacement"
        );
        drop(bridge);
        daemon.stop().await;
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// J2b · revoking cuts the next call even inside the freshness window
// =============================================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j2b_a_revoked_device_is_cut_at_its_next_call_without_waiting_for_the_window() {
    in_a_scratch_database("brain_j_revoke_now", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let daemon = start_daemon(LOOPBACK, J2B_PORT, &pool, &[]).await;
        let scratch = Scratch::new();
        let clock = JumpClock::new();

        let state = paired_state(&pool, J2B_PORT, "ana", "pc-lab-1").await;
        save(&scratch.state_path(), &state);
        let bridge = open_bridge(&scratch.state_path(), &clock, Some(0)).await;
        assert_named(
            &through_the_bridge(&bridge, whoami_call()).await,
            "pc-lab-1",
            "control: before the revocation the same call is served",
        );

        db::revoke_device(&pool, "ana", "pc-lab-1", Utc::now())
            .await
            .expect("revoking the device");

        let first = through_the_bridge(&bridge, whoami_call()).await;
        assert_eq!(
            first.status, 401,
            "the call that finds out is refused by the daemon: {}",
            first.body
        );
        for round in 2..=5 {
            let reply = through_the_bridge(&bridge, whoami_call()).await;
            assert_eq!(
                reply.status, 401,
                "round {round}: a revoked device is refused: {}",
                reply.body
            );
            assert!(
                reply
                    .error_message()
                    .contains("revocada: empareja de nuevo"),
                "round {round}: the clock did not move, yet from the second call on the user \
                 must be told to pair again, not shown the daemon's 'invalid bearer token'. \
                 Got: {}",
                reply.body
            );
        }

        let recorded = failures_the_daemon_recorded(J2B_PORT).await;
        assert!(
            recorded <= 1,
            "five calls from a revoked device left {recorded} refused credentials on the \
             daemon's brake: the bridge kept sending the credential for the whole freshness \
             window, and ten in a minute brake the address for the PC's other bridges"
        );
        drop(bridge);
        daemon.stop().await;
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// J3 · two bridges, one code
// =============================================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j3_two_bridges_racing_for_one_code_leave_exactly_one_credential() {
    in_a_scratch_database("brain_j_race", |url_| async move {
        let pool = migrated(&url_).await;
        let daemon = start_daemon(LOOPBACK, J3_PORT, &pool, &[]).await;

        for round in 0..ROUNDS_OF_THE_RACE {
            let name = format!("ana-{round}");
            account(&pool, &name).await;
            let code = code_for(&pool, &name).await;
            let start_line = Arc::new(tokio::sync::Barrier::new(2));
            let racers: Vec<_> = ["pc-a", "pc-b"]
                .into_iter()
                .map(|label| {
                    let (start_line, code) = (start_line.clone(), code.clone());
                    tokio::spawn(async move {
                        start_line.wait().await;
                        pair_against(J3_PORT, &code, label).await
                    })
                })
                .collect();
            let mut outcomes = Vec::new();
            for racer in racers {
                outcomes.push(racer.await.expect("a racer finishes"));
            }

            let winners = outcomes.iter().filter(|o| o.is_ok()).count();
            assert_eq!(
                winners, 1,
                "round {round}: one pairing code is one device. Two Ok means the claim let a \
                 used code through; none means the race lost both. Outcomes: {outcomes:?}"
            );
            let loser = outcomes
                .iter()
                .find_map(|o| o.as_ref().err())
                .expect("one racer lost");
            assert!(
                matches!(loser, PairError::Refused)
                    && loser.to_string().contains("la madre no aceptó el código"),
                "round {round}: the loser gets the same sentence as a mistyped code, which does \
                 not say the code was used: {loser}"
            );
            let credentials: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM brain_account_credentials c
                   JOIN brain_accounts a ON a.id = c.account_id WHERE a.name = $1",
            )
            .bind(&name)
            .fetch_one(&pool)
            .await
            .expect("counting credentials");
            assert_eq!(
                credentials, 1,
                "round {round}: the database holds one credential for the code, whoever won"
            );
        }
        daemon.stop().await;
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// J4 · the daemon moves, and a different host is not given the credential
// =============================================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j4_when_the_first_daemon_goes_away_the_bridge_finds_the_second_and_remembers_it() {
    in_a_scratch_database("brain_j_moved", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let first = start_daemon(LOOPBACK, J4_MOVED_FIRST, &pool, &[]).await;
        let second = start_daemon(LOOPBACK, J4_MOVED_SECOND, &pool, &[]).await;
        let scratch = Scratch::new();

        let mut state = paired_state(&pool, J4_MOVED_FIRST, "ana", "pc-lab-1").await;
        state.addresses = vec![addr_of(J4_MOVED_FIRST), addr_of(J4_MOVED_SECOND)];
        state.last_good = Some(addr_of(J4_MOVED_FIRST));
        save(&scratch.state_path(), &state);
        let bridge = open_bridge(&scratch.state_path(), &JumpClock::new(), Some(0)).await;
        assert_named(
            &through_the_bridge(&bridge, whoami_call()).await,
            "pc-lab-1",
            "control: with both daemons up the call is served",
        );

        first.stop().await;
        assert!(
            refuses_connections(addr_of(J4_MOVED_FIRST)).await,
            "anchor: the first daemon must really be gone, or the call below proves nothing"
        );

        assert_named(
            &through_the_bridge(&bridge, whoami_call()).await,
            "pc-lab-1",
            "the first address refuses the connection: the request cannot have arrived, so the \
             bridge looks again and the second daemon, which shares the database and so the \
             host id, answers",
        );
        let healed = read_state(&scratch.state_path())
            .expect("bridge.json is readable")
            .expect("bridge.json exists");
        assert_eq!(
            healed.last_good,
            Some(addr_of(J4_MOVED_SECOND)),
            "the address that answered is written down, so the next start does not begin by \
             asking a dead one"
        );
        drop(bridge);
        second.stop().await;
        pool.close().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j4_a_daemon_with_another_database_is_refused_with_both_fingerprints_and_hears_no_credential()
 {
    in_a_scratch_database("brain_j_pinned", |url_pinned| async move {
        let pinned_pool = migrated(&url_pinned).await;
        let inner_pinned = pinned_pool.clone();
        in_a_scratch_database("brain_j_other", move |url_other| async move {
            let pinned_pool = inner_pinned;
            let other_pool = migrated(&url_other).await;
            let pinned_id = db::host_identity(&pinned_pool).await.expect("host").host_id;
            let other_id = db::host_identity(&other_pool).await.expect("host").host_id;
            assert_ne!(
                pinned_id, other_id,
                "anchor: two databases are two installations, each with its own host id"
            );
            account(&pinned_pool, "ana").await;
            let pinned_host = start_daemon(LOOPBACK, J4_PINNED_HOST, &pinned_pool, &[]).await;
            let other_host = start_daemon(LOOPBACK, J4_OTHER_HOST, &other_pool, &[]).await;
            let scratch = Scratch::new();

            let mut state = paired_state(&pinned_pool, J4_PINNED_HOST, "ana", "pc-lab-1").await;
            let relay = Relay::in_front_of(addr_of(J4_OTHER_HOST)).await;
            state.addresses = vec![relay.addr];
            state.last_good = Some(relay.addr);
            save(&scratch.state_path(), &state);
            let before = std::fs::read(scratch.state_path()).expect("bridge.json");
            let bridge = open_bridge(&scratch.state_path(), &JumpClock::new(), Some(0)).await;

            let reply = through_the_bridge(&bridge, whoami_call()).await;
            assert_ne!(
                reply.status, 200,
                "the only address the bridge knows now answers as another installation: {}",
                reply.body
            );
            let said = reply.error_message();
            for (whose, id) in [("pinned", pinned_id), ("seen", other_id)] {
                assert!(
                    said.contains(&fingerprint(&id)),
                    "the refusal names both fingerprints so the operator can compare them with \
                     the host's; the {whose} one ({}) is missing from: {said}",
                    fingerprint(&id)
                );
            }
            assert!(
                said.contains("bridge pair"),
                "and says the one command that fixes it: {said}"
            );

            let heard = relay.heard();
            assert!(
                heard.contains("get /identity"),
                "anchor: the relay saw the bridge's identity question, or it was never in the \
                 path and its silence below means nothing. Heard: {heard}"
            );
            for secret_thing in [
                "authorization",
                "bearer",
                "mi1.",
                "tools/call",
                "cuba_whoami",
            ] {
                assert!(
                    !heard.contains(secret_thing),
                    "something that is not the pinned host answered, and {secret_thing:?} reached \
                     it: the bridge sent a credential (or a call) to an unverified address. \
                     Heard: {heard}"
                );
            }
            assert_eq!(
                std::fs::read(scratch.state_path()).expect("bridge.json"),
                before,
                "a refused host never rewrites bridge.json: a different installation is not \
                 accepted by itself, the operator re-pairs on purpose"
            );
            drop(bridge);
            other_host.stop().await;
            pinned_host.stop().await;
            other_pool.close().await;
        })
        .await;
        pinned_pool.close().await;
    })
    .await;
}

// =============================================================================================
// J5 · REQUIRE_ACCOUNTS
// =============================================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j5_the_knob_closes_the_shared_token_and_leaves_the_bridge_in() {
    in_a_scratch_database("brain_j_knob", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let closed = start_daemon(
            EVERY_INTERFACE,
            J5_KNOB_ON,
            &pool,
            &[("MEMORY_INDUSTRY_REQUIRE_ACCOUNTS", Some("1"))],
        )
        .await;
        let open = start_daemon(EVERY_INTERFACE, J5_KNOB_OFF, &pool, &[]).await;
        let scratch = Scratch::new();
        let (closed_path, open_path) = (scratch.0.join("on.json"), scratch.0.join("off.json"));
        save(
            &closed_path,
            &paired_state(&pool, J5_KNOB_ON, "ana", "pc-on").await,
        );
        save(
            &open_path,
            &paired_state(&pool, J5_KNOB_OFF, "ana", "pc-off").await,
        );
        let clock = JumpClock::new();

        let shared = to_the_daemon(J5_KNOB_ON, Some(SHARED), ping()).await;
        assert_eq!(
            shared.status, 401,
            "with the knob on, a bind that others can reach does not take the shared token: {}",
            shared.body
        );
        assert_eq!(
            shared.error_message(),
            REQUIRE_ACCOUNTS_REFUSAL,
            "the refusal is the exact text that tells a 0.28 client what to do, not the generic \
             'invalid bearer token' that sends the operator hunting for a typo"
        );
        let bridge_on = open_bridge(&closed_path, &clock, Some(0)).await;
        assert_named(
            &through_the_bridge(&bridge_on, whoami_call()).await,
            "pc-on",
            "the bridge carries an account credential, which the closed daemon admits",
        );

        let shared = to_the_daemon(J5_KNOB_OFF, Some(SHARED), ping()).await;
        assert_eq!(
            shared.status, 200,
            "control: the same daemon with the knob off still takes the shared token: {}",
            shared.body
        );
        let bridge_off = open_bridge(&open_path, &clock, Some(0)).await;
        assert_named(
            &through_the_bridge(&bridge_off, whoami_call()).await,
            "pc-off",
            "and the bridge enters the open daemon too: the knob closes a door, not both",
        );
        drop((bridge_on, bridge_off));
        closed.stop().await;
        open.stop().await;
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// J6 · the bridge restarts from its file
// =============================================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j6_a_restarted_bridge_serves_the_same_call_from_its_file_on_the_same_port() {
    in_a_scratch_database("brain_j_restart", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let daemon = start_daemon(LOOPBACK, J6_PORT, &pool, &[]).await;
        let scratch = Scratch::new();

        let free = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
        let port = free.local_addr().expect("its address").port();
        drop(free);
        let mut state = paired_state(&pool, J6_PORT, "ana", "pc-lab-1").await;
        state.port = port;
        save(&scratch.state_path(), &state);
        let file = std::fs::read(scratch.state_path()).expect("bridge.json");

        let first = open_bridge(&scratch.state_path(), &JumpClock::new(), None).await;
        assert_eq!(
            first.local_addr().port(),
            port,
            "the bridge listens where its file says"
        );
        assert_named(
            &through_the_bridge(&first, whoami_call()).await,
            "pc-lab-1",
            "control: the first run serves the call",
        );
        first.shutdown().await;
        assert!(
            refuses_connections(addr_of(port)).await,
            "anchor: the first bridge must really be down, or the second run proves nothing"
        );

        let second = open_bridge(&scratch.state_path(), &JumpClock::new(), None).await;
        assert_eq!(
            second.local_addr().port(),
            port,
            "the restarted bridge is on the port the client config already names"
        );
        assert_named(
            &through_the_bridge(&second, whoami_call()).await,
            "pc-lab-1",
            "a bridge that starts from bridge.json alone serves the same call: no pairing code, \
             no change to the client",
        );
        assert_eq!(
            std::fs::read(scratch.state_path()).expect("bridge.json"),
            file,
            "restarting rewrites nothing: the host did not move, so there is nothing to remember"
        );
        drop(second);
        daemon.stop().await;
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// J7 · the two halves of the protocol say the same thing
// =============================================================================================

struct RawPair {
    request: PairRequest,
    key: [u8; 32],
    nonce: [u8; 16],
    hash: [u8; 32],
}

/// A pairing request built from the shared functions, as the bridge builds it.
fn raw_pair_request(code: &str, label: &str) -> RawPair {
    let key = code_key(&normalize_code(code).expect("an issued code normalizes"));
    let nonce = random_bytes::<16>();
    let hash = Secret::generate().hash();
    let mac = pair_request_mac(&key, &nonce, label, &hash);
    RawPair {
        request: PairRequest {
            v: 1,
            nc: hex::encode(nonce),
            label: label.to_string(),
            h: hex::encode(hash),
            mac: hex::encode(mac),
        },
        key,
        nonce,
        hash,
    }
}

async fn identity_of(port: u16, query: &str) -> Value {
    let reply = send(http().get(format!("http://127.0.0.1:{port}/identity{query}"))).await;
    assert_eq!(reply.status, 200, "/identity{query}: {}", reply.body);
    reply.json()
}

async fn verdict_on(state: &BridgeState, nonce: [u8; 16]) -> Verdict {
    check_identity(
        &bridge_client(),
        addr_of(J7_PORT),
        state,
        nonce,
        CALL_BUDGET,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn j7_what_the_daemon_writes_is_what_the_bridge_reads_field_by_field() {
    in_a_scratch_database("brain_j_wire", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let host_id = db::host_identity(&pool).await.expect("host").host_id;
        let daemon = start_daemon(LOOPBACK, J7_PORT, &pool, &[]).await;

        let raw = raw_pair_request(&code_for(&pool, "ana").await, "pc-raw");
        let reply = send(
            http()
                .post(format!("http://127.0.0.1:{J7_PORT}/pair"))
                .json(&raw.request),
        )
        .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(
            sorted_keys(&reply.json()),
            [
                "account",
                "credential_id",
                "host_id",
                "host_name",
                "label",
                "mac",
                "v"
            ],
            "the pairing answer carries exactly the fields the bridge's PairResponse reads and \
             its MAC covers; a renamed one makes every bridge refuse to pair"
        );
        let response: PairResponse = serde_json::from_str(&reply.body).expect("a PairResponse");
        assert_eq!((response.v, response.host_id), (1, host_id));
        let expected = pair_response_mac(
            &raw.key,
            &raw.nonce,
            &raw.hash,
            &response.host_id,
            &response.credential_id,
            &response.account,
            &response.label,
            &response.host_name,
        );
        assert!(
            verify_mac(expected, &response.mac),
            "the daemon's MAC and the bridge's check are the same function of the same fields"
        );

        let state = paired_state(&pool, J7_PORT, "ana", "pc-lab-1").await;
        assert_eq!(
            (state.host_id, state.host_name.as_str()),
            (host_id, response.host_name.as_str()),
            "pairing through the bridge's own code pins the host id and name the daemon wrote"
        );

        let anonymous = identity_of(J7_PORT, "").await;
        assert_eq!(sorted_keys(&anonymous), ["host_id", "name", "service"]);
        assert_eq!(
            anonymous["name"],
            json!(response.host_name),
            "/identity calls the host by the same label /pair does"
        );

        let nonce = random_bytes::<16>();
        let query = format!(
            "?cred={}&nonce={}",
            state.credential_id.simple(),
            hex::encode(nonce)
        );
        let active = identity_of(J7_PORT, &query).await;
        assert_eq!(
            sorted_keys(&active),
            ["credential", "host_id", "name", "proof", "service"],
            "an active credential gets the proof and nothing else extra"
        );
        assert_eq!(
            active["proof"],
            json!(hex::encode(identity_proof(
                &state.secret.hash(),
                &host_id,
                &state.credential_id,
                &nonce
            ))),
            "the proof is the one the bridge computes from its own secret"
        );
        assert_eq!(
            verdict_on(&state, nonce).await,
            Verdict::Verified {
                fingerprint: fingerprint(&host_id)
            },
            "the bridge's own parser accepts what the daemon wrote"
        );

        db::revoke_device(&pool, "ana", "pc-raw", Utc::now())
            .await
            .expect("revoking the raw device");
        let state = BridgeState {
            credential_id: response.credential_id,
            ..state
        };
        let revoked = identity_of(
            J7_PORT,
            &format!(
                "?cred={}&nonce={}",
                state.credential_id.simple(),
                hex::encode(nonce)
            ),
        )
        .await;
        assert_eq!(
            sorted_keys(&revoked),
            ["credential", "host_id", "name", "service"],
            "a revoked credential gets no proof: its absence is how the bridge knows to stop"
        );
        assert_eq!(
            verdict_on(&state, nonce).await,
            Verdict::Revoked,
            "and the bridge reads that as revoked"
        );
        daemon.stop().await;
        pool.close().await;
    })
    .await;
}
