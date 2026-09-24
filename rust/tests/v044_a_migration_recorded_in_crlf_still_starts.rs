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
//
// The file name falls under NEEDS_A_SERVER ('v044_*') in ci.yml: it creates and
// drops its own database on the server DATABASE_URL names.

mod common;

use common::in_a_scratch_database;
use sha2::{Digest, Sha384};
use sqlx::PgPool;

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
    let lf = up.sql.replace("\r\n", "\n");
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
    in_a_scratch_database("brain_crlf", |url| async move {
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
