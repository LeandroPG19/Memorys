//! 0.29.0 · `setup service --apply` over an install that already exists.
//!
//! The defect: `--apply` resolved the address from the flags alone, so a second
//! `--apply` with no flags put a LAN daemon back on 127.0.0.1 and every client
//! on the subnet lost it. The decisions live in `service.rs` and
//! `service_apply.rs` (the unit tests there pin each one); what only a process
//! can show is the WIRING in `setup_agent.rs`, which `cargo mutants` does not
//! judge: that the installed env file is read BEFORE anything is written, that
//! a refusal leaves the directory as it was, and that `--register` asks for
//! elevation before it writes a byte.
//!
//! Unix only, on purpose and not as a skip: `--register` probes with `fltmc`,
//! which on a Windows host with an elevated console would exist and register a
//! real task. Here `PATH` is an empty directory, so there is no `fltmc`, no
//! `schtasks` and no `netsh` for the child to find. The target is forced to
//! `--windows` so the install root comes from the `LOCALAPPDATA` this test sets.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

const ROUTABLE: &str = "192.168.0.10:8787";
const ENV_NAME: &str = "memory-industry.env";

/// A machine of this test's own: a home, a `LOCALAPPDATA`, and an empty `PATH`
/// directory, all thrown away on the way out.
struct Machine(PathBuf);

impl Machine {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("mi-v029-apply-{}", uuid::Uuid::new_v4()));
        for sub in ["home", "local", "bin"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch directory is creatable");
        }
        Self(dir)
    }

    fn local_app_data(&self) -> PathBuf {
        self.0.join("local")
    }

    fn root(&self) -> PathBuf {
        self.local_app_data().join("MemoryIndustry")
    }

    fn env_path(&self) -> PathBuf {
        self.root().join(ENV_NAME)
    }

    /// An install somebody made earlier, with this env file in it.
    fn already_installed(&self, env_text: &str) {
        std::fs::create_dir_all(self.root()).expect("the install root is creatable");
        std::fs::write(self.env_path(), env_text).expect("the env file is writable");
    }

    fn env_text(&self) -> String {
        std::fs::read_to_string(self.env_path()).expect("the env file is still there")
    }

    /// The names under the install root, sorted; empty when it does not exist.
    fn root_listing(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.root())
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn setup_service(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_memory-industry"))
            .args(["setup", "service"])
            .args(args)
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("LOCALAPPDATA", self.local_app_data())
            .env("PATH", self.0.join("bin"))
            .output()
            .expect("the binary runs")
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn lan_args<'a>() -> [&'a str; 6] {
    [
        "--windows",
        "--apply",
        "--profile",
        "lan",
        "--addr",
        ROUTABLE,
    ]
}

#[test]
fn moving_to_the_lan_rewrites_only_the_address_line_and_keeps_the_token() {
    let token = "t".repeat(64);
    for ending in ["\r\n", "\n"] {
        let machine = Machine::new();
        let before = format!(
            "# mine, do not touch\r\n\
             CUBA_HTTP_TOKEN={token}\r\n\
             CUBA_HTTP_ADDR=127.0.0.1:8787{ending}\
             DATABASE_URL=postgresql://operador@db/brain\n"
        );
        machine.already_installed(&before);

        let output = machine.setup_service(&lan_args());

        assert!(
            output.status.success(),
            "moving a loopback install to the LAN is the supported upgrade:\n{}",
            err(&output)
        );
        assert_eq!(
            machine.env_text(),
            before.replace(
                &format!("CUBA_HTTP_ADDR=127.0.0.1:8787{ending}"),
                &format!("CUBA_HTTP_ADDR={ROUTABLE}{ending}")
            ),
            "`--apply` used to keep the file untouched and leave the daemon on the old \
             address; the new one must reach the file, with the operator's DATABASE_URL, \
             comments and the line ending of that line ({ending:?}) as they were"
        );
        assert!(
            out(&output).contains(&format!("dirección: 127.0.0.1:8787 -> {ROUTABLE}")),
            "the operator is told the address moved:\n{}",
            out(&output)
        );
        assert!(
            !out(&output).contains(&token) && !err(&output).contains(&token),
            "and the token they already gave their clients is not printed"
        );
    }
}

