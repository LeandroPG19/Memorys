use serde_json::json;

#[tokio::test]
#[ignore]
async fn a_handler_that_fails_is_recorded_and_one_that_works_is_not() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("no-such-action-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let failed =
        memory_industry::handlers::dispatch(&pool, "cuba_sync", json!({"action": marker})).await;
    assert!(failed.is_err(), "that call was supposed to fail");

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT tool, error FROM brain_handler_failures WHERE error LIKE '%' || $1 || '%'",
    )
    .bind(&marker)
    .fetch_all(&pool)
    .await
    .expect("read the failure log");
    assert_eq!(
        rows.len(),
        1,
        "until now nothing recorded that a tool call failed: the line went to tracing and from \
         there to the journal, where it survives until rotation and where nobody looks unless \
         they already suspect something. Got: {rows:?}"
    );
    assert_eq!(rows[0].0, "cuba_sync");
    assert!(
        rows[0].1.contains("Invalid action"),
        "and the message has to be the one the handler gave, or the log says a call failed \
         without saying how: {:?}",
        rows[0].1
    );

    memory_industry::handlers::dispatch(&pool, "cuba_sync", json!({"action": "status"}))
        .await
        .expect("a good call");

    let after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_handler_failures WHERE error LIKE '%' || $1 || '%'",
    )
    .bind(&marker)
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(
        after, 1,
        "successful calls must NOT land here. cuba_faro is called constantly, and a row per \
         call would make the telemetry table grow faster than the memory it watches on a \
         corpus that takes four writes a day. That is the whole reason the recent calls live \
         in a ring in memory instead.\n\nCounted by this test's own marker and not by the \
         whole table: the gate runs every test file as a parallel process against one \
         database, so any other binary's failed call lands in the same log and made this \
         assertion red for a reason that had nothing to do with what it checks"
    );

    let ring = memory_industry::observability::recent_calls(10);
    assert!(
        ring.iter()
            .any(|c| c.tool == "cuba_sync" && c.outcome == "ok"),
        "and the successful call still has to be visible somewhere — the ring is what the panel \
         reads for recent traffic. Got: {} entries",
        ring.len()
    );

    sqlx::query("DELETE FROM brain_handler_failures WHERE error LIKE '%' || $1 || '%'")
        .bind(&marker)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn a_failure_carrying_a_credential_is_redacted_before_it_is_stored() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    const TOKEN: &str = "ghp_abcdefghijklmnop";
    let tail = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let mark = format!("redact{tail}");
    let secret = format!("{mark}-{TOKEN}{tail}");
    let failed =
        memory_industry::handlers::dispatch(&pool, "cuba_sync", json!({"action": secret})).await;
    let message = format!("{:#}", failed.expect_err("an unknown action fails"));
    assert!(
        message.contains(&secret),
        "this test needs a handler that echoes its input into the error message, because that \
         is how a secret reaches the failure log at all. cuba_sync answers 'Invalid action: X' \
         with X quoted back. If that ever stops being true this test can no longer observe the \
         redaction and has to be pointed at another path rather than left passing. Got: {message}"
    );

    let stored: Vec<String> = sqlx::query_scalar(
        "SELECT error FROM brain_handler_failures WHERE error LIKE '%' || $1 || '%'",
    )
    .bind(&mark)
    .fetch_all(&pool)
    .await
    .expect("read the log");
    assert_eq!(
        stored.len(),
        1,
        "the refusal has to be recorded, and found by a marker that survives redaction so this \
         counts its own row and not one another test binary wrote a moment earlier against the \
         same database: {stored:?}"
    );
    assert!(
        !stored.iter().any(|e| e.contains(&secret)),
        "the failure log would otherwise become the one place in this database where rejected \
         credentials are kept in the clear — written by the very guard that exists to keep them \
         out. The redaction runs before the insert, so the token never reaches a column. \
         Stored: {stored:?}"
    );

    sqlx::query("DELETE FROM brain_handler_failures WHERE error LIKE '%' || $1 || '%'")
        .bind(&tail)
        .execute(&pool)
        .await
        .ok();
}
