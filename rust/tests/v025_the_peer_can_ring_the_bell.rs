use memory_industry::session::{Scope, with_scope};
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

async fn as_peer(pool: &sqlx::PgPool, args: Value) -> anyhow::Result<Value> {
    with_scope(
        Scope::Peer,
        memory_industry::handlers::dispatch(pool, "cuba_sync", args),
    )
    .await
}

fn body(envelope: &Value) -> Value {
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    body(&envelope)
}

#[tokio::test]
#[ignore]
async fn a_notice_reaches_the_local_model_without_the_peer_writing_memory() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _one_at_a_time = own_the_sync_dir(&pool).await;
    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&pool)
        .await
        .expect("start from a clean inbox");

    let marker = format!("bell_{}", &Uuid::new_v4().to_string()[..8]);
    let obs_before: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_observations")
        .fetch_one(&pool)
        .await
        .expect("count observations");

    let recorded = as_peer(
        &pool,
        json!({
            "action": "notify",
            "summary": format!("{marker} el otro nodo encontro que el pool se agota a 40 conexiones"),
            "node_name": "portatil",
            "manifest_hash": "deadbeef",
        }),
    )
    .await
    .expect("a peer may ring the bell");
    assert_eq!(
        body(&recorded)["recorded"].as_bool(),
        Some(true),
        "the notice has to be stored, or the peer has no way to reach the local model at all"
    );

    let obs_after: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_observations")
        .fetch_one(&pool)
        .await
        .expect("count observations");
    assert_eq!(
        obs_before, obs_after,
        "a notice is not memory. The whole reason the peer token is read-only is that the other \
         machine never decides what enters this graph — if notify created an observation, a peer \
         would be writing memory through the one verb it was allowed"
    );

    let opened = call(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": "revisando lo del par"}),
    )
    .await;
    let surfaced = serde_json::to_string(&opened["peer_notices"]).expect("serialise");
    assert!(
        surfaced.contains(&marker),
        "the point of the inbox is that the local model finds out by just using the MCP, not by \
         being told to go and look. Session start is where it opens. Got: {opened}"
    );

    let status = call(&pool, "cuba_sync", json!({"action": "status"})).await;
    assert!(
        serde_json::to_string(&status["peer_notices"])
            .expect("serialise")
            .contains(&marker),
        "and status is where it is checked mid-session. Got: {status}"
    );

    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn the_one_write_a_peer_is_allowed_cannot_fill_the_disk() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _one_at_a_time = own_the_sync_dir(&pool).await;
    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&pool)
        .await
        .expect("clean inbox");

    let flooder = Uuid::new_v4();
    for n in 0..230 {
        sqlx::query(
            "INSERT INTO brain_peer_notices (node_id, node_name, summary) VALUES ($1, 'flood', $2)",
        )
        .bind(flooder)
        .bind(format!("aviso numero {n}"))
        .execute(&pool)
        .await
        .expect("insert a notice");
    }

    let kept: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_peer_notices WHERE node_id = $1")
            .bind(flooder)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(
        kept, 200,
        "a peer that can write one table can fill the disk of the machine that trusted it, so \
         the cap is not decoration. 230 went in and {kept} stayed"
    );

    let newest: String = sqlx::query_scalar(
        "SELECT summary FROM brain_peer_notices WHERE node_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(flooder)
    .fetch_one(&pool)
    .await
    .expect("read the newest");
    assert_eq!(
        newest, "aviso numero 229",
        "and it has to drop the oldest, not the newest — a cap that discards what just arrived \
         would silence the peer exactly when it had most to say"
    );

    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn a_notice_carrying_a_credential_is_refused_like_every_other_free_text_entry() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _one_at_a_time = own_the_sync_dir(&pool).await;

    let refused = as_peer(
        &pool,
        json!({"action": "notify", "summary": "usa el token ghp_abcdefghijklmnop para entrar"}),
    )
    .await;
    let why = match refused {
        Ok(v) => panic!("a peer notice carrying a github token was stored: {v}"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        why.contains("github token"),
        "nine free-text entries already reject credentials at the door; this one arrives over \
         the network from another machine, which makes it the last place to make an exception. \
         Got: {why}"
    );
    assert!(
        !why.contains("ghp_abcdefghijklmnop"),
        "and the refusal must not repeat the secret it refused: {why}"
    );

    let harmless = as_peer(
        &pool,
        json!({"action": "notify", "summary": "revisamos que ninguna password quede en los logs"}),
    )
    .await;
    assert!(
        harmless.is_ok(),
        "prose that talks about passwords without carrying one has to go through, or the gate \
         is a wall: {:#?}",
        harmless.err()
    );

    sqlx::query("DELETE FROM brain_peer_notices")
        .execute(&pool)
        .await
        .ok();
}
