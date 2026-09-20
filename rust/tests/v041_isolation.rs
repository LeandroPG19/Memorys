use std::collections::BTreeSet;

use serde_json::json;
use uuid::Uuid;

fn unique(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

static SESSION_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn own_the_session() -> tokio::sync::MutexGuard<'static, ()> {
    // Session + pool stamp are process-global. cargo test runs these #[tokio::test]
    // in parallel; without the lock, decreto A writes under B's project (RLS)
    // and faro searches an empty tenant (count 0).
    // GLOBAL_STATE_GUARD is #[cfg(test)] on the lib and does not exist here.
    SESSION_GUARD.lock().await
}

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect and migrate")
}

fn inner(value: &serde_json::Value) -> serde_json::Value {
    value
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_else(|| value.clone())
}

fn types_in(faro: &serde_json::Value) -> Vec<String> {
    let faro = inner(faro);
    faro.get("results")
        .and_then(|r| r.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    row.get("type")
                        .or_else(|| row.get("t"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn result_blob(value: &serde_json::Value) -> String {
    inner(value)
        .get("results")
        .cloned()
        .unwrap_or(serde_json::json!([]))
        .to_string()
}

#[tokio::test]
#[ignore]
async fn starting_a_second_jornada_does_not_die_on_rls() {
    let _one_at_a_time = own_the_session().await;
    memory_industry::session::clear();
    let pool = pool().await;

    let project_a = unique("iso_a");
    let project_b = unique("iso_b");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": unique("sess_a"), "project": project_a}),
    )
    .await
    .expect("start A");
    let first = memory_industry::session::session_id();

    let started = inner(
        &memory_industry::handlers::dispatch(
            &pool,
            "cuba_jornada",
            json!({"action": "start", "name": unique("sess_b"), "project": project_b}),
        )
        .await
        .expect("start B while A is still open — 0063 must allow the write under cuba_app"),
    );

    assert_eq!(started["action"], "started", "{started}");
    assert_eq!(
        started["replaced_previous"], true,
        "the previous open session on this client has to close as abandoned: {started}"
    );
    let second = memory_industry::session::session_id();
    assert_ne!(first, second, "B must be a new session id");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "v041 rls"}),
    )
    .await
    .ok();
    memory_industry::session::clear();
}

#[tokio::test]
#[ignore]
async fn faro_scope_errors_does_not_return_observations() {
    let _one_at_a_time = own_the_session().await;
    memory_industry::session::clear();
    let pool = pool().await;
    let project = unique("iso_err");
    let marker = unique("err_marker");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": unique("sess_err"), "project": project}),
    )
    .await
    .expect("jornada");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_alma",
        json!({"action": "create", "name": unique("ent_err"), "entity_type": "concept"}),
    )
    .await
    .expect("alma");
    let ent = unique("ent_obs");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_alma",
        json!({"action": "create", "name": &ent, "entity_type": "concept"}),
    )
    .await
    .expect("alma obs");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_cronica",
        json!({
            "action": "add",
            "entity_name": ent,
            "content": format!("{marker} this is an observation not an error"),
            "observation_type": "fact"
        }),
    )
    .await
    .expect("cronica");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_alarma",
        json!({
            "error_type": "IsolationProbe",
            "error_message": format!("{marker} this is the error that scope=errors must find")
        }),
    )
    .await
    .expect("alarma");

    let faro = memory_industry::handlers::dispatch(
        &pool,
        "cuba_faro",
        json!({
            "query": marker,
            "scope": "errors",
            "format": "verbose",
            "limit": 20,
            "enable_bm25": false
        }),
    )
    .await
    .expect("faro scope=errors");

    let kinds = types_in(&faro);
    assert!(
        !kinds.is_empty() && kinds.iter().all(|t| t == "error"),
        "scope=errors leaked non-errors {kinds:?}: {}",
        inner(&faro)
    );
    assert!(
        result_blob(&faro).contains("this is the error"),
        "the error itself has to come back: {}",
        inner(&faro)
    );

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "v041 errors"}),
    )
    .await
    .ok();
    memory_industry::session::clear();
}

