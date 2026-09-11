use serde_json::{Value, json};
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the result is JSON inside that envelope")
}

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0023;

async fn own_the_sync_dir(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect(
            "CUBA_SYNC_DIR is process-global, so two tests in this binary that both set it \
                 clobber each other — one ends up exporting into the other's directory and the \
                 path guard refuses it",
        );
    tx
}

fn plant_tombstone(root: &std::path::Path, table: &str, id: Uuid) {
    std::fs::write(
        root.join("tombstones.json"),
        serde_json::to_vec_pretty(&json!([{
            "table_name": table,
            "row_id": id,
            "deleted_at": "2026-08-01T00:00:00Z",
            "origin_node": "the-other-machine"
        }]))
        .expect("serialise"),
    )
    .expect("write tombstones.json");
}

#[tokio::test]
#[ignore]
async fn a_deletion_travels_instead_of_being_undone_on_the_next_round() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-tomb-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns_the_dir = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity = Uuid::new_v4();
    let doomed = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity)
        .bind(format!("tomb_{}", &entity.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query("INSERT INTO brain_observations (id, entity_id, content) VALUES ($1, $2, $3)")
        .bind(doomed)
        .bind(entity)
        .bind("the other machine deleted this")
        .execute(&pool)
        .await
        .expect("seed the row the peer deleted");

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    plant_tombstone(&bundle, "brain_observations", doomed);

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;

    assert_eq!(
        imported["tombstones_applied"]["brain_observations"]
            .as_u64()
            .unwrap_or(0),
        1,
        "import ran no DELETE at all before this, so a row deleted on the other machine \
         survived here — and then travelled back on the next export and undid the delete at \
         the source. Measured end to end before this change. Got: {imported}"
    );

    let alive: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE id = $1")
        .bind(doomed)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(alive, 0, "the row named by the tombstone has to be gone");

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn an_entity_tombstone_never_takes_children_the_sender_did_not_know_about() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-tomb2-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns_the_dir = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity)
        .bind(format!("shared_{}", &entity.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed the entity");
    for i in 0..3 {
        sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
            .bind(entity)
            .bind(format!("only this machine has this one, number {i}"))
            .execute(&pool)
            .await
            .expect("seed a local-only child");
    }

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    plant_tombstone(&bundle, "brain_entities", entity);

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;

    let withheld = imported["tombstones_withheld"]
        .as_array()
        .expect("an array")
        .len();
    assert_eq!(
        withheld, 1,
        "deleting an entity cascades to seven tables. The sender knew about its own children; \
         it did not know about the three here. Honouring the tombstone by id would have taken \
         all three with it — on the live corpus the busiest entity has 332 children, and that \
         is the shape of the loss. Got: {imported}"
    );

    let survivors: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE entity_id = $1")
            .bind(entity)
            .fetch_one(&pool)
            .await
            .expect("count");
    assert_eq!(
        survivors, 3,
        "and not one of the children this machine knew about may be gone"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
