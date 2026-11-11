use anyhow::Result;
use sqlx::PgPool;

pub const DEFAULT_BACKFILL_LIMIT: i64 = 100;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BackfillReport {
    pub attempted: usize,
    pub embedded: usize,
    pub failed: usize,
    pub chunked: usize,
}

/// The tables that carry a vector a model has to produce. One literal statement per table: the
/// table name is never spliced into SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorTable {
    Observations,
    Episodes,
}

impl VectorTable {
    pub const fn update_sql(self) -> &'static str {
        match self {
            Self::Observations => {
                "UPDATE brain_observations SET embedding = $1::vector, embedding_model = $2 WHERE id = $3"
            }
            Self::Episodes => {
                "UPDATE brain_episodes SET embedding = $1::vector, embedding_model = $2 WHERE id = $3"
            }
        }
    }

    const fn missing_sql(self) -> &'static str {
        match self {
            Self::Observations => {
                "SELECT o.id, o.content, e.entity_type, e.name
                 FROM brain_observations o
                 JOIN brain_entities e ON e.id = o.entity_id
                 WHERE o.embedding IS NULL AND o.observation_type != 'superseded'
                 ORDER BY o.importance DESC, o.created_at DESC
                 LIMIT $1"
            }
            Self::Episodes => {
                "SELECT ep.id, ep.content, e.entity_type, e.name
                 FROM brain_episodes ep
                 JOIN brain_entities e ON e.id = ep.entity_id
                 WHERE ep.embedding IS NULL
                 ORDER BY ep.importance DESC, ep.created_at DESC
                 LIMIT $1"
            }
        }
    }
}

pub async fn store_chunks(
    pool: &PgPool,
    observation_id: uuid::Uuid,
    content: &str,
    entity_type: &str,
    entity_name: &str,
    project_id: Option<uuid::Uuid>,
) -> Result<usize> {
    let pieces = crate::embeddings::chunk::chunks_for(content);
    if pieces.is_empty() {
        return Ok(0);
    }

    sqlx::query("DELETE FROM brain_observation_chunks WHERE observation_id = $1")
        .bind(observation_id)
        .execute(pool)
        .await?;

    let model = crate::embeddings::onnx::current_model();
    let mut stored = 0usize;
    for (idx, piece) in pieces.iter().enumerate() {
        let Ok(vector) =
            crate::embeddings::onnx::embed_passage_contextual(piece, entity_type, entity_name)
                .await
        else {
            continue;
        };
        let done = sqlx::query(
            "INSERT INTO brain_observation_chunks
                (observation_id, chunk_index, content, embedding, embedding_model, project_id)
             VALUES ($1, $2, $3, $4::vector, $5, $6)
             ON CONFLICT (observation_id, chunk_index)
             DO UPDATE SET content = EXCLUDED.content,
                           embedding = EXCLUDED.embedding,
                           embedding_model = EXCLUDED.embedding_model",
        )
        .bind(observation_id)
        .bind(idx as i32)
        .bind(piece)
        .bind(pgvector::Vector::from(vector))
        .bind(&model)
        .bind(project_id)
        .execute(pool)
        .await;
        match done {
            Ok(_) => stored += 1,
            Err(e) => {
                tracing::warn!(error = %e, obs_id = %observation_id, chunk = idx, "could not persist chunk")
            }
        }
    }
    Ok(stored)
}

pub async fn count_unchunked(pool: &PgPool) -> Result<i64> {
    let threshold = crate::embeddings::chunk::threshold_chars() as i32;
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM brain_observations o
         WHERE o.observation_type != 'superseded'
           AND char_length(o.content) > $1
           AND NOT EXISTS (SELECT 1 FROM brain_observation_chunks c WHERE c.observation_id = o.id)",
    )
    .bind(threshold)
    .fetch_one(pool)
    .await?;
    Ok(n)
}

