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

const NOT_A_KNOB_AN_OPERATOR_SETS: [(&str, &str); 11] = [
    (
        "HOME",
        "the OS sets it; the code only reads it to locate ~/.cache",
    ),
    ("USERPROFILE", "the Windows spelling of HOME, same use"),
    (
        "LOCALAPPDATA",
        "Windows sets it, like USERPROFILE; the code only reads it to locate \
         %LOCALAPPDATA%\\MemoryIndustry, and refuses to install rather than guess a \
         directory when it is missing",
    ),
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
    (
        "MEMORY_INDUSTRY_EMBED_DEVICE",
        "nothing reads it: preferred_device_var() promoted only the reranker to the \
         MEMORY_INDUSTRY_ namespace. The name reaches this scanner because the device table \
         in gpu.rs names it to assert exactly that, and documenting it on that evidence would \
         publish a knob an operator can set with no effect and no way to find out why. The day \
         preferred_device_var() returns it, drop this row and give it a README row and an \
         .env.example line instead",
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
            // Only production lines: a fixture inside a `#[cfg(test)]` region is
            // never compiled into the binary, so it is not a credential anyone has
            // to rotate. On 2026-09-23 a test table in secure_cli.rs
            // (`for password in ["9f8e…", …]`) turned this contract red.
            // Regions are skipped with validar-handoff.sh's three-state machine on
            // column-0 lines, not by cutting at the first attribute: http.rs and
            // session.rs put a `#[cfg(test)]` on a loose helper and carry
            // production after it (1200 lines of http.rs), which a cut would hide.
            let mut state = 0u8;
            for line in body.lines() {
                let test_line = match state {
                    0 if line == "#[cfg(test)]" => {
                        state = 1;
                        true
                    }
                    0 => false,
                    1 => {
                        state = if line.trim_end().ends_with(';') { 0 } else { 2 };
                        true
                    }
                    _ => {
                        if line == "}" {
                            state = 0;
                        }
                        true
                    }
                };
                if test_line {
                    continue;
                }
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
        compiled.contains("memorys2026"),
        "the scan did not find `memorys2026`, the fallback container password setup.rs compiles \
         in as PG_PASSWORD (SECURITY.md names it). Either the scan is broken or skipping \
         #[cfg(test)] regions is swallowing production code, and a check that cannot find what \
         it knows is there proves nothing about what it cannot see. Found: {compiled:?}"
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
    env_names_offered_in(&read(".env.example"))
}

/// Every `NAME=` an env file offers, set or commented out.
fn env_names_offered_in(body: &str) -> BTreeSet<String> {
    body.lines()
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
    let lock = read("scripts/gate-lock.sh");
    assert!(
        lock.contains("GATE_EXIT_FILE=\"${CUBA_GATE_EXIT_FILE:-$HOME/.cache/cuba-gate/run.exit}\""),
        "the exit file lives outside /tmp, because /tmp is swept on reboot"
    );
    // Both scripts that can hold the lock record the code they exit with,
    // captured before the cleanup can overwrite $?. A 20-minute gate is
    // launched in the background, and whatever the wrapper reports is the exit
    // code of the last command in the chain, not of the gate: that is how
    // GATE_EXIT=101 was once read as green. Leaving this to the caller does not
    // work either — on 2026-08-16 the gate was launched three times with nohup
    // and no capture, and each time the result was read from a file that was
    // never going to appear. Since 0.28 merge-gate.sh writes the verdict, so
    // that a deny or mutation failure after the tests is not reported as 0.
    for (script, first_step) in [
        ("scripts/run-all-tests.sh", "=== cargo fmt --check ==="),
        (
            "scripts/merge-gate.sh",
            "\"$ROOT/scripts/run-all-tests.sh\"\n",
        ),
    ] {
        let gate = read(script);
        assert!(
            gate.contains("local code=$?") && gate.contains("exit_file_verdict \"$code\""),
            "{script} has to record the code it exits with, captured before its cleanup"
        );
        let trap = gate
            .find("trap on_exit EXIT")
            .unwrap_or_else(|| panic!("{script} installs its exit trap"));
        let step = gate
            .rfind(first_step)
            .unwrap_or_else(|| panic!("{script} runs {first_step}"));
        assert!(
            trap < step,
            "{script}: the trap has to be armed before the first step that can fail, or a \
             failure there leaves no exit code behind and the run looks like it never happened"
        );
    }
}

const UNIT_TESTS_ON_THE_GATE_DB: &str = "DATABASE_URL=\"$GATE_DATABASE_URL\" cargo test";

/// A line that runs `provision_gate_db`, as opposed to the one that defines it
/// (`provision_gate_db() {`) or a comment that names it.
fn calls_provision_gate_db(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("provision_gate_db") && !line.contains("()")
}

fn runs_the_unit_tests_on_the_gate_db(line: &str) -> bool {
    let line = line.trim_start();
    !line.starts_with('#') && line.contains(UNIT_TESTS_ON_THE_GATE_DB)
}

/// Line-based, not a substring search. The first version split the script at
/// `trap on_exit EXIT` and looked for `\nprovision_gate_db` after it, which
/// only found the call while the trap sat between the definition and the call.
/// db8490d moved the trap above the definition, the search started landing on
/// `provision_gate_db() {`, and the contract stayed green over a script whose
/// call it had stopped reading.
fn provisioning_precedes_the_unit_tests(gate: &str) -> Result<(), String> {
    let lines: Vec<&str> = gate.lines().collect();
    let call = lines
        .iter()
        .position(|l| calls_provision_gate_db(l))
        .ok_or("the gate never calls provision_gate_db")?;
    let unit_tests = lines
        .iter()
        .position(|l| runs_the_unit_tests_on_the_gate_db(l))
        .ok_or("the gate never runs `cargo test` against $GATE_DATABASE_URL")?;
    if call < unit_tests {
        Ok(())
    } else {
        Err(format!(
            "provision_gate_db is called on line {} and the unit tests run against its database \
             on line {}",
            call + 1,
            unit_tests + 1
        ))
    }
}

#[test]
fn the_gate_creates_the_database_it_points_the_unit_tests_at_before_running_them() {
    let gate = read("scripts/run-all-tests.sh");
    if let Err(why) = provisioning_precedes_the_unit_tests(&gate) {
        panic!(
            "{why}. The gate pointed `cargo test` at $GATE_DATABASE_URL and only created that \
             database on the next line. It passed for months because no unit test in src/ ever \
             opened a real connection — every one of them builds a pool that cannot connect on \
             purpose. The first one that did open a connection died with `database \
             \"brain_gate\" does not exist`, which reads like a broken test rather than a gate \
             that runs its steps out of order. Provision first: a step that needs a database it \
             was told to use must not be the step that proves it was missing"
        );
    }
}

#[test]
fn the_provisioning_contract_rejects_the_call_moved_behind_the_unit_tests() {
    let gate = read("scripts/run-all-tests.sh");
    let mut lines: Vec<&str> = gate.lines().collect();
    let call = lines
        .iter()
        .position(|l| calls_provision_gate_db(l))
        .expect("the gate calls provision_gate_db, so there is a call to move");
    let moved_call = lines.remove(call);
    let unit_tests = lines
        .iter()
        .position(|l| runs_the_unit_tests_on_the_gate_db(l))
        .expect("the gate runs the unit tests against the throwaway database");
    lines.insert(unit_tests + 1, moved_call);
    let sabotaged = lines.join("\n");

    let definition = sabotaged
        .find("\nprovision_gate_db() {")
        .expect("the sabotaged script still defines provision_gate_db");
    let unit_tests_at = sabotaged
        .find(UNIT_TESTS_ON_THE_GATE_DB)
        .expect("the sabotaged script still runs the unit tests");
    assert!(
        definition < unit_tests_at,
        "this sabotage is only the one the old substring search missed if the definition still \
         sits above `cargo test`, where that search found it and took it for the call"
    );
    assert!(
        provisioning_precedes_the_unit_tests(&sabotaged).is_err(),
        "the call to provision_gate_db was moved behind `cargo test` and the contract still \
         accepted the script, so it is reading the definition, a comment or nothing at all. A \
         contract that cannot see the one move it exists to catch is a paragraph"
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

/// Files whose variables somebody must set to stand a daemon up: the HTTP
/// surface, the GPU placement, the mode and the resource plan.
///
/// This is derived from where a variable is *read*, never from a hand-kept
/// list, because a hand-kept list is exactly what drifts. That is not a
/// hypothetical: a 0.25.0 deployment onto a GPU machine had to read `rust/src`
/// to discover `CUBA_RERANK_DEVICE` and `CUBA_GPU_MEM_LIMIT_MB`, because the
/// README documents them and `.env.example` — the file an operator actually
/// copies — never mentioned them.
const DEPLOYMENT_SURFACE_FILES: [&str; 4] = ["http.rs", "gpu.rs", "mode.rs", "resources.rs"];

fn deployment_surface() -> BTreeSet<String> {
    let excluded: BTreeMap<&str, &str> = NOT_A_KNOB_AN_OPERATOR_SETS.into_iter().collect();
    env_names_the_code_reads()
        .into_iter()
        .filter(|(name, _)| !excluded.contains_key(name.as_str()))
        .filter(|(_, files)| {
            files
                .iter()
                .any(|f| DEPLOYMENT_SURFACE_FILES.contains(&f.as_str()))
        })
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn every_variable_on_the_deployment_surface_is_offered_in_env_example() {
    let surface = deployment_surface();

    // Anchor before absence: a scanner that found nothing would pass this test
    // while proving nothing at all.
    for anchor in ["CUBA_HTTP_TOKEN", "CUBA_GPU_MEM_LIMIT_MB"] {
        assert!(
            surface.contains(anchor),
            "{anchor} is read on the deployment surface and the scan missed it, so it is the \
             scan that is broken and not the file. Found: {surface:?}"
        );
    }

    let offered = variables_offered_in_env_example();
    let missing: Vec<&String> = surface.difference(&offered).collect();

    assert!(
        missing.is_empty(),
        "these variables decide how the daemon binds, which device each model lands on and how \
         much VRAM the arena may take, and `.env.example` does not offer them: {missing:?}. The \
         README is not a substitute: it is 56 KB of reference and nobody opens it to start a \
         service. Whoever installs this next will do what the last operator did — read the \
         source — unless the file they copy lists the knob."
    );
}

/// Names a unit pins in line with `Environment=`, in any of the forms systemd
/// accepts: `Environment=A=1`, `Environment="A=1" B=2`.
fn names_pinned_in_line(unit: &str) -> BTreeSet<String> {
    unit.lines()
        .filter_map(|line| line.trim().strip_prefix("Environment="))
        .flat_map(str::split_whitespace)
        .filter_map(|assignment| assignment.trim_matches('"').split('=').next())
        .filter(|name| looks_like_an_env_name(name))
        .map(str::to_string)
        .collect()
}

/// What a unit pins in line that the operator is meant to set in the env
/// file instead.
fn operator_knobs_pinned_in_line(unit: &str, env_file: &BTreeSet<String>) -> Vec<String> {
    names_pinned_in_line(unit)
        .into_iter()
        .filter(|name| {
            name.starts_with("CUBA_")
                || name.starts_with("MEMORY_INDUSTRY_")
                || ["DATABASE_URL", "ORT_DYLIB_PATH"].contains(&name.as_str())
                || env_file.contains(name)
        })
        .collect()
}

const UNIT_ENV_FILE: &str = "packaging/memory-industry.env.example";

#[test]
fn every_unit_reads_its_knobs_from_the_env_file_and_pins_none_in_line() {
    let env_file = env_names_offered_in(&read(UNIT_ENV_FILE));
    assert!(
        env_file.contains("CUBA_HTTP_TOKEN") && env_file.contains("DATABASE_URL"),
        "the parser found {env_file:?} in {UNIT_ENV_FILE}, which offers both CUBA_HTTP_TOKEN and          DATABASE_URL. It is the parser that is broken, not the file"
    );

    let mut units_seen = 0usize;
    let mut with_env_file = 0usize;
    let mut pinned: Vec<String> = Vec::new();
    let packaging = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packaging");
    for entry in std::fs::read_dir(&packaging)
        .expect("packaging/ is readable")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "service") {
            continue;
        }
        units_seen += 1;
        let name = entry.file_name().to_string_lossy().into_owned();
        let unit = std::fs::read_to_string(&path).expect("readable");
        if unit
            .lines()
            .any(|l| l.trim().starts_with("EnvironmentFile="))
        {
            with_env_file += 1;
        } else {
            pinned.push(format!("packaging/{name}: no EnvironmentFile= line"));
        }
        for knob in operator_knobs_pinned_in_line(&unit, &env_file) {
            pinned.push(format!("packaging/{name}: Environment={knob}"));
        }
    }

    assert!(
        units_seen > 0 && with_env_file > 0,
        "the parser read {units_seen} .service file(s) under packaging/ and saw EnvironmentFile=          in {with_env_file}. packaging/memory-industry.service ships and declares one, so it is          the parser that is broken, not the units"
    );
    assert!(
        pinned.is_empty(),
        "a unit under packaging/ sets in line what the operator sets in the env file:          {pinned:#?}. Two deployment artifacts that disagree are worse than one: the unit          silently wins on Linux while the operator edits a file that has no effect. It happened:          packaging/cuba-memorys.service pinned a hand-written GPU ceiling that beat the          planner's measurement on every card the daemon ran on. The knob goes in {UNIT_ENV_FILE}"
    );
}

#[test]
fn the_unit_contract_rejects_a_knob_pinned_in_line() {
    let env_file = env_names_offered_in(&read(UNIT_ENV_FILE));
    let unit = read("packaging/memory-industry.service");
    assert!(
        operator_knobs_pinned_in_line(&unit, &env_file).is_empty(),
        "the shipped unit is the clean baseline this sabotage starts from"
    );
    let sabotaged = unit.replacen(
        "EnvironmentFile=",
        "Environment=\"CUBA_GPU_MEM_LIMIT_MB=2048\" RUST_LOG=info
EnvironmentFile=",
        1,
    );
    assert_eq!(
        operator_knobs_pinned_in_line(&sabotaged, &env_file),
        vec!["CUBA_GPU_MEM_LIMIT_MB".to_string()],
        "a unit that pins the GPU ceiling in line went unnoticed, or RUST_LOG — a log level, not          a knob the env file offers — was flagged. A contract that cannot see the one line it          exists to catch is a paragraph"
    );
}

#[test]
fn every_variable_the_generated_env_files_offer_is_offered_in_env_example() {
    let offered = variables_offered_in_env_example();
    for relative in [
        "packaging/memory-industry.env.example",
        "packaging/windows/memory-industry.env.example",
    ] {
        let generated = env_names_offered_in(&read(relative));
        assert!(
            generated.contains("CUBA_HTTP_TOKEN") && generated.contains("CUBA_GPU_MEM_LIMIT_MB"),
            "the parser found {generated:?} in {relative}, which offers both CUBA_HTTP_TOKEN and              CUBA_GPU_MEM_LIMIT_MB. It is the parser that is broken, not the file"
        );
        let missing: Vec<&String> = generated.difference(&offered).collect();
        assert!(
            missing.is_empty(),
            "{relative} is what `setup service` installs and offers {missing:?}, which              `.env.example` never mentions. The installed file is a subset of the reference one              by design; a knob only the generated file names is one an operator running from a              checkout never learns exists"
        );
    }
}

const LAN_GUIDE: &str = "docs/despliegue-lan.md";

/// Whitespace folded and backticks dropped: the guide wraps its prose at 80
/// columns and quotes commands in backticks, and a phrase split by a newline
/// is still the same phrase to the reader.
fn flatten(text: &str) -> String {
    text.replace('`', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `memory-industry setup service …` lines of a document, as the words the
/// binary would receive after `service`.
fn setup_service_commands(text: &str) -> Vec<(String, Vec<String>)> {
    commands_with_prefix(text, "memory-industry setup service")
}

/// The lines of a document that start with `prefix`, as the words the binary would receive
/// after it. Fenced and indented blocks both count; a trailing `\` or `^` joins the next
/// line; a ` #` starts a comment.
fn commands_with_prefix(text: &str, prefix: &str) -> Vec<(String, Vec<String>)> {
    let mut found = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let mut joined = line.trim().to_string();
        if !joined.starts_with(prefix) {
            continue;
        }
        while joined.ends_with('\\') || joined.ends_with('^') {
            joined.pop();
            match lines.next() {
                Some(next) => {
                    joined.push(' ');
                    joined.push_str(next.trim());
                }
                None => break,
            }
        }
        let command = joined.split(" #").next().unwrap_or("").trim().to_string();
        let args = command[prefix.len()..]
            .split_whitespace()
            .map(|w| w.trim_matches(|c| c == '"' || c == '\'').to_string())
            .collect();
        found.push((command, args));
    }
    found
}

/// E1. A command in the guide that the parser refuses is a command the reader
/// types, copies from the page, and gets «opción desconocida» for. The guide is
/// checked against `parse_request`, not against a list of flags somebody keeps.
///
/// Depends on slice C for `--register`: until the parser knows it, the line that
/// shows the registration step is refused here, which is the point.
#[test]
fn every_setup_service_command_in_the_lan_guide_is_one_the_parser_accepts() {
    let guide = read(LAN_GUIDE);
    let commands = setup_service_commands(&guide);

    assert!(
        commands.len() >= 4,
        "{LAN_GUIDE} shows {} `memory-industry setup service` command(s) and the procedure needs \
         at least 4: print the plan, apply the LAN profile, apply with --register, and \
         --uninstall. Found: {commands:?}. Fewer means the scan found nothing to judge and a \
         green result would prove nothing",
        commands.len()
    );

    let shows = |wanted: &dyn Fn(&[String]) -> bool| {
        commands.iter().any(|(_, args)| wanted(args.as_slice()))
    };
    let has = |args: &[String], word: &str| args.iter().any(|a| a == word);
    assert!(
        shows(&|a| !has(a, "--apply") && !has(a, "--uninstall")),
        "{LAN_GUIDE} never shows the plan (`setup service` or `--print`) that writes nothing: {commands:?}"
    );
    assert!(
        shows(&|a| has(a, "--apply") && has(a, "--profile") && has(a, "lan")),
        "{LAN_GUIDE} never shows `--apply --profile lan`, the command this page exists for: {commands:?}"
    );
    assert!(
        shows(&|a| has(a, "--apply") && has(a, "--register")),
        "{LAN_GUIDE} never shows `--apply --register`, the step that makes the daemon come back \
         after a reboot: {commands:?}"
    );
    assert!(
        shows(&|a| has(a, "--uninstall")),
        "{LAN_GUIDE} never shows `--uninstall`, how the install is taken away: {commands:?}"
    );

    let refused: Vec<String> = commands
        .iter()
        .filter_map(|(command, args)| {
            // The guide shows --register only in the Windows procedure; the
            // parser judges it for the target, and the target defaults to the
            // host. Without this, the check passes on a Windows host and fails
            // on Linux for a command that is correct.
            let mut judged = args.clone();
            let names_a_target = args.iter().any(|a| a == "--windows" || a == "--linux");
            if args.iter().any(|a| a == "--register") && !names_a_target {
                judged.push("--windows".to_string());
            }
            memory_industry::service::parse_request(judged.as_slice())
                .err()
                .map(|e| format!("`{command}` -> {e}"))
        })
        .collect();
    assert!(
        refused.is_empty(),
        "{LAN_GUIDE} tells the reader to type commands that `setup service` refuses: {refused:#?}"
    );
}

/// E2. The procedure's exact words, the troubleshooting table, and the tie
/// between the table's quoted error messages and the code that prints them.
///
/// Depends on slice B for `is not assigned to this machine` / `is already in
/// use` in `http.rs`, and on slice C for `--register` in `service.rs`.
#[test]
fn the_lan_guide_carries_the_procedure_and_a_troubleshooting_table_tied_to_the_code() {
    let raw = read(LAN_GUIDE);
    let guide = flatten(&raw);
    let service_rs = include_str!("../src/service.rs");
    let http_rs = include_str!("../src/http.rs");

    let procedure = [
        "--profile lan",
        "--register",
        "schtasks /Query /TN memory-industry",
        "netsh advfirewall firewall show rule name=memory-industry",
        "memory-industry.log",
        "MEMORY_INDUSTRY_BIND_WAIT_SECS",
        "memory-industry doctor",
    ];
    let missing: Vec<&str> = procedure
        .iter()
        .copied()
        .filter(|w| !guide.contains(w))
        .collect();
    assert!(
        missing.is_empty(),
        "{LAN_GUIDE} does not mention {missing:?}. The page is the whole procedure: apply with \
         --register, check the task and the firewall rule with those two read-only commands, read \
         the log, know the bind-wait knob, run doctor"
    );

    // Absence is only meaningful once the new text is known to be there.
    assert!(
        !guide.contains("a scheduled task that runs at logon"),
        "{LAN_GUIDE} still says «a scheduled task that runs at logon». The task now starts at \
         boot as SYSTEM, with no session; the old sentence is the promise 0.29.0 replaces"
    );

    // The names the guide uses are the names the code uses.
    for (word, why) in [
        ("--register", "the flag `setup service` parses"),
        ("memory-industry.log", "the file the launcher appends to"),
    ] {
        assert!(
            service_rs.contains(word),
            "{LAN_GUIDE} documents {word}, {why}, but rust/src/service.rs never mentions it: the \
             page promises something the binary does not have"
        );
    }
    assert!(
        http_rs.contains("MEMORY_INDUSTRY_BIND_WAIT_SECS"),
        "{LAN_GUIDE} documents MEMORY_INDUSTRY_BIND_WAIT_SECS but rust/src/http.rs does not read it"
    );

    let rows: Vec<&str> = raw
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .collect();
    assert!(
        rows.len() >= 9,
        "{LAN_GUIDE} has {} table rows; the troubleshooting table needs its header, its divider \
         and at least 7 symptoms. Found: {rows:?}",
        rows.len()
    );
    let table = flatten(&rows.join("\n"));
    let symptoms = [
        (
            "is not assigned to this machine",
            "the address belongs to no interface yet",
        ),
        ("is already in use", "another process holds the port"),
        (
            "refusing to bind",
            "token missing or shorter than 32 characters",
        ),
        (
            "remote clients time out",
            "local answers, remote does not: the firewall rule is missing",
        ),
        (
            "does not start after a reboot",
            "check the task and read the log",
        ),
        (
            "lexical search",
            "no models: USERPROFILE / ONNX_MODEL_PATH under SYSTEM",
        ),
        (
            "Postgres is not up after a reboot",
            "the container starts at sign-in, not at boot",
        ),
    ];
    let absent: Vec<String> = symptoms
        .iter()
        .filter(|(text, _)| !table.contains(text))
        .map(|(text, row)| format!("«{text}» ({row})"))
        .collect();
    assert!(
        absent.is_empty(),
        "the troubleshooting table in {LAN_GUIDE} has no row for: {absent:#?}. Each must sit in a \
         markdown table row (a line starting with `|`), phrased with exactly that text"
    );

    for message in [
        "is not assigned to this machine",
        "is already in use",
        "refusing to bind",
    ] {
        assert!(
            http_rs.contains(message),
            "the guide's table quotes «{message}» but rust/src/http.rs never prints it: the row \
             would send the operator looking for text that does not exist"
        );
    }
}

/// E3. What running as SYSTEM means, said in the page. These are presence
/// anchors, declared as such: the behaviour is judged by E1/E2 and by the unit
/// tests of slices A and C; this keeps the explanation from being dropped.
#[test]
fn the_lan_guide_says_what_running_as_system_implies() {
    let guide = flatten(&read(LAN_GUIDE));
    assert!(
        guide.contains("--profile lan"),
        "{LAN_GUIDE} has no `--profile lan` at all, so none of the facts below can be located: \
         the guide has not been rewritten for 0.29.0"
    );

    let facts = [
        ("SYSTEM", "the task runs as SYSTEM"),
        ("logged in", "and needs nobody logged in"),
        (
            "privileges",
            "SYSTEM runs the daemon with every privilege on the machine",
        ),
        ("absolute", "the .exe, env file and log paths are absolute"),
        (
            "--apply again",
            "if the .exe moves, --apply is repeated: the paths are baked in",
        ),
        ("5 MiB", "the log rotates at 5 MiB when the daemon starts"),
        (
            "without --profile",
            "--apply without --profile keeps the env file's address",
        ),
        (
            "CUBA_HTTP_ADDR",
            "the address is changed in the env file, then --apply",
        ),
        (
            "schtasks /Delete /TN memory-industry /F",
            "the first command --uninstall prints to remove the task",
        ),
        (
            "netsh advfirewall firewall delete rule name=memory-industry",
            "the second command --uninstall prints to remove the firewall rule",
        ),
    ];
    let missing: Vec<String> = facts
        .iter()
        .filter(|(text, _)| !guide.contains(text))
        .map(|(text, meaning)| format!("«{text}» ({meaning})"))
        .collect();
    assert!(
        missing.is_empty(),
        "{LAN_GUIDE} does not explain what the SYSTEM task implies; missing: {missing:#?}"
    );
}

/// E4. The versioned Windows files use `%LOCALAPPDATA%` as documentation; SYSTEM
/// has no such profile, so feeding them to `schtasks` by hand installs a task
/// that points at nothing. Presence anchor, declared as such.
#[test]
fn packaging_readme_warns_that_the_windows_goldens_are_not_installed_by_hand() {
    let readme = flatten(&read("packaging/README.md"));
    assert!(
        readme.contains("%LOCALAPPDATA%"),
        "packaging/README.md does not say the files under packaging/windows use %LOCALAPPDATA% \
         as documentation: the SYSTEM task cannot expand it, so passing them to `schtasks` as \
         they are installs a task pointing at an empty profile"
    );
    assert!(
        readme.contains("setup service --apply"),
        "packaging/README.md does not say the Windows files are installed with \
         `setup service --apply`, which bakes absolute paths in"
    );
}

// ── 0.29.0 part 2: accounts, the bridge and the fixed address (slice I) ──────────────

const ACCOUNTS_HEADING: &str = "Accounts";
const FIXED_ADDRESS_HEADING: &str = "Fixed address on the host";

/// The body of the `##` section whose heading line contains `heading`, up to the next `##`
/// heading. Lines inside a fence are never headings (a PowerShell comment starts with `#`).
fn section(text: &str, heading: &str) -> Option<String> {
    let mut body = Vec::new();
    let mut inside_section = false;
    let mut inside_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            inside_fence = !inside_fence;
        }
        let is_heading = !inside_fence && line.starts_with("## ");
        if is_heading && inside_section {
            break;
        }
        if is_heading && line.contains(heading) {
            inside_section = true;
            continue;
        }
        if inside_section {
            body.push(line);
        }
    }
    inside_section.then(|| body.join("\n"))
}

/// The lines a reader would copy: indented by four spaces, or inside a fence.
fn code_lines(text: &str) -> Vec<&str> {
    let mut inside_fence = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            inside_fence = !inside_fence;
            continue;
        }
        if inside_fence || line.starts_with("    ") {
            out.push(line);
        }
    }
    out
}

