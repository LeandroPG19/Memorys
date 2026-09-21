//! Environment lookups shared by the whole crate.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Prefer the MemoryIndustry name; fall back to the legacy `CUBA_*` one for a
/// release.
///
/// This lived private in `cognitive::judge`, which is why the rename shipped in
/// halves: the judge knobs answered to both names while the three that decide
/// whether the card gets used at all — the arena cap, the reranker placement
/// and the warm-up — answered only to `CUBA_*`. A machine configured from the
/// documented namespace read none of them, so the negative control that proves
/// the arena ceiling works (`MEMORY_INDUSTRY_GPU_MEM_LIMIT_MB=512` must be
/// raised, loudly) was silently ignored and looked like the fix had not landed.
pub fn alias(new_key: &str, legacy_key: &str) -> Result<String, std::env::VarError> {
    std::env::var(new_key).or_else(|_| std::env::var(legacy_key))
}

/// Where the operator's configuration lives, or an error naming both variables.
///
/// Falling back to `"."` was worse than failing. PowerShell and cmd.exe do not
/// define HOME — only Git Bash does, which is why this never showed up here —
/// so on Windows `setup claude --apply` *succeeded* and left `.claude.json` in
/// whatever directory the operator happened to be standing in, where no MCP
/// client ever looks. The reasonable conclusion was "this does not work", and
/// the first LAN deployment went on to write its configs by hand.
///
/// HOMEDRIVE+HOMEPATH is deliberately not a third try: on a domain-joined
/// machine that pair comes from the AD home-directory attribute and can point
/// at a share that is not mounted, which is this same defect with a longer
/// path. The other ten sites in this crate read exactly HOME then USERPROFILE.
pub fn home() -> Result<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .context(
            "no sé dónde está tu home: ni HOME ni USERPROFILE están definidas. \
             Definí una de las dos y repetí — escribir esto en el directorio actual \
             lo dejaría donde ningún cliente MCP lo lee",
        )
}

/// Puts a variable back the way it was when the guard drops — panic or not.
///
/// A test that calls `remove_var` after its assertions leaks the variable into
/// the rest of the process exactly when one of those assertions fires, and a
/// leaked `MEMORY_INDUSTRY_DOCTOR_DEEP_SECS=1` turns every later `--deep` check
/// into an instant give-up.
///
/// Take `session::GLOBAL_STATE_GUARD` before constructing one: `set_var` is
/// unsound while another thread may be reading the environment.
#[cfg(test)]
pub struct ScopedEnv {
    name: String,
    previous: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl ScopedEnv {
    pub fn set(name: &str, value: &str) -> Self {
        Self::swap(name, Some(value))
    }

    pub fn cleared(name: &str) -> Self {
        Self::swap(name, None)
    }

    fn swap(name: &str, value: Option<&str>) -> Self {
        let previous = std::env::var_os(name);
        unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        Self {
            name: name.to_string(),
            previous,
        }
    }
}

#[cfg(test)]
impl Drop for ScopedEnv {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var(&self.name, v),
                None => std::env::remove_var(&self.name),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pairs this release promoted to the new namespace. The judge's own
    /// table is deliberately not here: migrating it is a separate change, and
    /// copying it would leave two lists to keep in step.
    const PROMOTED_IN_THIS_RELEASE: [(&str, &str); 5] = [
        ("MEMORY_INDUSTRY_GPU_MEM_LIMIT_MB", "CUBA_GPU_MEM_LIMIT_MB"),
        ("MEMORY_INDUSTRY_RERANK_DEVICE", "CUBA_RERANK_DEVICE"),
        ("MEMORY_INDUSTRY_WARM_RERANKER", "CUBA_WARM_RERANKER"),
        (
            "MEMORY_INDUSTRY_WARM_BEFORE_SERVE_SECS",
            "CUBA_WARM_BEFORE_SERVE_SECS",
        ),
        ("MEMORY_INDUSTRY_DOCTOR_DEEP_SECS", "CUBA_DOCTOR_DEEP_SECS"),
    ];

    /// The modules that resolve those pairs. Named rather than discovered by
    /// walking `src/`, so this file can be excluded by construction: a
    /// structural test in this repository once matched its own source and
    /// asserted nothing at all for three runs.
    const CALL_SITES: [&str; 4] = ["gpu.rs", "http.rs", "doctor.rs", "resources.rs"];

    /// Names nothing else reads, so these tests cannot change what a test
    /// running beside them sees even if the guard above is ever dropped.
    fn invented_pair(tag: &str) -> (String, String) {
        (
            format!("MEMORY_INDUSTRY_ENVS_SELFTEST_{tag}"),
            format!("CUBA_ENVS_SELFTEST_{tag}"),
        )
    }

