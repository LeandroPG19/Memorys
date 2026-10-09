//! Finding the host again, and refusing to hand a credential to anyone who cannot prove
//! it is the host. Order, bounds and verdicts are decided here and nowhere else.

use std::collections::HashSet;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::net::TcpStream;
use uuid::Uuid;

use super::state::BridgeState;
use super::{Network, Timing};
use crate::accounts::{self, IdentityReply};

const SERVICE: &str = "memory-industry";

/// An identity answer is a few hundred bytes; anything bigger is not one.
const MAX_IDENTITY_BYTES: usize = 64 * 1024;

const NOT_PROBED_BUDGET: &str = "no probado (tiempo agotado)";
const NOT_PROBED_FOUND: &str = "no probado (ya se encontró la madre)";

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

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Explicit => "indicada",
            Source::LastGood => "última buena",
            Source::Stored => "guardada",
            Source::Name => "por nombre",
            Source::Sweep => "barrido",
        })
    }
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

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Verified { fingerprint } => write!(f, "verificada ({fingerprint})"),
            Verdict::NoAnswer(why) => write!(f, "sin respuesta ({why})"),
            Verdict::NotMemoryIndustry => f.write_str("no es MemoryIndustry"),
            Verdict::WrongHost { seen } => {
                write!(
                    f,
                    "otra instalación (huella {})",
                    accounts::fingerprint(seen)
                )
            }
            Verdict::Impostor => f.write_str("no pudo probar que es la madre"),
            Verdict::Revoked => f.write_str("credencial revocada"),
            Verdict::Unknown => f.write_str("credencial desconocida"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Checked(Verdict),
    /// A candidate the budget ran out before it was asked ("no probado (tiempo agotado)").
    NotProbed(String),
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Checked(verdict) => fmt::Display::fmt(verdict, f),
            Outcome::NotProbed(why) => f.write_str(why),
        }
    }
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

/// Lower is louder.
fn failure_of(verdict: &Verdict) -> Option<(u8, Failure)> {
    match verdict {
        Verdict::Revoked => Some((0, Failure::Revoked)),
        Verdict::Unknown => Some((1, Failure::Unknown)),
        Verdict::WrongHost { seen } => Some((2, Failure::WrongHost { seen: *seen })),
        Verdict::Impostor => Some((3, Failure::Impostor)),
        _ => None,
    }
}

impl Discovery {
    /// `None` when a host was verified.
    pub fn failure(&self) -> Option<Failure> {
        if self.verified.is_some() {
            return None;
        }
        let loudest = self
            .attempts
            .iter()
            .filter_map(|attempt| match &attempt.outcome {
                Outcome::Checked(verdict) => failure_of(verdict),
                Outcome::NotProbed(_) => None,
            })
            .min_by_key(|(rank, _)| *rank);
        Some(loudest.map_or(Failure::NotFound, |(_, failure)| failure))
    }

    /// What was tried and what each answered, for an operator. No secrets in it.
    pub fn report(&self) -> String {
        let lines: Vec<String> = self
            .attempts
            .iter()
            .map(|a| {
                format!(
                    "{} ({}): {}",
                    a.candidate.addr, a.candidate.source, a.outcome
                )
            })
            .collect();
        lines.join("; ")
    }
}

/// When the last proof was good, and whether anything since has cast doubt on it.
#[derive(Debug, Default)]
pub struct Freshness {
    verified: Option<Instant>,
    doubted: bool,
}

impl Freshness {
    pub fn needs_check(&self, now: Instant, t: &Timing) -> bool {
        self.doubted
            || self
                .verified
                .is_none_or(|at| now.saturating_duration_since(at) >= t.reverify_after)
    }

    pub fn verified_at(&mut self, now: Instant) {
        self.verified = Some(now);
        self.doubted = false;
    }

    pub fn transport_failed(&mut self) {
        self.doubted = true;
    }
}

