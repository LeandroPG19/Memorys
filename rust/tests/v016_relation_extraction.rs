use serde_json::json;
use uuid::Uuid;

fn unique_name(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

const RELATION_TEST_LOCK: i64 = 0x0CBA_A0D1_7106_0016;

async fn exclusive(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(RELATION_TEST_LOCK)
        .execute(&mut *tx)
        .await
        .expect(
            "both tests in this file share one database, and one of them asserts a global \
                 count while the other creates relations. Without serialising them the count is \
                 whatever the interleaving happened to be",
        );
    tx
}

#[tokio::test]
#[ignore]
async fn auto_extract_relations_land_as_inferred_edges() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let _serialised = exclusive(&pool).await;

    let a = unique_name("relext_a");
    let b = unique_name("relext_b");

    let reply = format!(
        r#"{{"facts":[{{"entity_name":"{a}","content":"{a} is a test subject","observation_type":"fact"}}],
            "relations":[{{"from":"{a}","to":"{b}","relation_type":"depends_on"}}]}}"#
    );

    let linked = memory_industry::handlers::ingesta::link_relations_from_reply(&pool, &reply)
        .await
        .expect("linking extracted relations");
    assert_eq!(linked, 1, "the one relation in the reply must be written");

    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT r.relation_type, r.provenance
         FROM brain_relations r
         JOIN brain_entities ea ON ea.id = r.from_entity
         JOIN brain_entities eb ON eb.id = r.to_entity
         WHERE ea.name = $1 AND eb.name = $2",
    )
    .bind(&a)
    .bind(&b)
    .fetch_optional(&pool)
    .await
    .expect("reading back the edge");

    let (relation_type, provenance) = row.expect("the edge must exist");
    assert_eq!(relation_type, "depends_on");
    assert_eq!(
        provenance, "inferred",
        "an LLM-read relation must be distinguishable from a hand-asserted one"
    );

    let endpoints: (i64,) =
        sqlx::query_as("SELECT count(*) FROM brain_entities WHERE name = $1 OR name = $2")
            .bind(&a)
            .bind(&b)
            .fetch_one(&pool)
            .await
            .expect("counting endpoints");
    assert_eq!(
        endpoints.0, 2,
        "both endpoints must be auto-created even though only one had a fact"
    );

    let second = memory_industry::handlers::ingesta::link_relations_from_reply(&pool, &reply)
        .await
        .expect("re-linking the same relation");
    assert_eq!(
        second, 1,
        "re-running must strengthen the existing edge, not error"
    );
    let count: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM brain_relations r
         JOIN brain_entities ea ON ea.id = r.from_entity
         JOIN brain_entities eb ON eb.id = r.to_entity
         WHERE ea.name = $1 AND eb.name = $2",
    )
    .bind(&a)
    .bind(&b)
    .fetch_one(&pool)
    .await
    .expect("counting edges");
    assert_eq!(count.0, 1, "re-running must not duplicate the edge");

    sqlx::query("DELETE FROM brain_entities WHERE name = $1 OR name = $2")
        .bind(&a)
        .bind(&b)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn a_reply_with_no_relations_writes_nothing() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let _serialised = exclusive(&pool).await;

    let before: (i64,) = sqlx::query_as("SELECT count(*) FROM brain_relations")
        .fetch_one(&pool)
        .await
        .expect("counting relations");

    let reply = json!({"facts": [], "relations": []}).to_string();
    let linked = memory_industry::handlers::ingesta::link_relations_from_reply(&pool, &reply)
        .await
        .expect("linking an empty reply");
    assert_eq!(linked, 0);

    let after: (i64,) = sqlx::query_as("SELECT count(*) FROM brain_relations")
        .fetch_one(&pool)
        .await
        .expect("counting relations");
    assert_eq!(before.0, after.0, "an empty reply must be a no-op");
}
