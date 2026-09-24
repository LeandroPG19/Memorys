// needs-a-server: creates six scratch databases on one server and migrates them all at the same instant
//
//! Six databases of one server, migrated at the same instant, all come up.
//!
//! Migration 0041 creates or alters the role `cuba_app`, and a role belongs to
//! the whole server (`pg_authid`), while sqlx serialises migrators with an
//! advisory lock per database. Two databases of one server migrating together
//! could both write `cuba_app`'s row, and the one that lost failed with
//! `while executing migration 41: error returned from database: tuple
//! concurrently updated` (seen in v045_a_merge_reaches_the_database, which
//! migrates three at once). `provision_app_role` writes the same row with its
//! `ALTER ROLE ... PASSWORD`. In the field: two daemons, or two installs,
//! starting together against one server. `create_pool` now retries that error,
//! and only that one.
//!
//! This test is NOT deterministic and cannot be made so: whether two sessions
//! reach the same row of `pg_authid` in the same instant is up to the server's
//! scheduler. Six migrations released by one barrier make the collision
//! likely, not certain, so a green run of this file without the fix proves
//! nothing on its own. The determinism lives in the unit tests of `db.rs`: the
//! retry loop is driven by an attempt that fails with that very error N times,
//! and the classification is checked on errors built by hand. This file is the
//! check that the whole of it holds against a real server.
//!
//! Needs a server: `DATABASE_URL` names it, and every database written here is
//! one this file creates and drops (`common::in_a_scratch_database`).

mod common;

use common::in_a_scratch_database;
use std::sync::{Arc, Mutex};
use tokio::sync::Barrier;

const DATABASES: usize = 6;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn six_databases_of_one_server_migrated_at_once_all_come_up() {
    let start = Arc::new(Barrier::new(DATABASES));
    // A failure is written down instead of panicking inside its body, so every
    // other body still gets past the barrier and every database is dropped.
    let failures = Arc::new(Mutex::new(Vec::new()));

    let each = (0..DATABASES).map(|_| {
        let start = Arc::clone(&start);
        let failures = Arc::clone(&failures);
        in_a_scratch_database("brain_at_once", move |url| async move {
            start.wait().await;
            match memory_industry::db::create_pool(&url).await {
                Ok(pool) => pool.close().await,
                Err(why) => failures.lock().unwrap().push(format!("{why:#}")),
            }
        })
    });
    futures::future::join_all(each).await;

    let failures = failures.lock().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {DATABASES} databases migrated at the same instant did not come up. \
         `tuple concurrently updated` here is two of them writing cuba_app's row in \
         pg_authid, which belongs to the whole server: {failures:#?}",
        failures.len()
    );
}
