//! The loopback side of the bridge: what a local MCP client talks to. It lets nobody in
//! but this PC, adds the credential, and forwards byte for byte.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::state::BridgeState;
use super::{Bridge, Refusal, Route};
use crate::accounts;

/// The daemon's own cap on a request body.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// The JSON-RPC code the bridge uses for everything it cannot do itself.
const BRIDGE_ERROR_CODE: i32 = -32003;

/// Describe one connection, not the end-to-end message: never forwarded in either
/// direction.
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// What the bridge sets itself, or that would name the bridge instead of the host.
const BRIDGE_OWN: [&str; 5] = [
    "host",
    "origin",
    "authorization",
    "mcp-machine-id",
    "content-length",
];

/// The running listener. Dropping it or calling `shutdown` stops serving.
pub struct BridgeHandle {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl BridgeHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn shutdown(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for BridgeHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Header names the client listed in its own `Connection` header: hop-by-hop too.
fn named_by_connection(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .collect()
}

fn is_hop_by_hop(name: &str, named: &[String]) -> bool {
    HOP_BY_HOP.contains(&name) || named.iter().any(|token| token == name)
}

fn machine_id(s: &BridgeState) -> String {
    let id = s.credential_id.simple().to_string();
    format!("{}-{}", s.device_label, &id[..8])
}

/// The headers the host receives: the client's, minus hop-by-hop ones, `Host`, `Origin`,
/// `Authorization` and `Mcp-Machine-Id`, plus the credential and the machine id. Never an
/// invented `Mcp-Client-Id`.
pub fn forward_headers(incoming: &HeaderMap, s: &BridgeState) -> HeaderMap {
    let named = named_by_connection(incoming);
    let mut out = HeaderMap::new();
    for (name, value) in incoming {
        let own = BRIDGE_OWN.contains(&name.as_str());
        if !own && !is_hop_by_hop(name.as_str(), &named) {
            out.append(name.clone(), value.clone());
        }
    }
    let bearer = accounts::bearer(s.credential_id, &s.secret);
    if let Ok(mut credential) = HeaderValue::from_str(&format!("Bearer {bearer}")) {
        credential.set_sensitive(true);
        out.insert(header::AUTHORIZATION, credential);
    }
    if let Ok(machine) = HeaderValue::from_str(&machine_id(s)) {
        out.insert("mcp-machine-id", machine);
    }
    out
}

/// The host's headers on their way back to the client, minus the hop-by-hop ones.
fn response_headers(upstream: &HeaderMap) -> HeaderMap {
    let named = named_by_connection(upstream);
    let mut out = HeaderMap::new();
    for (name, value) in upstream {
        if !is_hop_by_hop(name.as_str(), &named) {
            out.append(name.clone(), value.clone());
        }
    }
    out
}

/// An `Origin` is this bridge's own only when it is this machine on this port. A page
/// served from anywhere else, or from a sandbox (`null`), is not.
fn local_origin(origin: &str, port: u16) -> bool {
    let rest = origin
        .trim()
        .split_once("://")
        .map_or(origin.trim(), |(_, rest)| rest);
    let (name, declared) = rest.rsplit_once(':').unwrap_or((rest, ""));
    matches!(name, "localhost" | "127.0.0.1" | "[::1]") && declared == port.to_string()
}

fn local_host(host: &str, port: u16) -> bool {
    [format!("127.0.0.1:{port}"), format!("localhost:{port}")]
        .iter()
        .any(|own| own.eq_ignore_ascii_case(host.trim()))
}

fn origin_welcome(headers: &HeaderMap, port: u16) -> bool {
    match headers.get(header::ORIGIN) {
        None => true,
        Some(value) => value
            .to_str()
            .is_ok_and(|origin| local_origin(origin, port)),
    }
}

fn host_welcome(headers: &HeaderMap, port: u16) -> bool {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| local_host(host, port))
}

/// Refuses a web page (a foreign `Origin`) and a rebinding attack (a `Host` that is not
/// this bridge's own loopback name) before the host is contacted.
// The `Err` is the 403 itself, built once on the rejection path and handed back as is;
// boxing it would change the signature the tests call.
#[allow(clippy::result_large_err)]
pub fn browser_guard(headers: &HeaderMap, port: u16) -> Result<(), Response> {
    if origin_welcome(headers, port) && host_welcome(headers, port) {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        "this bridge does not serve web pages or other hosts' names",
    )
        .into_response())
}

/// The id of the call being answered, so a client matches an error to its request. A
/// batch, an unreadable body or a call without an id has none.
fn request_id(body: &[u8]) -> Value {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(mut call)) => match call.remove("id") {
            Some(id @ (Value::Number(_) | Value::String(_))) => id,
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

fn rpc_error(id: &Value, status: u16, message: &str) -> Response {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": BRIDGE_ERROR_CODE, "message": message },
    });
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (status, axum::Json(body)).into_response()
}

fn refused(id: &Value, refusal: &Refusal) -> Response {
    rpc_error(id, refusal.status(), &refusal.message())
}

