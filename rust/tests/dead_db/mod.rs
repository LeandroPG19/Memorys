// The database that is down on purpose, shared by the test files that declare `mod dead_db;`.
// It lives in a directory so cargo does not build it as a test target of its own and the
// `for file in tests/*.rs` discovery, which does not recurse, never hands it to `--test`. It has
// a single public item because each includer compiles its own copy and one that leaves an item
// unused trips dead_code under clippy `-D warnings` (the same rule as `common/mod.rs`).

/// The URL of a PostgreSQL that is not there. The password is the canary that
/// `v042::health_never_prints_a_secret` looks for in every response.
pub fn url() -> String {
    "postgresql://cuba:canarypass-db@127.0.0.1:1/nowhere".to_string()
}
