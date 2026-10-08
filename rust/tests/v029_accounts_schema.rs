// needs-a-server: migrates a scratch database it creates and drops, and checks what migration 0065 left in it
//
//! Migration 0065 creates the four tables of accounts and pairing, adds one column, can be
//! replayed and undone, and the database itself refuses the rows the code must never write:
//! a secret hash of the wrong length, a pairing code that lives longer than 24 hours, a second
//! host identity, an account name the CLI would have refused.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched.

mod common;

use common::in_a_scratch_database;
use sqlx::{Executor, PgPool};
use std::path::Path;

const NEW_TABLES: [&str; 4] = [
    "brain_host_identity",
    "brain_accounts",
    "brain_account_credentials",
    "brain_pairing_codes",
];

const CHECK_VIOLATION: &str = "23514";
const UNIQUE_VIOLATION: &str = "23505";
// ON DELETE RESTRICT raises restrict_violation (23001); foreign_key_violation (23503) is what
// NO ACTION raises, with the same effect on a non-deferrable key. The migration says RESTRICT
// on purpose, so the test expects the code RESTRICT really produces.
const RESTRICT_VIOLATION: &str = "23001";

fn migration(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("migrations")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} must exist and be readable ({e}): slice F ships migration 0065_accounts, and \
             every scenario of this file is about what it leaves in the database",
            path.display()
        )
    })
}

/// Migrates through `create_pool`, as the daemon does, and then hands back a plain connection to
/// the same database as its owner: the checks below delete rows and read the catalogue, and the
/// application role the daemon steps down to (when its password is set) may not DELETE here.
async fn migrated(url: &str) -> PgPool {
    memory_industry::db::create_pool(url)
        .await
        .expect("migrating the scratch database")
        .close()
        .await;
    PgPool::connect(url)
        .await
        .expect("connecting to the scratch database as its owner")
}

async fn exec(pool: &PgPool, sql: &str) {
    pool.execute(sql)
        .await
        .unwrap_or_else(|e| panic!("this statement must succeed ({e}):\n{sql}"));
}

/// The SQLSTATE the database answered with, or `None` when it accepted the statement.
async fn sqlstate_of(pool: &PgPool, sql: &str) -> Option<String> {
    match pool.execute(sql).await {
        Ok(_) => None,
        Err(sqlx::Error::Database(e)) => Some(e.code().map(|c| c.to_string()).unwrap_or_default()),
        Err(other) => panic!("a non-database failure on a plain statement: {other}\n{sql}"),
    }
}

async fn assert_refused(pool: &PgPool, why: &str, sql: &str, expected: &str) {
    let got = sqlstate_of(pool, sql).await;
    assert_eq!(
        got.as_deref(),
        Some(expected),
        "{why}\nThe database must refuse this with SQLSTATE {expected}, and answered with \
         {got:?} (None = accepted, 42P01 = the table does not exist).\n{sql}"
    );
}

async fn table_exists(pool: &PgPool, table: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = $1)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .expect("reading information_schema.tables")
}

async fn account_column_of_failures(pool: &PgPool) -> Option<(String, String)> {
    sqlx::query_as(
        "SELECT data_type, is_nullable FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'brain_handler_failures' \
           AND column_name = 'account'",
    )
    .fetch_optional(pool)
    .await
    .expect("reading information_schema.columns")
}

