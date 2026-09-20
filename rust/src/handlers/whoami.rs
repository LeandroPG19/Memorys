use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, _args: Value) -> Result<Value> {
    let node = crate::db::node_id(pool).await.ok().map(|id| id.to_string());
    let project = crate::project::current_project_id(pool)
        .await
        .ok()
        .flatten()
        .map(|id| id.to_string());
    let project_name = if let Some(ref id) = project {
        sqlx::query_scalar::<_, String>("SELECT name FROM brain_projects WHERE id = $1::uuid")
            .bind(id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
    } else {
        None
    };

    let provider = std::env::var("MEMORY_INDUSTRY_LLM_PROVIDER")
        .or_else(|_| std::env::var("CUBA_LLM_PROVIDER"))
        .ok()
        .filter(|s| !s.trim().is_empty());
    let base_url = std::env::var("MEMORY_INDUSTRY_LLM_BASE_URL")
        .or_else(|_| std::env::var("CUBA_LLM_BASE_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty());
    let llm_configured = provider.is_some() || base_url.is_some();
    let llm = serde_json::json!({
        "configured": llm_configured,
        "provider": provider,
        "judge_available": crate::cognitive::judge::resolve_offline_llm().is_some(),
        "hint": crate::cognitive::judge::generative_llm_setup_hint(),
    });

    let is_superuser: bool = sqlx::query_scalar("SELECT current_setting('is_superuser') = 'on'")
        .fetch_one(pool)
        .await
        .unwrap_or(true);

    let machine = crate::resources::probe();
    let plan = crate::resources::plan(&machine);

    Ok(serde_json::json!({
        "server": "MemoryIndustry",
        "version": env!("CARGO_PKG_VERSION"),
        "client_id": crate::session::current_client_label()
            .or_else(crate::session::current_client),
        "mcp_session": crate::session::current_mcp_session(),
        "session_id": crate::session::session_id().map(|s| s.to_string()),
        "rls_enforced": !is_superuser,
        "scope": match crate::session::current_scope() {
            crate::session::Scope::Peer => "peer",
            crate::session::Scope::Full => "full",
        },
        "node_id": node,
        "project_id": project,
        "project_name": project_name,
        "llm": llm,
        "graph_db": crate::graph_db::status_summary(),
        "resources": {
            "tier": plan.tier.as_str(),
            "describe": plan.describe(),
            "ram_available_mb": machine.ram_available_mb,
            "vram_free_mb": machine.vram_free_mb,
        },
        "mcp_endpoint": "/mcp",
        "connect_page": "/connect",
        "events": "/events",
        "events_ticket": "/events/ticket",
    }))
}
