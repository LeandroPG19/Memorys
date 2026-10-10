//! `memory-industry bridge …`: the client's side, a local bridge that finds the host again
//! and proves who it is before it sends a credential.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use self::discovery::{Discovery, Failure, Freshness, discover, subnet_candidates};
use self::pair::pair;
use self::state::{BridgeState, read_state, state_dir, write_state};
use crate::accounts;
use crate::service::Target;
use crate::service_apply::CommandRunner;
use crate::setup_agent::SystemRunner;

pub mod client_config;
pub mod discovery;
pub mod doctor;
pub mod install;
pub mod notify;
pub mod pair;
pub mod proxy;
pub mod state;
pub mod supervisor;

#[cfg(test)]
pub(crate) mod fake_host;

const USAGE: &str = "usage: memory-industry bridge <pair|run|status|doctor|install|uninstall> …\n\n\
Run on a client PC. The bridge listens on 127.0.0.1 only and holds the credential.";

pub const DEFAULT_BRIDGE_PORT: u16 = 8797;
pub const DEFAULT_HOST_PORT: u16 = 8787;
pub const STATE_BASENAME: &str = "bridge.json";
pub const BRIDGE_TASK_NAME: &str = "MemoryIndustry Bridge";

const NOT_PAIRED: &str =
    "no está emparejado: ejecuta bridge pair <código> (el código te lo da el operador de la madre)";

/// Private-range addresses used only to ask the OS which interface leads to a LAN: a UDP
/// `connect` sends nothing.
const SWEEP_ANCHORS: [Ipv4Addr; 3] = [
    Ipv4Addr::new(192, 168, 0, 1),
    Ipv4Addr::new(10, 0, 0, 1),
    Ipv4Addr::new(172, 16, 0, 1),
];

/// Every wait the bridge makes, named, so a test can shrink them to milliseconds and
/// production can say in one place how long it is willing to wait for a LAN.
#[derive(Debug, Clone)]
pub struct Timing {
    pub resolve: Duration,
    pub identity: Duration,
    pub probe_connect: Duration,
    pub probe_total: Duration,
    pub discovery_total: Duration,
    pub request_waits: Duration,
    pub reverify_after: Duration,
    pub revoked_recheck: Duration,
    pub rescan_cooldown: Duration,
    pub upstream_headers: Duration,
    pub probe_parallelism: usize,
}

impl Timing {
    pub fn production() -> Self {
        Timing {
            resolve: Duration::from_secs(3),
            identity: Duration::from_secs(2),
            probe_connect: Duration::from_millis(400),
            probe_total: Duration::from_secs(10),
            discovery_total: Duration::from_secs(20),
            request_waits: Duration::from_secs(10),
            reverify_after: Duration::from_secs(30),
            revoked_recheck: Duration::from_secs(60),
            rescan_cooldown: Duration::from_secs(60),
            upstream_headers: Duration::from_secs(600),
            probe_parallelism: 32,
        }
    }
}

/// The monotonic clock the freshness, cooldown and recheck rules read. Injected so a
/// test moves it by a minute without sleeping for one.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The questions the bridge asks the operating system. Injected so that no test resolves
/// a name, or learns the interface address, of the machine it runs on.
// clippy 1.99 reads the `#[must_use]` that `async_trait` puts on every boxed
// future as a second, message-less one on `resolve`. The expansion is not ours to
// edit, and dropping `async_trait` would change the trait object the bridge and its
// fakes share.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Network: Send + Sync {
    async fn resolve(&self, name: &str) -> Vec<IpAddr>;

    fn own_ipv4_towards(&self, peer: IpAddr) -> Option<Ipv4Addr>;

    /// The addresses a sweep would try: the own /24 on `port`, nothing when the own
    /// address cannot be learned. A fake overrides it with loopback listeners.
    fn sweep_targets(&self, port: u16) -> Vec<SocketAddr> {
        SWEEP_ANCHORS
            .iter()
            .find_map(|anchor| self.own_ipv4_towards(IpAddr::V4(*anchor)))
            .map(|own| subnet_candidates(own, port))
            .unwrap_or_default()
    }
}

/// The real resolver and the real interface table. The caller bounds `resolve` with
/// `Timing::resolve`.
pub struct SystemNetwork;

#[async_trait]
impl Network for SystemNetwork {
    async fn resolve(&self, name: &str) -> Vec<IpAddr> {
        let Ok(found) = tokio::net::lookup_host((name, 0)).await else {
            return Vec::new();
        };
        let mut ips: Vec<IpAddr> = Vec::new();
        for addr in found {
            if !ips.contains(&addr.ip()) {
                ips.push(addr.ip());
            }
        }
        ips.sort_by_key(IpAddr::is_ipv6);
        ips
    }

    fn own_ipv4_towards(&self, peer: IpAddr) -> Option<Ipv4Addr> {
        let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        socket.connect((peer, 9)).ok()?;
        usable_source(socket.local_addr().ok()?.ip())
    }
}

