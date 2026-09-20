use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::project;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        "list" => list(pool).await,
        "current" => current(pool).await,
        "switch" => {
            let name = required_str(&args, "name")?;
            switch(pool, name).await
        }
        "stats" => {
            let name = args.get("name").and_then(|v| v.as_str());
            stats(pool, name).await
        }
        "rename" => {
            let from = required_str(&args, "name")?;
            let to = required_str(&args, "to")?;
            rename(pool, from, to).await
        }
        "merge" => {
            let from = required_str(&args, "name")?;
            let to = required_str(&args, "to")?;
            merge(pool, from, to).await
        }
        "backfill" => {
            let name = required_str(&args, "name")?;
            let confirm = args
                .get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            backfill(pool, name, confirm).await
        }
        _ => anyhow::bail!("Invalid action: {action}"),
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string field: {key}"))
}

async fn list(pool: &PgPool) -> Result<Value> {
    type Row = (
        Uuid,
        String,
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
        i64,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT p.id, p.name, p.created_at, p.last_active_at,
                (SELECT COUNT(*) FROM brain_entities e WHERE e.project_id = p.id) AS entity_count
         FROM brain_projects p
         ORDER BY p.last_active_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("failed to list projects")?;

    let projects: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, created_at, last_active_at, entity_count)| {
            serde_json::json!({
                "id": id.to_string(),
                "name": name,
                "created_at": created_at.to_rfc3339(),
                "last_active_at": last_active_at.to_rfc3339(),
                "entity_count": entity_count,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "action": "list",
        "projects": projects.clone(),
        "count": projects.len(),
    }))
}

async fn current(pool: &PgPool) -> Result<Value> {
    let pid = project::current_project_id(pool).await?;
    let body = match pid {
        Some(id) => {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT name FROM brain_projects WHERE id = $1")
                    .bind(id)
                    .fetch_optional(pool)
                    .await?;
            serde_json::json!({
                "id": id.to_string(),
                "name": row.map(|(n,)| n),
            })
        }
        None => Value::Null,
    };
    Ok(serde_json::json!({"action": "current", "project": body}))
}

async fn switch(pool: &PgPool, name: &str) -> Result<Value> {
    let pid = project::upsert_project(pool, name).await?;

    let updated: Option<(Uuid,)> = match crate::session::session_id() {
        Some(sid) => {
            let mut tx = project::begin_write_scope(pool).await?;
            let row: Option<(Uuid,)> = sqlx::query_as(
                "UPDATE brain_sessions SET project_id = $1
                 WHERE id = $2 AND ended_at IS NULL
                 RETURNING id",
            )
            .bind(pid)
            .bind(sid)
            .fetch_optional(&mut *tx)
            .await?;
            tx.commit().await?;
            if row.is_some() {
                crate::session::set(sid, Some(pid));
            }
            row
        }
        None => None,
    };

    Ok(serde_json::json!({
        "action": "switch",
        "project": {"id": pid.to_string(), "name": name},
        "bound_to_session": updated.map(|(id,)| id.to_string()),
    }))
}

async fn backfill(pool: &PgPool, name: &str, confirm: bool) -> Result<Value> {
    let pid = project::upsert_project(pool, name).await?;
    let body = project::backfill_unscoped(pool, pid, confirm).await?;
    Ok(serde_json::json!({
        "action": "backfill",
        "project": {"id": pid.to_string(), "name": name},
        "result": body,
    }))
}

async fn stats(pool: &PgPool, name: Option<&str>) -> Result<Value> {
    let pid = match name {
        Some(n) => project::resolve_project_name(pool, n).await?,
        None => project::current_project_id(pool).await?,
    };

    let pid = match pid {
        Some(id) => id,
        None => {
            return Ok(serde_json::json!({
                "action": "stats",
                "project": null,
                "note": "no active project — pass `name` or start a session with --project",
            }));
        }
    };

    let entities: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM brain_entities WHERE project_id = $1")
            .bind(pid)
            .fetch_one(pool)
            .await?;
    let observations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM brain_observations WHERE project_id = $1")
            .bind(pid)
            .fetch_one(pool)
            .await?;
    let episodes: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM brain_episodes WHERE project_id = $1")
            .bind(pid)
            .fetch_one(pool)
            .await?;
    let sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM brain_sessions WHERE project_id = $1")
            .bind(pid)
            .fetch_one(pool)
            .await?;
    let errors: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM brain_errors WHERE project_id = $1")
        .bind(pid)
        .fetch_one(pool)
        .await?;
    let relations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM brain_relations WHERE project_id = $1")
            .bind(pid)
            .fetch_one(pool)
            .await?;

    Ok(serde_json::json!({
        "action": "stats",
        "project_id": pid.to_string(),
        "entities": entities,
        "observations": observations,
        "episodes": episodes,
        "sessions": sessions,
        "errors": errors,
        "relations": relations,
    }))
}

async fn rename(pool: &PgPool, from: &str, to: &str) -> Result<Value> {
    let updated: Option<(Uuid,)> =
        sqlx::query_as("UPDATE brain_projects SET name = $2 WHERE name = $1 RETURNING id")
            .bind(from)
            .bind(to)
            .fetch_optional(pool)
            .await
            .context("rename failed")?;
    Ok(serde_json::json!({
        "action": "rename",
        "from": from,
        "to": to,
        "renamed": updated.is_some(),
    }))
}

async fn merge(pool: &PgPool, from: &str, to: &str) -> Result<Value> {
    let from_id = project::resolve_project_name(pool, from)
        .await?
        .ok_or_else(|| anyhow::anyhow!("source project not found: {from}"))?;
    let to_id = project::upsert_project(pool, to).await?;

    let mut tx = pool.begin().await?;
    let tables = [
        "brain_entities",
        "brain_observations",
        "brain_episodes",
        "brain_sessions",
        "brain_errors",
        "brain_relations",
    ];
    let mut moved: Vec<(String, u64)> = Vec::with_capacity(tables.len());
    for t in tables {
        let q = format!("UPDATE {t} SET project_id = $1 WHERE project_id = $2");
        let rows = sqlx::query(&q)
            .bind(to_id)
            .bind(from_id)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("merge UPDATE failed on {t}"))?;
        moved.push((t.to_string(), rows.rows_affected()));
    }
    sqlx::query("DELETE FROM brain_projects WHERE id = $1")
        .bind(from_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;

    let moved_json: serde_json::Map<String, Value> = moved
        .into_iter()
        .map(|(t, n)| (t, serde_json::json!(n)))
        .collect();
    Ok(serde_json::json!({
        "action": "merge",
        "from": from,
        "to": to,
        "from_id": from_id.to_string(),
        "to_id": to_id.to_string(),
        "rows_moved": moved_json,
    }))
}
