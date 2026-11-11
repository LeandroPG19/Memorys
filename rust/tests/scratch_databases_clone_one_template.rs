// needs-a-server: clones sixteen scratch databases at once from the migrated template and creates one empty database, on the server DATABASE_URL names
//
//! A scratch database arrives migrated, cloned from one template, and no test migrates in parallel.
//!
//! Before, `common::in_a_scratch_database` created an empty database and every body migrated it
//! with `db::create_pool`. Migration 0041 writes `cuba_app`'s row in `pg_authid`, which belongs to
//! the whole server, while the sqlx lock is per database: two bodies migrating together made the
//! slower one die with `tuple concurrently updated` (v029_accounts_storage, 12 bodies, on
//! 518f35c). Measured 2026-10-14 with 16 sessions writing one role row, 91 % (Windows) and 93 %
//! (Linux) of the transactions lost, and `retry_on_catalog_race` (5 attempts) did not cover it.
//!
//! The way out is one migration per server and per set of migrations, kept as the sealed template
//! `test_tpl_<12 hex>`, and a clone of it per test. What this file pins is structural, not a race
//! that may or may not fire today:
//!   - a body finds `_sqlx_migrations` complete and NO row installed after the test started, so if
//!     the helper ever migrates again this is red whether or not the collision happens that day;
//!   - two clones are two installations (the template seeds `node_id`, `host_id` and the layer
//!     ids with `gen_random_uuid()`, and a plain clone would share them);
//!   - there is exactly one template, it takes no connections, and nobody is connected to it
//!     (a clone from a template with a session on it waits 5 s and fails with 55006, measured);
//!   - the builder waits for the last session to leave the database it is building before it
//!     renames it (CI-E21: on 10-nov-2026 two builds in a row failed their RENAME with 55006
//!     under load): `common::wait_until_no_session_is_left` outlasts a session that leaves late
//!     and, when one never leaves, names it;
//!   - a pool closed while one of its connections is being returned leaves no session on the
//!     sealed database (CI-E32: sqlx 0.8.6's `Pool::close()` returns with that connection open in
//!     the idle queue, and the builder's wait ran out on it twice on 10-nov-2026);
//!   - `empty_db::in_an_empty_database` still hands out a database with no schema at all, which
//!     v045_databases_of_one_server_migrate_at_once, v044_* and v043_a_model_of_another_width need.
//!
//! Needs a server: `DATABASE_URL` names it, and every database written here other than the
//! template is one the helpers create and drop.

mod common;
mod empty_db;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::in_a_scratch_database;
use empty_db::in_an_empty_database;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection, PgPool};
use tokio::sync::Notify;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

const DATABASES: usize = 16;

/// Sixteen clones at once took 0.50 s on the Windows judge's server and 1.48 s on the Linux one
/// (fsync on), measured 2026-10-14; sixteen migrations from empty in a debug build, in series,
/// take ~8-13 s, and in parallel they race on cuba_app's row.
const SIXTEEN_AT_ONCE_CEILING: Duration = Duration::from_secs(6);

/// The columns a clone has to re-roll: the identities the migrations seed with a random default.
const IDENTITIES: [&str; 3] = [
    "brain_node_identity.node_id",
    "brain_host_identity.host_id",
    "brain_memory_layers.layer_id",
];

fn database_url() -> String {
    std::env::var("DATABASE_URL").expect(
        "DATABASE_URL is required: this test creates and drops its own databases on the server \
         that URL names",
    )
}

fn sibling_url(url: &str, name: &str) -> String {
    let cut = url.rfind('/').expect("a database URL ends in /<name>");
    format!("{}/{name}", &url[..cut])
}

/// Builds the template if no earlier test of this process did; the clones below must not pay for
/// it, and the second test must find it.
async fn warm_the_template() {
    in_a_scratch_database("brain_tpl_warm", |_url| async {}).await;
}

