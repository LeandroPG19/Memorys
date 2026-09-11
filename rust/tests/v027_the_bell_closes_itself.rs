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
        .expect("CUBA_SYNC_DIR and the tokens are process-global");
    tx
}

async fn call(pool: &sqlx::PgPool, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, "cuba_sync", args)
        .await
        .unwrap_or_else(|e| panic!("cuba_sync failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

#[tokio::test]
#[ignore]
async fn taking_what_the_peer_offered_silences_its_bell() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let remote_url = std::env::var("CUBA_PEER_DATABASE_URL").expect(
        "this needs a second database, provisioned by scripts/run-all-tests.sh. Skipping when it \
         is absent would report green for a machine that never ran two nodes",
    );

    let local = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to the local database");
    let _one_at_a_time = own_the_process(&local).await;
    let remote = memory_industry::db::create_pool(&remote_url)
        .await
        .expect("connect to the second node");

    let bundle = std::env::temp_dir().join(format!("cuba-bell-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("scratch");
    unsafe {
        std::env::set_var("CUBA_SYNC_DIR", &bundle);
        std::env::set_var("CUBA_PEER_TOKEN", "bell-secret");
        std::env::set_var("CUBA_HTTP_TOKEN", "bell-admin-secret");
    }
    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&local)
        .await
        .expect("clean inbox");
    sqlx::query("DELETE FROM brain_sync_peers")
        .execute(&local)
        .await
        .expect("clean cursor");

    let marker = format!("bell_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&remote)
        .await
        .expect("the other machine learns something");

    let peer_node: Uuid = sqlx::query_scalar("SELECT node_id FROM brain_node_identity")
        .fetch_one(&remote)
        .await
        .expect("the second node has an identity of its own");
    let local_node: Uuid = sqlx::query_scalar("SELECT node_id FROM brain_node_identity")
        .fetch_one(&local)
        .await
        .expect("and so does this one");
    assert_ne!(
        peer_node, local_node,
        "two installs must not share a node id, or closing a notice by origin would close this \
         machine's own bells as well. Migration 0046 generates it per database for exactly \
         this. Read straight from the table and not through db::node_id, which memoises in a \
         process-wide OnceCell: correct for a daemon that serves one database, wrong for a test \
         holding two pools, where it hands back whichever it saw first"
    );

    sqlx::query(
        "INSERT INTO brain_peer_notices (node_id, node_name, summary)
         VALUES ($1, 'la-otra', $2)",
    )
    .bind(peer_node)
    .bind(format!("{marker} encontré algo que no tenés"))
    .execute(&local)
    .await
    .expect("the peer rings the bell");

    let served = remote.clone();
    let daemon = tokio::spawn(async move {
        memory_industry::http::serve_pool("127.0.0.1:18821", served, true).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let open_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_peer_notices WHERE resolved_at IS NULL")
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        open_before, 1,
        "the bell has to be ringing before we answer it"
    );

    let fetched = call(
        &local,
        json!({
            "action": "fetch",
            "peer": "la-otra",
            "url": "http://127.0.0.1:18821",
            "conflict": "skip"
        }),
    )
    .await;

    assert_eq!(
        fetched["notices_closed"].as_u64(),
        Some(1),
        "a notice says «I have something»; taking it is what makes that stop being true. \
         Closing by origin rather than by manifest hash is what lets the sender ring the bell \
         without first exporting a bundle just to learn its own hash — at four writes a day \
         that export would cost more than the change it announces. Got: {fetched}"
    );

    let still_open: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_peer_notices WHERE resolved_at IS NULL")
            .fetch_one(&local)
            .await
            .expect("count");
    assert_eq!(
        still_open, 0,
        "and it has to be closed in the database, not merely counted in the answer"
    );

    let arrived: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE name = $1")
        .bind(&marker)
        .fetch_one(&local)
        .await
        .expect("count");
    assert_eq!(
        arrived, 1,
        "and the thing the bell was about has to have actually arrived, or the notice was \
         closed on a promise"
    );

    daemon.abort();
    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&marker)
        .execute(&local)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&local)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe {
        std::env::remove_var("CUBA_SYNC_DIR");
        std::env::remove_var("CUBA_PEER_TOKEN");
        std::env::remove_var("CUBA_HTTP_TOKEN");
    }
}
