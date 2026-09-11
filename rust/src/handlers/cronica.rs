use crate::cognitive::{density, prediction_error};
use crate::constants::{DEDUP_THRESHOLD, VALID_OBSERVATION_TYPES, VALID_SOURCES};
use crate::search::entity_factoid::embedding_status;
use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;
use std::time::{Duration, Instant};

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let entity_name = args
        .get("entity_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match action {
        "add" => add(pool, entity_name, &args).await,
        "delete" => delete_obs(pool, &args).await,
        "list" => list(pool, entity_name).await,
        "batch_add" => batch_add(pool, &args).await,
        "episode_add" => episode_add(pool, entity_name, &args).await,
        "episode_list" => episode_list(pool, entity_name).await,
        "timeline" => timeline(pool, entity_name).await,
        _ => anyhow::bail!(
            "Invalid action: {action}. Use add/delete/list/batch_add/episode_add/episode_list/timeline"
        ),
    }
}

const WRITE_THROUGH_TIMEOUT: Duration = Duration::from_secs(2);

async fn persist_searchable(
    pool: &PgPool,
    obs_id: uuid::Uuid,
    content: &str,
    entity_type: &str,
    entity_name: &str,
    project_id: Option<uuid::Uuid>,
    precomputed: Option<Vec<f32>>,
) -> (String, usize) {
    let loaded = crate::embeddings::onnx::is_model_loaded();
    if !loaded {
        return (embedding_status(false, false).to_string(), 0);
    }
    let work = async {
        let emb = match precomputed {
            Some(e) => e,
            None => {
                crate::embeddings::onnx::embed_passage_contextual(content, entity_type, entity_name)
                    .await?
            }
        };
        sqlx::query(
            "UPDATE brain_observations SET embedding = $1::vector, embedding_model = $2 WHERE id = $3",
        )
        .bind(pgvector::Vector::from(emb))
        .bind(crate::embeddings::onnx::current_model())
        .bind(obs_id)
        .execute(pool)
        .await?;
        let chunks = if crate::embeddings::chunk::needs_chunking(content) {
            crate::embeddings::backfill::store_chunks(
                pool,
                obs_id,
                content,
                entity_type,
                entity_name,
                project_id,
            )
            .await
            .unwrap_or(0)
        } else {
            0
        };
        anyhow::Ok(chunks)
    };
    match tokio::time::timeout(WRITE_THROUGH_TIMEOUT, work).await {
        Ok(Ok(chunks)) => (embedding_status(true, true).to_string(), chunks),
        Ok(Err(e)) => {
            tracing::warn!(obs_id = %obs_id, error = %e, "write-through embed failed — FTS still works");
            (embedding_status(true, false).to_string(), 0)
        }
        Err(_) => {
            tracing::warn!(obs_id = %obs_id, "write-through embed timed out — marked pending");
            (embedding_status(true, false).to_string(), 0)
        }
    }
}

