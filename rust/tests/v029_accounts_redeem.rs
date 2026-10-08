// needs-a-server: migrates a scratch database it creates and drops, and redeems pairing codes against it, with two real connections racing for one code
//
//! Redeeming a pairing code: the 24-hour window by the caller's clock, one use only, the race of
//! two connections for the same code settled by the database and observed by this test (not
//! guessed with a sleep), a lost response retried safely, and an account revoked in between.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched.

mod common;

use chrono::{DateTime, Duration, TimeZone, Utc};
use common::in_a_scratch_database;
use memory_industry::accounts::{Secret, code_key, normalize_code};
use memory_industry::accounts_db::{
    self as db, CandidateState, ClaimOutcome, IssuedCode, PairingCandidate,
};
use sqlx::{Connection, PgConnection, PgPool};
use std::time::Instant;
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

async fn issue(
    pool: &PgPool,
    account_name: &str,
    label: Option<&str>,
    at: DateTime<Utc>,
) -> IssuedCode {
    db::issue_pairing_code(pool, account_name, label, at)
        .await
        .unwrap_or_else(|e| panic!("issue_pairing_code({account_name}) must succeed: {e:#}"))
}

/// The candidate for `code`, read at `at` the way the daemon reads them before a redemption.
async fn candidate(pool: &PgPool, code: &IssuedCode, at: DateTime<Utc>) -> PairingCandidate {
    let key = code_key(&normalize_code(&code.code).expect("an issued code normalizes"));
    db::pairing_candidates(pool, at)
        .await
        .unwrap_or_else(|e| panic!("pairing_candidates must succeed: {e:#}"))
        .into_iter()
        .find(|c| c.code_key == key)
        .unwrap_or_else(|| panic!("the code issued is not among the candidates read at {at}"))
}

fn hash() -> [u8; 32] {
    Secret::generate().hash()
}

async fn credentials_of(pool: &PgPool, account_name: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM brain_account_credentials c JOIN brain_accounts a ON a.id = c.account_id \
         WHERE a.name = $1",
    )
    .bind(account_name)
    .fetch_one(pool)
    .await
    .expect("counting credentials")
}

