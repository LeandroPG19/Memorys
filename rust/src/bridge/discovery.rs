//! Finding the host again, and refusing to hand a credential to anyone who cannot prove
//! it is the host. Order, bounds and verdicts are decided here and nowhere else.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::state::BridgeState;
use super::{Network, Timing};

/// Where a candidate address came from, kept in the report so an operator reads why a
/// host was or was not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Explicit,
    LastGood,
    Stored,
    Name,
    Sweep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub addr: SocketAddr,
    pub source: Source,
}

/// What one candidate turned out to be. `Verified` is the only verdict after which the
/// credential may be sent there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Verified { fingerprint: String },
    NoAnswer(String),
    NotMemoryIndustry,
    WrongHost { seen: Uuid },
    Impostor,
    Revoked,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Checked(Verdict),
    /// A candidate the budget ran out before it was asked ("no probado (tiempo agotado)").
    NotProbed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub candidate: Candidate,
    pub outcome: Outcome,
}

/// Why a discovery that found nothing found nothing: the loudest finding wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Revoked,
    Unknown,
    WrongHost { seen: Uuid },
    Impostor,
    NotFound,
}

#[derive(Debug, Clone)]
pub struct Discovery {
    pub verified: Option<SocketAddr>,
    pub attempts: Vec<Attempt>,
}

impl Discovery {
    /// `None` when a host was verified.
    pub fn failure(&self) -> Option<Failure> {
        // PASS-1 STUB
        unimplemented!("PASS-1 STUB: Discovery::failure")
    }
}

/// When the last proof was good, and whether anything since has cast doubt on it.
#[derive(Debug, Default)]
pub struct Freshness {
    _stub: (),
}

impl Freshness {
    pub fn needs_check(&self, _now: Instant, _t: &Timing) -> bool {
        // PASS-1 STUB
        unimplemented!("PASS-1 STUB: Freshness::needs_check")
    }

    pub fn verified_at(&mut self, _now: Instant) {
        // PASS-1 STUB
        unimplemented!("PASS-1 STUB: Freshness::verified_at")
    }

    pub fn transport_failed(&mut self) {
        // PASS-1 STUB
        unimplemented!("PASS-1 STUB: Freshness::transport_failed")
    }
}

/// Last good, then the stored addresses, then the resolved name, then the sweep; each
/// address once, in the position of its first appearance.
pub fn candidates_in_order(
    _s: &BridgeState,
    _resolved: &[IpAddr],
    _sweep: &[SocketAddr],
) -> Vec<Candidate> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: discovery::candidates_in_order")
}

/// `.1`-`.254` of the /24 of `own`, without `own`.
pub fn subnet_candidates(_own: Ipv4Addr, _port: u16) -> Vec<SocketAddr> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: discovery::subnet_candidates")
}

/// `GET /identity?cred=…&nonce=…` with no credential in it, judged against the pinned
/// host. Never sends `Authorization`.
pub async fn check_identity(
    _c: &reqwest::Client,
    _addr: SocketAddr,
    _s: &BridgeState,
    _nonce: [u8; 16],
    _limit: Duration,
) -> Verdict {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: discovery::check_identity")
}

pub async fn discover(
    _s: &BridgeState,
    _net: &dyn Network,
    _t: &Timing,
    _c: &reqwest::Client,
) -> Discovery {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: discovery::discover")
}

#[cfg(test)]
mod tests {
    use super::super::fake_host::{
        Closer, CredStatus, FakeHost, FakeNet, Identity, RawRecorder, Tarpit, Tarpits, client,
        contains_ci, ms, tiny_timing,
    };
    use super::*;
    use crate::accounts;

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    fn addr(text: &str) -> SocketAddr {
        text.parse().expect("a socket address")
    }

    fn identity_json(host_id: Uuid, credential: &str, proof: Option<String>) -> String {
        let mut doc = serde_json::json!({
            "service": "memory-industry",
            "host_id": host_id,
            "name": "madre",
            "credential": credential,
        });
        if let Some(proof) = proof {
            doc["proof"] = serde_json::json!(proof);
        }
        doc.to_string()
    }

    fn proof_for(id: &Identity, nonce: [u8; 16]) -> String {
        hex::encode(accounts::identity_proof(
            &id.secret.hash(),
            &id.host_id,
            &id.credential_id,
            &nonce,
        ))
    }

    async fn check(id: &Identity, at: SocketAddr) -> Verdict {
        let state = id.state(at.port(), vec![at], Some(at));
        check_identity(&client(), at, &state, [7u8; 16], ms(300)).await
    }