async fn add(pool: &PgPool, entity_name: &str, args: &Value) -> Result<Value> {
    let started = Instant::now();
    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required");
    }

    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.is_empty() || content.len() > 10_000 {
        anyhow::bail!("content must be 1-10000 characters");
    }

    crate::redact::refuse_secrets(args, "content", content)?;

    let obs_type = args
        .get("observation_type")
        .and_then(|v| v.as_str())
        .unwrap_or("fact");
    if !VALID_OBSERVATION_TYPES.contains(&obs_type) {
        anyhow::bail!("Invalid observation_type: {obs_type}");
    }

    let source = args
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("agent");
    if !VALID_SOURCES.contains(&source) {
        anyhow::bail!("Invalid source: {source}");
    }

    let project_id = crate::project::current_project_id(pool).await?;

    let (entity_id, dedup_entity_type) = ensure_entity_typed(pool, entity_name, project_id).await?;

    let density = information_density(content);
    let dedup = check_dedup(pool, entity_id, content, &dedup_entity_type, entity_name).await?;

    let precomputed_embedding = match dedup {
        DedupResult::Duplicate(existing_preview) => {
            return Ok(serde_json::json!({
                "action": "add",
                "deduplicated": true,
                "existing_content": existing_preview,
                "message": "Near-duplicate detected. Observation NOT added.",
                "write_to_searchable_ms": started.elapsed().as_millis() as u64
            }));
        }
        DedupResult::Reinforce(obs_id) => {
            sqlx::query(
                "UPDATE brain_observations SET
                    importance = LEAST(importance + 0.05, 1.0),
                    access_count = access_count + 1,
                    last_accessed = NOW()
                 WHERE id = $1",
            )
            .bind(obs_id)
            .execute(pool)
            .await?;

            return Ok(serde_json::json!({
                "action": "add",
                "prediction_error": "reinforce",
                "reinforced_id": obs_id.to_string(),
                "message": "Very similar observation exists. Reinforced existing instead.",
                "write_to_searchable_ms": started.elapsed().as_millis() as u64
            }));
        }
        DedupResult::Unique(emb) => emb,
    };

    let importance = crate::constants::importance_prior(obs_type, density);
    let tags = extract_tags(content);

    let active_session_id: Option<uuid::Uuid> = crate::session::session_id();

    let trust = crate::core::trust::for_source(source, args.get("trust").and_then(|v| v.as_str()));

    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source, importance, session_id, tags, project_id, trust)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(entity_id)
    .bind(content)
    .bind(obs_type)
    .bind(source)
    .bind(importance)
    .bind(active_session_id)
    .bind(&tags)
    .bind(project_id)
    .bind(&trust)
    .fetch_one(pool)
    .await
    .context("failed to insert observation")?;

    crate::core::bitemporal::append_observation_fact(
        pool,
        entity_id,
        entity_name,
        obs_type,
        content,
        project_id,
        importance as f32,
    )
    .await;

    tracing::info!(
        entity = %entity_name,
        obs_type = %obs_type,
        density = %density,
        importance = %importance,
        "observation added"
    );

    let (embedding, _chunks) = persist_searchable(
        pool,
        row.0,
        content,
        &dedup_entity_type,
        entity_name,
        project_id,
        precomputed_embedding,
    )
    .await;

    let obs_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_observations WHERE entity_id = $1 AND observation_type != 'superseded'"
    )
    .bind(entity_id)
    .fetch_one(pool)
    .await
    .unwrap_or((0,));

    let (overload_warning, superseded) = if obs_count.0 > 50 {
        let retired = retire_low_value_observations(pool, entity_id, entity_name).await;

        let pool_clone = pool.clone();
        let eid = entity_id;
        let entity_for_log = entity_name.to_string();
        crate::tasks::spawn(async move {
            let merged = sqlx::query(
                "WITH dupes AS (
                    SELECT a.id AS keep_id, b.id AS remove_id
                    FROM brain_observations a JOIN brain_observations b
                    ON a.entity_id = b.entity_id AND a.id < b.id
                    WHERE a.entity_id = $1
                      AND similarity(a.content, b.content) > 0.9
                      AND a.observation_type != 'superseded' AND b.observation_type != 'superseded'
                    LIMIT 10
                )
                UPDATE brain_observations SET observation_type = 'superseded'
                WHERE id IN (SELECT remove_id FROM dupes)",
            )
            .bind(eid)
            .execute(&pool_clone)
            .await;

            match merged {
                Ok(done) if done.rows_affected() > 0 => {
                    tracing::info!(entity = %entity_for_log, merged = done.rows_affected(), "near-duplicates marked superseded")
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(entity = %entity_for_log, error = %e, "near-duplicate consolidation failed")
                }
            }
        });

        (
            Some(format!(
                "Entity '{}' has {} observations. Auto-consolidation marked {} low-value observation(s) as 'superseded' — hidden from search, not deleted, reversible. Consider cuba_zafra(action='summarize') for further compression.",
                entity_name, obs_count.0, retired
            )),
            retired,
        )
    } else {
        (None, 0)
    };

    Ok(serde_json::json!({
        "action": "add",
        "id": row.0.to_string(),
        "entity_name": entity_name,
        "observation_type": obs_type,
        "information_density": density,
        "importance": importance,
        "tags": tags,
        "overload_warning": overload_warning,
        "superseded": superseded,
        "embedding": embedding,
        "write_to_searchable_ms": started.elapsed().as_millis() as u64
    }))
}

const RETIRE_LOW_VALUE_SQL: &str = "UPDATE brain_observations SET observation_type = 'superseded'
     WHERE entity_id = $1 AND importance < 0.05
       AND observation_type NOT IN ('decision', 'lesson', 'superseded')";

