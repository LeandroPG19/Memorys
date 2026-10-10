//! The UDP announcement: how a client on the same LAN learns where the host is without
//! knowing its address. Both sides of the protocol live here, the host's responder and the
//! client's `probe`, so the shape of a request and of a reply is written once.
//!
//! An answer is only ever a candidate. Whoever reads it still has to make the host prove
//! itself on `/identity` before a credential goes anywhere: a reply is cheap to forge.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What the host says about itself: the same `host_id` and name `/identity` gives, and the
/// TCP port its daemon serves on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub service: String,
    pub v: u32,
    pub host_id: Uuid,
    pub host_name: String,
    pub port: u16,
}

/// The reply `packet` deserves when it came from `from`, or `None` when it deserves none:
/// a request of the wrong shape or size, or an origin outside the private ranges and loopback.
///
/// The decision without the socket, because an origin like 8.8.8.8 cannot be produced on a
/// loopback test.
pub fn answer(packet: &[u8], from: SocketAddr, me: &Reply) -> Option<Vec<u8>> {
    let _ = (packet, from, me);
    unimplemented!("tajada A, pasada 2: answer")
}

/// How many answers each origin and the whole host may get, so the responder is no amplifier.
#[derive(Debug, Default)]
pub struct Limiter {}

impl Limiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether one more answer to `origin` fits at `now`.
    pub fn allow(&mut self, origin: IpAddr, now: Instant) -> bool {
        let _ = (origin, now);
        unimplemented!("tajada A, pasada 2: Limiter::allow")
    }
}

/// Answers every acceptable request on `socket`, one datagram at a time and in arrival
/// order, until the socket fails. `clock` is injected so a test freezes the rate window.
pub async fn respond<C>(socket: tokio::net::UdpSocket, me: Reply, clock: C) -> std::io::Result<()>
where
    C: FnMut() -> Instant + Send,
{
    let _ = (socket, me, clock);
    unimplemented!("tajada A, pasada 2: respond")
}

/// Sends one request to each of `targets` and collects the valid replies that arrive within
/// `wait`, each with the address it came from.
pub async fn probe(port: u16, wait: Duration, targets: &[SocketAddr]) -> Vec<(SocketAddr, Reply)> {
    let _ = (port, wait, targets);
    unimplemented!("tajada A, pasada 2: probe")
}

/// The limited broadcast and the directed broadcast of the own /24, on `port`.
pub fn broadcast_targets(own: Ipv4Addr, port: u16) -> Vec<SocketAddr> {
    let _ = (own, port);
    unimplemented!("tajada A, pasada 2: broadcast_targets")
}
