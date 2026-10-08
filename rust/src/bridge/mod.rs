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

pub mod discovery;
pub mod pair;
pub mod proxy;
pub mod state;

#[cfg(test)]
pub(crate) mod fake_host;

const USAGE: &str = "usage: memory-industry bridge <pair|run|status|install|uninstall> …\n\n\
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
        match socket.local_addr().ok()?.ip() {
            IpAddr::V4(own) if !own.is_unspecified() => Some(own),
            _ => None,
        }
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

    /// A send failed before the host could have received it: the address is in doubt.
    pub(crate) async fn transport_failed(&self) {
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

fn default_state_path() -> Result<PathBuf> {
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

fn option_value(rest: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String> {
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

fn parse_port(text: &str) -> Result<u16> {
    text.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .with_context(|| format!("{text:?} is not a port (1-65535)"))
}

async fn cli_pair(args: &[String]) -> Result<()> {
    let wanted = PairArgs::parse(args)?;
    let code = wanted
        .code
        .as_deref()
        .context("bridge pair needs the pairing code: bridge pair <code>")?;
    let deps = system_deps(default_state_path()?)?;
    let label = wanted.device.unwrap_or_else(default_label);
    let mut paired = pair(
        code,
        wanted.host.as_deref(),
        &label,
        &*deps.net,
        &deps.timing,
        &deps.client,
    )
    .await?;
    if let Some(port) = wanted.port {
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

async fn cli_run(args: &[String]) -> Result<()> {
    let port = match args {
        [] => None,
        [flag, value] if flag == "--port" => Some(parse_port(value)?),
        _ => bail!("usage: memory-industry bridge run [--port N]"),
    };
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

pub async fn run_cli(args: &[String]) -> Result<()> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        println!("{USAGE}");
        return Ok(());
    }
    match args.split_first() {
        Some((first, rest)) if first == "pair" => cli_pair(rest).await,
        Some((first, rest)) if first == "run" => cli_run(rest).await,
        Some((first, _)) if first == "status" => cli_status(),
        Some((other, _)) => bail!("bridge {other}: not available in this build\n{USAGE}"),
        None => bail!("bridge needs a subcommand\n{USAGE}"),
    }
}

#[cfg(test)]
mod tests {
    use super::fake_host::{FakeNet, ManualClock, TempDir, deps};
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
}
