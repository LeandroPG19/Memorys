// needs-a-server: starts the real daemon on loopback ports against scratch databases it creates and drops, and pairs, authenticates and revokes real accounts through it
//
//! The daemon's half of accounts, over real HTTP: `/identity`, `/pair`, who is admitted and as
//! whom, the failure brake they share, attribution in `whoami`, the failure log and the log
//! lines. What `doctor` says about it lives in `v029_doctor_reads_only_its_world.rs`, a binary of
//! its own: a world for `doctor` changes the process environment, and this one reads it.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched. One daemon per test, each on its own port, each over its own scratch database, so
//! the per-address brake of one test is never the brake of another.
//!
//! What cannot be driven from here: `MEMORY_INDUSTRY_REQUIRE_ACCOUNTS` refusing the shared token
//! needs a non-loopback bind, and a test cannot ask for one without depending on the machine
//! (127.0.0.2 is loopback too). That half is tested in `http::admission_tests` with a hand-built
//! state; this file covers the other half of the knob, that a loopback daemon ignores it and says
//! so, and the manual check on the host covers the real bind.

mod common;
mod ephemeral;

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use common::in_a_scratch_database;
use memory_industry::accounts::{
    self, PairRequest, PairResponse, Secret, code_key, generate_code, identity_proof,
    normalize_code, pair_request_mac, pair_response_mac, random_bytes, verify_mac,
};
use memory_industry::accounts_db::{self as db, CandidateState, ClaimOutcome, CredentialState};
use memory_industry::http::{Brake, PairOutcome, Reason, pair_core};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const SHARED: &str = "g-shared-token-for-the-lan-0123456789ab";
const PEER: &str = "g-peer-token-for-the-other-machine-0123";

/// The daemon's own ceiling on loading models before it opens for business. Short here because
/// nothing in this file asks the daemon anything a model could answer.
const WARM_CEILING: Duration = Duration::from_secs(2);

/// How long a test waits for its daemon to open the port. It must outlast the warm ceiling or a
/// daemon that did exactly what it was told is failed for it; the next line makes that a build
/// error instead of a sentence somebody can read past.
const OPEN_BUDGET: Duration = Duration::from_secs(30);

const _: () = assert!(
    OPEN_BUDGET.as_secs() > WARM_CEILING.as_secs(),
    "the probe budget must outlast the warm ceiling, or the daemon is failed for opening on time"
);

const PROBE_BUDGET: Duration = Duration::from_secs(5);
const PROBE_EVERY: Duration = Duration::from_millis(250);

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ---------------------------------------------------------------------------------------------
// The log the daemon writes, captured in memory by the one subscriber this test binary installs.
// JSON, so a field is a field whatever quoting the daemon's `tracing` calls choose.
// ---------------------------------------------------------------------------------------------

struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the log buffer")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn captured_log() -> &'static Arc<Mutex<Vec<u8>>> {
    static LOG: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    LOG.get_or_init(|| {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || Sink(sink.clone()))
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect(
            "this test binary installs the only tracing subscriber; something else got there \
             first, so the log lines below could not be read",
        );
        buffer
    })
}

fn log_text() -> String {
    String::from_utf8_lossy(&captured_log().lock().expect("the log buffer")).into_owned()
}

fn log_events() -> Vec<Value> {
    log_text()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The daemon, started the way the other real-HTTP tests start it.
// ---------------------------------------------------------------------------------------------

async fn probe(client: &reqwest::Client, url: &str) -> Result<(), String> {
    client
        .get(url)
        .send()
        .await
        .map(|_| ())
        .map_err(|e| format!("no answer at all: {e}"))
}

/// Block until `/health` answers on `port`, or fail saying it never did. Polling, not sleeping a
/// fixed number: any number is right on an idle machine and wrong on the busy one that failed.
async fn wait_until_it_serves(port: u16, daemon: &tokio::task::JoinHandle<()>) {
    let url = format!("http://127.0.0.1:{port}/health");
    let client = reqwest::Client::builder()
        .timeout(PROBE_BUDGET)
        .build()
        .expect("a probe client with a bounded attempt");
    let deadline = Instant::now() + OPEN_BUDGET;
    let mut last = String::from("it never answered");

    while Instant::now() < deadline {
        assert!(
            !daemon.is_finished(),
            "the daemon on 127.0.0.1:{port} ended before it served anything, so nothing below \
             can run. serve_pool printed why it stopped; `already in use` there means the OS \
             gave this port to another socket between `ephemeral::port()` and the bind, see \
             tests/ephemeral/mod.rs"
        );
        match probe(&client, &url).await {
            Ok(()) => return,
            Err(why) => last = why,
        }
        tokio::time::sleep(PROBE_EVERY).await;
    }
    panic!(
        "the daemon at {url} never answered within {}s. Last: {last}. serve_pool binds the port \
         before it loads its models, and it is pinned to open within {}s of warm-up",
        OPEN_BUDGET.as_secs(),
        WARM_CEILING.as_secs()
    );
}

/// Starts a daemon over `pool` and returns the port it was given. The caller holds `ENV_GUARD`:
/// the daemon reads its environment once, at boot, and the environment is the process's, shared
/// by every test here. The same lock is what keeps two daemons of this binary from being handed
/// one port between asking the OS and binding it.
async fn serve(pool: PgPool, env: &[(&str, Option<&str>)]) -> u16 {
    captured_log();
    unsafe {
        std::env::set_var("CUBA_HTTP_TOKEN", SHARED);
        std::env::set_var("CUBA_PEER_TOKEN", PEER);
        std::env::set_var(
            "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
            WARM_CEILING.as_secs().to_string(),
        );
        // And the models stay off: a daemon warming this developer's models cost ~25 s a start in
        // a debug build, and every test here starts one while the rest queue on `ENV_GUARD`
        // (553 s for the 20 tests, measured 2026-10-13). Nothing here asks a model anything:
        // `ping`, `cuba_whoami`, a `cuba_archivo` call that fails on its action, and the HTTP
        // routes. The same set as `v042::quiet_machine`. Set for every daemon of this binary and
        // never removed: they all want it quiet, and no test here reads these variables back.
        std::env::set_var("CUBA_RERANKER_PATH", "disabled-by-resource-plan");
        std::env::set_var("CUBA_NLI_PATH", "disabled-by-resource-plan");
        std::env::set_var(
            "ONNX_MODEL_PATH",
            std::env::temp_dir().join("v029-accounts-no-such-model"),
        );
        std::env::set_var("CUBA_WARM_RERANKER", "0");
        for name in [
            "CUBA_PANEL",
            "CUBA_HTTP_ALLOWED_ORIGINS",
            "MEMORY_INDUSTRY_REQUIRE_ACCOUNTS",
            "CUBA_NODE_NAME",
        ] {
            std::env::remove_var(name);
        }
        for (name, value) in env {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
    let port = ephemeral::port();
    let task = tokio::spawn(async move {
        let addr = format!("127.0.0.1:{port}");
        if let Err(why) = memory_industry::http::serve_pool(&addr, pool, false).await {
            eprintln!("the daemon on 127.0.0.1:{port} stopped: {why:#}");
        }
    });
    wait_until_it_serves(port, &task).await;
    port
}

async fn start(pool: &PgPool) -> u16 {
    let _env = ENV_GUARD.lock().await;
    serve(pool.clone(), &[]).await
}

// Migration 0041 runs `ALTER ROLE cuba_app`, a `pg_authid` row, global to the server. The product
// already retries that race between servers (`retry_on_catalog_race`, 5 attempts), but with the
// daemons started without models (553 s -> 10.9 s) this binary's ~20 tests migrate their scratch
// databases at the same instant and some exhaust the retries: `while executing migration 41: error
// returned from database: tuple concurrently updated` (g1, g7). Here the binary itself causes the
// race, so it migrates one database at a time.
static MIGRATE_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn migrated(url: &str) -> PgPool {
    let _one_at_a_time = MIGRATE_GUARD.lock().await;
    memory_industry::db::create_pool(url)
        .await
        .expect("migrating the scratch database")
}

// ---------------------------------------------------------------------------------------------
// Talking to it.
// ---------------------------------------------------------------------------------------------

struct Reply {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("the body is not JSON ({e}): {}", self.body))
    }

    fn error_message(&self) -> String {
        self.json()["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// The headers a client could tell two answers apart by, without the clock.
    fn headers_without_the_date(&self) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .filter(|(name, _)| name.as_str() != "date")
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect()
    }
}

async fn send(request: reqwest::RequestBuilder) -> Reply {
    let response = request.send().await.expect("the daemon answers");
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.text().await.expect("a body");
    Reply {
        status,
        headers,
        body,
    }
}

fn url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

async fn mcp(port: u16, bearer: Option<&str>, body: Value) -> Reply {
    let mut request = reqwest::Client::new().post(url(port, "/mcp")).json(&body);
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    send(request).await
}

fn ping() -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})
}