async fn retire_low_value_observations(
    pool: &PgPool,
    entity_id: uuid::Uuid,
    entity_name: &str,
) -> u64 {
    match sqlx::query(RETIRE_LOW_VALUE_SQL)
        .bind(entity_id)
        .execute(pool)
        .await
    {
        Ok(done) => {
            let retired = done.rows_affected();
            if retired > 0 {
                tracing::info!(entity = %entity_name, retired, "low-value observations marked superseded");
            }
            retired
        }
        Err(e) => {
            tracing::warn!(entity = %entity_name, error = %e, "failed to retire low-value observations");
            0
        }
    }
}

async fn delete_obs(pool: &PgPool, args: &Value) -> Result<Value> {
    let obs_id = args
        .get("observation_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let id: uuid::Uuid = obs_id.parse().context("invalid observation_id UUID")?;

    let result = sqlx::query("DELETE FROM brain_observations WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        anyhow::bail!("Observation not found");
    }

    Ok(serde_json::json!({
        "action": "delete",
        "deleted_id": obs_id
    }))
}

async fn list(pool: &PgPool, entity_name: &str) -> Result<Value> {
    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required");
    }

    let entity_id = get_entity_id(pool, entity_name).await?;
    let project_id = crate::project::current_project_id(pool).await?;

    let observations: Vec<(uuid::Uuid, String, String, f64, String, i32)> = sqlx::query_as(
        "SELECT id, content, observation_type, importance, source, access_count
         FROM brain_observations
         WHERE entity_id = $1 AND observation_type != 'superseded'
           AND ($2::uuid IS NULL OR project_id = $2 OR project_id IS NULL)
         ORDER BY importance DESC, created_at DESC",
    )
    .bind(entity_id)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let obs_json: Vec<Value> = observations
        .iter()
        .map(|(id, content, obs_type, imp, source, ac)| {
            serde_json::json!({
                "id": id.to_string(),
                "content": content,
                "type": obs_type,
                "importance": imp,
                "source": source,
                "access_count": ac
            })
        })
        .collect();

    Ok(serde_json::json!({
        "action": "list",
        "entity_name": entity_name,
        "observations": obs_json,
        "count": obs_json.len()
    }))
}

