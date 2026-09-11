use serde_json::json;
use uuid::Uuid;

fn unique_name(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
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
async fn quarantined_memories_are_withheld_from_search_until_promoted() {
    let pool = pool().await;
    let entity = unique_name("quar_entity");
    let marker = unique_name("zzmarker");
    let content = format!("The quarantine canary phrase is {marker}");

    memory_industry::handlers::cronica::handle(
        &pool,
        json!({
            "action": "add",
            "entity_name": entity,
            "content": content,
            "observation_type": "fact",
            "source": "inference",
            "trust": "quarantined"
        }),
    )
    .await
    .expect("adding a quarantined observation");

    let stored: (Uuid, String) = sqlx::query_as(
        "SELECT o.id, o.trust FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE e.name = $1",
    )
    .bind(&entity)
    .fetch_one(&pool)
    .await
    .expect("the observation must be stored even while quarantined");
    assert_eq!(stored.1, "quarantined");

    let found = memory_industry::handlers::faro::handle(
        &pool,
        json!({ "query": marker, "limit": 20, "format": "verbose" }),
    )
    .await
    .expect("searching");
    let quarantined_id = stored.0.to_string();
    let surfaced: Vec<String> = found
        .get("results")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !surfaced.contains(&quarantined_id),
        "a quarantined memory must not surface in search; results were {surfaced:?}"
    );

    let listed = memory_industry::handlers::eco::handle(&pool, json!({ "action": "pending" }))
        .await
        .expect("listing pending");
    assert!(
        serde_json::to_string(&listed)
            .unwrap_or_default()
            .contains(&marker),
        "a quarantined memory must be visible for review"
    );

    memory_industry::handlers::eco::handle(
        &pool,
        json!({ "action": "promote", "observation_id": stored.0.to_string() }),
    )
    .await
    .expect("promoting");

    let after: String = sqlx::query_scalar("SELECT trust FROM brain_observations WHERE id = $1")
        .bind(stored.0)
        .fetch_one(&pool)
        .await
        .expect("reading trust back");
    assert_eq!(after, "trusted", "promotion must flip the row to trusted");

    let found_after = memory_industry::handlers::faro::handle(
        &pool,
        json!({ "query": marker, "limit": 20, "format": "verbose" }),
    )
    .await
    .expect("searching after promotion");
    let surfaced_after: Vec<String> = found_after
        .get("results")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        surfaced_after.contains(&quarantined_id),
        "once promoted the memory must become retrievable, otherwise the gate is a one-way trap"
    );

    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&entity)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn a_trusted_write_is_retrievable_as_before() {
    let pool = pool().await;
    let entity = unique_name("trust_entity");
    let marker = unique_name("yymarker");

    memory_industry::handlers::cronica::handle(
        &pool,
        json!({
            "action": "add",
            "entity_name": entity,
            "content": format!("The trusted canary phrase is {marker}"),
            "observation_type": "fact",
            "source": "agent"
        }),
    )
    .await
    .expect("adding a normal observation");

    let trust: String = sqlx::query_scalar(
        "SELECT o.trust FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id WHERE e.name = $1",
    )
    .bind(&entity)
    .fetch_one(&pool)
    .await
    .expect("reading trust");
    assert_eq!(
        trust, "trusted",
        "an ordinary agent write must stay retrievable — the gate must not change default behaviour"
    );

    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&entity)
        .execute(&pool)
        .await
        .ok();
}
