//! Where every outbound HTTP client starts: model downloads, hosted LLMs, the bridge, sync between nodes, the probes.
//!
//! reqwest 0.13 built with `rustls-no-provider` leaves the crypto provider to the program and panics when a client
//! is built before one is installed. ring is the provider sqlx already uses, so the binary carries one; it is installed
//! here, once, and a client made anywhere else compiles all the same and panics when it is built if no other client of
//! that process got there first (`tests/group_no_server/outbound_http_contract.rs` fails if one is).
//!
//! A server's certificate is checked by the system's verifier, not by a bundle compiled in: on Linux the store
//! `ca-certificates` keeps (or the file `SSL_CERT_FILE` names), on Windows and macOS the operating system, which also
//! asks for revocation online. So the binary trusts the CAs the system trusts, a company's own included, and a Linux
//! host with no CA at all cannot build a client: `build()` says «No CA certificates were loaded from the system».

use std::sync::Once;

/// A client builder with the crypto provider installed. Every option the caller sets goes on it as on reqwest's own.
pub fn builder() -> reqwest::ClientBuilder {
    static PROVIDER: Once = Once::new();
    PROVIDER.call_once(|| {
        // `Err` means a provider was already installed, the same ring or the one a host program chose: either serves.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
}
