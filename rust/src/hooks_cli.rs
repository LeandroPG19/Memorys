use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sync::chunk::{EntityFile, EpisodeFile, ErrorFile, ProjectRow, RelationRow};
use crate::sync::paths;

const MARKER: &str = "# cuba-memorys hook — installed by `cuba-memorys hook install`";
const MERGE_DRIVER_NAME: &str = "cuba-memorys";

pub async fn run_cli(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("install") => {
            let mut with_codegraph = false;
            for a in &args[1..] {
                match a.as_str() {
                    "--with-codegraph" => with_codegraph = true,
                    other => anyhow::bail!("unknown hook install flag: {other} (try --help)"),
                }
            }
            print!("{}", install(with_codegraph)?);
            Ok(())
        }
        Some("uninstall") => uninstall(),
        Some("merge-driver") => merge_driver(&args[1..]),
        Some("-h") | Some("--help") | None => {
            eprintln!(
                "usage: memory-industry hook <install|uninstall> [--with-codegraph]\n\n\
                 Wires this repo's git so the knowledge graph stays in sync automatically.\n\
                 It lives in .memory-industry/, or in .cuba-memorys/ on a repo that already\n\
                 has that older directory; $CUBA_SYNC_DIR overrides both, and install\n\
                 writes the directory it resolves into the hooks:\n\
                 \x20 - post-commit  runs `sync export` after every commit\n\
                 \x20 - post-checkout runs `sync import` after checkout/branch switch\n\
                 \x20 - a git merge driver that unions observations/relations/entities\n\
                 \x20   by id instead of leaving conflict markers in graph JSON. Only for a\n\
                 \x20   directory inside this repo: git never merges anything outside it\n\n\
                 --with-codegraph also runs `codegraph build` (rust,python) after every\n\
                 commit, so the code graph stays current the way sync keeps memory current.\n\
                 Off by default — it re-parses the whole tree, which is not free on a large repo.\n\n\
                 `uninstall` removes exactly what `install` added (hook blocks, merge driver\n\
                 config, .gitattributes line) and leaves everything else untouched.\n\n\
                 Existing hooks are appended to, never overwritten. Safe to run twice."
            );
            Ok(())
        }
        Some(other) => anyhow::bail!("unknown hook subcommand: {other} (try --help)"),
    }
}

fn git_root() -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running `git rev-parse --show-toplevel` — is git installed?")?;
    if !out.status.success() {
        anyhow::bail!("not inside a git repository (git rev-parse failed)");
    }
    let s = String::from_utf8(out.stdout).context("git output was not utf8")?;
    Ok(PathBuf::from(s.trim()))
}

fn hooks_dir(root: &Path) -> PathBuf {
    let out = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .current_dir(root)
        .output();
    if let Ok(out) = out
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            let p = PathBuf::from(&s);
            return if p.is_absolute() { p } else { root.join(p) };
        }
    }
    root.join(".git").join("hooks")
}

fn read_existing_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {path:?}")),
    }
}

fn append_hook_block(path: &Path, block: &str) -> Result<bool> {
    let existing = read_existing_or_empty(path)?;
    if existing.contains(MARKER) {
        return Ok(false);
    }
    let mut body = existing;
    if body.is_empty() {
        body.push_str("#!/bin/sh\n");
    } else if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push('\n');
    body.push_str(block);
    std::fs::write(path, body).with_context(|| format!("writing hook {path:?}"))?;
    set_executable(path)?;
    Ok(true)
}

/// Leaves `block` as install's one block in the hook at `path`, replacing the
/// one an earlier install left. `append_hook_block` alone kept the first block
/// for ever, and the block now fixes CUBA_SYNC_DIR: a reinstall that moved the
/// `.gitattributes` line and not the hook would export where nothing guards.
fn put_hook_block(path: &Path, block: &str) -> Result<&'static str> {
    if read_existing_or_empty(path)?.contains(block) {
        return Ok("already present");
    }
    let replaced = remove_hook_block(path)?;
    append_hook_block(path, block)?;
    Ok(if replaced { "updated" } else { "installed" })
}

/// `value` as one `sh` word: inside single quotes nothing is special except
/// the single quote itself, which closes, is escaped, and reopens.
fn sh_quote(value: &str) -> String {
    // `'\u{27}'` is `'\''`, spelled so lizard's Rust reader does not take it for
    // a lifetime (sync/paths.rs::slug).
    format!("'{}'", value.replace('\u{27}', "'\\''"))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// The `.gitattributes` line that puts the merge driver on the files `sync`
/// actually writes.
///
/// The directory is NOT decided here. `sync export` asks `sync::paths` where to
/// write, and this file used to keep a second, hand-written copy of that rule
/// with the older name still in it: on a repo where neither directory existed
/// yet, sync wrote `.memory-industry/` while the driver was installed on
/// `.cuba-memorys/**`. It never reached one file sync touched, `install` printed
/// `installed` all the same, and the first time two machines reconciled one
/// graph it was resolved as text over JSON.
///
/// git reads a pattern only one way: from the top of the work tree, directories
/// joined with `/`, no `.` and no `..`. Measured with `git check-attr`, every
/// other spelling of the right directory — absolute, `./dir`, `x/../dir`,
/// `data\sync` from `Path::display` on Windows — is `merge: unspecified`, and
/// `install` used to write each of them as given and print `added`. So both
/// sides are resolved first (a relative CUBA_SYNC_DIR against the repo root,
/// which is where the hook runs it, not against wherever `install` was typed)
/// and the pattern is rebuilt from the components between them.
///
/// A directory outside the repo has no pattern at all: git merges only files
/// in its own work tree. That answer is `Outside`, for `install` to say so,
/// rather than a line that looks like protection and matches nothing.
fn gitattributes_line(root: &Path, configured_sync_root: Option<&Path>) -> AttributeLine {
    let root = paths::real_path(root);
    let dir = sync_dir_for(&root, configured_sync_root);
    let Ok(relative) = dir.strip_prefix(&root) else {
        return AttributeLine::Outside(dir);
    };
    let relative: Vec<_> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    AttributeLine::Inside(format!(
        "{} merge={MERGE_DRIVER_NAME}",
        attribute_pattern(&relative.join("/"))
    ))
}

#[derive(Debug, PartialEq)]
enum AttributeLine {
    /// The whole `.gitattributes` line.
    Inside(String),
    /// Where sync writes, resolved, for the message.
    Outside(PathBuf),
}

/// The directory `sync` will write to, as `sync::paths` resolves it, spelled
/// the way the filesystem spells it. `root` is the repo root; a relative
/// configured directory hangs from it, an absolute one replaces it.
fn sync_dir_for(root: &Path, configured_sync_root: Option<&Path>) -> PathBuf {
    let root = paths::real_path(root);
    match configured_sync_root {
        Some(configured) => paths::real_path(&root.join(configured)),
        None => paths::real_path(&paths::default_sync_dir(&root)),
    }
}

/// `<dir>/**`, quoted in the C style git accepts when `dir` holds whitespace or
/// a double quote: whitespace ends a pattern, so `it's a dir/**` bare is the
/// pattern `it's` and two attributes git refuses. Everything else stays bare,
/// which is the one spelling every git version reads.
fn attribute_pattern(dir: &str) -> String {
    // `'\u{22}'` is the double quote and `'\u{5c}'` the backslash, spelled with
    // escapes because lizard's Rust reader misreads quote characters in char
    // literals and then measures nothing to the next one (sync/paths.rs::slug).
    if !dir.contains(|c: char| c.is_whitespace() || c == '\u{22}') {
        return format!("{dir}/**");
    }
    let escaped = dir.replace('\u{5c}', "\\\\").replace('\u{22}', "\\\"");
    format!("\"{escaped}/**\"")
}

/// A line some `install` wrote, in any shape it has ever written: a bare
/// `<dir>/**` or a quoted one, absolute, `./`-prefixed or right.
fn is_driver_line(line: &str) -> bool {
    line.trim()
        .strip_suffix(&format!(" merge={MERGE_DRIVER_NAME}"))
        .is_some_and(|pattern| pattern.ends_with("/**") || pattern.ends_with("/**\""))
}

/// Leaves `line` as the one merge-driver line in `.gitattributes`. Earlier
/// installs wrote their lines in shapes git cannot match and never took them
/// back, so they piled up one per install, each reading like a guard; they go,
/// and every line that is not the driver's stays where it was.
fn write_gitattributes_line(root: &Path, line: &str) -> Result<bool> {
    let path = root.join(".gitattributes");
    let existing = read_existing_or_empty(&path)?;
    let (driver, other): (Vec<&str>, Vec<&str>) = existing.lines().partition(|l| is_driver_line(l));
    if driver == [line] {
        return Ok(false);
    }
    let lines: Vec<&str> = other.into_iter().chain([line]).collect();
    std::fs::write(&path, format!("{}\n", lines.join("\n")))
        .with_context(|| format!("writing {path:?}"))?;
    Ok(true)
}

fn remove_gitattributes_line(root: &Path) -> Result<bool> {
    let path = root.join(".gitattributes");
    if !path.exists() {
        return Ok(false);
    }
    let existing = read_existing_or_empty(&path)?;
    let filtered: Vec<&str> = existing.lines().filter(|l| !is_driver_line(l)).collect();
    let changed = filtered.len() != existing.lines().count();
    if changed {
        if filtered.is_empty() {
            std::fs::remove_file(&path).with_context(|| format!("removing {path:?}"))?;
        } else {
            std::fs::write(&path, format!("{}\n", filtered.join("\n")))
                .with_context(|| format!("writing {path:?}"))?;
        }
    }
    Ok(changed)
}

fn git_config(root: &Path, key: &str, value: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["config", "--local", key, value])
        .current_dir(root)
        .status()
        .with_context(|| format!("running `git config --local {key}`"))?;
    if !status.success() {
        anyhow::bail!("git config --local {key} failed (exit {status})");
    }
    Ok(())
}

