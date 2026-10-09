//! `bridge pair <code>`: the client generates the credential, proves it knows the code,
//! and checks the host knows it too before keeping anything. Touches no disk: the caller
//! stores the returned state.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use chrono::Utc;

use super::discovery::{probe_service, sweep_services};
use super::state::{BridgeState, STATE_VERSION};
use super::{DEFAULT_BRIDGE_PORT, DEFAULT_HOST_PORT, Network, Timing};
use crate::accounts::{self, CodeError, PairRequest, PairResponse, Secret};

/// A pairing answer is a short JSON object; anything bigger is not one.
const MAX_PAIR_BYTES: usize = 16 * 1024;

/// What to wait when the host brakes without saying for how long.
const DEFAULT_RETRY_AFTER_SECS: u64 = 60;

/// `--host madre[:9000]` or `--host 192.168.0.10[:9000]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    pub host: String,
    pub port: u16,
}

#[derive(Debug)]
pub enum PairError {
    BadCode(CodeError),
    BadLabel(String),
    BadHost(String),
    NotFound(String),
    /// Something answered the offer but could not prove it knows the code.
    Impostor,
    /// The host answered 401: mistyped, expired, used or revoked, which it does not say.
    Refused,
    /// 429, with the seconds the host asked to wait.
    Braked(u64),
    Transport(String),
}

impl PairError {
    /// Which of two failures from different candidates says more. A refusal by a real
    /// host beats an impostor, which beats a network error.
    fn weight(&self) -> u8 {
        match self {
            PairError::Refused => 3,
            PairError::Impostor => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for PairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PairError::BadCode(why) => write!(f, "código mal escrito: {why}"),
            PairError::BadLabel(why) | PairError::BadHost(why) | PairError::Transport(why) => {
                f.write_str(why)
            }
            PairError::NotFound(tried) => write!(f, "no se encontró la madre: {tried}"),
            PairError::Impostor => f.write_str(
                "el equipo que contestó no conoce el código: no es la madre. \
                 No se guardó nada",
            ),
            PairError::Refused => f.write_str(
                "la madre no aceptó el código (puede estar mal escrito, caducado o ya usado)",
            ),
            PairError::Braked(secs) => write!(
                f,
                "la madre pide esperar: espera {secs} s antes de volver a probar"
            ),
        }
    }
}

impl std::error::Error for PairError {}

fn is_hostname(text: &str) -> bool {
    !text.is_empty() && !text.contains(|c: char| c.is_whitespace() || c == '/')
}

pub fn parse_host(raw: &str) -> Result<HostSpec, PairError> {
    let bad = || {
        PairError::BadHost(format!(
            "--host {raw:?} no es válido: usa nombre, nombre:puerto, IP o IP:puerto"
        ))
    };
    let mut parts = raw.trim().split(':');
    let host = parts.next().filter(|h| is_hostname(h)).ok_or_else(bad)?;
    let port = match parts.next() {
        None => DEFAULT_HOST_PORT,
        Some(text) => text
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(bad)?,
    };
    if parts.next().is_some() {
        return Err(bad());
    }
    Ok(HostSpec {
        host: host.to_string(),
        port,
    })
}

/// A literal address as it is, a name through the operating system's resolver.
async fn addresses_of(host: &str, net: &dyn Network, t: &Timing) -> Vec<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![ip];
    }
    let asked = tokio::time::timeout(t.resolve, net.resolve(host)).await;
    asked.unwrap_or_default()
}

fn listed(addrs: &[SocketAddr]) -> String {
    let texts: Vec<String> = addrs.iter().map(SocketAddr::to_string).collect();
    texts.join(", ")
}

/// The addresses named by `--host` that speak MemoryIndustry.
async fn named_hosts(
    raw: &str,
    net: &dyn Network,
    t: &Timing,
    c: &reqwest::Client,
) -> Result<Vec<SocketAddr>, PairError> {
    let spec = parse_host(raw)?;
    let ips = addresses_of(&spec.host, net, t).await;
    if ips.is_empty() {
        return Err(PairError::NotFound(format!("{} no se resolvió", spec.host)));
    }
    let addrs: Vec<SocketAddr> = ips
        .iter()
        .map(|ip| SocketAddr::new(*ip, spec.port))
        .collect();
    let mut serving = Vec::new();
    for addr in &addrs {
        if probe_service(c, *addr, t.identity).await.is_ok() {
            serving.push(*addr);
        }
    }
    if serving.is_empty() {
        let tried = listed(&addrs);
        return Err(PairError::NotFound(format!(
            "nada habló como MemoryIndustry en {tried}"
        )));
    }
    Ok(serving)
}

