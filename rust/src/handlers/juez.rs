use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::cognitive::judge::{Judgment, default_max_pairs, resolve_judge};
use crate::constants::{JUEZ_AMBIGUOUS_HI, JUEZ_AMBIGUOUS_LO};

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "judge_pair" => {
            let a = required_uuid(&args, "observation_a")?;
            let b = required_uuid(&args, "observation_b")?;
            judge_pair(pool, a, b).await
        }
        "scan_entity" => {
            let entity = args
                .get("entity_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow::anyhow!("entity_name is required"))?;
            let max = args
                .get("max_pairs")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or_else(default_max_pairs);
            scan_entity(pool, entity, max).await
        }
        _ => anyhow::bail!("Invalid action: {action}. Use judge_pair/scan_entity"),
    }
}

fn required_uuid(args: &Value, key: &str) -> Result<Uuid> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing field: {key}"))?
        .parse::<Uuid>()
        .with_context(|| format!("invalid UUID for {key}"))
}

async fn judge_pair(pool: &PgPool, a: Uuid, b: Uuid) -> Result<Value> {
    let (a, b) = if a < b { (a, b) } else { (b, a) };

    if let Some(j) = lookup_cached(pool, a, b).await? {
        return Ok(serde_json::json!({
            "action": "judge_pair",
            "cached": true,
            "observation_a": a.to_string(),
            "observation_b": b.to_string(),
            "judgment": judgment_to_json(&j),
        }));
    }

    let (content_a, content_b) = match fetch_obs_pair(pool, a, b).await? {
        Some(c) => c,
        None => anyhow::bail!("one or both observation IDs not found"),
    };

    let judge = resolve_judge();
    let judgment = judge
        .judge(&content_a, &content_b)
        .await
        .context("backend judge failed")?;

    persist_judgment(pool, a, b, &judgment).await?;

    Ok(serde_json::json!({
        "action": "judge_pair",
        "cached": false,
        "observation_a": a.to_string(),
        "observation_b": b.to_string(),
        "judgment": judgment_to_json(&judgment),
    }))
}

async fn scan_entity(pool: &PgPool, entity_name: &str, max_pairs: usize) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;

    type PairRow = (Uuid, String, Uuid, String, f64);
    let pairs: Vec<PairRow> = sqlx::query_as(
        "SELECT a.id, a.content, b.id, b.content,
                (1.0 - (a.embedding <=> b.embedding))::float8 AS cosine_sim
         FROM brain_observations a
         JOIN brain_observations b ON a.entity_id = b.entity_id AND a.id < b.id
         WHERE a.embedding IS NOT NULL AND b.embedding IS NOT NULL
           AND a.observation_type NOT IN ('superseded', 'tool_usage')
           AND b.observation_type NOT IN ('superseded', 'tool_usage')
           AND (1.0 - (a.embedding <=> b.embedding)) BETWEEN $2 AND $3
           AND a.entity_id = (SELECT id FROM brain_entities WHERE name = $1)
           AND ($4::uuid IS NULL OR a.project_id = $4)
           AND ($4::uuid IS NULL OR b.project_id = $4)
         ORDER BY ABS((1.0 - (a.embedding <=> b.embedding)) - 0.7) ASC
         LIMIT $5",
    )
    .bind(entity_name)
    .bind(JUEZ_AMBIGUOUS_LO)
    .bind(JUEZ_AMBIGUOUS_HI)
    .bind(project_id)
    .bind(max_pairs as i64)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let judge = resolve_judge();
    let backend_name = judge.backend_name().to_string();
    let mut results: Vec<Value> = Vec::with_capacity(pairs.len());
    let mut llm_calls = 0u32;
    let mut cache_hits = 0u32;

    for (id_a, content_a, id_b, content_b, cosine_sim) in pairs {
        if let Some(j) = lookup_cached(pool, id_a, id_b).await? {
            cache_hits += 1;
            results.push(serde_json::json!({
                "observation_a": id_a.to_string(),
                "observation_b": id_b.to_string(),
                "cosine_sim": cosine_sim,
                "cached": true,
                "judgment": judgment_to_json(&j),
            }));
            continue;
        }
        match judge.judge(&content_a, &content_b).await {
            Ok(j) => {
                llm_calls += 1;
                if let Err(e) = persist_judgment(pool, id_a, id_b, &j).await {
                    tracing::warn!(error=%e, "persist judgment failed");
                }
                results.push(serde_json::json!({
                    "observation_a": id_a.to_string(),
                    "observation_b": id_b.to_string(),
                    "cosine_sim": cosine_sim,
                    "cached": false,
                    "judgment": judgment_to_json(&j),
                }));
            }
            Err(e) => {
                tracing::warn!(error=%e, "judge backend failed for pair");
                results.push(serde_json::json!({
                    "observation_a": id_a.to_string(),
                    "observation_b": id_b.to_string(),
                    "cosine_sim": cosine_sim,
                    "cached": false,
                    "error": e.to_string(),
                }));
            }
        }
    }

    Ok(serde_json::json!({
        "action": "scan_entity",
        "entity_name": entity_name,
        "backend": backend_name,
        "ambiguous_band": [JUEZ_AMBIGUOUS_LO, JUEZ_AMBIGUOUS_HI],
        "llm_calls": llm_calls,
        "cache_hits": cache_hits,
        "results": results,
    }))
}

async fn fetch_obs_pair(pool: &PgPool, a: Uuid, b: Uuid) -> Result<Option<(String, String)>> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, content FROM brain_observations WHERE id IN ($1, $2)")
            .bind(a)
            .bind(b)
            .fetch_all(pool)
            .await?;
    if rows.len() != 2 {
        return Ok(None);
    }
    let mut ca = String::new();
    let mut cb = String::new();
    for (id, content) in rows {
        if id == a {
            ca = content;
        } else if id == b {
            cb = content;
        }
    }
    Ok(Some((ca, cb)))
}

async fn lookup_cached(pool: &PgPool, a: Uuid, b: Uuid) -> Result<Option<Judgment>> {
    type Row = (String, f64, Option<String>, String, Option<String>);
    let row: Option<Row> = sqlx::query_as(
        "SELECT j.verdict, j.confidence::float8, j.reason, j.judge_backend, j.judge_model
         FROM brain_judgments j
         JOIN brain_observations o ON o.id = j.observation_a
         WHERE j.observation_a = $1 AND j.observation_b = $2",
    )
    .bind(a)
    .bind(b)
    .fetch_optional(pool)
    .await?;
    Ok(
        row.map(|(verdict, confidence, reason, backend, model)| Judgment {
            verdict,
            confidence,
            reason,
            backend,
            model,
        }),
    )
}

async fn persist_judgment(pool: &PgPool, a: Uuid, b: Uuid, j: &Judgment) -> Result<()> {
    let project_id = crate::project::current_project_id(pool).await?;
    sqlx::query(
        "INSERT INTO brain_judgments
            (observation_a, observation_b, verdict, confidence, reason,
             judge_backend, judge_model, project_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (observation_a, observation_b) DO NOTHING",
    )
    .bind(a)
    .bind(b)
    .bind(&j.verdict)
    .bind(j.confidence)
    .bind(&j.reason)
    .bind(&j.backend)
    .bind(&j.model)
    .bind(project_id)
    .execute(pool)
    .await
    .context("persist judgment")?;
    Ok(())
}

fn judgment_to_json(j: &Judgment) -> Value {
    serde_json::json!({
        "verdict": j.verdict,
        "confidence": j.confidence,
        "reason": j.reason,
        "backend": j.backend,
        "model": j.model,
    })
}