#[tokio::test]
#[ignore]
async fn decreto_finds_a_short_exact_title() {
    let _one_at_a_time = own_the_session().await;
    memory_industry::session::clear();
    let pool = pool().await;
    let project = unique("iso_dec");
    let title = unique("Use Postgres for the brain");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": unique("sess_dec"), "project": project}),
    )
    .await
    .expect("jornada");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_decreto",
        json!({
            "action": "record",
            "title": title,
            "context": "v041 isolation",
            "alternatives": ["sqlite"],
            "chosen": "postgres",
            "rationale": "pgvector"
        }),
    )
    .await
    .expect("record");

    let found = inner(
        &memory_industry::handlers::dispatch(
            &pool,
            "cuba_decreto",
            json!({"action": "query", "query": title}),
        )
        .await
        .expect("query"),
    );

    let count = found["count"].as_u64().unwrap_or(0);
    assert!(
        count >= 1,
        "decreto query of the exact title must return the row just written: {found}"
    );

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "v041 decreto"}),
    )
    .await
    .ok();
    memory_industry::session::clear();
}

#[tokio::test]
#[ignore]
async fn null_project_rows_stay_hidden_until_include_unscoped() {
    let _one_at_a_time = own_the_session().await;
    memory_industry::session::clear();
    let pool = pool().await;
    let marker = format!(
        "imported leftover observation {} about postgres connection errors",
        unique("null_leak")
    );
    let entity = unique("null_ent");

    sqlx::query(
        "INSERT INTO brain_entities (name, entity_type, project_id)
         VALUES ($1, 'concept', NULL)",
    )
    .bind(&entity)
    .execute(&pool)
    .await
    .expect("unscoped entity");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, project_id)
         SELECT id, $2, 'fact', NULL FROM brain_entities
         WHERE name = $1 AND project_id IS NULL",
    )
    .bind(&entity)
    .bind(&marker)
    .execute(&pool)
    .await
    .expect("unscoped observation");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": unique("sess_null"), "project": unique("iso_null")}),
    )
    .await
    .expect("jornada");

    let hidden = memory_industry::handlers::dispatch(
        &pool,
        "cuba_faro",
        json!({
            "query": marker,
            "format": "verbose",
            "limit": 20,
            "scope": "observations"
        }),
    )
    .await
    .expect("faro default hides NULL");
    assert!(
        !result_blob(&hidden).contains(&marker),
        "an active project must not see leftover NULL rows unless asked: {}",
        inner(&hidden)
    );

    let shown = memory_industry::handlers::dispatch(
        &pool,
        "cuba_faro",
        json!({
            "query": marker,
            "format": "verbose",
            "limit": 20,
            "scope": "observations",
            "include_unscoped": true
        }),
    )
    .await
    .expect("faro include_unscoped");
    assert!(
        result_blob(&shown).contains(&marker),
        "include_unscoped is the escape hatch: {}",
        inner(&shown)
    );

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "v041 null"}),
    )
    .await
    .ok();
    memory_industry::session::clear();
}

#[tokio::test]
#[ignore]
async fn the_same_entity_name_can_exist_in_two_projects() {
    let _one_at_a_time = own_the_session().await;
    memory_industry::session::clear();
    let pool = pool().await;
    let a = unique("iso_ent_a");
    let b = unique("iso_ent_b");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": unique("sess_ent_a"), "project": a}),
    )
    .await
    .expect("A");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_decreto",
        json!({
            "action": "record",
            "title": "Shared label",
            "context": "project A",
            "chosen": "A",
            "rationale": "A"
        }),
    )
    .await
    .expect("decreto A");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "A"}),
    )
    .await
    .ok();

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": unique("sess_ent_b"), "project": b}),
    )
    .await
    .expect("B");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_decreto",
        json!({
            "action": "record",
            "title": "Shared label",
            "context": "project B",
            "chosen": "B",
            "rationale": "B"
        }),
    )
    .await
    .expect("decreto B — 0064 must allow a second architecture_decisions node");

    let mut tx = memory_industry::project::begin_write_scope(&pool)
        .await
        .expect("lift RLS so the count can see both tenants");
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT p.name FROM brain_entities e
         JOIN brain_projects p ON p.id = e.project_id
         WHERE e.name = 'architecture_decisions' AND p.name IN ($1, $2)
         ORDER BY p.name",
    )
    .bind(&a)
    .bind(&b)
    .fetch_all(&mut *tx)
    .await
    .expect("list architecture_decisions per project");
    tx.commit().await.ok();
    assert_eq!(
        names,
        BTreeSet::from([a.clone(), b.clone()])
            .into_iter()
            .collect::<Vec<_>>(),
        "each project owns its architecture_decisions node, got {names:?}"
    );

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "v041 entity"}),
    )
    .await
    .ok();
    memory_industry::session::clear();
}

