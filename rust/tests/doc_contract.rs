use std::collections::{BTreeMap, BTreeSet};

fn read(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        // Windows checkouts may store CRLF; doc scans match on `\n` / backticks.
        .replace("\r\n", "\n")
}

/// Drop markdown fenced blocks so ```bash\n./scripts/foo.sh``` is not mistaken
/// for an inline `` `bash\n./scripts/foo.sh` `` path when splitting on backticks.
fn without_fenced_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        out.push_str(&rest[..start]);
        rest = &rest[start + 3..];
        match rest.find("```") {
            Some(end) => rest = &rest[end + 3..],
            None => break,
        }
    }
    out.push_str(rest);
    out
}

fn number(word: &str) -> Option<usize> {
    word.trim_matches(|c: char| !c.is_ascii_digit())
        .parse::<usize>()
        .ok()
}

fn clean(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
}

fn is_bare_tools_word(word: &str) -> bool {
    clean(word) == "tools"
}

fn mentions_tools(word: &str) -> bool {
    let cleaned = clean(word);
    cleaned == "tools" || cleaned.ends_with("_tools")
}

fn counts_claimed(text: &str) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if !words.iter().any(|w| mentions_tools(w)) {
            continue;
        }
        for (i, word) in words.iter().enumerate() {
            if is_bare_tools_word(word) {
                for back in 1..=2 {
                    let Some(j) = i.checked_sub(back) else { break };
                    if let Some(n) = number(words[j]) {
                        found.insert(n);
                        break;
                    }
                }
                if words.get(i + 1).is_some_and(|w| *w == "of")
                    && let Some(n) = words.get(i + 2).and_then(|w| number(w))
                {
                    found.insert(n);
                }
            }
            if *word == "of"
                && i > 0
                && let Some(a) = number(words[i - 1])
                && let Some(b) = words.get(i + 1).and_then(|w| number(w))
            {
                found.insert(a);
                found.insert(b);
            }
        }
    }
    found
}

fn percents_claimed(text: &str) -> BTreeSet<u64> {
    let mut found = BTreeSet::new();
    for line in text.lines() {
        if !line.contains("catalogue") {
            continue;
        }
        for word in line.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '%');
            if let Some(digits) = cleaned.strip_suffix('%')
                && let Ok(n) = digits.parse::<u64>()
            {
                found.insert(n);
            }
        }
    }
    found
}

#[test]
fn every_tool_count_in_the_docs_is_one_the_code_can_produce() {
    let full_tools = memory_industry::constants::tools_for("full");
    let lean_tools = memory_industry::constants::tools_for("lean");
    let full = full_tools.len();
    let lean = lean_tools.len();

    assert!(
        full > lean && lean > 0,
        "the profiles have to differ for this contract to mean anything: full={full} lean={lean}"
    );

    let full_chars = serde_json::to_string(&full_tools).unwrap().len();
    let lean_chars = serde_json::to_string(&lean_tools).unwrap().len();
    let measured_pct = 100.0 * (full_chars - lean_chars) as f64 / full_chars as f64;

    let readme = read("README.md");
    let crate_readme = read("rust/README.md");
    let pyproject = read("pyproject.toml");
    let server_json = read("server.json");
    let cargo_toml = read("rust/Cargo.toml");
    let schemas = read("rust/src/constants.rs");
    let described = schemas
        .split("fn meta_tool_defs")
        .nth(1)
        .expect("meta_tool_defs is where the cuba_tools description lives");

    for (source, text) in [
        ("README.md", readme.as_str()),
        ("rust/README.md", crate_readme.as_str()),
        ("pyproject.toml", pyproject.as_str()),
        ("server.json", server_json.as_str()),
        ("rust/Cargo.toml", cargo_toml.as_str()),
        ("cuba_tools", described),
    ] {
        for n in counts_claimed(text) {
            assert!(
                n == full || n == lean,
                "{source} claims «{n} tools», but the code produces {full} in the full profile \
                 and {lean} in lean. Three different numbers were in circulation at once — the \
                 README said 28 in one place and 30 in two others while the cuba_tools \
                 description shipped to every model said 29 — because each was written by hand \
                 at a different time. It looks two words back, not one: the header says «28 MCP \
                 tools», and a scan that only checked the word touching «tools» found «MCP» and \
                 waved the number through. It also has to look past a backtick or an underscore: \
                 `cuba_tools` in the same sentence as «8 of 30» sailed through because neither \
                 word literally started with «tools». This assertion is the only thing that makes \
                 adding a tool and forgetting a document impossible."
            );
        }

        for p in percents_claimed(text) {
            let diff = (p as f64 - measured_pct).abs();
            assert!(
                diff <= 2.0,
                "{source} claims the lean catalogue is «{p}%» smaller, but serializing \
                 tools_for(\"full\") ({full_chars} chars) against tools_for(\"lean\") \
                 ({lean_chars} chars) measures {measured_pct:.1}%, off by {diff:.1} points — more \
                 than the ±2 tolerance a rounding difference would explain. A percentage next to \
                 the word «catalogue» is as much a publishable claim as the tool count next to it, \
                 and nothing checked it before: README.md carried 73% in one place and 71% in two \
                 others while the true figure, measured the same way, is {measured_pct:.0}%."
            );
        }
    }
}

