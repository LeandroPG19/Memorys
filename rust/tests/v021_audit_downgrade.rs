use sha2::{Digest, Sha256};

fn sha256_chain(prev: &[u8], action: &str, payload: &[u8], stamp: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(prev);
    h.update(b"|");
    h.update(action.as_bytes());
    h.update(b"|");
    h.update(payload);
    h.update(b"|");
    h.update(stamp.as_bytes());
    h.finalize().to_vec()
}

fn canonical_iso(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
}

async fn verify(pool: &sqlx::PgPool) -> serde_json::Value {
    let envelope = memory_industry::handlers::dispatch(
        pool,
        "cuba_archivo",
        serde_json::json!({"action": "verify", "limit": 1_000_000}),
    )
    .await
    .expect("verify runs");
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the verdict is JSON inside that envelope")
}

async fn append(pool: &sqlx::PgPool, action: &str, i: i32) {
    memory_industry::handlers::dispatch(
        pool,
        "cuba_archivo",
        serde_json::json!({"action": "append", "event_action": action, "payload": {"i": i}}),
    )
    .await
    .expect("appending through the handler is the only supported writer");
}

#[tokio::test]
#[ignore]
async fn a_forged_sha256_row_appended_after_the_key_is_rejected_as_a_downgrade() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let scratch_home = std::env::temp_dir().join(format!("cuba-downgrade-{}", std::process::id()));
    std::fs::create_dir_all(&scratch_home).expect("a scratch HOME keeps a real key file out");
    unsafe { std::env::set_var("HOME", &scratch_home) };
    // Windows resolves the key via USERPROFILE when tests forget to override it;
    // earlier gate phases would then HMAC every append under the operator's real key.
    unsafe { std::env::set_var("USERPROFILE", &scratch_home) };
    unsafe { std::env::remove_var("CUBA_AUDIT_KEY") };

    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let marker = format!("downgrade_{}", &uuid::Uuid::new_v4().to_string()[..8]);

    let before = verify(&pool).await;
    assert_eq!(
        before["ok"], true,
        "the chain has to start unsealed and unbroken, because the ratchet is a property of \
         the whole chain rather than of any row. Earlier phases of the same gate run do append \
         audit rows, and that is fine — they are unkeyed, so they are exactly the pre-key state \
         this test needs. What is not fine is running twice against one database: this test \
         plants a forged row on purpose and never removes it, so the second run sees a chain \
         that is already broken. The gate provisions a throwaway database per run. It is \
         asserted rather than truncated because a test that clears brain_audit_log is one wrong \
         DATABASE_URL away from destroying the audit trail it exists to protect. Got: {before}"
    );
    let unprotected_before = before["unprotected_rows"].as_u64().unwrap_or(0);

    append(&pool, &marker, 1).await;
    append(&pool, &marker, 2).await;

    unsafe { std::env::set_var("CUBA_AUDIT_KEY", "the-operator-finally-set-a-key") };
    append(&pool, &marker, 3).await;

    let verdict = verify(&pool).await;
    assert_eq!(
        verdict["ok"], true,
        "two rows older than the key plus one written under it is an honest migration, and \
         it has to keep verifying — a ratchet that reports every pre-key row as tampered is \
         worse than no ratchet, because the operator learns to ignore the alarm. Got: {verdict}"
    );
    assert_eq!(
        verdict["unprotected_rows"].as_u64().expect("counted"),
        unprotected_before + 2,
        "the two rows this test appended before the key are added to whatever was already \
         unprotected, and the keyed row is not. Counting them out loud is the point: a verifier \
         that folds unprotected rows into a green tick is reporting a security property the \
         chain does not have. Got: {verdict}"
    );

    let (last_hash, last_id): (Vec<u8>, i64) =
        sqlx::query_as("SELECT current_hash, id FROM brain_audit_log ORDER BY id DESC LIMIT 1")
            .fetch_one(&pool)
            .await
            .expect("the chain has a head");

    let payload = serde_json::json!({"forged": true});
    let payload_bytes = serde_json::to_vec(&payload).expect("serialising the forged payload");
    let stamp = chrono::Utc::now();
    let forged_hash = sha256_chain(&last_hash, &marker, &payload_bytes, &canonical_iso(stamp));

    sqlx::query(
        "INSERT INTO brain_audit_log (prev_hash, action, payload, current_hash, created_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(&last_hash)
    .bind(&marker)
    .bind(&payload)
    .bind(&forged_hash)
    .bind(stamp)
    .execute(&pool)
    .await
    .expect(
        "INSERT is exactly the capability the attacker has: the append-only trigger blocks \
         UPDATE and DELETE and deliberately allows INSERT, so this is not a test artefact",
    );

    let verdict = verify(&pool).await;

    assert_eq!(
        verdict["ok"], false,
        "the forged row links correctly to the head and its SHA-256 recomputes, so every \
         check the old verifier ran says yes. What gives it away is that it is SHA-256 at \
         all: the chain was already sealed with a key, and whoever wrote this could hash \
         but could not sign. Got: {verdict}"
    );
    assert!(
        verdict["first_break_id"].as_i64().expect("an id") > last_id,
        "and it has to point at the forged row, not at one of the honest pre-key rows"
    );
    assert!(
        verdict["reason"]
            .as_str()
            .expect("a reason")
            .contains("sealed"),
        "the reason must say why a row that hashes correctly is still rejected, otherwise \
         the operator reads it as a false positive and turns the key off. Got: {verdict}"
    );

    unsafe { std::env::remove_var("CUBA_AUDIT_KEY") };
    let _ = std::fs::remove_dir_all(&scratch_home);
}