fn push_unique(out: &mut Vec<Candidate>, addr: SocketAddr, source: Source) {
    if !out.iter().any(|known| known.addr == addr) {
        out.push(Candidate { addr, source });
    }
}

/// Last good, then the stored addresses, then the resolved name, then the sweep; each
/// address once, in the position of its first appearance.
pub fn candidates_in_order(
    s: &BridgeState,
    resolved: &[IpAddr],
    sweep: &[SocketAddr],
) -> Vec<Candidate> {
    let mut out = Vec::new();
    if let Some(addr) = s.last_good {
        push_unique(&mut out, addr, Source::LastGood);
    }
    for addr in &s.addresses {
        push_unique(&mut out, *addr, Source::Stored);
    }
    for ip in resolved {
        push_unique(&mut out, SocketAddr::new(*ip, s.host_port), Source::Name);
    }
    for addr in sweep {
        push_unique(&mut out, *addr, Source::Sweep);
    }
    out
}

/// `.1`-`.254` of the /24 of `own`, without `own`.
pub fn subnet_candidates(own: Ipv4Addr, port: u16) -> Vec<SocketAddr> {
    let [a, b, c, own_last] = own.octets();
    (1..=254u8)
        .filter(|last| *last != own_last)
        .map(|last| SocketAddr::new(IpAddr::V4(Ipv4Addr::new(a, b, c, last)), port))
        .collect()
}

/// A word for an operator, not the transport library's sentence (which carries the URL).
fn transport_word(err: &reqwest::Error) -> String {
    let word = if err.is_timeout() {
        "tiempo agotado"
    } else if err.is_connect() {
        "conexión rechazada o sin ruta"
    } else {
        "cerró la conexión sin responder"
    };
    word.to_string()
}

/// `GET /identity<query>` and the reply, if what answered speaks MemoryIndustry. No
/// header of ours goes with it: the peer is not trusted yet.
async fn fetch_identity(
    c: &reqwest::Client,
    addr: SocketAddr,
    query: &str,
    limit: Duration,
) -> Result<IdentityReply, Verdict> {
    let sent = c
        .get(format!("http://{addr}/identity{query}"))
        .timeout(limit)
        .send()
        .await;
    let response = sent.map_err(|e| Verdict::NoAnswer(transport_word(&e)))?;
    let too_big = response
        .content_length()
        .is_some_and(|n| n > MAX_IDENTITY_BYTES as u64);
    if !response.status().is_success() || too_big {
        return Err(Verdict::NotMemoryIndustry);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Verdict::NoAnswer(transport_word(&e)))?;
    let reply: IdentityReply = serde_json::from_slice(&bytes)
        .ok()
        .filter(|_| bytes.len() <= MAX_IDENTITY_BYTES)
        .ok_or(Verdict::NotMemoryIndustry)?;
    if reply.service == SERVICE {
        Ok(reply)
    } else {
        Err(Verdict::NotMemoryIndustry)
    }
}

/// Who answers at `addr`, asked with no credential: for pairing, which offers a code only
/// to what speaks MemoryIndustry.
pub(crate) async fn probe_service(
    c: &reqwest::Client,
    addr: SocketAddr,
    limit: Duration,
) -> Result<IdentityReply, Verdict> {
    fetch_identity(c, addr, "", limit).await
}

fn proof_holds(reply: &IdentityReply, s: &BridgeState, nonce: &[u8; 16]) -> bool {
    let expected = accounts::identity_proof(&s.secret.hash(), &s.host_id, &s.credential_id, nonce);
    reply
        .proof
        .as_deref()
        .is_some_and(|presented| accounts::verify_mac(expected, presented))
}

/// The pinned host id is public, so knowing it proves nothing; the proof is what does.
fn judge(reply: &IdentityReply, s: &BridgeState, nonce: &[u8; 16]) -> Verdict {
    if reply.host_id != s.host_id {
        return Verdict::WrongHost {
            seen: reply.host_id,
        };
    }
    match reply.credential.as_deref() {
        Some("revoked") => Verdict::Revoked,
        Some("unknown") => Verdict::Unknown,
        Some("active") if proof_holds(reply, s, nonce) => Verdict::Verified {
            fingerprint: accounts::fingerprint(&s.host_id),
        },
        _ => Verdict::Impostor,
    }
}

