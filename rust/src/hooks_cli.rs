use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::sync::chunk::{EntityFile, EpisodeFile, ErrorFile, ProjectRow, RelationRow};

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
            install(with_codegraph)
        }
        Some("uninstall") => uninstall(),
        Some("merge-driver") => merge_driver(&args[1..]),
        Some("-h") | Some("--help") | None => {
            eprintln!(
                "usage: cuba-memorys hook <install|uninstall> [--with-codegraph]\n\n\
                 Wires this repo's git so the knowledge graph under .cuba-memorys/\n\
                 (or $CUBA_SYNC_DIR) stays in sync automatically:\n\
                 \x20 - post-commit  runs `sync export` after every commit\n\
                 \x20 - post-checkout runs `sync import` after checkout/branch switch\n\
                 \x20 - a git merge driver that unions observations/relations/entities\n\
                 \x20   by id instead of leaving conflict markers in graph JSON\n\n\
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

fn append_gitattributes_line(root: &Path, line: &str) -> Result<bool> {
    let path = root.join(".gitattributes");
    let existing = read_existing_or_empty(&path)?;
    if existing.lines().any(|l| l.trim() == line.trim()) {
        return Ok(false);
    }
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(line);
    body.push('\n');
    std::fs::write(&path, body).with_context(|| format!("writing {path:?}"))?;
    Ok(true)
}

fn remove_gitattributes_line(root: &Path) -> Result<bool> {
    let attr_suffix = format!("/** merge={MERGE_DRIVER_NAME}");
    let path = root.join(".gitattributes");
    if !path.exists() {
        return Ok(false);
    }
    let existing = read_existing_or_empty(&path)?;
    let filtered: Vec<&str> = existing
        .lines()
        .filter(|l| !l.trim().ends_with(&attr_suffix))
        .collect();
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

fn install(with_codegraph: bool) -> Result<()> {
    let root = git_root()?;
    let hooks = hooks_dir(&root);
    std::fs::create_dir_all(&hooks).context("creating hooks dir")?;

    let exe = std::env::current_exe().context("resolving path to this binary")?;
    let exe = exe.display();

    let resolve_url_sh = "db_url=$(git config --local --get cuba-memorys.database-url 2>/dev/null || true); [ -z \"$db_url\" ] && db_url=\"$DATABASE_URL\"";
    let codegraph_line = if with_codegraph {
        format!(
            " DATABASE_URL=\"$db_url\" \"{exe}\" codegraph build --lang rust,python >/dev/null 2>&1 || true\n"
        )
    } else {
        String::new()
    };
    let post_commit_block = format!(
        "{MARKER}\n\
         {resolve_url_sh}\n\
         if [ -n \"$db_url\" ]; then\n\
         \x20 DATABASE_URL=\"$db_url\" \"{exe}\" sync export --scope all >/dev/null 2>&1 || true\n\
         {codegraph_line}\
         fi\n"
    );
    let commit_changed = append_hook_block(&hooks.join("post-commit"), &post_commit_block)?;

    let post_checkout_block = format!(
        "{MARKER}\n\
         {resolve_url_sh}\n\
         if [ -n \"$db_url\" ]; then\n\
         \x20 DATABASE_URL=\"$db_url\" \"{exe}\" sync import --conflict merge >/dev/null 2>&1 || true\n\
         fi\n"
    );
    let checkout_changed = append_hook_block(&hooks.join("post-checkout"), &post_checkout_block)?;

    git_config(
        &root,
        &format!("merge.{MERGE_DRIVER_NAME}.name"),
        "cuba-memorys structural merge (union by id)",
    )?;
    git_config(
        &root,
        &format!("merge.{MERGE_DRIVER_NAME}.driver"),
        &format!("\"{exe}\" hook merge-driver %O %A %B %P"),
    )?;

    let sync_dir = std::env::var("CUBA_SYNC_DIR").unwrap_or_else(|_| ".cuba-memorys".to_string());
    let attr_line = format!("{sync_dir}/** merge={MERGE_DRIVER_NAME}");
    let attrs_changed = append_gitattributes_line(&root, &attr_line)?;

    println!(
        "post-commit hook:   {}",
        if commit_changed {
            "installed"
        } else {
            "already present"
        }
    );
    println!(
        "post-checkout hook: {}",
        if checkout_changed {
            "installed"
        } else {
            "already present"
        }
    );
    println!("merge driver:       configured (merge.{MERGE_DRIVER_NAME}.driver in .git/config)");
    println!(
        ".gitattributes:     {}",
        if attrs_changed {
            format!("added `{attr_line}`")
        } else {
            "already present".to_string()
        }
    );
    println!(
        "codegraph on commit: {}",
        if with_codegraph {
            "enabled"
        } else {
            "disabled (pass --with-codegraph to enable)"
        }
    );
    println!(
        "\nNOTE: both hooks are a no-op until this repo's database is set explicitly.\n\
         They deliberately do NOT fall back to auto-detecting a running container —\n\
         on a machine with more than one cuba-memorys database, that guess can export\n\
         from, or import into, the wrong one. Set it once, it persists in .git/config:\n\
         \x20 git config --local cuba-memorys.database-url \"postgresql://...\"\n\
         (DATABASE_URL in the environment also works as a fallback.)"
    );
    Ok(())
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

fn merge_driver(args: &[String]) -> Result<()> {
    let [ancestor, ours, theirs, path] = args else {
        anyhow::bail!("usage: hook merge-driver %O %A %B %P (git passes these itself)");
    };

    let path_lower = path.to_lowercase();
    let merged: Option<Vec<u8>> = if path_lower.contains("/entities/")
        || path_lower.ends_with(".json") && path_lower.contains("entities")
    {
        merge_entity_file(ours, theirs)?
    } else if path_lower.ends_with("relations.json") {
        merge_relations(ours, theirs)?
    } else if path_lower.ends_with("projects.json") {
        merge_projects(ours, theirs)?
    } else if path_lower.contains("/episodes/") {
        merge_episode_file(ours, theirs)?
    } else if path_lower.contains("/errors/") {
        merge_error_file(ours, theirs)?
    } else if path_lower.contains("/decisions/") {
        merge_decision_file(ours, theirs)?
    } else {
        None
    };

    let _ = ancestor;

    match merged {
        Some(bytes) => {
            std::fs::write(ours, bytes).with_context(|| format!("writing merged {ours}"))?;
            Ok(())
        }
        None => Ok(()),
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
    #[test]
    fn git_config_returns_err_when_the_git_process_exits_non_zero() {
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
