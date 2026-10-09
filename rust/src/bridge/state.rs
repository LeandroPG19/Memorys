//! The bridge's one file: who the host is, which credential this device holds, where the
//! host was last seen. It holds the secret, so it is written ACL-first and failing closed.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{DEFAULT_BRIDGE_PORT, DEFAULT_HOST_PORT, STATE_BASENAME};
use crate::accounts::Secret;
use crate::service::Target;
use crate::service_apply::CommandRunner;

/// The `version` this build reads and writes.
pub const STATE_VERSION: u32 = 1;

/// How many remembered addresses the file keeps, most recent first.
const MAX_ADDRESSES: usize = 8;

const STATE_DIR_NAME: &str = "MemoryIndustry";

/// `bridge.json`. `Debug` is derived on purpose: `Secret` prints `<redacted>`, so the
/// derive cannot leak it.
#[derive(Debug, Serialize, Deserialize)]
pub struct BridgeState {
    pub version: u32,
    pub host_id: Uuid,
    pub host_name: String,
    pub account: String,
    pub device_label: String,
    pub credential_id: Uuid,
    #[serde(with = "secret_hex")]
    pub secret: Secret,
    /// Most recent first, at most 8.
    #[serde(default)]
    pub addresses: Vec<SocketAddr>,
    #[serde(default)]
    pub last_good: Option<SocketAddr>,
    /// The host's port, for the address a name resolves to (the sweep and `addresses`
    /// carry their own).
    #[serde(default = "default_host_port")]
    pub host_port: u16,
    /// The bridge's own loopback port.
    #[serde(default = "default_bridge_port")]
    pub port: u16,
    pub paired_at: DateTime<Utc>,
}

fn default_host_port() -> u16 {
    DEFAULT_HOST_PORT
}

fn default_bridge_port() -> u16 {
    DEFAULT_BRIDGE_PORT
}

/// The secret travels as 64 lowercase hex digits. A malformed one is reported without
/// echoing what was read: that text may be most of a real secret.
mod secret_hex {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::accounts::Secret;

    pub fn serialize<S: Serializer>(secret: &Secret, out: S) -> Result<S::Ok, S::Error> {
        out.serialize_str(&secret.to_hex())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(input: D) -> Result<Secret, D::Error> {
        let text = String::deserialize(input)?;
        Secret::from_hex(&text)
            .ok_or_else(|| D::Error::custom("the secret is not 64 lowercase hex digits"))
    }
}

impl BridgeState {
    /// The state after `addr` answered as the host: it becomes the last good address and
    /// the head of the list. `None` when nothing would change.
    pub fn healed(&self, addr: SocketAddr) -> Option<BridgeState> {
        if self.last_good == Some(addr) && self.addresses.first() == Some(&addr) {
            return None;
        }
        let mut addresses = vec![addr];
        addresses.extend(self.addresses.iter().filter(|known| **known != addr));
        addresses.truncate(MAX_ADDRESSES);
        Some(BridgeState {
            version: self.version,
            host_id: self.host_id,
            host_name: self.host_name.clone(),
            account: self.account.clone(),
            device_label: self.device_label.clone(),
            credential_id: self.credential_id,
            secret: Secret::from_hex(&self.secret.to_hex())?,
            addresses,
            last_good: Some(addr),
            host_port: self.host_port,
            port: self.port,
            paired_at: self.paired_at,
        })
    }
}

/// `%LOCALAPPDATA%\MemoryIndustry` on Windows. Off Windows (development only)
/// `<home>/.local/share/MemoryIndustry`. A missing base is an error: guessing would put a
/// secret wherever the shell happened to be.
pub fn state_dir(
    target: Target,
    local_app_data: Option<&str>,
    home: Option<&Path>,
) -> Result<PathBuf> {
    if target.is_windows() {
        let base = local_app_data.map(str::trim).filter(|b| !b.is_empty());
        let base = base.context(
            "LOCALAPPDATA is not set: the bridge will not guess where to keep its credential",
        )?;
        // Built by hand: the path is Windows' even when this runs elsewhere (tests).
        return Ok(PathBuf::from(format!(
            "{}\\{STATE_DIR_NAME}",
            base.trim_end_matches('\\')
        )));
    }
    let home =
        home.context("no home directory: the bridge will not guess where to keep its credential")?;
    Ok(home.join(".local").join("share").join(STATE_DIR_NAME))
}

/// `Ok(None)` when the file does not exist (this device is not paired).
pub fn read_state(path: &Path) -> Result<Option<BridgeState>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", path.display())),
    };
    parse_state(&text, path).map(Some)
}