fn missing_from<'a>(haystack: &str, needles: &[&'a str]) -> Vec<&'a str> {
    needles
        .iter()
        .copied()
        .filter(|needle| !haystack.contains(needle))
        .collect()
}

/// I7. The guide's account, bridge and `setup client` commands are judged by the parsers
/// the binary uses, so a typo in the page is a red test and not a reader's error message.
#[test]
fn every_account_bridge_and_client_command_in_the_lan_guide_is_one_the_parsers_accept() {
    use memory_industry::accounts_cli::{self, Command as Account};
    use memory_industry::bridge::{self, Command as Bridge, client_config};

    let guide = read(LAN_GUIDE);
    let accounts = commands_with_prefix(&guide, "memory-industry account");
    let bridges = commands_with_prefix(&guide, "memory-industry bridge");
    let clients = commands_with_prefix(&guide, "memory-industry setup client");

    assert!(
        accounts.len() >= 5 && bridges.len() >= 4 && !clients.is_empty(),
        "{LAN_GUIDE} shows {} `memory-industry account`, {} `memory-industry bridge` and {} \
         `memory-industry setup client` command(s); the procedure needs at least 5, 4 and 1. \
         Fewer means the scan found nothing to judge and a green result would prove nothing. \
         Found: {accounts:?} {bridges:?} {clients:?}",
        accounts.len(),
        bridges.len(),
        clients.len()
    );

    let mut refused: Vec<String> = Vec::new();
    let mut shown_accounts = Vec::new();
    for (command, args) in &accounts {
        match accounts_cli::parse(args) {
            Ok(parsed) => shown_accounts.push(parsed),
            Err(err) => refused.push(format!("`{command}` -> {err}")),
        }
    }
    let mut shown_bridge = Vec::new();
    for (command, args) in &bridges {
        match bridge::parse(args) {
            Ok(parsed) => shown_bridge.push(parsed),
            Err(err) => refused.push(format!("`{command}` -> {err}")),
        }
    }
    for (command, args) in &clients {
        if let Err(err) = client_config::parse_client_args(args) {
            refused.push(format!("`{command}` -> {err}"));
        }
    }
    assert!(
        refused.is_empty(),
        "{LAN_GUIDE} tells the reader to type commands the binary refuses: {refused:#?}"
    );

    let teaches = |wanted: bool, what: &str| {
        assert!(
            wanted,
            "{LAN_GUIDE} never shows {what}. Parsed: {shown_accounts:?} {shown_bridge:?}"
        );
    };
    teaches(
        shown_accounts
            .iter()
            .any(|c| matches!(c, Account::Add { .. })),
        "`account add`, how an account is created",
    );
    teaches(
        shown_accounts
            .iter()
            .any(|c| matches!(c, Account::Pair { .. })),
        "`account pair`, how the code for a device is issued",
    );
    teaches(
        shown_accounts
            .iter()
            .any(|c| matches!(c, Account::List { .. })),
        "`account list`, how the operator sees who is paired",
    );
    teaches(
        shown_accounts
            .iter()
            .any(|c| matches!(c, Account::Revoke { .. })),
        "`account revoke`, how a person loses access",
    );
    teaches(
        shown_accounts
            .iter()
            .any(|c| matches!(c, Account::RevokeDevice { .. })),
        "`account revoke-device`, how one lost PC loses access",
    );
    teaches(
        shown_bridge
            .iter()
            .any(|c| matches!(c, Bridge::Pair { .. })),
        "`bridge pair`, the one command a client types to pair",
    );
    teaches(
        shown_bridge
            .iter()
            .any(|c| matches!(c, Bridge::Install { .. })),
        "`bridge install`, how the bridge starts at every logon",
    );
    teaches(
        shown_bridge.iter().any(|c| matches!(c, Bridge::Doctor)),
        "`bridge doctor`, how the client finds out why it cannot reach the host",
    );
}

