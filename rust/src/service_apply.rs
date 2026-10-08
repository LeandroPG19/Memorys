//! What `setup service --apply` does to a machine that already has an install:
//! it keeps the address and the token the operator set, and with `--register`
//! it hands the OS the scheduled task and the firewall rule.
//!
//! Everything that DECIDES lives here, behind `CommandRunner`, because
//! `setup_agent.rs` is outside the mutation run (`quality-gate.sh:164`): there
//! the real runner spawns the process and prints, and nothing else.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

use crate::service::{
    FIREWALL_RULE_NAME, TASK_NAME, TASK_XML_BASENAME, Target, Unit, firewall_add_argv,
    firewall_commands, firewall_delete_argv, manual_steps, schtasks_create_argv,
};

const ADDR_KEY: &str = "CUBA_HTTP_ADDR";
const TOKEN_KEY: &str = "CUBA_HTTP_TOKEN";

const NOT_ELEVATED: &str = "--register necesita una consola de administrador (elevada): crear una \
    tarea que corre como SYSTEM y abrir un puerto en el firewall son derechos de administrador. \
    Abrí PowerShell «como administrador» y repetí, o quitá --register y ejecutá a mano los \
    comandos que --apply imprime";

/// How a command ended, as far as a decision needs to know. `code` is `None`
/// when the process was killed by a signal and has no exit code.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The one door to the operating system. Injected so that no test spawns
/// `schtasks` or `netsh`, and so that the order of the calls is something a
/// test can read back.
pub trait CommandRunner {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<CommandOutcome>;
}

/// The value of `key` on one line, if the line sets it. A `#` line sets
/// nothing, and the key must be the whole name: `CUBA_HTTP_ADDR_OTHER` is
/// another variable.
fn line_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let line = line.trim_start();
    if line.starts_with('#') {
        return None;
    }
    let (name, value) = line.split_once('=')?;
    (name.trim() == key).then_some(value.trim())
}

/// The last effective `KEY=value` line's value, trimmed. A `#` line is not
/// effective, and CRLF is tolerated. The last wins because that is what the
/// launcher's `for /f` and systemd's `EnvironmentFile` do with a repeated key.
pub fn env_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .filter_map(|line| line_value(line, key))
        .next_back()
        .map(str::to_string)
}

/// Replaces ONLY the last effective line of `key`, keeping its own line ending;
/// appends `key=value` when there is none; every other byte is untouched.
pub fn set_env_value(text: &str, key: &str, value: &str) -> String {
    let mut offset = 0;
    let mut last = None;
    for line in text.split_inclusive('\n') {
        if line_value(line, key).is_some() {
            last = Some((offset, line));
        }
        offset += line.len();
    }
    let Some((at, line)) = last else {
        let separator = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return format!("{text}{separator}{key}={value}\n");
    };
    let ending = &line[line.trim_end_matches(['\r', '\n']).len()..];
    format!(
        "{}{key}={value}{ending}{}",
        &text[..at],
        &text[at + line.len()..]
    )
}

/// The address line, brought to what the unit resolved, and the report line
/// that says so when it moved.
fn sync_address(text: &str, unit: &Unit) -> (String, Option<String>) {
    let want = unit.addr.to_string();
    let have = env_value(text, ADDR_KEY).filter(|value| !value.is_empty());
    if have.as_deref() == Some(want.as_str()) {
        return (text.to_string(), None);
    }
    let was = have.as_deref().unwrap_or("(sin definir)");
    let line = format!("dirección: {was} -> {want}");
    (set_env_value(text, ADDR_KEY, &want), Some(line))
}

/// The token line: kept when there is one, filled when it is empty and the unit
/// has one, left alone otherwise. The report says which, and never the value.
fn sync_token(text: &str, unit: &Unit) -> (String, Option<String>) {
    let have = env_value(text, TOKEN_KEY).filter(|value| !value.is_empty());
    match (have, unit.token.as_deref()) {
        (Some(_), _) => (text.to_string(), Some("token: conservado".to_string())),
        (None, Some(token)) => (
            set_env_value(text, TOKEN_KEY, token),
            Some("token: rellenado".to_string()),
        ),
        (None, None) => (text.to_string(), None),
    }
}

/// Brings an existing env file in line with what `unit` resolved: the address
/// always, the token only when the file's is empty. Atomic (temp file in the
/// same directory, then rename), 0600 on unix, and no rewrite when nothing
/// changes. Returns the lines `--apply` reports.
pub fn sync_env_file(path: &Path, unit: &Unit) -> Result<Vec<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("no se pudo leer {}", path.display())),
    };
    let (with_address, address_line) = sync_address(&text, unit);
    let (synced, token_line) = sync_token(&with_address, unit);
    if synced != text {
        replace_atomically(path, &synced)?;
    }
    Ok(address_line.into_iter().chain(token_line).collect())
}

/// A file only this account can read, created or refused: `create_new` so a
/// planted file or link at the temp name is an error and not somebody else's
/// file written through.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    options.open(path)
}

