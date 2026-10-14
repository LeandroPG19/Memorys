// needs-a-server: migrates scratch databases it creates and drops, and runs `doctor` against them in a world of its own
//
//! What `doctor` says about accounts and the schema (G16), and that a `doctor` run from a test
//! reads only the world the test built (T1).
//!
//! These four tests used to live in `v029_accounts_over_real_http.rs`. `DoctorWorld` changes
//! variables of the PROCESS (HOME, USERPROFILE, LOCALAPPDATA, CUBA_HTTP_ADDR, and removes others)
//! while the other tests of that binary run in parallel and read HOME (the password of `cuba_app`
//! comes from a file under it) without taking `ENV_GUARD`. A lock cannot protect whoever reads
//! without taking it, so on the Windows judge seven of them failed with `password authentication
//! failed for user "cuba_app"` and a 503. A binary is a process: here nothing else reads the
//! environment while a world is up, and `ENV_GUARD` only orders the four tests among themselves.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched.

mod common;
mod doctor_world;

use chrono::Utc;
use common::in_a_scratch_database;
use doctor_world::DoctorWorld;
use memory_industry::accounts::{Secret, code_key, fingerprint, normalize_code};
use memory_industry::accounts_db::{self as db, ClaimOutcome};
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

static ENV_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// `migrated`, `account` and `paired` are copies of the helpers of the same name in
// `v029_accounts_over_real_http.rs`. They are not in `common/` because each file that includes it
// compiles its own copy, and an item one of them leaves unused is a dead_code warning that clippy
// `-D warnings` turns into a failed gate; the other binaries use none of the three.

async fn migrated(url: &str) -> PgPool {
    memory_industry::db::create_pool(url)
        .await
        .expect("migrating the scratch database")
}

async fn account(pool: &PgPool, name: &str) {
    db::add_account(pool, name, Utc::now())
        .await
        .unwrap_or_else(|e| panic!("add_account({name}) must succeed: {e:#}"));
}

/// A device paired straight through the storage layer, as of now. Returns its credential id and
/// the secret the "client" generated.
async fn paired(
    pool: &PgPool,
    account_name: &str,
    host_label: Option<&str>,
    label: &str,
) -> (Uuid, Secret) {
    let now = Utc::now();
    let issued = db::issue_pairing_code(pool, account_name, host_label, now)
        .await
        .unwrap_or_else(|e| panic!("issue_pairing_code({account_name}) must succeed: {e:#}"));
    let key = code_key(&normalize_code(&issued.code).expect("an issued code normalizes"));
    let candidates = db::pairing_candidates(pool, now)
        .await
        .expect("pairing_candidates");
    let candidate = candidates
        .iter()
        .find(|c| c.code_key == key)
        .expect("the code just issued is among the candidates");
    let secret = Secret::generate();
    match db::claim_pairing_code(pool, candidate, label, secret.hash(), now)
        .await
        .expect("claim_pairing_code")
    {
        ClaimOutcome::Created(created) => (created.credential_id, secret),
        other => panic!("a fresh code for {account_name} must be Created, got {other:?}"),
    }
}

fn accounts_check(checks: &[memory_industry::doctor::Check]) -> &memory_industry::doctor::Check {
    let found: Vec<_> = checks.iter().filter(|c| c.name == "accounts").collect();
    assert_eq!(
        found.len(),
        1,
        "doctor reports exactly one `accounts` check, and it reported {found:?} among {:?}",
        checks.iter().map(|c| &c.name).collect::<Vec<_>>()
    );
    found[0]
}