/// `GET /identity?cred=…&nonce=…` with no credential in it, judged against the pinned
/// host. Never sends `Authorization`.
pub async fn check_identity(
    c: &reqwest::Client,
    addr: SocketAddr,
    s: &BridgeState,
    nonce: [u8; 16],
    limit: Duration,
) -> Verdict {
    let query = format!(
        "?cred={}&nonce={}",
        s.credential_id.simple(),
        hex::encode(nonce)
    );
    match fetch_identity(c, addr, &query, limit).await {
        Ok(reply) => judge(&reply, s, &nonce),
        Err(verdict) => verdict,
    }
}

/// A TCP connect inside its own short budget: the part of a probe that, on a quiet /24,
/// is almost always a SYN nobody answers.
async fn reachable(addr: SocketAddr, within: Duration) -> bool {
    matches!(
        tokio::time::timeout(within, TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

/// The addresses of `addrs` that speak MemoryIndustry, in the order given. At most
/// `probe_parallelism` probes run at once and the whole sweep ends at `probe_total`.
pub(crate) async fn sweep_services(
    c: &reqwest::Client,
    t: &Timing,
    addrs: &[SocketAddr],
) -> Vec<SocketAddr> {
    let mut serving = vec![false; addrs.len()];
    let collect = async {
        let mut probes = futures::stream::iter(addrs.iter().copied().enumerate())
            .map(move |(i, addr)| async move {
                let up = reachable(addr, t.probe_connect).await;
                (i, up && probe_service(c, addr, t.identity).await.is_ok())
            })
            .buffer_unordered(t.probe_parallelism.max(1));
        while let Some((i, answers)) = probes.next().await {
            serving[i] = answers;
        }
    };
    let _ = tokio::time::timeout(t.probe_total, collect).await;
    addrs
        .iter()
        .zip(serving)
        .filter_map(|(addr, answers)| answers.then_some(*addr))
        .collect()
}

/// What to do after a verdict.
enum Flow {
    Found(SocketAddr),
    Stop,
    Continue,
}

fn flow_of(addr: SocketAddr, verdict: &Verdict) -> Flow {
    match verdict {
        Verdict::Verified { .. } => Flow::Found(addr),
        Verdict::Revoked | Verdict::Unknown => Flow::Stop,
        _ => Flow::Continue,
    }
}

/// `Some(answer)` when the search is over.
fn settle(flow: Flow) -> Option<Option<SocketAddr>> {
    match flow {
        Flow::Found(addr) => Some(Some(addr)),
        Flow::Stop => Some(None),
        Flow::Continue => None,
    }
}

#[derive(Clone, Copy)]
struct Ctx<'a> {
    s: &'a BridgeState,
    net: &'a dyn Network,
    t: &'a Timing,
    c: &'a reqwest::Client,
}

async fn ask(ctx: Ctx<'_>, addr: SocketAddr, limit: Duration) -> Verdict {
    check_identity(ctx.c, addr, ctx.s, accounts::random_bytes::<16>(), limit).await
}

async fn sweep_probe(ctx: Ctx<'_>, addr: SocketAddr) -> Verdict {
    if !reachable(addr, ctx.t.probe_connect).await {
        return Verdict::NoAnswer("sin conexión".to_string());
    }
    ask(ctx, addr, ctx.t.identity).await
}

struct Search<'a> {
    ctx: Ctx<'a>,
    deadline: Instant,
    tried: HashSet<SocketAddr>,
    attempts: Vec<Attempt>,
}

impl Search<'_> {
    fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// The candidates not asked yet, marked as asked.
    fn fresh(&mut self, found: Vec<Candidate>) -> Vec<Candidate> {
        found
            .into_iter()
            .filter(|candidate| self.tried.insert(candidate.addr))
            .collect()
    }

    fn skip(&mut self, left: &[Candidate], why: &str) {
        for candidate in left {
            self.attempts.push(Attempt {
                candidate: *candidate,
                outcome: Outcome::NotProbed(why.to_string()),
            });
        }
    }

    async fn run(&mut self) -> Option<SocketAddr> {
        let stored = self.fresh(candidates_in_order(self.ctx.s, &[], &[]));
        if let Some(done) = settle(self.in_turn(stored).await) {
            return done;
        }
        let named = self.named().await;
        if let Some(done) = settle(self.in_turn(named).await) {
            return done;
        }
        let swept = self.swept();
        settle(self.in_parallel(swept).await).flatten()
    }

    async fn named(&mut self) -> Vec<Candidate> {
        let name = self.ctx.s.host_name.as_str();
        if name.is_empty() {
            return Vec::new();
        }
        let asked = tokio::time::timeout(self.ctx.t.resolve, self.ctx.net.resolve(name)).await;
        let resolved = asked.unwrap_or_default();
        self.fresh(candidates_in_order(self.ctx.s, &resolved, &[]))
    }

    fn swept(&mut self) -> Vec<Candidate> {
        let targets = self.ctx.net.sweep_targets(self.ctx.s.host_port);
        self.fresh(candidates_in_order(self.ctx.s, &[], &targets))
    }

    /// One at a time, in order: the cheap, likely addresses.
    async fn in_turn(&mut self, candidates: Vec<Candidate>) -> Flow {
        for (i, candidate) in candidates.iter().enumerate() {
            let left = self.remaining();
            if left.is_zero() {
                self.skip(&candidates[i..], NOT_PROBED_BUDGET);
                return Flow::Continue;
            }
            let verdict = ask(self.ctx, candidate.addr, self.ctx.t.identity.min(left)).await;
            let flow = flow_of(candidate.addr, &verdict);
            self.attempts.push(Attempt {
                candidate: *candidate,
                outcome: Outcome::Checked(verdict),
            });
            if !matches!(flow, Flow::Continue) {
                return flow;
            }
        }
        Flow::Continue
    }

    /// The sweep: at most `probe_parallelism` at once, all of it inside `probe_total`.
    /// Whatever the budget cut off is reported as not probed.
    async fn in_parallel(&mut self, candidates: Vec<Candidate>) -> Flow {
        let ctx = self.ctx;
        let budget = ctx.t.probe_total.min(self.remaining());
        let addrs: Vec<SocketAddr> = candidates.iter().map(|c| c.addr).collect();
        let mut results: Vec<Option<Verdict>> = vec![None; addrs.len()];
        let collect = async {
            let mut probes = futures::stream::iter(addrs.iter().copied().enumerate())
                .map(move |(i, addr)| async move { (i, sweep_probe(ctx, addr).await) })
                .buffer_unordered(ctx.t.probe_parallelism.max(1));
            while let Some((i, verdict)) = probes.next().await {
                let flow = flow_of(addrs[i], &verdict);
                results[i] = Some(verdict);
                if !matches!(flow, Flow::Continue) {
                    return flow;
                }
            }
            Flow::Continue
        };
        let flow = tokio::time::timeout(budget, collect)
            .await
            .unwrap_or(Flow::Continue);
        self.record_swept(candidates, results, &flow);
        flow
    }

    fn record_swept(
        &mut self,
        candidates: Vec<Candidate>,
        results: Vec<Option<Verdict>>,
        flow: &Flow,
    ) {
        let why = if matches!(flow, Flow::Continue) {
            NOT_PROBED_BUDGET
        } else {
            NOT_PROBED_FOUND
        };
        for (candidate, result) in candidates.into_iter().zip(results) {
            let outcome = match result {
                Some(verdict) => Outcome::Checked(verdict),
                None => Outcome::NotProbed(why.to_string()),
            };
            self.attempts.push(Attempt { candidate, outcome });
        }
    }
}