/// The source address the OS picked for a connected socket, when a sweep can start from it:
/// `0.0.0.0` has no subnet to sweep, and an IPv6 source has no IPv4 /24.
fn usable_source(source: IpAddr) -> Option<Ipv4Addr> {
    match source {
        IpAddr::V4(own) if !own.is_unspecified() => Some(own),
        _ => None,
    }
}

/// Everything the bridge touches outside itself, in one place.
pub struct BridgeDeps {
    pub state_path: PathBuf,
    pub target: Target,
    pub runner: Box<dyn CommandRunner + Send>,
    pub net: Arc<dyn Network>,
    pub clock: Arc<dyn Clock>,
    pub timing: Timing,
    pub client: reqwest::Client,
}

/// Why a request got no answer from the host, as the caller is told.
#[derive(Debug, Clone)]
pub(crate) enum Refusal {
    Revoked,
    Unknown,
    WrongHost { pinned: String, seen: String },
    Impostor(String),
    NotFound(String),
    Busy,
    MayHaveArrived(String),
}

impl Refusal {
    pub(crate) fn status(&self) -> u16 {
        match self {
            Refusal::Revoked | Refusal::Unknown => 401,
            Refusal::Busy => 503,
            _ => 502,
        }
    }

    fn is_revocation(&self) -> bool {
        matches!(self, Refusal::Revoked | Refusal::Unknown)
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Refusal::Revoked => "credencial revocada: empareja de nuevo con \
                `memory-industry bridge pair <código>` (pide un código nuevo al operador de la madre)"
                .to_string(),
            Refusal::Unknown => "la madre no conoce esta credencial: empareja de nuevo con \
                `memory-industry bridge pair <código>`"
                .to_string(),
            Refusal::WrongHost { pinned, seen } => format!(
                "la madre cambió de huella: la fijada es {pinned} y la vista es {seen}. No se \
                 acepta sola; si el cambio es legítimo, ejecuta `memory-industry bridge pair <código>`"
            ),
            Refusal::Impostor(tried) => format!(
                "algo contestó pero no pudo probar que es la madre (no se le envió ninguna \
                 credencial). Probado: {tried}"
            ),
            Refusal::NotFound(tried) if tried.is_empty() => {
                "no se encontró la madre: no hay ninguna dirección que probar".to_string()
            }
            Refusal::NotFound(tried) => format!("no se encontró la madre. Probado: {tried}"),
            Refusal::Busy => {
                "el puente sigue buscando a la madre; reintenta en unos segundos".to_string()
            }
            Refusal::MayHaveArrived(why) => {
                format!("{why}; puede haberse entregado; no se reintenta")
            }
        }
    }
}

fn refusal_from(found: &Discovery, state: &BridgeState) -> Refusal {
    match found.failure() {
        Some(Failure::Revoked) => Refusal::Revoked,
        Some(Failure::Unknown) => Refusal::Unknown,
        Some(Failure::WrongHost { seen }) => Refusal::WrongHost {
            pinned: accounts::fingerprint(&state.host_id),
            seen: accounts::fingerprint(&seen),
        },
        Some(Failure::Impostor) => Refusal::Impostor(found.report()),
        Some(Failure::NotFound) | None => Refusal::NotFound(found.report()),
    }
}

/// A verified address and the state to address it with.
#[derive(Clone)]
pub(crate) struct Route {
    pub(crate) addr: SocketAddr,
    pub(crate) state: Arc<BridgeState>,
}

struct Failed {
    at: Instant,
    refusal: Refusal,
}

/// What the bridge currently believes. Held across a whole discovery, so concurrent
/// requests wait for one search instead of starting their own.
struct Core {
    state: Arc<BridgeState>,
    freshness: Freshness,
    verified: Option<SocketAddr>,
    failure: Option<Failed>,
}

impl Core {
    /// The answer that needs no search, if there is one: a failure still inside its
    /// cooldown, or a verified address still inside its window.
    fn ready(&self, now: Instant, t: &Timing) -> Option<Result<Route, Refusal>> {
        if let Some(failed) = &self.failure {
            let window = if failed.refusal.is_revocation() {
                t.revoked_recheck
            } else {
                t.rescan_cooldown
            };
            let cooling = now.saturating_duration_since(failed.at) < window;
            return cooling.then(|| Err(failed.refusal.clone()));
        }
        let addr = self.verified?;
        let fresh = !self.freshness.needs_check(now, t);
        fresh.then(|| {
            Ok(Route {
                addr,
                state: self.state.clone(),
            })
        })
    }

    fn label(&self, now: Instant, t: &Timing) -> &'static str {
        match self.ready(now, t) {
            Some(Ok(_)) => "verified",
            Some(Err(refusal)) if refusal.is_revocation() => "revoked",
            Some(Err(_)) => "unreachable",
            None => "unverified",
        }
    }
}

