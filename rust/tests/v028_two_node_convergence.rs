use serde_json::{Value, json};
use uuid::Uuid;

fn peer_pool_env() -> String {
    std::env::var("CUBA_PEER_DATABASE_URL")
        .expect("CUBA_PEER_DATABASE_URL env var required: the second throwaway node")
}

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0027;

async fn own_the_sync_dir(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect(
            "CUBA_SYNC_DIR is one process-wide environment variable, so two tests in this binary \
             running at once point the exporter at each other's directories. Serialising on the \
             same advisory lock every other sync test uses is what keeps them apart; running \
             with --test-threads=1 hides the problem instead of fixing it, and the gate does not \
             pass that flag",
        );
    tx
}

async fn pool_a() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to node A")
}

async fn pool_b() -> sqlx::PgPool {
    memory_industry::db::create_pool(&peer_pool_env())
        .await
        .expect("connect to node B")
}

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the result is JSON inside that envelope")
}

fn unique_name(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

fn scratch_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("cuba-refuter-{label}-{}", Uuid::new_v4()))
}

#[tokio::test]
#[ignore]
async fn a_fact_keeps_its_entity_link_when_the_entity_is_remapped_by_name() {
    let a = pool_a().await;
    let b = pool_b().await;
    let bundle = scratch_dir("fact-remap");
    std::fs::create_dir_all(&bundle).expect("scratch bundle dir");
    let _owns = own_the_sync_dir(&a).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let shared_name = unique_name("shared_concept");

    let entity_b: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&shared_name)
    .fetch_one(&b)
    .await
    .expect("seed the entity B already has under this name");

    let entity_a: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&shared_name)
    .fetch_one(&a)
    .await
    .expect("seed A's own copy of the same-named entity, a different uuid by construction");
    assert_ne!(entity_a, entity_b, "gen_random_uuid() must not collide");

    let fact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_facts (subject, predicate, object, subject_entity_id, valid_from, observed_at)
         VALUES ($1, 'is_a', 'test-concept', $2, NOW(), NOW()) RETURNING fact_id",
    )
    .bind(&shared_name)
    .bind(entity_a)
    .fetch_one(&a)
    .await
    .expect("seed the fact linked to A's entity");

    call(
        &a,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let imported = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;

    let landed_subject_entity_id: Option<Uuid> =
        sqlx::query_scalar("SELECT subject_entity_id FROM brain_facts WHERE fact_id = $1")
            .bind(fact_id)
            .fetch_one(&b)
            .await
            .expect(
                "the fact itself must have landed — subject/predicate/object are not at stake here",
            );

    eprintln!("import result: {imported}");
    eprintln!(
        "entity_a={entity_a} entity_b={entity_b} landed subject_entity_id={landed_subject_entity_id:?}"
    );

    assert_eq!(
        landed_subject_entity_id,
        Some(entity_b),
        "the fact is about the entity named {shared_name:?}, which DOES exist on B (as {entity_b}) \
         — the name-based dedup even ran and is recorded internally as a remap from {entity_a} to \
         {entity_b}. But facts.json import (handlers/sync.rs ~line 2187) resolves \
         subject_entity_id with a raw `SELECT id FROM brain_entities WHERE id = u.subject_entity_id` \
         instead of routing it through the same `resolve(&remapped, ..)` used for observations, \
         episodes and relations. Got {landed_subject_entity_id:?} instead: the fact survives as \
         text but loses its entity-graph link, which is exactly what subject_entity_id exists to \
         provide (find_similar_entities, graph traversal, entity-scoped fact queries)."
    );

    sqlx::query("DELETE FROM brain_facts WHERE fact_id = $1")
        .bind(fact_id)
        .execute(&a)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_a)
        .execute(&a)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_facts WHERE subject = $1")
        .bind(&shared_name)
        .execute(&b)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_b)
        .execute(&b)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn a_full_round_trip_unions_facts_procedures_and_source_trust_on_both_sides() {
    let a = pool_a().await;
    let b = pool_b().await;
    let bundle = scratch_dir("union");
    std::fs::create_dir_all(&bundle).expect("scratch bundle dir");
    let _owns = own_the_sync_dir(&a).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let subj_a = unique_name("only_a_subject");
    let subj_b = unique_name("only_b_subject");
    let proc_a = unique_name("only_a_procedure");
    let proc_b = unique_name("only_b_procedure");
    let source_a = unique_name("only_a_source");
    let source_b = unique_name("only_b_source");

    sqlx::query(
        "INSERT INTO brain_facts (subject, predicate, object, valid_from, observed_at) \
         VALUES ($1, 'lives_on', 'node A', NOW(), NOW())",
    )
    .bind(&subj_a)
    .execute(&a)
    .await
    .expect("seed A's fact");
    sqlx::query(
        "INSERT INTO brain_facts (subject, predicate, object, valid_from, observed_at) \
         VALUES ($1, 'lives_on', 'node B', NOW(), NOW())",
    )
    .bind(&subj_b)
    .execute(&b)
    .await
    .expect("seed B's fact");

    sqlx::query("INSERT INTO brain_procedures (name, steps) VALUES ($1, '[]'::jsonb)")
        .bind(&proc_a)
        .execute(&a)
        .await
        .expect("seed A's procedure");
    sqlx::query("INSERT INTO brain_procedures (name, steps) VALUES ($1, '[]'::jsonb)")
        .bind(&proc_b)
        .execute(&b)
        .await
        .expect("seed B's procedure");

    sqlx::query("INSERT INTO brain_source_trust (source, alpha, beta) VALUES ($1, 3, 1)")
        .bind(&source_a)
        .execute(&a)
        .await
        .expect("seed A's source trust");
    sqlx::query("INSERT INTO brain_source_trust (source, alpha, beta) VALUES ($1, 5, 2)")
        .bind(&source_b)
        .execute(&b)
        .await
        .expect("seed B's source trust");

    call(
        &a,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    let imported_b = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    eprintln!("A->B: {imported_b}");

    call(
        &b,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    let imported_a = call(
        &a,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    eprintln!("B->A: {imported_a}");

    for (label, pool) in [("A", &a), ("B", &b)] {
        let has_subj_a: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM brain_facts WHERE subject = $1)")
                .bind(&subj_a)
                .fetch_one(pool)
                .await
                .expect("check subj_a");
        let has_subj_b: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM brain_facts WHERE subject = $1)")
                .bind(&subj_b)
                .fetch_one(pool)
                .await
                .expect("check subj_b");
        assert!(
            has_subj_a,
            "{label} is missing A's fact after the round trip"
        );
        assert!(
            has_subj_b,
            "{label} is missing B's fact after the round trip"
        );

        let has_proc_a: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM brain_procedures WHERE name = $1)")
                .bind(&proc_a)
                .fetch_one(pool)
                .await
                .expect("check proc_a");
        let has_proc_b: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM brain_procedures WHERE name = $1)")
                .bind(&proc_b)
                .fetch_one(pool)
                .await
                .expect("check proc_b");
        assert!(
            has_proc_a,
            "{label} is missing A's procedure after the round trip"
        );
        assert!(
            has_proc_b,
            "{label} is missing B's procedure after the round trip"
        );

        let has_src_a: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM brain_source_trust WHERE source = $1)",
        )
        .bind(&source_a)
        .fetch_one(pool)
        .await
        .expect("check source_a");
        let has_src_b: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM brain_source_trust WHERE source = $1)",
        )
        .bind(&source_b)
        .fetch_one(pool)
        .await
        .expect("check source_b");
        assert!(
            has_src_a,
            "{label} is missing A's source_trust row after the round trip"
        );
        assert!(
            has_src_b,
            "{label} is missing B's source_trust row after the round trip"
        );
    }

    for pool in [&a, &b] {
        sqlx::query("DELETE FROM brain_facts WHERE subject IN ($1, $2)")
            .bind(&subj_a)
            .bind(&subj_b)
            .execute(pool)
            .await
            .ok();
        sqlx::query("DELETE FROM brain_procedures WHERE name IN ($1, $2)")
            .bind(&proc_a)
            .bind(&proc_b)
            .execute(pool)
            .await
            .ok();
        sqlx::query("DELETE FROM brain_source_trust WHERE source IN ($1, $2)")
            .bind(&source_a)
            .bind(&source_b)
            .execute(pool)
            .await
            .ok();
    }
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn a_9000_char_observation_keeps_its_text_but_not_its_chunks() {
    let a = pool_a().await;
    let b = pool_b().await;
    let bundle = scratch_dir("bigobs");
    std::fs::create_dir_all(&bundle).expect("scratch bundle dir");
    let _owns = own_the_sync_dir(&a).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity_name = unique_name("bigobs_entity");
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&entity_name)
    .fetch_one(&a)
    .await
    .expect("seed the entity");

    let phrase = "el gato subio al tejado y observo la ciudad entera con emoji 🐈 y ñ. ";
    let phrase_chars = phrase.chars().count();
    let big_content: String = phrase.repeat(9000 / phrase_chars + 1);
    assert!(big_content.chars().count() >= 9000);

    let obs_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2) RETURNING id",
    )
    .bind(entity_id)
    .bind(&big_content)
    .fetch_one(&a)
    .await
    .expect("seed the big observation");

    sqlx::query(
        "INSERT INTO brain_observation_chunks (observation_id, chunk_index, content) \
         VALUES ($1, 0, $2)",
    )
    .bind(obs_id)
    .bind(&big_content[..500])
    .execute(&a)
    .await
    .expect("seed a chunk on A, the way the backfill job would have");

    call(
        &a,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    let imported = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    eprintln!("import: {imported}");

    let landed_content: Option<String> =
        sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_optional(&b)
            .await
            .expect("read the content back");
    assert_eq!(
        landed_content.as_deref(),
        Some(big_content.as_str()),
        "the full text must travel byte for byte"
    );

    let chunks_on_b: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_observation_chunks WHERE observation_id = $1",
    )
    .bind(obs_id)
    .fetch_one(&b)
    .await
    .expect("count chunks on B");
    assert_eq!(
        chunks_on_b, 0,
        "confirms the known gap: brain_observation_chunks is not part of the bundle at all (no \
         struct in sync/chunk.rs, no read/write in handlers/sync.rs). What is lost immediately \
         after import is not the observation's text — it is its searchability past the \
         embedder's truncation limit, until B's own REM backfill job (protocol.rs \
         rem_backfill_chunks) re-chunks and re-embeds it locally, which needs the embedding \
         model present and does not happen as part of sync at all"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&a)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&b)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn breaking_content_round_trips_or_fails_loudly_never_silently() {
    let a = pool_a().await;
    let b = pool_b().await;
    let bundle = scratch_dir("breaking");
    std::fs::create_dir_all(&bundle).expect("scratch bundle dir");
    let _owns = own_the_sync_dir(&a).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity_name = unique_name("breaking_entity");
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&entity_name)
    .fetch_one(&a)
    .await
    .expect("seed the entity");

    let tricky =
        "quotes \"like this\" and 'this'\nnewline\ttab\r\nCRLF\n{\"json\":\"inside\",\"n\":1}\némoji 🐈🔥💀 y ñ á é"
            .to_string();
    let obs_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2) RETURNING id",
    )
    .bind(entity_id)
    .bind(&tricky)
    .fetch_one(&a)
    .await
    .expect("seed the tricky observation");

    let hundred_kb = "x".repeat(100 * 1024);
    let big_obs_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2) RETURNING id",
    )
    .bind(entity_id)
    .bind(&hundred_kb)
    .fetch_one(&a)
    .await
    .expect("seed the 100KB observation");

    let long_name = "n".repeat(300);
    let long_entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&long_name)
    .fetch_one(&a)
    .await
    .expect("seed a 300-char-named entity");

    let export_result = memory_industry::handlers::sync::handle(
        &a,
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    match &export_result {
        Ok(v) => eprintln!("export succeeded: {v}"),
        Err(e) => eprintln!("export failed: {e:#}"),
    }

    if export_result.is_ok() {
        let imported = call(
            &b,
            "cuba_sync",
            json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
        )
        .await;
        eprintln!("import: {imported}");

        let landed_tricky: Option<String> =
            sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
                .bind(obs_id)
                .fetch_optional(&b)
                .await
                .expect("read tricky content back");
        assert_eq!(
            landed_tricky.as_deref(),
            Some(tricky.as_str()),
            "quotes/newlines/emoji/embedded-JSON must round-trip byte for byte through \
             serde_json + jsonb_to_recordset"
        );

        let landed_big: Option<String> =
            sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
                .bind(big_obs_id)
                .fetch_optional(&b)
                .await
                .expect("read 100KB content back");
        assert_eq!(
            landed_big.as_ref().map(|s| s.len()),
            Some(hundred_kb.len()),
            "a 100KB observation must not be truncated"
        );

        let long_name_landed: Option<String> =
            sqlx::query_scalar("SELECT name FROM brain_entities WHERE id = $1")
                .bind(long_entity_id)
                .fetch_optional(&b)
                .await
                .expect("read the long name back");
        eprintln!(
            "300-char entity name landed on B: {:?}",
            long_name_landed.is_some()
        );
        assert_eq!(
            long_name_landed.as_deref(),
            Some(long_name.as_str()),
            "a 300-char entity name must not be silently dropped by the export/import round trip"
        );
    } else {
        eprintln!(
            "export failed outright on a bundle containing a 300-char entity name — the slug() \
             filename for it is name.len() + 1 (dash) + 8 (short id) + 5 (.json) bytes, which \
             exceeds ext4's 255-byte NAME_MAX. This means nothing else in the SAME scope=all \
             export reaches disk either if this entity sorts before it — one bad name blocks \
             every OTHER entity's sync too, loudly (an Err, not silence), but still a full \
             convergence failure for the whole node until the offending entity is renamed or \
             removed."
        );
    }

    let base = unique_name("CaseVariant");
    for variant in [base.clone(), base.to_lowercase(), base.to_uppercase()] {
        let _ =
            sqlx::query("INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept')")
                .bind(&variant)
                .execute(&a)
                .await;
    }
    let case_variants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE lower(name) = lower($1)")
            .bind(&base)
            .fetch_one(&a)
            .await
            .expect("count case variants");
    eprintln!(
        "case-only name variants of {base:?} coexisting in ONE database (not a sync question at \
         all — brain_entities.name is a case-sensitive UNIQUE TEXT column with no citext/lower \
         index): {case_variants}"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&a)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&b)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(long_entity_id)
        .execute(&a)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(long_entity_id)
        .execute(&b)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE lower(name) = lower($1)")
        .bind(&base)
        .execute(&a)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn a_corrupted_bundle_file_leaves_the_database_untouched_and_unmarked() {
    let a = pool_a().await;
    let b = pool_b().await;
    let bundle = scratch_dir("corrupt");
    std::fs::create_dir_all(&bundle).expect("scratch bundle dir");
    let _owns = own_the_sync_dir(&a).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity_name = unique_name("corrupt_entity");
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&entity_name)
    .fetch_one(&a)
    .await
    .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content) VALUES ($1, 'will not arrive')",
    )
    .bind(entity_id)
    .execute(&a)
    .await
    .expect("seed an observation that must never land on B");

    call(
        &a,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let entities_dir = bundle.join("entities");
    let mut corrupted_one = false;
    for entry in std::fs::read_dir(&entities_dir).expect("read entities dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|e| e == "json") {
            std::fs::write(&path, b"{ this is not valid json at all")
                .expect("corrupt one entity file");
            corrupted_one = true;
            break;
        }
    }
    assert!(
        corrupted_one,
        "the export must have produced at least one entity file to corrupt"
    );

    let (entities_before, observations_before, sync_state_rows_before): (i64, i64, i64) = (
        sqlx::query_scalar("SELECT count(*) FROM brain_entities")
            .fetch_one(&b)
            .await
            .expect("count entities before"),
        sqlx::query_scalar("SELECT count(*) FROM brain_observations")
            .fetch_one(&b)
            .await
            .expect("count observations before"),
        sqlx::query_scalar("SELECT count(*) FROM brain_sync_state")
            .fetch_one(&b)
            .await
            .expect("count sync_state before"),
    );

    let import_result = memory_industry::handlers::sync::handle(
        &b,
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    assert!(
        import_result.is_err(),
        "a corrupted bundle file must fail the import loudly, got: {import_result:?}"
    );
    eprintln!(
        "import failed as expected: {:#}",
        import_result.unwrap_err()
    );

    let (entities_after, observations_after, sync_state_rows_after): (i64, i64, i64) = (
        sqlx::query_scalar("SELECT count(*) FROM brain_entities")
            .fetch_one(&b)
            .await
            .expect("count entities after"),
        sqlx::query_scalar("SELECT count(*) FROM brain_observations")
            .fetch_one(&b)
            .await
            .expect("count observations after"),
        sqlx::query_scalar("SELECT count(*) FROM brain_sync_state")
            .fetch_one(&b)
            .await
            .expect("count sync_state after"),
    );

    assert_eq!(
        entities_before, entities_after,
        "a failed import must not have inserted any entity"
    );
    assert_eq!(
        observations_before, observations_after,
        "a failed import must not have inserted any observation — the corruption is caught by \
         validate_bundle, which runs before pool.begin(), so nothing should even have opened a \
         transaction"
    );
    assert_eq!(
        sync_state_rows_before, sync_state_rows_after,
        "the bundle must not be recorded as imported — a later retry with the fixed file must \
         still be treated as new work"
    );

    let landed_entity: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM brain_entities WHERE name = $1")
            .bind(&entity_name)
            .fetch_optional(&b)
            .await
            .expect("check the specific entity");
    assert!(
        landed_entity.is_none(),
        "the specific entity from this bundle must not exist on B"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&a)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn a_withheld_tombstone_is_retried_on_the_next_round_not_dropped() {
    let a = pool_a().await;
    let b = pool_b().await;
    let bundle = scratch_dir("tomb-retry");
    std::fs::create_dir_all(&bundle).expect("scratch bundle dir");
    let _owns = own_the_sync_dir(&a).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity_name = unique_name("tomb_retry_entity");
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&entity_name)
    .fetch_one(&a)
    .await
    .expect("seed the entity on A");
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&entity_name)
        .execute(&b)
        .await
        .expect("seed the SAME entity id on B (already synced before, the normal case)");
    for i in 0..3 {
        sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
            .bind(entity_id)
            .bind(format!("B-only child {i}, A never saw this"))
            .execute(&b)
            .await
            .expect("seed a B-only child");
    }

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&a)
        .await
        .expect("delete on A");
    call(
        &a,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let first = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    let withheld_first = first["tombstones_withheld"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(
        withheld_first, 1,
        "round 1 must withhold: B has children A never saw. Got {first}"
    );

    let alive_after_first: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE id = $1")
            .bind(entity_id)
            .fetch_one(&b)
            .await
            .expect("count after round 1");
    assert_eq!(
        alive_after_first, 1,
        "the entity must still be alive on B after round 1"
    );

    let second = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    assert_ne!(
        second["skipped"].as_bool(),
        Some(true),
        "a bundle with a still-withheld tombstone must never be treated as already imported. \
         Got {second}"
    );
    let withheld_second = second["tombstones_withheld"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(
        withheld_second, 1,
        "round 2, same bundle, nothing changed on B: the tombstone must be retried and withheld \
         again, not silently forgotten. Got {second}"
    );

    sqlx::query("DELETE FROM brain_observations WHERE entity_id = $1")
        .bind(entity_id)
        .execute(&b)
        .await
        .expect("remove B's children");
    let third = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    eprintln!("round 3 (children gone): {third}");
    let alive_after_third: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_entities WHERE id = $1")
            .bind(entity_id)
            .fetch_one(&b)
            .await
            .expect("count after round 3");
    assert_eq!(
        alive_after_third, 0,
        "once B's local children are gone, the SAME tombstone must finally take effect instead \
         of being permanently stuck because a lucky earlier round marked the manifest_hash done"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&a)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&b)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn a_manually_promoted_row_stays_promoted_after_a_round_trip() {
    let a = pool_a().await;
    let b = pool_b().await;
    let bundle = scratch_dir("promote-roundtrip");
    std::fs::create_dir_all(&bundle).expect("scratch bundle dir");
    let _owns = own_the_sync_dir(&a).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let entity_name = unique_name("promote_entity");
    let entity_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&entity_name)
    .fetch_one(&a)
    .await
    .expect("seed the entity");

    let obs_id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2) RETURNING id",
    )
    .bind(entity_id)
    .bind("el token de deploy es ghp_abcdefghijklmnopqrstuvwxyz0123456789")
    .fetch_one(&a)
    .await
    .expect("seed the observation that will quarantine on import");

    call(
        &a,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    let first = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    let trust_after_import: String =
        sqlx::query_scalar("SELECT trust FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_one(&b)
            .await
            .expect("read trust after first import");
    assert_eq!(
        trust_after_import, "quarantined",
        "the precondition for this test: the row must land quarantined. Got import {first}"
    );

    call(
        &b,
        "cuba_eco",
        json!({"action": "promote", "observation_id": obs_id.to_string()}),
    )
    .await;
    let trust_after_promote: String =
        sqlx::query_scalar("SELECT trust FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_one(&b)
            .await
            .expect("read trust after promotion");
    assert_eq!(trust_after_promote, "trusted");

    call(
        &b,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    call(
        &a,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    let trust_on_a: String =
        sqlx::query_scalar("SELECT trust FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_one(&a)
            .await
            .expect("read trust on A after B->A");
    eprintln!("trust on A after receiving the promoted row from B: {trust_on_a}");

    call(
        &a,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    let last = call(
        &b,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "confirm": true}),
    )
    .await;
    eprintln!("A->B again: {last}");
    let trust_final: String =
        sqlx::query_scalar("SELECT trust FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_one(&b)
            .await
            .expect("read final trust on B");
    assert_eq!(
        trust_final, "trusted",
        "a row promoted by hand on B must still be trusted after A's bundle (which still \
         contains the same credential-looking content) round-trips through it again — \
         re-quarantining on every import is the exact regression the project's own memory \
         (project_sync_bundle_untrusted.md) says was fixed"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&a)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&b)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
