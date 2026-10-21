use crate::degraded::LostSignals;
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        "stats" => stats(pool).await,
        "history" => history(pool, &args).await,
        "resolve" => resolve(pool, &args).await,
        "trust" => trust_stats(pool).await,
        "metrics" => metrics(pool).await,
        _ => anyhow::bail!("Invalid action: {action}. Use stats/history/resolve/trust/metrics"),
    }
}

async fn metrics(pool: &PgPool) -> Result<Value> {
    type Row = (f64, String);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT confidence::float8, outcome FROM brain_verify_log
         WHERE outcome IN ('correct', 'incorrect')",
    )
    .fetch_all(pool)
    .await
    .context("reading the verify log for the calibration metrics")?;

    if rows.is_empty() {
        return Ok(serde_json::json!({
            "action": "metrics",
            "brier": null,
            "ece": null,
            "n": 0,
            "note": "no resolved verify_log entries — call cuba_calibrar resolve first"
        }));
    }

    let n = rows.len() as f64;
    let brier: f64 = rows
        .iter()
        .map(|(p, outcome)| {
            let o = if outcome == "correct" { 1.0 } else { 0.0 };
            (p - o).powi(2)
        })
        .sum::<f64>()
        / n;

    const N_BINS: usize = 10;
    let mut bin_n = [0u32; N_BINS];
    let mut bin_acc_sum = [0.0_f64; N_BINS];
    let mut bin_conf_sum = [0.0_f64; N_BINS];
    for (p, outcome) in &rows {
        let idx = ((p * N_BINS as f64) as usize).min(N_BINS - 1);
        bin_n[idx] += 1;
        bin_conf_sum[idx] += p;
        if outcome == "correct" {
            bin_acc_sum[idx] += 1.0;
        }
    }
    let mut ece = 0.0_f64;
    let mut reliability = Vec::with_capacity(N_BINS);
    for k in 0..N_BINS {
        if bin_n[k] > 0 {
            let weight = bin_n[k] as f64 / n;
            let acc = bin_acc_sum[k] / bin_n[k] as f64;
            let conf = bin_conf_sum[k] / bin_n[k] as f64;
            ece += weight * (acc - conf).abs();
            reliability.push(serde_json::json!({
                "bin_lo": k as f64 / N_BINS as f64,
                "bin_hi": (k + 1) as f64 / N_BINS as f64,
                "n": bin_n[k],
                "accuracy": acc,
                "confidence": conf,
                "gap": acc - conf,
            }));
        }
    }

    Ok(serde_json::json!({
        "action": "metrics",
        "brier": brier,
        "ece": ece,
        "n": rows.len(),
        "reliability_diagram": reliability,
        "interpretation": "Brier (Brier 1950): lower = better calibrated. ECE (Naeini AAAI 2015): expected gap between confidence and empirical accuracy across bins."
    }))
}

async fn trust_stats(pool: &PgPool) -> Result<Value> {
    type Row = (String, f64, f64, chrono::DateTime<chrono::Utc>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT source, alpha, beta, updated_at
         FROM brain_source_trust
         ORDER BY (alpha / (alpha + beta)) DESC",
    )
    .fetch_all(pool)
    .await
    .context("reading the source trust table")?;

    let trust: Vec<Value> = rows
        .iter()
        .map(|(source, alpha, beta, updated_at)| {
            let p_correct = alpha / (alpha + beta);
            let total = alpha + beta - 2.0;
            let variance = (alpha * beta) / ((alpha + beta).powi(2) * (alpha + beta + 1.0));
            serde_json::json!({
                "source": source,
                "alpha": alpha,
                "beta": beta,
                "p_correct": p_correct,
                "credible_interval_width": (variance.sqrt() * 2.0).min(1.0),
                "total_resolved": total.max(0.0) as i64,
                "updated_at": updated_at.to_rfc3339()
            })
        })
        .collect();

    Ok(serde_json::json!({
        "action": "trust",
        "sources": trust,
        "interpretation": "Bayesian Beta(α, β) posterior. p_correct = α / (α + β). Width shrinks with more data."
    }))
}

