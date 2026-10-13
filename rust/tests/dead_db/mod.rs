// The database that is down on purpose, shared by the test files that declare `mod dead_db;`.
// It lives in a directory so cargo does not build it as a test target of its own and the
// `for file in tests/*.rs` discovery, which does not recurse, never hands it to `--test`. It has
// a single public item because each includer compiles its own copy and one that leaves an item
// unused trips dead_code under clippy `-D warnings` (the same rule as `common/mod.rs`).
//
// Why a listener that hangs up and not a port nobody listens on (`127.0.0.1:1`): sqlx 0.8 treats
// a refused connection as "the server is starting" and retries it with backoff until
// `acquire_timeout` (5 s, `db.rs`), then returns `PoolTimedOut`. That is sqlx, not Windows, so it
// costs the same on Linux. Measured with `memory-industry serve` against three dead databases:
//   127.0.0.1:1, nobody listening        7.04 s  PoolTimedOut (2 s host identity + 5 s pool)
//   listener that accepts and closes     0.02 s  Error::Io, sqlx does not retry it
//   listener that accepts and stays mute 7.03 s  PoolTimedOut
// Accepting and closing is the only one that fails at once and still fails for the right reason
// (`v042::the_dead_database_hangs_up_at_once_instead_of_timing_out` pins both).
//
// The listener is process-wide and made of std, not tokio: every `#[tokio::test]` has a runtime of
// its own that dies with the test, and the child processes that `cli_contract` and
// `v029_auto_host_serve_accepts_lan` spawn must still find it. The host is the literal
// `127.0.0.1`, never `localhost`: with the listener on IPv4 only, each connection would lose more
// than a second trying `::1` first.

use std::net::TcpListener;
use std::sync::OnceLock;

static PORT: OnceLock<u16> = OnceLock::new();

/// The URL of a PostgreSQL that is not there. The password is the canary that
/// `v042::health_never_prints_a_secret` looks for in every response.
pub fn url() -> String {
    let port = *PORT.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0")
            .expect("the dead database needs a free port on 127.0.0.1 to listen on");
        let port = listener
            .local_addr()
            .expect("the dead database's listener has no local address")
            .port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        port
    });
    format!("postgresql://cuba:canarypass-db@127.0.0.1:{port}/nowhere")
}
