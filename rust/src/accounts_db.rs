//! Account, credential and pairing-code storage (the schema is migration 0065).
//!
//! PASS-1 STUB: only the types and signatures the slice-F tests compile against exist
//! here. Every function fails on purpose until pass 2 writes the real bodies and the
//! `0065_accounts` migration. Nothing in this file may be relied on before then.

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

const STUB: &str = "PASS-1 STUB: accounts_db is not implemented yet (slice F, pass 2)";

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

// PASS-1 STUB
pub async fn host_identity(_pool: &PgPool) -> Result<HostIdentity> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn add_account(_pool: &PgPool, _name: &str, _now: DateTime<Utc>) -> Result<AccountRow> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn list_accounts(_pool: &PgPool) -> Result<Vec<AccountSummary>> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn revoke_account(
    _pool: &PgPool,
    _name: &str,
    _now: DateTime<Utc>,
) -> Result<RevokeReport> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn revoke_device(
    _pool: &PgPool,
    _account: &str,
    _label: &str,
    _now: DateTime<Utc>,
) -> Result<usize> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn issue_pairing_code(
    _pool: &PgPool,
    _account: &str,
    _label: Option<&str>,
    _now: DateTime<Utc>,
) -> Result<IssuedCode> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn pairing_candidates(
    _pool: &PgPool,
    _now: DateTime<Utc>,
) -> Result<Vec<PairingCandidate>> {
    bail!(STUB)
}

// PASS-1 STUB: answers wrongly on purpose, so no test passes by luck.
pub fn classify_candidate(_c: &PairingCandidate, _now: DateTime<Utc>) -> CandidateState {
    CandidateState::AccountRevoked
}

// PASS-1 STUB
pub async fn claim_pairing_code<'e, E: sqlx::PgExecutor<'e>>(
    _exec: E,
    _c: &PairingCandidate,
    _label: &str,
    _secret_hash: [u8; 32],
    _now: DateTime<Utc>,
) -> Result<ClaimOutcome> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn credential_for_auth(_pool: &PgPool, _id: Uuid) -> Result<Option<CredentialRow>> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn touch_last_seen(_pool: &PgPool, _id: Uuid, _now: DateTime<Utc>) -> Result<()> {
    bail!(STUB)
}

// PASS-1 STUB
pub async fn purge_pairing_codes(_pool: &PgPool, _now: DateTime<Utc>) -> Result<u64> {
    bail!(STUB)
}
