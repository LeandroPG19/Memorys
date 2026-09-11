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

async fn call(pool: &sqlx::PgPool, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, "cuba_sync", args)
        .await
        .unwrap_or_else(|e| panic!("cuba_sync failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

fn what_the_panel_sends() -> Value {
    let html = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/panel/index.html"),
    )
    .expect("the panel page");
    let call = html
        .split("callTool(\"cuba_sync\", { action: \"fetch\"")
        .nth(1)
        .expect(
            "the panel's peer button calls cuba_sync fetch, and if that ever changes shape this \
             test is checking a string that no longer exists — which is exactly how the previous \
             version of this guard passed while the button could delete",
        );
    let args = call.split("});").next().unwrap_or("");
    json!({ "sends_withhold": args.contains("deletes: \"withhold\"") })
}

#[tokio::test]
#[ignore]
async fn one_click_on_the_peer_button_cannot_delete_a_row() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-nodel-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("scratch");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    assert_eq!(
        what_the_panel_sends()["sends_withhold"].as_bool(),
        Some(true),
        "the panel's one button that writes has to ask for no deletions. Below the alarm \
         threshold a sync deletes without confirming — deliberately, because a guard that trips \
         on ordinary curation gets bypassed — so a bare fetch behind a button is one click from \
         an irreversible delete of up to 24 rows. The previous test only looked for the literal \
         string `action: \"import\"` and its own comment claimed fetching keeps history, which \
         is false"
    );

    let marker = format!("nodel_{}", &Uuid::new_v4().to_string()[..8]);
    let doomed = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(doomed)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed a row the tombstone will name");

    call(
        &pool,
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let tombstones = json!([{
        "table_name": "brain_entities",
        "row_id": doomed,
        "deleted_at": chrono::Utc::now(),
        "origin_node": "la-otra-maquina",
    }]);
    std::fs::write(
        bundle.join("tombstones.json"),
        serde_json::to_vec_pretty(&tombstones).expect("serialise"),
    )
    .expect("write the tombstone the peer would send");

    let withheld = call(
        &pool,
        json!({
            "action": "import",
            "dir": bundle.display().to_string(),
            "conflict": "skip",
            "deletes": "withhold"
        }),
    )
    .await;

    let still_here: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE id = $1")
        .bind(doomed)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        still_here, 1,
        "with deletes=withhold not one row may go, however small the tombstone list. Got: \
         {withheld}"
    );
    assert!(
        !withheld["tombstones_withheld"]
            .as_array()
            .expect("a list")
            .is_empty(),
        "and it has to say it refused, or the caller reads a silent success and never learns \
         the peer wanted something deleted: {withheld}"
    );

    let applied = call(
        &pool,
        json!({
            "action": "import",
            "dir": bundle.display().to_string(),
            "conflict": "skip"
        }),
    )
    .await;
    let gone: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE id = $1")
        .bind(doomed)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        gone, 0,
        "and the ordinary sync path still has to delete, or this change broke the thing \
         tombstones exist for and the first assertion proves nothing. Got: {applied}"
    );

    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
