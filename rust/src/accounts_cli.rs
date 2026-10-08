//! `memory-industry account …`: the host operator's side of accounts and pairing codes.

use anyhow::{Result, bail};

const USAGE: &str = "usage: memory-industry account <add|list|revoke|revoke-device|pair> …\n\n\
Run on the host. Accounts authenticate and attribute; they do not partition the graph.";

pub async fn run_cli(args: &[String]) -> Result<()> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "help"))
    {
        println!("{USAGE}");
        return Ok(());
    }
    bail!("account: not implemented yet")
}