/// Why one send failed, in the only terms that decide whether to retry.
enum SendFailure {
    /// No connection was made: the request cannot have reached the host.
    Connect(String),
    /// A connection existed: the request may have been delivered.
    Other(String),
}

fn failure_of(err: &reqwest::Error) -> SendFailure {
    if err.is_connect() {
        return SendFailure::Connect("no se pudo conectar con la madre".to_string());
    }
    let why = if err.is_timeout() {
        "la madre no contestó a tiempo"
    } else {
        "la conexión con la madre se cortó sin respuesta"
    };
    SendFailure::Other(why.to_string())
}

/// The host's answer, streamed: status and headers now, the body as it arrives. Dropping
/// it (the client left) drops the request to the host.
fn pass_back(upstream: reqwest::Response) -> Response {
    let status = upstream.status();
    let headers = response_headers(upstream.headers());
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

async fn send_once(
    bridge: &Bridge,
    route: &Route,
    parts: &Parts,
    body: &Bytes,
) -> Result<Response, SendFailure> {
    let target = parts.uri.path_and_query().map_or("/mcp", |p| p.as_str());
    let request = bridge
        .client
        .request(
            parts.method.clone(),
            format!("http://{}{target}", route.addr),
        )
        .headers(forward_headers(&parts.headers, &route.state))
        .body(body.clone());
    match tokio::time::timeout(bridge.timing.upstream_headers, request.send()).await {
        Ok(Ok(upstream)) => Ok(pass_back(upstream)),
        Ok(Err(err)) => Err(failure_of(&err)),
        Err(_) => Err(SendFailure::Other(
            "la madre no contestó a tiempo".to_string(),
        )),
    }
}

/// The one retry: only after a failure to connect, so the request cannot have arrived,
/// and only after looking for the host again.
async fn retry_elsewhere(
    bridge: &Bridge,
    parts: &Parts,
    body: &Bytes,
) -> Result<Response, Refusal> {
    bridge.transport_failed().await;
    let route = bridge.route().await?;
    send_once(bridge, &route, parts, body)
        .await
        .map_err(|failure| match failure {
            SendFailure::Connect(why) => Refusal::NotFound(why),
            SendFailure::Other(why) => Refusal::MayHaveArrived(why),
        })
}

async fn forward(bridge: &Bridge, parts: &Parts, body: Bytes) -> Result<Response, Refusal> {
    let route = bridge.route().await?;
    match send_once(bridge, &route, parts, &body).await {
        Ok(response) => Ok(response),
        Err(SendFailure::Connect(_)) => retry_elsewhere(bridge, parts, &body).await,
        Err(SendFailure::Other(why)) => Err(Refusal::MayHaveArrived(why)),
    }
}

async fn relay(bridge: &Bridge, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY).await else {
        return rpc_error(
            &Value::Null,
            413,
            "el cuerpo de la petición supera los 8 MiB o no se pudo leer",
        );
    };
    let id = request_id(&body);
    match forward(bridge, &parts, body).await {
        Ok(response) => response,
        Err(refusal) => refused(&id, &refusal),
    }
}

#[derive(Clone)]
struct App {
    bridge: Arc<Bridge>,
    port: u16,
}

