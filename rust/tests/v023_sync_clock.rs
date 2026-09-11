use uuid::Uuid;

#[tokio::test]
#[ignore]
async fn the_clock_ticks_for_changes_a_peer_cares_about_and_not_for_local_telemetry() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let entity = Uuid::new_v4();
    let obs = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity)
        .bind(format!("clock_{}", &entity.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (id, entity_id, content) VALUES ($1, $2, 'original')",
    )
    .bind(obs)
    .bind(entity)
    .execute(&pool)
    .await
    .expect("seed the observation");

    async fn version(pool: &sqlx::PgPool, id: Uuid) -> i32 {
        sqlx::query_scalar("SELECT version FROM brain_observations WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("read the version")
    }

    let start = version(&pool, obs).await;

    sqlx::query(
        "UPDATE brain_observations SET importance = 0.9, last_accessed = NOW() WHERE id = $1",
    )
    .bind(obs)
    .execute(&pool)
    .await
    .expect("a decay pass");
    assert_eq!(
        version(&pool, obs).await,
        start,
        "the REM decay pass writes importance and last_accessed on 1097 of 1880 rows every \
         four hours. If that ticked the sync clock, every export would ship a graph that had \
         not changed and the two machines would never stop talking to each other about nothing"
    );

    sqlx::query("UPDATE brain_observations SET embedding_model = 'something-else' WHERE id = $1")
        .bind(obs)
        .execute(&pool)
        .await
        .expect("a reembed pass");
    assert_eq!(
        version(&pool, obs).await,
        start,
        "reembedding replaces a vector, not a fact. The other machine has nothing to learn"
    );

    sqlx::query("UPDATE brain_observations SET content = 'corrected' WHERE id = $1")
        .bind(obs)
        .execute(&pool)
        .await
        .expect("a real change");
    let after_change = version(&pool, obs).await;
    assert_eq!(
        after_change,
        start + 1,
        "changing the content is exactly what the other machine needs to hear about"
    );

    sqlx::query("UPDATE brain_observations SET content = 'corrected' WHERE id = $1")
        .bind(obs)
        .execute(&pool)
        .await
        .expect("writing the same value again");
    assert_eq!(
        version(&pool, obs).await,
        after_change,
        "writing the same value is not a change. Without IS DISTINCT FROM, an idempotent \
         re-import would tick the clock on every row it touched and invent a conflict out of \
         agreement"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn correcting_through_the_handler_advances_the_clock_exactly_once() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let entity = Uuid::new_v4();
    let obs = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity)
        .bind(format!("clock2_{}", &entity.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (id, entity_id, content) VALUES ($1, $2, 'before')",
    )
    .bind(obs)
    .bind(entity)
    .execute(&pool)
    .await
    .expect("seed the observation");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_eco",
        serde_json::json!({
            "action": "correct",
            "observation_id": obs.to_string(),
            "correction": "after"
        }),
    )
    .await
    .expect("correct");

    let version: i32 = sqlx::query_scalar("SELECT version FROM brain_observations WHERE id = $1")
        .bind(obs)
        .fetch_one(&pool)
        .await
        .expect("read the version");
    assert_eq!(
        version, 2,
        "eco.rs already writes version = version + 1 by hand, and now a trigger writes it too. \
         Both compute OLD.version + 1, so they agree — but if either ever stops agreeing the \
         clock jumps by two and every comparison against a peer's version is off by one for \
         the rest of that row's life"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
}
