//! Anti-drift for the **local** merge judge only (not GitHub Actions).
//! If someone removes require_present / deny / docs / codigo-muerto from the
//! scripts, this fails before a soft gate can look green again.

use std::collections::HashMap;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn mutation_never_builds_into_a_target_somebody_else_uses() {
    for script in ["scripts/mutants-gate.sh", "scripts/quality-gate.sh"] {
        let body = read(script);
        assert!(
            body.contains("unset CARGO_TARGET_DIR"),
            "{script} runs cargo mutants, which compiles a scratch copy per mutant. Pointed at a target directory anything else uses, it leaves artifacts behind and the next ordinary build fails tests on sources nobody edited - measured as four reds in rrf.rs and eval/datasets.rs after a green run."
        );
    }
}

#[test]
fn merge_gate_is_the_local_judge_and_stays_strict() {
    let gate = read("scripts/merge-gate.sh");
    for needle in [
        "run-all-tests.sh",
        "--features docs",
        "cargo deny",
        "npm/install.test.js",
        "codigo-muerto.sh",
        "crap-gate.sh",
        "mutants-gate.sh",
        "WHAT THIS GATE DOES NOT CHECK",
        "never SKIPPED",
        "generative LLM",
    ] {
        assert!(
            gate.contains(needle),
            "scripts/merge-gate.sh must keep `{needle}` — local CI is the sole merge judge"
        );
    }
    assert!(
        !gate.contains("SKIPPED:"),
        "merge-gate.sh must not introduce soft SKIPPED lines"
    );
}

#[test]
fn run_all_tests_requires_models_and_generative_llm() {
    let suite = read("scripts/run-all-tests.sh");
    for needle in [
        "require_present",
        "require_generative_llm",
        "ONNX_MODEL_PATH",
        "CUBA_NLI_PATH",
        "CUBA_RERANKER_PATH",
        "DEFERRED_SECTIONS",
        "MEMORY_INDUSTRY_LLM_BASE_URL",
    ] {
        assert!(
            suite.contains(needle),
            "scripts/run-all-tests.sh must keep `{needle}`"
        );
    }
    assert!(
        !suite.contains("run_if_present"),
        "run_if_present soft-skips were removed; do not bring them back"
    );
    assert!(
        !suite.contains("require_command \"tests that need a local LLM CLI\" claude"),
        "claude-only require_command was replaced by require_generative_llm"
    );
    assert!(
        !suite.contains("SKIPPED:"),
        "run-all-tests.sh must not print soft SKIPPED coverage holes"
    );
}

#[test]
fn quality_gate_is_the_diff_judge_not_the_sil() {
    let q = read("scripts/quality-gate.sh");
    for needle in [
        "NO MIRA",
        "NO lo sustituye",
        "lizard",
        "cargo mutants",
        "rust/src",
        "merge-gate.sh",
    ] {
        assert!(
            q.contains(needle),
            "scripts/quality-gate.sh must keep `{needle}` — second judge, not a merge-gate alias"
        );
    }
    assert!(
        !q.contains("exec \"$ROOT/scripts/merge-gate.sh\""),
        "quality-gate.sh used to exec merge-gate.sh; it is now CRAP + mutants of the rust/src diff"
    );
    assert!(
        q.contains("unset CARGO_TARGET_DIR"),
        "quality-gate.sh must unset CARGO_TARGET_DIR — Cursor/sandbox points it at a shared cache; \
         cargo-mutants then compiles every scratch copy there and leftover mutant artifacts \
         (rrf.rs from mutants-gate) make the unmutated baseline fail tests that pass on a clean target"
    );
    assert!(
        q.contains("bin-only") && q.contains("src/main.rs"),
        "quality-gate.sh must skip src/main.rs — it is bin-only; cargo mutants -- --lib \
         never executes it, so drain_then_report/async_main always survive"
    );
    assert!(
        q.contains("src/handlers")
            && q.contains("--lib never awaits them")
            && (q.contains("SIL --ignored") || q.contains("--ignored")),
        "quality-gate.sh must skip src/handlers/* — cargo mutants -- --lib never awaits \
         async MCP handlers (362 survivors, 245 in reflexion alone, measured 2026-09-18); \
         the SIL --ignored + e2e cover them"
    );
    assert!(
        q.contains("src/cli.rs") && q.contains("src/doctor.rs") && q.contains("CLI/doctor surface"),
        "quality-gate.sh must skip CLI/doctor surfaces — cargo mutants -- --lib does not \
         drive those entrypoints; SIL e2e + doctor smoke cover them"
    );
    assert!(
        q.contains("--exclude-re"),
        "quality-gate.sh must pass --exclude-re for DB/CLI entrypoints that --lib never \
         reaches (fetch_adjacency, list_resources, run_checks_with, …) — otherwise they \
         always survive and the second judge can never close on a product crate"
    );
}