async fn handle(State(app): State<App>, request: Request) -> Response {
    if let Err(refusal) = browser_guard(request.headers(), app.port) {
        return refusal;
    }
    let reading = *request.method() == Method::GET;
    let path = request.uri().path().to_string();
    match path.as_str() {
        "/mcp" => relay(&app.bridge, request).await,
        "/bridge/health" if reading => axum::Json(app.bridge.health()).into_response(),
        _ => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Binds `127.0.0.1:<port>` and nothing else: it takes a port, not an address, so it
/// cannot be configured onto the LAN.
pub async fn start(port: u16, bridge: Arc<Bridge>) -> Result<BridgeHandle> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .with_context(|| {
            format!(
                "cannot listen on 127.0.0.1:{port}: is another bridge, or another program, \
                 using that port?"
            )
        })?;
    let addr = listener
        .local_addr()
        .context("cannot read the bound address")?;
    let app = Router::new().fallback(handle).with_state(App {
        bridge,
        port: addr.port(),
    });
    let task = tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!(error = %err, "the bridge's listener stopped");
        }
    });
    Ok(BridgeHandle { addr, task })
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::{Duration, Instant};

    use axum::http::{HeaderValue, StatusCode};
    use serde_json::Value;

    use super::super::fake_host::{CHUNK_ONE, CHUNK_TWO};
    use super::super::fake_host::{
        Closer, CredStatus, FakeHost, FakeNet, Harness, Identity, McpMode, RawRecorder,
        ResponseReader, contains_ci, eventually, write_state_json,
    };
    use super::*;

    type Case<'a> = (u16, Vec<(&'a str, &'a str)>, &'a [u8]);

    const CALL: &[u8] = br#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;

    struct World {
        id: Identity,
        host: FakeHost,
        h: Harness,
    }

    async fn world() -> World {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let state = id.state(host.addr().port(), vec![host.addr()], Some(host.addr()));
        let h = Harness::start(&state, FakeNet::new()).await;
        World { id, host, h }
    }

    fn rpc(reply: &super::super::fake_host::Reply) -> Value {
        serde_json::from_slice(&reply.body).expect("the bridge's error is a JSON document")
    }

    fn port_of(w: &World) -> u16 {
        w.h.handle.local_addr().port()
    }

    /// H23
    #[tokio::test]
    async fn the_bridge_listens_on_loopback_and_nothing_else() {
        let w = world().await;

        let bound = w.h.handle.local_addr();

        assert_eq!(
            bound.ip(),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            "the bridge adds the credential to anything it receives: bound to 0.0.0.0 or :: \
             it would be a credentialed door onto the LAN for every PC that reaches it"
        );
        assert_ne!(bound.port(), 0, "the OS gave a real port");
    }

    /// H23
    #[tokio::test]
    async fn a_busy_port_fails_and_names_the_port() {
        let id = Identity::new();
        let state = id.state(8787, vec![], None);
        let prepared = Harness::prepare(&state, FakeNet::new());
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
        let port = held.local_addr().expect("its address").port();

        let err = match start(port, prepared.bridge.clone()).await {
            Ok(_) => panic!("two listeners cannot share 127.0.0.1:{port}"),
            Err(err) => err,
        };

        assert!(
            format!("{err:#}").contains(&port.to_string()),
            "the operator needs to know which port is taken to move the bridge or stop the \
             other program. Got: {err:#}"
        );
    }

    /// H25
    #[tokio::test]
    async fn the_forward_is_faithful_and_the_credential_is_the_bridges_own() {
        let w = world().await;
        let mut body =
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"pad":""#.to_vec();
        body.resize(1022, b'x');
        body.extend_from_slice(b"\"}}");
        let sent = [
            ("Content-Type", "application/json"),
            ("Mcp-Client-Id", "ws-1"),
            ("Mcp-Session-Id", "s-9"),
            ("Connection", "keep-alive"),
            ("Authorization", "Bearer ROBADO"),
            ("Mcp-Machine-Id", "falso"),
        ];

        let reply = w.h.send("POST", "/mcp?x=1", &sent, &body).await;

        assert_eq!(reply.status, 200, "anchor: the call went through");
        let seen = w.host.mcp_requests();
        assert_eq!(seen.len(), 1);
        let seen = &seen[0];
        assert_eq!(seen.method, "POST");
        assert_eq!(seen.target, "/mcp?x=1", "path and query travel unchanged");
        assert_eq!(seen.body, body, "the body is forwarded byte for byte");
        assert_eq!(
            seen.all("mcp-client-id"),
            vec!["ws-1"],
            "the client's own id passes"
        );
        assert_eq!(
            seen.all("mcp-session-id"),
            vec!["s-9"],
            "the session id passes"
        );
        assert_eq!(
            seen.all("authorization"),
            vec![w.id.bearer().as_str()],
            "the host must receive the bridge's credential and only that: forwarding the \
             client's Authorization would let a local process pick its own identity"
        );
        assert_eq!(
            seen.all("mcp-machine-id"),
            vec![w.id.machine_id().as_str()],
            "the machine id is the bridge's `<label>-<8 hex>`, stable when the IP changes; the \
             client cannot claim to be another machine"
        );
        assert_eq!(
            seen.header("connection"),
            None,
            "Connection is hop-by-hop: it describes the client's link to the bridge, not the \
             bridge's link to the host"
        );
        assert_eq!(seen.header("origin"), None);
        assert_eq!(
            seen.header("host"),
            Some(format!("127.0.0.1:{}", w.host.addr().port()).as_str()),
            "the Host the host sees is its own address, not the bridge's"
        );
    }

    /// H25
    #[tokio::test]
    async fn the_bridge_never_invents_a_client_or_session_id() {
        let w = world().await;

        w.h.post_mcp(CALL).await;

        let seen = w.host.mcp_requests();
        assert_eq!(
            seen[0].header("mcp-client-id"),
            None,
            "an invented Mcp-Client-Id would give every chat the same session key and collapse \
             them into one"
        );
        assert_eq!(seen[0].header("mcp-session-id"), None);
    }

    /// H25
    #[tokio::test]
    async fn every_status_and_body_of_the_host_comes_back_intact() {
        let w = world().await;
        let cases: [Case<'_>; 7] = [
            (200, vec![], br#"{"jsonrpc":"2.0","id":7,"result":{}}"#),
            (202, vec![], b""),
            (400, vec![], br#"{"error":"bad request"}"#),
            (401, vec![], br#"{"error":"unauthorized"}"#),
            (413, vec![], b"payload too large from the host"),
            (429, vec![("Retry-After", "7")], b"slow down"),
            (
                504,
                vec![],
                b"this daemon gave up on the request after 120s",
            ),
        ];
        for (status, headers, body) in cases {
            w.host.set_mcp(McpMode::reply(status, &headers, body));

            let reply = w.h.post_mcp(CALL).await;

            assert_eq!(
                reply.status, status,
                "the host's {status} is the client's {status}"
            );
            assert_eq!(
                reply.body, body,
                "the body of a {status} comes back as the host wrote it"
            );
            if status == 429 {
                assert_eq!(
                    reply.header("retry-after"),
                    Some("7"),
                    "Retry-After is how the client learns how long to wait"
                );
            }
        }
    }

    /// H25
    #[tokio::test]
    async fn any_method_on_mcp_reaches_the_host_as_it_was_sent() {
        let w = world().await;

        for method in ["GET", "DELETE"] {
            let reply = w.h.send(method, "/mcp", &[], b"").await;
            assert_eq!(reply.status, 200, "{method} /mcp is forwarded");
        }

        let methods: Vec<String> = w
            .host
            .mcp_requests()
            .into_iter()
            .map(|r| r.method)
            .collect();
        assert_eq!(
            methods,
            vec!["GET", "DELETE"],
            "the method is not rewritten to POST"
        );
    }

    /// H25
    #[test]
    fn the_forwarded_headers_drop_the_hop_by_hop_ones_and_add_the_credential() {
        let id = Identity::new();
        let state = id.state(8787, vec![], None);
        let mut incoming = HeaderMap::new();
        for (name, value) in [
            ("connection", "keep-alive"),
            ("keep-alive", "timeout=5"),
            ("proxy-authorization", "Basic eHg6eHg="),
            ("proxy-authenticate", "Basic"),
            ("te", "trailers"),
            ("trailer", "x"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "websocket"),
            ("host", "127.0.0.1:8797"),
            ("origin", "http://127.0.0.1:8797"),
            ("authorization", "Bearer ROBADO"),
            ("mcp-machine-id", "falso"),
            ("mcp-client-id", "ws-1"),
            ("mcp-session-id", "s-9"),
            ("content-type", "application/json"),
            ("accept", "application/json, text/event-stream"),
        ] {
            incoming.append(name, HeaderValue::from_static(value));
        }

        let out = forward_headers(&incoming, &state);

        for gone in [
            "connection",
            "keep-alive",
            "proxy-authorization",
            "proxy-authenticate",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "host",
            "origin",
        ] {
            assert!(
                out.get(gone).is_none(),
                "{gone} must not be forwarded (hop-by-hop, or it would name the bridge instead \
                 of the host)"
            );
        }
        assert_eq!(out.get_all("authorization").iter().count(), 1);
        assert_eq!(
            out.get("authorization").and_then(|v| v.to_str().ok()),
            Some(id.bearer().as_str()),
            "the client's Authorization is replaced, not appended to"
        );
        assert_eq!(
            out.get("mcp-machine-id").and_then(|v| v.to_str().ok()),
            Some(id.machine_id().as_str())
        );
        for kept in [
            ("mcp-client-id", "ws-1"),
            ("mcp-session-id", "s-9"),
            ("content-type", "application/json"),
            ("accept", "application/json, text/event-stream"),
        ] {
            assert_eq!(
                out.get(kept.0).and_then(|v| v.to_str().ok()),
                Some(kept.1),
                "{} passes through untouched",
                kept.0
            );
        }
        let bare = forward_headers(&HeaderMap::new(), &state);
        assert!(
            bare.get("mcp-client-id").is_none() && bare.get("mcp-session-id").is_none(),
            "nothing is invented when the client sent nothing"
        );
    }

    /// H26
    #[tokio::test]
    async fn a_streamed_answer_is_passed_on_as_it_arrives() {
        let w = world().await;
        w.host.set_mcp(McpMode::Stream);
        let addr = w.h.handle.local_addr();

        let (mut reader, first) = tokio::time::timeout(Duration::from_secs(5), async {
            let mut reader = ResponseReader::open(
                addr,
                "POST",
                "/mcp",
                &[("Content-Type", "application/json")],
                CALL,
            )
            .await;
            let mut got = Vec::new();
            while got.len() < CHUNK_ONE.len() {
                got.extend(
                    reader
                        .next_data()
                        .await
                        .expect("the body ended before chunk one"),
                );
            }
            (reader, got)
        })
        .await
        .expect(
            "timeout esperando el trozo 1: the host sent it and withheld the second, so a \
             bridge that waits for the whole body can never show the first",
        );

        assert_eq!(reader.status, 200);
        assert_eq!(
            first, CHUNK_ONE,
            "chunk one arrives intact, before chunk two exists"
        );
        w.host.release_stream();
        let rest = reader.read_all().await;
        assert_eq!(
            rest, CHUNK_TWO,
            "after the gate opens the second chunk and the end follow"
        );
    }

    /// H27
    #[tokio::test]
    async fn a_client_that_leaves_cancels_the_request_to_the_host() {
        let w = world().await;
        w.host.set_mcp(McpMode::Stream);
        let addr = w.h.handle.local_addr();
        let mut reader = tokio::time::timeout(
            Duration::from_secs(5),
            ResponseReader::open(
                addr,
                "POST",
                "/mcp",
                &[("Content-Type", "application/json")],
                CALL,
            ),
        )
        .await
        .expect("the stream starts");
        let first = tokio::time::timeout(Duration::from_secs(5), reader.next_data())
            .await
            .expect("chunk one arrives");
        assert!(
            first.is_some(),
            "anchor: the stream is live before the client leaves"
        );

        drop(reader);

        assert!(
            w.host.wait_stream_closed(Duration::from_secs(2)).await,
            "two seconds after the client closed, the host's connection was still open: the \
             bridge kept a long request alive for nobody, and an event stream that never ends \
             would pin a connection on the host for ever"
        );
    }

    /// H28
    #[tokio::test]
    async fn a_request_that_may_have_arrived_is_not_sent_twice() {
        let w = world().await;
        w.host.set_mcp(McpMode::ReadAndClose);

        let reply = w.h.post_mcp(CALL).await;

        assert_eq!(reply.status, 502);
        assert!(
            reply
                .text()
                .contains("puede haberse entregado; no se reintenta"),
            "the client must be told the call may have run. Got: {}",
            reply.text()
        );
        assert_eq!(
            w.host.mcp_requests().len(),
            1,
            "the host read the whole request and then hung up: repeating a POST that may have \
             started a plant action is how a valve opens twice"
        );
    }

    /// H29
    #[test]
    fn the_browser_guard_lets_in_only_this_machines_own_names() {
        let port = 8797;
        let cases: [(Option<&str>, &str, bool); 9] = [
            (None, "127.0.0.1:8797", true),
            (None, "localhost:8797", true),
            (None, "malo.example:8797", false),
            (None, "127.0.0.1:9999", false),
            (Some("https://malo.example"), "127.0.0.1:8797", false),
            (Some("null"), "127.0.0.1:8797", false),
            (Some("http://127.0.0.1:8797"), "127.0.0.1:8797", true),
            (Some("http://localhost:8797"), "localhost:8797", true),
            (Some("http://127.0.0.1:1234"), "127.0.0.1:8797", false),
        ];
        for (origin, host, allowed) in cases {
            let mut headers = HeaderMap::new();
            headers.insert("host", HeaderValue::from_str(host).expect("a header value"));
            if let Some(origin) = origin {
                headers.insert(
                    "origin",
                    HeaderValue::from_str(origin).expect("a header value"),
                );
            }

            let verdict = browser_guard(&headers, port);

            assert_eq!(
                verdict.is_ok(),
                allowed,
                "origin {origin:?} with Host {host}: a page from another origin, or a name that \
                 only resolves to 127.0.0.1 by DNS rebinding, must not borrow the credential"
            );
            if let Err(response) = verdict {
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
        }
    }

    /// H29
    #[tokio::test]
    async fn a_web_page_cannot_use_the_bridge_and_the_host_is_not_even_asked() {
        let w = world().await;
        let port = port_of(&w);
        let refused: [(&str, Vec<(String, String)>); 3] = [
            (
                "a foreign Origin",
                vec![("Origin".into(), "https://malo.example".into())],
            ),
            (
                "a rebinding Host",
                vec![("Host".into(), format!("malo.example:{port}"))],
            ),
            ("Origin: null", vec![("Origin".into(), "null".into())]),
        ];
        for (name, headers) in refused {
            let borrowed: Vec<(&str, &str)> = headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();

            let reply = w.h.send("POST", "/mcp", &borrowed, CALL).await;

            assert_eq!(reply.status, 403, "{name} must be refused");
        }
        assert!(
            w.host.seen().is_empty(),
            "a refused page must not cost the host even an identity check. Saw: {:?}",
            w.host
                .seen()
                .iter()
                .map(|s| s.target.clone())
                .collect::<Vec<_>>()
        );
    }

    /// H29
    #[tokio::test]
    async fn the_clients_own_origin_and_host_pass_and_the_origin_is_not_forwarded() {
        let w = world().await;
        let port = port_of(&w);
        let own_origin = format!("http://127.0.0.1:{port}");
        let localhost = format!("localhost:{port}");

        let plain = w.h.send("POST", "/mcp", &[], CALL).await;
        let with_origin =
            w.h.send("POST", "/mcp", &[("Origin", own_origin.as_str())], CALL)
                .await;
        let by_name =
            w.h.send("POST", "/mcp", &[("Host", localhost.as_str())], CALL)
                .await;

        assert_eq!(plain.status, 200, "no Origin and the loopback Host passes");
        assert_eq!(with_origin.status, 200, "the bridge's own origin passes");
        assert_eq!(by_name.status, 200, "localhost:<port> is this machine too");
        let forwarded = w.host.mcp_requests();
        assert_eq!(forwarded.len(), 3);
        assert!(
            forwarded.iter().all(|r| r.header("origin").is_none()),
            "the daemon compares Origin with its own port and would reject the bridge itself"
        );
    }

    /// H30
    #[tokio::test]
    async fn a_body_over_eight_mib_is_refused_without_calling_the_host() {
        let w = world().await;
        let body = vec![b'a'; 8 * 1024 * 1024 + 1];

        let reply = w.h.send("POST", "/mcp", &[], &body).await;

        assert_eq!(
            reply.status, 413,
            "the bridge refuses what the daemon would refuse"
        );
        assert!(
            w.host.seen().is_empty(),
            "an oversized body is cut off locally; forwarding it would make the LAN carry 8 MiB \
             only to be told no"
        );
    }

    /// H30
    #[tokio::test]
    async fn a_body_of_exactly_eight_mib_goes_through() {
        let w = world().await;
        let body = vec![b'a'; 8 * 1024 * 1024];

        let reply = w.h.send("POST", "/mcp", &[], &body).await;

        assert_eq!(
            reply.status, 200,
            "the limit is inclusive, as the daemon's is"
        );
        let seen = w.host.mcp_requests();
        assert_eq!(seen[0].body.len(), body.len(), "all of it arrived");
    }

    /// H31
    #[tokio::test]
    async fn when_no_host_is_reachable_the_error_is_json_rpc_with_the_callers_id() {
        let id = Identity::new();
        let dead = Closer::start().await;
        let state = id.state(8787, vec![dead.addr()], Some(dead.addr()));
        let h = Harness::start(&state, FakeNet::new()).await;

        let numbered = h.post_mcp(CALL).await;
        let named = h
            .post_mcp(br#"{"jsonrpc":"2.0","id":"call-1","method":"tools/list"}"#)
            .await;
        let batch = h
            .post_mcp(br#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"}]"#)
            .await;
        let garbage = h.post_mcp(b"this is not json").await;

        for (name, reply, expected_id) in [
            ("a numeric id", &numbered, serde_json::json!(7)),
            ("a string id", &named, serde_json::json!("call-1")),
            ("a batch", &batch, Value::Null),
            ("an unreadable body", &garbage, Value::Null),
        ] {
            assert_eq!(
                reply.status, 502,
                "{name}: no host reachable is a bad gateway"
            );
            let doc = rpc(reply);
            assert_eq!(doc["jsonrpc"], "2.0", "{name}");
            assert_eq!(
                doc["id"], expected_id,
                "{name}: the client matches the answer to its call by id; a null id for a call \
                 that had one leaves it waiting"
            );
            assert_eq!(doc["error"]["code"], -32003, "{name}");
            let message = doc["error"]["message"].as_str().expect("a message");
            assert!(
                message.contains(&dead.addr().to_string()),
                "{name}: the message must name what was tried so the operator can see why. \
                 Got: {message}"
            );
            assert!(
                !contains_ci(&reply.body, &id.secret_hex()),
                "{name}: no error text may carry the secret"
            );
        }
    }

    /// H32
    #[tokio::test]
    async fn the_health_page_describes_the_bridge_without_secrets() {
        let w = world().await;

        let reply = w.h.send("GET", "/bridge/health", &[], b"").await;

        assert_eq!(reply.status, 200);
        let doc = rpc(&reply);
        assert_eq!(doc["paired"], true);
        assert_eq!(
            doc["host_fingerprint"],
            crate::accounts::fingerprint(&w.id.host_id),
            "the fingerprint is how an operator confirms which host this bridge trusts"
        );
        assert!(doc["state"].is_string(), "a one-word state: {doc}");
        assert_eq!(doc["last_good"], w.host.addr().to_string());
        let text = reply.text();
        assert!(
            !contains_ci(text.as_bytes(), &w.id.secret_hex())
                && !contains_ci(text.as_bytes(), &w.id.credential_id.simple().to_string())
                && !contains_ci(text.as_bytes(), &w.id.credential_id.to_string()),
            "the health page is readable by any local process: no secret, no full credential id"
        );
        assert!(
            w.host.seen().is_empty(),
            "asking the bridge about itself does not call the host"
        );
    }

    /// S-16
    #[tokio::test]
    async fn any_other_path_is_a_404_that_never_reaches_the_host() {
        let w = world().await;

        let reply = w.h.send("GET", "/admin/forget", &[], b"").await;

        assert_eq!(
            reply.status, 404,
            "the bridge forwards /mcp and answers /bridge/health; anything else would turn it \
             into a credentialed proxy for every daemon route"
        );
        assert!(w.host.seen().is_empty());
    }

    /// H14
    #[tokio::test]
    async fn a_stale_address_is_healed_and_not_asked_again() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let stale = Closer::start().await;
        let state = id.state(
            host.addr().port(),
            vec![stale.addr(), host.addr()],
            Some(stale.addr()),
        );
        let h = Harness::start(&state, FakeNet::new()).await;

        let first = h.post_mcp(CALL).await;

        assert_eq!(
            first.status, 200,
            "the request is answered by the host on its second address"
        );
        assert_eq!(
            stale.accepted(),
            1,
            "the stale address was asked exactly once"
        );
        eventually("the healed address is stored", || {
            h.stored().is_some_and(|s| s.last_good == Some(host.addr()))
        })
        .await;
        let stored = h.stored().expect("the state file");
        assert_eq!(
            stored.addresses.first(),
            Some(&host.addr()),
            "the address that worked goes to the head of the list"
        );
        assert_eq!(
            stored.secret.to_hex(),
            id.secret_hex(),
            "healing keeps the credential"
        );
        assert_eq!(stored.credential_id, id.credential_id);

        let second = h.post_mcp(CALL).await;

        assert_eq!(second.status, 200);
        assert_eq!(
            stale.accepted(),
            1,
            "the next request goes straight to the healed address; asking the stale one again \
             would tax every call with a dead probe"
        );
        assert_eq!(host.mcp_requests().len(), 2);
    }

    /// H14, S-18
    #[tokio::test]
    async fn a_refused_connection_is_rediscovered_and_retried_once_elsewhere() {
        let id = Identity::new();
        let mut first = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let second = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let state = id.state(
            first.addr().port(),
            vec![first.addr(), second.addr()],
            Some(first.addr()),
        );
        let h = Harness::start(&state, FakeNet::new()).await;
        assert_eq!(
            h.post_mcp(CALL).await.status,
            200,
            "anchor: the first host serves"
        );
        first.stop().await;

        let reply = h.post_mcp(CALL).await;

        assert_eq!(
            reply.status, 200,
            "the verified address died between two requests (inside the freshness window): a \
             connect that was refused never reached the host, so it is safe to look again and \
             resend once"
        );
        assert_eq!(
            first.mcp_requests().len(),
            1,
            "the dead host got nothing more"
        );
        assert_eq!(
            second.mcp_requests().len(),
            1,
            "the retry landed on the other address, once"
        );
    }

    /// H15
    #[tokio::test]
    async fn an_impostor_at_the_last_good_address_never_receives_the_call_or_the_credential() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let impostor = RawRecorder::start(&[(
            "GET /identity",
            serde_json::json!({
                "service": "memory-industry", "host_id": id.host_id, "name": "x",
                "credential": "active", "proof": "00".repeat(32),
            })
            .to_string(),
        )])
        .await;
        let state = id.state(
            host.addr().port(),
            vec![impostor.addr(), host.addr()],
            Some(impostor.addr()),
        );
        let h = Harness::start(&state, FakeNet::new()).await;
        let marker =
            br#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"marker":"BODY-B-7f3a"}}"#;

        let reply = h.post_mcp(marker).await;

        assert_eq!(reply.status, 200);
        assert_eq!(
            host.mcp_requests()[0].body,
            marker,
            "the call ends at the real host, intact"
        );
        let heard = impostor.received();
        for forbidden in [
            "authorization",
            "bearer",
            "mi1.",
            id.secret_hex().as_str(),
            "body-b-7f3a",
        ] {
            assert!(
                !contains_ci(&heard, forbidden),
                "the impostor received {forbidden:?}: the credential or the call left this PC \
                 before the peer proved it was the host"
            );
        }
        assert!(
            impostor
                .lines()
                .iter()
                .all(|l| l.starts_with("GET /identity")),
            "the impostor may only have been asked who it is. Saw {:?}",
            impostor.lines()
        );
    }

    /// H16
    #[tokio::test]
    async fn a_host_with_another_identity_is_rejected_loudly_and_the_state_is_left_alone() {
        let id = Identity::new();
        let other = uuid::Uuid::new_v4();
        let replaced = FakeHost::start(other, vec![id.fake_cred()]).await;
        let state = id.state(
            replaced.addr().port(),
            vec![replaced.addr()],
            Some(replaced.addr()),
        );
        let h = Harness::start(&state, FakeNet::new()).await;
        let before = std::fs::read(&h.state_path).expect("the state file");

        let reply = h.post_mcp(CALL).await;

        assert_eq!(reply.status, 502);
        let message = rpc(&reply)["error"]["message"]
            .as_str()
            .expect("a message")
            .to_string();
        for needed in [
            crate::accounts::fingerprint(&id.host_id),
            crate::accounts::fingerprint(&other),
            "bridge pair".to_string(),
        ] {
            assert!(
                message.contains(&needed),
                "the operator must see both fingerprints and the command that re-pairs; missing \
                 {needed:?} in: {message}"
            );
        }
        assert_eq!(
            std::fs::read(&h.state_path).expect("the state file"),
            before,
            "a host that changed its identity is never adopted on the bridge's own authority: \
             the pinned id in bridge.json is the whole defence against a swapped machine"
        );
        assert!(
            replaced.mcp_requests().is_empty()
                && !contains_ci(&replaced.received_bytes(), "authorization"),
            "and the other installation never saw the call or the credential"
        );
    }

    /// H17
    #[tokio::test]
    async fn a_revoked_credential_is_reported_locally_and_not_hammered() {
        let w = world().await;
        w.host
            .set_cred_status(w.id.credential_id, CredStatus::Revoked);

        for round in 1..=5 {
            let reply = w.h.post_mcp(CALL).await;
            assert!(
                reply.status >= 400,
                "round {round}: a revoked device gets an error"
            );
            assert!(
                reply.text().contains("revocada: empareja de nuevo"),
                "round {round}: the message says what to do. Got: {}",
                reply.text()
            );
        }

        assert!(
            w.host.mcp_requests().is_empty(),
            "no call is sent with a revoked credential"
        );
        assert_eq!(
            w.host.identity_requests().len(),
            1,
            "five calls in a row, one question to the host: a revoked device that keeps asking \
             is what the daemon's brake exists to punish"
        );

        w.h.clock.advance(Duration::from_secs(61));
        w.h.post_mcp(CALL).await;
        assert_eq!(
            w.host.identity_requests().len(),
            2,
            "after revoked_recheck (60 s) it asks once more, so a re-pair is noticed"
        );
    }

    /// H18
    #[tokio::test]
    async fn the_host_is_asked_who_it_is_once_per_freshness_window() {
        let w = world().await;

        w.h.post_mcp(CALL).await;
        w.h.post_mcp(CALL).await;

        assert_eq!(
            w.host.identity_requests().len(),
            1,
            "two calls inside the window, one proof"
        );
        assert_eq!(w.host.mcp_requests().len(), 2);

        w.h.clock.advance(Duration::from_secs(31));
        w.h.post_mcp(CALL).await;

        assert_eq!(
            w.host.identity_requests().len(),
            2,
            "past the window the address is verified again before the credential is sent"
        );
        assert_eq!(w.host.mcp_requests().len(), 3);
    }

    /// H20
    #[tokio::test]
    async fn twenty_simultaneous_calls_share_one_discovery_and_one_sweep_per_cooldown() {
        let id = Identity::new();
        let dead = Closer::start().await;
        let swept = Closer::start().await;
        let state = id.state(8787, vec![dead.addr()], Some(dead.addr()));
        let net = FakeNet::new().sweep(vec![swept.addr()]);
        let h = Harness::start(&state, net).await;
        let started = Instant::now();

        let replies = futures::future::join_all((0..20).map(|_| h.post_mcp(CALL))).await;

        for reply in &replies {
            assert_eq!(reply.status, 502, "nobody to forward to");
            assert!(
                reply.text().contains(&dead.addr().to_string()),
                "every caller is told what was tried. Got: {}",
                reply.text()
            );
        }
        assert!(
            started.elapsed() < super::super::fake_host::tiny_timing().request_waits,
            "twenty callers waiting on one failed discovery must all answer inside request_waits"
        );
        assert_eq!(
            h.net.resolves(),
            1,
            "one discovery for twenty requests: a LAN sweep per request would flood the network \
             exactly when the host is down"
        );
        assert_eq!(h.net.sweeps(), 1);
        assert_eq!(
            dead.accepted(),
            1,
            "the stored address was probed once, not twenty times"
        );

        let later = h.post_mcp(CALL).await;
        assert_eq!(later.status, 502);
        assert_eq!(
            h.net.sweeps(),
            1,
            "inside rescan_cooldown no second sweep starts"
        );

        h.clock.advance(Duration::from_secs(61));
        let after = h.post_mcp(CALL).await;
        assert_eq!(after.status, 502);
        assert_eq!(
            h.net.sweeps(),
            2,
            "after rescan_cooldown the LAN is searched again"
        );
    }

    /// H22
    #[tokio::test]
    async fn pairing_again_while_the_bridge_runs_takes_effect_after_the_recheck() {
        let old = Identity::new();
        let fresh = Identity::on_host(old.host_id);
        let host = FakeHost::start(old.host_id, vec![old.fake_cred(), fresh.fake_cred()]).await;
        host.set_cred_status(old.credential_id, CredStatus::Revoked);
        let state = old.state(host.addr().port(), vec![host.addr()], Some(host.addr()));
        let h = Harness::start(&state, FakeNet::new()).await;
        let refused = h.post_mcp(CALL).await;
        assert!(
            refused.text().contains("revocada: empareja de nuevo"),
            "anchor: the old credential is reported revoked. Got: {}",
            refused.text()
        );

        write_state_json(
            &h.state_path,
            &fresh.state(host.addr().port(), vec![host.addr()], Some(host.addr())),
        );
        h.clock.advance(Duration::from_secs(61));
        let reply = h.post_mcp(CALL).await;

        assert_eq!(
            reply.status, 200,
            "the operator ran `bridge pair` again; the running bridge must pick the new state \
             up without a restart"
        );
        let seen = host.mcp_requests();
        assert_eq!(
            seen.last().expect("one call").all("authorization"),
            vec![fresh.bearer().as_str()],
            "the call carries the NEW credential"
        );
    }
}
