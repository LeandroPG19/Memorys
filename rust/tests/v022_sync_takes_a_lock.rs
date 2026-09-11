use std::time::Duration;

#[tokio::test]
#[ignore]
async fn an_export_waits_instead_of_racing_a_concurrent_one() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-lock-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let mut holder = pool
        .acquire()
        .await
        .expect("a connection to hold the lock on");
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(memory_industry::handlers::sync::SYNC_LOCK)
        .execute(&mut *holder)
        .await
        .expect("take the sync lock the way a concurrent export would");

    let racing = memory_industry::handlers::dispatch(
        &pool,
        "cuba_sync",
        serde_json::json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    );
    let blocked = tokio::time::timeout(Duration::from_secs(3), racing).await;

    assert!(
        blocked.is_err(),
        "the export ran to completion while another holder had the sync lock. Without \
         serialisation, one export prunes the files the other just wrote — 480 race windows \
         per import on this corpus — and the worst outcome is not a crash: it is an import \
         that reads a torn directory, happens to parse, commits, and records the manifest \
         hash, after which that hash is skipped forever and the rows that never arrived \
         never will"
    );

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(memory_industry::handlers::sync::SYNC_LOCK)
        .execute(&mut *holder)
        .await
        .expect("release the lock");
    drop(holder);

    let freed = tokio::time::timeout(
        Duration::from_secs(60),
        memory_industry::handlers::dispatch(
            &pool,
            "cuba_sync",
            serde_json::json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
        ),
    )
    .await
    .expect("once the lock is free the export must proceed, or this is a deadlock and not a lock");
    assert!(
        freed.is_ok(),
        "the export failed after the lock was released: {freed:?}"
    );

    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
