// needs-a-server: creates and drops its own database where DATABASE_URL points, and migrates it three times
//
// sqlx records in `_sqlx_migrations.checksum` the SHA-384 of each migration's
// bytes as the binary that applied it embedded them, and `sqlx::migrate!` reads
// them from disk unchanged. A Windows checkout made with core.autocrlf=true
// before `*.sql text eol=lf` reached .gitattributes keeps its migrations in
// CRLF — git does not rewrite a file when attributes are added — so a daemon
// built there records CRLF checksums. Measured on a live base: 63 and 64 held
// the SHA-384 of their CRLF bytes. The binary CI publishes embeds LF and died
// against that base with «migration 63 was previously applied but has been
// modified», which took down the publishing gate.
//
// One test and one scratch database, not two tests: each would migrate its own
// database, and two databases of one server migrating at once can collide on
// the server-wide role that migration 0041 touches.

use crate::common::in_a_scratch_database;
use crate::empty_db::in_an_empty_database;
use serde_json::Value;
use sha2::{Digest, Sha384};
use sqlx::PgPool;
use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

const REALIGNED_VERSION: i64 = 63;
const TAMPERED_VERSION: i64 = 64;

fn up_migration(version: i64) -> &'static sqlx::migrate::Migration {
    MIGRATOR
        .iter()
        .find(|m| m.version == version && !m.migration_type.is_down_migration())
        .unwrap_or_else(|| panic!("this binary embeds no up migration {version}"))
}

/// The checksum a binary built from the other line endings would have recorded:
/// CRLF when this one embeds LF, which is the base that failed, and LF when this
/// one was built from a CRLF checkout.
fn checksum_in_the_other_line_endings(version: i64) -> Vec<u8> {
    let up = up_migration(version);
    let lf = up.sql.as_str().replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    [lf, crlf]
        .iter()
        .map(|sql| Sha384::digest(sql.as_bytes()).to_vec())
        .find(|checksum| checksum.as_slice() != &*up.checksum)
        .unwrap_or_else(|| panic!("migration {version} has no newline to write another way"))
}

async fn recorded_checksum(admin: &PgPool, version: i64) -> Vec<u8> {
    sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = $1")
        .bind(version)
        .fetch_one(admin)
        .await
        .unwrap_or_else(|e| panic!("reading the recorded checksum of migration {version}: {e}"))
}

async fn record_checksum(admin: &PgPool, version: i64, checksum: &[u8]) {
    let done = sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = $2")
        .bind(checksum)
        .bind(version)
        .execute(admin)
        .await
        .unwrap_or_else(|e| panic!("rewriting the recorded checksum of migration {version}: {e}"));
    assert_eq!(
        done.rows_affected(),
        1,
        "migration {version} is not in the scratch database's _sqlx_migrations, so this test \
         has no record to rewrite"
    );
}

#[tokio::test]
async fn a_record_in_the_other_line_endings_is_realigned_and_no_other_is() {
    in_an_empty_database("brain_crlf", |url| async move {
        memory_industry::db::create_pool(&url)
            .await
            .unwrap_or_else(|e| panic!("migrating the empty scratch database: {e:#}"))
            .close()
            .await;
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let embedded = up_migration(REALIGNED_VERSION).checksum.to_vec();
        assert_eq!(
            recorded_checksum(&admin, REALIGNED_VERSION).await,
            embedded,
            "a database this binary migrated does not hold the checksum this binary embeds, so \
             nothing below can tell a realigned record from one that was never different"
        );

        let other = checksum_in_the_other_line_endings(REALIGNED_VERSION);
        record_checksum(&admin, REALIGNED_VERSION, &other).await;
        let restarted = memory_industry::db::create_pool(&url).await;
        let realigned = recorded_checksum(&admin, REALIGNED_VERSION).await;
        restarted
            .unwrap_or_else(|e| {
                panic!(
                    "the daemon did not start against a base whose migration \
                     {REALIGNED_VERSION} was recorded from the same SQL in other line endings — \
                     the base a CRLF checkout on Windows leaves, and the one the published LF \
                     binary died on: {e:#}"
                )
            })
            .close()
            .await;
        assert_eq!(
            realigned, embedded,
            "the daemon started, but migration {REALIGNED_VERSION} still carries the checksum \
             of the other line endings instead of the one this binary embeds"
        );

        let tampered = Sha384::digest(b"not the SQL of any migration").to_vec();
        record_checksum(&admin, TAMPERED_VERSION, &tampered).await;
        let refused = memory_industry::db::create_pool(&url).await;
        let left = recorded_checksum(&admin, TAMPERED_VERSION).await;
        admin.close().await;
        let error = match refused {
            Ok(pool) => {
                pool.close().await;
                panic!(
                    "the daemon started against a base whose migration {TAMPERED_VERSION} was \
                     recorded from SQL that is not this binary's in any line endings: the \
                     realignment forgave a real difference, which is what sqlx's check exists \
                     to refuse"
                )
            }
            Err(e) => format!("{e:#}"),
        };
        assert!(
            error.contains("previously applied but has been modified")
                && error.contains(&TAMPERED_VERSION.to_string()),
            "the base was refused, but not by sqlx's checksum check on migration \
             {TAMPERED_VERSION}: {error}"
        );
        assert_eq!(
            left, tampered,
            "a record that differs in content was rewritten; only one that differs by line \
             endings may be"
        );
    })
    .await;
}

