use serde_json::json;
use uuid::Uuid;

#[tokio::test]
#[ignore]
async fn a_sync_that_cannot_get_the_lock_gives_up_and_says_why() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-stuck-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let mut holder = pool.begin().await.expect("begin the blocking transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(memory_industry::handlers::sync::SYNC_LOCK)
        .execute(&mut *holder)
        .await
        .expect("hold the sync lock the way a running import would");

    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        memory_industry::handlers::dispatch(
            &pool,
            "cuba_sync",
            json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
        ),
    )
    .await;

    let elapsed = started.elapsed();
    let inner = outcome.unwrap_or_else(|_| {
        panic!(
            "the sync was still waiting for the lock after 60s. pg_advisory_xact_lock blocks \
             forever by default, and lock_timeout, statement_timeout and \
             idle_in_transaction_session_timeout were all measured at 0 on this database — so a \
             sync that collided with another had no way to end. A hung handler is worse than a \
             failed one: it holds a connection and reports nothing"
        )
    });

    let why = match inner {
        Ok(v) => panic!("the export ran while another transaction held the sync lock: {v}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        why.contains("another sync is holding the lock"),
        "and it has to say what happened, or the operator reads a bare Postgres timeout as a \
         database fault and goes looking in the wrong place. Got: {why}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "it gave up after {elapsed:?}, which is far past the configured wait: the timeout is not \
         the one taking effect"
    );

    drop(holder);
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
