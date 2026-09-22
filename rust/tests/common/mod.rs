// Shared by the test files that declare `mod common;`. It lives in a
// directory so cargo does not build it as a test target of its own, and so
// the `for file in tests/*.rs` discovery in run-all-tests.sh and ci.yml, which
// does not recurse, never hands it to `--test`. Everything here has to be used
// by every file that includes it: each of them compiles its own copy, and an
// item one of them leaves unused is a dead_code warning that clippy
// `-D warnings` turns into a failed gate.

use std::future::Future;

use sqlx::{Connection, Executor, PgConnection};

fn database_url() -> String {
    std::env::var("DATABASE_URL").expect(
        "DATABASE_URL is required: this test creates and drops its own database on the server \
         that URL names, and never writes to the database the URL itself points at",
    )
}

fn sibling_url(url: &str, name: &str) -> String {
    let cut = url.rfind('/').expect("a database URL ends in /<name>");
    format!("{}/{name}", &url[..cut])
}

/// Runs `body` against a database created for it, on the server `DATABASE_URL`
/// names, and drops that database afterwards, also when `body` panics. The body
/// runs in its own task so a failed assertion comes back as a `JoinError`
/// instead of unwinding past the DROP; the panic is re-raised only once the
/// database is gone. The precedent in v037 drops before asserting, but an
/// `expect` that fires earlier than that would still leave its database behind.
///
/// The database starts empty. A body that needs the schema migrates it with
/// `db::create_pool`, which runs the embedded migrator on whatever it connects
/// to, exactly as it does on the gate's brain_gate.
pub async fn in_a_scratch_database<F, Fut>(prefix: &str, body: F)
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let url = database_url();
    let scratch = format!("{prefix}_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let mut admin = PgConnection::connect(&sibling_url(&url, "postgres"))
        .await
        .expect("connecting to the maintenance database");
    admin
        .execute(format!("CREATE DATABASE {scratch}").as_str())
        .await
        .expect("creating the scratch database");

    let outcome = tokio::spawn(body(sibling_url(&url, &scratch))).await;

    admin
        .execute(format!("DROP DATABASE IF EXISTS {scratch} WITH (FORCE)").as_str())
        .await
        .expect("dropping the scratch database");
    if let Err(joined) = outcome {
        std::panic::resume_unwind(joined.into_panic());
    }
}