/// The paired bridge: its state, what it last verified, and the single-flight
/// discovery. Opaque to callers.
pub struct Bridge {
    state_path: PathBuf,
    target: Target,
    runner: Mutex<Box<dyn CommandRunner + Send>>,
    net: Arc<dyn Network>,
    clock: Arc<dyn Clock>,
    timing: Timing,
    client: reqwest::Client,
    configured_port: u16,
    core: tokio::sync::Mutex<Core>,
}

impl Bridge {
    /// Reads the state file; a missing one is "not paired".
    pub fn load(deps: BridgeDeps) -> Result<Arc<Bridge>> {
        let state = read_state(&deps.state_path)?.context(NOT_PAIRED)?;
        Ok(Arc::new(Bridge {
            configured_port: state.port,
            core: tokio::sync::Mutex::new(Core {
                state: Arc::new(state),
                freshness: Freshness::default(),
                verified: None,
                failure: None,
            }),
            state_path: deps.state_path,
            target: deps.target,
            runner: Mutex::new(deps.runner),
            net: deps.net,
            clock: deps.clock,
            timing: deps.timing,
            client: deps.client,
        }))
    }

    /// The address to send a request to: verified, and verified recently enough. A
    /// request that finds none waits for the one search that is running, or starts it.
    pub(crate) async fn route(&self) -> Result<Route, Refusal> {
        let waiting = tokio::time::timeout(self.timing.request_waits, self.core.lock()).await;
        let mut core = waiting.map_err(|_| Refusal::Busy)?;
        if let Some(ready) = core.ready(self.clock.now(), &self.timing) {
            return ready;
        }
        self.search(&mut core).await
    }

    /// The verified address is in doubt: a send failed before the host could have received
    /// it, or the host refused the credential. The next call asks `/identity` again first.
    pub(crate) async fn doubt(&self) {
        self.core.lock().await.freshness.transport_failed();
    }

    async fn search(&self, core: &mut Core) -> Result<Route, Refusal> {
        self.reload_state(core);
        let found = discover(&core.state, &*self.net, &self.timing, &self.client).await;
        let now = self.clock.now();
        if let Some(addr) = found.verified {
            self.adopt(core, addr, now);
            return Ok(Route {
                addr,
                state: core.state.clone(),
            });
        }
        let refusal = refusal_from(&found, &core.state);
        tracing::warn!(refusal = %refusal.message(), "the bridge found no host");
        core.verified = None;
        core.failure = Some(Failed {
            at: now,
            refusal: refusal.clone(),
        });
        Err(refusal)
    }

    /// A re-pair while the bridge runs replaces the file; the next search picks it up.
    fn reload_state(&self, core: &mut Core) {
        match read_state(&self.state_path) {
            Ok(Some(fresh)) => core.state = Arc::new(fresh),
            Ok(None) => {}
            Err(err) => tracing::warn!(error = %format!("{err:#}"), "bridge state not reloaded"),
        }
    }

    /// Remembers the host as verified and stores the address that worked.
    fn adopt(&self, core: &mut Core, addr: SocketAddr, now: Instant) {
        core.verified = Some(addr);
        core.failure = None;
        core.freshness.verified_at(now);
        let Some(healed) = core.state.healed(addr) else {
            return;
        };
        match self.persist(&healed) {
            Ok(()) => core.state = Arc::new(healed),
            Err(err) => tracing::warn!(error = %format!("{err:#}"), "bridge state not saved"),
        }
    }

    fn persist(&self, state: &BridgeState) -> Result<()> {
        let mut runner = self
            .runner
            .lock()
            .map_err(|_| anyhow!("the command runner is poisoned"))?;
        write_state(&self.state_path, state, self.target, &mut **runner)
    }

    /// For `/bridge/health`: no secret, no full credential id, and it never waits on a
    /// search in progress.
    pub(crate) fn health(&self) -> Value {
        let Ok(core) = self.core.try_lock() else {
            return json!({
                "paired": true, "state": "searching",
                "host_fingerprint": null, "last_good": null,
            });
        };
        json!({
            "paired": true,
            "state": core.label(self.clock.now(), &self.timing),
            "host_fingerprint": accounts::fingerprint(&core.state.host_id),
            "last_good": core.state.last_good.map(|addr| addr.to_string()),
        })
    }
}

/// `bridge run`: load the state (refusing when unpaired, before any port is opened) and
/// serve. `port` overrides the one in the state.
pub async fn run(deps: BridgeDeps, port: Option<u16>) -> Result<proxy::BridgeHandle> {
    let bridge = Bridge::load(deps)?;
    let port = port.unwrap_or(bridge.configured_port);
    proxy::start(port, bridge).await
}

