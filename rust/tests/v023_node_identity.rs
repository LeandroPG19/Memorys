use uuid::Uuid;

#[tokio::test]
#[ignore]
async fn a_machine_knows_who_it_is_without_asking_the_environment() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let once = memory_industry::db::node_id(&pool)
        .await
        .expect("every migrated database has exactly one identity row");
    let twice = memory_industry::db::node_id(&pool)
        .await
        .expect("and reading it again gives the same answer");
    assert_eq!(
        once, twice,
        "the identity a conflict tiebreak compares cannot change between two reads"
    );
    assert_ne!(once, Uuid::nil());

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_node_identity")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        rows, 1,
        "exactly one row, enforced by the schema rather than by everyone remembering"
    );

    let refused = sqlx::query("INSERT INTO brain_node_identity (label) VALUES ('a second self')")
        .execute(&pool)
        .await;
    assert!(
        refused.is_err(),
        "a machine with two identities has none. The environment chain this replaces —\
         CUBA_NODE_NAME then HOSTNAME then COMPUTERNAME then the empty string — could \
         produce a different answer per process, and did: 240 of 1880 rows in the live \
         corpus carry no origin at all because HOSTNAME is not exported to child processes"
    );
}