/// I7. Whatever else the guide tells the reader to run is a command the binary has.
#[test]
fn every_other_memory_industry_command_in_the_lan_guide_exists() {
    let guide = read(LAN_GUIDE);
    let known: BTreeSet<&str> = memory_industry::cli::COMMANDS.iter().copied().collect();
    let judged_elsewhere = ["account", "bridge"];
    let setup_subcommands = ["service", "client", "check", "print", "hook"];

    let mut unknown = Vec::new();
    let mut seen = 0;
    for line in guide.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let [first, command, rest @ ..] = words.as_slice() else {
            continue;
        };
        if *first != "memory-industry" || judged_elsewhere.contains(command) {
            continue;
        }
        seen += 1;
        let ok = if *command == "setup" {
            rest.first().is_some_and(|sub| {
                setup_subcommands.contains(sub) || ["claude", "mcp", "cursor"].contains(sub)
            })
        } else {
            known.contains(command)
        };
        if !ok {
            unknown.push(line.trim().to_string());
        }
    }
    assert!(
        seen >= 3,
        "the scan found {seen} other `memory-industry` command line(s) in {LAN_GUIDE} and the \
         page shows at least `models all`, `doctor --deep` and `setup service`: it is not \
         reading the page"
    );
    assert!(
        unknown.is_empty(),
        "{LAN_GUIDE} tells the reader to run commands the binary does not have: {unknown:#?}"
    );
}

