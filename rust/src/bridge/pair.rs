//! `bridge pair <code>`: the client generates the credential, proves it knows the code,
//! and checks the host knows it too before keeping anything. Touches no disk: the caller
//! stores the returned state.

use std::fmt;

use super::state::BridgeState;
use super::{Network, Timing};
use crate::accounts::CodeError;

/// `--host madre[:9000]` or `--host 192.168.0.10[:9000]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    pub host: String,
    pub port: u16,
}

#[derive(Debug)]
pub enum PairError {
    BadCode(CodeError),
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

impl fmt::Display for PairError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // PASS-1 STUB
        f.write_str("PASS-1 STUB: PairError")
    }
}

impl std::error::Error for PairError {}

pub fn parse_host(_raw: &str) -> Result<HostSpec, PairError> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: pair::parse_host")
}

pub async fn pair(
    _code: &str,
    _host: Option<&str>,
    _label: &str,
    _net: &dyn Network,
    _t: &Timing,
    _c: &reqwest::Client,
) -> Result<BridgeState, PairError> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: pair::pair")
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
            err.to_string().contains('0'),
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
}