/// Installs the hooks and the merge driver in the repo git finds from here,
/// and returns the report for the person who ran it.
fn install(with_codegraph: bool) -> Result<String> {
    let root = git_root()?;
    let hooks = hooks_dir(&root);
    std::fs::create_dir_all(&hooks).context("creating hooks dir")?;

    let exe = std::env::current_exe().context("resolving path to this binary")?;
    let exe = exe.display();
    let configured = paths::configured_root();

    // The hook fixes CUBA_SYNC_DIR to what it resolves to now. It used to read
    // it from whatever environment ran the commit, and that is not the shell
    // `install` ran in: an IDE, a GUI client or a cron job each bring their own,
    // so the same commit exported to one directory from the terminal and to
    // another (or to the default) from the editor — while the `.gitattributes`
    // line, written once here, guards only the first.
    let pin_sync_dir = format!(
        "CUBA_SYNC_DIR={}; export CUBA_SYNC_DIR",
        sh_quote(&sync_dir_for(&root, configured.as_deref()).to_string_lossy())
    );
    let resolve_url_sh = "db_url=$(git config --local --get cuba-memorys.database-url 2>/dev/null || true); [ -z \"$db_url\" ] && db_url=\"$DATABASE_URL\"";
    let (codegraph_line, codegraph_state) = if with_codegraph {
        (
            format!(
                " DATABASE_URL=\"$db_url\" \"{exe}\" codegraph build --lang rust,python >/dev/null 2>&1 || true\n"
            ),
            "enabled",
        )
    } else {
        (String::new(), "disabled (pass --with-codegraph to enable)")
    };
    let post_commit_block = format!(
        "{MARKER}\n\
         {pin_sync_dir}\n\
         {resolve_url_sh}\n\
         if [ -n \"$db_url\" ]; then\n\
         \x20 DATABASE_URL=\"$db_url\" \"{exe}\" sync export --scope all >/dev/null 2>&1 || true\n\
         {codegraph_line}\
         fi\n"
    );
    let commit_state = put_hook_block(&hooks.join("post-commit"), &post_commit_block)?;

    let post_checkout_block = format!(
        "{MARKER}\n\
         {pin_sync_dir}\n\
         {resolve_url_sh}\n\
         if [ -n \"$db_url\" ]; then\n\
         \x20 DATABASE_URL=\"$db_url\" \"{exe}\" sync import --conflict merge >/dev/null 2>&1 || true\n\
         fi\n"
    );
    let checkout_state = put_hook_block(&hooks.join("post-checkout"), &post_checkout_block)?;

    let driver_report = wire_merge_driver(
        &root,
        &format!("\"{exe}\" hook merge-driver %O %A %B %P"),
        gitattributes_line(&root, configured.as_deref()),
    )?;

    Ok(format!(
        "post-commit hook:   {commit_state}\n\
         post-checkout hook: {checkout_state}\n\
         {driver_report}\
         codegraph on commit: {codegraph_state}\n\
         \n\
         NOTE: both hooks are a no-op until this repo's database is set explicitly.\n\
         They deliberately do NOT fall back to auto-detecting a running container —\n\
         on a machine with more than one MemoryIndustry database, that guess can export\n\
         from, or import into, the wrong one. Set it once, it persists in .git/config:\n\
         \x20 git config --local cuba-memorys.database-url \"postgresql://...\"\n\
         (DATABASE_URL in the environment also works as a fallback.)\n"
    ))
}

/// The merge driver's half of `install`: the driver in `.git/config` and the
/// `.gitattributes` line that hands it the sync files, or — when the sync
/// directory is outside the repo, where git never merges — neither, and the
/// report says why.
fn wire_merge_driver(root: &Path, driver: &str, line: AttributeLine) -> Result<String> {
    let line = match line {
        AttributeLine::Inside(line) => line,
        AttributeLine::Outside(dir) => {
            return Ok(format!(
                "merge driver:       not configured (nothing in this repo for it to merge)\n\
                 .gitattributes:     skipped — {} is outside this repo; git never merges it\n",
                dir.display()
            ));
        }
    };
    git_config(
        root,
        &format!("merge.{MERGE_DRIVER_NAME}.name"),
        "cuba-memorys structural merge (union by id)",
    )?;
    git_config(root, &format!("merge.{MERGE_DRIVER_NAME}.driver"), driver)?;
    let attributes = if write_gitattributes_line(root, &line)? {
        format!("added `{line}`")
    } else {
        "already present".to_string()
    };
    Ok(format!(
        "merge driver:       configured (merge.{MERGE_DRIVER_NAME}.driver in .git/config)\n\
         .gitattributes:     {attributes}\n"
    ))
}

fn remove_hook_block(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let Some(marker_pos) = content.find(MARKER) else {
        return Ok(false);
    };

    let before_marker = &content[..marker_pos];
    let truncate_at = before_marker.trim_end_matches('\n').len();
    let remainder = &content[marker_pos..];
    let our_block_end = remainder
        .find("\nfi\n")
        .map(|i| marker_pos + i + "\nfi\n".len())
        .unwrap_or(content.len());
    if our_block_end < content.len() {
        eprintln!(
            "warning: {path:?} has content after the cuba-memorys block — leaving it, \
             only the marker line and this tool's own lines were removed"
        );
    }

    let kept_before = &content[..truncate_at];
    let kept_after = &content[our_block_end..];
    let new_content = format!("{kept_before}\n{kept_after}");
    let new_content = new_content.trim_end_matches('\n');
    let new_content = if new_content == "#!/bin/sh" {
        String::new()
    } else {
        format!("{new_content}\n")
    };

    if new_content.is_empty() {
        std::fs::remove_file(path).with_context(|| format!("removing empty hook {path:?}"))?;
    } else {
        std::fs::write(path, new_content).with_context(|| format!("rewriting hook {path:?}"))?;
    }
    Ok(true)
}

fn uninstall() -> Result<()> {
    let root = git_root()?;
    let hooks = hooks_dir(&root);

    let commit_removed = remove_hook_block(&hooks.join("post-commit"))?;
    let checkout_removed = remove_hook_block(&hooks.join("post-checkout"))?;

    let _ = Command::new("git")
        .args([
            "config",
            "--unset",
            &format!("merge.{MERGE_DRIVER_NAME}.name"),
        ])
        .current_dir(&root)
        .status();
    let _ = Command::new("git")
        .args([
            "config",
            "--unset",
            &format!("merge.{MERGE_DRIVER_NAME}.driver"),
        ])
        .current_dir(&root)
        .status();

    let attrs_removed = remove_gitattributes_line(&root)?;

    println!(
        "post-commit hook:   {}",
        if commit_removed {
            "removed"
        } else {
            "was not installed"
        }
    );
    println!(
        "post-checkout hook: {}",
        if checkout_removed {
            "removed"
        } else {
            "was not installed"
        }
    );
    println!("merge driver:       unset (merge.{MERGE_DRIVER_NAME}.* removed from .git/config)");
    println!(
        ".gitattributes:     {}",
        if attrs_removed {
            "line removed"
        } else {
            "unchanged"
        }
    );
    Ok(())
}

/// git's merge driver for the sync files. Exit 0 tells git the file merged
/// cleanly and that %A holds the result; so every path that ends without a
/// merge written over %A is an error, and git leaves the file conflicted for a
/// person to resolve. Returning 0 there, as this did, handed git ours as the
/// merge: theirs was dropped with no conflict and nothing to say so.
fn merge_driver(args: &[String]) -> Result<()> {
    let [_ancestor, ours, theirs, path] = args else {
        anyhow::bail!("usage: hook merge-driver %O %A %B %P (git passes these itself)");
    };

    let Some(kind) = sync_file_kind(path) else {
        anyhow::bail!("{path} is not a file `sync export` writes; left to git as a conflict");
    };
    let merge: fn(&str, &str) -> Result<Option<Vec<u8>>> = match kind {
        SyncFile::Entity => merge_entity_file,
        SyncFile::Relations => merge_relations,
        SyncFile::Projects => merge_projects,
        SyncFile::Episode => merge_episode_file,
        SyncFile::Error => merge_error_file,
        SyncFile::Decision => merge_decision_file,
        SyncFile::Tombstones => merge_tombstones,
    };
    let Some(bytes) = merge(ours, theirs)? else {
        anyhow::bail!("{path}: one side does not read as {kind:?} JSON; left to git as a conflict");
    };
    std::fs::write(ours, bytes).with_context(|| format!("writing merged {ours}"))?;
    Ok(())
}

#[derive(Debug)]
enum SyncFile {
    Entity,
    Relations,
    Projects,
    Episode,
    Error,
    Decision,
    Tombstones,
}

/// Which sync file git's `%P` names, read from its last components — the
/// layout `sync export` writes under its root — and nothing in front of them.
///
/// `%P` is the path from the top of the work tree, so the sync root's own path
/// comes first: nothing at all when the root is the repository root, anything
/// CUBA_SYNC_DIR names otherwise. The rules used to be substrings of the whole
/// path, and that is where both of their data losses came from: `/episodes/`
/// with a slash in front never matched at a root with nothing in front of it,
/// and a root whose path merely said `entities` sent `relations.json` and
/// `projects.json` to the entity merge. Now the file name decides first, then
/// the directory holding the file, then the one above it (an episode lives at
/// `episodes/<YYYY-MM>/`). `/` is the only separator: git writes no other.
///
/// Two files every export writes get no rule on purpose, so git leaves them
/// to a person. `manifest.json`: the import recomputes its `manifest_hash` and
/// only reports a mismatch (handlers/sync.rs:1851-1852, `edited_since_export`
/// at :2678), but it acts on the rest of it unchecked — `project_id` sets the
/// scope the rows are written under (:1810-1820), and `with_embeddings`,
/// `embedding_dim` and `embedding_model` decide whether and how the vectors
/// are read (:1824-1848, :2583-2589). Nothing, `validate_bundle` included,
/// checks those against the files, so which side's manifest stands is not a
/// call this driver can make safely. `embeddings.bin.zst`: zstd over records
/// whose width only the manifest declares, and not regenerable from the other
/// files — the vectors come from the database and the model that made them.
fn sync_file_kind(path: &str) -> Option<SyncFile> {
    let path = path.to_lowercase();
    let mut up = path.rsplit('/');
    let name = up.next().unwrap_or_default();
    let parent = up.next().unwrap_or_default();
    let grandparent = up.next().unwrap_or_default();
    match (name, parent, grandparent) {
        ("relations.json", _, _) => Some(SyncFile::Relations),
        ("projects.json", _, _) => Some(SyncFile::Projects),
        ("tombstones.json", _, _) => Some(SyncFile::Tombstones),
        (_, "entities", _) => Some(SyncFile::Entity),
        (_, "errors", _) => Some(SyncFile::Error),
        (_, "decisions", _) => Some(SyncFile::Decision),
        (_, "episodes", _) | (_, _, "episodes") => Some(SyncFile::Episode),
        _ => None,
    }
}

fn merge_entity_file(ours_path: &str, theirs_path: &str) -> Result<Option<Vec<u8>>> {
    let a: Option<EntityFile> = read_json(ours_path)?;
    let b: Option<EntityFile> = read_json(theirs_path)?;
    let (Some(mut a), Some(b)) = (a, b) else {
        return Ok(None);
    };

    let mut by_id: HashMap<_, _> = a.observations.drain(..).map(|o| (o.id, o)).collect();
    for obs in b.observations {
        by_id.entry(obs.id).or_insert(obs);
    }
    let mut merged: Vec<_> = by_id.into_values().collect();
    merged.sort_by_key(|o| o.created_at);
    a.observations = merged;
    a.access_count = a.access_count.max(b.access_count);
    a.importance = a.importance.max(b.importance);

    Ok(Some(serde_json::to_vec_pretty(&a)?))
}