#[test]
fn the_docs_do_not_promise_commands_the_binary_does_not_have() {
    let readme = read("README.md");
    let known: BTreeSet<&str> = memory_industry::cli::COMMANDS.iter().copied().collect();

    let mut promised = BTreeSet::new();
    let mut inside_fence = false;
    for line in readme.lines() {
        if line.trim_start().starts_with("```") {
            inside_fence = !inside_fence;
            continue;
        }
        let spans: Vec<&str> = if inside_fence {
            vec![line]
        } else {
            line.split('`').skip(1).step_by(2).collect()
        };
        for span in spans {
            for prefix in ["cuba-memorys ", "memory-industry "] {
                for rest in span.split(prefix).skip(1) {
                    let word = rest.split_whitespace().next().unwrap_or("");
                    if word.starts_with('-') {
                        continue;
                    }
                    let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
                    if !word.is_empty()
                        && word
                            .chars()
                            .all(|c: char| c.is_ascii_lowercase() || c == '-')
                    {
                        promised.insert(word.to_string());
                    }
                }
            }
        }
    }
    assert!(
        promised.len() > 3,
        "the extractor found almost no commands in the README, so a green result would mean \
         nothing. Found: {promised:?}"
    );

    let unknown: Vec<&String> = promised
        .iter()
        .filter(|w| !known.contains(w.as_str()))
        .collect();

    assert!(
        unknown.is_empty(),
        "the README tells the reader to run `cuba-memorys <x>` for commands that do not exist. \
         A copied-and-pasted line that errors out is how a reader decides the whole document is \
         stale. Known commands: {known:?}. Not found: {unknown:?}"
    );
}

const NOT_A_KNOB_AN_OPERATOR_SETS: [(&str, &str); 9] = [
    (
        "HOME",
        "the OS sets it; the code only reads it to locate ~/.cache",
    ),
    ("USERPROFILE", "the Windows spelling of HOME, same use"),
    (
        "HOSTNAME",
        "read only as the fallback for CUBA_NODE_NAME, which is documented",
    ),
    ("COMPUTERNAME", "the Windows spelling of HOSTNAME, same use"),
    (
        "PATH",
        "the OS search path; the code only asks whether the judge CLI is on it",
    ),
    (
        "LD_LIBRARY_PATH",
        "the dynamic loader's own variable, searched for libonnxruntime",
    ),
    (
        "LISTEN_PID",
        "systemd's socket-activation protocol: systemd sets it, an operator never does",
    ),
    (
        "LISTEN_FDS",
        "systemd's socket-activation protocol, same as LISTEN_PID",
    ),
    (
        "CUBA_RESOURCES_TEST_KNOB",
        "invented inside a #[test] in resources.rs; it does not exist at runtime",
    ),
];

fn looks_like_an_env_name(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_uppercase())
        && word
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn env_names_read_in(body: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    for marker in ["env::var(", "env::var_os("] {
        for call in body.split(marker).skip(1) {
            if let Some(literal) = call
                .trim_start()
                .strip_prefix('"')
                .and_then(|rest| rest.split('"').next())
                && looks_like_an_env_name(literal)
            {
                names.insert(literal.to_string());
            }
        }
    }

    for line in body.lines() {
        for literal in line.split('"').skip(1).step_by(2) {
            if (literal.starts_with("CUBA_") || literal.starts_with("MEMORY_INDUSTRY_"))
                && looks_like_an_env_name(literal)
            {
                names.insert(literal.to_string());
            }
        }
    }

    if body.contains("from_default_env(") {
        names.insert("RUST_LOG".to_string());
    }

    names
}