#[tokio::test]
async fn a_code_is_redeemable_until_the_instant_it_expires_and_no_later() {
    in_a_scratch_database("brain_redeem_window", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let at_expiry = issue(&pool, "ana", None, t0()).await;
        let just_before = issue(&pool, "ana", None, t0()).await;
        let expiry_candidate = candidate(&pool, &at_expiry, t0()).await;
        let just_before_candidate = candidate(&pool, &just_before, t0()).await;
        let day = Duration::hours(24);

        assert_eq!(
            db::classify_candidate(&expiry_candidate, t0()),
            CandidateState::Redeemable,
            "a code is redeemable the instant it is issued"
        );
        assert_eq!(
            db::classify_candidate(&expiry_candidate, t0() + day - Duration::seconds(1)),
            CandidateState::Redeemable,
            "one second before 24 hours it is still redeemable"
        );
        assert_eq!(
            db::classify_candidate(&expiry_candidate, t0() + day),
            CandidateState::Expired,
            "a code is valid only while now < expires_at, so at T+24h exactly it is expired: an \
             inclusive bound would give every code one more instant than the 24 hours promised"
        );

        let refused = db::claim_pairing_code(&pool, &expiry_candidate, "pc", hash(), t0() + day)
            .await
            .expect("claiming an expired code is a refusal, not an error");
        assert!(
            matches!(refused, ClaimOutcome::Refused),
            "at T+24h exactly the claim is Refused, got {refused:?}"
        );
        assert_eq!(
            credentials_of(&pool, "ana").await,
            0,
            "a refused claim creates no credential"
        );

        let accepted = db::claim_pairing_code(
            &pool,
            &just_before_candidate,
            "pc",
            hash(),
            t0() + day - Duration::seconds(1),
        )
        .await
        .expect("claiming one second before the expiry");
        assert!(
            matches!(accepted, ClaimOutcome::Created(_)),
            "one second before T+24h the claim succeeds, got {accepted:?}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn used_and_expired_codes_stay_listed_for_a_week_so_they_can_be_told_apart() {
    in_a_scratch_database("brain_redeem_retain", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let code = issue(&pool, "ana", None, t0()).await;
        let key = code_key(&normalize_code(&code.code).expect("normalized"));

        for (days, listed) in [(6, true), (8, false)] {
            let seen = db::pairing_candidates(&pool, t0() + Duration::days(days))
                .await
                .expect("candidates")
                .iter()
                .any(|c| c.code_key == key);
            assert_eq!(
                seen,
                listed,
                "{days} days after it was issued the code must {}be among the candidates: they \
                 are kept for 7 days so a late client is told «expired» or «used» and not just \
                 «unknown»",
                if listed { "" } else { "not " }
            );
        }
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_callers_clock_decides_not_the_databases() {
    in_a_scratch_database("brain_redeem_clock", |url| async move {
        let pool = migrated(&url).await;
        let two_days_ago = Utc::now() - Duration::days(2);
        account(&pool, "ana", two_days_ago).await;
        let code = issue(&pool, "ana", None, two_days_ago).await;
        let one_hour_in = two_days_ago + Duration::hours(1);
        let c = candidate(&pool, &code, one_hour_in).await;

        assert_eq!(
            db::classify_candidate(&c, one_hour_in),
            CandidateState::Redeemable,
            "judged at the instant it was handed, a code issued two days ago is an hour old"
        );
        let outcome = db::claim_pairing_code(&pool, &c, "pc", hash(), one_hour_in)
            .await
            .expect("claiming with an injected clock");
        assert!(
            matches!(outcome, ClaimOutcome::Created(_)),
            "the claim uses the `now` it is given: by the database's own clock this code expired a \
             day ago, and a query that read now() would refuse it. Got {outcome:?}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_code_is_used_once_and_the_second_claim_creates_nothing() {
    in_a_scratch_database("brain_redeem_once", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let code = issue(&pool, "ana", Some("pc-lab-1"), t0()).await;
        let c = candidate(&pool, &code, t0()).await;
        let now = t0() + Duration::minutes(5);

        let first = db::claim_pairing_code(&pool, &c, "pc-lab-1", hash(), now)
            .await
            .expect("the first claim");
        let ClaimOutcome::Created(created) = first else {
            panic!("the first claim of a valid code must be Created, got {first:?}");
        };
        let second = db::claim_pairing_code(&pool, &c, "otro", hash(), now)
            .await
            .expect("the second claim is a refusal, not an error");
        assert!(
            matches!(second, ClaimOutcome::Refused),
            "a code is single-use: the second claim, with another secret, is Refused. Got {second:?}"
        );

        assert_eq!(
            credentials_of(&pool, "ana").await,
            1,
            "exactly one credential exists for the code: the refused claim minted none"
        );
        let (used_at, credential_id): (Option<DateTime<Utc>>, Option<Uuid>) =
            sqlx::query_as("SELECT used_at, credential_id FROM brain_pairing_codes WHERE id = $1")
                .bind(c.id)
                .fetch_one(&pool)
                .await
                .expect("reading the code row");
        assert_eq!(used_at, Some(now), "the code records when it was used, by the caller's clock");
        assert_eq!(
            credential_id,
            Some(created.credential_id),
            "the code records which credential it produced"
        );
        let after = candidate(&pool, &code, now).await;
        assert_eq!(
            db::classify_candidate(&after, now),
            CandidateState::Used,
            "a used code reads as Used"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_label_the_host_fixed_wins_over_the_one_the_client_sends() {
    in_a_scratch_database("brain_redeem_label", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let fixed = issue(&pool, "ana", Some("host-says"), t0()).await;
        let open = issue(&pool, "ana", None, t0()).await;

        for (code, client_label, expected) in [
            (&fixed, "client-says", "host-says"),
            (&open, "client-says", "client-says"),
        ] {
            let c = candidate(&pool, code, t0()).await;
            let outcome = db::claim_pairing_code(&pool, &c, client_label, hash(), t0())
                .await
                .expect("claiming");
            let ClaimOutcome::Created(created) = outcome else {
                panic!("a fresh code must be Created, got {outcome:?}");
            };
            let stored: String = sqlx::query_scalar(
                "SELECT device_label FROM brain_account_credentials WHERE id = $1",
            )
            .bind(created.credential_id)
            .fetch_one(&pool)
            .await
            .expect("reading the credential");
            assert_eq!(
                stored, expected,
                "the operator who typed `--device` decides the label; a code without one takes \
                 the client's"
            );
        }
        pool.close().await;
    })
    .await;
}

/// Polls `pg_stat_activity` until `pid` waits on a lock. This is the synchronisation of the race:
/// B is known to be blocked behind A, whatever the machine's speed, and the deadline only turns a
/// broken implementation into a failure instead of a hang.
async fn wait_until_blocked<T>(
    observer: &mut PgConnection,
    pid: i32,
    b: &tokio::task::JoinHandle<T>,
) {
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let waiting: Option<String> =
            sqlx::query_scalar("SELECT wait_event_type FROM pg_stat_activity WHERE pid = $1")
                .bind(pid)
                .fetch_one(&mut *observer)
                .await
                .expect("reading pg_stat_activity");
        if waiting.as_deref() == Some("Lock") {
            return;
        }
        assert!(
            !b.is_finished(),
            "connection B finished its claim without ever waiting on a lock while A's claim was \
             open: the redemption is not one statement that takes the row, so two connections \
             can both read the code as unused"
        );
        assert!(
            Instant::now() < deadline,
            "connection B was never seen waiting on a lock within 10 s (its wait event was \
             {waiting:?})"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

async fn one_race(url: &str, pool: &PgPool, round: usize) {
    let now = Utc::now();
    let code = issue(pool, "ana", None, now).await;
    let c = candidate(pool, &code, now).await;
    let (hash_a, hash_b) = (hash(), hash());

    let mut conn_a = PgConnection::connect(url).await.expect("connection A");
    let mut conn_b = PgConnection::connect(url).await.expect("connection B");
    let mut observer = PgConnection::connect(url).await.expect("the observer");
    let b_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut conn_b)
        .await
        .expect("B's backend pid");

    let mut tx_a = conn_a.begin().await.expect("A begins");
    let a = db::claim_pairing_code(&mut *tx_a, &c, "a", hash_a, now)
        .await
        .expect("A's claim");
    assert!(
        matches!(a, ClaimOutcome::Created(_)),
        "round {round}: A, first on a fresh code, must create the credential, got {a:?}"
    );

    let c_for_b = c.clone();
    let b = tokio::spawn(async move {
        let mut tx_b = conn_b.begin().await.expect("B begins");
        let outcome = db::claim_pairing_code(&mut *tx_b, &c_for_b, "b", hash_b, now).await;
        tx_b.commit().await.expect("B commits");
        outcome
    });
    wait_until_blocked(&mut observer, b_pid, &b).await;
    tx_a.commit().await.expect("A commits");

    let b_outcome = b
        .await
        .expect("B's task")
        .expect("B's claim runs once A has committed");
    assert!(
        matches!(b_outcome, ClaimOutcome::Refused),
        "round {round}: B waited for A and A won, so B must end Refused; got {b_outcome:?}. \
         If B is Created the UPDATE did not re-check `used_at IS NULL` after the wait and the \
         same code paired two devices"
    );

    let hashes = vec![hash_a.to_vec(), hash_b.to_vec()];
    let winners: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT secret_hash FROM brain_account_credentials WHERE secret_hash = ANY($1)",
    )
    .bind(&hashes)
    .fetch_all(pool)
    .await
    .expect("reading the credentials that exist");
    assert_eq!(
        winners,
        vec![hash_a.to_vec()],
        "round {round}: exactly one credential exists for the code, and it is A's"
    );
}

#[tokio::test]
async fn two_connections_racing_for_one_code_create_exactly_one_credential() {
    in_a_scratch_database("brain_redeem_race", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", Utc::now()).await;
        for round in 0..20 {
            one_race(&url, &pool, round).await;
        }
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_lost_response_is_retried_with_the_same_hash_and_only_that() {
    in_a_scratch_database("brain_redeem_replay", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let code = issue(&pool, "ana", Some("pc-lab-1"), t0()).await;
        let c = candidate(&pool, &code, t0()).await;
        let h1 = hash();

        let first = db::claim_pairing_code(&pool, &c, "pc-lab-1", h1, t0())
            .await
            .expect("the first claim");
        let ClaimOutcome::Created(created) = first else {
            panic!("the first claim must be Created, got {first:?}");
        };

        let later = t0() + Duration::minutes(1);
        let retry_candidate = candidate(&pool, &code, later).await;
        let retried = db::claim_pairing_code(&pool, &retry_candidate, "pc-lab-1", h1, later)
            .await
            .expect("the retry");
        let ClaimOutcome::Replayed(replayed) = retried else {
            panic!(
                "the same code with the same hash again is the client retrying a response it \
                 never got: it must be Replayed, got {retried:?}"
            );
        };
        assert_eq!(
            replayed.credential_id, created.credential_id,
            "a replay hands back the credential that was created, not a new one"
        );
        assert_eq!(
            credentials_of(&pool, "ana").await,
            1,
            "a replay creates no second credential"
        );

        let stranger = db::claim_pairing_code(&pool, &retry_candidate, "pc-lab-1", hash(), later)
            .await
            .expect("a claim with another hash is a refusal, not an error");
        assert!(
            matches!(stranger, ClaimOutcome::Refused),
            "someone who has the code but not the client's secret must not be given its \
             credential: a different hash is Refused. Got {stranger:?}"
        );

        db::revoke_device(&pool, "ana", "pc-lab-1", later)
            .await
            .expect("revoking the device");
        let after_revoke = db::claim_pairing_code(&pool, &retry_candidate, "pc-lab-1", h1, later)
            .await
            .expect("a replay after the revocation");
        assert!(
            matches!(after_revoke, ClaimOutcome::Refused),
            "a revoked credential is not handed back by a replay: Refused. Got {after_revoke:?}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_code_read_before_its_account_was_revoked_cannot_be_redeemed() {
    in_a_scratch_database("brain_redeem_revoked", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let code = issue(&pool, "ana", None, t0()).await;
        let c = candidate(&pool, &code, t0()).await;

        db::revoke_account(&pool, "ana", t0() + Duration::minutes(1))
            .await
            .expect("revoking ana");
        let outcome = db::claim_pairing_code(&pool, &c, "pc", hash(), t0() + Duration::minutes(2))
            .await
            .expect("claiming after the revocation is a refusal, not an error");
        assert!(
            matches!(outcome, ClaimOutcome::Refused),
            "a candidate read before the revocation cannot be redeemed after it. Got {outcome:?}"
        );
        assert_eq!(
            credentials_of(&pool, "ana").await,
            0,
            "no credential was created for the revoked account"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn the_claim_itself_checks_the_account_even_when_the_code_row_is_still_there() {
    in_a_scratch_database("brain_redeem_revoked_row", |url| async move {
        let pool = migrated(&url).await;
        account(&pool, "ana", t0()).await;
        let code = issue(&pool, "ana", None, t0()).await;
        let c = candidate(&pool, &code, t0()).await;

        // revoke_account deletes the unused codes, which would hide a claim that forgot to check
        // the account: here the account is flagged revoked and the code row is left in place.
        sqlx::query("UPDATE brain_accounts SET revoked_at = $1 WHERE name = 'ana'")
            .bind(t0() + Duration::minutes(1))
            .execute(&pool)
            .await
            .expect("flagging the account revoked without deleting its codes");

        let outcome = db::claim_pairing_code(&pool, &c, "pc", hash(), t0() + Duration::minutes(2))
            .await
            .expect("claiming a code whose account is revoked");
        assert!(
            matches!(outcome, ClaimOutcome::Refused),
            "the claim statement must check `revoked_at IS NULL` on the account itself and not \
             lean on revoke_account having deleted the code first. Got {outcome:?}"
        );
        assert_eq!(
            credentials_of(&pool, "ana").await,
            0,
            "no credential was created for the revoked account"
        );
        pool.close().await;
    })
    .await;
}