#[tokio::test]
#[ignore]
async fn two_protocol_sessions_do_not_share_a_jornada() {
    let _one_at_a_time = own_the_session().await;
    memory_industry::session::clear();
    let pool = pool().await;
    let a = unique("iso_chat_a");
    let b = unique("iso_chat_b");

    memory_industry::session::with_identity("cursor".into(), Some("chat-a".into()), async {
        memory_industry::handlers::dispatch(
            &pool,
            "cuba_jornada",
            json!({"action": "start", "name": unique("win_a"), "project": a}),
        )
        .await
        .expect("chat-a");
        let project_a = memory_industry::session::project_id();

        memory_industry::session::with_identity("cursor".into(), Some("chat-b".into()), async {
            memory_industry::handlers::dispatch(
                &pool,
                "cuba_jornada",
                json!({"action": "start", "name": unique("win_b"), "project": b}),
            )
            .await
            .expect("chat-b");
            assert_ne!(
                memory_industry::session::project_id(),
                project_a,
                "Mcp-Session-Id must split two chats that share Mcp-Client-Id=cursor"
            );
        })
        .await;

        assert_eq!(
            memory_industry::session::project_id(),
            project_a,
            "chat-a must still be bound to project A after chat-b started"
        );
    })
    .await;

    memory_industry::session::forget_client("cursor");
    memory_industry::session::clear();
}

#[tokio::test]
#[ignore]
async fn quarantined_observations_do_not_surface_in_faro() {
    let _one_at_a_time = own_the_session().await;
    memory_industry::session::clear();
    let pool = pool().await;
    let project = unique("iso_q");
    let visible = format!(
        "trusted quarantine probe {} that faro must return",
        unique("q_ok")
    );
    let hidden = format!(
        "quarantined secret {} that faro must not return",
        unique("q_no")
    );
    let entity = unique("q_ent");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": unique("sess_q"), "project": project}),
    )
    .await
    .expect("jornada");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_alma",
        json!({"action": "create", "name": &entity, "entity_type": "concept"}),
    )
    .await
    .expect("alma");

    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, project_id, trust)
         SELECT id, $2, 'fact', project_id, 'trusted' FROM brain_entities
         WHERE name = $1
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&entity)
    .bind(&visible)
    .execute(&pool)
    .await
    .expect("trusted row");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, project_id, trust)
         SELECT id, $2, 'fact', project_id, 'quarantined' FROM brain_entities
         WHERE name = $1
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&entity)
    .bind(&hidden)
    .execute(&pool)
    .await
    .expect("quarantined row");

    let faro = memory_industry::handlers::dispatch(
        &pool,
        "cuba_faro",
        json!({
            "query": "quarantine probe",
            "format": "verbose",
            "limit": 20,
            "scope": "observations"
        }),
    )
    .await
    .expect("faro");
    let hits = result_blob(&faro);
    assert!(
        hits.contains(&visible),
        "trusted twin must surface so a miss is not a dead search: {} / {hits}",
        inner(&faro)
    );
    assert!(
        !hits.contains(&hidden),
        "quarantined text must not reach faro: {} / {hits}",
        inner(&faro)
    );

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "v041 quarantine"}),
    )
    .await
    .ok();
    memory_industry::session::clear();
}
