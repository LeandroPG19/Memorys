use serde_json::json;
use uuid::Uuid;

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0026;

async fn own_the_sync_dir(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_SYNC_DIR and CUBA_EMBED_MODEL are process-global");
    tx
}

#[tokio::test]
#[ignore]
async fn vectors_from_another_model_are_refused_instead_of_quietly_ruining_search() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-emb-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(bundle.join("entities")).expect("entities dir");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let dim = memory_industry::embeddings::onnx::embedding_dim();
    std::fs::write(
        bundle.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "manifest_hash": "irrelevant, the import recomputes it",
            "project_id": null, "project_name": null,
            "exported_at": "2026-08-01T00:00:00Z",
            "counts": {"entities": 0, "observations": 0, "episodes": 0, "decisions": 0, "errors": 0, "relations": 0},
            "with_embeddings": true,
            "embedding_dim": dim,
            "embedding_model": "a-model-this-machine-does-not-run"
        }))
        .expect("serialise"),
    )
    .expect("manifest");
    std::fs::write(
        bundle.join("embeddings.bin.zst"),
        memory_industry::sync::compressor::compress(&vec![0u8; 16 + dim * 4]).expect("compress"),
    )
    .expect("blob");

    let refused = memory_industry::handlers::dispatch(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string()}),
    )
    .await;

    let Err(failure) = refused else {
        panic!(
            "the import accepted vectors produced by a different model. Nothing downstream \
             notices: the dimensions match, pgvector stores them, the HNSW index accepts them, \
             and every search from then on compares this machine's queries against another \
             model's space. The results just quietly get worse, and the only thing that ever \
             mentions it is `doctor` counting stale rows after the fact"
        );
    };
    let chain = format!("{failure:#}");
    assert!(
        chain.contains("not the same space"),
        "the refusal has to explain why same-dimension is not same-space, or it reads as \
         pedantry and someone works around it. Got: {chain}"
    );

    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