async fn batch_add(pool: &PgPool, args: &Value) -> Result<Value> {
    let observations = args
        .get("observations")
        .and_then(|v| v.as_array())
        .context("'observations' array is required for batch_add")?;

    if observations.len() > 100 {
        anyhow::bail!(
            "batch_add limit is 100 observations per call (got {})",
            observations.len()
        );
    }

    for (index, obs) in observations.iter().enumerate() {
        if let Some(content) = obs.get("content").and_then(|v| v.as_str()) {
            crate::redact::refuse_secrets(
                args,
                &format!("observations[{index}].content"),
                content,
            )?;
        }
    }

    type PendingEmbed = (uuid::Uuid, String, String, String, Option<Vec<f32>>);

    struct PendingObs {
        precomputed_embedding: Option<Vec<f32>>,
        entity_id: uuid::Uuid,
        entity_name: String,
        entity_type: String,
        content: String,
        obs_type: String,
        source: String,
        importance: f64,
        tags: Vec<String>,
        trust: String,
    }
    struct InsertedObs {
        entity_id: uuid::Uuid,
        entity_name: String,
        content: String,
        obs_type: String,
        importance: f64,
    }
    enum BatchAction {
        Insert(PendingObs),
        Reinforce(uuid::Uuid),
    }

    let mut actions: Vec<BatchAction> = Vec::new();
    let mut deduplicated = 0u32;

    let project_id = crate::project::current_project_id(pool).await?;

    for obs in observations {
        let entity_name = obs
            .get("entity_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let content = obs.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let obs_type = obs
            .get("observation_type")
            .and_then(|v| v.as_str())
            .unwrap_or("fact");
        let source = obs
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");

        if entity_name.is_empty() || content.is_empty() {
            continue;
        }

        let (entity_id, entity_type) = ensure_entity_typed(pool, entity_name, project_id).await?;

        let dedup = check_dedup(pool, entity_id, content, &entity_type, entity_name).await?;

        match dedup {
            DedupResult::Duplicate(_) => {
                deduplicated += 1;
            }
            DedupResult::Reinforce(obs_id) => {
                actions.push(BatchAction::Reinforce(obs_id));
            }
            DedupResult::Unique(precomputed) => {
                let density = information_density(content);
                let importance = crate::constants::importance_prior(obs_type, density);
                let tags = extract_tags(content);
                actions.push(BatchAction::Insert(PendingObs {
                    precomputed_embedding: precomputed,
                    entity_id,
                    entity_name: entity_name.to_string(),
                    entity_type,
                    content: content.to_string(),
                    obs_type: obs_type.to_string(),
                    source: source.to_string(),
                    importance,
                    tags,
                    trust: crate::core::trust::for_source(
                        source,
                        obs.get("trust").and_then(|v| v.as_str()),
                    ),
                }));
            }
        }
    }

    let active_session_id: Option<uuid::Uuid> = crate::session::session_id();

    let mut tx = pool.begin().await.context("failed to begin transaction")?;
    let mut added = 0u32;
    let mut reinforced = 0u32;
    let mut inserted_for_embed: Vec<PendingEmbed> = Vec::new();
    let mut inserted_for_facts: Vec<InsertedObs> = Vec::new();

    for action in &actions {
        match action {
            BatchAction::Insert(pending) => {
                let row: (uuid::Uuid,) = sqlx::query_as(
                    "INSERT INTO brain_observations (entity_id, content, observation_type, source, importance, session_id, tags, project_id, trust)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
                )
                .bind(pending.entity_id)
                .bind(&pending.content)
                .bind(&pending.obs_type)
                .bind(&pending.source)
                .bind(pending.importance)
                .bind(active_session_id)
                .bind(&pending.tags)
                .bind(project_id)
                .bind(&pending.trust)
                .fetch_one(&mut *tx)
                .await?;
                inserted_for_embed.push((
                    row.0,
                    pending.content.clone(),
                    pending.entity_name.clone(),
                    pending.entity_type.clone(),
                    pending.precomputed_embedding.clone(),
                ));
                inserted_for_facts.push(InsertedObs {
                    entity_id: pending.entity_id,
                    entity_name: pending.entity_name.clone(),
                    content: pending.content.clone(),
                    obs_type: pending.obs_type.clone(),
                    importance: pending.importance,
                });
                added += 1;
            }
            BatchAction::Reinforce(obs_id) => {
                sqlx::query(
                    "UPDATE brain_observations SET
                        importance = LEAST(importance + 0.05, 1.0),
                        access_count = access_count + 1,
                        last_accessed = NOW()
                     WHERE id = $1",
                )
                .bind(obs_id)
                .execute(&mut *tx)
                .await?;
                reinforced += 1;
            }
        }
    }

    tx.commit()
        .await
        .context("failed to commit batch_add transaction")?;

    for obs in &inserted_for_facts {
        crate::core::bitemporal::append_observation_fact(
            pool,
            obs.entity_id,
            &obs.entity_name,
            &obs.obs_type,
            &obs.content,
            project_id,
            obs.importance as f32,
        )
        .await;
    }

    let mut chunks_written = 0usize;
    for (obs_id, content, entity_name, entity_type, precomputed) in inserted_for_embed {
        let (_status, n) = persist_searchable(
            pool,
            obs_id,
            &content,
            &entity_type,
            &entity_name,
            project_id,
            precomputed,
        )
        .await;
        chunks_written += n;
    }

    Ok(serde_json::json!({
        "action": "batch_add",
        "added": added,
        "deduplicated": deduplicated,
        "reinforced": reinforced,
        "total_processed": added + deduplicated + reinforced,
        "chunks": chunks_written
    }))
}

async fn timeline(pool: &PgPool, entity_name: &str) -> Result<Value> {
    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required for timeline");
    }

    let project_id = crate::project::current_project_id(pool).await?;

    type ObsRow = (
        uuid::Uuid,
        String,
        String,
        f64,
        i32,
        serde_json::Value,
        chrono::DateTime<chrono::Utc>,
    );
    let observations: Vec<ObsRow> = sqlx::query_as(
        "SELECT o.id, o.content, o.observation_type, o.importance::float8,
                o.version, o.previous_versions, o.created_at
         FROM brain_observations o
         JOIN brain_entities e ON o.entity_id = e.id
         WHERE e.name = $1 AND o.observation_type != 'superseded'
           AND ($2::uuid IS NULL OR o.project_id = $2 OR o.project_id IS NULL)
         ORDER BY o.created_at ASC
         LIMIT 100",
    )
    .bind(entity_name)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    type EpRow = (
        uuid::Uuid,
        String,
        Vec<String>,
        Vec<String>,
        f64,
        chrono::DateTime<chrono::Utc>,
    );
    let episodes: Vec<EpRow> = sqlx::query_as(
        "SELECT ep.id, ep.content, ep.actors, ep.artifacts,
                ep.importance::float8, ep.started_at
         FROM brain_episodes ep
         JOIN brain_entities e ON ep.entity_id = e.id
         WHERE e.name = $1
           AND ($2::uuid IS NULL OR ep.project_id = $2 OR ep.project_id IS NULL)
         ORDER BY ep.started_at ASC
         LIMIT 50",
    )
    .bind(entity_name)
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut timeline_items: Vec<Value> = Vec::new();

    for (id, content, obs_type, importance, version, prev_versions, created_at) in &observations {
        let mut item = serde_json::json!({
            "type": "observation",
            "id": id.to_string(),
            "content": content,
            "observation_type": obs_type,
            "importance": importance,
            "version": version,
            "timestamp": created_at.to_rfc3339()
        });
        if *version > 1 {
            item["previous_versions"] = prev_versions.clone();
        }
        timeline_items.push(item);
    }

    for (id, content, actors, artifacts, importance, started_at) in &episodes {
        timeline_items.push(serde_json::json!({
            "type": "episode",
            "id": id.to_string(),
            "content": content,
            "actors": actors,
            "artifacts": artifacts,
            "importance": importance,
            "timestamp": started_at.to_rfc3339()
        }));
    }

    timeline_items.sort_by(|a, b| {
        let ts_a = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let ts_b = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        ts_a.cmp(ts_b)
    });

    let count = timeline_items.len();
    Ok(serde_json::json!({
        "action": "timeline",
        "entity_name": entity_name,
        "items": timeline_items,
        "count": count
    }))
}

