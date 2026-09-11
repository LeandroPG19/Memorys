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
async fn a_tombstone_for_a_table_not_keyed_by_id_does_not_abort_the_import() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-tk-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let subject = format!("gone_{}", &Uuid::new_v4().to_string()[..8]);
    let fact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_facts (subject, predicate, object, valid_from, observed_at)
         VALUES ($1, 'used', 'postgres 17', NOW(), NOW()) RETURNING fact_id",
    )
    .bind(&subject)
    .fetch_one(&pool)
    .await
    .expect("seed a fact");
    sqlx::query("DELETE FROM brain_facts WHERE fact_id = $1")
        .bind(fact_id)
        .execute(&pool)
        .await
        .expect("delete it so the trigger writes the tombstone");

    let buried: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_tombstones WHERE table_name = 'brain_facts' AND row_id = $1",
    )
    .bind(fact_id)
    .fetch_one(&pool)
    .await
    .expect("read the tombstone back");
    assert_eq!(
        buried, 1,
        "without a brain_facts tombstone in the database this test cannot observe the bug at \
         all: the export would write a tombstones.json that never names a table keyed by \
         anything other than id, and the import would take the happy path"
    );

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "skip"}),
    )
    .await;
    assert!(
        imported["skipped"].as_bool() != Some(true),
        "the import has to have actually run for this to prove anything, and it deduplicates by \
         manifest hash. Got: {imported}"
    );

    sqlx::query("DELETE FROM brain_tombstones WHERE row_id = $1")
        .bind(fact_id)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn the_import_deletes_from_exactly_the_tables_that_write_tombstones() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let with_triggers: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text
         FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
         WHERE NOT t.tgisinternal AND t.tgname LIKE '%\\_tombstone'
         ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .expect("read the tombstone triggers");

    let mut allowed: Vec<String> = memory_industry::handlers::sync::TOMBSTONED_TABLES
        .iter()
        .map(|(t, _)| t.to_string())
        .collect();
    allowed.sort();

    assert_eq!(
        with_triggers, allowed,
        "these two lists have to be the same set, and they drifted the moment a migration \
         added tombstone triggers for two more tables while the import's allow-list still \
         named six. The gate caught it as a refused bundle — which is the safe direction, \
         because the import refuses a tombstone for a table it was not built to delete from \
         rather than guessing. The unsafe direction is worse and this test covers it too: a \
         table that stops writing tombstones while the import still deletes from it means \
         deletions stop travelling and nothing says so"
    );

    for (table, key) in memory_industry::handlers::sync::TOMBSTONED_TABLES {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.columns
                            WHERE table_name = $1 AND column_name = $2)",
        )
        .bind(table)
        .bind(key)
        .fetch_one(&pool)
        .await
        .expect("check the key column");
        assert!(
            exists,
            "the import deletes from {table} by {key}, and that column does not exist. \
             brain_facts is keyed by fact_id, not id — assuming every table names its key \
             `id` is how the tombstone trigger itself broke before migration 0049"
        );
    }
}
