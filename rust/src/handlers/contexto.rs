use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashSet;

const DEFAULT_BUDGET_CHARS: usize = 12_000;
const RECALL_CAP: usize = 15;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let budget = args
        .get("budget_chars")
        .and_then(|v| v.as_i64())
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_BUDGET_CHARS)
        .clamp(1_000, 100_000);

    let project_id = crate::project::current_project_id(pool).await?;
    let session_id = crate::session::session_id();
    let client = crate::session::current_client();

    let wm: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT content, tag FROM brain_wm
         WHERE expires_at > NOW()
           AND session_id IS NOT DISTINCT FROM $1
           AND ($2::uuid IS NULL OR project_id = $2 OR project_id IS NULL)
         ORDER BY created_at DESC
         LIMIT 20",
    )
    .bind(session_id)
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let notes: Vec<(String, Option<String>)> = if let Some(ref cid) = client {
        sqlx::query_as(
            "SELECT message, from_agent FROM brain_triggers
             WHERE active = TRUE
               AND condition_type = 'on_session_start'
               AND (expires_at IS NULL OR expires_at > NOW())
               AND entity_pattern = $1
             ORDER BY created_at DESC
             LIMIT 10",
        )
        .bind(cid)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    let artifacts: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT path, version, content_hash FROM brain_artifacts
         WHERE (
                ($1::uuid IS NULL AND project_id IS NULL)
             OR ($1::uuid IS NOT NULL AND (project_id IS NOT DISTINCT FROM $1 OR project_id IS NULL))
           )
         ORDER BY updated_at DESC
         LIMIT 30",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let session_writes: Vec<(String, String, f64)> = if let Some(sid) = session_id {
        sqlx::query_as(
            "SELECT e.name, left(o.content, 240), o.importance::float8
             FROM brain_observations o
             JOIN brain_entities e ON e.id = o.entity_id
             WHERE o.session_id = $1
               AND o.observation_type != 'superseded'
               AND ($2::uuid IS NULL OR o.project_id = $2 OR o.project_id IS NULL)
             ORDER BY o.created_at DESC
             LIMIT 8",
        )
        .bind(sid)
        .bind(project_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    let wm_blob: String = wm
        .iter()
        .map(|(c, _)| c.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let wm_mentions: Vec<(String, String, f64)> = if wm_blob.chars().count() >= 4 {
        let names: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM brain_entities WHERE char_length(name) >= 4")
                .fetch_all(pool)
                .await
                .unwrap_or_default();
        let hits =
            crate::search::mentions::names_in_query(&wm_blob, names.iter().map(|(n,)| n.as_str()));
        if hits.is_empty() {
            Vec::new()
        } else {
            sqlx::query_as(
                "SELECT e.name, left(o.content, 240), o.importance::float8
                 FROM brain_observations o
                 JOIN brain_entities e ON e.id = o.entity_id
                 WHERE e.name = ANY($1)
                   AND o.observation_type != 'superseded'
                   AND o.trust = 'trusted'
                   AND ($2::uuid IS NULL OR o.project_id = $2 OR o.project_id IS NULL)
                 ORDER BY o.importance DESC NULLS LAST, o.created_at DESC
                 LIMIT 8",
            )
            .bind(&hits)
            .bind(project_id)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        }
    } else {
        Vec::new()
    };

    let importance_fallback: Vec<(String, String, f64)> = sqlx::query_as(
        "SELECT e.name, left(o.content, 240), o.importance::float8
         FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE ($1::uuid IS NULL OR o.project_id = $1 OR o.project_id IS NULL)
         ORDER BY o.importance DESC NULLS LAST, o.updated_at DESC NULLS LAST
         LIMIT 15",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let recall = merge_recall(session_writes, wm_mentions, importance_fallback, RECALL_CAP);

    let layers = serde_json::json!({
        "working_memory": wm.iter().map(|(c, t)| serde_json::json!({"content": c, "tag": t})).collect::<Vec<_>>(),
        "agent_notes": notes.iter().map(|(m, f)| serde_json::json!({"message": m, "from_agent": f})).collect::<Vec<_>>(),
        "artifacts": artifacts.iter().map(|(p, v, h)| serde_json::json!({"path": p, "version": v, "content_hash": h})).collect::<Vec<_>>(),
        "top_recall": recall.iter().map(|(n, c, i)| serde_json::json!({"entity": n, "snippet": c, "importance": i})).collect::<Vec<_>>(),
    });

    let (layers, truncated) = apply_char_budget(layers, budget);

    Ok(serde_json::json!({
        "action": "contexto",
        "client_id": client,
        "session_id": session_id.map(|s| s.to_string()),
        "project_id": project_id.map(|p| p.to_string()),
        "budget_chars": budget,
        "truncated": truncated,
        "layers": layers,
    }))
}

/// Compact transparency helper used by pre_compact after it runs.
pub fn transparency_block(saved_summary_chars: usize, discarded_hint: &str) -> Value {
    serde_json::json!({
        "saved_summary_chars": saved_summary_chars,
        "discarded": discarded_hint,
        "note": "pre_compact persists a recoverable snapshot; this block is the receipt"
    })
}

#[allow(dead_code)]
pub async fn ensure_table_exists(pool: &PgPool) -> Result<()> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.tables
         WHERE table_name = 'brain_artifacts'",
    )
    .fetch_one(pool)
    .await
    .context("probe artifacts table")?;
    if n == 0 {
        anyhow::bail!("brain_artifacts missing — run migrations");
    }
    Ok(())
}

pub fn merge_recall(
    session: Vec<(String, String, f64)>,
    wm: Vec<(String, String, f64)>,
    importance: Vec<(String, String, f64)>,
    cap: usize,
) -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for src in [session, wm, importance] {
        for row in src {
            let key = format!("{}|{}", row.0.to_ascii_lowercase(), row.1);
            if seen.insert(key) {
                out.push(row);
            }
            if out.len() >= cap {
                return out;
            }
        }
    }
    out
}