    fn wired_through_alias(preferred: &str, legacy: &str) -> bool {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        CALL_SITES.iter().any(|file| {
            let path = src.join(file);
            let body = std::fs::read_to_string(path).expect("module is readable");
            body.contains("envs::alias(")
                && body.contains(&format!("\"{preferred}\""))
                && body.contains(&format!("\"{legacy}\""))
        })
    }

    #[tokio::test]
    async fn the_preferred_name_wins_when_both_are_set() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let (preferred, legacy) = invented_pair("BOTH");
        let _p = ScopedEnv::set(&preferred, "new");
        let _l = ScopedEnv::set(&legacy, "old");

        assert_eq!(
            alias(&preferred, &legacy).as_deref(),
            Ok("new"),
            "an operator who moved to the documented namespace and left the old line in the \
             unit file must get the value they edited, not the one they forgot"
        );
    }

    #[tokio::test]
    async fn the_legacy_name_still_answers_when_it_is_the_only_one() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let (preferred, legacy) = invented_pair("LEGACY_ONLY");
        let _p = ScopedEnv::cleared(&preferred);
        let _l = ScopedEnv::set(&legacy, "old");

        assert_eq!(
            alias(&preferred, &legacy).as_deref(),
            Ok("old"),
            "every install in the field is set up with the CUBA_* names. Reading the new one \
             first must not stop the old one from working, or this release breaks every \
             machine it is meant to fix"
        );
    }

    #[tokio::test]
    async fn neither_set_is_the_same_error_as_before() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let (preferred, legacy) = invented_pair("NEITHER");
        let _p = ScopedEnv::cleared(&preferred);
        let _l = ScopedEnv::cleared(&legacy);

        assert_eq!(
            alias(&preferred, &legacy),
            Err(std::env::VarError::NotPresent),
            "every caller reads this as «nothing configured, take the default». Any other \
             error here would be a knob that stopped having a default"
        );
    }

    #[tokio::test]
    async fn every_new_knob_answers_to_both_names() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        for (preferred, legacy) in PROMOTED_IN_THIS_RELEASE {
            {
                let _p = ScopedEnv::cleared(preferred);
                let _l = ScopedEnv::set(legacy, "legacy-answer");
                assert_eq!(
                    alias(preferred, legacy).as_deref(),
                    Ok("legacy-answer"),
                    "{legacy} is what the units in packaging/ and every deployed launcher set"
                );
            }
            {
                let _p = ScopedEnv::set(preferred, "preferred-answer");
                let _l = ScopedEnv::set(legacy, "legacy-answer");
                assert_eq!(
                    alias(preferred, legacy).as_deref(),
                    Ok("preferred-answer"),
                    "{preferred} is the name README.md and .env.example offer"
                );
            }

            assert!(
                wired_through_alias(preferred, legacy),
                "{preferred} resolves here but no module reads it, which is the whole defect \
                 this pair is meant to close: the plan asked for these to move to the shared \
                 helper, they stayed on a bare env::var of {legacy}, and the machine the fix \
                 was written for went on ignoring the documented name"
            );
        }
    }

    #[tokio::test]
    async fn home_comes_from_userprofile_when_windows_has_no_home() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let _h = ScopedEnv::cleared("HOME");
        let _u = ScopedEnv::set("USERPROFILE", "C:\\Users\\operador");

        assert_eq!(
            home().expect("USERPROFILE answers when HOME does not"),
            PathBuf::from("C:\\Users\\operador"),
            "PowerShell and cmd.exe define USERPROFILE and not HOME, which is every \
             Windows operator who did not start from Git Bash"
        );
    }

    #[tokio::test]
    async fn home_prefers_home_when_both_are_set() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let _h = ScopedEnv::set("HOME", "/home/elegido");
        let _u = ScopedEnv::set("USERPROFILE", "C:\\Users\\heredado");

        assert_eq!(
            home().expect("HOME answers"),
            PathBuf::from("/home/elegido"),
            "HOME is the one an operator sets on purpose; USERPROFILE is the one the \
             system sets for them. The other ten sites in this crate order them this \
             way, and a config written under a different root than the cache is a \
             second defect"
        );
    }

    #[tokio::test]
    async fn neither_variable_is_an_error_that_names_both() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let _h = ScopedEnv::cleared("HOME");
        let _u = ScopedEnv::cleared("USERPROFILE");

        match home() {
            Ok(guessed) => panic!(
                "resolved to {} with neither variable set. Guessing succeeds and leaves \
                 the file in the wrong place; only a loud failure sends the operator to \
                 set the variable",
                guessed.display()
            ),
            Err(e) => {
                let said = e.to_string();
                assert!(
                    said.contains("HOME"),
                    "the message must name the variable to set: {said}"
                );
                assert!(
                    said.contains("USERPROFILE"),
                    "naming only HOME sends a Windows operator to define the one variable \
                     their shell does not use: {said}"
                );
            }
        }
    }
}
