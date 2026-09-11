use std::process::Command;

const DEAD_DB: &str = "postgresql://nobody@127.0.0.1:1/nothing";

fn run(args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_cuba-memorys"))
        .args(args)
        .env("DATABASE_URL", DEAD_DB)
        .output()
        .expect("binary runs");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn version_is_inert_and_matches_the_crate() {
    let (stdout, stderr, code) = run(&["--version"]);

    assert_eq!(
        code, 0,
        "--version must exit 0, got {code}\nstderr: {stderr}"
    );
    assert_eq!(
        stdout.trim(),
        format!("memory-industry {}", env!("CARGO_PKG_VERSION")),
        "--version must print exactly the crate version on stdout"
    );
    assert!(
        !stderr.contains("connected to PostgreSQL") && !stderr.contains("migrations"),
        "--version must not touch the database — it ran migrations before this test existed.\nstderr: {stderr}"
    );

    for alias in ["-V", "version"] {
        let (out, _, code) = run(&[alias]);
        assert_eq!(code, 0, "`{alias}` must work too");
        assert!(
            out.contains(env!("CARGO_PKG_VERSION")),
            "`{alias}` prints the version"
        );
    }
}

#[test]
fn help_documents_the_command_surface() {
    let (stdout, _, code) = run(&["--help"]);
    assert_eq!(code, 0, "--help must exit 0");

    for cmd in memory_industry::cli::COMMANDS {
        assert!(
            stdout.contains(cmd),
            "--help must document `{cmd}`. This used to check a hand-written list of 13 \
             names frozen at the time it was written, so models, secure, serve, tunnel, \
             sync, hook, codegraph, rem and dedupe could all have vanished from the help \
             with the test still green. It now walks the same list the dispatcher's error \
             message prints"
        );
    }

    let (short, _, code) = run(&["-h"]);
    assert_eq!(code, 0);
    assert_eq!(short, stdout, "-h and --help must agree");
}

#[test]
fn an_unknown_argument_is_an_error_not_a_server_launch() {
    for typo in ["doctro", "--verison", "sarch"] {
        let (_, stderr, code) = run(&[typo]);
        assert_eq!(
            code, 2,
            "`{typo}` must exit 2 (usage error), not start the MCP server"
        );
        assert!(
            stderr.contains(typo),
            "the error must name the offending argument, so the typo is obvious"
        );
    }
}

fn pyproject_version(src: &str) -> Option<String> {
    let mut in_project = false;
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_project = t == "[project]";
            continue;
        }
        if !in_project {
            continue;
        }
        if let Some(v) = t
            .strip_prefix("version")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
        {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

#[test]
fn every_file_that_holds_a_version_agrees() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root");
    let cargo = env!("CARGO_PKG_VERSION");

    let read = |name: &str| {
        std::fs::read_to_string(root.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let json = |name: &str| -> serde_json::Value {
        serde_json::from_str(&read(name)).unwrap_or_else(|e| panic!("{name} must parse: {e}"))
    };

    let pkg = json("package.json");
    let npm = pkg["version"].as_str().expect("package.json has a version");
    assert_eq!(
        npm, cargo,
        "package.json ({npm}) vs Cargo.toml ({cargo}): npm's postinstall downloads from \
         releases/download/v{npm}/, an asset the release workflow only builds for the Cargo version"
    );

    let py =
        pyproject_version(&read("pyproject.toml")).expect("pyproject.toml has a [project] version");
    let (minor, patch) = {
        let p: Vec<&str> = cargo.split('.').collect();
        (p[1].parse::<u32>().expect("cargo minor"), p[2].to_string())
    };
    let expected_py = format!("1.{}.{}", minor + 2, patch);
    assert_eq!(
        py, expected_py,
        "pyproject.toml ({py}) must follow 1.{{minor+2}}.{{patch}} for Cargo {cargo}"
    );

    let srv = json("server.json");
    let srv_version = srv["version"].as_str().expect("server.json has a version");
    assert_eq!(
        srv_version, cargo,
        "server.json ({srv_version}) vs Cargo.toml ({cargo}) — this is what the MCP Registry \
         publishes; stale here means the registry advertises the previous release"
    );

    for p in srv["packages"]
        .as_array()
        .expect("server.json has packages")
    {
        let registry = p["registryType"]
            .as_str()
            .or_else(|| p["registry_name"].as_str())
            .expect("package has a registry");
        let got = p["version"].as_str().expect("package has a version");
        let want = match registry {
            "npm" => npm,
            "pypi" => &expected_py,
            other => panic!(
                "server.json declares an unknown registry `{other}` — teach this test about it"
            ),
        };
        assert_eq!(
            got, want,
            "server.json's {registry} entry says {got}, but {registry} will receive {want}"
        );
    }
}

#[test]
fn every_listed_command_is_actually_dispatched() {
    for cmd in memory_industry::cli::COMMANDS {
        let (_, stderr, code) = run(&[cmd, "--help"]);
        assert_ne!(
            code, 2,
            "`{cmd}` is in COMMANDS but the dispatcher does not know it: {stderr}"
        );
        assert!(
            !stderr.contains("unknown command"),
            "`{cmd}` reached the unknown-command arm: {stderr}"
        );
    }
}

#[test]
fn the_readme_counts_the_commands_that_exist() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root");
    let readme = std::fs::read_to_string(root.join("README.md")).expect("README.md");

    let claimed = readme
        .split_whitespace()
        .zip(readme.split_whitespace().skip(1))
        .find_map(|(n, word)| {
            word.starts_with("CLI")
                .then(|| {
                    n.trim_matches(|c: char| !c.is_ascii_digit())
                        .parse::<usize>()
                        .ok()
                })
                .flatten()
        })
        .expect("README must state how many CLI commands there are");

    assert_eq!(
        claimed,
        memory_industry::cli::COMMANDS.len(),
        "the README says {claimed} CLI commands and the binary dispatches {}. A count in \
         prose is a claim like any other, and this one drifted through three different \
         numbers — 19 in the README, 20 in --help, 22 dispatched — before anything checked it",
        memory_industry::cli::COMMANDS.len()
    );
}

#[test]
fn nothing_can_exit_a_draining_command_without_draining_first() {
    // Windows checkouts may store CRLF; the structural scan below matches on `\n`.
    let main_rs = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("src/main.rs")
    .replace("\r\n", "\n");

    let helper = main_rs
        .split_once("async fn drain_then_report")
        .expect("the draining commands must funnel through drain_then_report")
        .1;
    let body = helper.split_once("\n}\n").expect("helper body").0;

    let drain = body
        .find("drain_background_tasks()")
        .expect("the helper must drain");
    let exit = body
        .find("std::process::exit")
        .expect("the helper must exit");
    assert!(
        drain < exit,
        "the drain has to happen BEFORE the exit. It used to sit after it inside each arm, \
         so a `save` that returned an error killed the process with its embedding still in \
         flight — the write survived, the vector did not, and nothing said so"
    );

    let mut walked: Vec<String> = Vec::new();
    let mut offenders = Vec::new();
    for arm in main_rs.split("        Some(").skip(1) {
        let Some((head, rest)) = arm.split_once("=>") else {
            continue;
        };
        let named: Vec<&str> = head
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|c| memory_industry::cli::COMMANDS.contains(c))
            .collect();
        if named.is_empty() {
            continue;
        }
        let command = named.join("|");
        let body = rest.split_once("\n        }").map_or(rest, |(b, _)| b);
        let reaches_the_dispatcher = body.contains("handlers::")
            || body.contains("http::serve")
            || body.contains("protocol::run_mcp")
            || body.contains("drain_then_report");
        if !reaches_the_dispatcher {
            continue;
        }
        walked.push(command.clone());
        let Some(exit) = body.find("std::process::exit") else {
            continue;
        };
        match body.find("drain_background_tasks()") {
            Some(drain) if drain < exit => {}
            _ => offenders.push(command.clone()),
        }
    }

    assert!(
        walked.iter().any(|c| c == "serve"),
        "the scan never reached the `serve` arm, and that is the one this check was widened \
         for: the daemon exited on error without draining while the commit that wrote this \
         contract claimed every command had been fixed. Walked: {walked:?}"
    );
    assert!(
        walked.iter().any(|c| c.contains("save")),
        "and it has to see the combined arm too — search|save|delete|export|dashboard live in \
         one `Some(cmd @ (...))` pattern, which the first version of this scan could not read \
         at all. Walked: {walked:?}"
    );
    assert!(
        offenders.is_empty(),
        "these arms can exit without draining first: {offenders:?}. Every write queues its \
         embedding through tasks::spawn, so an exit that skips the drain leaves the row saved \
         and its vector never computed — the search stops finding it and nothing says so. \
         This check used to name two commands by hand, `reembed` and `rem`, and `serve` was \
         not one of them: the daemon exited on error without draining while the commit that \
         wrote this contract claimed every command had been fixed. A list maintained by hand \
         only ever covers what somebody remembered"
    );
}
