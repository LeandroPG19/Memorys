use uuid::Uuid;

const REM_RELATION_BATCH_LOCK: i64 = 0x0CBA_A0D1_7106_0031;

async fn own_the_rem_relation_batch_env(
    pool: &sqlx::PgPool,
) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REM_RELATION_BATCH_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_REM_RELATION_BATCH is process-global");
    tx
}

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

async fn isolated_entity_awaiting_scan(pool: &sqlx::PgPool, name: &str) -> Uuid {
    let id: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'project') RETURNING id",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .expect("creating a fixture entity");

    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source)
         VALUES ($1, 'nota de relleno para el escaneo de relaciones', 'fact', 'agent')",
    )
    .bind(id.0)
    .execute(pool)
    .await
    .expect("adding a trusted observation");

    id.0
}

#[tokio::test]
#[ignore]
async fn an_explicit_batch_override_wins_over_the_queue_size() {
    let pool = pool().await;
    let _owns = own_the_rem_relation_batch_env(&pool).await;
    unsafe { std::env::set_var("CUBA_REM_RELATION_BATCH", "7") };

    let batch = memory_industry::protocol::rem_relation_scan_batch(&pool).await;

    unsafe { std::env::remove_var("CUBA_REM_RELATION_BATCH") };

    assert_eq!(
        batch, 7,
        "an explicit CUBA_REM_RELATION_BATCH must win no matter how large the backlog is — it \
         is the operator's escape hatch, and an adaptive default that overrules it would take \
         that hatch away silently"
    );
}

#[tokio::test]
#[ignore]
async fn a_batch_of_zero_still_disables_the_scan() {
    let pool = pool().await;
    let _owns = own_the_rem_relation_batch_env(&pool).await;
    unsafe { std::env::set_var("CUBA_REM_RELATION_BATCH", "0") };

    let batch = memory_industry::protocol::rem_relation_scan_batch(&pool).await;

    unsafe { std::env::remove_var("CUBA_REM_RELATION_BATCH") };

    assert_eq!(
        batch, 0,
        "CUBA_REM_RELATION_BATCH=0 must still turn the relation scan off entirely — going \
         adaptive must not resurrect a scan the operator explicitly killed"
    );
}

#[tokio::test]
#[ignore]
async fn a_backlog_of_fifty_or_more_grows_the_batch_past_the_default() {
    let pool = pool().await;
    let _owns = own_the_rem_relation_batch_env(&pool).await;
    unsafe { std::env::remove_var("CUBA_REM_RELATION_BATCH") };

    let baseline = memory_industry::handlers::ingesta::entities_awaiting_relation_scan(&pool, 50)
        .await
        .expect("measuring the ambient queue")
        .len();

    let mut fixtures = Vec::new();
    while baseline + fixtures.len() < 50 {
        let name = format!("RemBatchLarge_{}", &Uuid::new_v4().to_string()[..8]);
        fixtures.push(isolated_entity_awaiting_scan(&pool, &name).await);
    }

    let batch = memory_industry::protocol::rem_relation_scan_batch(&pool).await;

    for id in &fixtures {
        sqlx::query("DELETE FROM brain_entities WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .ok();
    }

    assert_eq!(
        batch, 20,
        "with 50 or more entities waiting, the batch must grow to 20 — five per REM cycle took \
         seven days to work through 226 pending entities. 20 is the chosen ceiling, not 50: \
         each entity costs one LLM call carrying a 90s budget, so 20 entities cost at most \
         1800s (30 min) inside a 4-hour REM_INTERVAL, leaving ample room; 50 would cost up to \
         4500s (75 min) for a call whose backend is a subprocess that can stall, and the \
         2-consecutive-failure cutoff already bounds a bad run — raising the ceiling only \
         raises how much of the cycle a bad run can eat"
    );
}

#[tokio::test]
#[ignore]
async fn a_small_backlog_keeps_the_default_batch_of_five() {
    let pool = pool().await;
    let _owns = own_the_rem_relation_batch_env(&pool).await;
    unsafe { std::env::remove_var("CUBA_REM_RELATION_BATCH") };

    let baseline = memory_industry::handlers::ingesta::entities_awaiting_relation_scan(&pool, 50)
        .await
        .expect("measuring the ambient queue")
        .len();
    assert!(
        baseline < 50,
        "this test proves the batch stays at its default of 5 while the backlog is small, and \
         that only means something against a queue under 50 — this database already has \
         {baseline} entities waiting, at or past the very threshold the sibling test \
         (a_backlog_of_fifty_or_more_grows_the_batch_past_the_default) exists to exercise. \
         Point DATABASE_URL at a database with a smaller backlog, or clear it, before running \
         this one"
    );

    let batch = memory_industry::protocol::rem_relation_scan_batch(&pool).await;

    assert_eq!(
        batch, 5,
        "below the 50-entity threshold, the batch must stay at the original default of 5 — \
         adaptive means it reacts to the queue, not that it is always large"
    );
}
