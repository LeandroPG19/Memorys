use std::collections::HashSet;
use uuid::Uuid;

#[tokio::test]
#[ignore]
async fn concurrent_appends_succeed_as_the_app_role_and_keep_the_chain_linear() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("audit_probe_{}", &Uuid::new_v4().to_string()[..8]);
    const APPENDS: usize = 6;

    let mut handles = Vec::with_capacity(APPENDS);
    for i in 0..APPENDS {
        let pool = pool.clone();
        let action = marker.clone();
        handles.push(tokio::spawn(async move {
            memory_industry::handlers::dispatch(
                &pool,
                "cuba_archivo",
                serde_json::json!({
                    "action": "append",
                    "event_action": action,
                    "payload": {"i": i},
                }),
            )
            .await
        }));
    }

    let mut failures = Vec::new();
    for handle in handles {
        match handle.await.expect("append task panicked") {
            Ok(_) => {}
            Err(e) => failures.push(format!("{e:#}")),
        }
    }

    let cleanup = |pool: sqlx::PgPool, marker: String| async move {
        if let Ok(admin) =
            sqlx::PgPool::connect(&std::env::var("DATABASE_URL").unwrap_or_default()).await
        {
            sqlx::query("DELETE FROM brain_audit_log WHERE action = $1")
                .bind(&marker)
                .execute(&admin)
                .await
                .ok();
        }
        drop(pool);
    };

    if !failures.is_empty() {
        let detail = failures.join(" | ");
        cleanup(pool, marker).await;
        panic!(
            "{}/{APPENDS} appends failed under the cuba_app role. A `permission denied for table \
             brain_audit_log` here means a row lock came back into the read of the chain tip: \
             0041 revoked UPDATE on purpose, and SELECT ... FOR UPDATE needs it. Errors: {detail}",
            failures.len()
        );
    }

    let rows: Vec<(i64, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT id, prev_hash FROM brain_audit_log WHERE action = $1 ORDER BY id")
            .bind(&marker)
            .fetch_all(&pool)
            .await
            .expect("reading back the rows this test wrote");

    let written = rows.len();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let mut forked = None;
    for (id, prev) in &rows {
        let key = prev.clone().unwrap_or_default();
        if !seen.insert(key) {
            forked = Some(*id);
            break;
        }
    }

    cleanup(pool, marker).await;

    assert_eq!(
        written, APPENDS,
        "every append reported success, so every row must be in the log"
    );
    assert!(
        forked.is_none(),
        "row {:?} chains from a prev_hash another row already used: the chain forked. Two \
         concurrent appends read the same tip and both committed, which is what SERIALIZABLE \
         exists to stop here — check that the isolation level is still being set, and that the \
         error is not swallowed.",
        forked
    );
}
