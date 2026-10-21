// needs-a-server: migrates a scratch database it creates and drops, and writes to the scratchpad in it
//
//! The scratchpad forgets what expired, or `brain_wm` only ever grows.
//!
//! `cuba_pizarra` is a TTL-bounded buffer: every note is written with `expires_at`, and `read`
//! hides what is past it (`WHERE expires_at > NOW()`). Hiding is all that happened. The one
//! function that deletes expired rows, `pizarra::purge_expired`, had no caller anywhere (found
//! by the 0.32 audit looking for dead `pub` items, 2026-10-21), the REM cycle does not touch
//! the table, and nothing else in `src/` deletes from it except `clear`, which an agent calls on
//! its own notes. So a note that expired stayed in the table for the life of the database; on a
//! machine where agents write one every few turns that is a table with a TTL that behaves like
//! a log.
//!
//! The purge belongs to the write, not to a daemon: a handler runs inside the caller's scope
//! (`app.current_project`), so under row-level security it removes only rows it may see, and a
//! cycle that runs outside any scope could not be trusted to do the same. The cost is one
//! delete over `idx_wm_expires`.
//!
//! `DATABASE_URL` names the server. The database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the one the URL points at is never touched.

mod common;

use common::in_a_scratch_database;
use serde_json::json;

#[tokio::test]
async fn a_write_to_the_scratchpad_removes_the_notes_whose_time_ran_out() {
    in_a_scratch_database("brain_wm_purge", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");

        sqlx::query(
            "INSERT INTO brain_wm (content, ttl_seconds, expires_at) VALUES
                 ('expired an hour ago', 60, NOW() - interval '1 hour'),
                 ('expired a day ago',   60, NOW() - interval '1 day'),
                 ('still alive',       3600, NOW() + interval '1 hour')",
        )
        .execute(&pool)
        .await
        .expect("seeding the scratchpad");

        memory_industry::handlers::dispatch(
            &pool,
            "cuba_pizarra",
            json!({"action": "write", "content": "a new note", "ttl_seconds": 60}),
        )
        .await
        .expect("writing to the scratchpad");

        let kept: Vec<String> = sqlx::query_scalar("SELECT content FROM brain_wm ORDER BY content")
            .fetch_all(&pool)
            .await
            .expect("reading the table back");
        assert_eq!(
            kept,
            ["a new note", "still alive"],
            "after a write, the table must hold the note just written and the one still inside \
             its TTL, and nothing that expired: `read` hides expired notes but nothing deletes \
             them, so `brain_wm` grows for ever. Left in the table: {kept:?}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_failed_write_does_not_cost_the_notes_that_expired_nothing() {
    in_a_scratch_database("brain_wm_refused", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");

        sqlx::query(
            "INSERT INTO brain_wm (content, ttl_seconds, expires_at)
             VALUES ('still alive', 3600, NOW() + interval '1 hour')",
        )
        .execute(&pool)
        .await
        .expect("seeding the scratchpad");

        let refused = memory_industry::handlers::dispatch(
            &pool,
            "cuba_pizarra",
            json!({"action": "write", "content": "ttl is wrong", "ttl_seconds": 0}),
        )
        .await;
        assert!(refused.is_err(), "a ttl of zero is refused");

        let alive: i64 =
            sqlx::query_scalar("SELECT count(*) FROM brain_wm WHERE content = 'still alive'")
                .fetch_one(&pool)
                .await
                .expect("counting");
        assert_eq!(
            alive, 1,
            "the purge deletes by `expires_at <= NOW()` and by nothing else: a refused write \
             must leave every live note where it was"
        );
        pool.close().await;
    })
    .await;
}
