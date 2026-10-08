//! Account, credential and pairing-code storage (the schema is migration 0065).
//!
//! The clock is always the caller's: every function that decides something by time takes `now`,
//! and no SQL here reads the database's own `now()`. Secrets never reach this file in clear: a
//! credential arrives as its SHA-256 and a pairing code is stored as `code_key`.
//!
//! Lifecycle events (`account.add`, `account.revoke`, `account.revoke_device`,
//! `account.pair_issued`) are appended to the audit chain after the change they describe has
//! committed. The chain has its own serializable transaction, so the two cannot share one; a
//! failed append leaves the change in place and reports the error.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::accounts::{
    self, MAX_ACTIVE_PAIRING_CODES, PAIRING_CODE_TTL, PAIRING_RETAIN, code_key, normalize_code,
};

/// `pairing_candidates` never returns more than this many rows.
const CANDIDATE_LIMIT: i64 = 200;

#[derive(Debug, Clone)]
pub struct HostIdentity {
    pub host_id: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AccountRow {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CredentialSummary {
    pub id: Uuid,
    pub device_label: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct AccountSummary {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub credentials: Vec<CredentialSummary>,
}

/// What a revocation changed; a repeat reports zeros.
#[derive(Debug, Clone)]
pub struct RevokeReport {
    pub credentials_revoked: usize,
    pub codes_deleted: usize,
}

#[derive(Debug, Clone)]
pub struct IssuedCode {
    pub code: String,
    pub expires_at: DateTime<Utc>,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct PairingCandidate {
    pub id: Uuid,
    pub code_key: [u8; 32],
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub account_revoked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateState {
    Redeemable,
    Used,
    Expired,
    AccountRevoked,
}

#[derive(Debug, Clone)]
pub struct NewCredential {
    pub credential_id: Uuid,
    pub account: String,
    pub device_label: String,
}

#[derive(Debug, Clone)]
pub enum ClaimOutcome {
    Created(NewCredential),
    Replayed(NewCredential),
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialState {
    Active,
    Revoked,
    AccountRevoked,
}

#[derive(Debug, Clone)]
pub struct CredentialRow {
    pub id: Uuid,
    pub account: String,
    pub device_label: String,
    pub secret_hash: [u8; 32],
    pub state: CredentialState,
}

fn key32(bytes: Vec<u8>, what: &str) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|b: Vec<u8>| anyhow!("{what} holds {} bytes, expected 32", b.len()))
}

async fn audit(pool: &PgPool, action: &str, payload: Value) -> Result<()> {
    crate::handlers::archivo::handle(
        pool,
        json!({"action": "append", "event_action": action, "payload": payload}),
    )
    .await
    .with_context(|| format!("appending {action} to the audit chain"))?;
    Ok(())
}

pub async fn host_identity(pool: &PgPool) -> Result<HostIdentity> {
    let (host_id, created_at): (Uuid, DateTime<Utc>) =
        sqlx::query_as("SELECT host_id, created_at FROM brain_host_identity")
            .fetch_one(pool)
            .await
            .context("brain_host_identity holds exactly one row, created by migration 0065")?;
    Ok(HostIdentity {
        host_id,
        created_at,
    })
}

pub async fn add_account(pool: &PgPool, name: &str, now: DateTime<Utc>) -> Result<AccountRow> {
    let name = accounts::validate_account_name(name)?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO brain_accounts (name, created_at) VALUES ($1, $2)
         ON CONFLICT (name) DO NOTHING RETURNING id",
    )
    .bind(&name)
    .bind(now)
    .fetch_optional(pool)
    .await
    .context("creating the account")?;
    let id = id.ok_or_else(|| anyhow!("account {name:?} already exists"))?;
    audit(
        pool,
        "account.add",
        json!({"account": name, "actor": "cli"}),
    )
    .await?;
    Ok(AccountRow {
        id,
        name,
        created_at: now,
    })
}

type ListRow = (
    Uuid,
    String,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<Uuid>,
    Option<String>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

/// Rows arrive ordered by account, so an account is a run of consecutive rows.
fn fold_accounts(rows: Vec<ListRow>) -> Vec<AccountSummary> {
    let mut out: Vec<AccountSummary> = Vec::new();
    for (id, name, created_at, revoked_at, cred, label, cred_created, last_seen, cred_revoked) in
        rows
    {
        if out.last().is_none_or(|a| a.id != id) {
            out.push(AccountSummary {
                id,
                name,
                created_at,
                revoked_at,
                credentials: Vec::new(),
            });
        }
        if let (Some(cred), Some(label), Some(cred_created), Some(account)) =
            (cred, label, cred_created, out.last_mut())
        {
            account.credentials.push(CredentialSummary {
                id: cred,
                device_label: label,
                created_at: cred_created,
                last_seen_at: last_seen,
                revoked_at: cred_revoked,
            });
        }
    }
    out
}

pub async fn list_accounts(pool: &PgPool) -> Result<Vec<AccountSummary>> {
    let rows: Vec<ListRow> = sqlx::query_as(
        "SELECT a.id, a.name, a.created_at, a.revoked_at,
                c.id, c.device_label, c.created_at, c.last_seen_at, c.revoked_at
           FROM brain_accounts a
           LEFT JOIN brain_account_credentials c ON c.account_id = a.id
          ORDER BY a.name, a.created_at, c.created_at, c.id",
    )
    .fetch_all(pool)
    .await
    .context("listing accounts")?;
    Ok(fold_accounts(rows))
}

async fn lock_account(tx: &mut sqlx::PgConnection, name: &str) -> Result<Uuid> {
    let id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM brain_accounts WHERE name = $1 FOR UPDATE")
            .bind(name)
            .fetch_optional(tx)
            .await?;
    id.ok_or_else(|| anyhow!("account {name:?} does not exist"))
}

/// Returns whether the account was revoked by THIS call, and what else it changed.
async fn revoke_rows(
    tx: &mut sqlx::PgConnection,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<(bool, RevokeReport)> {
    let newly = sqlx::query(
        "UPDATE brain_accounts SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let credentials_revoked = sqlx::query(
        "UPDATE brain_account_credentials SET revoked_at = $2
          WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected() as usize;
    let codes_deleted =
        sqlx::query("DELETE FROM brain_pairing_codes WHERE account_id = $1 AND used_at IS NULL")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;
    let report = RevokeReport {
        credentials_revoked,
        codes_deleted,
    };
    Ok((newly > 0, report))
}

async fn revoke_in_tx(
    pool: &PgPool,
    name: &str,
    now: DateTime<Utc>,
) -> Result<(bool, RevokeReport)> {
    let mut tx = pool.begin().await?;
    let id = lock_account(&mut tx, name).await?;
    let outcome = revoke_rows(&mut tx, id, now).await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Revoking sets `revoked_at` on the account and on every credential that is still active, and
/// deletes the unused pairing codes; used codes stay as the record of how each device was
/// paired. A repeat changes nothing, so the first revocation instant is the one that sticks.
pub async fn revoke_account(pool: &PgPool, name: &str, now: DateTime<Utc>) -> Result<RevokeReport> {
    let name = accounts::validate_account_name(name)?;
    let (newly, report) = revoke_in_tx(pool, &name, now).await?;
    if newly {
        audit(
            pool,
            "account.revoke",
            json!({"account": name, "actor": "cli"}),
        )
        .await?;
    }
    Ok(report)
}

/// The label is matched without regard to case; every active device with it is revoked.
pub async fn revoke_device(
    pool: &PgPool,
    account: &str,
    label: &str,
    now: DateTime<Utc>,
) -> Result<usize> {
    let account = accounts::validate_account_name(account)?;
    let revoked = sqlx::query(
        "UPDATE brain_account_credentials c SET revoked_at = $3
           FROM brain_accounts a
          WHERE a.id = c.account_id AND a.name = $1
            AND lower(c.device_label) = lower($2) AND c.revoked_at IS NULL",
    )
    .bind(&account)
    .bind(label)
    .bind(now)
    .execute(pool)
    .await?
    .rows_affected() as usize;
    if revoked == 0 {
        bail!("account {account:?} has no active device labelled {label:?}");
    }
    audit(
        pool,
        "account.revoke_device",
        json!({"account": account, "device": label, "actor": "cli"}),
    )
    .await?;
    Ok(revoked)
}

async fn active_account_id(pool: &PgPool, name: &str) -> Result<Uuid> {
    let found: Option<(Uuid, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT id, revoked_at FROM brain_accounts WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await?;
    match found {
        Some((id, None)) => Ok(id),
        Some((_, Some(_))) => bail!("account {name:?} is revoked"),
        None => bail!("account {name:?} does not exist"),
    }
}

async fn ensure_code_capacity(pool: &PgPool, now: DateTime<Utc>) -> Result<()> {
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_pairing_codes WHERE used_at IS NULL AND expires_at > $1",
    )
    .bind(now)
    .fetch_one(pool)
    .await?;
    if active >= MAX_ACTIVE_PAIRING_CODES as i64 {
        bail!(
            "too many active pairing codes ({active}, the limit is {MAX_ACTIVE_PAIRING_CODES}): \
             wait for them to expire or revoke the account they belong to"
        );
    }
    Ok(())
}

/// The checks before a code is minted, with the inputs in their canonical form.
async fn prepare_issue(
    pool: &PgPool,
    account: &str,
    label: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(String, Option<String>, Uuid)> {
    let account = accounts::validate_account_name(account)?;
    let label = label.map(accounts::validate_device_label).transpose()?;
    let account_id = active_account_id(pool, &account).await?;
    ensure_code_capacity(pool, now).await?;
    Ok((account, label, account_id))
}

/// Mints a code and stores its key only; the code itself leaves with the caller.
async fn store_code(
    pool: &PgPool,
    account_id: Uuid,
    label: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(String, DateTime<Utc>)> {
    let code = accounts::generate_code();
    let key = code_key(&normalize_code(&code)?);
    let expires_at = now + PAIRING_CODE_TTL;
    sqlx::query(
        "INSERT INTO brain_pairing_codes (account_id, code_key, device_label, created_at, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(account_id)
    .bind(key.as_slice())
    .bind(label)
    .bind(now)
    .bind(expires_at)
    .execute(pool)
    .await
    .context("storing the pairing code")?;
    Ok((code, expires_at))
}

fn issued_payload(account: &str, label: Option<&str>) -> Value {
    let mut payload = json!({"account": account, "actor": "cli"});
    if let Some(device) = label {
        payload["device"] = json!(device);
    }
    payload
}

pub async fn issue_pairing_code(
    pool: &PgPool,
    account: &str,
    label: Option<&str>,
    now: DateTime<Utc>,
) -> Result<IssuedCode> {
    let (account, label, account_id) = prepare_issue(pool, account, label, now).await?;
    let (code, expires_at) = store_code(pool, account_id, label.as_deref(), now).await?;
    audit(
        pool,
        "account.pair_issued",
        issued_payload(&account, label.as_deref()),
    )
    .await?;
    let host = host_identity(pool).await?;
    Ok(IssuedCode {
        code,
        expires_at,
        fingerprint: accounts::fingerprint(&host.host_id),
    })
}

type CandidateRow = (Uuid, Vec<u8>, DateTime<Utc>, Option<DateTime<Utc>>, bool);

fn candidate_from(row: CandidateRow) -> Result<PairingCandidate> {
    let (id, key, expires_at, used_at, account_revoked) = row;
    Ok(PairingCandidate {
        id,
        code_key: key32(key, "code_key")?,
        expires_at,
        used_at,
        account_revoked,
    })
}

/// Every code issued within `PAIRING_RETAIN`, used or expired or of a revoked account included,
/// so the caller can tell a late client why its code no longer works.
pub async fn pairing_candidates(
    pool: &PgPool,
    now: DateTime<Utc>,
) -> Result<Vec<PairingCandidate>> {
    let rows: Vec<CandidateRow> = sqlx::query_as(
        "SELECT p.id, p.code_key, p.expires_at, p.used_at, a.revoked_at IS NOT NULL
           FROM brain_pairing_codes p
           JOIN brain_accounts a ON a.id = p.account_id
          WHERE p.created_at > $1
          ORDER BY p.created_at DESC, p.id
          LIMIT $2",
    )
    .bind(now - PAIRING_RETAIN)
    .bind(CANDIDATE_LIMIT)
    .fetch_all(pool)
    .await
    .context("reading pairing candidates")?;
    rows.into_iter().map(candidate_from).collect()
}

/// A code is valid only while `now < expires_at`: at the expiry instant itself it is expired.
pub fn classify_candidate(c: &PairingCandidate, now: DateTime<Utc>) -> CandidateState {
    if c.used_at.is_some() {
        CandidateState::Used
    } else if c.account_revoked {
        CandidateState::AccountRevoked
    } else if now >= c.expires_at {
        CandidateState::Expired
    } else {
        CandidateState::Redeemable
    }
}

/// Redeems a code in ONE statement, so two connections cannot both read it as unused.
///
/// `claimed` takes the code row (an UPDATE, which blocks a second claimer until the first
/// commits; READ COMMITTED then re-checks `used_at IS NULL` on the new row version and finds
/// nothing). `created` inserts the credential from what `claimed` returned, and the code points
/// at it in the same statement. `replayed` answers a client that lost the response: the same
/// code with the same `secret_hash` and a credential that is still active. Expiry, use and the
/// account's revocation are all judged at the caller's `now`, never the database's.
const CLAIM_SQL: &str = "
WITH claimed AS (
    UPDATE brain_pairing_codes p
       SET used_at = $2::timestamptz, credential_id = $3::uuid
      FROM brain_accounts a
     WHERE p.id = $1::uuid AND a.id = p.account_id
       AND p.used_at IS NULL AND p.expires_at > $2::timestamptz AND a.revoked_at IS NULL
    RETURNING p.account_id, p.device_label AS fixed_label, a.name AS account
), created AS (
    INSERT INTO brain_account_credentials (id, account_id, device_label, secret_hash, created_at)
    SELECT $3::uuid, account_id, COALESCE(fixed_label, $4::text), $5::bytea, $2::timestamptz
      FROM claimed
    RETURNING id, device_label
), replayed AS (
    SELECT cr.id, cr.device_label, a.name AS account
      FROM brain_pairing_codes p
      JOIN brain_account_credentials cr ON cr.id = p.credential_id
      JOIN brain_accounts a ON a.id = cr.account_id
     WHERE p.id = $1::uuid AND cr.secret_hash = $5::bytea
       AND cr.revoked_at IS NULL AND a.revoked_at IS NULL
       AND NOT EXISTS (SELECT 1 FROM claimed)
)
SELECT 'created' AS kind, created.id, claimed.account, created.device_label
  FROM created, claimed
UNION ALL
SELECT 'replayed', id, account, device_label FROM replayed";

pub async fn claim_pairing_code<'e, E: sqlx::PgExecutor<'e>>(
    exec: E,
    c: &PairingCandidate,
    label: &str,
    secret_hash: [u8; 32],
    now: DateTime<Utc>,
) -> Result<ClaimOutcome> {
    let row: Option<(String, Uuid, String, String)> = sqlx::query_as(CLAIM_SQL)
        .bind(c.id)
        .bind(now)
        .bind(Uuid::new_v4())
        .bind(label)
        .bind(secret_hash.as_slice())
        .fetch_optional(exec)
        .await
        .context("redeeming the pairing code")?;
    Ok(match row {
        None => ClaimOutcome::Refused,
        Some((kind, credential_id, account, device_label)) => {
            let credential = NewCredential {
                credential_id,
                account,
                device_label,
            };
            if kind == "created" {
                ClaimOutcome::Created(credential)
            } else {
                ClaimOutcome::Replayed(credential)
            }
        }
    })
}

fn state_of(account_revoked: bool, revoked: bool) -> CredentialState {
    match (account_revoked, revoked) {
        (true, _) => CredentialState::AccountRevoked,
        (false, true) => CredentialState::Revoked,
        (false, false) => CredentialState::Active,
    }
}

type AuthRow = (Uuid, String, String, Vec<u8>, bool, bool);

fn credential_from(row: AuthRow) -> Result<CredentialRow> {
    let (id, account, device_label, hash, revoked, account_revoked) = row;
    Ok(CredentialRow {
        id,
        account,
        device_label,
        secret_hash: key32(hash, "secret_hash")?,
        state: state_of(account_revoked, revoked),
    })
}

pub async fn credential_for_auth(pool: &PgPool, id: Uuid) -> Result<Option<CredentialRow>> {
    let row: Option<AuthRow> = sqlx::query_as(
        "SELECT c.id, a.name, c.device_label, c.secret_hash,
                c.revoked_at IS NOT NULL, a.revoked_at IS NOT NULL
           FROM brain_account_credentials c
           JOIN brain_accounts a ON a.id = c.account_id
          WHERE c.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .context("looking up the credential")?;
    row.map(credential_from).transpose()
}

/// At most one write a minute per credential, so authenticating is not a database write.
pub async fn touch_last_seen(pool: &PgPool, id: Uuid, now: DateTime<Utc>) -> Result<()> {
    sqlx::query(
        "UPDATE brain_account_credentials SET last_seen_at = $2::timestamptz
          WHERE id = $1
            AND (last_seen_at IS NULL OR last_seen_at < $2::timestamptz - interval '60 seconds')",
    )
    .bind(id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Deletes codes used, or expired, more than `PAIRING_RETAIN` before `now`.
pub async fn purge_pairing_codes(pool: &PgPool, now: DateTime<Utc>) -> Result<u64> {
    let purged =
        sqlx::query("DELETE FROM brain_pairing_codes WHERE used_at < $1 OR expires_at < $1")
            .bind(now - PAIRING_RETAIN)
            .execute(pool)
            .await?
            .rows_affected();
    Ok(purged)
}
