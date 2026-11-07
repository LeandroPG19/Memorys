// needs-a-server: migrates scratch databases to every published cut and on to head, and compares each with one migrated from scratch
//
//! A database that already has a schema and rows must come up under the binary
//! that ships now. The throwaway database of the gate is created from scratch,
//! so a migration that only works on an empty database passed every run until
//! this file: an `ADD COLUMN ... NOT NULL` on a table with rows, or an index
//! that only a populated table gets, lands on the server that already runs the
//! previous version and not on the gate's.
//!
//! The cuts are the last migration of each published version (`PUBLISHED`,
//! read with `git ls-tree -r --name-only <tag> -- rust/migrations`) and every
//! migration after the last one published, read from the binary at run time.
//! For each cut: an empty database is migrated up to the cut, given one row in
//! `brain_entities`, `brain_observations` and `brain_relations`, and then
//! migrated with the whole binary. The rows must survive, `_sqlx_migrations`
//! must hold every version, and the schema (columns with type, nullability and
//! default; indexes; constraints, read from the catalog) must equal the schema
//! of a database migrated from scratch.
//!
//! The seed uses only the columns of 0001_initial_schema and the NOT NULL
//! columns added later all carry a DEFAULT, so one seed is valid at every cut.

mod empty_db;

use empty_db::in_an_empty_database;
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::{Connection, PgConnection};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

/// The last migration of each published version: the last `*.up.sql` of the
/// tag's `rust/migrations`. v0.3.0, v0.6.0 and v0.7.0 have none and are not here.
const PUBLISHED: [(&str, i64); 26] = [
    ("v0.9.3", 17),
    ("v0.10.0", 25),
    ("v0.11.0", 33),
    ("v0.11.1", 33),
    ("v0.11.2", 33),
    ("v0.12.0", 33),
    ("v0.13.0", 33),
    ("v0.13.1", 33),
    ("v0.14.0", 34),
    ("v0.14.1", 34),
    ("v0.15.0", 35),
    ("v0.16.0", 38),
    ("v0.17.0", 39),
    ("v0.17.1", 39),
    ("v0.18.0", 39),
    ("v0.19.0", 39),
    ("v0.20.0", 39),
    ("v0.21.0", 42),
    ("v0.22.0", 54),
    ("v0.23.0", 57),
    ("v0.24.0", 59),
    ("v0.25.0", 62),
    ("v0.27.0", 64),
    ("v0.28.0", 64),
    ("v0.29.0", 65),
    ("v0.30.0", 65),
];

/// Two entities, an observation on each and a relation between them.
const SEED: [&str; 3] = [
    "INSERT INTO brain_entities (name) VALUES ('v046-a'), ('v046-b')",
    "INSERT INTO brain_observations (entity_id, content) \
     SELECT id, 'v046 observation of ' || name FROM brain_entities",
    "INSERT INTO brain_relations (from_entity, to_entity, relation_type) \
     SELECT a.id, b.id, 'v046' FROM brain_entities a \
     JOIN brain_entities b ON a.name = 'v046-a' AND b.name = 'v046-b'",
];

/// The rows `SEED` leaves, as (what to count, how many).
const SEEDED: [(&str, i64); 3] = [
    (
        "SELECT count(*) FROM brain_entities WHERE name LIKE 'v046-%'",
        2,
    ),
    (
        "SELECT count(*) FROM brain_observations WHERE content LIKE 'v046 observation%'",
        2,
    ),
    (
        "SELECT count(*) FROM brain_relations WHERE relation_type = 'v046'",
        1,
    ),
];

/// One line per column, index and constraint of the public schema, sorted, so
/// that a column added by ALTER (last in the table) and one from CREATE (in
/// its place) compare equal.
const SCHEMA_LINES: &str = "\
SELECT line FROM (
    SELECT 'column ' || table_name::text || '.' || column_name::text || ' '
           || data_type::text || '/' || udt_name::text
           || ' nullable=' || is_nullable::text
           || ' default=' || coalesce(column_default::text, '-') AS line
      FROM information_schema.columns
     WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'
    UNION ALL
    SELECT 'index ' || indexdef::text
      FROM pg_indexes
     WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations'
    UNION ALL
    SELECT 'constraint ' || conrelid::regclass::text || ' ' || conname::text
           || ' ' || pg_get_constraintdef(oid)
      FROM pg_constraint
     WHERE connamespace = 'public'::regnamespace
       AND conrelid <> '_sqlx_migrations'::regclass
) AS schema_lines
ORDER BY line";