/// Finds the host: stored addresses, then the name, then the bounded sweep. Every
/// candidate is accepted only after its identity check; a revoked or unknown credential
/// ends the search, a wrong host id is a finding and the search goes on.
pub async fn discover(
    s: &BridgeState,
    net: &dyn Network,
    t: &Timing,
    c: &reqwest::Client,
) -> Discovery {
    let mut search = Search {
        ctx: Ctx { s, net, t, c },
        deadline: Instant::now() + t.discovery_total,
        tried: HashSet::new(),
        attempts: Vec::new(),
    };
    let verified = search.run().await;
    Discovery {
        verified,
        attempts: search.attempts,
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake_host::{
        Closer, CredStatus, FakeHost, FakeNet, Identity, RawRecorder, Tarpit, Tarpits, client,
        contains_ci, ms, tiny_timing,
    };
    use super::*;
    use crate::accounts;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// An address nothing listens on: bound and let go, so a connect to it is refused at once.
    fn refused_address() -> SocketAddr {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("a free loopback port");
        let at = probe.local_addr().expect("the bound address");
        drop(probe);
        at
    }

    /// One answer, with the status line and body given, to the one request it reads. A
    /// RawRecorder only answers 200, and a non-2xx with a body is what this needs.
    async fn answer_once(status_line: &'static str, body: String) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a free loopback port");
        let at = listener.local_addr().expect("the bound address");
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0u8; 1];
                match stream.read(&mut byte).await {
                    Ok(1) => head.push(byte[0]),
                    _ => return,
                }
            }
            let reply = format!(
                "{status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        at
    }

    /// A MemoryIndustry identity answer of `total` bytes, padded with an unknown field.
    fn identity_padded_to(host_id: Uuid, total: usize) -> String {
        let mut doc = serde_json::json!({
            "service": SERVICE, "host_id": host_id, "name": "madre",
            "credential": "revoked", "pad": "",
        });
        let bare = doc.to_string().len();
        doc["pad"] = serde_json::json!("x".repeat(total.saturating_sub(bare)));
        doc.to_string()
    }

    /// An impostor is the loudest failure after a revoked or unknown credential, and it is
    /// not the same news as a host that was never found.
    #[test]
    fn an_impostor_verdict_is_reported_as_an_impostor_and_not_as_a_missing_host() {
        let found = Discovery {
            verified: None,
            attempts: vec![Attempt {
                candidate: Candidate {
                    addr: addr("192.168.0.30:8787"),
                    source: Source::Stored,
                },
                outcome: Outcome::Checked(Verdict::Impostor),
            }],
        };

        assert_eq!(
            found.failure(),
            Some(Failure::Impostor),
            "an answer that could not prove it is the host is an impostor, not a host that was \
             not found"
        );
    }

    /// Each way a transport can fail has its own word, which the operator reads in
    /// `bridge doctor`. The words are the contract, so the exact strings are asserted.
    #[tokio::test]
    async fn a_transport_failure_is_named_by_a_word_for_each_way_it_can_fail() {
        let id = Identity::new();
        let state = id.state(8787, vec![], None);
        let silent = Tarpit::start().await;
        let hung_up = Closer::start().await;

        assert_eq!(
            check_identity(&client(), silent.addr(), &state, [7u8; 16], ms(50)).await,
            Verdict::NoAnswer("tiempo agotado".to_string()),
            "a host that holds the connection past the limit is a timeout"
        );
        assert_eq!(
            check_identity(&client(), refused_address(), &state, [7u8; 16], ms(300)).await,
            Verdict::NoAnswer("conexión rechazada o sin ruta".to_string()),
            "nothing listens at the address: the connect is refused"
        );
        assert_eq!(
            check_identity(&client(), hung_up.addr(), &state, [7u8; 16], ms(300)).await,
            Verdict::NoAnswer("cerró la conexión sin responder".to_string()),
            "a peer that accepts and hangs up answers nothing"
        );
    }

    /// The connect a sweep probe makes first: a listening port is reachable, a refused one
    /// is not, and a sweep must not ask a port that is not.
    #[tokio::test]
    async fn reachable_tells_a_listening_port_from_a_refused_one() {
        let listening = Closer::start().await;

        assert!(
            reachable(listening.addr(), ms(500)).await,
            "a port that accepts a connect is reachable"
        );
        assert!(
            !reachable(refused_address(), ms(500)).await,
            "a port nothing listens on is not reachable"
        );
    }

    /// The limit is 64 KiB. A 2 KiB identity is read. The `*` to `+` mutant reads the limit
    /// as 1 KiB + 64 bytes, and it is that mutant this refuses.
    #[tokio::test]
    async fn an_identity_of_two_kilobytes_is_read_and_not_refused_for_its_size() {
        let id = Identity::new();
        let body = identity_padded_to(id.host_id, 2048);
        let host = RawRecorder::start(&[("GET /identity", body)]).await;

        assert_eq!(
            check(&id, host.addr()).await,
            Verdict::Revoked,
            "a 2 KiB answer is inside the 64 KiB limit and says what the credential is"
        );
    }

    /// Exactly the limit is read; one byte more is not. The Content-Length check is the one
    /// that refuses early, before the body is read.
    #[tokio::test]
    async fn an_identity_of_exactly_the_limit_is_read_and_one_byte_more_is_not() {
        let id = Identity::new();
        let at_limit = identity_padded_to(id.host_id, MAX_IDENTITY_BYTES);
        assert_eq!(
            at_limit.len(),
            MAX_IDENTITY_BYTES,
            "anchor: the helper builds the boundary it claims"
        );
        let host = RawRecorder::start(&[("GET /identity", at_limit)]).await;
        assert_eq!(
            check(&id, host.addr()).await,
            Verdict::Revoked,
            "64 KiB is the limit itself, and the limit is read"
        );

        let over = identity_padded_to(id.host_id, MAX_IDENTITY_BYTES + 1);
        let host = RawRecorder::start(&[("GET /identity", over)]).await;
        assert_eq!(
            check(&id, host.addr()).await,
            Verdict::NotMemoryIndustry,
            "one byte past the limit is not read as an identity"
        );
    }

    /// Anything that is not 2xx is not an identity, whatever body it carries. The body here
    /// is a valid revoked identity, so only the status decides.
    #[tokio::test]
    async fn an_answer_that_is_not_2xx_is_not_memory_industry_even_with_an_identity_body() {
        let id = Identity::new();
        let body = identity_json(id.host_id, "revoked", None);
        let not_found = answer_once("HTTP/1.1 404 Not Found", body.clone()).await;
        let ok = answer_once("HTTP/1.1 200 OK", body).await;

        assert_eq!(
            check(&id, ok).await,
            Verdict::Revoked,
            "anchor: the same body under a 200 is a revoked identity"
        );
        assert_eq!(
            check(&id, not_found).await,
            Verdict::NotMemoryIndustry,
            "a 404 is not this service, even when its body reads like one"
        );
    }

    /// The sweep is over at the first host that proves itself, whatever the silent
    /// candidates before it did. The host here is the only one that answers.
    #[tokio::test]
    async fn the_sweep_finds_the_host_among_silent_candidates() {
        let id = Identity::new();
        let host = FakeHost::start(id.host_id, vec![id.fake_cred()]).await;
        let silent = Closer::start().await;
        let state = id.state(host.addr().port(), vec![], None);
        let net = FakeNet::new().sweep(vec![silent.addr(), host.addr()]);

        let found = discover(&state, &net, &tiny_timing(), &client()).await;

        assert_eq!(
            found.verified,
            Some(host.addr()),
            "a silent candidate in the sweep does not end it: the host that proves itself wins"
        );
        assert_eq!(
            net.sweeps(),
            1,
            "the stored and named candidates found nothing, so the sweep ran"
        );
    }
}