fn merge_relations(ours_path: &str, theirs_path: &str) -> Result<Option<Vec<u8>>> {
    let a: Option<Vec<RelationRow>> = read_json(ours_path)?;
    let b: Option<Vec<RelationRow>> = read_json(theirs_path)?;
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(None);
    };

    let mut by_key: HashMap<(uuid::Uuid, uuid::Uuid, String), RelationRow> = HashMap::new();
    for rel in a.into_iter().chain(b) {
        let key = (rel.from_entity, rel.to_entity, rel.relation_type.clone());
        by_key
            .entry(key)
            .and_modify(|existing| {
                if rel.strength > existing.strength {
                    *existing = rel.clone();
                }
            })
            .or_insert(rel);
    }
    let mut merged: Vec<_> = by_key.into_values().collect();
    merged.sort_by_key(|r| r.created_at);

    Ok(Some(serde_json::to_vec_pretty(&merged)?))
}

fn merge_projects(ours_path: &str, theirs_path: &str) -> Result<Option<Vec<u8>>> {
    let a: Option<Vec<ProjectRow>> = read_json(ours_path)?;
    let b: Option<Vec<ProjectRow>> = read_json(theirs_path)?;
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(None);
    };

    let mut by_id: HashMap<uuid::Uuid, ProjectRow> = HashMap::new();
    for p in a.into_iter().chain(b) {
        by_id.entry(p.id).or_insert(p);
    }
    let mut merged: Vec<_> = by_id.into_values().collect();
    merged.sort_by_key(|p| p.created_at);

    Ok(Some(serde_json::to_vec_pretty(&merged)?))
}

fn merge_episode_file(ours_path: &str, theirs_path: &str) -> Result<Option<Vec<u8>>> {
    let a: Option<EpisodeFile> = read_json(ours_path)?;
    let b: Option<EpisodeFile> = read_json(theirs_path)?;
    let (Some(mut a), Some(b)) = (a, b) else {
        return Ok(None);
    };

    for actor in b.actors {
        if !a.actors.contains(&actor) {
            a.actors.push(actor);
        }
    }
    for artifact in b.artifacts {
        if !a.artifacts.contains(&artifact) {
            a.artifacts.push(artifact);
        }
    }
    a.importance = a.importance.max(b.importance);
    a.ended_at = match (a.ended_at, b.ended_at) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    };

    Ok(Some(serde_json::to_vec_pretty(&a)?))
}

fn merge_error_file(ours_path: &str, theirs_path: &str) -> Result<Option<Vec<u8>>> {
    let a: Option<ErrorFile> = read_json(ours_path)?;
    let b: Option<ErrorFile> = read_json(theirs_path)?;
    let (Some(mut a), Some(b)) = (a, b) else {
        return Ok(None);
    };

    a.resolved = a.resolved || b.resolved;
    a.solution = a.solution.or(b.solution);

    Ok(Some(serde_json::to_vec_pretty(&a)?))
}

#[derive(serde::Deserialize, serde::Serialize)]
struct DecisionFile {
    id: uuid::Uuid,
    content: String,
}

fn merge_decision_file(ours_path: &str, theirs_path: &str) -> Result<Option<Vec<u8>>> {
    let a: Option<DecisionFile> = read_json(ours_path)?;
    let b: Option<DecisionFile> = read_json(theirs_path)?;
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(None);
    };

    let merged = if a.content.is_empty() { b } else { a };
    Ok(Some(serde_json::to_vec_pretty(&merged)?))
}

/// One row of `tombstones.json` as `export_into` writes it. A field this build
/// does not know fails the read: writing the merge back would drop it, so the
/// file is left to a person instead.
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct TombstoneRow {
    table_name: String,
    row_id: uuid::Uuid,
    deleted_at: chrono::DateTime<chrono::Utc>,
    origin_node: Option<String>,
}

