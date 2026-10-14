// Shared by the test files that declare `mod common;`. It lives in a
// directory so cargo does not build it as a test target of its own, and so
// the `for file in tests/*.rs` discovery in run-all-tests.sh and ci.yml, which
// does not recurse, never hands it to `--test`. Everything here has to be used
// by every file that includes it: each of them compiles its own copy, and an
// item one of them leaves unused is a dead_code warning that clippy
// `-D warnings` turns into a failed gate.

use std::future::Future;

use memory_industry::db;
use sha2::{Digest, Sha384};
use sqlx::{Connection, Executor, PgConnection};
use tokio::sync::OnceCell;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// One per process, shared by every `#[tokio::test]` of the binary: the semaphore inside a tokio
/// `OnceCell` does not belong to a runtime. A panic in the init leaves it empty, so the next test
/// tries again.
static TEMPLATE: OnceCell<String> = OnceCell::const_new();

/// Re-rolls every id a migration seeded with a random default, in the tables the clone holds rows
/// of. 33 columns in `public` default to `gen_random_uuid()`, 3 of them in tables with rows
/// (brain_node_identity.node_id, brain_host_identity.host_id, the four
/// brain_memory_layers.layer_id), no trigger on those, and the one foreign key onto them
/// (brain_facts.layer_id) has no rows. A rule over the catalog instead of a list, so a later
/// migration that seeds another id enters by itself.
const REROLL_SEEDED_IDENTITIES: &str = r#"
DO $$
DECLARE
    seeded record;
    has_rows boolean;
BEGIN
    FOR seeded IN
        SELECT c.table_name::text AS table_name, c.column_name::text AS column_name
        FROM information_schema.columns c
        JOIN information_schema.tables t
          ON t.table_schema = c.table_schema AND t.table_name = c.table_name
         AND t.table_type = 'BASE TABLE'
        WHERE c.table_schema = 'public' AND c.column_default = 'gen_random_uuid()'
    LOOP
        EXECUTE format('SELECT EXISTS (SELECT 1 FROM public.%I)', seeded.table_name)
            INTO has_rows;
        IF has_rows THEN
            EXECUTE format('UPDATE public.%I SET %I = DEFAULT', seeded.table_name, seeded.column_name);
        END IF;
    END LOOP;
END
$$
"#;

fn database_url() -> String {
    std::env::var("DATABASE_URL").expect(
        "DATABASE_URL is required: this test creates and drops its own database on the server \
         that URL names, and never writes to the database the URL itself points at",
    )
}

fn sibling_url(url: &str, name: &str) -> String {
    let cut = url.rfind('/').expect("a database URL ends in /<name>");
    format!("{}/{name}", &url[..cut])
}

fn has_sqlstate(why: &sqlx::Error, code: &str) -> bool {
    why.as_database_error()
        .and_then(|database| database.code())
        .is_some_and(|got| got == code)
}

/// Stops the test saying which step failed. A 55006 gets its cause: the template (or the database
/// being sealed) had a session on it, and the server waited 5 s for it to leave before refusing.
fn refuse(step: &str, why: &sqlx::Error) -> ! {
    if has_sqlstate(why, "55006") {
        panic!(
            "{step}: {why}. Someone is connected to the template, which accepts no connections; \
             the process that built it had to close its pool before sealing it"
        );
    }
    panic!("{step}: {why}");
}

async fn run(admin: &mut PgConnection, statement: &str, step: &str) {
    admin
        .execute(statement)
        .await
        .unwrap_or_else(|why| refuse(step, &why));
}

/// The first 12 hex of the SHA-384 of the up migrations' (version, checksum): the template is
/// rebuilt only when what the binary embeds changes.
fn template_key() -> String {
    let mut digest = Sha384::new();
    for migration in MIGRATOR
        .iter()
        .filter(|migration| !migration.migration_type.is_down_migration())
    {
        digest.update(migration.version.to_le_bytes());
        digest.update(&migration.checksum[..]);
    }
    hex::encode(digest.finalize())[..12].to_owned()
}

/// A sealed `test_tpl_<key>` is a clone source nobody can connect to. A clone from a template with
/// a session on it waits 5 s and fails with 55006 (measured), and RENAME has the same requirement,
/// so the builder closes its only pool, seals with ALLOW_CONNECTIONS false (which keeps out
/// autovacuum too) and only then publishes under the final name. STRATEGY FILE_COPY is not used:
/// it forces a checkpoint, 0.86-3.4 s per clone against 0.11-0.37 s with the default WAL_LOG.
async fn the_template(url: &str) -> &'static str {
    TEMPLATE.get_or_init(|| build_the_template(url)).await
}

