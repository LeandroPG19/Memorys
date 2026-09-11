use sqlx::{Connection, Executor, PgConnection, PgPool};

const CYCLES_TABLE: &str = "CREATE TABLE brain_rem_cycles (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    started_at timestamptz NOT NULL,
    finished_at timestamptz NOT NULL,
    duration_ms bigint NOT NULL,
    decayed_count bigint NOT NULL DEFAULT 0,
    autolink_edges bigint NOT NULL DEFAULT 0,
    embeddings_backfilled bigint NOT NULL DEFAULT 0,
    entities_scanned bigint NOT NULL DEFAULT 0,
    facts_extracted bigint NOT NULL DEFAULT 0,
    communities bigint NOT NULL DEFAULT 0,
    duplicate_candidates bigint NOT NULL DEFAULT 0,
    relation_scan_failed bigint NOT NULL DEFAULT 0,
    extraction_failed bigint NOT NULL DEFAULT 0,
    error text,
    created_at timestamptz NOT NULL DEFAULT NOW()
)";

fn admin_url() -> String {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for this test");
    let cut = url.rfind('/').expect("a database URL ends in /<name>");
    format!("{}/postgres", &url[..cut])
}

fn scratch_url(name: &str) -> String {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for this test");
    let cut = url.rfind('/').expect("a database URL ends in /<name>");
    format!("{}/{}", &url[..cut], name)
}

async fn admin() -> PgConnection {
    PgConnection::connect(&admin_url())
        .await
        .expect("connecting to the maintenance database")
}

#[tokio::test]
#[ignore]
async fn a_cycle_that_dies_is_written_down_by_the_caller_that_watched_it_die() {
    let scratch = format!("brain_remfail_{}", &uuid::Uuid::new_v4().to_string()[..8]);

    let mut conn = admin().await;
    conn.execute(format!("CREATE DATABASE {scratch}").as_str())
        .await
        .expect("creating the scratch database");
    drop(conn);

    let pool = PgPool::connect(&scratch_url(&scratch))
        .await
        .expect("connecting to the scratch database");
    pool.execute(CYCLES_TABLE)
        .await
        .expect("creating the only table the failure path is allowed to touch");

    let outcome = memory_industry::protocol::run_rem_consolidation(&pool).await;

    let recorded: Vec<(Option<String>, i64)> =
        sqlx::query_as("SELECT error, entities_scanned FROM brain_rem_cycles")
            .fetch_all(&pool)
            .await
            .expect("reading brain_rem_cycles back");

    pool.close().await;
    let mut conn = admin().await;
    conn.execute(format!("DROP DATABASE IF EXISTS {scratch} WITH (FORCE)").as_str())
        .await
        .ok();

    assert!(
        outcome.is_err(),
        "this test only means anything if the cycle actually failed: the scratch database has \
         brain_rem_cycles and nothing else, so the consolidation must die on its first real \
         query. It returned Ok, so the failure path was never exercised and the assertion \
         below would pass for the wrong reason"
    );
    assert_eq!(
        recorded.len(),
        1,
        "a cycle that dies mid-flight must leave exactly one row behind. Calling \
         record_rem_cycle_failure directly proves the INSERT works; only this proves \
         run_rem_consolidation still calls it. Deleting that call from the wrapper left every \
         other test green — the panel would go on showing an older successful cycle as if it \
         were the latest run, which is the exact blindness brain_rem_cycles was added to end"
    );
    assert!(
        recorded[0].0.is_some(),
        "the row is there but its error column is NULL, so it reads back exactly like a clean \
         cycle that happened to do nothing: {recorded:?}"
    );
}
