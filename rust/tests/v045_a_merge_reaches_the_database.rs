// needs-a-server: creates two scratch databases and runs the real sync and git merge hooks against them
//
//! Two machines, one repo: each exports its own database onto its own branch,
//! and the machine that runs `git merge` ends up with both in its database,
//! and still with both in the bundle after its next commit exports.
//!
//! Needs a server: `DATABASE_URL` names it, and every database written here is
//! one this file creates and drops (`common::in_a_scratch_database`). The hooks
//! run the real binary against those databases; the merge is a real `git
//! merge` through the driver `hook install` wires.

mod common;

use common::in_a_scratch_database;
use serde_json::Value;
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");
const SYNC_DIR: &str = ".memory-industry";

/// A throwaway repo whose git sees none of this machine's configuration.
struct Repo {
    base: PathBuf,
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("mi-merge-db-{}", Uuid::new_v4()));
        let root = base.join("repo");
        std::fs::create_dir_all(root.join(SYNC_DIR)).unwrap();
        std::fs::write(base.join("empty-gitconfig"), "").unwrap();
        let repo = Repo { base, root };
        repo.git(&["init", "--quiet"]);
        repo
    }

    fn sync_dir(&self) -> PathBuf {
        self.root.join(SYNC_DIR)
    }

    /// `program` in the repo, cut off from the operator's git config, database
    /// and sync directory. The hooks read the database from the repo's own
    /// config, and nothing else.
    fn run(&self, program: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        Command::new(program)
            .args(args)
            .current_dir(&self.root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("CUBA_SYNC_DIR")
            .env_remove("DATABASE_URL")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.base.join("empty-gitconfig"))
            .env("XDG_CONFIG_HOME", self.base.join("no-xdg-config"))
            .env("GIT_AUTHOR_NAME", "merge-db")
            .env("GIT_AUTHOR_EMAIL", "merge-db@example.invalid")
            .env("GIT_COMMITTER_NAME", "merge-db")
            .env("GIT_COMMITTER_EMAIL", "merge-db@example.invalid")
            .envs(env.iter().copied())
            .output()
            .unwrap_or_else(|e| panic!("{program} {args:?}: {e}"))
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.run("git", args, &[]);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// `memory-industry sync <args>` against `database_url`, on the sync
    /// directory of this repo — what one machine's `sync export` leaves.
    fn sync(&self, database_url: &str, args: &[&str]) -> Output {
        let dir = self.sync_dir();
        let args: Vec<&str> = std::iter::once("sync")
            .chain(args.iter().copied())
            .collect();
        let out = self.run(
            BIN,
            &args,
            &[
                ("DATABASE_URL", database_url),
                ("CUBA_SYNC_DIR", dir.to_str().unwrap()),
            ],
        );
        assert!(
            out.status.success(),
            "sync {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", message]);
    }

    /// Every entity file and facts.json in the bundle, as one text.
    fn bundle_text(&self) -> String {
        let entities = self.sync_dir().join("entities");
        let mut text = std::fs::read_to_string(self.sync_dir().join("facts.json")).unwrap();
        for entry in std::fs::read_dir(&entities).unwrap().flatten() {
            text.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
        text
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// One entity with one observation, and one fact, written straight into the
/// database the way a machine's own work leaves them.
async fn remember(pool: &PgPool, entity: &str, content: &str, subject: &str) {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'concept') RETURNING id",
    )
    .bind(entity)
    .fetch_one(pool)
    .await
    .expect("seeding an entity");
    sqlx::query(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source)
         VALUES ($1, $2, 'fact', 'agent')",
    )
    .bind(id)
    .bind(content)
    .execute(pool)
    .await
    .expect("seeding an observation");
    sqlx::query(
        "INSERT INTO brain_facts (subject, predicate, object, valid_from, observed_at)
         VALUES ($1, 'runs on', 'postgres', NOW(), NOW())",
    )
    .bind(subject)
    .execute(pool)
    .await
    .expect("seeding a fact");
}

/// Whether the database holds the observation and the fact one machine wrote.
async fn holds(pool: &PgPool, content: &str, subject: &str) -> bool {
    let observations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_observations WHERE content = $1")
            .bind(content)
            .fetch_one(pool)
            .await
            .expect("counting observations");
    let facts: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_facts WHERE subject = $1")
        .bind(subject)
        .fetch_one(pool)
        .await
        .expect("counting facts");
    observations == 1 && facts == 1
}