fn the_migrations_this_binary_embeds() -> (i64, i64) {
    let ups: Vec<i64> = MIGRATOR
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .map(|m| m.version)
        .collect();
    let newest = *ups.iter().max().expect("this binary embeds migrations");
    (
        newest,
        i64::try_from(ups.len()).expect("a handful of migrations"),
    )
}

/// What one clone shows, read without `create_pool`: a body that migrated would hide the very
/// thing under test.
struct Seen {
    migrations: (Option<i64>, i64),
    installed_after_the_start: i64,
    identities: Vec<(String, String)>,
}

async fn what_a_clone_holds(url: &str, started: &str) -> Result<Seen, String> {
    let pool = PgPool::connect(url)
        .await
        .map_err(|e| format!("connecting to the clone: {e}"))?;
    let seen = read_a_clone(&pool, started).await;
    pool.close().await;
    seen
}

async fn read_a_clone(pool: &PgPool, started: &str) -> Result<Seen, String> {
    let migrations: (Option<i64>, i64) =
        sqlx::query_as("SELECT max(version), count(*) FROM _sqlx_migrations")
            .fetch_one(pool)
            .await
            .map_err(|e| format!("arrived empty, there is no _sqlx_migrations: {e}"))?;
    let installed_after_the_start: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM _sqlx_migrations WHERE installed_on >= $1::text::timestamptz",
    )
    .bind(started)
    .fetch_one(pool)
    .await
    .map_err(|e| format!("reading installed_on: {e}"))?;

    let columns: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.table_name::text, c.column_name::text \
         FROM information_schema.columns c \
         JOIN information_schema.tables t \
           ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
          AND t.table_type = 'BASE TABLE' \
         WHERE c.table_schema = 'public' AND c.column_default = 'gen_random_uuid()' \
         ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("listing the columns with a random default: {e}"))?;
    let mut identities = Vec::new();
    for (table, column) in columns {
        let values: Vec<Option<String>> =
            sqlx::query_scalar(&format!("SELECT \"{column}\"::text FROM \"{table}\""))
                .fetch_all(pool)
                .await
                .map_err(|e| format!("reading {table}.{column}: {e}"))?;
        identities.extend(
            values
                .into_iter()
                .flatten()
                .map(|value| (format!("{table}.{column}"), value)),
        );
    }
    Ok(Seen {
        migrations,
        installed_after_the_start,
        identities,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
async fn sixteen_scratch_databases_at_once_arrive_migrated_and_none_of_them_ran_a_migration() {
    warm_the_template().await;

    let mut maintenance = PgConnection::connect(&sibling_url(&database_url(), "postgres"))
        .await
        .expect("connecting to the maintenance database");
    // The server's clock, so the comparison with `installed_on` has no skew.
    let started: String = sqlx::query_scalar("SELECT clock_timestamp()::text")
        .fetch_one(&mut maintenance)
        .await
        .expect("reading the server's clock");
    maintenance.close().await.expect("closing the connection");

    let seen = Arc::new(Mutex::new(Vec::new()));
    // A failure is written down instead of panicking inside its body, so every database is dropped.
    let failures = Arc::new(Mutex::new(Vec::new()));
    let began = Instant::now();
    let each = (0..DATABASES).map(|_| {
        let seen = Arc::clone(&seen);
        let failures = Arc::clone(&failures);
        let started = started.clone();
        in_a_scratch_database("brain_tpl_at_once", move |url| async move {
            match what_a_clone_holds(&url, &started).await {
                Ok(one) => seen.lock().unwrap().push(one),
                Err(why) => failures.lock().unwrap().push(why),
            }
        })
    });
    futures::future::join_all(each).await;
    let took = began.elapsed();

    let failures = failures.lock().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {DATABASES} scratch databases did not arrive migrated. A scratch database has to \
         arrive migrated: the body used to migrate it, and sixteen bodies migrating at once is \
         the `tuple concurrently updated` on cuba_app's row in pg_authid (migration 0041), which \
         v029_accounts_storage hit on 518f35c: {failures:#?}",
        failures.len()
    );
    let seen = seen.lock().unwrap();
    let (newest, count) = the_migrations_this_binary_embeds();
    let wrong: Vec<_> = seen
        .iter()
        .map(|one| one.migrations)
        .filter(|got| *got != (Some(newest), count))
        .collect();
    assert!(
        wrong.is_empty(),
        "every clone has to hold the {count} up migrations this binary embeds, newest {newest}: \
         {wrong:?}"
    );

    let installed_late: i64 = seen.iter().map(|one| one.installed_after_the_start).sum();
    assert_eq!(
        installed_late, 0,
        "a clone does not migrate: {installed_late} rows of _sqlx_migrations were installed after \
         the test started, so the helper migrated again, and that is the race on cuba_app's row"
    );

    let mut by_column: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for one in seen.iter() {
        for (column, value) in &one.identities {
            by_column
                .entry(column.as_str())
                .or_default()
                .push(value.as_str());
        }
    }
    for column in IDENTITIES {
        assert!(
            by_column.contains_key(column),
            "{column} was not found with a gen_random_uuid() default and a row in a clone; the \
             check below would pass over nothing. Seen: {:?}",
            by_column.keys().collect::<Vec<_>>()
        );
    }
    for (column, values) in &by_column {
        let distinct: BTreeSet<_> = values.iter().collect();
        assert_eq!(
            distinct.len(),
            values.len(),
            "{column} repeats between scratch databases. Two scratch databases are two \
             installations; cloned as they are, every one would carry the template's node and \
             host id (v029_bridge j4, v045_a_merge)"
        );
    }

    assert!(
        took < SIXTEEN_AT_ONCE_CEILING,
        "{DATABASES} scratch databases at once took {took:?}, over {SIXTEEN_AT_ONCE_CEILING:?}; \
         clones took 0.50 s on the Windows server and 1.48 s on the Linux one"
    );
}

#[tokio::test]
async fn the_template_is_sealed_and_there_is_exactly_one() {
    warm_the_template().await;

    let url = sibling_url(&database_url(), "postgres");
    let mut maintenance = PgConnection::connect(&url)
        .await
        .expect("connecting to the maintenance database");
    let templates: Vec<(String, bool)> = sqlx::query_as(
        "SELECT datname::text, datallowconn FROM pg_database \
         WHERE datname LIKE 'test\\_tpl\\_%' ORDER BY datname",
    )
    .fetch_all(&mut maintenance)
    .await
    .expect("listing the templates");

    let sealed: Vec<&str> = templates
        .iter()
        .filter(|(_, accepts)| !accepts)
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(
        sealed.len(),
        1,
        "exactly one test_tpl_% accepts no connections once a scratch database was handed out; \
         ALLOW_CONNECTIONS false is what keeps autovacuum and every other session out, and a \
         template with a session makes CREATE DATABASE wait 5 s and fail with 55006 (`source \
         database is being accessed by other users`): {templates:?}"
    );
    let name = sealed[0];
    let strays: Vec<&str> = templates
        .iter()
        .map(|(other, _)| other.as_str())
        .filter(|other| !other.starts_with(name))
        .collect();
    assert!(
        strays.is_empty(),
        "a template of another key was not swept: {strays:?} next to {name}"
    );

    let refused = match PgConnection::connect(&sibling_url(&url, name)).await {
        Ok(_) => panic!("{name} let a connection in; a sealed template accepts none"),
        Err(why) => why,
    };
    let code = refused
        .as_database_error()
        .and_then(|why| why.code())
        .map(|code| code.into_owned());
    assert_eq!(
        code.as_deref(),
        Some("55000"),
        "connecting to the sealed template has to fail with 'not currently accepting \
         connections': {refused}"
    );

    let backends: i32 =
        sqlx::query_scalar("SELECT numbackends FROM pg_stat_database WHERE datname = $1")
            .bind(name)
            .fetch_one(&mut maintenance)
            .await
            .expect("reading the backends of the template");
    assert_eq!(
        backends, 0,
        "{name} has {backends} sessions; a clone would wait 5 s on them and fail with 55006"
    );
}

#[tokio::test]
async fn an_empty_scratch_database_has_no_schema_at_all() {
    in_an_empty_database("brain_tpl_empty", |url| async move {
        let pool = PgPool::connect(&url)
            .await
            .expect("connecting to the empty database");
        let migrations: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
                .fetch_one(&pool)
                .await
                .expect("looking for _sqlx_migrations");
        let tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public'",
        )
        .fetch_one(&pool)
        .await
        .expect("counting the tables");
        pool.close().await;

        assert_eq!(
            migrations, None,
            "an empty scratch database has no _sqlx_migrations: v045_databases_of_one_server_\
             migrate_at_once and v044_* test the migration from scratch"
        );
        assert_eq!(
            tables, 0,
            "an empty scratch database has no table in public: v043_a_model_of_another_width \
             builds its own schema, and a stray vector column would break it"
        );
    })
    .await;
}

