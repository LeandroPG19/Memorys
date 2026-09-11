use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const ENV_VAR: &str = "CUBA_SYNC_DIR";
const DEFAULT_DIR: &str = ".memory-industry";
const LEGACY_DIR: &str = ".cuba-memorys";

fn configured_root() -> Option<PathBuf> {
    std::env::var(ENV_VAR)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn default_sync_dir(root: &Path) -> PathBuf {
    let preferred = root.join(DEFAULT_DIR);
    let legacy = root.join(LEGACY_DIR);
    if preferred.exists() || !legacy.exists() {
        preferred
    } else {
        legacy
    }
}

fn target_under_root(
    root: &Path,
    override_arg: Option<&str>,
    root_is_explicit: bool,
) -> Result<PathBuf> {
    let candidate = match override_arg.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => {
            let requested = PathBuf::from(raw);
            if requested.is_absolute() {
                requested
            } else {
                root.join(requested)
            }
        }
        None if root_is_explicit => root.to_path_buf(),
        None => default_sync_dir(root),
    };
    ensure_within(root, &candidate).with_context(|| {
        format!("sync directories are confined to {root:?}. Set {ENV_VAR} to work somewhere else")
    })?;
    Ok(candidate)
}

pub fn resolve_dir(override_arg: Option<&str>) -> Result<PathBuf> {
    let explicit = configured_root();
    let root_is_explicit = explicit.is_some();
    let root = match explicit {
        Some(r) => r,
        None => std::env::current_dir().context("resolving the working directory as sync root")?,
    };
    if !root.exists() {
        std::fs::create_dir_all(&root).with_context(|| format!("creating sync root {root:?}"))?;
    }

    let path = target_under_root(&root, override_arg, root_is_explicit)?;
    if !path.exists() {
        std::fs::create_dir_all(&path).with_context(|| format!("creating sync dir {path:?}"))?;
    }
    path.canonicalize()
        .with_context(|| format!("canonicalize {path:?}"))
}

const MAX_SLUG_CHARS: usize = 120;

pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        return "entity".to_string();
    }
    let capped: String = trimmed.chars().take(MAX_SLUG_CHARS).collect();
    capped.trim_end_matches('-').to_string()
}

pub fn ensure_within(root: &Path, candidate: &Path) -> Result<()> {
    let root_cmp = for_containment(root);
    let normalized = for_containment(&lexical_join(&root_cmp, candidate));
    if !normalized.starts_with(&root_cmp) {
        anyhow::bail!("path traversal blocked: {candidate:?} escapes root {root:?}");
    }

    let existing = candidate
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(candidate);
    if let Ok(real) = existing.canonicalize()
        && let Ok(real_root) = root.canonicalize()
    {
        let real = for_containment(&real);
        let real_root = for_containment(&real_root);
        if !real.starts_with(&real_root) {
            anyhow::bail!("path traversal blocked: {candidate:?} resolves outside {root:?}");
        }
    }
    Ok(())
}

/// Windows `canonicalize` yields `\\?\C:\...` while `CUBA_SYNC_DIR` is usually plain
/// `C:\...`. `Path::starts_with` treats those as different prefixes, so a path under
/// the sync root looks like an escape. Strip the verbatim prefix before comparing.
fn for_containment(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            if let Some(unc) = rest.strip_prefix("UNC\\") {
                return PathBuf::from(format!(r"\\{unc}"));
            }
            return PathBuf::from(rest.to_string());
        }
    }
    path.to_path_buf()
}