fn system_deps(state_path: PathBuf) -> Result<BridgeDeps> {
    let timing = Timing::production();
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(timing.identity)
        .build()
        .context("cannot build the HTTP client")?;
    Ok(BridgeDeps {
        state_path,
        target: Target::host(),
        runner: Box::new(SystemRunner),
        net: Arc::new(SystemNetwork),
        clock: Arc::new(SystemClock),
        timing,
        client,
    })
}

pub(crate) fn default_state_path() -> Result<PathBuf> {
    let local_app_data = std::env::var("LOCALAPPDATA").ok();
    let home = crate::envs::home().ok();
    let dir = state_dir(Target::host(), local_app_data.as_deref(), home.as_deref())?;
    Ok(dir.join(STATE_BASENAME))
}

/// The machine's name as a device label: letters, digits and `._- `, at most 40.
fn default_label() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    label_from(&raw)
}

/// What a raw machine name becomes as a label. Split from `default_label` so the rule is
/// tested without touching the process environment.
fn label_from(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._- ".contains(*c))
        .take(40)
        .collect();
    let label = kept
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .trim_end();
    if label.is_empty() {
        "device".to_string()
    } else {
        label.to_string()
    }
}

#[derive(Default)]
struct PairArgs {
    code: Option<String>,
    host: Option<String>,
    device: Option<String>,
    port: Option<u16>,
}

pub(crate) fn option_value(rest: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String> {
    rest.next()
        .cloned()
        .with_context(|| format!("{flag} needs a value"))
}

impl PairArgs {
    fn parse(args: &[String]) -> Result<Self> {
        let mut wanted = PairArgs::default();
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            wanted.take(arg, &mut rest)?;
        }
        Ok(wanted)
    }

    fn take(&mut self, arg: &str, rest: &mut std::slice::Iter<'_, String>) -> Result<()> {
        match arg {
            "--host" => self.host = Some(option_value(rest, "--host")?),
            "--device" => self.device = Some(option_value(rest, "--device")?),
            "--port" => self.port = Some(parse_port(&option_value(rest, "--port")?)?),
            flag if flag.starts_with("--") => bail!("bridge pair: unknown option {flag}"),
            positional => self.set_code(positional)?,
        }
        Ok(())
    }

    fn set_code(&mut self, code: &str) -> Result<()> {
        if self.code.replace(code.to_string()).is_some() {
            bail!("bridge pair: give the pairing code once");
        }
        Ok(())
    }
}

pub(crate) fn parse_port(text: &str) -> Result<u16> {
    text.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .with_context(|| format!("{text:?} is not a port (1-65535)"))
}

/// What `memory-industry bridge …` was asked to do, parsed without touching the network or
/// the disk so the guide's commands can be judged against it.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Pair {
        code: String,
        host: Option<String>,
        device: Option<String>,
        port: Option<u16>,
    },
    Run {
        port: Option<u16>,
    },
    Status,
    Install {
        print: bool,
    },
    Uninstall,
    Doctor,
    Reconnect {
        host: Option<String>,
    },
    Help,
}

/// A subcommand that takes nothing refuses the first stray word by name: `doctor now` is not
/// what the author meant.
fn no_arguments(rest: &[String], sub: &str, command: Command) -> Result<Command> {
    match rest.first() {
        None => Ok(command),
        Some(extra) => bail!("bridge {sub} takes no arguments, got {extra}"),
    }
}

fn parse_pair(rest: &[String]) -> Result<Command> {
    let wanted = PairArgs::parse(rest)?;
    let code = wanted
        .code
        .context("bridge pair needs the pairing code: bridge pair <code>")?;
    Ok(Command::Pair {
        code,
        host: wanted.host,
        device: wanted.device,
        port: wanted.port,
    })
}

fn parse_run(rest: &[String]) -> Result<Command> {
    let port = match rest {
        [] => None,
        [flag, value] if flag == "--port" => Some(parse_port(value)?),
        _ => bail!("usage: memory-industry bridge run [--port N]"),
    };
    Ok(Command::Run { port })
}

fn parse_install(rest: &[String]) -> Result<Command> {
    match rest {
        [] => Ok(Command::Install { print: false }),
        [flag] if flag == "--print" => Ok(Command::Install { print: true }),
        [other, ..] => bail!("bridge install: unknown option {other} (the only one is --print)"),
    }
}

fn parse_subcommand(sub: &str, rest: &[String]) -> Result<Command> {
    match sub {
        "pair" => parse_pair(rest),
        "run" => parse_run(rest),
        "install" => parse_install(rest),
        "status" => no_arguments(rest, "status", Command::Status),
        "doctor" => no_arguments(rest, "doctor", Command::Doctor),
        "uninstall" => no_arguments(rest, "uninstall", Command::Uninstall),
        other => bail!("bridge {other}: unknown subcommand\n{USAGE}"),
    }
}

