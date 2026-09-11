use serde_json::{Value, json};
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

fn peer_entry<'a>(response: &'a Value, name: &str) -> Option<&'a Value> {
    response["sync_peers"]
        .as_array()?
        .iter()
        .find(|p| p["name"] == name)
}

#[tokio::test]
#[ignore]
async fn a_search_says_how_stale_its_synced_peers_are() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    sqlx::query("DELETE FROM brain_sync_peers")
        .execute(&pool)
        .await
        .expect("start from no peers");

    let empty = call(&pool, "cuba_faro", json!({"query": "does not matter"})).await;
    assert!(
        empty.get("sync_peers").is_none(),
        "cuba_faro is the most-called tool in this server: a field that is present on a \
         single-machine install would sit in every response of every search anyone ever runs \
         here, teaching the caller to ignore it long before it ever mattered. Got: {empty:?}"
    );

    let marker = format!("peer-{}", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO brain_sync_peers (name, url, last_synced_at)
         VALUES ($1, 'http://peer.invalid', NOW() - INTERVAL '5 seconds')",
    )
    .bind(&marker)
    .execute(&pool)
    .await
    .expect("seed a freshly synced peer");

    let fresh = call(&pool, "cuba_faro", json!({"query": "does not matter"})).await;
    let entry = peer_entry(&fresh, &marker).unwrap_or_else(|| {
        panic!("the seeded peer must be reported once brain_sync_peers is non-empty: {fresh:?}")
    });
    let age = entry["age_seconds"]
        .as_i64()
        .expect("a peer synced 5s ago must carry its age in seconds");
    assert!(
        (0..60).contains(&age),
        "synced 5s ago, got age_seconds={age}"
    );
    assert!(
        entry.get("warning").is_none(),
        "a peer that synced 5s ago is not the stale-generation risk this field exists to flag; \
         a warning here would be the same noise the empty-table case above avoids. Got: {entry:?}"
    );

    sqlx::query(
        "UPDATE brain_sync_peers SET last_synced_at = NOW() - INTERVAL '3 days' WHERE name = $1",
    )
    .bind(&marker)
    .execute(&pool)
    .await
    .expect("age the peer past any reasonable threshold");

    let stale = call(&pool, "cuba_faro", json!({"query": "does not matter"})).await;
    let entry = peer_entry(&stale, &marker).expect("the peer is still there, just older");
    let warning = entry["warning"].as_str().unwrap_or_else(|| {
        panic!(
            "3 days unsynced must trip the warning: an agent session on this server routinely \
             spans hours, so a peer that fell a full day behind the threshold (chosen at 24h, \
             one working day) has almost certainly diverged from what this response treats as \
             current. 5s above must stay silent and 3 days here must not, or the threshold does \
             nothing. Got: {entry:?}"
        )
    });
    assert!(
        warning.contains(&marker),
        "the warning must name which peer is stale, not just that some peer is: {warning:?}"
    );

    sqlx::query("UPDATE brain_sync_peers SET last_error = $2 WHERE name = $1")
        .bind(&marker)
        .bind("connection refused")
        .execute(&pool)
        .await
        .expect("mark the peer's last attempt as failed");

    let failing = call(&pool, "cuba_faro", json!({"query": "does not matter"})).await;
    let entry = peer_entry(&failing, &marker).expect("the peer is still there, now failing too");
    let warning = entry["warning"]
        .as_str()
        .expect("an unresolved last_error must still produce a warning");
    assert!(
        warning.contains("connection refused"),
        "'3 days ago' and '3 days ago and also failing' are different situations for whoever \
         reads this: a peer that is merely old might catch up on its own, one whose last \
         attempt failed will not until something is fixed. The warning must name the error, \
         not just the age. Got: {warning:?}"
    );

    sqlx::query("DELETE FROM brain_sync_peers WHERE name = $1")
        .bind(&marker)
        .execute(&pool)
        .await
        .ok();
}
