use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

async fn refresh_ood_cache(pool: &PgPool, project_id: Option<uuid::Uuid>) -> Result<()> {
    use crate::search::ood::{MIN_SAMPLES_FOR_OOD, OodStats};
    let raw: Vec<(pgvector::Vector,)> = sqlx::query_as(
        "SELECT embedding FROM brain_observations
         WHERE embedding IS NOT NULL AND observation_type != 'superseded'
           AND ($1::uuid IS NULL OR project_id = $1)
         ORDER BY id LIMIT $2",
    )
    .bind(project_id)
    .bind(crate::resources::ood_fit_limit())
    .fetch_all(pool)
    .await?;
    if raw.len() < MIN_SAMPLES_FOR_OOD {
        return Ok(());
    }
    let embeddings: Vec<Vec<f32>> = raw.into_iter().map(|(v,)| v.to_vec()).collect();
    if let Some(stats) = OodStats::fit(&embeddings) {
        crate::search::ood_cache::store(project_id, std::sync::Arc::new(stats));
    }
    Ok(())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StaleObservation {
    pub id: uuid::Uuid,
    pub content: String,
    pub entity_type: String,
    pub entity_name: String,
}

pub async fn reembed_batch(
    pool: &PgPool,
    rows: &[StaleObservation],
    model: &str,
) -> (usize, usize) {
    let mut updated = 0usize;
    let mut failed = 0usize;
    for row in rows {
        let embedding = crate::embeddings::onnx::embed_passage_contextual(
            &row.content,
            &row.entity_type,
            &row.entity_name,
        )
        .await;
        let embedding = match embedding {
            Ok(embedding) => embedding,
            Err(e) => {
                tracing::warn!(obs_id = %row.id, error = %e, "reembed: ONNX failed for observation");
                failed += 1;
                continue;
            }
        };
        let written = sqlx::query(
            "UPDATE brain_observations SET embedding = $1::vector, embedding_model = $2 WHERE id = $3",
        )
        .bind(pgvector::Vector::from(embedding))
        .bind(model)
        .bind(row.id)
        .execute(pool)
        .await;
        match written {
            Ok(_) => updated += 1,
            Err(e) => {
                tracing::warn!(obs_id = %row.id, error = %e, "reembed: could not persist embedding");
                failed += 1;
            }
        }
    }
    (updated, failed)
}

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let project_id = crate::project::current_project_id(pool).await?;

    match action {
        "decay" => {
            let global_override = args.get("halflife_days").and_then(|v| v.as_f64());
            let result = if let Some(halflife) = global_override {
                sqlx::query(
                    "UPDATE brain_observations SET
                        importance = GREATEST(
                            importance * EXP(-0.693
                                * EXTRACT(EPOCH FROM (NOW() - GREATEST(last_accessed, last_decayed_at))) / 86400.0
                                / ($1 * (1.0 + LN(1.0 + access_count::float8)))),
                            0.01
                        ),
                        last_decayed_at = NOW(),
                        updated_at = NOW()
                     WHERE observation_type NOT IN ('decision', 'lesson', 'superseded')
                       AND last_accessed < NOW() - INTERVAL '1 day'
                       AND ($2::uuid IS NULL OR project_id = $2)"
                )
                .bind(halflife)
                .bind(project_id)
                .execute(pool)
                .await?
            } else {
                sqlx::query(
                    "UPDATE brain_observations SET
                        importance = GREATEST(
                            importance * EXP(-0.693
                                * EXTRACT(EPOCH FROM (NOW() - GREATEST(last_accessed, last_decayed_at))) / 86400.0
                                / ((CASE observation_type
                                    WHEN 'fact'       THEN 30.0
                                    WHEN 'preference' THEN 30.0
                                    WHEN 'error'      THEN 14.0
                                    WHEN 'solution'   THEN 14.0
                                    WHEN 'context'    THEN  7.0
                                    WHEN 'tool_usage' THEN  7.0
                                    ELSE 30.0
                                END) * (1.0 + LN(1.0 + access_count::float8)))
                            ),
                            0.01
                        ),
                        last_decayed_at = NOW(),
                        updated_at = NOW()
                     WHERE observation_type NOT IN ('decision', 'lesson', 'superseded')
                       AND last_accessed < NOW() - INTERVAL '1 day'
                       AND ($1::uuid IS NULL OR project_id = $1)"
                )
                .bind(project_id)
                .execute(pool)
                .await?
            };
            Ok(serde_json::json!({
                "action": "decay",
                "decayed": result.rows_affected(),
                "project_scoped": project_id.is_some(),
                "stratification": {
                    "fact/preference": "30d base halflife",
                    "error/solution": "14d base halflife",
                    "context/tool_usage": "7d base halflife",
                    "decision/lesson": "never decay"
                },
                "formula": "importance * EXP(-0.693 * days / (base_halflife * (1 + ln(1 + access_count))))",
                "testing_effect": "Karpicke-Roediger 2008 — high-access obs decay slower"
            }))
        }
        "prune" => {
            let threshold = args
                .get("threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.1);
            let confirm = args
                .get("confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let doomed: Vec<(Option<String>, i64)> = sqlx::query_as(
                "SELECT p.name, COUNT(*)
                 FROM brain_observations o
                 LEFT JOIN brain_projects p ON p.id = o.project_id
                 WHERE o.importance < $1
                   AND o.observation_type NOT IN ('decision', 'lesson')
                   AND ($2::uuid IS NULL OR o.project_id = $2)
                 GROUP BY p.name
                 ORDER BY COUNT(*) DESC",
            )
            .bind(threshold)
            .bind(project_id)
            .fetch_all(pool)
            .await
            .context("failed to count the observations prune would delete")?;

            let would_prune: i64 = doomed.iter().map(|(_, n)| n).sum();
            let by_project: Vec<Value> = doomed
                .iter()
                .map(|(name, n)| {
                    serde_json::json!({
                        "project": name.clone().unwrap_or_else(|| "(global)".to_string()),
                        "observations": n
                    })
                })
                .collect();

            if !confirm {
                return Ok(serde_json::json!({
                    "action": "prune",
                    "dry_run": true,
                    "would_prune": would_prune,
                    "threshold": threshold,
                    "by_project": by_project,
                    "project_scoped": project_id.is_some(),
                    "hint": "Nothing was deleted. Deletion is irreversible and there is no undo: \
                             re-run with confirm=true only after reading would_prune and by_project."
                }));
            }

            let result = sqlx::query(
                "DELETE FROM brain_observations
                 WHERE importance < $1
                   AND observation_type NOT IN ('decision', 'lesson')
                   AND ($2::uuid IS NULL OR project_id = $2)",
            )
            .bind(threshold)
            .bind(project_id)
            .execute(pool)
            .await?;

            tracing::warn!(
                pruned = result.rows_affected(),
                threshold,
                project_scoped = project_id.is_some(),
                "prune deleted observations"
            );

            Ok(serde_json::json!({
                "action": "prune",
                "dry_run": false,
                "pruned": result.rows_affected(),
                "threshold": threshold,
                "by_project": by_project,
                "project_scoped": project_id.is_some()
            }))
        }
        "merge" => {
            let sim_threshold = args
                .get("similarity_threshold")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.8);
            let dupes: Vec<(uuid::Uuid, uuid::Uuid, f64)> = sqlx::query_as(
                "SELECT a.id, b.id, similarity(a.content, b.content)::float8 AS sim
                 FROM brain_observations a JOIN brain_observations b ON a.entity_id = b.entity_id AND a.id < b.id
                 WHERE similarity(a.content, b.content) > $1 AND a.observation_type != 'superseded' AND b.observation_type != 'superseded'
                   AND ($2::uuid IS NULL OR a.project_id = $2)
                   AND ($2::uuid IS NULL OR b.project_id = $2)
                 LIMIT 100"
            ).bind(sim_threshold).bind(project_id).fetch_all(pool).await?;

            let mut tx = pool
                .begin()
                .await
                .context("failed to begin merge transaction")?;
            let mut merged = 0u32;
            for (keep_id, remove_id, _) in &dupes {
                sqlx::query(
                    "UPDATE brain_observations SET observation_type = 'superseded' WHERE id = $1",
                )
                .bind(remove_id)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE brain_observations SET importance = LEAST(importance + 0.05, 1.0) WHERE id = $1").bind(keep_id).execute(&mut *tx).await?;
                merged += 1;
            }
            tx.commit()
                .await
                .context("failed to commit merge transaction")?;
            Ok(
                serde_json::json!({"action": "merge", "merged": merged, "threshold": sim_threshold, "project_scoped": project_id.is_some()}),
            )
        }
        "stats" => {
            refresh_ood_cache(pool, project_id).await.ok();
            let entities: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM brain_entities
                 WHERE ($1::uuid IS NULL OR project_id = $1)",
            )
            .bind(project_id)
            .fetch_one(pool)
            .await?;
            let observations: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM brain_observations
                 WHERE ($1::uuid IS NULL OR project_id = $1)",
            )
            .bind(project_id)
            .fetch_one(pool)
            .await?;
            let superseded: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM brain_observations
                 WHERE observation_type = 'superseded'
                   AND ($1::uuid IS NULL OR project_id = $1)",
            )
            .bind(project_id)
            .fetch_one(pool)
            .await?;
            Ok(
                serde_json::json!({"action": "stats", "entities": entities.0, "observations": observations.0, "superseded": superseded.0, "active": observations.0 - superseded.0, "project_scoped": project_id.is_some()}),
            )
        }
        "pagerank" => {
            let ranked = crate::graph::pagerank::compute_and_store(pool).await?;
            let energy = crate::graph::energy::refresh_energy_scores(pool)
                .await
                .unwrap_or(0);
            Ok(
                serde_json::json!({"action": "pagerank", "updated": ranked, "energy_refreshed": energy}),
            )
        }
        "communities" => {
            let (communities, nodes) = crate::graph::community::detect_and_persist(pool).await?;
            Ok(serde_json::json!({
                "action": "communities",
                "communities": communities.len(),
                "nodes_tagged": nodes,
                "algorithm": "leiden_v1"
            }))
        }
        "summarize" => {
            let entity_name = args
                .get("entity_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let summary = args
                .get("compressed_summary")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if entity_name.is_empty() || summary.is_empty() {
                anyhow::bail!("entity_name and compressed_summary are required");
            }
            crate::redact::refuse_secrets(&args, "compressed_summary", summary)?;
            let entity_id: (uuid::Uuid,) = sqlx::query_as(
                "SELECT id FROM brain_entities
                 WHERE name = $1 AND ($2::uuid IS NULL OR project_id = $2)",
            )
            .bind(entity_name)
            .bind(project_id)
            .fetch_one(pool)
            .await?;
            let marked = sqlx::query(
                "UPDATE brain_observations SET observation_type = 'superseded'
                 WHERE entity_id = $1 AND observation_type != 'superseded'
                   AND ($2::uuid IS NULL OR project_id = $2)",
            )
            .bind(entity_id.0)
            .bind(project_id)
            .execute(pool)
            .await?;
            sqlx::query("INSERT INTO brain_observations (entity_id, content, observation_type, source, project_id) VALUES ($1, $2, 'fact', 'consolidation', $3)")
                .bind(entity_id.0).bind(summary).bind(project_id).execute(pool).await?;
            Ok(
                serde_json::json!({"action": "summarize", "entity": entity_name, "superseded": marked.rows_affected(), "project_scoped": project_id.is_some()}),
            )
        }
        "find_duplicates" => {
            let dupes: Vec<(String, String, f64)> = sqlx::query_as(
                "SELECT a.content, b.content, similarity(a.content, b.content)::float8 AS sim
                 FROM brain_observations a JOIN brain_observations b ON a.entity_id = b.entity_id AND a.id < b.id
                 WHERE similarity(a.content, b.content) > 0.7 AND a.observation_type != 'superseded' AND b.observation_type != 'superseded'
                   AND ($1::uuid IS NULL OR a.project_id = $1)
                   AND ($1::uuid IS NULL OR b.project_id = $1)
                 ORDER BY sim DESC LIMIT 20"
            ).bind(project_id).fetch_all(pool).await?;
            let results: Vec<Value> = dupes
                .iter()
                .map(|(a, b, s)| {
                    serde_json::json!({
                        "content_a": safe_truncate(a, 100),
                        "content_b": safe_truncate(b, 100),
                        "similarity": s
                    })
                })
                .collect();
            Ok(
                serde_json::json!({"action": "find_duplicates", "duplicates": results, "count": results.len(), "project_scoped": project_id.is_some()}),
            )
        }
        "export" => {
            let entities: Vec<(uuid::Uuid, String, String, f64)> = sqlx::query_as("SELECT id, name, entity_type, importance FROM brain_entities ORDER BY importance DESC LIMIT 500").fetch_all(pool).await?;
            let ent_json: Vec<Value> = entities.iter().map(|(id, n, t, i)| serde_json::json!({"id": id.to_string(), "name": n, "type": t, "importance": i})).collect();
            Ok(
                serde_json::json!({"action": "export", "entities": ent_json, "count": ent_json.len()}),
            )
        }
        "decay_episodes" => {
            let c = args.get("c").and_then(|v| v.as_f64()).unwrap_or(0.1);
            let beta = args.get("beta").and_then(|v| v.as_f64()).unwrap_or(0.5);
            let result = sqlx::query(
                "UPDATE brain_episodes SET
                    importance = GREATEST(
                        0.5 / POWER(
                            1.0 + $1 * EXTRACT(EPOCH FROM (NOW() - created_at)) / 86400.0,
                            $2
                        ),
                        0.01
                    )
                 WHERE created_at < NOW() - INTERVAL '1 hour'",
            )
            .bind(c)
            .bind(beta)
            .execute(pool)
            .await?;
            Ok(serde_json::json!({
                "action": "decay_episodes",
                "decayed": result.rows_affected(),
                "formula": "0.5 / (1 + c·t)^β  (Wixted 2004, idempotent from initial=0.5)",
                "c": c,
                "beta": beta
            }))
        }
        "reembed" => {
            if !crate::embeddings::onnx::is_model_loaded() {
                return Ok(serde_json::json!({
                    "action": "reembed",
                    "updated": 0,
                    "error": "ONNX model not loaded — set ONNX_MODEL_PATH to enable reembed"
                }));
            }

            let batch_size = args
                .get("batch_size")
                .and_then(|v| v.as_i64())
                .unwrap_or(500);
            let current_model = crate::embeddings::onnx::current_model();
            let obs: Vec<StaleObservation> = sqlx::query_as(
                "SELECT o.id, o.content, e.entity_type, e.name AS entity_name
                 FROM brain_observations o
                 JOIN brain_entities e ON e.id = o.entity_id
                 WHERE o.observation_type != 'superseded'
                   AND (o.embedding_model != $2 OR o.embedding_model IS NULL OR o.embedding IS NULL)
                 ORDER BY o.importance DESC
                 LIMIT $1",
            )
            .bind(batch_size)
            .bind(&current_model)
            .fetch_all(pool)
            .await?;

            let total = obs.len();
            let mut updated = 0usize;

            let progress_token = args
                .get("_meta")
                .and_then(|m| m.get("progressToken"))
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| format!("zafra_reembed_{}", batch_size));
            let progress_step = (total / 20).max(1);

            let mut done = 0usize;
            for slice in obs.chunks(progress_step) {
                let (ok, _failed) = reembed_batch(pool, slice, &current_model).await;
                updated += ok;
                done += slice.len();
                if total > 50 {
                    crate::protocol::notify_progress(
                        &progress_token,
                        done as f64,
                        Some(total as f64),
                        Some(&format!("re-embedded {}/{}", done, total)),
                    );
                }
            }

            Ok(serde_json::json!({
                "action": "reembed",
                "total_fetched": total,
                "updated": updated,
                "model": current_model,
                "dim": crate::embeddings::onnx::embedding_dim(),
                "note": "Run after switching embedding models to ensure vector search consistency"
            }))
        }
        _ => anyhow::bail!("Invalid action: {action}"),
    }
}

