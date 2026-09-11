use uuid::Uuid;

const REM_RELATION_BATCH_LOCK: i64 = 0x0CBA_A0D1_7106_0031;
const REM_EXTRACTION_BATCH_LOCK: i64 = 0x0CBA_A0D1_7106_0032;

async fn own_the_rem_relation_batch_env(
    pool: &sqlx::PgPool,
) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REM_RELATION_BATCH_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_REM_RELATION_BATCH is process-global");
    tx
}

async fn own_the_rem_extraction_batch_env(
    pool: &sqlx::PgPool,
) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REM_EXTRACTION_BATCH_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_REM_EXTRACTION_BATCH is process-global");
    tx
}

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

fn unique_name(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

async fn trusted_observation(
    pool: &sqlx::PgPool,
    entity_name: &str,
    content: &str,
) -> (Uuid, Uuid) {
    let entity: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'project') RETURNING id",
    )
    .bind(entity_name)
    .fetch_one(pool)
    .await
    .expect("creating the fixture entity");

    let observation: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source, trust)
         VALUES ($1, $2, 'fact', 'agent', 'trusted') RETURNING id",
    )
    .bind(entity.0)
    .bind(content)
    .fetch_one(pool)
    .await
    .expect("creating the fixture observation");

    (entity.0, observation.0)
}

const SESSION_LOCK: i64 = 0x0CBA_A0D1_7106_0031;

async fn own_the_session(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SESSION_LOCK)
        .execute(&mut *tx)
        .await
        .expect(
            "the active session is process-global, so two tests that open one at the same time              read each other's. GLOBAL_STATE_GUARD lives behind #[cfg(test)] and does not exist              for an integration test compiled against the library, which is why this uses the              advisory lock the other integration tests already take",
        );
    tx
}

#[tokio::test]
#[ignore]
async fn a_fresh_observation_queues_for_extraction_until_marked() {
    let pool = pool().await;
    let name = unique_name("RemExtract");
    let (entity_id, obs_id) =
        trusted_observation(&pool, &name, "nota fresca para la extracción automática").await;

    let pending =
        memory_industry::handlers::ingesta::observations_awaiting_extraction(&pool, 5_000)
            .await
            .expect("listing candidates");
    assert!(
        pending.iter().any(|(id, _)| *id == obs_id),
        "a freshly written trusted observation with extracted_at NULL must queue for extraction"
    );

    sqlx::query("UPDATE brain_observations SET extracted_at = NOW() WHERE id = $1")
        .bind(obs_id)
        .execute(&pool)
        .await
        .expect("marking as extracted");

    let requeued =
        memory_industry::handlers::ingesta::observations_awaiting_extraction(&pool, 5_000)
            .await
            .expect("listing candidates again");
    assert!(
        !requeued.iter().any(|(id, _)| *id == obs_id),
        "once extracted_at is stamped, the observation must not queue again"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn autolink_apply_tags_its_edges_as_predicted_not_extracted() {
    let pool = pool().await;
    let a_name = unique_name("AutolinkA");
    let b_name = unique_name("AutolinkB");
    let a: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&a_name)
    .fetch_one(&pool)
    .await
    .expect("creating entity a");
    let b: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&b_name)
    .fetch_one(&pool)
    .await
    .expect("creating entity b");

    let candidate = memory_industry::graph::autolink::Candidate {
        from_id: a.0,
        to_id: b.0,
        from_name: a_name.clone(),
        to_name: b_name.clone(),
        co_sessions: 3,
        npmi: 0.5,
    };

    let inserted = memory_industry::graph::autolink::apply(&pool, &[candidate])
        .await
        .expect("applying the candidate");
    assert_eq!(inserted, 1);

    let provenance: (String,) = sqlx::query_as(
        "SELECT provenance FROM brain_relations WHERE from_entity = $1 AND to_entity = $2",
    )
    .bind(a.0)
    .bind(b.0)
    .fetch_one(&pool)
    .await
    .expect("reading the new edge");

    sqlx::query("DELETE FROM brain_entities WHERE id IN ($1, $2)")
        .bind(a.0)
        .bind(b.0)
        .execute(&pool)
        .await
        .ok();

    assert_eq!(
        provenance.0, "predicted",
        "an NPMI co-occurrence edge is a persisted statistical suggestion, not something a \
         person typed through cuba_puente create (extracted) or an LLM read out of free text \
         (inferred); leaving the column at its 'extracted' default made an autolink edge \
         indistinguishable from one a human typed by hand"
    );
}

