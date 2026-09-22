use anyhow::{Context, Result};

use crate::search::calibrate::{self, DEFAULT_ALPHA};

fn load_queries(path: Option<&str>) -> Result<Vec<String>> {
    let Some(path) = path else {
        anyhow::bail!(
            "hace falta un dataset de calibración: --dataset rust/tests/datasets/longmemeval_abilities_es.jsonl\n\
             Deben ser consultas RESPONDIBLES: la garantía conformal es sobre no rechazarlas."
        );
    };
    let text = std::fs::read_to_string(path).with_context(|| format!("leyendo {path}"))?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).context("línea JSONL inválida")?;
        if v.get("abstain").and_then(serde_json::Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(q) = v.get("query").and_then(serde_json::Value::as_str) {
            out.push(q.to_string());
        }
    }
    Ok(out)
}

#[derive(Debug, Default, PartialEq)]
pub struct CalibrateArgs {
    pub dataset: Option<String>,
    pub alpha: f64,
    pub sample_limit: i64,
    pub json: bool,
    pub apply: bool,
    pub help: bool,
}

pub fn parse_args(args: &[String]) -> Result<CalibrateArgs> {
    let mut out = CalibrateArgs {
        alpha: DEFAULT_ALPHA,
        sample_limit: 5000,
        ..Default::default()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dataset" | "-d" => out.dataset = it.next().cloned(),
            "--alpha" => {
                out.alpha = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--alpha needs a float, e.g. 0.05")?
            }
            "--samples" => {
                out.sample_limit = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .context("--samples needs an integer")?
            }
            "--json" => out.json = true,
            "--apply" => out.apply = true,
            "-h" | "--help" => out.help = true,
            other => anyhow::bail!("unknown calibrate flag: {other} (try --help)"),
        }
    }
    Ok(out)
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    let parsed = parse_args(args)?;
    if parsed.help {
        eprintln!(
            "usage: memory-industry calibrate --dataset PATH.jsonl [--alpha 0.05] [--samples N] [--json] [--apply]\n\n\
             Diagnoses the OOD abstention threshold and computes a conformal one.\n\
             The theoretical χ² cutoff assumes Gaussian embeddings and a well-estimated\n\
             covariance; e5 normalizes to the unit sphere and the covariance is fitted\n\
             from 500 samples in 384 dimensions. Neither holds, and abstention rejects\n\
             100% of answerable queries as a result.\n\n\
             --alpha  the false-abstention rate you accept (default 0.05)\n\
             --apply  persist the threshold so the server uses it (brain_calibration)\n\
             --json   machine-readable report; honours --apply and reports it as \"applied\""
        );
        return Ok(());
    }
    let CalibrateArgs {
        dataset,
        alpha,
        sample_limit,
        json,
        apply,
        ..
    } = parsed;

    let queries = load_queries(dataset.as_deref())?;
    if queries.is_empty() {
        anyhow::bail!("el dataset no tiene consultas respondibles");
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for calibration")?;

    let report = calibrate::calibrate(&pool, &queries, alpha, sample_limit).await?;

    if json {
        let mut out = serde_json::to_value(&report)?;
        let mut applied = false;
        if apply && let Some(t) = report.conformal_threshold {
            calibrate::store_ood_threshold(&pool, t, &report).await?;
            applied = true;
        }
        if let Some(obj) = out.as_object_mut() {
            obj.insert("applied".to_string(), serde_json::json!(applied));
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!("Calibración del umbral de abstención (OOD)\n");
    println!("  dimensión del embedding : {}", report.embedding_dim);
    println!("  muestras para ajustar Σ : {}", report.fit_samples);
    println!(
        "  ratio n/d               : {:.2}  {}",
        report.fit_samples as f64 / report.embedding_dim as f64,
        if (report.fit_samples as f64) < 10.0 * report.embedding_dim as f64 {
            "← demasiado bajo: Σ mal condicionada, su inversa amplifica ruido"
        } else {
            ""
        }
    );
    println!();

    println!(
        "  umbral TEÓRICO (χ², Wilson-Hilferty) : {:.2}",
        report.theoretical_threshold
    );
    if let Some(c) = &report.corpus {
        println!(
            "\n  Distancias del CORPUS contra su propia distribución (n={}):",
            c.n
        );
        println!(
            "    min={:.1}  p50={:.1}  p90={:.1}  p95={:.1}  p99={:.1}  max={:.1}",
            c.min, c.p50, c.p90, c.p95, c.p99, c.max
        );
        println!(
            "    → el umbral teórico rechaza el {:.1}% del propio corpus (debería ser ~1%)",
            report.theoretical_rejects_corpus * 100.0
        );
    }
    if let Some(q) = &report.queries {
        println!("\n  Distancias de las CONSULTAS respondibles (n={}):", q.n);
        println!(
            "    min={:.1}  p50={:.1}  p90={:.1}  p95={:.1}  p99={:.1}  max={:.1}",
            q.min, q.p50, q.p90, q.p95, q.p99, q.max
        );
    }

    println!();
    match report.conformal_threshold {
        Some(t) => {
            println!(
                "  umbral CONFORMAL (α={:.2})           : {:.2}",
                report.alpha, t
            );
            println!(
                "\n  Garantía: como mucho el {:.0}% de las consultas respondibles futuras\n  \
                 serán rechazadas por error — sin asumir gaussianidad ni una Σ bien estimada.",
                report.alpha * 100.0
            );
            if apply {
                calibrate::store_ood_threshold(&pool, t, &report).await?;
                println!("\n  Guardado en brain_calibration. El servidor ya lo usa.");
            } else {
                println!("\n  Esto fue un diagnóstico — no se guardó nada.");
                println!(
                    "  Para que el servidor lo use:  memory-industry calibrate --dataset ... --apply"
                );
            }
        }
        None => println!("  no se pudo calcular el umbral conformal (sin consultas)"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn json_and_apply_are_not_mutually_exclusive() {
        let p = parse_args(&args(&["--dataset", "d.jsonl", "--apply", "--json"])).unwrap();
        assert!(p.apply, "--apply must survive alongside --json");
        assert!(p.json);
    }

    #[test]
    fn defaults_are_conservative() {
        let p = parse_args(&args(&["--dataset", "d.jsonl"])).unwrap();
        assert!(
            !p.apply,
            "calibration must not persist anything unless asked"
        );
        assert!(!p.json);
        assert_eq!(p.alpha, DEFAULT_ALPHA);
    }

    #[test]
    fn an_unknown_flag_is_rejected() {
        assert!(parse_args(&args(&["--nonsense"])).is_err());
    }

    #[test]
    fn alpha_and_samples_parse() {
        let p = parse_args(&args(&["--alpha", "0.1", "--samples", "250"])).unwrap();
        assert_eq!(p.alpha, 0.1);
        assert_eq!(p.sample_limit, 250);
    }
}