pub fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const FIXTURE_IMPORTANCE: f64 = 0.001;
    const FIXTURE_THRESHOLD: f64 = 0.002;

    fn unique_name(prefix: &str) -> String {
        format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
    }

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://zafra-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn the_summary_that_replaces_a_whole_entity_is_refused_when_it_carries_a_credential() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "action": "summarize",
            "entity_name": "deploy",
            "compressed_summary": "todo el despliegue se hacía con ghp_abcdefghijklmnop"
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "summarize answered Ok on a pool that cannot connect: nothing but the secret \
                 gate could have answered, and the gate must refuse"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("github token") && chain.contains("compressed_summary"),
            "summarize supersedes every observation of the entity and leaves this text as the \
             only survivor: a credential written here is both stored and impossible to find by \
             rereading what it replaced. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");
        crate::db::create_pool(&url)
            .await
            .expect("connect to test database")
    }

    async fn seed_observation(
        pool: &PgPool,
        entity_name: &str,
        entity_type: &str,
        content: &str,
        importance: f64,
        project_id: Option<Uuid>,
    ) -> Uuid {
        let entity_id: (Uuid,) = sqlx::query_as(
            "INSERT INTO brain_entities (name, entity_type, project_id)
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(entity_name)
        .bind(entity_type)
        .bind(project_id)
        .fetch_one(pool)
        .await
        .expect("creating the fixture entity");

        let obs_id: (Uuid,) = sqlx::query_as(
            "INSERT INTO brain_observations
                (entity_id, content, observation_type, source, importance, project_id)
             VALUES ($1, $2, 'fact', 'agent', $3, $4) RETURNING id",
        )
        .bind(entity_id.0)
        .bind(content)
        .bind(importance)
        .bind(project_id)
        .fetch_one(pool)
        .await
        .expect("creating the fixture observation");

        obs_id.0
    }

    async fn observation_exists(pool: &PgPool, obs_id: Uuid) -> bool {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM brain_observations WHERE id = $1 LIMIT 1")
                .bind(obs_id)
                .fetch_optional(pool)
                .await
                .expect("checking whether the observation survived");
        row.is_some()
    }

    async fn observation_exists_unscoped(pool: &PgPool, obs_id: Uuid) -> bool {
        let saved = crate::session::get();
        crate::session::clear();
        let found = observation_exists(pool, obs_id).await;
        if let Some(s) = saved {
            crate::session::set(s.session_id, s.project_id);
        }
        found
    }

    async fn drop_entity(pool: &PgPool, entity_name: &str) {
        sqlx::query("DELETE FROM brain_entities WHERE name = $1")
            .bind(entity_name)
            .execute(pool)
            .await
            .ok();
    }

    fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max)
    }

    #[tokio::test]
    #[ignore]
    async fn reembed_reproduces_the_vector_the_write_path_stores() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = test_pool().await;
        let entity_name = unique_name("reembed_entity");
        let entity_type = "concept";
        let content = "El drenaje perdió el embedding de esta observación y hay que repararlo.";

        let obs_id = seed_observation(&pool, &entity_name, entity_type, content, 0.5, None).await;

        let rows: Vec<StaleObservation> = sqlx::query_as(
            "SELECT o.id, o.content, e.entity_type, e.name AS entity_name
             FROM brain_observations o
             JOIN brain_entities e ON e.id = o.entity_id
             WHERE o.id = $1",
        )
        .bind(obs_id)
        .fetch_all(&pool)
        .await
        .expect("reading back the observation the way reembed does");

        let (updated, failed) = reembed_batch(&pool, &rows, "test-reembed-model").await;
        assert_eq!(
            (updated, failed),
            (1, 0),
            "the fixture must be re-embedded exactly once for the comparison to mean anything"
        );

        let stored: (pgvector::Vector,) =
            sqlx::query_as("SELECT embedding FROM brain_observations WHERE id = $1")
                .bind(obs_id)
                .fetch_one(&pool)
                .await
                .expect("reading the stored embedding");
        let stored = stored.0.to_vec();

        let write_path =
            crate::embeddings::onnx::embed_passage_contextual(content, entity_type, &entity_name)
                .await
                .expect("embedding the way cronica does when the observation is written");
        let bare = crate::embeddings::onnx::embed_passage(content)
            .await
            .expect("embedding the bare content");

        drop_entity(&pool, &entity_name).await;

        assert!(
            max_abs_diff(&bare, &write_path) > 1e-4,
            "the fixture is useless unless the two encodings differ: the entity prefix must \
             change the vector, otherwise this test cannot detect the corruption"
        );
        assert!(
            max_abs_diff(&stored, &write_path) < 1e-5,
            "reembed must store the vector cronica would have written — anything else leaves \
             the repaired rows in a different space than the rest of the corpus, which is \
             exactly the silent corruption main.rs recommends reembed as the cure for \
             (max abs diff {:.6})",
            max_abs_diff(&stored, &write_path)
        );
    }

    #[tokio::test]
    #[ignore]
    async fn prune_plans_before_it_deletes_and_stays_inside_the_active_project() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = test_pool().await;

        let mine = crate::project::upsert_project(&pool, &unique_name("zafra_proj_mine"))
            .await
            .expect("creating the active project");
        let other = crate::project::upsert_project(&pool, &unique_name("zafra_proj_other"))
            .await
            .expect("creating the bystander project");

        let mine_entity = unique_name("prune_mine");
        let other_entity = unique_name("prune_other");
        let mine_obs = seed_observation(
            &pool,
            &mine_entity,
            "concept",
            "observación de bajo valor del proyecto activo",
            FIXTURE_IMPORTANCE,
            Some(mine),
        )
        .await;
        let other_obs = seed_observation(
            &pool,
            &other_entity,
            "concept",
            "observación de bajo valor de otro proyecto",
            FIXTURE_IMPORTANCE,
            Some(other),
        )
        .await;

        crate::session::set(Uuid::new_v4(), Some(mine));

        let before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM brain_observations")
            .fetch_one(&pool)
            .await
            .expect("counting observations before the dry run");

        let stats = handle(&pool, serde_json::json!({"action": "stats"}))
            .await
            .expect("stats must not fail");
        assert_eq!(
            stats.get("project_scoped").and_then(|v| v.as_bool()),
            Some(true),
            "stats must say whether its numbers describe the whole database or just the \
             active project — reporting a global count as if it were the project's is how \
             an agent decides to prune"
        );

        let planned = handle(
            &pool,
            serde_json::json!({"action": "prune", "threshold": FIXTURE_THRESHOLD}),
        )
        .await
        .expect("prune must not fail");

        assert_eq!(
            planned.get("dry_run").and_then(|v| v.as_bool()),
            Some(true),
            "prune without confirm=true must plan, not delete: it is an MCP tool an agent can \
             call on its own, and against the live brain the default threshold reaches 47% of \
             the rows with no undo"
        );
        assert!(
            planned
                .get("would_prune")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                >= 1,
            "the plan must count the fixture it would delete, or there is nothing to read \
             before confirming: {planned}"
        );
        let plan_projects = planned
            .get("by_project")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        assert!(
            plan_projects.iter().any(|p| p
                .get("observations")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                >= 1),
            "the plan must break the count down by project so the caller can see whose \
             memory is at stake: {planned}"
        );

        assert!(
            observation_exists(&pool, mine_obs).await,
            "a dry run must leave the active project's observation alive"
        );
        assert!(
            observation_exists_unscoped(&pool, other_obs).await,
            "a dry run must leave every other project's observation alive. This has to look \
             with the session cleared: once the runtime downgrades to cuba_app, before_acquire \
             stamps the active project onto the connection and tenant_isolation genuinely hides \
             the other project's rows — a scoped read cannot tell `hidden` from `deleted`"
        );
        let after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM brain_observations")
            .fetch_one(&pool)
            .await
            .expect("counting observations after the dry run");
        assert_eq!(
            after.0, before.0,
            "a dry run must not change the row count at all"
        );

        let applied = handle(
            &pool,
            serde_json::json!({
                "action": "prune",
                "threshold": FIXTURE_THRESHOLD,
                "confirm": true
            }),
        )
        .await
        .expect("confirmed prune must not fail");
        assert_eq!(
            applied.get("dry_run").and_then(|v| v.as_bool()),
            Some(false),
            "a confirmed prune must report that it really deleted: {applied}"
        );

        let mine_survived = observation_exists(&pool, mine_obs).await;
        let other_survived = observation_exists_unscoped(&pool, other_obs).await;

        crate::session::clear();
        drop_entity(&pool, &mine_entity).await;
        drop_entity(&pool, &other_entity).await;
        sqlx::query("DELETE FROM brain_projects WHERE id = ANY($1)")
            .bind(vec![mine, other])
            .execute(&pool)
            .await
            .ok();

        assert!(
            !mine_survived,
            "a confirmed prune must delete the low-importance observation of the active project"
        );
        assert!(
            other_survived,
            "prune must not reach into another project: the session picks one project, and a \
             maintenance action that ignores it deletes memory nobody asked about. Read \
             unscoped, because tenant_isolation now hides it from the active session either way"
        );
    }
}