#[tokio::test]
async fn the_migration_creates_the_four_tables_one_host_row_and_the_failures_column() {
    in_a_scratch_database("brain_acct_schema", |url| async move {
        let pool = migrated(&url).await;

        for table in NEW_TABLES {
            assert!(
                table_exists(&pool, table).await,
                "{table} must exist after the migrations ran: 0065_accounts creates it"
            );
        }
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_host_identity")
            .fetch_one(&pool)
            .await
            .expect("counting host identity rows");
        assert_eq!(
            rows, 1,
            "brain_host_identity holds exactly one row from the moment the migration ran: the \
             host id is minted by the migration, not by whoever asks first"
        );
        assert_eq!(
            account_column_of_failures(&pool).await,
            Some(("text".to_string(), "YES".to_string())),
            "brain_handler_failures.account must be a nullable text column: failures written by \
             the shared token have no account"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn replaying_the_migration_changes_nothing_and_keeps_the_host_id() {
    in_a_scratch_database("brain_acct_replay", |url| async move {
        let pool = migrated(&url).await;
        let before: uuid::Uuid = sqlx::query_scalar("SELECT host_id FROM brain_host_identity")
            .fetch_one(&pool)
            .await
            .expect("the host id the migration minted");
        exec(
            &pool,
            "INSERT INTO brain_accounts (name, created_at) VALUES ('survivor', now())",
        )
        .await;

        let up = migration("0065_accounts.up.sql");
        pool.execute(up.as_str()).await.unwrap_or_else(|e| {
            panic!(
                "running the body of 0065 a second time must not abort ({e}): sqlx replays \
                 migrations against a database that already has the objects, and PostgreSQL \
                 has no IF NOT EXISTS for constraints, so every object here is guarded"
            )
        });

        let after: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT host_id FROM brain_host_identity")
            .fetch_all(&pool)
            .await
            .expect("reading the host id again");
        assert_eq!(
            after,
            vec![before],
            "a replay keeps one host identity row and its id: the id is the fingerprint every \
             paired client pinned, and a new one would make every bridge refuse its host"
        );
        let survivors: i64 =
            sqlx::query_scalar("SELECT count(*) FROM brain_accounts WHERE name = 'survivor'")
                .fetch_one(&pool)
                .await
                .expect("counting the account that was there before the replay");
        assert_eq!(survivors, 1, "a replay must not touch existing rows");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_down_migration_removes_exactly_what_the_up_added_and_the_up_works_again() {
    in_a_scratch_database("brain_acct_down", |url| async move {
        let pool = migrated(&url).await;

        let down = migration("0065_accounts.down.sql");
        pool.execute(down.as_str()).await.unwrap_or_else(|e| {
            panic!(
                "0065_accounts.down.sql must run on a migrated database ({e}); the tables are \
                 dropped from the referencing one to the referenced one"
            )
        });
        for table in NEW_TABLES {
            assert!(
                !table_exists(&pool, table).await,
                "{table} must be gone after the down migration"
            );
        }
        assert_eq!(
            account_column_of_failures(&pool).await,
            None,
            "the down migration must drop brain_handler_failures.account as well as the tables: \
             a down that leaves its column behind is not a down"
        );

        let up = migration("0065_accounts.up.sql");
        pool.execute(up.as_str())
            .await
            .expect("the up migration must work again after the down one");
        for table in NEW_TABLES {
            assert!(
                table_exists(&pool, table).await,
                "{table} must exist again after up, down, up"
            );
        }
        pool.close().await;
    })
    .await;
}

fn without_sql_comments(sql: &str) -> String {
    sql.lines()
        .map(|line| line.split("--").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_uppercase()
}

#[test]
fn the_up_migration_only_adds_and_touches_no_table_that_already_existed_but_one_column() {
    let up = without_sql_comments(&migration("0065_accounts.up.sql"));

    for forbidden in [
        "DROP ",
        "RENAME",
        "ALTER COLUMN",
        "TRUNCATE",
        "DELETE FROM",
        "CREATE TRIGGER",
    ] {
        assert!(
            !up.contains(forbidden),
            "0065_accounts.up.sql contains {forbidden:?}: migrations are expand-only. The old \
             binary keeps running against the new schema until the new one is deployed, so \
             nothing is renamed, dropped, retyped or emptied in the same cut"
        );
    }

    let new_tables: Vec<String> = NEW_TABLES.iter().map(|t| t.to_ascii_uppercase()).collect();
    let altered: Vec<&str> = up
        .split("ALTER TABLE")
        .skip(1)
        .map(|rest| rest.split_whitespace().next().unwrap_or(""))
        .filter(|table| !new_tables.iter().any(|new| new == table))
        .collect();
    assert_eq!(
        altered,
        vec!["BRAIN_HANDLER_FAILURES"],
        "the only existing table 0065 may alter is brain_handler_failures, and only to add the \
         account column"
    );

    assert_eq!(
        up.matches("CREATE TABLE").count(),
        up.matches("CREATE TABLE IF NOT EXISTS").count(),
        "every CREATE TABLE in 0065 is IF NOT EXISTS, so a replay does not abort"
    );
    assert_eq!(
        up.matches("CREATE INDEX").count(),
        up.matches("CREATE INDEX IF NOT EXISTS").count(),
        "every CREATE INDEX in 0065 is IF NOT EXISTS, so a replay does not abort"
    );
    assert!(
        up.contains("ADD COLUMN IF NOT EXISTS ACCOUNT"),
        "the account column on brain_handler_failures is added with ADD COLUMN IF NOT EXISTS"
    );
}

#[tokio::test]
async fn the_database_refuses_what_the_columns_forbid() {
    in_a_scratch_database("brain_acct_checks", |url| async move {
        let pool = migrated(&url).await;
        exec(
            &pool,
            "INSERT INTO brain_accounts (name, created_at) VALUES ('ana', now())",
        )
        .await;

        assert_refused(
            &pool,
            "an account name is lowercase by construction; «Ana» would be a second spelling of «ana»",
            "INSERT INTO brain_accounts (name, created_at) VALUES ('Ana', now())",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "an account name with a space is not a name the CLI accepts",
            "INSERT INTO brain_accounts (name, created_at) VALUES ('a b', now())",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "an account name is at most 32 characters, and this one has 33",
            "INSERT INTO brain_accounts (name, created_at) VALUES ('a' || repeat('b', 32), now())",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "an account name is unique: two accounts called «ana» could not be told apart in the audit",
            "INSERT INTO brain_accounts (name, created_at) VALUES ('ana', now())",
            UNIQUE_VIOLATION,
        )
        .await;

        assert_refused(
            &pool,
            "a secret hash is a SHA-256: 32 bytes, and this one has 31",
            "INSERT INTO brain_account_credentials (account_id, device_label, secret_hash, created_at) \
             SELECT id, 'pc', decode(repeat('ab', 31), 'hex'), now() FROM brain_accounts WHERE name = 'ana'",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "a device label cannot hold a line break: it is printed in lists and in the audit",
            "INSERT INTO brain_account_credentials (account_id, device_label, secret_hash, created_at) \
             SELECT id, E'pc\\nx', decode(repeat('ab', 32), 'hex'), now() FROM brain_accounts WHERE name = 'ana'",
            CHECK_VIOLATION,
        )
        .await;

        assert_refused(
            &pool,
            "a pairing code lives at most 24 hours; this one is cut to live 25",
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at) \
             SELECT id, decode(repeat('cd', 32), 'hex'), now(), now() + interval '25 hours' \
             FROM brain_accounts WHERE name = 'ana'",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "a pairing code expires after it was created, not at the same instant",
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at) \
             SELECT id, decode(repeat('cd', 32), 'hex'), now(), now() FROM brain_accounts WHERE name = 'ana'",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "a code key is a SHA-256: 32 bytes, and this one has 31",
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at) \
             SELECT id, decode(repeat('cd', 31), 'hex'), now(), now() + interval '1 hour' \
             FROM brain_accounts WHERE name = 'ana'",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "a code marked used must say which credential it produced",
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at, used_at) \
             SELECT id, decode(repeat('cd', 32), 'hex'), now(), now() + interval '1 hour', now() \
             FROM brain_accounts WHERE name = 'ana'",
            CHECK_VIOLATION,
        )
        .await;

        assert_refused(
            &pool,
            "there is exactly one host identity, and `singleton = false` is a way around it",
            "INSERT INTO brain_host_identity (singleton) VALUES (false)",
            CHECK_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "a second host identity row would give the host two fingerprints",
            "INSERT INTO brain_host_identity (singleton) VALUES (true)",
            UNIQUE_VIOLATION,
        )
        .await;
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_credential_and_the_code_that_made_it_must_agree_and_deletes_follow_the_declared_rules() {
    in_a_scratch_database("brain_acct_links", |url| async move {
        let pool = migrated(&url).await;
        exec(
            &pool,
            "INSERT INTO brain_accounts (name, created_at) VALUES ('ana', now()), ('bea', now())",
        )
        .await;
        exec(
            &pool,
            "INSERT INTO brain_account_credentials (account_id, device_label, secret_hash, created_at) \
             SELECT id, 'pc', decode(repeat('ab', 32), 'hex'), now() FROM brain_accounts WHERE name = 'ana'",
        )
        .await;

        assert_refused(
            &pool,
            "a code with a credential but no used_at is half redeemed: the two columns say the same thing",
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at, credential_id) \
             SELECT a.id, decode(repeat('cd', 32), 'hex'), now(), now() + interval '1 hour', c.id \
             FROM brain_accounts a, brain_account_credentials c WHERE a.name = 'ana'",
            CHECK_VIOLATION,
        )
        .await;

        exec(
            &pool,
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at, used_at, credential_id) \
             SELECT a.id, decode(repeat('cd', 32), 'hex'), now(), now() + interval '24 hours', now(), c.id \
             FROM brain_accounts a, brain_account_credentials c WHERE a.name = 'ana'",
        )
        .await;
        exec(
            &pool,
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at) \
             SELECT id, decode(repeat('ce', 32), 'hex'), now(), now() + interval '24 hours' \
             FROM brain_accounts WHERE name = 'bea'",
        )
        .await;

        assert_refused(
            &pool,
            "an account that has credentials cannot be deleted: its history would go with it (ON DELETE RESTRICT raises restrict_violation 23001, not foreign_key_violation 23503, which is NO ACTION's)",
            "DELETE FROM brain_accounts WHERE name = 'ana'",
            RESTRICT_VIOLATION,
        )
        .await;
        assert_refused(
            &pool,
            "a credential that a code points at cannot be deleted either (ON DELETE RESTRICT raises restrict_violation 23001, not foreign_key_violation 23503, which is NO ACTION's)",
            "DELETE FROM brain_account_credentials",
            RESTRICT_VIOLATION,
        )
        .await;

        exec(&pool, "DELETE FROM brain_accounts WHERE name = 'bea'").await;
        let orphans: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM brain_pairing_codes WHERE code_key = decode(repeat('ce', 32), 'hex')",
        )
        .fetch_one(&pool)
        .await
        .expect("counting the codes of the deleted account");
        assert_eq!(
            orphans, 0,
            "an account without credentials takes its unused codes with it (ON DELETE CASCADE)"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_edges_the_columns_allow_are_accepted() {
    in_a_scratch_database("brain_acct_edges", |url| async move {
        let pool = migrated(&url).await;
        exec(
            &pool,
            "INSERT INTO brain_accounts (name, created_at) VALUES ('a' || repeat('b', 31), now())",
        )
        .await;
        exec(
            &pool,
            "INSERT INTO brain_pairing_codes (account_id, code_key, created_at, expires_at) \
             SELECT id, decode(repeat('cd', 32), 'hex'), now(), now() + interval '24 hours' \
             FROM brain_accounts LIMIT 1",
        )
        .await;
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_clock_is_the_callers_and_the_foreign_keys_have_indexes() {
    in_a_scratch_database("brain_acct_shape", |url| async move {
        let pool = migrated(&url).await;

        for (table, column) in [
            ("brain_accounts", "created_at"),
            ("brain_account_credentials", "created_at"),
            ("brain_pairing_codes", "created_at"),
            ("brain_pairing_codes", "expires_at"),
        ] {
            let default: Option<String> = sqlx::query_scalar(
                "SELECT column_default FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
            )
            .bind(table)
            .bind(column)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("{table}.{column} must exist ({e})"));
            assert_eq!(
                default, None,
                "{table}.{column} has no DEFAULT: the caller passes the clock, so tests can \
                 inject it and a DEFAULT now() cannot hide a clock nobody injected"
            );
        }

        for index in [
            "idx_brain_account_credentials_account",
            "idx_brain_pairing_codes_account",
            "idx_brain_pairing_codes_credential",
        ] {
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND indexname = $1)",
            )
            .bind(index)
            .fetch_one(&pool)
            .await
            .expect("reading pg_indexes");
            assert!(
                found,
                "{index} must exist: every foreign key has its own index, or deleting an account \
                 scans the whole table that references it"
            );
        }
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_application_role_cannot_delete_history_but_can_work() {
    in_a_scratch_database("brain_acct_privs", |url| async move {
        let pool = migrated(&url).await;

        for table in [
            "brain_accounts",
            "brain_account_credentials",
            "brain_host_identity",
        ] {
            let can_delete: bool =
                sqlx::query_scalar("SELECT has_table_privilege('cuba_app', $1, 'DELETE')")
                    .bind(table)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|e| panic!("asking for the privileges on {table}: {e}"));
            assert!(
                !can_delete,
                "cuba_app must not have DELETE on {table}: deleting an account or a credential \
                 deletes the record of who did what, so revocation is a column, never a row removal"
            );
        }
        let can_delete_codes: bool = sqlx::query_scalar(
            "SELECT has_table_privilege('cuba_app', 'brain_pairing_codes', 'DELETE')",
        )
        .fetch_one(&pool)
        .await
        .expect("asking for the privileges on brain_pairing_codes");
        assert!(
            can_delete_codes,
            "cuba_app keeps DELETE on brain_pairing_codes: revoking an account deletes its unused \
             codes and the purge deletes old ones"
        );

        for table in NEW_TABLES {
            for privilege in ["SELECT", "INSERT", "UPDATE"] {
                let allowed: bool =
                    sqlx::query_scalar("SELECT has_table_privilege('cuba_app', $1, $2)")
                        .bind(table)
                        .bind(privilege)
                        .fetch_one(&pool)
                        .await
                        .unwrap_or_else(|e| panic!("asking for {privilege} on {table}: {e}"));
                assert!(
                    allowed,
                    "cuba_app must have {privilege} on {table}: the daemon and the CLI both run \
                     as the application role"
                );
            }
        }
        pool.close().await;
    })
    .await;
}
