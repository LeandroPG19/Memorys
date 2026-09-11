use serde_json::{Value, json};
use uuid::Uuid;

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0025;

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
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the result is JSON inside that envelope")
}

#[tokio::test]
#[ignore]
async fn editing_the_bundle_by_hand_is_not_silently_discarded() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-edit-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity = Uuid::new_v4();
    let obs = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity)
        .bind(format!("edited_{}", &entity.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (id, entity_id, content) VALUES ($1, $2, 'as exported')",
    )
    .bind(obs)
    .bind(entity)
    .execute(&pool)
    .await
    .expect("seed the observation");

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    let first = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string()}),
    )
    .await;
    assert!(
        first["skipped"].as_bool() != Some(true),
        "the first import of a fresh bundle must do work: {first}"
    );

    let again = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string()}),
    )
    .await;
    assert_eq!(
        again["skipped"].as_bool(),
        Some(true),
        "importing the identical files twice still has to be a no-op, or every checkout \
         re-does the whole bundle: {again}"
    );

    for entry in std::fs::read_dir(bundle.join("entities"))
        .expect("entities dir")
        .flatten()
    {
        let path = entry.path();
        let Ok(mut file) = serde_json::from_slice::<Value>(&std::fs::read(&path).expect("read"))
        else {
            continue;
        };
        if file.get("id").and_then(Value::as_str) != Some(&entity.to_string()) {
            continue;
        }
        for o in file["observations"].as_array_mut().expect("observations") {
            o["content"] = Value::String("edited by hand in the git repo".into());
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&file).expect("serialise")).expect("write");
    }

    let after_edit = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "overwrite"}),
    )
    .await;
    assert!(
        after_edit["skipped"].as_bool() != Some(true),
        "the import deduplicated on the hash the manifest declares about itself, so editing a \
         file without re-exporting left that hash unchanged and the whole bundle was answered \
         with `skipped: true`. Editing the JSON in the git repo is the flow this format exists \
         for, and it was the one flow that silently did nothing. Got: {after_edit}"
    );
    assert_eq!(
        after_edit["edited_since_export"].as_bool(),
        Some(true),
        "and it has to say that the files no longer match what the manifest claims — not to \
         refuse them, but so nobody has to wonder: {after_edit}"
    );

    let stored: String = sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
        .bind(obs)
        .fetch_one(&pool)
        .await
        .expect("read back");
    assert_eq!(stored, "edited by hand in the git repo");

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