/// I7. The section on accounts and the bridge is the procedure: what to type on the host and
/// on a client, what the bridge is, how to revoke, how to read `bridge doctor`, what the
/// proof of identity does not cover, and a table of symptoms tied to the messages the code
/// prints. Presence anchors, declared as such: the commands themselves are judged by the
/// parsers above, and the behaviour by the unit tests of slices F, H and I.
#[test]
fn the_lan_guide_teaches_accounts_pairing_and_the_bridge() {
    let raw = read(LAN_GUIDE);
    let accounts_section = section(&raw, ACCOUNTS_HEADING).unwrap_or_else(|| {
        panic!(
            "{LAN_GUIDE} has no `## …{ACCOUNTS_HEADING}…` section: the page has not been \
             extended for accounts and the bridge, so none of the facts below can be located"
        )
    });
    let guide = flatten(&accounts_section);

    let cited = [
        "memory-industry account add",
        "memory-industry account pair",
        "memory-industry account list",
        "memory-industry account revoke",
        "memory-industry account revoke-device",
        "memory-industry bridge pair",
        "memory-industry bridge install",
        "memory-industry bridge doctor",
        "memory-industry setup client",
        "MEMORY_INDUSTRY_REQUIRE_ACCOUNTS",
        r"%LOCALAPPDATA%\MemoryIndustry\bridge.json",
    ];
    let missing = missing_from(&guide, &cited);
    assert!(
        missing.is_empty(),
        "the accounts section of {LAN_GUIDE} does not cite {missing:?}"
    );

    let bridge_loopback = format!("127.0.0.1:{}", memory_industry::bridge::DEFAULT_BRIDGE_PORT);
    assert!(
        guide.contains(&bridge_loopback),
        "the accounts section names the bridge's loopback address, and it must be \
         {bridge_loopback}: bridge::DEFAULT_BRIDGE_PORT. A page with another port sends the \
         reader's client where nothing listens"
    );
    let host_port = memory_industry::http::DEFAULT_ADDR
        .rsplit(':')
        .next()
        .unwrap_or("");
    assert_eq!(
        host_port,
        memory_industry::bridge::DEFAULT_HOST_PORT.to_string(),
        "the bridge looks for the host on the port the daemon binds by default; two constants \
         that disagree make every pairing without --host miss"
    );
    assert!(
        guide.contains(&format!("192.168.0.10:{host_port}")),
        "the accounts section shows the host as <address>:{host_port} (http::DEFAULT_ADDR's \
         port), the address `bridge doctor` prints in its last line"
    );

    let facts = [
        ("two minutes", "pairing a client takes under two minutes"),
        ("without administrator", "and needs no administrator rights"),
        (
            "holds the credential",
            "what the bridge is: it holds the credential",
        ),
        ("next request", "revocation applies from the next request"),
        ("huella", "the fingerprint word `bridge doctor` prints"),
        (
            "madre:",
            "the last line of `bridge doctor`, the host it found",
        ),
        ("clear text", "traffic is still plain HTTP"),
        ("TLS", "and TLS is the answer"),
        ("next version", "which is the next version"),
        (
            "does not cover",
            "what the proof of identity does not cover",
        ),
    ];
    let absent: Vec<String> = facts
        .iter()
        .filter(|(text, _)| !guide.contains(text))
        .map(|(text, meaning)| format!("«{text}» ({meaning})"))
        .collect();
    assert!(
        absent.is_empty(),
        "the accounts section of {LAN_GUIDE} is missing: {absent:#?}"
    );
}