fn env_names_the_code_reads() -> BTreeMap<String, BTreeSet<String>> {
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src/ is readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("readable");
            let file = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            for name in env_names_read_in(&body) {
                found.entry(name).or_default().insert(file.clone());
            }
        }
    }
    found
}

#[test]
fn every_environment_variable_the_code_reads_is_in_the_readme() {
    let read_by_code = env_names_the_code_reads();

    assert!(
        read_by_code.len() >= 60,
        "the scanner found {} environment variables in src/, and there are around 70. A green \
         result from a scanner that found almost nothing proves nothing at all",
        read_by_code.len()
    );
    for anchor in ["DATABASE_URL", "CUBA_DOCS"] {
        assert!(
            read_by_code.contains_key(anchor),
            "{anchor} is read by the code and the scanner missed it, so the scanner is broken. \
             These two anchor the two halves: DATABASE_URL is read through env::var(\"…\") \
             directly, CUBA_DOCS only ever reaches env::var as an argument to mode::env_toggle, \
             and a scanner that only looked for the first would silently skip every variable \
             read through a helper or a constant"
        );
    }

    let readme = read("README.md");
    let documented: BTreeSet<&str> = readme
        .split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
        .filter(|w| looks_like_an_env_name(w))
        .collect();
    assert!(
        documented.contains("CUBA_MODE"),
        "the README parser did not even find CUBA_MODE, the first row of the configuration \
         table, so it is the parser that is broken and not the document"
    );

    let excluded: BTreeMap<&str, &str> = NOT_A_KNOB_AN_OPERATOR_SETS.into_iter().collect();
    for (name, reason) in &excluded {
        assert!(
            read_by_code.contains_key(*name),
            "{name} is excluded from this contract because {reason}, but nothing in src/ reads \
             it any more. An exclusion that outlives its variable is a hole nobody can audit: \
             drop the entry from NOT_A_KNOB_AN_OPERATOR_SETS"
        );
    }

    let undocumented: Vec<String> = read_by_code
        .iter()
        .filter(|(name, _)| !documented.contains(name.as_str()))
        .filter(|(name, _)| !excluded.contains_key(name.as_str()))
        .map(|(name, files)| format!("{name} (read in {files:?})"))
        .collect();

    assert!(
        undocumented.is_empty(),
        "these variables change what the program does and the README never mentions them, so \
         nobody outside this file knows they exist. That is not hypothetical here: \
         CUBA_AUDIT_KEY decided whether the audit chain could be forged and was documented \
         nowhere, so every operator ran with a chain anyone could recompute. Either add a row \
         to the configuration table in README.md or, if the variable is not something an \
         operator sets, add it to NOT_A_KNOB_AN_OPERATOR_SETS with the reason. Missing: \
         {undocumented:#?}"
    );
}

#[test]
fn a_credential_compiled_into_the_binary_is_disclosed_in_security_md() {
    let security = read("SECURITY.md");
    let mut compiled: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src/ is readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let body = std::fs::read_to_string(&path).expect("readable");
            for line in body.lines() {
                let upper = line.to_uppercase();
                if !upper.contains("PASSWORD") && !upper.contains("PASSWD") {
                    continue;
                }
                for literal in line.split('"').skip(1).step_by(2) {
                    if literal.len() >= 6
                        && literal.chars().any(|c| c.is_ascii_digit())
                        && !literal.contains(' ')
                        && !literal.contains("://")
                        && !literal.contains('{')
                    {
                        compiled.insert(literal.to_string());
                    }
                }
            }
        }
    }

    assert!(
        !compiled.is_empty(),
        "the scan found no compiled credential at all. That is either very good news or a \
         broken scan, and today it is the second: setup.rs carries the container defaults. A \
         check that cannot find what it knows is there proves nothing about what it cannot see"
    );

    let undisclosed: Vec<&String> = compiled
        .iter()
        .filter(|literal| !security.contains(literal.as_str()))
        .collect();

    assert!(
        undisclosed.is_empty(),
        "SECURITY.md is explicit that the fallback container password and the app-role password \
         are compiled into setup.rs, and naming them is what lets an operator decide to rotate \
         them. A credential that gets compiled in without appearing there is one nobody knows to \
         rotate — and an earlier version of this very test asserted the opposite claim, that no \
         credentials are compiled in, then passed three times in a row without ever running its \
         body because SECURITY.md never contained the phrase it was looking for. Undisclosed: \
         {undisclosed:?}"
    );
}

