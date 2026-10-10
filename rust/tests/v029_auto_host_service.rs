//! `setup service` for the automatic LAN mode (0.29, slice A6): a LAN install with no
//! `--addr` writes `CUBA_HTTP_ADDR=lan:8787` and keeps it on the next `--apply`, an explicit
//! address stays a fixed IP, and on Windows `--register` opens the UDP announcement beside the
//! TCP port, with a rule of its own that `--uninstall` names too.
//!
//! Nothing here runs `netsh` or `schtasks`: a recorder stands in for the operating system.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use memory_industry::service::{self, FIREWALL_RULE_NAME, Profile, Target, Unit};
use memory_industry::service_apply::{self, CommandOutcome, CommandRunner};

const TOKEN: &str = "an-installed-token-0123456789abcdef0123456789abcdef";

static ENV_GUARD: Mutex<()> = Mutex::new(());

struct Env {
    name: &'static str,
    previous: Option<OsString>,
}

impl Env {
    fn set(name: &'static str, value: &Path) -> Self {
        Self::swap(name, Some(value.as_os_str().to_owned()))
    }

    fn cleared(name: &'static str) -> Self {
        Self::swap(name, None)
    }

    fn swap(name: &'static str, value: Option<OsString>) -> Self {
        let previous = std::env::var_os(name);
        unsafe {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
        Self { name, previous }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }
}

/// What `Unit::from_env(.., Target::Windows, ..)` reads, pinned: `LOCALAPPDATA` only exists
/// on a Windows host, and the operator's variables have no business in a scratch install.
/// Field order matters: the variables are restored before the lock is released.
struct HostEnv {
    _variables: Vec<Env>,
    _lock: MutexGuard<'static, ()>,
    root: PathBuf,
}

impl Drop for HostEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn windows_host() -> HostEnv {
    let lock = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
    let root = std::env::temp_dir().join(format!("v029-auto-host-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    HostEnv {
        _variables: vec![
            Env::set("LOCALAPPDATA", &root),
            Env::cleared("DATABASE_URL"),
            Env::cleared("CUBA_MODE"),
            Env::cleared("ONNX_MODEL_PATH"),
            Env::cleared("ORT_DYLIB_PATH"),
        ],
        _lock: lock,
        root,
    }
}

fn address_of(unit: &Unit) -> &str {
    unit.chosen
        .iter()
        .find(|(key, _)| key == "CUBA_HTTP_ADDR")
        .map(|(_, value)| value.as_str())
        .expect("every install sets CUBA_HTTP_ADDR")
}

fn lan_auto_unit() -> Unit {
    Unit::from_env(Some(Profile::Lan), Target::Windows, None, None, None)
        .expect("a LAN install with no --addr is the automatic mode, not a refusal")
}

fn fixed_lan_unit() -> Unit {
    Unit::from_env(
        Some(Profile::Lan),
        Target::Windows,
        Some("192.168.0.10:8787"),
        None,
        None,
    )
    .expect("an explicit LAN address installs")
}

/// Every command the code under test asked to run, each one succeeding.
#[derive(Default)]
struct Recorder {
    calls: Vec<Vec<String>>,
}

impl CommandRunner for Recorder {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<CommandOutcome> {
        let mut call = vec![program.to_string()];
        call.extend(args.iter().cloned());
        self.calls.push(call);
        Ok(CommandOutcome {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

/// `netsh advfirewall firewall <verb> rule …` calls, in order.
fn netsh<'a>(calls: &'a [Vec<String>], verb: &str) -> Vec<&'a Vec<String>> {
    calls
        .iter()
        .filter(|c| c.first().is_some_and(|p| p == "netsh") && c.get(3).is_some_and(|v| v == verb))
        .collect()
}

fn property<'a>(argv: &'a [String], key: &str) -> Option<&'a str> {
    argv.iter()
        .find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
}

/// The rules `--register` added, as (protocol, name, port, profile).
fn added_rules(calls: &[Vec<String>]) -> Vec<(String, String, String, String)> {
    netsh(calls, "add")
        .into_iter()
        .map(|argv| {
            let get = |key| property(argv, key).unwrap_or("").to_string();
            (
                get("protocol"),
                get("name"),
                get("localport"),
                get("profile"),
            )
        })
        .collect()
}

fn registered(unit: &Unit) -> Recorder {
    let mut recorder = Recorder::default();
    service_apply::register(&mut recorder, true, unit, Path::new("task.xml"))
        .expect("an elevated register with every command succeeding goes through");
    recorder
}

/// The name of the UDP rule `--register` creates for `unit`.
fn udp_rule_name(unit: &Unit) -> String {
    let rules = added_rules(&registered(unit).calls);
    rules
        .iter()
        .find(|(protocol, ..)| protocol.eq_ignore_ascii_case("udp"))
        .map(|(_, name, ..)| name.clone())
        .unwrap_or_else(|| panic!("--register added no UDP rule: {rules:?}"))
}

// --- the address the install writes -----------------------------------------

#[test]
fn a_lan_install_without_an_address_writes_lan_auto_on_8787() {
    let _host = windows_host();

    let unit = lan_auto_unit();

    assert_eq!(unit.profile, Profile::Lan);
    assert_eq!(
        address_of(&unit),
        "lan:8787",
        "the program picks the IP, not the person: no --addr means the automatic mode"
    );
    assert!(
        service::render_env_file(&unit)
            .lines()
            .any(|line| line == "CUBA_HTTP_ADDR=lan:8787"),
        "the env file the daemon reads says lan:8787"
    );
    assert!(
        unit.token
            .as_deref()
            .is_some_and(|t| t.chars().count() >= 32),
        "automatic mode is reachable from other machines, so the install carries a real token"
    );
}

#[test]
fn an_explicit_address_stays_a_fixed_ip() {
    let _host = windows_host();

    let unit = fixed_lan_unit();

    assert_eq!(
        address_of(&unit),
        "192.168.0.10:8787",
        "an operator who typed --addr keeps the fixed IP of today"
    );
}

#[test]
fn a_second_apply_with_no_flags_keeps_lan_auto_and_the_token() {
    let _host = windows_host();
    let installed = format!("CUBA_HTTP_ADDR=lan:8787\nCUBA_HTTP_TOKEN={TOKEN}\n");

    let unit = Unit::from_env(None, Target::Windows, None, None, Some(&installed))
        .expect("an env file the installer itself wrote is one it understands");

    assert_eq!(address_of(&unit), "lan:8787");
    assert_eq!(unit.profile, Profile::Lan);
    assert_eq!(
        unit.token.as_deref(),
        Some(TOKEN),
        "a second --apply keeps the token every client is already configured with"
    );
}

#[test]
fn apply_moves_a_fixed_install_to_lan_auto_and_keeps_the_token() {
    let host = windows_host();
    let installed = format!("CUBA_HTTP_ADDR=192.168.0.10:8787\nCUBA_HTTP_TOKEN={TOKEN}\n");
    let env = host.root.join("memory-industry.env");
    std::fs::write(&env, &installed).unwrap();

    let unit = Unit::from_env(
        Some(Profile::Lan),
        Target::Windows,
        None,
        None,
        Some(&installed),
    )
    .expect("--profile lan with no --addr over a fixed install");
    let report = service_apply::sync_env_file(&env, &unit).expect("the env file is rewritten");

    let after = std::fs::read_to_string(&env).unwrap();
    assert!(
        after.lines().any(|line| line == "CUBA_HTTP_ADDR=lan:8787"),
        "the address line becomes lan:8787: {after}"
    );
    assert!(
        after
            .lines()
            .any(|line| line == format!("CUBA_HTTP_TOKEN={TOKEN}")),
        "the token survives the move: {after}"
    );
    assert!(
        report.iter().any(|line| line.contains("lan:8787")),
        "the report says where the address went: {report:?}"
    );
}

// --- the firewall -------------------------------------------------------------

#[test]
fn register_opens_the_udp_announcement_beside_the_tcp_port() {
    let _host = windows_host();

    for (what, unit) in [("automatic", lan_auto_unit()), ("fixed", fixed_lan_unit())] {
        let calls = registered(&unit).calls;
        let rules = added_rules(&calls);

        assert!(
            rules.contains(&(
                "TCP".to_string(),
                FIREWALL_RULE_NAME.to_string(),
                "8787".to_string(),
                "private,domain".to_string()
            )),
            "{what}: the TCP rule of today is still there: {rules:?}"
        );
        let udp: Vec<_> = rules
            .iter()
            .filter(|(protocol, ..)| protocol.eq_ignore_ascii_case("udp"))
            .collect();
        assert_eq!(udp.len(), 1, "{what}: exactly one UDP rule: {rules:?}");
        let (_, name, port, profile) = udp[0];
        assert_eq!(
            port, "8787",
            "{what}: the announcement listens on the same number"
        );
        assert_eq!(
            profile, "private,domain",
            "{what}: never on a public network, like the TCP rule"
        );
        assert!(
            !name.is_empty() && name != FIREWALL_RULE_NAME,
            "{what}: the UDP rule has a name of its own, or deleting one deletes both: {name:?}"
        );

        let udp_delete = calls.iter().position(|c| {
            c.get(3).is_some_and(|v| v == "delete") && property(c, "name") == Some(name.as_str())
        });
        let udp_add = calls.iter().position(|c| {
            c.get(3).is_some_and(|v| v == "add") && property(c, "name") == Some(name.as_str())
        });
        assert!(
            matches!((udp_delete, udp_add), (Some(d), Some(a)) if d < a),
            "{what}: delete then add, so a second run leaves one UDP rule and not two: {calls:?}"
        );
        assert_eq!(
            calls.last().and_then(|c| c.first()).map(String::as_str),
            Some("schtasks"),
            "{what}: the task is registered after the firewall, as today: {calls:?}"
        );
    }
}

#[test]
fn a_loopback_install_opens_no_rule_at_all() {
    let _host = windows_host();
    let unit = Unit::from_env(Some(Profile::Loopback), Target::Windows, None, None, None).unwrap();

    let calls = registered(&unit).calls;

    assert!(
        netsh(&calls, "add").is_empty() && netsh(&calls, "delete").is_empty(),
        "nothing outside this machine reaches loopback, so no port opens: {calls:?}"
    );
}

#[test]
fn uninstall_names_both_rules() {
    let _host = windows_host();
    let udp = udp_rule_name(&lan_auto_unit());

    let hints = service::uninstall_hints(Target::Windows);

    let deletes = |name: &str| {
        hints.iter().any(|hint| {
            hint.contains("delete") && hint.split_whitespace().any(|w| w == format!("name={name}"))
        })
    };
    assert!(deletes(FIREWALL_RULE_NAME), "the TCP rule: {hints:?}");
    assert!(
        deletes(&udp),
        "--uninstall leaves the UDP rule open unless it names it too: {hints:?}"
    );
}

#[test]
fn the_manual_steps_include_the_udp_rule() {
    let host = windows_host();
    let unit = lan_auto_unit();
    let mut recorder = Recorder::default();

    let steps = service_apply::next_steps(&mut recorder, false, &unit, Target::Windows, &host.root)
        .expect("without --register the steps are printed, not run");

    assert!(recorder.calls.is_empty(), "nothing runs without --register");
    assert!(
        steps
            .iter()
            .any(|line| line.contains("protocol=UDP") && line.contains("localport=8787")),
        "the operator who runs the firewall by hand is told about the UDP rule too: {steps:?}"
    );
}