/// Pure on purpose: the guide's `memory-industry bridge …` lines are judged by it.
pub fn parse(args: &[String]) -> Result<Command> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        return Ok(Command::Help);
    }
    let Some((sub, rest)) = args.split_first() else {
        bail!("bridge needs a subcommand\n{USAGE}");
    };
    parse_subcommand(sub, rest)
}

async fn cli_pair(
    code: &str,
    host: Option<&str>,
    device: Option<String>,
    port: Option<u16>,
) -> Result<()> {
    let deps = system_deps(default_state_path()?)?;
    let label = device.unwrap_or_else(default_label);
    let mut paired = pair(code, host, &label, &*deps.net, &deps.timing, &deps.client).await?;
    if let Some(port) = port {
        paired.port = port;
    }
    let mut runner = SystemRunner;
    write_state(&deps.state_path, &paired, deps.target, &mut runner)?;
    println!(
        "paired with {} as {}/{}: host fingerprint {} (compare it with the one the operator \
         sees). Start the bridge with: memory-industry bridge run",
        paired.host_name,
        paired.account,
        paired.device_label,
        accounts::fingerprint(&paired.host_id)
    );
    Ok(())
}

async fn cli_run(port: Option<u16>) -> Result<()> {
    let handle = run(system_deps(default_state_path()?)?, port).await?;
    println!("bridge listening on {}", handle.local_addr());
    tokio::signal::ctrl_c()
        .await
        .context("cannot wait for Ctrl-C")?;
    handle.shutdown().await;
    Ok(())
}

fn cli_status() -> Result<()> {
    let path = default_state_path()?;
    let Some(paired) = read_state(&path)? else {
        println!("not paired: no state at {}", path.display());
        return Ok(());
    };
    println!("state:       {}", path.display());
    println!(
        "host:        {} (fingerprint {})",
        paired.host_name,
        accounts::fingerprint(&paired.host_id)
    );
    println!("account:     {}/{}", paired.account, paired.device_label);
    let last = paired
        .last_good
        .map_or_else(|| "none".to_string(), |a| a.to_string());
    println!("last good:   {last}");
    println!("local port:  {}", paired.port);
    Ok(())
}

fn cli_install(print_only: bool) -> Result<()> {
    let exe = std::env::current_exe().context("cannot resolve the path of this binary")?;
    let mut runner = SystemRunner;
    for line in install::execute_install(&mut runner, &exe, print_only)? {
        println!("{line}");
    }
    Ok(())
}

fn cli_uninstall() -> Result<()> {
    let mut runner = SystemRunner;
    for line in install::run_uninstall(&mut runner)? {
        println!("{line}");
    }
    Ok(())
}

/// `bridge reconnect [--host X]`: asks the running bridge to look again, or looks once itself
/// when none runs.
async fn cli_reconnect(_host: Option<&str>) -> Result<()> {
    unimplemented!("pasada verde: B5")
}

/// Looks for the host the way the bridge does and prints what it tried. A bridge that
/// finds nothing fails, so a script can tell.
async fn cli_doctor() -> Result<()> {
    let deps = system_deps(default_state_path()?)?;
    let paired = read_state(&deps.state_path)?.context(NOT_PAIRED)?;
    let found = discover(&paired, &*deps.net, &deps.timing, &deps.client).await;
    println!("{}", doctor::render_report(&found, Some(&paired)));
    if doctor::exit_code(&found) != 0 {
        bail!("no host was verified: see the report above");
    }
    Ok(())
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    match parse(args)? {
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::Pair {
            code,
            host,
            device,
            port,
        } => cli_pair(&code, host.as_deref(), device, port).await,
        Command::Run { port } => cli_run(port).await,
        Command::Status => cli_status(),
        Command::Install { print } => cli_install(print),
        Command::Uninstall => cli_uninstall(),
        Command::Doctor => cli_doctor().await,
        Command::Reconnect { host } => cli_reconnect(host.as_deref()).await,
    }
}

#[cfg(test)]
mod tests {
    use super::fake_host::{FakeNet, Identity, ManualClock, TempDir, deps};
    use super::*;

