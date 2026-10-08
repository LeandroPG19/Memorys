// needs-a-server: runs the real binary's `account` commands against a scratch database it creates and drops
//
//! `memory-industry account add|list|revoke|revoke-device|pair`, as an operator on the host runs
//! them: the real binary, talking to the database directly (it never needs the daemon or its
//! token), printing the pairing code once and nothing that could be used to log in.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched, and the binary is pointed at the scratch one through its own `DATABASE_URL`.

mod common;

use chrono::{DateTime, Duration, TimeZone, Utc};
use common::in_a_scratch_database;
use memory_industry::accounts::{Secret, code_key, normalize_code};
use memory_industry::accounts_db::{self as db, ClaimOutcome};
use sqlx::PgPool;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0)
        .single()
        .expect("a valid instant")
}

fn account_cli(database_url: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .arg("account")
        .args(args)
        .env("DATABASE_URL", database_url)
        .output()
        .unwrap_or_else(|e| panic!("running `memory-industry account {args:?}`: {e}"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

async fn migrated(url: &str) -> PgPool {
    memory_industry::db::create_pool(url)
        .await
        .expect("migrating the scratch database")
}

async fn paired(pool: &PgPool, account: &str, label: &str) -> Secret {
    let issued = db::issue_pairing_code(pool, account, None, t0())
        .await
        .unwrap_or_else(|e| panic!("issuing a code for {account}: {e:#}"));
    let key = code_key(&normalize_code(&issued.code).expect("normalized"));
    let candidates = db::pairing_candidates(pool, t0())
        .await
        .expect("candidates");
    let candidate = candidates
        .iter()
        .find(|c| c.code_key == key)
        .expect("the code just issued is a candidate");
    let secret = Secret::generate();
    let outcome = db::claim_pairing_code(pool, candidate, label, secret.hash(), t0())
        .await
        .expect("claiming");
    assert!(
        matches!(outcome, ClaimOutcome::Created(_)),
        "the setup pairs a device: {outcome:?}"
    );
    secret
}

fn codes_in(output: &str) -> Vec<String> {
    output
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-'))
        .filter(|t| t.len() == 14 && t.matches('-').count() == 2 && normalize_code(t).is_ok())
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn add_creates_the_account_in_lowercase_and_a_repeat_fails_saying_so() {
    in_a_scratch_database("brain_acct_cli_add", |url| async move {
        let pool = migrated(&url).await;

        let added = account_cli(&url, &["add", "Ana"]);
        assert!(
            added.status.success(),
            "`account add Ana` must succeed: {}{}",
            text(&added.stdout),
            text(&added.stderr)
        );
        assert!(
            text(&added.stdout).contains("ana"),
            "the command says which account it created: {}",
            text(&added.stdout)
        );
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM brain_accounts")
            .fetch_all(&pool)
            .await
            .expect("reading accounts");
        assert_eq!(names, ["ana"], "the account exists, in lowercase");

        let again = account_cli(&url, &["add", "ana"]);
        assert!(
            !again.status.success(),
            "adding an account that exists is an error the operator must see in the exit code"
        );
        assert!(
            text(&again.stderr).contains("already exists"),
            "and says why: {}",
            text(&again.stderr)
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn list_prints_accounts_by_name_and_never_a_secret_in_either_format() {
    in_a_scratch_database("brain_acct_cli_list", |url| async move {
        let pool = migrated(&url).await;
        db::add_account(&pool, "ana", t0()).await.expect("ana");
        db::add_account(&pool, "zed", t0() + Duration::minutes(1))
            .await
            .expect("zed");
        let secrets = vec![
            paired(&pool, "ana", "pc-a1").await,
            paired(&pool, "ana", "pc-a2").await,
            paired(&pool, "zed", "pc-z1").await,
        ];
        let first_id = db::list_accounts(&pool).await.expect("listing").remove(0).credentials[0].id;
        let short = &first_id.simple().to_string()[..8];

        for args in [&["list"][..], &["list", "--json"][..]] {
            let out = account_cli(&url, args);
            let shown = text(&out.stdout);
            assert!(
                out.status.success(),
                "`account {args:?}` must succeed: {shown}{}",
                text(&out.stderr)
            );
            assert!(
                shown.find("ana") < shown.find("zed") && shown.contains("ana"),
                "`account {args:?}` lists ana before zed (by name, though zed is newer):\n{shown}"
            );
            for label in ["pc-a1", "pc-a2", "pc-z1"] {
                assert!(shown.contains(label), "`account {args:?}` shows the device {label}:\n{shown}");
            }
            assert!(
                shown.contains(short),
                "`account {args:?}` identifies a device by the first 8 hex of its id ({short}):\n{shown}"
            );
            for secret in &secrets {
                for hidden in [hex::encode(secret.hash()), secret.to_hex()] {
                    assert!(
                        !shown.contains(&hidden),
                        "`account {args:?}` printed a secret or a secret hash ({hidden}); a list \
                         is pasted into tickets and screenshots"
                    );
                }
            }
            for forbidden in ["mi1.", "code_key"] {
                assert!(
                    !shown.contains(forbidden),
                    "`account {args:?}` must not print {forbidden:?}:\n{shown}"
                );
            }
        }

        let json: serde_json::Value =
            serde_json::from_str(&text(&account_cli(&url, &["list", "--json"]).stdout))
                .expect("`account list --json` prints JSON and nothing else on stdout");
        assert!(json.is_array(), "the JSON list is an array of accounts: {json}");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn pair_prints_the_code_once_with_the_fingerprint_the_expiry_and_the_client_command() {
    in_a_scratch_database("brain_acct_cli_pair", |url| async move {
        let pool = migrated(&url).await;
        db::add_account(&pool, "ana", t0()).await.expect("ana");

        let before = Utc::now();
        let out = account_cli(&url, &["pair", "ana", "--device", "pc-lab-1"]);
        let after = Utc::now();
        let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
        assert!(
            out.status.success(),
            "`account pair` must succeed: {stdout}{stderr}"
        );

        let codes = codes_in(&stdout);
        assert_eq!(
            codes.len(),
            1,
            "the output holds exactly one pairing code, got {codes:?}:\n{stdout}"
        );
        let code = &codes[0];
        assert_eq!(
            stdout.matches(code.as_str()).count(),
            1,
            "the code appears once in the output:\n{stdout}"
        );
        assert!(
            !stderr.contains(code.as_str()),
            "the code never goes to stderr or to the logs, only to the operator's screen once"
        );
        assert!(
            stdout
                .lines()
                .find(|l| l.contains(code.as_str()))
                .is_some_and(|l| l.trim_start().starts_with("memory-industry bridge pair")),
            "the code sits in the command the client types:\n{stdout}"
        );

        let host = db::host_identity(&pool).await.expect("the host identity");
        assert!(
            stdout.contains(&memory_industry::accounts::fingerprint(&host.host_id)),
            "the host fingerprint is printed so the client can compare it by eye:\n{stdout}"
        );

        let (key, label, expires_at): (Vec<u8>, Option<String>, DateTime<Utc>) =
            sqlx::query_as("SELECT code_key, device_label, expires_at FROM brain_pairing_codes")
                .fetch_one(&pool)
                .await
                .expect("the one code the command stored");
        assert_eq!(
            key,
            code_key(&normalize_code(code).expect("normalized")).to_vec(),
            "the printed code is the one whose key was stored"
        );
        assert_eq!(
            label.as_deref(),
            Some("pc-lab-1"),
            "`--device` fixes the label"
        );
        assert!(
            expires_at >= before + Duration::hours(24) - Duration::seconds(1)
                && expires_at <= after + Duration::hours(24),
            "the command uses the real clock and a 24 hour validity: expires at {expires_at}, \
             ran between {before} and {after}"
        );
        let expiry = expires_at.format("%Y-%m-%d %H:%M UTC").to_string();
        assert!(
            stdout
                .lines()
                .any(|l| l.contains("valid until") && l.contains(&expiry)),
            "the output says `valid until {expiry}` (UTC, to the minute):\n{stdout}"
        );

        let unknown = account_cli(&url, &["pair", "ghost"]);
        assert!(
            !unknown.status.success(),
            "pairing an account that does not exist is an error"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn revoke_device_fails_when_nothing_matched_and_revoke_takes_the_whole_account() {
    in_a_scratch_database("brain_acct_cli_revoke", |url| async move {
        let pool = migrated(&url).await;
        db::add_account(&pool, "ana", t0()).await.expect("ana");
        paired(&pool, "ana", "pc-lab-1").await;
        paired(&pool, "ana", "otro").await;

        let nothing = account_cli(&url, &["revoke-device", "ana", "nunca-existio"]);
        assert!(
            !nothing.status.success() && !text(&nothing.stderr).trim().is_empty(),
            "revoking a device that matched nothing exits non-zero and says so on stderr: {}{}",
            text(&nothing.stdout),
            text(&nothing.stderr)
        );

        let one = account_cli(&url, &["revoke-device", "ana", "PC-LAB-1"]);
        assert!(
            one.status.success(),
            "revoking a device by label (in any case) succeeds: {}{}",
            text(&one.stdout),
            text(&one.stderr)
        );
        let active: Vec<String> = sqlx::query_scalar(
            "SELECT device_label FROM brain_account_credentials WHERE revoked_at IS NULL",
        )
        .fetch_all(&pool)
        .await
        .expect("reading active credentials");
        assert_eq!(active, ["otro"], "only the other device is still active");

        let whole = account_cli(&url, &["revoke", "ana"]);
        assert!(
            whole.status.success(),
            "revoking the account succeeds: {}{}",
            text(&whole.stdout),
            text(&whole.stderr)
        );
        let revoked: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT revoked_at FROM brain_accounts WHERE name = 'ana'")
                .fetch_one(&pool)
                .await
                .expect("reading ana");
        assert!(
            revoked.is_some(),
            "the account carries a revocation instant"
        );
        pool.close().await;
    })
    .await;
}

#[test]
fn account_help_exits_zero_without_the_database() {
    for flag in ["--help", "-h"] {
        let out = Command::new(BIN)
            .args(["account", flag])
            .env_remove("DATABASE_URL")
            // With no URL, a command that tried to open the database would stop here with exit 1
            // at once, instead of starting a PostgreSQL container and waiting for it.
            .env("CUBA_MODE", "red")
            .output()
            .expect("running `memory-industry account --help`");
        let shown = text(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "`account {flag}` exits 0 without a database: {shown}{}",
            text(&out.stderr)
        );
        assert!(
            shown.starts_with("usage: memory-industry account"),
            "the help starts with the usage line: {shown}"
        );
        assert!(
            !shown.to_ascii_lowercase().contains("postgres"),
            "the help does not talk about PostgreSQL: it is a command for an operator, who runs it \
             to learn the words, not to reach the database:\n{shown}"
        );
    }
}
