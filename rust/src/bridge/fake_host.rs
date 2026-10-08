//! Test support for the bridge. Nothing here is production code and nothing here touches a
//! network that is not `127.0.0.1`.
//!
//! * `FakeHost`: an in-process host that speaks `/identity`, `/pair` and `/mcp` for real.
//!   It uses the functions of `accounts.rs` (not a copy of them), so a protocol change
//!   breaks the bridge tests instead of drifting from them. It records every request.
//! * `RawRecorder`: the impostor. It records every byte it receives and answers canned HTTP.
//! * `Closer` / `Tarpit`: the dead candidates. A real listener that accepts and hangs up (or
//!   accepts and says nothing) fails fast and is the same on every OS; a port nobody
//!   listens on is slow on Windows (ci-rapido.md).
//! * A raw HTTP/1.1 client, so a test controls every header it sends (a `Connection`, a
//!   `Host`, a duplicate) and reads a streamed body piece by piece.

// A toolbox: a helper the next test needs is not dead code, and which of them the current
// tests leave unused is not something this file can check without a compiler.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::proxy::{self, BridgeHandle};
use super::state::{BridgeState, STATE_VERSION, read_state};
use super::{Bridge, BridgeDeps, Clock, Network, STATE_BASENAME, Timing};
use crate::accounts::{self, Secret};
use crate::service::Target;
use crate::service_apply::{CommandOutcome, CommandRunner};

pub(crate) const ACCOUNT: &str = "ana";
pub(crate) const DEVICE_LABEL: &str = "pc-lab-1";
pub(crate) const HOST_NAME: &str = "madre";

/// What `McpMode::Stream` sends, split in two so a test can hold the second back.
pub(crate) const CHUNK_ONE: &[u8] = b"data: one\n\n";
pub(crate) const CHUNK_TWO: &[u8] = b"data: two\n\n";

pub(crate) fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("a test mutex is never poisoned")
}

/// Milliseconds for everything, seconds only where a rule is about a window the clock
/// (not the wall) moves. A test that needs other values overrides fields.
pub(crate) fn tiny_timing() -> Timing {
    Timing {
        resolve: ms(200),
        identity: ms(300),
        probe_connect: ms(150),
        probe_total: ms(600),
        discovery_total: Duration::from_secs(3),
        request_waits: Duration::from_secs(4),
        reverify_after: Duration::from_secs(30),
        revoked_recheck: Duration::from_secs(60),
        rescan_cooldown: Duration::from_secs(60),
        probe_parallelism: 8,
        ..Timing::production()
    }
}

pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("a plain HTTP client")
}

pub(crate) fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

pub(crate) fn contains_ci(haystack: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(haystack)
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// Every run of exactly 64 hex digits in `bytes`: the shape of `h`, a MAC and a secret.
pub(crate) fn hex_runs_of_64(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split(|c: char| !c.is_ascii_hexdigit())
        .filter(|run| run.len() == 64)
        .map(str::to_string)
        .collect()
}

/// Waits (polling, bounded) for something that happens after the reply, such as a file
/// written by a background task. Panics naming what never happened.
pub(crate) async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(
            Instant::now() < deadline,
            "never happened within 3 s: {what}"
        );
        tokio::time::sleep(ms(10)).await;
    }
}

// ── scratch directory ────────────────────────────────────────────────────────────────

pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("mi-bridge-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// File names in `dir`, sorted.
pub(crate) fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("the directory exists")
        .map(|entry| {
            entry
                .expect("a readable entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

// ── a paired device, as the tests see it ─────────────────────────────────────────────

pub(crate) struct Identity {
    pub host_id: Uuid,
    pub credential_id: Uuid,
    pub secret: Secret,
}

impl Identity {
    pub fn new() -> Self {
        Self::on_host(Uuid::new_v4())
    }

    pub fn on_host(host_id: Uuid) -> Self {
        Identity {
            host_id,
            credential_id: Uuid::new_v4(),
            secret: Secret::generate(),
        }
    }

    pub fn secret_hex(&self) -> String {
        self.secret.to_hex()
    }

    pub fn bearer(&self) -> String {
        accounts::bearer(self.credential_id, &self.secret)
    }

    /// `<label>-<8 hex of the credential id>`
    pub fn machine_id(&self) -> String {
        format!(
            "{DEVICE_LABEL}-{}",
            &self.credential_id.simple().to_string()[..8]
        )
    }

    pub fn fake_cred(&self) -> FakeCred {
        FakeCred {
            id: self.credential_id,
            secret_hash: self.secret.hash(),
            status: CredStatus::Active,
        }
    }

    pub fn state(
        &self,
        host_port: u16,
        addresses: Vec<SocketAddr>,
        last_good: Option<SocketAddr>,
    ) -> BridgeState {
        BridgeState {
            version: STATE_VERSION,
            host_id: self.host_id,
            host_name: HOST_NAME.to_string(),
            account: ACCOUNT.to_string(),
            device_label: DEVICE_LABEL.to_string(),
            credential_id: self.credential_id,
            secret: Secret::from_hex(&self.secret.to_hex()).expect("a secret round-trips as hex"),
            addresses,
            last_good,
            host_port,
            port: super::DEFAULT_BRIDGE_PORT,
            paired_at: DateTime::parse_from_rfc3339("2026-10-08T12:00:00Z")
                .expect("a fixed instant")
                .with_timezone(&Utc),
        }
    }
}

/// `bridge.json` written WITHOUT the production writer, from the documented field names,
/// so a test of the reader or the proxy does not depend on the writer being right, and the
/// file format is pinned by something other than the code that produces it.
pub(crate) fn write_state_json(path: &Path, s: &BridgeState) {
    let document = json!({
        "version": s.version,
        "host_id": s.host_id,
        "host_name": s.host_name,
        "account": s.account,
        "device_label": s.device_label,
        "credential_id": s.credential_id,
        "secret": s.secret.to_hex(),
        "addresses": s.addresses.iter().map(SocketAddr::to_string).collect::<Vec<_>>(),
        "last_good": s.last_good.map(|a| a.to_string()),
        "host_port": s.host_port,
        "port": s.port,
        "paired_at": s.paired_at.to_rfc3339_opts(SecondsFormat::Secs, true),
    });
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&document).expect("a JSON document"),
    )
    .expect("the state file is written");
}

pub(crate) fn same_state(a: &BridgeState, b: &BridgeState) {
    assert_eq!(a.version, b.version, "version");
    assert_eq!(a.host_id, b.host_id, "host_id");
    assert_eq!(a.host_name, b.host_name, "host_name");
    assert_eq!(a.account, b.account, "account");
    assert_eq!(a.device_label, b.device_label, "device_label");
    assert_eq!(a.credential_id, b.credential_id, "credential_id");
    assert_eq!(a.secret.to_hex(), b.secret.to_hex(), "secret");
    assert_eq!(a.addresses, b.addresses, "addresses");
    assert_eq!(a.last_good, b.last_good, "last_good");
    assert_eq!(a.host_port, b.host_port, "host_port");
    assert_eq!(a.port, b.port, "port");
    assert_eq!(a.paired_at, b.paired_at, "paired_at");
}

// ── injected clock, network, command runner ──────────────────────────────────────────

pub(crate) struct ManualClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl ManualClock {
    pub fn new() -> Self {
        ManualClock {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    pub fn advance(&self, by: Duration) {
        *lock(&self.offset) += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.base + *lock(&self.offset)
    }
}

#[derive(Default)]
pub(crate) struct FakeNet {
    names: HashMap<String, Vec<IpAddr>>,
    own: Option<Ipv4Addr>,
    sweep: Vec<SocketAddr>,
    resolves: AtomicUsize,
    sweeps: AtomicUsize,
    asked: Mutex<Vec<String>>,
}

impl FakeNet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolving(mut self, name: &str, ips: &[IpAddr]) -> Self {
        self.names.insert(name.to_string(), ips.to_vec());
        self
    }

    pub fn own(mut self, ip: Ipv4Addr) -> Self {
        self.own = Some(ip);
        self
    }

    pub fn sweep(mut self, addrs: Vec<SocketAddr>) -> Self {
        self.sweep = addrs;
        self
    }

    pub fn resolves(&self) -> usize {
        self.resolves.load(Ordering::SeqCst)
    }

    pub fn sweeps(&self) -> usize {
        self.sweeps.load(Ordering::SeqCst)
    }

    pub fn asked(&self) -> Vec<String> {
        lock(&self.asked).clone()
    }
}

#[async_trait]
impl Network for FakeNet {
    async fn resolve(&self, name: &str) -> Vec<IpAddr> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        lock(&self.asked).push(name.to_string());
        self.names.get(name).cloned().unwrap_or_default()
    }

    fn own_ipv4_towards(&self, _peer: IpAddr) -> Option<Ipv4Addr> {
        self.own
    }

    fn sweep_targets(&self, _port: u16) -> Vec<SocketAddr> {
        self.sweeps.fetch_add(1, Ordering::SeqCst);
        self.sweep.clone()
    }
}

/// Records what the code under test asked the operating system to run, and answers from a
/// script. Looks at the file `icacls` is pointed at, at the moment it is asked.
pub(crate) struct ScriptedRunner {
    pub calls: Vec<Vec<String>>,
    /// `(exists, length)` of the first argument of `icacls`, when it was run.
    pub at_icacls: Option<(bool, u64)>,
    whoami_out: String,
    whoami_code: i32,
    icacls_code: i32,
}

impl ScriptedRunner {
    pub fn new(whoami_out: &str, whoami_code: i32, icacls_code: i32) -> Self {
        ScriptedRunner {
            calls: Vec::new(),
            at_icacls: None,
            whoami_out: whoami_out.to_string(),
            whoami_code,
            icacls_code,
        }
    }

    /// For a target that must never call anything: records, so a test can assert it did not.
    pub fn idle() -> Self {
        Self::new("", 0, 0)
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<CommandOutcome> {
        let argv: Vec<String> = std::iter::once(program.to_string())
            .chain(args.iter().cloned())
            .collect();
        self.calls.push(argv);
        let (code, stdout) = match program {
            "whoami" => (self.whoami_code, self.whoami_out.clone()),
            "icacls" => {
                self.at_icacls = args.first().map(|file| match std::fs::metadata(file) {
                    Ok(meta) => (true, meta.len()),
                    Err(_) => (false, 0),
                });
                (self.icacls_code, String::new())
            }
            _ => (0, String::new()),
        };
        Ok(CommandOutcome {
            code: Some(code),
            stdout,
            stderr: String::new(),
        })
    }
}

/// The runner of a bridge on a Linux target: it is never asked for anything.
pub(crate) struct NoRunner;

impl CommandRunner for NoRunner {
    fn run(&mut self, program: &str, _args: &[String]) -> std::io::Result<CommandOutcome> {
        panic!("a bridge on a Linux target ran `{program}`: Windows commands belong to Windows")
    }
}

pub(crate) fn deps(state_path: &Path, net: Arc<FakeNet>, clock: Arc<ManualClock>) -> BridgeDeps {
    BridgeDeps {
        state_path: state_path.to_path_buf(),
        target: Target::Linux,
        runner: Box::new(NoRunner),
        net,
        clock,
        timing: tiny_timing(),
        client: client(),
    }
}

// ── a bridge under test ──────────────────────────────────────────────────────────────

pub(crate) struct Prepared {
    pub _dir: TempDir,
    pub state_path: PathBuf,
    pub clock: Arc<ManualClock>,
    pub net: Arc<FakeNet>,
    pub bridge: Arc<Bridge>,
}

pub(crate) struct Harness {
    pub _dir: TempDir,
    pub state_path: PathBuf,
    pub clock: Arc<ManualClock>,
    pub net: Arc<FakeNet>,
    pub handle: BridgeHandle,
}

impl Harness {
    /// A paired bridge, loaded but not serving.
    pub fn prepare(state: &BridgeState, net: FakeNet) -> Prepared {
        let dir = TempDir::new("bridge");
        let state_path = dir.path().join(STATE_BASENAME);
        write_state_json(&state_path, state);
        let clock = Arc::new(ManualClock::new());
        let net = Arc::new(net);
        let bridge = Bridge::load(deps(&state_path, net.clone(), clock.clone()))
            .expect("a paired bridge loads its state");
        Prepared {
            _dir: dir,
            state_path,
            clock,
            net,
            bridge,
        }
    }

    pub async fn start(state: &BridgeState, net: FakeNet) -> Harness {
        let prepared = Self::prepare(state, net);
        let handle = proxy::start(0, prepared.bridge.clone())
            .await
            .expect("the bridge starts on a free loopback port");
        Harness {
            _dir: prepared._dir,
            state_path: prepared.state_path,
            clock: prepared.clock,
            net: prepared.net,
            handle,
        }
    }

    pub async fn send(
        &self,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Reply {
        request(self.handle.local_addr(), method, target, headers, body).await
    }

    pub async fn post_mcp(&self, body: &[u8]) -> Reply {
        self.send(
            "POST",
            "/mcp",
            &[("Content-Type", "application/json")],
            body,
        )
        .await
    }

    /// The state file as the production reader sees it now.
    pub fn stored(&self) -> Option<BridgeState> {
        read_state(&self.state_path).ok().flatten()
    }
}

// ── raw HTTP/1.1 ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct Head {
    pub first_line: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
}

impl Head {
    fn parse(raw: &[u8]) -> Head {
        let text = String::from_utf8_lossy(raw);
        let mut lines = text.split("\r\n");
        let first_line = lines.next().unwrap_or_default().to_string();
        let headers = lines
            .filter(|line| !line.is_empty())
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        Head {
            first_line,
            headers,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn content_length(&self) -> Option<usize> {
        self.header("content-length")
            .and_then(|value| value.parse().ok())
    }

    fn is_chunked(&self) -> bool {
        self.header("transfer-encoding")
            .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    }
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|at| at + 4)
}

fn crlf_at(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|window| window == b"\r\n")
}

/// A complete chunked body, or `None` while it is still arriving.
fn decode_chunked(buf: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let line_end = at + crlf_at(&buf[at..])?;
        let size_text = std::str::from_utf8(&buf[at..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        at = line_end + 2;
        if size == 0 {
            return crlf_at(&buf[at..]).map(|_| out);
        }
        if buf.len() < at + size + 2 {
            return None;
        }
        out.extend_from_slice(&buf[at..at + size]);
        at += size + 2;
    }
}

/// One whole request (head and body), appending every byte read to `sink`.
async fn read_request(stream: &mut TcpStream, sink: &Mutex<Vec<u8>>) -> Option<(Head, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut piece = [0u8; 16 * 1024];
    loop {
        if let Some(end) = head_end(&buf) {
            let head = Head::parse(&buf[..end]);
            let rest = &buf[end..];
            if head.is_chunked() {
                if let Some(body) = decode_chunked(rest) {
                    return Some((head, body));
                }
            } else {
                let wanted = head.content_length().unwrap_or(0);
                if rest.len() >= wanted {
                    return Some((head, rest[..wanted].to_vec()));
                }
            }
        }
        let n = stream.read(&mut piece).await.ok()?;
        if n == 0 {
            return None;
        }
        lock(sink).extend_from_slice(&piece[..n]);
        buf.extend_from_slice(&piece[..n]);
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}

fn json_headers() -> Vec<(String, String)> {
    vec![("Content-Type".to_string(), "application/json".to_string())]
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reason(status),
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

async fn write_chunk<W: AsyncWriteExt + Unpin>(out: &mut W, data: &[u8]) -> std::io::Result<()> {
    out.write_all(format!("{:x}\r\n", data.len()).as_bytes())
        .await?;
    out.write_all(data).await?;
    out.write_all(b"\r\n").await?;
    out.flush().await
}

// ── the raw client ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

enum BodyMode {
    Length(usize),
    Chunked,
    UntilClose,
    Done,
}

/// An open response. Dropping it closes the connection, which is how a test plays a client
/// that walks away.
pub(crate) struct ResponseReader {
    read: OwnedReadHalf,
    holder: JoinHandle<()>,
    buf: Vec<u8>,
    pub status: u16,
    pub head: Head,
    mode: BodyMode,
}

impl ResponseReader {
    /// Sends the request and returns once the response head has arrived. Without a `Host`
    /// the target address is used, without a `Content-Length` the body's, and without a
    /// `Connection` header `close`.
    pub async fn open(
        addr: SocketAddr,
        method: &str,
        target: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> ResponseReader {
        let stream = TcpStream::connect(addr)
            .await
            .expect("the server under test accepts a connection");
        let (mut read, mut write) = stream.into_split();
        let given = |name: &str| {
            headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case(name))
        };
        let mut payload = format!("{method} {target} HTTP/1.1\r\n").into_bytes();
        if !given("host") {
            payload.extend_from_slice(format!("Host: {addr}\r\n").as_bytes());
        }
        if !given("content-length") {
            payload.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        if !given("connection") {
            payload.extend_from_slice(b"Connection: close\r\n");
        }
        for (name, value) in headers {
            payload.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        payload.extend_from_slice(b"\r\n");
        payload.extend_from_slice(body);
        // The write half is kept open until the reader is dropped: an HTTP/1.1 server may
        // treat a half-closed connection as a client that left, and cancel its answer.
        let holder = tokio::spawn(async move {
            let _ = write.write_all(&payload).await;
            std::future::pending::<()>().await;
        });

        let mut buf = Vec::new();
        let end = loop {
            if let Some(end) = head_end(&buf) {
                break end;
            }
            let mut piece = [0u8; 8192];
            let n = tokio::time::timeout(Duration::from_secs(15), read.read(&mut piece))
                .await
                .expect("the server answered within 15 s")
                .expect("the response head could be read");
            assert!(n > 0, "the server closed the connection before it answered");
            buf.extend_from_slice(&piece[..n]);
        };
        let head = Head::parse(&buf[..end]);
        buf.drain(..end);
        let status: u16 = head
            .first_line
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("a status line");
        let mode = if status == 204 || status == 304 || status < 200 {
            BodyMode::Length(0)
        } else if head.is_chunked() {
            BodyMode::Chunked
        } else if let Some(length) = head.content_length() {
            BodyMode::Length(length)
        } else {
            BodyMode::UntilClose
        };
        ResponseReader {
            read,
            holder,
            buf,
            status,
            head,
            mode,
        }
    }

    async fn fill(&mut self) -> bool {
        let mut piece = [0u8; 16 * 1024];
        let n = tokio::time::timeout(Duration::from_secs(15), self.read.read(&mut piece))
            .await
            .expect("the next piece of the body arrived within 15 s")
            .unwrap_or(0);
        self.buf.extend_from_slice(&piece[..n]);
        n > 0
    }

    /// `Some(Some(data))` a chunk, `Some(None)` the end, `None` not enough bytes yet.
    fn take_chunk(&mut self) -> Option<Option<Vec<u8>>> {
        let line_end = crlf_at(&self.buf)?;
        let size_text = std::str::from_utf8(&self.buf[..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        if size == 0 {
            if self.buf.len() < line_end + 4 {
                return None;
            }
            self.buf.drain(..line_end + 4);
            self.mode = BodyMode::Done;
            return Some(None);
        }
        let total = line_end + 2 + size + 2;
        if self.buf.len() < total {
            return None;
        }
        let data = self.buf[line_end + 2..line_end + 2 + size].to_vec();
        self.buf.drain(..total);
        Some(Some(data))
    }

    /// The next piece of the decoded body, or `None` at its end.
    pub async fn next_data(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.mode {
                BodyMode::Done => return None,
                BodyMode::Length(0) => {
                    self.mode = BodyMode::Done;
                    return None;
                }
                BodyMode::Length(left) => {
                    if !self.buf.is_empty() {
                        let take = left.min(self.buf.len());
                        let piece: Vec<u8> = self.buf.drain(..take).collect();
                        self.mode = BodyMode::Length(left - take);
                        return Some(piece);
                    }
                }
                BodyMode::Chunked => {
                    if let Some(chunk) = self.take_chunk() {
                        return chunk;
                    }
                }
                BodyMode::UntilClose => {
                    if !self.buf.is_empty() {
                        return Some(std::mem::take(&mut self.buf));
                    }
                }
            }
            if !self.fill().await {
                if matches!(self.mode, BodyMode::UntilClose) {
                    self.mode = BodyMode::Done;
                    return None;
                }
                panic!("the connection closed in the middle of the response body");
            }
        }
    }

    pub async fn read_all(&mut self) -> Vec<u8> {
        let mut body = Vec::new();
        while let Some(piece) = self.next_data().await {
            body.extend_from_slice(&piece);
        }
        body
    }
}

impl Drop for ResponseReader {
    fn drop(&mut self) {
        self.holder.abort();
    }
}

pub(crate) async fn request(
    addr: SocketAddr,
    method: &str,
    target: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Reply {
    let mut reader = ResponseReader::open(addr, method, target, headers, body).await;
    let body = reader.read_all().await;
    Reply {
        status: reader.status,
        headers: reader.head.headers.clone(),
        body,
    }
}

// ── the fake host ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub method: String,
    /// Path and query, as sent.
    pub target: String,
    /// Names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Every value of a header, in the order received.
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    pub fn query_param(&self, name: &str) -> Option<String> {
        let (_, query) = self.target.split_once('?')?;
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredStatus {
    Active,
    Revoked,
}

#[derive(Debug, Clone)]
pub(crate) struct FakeCred {
    pub id: Uuid,
    pub secret_hash: [u8; 32],
    pub status: CredStatus,
}

struct FakeCode {
    key: [u8; 32],
    account: String,
    label: Option<String>,
    /// The credential minted for it and the `h` it was minted for: a second redemption with
    /// the same `h` is a replay, with another it is refused.
    used_with: Option<(Uuid, [u8; 32])>,
}

#[derive(Debug, Clone)]
pub(crate) struct Issued {
    pub credential_id: Uuid,
    pub h: [u8; 32],
    pub account: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub(crate) enum PairMode {
    /// Verify the request MAC against the configured codes, like the daemon.
    Real,
    /// Answer 401 with this body whatever was asked.
    Refuse(String),
    /// Answer 429 with this `Retry-After` in seconds.
    Brake(u64),
}

#[derive(Debug, Clone)]
pub(crate) enum McpMode {
    Reply {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// 200 with `CHUNK_ONE` at once and `CHUNK_TWO` only after `release_stream()`.
    Stream,
    /// Read the whole request, then hang up without answering.
    ReadAndClose,
}

impl McpMode {
    pub fn reply(status: u16, headers: &[(&str, &str)], body: &[u8]) -> McpMode {
        McpMode::Reply {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            body: body.to_vec(),
        }
    }
}

struct Inner {
    creds: HashMap<Uuid, FakeCred>,
    codes: Vec<FakeCode>,
    pair: PairMode,
    mcp: McpMode,
    issued: Vec<Issued>,
}

struct Shared {
    host_id: Uuid,
    inner: Mutex<Inner>,
    seen: Mutex<Vec<Seen>>,
    received: Mutex<Vec<u8>>,
    accepted: AtomicUsize,
    gate: Notify,
    stream_closed: AtomicBool,
}

pub(crate) struct FakeHost {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
}

impl FakeHost {
    pub async fn start(host_id: Uuid, creds: Vec<FakeCred>) -> FakeHost {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port for the fake host");
        let addr = listener.local_addr().expect("the bound address");
        let shared = Arc::new(Shared {
            host_id,
            inner: Mutex::new(Inner {
                creds: creds.into_iter().map(|c| (c.id, c)).collect(),
                codes: Vec::new(),
                pair: PairMode::Real,
                mcp: McpMode::reply(
                    200,
                    &[("Content-Type", "application/json")],
                    br#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
                ),
                issued: Vec::new(),
            }),
            seen: Mutex::new(Vec::new()),
            received: Mutex::new(Vec::new()),
            accepted: AtomicUsize::new(0),
            gate: Notify::new(),
            stream_closed: AtomicBool::new(false),
        });
        let task = tokio::spawn(accept_loop(listener, shared.clone()));
        FakeHost {
            addr,
            shared,
            task: Some(task),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn host_id(&self) -> Uuid {
        self.shared.host_id
    }

    pub fn name(&self) -> &'static str {
        HOST_NAME
    }

    /// Stops listening. The port is then closed, which is what a host that went away looks
    /// like to the next connection.
    pub async fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }

    /// Makes `code` redeemable for `account`; `label` is the device label the host assigns.
    pub fn add_code(&self, code: &str, account: &str, label: Option<&str>) {
        let key = accounts::code_key(&accounts::normalize_code(code).expect("a valid test code"));
        lock(&self.shared.inner).codes.push(FakeCode {
            key,
            account: account.to_string(),
            label: label.map(str::to_string),
            used_with: None,
        });
    }

    pub fn set_pair(&self, mode: PairMode) {
        lock(&self.shared.inner).pair = mode;
    }

    pub fn set_mcp(&self, mode: McpMode) {
        lock(&self.shared.inner).mcp = mode;
    }

    pub fn set_cred_status(&self, id: Uuid, status: CredStatus) {
        if let Some(cred) = lock(&self.shared.inner).creds.get_mut(&id) {
            cred.status = status;
        }
    }

    pub fn release_stream(&self) {
        self.shared.gate.notify_one();
    }

    /// True once the host saw the client of a stream hang up.
    pub async fn wait_stream_closed(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.shared.stream_closed.load(Ordering::SeqCst) {
                return true;
            }
            tokio::time::sleep(ms(10)).await;
        }
        self.shared.stream_closed.load(Ordering::SeqCst)
    }

    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.shared.seen).clone()
    }

    fn seen_at(&self, path: &str) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path() == path)
            .collect()
    }

    pub fn identity_requests(&self) -> Vec<Seen> {
        self.seen_at("/identity")
    }

    pub fn pair_requests(&self) -> Vec<Seen> {
        self.seen_at("/pair")
    }

    pub fn mcp_requests(&self) -> Vec<Seen> {
        self.seen_at("/mcp")
    }

    /// Every byte the host read from any connection.
    pub fn received_bytes(&self) -> Vec<u8> {
        lock(&self.shared.received).clone()
    }

    pub fn accepted(&self) -> usize {
        self.shared.accepted.load(Ordering::SeqCst)
    }

    pub fn issued(&self) -> Vec<Issued> {
        lock(&self.shared.inner).issued.clone()
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while let Ok((stream, _)) = listener.accept().await {
        shared.accepted.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(serve_connection(stream, shared.clone()));
    }
}

async fn serve_connection(mut stream: TcpStream, shared: Arc<Shared>) {
    let Some((head, body)) = read_request(&mut stream, &shared.received).await else {
        return;
    };
    let mut parts = head.first_line.split(' ');
    let seen = Seen {
        method: parts.next().unwrap_or_default().to_string(),
        target: parts.next().unwrap_or_default().to_string(),
        headers: head.headers.clone(),
        body,
    };
    lock(&shared.seen).push(seen.clone());
    match (seen.method.as_str(), seen.path()) {
        ("GET", "/identity") => answer_identity(&mut stream, &shared, &seen).await,
        ("POST", "/pair") => answer_pair(&mut stream, &shared, &seen).await,
        (_, "/mcp") => answer_mcp(&mut stream, &shared).await,
        _ => write_response(&mut stream, 404, &[], b"not found").await,
    }
}

async fn json_response(stream: &mut TcpStream, status: u16, document: &serde_json::Value) {
    write_response(
        stream,
        status,
        &json_headers(),
        document.to_string().as_bytes(),
    )
    .await;
}

fn hex_array<const N: usize>(text: &str) -> Option<[u8; N]> {
    hex::decode(text).ok()?.try_into().ok()
}

async fn answer_identity(stream: &mut TcpStream, shared: &Shared, seen: &Seen) {
    let mut reply = json!({
        "service": "memory-industry",
        "host_id": shared.host_id,
        "name": HOST_NAME,
    });
    if let Some(cred) = seen.query_param("cred") {
        let found = Uuid::parse_str(&cred)
            .ok()
            .and_then(|id| lock(&shared.inner).creds.get(&id).cloned());
        match found {
            None => reply["credential"] = json!("unknown"),
            Some(c) if c.status == CredStatus::Revoked => reply["credential"] = json!("revoked"),
            Some(c) => {
                reply["credential"] = json!("active");
                let nonce = seen.query_param("nonce").and_then(|n| hex_array::<16>(&n));
                if let Some(nonce) = nonce {
                    let proof =
                        accounts::identity_proof(&c.secret_hash, &shared.host_id, &c.id, &nonce);
                    reply["proof"] = json!(hex::encode(proof));
                }
            }
        }
    }
    json_response(stream, 200, &reply).await;
}

/// The daemon's redemption, reduced: the MAC picks the code, a code is used once, the same
/// `h` again is a replay, anything else is the same refusal.
fn redeem(shared: &Shared, raw: &[u8]) -> Result<accounts::PairResponse, u16> {
    let req: accounts::PairRequest = serde_json::from_slice(raw).map_err(|_| 400u16)?;
    let nc = hex_array::<16>(&req.nc).ok_or(400u16)?;
    let h = hex_array::<32>(&req.h).ok_or(400u16)?;
    let mut guard = lock(&shared.inner);
    let inner = &mut *guard;
    let matched = inner
        .codes
        .iter()
        .position(|c| {
            accounts::verify_mac(
                accounts::pair_request_mac(&c.key, &nc, &req.label, &h),
                &req.mac,
            )
        })
        .ok_or(401u16)?;
    let code = &mut inner.codes[matched];
    let earlier = code.used_with;
    let credential_id = match earlier {
        None => {
            let id = Uuid::new_v4();
            code.used_with = Some((id, h));
            id
        }
        Some((id, h_then)) if h_then == h => id,
        Some(_) => return Err(401),
    };
    let key = code.key;
    let account = code.account.clone();
    let label = code.label.clone().unwrap_or_else(|| req.label.clone());
    inner.creds.insert(
        credential_id,
        FakeCred {
            id: credential_id,
            secret_hash: h,
            status: CredStatus::Active,
        },
    );
    if !inner
        .issued
        .iter()
        .any(|i| i.credential_id == credential_id)
    {
        inner.issued.push(Issued {
            credential_id,
            h,
            account: account.clone(),
            label: label.clone(),
        });
    }
    let mac = accounts::pair_response_mac(
        &key,
        &nc,
        &h,
        &shared.host_id,
        &credential_id,
        &account,
        &label,
        HOST_NAME,
    );
    Ok(accounts::PairResponse {
        v: 1,
        host_id: shared.host_id,
        host_name: HOST_NAME.to_string(),
        credential_id,
        account,
        label,
        mac: hex::encode(mac),
    })
}

async fn answer_pair(stream: &mut TcpStream, shared: &Shared, seen: &Seen) {
    let mode = lock(&shared.inner).pair.clone();
    match mode {
        PairMode::Refuse(body) => {
            write_response(stream, 401, &json_headers(), body.as_bytes()).await;
        }
        PairMode::Brake(seconds) => {
            let headers = [("Retry-After".to_string(), seconds.to_string())];
            write_response(stream, 429, &headers, br#"{"error":"slow down"}"#).await;
        }
        PairMode::Real => match redeem(shared, &seen.body) {
            Ok(response) => {
                let document = serde_json::to_value(&response).expect("a PairResponse is JSON");
                json_response(stream, 200, &document).await;
            }
            Err(400) => json_response(stream, 400, &json!({"error": "bad request"})).await,
            Err(_) => json_response(stream, 401, &json!({"error": "pairing failed"})).await,
        },
    }
}

async fn answer_mcp(stream: &mut TcpStream, shared: &Shared) {
    let mode = lock(&shared.inner).mcp.clone();
    match mode {
        McpMode::Reply {
            status,
            headers,
            body,
        } => write_response(stream, status, &headers, &body).await,
        McpMode::ReadAndClose => {}
        McpMode::Stream => stream_in_two(stream, shared).await,
    }
}

async fn stream_in_two(stream: &mut TcpStream, shared: &Shared) {
    let (mut read, mut write) = stream.split();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let started = write.write_all(head.as_bytes()).await.is_ok()
        && write_chunk(&mut write, CHUNK_ONE).await.is_ok();
    if !started {
        shared.stream_closed.store(true, Ordering::SeqCst);
        return;
    }
    let mut scratch = [0u8; 64];
    loop {
        tokio::select! {
            _ = shared.gate.notified() => {
                let finished = write_chunk(&mut write, CHUNK_TWO).await.is_ok()
                    && write.write_all(b"0\r\n\r\n").await.is_ok();
                if !finished {
                    shared.stream_closed.store(true, Ordering::SeqCst);
                }
                let _ = write.shutdown().await;
                return;
            }
            heard = read.read(&mut scratch) => {
                if matches!(heard, Ok(0) | Err(_)) {
                    shared.stream_closed.store(true, Ordering::SeqCst);
                    return;
                }
            }
        }
    }
}

// ── the impostor and the dead candidates ─────────────────────────────────────────────

/// Records every byte it receives and answers `200` with the body of the first route whose
/// prefix starts the request line (`""` matches everything), `404` otherwise.
pub(crate) struct RawRecorder {
    addr: SocketAddr,
    received: Arc<Mutex<Vec<u8>>>,
    lines: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl RawRecorder {
    pub async fn start(routes: &[(&str, String)]) -> RawRecorder {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port for the recorder");
        let addr = listener.local_addr().expect("the bound address");
        let received = Arc::new(Mutex::new(Vec::new()));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Vec<(String, String)>> = Arc::new(
            routes
                .iter()
                .map(|(prefix, body)| (prefix.to_string(), body.clone()))
                .collect(),
        );
        let task = tokio::spawn(record_loop(
            listener,
            routes,
            received.clone(),
            lines.clone(),
        ));
        RawRecorder {
            addr,
            received,
            lines,
            task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Everything the recorder was sent.
    pub fn received(&self) -> Vec<u8> {
        lock(&self.received).clone()
    }

    /// The request line of every request it was sent.
    pub fn lines(&self) -> Vec<String> {
        lock(&self.lines).clone()
    }
}

impl Drop for RawRecorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn record_loop(
    listener: TcpListener,
    routes: Arc<Vec<(String, String)>>,
    received: Arc<Mutex<Vec<u8>>>,
    lines: Arc<Mutex<Vec<String>>>,
) {
    while let Ok((mut stream, _)) = listener.accept().await {
        let (routes, received, lines) = (routes.clone(), received.clone(), lines.clone());
        tokio::spawn(async move {
            let Some((head, _body)) = read_request(&mut stream, &received).await else {
                return;
            };
            lock(&lines).push(head.first_line.clone());
            let answer = routes
                .iter()
                .find(|(prefix, _)| head.first_line.starts_with(prefix.as_str()));
            match answer {
                Some((_, body)) => {
                    write_response(&mut stream, 200, &json_headers(), body.as_bytes()).await;
                }
                None => write_response(&mut stream, 404, &[], b"").await,
            }
        });
    }
}

/// Accepts and hangs up at once.
pub(crate) struct Closer {
    addr: SocketAddr,
    accepted: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Closer {
    pub async fn start() -> Closer {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port for the closer");
        let addr = listener.local_addr().expect("the bound address");
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        Closer {
            addr,
            accepted,
            task,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for Closer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// When each connection to a set of tarpits opened and closed.
#[derive(Default)]
pub(crate) struct ConnLog {
    spans: Mutex<Vec<(Instant, Option<Instant>)>>,
}

impl ConnLog {
    fn open(&self) -> usize {
        let mut spans = lock(&self.spans);
        spans.push((Instant::now(), None));
        spans.len() - 1
    }

    fn close(&self, index: usize) {
        lock(&self.spans)[index].1 = Some(Instant::now());
    }

    /// The most connections open at once, ignoring the last `slack` of each: a client that
    /// drops one probe and starts the next in the same instant is not two at once, and the
    /// server cannot order the FIN against the SYN to the microsecond.
    pub fn max_overlap(&self, slack: Duration) -> usize {
        let now = Instant::now();
        let mut edges: Vec<(Instant, i32)> = Vec::new();
        for (start, end) in lock(&self.spans).iter() {
            let end = end.unwrap_or(now);
            if let Some(shrunk) = end.checked_sub(slack)
                && shrunk > *start
            {
                edges.push((*start, 1));
                edges.push((shrunk, -1));
            }
        }
        edges.sort();
        let (mut live, mut peak) = (0i32, 0i32);
        for (_, step) in edges {
            live += step;
            peak = peak.max(live);
        }
        usize::try_from(peak).unwrap_or_default()
    }
}

/// Accepts and says nothing until the other side leaves.
pub(crate) struct Tarpit {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Tarpit {
    pub async fn start() -> Tarpit {
        Self::start_logged(Arc::new(ConnLog::default())).await
    }

    async fn start_logged(log: Arc<ConnLog>) -> Tarpit {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port for the tarpit");
        let addr = listener.local_addr().expect("the bound address");
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let log = log.clone();
                tokio::spawn(async move {
                    let index = log.open();
                    let mut scratch = [0u8; 1024];
                    while matches!(stream.read(&mut scratch).await, Ok(n) if n > 0) {}
                    log.close(index);
                });
            }
        });
        Tarpit { addr, task }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for Tarpit {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Many tarpits that share one connection log.
pub(crate) struct Tarpits {
    pub addrs: Vec<SocketAddr>,
    pub log: Arc<ConnLog>,
    _tarpits: Vec<Tarpit>,
}

impl Tarpits {
    pub async fn start_many(count: usize) -> Tarpits {
        let log = Arc::new(ConnLog::default());
        let mut tarpits = Vec::new();
        for _ in 0..count {
            tarpits.push(Tarpit::start_logged(log.clone()).await);
        }
        Tarpits {
            addrs: tarpits.iter().map(Tarpit::addr).collect(),
            log,
            _tarpits: tarpits,
        }
    }
}