/// Both sides' tombstones as one list, keyed like brain_tombstones' primary
/// key, (table_name, row_id). Where both name a row the later deletion stands,
/// as brain_record_tombstone() does with ON CONFLICT (migration 0045); an
/// instant both share goes to the greater origin_node, so that which branch is
/// ours never decides it. Written oldest deletion first, as the export orders
/// it, then by the key: the same two sides give the same bytes either way
/// round, and merging the result again changes nothing.
fn merge_tombstones(ours_path: &str, theirs_path: &str) -> Result<Option<Vec<u8>>> {
    let a: Option<Vec<TombstoneRow>> = read_json(ours_path)?;
    let b: Option<Vec<TombstoneRow>> = read_json(theirs_path)?;
    let (Some(a), Some(b)) = (a, b) else {
        return Ok(None);
    };

    let mut merged: Vec<TombstoneRow> = a.into_iter().chain(b).collect();
    merged.sort_by_key(|t| {
        (
            t.table_name.clone(),
            t.row_id,
            std::cmp::Reverse(t.deleted_at),
            std::cmp::Reverse(t.origin_node.clone()),
        )
    });
    merged
        .dedup_by(|later, kept| later.table_name == kept.table_name && later.row_id == kept.row_id);
    merged.sort_by_key(|t| (t.deleted_at, t.table_name.clone(), t.row_id));

    Ok(Some(serde_json::to_vec_pretty(&merged)?))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str) -> Result<Option<T>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    Ok(serde_json::from_slice(&bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::chunk::ObservationRow;
    use chrono::Utc;
    use uuid::Uuid;

    fn obs(id: Uuid, content: &str) -> ObservationRow {
        ObservationRow {
            id,
            content: content.to_string(),
            observation_type: "fact".to_string(),
            source: "agent".to_string(),
            importance: 0.5,
            tags: vec![],
            project_id: None,
            session_id: None,
            created_at: Utc::now(),
            embedding_model: None,
            ..Default::default()
        }
    }

    fn entity_file(observations: Vec<ObservationRow>) -> EntityFile {
        EntityFile {
            id: Uuid::new_v4(),
            name: "test".to_string(),
            entity_type: "concept".to_string(),
            importance: 0.5,
            access_count: 0,
            project_id: None,
            created_at: Utc::now(),
            observations,
        }
    }

    #[test]
    fn union_merge_keeps_observations_unique_to_each_side() {
        let shared_id = Uuid::new_v4();
        let only_a_id = Uuid::new_v4();
        let only_b_id = Uuid::new_v4();

        let mut a = entity_file(vec![
            obs(shared_id, "shared"),
            obs(only_a_id, "only in ours"),
        ]);
        let b = entity_file(vec![
            obs(shared_id, "shared"),
            obs(only_b_id, "only in theirs"),
        ]);
        a.id = b.id;
        a.name = b.name.clone();

        let dir = std::env::temp_dir().join(format!("cuba-merge-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a_path = dir.join("a.json");
        let b_path = dir.join("b.json");
        std::fs::write(&a_path, serde_json::to_vec(&a).unwrap()).unwrap();
        std::fs::write(&b_path, serde_json::to_vec(&b).unwrap()).unwrap();

        let merged_bytes = merge_entity_file(a_path.to_str().unwrap(), b_path.to_str().unwrap())
            .unwrap()
            .unwrap();
        let merged: EntityFile = serde_json::from_slice(&merged_bytes).unwrap();

        let ids: std::collections::HashSet<_> = merged.observations.iter().map(|o| o.id).collect();
        assert_eq!(ids.len(), 3, "shared + only_a + only_b, deduplicated by id");
        assert!(ids.contains(&shared_id));
        assert!(ids.contains(&only_a_id));
        assert!(ids.contains(&only_b_id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_file_shape_returns_none_and_keeps_ours() {
        let dir = std::env::temp_dir().join(format!("cuba-merge-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a_path = dir.join("manifest.json");
        std::fs::write(&a_path, b"{\"not\":\"an entity file\"}").unwrap();

        let result = merge_entity_file(a_path.to_str().unwrap(), a_path.to_str().unwrap()).unwrap();
        assert!(result.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merge_driver_dispatches_episode_paths_instead_of_silently_keeping_ours() {
        let dir = std::env::temp_dir().join(format!("cuba-merge-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let id = Uuid::new_v4();
        let started = Utc::now();
        let ours = EpisodeFile {
            id,
            entity_id: Uuid::new_v4(),
            content: "pairing session".to_string(),
            actors: vec!["alice".to_string()],
            artifacts: vec![],
            importance: 0.5,
            project_id: None,
            started_at: started,
            ended_at: None,
        };
        let theirs = EpisodeFile {
            actors: vec![],
            ended_at: Some(started + chrono::Duration::hours(1)),
            ..ours.clone()
        };

        let ours_path = dir.join("ours.json");
        let theirs_path = dir.join("theirs.json");
        std::fs::write(&ours_path, serde_json::to_vec(&ours).unwrap()).unwrap();
        std::fs::write(&theirs_path, serde_json::to_vec(&theirs).unwrap()).unwrap();

        let logical_path = format!(".cuba-memorys/episodes/2026-07/{id}.json");
        let args = vec![
            "unused-ancestor".to_string(),
            ours_path.to_str().unwrap().to_string(),
            theirs_path.to_str().unwrap().to_string(),
            logical_path,
        ];
        merge_driver(&args).unwrap();

        let merged: EpisodeFile =
            serde_json::from_slice(&std::fs::read(&ours_path).unwrap()).unwrap();
        assert_eq!(
            merged.actors,
            vec!["alice".to_string()],
            "the actor recorded on our side must not be dropped by the merge"
        );
        assert_eq!(
            merged.ended_at, theirs.ended_at,
            "the session close time recorded on their side must survive the merge \
             instead of silently disappearing with no conflict markers"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The seven merges `merge_driver` can pick, as seen from outside it.
    ///
    /// Each one comes with a pair of sides that only its own merge can parse,
    /// and a test of the file that only its own merge writes back. A merge that
    /// did not run leaves ours as it was; a different merge either cannot parse
    /// the pair or reshapes it into something this test does not accept — the
    /// decision merge reads an episode, for one, and writes back only
    /// `{id, content}`. So running all seven pairs through one path names the
    /// merge that path gets, and names none when it gets none.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Merge {
        Entity,
        Relations,
        Projects,
        Episode,
        Error,
        Decision,
        Tombstones,
    }

    const EVERY_MERGE: [Merge; 7] = [
        Merge::Entity,
        Merge::Relations,
        Merge::Projects,
        Merge::Episode,
        Merge::Error,
        Merge::Decision,
        Merge::Tombstones,
    ];

    fn as_json<T: serde::Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).expect("the fixture serialises")
    }

    impl Merge {
        /// (ours, theirs).
        fn sides(self) -> (serde_json::Value, serde_json::Value) {
            let now = Utc::now();
            match self {
                Merge::Entity => {
                    let ours = entity_file(vec![obs(Uuid::new_v4(), "ours")]);
                    let theirs = EntityFile {
                        observations: vec![obs(Uuid::new_v4(), "theirs")],
                        ..ours.clone()
                    };
                    (as_json(&ours), as_json(&theirs))
                }
                Merge::Relations => {
                    let row = |relation_type: &str| RelationRow {
                        id: Uuid::new_v4(),
                        from_entity: Uuid::new_v4(),
                        to_entity: Uuid::new_v4(),
                        relation_type: relation_type.to_string(),
                        strength: 0.5,
                        bidirectional: false,
                        project_id: None,
                        created_at: now,
                        provenance: "extracted".to_string(),
                    };
                    (as_json(&[row("ours")]), as_json(&[row("theirs")]))
                }
                Merge::Projects => {
                    let row = |name: &str| ProjectRow {
                        id: Uuid::new_v4(),
                        name: name.to_string(),
                        created_at: now,
                    };
                    (as_json(&[row("ours")]), as_json(&[row("theirs")]))
                }
                Merge::Episode => {
                    let ours = EpisodeFile {
                        id: Uuid::new_v4(),
                        entity_id: Uuid::new_v4(),
                        content: "pairing session".to_string(),
                        actors: vec!["alice".to_string()],
                        artifacts: vec![],
                        importance: 0.5,
                        project_id: None,
                        started_at: now,
                        ended_at: None,
                    };
                    let theirs = EpisodeFile {
                        actors: vec!["bob".to_string()],
                        ..ours.clone()
                    };
                    (as_json(&ours), as_json(&theirs))
                }
                Merge::Error => {
                    let ours = ErrorFile {
                        id: Uuid::new_v4(),
                        error_type: "timeout".to_string(),
                        error_message: "the pool never answered".to_string(),
                        solution: None,
                        resolved: false,
                        project: "probe".to_string(),
                        project_id: None,
                        created_at: now,
                    };
                    let theirs = ErrorFile {
                        solution: Some("restart the pool".to_string()),
                        resolved: true,
                        ..ours.clone()
                    };
                    (as_json(&ours), as_json(&theirs))
                }
                Merge::Decision => {
                    let id = Uuid::new_v4();
                    (
                        serde_json::json!({ "id": id, "content": "" }),
                        serde_json::json!({ "id": id, "content": "theirs" }),
                    )
                }
                // Spelled as JSON, the way `export_into` writes it, and not
                // through a type of the driver's: this is what git hands it.
                Merge::Tombstones => {
                    let row = |table: &str| {
                        serde_json::json!({
                            "table_name": table,
                            "row_id": Uuid::new_v4(),
                            "deleted_at": now,
                            "origin_node": null,
                        })
                    };
                    (
                        serde_json::json!([row("brain_observations")]),
                        serde_json::json!([row("brain_relations")]),
                    )
                }
            }
        }

        /// Whether `written` is what this merge makes of `sides()`.
        fn wrote(self, written: &[u8]) -> bool {
            match self {
                Merge::Entity => serde_json::from_slice::<EntityFile>(written)
                    .is_ok_and(|merged| merged.observations.len() == 2),
                Merge::Relations => serde_json::from_slice::<Vec<RelationRow>>(written)
                    .is_ok_and(|merged| merged.len() == 2),
                Merge::Projects => serde_json::from_slice::<Vec<ProjectRow>>(written)
                    .is_ok_and(|merged| merged.len() == 2),
                Merge::Episode => serde_json::from_slice::<EpisodeFile>(written)
                    .is_ok_and(|merged| merged.actors == ["alice", "bob"]),
                Merge::Error => serde_json::from_slice::<ErrorFile>(written).is_ok_and(|merged| {
                    merged.resolved && merged.solution.as_deref() == Some("restart the pool")
                }),
                Merge::Decision => serde_json::from_slice::<DecisionFile>(written)
                    .is_ok_and(|merged| merged.content == "theirs"),
                Merge::Tombstones => serde_json::from_slice::<Vec<serde_json::Value>>(written)
                    .is_ok_and(|merged| {
                        merged.len() == 2 && merged.iter().all(|t| t.get("table_name").is_some())
                    }),
            }
        }
    }

    /// Runs `merge_driver` over `merge`'s sides as git would for `path`, and
    /// says whether that merge is the one that ran.
    ///
    /// The exit status is not what this asks: a path that gets no merge now
    /// exits non-zero so that git leaves a conflict, and that has tests of its
    /// own. Here only what was written back over ours counts.
    fn merged_by(dir: &Path, path: &str, merge: Merge) -> bool {
        let (ours, theirs) = merge.sides();
        let ours_path = dir.join("ours.json");
        let theirs_path = dir.join("theirs.json");
        std::fs::write(&ours_path, serde_json::to_vec(&ours).unwrap()).unwrap();
        std::fs::write(&theirs_path, serde_json::to_vec(&theirs).unwrap()).unwrap();

        let args = [
            "unused-ancestor".to_string(),
            ours_path.to_str().unwrap().to_string(),
            theirs_path.to_str().unwrap().to_string(),
            path.to_string(),
        ];
        let _exit = merge_driver(&args);

        merge.wrote(&std::fs::read(&ours_path).unwrap())
    }

    /// Which merge every path shape gets.
    ///
    /// `merge_driver` chose with `a || b && c` and five `else if`, CC 18 in one
    /// body. This table was that answer for each shape of `%P` git can hand
    /// it, written and run green before a line of the decision moved, so the
    /// move could only be a move. Two groups of rows were defects and were
    /// pinned all the same until the fix that came with its own tests: the
    /// kind is now read from the components at the end of the path — the
    /// layout under the sync root — and not from substrings anywhere in it.
    /// Every row that fix moved says `was:` with the answer it replaced.
    #[test]
    fn merge_driver_picks_the_same_merge_for_every_path_shape() {
        let dir = std::env::temp_dir().join(format!("cuba-merge-table-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let table: [(&str, Option<Merge>); 40] = [
            // The layout `sync export` writes, under both names of its root.
            (".memory-industry/entities/0b6c.json", Some(Merge::Entity)),
            (".cuba-memorys/entities/0b6c.json", Some(Merge::Entity)),
            (".memory-industry/relations.json", Some(Merge::Relations)),
            (".memory-industry/projects.json", Some(Merge::Projects)),
            (
                ".memory-industry/episodes/2026-07/0b6c.json",
                Some(Merge::Episode),
            ),
            (".memory-industry/errors/0b6c.json", Some(Merge::Error)),
            (
                ".memory-industry/decisions/0b6c.json",
                Some(Merge::Decision),
            ),
            // Lowercased before any rule reads it.
            (".memory-industry/ENTITIES/0B6C.JSON", Some(Merge::Entity)),
            (
                ".memory-industry/Episodes/2026-07/x.json",
                Some(Merge::Episode),
            ),
            (".memory-industry/RELATIONS.JSON", Some(Merge::Relations)),
            // The entity rule is the parent directory; the extension never
            // mattered and still does not. The old rule had a second half, a
            // `.json` whose path merely contained `entities` — the half that
            // let a root's own name pick the merge.
            (".memory-industry/entities/0b6c.bak", Some(Merge::Entity)),
            ("entities/0b6c.json", Some(Merge::Entity)),
            // was: Entity, through that second half. Sync writes no such file.
            ("entities.json", None),
            // was: None — the same file as `.memory-industry/entities/0b6c.bak`
            // two rows up, with the sync root at the repository root.
            ("entities/0b6c.bak", Some(Merge::Entity)),
            (".memory-industry/entities.md", None),
            // A sync root at the repository root: no leading slash. These were
            // the first defect — the episode, error and decision rules wanted
            // `/episodes/` with a slash in front, so git kept ours and dropped
            // theirs without a conflict marker.
            ("relations.json", Some(Merge::Relations)),
            ("projects.json", Some(Merge::Projects)),
            // was: None.
            ("episodes/2026-07/x.json", Some(Merge::Episode)),
            // was: None.
            ("errors/x.json", Some(Merge::Error)),
            // was: None.
            ("decisions/x.json", Some(Merge::Decision)),
            // Two rules fit. These two were the second defect: a root whose
            // path says "entities" sent relations.json and projects.json to the
            // entity merge, which cannot parse them, so ours was kept and
            // theirs dropped in silence. The file name is read first now.
            // was: Entity.
            ("entities-archive/relations.json", Some(Merge::Relations)),
            // was: Entity.
            ("my-entities/projects.json", Some(Merge::Projects)),
            // was: Entity, through the `.json`-containing-`entities` half. Its
            // place in the layout is an episode's.
            (
                ".memory-industry/episodes/2026-07/entities.json",
                Some(Merge::Episode),
            ),
            (
                ".memory-industry/errors/relations.json",
                Some(Merge::Relations),
            ),
            (
                ".memory-industry/episodes/projects.json",
                Some(Merge::Projects),
            ),
            (
                ".memory-industry/errors/episodes/x.json",
                Some(Merge::Episode),
            ),
            (
                ".memory-industry/decisions/errors/x.json",
                Some(Merge::Error),
            ),
            // A file name, not a suffix. was: Relations — `ends_with` took
            // any name that finished in `relations.json`.
            (".memory-industry/old-relations.json", None),
            (".memory-industry/relations.json.orig", None),
            // Every export writes tombstones.json, so two branches that both
            // exported conflicted on it in every merge. was: None, all four.
            (".memory-industry/tombstones.json", Some(Merge::Tombstones)),
            ("tombstones.json", Some(Merge::Tombstones)),
            (".memory-industry/TOMBSTONES.JSON", Some(Merge::Tombstones)),
            (
                ".memory-industry/errors/tombstones.json",
                Some(Merge::Tombstones),
            ),
            (".memory-industry/old-tombstones.json", None),
            // Sync writes these too, and they stay a person's to resolve: the
            // import acts on manifest.json's project_id and embedding fields
            // without checking them against the files, and embeddings.bin.zst
            // is read with the width manifest.json declares.
            (".memory-industry/manifest.json", None),
            ("manifest.json", None),
            (".memory-industry/embeddings.bin.zst", None),
            // Nothing sync writes.
            ("README.md", None),
            // Git always hands `%P` over with forward slashes, so a backslash
            // is part of a name, never a separator: neither of these has a
            // directory above its file. was: Entity for the first, through
            // the `.json`-containing-`entities` half.
            (".memory-industry\\entities\\x.json", None),
            (".memory-industry\\episodes\\x.json", None),
        ];

        let mut wrong = Vec::new();
        for (path, expected) in table {
            let seen: Vec<Merge> = EVERY_MERGE
                .into_iter()
                .filter(|&merge| merged_by(&dir, path, merge))
                .collect();
            let expected: Vec<Merge> = expected.into_iter().collect();
            if seen != expected {
                wrong.push(format!("{path}: expected {expected:?}, merged {seen:?}"));
            }
        }

        std::fs::remove_dir_all(&dir).ok();
        assert!(
            wrong.is_empty(),
            "merge_driver no longer picks the merge it picked for these paths:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn a_sync_root_named_like_a_bundle_directory_does_not_pick_the_merge() {
        let dir = std::env::temp_dir().join(format!("cuba-merge-root-name-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // CUBA_SYNC_DIR=entities, =errors, =episodes: the root's own name sits
        // in `%P` in front of the layout, and only the layout may decide.
        let table: [(&str, Merge); 6] = [
            ("entities/relations.json", Merge::Relations),
            ("entities/projects.json", Merge::Projects),
            ("entities/tombstones.json", Merge::Tombstones),
            ("errors/entities/0b6c.json", Merge::Entity),
            ("episodes/errors/0b6c.json", Merge::Error),
            ("entities/episodes/2026-07/0b6c.json", Merge::Episode),
        ];

        let mut wrong = Vec::new();
        for (path, expected) in table {
            let seen: Vec<Merge> = EVERY_MERGE
                .into_iter()
                .filter(|&merge| merged_by(&dir, path, merge))
                .collect();
            if seen != [expected] {
                wrong.push(format!("{path}: expected {expected:?}, merged {seen:?}"));
            }
        }

        std::fs::remove_dir_all(&dir).ok();
        assert!(
            wrong.is_empty(),
            "a sync root is whatever directory CUBA_SYNC_DIR names, so its name is not \
             evidence of what a file under it holds. Read as a substring it was: \
             `entities/relations.json` went to the entity merge, which cannot parse a \
             list of relations, and git kept ours and dropped theirs without a marker:\n{}",
            wrong.join("\n")
        );
    }

    /// Writes `ours` and `theirs` under `dir` and returns the four arguments
    /// git would hand the driver for `path`.
    fn driver_args(dir: &Path, ours: &[u8], theirs: &[u8], path: &str) -> [String; 4] {
        let ours_path = dir.join("ours.json");
        let theirs_path = dir.join("theirs.json");
        std::fs::write(&ours_path, ours).unwrap();
        std::fs::write(&theirs_path, theirs).unwrap();
        [
            "unused-ancestor".to_string(),
            ours_path.to_str().unwrap().to_string(),
            theirs_path.to_str().unwrap().to_string(),
            path.to_string(),
        ]
    }

    #[test]
    fn a_file_no_rule_recognises_is_left_to_git_as_a_conflict() {
        let dir = std::env::temp_dir().join(format!("cuba-merge-unknown-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ours = br#"{"manifest_hash":"ours"}"#;
        let theirs = br#"{"manifest_hash":"theirs"}"#;

        for path in [".memory-industry/manifest.json", "README.md"] {
            let args = driver_args(&dir, ours, theirs, path);

            let exit = merge_driver(&args);

            let err = exit.expect_err(
                "git reads exit 0 as `merged cleanly` and takes whatever is in %A, which is \
                 ours untouched: theirs was dropped with no conflict and nothing to say so. \
                 Non-zero is how a driver tells git to leave the file conflicted instead",
            );
            assert!(
                err.to_string().contains(path),
                "the message is what the person resolving the conflict reads; it has to name \
                 the file: {err}"
            );
            assert_eq!(
                std::fs::read(&args[1]).unwrap(),
                ours,
                "no merge ran for {path}, so nothing may be written over ours"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_recognised_file_whose_sides_do_not_parse_is_left_to_git_as_a_conflict() {
        let dir = std::env::temp_dir().join(format!("cuba-merge-unparsed-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let ours = br#"{"not":"an entity file"}"#;
        let args = driver_args(
            &dir,
            ours,
            br#"{"also not":"an entity file"}"#,
            ".memory-industry/entities/0b6c.json",
        );

        let exit = merge_driver(&args);

        assert!(
            exit.is_err(),
            "the entity merge could not read these sides and wrote nothing; exiting 0 then \
             is the same silent drop of theirs as a path no rule knows"
        );
        assert_eq!(std::fs::read(&args[1]).unwrap(), ours);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// One tombstone, spelled the way `export_into` writes it.
    fn tombstone(
        table: &str,
        row_id: Uuid,
        deleted_at: &str,
        origin_node: Option<&str>,
    ) -> serde_json::Value {
        serde_json::json!({
            "table_name": table,
            "row_id": row_id,
            "deleted_at": deleted_at,
            "origin_node": origin_node,
        })
    }

    type ReadTombstone = (String, Uuid, chrono::DateTime<Utc>, Option<String>);

    /// A tombstones.json read the way `apply_tombstones` reads one, in file
    /// order. A row that does not read here is a row the import would skip.
    fn tombstones_in(bytes: &[u8]) -> Vec<ReadTombstone> {
        let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes).unwrap_or_else(|e| {
            panic!(
                "not a list of tombstones ({e}): {}",
                String::from_utf8_lossy(bytes)
            )
        });
        rows.iter()
            .map(|row| {
                (
                    row["table_name"].as_str().expect("table_name").to_string(),
                    row["row_id"]
                        .as_str()
                        .and_then(|s| Uuid::parse_str(s).ok())
                        .expect("row_id"),
                    row["deleted_at"]
                        .as_str()
                        .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
                        .expect("deleted_at"),
                    row["origin_node"].as_str().map(str::to_string),
                )
            })
            .collect()
    }

    /// The driver over (ours, theirs) as git runs it for tombstones.json: what
    /// it left in %A, or why it refused.
    fn merge_tombstones(dir: &Path, ours: &[u8], theirs: &[u8]) -> Result<Vec<u8>> {
        let args = driver_args(dir, ours, theirs, ".memory-industry/tombstones.json");
        merge_driver(&args)?;
        Ok(std::fs::read(&args[1]).unwrap())
    }

    #[test]
    fn tombstones_merge_into_one_list_keyed_by_table_and_row() {
        let dir = std::env::temp_dir().join(format!("cuba-merge-tombstones-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let [a, b, k] = [(); 3].map(|_| Uuid::new_v4());
        let (low, high) = if a < b { (a, b) } else { (b, a) };
        let (t1, t2, t3) = (
            "2026-08-01T00:00:00Z",
            "2026-08-02T00:00:00Z",
            "2026-08-03T00:00:00Z",
        );
        let (obs, rel) = ("brain_observations", "brain_relations");
        let row = tombstone;
        let list = |rows: &[serde_json::Value]| serde_json::Value::Array(rows.to_vec());

        // (case, ours, theirs, the merge in the order it has to be written:
        // oldest deletion first, as the export writes it, then table, then row)
        let cases: [(
            &str,
            serde_json::Value,
            serde_json::Value,
            serde_json::Value,
        ); 11] = [
            (
                "disjoint",
                list(&[row(obs, a, t1, Some("pc-a"))]),
                list(&[row(obs, b, t2, Some("pc-b"))]),
                list(&[row(obs, a, t1, Some("pc-a")), row(obs, b, t2, Some("pc-b"))]),
            ),
            (
                "disjoint, theirs deleted first",
                list(&[row(obs, b, t2, Some("pc-a"))]),
                list(&[row(obs, a, t1, Some("pc-b"))]),
                list(&[row(obs, a, t1, Some("pc-b")), row(obs, b, t2, Some("pc-a"))]),
            ),
            (
                "overlapping, theirs deleted it again later",
                list(&[row(obs, k, t1, Some("pc-a")), row(obs, a, t2, Some("pc-a"))]),
                list(&[row(obs, k, t3, Some("pc-b"))]),
                list(&[row(obs, a, t2, Some("pc-a")), row(obs, k, t3, Some("pc-b"))]),
            ),
            (
                "overlapping, ours deleted it again later",
                list(&[row(obs, k, t3, Some("pc-a"))]),
                list(&[row(obs, k, t1, Some("pc-b")), row(obs, b, t2, Some("pc-b"))]),
                list(&[row(obs, b, t2, Some("pc-b")), row(obs, k, t3, Some("pc-a"))]),
            ),
            (
                "one id in two tables is two tombstones",
                list(&[row(rel, k, t1, None)]),
                list(&[row(obs, k, t1, None)]),
                list(&[row(obs, k, t1, None), row(rel, k, t1, None)]),
            ),
            (
                "one row, one instant, two nodes",
                list(&[row(obs, k, t1, Some("pc-a"))]),
                list(&[row(obs, k, t1, Some("pc-b"))]),
                list(&[row(obs, k, t1, Some("pc-b"))]),
            ),
            (
                "two rows, one instant",
                list(&[row(obs, high, t1, None)]),
                list(&[row(obs, low, t1, None)]),
                list(&[row(obs, low, t1, None), row(obs, high, t1, None)]),
            ),
            (
                "identical",
                list(&[row(obs, a, t1, None), row(obs, b, t2, None)]),
                list(&[row(obs, a, t1, None), row(obs, b, t2, None)]),
                list(&[row(obs, a, t1, None), row(obs, b, t2, None)]),
            ),
            (
                "ours empty",
                list(&[]),
                list(&[row(obs, a, t1, None)]),
                list(&[row(obs, a, t1, None)]),
            ),
            (
                "theirs empty",
                list(&[row(obs, a, t1, None)]),
                list(&[]),
                list(&[row(obs, a, t1, None)]),
            ),
            ("both empty", list(&[]), list(&[]), list(&[])),
        ];

        let mut wrong = Vec::new();
        for (case, ours, theirs, expected) in cases {
            let ours = serde_json::to_vec_pretty(&ours).unwrap();
            let theirs = serde_json::to_vec_pretty(&theirs).unwrap();
            let expected = tombstones_in(&serde_json::to_vec(&expected).unwrap());

            let merged = match merge_tombstones(&dir, &ours, &theirs) {
                Ok(merged) => merged,
                Err(e) => {
                    wrong.push(format!("{case}: left as a conflict: {e}"));
                    continue;
                }
            };
            if tombstones_in(&merged) != expected {
                wrong.push(format!(
                    "{case}: expected {expected:?}, merged {:?}",
                    tombstones_in(&merged)
                ));
            }
            let swapped = merge_tombstones(&dir, &theirs, &ours).ok();
            if swapped.as_deref() != Some(merged.as_slice()) {
                wrong.push(format!(
                    "{case}: the merge depends on which branch is ours; the other way round \
                     it wrote {:?}",
                    swapped.as_deref().map(String::from_utf8_lossy)
                ));
            }
            for side in [&ours, &theirs] {
                let again = merge_tombstones(&dir, &merged, side).ok();
                if again.as_deref() != Some(merged.as_slice()) {
                    wrong.push(format!(
                        "{case}: merging the result with a side it already holds changed it: \
                         {:?}",
                        again.as_deref().map(String::from_utf8_lossy)
                    ));
                }
            }
        }

        std::fs::remove_dir_all(&dir).ok();
        assert!(
            wrong.is_empty(),
            "every export writes tombstones.json, so two branches that both exported used to \
             stop on it in every merge. The two sides are one set, keyed like brain_tombstones \
             (table_name, row_id), and when both name a row the later deletion stands, as \
             brain_record_tombstone() does with ON CONFLICT. The result may not depend on which \
             side is ours or on how many times it is merged:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn a_tombstones_file_that_does_not_read_as_tombstones_is_left_to_git_as_a_conflict() {
        let dir =
            std::env::temp_dir().join(format!("cuba-merge-bad-tombstones-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = Uuid::new_v4();
        let good = serde_json::to_vec_pretty(&serde_json::json!([tombstone(
            "brain_observations",
            id,
            "2026-08-01T00:00:00Z",
            None
        )]))
        .unwrap();
        let bytes = |value: serde_json::Value| serde_json::to_vec_pretty(&value).unwrap();

        let unreadable: [(&str, Vec<u8>); 7] = [
            ("not a list", br#"{"not":"a list"}"#.to_vec()),
            (
                "cut off mid-row",
                br#"[{"table_name":"brain_observations","#.to_vec(),
            ),
            ("an empty file", Vec::new()),
            (
                "a row with no deleted_at",
                bytes(serde_json::json!([{
                    "table_name": "brain_observations",
                    "row_id": id,
                    "origin_node": null,
                }])),
            ),
            (
                "a row_id that is not a uuid",
                bytes(serde_json::json!([{
                    "table_name": "brain_observations",
                    "row_id": "row-7",
                    "deleted_at": "2026-08-01T00:00:00Z",
                    "origin_node": null,
                }])),
            ),
            (
                "a deleted_at that is not an instant",
                bytes(serde_json::json!([tombstone(
                    "brain_observations",
                    id,
                    "yesterday",
                    None
                )])),
            ),
            (
                "a field this build does not know",
                bytes(serde_json::json!([{
                    "table_name": "brain_observations",
                    "row_id": id,
                    "deleted_at": "2026-08-01T00:00:00Z",
                    "origin_node": null,
                    "reason": "gdpr",
                }])),
            ),
        ];

        let mut wrong = Vec::new();
        for (label, bad) in unreadable {
            for (side, ours, theirs) in [("theirs", &good, &bad), ("ours", &bad, &good)] {
                let args = driver_args(&dir, ours, theirs, ".memory-industry/tombstones.json");
                let exit = merge_driver(&args);
                let left = std::fs::read(&args[1]).unwrap();
                if exit.is_ok() || left != *ours {
                    wrong.push(format!(
                        "{label}, as {side}: exit {exit:?}, left {:?}",
                        String::from_utf8_lossy(&left)
                    ));
                }
            }
        }

        std::fs::remove_dir_all(&dir).ok();
        assert!(
            wrong.is_empty(),
            "a side the merge cannot read whole is a side it would drop rows of, and a dropped \
             tombstone is a deleted row that comes back on the next import. A field it does not \
             know would be dropped by writing the merge back. Each of these has to exit non-zero \
             with ours untouched, so git leaves the file for a person:\n{}",
            wrong.join("\n")
        );
    }

    #[test]
    fn remove_gitattributes_line_matches_the_line_actually_in_the_file_not_the_current_env_var() {
        let dir = std::env::temp_dir().join(format!("cuba-attrs-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".gitattributes"),
            format!("custom-dir/** merge={MERGE_DRIVER_NAME}\n"),
        )
        .unwrap();

        let removed = remove_gitattributes_line(&dir).unwrap();

        assert!(
            removed,
            "must remove the line install() actually wrote, regardless of what \
             CUBA_SYNC_DIR is currently set (or not set) to"
        );
        assert!(
            !dir.join(".gitattributes").exists(),
            "the file only ever held our line — it should be gone, not left orphaned"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A stand-in for a repo root. `install()` writes `.gitattributes` at the git
    /// top level, and `sync export` resolves its own directory against the working
    /// directory, which git sets to that same top level when it runs a hook. So a
    /// single directory is both halves of the question these tests ask.
    fn scratch_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cuba-attrs-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The directory a `.gitattributes` line names, spelled exactly as the line
    /// spells it.
    ///
    /// The shape is checked here rather than asserted on, because it is load-bearing
    /// in the other direction too: `remove_gitattributes_line` uninstalls by dropping
    /// every line that ends in `/** merge=cuba-memorys`, so a line written in any
    /// other shape is a line `hook uninstall` leaves behind for ever.
    fn directory_named_by(line: &str) -> &str {
        let suffix = format!(" merge={MERGE_DRIVER_NAME}");
        let pattern = line.strip_suffix(&suffix).unwrap_or_else(|| {
            panic!("the line must end in `{suffix}`, or uninstall cannot find it again: {line}")
        });
        pattern.strip_suffix("/**").unwrap_or_else(|| {
            panic!("the pattern must cover a whole directory, `<dir>/**`: {pattern}")
        })
    }

    /// The directory a `.gitattributes` line actually hands to the merge driver,
    /// read the way git reads the pattern: relative to the file's own directory.
    ///
    /// Resolving against the root answers *which* directory and deliberately cannot
    /// answer *how it was written*, because `Path::join` drops the base when what it
    /// is handed is already absolute. That second question has its own test, on
    /// `directory_named_by` directly.
    fn directory_covered_by(line: &str, root: &Path) -> PathBuf {
        root.join(directory_named_by(line))
    }

    /// The line of an answer that says the sync directory is in the repo.
    fn inside(answer: AttributeLine) -> String {
        match answer {
            AttributeLine::Inside(line) => line,
            AttributeLine::Outside(dir) => {
                panic!("the sync directory is in this repo, and was reported outside: {dir:?}")
            }
        }
    }

    #[test]
    fn a_fresh_repo_gets_the_driver_on_the_directory_sync_actually_writes_to() {
        let root = scratch_root("fresh");

        let covered = directory_covered_by(&inside(gitattributes_line(&root, None)), &root);

        assert_eq!(
            covered,
            crate::sync::paths::default_sync_dir(&root),
            "the two halves of `hook install` have to name one directory. `sync export` \
             asks sync::paths where to write, and the merge driver only ever reaches the \
             files the `.gitattributes` pattern names. The pattern was built from a second, \
             hand-written copy of that fallback, so on a repo where neither directory \
             exists yet sync writes one and the driver guards the other: install still \
             prints `installed`, and the first time two machines merge the same graph git \
             resolves it with a text merge over JSON instead of the union by id that this \
             whole command exists to arrange. The oracle is sync::paths, not the literal \
             that happens to be right today — a test spelling out the current default goes \
             green again the next time the default moves, which is exactly how this got here"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_repo_that_already_has_the_legacy_directory_is_still_covered() {
        let legacy = scratch_root("legacy");
        std::fs::create_dir_all(legacy.join(".cuba-memorys")).unwrap();
        let fresh = scratch_root("fresh-beside-legacy");

        let covered_legacy =
            directory_covered_by(&inside(gitattributes_line(&legacy, None)), &legacy);
        let covered_fresh = directory_covered_by(&inside(gitattributes_line(&fresh, None)), &fresh);

        assert_eq!(
            covered_legacy,
            crate::sync::paths::default_sync_dir(&legacy),
            "sync keeps writing into a legacy directory that is already on disk, so the \
             pattern has to keep naming it. This is the case that makes the answer NOT \
             `swap the literal for the new name`: that repairs the fresh repo and silently \
             unhooks every installation that works today"
        );
        assert_ne!(
            covered_legacy.file_name(),
            covered_fresh.file_name(),
            "and the answer has to depend on the repo it is run in. sync::paths takes the \
             legacy directory only when it is there and the preferred one is not, so no \
             single constant can be right for both of these roots: whichever one is \
             written, the other repo gets a pattern matching nothing it writes. Legacy \
             covered {covered_legacy:?}, fresh covered {covered_fresh:?}"
        );

        std::fs::remove_dir_all(&legacy).ok();
        std::fs::remove_dir_all(&fresh).ok();
    }

    #[test]
    fn a_configured_sync_root_beats_both_the_default_and_the_legacy_directory() {
        let root = scratch_root("configured");
        std::fs::create_dir_all(root.join(".cuba-memorys")).unwrap();
        std::fs::create_dir_all(root.join(".memory-industry")).unwrap();
        let configured = root.join("graph-sync");
        std::fs::create_dir_all(&configured).unwrap();

        let covered =
            directory_covered_by(&inside(gitattributes_line(&root, Some(&configured))), &root);

        assert_eq!(
            covered, configured,
            "CUBA_SYNC_DIR is where sync puts the graph, full stop: sync::paths takes it as \
             the root and does not nest a default underneath it, so neither of the two \
             directories sitting in this repo is the one being written. Both of them were \
             created here on purpose, so that an implementation which looks at the disk \
             instead of at the configured root is caught rather than flattered"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_pattern_is_written_relative_the_only_way_git_can_read_one() {
        let root = scratch_root("relative");

        let line = inside(gitattributes_line(&root, None));
        let named = directory_named_by(&line);

        assert!(
            Path::new(named).is_relative(),
            "git matches a `.gitattributes` pattern against paths relative to the directory \
             the file sits in, and has no syntax for an absolute one: a pattern starting at \
             the filesystem root matches nothing the repo contains. `default_sync_dir` hands \
             back `root.join(..)`, so the directory arrives here absolute and the only thing \
             between it and a pattern that covers no file at all is stripping the root back \
             off. The three tests above cannot see this, and that is not an oversight in them \
             — they resolve the pattern with `root.join`, which throws the base away when it \
             is handed something already absolute, so both spellings come back as the same \
             directory and both look right. Asserting on the absence of a drive letter or of \
             a leading slash would only name whichever shape this machine's temp dir happens \
             to take; `is_relative` is the one question that means the same thing on Windows \
             and on Linux. Got `{named}` for root {root:?}"
        );
        assert_eq!(
            root.join(named),
            crate::sync::paths::default_sync_dir(&root),
            "and read back against the root it must still land on the directory sync writes \
             to: relative is only half the contract, a pattern can be relative and wrong"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_relative_sync_dir_is_read_against_the_repo_root_and_written_the_way_git_matches() {
        let root = scratch_root("configured-relative");
        let expected = format!("graph-sync/** merge={MERGE_DRIVER_NAME}");

        for configured in [
            "graph-sync",
            "./graph-sync",
            "x/../graph-sync",
            "./x/./../graph-sync/",
        ] {
            assert_eq!(
                gitattributes_line(&root, Some(Path::new(configured))),
                AttributeLine::Inside(expected.clone()),
                "git matches `.gitattributes` patterns against the path from the top of the \
                 work tree, component by component, with no `.` and no `..` in it: \
                 `./graph-sync/**` and `x/../graph-sync/**` are both lines that match nothing \
                 at all. And the directory is the repo root's, not this process's: the hook \
                 runs from the top of the work tree, and `hook install` can be run from any \
                 directory under it — this test runs from the crate's, which is not the repo \
                 under test. CUBA_SYNC_DIR={configured}"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_nested_sync_dir_is_written_with_forward_slashes_on_every_system() {
        let root = scratch_root("configured-nested");

        let line = gitattributes_line(&root, Some(&root.join("data").join("sync")));

        assert_eq!(
            line,
            AttributeLine::Inside(format!("data/sync/** merge={MERGE_DRIVER_NAME}")),
            "a `.gitattributes` pattern separates directories with `/` on Windows too; \
             `Path::display` there prints `data\\sync`, a pattern git reads as one name with \
             an escaped `s` in it, which matches nothing"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_sync_dir_outside_the_repo_gets_no_line_and_says_where_it_is() {
        let root = scratch_root("configured-outside-repo");
        let elsewhere = scratch_root("configured-outside-dir");

        for configured in [
            elsewhere.clone(),
            PathBuf::from("..").join(elsewhere.file_name().unwrap()),
        ] {
            match gitattributes_line(&root, Some(&configured)) {
                AttributeLine::Outside(dir) => assert_eq!(
                    dir.file_name(),
                    elsewhere.file_name(),
                    "the path reported has to be the directory sync writes to, so the \
                     person reading `skipped` knows which one: {dir:?}"
                ),
                AttributeLine::Inside(line) => panic!(
                    "git only ever merges files inside its own work tree, so no line can \
                     reach {configured:?}; writing one anyway is how `install` used to print \
                     `added` over a pattern that matches nothing: {line}"
                ),
            }
        }

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    /// Runs `f` with git pointed at a throwaway repo at `root` and CUBA_SYNC_DIR
    /// set to `sync_dir` (or unset), and hands the environment back the way it
    /// found it.
    ///
    /// `install()` finds its repo with `git rev-parse --show-toplevel` and no
    /// `current_dir`, so it answers from the process working directory — which is
    /// this crate's checkout, and a test must not install hooks there. Moving the
    /// working directory would move it for every test thread that resolves a
    /// relative path; GIT_DIR and GIT_WORK_TREE move only what git reads, and git
    /// is the only thing that reads them. They are still process-wide, so the
    /// caller holds `GLOBAL_STATE_GUARD`, and so does every other test in this
    /// module that spawns git.
    ///
    /// The global and system git configs are cut off because `hooks_dir` reads
    /// `core.hooksPath` without `--local`: on a machine whose global config sets
    /// one, install would write its hook blocks into the operator's own hooks
    /// directory, outside this repo, and the assertions below would read a file
    /// install never touched. CUBA_SYNC_DIR is always set or cleared here because
    /// it replaces the directory under test outright.
    fn in_scratch_repo<R>(root: &Path, sync_dir: Option<&str>, f: impl FnOnce() -> R) -> R {
        let empty_config = root.join("empty-gitconfig");
        std::fs::write(&empty_config, "").unwrap();
        let _no_system = crate::envs::ScopedEnv::set("GIT_CONFIG_NOSYSTEM", "1");
        let _no_global =
            crate::envs::ScopedEnv::set("GIT_CONFIG_GLOBAL", empty_config.to_str().unwrap());
        let _sync_dir = match sync_dir {
            Some(dir) => crate::envs::ScopedEnv::set("CUBA_SYNC_DIR", dir),
            None => crate::envs::ScopedEnv::cleared("CUBA_SYNC_DIR"),
        };

        let init = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(init.success(), "control: the scratch repo has to exist");

        let _repo = crate::envs::ScopedEnv::set("GIT_DIR", root.join(".git").to_str().unwrap());
        let _work_tree = crate::envs::ScopedEnv::set("GIT_WORK_TREE", root.to_str().unwrap());
        f()
    }

    /// The real `install()` in the throwaway repo at `root`; returns what it
    /// reports to the person who ran it.
    fn install_into_scratch_repo(root: &Path, sync_dir: Option<&str>) -> String {
        in_scratch_repo(root, sync_dir, || {
            install(false).expect("install into a scratch repo")
        })
    }

    /// What git itself says the `merge` attribute of `path` is in the repo at
    /// `root`. This is the oracle: a test that reasons about how a pattern
    /// ought to match is how `./dir/**` and `dir\sub/**` shipped as `added`.
    fn merge_attribute(root: &Path, path: &str) -> String {
        let out = Command::new("git")
            .args(["check-attr", "merge", "--", path])
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("empty-gitconfig"))
            // core.attributesFile defaults to $XDG_CONFIG_HOME/git/attributes,
            // read even with the global config cut off.
            .env("XDG_CONFIG_HOME", root.join("no-xdg-config"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git check-attr failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        stdout
            .trim_end()
            .rsplit(": merge: ")
            .next()
            .unwrap()
            .to_string()
    }

    fn is_set_locally(root: &Path, key: &str) -> bool {
        Command::new("git")
            .args(["config", "--local", "--get", key])
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap()
            .success()
    }

    fn read_hook(root: &Path, name: &str) -> String {
        let path = root.join(".git").join("hooks").join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"))
    }

    fn local_git_config(root: &Path, key: &str) -> String {
        let out = Command::new("git")
            .args(["config", "--local", "--get", key])
            .current_dir(root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success(), "{key} is not set in {root:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end()
            .to_string()
    }

    /// The single `.gitattributes` line `install()` left at `root`.
    fn the_one_attribute_line(root: &Path) -> String {
        let written = std::fs::read_to_string(root.join(".gitattributes")).unwrap_or_else(|e| {
            panic!(
                "install() reported success and left no .gitattributes at {root:?} ({e}): the \
                 merge driver is configured and guards no file at all"
            )
        });
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "a fresh repo gets exactly one line: {written:?}"
        );
        lines[0].to_string()
    }

    #[tokio::test]
    async fn install_wires_the_driver_onto_the_directory_sync_writes_in_a_real_repo() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let root = scratch_root("install-fresh");

        install_into_scratch_repo(&root, None);

        let line = the_one_attribute_line(&root);
        let named = directory_named_by(&line);
        assert_eq!(
            merge_attribute(&root, &format!("{named}/entities/0b6c.json")),
            MERGE_DRIVER_NAME,
            "git itself has to hand the files under the sync directory to the driver: {line}"
        );
        assert!(
            Path::new(named).is_relative(),
            "git has no syntax for an absolute pattern: {line}"
        );
        assert_eq!(
            root.join(named),
            crate::sync::paths::default_sync_dir(&root),
            "this is 7383106 end to end. The tests above hold gitattributes_line, the decision; \
             nothing held install(), the wiring, so an install that computed the right line and \
             wrote another, or asked about a different root, or wrote nothing and printed \
             `installed`, left the suite green. On a repo where neither directory exists yet, the \
             line install() actually leaves on disk has to name the directory sync::paths \
             resolves for that same root, or the first merge of two machines' graphs is a text \
             merge over JSON: {line}"
        );

        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            local_git_config(&root, &format!("merge.{MERGE_DRIVER_NAME}.driver")),
            format!("\"{}\" hook merge-driver %O %A %B %P", exe.display()),
            "the attribute names a driver by name; without this entry in .git/config git has \
             nothing to run for it and falls back to its text merge"
        );
        assert_eq!(
            local_git_config(&root, &format!("merge.{MERGE_DRIVER_NAME}.name")),
            "cuba-memorys structural merge (union by id)"
        );

        let hooks = root.join(".git").join("hooks");
        let post_commit = std::fs::read_to_string(hooks.join("post-commit")).unwrap();
        assert!(
            post_commit.contains(MARKER) && post_commit.contains("sync export --scope all"),
            "post-commit is what keeps the graph on disk current: {post_commit}"
        );
        assert!(
            !post_commit.contains("codegraph build"),
            "codegraph on commit is opt-in and was not asked for: {post_commit}"
        );
        let post_checkout = std::fs::read_to_string(hooks.join("post-checkout")).unwrap();
        assert!(
            post_checkout.contains(MARKER)
                && post_checkout.contains("sync import --conflict merge"),
            "post-checkout is what pulls a switched branch's graph back in: {post_checkout}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn install_in_a_repo_with_the_legacy_directory_keeps_guarding_it() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let root = scratch_root("install-legacy");
        std::fs::create_dir_all(root.join(".cuba-memorys")).unwrap();

        install_into_scratch_repo(&root, None);

        let line = the_one_attribute_line(&root);
        assert_eq!(
            root.join(directory_named_by(&line)),
            crate::sync::paths::default_sync_dir(&root),
            "the fresh-repo test cannot tell which root install() asked about: every root \
             without a legacy directory resolves to the same relative name, so an install that \
             consulted the process working directory instead of the repo it found would pass \
             it. Here the answer depends on what is on disk in THIS repo: {line}"
        );
        assert_eq!(
            crate::sync::paths::default_sync_dir(&root).file_name(),
            Some(std::ffi::OsStr::new(".cuba-memorys")),
            "control: sync::paths has to see the legacy directory here, or this test is the \
             fresh one twice"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn install_writes_a_line_git_matches_for_every_spelling_of_a_sync_dir_in_the_repo() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        // (label, CUBA_SYNC_DIR for a given repo root, the directory git must see)
        type Case = (&'static str, fn(&Path) -> String, &'static str);
        let cases: [Case; 4] = [
            (
                "dot-slash",
                |_: &Path| "./graph-sync".to_string(),
                "graph-sync",
            ),
            (
                "dot-dot",
                |_: &Path| "x/../graph-sync".to_string(),
                "graph-sync",
            ),
            (
                "nested-relative",
                |_: &Path| "data/sync".to_string(),
                "data/sync",
            ),
            (
                "nested-absolute",
                |root: &Path| root.join("data").join("sync").display().to_string(),
                "data/sync",
            ),
        ];

        let mut wrong = Vec::new();
        for (label, sync_dir_for_root, dir) in cases {
            let root = scratch_root(&format!("install-{label}"));
            let configured = sync_dir_for_root(&root);

            install_into_scratch_repo(&root, Some(&configured));

            let line = the_one_attribute_line(&root);
            let expected = format!("{dir}/** merge={MERGE_DRIVER_NAME}");
            let attribute = merge_attribute(&root, &format!("{dir}/entities/0b6c.json"));
            if line != expected || attribute != MERGE_DRIVER_NAME {
                wrong.push(format!(
                    "CUBA_SYNC_DIR={configured}: wrote `{line}`, expected `{expected}`; \
                     git check-attr says merge: {attribute}"
                ));
            }
            std::fs::remove_dir_all(&root).ok();
        }

        assert!(
            wrong.is_empty(),
            "git only matches a pattern spelled from the top of the work tree with `/` and \
             no `.` or `..`. Each of these used to be written as given — `./graph-sync/**`, \
             `x/../graph-sync/**`, `data\\sync/**` on Windows — and `install` printed `added` \
             over a line that hands the driver no file at all, so the first merge of two \
             machines' graphs was a text merge over JSON:\n{}",
            wrong.join("\n")
        );
    }

    #[tokio::test]
    async fn install_with_the_sync_dir_outside_the_repo_installs_the_hooks_and_no_driver() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let root = scratch_root("install-outside");
        let elsewhere = scratch_root("install-outside-target");
        let elsewhere_name = elsewhere.file_name().unwrap().to_str().unwrap().to_string();

        let report = install_into_scratch_repo(&root, Some(elsewhere.to_str().unwrap()));

        assert!(
            !root.join(".gitattributes").exists(),
            "no pattern in this repo can reach {elsewhere:?}: git only merges files in its own \
             work tree. A line written anyway matches nothing and reads as protection"
        );
        assert!(
            !is_set_locally(&root, &format!("merge.{MERGE_DRIVER_NAME}.driver")),
            "a driver with no line pointing at it is configuration for nothing"
        );
        let attributes_line = report
            .lines()
            .find(|l| l.starts_with(".gitattributes:"))
            .unwrap_or_else(|| panic!("the report says nothing about .gitattributes:\n{report}"));
        assert!(
            attributes_line.contains("skipped")
                && attributes_line.contains(&elsewhere_name)
                && attributes_line.contains("outside this repo; git never merges it"),
            "the person who ran install has to learn that git will not merge the graph, and \
             where it is — a silent skip is the old silent dead line again:\n{report}"
        );
        for (hook, action) in [
            ("post-commit", "sync export --scope all"),
            ("post-checkout", "sync import --conflict merge"),
        ] {
            let body = read_hook(&root, hook);
            assert!(
                body.contains(MARKER) && body.contains(action) && body.contains(&elsewhere_name),
                "the hooks still export to and import from the shared directory — that is \
                 what CUBA_SYNC_DIR outside the repo is for — and they have to name it: {body}"
            );
        }

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
    }

    #[tokio::test]
    async fn install_replaces_the_merge_driver_lines_an_earlier_install_left() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let root = scratch_root("install-stale-lines");
        let absolute = root.join(".memory-industry");
        std::fs::write(
            root.join(".gitattributes"),
            format!(
                "*.png binary\n\
                 {}/** merge={MERGE_DRIVER_NAME}\n\
                 ./.memory-industry/** merge={MERGE_DRIVER_NAME}\n",
                absolute.display()
            ),
        )
        .unwrap();

        install_into_scratch_repo(&root, None);

        let written = std::fs::read_to_string(root.join(".gitattributes")).unwrap();
        let driver_lines: Vec<&str> = written
            .lines()
            .filter(|l| l.contains(&format!("merge={MERGE_DRIVER_NAME}")))
            .collect();
        assert_eq!(
            driver_lines,
            [format!(".memory-industry/** merge={MERGE_DRIVER_NAME}")],
            "the lines earlier installs wrote in the shapes git cannot match have to go: kept, \
             they pile up one per install and each reads like a guard. Only ours may stay:\n\
             {written}"
        );
        assert!(
            written.lines().any(|l| l == "*.png binary"),
            "a line that is not the driver's is not install's to touch:\n{written}"
        );
        assert_eq!(
            merge_attribute(&root, ".memory-industry/entities/0b6c.json"),
            MERGE_DRIVER_NAME
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_reinstall_moves_the_pinned_sync_dir_and_keeps_one_block() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let root = scratch_root("install-twice");

        install_into_scratch_repo(&root, Some("first-sync"));
        install_into_scratch_repo(&root, Some("second-sync"));

        for hook in ["post-commit", "post-checkout"] {
            let body = read_hook(&root, hook);
            assert_eq!(
                body.matches(MARKER).count(),
                1,
                "one block per hook, however many installs: {body}"
            );
            assert!(
                body.contains("CUBA_SYNC_DIR=")
                    && body.contains("second-sync")
                    && !body.contains("first-sync"),
                "the hook fixes CUBA_SYNC_DIR so that a commit from an IDE with another \
                 environment exports where this repo's merge driver looks. A reinstall with a \
                 new directory moves the line to it, so the block has to move with it; a hook \
                 still pinned to the first one exports where nothing guards it: {body}"
            );
        }
        assert_eq!(
            the_one_attribute_line(&root),
            format!("second-sync/** merge={MERGE_DRIVER_NAME}")
        );

        let before = read_hook(&root, "post-commit");
        let report = install_into_scratch_repo(&root, Some("second-sync"));
        assert_eq!(
            read_hook(&root, "post-commit"),
            before,
            "the same install twice changes nothing"
        );
        assert!(
            report
                .lines()
                .any(|l| l.starts_with("post-commit hook:") && l.contains("already present")),
            "and says so:\n{report}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn uninstall_takes_the_pinned_sync_dir_with_the_rest_of_the_block() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let root = scratch_root("install-uninstall");

        in_scratch_repo(&root, Some("graph-sync"), || {
            install(false).expect("install");
            uninstall().expect("uninstall");
        });

        let hooks = root.join(".git").join("hooks");
        for hook in ["post-commit", "post-checkout"] {
            assert!(
                !hooks.join(hook).exists(),
                "the file held nothing but install's block, the pinned CUBA_SYNC_DIR included; \
                 uninstall has to leave no trace of it"
            );
        }
        assert!(!root.join(".gitattributes").exists());
        assert!(!is_set_locally(
            &root,
            &format!("merge.{MERGE_DRIVER_NAME}.driver")
        ));

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_sync_dir_with_a_space_and_a_quote_survives_the_attributes_file_and_the_hook() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let root = scratch_root("install-odd-name");
        let odd = root.join("it's a dir");
        std::fs::create_dir_all(&odd).unwrap();

        install_into_scratch_repo(&root, Some(odd.to_str().unwrap()));

        assert_eq!(
            merge_attribute(&root, "it's a dir/entities/0b6c.json"),
            MERGE_DRIVER_NAME,
            "whitespace ends a `.gitattributes` pattern, so `it's a dir/**` bare is the \
             pattern `it's` followed by two attributes git refuses; the directory needs the \
             C-style quotes git accepts: {:?}",
            std::fs::read_to_string(root.join(".gitattributes"))
        );
        let post_commit = read_hook(&root, "post-commit");
        assert!(
            post_commit.contains("CUBA_SYNC_DIR="),
            "the hook has to fix the directory: {post_commit}"
        );
        #[cfg(unix)]
        assert_eq!(
            sync_dir_a_hook_exports(&root.join(".git").join("hooks").join("post-commit"), &root),
            odd.canonicalize().unwrap().display().to_string(),
            "a quote or a space in the path must reach the program the hook starts as one \
             word, unchanged: {post_commit}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Checks `hook` parses under `sh -n`, then runs it the way git does — `sh`,
    /// from the top of the work tree, no database configured so nothing is
    /// started — and returns the `CUBA_SYNC_DIR` a program it started would see.
    #[cfg(unix)]
    fn sync_dir_a_hook_exports(hook: &Path, root: &Path) -> String {
        let syntax = Command::new("sh").arg("-n").arg(hook).output().unwrap();
        assert!(
            syntax.status.success(),
            "sh -n refuses the hook install wrote: {}",
            String::from_utf8_lossy(&syntax.stderr)
        );
        let out = Command::new("sh")
            .args([
                "-c",
                ". \"$1\"; exec sh -c 'printf %s \"$CUBA_SYNC_DIR\"'",
                "sh",
            ])
            .arg(hook)
            .current_dir(root)
            .env_remove("CUBA_SYNC_DIR")
            .env_remove("DATABASE_URL")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("empty-gitconfig"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the hook failed under sh: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn remove_hook_block_deletes_the_file_when_our_block_was_the_only_content() {
        let dir = std::env::temp_dir().join(format!("cuba-hook-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("post-commit");
        std::fs::write(
            &path,
            format!("#!/bin/sh\n\n{MARKER}\nsome generated line\nfi\n"),
        )
        .unwrap();

        let removed = remove_hook_block(&path).unwrap();
        assert!(removed);
        assert!(
            !path.exists(),
            "nothing but our block was there — the file should be gone"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_hook_block_preserves_a_pre_existing_hook_before_our_marker() {
        let dir = std::env::temp_dir().join(format!("cuba-hook-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("post-commit");
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho 'pre-existing hook'\n\n{MARKER}\nsome generated line\nfi\n"),
        )
        .unwrap();

        let removed = remove_hook_block(&path).unwrap();
        assert!(removed);
        let remaining = std::fs::read_to_string(&path).unwrap();
        assert!(remaining.contains("pre-existing hook"));
        assert!(!remaining.contains(MARKER));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_hook_block_preserves_content_appended_without_a_blank_line() {
        let dir = std::env::temp_dir().join(format!("cuba-hook-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("post-commit");
        std::fs::write(
            &path,
            format!("#!/bin/sh\n\n{MARKER}\nsome generated line\nfi\nmy-custom-step\n"),
        )
        .unwrap();

        let removed = remove_hook_block(&path).unwrap();
        assert!(removed);
        let remaining = std::fs::read_to_string(&path).unwrap();
        assert!(
            remaining.contains("my-custom-step"),
            "content appended after our block without a blank line must survive uninstall"
        );
        assert!(!remaining.contains(MARKER));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_hook_block_on_a_file_without_our_marker_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("cuba-hook-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("post-commit");
        std::fs::write(&path, "#!/bin/sh\necho 'someone else's hook'\n").unwrap();

        let removed = remove_hook_block(&path).unwrap();
        assert!(!removed);
        assert!(path.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_hook_block_on_a_missing_file_is_a_no_op() {
        let path = std::env::temp_dir().join(format!("cuba-hook-nonexistent-{}", Uuid::new_v4()));
        assert!(!remove_hook_block(&path).unwrap());
    }

    #[test]
    fn append_hook_block_errors_instead_of_overwriting_non_utf8_existing_hook() {
        let dir = std::env::temp_dir().join(format!("cuba-hook-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("post-commit");
        let original_bytes = [0x23, 0x21, 0x2f, 0x62, 0x69, 0x6e, 0xff, 0xfe];
        std::fs::write(&path, original_bytes).unwrap();

        let result = append_hook_block(&path, "some generated line\n");

        assert!(
            result.is_err(),
            "a pre-existing hook that isn't valid UTF-8 must error, not be silently \
             treated as empty and overwritten"
        );
        let remaining = std::fs::read(&path).unwrap();
        assert_eq!(
            remaining, original_bytes,
            "the pre-existing hook's content must survive untouched"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn install_rejects_an_unrecognized_flag_instead_of_silently_ignoring_it() {
        let args = vec!["install".to_string(), "--with-codgraph".to_string()];
        let err = run_cli(&args)
            .await
            .expect_err("a typo'd flag must be a hard error, not a silent no-op");
        assert!(
            err.to_string().contains("--with-codgraph"),
            "error should name the offending flag, got: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn git_config_returns_err_when_the_git_process_exits_non_zero() {
        // The guard is for `install_into_scratch_repo`: while it holds GIT_DIR, a
        // `git init` or `git config` spawned here would act on that repo instead.
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        // Do NOT rely on chmod 555 of `.git`: the merge gate often runs as root in WSL,
        // and root bypasses directory mode bits, so that setup falsely stays green.
        // Replacing `.git/config` with a directory makes `git config --local` exit
        // non-zero even for uid 0.
        let dir = std::env::temp_dir().join(format!("cuba-git-config-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let init_status = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(init_status.success());

        let config = dir.join(".git").join("config");
        std::fs::remove_file(&config).unwrap();
        std::fs::create_dir(&config).unwrap();

        let result = git_config(&dir, "merge.cuba-memorys-test.name", "irrelevant value");

        assert!(
            result.is_err(),
            "git config --local must return Err when git exits non-zero, got Ok"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