/// Without `--host`: the own /24, and only what answers like MemoryIndustry.
async fn swept_hosts(
    net: &dyn Network,
    t: &Timing,
    c: &reqwest::Client,
) -> Result<Vec<SocketAddr>, PairError> {
    let targets = net.sweep_targets(DEFAULT_HOST_PORT);
    if targets.is_empty() {
        return Err(PairError::NotFound(
            "no pude averiguar la red propia; usa --host <nombre|ip>".to_string(),
        ));
    }
    let serving = sweep_services(c, t, &targets).await;
    if serving.is_empty() {
        return Err(PairError::NotFound(format!(
            "ninguna MemoryIndustry contestó en {} direcciones de la red propia; usa --host <nombre|ip>",
            targets.len()
        )));
    }
    Ok(serving)
}

/// Where the host answered: the `POST /pair` and what it said. A 401 and a 429 are
/// answers; so is anything else that is not a 2xx.
async fn exchange(
    c: &reqwest::Client,
    addr: SocketAddr,
    request: &PairRequest,
    t: &Timing,
) -> Result<PairResponse, PairError> {
    let sent = c
        .post(format!("http://{addr}/pair"))
        .json(request)
        .timeout(t.request_waits)
        .send()
        .await;
    let response = sent.map_err(|_| PairError::Transport(format!("{addr} no contestó")))?;
    let status = response.status();
    if status.as_u16() == 429 {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        return Err(PairError::Braked(
            seconds.unwrap_or(DEFAULT_RETRY_AFTER_SECS),
        ));
    }
    if status.as_u16() == 401 {
        return Err(PairError::Refused);
    }
    if !status.is_success() {
        return Err(PairError::Transport(format!("{addr} respondió {status}")));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| PairError::Transport(format!("{addr} cortó la respuesta")))?;
    serde_json::from_slice(&bytes)
        .ok()
        .filter(|_| bytes.len() <= MAX_PAIR_BYTES)
        .ok_or(PairError::Impostor)
}

/// The host's MAC over everything it assigned, keyed by the code: only whoever knows the
/// code, which the host does, can make it.
fn host_knows_the_code(reply: &PairResponse, key: &[u8; 32], nc: &[u8; 16], h: &[u8; 32]) -> bool {
    let expected = accounts::pair_response_mac(
        key,
        nc,
        h,
        &reply.host_id,
        &reply.credential_id,
        &reply.account,
        &reply.label,
        &reply.host_name,
    );
    reply.v == 1 && accounts::verify_mac(expected, &reply.mac)
}

/// One offer of the code to one address. The secret never leaves: only its hash does.
async fn offer(
    addr: SocketAddr,
    key: &[u8; 32],
    label: &str,
    t: &Timing,
    c: &reqwest::Client,
) -> Result<BridgeState, PairError> {
    let secret = Secret::generate();
    let nc = accounts::random_bytes::<16>();
    let h = secret.hash();
    let request = PairRequest {
        v: 1,
        nc: hex::encode(nc),
        label: label.to_string(),
        h: hex::encode(h),
        mac: hex::encode(accounts::pair_request_mac(key, &nc, label, &h)),
    };
    let reply = exchange(c, addr, &request, t).await?;
    if !host_knows_the_code(&reply, key, &nc, &h) {
        return Err(PairError::Impostor);
    }
    Ok(BridgeState {
        version: STATE_VERSION,
        host_id: reply.host_id,
        host_name: reply.host_name,
        account: reply.account,
        device_label: reply.label,
        credential_id: reply.credential_id,
        secret,
        addresses: vec![addr],
        last_good: Some(addr),
        host_port: addr.port(),
        port: DEFAULT_BRIDGE_PORT,
        paired_at: Utc::now(),
    })
}