fn variables_offered_in_env_example() -> BTreeSet<String> {
    read(".env.example")
        .lines()
        .filter_map(|line| {
            line.trim_start()
                .trim_start_matches('#')
                .trim()
                .split('=')
                .next()
        })
        .filter(|word| looks_like_an_env_name(word))
        .map(str::to_string)
        .collect()
}

#[test]
fn every_variable_offered_in_env_example_is_one_something_still_reads() {
    let offered = variables_offered_in_env_example();
    assert!(
        offered.len() >= 12,
        "the parser found {} variables in .env.example and the file lists more than a dozen. A \
         green result from a parser that found almost nothing proves nothing",
        offered.len()
    );

    let read_by_code = env_names_the_code_reads();
    let mut read_by_scripts = String::new();
    for entry in std::fs::read_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("scripts"),
    )
    .expect("scripts/ is readable")
    .flatten()
    {
        read_by_scripts.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
    }
    read_by_scripts.push_str(&read("docker-compose.yml"));

    let dead: Vec<&String> = offered
        .iter()
        .filter(|name| {
            !read_by_code.contains_key(*name) && !read_by_scripts.contains(name.as_str())
        })
        .collect();

    assert!(
        dead.is_empty(),
        ".env.example is what an operator copies to .env, so every line in it is a promise that \
         setting the variable changes something. These are read by nothing: the code that used \
         them is gone and the offer stayed. It happened with ANTHROPIC_API_KEY, which survived \
         the deletion of the anthropic-api feature in 0.23.0 because the sibling contract only \
         checks the other direction — that what the code reads is documented — and a variable \
         nothing reads is invisible to it. Dead: {dead:?}"
    );
}

#[test]
fn the_gate_does_not_run_its_provisioning_inside_the_database_container() {
    let gate = read("scripts/run-all-tests.sh");
    let offenders: Vec<&str> = gate
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| !line.contains(">&2"))
        .filter(|line| {
            line.contains("docker exec")
                && ["psql", "pg_dump", "pg_restore", "pg_isready"]
                    .iter()
                    .any(|tool| line.contains(tool))
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "a process started inside the postgres container is adopted by the postmaster the moment \
         its docker exec is interrupted, and when an adopted child exits non-zero the postmaster \
         reads it as a crashed backend and restarts the whole cluster. Measured on 2026-08-14 \
         against a throwaway pg18: an orphan exiting 2 produced `untracked child process exited \
         with exit code 2` followed by `terminating any other active server processes` and a full \
         recovery; the same orphan exiting 0 produced only a log line. That is what took the live \
         brain database into recovery mid-gate. The gate reaches the server over TCP with the \
         host's psql instead, which cannot be adopted by anything. Offending lines: {offenders:#?}"
    );
}

#[test]
fn the_gate_writes_its_own_exit_code_where_a_reader_can_find_it() {
    let gate = read("scripts/run-all-tests.sh");
    assert!(
        gate.contains("local code=$?") && gate.contains("echo \"$code\" > \"$EXIT_FILE\""),
        "the gate has to record its own exit code, captured before the cleanup can overwrite \
         $?, and outside /tmp because /tmp is swept on reboot. A 20-minute gate is launched in \
         the background, and whatever the wrapper reports is the exit code of the last command \
         in the chain, not of the gate: that is how GATE_EXIT=101 was once read as green. \
         Leaving this to the caller does not work either — on 2026-08-16 the gate was launched \
         three times with nohup and no capture, and each time the result was read from a file \
         that was never going to appear"
    );
    let trap = gate
        .find("trap on_exit EXIT")
        .expect("the gate installs its exit trap");
    let first_step = gate
        .find("=== cargo fmt --check ===")
        .expect("the gate starts by checking formatting");
    assert!(
        trap < first_step,
        "the trap has to be armed before the first step that can fail, or a failure in fmt or \
         clippy leaves no exit code behind and the run looks like it never happened"
    );
}

