// `secure` connected through `db::create_pool`, which migrates, gives cuba_app
// the pgpass_app password (provision_app_role) and then steps down to cuba_app
// whenever that role logs in. Migration 0041 creates cuba_app on every server
// that migrates, so once the migrations had run the pool `secure` got back was
// cuba_app's, and `secure` refused with «tiene que correr como un rol admin»:
// on an install it had already secured, and on a fresh one too, since it
// migrates before it checks.
//
// This test runs the real binary, so unlike its neighbour in
// v044_a_new_app_role_does_not_get_a_published_password it acts on the real
// cuba_app: `secure` has no other role to act on, and roles belong to the whole
// server. What it does to that role is what every test that calls
// `db::create_pool` already does: set its password to the pgpass_app of the
// HOME this test inherits, the one a daemon of this user reads, and reimpose
// NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS, which 0041 set already. Its
// grants and default privileges live in the scratch database and go with it.
// HOME is inherited on purpose: a scratch HOME would hand `secure` a fresh
// pgpass_app, and it would move the live role to a password no daemon has.

mod common;

use std::process::{Command, Output};

use common::in_a_scratch_database;
use sqlx::{Connection, PgConnection};

/// `memory-industry secure` against `database_url`, with the step down to the
/// application role left on: CUBA_APP_ROLE=0 is what hid the refusal, and a
/// shell that exports it would make this test prove nothing.
fn secure(database_url: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memory-industry"))
        .arg("secure")
        .env("DATABASE_URL", database_url)
        .env_remove("CUBA_APP_ROLE")
        .env_remove("CUBA_SKIP_MIGRATIONS")
        .output()
        .expect("starting memory-industry secure")
}

/// The URL `secure` tells the operator to export, as it printed it.
fn printed_runtime_url(stdout: &str) -> Option<&str> {
    stdout.lines().find_map(|line| {
        line.trim()
            .strip_prefix("export DATABASE_URL=\"")?
            .strip_suffix('"')
    })
}

#[tokio::test]
async fn secure_runs_again_on_an_install_it_already_secured() {
    in_a_scratch_database("brain_secure_twice", |url| async move {
        let mut admin = PgConnection::connect(&url)
            .await
            .expect("connecting to the scratch database as the DATABASE_URL role");
        let is_super: bool =
            sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
                .fetch_one(&mut admin)
                .await
                .expect("reading whether the DATABASE_URL role is a superuser");
        assert!(
            is_super,
            "DATABASE_URL does not name a superuser, and `secure` refuses any other role before \
             it acts, fixed or not. Point it at the owner's URL (cuba)"
        );
        admin.close().await.expect("closing the admin connection");

        let first = secure(&url);
        assert!(
            first.status.success(),
            "the first `secure` on a fresh database failed. It connects through a pool that \
             steps down to cuba_app as soon as that role logs in, and migration 0041 has just \
             created cuba_app, so it ends up checking for a superuser as cuba_app. stderr:\n{}",
            String::from_utf8_lossy(&first.stderr)
        );

        let second = secure(&url);
        let stdout = String::from_utf8_lossy(&second.stdout);
        assert!(
            second.status.success(),
            "`secure` refused to run over an install it had just secured: an operator who \
             runs it again to reimpose the role's attributes is told to use the admin URL he \
             is already using. stderr:\n{}",
            String::from_utf8_lossy(&second.stderr)
        );
        assert!(
            stdout.starts_with("Rol cuba_app ya existía"),
            "the second `secure` found cuba_app in place, so it has to say the role already \
             existed and that its password is this machine's pgpass_app, set by the migration \
             that ran first as admin. stdout:\n{stdout}"
        );

        let runtime = printed_runtime_url(&stdout).unwrap_or_else(|| {
            panic!("the second `secure` printed no `export DATABASE_URL=\"…\"` line:\n{stdout}")
        });
        let mut app = PgConnection::connect(runtime).await.unwrap_or_else(|e| {
            panic!(
                "the URL the second `secure` printed does not log in: {e}. It carries \
                 pgpass_app, the password the daemon uses, so running `secure` again took \
                 that daemon down"
            )
        });
        let (who, is_super, database): (String, bool, String) = sqlx::query_as(
            "SELECT current_user::text, rolsuper, current_database()::text \
             FROM pg_roles WHERE rolname = current_user",
        )
        .fetch_one(&mut app)
        .await
        .expect("reading who the printed URL logged in as");
        app.close().await.expect("closing the cuba_app connection");

        assert_eq!(
            who,
            memory_industry::db::APP_ROLE,
            "the printed URL logs in as {who}, not as the application role"
        );
        assert!(
            !is_super,
            "the printed URL logs in as a superuser, so row-level security does not apply to \
             the daemon that uses it"
        );
        let scratch = url
            .rsplit('/')
            .next()
            .expect("a database URL ends in /<name>");
        assert_eq!(
            database, scratch,
            "the printed URL lands in {database}, not in the database `secure` was run on"
        );
    })
    .await;
}