/// Every cut to migrate to: the distinct cuts of `PUBLISHED`, and each version
/// of this binary after the last one published. A published cut that is not a
/// migration of this binary is refused, since migrating to it would do nothing.
fn cuts() -> Vec<i64> {
    let mut cuts: Vec<i64> = PUBLISHED.iter().map(|(_, cut)| *cut).collect();
    for cut in &cuts {
        assert!(
            MIGRATOR.iter().any(|m| m.version == *cut),
            "PUBLISHED names cut {cut}, which is not a migration of this binary: the table is stale"
        );
    }
    let last = cuts.iter().copied().max().expect("PUBLISHED lists cuts");
    cuts.extend(MIGRATOR.iter().map(|m| m.version).filter(|v| *v > last));
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

/// What a migrated database looks like: the schema lines and the versions it
/// records in `_sqlx_migrations`.
#[derive(Debug, PartialEq)]
struct Shape {
    schema: Vec<String>,
    versions: Vec<i64>,
}

async fn shape_of(pool: &sqlx::PgPool) -> Shape {
    let schema = sqlx::query_scalar::<_, String>(SCHEMA_LINES)
        .fetch_all(pool)
        .await
        .expect("reading the schema from the catalog");
    let mut versions = sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations")
        .fetch_all(pool)
        .await
        .expect("reading _sqlx_migrations");
    versions.sort_unstable();
    Shape { schema, versions }
}

/// Runs `migrator` on `conn`. `run` is not used: on a connection it asks for
/// `Acquire<'a>` for every lifetime, and this future is spawned. The box is
/// the same device db.rs uses (`realign_then_migrate`), so Send is proven
/// for the trait object and not for the whole async block.
async fn run<'a>(migrator: &'a Migrator, conn: &'a mut PgConnection) -> Result<(), MigrateError> {
    let run: Pin<Box<dyn Future<Output = Result<(), MigrateError>> + Send + 'a>> =
        Box::pin(migrator.run_direct(conn));
    run.await
}

/// The migrations of this binary up to and including `cut`, and nothing else.
fn up_to(cut: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|m| m.version <= cut)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

/// The problems with an empty database migrated to `cut`, seeded, and then
/// migrated with the whole binary, compared with `reference`. Empty when none.
async fn cut_then_head(url: String, cut: i64, reference: &Shape) -> Vec<String> {
    let mut conn = PgConnection::connect(&url)
        .await
        .expect("connecting to the scratch database");
    if let Err(why) = run(&up_to(cut), &mut conn).await {
        return vec![format!("does not migrate to its own cut: {why}")];
    }
    for statement in SEED {
        sqlx::query(statement)
            .execute(&mut conn)
            .await
            .unwrap_or_else(|why| panic!("seeding {statement:?} at cut {cut}: {why}"));
    }
    drop(conn);

    let pool = memory_industry::db::create_admin_pool(&url)
        .await
        .expect("migrating the seeded database with the whole binary");
    let mut problems = Vec::new();
    for (count, want) in SEEDED {
        let got: i64 = sqlx::query_scalar(count)
            .fetch_one(&pool)
            .await
            .expect("counting the seeded rows");
        if got != want {
            problems.push(format!(
                "seeded rows went missing: `{count}` gives {got}, not {want}"
            ));
        }
    }
    let shape = shape_of(&pool).await;
    pool.close().await;

    if shape.versions != reference.versions {
        problems.push(format!(
            "_sqlx_migrations holds {:?}, a database migrated from scratch holds {:?}",
            shape.versions, reference.versions
        ));
    }
    let only_fresh: Vec<&String> = reference
        .schema
        .iter()
        .filter(|line| !shape.schema.contains(line))
        .collect();
    let only_here: Vec<&String> = shape
        .schema
        .iter()
        .filter(|line| !reference.schema.contains(line))
        .collect();
    if !only_fresh.is_empty() || !only_here.is_empty() {
        problems.push(format!(
            "the schema differs from one migrated from scratch. Only in a fresh database: \
             {only_fresh:#?}. Only here: {only_here:#?}"
        ));
    }
    problems
}

#[tokio::test]
async fn a_database_at_any_cut_comes_up_to_head_with_its_rows_and_the_schema_of_a_fresh_one() {
    let fresh: Arc<Mutex<Option<Shape>>> = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&fresh);
    in_an_empty_database("v046_fresh", move |url| async move {
        let pool = memory_industry::db::create_admin_pool(&url)
            .await
            .expect("migrating an empty database from scratch");
        let shape = shape_of(&pool).await;
        pool.close().await;
        *slot.lock().unwrap() = Some(shape);
    })
    .await;
    let reference = Arc::new(
        fresh
            .lock()
            .unwrap()
            .take()
            .expect("the database migrated from scratch was read"),
    );

    // `migrate!` keeps both halves of every reversible pair: `.down.sql` shares
    // the version of its `.up.sql`. _sqlx_migrations records one row per version,
    // so only the up halves can be the versions a migrated database holds.
    let want_versions: Vec<i64> = {
        let mut versions: Vec<i64> = MIGRATOR
            .iter()
            .filter(|m| m.migration_type.is_up_migration())
            .map(|m| m.version)
            .collect();
        versions.sort_unstable();
        versions
    };
    assert!(
        !reference.schema.is_empty() && reference.versions == want_versions,
        "a database migrated from scratch has {} schema lines and versions {:?}, not the {} \
         migrations of this binary: the comparison below would prove nothing",
        reference.schema.len(),
        reference.versions,
        want_versions.len()
    );

    let cuts = cuts();
    let failures = Arc::new(Mutex::new(Vec::<String>::new()));
    for cut in &cuts {
        let cut = *cut;
        let failures = Arc::clone(&failures);
        let reference = Arc::clone(&reference);
        in_an_empty_database("v046_cut", move |url| async move {
            let problems = cut_then_head(url, cut, &reference).await;
            failures
                .lock()
                .unwrap()
                .extend(problems.into_iter().map(|p| format!("cut {cut}: {p}")));
        })
        .await;
    }

    let failures = failures.lock().unwrap();
    assert!(
        failures.is_empty(),
        "{} of {} cuts ({cuts:?}) do not come up to head as a fresh database does: {failures:#?}",
        failures
            .iter()
            .map(|f| f.split(':').next().unwrap_or(""))
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        cuts.len()
    );
}