#[test]
fn como_el_ci_todo_is_the_sil() {
    let s = read("scripts/como-el-ci.sh");
    assert!(
        s.contains("merge-gate.sh"),
        "como-el-ci.sh todo must stay an alias of the SIL"
    );
    assert!(
        s.contains("quality-gate.sh"),
        "como-el-ci.sh quality must chain the second judge"
    );
}

#[test]
fn six_pack_rules_are_versioned_and_not_gitignored() {
    for rel in [
        ".cursor/rules/el-gate-es-el-juez.mdc",
        ".cursor/rules/testear-hasta-rojo.mdc",
        ".cursor/rules/programar-inmutable.mdc",
        ".cursor/rules/swarm-forge-orquestador.mdc",
        ".cursor/rules/swarm-forge-ciclo.mdc",
        ".cursor/rules/swarm-forge-tdd.mdc",
        ".cursor/rules/swarm-forge-crap.mdc",
        ".cursor/rules/swarm-forge-mutacion.mdc",
        ".cursor/rules/swarm-forge-handoff.mdc",
        ".cursor/rules/governance.mdc",
        ".cursor/rules/memory-industry-gate.mdc",
        ".cursor/rules/memory-industry-gobernanza.mdc",
        ".cursor/handoffs/handoff.example.yml",
        "scripts/como-el-ci.sh",
        "scripts/validar-handoff.sh",
        "docs/gate.md",
        "docs/despliegue-lan.md",
    ] {
        assert!(
            repo_root().join(rel).is_file(),
            "{rel} must exist in the tree — a junction to ~/.cursor/rules wipes project rules on clone"
        );
    }
    let gi = read(".gitignore");
    let ignores_rules = gi.lines().any(|l| {
        let t = l.trim();
        t == ".cursor/rules/" || t == ".cursor/rules" || t == ".cursor/rules/*"
    });
    assert!(
        !ignores_rules,
        ".gitignore must not ignore .cursor/rules/ — that is why the six-pack never shipped"
    );
    for doc in ["!docs/gate.md", "!docs/despliegue-lan.md"] {
        assert!(
            gi.contains(doc),
            "docs/* is ignored, so {doc} must stay force-included or the file is invisible to              git: it would sit in the tree, pass every test that reads it from disk, and never              reach a clone"
        );
    }
    assert!(
        !gi.lines().any(|l| l.trim() == "docs/"),
        "a trailing-slash `docs/` ignores the directory itself and makes !docs/gate.md a no-op"
    );
    // `git check-ignore`, not `git add -n`. The latter prints the path only
    // while the file is still untracked, so the moment one of these docs got
    // committed the assertion went quiet and stopped testing anything — which
    // is precisely what happened to docs/gate.md. check-ignore answers the
    // question we actually care about, in any tracked state: exit 1 means git
    // does not ignore this path, so the force-include is doing its job.
    for doc in ["docs/gate.md", "docs/despliegue-lan.md"] {
        let ignored = std::process::Command::new("git")
            .args(["check-ignore", "-q", "--", doc])
            .current_dir(repo_root())
            .status()
            .expect("git check-ignore runs");
        assert!(
            !ignored.success(),
            "git ignores {doc}. `docs/*` hides the whole directory, so without its own              `!docs/{}` line the file sits in the tree, passes every test that reads it from              disk, and never reaches a clone",
            doc.trim_start_matches("docs/")
        );
    }
}

#[test]
fn live_session_resolves_the_windows_exe() {
    let py = read("scripts/mcp_live_session_test.py");
    assert!(
        py.contains(".exe"),
        "mcp_live_session_test.py must accept memory-industry.exe — Git Bash \
         exported the extensionless path and Python Path.is_file() said no"
    );
}

#[test]
fn release_build_finds_the_binary_under_cargo_target_dir() {
    let gpu = read("scripts/build-gpu.sh");
    assert!(
        gpu.contains("CARGO_TARGET_DIR"),
        "build-gpu.sh must look under CARGO_TARGET_DIR — after a 6 min link it \
         exec'd rust/target/release/cuba-memorys and exited 127"
    );
    let suite = read("scripts/run-all-tests.sh");
    assert!(
        suite.contains("gate_target_dir") && suite.contains("CUBA_BINARY_PATH"),
        "E2E CUBA_BINARY_PATH must follow the same target dir as the release build"
    );
}

