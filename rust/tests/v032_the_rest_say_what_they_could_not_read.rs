// needs-a-server: migrates a scratch database per test, breaks one table or column in it, and calls the tool
//
//! `cuba_pre_compact`, `cuba_jornada` and `cuba_calibrar` say what they could not read.
//!
//! `cuba_faro` and `cuba_contexto` already do (`v032_a_search_says_what_it_could_not_read`,
//! `v032_contexto_says_which_layer_it_could_not_read`). The same audit finding, 60 database reads
//! in `src/` ended in `unwrap_or_default()` / `unwrap_or(0)` / `.ok()`, sits in three more
//! handlers an agent leans on, and in each one the empty answer is a claim:
//!
//! * `cuba_pre_compact snapshot` freezes what survives `/compact`. A list it could not read was
//!   stored empty, the response said `saved: {decisions: 0}`, and the agent compacted believing
//!   its decisions were safe. The snapshot is now stored with a line that says it is incomplete,
//!   so `restore` says so too, after the warning in the response has been compacted away.
//! * `cuba_jornada` decorates `start`, `end` and `current` with the previous session, the
//!   reminders the user asked for with `cuba_centinela` («remind me when a session starts»), the
//!   peer notices, the quarantine count, the session diff and the last snapshot. A reminder that
//!   could not be looked up simply never fired.
//! * `cuba_calibrar` is a set of reads. `metrics`, `trust` and `stats` over a table that cannot
//!   be read answered «no resolved entries», n = 0 — a calibration of nothing, in the shape of
//!   a calibration. They fail now, because the read is the whole action; `resolve`, whose trust
//!   update is a second step, keeps working and says it skipped it.
//!
//! Each failure is made the way an install breaks (a table gone, a column renamed) in a scratch
//! database, and the call must still answer where it can: `signals_degraded`, `signals_lost`
//! naming the signal in a form a program can match, and a reason. A healthy call carries none.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the one the URL points at is never touched.

mod common;

use common::in_a_scratch_database;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use std::future::Future;
use uuid::Uuid;

/// Each test is its own client, so the session (process-wide otherwise) is its own too and the
/// tests run side by side.
async fn as_a_client<F: Future<Output = ()>>(body: F) {
    memory_industry::session::with_client(format!("rest-test-{}", Uuid::new_v4()), body).await;
}

async fn call(pool: &PgPool, tool: &str, args: Value) -> Value {
    try_call(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"))
}

async fn try_call(pool: &PgPool, tool: &str, args: Value) -> anyhow::Result<Value> {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args).await?;
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    Ok(serde_json::from_str(text).expect("json"))
}

async fn migrated(url: &str) -> PgPool {
    memory_industry::db::create_pool(url)
        .await
        .expect("migrating the scratch database")
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

/// A session with one observation and one decision in it. Returns the session id.
async fn a_session_with_a_decision(pool: &PgPool) -> Uuid {
    let started = call(
        pool,
        "cuba_jornada",
        json!({"action": "start", "name": "sesion de prueba"}),
    )
    .await;
    let session: Uuid = started["session"]["id"]
        .as_str()
        .expect("jornada start returns the session")
        .parse()
        .expect("a uuid");
    let entity = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, 'tema', 'concept')",
    )
    .bind(entity)
    .execute(pool)
    .await
    .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, session_id)
         VALUES ($1, 'usamos postgres y no sqlite', 'decision', $2),
                ($1, 'una nota cualquiera', 'fact', $2)",
    )
    .bind(entity)
    .bind(session)
    .execute(pool)
    .await
    .expect("seed the session's observations");
    session
}

