// needs-a-server: migrates a scratch database it creates and drops, breaks one table or column in it, and searches
//
//! A search that could not read one of its secondary signals says so.
//!
//! `cuba_faro` already tells the caller when the vector branch, BM25 or the reranker did not
//! run (`degraded`, `bm25_degraded`, `reranker_degraded`), because a branch that fails and
//! returns nothing is indistinguishable from a corpus with no answer. Three reads around the
//! ranking were left outside that rule (found by the 0.32 audit, 2026-10-21, counting
//! `unwrap_or_default()` on database reads: 60 in `src/`):
//!
//! * the session's goals and working memory, which boost what the session is about
//!   (`session_context`: the whole read swallowed without a log line, and the working-memory
//!   half swallowed apart);
//! * the evidence annotation, which marks a result as verified or contradicted instead of
//!   merely asserted (`annotate_evidence`): lost, the result reads as «asserted» and a
//!   verified fact looks like hearsay;
//! * `new_since_you_started`, which marks what another agent wrote after the session opened
//!   (`written_since`): lost, the answer is «nothing new», which is a claim.
//!
//! Each of these is broken here the way a real install breaks it (a table that is not there, a
//! column a migration renamed) in a scratch database, with the search itself still working: the
//! results must come back, and the response must carry `signals_degraded` with a reason that
//! names what was lost. A healthy search carries neither key, so the common case costs nothing.
//!
//! `DATABASE_URL` names the server. The database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the one the URL points at is never touched.

mod common;

use common::in_a_scratch_database;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

/// The session is process-wide (`session::set`), so two of these running side by side would
/// open each other's sessions in the wrong database.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn call(pool: &PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

/// One entity with one observation written two hours ago, and a session opened now. Returns the
/// marker the search looks for.
async fn a_corpus_and_an_open_session(pool: &PgPool) -> String {
    let marker = format!("senal_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(pool)
        .await
        .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, created_at)
         VALUES ($1, $2, NOW() - interval '2 hours')",
    )
    .bind(entity_id)
    .bind(format!("{marker} lo que ya estaba cuando llegaste"))
    .execute(pool)
    .await
    .expect("seed the observation");

    memory_industry::session::clear();
    call(
        pool,
        "cuba_jornada",
        json!({"action": "start", "name": format!("sesion de {marker}")}),
    )
    .await;
    marker
}

/// The pool runs as `cuba_app`, which owns nothing, so the damage is done with the server's own
/// account on the same scratch database.
async fn break_it(url: &str, ddl: &str) {
    let mut owner = PgConnection::connect(url)
        .await
        .expect("connecting to the scratch database as its owner");
    sqlx::query(ddl)
        .execute(&mut owner)
        .await
        .unwrap_or_else(|e| panic!("{ddl}: {e}"));
}

async fn search(pool: &PgPool, marker: &str) -> Value {
    call(pool, "cuba_faro", json!({"query": marker, "limit": 10})).await
}

fn must_still_find(marker: &str, response: &Value) {
    assert!(
        response["count"].as_u64().unwrap_or(0) >= 1,
        "the search itself has to keep working with the broken signal, or this test measures \
         nothing about that signal. Searched {marker}, got: {response}"
    );
}

fn must_say_it(response: &Value, lost: &str) {
    assert_eq!(
        response.get("signals_degraded").and_then(Value::as_bool),
        Some(true),
        "a read around the ranking failed ({lost}) and the response says nothing. Left as it is, \
         the caller reads the missing signal as an answer: no session boost, no evidence \
         marks, nothing new. Got: {response}"
    );
    let named: Vec<&str> = response
        .get("signals_lost")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(
        named.contains(&lost),
        "signals_lost must name what was lost ({lost}), in a form a program can match, not \
         just say that something was. Got: {named:?}"
    );
    assert!(
        response
            .get("signals_degraded_reason")
            .and_then(Value::as_str)
            .is_some_and(|r| !r.trim().is_empty()),
        "and signals_degraded_reason must say in words what that costs the reader. Got: {response}"
    );
}

#[tokio::test]
async fn a_search_that_could_not_read_the_working_memory_says_so() {
    let _one = ONE_AT_A_TIME.lock().await;
    in_a_scratch_database("brain_sig_wm", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let marker = a_corpus_and_an_open_session(&pool).await;

        let healthy = search(&pool, &marker).await;
        must_still_find(&marker, &healthy);
        assert!(
            healthy.get("signals_degraded").is_none()
                && healthy.get("signals_lost").is_none()
                && healthy.get("signals_degraded_reason").is_none(),
            "a healthy search carries no degradation keys: the common case costs nothing. Got: \
             {healthy}"
        );

        break_it(&url, "DROP TABLE brain_wm").await;
        let broken = search(&pool, &marker).await;
        must_still_find(&marker, &broken);
        must_say_it(&broken, "working_memory");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_search_that_could_not_read_the_session_says_so() {
    let _one = ONE_AT_A_TIME.lock().await;
    in_a_scratch_database("brain_sig_session", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let marker = a_corpus_and_an_open_session(&pool).await;

        break_it(
            &url,
            "ALTER TABLE brain_sessions RENAME COLUMN goals TO goals_gone",
        )
        .await;
        let broken = search(&pool, &marker).await;
        must_still_find(&marker, &broken);
        must_say_it(&broken, "session_goals");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_search_that_could_not_mark_the_evidence_says_so() {
    let _one = ONE_AT_A_TIME.lock().await;
    in_a_scratch_database("brain_sig_evidence", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let marker = a_corpus_and_an_open_session(&pool).await;

        break_it(
            &url,
            "ALTER TABLE brain_observations RENAME COLUMN verified_at TO verified_at_gone",
        )
        .await;
        let broken = search(&pool, &marker).await;
        must_still_find(&marker, &broken);
        must_say_it(&broken, "evidence_marks");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_search_that_could_not_tell_what_is_new_says_so() {
    let _one = ONE_AT_A_TIME.lock().await;
    in_a_scratch_database("brain_sig_new", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let marker = a_corpus_and_an_open_session(&pool).await;

        break_it(
            &url,
            "ALTER TABLE brain_episodes RENAME COLUMN started_at TO started_at_gone",
        )
        .await;
        let broken = search(&pool, &marker).await;
        must_still_find(&marker, &broken);
        must_say_it(&broken, "new_since_session_start");
        pool.close().await;
    })
    .await;
}
