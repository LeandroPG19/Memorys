//! Two branches that both exported, merged by a real `git merge` through the
//! driver `hook install` wires, and the hooks git runs around that merge.
//!
//! Every export writes `tombstones.json`, `manifest.json` and the four row
//! files, so every merge of two such branches hands all of them to the driver.
//! The unit tables in hooks_cli.rs call the driver directly; this asks git,
//! which is the only judge of whether the attribute, the config entry and the
//! binary's exit code add up to a merge, and of which hook runs when.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");
const SYNC_DIR: &str = ".memory-industry";

/// The four files every export writes one list of rows into, and the field
/// that names a row in each: the fixture writes the side's name there.
const ROW_FILES: [(&str, &str); 4] = [
    ("facts.json", "subject"),
    ("procedures.json", "name"),
    ("artifacts.json", "path"),
    ("source_trust.json", "source"),
];

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
        self.export_with(&manifest(side), side, tombstones, relations);
    }

    /// `export` with the manifest given: the tombstones and relations given,
    /// and one row named `side` in each of the four row files.
    fn export_with(&self, manifest: &Value, side: &str, tombstones: &[Value], relations: &[Value]) {
        let dir = self.repo.join(SYNC_DIR);
        let write = |name: &str, value: &Value| {
            std::fs::write(dir.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap()
        };
        write("manifest.json", manifest);
        write("tombstones.json", &Value::Array(tombstones.to_vec()));
        write("relations.json", &Value::Array(relations.to_vec()));
        for (file, _) in ROW_FILES {
            write(file, &json!([row(file, side)]));
        }
        std::fs::write(dir.join("hand-notes.md"), format!("written on {side}\n")).unwrap();
    }

    /// A file at the top of the work tree, outside the sync directory.
    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.repo.join(name), content).unwrap();
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

/// One row of `file`, spelled the way `export_into` writes it, named `side`.
fn row(file: &str, side: &str) -> Value {
    let at = "2026-09-01T00:00:00Z";
    match file {
        "facts.json" => json!({
            "fact_id": Uuid::new_v4(), "subject": side, "predicate": "runs on",
            "object": "postgres", "valid_from": at, "observed_at": at, "valid_to": null,
            "subject_entity_id": null, "project_id": null, "confidence": 0.5,
            "is_current": true, "created_at": at, "layer_name": null,
        }),
        "procedures.json" => json!({
            "id": Uuid::new_v4(), "name": side, "steps": [{"do": side}], "created_at": at,
            "updated_at": at, "trigger_context": "", "preconditions": "", "verification": "",
            "success_count": 0, "failure_count": 0, "last_outcome": null, "last_used_at": null,
            "project_id": null, "embedding_model": null,
        }),
        "artifacts.json" => json!({
            "id": Uuid::new_v4(), "path": side, "content": format!("written on {side}"),
            "content_hash": format!("hash of {side}"), "version": 1, "project_id": null,
            "origin_node": side, "crdt_actor": side, "crdt_counter": 1, "updated_at": at,
        }),
        "source_trust.json" => json!({
            "source": side, "alpha": 1.0, "beta": 1.0, "updated_at": at,
        }),
        other => panic!("the fixture writes no rows into {other}"),
    }
}

