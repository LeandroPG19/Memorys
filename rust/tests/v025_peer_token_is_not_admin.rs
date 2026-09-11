use memory_industry::session::{Scope, with_scope};
use serde_json::json;

async fn refused(pool: &sqlx::PgPool, tool: &str, args: serde_json::Value) -> String {
    let outcome = with_scope(
        Scope::Peer,
        memory_industry::handlers::dispatch(pool, tool, args),
    )
    .await;
    match outcome {
        Ok(value) => panic!("{tool} was served to a peer token: {value}"),
        Err(e) => format!("{e:#}"),
    }
}

#[tokio::test]
#[ignore]
async fn a_peer_token_cannot_reach_the_destructive_tools() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let why = refused(&pool, "cuba_forget", json!({"query": "anything"})).await;
    assert!(
        why.contains("peer token"),
        "the refusal has to name the reason, or an operator reads it as an outage and goes \
         looking for the bug. Got: {why}"
    );

    refused(
        &pool,
        "cuba_zafra",
        json!({"action": "prune", "confirm": true}),
    )
    .await;
    refused(&pool, "cuba_proyecto", json!({"action": "list"})).await;
    refused(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all"}),
    )
    .await;
    refused(&pool, "cuba_sync", json!({"action": "import"})).await;
}

#[tokio::test]
#[ignore]
async fn a_peer_cannot_smuggle_a_forbidden_tool_inside_cuba_call() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let why = refused(
        &pool,
        "cuba_call",
        json!({"tool": "cuba_forget", "args": {"query": "anything"}}),
    )
    .await;
    assert!(
        why.contains("cuba_call"),
        "cuba_call is the whole reason this check lives inside dispatch instead of at the HTTP \
         edge: it takes a tool name as an argument and reaches every handler through the same \
         function, so an allow-list that only looked at the outer envelope would pass \
         {{\"tool\": \"cuba_forget\"}} straight through. Got: {why}"
    );
}

#[tokio::test]
#[ignore]
async fn the_peer_verb_itself_still_works_and_the_full_scope_is_untouched() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let served = with_scope(
        Scope::Peer,
        memory_industry::handlers::dispatch(&pool, "cuba_sync", json!({"action": "status"})),
    )
    .await;
    assert!(
        served.is_ok(),
        "a guard that refuses everything is not a guard, it is an outage with a uniform. The \
         peer verb has to go through: {:#?}",
        served.err()
    );

    let unrestricted =
        memory_industry::handlers::dispatch(&pool, "cuba_proyecto", json!({"action": "list"}))
            .await;
    assert!(
        unrestricted.is_ok(),
        "and with no scope set — every CLI run, every stdio session, every test — dispatch has \
         to behave exactly as it did before. The task-local defaults to Full precisely so that \
         adding this check cannot change the local path"
    );
}
