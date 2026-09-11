async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

#[tokio::test]
#[ignore]
async fn admin_status_reflects_a_real_row_from_brain_rem_cycles() {
    let pool = pool().await;

    let started = chrono::Utc::now() + chrono::Duration::minutes(10);
    let finished = started + chrono::Duration::seconds(7);
    let inserted: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_rem_cycles
             (started_at, finished_at, duration_ms, decayed_count, autolink_edges,
              embeddings_backfilled, entities_scanned, facts_extracted, communities,
              duplicate_candidates, relation_scan_failed, extraction_failed, error)
         VALUES ($1, $2, 7000, 42, 3, 9, 6, 5, 2, 1, 0, 0, NULL)
         RETURNING id",
    )
    .bind(started)
    .bind(finished)
    .fetch_one(&pool)
    .await
    .expect(
        "insert a fixture cycle 10 minutes in the future, so ORDER BY finished_at DESC \
             makes it win over anything a concurrent test in this shared database wrote with a \
             real NOW()",
    );

    let status = memory_industry::admin::handle(&pool, "admin/status", 0, vec![])
        .await
        .expect("admin/status");

    sqlx::query("DELETE FROM brain_rem_cycles WHERE id = $1")
        .bind(inserted.0)
        .execute(&pool)
        .await
        .ok();

    let mine = status["rem"]["recent"]
        .as_array()
        .expect("recent is an array")
        .iter()
        .find(|c| c["decayed_count"] == 42)
        .unwrap_or_else(|| {
            panic!(
                "the fixture's own decayed_count=42 has to come back through admin/status — \
                 this test does not assume it lands at rem.last, because a sibling test in \
                 this same file inserts a fixture too and both run concurrently against one \
                 database: {status}"
            )
        });

    assert_eq!(
        mine["autolink_edges"], 3,
        "admin/status has to be reading brain_rem_cycles (migration 0059) through the exact \
         column names this test wrote, not a stale cache or a differently-named column: {mine}"
    );
    assert_eq!(mine["embeddings_backfilled"], 9);
    assert_eq!(mine["entities_scanned"], 6);
    assert_eq!(mine["facts_extracted"], 5);
    assert_eq!(mine["communities"], 2);
    assert_eq!(mine["duplicate_candidates"], 1);
    assert_eq!(mine["duration_ms"], 7000);
    assert_eq!(
        mine["llm_degraded"], false,
        "neither LLM step failed in this fixture"
    );
    assert_eq!(
        status["rem"]["stale"], false,
        "a cycle finished 10 minutes in the future is not stale by any reading of the clock"
    );
}

#[tokio::test]
#[ignore]
async fn a_cycle_whose_relation_scan_gave_up_shows_up_as_degraded_through_the_real_table() {
    let pool = pool().await;

    let started = chrono::Utc::now() + chrono::Duration::minutes(11);
    let finished = started + chrono::Duration::seconds(3);
    let inserted: (uuid::Uuid,) = sqlx::query_as(
        "INSERT INTO brain_rem_cycles
             (started_at, finished_at, duration_ms, decayed_count, autolink_edges,
              embeddings_backfilled, entities_scanned, facts_extracted, communities,
              duplicate_candidates, relation_scan_failed, extraction_failed, error)
         VALUES ($1, $2, 3000, 99, 0, 0, 2, 0, 0, 0, 2, 0, NULL)
         RETURNING id",
    )
    .bind(started)
    .bind(finished)
    .fetch_one(&pool)
    .await
    .expect("insert a fixture cycle whose relation scan hit the two-failure cutoff");

    let status = memory_industry::admin::handle(&pool, "admin/status", 0, vec![])
        .await
        .expect("admin/status");

    sqlx::query("DELETE FROM brain_rem_cycles WHERE id = $1")
        .bind(inserted.0)
        .execute(&pool)
        .await
        .ok();

    let mine = status["rem"]["recent"]
        .as_array()
        .expect("recent is an array")
        .iter()
        .find(|c| c["decayed_count"] == 99)
        .unwrap_or_else(|| {
            panic!(
                "this fixture's own decayed_count=99 has to come back through admin/status, \
                 found among rem.recent rather than assumed at rem.last, because a sibling \
                 test in this same file inserts a fixture too and both run concurrently \
                 against one database: {status}"
            )
        });

    assert_eq!(
        mine["llm_degraded"], true,
        "relation_scan_failed=2 is exactly the case the daemon used to leave silent: the CLI \
         falls off PATH, the relation scan fails twice in a row, gives up for the cycle, and \
         nothing said so anywhere but a log line gone at the next restart. It has to reach \
         admin/status through the real table: {mine}"
    );
    assert!(
        mine["error"].is_null(),
        "the cycle itself still completed — run_rem_consolidation returns Ok even when a step \
         inside it gives up — so this must not read as a crashed cycle: {mine}"
    );
}
