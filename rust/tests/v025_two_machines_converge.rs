use serde_json::{Value, json};
use uuid::Uuid;

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0027;

async fn own_the_process(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_SYNC_DIR and CUBA_PEER_TOKEN are process-global");
    tx
}

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

fn second_node_url() -> String {
    std::env::var("CUBA_PEER_DATABASE_URL").expect(
        "the two-node test needs a second database and the runtime role cannot create one, so \
         scripts/run-all-tests.sh provisions it and exports CUBA_PEER_DATABASE_URL. Skipping \
         when it is missing would report green for a machine that never ran two nodes",
    )
}

#[tokio::test]
#[ignore]
async fn what_one_machine_learns_offline_reaches_the_other_when_the_link_comes_back() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let remote_url = second_node_url();

    let local = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to the local database");
    let _one_at_a_time = own_the_process(&local).await;

    let remote = memory_industry::db::create_pool(&remote_url)
        .await
        .unwrap_or_else(|e| panic!("connecting to the second node at {remote_url}: {e:#}"));
    sqlx::query("DELETE FROM brain_sync_peers")
        .execute(&local)
        .await
        .expect("start from no cursor");

    let bundle = std::env::temp_dir().join(format!("cuba-2n-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch sync directory");
    unsafe {
        std::env::set_var("CUBA_SYNC_DIR", &bundle);
        std::env::set_var("CUBA_PEER_TOKEN", "two-node-secret");
        std::env::set_var("CUBA_HTTP_TOKEN", "admin-secret-that-differs");
        std::env::set_var("CUBA_HTTP_ADDR", "127.0.0.1:18797");
    }

    let marker = format!("offline_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&remote)
        .await
        .expect("the other machine learns something while the link is down");
    sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
        .bind(entity_id)
        .bind(format!(
            "{marker} el pool se agota a 40 conexiones bajo carga real"
        ))
        .execute(&remote)
        .await
        .expect("seed the observation");

    let served = remote.clone();
    let daemon = tokio::spawn(async move {
        memory_industry::http::serve_pool("127.0.0.1:18797", served, true).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;

    let before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE content LIKE $1")
            .bind(format!("%{marker}%"))
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        before, 0,
        "the local machine must not already have what the remote learned, or the test proves \
         nothing about it travelling"
    );

    let fetched = call(
        &local,
        "cuba_sync",
        json!({
            "action": "fetch",
            "peer": "the-other-one",
            "url": "http://127.0.0.1:18797",
            "conflict": "skip"
        }),
    )
    .await;
    assert!(
        fetched["imported"]["rows_inserted"].as_u64().unwrap_or(0) > 0,
        "the fetch reported no rows, so either the link or the import did nothing: {fetched}"
    );

    let after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE content LIKE $1")
            .bind(format!("%{marker}%"))
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        after, 1,
        "what the other machine learned while the link was down has to be here now. This is the \
         whole promise: nothing is lost while disconnected, and reconnecting is what moves it"
    );

    let again = call(
        &local,
        "cuba_sync",
        json!({"action": "fetch", "peer": "the-other-one", "conflict": "skip"}),
    )
    .await;
    assert_eq!(
        again["unchanged"].as_bool(),
        Some(true),
        "a second fetch with nothing new has to stop at the cursor without opening a \
         transaction. Before the peer table existed the only loop breaker was the manifest \
         hash, and ordinary use moves access_count, so every export produced a new hash and \
         both sides re-imported forever: the cycle converged in data and never in work. \
         Got: {again}"
    );
    assert!(
        again["url"].as_str().is_some_and(|u| u.contains("18797")),
        "and the address has to be remembered, or every fetch needs it spelled out again: \
         {again}"
    );

    daemon.abort();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe {
        std::env::remove_var("CUBA_SYNC_DIR");
        std::env::remove_var("CUBA_PEER_TOKEN");
        std::env::remove_var("CUBA_HTTP_TOKEN");
        std::env::remove_var("CUBA_HTTP_ADDR");
    }
}