    /// H13
    #[test]
    fn candidates_follow_the_order_of_discovery_without_repeats() {
        let id = Identity::new();
        let (l, a, b, c, d) = (
            addr("10.0.0.5:8787"),
            addr("10.0.0.6:8787"),
            addr("10.0.0.7:8787"),
            addr("10.0.0.8:8787"),
            addr("10.0.0.9:8787"),
        );
        let state = id.state(8787, vec![l, a, b], Some(l));

        let found = candidates_in_order(&state, &[a.ip(), c.ip()], &[c, d]);

        assert_eq!(
            found,
            vec![
                Candidate {
                    addr: l,
                    source: Source::LastGood
                },
                Candidate {
                    addr: a,
                    source: Source::Stored
                },
                Candidate {
                    addr: b,
                    source: Source::Stored
                },
                Candidate {
                    addr: c,
                    source: Source::Name
                },
                Candidate {
                    addr: d,
                    source: Source::Sweep
                },
            ],
            "the cheapest and likeliest first (last good, stored, the name the OS resolves, \
             the sweep), each address once. Probing the name before the stored addresses \
             would make every request pay a resolver round trip; repeats would probe one dead \
             address three times"
        );
    }

    /// H11
    #[test]
    fn the_subnet_sweep_is_the_own_24_minus_the_own_address() {
        let found = subnet_candidates(Ipv4Addr::new(10, 0, 5, 7), 8787);

        assert_eq!(
            found.len(),
            253,
            ".1 to .254 is 254 hosts and the own address is not one to ask: 253. (.0 and .255 \
             are the network and broadcast addresses)"
        );
        let mut seen = std::collections::HashSet::new();
        for candidate in &found {
            let IpAddr::V4(ip) = candidate.ip() else {
                panic!("the sweep is IPv4 only, got {candidate}");
            };
            assert_eq!(
                ip.octets()[..3],
                [10, 0, 5],
                "every candidate stays inside the own /24, got {candidate}"
            );
            assert!(
                (1..=254).contains(&ip.octets()[3]) && ip.octets()[3] != 7,
                "no .0, no .255 and not the own address: {candidate}"
            );
            assert_eq!(candidate.port(), 8787, "the known host port");
            assert!(seen.insert(*candidate), "{candidate} appears twice");
        }
    }

    /// H11
    #[test]
    fn the_default_sweep_of_a_network_is_built_from_its_own_address() {
        struct OwnAddressOnly;
        #[async_trait::async_trait]
        impl Network for OwnAddressOnly {
            async fn resolve(&self, _name: &str) -> Vec<IpAddr> {
                Vec::new()
            }
            fn own_ipv4_towards(&self, _peer: IpAddr) -> Option<Ipv4Addr> {
                Some(Ipv4Addr::new(10, 0, 5, 7))
            }
        }

        let found = OwnAddressOnly.sweep_targets(8787);

        assert_eq!(
            found,
            subnet_candidates(Ipv4Addr::new(10, 0, 5, 7), 8787),
            "a Network that only knows its own address sweeps that address's /24 on the host \
             port; the production network does not override this"
        );
    }

    #[tokio::test]
    async fn a_host_that_proves_itself_is_verified_with_its_fingerprint() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;

        let verdict = check(&id, host.addr()).await;