async fn build_the_template(url: &str) -> String {
    let name = format!("test_tpl_{}", template_key());
    let mut admin = PgConnection::connect(&sibling_url(url, "postgres"))
        .await
        .expect("connecting to the maintenance database");

    let existing: Vec<(String, bool)> = sqlx::query_as(
        "SELECT datname::text, datallowconn FROM pg_database WHERE datname LIKE 'test\\_tpl\\_%'",
    )
    .fetch_all(&mut admin)
    .await
    .unwrap_or_else(|why| refuse("listing the templates", &why));
    if let Some((_, accepts)) = existing.iter().find(|(other, _)| *other == name) {
        assert!(
            !accepts,
            "{name} accepts connections: it was not published by this helper, which seals before \
             it renames. Drop it by hand"
        );
        return name;
    }
    // Leftovers of this same key (`{name}_<8hex>`) stay: they may be another process's live build.
    // Only another key's templates go, which assumes what ci-rapido already requires: no two
    // `cargo test` on one cluster.
    for other in existing
        .iter()
        .map(|(other, _)| other)
        .filter(|other| !other.starts_with(&name))
    {
        run(
            &mut admin,
            &format!("DROP DATABASE IF EXISTS {other} WITH (FORCE)"),
            "sweeping the template of another set of migrations",
        )
        .await;
    }

    let building = format!("{name}_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    run(
        &mut admin,
        &format!("CREATE DATABASE {building}"),
        "creating the database the template is built in",
    )
    .await;
    let pool = db::create_admin_pool(&sibling_url(url, &building))
        .await
        .expect("migrating the template");
    pool.close().await;
    run(
        &mut admin,
        &format!("ALTER DATABASE {building} WITH ALLOW_CONNECTIONS false"),
        "sealing the template",
    )
    .await;
    match admin
        .execute(format!("ALTER DATABASE {building} RENAME TO {name}").as_str())
        .await
    {
        Ok(_) => {}
        // Another process published first (duplicate_database): its template is as good as this one.
        Err(why) if has_sqlstate(&why, "42P04") => {
            run(
                &mut admin,
                &format!("DROP DATABASE IF EXISTS {building} WITH (FORCE)"),
                "discarding the template this process built second",
            )
            .await;
        }
        Err(why) => refuse("publishing the template", &why),
    }
    name
}

/// Runs `body` against a database created for it, on the server `DATABASE_URL`
/// names, and drops that database afterwards, also when `body` panics. The body
/// runs in its own task so a failed assertion comes back as a `JoinError`
/// instead of unwinding past the DROP; the panic is re-raised only once the
/// database is gone. The precedent in v037 drops before asserting, but an
/// `expect` that fires earlier than that would still leave its database behind.
///
/// The database arrives migrated: it is a clone of one template, built once per server and per set
/// of embedded migrations and kept between runs as `test_tpl_<key>` (dropping it by hand rebuilds
/// it; like any database with grants to cuba_app it blocks `DROP ROLE cuba_app` while it exists).
/// Migrating in every body made twelve bodies die with `tuple concurrently updated`: migration
/// 0041 and the password provisioning write cuba_app's row in pg_authid, which belongs to the
/// whole server, while sqlx's lock is per database (91-93 % of the transactions lost with sixteen
/// sessions, measured). With a clone each test writes that row once, in its `create_pool`, and
/// none runs a migration. A body that calls `create_pool` on it still gets the daemon's own path:
/// nothing to migrate, provisioning, step down to cuba_app.
///
/// Two clones must be two installations, so the ids the migrations seed are re-rolled in each
/// (see REROLL_SEEDED_IDENTITIES). A test that needs a database with no schema at all uses
/// `empty_db::in_an_empty_database`.
pub async fn in_a_scratch_database<F, Fut>(prefix: &str, body: F)
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let url = database_url();
    let template = the_template(&url).await;
    let scratch = format!("{prefix}_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let mut admin = PgConnection::connect(&sibling_url(&url, "postgres"))
        .await
        .expect("connecting to the maintenance database");
    run(
        &mut admin,
        &format!("CREATE DATABASE {scratch} TEMPLATE {template}"),
        "cloning the template into the scratch database",
    )
    .await;

    if let Err(why) = reroll_identities(&sibling_url(&url, &scratch)).await {
        run(
            &mut admin,
            &format!("DROP DATABASE IF EXISTS {scratch} WITH (FORCE)"),
            "dropping the scratch database",
        )
        .await;
        refuse(
            "re-rolling the identities the clone shares with its template",
            &why,
        );
    }

    let outcome = tokio::spawn(body(sibling_url(&url, &scratch))).await;

    admin
        .execute(format!("DROP DATABASE IF EXISTS {scratch} WITH (FORCE)").as_str())
        .await
        .expect("dropping the scratch database");
    if let Err(joined) = outcome {
        std::panic::resume_unwind(joined.into_panic());
    }
}

async fn reroll_identities(clone_url: &str) -> Result<(), sqlx::Error> {
    let mut clone = PgConnection::connect(clone_url).await?;
    clone.execute(REROLL_SEEDED_IDENTITIES).await?;
    clone.close().await
}
