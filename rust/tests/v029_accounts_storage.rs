// needs-a-server: migrates a scratch database it creates and drops, and runs the account storage against it
//
//! What the host's database does with accounts, credentials and pairing codes: creating and
//! listing accounts, issuing a code that is stored only as a key, revoking, the authentication
//! lookup and its throttled `last_seen_at`, and the purge.
//!
//! The clock is always the caller's: every instant here is passed in, and several are far in
//! the past on purpose, so a query that quietly used the database's own `now()` is caught.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched.

mod common;

use chrono::{DateTime, Duration, TimeZone, Utc};
use common::in_a_scratch_database;
use memory_industry::accounts::{self, Secret, code_key, normalize_code};
use memory_industry::accounts_db::{self as db, ClaimOutcome, CredentialState};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0)
        .single()
        .expect("a valid instant")
}

async fn migrated(url: &str) -> PgPool {
    memory_industry::db::create_pool(url)
        .await
        .expect("migrating the scratch database")
}

async fn account(pool: &PgPool, name: &str, now: DateTime<Utc>) {
    db::add_account(pool, name, now)
        .await
        .unwrap_or_else(|e| panic!("add_account({name}) must succeed: {e:#}"));
}

/// Issues a code for `account_name`, reads the candidates the way the daemon does and redeems it.
/// Returns the credential id and the secret the "client" generated.
async fn pair_device(
    pool: &PgPool,
    account_name: &str,
    host_label: Option<&str>,
    client_label: &str,
    at: DateTime<Utc>,
) -> (Uuid, Secret) {
    let issued = db::issue_pairing_code(pool, account_name, host_label, at)
        .await
        .unwrap_or_else(|e| panic!("issue_pairing_code({account_name}) must succeed: {e:#}"));
    let key = code_key(&normalize_code(&issued.code).expect("an issued code normalizes"));
    let candidates = db::pairing_candidates(pool, at)
        .await
        .unwrap_or_else(|e| panic!("pairing_candidates must succeed: {e:#}"));
    let candidate = candidates
        .iter()
        .find(|c| c.code_key == key)
        .expect("the code just issued is among the candidates");
    let secret = Secret::generate();
    match db::claim_pairing_code(pool, candidate, client_label, secret.hash(), at)
        .await
        .unwrap_or_else(|e| panic!("claim_pairing_code must succeed: {e:#}"))
    {
        ClaimOutcome::Created(created) => (created.credential_id, secret),
        other => panic!("a fresh code for {account_name} must be Created, got {other:?}"),
    }
}

async fn last_seen_of(pool: &PgPool, id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT last_seen_at FROM brain_account_credentials WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("reading last_seen_at")
}

async fn audit_payloads(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT payload FROM brain_audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .expect("reading the audit log")
}

async fn whole_audit_text(pool: &PgPool) -> String {
    let rows: Vec<String> = sqlx::query_scalar("SELECT payload::text FROM brain_audit_log")
        .fetch_all(pool)
        .await
        .expect("reading the audit log");
    rows.join("\n")
}

/// Every table of the public schema whose rows, rendered as text, contain one of `needles`
/// (case-insensitively). A bytea column renders as `\x` plus lowercase hex, so hex needles find
/// stored bytes.
async fn tables_containing(pool: &PgPool, needles: &[String]) -> Vec<String> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE' ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .expect("listing tables");
    let patterns: Vec<String> = needles.iter().map(|n| format!("%{n}%")).collect();
    let mut hits = Vec::new();
    for table in tables {
        let sql =
            format!("SELECT EXISTS (SELECT 1 FROM \"{table}\" r WHERE r::text ILIKE ANY($1))");
        let found: bool = sqlx::query_scalar(&sql)
            .bind(&patterns)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("scanning {table}: {e}"));
        if found {
            hits.push(table);
        }
    }
    hits
}