    /// H24
    #[tokio::test]
    async fn an_unpaired_bridge_refuses_to_run_and_opens_no_port() {
        let dir = TempDir::new("unpaired");
        let state_path = dir.path().join(STATE_BASENAME);
        assert!(
            !state_path.exists(),
            "the scenario starts with no bridge.json"
        );
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("a free loopback port");
        let port = held.local_addr().expect("its address").port();
        drop(held);

        let err = match run(
            deps(
                &state_path,
                Arc::new(FakeNet::new()),
                Arc::new(ManualClock::new()),
            ),
            Some(port),
        )
        .await
        {
            Ok(_) => panic!("an unpaired bridge must not start"),
            Err(err) => err,
        };

        let said = format!("{err:#}");
        assert!(
            said.contains("no está emparejado: ejecuta bridge pair"),
            "the operator must be told the one command that fixes it, not shown a bind or file \
             error. Got: {said}"
        );
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "an unpaired bridge opened port {port} anyway: a listener with no credential behind \
             it answers every client with an error that looks like a broken daemon"
        );
        assert!(
            !state_path.exists(),
            "refusing to run must not create a state file as a side effect"
        );
    }

    /// A bridge's core with no search behind it: the answers `ready` and `label` give
    /// without a discovery. `verified` carries the instant the address was checked.
    fn core_at(verified: Option<(SocketAddr, Instant)>, failure: Option<Failed>) -> Core {
        let mut freshness = Freshness::default();
        let verified = verified.map(|(addr, at)| {
            freshness.verified_at(at);
            addr
        });
        Core {
            state: Arc::new(Identity::new().state(8787, vec![], None)),
            freshness,
            verified,
            failure,
        }
    }

    fn failed(at: Instant, refusal: Refusal) -> Option<Failed> {
        Some(Failed { at, refusal })
    }

    #[test]
    fn each_refusal_says_its_status_and_whether_it_is_a_revocation() {
        assert_eq!(Refusal::Revoked.status(), 401);
        assert_eq!(Refusal::Unknown.status(), 401);
        assert_eq!(
            Refusal::Busy.status(),
            503,
            "a search still running asks the caller to retry; it does not blame the host"
        );
        assert_eq!(Refusal::NotFound(String::new()).status(), 502);
        assert_eq!(Refusal::Impostor(String::new()).status(), 502);
        assert_eq!(Refusal::MayHaveArrived(String::new()).status(), 502);
        assert!(Refusal::Revoked.is_revocation());
        assert!(Refusal::Unknown.is_revocation());
        for other in [
            Refusal::Busy,
            Refusal::NotFound(String::new()),
            Refusal::Impostor(String::new()),
            Refusal::MayHaveArrived(String::new()),
            Refusal::WrongHost {
                pinned: String::new(),
                seen: String::new(),
            },
        ] {
            assert!(!other.is_revocation(), "{other:?} is not a revocation");
        }
    }

    #[test]
    fn a_search_that_tried_no_address_says_so_and_does_not_list_nothing() {
        let nothing = Refusal::NotFound(String::new()).message();
        assert!(
            nothing.contains("no hay ninguna dirección que probar"),
            "an empty list of tried addresses is named as such. Got: {nothing}"
        );
        assert_eq!(
            Refusal::NotFound("10.0.5.20:8787".to_string()).message(),
            "no se encontró la madre. Probado: 10.0.5.20:8787",
            "a search that tried addresses lists them"
        );
    }

    #[test]
    fn a_failure_answers_until_its_window_ends_and_a_revocation_has_its_own_window() {
        let t = Timing {
            revoked_recheck: Duration::from_secs(10),
            rescan_cooldown: Duration::from_secs(60),
            ..Timing::production()
        };
        let t0 = Instant::now();

        let rescan = core_at(None, failed(t0, Refusal::NotFound(String::new())));
        assert!(
            rescan.ready(t0 + Duration::from_secs(59), &t).is_some(),
            "inside the rescan cooldown the failure is answered without a search"
        );
        assert!(
            rescan.ready(t0 + Duration::from_secs(60), &t).is_none(),
            "at the end of the cooldown the bridge searches again: the window is `<`, not `<=`"
        );

        let revoked = core_at(None, failed(t0, Refusal::Revoked));
        assert!(
            revoked.ready(t0 + Duration::from_secs(9), &t).is_some(),
            "a revoked device is answered inside its own, shorter, window"
        );
        assert!(
            revoked.ready(t0 + Duration::from_secs(10), &t).is_none(),
            "and rechecked when that window ends, not when the rescan cooldown does"
        );
    }

    #[test]
    fn the_label_names_what_the_bridge_knows_and_why_it_knows_no_more() {
        let t = Timing::production();
        let now = Instant::now();
        let addr: SocketAddr = "10.0.5.20:8787".parse().expect("an address");

        assert_eq!(core_at(Some((addr, now)), None).label(now, &t), "verified");
        assert_eq!(
            core_at(None, failed(now, Refusal::Revoked)).label(now, &t),
            "revoked",
            "a revoked credential is labelled as such, so the operator knows to pair again"
        );
        assert_eq!(
            core_at(None, failed(now, Refusal::NotFound(String::new()))).label(now, &t),
            "unreachable",
            "a host that was not found is unreachable, not revoked"
        );
        assert_eq!(core_at(None, None).label(now, &t), "unverified");
    }

    #[test]
    fn the_default_state_path_ends_in_the_bridge_file() {
        let path = default_state_path().expect("a home or local app data to keep the state in");
        assert!(
            path.ends_with(STATE_BASENAME),
            "the state lives in the bridge's own file, not in a bare directory. Got: {}",
            path.display()
        );
    }

    #[test]
    fn a_machine_name_becomes_a_label_that_keeps_its_inside_and_loses_the_rest() {
        assert_eq!(label_from("pc-02"), "pc-02", "a plain name is the label");
        assert_eq!(
            label_from("  .-PC 02 "),
            "PC 02",
            "leading punctuation and spaces go; the inside stays"
        );
        assert_eq!(
            label_from("ñ/*"),
            "device",
            "when nothing usable is left the label is the fixed word"
        );
        assert_eq!(label_from(""), "device");
        assert_eq!(
            label_from(&"x".repeat(60)).len(),
            40,
            "a label is at most 40 characters"
        );
    }

    #[tokio::test]
    async fn the_default_label_is_the_machine_name_as_labelled_and_device_without_one() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        {
            let _host = crate::envs::ScopedEnv::set("HOSTNAME", "nodo-de-respaldo");
            let _computer = crate::envs::ScopedEnv::set("COMPUTERNAME", "  caja-de-planta  ");
            assert_eq!(
                default_label(),
                "caja-de-planta",
                "the label is COMPUTERNAME trimmed the way label_from trims it, and COMPUTERNAME \
                 wins over HOSTNAME. A default that returned a constant word or an empty string \
                 would stop naming the machine at all"
            );
        }

        let _host = crate::envs::ScopedEnv::cleared("HOSTNAME");
        let _computer = crate::envs::ScopedEnv::cleared("COMPUTERNAME");
        assert_eq!(
            default_label(),
            "device",
            "with neither variable set there is no name to read, and the answer is the fixed \
             fallback, not an empty label"
        );
    }

    #[tokio::test]
    async fn a_literal_address_resolves_to_itself() {
        assert_eq!(
            SystemNetwork.resolve("127.0.0.1").await,
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            "an address written as a number is its own answer, and it is listed"
        );
        assert_eq!(
            SystemNetwork.resolve("::1").await,
            vec![IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)],
        );
    }

    #[test]
    fn the_own_address_towards_loopback_is_loopback() {
        assert_eq!(
            SystemNetwork.own_ipv4_towards(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            Some(Ipv4Addr::LOCALHOST),
            "a UDP connect to loopback sends nothing and reports the loopback source"
        );
    }

    #[test]
    fn only_a_real_ipv4_source_can_start_a_sweep() {
        assert_eq!(
            usable_source(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20))),
            Some(Ipv4Addr::new(192, 168, 1, 20))
        );
        assert_eq!(
            usable_source(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            None,
            "0.0.0.0 has no subnet to sweep"
        );
        assert_eq!(
            usable_source(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
            None,
            "an IPv6 source has no IPv4 /24"
        );
    }

    #[tokio::test]
    async fn the_cli_refuses_a_line_it_cannot_read_before_it_touches_the_disk() {
        let bogus: Vec<String> = vec!["bogus".to_string()];
        let err = match run_cli(&bogus).await {
            Ok(()) => panic!("`bridge bogus` must be refused, not run"),
            Err(err) => err,
        };
        assert!(
            format!("{err:#}").contains("unknown subcommand"),
            "the reader is told the subcommand is unknown. Got: {err:#}"
        );
        let no_code: Vec<String> = vec!["pair".to_string()];
        assert!(
            run_cli(&no_code).await.is_err(),
            "`bridge pair` without a code cannot pair and must not reach the network"
        );
    }

    /// `pair` reads the code before it looks for a host or writes a state file, so a symbol
    /// outside the pairing alphabet is refused on this machine with no network and no disk.
    #[tokio::test]
    async fn the_pair_cli_refuses_a_mistyped_code_before_any_network_or_state_write() {
        let err = match cli_pair("!", None, Some("pc-02".to_string()), None).await {
            Ok(()) => panic!("a code with a symbol outside the alphabet must not pair"),
            Err(err) => err,
        };
        assert!(
            format!("{err:#}").contains("código mal escrito"),
            "the operator is told the code is mistyped, not shown a network error. Got: {err:#}"
        );
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn parsed(line: &str) -> Command {
        match parse(&words(line)) {
            Ok(command) => command,
            Err(err) => panic!("`bridge {line}` must parse, the guide shows it. Got: {err:#}"),
        }
    }

    fn refused(line: &str) -> String {
        match parse(&words(line)) {
            Ok(command) => panic!("`bridge {line}` must be refused. Parsed as {command:?}"),
            Err(err) => format!("{err:#}"),
        }
    }

    /// I7 (the parser the guide is judged against)
    #[test]
    fn every_subcommand_the_guide_teaches_parses_to_its_own_command() {
        assert_eq!(
            parsed("pair K7QM-3WXP-9HNR"),
            Command::Pair {
                code: "K7QM-3WXP-9HNR".to_string(),
                host: None,
                device: None,
                port: None,
            },
            "`bridge pair <code>` is the one command a client types to pair"
        );
        assert_eq!(
            parsed("pair K7QM-3WXP-9HNR --host madre --device pc-02 --port 9100"),
            Command::Pair {
                code: "K7QM-3WXP-9HNR".to_string(),
                host: Some("madre".to_string()),
                device: Some("pc-02".to_string()),
                port: Some(9100),
            },
            "`--host`, `--device` and `--port` each carry their own value"
        );
        assert_eq!(parsed("run"), Command::Run { port: None });
        assert_eq!(
            parsed("run --port 9000"),
            Command::Run { port: Some(9000) },
            "`bridge run --port` overrides the port in the state file"
        );
        assert_eq!(parsed("status"), Command::Status);
        assert_eq!(parsed("doctor"), Command::Doctor);
        assert_eq!(
            parsed("install"),
            Command::Install { print: false },
            "`bridge install` registers the task; only `--print` is a dry run"
        );
        assert_eq!(parsed("install --print"), Command::Install { print: true });
        assert_eq!(parsed("uninstall"), Command::Uninstall);
    }

    /// I7 (a typo in the guide must be caught by the parser, not by a reader)
    #[test]
    fn a_misspelled_subcommand_or_option_is_refused_by_name() {
        assert!(
            refused("pari K7QM-3WXP-9HNR").contains("pari"),
            "the typo must be named so the reader sees what to fix"
        );
        assert!(
            refused("install --bogus").contains("--bogus"),
            "an option nobody defined must not be ignored: `install --prnt` would register the \
             task the reader only wanted to look at"
        );
        assert!(
            refused("doctor now").contains("now"),
            "`doctor` takes nothing; a stray word means the line is not what the author meant"
        );
        assert!(
            refused("pair").contains("code"),
            "`bridge pair` without a code cannot pair; the error must say what is missing"
        );
        assert!(
            refused("pair K7QM-3WXP-9HNR --port 0").contains("port"),
            "port 0 is not a port the bridge could listen on"
        );
    }

    #[test]
    fn help_is_asked_for_anywhere_and_a_bare_bridge_needs_a_subcommand() {
        assert_eq!(parsed("--help"), Command::Help);
        assert_eq!(
            parsed("pair --help"),
            Command::Help,
            "`--help` after a subcommand is still a request for help, as `run_cli` treats it today"
        );
        assert!(
            refused("").contains("subcommand"),
            "`bridge` alone must say that it needs a subcommand"
        );
    }

    /// I7 (a flag nobody defined is named even after the code, not taken for a second code)
    #[test]
    fn an_unknown_flag_after_the_code_is_named_by_it() {
        assert!(
            refused("pair K7QM-3WXP-9HNR --bogus").contains("--bogus"),
            "`--bogus` is an option the parser does not know: it must be named, not reported as \
             a second pairing code"
        );
    }

    /// I7 (`run` takes the port flag and nothing else)
    #[test]
    fn run_takes_no_flag_but_the_port() {
        assert!(
            refused("run --bogus 9000").contains("usage: memory-industry bridge run"),
            "`run` answers any flag but `--port` with its usage line, so the reader sees the one \
             form that exists"
        );
    }

    /// B5: the button «Reintentar ahora» runs `bridge reconnect`, and «Escribir nombre o IP»
    /// runs it with `--host`.
    #[test]
    fn reconnect_parses_with_and_without_a_host() {
        assert_eq!(parsed("reconnect"), Command::Reconnect { host: None });
        assert_eq!(
            parsed("reconnect --host madre"),
            Command::Reconnect {
                host: Some("madre".to_string())
            },
            "a name typed in the window"
        );
        assert_eq!(
            parsed("reconnect --host 192.168.1.20:8787"),
            Command::Reconnect {
                host: Some("192.168.1.20:8787".to_string())
            },
            "or an address with its port, kept as typed for the bridge to resolve"
        );
    }

    /// B5: what the window could pass wrong is refused by name, not ignored.
    #[test]
    fn reconnect_refuses_what_it_does_not_take() {
        assert!(
            refused("reconnect --bogus").contains("--bogus"),
            "an option nobody defined is named"
        );
        assert!(
            refused("reconnect --host").contains("--host"),
            "`--host` without its value says which flag is missing it"
        );
        assert!(
            refused("reconnect ahora").contains("ahora"),
            "a stray word is named: `reconnect` takes `--host` and nothing else"
        );
    }

    /// I2/I3 (the help names every subcommand the guide teaches)
    #[test]
    fn the_usage_names_every_subcommand() {
        for sub in [
            "pair",
            "run",
            "status",
            "install",
            "uninstall",
            "doctor",
            "reconnect",
        ] {
            assert!(
                USAGE.contains(sub),
                "`memory-industry bridge --help` does not list `{sub}`: a command the page \
                 teaches and the help hides is one nobody finds. USAGE: {USAGE}"
            );
        }
    }
}
