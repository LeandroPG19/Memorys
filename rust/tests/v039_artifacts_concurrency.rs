use serde_json::{Value, json};
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Result<Value, String> {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let text = envelope["content"][0]["text"]
        .as_str()
        .ok_or_else(|| "envelope".to_string())?;
    serde_json::from_str(text).map_err(|e| e.to_string())
}

#[tokio::test]
#[ignore]
async fn two_clients_cannot_silently_overwrite_an_artifact() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let pool = memory_industry::db::create_pool(&url).await.expect("pool");
    let path = format!("e2e/plan_{}.md", &Uuid::new_v4().to_string()[..8]);

    let first = memory_industry::session::with_client("agent-a".into(), async {
        call(
            &pool,
            "cuba_artefacto",
            json!({"action": "put", "path": &path, "content": "draft-a", "base_version": 0}),
        )
        .await
    })
    .await
    .expect("agent-a create");
    assert_eq!(first["version"], 1);

    let conflict = memory_industry::session::with_client("agent-b".into(), async {
        call(
            &pool,
            "cuba_artefacto",
            json!({"action": "put", "path": &path, "content": "draft-b", "base_version": 0}),
        )
        .await
    })
    .await;
    assert!(
        conflict.is_err(),
        "stale base_version must refuse, got {conflict:?}"
    );

    let ok = memory_industry::session::with_client("agent-b".into(), async {
        call(
            &pool,
            "cuba_artefacto",
            json!({"action": "put", "path": &path, "content": "draft-b", "base_version": 1}),
        )
        .await
    })
    .await
    .expect("agent-b with correct base");
    assert_eq!(ok["version"], 2);

    let who = call(&pool, "cuba_whoami", json!({})).await.expect("whoami");
    assert_eq!(who["server"], "MemoryIndustry");

    let ctx = call(&pool, "memory_context", json!({"budget_chars": 4000}))
        .await
        .expect("alias context");
    assert!(ctx["layers"].is_object() || ctx["truncated"] == true);
}
