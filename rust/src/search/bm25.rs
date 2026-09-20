use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn bm25_search(
    pool: &PgPool,
    query: &str,
    scope: &str,
    limit: i64,
    project_id: Option<Uuid>,
    include_unscoped: bool,
) -> Result<Vec<Value>> {
    let mut results = Vec::new();

    if scope == "all" || scope == "observations" {
        let rows: Vec<(Uuid, String, String, String, f64, f64)> = sqlx::query_as(
            "SELECT o.id, e.name, o.content, o.observation_type, o.importance::float8,
                    ts_rank_cd(o.search_vector, cuba_or_tsquery($1))::float8 AS bm25
             FROM brain_observations o
             JOIN brain_entities e ON o.entity_id = e.id
             WHERE o.search_vector @@ cuba_or_tsquery($1)
               AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
               AND ($3::uuid IS NULL OR o.project_id = $3 OR ($4::bool AND o.project_id IS NULL))
             ORDER BY bm25 DESC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .bind(project_id)
        .bind(include_unscoped)
        .fetch_all(pool)
        .await
        .context(
            "BM25 lexical search over brain_observations failed — the hybrid answer would \
             have silently degraded to vector-only. Usual causes: a malformed query reaching \
             cuba_or_tsquery, the pool exhausted, or the GIN index on search_vector missing",
        )?;
        results.extend(rows.into_iter().map(
            |(id, entity_name, content, obs_type, importance, bm25)| {
                serde_json::json!({
                    "id": id.to_string(),
                    "type": "observation",
                    "entity_name": entity_name,
                    "content": content,
                    "observation_type": obs_type,
                    "importance": importance,
                    "bm25_score": bm25
                })
            },
        ));
    }

    if scope == "all" || scope == "entities" {
        let rows: Vec<(Uuid, String, String, f64, f64)> = sqlx::query_as(
            "SELECT id, name, entity_type, importance::float8,
                    ts_rank_cd(search_vector, cuba_or_tsquery($1))::float8 AS bm25
             FROM brain_entities
             WHERE search_vector @@ cuba_or_tsquery($1)
               AND ($3::uuid IS NULL OR project_id = $3 OR ($4::bool AND project_id IS NULL))
             ORDER BY bm25 DESC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .bind(project_id)
        .bind(include_unscoped)
        .fetch_all(pool)
        .await
        .context(
            "BM25 lexical search over brain_entities failed — the hybrid answer would \
             have silently degraded to vector-only. Usual causes: a malformed query reaching \
             cuba_or_tsquery, the pool exhausted, or the GIN index on search_vector missing",
        )?;
        results.extend(
            rows.into_iter()
                .map(|(id, name, entity_type, importance, bm25)| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "type": "entity",
                        "name": name,
                        "entity_type": entity_type,
                        "importance": importance,
                        "bm25_score": bm25
                    })
                }),
        );
    }

    if scope == "all" || scope == "errors" {
        let rows: Vec<(Uuid, String, String, bool, f64)> = sqlx::query_as(
            "SELECT id, error_type, error_message, resolved,
                    ts_rank_cd(search_vector, cuba_or_tsquery($1))::float8 AS bm25
             FROM brain_errors
             WHERE search_vector @@ cuba_or_tsquery($1)
               AND ($3::uuid IS NULL OR project_id = $3 OR ($4::bool AND project_id IS NULL))
             ORDER BY bm25 DESC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .bind(project_id)
        .bind(include_unscoped)
        .fetch_all(pool)
        .await
        .context(
            "BM25 lexical search over brain_errors failed — the hybrid answer would \
             have silently degraded to vector-only. Usual causes: a malformed query reaching \
             cuba_or_tsquery, the pool exhausted, or the GIN index on search_vector missing",
        )?;
        results.extend(
            rows.into_iter()
                .map(|(id, error_type, error_message, resolved, bm25)| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "type": "error",
                        "error_type": error_type,
                        "error_message": error_message,
                        "resolved": resolved,
                        "bm25_score": bm25
                    })
                }),
        );
    }

    results.sort_by(|a, b| {
        let sa = a.get("bm25_score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let sb = b.get("bm25_score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(limit as usize);

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://bm25-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn a_failing_query_is_reported_instead_of_passing_for_zero_lexical_hits() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        for (scope, table) in [
            ("observations", "brain_observations"),
            ("entities", "brain_entities"),
            ("errors", "brain_errors"),
            ("all", "brain_observations"),
        ] {
            let Err(failure) = bm25_search(&pool, "anything at all", scope, 10, None, false).await
            else {
                panic!(
                    "scope {scope:?} answered Ok on a pool that cannot connect: the lexical \
                     half of the hybrid contributed nothing and the caller has no way to tell \
                     that apart from a query with no lexical matches"
                );
            };

            let chain = format!("{failure:#}");
            assert!(
                chain.contains(table),
                "scope {scope:?} must name the table it failed on so the operator knows which \
                 index or query to look at; got: {chain}"
            );
        }
    }
}