#[test]
fn gate_bin_finds_the_binary_under_cargo_target_dir() {
    let suite = read("scripts/run-all-tests.sh");
    assert!(
        suite.contains("CARGO_TARGET_DIR"),
        "gate_bin must honor CARGO_TARGET_DIR — Cursor/sandbox sets it and rust/target/debug \
         then does not exist, so doctor never migrates brain_gate"
    );
    assert!(
        suite.contains(".exe"),
        "gate_bin must accept memory-industry.exe — Git Bash -x on the extensionless name is false"
    );
}

#[test]
fn gate_psql_puts_options_before_the_url() {
    let suite = read("scripts/run-all-tests.sh");
    assert!(
        suite.contains("psql_url") || suite.contains("psql -d \"$"),
        "run-all-tests.sh must call host psql with the URI as -d, not as argv[1]"
    );
    for needle in [
        "psql \"$ADMIN_DATABASE_URL\"",
        "psql \"$GATE_DATABASE_URL\"",
        "psql \"$PEER_DATABASE_URL\"",
    ] {
        assert!(
            !suite.contains(needle),
            "Windows psql 16 ignores -c when the URI is the first argument — CREATE \
             DATABASE never ran and the gate died with 'database brain_gate does not exist'. \
             Found `{needle}`"
        );
    }
}

#[test]
fn backup_scripts_name_the_compose_container() {
    let compose = read("docker-compose.yml");
    assert!(
        compose.contains("container_name: memory-industry-db"),
        "docker-compose.yml is the source of the live-cluster name"
    );
    for rel in [
        "scripts/backup-db.sh",
        "scripts/restore-db.sh",
        "scripts/merge-gate.sh",
    ] {
        let s = read(rel);
        assert!(
            s.contains("memory-industry-db"),
            "{rel} still looks only for cuba-memorys-db; compose renamed the container and \
             the host pg_dump (16) cannot dump PG 18 — that is how merge-gate died on the \
             first line after Postgres :5488"
        );
        assert!(
            s.contains("cuba-memorys-db"),
            "{rel} must still accept the pre-rebrand container"
        );
    }
}

#[test]
fn comments_stay_plant_rules_are_not_copied() {
    let agents = read("AGENTS.md");
    assert!(
        agents.contains("Comments stay") || agents.contains("load-bearing"),
        "AGENTS.md must keep the comment policy — this crate is not Mapupita-Rust cero comentarios"
    );
    let rules_dir = repo_root().join(".cursor/rules");
    for entry in
        std::fs::read_dir(&rules_dir).unwrap_or_else(|e| panic!("{}: {e}", rules_dir.display()))
    {
        let path = entry.expect("rules dir entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(
            !name.contains("guardian-planta")
                && !name.contains("react-query")
                && name != "mapupita-e2e.mdc"
                && name != "mapupita-gate.mdc",
            "plant-only rule leaked into Memorys: {name}"
        );
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !text.contains("Los `.rs` no llevan comentarios")
                && !text.contains("Los .rs no llevan comentarios"),
            "{name} copied Mapupita-Rust's no-comments-in-sources rule as if it applied here"
        );
    }
}

/// The guard that could not fail.
///
/// `codigo-muerto.sh` printed "OK every #[ignore] integration file is covered"
/// over every commit for months while being structurally incapable of
/// returning anything else: `has_discovery` was always 1, so every file hit an
/// empty `if` body and `continue`d before reaching the counter.
///
/// Reading the script's text would not have caught that, and would not catch
/// the next version of it either. This runs the script against fixtures built
/// to break each of its guards, and the script reports whether they broke.
#[test]
fn codigo_muerto_can_actually_fail() {
    let out = std::process::Command::new(git_bash())
        .args(["scripts/codigo-muerto.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "codigo-muerto.sh --self-test did not pass, so at least one of its guards no longer          fails when it should. stdout: {stdout}
stderr: {stderr}"
    );
    // Anchored on the mode's label, not on its sentence: that line names the
    // fixtures it ran, so adding one rewrites it — which is how the twin of this
    // assert over `quality-gate.sh` went red over a script that had passed.
    // `self-test:` is the flag this test passes, so renaming the mode forces the
    // `args` above to change with it, and no other line of the script prints it.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let the          original guard pass while doing nothing. stdout: {stdout}"
    );
}