enum DedupResult {
    Unique(Option<Vec<f32>>),
    Duplicate(String),
    Reinforce(uuid::Uuid),
}

async fn check_dedup(
    pool: &PgPool,
    entity_id: uuid::Uuid,
    content: &str,
    entity_type: &str,
    entity_name: &str,
) -> Result<DedupResult> {
    let dupes: Vec<(uuid::Uuid, String, f64)> = sqlx::query_as(
        "SELECT id, content, similarity(content, $2)::float8 AS sim
         FROM brain_observations
         WHERE entity_id = $1 AND similarity(content, $2) > 0.3
         ORDER BY sim DESC LIMIT 5",
    )
    .bind(entity_id)
    .bind(content)
    .fetch_all(pool)
    .await?;

    if dupes.is_empty() {
        if crate::embeddings::onnx::is_model_loaded()
            && let Ok(emb) =
                crate::embeddings::onnx::embed_passage_contextual(content, entity_type, entity_name)
                    .await
        {
            let semantic_match: Option<(uuid::Uuid, f64)> = sqlx::query_as(
                "SELECT id, (1.0 - (embedding <=> $1::vector))::float8 AS sim
                 FROM brain_observations
                 WHERE entity_id = $2 AND embedding IS NOT NULL AND observation_type != 'superseded'
                 ORDER BY embedding <=> $1::vector LIMIT 1",
            )
            .bind(pgvector::Vector::from(emb.clone()))
            .bind(entity_id)
            .fetch_optional(pool)
            .await?;

            if let Some((obs_id, sim)) = semantic_match
                && sim > 0.92
            {
                return Ok(DedupResult::Reinforce(obs_id));
            }
            return Ok(DedupResult::Unique(Some(emb)));
        }
        return Ok(DedupResult::Unique(None));
    }

    let recent_sims: Vec<(f64,)> = sqlx::query_as(
        "SELECT similarity(content, $2)::float8
         FROM brain_observations
         WHERE entity_id = $1 AND observation_type != 'superseded'
         ORDER BY created_at DESC LIMIT 20",
    )
    .bind(entity_id)
    .bind(content)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let recent: Vec<f64> = recent_sims.iter().map(|(s,)| *s).collect();

    for (obs_id, existing_content, sim) in &dupes {
        let sim = *sim;
        if sim > DEDUP_THRESHOLD {
            return Ok(DedupResult::Duplicate(
                super::zafra::safe_truncate(existing_content, 80).to_string(),
            ));
        }
        let action = prediction_error::adaptive_gate(sim, &recent);
        match action {
            prediction_error::GatingAction::Reinforce => {
                return Ok(DedupResult::Reinforce(*obs_id));
            }
            prediction_error::GatingAction::Update | prediction_error::GatingAction::Create => {}
        }
    }

    Ok(DedupResult::Unique(None))
}

