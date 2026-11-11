//! What reqwest 0.13 changed under every outbound HTTP client of the product, judged with no network.
//!
//! 0.13 asks the program for the TLS crypto provider (`rustls-no-provider`, which keeps aws-lc-rs out of the
//! binary) and checks a server's certificate with the operating system's verifier, not with a bundle compiled
//! in. The first test reads the code: a client that does not come from `http_client::builder` compiles all the
//! same and panics when it is built if no other client of that process installed the provider first, so it
//! works in one test binary and fails in another, and no test of behaviour sees that in a fixed order. The
//! other two put a TLS server on 127.0.0.1 with certificates this file makes (no key sits in the repository)
//! and look at what a client does with them: it refuses a certificate no root it trusts has signed, and it
//! accepts one that a CA of the system store has signed.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use memory_industry::cognitive::judge::{ContradictionJudge, OpenAiCompatJudge};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring::default_provider;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::server::TlsStream;

/// Each way to make a reqwest client behind the back of `http_client::builder`. `Client::builder(` is also
/// what `reqwest::Client::builder(` and `reqwest::blocking::Client::builder(` contain.
const OUTSIDE_THE_BUILDER: [&str; 4] = [
    "Client::builder(",
    "Client::new(",
    "ClientBuilder::new(",
    "reqwest::get(",
];

/// The one file that makes a client, and the one call it makes.
const THE_BUILDER: &str = "src/http_client.rs";
const THE_CALL: &str = "reqwest::Client::builder(";

/// This file holds every pattern above as text, so the scan leaves it out.
const THIS_FILE: &str = "tests/group_no_server/outbound_http_contract.rs";

/// What the server says when a handshake does finish: the shape `OpenAiCompatJudge` reads.
const ANSWER: &str = r#"{"choices":[{"message":{"content":"ok"}}]}"#;

fn rust_files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("the directory is readable")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files_under(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// What is wrong with the builder file itself: missing, or not making exactly one client builder.
fn builder_problems(path: &Path) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(path) else {
        return vec![format!(
            "{THE_BUILDER} does not exist: there is no common builder for the clients to go through"
        )];
    };
    let calls: usize = body
        .lines()
        .filter(|line| !is_comment(line))
        .map(|line| line.matches(THE_CALL).count())
        .sum();
    if calls == 1 {
        return Vec::new();
    }
    vec![format!(
        "{THE_BUILDER} has {calls} `{THE_CALL}` and has to have exactly one"
    )]
}

#[test]
fn every_outbound_http_client_is_built_by_http_client_builder() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (mut sources, mut tests) = (Vec::new(), Vec::new());
    rust_files_under(&root.join("src"), &mut sources);
    rust_files_under(&root.join("tests"), &mut tests);
    assert!(
        sources.len() >= 100 && tests.len() >= 100,
        "the scan read {} .rs files under src/ and {} under tests/, and there are about 140 and 130; a scan \
         that read almost none proves nothing",
        sources.len(),
        tests.len()
    );

    let mut problems: Vec<String> = Vec::new();
    for path in sources.iter().chain(&tests) {
        let name = path
            .strip_prefix(root)
            .expect("under rust/")
            .to_string_lossy()
            .replace('\\', "/");
        if name == THE_BUILDER || name == THIS_FILE {
            continue;
        }
        let body = std::fs::read_to_string(path).expect("a source file is readable");
        for (index, line) in body.lines().enumerate() {
            if is_comment(line) {
                continue;
            }
            if let Some(call) = OUTSIDE_THE_BUILDER
                .iter()
                .find(|call| line.contains(**call))
            {
                problems.push(format!("{name}:{}: `{call}`", index + 1));
            }
        }
    }
    problems.extend(builder_problems(&root.join(THE_BUILDER)));

    assert!(
        problems.is_empty(),
        "reqwest 0.13 with `rustls-no-provider` does not pick a crypto provider. `http_client::builder` installs \
         ring once; a client made anywhere else compiles and panics when it is built if no other client of the \
         process installed one first, which depends on the order the tests of a binary run in. Every client \
         is made by `http_client::builder()`: {problems:#?}"
    );
}