/// The name of the database a body was handed: what follows the last `/` of its URL.
fn database_of(url: &str) -> String {
    let cut = url.rfind('/').expect("a database URL ends in /<name>");
    url[cut + 1..].to_owned()
}

/// Polls until the waiter's connection has run its first look at `pg_stat_activity`: its last
/// query shows there once it did. This is the synchronisation of the first test, whatever the
/// machine's speed; the deadline only turns a helper that never looks into a failure.
async fn wait_until_the_waiter_has_looked(observer: &mut PgConnection) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let looked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
             WHERE application_name = 'e21_waiter' AND query LIKE '%pg_stat_activity%')",
        )
        .fetch_one(&mut *observer)
        .await
        .expect("reading pg_stat_activity");
        if looked {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the wait never ran its first look at pg_stat_activity within 10 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn the_wait_for_an_empty_database_outlasts_a_session_and_ends_when_it_leaves() {
    in_an_empty_database("brain_tpl_wait", |url| async move {
        let database = database_of(&url);
        let maintenance = sibling_url(&url, "postgres");
        let holder = PgConnection::connect(&url)
            .await
            .expect("connecting the session that stays");
        let mut waiter = PgConnection::connect(&maintenance)
            .await
            .expect("connecting the waiter");
        sqlx::query("SET application_name = 'e21_waiter'")
            .execute(&mut waiter)
            .await
            .expect("naming the waiter");
        let mut observer = PgConnection::connect(&maintenance)
            .await
            .expect("connecting the observer");

        let waiting = tokio::spawn(async move {
            common::wait_until_no_session_is_left(&mut waiter, &database, Duration::from_secs(60))
                .await
        });
        wait_until_the_waiter_has_looked(&mut observer).await;
        assert!(
            !waiting.is_finished(),
            "the wait ended while a session was still connected to the database: it looked at \
             pg_stat_activity once, saw it, and the holder was there"
        );

        holder
            .close()
            .await
            .expect("closing the session that stays");
        tokio::time::timeout(Duration::from_secs(30), waiting)
            .await
            .expect("the wait did not end within 30 s of the last session leaving")
            .expect("the waiting task panicked")
            .expect("the wait has to end well once the database has no session");
    })
    .await;
}

#[tokio::test]
async fn a_session_that_never_leaves_is_named_when_the_time_runs_out() {
    in_an_empty_database("brain_tpl_stays", |url| async move {
        let database = database_of(&url);
        let mut holder = PgConnection::connect(&url)
            .await
            .expect("connecting the session that stays");
        sqlx::query("SET application_name = 'e21_stays'")
            .execute(&mut holder)
            .await
            .expect("naming the session that stays");
        let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut holder)
            .await
            .expect("reading the pid of the session that stays");
        let mut admin = PgConnection::connect(&sibling_url(&url, "postgres"))
            .await
            .expect("connecting the admin");

        let within = Duration::from_millis(300);
        let began = Instant::now();
        let who = common::wait_until_no_session_is_left(&mut admin, &database, within)
            .await
            .expect_err("a session is connected for the whole time: the wait cannot end well");
        assert!(
            began.elapsed() >= within,
            "the wait gave up before its time ran out: {who}"
        );
        for part in [
            format!("pid {holder_pid} "),
            "client backend".to_owned(),
            "e21_stays".to_owned(),
        ] {
            assert!(
                who.contains(&part),
                "the error has to name who is still connected, and it lacks `{part}`: {who}"
            );
        }

        holder
            .close()
            .await
            .expect("closing the session that stays");
    })
    .await;
}