async fn ensure_entity(
    pool: &PgPool,
    name: &str,
    project_id: Option<uuid::Uuid>,
) -> Result<uuid::Uuid> {
    Ok(ensure_entity_typed(pool, name, project_id).await?.0)
}

async fn ensure_entity_typed(
    pool: &PgPool,
    name: &str,
    project_id: Option<uuid::Uuid>,
) -> Result<(uuid::Uuid, String)> {
    let existing: Option<(uuid::Uuid, String)> =
        sqlx::query_as("SELECT id, entity_type FROM brain_entities WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?;

    if let Some(found) = existing {
        return Ok(found);
    }

    let merged_away: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT e.id, e.entity_type
         FROM brain_entity_aliases a JOIN brain_entities e ON e.id = a.entity_id
         WHERE a.alias_text = $1
         LIMIT 1",
    )
    .bind(name)
    .fetch_optional(pool)
    .await?;

    if let Some(found) = merged_away {
        return Ok(found);
    }

    let row: (uuid::Uuid, String) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type, project_id)
         VALUES ($1, 'concept', $2)
         ON CONFLICT (name) DO UPDATE SET updated_at = NOW()
         RETURNING id, entity_type",
    )
    .bind(name)
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    tracing::info!(entity = %name, "auto-created entity for observation");
    Ok(row)
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

async fn episode_add(pool: &PgPool, entity_name: &str, args: &Value) -> Result<Value> {
    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required for episode_add");
    }

    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if content.is_empty() {
        anyhow::bail!("content is required for episode_add");
    }

    crate::redact::refuse_secrets(args, "content", content)?;

    let actors: Vec<String> = args
        .get("actors")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let artifacts: Vec<String> = args
        .get("artifacts")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let project_id = crate::project::current_project_id(pool).await?;

    let entity_id = ensure_entity(pool, entity_name, project_id).await?;
    let density = information_density(content);

    let active_session_id: Option<uuid::Uuid> = crate::session::session_id();

    let row: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_episodes (entity_id, content, actors, artifacts, importance, session_id, project_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(entity_id)
    .bind(content)
    .bind(&actors)
    .bind(&artifacts)
    .bind(density.min(1.0))
    .bind(active_session_id)
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    let ep_id = row.0;

    let embed_pool = pool.clone();
    let content_owned = content.to_string();
    let entity_name_owned = entity_name.to_string();
    crate::tasks::spawn(async move {
        let entity_type: String =
            sqlx::query_scalar("SELECT entity_type FROM brain_entities WHERE name = $1")
                .bind(&entity_name_owned)
                .fetch_optional(&embed_pool)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| "concept".to_string());

        if crate::embeddings::onnx::is_model_loaded()
            && let Ok(emb) = crate::embeddings::onnx::embed_passage_contextual(
                &content_owned,
                &entity_type,
                &entity_name_owned,
            )
            .await
        {
            let model = crate::embeddings::onnx::current_model();
            let stored = sqlx::query(
                "UPDATE brain_episodes SET embedding = $1::vector, embedding_model = $2 WHERE id = $3",
            )
            .bind(pgvector::Vector::from(emb))
            .bind(model)
            .bind(ep_id)
            .execute(&embed_pool)
            .await;
            if let Err(e) = stored {
                tracing::warn!(episode_id = %ep_id, error = %e, "failed to store embedding — episode stays invisible to vector search");
            }
        }
    });

    tracing::info!(
        entity = %entity_name,
        episode_id = %ep_id,
        actors = actors.len(),
        artifacts = artifacts.len(),
        "episode added"
    );

    Ok(serde_json::json!({
        "action": "episode_add",
        "id": ep_id.to_string(),
        "entity_name": entity_name,
        "actors": actors,
        "artifacts": artifacts,
        "importance": density.min(1.0)
    }))
}

