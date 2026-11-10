use std::time::{Duration, Instant};

#[tokio::test]
#[ignore]
async fn a_sync_that_cannot_get_the_lock_gives_up_and_says_why() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let mut holder = pool.begin().await.expect("begin the blocking transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(memory_industry::handlers::sync::SYNC_LOCK)
        .execute(&mut *holder)
        .await
        .expect("hold the sync lock the way a running import would");

    // The wait is a parameter: production gives 10 s (pinned by a unit test in handlers/sync.rs), and
    // this test gives 1 s so that it does not sit out the production wait to prove the wait ends.
    let wait = Duration::from_secs(1);
    let mut waiter = pool.begin().await.expect("begin the waiting transaction");
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        wait * 5,
        memory_industry::handlers::sync::take_sync_lock(&mut waiter, wait),
    )
    .await;
    let elapsed = started.elapsed();

    let taken = outcome.unwrap_or_else(|_| {
        panic!(
            "still waiting for the lock after 5 × the wait: lock_timeout is not the one taking \
             effect. pg_advisory_xact_lock blocks forever by default, and a sync that collided \
             with another had no way to end: a hung handler is worse than a failed one, it holds \
             a connection and reports nothing"
        )
    });

    let why = match taken {
        Ok(()) => panic!("the lock was taken while another transaction held it"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        why.contains("another sync is holding the lock"),
        "and it has to say what happened, or the operator reads a bare Postgres timeout as a \
         database fault and goes looking in the wrong place. Got: {why}"
    );
    assert!(
        why.contains("waited 1s"),
        "the error names the wait it gave up after, which is the one it was given. Got: {why}"
    );
    assert!(
        elapsed >= wait,
        "it gave up after {elapsed:?}, before the {wait:?} it was told to wait: what ended it was \
         not the lock_timeout but some other, instant failure"
    );

    drop(waiter);
    drop(holder);
}
