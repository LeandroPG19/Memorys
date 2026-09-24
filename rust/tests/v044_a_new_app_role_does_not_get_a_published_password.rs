// needs-a-server: creates and drops login roles as a superuser, on a server that checks passwords
//
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
use sqlx::{Connection, Executor, PgConnection, PgPool, Row};

const PUBLISHED_PASSWORD: &str = "app2026";
const CREATE_APP_ROLE_SQL: &str = include_str!("../embed/create-app-role.sql");

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

/// What the daemon does each time it starts as admin and migrates: it gives the
/// application role the password in pgpass_app, so a role still opening with an
/// older one (`app2026`, or whatever an earlier run left there) moves to the one
/// the daemon is about to log in with. Nothing watched that happen: replacing
/// `provision_app_role`, `write_app_role_password` or `bind_app_role` with a
/// no-op left every test green, because the password on the gate's server is
/// whatever an earlier run left and nobody asked which one opens.
#[tokio::test]
async fn provisioning_moves_an_existing_role_onto_the_new_password_and_off_the_old_one() {
    with_a_throwaway_role(|url, role| async move {
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let old = fresh_secret();
        admin
            .execute(format!("CREATE ROLE {role} LOGIN PASSWORD '{old}'").as_str())
            .await
            .unwrap_or_else(|e| panic!("creating {role} with the password it had before: {e}"));
        assert!(
            logs_in(&url, &role, &old).await,
            "{role} does not log in with the password it was just created with, so this test \
             cannot tell whether provisioning moved it"
        );
        assert_the_server_checks_passwords(&url, &role).await;

        let new = fresh_secret();
        memory_industry::db::provision_app_role(&admin, &role, &new).await;

        assert!(
            logs_in(&url, &role, &new).await,
            "after provisioning, {role} does not log in with the password it was given. That \
             password is pgpass_app, the one the daemon reads, so a role that refuses it is a \
             daemon that falls back to the superuser connection"
        );
        assert!(
            !logs_in(&url, &role, &old).await,
            "after provisioning, {role} still opens with its old password: an install that \
             started on `{PUBLISHED_PASSWORD}` keeps a door anyone who read the repository can \
             open"
        );
        admin.close().await;
    })
    .await;
}

/// An iteration count the server never picks on its own. Set as the scratch
/// database's `scram_iterations`, it is the count the server hashes with when
/// a password reaches CREATE ROLE or ALTER ROLE in the clear, while a
/// verifier the binary computed arrives with its own 4096 and is stored as it
/// is. The count in pg_authid then says who hashed the password, and so
/// whether the password itself travelled to the server.
const SERVER_SIDE_ITERATIONS: &str = "1234";

/// The admin pool on `url` after setting the scratch database's
/// `scram_iterations` to SERVER_SIDE_ITERATIONS. The pool is opened after the
/// ALTER DATABASE, so every session it hands out already has the setting.
async fn admin_pool_that_marks_server_side_hashing(url: &str) -> PgPool {
    let mut setup = PgConnection::connect(url)
        .await
        .expect("connecting to the scratch database to set its scram_iterations");
    setup
        .execute(
            format!(
                "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I SET scram_iterations = {SERVER_SIDE_ITERATIONS}', current_database()); END $$"
            )
            .as_str(),
        )
        .await
        .expect("setting scram_iterations on the scratch database (PostgreSQL 16 or later)");
    setup
        .close()
        .await
        .expect("closing the connection that set scram_iterations");

    let admin = PgPool::connect(url)
        .await
        .expect("connecting to the scratch database as the admin role");
    let live: String = sqlx::query_scalar("SELECT current_setting('scram_iterations')")
        .fetch_one(&admin)
        .await
        .expect("reading scram_iterations in the admin pool");
    assert_eq!(
        live, SERVER_SIDE_ITERATIONS,
        "the admin pool does not run with the scram_iterations set on its database, so the \
         iteration count cannot tell a password the server hashed from a verifier the binary \
         computed"
    );
    admin
}

/// What pg_authid keeps for `role`: the verifier the server checks a login
/// against, or None for a role without a password. Only a superuser reads
/// pg_authid, and only a superuser runs these tests.
async fn stored_verifier(pool: &PgPool, role: &str) -> Option<String> {
    sqlx::query_scalar("SELECT rolpassword FROM pg_authid WHERE rolname = $1")
        .bind(role)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("reading what pg_authid keeps for {role}: {e}"))
}

/// The iteration count and the Base64 salt of a verifier in the form
/// PostgreSQL stores it, `SCRAM-SHA-256$<iterations>:<salt>$<StoredKey>:<ServerKey>`,
/// or None for anything else.
fn iterations_and_salt(verifier: &str) -> Option<(&str, &str)> {
    let (head, _keys) = verifier.strip_prefix("SCRAM-SHA-256$")?.split_once('$')?;
    head.split_once(':')
}