async fn episode_list(pool: &PgPool, entity_name: &str) -> Result<Value> {
    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required for episode_list");
    }

    let project_id = crate::project::current_project_id(pool).await?;

    type EpisodeRow = (
        uuid::Uuid,
        String,
        Vec<String>,
        Vec<String>,
        f64,
        chrono::DateTime<chrono::Utc>,
    );
    let episodes: Vec<EpisodeRow> = sqlx::query_as(
        "SELECT ep.id, ep.content, ep.actors, ep.artifacts,
                    ep.importance::float8, ep.started_at
             FROM brain_episodes ep
             JOIN brain_entities e ON ep.entity_id = e.id
             WHERE e.name = $1
               AND ($2::uuid IS NULL OR ep.project_id = $2 OR ep.project_id IS NULL)
             ORDER BY ep.started_at DESC
             LIMIT 50",
    )
    .bind(entity_name)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let items: Vec<Value> = episodes
        .iter()
        .map(|(id, content, actors, artifacts, importance, started_at)| {
            serde_json::json!({
                "id": id.to_string(),
                "content": content,
                "actors": actors,
                "artifacts": artifacts,
                "importance": importance,
                "started_at": started_at.to_rfc3339()
            })
        })
        .collect();

    Ok(serde_json::json!({
        "action": "episode_list",
        "entity_name": entity_name,
        "episodes": items,
        "count": items.len()
    }))
}

fn information_density(content: &str) -> f64 {
    density::information_density(content)
}