fn lost(response: &Value) -> Vec<&str> {
    response
        .get("signals_lost")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn must_say_it(response: &Value, signal: &str) {
    assert_eq!(
        response.get("signals_degraded").and_then(Value::as_bool),
        Some(true),
        "{signal} could not be read and the response says nothing: the empty answer is what a \
         session with nothing in it would give. Got: {response}"
    );
    assert!(
        lost(response).contains(&signal),
        "signals_lost must name {signal} in a form a program can match. Got: {:?}",
        lost(response)
    );
    assert!(
        response
            .get("signals_degraded_reason")
            .and_then(Value::as_str)
            .is_some_and(|r| !r.trim().is_empty()),
        "and signals_degraded_reason must say in words what the missing signal means. Got: \
         {response}"
    );
}

fn must_be_silent(response: &Value) {
    assert!(
        response.get("signals_degraded").is_none()
            && response.get("signals_lost").is_none()
            && response.get("signals_degraded_reason").is_none(),
        "a healthy call carries no degradation keys: these are called every turn. Got: {response}"
    );
}

// ---------------------------------------------------------------- cuba_pre_compact

#[tokio::test]
async fn a_healthy_snapshot_is_complete_and_carries_no_warning() {
    in_a_scratch_database("brain_rest_snap_ok", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            a_session_with_a_decision(&pool).await;

            let snap = call(&pool, "cuba_pre_compact", json!({"action": "snapshot"})).await;
            must_be_silent(&snap);
            assert_eq!(snap["decisions"].as_array().map(Vec::len), Some(1));
            pool.close().await;
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn a_snapshot_that_could_not_read_the_errors_says_so_and_so_does_its_restore() {
    in_a_scratch_database("brain_rest_snap_err", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            a_session_with_a_decision(&pool).await;
            break_it(
                &url,
                "ALTER TABLE brain_errors RENAME COLUMN error_message TO error_message_gone",
            )
            .await;

            let snap = call(&pool, "cuba_pre_compact", json!({"action": "snapshot"})).await;
            must_say_it(&snap, "unresolved_errors");
            assert_eq!(
                snap["decisions"].as_array().map(Vec::len),
                Some(1),
                "what can be read is still saved: {snap}"
            );

            let stored: String = sqlx::query_scalar(
                "SELECT summary_md FROM brain_compaction_snapshots WHERE id = $1::uuid",
            )
            .bind(snap["snapshot_id"].as_str().expect("snapshot id"))
            .fetch_one(&pool)
            .await
            .expect("the snapshot row");
            assert!(
                stored.contains("INCOMPLETO") && stored.contains("unresolved_errors"),
                "the warning of the response is gone after /compact; the stored snapshot has to \
                 carry its own, or the restore reads as a complete one. Stored: {stored:?}"
            );

            let restored = call(&pool, "cuba_pre_compact", json!({"action": "restore"})).await;
            let said = restored["snapshot"]["summary_md"]
                .as_str()
                .unwrap_or_default();
            assert!(
                said.contains("INCOMPLETO"),
                "restore must say the snapshot it hands back is incomplete. Got: {said:?}"
            );
            pool.close().await;
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn a_snapshot_that_could_not_read_the_pending_embeddings_says_so() {
    in_a_scratch_database("brain_rest_snap_pend", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            a_session_with_a_decision(&pool).await;
            break_it(
                &url,
                "ALTER TABLE brain_observations RENAME COLUMN embedding TO embedding_gone",
            )
            .await;

            let snap = call(&pool, "cuba_pre_compact", json!({"action": "snapshot"})).await;
            must_say_it(&snap, "pending_embeddings");
            pool.close().await;
        })
        .await;
    })
    .await;
}

// ---------------------------------------------------------------- cuba_jornada

#[tokio::test]
async fn a_healthy_session_start_carries_no_degradation_keys() {
    in_a_scratch_database("brain_rest_start_ok", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            let started = call(
                &pool,
                "cuba_jornada",
                json!({"action": "start", "name": "s"}),
            )
            .await;
            assert_eq!(started["action"], "started");
            must_be_silent(&started);
            pool.close().await;
        })
        .await;
    })
    .await;
}

