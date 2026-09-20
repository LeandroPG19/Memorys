use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

use super::zafra::safe_truncate;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let error_id_str = args.get("error_id").and_then(|v| v.as_str()).unwrap_or("");
    let solution = args.get("solution").and_then(|v| v.as_str()).unwrap_or("");

    if error_id_str.is_empty() || solution.is_empty() {
        anyhow::bail!("error_id and solution are required");
    }

    crate::redact::refuse_secrets(&args, "solution", solution)?;

    let error_id: uuid::Uuid = error_id_str.parse().context("invalid error_id UUID")?;
    let project_id = crate::project::current_project_id(pool).await?;

    let result = sqlx::query(
        "UPDATE brain_errors SET solution = $2, resolved = true, resolved_at = NOW()
         WHERE id = $1
           AND ($3::uuid IS NULL OR project_id = $3)",
    )
    .bind(error_id)
    .bind(solution)
    .bind(project_id)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        anyhow::bail!("Error not found: {error_id_str}");
    }

    let similar: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT e2.id, e2.error_message FROM brain_errors e1
         JOIN brain_errors e2 ON similarity(e1.error_message, e2.error_message) > 0.5
         WHERE e1.id = $1 AND e2.resolved = false AND e2.id != $1 LIMIT 5",
    )
    .bind(error_id)
    .fetch_all(pool)
    .await?;

    let cross_refs: Vec<Value> = similar.iter().map(|(id, msg)| {
        serde_json::json!({"id": id.to_string(), "error_message": safe_truncate(msg, 100)})
    }).collect();

    Ok(serde_json::json!({
        "error_id": error_id_str,
        "resolved": true,
        "solution": solution,
        "similar_unresolved": cross_refs,
        "similar_count": cross_refs.len()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://remedio-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn a_solution_that_pastes_the_credential_it_used_is_refused() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "error_id": uuid::Uuid::nil().to_string(),
            "solution": "se arregló exportando GITHUB_TOKEN=ghp_abcdefghijklmnop antes del deploy"
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "remedio answered Ok on a pool that cannot connect: nothing but the secret gate \
                 could have answered, and the gate must refuse"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("token field") && chain.contains("solution"),
            "the credential in the solution reached the database. cuba_alarma already refuses \
             the error half of the pair, so leaving the fix half open means the credential lands \
             in the row the agent reads back first — the fix is what gets copied. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    #[tokio::test]
    async fn a_fix_that_merely_talks_about_a_password_is_not_a_credential() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "error_id": uuid::Uuid::nil().to_string(),
            "solution": "el arreglo fue validar la password antes de guardarla, y rotar el token \
                         de sesión cuando el login falla cinco veces"
        });

        let chain = format!(
            "{:#}",
            handle(&pool, args)
                .await
                .expect_err("the pool cannot connect, so this call always ends in an error")
        );
        assert!(
            !chain.contains("Remove it and store a pointer"),
            "prose about credentials is what a solution is normally made of: refusing it would \
             lose the very fix the agent tried to record, and it would learn to stop writing \
             solutions. The call must have died at the database instead. Got: {chain}"
        );
    }
}