async fn stats(pool: &PgPool) -> Result<Value> {
    type StatRow = (String, i64, i64, f64);
    let rows: Vec<StatRow> = sqlx::query_as(
        "SELECT grounding_level,
                COUNT(*) FILTER (WHERE outcome = 'correct') + 1 AS alpha,
                COUNT(*) FILTER (WHERE outcome = 'incorrect') + 1 AS beta,
                (COUNT(*) FILTER (WHERE outcome = 'correct') + 1)::float8 /
                (COUNT(*) FILTER (WHERE outcome = 'correct') + COUNT(*) FILTER (WHERE outcome = 'incorrect') + 2)::float8 AS p_correct
         FROM brain_verify_log
         WHERE outcome != 'pending'
         GROUP BY grounding_level
         ORDER BY p_correct DESC"
    )
    .fetch_all(pool)
    .await
    .context("reading the verify log for the calibration stats")?;

    let levels: Vec<Value> = rows
        .iter()
        .map(|(level, alpha, beta, p_correct)| {
            serde_json::json!({
                "grounding_level": level,
                "alpha": alpha,
                "beta": beta,
                "p_correct": p_correct,
                "total_resolved": alpha + beta - 2
            })
        })
        .collect();

    let total: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT COUNT(*),
                COUNT(*) FILTER (WHERE outcome = 'correct'),
                COUNT(*) FILTER (WHERE outcome = 'incorrect')
         FROM brain_verify_log",
    )
    .fetch_optional(pool)
    .await
    .context("counting the verify log for the calibration stats")?;

    let (total_count, correct, incorrect) = total.unwrap_or((0, 0, 0));

    Ok(serde_json::json!({
        "action": "stats",
        "levels": levels,
        "overall": {
            "total_predictions": total_count,
            "correct": correct,
            "incorrect": incorrect,
            "pending": total_count - correct - incorrect
        }
    }))
}

async fn history(pool: &PgPool, args: &Value) -> Result<Value> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(20)
        .min(100);

    type LogRow = (
        uuid::Uuid,
        String,
        Option<String>,
        f64,
        String,
        String,
        chrono::DateTime<chrono::Utc>,
    );
    let rows: Vec<LogRow> = sqlx::query_as(
        "SELECT id, claim, entity_name, confidence, grounding_level, outcome, created_at
         FROM brain_verify_log
         ORDER BY created_at DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let items: Vec<Value> = rows
        .iter()
        .map(|(id, claim, entity, conf, level, outcome, created)| {
            serde_json::json!({
                "id": id.to_string(),
                "claim": claim,
                "entity_name": entity,
                "confidence": conf,
                "grounding_level": level,
                "outcome": outcome,
                "created_at": created.to_rfc3339()
            })
        })
        .collect();

    let count = items.len();
    Ok(serde_json::json!({
        "action": "history",
        "entries": items,
        "count": count
    }))
}

async fn resolve(pool: &PgPool, args: &Value) -> Result<Value> {
    let verify_id = args.get("verify_id").and_then(|v| v.as_str()).unwrap_or("");
    let outcome = args.get("outcome").and_then(|v| v.as_str()).unwrap_or("");

    let id: uuid::Uuid = verify_id.parse().context("invalid verify_id UUID")?;

    if outcome != "correct" && outcome != "incorrect" {
        anyhow::bail!("outcome must be 'correct' or 'incorrect'");
    }

    let entity_row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT entity_name FROM brain_verify_log WHERE id = $1 AND outcome = 'pending'",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let entity_name: Option<String> = entity_row.and_then(|(n,)| n);

    let result = sqlx::query(
        "UPDATE brain_verify_log SET outcome = $2 WHERE id = $1 AND outcome = 'pending'",
    )
    .bind(id)
    .bind(outcome)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        anyhow::bail!("Verify log entry not found or already resolved");
    }

    let mut sources_updated = 0u32;
    let mut lost = LostSignals::default();
    if let Some(ref ename) = entity_name {
        let sources: Vec<(String,)> = lost.note(
            "trust_sources",
            sqlx::query_as(
                "SELECT DISTINCT o.source FROM brain_observations o
                 JOIN brain_entities e ON o.entity_id = e.id
                 WHERE e.name = $1 AND o.observation_type != 'superseded'",
            )
            .bind(ename)
            .fetch_all(pool)
            .await,
        );

        let column = if outcome == "correct" {
            "alpha"
        } else {
            "beta"
        };
        for (source,) in &sources {
            let sql = format!(
                "INSERT INTO brain_source_trust (source, alpha, beta, updated_at)
                 VALUES ($1, 1.0, 1.0, NOW())
                 ON CONFLICT (source) DO UPDATE
                   SET {col} = brain_source_trust.{col} + 1.0,
                       updated_at = NOW()",
                col = column
            );
            if sqlx::query(&sql).bind(source).execute(pool).await.is_ok() {
                sources_updated += 1;
            }
        }
    }

    let mut response = serde_json::json!({
        "action": "resolve",
        "verify_id": verify_id,
        "outcome": outcome,
        "entity_name": entity_name,
        "sources_credibility_updated": sources_updated
    });
    lost.annotate(&mut response, what_a_lost_signal_costs);
    Ok(response)
}

fn what_a_lost_signal_costs(signal: &str) -> &'static str {
    match signal {
        "trust_sources" => {
            "las fuentes cuya credibilidad debía actualizarse (el resultado quedó registrado, la confianza de sus fuentes NO se actualizó)"
        }
        _ => "una señal de la calibración",
    }
}
