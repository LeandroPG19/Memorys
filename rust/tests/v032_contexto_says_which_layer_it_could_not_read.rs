// needs-a-server: migrates a scratch database it creates and drops, breaks one table or column in it, and asks for the context
//
//! `cuba_contexto` says which layer it could not read.
//!
//! The tool exists to be called «at the start of a turn» and hand the agent what it already
//! holds: working memory, notes left for it, the artifact index and a recall of what matters.
//! Each layer is its own query, and each one was `unwrap_or_default()`: a layer that could not
//! be read came back as an empty list, which is also what a layer with nothing in it looks like.
//! An agent that gets `working_memory: []` after a failure re-plans from zero, and one that gets
//! `artifacts: []` writes a file somebody else already versioned (found by the 0.32 audit,
//! 2026-10-21, with the same count of swallowed reads that `cuba_faro` had).
//!
//! Each layer is broken here the way an install breaks it, in a scratch database, and the call
//! must still answer with the layers that do work, plus `layers_degraded`, `layers_lost` naming
//! the layers in a form a program can match, and a reason. A healthy call carries none of them.
//!
//! `DATABASE_URL` names the server. The database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the one the URL points at is never touched.

mod common;

use common::in_a_scratch_database;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

/// The session and the notes are keyed by client identity (`session::with_client`), so every test
/// is its own client and the five run side by side instead of one after the other.
fn a_client() -> String {
    format!("contexto-test-{}", Uuid::new_v4())
}

async fn context(pool: &PgPool, client: &str) -> Value {
    let envelope = memory_industry::session::with_client(
        client.to_string(),
        memory_industry::handlers::dispatch(pool, "cuba_contexto", json!({})),
    )
    .await
    .unwrap_or_else(|e| panic!("cuba_contexto failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

/// Something in every layer, so that a layer which comes back empty is one that was not read.
async fn a_corpus_with_every_layer(pool: &PgPool, client: &str) {
    let entity = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, 'capa_viva', 'concept')",
    )
    .bind(entity)
    .execute(pool)
    .await
    .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, importance)
         VALUES ($1, 'algo importante que siempre se recuerda', 0.9)",
    )
    .bind(entity)
    .execute(pool)
    .await
    .expect("seed the observation");
    sqlx::query(
        "INSERT INTO brain_artifacts (path, version, content_hash, content)
         VALUES ('notas/plan.md', 1, 'h1', 'el plan')",
    )
    .execute(pool)
    .await
    .expect("seed the artifact");
    sqlx::query(
        "INSERT INTO brain_triggers (condition_type, entity_pattern, message, active)
         VALUES ('on_session_start', $1, 'recordatorio para este cliente', TRUE)",
    )
    .bind(client)
    .execute(pool)
    .await
    .expect("seed the note for this client");
    memory_industry::session::with_client(
        client.to_string(),
        memory_industry::handlers::dispatch(
            pool,
            "cuba_pizarra",
            json!({"action": "write", "content": "una nota de la pizarra", "ttl_seconds": 600}),
        ),
    )
    .await
    .expect("seed working memory");
}

async fn break_it(url: &str, ddl: &str) {
    let mut owner = PgConnection::connect(url)
        .await
        .expect("connecting to the scratch database as its owner");
    sqlx::query(ddl)
        .execute(&mut owner)
        .await
        .unwrap_or_else(|e| panic!("{ddl}: {e}"));
}

fn must_say_it(response: &Value, layer: &str) {
    assert_eq!(
        response.get("layers_degraded").and_then(Value::as_bool),
        Some(true),
        "the {layer} layer could not be read and the response says nothing: an empty list there \
         is what a layer with nothing in it looks like. Got: {response}"
    );
    let named: Vec<&str> = response
        .get("layers_lost")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        named.contains(&layer),
        "layers_lost must name the layer ({layer}) in a form a program can match. Got: {named:?}"
    );
    assert!(
        response
            .get("layers_degraded_reason")
            .and_then(Value::as_str)
            .is_some_and(|r| !r.trim().is_empty()),
        "and layers_degraded_reason must say in words what the reader should not conclude. \
         Got: {response}"
    );
}

fn must_still_answer(response: &Value, layer_that_works: &str) {
    assert!(
        response["layers"][layer_that_works]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "the layers that can be read must keep coming back, or the failure took the whole call \
         down and this test measures nothing about the broken one. {layer_that_works} is empty \
         in: {response}"
    );
}

#[tokio::test]
async fn a_healthy_context_carries_every_layer_and_no_degradation_keys() {
    in_a_scratch_database("brain_ctx_ok", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let client = a_client();
        a_corpus_with_every_layer(&pool, &client).await;

        let healthy = context(&pool, &client).await;
        for layer in ["working_memory", "agent_notes", "artifacts", "top_recall"] {
            must_still_answer(&healthy, layer);
        }
        assert!(
            healthy.get("layers_degraded").is_none()
                && healthy.get("layers_lost").is_none()
                && healthy.get("layers_degraded_reason").is_none(),
            "a healthy call carries no degradation keys: it is made at the start of every turn \
             and has to cost nothing when nothing is wrong. Got: {healthy}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_context_that_could_not_read_the_working_memory_says_so() {
    in_a_scratch_database("brain_ctx_wm", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let client = a_client();
        a_corpus_with_every_layer(&pool, &client).await;
        break_it(&url, "DROP TABLE brain_wm").await;

        let broken = context(&pool, &client).await;
        must_still_answer(&broken, "artifacts");
        must_say_it(&broken, "working_memory");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_context_that_could_not_read_the_notes_for_this_client_says_so() {
    in_a_scratch_database("brain_ctx_notes", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let client = a_client();
        a_corpus_with_every_layer(&pool, &client).await;
        break_it(&url, "DROP TABLE brain_triggers").await;

        let broken = context(&pool, &client).await;
        must_still_answer(&broken, "artifacts");
        must_say_it(&broken, "agent_notes");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_context_that_could_not_read_the_artifact_index_says_so() {
    in_a_scratch_database("brain_ctx_artifacts", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let client = a_client();
        a_corpus_with_every_layer(&pool, &client).await;
        break_it(&url, "DROP TABLE brain_artifacts").await;

        let broken = context(&pool, &client).await;
        must_still_answer(&broken, "working_memory");
        must_say_it(&broken, "artifacts");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_context_that_could_not_read_the_recall_says_so() {
    in_a_scratch_database("brain_ctx_recall", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let client = a_client();
        a_corpus_with_every_layer(&pool, &client).await;
        break_it(
            &url,
            "ALTER TABLE brain_observations RENAME COLUMN updated_at TO updated_at_gone",
        )
        .await;

        let broken = context(&pool, &client).await;
        must_still_answer(&broken, "artifacts");
        must_say_it(&broken, "top_recall");
        pool.close().await;
    })
    .await;
}