/// I7. The symptom table of the accounts section quotes what the bridge prints, so a row
/// cannot send the operator looking for text that does not exist.
#[test]
fn the_accounts_troubleshooting_table_quotes_messages_the_bridge_prints() {
    let raw = read(LAN_GUIDE);
    let accounts_section = section(&raw, ACCOUNTS_HEADING)
        .unwrap_or_else(|| panic!("{LAN_GUIDE} has no `## …{ACCOUNTS_HEADING}…` section"));
    let rows: Vec<&str> = accounts_section
        .lines()
        .filter(|l| l.trim_start().starts_with('|'))
        .collect();
    assert!(
        rows.len() >= 7,
        "the accounts section has {} table rows; the table needs its header, its divider and at \
         least 5 symptoms. Found: {rows:?}",
        rows.len()
    );
    let table = flatten(&rows.join("\n"));

    let mod_rs = include_str!("../src/bridge/mod.rs");
    let pair_rs = include_str!("../src/bridge/pair.rs");
    let proxy_rs = include_str!("../src/bridge/proxy.rs");
    let symptoms = [
        (
            "la madre cambió de huella",
            mod_rs,
            "the host answered with another fingerprint than the one pinned",
        ),
        (
            "credencial revocada",
            mod_rs,
            "the operator revoked this device",
        ),
        (
            "cannot listen on 127.0.0.1",
            proxy_rs,
            "the bridge's port is taken by another program",
        ),
        (
            "no se encontró la madre",
            mod_rs,
            "nothing on the LAN proved to be the host",
        ),
        (
            "la madre no aceptó el código",
            pair_rs,
            "the pairing code is mistyped, expired or used",
        ),
    ];
    let absent: Vec<String> = symptoms
        .iter()
        .filter(|(text, _, _)| !table.contains(text))
        .map(|(text, _, row)| format!("«{text}» ({row})"))
        .collect();
    assert!(
        absent.is_empty(),
        "the accounts table in {LAN_GUIDE} has no row for: {absent:#?}. Each must sit in a \
         markdown table row (a line starting with `|`), phrased with exactly that text"
    );
    for (text, source, _) in symptoms {
        assert!(
            source.contains(text),
            "the guide's table quotes «{text}» but the bridge's source never prints it: the row \
             would send the operator looking for text that does not exist"
        );
    }
}

