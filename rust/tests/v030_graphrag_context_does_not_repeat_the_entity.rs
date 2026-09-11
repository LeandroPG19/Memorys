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
async fn one_entity_with_several_matching_observations_gets_one_graphrag_entry() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("v030hub{}", &Uuid::new_v4().to_string()[..8]);
    let hub_id = Uuid::new_v4();
    let neighbor_id = Uuid::new_v4();
    let neighbor_name = format!("vecino_estable_{}", &Uuid::new_v4().to_string()[..8]);

    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(hub_id)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed the hub entity");
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(neighbor_id)
        .bind(&neighbor_name)
        .execute(&pool)
        .await
        .expect("seed the neighbor entity");
    sqlx::query(
        "INSERT INTO brain_relations (from_entity, to_entity, relation_type, strength)
         VALUES ($1, $2, 'related_to', 0.9)",
    )
    .bind(hub_id)
    .bind(neighbor_id)
    .execute(&pool)
    .await
    .expect("seed the relation");

    for extra in [
        "alfa flamenco tigre",
        "beta canario zorro",
        "gamma delfin lince",
    ] {
        sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
            .bind(hub_id)
            .bind(format!("{marker} {extra}"))
            .execute(&pool)
            .await
            .expect("seed a distinct observation under the hub entity");
    }

    let response = call(&pool, "cuba_faro", json!({"query": marker, "limit": 5})).await;

    sqlx::query("DELETE FROM brain_relations WHERE from_entity = $1 OR to_entity = $1")
        .bind(hub_id)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = ANY($1)")
        .bind(vec![hub_id, neighbor_id])
        .execute(&pool)
        .await
        .ok();

    let context = response["graphrag_context"]
        .as_array()
        .unwrap_or_else(|| panic!("graphrag_context must be an array: {response}"));

    let hub_entries: Vec<&Value> = context
        .iter()
        .filter(|entry| entry.get("entity").and_then(Value::as_str) == Some(marker.as_str()))
        .collect();

    assert_eq!(
        hub_entries.len(),
        1,
        "three observations of the SAME entity landed in the top results, so the entity must \
         appear once in graphrag_context, not once per result: repeating it wastes the tokens \
         the caller pays for context and teaches the model the same fact is three facts. \
         Got {} entries: {context:?}",
        hub_entries.len()
    );

    let neighbors = hub_entries[0]["neighbors"]
        .as_array()
        .unwrap_or_else(|| panic!("the hub entity must carry its neighbors: {context:?}"));
    assert!(
        neighbors
            .iter()
            .any(|n| n.get("name").and_then(Value::as_str) == Some(neighbor_name.as_str())),
        "the one seeded relation must resolve to the seeded neighbor: {context:?}"
    );
}
