use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let entity_name = args
        .get("entity_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required");
    }

    let confirm = args
        .get("confirm")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !confirm {
        anyhow::bail!(
            "GDPR erasure is IRREVERSIBLE. Set confirm=true to proceed. \
             This will delete entity '{}' and ALL references across \
             observations, relations, errors, and sessions.",
            entity_name
        );
    }

    let project_id = crate::project::current_project_id(pool).await?;

    let mut tx = pool.begin().await.context("failed to begin transaction")?;

    let errors_deleted: (i64,) = sqlx::query_as(
        "WITH deleted AS (
            DELETE FROM brain_errors
            WHERE POSITION(LOWER($1) IN LOWER(error_message)) > 0
               OR POSITION(LOWER($1) IN LOWER(context::text)) > 0
            RETURNING 1
        ) SELECT COUNT(*) FROM deleted",
    )
    .bind(entity_name)
    .fetch_one(&mut *tx)
    .await
    .context("failed to delete errors")?;

    let sessions_deleted: (i64,) = sqlx::query_as(
        "WITH deleted AS (
            DELETE FROM brain_sessions
            WHERE POSITION(LOWER($1) IN LOWER(goals::text)) > 0
               OR POSITION(LOWER($1) IN LOWER(session_name)) > 0
               OR POSITION(LOWER($1) IN LOWER(COALESCE(summary, ''))) > 0
            RETURNING 1
        ) SELECT COUNT(*) FROM deleted",
    )
    .bind(entity_name)
    .fetch_one(&mut *tx)
    .await
    .context("failed to delete sessions")?;

    let entity_deleted = sqlx::query(
        "DELETE FROM brain_entities WHERE name = $1
         AND ($2::uuid IS NULL OR project_id = $2)",
    )
    .bind(entity_name)
    .bind(project_id)
    .execute(&mut *tx)
    .await
    .context("failed to delete entity")?;

    tx.commit().await.context("failed to commit transaction")?;

    let entity_found = entity_deleted.rows_affected() > 0;

    tracing::info!(
        entity = %entity_name,
        entity_deleted = entity_found,
        errors_purged = errors_deleted.0,
        sessions_purged = sessions_deleted.0,
        "GDPR erasure completed"
    );

    Ok(serde_json::json!({
        "action": "forget",
        "entity_name": entity_name,
        "entity_deleted": entity_found,
        "cascaded": {
            "observations": "via FK CASCADE",
            "episodes": "via FK CASCADE",
            "relations": "via FK CASCADE",
            "errors_purged": errors_deleted.0,
            "sessions_purged": sessions_deleted.0
        },
        "gdpr_compliance": "Right to Erasure (ARCO) satisfied"
    }))
}