/// I7. The host needs a fixed address, and the page says how to give it one without a router
/// the operator cannot reach, and what can go wrong. Presence anchors, declared as such.
#[test]
fn the_lan_guide_gives_the_fixed_address_steps_and_the_dhcp_warning() {
    let raw = read(LAN_GUIDE);
    let fixed = section(&raw, FIXED_ADDRESS_HEADING).unwrap_or_else(|| {
        panic!(
            "{LAN_GUIDE} has no `## …{FIXED_ADDRESS_HEADING}…` section: the host keeps one \
             address only if somebody fixes it, and this is where the page says how"
        )
    });

    let code = code_lines(&fixed).join("\n");
    let commands = [
        "Get-NetIPConfiguration",
        "Set-NetIPInterface",
        "-Dhcp Disabled",
        "New-NetIPAddress",
        "Set-DnsClientServerAddress",
        "ipconfig /all",
    ];
    let not_copyable = missing_from(&code, &commands);
    assert!(
        not_copyable.is_empty(),
        "the `{FIXED_ADDRESS_HEADING}` section must show {not_copyable:?} in a code block the \
         reader can copy (indented four spaces or fenced), not only in prose. Code lines found:\n{code}"
    );

    let prose = flatten(&fixed);
    let facts = [
        ("elevated", "these commands need an elevated PowerShell"),
        (
            "outside the DHCP",
            "choose an address outside the DHCP range",
        ),
        (
            "cannot see",
            "Windows cannot see the DHCP range of the router",
        ),
        (
            "administers the router",
            "ask whoever administers the router for the range",
        ),
        (
            "memory-industry bridge doctor",
            "the check afterwards: another machine on that IP is rejected by the bridge",
        ),
    ];
    let absent: Vec<String> = facts
        .iter()
        .filter(|(text, _)| !prose.contains(text))
        .map(|(text, meaning)| format!("«{text}» ({meaning})"))
        .collect();
    assert!(
        absent.is_empty(),
        "the `{FIXED_ADDRESS_HEADING}` section of {LAN_GUIDE} is missing: {absent:#?}"
    );
}

