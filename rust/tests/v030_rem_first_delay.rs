use uuid::Uuid;

const REM_FIRST_DELAY_LOCK: i64 = 0x0CBA_A0D1_7106_0030;

async fn own_the_rem_first_delay_env(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REM_FIRST_DELAY_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_REM_FIRST_DELAY_SECS and CUBA_REM_RELATION_BATCH are process-global");
    tx
}

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

#[tokio::test]
#[ignore]
async fn rem_first_delay_defaults_to_five_minutes() {
    let pool = pool().await;
    let _owns = own_the_rem_first_delay_env(&pool).await;
    unsafe { std::env::remove_var("CUBA_REM_FIRST_DELAY_SECS") };

    let delay = memory_industry::protocol::rem_first_delay();

    assert_eq!(
        delay,
        std::time::Duration::from_secs(300),
        "with no CUBA_REM_FIRST_DELAY_SECS the daemon must wait five minutes for its first REM \
         cycle, not the 4-hour REM_INTERVAL it used to sleep before consolidation ever ran once"
    );
}

#[tokio::test]
#[ignore]
async fn cuba_rem_first_delay_secs_overrides_the_default() {
    let pool = pool().await;
    let _owns = own_the_rem_first_delay_env(&pool).await;
    unsafe { std::env::set_var("CUBA_REM_FIRST_DELAY_SECS", "7") };

    let delay = memory_industry::protocol::rem_first_delay();

    unsafe { std::env::remove_var("CUBA_REM_FIRST_DELAY_SECS") };

    assert_eq!(
        delay,
        std::time::Duration::from_secs(7),
        "CUBA_REM_FIRST_DELAY_SECS must override the five-minute default, the same pattern \
         CUBA_REM_RELATION_BATCH already follows in this file's neighbour"
    );
}

#[tokio::test]
#[ignore]
async fn the_daemon_decays_a_stale_observation_within_seconds_not_within_four_hours() {
    let pool = pool().await;
    let _owns = own_the_rem_first_delay_env(&pool).await;
    unsafe { std::env::set_var("CUBA_REM_FIRST_DELAY_SECS", "1") };
    unsafe { std::env::set_var("CUBA_REM_RELATION_BATCH", "0") };

    let name = format!("RemFirstCycle_{}", &Uuid::new_v4().to_string()[..8]);
    let entity: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'project') RETURNING id",
    )
    .bind(&name)
    .fetch_one(&pool)
    .await
    .expect("creating the fixture entity");

    let observation: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_observations
            (entity_id, content, observation_type, source, last_accessed, last_decayed_at)
         VALUES ($1, 'marca de ciclo REM', 'fact', 'agent',
                 NOW() - INTERVAL '10 days', NOW() - INTERVAL '10 days')
         RETURNING id",
    )
    .bind(entity.0)
    .fetch_one(&pool)
    .await
    .expect("creating the fixture observation");

    let daemon_pool = pool.clone();
    let daemon = tokio::spawn(memory_industry::protocol::rem_daemon(daemon_pool));

    let cycled = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let stamp: (chrono::DateTime<chrono::Utc>,) =
                sqlx::query_as("SELECT last_decayed_at FROM brain_observations WHERE id = $1")
                    .bind(observation.0)
                    .fetch_one(&pool)
                    .await
                    .expect("reading the decay stamp");
            if stamp.0 > chrono::Utc::now() - chrono::Duration::minutes(1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    })
    .await;

    daemon.abort();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity.0)
        .execute(&pool)
        .await
        .ok();
    unsafe { std::env::remove_var("CUBA_REM_FIRST_DELAY_SECS") };
    unsafe { std::env::remove_var("CUBA_REM_RELATION_BATCH") };

    assert!(
        cycled.is_ok(),
        "with CUBA_REM_FIRST_DELAY_SECS=1 the daemon's first REM cycle must stamp this 10-day-old \
         observation's last_decayed_at within 30 seconds. REM_INTERVAL is 4 hours, so a 30s \
         bound only ever passes if rem_daemon sleeps CUBA_REM_FIRST_DELAY_SECS and runs a cycle \
         BEFORE its first tokio::time::interval tick — the old order was \
         interval.tick().await (resolves instantly, tokio fires the first tick right away) \
         followed by a second interval.tick().await inside the loop, which waits the full \
         4-hour period before consolidation ever ran once. Adelantar el ciclo no daña los \
         datos: el decay usa EXP(-0.693 * (NOW() - GREATEST(last_accessed, last_decayed_at)) \
         / 86400), o sea decae por tiempo transcurrido, no por número de ejecuciones"
    );
}