fn tool_call(name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    })
}

/// What a tool answered: the JSON the daemon wraps in `content[0].text`.
fn tool_result(reply: &Reply) -> Value {
    let envelope = reply.json();
    let text = envelope["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no tool result in {envelope}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("the tool result is not JSON ({e}): {text}"))
}

async fn whoami(port: u16, bearer: &str) -> Value {
    let reply = mcp(port, Some(bearer), tool_call("cuba_whoami", json!({}))).await;
    assert_eq!(reply.status, 200, "whoami must be served: {}", reply.body);
    tool_result(&reply)
}

async fn identity(port: u16, query: &str) -> Reply {
    send(reqwest::Client::new().get(url(port, &format!("/identity{query}")))).await
}

// ---------------------------------------------------------------------------------------------
// Accounts, credentials and pairing requests, built with the same functions the client uses.
// ---------------------------------------------------------------------------------------------

async fn account(pool: &PgPool, name: &str) {
    db::add_account(pool, name, Utc::now())
        .await
        .unwrap_or_else(|e| panic!("add_account({name}) must succeed: {e:#}"));
}

/// A device paired straight through the storage layer, as of now. Returns its credential id and
/// the secret the "client" generated.
async fn paired(
    pool: &PgPool,
    account_name: &str,
    host_label: Option<&str>,
    label: &str,
) -> (Uuid, Secret) {
    let now = Utc::now();
    let issued = db::issue_pairing_code(pool, account_name, host_label, now)
        .await
        .unwrap_or_else(|e| panic!("issue_pairing_code({account_name}) must succeed: {e:#}"));
    let key = code_key(&normalize_code(&issued.code).expect("an issued code normalizes"));
    let candidates = db::pairing_candidates(pool, now)
        .await
        .expect("pairing_candidates");
    let candidate = candidates
        .iter()
        .find(|c| c.code_key == key)
        .expect("the code just issued is among the candidates");
    let secret = Secret::generate();
    match db::claim_pairing_code(pool, candidate, label, secret.hash(), now)
        .await
        .expect("claim_pairing_code")
    {
        ClaimOutcome::Created(created) => (created.credential_id, secret),
        other => panic!("a fresh code for {account_name} must be Created, got {other:?}"),
    }
}

async fn issue(
    pool: &PgPool,
    account_name: &str,
    host_label: Option<&str>,
    at: DateTime<Utc>,
) -> String {
    db::issue_pairing_code(pool, account_name, host_label, at)
        .await
        .unwrap_or_else(|e| panic!("issue_pairing_code({account_name}) must succeed: {e:#}"))
        .code
}

/// One pairing request for `code`, built the way a client builds it.
struct Ask {
    code: String,
    request: PairRequest,
    code_key: [u8; 32],
    nonce: [u8; 16],
    hash: [u8; 32],
    secret: Secret,
}

fn ask(code: &str, label: &str) -> Ask {
    ask_with(code, label, Secret::generate())
}

fn ask_with(code: &str, label: &str, secret: Secret) -> Ask {
    let key = code_key(&normalize_code(code).expect("a code that normalizes"));
    let nonce = random_bytes::<16>();
    let hash = secret.hash();
    let mac = pair_request_mac(&key, &nonce, label, &hash);
    Ask {
        code: code.to_string(),
        request: PairRequest {
            v: 1,
            nc: hex::encode(nonce),
            label: label.to_string(),
            h: hex::encode(hash),
            mac: hex::encode(mac),
        },
        code_key: key,
        nonce,
        hash,
        secret,
    }
}

impl Ask {
    /// The same request with its MAC changed by one character: a client that does not know the code.
    fn with_a_bad_mac(&self) -> PairRequest {
        let mut bad = self.request.clone();
        let first = if bad.mac.starts_with('0') { "1" } else { "0" };
        bad.mac.replace_range(0..1, first);
        bad
    }
}

async fn pair(port: u16, request: &PairRequest) -> Reply {
    send(
        reqwest::Client::new()
            .post(url(port, "/pair"))
            .json(request),
    )
    .await
}

async fn status_of_code(pool: &PgPool, code: &str) -> CandidateState {
    let key = code_key(&normalize_code(code).expect("a code that normalizes"));
    let candidates = db::pairing_candidates(pool, Utc::now())
        .await
        .expect("pairing_candidates");
    let candidate = candidates
        .iter()
        .find(|c| c.code_key == key)
        .unwrap_or_else(|| panic!("the code is not among the candidates"));
    db::classify_candidate(candidate, Utc::now())
}

fn bearer_of(id: Uuid, secret: &Secret) -> String {
    accounts::bearer(id, secret)
}

async fn audit_payloads(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT payload FROM brain_audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .expect("reading the audit log")
}

async fn whole_audit_text(pool: &PgPool) -> String {
    let rows: Vec<String> = sqlx::query_scalar("SELECT payload::text FROM brain_audit_log")
        .fetch_all(pool)
        .await
        .expect("reading the audit log");
    rows.join("\n")
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0)
        .single()
        .expect("a valid instant")
}

fn ip(last: u8) -> std::net::IpAddr {
    std::net::IpAddr::from([10, 0, 0, last])
}

// =============================================================================================
// G1 · /identity without parameters
// =============================================================================================

