use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action_kind = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action_kind {
        "append" => append(pool, &args).await,
        "verify" => verify(pool, &args).await,
        "tail" => tail(pool, &args).await,
        _ => anyhow::bail!("Invalid action: {action_kind}. Use append/verify/tail"),
    }
}

fn canonical_iso(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
}

pub fn audit_key() -> Option<Vec<u8>> {
    if let Ok(raw) = std::env::var("CUBA_AUDIT_KEY") {
        let raw = raw.trim();
        if !raw.is_empty() {
            return Some(raw.as_bytes().to_vec());
        }
    }
    let path = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|h| {
            let cache = std::path::PathBuf::from(h).join(".cache");
            let preferred = cache.join("memory-industry").join("audit_key");
            let legacy = cache.join("cuba-memorys").join("audit_key");
            if preferred.exists() || !legacy.exists() {
                preferred
            } else {
                legacy
            }
        })?;
    let key = std::fs::read_to_string(path).ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.as_bytes().to_vec())
}

pub fn compute_hash(
    prev_hash: &[u8],
    action: &str,
    payload: &[u8],
    created_at_iso: &str,
) -> Vec<u8> {
    match audit_key() {
        Some(key) => compute_hmac(&key, prev_hash, action, payload, created_at_iso),
        None => compute_sha256(prev_hash, action, payload, created_at_iso),
    }
}

fn compute_sha256(prev_hash: &[u8], action: &str, payload: &[u8], created_at_iso: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(prev_hash);
    h.update(b"|");
    h.update(action.as_bytes());
    h.update(b"|");
    h.update(payload);
    h.update(b"|");
    h.update(created_at_iso.as_bytes());
    h.finalize().to_vec()
}

fn compute_hmac(
    key: &[u8],
    prev_hash: &[u8],
    action: &str,
    payload: &[u8],
    created_at_iso: &str,
) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(prev_hash);
    mac.update(b"|");
    mac.update(action.as_bytes());
    mac.update(b"|");
    mac.update(payload);
    mac.update(b"|");
    mac.update(created_at_iso.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashKind {
    Hmac,
    Sha256,
    Neither,
}

pub fn classify_hash(
    stored: &[u8],
    prev_hash: &[u8],
    action: &str,
    payload: &[u8],
    created_at_iso: &str,
) -> HashKind {
    if let Some(key) = audit_key()
        && compute_hmac(&key, prev_hash, action, payload, created_at_iso) == stored
    {
        return HashKind::Hmac;
    }
    if compute_sha256(prev_hash, action, payload, created_at_iso) == stored {
        return HashKind::Sha256;
    }
    HashKind::Neither
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainVerdict {
    Protected,
    Unprotected,
    Downgraded,
    Broken,
}

pub fn ratchet(kind: HashKind, sealed: bool) -> ChainVerdict {
    match (kind, sealed) {
        (HashKind::Hmac, _) => ChainVerdict::Protected,
        (HashKind::Sha256, false) => ChainVerdict::Unprotected,
        (HashKind::Sha256, true) => ChainVerdict::Downgraded,
        (HashKind::Neither, _) => ChainVerdict::Broken,
    }
}

async fn append(pool: &PgPool, args: &Value) -> Result<Value> {
    let action = args
        .get("event_action")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("event_action is required"))?;
    let payload = args
        .get("payload")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    const MAX_RETRIES: usize = 5;
    let mut attempt = 0;
    loop {
        let mut tx = pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
            .execute(&mut *tx)
            .await
            .context("audit append requires SERIALIZABLE to keep the hash chain linear")?;

        const AUDIT_CHAIN_LOCK: i64 = 0x0CBA_A0D1_7106_0001;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(AUDIT_CHAIN_LOCK)
            .execute(&mut *tx)
            .await
            .context("taking the audit chain lock")?;

        let prev: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT current_hash FROM brain_audit_log ORDER BY id DESC LIMIT 1")
                .fetch_optional(&mut *tx)
                .await?;
        let prev_hash: Vec<u8> = prev.map(|(h,)| h).unwrap_or_default();

        let now = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(
            chrono::Utc::now().timestamp_micros(),
        )
        .expect("epoch in range");
        let now_iso = canonical_iso(now);
        let payload_bytes = serde_json::to_vec(&payload).context("serialize payload")?;
        let current = compute_hash(&prev_hash, action, &payload_bytes, &now_iso);

        let insert_res: Result<(i64,), sqlx::Error> = sqlx::query_as(
            "INSERT INTO brain_audit_log (prev_hash, action, payload, current_hash, created_at)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(if prev_hash.is_empty() {
            None
        } else {
            Some(&prev_hash)
        })
        .bind(action)
        .bind(&payload)
        .bind(&current)
        .bind(now)
        .fetch_one(&mut *tx)
        .await;

        match insert_res {
            Ok(row) => match tx.commit().await {
                Ok(()) => {
                    return Ok(serde_json::json!({
                        "action": "append",
                        "id": row.0,
                        "current_hash": hex::encode(&current),
                        "retries": attempt,
                    }));
                }
                Err(e) if is_serialization_failure(&e) && attempt < MAX_RETRIES => {
                    attempt += 1;
                    let backoff_ms = 5u64 * (1 << attempt);
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(e) => return Err(anyhow::Error::from(e)),
            },
            Err(e) if is_serialization_failure(&e) && attempt < MAX_RETRIES => {
                let _ = tx.rollback().await;
                attempt += 1;
                let backoff_ms = 5u64 * (1 << attempt);
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                continue;
            }
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(anyhow::Error::from(e).context("insert audit row"));
            }
        }
    }
}

