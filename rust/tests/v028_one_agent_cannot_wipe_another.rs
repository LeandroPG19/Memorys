use serde_json::{Value, json};
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

#[tokio::test]
#[ignore]
async fn an_agent_without_a_session_cannot_read_or_wipe_another_agents_scratchpad() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("wm_{}", &Uuid::new_v4().to_string()[..8]);
    let session: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_sessions (session_name) VALUES ('the other agent') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .expect("the other agent opened a session");
    memory_industry::session::set(session, None);

    call(
        &pool,
        "cuba_pizarra",
        json!({"action": "write", "content": format!("{marker} el plan de la otra IA")}),
    )
    .await;

    let mine = call(&pool, "cuba_pizarra", json!({"action": "read"})).await;
    assert!(
        serde_json::to_string(&mine)
            .expect("serialise")
            .contains(&marker),
        "the agent that wrote it has to see its own note: {mine}"
    );

    memory_industry::session::clear();

    let seen = call(&pool, "cuba_pizarra", json!({"action": "read"})).await;
    assert!(
        !serde_json::to_string(&seen)
            .expect("serialise")
            .contains(&marker),
        "an agent with no session of its own must not read another agent's working memory. The \
         filter is `($1::uuid IS NULL OR session_id = $1)`, and with no session that first arm \
         is true, so the whole condition passes and every row of every session comes back. Two \
         AIs against this daemon right now — cuba_pizarra is documented as a private scratchpad \
         and was not. Got: {seen}"
    );

    let wiped = call(&pool, "cuba_pizarra", json!({"action": "clear"})).await;
    let survivors: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_wm WHERE content LIKE $1")
        .bind(format!("%{marker}%"))
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        survivors, 1,
        "and it certainly must not DELETE it. Same filter, same always-true arm, no tag: that is \
         `DELETE FROM brain_wm` with nothing to stop it — one agent that never called \
         cuba_jornada wipes every other agent's plan mid-task. Reported: {wiped}"
    );

    memory_industry::session::set(session, None);
    let still_mine = call(&pool, "cuba_pizarra", json!({"action": "read"})).await;
    assert!(
        serde_json::to_string(&still_mine)
            .expect("serialise")
            .contains(&marker),
        "and the owner still sees it afterwards, or the fix works by hiding the note from \
         everybody, which is the same loss with better manners: {still_mine}"
    );

    sqlx::query("DELETE FROM brain_wm WHERE content LIKE $1")
        .bind(format!("%{marker}%"))
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_sessions WHERE id = $1")
        .bind(session)
        .execute(&pool)
        .await
        .ok();
    memory_industry::session::clear();
}
