use serde_json::{Value, json};
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

const TRIGGERS_LOCK: i64 = 0x0CBA_A0D1_7106_0029;

async fn own_the_trigger_table(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(TRIGGERS_LOCK)
        .execute(&mut *tx)
        .await
        .expect(
            "the cap is global across on_session_start rows, so the test that fills it has to \
             own the table while it does — otherwise its DELETE takes the note the sibling test \
             just left, which is what turned this file red in the gate and never locally, where \
             it was run one thread at a time",
        );
    tx
}

#[tokio::test]
#[ignore]
async fn a_note_left_for_another_agent_arrives_when_it_opens_its_session() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_trigger_table(&pool).await;

    let marker = format!("recado_{}", &Uuid::new_v4().to_string()[..8]);

    call(
        &pool,
        "cuba_centinela",
        json!({
            "action": "create",
            "entity_pattern": marker,
            "condition_type": "on_session_start",
            "message": format!("{marker} el pool se agota a 40 conexiones, no toques ese modulo"),
            "max_fires": 1
        }),
    )
    .await;

    let opened = call(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": marker}),
    )
    .await;
    let session = opened["session"]["id"]
        .as_str()
        .expect("jornada start returns the session")
        .to_string();

    let delivered = serde_json::to_string(&opened["triggered_reminders"]).expect("serialise");
    assert!(
        delivered.contains(&marker),
        "the note has to arrive when the other agent opens its session, which is the only \
         moment it is guaranteed to be reading. This needed no new table and no new tool: \
         cuba_centinela already stores a message, jornada start already delivers it, and \
         max_fires already closes it — the same three things the peer inbox needed and that a \
         local note has no bundle to trigger. Got: {opened}"
    );

    let closed = call(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": marker}),
    )
    .await;
    let again = serde_json::to_string(&closed["triggered_reminders"]).expect("serialise");
    assert!(
        !again.contains(&marker),
        "and it must not arrive twice. max_fires is what closes a local note, since there is no \
         bundle hash to match it against the way a peer notice is closed. Without that it would \
         be redelivered at every session start until it expired. Got: {closed}"
    );

    let second: Uuid =
        sqlx::query_scalar("INSERT INTO brain_sessions (session_name) VALUES ($1) RETURNING id")
            .bind(format!("{marker}_2"))
            .fetch_one(&pool)
            .await
            .expect("a second session for cleanup");

    sqlx::query("DELETE FROM brain_triggers WHERE entity_pattern = $1")
        .bind(&marker)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_sessions WHERE id = $1::uuid OR id = $2")
        .bind(&session)
        .bind(second)
        .execute(&pool)
        .await
        .ok();
    memory_industry::session::clear();
}

#[tokio::test]
#[ignore]
async fn the_notes_cannot_grow_without_end() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_trigger_table(&pool).await;
    sqlx::query("DELETE FROM brain_triggers WHERE condition_type = 'on_session_start'")
        .execute(&pool)
        .await
        .expect("start from an empty inbox");

    sqlx::query(
        "INSERT INTO brain_triggers (entity_pattern, condition_type, message)
         VALUES ('cualquiera', 'on_access', 'un recordatorio de entidad de los de siempre')",
    )
    .execute(&pool)
    .await
    .expect("an entity reminder, seeded FIRST so it is the oldest row in the table");

    for n in 0..230 {
        sqlx::query(
            "INSERT INTO brain_triggers (entity_pattern, condition_type, message)
             VALUES ('flood', 'on_session_start', $1)",
        )
        .bind(format!("aviso numero {n}"))
        .execute(&pool)
        .await
        .expect("insert a note");
    }

    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_triggers WHERE condition_type = 'on_session_start'",
    )
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(
        kept, 200,
        "every other inbox in this schema has a ceiling — peer notices at 200, handler failures \
         at 1000 — for the same reason: a table one caller can write grows fastest exactly when \
         something is looping, which is the worst moment to also fill the disk. 230 went in and \
         {kept} stayed"
    );

    let reminder: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_triggers WHERE entity_pattern = 'cualquiera'",
    )
    .fetch_one(&pool)
    .await
    .expect("count");
    assert_eq!(
        reminder, 1,
        "and the cap must leave the entity reminders alone. It is seeded first on purpose: a cap \
         that ordered by date across the whole table would delete the OLDEST row, which is \
         exactly this one — somebody's deliberate reminder destroyed by somebody else's noise. \
         An earlier version of this test inserted it last and could not see that at all"
    );

    sqlx::query("DELETE FROM brain_triggers WHERE entity_pattern IN ('flood', 'cualquiera')")
        .execute(&pool)
        .await
        .ok();
}
