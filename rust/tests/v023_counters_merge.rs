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

#[tokio::test]
#[ignore]
async fn neither_machine_loses_the_reinforcement_it_earned() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-count-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    for (id, n) in [(a, "counters_a"), (b, "counters_b")] {
        sqlx::query(
            "INSERT INTO brain_entities (id, name, entity_type, importance, access_count)
             VALUES ($1, $2, 'concept', 0.9, 40)",
        )
        .bind(id)
        .bind(format!("{n}_{}", &id.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed an entity this machine has used a lot");
    }
    sqlx::query(
        "INSERT INTO brain_relations (from_entity, to_entity, relation_type, strength, provenance)
         VALUES ($1, $2, 'uses', 0.8, 'extracted')",
    )
    .bind(a)
    .bind(b)
    .execute(&pool)
    .await
    .expect("seed a relation this machine has traversed a lot");

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    sqlx::query("UPDATE brain_entities SET importance = 0.2, access_count = 3 WHERE id = ANY($1)")
        .bind(vec![a, b])
        .execute(&pool)
        .await
        .expect("now pretend this machine is the one that barely used it");
    sqlx::query("UPDATE brain_relations SET strength = 0.1 WHERE from_entity = $1")
        .bind(a)
        .execute(&pool)
        .await
        .expect("same for the edge");

    call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "merge"}),
    )
    .await;

    let (importance, access): (f64, i32) =
        sqlx::query_as("SELECT importance::float8, access_count FROM brain_entities WHERE id = $1")
            .bind(a)
            .fetch_one(&pool)
            .await
            .expect("read the entity back");
    assert!(
        importance > 0.8 && access >= 40,
        "importance and access_count grow with use on each machine independently. Whoever \
         imported last used to win, which throws away everything the other side learned — and \
         with conflict=merge, which is the default the git hook installs, nothing merged at \
         all. Got importance={importance} access_count={access}"
    );

    let strength: f64 =
        sqlx::query_scalar("SELECT strength::float8 FROM brain_relations WHERE from_entity = $1")
            .bind(a)
            .fetch_one(&pool)
            .await
            .expect("read the relation back");
    assert!(
        strength > 0.7,
        "strength is Hebbian: puente.rs grows it 0.1 per traversal, capped at 1.0, on each \
         machine separately. Taking one side's number discards the other's traversals. \
         Got {strength}"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = ANY($1)")
        .bind(vec![a, b])
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