pub async fn backfill_chunks(pool: &PgPool, limit: i64) -> Result<usize> {
    if limit == 0 {
        return Ok(0);
    }
    let threshold = crate::embeddings::chunk::threshold_chars() as i32;
    let rows: Vec<(uuid::Uuid, String, String, String, Option<uuid::Uuid>)> = sqlx::query_as(
        "SELECT o.id, o.content, e.entity_type, e.name, o.project_id
         FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE o.observation_type != 'superseded'
           AND char_length(o.content) > $1
           AND NOT EXISTS (SELECT 1 FROM brain_observation_chunks c WHERE c.observation_id = o.id)
         ORDER BY char_length(o.content) DESC
         LIMIT $2",
    )
    .bind(threshold)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut done = 0usize;
    for (id, content, entity_type, entity_name, project_id) in rows {
        if store_chunks(pool, id, &content, &entity_type, &entity_name, project_id)
            .await
            .unwrap_or(0)
            > 0
        {
            done += 1;
        }
    }
    Ok(done)
}

pub fn backfill_limit() -> i64 {
    parse_limit(std::env::var("CUBA_REM_BACKFILL_LIMIT").ok().as_deref())
}

fn parse_limit(raw: Option<&str>) -> i64 {
    raw.and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|&n| n >= 0)
        .unwrap_or(DEFAULT_BACKFILL_LIMIT)
}

pub async fn count_missing(pool: &PgPool) -> Result<i64> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM brain_observations
                 WHERE embedding IS NULL AND observation_type != 'superseded')
              + (SELECT count(*) FROM brain_episodes WHERE embedding IS NULL)",
    )
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Fills what is missing in both tables, up to `limit` rows of each. Without a loaded model it
/// writes nothing: the hash fallback stored under the real model's name is a vector `reembed`
/// would take for a good one.
pub async fn backfill_missing(pool: &PgPool, limit: i64) -> Result<BackfillReport> {
    if limit == 0 || !crate::embeddings::onnx::model_loaded().await {
        return Ok(BackfillReport::default());
    }

    let model = crate::embeddings::onnx::current_model();
    let mut report = BackfillReport::default();
    for table in [VectorTable::Observations, VectorTable::Episodes] {
        let rows: Vec<(uuid::Uuid, String, String, String)> = sqlx::query_as(table.missing_sql())
            .bind(limit)
            .fetch_all(pool)
            .await?;
        report.attempted += rows.len();
        for (id, content, entity_type, entity_name) in rows {
            let vector = crate::embeddings::onnx::embed_passage_contextual(
                &content,
                &entity_type,
                &entity_name,
            )
            .await;
            let Ok(vector) = vector else {
                report.failed += 1;
                continue;
            };
            let written = sqlx::query(table.update_sql())
                .bind(pgvector::Vector::from(vector))
                .bind(&model)
                .bind(id)
                .execute(pool)
                .await;
            match written {
                Ok(_) => report.embedded += 1,
                Err(e) => {
                    tracing::warn!(error = %e, row_id = %id, table = ?table, "backfill: could not persist embedding");
                    report.failed += 1;
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_or_malformed_limit_falls_back_to_the_default() {
        assert_eq!(parse_limit(None), DEFAULT_BACKFILL_LIMIT);
        assert_eq!(parse_limit(Some("not a number")), DEFAULT_BACKFILL_LIMIT);
        assert_eq!(parse_limit(Some("")), DEFAULT_BACKFILL_LIMIT);
    }

    #[test]
    fn a_negative_limit_is_rejected_rather_than_passed_to_sql() {
        assert_eq!(parse_limit(Some("-3")), DEFAULT_BACKFILL_LIMIT);
    }

    #[test]
    fn zero_is_honoured_as_an_explicit_opt_out() {
        assert_eq!(parse_limit(Some("0")), 0);
    }

    #[test]
    fn a_valid_limit_is_used_verbatim() {
        assert_eq!(parse_limit(Some("7")), 7);
        assert_eq!(parse_limit(Some("  25  ")), 25);
    }
}