/// The password the daemon provisions used to reach the server in the clear,
/// inside `ALTER ROLE ... PASSWORD`, and the bound setting that carried it
/// there: log_statement, log_min_duration_statement and pg_stat_statements
/// can all keep a copy of either. The binary now computes the SCRAM-SHA-256
/// verifier and hands the server only that, which PostgreSQL stores as it is
/// («If the presented password string is already in MD5-encrypted or
/// SCRAM-encrypted format, then it is stored as-is regardless of
/// password_encryption», CREATE ROLE). Whoever hashed it shows in the stored
/// iteration count.
#[tokio::test]
async fn provisioning_hands_the_server_a_verifier_and_never_the_password() {
    with_a_throwaway_role(|url, role| async move {
        let admin = admin_pool_that_marks_server_side_hashing(&url).await;
        let old = fresh_secret();
        admin
            .execute(format!("CREATE ROLE {role} LOGIN PASSWORD '{old}'").as_str())
            .await
            .unwrap_or_else(|e| panic!("creating {role} with a password in the clear: {e}"));
        let hashed_by_the_server = stored_verifier(&admin, &role).await;
        assert_eq!(
            hashed_by_the_server
                .as_deref()
                .and_then(iterations_and_salt)
                .map(|(iterations, _)| iterations),
            Some(SERVER_SIDE_ITERATIONS),
            "a password handed over in the clear should be hashed here with \
             {SERVER_SIDE_ITERATIONS} iterations, but {role} stored {hashed_by_the_server:?}. \
             Without that the count cannot say who hashed a password (is password_encryption \
             scram-sha-256 on this server?)"
        );
        assert_the_server_checks_passwords(&url, &role).await;

        let new = fresh_secret();
        memory_industry::db::provision_app_role(&admin, &role, &new).await;

        let first = stored_verifier(&admin, &role).await;
        let (iterations, salt) = first
            .as_deref()
            .and_then(iterations_and_salt)
            .unwrap_or_else(|| {
                panic!("after provisioning, {role} keeps {first:?}, not a SCRAM-SHA-256 verifier")
            });
        assert_eq!(
            iterations, "4096",
            "after provisioning, {role} keeps a verifier of {iterations} iterations, the count \
             this server hashes with: the password reached the server in the clear, where the \
             statement log and pg_stat_statements can keep it, instead of the verifier the \
             binary computes with 4096"
        );
        assert!(
            salt.len() == 24 && salt.ends_with("=="),
            "the salt of the verifier is {salt:?}, which is not 16 bytes in Base64 (24 \
             characters ending in ==), the salt length PostgreSQL uses"
        );
        assert!(
            logs_in(&url, &role, &new).await,
            "{role} does not log in with the password its verifier was computed from, so the \
             server cannot check that verifier and the daemon falls back to the superuser"
        );
        assert!(
            !logs_in(&url, &role, &old).await,
            "after provisioning, {role} still opens with its old password"
        );

        memory_industry::db::provision_app_role(&admin, &role, &new).await;
        let second = stored_verifier(&admin, &role).await;
        assert_ne!(
            first, second,
            "provisioning the same password twice stored the same verifier: the salt is not \
             drawn afresh, so one precomputed table opens every install with that password"
        );
        assert!(
            logs_in(&url, &role, &new).await,
            "{role} does not log in with its password after the second provisioning"
        );
        admin.close().await;
    })
    .await;
}

/// `secure` creates the role through the embedded script, which put the
/// password it was handed into CREATE ROLE. It hands the script the verifier
/// now, and the role keeps exactly that.
#[tokio::test]
async fn a_role_created_by_secure_keeps_the_verifier_the_binary_computed() {
    with_a_throwaway_role(|url, role| async move {
        let admin = admin_pool_that_marks_server_side_hashing(&url).await;
        let secret = fresh_secret();

        let outcome = ensure_app_role(&admin, &role, &secret)
            .await
            .unwrap_or_else(|e| panic!("creating the application role {role}: {e:#}"));
        assert_eq!(
            outcome,
            AppRole::Created,
            "{role} did not exist before this call, so it had to be created"
        );

        let stored = stored_verifier(&admin, &role).await;
        assert_eq!(
            stored
                .as_deref()
                .and_then(iterations_and_salt)
                .map(|(iterations, _)| iterations),
            Some("4096"),
            "{role} was created with {stored:?}: a count of {SERVER_SIDE_ITERATIONS} means the \
             server hashed a password that reached CREATE ROLE in the clear, instead of \
             storing the verifier the binary computes with 4096"
        );
        assert!(
            logs_in(&url, &role, &secret).await,
            "{role} does not log in with the password its verifier was computed from"
        );
        assert_the_server_checks_passwords(&url, &role).await;
        admin.close().await;
    })
    .await;
}

