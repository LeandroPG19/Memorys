use serde_json::json;
use uuid::Uuid;

fn unique(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

#[tokio::test]
#[ignore]
async fn test_project_scoping_end_to_end() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");

    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("first init_schema");
    drop(pool);
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("second init_schema (idempotent)");
    println!("  ✓ project scoping migration is idempotent");

    let cols: Vec<(String,)> = sqlx::query_as(
        "SELECT table_name::text FROM information_schema.columns
         WHERE column_name = 'project_id'
           AND table_name LIKE 'brain_%'
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .expect("query project_id columns");
    let names: Vec<&str> = cols.iter().map(|(n,)| n.as_str()).collect();
    for required in &[
        "brain_entities",
        "brain_observations",
        "brain_episodes",
        "brain_sessions",
        "brain_errors",
        "brain_relations",
    ] {
        assert!(
            names.contains(required),
            "Missing project_id column on {required}; have: {names:?}"
        );
    }
    let projects_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM information_schema.tables WHERE table_name='brain_projects')",
    )
    .fetch_one(&pool)
    .await
    .expect("check brain_projects table");
    assert!(projects_exists, "brain_projects table missing");
    println!("  ✓ brain_projects + project_id columns present on 6 tables");

    let project_a = unique("test_proj_a");
    let project_b = unique("test_proj_b");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": "session-a", "project": project_a}),
    )
    .await
    .expect("start session A");

    let ent_a = unique("entity_a");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_alma",
        json!({"action": "create", "name": ent_a, "entity_type": "concept"}),
    )
    .await
    .expect("alma create A");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_cronica",
        json!({
            "action": "add",
            "entity_name": ent_a,
            "content": "Project A specific observation",
            "observation_type": "fact"
        }),
    )
    .await
    .expect("cronica add A");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_alarma",
        json!({
            "error_type": "TestError",
            "error_message": "Project A error",
            "project": "default"
        }),
    )
    .await
    .expect("alarma A");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "A done"}),
    )
    .await
    .expect("end session A");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": "session-b", "project": project_b}),
    )
    .await
    .expect("start session B");

    let ent_b = unique("entity_b");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_alma",
        json!({"action": "create", "name": ent_b, "entity_type": "concept"}),
    )
    .await
    .expect("alma create B");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_cronica",
        json!({
            "action": "add",
            "entity_name": ent_b,
            "content": "Project B specific observation",
            "observation_type": "fact"
        }),
    )
    .await
    .expect("cronica add B");

    println!("  ✓ wrote entities/observations/errors under two projects");

    let faro_b = memory_industry::handlers::dispatch(
        &pool,
        "cuba_faro",
        json!({"query": "Project A specific observation", "limit": 20}),
    )
    .await
    .expect("faro under project B");
    let faro_b_text = extract_content_text(&faro_b);
    assert!(
        !faro_b_text.contains(&ent_a),
        "Project B search leaked Project A entity '{ent_a}': {faro_b_text}"
    );
    println!("  ✓ faro under project B does not leak project A content");

    let vigia_b =
        memory_industry::handlers::dispatch(&pool, "cuba_vigia", json!({"metric": "summary"}))
            .await
            .expect("vigia summary B");
    let vigia_b_text = extract_content_text(&vigia_b);
    assert!(
        vigia_b_text.contains("\"project_scoped\":true"),
        "vigia summary did not advertise project_scoped=true: {vigia_b_text}"
    );
    println!("  ✓ vigia summary is project_scoped under active project");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "B done"}),
    )
    .await
    .expect("end session B");
    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": "session-global"}),
    )
    .await
    .expect("start global session");

    let faro_global = memory_industry::handlers::dispatch(
        &pool,
        "cuba_faro",
        json!({"query": ent_a.clone(), "limit": 20}),
    )
    .await
    .expect("faro under global session");
    let faro_global_text = extract_content_text(&faro_global);
    assert!(
        faro_global_text.contains(&ent_a) || faro_global_text.contains(&ent_b),
        "global session should see at least one of the project entities: {faro_global_text}. \
         The query above is this run's own entity name, not the generic word \"Project\": this \
         gate runs against the developer's live database, so a common term competes with the \
         whole corpus for the top-20 and starts failing the day someone saves enough memories — \
         it passed at 1804 observations and failed at 1826 with no code change."
    );
    println!("  ✓ session without project sees rows from any project");

    let list =
        memory_industry::handlers::dispatch(&pool, "cuba_proyecto", json!({"action": "list"}))
            .await
            .expect("proyecto list");
    let list_text = extract_content_text(&list);
    assert!(
        list_text.contains(&project_a) && list_text.contains(&project_b),
        "proyecto list missing projects: {list_text}"
    );

    let stats_a = memory_industry::handlers::dispatch(
        &pool,
        "cuba_proyecto",
        json!({"action": "stats", "name": project_a}),
    )
    .await
    .expect("proyecto stats A");
    let stats_a_text = extract_content_text(&stats_a);
    assert!(
        stats_a_text.contains("\"entities\":1") || stats_a_text.contains("\"entities\": 1"),
        "expected exactly 1 entity in project A: {stats_a_text}"
    );
    println!("  ✓ cuba_proyecto list/stats expose per-project counters");

    unsafe {
        std::env::set_var("CUBA_PROJECT_FILTER", "off");
    }
    let pid = memory_industry::project::current_project_id(&pool)
        .await
        .expect("current_project_id with kill-switch");
    assert!(
        pid.is_none(),
        "kill-switch must force current_project_id() to None"
    );
    unsafe {
        std::env::remove_var("CUBA_PROJECT_FILTER");
    }
    println!("  ✓ CUBA_PROJECT_FILTER=off disables scoping");

    memory_industry::handlers::dispatch(
        &pool,
        "cuba_jornada",
        json!({"action": "end", "outcome": "success", "summary": "test cleanup"}),
    )
    .await
    .ok();

    sqlx::query("DELETE FROM brain_entities WHERE name = $1 OR name = $2")
        .bind(&ent_a)
        .bind(&ent_b)
        .execute(&pool)
        .await
        .ok();
    sqlx::query("DELETE FROM brain_projects WHERE name = $1 OR name = $2")
        .bind(&project_a)
        .bind(&project_b)
        .execute(&pool)
        .await
        .ok();

    println!("\n  ✅ project scoping end-to-end OK");
}

fn extract_content_text(value: &serde_json::Value) -> String {
    value
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
        .map(String::from)
        .unwrap_or_else(|| value.to_string())
}
