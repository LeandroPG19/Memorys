mod common;

use common::in_a_scratch_database;
use sqlx::{Executor, PgPool};

#[tokio::test]
async fn the_daemon_refuses_a_vector_column_of_another_width_and_names_both() {
    assert!(
        memory_industry::embeddings::onnx::is_model_loaded(),
        "this test needs the ONNX embedder loaded (ONNX_MODEL_PATH and ORT_DYLIB_PATH, installed \
         by `memory-industry models all`). assert_embedding_dim returns Ok without reading a \
         single column when no model is loaded, so without one the guard and its replacement by \
         Ok(()) are indistinguishable here"
    );
    let runtime = memory_industry::embeddings::onnx::embedding_dim();
    let other = runtime * 2;

    in_a_scratch_database("brain_dimguard", move |url| async move {
        let pool = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database");
        pool.execute("CREATE EXTENSION IF NOT EXISTS vector")
            .await
            .expect("pgvector in the scratch database");
        pool.execute(format!("CREATE TABLE fits (embedding vector({runtime}))").as_str())
            .await
            .expect("a column as wide as the model");
        let matching = memory_industry::db::assert_embedding_dim(&pool).await;

        pool.execute(format!("CREATE TABLE left_behind (embedding vector({other}))").as_str())
            .await
            .expect("a column from another model");
        let mismatched = memory_industry::db::assert_embedding_dim(&pool).await;
        pool.close().await;

        assert!(
            matching.is_ok(),
            "every vector column is vector({runtime}), the width the loaded model produces, and \
             the guard still refused to start: {matching:?}"
        );
        let err = format!(
            "{:#}",
            mismatched.expect_err(
                "left_behind.embedding is vector({other}) and the model writes vector({runtime}): \
                 the daemon must not start. Letting it through is how a 768-d model writes \
                 against a 384-d base and search quietly degrades to lexical"
            )
        );
        assert!(
            err.contains(&format!("vector({runtime})")),
            "the refusal has to say what width the model produces, or the operator cannot tell \
             which side to change: {err}"
        );
        assert!(
            err.contains(&format!("vector({other})")),
            "the refusal has to say what width the column has: {err}"
        );
        assert!(
            err.contains("left_behind.embedding"),
            "the refusal has to name the column that does not fit: {err}"
        );
    })
    .await;
}