#[tokio::test]
#[ignore]
async fn duplicate_candidates_grow_when_near_duplicate_names_exist() {
    let pool = pool().await;
    let baseline = memory_industry::protocol::rem_count_duplicate_candidates(&pool).await;

    let stem = &Uuid::new_v4().to_string()[..8];
    let a_name = format!("Dedupe Target Alpha {stem}");
    let b_name = format!("Dedupe Target Alpha {stem}!");
    let a: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&a_name)
    .fetch_one(&pool)
    .await
    .expect("creating entity a");
    let b: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&b_name)
    .fetch_one(&pool)
    .await
    .expect("creating entity b");

    let after = memory_industry::protocol::rem_count_duplicate_candidates(&pool).await;

    sqlx::query("DELETE FROM brain_entities WHERE id IN ($1, $2)")
        .bind(a.0)
        .bind(b.0)
        .execute(&pool)
        .await
        .ok();

    assert!(
        after > baseline,
        "two near-identical entity names differing by one trailing character must add at \
         least one candidate pair to the count the REM cycle logs every pass: baseline={baseline}, \
         after={after}"
    );
}

#[tokio::test]
#[ignore]
async fn an_explicit_extraction_batch_override_wins_and_zero_disables() {
    let pool = pool().await;
    let _owns = own_the_rem_extraction_batch_env(&pool).await;

    unsafe { std::env::set_var("CUBA_REM_EXTRACTION_BATCH", "9") };
    let batch = memory_industry::protocol::rem_extraction_batch();
    assert_eq!(
        batch, 9,
        "an explicit CUBA_REM_EXTRACTION_BATCH must override the default of 5, the operator's \
         escape hatch the relation scan's own batch knob already has"
    );

    unsafe { std::env::set_var("CUBA_REM_EXTRACTION_BATCH", "0") };
    let batch = memory_industry::protocol::rem_extraction_batch();
    assert_eq!(
        batch, 0,
        "CUBA_REM_EXTRACTION_BATCH=0 must be able to turn auto-extraction off entirely"
    );

    unsafe { std::env::remove_var("CUBA_REM_EXTRACTION_BATCH") };
    let batch = memory_industry::protocol::rem_extraction_batch();
    unsafe { std::env::remove_var("CUBA_REM_EXTRACTION_BATCH") };
    assert_eq!(
        batch, 5,
        "with nothing set the default must be 5, matching the relation scan's own default"
    );
}

#[tokio::test]
#[ignore]
async fn community_detection_assigns_every_entity_a_community_after_one_rem_cycle() {
    let pool = pool().await;
    let _owns_relation_batch = own_the_rem_relation_batch_env(&pool).await;
    let _owns_extraction_batch = own_the_rem_extraction_batch_env(&pool).await;
    unsafe { std::env::set_var("CUBA_REM_RELATION_BATCH", "0") };
    unsafe { std::env::set_var("CUBA_REM_EXTRACTION_BATCH", "0") };

    let name = unique_name("CommunityFixture");
    let entity: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&name)
    .fetch_one(&pool)
    .await
    .expect("creating the fixture entity");

    let before: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT community_id FROM brain_node_metrics WHERE node_id = $1")
            .bind(entity.0)
            .fetch_optional(&pool)
            .await
            .expect("reading community before");
    assert!(
        before.is_none(),
        "a brand-new entity must start with no metrics row at all"
    );

    let cycled = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        memory_industry::protocol::run_rem_consolidation(&pool),
    )
    .await;

    unsafe { std::env::remove_var("CUBA_REM_RELATION_BATCH") };
    unsafe { std::env::remove_var("CUBA_REM_EXTRACTION_BATCH") };

    cycled
        .expect("one REM cycle must finish within 120s")
        .expect("REM consolidation must not error");

    let after: (Option<Uuid>,) =
        sqlx::query_as("SELECT community_id FROM brain_node_metrics WHERE node_id = $1")
            .bind(entity.0)
            .fetch_one(&pool)
            .await
            .expect("reading community after");

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity.0)
        .execute(&pool)
        .await
        .ok();

    assert!(
        after.0.is_some(),
        "one REM cycle must assign every entity — including a freshly created, unconnected one \
         — to a community. Leiden/Louvain detection used to run only from the manual \
         `cuba_zafra` CLI, so nothing kept community_id in sync with new entities and 183 \
         communities sat unchanged over 325 entities"
    );
}