/// Offers the code to each address in turn. A brake ends it (every further try spends the
/// host's budget); anything else moves on, and the most telling failure is the one kept.
async fn offer_to_each(
    addrs: &[SocketAddr],
    key: &[u8; 32],
    label: &str,
    t: &Timing,
    c: &reqwest::Client,
) -> Result<BridgeState, PairError> {
    let mut worst: Option<PairError> = None;
    for addr in addrs {
        match offer(*addr, key, label, t, c).await {
            Ok(state) => return Ok(state),
            Err(braked @ PairError::Braked(_)) => return Err(braked),
            Err(err) if worst.as_ref().is_none_or(|w| err.weight() > w.weight()) => {
                worst = Some(err);
            }
            Err(_) => {}
        }
    }
    Err(worst.unwrap_or_else(|| PairError::NotFound(listed(addrs))))
}

/// Pairs this device with the host that knows `code`. The code is checked before any
/// connection; the credential is generated here and only its hash is sent; nothing is
/// kept unless the host proves it knew the code too.
pub async fn pair(
    code: &str,
    host: Option<&str>,
    label: &str,
    net: &dyn Network,
    t: &Timing,
    c: &reqwest::Client,
) -> Result<BridgeState, PairError> {
    let normalized = accounts::normalize_code(code).map_err(PairError::BadCode)?;
    let label = accounts::validate_device_label(label)
        .map_err(|e| PairError::BadLabel(format!("{e:#}")))?;
    let key = accounts::code_key(&normalized);
    let hosts = match host {
        Some(raw) => named_hosts(raw, net, t, c).await?,
        None => swept_hosts(net, t, c).await?,
    };
    offer_to_each(&hosts, &key, &label, t, c).await
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Instant;

    use super::super::discovery::{Verdict, check_identity};
    use super::super::fake_host::{
        Closer, FakeHost, FakeNet, PairMode, RawRecorder, Tarpit, client, contains_bytes,
        contains_ci, hex_runs_of_64, ms, tiny_timing,
    };
    use super::*;
    use crate::accounts;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const REFUSED: &str =
        "la madre no aceptó el código (puede estar mal escrito, caducado o ya usado)";

    fn host_arg(host: &FakeHost) -> String {
        format!("127.0.0.1:{}", host.addr().port())
    }

    async fn host_with_code() -> (FakeHost, String) {
        let host = FakeHost::start(uuid::Uuid::new_v4(), vec![]).await;
        let code = accounts::generate_code();
        host.add_code(&code, "ana", Some("pc-lab-1"));
        (host, code)
    }

    /// H6
    #[tokio::test]
    async fn pairing_with_the_host_keeps_what_the_host_assigned_and_sends_no_secret() {
        let (host, code) = host_with_code().await;

        let state = pair(
            &code,
            Some(&host_arg(&host)),
            "mi-pc",
            &FakeNet::new(),
            &tiny_timing(),
            &client(),
        )
        .await
        .expect("a valid code pairs");

        let issued = host.issued();
        assert_eq!(issued.len(), 1, "one code, one credential");
        assert_eq!(
            state.host_id,
            host.host_id(),
            "the state pins the host that answered"
        );
        assert_eq!(state.host_name, host.name());
        assert_eq!(state.account, "ana");
        assert_eq!(
            state.device_label, "pc-lab-1",
            "the label the host assigned wins over the client's 'mi-pc': the operator chose it \
             when issuing the code, and it is what `account revoke-device` will be asked for"
        );
        assert_eq!(
            state.credential_id, issued[0].credential_id,
            "the credential id is the one the host minted (it is in the bearer the host looks up)"
        );
        assert_eq!(
            state.addresses,
            vec![host.addr()],
            "the address that worked is remembered"
        );
        assert_eq!(state.host_port, host.addr().port());
        assert_eq!(state.port, 8797, "the bridge's own port defaults to 8797");

        let pair_requests = host.pair_requests();
        assert_eq!(pair_requests.len(), 1);
        let sent: accounts::PairRequest = serde_json::from_slice(&pair_requests[0].body)
            .expect("the /pair body is a PairRequest");
        assert_eq!(
            sent.label, "mi-pc",
            "the client offers its own label; the host decides"
        );
        assert_eq!(
            sent.h,
            hex::encode(state.secret.hash()),
            "the host receives SHA-256(secret), which is all it needs to recognise the bearer \
             later"
        );
        let nc: [u8; 16] = hex::decode(&sent.nc)
            .expect("nc is hex")
            .try_into()
            .expect("16 bytes");
        let h: [u8; 32] = hex::decode(&sent.h)
            .expect("h is hex")
            .try_into()
            .expect("32 bytes");
        let key = accounts::code_key(&accounts::normalize_code(&code).expect("a valid code"));
        assert_eq!(
            sent.mac,
            hex::encode(accounts::pair_request_mac(&key, &nc, &sent.label, &h)),
            "the request is authenticated with the key derived from the code"
        );

        let wire = host.received_bytes();
        let secret_hex = state.secret.to_hex();
        let secret_raw = hex::decode(&secret_hex).expect("the secret is hex");
        assert!(
            !contains_ci(&wire, &secret_hex) && !contains_bytes(&wire, &secret_raw),
            "the secret must never travel: the host holds only its hash"
        );
        let normalized = accounts::normalize_code(&code).expect("a valid code");
        assert!(
            !contains_ci(&wire, &normalized),
            "the code itself is not sent either, only a MAC keyed by it"
        );
    }

    /// H7
    #[tokio::test]
    async fn an_impostor_that_does_not_know_the_code_is_refused_and_learns_nothing() {
        let code = accounts::generate_code();
        let pretender = uuid::Uuid::new_v4();
        let response = serde_json::json!({
            "v": 1, "host_id": pretender, "host_name": "madre",
            "credential_id": uuid::Uuid::new_v4(), "account": "ana",
            "label": "pc-lab-1", "mac": "ab".repeat(32),
        })
        .to_string();
        let identity = serde_json::json!({
            "service": "memory-industry", "host_id": pretender, "name": "madre"
        })
        .to_string();
        let impostor =
            RawRecorder::start(&[("GET /identity", identity), ("POST /pair", response)]).await;

        let err = match pair(
            &code,
            Some(&format!("127.0.0.1:{}", impostor.addr().port())),
            "mi-pc",
            &FakeNet::new(),
            &tiny_timing(),
            &client(),
        )
        .await
        {
            Ok(state) => panic!("a response with an invented MAC was accepted: {state:?}"),
            Err(err) => err,
        };

        assert!(
            err.to_string()
                .contains("el equipo que contestó no conoce el código"),
            "the operator must learn that whoever answered did not know the code (a typo would \
             have been refused with 401, not answered with a well-formed 200). Got: {err}"
        );
        let runs = hex_runs_of_64(&impostor.received());
        assert_eq!(
            runs.len(),
            2,
            "the only 64-hex values the impostor read are `h` and the request MAC; a third \
             would be the secret. Saw: {runs:?}"
        );
    }

    /// H8
    #[tokio::test]
    async fn a_mistyped_code_is_caught_before_any_connection() {
        let (host, _) = host_with_code().await;

        let err = pair(
            "K7QM-3WXP-9HN0",
            Some(&host_arg(&host)),
            "mi-pc",
            &FakeNet::new(),
            &tiny_timing(),
            &client(),
        )
        .await
        .expect_err("0 is not in the alphabet");

        assert!(
            err.to_string().contains("0"),
            "the message names the offending character. Got: {err}"
        );
        assert_eq!(
            host.accepted(),
            0,
            "validating after connecting would send a typo to the network and spend a failure \
             in the host's brake"
        );
        assert!(host.seen().is_empty(), "the host saw no request at all");
    }

    /// H9
    #[tokio::test]
    async fn a_refusal_says_the_same_thing_whatever_the_host_gave_as_reason() {
        let bodies = [
            r#"{"error":"pairing failed"}"#,
            r#"{"error":"expired"}"#,
            r#"{"error":"used"}"#,
            r#"{"error":"account revoked"}"#,
            "",
        ];
        let mut messages = Vec::new();
        for body in bodies {
            let (host, code) = host_with_code().await;
            host.set_pair(PairMode::Refuse(body.to_string()));

            let err = pair(
                &code,
                Some(&host_arg(&host)),
                "mi-pc",
                &FakeNet::new(),
                &tiny_timing(),
                &client(),
            )
            .await
            .expect_err("a 401 is a refusal");

            assert!(
                err.to_string().contains(REFUSED),
                "body {body:?}: the client cannot tell the four real reasons apart and must \
                 not pretend to. Got: {err}"
            );
            messages.push(err.to_string());
        }
        messages.dedup();
        assert_eq!(
            messages.len(),
            1,
            "one message for every body: {messages:?}"
        );
    }

    /// H10
    #[tokio::test]
    async fn a_braked_host_stops_the_pairing_and_the_other_candidates_are_left_alone() {
        let (braking, code) = host_with_code().await;
        braking.set_pair(PairMode::Brake(30));
        let (next, _) = host_with_code().await;
        next.add_code(&code, "ana", Some("pc-lab-1"));
        let net = FakeNet::new()
            .own(Ipv4Addr::new(10, 0, 5, 7))
            .sweep(vec![braking.addr(), next.addr()]);

        let err = pair(&code, None, "mi-pc", &net, &tiny_timing(), &client())
            .await
            .expect_err("the first host asked to wait");

        assert!(
            err.to_string().contains("espera 30 s"),
            "the message carries the host's Retry-After. Got: {err}"
        );
        assert!(
            next.pair_requests().is_empty(),
            "after a 429 the code is not offered to anyone else: every extra try spends the \
             brake's budget and makes the wait longer"
        );
    }

    /// H11
    #[tokio::test]
    async fn without_a_host_the_sweep_offers_the_code_only_to_memory_industry() {
        let (host, code) = host_with_code().await;
        let anything = RawRecorder::start(&[("", r#"{"hola":1}"#.to_string())]).await;
        let closer = Closer::start().await;
        let tarpit = Tarpit::start().await;
        let net = FakeNet::new().own(Ipv4Addr::new(10, 0, 5, 7)).sweep(vec![
            anything.addr(),
            closer.addr(),
            tarpit.addr(),
            host.addr(),
        ]);
        let timing = tiny_timing();

        let started = Instant::now();
        let state = pair(&code, None, "mi-pc", &net, &timing, &client())
            .await
            .expect("the one MemoryIndustry in the sweep takes the code");
        let took = started.elapsed();

        assert_eq!(state.host_id, host.host_id());
        assert_eq!(host.pair_requests().len(), 1);
        let lines = anything.lines();
        assert!(
            lines.iter().all(|line| line.starts_with("GET /identity")),
            "a server that is not MemoryIndustry may only be asked who it is, never offered \
             the code. Saw: {lines:?}"
        );
        assert!(
            took < timing.probe_total + ms(500),
            "a silent candidate in the sweep must not stall the pairing past probe_total; took {took:?}"
        );
    }

    /// H12
    #[test]
    fn the_forms_of_host_are_understood_and_the_malformed_ones_refused() {
        let accepted = [
            ("madre", "madre", 8787),
            ("madre:9000", "madre", 9000),
            ("192.168.0.10", "192.168.0.10", 8787),
            ("192.168.0.10:9000", "192.168.0.10", 9000),
        ];
        for (raw, host, port) in accepted {
            assert_eq!(
                parse_host(raw).expect(raw),
                HostSpec {
                    host: host.to_string(),
                    port
                },
                "{raw}: the default port is the daemon's 8787"
            );
        }
        for raw in [
            "",
            "a:b:c",
            "madre:0",
            "madre:70000",
            "madre:",
            "madre:abc",
            ":9000",
        ] {
            assert!(
                parse_host(raw).is_err(),
                "{raw:?} is not a host the bridge can dial (port 0 and 70000 are not ports)"
            );
        }
    }

    /// H12
    #[tokio::test]
    async fn a_host_name_is_resolved_by_the_operating_system_and_never_guessed() {
        let (host, code) = host_with_code().await;
        let net = FakeNet::new().resolving("madre", &[LOOPBACK]);

        let state = pair(
            &code,
            Some(&format!("madre:{}", host.addr().port())),
            "mi-pc",
            &net,
            &tiny_timing(),
            &client(),
        )
        .await
        .expect("the name resolves to the fake host");

        assert_eq!(
            net.asked(),
            vec!["madre".to_string()],
            "the OS resolver is asked for the name given"
        );
        assert_eq!(state.addresses, vec![host.addr()]);
    }

    #[tokio::test]
    async fn a_host_that_does_not_answer_is_named_in_the_error() {
        let (_, code) = host_with_code().await;
        let closer = Closer::start().await;

        let err = pair(
            &code,
            Some(&format!("127.0.0.1:{}", closer.addr().port())),
            "mi-pc",
            &FakeNet::new(),
            &tiny_timing(),
            &client(),
        )
        .await
        .expect_err("nothing there speaks MemoryIndustry");

        assert!(
            err.to_string().contains(&closer.addr().to_string()),
            "the operator needs to see which address was tried. Got: {err}"
        );
    }

    /// Two PCs typing the same code at the same moment.
    #[tokio::test]
    async fn two_bridges_pairing_the_same_code_leave_exactly_one_credential() {
        let (host, code) = host_with_code().await;
        let timing = tiny_timing();
        let (one_client, other_client) = (client(), client());
        let net = FakeNet::new();
        let target = host_arg(&host);

        let (first, second) = tokio::join!(
            pair(&code, Some(&target), "pc-a", &net, &timing, &one_client),
            pair(&code, Some(&target), "pc-b", &net, &timing, &other_client),
        );

        let results = [first, second];
        let winners: Vec<&BridgeState> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(
            winners.len(),
            1,
            "a code is single use: of two devices racing for it exactly one pairs. Got {} \
             winners",
            winners.len()
        );
        let loser = results
            .iter()
            .find_map(|r| r.as_ref().err())
            .expect("one loser");
        assert!(
            loser.to_string().contains(REFUSED),
            "the loser is told the code was not accepted. Got: {loser}"
        );
        assert_eq!(
            host.issued().len(),
            1,
            "the host minted one credential, not two"
        );

        let winner = winners[0];
        let verdict = check_identity(
            &client(),
            host.addr(),
            winner,
            [3u8; 16],
            tiny_timing().identity,
        )
        .await;
        assert!(
            matches!(verdict, Verdict::Verified { .. }),
            "the winner's secret is the one the host stored, so its proof verifies: {verdict:?}"
        );
    }

    /// Answers the offer with a MAC it cannot have computed: an impostor, not the host.
    async fn impostor_host() -> RawRecorder {
        let response = serde_json::json!({
            "v": 1, "host_id": uuid::Uuid::new_v4(), "host_name": "madre",
            "credential_id": uuid::Uuid::new_v4(), "account": "ana",
            "label": "pc-lab-1", "mac": "ab".repeat(32),
        })
        .to_string();
        RawRecorder::start(&[("POST /pair", response)]).await
    }

    /// The body of the one request a fake host was sent, read by its Content-Length.
    async fn request_body(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream
                .read_exact(&mut byte)
                .await
                .expect("the request head");
            head.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&head).to_ascii_lowercase();
        let length = text
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        stream
            .read_exact(&mut body)
            .await
            .expect("the request body");
        body
    }

    /// A host that answers the offer with a MAC it computes from the request, as a real one
    /// does, and pads the answer with an unknown field up to `total` bytes. The client must
    /// read it: a real answer is a few hundred bytes, and the cap is 16 KiB.
    async fn padded_host(code: &str, total: usize) -> SocketAddr {
        let key = accounts::code_key(&accounts::normalize_code(code).expect("a valid code"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port for the padded host");
        let addr = listener.local_addr().expect("the bound address");
        let host_id = uuid::Uuid::new_v4();
        let credential_id = uuid::Uuid::new_v4();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let sent: accounts::PairRequest =
                serde_json::from_slice(&request_body(&mut stream).await).expect("a PairRequest");
            let nc: [u8; 16] = hex::decode(&sent.nc)
                .expect("nc is hex")
                .try_into()
                .expect("16 bytes");
            let h: [u8; 32] = hex::decode(&sent.h)
                .expect("h is hex")
                .try_into()
                .expect("32 bytes");
            let mac = accounts::pair_response_mac(
                &key,
                &nc,
                &h,
                &host_id,
                &credential_id,
                "ana",
                "pc-lab-1",
                "madre",
            );
            let mut doc = serde_json::json!({
                "v": 1, "host_id": host_id, "host_name": "madre",
                "credential_id": credential_id, "account": "ana", "label": "pc-lab-1",
                "mac": hex::encode(mac), "pad": "",
            });
            let bare = doc.to_string().len();
            doc["pad"] = serde_json::json!("x".repeat(total.saturating_sub(bare)));
            let answer = doc.to_string();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                answer.len()
            );
            let _ = stream.write_all(format!("{head}{answer}").as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        addr
    }

    /// The weight is what decides which of several failures is the one reported.
    #[test]
    fn a_refusal_outweighs_an_impostor_which_outweighs_a_network_error() {
        assert_eq!(PairError::Refused.weight(), 3);
        assert_eq!(PairError::Impostor.weight(), 2);
        assert_eq!(PairError::Transport(String::new()).weight(), 1);
        assert_eq!(PairError::NotFound(String::new()).weight(), 1);
    }

    #[test]
    fn a_host_name_is_one_word_without_a_slash_and_never_empty() {
        for name in ["madre", "madre.local", "mi-pc", "1madre", "192.168.0.10"] {
            assert!(
                is_hostname(name),
                "{name:?} is a host name the resolver can take"
            );
        }
        for text in ["", "mi pc", "a/b", "madre\tlocal"] {
            assert!(!is_hostname(text), "{text:?} is not a host name");
        }
    }

    /// Of several candidates that all failed, the most telling failure is the one reported: a
    /// refusal beats an impostor, which beats a network error. Between two of the same weight
    /// the first one seen is kept.
    #[tokio::test]
    async fn the_most_telling_failure_of_the_candidates_is_the_one_reported() {
        let key = [9u8; 32];
        let timing = tiny_timing();
        let (refusing, _) = host_with_code().await;
        refusing.set_pair(PairMode::Refuse(String::new()));
        let impostor = impostor_host().await;
        let silent = Closer::start().await;
        let other_silent = Closer::start().await;
        let (refused_at, impostor_at) = (refusing.addr(), impostor.addr());
        let (silent_at, other_at) = (silent.addr(), other_silent.addr());

        let err = offer_to_each(&[refused_at, silent_at], &key, "mi-pc", &timing, &client())
            .await
            .expect_err("nobody paired");
        assert_eq!(
            err.to_string(),
            REFUSED,
            "a refusal beats a network error, though the network error came first"
        );

        let err = offer_to_each(
            &[impostor_at, refused_at],
            &key,
            "mi-pc",
            &timing,
            &client(),
        )
        .await
        .expect_err("nobody paired");
        assert_eq!(
            err.to_string(),
            REFUSED,
            "a refusal beats an impostor that came first"
        );

        let err = offer_to_each(&[silent_at, impostor_at], &key, "mi-pc", &timing, &client())
            .await
            .expect_err("nobody paired");
        assert!(
            matches!(err, PairError::Impostor),
            "an impostor beats a network error that came first. Got: {err}"
        );

        let err = offer_to_each(&[silent_at, other_at], &key, "mi-pc", &timing, &client())
            .await
            .expect_err("nobody paired");
        assert_eq!(
            err.to_string(),
            format!("{silent_at} no contestó"),
            "two network errors of the same weight: the first one seen is the one kept"
        );
    }

    /// The cap is 16 KiB. A 2 KiB answer with the host's own MAC is read and the code is
    /// paired; one of 17 KiB is refused for its size, whatever its MAC says.
    #[tokio::test]
    async fn an_answer_of_a_few_kilobytes_is_read_and_one_past_the_cap_is_refused() {
        let code = accounts::generate_code();
        let key = accounts::code_key(&accounts::normalize_code(&code).expect("a valid code"));

        let within = padded_host(&code, 2048).await;
        let state = offer(within, &key, "mi-pc", &tiny_timing(), &client())
            .await
            .expect("a 2 KiB answer with the right MAC is inside the 16 KiB limit");
        assert_eq!(state.device_label, "pc-lab-1");

        let beyond = padded_host(&code, 17 * 1024).await;
        let err = offer(beyond, &key, "mi-pc", &tiny_timing(), &client())
            .await
            .expect_err("17 KiB is past the limit, whatever its MAC says");
        assert!(
            matches!(err, PairError::Impostor),
            "an answer past the limit is refused as an impostor's. Got: {err}"
        );
    }
}
