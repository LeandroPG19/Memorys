use serde_json::json;

const ADMIN: &str = "panel-admin-token";
const PEER: &str = "panel-peer-token";

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn daemon(port: u16, panel: bool, public: bool) -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    unsafe {
        std::env::set_var("CUBA_HTTP_TOKEN", ADMIN);
        std::env::set_var("CUBA_PEER_TOKEN", PEER);
        if panel {
            std::env::set_var("CUBA_PANEL", "1");
        } else {
            std::env::remove_var("CUBA_PANEL");
        }
        if public {
            std::env::set_var("CUBA_PANEL_PUBLIC", "1");
        } else {
            std::env::remove_var("CUBA_PANEL_PUBLIC");
        }
    }

    let served = pool.clone();
    let addr = format!("127.0.0.1:{port}");
    tokio::spawn(async move {
        let _ = memory_industry::http::serve_pool(&addr, served, true).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    pool
}

async fn rpc(port: u16, token: &str, method: &str) -> (u16, serde_json::Value) {
    let client = reqwest::Client::new();
    let r = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .bearer_auth(token)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": {}}))
        .send()
        .await
        .expect("the daemon answers");
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(serde_json::Value::Null))
}

#[tokio::test]
#[ignore]
async fn a_peer_token_cannot_open_the_admin_surface() {
    let _env = ENV_GUARD.lock().await;
    let _pool = daemon(18811, true, false).await;

    for method in memory_industry::admin::METHODS {
        let (status, body) = rpc(18811, ADMIN, method).await;
        assert_eq!(status, 200, "{method} refused the admin token: {body}");
        assert!(
            body["result"].is_object(),
            "{method} answered without a result, so the control half of this test proves \
             nothing about the refusal half: {body}"
        );

        let (_, refused) = rpc(18811, PEER, method).await;
        assert!(
            refused["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("peer token")),
            "the peer scope is enforced inside handlers::dispatch, and admin/* does not go \
             through dispatch. Without its own check the read-only token — the one that exists \
             so the other machine cannot call cuba_forget — would read the whole diagnostic \
             surface, the client list and the failure log through a different door. \
             {method} answered: {refused}"
        );
    }

    let (_, unknown) = rpc(18811, ADMIN, "admin/whatever").await;
    assert!(
        unknown["error"].is_object(),
        "an unlisted admin method must not be served: {unknown}"
    );
}

#[tokio::test]
#[ignore]
async fn the_panel_route_stays_shut_unless_it_is_switched_on() {
    let _env = ENV_GUARD.lock().await;
    let _pool = daemon(18812, false, false).await;
    let client = reqwest::Client::new();

    let off = client
        .get("http://127.0.0.1:18812/panel")
        .send()
        .await
        .expect("the daemon answers");
    assert_eq!(
        off.status().as_u16(),
        404,
        "without CUBA_PANEL=1 the route must not even be registered. Default-on would publish \
         an administration page on every install that ever binds anything"
    );

    let health = client
        .get("http://127.0.0.1:18812/health")
        .send()
        .await
        .expect("health answers");
    assert_eq!(
        health.status().as_u16(),
        200,
        "and the rest of the daemon has to keep working — a 404 everywhere would make the \
         first assertion meaningless"
    );
}

#[tokio::test]
#[ignore]
async fn the_panel_refuses_a_request_that_arrived_through_a_tunnel() {
    let _env = ENV_GUARD.lock().await;
    let _pool = daemon(18813, true, false).await;
    let client = reqwest::Client::new();

    let direct = client
        .get("http://127.0.0.1:18813/panel")
        .send()
        .await
        .expect("the daemon answers");
    assert_eq!(
        direct.status().as_u16(),
        200,
        "from this machine, with the switch on, the page has to load"
    );
    let body = direct.text().await.expect("a body");
    assert!(
        body.contains("sessionStorage") && body.contains("/mcp"),
        "and it has to be the real page, not an error rendered with a 200"
    );

    assert!(
        memory_industry::http::FORWARDING_HEADERS.contains(&"forwarded"),
        "the list has to include RFC 7239 `Forwarded`, which is the standard header and the one \
         a proxy that follows the spec sends instead of the x- ones. The first version of this \
         check knew only three names and tested itself against exactly those three: it proved \
         the code matched its own list, not that the list matched what proxies send"
    );

    for header in memory_industry::http::FORWARDING_HEADERS {
        let forwarded = client
            .get("http://127.0.0.1:18813/panel")
            .header(header, "203.0.113.7")
            .send()
            .await
            .expect("the daemon answers");
        assert_eq!(
            forwarded.status().as_u16(),
            403,
            "the Cloudflare tunnel points at 127.0.0.1, so behind it the client address is \
             loopback too and checking the peer address proves nothing — a route registered on \
             'we are bound to loopback' would be published to the internet by a tunnel nobody \
             thought about. The forwarding header is the only thing that tells the two apart, \
             and {header} got through.\n\nAnd the honest limit, which belongs next to the \
             check rather than in a commit nobody re-reads: a raw TCP forward (ssh -L, socat) \
             sends no header at all and cannot be caught this way. This guard stops HTTP \
             proxies; it is not proof that a request is local"
        );
    }
}