#[tokio::test]
async fn g1_identity_names_the_service_the_installation_and_a_label_and_nothing_else() {
    in_a_scratch_database("brain_g_identity", |url_| async move {
        let pool = migrated(&url_).await;
        let host_id = db::host_identity(&pool)
            .await
            .expect("host identity")
            .host_id;

        let _env = ENV_GUARD.lock().await;
        let port = serve(pool.clone(), &[("CUBA_NODE_NAME", Some("planta-madre"))]).await;

        let anonymous = identity(port, "").await;
        assert_eq!(
            anonymous.status, 200,
            "a client that has never been paired asks who answers, so there is no credential to \
             ask for: {}",
            anonymous.body
        );
        let body = anonymous.json();
        assert_eq!(body["service"], json!("memory-industry"));
        assert_eq!(
            body["host_id"],
            json!(host_id.to_string()),
            "the id the client pins is the one in brain_host_identity, the row migration 0065 \
             created, so it survives a restart and a restore"
        );
        assert_eq!(
            body["name"],
            json!("planta-madre"),
            "CUBA_NODE_NAME is the label the operator chose"
        );
        let mut keys: Vec<&str> = body
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            ["host_id", "name", "service"],
            "to anyone on the LAN this tells which service answered and nothing more: not the \
             version, not a path, not the database"
        );
        assert!(
            !anonymous.body.contains(env!("CARGO_PKG_VERSION")),
            "the version would tell a stranger which known bugs to try: {}",
            anonymous.body
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g1_the_label_falls_back_to_the_machine_name_and_then_to_nothing() {
    in_a_scratch_database("brain_g_label", |url_| async move {
        let pool = migrated(&url_).await;

        let _env = ENV_GUARD.lock().await;
        let first = serve(
            pool.clone(),
            &[("HOSTNAME", Some("hostname-g1")), ("COMPUTERNAME", None)],
        )
        .await;
        assert_eq!(
            identity(first, "").await.json()["name"],
            json!("hostname-g1"),
            "without CUBA_NODE_NAME the daemon says the name of the machine it runs on"
        );

        let second = serve(
            pool.clone(),
            &[("HOSTNAME", None), ("COMPUTERNAME", Some("PC-LAB-G1"))],
        )
        .await;
        assert_eq!(
            identity(second, "").await.json()["name"],
            json!("PC-LAB-G1"),
            "and on Windows, where there is no HOSTNAME, COMPUTERNAME"
        );

        let third = serve(pool.clone(), &[("HOSTNAME", None), ("COMPUTERNAME", None)]).await;
        let nameless = identity(third, "").await;
        assert_eq!(nameless.status, 200, "{}", nameless.body);
        assert_eq!(
            nameless.json()["name"],
            json!(""),
            "a host with no name says so with an empty string: the field is always there, so a \
             client never has to tell «absent» from «unreadable»"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G2 + G3 · /identity with a credential
// =============================================================================================

#[tokio::test]
async fn g2_g3_identity_proves_knowledge_of_an_active_credential_and_only_that() {
    in_a_scratch_database("brain_g_proof", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        account(&pool, "bob").await;
        let host_id = db::host_identity(&pool)
            .await
            .expect("host identity")
            .host_id;
        let (active, secret) = paired(&pool, "ana", None, "pc-1").await;
        let (to_revoke, _) = paired(&pool, "ana", None, "pc-2").await;
        let (of_bob, _) = paired(&pool, "bob", None, "pc-3").await;
        let port = start(&pool).await;

        let nonce = random_bytes::<16>();
        let reply = identity(
            port,
            &format!("?cred={}&nonce={}", active.simple(), hex::encode(nonce)),
        )
        .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body = reply.json();
        assert_eq!(body["credential"], json!("active"));
        let expected = hex::encode(identity_proof(&secret.hash(), &host_id, &active, &nonce));
        assert_eq!(
            body["proof"],
            json!(expected),
            "the proof is the HMAC keyed by the credential's hash over the host, the credential \
             and this nonce: it is what lets the client send its secret to THIS host and not to \
             whoever answered on the old address. Keyed by the secret itself, or ignoring the \
             nonce, it would not match what the client computes"
        );
        assert!(
            !reply.body.contains(&secret.to_hex())
                && !reply.body.contains(&hex::encode(secret.hash())),
            "neither the secret nor its hash may ever be in the answer: {}",
            reply.body
        );

        let other_nonce = random_bytes::<16>();
        let again = identity(
            port,
            &format!(
                "?cred={}&nonce={}",
                active.simple(),
                hex::encode(other_nonce)
            ),
        )
        .await
        .json();
        assert_eq!(
            again["proof"],
            json!(hex::encode(identity_proof(
                &secret.hash(),
                &host_id,
                &active,
                &other_nonce
            ))),
            "a fresh nonce gives its own proof; one proof that served for every nonce could be \
             recorded once and replayed by an impostor forever"
        );
        assert_ne!(again["proof"], body["proof"]);

        db::revoke_device(&pool, "ana", "pc-2", Utc::now())
            .await
            .expect("revoking one device");
        db::revoke_account(&pool, "bob", Utc::now())
            .await
            .expect("revoking an account");
        for (why, id, state) in [
            ("a revoked device", to_revoke, "revoked"),
            ("a credential of a revoked account", of_bob, "revoked"),
            ("a credential nobody ever issued", Uuid::new_v4(), "unknown"),
        ] {
            let reply = identity(
                port,
                &format!("?cred={}&nonce={}", id.simple(), hex::encode(nonce)),
            )
            .await;
            assert_eq!(reply.status, 200, "{why}: {}", reply.body);
            let body = reply.json();
            assert_eq!(body["credential"], json!(state), "{why}");
            assert!(
                body.get("proof").is_none(),
                "{why}: a proof for a credential that no longer opens anything would tell a \
                 stolen device it still has a host to talk to, and the bridge uses the absence \
                 of the proof to stop trying: {body}"
            );
        }

        for (why, query) in [
            (
                "a credential id that is not 32 hex digits",
                format!("?cred=xyz&nonce={}", hex::encode(nonce)),
            ),
            (
                "a credential id with the dashes of a UUID",
                format!("?cred={active}&nonce={}", hex::encode(nonce)),
            ),
            (
                "a nonce that is too short",
                format!("?cred={}&nonce=00", active.simple()),
            ),
            (
                "a nonce that is not hex",
                format!("?cred={}&nonce={}", active.simple(), "z".repeat(32)),
            ),
        ] {
            let reply = identity(port, &query).await;
            assert_eq!(reply.status, 400, "{why}: {}", reply.body);
            assert_eq!(
                reply.json(),
                json!({"error": "bad request"}),
                "{why}: one sentence for every malformed query, so the answer teaches nothing \
                 about which part was wrong"
            );
        }
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G4 · /pair succeeds
// =============================================================================================

#[tokio::test]
async fn g4_pairing_creates_the_credential_and_the_host_proves_it_knew_the_code() {
    in_a_scratch_database("brain_g_pair", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let host_id = db::host_identity(&pool).await.expect("host identity").host_id;
        let port = start(&pool).await;

        let code = issue(&pool, "ana", Some("pc-lab-1"), Utc::now()).await;
        let asked = ask(&code, "mi-pc");
        let reply = pair(port, &asked.request).await;

        assert_eq!(reply.status, 200, "a valid code must pair: {}", reply.body);
        let response: PairResponse = serde_json::from_str(&reply.body).expect("a PairResponse");
        assert_eq!(response.host_id, host_id);
        assert_eq!(response.account, "ana");
        assert_eq!(
            response.label, "pc-lab-1",
            "the label the operator fixed when issuing the code wins over the one the client sends"
        );
        let expected = pair_response_mac(
            &asked.code_key,
            &asked.nonce,
            &asked.hash,
            &response.host_id,
            &response.credential_id,
            &response.account,
            &response.label,
            &response.host_name,
        );
        assert!(
            verify_mac(expected, &response.mac),
            "the response is MACed with the key derived from the code: it is the proof, before \
             the client stores anything, that the host answering knows the code. Signed with the \
             hash instead of the code key, an impostor could forge it from what it was sent"
        );

        let stored = db::credential_for_auth(&pool, response.credential_id)
            .await
            .expect("looking the credential up")
            .expect("the credential exists");
        assert_eq!(
            stored.secret_hash, asked.hash,
            "the database holds exactly the hash the client sent, as 32 bytes: not the hex text \
             of it, which would never match a bearer"
        );
        assert_eq!(stored.device_label, "pc-lab-1");
        assert_eq!(stored.account, "ana");
        assert_eq!(stored.state, CredentialState::Active);
        assert_eq!(status_of_code(&pool, &code).await, CandidateState::Used);

        assert!(
            !reply.body.contains(&asked.secret.to_hex())
                && !reply.body.contains(&asked.request.h),
            "the secret never travels, and its hash is not echoed: {}",
            reply.body
        );

        let events = audit_payloads(&pool, "account.pair_redeemed").await;
        assert_eq!(events.len(), 1, "one redemption, one audit event: {events:?}");
        assert_eq!(events[0]["account"], json!("ana"));
        assert_eq!(events[0]["device"], json!("pc-lab-1"));
        let audit = whole_audit_text(&pool).await;
        for hidden in [
            asked.secret.to_hex(),
            asked.request.h.clone(),
            asked.request.mac.clone(),
            code.clone(),
            code.replace('-', ""),
        ] {
            assert!(
                !audit.contains(&hidden),
                "the audit chain is append-only and cannot be scrubbed; {hidden:?} must never reach it"
            );
        }

        let free_code = issue(&pool, "ana", None, Utc::now()).await;
        let free = pair(port, &ask(&free_code, "mi-pc").request).await;
        assert_eq!(free.status, 200, "{}", free.body);
        assert_eq!(
            serde_json::from_str::<PairResponse>(&free.body)
                .expect("a PairResponse")
                .label,
            "mi-pc",
            "a code that fixed no label takes the one the client brings"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G5 · one body for every failure, many reasons inside
// =============================================================================================

struct Failures {
    unknown: Ask,
    expired: Ask,
    used: Ask,
    account_revoked: Ask,
    bad_mac: Ask,
}

/// Five requests that must all fail, each for a different reason, built over `pool`.
async fn five_ways_to_fail(pool: &PgPool) -> Failures {
    account(pool, "ana").await;
    account(pool, "bob").await;

    let unknown = ask(&generate_code(), "pc");

    let long_ago = Utc::now() - ChronoDuration::hours(25);
    let expired = ask(&issue(pool, "ana", None, long_ago).await, "pc");

    let spent_code = issue(pool, "ana", None, Utc::now()).await;
    let first = ask(&spent_code, "pc");
    let key = first.code_key;
    let candidates = db::pairing_candidates(pool, Utc::now())
        .await
        .expect("candidates");
    let candidate = candidates
        .iter()
        .find(|c| c.code_key == key)
        .expect("the spent code is a candidate");
    match db::claim_pairing_code(pool, candidate, "pc", first.hash, Utc::now())
        .await
        .expect("claiming")
    {
        ClaimOutcome::Created(_) => {}
        other => panic!("the first redemption must create the credential, got {other:?}"),
    }
    let used = ask(&spent_code, "pc");

    let bob_code = issue(pool, "bob", None, Utc::now()).await;
    sqlx::query("UPDATE brain_accounts SET revoked_at = $1 WHERE name = 'bob'")
        .bind(Utc::now())
        .execute(pool)
        .await
        .expect(
            "revoking bob in place: revoke_account would delete his unused codes, and this case \
             is the code that survived the revocation",
        );
    let account_revoked = ask(&bob_code, "pc");

    let bad_mac = ask(&issue(pool, "ana", None, Utc::now()).await, "pc");
    Failures {
        unknown,
        expired,
        used,
        account_revoked,
        bad_mac,
    }
}

#[tokio::test]
async fn g5_every_pairing_failure_looks_the_same_from_outside_and_differs_inside() {
    in_a_scratch_database("brain_g_oracle", |url_| async move {
        let pool = migrated(&url_).await;
        let failures = five_ways_to_fail(&pool).await;
        let port = start(&pool).await;
        let log_before = log_text().len();

        let sent = [
            ("an unknown code", failures.unknown.request.clone()),
            ("an expired code", failures.expired.request.clone()),
            (
                "a code already used by another device",
                failures.used.request.clone(),
            ),
            (
                "a code of a revoked account",
                failures.account_revoked.request.clone(),
            ),
            (
                "a request with a bad MAC",
                failures.bad_mac.with_a_bad_mac(),
            ),
        ];
        let mut replies = Vec::new();
        for (why, request) in &sent {
            let reply = pair(port, request).await;
            assert_eq!(reply.status, 401, "{why}: {}", reply.body);
            replies.push((*why, reply));
        }
        let first = &replies[0].1;
        assert_eq!(
            first.json(),
            json!({"error": "pairing failed"}),
            "the one sentence every failure gets"
        );
        for (why, reply) in &replies[1..] {
            assert_eq!(
                reply.body, first.body,
                "{why}: the body differs from the unknown code's, so a stranger can learn which \
                 codes exist, which expired and which belong to a revoked account just by asking"
            );
            assert_eq!(
                reply.headers_without_the_date(),
                first.headers_without_the_date(),
                "{why}: the headers differ, which is the same oracle by another route"
            );
        }

        let reasons = [
            Reason::UnknownOrBadMac,
            Reason::Expired,
            Reason::Used,
            Reason::AccountRevoked,
            Reason::UnknownOrBadMac,
        ];
        for ((why, request), expected) in sent.iter().zip(reasons) {
            let outcome = pair_core(&pool, request, ip(9), Utc::now(), &Brake::default()).await;
            assert!(
                matches!(outcome, PairOutcome::Refused(got) if got == expected),
                "{why}: the operator's log has to say why, even though the client is not told. \
                 Expected Refused({expected:?}), got {outcome:?}"
            );
        }

        let log = log_text();
        let lines: Vec<Value> = log[log_before..]
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        let logged: Vec<String> = lines
            .iter()
            .filter(|event| event["level"] == "WARN")
            .filter_map(|event| event["fields"].get("reason"))
            .map(|reason| {
                reason
                    .to_string()
                    .to_lowercase()
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric())
                    .collect()
            })
            .collect();
        for needed in ["unknownorbadmac", "expired", "used", "accountrevoked"] {
            assert!(
                logged.iter().any(|reason| reason == needed),
                "a refused pairing has to leave a WARN with a `reason` field, and none said \
                 {needed:?}. Seen: {logged:?}"
            );
        }
        for (what, secret) in [
            (
                "the MAC of the bad request",
                failures.bad_mac.with_a_bad_mac().mac,
            ),
            (
                "the MAC of the unknown code's request",
                failures.unknown.request.mac.clone(),
            ),
            ("the hash sent", failures.unknown.request.h.clone()),
            (
                "the hash of the expired request",
                failures.expired.request.h.clone(),
            ),
            ("the secret", failures.unknown.secret.to_hex()),
        ] {
            assert!(
                !log.contains(&secret),
                "{what} reached the log, which is shipped to places the database is not"
            );
        }
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g5_malformed_requests_are_a_400_that_is_not_a_pairing_failure() {
    in_a_scratch_database("brain_g_malformed", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let port = start(&pool).await;
        let code = issue(&pool, "ana", None, Utc::now()).await;
        let good = ask(&code, "pc").request;

        let client = reqwest::Client::new();
        let mut short_nonce = good.clone();
        short_nonce.nc = good.nc[..30].to_string();
        let mut hash_not_hex = good.clone();
        hash_not_hex.h = "z".repeat(64);
        let mut short_mac = good.clone();
        short_mac.mac = good.mac[..62].to_string();
        let mut upper_mac = good.clone();
        upper_mac.mac = good.mac.to_uppercase();

        let mut cases: Vec<(&str, reqwest::RequestBuilder)> = vec![(
            "a body that is not JSON",
            client
                .post(url(port, "/pair"))
                .header("content-type", "application/json")
                .body("this is not json"),
        )];
        for (why, request) in [
            ("a nonce of 30 hex digits", &short_nonce),
            ("a hash that is not hex", &hash_not_hex),
            ("a MAC of 62 hex digits", &short_mac),
            ("a MAC in upper case", &upper_mac),
        ] {
            cases.push((why, client.post(url(port, "/pair")).json(request)));
        }
        for (why, request) in cases {
            let reply = send(request).await;
            assert_eq!(reply.status, 400, "{why}: {}", reply.body);
            assert_eq!(
                reply.json(),
                json!({"error": "bad request"}),
                "{why}: a request that cannot be parsed is not a guess at a code, and the reply \
                 must not read like one"
            );
        }
        assert_eq!(
            status_of_code(&pool, &code).await,
            CandidateState::Redeemable,
            "none of them touched the code"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G6 · the brake, per address and shared
// =============================================================================================

#[tokio::test]
async fn g6_ten_failures_from_an_address_brake_it_and_only_it_until_the_window_ends() {
    in_a_scratch_database("brain_g_brake", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let brake = Brake::default();
        let now = Utc::now();
        let (a, b, c) = (ip(1), ip(2), ip(3));

        let for_a = ask(&issue(&pool, "ana", None, now).await, "pc-a");
        let wrong = for_a.with_a_bad_mac();
        for attempt in 1..=10 {
            let outcome = pair_core(&pool, &wrong, a, now, &brake).await;
            assert!(
                matches!(outcome, PairOutcome::Refused(Reason::UnknownOrBadMac)),
                "failure {attempt} of 10 from A is refused, not braked yet: {outcome:?}"
            );
        }

        let braked = pair_core(&pool, &for_a.request, a, now, &brake).await;
        let wait = match braked {
            PairOutcome::Braked(wait) => wait,
            other => panic!(
                "the 11th request from A carries a GOOD code and must still be braked, or ten \
                 free guesses a minute stay free: {other:?}"
            ),
        };
        assert!(
            !wait.is_zero() && wait <= Duration::from_secs(60),
            "the wait is what is left of the 60 s window: {wait:?}"
        );
        assert_eq!(
            status_of_code(&pool, &for_a.code).await,
            CandidateState::Redeemable,
            "and the braked request did not redeem anything"
        );

        let for_b = ask(&issue(&pool, "ana", None, now).await, "pc-b");
        assert!(
            matches!(
                pair_core(&pool, &for_b.request, b, now, &brake).await,
                PairOutcome::Ok(_)
            ),
            "the brake is per address: B shares nothing with A"
        );

        brake.write().expect("the brake").insert(
            a,
            (
                10,
                Instant::now()
                    .checked_sub(Duration::from_secs(61))
                    .expect("the process is older than a minute"),
            ),
        );
        assert!(
            matches!(
                pair_core(&pool, &for_a.request, a, now, &brake).await,
                PairOutcome::Ok(_)
            ),
            "a window that has run its 60 s is over and A can try again"
        );

        let for_c = ask(&issue(&pool, "ana", None, now).await, "pc-c");
        for _ in 0..9 {
            pair_core(&pool, &for_c.with_a_bad_mac(), c, now, &brake).await;
        }
        assert!(
            matches!(
                pair_core(&pool, &for_c.request, c, now, &brake).await,
                PairOutcome::Ok(_)
            ),
            "nine failures are still inside the allowance: the limit is ten, not nine"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g6_a_good_redemption_and_a_replay_both_clear_the_failures_of_their_address() {
    in_a_scratch_database("brain_g_clears", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let brake = Brake::default();
        let now = Utc::now();
        let (d, e) = (ip(4), ip(5));

        let for_d = ask(&issue(&pool, "ana", None, now).await, "pc-d");
        for _ in 0..3 {
            pair_core(&pool, &for_d.with_a_bad_mac(), d, now, &brake).await;
        }
        assert!(
            brake.read().expect("the brake").contains_key(&d),
            "control: three failures are on the books before the redemption"
        );
        assert!(matches!(
            pair_core(&pool, &for_d.request, d, now, &brake).await,
            PairOutcome::Ok(_)
        ));
        assert!(
            !brake.read().expect("the brake").contains_key(&d),
            "a correct redemption clears the address, like a correct token does: a device that \
             mistyped a code twice must not carry those two toward the limit forever"
        );

        let for_e = ask(&issue(&pool, "ana", None, now).await, "pc-e");
        assert!(matches!(
            pair_core(&pool, &for_e.request, e, now, &brake).await,
            PairOutcome::Ok(_)
        ));
        for _ in 0..3 {
            pair_core(&pool, &for_e.with_a_bad_mac(), e, now, &brake).await;
        }
        assert!(matches!(
            pair_core(&pool, &for_e.request, e, now, &brake).await,
            PairOutcome::Replayed(_)
        ));
        assert!(
            !brake.read().expect("the brake").contains_key(&e),
            "a replay is the retry of a device that lost the answer, and it clears the address \
             too"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g6_expiry_is_judged_by_the_clock_pair_core_is_given() {
    in_a_scratch_database("brain_g_clock", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let one_second = ChronoDuration::seconds(1);
        let day = ChronoDuration::hours(24);

        let last_moment = ask(&issue(&pool, "ana", None, t0()).await, "pc-1");
        let outcome = pair_core(
            &pool,
            &last_moment.request,
            ip(1),
            t0() + day - one_second,
            &Brake::default(),
        )
        .await;
        assert!(
            matches!(outcome, PairOutcome::Ok(_)),
            "a code issued at T is good one second before T + 24 h by the caller's clock, \
             whatever the machine's clock says today: {outcome:?}"
        );

        let first_moment_too_late = ask(&issue(&pool, "ana", None, t0()).await, "pc-2");
        let outcome = pair_core(
            &pool,
            &first_moment_too_late.request,
            ip(1),
            t0() + day,
            &Brake::default(),
        )
        .await;
        assert!(
            matches!(outcome, PairOutcome::Refused(Reason::Expired)),
            "at T + 24 h exactly the code is expired (valid only while now < expires_at): \
             {outcome:?}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g6_failures_of_pair_and_of_mcp_add_up_in_one_counter_and_identity_adds_nothing() {
    in_a_scratch_database("brain_g_shared_brake", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let port = start(&pool).await;
        let code = issue(&pool, "ana", None, Utc::now()).await;
        let asked = ask(&code, "pc");

        let nobody = format!(
            "?cred={}&nonce={}",
            Uuid::new_v4().simple(),
            hex::encode([0u8; 16])
        );
        for _ in 0..15 {
            let reply = identity(port, &nobody).await;
            assert_eq!(reply.status, 200, "{}", reply.body);
        }
        let clears = mcp(port, Some(SHARED), ping()).await;
        assert_eq!(
            clears.status, 200,
            "fifteen questions to /identity about credentials nobody has are not failures: \
             asking who answers is what a client does on every start, and a brake on it would \
             lock a whole subnet out of discovery. {}",
            clears.body
        );

        for _ in 0..5 {
            assert_eq!(pair(port, &asked.with_a_bad_mac()).await.status, 401);
        }
        for _ in 0..5 {
            assert_eq!(mcp(port, Some("not-the-token"), ping()).await.status, 401);
        }

        let braked_mcp = mcp(port, Some(SHARED), ping()).await;
        assert_eq!(
            braked_mcp.status, 429,
            "five bad pairing requests and five bad tokens are ten failures from one address, \
             and the eleventh request is braked even with the right token: there is one counter, \
             or an attacker gets ten guesses at each door per minute instead of ten in all"
        );
        let wait: u64 = braked_mcp
            .headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .expect("a 429 says how long to wait");
        assert!((1..=60).contains(&wait), "Retry-After: {wait}");

        let braked_pair = pair(port, &asked.request).await;
        assert_eq!(
            braked_pair.status, 429,
            "and the same counter brakes /pair, even with the right code: {}",
            braked_pair.body
        );
        assert!(braked_pair.headers.contains_key("retry-after"));
        assert_eq!(
            status_of_code(&pool, &code).await,
            CandidateState::Redeemable,
            "a braked request redeems nothing"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G7 · a lost response can be asked for again
// =============================================================================================

#[tokio::test]
async fn g7_the_same_pairing_request_twice_gives_the_same_credential_once() {
    in_a_scratch_database("brain_g_replay", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let port = start(&pool).await;
        let code = issue(&pool, "ana", None, Utc::now()).await;
        let asked = ask(&code, "pc");

        let first = pair(port, &asked.request).await;
        let second = pair(port, &asked.request).await;

        assert_eq!(first.status, 200, "{}", first.body);
        assert_eq!(
            second.status, 200,
            "a device whose first answer was lost on the way asks again with the same request \
             and must get the answer, not «pairing failed»: {}",
            second.body
        );
        let (first, second): (PairResponse, PairResponse) = (
            serde_json::from_str(&first.body).expect("a PairResponse"),
            serde_json::from_str(&second.body).expect("a PairResponse"),
        );
        assert_eq!(first.credential_id, second.credential_id);
        let credentials: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM brain_account_credentials c JOIN brain_accounts a \
             ON a.id = c.account_id WHERE a.name = 'ana'",
        )
        .fetch_one(&pool)
        .await
        .expect("counting credentials");
        assert_eq!(
            credentials, 1,
            "asking twice must not mint two credentials from one code"
        );

        let someone_else = pair(port, &ask_with(&code, "pc", Secret::generate()).request).await;
        assert_eq!(
            someone_else.status, 401,
            "but a different device presenting the same code, with a different secret, is a \
             second redemption and is refused: {}",
            someone_else.body
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G8 · not through a proxy, not from a page, not a big body
// =============================================================================================

#[tokio::test]
async fn g8_identity_and_pair_refuse_proxies_foreign_pages_and_big_bodies() {
    in_a_scratch_database("brain_g_doors", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let port = start(&pool).await;
        let code = issue(&pool, "ana", None, Utc::now()).await;
        let asked = ask(&code, "pc");
        let client = reqwest::Client::new();

        for header in memory_industry::http::FORWARDING_HEADERS {
            let identity_through = send(
                client
                    .get(url(port, "/identity"))
                    .header(header, "203.0.113.7"),
            )
            .await;
            assert_eq!(
                identity_through.status, 403,
                "/identity through a proxy that sent {header}: the Cloudflare tunnel points at \
                 127.0.0.1, so behind it every client looks local, and a pairing route published \
                 to the internet is the thing S-22 exists to prevent"
            );
            let pair_through = send(
                client
                    .post(url(port, "/pair"))
                    .header(header, "203.0.113.7")
                    .json(&asked.request),
            )
            .await;
            assert_eq!(
                pair_through.status, 403,
                "/pair through a proxy that sent {header}: the brake is per address and behind a \
                 tunnel every client is one address"
            );
        }

        for (route, request) in [
            (
                "/identity",
                client
                    .get(url(port, "/identity"))
                    .header("origin", "https://malo.example"),
            ),
            (
                "/pair",
                client
                    .post(url(port, "/pair"))
                    .header("origin", "https://malo.example")
                    .json(&asked.request),
            ),
        ] {
            assert_eq!(
                send(request).await.status,
                403,
                "{route} from a page on another origin: any page the operator opens could \
                 otherwise ask the host who it is, or start guessing codes from the operator's \
                 own machine"
            );
        }
        assert_eq!(
            send(
                client
                    .get(url(port, "/identity"))
                    .header("origin", format!("http://127.0.0.1:{port}"))
            )
            .await
            .status,
            200,
            "the daemon's own origin is allowed, as on every other route"
        );

        let at_the_limit = send(
            client
                .post(url(port, "/pair"))
                .header("content-type", "application/json")
                .body(vec![b' '; 4096]),
        )
        .await;
        assert_eq!(
            at_the_limit.status, 400,
            "4 KiB is the biggest pairing request the daemon reads: the body is read and found \
             wanting, not refused for its size. {}",
            at_the_limit.body
        );
        let over_the_limit = send(
            client
                .post(url(port, "/pair"))
                .header("content-type", "application/json")
                .body(vec![b' '; 4097]),
        )
        .await;
        assert_eq!(
            over_the_limit.status, 413,
            "one byte more is refused for its size. A pairing request is a few hundred bytes; \
             the 8 MiB the rest of the daemon reads would be a free way to hold memory for \
             anyone on the LAN"
        );

        let control = pair(port, &asked.request).await;
        assert_eq!(
            control.status, 200,
            "control: every refusal above happened before the code was looked at, so the same \
             request, with nothing wrong about how it arrived, still pairs: {}",
            control.body
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G9 · an account credential opens the doors the shared token opens
// =============================================================================================

#[tokio::test]
async fn g9_an_account_credential_is_a_full_scope_caller_who_is_named_in_whoami() {
    in_a_scratch_database("brain_g_admit", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let (id, secret) = paired(&pool, "ana", None, "pc-lab-1").await;
        let bearer = bearer_of(id, &secret);
        let port = start(&pool).await;

        let named = whoami(port, &bearer).await;
        assert_eq!(
            named["scope"],
            json!("full"),
            "an account has the same reach as the shared token (S-2): a peer scope here would \
             make every paired PC unable to write"
        );
        assert_eq!(named["account"], json!("ana"), "{named}");
        assert_eq!(named["device"], json!("pc-lab-1"), "{named}");

        let legacy = whoami(port, SHARED).await;
        assert_eq!(legacy["scope"], json!("full"));
        assert_eq!(legacy["server"], json!("MemoryIndustry"));
        assert!(
            legacy.get("account").is_some_and(Value::is_null)
                && legacy.get("device").is_some_and(Value::is_null),
            "the shared token is nobody in particular, and says so with null rather than by \
             leaving the keys out: {legacy}"
        );

        let client = reqwest::Client::new();
        let ticket = send(
            client
                .post(url(port, "/events/ticket"))
                .bearer_auth(&bearer),
        )
        .await;
        assert_eq!(ticket.status, 200, "/events/ticket: {}", ticket.body);
        assert!(
            ticket.json()["ticket"]
                .as_str()
                .is_some_and(|t| !t.is_empty())
        );

        let stream = client
            .get(url(port, "/events"))
            .bearer_auth(&bearer)
            .send()
            .await
            .expect("the daemon answers");
        assert_eq!(stream.status().as_u16(), 200, "/events by header");
        assert!(
            stream
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with("text/event-stream")),
            "and it is the event stream, not an error page"
        );
        drop(stream);
        let refused = client
            .get(url(port, "/events"))
            .bearer_auth(bearer_of(id, &Secret::generate()))
            .send()
            .await
            .expect("the daemon answers");
        assert_eq!(
            refused.status().as_u16(),
            401,
            "control: the right id with the wrong secret does not open /events"
        );

        let full = send(client.get(url(port, "/health")).bearer_auth(&bearer))
            .await
            .json();
        assert!(
            full["runtime"].is_object() && full["clients"].is_array(),
            "/health gives a paired device the full view, as it gives it to the shared token: \
             {full}"
        );
        let anonymous = send(client.get(url(port, "/health"))).await.json();
        assert!(
            anonymous.get("runtime").is_none() && anonymous.get("clients_count").is_some(),
            "control: with no credential the view is the limited one: {anonymous}"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G10 · revocation bites on the next request
// =============================================================================================

#[tokio::test]
async fn g10_a_revoked_device_or_account_stops_on_the_very_next_request() {
    in_a_scratch_database("brain_g_revoke", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        account(&pool, "bob").await;
        let (first, first_secret) = paired(&pool, "ana", None, "pc-1").await;
        let (second, second_secret) = paired(&pool, "ana", None, "pc-2").await;
        let (third, third_secret) = paired(&pool, "bob", None, "pc-3").await;
        let (first, second, third) = (
            bearer_of(first, &first_secret),
            bearer_of(second, &second_secret),
            bearer_of(third, &third_secret),
        );
        let port = start(&pool).await;

        for (who, bearer) in [("pc-1", &first), ("pc-2", &second), ("pc-3", &third)] {
            assert_eq!(
                mcp(port, Some(bearer), ping()).await.status,
                200,
                "{who} is admitted before anything is revoked"
            );
        }

        db::revoke_device(&pool, "ana", "PC-1", Utc::now())
            .await
            .expect("revoking a device");
        let stopped = mcp(port, Some(&first), ping()).await;
        assert_eq!(
            stopped.status, 401,
            "no pause, no restart: the request after the revocation is the one that finds out. \
             A cache of the credential row would let the stolen device keep working for as long \
             as the cache lives"
        );
        assert_eq!(
            stopped.error_message(),
            "invalid bearer token",
            "and it reads like any wrong token, so a revoked device learns nothing about why"
        );
        assert_eq!(
            mcp(port, Some(&second), ping()).await.status,
            200,
            "revoking one device leaves the account's other device alone"
        );
        assert_eq!(mcp(port, Some(&third), ping()).await.status, 200);

        db::revoke_account(&pool, "bob", Utc::now())
            .await
            .expect("revoking an account");
        assert_eq!(
            mcp(port, Some(&third), ping()).await.status,
            401,
            "revoking an account stops its devices on the next request too"
        );
        db::revoke_account(&pool, "ana", Utc::now())
            .await
            .expect("revoking an account");
        assert_eq!(
            mcp(port, Some(&second), ping()).await.status,
            401,
            "and the device that survived the first revocation falls with its account"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G11 · a bad credential is a failure, whichever way it is bad
// =============================================================================================

#[tokio::test]
async fn g11_every_kind_of_bad_credential_is_a_401_that_counts_toward_the_brake() {
    in_a_scratch_database("brain_g_bad_credential", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let (id, secret) = paired(&pool, "ana", None, "pc-1").await;
        let good = bearer_of(id, &secret);
        let port = start(&pool).await;

        assert_eq!(
            mcp(port, Some(&good), ping()).await.status,
            200,
            "control: the credential is good before anyone tampers with it, so the refusals \
             below are about the tampering and not about a credential that never worked"
        );
        let wrong_secret = bearer_of(id, &Secret::generate());
        let unknown_id = bearer_of(Uuid::new_v4(), &Secret::generate());
        let malformed = "mi1.xyz".to_string();
        let mut sent = 0;
        for (why, bearer, times) in [
            ("an existing id with the wrong secret", &wrong_secret, 4),
            ("an id nobody was issued", &unknown_id, 3),
            (
                "something that only starts like a credential",
                &malformed,
                3,
            ),
        ] {
            for _ in 0..times {
                let reply = mcp(port, Some(bearer), ping()).await;
                assert_eq!(reply.status, 401, "{why}: {}", reply.body);
                assert_eq!(reply.error_message(), "invalid bearer token", "{why}");
                sent += 1;
            }
        }
        assert_eq!(sent, 10, "the three kinds add up to the limit");

        let braked = mcp(port, Some(&good), ping()).await;
        assert_eq!(
            braked.status, 429,
            "ten failures of the three kinds from one address brake the eleventh request even \
             if it carries a valid credential. If one kind did not count, the total here is \
             below ten and this request is admitted: that kind is a free guess"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G12 · REQUIRE_ACCOUNTS on a loopback bind
// =============================================================================================

#[tokio::test]
async fn g12_a_loopback_daemon_ignores_the_knob_and_says_so() {
    in_a_scratch_database("brain_g_loopback_knob", |url_| async move {
        let pool = migrated(&url_).await;
        let _env = ENV_GUARD.lock().await;
        let port = serve(
            pool.clone(),
            &[("MEMORY_INDUSTRY_REQUIRE_ACCOUNTS", Some("1"))],
        )
        .await;

        let reply = mcp(port, Some(SHARED), ping()).await;
        assert_eq!(
            reply.status, 200,
            "on a loopback bind the shared token is the only way in, so a knob that closed it \
             would lock the operator out of their own daemon: {}",
            reply.body
        );
        assert!(reply.json()["result"].is_object());

        let warned = log_events().iter().any(|event| {
            event["level"] == "WARN"
                && event["fields"]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("ignored on loopback"))
        });
        assert!(
            warned,
            "a setting that is silently ignored is a setting the operator believes in: the \
             daemon has to log a WARN saying it is ignored on loopback"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G13 · the database is down
// =============================================================================================

#[tokio::test]
async fn g13_a_credential_cannot_be_checked_without_the_database_and_that_is_a_503_not_a_failure() {
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port for a database that accepts and hangs up");
    let silent_port = silent.local_addr().expect("its address").port();
    tokio::spawn(async move {
        loop {
            if let Ok((connection, _)) = silent.accept().await {
                drop(connection);
            }
        }
    });
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(500))
        .connect_lazy(&format!(
            "postgres://nobody:nothing@127.0.0.1:{silent_port}/none"
        ))
        .expect("a lazy pool never connects, so building one cannot fail");
    let port = {
        let _env = ENV_GUARD.lock().await;
        serve(pool, &[]).await
    };

    let credential = bearer_of(Uuid::new_v4(), &Secret::generate());
    let started = Instant::now();
    let reply = mcp(port, Some(&credential), ping()).await;
    assert_eq!(
        reply.status, 503,
        "a credential that cannot be checked is not a wrong credential: answering 401 would \
         send the operator to re-pair a device whose only problem is the database: {}",
        reply.body
    );
    assert!(
        reply
            .body
            .contains("cannot verify account credentials: the database is unreachable"),
        "the sentence says what failed and why: {}",
        reply.body
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "and it says so inside the pool's own budget, not after the router's deadline: {:?}",
        started.elapsed()
    );

    let burst: Vec<Reply> =
        futures::future::join_all((0..11).map(|_| mcp(port, Some(&credential), ping()))).await;
    assert!(
        burst.iter().all(|r| r.status == 503),
        "eleven more, at once, all 503: {:?}",
        burst.iter().map(|r| r.status).collect::<Vec<_>>()
    );
    let after = mcp(port, Some(&credential), ping()).await;
    assert_eq!(
        after.status, 503,
        "twelve unverifiable requests did not turn into a 429: the daemon's own outage is not \
         the client's failure and must not lock it out when the database comes back"
    );

    let shared = mcp(port, Some(SHARED), ping()).await;
    assert_eq!(
        shared.status, 200,
        "the shared token never touches the database to be admitted, so a database outage does \
         not take the legacy clients with it: {}",
        shared.body
    );
}

// =============================================================================================
// G14 · attribution
// =============================================================================================

#[tokio::test]
async fn g14_the_failure_log_and_the_log_lines_name_the_account_and_never_the_credential() {
    in_a_scratch_database("brain_g_attribution", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ina-g14").await;
        let (id, secret) = paired(&pool, "ina-g14", None, "pc-lab-1").await;
        let bearer = bearer_of(id, &secret);
        let port = start(&pool).await;

        let failing = tool_call("cuba_archivo", json!({"action": "nope"}));
        let legacy_failure = mcp(port, Some(SHARED), failing.clone()).await;
        assert_eq!(legacy_failure.status, 200, "{}", legacy_failure.body);
        let failure = mcp(port, Some(&bearer), failing).await;
        assert_eq!(failure.status, 200, "{}", failure.body);
        whoami(port, &bearer).await;

        let accounts: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT account FROM brain_handler_failures WHERE tool = 'cuba_archivo' \
             ORDER BY created_at",
        )
        .fetch_all(&pool)
        .await
        .expect("reading the failure log");
        assert_eq!(
            accounts,
            vec![None, Some("ina-g14".to_string())],
            "a tool that fails records who asked: the shared token's failure has no account, \
             the paired device's names it. The operator reading the failure log is asking \
             «which PC», and without this column the answer is an IP"
        );

        let completed = log_events().into_iter().find(|event| {
            event["fields"]["message"] == "handler completed"
                && event["fields"]["tool"] == "cuba_whoami"
                && event["fields"]["account"] == "ina-g14"
        });
        let completed = completed.expect(
            "the `handler completed` line of the whoami call has to carry the account: the log \
             is where an operator looks first, and it is shipped where the database is not",
        );
        assert_eq!(
            completed["fields"]["device"], "pc-lab-1",
            "and the device, which is what tells one of ana's PCs from another"
        );

        let log = log_text();
        for (what, hidden) in [
            ("the secret", secret.to_hex()),
            ("its hash", hex::encode(secret.hash())),
            ("the bearer", bearer.clone()),
            ("the credential prefix", "mi1.".to_string()),
        ] {
            assert!(
                !log.contains(&hidden),
                "{what} reached the log: the account is named, the credential never is"
            );
        }
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G15 · last_seen
// =============================================================================================

async fn last_seen(pool: &PgPool, id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("SELECT last_seen_at FROM brain_account_credentials WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("reading last_seen_at")
}

#[tokio::test]
async fn g15_last_seen_is_written_by_the_first_request_and_not_by_every_one() {
    in_a_scratch_database("brain_g_last_seen", |url_| async move {
        let pool = migrated(&url_).await;
        account(&pool, "ana").await;
        let (id, secret) = paired(&pool, "ana", None, "pc-1").await;
        let bearer = bearer_of(id, &secret);
        let port = start(&pool).await;
        assert_eq!(
            last_seen(&pool, id).await,
            None,
            "control: a device that never connected has never been seen"
        );

        assert_eq!(mcp(port, Some(&bearer), ping()).await.status, 200);
        let deadline = Instant::now() + Duration::from_secs(5);
        let first = loop {
            if let Some(seen) = last_seen(&pool, id).await {
                break seen;
            }
            assert!(
                Instant::now() < deadline,
                "a request admitted with the credential never set last_seen_at, so the operator \
                 cannot tell a device in use from one nobody has touched in months"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        };

        for _ in 0..5 {
            assert_eq!(mcp(port, Some(&bearer), ping()).await.status, 200);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            last_seen(&pool, id).await,
            Some(first),
            "five more requests inside the minute did not move it: authenticating is a read, \
             and writing a row per call would make every tool call a database write. The pause \
             only lets a write the daemon makes after answering land before this looks; the \
             assertion cannot be fooled the other way, because the stamp was already set"
        );
        pool.close().await;
    })
    .await;
}

// =============================================================================================
// G17 · no tool for accounts
// =============================================================================================

// The catalogue had 29 tools before this release (cuba_docs, behind its own feature, left out) and
// still has 29. Measured by listing the `tool_def(` names inside `tool_definitions` in
// `rust/src/constants.rs` at the v0.28.0 tag and at this tree: 30 names each, the same 30,
// one of them `cuba_docs`. The spec's 31 counted something else.
#[test]
fn g17_the_tool_catalogue_has_no_account_or_pairing_tool() {
    let names: Vec<String> = memory_industry::constants::tool_definitions()
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .filter(|name| *name != "cuba_docs")
        .map(str::to_string)
        .collect();

    assert_eq!(
        names.len(),
        29,
        "accounts are administered from the host's CLI and nowhere else (S-3): a tool would be \
         175 tokens in every model's context for every user, and a way for a compromised agent \
         to mint credentials. Tools: {names:?}"
    );
    let offenders: Vec<&String> = names
        .iter()
        .filter(|name| name.contains("account") || name.contains("pair"))
        .collect();
    assert!(offenders.is_empty(), "{offenders:?}");
}
