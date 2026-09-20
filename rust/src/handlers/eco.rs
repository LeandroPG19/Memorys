use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let entity_name = args.get("entity_name").and_then(|v| v.as_str());
    let observation_id = args.get("observation_id").and_then(|v| v.as_str());

    match action {
        "positive" => positive(pool, entity_name, observation_id).await,
        "negative" => negative(pool, entity_name, observation_id).await,
        "correct" => correct(pool, observation_id, &args).await,
        "promote" => {
            set_trust(
                pool,
                Kind::parse(&args)?,
                &args,
                crate::core::trust::TRUSTED,
            )
            .await
        }
        "quarantine" => {
            set_trust(
                pool,
                Kind::parse(&args)?,
                &args,
                crate::core::trust::QUARANTINED,
            )
            .await
        }
        "pending" => list_quarantined(pool, &args).await,
        _ => anyhow::bail!(
            "Invalid action: {action}. Use positive/negative/correct/promote/quarantine/pending"
        ),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Observation,
    Episode,
    Error,
}

impl Kind {
    fn parse(args: &Value) -> Result<Self> {
        match args
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("observation")
        {
            "observation" => Ok(Self::Observation),
            "episode" => Ok(Self::Episode),
            "error" => Ok(Self::Error),
            other => anyhow::bail!("Invalid kind: {other}. Use observation/episode/error"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Episode => "episode",
            Self::Error => "error",
        }
    }

    fn id_field(self) -> &'static str {
        match self {
            Self::Observation => "observation_id",
            Self::Episode | Self::Error => "id",
        }
    }

    fn update_trust_sql(self) -> &'static str {
        match self {
            Self::Observation => {
                "UPDATE brain_observations SET trust = $1, updated_at = NOW() WHERE id = $2"
            }
            Self::Episode => "UPDATE brain_episodes SET trust = $1 WHERE id = $2",
            Self::Error => "UPDATE brain_errors SET trust = $1 WHERE id = $2",
        }
    }
}

async fn in_scope(
    pool: &PgPool,
    kind: Kind,
    id: uuid::Uuid,
    project_id: Option<uuid::Uuid>,
) -> Result<bool> {
    let sql = match kind {
        Kind::Observation => {
            return crate::project::observation_in_scope(pool, id, project_id).await;
        }
        Kind::Episode => {
            "SELECT 1 FROM brain_episodes
             WHERE id = $1 AND (project_id = $2)
             LIMIT 1"
        }
        Kind::Error => {
            "SELECT 1 FROM brain_errors
             WHERE id = $1 AND (project_id = $2)
             LIMIT 1"
        }
    };
    let Some(pid) = project_id else {
        return Ok(true);
    };
    let row: Option<(i32,)> = sqlx::query_as(sql)
        .bind(id)
        .bind(pid)
        .fetch_optional(pool)
        .await
        .with_context(|| format!("checking {} project scope", kind.as_str()))?;
    Ok(row.is_some())
}

async fn set_trust(pool: &PgPool, kind: Kind, args: &Value, trust: &str) -> Result<Value> {
    let id_str = args
        .get("id")
        .or_else(|| args.get("observation_id"))
        .and_then(|v| v.as_str())
        .with_context(|| format!("{} is required", kind.id_field()))?;
    let id: uuid::Uuid = id_str
        .parse()
        .with_context(|| format!("invalid {}", kind.id_field()))?;
    let project_id = crate::project::current_project_id(pool).await?;
    if !in_scope(pool, kind, id, project_id).await? {
        anyhow::bail!("{} not in current project scope", kind.as_str());
    }

    let result = sqlx::query(kind.update_trust_sql())
        .bind(trust)
        .bind(id)
        .execute(pool)
        .await
        .with_context(|| format!("updating {} trust", kind.as_str()))?;

    if result.rows_affected() == 0 {
        anyhow::bail!("{} not found: {id}", kind.as_str());
    }

    let event = if trust == crate::core::trust::TRUSTED {
        "memory.promote"
    } else {
        "memory.quarantine"
    };
    crate::handlers::archivo::handle(
        pool,
        serde_json::json!({
            "action": "append",
            "event_action": event,
            "payload": {
                "kind": kind.as_str(),
                (kind.id_field()): id.to_string(),
                "trust": trust
            }
        }),
    )
    .await
    .ok();

    Ok(serde_json::json!({
        "action": if trust == crate::core::trust::TRUSTED { "promote" } else { "quarantine" },
        "kind": kind.as_str(),
        (kind.id_field()): id.to_string(),
        "trust": trust,
        "retrievable": trust == crate::core::trust::TRUSTED,
    }))
}