#[tokio::test]
async fn a_clean_merge_reaches_the_database_and_the_next_export_keeps_both_sides() {
    in_a_scratch_database("brain_merge_ours", |ours_url| async move {
        in_a_scratch_database("brain_merge_theirs", move |theirs_url| async move {
            journey(ours_url, theirs_url).await;
        })
        .await;
    })
    .await;
}

async fn journey(ours_url: String, theirs_url: String) {
    let ours = memory_industry::db::create_pool(&ours_url)
        .await
        .expect("migrating the merging machine's database");
    let theirs = memory_industry::db::create_pool(&theirs_url)
        .await
        .expect("migrating the other machine's database");
    let t = Uuid::new_v4().to_string()[..8].to_string();
    let (our_content, our_subject) = (
        format!("lo que sabe esta maquina {t}"),
        format!("fact_ours_{t}"),
    );
    let (their_content, their_subject) = (
        format!("lo que sabe la otra maquina {t}"),
        format!("fact_theirs_{t}"),
    );

    let repo = Repo::new();
    let install = repo.run(BIN, &["hook", "install"], &[]);
    assert!(
        install.status.success(),
        "hook install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    // Until the repo names a database every hook is a no-op, so each branch
    // carries exactly what its machine exported.
    repo.sync(&ours_url, &["export", "--scope", "all"]);
    repo.commit("base export");
    let main = repo.git(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let main = main.trim();

    repo.git(&["checkout", "--quiet", "-b", "theirs"]);
    remember(
        &theirs,
        &format!("entity_theirs_{t}"),
        &their_content,
        &their_subject,
    )
    .await;
    repo.sync(&theirs_url, &["export", "--scope", "all"]);
    repo.commit("the other machine's export");

    repo.git(&["checkout", "--quiet", main]);
    remember(
        &ours,
        &format!("entity_ours_{t}"),
        &our_content,
        &our_subject,
    )
    .await;
    repo.sync(&ours_url, &["export", "--scope", "all"]);
    repo.commit("this machine's export");

    repo.git(&["config", "--local", "cuba-memorys.database-url", &ours_url]);
    let merge = repo.run("git", &["merge", "--no-edit", "theirs"], &[]);
    let unmerged = repo.git(&["diff", "--name-only", "--diff-filter=U"]);
    assert!(
        merge.status.success() && unmerged.is_empty(),
        "two exports of two databases differ in every file an export writes, and the only \
         thing a person should have to decide is what the driver cannot: here nothing is. \
         Unmerged: {unmerged}\n{}{}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );

    if !holds(&ours, &their_content, &their_subject).await {
        let by_hand = repo.run(
            BIN,
            &["sync", "import", "--conflict", "merge"],
            &[
                ("DATABASE_URL", ours_url.as_str()),
                ("CUBA_SYNC_DIR", repo.sync_dir().to_str().unwrap()),
            ],
        );
        panic!(
            "after a clean `git merge` the merging machine's database does not hold the other \
             branch's observation and fact. git runs post-merge after a merge and not \
             post-commit (githooks(5)), so the import has to be there; without it the next \
             commit exports a database that never saw the other branch over the merged bundle. \
             The same import run by hand says: {}{}",
            String::from_utf8_lossy(&by_hand.stdout),
            String::from_utf8_lossy(&by_hand.stderr)
        );
    }
    assert!(
        holds(&ours, &our_content, &our_subject).await,
        "control: this machine's own rows are still there after the merge"
    );

    std::fs::write(repo.root.join("notes.txt"), "after the merge\n").unwrap();
    repo.git(&["add", "notes.txt"]);
    repo.git(&["commit", "--quiet", "-m", "an ordinary commit"]);

    let bundle = repo.bundle_text();
    ours.close().await;
    theirs.close().await;
    for (what, text) in [
        ("the other machine's observation", &their_content),
        ("the other machine's fact", &their_subject),
        ("this machine's observation", &our_content),
        ("this machine's fact", &our_subject),
    ] {
        assert!(
            bundle.contains(text.as_str()),
            "the ordinary commit after the merge exported the database over the bundle, and \
             {what} ({text}) is not in it any more: whatever the merge brought only lasted until \
             the next commit"
        );
    }
}

#[tokio::test]
async fn an_export_without_embeddings_leaves_no_blob_it_did_not_write() {
    in_a_scratch_database("brain_no_blob", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let t = Uuid::new_v4().to_string()[..8].to_string();
        remember(
            &pool,
            &format!("entity_blob_{t}"),
            &format!("an observation {t}"),
            &format!("fact_blob_{t}"),
        )
        .await;
        pool.close().await;

        let repo = Repo::new();
        let blob = repo.sync_dir().join("embeddings.bin.zst");
        std::fs::write(
            &blob,
            memory_industry::sync::compressor::compress(&[0u8; 64]).unwrap(),
        )
        .unwrap();

        repo.sync(&url, &["export", "--scope", "all"]);
        let left_behind = blob.exists();
        let import = repo.sync(&url, &["import", "--conflict", "merge", "--json"]);
        let stdout = String::from_utf8_lossy(&import.stdout);
        let report: Value = stdout
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str(line).ok())
            .unwrap_or_else(|| panic!("the import printed no JSON report:\n{stdout}"));

        assert!(
            !left_behind,
            "an export without embeddings wrote no embeddings.bin.zst and left the one an \
             earlier export wrote: the manifest now says with_embeddings false, and the blob \
             describes some other state of the database"
        );
        assert_eq!(
            report["edited_since_export"],
            Value::Bool(false),
            "the import hashes every file in the directory, the blob included, so a blob the \
             export did not write made every bundle read as hand-edited, for ever: {report}"
        );
    })
    .await;
}

/// Every file under `dir`, at any depth.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

#[tokio::test]
async fn an_export_of_a_database_that_did_not_change_leaves_every_file_alone() {
    in_a_scratch_database("brain_idle_export", |url| async move {
        let pool = memory_industry::db::create_pool(&url)
            .await
            .expect("migrating the scratch database");
        let t = Uuid::new_v4().to_string()[..8].to_string();
        remember(
            &pool,
            &format!("entity_idle_{t}"),
            &format!("an observation {t}"),
            &format!("fact_idle_{t}"),
        )
        .await;
        pool.close().await;

        let repo = Repo::new();
        repo.sync(&url, &["export", "--scope", "all"]);
        repo.commit("what post-commit exported");
        let files = files_under(&repo.sync_dir());
        let entities = repo.sync_dir().join("entities");
        assert!(
            files.iter().any(|f| f.ends_with("manifest.json"))
                && files.iter().any(|f| f.parent() == Some(entities.as_path())),
            "control: the first export wrote a manifest and an entity file, or there is nothing \
             here for the second one to leave alone: {files:?}"
        );
        // A fixed instant in the past, so a rewrite shows whatever the
        // filesystem's clock resolution: a file the second export writes gets
        // the time of that write.
        let long_ago = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for file in &files {
            std::fs::File::options()
                .write(true)
                .open(file)
                .and_then(|f| f.set_modified(long_ago))
                .unwrap_or_else(|e| panic!("dating {file:?}: {e}"));
        }

        repo.sync(&url, &["export", "--scope", "all"]);

        let status = repo.git(&["status", "--porcelain", "--untracked-files=all"]);
        let rewritten: Vec<&PathBuf> = files.iter().filter(|f| modified(f) != long_ago).collect();
        let import = repo.sync(&url, &["import", "--conflict", "merge", "--json"]);
        let stdout = String::from_utf8_lossy(&import.stdout);
        let report: Value = stdout
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str(line).ok())
            .unwrap_or_else(|| panic!("the import printed no JSON report:\n{stdout}"));

        assert!(
            status.is_empty(),
            "post-commit exports after every commit, and an export of a database nothing wrote \
             to since the last one rewrote manifest.json with a new exported_at. The tree was \
             dirty after every commit, and `git merge`, `git checkout` and `git pull` refused \
             with «your local changes would be overwritten». Nothing changed, so nothing may \
             differ from the commit:\n{status}"
        );
        assert!(
            rewritten.is_empty(),
            "the same bytes written again are still a write: the file's mtime moves, and every \
             tool that watches the tree — git's index, an editor, a sync client — sees a change \
             that is not there. Rewritten with nothing new: {rewritten:?}"
        );
        assert_eq!(
            report["edited_since_export"],
            Value::Bool(false),
            "the manifest the second export left alone still has to describe the files: its \
             manifest_hash is what the import checks them against: {report}"
        );
    })
    .await;
}
