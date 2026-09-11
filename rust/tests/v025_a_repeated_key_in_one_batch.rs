use serde_json::{Value, json};
use uuid::Uuid;

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0027;

async fn own_the_sync_dir(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_SYNC_DIR is process-global");
    tx
}

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

#[tokio::test]
#[ignore]
async fn the_same_row_twice_in_one_bundle_does_not_abort_the_import() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-dup-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(bundle.join("entities")).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let marker = format!("dup_{}", &Uuid::new_v4().to_string()[..8]);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let shared_obs = Uuid::new_v4();
    let now = chrono::Utc::now().to_rfc3339();

    for (n, id) in [(1, a), (2, b)] {
        let file = json!({
            "id": id,
            "name": format!("{marker}_{n}"),
            "entity_type": "concept",
            "importance": 0.5,
            "access_count": 0,
            "project_id": null,
            "created_at": now,
            "observations": [{
                "id": shared_obs,
                "content": format!("{marker} la misma observacion en dos ficheros"),
                "observation_type": "fact",
                "source": "agent",
                "importance": 0.5,
                "tags": ["sync"],
                "project_id": null,
                "session_id": null,
                "created_at": now,
                "embedding_model": null,
            }],
        });
        std::fs::write(
            bundle.join("entities").join(format!("{marker}-{n}.json")),
            serde_json::to_vec_pretty(&file).expect("serialise"),
        )
        .expect("write the entity file");
    }

    let rel = json!([
        {"id": Uuid::new_v4(), "from_entity": a, "to_entity": b, "relation_type": "relates_to",
         "strength": 0.4, "bidirectional": false, "project_id": null, "created_at": now,
         "provenance": "extracted"},
        {"id": Uuid::new_v4(), "from_entity": a, "to_entity": b, "relation_type": "relates_to",
         "strength": 0.9, "bidirectional": false, "project_id": null, "created_at": now,
         "provenance": "extracted"}
    ]);
    std::fs::write(
        bundle.join("relations.json"),
        serde_json::to_vec_pretty(&rel).expect("serialise"),
    )
    .expect("write relations");
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": memory_industry::sync::chunk::SCHEMA_VERSION,
            "manifest_hash": format!("dup{}", &Uuid::new_v4().to_string()[..12]),
            "exported_at": now,
            "project_id": null,
            "project_name": null,
            "with_embeddings": false,
            "counts": {"entities": 2, "observations": 1, "episodes": 0, "decisions": 0, "errors": 0, "relations": 2},
        }))
        .expect("serialise"),
    )
    .expect("write the manifest");

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "overwrite"}),
    )
    .await;
    assert!(
        imported["rows_inserted"].as_u64().unwrap_or(0) > 0,
        "batching is what makes this a failure mode: row at a time, the second copy of a row was \
         a harmless no-op, but one INSERT ... ON CONFLICT DO UPDATE that touches the same key \
         twice makes PostgreSQL abort the whole statement with 'cannot affect row a second \
         time'. The import has to dedupe before it batches. Got: {imported}"
    );

    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE id = $1")
        .bind(shared_obs)
        .fetch_one(&pool)
        .await
        .expect("count the observation");
    assert_eq!(
        stored, 1,
        "and the row still has to be there once: deduping must drop the duplicate, not the row"
    );

    let strength: f64 = sqlx::query_scalar(
        "SELECT strength FROM brain_relations
         WHERE from_entity = $1 AND to_entity = $2 AND relation_type = 'relates_to'",
    )
    .bind(a)
    .bind(b)
    .fetch_one(&pool)
    .await
    .expect("read the relation back");
    assert!(
        (strength - 0.4).abs() < 1e-9,
        "the first copy of a repeated natural key wins, which is arbitrary but has to be stated \
         and fixed: silently keeping whichever one the file order happened to put last is how \
         two runs over the same bundle end up disagreeing. Got strength {strength}"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = ANY($1)")
        .bind(vec![a, b])
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