async fn list_quarantined(pool: &PgPool, args: &Value) -> Result<Value> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(20)
        .clamp(1, 200);
    let project_id = crate::project::current_project_id(pool).await?;

    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        String,
        chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT o.id, e.name, o.content, o.source, o.created_at
             FROM brain_observations o
             JOIN brain_entities e ON e.id = o.entity_id
             WHERE o.trust = 'quarantined'
               AND ($1::uuid IS NULL OR o.project_id = $1)
             ORDER BY o.created_at DESC
             LIMIT $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("listing quarantined observations")?;

    let items: Vec<Value> = rows
        .iter()
        .map(|(id, entity, content, source, created)| {
            serde_json::json!({
                "kind": "observation",
                "observation_id": id.to_string(),
                "entity": entity,
                "content": content,
                "source": source,
                "created_at": created.to_rfc3339(),
            })
        })
        .collect();

    let episode_rows: Vec<(uuid::Uuid, String, String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT ep.id, e.name, ep.content, ep.created_at
             FROM brain_episodes ep
             JOIN brain_entities e ON e.id = ep.entity_id
             WHERE ep.trust = 'quarantined'
               AND ($1::uuid IS NULL OR ep.project_id = $1)
             ORDER BY ep.created_at DESC
             LIMIT $2",
        )
        .bind(project_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .context("listing quarantined episodes")?;

    let episodes: Vec<Value> = episode_rows
        .iter()
        .map(|(id, entity, content, created)| {
            serde_json::json!({
                "kind": "episode",
                "id": id.to_string(),
                "entity": entity,
                "content": content,
                "created_at": created.to_rfc3339(),
            })
        })
        .collect();

    type ErrorRow = (
        uuid::Uuid,
        String,
        String,
        Option<String>,
        chrono::DateTime<chrono::Utc>,
    );
    let error_rows: Vec<ErrorRow> = sqlx::query_as(
        "SELECT id, error_type, error_message, project, created_at
             FROM brain_errors
             WHERE trust = 'quarantined'
               AND ($1::uuid IS NULL OR project_id = $1)
             ORDER BY created_at DESC
             LIMIT $2",
    )
    .bind(project_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("listing quarantined errors")?;

    let errors: Vec<Value> = error_rows
        .iter()
        .map(|(id, error_type, message, project, created)| {
            serde_json::json!({
                "kind": "error",
                "id": id.to_string(),
                "error_type": error_type,
                "error_message": message,
                "project": project,
                "created_at": created.to_rfc3339(),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "action": "pending",
        "count": items.len() + episodes.len() + errors.len(),
        "quarantined": items,
        "quarantined_episodes": episodes,
        "quarantined_errors": errors,
        "note": "these are withheld from cuba_faro and cuba_expediente until promoted with action=promote, passing kind=episode or kind=error for the last two lists",
    }))
}

async fn positive(
    pool: &PgPool,
    entity_name: Option<&str>,
    observation_id: Option<&str>,
) -> Result<Value> {
    let mut boosted = 0u32;
    let project_id = crate::project::current_project_id(pool).await?;

    if let Some(obs_id_str) = observation_id {
        let obs_id: uuid::Uuid = obs_id_str.parse().context("invalid observation_id")?;
        if !crate::project::observation_in_scope(pool, obs_id, project_id).await? {
            anyhow::bail!("observation not in current project scope");
        }
        let result = sqlx::query(
            "UPDATE brain_observations SET
                importance = LEAST(
                    importance + (0.05 / SQRT(1.0 + access_count::float8 / 100.0)) * (1.0 - importance),
                    1.0
                ),
                access_count = access_count + 1,
                last_accessed = NOW()
             WHERE id = $1",
        )
        .bind(obs_id)
        .execute(pool)
        .await?;
        boosted += result.rows_affected() as u32;
    }

    if let Some(name) = entity_name {
        let result = sqlx::query(
            "UPDATE brain_entities SET
                importance = LEAST(
                    importance + (0.05 / SQRT(1.0 + access_count::float8 / 100.0)) * (1.0 - importance),
                    1.0
                ),
                access_count = access_count + 1,
                updated_at = NOW()
             WHERE name = $1
               AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(name)
        .bind(project_id)
        .execute(pool)
        .await?;
        boosted += result.rows_affected() as u32;
    }

    Ok(serde_json::json!({
        "action": "positive",
        "boosted_count": boosted,
        "rule": "oja_positive_robbins_monro"
    }))
}

async fn negative(
    pool: &PgPool,
    entity_name: Option<&str>,
    observation_id: Option<&str>,
) -> Result<Value> {
    let mut decreased = 0u32;
    let project_id = crate::project::current_project_id(pool).await?;

    if let Some(obs_id_str) = observation_id {
        let obs_id: uuid::Uuid = obs_id_str.parse().context("invalid observation_id")?;
        if !crate::project::observation_in_scope(pool, obs_id, project_id).await? {
            anyhow::bail!("observation not in current project scope");
        }
        let result = sqlx::query(
            "UPDATE brain_observations SET
                importance = GREATEST(
                    importance - (0.05 / SQRT(1.0 + access_count::float8 / 100.0)) * importance,
                    0.0
                ),
                last_accessed = NOW()
             WHERE id = $1",
        )
        .bind(obs_id)
        .execute(pool)
        .await?;
        decreased += result.rows_affected() as u32;
    }

    if let Some(name) = entity_name {
        let result = sqlx::query(
            "UPDATE brain_entities SET
                importance = GREATEST(
                    importance - (0.05 / SQRT(1.0 + access_count::float8 / 100.0)) * importance,
                    0.0
                ),
                updated_at = NOW()
             WHERE name = $1
               AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(name)
        .bind(project_id)
        .execute(pool)
        .await?;
        decreased += result.rows_affected() as u32;
    }

    Ok(serde_json::json!({
        "action": "negative",
        "decreased_count": decreased,
        "rule": "oja_negative_robbins_monro"
    }))
}

async fn correct(pool: &PgPool, observation_id: Option<&str>, args: &Value) -> Result<Value> {
    let obs_id_str = observation_id.context("observation_id required for correct")?;
    let obs_id: uuid::Uuid = obs_id_str.parse().context("invalid observation_id")?;
    let correction = args
        .get("correction")
        .and_then(|v| v.as_str())
        .context("correction text is required")?;

    crate::redact::refuse_secrets(args, "correction", correction)?;

    let project_id = crate::project::current_project_id(pool).await?;
    if !crate::project::observation_in_scope(pool, obs_id, project_id).await? {
        anyhow::bail!("observation not in current project scope");
    }

    let result = sqlx::query(
        "UPDATE brain_observations SET
            previous_versions = brain_append_version(
                previous_versions,
                jsonb_build_array(jsonb_build_object(
                    'content', content, 'version', version,
                    'origin_node', origin_node, 'corrected_at', NOW()::text
                ))
            ),
            content = $2,
            version = version + 1,
            embedding = NULL,
            embedding_half = NULL,
            last_accessed = NOW(),
            updated_at = NOW()
         WHERE id = $1",
    )
    .bind(obs_id)
    .bind(correction)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        anyhow::bail!("Observation not found");
    }

    Ok(serde_json::json!({
        "action": "correct",
        "observation_id": obs_id_str,
        "new_content": correction,
        "versioned": true
    }))
}

pub async fn reflect(pool: &PgPool, session_id: uuid::Uuid) -> Result<String> {
    type Row = (String, i64);
    let by_type: Vec<Row> = sqlx::query_as(
        "SELECT observation_type, COUNT(*) FROM brain_observations
         WHERE session_id = $1
         GROUP BY observation_type
         ORDER BY COUNT(*) DESC",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let recent: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT e.name, o.observation_type, o.content
         FROM brain_observations o
         JOIN brain_entities e ON o.entity_id = e.id
         WHERE o.session_id = $1 AND o.observation_type != 'superseded'
         ORDER BY o.created_at DESC
         LIMIT 12",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut md = String::with_capacity(2048);
    md.push_str("# Session reflection\n\n");
    if !by_type.is_empty() {
        md.push_str("## Counts by type\n");
        for (t, n) in &by_type {
            md.push_str(&format!("- {t}: {n}\n"));
        }
        md.push('\n');
    }
    if !recent.is_empty() {
        md.push_str("## Recent observations (newest first)\n");
        for (entity, ty, content) in &recent {
            let snippet = crate::handlers::zafra::safe_truncate(content, 160);
            md.push_str(&format!("- [{ty}] {entity}: {snippet}\n"));
        }
    }
    Ok(md)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://eco-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    async fn test_pool() -> PgPool {
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");
        crate::db::create_pool(&url)
            .await
            .expect("connect to test database")
    }

    fn unique_name(prefix: &str) -> String {
        format!("{}_{}", prefix, &uuid::Uuid::new_v4().to_string()[..8])
    }

    async fn failure_chain(args: Value) -> String {
        let pool = pool_that_cannot_connect();
        match handle(&pool, args).await {
            Err(failure) => format!("{failure:#}"),
            Ok(value) => panic!(
                "the gate answered Ok on a pool that cannot connect, so it never reached the \
                 table it claims to write: {value}"
            ),
        }
    }

    #[tokio::test]
    async fn promote_writes_the_table_the_kind_names() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        crate::session::clear();
        let id = uuid::Uuid::new_v4().to_string();

        let legacy =
            failure_chain(serde_json::json!({ "action": "promote", "observation_id": id.clone() }))
                .await;
        assert!(
            legacy.contains("updating observation trust"),
            "a client that already exists sends observation_id and no kind: that call has to keep \
             landing on brain_observations. Got: {legacy}"
        );

        let episode = failure_chain(
            serde_json::json!({ "action": "promote", "kind": "episode", "id": id.clone() }),
        )
        .await;
        assert!(
            episode.contains("updating episode trust"),
            "kind=episode has to reach brain_episodes; sync imports episodes too, and a promotion \
             that silently updated brain_observations would leave the quarantined episode \
             unreachable forever. Got: {episode}"
        );

        let error = failure_chain(
            serde_json::json!({ "action": "quarantine", "kind": "error", "id": id.clone() }),
        )
        .await;
        assert!(
            error.contains("updating error trust"),
            "and kind=error has to reach brain_errors, the other table expediente serves back. \
             Got: {error}"
        );
    }

    #[tokio::test]
    async fn an_unknown_kind_is_refused_before_any_table_is_touched() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        crate::session::clear();

        let chain = failure_chain(serde_json::json!({
            "action": "promote",
            "kind": "brain_users",
            "id": uuid::Uuid::new_v4().to_string()
        }))
        .await;

        assert!(
            chain.contains("Invalid kind: brain_users"),
            "an unrecognised kind has to be named and refused, not quietly treated as an \
             observation: the caller would read the Ok as 'the episode is retrievable now'. \
             Got: {chain}"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn quarantined_episodes_and_errors_are_listed_and_can_be_promoted_back() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        crate::session::clear();
        let pool = test_pool().await;
        let marker = unique_name("eco_kind");

        let (entity_id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
        )
        .bind(&marker)
        .fetch_one(&pool)
        .await
        .expect("creating the fixture entity");

        let (episode_id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO brain_episodes (entity_id, content, trust)
             VALUES ($1, $2, 'quarantined') RETURNING id",
        )
        .bind(entity_id)
        .bind(format!(
            "{marker} el importador trajo esto de un bundle editado a mano"
        ))
        .fetch_one(&pool)
        .await
        .expect("creating the fixture episode");

        let (error_id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO brain_errors (error_type, error_message, trust)
             VALUES ('ImportError', $1, 'quarantined') RETURNING id",
        )
        .bind(format!("{marker} el pool se quedó sin conexiones"))
        .fetch_one(&pool)
        .await
        .expect("creating the fixture error");

        let (observation_id,): (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO brain_observations (entity_id, content, observation_type, source, trust)
             VALUES ($1, $2, 'fact', 'agent', 'quarantined') RETURNING id",
        )
        .bind(entity_id)
        .bind(format!("{marker} la observación que ya se sabía retener"))
        .fetch_one(&pool)
        .await
        .expect("creating the fixture observation");

        let listed = handle(
            &pool,
            serde_json::json!({ "action": "pending", "limit": 200 }),
        )
        .await
        .expect("listing what is withheld");
        let listed_text = serde_json::to_string(&listed).expect("serialise the listing");
        assert!(
            listed_text.contains(&episode_id.to_string())
                && listed_text.contains(&error_id.to_string()),
            "a row nobody can list is a row nobody can promote: pending has to show all three \
             kinds. Got: {listed_text}"
        );

        let episode_promotion = handle(
            &pool,
            serde_json::json!({ "action": "promote", "kind": "episode", "id": episode_id.to_string() }),
        )
        .await;
        let error_promotion = handle(
            &pool,
            serde_json::json!({ "action": "promote", "kind": "error", "id": error_id.to_string() }),
        )
        .await;
        let legacy_promotion = handle(
            &pool,
            serde_json::json!({ "action": "promote", "observation_id": observation_id.to_string() }),
        )
        .await;

        let episode_trust: String =
            sqlx::query_scalar("SELECT trust FROM brain_episodes WHERE id = $1")
                .bind(episode_id)
                .fetch_one(&pool)
                .await
                .expect("reading the episode trust back");
        let error_trust: String =
            sqlx::query_scalar("SELECT trust FROM brain_errors WHERE id = $1")
                .bind(error_id)
                .fetch_one(&pool)
                .await
                .expect("reading the error trust back");
        let legacy_trust: String =
            sqlx::query_scalar("SELECT trust FROM brain_observations WHERE id = $1")
                .bind(observation_id)
                .fetch_one(&pool)
                .await
                .expect("reading the observation trust back");

        let episode_withdrawal = handle(
            &pool,
            serde_json::json!({ "action": "quarantine", "kind": "episode", "id": episode_id.to_string() }),
        )
        .await;
        let episode_withdrawn: String =
            sqlx::query_scalar("SELECT trust FROM brain_episodes WHERE id = $1")
                .bind(episode_id)
                .fetch_one(&pool)
                .await
                .expect("reading the episode trust back after withdrawal");

        sqlx::query("DELETE FROM brain_observations WHERE id = $1")
            .bind(observation_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM brain_errors WHERE id = $1")
            .bind(error_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM brain_episodes WHERE id = $1")
            .bind(episode_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM brain_entities WHERE id = $1")
            .bind(entity_id)
            .execute(&pool)
            .await
            .ok();

        assert_eq!(
            (episode_trust.as_str(), error_trust.as_str()),
            ("trusted", "trusted"),
            "promotion has to flip the row in its own table, or the quarantine is a one-way trap: \
             the data stays stored and stays unreachable. The episode promotion answered \
             {episode_promotion:?} and the error promotion {error_promotion:?}"
        );
        assert_eq!(
            episode_withdrawn, "quarantined",
            "and the transition has to work both ways, the same as it does for observations. \
             Withdrawal answered {episode_withdrawal:?}"
        );
        assert_eq!(
            legacy_trust, "trusted",
            "a promotion that names no kind and passes observation_id is what every client \
             written before this change sends, and it has to keep meaning exactly what it meant. \
             Answered {legacy_promotion:?}"
        );
    }

    #[tokio::test]
    async fn correcting_an_observation_cannot_smuggle_in_what_writing_it_would_have_refused() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = serde_json::json!({
            "action": "correct",
            "observation_id": uuid::Uuid::nil().to_string(),
            "correction": "en realidad la cabecera era ghp_abcdefghijklmnop"
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "correct answered Ok on a pool that cannot connect: nothing but the secret gate \
                 could have answered, and the gate must refuse"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("github token") && chain.contains("correction"),
            "correct overwrites the content of an already-stored observation, so an ungated \
             correction is a way to put into the graph exactly what cuba_cronica add refuses to \
             accept — a hole underneath an existing guard. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn a_quarantined_row_from_another_project_cannot_be_promoted() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL required");
        let pool = crate::db::create_pool(&url).await.expect("connect");

        let mine = crate::project::upsert_project(&pool, "eco_scope_mine")
            .await
            .expect("my project");
        let theirs = crate::project::upsert_project(&pool, "eco_scope_theirs")
            .await
            .expect("their project");

        let entity: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO brain_entities (name, entity_type, project_id)
             VALUES ($1, 'concept', $2) RETURNING id",
        )
        .bind(format!("eco_scope_{}", uuid::Uuid::new_v4()))
        .bind(theirs)
        .fetch_one(&pool)
        .await
        .expect("seed the entity");

        let episode: (uuid::Uuid,) = sqlx::query_as(
            "INSERT INTO brain_episodes (entity_id, content, project_id, trust)
             VALUES ($1, 'ajeno', $2, 'quarantined') RETURNING id",
        )
        .bind(entity.0)
        .bind(theirs)
        .fetch_one(&pool)
        .await
        .expect("seed the episode");

        crate::session::set(uuid::Uuid::new_v4(), Some(mine));
        let refused = handle(
            &pool,
            serde_json::json!({"action": "promote", "kind": "episode", "id": episode.0.to_string()}),
        )
        .await;
        crate::session::clear();

        let Err(failure) = refused else {
            let trust: String =
                sqlx::query_scalar("SELECT trust FROM brain_episodes WHERE id = $1")
                    .bind(episode.0)
                    .fetch_one(&pool)
                    .await
                    .expect("read the trust back");
            panic!(
                "promoting reported success on an episode belonging to another project, and the \
                 row is now trust={trust}. The scope branch for episode and error was written \
                 and never exercised: quarantine is what holds an imported credential back, so \
                 a client in one project releasing another project's quarantined row is the \
                 exact hole quarantine exists to close"
            );
        };
        assert!(
            format!("{failure:#}").contains("scope"),
            "the refusal has to say it is a scope problem, not look like a missing row. Got: {failure:#}"
        );

        let trust: String = sqlx::query_scalar("SELECT trust FROM brain_episodes WHERE id = $1")
            .bind(episode.0)
            .fetch_one(&pool)
            .await
            .expect("read the trust back");
        assert_eq!(trust, "quarantined", "and the row must not have moved");

        sqlx::query("DELETE FROM brain_entities WHERE id = $1")
            .bind(entity.0)
            .execute(&pool)
            .await
            .ok();
    }
}
