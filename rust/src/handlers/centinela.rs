use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        "create" => create(pool, &args).await,
        "list" => list(pool).await,
        "delete" => delete(pool, &args).await,
        "check" => {
            let entity_name = args
                .get("entity_pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let condition = args
                .get("condition_type")
                .and_then(|v| v.as_str())
                .unwrap_or("on_access");
            let triggered = check_triggers(pool, entity_name, condition).await?;
            Ok(serde_json::json!({
                "action": "check",
                "triggered": triggered,
                "count": triggered.len()
            }))
        }
        _ => anyhow::bail!("Invalid action: {action}. Use create/list/delete/check"),
    }
}

async fn create(pool: &PgPool, args: &Value) -> Result<Value> {
    let entity_pattern = args
        .get("entity_pattern")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let condition_type = args
        .get("condition_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let from_agent = crate::session::current_client();

    if entity_pattern.is_empty() {
        anyhow::bail!("entity_pattern is required");
    }
    if message.is_empty() {
        anyhow::bail!("message is required");
    }

    crate::redact::refuse_secrets(args, "message", message)?;

    let valid_conditions = ["on_access", "on_session_start", "on_error_match"];
    if !valid_conditions.contains(&condition_type) {
        anyhow::bail!(
            "Invalid condition_type: {condition_type}. Use on_access/on_session_start/on_error_match"
        );
    }

    let max_fires = args
        .get("max_fires")
        .and_then(|v| v.as_i64())
        .unwrap_or(1)
        .clamp(-1, 10_000) as i32;
    let expires_at = args
        .get("expires_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc));

    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_triggers
            (entity_pattern, condition_type, message, max_fires, expires_at, from_agent)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(entity_pattern)
    .bind(condition_type)
    .bind(message)
    .bind(max_fires)
    .bind(expires_at)
    .bind(from_agent.as_deref())
    .fetch_one(pool)
    .await
    .context("failed to create trigger")?;

    tracing::info!(
        trigger_id = %row.0,
        pattern = %entity_pattern,
        condition = %condition_type,
        "trigger created"
    );

    Ok(serde_json::json!({
        "action": "create",
        "trigger_id": row.0.to_string(),
        "entity_pattern": entity_pattern,
        "condition_type": condition_type,
        "message": message,
        "max_fires": max_fires
    }))
}

async fn list(pool: &PgPool) -> Result<Value> {
    type TriggerRow = (
        uuid::Uuid,
        String,
        String,
        String,
        bool,
        i32,
        i32,
        chrono::DateTime<chrono::Utc>,
        Option<chrono::DateTime<chrono::Utc>>,
    );
    let triggers: Vec<TriggerRow> = sqlx::query_as(
        "SELECT id, entity_pattern, condition_type, message, active,
                fire_count, max_fires, created_at, expires_at
         FROM brain_triggers
         WHERE active = TRUE AND (expires_at IS NULL OR expires_at > NOW())
         ORDER BY created_at DESC
         LIMIT 50",
    )
    .fetch_all(pool)
    .await?;

    let items: Vec<Value> = triggers
        .iter()
        .map(
            |(id, pattern, cond, msg, active, fires, max_f, created, expires)| {
                serde_json::json!({
                    "id": id.to_string(),
                    "entity_pattern": pattern,
                    "condition_type": cond,
                    "message": msg,
                    "active": active,
                    "fire_count": fires,
                    "max_fires": max_f,
                    "created_at": created.to_rfc3339(),
                    "expires_at": expires.map(|e| e.to_rfc3339())
                })
            },
        )
        .collect();

    let count = items.len();
    Ok(serde_json::json!({
        "action": "list",
        "triggers": items,
        "count": count
    }))
}

async fn delete(pool: &PgPool, args: &Value) -> Result<Value> {
    let trigger_id = args
        .get("trigger_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let id: uuid::Uuid = trigger_id.parse().context("invalid trigger_id UUID")?;

    let result = sqlx::query("DELETE FROM brain_triggers WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        anyhow::bail!("Trigger not found");
    }

    Ok(serde_json::json!({
        "action": "delete",
        "trigger_id": trigger_id
    }))
}

pub async fn check_triggers(
    pool: &PgPool,
    entity_name: &str,
    condition: &str,
) -> Result<Vec<Value>> {
    if entity_name.is_empty() {
        return Ok(vec![]);
    }

    type TrigRow = (uuid::Uuid, String, String, i32, i32, Option<String>);
    let client = crate::session::current_client();
    let triggers: Vec<TrigRow> = if condition == "on_session_start" {
        // Exact match only: session name OR Mcp-Client-Id. Similarity caused
        // silent cross-delivery between similarly named agents.
        sqlx::query_as(
            "SELECT id, entity_pattern, message, fire_count, max_fires, from_agent
             FROM brain_triggers
             WHERE active = TRUE
               AND (expires_at IS NULL OR expires_at > NOW())
               AND condition_type = $1
               AND (
                    entity_pattern = $2
                 OR ($3::text IS NOT NULL AND entity_pattern = $3)
               )
             LIMIT 10",
        )
        .bind(condition)
        .bind(entity_name)
        .bind(client.as_deref())
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    } else {
        sqlx::query_as(
            "SELECT id, entity_pattern, message, fire_count, max_fires, from_agent
             FROM brain_triggers
             WHERE active = TRUE
               AND (expires_at IS NULL OR expires_at > NOW())
               AND condition_type = $1
               AND (entity_pattern = $2 OR similarity(entity_pattern, $2) > 0.5)
             LIMIT 10",
        )
        .bind(condition)
        .bind(entity_name)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
    };

    let mut fired: Vec<Value> = Vec::new();

    for (id, pattern, message, fire_count, max_fires, from_agent) in &triggers {
        let new_count = fire_count + 1;
        let deactivate = *max_fires > 0 && new_count >= *max_fires;

        if let Err(e) =
            sqlx::query("UPDATE brain_triggers SET fire_count = $2, active = $3 WHERE id = $1")
                .bind(id)
                .bind(new_count)
                .bind(!deactivate)
                .execute(pool)
                .await
        {
            tracing::warn!(trigger_id = %id, error = %e, "failed to update trigger fire_count");
        }

        fired.push(serde_json::json!({
            "trigger_id": id.to_string(),
            "entity_pattern": pattern,
            "message": message,
            "from_agent": from_agent,
            "fire_count": new_count,
            "deactivated": deactivate
        }));
    }

    Ok(fired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://centinela-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn a_reminder_that_carries_a_credential_is_refused_before_it_can_fire() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "action": "create",
            "entity_pattern": "deploy",
            "condition_type": "on_access",
            "message": "acordate de rotar ghp_abcdefghijklmnop"
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "create answered Ok on a pool that cannot connect: nothing but the secret gate \
                 could have answered, and the gate must refuse"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("github token") && chain.contains("message"),
            "a trigger message is not stored and forgotten: it is pushed unprompted into the \
             response of whatever touches the matching entity, so a credential here reaches \
             clients that never searched for it. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }
}