#[test]
fn the_gate_creates_the_database_it_points_the_unit_tests_at_before_running_them() {
    let gate = read("scripts/run-all-tests.sh");
    let body = gate
        .split_once("trap on_exit EXIT")
        .expect("the gate provisions a throwaway database and drops it on exit")
        .1;
    let provision = body
        .find("\nprovision_gate_db")
        .expect("the gate calls provision_gate_db");
    let unit_tests = body
        .find("DATABASE_URL=\"$GATE_DATABASE_URL\" cargo test")
        .expect("the gate runs the unit tests against the throwaway database");

    assert!(
        provision < unit_tests,
        "the gate pointed `cargo test` at $GATE_DATABASE_URL and only created that database on \
         the next line. It passed for months because no unit test in src/ ever opened a real \
         connection — every one of them builds a pool that cannot connect on purpose. The first \
         one that did open a connection died with `database \"brain_gate\" does not exist`, which \
         reads like a broken test rather than a gate that runs its steps out of order. Provision \
         first: a step that needs a database it was told to use must not be the step that proves \
         it was missing"
    );
}

#[test]
fn the_gate_refuses_to_start_when_the_disk_cannot_hold_the_link() {
    let gate = read("scripts/run-all-tests.sh");
    assert!(
        gate.contains("-lt \"$MIN_FREE_GB\"")
            && gate.contains("df --output=avail")
            && gate.contains("exit 1"),
        "the gate has to check free disk before anything compiles. Measured on 2026-08-15: a \
         run failed with `collect2: fatal error: ld terminated with signal 7 [Bus error]` and \
         three test binaries reported as «could not compile», on a partition at 98% where \
         target/ alone held 64 GB. Nothing in that output mentions disk, so it reads as a code \
         failure and costs an hour of looking for a bug that is not there"
    );
    assert!(
        gate.contains("-mtime \"+$SWEEP_OLDER_THAN_DAYS\"") && gate.contains("-delete"),
        "and it has to sweep what nothing uses any more. cargo never removes the binaries of \
         earlier compilations — every edit produces a new hash and the old one stays — so \
         target/debug/deps grows without bound. Sweeping by AGE and not by size is what makes \
         it safe: an artifact this run still needs was written today"
    );
}

#[test]
fn every_repo_file_the_docs_point_at_is_a_file_that_exists() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut promised: BTreeSet<(&str, String)> = BTreeSet::new();

    for source in [
        "README.md",
        "SECURITY.md",
        "CONTRIBUTING.md",
        "rust/README.md",
    ] {
        let text = without_fenced_code(&read(source));
        for span in text.split('`').skip(1).step_by(2) {
            let path = span.trim();
            let looks_like_a_repo_file = path.contains('/')
                && !path.contains(' ')
                && !path.contains('\n')
                && !path.starts_with('/')
                && !path.starts_with("http")
                && !path.contains('$')
                && !path.contains('*')
                && [".rs", ".sql", ".sh", ".toml", ".py", ".json", ".yml", ".md"]
                    .iter()
                    .any(|ext| path.ends_with(ext));
            if looks_like_a_repo_file {
                promised.insert((source, path.to_string()));
            }
        }
    }

    assert!(
        promised.len() >= 3,
        "the scan found almost no repo paths in the docs, so a green result would prove \
         nothing. Found: {promised:?}"
    );

    let missing: Vec<&(&str, String)> = promised
        .iter()
        .filter(|(_, path)| !root.join(path).exists() && !root.join("rust").join(path).exists())
        .collect();

    assert!(
        missing.is_empty(),
        "a document points the reader at a file that is not there: {missing:?}. This is what \
         moving a file looks like from the outside — on 2026-08-16 secure_role.sql moved to \
         scripts/create-app-role.sql so that two frozen migration comments would stop lying, \
         and the move left SECURITY.md naming the old path twice. Nothing caught it: the \
         crate still compiled and every test stayed green, because a stale path in prose only \
         fails for the person who types it"
    );
}
