// needs-a-server: migrates a scratch database it creates and drops, fills it and exports it with the real binary
//
//! Accounts belong to the host that issued them. A sync bundle is committed to git and pulled by
//! other machines, so nothing about accounts, devices or credentials may travel in one: the
//! bundle holds the knowledge graph and only that.
//!
//! `DATABASE_URL` names the server. Every database written here is one this file creates and
//! drops (`common::in_a_scratch_database`); the database the URL itself points at is never
//! touched, and the export reads the scratch one.

mod common;

use chrono::{DateTime, TimeZone, Utc};
use common::in_a_scratch_database;
use memory_industry::accounts::{Secret, code_key, normalize_code};
use memory_industry::accounts_db::{self as db, ClaimOutcome};
use std::path::{Path, PathBuf};
use std::process::Command;
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0)
        .single()
        .expect("a valid instant")
}

fn every_file_text(dir: &Path, into: &mut String) {
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            every_file_text(&path, into);
        } else {
            into.push_str(&path.display().to_string());
            into.push('\n');
            into.push_str(&String::from_utf8_lossy(
                &std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display())),
            ));
            into.push('\n');
        }
    }
}

#[tokio::test]
async fn an_export_carries_the_knowledge_and_nothing_about_accounts() {
    in_a_scratch_database("brain_acct_bundle", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let tag = Uuid::new_v4().simple().to_string()[..8].to_string();
        let (account, label, anchor) = (
            format!("f15acct{tag}"),
            format!("f15-device-{tag}"),
            format!("what the anchor observation says {tag}"),
        );

        let entity: Uuid = sqlx::query_scalar(
            "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
        )
        .bind(format!("anchor_{tag}"))
        .fetch_one(&pool)
        .await
        .expect("seeding an entity");
        sqlx::query(
            "INSERT INTO brain_observations (entity_id, content, observation_type, source) \
             VALUES ($1, $2, 'fact', 'agent')",
        )
        .bind(entity)
        .bind(&anchor)
        .execute(&pool)
        .await
        .expect("seeding the anchor observation");

        db::add_account(&pool, &account, t0())
            .await
            .expect("an account");
        let issued = db::issue_pairing_code(&pool, &account, Some(&label), t0())
            .await
            .expect("a pairing code");
        let key = code_key(&normalize_code(&issued.code).expect("normalized"));
        let candidates = db::pairing_candidates(&pool, t0())
            .await
            .expect("candidates");
        let candidate = candidates
            .iter()
            .find(|c| c.code_key == key)
            .expect("the code is a candidate");
        let secret = Secret::generate();
        let outcome = db::claim_pairing_code(&pool, candidate, &label, secret.hash(), t0())
            .await
            .expect("claiming");
        assert!(
            matches!(outcome, ClaimOutcome::Created(_)),
            "the setup pairs a device: {outcome:?}"
        );

        let dir: PathBuf = std::env::temp_dir().join(format!("mi-acct-bundle-{tag}"));
        std::fs::create_dir_all(&dir).expect("a directory for the bundle");
        let export = Command::new(BIN)
            .args(["sync", "export", "--scope", "all"])
            .env("DATABASE_URL", &url)
            .env("CUBA_SYNC_DIR", &dir)
            .output()
            .expect("running `memory-industry sync export`");
        assert!(
            export.status.success(),
            "the export must succeed: {}{}",
            String::from_utf8_lossy(&export.stdout),
            String::from_utf8_lossy(&export.stderr)
        );

        let mut bundle = String::new();
        every_file_text(&dir, &mut bundle);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            bundle.contains(&anchor),
            "control: the bundle holds the observation written next to the account, so the \
             absences below are absences and not an empty export"
        );
        for (what, hidden) in [
            ("the account name", account.clone()),
            ("the device label", label.clone()),
            ("the credential's secret hash", hex::encode(secret.hash())),
            ("the credential's secret", secret.to_hex()),
            ("the pairing code's key", hex::encode(key)),
            ("the pairing code", issued.code.clone()),
        ] {
            assert!(
                !bundle.contains(&hidden),
                "{what} ({hidden}) is in a sync bundle: accounts are the host's own and a bundle \
                 is committed to git and imported by other machines"
            );
        }
        pool.close().await;
    })
    .await;
}
