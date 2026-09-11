use uuid::Uuid;

fn unique_name(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

#[tokio::test]
#[ignore]
async fn a_long_observation_becomes_reachable_past_the_truncation_limit() {
    assert!(
        memory_industry::embeddings::onnx::is_model_loaded(),
        "no ONNX model loaded. Chunking exists so a long observation stays reachable past the \
         truncation limit, and that is a claim about SIMILARITY — under the hash fallback it \
         means nothing, so reporting ok would be reporting on a measurement never taken"
    );

    let pool = pool().await;
    let entity = unique_name("chunk_entity");
    let tail_marker = unique_name("tailconcept");

    let head = "The build pipeline compiles the Rust workspace, caches cargo registry \
                artifacts between runs, links the ONNX runtime, and uploads the resulting \
                binaries. Compiler flags, linker paths, and container base images are \
                configured per target architecture. "
        .repeat(9);
    let tail = format!(
        "Beekeeping notes on {tail_marker}: the colony overwinters best when the hive is \
         insulated and the entrance reduced. Foragers return with pollen once the maples \
         bloom, and the queen resumes laying as daylight lengthens. Inspect brood frames \
         for chalkbrood, and never harvest honey below the winter reserve. Swarm control \
         with {tail_marker} means giving the colony room before it decides to leave. "
    )
    .repeat(3);
    let content = format!("{head}\n\n{tail}");

    let threshold = memory_industry::embeddings::chunk::threshold_chars();
    assert!(
        content.chars().count() > threshold,
        "the fixture must exceed the chunking threshold"
    );
    assert!(
        head.chars().count() > threshold,
        "the head alone must already fill the truncation window, so the tail is what the \
         whole-document embedding cannot see"
    );

    let entity_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&entity)
    .fetch_one(&pool)
    .await
    .expect("creating entity");

    let obs_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source)
         VALUES ($1, $2, 'fact', 'agent') RETURNING id",
    )
    .bind(entity_id.0)
    .bind(&content)
    .fetch_one(&pool)
    .await
    .expect("creating observation");

    let full =
        memory_industry::embeddings::onnx::embed_passage_contextual(&content, "concept", &entity)
            .await
            .expect("embedding the full text");
    sqlx::query("UPDATE brain_observations SET embedding = $1::vector WHERE id = $2")
        .bind(pgvector::Vector::from(full))
        .bind(obs_id.0)
        .execute(&pool)
        .await
        .expect("storing the full-text embedding");

    let stored = memory_industry::embeddings::backfill::store_chunks(
        &pool, obs_id.0, &content, "concept", &entity, None,
    )
    .await
    .expect("chunking");
    assert!(stored > 1, "a text this long must produce several chunks");

    let query_vec = memory_industry::embeddings::onnx::embed(&format!(
        "how does the bee colony overwinter, and what is {tail_marker} swarm control"
    ))
    .await
    .expect("embedding the query");
    let qv = pgvector::Vector::from(query_vec);

    let via_full: (f64,) = sqlx::query_as(
        "SELECT 1.0 - (embedding <=> $1::vector) FROM brain_observations WHERE id = $2",
    )
    .bind(&qv)
    .bind(obs_id.0)
    .fetch_one(&pool)
    .await
    .expect("similarity via the truncated full-text embedding");

    let via_chunk: (f64,) = sqlx::query_as(
        "SELECT max(1.0 - (embedding <=> $1::vector)) FROM brain_observation_chunks
         WHERE observation_id = $2",
    )
    .bind(&qv)
    .bind(obs_id.0)
    .fetch_one(&pool)
    .await
    .expect("similarity via the best chunk");

    assert!(
        via_chunk.0 > via_full.0,
        "the chunk covering the tail must match a tail query better than the truncated \
         whole-document embedding does (chunk {:.4} vs full {:.4})",
        via_chunk.0,
        via_full.0
    );

    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&entity)
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn chunking_is_idempotent_and_never_duplicates() {
    let pool = pool().await;
    let entity = unique_name("chunk_idem");
    let content = "Repeated sentence for idempotency. ".repeat(120);

    let entity_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(&entity)
    .fetch_one(&pool)
    .await
    .expect("creating entity");

    let obs_id: (Uuid,) = sqlx::query_as(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source)
         VALUES ($1, $2, 'fact', 'agent') RETURNING id",
    )
    .bind(entity_id.0)
    .bind(&content)
    .fetch_one(&pool)
    .await
    .expect("creating observation");

    for _ in 0..2 {
        memory_industry::embeddings::backfill::store_chunks(
            &pool, obs_id.0, &content, "concept", &entity, None,
        )
        .await
        .expect("chunking");
    }

    let indexes: Vec<(i32,)> = sqlx::query_as(
        "SELECT chunk_index FROM brain_observation_chunks WHERE observation_id = $1 ORDER BY 1",
    )
    .bind(obs_id.0)
    .fetch_all(&pool)
    .await
    .expect("reading chunk indexes");

    let unique: std::collections::HashSet<i32> = indexes.iter().map(|(i,)| *i).collect();
    assert_eq!(
        unique.len(),
        indexes.len(),
        "re-chunking must not duplicate rows"
    );

    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&entity)
        .execute(&pool)
        .await
        .ok();
}
