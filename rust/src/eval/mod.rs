pub mod datasets;
pub mod harness;
pub mod metrics;
pub mod reporters;

use anyhow::{Context, Result};

fn load_eval_json(path: &str) -> Result<serde_json::Value> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let raw = raw.trim_start_matches('\u{feff}');
    serde_json::from_str(raw).with_context(|| format!("{path} is not valid JSON"))
}

fn f64_list(doc: &serde_json::Value, pointer: &str) -> Vec<f64> {
    doc.pointer(pointer)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_f64()).collect())
        .unwrap_or_default()
}

fn print_paired(label: &str, a: &[f64], b: &[f64]) {
    if a.is_empty() || a.len() != b.len() {
        return;
    }
    let Some((mean, lo, hi)) = metrics::paired_bootstrap(a, b, 2000, 0.95) else {
        return;
    };
    let mde = metrics::minimum_detectable_effect_paired(a, b);
    let moved = a
        .iter()
        .zip(b)
        .filter(|(x, y)| (*y - *x).abs() > 1e-12)
        .count();
    println!("{label} n={}", a.len());
    println!("Δ nDCG@10 = {mean:+.4}  IC95 = [{lo:+.4}, {hi:+.4}]");
    println!("efecto mínimo detectable (pareado) = {mde:.4}");
    println!("preguntas cuyo nDCG cambia: {moved} de {}", a.len());
    if lo > 0.0 || hi < 0.0 {
        println!("el intervalo NO toca cero: la diferencia es real");
    } else {
        println!("el intervalo cruza cero: no se puede distinguir de ruido");
    }
}

fn compare_runs(before: &str, after: &str) -> Result<()> {
    let a_doc = load_eval_json(before)?;
    let b_doc = load_eval_json(after)?;
    let a = f64_list(&a_doc, "/metrics/per_query_ndcg");
    let b = f64_list(&b_doc, "/metrics/per_query_ndcg");
    if a.is_empty() {
        anyhow::bail!("{before} has no metrics.per_query_ndcg — rerun that arm with --json");
    }
    if a.len() != b.len() {
        anyhow::bail!(
            "cannot pair {} questions against {} — the two runs did not score the same dataset",
            a.len(),
            b.len()
        );
    }

    println!("pareado ({before} → {after})");
    print_paired("todas", &a, &b);

    let abilities_a = a_doc
        .pointer("/metrics/per_query_ability")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if abilities_a.len() == a.len() {
        let mut hop_a = Vec::new();
        let mut hop_b = Vec::new();
        for (i, label) in abilities_a.iter().enumerate() {
            if label.as_str() == Some("multi_hop") {
                hop_a.push(a[i]);
                hop_b.push(b[i]);
            }
        }
        if !hop_a.is_empty() {
            println!();
            print_paired("multi_hop", &hop_a, &hop_b);
        }
    }

    let g0 = a_doc
        .pointer("/metrics/mean_graph_tokens")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let g1 = b_doc
        .pointer("/metrics/mean_graph_tokens")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let h0 = a_doc
        .pointer("/metrics/mean_hop_ball_tokens")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let h1 = b_doc
        .pointer("/metrics/mean_hop_ball_tokens")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if g0 > 0.0 || g1 > 0.0 || h0 > 0.0 || h1 > 0.0 {
        println!();
        println!("grafo paths tok {g0:.0} → {g1:.0}");
        println!("grafo bola tok  {h0:.0} → {h1:.0}");
        if h1 > 0.0 {
            let ratio = g1 / h1;
            println!("ratio paths/bola (tratamiento) = {ratio:.3}");
            if ratio <= 0.8 {
                println!("cumple el tope PathRAG-lt: tokens de grafo ≤ 80% de la bola");
            } else {
                println!("no cumple el tope PathRAG-lt (objetivo ≤ 0.80)");
            }
        }
    }
    Ok(())
}

