use serde_json::Value;
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the result is JSON inside that envelope")
}

#[tokio::test]
#[ignore]
async fn an_entity_both_machines_invented_separately_does_not_lose_the_whole_bundle() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-name-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let shared_name = format!("Postgres_{}", &Uuid::new_v4().to_string()[..8]);
    let theirs = Uuid::new_v4();
    let mine = Uuid::new_v4();

    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'technology')")
        .bind(mine)
        .bind(&shared_name)
        .execute(&pool)
        .await
        .expect("this machine already had its own idea of that entity");

    let entities_dir = bundle.join("entities");
    std::fs::create_dir_all(&entities_dir).expect("entities dir");
    let obs_id = Uuid::new_v4();
    let payload = serde_json::json!({
        "id": theirs,
        "name": shared_name,
        "entity_type": "technology",
        "importance": 0.5,
        "access_count": 0,
        "project_id": null,
        "created_at": "2026-08-01T00:00:00Z",
        "observations": [{
            "id": obs_id,
            "content": "lo que solo sabe la otra maquina",
            "observation_type": "fact",
            "source": "agent",
            "importance": 0.5,
            "tags": [],
            "project_id": null,
            "session_id": null,
            "created_at": "2026-08-01T00:00:00Z",
            "embedding_model": null
        }]
    });
    std::fs::write(
        entities_dir.join("theirs.json"),
        serde_json::to_vec_pretty(&payload).expect("serialise"),
    )
    .expect("write the incoming entity file");
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "manifest_hash": Uuid::new_v4().to_string(),
            "project_id": null,
            "project_name": null,
            "exported_at": "2026-08-01T00:00:00Z",
            "counts": {"entities": 1, "observations": 1, "episodes": 0, "decisions": 0, "errors": 0, "relations": 0},
            "with_embeddings": false
        }))
        .expect("serialise"),
    )
    .expect("write the manifest");

    let imported = call(
        &pool,
        "cuba_sync",
        serde_json::json!({"action": "import", "dir": bundle.display().to_string()}),
    )
    .await;

    assert!(
        imported["rows_inserted"].as_u64().unwrap_or(0) >= 1,
        "brain_entities has UNIQUE (name) and the import used ON CONFLICT (id), so two \
         machines that each invented `Postgres` on their own produced a unique_violation \
         that took the whole transaction down — measured before this change: the import \
         died and zero observations arrived, losing every other row in the bundle too. \
         Got: {imported}"
    );

    let named: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE name = $1")
        .bind(&shared_name)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        named, 1,
        "and it must not have created a second row for the same name — the incoming id is \
         remapped onto the one that was already here"
    );

    let parent: Uuid = sqlx::query_scalar("SELECT entity_id FROM brain_observations WHERE id = $1")
        .bind(obs_id)
        .fetch_one(&pool)
        .await
        .expect("the observation arrived");
    assert_eq!(
        parent, mine,
        "the observation has to hang off the local entity, not off an id that does not exist \
         here. Reparenting is the whole point: without it the row is orphaned or the FK \
         rejects it"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(mine)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
