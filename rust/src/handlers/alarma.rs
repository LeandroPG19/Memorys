use crate::degraded::LostSignals;
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

use super::zafra::safe_truncate;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let error_type = args
        .get("error_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let error_message = args
        .get("error_message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let context = args
        .get("context")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));
    let project = args
        .get("project")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    if error_message.is_empty() {
        anyhow::bail!("error_message is required");
    }

    crate::redact::refuse_secrets(&args, "error_message", error_message)?;
    let context_text = serde_json::to_string_pretty(&context)
        .context("failed to serialise the error context for the secret scan")?;
    crate::redact::refuse_secrets(&args, "context", &context_text)?;

    let project_id = crate::project::current_project_id(pool).await?;

    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_errors (error_type, error_message, context, project, project_id) VALUES ($1, $2, $3, $4, $5) RETURNING id"
    )
    .bind(error_type)
    .bind(safe_truncate(error_message, 5000))
    .bind(&context)
    .bind(project)
    .bind(project_id)
    .fetch_one(pool)
    .await
    .context("failed to insert error")?;

    let similar_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_errors WHERE similarity(error_message, $1) > 0.5 AND project = $2 AND id != $3"
    )
    .bind(error_message)
    .bind(project)
    .bind(row.0)
    .fetch_one(pool)
    .await?;

    if similar_count.0 >= 3 {
        sqlx::query(
            "UPDATE brain_errors SET synapse_weight = LEAST(synapse_weight + 0.1, 5.0)
             WHERE similarity(error_message, $1) > 0.5 AND project = $2 AND id != $3",
        )
        .bind(error_message)
        .bind(project)
        .bind(row.0)
        .execute(pool)
        .await?;
    }

    let mut response = serde_json::json!({
        "error_id": row.0.to_string(),
        "error_type": error_type,
        "project": project,
        "similar_errors": similar_count.0
    });

    if similar_count.0 >= 3 {
        response["pattern_warning"] = serde_json::json!(format!(
            "⚠️ Pattern detected: {} similar errors in project '{}'",
            similar_count.0, project
        ));
    }

    let mut lost = LostSignals::default();
    let triggered = lost.note(
        "triggered_reminders",
        crate::handlers::centinela::check_triggers(pool, error_type, "on_error_match").await,
    );
    if !triggered.is_empty() {
        response["triggered_reminders"] = serde_json::json!(triggered);
    }
    lost.annotate(&mut response, what_a_lost_signal_costs);

    Ok(response)
}

fn what_a_lost_signal_costs(signal: &str) -> &'static str {
    match signal {
        "triggered_reminders" => {
            "los recordatorios de cuba_centinela para este error (si había uno, NO se disparó)"
        }
        _ => "una señal del reporte",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://alarma-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn an_error_report_refuses_a_credential_in_its_message_or_in_its_context() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let cases = [
            (
                "error_message",
                serde_json::json!({
                    "error_type": "HttpError",
                    "error_message": "401 al llamar con ghp_abcdefghijklmnop"
                }),
            ),
            (
                "context",
                serde_json::json!({
                    "error_type": "HttpError",
                    "error_message": "401 al llamar a la API",
                    "context": {"file": "deploy.rs", "header": "ghp_abcdefghijklmnop"}
                }),
            ),
        ];

        for (field, args) in cases {
            let Err(failure) = handle(&pool, args).await else {
                panic!(
                    "the report answered Ok on a pool that cannot connect: nothing but the \
                     secret gate could have answered, and the gate must refuse"
                );
            };

            let chain = format!("{failure:#}");
            assert!(
                chain.contains("github token"),
                "the token in {field} reached the database: a stack trace or a request context \
                 is exactly where a live header gets pasted without anyone reading it. Got: \
                 {chain}"
            );
            assert!(
                !chain.contains("ghp_abcdefghijklmnop"),
                "the refusal repeated the secret, and refusals get logged: {chain}"
            );
        }
    }
}