/// The version is read before the shape, so a file from a newer build is reported as
/// that and not as a pile of missing fields.
fn parse_state(text: &str, path: &Path) -> Result<BridgeState> {
    let document: serde_json::Value = serde_json::from_str(text)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    let version = document.get("version").and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(STATE_VERSION)) {
        let found = version.map_or_else(|| "none".to_string(), |v| v.to_string());
        bail!(
            "{} has bridge state version {found}; this build reads version {STATE_VERSION}",
            path.display()
        );
    }
    serde_json::from_value(document)
        .with_context(|| format!("{} has a missing or malformed field", path.display()))
}

/// Writes `state` next to `path` and renames it into place. On Windows the ACL is put on
/// the EMPTY temp file before a byte of the secret exists; if it cannot be, nothing is
/// stored and the previous file is untouched. On unix the file is born `0600`.
pub fn write_state(
    path: &Path,
    state: &BridgeState,
    target: Target,
    runner: &mut dyn CommandRunner,
) -> Result<()> {
    let document = serde_json::to_vec_pretty(state).context("cannot encode the bridge state")?;
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let temp = dir.join(format!(".{STATE_BASENAME}.{}.tmp", Uuid::new_v4().simple()));
    let written = write_through(&temp, path, &document, target, runner);
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

fn write_through(
    temp: &Path,
    path: &Path,
    document: &[u8],
    target: Target,
    runner: &mut dyn CommandRunner,
) -> Result<()> {
    let mut file =
        create_private(temp).with_context(|| format!("cannot create {}", temp.display()))?;
    if target.is_windows() {
        protect(temp, runner)?;
    }
    file.write_all(document)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("cannot write {}", temp.display()))?;
    drop(file);
    std::fs::rename(temp, path).with_context(|| format!("cannot replace {}", path.display()))
}

