use serde_json::json;
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
async fn auto_extract_falls_back_to_the_local_cli_when_the_client_has_no_sampling() {
    assert!(
        !memory_industry::protocol::client_supports_sampling(),
        "this test must run outside an MCP session so the fallback is what gets exercised"
    );
    assert!(
        memory_industry::cognitive::judge::resolve_offline_llm().is_some(),
        "no generative LLM (OpenAI-compat URL or claude/gemini CLI). {}",
        memory_industry::cognitive::judge::generative_llm_setup_hint()
    );

    let pool = pool().await;
    let subject = unique_name("Proyecto");
    let text = format!(
        "El servicio {subject} corre sobre PostgreSQL y depende de Redis para la cola de \
         trabajos. Lo mantiene el equipo de plataforma."
    );

    let result = memory_industry::handlers::ingesta::handle(
        &pool,
        json!({ "action": "auto_extract", "text": text, "entity_hint": subject }),
    )
    .await
    .expect("auto_extract must not error when a CLI is reachable");

    let reason = result.get("reason").and_then(|v| v.as_str());
    assert_ne!(
        reason,
        Some("no_backend"),
        "auto_extract reported that no LLM is reachable while resolve_offline_llm found one \
         two assertions ago. That is the failure this test exists for: auto_extract was dead \
         for months while its suite reported green. Got: {result}"
    );
    if matches!(reason, Some("out_of_budget") | Some("backend_failed")) {
        panic!(
            "generative LLM was found but the call failed ({reason:?}). Soft-skipping here \
             made the gate green while extract was broken. Fix auth/timeout/model, or point \
             MEMORY_INDUSTRY_LLM_BASE_URL at a healthy OpenAI-compat server. Result: {result}"
        );
    }
    assert_ne!(
        result.get("degraded").and_then(|v| v.as_bool()),
        Some(true),
        "with a generative backend reachable auto_extract must not degrade: {result}"
    );
    let backend = result.get("backend").and_then(|v| v.as_str());
    assert!(
        matches!(
            backend,
            Some("claude_cli") | Some("gemini_cli") | Some("openai_compat")
        ),
        "the reply must name a known generative backend, got {backend:?}: {result}"
    );

    let extracted = result
        .get("extracted")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let linked = result
        .get("relations_linked")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert!(
        extracted > 0 || linked > 0,
        "the CLI must return something usable from a fact-dense text: {result}"
    );

    let stored: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM brain_observations o
         JOIN brain_entities e ON e.id = o.entity_id
         WHERE o.source = 'inference' AND e.name ILIKE $1",
    )
    .bind(format!("%{subject}%"))
    .fetch_one(&pool)
    .await
    .expect("counting inferred observations");

    if extracted > 0 {
        assert!(
            stored.0 > 0,
            "extracted facts must land tagged source='inference', which is what production had \
             zero of"
        );
    }

    sqlx::query("DELETE FROM brain_entities WHERE name ILIKE $1")
        .bind(format!("%{subject}%"))
        .execute(&pool)
        .await
        .ok();
}

#[tokio::test]
#[ignore]
async fn the_judge_still_reaches_a_verdict_with_mcp_servers_disabled() {
    assert!(
        memory_industry::cognitive::judge::resolve_offline_llm().is_some(),
        "no generative LLM for the offline judge: {}",
        memory_industry::cognitive::judge::generative_llm_setup_hint()
    );

    let judge = memory_industry::cognitive::judge::resolve_offline_llm().expect("checked above");
    let judgment = memory_industry::cognitive::judge::ContradictionJudge::judge(
        judge.as_ref(),
        "El servicio corre en el puerto 8080.",
        "El servicio corre en el puerto 9090.",
    )
    .await
    .expect("the offline judge must still answer once MCP servers are excluded");

    assert!(
        matches!(
            judgment.backend.as_str(),
            "claude_cli" | "gemini_cli" | "openai_compat"
        ),
        "unexpected backend: {judgment:?}"
    );
    assert_eq!(
        judgment.verdict, "contradicts",
        "two different ports for one service contradict each other: {judgment:?}"
    );
}

#[test]
fn the_cli_json_envelope_is_unwrapped_but_a_bare_reply_is_left_alone() {
    let enveloped = r#"{"type":"result","subtype":"success","is_error":false,
        "result":"{\"facts\":[],\"relations\":[]}","session_id":"abc","total_cost_usd":0.01}"#;
    assert_eq!(
        memory_industry::cognitive::judge::unwrap_cli_reply(enveloped),
        r#"{"facts":[],"relations":[]}"#,
        "the CLI wraps its answer in an envelope; the extractor must see the inner text"
    );

    let bare = r#"{"facts":[{"entity_name":"x","content":"y"}],"relations":[]}"#;
    assert_eq!(
        memory_industry::cognitive::judge::unwrap_cli_reply(bare),
        bare,
        "a sampling reply has no envelope and must survive untouched"
    );

    let fenced = "```json\n{\"facts\":[]}\n```";
    assert_eq!(
        memory_industry::cognitive::judge::unwrap_cli_reply(fenced),
        fenced,
        "non-JSON text must pass through for the downstream parser to handle"
    );
}
