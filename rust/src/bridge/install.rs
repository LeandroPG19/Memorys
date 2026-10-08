//! `memory-industry bridge install|uninstall`: a per-user logon task that starts the bridge
//! without an administrator. Every command goes through the injected `CommandRunner`.

use std::path::Path;

use anyhow::Result;

use crate::service_apply::CommandRunner;

// PASS-1 STUB: every function answers "nothing" until pass 2 implements it.
pub fn install_commands(_exe: &Path, _print_only: bool) -> Result<Vec<Vec<String>>> {
    Ok(Vec::new())
}

// PASS-1 STUB
pub fn run_install(_runner: &mut dyn CommandRunner, _exe: &Path) -> Result<Vec<String>> {
    Ok(Vec::new())
}

// PASS-1 STUB
pub fn run_uninstall(_runner: &mut dyn CommandRunner) -> Result<Vec<String>> {
    Ok(Vec::new())
}

// PASS-1 STUB
pub fn execute_install(
    _runner: &mut dyn CommandRunner,
    _exe: &Path,
    _print_only: bool,
) -> Result<Vec<String>> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::super::fake_host::ScriptedRunner;
    use super::*;
    use crate::service_apply::CommandOutcome;

    const EXE: &str = r"C:\Users\ana\AppData\Local\MemoryIndustry\memory-industry.exe";
    const EXE_WITH_SPACES: &str = r"C:\Program Files\Memory Industry\memory-industry.exe";

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(ToString::to_string).collect()
    }

    /// The `/TR` argument as the task will run it: the exe quoted, so a path with spaces
    /// survives as one program.
    fn task_run(exe: &str) -> String {
        format!("conhost.exe --headless \"{exe}\" bridge run")
    }

    fn create_argv(exe: &str) -> Vec<String> {
        let mut argv = strings(&[
            "schtasks",
            "/Create",
            "/SC",
            "ONLOGON",
            "/TN",
            "MemoryIndustry Bridge",
            "/TR",
        ]);
        argv.push(task_run(exe));
        argv.extend(strings(&["/RL", "LIMITED", "/F"]));
        argv
    }

    fn run_argv() -> Vec<String> {
        strings(&["schtasks", "/Run", "/TN", "MemoryIndustry Bridge"])
    }

    fn end_argv() -> Vec<String> {
        strings(&["schtasks", "/End", "/TN", "MemoryIndustry Bridge"])
    }

    fn delete_argv() -> Vec<String> {
        strings(&["schtasks", "/Delete", "/TN", "MemoryIndustry Bridge", "/F"])
    }

    /// An exe path whose `/TR` is exactly `tr_len` characters.
    fn exe_for_tr_len(tr_len: usize) -> String {
        let fixed = task_run("").chars().count();
        let prefix = r"C:\m\";
        let filler = tr_len - fixed - prefix.chars().count();
        format!("{prefix}{}", "a".repeat(filler))
    }

    /// Answers each `schtasks` verb from a table and records every call, in order.
    struct Schtasks {
        calls: Vec<Vec<String>>,
        answers: Vec<(&'static str, i32, &'static str)>,
        cannot_start: bool,
    }

    impl Schtasks {
        fn answering(answers: &[(&'static str, i32, &'static str)]) -> Self {
            Schtasks {
                calls: Vec::new(),
                answers: answers.to_vec(),
                cannot_start: false,
            }
        }

        fn missing() -> Self {
            Schtasks {
                calls: Vec::new(),
                answers: Vec::new(),
                cannot_start: true,
            }
        }
    }

    impl CommandRunner for Schtasks {
        fn run(&mut self, program: &str, args: &[String]) -> io::Result<CommandOutcome> {
            let argv: Vec<String> = std::iter::once(program.to_string())
                .chain(args.iter().cloned())
                .collect();
            self.calls.push(argv);
            if self.cannot_start {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            let verb = args.first().map_or("", String::as_str);
            let (code, stderr) = self
                .answers
                .iter()
                .find(|(asked, _, _)| *asked == verb)
                .map_or((0, ""), |(_, code, stderr)| (*code, *stderr));
            Ok(CommandOutcome {
                code: Some(code),
                stdout: String::new(),
                stderr: stderr.to_string(),
            })
        }
    }

    fn install_commands_ok(exe: &str) -> Vec<Vec<String>> {
        match install_commands(Path::new(exe), false) {
            Ok(commands) => commands,
            Err(err) => panic!("install_commands must accept {exe}. Got: {err:#}"),
        }
    }

    /// I1
    #[test]
    fn install_creates_a_per_user_logon_task_and_then_runs_it() {
        let commands = install_commands_ok(EXE);

        assert_eq!(
            commands,
            vec![create_argv(EXE), run_argv()],
            "the bridge is a per-user task that fires at logon, limited rights, forced over an \
             older copy, and started now so the operator need not sign out. Any other argv is a \
             task that needs an administrator or that does not exist until the next logon"
        );
    }

    /// I1
    #[test]
    fn the_task_asks_for_no_elevation_and_no_other_account() {
        let commands = install_commands_ok(EXE);

        let words: Vec<&str> = commands.iter().flatten().map(String::as_str).collect();
        for forbidden in ["/RU", "SYSTEM", "HIGHEST", "/XML"] {
            assert!(
                !words.contains(&forbidden),
                "the argv carries {forbidden}, which makes the task something a standard user \
                 cannot create. Argv: {commands:?}"
            );
        }
        assert!(
            words.contains(&"LIMITED"),
            "the anchor for the absence checks above: /RL LIMITED must be there. Argv: {commands:?}"
        );
    }

    /// I1
    #[test]
    fn a_path_with_spaces_stays_one_quoted_argument() {
        let commands = install_commands_ok(EXE_WITH_SPACES);

        assert_eq!(
            commands.len(),
            2,
            "a create and a run are expected. Got: {commands:?}"
        );
        let create = &commands[0];
        assert_eq!(
            create.len(),
            create_argv(EXE_WITH_SPACES).len(),
            "the spaces in the path must not split /TR into several arguments. Argv: {create:?}"
        );
        assert_eq!(
            create[7],
            task_run(EXE_WITH_SPACES),
            "the exe must sit inside quotes within the single /TR value, or schtasks runs \
             `C:\\Program` and fails at every logon"
        );
    }

    /// I1
    #[test]
    fn the_run_value_may_be_261_characters_and_not_one_more() {
        let at_the_limit = exe_for_tr_len(261);
        assert_eq!(
            task_run(&at_the_limit).chars().count(),
            261,
            "the helper must build the boundary it claims"
        );
        let accepted = install_commands_ok(&at_the_limit);
        assert_eq!(
            accepted
                .first()
                .and_then(|create| create.get(7))
                .map(|tr| tr.chars().count()),
            Some(261),
            "a /TR of exactly 261 characters is what schtasks accepts"
        );

        let too_long = exe_for_tr_len(262);
        let err = install_commands(Path::new(&too_long), false)
            .expect_err("a /TR of 262 characters is refused by schtasks at the first logon");
        assert!(
            format!("{err:#}").contains("261"),
            "the error must give the limit so the operator knows to move the exe to a shorter \
             path. Got: {err:#}"
        );
    }

    /// I1/I2
    #[test]
    fn print_shows_the_same_commands_that_install_runs() {
        let printed = install_commands(Path::new(EXE), true).expect("print accepts the same exe");
        assert_eq!(
            printed.len(),
            2,
            "the plan has a create and a run to show. Got: {printed:?}"
        );
        assert_eq!(
            printed,
            install_commands_ok(EXE),
            "`--print` is the dry run of install: if it showed other commands, reading it first \
             would prove nothing"
        );
    }

    /// I2
    #[test]
    fn install_sends_the_create_and_the_run_and_nothing_else() {
        let mut runner = Schtasks::answering(&[]);

        let lines = run_install(&mut runner, Path::new(EXE)).expect("both commands succeed");

        assert_eq!(
            runner.calls,
            vec![create_argv(EXE), run_argv()],
            "exactly the two schtasks calls: no `fltmc` (elevation probe), no `netsh` (firewall). \
             The bridge listens on loopback and needs neither"
        );
        assert!(
            !lines.is_empty(),
            "the operator must be told what was done, not left with silence"
        );
    }

    /// I2
    #[test]
    fn a_failed_create_is_reported_and_the_task_is_never_run() {
        let mut runner = Schtasks::answering(&[("/Create", 1, "ERROR: Access is denied.")]);

        let err = run_install(&mut runner, Path::new(EXE))
            .expect_err("a task that was not created cannot be started");

        let said = format!("{err:#}");
        assert!(
            said.contains("/Create") && said.contains("Access is denied"),
            "the error must name the command that failed and carry schtasks' own reason, or the \
             operator cannot tell a policy block from a typo. Got: {said}"
        );
        assert_eq!(
            runner.calls,
            vec![create_argv(EXE)],
            "after a failed /Create the next call would start a task that does not exist (or an \
             old one from a previous install)"
        );
    }

    /// I2
    #[test]
    fn a_machine_without_schtasks_is_an_error_that_names_it() {
        let mut runner = Schtasks::missing();

        let err = run_install(&mut runner, Path::new(EXE))
            .expect_err("a runner that cannot start the program is not a success");

        assert!(
            format!("{err:#}").contains("schtasks"),
            "the operator must learn which program was missing (this is not a Windows host?). \
             Got: {err:#}"
        );
    }

    /// I2
    #[test]
    fn uninstall_stops_then_deletes_even_when_the_task_was_not_running() {
        let mut runner = Schtasks::answering(&[(
            "/End",
            1,
            "ERROR: There is no running instance of the task.",
        )]);

        let lines = run_uninstall(&mut runner)
            .expect("a task that is not running is not a reason to keep it installed");

        assert_eq!(
            runner.calls,
            vec![end_argv(), delete_argv()],
            "uninstall is /End and then /Delete /F, in that order: deleting a running task would \
             leave the bridge up until the next logon"
        );
        assert!(!lines.is_empty(), "the operator must be told what was done");
    }

    /// I2
    #[test]
    fn uninstall_of_a_task_that_does_not_exist_ends_well() {
        let mut runner = Schtasks::answering(&[
            (
                "/End",
                1,
                "ERROR: The system cannot find the file specified.",
            ),
            (
                "/Delete",
                1,
                "ERROR: The system cannot find the file specified.",
            ),
        ]);

        run_uninstall(&mut runner)
            .expect("removing what is already gone is the state the operator asked for");

        assert_eq!(
            runner.calls,
            vec![end_argv(), delete_argv()],
            "both calls are still made: the fake answers «not found» to each"
        );
    }

    /// I2
    #[test]
    fn print_only_never_touches_the_runner_and_shows_the_commands() {
        let mut runner = ScriptedRunner::idle();

        let lines = execute_install(&mut runner, Path::new(EXE), true)
            .expect("printing the plan cannot fail for a sane exe");

        assert!(
            runner.calls.is_empty(),
            "`bridge install --print` ran {:?}: it exists to look without touching the machine",
            runner.calls
        );
        let shown = lines.join("\n");
        for needle in ["schtasks", "/Create", "ONLOGON", "bridge run"] {
            assert!(
                shown.contains(needle),
                "the printed plan must show the command it stands for ({needle} missing). \
                 Got: {shown}"
            );
        }
    }

    /// I2
    #[test]
    fn without_print_the_cli_path_runs_the_two_commands() {
        let mut runner = ScriptedRunner::idle();

        execute_install(&mut runner, Path::new(EXE), false).expect("both succeed");

        assert_eq!(
            runner.calls,
            vec![create_argv(EXE), run_argv()],
            "without --print the same commands reach the runner"
        );
    }
}