/// Run by hand through psql, the script took the password itself and put it
/// into CREATE ROLE. It takes a verifier now, and refuses anything else
/// before it creates a role: accepting a password in the clear would keep
/// the manual path sending it to the server. The refusal must not repeat the
/// value either, or the server log gets the password from the error.
#[tokio::test]
async fn the_script_refuses_a_password_in_the_clear() {
    with_a_throwaway_role(|url, role| async move {
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let secret = fresh_secret();

        let mut tx = admin
            .begin()
            .await
            .expect("opening the transaction the script runs in");
        let setup = sqlx::Executor::execute(
            &mut *tx,
            sqlx::query(
                "SELECT set_config('memory_industry.app_role', $1, true), \
                        set_config('memory_industry.app_password', $2, true)",
            )
            .bind(&role)
            .bind(&secret),
        )
        .await;
        let script = sqlx::Executor::execute(&mut *tx, sqlx::raw_sql(CREATE_APP_ROLE_SQL)).await;
        let rolled_back = tx.rollback().await;

        rolled_back.expect("rolling back the transaction the script ran in");
        setup.expect("setting the role and the password for the transaction");
        let error = match script {
            Ok(_) => panic!(
                "embed/create-app-role.sql created {role} from a password in the clear: that \
                 password goes into CREATE ROLE, where the statement log and \
                 pg_stat_statements can keep it. The script has to take a SCRAM-SHA-256 \
                 verifier and refuse anything else"
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            error.contains("SCRAM-SHA-256"),
            "the script refused a password in the clear, but its error does not say that it \
             wants a SCRAM-SHA-256 verifier, so nobody reading it knows what to hand it: {error}"
        );
        assert!(
            !error.contains(&secret),
            "the refusal repeats the password it was handed, and the server log keeps errors: \
             {error}"
        );
        admin.close().await;
    })
    .await;
}

/// The real application role as pg_roles shows it (the password is masked
/// there), or None when the server has none.
async fn the_real_app_role(pool: &PgPool) -> Option<String> {
    sqlx::query_scalar("SELECT r::text FROM pg_roles r WHERE rolname = $1")
        .bind(memory_industry::db::APP_ROLE)
        .fetch_optional(pool)
        .await
        .expect("reading the real application role from pg_roles")
}

/// A caller that forgets `bind_app_role` used to get `cuba_app`: the script
/// fell back to it when `memory_industry.app_role` was unset, and so acted on
/// the role the user's real daemon logs in as. It has to refuse instead.
///
/// With the fallback still in place this test runs the script against the real
/// `cuba_app`. That is why everything happens inside one transaction that is
/// rolled back before any assertion can panic: CREATE ROLE, ALTER ROLE and
/// GRANT are all transactional in PostgreSQL, so nothing the script did
/// outlives the rollback, and a process killed halfway leaves an uncommitted
/// transaction that the server aborts. The last assertions then read the real
/// role again and require it unchanged.
#[tokio::test]
async fn the_script_refuses_to_run_when_it_is_not_told_which_role() {
    with_a_throwaway_role(|url, role| async move {
        let admin = PgPool::connect(&url)
            .await
            .expect("connecting to the scratch database as the admin role");
        let before = the_real_app_role(&admin).await;

        // Only the password is handed over, so the one thing missing is the role
        // name and the refusal cannot be the one about the password.
        let mut tx = admin
            .begin()
            .await
            .expect("opening the transaction the script runs in");
        let setup = sqlx::Executor::fetch_one(
            &mut *tx,
            sqlx::query(
                "SELECT current_setting('memory_industry.app_role', true),                         set_config('memory_industry.app_password', $1, true)",
            )
            .bind(fresh_secret()),
        )
        .await;
        let script = sqlx::Executor::execute(&mut *tx, sqlx::raw_sql(CREATE_APP_ROLE_SQL)).await;
        let rolled_back = tx.rollback().await;

        rolled_back.expect("rolling back the transaction the script ran in");
        let role_setting: Option<String> = setup
            .expect("setting memory_industry.app_password for the transaction")
            .try_get(0)
            .expect("reading memory_industry.app_role");
        assert!(
            role_setting.as_deref().unwrap_or_default().is_empty(),
            "memory_industry.app_role was already {role_setting:?} before the script ran (set              on the server, the database or this role), so this test cannot show what the              script does when nobody names the role"
        );
        assert_eq!(
            the_real_app_role(&admin).await,
            before,
            "the real {} is not what it was before the test: the script touched the role              the user's daemon logs in as",
            memory_industry::db::APP_ROLE
        );
        let probe_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
                .bind(&role)
                .fetch_one(&admin)
                .await
                .unwrap_or_else(|e| panic!("looking {role} up in pg_roles: {e}"));
        assert!(
            !probe_exists,
            "{role} exists, but its name was never handed to the script"
        );

        let error = match script {
            Ok(_) => panic!(
                "embed/create-app-role.sql ran to the end without memory_industry.app_role:                  with no role named it falls back to {}, the role the user's real daemon                  logs in as, so code that forgets bind_app_role alters the live role instead                  of failing",
                memory_industry::db::APP_ROLE
            ),
            Err(e) => e.to_string(),
        };
        assert!(
            error.contains("memory_industry.app_role"),
            "the script refused to run without a role, but its error does not name the              setting that was missing, so nobody reading it knows what to set: {error}"
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
