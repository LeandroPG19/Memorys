use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let entity_name = "architecture_decisions";

    let project_id = crate::project::current_project_id(pool).await?;

    match action {
        "record" => {
            let title = args
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Untitled");
            let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");
            let alternatives = args
                .get("alternatives")
                .cloned()
                .unwrap_or(Value::Array(vec![]));
            let chosen = args.get("chosen").and_then(|v| v.as_str()).unwrap_or("");
            let rationale = args.get("rationale").and_then(|v| v.as_str()).unwrap_or("");

            let decision_content = format!(
                "**{}**\nContext: {}\nAlternatives: {}\nChosen: {}\nRationale: {}",
                title, context, alternatives, chosen, rationale
            );

            crate::redact::refuse_secrets(&args, "the decision content", &decision_content)?;

            sqlx::query(
                "INSERT INTO brain_entities (name, entity_type, project_id)
                 VALUES ($1, 'concept', $2)
                 ON CONFLICT ON CONSTRAINT uq_brain_entities_name_project DO NOTHING",
            )
            .bind(entity_name)
            .bind(project_id)
            .execute(pool)
            .await?;
            let entity_id: (uuid::Uuid,) = sqlx::query_as(
                "SELECT id FROM brain_entities
                 WHERE name = $1 AND project_id IS NOT DISTINCT FROM $2",
            )
            .bind(entity_name)
            .bind(project_id)
            .fetch_one(pool)
            .await?;

            let row: (uuid::Uuid,) = sqlx::query_as(
                "INSERT INTO brain_observations (entity_id, content, observation_type, source, project_id)
                 VALUES ($1, $2, 'decision', 'agent', $3) RETURNING id",
            )
            .bind(entity_id.0)
            .bind(&decision_content)
            .bind(project_id)
            .fetch_one(pool)
            .await?;

            Ok(serde_json::json!({"action": "record", "id": row.0.to_string(), "title": title}))
        }
        "query" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let decisions: Vec<(uuid::Uuid, String, f64)> = sqlx::query_as(
                "SELECT id, content,
                        (ts_rank(search_vector, cuba_or_tsquery($1))
                         + similarity(content, $1))::float8 AS sim
                 FROM brain_observations
                 WHERE observation_type = 'decision'
                   AND (search_vector @@ cuba_or_tsquery($1)
                        OR content ILIKE '%' || $1 || '%'
                        OR similarity(content, $1) > 0.1)
                   AND ($2::uuid IS NULL OR project_id = $2)
                 ORDER BY sim DESC LIMIT 10",
            )
            .bind(query)
            .bind(project_id)
            .fetch_all(pool)
            .await?;
            let results: Vec<Value> = decisions.iter().map(|(id, content, sim)| {
                serde_json::json!({"id": id.to_string(), "content": content, "similarity": sim})
            }).collect();
            Ok(serde_json::json!({"action": "query", "results": results, "count": results.len()}))
        }
        "list" => {
            let decisions: Vec<(uuid::Uuid, String)> = sqlx::query_as(
                "SELECT id, content FROM brain_observations
                 WHERE observation_type = 'decision'
                   AND ($1::uuid IS NULL OR project_id = $1)
                 ORDER BY created_at DESC LIMIT 20",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await?;
            let list: Vec<Value> = decisions
                .iter()
                .map(|(id, c)| serde_json::json!({"id": id.to_string(), "content": c}))
                .collect();
            Ok(serde_json::json!({"action": "list", "decisions": list, "count": list.len()}))
        }
        _ => anyhow::bail!("Invalid action: {action}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://decreto-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn a_decision_that_pastes_a_credential_is_refused_from_any_of_its_fields() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        for field in ["context", "chosen", "rationale"] {
            let args = serde_json::json!({
                "action": "record",
                "title": "rotar el token del despliegue",
                field: "quedó apuntado que el viejo era ghp_abcdefghijklmnop"
            });

            let Err(failure) = handle(&pool, args).await else {
                panic!(
                    "record answered Ok on a pool that cannot connect: nothing but the secret \
                     gate could have answered, and the gate must refuse"
                );
            };

            let chain = format!("{failure:#}");
            assert!(
                chain.contains("github token"),
                "the token pasted into {field} reached the database layer: every field of a \
                 decision ends up concatenated into one stored observation, so checking only \
                 `chosen` would leave four doors open. Got: {chain}"
            );
            assert!(
                !chain.contains("ghp_abcdefghijklmnop"),
                "the refusal repeated the secret, and refusals get logged: {chain}"
            );
        }
    }
}
