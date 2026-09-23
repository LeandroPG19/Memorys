// `secure` used to create the application role with `PASSWORD 'app2026'`, a
// literal that sits in this repository. Every fresh install got a LOGIN role
// with SELECT, INSERT, UPDATE and DELETE on every table and a password anyone
// who read the repo knew, which on a base listening beyond loopback is an open
// door. A role that already exists must keep its password, though: the daemon
// of every install that upgrades connects with it.
//
// Roles belong to the whole server, not to a database. `cuba_app` in a scratch
// database is the same `cuba_app` the user's real daemon logs in as when both
// share a server, so these tests never name it: each run makes up a role of its
// own, hands that name to `ensure_app_role`, and drops it once the scratch
// database (which holds its grants and default privileges) is gone.
//
// They also never call `db::create_pool`: after migrating it runs
// `provision_app_role`, which rewrites the password of the real `cuba_app`.

mod common;

use std::future::Future;

use common::in_a_scratch_database;
use memory_industry::secure_cli::{AppRole, ensure_app_role};
use sqlx::{Connection, Executor, PgConnection, PgPool};

const PUBLISHED_PASSWORD: &str = "app2026";

fn with_credentials(url: &str, role: &str, password: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("a database URL has a scheme");
    let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
    format!("{scheme}://{role}:{password}@{host}")
}

fn maintenance_url(url: &str) -> String {
    let cut = url.rfind('/').expect("a database URL ends in /<name>");
    format!("{}/postgres", &url[..cut])
}

fn fresh_secret() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

async fn logs_in(url: &str, role: &str, password: &str) -> bool {
    PgConnection::connect(&with_credentials(url, role, password))
        .await
        .is_ok()
}

/// A password is only judged if the server asks for one. A pg_hba.conf that
/// trusts the test's address lets any password in, and then «app2026 is
/// refused» could never hold, whatever the SQL did.
async fn assert_the_server_checks_passwords(url: &str, role: &str) {
    assert!(
        !logs_in(url, role, "certainly-not-its-password").await,
        "the server let {role} in with a password nobody set: pg_hba.conf trusts connections \
         from this address, so no assertion about which password works means anything here. \
         Point DATABASE_URL at a server that authenticates by password (scram-sha-256)"
    );
}

/// Runs `body` with a scratch database and a role name nothing else uses, then
/// drops the role, also when `body` panics. The role goes after the database:
/// its grants and default privileges live there and would block DROP ROLE.
async fn with_a_throwaway_role<F, Fut>(body: F)
where
    F: FnOnce(String, String) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let role = format!(
        "mi_app_probe_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let for_body = role.clone();
    let outcome = tokio::spawn(in_a_scratch_database("brain_app_role", move |url| {
        body(url, for_body)
    }))
    .await;

    if let Ok(url) = std::env::var("DATABASE_URL") {
        let mut admin = PgConnection::connect(&maintenance_url(&url))
            .await
            .expect("connecting to the maintenance database to drop the probe role");
        admin
            .execute(format!("DROP ROLE IF EXISTS {role}").as_str())
            .await
            .unwrap_or_else(|e| panic!("dropping the probe role {role}: {e}"));
    }
    if let Err(joined) = outcome {
        std::panic::resume_unwind(joined.into_panic());
    }
}

#[tokio::test]
async fn a_role_created_by_secure_does_not_open_with_the_password_in_the_repo() {
    with_a_throwaway_role(|url, role| async move {
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let secret = fresh_secret();

        let outcome = ensure_app_role(&admin, &role, &secret)
            .await
            .unwrap_or_else(|e| panic!("creating the application role {role}: {e:#}"));
        assert_eq!(
            outcome,
            AppRole::Created,
            "{role} did not exist before this call, so it had to be created"
        );

        let (is_super, bypasses): (bool, bool) =
            sqlx::query_as("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = $1")
                .bind(&role)
                .fetch_one(&admin)
                .await
                .unwrap_or_else(|e| panic!("{role} is not in pg_roles after being created: {e}"));
        assert!(
            !is_super && !bypasses,
            "{role} evades row-level security (superuser {is_super}, bypassrls {bypasses}), \
             which is the one thing the application role exists not to do"
        );

        assert!(
            logs_in(&url, &role, &secret).await,
            "{role} does not log in with the password it was created with: the daemon reads \
             that password from pgpass_app, so a role that refuses it is a daemon that falls \
             back to the superuser"
        );
        assert_the_server_checks_passwords(&url, &role).await;
        assert!(
            !logs_in(&url, &role, PUBLISHED_PASSWORD).await,
            "a newly created application role logs in with `{PUBLISHED_PASSWORD}`, the password \
             written in scripts/create-app-role.sql. Anyone who has read the repository can read \
             and rewrite every table of a base that listens beyond loopback"
        );
        admin.close().await;
    })
    .await;
}

#[tokio::test]
async fn a_role_that_already_exists_keeps_the_password_its_daemon_uses() {
    with_a_throwaway_role(|url, role| async move {
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let known = fresh_secret();
        admin
            .execute(format!("CREATE ROLE {role} LOGIN CREATEDB PASSWORD '{known}'").as_str())
            .await
            .unwrap_or_else(|e| panic!("creating {role} the way an older install left it: {e}"));
        assert!(
            logs_in(&url, &role, &known).await,
            "{role} does not log in with the password it was just created with, so this test \
             cannot tell whether ensure_app_role changed it"
        );
        assert_the_server_checks_passwords(&url, &role).await;

        let offered = fresh_secret();
        let outcome = ensure_app_role(&admin, &role, &offered)
            .await
            .unwrap_or_else(|e| panic!("running the role setup over an existing {role}: {e:#}"));
        assert_eq!(
            outcome,
            AppRole::AlreadyExisted,
            "{role} was there before the call, so nothing was created and no new password \
             should be handed to anyone"
        );

        assert!(
            logs_in(&url, &role, &known).await,
            "re-running the role setup changed the password of a role that already existed. \
             Every install that upgrades has a daemon connecting with the old one, and this \
             takes all of them down"
        );
        assert!(
            !logs_in(&url, &role, &offered).await,
            "the existing {role} now accepts the password offered for a new role, so the setup \
             rewrote it instead of leaving it alone"
        );

        let creates_databases: bool =
            sqlx::query_scalar("SELECT rolcreatedb FROM pg_roles WHERE rolname = $1")
                .bind(&role)
                .fetch_one(&admin)
                .await
                .unwrap_or_else(|e| panic!("reading the attributes of {role}: {e}"));
        assert!(
            !creates_databases,
            "the setup left CREATEDB on an existing {role}: keeping its password must not mean \
             skipping the NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS it reimposes"
        );
        admin.close().await;
    })
    .await;
}

#[test]
fn the_script_for_psql_is_the_one_the_binary_embeds() {
    let rust = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |path: std::path::PathBuf| {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
            .replace("\r\n", "\n")
    };
    let embedded = read(rust.join("embed").join("create-app-role.sql"));
    let manual = read(
        rust.parent()
            .expect("repo root")
            .join("scripts")
            .join("create-app-role.sql"),
    );
    assert!(
        embedded == manual,
        "rust/embed/create-app-role.sql, which `secure` runs, and scripts/create-app-role.sql, \
         which the migrations tell a reader to run with psql -f, have drifted apart. A fix to \
         one of them leaves the other creating the role the old way"
    );
}
