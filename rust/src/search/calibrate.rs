use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

use super::ood::OodStats;

pub const DEFAULT_ALPHA: f64 = 0.05;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Distribution {
    pub n: usize,
    pub min: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
}

impl Distribution {
    fn from(mut scores: Vec<f64>) -> Option<Self> {
        if scores.is_empty() {
            return None;
        }
        scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let at = |q: f64| -> f64 {
            let idx = ((scores.len() as f64 - 1.0) * q).round() as usize;
            scores[idx.min(scores.len() - 1)]
        };
        Some(Self {
            n: scores.len(),
            min: scores[0],
            p50: at(0.50),
            p90: at(0.90),
            p95: at(0.95),
            p99: at(0.99),
            max: scores[scores.len() - 1],
        })
    }
}

pub fn conformal_quantile(scores: &[f64], alpha: f64) -> Option<f64> {
    if scores.is_empty() {
        return None;
    }
    let alpha = alpha.clamp(0.0, 1.0);
    let mut sorted = scores.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = sorted.len() as f64;
    let rank = ((n + 1.0) * (1.0 - alpha)).ceil() as usize;
    if rank == 0 {
        return Some(sorted[0]);
    }
    if rank > sorted.len() {
        return Some(sorted[sorted.len() - 1]);
    }
    Some(sorted[rank - 1])
}

async fn fit_stats(pool: &PgPool, sample_limit: i64) -> Result<(OodStats, usize)> {
    let rows = sqlx::query(
        "SELECT embedding FROM brain_observations
         WHERE embedding IS NOT NULL AND observation_type != 'superseded'
           AND trust = 'trusted'
         ORDER BY id LIMIT $1",
    )
    .bind(sample_limit)
    .fetch_all(pool)
    .await
    .context("reading embeddings for calibration")?;

    let embeddings: Vec<Vec<f32>> = rows
        .iter()
        .filter_map(|r| {
            r.try_get::<pgvector::Vector, _>("embedding")
                .ok()
                .map(|v| v.to_vec())
        })
        .collect();

    let n = embeddings.len();
    let stats = OodStats::fit(&embeddings)
        .context("could not fit the embedding distribution (too few or degenerate samples)")?;
    Ok((stats, n))
}

pub async fn corpus_distances(pool: &PgPool, stats: &OodStats, limit: i64) -> Result<Vec<f64>> {
    let rows = sqlx::query(
        "SELECT embedding FROM brain_observations
         WHERE embedding IS NOT NULL AND observation_type != 'superseded'
           AND trust = 'trusted'
         ORDER BY id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<pgvector::Vector, _>("embedding").ok())
        .filter_map(|v| stats.mahalanobis(&v.to_vec()))
        .collect())
}

pub async fn query_distances(stats: &OodStats, queries: &[String]) -> Result<Vec<f64>> {
    let mut out = Vec::with_capacity(queries.len());
    for q in queries {
        let v = crate::embeddings::onnx::embed_passage(q)
            .await
            .with_context(|| format!("embedding calibration query: {q}"))?;
        if let Some(d) = stats.mahalanobis(&v) {
            out.push(d);
        }
    }
    Ok(out)
}

#[derive(Debug, serde::Serialize)]
pub struct CalibrationReport {
    pub embedding_dim: usize,
    pub fit_samples: usize,
    pub theoretical_threshold: f64,
    pub corpus: Option<Distribution>,
    pub queries: Option<Distribution>,
    pub alpha: f64,
    pub conformal_threshold: Option<f64>,
    pub theoretical_rejects_corpus: f64,
}

pub async fn calibrate(
    pool: &PgPool,
    calibration_queries: &[String],
    alpha: f64,
    sample_limit: i64,
) -> Result<CalibrationReport> {
    let (stats, fit_samples) = fit_stats(pool, sample_limit).await?;
    let dim = crate::embeddings::onnx::embedding_dim();
    let theoretical = super::ood::default_threshold(dim);

    let corpus_d = corpus_distances(pool, &stats, sample_limit).await?;
    let rejects = if corpus_d.is_empty() {
        0.0
    } else {
        corpus_d.iter().filter(|&&d| d > theoretical).count() as f64 / corpus_d.len() as f64
    };

    let query_d = query_distances(&stats, calibration_queries).await?;
    let conformal = conformal_quantile(&query_d, alpha);

    Ok(CalibrationReport {
        embedding_dim: dim,
        fit_samples,
        theoretical_threshold: theoretical,
        corpus: Distribution::from(corpus_d),
        queries: Distribution::from(query_d),
        alpha,
        conformal_threshold: conformal,
        theoretical_rejects_corpus: rejects,
    })
}

pub const OOD_THRESHOLD_KEY: &str = "ood_threshold";

pub async fn store_ood_threshold(
    pool: &PgPool,
    threshold: f64,
    report: &CalibrationReport,
) -> Result<()> {
    let metadata = serde_json::json!({
        "embedding_dim": report.embedding_dim,
        "alpha": report.alpha,
        "fit_samples": report.fit_samples,
        "calibration_queries": report.queries.as_ref().map(|q| q.n),
        "theoretical_threshold": report.theoretical_threshold,
        "method": "conformal",
    });
    sqlx::query(
        "INSERT INTO brain_calibration (key, value, metadata, updated_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (key) DO UPDATE
           SET value = EXCLUDED.value,
               metadata = EXCLUDED.metadata,
               updated_at = now()",
    )
    .bind(OOD_THRESHOLD_KEY)
    .bind(threshold)
    .bind(&metadata)
    .execute(pool)
    .await
    .context("persisting the calibrated OOD threshold")?;
    Ok(())
}

pub async fn load_ood_threshold(pool: &PgPool, dim: usize) -> Option<f64> {
    let row = sqlx::query("SELECT value, metadata FROM brain_calibration WHERE key = $1")
        .bind(OOD_THRESHOLD_KEY)
        .fetch_optional(pool)
        .await
        .ok()??;

    let value: f64 = row.try_get("value").ok()?;
    let metadata: serde_json::Value = row.try_get("metadata").ok()?;
    let stored_dim = metadata
        .get("embedding_dim")
        .and_then(serde_json::Value::as_u64)? as usize;

    if stored_dim != dim {
        tracing::warn!(
            stored_dim,
            current_dim = dim,
            "calibrated OOD threshold was measured for a different embedding dimension — ignoring it; re-run `memory-industry calibrate`"
        );
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conformal_quantile_is_the_rank_the_theory_asks_for() {
        let scores: Vec<f64> = (1..=9).map(f64::from).collect();
        assert_eq!(conformal_quantile(&scores, 0.1), Some(9.0));
        assert_eq!(conformal_quantile(&scores, 0.5), Some(5.0));
    }

    #[test]
    fn too_few_points_to_certify_alpha_gives_the_most_conservative_threshold() {
        let scores: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(conformal_quantile(&scores, 0.01), Some(10.0));
    }

    #[test]
    fn an_empty_calibration_set_yields_no_threshold() {
        assert_eq!(conformal_quantile(&[], 0.05), None);
    }

    #[test]
    fn the_distribution_summary_is_ordered() {
        let d = Distribution::from(vec![5.0, 1.0, 3.0, 2.0, 4.0]).expect("non-empty");
        assert_eq!(d.n, 5);
        assert_eq!(d.min, 1.0);
        assert_eq!(d.max, 5.0);
        assert!(d.p50 <= d.p90 && d.p90 <= d.p99);
    }
}