async fn run_coverage(path: &str) -> Result<()> {
    let samples = datasets::load_jsonl_dataset(path).with_context(|| format!("loading {path}"))?;
    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for coverage")?;

    let mut gold = 0usize;
    let mut present = 0usize;
    for s in &samples {
        for name in &s.gold_entities {
            gold += 1;
            let hit: Option<(i32,)> =
                sqlx::query_as("SELECT 1 FROM brain_entities WHERE name = $1 LIMIT 1")
                    .bind(name)
                    .fetch_optional(&pool)
                    .await?;
            if hit.is_some() {
                present += 1;
            }
        }
    }
    let pct = if gold == 0 {
        0.0
    } else {
        100.0 * present as f64 / gold as f64
    };
    println!(
        "{}",
        serde_json::json!({
            "dataset": path,
            "samples": samples.len(),
            "gold_entities": gold,
            "present_in_kg": present,
            "coverage_pct": (pct * 10.0).round() / 10.0,
            "note": "Han et al. 2025: GraphRAG recall cannot exceed entity coverage of the KG"
        })
    );
    Ok(())
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let mut dataset_path: Option<String> = None;
    let mut json = false;
    let mut cfg = harness::EvalConfig::default();

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--coverage" => {
                let path = it
                    .next()
                    .cloned()
                    .context("--coverage needs a JSONL dataset")?;
                return run_coverage(&path).await;
            }
            "--compare" => {
                let before = it.next().context("--compare needs two JSON reports")?;
                let after = it.next().context("--compare needs two JSON reports")?;
                return compare_runs(before, after);
            }
            "--dataset" | "-d" => dataset_path = it.next().cloned(),
            "--k" => {
                cfg.k = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--k needs an integer")?
            }
            "--json" => json = true,
            "--associative" => cfg.associative = true,
            "--abstain" => cfg.abstain = true,
            "--rerank" => cfg.rerank = true,
            "--format" => {
                cfg.format = it
                    .next()
                    .cloned()
                    .context("--format needs verbose|compact")?
            }
            "--max-tokens" => {
                cfg.max_tokens = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--max-tokens needs an integer")?
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: cuba-memorys eval [--dataset PATH.jsonl] [--k N]\n\
                     \x20                        [--associative] [--abstain] [--rerank]\n\
                     \x20                        [--format verbose|compact] [--max-tokens N] [--json]\n\n\
                     --associative  multi-hop expansion (v0.11)\n\
                     --abstain      let the OOD gate fire, so abstention is actually exercised\n\
                     --rerank       run the cross-encoder reranker\n\
                     --format       response shape whose token cost is measured (default verbose)\n\
                     --max-tokens   response budget. Defaults to unlimited so the score measures\n\
                     \x20              the ranking; pass 5000 to reproduce what an MCP client sees\n\
                     --compare A.json B.json   paired bootstrap between two --json runs of the\n\
                     \x20              same dataset. This is the test to accept or reject a change\n\
                     \x20              with; the per-run interval printed below is not.\n\
                     --coverage PATH.jsonl     fraction of gold_entities present in brain_entities\n\
                     \x20              (construction ceiling; Han et al. 2025).\n\n\
                     Every run reports mean/max response tokens: quality that costs twice the\n\
                     context is not free, and you cannot see that without printing both.\n\n\
                     JSONL row: {{\"query\": \"...\", \"relevant_markers\": [\"...\"], \"expected_answer\": \"...\"?}}"
                );
                return Ok(());
            }
            other => anyhow::bail!("unknown eval flag: {other} (try --help)"),
        }
    }

    let samples = match &dataset_path {
        Some(p) => {
            datasets::load_jsonl_dataset(p).with_context(|| format!("loading dataset {p}"))?
        }
        None => datasets::builtin_retrieval_set(),
    };
    if samples.is_empty() {
        anyhow::bail!("dataset is empty — nothing to evaluate");
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for eval")?;

    let report = harness::run_faro_eval(&pool, &samples, &cfg).await?;

    if json {
        println!(
            "{}",
            reporters::generate_json_report(&report, samples.len(), cfg.k)
        );
    } else {
        let budget = if cfg.max_tokens == i64::MAX {
            "unlimited".to_string()
        } else {
            cfg.max_tokens.to_string()
        };
        eprintln!(
            "eval dataset={} samples={} k={} associative={} abstain={} rerank={} format={} max_tokens={}",
            dataset_path.as_deref().unwrap_or("<builtin>"),
            samples.len(),
            cfg.k,
            cfg.associative,
            cfg.abstain,
            cfg.rerank,
            cfg.format,
            budget,
        );
        println!("{}", reporters::summary_line(&report));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_bom_report(path: &std::path::Path, ndcgs: &[f64]) {
        let body = serde_json::json!({
            "metrics": {
                "per_query_ndcg": ndcgs,
                "per_query_ability": ["factoid", "multi_hop"]
            }
        });
        let mut file = std::fs::File::create(path).expect("temp eval report");
        file.write_all("\u{feff}".as_bytes()).expect("BOM");
        file.write_all(body.to_string().as_bytes())
            .expect("json body");
    }

    #[test]
    fn compare_accepts_utf8_bom_from_powershell_out_file() {
        let dir =
            std::env::temp_dir().join(format!("memory-industry-eval-bom-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let before = dir.join("before.json");
        let after = dir.join("after.json");
        write_bom_report(&before, &[0.2, 0.4]);
        write_bom_report(&after, &[0.3, 0.5]);

        compare_runs(
            before.to_str().expect("utf8 path"),
            after.to_str().expect("utf8 path"),
        )
        .expect("PowerShell Set-Content -Encoding utf8 prefixes a BOM; --compare must strip it");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