#[tokio::test]
async fn the_host_identity_is_one_stable_row_the_code_only_reads() {
    in_a_scratch_database("brain_acct_host", |url| async move {
        let pool = migrated(&url).await;
        let first = db::host_identity(&pool)
            .await
            .expect("host_identity reads the row the migration minted");
        let second = db::host_identity(&pool).await.expect("and reads it again");
        assert_eq!(
            first.host_id, second.host_id,
            "the host id never changes between reads: it is the fingerprint every client pinned"
        );
        let stored: Uuid = sqlx::query_scalar("SELECT host_id FROM brain_host_identity")
            .fetch_one(&pool)
            .await
            .expect("the single host row");
        assert_eq!(
            first.host_id, stored,
            "host_identity returns what the table holds"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn an_account_is_created_in_lowercase_once_and_the_audit_remembers_it() {
    in_a_scratch_database("brain_acct_add", |url| async move {
        let pool = migrated(&url).await;

        let created = db::add_account(&pool, "Ana", t0())
            .await
            .expect("a valid name creates an account");
        assert_eq!(
            created.name, "ana",
            "account names are stored in lowercase: «Ana» and «ana» are one account"
        );
        assert_eq!(
            created.created_at,
            t0(),
            "the caller's clock is the creation time"
        );

        for again in ["ana", "ANA"] {
            let err = db::add_account(&pool, again, t0())
                .await
                .expect_err("a second account with the same name must be refused");
            assert!(
                format!("{err:#}").contains("already exists"),
                "creating {again:?} twice must say the account already exists, got: {err:#}"
            );
        }
        let err = db::add_account(&pool, "a b", t0())
            .await
            .expect_err("a name with a space must be refused");
        assert!(
            format!("{err:#}").contains("account name"),
            "a bad name is refused with the reason (the name rule), got: {err:#}"
        );

        assert_eq!(
            audit_payloads(&pool, "account.add").await,
            vec![json!({"account": "ana", "actor": "cli"})],
            "exactly one account.add event, written for the creation that happened and for none \
             of the three refusals, with the account and who did it"
        );
        let chain = memory_industry::handlers::archivo::handle(&pool, json!({"action": "verify"}))
            .await
            .expect("verifying the audit chain");
        assert_eq!(
            chain["ok"],
            json!(true),
            "the event joins the hash chain without breaking it: {chain}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_revoked_account_name_is_never_given_out_again() {
    in_a_scratch_database("brain_acct_reuse", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        db::revoke_account(&pool, "ana", t0() + Duration::hours(1))
            .await
            .expect("revoking an account");
        let err = db::add_account(&pool, "ana", t0() + Duration::hours(2))
            .await
            .expect_err("a revoked name is still taken");
        assert!(
            format!("{err:#}").contains("already exists"),
            "a revoked account keeps its name, or the audit would name two people the same: {err:#}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_list_is_ordered_by_name_and_carries_each_device_without_a_secret() {
    in_a_scratch_database("brain_acct_list", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        account(&pool, "zed", t0() + Duration::minutes(1)).await;
        let (_, first) = pair_device(&pool, "ana", None, "pc-a1", t0()).await;
        let (_, second) = pair_device(&pool, "ana", None, "pc-a2", t0()).await;
        let (zed_credential, third) = pair_device(&pool, "zed", None, "pc-z1", t0()).await;

        let listed = db::list_accounts(&pool).await.expect("listing accounts");
        let names: Vec<&str> = listed.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            ["ana", "zed"],
            "accounts come back ordered by name, not by when they were created (zed is newer, ana \
             is first): the same list twice must read the same"
        );
        let mut labels: Vec<&str> = listed[0]
            .credentials
            .iter()
            .map(|c| c.device_label.as_str())
            .collect();
        labels.sort_unstable();
        assert_eq!(labels, ["pc-a1", "pc-a2"], "ana's two devices, by label");
        assert_eq!(listed[1].credentials.len(), 1, "zed's one device");
        assert_eq!(listed[1].credentials[0].id, zed_credential);
        assert!(
            listed[1].credentials[0].last_seen_at.is_none(),
            "a device that never called has no last_seen_at"
        );
        assert!(
            listed.iter().all(|a| a.revoked_at.is_none()),
            "nobody was revoked"
        );

        let dump = format!("{listed:?}");
        for (what, secret) in [("first", &first), ("second", &second), ("third", &third)] {
            for hidden in [hex::encode(secret.hash()), secret.to_hex()] {
                assert!(
                    !dump.contains(&hidden),
                    "the {what} device's secret or its hash ({hidden}) must not be in what \
                     list_accounts returns: a list is printed and pasted into tickets"
                );
            }
        }
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_pairing_code_expires_after_exactly_24_hours_and_is_stored_only_as_a_key() {
    in_a_scratch_database("brain_acct_issue", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;

        let issued = db::issue_pairing_code(&pool, "ana", Some("pc-lab-1"), t0())
            .await
            .expect("issuing a code for an active account");
        let normalized = normalize_code(&issued.code).unwrap_or_else(|e| {
            panic!("the issued code {:?} is not a valid code: {e}", issued.code)
        });
        let groups: Vec<&str> = issued.code.split('-').collect();
        assert!(
            groups.len() == 3 && groups.iter().all(|g| g.len() == 4),
            "the code is printed as three groups of four, got {:?}",
            issued.code
        );
        assert_eq!(
            issued.expires_at,
            t0() + Duration::hours(24),
            "a code is valid for exactly 24 hours from the instant it was issued, by the \
             caller's clock"
        );
        let host = db::host_identity(&pool).await.expect("the host identity");
        assert_eq!(
            issued.fingerprint,
            accounts::fingerprint(&host.host_id),
            "the fingerprint printed with the code is the host's, so the client can check it by eye"
        );

        let rows: Vec<(Vec<u8>, Option<String>, DateTime<Utc>)> =
            sqlx::query_as("SELECT code_key, device_label, expires_at FROM brain_pairing_codes")
                .fetch_all(&pool)
                .await
                .expect("reading the pairing codes");
        assert_eq!(rows.len(), 1, "one code was issued");
        assert_eq!(
            rows[0].0,
            code_key(&normalized).to_vec(),
            "the row stores the code's key, and that key is what the client's MAC is checked with"
        );
        assert_eq!(
            rows[0].1.as_deref(),
            Some("pc-lab-1"),
            "the device label the operator fixed is stored in device_label, not the code"
        );
        assert_eq!(
            rows[0].2, issued.expires_at,
            "the stored expiry is the reported one"
        );

        let plain = [
            issued.code.clone(),
            normalized.clone(),
            hex::encode(issued.code.as_bytes()),
            hex::encode(normalized.as_bytes()),
        ];
        assert_eq!(
            tables_containing(&pool, &plain).await,
            Vec::<String>::new(),
            "no column of any table may hold the code in clear, hyphenated or not, as text or as \
             bytes: the database stores only code_key, so a copy of the database is not a stack \
             of valid codes"
        );
        assert_eq!(
            tables_containing(&pool, &[hex::encode(code_key(&normalized))]).await,
            vec!["brain_pairing_codes".to_string()],
            "control: the scan does see stored bytes — the code's key is in brain_pairing_codes \
             and nowhere else, so an empty answer above means absent, not blind"
        );

        let events = audit_payloads(&pool, "account.pair_issued").await;
        assert_eq!(
            events.len(),
            1,
            "issuing a code leaves one account.pair_issued event"
        );
        assert_eq!(
            events[0]["account"],
            json!("ana"),
            "the event names the account"
        );
        assert!(
            events[0].get("actor").is_some(),
            "the event says who issued it: {}",
            events[0]
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn at_most_50_codes_are_active_at_once_and_expired_ones_do_not_count() {
    in_a_scratch_database("brain_acct_cap", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        for n in 0..accounts::MAX_ACTIVE_PAIRING_CODES {
            db::issue_pairing_code(&pool, "ana", None, t0())
                .await
                .unwrap_or_else(|e| panic!("code number {} of 50 must be issued: {e:#}", n + 1));
        }
        let err = db::issue_pairing_code(&pool, "ana", None, t0())
            .await
            .expect_err("the 51st active code must be refused");
        assert!(
            format!("{err:#}").contains("too many active pairing codes"),
            "the refusal says why, got: {err:#}"
        );
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_pairing_codes")
            .fetch_one(&pool)
            .await
            .expect("counting codes");
        assert_eq!(stored, 50, "the refused code was not stored");

        db::issue_pairing_code(&pool, "ana", None, t0() + Duration::hours(25))
            .await
            .expect(
                "25 hours later the first 50 have expired and no longer count, so one more is \
                 allowed: the cap is on codes that can still be redeemed",
            );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn no_code_is_issued_for_an_unknown_or_revoked_account() {
    in_a_scratch_database("brain_acct_issue_refused", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        db::revoke_account(&pool, "ana", t0() + Duration::minutes(1))
            .await
            .expect("revoking ana");

        let revoked = db::issue_pairing_code(&pool, "ana", None, t0() + Duration::minutes(2))
            .await
            .expect_err(
                "a code for a revoked account would be a door into an account that cannot be used",
            );
        assert!(
            format!("{revoked:#}").contains("is revoked"),
            "the refusal names the reason, not the foreign key the insert would trip over: \
             {revoked:#}"
        );
        let unknown = db::issue_pairing_code(&pool, "ghost", None, t0() + Duration::minutes(2))
            .await
            .expect_err("a code for an unknown account would be a door into nothing");
        assert!(
            format!("{unknown:#}").contains("does not exist"),
            "the refusal names the reason, not the foreign key the insert would trip over: \
             {unknown:#}"
        );
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_pairing_codes")
            .fetch_one(&pool)
            .await
            .expect("counting codes");
        assert_eq!(stored, 0, "the refusals stored nothing");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn revoking_an_account_revokes_its_devices_deletes_its_unused_codes_and_can_be_repeated() {
    in_a_scratch_database("brain_acct_revoke", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        account(&pool, "bea", t0()).await;
        let mut known = Vec::new();
        let mut ana_credentials = Vec::new();
        for label in ["pc-lab-1", "pc-lab-1", "otro"] {
            let (id, secret) = pair_device(&pool, "ana", None, label, t0()).await;
            ana_credentials.push(id);
            known.push(secret);
        }
        let (bea_credential, bea_secret) = pair_device(&pool, "bea", None, "pc-lab-1", t0()).await;
        known.push(bea_secret);
        db::issue_pairing_code(&pool, "ana", None, t0())
            .await
            .expect("an unused code for ana");
        db::issue_pairing_code(&pool, "bea", None, t0())
            .await
            .expect("an unused code for bea");

        let when = t0() + Duration::hours(2);
        let report = db::revoke_account(&pool, "ana", when)
            .await
            .expect("revoking ana");
        let after_first = audit_payloads(&pool, "account.revoke").await;
        assert_eq!(
            after_first.len(),
            1,
            "the FIRST revocation writes its account.revoke event at once: a repeat that writes \
             it instead is the bug the events check below would miss"
        );
        assert_eq!(
            after_first[0]["account"],
            json!("ana"),
            "and that event names ana"
        );
        assert_eq!(report.credentials_revoked, 3, "all three of ana's devices are revoked");
        assert_eq!(
            report.codes_deleted, 1,
            "ana's one unused code is deleted and the report says how many"
        );

        let account_revoked: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT revoked_at FROM brain_accounts WHERE name = 'ana'")
                .fetch_one(&pool)
                .await
                .expect("reading ana");
        assert_eq!(account_revoked, Some(when), "the account carries the revocation instant");
        let unrevoked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM brain_account_credentials c JOIN brain_accounts a ON a.id = c.account_id \
             WHERE a.name = 'ana' AND c.revoked_at IS DISTINCT FROM $1",
        )
        .bind(when)
        .fetch_one(&pool)
        .await
        .expect("counting ana's credentials not revoked at that instant");
        assert_eq!(
            unrevoked, 0,
            "every credential of the account is revoked with it, or a stolen laptop keeps its door"
        );
        let ana_codes: Vec<bool> = sqlx::query_scalar(
            "SELECT p.used_at IS NOT NULL FROM brain_pairing_codes p JOIN brain_accounts a ON a.id = p.account_id \
             WHERE a.name = 'ana'",
        )
        .fetch_all(&pool)
        .await
        .expect("reading ana's codes");
        assert!(
            ana_codes.iter().all(|used| *used) && ana_codes.len() == 3,
            "only the unused code went; the three used ones stay as the record of how each device \
             was paired: {ana_codes:?}"
        );
        let bea_codes: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM brain_pairing_codes p JOIN brain_accounts a ON a.id = p.account_id \
             WHERE a.name = 'bea' AND p.used_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .expect("counting bea's unused codes");
        assert_eq!(bea_codes, 1, "another account's unused code is untouched");

        for id in &ana_credentials {
            let row = db::credential_for_auth(&pool, *id)
                .await
                .expect("authentication lookup")
                .expect("a revoked credential is still found, so the reason can be told");
            assert_eq!(
                row.state,
                CredentialState::AccountRevoked,
                "a credential of a revoked account answers AccountRevoked, not Active"
            );
        }
        let bea = db::credential_for_auth(&pool, bea_credential)
            .await
            .expect("authentication lookup")
            .expect("bea's credential");
        assert_eq!(bea.state, CredentialState::Active, "bea is untouched");

        let again = db::revoke_account(&pool, "ana", when + Duration::hours(5))
            .await
            .expect("revoking an already revoked account succeeds: revocation is idempotent");
        assert_eq!(
            (again.credentials_revoked, again.codes_deleted),
            (0, 0),
            "the repeat revoked and deleted nothing"
        );
        let still: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT revoked_at FROM brain_accounts WHERE name = 'ana'")
                .fetch_one(&pool)
                .await
                .expect("reading ana again");
        assert_eq!(
            still,
            Some(when),
            "a repeat does not move the revocation instant: the first one is the one that happened"
        );

        let events = audit_payloads(&pool, "account.revoke").await;
        assert!(
            events.iter().any(|e| e["account"] == json!("ana")),
            "the revocation is in the audit chain, naming the account: {events:?}"
        );
        assert_eq!(
            events.len(),
            1,
            "one revocation is one account.revoke event: the repeat changed nothing and must write none"
        );
        let audit = whole_audit_text(&pool).await;
        for secret in &known {
            for hidden in [secret.to_hex(), hex::encode(secret.hash())] {
                assert!(
                    !audit.contains(&hidden),
                    "no secret or hash may reach the audit chain, which is append-only and cannot \
                     be scrubbed afterwards"
                );
            }
        }
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn revoking_a_device_matches_its_label_in_any_case_and_only_that_account() {
    in_a_scratch_database("brain_acct_device", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "dan", t0()).await;
        account(&pool, "eve", t0()).await;
        let (d1, _) = pair_device(&pool, "dan", None, "pc-lab-1", t0()).await;
        let (d2, _) = pair_device(&pool, "dan", None, "PC-Lab-1", t0()).await;
        let (d3, _) = pair_device(&pool, "dan", None, "otro", t0()).await;
        let (e1, _) = pair_device(&pool, "eve", None, "pc-lab-1", t0()).await;

        let when = t0() + Duration::hours(1);
        let revoked = db::revoke_device(&pool, "dan", "PC-LAB-1", when)
            .await
            .expect("revoking a device by label");
        assert_eq!(
            revoked, 2,
            "both of dan's devices called pc-lab-1 (in any capitalization) are revoked and the \
             number is returned"
        );
        for (id, expected, what) in [
            (d1, CredentialState::Revoked, "dan's first pc-lab-1"),
            (d2, CredentialState::Revoked, "dan's second PC-Lab-1"),
            (d3, CredentialState::Active, "dan's other device"),
            (
                e1,
                CredentialState::Active,
                "eve's device with the same label",
            ),
        ] {
            let row = db::credential_for_auth(&pool, id)
                .await
                .expect("authentication lookup")
                .expect("the credential exists");
            assert_eq!(row.state, expected, "{what}");
        }

        for label in ["PC-LAB-1", "nunca-existio"] {
            db::revoke_device(&pool, "dan", label, when + Duration::hours(1))
                .await
                .expect_err(
                    "revoking a label that matches no active device is an error: the operator \
                     typed something that did not happen, and the CLI must exit non-zero",
                );
        }
        db::revoke_device(&pool, "nobody", "pc-lab-1", when)
            .await
            .expect_err("an unknown account has no devices to revoke");

        let events = audit_payloads(&pool, "account.revoke_device").await;
        assert!(
            events.iter().any(|e| e["account"] == json!("dan")),
            "the device revocation is in the audit chain, naming the account: {events:?}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_authentication_lookup_returns_the_hash_the_account_the_label_and_the_state() {
    in_a_scratch_database("brain_acct_auth", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let (id, secret) = pair_device(&pool, "ana", None, "pc-lab-1", t0()).await;

        let row = db::credential_for_auth(&pool, id)
            .await
            .expect("authentication lookup")
            .expect("the credential exists");
        assert_eq!(row.id, id);
        assert_eq!(row.account, "ana", "the lookup names the account");
        assert_eq!(row.device_label, "pc-lab-1", "and the device");
        assert_eq!(
            row.secret_hash,
            secret.hash(),
            "and the hash the bearer is compared with in constant time"
        );
        assert_eq!(
            row.state,
            CredentialState::Active,
            "a fresh credential is active"
        );

        let unknown = db::credential_for_auth(&pool, Uuid::new_v4())
            .await
            .expect("an unknown id is not an error");
        assert!(unknown.is_none(), "an unknown id finds nothing");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn last_seen_is_written_at_most_once_a_minute() {
    in_a_scratch_database("brain_acct_seen", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let (id, _) = pair_device(&pool, "ana", None, "pc-lab-1", t0()).await;
        assert_eq!(
            last_seen_of(&pool, id).await,
            None,
            "a device that never called has no last_seen_at"
        );

        let first = t0() + Duration::minutes(10);
        db::touch_last_seen(&pool, id, first)
            .await
            .expect("first touch");
        assert_eq!(
            last_seen_of(&pool, id).await,
            Some(first),
            "the first call is recorded"
        );

        db::touch_last_seen(&pool, id, first + Duration::seconds(30))
            .await
            .expect("a touch 30 seconds later");
        assert_eq!(
            last_seen_of(&pool, id).await,
            Some(first),
            "30 seconds later nothing is written: one write per call would turn every MCP call \
             into a database write"
        );

        let later = first + Duration::seconds(61);
        db::touch_last_seen(&pool, id, later)
            .await
            .expect("a touch 61 seconds later");
        assert_eq!(
            last_seen_of(&pool, id).await,
            Some(later),
            "after a minute it is written again"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_purge_removes_codes_used_or_expired_over_a_week_ago_and_keeps_the_rest() {
    in_a_scratch_database("brain_acct_purge", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let day = |n: i64| t0() + Duration::days(n);

        let old_used = db::issue_pairing_code(&pool, "ana", None, day(0))
            .await
            .expect("a code that will be used long ago");
        let old_expired = db::issue_pairing_code(&pool, "ana", None, day(0))
            .await
            .expect("a code that will expire long ago");
        let recent_expired = db::issue_pairing_code(&pool, "ana", None, day(8))
            .await
            .expect("a code that expired just now");
        let still_valid = db::issue_pairing_code(&pool, "ana", None, day(8) + Duration::hours(12))
            .await
            .expect("a code that is still valid");
        let recent_used = db::issue_pairing_code(&pool, "ana", None, day(8))
            .await
            .expect("a code that was used yesterday");

        for (code, redeemed_at) in [
            (&old_used, day(0)),
            (&recent_used, day(8) + Duration::hours(1)),
        ] {
            let key = code_key(&normalize_code(&code.code).expect("a normalized code"));
            let candidates = db::pairing_candidates(&pool, redeemed_at)
                .await
                .expect("candidates");
            let candidate = candidates
                .iter()
                .find(|c| c.code_key == key)
                .expect("the issued code is a candidate");
            let outcome = db::claim_pairing_code(
                &pool,
                candidate,
                "pc",
                Secret::generate().hash(),
                redeemed_at,
            )
            .await
            .expect("claiming");
            assert!(
                matches!(outcome, ClaimOutcome::Created(_)),
                "the setup redeems this code: {outcome:?}"
            );
        }

        let now = day(9);
        let purged = db::purge_pairing_codes(&pool, now).await.expect("purging");
        assert_eq!(
            purged, 2,
            "the code used 9 days ago and the one that expired 8 days ago go; the other three stay"
        );

        let kept: Vec<Vec<u8>> = sqlx::query_scalar("SELECT code_key FROM brain_pairing_codes")
            .fetch_all(&pool)
            .await
            .expect("reading what is left");
        let key_of = |c: &db::IssuedCode| {
            code_key(&normalize_code(&c.code).expect("a normalized code")).to_vec()
        };
        for (code, what) in [
            (&recent_expired, "expired a day ago"),
            (&still_valid, "still valid"),
            (&recent_used, "used yesterday"),
        ] {
            assert!(
                kept.contains(&key_of(code)),
                "the code {what} must survive the purge: it is the history of the last week"
            );
        }
        for (code, what) in [
            (&old_used, "used 9 days ago"),
            (&old_expired, "expired 8 days ago"),
        ] {
            assert!(
                !kept.contains(&key_of(code)),
                "the code {what} must be purged"
            );
        }
        pool.close().await;
    })
    .await;
}
