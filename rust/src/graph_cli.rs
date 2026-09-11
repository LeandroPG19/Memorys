//! CLI for the optional graph projection (FalkorDB / Neo4j).

use anyhow::{Context, Result, bail};
use std::fs;
use std::io::Write;

pub async fn run_cli(args: &[String]) -> Result<()> {
    crate::graph_db::load_saved_config_into_env();
    let cmd = args.first().map(String::as_str).unwrap_or("status");
    match cmd {
        "status" | "" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::graph_db::status_summary())?
            );
            Ok(())
        }
        "set" => set_backend(&args[1..]),
        "reconcile" => {
            let url = crate::setup::resolve_database_url().await;
            let pool = crate::db::create_pool(&url).await?;
            let report = crate::graph_db::reconcile_stub(&pool).await;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "tokens" => {
            let start = args.get(1).context("usage: graph tokens NAME [depth]")?;
            let depth = args.get(2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(3);
            match crate::graph_db::traverse_falkor_with_paths(start, depth) {
                Ok((hops, paths)) => {
                    let hop_payload = crate::graph_db::hop_ball_payload(&hops);
                    let path_payload = serde_json::json!(
                        paths
                            .iter()
                            .map(crate::graph_db::compact_rel_path)
                            .collect::<Vec<_>>()
                    );
                    let hop_tokens = crate::search::budget::count_tokens(&hop_payload.to_string());
                    let path_tokens =
                        crate::search::budget::count_tokens(&path_payload.to_string());
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "backend": "falkor",
                            "start": start,
                            "depth": depth,
                            "hops": hops.len(),
                            "paths": paths.len(),
                            "hop_ball_tokens": hop_tokens,
                            "path_tokens": path_tokens,
                            "ratio": if hop_tokens > 0 {
                                path_tokens as f64 / hop_tokens as f64
                            } else {
                                0.0
                            }
                        }))?
                    );
                    Ok(())
                }
                Err(e) => bail!("falkor tokens failed: {e}"),
            }
        }
        "traverse" => {
            let start = args.get(1).context("usage: graph traverse NAME [depth]")?;
            let depth = args.get(2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(3);
            match crate::graph_db::traverse_falkor_with_paths(start, depth) {
                Ok((hops, paths)) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "backend": "falkor",
                            "start": start,
                            "hops": hops.len(),
                            "nodes": hops.iter().map(|h| serde_json::json!({
                                "name": h.name,
                                "relation": h.relation,
                                "strength": h.strength,
                                "depth": h.depth
                            })).collect::<Vec<_>>(),
                            "paths": paths.iter().map(|p| serde_json::json!({
                                "nodes": p.nodes,
                                "relations": p.relations,
                                "score": p.score
                            })).collect::<Vec<_>>(),
                        }))?
                    );
                    Ok(())
                }
                Err(e) => bail!("falkor traverse failed: {e}"),
            }
        }
        "help" | "-h" | "--help" => {
            println!(
                "usage: memory-industry graph [status|set falkor|reconcile|traverse NAME|tokens NAME]\n\n\
Postgres remains source of truth. `graph set falkor` writes graph.env and expects\n\
`docker compose --profile graph up -d` (redis://127.0.0.1:6389).\n\
MEMORY_INDUSTRY_GRAPH_NAME selects the graph (default memory_industry)."
            );
            Ok(())
        }
        other => bail!("unknown graph subcommand: {other} (try help)"),
    }
}

fn set_backend(args: &[String]) -> Result<()> {
    let name = args
        .first()
        .map(String::as_str)
        .context("usage: memory-industry graph set falkor|neo4j|off")?;
    let (backend, default_url) = match name {
        "falkor" | "falkordb" => ("falkor", "redis://127.0.0.1:6389"),
        "neo4j" => ("neo4j", "http://127.0.0.1:7474"),
        "off" => ("off", ""),
        other => bail!("unknown backend `{other}` — use falkor|neo4j|off"),
    };
    let path = crate::graph_db::config_path().context("no config path")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut body = format!("MEMORY_INDUSTRY_GRAPH_DB={backend}\n");
    if !default_url.is_empty() {
        body.push_str(&format!("MEMORY_INDUSTRY_GRAPH_URL={default_url}\n"));
    }
    let mut f = fs::File::create(&path)?;
    f.write_all(body.as_bytes())?;
    println!("wrote {}", path.display());
    crate::graph_db::load_saved_config_into_env();
    println!(
        "{}",
        serde_json::to_string_pretty(&crate::graph_db::status_summary())?
    );
    Ok(())
}