#[tokio::test]
#[ignore]
async fn a_session_start_reports_how_many_observations_are_waiting_for_review() {
    let pool = pool().await;
    let _one_at_a_time = own_the_session(&pool).await;

    let name = unique_name("QuarantineFixture");
    let entity: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&name)
    .fetch_one(&pool)
    .await
    .expect("creating the fixture entity");

    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source, trust)
         VALUES ($1, 'hecho extraído automáticamente, sin revisar todavía', 'fact', 'inference',
                 'quarantined')",
    )
    .bind(entity.0)
    .execute(&pool)
    .await
    .expect("creating the quarantined fixture");

    let expected: (i64,) =
        sqlx::query_as("SELECT count(*) FROM brain_observations WHERE trust = 'quarantined'")
            .fetch_one(&pool)
            .await
            .expect("measuring the ground truth count");

    let response = memory_industry::handlers::jornada::handle(
        &pool,
        serde_json::json!({"action": "start", "name": &name}),
    )
    .await
    .expect("starting a session");

    memory_industry::handlers::jornada::handle(&pool, serde_json::json!({"action": "end"}))
        .await
        .ok();

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity.0)
        .execute(&pool)
        .await
        .ok();

    assert_eq!(
        response["observations_pending_review"],
        serde_json::json!(expected.0),
        "cuba_jornada start must report exactly how many observations are quarantined and \
         waiting for a human, or the extraction pipeline writes into a hole nobody ever checks \
         at the one moment every session already reads. Response was: {response}"
    );
}

#[tokio::test]
#[ignore]
async fn an_observation_that_can_never_be_extracted_leaves_the_queue() {
    let pool = pool().await;

    let name = unique_name("PoisonPill");
    let entity: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&name)
    .fetch_one(&pool)
    .await
    .expect("creating the fixture entity");

    let observation: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_observations (entity_id, content, extracted_at)
         VALUES ($1, $2, NULL) RETURNING id",
    )
    .bind(entity.0)
    .bind("el token es ghp_0123456789abcdefghijklmnopqrstuvwxyzAB y no se puede procesar")
    .fetch_one(&pool)
    .await
    .expect("seeding an observation the write gate refuses to hand to a model");

    let outcome = memory_industry::handlers::ingesta::rem_extract_observation(
        &pool,
        observation.0,
        "el token es ghp_0123456789abcdefghijklmnopqrstuvwxyzAB y no se puede procesar",
    )
    .await;

    let marked: (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT extracted_at FROM brain_observations WHERE id = $1")
            .bind(observation.0)
            .fetch_one(&pool)
            .await
            .expect("reading the progress mark back");

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity.0)
        .execute(&pool)
        .await
        .ok();

    assert!(
        outcome.is_ok(),
        "content the write gate refuses is not a failure of the cycle, and counting it as one \
         spends one of the two consecutive failures a batch is allowed. Two such observations \
         would stop the only periodic task in the system for good"
    );
    assert!(
        marked.0.is_some(),
        "an observation that can never be extracted has to leave the queue. It carries a \
         credential, so refuse_secrets will reject it on every future cycle exactly as it did \
         on this one — retrying it forever is how one poisoned row starves every observation \
         written after it"
    );
}
