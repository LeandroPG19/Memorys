use serde_json::{Value, json};
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

#[tokio::test]
#[ignore]
async fn a_search_marks_what_the_other_agent_wrote_after_you_started() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("nuevo_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, created_at)
         VALUES ($1, $2, NOW() - interval '2 hours')",
    )
    .bind(entity_id)
    .bind(format!("{marker} lo que ya estaba cuando llegaste"))
    .execute(&pool)
    .await
    .expect("seed the old observation");

    memory_industry::session::clear();
    let without = call(&pool, "cuba_faro", json!({"query": marker, "limit": 10})).await;
    assert!(
        without.get("new_since_you_started").is_none(),
        "with no session open there is no reference point, so the field must be absent \
         entirely — not an empty array, which would say «I looked and found none». Most \
         searches happen without cuba_jornada, so this is the common case and it has to cost \
         zero tokens. Got: {without}"
    );

    let started = call(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": format!("sesion de {marker}")}),
    )
    .await;
    let session = started["session"]["id"]
        .as_str()
        .expect("jornada start returns the session")
        .to_string();

    let quiet = call(&pool, "cuba_faro", json!({"query": marker, "limit": 10})).await;
    assert!(
        quiet.get("new_since_you_started").is_none(),
        "nothing was written after the session opened, so still no field: the annotation has \
         to earn its tokens on every one of the four hundred searches this server answers in \
         ten days. Got: {quiet}"
    );

    sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
        .bind(entity_id)
        .bind(format!("{marker} lo que la otra IA acaba de escribir"))
        .execute(&pool)
        .await
        .expect("the other agent writes while this session is open");

    let after = call(&pool, "cuba_faro", json!({"query": marker, "limit": 10})).await;
    let fresh = after["new_since_you_started"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "another agent wrote to this project while the session was open and the search \
                 did not say so. That is the whole point: two AIs share one database, each sees \
                 the other's writes on the next query, and neither has any way to know which \
                 results are new to it. Got: {after}"
            )
        })
        .clone();

    assert!(
        !fresh.is_empty(),
        "the array exists but is empty, which means the field was added without the check \
         behind it: {after}"
    );
    let results = after["results"].as_array().expect("results");
    for at in &fresh {
        let at = at.as_u64().expect("an index") as usize;
        let row = results
            .get(at)
            .unwrap_or_else(|| panic!("index {at} is past the {} results returned", results.len()));
        let text = serde_json::to_string(row).expect("serialise");
        assert!(
            text.contains("acaba de escribir"),
            "index {at} points at a result that was NOT written after the session opened — the \
             indices have to line up with the array they describe, or the model reads the wrong \
             row and trusts it. Got: {row}"
        );
    }

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_sessions WHERE id = $1::uuid")
        .bind(&session)
        .execute(&pool)
        .await
        .ok();
    memory_industry::session::clear();
}