fn is_serialization_failure(e: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = e
        && let Some(code) = db_err.code()
    {
        return code == "40001";
    }
    false
}

async fn verify(pool: &PgPool, args: &Value) -> Result<Value> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(10_000)
        .clamp(1, 1_000_000);

    type Row = (
        i64,
        Option<Vec<u8>>,
        String,
        Value,
        Vec<u8>,
        chrono::DateTime<chrono::Utc>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, prev_hash, action, payload, current_hash, created_at
         FROM brain_audit_log ORDER BY id ASC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let mut last_hash: Vec<u8> = Vec::new();
    let mut sealed = false;
    let mut unprotected = 0usize;
    for (id, prev_hash, action, payload, stored_hash, created_at) in &rows {
        let prev_for_check = prev_hash.clone().unwrap_or_default();
        if prev_for_check != last_hash {
            return Ok(serde_json::json!({
                "action": "verify",
                "ok": false,
                "first_break_id": id,
                "reason": "prev_hash mismatch with previous row's current_hash"
            }));
        }
        let payload_bytes = serde_json::to_vec(payload)?;
        let kind = classify_hash(
            stored_hash,
            &prev_for_check,
            action,
            &payload_bytes,
            &canonical_iso(*created_at),
        );
        match ratchet(kind, sealed) {
            ChainVerdict::Protected => sealed = true,
            ChainVerdict::Unprotected => unprotected += 1,
            ChainVerdict::Downgraded => {
                return Ok(serde_json::json!({
                    "action": "verify",
                    "ok": false,
                    "first_break_id": id,
                    "reason": "row is plain SHA-256 after the chain was already sealed with a \
                               key. A downgrade is what a forged row looks like: whoever wrote \
                               it could compute SHA-256 but not the HMAC"
                }));
            }
            ChainVerdict::Broken => {
                let reason = if sealed {
                    "current_hash recomputation differs (row tampered). Earlier rows verified \
                     with this key, so the key is the right one and the row is not"
                } else {
                    "current_hash recomputation differs: either the row was tampered with or \
                     CUBA_AUDIT_KEY is not the key this chain was written with. No row has \
                     verified with the current key yet, so both remain possible"
                };
                return Ok(serde_json::json!({
                    "action": "verify",
                    "ok": false,
                    "first_break_id": id,
                    "reason": reason
                }));
            }
        }
        last_hash = stored_hash.clone();
    }

    let mut out = serde_json::json!({
        "action": "verify",
        "ok": true,
        "rows_checked": rows.len(),
        "unprotected_rows": unprotected,
    });
    if unprotected > 0 {
        out["what_this_does_not_establish"] = serde_json::json!(format!(
            "{unprotected} of {} rows are plain SHA-256, which anyone with INSERT on \
             brain_audit_log can reproduce. They are accepted because they predate the key, \
             not because they are tamper-evident. Set CUBA_AUDIT_KEY to seal the chain from \
             here on.",
            rows.len()
        ));
    }
    Ok(out)
}

async fn tail(pool: &PgPool, args: &Value) -> Result<Value> {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(20)
        .clamp(1, 200);
    type Row = (i64, String, Value, chrono::DateTime<chrono::Utc>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, action, payload, created_at FROM brain_audit_log
         ORDER BY id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    let entries: Vec<Value> = rows
        .into_iter()
        .map(|(id, action, payload, created_at)| {
            serde_json::json!({
                "id": id,
                "action": action,
                "payload": payload,
                "created_at": created_at.to_rfc3339(),
            })
        })
        .collect();
    Ok(serde_json::json!({"action": "tail", "entries": entries.clone(), "count": entries.len()}))
}