// The same moment seen from the other side: what the start does with a base whose
// `_sqlx_migrations` it cannot take as its own. A binary one release behind meets a base the newer
// one already migrated (a roll back), and a base whose migrations fail. The first must start, as
// the role it always steps down to, and say what it does not know; the second must not be served
// with the role of the URL: that role is the owner, so row-level security and the append-only
// audit trigger would be off without a word. These two run the real binary, as the operator does,
// on a clone of the migrated template, which is why they do not use `in_an_empty_database`.

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

/// The wait for a child that has to end by itself. It guards against a hang; it is not a verdict.
const GUARD: Duration = Duration::from_secs(60);

const NEWER_VERSION: i64 = 9066;
const WITHOUT_DATABASE: &str =
    "starting without PostgreSQL — tools will fail until it is reachable";
const DOWNGRADED: &str = "runtime downgraded to a non-superuser role";
const TOOLS_LIST: &str = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n";

struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// The binary with `args` on the database `url`, the model directories empty so nothing loads, one
/// request on stdin and then the end of file; waits for it to end. `kill_on_drop` keeps a child
/// that hangs from outliving its test. CUBA_APP_ROLE is removed so the step down to the
/// application role is whatever the machine allows, never what the judge's environment says.
async fn start(args: &[&str], url: &str, extra_env: &[(&str, &str)]) -> Ran {
    let models = std::env::temp_dir().join(format!("mi-p8-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&models).expect("the empty model directory is created");
    let mut child = Command::new(BIN)
        .args(args)
        .env("DATABASE_URL", url)
        .env("MEMORY_INDUSTRY_REQUIRE_DATABASE_URL", "1")
        .env("RUST_LOG", "memory_industry=info")
        .env("ONNX_MODEL_PATH", &models)
        .env("CUBA_RERANKER_PATH", &models)
        .env_remove("CUBA_APP_ROLE")
        .env_remove("CUBA_SKIP_MIGRATIONS")
        .envs(extra_env.iter().copied())
        .env_remove("CUBA_HTTP_ADDR")
        .env_remove("CUBA_HTTP_TOKEN")
        .env_remove("CUBA_PEER_TOKEN")
        .env_remove("LISTEN_FDS")
        .env_remove("LISTEN_PID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("the binary runs");
    let mut pipe = child.stdin.take().expect("stdin is piped");
    pipe.write_all(TOOLS_LIST.as_bytes())
        .await
        .expect("the request reaches the child");
    drop(pipe);
    let output = tokio::time::timeout(GUARD, child.wait_with_output())
        .await
        .expect("the binary did not end by itself within the guard")
        .expect("the child can be waited for");
    std::fs::remove_dir_all(&models).ok();
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn log_events(stderr: &str) -> Vec<Value> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn said(events: &[Value], message_start: &str) -> bool {
    events.iter().any(|event| {
        event["fields"]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with(message_start))
    })
}

async fn recorded_versions(admin: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(admin)
        .await
        .expect("counting the recorded migrations")
}

#[tokio::test]
async fn a_database_migrated_by_a_newer_binary_still_starts_and_says_so() {
    in_a_scratch_database("brain_newer", |url| async move {
        let control = start(&[], &url, &[]).await;
        let control_events = log_events(&control.stderr);
        assert_eq!(
            control.code,
            Some(0),
            "the control start, on the migrated clone as it is, did not exit 0 at the end of \
             file, so nothing below says anything about the version it does not know.\nstderr: {}",
            control.stderr
        );
        assert!(
            !said(&control_events, WITHOUT_DATABASE),
            "the control start fell to the lazy pool on a database nobody touched.\nstderr: {}",
            control.stderr
        );

        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let before = recorded_versions(&admin).await;
        let unknown = Sha384::digest(b"a migration this binary does not embed").to_vec();
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES ($1, $2, TRUE, $3, 0)",
        )
        .bind(NEWER_VERSION)
        .bind("applied by a newer binary")
        .bind(&unknown)
        .execute(&admin)
        .await
        .expect("recording a migration this binary does not embed, as a newer one would");

        let ran = start(&[], &url, &[]).await;
        let after = recorded_versions(&admin).await;
        let kept: Vec<u8> =
            sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = $1")
                .bind(NEWER_VERSION)
                .fetch_one(&admin)
                .await
                .expect("the row of the newer binary is still there");
        admin.close().await;

        let events = log_events(&ran.stderr);
        assert!(
            !said(&events, WITHOUT_DATABASE),
            "a base that a newer binary already migrated (version {NEWER_VERSION}) sent this \
             binary to the lazy pool: the migrator refuses a version it does not embed, and the \
             daemon then serves with the role of DATABASE_URL, without RLS or the audit trigger, \
             and without REM or sync.\nstderr: {}",
            ran.stderr
        );
        assert_eq!(
            ran.code,
            Some(0),
            "the start on a base with a newer migration did not exit 0 at the end of file.\n\
             stderr: {}",
            ran.stderr
        );
        assert!(
            events.iter().any(|event| event["level"] == "WARN"
                && event.to_string().contains(&NEWER_VERSION.to_string())),
            "the start tolerated migration {NEWER_VERSION} but no WARN of its log names it, so \
             whoever rolled back is never told the base is ahead of the binary.\nstderr: {}",
            ran.stderr
        );
        assert_eq!(
            said(&events, DOWNGRADED),
            said(&control_events, DOWNGRADED),
            "the start on a base with a newer migration does not step down to the application \
             role the way the same base without it does (control said it: {}).\nstderr: {}",
            said(&control_events, DOWNGRADED),
            ran.stderr
        );
        assert_eq!(
            after, before + 1,
            "the start touched the record of migrations: {before} rows before it, {after} after \
             one was added"
        );
        assert_eq!(
            kept, unknown,
            "the start rewrote the checksum of a migration it does not embed"
        );
    })
    .await;
}

#[tokio::test]
async fn a_database_whose_migrations_fail_is_not_served_as_the_url_role() {
    in_a_scratch_database("brain_failed", |url| async move {
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let tampered = Sha384::digest(b"not the SQL of any migration").to_vec();
        record_checksum(&admin, TAMPERED_VERSION, &tampered).await;
        admin.close().await;
        let refusal =
            format!("migration {TAMPERED_VERSION} was previously applied but has been modified");

        let stdio = start(&[], &url, &[]).await;
        assert_eq!(
            stdio.code,
            Some(1),
            "a base that answers and whose migrations fail must stop the stdio start with exit 1. \
             It is served instead, with a lazy pool and the role of DATABASE_URL (the owner), so \
             RLS and the audit trigger are off and nothing says so.\nstderr: {}",
            stdio.stderr
        );
        assert!(
            !said(&log_events(&stdio.stderr), WITHOUT_DATABASE),
            "the stdio start took a failed migration for a database that is not there.\nstderr: {}",
            stdio.stderr
        );
        assert!(
            stdio.stderr.contains("MemoryIndustry NO puede arrancar")
                && stdio.stderr.contains(&refusal),
            "the refusal has to say that it cannot start and why, as the one for the width of the \
             vectors does.\nstderr: {}",
            stdio.stderr
        );
        assert!(
            stdio.stdout.is_empty(),
            "a start that refused to serve answered something: {}",
            stdio.stdout
        );

        let taken = TcpListener::bind("127.0.0.1:0").expect("a port to hold, so a bind would fail");
        let address = format!(
            "127.0.0.1:{}",
            taken
                .local_addr()
                .expect("the held port has an address")
                .port()
        );
        let served = start(&["serve", address.as_str()], &url, &[]).await;
        drop(taken);
        assert!(
            !said(&log_events(&served.stderr), WITHOUT_DATABASE),
            "`serve` took a failed migration for a database that is not there: it went on to bind \
             with the role of DATABASE_URL.\nstderr: {}",
            served.stderr
        );
        assert_eq!(
            served.code,
            Some(1),
            "`serve` on a base whose migrations fail must exit 1.\nstderr: {}",
            served.stderr
        );
        assert!(
            served
                .stderr
                .lines()
                .any(|line| line.starts_with("serve: ") && line.contains(&refusal)),
            "`serve` exited 1 but its last word is not the migration that failed.\nstderr: {}",
            served.stderr
        );

        // A base BEHIND the binary under CUBA_SKIP_MIGRATIONS=1 (set on the child only): init_schema
        // refuses it with a bail! and nothing brings it forward. Same refusal, same exit.
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = (SELECT MAX(version) FROM _sqlx_migrations)")
            .execute(&admin)
            .await
            .expect("forgetting the newest migration, so the base is one behind this binary");
        let behind: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = TRUE")
            .fetch_one(&admin)
            .await
            .expect("the newest migration the base still records");
        admin.close().await;
        let embedded = MIGRATOR
            .iter()
            .map(|m| m.version)
            .max()
            .expect("this binary embeds at least one migration");
        let late = format!("this database is at migration {behind} and this binary expects {embedded}");

        let skipped = start(&[], &url, &[("CUBA_SKIP_MIGRATIONS", "1")]).await;
        assert_eq!(
            skipped.code,
            Some(1),
            "a base one migration behind this binary, with CUBA_SKIP_MIGRATIONS=1, must stop the \
             stdio start with exit 1. It is served from a lazy pool instead, as the owner, and \
             nothing will ever bring it forward.\nstderr: {}",
            skipped.stderr
        );
        assert!(
            !said(&log_events(&skipped.stderr), WITHOUT_DATABASE),
            "the stdio start took a base behind the binary for a database that is not there.\n\
             stderr: {}",
            skipped.stderr
        );
        assert!(
            skipped.stderr.contains("MemoryIndustry NO puede arrancar")
                && skipped.stderr.contains(&late),
            "the refusal has to say «{late}».\nstderr: {}",
            skipped.stderr
        );
    })
    .await;
}
