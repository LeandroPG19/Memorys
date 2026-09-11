use sqlx::postgres::PgListener;
use uuid::Uuid;

async fn subscribed(url: &str) -> PgListener {
    let mut listener = PgListener::connect(url)
        .await
        .expect("a listener connection of its own, outside the pool");
    listener
        .listen(memory_industry::protocol::SYNC_CLOCK_CHANNEL)
        .await
        .expect("subscribe to the sync clock channel");
    listener
}

#[tokio::test]
#[ignore]
async fn a_real_edit_wakes_the_listener_and_decay_does_not() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let entity_id = Uuid::new_v4();
    let obs_id = Uuid::new_v4();
    let marker = format!("hear_{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query("INSERT INTO brain_observations (id, entity_id, content) VALUES ($1, $2, $3)")
        .bind(obs_id)
        .bind(entity_id)
        .bind(format!("{marker} lo que habia antes"))
        .execute(&pool)
        .await
        .expect("seed the observation");

    let mut listener = subscribed(&url).await;

    sqlx::query("UPDATE brain_observations SET content = $2 WHERE id = $1")
        .bind(obs_id)
        .bind(format!("{marker} lo que dice ahora"))
        .execute(&pool)
        .await
        .expect("edit the content");

    let heard = tokio::time::timeout(std::time::Duration::from_secs(10), listener.recv())
        .await
        .expect(
            "nothing arrived in ten seconds. Without this the only way a peer learns of a change \
             is by asking on a timer, and the difference between real time and a timer is the \
             whole point of the phase",
        )
        .expect("the listener connection stayed up");

    assert_eq!(
        heard.payload(),
        format!("brain_observations:{obs_id}"),
        "the payload carries an identifier and nothing else: NOTIFY has a hard 8000-byte limit \
         that no setting moves, and an observation's content routinely goes past it, so sending \
         the text would turn an ordinary write into a failed transaction"
    );

    sqlx::query("UPDATE brain_observations SET importance = importance + 0.01 WHERE id = $1")
        .bind(obs_id)
        .execute(&pool)
        .await
        .expect("move only the telemetry, the way decay does");

    let quiet = tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv()).await;
    assert!(
        quiet.is_err(),
        "moving importance must NOT wake anybody. The REM cycle rewrites importance on 1097 of \
         1880 rows every four hours; if that notified, this machine would wake its peer six \
         times a day to announce that nothing changed, and the peer would fetch a bundle that \
         inserts zero rows. The column-specific trigger from 0047 is what makes the difference, \
         and this assertion is what stops somebody widening it later. Got: {quiet:?}"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&pool)
        .await
        .ok();
}
