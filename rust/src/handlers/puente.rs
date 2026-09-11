use crate::constants::VALID_RELATION_TYPES;
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::{PgPool, Row};

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");

    match action {
        "create" => create(pool, &args).await,
        "delete" => delete(pool, &args).await,
        "traverse" => traverse(pool, &args).await,
        "infer" => infer(pool, &args).await,
        "predict" => predict_links(pool, &args).await,
        _ => anyhow::bail!("Invalid action: {action}. Use create/delete/traverse/infer/predict"),
    }
}

async fn create(pool: &PgPool, args: &Value) -> Result<Value> {
    let from = args
        .get("from_entity")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let to = args.get("to_entity").and_then(|v| v.as_str()).unwrap_or("");
    let rel_type = args
        .get("relation_type")
        .and_then(|v| v.as_str())
        .unwrap_or("related_to");
    let bidirectional = args
        .get("bidirectional")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if from.is_empty() || to.is_empty() {
        anyhow::bail!("from_entity and to_entity are required");
    }
    if !VALID_RELATION_TYPES.contains(&rel_type) {
        anyhow::bail!("Invalid relation_type: {rel_type}");
    }

    let from_id = get_entity_id(pool, from).await?;
    let to_id = get_entity_id(pool, to).await?;

    let project_id = crate::project::current_project_id(pool).await?;

    let result = sqlx::query(
        "INSERT INTO brain_relations (from_entity, to_entity, relation_type, bidirectional, project_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (from_entity, to_entity, relation_type)
         DO UPDATE SET strength = LEAST(brain_relations.strength + 0.1, 1.0),
                       last_traversed = NOW(),
                       provenance = 'extracted'
         RETURNING (xmax = 0) AS is_insert",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(rel_type)
    .bind(bidirectional)
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    let is_new: bool = result.get::<bool, _>("is_insert");
    if !is_new {
        tracing::info!(
            from = from,
            to = to,
            rel = rel_type,
            "relation already exists — strengthened (Hebbian)"
        );
    }

    let _ = crate::graph_db::project_relation_full(from, to, rel_type, 1.0).await;

    if bidirectional {
        sqlx::query(
            "INSERT INTO brain_relations (from_entity, to_entity, relation_type, bidirectional, project_id)
             VALUES ($1, $2, $3, true, $4)
             ON CONFLICT (from_entity, to_entity, relation_type)
             DO UPDATE SET strength = LEAST(brain_relations.strength + 0.1, 1.0),
                           last_traversed = NOW(),
                           provenance = 'extracted'",
        )
        .bind(to_id)
        .bind(from_id)
        .bind(rel_type)
        .bind(project_id)
        .execute(pool)
        .await?;
        let _ = crate::graph_db::project_relation_full(to, from, rel_type, 1.0).await;
    }

    Ok(serde_json::json!({
        "action": "create",
        "from": from,
        "to": to,
        "relation_type": rel_type,
        "bidirectional": bidirectional
    }))
}

async fn delete(pool: &PgPool, args: &Value) -> Result<Value> {
    let from = args
        .get("from_entity")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let to = args.get("to_entity").and_then(|v| v.as_str()).unwrap_or("");
    let rel_type = args
        .get("relation_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let from_id = get_entity_id(pool, from).await?;
    let to_id = get_entity_id(pool, to).await?;

    let result = sqlx::query(
        "DELETE FROM brain_relations
         WHERE from_entity = $1 AND to_entity = $2 AND relation_type = $3",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(rel_type)
    .execute(pool)
    .await?;

    if result.rows_affected() > 0 {
        let _ = crate::graph_db::unproject_relation(from, to, rel_type).await;
    }

    Ok(serde_json::json!({
        "action": "delete",
        "deleted": result.rows_affected() > 0
    }))
}

async fn traverse(pool: &PgPool, args: &Value) -> Result<Value> {
    let start = args
        .get("start_entity")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let max_depth = args
        .get("max_depth")
        .and_then(|v| v.as_i64())
        .unwrap_or(3)
        .min(5);

    if start.is_empty() {
        anyhow::bail!("start_entity is required");
    }

    if let Ok((hops, pruned)) = crate::graph_db::traverse_falkor_with_paths(start, max_depth as i32)
        && !hops.is_empty()
    {
        let nodes: Vec<Value> = hops
            .iter()
            .map(|h| {
                serde_json::json!({
                    "name": h.name,
                    "relation": h.relation,
                    "strength": h.strength,
                    "depth": h.depth,
                    "provenance": "falkor"
                })
            })
            .collect();
        let paths: Vec<Value> = pruned
            .iter()
            .map(crate::graph_db::compact_rel_path)
            .collect();
        let hop_ball_tokens =
            crate::search::budget::count_tokens(&serde_json::json!(nodes).to_string());
        let path_tokens =
            crate::search::budget::count_tokens(&serde_json::json!(paths).to_string());
        return Ok(serde_json::json!({
            "action": "traverse",
            "start": start,
            "max_depth": max_depth,
            "nodes": nodes,
            "paths": paths,
            "count": nodes.len(),
            "backend": "falkor",
            "graph_hop_ball_tokens": hop_ball_tokens,
            "graph_context_tokens": path_tokens,
            "graph_token_ratio": if hop_ball_tokens > 0 {
                path_tokens as f64 / hop_ball_tokens as f64
            } else {
                0.0
            }
        }));
    }

    let start_id = get_entity_id(pool, start).await?;

    let project_id = crate::project::current_project_id(pool).await?;

    let paths: Vec<(String, String, f64, i32, String)> = sqlx::query_as(
        r#"
        WITH RECURSIVE graph_walk AS (
            SELECT
                r.to_entity AS current_node,
                r.relation_type,
                e2.name AS node_name,
                r.strength,
                1 AS depth,
                r.provenance
            FROM brain_relations r
            JOIN brain_entities e2 ON r.to_entity = e2.id
            WHERE r.from_entity = $1
              AND ($3::uuid IS NULL OR r.project_id = $3 OR r.project_id IS NULL)

            UNION ALL

            SELECT
                r.to_entity,
                r.relation_type,
                e2.name,
                r.strength,
                gw.depth + 1,
                r.provenance
            FROM brain_relations r
            JOIN brain_entities e2 ON r.to_entity = e2.id
            JOIN graph_walk gw ON r.from_entity = gw.current_node
            WHERE gw.depth < $2
              AND ($3::uuid IS NULL OR r.project_id = $3 OR r.project_id IS NULL)
        )
        SELECT node_name, relation_type, strength, depth, provenance
        FROM graph_walk
        ORDER BY depth, strength DESC
        LIMIT 50
        "#,
    )
    .bind(start_id)
    .bind(max_depth)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    sqlx::query(
        "UPDATE brain_relations SET
            strength = LEAST(strength + 0.02, 1.0),
            last_traversed = NOW()
         WHERE from_entity = $1
           AND ($2::uuid IS NULL OR project_id = $2 OR project_id IS NULL)",
    )
    .bind(start_id)
    .bind(project_id)
    .execute(pool)
    .await?;

    let nodes: Vec<Value> = paths
        .iter()
        .map(|(name, rel_type, strength, depth, provenance)| {
            serde_json::json!({
                "name": name,
                "relation": rel_type,
                "strength": strength,
                "depth": depth,
                "provenance": provenance
            })
        })
        .collect();

    Ok(serde_json::json!({
        "action": "traverse",
        "start": start,
        "max_depth": max_depth,
        "nodes": nodes,
        "count": nodes.len(),
        "backend": "postgres"
    }))
}

async fn infer(pool: &PgPool, args: &Value) -> Result<Value> {
    let start = args
        .get("start_entity")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let max_depth = args
        .get("max_depth")
        .and_then(|v| v.as_i64())
        .unwrap_or(3)
        .min(5);

    if start.is_empty() {
        anyhow::bail!("start_entity is required");
    }

    let start_id = get_entity_id(pool, start).await?;

    let project_id = crate::project::current_project_id(pool).await?;

    let inferences: Vec<(String, i32, f64)> = sqlx::query_as(
        r#"
        WITH RECURSIVE transitive_closure AS (
            SELECT
                r.to_entity AS current_node,
                1 AS depth,
                r.strength AS path_strength
            FROM brain_relations r
            WHERE r.from_entity = $1
              AND ($3::uuid IS NULL OR r.project_id = $3 OR r.project_id IS NULL)

            UNION ALL

            SELECT
                r.to_entity,
                tc.depth + 1,
                tc.path_strength * r.strength
            FROM brain_relations r
            JOIN transitive_closure tc ON r.from_entity = tc.current_node
            WHERE tc.depth < $2
              AND ($3::uuid IS NULL OR r.project_id = $3 OR r.project_id IS NULL)
        )
        SELECT e.name, tc.depth, tc.path_strength
        FROM transitive_closure tc
        JOIN brain_entities e ON tc.current_node = e.id
        WHERE tc.depth > 1
          AND ($3::uuid IS NULL OR e.project_id = $3 OR e.project_id IS NULL)
        ORDER BY tc.path_strength DESC
        LIMIT 20
        "#,
    )
    .bind(start_id)
    .bind(max_depth)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let inferred: Vec<Value> = inferences
        .iter()
        .map(|(name, depth, strength)| {
            serde_json::json!({
                "entity": name,
                "hops": depth,
                "inferred_strength": strength
            })
        })
        .collect();

    Ok(serde_json::json!({
        "action": "infer",
        "start": start,
        "inferred_connections": inferred,
        "count": inferred.len()
    }))
}

async fn predict_links(pool: &PgPool, args: &Value) -> Result<Value> {
    let entity_name = args
        .get("entity_name")
        .or_else(|| args.get("start_entity"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required for predict");
    }

    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10)
        .min(50);
    let persist = args
        .get("persist")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let persist_relation_type = args
        .get("relation_type")
        .and_then(|v| v.as_str())
        .filter(|t| VALID_RELATION_TYPES.contains(t))
        .unwrap_or("related_to");

    let project_id = crate::project::current_project_id(pool).await?;

    let entity_id: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM brain_entities
         WHERE name = $1
           AND ($2::uuid IS NULL OR project_id = $2 OR project_id IS NULL)
         LIMIT 1",
    )
    .bind(entity_name)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;

    let entity_id = match entity_id {
        Some((id,)) => id,
        None => {
            return Ok(serde_json::json!({
                "action": "predict",
                "entity": entity_name,
                "predictions": [],
                "note": "Entity not found (within current project scope)"
            }));
        }
    };

    let predictions: Vec<(uuid::Uuid, String, String, f64)> = sqlx::query_as(
        r#"
        WITH entity_neighbors AS (
            SELECT CASE WHEN from_entity = $1 THEN to_entity ELSE from_entity END AS neighbor
            FROM brain_relations
            WHERE (from_entity = $1 OR to_entity = $1)
              AND ($3::uuid IS NULL OR project_id = $3 OR project_id IS NULL)
        ),
        candidate_links AS (
            SELECT DISTINCT
                CASE WHEN r.from_entity = en.neighbor THEN r.to_entity ELSE r.from_entity END AS candidate,
                en.neighbor AS shared_neighbor
            FROM entity_neighbors en
            JOIN brain_relations r ON r.from_entity = en.neighbor OR r.to_entity = en.neighbor
            WHERE CASE WHEN r.from_entity = en.neighbor THEN r.to_entity ELSE r.from_entity END != $1
              AND CASE WHEN r.from_entity = en.neighbor THEN r.to_entity ELSE r.from_entity END
                  NOT IN (SELECT neighbor FROM entity_neighbors)
              AND ($3::uuid IS NULL OR r.project_id = $3 OR r.project_id IS NULL)
        ),
        neighbor_degrees AS (
            SELECT node, SUM(cnt) AS degree FROM (
                SELECT from_entity AS node, COUNT(*) AS cnt FROM brain_relations
                  WHERE ($3::uuid IS NULL OR project_id = $3 OR project_id IS NULL)
                  GROUP BY from_entity
                UNION ALL
                SELECT to_entity, COUNT(*) FROM brain_relations
                  WHERE ($3::uuid IS NULL OR project_id = $3 OR project_id IS NULL)
                  GROUP BY to_entity
            ) sub GROUP BY node
        ),
        aa_scores AS (
            SELECT cl.candidate, SUM(1.0 / LN(2.0 + COALESCE(nd.degree, 1))) AS aa_score
            FROM candidate_links cl
            LEFT JOIN neighbor_degrees nd ON cl.shared_neighbor = nd.node
            GROUP BY cl.candidate
        )
        SELECT aa.candidate, e.name, e.entity_type, aa.aa_score::float8
        FROM aa_scores aa
        JOIN brain_entities e ON aa.candidate = e.id
        WHERE ($3::uuid IS NULL OR e.project_id = $3 OR e.project_id IS NULL)
        ORDER BY aa.aa_score DESC
        LIMIT $2
        "#,
    )
    .bind(entity_id)
    .bind(limit)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let mut persisted = 0u32;
    if persist {
        for (candidate_id, _, _, score) in &predictions {
            let strength = normalize_aa_score(*score);
            let result = sqlx::query(
                "INSERT INTO brain_relations
                    (from_entity, to_entity, relation_type, strength, project_id, provenance)
                 VALUES ($1, $2, $3, $4, $5, 'predicted')
                 ON CONFLICT (from_entity, to_entity, relation_type) DO NOTHING",
            )
            .bind(entity_id)
            .bind(candidate_id)
            .bind(persist_relation_type)
            .bind(strength)
            .bind(project_id)
            .execute(pool)
            .await?;
            persisted += result.rows_affected() as u32;
        }
    }

    let prediction_json: Vec<Value> = predictions
        .iter()
        .map(|(_, name, etype, score)| {
            serde_json::json!({
                "entity": name,
                "entity_type": etype,
                "adamic_adar_score": score,
                "recommendation": if persist {
                    format!("Persisted as a 'predicted' relation ({persist_relation_type})")
                } else {
                    "Consider creating a relation between these entities".to_string()
                }
            })
        })
        .collect();

    let mut merged = prediction_json;
    if let Ok(activated) =
        crate::graph::activation::activated_neighbor_names(pool, entity_name, limit as usize).await
    {
        let existing: std::collections::HashSet<String> = merged
            .iter()
            .filter_map(|v| v.get("entity").and_then(|e| e.as_str()).map(str::to_string))
            .collect();
        for name in activated {
            if existing.contains(&name) {
                continue;
            }
            merged.push(serde_json::json!({
                "entity": name,
                "entity_type": "concept",
                "adamic_adar_score": 0.0,
                "activation_score": 0.5,
                "recommendation": "Spreading-activation neighbor (graph proximity)"
            }));
        }
    }

    let count = merged.len();
    Ok(serde_json::json!({
        "action": "predict",
        "entity": entity_name,
        "predictions": merged,
        "count": count,
        "algorithm": "Adamic-Adar + spreading activation",
        "persisted": persist,
        "relations_created": persisted
    }))
}

async fn get_entity_id(pool: &PgPool, name: &str) -> Result<uuid::Uuid> {
    let row: Option<(uuid::Uuid,)> =
        sqlx::query_as("SELECT id FROM brain_entities WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?;

    row.map(|(id,)| id)
        .context(format!("Entity '{name}' not found"))
}

fn normalize_aa_score(score: f64) -> f64 {
    (score / (score + 1.0)).clamp(0.01, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_stays_within_the_strength_column_bounds() {
        assert!(normalize_aa_score(0.0) >= 0.01);
        assert!(normalize_aa_score(1000.0) <= 1.0);
        assert!(
            normalize_aa_score(-5.0) >= 0.01,
            "a pathological negative input must not violate the CHECK constraint"
        );
    }

    #[test]
    fn normalize_is_monotonic() {
        assert!(normalize_aa_score(0.5) < normalize_aa_score(1.0));
        assert!(normalize_aa_score(1.0) < normalize_aa_score(5.0));
    }

    #[tokio::test]
    #[ignore]
    async fn create_resets_provenance_to_extracted_on_conflict() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");
        let pool = crate::db::create_pool(&url)
            .await
            .expect("connect to test database");

        let from = format!("puente_test_from_{}", uuid::Uuid::new_v4());
        let to = format!("puente_test_to_{}", uuid::Uuid::new_v4());
        for name in [&from, &to] {
            sqlx::query("INSERT INTO brain_entities (name) VALUES ($1)")
                .bind(name)
                .execute(&pool)
                .await
                .expect("create test entity");
        }

        let from_id = get_entity_id(&pool, &from).await.expect("from_id");
        let to_id = get_entity_id(&pool, &to).await.expect("to_id");
        sqlx::query(
            "INSERT INTO brain_relations (from_entity, to_entity, relation_type, provenance)
             VALUES ($1, $2, 'related_to', 'predicted')",
        )
        .bind(from_id)
        .bind(to_id)
        .execute(&pool)
        .await
        .expect("seed predicted relation");

        create(
            &pool,
            &serde_json::json!({
                "from_entity": from,
                "to_entity": to,
                "relation_type": "related_to"
            }),
        )
        .await
        .expect("create should hit the ON CONFLICT branch and succeed");

        let provenance: (String,) = sqlx::query_as(
            "SELECT provenance FROM brain_relations
             WHERE from_entity = $1 AND to_entity = $2 AND relation_type = 'related_to'",
        )
        .bind(from_id)
        .bind(to_id)
        .fetch_one(&pool)
        .await
        .expect("fetch relation after create");

        assert_eq!(
            provenance.0, "extracted",
            "an explicit create() must overwrite a stale 'predicted' provenance, not leave it mislabeled"
        );

        sqlx::query("DELETE FROM brain_relations WHERE from_entity = $1 OR to_entity = $1 OR from_entity = $2 OR to_entity = $2")
            .bind(from_id)
            .bind(to_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM brain_entities WHERE id = $1 OR id = $2")
            .bind(from_id)
            .bind(to_id)
            .execute(&pool)
            .await
            .ok();
    }
}
