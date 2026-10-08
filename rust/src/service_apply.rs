//! What `setup service --apply` does to a machine that already has an install:
//! it keeps the address and the token the operator set, and with `--register`
//! it hands the OS the scheduled task and the firewall rule.
//!
//! Everything that DECIDES lives here, behind `CommandRunner`, because
//! `setup_agent.rs` is outside `cargo mutants` (`quality-gate.sh:164`): there
//! the real runner spawns the process and prints, and nothing else.
//!
//! PASS 1 OF 2 (0.29.0, slice C): the types are the real seams, the function
//! bodies are stand-ins that behave like 0.28 — they change nothing — so the
//! tests below compile and fail on their assertions. Pass 2 replaces every
//! `PASS-1 STUB` and leaves the `#[cfg(test)]` region byte for byte.

use std::path::Path;

use anyhow::Result;

use crate::service::Unit;

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

/// The last effective `KEY=value` line's value, trimmed. A `#` line is not
/// effective, and CRLF is tolerated.
// PASS-1 STUB: 0.28 never reads a key back out of the env file; pass 2 parses it. Returns None.
pub fn env_value(_text: &str, _key: &str) -> Option<String> {
    None
}

/// Replaces ONLY the last effective line of `key`, keeping its own line ending;
/// appends `key=value` when there is none; every other byte is untouched.
// PASS-1 STUB: 0.28 never edits an env file; pass 2 does the line surgery. Returns the text as it came.
pub fn set_env_value(text: &str, _key: &str, _value: &str) -> String {
    text.to_string()
}

/// Brings an existing env file in line with what `unit` resolved: the address
/// always, the token only when the file's is empty. Atomic (temp file in the
/// same directory, then rename), 0600 on unix, and no rewrite when nothing
/// changes. Returns the lines `--apply` reports.
// PASS-1 STUB: 0.28 leaves an existing env file exactly as it is; pass 2 syncs it. Reports nothing.
pub fn sync_env_file(_path: &Path, _unit: &Unit) -> Result<Vec<String>> {
    Ok(Vec::new())
}

/// Whether this console may create a scheduled task for SYSTEM and open a
/// firewall port: `fltmc` succeeds only for an administrator. Never panics.
// PASS-1 STUB: 0.28 never asks; pass 2 runs `fltmc`. Answers false without running anything.
pub fn probe_elevated(_runner: &mut dyn CommandRunner) -> bool {
    false
}

/// `--register`: the firewall rule (only for a routable address) and then the
/// task, in that order, each safe to repeat.
// PASS-1 STUB: 0.28 only prints the commands; pass 2 runs them through the runner. Does nothing, returns Ok.
pub fn register(
    _runner: &mut dyn CommandRunner,
    _elevated: bool,
    _unit: &Unit,
    _xml: &Path,
) -> Result<Vec<String>> {
    Ok(Vec::new())
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
}