/// New, empty, owner-only from the first instant on unix (the mode is part of the
/// `open`, not a `chmod` after it).
fn create_private(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn protect(temp: &Path, runner: &mut dyn CommandRunner) -> Result<()> {
    let principal = current_principal(runner)?;
    let argv = acl_argv(temp, &principal);
    let outcome = runner
        .run(&argv[0], &argv[1..])
        .context("the ACL could not be applied: icacls did not run")?;
    if outcome.code != Some(0) {
        bail!(
            "the ACL could not be applied to the state file (icacls exited with {:?}): \
             nothing was saved",
            outcome.code
        );
    }
    Ok(())
}

/// `whoami`'s answer, if it is something a grant can be addressed to.
fn current_principal(runner: &mut dyn CommandRunner) -> Result<String> {
    let outcome = runner
        .run("whoami", &[])
        .context("the ACL could not be applied: whoami did not run")?;
    let who = outcome.stdout.trim();
    if outcome.code != Some(0) || who.is_empty() || who.contains(['"', ':', '\n', '\r']) {
        bail!("the ACL could not be applied: whoami did not name a usable account");
    }
    Ok(who.to_string())
}

/// Grants the one user full control and drops everything inherited.
pub fn acl_argv(file: &Path, principal: &str) -> Vec<String> {
    vec![
        "icacls".to_string(),
        file.to_string_lossy().into_owned(),
        "/inheritance:r".to_string(),
        "/grant:r".to_string(),
        format!("{principal}:(F)"),
    ]
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

    /// Only a missing file means "not paired": any other read failure must stay an error, or
    /// a broken state file would look like a device that never paired.
    #[test]
    fn a_state_path_that_cannot_be_read_is_an_error_and_not_an_unpaired_device() {
        let dir = TempDir::new("unreadable");
        let err = match read_state(dir.path()) {
            Ok(found) => panic!(
                "a directory read as a state (present: {}); only a missing file means not paired",
                found.is_some()
            ),
            Err(err) => err,
        };
        assert!(
            format!("{err:#}").contains("cannot read"),
            "the operator must learn the file could not be read. Got: {err:#}"
        );
    }

    /// The port fields are optional in a file written before they existed: the defaults are
    /// the bridge's real ports, not zero.
    #[test]
    fn a_state_file_without_the_port_fields_reads_with_the_default_ports() {
        let dir = TempDir::new("default-ports");
        let path = state_file(&dir);
        write_state(&path, &sample(), Target::Linux, &mut ScriptedRunner::idle())
            .expect("a development write");
        let mut document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("the state file"))
                .expect("it is JSON");
        let fields = document.as_object_mut().expect("an object");
        fields.remove("host_port");
        fields.remove("port");
        std::fs::write(&path, document.to_string()).expect("the file is written back");

        let state = read_state(&path)
            .expect("a file without the port fields still reads")
            .expect("and exists");

        assert_eq!(
            state.host_port, DEFAULT_HOST_PORT,
            "the host's port defaults to the one the host listens on"
        );
        assert_eq!(
            state.port, DEFAULT_BRIDGE_PORT,
            "the bridge's loopback port defaults to the one the client config names"
        );
    }

    const ADDR_A: &str = "10.0.5.20:8787";
    const ADDR_B: &str = "10.0.5.21:8787";
    const ADDR_C: &str = "10.0.5.22:8787";

    fn sa(text: &str) -> SocketAddr {
        text.parse().expect("an address")
    }

    fn with_addresses(addresses: &[&str], last_good: Option<&str>) -> BridgeState {
        let known: Vec<SocketAddr> = addresses
            .iter()
            .map(|a| a.parse::<SocketAddr>().expect("an address"))
            .collect();
        let last: Option<SocketAddr> = last_good.map(sa);
        Identity::new().state(8787, known, last)
    }

    #[test]
    fn a_host_at_the_head_of_the_list_and_last_good_needs_no_rewrite() {
        let state = with_addresses(&[ADDR_A, ADDR_B], Some(ADDR_A));
        assert!(
            state.healed(sa(ADDR_A)).is_none(),
            "nothing changed, so nothing is written"
        );
    }

    #[test]
    fn a_host_that_answers_where_it_is_last_good_but_not_first_takes_the_head() {
        let state = with_addresses(&[ADDR_A, ADDR_B], Some(ADDR_B));
        let healed = state
            .healed(sa(ADDR_B))
            .expect("the head of the list is stale, so the state changes");
        assert_eq!(healed.addresses, vec![sa(ADDR_B), sa(ADDR_A)]);
        assert_eq!(healed.last_good, Some(sa(ADDR_B)));
    }

    #[test]
    fn a_host_that_answers_at_the_head_but_not_as_last_good_becomes_last_good() {
        let state = with_addresses(&[ADDR_A, ADDR_B], Some(ADDR_B));
        let healed = state
            .healed(sa(ADDR_A))
            .expect("the last good address moves, so the state changes");
        assert_eq!(healed.addresses, vec![sa(ADDR_A), sa(ADDR_B)]);
        assert_eq!(healed.last_good, Some(sa(ADDR_A)));
    }

    #[test]
    fn a_host_that_is_last_good_but_not_first_moves_to_the_front_once() {
        let state = with_addresses(&[ADDR_B, ADDR_A], Some(ADDR_A));
        let healed = state
            .healed(sa(ADDR_A))
            .expect("the head of the list is stale, so the state changes");
        assert_eq!(
            healed.addresses,
            vec![sa(ADDR_A), sa(ADDR_B)],
            "the address appears once, at the front, and the others keep their order"
        );
    }

    #[test]
    fn a_new_address_goes_first_and_the_known_ones_keep_their_order() {
        let state = with_addresses(&[ADDR_A, ADDR_B], Some(ADDR_A));
        let healed = state
            .healed(sa(ADDR_C))
            .expect("a new address changes the state");
        assert_eq!(
            healed.addresses,
            vec![sa(ADDR_C), sa(ADDR_A), sa(ADDR_B)],
            "a new answer is remembered and the addresses already known are kept, not dropped"
        );
        assert_eq!(healed.last_good, Some(sa(ADDR_C)));
    }
}