#[tokio::test]
async fn g16_account_facts_count_what_the_database_holds_and_doctor_prints_them() {
    in_a_scratch_database("brain_g_facts", |url_| async move {
        let _env = ENV_GUARD.lock().await;
        let mut world = DoctorWorld::new("g16-facts");
        let pool = migrated(&url_).await;
        let host_id = db::host_identity(&pool)
            .await
            .expect("host identity")
            .host_id;

        let fresh = world.run_checks(&pool, &url_).await;
        let check = accounts_check(&fresh);
        assert_eq!(
            check.status,
            memory_industry::doctor::Status::Ok,
            "a host with no accounts is healthy: {}",
            check.detail
        );
        assert!(
            check.detail.contains("sin cuentas") && check.detail.contains(&fingerprint(&host_id)),
            "and says so, with the fingerprint a client will be shown: {}",
            check.detail
        );

        for name in ["ana", "bob", "carol"] {
            account(&pool, name).await;
        }
        paired(&pool, "ana", None, "pc-1").await;
        paired(&pool, "ana", None, "pc-2").await;
        paired(&pool, "ana", None, "pc-3").await;
        paired(&pool, "bob", None, "pc-4").await;
        paired(&pool, "carol", None, "pc-5").await;
        db::revoke_device(&pool, "ana", "pc-3", Utc::now())
            .await
            .expect("revoking a device");
        db::revoke_account(&pool, "carol", Utc::now())
            .await
            .expect("revoking an account");

        let facts = db::account_facts(&pool).await.expect("account_facts");
        assert!(facts.tables_present, "the tables are there: {facts:?}");
        assert_eq!(
            facts.accounts, 2,
            "ana and bob: carol is revoked and no longer counts as an account that can come in"
        );
        assert_eq!(
            facts.active_credentials, 3,
            "pc-1, pc-2 and pc-4: neither a revoked device nor a revoked account's device is active"
        );
        assert_eq!(
            facts.revoked_credentials, 2,
            "pc-3 by its own revocation and pc-5 by its account's"
        );
        assert_eq!(facts.fingerprint, fingerprint(&host_id));

        let checks = world.run_checks(&pool, &url_).await;
        let check = accounts_check(&checks);
        assert!(
            check.detail.contains(&fingerprint(&host_id)),
            "doctor prints the fingerprint from the database, not a placeholder: {}",
            check.detail
        );
        assert!(
            !check.detail.contains("sin cuentas"),
            "and no longer claims there are no accounts: {}",
            check.detail
        );
        let printed = format!(
            "{} {}",
            check.detail,
            check.hint.clone().unwrap_or_default()
        );
        assert!(
            !printed.contains("mi1."),
            "doctor output is pasted into issues and chats: {printed}"
        );
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g16_a_database_without_the_account_tables_is_a_doctor_failure_not_a_crash() {
    in_a_scratch_database("brain_g_no_tables", |url_| async move {
        let _env = ENV_GUARD.lock().await;
        let mut world = DoctorWorld::new("g16-no-tables");
        let pool = migrated(&url_).await;
        let mut owner = PgConnection::connect(&url_)
            .await
            .expect("the owner connection");
        sqlx::query(
            "DROP TABLE brain_pairing_codes, brain_account_credentials, brain_accounts, \
             brain_host_identity",
        )
        .execute(&mut owner)
        .await
        .expect("removing the four account tables, as a database from before 0065 is");

        let facts = db::account_facts(&pool)
            .await
            .expect("an old database is a fact to report, not an error to raise");
        assert!(
            !facts.tables_present,
            "account_facts must say the tables are missing: {facts:?}"
        );

        let checks = world.run_checks(&pool, &url_).await;
        let check = accounts_check(&checks);
        assert_eq!(check.status, memory_industry::doctor::Status::Fail);
        assert!(
            check.detail.contains("0065"),
            "and names the migration: {}",
            check.detail
        );
        owner.close().await.expect("closing the owner connection");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g16_the_schema_check_counts_the_account_tables() {
    in_a_scratch_database("brain_g_schema_count", |url_| async move {
        let _env = ENV_GUARD.lock().await;
        let mut world = DoctorWorld::new("g16-schema");
        let pool = migrated(&url_).await;
        let schema = |checks: Vec<memory_industry::doctor::Check>| {
            checks
                .into_iter()
                .find(|c| c.name == "schema")
                .expect("doctor always reports `schema`")
        };

        let complete = schema(world.run_checks(&pool, &url_).await);
        assert_eq!(
            complete.status,
            memory_industry::doctor::Status::Ok,
            "a freshly migrated database has every table doctor expects: {}",
            complete.detail
        );

        let mut owner = PgConnection::connect(&url_)
            .await
            .expect("the owner connection");
        sqlx::query("DROP TABLE brain_pairing_codes")
            .execute(&mut owner)
            .await
            .expect("removing one of the four tables 0065 added");
        let short = schema(world.run_checks(&pool, &url_).await);
        assert_eq!(
            short.status,
            memory_industry::doctor::Status::Fail,
            "with one table of 0065 missing the schema check has to fail. It passed, which means \
             EXPECTED_TABLES still counts the database as it was before 0065 and a daemon whose \
             database never took the migration would be called healthy: {}",
            short.detail
        );
        owner.close().await.expect("closing the owner connection");
        pool.close().await;
    })
    .await;
}

#[tokio::test]
async fn g16_doctor_reads_only_the_world_the_test_built() {
    in_a_scratch_database("brain_g_world", |url_| async move {
        const CANARY: &str = "MEMORY_INDUSTRY_T1_CANARY";
        const TOUCHED: [&str; 5] = [
            "HOME",
            "USERPROFILE",
            "LOCALAPPDATA",
            "CUBA_HTTP_ADDR",
            "CUBA_RERANKER_PATH",
        ];
        let _env = ENV_GUARD.lock().await;
        let pool = migrated(&url_).await;
        let before: Vec<_> = TOUCHED.iter().map(std::env::var_os).collect();
        let mut world = DoctorWorld::new("g16-world");

        // Spelled out by hand, not through `llm_cli::config_path`: with the product's own
        // function a sabotage of it would move the reader and the fixture together.
        let config_dir = if cfg!(windows) {
            world.root().join("home").join("AppData").join("Roaming")
        } else {
            world.root().join("home").join(".config")
        }
        .join("memory-industry");
        std::fs::create_dir_all(&config_dir).expect("the world's config directory is creatable");
        let llm_env = config_dir.join("llm.env");
        std::fs::write(&llm_env, format!("{CANARY}=planted
")).expect("planting the llm.env");

        assert_eq!(
            memory_industry::llm_cli::config_path().expect("HOME answers inside the world"),
            llm_env,
            "the planted file has to be the one doctor would read; anywhere else, the canary              check below would pass without doctor ever reaching the file"
        );
        assert!(
            std::env::var_os(CANARY).is_none(),
            "the canary is in the environment before doctor ran, so its absence afterwards              would prove nothing"
        );

        let checks = world.run_checks(&pool, &url_).await;

        let leaked = std::env::var_os(CANARY);
        unsafe { std::env::remove_var(CANARY) };
        assert!(
            leaked.is_none(),
            "doctor wrote the llm.env of the home into the process environment ({leaked:?});              that is how the developer's own llm.env gets into every other test of this binary"
        );

        let check = |name: &str| {
            checks
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("doctor reports a `{name}` check, among {checks:?}"))
        };
        let root = world.root().display().to_string();
        assert!(
            check("service_install").detail.contains(&root),
            "the service check names a path outside the world, so it read the machine's own              install: {}",
            check("service_install").detail
        );
        assert!(
            check("legacy_cache").detail.contains(&root),
            "the legacy cache check names a path outside the world, so it read the machine's              own home: {}",
            check("legacy_cache").detail
        );

        let addr = std::env::var("CUBA_HTTP_ADDR").expect("the world defines the daemon address");
        assert!(addr.starts_with("127.0.0.1:"), "{addr}");
        assert!(
            check("http_port").detail.contains(&addr),
            "the port check looks at an address that is not the world's, so it follows the              machine's CUBA_HTTP_ADDR or the default: {}",
            check("http_port").detail
        );
        let lan = &check("lan").detail;
        assert!(
            lan.contains(&addr) && lan.contains("loopback"),
            "a loopback address never sends doctor out to ask a daemon: {lan}"
        );

        assert!(
            check("graph_db").detail.starts_with("projection disabled"),
            "the graph backend of the machine reached doctor: {}",
            check("graph_db").detail
        );
        assert!(
            !check("generative_llm").detail.contains("openai_compat"),
            "the LLM provider of the machine reached doctor: {}",
            check("generative_llm").detail
        );

        let gone = world.root().to_path_buf();
        drop(world);
        let after: Vec<_> = TOUCHED.iter().map(std::env::var_os).collect();
        assert_eq!(
            after, before,
            "the world left the process environment different from how it found it"
        );
        assert!(
            !gone.exists(),
            "the world left its directory behind: {}",
            gone.display()
        );
        pool.close().await;
    })
    .await;
}