fn lexical_join(root: &Path, candidate: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = if candidate.is_absolute() {
        PathBuf::new()
    } else {
        root.to_path_buf()
    };

    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_name_cannot_produce_a_filename_the_kernel_refuses() {
        let long = "Postgres ".repeat(60);
        let out = slug(&long);
        assert!(
            out.chars().count() <= MAX_SLUG_CHARS,
            "the bundle names files slug(name)-{{id8}}.json, and ext4 stops at NAME_MAX=255. \
             An untruncated slug did not corrupt one entity: it made `cuba_sync export` fail \
             outright with `File name too long`, so a single long name blocked the whole node \
             from syncing until somebody renamed it. Got {} chars",
            out.chars().count()
        );
        assert!(
            !out.ends_with('-'),
            "and cutting mid-separator would leave a trailing dash that changes the filename \
             for no reason: {out}"
        );
        assert!(
            out.starts_with("postgres"),
            "truncating must keep the front, which is what a person recognises: {out}"
        );
    }

    #[test]
    fn two_long_names_that_share_a_prefix_still_get_different_files() {
        let a = format!("{} alpha", "x".repeat(200));
        let b = format!("{} beta", "x".repeat(200));
        assert_eq!(
            slug(&a),
            slug(&b),
            "truncation makes these collide, which is fine and expected — the filename carries \
             an 8-hex id suffix precisely so the slug never has to be unique. This assertion \
             exists so that anyone who later makes the slug unique knows the suffix was already \
             doing that job"
        );
    }

    #[test]
    fn test_slug_basic() {
        assert_eq!(slug("Postgres Database"), "postgres-database");
        assert_eq!(slug("foo/../bar"), "foo-bar");
        assert_eq!(slug("---"), "entity");
        assert_eq!(slug(""), "entity");
        assert_eq!(slug("Auth_Flow-v2"), "auth_flow-v2");
    }

    #[test]
    fn verbatim_windows_prefix_does_not_look_like_an_escape() {
        let root = PathBuf::from(r"C:\Users\someone\AppData\Local\Temp\cuba-sync");
        let inside =
            PathBuf::from(r"\\?\C:\Users\someone\AppData\Local\Temp\cuba-sync\.peer-inbox");
        ensure_within(&root, &inside).expect(
            "canonicalize on Windows adds \\\\?\\; confinement must still see a child of the root",
        );
    }

    fn root() -> PathBuf {
        PathBuf::from("/home/someone/repo")
    }

    #[test]
    fn a_caller_supplied_absolute_path_cannot_leave_the_root() {
        for escape in ["/etc", "/tmp/anywhere", "/home/someone/other-repo"] {
            let denied = target_under_root(&root(), Some(escape), false);
            assert!(
                denied.is_err(),
                "cuba_sync takes `dir` straight from the MCP tool arguments and then \
                 creates it, writes the graph into it and deletes *.json from it — an \
                 absolute path would make that remote file write: {escape}"
            );
        }
    }

    #[test]
    fn dot_dot_cannot_climb_out_of_the_root() {
        for escape in ["../elsewhere", "sub/../../elsewhere", "./../.."] {
            assert!(
                target_under_root(&root(), Some(escape), false).is_err(),
                "{escape} normalizes above the root"
            );
        }
    }

    #[test]
    fn a_path_inside_the_root_is_still_allowed() {
        let ok = target_under_root(&root(), Some("exports/backup"), false)
            .expect("a subdirectory of the root is the normal case");
        assert_eq!(ok, root().join("exports/backup"));

        let dotted = target_under_root(&root(), Some("./exports/../exports"), false)
            .expect("a path that normalizes back inside is fine");
        assert_eq!(dotted, root().join("./exports/../exports"));
    }

    #[test]
    fn without_an_override_the_default_stays_where_it_always_was() {
        let implicit = target_under_root(&root(), None, false).expect("default must resolve");
        assert_eq!(
            implicit,
            root().join(DEFAULT_DIR),
            "the historical default is {DEFAULT_DIR} under the working directory, and \
             confining the root must not move it"
        );

        let configured = target_under_root(&root(), None, true).expect("explicit root resolves");
        assert_eq!(
            configured,
            *root(),
            "with {ENV_VAR} set, that directory IS the sync dir — it is not nested again"
        );
    }

    #[test]
    fn an_explicit_root_is_the_way_to_work_outside_the_working_directory() {
        let elsewhere = PathBuf::from("/var/backups/cuba");
        let inside = target_under_root(&elsewhere, Some("nightly"), true)
            .expect("a subdirectory of the configured root is allowed");
        assert_eq!(inside, elsewhere.join("nightly"));

        assert!(
            target_under_root(&elsewhere, Some("/etc"), true).is_err(),
            "the environment moves the root; it does not remove the confinement"
        );
    }

    #[test]
    fn an_empty_override_is_not_an_override() {
        for blank in ["", "   "] {
            let resolved =
                target_under_root(&root(), Some(blank), false).expect("blank falls through");
            assert_eq!(
                resolved,
                root().join(DEFAULT_DIR),
                "a blank string must not resolve to the root itself"
            );
        }
    }
}