/// On Windows the `bash` on PATH is WSL's, and it cannot translate a `D:\...`
/// working directory — it prints "Failed to translate" and exits without ever
/// reading the script. The gate itself is documented as running under Git
/// Bash for exactly this reason, so a test that shells out has to resolve the
/// same interpreter rather than trust PATH.
fn git_bash() -> std::path::PathBuf {
    if cfg!(windows) {
        for candidate in [
            "C:/Program Files/Git/bin/bash.exe",
            "C:/Program Files (x86)/Git/bin/bash.exe",
        ] {
            let path = std::path::PathBuf::from(candidate);
            if path.is_file() {
                return path;
            }
        }
    }
    std::path::PathBuf::from("bash")
}

/// One second judge, one implementation.
///
/// `quality-gate.ps1` used to be a parallel port of the `.sh`, and the two
/// drifted precisely where nothing was looking: the `.sh` had a contract and
/// the `.ps1` did not, so its lizard branch never checked its exit code and
/// its CRAP gate could not fail. Windows is where this product gets installed,
/// so that was the judge running on the machines that matter.
#[test]
fn the_windows_second_judge_is_a_wrapper_and_not_a_second_opinion() {
    let ps = read("scripts/quality-gate.ps1");

    assert!(
        ps.contains("scripts/quality-gate.sh"),
        "quality-gate.ps1 has to delegate to the shell script rather than reimplement it. Two judges that are supposed to agree only agree for as long as somebody keeps them in step."
    );
    assert!(
        ps.contains("Git/bin/bash.exe"),
        "it must resolve Git Bash explicitly: the bash on PATH under Windows is WSL's, which cannot translate a Windows drive path and exits without reading anything."
    );
    assert!(
        ps.contains("exit $LASTEXITCODE"),
        "a wrapper that swallows the exit code is worse than no wrapper: it reports green for whatever the real judge said."
    );
    for reimplemented in ["cargo mutants", "exclude-re", "lizard -C"] {
        assert!(
            !ps.contains(reimplemented),
            "quality-gate.ps1 mentions {reimplemented}, so it is deciding something the .sh also decides. That is the drift this wrapper exists to make impossible."
        );
    }
}

/// The second judge has to be able to judge, and to fail.
#[test]
fn the_second_judge_can_look_at_a_branch_and_its_crap_gate_can_fail() {
    let q = read("scripts/quality-gate.sh");

    assert!(
        q.contains("QG_BASE"),
        "without a base ref this judges only uncommitted work: run it after committing the change it was meant to judge and it printed SIN DIFF and exited 0, which reads like a pass in a log."
    );
    assert!(
        q.contains("--relative=rust \"$base...HEAD\""),
        "the mutation half has to use the same base as the file list, or the two halves of this judge disagree about what the change is."
    );
    assert!(
        !q.contains("lizard \"$@\") || true"),
        "lizard used to run as (cd .. && lizard \"$@\") || true, with no ceiling, so its exit code carried no information either way and the CRAP gate was printed text."
    );
    assert!(
        q.contains("LIZARD_CC_MAX") && q.contains("lizard -C"),
        "lizard only returns a meaningful exit code when it is given a ceiling."
    );

    let baseline = read("scripts/lizard-baseline.txt");
    let entries = baseline.lines().filter(|l| !l.starts_with('#')).count();
    assert!(
        entries > 50,
        "the complexity baseline lists {entries} functions. Without it a ceiling is unusable on a tree that already has complex ones: touching a single line of a CC 30 function would fail the whole diff, and the first thing anybody would do is put the || true back."
    );
}

/// The banner the gate prints and the doc a reader opens are the same
/// paragraph in two files, and the doc says so in as many words ("copied from
/// the gate banner"). Two copies with nothing holding them together drift the
/// moment one of them is right: somebody makes the gate check something new,
/// updates the banner they can see scrolling past, and the doc keeps promising
/// the old hole to everybody who reads it instead of running it.
fn banner_does_not_check_block() -> String {
    let gate = read("scripts/merge-gate.sh");
    let start = gate
        .find("WHAT THIS GATE DOES NOT CHECK")
        .expect("merge-gate.sh must keep printing what a green run does not prove");
    let rest = &gate[start..];
    let end = rest
        .find("WHAT IT REQUIRES")
        .expect("the not-checked list ends where the requirements begin");
    rest[..end].to_string()
}

