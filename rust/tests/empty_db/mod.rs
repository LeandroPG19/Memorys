// The scratch database that starts EMPTY, shared by the test files that declare `mod empty_db;`.
// `common::in_a_scratch_database` hands out a database cloned from the migrated template; these
// files cannot take one, each for its own reason:
//   v045_databases_of_one_server_migrate_at_once   is the product test of six migrations from
//                                                  scratch at the same instant; a clone has
//                                                  nothing to migrate
//   v044_a_migration_recorded_in_crlf_still_starts asserts which checksums THIS binary writes when
//                                                  it migrates the empty database
//   v044_secure_runs_again_on_an_install_it_secured runs `secure` on a new install, and the real
//                                                  binary migrates from scratch
//   v044_a_new_app_role_does_not_get_a_published_password never migrates: the database only hosts
//                                                  the grants of its own roles, and with a schema
//                                                  create-app-role.sql would also grant on 43
//                                                  tables it does not ask about
//   v043_a_model_of_another_width_does_not_start   builds its own schema, and
//                                                  `assert_embedding_dim` reads EVERY `vector`
//                                                  column of `public`: the vector(384) of the
//                                                  template would break a model of another width
//
// It lives in a directory so cargo does not build it as a test target of its own and the
// `for file in tests/*.rs` discovery in run-all-tests.sh, which does not recurse, never hands it
// to `--test`. It has a single public item because each includer compiles its own copy and one
// that leaves an item unused trips dead_code under clippy `-D warnings` (the same rule as
// `common/mod.rs`, `dead_db/` and `ephemeral/`).

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

/// Runs `body` against an empty database created for it, on the server `DATABASE_URL` names, and
/// drops that database afterwards, also when `body` panics. The body runs in its own task so a
/// failed assertion comes back as a `JoinError` instead of unwinding past the DROP; the panic is
/// re-raised only once the database is gone.
///
/// The database has no `_sqlx_migrations` and no table: whatever migrates it, the body does.
pub async fn in_an_empty_database<F, Fut>(prefix: &str, body: F)
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