/// A TLS server on 127.0.0.1 that counts what happens to it. `handshakes` counts the connections whose
/// handshake the client finished, which a client that checks certificates never does with one it does not trust.
struct TlsServer {
    port: u16,
    accepted: Arc<AtomicUsize>,
    handshakes: Arc<AtomicUsize>,
}

impl TlsServer {
    async fn start(cert: CertificateDer<'static>, key: PrivatePkcs8KeyDer<'static>) -> Self {
        // The server names its own provider: what it speaks must not depend on the one the process installed, or did not.
        let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring speaks the safe protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert], PrivateKeyDer::Pkcs8(key))
            .expect("a certificate and the key it was made with");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("a free loopback port");
        let port = listener.local_addr().expect("the bound address").port();

        let accepted = Arc::new(AtomicUsize::new(0));
        let handshakes = Arc::new(AtomicUsize::new(0));
        let (seen_accepted, seen_handshakes) = (accepted.clone(), handshakes.clone());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                seen_accepted.fetch_add(1, Ordering::SeqCst);
                let (acceptor, handshakes) = (acceptor.clone(), seen_handshakes.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    handshakes.fetch_add(1, Ordering::SeqCst);
                    answer(tls).await;
                });
            }
        });
        TlsServer {
            port,
            accepted,
            handshakes,
        }
    }

    fn base_url(&self) -> String {
        format!("https://127.0.0.1:{}/v1", self.port)
    }
}

