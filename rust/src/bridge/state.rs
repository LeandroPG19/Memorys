//! The bridge's one file: who the host is, which credential this device holds, where the
//! host was last seen. It holds the secret, so it is written ACL-first and failing closed.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::accounts::Secret;
use crate::service::Target;
use crate::service_apply::CommandRunner;

/// The `version` this build reads and writes.
pub const STATE_VERSION: u32 = 1;

/// `bridge.json`. `Debug` is derived on purpose: `Secret` prints `<redacted>`, so the
/// derive cannot leak it.
#[derive(Debug)]
pub struct BridgeState {
    pub version: u32,
    pub host_id: Uuid,
    pub host_name: String,
    pub account: String,
    pub device_label: String,
    pub credential_id: Uuid,
    pub secret: Secret,
    /// Most recent first, at most 8.
    pub addresses: Vec<SocketAddr>,
    pub last_good: Option<SocketAddr>,
    /// The host's port, for the address a name resolves to (the sweep and `addresses`
    /// carry their own).
    pub host_port: u16,
    /// The bridge's own loopback port.
    pub port: u16,
    pub paired_at: DateTime<Utc>,
}

pub fn state_dir(
    _target: Target,
    _local_app_data: Option<&str>,
    _home: Option<&Path>,
) -> Result<PathBuf> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: state::state_dir")
}

pub fn read_state(_path: &Path) -> Result<Option<BridgeState>> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: state::read_state")
}

pub fn write_state(
    _path: &Path,
    _state: &BridgeState,
    _target: Target,
    _runner: &mut dyn CommandRunner,
) -> Result<()> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: state::write_state")
}

