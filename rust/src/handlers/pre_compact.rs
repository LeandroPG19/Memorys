use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "snapshot" => snapshot(pool).await,
        "restore" => restore(pool).await,
        _ => anyhow::bail!("Invalid action: {action}. Use snapshot/restore"),
    }
}

async fn snapshot(pool: &PgPool) -> Result<Value> {
    let session_id = match crate::session::session_id() {
        Some(id) => id,
        None => {
            return Ok(serde_json::json!({
                "action": "snapshot",
                "session_id": null,
                "summary_md": "",
                "note": "no active session — start one with cuba_jornada start"
            }));
        }
    };

    let project_id: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT project_id FROM brain_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(pool)
            .await?
            .flatten();

    let summary_md = super::eco::reflect(pool, session_id).await?;

    let key_obs: Vec<(uuid::Uuid, String, String, String)> = sqlx::query_as(
        "SELECT o.id, e.name, o.observation_type, o.content
         FROM brain_observations o
         JOIN brain_entities e ON o.entity_id = e.id
         WHERE o.session_id = $1 AND o.observation_type != 'superseded'
         ORDER BY o.created_at DESC
         LIMIT 50",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let key_obs_json: Vec<Value> = key_obs
        .iter()
        .map(|(id, ent, ty, content)| {
            serde_json::json!({
                "id": id.to_string(),
                "entity": ent,
                "type": ty,
                "content": super::zafra::safe_truncate(content, 240),
            })
        })
        .collect();

    let decisions: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT o.id, e.name, o.content
         FROM brain_observations o
         JOIN brain_entities e ON o.entity_id = e.id
         WHERE o.session_id = $1 AND o.observation_type = 'decision'
         ORDER BY o.created_at DESC
         LIMIT 20",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let decisions_json: Vec<Value> = decisions
        .iter()
        .map(|(id, ent, content)| {
            serde_json::json!({
                "id": id.to_string(),
                "entity": ent,
                "content": super::zafra::safe_truncate(content, 240),
            })
        })
        .collect();

    let unresolved: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT id, error_type, error_message
         FROM brain_errors
         WHERE resolved = FALSE
           AND ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY created_at DESC
         LIMIT 20",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let unresolved_json: Vec<Value> = unresolved
        .iter()
        .map(|(id, ty, msg)| {
            serde_json::json!({
                "id": id.to_string(),
                "error_type": ty,
                "message": super::zafra::safe_truncate(msg, 160),
            })
        })
        .collect();

    let pending: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id, content
         FROM brain_observations
         WHERE session_id = $1 AND embedding IS NULL
         ORDER BY created_at DESC
         LIMIT 20",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let pending_json: Vec<Value> = pending
        .iter()
        .map(|(id, c)| {
            serde_json::json!({
                "id": id.to_string(),
                "content_preview": super::zafra::safe_truncate(c, 80),
            })
        })
        .collect();

    let goals_row: Option<(Value,)> =
        sqlx::query_as("SELECT goals FROM brain_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(pool)
            .await?;
    let active_goals = goals_row
        .map(|(g,)| g)
        .unwrap_or_else(|| Value::Array(vec![]));

    let snap: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_compaction_snapshots
            (session_id, project_id, summary_md, key_observations, decisions,
             unresolved_errors, pending_embeddings, active_goals, obs_count)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id",
    )
    .bind(session_id)
    .bind(project_id)
    .bind(&summary_md)
    .bind(Value::Array(key_obs_json.clone()))
    .bind(Value::Array(decisions_json.clone()))
    .bind(Value::Array(unresolved_json.clone()))
    .bind(Value::Array(pending_json.clone()))
    .bind(&active_goals)
    .bind(key_obs.len() as i32)
    .fetch_one(pool)
    .await
    .context("failed to persist compaction snapshot")?;

    tracing::info!(
        session_id = %session_id,
        snapshot_id = %snap.0,
        obs = key_obs.len(),
        decisions = decisions.len(),
        unresolved = unresolved.len(),
        pending_emb = pending.len(),
        "compaction snapshot persisted"
    );

    Ok(serde_json::json!({
        "action": "snapshot",
        "snapshot_id": snap.0.to_string(),
        "session_id": session_id.to_string(),
        "summary_md": summary_md,
        "key_observations": key_obs_json,
        "decisions": decisions_json,
        "unresolved_errors": unresolved_json,
        "pending_embeddings": pending_json,
        "active_goals": active_goals,
        "obs_count": key_obs.len(),
        "saved": {
            "observations": key_obs.len(),
            "decisions": decisions.len(),
            "unresolved_errors": unresolved.len(),
            "pending_embeddings": pending.len(),
            "summary_chars": summary_md.len(),
        },
        "discarded": {
            "raw_turns": "not retained — only structured snapshot fields survive compact",
            "observations_beyond_limit": "session observations after the newest 50",
            "note": "pre_compact does not delete graph rows; it only freezes a recoverable slice",
        },
        "transparency": crate::handlers::contexto::transparency_block(
            summary_md.len(),
            "raw conversation turns outside this snapshot are not retained by pre_compact; \
             restore re-injects summary_md + structured lists only",
        ),
    }))
}

async fn restore(pool: &PgPool) -> Result<Value> {
    let session_id = match crate::session::session_id() {
        Some(id) => id,
        None => {
            return Ok(serde_json::json!({
                "action": "restore",
                "snapshot": null,
                "note": "no active session"
            }));
        }
    };

    type Row = (
        uuid::Uuid,
        String,
        Value,
        Value,
        Value,
        Value,
        Value,
        i32,
        chrono::DateTime<chrono::Utc>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, summary_md, key_observations, decisions, unresolved_errors,
                pending_embeddings, active_goals, obs_count, created_at
         FROM brain_compaction_snapshots
         WHERE session_id = $1
         ORDER BY created_at DESC
         LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Ok(serde_json::json!({
            "action": "restore",
            "snapshot": null,
            "session_id": session_id.to_string(),
            "note": "no snapshot found for this session — call snapshot first"
        })),
        Some((id, summary_md, key_obs, decisions, unresolved, pending, goals, obs_count, ts)) => {
            Ok(serde_json::json!({
                "action": "restore",
                "session_id": session_id.to_string(),
                "snapshot": {
                    "id": id.to_string(),
                    "summary_md": summary_md,
                    "key_observations": key_obs,
                    "decisions": decisions,
                    "unresolved_errors": unresolved,
                    "pending_embeddings": pending,
                    "active_goals": goals,
                    "obs_count": obs_count,
                    "created_at": ts.to_rfc3339(),
                }
            }))
        }
    }
}
