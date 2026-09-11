use std::time::Duration;
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

#[tokio::test]
#[ignore]
async fn a_second_concurrent_rem_cycle_does_no_work_and_returns_quickly() {
    let pool = pool().await;

    let mut holder = pool
        .acquire()
        .await
        .expect("a connection to hold the REM lock on, the way a first cycle would");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(memory_industry::protocol::REM_LOCK)
        .execute(&mut *holder)
        .await
        .expect("take the REM lock the way a first, still-running cycle would");

    let name = unique_name("RemLockFixture");
    let entity: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&name)
    .fetch_one(&pool)
    .await
    .expect("creating the fixture entity");

    let cycled = tokio::time::timeout(
        Duration::from_secs(5),
        memory_industry::protocol::run_rem_consolidation(&pool),
    )
    .await;

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(memory_industry::protocol::REM_LOCK)
        .execute(&mut *holder)
        .await
        .ok();
    drop(holder);

    let after: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT community_id FROM brain_node_metrics WHERE node_id = $1")
            .bind(entity.0)
            .fetch_optional(&pool)
            .await
            .expect("reading community after");

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity.0)
        .execute(&pool)
        .await
        .ok();

    let cycled = cycled.expect(
        "a cycle that loses the lock race must return within 5s instead of waiting for the \
         lock — two REM daemons (one per connected client) plus `cuba-memorys rem` run by \
         hand must never queue up behind each other",
    );
    assert!(
        cycled.is_ok(),
        "a cycle that loses the lock race is not a failure of the cycle itself, it must exit \
         clean: {cycled:?}"
    );
    assert!(
        after.is_none(),
        "a REM cycle did its consolidation work — including community detection, which wipes \
         and rebuilds brain_communities inside a transaction — while another cycle was known \
         to be holding the lock. Two overlapping cycles racing that table, or claiming the \
         same auto-extraction and relation-scan rows twice, is exactly what this lock exists \
         to prevent"
    );
}
