use serde_json::{Value, json};
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

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0024;

async fn own_the_sync_dir(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect(
            "CUBA_SYNC_DIR is process-global: two tests in one binary that both set it \
                 end up exporting into each other's directory",
        );
    tx
}

#[tokio::test]
#[ignore]
async fn what_the_conflict_rules_compare_actually_travels() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-clockb-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns_the_dir = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity = Uuid::new_v4();
    let obs = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity)
        .bind(format!("clockb_{}", &entity.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (id, entity_id, content, evidence, verification, origin_node)
         VALUES ($1, $2, 'parsed from a real syntax tree', 'observed', 'tree-sitter parse of x.rs', 'the-other-machine')",
    )
    .bind(obs)
    .bind(entity)
    .execute(&pool)
    .await
    .expect("seed an observation that is more than asserted");
    sqlx::query("UPDATE brain_observations SET content = 'corrected once' WHERE id = $1")
        .bind(obs)
        .execute(&pool)
        .await
        .expect("advance its clock");

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let mut found = None;
    for entry in std::fs::read_dir(bundle.join("entities"))
        .expect("entities dir")
        .flatten()
    {
        let file: Value =
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read")).expect("parse");
        if file.get("id").and_then(Value::as_str) != Some(&entity.to_string()) {
            continue;
        }
        found = file["observations"]
            .as_array()
            .and_then(|o| o.first().cloned());
    }
    let row = found.expect("the exported entity carries its observation");

    assert_eq!(
        row["version"].as_i64(),
        Some(2),
        "every conflict rule in this design ends with comparing versions, and the version did \
         not leave the machine. Whatever the clock says here, the peer received nothing and \
         had to guess. Got: {row}"
    );
    assert!(
        row["updated_at"].is_string(),
        "same for updated_at, which is the tiebreak when versions match: {row}"
    );
    assert_eq!(
        row["origin_node"].as_str(),
        Some("the-other-machine"),
        "origin_node is filled in on every machine and was dropped at the door. The column \
         that exists to answer «where was this decided» said the importing machine's name for \
         every row that crossed: {row}"
    );
    assert_eq!(
        row["evidence"].as_str(),
        Some("observed"),
        "migration 0044 separates what a model asserted from what tree-sitter parsed out of a \
         real AST. Without this field in the bundle that distinction is erased on the first \
         trip — everything arrives as asserted: {row}"
    );
    assert_eq!(
        row["verification"].as_str(),
        Some("tree-sitter parse of x.rs"),
        "and a level with no record of what supports it is exactly the unfalsifiable claim \
         0044 refuses to store: {row}"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn a_bundle_written_before_any_of_this_still_imports() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-v1-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns_the_dir = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(bundle.join("entities")).expect("entities dir");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity = Uuid::new_v4();
    let obs = Uuid::new_v4();
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manifest_hash": Uuid::new_v4().to_string(),
            "project_id": null, "project_name": null,
            "exported_at": "2026-07-01T00:00:00Z",
            "counts": {"entities": 1, "observations": 1, "episodes": 0, "decisions": 0, "errors": 0, "relations": 0},
            "with_embeddings": false
        }))
        .expect("serialise"),
    )
    .expect("manifest");
    std::fs::write(
        bundle.join("entities").join("old.json"),
        serde_json::to_vec_pretty(&json!({
            "id": entity, "name": format!("v1_{}", &entity.to_string()[..8]),
            "entity_type": "concept", "importance": 0.5, "access_count": 0,
            "project_id": null, "created_at": "2026-07-01T00:00:00Z",
            "observations": [{
                "id": obs, "content": "written before any of these columns existed",
                "observation_type": "fact", "source": "agent", "importance": 0.5,
                "tags": [], "project_id": null, "session_id": null,
                "created_at": "2026-07-01T00:00:00Z", "embedding_model": null
            }]
        }))
        .expect("serialise"),
    )
    .expect("entity file");

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string()}),
    )
    .await;
    assert!(
        imported["rows_inserted"].as_u64().unwrap_or(0) >= 1,
        "bumping SCHEMA_VERSION must not orphan the bundles already sitting in people's git \
         repositories. Every new field is #[serde(default)] and every insert COALESCEs, so a \
         v1 file lands with the defaults the database would have given it anyway. Got: {imported}"
    );
    let evidence: String =
        sqlx::query_scalar("SELECT evidence FROM brain_observations WHERE id = $1")
            .bind(obs)
            .fetch_one(&pool)
            .await
            .expect("the row arrived");
    assert_eq!(
        evidence, "asserted",
        "and an old bundle says nothing about evidence, so it has to land as asserted — not \
         as something stronger it never claimed"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
