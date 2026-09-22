use anyhow::{Context, Result};

use crate::graph::autolink::{self, DEFAULT_NPMI_THRESHOLD, MIN_CO_SESSIONS};

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut threshold = DEFAULT_NPMI_THRESHOLD;
    let mut min_co = MIN_CO_SESSIONS;
    let mut apply = false;
    let mut limit = 40usize;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--threshold" => {
                threshold = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--threshold needs a float in [-1,1], e.g. 0.3")?
            }
            "--min-sessions" => {
                min_co = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--min-sessions needs an integer")?
            }
            "--show" => {
                limit = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--show needs an integer")?
            }
            "--apply" => apply = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: memory-industry link [--threshold 0.3] [--min-sessions 2] [--apply]\n\n\
                     Proposes `related_to` edges between entities that co-occur in working\n\
                     sessions MORE THAN CHANCE PREDICTS, scored by normalized pointwise mutual\n\
                     information. A raw co-occurrence count would just wire the graph to\n\
                     whatever you work on most; NPMI corrects for that.\n\n\
                     Never modifies an existing relation. Without --apply, only shows the plan."
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown link flag: {other} (try --help)"),
        }
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database")?;

    let cands = autolink::candidates(&pool, min_co, threshold).await?;
    if cands.is_empty() {
        println!("No hay pares con NPMI >= {threshold:.2} y {min_co}+ sesiones en común.");
        return Ok(());
    }

    println!(
        "{} aristas propuestas (NPMI >= {:.2}, {}+ sesiones compartidas):\n",
        cands.len(),
        threshold,
        min_co
    );
    for c in cands.iter().take(limit) {
        println!(
            "  {:.3}  {} ←→ {}   ({} sesiones)",
            c.npmi, c.from_name, c.to_name, c.co_sessions
        );
    }
    if cands.len() > limit {
        println!("  … y {} más", cands.len() - limit);
    }
    println!();

    if !apply {
        println!("Esto fue un plan — no se creó ninguna arista.");
        println!("Para aplicarlo:  memory-industry link --apply");
        return Ok(());
    }

    let n = autolink::apply(&pool, &cands).await?;
    println!("{n} aristas creadas (tipo related_to, fuerza = NPMI, bidireccionales).");
    println!("Ninguna relación existente fue modificada.");
    Ok(())
}
