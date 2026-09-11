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
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the result is JSON inside that envelope")
}

#[tokio::test]
#[ignore]
async fn a_fact_carries_its_layer_by_name_because_the_uuid_is_local_to_one_install() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-f4-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let subject = format!("thing_{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query(
        "INSERT INTO brain_facts (subject, predicate, object, valid_from, observed_at, layer_id)
         VALUES ($1, 'runs on', 'postgres', NOW(), NOW(),
                 (SELECT layer_id FROM brain_memory_layers ORDER BY layer_name LIMIT 1))",
    )
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("seed a fact that belongs to a memory layer");
    let procedure = format!("recipe_{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query(
        "INSERT INTO brain_procedures (name, steps, trigger_context, verification, success_count)
         VALUES ($1, '[\"make release\"]'::jsonb, 'cutting a version', 'asks for no password', 7)",
    )
    .bind(&procedure)
    .execute(&pool)
    .await
    .expect("seed a procedure");

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let facts: Vec<Value> =
        serde_json::from_slice(&std::fs::read(bundle.join("facts.json")).expect("facts.json"))
            .expect("parse");
    let mine = facts
        .iter()
        .find(|f| f["subject"].as_str() == Some(&subject))
        .expect("the fact was exported");
    assert!(
        mine.get("layer_id").is_none() && mine["layer_name"].is_string(),
        "brain_memory_layers.layer_id is gen_random_uuid() and migration 0020 inserts the four \
         layers without pinning it, so every installation has different ones. Shipping the uuid \
         means the receiving side violates the foreign key and loses the whole bundle — not just \
         the fact. Measured: 0 of 4 ids match between two databases. Got: {mine}"
    );

    let procedures: Vec<Value> = serde_json::from_slice(
        &std::fs::read(bundle.join("procedures.json")).expect("procedures.json"),
    )
    .expect("parse");
    let recipe = procedures
        .iter()
        .find(|p| p["name"].as_str() == Some(&procedure))
        .expect("the procedure was exported");
    assert!(
        recipe.get("embedding").is_none(),
        "vectors travel out of band in embeddings.bin.zst, and a receiver whose model differs \
         re-embeds anyway. A 1024-dimension vector per recipe inside a JSON file is bulk that \
         helps nobody: {recipe}"
    );
    assert_eq!(
        recipe["verification"].as_str(),
        Some("asks for no password"),
        "a procedure without its verification is a recipe nobody can check ran correctly, \
         which is the field that makes brain_procedures worth syncing at all"
    );

    sqlx::query("DELETE FROM brain_facts WHERE subject = $1")
        .bind(&subject)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_procedures WHERE name = $1")
        .bind(&procedure)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn two_machines_that_disagree_do_not_leave_two_current_truths() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-f4c-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let subject = format!("svc_{}", &Uuid::new_v4().to_string()[..8]);
    let old_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO brain_facts (fact_id, subject, predicate, object, valid_from, observed_at)
         VALUES ($1, $2, 'runs', 'postgres 17', NOW() - INTERVAL '2 days',
                 NOW() - INTERVAL '2 days')",
    )
    .bind(old_id)
    .bind(&subject)
    .execute(&pool)
    .await
    .expect("what this machine believed two days ago");

    let newer = Uuid::new_v4();
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2, "manifest_hash": "recomputed anyway",
            "project_id": null, "project_name": null,
            "exported_at": "2026-08-01T00:00:00Z",
            "counts": {"entities": 0, "observations": 0, "episodes": 0, "decisions": 0, "errors": 0, "relations": 0},
            "with_embeddings": false
        }))
        .expect("serialise"),
    )
    .expect("manifest");
    std::fs::write(
        bundle.join("facts.json"),
        serde_json::to_vec_pretty(&json!([{
            "fact_id": newer, "subject": subject, "predicate": "runs",
            "object": "postgres 18",
            "valid_from": chrono::Utc::now(), "observed_at": chrono::Utc::now(),
            "is_current": true
        }]))
        .expect("serialise"),
    )
    .expect("facts.json");

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string()}),
    )
    .await;

    assert_eq!(
        imported["facts_superseded"].as_u64(),
        Some(1),
        "brain_facts has no unique index on (subject, predicate), so two machines that each \
         recorded a current answer produce two rows nothing reconciles — and cuba_faro would \
         return whichever it happened to rank first, as the truth. Got: {imported}"
    );

    let current: Vec<String> = sqlx::query_scalar(
        "SELECT object FROM brain_facts WHERE subject = $1 AND predicate = 'runs' AND is_current",
    )
    .bind(&subject)
    .fetch_all(&pool)
    .await
    .expect("read the current answers");
    assert_eq!(
        current,
        vec!["postgres 18".to_string()],
        "exactly one current answer, and it is the one observed later"
    );

    let (still_there, closed_at): (i64, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT count(*), max(valid_to) FROM brain_facts WHERE fact_id = $1")
            .bind(old_id)
            .fetch_one(&pool)
            .await
            .expect("read the superseded row");
    assert_eq!(
        still_there, 1,
        "and the older answer is closed, not deleted — the whole point of valid_from/valid_to \
         is that what used to be true stays askable"
    );
    assert!(
        closed_at.is_some(),
        "a row that stops being current without a valid_to is a fact with no end, which is \
         the state ck_facts_current_open exists to forbid"
    );

    sqlx::query("DELETE FROM brain_facts WHERE subject = $1")
        .bind(&subject)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