/// Reads one request, head and body, and answers it 200 with `ANSWER`: a client that did accept the certificate
/// gets an `Ok` out of it, instead of a connection that hangs.
async fn answer(mut stream: TlsStream<tokio::net::TcpStream>) {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let Ok(read) = stream.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(at) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
    let body_length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while request.len() < head_end + body_length {
        let Ok(read) = stream.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{ANSWER}",
        ANSWER.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// A certificate for 127.0.0.1 that signed itself: no root anywhere holds it.
fn self_signed_for_loopback() -> (CertificateDer<'static>, PrivatePkcs8KeyDer<'static>) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
            .expect("a self-signed certificate for 127.0.0.1");
    (
        cert.der().clone(),
        PrivatePkcs8KeyDer::from(signing_key.serialize_der()),
    )
}

#[tokio::test]
async fn an_outbound_client_refuses_a_certificate_that_no_root_it_trusts_has_signed() {
    let (cert, key) = self_signed_for_loopback();
    let server = TlsServer::start(cert, key).await;
    let judge = OpenAiCompatJudge {
        base_url: server.base_url(),
        model: "none".to_string(),
        api_key: None,
        timeout: Duration::from_secs(10),
        provider: "custom".to_string(),
    };

    let outcome = judge.run_prompt("ping").await;

    // The server counts before the client can have a certificate to refuse: it accepts the connection, then sends it.
    assert!(
        server.accepted.load(Ordering::SeqCst) >= 1,
        "the request never reached the server, so nothing was judged: a proxy in the environment, or a client \
         that did not build (the system store holds no CA?): {outcome:?}"
    );
    assert_eq!(
        server.handshakes.load(Ordering::SeqCst),
        0,
        "a client finished a TLS handshake with a certificate that no root signed: its verification is off or \
         it trusts what it should not. `run_prompt` answered {outcome:?}"
    );
    let err = outcome.expect_err("a certificate nobody trusts must not give an answer");
    let chain = format!("{err:#}");
    assert!(
        chain.to_ascii_lowercase().contains("certificate"),
        "the error should name the certificate that was refused, and says: {chain}"
    );
}

/// `SSL_CERT_FILE` is how rustls-native-certs is told to read a store of the test's own, and only unix that is not
/// macOS reads it: Windows and macOS ask the operating system, which a test cannot be given a CA for.
#[cfg(all(unix, not(target_os = "macos")))]
mod the_system_store {
    use std::process::Stdio;

    use uuid::Uuid;

    use super::*;

    const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

    /// How long the child may take. A child that has nothing to load answers in well under a second; the bound
    /// makes a hung one fail this test and not the run.
    const CHILD_WAIT: Duration = Duration::from_secs(60);

    /// What `memory-industry llm status` would read from the environment to talk to another endpoint, and what
    /// could send the probe through a proxy: none of it may reach the child from the machine that runs the test.
    const CLEARED: [&str; 16] = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "SSL_CERT_DIR",
        "DATABASE_URL",
        "OPENAI_API_KEY",
        "CUBA_LLM_BASE_URL",
        "CUBA_LLM_PROVIDER",
        "CUBA_LLM_API_KEY",
        "MEMORY_INDUSTRY_LLM_PROVIDER",
        "MEMORY_INDUSTRY_LLM_API_KEY",
        "MEMORY_INDUSTRY_LLM_CLI",
        "MEMORY_INDUSTRY_JUDGE",
    ];

    /// A directory of this test's own, removed when the test ends, also when it panics.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "memory-industry-outbound-tls-{}",
                Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&dir).expect("a scratch directory under the temp dir");
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A CA of this test's own, as PEM (the file a system store holds), and a certificate for 127.0.0.1 that it signed.
    fn leaf_signed_by_a_ca() -> (String, CertificateDer<'static>, PrivatePkcs8KeyDer<'static>) {
        let mut ca_params =
            rcgen::CertificateParams::new(Vec::<String>::new()).expect("parameters for a CA");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "memory-industry test CA");
        let ca_key = rcgen::KeyPair::generate().expect("the CA's key");
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .expect("the CA's certificate");
        let issuer = rcgen::Issuer::new(ca_params, ca_key);

        let leaf_key = rcgen::KeyPair::generate().expect("the server's key");
        let leaf = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()])
            .expect("parameters for 127.0.0.1")
            .signed_by(&leaf_key, &issuer)
            .expect("a certificate for 127.0.0.1 signed by the CA");
        (
            ca_cert.pem(),
            leaf.der().clone(),
            PrivatePkcs8KeyDer::from(leaf_key.serialize_der()),
        )
    }

    /// Whether the client trusts what the system store holds is something only a real store can say, and the real
    /// binary is the one that has to say it: `llm status` probes the endpoint it is pointed at with the client of
    /// `probe_health`. The child gets a store of its own (`SSL_CERT_FILE`, set on the child and not on this
    /// process) with only this test's CA in it, so the answer does not depend on what the machine has installed.
    /// The CA is nobody's, so a client that trusts a bundle compiled in, and not the store, refuses the server.
    #[tokio::test]
    async fn an_outbound_client_accepts_a_certificate_signed_by_a_root_of_the_system_store() {
        let (ca_pem, cert, key) = leaf_signed_by_a_ca();
        let server = TlsServer::start(cert, key).await;
        let scratch = Scratch::new();
        let store = scratch.0.join("ca.pem");
        std::fs::write(&store, ca_pem).expect("the store file is written");

        let mut command = tokio::process::Command::new(BIN);
        command
            .args(["llm", "status"])
            .env("HOME", scratch.0.as_path())
            .env("SSL_CERT_FILE", store.as_path())
            .env("MEMORY_INDUSTRY_LLM_BASE_URL", server.base_url())
            .env("MEMORY_INDUSTRY_REQUIRE_DATABASE_URL", "1")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        for name in CLEARED {
            command.env_remove(name);
        }
        let output = tokio::time::timeout(CHILD_WAIT, command.output())
            .await
            .expect("`llm status` ends")
            .expect("`llm status` starts");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let probe = stdout
            .lines()
            .find(|line| line.starts_with("probe:"))
            .unwrap_or("(no `probe:` line)");
        assert!(
            probe.contains(" OK"),
            "the client did not accept a certificate signed by the CA that its store holds. {probe}\n\
             --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
        );
        assert!(
            server.handshakes.load(Ordering::SeqCst) >= 1,
            "the probe says OK and the server finished no handshake: {probe}"
        );
    }
}
