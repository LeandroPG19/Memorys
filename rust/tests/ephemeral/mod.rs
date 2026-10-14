// The port of a test's daemon, asked of the OS and never written down. Shared by the test files
// that declare `mod ephemeral;`. It lives in a directory so cargo does not build it as a test target
// of its own, and it has a single public item because each includer compiles its own copy and one
// that leaves an item unused trips dead_code under clippy `-D warnings` (the same rule as
// `dead_db/mod.rs` and `common/mod.rs`).
//
// Why not a number in the test: the judge runs once on Windows and once on WSL, and with
// `networkingMode=mirrored` both share one port table. The ephemeral ranges measured on 2026-10-14
// are Windows 49152-65535 and WSL 44620-48715, disjoint, so an OS-chosen port cannot land on a
// socket of the other host. The fixed ports the tests used (18797-18891) sit outside both ranges,
// so only another fixed-port test could hold one, and that is what happened: the Windows judge left
// 127.0.0.1:18853 in TIME_WAIT and the WSL judge, a minute later, got `os error 98` (about 70 s).
//
// What is left is the gap between this function dropping the listener and `serve_pool` binding the
// port again: the milliseconds that `parse_listen`, `host_info` and the router take. If the OS hands
// the port to another socket in that gap the daemon says "already in use" and the test fails
// readable; each caller asks for it on the line before its `tokio::spawn`, inside the section that
// already serialises daemon starts, so it can never be another daemon of the same binary. If that
// ever shows up in a judge, the way out is `serve_pool_on(listener, ..)` in `http.rs`, which takes
// the bound listener instead of a number.
//
// There is no retry: nothing known makes `bind` on port zero fail with AddrInUse, and retrying
// there would hide a real cause.

use std::net::TcpListener;

/// A port on 127.0.0.1 that the OS chose and that is free at the moment this returns.
pub fn port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0")
        .expect("the test's daemon needs the OS to give it a free port on 127.0.0.1");
    listener
        .local_addr()
        .expect("the listener that asked the OS for a port has no local address")
        .port()
}
