//! `memory-industry bridge …`: the client's side, a local bridge that finds the host again
//! and proves who it is before it sends a credential.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::service::Target;
use crate::service_apply::CommandRunner;

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

/// The two questions the bridge asks the operating system. Injected so that no test
/// resolves a name, or learns the interface address, of the machine it runs on.
#[async_trait]
pub trait Network: Send + Sync {
    async fn resolve(&self, name: &str) -> Vec<IpAddr>;

    fn own_ipv4_towards(&self, peer: IpAddr) -> Option<Ipv4Addr>;

    /// The addresses a sweep would try: the own /24 on `port`, nothing when the own
    /// address cannot be learned. A fake overrides it with loopback listeners.
    fn sweep_targets(&self, _port: u16) -> Vec<SocketAddr> {
        // PASS-1 STUB
        unimplemented!("PASS-1 STUB: Network::sweep_targets default")
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

/// The paired bridge: its state, what it last verified, and the single-flight
/// discovery. Opaque to callers.
pub struct Bridge {
    _stub: (),
}

impl Bridge {
    /// Reads the state file; a missing one is "not paired".
    pub fn load(_deps: BridgeDeps) -> Result<Arc<Bridge>> {
        // PASS-1 STUB
        unimplemented!("PASS-1 STUB: Bridge::load")
    }
}

/// `bridge run`: load the state (refusing when unpaired, before any port is opened) and
/// serve. `port` overrides the one in the state.
pub async fn run(_deps: BridgeDeps, _port: Option<u16>) -> Result<proxy::BridgeHandle> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: bridge::run")
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        println!("{USAGE}");
        return Ok(());
    }
    bail!("bridge: not implemented yet")
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
