//! `memory-industry bridge …`: the client's side, a local bridge that finds the host again
//! and proves who it is before it sends a credential.

use anyhow::{Result, bail};

const USAGE: &str = "usage: memory-industry bridge <pair|run|status|install|uninstall> …\n\n\
Run on a client PC. The bridge listens on 127.0.0.1 only and holds the credential.";

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