/// Where the temp file of a rewrite of `path` goes: next to it, because a rename
/// across filesystems is a copy and no longer atomic. A bare file name has an
/// empty parent, which is the current directory. Its own function because once
/// the rename is done the temp's location leaves no trace to test.
fn temp_beside(path: &Path) -> std::path::PathBuf {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    directory.join(format!(".memory-industry.env.{}.tmp", uuid::Uuid::new_v4()))
}

/// Temp file in the same directory, then rename: a crash leaves the old file or
/// the new one, never half of either. The token is in this file, so the temp is
/// private from the first byte and removed if anything fails.
fn replace_atomically(path: &Path, contents: &str) -> Result<()> {
    let temp = temp_beside(path);
    let done = create_private(&temp)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&temp, path));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("no se pudo actualizar {}", path.display()));
    }
    Ok(())
}

/// Whether this console may create a scheduled task for SYSTEM and open a
/// firewall port: `fltmc` succeeds only for an administrator. Never panics.
pub fn probe_elevated(runner: &mut dyn CommandRunner) -> bool {
    runner
        .run("fltmc", &[])
        .is_ok_and(|outcome| outcome.code == Some(0))
}

/// The refusal `--apply --register` gives BEFORE it writes a file: a half
/// install (the launcher and the env file with its token, and no task) reads as
/// «failed» to the operator and as «installed» to the next `--apply`.
pub fn require_elevated(runner: &mut dyn CommandRunner) -> Result<()> {
    if probe_elevated(runner) {
        Ok(())
    } else {
        Err(anyhow!(NOT_ELEVATED))
    }
}

/// One command, judged by its exit code. The reason carries the first line of
/// what the command said, which for `netsh` is what names the cause.
fn run_checked(runner: &mut dyn CommandRunner, argv: &[String]) -> std::result::Result<(), String> {
    let [program, args @ ..] = argv else {
        return Err("comando vacío".to_string());
    };
    match runner.run(program, args) {
        Ok(done) if done.code == Some(0) => Ok(()),
        Ok(done) => Err(format!("{program} terminó {}", reason(&done))),
        Err(e) => Err(format!("no se pudo ejecutar {program}: {e}")),
    }
}

