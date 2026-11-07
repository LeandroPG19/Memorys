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
/// path.
///
/// The ten sites that used to read the two variables by hand now come through
/// here. Eight of them take `.ok()` rather than `?`, and that is not laziness:
/// their answer to «no home» is a quiet None that a caller reads as «nothing
/// installed / nothing configured», which is a supported machine. Turning any
/// of those into a propagated error would report an unconfigured install as a
/// broken one.
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

/// A temp path this run owns, for a test that points HOME or USERPROFILE at it.
///
/// Nothing is created here, and that is the point: several callers need the
/// path to stay missing, which is how they prove a reversed HOME/USERPROFILE
/// order resolves where nothing is. `handlers::sync`'s `scratch` creates, so
/// it is the wrong helper for them.
///
/// The uuid is not decoration. These tests move HOME process-wide and several
/// end in `remove_dir_all`, so two of them sharing a path would have one
/// deleting the other's fixture mid-run — and the loser would read as a broken
/// home resolution rather than as the collision it is.
#[cfg(test)]
pub fn scratch_root(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("memory-industry-{tag}-{}", uuid::Uuid::new_v4()))
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
             system sets for them. Every site in this crate that needs a home now asks \
             here, so this order is the only one there is — reverse it and a config \
             written under a different root than the cache is a second defect, on all \
             of them at once"
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

#[cfg(test)]
mod trace_tests {
    const AVISO: &str = "aviso de un callsite que solo alcanza un test de envs";

    /// A callsite of its own: only the test below reaches it, so no other test
    /// can register it first.
    fn warn_at_this_callsite() {
        tracing::warn!("{AVISO}");
    }

    /// What `f` logged at WARN or above on the thread that runs it.
    fn warnings_while(f: impl FnOnce()) -> String {
        crate::envs::quiet_global_subscriber();
        #[derive(Clone, Default)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("capture lock")
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        String::from_utf8_lossy(&capture.0.lock().expect("capture lock")).into_owned()
    }

    #[test]
    fn a_warning_another_thread_reached_first_is_still_captured() {
        let logged = warnings_while(|| {
            std::thread::scope(|s| {
                s.spawn(warn_at_this_callsite)
                    .join()
                    .expect("the other thread finishes");
            });
            warn_at_this_callsite();
        });
        assert!(
            logged.contains(AVISO),
            "the warning this callsite raised on the capturing thread is in the capture, \
             even though another thread without a subscriber reached the callsite first; \
             logged: {logged:?}"
        );
    }

    /// The same case in a fresh process where the capture is the only live
    /// dispatcher: there the first registration is the one that caches `never`.
    #[test]
    fn the_same_case_in_a_fresh_process_where_the_capture_is_the_only_dispatcher() {
        let (_crate_name, in_crate) = module_path!()
            .split_once("::")
            .expect("a module path starts with the crate");
        let output = std::process::Command::new(
            std::env::current_exe().expect("the test binary knows where it lives"),
        )
        .arg(format!(
            "{in_crate}::a_warning_another_thread_reached_first_is_still_captured"
        ))
        .args(["--exact", "--test-threads=1"])
        .output()
        .expect("the test binary runs again");
        let printed = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success() && printed.contains("1 passed"),
            "the child run did not pass exactly one test:\n{printed}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// A global subscriber that records nothing and answers `Interest::sometimes`.
///
/// Once it is global, no callsite can cache `never` from a thread without a
/// subscriber: the macro asks the thread's own default on each event. The
/// test-only helpers that capture WARN lines call `quiet_global_subscriber`
/// first, so the interest of their callsites is fixed by this subscriber and
/// not by whichever thread reaches them first.
#[cfg(test)]
struct QuietSubscriber;

#[cfg(test)]
impl tracing::subscriber::Subscriber for QuietSubscriber {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        false
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::WARN)
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, _: &tracing::Event<'_>) {}

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Installs `QuietSubscriber` as the process's global default, once.
#[cfg(test)]
pub(crate) fn quiet_global_subscriber() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = tracing::subscriber::set_global_default(QuietSubscriber);
    });
}