fn extract_tags(content: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can",
        "for", "and", "but", "or", "nor", "not", "no", "so", "yet", "both", "either", "neither",
        "each", "every", "all", "any", "few", "more", "most", "other", "some", "such", "than",
        "too", "very", "just", "about", "above", "after", "again", "also", "as", "at", "before",
        "between", "by", "from", "how", "in", "into", "it", "its", "of", "on", "only", "out",
        "over", "own", "same", "that", "this", "these", "those", "through", "to", "under", "until",
        "up", "what", "when", "where", "which", "while", "who", "whom", "why", "with", "el", "la",
        "los", "las", "un", "una", "unos", "unas", "de", "del", "en", "con", "por", "para", "es",
        "son", "fue", "ser", "estar", "que", "se", "si", "como", "pero", "mas", "ya", "entre",
        "cuando", "todo", "esta", "desde", "su", "sus", "le", "les",
    ];

    let words: Vec<String> = content
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|w| w.len() > 2 && !STOPWORDS.contains(&&**w))
        .map(String::from)
        .collect();

    let mut freq: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for w in &words {
        *freq.entry(w.clone()).or_default() += 1;
    }

    let mut sorted: Vec<(String, usize)> = freq.into_iter().collect();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.1));
    sorted.into_iter().take(5).map(|(w, _)| w).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://cronica-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    const PASTED_TOKEN: &str = "el deploy usa ghp_abcdefghijklmnop";

    #[tokio::test]
    async fn every_free_text_entry_point_refuses_a_credential_before_the_first_query() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        for args in [
            serde_json::json!({"action": "add", "entity_name": "redact-gate", "content": PASTED_TOKEN}),
            serde_json::json!({"action": "episode_add", "entity_name": "redact-gate", "content": PASTED_TOKEN}),
            serde_json::json!({"action": "batch_add", "observations": [
                {"entity_name": "redact-gate", "content": "esta observación no tiene nada raro"},
                {"entity_name": "redact-gate", "content": PASTED_TOKEN}
            ]}),
        ] {
            let action = args["action"].as_str().unwrap_or_default().to_string();
            let Err(failure) = handle(&pool, args).await else {
                panic!(
                    "{action} answered Ok on a pool that cannot connect: it never reached the \
                     database, so nothing but the secret gate could have answered — and the gate \
                     must refuse, not accept"
                );
            };

            let chain = format!("{failure:#}");
            assert!(
                chain.contains("github token"),
                "{action} failed on the database instead of refusing the pasted token: the check \
                 either is not wired in or runs after the first query, and by then batch_add has \
                 already created entities. Got: {chain}"
            );
            assert!(
                !chain.contains("ghp_abcdefghijklmnop"),
                "{action} repeated the secret in its refusal, and refusals get logged: {chain}"
            );
        }
    }

    #[tokio::test]
    async fn allow_secret_lets_the_write_reach_the_database() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "action": "add",
            "entity_name": "redact-gate",
            "content": PASTED_TOKEN,
            "allow_secret": true
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!("a pool that cannot connect cannot answer Ok to an insert");
        };

        let chain = format!("{failure:#}");
        assert!(
            !chain.contains("refusing to write"),
            "allow_secret did not open the gate: a caller with a legitimate reason to store the \
             text has no way through, and will store it somewhere with no gate at all. Got: \
             {chain}"
        );
    }

    #[test]
    fn test_information_density_diverse() {
        let d = information_density("rust is fast and safe for systems");
        assert!(d > 0.8, "diverse text should have high density: got {d}");
    }

    #[test]
    fn test_information_density_repetitive() {
        let d = information_density("hello hello hello hello hello");
        assert!(d < 0.3, "repetitive text should have low density: got {d}");
    }

    #[test]
    fn test_information_density_empty() {
        assert_eq!(information_density(""), 0.0);
    }

    #[test]
    fn auto_consolidation_can_never_destroy_an_observation() {
        assert!(
            !RETIRE_LOW_VALUE_SQL.to_uppercase().contains("DELETE"),
            "this runs unasked on every add to an entity over 50 observations, in the background \
             and over rows the user never selected: it must not be able to destroy one. SQL: \
             {RETIRE_LOW_VALUE_SQL}"
        );
        assert!(
            RETIRE_LOW_VALUE_SQL.contains("observation_type = 'superseded'"),
            "retirement must use the same reversible marker the rest of the project already uses \
             (zafra merge/summarize write it, every search excludes it). SQL: \
             {RETIRE_LOW_VALUE_SQL}"
        );
        assert!(
            RETIRE_LOW_VALUE_SQL.contains("NOT IN ('decision', 'lesson', 'superseded')"),
            "skipping the already-superseded rows is what keeps the count reported back to the \
             user honest: without it, every later add re-marks and re-reports the same rows as \
             newly retired. SQL: {RETIRE_LOW_VALUE_SQL}"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn adding_to_an_overloaded_entity_supersedes_low_value_observations_and_says_so() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");
        let pool = crate::db::create_pool(&url)
            .await
            .expect("connect to test database");

        let entity_name = format!("cronica-overload-{}", uuid::Uuid::new_v4());
        let entity_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
        )
        .bind(&entity_name)
        .fetch_one(&pool)
        .await
        .expect("seed the test entity");

        for i in 0..50 {
            sqlx::query(
                "INSERT INTO brain_observations (entity_id, content, observation_type, importance)
                 VALUES ($1, $2, 'fact', 0.5)",
            )
            .bind(entity_id)
            .bind(format!("qwrtzp{i} lkjhgf{i} mnbvcx{i}"))
            .execute(&pool)
            .await
            .expect("seed filler observations to push the entity over the overload threshold");
        }

        let low_value: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO brain_observations (entity_id, content, observation_type, importance)
             VALUES ($1, $2, 'fact', 0.01) RETURNING id",
        )
        .bind(entity_id)
        .bind("anotacion decaida que su duenio todavia podria querer recuperar")
        .fetch_one(&pool)
        .await
        .expect("seed the observation sitting in the deletion zone");

        let result = add(
            &pool,
            &entity_name,
            &serde_json::json!({
                "content": "the nine o'clock train pulls into central station every tuesday"
            }),
        )
        .await
        .expect("add must succeed on an overloaded entity");

        let survivor: Option<String> =
            sqlx::query_scalar("SELECT observation_type FROM brain_observations WHERE id = $1")
                .bind(low_value)
                .fetch_optional(&pool)
                .await
                .expect("read the low-importance observation back");
        let reported = result.get("superseded").and_then(|v| v.as_u64());
        let warning = result
            .get("overload_warning")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        sqlx::query("DELETE FROM brain_entities WHERE id = $1")
            .bind(entity_id)
            .execute(&pool)
            .await
            .expect("clean up the test entity and its cascaded rows");

        assert_eq!(
            survivor.as_deref(),
            Some("superseded"),
            "the row must still be there, only marked: an unrequested background DELETE was wiping \
             observations for good on every add to a big entity"
        );
        assert_eq!(
            reported,
            Some(1),
            "the caller has to be told what was done to its data — before, the response only said \
             'auto-consolidation triggered' and never mentioned the rows it had removed"
        );
        assert!(
            warning.contains("not deleted"),
            "the warning must say the rows survive, or the count reads as a deletion count: {warning}"
        );
    }
}
