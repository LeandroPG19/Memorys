//! 0.31.0 · T-DBURL. A process the judge starts with no `DATABASE_URL` reaches no default database.
//!
//! The defect: `resolve_database_url` answers an unset `DATABASE_URL` by asking `docker ps` for
//! the developer's own container and, if it runs, handing back its URL (`127.0.0.1:5488`, the live
//! corpus); and if there is none, by creating a PostgreSQL container with a persistent volume. A
//! test child that removed `DATABASE_URL` and then reached the resolver would have written to that
//! corpus or built a container. Nothing reached it, by luck of which subcommand each child ran.
//!
//! `MEMORY_INDUSTRY_REQUIRE_DATABASE_URL` turns that default into a refusal: exit 2, a sentence
//! naming both variables, and not one call to `docker`. This file holds the three halves of the
//! guarantee:
//!
//! - the binary with the switch refuses (§2.1) and without it still takes the default path (§2.2),
//!   both against a stand-in `docker` that records its calls and answers like a machine with no
//!   container. With that stand-in no check, however broken, reaches `127.0.0.1:5488` or creates
//!   anything: `ps` finds nothing, so `port_answers` is never asked, and `run` fails;
//! - no test of the tree removes `DATABASE_URL` without setting the switch (§2.4), so a test is
//!   protected also when it runs by hand, outside the judge that exports it.
//!
//! No server is needed and the file carries no `needs-a-server` line. The binary tests are unix
//! only: the stand-in is a `#!/bin/sh` script, as in `v029_bridge_cli`. The scan runs everywhere.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SWITCH: &str = "MEMORY_INDUSTRY_REQUIRE_DATABASE_URL";

#[cfg(unix)]
mod binary {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

    /// The most any run may take. The refusal and the default path against the stand-in both end in
    /// well under a second; a run still going at this point is a binary waiting for something.
    const BUDGET: Duration = Duration::from_secs(10);

    /// A machine of this test's own: a home, a working directory and a `bin/` holding the only
    /// `docker` any child can find. Thrown away on the way out.
    pub struct Machine(PathBuf);

    pub struct Run {
        pub code: Option<i32>,
        pub stdout: String,
        pub stderr: String,
    }

