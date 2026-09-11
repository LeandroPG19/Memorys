use serde_json::json;
use uuid::Uuid;

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ADMIN: &str = "identity-admin-token";

async fn daemon(port: u16) -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    unsafe {
        std::env::set_var("CUBA_HTTP_TOKEN", ADMIN);
        std::env::remove_var("CUBA_PEER_TOKEN");
        std::env::remove_var("CUBA_PANEL");
    }
    let served = pool.clone();
    let addr = format!("127.0.0.1:{port}");
    tokio::spawn(async move {
        let _ = memory_industry::http::serve_pool(&addr, served, true).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    pool
}

async fn open_a_session(port: u16, header: Option<&str>, name: &str) -> serde_json::Value {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .bearer_auth(ADMIN);
    if let Some(id) = header {
        request = request.header("mcp-client-id", id);
    }
    let body = request
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "cuba_jornada",
                "arguments": {"action": "start", "name": name},
                "clientInfo": {"name": "claude-code"}
            }
        }))
        .send()
        .await
        .expect("the daemon answers")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let text = body["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no envelope: {body}"));
    serde_json::from_str(text).expect("json")
}

async fn whose_session(port: u16, header: Option<&str>) -> Option<String> {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .bearer_auth(ADMIN);
    if let Some(id) = header {
        request = request.header("mcp-client-id", id);
    }
    let body = request
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "cuba_jornada",
                "arguments": {"action": "current"},
                "clientInfo": {"name": "claude-code"}
            }
        }))
        .send()
        .await
        .expect("the daemon answers")
        .json::<serde_json::Value>()
        .await
        .expect("json");
    let text = body["result"]["content"][0]["text"].as_str()?;
    let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
    parsed["session"]["id"]
        .as_str()
        .or_else(|| parsed["session_id"].as_str())
        .map(str::to_string)
}

#[tokio::test]
#[ignore]
async fn a_client_that_only_says_claude_code_does_not_inherit_another_agents_session() {
    let _env = ENV_GUARD.lock().await;
    let pool = daemon(18841).await;

    let started = open_a_session(18841, None, "el trabajo de la primera IA").await;
    let owned = started["session"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("jornada start returns the session: {started}"))
        .to_string();

    let inherited = whose_session(18841, None).await;
    assert_ne!(
        inherited.as_deref(),
        Some(owned.as_str()),
        "both calls declared no id, so both fall back to clientInfo.name — the same string for \
         every Claude Code instance. The second call picked up the session the first one \
         opened. Two AIs are connected to this daemon today and only avoid colliding because \
         somebody set Mcp-Client-Id by hand in each project; a third added with the default \
         `claude mcp add` would land here. Losing the session is honest, inheriting somebody \
         else's is not"
    );

    let declared = format!("agent-a-{}", &Uuid::new_v4().to_string()[..8]);
    let mine = open_a_session(18841, Some(&declared), "la que sí se identifica").await;
    let mine_id = mine["session"]["id"]
        .as_str()
        .expect("session id")
        .to_string();
    assert_eq!(
        whose_session(18841, Some(&declared)).await.as_deref(),
        Some(mine_id.as_str()),
        "and an agent that DOES declare an id has to keep its session across calls, or the fix \
         works by breaking sessions for everybody, which is the same loss with better manners"
    );

    sqlx::query("DELETE FROM brain_sessions WHERE id = ANY($1::uuid[])")
        .bind(vec![
            Uuid::parse_str(&owned).expect("uuid"),
            Uuid::parse_str(&mine_id).expect("uuid"),
        ])
        .execute(&pool)
        .await
        .ok();
    unsafe {
        std::env::remove_var("CUBA_HTTP_TOKEN");
    }
}