fn doc_does_not_check_section() -> String {
    const HEADING: &str = "## What it does **not** check";
    let doc = read("docs/gate.md");
    let start = doc
        .find(HEADING)
        .unwrap_or_else(|| panic!("docs/gate.md must keep the `{HEADING}` section"));
    let rest = &doc[start + HEADING.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn the_gate_banner_and_the_gate_doc_do_not_disagree() {
    let banner = banner_does_not_check_block();
    let section = doc_does_not_check_section();

    let banner_bullets = banner.lines().filter(|line| line.contains('·')).count();
    let doc_titles: Vec<String> = section
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- **"))
        .filter_map(|line| line.split("**").next())
        .map(|title| title.trim_end_matches('.').to_lowercase())
        .collect();

    assert!(
        banner_bullets > 0 && !doc_titles.is_empty(),
        "the scan found {banner_bullets} banner bullet(s) and {} doc bullet(s). A green \
         result from a scan that found nothing proves nothing, and every assertion below \
         would be vacuous",
        doc_titles.len()
    );
    assert_eq!(
        banner_bullets,
        doc_titles.len(),
        "merge-gate.sh lists {banner_bullets} thing(s) it does not check and docs/gate.md \
         lists {}: {doc_titles:?}. The doc says it is copied from the banner, so the two \
         move in the same edit or the copy starts lying — and the copy is the one people \
         read instead of running the gate",
        doc_titles.len()
    );

    let banner_lower = banner.to_lowercase();
    for title in &doc_titles {
        assert!(
            banner_lower.contains(title.as_str()),
            "docs/gate.md warns about `{title}` and the gate banner never mentions it. \
             Whichever of the two is right, the other one is telling somebody the gate \
             covers something it does not"
        );
    }
    assert!(
        section.contains("copied from the gate banner"),
        "the doc has to keep pointing at the banner as its source, or the next reader has \
         no way to know there is a second copy at all"
    );
}

/// The hole this doc used to have was an omission, which is the kind nobody
/// notices: "GPU placement — a CUDA build can still run work on CPU; nothing
/// fails if it does" said what was not checked and never said that a machine
/// without a card cannot check it, so the sentence read like laziness rather
/// than a limit. Now that the gate does assert the decision, both copies have
/// to say which half they cover, and the step has to exist.
#[test]
fn the_gpu_hole_is_declared_and_the_half_that_can_be_checked_is_checked() {
    let suite = read("scripts/run-all-tests.sh");
    let e2e = suite
        .find("tests/e2e_all_tools.py")
        .expect("run-all-tests.sh runs the E2E suite");
    let placement = suite.find("scripts/gpu-placement-check.sh").expect(
        "run-all-tests.sh must run scripts/gpu-placement-check.sh. Without it both the \
         banner and the doc below describe an assertion nobody makes, which is worse than \
         the omission they replaced",
    );
    assert!(
        placement > e2e,
        "the placement check has to run after the E2E: it reads the release binary the E2E \
         drives, and running it first would judge whatever build happened to be lying around"
    );

    for (which, text) in [
        ("scripts/merge-gate.sh", banner_does_not_check_block()),
        ("docs/gate.md", doc_does_not_check_section()),
    ] {
        let lower = text.to_lowercase();
        assert!(
            lower.contains("doctor --json"),
            "{which} has to name what now asserts GPU placement — `doctor --json` — or the \
             claim cannot be checked by the person reading it"
        );
        assert!(
            lower.contains("kernel") && lower.contains("without a card"),
            "{which} has to say which half is still uncovered: that a kernel really executed \
             on the GPU is unprovable without a card, and there is none in CI. An omission is \
             the kind of hole nobody argues with"
        );
        assert!(
            !lower.contains("nothing fails if it does") && !lower.contains("no assertion fails"),
            "{which} still says nothing fails when a CUDA build lands on the CPU. Something \
             does now, and a gate description that undersells itself gets deleted by the next \
             person who checks it"
        );
    }
}

/// The guard that has to be able to fail.
///
/// `gpu-placement-check.sh` decides from two independent readings of the same
/// machine, and on a machine with no card the honest answer is "CPU" — which
/// means the common case is green and a checker that had quietly stopped
/// deciding anything would look exactly the same. Its `--self-test` puts a
/// machine and a doctor answer side by side that contradict each other, one
/// per guard, and reports whether each one was refused.
#[test]
fn the_gpu_placement_check_can_actually_fail() {
    let out = std::process::Command::new(git_bash())
        .args(["scripts/gpu-placement-check.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "gpu-placement-check.sh --self-test did not pass, so at least one of its guards no \
         longer refuses a contradiction. stdout: {stdout}\nstderr: {stderr}"
    );
    // Anchored on the mode's label, not on its sentence: that verdict ends in a
    // comma and carries on over a second line, so half of it was never matched
    // anyway and the half that was is free prose. The script had no label on its
    // success path until this test needed one; it has the same `self-test:` the
    // other two gate scripts print, which is the flag passed in `args` above.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing. stdout: {stdout}"
    );
}

fn looks_absolute(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

fn print_paths(env: &[(&str, &str)]) -> (HashMap<String, String>, String) {
    let mut cmd = std::process::Command::new(git_bash());
    cmd.args(["scripts/run-all-tests.sh", "--print-paths"])
        .current_dir(repo_root())
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CUBA_BINARY_PATH");
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = cmd
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "run-all-tests.sh --print-paths exited {:?}. It resolves paths and exits before it \
         runs anything, so a failure here is the resolution itself.\nstdout: {stdout}\n\
         stderr: {stderr}",
        out.status.code()
    );

    let paths = stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    (paths, stderr)
}

/// Every path this gate hands to another process is absolute, and the test
/// that was meant to guarantee it was a paragraph.
///
/// `gate_bin_finds_the_binary_under_cargo_target_dir` asserts that
/// run-all-tests.sh *contains the string* "CARGO_TARGET_DIR". It did, all
/// along, while the script passed a relative value straight through: cargo
/// resolves that variable against each process's cwd and every cargo call in
/// the gate runs from rust/, so `rust/target-sil` meant `rust/rust/target-sil`
/// for the build (11 GB of it, measured) and `<root>/rust/target-sil` for
/// everything that read it from the repository root. Both existed here and the
/// second one held an older binary.
///
/// The failure it produced names nothing: `[[ -x ]]` accepts a relative path,
/// Python's `os.path.exists()` accepts the same string, and then
/// `subprocess.run()` on Windows refuses to start it with `WinError 2` — forty
/// subprocesses into the E2E. So this drives the real script and reads back
/// what it resolved, rather than looking for a word in its source.
#[test]
fn every_path_the_gate_hands_to_another_process_is_absolute() {
    let (paths, stderr) = print_paths(&[("CARGO_TARGET_DIR", "rust/target-sil")]);
    assert_eq!(
        paths.len(),
        3,
        "--print-paths must answer with CARGO_TARGET_DIR, target_dir and binary. It answered \
         {paths:?}, and a scan that found nothing would pass the loop below without looking \
         at a single path"
    );
    for (key, value) in &paths {
        assert!(
            looks_absolute(value),
            "with CARGO_TARGET_DIR=rust/target-sil the gate resolved {key} to `{value}`. A \
             relative path survives every check bash and Python can make and then cannot be \
             executed at all on Windows, forty subprocesses later.\nstderr: {stderr}"
        );
    }
    assert!(
        stderr.contains("is relative"),
        "a run that silently relocates somebody's build directory is the other half of this \
         bug: whoever set CARGO_TARGET_DIR is entitled to read where it ended up. stderr was: \
         {stderr}"
    );

    let (paths, _) = print_paths(&[("CUBA_BINARY_PATH", "target/release/memory-industry")]);
    let binary = paths
        .get("binary")
        .expect("--print-paths answers with the binary it would hand to the E2E");
    assert!(
        looks_absolute(binary),
        "a caller who exported CUBA_BINARY_PATH by hand got `{binary}` back. The E2E launches \
         that string with subprocess.run(), which is the one caller that cannot cope with a \
         relative path"
    );
}

/// Every `cargo` call in a shell script, as the argument list that follows the
/// word `cargo`.
///
/// Whole-line comments are dropped and backslash continuations joined before
/// anything is read: the call this exists to judge is spread over three
/// physical lines, and the paragraph directly above it names
/// `cargo build --release` in prose, so a scan that looked at physical lines
/// would both miss the call and invent one. Arguments stop at a bare `--`,
/// because everything after it belongs to the test harness rather than to
/// cargo, and at `&&`, `||`, `;` or `|`, because everything after those is a
/// different command.
fn cargo_invocations(script: &str) -> Vec<Vec<String>> {
    let mut logical: Vec<String> = Vec::new();
    let mut pending = String::new();
    for line in script.lines() {
        let trimmed = line.trim();
        if pending.is_empty() && (trimmed.is_empty() || trimmed.starts_with('#')) {
            continue;
        }
        if let Some(head) = trimmed.strip_suffix('\\') {
            pending.push_str(head);
            pending.push(' ');
        } else {
            pending.push_str(trimmed);
            logical.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        logical.push(pending);
    }

    let mut calls: Vec<Vec<String>> = Vec::new();
    for line in &logical {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let mut i = 0;
        while i < tokens.len() {
            if tokens[i] != "cargo" {
                i += 1;
                continue;
            }
            i += 1;
            let mut args: Vec<String> = Vec::new();
            while i < tokens.len() {
                let token = tokens[i];
                if matches!(token, "--" | "&&" | "||" | ";" | "|") {
                    break;
                }
                args.push(token.to_string());
                i += 1;
            }
            calls.push(args);
        }
    }
    calls
}

/// The features a cargo call asks for, sorted so that two calls that ask for
/// the same set compare equal whatever order they were typed in. Order does
/// not change the fingerprint cargo computes, so it must not change the answer
/// here either.
fn features_asked_for(args: &[String]) -> Vec<String> {
    let mut features: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let value = if let Some(rest) = args[i].strip_prefix("--features=") {
            Some(rest)
        } else if args[i] == "--features" {
            i += 1;
            args.get(i).map(String::as_str)
        } else {
            None
        };
        if let Some(value) = value {
            for feature in value.split(',') {
                let feature = feature.trim_matches('"');
                if !feature.is_empty() {
                    features.push(feature.to_string());
                }
            }
        }
        i += 1;
    }
    features.sort();
    features.dedup();
    features
}

/// A release cargo call in the gate carries the features the release build
/// used, or it silently replaces the binary every later step reads.
///
/// `scripts/build-gpu.sh` builds `--release --features cuda` and the E2E and
/// the GPU placement check both run against what it leaves in
/// `CARGO_TARGET_DIR`. Twelve lines under the comment that explains exactly
/// this, the reranker step ran `cargo test --release` with no features at all,
/// into that same directory: the feature set is part of what cargo
/// fingerprints, so cargo saw a different build, rebuilt and relinked.
///
/// The effect lands while compiling, not while running, which is what made it
/// invisible — it was reproduced with `-- --list`, which executes no test
/// whatsoever, and `doctor` went from `ok — cuda · reranker=gpu` to
/// `warn — built without support`. `v017_rerank_gpu` never noticed either: its
/// own expectations are `cfg!(feature = "cuda")`, so they flipped to the CPU
/// answer along with the build and it passed without entering a CUDA branch.
///
/// So each call is extracted and judged on its own, against the feature set
/// build-gpu.sh actually uses rather than against a word hardcoded here. A
/// `contains("--features cuda")` check would have passed on the string sitting
/// in that very comment.
#[test]
fn a_release_cargo_call_in_the_gate_carries_the_features_the_release_build_used() {
    let build = read("scripts/build-gpu.sh");
    let built: Vec<Vec<String>> = cargo_invocations(&build)
        .into_iter()
        .filter(|args| args.iter().any(|arg| arg == "--release"))
        .collect();
    assert!(
        !built.is_empty(),
        "no --release cargo call found in scripts/build-gpu.sh, so there is no feature set \
         to hold the rest of the gate to and every assertion below would be vacuous. Either \
         the release build moved elsewhere or this extraction stopped matching the script"
    );

    let mut expected: Vec<String> = Vec::new();
    for args in &built {
        let features = features_asked_for(args);
        assert!(
            !features.is_empty(),
            "scripts/build-gpu.sh builds release with no features: `cargo {}`. Without \
             --features cuda gpu::wants_gpu() returns false unconditionally and the \
             reranker runs on CPU at 58x the cost — that is the whole reason this script \
             exists instead of a line in the README",
            args.join(" ")
        );
        if expected.is_empty() {
            expected = features;
        } else {
            assert_eq!(
                expected, features,
                "scripts/build-gpu.sh has one release branch under systemd-run and one \
                 without, and they ask for different feature sets. Which binary the gate \
                 gets would then depend on whether systemd is on the machine"
            );
        }
    }

    let suite = read("scripts/run-all-tests.sh");
    let calls = cargo_invocations(&suite);
    let total = calls.len();
    let release: Vec<Vec<String>> = calls
        .into_iter()
        .filter(|args| args.iter().any(|arg| arg == "--release"))
        .collect();
    assert!(
        !release.is_empty(),
        "the scan read {total} cargo call(s) out of scripts/run-all-tests.sh and not one of \
         them was --release. Either the gate stopped exercising the release profile — in \
         which case the E2E and the placement check judge whatever binary was lying around \
         — or this extraction no longer matches how the script is written. A guard that \
         finds nothing to judge is a paragraph"
    );

    for args in &release {
        let features = features_asked_for(args);
        assert_eq!(
            expected,
            features,
            "`cargo {}` runs the release profile asking for {features:?}, while \
             scripts/build-gpu.sh filled the same target directory asking for {expected:?}. \
             cargo fingerprints the feature set, so this call rebuilds and relinks that \
             binary, and everything downstream then reads a build with no CUDA provider \
             compiled in. It happens at compile time, so it happens even when no test runs",
            args.join(" ")
        );
    }
}

/// The half of the second judge that has to be able to fail.
///
/// The CRAP half decides by absence: on a diff that made nothing worse it
/// prints no violation and exits 0, which is also exactly what a filter that
/// stopped filtering looks like — right up to the day it prints a verdict
/// contradicting itself in its own sentence, which is how this was found:
///
/// ```text
/// CRAP: src/service.rs::keys_offered is at CC 6, over the ceiling of 8
/// ```
///
/// Six is not over eight. `lizard -w` also warns on length and on NLOC, and
/// every warning a CCN could be parsed out of was counted as a complexity
/// violation. Its `--self-test` runs a fixture for each shape the filter has to
/// tell apart — the warnings it must swallow whole, the violations it must
/// report with their number — and says whether every direction held. Which
/// fixtures those are is the script's business: counting them here is the same
/// drift the assert below is anchored against.
#[test]
fn the_crap_half_of_the_second_judge_can_actually_fail() {
    let out = std::process::Command::new(git_bash())
        .args(["scripts/quality-gate.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "quality-gate.sh --self-test exited {:?}, so the CRAP half no longer reports CC \
         violations and only CC violations. Exit 2 is its own answer for a missing lizard, \
         which AGENTS.md already counts as a judge that cannot close: install it rather than \
         skip this.\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
    // Anchored on the mode's label, not on its sentence: that last line names the
    // fixtures it ran, so adding one rewrites it — the third fixture landed today,
    // the line wrapped in two, and this assert went red over a script that had
    // passed. Every verdict of this mode, OK or FAIL, carries `self-test:`, and
    // nothing on the normal diff path prints that label.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing, and a script that cannot find \
         its own fixtures exits 0 too. stdout: {stdout}"
    );
}

/// The guard that could not fail, and for the longest of all.
///
/// `swarm-forge.md` says of the green pass: "El codigo, sin tocar los tests:
/// `validar-handoff` compara contra el commit rojo". It did not. Until 0.28
/// this script checked the *shape* of the YAML and nothing else: `commit` only
/// had to look hexadecimal, nobody checked it existed, and nothing compared a
/// single line of test against it. The two-pass protocol rested on the agent
/// being honest rather than on a guard, and two full cycles ran on this branch
/// (dcb97ad->94ff39c and d624f2f->...) without it holding either of them.
///
/// Why this cannot be a text assertion over the script: "you did not touch the
/// tests" is not answerable from git path names here. The crate keeps 112
/// `#[cfg(test)]` blocks inside production files and zero sibling `tests.rs`,
/// so the guard has to extract regions, and an extractor is exactly the kind of
/// thing that keeps returning an empty answer while looking green. Its
/// `--self-test` builds throwaway git repos - a frozen handoff that edited a
/// test, a written one that edited none, a sha that does not exist, a CRLF tree
/// whose blobs are LF - and reports whether each was refused.
#[test]
fn the_two_pass_guard_can_actually_fail() {
    let out = std::process::Command::new(git_bash())
        .args(["scripts/validar-handoff.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "validar-handoff.sh --self-test did not pass, so at least one of its guards no \
         longer refuses a handoff that lies about its tests. stdout: {stdout}\nstderr: {stderr}"
    );
    // Anchored on the mode's label, not on its sentence: the other three asserts
    // of this shape were anchored on prose that named the fixtures, and every one
    // of them went red the day a fixture was added to a script that still passed.
    // `self-test:` is the flag named in `args` above, so renaming the mode forces
    // this test to change with it, and no other line of the script prints it.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing. stdout: {stdout}"
    );
}