#[test]
fn an_empty_token_is_filled_for_a_lan_install_and_never_printed() {
    let machine = Machine::new();
    machine.already_installed("CUBA_HTTP_TOKEN=\nCUBA_HTTP_ADDR=127.0.0.1:8787\n");

    let output = machine.setup_service(&lan_args());

    assert!(
        output.status.success(),
        "an empty token on a LAN install is filled, not refused:\n{}",
        err(&output)
    );
    let after = machine.env_text();
    let token = after
        .lines()
        .find_map(|line| line.strip_prefix("CUBA_HTTP_TOKEN="))
        .unwrap_or_default();
    assert!(
        token.chars().count() >= 32,
        "the daemon refuses to bind a routable address with a token under 32 characters, so \
         the file has to end up with one that clears it: {after:?}"
    );
    assert!(
        out(&output).contains("token: rellenado"),
        "the operator is told a token was written:\n{}",
        out(&output)
    );
    assert!(
        !out(&output).contains(token) && !err(&output).contains(token),
        "and never shown it: stdout lands in scrollback and CI logs"
    );
}

#[test]
fn a_different_token_over_the_one_in_the_file_changes_nothing() {
    let machine = Machine::new();
    let installed = format!(
        "CUBA_HTTP_TOKEN={}\nCUBA_HTTP_ADDR={ROUTABLE}\n",
        "a".repeat(64)
    );
    machine.already_installed(&installed);

    let other = "b".repeat(64);

    let output = machine.setup_service(&["--windows", "--apply", "--token", other.as_str()]);

    assert!(
        !output.status.success(),
        "a token that is not the one in the file would log every client out; it must be refused"
    );
    assert!(
        err(&output).contains("ya tiene un token"),
        "and the refusal says why:\n{}",
        err(&output)
    );
    assert_eq!(machine.env_text(), installed, "the env file is as it was");
    assert_eq!(
        machine.root_listing(),
        [ENV_NAME],
        "and nothing else was written before the refusal: a launcher written next to a \
         refused install is half of one"
    );
}

#[test]
fn an_address_nobody_can_read_stops_the_install_before_it_writes() {
    let machine = Machine::new();
    let installed = "CUBA_HTTP_ADDR=no-es-una-direccion\n";
    machine.already_installed(installed);

    let output = machine.setup_service(&["--windows", "--apply"]);

    assert!(
        !output.status.success(),
        "a garbled address is not a reason to install the default over it"
    );
    let said = err(&output);
    assert!(
        said.contains("no-es-una-direccion") && said.contains(ENV_NAME),
        "the file and the value are named, or the operator hunts for them:\n{said}"
    );
    assert_eq!(machine.env_text(), installed, "the env file is as it was");
    assert_eq!(
        machine.root_listing(),
        [ENV_NAME],
        "and no launcher was written next to it"
    );
}

#[test]
fn register_without_an_elevated_console_stops_before_a_single_file_is_written() {
    let machine = Machine::new();
    let mut args = lan_args().to_vec();
    args.push("--register");

    let output = machine.setup_service(&args);

    assert!(
        !output.status.success(),
        "there is no `fltmc` on this machine, so nothing proves an administrator: refuse"
    );
    let said = err(&output);
    assert!(
        said.contains("administrador") && said.contains("--register"),
        "the refusal names the right that is missing and the way out:\n{said}"
    );
    assert!(
        !machine.root().exists(),
        "the elevation check comes BEFORE write_all. A refused --register that already wrote \
         the launcher and the env file with its token leaves a half install, and the operator \
         reads «failed» as «nothing happened». Found: {:?}",
        machine.root_listing()
    );
}