/// CI-E32. A connection that is being returned when the pool closes is the one sqlx 0.8.6's
/// `Pool::close()` leaves behind: the connection goes into the idle queue just before it releases
/// its permit, that permit is what lets `close()` finish, and `close()` does not empty the queue
/// again. The connection then stays open for as long as the `Pool` lives. This test builds that
/// order with two `Notify`: the pool's `after_release` says it entered and waits, the test closes
/// the pool, and only when the pool says it is closed (`close_event`) does it let the hook go.
/// No clock orders anything (the 30 s is only how long a failure waits before it is named), and
/// the pool is not cloned to watch it: a clone alive would hold the session open with the fix too.
#[tokio::test]
async fn a_pool_closed_while_a_connection_returns_leaves_no_session_on_the_sealed_database() {
    in_an_empty_database("brain_tpl_returning", |url| async move {
        let database = database_of(&url);
        let mut admin = PgConnection::connect(&sibling_url(&url, "postgres"))
            .await
            .expect("connecting the admin");

        let entered = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());
        let (hook_entered, hook_gate) = (Arc::clone(&entered), Arc::clone(&gate));
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .after_release(move |_conn, _meta| {
                let (entered, gate) = (Arc::clone(&hook_entered), Arc::clone(&hook_gate));
                Box::pin(async move {
                    entered.notify_one();
                    gate.notified().await;
                    Ok(true)
                })
            })
            .connect(&url)
            .await
            .expect("opening the pool");

        drop(pool.acquire().await.expect("taking the only connection"));
        tokio::time::timeout(Duration::from_secs(30), entered.notified())
            .await
            .expect("the returned connection never reached after_release");
        let on_the_database: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname = $1")
                .bind(&database)
                .fetch_one(&mut admin)
                .await
                .expect("counting the sessions on the database");
        assert_eq!(
            on_the_database, 1,
            "the connection that is being returned is the one session on the database; without \
             it the test is not holding the race it exists to hold"
        );

        let closed = pool.close_event();
        let sealing = common::close_and_seal(pool, &mut admin, &database, Duration::from_secs(30));
        let let_the_hook_go = async {
            closed.await;
            gate.notify_one();
        };
        let (outcome, ()) = tokio::join!(sealing, let_the_hook_go);
        assert_eq!(
            outcome,
            Ok(()),
            "closing the pool while a connection was being returned left a session on the sealed \
             database, so the template could not be published: sqlx 0.8.6's `Pool::close()` \
             returns with that connection open in the idle queue, and it lives until the last \
             `Pool` is dropped"
        );

        let accepts: bool =
            sqlx::query_scalar("SELECT datallowconn FROM pg_database WHERE datname = $1")
                .bind(&database)
                .fetch_one(&mut admin)
                .await
                .expect("reading datallowconn");
        assert!(
            !accepts,
            "{database} was left accepting connections: close_and_seal has to seal it"
        );
    })
    .await;
}
