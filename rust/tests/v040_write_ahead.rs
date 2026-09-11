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

async fn cleanup(pool: &sqlx::PgPool, entity: &str) {
    let _ = sqlx::query(
        "DELETE FROM brain_observation_chunks WHERE observation_id IN (
            SELECT o.id FROM brain_observations o
            JOIN brain_entities e ON e.id = o.entity_id
            WHERE e.name = $1)",
    )
    .bind(entity)
    .execute(pool)
    .await;
    let _ = sqlx::query(
        "DELETE FROM brain_observations WHERE entity_id IN (SELECT id FROM brain_entities WHERE name = $1)",
    )
    .bind(entity)
    .execute(pool)
    .await;
    let _ = sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(entity)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore]
async fn add_returns_only_after_embedding_is_written_or_explicitly_pending() {
    unsafe { std::env::set_var("MEMORY_INDUSTRY_GRAPH_DB", "off") };
    let pool = pool().await;
    let entity = unique_name("write_through");
    let content = format!("write-through probe {entity}: puerto 5488 es el SoT de Postgres");

    let added = memory_industry::handlers::cronica::handle(
        &pool,
        json!({
            "action": "add",
            "entity_name": entity,
            "content": content,
            "observation_type": "fact"
        }),
    )
    .await
    .expect("add");

    let id = added
        .get("id")
        .and_then(|v| v.as_str())
        .expect("add returns id");
    let status = added
        .get("embedding")
        .and_then(|v| v.as_str())
        .expect("add must declare embedding ready|pending");
    assert!(
        added
            .get("write_to_searchable_ms")
            .and_then(|v| v.as_u64())
            .is_some(),
        "add must expose write_to_searchable_ms: {added}"
    );

    let has_vec: bool =
        sqlx::query_scalar("SELECT embedding IS NOT NULL FROM brain_observations WHERE id = $1")
            .bind(Uuid::parse_str(id).expect("uuid"))
            .fetch_one(&pool)
            .await
            .expect("select embedding");

    if memory_industry::embeddings::onnx::is_model_loaded() {
        assert_eq!(status, "ready");
        assert!(
            has_vec,
            "ONNX loaded: embedding must be Some before the JSON returns"
        );
    } else {
        assert_eq!(status, "pending");
        assert!(
            !has_vec,
            "without ONNX the row persists but vector stays null"
        );
    }

    cleanup(&pool, &entity).await;
}

#[tokio::test]
#[ignore]
async fn batch_add_of_long_text_writes_chunks_before_return() {
    unsafe { std::env::set_var("MEMORY_INDUSTRY_GRAPH_DB", "off") };
    let pool = pool().await;
    let entity = unique_name("batch_chunk");
    let content = format!(
        "{} unique-tail-{entity}",
        "Long industrial note. ".repeat(200)
    );
    assert!(memory_industry::embeddings::chunk::needs_chunking(&content));

    let result = memory_industry::handlers::cronica::handle(
        &pool,
        json!({
            "action": "batch_add",
            "observations": [{
                "entity_name": entity,
                "content": content,
                "observation_type": "fact"
            }]
        }),
    )
    .await
    .expect("batch_add");

    assert_eq!(result.get("added").and_then(|v| v.as_u64()), Some(1));

    let obs_id: Uuid = sqlx::query_scalar(
        "SELECT o.id FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE e.name = $1
         ORDER BY o.created_at DESC LIMIT 1",
    )
    .bind(&entity)
    .fetch_one(&pool)
    .await
    .expect("inserted obs");

    let chunks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_observation_chunks WHERE observation_id = $1",
    )
    .bind(obs_id)
    .fetch_one(&pool)
    .await
    .expect("count chunks");

    if memory_industry::embeddings::onnx::is_model_loaded() {
        assert!(
            chunks > 0,
            "batch_add must persist chunks before returning when ONNX is loaded; chunks={chunks}"
        );
        assert!(
            result.get("chunks").and_then(|v| v.as_u64()).unwrap_or(0) > 0,
            "measurable chunks flag: {result}"
        );
    }

    cleanup(&pool, &entity).await;
}

#[tokio::test]
#[ignore]
async fn contexto_recall_includes_entity_from_this_session_write() {
    unsafe { std::env::set_var("MEMORY_INDUSTRY_GRAPH_DB", "off") };
    let pool = pool().await;
    let entity = unique_name("session_prefetch");
    let sid = Uuid::new_v4();
    memory_industry::session::clear();
    memory_industry::session::set(sid, None);

    memory_industry::handlers::cronica::handle(
        &pool,
        json!({
            "action": "add",
            "entity_name": entity,
            "content": format!("{entity} acaba de guardarse en esta sesion"),
            "observation_type": "fact"
        }),
    )
    .await
    .expect("add");

    let ctx = memory_industry::handlers::contexto::handle(&pool, json!({"budget_chars": 8000}))
        .await
        .expect("contexto");

    let recall = ctx
        .pointer("/layers/top_recall")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        recall
            .iter()
            .any(|row| row.get("entity").and_then(|v| v.as_str()) == Some(entity.as_str())),
        "cuba_contexto must recall the entity of the last write in this session: {ctx}"
    );

    let tiny = memory_industry::handlers::contexto::handle(&pool, json!({"budget_chars": 1000}))
        .await
        .expect("tiny context");
    let encoded = serde_json::to_string(&tiny).expect("json");
    assert!(
        encoded.len() <= 1000 + 400,
        "memory://context / cuba_contexto must respect budget_chars, got {}",
        encoded.len()
    );

    memory_industry::session::clear();
    cleanup(&pool, &entity).await;
}
