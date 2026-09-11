use uuid::Uuid;

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

fn unique_name(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

async fn trusted_observation_created_at(
    pool: &sqlx::PgPool,
    entity_name: &str,
    content: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> (Uuid, Uuid) {
    let entity: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'project') RETURNING id",
    )
    .bind(entity_name)
    .fetch_one(pool)
    .await
    .expect("creating the fixture entity");

    let observation: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_observations
             (entity_id, content, observation_type, source, trust, created_at)
         VALUES ($1, $2, 'fact', 'agent', 'trusted', $3) RETURNING id",
    )
    .bind(entity.0)
    .bind(content)
    .bind(created_at)
    .fetch_one(pool)
    .await
    .expect("creating the fixture observation");

    (entity.0, observation.0)
}

#[tokio::test]
#[ignore]
async fn a_row_that_keeps_failing_sinks_behind_fresh_work_it_used_to_block() {
    let pool = pool().await;
    let old = chrono::Utc::now() - chrono::Duration::hours(1);

    let (stuck_entity, stuck_obs) = trusted_observation_created_at(
        &pool,
        &unique_name("StuckExtraction"),
        "contenido que el LLM nunca podrá procesar",
        old,
    )
    .await;
    let (fresh_entity, fresh_obs) = trusted_observation_created_at(
        &pool,
        &unique_name("FreshExtraction"),
        "una nota nueva sin intentos previos",
        chrono::Utc::now(),
    )
    .await;

    sqlx::query("UPDATE brain_observations SET extraction_attempts = 3 WHERE id = $1")
        .bind(stuck_obs)
        .execute(&pool)
        .await
        .expect("simulating three prior consecutive failures");

    let queue = memory_industry::handlers::ingesta::observations_awaiting_extraction(&pool, 5_000)
        .await
        .expect("listing candidates");

    let stuck_pos = queue
        .iter()
        .position(|(id, _)| *id == stuck_obs)
        .expect("the stuck row must still be retried, never dropped from the queue");
    let fresh_pos = queue
        .iter()
        .position(|(id, _)| *id == fresh_obs)
        .expect("a freshly written observation must queue for extraction");

    sqlx::query("DELETE FROM brain_entities WHERE id IN ($1, $2)")
        .bind(stuck_entity)
        .bind(fresh_entity)
        .execute(&pool)
        .await
        .ok();

    assert!(
        fresh_pos < stuck_pos,
        "ORDER BY created_at ASC alone put the older, permanently-failing row ahead of every \
         write that came after it — with a small per-cycle batch and a two-consecutive-\
         failure cutoff, that row and whatever else failed with it were the only two \
         candidates the queue ever returned, forever. Sorting by extraction_attempts first \
         must let untried work cut ahead of a row that already failed three times: \
         queue={queue:?}"
    );
}

#[tokio::test]
#[ignore]
async fn a_failed_extraction_attempt_is_recorded_on_the_row_it_failed_on() {
    let pool = pool().await;
    let (entity_id, obs_id) = trusted_observation_created_at(
        &pool,
        &unique_name("AttemptCounter"),
        "nota cualquiera",
        chrono::Utc::now(),
    )
    .await;

    memory_industry::handlers::ingesta::mark_extraction_attempt_failed(&pool, obs_id).await;
    memory_industry::handlers::ingesta::mark_extraction_attempt_failed(&pool, obs_id).await;

    let attempts: (i32,) =
        sqlx::query_as("SELECT extraction_attempts FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_one(&pool)
            .await
            .expect("reading the attempts counter back");

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&pool)
        .await
        .ok();

    assert_eq!(
        attempts.0, 2,
        "two recorded failures must leave the counter at 2, or the queue has no way to tell \
         a row that keeps failing from one that has never been tried"
    );
}

async fn isolated_entity_awaiting_scan_created_at(
    pool: &sqlx::PgPool,
    name: &str,
    scanned_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Uuid {
    let id: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type, relations_scanned_at)
         VALUES ($1, 'project', $2) RETURNING id",
    )
    .bind(name)
    .bind(scanned_at)
    .fetch_one(pool)
    .await
    .expect("creating a fixture entity");

    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source, trust)
         VALUES ($1, 'nota de relleno para el escaneo de relaciones', 'fact', 'agent', 'trusted')",
    )
    .bind(id.0)
    .execute(pool)
    .await
    .expect("adding a trusted observation");

    id.0
}

#[tokio::test]
#[ignore]
async fn an_entity_that_keeps_failing_its_relation_scan_sinks_behind_fresh_entities() {
    let pool = pool().await;
    let never_scanned = None;

    let stuck = isolated_entity_awaiting_scan_created_at(
        &pool,
        &unique_name("StuckRelationScan"),
        never_scanned,
    )
    .await;
    let fresh = isolated_entity_awaiting_scan_created_at(
        &pool,
        &unique_name("FreshRelationScan"),
        never_scanned,
    )
    .await;

    sqlx::query("UPDATE brain_entities SET relation_scan_attempts = 3 WHERE id = $1")
        .bind(stuck)
        .execute(&pool)
        .await
        .expect("simulating three prior consecutive failures");

    let queue = memory_industry::handlers::ingesta::entities_awaiting_relation_scan(&pool, 5_000)
        .await
        .expect("listing candidates");

    let stuck_pos = queue
        .iter()
        .position(|id| *id == stuck)
        .expect("the stuck entity must still be retried, never dropped from the queue");
    let fresh_pos = queue
        .iter()
        .position(|id| *id == fresh)
        .expect("a freshly isolated entity must queue for a relation scan");

    sqlx::query("DELETE FROM brain_entities WHERE id IN ($1, $2)")
        .bind(stuck)
        .bind(fresh)
        .execute(&pool)
        .await
        .ok();

    assert!(
        fresh_pos < stuck_pos,
        "ORDER BY relations_scanned_at ASC NULLS FIRST alone treats every never-scanned entity \
         as equally overdue, so one that fails its scan every cycle stays at the front forever \
         and starves entities discovered after it. Sorting by relation_scan_attempts first \
         must let an untried entity cut ahead of one that already failed three times: \
         queue={queue:?}"
    );
}

#[tokio::test]
#[ignore]
async fn a_failed_relation_scan_attempt_is_recorded_on_the_entity_it_failed_on() {
    let pool = pool().await;
    let id =
        isolated_entity_awaiting_scan_created_at(&pool, &unique_name("ScanAttemptCounter"), None)
            .await;

    memory_industry::handlers::ingesta::mark_relation_scan_attempt_failed(&pool, id).await;
    memory_industry::handlers::ingesta::mark_relation_scan_attempt_failed(&pool, id).await;

    let attempts: (i32,) =
        sqlx::query_as("SELECT relation_scan_attempts FROM brain_entities WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("reading the attempts counter back");

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();

    assert_eq!(
        attempts.0, 2,
        "two recorded failures must leave the counter at 2, or the queue has no way to tell \
         an entity that keeps failing from one that has never been scanned"
    );
}