pub fn acl_argv(_file: &Path, _principal: &str) -> Vec<String> {
    // PASS-1 STUB
    unimplemented!("PASS-1 STUB: state::acl_argv")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::fake_host::{Identity, ScriptedRunner, TempDir, dir_entries, same_state};
    use super::*;

    fn state_file(dir: &TempDir) -> PathBuf {
        dir.path().join(crate::bridge::STATE_BASENAME)
    }

    fn sample() -> BridgeState {
        Identity::new().state(
            8787,
            vec!["10.0.5.20:8787".parse().expect("an address")],
            Some("10.0.5.20:8787".parse().expect("an address")),
        )
    }

    /// H1
    #[test]
    fn the_state_lives_under_local_app_data_on_windows() {
        let dir = state_dir(
            Target::Windows,
            Some(r"C:\Users\ana\AppData\Local"),
            Some(Path::new("/home/ana")),
        )
        .expect("LOCALAPPDATA is given");
        assert_eq!(
            dir.to_string_lossy(),
            r"C:\Users\ana\AppData\Local\MemoryIndustry",
            "the state belongs to the user's local profile; roaming (%APPDATA%) would copy the \
             credential to every PC that user logs into"
        );
    }

    /// H1
    #[test]
    fn the_state_does_not_guess_a_directory_without_local_app_data() {
        for missing in [None, Some(""), Some("   ")] {
            let err = state_dir(Target::Windows, missing, Some(Path::new("/home/ana")))
                .expect_err("no LOCALAPPDATA means no place to put a credential");
            assert!(
                format!("{err:#}").contains("LOCALAPPDATA"),
                "the error must name the variable the operator has to fix; falling back to the \
                 working directory would drop a secret next to whatever the shell was in. Got: \
                 {err:#} for {missing:?}"
            );
        }
    }

    /// H1
    #[test]
    fn on_linux_the_state_lives_under_the_home_for_development() {
        let dir = state_dir(
            Target::Linux,
            Some(r"C:\ignored"),
            Some(Path::new("/home/ana")),
        )
        .expect("a home is given");
        assert_eq!(
            dir,
            PathBuf::from("/home/ana/.local/share/MemoryIndustry"),
            "off Windows only the home counts; LOCALAPPDATA means nothing there"
        );
        assert!(
            state_dir(Target::Linux, None, None).is_err(),
            "without a home there is nowhere honest to write"
        );
    }

    #[test]
    fn the_acl_command_has_exactly_the_documented_shape() {
        assert_eq!(
            acl_argv(Path::new(r"C:\Users\ana\bridge.json.tmp"), r"PC-LAB\ana"),
            vec![
                "icacls".to_string(),
                r"C:\Users\ana\bridge.json.tmp".to_string(),
                "/inheritance:r".to_string(),
                "/grant:r".to_string(),
                r"PC-LAB\ana:(F)".to_string(),
            ],
            "/inheritance:r drops the parent's grants and /grant:r leaves exactly one owner; \
             without either, Users and Everyone could keep reading the secret"
        );
    }

    /// H2
    #[test]
    fn the_acl_lands_on_the_empty_temp_file_before_a_byte_of_the_secret_is_written() {
        let dir = TempDir::new("h2");
        let path = state_file(&dir);
        let state = sample();
        let secret_hex = state.secret.to_hex();
        let mut runner = ScriptedRunner::new("PC-LAB\\ana\r\n", 0, 0);

        write_state(&path, &state, Target::Windows, &mut runner)
            .expect("a protectable state is written");

        assert_eq!(
            runner.calls.len(),
            2,
            "exactly whoami then icacls, nothing else touches the system. Got: {:?}",
            runner.calls
        );
        assert_eq!(runner.calls[0], vec!["whoami".to_string()]);
        let icacls = &runner.calls[1];
        assert_eq!(
            icacls.len(),
            5,
            "icacls <temp> /inheritance:r /grant:r <who>:(F): {icacls:?}"
        );
        assert_eq!(
            icacls[2..].to_vec(),
            vec![
                "/inheritance:r".to_string(),
                "/grant:r".to_string(),
                r"PC-LAB\ana:(F)".to_string(),
            ],
            "the principal is whoami's output trimmed, and the grant is full control for that \
             one user"
        );
        assert_eq!(icacls[0], "icacls");
        let temp = PathBuf::from(&icacls[1]);
        assert_ne!(
            temp, path,
            "the ACL goes on the TEMP file: protecting the final path would leave the old file \
             exposed and the new one unprotected until the rename"
        );
        assert_eq!(
            temp.parent(),
            path.parent(),
            "the temp file lives in the same directory so the rename is atomic and inherits \
             nothing from another volume"
        );
        assert_eq!(
            runner.at_icacls,
            Some((true, 0)),
            "when icacls ran the temp file had to EXIST and be EMPTY: a secret written before \
             the ACL exists is readable by every local user for that moment"
        );

        assert!(
            !temp.exists(),
            "the temp file is renamed onto the final path, not copied and left behind"
        );
        assert_eq!(
            dir_entries(dir.path()),
            vec!["bridge.json".to_string()],
            "the directory keeps no .tmp"
        );
        let on_disk = std::fs::read_to_string(&path).expect("the state file exists");
        assert!(
            on_disk.contains(&secret_hex),
            "anchor: the secret is on disk, in the one protected file"
        );
        let back = read_state(&path)
            .expect("the file we wrote reads back")
            .expect("and exists");
        same_state(&state, &back);
    }

    /// H3
    #[test]
    fn a_state_that_cannot_be_protected_is_not_stored() {
        let cases = [
            ("icacls refuses", "PC-LAB\\ana\r\n", 0, 5),
            ("whoami prints nothing", "", 0, 0),
            ("whoami prints a quote", "PC-LAB\\\"ana\r\n", 0, 0),
        ];
        for (name, whoami_out, whoami_code, icacls_code) in cases {
            let dir = TempDir::new("h3");
            let path = state_file(&dir);
            let before = sample();
            write_state(&path, &before, Target::Linux, &mut ScriptedRunner::idle())
                .expect("a development write needs no ACL");
            let bytes_before = std::fs::read(&path).expect("the earlier state exists");

            let after = sample();
            let mut runner = ScriptedRunner::new(whoami_out, whoami_code, icacls_code);
            let err = match write_state(&path, &after, Target::Windows, &mut runner) {
                Ok(()) => panic!("{name}: a secret that cannot be protected must not be saved"),
                Err(err) => err,
            };

            if icacls_code != 0 {
                assert!(
                    format!("{err:#}").to_lowercase().contains("acl"),
                    "{name}: the operator must learn the ACL is why nothing was saved. Got: {err:#}"
                );
            }
            if whoami_out.trim().is_empty() || whoami_out.contains('"') {
                assert!(
                    !runner.calls.iter().any(|c| c[0] == "icacls"),
                    "{name}: with no usable principal icacls must not run at all (an empty or \
                     quoted principal is how a grant goes to the wrong account). Calls: {:?}",
                    runner.calls
                );
            }
            assert_eq!(
                std::fs::read(&path).expect("the earlier state is still there"),
                bytes_before,
                "{name}: the previous bridge.json must survive byte for byte; overwriting before \
                 the rename would lose the paired device"
            );
            assert_eq!(
                dir_entries(dir.path()),
                vec!["bridge.json".to_string()],
                "{name}: no temp file may be left holding the new secret"
            );
            assert!(
                !std::fs::read_to_string(&path)
                    .expect("readable")
                    .contains(&after.secret.to_hex()),
                "{name}: no trace of the rejected state"
            );
        }
    }

    /// H4
    #[cfg(unix)]
    #[test]
    fn on_unix_the_state_file_is_owner_only_from_its_creation() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new("h4");
        let path = state_file(&dir);
        let mut runner = ScriptedRunner::idle();

        write_state(&path, &sample(), Target::Linux, &mut runner).expect("a development write");
        let mode = std::fs::metadata(&path)
            .expect("exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the secret file must not be group or world readable (umask would give 0644)"
        );
        assert!(
            runner.calls.is_empty(),
            "off Windows nothing is delegated to icacls. Calls: {:?}",
            runner.calls
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        write_state(&path, &sample(), Target::Linux, &mut runner).expect("a rewrite");
        let again = std::fs::metadata(&path)
            .expect("exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            again, 0o600,
            "a rewrite replaces the file by a new owner-only one; it must not inherit the old mode"
        );
    }

    /// H5
    #[test]
    fn printing_a_state_never_shows_the_secret() {
        let state = sample();
        let shown = format!("{state:?}");
        assert!(
            shown.contains("ana") && shown.contains("pc-lab-1"),
            "anchor: the debug output does describe the state. Got: {shown}"
        );
        assert!(
            !shown.contains(&state.secret.to_hex()),
            "a `{{:?}}` of the state (a log line, a panic) printed the secret"
        );
    }

    /// H5
    #[test]
    fn reading_a_broken_state_never_echoes_the_secret_and_names_the_file() {
        let dir = TempDir::new("h5");
        let path = state_file(&dir);
        let state = sample();
        let secret_hex = state.secret.to_hex();
        write_state(&path, &state, Target::Linux, &mut ScriptedRunner::idle())
            .expect("a development write");
        let good = std::fs::read_to_string(&path).expect("the state file");
        let document: serde_json::Value = serde_json::from_str(&good).expect("it is JSON");
        assert_eq!(
            document["secret"], secret_hex,
            "anchor: the file stores the secret as lowercase hex under `secret`"
        );

        let cut = good.find(&secret_hex).expect("the secret is in the file") + secret_hex.len() + 2;
        let mut unknown_version = document.clone();
        unknown_version["version"] = serde_json::json!(99);
        let mut missing_field = document.clone();
        missing_field
            .as_object_mut()
            .expect("an object")
            .remove("host_id");
        let broken = [
            ("a truncated file", good[..cut].to_string()),
            ("an unknown version", unknown_version.to_string()),
            ("a missing field", missing_field.to_string()),
        ];
        for (name, text) in broken {
            std::fs::write(&path, &text).expect("write the broken file");
            let err = match read_state(&path) {
                Ok(_) => panic!("{name} must be rejected"),
                Err(err) => err,
            };
            let said = format!("{err:#}");
            assert!(
                !said.contains(&secret_hex),
                "{name}: the error message carried the secret: {said}"
            );
            if name == "an unknown version" {
                assert!(
                    said.contains(&*path.to_string_lossy()),
                    "an unknown version must name the file so the operator knows what to \
                     delete or update. Got: {said}"
                );
            }
            if name == "a missing field" {
                assert!(
                    said.contains("host_id"),
                    "the error must say which field is missing. Got: {said}"
                );
            }
        }
    }

    #[test]
    fn a_missing_state_file_is_not_an_error() {
        let dir = TempDir::new("h5-missing");
        let read = read_state(&state_file(&dir)).expect("a missing file is a clean read");
        assert!(
            read.is_none(),
            "no file means this device is not paired, which `bridge run` reports itself"
        );
    }
}