fn reason(done: &CommandOutcome) -> String {
    let said = [&done.stderr, &done.stdout]
        .into_iter()
        .find_map(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("sin mensaje");
    match done.code {
        Some(code) => format!("con código {code}: {said}"),
        None => format!("sin código de salida: {said}"),
    }
}

/// Delete, then add: a second run and a changed port leave ONE rule.
fn open_port(runner: &mut dyn CommandRunner, port: u16) -> Result<String> {
    // Any outcome is fine: with no rule to remove `netsh` exits 1, which is
    // every first run. If `netsh` cannot run at all, the add says so.
    let _ = run_checked(runner, &firewall_delete_argv());
    run_checked(runner, &firewall_add_argv(port)).map_err(|why| {
        anyhow!(
            "no se pudo crear la regla de firewall del puerto {port} ({why}). La regla anterior \
             ya se borró y la tarea NO se registró; repetir --register es seguro"
        )
    })?;
    Ok(format!(
        "firewall: regla {FIREWALL_RULE_NAME} abierta en TCP {port} (redes privada y de dominio)"
    ))
}

fn create_task(runner: &mut dyn CommandRunner, xml: &Path, port_opened: bool) -> Result<String> {
    run_checked(runner, &schtasks_create_argv(xml)).map_err(|why| {
        let firewall = if port_opened {
            "la regla de firewall quedó puesta"
        } else {
            "el firewall no se tocó"
        };
        anyhow!("no se pudo registrar la tarea ({why}). {firewall}; repetir --register es seguro")
    })?;
    Ok(format!(
        "tarea: {TASK_NAME} registrada (arranca con el equipo, como SYSTEM)"
    ))
}

/// What `--apply` ends with: the registration when it was asked for, and
/// otherwise the commands the operator runs by hand.
pub fn next_steps(
    runner: &mut dyn CommandRunner,
    registering: bool,
    unit: &Unit,
    target: Target,
    root: &Path,
) -> Result<Vec<String>> {
    if registering {
        register(runner, true, unit, &root.join(TASK_XML_BASENAME))
    } else {
        Ok(manual_steps(&unit.addr, target, root))
    }
}

/// `--register`: the firewall rule (only for a routable address) and then the
/// task, in that order, each safe to repeat. There is no rollback: when a step
/// fails the error says what is already done.
pub fn register(
    runner: &mut dyn CommandRunner,
    elevated: bool,
    unit: &Unit,
    xml: &Path,
) -> Result<Vec<String>> {
    if !elevated {
        return Err(anyhow!(NOT_ELEVATED));
    }
    let routable = !firewall_commands(&unit.addr).is_empty();
    let firewall = if routable {
        open_port(runner, unit.addr.port())?
    } else {
        format!(
            "firewall: no se abrió ningún puerto porque {} es loopback y ninguna otra máquina lo alcanza",
            unit.addr
        )
    };
    let task = create_task(runner, xml, routable)?;
    Ok(vec![firewall, task])
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::rc::Rc;

    use super::*;
    use crate::service::{
        ENV_BASENAME, Profile, firewall_add_argv, firewall_delete_argv, schtasks_create_argv,
    };

    const ROUTABLE: &str = "192.168.0.10:8787";
    const LOOPBACK: &str = "127.0.0.1:8787";

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("mi-service-apply-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("scratch directory is creatable");
            Self(dir)
        }

        fn env_path(&self) -> PathBuf {
            self.0.join(ENV_BASENAME)
        }

        fn entries(&self) -> usize {
            std::fs::read_dir(&self.0)
                .expect("scratch is readable")
                .count()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A unit on `addr`, field by field: what `sync_env_file` and `register`
    /// read is the address and the token, and nothing else may matter.
    fn unit_at(addr: &str, token: Option<&str>) -> Unit {
        let addr: SocketAddr = addr.parse().expect("the tests spell addresses correctly");
        Unit {
            profile: if addr.ip().is_loopback() {
                Profile::Loopback
            } else {
                Profile::Lan
            },
            exe: PathBuf::from("/opt/mi/memory-industry"),
            addr,
            token: token.map(str::to_string),
            env_file: PathBuf::from("/opt/mi/memory-industry.env"),
            computed: Vec::new(),
            chosen: Vec::new(),
            why: String::new(),
            home_dir: PathBuf::from("/home/operador"),
        }
    }

    fn xml() -> PathBuf {
        PathBuf::from("/opt/mi/memory-industry-task.xml")
    }

    fn says(report: &[String], needle: &str) -> bool {
        report.iter().any(|line| line.contains(needle))
    }

    /// An env file as an operator leaves it: their comments, their
    /// `DATABASE_URL`, mixed line endings, and a commented knob at the end.
    fn operators_file(addr: &str, addr_ending: &str, token: &str) -> String {
        format!(
            "# MemoryIndustry — mine, do not touch\r\n\n\
             CUBA_HTTP_TOKEN={token}\r\n\
             CUBA_HTTP_ADDR={addr}{addr_ending}\
             DATABASE_URL=postgresql://operador@db/brain\n\
             CUBA_MODE=completo\r\n\
             # CUBA_RERANK_DEVICE=\n"
        )
    }

    fn write_env(scratch: &Scratch, text: &str) -> PathBuf {
        let path = scratch.env_path();
        std::fs::write(&path, text).expect("scratch is writable");
        path
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).expect("the env file is readable")
    }

    // --- env_value / set_env_value: the line surgery ---------------------------

    #[test]
    fn env_value_reads_the_last_effective_line_and_trims_it() {
        let text = "# CUBA_HTTP_ADDR=1.1.1.1:1\n\
                    CUBA_HTTP_ADDR=2.2.2.2:2\r\n\
                    CUBA_HTTP_ADDR_OTHER=3.3.3.3:3\n\
                    CUBA_HTTP_ADDR=  192.168.0.10:8787 \r\n";

        assert_eq!(
            env_value(text, "CUBA_HTTP_ADDR").as_deref(),
            Some("192.168.0.10:8787"),
            "the daemon's loader lets a later line win, a `#` line is not a line, a key that \
             merely starts with this one is another key, and CRLF and padding are not part of \
             the value"
        );
        assert_eq!(
            env_value(text, "CUBA_HTTP_TOKEN"),
            None,
            "a key that is not there is None, not an empty string the caller reads as «empty»"
        );
    }

    #[test]
    fn set_env_value_changes_only_the_last_effective_line_and_keeps_its_line_ending() {
        let text = "# CUBA_HTTP_ADDR=commented\r\n\
                    CUBA_HTTP_ADDR=first:1\n\
                    DATABASE_URL=postgresql://operador@db/brain\r\n\
                    CUBA_HTTP_ADDR=127.0.0.1:8787\r\n\
                    # tail";

        assert_eq!(
            set_env_value(text, "CUBA_HTTP_ADDR", ROUTABLE),
            text.replace(
                "CUBA_HTTP_ADDR=127.0.0.1:8787\r\n",
                "CUBA_HTTP_ADDR=192.168.0.10:8787\r\n"
            ),
            "one line changes, and it keeps the \\r\\n it had. Replacing every line that holds \
             the key, normalising the endings or re-rendering the file takes the operator's \
             DATABASE_URL and comments with it"
        );

        let lf_only = "CUBA_HTTP_ADDR=127.0.0.1:8787\nZ=1";
        assert_eq!(
            set_env_value(lf_only, "CUBA_HTTP_ADDR", ROUTABLE),
            "CUBA_HTTP_ADDR=192.168.0.10:8787\nZ=1",
            "a file written on Linux stays LF: the ending belongs to the line, not to the tool"
        );
    }

    #[test]
    fn set_env_value_appends_the_key_when_the_file_has_no_line_for_it() {
        for text in ["A=1\nB=2\n", "A=1\nB=2", ""] {
            let after = set_env_value(text, "CUBA_HTTP_ADDR", LOOPBACK);

            assert!(
                after.starts_with(text),
                "everything that was there stays, byte for byte, in front: {after:?}"
            );
            assert_eq!(
                after.lines().last(),
                Some("CUBA_HTTP_ADDR=127.0.0.1:8787"),
                "and the key ends the file, as a line of its own: {after:?}"
            );
        }
    }

    // --- sync_env_file: --apply does not undo what the operator set ----------------

    #[test]
    fn sync_changes_only_the_address_line_keeps_its_ending_and_says_so() {
        let token = "t".repeat(64);
        for ending in ["\r\n", "\n"] {
            let scratch = Scratch::new();
            let text = operators_file("127.0.0.1:8787", ending, &token);
            let path = write_env(&scratch, &text);

            let report = sync_env_file(&path, &unit_at(ROUTABLE, Some(&token)))
                .expect("an env file the operator edited is syncable");

            assert_eq!(
                read(&path),
                text.replace(
                    &format!("CUBA_HTTP_ADDR=127.0.0.1:8787{ending}"),
                    &format!("CUBA_HTTP_ADDR=192.168.0.10:8787{ending}")
                ),
                "byte for byte what the operator had, except the address line, which keeps its \
                 own {ending:?}. Re-rendering the file from the unit would also take their \
                 DATABASE_URL and comments"
            );
            assert!(
                says(&report, "dirección: 127.0.0.1:8787 -> 192.168.0.10:8787"),
                "the operator reads the report to learn that the address moved: {report:?}"
            );
            assert!(
                says(&report, "token: conservado"),
                "and that the token they already gave their clients is the same one: {report:?}"
            );
            assert_eq!(
                scratch.entries(),
                1,
                "the atomic write goes through a temp file and renames it; one left behind is \
                 a second copy of the token on disk"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn sync_keeps_the_env_file_unreadable_to_other_users() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new();
        let token = "t".repeat(64);
        let path = write_env(&scratch, &operators_file("127.0.0.1:8787", "\n", &token));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("scratch is chmod-able");

        sync_env_file(&path, &unit_at(ROUTABLE, Some(&token))).expect("syncable");

        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the file carries the bearer token for the whole graph: a rewrite that comes out \
             0o{mode:o} lets every other account on the box read it"
        );
    }

    #[test]
    fn a_file_that_already_agrees_is_not_rewritten() {
        let scratch = Scratch::new();
        let token = "t".repeat(64);
        let text = operators_file(ROUTABLE, "\r\n", &token);
        let path = write_env(&scratch, &text);
        let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("reopen")
            .set_modified(long_ago)
            .expect("mtime is settable");

        let report = sync_env_file(&path, &unit_at(ROUTABLE, Some(&token))).expect("syncable");

        assert_eq!(read(&path), text, "nothing to change is nothing written");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("metadata")
                .modified()
                .expect("mtime"),
            long_ago,
            "a second `--apply` that touches the file's mtime makes every backup tool and \
             every `git status` over a synced directory think the token changed"
        );
        assert!(
            !says(&report, "->"),
            "no address moved, so the report must not claim one did: {report:?}"
        );
    }

    #[test]
    fn an_empty_or_missing_token_is_filled_and_the_report_never_prints_it() {
        let token = "t".repeat(64);
        for text in [
            "CUBA_HTTP_ADDR=192.168.0.10:8787\nCUBA_HTTP_TOKEN=\n",
            "CUBA_HTTP_ADDR=192.168.0.10:8787\n",
        ] {
            let scratch = Scratch::new();
            let path = write_env(&scratch, text);

            let report = sync_env_file(&path, &unit_at(ROUTABLE, Some(&token))).expect("syncable");

            let after = read(&path);
            assert_eq!(
                env_value(&after, "CUBA_HTTP_TOKEN").as_deref(),
                Some(token.as_str()),
                "a LAN daemon with no token refuses to start; the installer fills it: {after:?}"
            );
            assert!(
                after.starts_with("CUBA_HTTP_ADDR=192.168.0.10:8787\n"),
                "and the address line, which was right, is not touched: {after:?}"
            );
            assert!(
                says(&report, "token: rellenado"),
                "the operator is told a token was written: {report:?}"
            );
            assert!(
                !says(&report, &token),
                "and is never shown it: the report goes to a terminal, a scrollback and a CI \
                 log: {report:?}"
            );
        }
    }

    #[test]
    fn a_token_that_is_there_is_never_overwritten() {
        let scratch = Scratch::new();
        let theirs = "a".repeat(64);
        let other = "b".repeat(64);
        let text = operators_file(ROUTABLE, "\n", &theirs);
        let path = write_env(&scratch, &text);

        let report = sync_env_file(&path, &unit_at(ROUTABLE, Some(&other))).expect("syncable");

        assert_eq!(
            read(&path),
            text,
            "the token every client is already configured with is the file's; a sync that \
             writes the unit's over it logs them all out at once"
        );
        assert!(
            says(&report, "token: conservado"),
            "and the report says it was kept: {report:?}"
        );
    }

    #[test]
    fn going_down_to_loopback_moves_the_address_and_keeps_the_token() {
        let scratch = Scratch::new();
        let token = "t".repeat(64);
        let text = operators_file(ROUTABLE, "\n", &token);
        let path = write_env(&scratch, &text);

        let report = sync_env_file(&path, &unit_at(LOOPBACK, None)).expect("syncable");

        let after = read(&path);
        assert_eq!(
            after,
            text.replace(
                "CUBA_HTTP_ADDR=192.168.0.10:8787\n",
                "CUBA_HTTP_ADDR=127.0.0.1:8787\n"
            ),
            "an explicit --profile loopback is the one way down, and it moves the address and \
             nothing else"
        );
        assert_eq!(
            env_value(&after, "CUBA_HTTP_TOKEN").as_deref(),
            Some(token.as_str()),
            "the token stays: going back up must not hand the clients a new one"
        );
        assert!(
            says(&report, "dirección: 192.168.0.10:8787 -> 127.0.0.1:8787"),
            "never silent: the operator is told the daemon stopped being reachable: {report:?}"
        );
    }

    #[test]
    fn a_loopback_install_gets_no_token_even_when_the_one_in_the_file_is_empty() {
        let scratch = Scratch::new();
        let text = "CUBA_HTTP_ADDR=127.0.0.1:8787\nCUBA_HTTP_TOKEN=\n";
        let path = write_env(&scratch, text);

        sync_env_file(&path, &unit_at(LOOPBACK, None)).expect("syncable");

        assert_eq!(
            read(&path),
            text,
            "nothing reaches 127.0.0.1 from outside, so a token nobody asked for is one more \
             secret to rotate"
        );
    }

    #[test]
    fn a_file_without_an_address_line_gets_one_at_the_end() {
        let scratch = Scratch::new();
        let text = "DATABASE_URL=postgresql://operador@db/brain\nCUBA_MODE=completo\n";
        let path = write_env(&scratch, text);

        let report = sync_env_file(&path, &unit_at(LOOPBACK, None)).expect("syncable");

        let after = read(&path);
        assert!(
            after.starts_with(text),
            "what the operator wrote stays where it was: {after:?}"
        );
        assert_eq!(
            after.lines().last(),
            Some("CUBA_HTTP_ADDR=127.0.0.1:8787"),
            "the address the daemon will bind has to be in the file `doctor` reads: {after:?}"
        );
        assert!(
            says(&report, "127.0.0.1:8787"),
            "and the report names the address it added: {report:?}"
        );
    }

    #[test]
    fn with_no_env_file_sync_creates_nothing() {
        let scratch = Scratch::new();
        let path = scratch.env_path();

        sync_env_file(&path, &unit_at(ROUTABLE, Some(&"t".repeat(64))))
            .expect("a missing file is not an error: `write_all` creates it");

        assert!(
            !path.exists(),
            "creating the file is `write_all`'s job, and it creates it with the right header"
        );
    }

    #[test]
    fn an_env_path_that_cannot_be_read_is_an_error_and_not_an_absent_file() {
        let scratch = Scratch::new();
        let path = scratch.env_path();
        std::fs::create_dir(&path).expect("a directory where the env file should be");

        let refused = sync_env_file(&path, &unit_at(ROUTABLE, Some(&"t".repeat(64))))
            .expect_err("a path that is there and cannot be read is not «no install»");

        let said = format!("{refused:#}");
        assert!(
            said.contains("no se pudo leer") && said.contains(&path.display().to_string()),
            "reading None into «nothing to sync» leaves the daemon on the old address while \
             `--apply` reports success; the error has to name the path: {said}"
        );
    }

    #[test]
    fn a_key_missing_from_the_file_is_appended_as_one_line_and_never_after_a_blank_one() {
        for (text, expected) in [
            ("A=1\nB=2\n", "A=1\nB=2\nCUBA_HTTP_ADDR=127.0.0.1:8787\n"),
            ("A=1\nB=2", "A=1\nB=2\nCUBA_HTTP_ADDR=127.0.0.1:8787\n"),
            ("", "CUBA_HTTP_ADDR=127.0.0.1:8787\n"),
        ] {
            assert_eq!(
                set_env_value(text, "CUBA_HTTP_ADDR", LOOPBACK),
                expected,
                "the separator is a newline only when the last line lacks one: a file that \
                 already ends in «\\n» (or is empty) gets no blank line in front of the key, \
                 and one that does not gets exactly one break. Input {text:?}"
            );
        }
    }

    #[test]
    fn the_temp_file_of_a_rewrite_lives_next_to_the_file_it_replaces() {
        let beside = temp_beside(Path::new("/opt/mi/memory-industry.env"));
        assert_eq!(
            beside.parent(),
            Some(Path::new("/opt/mi")),
            "a temp elsewhere makes the rename a copy across filesystems, which is not atomic \
             and can leave half a token file: {beside:?}"
        );
        let name = beside.file_name().and_then(|name| name.to_str());
        assert!(
            name.is_some_and(
                |name| name.starts_with(".memory-industry.env.") && name.ends_with(".tmp")
            ),
            "and it is recognisable as ours, so a leftover is not mistaken for the operator's: \
             {beside:?}"
        );

        let bare = temp_beside(Path::new("memory-industry.env"));
        assert_eq!(
            bare.parent(),
            Some(Path::new(".")),
            "a bare file name has an empty parent, and the current directory is where it \
             lives: {bare:?}"
        );
    }

    // --- The elevation probe -------------------------------------------------------

    type Answer = Box<dyn FnMut(&[String]) -> io::Result<CommandOutcome>>;

    /// Records every argv it is given and answers from a closure. No test in
    /// this module spawns a process.
    struct Fake {
        calls: Vec<Vec<String>>,
        answer: Answer,
    }

    impl Fake {
        fn new(answer: impl FnMut(&[String]) -> io::Result<CommandOutcome> + 'static) -> Self {
            Self {
                calls: Vec::new(),
                answer: Box::new(answer),
            }
        }

        fn succeeding() -> Self {
            Self::new(|_| Ok(exit(0)))
        }

        /// Exits 1 for the one command whose verb is `verb`, 0 for the rest.
        fn failing(verb_that_fails: &'static str) -> Self {
            Self::new(move |argv| Ok(exit(i32::from(verb(argv) == verb_that_fails))))
        }
    }

    impl CommandRunner for Fake {
        fn run(&mut self, program: &str, args: &[String]) -> io::Result<CommandOutcome> {
            let argv: Vec<String> = std::iter::once(program.to_string())
                .chain(args.iter().cloned())
                .collect();
            self.calls.push(argv.clone());
            (self.answer)(&argv)
        }
    }

    fn exit(code: i32) -> CommandOutcome {
        CommandOutcome {
            code: Some(code),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    /// `schtasks` for the task, `delete` or `add` for `netsh advfirewall
    /// firewall <verb> rule`.
    fn verb(argv: &[String]) -> &str {
        if argv.first().is_some_and(|program| program == "schtasks") {
            "schtasks"
        } else {
            argv.get(3).map_or("", String::as_str)
        }
    }

    fn probe_answer(case: &'static str) -> io::Result<CommandOutcome> {
        match case {
            "administrator" => Ok(exit(0)),
            "denied" => Ok(exit(5)),
            "killed" => Ok(CommandOutcome {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
            }),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    #[test]
    fn only_a_successful_fltmc_counts_as_elevated() {
        for (case, expected) in [
            ("administrator", true),
            ("denied", false),
            ("killed", false),
            ("no fltmc on this machine", false),
        ] {
            let mut runner = Fake::new(move |_| probe_answer(case));

            assert_eq!(
                probe_elevated(&mut runner),
                expected,
                "{case}: only a clean exit means the console may create a task for SYSTEM. A \
                 command that cannot even start is «not elevated», never a panic"
            );
            assert_eq!(
                runner
                    .calls
                    .iter()
                    .map(|c| c[0].as_str())
                    .collect::<Vec<_>>(),
                ["fltmc"],
                "{case}: the probe is one `fltmc` and nothing that changes the machine"
            );
        }
    }

    // --- register ------------------------------------------------------------------

    #[test]
    fn without_elevation_register_refuses_and_runs_nothing() {
        let mut runner = Fake::succeeding();

        let refused = register(
            &mut runner,
            false,
            &unit_at(ROUTABLE, Some(&"t".repeat(64))),
            &xml(),
        )
        .expect_err("a console that cannot create a task for SYSTEM must not try");

        let said = format!("{refused:#}");
        assert!(
            said.contains("administrador") && said.contains("--register"),
            "the refusal names the right that is missing and the way out (an elevated console, \
             or no --register): {said}"
        );
        assert!(
            runner.calls.is_empty(),
            "not even the firewall delete: a half-applied --register leaves a door open and no \
             task behind it. Called: {:?}",
            runner.calls
        );
    }

    #[test]
    fn an_elevated_lan_register_opens_the_door_and_then_creates_the_task_in_that_order() {
        let token = "t".repeat(64);
        let mut runner = Fake::succeeding();

        let report = register(&mut runner, true, &unit_at(ROUTABLE, Some(&token)), &xml())
            .expect("an elevated console registers a LAN install");

        assert_eq!(
            runner.calls,
            vec![
                firewall_delete_argv(),
                firewall_add_argv(8787),
                schtasks_create_argv(&xml())
            ],
            "delete, add, task. Add before delete removes the rule that was just made; the task \
             before the rule starts a daemon nobody can reach"
        );
        assert!(!report.is_empty(), "anchor: register reports what it did");
        for text in runner.calls.iter().flatten().chain(report.iter()) {
            assert!(
                !text.contains(&token),
                "the bearer token reached a command line or a report line, where `tasklist`, \
                 the event log and the terminal scrollback keep it: {text}"
            );
        }
    }

    #[test]
    fn a_loopback_register_creates_the_task_and_explains_why_the_firewall_was_left_alone() {
        let mut runner = Fake::succeeding();

        let report = register(&mut runner, true, &unit_at(LOOPBACK, None), &xml())
            .expect("an elevated console registers a loopback install");

        assert_eq!(
            runner.calls,
            vec![schtasks_create_argv(&xml())],
            "nothing reaches 127.0.0.1 from outside: a rule for it is a hole opened for no one"
        );
        assert!(
            report
                .iter()
                .any(|line| line.contains("firewall") && line.contains("loopback")),
            "and the operator is told why there is no rule, or the missing one reads as a \
             failure: {report:?}"
        );
    }

    /// `netsh advfirewall firewall delete|add rule name=… localport=…`, as a
    /// set of `name:port` strings: a delete takes every rule of its name, an
    /// add puts one.
    fn firewall_model(rules: Rc<RefCell<Vec<String>>>) -> Fake {
        Fake::new(move |argv| {
            let property = |key: &str| {
                argv.iter()
                    .find_map(|a| a.strip_prefix(key))
                    .unwrap_or_default()
                    .to_string()
            };
            let name = property("name=");
            match verb(argv) {
                "delete" => rules
                    .borrow_mut()
                    .retain(|r| !r.starts_with(&format!("{name}:"))),
                "add" => rules
                    .borrow_mut()
                    .push(format!("{name}:{}", property("localport="))),
                _ => {}
            }
            Ok(exit(0))
        })
    }

    #[test]
    fn registering_twice_leaves_one_rule_and_the_same_commands() {
        let rules = Rc::new(RefCell::new(Vec::new()));
        let mut runner = firewall_model(rules.clone());
        let unit = unit_at(ROUTABLE, Some(&"t".repeat(64)));

        register(&mut runner, true, &unit, &xml()).expect("first run");
        let first = runner.calls.clone();
        register(&mut runner, true, &unit, &xml()).expect("second run");

        assert_eq!(
            *rules.borrow(),
            ["memory-industry:8787"],
            "two runs, one rule: without the delete the second `add` stacks a duplicate and \
             every later run adds another"
        );
        assert_eq!(
            runner.calls[first.len()..],
            first[..],
            "the second run is the same sequence of commands, so repeating it is safe by \
             construction and not by luck"
        );

        let moved = unit_at("192.168.0.10:9000", Some(&"t".repeat(64)));
        register(&mut runner, true, &moved, &xml()).expect("third run, other port");
        assert_eq!(
            *rules.borrow(),
            ["memory-industry:9000"],
            "a changed port replaces the rule; the old one left open on 8787 is an orphan door"
        );
    }

    #[test]
    fn a_delete_that_finds_no_rule_is_not_a_failure() {
        let mut runner = Fake::failing("delete");

        register(
            &mut runner,
            true,
            &unit_at(ROUTABLE, Some(&"t".repeat(64))),
            &xml(),
        )
        .expect("netsh exits 1 when there is no rule to delete, which is every first run");

        assert_eq!(
            runner.calls.len(),
            3,
            "and the run goes on to the add and the task: {:?}",
            runner.calls
        );
    }

    #[test]
    fn a_failed_add_says_the_task_was_not_registered_and_never_runs_schtasks() {
        let mut runner = Fake::failing("add");

        let failed = register(
            &mut runner,
            true,
            &unit_at(ROUTABLE, Some(&"t".repeat(64))),
            &xml(),
        )
        .expect_err("a rule that could not be added is a failure");

        let said = format!("{failed:#}");
        assert!(
            said.contains("regla de firewall") && said.contains("la tarea NO se registró"),
            "the operator must learn what is done and what is not: {said}"
        );
        assert!(
            runner.calls.iter().all(|call| call[0] != "schtasks"),
            "a task that starts a daemon behind a closed door is worse than none: {:?}",
            runner.calls
        );
    }

    #[test]
    fn a_failed_task_says_the_rule_stays_and_that_repeating_is_safe() {
        let mut runner = Fake::failing("schtasks");

        let failed = register(
            &mut runner,
            true,
            &unit_at(ROUTABLE, Some(&"t".repeat(64))),
            &xml(),
        )
        .expect_err("a task that could not be created is a failure");

        let said = format!("{failed:#}");
        assert!(
            said.contains("la regla de firewall quedó puesta")
                && said.contains("repetir --register es seguro"),
            "there is no rollback, so the error is the rollback plan: what stayed, and that the \
             next run needs no cleanup: {said}"
        );
    }

    // --- require_elevated: the refusal that comes before any file is written --------

    #[test]
    fn require_elevated_refuses_after_one_fltmc_probe_and_lets_an_administrator_through() {
        let mut denied = Fake::new(|_| probe_answer("denied"));

        let refused = require_elevated(&mut denied)
            .expect_err("a console without administrator rights must not get to write files");

        let said = format!("{refused:#}");
        assert!(
            said.contains("administrador") && said.contains("--register"),
            "the refusal names the right that is missing and the flag it is for: {said}"
        );
        assert_eq!(
            denied.calls,
            vec![vec!["fltmc".to_string()]],
            "the check is one `fltmc` and nothing that changes the machine"
        );

        let mut administrator = Fake::new(|_| probe_answer("administrator"));
        require_elevated(&mut administrator).expect("an elevated console goes on");
        assert_eq!(
            administrator.calls,
            vec![vec!["fltmc".to_string()]],
            "and it was asked, not assumed"
        );
    }

    // --- reason: what a failed command said -----------------------------------------

    fn reason_of(code: Option<i32>, stdout: &str, stderr: &str) -> String {
        reason(&CommandOutcome {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }

    #[test]
    fn the_reason_is_the_first_non_blank_line_of_stderr_then_stdout_with_the_exit_code() {
        for (code, stdout, stderr, expected) in [
            (Some(1), "", "", "con código 1: sin mensaje"),
            (
                Some(1),
                "  solo stdout  \nsegunda",
                "",
                "con código 1: solo stdout",
            ),
            (
                Some(2),
                "",
                "solo stderr\nsegunda",
                "con código 2: solo stderr",
            ),
            (Some(3), "out", "err", "con código 3: err"),
            (
                Some(1),
                "",
                "\n   \n  ERROR: no rule  \nresto",
                "con código 1: ERROR: no rule",
            ),
            (Some(1), "out", " \n\t\n", "con código 1: out"),
            (None, "", "killed", "sin código de salida: killed"),
        ] {
            assert_eq!(
                reason_of(code, stdout, stderr),
                expected,
                "stdout {stdout:?}, stderr {stderr:?}: `netsh` names the cause on its first \
                 non-blank line, blank lines in front are not a message, stderr wins over \
                 stdout, and an empty answer says so instead of ending in «: »"
            );
        }
    }

    // --- next_steps: the end of --apply ---------------------------------------------

    #[test]
    fn registering_ends_with_the_report_of_what_register_did_and_runs_its_commands() {
        let root = PathBuf::from("/opt/mi");
        let mut runner = Fake::succeeding();

        let report = next_steps(
            &mut runner,
            true,
            &unit_at(ROUTABLE, Some(&"t".repeat(64))),
            Target::Windows,
            &root,
        )
        .expect("an elevated console registers a LAN install");

        assert_eq!(
            report,
            [
                format!(
                    "firewall: regla {FIREWALL_RULE_NAME} abierta en TCP 8787 (redes privada y de \
                     dominio)"
                ),
                format!("tarea: {TASK_NAME} registrada (arranca con el equipo, como SYSTEM)"),
            ],
            "the operator reads this to learn that the door and the task exist"
        );
        assert_eq!(
            runner.calls,
            vec![
                firewall_delete_argv(),
                firewall_add_argv(8787),
                schtasks_create_argv(&root.join(TASK_XML_BASENAME))
            ],
            "the task registered is the XML `--apply` wrote under the install root"
        );

        let mut loopback = Fake::succeeding();
        let report = next_steps(
            &mut loopback,
            true,
            &unit_at(LOOPBACK, None),
            Target::Windows,
            &root,
        )
        .expect("an elevated console registers a loopback install");
        assert_eq!(
            report,
            [
                format!(
                    "firewall: no se abrió ningún puerto porque {LOOPBACK} es loopback y ninguna \
                     otra máquina lo alcanza"
                ),
                format!("tarea: {TASK_NAME} registrada (arranca con el equipo, como SYSTEM)"),
            ],
            "and for loopback, why there is no rule"
        );
        assert_eq!(
            loopback.calls,
            vec![schtasks_create_argv(&root.join(TASK_XML_BASENAME))],
            "a loopback install talks to schtasks only"
        );
    }

    #[test]
    fn not_registering_ends_with_the_commands_to_run_by_hand_and_runs_none() {
        let root = PathBuf::from("/opt/mi");
        let token = "t".repeat(64);

        for (target, unit, must_say) in [
            (
                Target::Windows,
                unit_at(ROUTABLE, Some(&token)),
                "netsh advfirewall firewall add rule",
            ),
            (
                Target::Windows,
                unit_at(LOOPBACK, None),
                "schtasks /Create /XML",
            ),
            (
                Target::Linux,
                unit_at(ROUTABLE, Some(&token)),
                "systemctl --user enable --now memory-industry.service",
            ),
        ] {
            let mut runner = Fake::succeeding();

            let steps = next_steps(&mut runner, false, &unit, target, &root)
                .expect("printing instructions cannot fail");

            assert_eq!(
                steps,
                manual_steps(&unit.addr, target, &root),
                "{target:?} {}: without --register the end of `--apply` is the manual steps, \
                 as `manual_steps` renders them",
                unit.addr
            );
            assert!(
                steps
                    .first()
                    .is_some_and(|line| line.starts_with("Habilitalo con"))
                    && steps.iter().any(|line| line.contains(must_say)),
                "{target:?} {}: an empty or invented list leaves the operator with a daemon \
                 that was never enabled: {steps:?}",
                unit.addr
            );
            assert!(
                runner.calls.is_empty(),
                "the operator did not ask for --register, so nothing touches the machine: {:?}",
                runner.calls
            );
        }
    }
}