fn apply_char_budget(layers: Value, budget: usize) -> (Value, bool) {
    let text = serde_json::to_string(&layers).unwrap_or_default();
    if text.len() <= budget {
        return (layers, false);
    }
    let mut cap = budget.saturating_sub(64);
    loop {
        let mut preview = text.clone();
        preview.truncate(cap);
        let out = serde_json::json!({
            "truncated": true,
            "budget_chars": budget,
            "preview": preview,
        });
        let encoded = serde_json::to_string(&out).unwrap_or_default();
        if encoded.len() <= budget || cap == 0 {
            return (out, true);
        }
        cap = cap.saturating_sub(encoded.len().saturating_sub(budget));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_writes_win_over_global_importance() {
        let session = vec![("PostgreSQL".into(), "puerto 5432".into(), 0.01)];
        let importance = vec![("Mapupita-Web".into(), "inventario QR".into(), 0.99)];
        let merged = merge_recall(session, Vec::new(), importance, 15);
        assert_eq!(merged[0].0, "PostgreSQL");
        assert!(merged.iter().any(|(e, _, _)| e == "Mapupita-Web"));
    }

    #[test]
    fn recall_respects_cap() {
        let importance: Vec<_> = (0..20)
            .map(|i| (format!("E{i}"), format!("s{i}"), 0.5))
            .collect();
        assert_eq!(
            merge_recall(Vec::new(), Vec::new(), importance, 15).len(),
            15
        );
    }

    #[test]
    fn context_payload_never_exceeds_budget_chars() {
        let fat = serde_json::json!({
            "top_recall": (0..40).map(|i| serde_json::json!({"entity": format!("E{i}"), "snippet": "x".repeat(400)})).collect::<Vec<_>>()
        });
        let (out, truncated) = apply_char_budget(fat, 1_200);
        assert!(truncated);
        let text = serde_json::to_string(&out).unwrap();
        assert!(
            text.len() <= 1_200,
            "memory://context must stay within budget_chars, got {}",
            text.len()
        );
        assert_eq!(out["budget_chars"], 1_200);
    }
}