/// One `###` part of the changelog block headed `[<version>]`. A contract that belongs to the
/// release that introduced it names that release here instead of following the crate's version.
fn changelog_part_of(changelog: &str, version: &str, heading: &str) -> Option<String> {
    let opening = format!("{version}]");
    let block = changelog
        .split("\n## [")
        .find(|block| block.starts_with(&opening))?;
    let mut parts = block.split("\n### ");
    parts
        .find(|part| part.starts_with(heading))
        .map(str::to_string)
}

/// I8, the contract of 0.29.0, the release that introduced accounts and the bridge. Its changelog
/// block says what a deployer has to know before updating: that accounts, pairing, the bridge and
/// the knob change behaviour, and what has not been verified on a real host. Pinned to `[0.29.0]`
/// and not to the release in progress: from 0.30.0 on that block talks about something else.
/// Presence anchors, declared as such.
#[test]
fn the_changelog_announces_accounts_and_the_bridge_and_what_is_not_verified() {
    let changelog = read("CHANGELOG.md");
    let behaviour = changelog_part_of(&changelog, "0.29.0", "Behaviour changes").expect(
        "CHANGELOG.md has no `### Behaviour changes` inside `## [0.29.0]`: the anchor for the \
         checks below is gone",
    );
    let behaviour = flatten(&behaviour);
    let announced = [
        "MEMORY_INDUSTRY_REQUIRE_ACCOUNTS",
        "account",
        "pairing code",
        "bridge",
    ];
    let missing = missing_from(&behaviour, &announced);
    assert!(
        missing.is_empty(),
        "0.29.0 → `Behaviour changes` does not mention {missing:?}: a deployer \
         reads that part before updating, and accounts, pairing, the bridge and the knob change \
         what the daemon accepts"
    );

    let unverified = changelog_part_of(&changelog, "0.29.0", "Not verified").expect(
        "CHANGELOG.md has no `### Not verified` inside `## [0.29.0]`: the unverified list is \
         where the next person learns what only a real host can prove",
    );
    let unverified = flatten(&unverified);
    let listed = [
        "ONLOGON",
        "conhost --headless",
        "Set-NetIPInterface",
        "Cursor",
    ];
    let missing = missing_from(&unverified, &listed);
    assert!(
        missing.is_empty(),
        "0.29.0 → `Not verified` does not list {missing:?}: the per-user logon \
         task, the hidden console, the fixed-address cmdlets and Cursor's `http` entry were never \
         run on a real machine, and the changelog must say so"
    );
}

/// I8. The README (written by slice G, only watched here) has a row for the knob and names
/// the two routes. Presence anchors, declared as such.
#[test]
fn the_readme_documents_the_accounts_knob_and_the_two_pairing_routes() {
    let readme = read("README.md");
    assert!(
        readme
            .lines()
            .any(|l| l.trim_start().starts_with('|')
                && l.contains("MEMORY_INDUSTRY_REQUIRE_ACCOUNTS")),
        "README.md has no table row for MEMORY_INDUSTRY_REQUIRE_ACCOUNTS: the knob that closes \
         the shared token to the LAN would be one an operator never learns exists"
    );
    for route in ["/identity", "/pair"] {
        assert!(
            readme.contains(route),
            "README.md never names the {route} route: a firewall rule or a monitor cannot be \
             written for an endpoint the page does not list"
        );
    }
}