/// The names `field` holds across the rows of a row file after the merge.
fn names_in(file: &str, content: &str, field: &str) -> BTreeSet<String> {
    let rows: Vec<Value> = serde_json::from_str(content)
        .unwrap_or_else(|e| panic!("{file} does not parse after the merge ({e}):\n{content}"));
    rows.iter()
        .map(|r| r[field].as_str().unwrap_or_default().to_string())
        .collect()
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

    for (file, field) in ROW_FILES {
        assert!(
            !unmerged.contains(&path(file)),
            "every export writes {file}, so a merge of two branches that both exported stopped \
             on it every time. Unmerged: {unmerged:?}\n{said}"
        );
        let merged = scratch.read(file);
        assert_eq!(
            names_in(file, &merged, field),
            BTreeSet::from(["ours".to_string(), "theirs".to_string()]),
            "{file} after the merge holds both branches' rows, once each:\n{merged}"
        );
    }
    assert!(
        !unmerged.contains(&path("manifest.json")),
        "both manifests name one project and one embedding space, so the import reads the \
         merged bundle the same whichever side stands; the rest of the file is derived and the \
         next export rewrites it. A conflict here stopped every such merge. Unmerged: \
         {unmerged:?}\n{said}"
    );
    let kept: Value = serde_json::from_str(&scratch.read("manifest.json")).unwrap();
    assert_eq!(
        kept["manifest_hash"], "hash-of-ours",
        "the manifest that stands is ours: {kept}"
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

#[test]
fn a_manifest_of_another_project_stops_the_merge_and_says_which_field() {
    let scratch = Scratch::new();
    let install = scratch.run(BIN, &["hook", "install"]);
    assert!(
        install.status.success(),
        "hook install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );

    scratch.export("base", &[], &[]);
    scratch.commit("base export");
    let main = scratch.git_ok(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let main = main.trim();

    scratch.git_ok(&["checkout", "--quiet", "-b", "theirs"]);
    let mut theirs = manifest("theirs");
    theirs["project_id"] = json!(Uuid::new_v4());
    scratch.export_with(&theirs, "theirs", &[], &[]);
    scratch.commit("their export");

    scratch.git_ok(&["checkout", "--quiet", main]);
    scratch.export("ours", &[], &[]);
    scratch.commit("our export");

    let merge = scratch.run("git", &["merge", "--no-edit", "theirs"]);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&merge.stdout),
        String::from_utf8_lossy(&merge.stderr)
    );
    let unmerged = scratch.git_ok(&["diff", "--name-only", "--diff-filter=U"]);

    assert!(
        unmerged
            .lines()
            .any(|l| l == format!("{SYNC_DIR}/manifest.json")),
        "the import writes a bundle's rows under the manifest's project_id. Two manifests that \
         name different projects describe bundles of different scopes, and keeping ours would \
         import the other branch's rows under a scope nobody chose: git has to stop here. \
         Unmerged: {unmerged}\n{said}"
    );
    assert!(
        said.contains("project_id"),
        "what git prints is all the person resolving it gets to read; it has to say which field \
         the two sides disagree on:\n{said}"
    );
}

/// Replaces the binary the installed hooks start with a script that writes
/// down the arguments of every call, one line each, into the file returned:
/// which hook ran, and in what order, without a database.
fn stub_the_hooks(scratch: &Scratch) -> PathBuf {
    let slash = |p: &Path| p.to_string_lossy().replace('\\', "/");
    let log = scratch.base.join("hook-calls.log");
    let stub = scratch.base.join("memory-industry-stub");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", slash(&log)),
    )
    .unwrap();
    make_executable(&stub);

    let spellings = [
        BIN.to_string(),
        std::fs::canonicalize(BIN).unwrap().display().to_string(),
    ];
    let hooks = scratch.repo.join(".git").join("hooks");
    for hook in ["post-commit", "post-checkout", "post-merge"] {
        let path = hooks.join(hook);
        let Ok(body) = std::fs::read_to_string(&path) else {
            continue;
        };
        let stubbed = spellings.iter().fold(body.clone(), |body, exe| {
            body.replace(&format!("\"{exe}\""), &format!("\"{}\"", slash(&stub)))
        });
        assert_ne!(
            stubbed, body,
            "control: {hook} does not start this binary under any spelling of its path, so the \
             stub would record nothing and every assertion below would read an empty log:\n{body}"
        );
        std::fs::write(&path, stubbed).unwrap();
    }
    log
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// The calls the stub recorded since the last read, leaving the log empty.
fn calls_since(log: &Path) -> Vec<String> {
    let calls = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let _ = std::fs::remove_file(log);
    calls
}

#[test]
fn a_merge_is_imported_before_anything_exports_over_it() {
    let scratch = Scratch::new();
    let install = scratch.run(BIN, &["hook", "install"]);
    assert!(
        install.status.success(),
        "hook install failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    scratch.git_ok(&[
        "config",
        "--local",
        "cuba-memorys.database-url",
        "postgresql://stub.invalid/none",
    ]);
    let log = stub_the_hooks(&scratch);
    let export = "sync export --scope all".to_string();
    let import = "sync import --conflict merge".to_string();

    scratch.write("README.md", "base\n");
    scratch.commit("base");
    assert_eq!(
        calls_since(&log),
        [export.clone()],
        "control: a commit exports, and the stub hears it; without this the log says nothing"
    );
    let main = scratch.git_ok(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let main = main.trim();

    scratch.git_ok(&["checkout", "--quiet", "-b", "clean"]);
    scratch.write("a.txt", "theirs\n");
    scratch.commit("their change");
    scratch.git_ok(&["checkout", "--quiet", main]);
    scratch.write("b.txt", "ours\n");
    scratch.commit("our change");
    calls_since(&log);

    scratch.git_ok(&["merge", "--quiet", "--no-edit", "clean"]);
    assert_eq!(
        calls_since(&log),
        [import.clone()],
        "a clean `git merge` runs post-merge and neither post-commit nor post-checkout \
         (githooks(5): post-commit \"is invoked by git-commit\", post-merge \"is invoked by \
         git-merge\"). With no post-merge nothing brought the merged bundle into the database, \
         and the next commit's export rewrote the bundle from a database that never saw the \
         other branch: its rows were gone"
    );

    scratch.git_ok(&["checkout", "--quiet", "-b", "conflicting"]);
    scratch.write("c.txt", "theirs\n");
    scratch.commit("their c");
    scratch.git_ok(&["checkout", "--quiet", main]);
    scratch.write("c.txt", "ours\n");
    scratch.commit("our c");
    calls_since(&log);

    let stopped = scratch.run("git", &["merge", "--no-edit", "conflicting"]);
    assert!(
        !stopped.status.success(),
        "control: c.txt differs on both sides, so git stops for a person"
    );
    assert_eq!(
        calls_since(&log),
        Vec::<String>::new(),
        "control: githooks(5) — post-merge \"is not executed, if the merge failed due to \
         conflicts\". The commit that concludes it is the only hook left to run"
    );
    scratch.write("c.txt", "both\n");
    scratch.git_ok(&["add", "c.txt"]);
    scratch.git_ok(&["commit", "--quiet", "--no-edit"]);
    assert_eq!(
        calls_since(&log),
        [import.clone(), export.clone()],
        "the commit that concludes a merge is the first hook to run after it, and post-commit \
         only exported: the person's resolution of the bundle was rewritten from a database \
         that had imported neither side of the merge. On a merge commit (HEAD^2 exists) it \
         has to import first, then export"
    );

    scratch.write("d.txt", "later\n");
    scratch.commit("an ordinary commit");
    assert_eq!(
        calls_since(&log),
        [export],
        "an ordinary commit has one parent and only exports, as before: the import is paid \
         for on merges alone"
    );
}
