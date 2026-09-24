//! Two branches that both exported, merged by a real `git merge` through the
//! driver `hook install` wires.
//!
//! Every export writes `tombstones.json` and `manifest.json`, so every merge of
//! two such branches hands both to the driver. The unit tables in hooks_cli.rs
//! call the driver directly; this asks git, which is the only judge of whether
//! the attribute, the config entry and the binary's exit code add up to a merge.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Output};
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");
const SYNC_DIR: &str = ".memory-industry";

/// A throwaway repo whose git sees none of this machine's configuration.
struct Scratch {
    base: PathBuf,
    repo: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("mi-merge-e2e-{}", Uuid::new_v4()));
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join(SYNC_DIR)).unwrap();
        std::fs::write(base.join("empty-gitconfig"), "").unwrap();
        let scratch = Scratch { base, repo };
        scratch.git_ok(&["init", "--quiet"]);
        scratch
    }

    /// `program` in the repo, the way git runs a hook or the merge driver,
    /// cut off from the operator's git config, database and sync directory.
    /// With no database the hooks `install` writes do nothing, so no commit
    /// below exports: the bundle on each branch is the one written here.
    fn run(&self, program: &str, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .current_dir(&self.repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("CUBA_SYNC_DIR")
            .env_remove("DATABASE_URL")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", self.base.join("empty-gitconfig"))
            .env("XDG_CONFIG_HOME", self.base.join("no-xdg-config"))
            .env("GIT_AUTHOR_NAME", "merge-e2e")
            .env("GIT_AUTHOR_EMAIL", "merge-e2e@example.invalid")
            .env("GIT_COMMITTER_NAME", "merge-e2e")
            .env("GIT_COMMITTER_EMAIL", "merge-e2e@example.invalid")
            .output()
            .unwrap_or_else(|e| panic!("{program} {args:?}: {e}"))
    }

    fn git_ok(&self, args: &[&str]) -> String {
        let out = self.run("git", args);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Writes what one `sync export` would leave, as `export_into` spells it.
    fn export(&self, side: &str, tombstones: &[Value], relations: &[Value]) {
        let dir = self.repo.join(SYNC_DIR);
        let write = |name: &str, value: &Value| {
            std::fs::write(dir.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap()
        };
        write("manifest.json", &manifest(side));
        write("tombstones.json", &Value::Array(tombstones.to_vec()));
        write("relations.json", &Value::Array(relations.to_vec()));
        std::fs::write(dir.join("hand-notes.md"), format!("written on {side}\n")).unwrap();
    }

    fn commit(&self, message: &str) {
        self.git_ok(&["add", "-A"]);
        self.git_ok(&["commit", "--quiet", "-m", message]);
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.repo.join(SYNC_DIR).join(name)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn manifest(side: &str) -> Value {
    json!({
        "schema_version": 1,
        "manifest_hash": format!("hash-of-{side}"),
        "project_id": null,
        "project_name": null,
        "exported_at": "2026-09-01T00:00:00Z",
        "counts": {
            "entities": 0, "observations": 0, "episodes": 0, "decisions": 0,
            "errors": 0, "relations": 1, "artifacts": 0
        },
        "with_embeddings": false,
        "embedding_dim": null,
        "node_id": null,
        "embedding_model": null,
    })
}

fn tombstone(row_id: Uuid, deleted_at: &str, origin_node: &str) -> Value {
    json!({
        "table_name": "brain_observations",
        "row_id": row_id,
        "deleted_at": deleted_at,
        "origin_node": origin_node,
    })
}

fn relation(relation_type: &str) -> Value {
    json!({
        "id": Uuid::new_v4(),
        "from_entity": Uuid::new_v4(),
        "to_entity": Uuid::new_v4(),
        "relation_type": relation_type,
        "strength": 0.5,
        "bidirectional": false,
        "project_id": null,
        "created_at": "2026-09-01T00:00:00Z",
        "provenance": "extracted",
    })
}

type Named = (String, DateTime<Utc>, String);

fn instant(at: &str) -> DateTime<Utc> {
    at.parse()
        .unwrap_or_else(|e| panic!("{at:?} is not an instant: {e}"))
}

/// (row_id, deleted_at, origin_node) of every tombstone in the file.
fn tombstones_named(file: &str) -> BTreeSet<Named> {
    let rows: Vec<Value> = serde_json::from_str(file).unwrap_or_else(|e| {
        panic!("tombstones.json does not parse after the merge ({e}):\n{file}")
    });
    rows.iter()
        .map(|r| {
            let field = |k: &str| r[k].as_str().unwrap_or_default().to_string();
            (
                field("row_id"),
                instant(&field("deleted_at")),
                field("origin_node"),
            )
        })
        .collect()
}

#[test]
fn a_merge_of_two_branches_that_exported_stops_only_where_a_person_has_to_decide() {
    let scratch = Scratch::new();

    let install = scratch.run(BIN, &["hook", "install"]);
    assert!(
        install.status.success(),
        "hook install failed: {}{}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );
    let attribute = scratch.git_ok(&[
        "check-attr",
        "merge",
        "--",
        &format!("{SYNC_DIR}/tombstones.json"),
    ]);
    assert!(
        attribute.trim_end().ends_with(": merge: cuba-memorys"),
        "control: git has to hand the sync directory to the driver, or this merge is git's \
         own text merge and says nothing about the driver: {attribute}"
    );

    let [shared, only_ours, only_theirs] = [(); 3].map(|_| Uuid::new_v4());
    scratch.export(
        "base",
        &[tombstone(shared, "2026-08-01T00:00:00Z", "pc-base")],
        &[],
    );
    scratch.commit("base export");
    let main = scratch.git_ok(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let main = main.trim();

    scratch.git_ok(&["checkout", "--quiet", "-b", "theirs"]);
    scratch.export(
        "theirs",
        &[
            tombstone(shared, "2026-08-03T00:00:00Z", "pc-theirs"),
            tombstone(only_theirs, "2026-08-02T00:00:00Z", "pc-theirs"),
        ],
        &[relation("theirs")],
    );
    scratch.commit("their export");

    scratch.git_ok(&["checkout", "--quiet", main]);
    scratch.export(
        "ours",
        &[
            tombstone(shared, "2026-08-01T00:00:00Z", "pc-base"),
            tombstone(only_ours, "2026-08-02T00:00:00Z", "pc-ours"),
        ],
        &[relation("ours")],
    );
    scratch.commit("our export");

    let merge = scratch.run("git", &["merge", "--no-edit", "theirs"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );
    let unmerged: BTreeSet<String> = scratch
        .git_ok(&["diff", "--name-only", "--diff-filter=U"])
        .lines()
        .map(str::to_string)
        .collect();
    let path = |name: &str| format!("{SYNC_DIR}/{name}");

    assert!(
        !unmerged.contains(&path("relations.json")),
        "control: relations.json has been merged by the driver since before this change; if \
         git stops on it here the driver never ran and the rest of this test means nothing. \
         Unmerged: {unmerged:?}\n{said}"
    );
    assert!(
        !unmerged.contains(&path("tombstones.json")),
        "every export writes tombstones.json, so a merge of two branches that both exported \
         stopped on it every time, and a person resolved a list of deletions by hand as text. \
         The driver merges it as a set keyed by (table_name, row_id). Unmerged: \
         {unmerged:?}\n{said}"
    );
    let merged = scratch.read("tombstones.json");
    let expected: BTreeSet<Named> = [
        (shared, "2026-08-03T00:00:00Z", "pc-theirs"),
        (only_ours, "2026-08-02T00:00:00Z", "pc-ours"),
        (only_theirs, "2026-08-02T00:00:00Z", "pc-theirs"),
    ]
    .into_iter()
    .map(|(id, at, node)| (id.to_string(), instant(at), node.to_string()))
    .collect();
    assert_eq!(
        tombstones_named(&merged),
        expected,
        "both branches' deletions, once each, the row both deleted at its later deletion:\n\
         {merged}"
    );

    assert!(
        unmerged.contains(&path("manifest.json")),
        "manifest.json stays a person's to resolve: the import acts on its project_id, \
         with_embeddings, embedding_dim and embedding_model without checking them against \
         the files, so picking a side is not the driver's call. Unmerged: {unmerged:?}\n{said}"
    );
    assert!(
        unmerged.contains(&path("hand-notes.md")),
        "a file sync never writes is left to git as a conflict, never kept as ours in \
         silence. Unmerged: {unmerged:?}\n{said}"
    );
    assert!(
        !merge.status.success(),
        "git has to report the merge as stopped while anything is unmerged:\n{said}"
    );
}