        assert_eq!(
            verdict,
            Verdict::Verified {
                fingerprint: accounts::fingerprint(&id.host_id)
            },
            "the pinned host, answering with a proof only the credential holder and the host \
             can compute, is verified and shown by its fingerprint"
        );
    }

    /// H15
    #[tokio::test]
    async fn the_identity_probe_carries_a_nonce_and_never_the_credential() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let state = id.state(host.addr().port(), vec![host.addr()], Some(host.addr()));

        let verdict = check_identity(&client(), host.addr(), &state, [7u8; 16], ms(300)).await;

        assert!(
            matches!(verdict, Verdict::Verified { .. }),
            "anchor: the probe worked: {verdict:?}"
        );
        let asked = host.identity_requests();
        assert_eq!(asked.len(), 1, "one probe, one GET /identity");
        let probe = &asked[0];
        assert_eq!(probe.method, "GET");
        assert_eq!(
            probe.header("authorization"),
            None,
            "the probe is sent BEFORE the host is trusted: an Authorization header would hand \
             the credential to whoever answers"
        );
        assert_eq!(
            probe.query_param("cred").as_deref(),
            Some(id.credential_id.simple().to_string().as_str()),
            "the host needs the credential's id (not its secret) to know whose proof to compute"
        );
        assert_eq!(
            probe.query_param("nonce").as_deref(),
            Some("07070707070707070707070707070707"),
            "the nonce passed in is the nonce sent, 16 bytes as 32 hex"
        );
        let wire = host.received_bytes();
        assert!(
            !contains_ci(&wire, &id.secret_hex())
                && !contains_ci(&wire, "mi1.")
                && !contains_ci(&wire, "bearer"),
            "nothing of the credential may be on the wire of a probe"
        );
    }

    #[tokio::test]
    async fn another_host_id_is_a_wrong_host_never_a_match() {
        let id = Identity::new();
        let other = Uuid::new_v4();
        let host = FakeHost::start(other, vec![id.fake_cred()]).await;

        assert_eq!(
            check(&id, host.addr()).await,
            Verdict::WrongHost { seen: other },
            "a MemoryIndustry with another host id is another installation (or a restored \
             one): reported with the id it showed, never accepted silently"
        );
    }

    /// H17
    #[tokio::test]
    async fn a_revoked_and_an_unknown_credential_are_told_apart() {
        let id = Identity::new();
        let revoking = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        revoking.set_cred_status(id.credential_id, CredStatus::Revoked);
        let forgetting = FakeHost::start(id.host_id, vec![]).await;

        assert_eq!(check(&id, revoking.addr()).await, Verdict::Revoked);
        assert_eq!(
            check(&id, forgetting.addr()).await,
            Verdict::Unknown,
            "a host that never heard of the credential is not the same news as a revoked one: \
             one says 'pair again', the other 'this is a different database'"
        );
    }

    /// H15
    #[tokio::test]
    async fn an_answer_without_a_valid_proof_is_an_impostor() {
        let id = Identity::new();
        let nonce = [7u8; 16];
        let cases = [
            ("no proof at all", identity_json(id.host_id, "active", None)),
            (
                "a proof of zeros",
                identity_json(id.host_id, "active", Some("0".repeat(64))),
            ),
            (
                "a genuine proof, but for another nonce (a replay)",
                identity_json(id.host_id, "active", Some(proof_for(&id, [9u8; 16]))),
            ),
        ];
        for (name, body) in cases {
            let imposter = RawRecorder::start(&[("GET /identity", body)]).await;
            assert_eq!(
                check(&id, imposter.addr()).await,
                Verdict::Impostor,
                "{name}: it knows the host id (that is public) but cannot prove the credential"
            );
        }

        let replaying = RawRecorder::start(&[(
            "GET /identity",
            identity_json(id.host_id, "active", Some(proof_for(&id, nonce))),
        )])
        .await;
        assert!(
            matches!(check(&id, replaying.addr()).await, Verdict::Verified { .. }),
            "control: the same canned answer IS accepted when the nonce matches, so the replay \
             case above fails because of the nonce and for no other reason"
        );
    }

    #[tokio::test]
    async fn something_that_is_not_memory_industry_is_said_so() {
        let id = Identity::new();
        let someone_else = Uuid::new_v4();
        let cases = [
            ("a web server", r#"{"hola":1}"#.to_string()),
            (
                "another service",
                serde_json::json!({"service":"other","host_id":someone_else,"name":"x"})
                    .to_string(),
            ),
            ("an html page", "<html>not found</html>".to_string()),
        ];
        for (name, body) in cases {
            let server = RawRecorder::start(&[("", body)]).await;
            assert_eq!(
                check(&id, server.addr()).await,
                Verdict::NotMemoryIndustry,
                "{name}: an answer that is not this service is not an impostor of it, just not it"
            );
        }
    }

    #[tokio::test]
    async fn a_dead_or_silent_candidate_has_no_answer_inside_its_limit() {
        let id = Identity::new();
        let closer = Closer::start().await;
        let tarpit = Tarpit::start().await;

        let dead = check(&id, closer.addr()).await;
        let started = Instant::now();
        let silent = check(&id, tarpit.addr()).await;

        assert!(
            matches!(dead, Verdict::NoAnswer(_)),
            "accept-and-close: {dead:?}"
        );
        assert!(
            matches!(silent, Verdict::NoAnswer(_)),
            "accept-and-hold: {silent:?}"
        );
        assert!(
            started.elapsed() < ms(1500),
            "the 300 ms limit bounds a candidate that never answers; took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn every_discovery_asks_with_a_fresh_nonce() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let state = id.state(host.addr().port(), vec![host.addr()], Some(host.addr()));
        let net = FakeNet::new();

        for _ in 0..2 {
            let found = discover(&state, &net, &tiny_timing(), &client()).await;
            assert_eq!(
                found.verified,
                Some(host.addr()),
                "anchor: the host is found"
            );
        }

        let nonces: Vec<String> = host
            .identity_requests()
            .iter()
            .filter_map(|r| r.query_param("nonce"))
            .collect();
        assert_eq!(nonces.len(), 2, "one probe per discovery");
        assert_ne!(
            nonces[0], nonces[1],
            "a repeated nonce lets an impostor replay one genuine proof it recorded earlier"
        );
    }

    /// H11
    #[tokio::test]
    async fn last_good_is_probed_first_and_a_hit_stops_the_search() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let closer = Closer::start().await;
        let state = id.state(host.addr().port(), vec![closer.addr()], Some(host.addr()));
        let net = FakeNet::new().sweep(vec![closer.addr()]);

        let found = discover(&state, &net, &tiny_timing(), &client()).await;

        assert_eq!(found.verified, Some(host.addr()));
        assert_eq!(
            found.attempts.len(),
            1,
            "the first hit ends it: {:?}",
            found.attempts
        );
        assert_eq!(found.attempts[0].candidate.source, Source::LastGood);
        assert_eq!(
            closer.accepted(),
            0,
            "a stored address behind a hit is never touched"
        );
        assert_eq!(
            net.resolves(),
            0,
            "no resolver round trip when an address answered"
        );
        assert_eq!(net.sweeps(), 0, "no sweep when an address answered");
    }

    /// H15
    #[tokio::test]
    async fn an_impostor_in_the_way_never_sees_the_credential() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let imposter = RawRecorder::start(&[(
            "GET /identity",
            identity_json(id.host_id, "active", Some("0".repeat(64))),
        )])
        .await;
        let state = id.state(
            host.addr().port(),
            vec![imposter.addr(), host.addr()],
            Some(imposter.addr()),
        );

        let found = discover(&state, &FakeNet::new(), &tiny_timing(), &client()).await;

        assert_eq!(
            found.verified,
            Some(host.addr()),
            "the stored address that really is the host wins after the impostor fails its proof"
        );
        assert_eq!(
            found.attempts[0].outcome,
            Outcome::Checked(Verdict::Impostor),
            "the impostor is on the record"
        );
        let heard = imposter.received();
        for forbidden in ["authorization", "bearer", "mi1.", id.secret_hex().as_str()] {
            assert!(
                !contains_ci(&heard, forbidden),
                "the impostor received {forbidden:?}: the credential left the machine before the \
                 peer proved who it was"
            );
        }
        assert!(
            imposter
                .lines()
                .iter()
                .all(|line| line.starts_with("GET /identity")),
            "the impostor may only have been asked who it is. Saw: {:?}",
            imposter.lines()
        );
    }

    /// H16
    #[tokio::test]
    async fn a_changed_host_id_is_reported_and_never_accepted() {
        let id = Identity::new();
        let other = Uuid::new_v4();
        let replaced = FakeHost::start(other, vec![id.fake_cred()]).await;
        let state = id.state(
            replaced.addr().port(),
            vec![replaced.addr()],
            Some(replaced.addr()),
        );

        let found = discover(&state, &FakeNet::new(), &tiny_timing(), &client()).await;

        assert_eq!(
            found.verified, None,
            "a different host id must never be adopted"
        );
        assert!(
            found
                .attempts
                .iter()
                .any(|a| a.outcome == Outcome::Checked(Verdict::WrongHost { seen: other })),
            "the report must show the wrong host it met. Got: {:?}",
            found.attempts
        );
        assert_eq!(
            found.failure(),
            Some(Failure::WrongHost { seen: other }),
            "the final reason is 'the host changed', not 'host not found': the operator has to \
             decide whether the machine was replaced, and the bridge cannot"
        );
        assert!(
            !contains_ci(&replaced.received_bytes(), "authorization"),
            "the other installation never gets the credential either"
        );
    }

    /// H16
    #[tokio::test]
    async fn another_candidate_that_verifies_wins_over_a_wrong_host() {
        let id = Identity::new();
        let other = FakeHost::start(Uuid::new_v4(), vec![id.fake_cred()]).await;
        let real = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let state = id.state(
            real.addr().port(),
            vec![other.addr(), real.addr()],
            Some(other.addr()),
        );

        let found = discover(&state, &FakeNet::new(), &tiny_timing(), &client()).await;

        assert_eq!(found.verified, Some(real.addr()));
        assert_eq!(
            found.failure(),
            None,
            "a verified host means no failure to report"
        );
        assert!(
            found
                .attempts
                .iter()
                .any(|a| matches!(a.outcome, Outcome::Checked(Verdict::WrongHost { .. }))),
            "the wrong host stays in the report as a finding"
        );
    }

    /// H17
    #[tokio::test]
    async fn a_revoked_answer_stops_the_search() {
        let id = Identity::new();
        let revoking = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        revoking.set_cred_status(id.credential_id, CredStatus::Revoked);
        let closer = Closer::start().await;
        let state = id.state(
            revoking.addr().port(),
            vec![revoking.addr(), closer.addr()],
            Some(revoking.addr()),
        );
        let net = FakeNet::new().sweep(vec![closer.addr()]);

        let found = discover(&state, &net, &tiny_timing(), &client()).await;

        assert_eq!(found.verified, None);
        assert_eq!(found.failure(), Some(Failure::Revoked));
        assert_eq!(
            closer.accepted(),
            0,
            "a revoked credential will not become valid on another address: asking the rest \
             of the LAN about it is noise"
        );
        assert_eq!(net.sweeps(), 0, "and no sweep is started for it");
    }

    /// H21
    #[tokio::test]
    async fn the_name_is_tried_before_any_sweep() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let dead = Closer::start().await;
        let state = id.state(host.addr().port(), vec![dead.addr()], Some(dead.addr()));
        let net = FakeNet::new()
            .resolving("madre", &[LOOPBACK])
            .sweep(vec![dead.addr()]);

        let found = discover(&state, &net, &tiny_timing(), &client()).await;

        assert_eq!(found.verified, Some(host.addr()));
        let hit = found
            .attempts
            .iter()
            .find(|a| matches!(a.outcome, Outcome::Checked(Verdict::Verified { .. })))
            .expect("one attempt verified");
        assert_eq!(
            hit.candidate.source,
            Source::Name,
            "the host moved to an address nobody stored: the name finds it"
        );
        assert_eq!(
            net.asked(),
            vec!["madre".to_string()],
            "the pinned host name is what is resolved"
        );
        assert_eq!(net.sweeps(), 0, "no sweep when the name was enough");
    }

    /// H18
    #[test]
    fn the_verification_goes_stale_after_the_window_or_a_transport_failure() {
        let t = Timing::production();
        let base = Instant::now();
        let mut fresh = Freshness::default();

        assert!(fresh.needs_check(base, &t), "nothing was ever verified");
        fresh.verified_at(base);
        assert!(
            !fresh.needs_check(base + Duration::from_secs(29), &t),
            "29 s after a good proof the address is reused"
        );
        assert!(
            fresh.needs_check(base + Duration::from_secs(31), &t),
            "31 s after, the address may have changed hands (DHCP): ask again before sending \
             the credential"
        );
        fresh.verified_at(base);
        fresh.transport_failed();
        assert!(
            fresh.needs_check(base + Duration::from_secs(1), &t),
            "a transport error casts doubt on the address at once, whatever the window says"
        );
    }

    /// H19
    #[tokio::test]
    async fn the_sweep_never_probes_more_than_the_parallelism_and_ends_with_the_budget() {
        let id = Identity::new();
        let tarpits = Tarpits::start_many(40).await;
        let state = id.state(8787, vec![], None);
        let net = FakeNet::new().sweep(tarpits.addrs.clone());
        let timing = Timing {
            probe_parallelism: 4,
            probe_total: ms(300),
            identity: ms(200),
            probe_connect: ms(100),
            discovery_total: ms(3000),
            ..tiny_timing()
        };

        let started = Instant::now();
        let found = discover(&state, &net, &timing, &client()).await;
        let took = started.elapsed();

        assert_eq!(found.verified, None, "nobody in the sweep is the host");
        assert!(
            took < timing.probe_total + ms(500),
            "the sweep has a total budget of {:?}; 40 silent candidates took {took:?} (an \
             unbounded sweep would take 40/4 x 200 ms = 2 s)",
            timing.probe_total
        );
        assert!(
            tarpits.log.max_overlap(ms(15)) <= 4,
            "at most probe_parallelism (4) connections may be open at once; saw {}",
            tarpits.log.max_overlap(ms(15))
        );
        assert_eq!(
            found.attempts.len(),
            40,
            "every candidate is in the report, probed or not"
        );
        let skipped = found
            .attempts
            .iter()
            .filter(|a| matches!(&a.outcome, Outcome::NotProbed(why) if why.contains("no probado (tiempo agotado)")))
            .count();
        let asked = found
            .attempts
            .iter()
            .filter(|a| matches!(a.outcome, Outcome::Checked(_)))
            .count();
        assert!(
            asked <= 8 && skipped >= 32 && asked + skipped == 40,
            "in 300 ms at 4 at a time, 200 ms each, at most 8 can be asked; the rest must be \
             reported as 'no probado (tiempo agotado)'. Asked {asked}, skipped {skipped}"
        );
    }
}