    impl Machine {
        pub fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("mi-v047-{}", uuid::Uuid::new_v4()));
            for sub in ["home", "work", "bin"] {
                std::fs::create_dir_all(dir.join(sub)).expect("scratch directory is creatable");
            }
            let machine = Self(dir);
            let double = machine.0.join("bin").join("docker");
            // `--version` answers, `ps` finds no container, anything else fails: a machine with
            // Docker installed and no memory-industry-db on it. The calls go to a file in the
            // scratch directory, which is what lets a test say «docker was never called».
            let script = format!(
                "#!/bin/sh\n\
                 echo \"$*\" >> '{}'\n\
                 case \"$1\" in\n\
                 --version) echo 'Docker version 0.0.0-double'; exit 0 ;;\n\
                 ps) exit 0 ;;\n\
                 *) exit 1 ;;\n\
                 esac\n",
                machine.docker_calls_path().display()
            );
            std::fs::write(&double, script).expect("the stand-in is writable");
            std::fs::set_permissions(&double, std::fs::Permissions::from_mode(0o755))
                .expect("the stand-in is executable");
            machine
        }

        fn docker_calls_path(&self) -> PathBuf {
            self.0.join("docker.calls")
        }

        /// Every call the stand-in recorded, one per line; empty when it was never run.
        pub fn docker_calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.docker_calls_path())
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        /// `memory-industry recall --quiet` from the working directory, with nothing in its
        /// environment but a home, the stand-in's `PATH` and, when asked, the switch. Killed, and
        /// the test failed, if it is still running after the budget.
        pub fn recall(&self, switch: Option<&str>) -> Run {
            let mut command = Command::new(BIN);
            command
                .args(["recall", "--quiet"])
                .current_dir(self.0.join("work"))
                .env_clear()
                .env("HOME", self.0.join("home"))
                .env("PATH", self.0.join("bin"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(value) = switch {
                command.env(super::SWITCH, value);
            }
            let mut child = command.spawn().expect("the binary starts");
            let mut out = child.stdout.take().expect("stdout is piped");
            let mut err = child.stderr.take().expect("stderr is piped");
            let read_out = std::thread::spawn(move || {
                let mut text = String::new();
                out.read_to_string(&mut text).ok();
                text
            });
            let read_err = std::thread::spawn(move || {
                let mut text = String::new();
                err.read_to_string(&mut text).ok();
                text
            });

            let deadline = Instant::now() + BUDGET;
            let code = loop {
                if let Some(status) = child.try_wait().expect("the child can be polled") {
                    break status.code();
                }
                if Instant::now() >= deadline {
                    child.kill().ok();
                    child.wait().ok();
                    panic!(
                        "`recall --quiet` was still running after {}s and was killed. A binary \
                         that waits here is waiting for a database or for a container: exactly \
                         what the switch exists to stop. docker calls so far: {}",
                        BUDGET.as_secs(),
                        super::hidden(&self.docker_calls().join(" | "))
                    );
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            Run {
                code,
                stdout: read_out.join().expect("stdout reader"),
                stderr: read_err.join().expect("stderr reader"),
            }
        }
    }

    impl Drop for Machine {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// `text` with every token that can carry a credential replaced, so that no assertion message
/// prints a URL or the password the binary hands to `docker run`.
#[cfg(unix)]
fn hidden(text: &str) -> String {
    text.lines()
        .map(|line| {
            line.split_whitespace()
                .map(|token| {
                    if token.starts_with("POSTGRES_PASSWORD=") {
                        "POSTGRES_PASSWORD=<hidden>"
                    } else if token.contains("postgresql://") || token.contains("memorys2026") {
                        "<hidden>"
                    } else {
                        token
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(unix)]
fn refuses_without_touching_docker(switch_value: &str) {
    let machine = binary::Machine::new();
    let ran = machine.recall(Some(switch_value));
    let calls = machine.docker_calls();

    assert_eq!(
        ran.code,
        Some(2),
        "exit {:?}, expected Some(2): with {SWITCH}={switch_value} and no DATABASE_URL the binary \
         must refuse. docker calls: {}. stderr: {}",
        ran.code,
        hidden(&calls.join(" | ")),
        hidden(&ran.stderr)
    );
    assert!(
        calls.is_empty(),
        "the refusal came after docker was called: {}. It has to come before the first call, or \
         the check that stops the default database has already looked at the developer's \
         containers",
        hidden(&calls.join(" | "))
    );
    assert!(
        ran.stderr.contains("DATABASE_URL") && ran.stderr.contains(SWITCH),
        "the refusal has to name both variables, or the operator reads exit 2 and does not know \
         which one to define or unset. stderr: {}",
        hidden(&ran.stderr)
    );
    for (stream, text) in [("stdout", &ran.stdout), ("stderr", &ran.stderr)] {
        for forbidden in ["postgresql://", "memorys2026"] {
            assert!(
                !text.contains(forbidden),
                "the refusal printed `{forbidden}` on {stream}: it must not show the default URL \
                 or its password"
            );
        }
    }
    assert!(
        !ran.stderr.contains("Attempting automatic PostgreSQL setup"),
        "the refusal ran after the default path announced itself. stderr: {}",
        hidden(&ran.stderr)
    );
}

#[cfg(unix)]
#[test]
fn with_the_switch_and_no_url_the_binary_refuses_with_two_and_never_calls_docker() {
    refuses_without_touching_docker("1");
}

#[cfg(unix)]
#[test]
fn any_non_empty_value_of_the_switch_turns_it_on() {
    refuses_without_touching_docker("0");
}

/// What does not change for anyone: with neither a URL nor the switch the binary still goes down
/// the default path and tries to create the container. It is green before the fix, and its red is
/// the sabotage that makes the default path refuse too.
#[cfg(unix)]
#[test]
fn without_the_switch_the_default_path_still_tries_to_create_the_container() {
    let machine = binary::Machine::new();
    let ran = machine.recall(None);
    let calls = machine.docker_calls();
    let shown = hidden(&calls.join(" | "));

    assert_eq!(
        ran.code,
        Some(1),
        "exit {:?}, expected Some(1): the stand-in fails `docker run`, which is the default \
         path's own failure. docker calls: {shown}. stderr: {}",
        ran.code,
        hidden(&ran.stderr)
    );
    assert!(
        calls
            .first()
            .is_some_and(|first| { first.starts_with("ps -a --filter name=^memory-industry-db$") }),
        "the default path opens by asking for the container: {shown}"
    );
    assert!(
        calls.iter().any(|call| call == "--version"),
        "the default path checks that Docker exists before creating anything: {shown}"
    );
    let creates = calls
        .iter()
        .find(|call| call.starts_with("run -d --name memory-industry-db"));
    let creates = creates.unwrap_or_else(|| {
        panic!("the default path never tried to create the container. docker calls: {shown}")
    });
    for part in [
        "-p 127.0.0.1:5488:5432",
        "-v memory_industry_data:/var/lib/postgresql",
        "pgvector/pgvector:pg18",
    ] {
        assert!(
            creates.contains(part),
            "the container the default path creates lost `{part}`: {}",
            hidden(creates)
        );
    }
    assert!(
        ran.stderr
            .contains("DATABASE_URL not set. Attempting automatic PostgreSQL setup..."),
        "the default path says what it is doing. stderr: {}",
        hidden(&ran.stderr)
    );
    assert!(
        !ran.stderr.contains(SWITCH),
        "without the switch nothing may mention it: the message of the refusal leaked into the \
         default path. stderr: {}",
        hidden(&ran.stderr)
    );
}

// §2.4: the guard that no test removes `DATABASE_URL` and forgets the switch.

/// `text` with comments blanked (newlines kept, so offsets and line numbers stay) and a second
/// copy in which the contents of every string and char literal are also blanked. The second finds
/// the structure (`;`, braces, calls) without a `;` inside a shell snippet cutting a statement; the
/// first reads the literals the structure points at.
fn lexed(text: &str) -> (Vec<u8>, Vec<u8>) {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        Str,
        Raw(usize),
        Line,
        Block(usize),
    }

    let src = text.as_bytes();
    let (mut code, mut masked) = (Vec::with_capacity(src.len()), Vec::with_capacity(src.len()));
    let mut state = State::Code;
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        let next = src.get(i + 1).copied();
        let keep_line = b == b'\n';
        match state {
            State::Code => {
                if b == b'/' && next == Some(b'/') {
                    state = State::Line;
                    blank(&mut code, &mut masked, b);
                } else if b == b'/' && next == Some(b'*') {
                    state = State::Block(1);
                    blank(&mut code, &mut masked, b);
                } else if b == b'"' {
                    state = State::Str;
                    code.push(b);
                    masked.push(b);
                } else if b == b'r' && i.checked_sub(1).is_none_or(|p| !is_ident(src[p])) {
                    let hashes = src[i + 1..].iter().take_while(|c| **c == b'#').count();
                    if src.get(i + 1 + hashes) == Some(&b'"') {
                        // `r#"` is pushed whole; the contents follow in `Raw`.
                        for offset in 0..=hashes + 1 {
                            code.push(src[i + offset]);
                            masked.push(src[i + offset]);
                        }
                        i += hashes + 2;
                        state = State::Raw(hashes);
                        continue;
                    }
                    code.push(b);
                    masked.push(b);
                } else if b == b'\'' {
                    // A char literal (`'"'`, `'\n'`) or a lifetime; only the former has a closing quote.
                    let width = if next == Some(b'\\') {
                        // `'\n'`, `'\''`, `'\u{1F600}'`: the closing quote is past the escaped char.
                        src.get(i + 3..)
                            .unwrap_or_default()
                            .iter()
                            .take(10)
                            .position(|c| *c == b'\'')
                            .map(|p| p + 4)
                    } else if src.get(i + 2) == Some(&b'\'') {
                        Some(3)
                    } else {
                        None
                    };
                    let width = width.unwrap_or(1);
                    for offset in 0..width {
                        code.push(src[i + offset]);
                        masked.push(if offset == 0 || offset == width - 1 {
                            src[i + offset]
                        } else {
                            b'_'
                        });
                    }
                    i += width;
                    continue;
                } else {
                    code.push(b);
                    masked.push(b);
                }
            }
            State::Str => {
                if b == b'\\' {
                    code.push(b);
                    masked.push(b'_');
                    if let Some(escaped) = next {
                        code.push(escaped);
                        masked.push(if escaped == b'\n' { b'\n' } else { b'_' });
                        i += 1;
                    }
                } else if b == b'"' {
                    state = State::Code;
                    code.push(b);
                    masked.push(b);
                } else {
                    code.push(b);
                    masked.push(if keep_line { b'\n' } else { b'_' });
                }
            }
            State::Raw(hashes) => {
                let closes =
                    b == b'"' && src[i + 1..].iter().take_while(|c| **c == b'#').count() >= hashes;
                if closes {
                    for offset in 0..=hashes {
                        code.push(src[i + offset]);
                        masked.push(src[i + offset]);
                    }
                    i += hashes + 1;
                    state = State::Code;
                    continue;
                }
                code.push(b);
                masked.push(if keep_line { b'\n' } else { b'_' });
            }
            State::Line => {
                if keep_line {
                    state = State::Code;
                    code.push(b);
                    masked.push(b);
                } else {
                    blank(&mut code, &mut masked, b);
                }
            }
            State::Block(depth) => {
                if b == b'*' && next == Some(b'/') {
                    state = if depth == 1 {
                        State::Code
                    } else {
                        State::Block(depth - 1)
                    };
                    blank(&mut code, &mut masked, b);
                    blank(&mut code, &mut masked, b'/');
                    i += 1;
                } else if b == b'/' && next == Some(b'*') {
                    state = State::Block(depth + 1);
                    blank(&mut code, &mut masked, b);
                    blank(&mut code, &mut masked, b'*');
                    i += 1;
                } else {
                    blank(&mut code, &mut masked, b);
                }
            }
        }
        i += 1;
    }
    (code, masked)
}

/// A comment byte: a space in both copies, except a newline, which is what keeps the lines.
fn blank(code: &mut Vec<u8>, masked: &mut Vec<u8>, byte: u8) {
    let blank = if byte == b'\n' { b'\n' } else { b' ' };
    code.push(blank);
    masked.push(blank);
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn find_all(hay: &[u8], needle: &str) -> Vec<usize> {
    let needle = needle.as_bytes();
    (0..hay.len().saturating_sub(needle.len() - 1))
        .filter(|&at| &hay[at..at + needle.len()] == needle)
        .collect()
}

/// The next `n` bytes of `code` from `at` that are not whitespace: `.env( "A" , "" )` and
/// `.env("A","")` are the same call.
fn squeezed(code: &[u8], at: usize, n: usize) -> String {
    code[at.min(code.len())..]
        .iter()
        .filter(|b| !b.is_ascii_whitespace())
        .take(n)
        .map(|b| *b as char)
        .collect()
}

fn line_of(code: &[u8], at: usize) -> usize {
    code[..at].iter().filter(|b| **b == b'\n').count() + 1
}

/// One place where a test removes `DATABASE_URL` from a child (`.env_remove("DATABASE_URL")` or
/// `.env_clear()`), and whether its statement also sets the switch.
struct Removal {
    line: usize,
    covered: bool,
}

/// Where a statement that holds `at` starts and ends. It starts at the nearest `Command::new`,
/// `;`, `{` or `}` before it, and ends at the first `.output()`, `.status()`, `.spawn()`, `;` or
/// closing brace after it: a chain, or the statement that configures a command built earlier.
fn statement(masked: &[u8], at: usize) -> (usize, usize) {
    let boundary = |b: &&u8| matches!(**b, b';' | b'{' | b'}');
    let after_boundary = masked[..at]
        .iter()
        .rposition(|b| boundary(&b))
        .map_or(0, |p| p + 1);
    let after_new = find_all(&masked[..at], "Command::new")
        .last()
        .copied()
        .unwrap_or(0);
    let start = after_boundary.max(after_new);

    let mut end = masked.len();
    let mut depth = 0_i32;
    for (offset, byte) in masked[at..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' if depth == 0 => {
                end = at + offset;
                break;
            }
            b'}' => depth -= 1,
            b';' if depth == 0 => {
                end = at + offset;
                break;
            }
            _ => {}
        }
    }
    for call in [".output()", ".status()", ".spawn()"] {
        if let Some(found) = find_all(&masked[at..end], call).first() {
            end = end.min(at + found + call.len());
        }
    }
    (start, end)
}

/// Whether `code[from..to]` holds `.env("SWITCH", <something that is not "">)`.
fn sets_the_switch(code: &[u8], masked: &[u8], from: usize, to: usize) -> bool {
    let head = format!("\"{SWITCH}\",");
    find_all(&masked[from..to], ".env(").into_iter().any(|at| {
        let open = from + at + ".env(".len();
        let call = squeezed(code, open, head.len() + 3);
        call.starts_with(&head) && !call.starts_with(&format!("{head}\"\")"))
    })
}

fn removals(text: &str) -> Vec<Removal> {
    let (code, masked) = lexed(text);
    let mut found = Vec::new();
    let mut hits: Vec<(usize, bool)> = find_all(&masked, ".env_remove(")
        .into_iter()
        .filter(|at| {
            squeezed(&code, at + ".env_remove(".len(), 16).starts_with("\"DATABASE_URL\")")
        })
        .map(|at| (at, false))
        .collect();
    hits.extend(
        find_all(&masked, ".env_clear()")
            .into_iter()
            .map(|at| (at, true)),
    );
    hits.sort_unstable();
    for (at, clears_everything) in hits {
        let (start, end) = statement(&masked, at);
        // `env_clear` wipes what was set before it in the chain, so only a switch after it counts.
        let from = if clears_everything { at } else { start };
        found.push(Removal {
            line: line_of(&code, at),
            covered: sets_the_switch(&code, &masked, from, end),
        });
    }
    found
}

/// The lines of `text` that take the switch away from a child or from the process: removing it,
/// clearing it with a scoped guard, or setting it to nothing.
fn undoes_the_switch(text: &str) -> Vec<usize> {
    let (code, masked) = lexed(text);
    let name = format!("\"{SWITCH}\"");
    let mut lines = Vec::new();
    for call in [
        ".env_remove(",
        "ScopedEnv::cleared(",
        "Env::cleared(",
        "remove_var(",
    ] {
        for at in find_all(&masked, call) {
            if squeezed(&code, at + call.len(), name.len() + 1).starts_with(&format!("{name})")) {
                lines.push(line_of(&code, at));
            }
        }
    }
    for at in find_all(&masked, ".env(") {
        if squeezed(&code, at + ".env(".len(), name.len() + 4).starts_with(&format!("{name},\"\")"))
        {
            lines.push(line_of(&code, at));
        }
    }
    // `Env::cleared(` is also the tail of `ScopedEnv::cleared(`: one line, once.
    lines.sort_unstable();
    lines.dedup();
    lines
}

/// What is wrong in `text`, as `(line, why)`.
fn scan(text: &str) -> Vec<(usize, String)> {
    let mut problems: Vec<(usize, String)> = removals(text)
        .into_iter()
        .filter(|removal| !removal.covered)
        .map(|removal| {
            (
                removal.line,
                format!("removes DATABASE_URL from a child and does not set {SWITCH}"),
            )
        })
        .collect();
    problems.extend(
        undoes_the_switch(text)
            .into_iter()
            .map(|line| (line, format!("takes {SWITCH} away"))),
    );
    problems.sort();
    problems
}

#[test]
fn the_scan_flags_each_way_a_test_can_forget_the_switch_and_only_those() {
    let set = format!(".env(\"{SWITCH}\", \"1\")");
    let cases: Vec<(&str, String, Vec<usize>)> = vec![
        (
            "a chain that removes the URL and forgets the switch",
            "fn f() {\n    Command::new(BIN)\n        .env_remove(\"DATABASE_URL\")\n        .output()\n        .unwrap();\n}\n".to_string(),
            vec![3],
        ),
        (
            "a chain that clears the environment and forgets the switch",
            "fn f() {\n    Command::new(BIN)\n        .env_clear()\n        .env(\"HOME\", h)\n        .output()\n        .unwrap();\n}\n".to_string(),
            vec![3],
        ),
        (
            "a chain that removes the URL and sets the switch",
            format!("fn f() {{\n    Command::new(BIN)\n        .env_remove(\"DATABASE_URL\")\n        {set}\n        .output()\n        .unwrap();\n}}\n"),
            vec![],
        ),
        (
            "the switch set before an env_clear that wipes it",
            format!("fn f() {{\n    Command::new(BIN)\n        {set}\n        .env_clear()\n        .output()\n        .unwrap();\n}}\n"),
            vec![4],
        ),
        (
            "the second of two chains, when only the first sets the switch",
            format!("fn f() {{\n    Command::new(a)\n        .env_remove(\"DATABASE_URL\")\n        {set}\n        .output()\n        .unwrap();\n    Command::new(b)\n        .env_remove(\"DATABASE_URL\")\n        .output()\n        .unwrap();\n}}\n"),
            vec![8],
        ),
        (
            "the second of two chains in tail position, with no `;`",
            format!("fn f() -> Output {{\n    if x {{\n        Command::new(a)\n            .env_remove(\"DATABASE_URL\")\n            {set}\n            .output()\n            .unwrap()\n    }} else {{\n        Command::new(b)\n            .env_remove(\"DATABASE_URL\")\n            .output()\n            .unwrap()\n    }}\n}}\n"),
            vec![10],
        ),
        (
            "a command configured in a statement after the one that built it",
            "fn f() -> Command {\n    let mut c = Command::new(BIN);\n    c.arg(\"x\")\n        .env_remove(\"DATABASE_URL\")\n        .stdin(Stdio::null());\n    c\n}\n".to_string(),
            vec![4],
        ),
        (
            "a shell snippet with a `;` inside the string does not cut the statement",
            format!("fn f() {{\n    Command::new(\"sh\")\n        {set}\n        .args([\"-c\", \". $1; exec sh\"])\n        .env_remove(\"DATABASE_URL\")\n        .output()\n        .unwrap();\n}}\n"),
            vec![],
        ),
        (
            "taking the switch away from a child",
            format!("fn f() {{\n    Command::new(BIN)\n        .env_remove(\"{SWITCH}\")\n        .output()\n        .unwrap();\n}}\n"),
            vec![3],
        ),
        (
            "clearing the switch with a scoped guard",
            format!("fn f() {{\n    let _a = ScopedEnv::cleared(\"{SWITCH}\");\n}}\n"),
            vec![2],
        ),
        (
            "clearing the switch with the other guard",
            format!("fn f() {{\n    let _a = Env::cleared(\"{SWITCH}\");\n}}\n"),
            vec![2],
        ),
        (
            "removing the switch from the process",
            format!("fn f() {{\n    unsafe {{ std::env::remove_var(\"{SWITCH}\") }};\n}}\n"),
            vec![2],
        ),
        (
            "setting the switch to nothing",
            format!("fn f() {{\n    Command::new(BIN)\n        .env(\"{SWITCH}\", \"\")\n        .output()\n        .unwrap();\n}}\n"),
            vec![3],
        ),
        (
            "a comment that names the removal",
            "fn f() {\n    // Command::new(BIN).env_remove(\"DATABASE_URL\").output();\n    /* .env_clear() */\n}\n".to_string(),
            vec![],
        ),
        (
            "a string that holds the removal as text",
            "const SNIPPET: &str = r#\"Command::new(BIN).env_remove(\"DATABASE_URL\").output()\"#;\nconst OTHER: &str = \".env_clear()\";\n".to_string(),
            vec![],
        ),
    ];

    for (what, text, expected) in cases {
        let lines: Vec<usize> = scan(&text).into_iter().map(|(line, _)| line).collect();
        assert_eq!(
            lines, expected,
            "{what}: the scan flagged lines {lines:?}, expected {expected:?}, in:\n{text}"
        );
    }
}

fn rust_files(dir: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs") {
            into.push(path);
        }
    }
}

#[test]
fn no_test_removes_the_database_url_from_a_child_without_setting_the_switch() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Not scanned: this file. `without_the_switch_the_default_path_still_tries_to_create_the_container`
    // clears the environment on purpose, with no switch, to watch the default path; and it is safe
    // because the only `docker` on that child's PATH is the stand-in.
    let this_file = Path::new(file!())
        .file_name()
        .expect("this file has a name")
        .to_owned();

    let mut files = Vec::new();
    rust_files(&manifest.join("src"), &mut files);
    rust_files(&manifest.join("tests"), &mut files);

    let mut chains: BTreeMap<String, usize> = BTreeMap::new();
    let mut problems: Vec<String> = Vec::new();
    for path in files {
        if path.file_name() == Some(this_file.as_os_str()) {
            continue;
        }
        let shown = path
            .strip_prefix(manifest.parent().expect("the repo root"))
            .unwrap_or(&path)
            .display()
            .to_string()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{shown} is readable: {e}"))
            .replace("\r\n", "\n");
        let seen = removals(&text).len();
        if seen > 0 {
            chains.insert(shown.clone(), seen);
        }
        problems.extend(
            scan(&text)
                .into_iter()
                .map(|(line, why)| format!("{shown}:{line}: {why}")),
        );
    }

    let total: usize = chains.values().sum();
    assert!(
        total >= 7,
        "the scan found {total} chains that remove DATABASE_URL or clear the environment, and \
         there are seven (accounts_cli, auto_client_cli, bridge_cli, apply_keeps, the two v045 \
         and the hook test in hooks_cli.rs). A scan that finds nothing passes anything: {chains:?}"
    );
    assert!(
        problems.is_empty(),
        "a child that has no DATABASE_URL and no {SWITCH} would, if it ever reached the \
         resolver, write to the developer's own database or start a PostgreSQL container. Set \
         `.env(\"{SWITCH}\", \"1\")` in the same chain (after `env_clear`, which would wipe \
         it), and never remove it:\n{}",
        problems.join("\n")
    );
}
