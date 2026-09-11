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
async fn a_search_without_an_embedding_model_says_it_is_degraded() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let model_loaded = memory_industry::embeddings::onnx::is_model_loaded();

    let marker = format!("v030nomodel{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
        .bind(entity_id)
        .bind(format!("{marker} contenido de prueba para B2"))
        .execute(&pool)
        .await
        .expect("seed the observation");

    let response = call(&pool, "cuba_faro", json!({"query": marker, "limit": 5})).await;

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&pool)
        .await
        .ok();

    if model_loaded {
        assert_eq!(
            response.get("degraded").and_then(Value::as_bool),
            None,
            "the embedding model is loaded, so the vector branch ran and nothing is degraded. \
             Marking a healthy search would be the mirror of the bug this file exists for, and \
             the gate runs with the model present, so this is the half it can prove. What this \
             run does NOT establish: that a search WITHOUT a model says so — for that, run it \
             with ONNX_MODEL_PATH=\"\". Got: {response}"
        );
        return;
    }

    assert_eq!(
        response.get("degraded").and_then(Value::as_bool),
        Some(true),
        "no embedding model is loaded, so the vector branch contributed nothing to this search; \
         the response has to say so with degraded=true instead of looking like a healthy search \
         that merely found lexical matches. vector_search used to return Ok(vec![]) here, which \
         annotate_degradation never sees. Got: {response}"
    );
    let reason = response
        .get("degraded_reason")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("degraded_reason must be present and readable: {response}"));
    assert!(
        !reason.trim().is_empty(),
        "degraded_reason must explain what was lost, not just flag it: {response}"
    );
}