async fn a_start_with(prefix: &str, ddl: &'static str, signal: &'static str) {
    in_a_scratch_database(prefix, move |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            break_it(&url, ddl).await;

            let started = call(
                &pool,
                "cuba_jornada",
                json!({"action": "start", "name": "s"}),
            )
            .await;
            assert_eq!(
                started["action"], "started",
                "the session has to start even if a decoration of it cannot be read: {started}"
            );
            must_say_it(&started, signal);
            pool.close().await;
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn a_start_that_could_not_look_up_the_reminders_says_so() {
    a_start_with(
        "brain_rest_start_rem",
        "DROP TABLE brain_triggers",
        "triggered_reminders",
    )
    .await;
}

#[tokio::test]
async fn a_start_that_could_not_read_the_previous_session_says_so() {
    a_start_with(
        "brain_rest_start_prev",
        "ALTER TABLE brain_sessions RENAME COLUMN summary TO summary_gone",
        "previous_session",
    )
    .await;
}

#[tokio::test]
async fn a_start_that_could_not_read_the_peer_notices_says_so() {
    a_start_with(
        "brain_rest_start_peer",
        "DROP TABLE brain_peer_notices",
        "peer_notices",
    )
    .await;
}

#[tokio::test]
async fn a_start_that_could_not_count_the_quarantine_says_so() {
    a_start_with(
        "brain_rest_start_quar",
        "ALTER TABLE brain_observations RENAME COLUMN trust TO trust_gone",
        "observations_pending_review",
    )
    .await;
}

#[tokio::test]
async fn an_end_that_could_not_diff_the_session_says_so() {
    in_a_scratch_database("brain_rest_end", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            a_session_with_a_decision(&pool).await;
            break_it(
                &url,
                "ALTER TABLE brain_observations RENAME COLUMN observation_type TO observation_type_gone",
            )
            .await;

            let ended = call(
                &pool,
                "cuba_jornada",
                json!({"action": "end", "outcome": "success"}),
            )
            .await;
            assert_eq!(ended["action"], "ended");
            must_say_it(&ended, "session_diff");
            pool.close().await;
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn a_current_that_could_not_read_the_last_snapshot_says_so() {
    in_a_scratch_database("brain_rest_cur", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            a_session_with_a_decision(&pool).await;
            break_it(&url, "DROP TABLE brain_compaction_snapshots").await;

            let current = call(&pool, "cuba_jornada", json!({"action": "current"})).await;
            assert_eq!(current["action"], "current");
            must_say_it(&current, "last_snapshot");
            pool.close().await;
        })
        .await;
    })
    .await;
}

// ---------------------------------------------------------------- cuba_calibrar

#[tokio::test]
async fn a_calibration_of_a_table_that_cannot_be_read_fails_instead_of_being_empty() {
    in_a_scratch_database("brain_rest_cal", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;

            let healthy = call(&pool, "cuba_calibrar", json!({"action": "metrics"})).await;
            assert_eq!(
                healthy["n"], 0,
                "an empty verify log is a real n = 0: {healthy}"
            );

            break_it(&url, "DROP TABLE brain_verify_log").await;
            for action in ["metrics", "stats"] {
                let answer = try_call(&pool, "cuba_calibrar", json!({"action": action})).await;
                assert!(
                    answer.is_err(),
                    "cuba_calibrar {action} over a verify log that cannot be read must fail: \
                     answering n = 0 is a calibration of nothing in the shape of a calibration. \
                     Got: {answer:?}"
                );
            }

            break_it(
                &url,
                "ALTER TABLE brain_source_trust RENAME COLUMN alpha TO alpha_gone",
            )
            .await;
            let trust = try_call(&pool, "cuba_calibrar", json!({"action": "trust"})).await;
            assert!(
                trust.is_err(),
                "cuba_calibrar trust over a table that cannot be read must fail. Got: {trust:?}"
            );
            pool.close().await;
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn a_resolve_that_could_not_find_the_sources_to_retrain_says_so() {
    in_a_scratch_database("brain_rest_cal_res", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            let entity = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, 'tema', 'concept')",
            )
            .bind(entity)
            .execute(&pool)
            .await
            .expect("seed the entity");
            sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, 'algo')")
                .bind(entity)
                .execute(&pool)
                .await
                .expect("seed the observation");
            let verify: Uuid = sqlx::query_scalar(
                "INSERT INTO brain_verify_log (claim, entity_name, confidence, grounding_level)
                 VALUES ('algo', 'tema', 0.8, 'verified') RETURNING id",
            )
            .fetch_one(&pool)
            .await
            .expect("seed the prediction");
            break_it(
                &url,
                "ALTER TABLE brain_observations RENAME COLUMN source TO source_gone",
            )
            .await;

            let resolved = call(
                &pool,
                "cuba_calibrar",
                json!({"action": "resolve", "verify_id": verify.to_string(), "outcome": "correct"}),
            )
            .await;
            assert_eq!(resolved["sources_credibility_updated"], 0);
            must_say_it(&resolved, "trust_sources");
            pool.close().await;
        })
        .await;
    })
    .await;
}

// ---------------------------------------------------------------- the other readers of the reminders

#[tokio::test]
async fn an_error_report_that_could_not_look_up_the_reminders_says_so() {
    in_a_scratch_database("brain_rest_alarma", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            break_it(&url, "DROP TABLE brain_triggers").await;

            let reported = call(
                &pool,
                "cuba_alarma",
                json!({"error_type": "TypeError", "error_message": "boom"}),
            )
            .await;
            assert!(
                reported["error_id"].is_string(),
                "the report itself still lands: {reported}"
            );
            must_say_it(&reported, "triggered_reminders");
            pool.close().await;
        })
        .await;
    })
    .await;
}

#[tokio::test]
async fn an_entity_read_that_could_not_look_up_the_reminders_says_so() {
    in_a_scratch_database("brain_rest_alma", |url| async move {
        as_a_client(async {
            let pool = migrated(&url).await;
            call(
                &pool,
                "cuba_alma",
                json!({"action": "create", "name": "tema", "entity_type": "concept"}),
            )
            .await;
            break_it(&url, "DROP TABLE brain_triggers").await;

            let got = call(&pool, "cuba_alma", json!({"action": "get", "name": "tema"})).await;
            assert_eq!(
                got["entity"]["name"], "tema",
                "the read itself still works: {got}"
            );
            must_say_it(&got, "triggered_reminders");
            pool.close().await;
        })
        .await;
    })
    .await;
}
