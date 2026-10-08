use std::net::TcpListener;
use std::time::Duration;

/// 192.0.2.0/24 is TEST-NET-1 (RFC 5737): reserved for documentation, so no
/// machine owns an address in it. It stands for the LAN address a Windows box
/// has not been given yet right after a reboot.
const UNASSIGNED_IP: &str = "192.0.2.1";

/// Generous for a bind that must fail at once; far short of the five minutes a
/// daemon that ignored `MEMORY_INDUSTRY_BIND_WAIT_SECS=0` would spend.
const MUST_RETURN_WITHIN: Duration = Duration::from_secs(5);

/// A port nobody listens on right now, found the same way a test finds any.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("the loopback accepts a bind on a port the system picks")
        .local_addr()
        .unwrap()
        .port()
}

/// The test is only about an address the machine does not have. A machine that
/// lets any address be bound (`net.ipv4.ip_nonlocal_bind=1`) cannot show that,
/// and passing there would prove nothing, so it fails and says why — it never
/// skips.
fn require_an_address_the_machine_does_not_have(addr: &str) {
    match TcpListener::bind(addr) {
        Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {}
        Ok(_) => panic!(
            "{addr} could be bound on this machine, so it is not an address this machine \
             lacks. This test needs one: this machine allows non-local binds \
             (net.ipv4.ip_nonlocal_bind=1 on Linux). Turn that off for the run; the test does \
             not skip, because a skipped test would report green on the exact machine that \
             cannot exercise it"
        ),
        Err(e) => panic!(
            "binding {addr} failed with {e} ({:?}), not with AddrNotAvailable, so this test \
             cannot tell «the address is not mine» from anything else on this machine",
            e.kind()
        ),
    }
}

#[tokio::test]
async fn serve_returns_the_not_assigned_error_instead_of_hanging_on_an_ip_the_machine_lacks() {
    let addr = format!("{UNASSIGNED_IP}:{}", free_port());
    require_an_address_the_machine_does_not_have(&addr);

    unsafe {
        std::env::set_var("CUBA_HTTP_TOKEN", "t".repeat(64));
        std::env::set_var("MEMORY_INDUSTRY_BIND_WAIT_SECS", "0");
        std::env::remove_var("CUBA_BIND_WAIT_SECS");
        std::env::remove_var("CUBA_PEER_TOKEN");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_PID");
    }
    let pool = memory_industry::db::create_lazy_pool("postgres://nobody@127.0.0.1/none");

    let outcome = tokio::time::timeout(
        MUST_RETURN_WITHIN,
        memory_industry::http::serve_pool(&addr, pool, false),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "serve_pool on {addr} was still waiting for the address after \
                     {MUST_RETURN_WITHIN:?} with MEMORY_INDUSTRY_BIND_WAIT_SECS=0: the bind \
                     ignores the window the operator set (or has no way out of its loop)"
        )
    });

    let refusal = format!(
        "{:#}",
        outcome.expect_err("an address the machine lacks cannot be served")
    );
    assert!(
        refusal.contains("is not assigned to this machine"),
        "the process exits 1 with this sentence and the supervisor restarts it; it used to say \
         «is another daemon already running?» about an IP that simply was not there yet, which \
         sent the operator hunting for a process that did not exist: {refusal}"
    );
    assert!(
        refusal.contains(&addr),
        "the operator has to see which address the machine lacks: {refusal}"
    );
}
