//! Anti-drift for the **local** merge judge only (not GitHub Actions).
//! If someone removes require_present / deny / docs / codigo-muerto from the
//! scripts, this fails before a soft gate can look green again.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

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
        // With its call and flag, not the bare name: the stand-in the gate's own
        // --self-test writes is called test-times.sh too.
        "test-times.sh\" --judge",
        // The same for the judge of the ignored advisories (G6).
        "advisory-exceptions.sh\" --judge",
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

/// Only the scripts that back up and restore the live corpus name its container.
/// `merge-gate.sh` left the list: without `pg_isready` it asks the container that
/// publishes the port of `DATABASE_URL`, whatever that container is called, and
/// a name would judge a server `DATABASE_URL` does not name.
#[test]
fn backup_scripts_name_the_compose_container() {
    let compose = read("docker-compose.yml");
    assert!(
        compose.contains("container_name: memory-industry-db"),
        "docker-compose.yml is the source of the live-cluster name"
    );
    for rel in ["scripts/backup-db.sh", "scripts/restore-db.sh"] {
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
    let out = &judge_env_self_test("codigo-muerto.sh").output;

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

/// A ceiling the judge cannot break is a paragraph.
///
/// `test-times.sh` reads the gate's own log and says the time of every test
/// binary is within its ceiling. A judge whose ceiling never trips, whose floor
/// never trips, or that reads a log it cannot pair and says OK, prints the same
/// line over every run. Its `--self-test` builds the log in both shapes cargo
/// prints (Windows and Linux) and runs the script through its CLI over each
/// guard, and the script reports whether they broke.
#[test]
fn test_times_can_actually_fail() {
    let out = &judge_env_self_test("test-times.sh").output;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "test-times.sh --self-test did not pass, so at least one of its guards no longer \
         fails when it should. stdout: {stdout}\nstderr: {stderr}"
    );
    // Anchored on the mode's label, not on its sentence, like the other self-tests.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing. stdout: {stdout}"
    );
}

/// A list of exceptions the judge cannot break is a paragraph.
///
/// `advisory-exceptions.sh` reads the one list of ignored advisories, the two
/// TOML mirrors of its IDs and a `cargo audit --json` report made without
/// ignores, and says each exception has an owner, a date and an advisory of
/// `Cargo.lock` behind it. A judge that does not refuse an expired line, one that
/// matches nothing, or a mirror that drifted prints the same `OK` over every run.
/// Its `--self-test` runs the script through its CLI over a file built to break
/// each guard, and the script reports whether they broke. The calendar is not
/// read here: the real list expires, and a `cargo test` that judged it would go
/// red by date; the gate judges it.
#[test]
fn advisory_exceptions_can_actually_fail() {
    let out = &judge_env_self_test("advisory-exceptions.sh").output;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "advisory-exceptions.sh --self-test did not pass, so at least one of its guards no \
         longer fails when it should. stdout: {stdout}\nstderr: {stderr}"
    );
    // Anchored on the mode's label, not on its sentence, like the other self-tests.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing. stdout: {stdout}"
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

/// One handoff judge, one implementation — the same drift, the same cure.
///
/// `validar-handoff.ps1` was a port of the `.sh` from before 0.28, and it kept
/// judging shape only: no `commit` that has to exist, no `tests: written|frozen`,
/// no `paths:`. A handoff the `.sh` refuses for editing a frozen test went
/// through it as `OK`, and Windows is where the orchestrator runs.
#[test]
fn the_windows_handoff_judge_is_a_wrapper_and_not_a_second_opinion() {
    let ps = read("scripts/validar-handoff.ps1");

    assert!(
        ps.contains("validar-handoff.sh"),
        "validar-handoff.ps1 has to hand the handoff to the shell script. A copy of its rules \
         agrees with it only until the .sh gains its next guard, and this one missed all three \
         that 0.28 added."
    );
    assert!(
        ps.contains("Git/bin/bash.exe"),
        "it must resolve Git Bash explicitly: the bash on PATH under Windows is WSL's, which \
         cannot translate a Windows drive path and exits without reading anything."
    );
    assert!(
        ps.contains("exit $LASTEXITCODE"),
        "a wrapper that swallows the exit code passes every handoff the real judge refused."
    );
    for reimplemented in ["especificador", "FALTA campo", "0-9a-f", "Get-Content"] {
        assert!(
            !ps.contains(reimplemented),
            "validar-handoff.ps1 mentions {reimplemented}, so it is reading or judging the \
             handoff itself instead of handing it to the .sh. That second opinion is the one \
             that stayed at shape-only while the .sh learned the two-pass protocol."
        );
    }
}

/// Whether a line of shell reads `XDG_CACHE_HOME`, as opposed to setting it for
/// a fixture or unsetting it.
fn reads_xdg_cache_home(line: &str) -> bool {
    line.contains("$XDG_CACHE_HOME") || line.contains("${XDG_CACHE_HOME")
}

/// The gate's own scratch directories, which no binary ever reads: the cargo
/// target and the `TMPDIR` that `gate_linux_env` (gate-lock.sh) sets.
fn is_gate_scratch_dir(line: &str) -> bool {
    line.contains("/cargo-target/") || line.contains("/memory-industry-gate/")
}

/// A line the scan below has to flag: it reads `XDG_CACHE_HOME` and is not one of
/// the gate's own scratch directories.
fn reads_models_cache_under_xdg(line: &str) -> bool {
    reads_xdg_cache_home(line) && !is_gate_scratch_dir(line)
}

/// The exemption of the scan below is for the gate's scratch directories and
/// nothing else: a line that looks for models under `XDG_CACHE_HOME` stays flagged.
#[test]
fn the_xdg_scan_exempts_only_the_gates_scratch_directories() {
    for scratch in [
        r#"export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/cargo-target/memory-industry""#,
        r#"export TMPDIR="${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry-gate/tmp""#,
    ] {
        assert!(
            reads_xdg_cache_home(scratch) && !reads_models_cache_under_xdg(scratch),
            "`{scratch}` is the gate's own scratch directory and must be the one line the scan lets through"
        );
    }
    for models in [
        r#"CACHE_NEW="${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry""#,
        r#"CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/cuba-memorys""#,
        r#"export ONNX_MODEL_PATH="${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry/models""#,
    ] {
        assert!(
            reads_models_cache_under_xdg(models),
            "`{models}` looks for models under XDG_CACHE_HOME and must stay flagged: the exemption is for the gate's scratch directories only"
        );
    }
}

/// The scripts look for the models where the binary keeps them.
///
/// The binary never reads `XDG_CACHE_HOME`: `envs::home()` joins `.cache` to
/// HOME or USERPROFILE, and `models all` / `models runtime` download there.
/// `gpu-placement-check.sh` honoured the variable and was fixed for it in 0.28;
/// `run-all-tests.sh` and `_validate-gate.sh` still did, so on a machine that
/// sets it the gate pointed `ONNX_MODEL_PATH` at a directory the binary never
/// wrote. `run-all-tests.sh --self-test` drives its half for real; this is the
/// net under every script.
///
/// One family of lines is exempt: the gate's own scratch space. `gate-lock.sh`
/// puts the cargo target and `TMPDIR` of a gate run under
/// `${XDG_CACHE_HOME:-$HOME/.cache}`, which is the gate's choice and not a place
/// the binary is expected to look. The exemption names those two directories and
/// nothing else, so a line that looks for models there is still flagged.
#[test]
fn no_script_looks_for_the_cache_where_the_binary_does_not() {
    for read_it in [
        r#"CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/cuba-memorys""#,
        r#"CACHE_NEW="${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry""#,
        r#"cache="$XDG_CACHE_HOME""#,
    ] {
        assert!(
            reads_xdg_cache_home(read_it),
            "the detector missed `{read_it}`, a line that reads the variable, so a clean scan \
             below would prove nothing"
        );
    }
    for set_it in [
        r#"HOME="$scratch/home" XDG_CACHE_HOME="$scratch/xdg" || bad=1"#,
        "unset ORT_DYLIB_PATH HOME USERPROFILE XDG_CACHE_HOME LD_LIBRARY_PATH",
    ] {
        assert!(
            !reads_xdg_cache_home(set_it),
            "`{set_it}` sets or unsets the variable for a fixture, which is how a self-test \
             proves it is ignored; the detector must not count it as a read"
        );
    }

    let mut scanned = Vec::new();
    let mut readers = Vec::new();
    for entry in std::fs::read_dir(repo_root().join("scripts"))
        .expect("scripts/ is readable")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "sh") {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let body = std::fs::read_to_string(&path).expect("readable");
        for (n, line) in body.lines().enumerate() {
            if !line.trim_start().starts_with('#') && reads_models_cache_under_xdg(line) {
                readers.push(format!("{name}:{}: {}", n + 1, line.trim()));
            }
        }
        scanned.push(name);
    }

    for anchor in ["run-all-tests.sh", "gate-lock.sh", "quality-gate.sh"] {
        assert!(
            scanned.iter().any(|s| s == anchor),
            "the scan never read {anchor}, so it is the scan that is broken. Read: {scanned:?}"
        );
    }
    assert!(
        readers.is_empty(),
        "these lines look for the cache under XDG_CACHE_HOME, which the binary never reads. \
         On a machine that sets it they name a directory `models all` never wrote, and the \
         gate measures a model the binary would not load: {readers:#?}"
    );
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
        q.contains("cc_max=\"${LIZARD_CC_MAX:-8}\""),
        "the ceiling has to come from LIZARD_CC_MAX with a default: without one there is no line between a function that is allowed and one that is not."
    );
    assert!(
        q.contains("-v max=\"$cc_max\"") && q.contains("if (ccn[i] <= max + 0) continue"),
        "lizard is run once with --csv and no -C, so its exit code says nothing: the ceiling lives in the awk of lizard_warnings, which has to compare every CCN with cc_max. Take that comparison out and every function in the diff is reported, or none is."
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
    let body = read("scripts/run-all-tests.sh").replace("\r\n", "\n");
    let suite: String = outside_self_test(&body)
        .into_iter()
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
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
    let out = &judge_env_self_test("gpu-placement-check.sh").output;

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

/// One gate at a time, held by something that can refuse.
///
/// On 2026-09-22 the SIL went red with `database "brain_gate" does not exist`
/// in tests/integration.rs, minutes after the gate had created it. An orphaned
/// earlier run had finished and its EXIT trap dropped the database from under
/// the new one. The rule "one gate at a time" was a sentence; a second run is
/// now refused by a lock that names the first, and an exit drops only the
/// databases that still carry its own record. The `--self-test` builds each
/// case with real processes, and ends by launching the script itself as a
/// second gate against a live one, and on its own against a `DATABASE_URL`
/// that `brain_gate` cannot be derived from.
#[test]
fn a_second_gate_is_refused_and_an_exit_drops_only_its_own_databases() {
    let out = &judge_env_self_test("run-all-tests.sh").output;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "run-all-tests.sh --self-test did not pass, so a second gate can start next to a \
         live one again, or an exiting gate can drop a database it did not create. \
         stdout: {stdout}\nstderr: {stderr}"
    );
    // Same anchor as the other self-tests: the mode's label, which only its
    // verdicts print.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. Without the mode, this script \
         ignores the flag and starts a whole gate. stdout: {stdout}"
    );
    // The sentence of the row for a DATABASE_URL it cannot derive brain_gate
    // from, which only that verdict prints.
    assert!(
        stdout.contains("refuses a DATABASE_URL it cannot derive brain_gate from"),
        "the self-test passed without its row for a DATABASE_URL that ends in a query, so \
         nothing shows that a run on its own stops before it provisions its databases on a \
         server the URL does not name. stdout: {stdout}"
    );
}

/// A broken machine scored a perfect kill rate.
///
/// On 2026-09-23 `mutants-gate.sh` printed `3 caught, 51 unviable ...
/// kill_rate=1.000` and the SIL said `MERGE GATE PASSED`. The good run of the
/// same commit was 53 caught, 1 unviable. 44 of those 51 builds never reached
/// the compiler: `cargo.exe` ended with `Failure(-1073741502)`, which is
/// `0xC0000142 STATUS_DLL_INIT_FAILED` — the process could not start under
/// memory pressure. The kill rate excludes unviable mutants, so every mutant
/// the machine could not build left the denominator, and the fewer mutants it
/// managed to judge the better the score looked.
///
/// The other 7 exited 101, like a compile error, and their logs hold no rustc
/// diagnostic: the rustc or link.exe child died of the same status. So the
/// check reads each unviable mutant's build status out of `outcomes.json` and
/// its log, and counts it only when the build exited 1..255 and rustc said why
/// inside the build phase. Its `--self-test` feeds it genuine compile errors,
/// which must pass, and an NTSTATUS, a signal, a timeout, a silent 101, a
/// linker-only 101, a diagnostic outside the build phase, a missing log and a
/// miscounted file, which must not.
#[test]
fn a_build_the_machine_killed_is_not_an_unviable_mutant() {
    // Checked before the script is launched, not after: without the mode the
    // script ignores the flag and starts a real mutation run, which takes 26
    // minutes on the machine this was measured on before it goes red.
    let gate = read("scripts/mutants-gate.sh");
    assert!(
        gate.contains("\"--self-test\"") && gate.contains("\"--check-builds\""),
        "mutants-gate.sh has no --self-test / --check-builds mode, so nothing can judge an \
         outcomes.json without running cargo mutants, and nothing proves the unviable-build \
         check can go red"
    );

    let out = &judge_env_self_test("mutants-gate.sh").output;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "mutants-gate.sh --self-test did not pass, so a mutant whose build the machine killed \
         can be counted as unviable again and leave the kill rate's denominator. \
         stdout: {stdout}\nstderr: {stderr}"
    );
    // Same anchor as the other self-tests: the mode's label.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing. stdout: {stdout}"
    );
}

/// The second judge's mutation step has the same hole: `cargo mutants` exits 0
/// when every mutant it could not build is unviable, whatever stopped the
/// build. It has to hand its outcomes to the same check, not to a copy of it.
#[test]
fn the_second_judge_hands_its_mutation_outcomes_to_the_same_build_check() {
    let q = read("scripts/quality-gate.sh");
    assert!(
        q.contains("scripts/mutants-gate.sh\" --check-builds"),
        "quality-gate.sh runs cargo mutants without judging why its unviable builds failed. \
         A machine out of memory makes every mutant unviable, cargo mutants exits 0 on that, \
         and the diff judge closes over mutants nobody built"
    );
    assert!(
        !q.contains("process_status"),
        "quality-gate.sh reads process_status itself. The rule lives in mutants-gate.sh, whose \
         --self-test proves it can fail; a second copy is the drift quality-gate.ps1 was"
    );
}

/// The lines of `body` that are not between the self-test markers, as
/// `(line number, line)`. A self-test's fixtures name the same files, calls and
/// words as the code they test (a meminfo fixture has a `MemTotal:` line), so a
/// contract that searches the whole script can find a fixture first. Only the code
/// that runs is judged.
fn outside_self_test(body: &str) -> Vec<(usize, &str)> {
    let mut inside = false;
    let mut out = Vec::new();
    for (index, line) in body.lines().enumerate() {
        if line == SELF_TEST_BEGIN {
            inside = true;
        } else if line == SELF_TEST_END {
            inside = false;
        } else if !inside {
            out.push((index + 1, line));
        }
    }
    out
}

/// The lines, outside the self-test, that read the machine's TOTAL memory. Comments
/// may name it (the reason this exists is written in one); a line of code may not.
fn reads_total_memory(body: &str) -> Vec<usize> {
    outside_self_test(body)
        .into_iter()
        .filter(|(_, line)| !line.trim_start().starts_with('#') && line.contains("MemTotal"))
        .map(|(n, _)| n)
        .collect()
}

/// 2026-10-14: the jobs of the mutation step came out of the machine's TOTAL memory
/// (63 GB, 6 jobs) on a VM with about 10 GB free, two builds died in rustc's
/// handle_alloc_error and the judge, rightly, refused: 48 minutes. The rule reads the
/// memory that is available and lives once, in mutants-gate.sh; the second judge asks
/// it. A `MemTotal` in the code of either script is the incident again, and a formula
/// in quality-gate.sh is the drift quality-gate.ps1 was.
#[test]
fn the_mutation_jobs_come_from_the_memory_that_is_available_and_one_script_holds_the_rule() {
    // The scan has to be able to fail before a clean answer from it means anything.
    let code = "x=$(awk '/MemTotal/ {print $2}' /proc/meminfo)\n# MemTotal in a comment\n";
    let fixture = format!("{code}{SELF_TEST_BEGIN}\nprintf 'MemTotal: 1 kB'\n{SELF_TEST_END}\n");
    assert_eq!(
        reads_total_memory(&fixture),
        vec![1],
        "the scan must flag the code line, and only it: not the comment, not the fixture \
         between the markers"
    );

    for script in ["scripts/mutants-gate.sh", "scripts/quality-gate.sh"] {
        let body = read(script).replace("\r\n", "\n");
        assert!(
            reads_total_memory(&body).is_empty(),
            "{script} reads MemTotal outside its self-test (line {:?}). The jobs of the \
             mutation step are sized from the memory that is available: on 2026-10-14 a VM \
             with 63 GB in total and ~10 GB free got 6 jobs and two builds died",
            reads_total_memory(&body)
        );
    }

    let sil = read("scripts/mutants-gate.sh");
    for mode in ["--available-kb", "--jobs-plan", "--default-jobs"] {
        assert!(
            sil.contains(&format!("\"{mode}\")")),
            "mutants-gate.sh has no `{mode}` mode: the rule that sizes the jobs is called \
             through the CLI by the second judge and by its own --self-test"
        );
    }
    let q = read("scripts/quality-gate.sh");
    assert!(
        q.contains("scripts/mutants-gate.sh\" --default-jobs"),
        "quality-gate.sh does not ask mutants-gate.sh for its default jobs, so it keeps a \
         formula of its own: the comment over qg_mutants_jobs said 'same derivation' and \
         that is what drifted"
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

    // CI-F1 (D-F3c): the feature set is the one of the single `--no-run` release call the gate
    // makes, not the one build-gpu.sh asks for. The gate no longer calls build-gpu.sh, and the
    // compile that the E2E judges is that call, so its features are the ones to hold.
    let suite = read("scripts/run-all-tests.sh");
    let suite_code: String = outside_self_test(&suite)
        .iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    let no_run: Vec<Vec<String>> = cargo_invocations(&suite_code)
        .into_iter()
        .filter(|args| {
            args.iter().any(|arg| arg == "--release") && args.iter().any(|arg| arg == "--no-run")
        })
        .collect();
    assert_eq!(
        no_run.len(),
        1,
        "scripts/run-all-tests.sh must make exactly one `--release --no-run` cargo call, outside \
         its self-test, and it makes {}. That call is the one compile of the release binary, so \
         the feature set every other release call and build-gpu.sh asks for is read from it",
        no_run.len()
    );
    let expected: Vec<String> = features_asked_for(&no_run[0]);
    assert!(
        !expected.is_empty(),
        "the `--release --no-run` call of scripts/run-all-tests.sh asks for no features: `cargo {}`. \
         Without --features cuda gpu::wants_gpu() returns false unconditionally and the reranker \
         runs on CPU at 58x the cost",
        no_run[0].join(" ")
    );

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
        assert_eq!(
            expected,
            features,
            "`cargo {}` in scripts/build-gpu.sh asks for {features:?}, while the gate's release \
             build asks for {expected:?}. A person who runs build-gpu.sh has to compile what the \
             judge tested",
            args.join(" ")
        );
    }

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

/// Whether a line of a script calls cargo: a bare `cargo` word in a command, not an argument of
/// echo, printf or a refusal, and not a comment.
fn is_cargo_call_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }
    if matches!(
        trimmed.split_whitespace().next(),
        Some("echo" | "printf" | "gate_refuse")
    ) {
        return false;
    }
    trimmed.split_whitespace().any(|word| word == "cargo")
}

/// CI-F1 (F1.1, D-F1a): merge-gate counts its cores and exports CARGO_BUILD_JOBS before its first
/// cargo call, outside every self-test. Without the export, every cargo call of the gate takes the
/// jobs of the machine, and the cgroup pesado gives it is not what decides how many compile.
#[test]
fn the_gate_exports_cargo_build_jobs_before_its_first_cargo_call() {
    let gate = read("scripts/merge-gate.sh");
    let outside: Vec<&str> = outside_self_test(&gate)
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    let export = outside
        .iter()
        .position(|line| line.trim_start().starts_with("export CARGO_BUILD_JOBS="))
        .expect(
            "scripts/merge-gate.sh never exports CARGO_BUILD_JOBS outside its self-test, so its \
             cargo calls take the jobs of the machine and not the cores the cgroup leaves",
        );
    let first_call = outside
        .iter()
        .position(|line| is_cargo_call_line(line))
        .expect("scripts/merge-gate.sh has no cargo call outside its self-test");
    assert!(
        export < first_call,
        "scripts/merge-gate.sh exports CARGO_BUILD_JOBS on line {} and its first cargo call is on \
         line {}: the export has to come first, or that call compiles with the wrong job count",
        export + 1,
        first_call + 1
    );
}

/// CI-F1 (F1.3): the file a laptop reads by hand keeps its three jobs. The gate's export wins over
/// it (cargo reads the environment before the file), and the file itself is not touched.
#[test]
fn the_laptop_keeps_its_three_jobs() {
    let config = read("rust/.cargo/config.toml");
    assert!(
        config.lines().any(|line| line.trim() == "jobs = 3"),
        "rust/.cargo/config.toml must keep `jobs = 3`: a build run by hand on the laptop reads it"
    );
}

/// CI-F1 (F2.1, D-F2a): the steps that cost seconds run after cargo deny and before the first cargo
/// call that compiles, so a red there arrives before the build and not after it.
#[test]
fn the_cheap_steps_come_before_the_first_build() {
    let gate = read("scripts/merge-gate.sh");
    let code: String = outside_self_test(&gate)
        .iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    let deny = first_code_line(&code, "cargo deny check licenses bans sources")
        .expect("scripts/merge-gate.sh has no cargo deny step outside its self-test");
    let build = first_code_line(&code, "cargo clippy --all-targets")
        .expect("scripts/merge-gate.sh has no clippy call outside its self-test");
    for (step, needle) in [
        ("cargo fmt --check", "cargo fmt --check"),
        ("codigo-muerto.sh", "$ROOT/scripts/codigo-muerto.sh\""),
        ("node npm/install.test.js", "node npm/install.test.js"),
        (
            "run-all-tests.sh --check-models",
            "run-all-tests.sh\" --check-models",
        ),
    ] {
        let at = first_code_line(&code, needle).unwrap_or_else(|| {
            panic!("scripts/merge-gate.sh has no `{step}` step outside its self-test")
        });
        assert!(
            deny < at && at < build,
            "scripts/merge-gate.sh runs `{step}` on line {at}, which is not after cargo deny (line \
             {deny}) and before the first compiling cargo call (line {build}): a red there would \
             arrive after the build"
        );
    }
}

/// CI-F1 (F3.1, D-F3a): the judge builds release once, in one `--release --no-run` call with --lib
/// and --test v017_rerank_gpu and no --bins. No step of run-all-tests.sh runs build-gpu.sh, or puts
/// a systemd-run or a nice in front of cargo: the release compile runs inside the scope pesado gives.
#[test]
fn the_gate_does_not_build_through_build_gpu_or_a_nested_scope() {
    let suite = read("scripts/run-all-tests.sh");
    let code: String = outside_self_test(&suite)
        .iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    for (index, line) in code.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let first = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '"' || c == '\'');
        assert!(
            !first.ends_with("build-gpu.sh") && first != "systemd-run" && first != "nice",
            "scripts/run-all-tests.sh line {} runs `{trimmed}` itself: the judge's release build is \
             the one `--no-run` call, and it runs inside the scope pesado gives, not a nested one",
            index + 1
        );
    }
    let no_run: Vec<Vec<String>> = cargo_invocations(&code)
        .into_iter()
        .filter(|args| {
            args.iter().any(|arg| arg == "--release") && args.iter().any(|arg| arg == "--no-run")
        })
        .collect();
    assert_eq!(
        no_run.len(),
        1,
        "scripts/run-all-tests.sh must make exactly one `--release --no-run` cargo call outside its \
         self-test, and it makes {}",
        no_run.len()
    );
    let args = &no_run[0];
    assert!(
        args.iter().any(|arg| arg == "--lib"),
        "`cargo {}` does not build the lib test unit, and the gpu:: table runs on what it leaves",
        args.join(" ")
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--test" && pair[1] == "v017_rerank_gpu"),
        "`cargo {}` does not build the v017_rerank_gpu test, and that suite runs on what it leaves",
        args.join(" ")
    );
    assert!(
        !args.iter().any(|arg| arg == "--bins"),
        "`cargo {}` asks for --bins, which compiles the binary's own test harness with LTO on top \
         of the bins the judge already has",
        args.join(" ")
    );
}

/// A stand-in on the `PATH` of `run_build_gpu`: written executable, because bash
/// looks a command up by its mode on Linux (Git Bash reads the shebang instead).
fn write_stand_in(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make the stand-in executable");
    }
}

/// Runs a copy of `scripts/build-gpu.sh` in a throwaway tree, with `cargo` and
/// `systemd-run` replaced by the stand-ins given. Returns whether it exited 0,
/// its output, and the lines the stand-in cargo logged (its arguments and
/// whether it ran inside the stand-in scope).
fn run_build_gpu(systemd_run: &str) -> (bool, String, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("mi-build-gpu-{}", uuid::Uuid::new_v4()));
    let bin = dir.join("bin");
    for sub in [&bin, &dir.join("scripts"), &dir.join("rust")] {
        std::fs::create_dir_all(sub).expect("create the throwaway tree");
    }
    std::fs::write(
        dir.join("scripts/build-gpu.sh"),
        read("scripts/build-gpu.sh"),
    )
    .expect("copy build-gpu.sh");
    write_stand_in(&bin, "systemd-run", systemd_run);
    write_stand_in(
        &bin,
        "cargo",
        "#!/usr/bin/env bash\n\
         printf '%s scope=%s\\n' \"$*\" \"${STANDIN_UNDER_SCOPE:-0}\" >> \"$STANDIN_LOG\"\n\
         mkdir -p \"$CARGO_TARGET_DIR/release\"\n\
         printf '#!/usr/bin/env bash\\necho memory-industry stand-in\\n' \
           > \"$CARGO_TARGET_DIR/release/memory-industry\"\n\
         chmod +x \"$CARGO_TARGET_DIR/release/memory-industry\"\n",
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
            .expect("join PATH");
    let log = dir.join("cargo.calls");
    let out = std::process::Command::new(git_bash())
        .arg(dir.join("scripts/build-gpu.sh"))
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env("CUBA_BUILD_JOBS", "1")
        .env("STANDIN_LOG", &log)
        .env_remove("CUBA_BUILD_MEM_MAX")
        .env_remove("STANDIN_UNDER_SCOPE")
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");
    let calls = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text, calls)
}

/// The memory cap is the one thing `systemd-run` was there for, so a machine
/// that cannot give it builds without it, as the script already says it will.
///
/// On 2026-10-21 the first gate in a cloud container (Ubuntu 24.04, no systemd
/// as PID 1) stopped at the release build on `Failed to connect to bus: No
/// medium found`: `systemd-run` is installed there, so `command -v` found it,
/// but there is no user session for `--user --scope` to talk to, and `set -e`
/// ended the gate on the wrapper, before cargo had run at all. WSL2 without
/// `systemd=true` in `wsl.conf` is the same machine. The second row keeps the
/// other half: where a scope can be had, the build still runs inside it, so a
/// script that stopped asking for the cap would be red too.
#[test]
fn a_systemd_run_with_no_session_to_talk_to_builds_without_the_cap_and_one_with_a_session_caps() {
    let no_bus = "#!/usr/bin/env bash\n\
                  echo 'Failed to connect to bus: No medium found' >&2\n\
                  exit 1\n";
    let (ok, text, calls) = run_build_gpu(no_bus);
    assert!(
        ok,
        "build-gpu.sh failed where systemd-run is installed but has no user session to \
         start a scope in, the cloud container of 2026-10-21. It has a branch that builds \
         without the memory cap for a machine without systemd-run; this one is that machine \
         too. {text}"
    );
    assert_eq!(
        calls.len(),
        1,
        "the release build has to run exactly once, outside any scope: {calls:?}\n{text}"
    );
    assert!(
        calls[0].contains("build --release --features cuda") && calls[0].ends_with("scope=0"),
        "the one cargo call was not the uncapped release build: {calls:?}\n{text}"
    );
    assert!(
        text.contains("without a memory cap"),
        "the build ran without its cap and the log does not say so. {text}"
    );

    let scope = "#!/usr/bin/env bash\n\
                 while [[ $# -gt 0 && \"$1\" == -* ]]; do\n\
                 case \"$1\" in -p) shift 2 ;; *) shift ;; esac\n\
                 done\n\
                 STANDIN_UNDER_SCOPE=1 exec \"$@\"\n";
    let (ok, text, calls) = run_build_gpu(scope);
    assert!(
        ok,
        "build-gpu.sh failed with a systemd-run that works. {text}"
    );
    let builds: Vec<&String> = calls.iter().filter(|c| c.starts_with("build ")).collect();
    assert_eq!(
        builds.len(),
        1,
        "the release build has to run exactly once: {calls:?}\n{text}"
    );
    assert!(
        builds[0].ends_with("scope=1"),
        "systemd-run could start a scope and the release build ran outside it, without the \
         memory cap that keeps a fat-LTO link from taking the machine down: {calls:?}\n{text}"
    );
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
/// Six is not over eight. `lizard -w` also warned on length and on NLOC, and
/// every warning a CCN could be parsed out of was counted as a complexity
/// violation; the script now reads the CCN from the `--csv` column and applies
/// the ceiling itself. Its `--self-test` runs a fixture for each shape the filter has to
/// tell apart — the warnings it must swallow whole, the violations it must
/// report with their number — and says whether every direction held. Which
/// fixtures those are is the script's business: counting them here is the same
/// drift the assert below is anchored against.
#[test]
fn the_crap_half_of_the_second_judge_can_actually_fail() {
    let out = &judge_env_self_test("quality-gate.sh").output;

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

/// Each job of publish.yml and the jobs it `needs:`, read off the two-space
/// indented keys under `jobs:`.
fn publish_jobs() -> Vec<(String, Vec<String>)> {
    let yaml = read(".github/workflows/publish.yml");
    let mut jobs: Vec<(String, Vec<String>)> = Vec::new();
    let mut in_jobs = false;
    for line in yaml.lines() {
        if line.starts_with("jobs:") {
            in_jobs = true;
            continue;
        }
        if !in_jobs {
            continue;
        }
        let trimmed = line.trim_start();
        let depth = line.len() - trimmed.len();
        if depth == 2 && !trimmed.starts_with('#') && trimmed.trim_end().ends_with(':') {
            jobs.push((
                trimmed.trim_end().trim_end_matches(':').to_string(),
                Vec::new(),
            ));
        } else if depth == 4 && trimmed.starts_with("needs:") {
            let value = trimmed["needs:".len()..].trim();
            let needs = value
                .trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty());
            if let Some(job) = jobs.last_mut() {
                job.1.extend(needs);
            }
        }
    }
    jobs
}

/// Publishing depends on the local gate, not on GitHub.
///
/// publish.yml used to open with `ci-was-green`, which ran `gh run list
/// --workflow=ci.yml` and refused to publish unless GitHub's CI had passed on
/// the commit: the badge AGENTS.md says is not the judge was the judge of every
/// release. scripts/release.sh now runs merge-gate.sh on the commit main holds
/// and writes `local-gate: MERGE GATE PASSED <sha>` into the annotated tag it
/// pushes; publish.yml reads that line back through the same script. Every
/// other job has to reach that check through `needs:`, or it publishes
/// whatever the tag points at.
#[test]
fn publishing_is_gated_by_the_local_receipt_and_not_by_github_ci() {
    let yaml = read(".github/workflows/publish.yml");
    assert!(
        !yaml.contains("gh run list") && !yaml.contains("ci.yml"),
        "publish.yml asks GitHub about ci.yml again. The release is judged by \
         ./scripts/merge-gate.sh locally, and its receipt travels in the tag"
    );
    assert!(
        yaml.contains("bash scripts/release.sh --verify-receipt \"$TAG\" \"$SHA\""),
        "publish.yml must check the receipt with scripts/release.sh --verify-receipt, the same \
         code whose --self-test proves it accepts what release.sh writes and nothing else"
    );
    assert!(
        yaml.contains("git fetch --no-tags --force origin \"refs/tags/$TAG:refs/tags/$TAG\""),
        "actions/checkout leaves the tag as a lightweight ref on the commit \
         (actions/checkout#290), so the receipt is not in the clone until the annotated tag \
         object is fetched over it with --force"
    );

    let jobs = publish_jobs();
    let gate = "local-gate-receipt";
    assert!(
        jobs.len() > 1 && jobs.iter().any(|(name, _)| name == gate),
        "publish.yml parsed into {jobs:?}. Without the `{gate}` job, or without any job to hold \
         to it, the loop below judges nothing"
    );
    for (name, _) in jobs.iter().filter(|(name, _)| name != gate) {
        let mut seen: Vec<&str> = Vec::new();
        let mut stack: Vec<&str> = vec![name.as_str()];
        while let Some(job) = stack.pop() {
            if seen.contains(&job) {
                continue;
            }
            seen.push(job);
            if let Some((_, needs)) = jobs.iter().find(|(n, _)| n == job) {
                stack.extend(needs.iter().map(String::as_str));
            }
        }
        assert!(
            seen.contains(&gate),
            "publish.yml job `{name}` does not reach `{gate}` through needs:, so it runs whether \
             or not the tag carries the local gate's receipt"
        );
    }
}

/// The release script's guards have to be able to fail.
///
/// Its `--self-test` builds a throwaway origin and clone with a stand-in
/// merge-gate.sh, and drives the real script into each refusal: a dirty tree, a
/// HEAD ahead of or behind origin/main, a tag already taken here or on origin, a
/// version rust/Cargo.toml does not declare, a red gate and an exit 0 over
/// SKIPPED or a failed test run. It then checks that the receipt a green run
/// writes is the one `--verify-receipt` accepts, and that a lightweight tag,
/// another commit and a hand-made annotated tag are refused.
#[test]
fn the_release_script_refuses_what_it_should_and_its_receipt_is_the_one_publish_reads() {
    let out = &judge_env_self_test("release.sh").output;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "release.sh --self-test did not pass, so a release can be tagged past one of its guards \
         or publish.yml cannot read the receipt it writes. stdout: {stdout}\nstderr: {stderr}"
    );
    // Same anchor as the other self-tests: the mode's label.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing. stdout: {stdout}"
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
    let out = &judge_env_self_test("validar-handoff.sh").output;

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

const SELF_TEST_BEGIN: &str = "# --- self-test begin ---";
const SELF_TEST_END: &str = "# --- self-test end ---";

/// Every script of `scripts/` that has a `--self-test` mode, in the order
/// `read_dir` + sort gives. Two-way on purpose: a script that gains a mode, or
/// loses its own, is red until this list says so.
const SCRIPTS_WITH_SELF_TEST: [&str; 14] = [
    "advisory-exceptions.sh",
    "codigo-muerto.sh",
    "crap-gate.sh",
    "gate-audit.sh",
    "gate-cluster.sh",
    "gpu-placement-check.sh",
    "merge-gate.sh",
    "mutants-gate.sh",
    "qg-weekly-audit.sh",
    "quality-gate.sh",
    "release.sh",
    "run-all-tests.sh",
    "test-times.sh",
    "validar-handoff.sh",
];

/// The line that hands control to the self-test: the `if` of a script whose test
/// is a block, or the arm of the `case` of one whose test is a function.
fn dispatches_self_test(line: &str) -> bool {
    let arm = line.trim_start();
    line.contains("== \"--self-test\"")
        || arm.starts_with("--self-test)")
        || arm.starts_with("\"--self-test\")")
}

/// What is wrong with the markers of `body`, as `<name>:<line>: <what to do>`,
/// and whether it has a dispatch line at all. `body` already has LF endings.
fn self_test_marks(name: &str, body: &str) -> (Vec<String>, bool) {
    let mut problems = Vec::new();
    let mut open: Option<usize> = None;
    let mut dispatches = false;
    for (index, line) in body.lines().enumerate() {
        let n = index + 1;
        if line == SELF_TEST_BEGIN {
            match open {
                Some(first) => problems.push(format!(
                    "{name}:{n}: `{SELF_TEST_BEGIN}` opens inside the region of line {first}; \
                     regions do not nest, remove it"
                )),
                None => open = Some(n),
            }
        } else if line == SELF_TEST_END {
            if open.take().is_none() {
                problems.push(format!(
                    "{name}:{n}: `{SELF_TEST_END}` closes nothing; pair it with a begin or remove it"
                ));
            }
        } else if dispatches_self_test(line) {
            dispatches = true;
            if open.is_none() {
                problems.push(format!(
                    "{name}:{n}: dispatches --self-test outside every marked region, so \
                     validar-handoff.sh cannot see its tests; wrap the block or the function, \
                     its helpers and the `case` arm between the two markers, at column 0"
                ));
            }
        }
    }
    if let Some(first) = open {
        problems.push(format!(
            "{name}:{first}: `{SELF_TEST_BEGIN}` has no `{SELF_TEST_END}`; add the end"
        ));
    }
    (problems, dispatches)
}

/// The self-tests of the gate scripts are frozen between the red pass and the
/// green one, and in 0.30 that was done by hand with `sed` and `cmp` on every
/// slice. `validar-handoff.sh` compares the lines between the two markers, but it
/// only sees what a handoff touches. This sees the whole tree on every run: each
/// dispatch of `--self-test` sits inside a pair, no pair is open, nested or
/// orphaned, and the scripts that have the mode are exactly the written list.
#[test]
fn every_gate_script_with_a_self_test_marks_the_whole_of_it() {
    // The scan has to be able to fail before a clean answer from it means anything.
    let honest = format!(
        "{SELF_TEST_BEGIN}\nif [[ \"${{1:-}}\" == \"--self-test\" ]]; then\n  exit 0\nfi\n{SELF_TEST_END}\n"
    );
    let (clean, saw) = self_test_marks("honest.sh", &honest);
    assert!(
        clean.is_empty() && saw,
        "the scan refused a block that is marked and dispatched: {clean:?}"
    );
    let arm_outside = format!(
        "{SELF_TEST_BEGIN}\nself_test() {{ :; }}\n{SELF_TEST_END}\ncase \"$1\" in\n  --self-test) self_test ;;\nesac\n"
    );
    let defects = [
        ("dispatch outside", arm_outside),
        ("begin without end", format!("{SELF_TEST_BEGIN}\ntrue\n")),
        ("end without begin", format!("true\n{SELF_TEST_END}\n")),
        (
            "nested begin",
            format!("{SELF_TEST_BEGIN}\n{SELF_TEST_BEGIN}\n{SELF_TEST_END}\n"),
        ),
    ];
    for (what, body) in &defects {
        let (found, _) = self_test_marks("defect.sh", body);
        assert!(
            !found.is_empty(),
            "the scan let `{what}` through: a check that cannot fail is a paragraph"
        );
    }

    let mut names: Vec<String> = std::fs::read_dir(repo_root().join("scripts"))
        .expect("scripts/ is readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".sh"))
        .collect();
    names.sort();

    let mut problems = Vec::new();
    let mut with_self_test = Vec::new();
    for name in &names {
        let body = read(&format!("scripts/{name}")).replace("\r\n", "\n");
        let (found, dispatches) = self_test_marks(name, &body);
        problems.extend(found);
        if dispatches {
            with_self_test.push(name.clone());
        }
    }
    assert!(
        problems.is_empty(),
        "the self-test of these gate scripts is not marked as a whole, so a handoff \
         that edits it would read as frozen or as written without anyone having compared \
         it: {problems:#?}"
    );
    assert_eq!(
        with_self_test, SCRIPTS_WITH_SELF_TEST,
        "the scripts with a `--self-test` dispatch are not the written list. A new one has to \
         mark its self-test and go on SCRIPTS_WITH_SELF_TEST; one that lost its mode has to \
         come off it. If the patterns stopped matching, this is the anchor that says so"
    );
}

/// The file a dead session reads its answer from has to be the whole gate's
/// verdict, and has to say "running" for as long as there is none.
///
/// `~/.cache/cuba-gate/run.exit` was written by `run-all-tests.sh`, which is
/// only the first half of `merge-gate.sh`. A run whose tests passed wrote `0`
/// there and went on to `cargo deny`, `cargo audit`, `codigo-muerto`,
/// `crap-gate` and `mutants-gate`, any of which could still fail with the file
/// saying 0. And a gate killed halfway — SIGKILL under memory pressure, twice
/// on 2026-09-23 — never reaches a trap, so it left whatever an earlier run had
/// written: an old 0 read as this run's green. `merge-gate.sh --self-test`
/// copies the script into a throwaway tree whose every step is a stand-in, and
/// reads the file after a green run, after a step past the tests fails, after
/// the gate is killed in the middle, and after a second gate is refused. Its
/// rows for the readiness check give a stand-in `pg_isready` each `DATABASE_URL`
/// and read back what the gate asked it, which port it printed and what it
/// refused before asking.
#[test]
fn the_exit_file_holds_the_whole_gates_verdict_and_says_running_until_then() {
    // Checked before the script is launched, not after: without the mode,
    // merge-gate.sh ignores the flag and starts a real gate against the live
    // cluster, backup included.
    let gate = read("scripts/merge-gate.sh");
    assert!(
        gate.contains("\"--self-test\""),
        "merge-gate.sh has no --self-test mode, so nothing shows that the exit file it leaves \
         behind is its own verdict rather than the one its first half wrote"
    );

    let out = &judge_env_self_test("merge-gate.sh").output;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "merge-gate.sh --self-test did not pass, so ~/.cache/cuba-gate/run.exit can say 0 \
         over a gate that failed after its tests, or over one that died in the middle. \
         stdout: {stdout}\nstderr: {stderr}"
    );
    // Same anchor as the other self-tests: the mode's label.
    assert!(
        stdout.contains("self-test:"),
        "the self-test exited 0 without saying it ran. An exit code alone is what let \
         codigo-muerto.sh pass for months while doing nothing. stdout: {stdout}"
    );
    // The sentence of the readiness rows, which only its verdict prints: without
    // it a self-test that lost those rows would still exit 0 and say `self-test:`.
    assert!(
        stdout.contains("the readiness check hands DATABASE_URL to pg_isready -d as it is"),
        "the self-test passed without its rows for the readiness check, so nothing shows that \
         the gate asks libpq about the DATABASE_URL it was given and not about :5488. stdout: \
         {stdout}"
    );
    // The sentences of the rows for the order of the advisory check and for the
    // readiness fallback, in the same way.
    assert!(
        stdout.contains("cargo audit runs first"),
        "the self-test passed without its rows for the order of the steps, so nothing shows \
         that cargo audit judges the RustSec advisories before cargo deny and before the \
         tests, under a ceiling. stdout: {stdout}"
    );
    assert!(
        stdout.contains("the container that publishes the port of DATABASE_URL"),
        "the self-test passed without its rows for the readiness fallback, so nothing shows \
         that, without pg_isready, the gate asks the container that publishes the port of \
         DATABASE_URL and not one picked by its name. stdout: {stdout}"
    );
    // The sentence of the rows for the order of the time judge, in the same way.
    assert!(
        stdout.contains("the test times are judged last"),
        "the self-test passed without its rows for the test times, so nothing shows that the \
         gate judges the time of every test binary last, over its own log, and that a red or \
         an unreadable verdict from it is the gate's. stdout: {stdout}"
    );
}

/// What `gate_require_database_url` has to say to each URL: the URL, the exit
/// code, and the words its refusal must contain. `run-all-tests.sh` derives
/// `brain_gate`, `brain_gate_peer` and `postgres` by replacing what follows the
/// last slash, which only works for a URL that ends in `/<database>`.
/// libpq ends the user and the password at the first `@` and sqlx at the last, so
/// more than one `@` before the host is refused: the readiness check and the
/// binary would ask different servers, and libpq would print the rest of the
/// password as the host. An `@` after the authority (`/br@in`) is not one of them.
const DATABASE_URL_GUARD: [(&str, i32, &[&str]); 23] = [
    ("postgresql://cuba:memorys2026@127.0.0.1:5488/brain", 0, &[]),
    ("postgres://gate:pw0@127.0.0.1:5575/brain", 0, &[]),
    ("postgresql://gate:pw0@db.example.invalid/brain", 0, &[]),
    (
        "postgresql://ga%40te:p%40ss%2Fw%3Frd@127.0.0.1:6543/brain",
        0,
        &[],
    ),
    ("postgresql://gate@%2Frun%2Fpostgresql:5433/brain", 0, &[]),
    ("postgresql://gate:pw0@[::1]:5434/brain", 0, &[]),
    ("postgresql:///brain", 0, &[]),
    (
        "postgresql://gate:S3cr3tW2@/brain?host=%2Frun%2Fpostgresql",
        2,
        &["%2F"],
    ),
    (
        "postgresql://gate:S3cr3tW2@127.0.0.1:5575/brain?sslmode=require",
        2,
        &["%2F"],
    ),
    (
        "postgresql://gate:S3cr3tW2@127.0.0.1:5575",
        2,
        &["%2F", "%3F", "%40"],
    ),
    (
        "postgresql://gate:S3cr3tW2@127.0.0.1:5575/",
        2,
        &["%2F", "%3F", "%40"],
    ),
    (
        "postgresql://gate:S3/cr3tW2@127.0.0.1:5575/brain",
        2,
        &["%2F", "%3F", "%40"],
    ),
    (
        "host=127.0.0.1 port=5575 dbname=brain password=S3cr3tW2",
        2,
        &["postgresql://"],
    ),
    (
        "mysql://gate:S3cr3tW2@127.0.0.1/brain",
        2,
        &["postgresql://"],
    ),
    ("", 2, &["postgresql://"]),
    ("postgresql://127.0.0.1:5575/brain", 0, &[]),
    ("postgresql://gate@127.0.0.1:5575/brain", 0, &[]),
    ("postgresql://gate:pw0@127.0.0.1:5575/br@in", 0, &[]),
    (
        "postgresql://gate:x@S3cr3tW2@127.0.0.1:5575/brain",
        2,
        &["%40", "more than one '@'"],
    ),
    (
        "postgresql://gate:S3cr3tW2@@127.0.0.1:5575/brain",
        2,
        &["%40", "more than one '@'"],
    ),
    (
        "postgresql://ga@S3cr3tW2@127.0.0.1:5575/brain",
        2,
        &["%40", "more than one '@'"],
    ),
    (
        "postgresql://gate:x@S3cr3tW2@[::1]:5434/brain",
        2,
        &["%40", "more than one '@'"],
    ),
    (
        "postgresql://gate:x@S3cr3tW2@%2Frun%2Fpostgresql:5433/brain",
        2,
        &["%40", "more than one '@'"],
    ),
];

/// The head of the script that drives the table: one bash for all of it, since
/// on Windows each costs about 0.1 s. `row N URL` runs the guard in a subshell,
/// so that the `exit 2` of a refusal ends the row and not the script, and prints
/// `ROW|N|exit code|stdout|stderr` with the line breaks turned into `~`. The
/// marker after the call is how a row shows that bash went on after the guard
/// said yes, and that it did not after a no.
const DATABASE_URL_GUARD_SCRIPT: [&str; 8] = [
    r#"source scripts/gate-lock.sh"#,
    r#"d="$(mktemp -d)""#,
    r#"trap 'rm -rf "$d"' EXIT"#,
    r#"row() {"#,
    r#"  local rc=0"#,
    r#"  ( gate_require_database_url "$2" && echo AFTER ) >"$d/out" 2>"$d/err" </dev/null || rc=$?"#,
    r#"  printf 'ROW|%s|%s|%s|%s\n' "$1" "$rc" "$(tr '\n' '~' <"$d/out")" "$(tr '\n' '~' <"$d/err")""#,
    r#"}"#,
];

/// `merge-gate.sh` and a run of `run-all-tests.sh` on its own both refuse a
/// `DATABASE_URL` that `brain_gate` cannot be derived from, with the one
/// function `gate-lock.sh` gives them. A `?host=` that passed the readiness
/// check would send the gate to create `brain_gate` on another server, and a
/// `dbname=` in the query would make the live corpus the throwaway database.
/// This sources `gate-lock.sh` in a bash and calls the function on every row of
/// the table. A refusal is `FAIL: <reason>` and exit 2, with nothing on stdout
/// and no piece of the password in the reason; an accepted URL prints nothing
/// and lets bash go on. The rows that must pass are the presence anchor: a
/// function that does not exist makes bash say 127, which is neither answer.
#[test]
fn the_database_url_guard_refuses_what_the_gate_cannot_derive_brain_gate_from() {
    use std::io::Write;
    use std::process::Stdio;

    let mut script: Vec<String> = DATABASE_URL_GUARD_SCRIPT
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    for (index, (url, _, _)) in DATABASE_URL_GUARD.iter().enumerate() {
        assert!(
            !url.contains('\''),
            "row {index} has a single quote, which the script that drives the table cannot carry"
        );
        script.push(format!("row {index} '{url}'"));
    }
    script.push(String::new());

    let mut child = std::process::Command::new(git_bash())
        .arg("-s")
        .current_dir(repo_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");
    child
        .stdin
        .take()
        .expect("bash was given a pipe for its script")
        .write_all(script.join("\n").as_bytes())
        .expect("hand the script to bash");
    let out = child.wait_with_output().expect("bash finishes");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "the script that drives the table did not run to its end, so the rows below prove \
         nothing. stdout: {stdout}\nstderr: {stderr}"
    );

    let mut answers: Vec<Option<(i32, String, String)>> = vec![None; DATABASE_URL_GUARD.len()];
    for line in stdout.lines() {
        let Some(rest) = line.trim_end_matches('\r').strip_prefix("ROW|") else {
            continue;
        };
        let mut parts = rest.splitn(4, '|');
        let (Some(index), Some(code), Some(said), Some(err)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if let (Ok(index), Ok(code)) = (index.parse::<usize>(), code.parse::<i32>())
            && let Some(slot) = answers.get_mut(index)
        {
            *slot = Some((
                code,
                said.trim_end_matches('~').to_string(),
                err.trim_end_matches('~').to_string(),
            ));
        }
    }

    let mut problems = Vec::new();
    for (index, (url, want, words)) in DATABASE_URL_GUARD.iter().enumerate() {
        let Some((code, said, err)) = &answers[index] else {
            problems.push(format!("`{url}`: the table printed no answer for it"));
            continue;
        };
        if code != want {
            problems.push(format!(
                "`{url}`: exit {code}, not {want} (127 is a function that does not exist). \
                 stdout `{said}`, stderr `{err}`"
            ));
            continue;
        }
        if *want == 0 {
            if said != "AFTER" || !err.is_empty() {
                problems.push(format!(
                    "`{url}` is a URL the gate can use, and the guard printed `{said}` / `{err}` \
                     instead of nothing and letting bash go on"
                ));
            }
            continue;
        }
        if !said.is_empty() {
            problems.push(format!(
                "`{url}`: bash went on after the refusal (`{said}`)"
            ));
        }
        if !err.starts_with("FAIL: ") {
            problems.push(format!("`{url}`: the refusal is `{err}`, not a FAIL line"));
        }
        if err.contains("cr3tW2") || (!url.is_empty() && err.contains(url)) {
            problems.push(format!(
                "`{url}`: the reason carries a piece of the URL: `{err}`"
            ));
        }
        for word in *words {
            if !err.contains(word) {
                problems.push(format!(
                    "`{url}`: the reason does not say `{word}`: `{err}`"
                ));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "gate_require_database_url (scripts/gate-lock.sh) has to let through a URL that ends in \
         /<database> and to refuse, with exit 2 and a reason that carries none of the URL, the \
         ones run-all-tests.sh would cut at the wrong place: {problems:#?}"
    );
}

/// Runs `mutants-gate.sh --check-builds` over an outcomes.json holding one
/// caught mutant whose Test phase ended with `test_status`, and nothing else
/// the check could object to. Returns (exit 0, stdout, stderr).
fn check_builds_on_a_caught_mutant(dir: &Path, test_status: &str) -> (bool, String, String) {
    let file = dir.join(format!("outcomes-{}.json", uuid::Uuid::new_v4()));
    let json = format!(
        r#"{{"outcomes":[{{"scenario":"Baseline","summary":"Success","log_path":"log/baseline.log","phase_results":[{{"phase":"Build","process_status":"Success"}},{{"phase":"Test","process_status":"Success"}}]}},{{"scenario":{{"Mutant":{{"name":"src/search/rrf.rs:9:9: replace f -> u32 with 0"}}}},"summary":"CaughtMutant","log_path":"log/caught.log","phase_results":[{{"phase":"Build","process_status":"Success"}},{{"phase":"Test","process_status":{test_status}}}]}}],"total_mutants":1,"caught":1,"missed":0,"timeout":0,"unviable":0,"success":0,"cargo_mutants_version":"27.1.0"}}"#
    );
    std::fs::write(&file, json).expect("write the outcomes.json fixture");
    // Forward slashes: Git Bash and the Windows Python it hands the file to
    // both read C:/..., and neither has to guess what a backslash means.
    let arg = file.to_string_lossy().replace('\\', "/");
    let out = std::process::Command::new(git_bash())
        .args(["scripts/mutants-gate.sh", "--check-builds", arg.as_str()])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A caught mutant is one its tests caught, not one the machine stopped.
///
/// The unviable half of `--check-builds` exists because a machine out of
/// memory ended 44 builds with `0xC0000142 STATUS_DLL_INIT_FAILED`, and the
/// kill rate leaves unviable mutants out. The caught half had the same hole
/// facing the other way: a mutant whose `cargo test` could not even start ends
/// its Test phase with that same NTSTATUS, cargo-mutants files it as
/// CaughtMutant, and it counts FOR the kill rate — the fewer tests the machine
/// managed to start, the better the score. A genuine catch is a test process
/// that ran to its own exit: 101 when tests fail. The one NTSTATUS that is a
/// genuine catch is `0xC00000FD STATUS_STACK_OVERFLOW`, a mutant that recurses
/// without end and dies of it inside the test process, which is the tests
/// catching it; it is accepted by name, and said so.
///
/// This builds the outcomes.json itself instead of running the script's
/// `--self-test`, so it goes red whatever happens to the script's fixtures.
#[test]
fn a_caught_mutant_whose_test_process_never_ran_is_not_caught() {
    let dir = std::env::temp_dir().join(format!("mi-caught-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    let genuine = check_builds_on_a_caught_mutant(&dir, r#"{"Failure":101}"#);
    let never_started = check_builds_on_a_caught_mutant(&dir, r#"{"Failure":-1073741502}"#);
    let stack_overflow = check_builds_on_a_caught_mutant(&dir, r#"{"Failure":-1073741571}"#);
    let _ = std::fs::remove_dir_all(&dir);

    // The presence anchor: without it, a check that refused every outcome
    // would pass the red case below.
    assert!(
        genuine.0,
        "a caught mutant whose tests failed with 101 was refused. stdout: {}\nstderr: {}",
        genuine.1, genuine.2
    );
    assert!(
        !never_started.0,
        "a caught mutant whose test run ended with 0xC0000142 STATUS_DLL_INIT_FAILED was \
         accepted: cargo could not start, no test ran, and the mutant still counts for the \
         kill rate. stdout: {}\nstderr: {}",
        never_started.1, never_started.2
    );
    assert!(
        never_started.2.contains("0xC0000142"),
        "the refusal did not name the NTSTATUS, so whoever reads it cannot tell a machine out \
         of memory from a test that failed. stderr: {}",
        never_started.2
    );
    assert!(
        stack_overflow.0,
        "a caught mutant that died of 0xC00000FD STATUS_STACK_OVERFLOW was refused. A mutant \
         that recurses without end is caught by its tests' own process. stdout: {}\nstderr: {}",
        stack_overflow.1, stack_overflow.2
    );
    assert!(
        stack_overflow.1.contains("STATUS_STACK_OVERFLOW"),
        "the one NTSTATUS the check accepts as a catch has to be accepted out loud, or the \
         exception reads like a hole. stdout: {}",
        stack_overflow.1
    );
}

/// What the crate and its contracts read back from disk and compare by content,
/// one tracked file per extension. The file is what the checkout below
/// reproduces, so a rule written for a directory instead of an extension is
/// judged on the path that matters.
const READ_BACK_AS_TEXT: [(&str, &str); 12] = [
    ("rs", "rust/src/handlers/faro.rs"),
    ("html", "rust/src/panel/index.html"),
    ("md", "README.md"),
    ("toml", "rust/Cargo.toml"),
    ("json", "server.json"),
    ("jsonl", "rust/eval-datasets/isolation.jsonl"),
    ("yml", ".github/workflows/ci.yml"),
    ("py", "rust/tests/e2e_all_tools.py"),
    ("ps1", "scripts/validar-handoff.ps1"),
    ("service", "packaging/memory-industry.service"),
    ("socket", "packaging/memory-industry.socket"),
    ("example", "packaging/memory-industry.env.example"),
];

fn declares_lf(attributes: &str, pattern: &str) -> bool {
    attributes.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some(pattern) && words.any(|attr| attr == "eol=lf")
    })
}

/// The SIL was red on every fresh clone on Windows while it stayed green in
/// the tree that published 0.27: `core.autocrlf=true` comes from the system
/// gitconfig of Git for Windows, `.gitattributes` pinned `*.sql` and `*.sh`
/// but not `*.rs`, and `the_rerank_budget_starts_before_the_model_is_resolved`
/// looks for a needle with a newline in `include_str!("faro.rs")`. rustc hands
/// the needle a bare LF; the checkout handed the file CRLF.
#[test]
fn the_text_the_crate_reads_back_is_declared_lf() {
    let attributes = read(".gitattributes");
    let missing: Vec<String> = READ_BACK_AS_TEXT
        .iter()
        .map(|(ext, _)| format!("*.{ext}"))
        .filter(|pattern| !declares_lf(&attributes, pattern))
        .collect();
    assert!(
        missing.is_empty(),
        ".gitattributes has no `eol=lf` line for {missing:?}. With core.autocrlf=true, which \
         Git for Windows sets system-wide, a fresh clone checks these out in CRLF, and a test \
         that reads one back and looks for a line break (include_str! of its own source, a \
         contract over a doc or a manifest) goes red on that clone and green on the tree that \
         happened to hold LF"
    );
}

fn git_with_autocrlf(dir: &Path, args: &[&str]) {
    // A git hook exports GIT_DIR and GIT_INDEX_FILE, and with them set these
    // commands would write to the repository the tests run from instead of
    // the scratch one.
    let out = std::process::Command::new("git")
        .args(["-c", "core.autocrlf=true", "-c", "core.safecrlf=false"])
        .args(args)
        .current_dir(dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Reading `.gitattributes` proves a line is there, not that git applies it:
/// a later `-text`, a typo in the pattern or a path rule that shadows it all
/// leave the line in place. This checks out the same paths under this
/// `.gitattributes` with `core.autocrlf=true`, as a fresh clone on Windows
/// does, and reads the bytes back. `checkout-index` rather than `checkout`, so
/// no hook of the machine runs.
#[test]
fn a_fresh_clone_with_autocrlf_checks_out_the_text_the_crate_reads_back_in_lf() {
    // The anchor: an extension no rule names has to come back in CRLF, or the
    // scratch repository is not converting at all and every LF below would be
    // proving nothing.
    const UNRULED: &str = "anchor.unruled";
    let dir = std::env::temp_dir().join(format!("mi-eol-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create the scratch directory");
    git_with_autocrlf(&dir, &["init", "-q"]);
    std::fs::write(dir.join(".gitattributes"), read(".gitattributes"))
        .expect("write .gitattributes");
    let paths: Vec<&str> = READ_BACK_AS_TEXT
        .iter()
        .map(|(_, path)| *path)
        .chain([UNRULED])
        .collect();
    for path in &paths {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().expect("a parent")).expect("create the parent");
        std::fs::write(&file, "first\nsecond\n").expect("write the file");
    }
    git_with_autocrlf(&dir, &["add", "--", "."]);
    for path in &paths {
        std::fs::remove_file(dir.join(path)).expect("remove the file");
    }
    git_with_autocrlf(&dir, &["checkout-index", "--all", "--force"]);
    let crlf: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|path| {
            std::fs::read(dir.join(path))
                .expect("checked out again")
                .windows(2)
                .any(|pair| pair == b"\r\n")
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);

    for (_, path) in READ_BACK_AS_TEXT {
        assert!(
            repo_root().join(path).is_file(),
            "{path} is not in the tree any more: name another file with its extension, or \
             this row checks out a path the repository does not have"
        );
    }
    assert!(
        crlf.contains(&UNRULED),
        "a file no rule of .gitattributes names came back without CRLF under \
         core.autocrlf=true, so this checkout does not convert and cannot tell a rule that \
         works from one that does not"
    );
    let offenders: Vec<&str> = crlf.into_iter().filter(|path| *path != UNRULED).collect();
    assert!(
        offenders.is_empty(),
        "a fresh clone with core.autocrlf=true checks out {offenders:?} in CRLF under this \
         .gitattributes. Anything that reads one of them back and looks for a line break goes \
         red on that clone: that is how the SIL of 0.27 was red on a new Windows clone and \
         green on the tree that published it"
    );
}

/// Scripts that are only ever `source`d and never launched, so the mode bit of
/// the file does not matter.
const SOURCED_ONLY: [&str; 1] = ["gate-lock.sh"];

/// Whether `name`, the path under `scripts/`, is a script somebody launches: a
/// `.sh` or `.py` directly there, not on the `SOURCED_ONLY` list.
fn is_launched_script(name: &str) -> bool {
    !name.contains('/')
        && (name.ends_with(".sh") || name.ends_with(".py"))
        && !SOURCED_ONLY.contains(&name)
}

/// A row of `git ls-files -s` (`<mode> <sha> <stage>\t<path>`) as (mode, path
/// under `scripts/`); none for a row outside `scripts/`.
fn mode_and_script_path(row: &str) -> Option<(&str, &str)> {
    let (meta, path) = row.split_once('\t')?;
    let name = path.strip_prefix("scripts/")?;
    Some((meta.split_whitespace().next()?, name))
}

/// The rows that name a launched script whose mode is not `100755`, as
/// `<mode> scripts/<name>`.
fn not_executable(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(mode_and_script_path)
        .filter(|(mode, name)| is_launched_script(name) && *mode != "100755")
        .map(|(mode, name)| format!("{mode} scripts/{name}"))
        .collect()
}

/// Windows creates every file as 100644 and git records what it finds, so a
/// script written there enters the index non-executable. Git Bash launches it
/// anyway. Linux does not: the gate's own scripts call each other as
/// `"$ROOT/scripts/mutants-gate.sh"`, and the first of those is `Permission
/// denied` at the first step of the gate, with nothing in the message that
/// points at a mode bit. Eight of them were 100644 on 2026-10-03, the day the
/// gate moved to Linux.
#[test]
fn every_gate_entry_point_is_executable_in_the_index() {
    let listing = "\
100644 aaaa 0\tscripts/merge-gate.sh
100755 bbbb 0\tscripts/demo.sh
100644 cccc 0\tscripts/gate-lock.sh
100644 dddd 0\tscripts/create-app-role.sql
100644 eeee 0\tscripts/nested/inner.sh
100644 ffff 0\tscripts/tool.py
100644 9999 0\trust/src/main.rs
";
    assert_eq!(
        not_executable(listing),
        vec![
            "100644 scripts/merge-gate.sh".to_string(),
            "100644 scripts/tool.py".to_string()
        ],
        "the check has to flag a 100644 .sh and .py under scripts/, and leave alone a 100755 \
         one, the sourced-only gate-lock.sh, a .sql, a nested script and anything outside \
         scripts/. A check that flags nothing is the one that passes on the broken index"
    );
    assert!(
        not_executable("100755 bbbb 0\tscripts/demo.sh\n").is_empty(),
        "a 100755 script was flagged"
    );

    let out = std::process::Command::new("git")
        .args(["ls-files", "-s", "--", "scripts"])
        .current_dir(repo_root())
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git has to be reachable: the mode of a file lives in the index, not on disk");
    assert!(
        out.status.success(),
        "git ls-files -s failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout).into_owned();

    let launchable = listing
        .lines()
        .filter(|row| row.ends_with(".sh") || row.ends_with(".py"))
        .count();
    assert!(
        launchable >= 15 && listing.contains("\tscripts/merge-gate.sh"),
        "git ls-files -s listed {launchable} scripts and this scan is only worth reading if it \
         saw the gate: merge-gate.sh among at least 15. Listing: {listing}"
    );
    let wrong = not_executable(&listing);
    assert!(
        wrong.is_empty(),
        "these scripts are not executable in the index: {wrong:?}. Windows creates 100644 and \
         git records it; Git Bash launches such a script anyway, Linux answers `Permission \
         denied` at the first step of the gate that calls it. Fix: `git update-index \
         --chmod=+x <path>`. A script that is only sourced goes on SOURCED_ONLY"
    );
}

/// The lines of `body` that point into a Windows drive as WSL mounts it
/// (`/mnt/c/`, `/mnt/d/`, whatever the case), as `<name>:<line>: <text>`.
fn windows_drive_refs(name: &str, body: &str) -> Vec<String> {
    body.lines()
        .enumerate()
        .filter(|(_, line)| {
            let line = line.to_ascii_lowercase();
            line.contains("/mnt/c/") || line.contains("/mnt/d/")
        })
        .map(|(n, line)| format!("{name}:{}: {}", n + 1, line.trim()))
        .collect()
}

/// `_validate-gate.sh`, `_rerun-gate.sh` and `_finish-and-gate.sh` were left over
/// from running the gate under WSL against a checkout on `D:`. They wrote
/// `~/.local/bin/claude` as an `exec` of a `claude.cmd` under `/mnt/c/Users/...`
/// whenever no `claude` was on the PATH: on a machine with the native `claude`
/// that is a wrapper to a path that does not exist, and the work happens on a
/// 9p mount fifty times slower than ext4. Nothing under `scripts/` points into a
/// Windows drive any more.
#[test]
fn no_gate_script_points_into_a_windows_drive() {
    let strays = windows_drive_refs(
        "fixture.sh",
        "exec /mnt/c/Users/x/claude.cmd \"$@\"\ncd /mnt/d/Proyectos/Memorys\nls /MNT/D/x\n\
         ls /mnt/disk/x /mount/c/y /usr/bin\n",
    );
    assert_eq!(
        strays.len(),
        3,
        "the scan must flag /mnt/c/ and /mnt/d/ in any case and nothing that only looks like \
         them. It flagged {strays:?}"
    );

    let mut read_files = 0;
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(repo_root().join("scripts"))
        .expect("scripts/ is readable")
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let body = String::from_utf8_lossy(&std::fs::read(&path).expect("readable")).into_owned();
        offenders.extend(windows_drive_refs(&name, &body));
        read_files += 1;
    }
    assert!(
        read_files >= 15,
        "the scan read {read_files} files of scripts/: it is the scan that is broken, and a \
         clean answer from it proves nothing"
    );
    assert!(
        offenders.is_empty(),
        "these lines point into a Windows drive. The gate runs on Linux, in ext4, and a path \
         under /mnt/c or /mnt/d is a 9p mount fifty times slower that may not even exist: \
         {offenders:#?}"
    );
}

/// 1-based number of the first line of `script` that is code, not a comment,
/// and contains `needle`.
fn first_code_line(script: &str, needle: &str) -> Option<usize> {
    script
        .lines()
        .position(|line| !line.trim_start().starts_with('#') && line.contains(needle))
        .map(|index| index + 1)
}

/// 1-based number of the first line of `script` that is nothing but a call of
/// `gate_linux_env`. Not "contains": a `--self-test` runs it inside a `bash -c`
/// string, long before the gate's own first step, and a scan that counted that
/// would pass over a script that never calls it.
fn first_bare_call(script: &str) -> Option<usize> {
    script
        .lines()
        .position(|line| line.trim() == "gate_linux_env")
        .map(|index| index + 1)
}

/// Whether `script` sources gate-lock.sh, then calls `gate_linux_env`, and only
/// then reaches `first_step`.
fn sets_up_the_linux_environment_before(script: &str, first_step: &str) -> bool {
    let source = first_code_line(script, r#"source "$ROOT/scripts/gate-lock.sh""#);
    let call = first_bare_call(script);
    let step = first_code_line(script, first_step);
    matches!((source, call, step), (Some(s), Some(c), Some(t)) if s < c && c < t)
}

/// What each entry point of the gate does first, as the text of that line.
const GATE_ENTRY_POINTS: [(&str, &str); 4] = [
    ("scripts/merge-gate.sh", r#"if command -v pg_isready"#),
    (
        "scripts/quality-gate.sh",
        r#"echo "=== exclusions of the mutation step"#,
    ),
    ("scripts/mutants-gate.sh", r#"OUT_DIR="$(mktemp -d"#),
    ("scripts/release.sh", r#"check_the_tree "$tag""#),
];

/// The environment a gate run gets on Linux (the build target and `TMPDIR`
/// outside the tree and off tmpfs, a `node` that is new enough) is set by one
/// function, and it has to be set before the first step that needs it, not
/// halfway: the mutation tool copies the tree into `TMPDIR`, which is tmpfs and
/// so RAM on this machine, and a target inside `rust/` made it copy 27.5 GB.
/// `merge-gate.sh --self-test` drives the function itself; this holds each of
/// the four entry points to calling it.
#[test]
fn every_gate_entry_point_sets_up_the_linux_environment_before_its_first_step() {
    let good = "source \"$ROOT/scripts/gate-lock.sh\"\n  gate_linux_env\ncommand -v pg_isready\n";
    let step = "command -v pg_isready";
    assert!(
        sets_up_the_linux_environment_before(good, step),
        "the check rejected a script that sources gate-lock.sh, calls gate_linux_env and then \
         runs its first step"
    );
    for (what, bad) in [
        (
            "calls it after the first step",
            "source \"$ROOT/scripts/gate-lock.sh\"\ncommand -v pg_isready\ngate_linux_env\n",
        ),
        (
            "mentions it only in a comment",
            "source \"$ROOT/scripts/gate-lock.sh\"\n# gate_linux_env\ncommand -v pg_isready\n",
        ),
        (
            "runs it only inside a bash -c string",
            "source \"$ROOT/scripts/gate-lock.sh\"\nbash -c 'gate_linux_env'\ncommand -v pg_isready\n",
        ),
        (
            "never sources gate-lock.sh",
            "gate_linux_env\ncommand -v pg_isready\n",
        ),
        (
            "never calls it",
            "source \"$ROOT/scripts/gate-lock.sh\"\ncommand -v pg_isready\n",
        ),
        (
            "has no first step to compare with",
            "source \"$ROOT/scripts/gate-lock.sh\"\ngate_linux_env\n",
        ),
    ] {
        assert!(
            !sets_up_the_linux_environment_before(bad, step),
            "the check accepted a script that {what}"
        );
    }

    for (script, first_step) in GATE_ENTRY_POINTS {
        let body = read(script);
        assert!(
            first_code_line(&body, first_step).is_some(),
            "{script} no longer has the line `{first_step}`, so there is nothing to put \
             gate_linux_env before. Name its new first step in GATE_ENTRY_POINTS"
        );
        assert!(
            sets_up_the_linux_environment_before(&body, first_step),
            "{script} must source scripts/gate-lock.sh and call gate_linux_env, on a line of \
             its own, before `{first_step}`. Without it the target directory and TMPDIR are \
             whatever the shell had (the tree, tmpfs) and `node` is whatever is on PATH"
        );
    }
}

/// Runs `scripts/memory-industry-test.sh e2e` with a stand-in `python3` and
/// returns the `CUBA_BINARY_PATH` that every Python process of the E2E was
/// handed. The stand-in is a bash function in the environment
/// (`BASH_FUNC_python3%%`) and not a file: an executable written and launched
/// in a test that runs beside others fails now and then with `Text file busy`,
/// because a child forked by another thread holds the descriptor it was written
/// through.
fn e2e_binary_path(env: &[(&str, &str)]) -> String {
    let mut cmd = std::process::Command::new(git_bash());
    cmd.args(["scripts/memory-industry-test.sh", "e2e"])
        .current_dir(repo_root())
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CUBA_BINARY_PATH")
        .env(
            "BASH_FUNC_python3%%",
            "() { printf 'CUBA_BINARY_PATH=%s\\n' \"$CUBA_BINARY_PATH\"; }",
        );
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
        "memory-industry-test.sh e2e exited {:?} with a stand-in python3.\nstdout: {stdout}\n\
         stderr: {stderr}",
        out.status.code()
    );
    let seen: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("CUBA_BINARY_PATH="))
        .collect();
    assert!(
        !seen.is_empty() && seen.iter().all(|path| *path == seen[0]),
        "the E2E has two Python processes and both have to be handed the same binary. They \
         saw {seen:?}.\nstdout: {stdout}\nstderr: {stderr}"
    );
    seen[0].to_string()
}

/// `memory-industry-test.sh e2e` fixed the binary at `rust/target/release/...`
/// whatever `CARGO_TARGET_DIR` said. On this machine the target lives outside
/// the tree (`gate_linux_env`), so the E2E that this entry point launches would
/// drive whatever stale build is left under `rust/target`, or find none. It has
/// to ask the script that resolves it for the gate (`run-all-tests.sh
/// --print-paths`) instead of keeping a second copy of the rule, and a
/// `CUBA_BINARY_PATH` set by hand still wins.
#[test]
fn the_e2e_entry_point_resolves_the_binary_under_cargo_target_dir() {
    let dir = std::env::temp_dir().join(format!("mi-e2e-target-{}", uuid::Uuid::new_v4()));
    let release = dir.join("release");
    std::fs::create_dir_all(&release).expect("create the scratch target");
    let binary = release.join("memory-industry");
    std::fs::write(&binary, "").expect("write the stand-in binary");
    let target = dir.to_string_lossy().into_owned();

    let by_hand = "/nowhere/by-hand/memory-industry";
    let kept = e2e_binary_path(&[
        ("CARGO_TARGET_DIR", target.as_str()),
        ("CUBA_BINARY_PATH", by_hand),
    ]);
    let found = e2e_binary_path(&[("CARGO_TARGET_DIR", target.as_str())]);
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        kept, by_hand,
        "a CUBA_BINARY_PATH exported by hand has to reach the E2E as it is: the stand-in \
         python3 is the anchor that proves what the script hands over is what is read here"
    );
    // Compared as paths, not strings. On Windows the script prefixes
    // CARGO_TARGET_DIR as it was given (`C:\...\target`) and appends
    // `/release/memory-industry`: both separators name the same file there, and
    // Path's equality compares components, so another directory, another file
    // name or a `.exe` still fails. On Linux it is the string comparison it was.
    assert_eq!(
        Path::new(&found),
        binary.as_path(),
        "with CARGO_TARGET_DIR={target} and a release binary there, the E2E was handed `{found}`, \
         not `{}`. The entry point fixes rust/target/release/memory-industry, a directory the \
         gate on this machine never builds into",
        binary.display()
    );
}

// --- CI-J: no default URL, the judge's server, the eval's database, the counted sections, the banner ---

/// Whether `line` gives `DATABASE_URL` (or `LIVE_DATABASE_URL`) a default for when it is not set:
/// `${DATABASE_URL:-…}`, `${DATABASE_URL-…}`, `:=`, `=`, or a Python `"DATABASE_URL", <default>`
/// argument whose default is neither `None` nor the empty string. `${DATABASE_URL:?…}` is a
/// requirement, not a default, and so is `${DATABASE_URL}` with nothing after it.
fn names_a_default_url(line: &str) -> bool {
    for var in ["DATABASE_URL", "LIVE_DATABASE_URL"] {
        let needle = format!("${{{var}");
        let mut rest = line;
        while let Some(at) = rest.find(&needle) {
            let after = &rest[at + needle.len()..];
            for op in [":-", ":=", "-", "="] {
                if let Some(default) = after.strip_prefix(op) {
                    if !default.split('}').next().unwrap_or("").is_empty() {
                        return true;
                    }
                    break;
                }
            }
            rest = after;
        }
    }
    let key = "\"DATABASE_URL\",";
    let mut rest = line;
    while let Some(at) = rest.find(key) {
        let arg = rest[at + key.len()..].trim_start();
        let no_default = arg.is_empty()
            || arg.starts_with(')')
            || arg.starts_with("None")
            || arg.starts_with("\"\"")
            || arg.starts_with("''");
        if !no_default {
            return true;
        }
        rest = &rest[at + key.len()..];
    }
    false
}

/// The one file and the one line where the live port is spelled as code (D-A11): the definition
/// of `GATE_LIVE_PORT` in `gate-lock.sh`. Every other line that names the port is a defect.
const LIVE_PORT_HOME: &str = "scripts/gate-lock.sh";
const LIVE_PORT_DEFINITION: &str = "GATE_LIVE_PORT=5488";

/// What in `body` (the file `name`) names the live port 5488 or gives DATABASE_URL a default,
/// as `file:line: …`. Comments may name both, and so may the self-test regions of a shell script,
/// whose fixtures carry URLs. Python has no regions, so all of it is code.
fn default_url_defects(name: &str, body: &str) -> Vec<String> {
    let lines: Vec<(usize, &str)> = if name.ends_with(".py") {
        body.lines()
            .enumerate()
            .map(|(i, line)| (i + 1, line))
            .collect()
    } else {
        outside_self_test(body)
    };
    lines
        .into_iter()
        .filter(|(_, line)| !line.trim_start().starts_with('#'))
        .filter_map(|(n, line)| {
            if name == LIVE_PORT_HOME && line.trim() == LIVE_PORT_DEFINITION {
                None
            } else if line.contains("5488") {
                Some(format!(
                    "{name}:{n}: names the live port 5488: {}",
                    line.trim()
                ))
            } else if names_a_default_url(line) {
                Some(format!(
                    "{name}:{n}: gives DATABASE_URL a default: {}",
                    line.trim()
                ))
            } else {
                None
            }
        })
        .collect()
}

/// Every script under `scripts/` (`.sh` and `.py`) and every Python client of `rust/tests/`,
/// relative to the repo root, sorted.
fn judge_scripts() -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if matches!(path.extension().and_then(|e| e.to_str()), Some("sh" | "py")) {
                out.push(relative_to_repo(&path));
            }
        }
    }
    let mut out = Vec::new();
    walk(&repo_root().join("scripts"), &mut out);
    for entry in std::fs::read_dir(repo_root().join("rust/tests"))
        .expect("rust/tests is readable")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("py") {
            out.push(relative_to_repo(&path));
        }
    }
    out.sort();
    out
}

fn relative_to_repo(path: &Path) -> String {
    path.strip_prefix(repo_root())
        .expect("a file of the repo lies under the repo root")
        .to_string_lossy()
        .replace('\\', "/")
}

/// The judge's scripts and clients never name the live port nor default DATABASE_URL: without it
/// they refuse with exit 2 and ask no server. `merge-gate.sh` and the rest of the judge keep
/// their own URL in one place, the one the guard verified.
#[test]
fn no_gate_script_defaults_database_url_and_none_names_the_live_port() {
    let shell = "#!/usr/bin/env bash\n\
                 # a comment may name ${DATABASE_URL:-x} and 5488, it is not code\n\
                 export DATABASE_URL=\"${DATABASE_URL:-postgresql://cuba:pw@127.0.0.1:5488/brain}\"\n";
    assert_eq!(
        default_url_defects("planted.sh", shell).len(),
        1,
        "the scan must flag the planted default and nothing else in its own fixture: {:?}",
        default_url_defects("planted.sh", shell)
    );
    let python = "DATABASE_URL = os.environ.get(\n    \"DATABASE_URL\", \"postgresql://cuba:pw@127.0.0.1:5575/brain\"\n)\n";
    assert_eq!(
        default_url_defects("planted.py", python).len(),
        1,
        "the scan must flag a Python default that sits on the line after os.environ.get( : {:?}",
        default_url_defects("planted.py", python)
    );
    let region = format!(
        "{SELF_TEST_BEGIN}\nDATABASE_URL=\"${{DATABASE_URL:-postgresql://cuba:pw@127.0.0.1:5488/brain}}\"\n{SELF_TEST_END}\n"
    );
    assert!(
        default_url_defects("region.sh", &region).is_empty(),
        "a self-test region may carry a URL fixture; the scan reads it as code: {:?}",
        default_url_defects("region.sh", &region)
    );
    let required = "DATABASE_URL=\"${DATABASE_URL:?set DATABASE_URL to the judge's server}\"\n";
    assert!(
        default_url_defects("required.sh", required).is_empty(),
        "`:?` requires the variable, it does not default it: {:?}",
        default_url_defects("required.sh", required)
    );

    let definition = format!("{LIVE_PORT_DEFINITION}\n");
    assert!(
        default_url_defects(LIVE_PORT_HOME, &definition).is_empty(),
        "the definition of the live port is admitted as the one exact line of gate-lock.sh: {:?}",
        default_url_defects(LIVE_PORT_HOME, &definition)
    );
    assert_eq!(
        default_url_defects(LIVE_PORT_HOME, "PORT=5488\n").len(),
        1,
        "another line of gate-lock.sh that spells the live port is still a defect"
    );
    assert_eq!(
        default_url_defects("scripts/other.sh", &definition).len(),
        1,
        "the same line in another file is still a defect: the exemption is one line of one file"
    );

    let mut defects = Vec::new();
    for name in judge_scripts() {
        defects.extend(default_url_defects(&name, &read(&name)));
    }
    assert!(
        defects.is_empty(),
        "these lines of the judge's scripts and clients name the live port 5488 or default \
         DATABASE_URL. Without DATABASE_URL they must refuse with exit 2 and ask no server: \
         {defects:#?}"
    );
}

/// Whether `line` prints a `DATABASE_URL` with its credentials: `echo`, `printf` or `print(` of the
/// variable itself (`$DATABASE_URL`, `${DATABASE_URL`, `{DATABASE_URL}`), or of a literal URL whose
/// userinfo holds a `:` before its `@`. A parameter expansion that drops the userinfo
/// (`${DATABASE_URL#*@}`, `${DATABASE_URL%%@*}`) is the masking the banner needs, not a hit.
fn prints_a_database_url_with_credentials(line: &str) -> bool {
    let prints = line.contains("echo ") || line.contains("printf ") || line.contains("print(");
    if !prints {
        return false;
    }
    let masked = ["#*@", "##*@", "%@*", "%%@*"]
        .iter()
        .any(|m| line.contains(*m));
    let names_it = [
        "$DATABASE_URL",
        "${DATABASE_URL",
        "{DATABASE_URL}",
        "{DATABASE_URL:",
    ]
    .iter()
    .any(|v| line.contains(*v));
    if names_it && !masked {
        return true;
    }
    if let Some(at) = line.find("://") {
        let rest = &line[at + 3..];
        if let Some(end) = rest.find('@') {
            let userinfo = &rest[..end];
            return userinfo.contains(':') && !userinfo.chars().any(char::is_whitespace);
        }
    }
    false
}

/// No script of the judge prints a `DATABASE_URL` with its password. The banner of the MCP
/// client did, and the password reached the logs of every run. A self-test's fixtures
/// are not code, so the scan reads the code outside them; Python has no regions.
#[test]
fn no_judge_script_prints_a_database_url_with_its_credentials() {
    for (what, line) in [
        ("a shell echo of the variable", "echo \"DB: $DATABASE_URL\""),
        (
            "a Python f-string of the variable",
            "print(f\"DB:     {DATABASE_URL}\")",
        ),
        (
            "a literal URL with a password",
            "echo \"postgresql://gate:pw0@127.0.0.1:5489/brain\"",
        ),
    ] {
        assert!(
            prints_a_database_url_with_credentials(line),
            "the scan let through {what}: `{line}`, which prints the password of DATABASE_URL"
        );
    }
    for (what, line) in [
        (
            "the host, port and database of the variable",
            "echo \"DB: ${DATABASE_URL#*@}\"",
        ),
        (
            "a message that names the variable",
            "echo \"DATABASE_URL is not set\"",
        ),
        (
            "a Python print with no URL",
            "print(\"DB: judge server\", port)",
        ),
        (
            "a read of the variable with no print",
            "DATABASE_URL = os.environ[\"DATABASE_URL\"]",
        ),
    ] {
        assert!(
            !prints_a_database_url_with_credentials(line),
            "the scan flagged {what}, which prints no password: `{line}`"
        );
    }

    let mut defects = Vec::new();
    for name in judge_scripts() {
        let body = read(&name);
        let lines: Vec<(usize, &str)> = if name.ends_with(".py") {
            body.lines()
                .enumerate()
                .map(|(i, line)| (i + 1, line))
                .collect()
        } else {
            outside_self_test(&body)
        };
        for (n, line) in lines {
            if !line.trim_start().starts_with('#') && prints_a_database_url_with_credentials(line) {
                defects.push(format!("{name}:{n}: {}", line.trim()));
            }
        }
    }
    assert!(
        defects.is_empty(),
        "these lines print a DATABASE_URL with its password, and the judge's logs keep what they \
         print. Print host:port/base, which is what the operator needs: {defects:#?}"
    );
}

/// Every file and directory under `dir`, relative to it, sorted.
fn entries_under(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            out.push(relative_to(root, &path));
            if path.is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn relative_to(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Launches of `run_self_test` in this process (CI-J7). With the pid it names each launch's base, so
/// two launches at the same time never share a directory.
static SELF_TEST_LAUNCHES: AtomicUsize = AtomicUsize::new(0);

/// What gate-cluster.sh builds under the TMPDIR of a self-test, after the base (CI-J7): `/t` (the
/// TMPDIR of the run), mktemp's `tmp.XXXXXXXXXX`, the root of the case `noowner` (the longest lib
/// name that reaches gate_cluster_up), the lane `dbg.XXXXXX` and the socket file. 55 bytes.
const SELF_TEST_SOCKET_SUFFIX: &str = "/t/tmp.XXXXXXXXXX/noowner/root/dbg.XXXXXX/.s.PGSQL.5432";

/// A unix socket path holds 107 bytes at most (sun_path), and gate-cluster.sh:155 refuses past it.
const SOCKET_PATH_MAX: usize = 107;

/// Where the base of a launch goes (CI-J7): `/tmp` when it is a directory, because the TMPDIR the
/// test process inherits is not guaranteed short, and `temp_dir()` only where `/tmp` is not one.
fn self_test_parent() -> std::path::PathBuf {
    let tmp = Path::new("/tmp");
    if tmp.is_dir() {
        tmp.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// Runs `script --self-test` with HOME and XDG_CACHE_HOME, each a fresh, empty directory of the
/// run's own, and `env` set on top (CI-J3). The judge's names are removed from the inherited
/// environment first, so a run gets exactly what `env` gives it for them. Returns the run and what
/// it left in those two directories. This environment inherits `XDG_CACHE_HOME` from the login, and
/// `gate_linux_env` puts its temp under it, so an empty HOME alone proves nothing. TMPDIR is
/// removed, as an unset TMPDIR is what the script gets here, unless `own_tmpdir` gives the run an
/// empty TMPDIR of its own, `base/t` (CI-J7). The exit code is not read by the leftovers: what the
/// run left is the answer.
///
/// The base is `self_test_parent()/st-<pid>-<n>`, short on purpose (CI-J7). The socket of the
/// longest case is that base plus `SELF_TEST_SOCKET_SUFFIX`, and the arithmetic is 107 − 70 = 37:
/// with a 7-digit pid (pid_max 4194304 here) and a 3-digit counter, the path is the parent plus 70
/// bytes, so the parent may be at most 37 bytes. `/tmp` is 4, a margin of 33 (the socket is 74).
/// The assert below checks the real base before any launch, so a longer parent is a red of its own
/// that says how many bytes to cut, and not the B5 lane refused by gate-cluster's guard.
fn run_self_test(
    script: &Path,
    env: &[(&str, &str)],
    own_tmpdir: bool,
) -> (std::process::Output, Vec<String>) {
    let launch = SELF_TEST_LAUNCHES.fetch_add(1, Ordering::Relaxed);
    let base = self_test_parent().join(format!("st-{}-{launch}", std::process::id()));
    let longest = base.as_os_str().len() + SELF_TEST_SOCKET_SUFFIX.len();
    assert!(
        longest <= SOCKET_PATH_MAX,
        "the base of this self-test is {} ({} bytes), so the longest socket gate-cluster.sh builds \
         under it is {longest} bytes, over the {SOCKET_PATH_MAX} a unix socket path holds: the temp \
         dir has to be {} bytes shorter, or the B5 lane is refused by its own guard",
        base.display(),
        base.as_os_str().len(),
        longest - SOCKET_PATH_MAX
    );
    let _ = std::fs::remove_dir_all(&base);
    let home = base.join("home");
    let cache = base.join("xdg-cache");
    let tmp = base.join("t");
    std::fs::create_dir_all(&home).expect("creating the empty HOME of the self-test");
    std::fs::create_dir_all(&cache).expect("creating the empty XDG_CACHE_HOME of the self-test");
    std::fs::create_dir_all(&tmp).expect("creating the TMPDIR of the self-test");
    let mut command = std::process::Command::new(git_bash());
    command
        .arg(script)
        .arg("--self-test")
        .current_dir(repo_root())
        .env("HOME", &home)
        .env("XDG_CACHE_HOME", &cache)
        .env_remove("TMPDIR");
    for (name, _) in JUDGE_ENVIRONMENT {
        command.env_remove(name);
    }
    command.envs(env.iter().copied());
    if own_tmpdir {
        command.env("TMPDIR", &tmp);
    }
    let output = command
        .output()
        .expect("running the self-test of a judge script");
    let mut left: Vec<String> = entries_under(&home)
        .into_iter()
        .map(|entry| format!("HOME/{entry}"))
        .collect();
    left.extend(
        entries_under(&cache)
            .into_iter()
            .map(|entry| format!("XDG_CACHE_HOME/{entry}")),
    );
    let _ = std::fs::remove_dir_all(&base);
    (output, left)
}

/// What `script --self-test` leaves in HOME and XDG_CACHE_HOME (D-J7b). See `run_self_test`.
fn left_by_self_test(script: &Path) -> Vec<String> {
    run_self_test(script, &[], false).1
}

/// The default cluster root of gate-cluster.sh, computed by the script itself with GATE_CLUSTER_ROOT
/// unset (CI-F2, K11): `${GATE_CLUSTER_TMP:-/tmp}/memory-industry-gate-<uid>`. `cluster_tmp` is the
/// GATE_CLUSTER_TMP of the computation, so the root it names is one the test owns.
fn default_cluster_root(cluster_tmp: &Path) -> std::path::PathBuf {
    let output = std::process::Command::new(git_bash())
        .arg("-c")
        .arg("source \"$1\"; printf %s \"$GATE_CLUSTER_ROOT\"")
        .arg("default-root")
        .arg(repo_root().join("scripts").join("gate-cluster.sh"))
        .env_remove("GATE_CLUSTER_ROOT")
        .env("GATE_CLUSTER_TMP", cluster_tmp)
        .output()
        .expect("asking gate-cluster.sh for its default cluster root");
    assert!(
        output.status.success(),
        "gate-cluster.sh did not say its default cluster root: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::path::PathBuf::from(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What differs between two listings of a directory (`entries_under`): what went, and what came.
fn listing_changes(before: &[String], after: &[String]) -> Vec<String> {
    before
        .iter()
        .filter(|entry| !after.contains(*entry))
        .map(|entry| format!("gone: {entry}"))
        .chain(
            after
                .iter()
                .filter(|entry| !before.contains(*entry))
                .map(|entry| format!("new: {entry}")),
        )
        .collect()
}

/// Concurrent launches at most (CI-J7). A self-test is a bash script that starts its own processes
/// and mostly waits on them, so the cost is wall time. Six slots serve the fourteen launches of a
/// round without queuing each behind the last, and cap how many of their processes run at once next
/// to the rest of the binary's tests. Chosen here, not measured: the orchestrator measures it in
/// the whole-judge run, and a different number is a change of this constant only.
const SELF_TEST_THREADS: usize = 6;

/// Runs `run` on every name, at most `SELF_TEST_THREADS` at a time, and gives the results back in
/// the order of `names` (CI-J7). A panic in one of the launches is re-raised here, so the test
/// fails with that message and does not abort the binary.
fn in_parallel<'a, T: Send>(names: &[&'a str], run: impl Fn(&'a str) -> T + Sync) -> Vec<T> {
    let slots: Vec<Mutex<Option<T>>> = names.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..SELF_TEST_THREADS.min(names.len()))
            .map(|_| {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&name) = names.get(index) else {
                            break;
                        };
                        let value = run(name);
                        *slots[index].lock().expect(
                            "each slot is written once, by the worker that took its index",
                        ) = Some(value);
                    }
                })
            })
            .collect();
        for worker in workers {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("no launch panicked, so no slot was poisoned")
                .expect("every name was taken by a worker")
        })
        .collect()
}

/// One run of a self-test under the judge's exported environment, with what it left (CI-J7).
struct SelfTestRun {
    output: std::process::Output,
    left: Vec<String>,
}

/// The one run of each script of SCRIPTS_WITH_SELF_TEST under JUDGE_ENVIRONMENT and a TMPDIR of its
/// own (CI-J7). CI-J3 reads all of them, and the ten `*_can_actually_fail` tests read their own
/// script's run from here, so the judge's environment is launched once per script and not once per
/// test. Whichever asks first fills it: the result is the same either way.
static JUDGE_ENV_SELF_TESTS: OnceLock<Vec<(&'static str, SelfTestRun)>> = OnceLock::new();

/// The run of `name` under the judge's exported environment (CI-J7). Its output is what the test
/// asserts on, with the same messages it had when it launched the script itself.
fn judge_env_self_test(name: &str) -> &'static SelfTestRun {
    let runs = JUDGE_ENV_SELF_TESTS.get_or_init(|| {
        let runs = in_parallel(&SCRIPTS_WITH_SELF_TEST, |script_name| {
            let script = repo_root().join("scripts").join(script_name);
            let (output, left) = run_self_test(&script, &JUDGE_ENVIRONMENT, true);
            SelfTestRun { output, left }
        });
        SCRIPTS_WITH_SELF_TEST.iter().copied().zip(runs).collect()
    });
    runs.iter()
        .find(|(known, _)| *known == name)
        .map(|(_, run)| run)
        .unwrap_or_else(|| panic!("{name} is not one of SCRIPTS_WITH_SELF_TEST"))
}

/// The self-tests of the judge's scripts are hermetic (D-J7b). Each script that sources
/// `gate-lock.sh` runs its `--self-test` with an empty HOME and an empty XDG_CACHE_HOME of its
/// own, and leaves both empty, whatever its exit code. The scripts are read from `SCRIPTS_WITH_SELF_TEST`, so a new one
/// that sources the lock is checked without being listed here. The fixture first: a script that
/// writes under HOME has to be caught, or the check is a paragraph.
#[test]
fn a_self_test_of_the_judge_leaves_the_home_it_is_given_as_it_found_it() {
    let liar_dir = std::env::temp_dir().join(format!("selftest-liar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&liar_dir);
    std::fs::create_dir_all(&liar_dir).expect("creating the liar fixture");
    let liar = liar_dir.join("liar.sh");
    std::fs::write(
        &liar,
        "#!/usr/bin/env bash\nmkdir -p \"$HOME/.cache/x\"\nexit 0\n",
    )
    .expect("writing the liar fixture");
    let caught = left_by_self_test(&liar);
    let _ = std::fs::remove_dir_all(&liar_dir);
    assert!(
        !caught.is_empty(),
        "the check let through a script that writes ~/.cache/x under HOME during its self-test, \
         so it cannot tell a clean self-test from a dirty one"
    );

    // D6 (CI-F2): the same for the default cluster root, `${GATE_CLUSTER_TMP:-/tmp}/memory-industry-gate-<uid>`.
    // Hermetic: the real root is where the judge's own lanes live (run_in_lane), so it is never listed. The
    // fixture's liar writes into a root under a GATE_CLUSTER_TMP of its own, and the check must see that.
    let fixture_tmp =
        std::env::temp_dir().join(format!("selftest-root-liar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&fixture_tmp);
    std::fs::create_dir_all(&fixture_tmp)
        .expect("creating the GATE_CLUSTER_TMP of the root fixture");
    let fixture_tmp_text = fixture_tmp
        .to_str()
        .expect("the temp dir of the fixture is UTF-8");
    let fixture_root = default_cluster_root(&fixture_tmp);
    let root_liar = fixture_tmp.join("root-liar.sh");
    std::fs::write(
        &root_liar,
        "#!/usr/bin/env bash\nmkdir -p \"$GATE_CLUSTER_TMP/memory-industry-gate-$(id -u)/liar\"\nexit 0\n",
    )
    .expect("writing the root liar fixture");
    let fixture_before = entries_under(&fixture_root);
    let _ = run_self_test(&root_liar, &[("GATE_CLUSTER_TMP", fixture_tmp_text)], false);
    let fixture_changes = listing_changes(&fixture_before, &entries_under(&fixture_root));
    let _ = std::fs::remove_dir_all(&fixture_tmp);
    assert!(
        !fixture_changes.is_empty(),
        "the root check let through a script that writes under the default cluster root during its \
         self-test, so it cannot tell a clean self-test from one that left a lane behind (D6)"
    );

    let sourcing: Vec<&str> = SCRIPTS_WITH_SELF_TEST
        .iter()
        .copied()
        .filter(|name| read(&format!("scripts/{name}")).contains("gate-lock.sh"))
        .collect();
    assert!(
        !sourcing.is_empty(),
        "no script of SCRIPTS_WITH_SELF_TEST sources gate-lock.sh: the check would prove nothing"
    );
    let cluster_tmp = self_test_parent().join(format!("ct-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cluster_tmp);
    std::fs::create_dir_all(&cluster_tmp).expect("creating the GATE_CLUSTER_TMP of the self-tests");
    let cluster_tmp_text = cluster_tmp
        .to_str()
        .expect("the GATE_CLUSTER_TMP of the self-tests is UTF-8");
    let root = default_cluster_root(&cluster_tmp);
    let root_before = entries_under(&root);
    let lefts = in_parallel(&sourcing, |name| {
        run_self_test(
            &repo_root().join("scripts").join(name),
            &[("GATE_CLUSTER_TMP", cluster_tmp_text)],
            false,
        )
        .1
    });
    let root_changes = listing_changes(&root_before, &entries_under(&root));
    let _ = std::fs::remove_dir_all(&cluster_tmp);
    let leftovers: Vec<String> = sourcing
        .iter()
        .zip(lefts)
        .filter(|(_, left)| !left.is_empty())
        .map(|(name, left)| format!("scripts/{name} --self-test left behind: {left:?}"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "a self-test of the judge wrote into the HOME or the XDG_CACHE_HOME it was given. A \
         self-test has to leave both as it found them (D-J7b), or a judge that runs next to it \
         loses its output: {leftovers:#?}"
    );
    assert!(
        root_changes.is_empty(),
        "a self-test of the judge changed the default cluster root {} (D6, CI-F2): {root_changes:#?}. \
         A self-test that makes a lane has to name GATE_CLUSTER_ROOT, or the lane is left in the root \
         of whoever runs next to it",
        root.display()
    );
}

/// What the judge hands its children in the environment (CI-J3): every name its scripts export
/// outside their self-tests, and CUBA_GATE_EXIT_FILE, which gate-lock.sh reads from the login. Each
/// name carries a value that is not the judge's, so a self-test that reads what it inherits fails
/// here and not under the judge alone. GATE_SERVER_SYSTEM_ID is the identity of the real server: a
/// self-test that takes it as its own fixture's identity is red under any other value.
const JUDGE_ENVIRONMENT: [(&str, &str); 27] = [
    ("DATABASE_URL", "postgresql://ajena:ajena@127.0.0.1:1/ajena"),
    // CI-F3.3b-1b-i (D-F3k): merge-gate.sh exports the lock's directory so that the planner can write the
    // lane tickets under it. gate-lock.sh assigns GATE_LOCK itself, so no self-test reads what it inherits.
    ("GATE_LOCK", "/nonexistent-ajeno/lock"),
    ("GATE_SERVER_SYSTEM_ID", "1"),
    // F2b-fix (D-F2i): the root of the clusters is not exported, and run_in_lane hands it to the step of a
    // lane. A self-test must not inherit a real root: the rows that read it unset it at their start.
    ("GATE_CLUSTER_ROOT", "/nonexistent-ajeno/clusters"),
    // CI-F2 (D-F2i): merge-gate.sh exports the lane of each step. Not exported yet: this entry
    // is red until the code of F2b exports it, and a self-test must not inherit a real lane.
    ("GATE_LANE", "ajena"),
    ("CUBA_GATE_LOCK_OWNER", "ajeno-0000"),
    ("CUBA_GATE_EXIT_FILE", "/nonexistent-ajeno/run.exit"),
    ("CARGO_BUILD_JOBS", "1"),
    ("GATE_CARGO_JOBS_FROM_GATE", "1"),
    ("CARGO_TARGET_DIR", "/nonexistent-ajeno/target"),
    (
        "CUBA_PEER_DATABASE_URL",
        "postgresql://ajena:ajena@127.0.0.1:1/ajena",
    ),
    ("CUBA_BINARY_PATH", "/nonexistent-ajeno/binary"),
    ("CUBA_JUDGE", "ajeno"),
    ("ONNX_MODEL_PATH", "/nonexistent-ajeno/onnx"),
    ("ORT_DYLIB_PATH", "/nonexistent-ajeno/ort.so"),
    ("CUBA_RERANKER_PATH", "/nonexistent-ajeno/reranker"),
    ("CUBA_NLI_PATH", "/nonexistent-ajeno/nli"),
    ("CUBA_EMBEDDING_DIM", "7"),
    ("CUBA_EMBED_MODEL", "ajeno"),
    ("CUBA_POOLING", "ajeno"),
    // CI-J9: the planner hands a step the token of its lane ticket (gate-dag.py, D-F3j). A self-test
    // that inherits it spends or refuses a ticket of a gate it is not part of.
    ("CUBA_GATE_LANE_TOKEN", "ajena-0000"),
    // CI-J9: the planner names these per step (ALLOWED_ENV in gate-dag.py) and the judge does not export
    // them. They get a foreign value, so a self-test that reads one of them fails here, not under the planner.
    ("CUBA_EMBED_INTRA_THREADS", "7"),
    ("CUBA_NLI_INTRA_THREADS", "7"),
    ("CUBA_RERANK_INTRA_THREADS", "7"),
    ("MUTANTS_JOBS", "7"),
    ("PYTHONUTF8", "0"),
    ("PYTHONIOENCODING", "latin-1"),
];

/// The names the judge's scripts export that no self-test is handed, each with its reason, its owner and the
/// day it expires (CI-J3, CI-J13). Excluded names still have to be exported, or the entry is stale; an entry
/// without an owner, or past its day, is red (`the_exceptions_of_the_judge_have_an_owner_and_have_not_expired`).
const NOT_HANDED_DOWN: [(&str, &str, &str, &str); 1] = [(
    "TMPDIR",
    "gate-lock.sh keeps the TMPDIR it is given when it is on disk, and does not check that it \
     exists: a missing one breaks every mktemp of every self-test. The judge does not own a foreign \
     TMPDIR that exists to test with, so there is no value to hand here. Open: whether the judge \
     should refuse a TMPDIR that does not exist (gate-lock.sh, gate_tmpdir_on_disk)",
    "orquestador",
    "2027-02-28",
)];

/// The judge's scripts that export into the environment of what they launch.
const JUDGE_EXPORTERS: [&str; 3] = ["merge-gate.sh", "run-all-tests.sh", "gate-lock.sh"];

/// Whether a self-test passed: it exited 0 and printed its `OK  self-test:` line.
fn self_test_passed(run: &std::process::Output) -> bool {
    run.status.success()
        && String::from_utf8_lossy(&run.stdout)
            .lines()
            .any(|line| line.starts_with("OK  self-test:"))
}

/// The line a failed self-test ends on: its last `FAIL` line, else its last line.
fn failure_line(run: &std::process::Output) -> String {
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let lines: Vec<String> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect();
    lines
        .iter()
        .rev()
        .find(|line| line.contains("FAIL"))
        .or(lines.last())
        .cloned()
        .unwrap_or_else(|| "(no output)".to_string())
}

/// The names a line exports, when the line starts with `export`: `export A=1 B="x y"` gives A and
/// B, and the first word that is neither a name nor an assignment ends the list. A line that only
/// quotes the word (an echo, a comment) exports nothing.
fn exported_names(line: &str) -> Vec<String> {
    let Some(words) = line.trim_start().strip_prefix("export ") else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for word in words.split_whitespace() {
        let name = word.split_once('=').map_or(word, |(name, _)| name);
        let is_name = !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !is_name {
            break;
        }
        names.push(name.to_string());
    }
    names
}

/// Whether `text` mentions `name` as a whole word.
fn names_word(text: &str, name: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|word| word == name)
}

/// CI-J3. The self-tests of the judge's scripts run inside the judge, which hands them its
/// exported environment (JUDGE_ENVIRONMENT). Each one must pass with those values: a self-test that
/// reads an inherited name as its own fixture is a test of the judge's machine, not of the script.
#[test]
fn every_self_test_passes_under_the_judges_exported_environment() {
    // The launcher has to hand the judge's names over, or the passes below prove nothing. A probe
    // that prints its OK line only while GATE_SERVER_SYSTEM_ID is unset passes bare and fails
    // under the judge's names.
    let dir = std::env::temp_dir().join(format!("selftest-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creating the directory of the probe");
    let probe = dir.join("probe.sh");
    std::fs::write(
        &probe,
        "#!/usr/bin/env bash\nif [[ -z \"${GATE_SERVER_SYSTEM_ID:-}\" ]]; then\n  echo \"OK  self-test: probe\"\nfi\n",
    )
    .expect("writing the probe");
    let bare = run_self_test(&probe, &[], false).0;
    let judged = run_self_test(&probe, &JUDGE_ENVIRONMENT, false).0;
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        self_test_passed(&bare) && !self_test_passed(&judged),
        "the launcher does not hand the judge's environment to a self-test, so the passes of the \
         real self-tests prove nothing"
    );

    let mut failed = Vec::new();
    let mut leftovers = Vec::new();
    for name in SCRIPTS_WITH_SELF_TEST {
        let run = judge_env_self_test(name);
        if !self_test_passed(&run.output) {
            failed.push(format!(
                "scripts/{name} exited {:?}: {}",
                run.output.status.code(),
                failure_line(&run.output)
            ));
        }
        if !run.left.is_empty() {
            leftovers.push(format!(
                "scripts/{name} --self-test left behind: {:?}",
                run.left
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "these self-tests do not pass under the judge's exported environment: they inherit it. \
         Unset what the region inherits, at its start (CI-J3): {failed:#?}"
    );
    assert!(
        leftovers.is_empty(),
        "a self-test run under the judge's exported environment wrote into the HOME or the \
         XDG_CACHE_HOME it was given (CI-J3, D-J7b): {leftovers:#?}"
    );
}

/// CI-J3. JUDGE_ENVIRONMENT plus NOT_HANDED_DOWN is every name the judge's scripts export outside
/// their self-tests, and nothing else. A new export without a decision is red, and so is a listed
/// name that the scripts no longer mention, or an excluded one the judge no longer exports.
#[test]
fn the_judges_exported_environment_list_is_complete() {
    // The scan has to be able to fail before a clean answer from it means anything.
    assert_eq!(
        exported_names("    export ZZ_A=1 ZZ_B=\"x y\" # note"),
        vec!["ZZ_A", "ZZ_B"]
    );
    assert_eq!(
        exported_names("export CUBA_BINARY_PATH"),
        vec!["CUBA_BINARY_PATH"]
    );
    assert!(
        exported_names("echo \"or export PATH=<dir>\"").is_empty(),
        "a line that only quotes `export` exports nothing"
    );

    let mut exported: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut code = String::new();
    for file in JUDGE_EXPORTERS {
        let body = read(&format!("scripts/{file}")).replace("\r\n", "\n");
        for (_, line) in outside_self_test(&body) {
            exported.extend(exported_names(line));
            code.push_str(line);
            code.push('\n');
        }
    }
    assert!(
        !exported.is_empty(),
        "no export found in the judge's scripts: the scan is broken"
    );

    let handed: Vec<&str> = JUDGE_ENVIRONMENT.iter().map(|(name, _)| *name).collect();
    let excluded: Vec<&str> = NOT_HANDED_DOWN.iter().map(|(name, ..)| *name).collect();

    let unlisted: Vec<&String> = exported
        .iter()
        .filter(|name| !handed.contains(&name.as_str()) && !excluded.contains(&name.as_str()))
        .collect();
    assert!(
        unlisted.is_empty(),
        "the judge exports these names outside its self-tests and neither JUDGE_ENVIRONMENT nor \
         NOT_HANDED_DOWN lists them. Give each one a foreign value in JUDGE_ENVIRONMENT, or an \
         excluded entry with its reason (CI-J3): {unlisted:#?}"
    );

    // CI-J9: a name the planner sets is not exported by the judge's scripts; the planner's own list is
    // where it is still named, so it is not stale while the planner names it.
    let planner = planner_allowed_names(&read("scripts/gate-dag.py"));
    let stale: Vec<&str> = handed
        .iter()
        .chain(excluded.iter())
        .copied()
        .filter(|&name| !names_word(&code, name) && !planner.iter().any(|p| p.as_str() == name))
        .collect();
    assert!(
        stale.is_empty(),
        "these listed names no longer appear outside the self-tests of the judge's scripts, nor in \
         the planner's ALLOWED_ENV: {stale:?}. Remove them from the list (CI-J3, CI-J9)"
    );

    let not_exported: Vec<&str> = excluded
        .iter()
        .copied()
        .filter(|&name| !exported.contains(name))
        .collect();
    assert!(
        not_exported.is_empty(),
        "these excluded names are not exported by the judge any more, so the exclusion is \
         stale: {not_exported:?}"
    );

    let both: Vec<&str> = handed
        .iter()
        .copied()
        .filter(|&name| excluded.contains(&name))
        .collect();
    assert!(
        both.is_empty(),
        "these names are both handed a foreign value and excluded: {both:?}"
    );
}

/// The names of the planner's ALLOWED_ENV literal in `gate-dag.py` (CI-J9): the closed list of what a step's
/// environment may change (D-F3j). Read from the file, so a name the planner adds is judged without this
/// test changing.
fn planner_allowed_names(dag: &str) -> Vec<String> {
    let Some(start) = dag.find("ALLOWED_ENV = frozenset(") else {
        return Vec::new();
    };
    let literal = &dag[start..];
    let end = literal.find("\n)").unwrap_or(literal.len());
    literal[..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// CI-J9. The planner puts names in a step's environment (ALLOWED_ENV), and the judge hands its children
/// its own (JUDGE_ENVIRONMENT). A name the planner sets that neither list judges is red: the judge would
/// hand a self-test the planner's value of it without anyone deciding so. The other direction is the
/// stale check of CI-J3, which now also counts the planner's list as a place where a name is still named.
#[test]
fn every_name_the_planner_puts_in_a_step_is_judged() {
    // The scan has to be able to fail before a clean answer from it means anything.
    let fixture = "ALLOWED_ENV = frozenset(\n    {\n        \"AA_B\",\n        \"CC\",\n    }\n)\nOTHER = {\"ZZ\"}\n";
    assert_eq!(
        planner_allowed_names(fixture),
        vec!["AA_B".to_string(), "CC".to_string()],
        "the scan of the planner's ALLOWED_ENV must read its names and nothing after the closing paren"
    );

    let planner = planner_allowed_names(&read("scripts/gate-dag.py"));
    assert!(
        planner.len() >= 5,
        "the planner's ALLOWED_ENV has {} names: the scan of gate-dag.py is broken",
        planner.len()
    );

    let handed: Vec<&str> = JUDGE_ENVIRONMENT.iter().map(|(name, _)| *name).collect();
    let excluded: Vec<&str> = NOT_HANDED_DOWN.iter().map(|(name, ..)| *name).collect();
    let unjudged: Vec<&String> = planner
        .iter()
        .filter(|name| !handed.contains(&name.as_str()) && !excluded.contains(&name.as_str()))
        .collect();
    assert!(
        unjudged.is_empty(),
        "the planner puts these names in a step's environment, and neither JUDGE_ENVIRONMENT nor \
         NOT_HANDED_DOWN says what the judge does with them (CI-J9). Give each one a foreign value in \
         JUDGE_ENVIRONMENT, or an excluded entry with its reason: {unjudged:#?}"
    );
}

/// CI-J5. The seven names that choose the models are set in one place, `gate_model_env` (gate-lock.sh),
/// and both callers run it: run-all-tests.sh for the full run and merge-gate.sh before the
/// --features docs step. A second assignment is a second source, and the docs step would test other
/// models than the full run.
const MODEL_ENVIRONMENT: [&str; 7] = [
    "ONNX_MODEL_PATH",
    "ORT_DYLIB_PATH",
    "CUBA_RERANKER_PATH",
    "CUBA_NLI_PATH",
    "CUBA_EMBEDDING_DIM",
    "CUBA_EMBED_MODEL",
    "CUBA_POOLING",
];

/// The scripts that set a model name outside gate_model_env, each with its reason, its owner and the day it
/// expires (CI-J5, CI-J13). An excluded script still has to set one, or its entry is stale.
const MODEL_NAMES_ELSEWHERE: [(&str, &str, &str, &str); 1] = [(
    "demo.sh",
    "a presentation of the binary: it picks its own cache, and no gate runs it (record-demo-gif.sh does)",
    "orquestador",
    "2027-02-28",
)];

/// The model names a line of shell sets: an export, or a default `NAME="${NAME:-...}"`. A prefix
/// assignment for one command (`ONNX_MODEL_PATH="" \`) sets nothing for the gate and is not one.
fn sets_model_names(line: &str) -> Vec<&'static str> {
    let exported = exported_names(line);
    let trimmed = line.trim_start();
    MODEL_ENVIRONMENT
        .iter()
        .copied()
        .filter(|name| {
            exported.iter().any(|e| e.as_str() == *name)
                || (trimmed.starts_with(&format!("{name}="))
                    && trimmed.contains(&format!("${{{name}:-")))
        })
        .collect()
}

/// CI-J5. The model names are set once, in gate_model_env, and the two callers run it. The detector
/// is judged first, on lines that must and must not count.
#[test]
fn the_model_environment_is_set_in_one_place() {
    assert_eq!(
        sets_model_names(r#"    export ONNX_MODEL_PATH="${ONNX_MODEL_PATH:-$CACHE/models}""#),
        vec!["ONNX_MODEL_PATH"]
    );
    assert_eq!(
        sets_model_names(r#"CUBA_POOLING="${CUBA_POOLING:-cls}""#),
        vec!["CUBA_POOLING"]
    );
    for not_a_set in [
        r#"if [[ "$CUBA_EMBEDDING_DIM" != "384" ]]; then"#,
        r#"    ONNX_MODEL_PATH="" \"#,
        r#"DATABASE_URL="$GATE_DATABASE_URL" CUBA_APP_ROLE=0 ONNX_MODEL_PATH="" \"#,
    ] {
        assert!(
            sets_model_names(not_a_set).is_empty(),
            "`{not_a_set}` sets no model name for the gate, so it must not be counted"
        );
    }

    let gate_lock = read("scripts/gate-lock.sh").replace("\r\n", "\n");
    let lines: Vec<&str> = gate_lock.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with("gate_model_env() {"))
        .expect("scripts/gate-lock.sh has no gate_model_env: the model environment is set nowhere");
    let end = start
        + lines[start..]
            .iter()
            .position(|line| *line == "}")
            .expect("gate_model_env has no closing brace at column 0");

    let mut scripts: Vec<String> = std::fs::read_dir(repo_root().join("scripts"))
        .expect("scripts/ is readable")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".sh"))
        .collect();
    scripts.sort();

    let mut inside: Vec<&str> = Vec::new();
    let mut outside: Vec<String> = Vec::new();
    let mut seen_elsewhere: Vec<String> = Vec::new();
    for name in &scripts {
        let body = read(&format!("scripts/{name}")).replace("\r\n", "\n");
        for (n, line) in outside_self_test(&body) {
            let names = sets_model_names(line);
            if names.is_empty() {
                continue;
            }
            let at = n - 1;
            if name == "gate-lock.sh" && at > start && at < end {
                inside.extend(names);
            } else if MODEL_NAMES_ELSEWHERE
                .iter()
                .any(|(file, ..)| *file == name.as_str())
            {
                seen_elsewhere.push(name.clone());
            } else {
                outside.push(format!("scripts/{name}:{n}: {}", line.trim()));
            }
        }
    }
    assert!(
        outside.is_empty(),
        "these lines set a model name outside gate_model_env, a second source for the models of \
         the gate (CI-J5): {outside:#?}"
    );
    let missing: Vec<&str> = MODEL_ENVIRONMENT
        .iter()
        .copied()
        .filter(|name| !inside.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "gate_model_env does not set these names: {missing:?} (CI-J5)"
    );
    for (file, ..) in MODEL_NAMES_ELSEWHERE {
        assert!(
            seen_elsewhere.iter().any(|seen| seen.as_str() == file),
            "scripts/{file} no longer sets a model name, so its entry in MODEL_NAMES_ELSEWHERE is \
             stale (CI-J5)"
        );
    }

    for caller in ["run-all-tests.sh", "merge-gate.sh"] {
        let body = read(&format!("scripts/{caller}")).replace("\r\n", "\n");
        assert!(
            outside_self_test(&body)
                .iter()
                .any(|(_, line)| line.trim() == "gate_model_env"),
            "scripts/{caller} does not call gate_model_env, so it runs with models the gate did not \
             choose (CI-J5)"
        );
    }
}

/// The section counts come from one counter, `test-times.sh --count` (D-J4c, D-J4d): the script
/// reads no `test result:` line itself, so there is one parse of a cargo log, not two.
#[test]
fn the_section_counts_come_from_test_times_count_alone() {
    let body = read("scripts/run-all-tests.sh");
    let code: Vec<(usize, &str)> = outside_self_test(&body)
        .into_iter()
        .filter(|(_, line)| !line.trim_start().starts_with('#'))
        .collect();
    let own: Vec<String> = code
        .iter()
        .filter(|(_, line)| line.contains("test result"))
        .map(|(n, line)| format!("{n}: {}", line.trim()))
        .collect();
    assert!(
        own.is_empty(),
        "run-all-tests.sh parses `test result:` lines by itself, a second parser of the cargo \
         log next to test-times.sh: {own:#?}"
    );
    assert!(
        code.iter()
            .any(|(_, line)| line.contains("test-times.sh") && line.contains("--count")),
        "run-all-tests.sh does not count its sections with `scripts/test-times.sh --count`, the one \
         counter of the judge"
    );
}

/// The Python clients refuse without `DATABASE_URL` before they launch the binary: with
/// `CUBA_BINARY_PATH` pointing at a stand-in that leaves a mark, the mark must not appear, and
/// they exit 2 naming `DATABASE_URL`. Unix only: the stand-in is a shell script.
#[cfg(unix)]
#[test]
fn the_python_clients_refuse_without_database_url() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("ci-j-j17-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a scratch directory for the stand-in");
    let binary = dir.join("stand-in-binary");
    let mark = dir.join("ran");
    std::fs::write(
        &binary,
        format!("#!/bin/sh\necho ran >\"{}\"\nexit 0\n", mark.display()),
    )
    .expect("writing the stand-in binary");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
        .expect("making the stand-in executable");

    for client in [
        "rust/tests/e2e_all_tools.py",
        "scripts/mcp_live_session_test.py",
    ] {
        let _ = std::fs::remove_file(&mark);
        let out = Command::new("python3")
            .arg(repo_root().join(client))
            .env_remove("DATABASE_URL")
            .env("CUBA_BINARY_PATH", &binary)
            .current_dir(&dir)
            .output()
            .expect("python3 must be on PATH to run the clients' contract");
        assert!(
            !mark.exists(),
            "{client} launched the binary with DATABASE_URL unset: it defaults the URL to the live port"
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "{client} without DATABASE_URL must exit 2; stdout: {} stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            said.contains("DATABASE_URL"),
            "{client} refused without saying which variable is missing: {said}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The first line whose trimmed text satisfies `pred`, as its number in the file.
fn first_line_where(lines: &[(usize, String)], pred: impl Fn(&str) -> bool) -> Option<usize> {
    lines
        .iter()
        .find(|(_, line)| pred(line.as_str()))
        .map(|(n, _)| *n)
}

/// `merge-gate.sh` judges the URL before the first step that uses it, assigns it once, and hands
/// that one value to its children; the children that take the URL on their own call the same
/// guard. The anchors are the step headers and calls that the gate prints and runs.
#[test]
fn every_gate_step_after_the_guard_gets_the_verified_url() {
    let gate_body = read("scripts/merge-gate.sh");
    let gate: Vec<(usize, String)> = outside_self_test(&gate_body)
        .into_iter()
        .map(|(n, line)| (n, line.trim().to_string()))
        .filter(|(_, line)| !line.starts_with('#'))
        .collect();

    let guard = first_line_where(&gate, |l| l.contains("gate_require_judge_server"));
    let audit = first_line_where(&gate, |l| l.contains("=== cargo audit ==="));
    let suite = first_line_where(&gate, |l| {
        l.starts_with("\"$ROOT/scripts/run-all-tests.sh\"")
    });
    let (Some(guard), Some(audit), Some(suite)) = (guard, audit, suite) else {
        panic!(
            "merge-gate.sh must call gate_require_judge_server (found on {guard:?}), print \
             `=== cargo audit ===` ({audit:?}) and call run-all-tests.sh ({suite:?}) outside its \
             self-test"
        );
    };
    assert!(
        guard < audit && guard < suite,
        "merge-gate.sh judges the server at line {guard}, after cargo audit ({audit}) or \
         run-all-tests.sh ({suite}): a step would have used a URL nobody verified"
    );

    let assignments: Vec<usize> = gate
        .iter()
        .filter(|(_, l)| l.starts_with("export DATABASE_URL=") || l.starts_with("DATABASE_URL="))
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(
        assignments.len(),
        1,
        "merge-gate.sh assigns DATABASE_URL on lines {assignments:?}. The verified URL must be the \
         only value the parent hands on, so one assignment"
    );
    assert!(
        assignments[0] < suite,
        "the verified URL is exported at line {} and run-all-tests.sh is called at {suite}: the \
         child would not inherit it",
        assignments[0]
    );
    assert!(
        gate.iter()
            .any(|(_, l)| l.starts_with("export GATE_SERVER_SYSTEM_ID=")),
        "merge-gate.sh does not export GATE_SERVER_SYSTEM_ID, the system_identifier it verified, \
         so run-all-tests.sh has nothing to compare against"
    );

    for script in [
        "scripts/crap-gate.sh",
        "scripts/mutants-gate.sh",
        "scripts/run-all-tests.sh",
    ] {
        let body = read(script);
        let calls_guard = outside_self_test(&body).iter().any(|(_, l)| {
            !l.trim_start().starts_with('#') && l.contains("gate_require_judge_server")
        });
        assert!(
            calls_guard,
            "{script} runs cargo with whatever DATABASE_URL it inherits and does not call \
             gate_require_judge_server; with DATABASE_URL set it must judge it as merge-gate does"
        );
    }

    // A2 (CI-J2, D-J1c): the comparison with GATE_SERVER_SYSTEM_ID lives in
    // gate_require_judge_server, the one place that reads the identity. Every step that calls the
    // guard then judges the server merge-gate.sh read, and none keeps its own copy of the test.
    let lock_lines: Vec<String> = read("scripts/gate-lock.sh")
        .lines()
        .map(str::to_string)
        .collect();
    let start = lock_lines
        .iter()
        .position(|l| l.starts_with("gate_require_judge_server() {"))
        .expect("scripts/gate-lock.sh has no gate_require_judge_server() function");
    let end = start
        + lock_lines[start..]
            .iter()
            .position(|l| l == "}")
            .expect("gate_require_judge_server() in scripts/gate-lock.sh is not closed");
    let guard_body = &lock_lines[start..=end];
    assert!(
        guard_body
            .iter()
            .any(|l| l.contains("GATE_SERVER_SYSTEM_ID")),
        "gate_require_judge_server (scripts/gate-lock.sh) does not read GATE_SERVER_SYSTEM_ID: a \
         server that changed under the run would go unseen by every step that calls the guard"
    );
    assert!(
        guard_body
            .iter()
            .any(|l| l.contains("GATE_SERVER_SYSTEM_ID") && l.contains("!=")),
        "gate_require_judge_server (scripts/gate-lock.sh) does not compare GATE_SERVER_SYSTEM_ID \
         with the system_identifier it reads"
    );
    assert!(
        guard_body.iter().any(|l| l.contains("the server changed")),
        "gate_require_judge_server (scripts/gate-lock.sh) does not stop with `the server changed` \
         when the system_identifier it reads differs from the one merge-gate.sh exported"
    );

    // merge-gate.sh asks again after run-all-tests.sh, before the --features docs step: the
    // identity exported above has to be the one that answers at that point (D-J1c, A2).
    let export_at = first_line_where(&gate, |l| l.starts_with("export GATE_SERVER_SYSTEM_ID="));
    let docs_at = first_line_where(&gate, |l| l.contains("features docs ==="));
    let (Some(export_at), Some(docs_at)) = (export_at, docs_at) else {
        panic!(
            "merge-gate.sh must export GATE_SERVER_SYSTEM_ID (found on {export_at:?}) and print the \
             --features docs step (found on {docs_at:?}) outside its self-test"
        );
    };
    assert!(
        gate.iter().any(|(n, l)| {
            l.contains("gate_require_judge_server") && *n > export_at && *n < docs_at
        }),
        "merge-gate.sh does not call gate_require_judge_server between the export of \
         GATE_SERVER_SYSTEM_ID (line {export_at}) and the --features docs step (line {docs_at}): a \
         server that changed under run-all-tests.sh would reach the docs step unseen"
    );
}

/// The eval smoke scores a fixture that is versioned and seeded into a scratch database that
/// the judge owns; the live corpus is never named.
#[test]
fn the_eval_reads_a_seeded_scratch_database_and_never_the_live_one() {
    let suite_body = read("scripts/run-all-tests.sh");
    let code: Vec<(usize, &str)> = outside_self_test(&suite_body)
        .into_iter()
        .filter(|(_, l)| !l.trim_start().starts_with('#'))
        .collect();

    let live: Vec<usize> = code
        .iter()
        .filter(|(_, l)| l.contains("LIVE_DATABASE_URL"))
        .map(|(n, _)| *n)
        .collect();
    assert!(
        live.is_empty(),
        "run-all-tests.sh still names LIVE_DATABASE_URL on lines {live:?}. The eval reads the \
         live corpus through it: the eval has to read the seeded fixture, in brain_gate_eval"
    );

    let call = code
        .iter()
        .position(|(_, l)| l.contains("--dataset") && l.contains("smoke.jsonl"))
        .expect("run-all-tests.sh runs the eval on smoke.jsonl");
    let window = &code[call.saturating_sub(3)..=call];
    let env_line: &str = window
        .iter()
        .map(|(_, l)| *l)
        .find(|l| l.trim_start().starts_with("DATABASE_URL=\"$"))
        .expect(
            "the eval call names no DATABASE_URL, so it would reach whatever the environment holds",
        );
    let var = env_line.trim_start()["DATABASE_URL=\"$".len()..]
        .split('"')
        .next()
        .unwrap_or("");
    let definition = code
        .iter()
        .find(|(_, l)| l.trim_start().starts_with(&format!("{var}=")));
    let (n, def) = definition.expect("the variable the eval call uses is not defined");
    assert!(
        def.contains("brain_gate_eval"),
        "run-all-tests.sh line {n} defines the eval's database as `{}`, not brain_gate_eval, the \
         scratch database the judge seeds from the fixture",
        def.trim()
    );
}

/// The sections of the judge that run tests by name or with `--ignored` each have a count in
/// `scripts/test-section-counts.txt`, written as `<section> <tests> <motive>`. Every such
/// section is named by a `--count` call in run-all-tests.sh, and every call has its line here.
const CI_J_COUNTED_SECTIONS: [&str; 9] = [
    "discovered-ignored",
    "lib-ignored",
    "admin-role",
    "embedding-model",
    "nli",
    "generative-llm",
    "reranker-placement",
    "placement-table",
    "reranker-model",
];

#[test]
fn every_section_with_a_count_runs_and_every_count_has_a_section() {
    let file = "scripts/test-section-counts.txt";
    assert!(
        repo_root().join(file).is_file(),
        "falta {file}: CI-J (pasada 2) lo escribe con las secciones que corren --ignored o van \
         nombradas, con los números del log de F0 (ci-rapido-J-F1.spec.md, D-J4b)"
    );
    let text = read(file);
    let mut ids: Vec<String> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let id = fields.next().unwrap_or("");
        let tests = fields.next().unwrap_or("");
        let motive = fields.collect::<Vec<_>>().join(" ");
        assert!(
            tests.parse::<u64>().is_ok(),
            "{file}:{}: `{line}` has no test count; the line is <section> <tests> <motive>",
            index + 1
        );
        assert!(
            !motive.is_empty(),
            "{file}:{}: `{id}` has no motive; a count without one is a number nobody can judge",
            index + 1
        );
        ids.push(id.to_string());
    }
    for id in CI_J_COUNTED_SECTIONS {
        assert!(
            ids.iter().any(|i| i.as_str() == id),
            "{file} has no line for the section `{id}`"
        );
    }

    let suite_body = read("scripts/run-all-tests.sh");
    // The section named by each `run_counted <section> <named> <command>` call outside the
    // self-test: the counter is one (D-J4d), so each counted section is one of these calls.
    // A line that is a comment, or the definition `run_counted() {`, names no section.
    let called_sections = |body: &str| -> Vec<String> {
        let mut out = Vec::new();
        for (_, line) in outside_self_test(body) {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let tokens: Vec<&str> = line.split_whitespace().collect();
            for (at, token) in tokens.iter().enumerate() {
                if *token != "run_counted" {
                    continue;
                }
                if let Some(section) = tokens.get(at + 1) {
                    let shaped = section.starts_with(|c: char| c.is_ascii_lowercase())
                        && section
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                    if shaped {
                        out.push(section.to_string());
                    }
                }
            }
        }
        out
    };
    // Each section is counted by one call: a second `run_counted` with the same name would
    // count the section twice, and the set comparison below cannot see that.
    let calls = called_sections(&suite_body);
    let mut once = std::collections::BTreeMap::<String, usize>::new();
    for section in &calls {
        *once.entry(section.clone()).or_insert(0) += 1;
    }
    let twice: Vec<&String> = once
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(section, _)| section)
        .collect();
    assert!(
        twice.is_empty(),
        "run-all-tests.sh counts these sections with more than one run_counted call, so each is \
         counted twice: {twice:?}"
    );
    let mut called = calls;
    called.sort();
    called.dedup();
    let mut named = ids.clone();
    named.sort();
    named.dedup();
    assert!(
        !called.is_empty(),
        "run-all-tests.sh has no `run_counted` call outside its self-test, so no section is counted"
    );
    for id in &called {
        assert!(
            named.contains(id),
            "run-all-tests.sh counts the section `{id}` with run_counted and {file} has no line for \
             it: a count nobody states is a silence"
        );
    }
    for id in &named {
        assert!(
            called.contains(id),
            "{file} states a count for the section `{id}` and no run_counted call in \
             run-all-tests.sh counts it: a line for a section that is not counted proves nothing"
        );
    }
}

/// The product test of migrations over an existing schema is in
/// `rust/tests/v046_migrations_apply_over_an_existing_schema.rs`, so the banner and the doc stop
/// saying that migrations on old databases are not covered: they are, and the two copies must
/// move in the same edit.
#[test]
fn the_banner_and_the_doc_stop_saying_old_database_migrations_are_not_covered() {
    let banner = banner_does_not_check_block().to_lowercase();
    let doc = doc_does_not_check_section().to_lowercase();
    assert!(
        !banner.contains("migrations on old db"),
        "the gate banner still says that migrations on old databases are not covered; the \
         product test rust/tests/v046_migrations_apply_over_an_existing_schema.rs covers them"
    );
    assert!(
        !doc.contains("migrations on old db"),
        "docs/gate.md still says that migrations on old databases are not covered; the product \
         test rust/tests/v046_migrations_apply_over_an_existing_schema.rs covers them"
    );
    assert!(
        repo_root()
            .join("rust/tests/v046_migrations_apply_over_an_existing_schema.rs")
            .is_file(),
        "the test that covers migrations over an existing schema is missing"
    );
}
/// J3.3 is judged in one place: `scripts/eval_seed.py --check-fixture` (CI-J2, A6). This contract
/// runs it over the real fixture, which has to pass, and over a fixture of one observation that
/// holds no marker of any query, which has to be refused with exit 1 and a FAIL line. The rule is
/// not copied here: a copy in Rust would go on passing when the judge's mode drifted from it.
#[test]
fn every_smoke_query_has_a_relevant_observation_in_the_fixture() {
    let script = repo_root().join("scripts/eval_seed.py");
    let queries = repo_root().join("rust/eval-datasets/smoke.jsonl");
    let check = |fixture: &std::path::Path| {
        std::process::Command::new("python3")
            .arg("-I")
            .arg(&script)
            .arg("--check-fixture")
            .arg("--fixture")
            .arg(fixture)
            .arg("--queries")
            .arg(&queries)
            .output()
            .expect("python3 runs scripts/eval_seed.py --check-fixture")
    };
    let real = check(
        repo_root()
            .join("rust/eval-datasets/smoke-corpus.jsonl")
            .as_path(),
    );
    assert!(
        real.status.success(),
        "eval_seed.py --check-fixture refuses the real fixture: {}{}",
        String::from_utf8_lossy(&real.stdout),
        String::from_utf8_lossy(&real.stderr)
    );

    let bad =
        std::env::temp_dir().join(format!("memory-industry-j3-3-{}.jsonl", std::process::id()));
    std::fs::write(&bad, "{\"content\": \"nada que buscar\"}\n").expect("write the bad fixture");
    let refused = check(bad.as_path());
    let _ = std::fs::remove_file(&bad);
    assert_eq!(
        refused.status.code(),
        Some(1),
        "eval_seed.py --check-fixture over a fixture with no marker of any query is not exit 1: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("FAIL:"),
        "the refusal names no FAIL line: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
}

/// restore-db.sh names the database it is about to replace without its credentials (CI-J2, A8,
/// D-J1h): the authority is cut at the LAST '@', as mcp_live_session_test.py cuts it, so a password
/// with an unencoded '@' never reaches the screen. The run goes as far as the prompt, with a
/// `docker` that finds nothing, a stub `pg_restore` that lists one table, and a `backup-db.sh`
/// that does nothing; it answers no, so nothing is restored.
#[cfg(unix)]
#[test]
fn restore_names_the_database_after_the_last_at_and_changes_nothing() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};

    let dir = std::env::temp_dir().join(format!("memory-industry-a8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let stubs = dir.join("bin");
    let tree = dir.join("tree");
    std::fs::create_dir_all(&stubs).expect("create the stub directory");
    std::fs::create_dir_all(tree.join("scripts")).expect("create the tree");
    let stub = |path: std::path::PathBuf, body: &str| {
        std::fs::write(&path, body).expect("write a stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make a stub executable");
    };
    stub(stubs.join("docker"), "#!/bin/sh\nexit 1\n");
    stub(
        stubs.join("pg_restore"),
        "#!/bin/sh\necho '; TABLE DATA public memory_entities'\n",
    );
    stub(tree.join("scripts/backup-db.sh"), "#!/bin/sh\nexit 0\n");
    std::fs::copy(
        repo_root().join("scripts/restore-db.sh"),
        tree.join("scripts/restore-db.sh"),
    )
    .expect("copy restore-db.sh");
    let dump = dir.join("brain.dump");
    std::fs::write(&dump, "dump").expect("write the dump");

    let path = format!(
        "{}:{}",
        stubs.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new("bash")
        .arg(tree.join("scripts/restore-db.sh"))
        .arg(&dump)
        .env(
            "DATABASE_URL",
            "postgresql://cuba:pa@ss@127.0.0.1:5489/brain",
        )
        .env("PATH", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run restore-db.sh");
    child
        .stdin
        .take()
        .expect("restore-db.sh has a stdin")
        .write_all(b"n\n")
        .expect("answer the prompt");
    let out = child.wait_with_output().expect("wait for restore-db.sh");
    let _ = std::fs::remove_dir_all(&dir);

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "restore-db.sh answered no and should exit 0: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("replace the contents of 127.0.0.1:5489/brain "),
        "restore-db.sh does not name the database as host:port/base: {stdout}"
    );
    assert!(
        !stdout.contains("ss@") && !stdout.contains("pa@"),
        "restore-db.sh printed part of the credentials of DATABASE_URL: {stdout}"
    );
}

/// CI-F4 (F4a): each mutant builds only the lib test it runs. The `--lib` that selects the
/// test goes to the build through `--cargo-arg`, and the filter stays after the `--`. With
/// `-- --lib search::` every one of the 54 mutants compiled the 116 integration binaries
/// it never ran: 973 s of the 1053 s of the baseline, measured on 2026-11-06.
#[test]
fn each_mutant_builds_only_the_lib_test_it_runs() {
    let script = read("scripts/mutants-gate.sh").replace("\r\n", "\n");
    let body: String = outside_self_test(&script)
        .into_iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    assert!(
        !body.contains("-- --lib search::"),
        "mutants-gate.sh still passes `-- --lib search::`: the --lib after the `--` reaches the \
         test only, so every mutant's build compiles all the test binaries"
    );
    // D-F6b (F6a): the call of the main path is `mutants_lab_run ... \`, the one that names the files and the
    // filter. The `cargo-mutants mutants` it makes (the shim as CARGO, D-F4m, and the seed) now lives in the
    // function, which the quality-gate.sh also calls through `--lab-run`: it is no longer a line of the main path.
    let lines: Vec<&str> = body.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.starts_with("mutants_lab_run ") && line.ends_with('\\'))
        .expect("the main path of mutants-gate.sh has no `mutants_lab_run ... \\` call over several lines");
    let last = first
        + lines[first..]
            .iter()
            .position(|line| !line.ends_with('\\'))
            .expect("the mutants_lab_run call does not end: its last line still has a `\\`");
    let call = lines[first..=last].join("\n");
    let function = body
        .split_once("\nmutants_lab_run() {")
        .expect("mutants-gate.sh has no `mutants_lab_run() {` function")
        .1
        .split_once("\n}\n")
        .expect("mutants_lab_run() is not closed by a `}` at the start of a line")
        .0;
    assert!(
        function.contains("cargo-mutants mutants"),
        "mutants_lab_run must be the one place that runs `cargo-mutants mutants`: the main path and \
         quality-gate.sh both reach it, and a second copy of the call is a second lab to keep in step"
    );
    let call = call.as_str();
    let (build_side, test_side) = call
        .split_once("\n  -- ")
        .expect("the cargo mutants invocation has no `  -- ` line before its test arguments");
    assert!(
        build_side.contains("--cargo-arg=--lib"),
        "the cargo mutants invocation must pass `--cargo-arg=--lib` before the `--`, so the \
         build of each mutant is the lib test and not every test binary"
    );
    assert_eq!(
        test_side.trim(),
        "search::",
        "after the `--` cargo mutants must receive exactly the test filter and nothing else"
    );
}

/// The commands of `body` that run, as one line each: outside the self-test regions, with the comment
/// lines gone, the `\` continuations joined and the blanks collapsed. A fixture or a comment may name the
/// words a contract forbids; only a command that runs is judged.
fn commands_outside_self_test(body: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut open = String::new();
    for (_, line) in outside_self_test(body) {
        if line.trim_start().starts_with('#') {
            continue;
        }
        match line.trim_end().strip_suffix('\\') {
            Some(head) => {
                open.push(' ');
                open.push_str(head);
            }
            None => {
                open.push(' ');
                open.push_str(line);
                commands.push(open.split_whitespace().collect::<Vec<_>>().join(" "));
                open.clear();
            }
        }
    }
    commands
}

/// What is wrong with the call of the second judge to the lab of mutants-gate.sh: it has to exist, pass
/// `--cargo-arg=--lib` among the arguments of cargo-mutants, and pass no `--lib` after the `--` of those
/// arguments (the first ` -- ` is the mode's own separator, the second one would be cargo-mutants').
fn lab_call_problems(commands: &[String]) -> Vec<String> {
    let Some(call) = commands
        .iter()
        .find(|command| command.contains("scripts/mutants-gate.sh\" --lab-run"))
    else {
        return vec!["no command calls `scripts/mutants-gate.sh\" --lab-run`".to_string()];
    };
    let Some((_, args)) = call.split_once(" -- ") else {
        return vec![format!(
            "the call has no `--` after the mode's own arguments: {call}"
        )];
    };
    let mut problems = Vec::new();
    if !args.contains("--cargo-arg=--lib") {
        problems.push(format!(
            "the call does not pass `--cargo-arg=--lib`: {call}"
        ));
    }
    if let Some((_, after)) = args.split_once(" -- ")
        && after.contains("--lib")
    {
        problems.push(format!(
            "the call passes `--lib` after cargo-mutants' `--`: {call}"
        ));
    }
    problems
}

/// CI-F6 (F6a, D-F6b): the second judge does not build a lab of its own. Its mutation step asks mutants-gate.sh
/// for one (`--lab-run`), which holds the seed, the stage, the shim and the guards that F4 proved; a copy of any
/// of them here would be a second lab to keep in step, which is the drift that `quality-gate.ps1` was.
#[test]
fn the_diff_judge_mutates_through_the_lab_of_mutants_gate() {
    let good = vec![
        "\"$ROOT/scripts/mutants-gate.sh\" --lab-run \"$out\" 2 -- \"$@\" --timeout 90 --cargo-arg=--lib"
            .to_string(),
    ];
    assert!(
        lab_call_problems(&good).is_empty(),
        "the scan flags a call that is right"
    );
    for (what, bad) in [
        (
            "a bare --lib after the --",
            "\"$ROOT/scripts/mutants-gate.sh\" --lab-run \"$out\" 2 -- --file a --cargo-arg=--lib -- --lib",
        ),
        (
            "no --cargo-arg=--lib",
            "\"$ROOT/scripts/mutants-gate.sh\" --lab-run \"$out\" 2 -- --file a --timeout 90",
        ),
        (
            "no separator",
            "\"$ROOT/scripts/mutants-gate.sh\" --lab-run \"$out\" 2",
        ),
    ] {
        assert!(
            !lab_call_problems(&[bad.to_string()]).is_empty(),
            "the scan must be able to fail: it let through a call with {what}"
        );
    }
    assert!(
        !lab_call_problems(&[]).is_empty(),
        "the scan must flag a script that never calls the lab"
    );

    let script = read("scripts/quality-gate.sh").replace("\r\n", "\n");
    let commands = commands_outside_self_test(&script);
    let problems = lab_call_problems(&commands);
    assert!(
        problems.is_empty(),
        "quality-gate.sh has to run its full mutation through `scripts/mutants-gate.sh --lab-run`, building only the lib test: {problems:#?}"
    );
    for needle in [
        "cargo-shim",
        "mutants-seed",
        "--copy-target",
        "memory.max",
        "MemoryMax",
    ] {
        let found: Vec<&String> = commands.iter().filter(|c| c.contains(needle)).collect();
        assert!(
            found.is_empty(),
            "quality-gate.sh runs `{needle}` itself: the seed, the shim, the copy of the target and the memory of the \
             cgroup are mutants-gate.sh's (D-F6b), and a second copy of any of them drifts: {found:#?}"
        );
    }
}

/// What is wrong with the default of the mutation mode: every default reads `QG_SELECT:-full`, and within a
/// screen of it the script says that `cover` is adopted only after a study with 0 differences noted in the backlog.
fn default_mode_problems(lines: &[(usize, &str)]) -> Vec<String> {
    let Some(at) = lines
        .iter()
        .position(|(_, line)| line.contains("QG_SELECT:-"))
    else {
        return vec!["no line reads `QG_SELECT:-<default>`".to_string()];
    };
    let mut problems = Vec::new();
    for (number, line) in lines {
        if line.contains("QG_SELECT:-") && !line.contains("QG_SELECT:-full") {
            problems.push(format!(
                "line {number} defaults QG_SELECT to something that is not `full`: {line}"
            ));
        }
    }
    let window: String = lines[at.saturating_sub(25)..(at + 25).min(lines.len())]
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    for word in ["study", "0 differences", "backlog"] {
        if !window.contains(word) {
            problems.push(format!(
                "within 25 lines of the default there is no `{word}`: the change to `cover` has to say it needs a study with 0 differences noted in the backlog (D-F6q)"
            ));
        }
    }
    problems
}

/// CI-F6 (F6a, D-F6a, D-F6q): `cover` selects the tests of each mutant, and what it spares is time that it only
/// earns if the study shows the same class for every mutant. Until a study with 0 differences is in the backlog, the
/// default is `full`, which changes no test.
#[test]
fn the_diff_judge_selects_tests_only_after_a_study() {
    let fixture = [
        (1, "mode=\"${QG_SELECT:-cover}\""),
        (2, "# a study with 0 differences, in the backlog"),
    ];
    assert!(
        !default_mode_problems(&fixture).is_empty(),
        "the scan must be able to fail: it let through a default of `cover`"
    );
    let silent = [(1, "mode=\"${QG_SELECT:-full}\"")];
    assert!(
        !default_mode_problems(&silent).is_empty(),
        "the scan must be able to fail: it let through a default that does not say a study is needed"
    );

    let script = read("scripts/quality-gate.sh").replace("\r\n", "\n");
    let problems = default_mode_problems(&outside_self_test(&script));
    assert!(problems.is_empty(), "quality-gate.sh: {problems:#?}");
}

/// CI-F6 (F6a, D-F6a, D-F6s): `legacy` is the reference of the study and the way back: the call of the step as it
/// was before F6, which the study compares against. Its timeout, its `--gitignore=false` and the `-- --lib` that
/// only reaches the test phase stay as they were, and it does not borrow the lab's flags.
#[test]
fn the_reference_of_the_study_is_todays_judge() {
    let script = read("scripts/quality-gate.sh").replace("\r\n", "\n");
    let commands = commands_outside_self_test(&script);
    let legacy = commands
        .iter()
        .find(|c| c.contains("cargo mutants \"$@\""))
        .expect("quality-gate.sh no longer has the `cargo mutants \"$@\"` call of today: `legacy` is that call, unchanged");
    for needle in ["--timeout 90", "--gitignore=false"] {
        assert!(
            legacy.contains(needle),
            "the legacy call lost `{needle}`, so it is no longer the judge the study compares with: {legacy}"
        );
    }
    let tail = legacy
        .rsplit_once(" -- --lib")
        .map(|(_, tail)| tail.trim())
        .unwrap_or_else(|| panic!("the legacy call has to end in `-- --lib`, as it did: {legacy}"));
    assert!(
        tail.is_empty() || tail.starts_with(')') || tail.starts_with("||"),
        "the legacy call has something after its `-- --lib` (`{tail}`): the --lib after the `--` is the last argument, as it was"
    );
    for needle in ["--dir", "--copy-target", "--jobserver-tasks", "--cargo-arg"] {
        assert!(
            !legacy.contains(needle),
            "the legacy call carries `{needle}`, which belongs to the lab of the full step: {legacy}"
        );
    }
    assert!(
        commands.iter().any(|c| c.contains("legacy")),
        "quality-gate.sh does not name `legacy` in a command that runs (its self-test does, which proves nothing about the step)"
    );
}

/// What is wrong with how the second judge asks for the merge of its passes: some command that runs has to call
/// `"$QG_SELF" --merge-select`. The bare word is the mode's own dispatcher, which merges nothing.
fn merge_call_problems(commands: &[String]) -> Vec<String> {
    if commands
        .iter()
        .any(|command| command.contains("\"$QG_SELF\" --merge-select"))
    {
        Vec::new()
    } else {
        vec!["no command calls `\"$QG_SELF\" --merge-select`".to_string()]
    }
}

/// What is wrong with the section of docs/gate.md that explains the selection of tests by coverage: it has to
/// exist, and it has to say that a missed mutant is confirmed with the whole suite, because that sentence is what
/// lets a reader trust a `caught` that only ran a subset.
fn selection_doc_problems(doc: &str) -> Vec<String> {
    const HEADING: &str = "## Test selection by coverage (Q2)";
    let Some(start) = doc.find(HEADING) else {
        return vec![format!("docs/gate.md has no `{HEADING}` section")];
    };
    let rest = &doc[start + HEADING.len()..];
    let section = &rest[..rest.find("\n## ").unwrap_or(rest.len())];
    if section.contains("confirmed with the whole suite") {
        Vec::new()
    } else {
        vec![format!(
            "the `{HEADING}` section does not say that every missed and timed out mutant is `confirmed with the whole suite`"
        )]
    }
}

/// CI-F6 (F6b, D-F6m, D-F6n): a `caught` that only ran the tests that cover its mutant is trusted because
/// anything the subset does not catch is run again with every test before it counts. The judge asks the merge of
/// its passes for it, and the doc says so.
#[test]
fn every_missed_of_a_subset_is_confirmed_by_the_whole_suite() {
    let dispatcher = vec![r#"if [[ "${1:-}" == --merge-select ]]; then"#.to_string()];
    assert!(
        !merge_call_problems(&dispatcher).is_empty(),
        "the scan must be able to fail: it took the mode's dispatcher for a call"
    );
    assert!(
        merge_call_problems(&[
            r#"bash "$QG_SELF" --merge-select "$cdir/merged.json" "$a0" "$a1" "$b""#.to_string()
        ])
        .is_empty(),
        "the scan flags a call that is right"
    );
    assert!(
        !selection_doc_problems("# gate\n\n## Other\n\nconfirmed with the whole suite\n")
            .is_empty(),
        "the scan must be able to fail: it let through a doc with no section on the selection"
    );
    assert!(
        !selection_doc_problems("## Test selection by coverage (Q2)\n\nsome words\n\n## Next\n\nconfirmed with the whole suite\n")
            .is_empty(),
        "the scan must be able to fail: it read the sentence of another section"
    );

    let script = read("scripts/quality-gate.sh").replace("\r\n", "\n");
    let problems = merge_call_problems(&commands_outside_self_test(&script));
    assert!(
        problems.is_empty(),
        "quality-gate.sh has to merge the classes of its three passes through its `--merge-select` mode: {problems:#?}"
    );
    let doc = read("docs/gate.md").replace("\r\n", "\n");
    let problems = selection_doc_problems(&doc);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// CI-F4 (F4b): the mutation seed lives under the cache of the gate, not under the scratch
/// that the judge sweeps every six hours, so that it survives between runs.
#[test]
fn the_mutation_seed_lives_outside_the_swept_scratch() {
    let body = read("scripts/mutants-gate.sh").replace("\r\n", "\n");
    let line = body
        .lines()
        .find(|l| l.trim_start().starts_with("SEED_ROOT="))
        .expect(
            "mutants-gate.sh has no SEED_ROOT= assignment: the mutation seed has a root of its own",
        );
    assert!(
        line.contains("memory-industry-gate/mutants-seed"),
        "the mutation seed root `{line}` is not under memory-industry-gate/mutants-seed"
    );
    assert!(
        !line.contains("MUTANTS_SCRATCH") && !line.contains("memory-industry-mutants"),
        "the mutation seed root `{line}` is under the scratch the judge sweeps: a seed there is swept away"
    );
}

/// The key that signs the packages of the judge's Postgres (PGDG), which the script and the doc both name.
const PGDG_SIGNING_KEY: &str = "B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8";

/// The value a script assigns on a line of its own, `NAME=value` at column 0.
fn assigned(script: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    script.lines().find_map(|line| {
        line.strip_prefix(prefix.as_str())
            .map(|value| value.trim().to_string())
    })
}

/// What is wrong between the constants of the judge's Postgres in gate-cluster.sh and what docs/gate.md says (E2). The values
/// are read from the script (CI-J15e), so this holds no copy of them; and the major is derived from the version, written once.
fn postgres_doc_problems(cluster: &str, doc: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for name in [
        "GATE_PG_VERSION",
        "GATE_PGVECTOR_VERSION",
        "GATE_PG_LOCALE",
        "GATE_PG_TIMEZONE",
    ] {
        match assigned(cluster, name) {
            None => problems.push(format!(
                "scripts/gate-cluster.sh does not set `{name}=` on a line of its own (E2)"
            )),
            Some(value) if value.is_empty() => problems.push(format!(
                "scripts/gate-cluster.sh sets {name} to nothing (E2)"
            )),
            Some(value) => {
                if !doc.contains(value.as_str()) {
                    problems.push(format!(
                        "docs/gate.md does not say {value}, which scripts/gate-cluster.sh sets as {name} (E2)"
                    ));
                }
            }
        }
    }
    for text in [("scripts/gate-cluster.sh", cluster), ("docs/gate.md", doc)] {
        if !text.1.contains(PGDG_SIGNING_KEY) {
            problems.push(format!(
                "{} does not say the signing key {PGDG_SIGNING_KEY} (E2)",
                text.0
            ));
        }
    }
    let version_at = cluster
        .lines()
        .position(|line| line.starts_with("GATE_PG_VERSION="));
    let major_at = cluster
        .lines()
        .position(|line| line == "GATE_PG_MAJOR=${GATE_PG_VERSION%%.*}");
    match (version_at, major_at) {
        (Some(version), Some(major)) if version < major => {}
        _ => problems.push(
            "scripts/gate-cluster.sh has to derive its major from its version, after it: GATE_PG_MAJOR=${GATE_PG_VERSION%%.*} (CI-J15e)"
                .to_string(),
        ),
    }
    problems
}

/// CI-F2, E2: the judge's Postgres versions, locale and zone are written once, as constants of
/// gate-cluster.sh, and docs/gate.md and the script name the same values, the fingerprint of the
/// signing key included. A change to one of them without the other is red here. CI-J15e: the values are the
/// script's, read from it, and the major (`GATE_PG_MAJOR`) is the first number of the version, not a second constant.
#[test]
fn the_judge_postgres_versions_are_written_once_and_the_doc_agrees() {
    let cluster = read("scripts/gate-cluster.sh").replace("\r\n", "\n");
    let doc = read("docs/gate.md");
    let problems = postgres_doc_problems(&cluster, &doc);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// CI-J15e. The check is judged first, on a script and a doc that agree and on the three ways to stop agreeing: the script
/// moves and the doc does not, the major is a number written by hand, and the version is no longer on a line of its own.
#[test]
fn the_check_of_the_judge_postgres_versions_can_fail() {
    let cluster = format!(
        "GATE_PG_VERSION=18.6\nGATE_PG_MAJOR=${{GATE_PG_VERSION%%.*}}\nGATE_PGVECTOR_VERSION=0.8.7\nGATE_PG_LOCALE=en_US.utf8\nGATE_PG_TIMEZONE=Etc/UTC\n# signed by {PGDG_SIGNING_KEY}\n"
    );
    let doc =
        format!("PostgreSQL 18.6, pgvector 0.8.7, en_US.utf8, Etc/UTC, key {PGDG_SIGNING_KEY}");
    let clean = postgres_doc_problems(&cluster, &doc);
    assert!(clean.is_empty(), "the control is not clean: {clean:#?}");
    let moved = postgres_doc_problems(
        &cluster.replace("GATE_PG_VERSION=18.6", "GATE_PG_VERSION=18.7"),
        &doc,
    );
    assert!(
        moved
            .iter()
            .any(|p| p.contains("18.7") && p.contains("docs/gate.md")),
        "a version that moved in the script and not in the doc is named: {moved:#?}"
    );
    let by_hand = postgres_doc_problems(
        &cluster.replace("GATE_PG_MAJOR=${GATE_PG_VERSION%%.*}", "GATE_PG_MAJOR=18"),
        &doc,
    );
    assert!(
        by_hand.iter().any(|p| p.contains("GATE_PG_MAJOR")),
        "a major written by hand is named: {by_hand:#?}"
    );
    let hidden = postgres_doc_problems(
        &cluster.replace("GATE_PG_VERSION=18.6", " GATE_PG_VERSION=18.6"),
        &doc,
    );
    assert!(
        hidden.iter().any(|p| p.contains("GATE_PG_VERSION=")),
        "a version that is not on a line of its own is named: {hidden:#?}"
    );
}

/// CI-J15e (H9): the image of production is the judge's Postgres: `pgvector/pgvector:pg<major>` with the major of
/// GATE_PG_VERSION, in rust/src/setup.rs (`PG_IMAGE`) and in docker-compose.yml. A major that moves in one place and not
/// in the others has the judge testing a server that nobody runs.
fn production_image_problems(cluster: &str, setup: &str, compose: &str) -> Vec<String> {
    let Some(version) = assigned(cluster, "GATE_PG_VERSION") else {
        return vec!["scripts/gate-cluster.sh sets no GATE_PG_VERSION".to_string()];
    };
    let major = version.split('.').next().unwrap_or_default();
    let want = format!("pgvector/pgvector:pg{major}");
    let mut problems = Vec::new();
    let in_setup = setup
        .lines()
        .find(|line| line.contains("const PG_IMAGE"))
        .unwrap_or_default();
    if !in_setup.contains(&format!("\"{want}\"")) {
        problems.push(format!(
            "rust/src/setup.rs: PG_IMAGE is `{in_setup}` and the judge's Postgres {version} says {want}"
        ));
    }
    let in_compose = compose
        .lines()
        .find(|line| line.trim_start().starts_with("image:") && line.contains("pgvector"))
        .unwrap_or_default();
    if in_compose.trim() != format!("image: {want}") {
        problems.push(format!(
            "docker-compose.yml: the image is `{}` and the judge's Postgres {version} says {want}",
            in_compose.trim()
        ));
    }
    problems
}

#[test]
fn the_production_image_is_the_major_of_the_judge_postgres() {
    let problems = production_image_problems(
        &read("scripts/gate-cluster.sh").replace("\r\n", "\n"),
        &read("rust/src/setup.rs"),
        &read("docker-compose.yml"),
    );
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_check_of_the_production_image_can_fail() {
    let cluster = "GATE_PG_VERSION=18.6\n";
    let setup = "const PG_IMAGE: &str = \"pgvector/pgvector:pg18\";\n";
    let compose = "services:\n  db:\n    image: pgvector/pgvector:pg18\n";
    let clean = production_image_problems(cluster, setup, compose);
    assert!(clean.is_empty(), "the control is not clean: {clean:#?}");
    let old_setup = production_image_problems(
        cluster,
        "const PG_IMAGE: &str = \"pgvector/pgvector:pg17\";\n",
        compose,
    );
    assert!(
        old_setup.len() == 1 && old_setup[0].contains("setup.rs"),
        "a PG_IMAGE of another major is named: {old_setup:#?}"
    );
    let old_compose = production_image_problems(
        cluster,
        setup,
        "services:\n  db:\n    image: pgvector/pgvector:pg17\n",
    );
    assert!(
        old_compose.len() == 1 && old_compose[0].contains("docker-compose.yml"),
        "an image of another major in the compose is named: {old_compose:#?}"
    );
    let moved = production_image_problems("GATE_PG_VERSION=19.1\n", setup, compose);
    assert_eq!(
        moved.len(),
        2,
        "a judge that moves to 19 leaves both files behind: {moved:#?}"
    );
}

/// CI-F2, C4: no gate script builds a TCP URL for a lane. The one construction of a lane's URL is
/// gate_cluster_url in gate-cluster.sh, a socket directory as the host; outside the self-test
/// regions and the comments, no script names the live corpus port, listens on TCP, or writes a
/// `postgresql://cuba:` URL except that one.
#[test]
fn no_gate_script_builds_a_tcp_url_for_a_lane() {
    let mut constructions = 0;
    for file in [
        "scripts/gate-cluster.sh",
        "scripts/merge-gate.sh",
        "scripts/run-all-tests.sh",
    ] {
        let body = read(file).replace("\r\n", "\n");
        for (number, line) in outside_self_test(&body) {
            let code = line.trim_start();
            if code.starts_with('#') {
                continue;
            }
            assert!(
                !code.contains("5488"),
                "{file}:{number} names :5488 outside its self-test: `{line}`"
            );
            if code.contains("listen_addresses") {
                assert!(
                    code.contains("listen_addresses= ") || code.contains("listen_addresses=''"),
                    "{file}:{number} listens on something other than the empty address: `{line}`"
                );
            }
            if code.contains("postgresql://cuba:") {
                assert_eq!(
                    file, "scripts/gate-cluster.sh",
                    "{file}:{number} builds a lane URL outside gate_cluster_url: `{line}`"
                );
                constructions += 1;
            }
        }
    }
    assert_eq!(
        constructions, 1,
        "the lane URL has to be built in exactly one place (gate_cluster_url), found {constructions} (C4)"
    );
}

/// The shell that makes lane t1 for a_lane_cluster_is_the_judges_own_throwaway_server. It sources the
/// script it is given, takes every lane it made down on any exit, reports the lane, and holds until its
/// stdin closes, so the test decides when the lane goes down. Its owner is an owner_record of its own pid
/// (LANE_OWNER is the token), assigned after the source: gate-lock.sh sets GATE_OWNER to "" when it is
/// sourced, so a GATE_OWNER from the environment would be lost, and gate-cluster refuses that.
const LANE_SHELL: &str = r#"
source "$1" || exit 2
GATE_OWNER="$(owner_record "$$" "$LANE_OWNER")"
trap 'gate_cluster_down_all >/dev/null' EXIT
gate_cluster_up t1 >/dev/null || exit 2
printf 'shell=%s\npostmaster=%s\ndir=%s\nwal_level=%s\nready\n' "$$" "${GATE_CLUSTER_PID[t1]}" "${GATE_CLUSTER_DIR[t1]}" "$GATE_PG_WAL_LEVEL"
read -r _ || true
exit 0
"#;

/// Holds a lane shell and makes sure the lane is gone: dropping it closes the shell's stdin, so the shell
/// takes its lane down, and then removes the test's base directory. A failing assertion leaves no server.
#[cfg(unix)]
struct LaneShell {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    base: std::path::PathBuf,
}

#[cfg(unix)]
impl LaneShell {
    /// Closes the shell's stdin and waits for it for at most `within`. A shell that is still there at the
    /// deadline is killed, and the answer is None: a test that hangs has to fail, not wait for ever.
    fn release(&mut self, within: std::time::Duration) -> Option<std::process::ExitStatus> {
        drop(self.stdin.take());
        self.wait_or_kill(within)
    }

    fn wait_or_kill(&mut self, within: std::time::Duration) -> Option<std::process::ExitStatus> {
        let deadline = std::time::Instant::now() + within;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return None;
                }
            }
        }
    }
}

#[cfg(unix)]
impl Drop for LaneShell {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.wait_or_kill(std::time::Duration::from_secs(10));
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The zone of the SERVER of a lane, asked through the psql of its bindir on the lane's socket. It is not asked
/// through sqlx: sqlx sets TimeZone=UTC for every session it opens, so its `show timezone` says UTC whatever the
/// server's zone is. The password goes to psql in PGPASSWORD, never in argv or in a message. The three answers
/// are the zone name, the log zone name and the offset of now() from UTC in seconds.
#[cfg(unix)]
fn server_zone(bindir: &Path, dir: &Path, password: &str) -> Result<[String; 3], String> {
    let mut command = std::process::Command::new(bindir.join("psql"));
    command
        .arg("-X")
        .arg("-A")
        .arg("-t")
        .arg("-h")
        .arg(dir)
        .arg("-p")
        .arg("5432")
        .arg("-U")
        .arg("cuba")
        .arg("-d")
        .arg("brain")
        .arg("-c")
        .arg("show timezone")
        .arg("-c")
        .arg("show log_timezone")
        .arg("-c")
        .arg("select extract(timezone from now())::int");
    for name in [
        "PGTZ",
        "PGOPTIONS",
        "PGHOST",
        "PGHOSTADDR",
        "PGPORT",
        "PGUSER",
        "PGDATABASE",
        "PGPASSFILE",
        "PGSERVICE",
    ] {
        command.env_remove(name);
    }
    command.env("PGPASSWORD", password);
    let output = command
        .output()
        .map_err(|why| format!("psql did not start: {why}"))?;
    if !output.status.success() {
        return Err(format!(
            "psql exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let lines: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    <[String; 3]>::try_from(lines)
        .map_err(|lines| format!("psql answered {} lines, not three: {lines:?}", lines.len()))
}

/// CI-F2, B1 to B3 and E1: a lane of gate-cluster.sh is the judge's own throwaway server. It runs with the
/// settings the script writes (D-F2d), its URL is a socket under its root, its postmaster is a child of the
/// shell that made it, in that shell's cgroup, with stdin on /dev/null and its output on the lane's
/// server.log, and it checks passwords on the socket too. Needs the PostgreSQL 18.6 binaries of docs/gate.md:
/// without them gate-cluster.sh refuses the lane, and this test fails with that refusal.
#[cfg(unix)]
#[test]
fn a_lane_cluster_is_the_judges_own_throwaway_server() {
    use sqlx::Connection;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::FileTypeExt;
    use std::process::{Command, Stdio};

    let base = self_test_parent().join(format!("f2b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home")).expect("creating the HOME of the lane");
    std::fs::create_dir_all(base.join("cache")).expect("creating the XDG_CACHE_HOME of the lane");
    let root = base.join("root");
    let script = repo_root().join("scripts").join("gate-cluster.sh");
    let stderr = std::fs::File::create(base.join("driver.err"))
        .expect("creating the stderr of the lane shell");

    let mut command = Command::new(git_bash());
    command
        .arg("-c")
        .arg(LANE_SHELL)
        .arg("lane-shell")
        .arg(&script);
    for (name, _) in JUDGE_ENVIRONMENT {
        command.env_remove(name);
    }
    command
        .env("GATE_CLUSTER_ROOT", &root)
        .env("LANE_OWNER", format!("f2b-contract-{}", std::process::id()))
        .env("TZ", "America/Mexico_City")
        .env("HOME", base.join("home"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr));
    let mut child = command
        .spawn()
        .expect("starting the bash that makes lane t1");
    let stdin = child.stdin.take().expect("the lane shell's stdin is piped");
    let stdout = child
        .stdout
        .take()
        .expect("the lane shell's stdout is piped");
    let mut lane = LaneShell {
        child,
        stdin: Some(stdin),
        base: base.clone(),
    };

    let driver_says = || std::fs::read_to_string(base.join("driver.err")).unwrap_or_default();
    let lane_timeout = std::time::Duration::from_secs(60);
    // The report is read on a thread of its own, up to the `ready` line the shell prints after its keys. The
    // test waits for it at most `lane_timeout`: a shell that stops before `ready` gives EOF at once, and a
    // shell that never gets there is a failure with its stderr, not a wait for ever.
    let (report_tx, report_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut report: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line == "ready" {
                break;
            }
            if let Some((key, value)) = line.split_once('=') {
                report.insert(key.to_string(), value.to_string());
            }
        }
        let _ = report_tx.send(report);
    });
    let report = match report_rx.recv_timeout(lane_timeout) {
        Ok(report) => report,
        Err(_) => panic!(
            "lane t1 did not report within {} s, so its shell is stuck. gate-cluster.sh says: {}",
            lane_timeout.as_secs(),
            driver_says()
        ),
    };
    for key in ["shell", "postmaster", "dir", "wal_level"] {
        assert!(
            report.contains_key(key),
            "lane t1 was not made: its shell gave no `{key}`. gate-cluster.sh says: {}",
            driver_says()
        );
    }
    let shell_pid: u32 = report["shell"]
        .parse()
        .expect("the shell's pid is a number");
    let postmaster: u32 = report["postmaster"]
        .parse()
        .expect("the postmaster's pid is a number");
    let dir = std::path::PathBuf::from(&report["dir"]);
    let wal_level = report["wal_level"].clone();
    // The URL is built here from the directory and the password file, never printed by the shell: the
    // driver reports only the directory. Same shape as gate_cluster_url: the socket directory as the host.
    let password =
        std::fs::read_to_string(dir.join("pw")).expect("reading the password file of lane t1");
    let url = format!(
        "postgresql://cuba:{password}@{}:5432/brain",
        dir.display().to_string().replace('/', "%2F")
    );

    let mut expected: Vec<(&str, String)> = vec![
        ("show fsync", "off".to_string()),
        ("show synchronous_commit", "off".to_string()),
        ("show full_page_writes", "off".to_string()),
        ("show cluster_name", "memory-industry-gate-t1".to_string()),
        ("show max_connections", "300".to_string()),
        ("show wal_level", wal_level.clone()),
        ("show listen_addresses", String::new()),
        ("show data_checksums", "on".to_string()),
        (
            "select current_setting('server_version_num')",
            "180006".to_string(),
        ),
        (
            "select default_version::text from pg_available_extensions where name = 'vector'",
            "0.8.7".to_string(),
        ),
        (
            "select datcollate::text from pg_database where datname = 'template1'",
            "en_US.utf8".to_string(),
        ),
        (
            "select pg_encoding_to_char(encoding)::text from pg_database where datname = 'template1'",
            "UTF8".to_string(),
        ),
        (
            "select count(*)::text from pg_database where datname = 'brain'",
            "1".to_string(),
        ),
    ];
    if wal_level == "minimal" {
        expected.push(("show max_wal_senders", "0".to_string()));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the queries of lane t1");
    // The timeout is built inside the runtime: its Sleep needs the reactor when it is created, not when it is polled.
    let outcome = runtime.block_on(async {
        tokio::time::timeout(lane_timeout, async {
            let mut conn = sqlx::PgConnection::connect(&url)
                .await
                .expect("connecting to lane t1 through its socket");
            let mut answers: Vec<Result<String, String>> = Vec::new();
            for (query, _) in &expected {
                let sql: &str = query;
                let answer: Result<String, sqlx::Error> =
                    sqlx::query_scalar(sql).fetch_one(&mut conn).await;
                answers.push(answer.map_err(|why| why.to_string()));
            }
            let division = match sqlx::query("select 1/0").execute(&mut conn).await {
                Ok(_) => "no error".to_string(),
                Err(why) => why
                    .as_database_error()
                    .map_or_else(|| why.to_string(), |db| db.message().to_string()),
            };
            let _ = conn.close().await;

            // The same URL with another password: the lane's own password is the only one that is replaced,
            // and the wrong one is never printed.
            let wrong = url.replacen(password.as_str(), "not-the-password-of-this-lane", 1);
            let refused = match sqlx::PgConnection::connect(&wrong).await {
                Ok(_) => "connected".to_string(),
                Err(why) => why
                    .as_database_error()
                    .and_then(|db| db.code())
                    .map_or_else(|| "no SQLSTATE".to_string(), |code| code.to_string()),
            };
            (answers, division, refused, wrong != url)
        })
        .await
    });
    let Ok((answers, division, refused, wrong_differs)) = outcome else {
        panic!(
            "the queries on lane t1 did not finish within {} s. gate-cluster.sh says: {}",
            lane_timeout.as_secs(),
            driver_says()
        );
    };

    let mut failures: Vec<String> = Vec::new();
    for ((query, want), answer) in expected.iter().zip(answers) {
        match answer {
            Ok(got) if got == *want => {}
            Ok(got) => failures.push(format!(
                "`{query}` answers `{got}`, the lane should answer `{want}`"
            )),
            Err(why) => failures.push(format!("`{query}` fails: {why}")),
        }
    }
    let bindir = std::path::PathBuf::from(
        std::env::var("GATE_PG_BINDIR")
            .unwrap_or_else(|_| "/usr/lib/postgresql/18/bin".to_string()),
    );
    match server_zone(&bindir, &dir, &password) {
        Err(why) => failures.push(format!(
            "the zone of the server of lane t1 could not be asked through psql: {why} (B1)"
        )),
        Ok([timezone, log_timezone, offset]) => {
            for (name, got) in [
                ("show timezone", &timezone),
                ("show log_timezone", &log_timezone),
            ] {
                if got.as_str() != "UTC" && got.as_str() != "Etc/UTC" {
                    failures.push(format!(
                        "the server of lane t1 answers `{got}` to `{name}`, the lane should be in UTC (`UTC` or `Etc/UTC`): the zone of the host got in (B1)"
                    ));
                }
            }
            if offset != "0" {
                failures.push(format!(
                    "the offset of now() from UTC on the server of lane t1 is {offset} s, the lane should be 0 s: its zone is not UTC (B1)"
                ));
            }
        }
    }
    if division != "division by zero" {
        failures.push(format!(
            "`select 1/0` says `{division}`, not the English `division by zero`"
        ));
    }
    if !wrong_differs {
        failures.push(
            "the wrong password of the test is the lane's password: nothing is proved".to_string(),
        );
    }
    if refused != "28P01" {
        failures.push(format!(
            "a wrong password for cuba through the socket gives SQLSTATE {refused}, not 28P01 (B2)"
        ));
    }

    let on_socket = dir
        .join(".s.PGSQL.5432")
        .symlink_metadata()
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false);
    let under_root = dir.starts_with(&root);
    if !(on_socket && under_root) {
        failures.push(format!(
            "lane t1 has no socket under its root (socket {on_socket}, under the root {under_root})"
        ));
    }
    let data_fs = Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(dir.join("data"))
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    if data_fs != "tmpfs" && data_fs != "ramfs" {
        failures.push(format!(
            "the data directory of lane t1 is on `{data_fs}`, not on tmpfs"
        ));
    }

    let ppid = std::fs::read_to_string(format!("/proc/{postmaster}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("PPid:"))
                .and_then(|value| value.trim().parse::<u32>().ok())
        });
    if ppid != Some(shell_pid) {
        failures.push(format!(
            "the postmaster's parent is {ppid:?}, not the shell that made it ({shell_pid}) (B3)"
        ));
    }
    let shell_cgroup = std::fs::read(format!("/proc/{shell_pid}/cgroup"));
    let postmaster_cgroup = std::fs::read(format!("/proc/{postmaster}/cgroup"));
    let same_cgroup = match (&shell_cgroup, &postmaster_cgroup) {
        (Ok(shell), Ok(theirs)) => shell == theirs,
        _ => false,
    };
    if !same_cgroup {
        failures
            .push("the postmaster is not in the cgroup of the shell that made it (B3)".to_string());
    }
    for (fd, want) in [
        (0, std::path::PathBuf::from("/dev/null")),
        (1, dir.join("server.log")),
        (2, dir.join("server.log")),
    ] {
        let got = std::fs::read_link(format!("/proc/{postmaster}/fd/{fd}"));
        if got.as_ref().ok() != Some(&want) {
            failures.push(format!(
                "descriptor {fd} of the postmaster is {got:?}, not {want:?} (B3)"
            ));
        }
    }

    match lane.release(lane_timeout) {
        Some(status) if status.success() => {}
        Some(status) => failures.push(format!(
            "the lane shell exited with {status}: lane t1 was not taken down"
        )),
        None => failures.push(format!(
            "the lane shell did not take lane t1 down within {} s: it was killed",
            lane_timeout.as_secs()
        )),
    }
    if Path::new(&format!("/proc/{postmaster}")).exists() {
        failures.push("the postmaster outlives the lane's down".to_string());
    }
    if dir.exists() {
        failures.push("the directory of lane t1 is still there after its down".to_string());
    }

    assert!(
        failures.is_empty(),
        "lane t1 is not the judge's own throwaway server: {failures:#?}\ngate-cluster.sh said: {}",
        driver_says()
    );
}
// --- CI-F3.3a: the step table and the lanes of run-all-tests.sh ---------------------------------
// D-F3a: scripts/gate-dag.txt is the one source of the steps. D-F3y: a run without arguments keeps
// the idx order. §5.1 C2: the table and the --lane names do not drift, the graph has no cycle, and
// a consumer of the release artifact (T_rel) has its producer B3 above it.

/// One row of scripts/gate-dag.txt as (idx, id, deps, whole line). Blank lines and comments are skipped.
fn dag_rows() -> Vec<(u32, String, Vec<String>, String)> {
    read("scripts/gate-dag.txt")
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            assert!(
                fields.len() >= 8,
                "a gate-dag.txt row has at least 8 fields: {line}"
            );
            let idx: u32 = fields[0]
                .parse()
                .expect("the idx of a gate-dag.txt row is a number");
            let deps = if fields[2] == "-" {
                Vec::new()
            } else {
                fields[2].split(',').map(String::from).collect()
            };
            (idx, fields[1].to_string(), deps, line.to_string())
        })
        .collect()
}

/// CI-J15e (H10): the rows that compile are the ones the table says: those that name a target and are not readers (a
/// reader builds nothing in it). The list is derived, so a row that is added and compiles is judged without anybody
/// adding its id here. As (id, target).
fn compiling_rows(table: &str) -> Vec<(String, String)> {
    table
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let resources = resources_of(line);
            if resources.iter().any(|r| r.as_str() == "reader") {
                return None;
            }
            let target = resources.iter().find_map(|r| r.strip_prefix("target:"))?;
            let id = line.split_whitespace().nth(1)?;
            Some((id.to_string(), target.to_string()))
        })
        .collect()
}

/// The targets that more than one compiling row holds, each with its rows: two compiling rows in one target would wait on
/// its lock and build over each other.
fn shared_targets(rows: &[(String, String)]) -> Vec<String> {
    let mut by_target: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for (id, target) in rows {
        by_target
            .entry(target.as_str())
            .or_default()
            .push(id.as_str());
    }
    by_target
        .into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(target, ids)| format!("{target}: {}", ids.join(", ")))
        .collect()
}

/// CI-F3.3c (D-F3g): every row that compiles builds in a target of its own, and the rows that read the release binary
/// read the one a row that compiles built. Two compiling rows in one target would wait on its lock and build over each
/// other. CI-J15e: the rows that compile are read from the table, not listed here.
#[test]
fn each_row_that_compiles_has_a_target_of_its_own_and_the_release_readers_share_b3s() {
    let table = read("scripts/gate-dag.txt");
    let compiling = compiling_rows(&table);
    let ids: Vec<&str> = compiling.iter().map(|(id, _)| id.as_str()).collect();
    for anchor in ["DBG", "B3", "DC", "CV", "M"] {
        assert!(
            ids.contains(&anchor),
            "row {anchor} builds and the table does not say so: the rows that compile are {ids:?}"
        );
    }
    let shared = shared_targets(&compiling);
    assert!(
        shared.is_empty(),
        "two compiling rows share a target: {shared:?}"
    );
    for line in table
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        let resources = resources_of(line);
        if !resources.iter().any(|r| r.as_str() == "reader") {
            continue;
        }
        let id = line.split_whitespace().nth(1).unwrap_or_default();
        let target = resources
            .iter()
            .find_map(|r| r.strip_prefix("target:"))
            .unwrap_or_default();
        assert!(
            compiling.iter().any(|(_, built)| built.as_str() == target),
            "reader {id} reads {target}, which no row that compiles builds: {line}"
        );
    }
}

/// CI-J15e. The derivation is judged first, on a table with the ways to be wrong: a reader is not a compiling row, and a
/// row added that builds in a target another row builds in is named with both.
#[test]
fn the_check_of_the_compiling_rows_can_fail() {
    let table = "20 DBG P target:T_dbg normal 8 8 1 bash scripts/merge-gate.sh --step DBG\n\
                 30 B3 P target:T_rel,cluster:C_b3 normal 8 8 1 bash scripts/merge-gate.sh --step B3\n\
                 40 REL B3 target:T_rel,reader normal 8 8 1 bash scripts/merge-gate.sh --step REL\n";
    let clean = compiling_rows(table);
    assert_eq!(clean.len(), 2, "a reader (REL) does not compile: {clean:?}");
    assert!(
        shared_targets(&clean).is_empty(),
        "a reader of T_rel is not a second builder of it: {clean:?}"
    );
    let sabotaged = format!(
        "{table}50 NEW P target:T_rel normal 8 8 1 bash scripts/merge-gate.sh --step NEW\n"
    );
    let shared = shared_targets(&compiling_rows(&sabotaged));
    assert_eq!(shared.len(), 1, "{shared:?}");
    assert!(
        shared[0].contains("T_rel") && shared[0].contains("B3") && shared[0].contains("NEW"),
        "the shared target is named with both rows: {shared:?}"
    );
}

/// The body of `step_<id>()` in merge-gate.sh: the lines between its header and the `}` that closes it.
/// CI-F3.3b-2: a row of the table is a step of merge-gate.sh, and the --lane ids it runs are in its body.
fn step_body(gate: &str, id: &str) -> Option<Vec<String>> {
    let header = format!("step_{id}() {{");
    let mut lines = gate.lines();
    lines.find(|line| line.trim_end() == header)?;
    let mut body = Vec::new();
    for line in lines {
        if line == "}" {
            return Some(body);
        }
        body.push(line.to_string());
    }
    None
}

/// The --lane ids a step body runs, in the order its text gives them.
fn lanes_in(body: &[String]) -> Vec<String> {
    body.iter()
        .flat_map(|line| {
            line.split("--lane ")
                .skip(1)
                .filter_map(|rest| rest.split_whitespace().next())
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether `from` reaches `to` through its deps. The depth bound keeps a cycle from looping forever.
fn depends_on(
    rows: &[(u32, String, Vec<String>, String)],
    from: &str,
    to: &str,
    depth: usize,
) -> bool {
    if depth > rows.len() {
        return false;
    }
    rows.iter()
        .find(|row| row.1.as_str() == from)
        .is_some_and(|row| {
            row.2
                .iter()
                .any(|dep| dep.as_str() == to || depends_on(rows, dep.as_str(), to, depth + 1))
        })
}

#[test]
fn every_row_of_the_step_table_is_a_step_of_merge_gate_whose_lanes_are_run_all_tests_functions() {
    let gate = read("scripts/merge-gate.sh");
    let suite = read("scripts/run-all-tests.sh");
    let rows = dag_rows();
    assert!(
        !rows.is_empty(),
        "gate-dag.txt has no rows: the planner has nothing to run"
    );
    for (idx, id, _, line) in &rows {
        // CI-F3.3c (D-F3j): leading NAME=value tokens are the step's environment; the order is what follows them.
        let order = line
            .split_whitespace()
            .skip(8)
            .skip_while(|token| token.contains('='))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            order,
            format!("bash scripts/merge-gate.sh --step {id}"),
            "row {idx}: the order of a row is its step of merge-gate.sh (CI-F3.3b-2)"
        );
        let body = step_body(&gate, id).unwrap_or_else(|| {
            panic!("row {idx}: merge-gate.sh has no step_{id}() function for the row")
        });
        for lane in lanes_in(&body) {
            assert!(
                suite.contains(&format!("lane_{lane}() {{")),
                "step_{id}: run-all-tests.sh has no function lane_{lane}() for --lane {lane}"
            );
        }
    }
}

#[test]
fn run_all_tests_without_arguments_runs_its_lanes_in_idx_order() {
    let suite = read("scripts/run-all-tests.sh");
    let order_line = suite
        .lines()
        .find(|line| line.starts_with("DAG_LANES=("))
        .expect(
            "run-all-tests.sh names its lanes in one DAG_LANES=(...) line, in idx order (D-F3y)",
        );
    let order: Vec<String> = order_line
        .trim_start_matches("DAG_LANES=(")
        .trim_end_matches(')')
        .split_whitespace()
        .map(String::from)
        .collect();
    let gate = read("scripts/merge-gate.sh");
    let mut rows = dag_rows();
    rows.sort_by_key(|row| row.0);
    let expected: Vec<String> = rows
        .iter()
        .flat_map(|row| {
            lanes_in(
                &step_body(&gate, &row.1)
                    .unwrap_or_else(|| panic!("merge-gate.sh has no step_{}() function", row.1)),
            )
        })
        .collect();
    assert_eq!(
        order, expected,
        "DAG_LANES must list the --lane steps of the table in idx order"
    );
}

#[test]
fn the_step_graph_has_no_cycle_and_every_consumer_has_its_producer_above_it() {
    let rows = dag_rows();
    for (idx, id, deps, _) in &rows {
        for dep in deps {
            assert!(
                rows.iter().any(|row| row.1 == *dep),
                "row {idx} {id}: its dep {dep} is not a row of gate-dag.txt"
            );
        }
    }
    let mut placed: Vec<String> = Vec::new();
    while placed.len() < rows.len() {
        let before = placed.len();
        for (_, id, deps, _) in &rows {
            let ready = deps.iter().all(|dep| placed.contains(dep));
            if ready && !placed.contains(id) {
                placed.push(id.clone());
            }
        }
        assert!(
            placed.len() > before,
            "gate-dag.txt has a cycle: {} of {} rows can be placed, the rest wait on each other",
            placed.len(),
            rows.len()
        );
    }
    for consumer in ["REL", "E2E"] {
        assert!(
            depends_on(&rows, consumer, "B3", 0),
            "{consumer} reads T_rel and does not depend on B3 (C2)"
        );
    }
}

// CI-F3.3b-2 (D-F3b, spec F3.3b-1b): merge-gate.sh hands the table to the planner (gate-dag.py) once, after its
// preamble (the lock, the server check and the sweep) and before the boundary line that test-times reads up to.
#[test]
fn merge_gate_runs_the_table_through_the_planner_once_after_its_preamble() {
    let gate = read("scripts/merge-gate.sh");
    let code: String = outside_self_test(&gate)
        .iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    let planner_call = "python3 -I \"$ROOT/scripts/gate-dag.py\" run --table \"$ROOT/scripts/gate-dag.txt\" --timed-end \"$TIMED_STEPS_END\"";
    let calls = code
        .lines()
        .filter(|line| line.contains(planner_call))
        .count();
    assert_eq!(
        calls, 1,
        "merge-gate.sh must run the planner over gate-dag.txt exactly once, with the boundary line it prints, found {calls}"
    );
    let sweep = first_code_line(&code, "gate_cluster_sweep")
        .expect("merge-gate.sh has no lane sweep outside its self-test");
    let planner = first_code_line(&code, planner_call).expect("planner call");
    // The planner prints the boundary before the first untimed row. merge-gate must not print it after the planner:
    // that would put the output of CV and M before the judge's boundary (CI-F3.3b-2).
    assert!(
        code.lines()
            .all(|line| line.trim() != "echo \"$TIMED_STEPS_END\""),
        "merge-gate.sh prints the boundary line itself, after the planner: the judge would read the untimed rows"
    );
    let judge = first_code_line(
        &code,
        "test-times.sh\" --judge \"$GATE_LOG\" --until \"$TIMED_STEPS_END\"",
    )
    .expect("merge-gate.sh does not judge the test times up to the boundary line");
    assert!(
        sweep < planner && planner < judge,
        "merge-gate.sh must run sweep ({sweep}), planner ({planner}) and the test times ({judge}) in that order"
    );
}

// CI-F3.3b-2 (spec F3.3b-1b-iii): the judge of the test times reads this gate's log up to the boundary line, and that
// line sits before the first untimed row of gate-dag.txt. The untimed rows are CV and M and only them, they come after
// every timed row in idx order, and the planner is the one that receives and prints the boundary.
#[test]
fn the_judge_of_the_test_times_stops_before_the_untimed_rows_and_only_they_are_untimed() {
    let mut rows = dag_rows();
    rows.sort_by_key(|row| row.0);
    let class_of = |line: &str| {
        line.split_whitespace()
            .nth(4)
            .unwrap_or_default()
            .to_string()
    };
    let untimed: Vec<String> = rows
        .iter()
        .filter(|row| class_of(row.3.as_str()) == "untimed")
        .map(|row| row.1.clone())
        .collect();
    assert_eq!(
        untimed,
        vec!["CV".to_string(), "M".to_string()],
        "the untimed rows of gate-dag.txt are CV and M: their output is past the judge's boundary"
    );
    let first_untimed = rows
        .iter()
        .position(|row| class_of(row.3.as_str()) == "untimed")
        .expect("gate-dag.txt has an untimed row");
    // CI-F3.3c: timed is every row that is not untimed, normal or exclusive. NLI is exclusive and timed: it runs last
    // in time, but its block is printed in idx order, before CV, so the boundary line still comes after it.
    assert!(
        rows[..first_untimed]
            .iter()
            .all(|row| matches!(class_of(row.3.as_str()).as_str(), "normal" | "exclusive")),
        "a timed row comes after the first untimed row in idx order: the boundary would cut it"
    );
    assert!(
        rows[..first_untimed].iter().any(|row| row.1 == "NLI"),
        "NLI is a timed row (exclusive) and its finished-in lines must be judged: it has to sit before the boundary"
    );
    let planner = read("scripts/gate-dag.py");
    assert!(
        planner.contains("\"--timed-end\""),
        "scripts/gate-dag.py has no --timed-end option: the boundary would not come from the planner"
    );
    assert!(
        planner.contains("\"untimed\""),
        "scripts/gate-dag.py does not know the class untimed of gate-dag.txt"
    );
}

// CI-F3.3b-2 (D-F3k, spec F3.3b-1b-ii): a step launched by the planner is `merge-gate.sh --step <id>`. The
// dispatch of the step mode is what decides between a step and the whole gate, and the whole gate (gate_main,
// the flow that takes the lock) is called only from its else branch: a step never reaches the lock.
#[test]
fn merge_gate_answers_the_step_mode_before_its_gate_runs() {
    let gate = read("scripts/merge-gate.sh");
    let code: String = outside_self_test(&gate)
        .iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    let step_mode = first_code_line(&code, "if [[ \"${1:-}\" == --step ]]; then")
        .expect("merge-gate.sh has no --step mode outside its self-test");
    let gate_run = code
        .lines()
        .position(|line| line.trim() == "gate_main")
        .expect("merge-gate.sh does not call gate_main, the whole gate, outside its self-test");
    assert!(
        step_mode < gate_run,
        "the --step mode is on line {} and the whole gate is called on line {}: a step must be answered before the gate, which takes the lock",
        step_mode + 1,
        gate_run + 1
    );
}

// CI-F3.3b-2 (spec F3.3b-1b, «la tabla»): the rows of the judge, with their order, their dependencies and
// their starting durations. The order of a row is its step; the dependencies are the spec's (DBG, B3, DC, CV and M
// wait for P; REL waits for B3; E2E waits for B3 and DBG). B1 does not enter the table.
// CI-F3.3c (D-F3l): NLI leaves DBG for its own row, exclusive, after DBG (it needs the target DBG built). Its clock
// assertion (nli_entailment, < 500 ms) measured 1199 ms under contention. The dur of NLI is not measured yet.
#[test]
fn the_step_table_has_the_nine_rows_of_the_judge_with_their_deps_and_durations() {
    // The class of CV and M is `untimed` (CI-F3.3b-2): their output is past the judge's boundary line.
    // CI-J15e (H10): the starting dur_s is not frozen here. It is the planner's first guess for a row with no history, and
    // the numbers move with every measured run; the planner refuses a row whose dur_s is not above 0, and so does this.
    let expected = [
        (10, "P", "-", "normal"),
        (20, "DBG", "P", "normal"),
        (25, "NLI", "DBG", "exclusive"),
        (30, "B3", "P", "normal"),
        (40, "REL", "B3", "normal"),
        (50, "E2E", "B3,DBG", "normal"),
        (60, "DC", "P", "normal"),
        (70, "CV", "P", "untimed"),
        (80, "M", "P", "untimed"),
    ];
    let table = read("scripts/gate-dag.txt");
    let rows: Vec<Vec<&str>> = table
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(
        rows.len(),
        expected.len(),
        "gate-dag.txt has {} rows, the judge has {}",
        rows.len(),
        expected.len()
    );
    for (row, (idx, id, deps, class)) in rows.iter().zip(expected) {
        assert_eq!(row[0], idx.to_string(), "row {id}: its idx");
        assert_eq!(row[1], id, "row {idx}: its id");
        assert_eq!(row[2], deps, "row {id}: its deps");
        assert_eq!(
            row[4], class,
            "row {id}: its class (untimed rows are past the judge's boundary)"
        );
        let dur: f64 = row[7].parse().unwrap_or_else(|_| {
            panic!("row {id}: its starting dur_s `{}` is not a number", row[7])
        });
        assert!(
            dur > 0.0,
            "row {id}: its starting dur_s is {dur}, and it has to be above 0"
        );
        // CI-F3.3c (D-F3j): leading NAME=value tokens are the step's environment, and the order is what follows them.
        let order: Vec<&str> = row[8..]
            .iter()
            .copied()
            .skip_while(|token| token.contains('='))
            .collect();
        assert_eq!(
            order.join(" "),
            format!("bash scripts/merge-gate.sh --step {id}"),
            "row {id}: its order"
        );
    }
    assert!(
        read("scripts/gate-dag.txt").lines().any(|line| {
            line.starts_with('#')
                && line.contains("criterio")
                && line.contains("cpu")
                && line.contains("ram")
        }),
        "gate-dag.txt must state the criterion of the cpu and ram of its rows in a comment"
    );
}

/// The NAME=value tokens that lead a row's order: the environment the planner gives that step (D-F3j).
fn row_env(line: &str) -> Vec<(String, String)> {
    line.split_whitespace()
        .skip(8)
        .take_while(|token| token.contains('='))
        .filter_map(|token| {
            token
                .split_once('=')
                .map(|(name, value)| (name.to_string(), value.to_string()))
        })
        .collect()
}

fn row_value(env: &[(String, String)], name: &str) -> Option<String> {
    env.iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

/// The hardware threads of the machine the table is written for (CI-F3.3c, D-F3m): the plan of production gives each ORT
/// pool half of them, up to the ceiling of that pool.
const TABLE_MACHINE_THREADS: usize = 24;

/// CI-J15b (H4): the ceilings of the ORT pools, read where the plan of production holds them: the
/// `const MAX_<POOL>_INTRA_THREADS: usize = N;` of rust/src/resources.rs, as (the variable of the pool, N).
fn plan_pool_ceilings(resources: &str) -> Vec<(String, usize)> {
    ["EMBED", "NLI", "RERANK"]
        .iter()
        .map(|pool| {
            let prefix = format!("const MAX_{pool}_INTRA_THREADS: usize = ");
            let value = resources
                .lines()
                .find_map(|line| line.trim().strip_prefix(prefix.as_str()))
                .unwrap_or_else(|| panic!("resources.rs has no `{prefix}N;`"))
                .trim_end_matches(';')
                .trim()
                .parse::<usize>()
                .unwrap_or_else(|_| {
                    panic!("the ceiling of the {pool} pool in resources.rs is not a whole number")
                });
            (format!("CUBA_{pool}_INTRA_THREADS"), value)
        })
        .collect()
}

/// What is wrong between the ORT pools the rows of `table` name and the plan of production (`resources`): a row with models
/// names each pool with min(half the table's machine, the ceiling), and E2E names none, because there the plan decides.
fn pool_problems(table: &str, resources: &str) -> Vec<String> {
    let ceilings = plan_pool_ceilings(resources);
    let mut problems = Vec::new();
    for line in table
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
    {
        let id = line.split_whitespace().nth(1).unwrap_or_default();
        let models = resources_of(line).iter().any(|r| r.as_str() == "models");
        let env = row_env(line);
        for (name, ceiling) in &ceilings {
            let want = if models && id != "E2E" {
                Some((TABLE_MACHINE_THREADS / 2).min(*ceiling).to_string())
            } else {
                None
            };
            let got = row_value(&env, name);
            if got != want {
                problems.push(format!(
                    "row {id}: {name} is {got:?} and the plan of production gives {want:?} (half of {TABLE_MACHINE_THREADS} threads, at most the {ceiling} of resources.rs)"
                ));
            }
        }
    }
    problems
}

/// CI-F3.3c (D-F3j, D-F3m, V6): a row that builds names its cargo jobs, and a row with models names the three ORT pools
/// with the values of the plan of production on this 24-thread machine (CI-J15b: read from rust/src/resources.rs, in
/// `the_ort_pools_of_the_rows_are_the_plan_of_production`). E2E names none. A row's cpu is the most it asks for (its jobs or
/// its widest pool), so the planner never admits a row that asks for more than the table says.
#[test]
fn each_row_declares_the_cargo_jobs_and_the_threads_it_asks_for() {
    let pools = plan_pool_ceilings(&read("rust/src/resources.rs"));
    for line in read("scripts/gate-dag.txt").lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let tok: Vec<&str> = line.split_whitespace().collect();
        let id = tok[1];
        let cpu: u32 = tok[5]
            .parse()
            .expect("the cpu column of a row is a whole number");
        let env = row_env(line);
        let jobs: Option<u32> = row_value(&env, "CARGO_BUILD_JOBS").map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("row {id}: CARGO_BUILD_JOBS is not a number: {line}"))
        });
        let recursos: Vec<&str> = tok[3].split(',').collect();
        let builds = recursos.iter().any(|r| r.starts_with("target:"));
        if builds {
            let n = jobs.unwrap_or_else(|| {
                panic!("row {id} builds in a target and names no CARGO_BUILD_JOBS: {line}")
            });
            assert!(
                (1..=cpu).contains(&n),
                "row {id}: CARGO_BUILD_JOBS={n} is not between 1 and its cpu {cpu}: {line}"
            );
        }
        let mut widest = jobs.unwrap_or(0);
        for (name, _) in &pools {
            if let Some(v) = row_value(&env, name) {
                widest = widest.max(v.parse().expect("a pool is a whole number"));
            }
        }
        assert!(
            cpu >= widest,
            "row {id}: cpu {cpu} is less than the widest thing it asks for ({widest}): {line}"
        );
    }
}

/// CI-F3.3c (D-F3l, V9, V10) and CI-J12: the NLI clock assertion runs in a row of its own, exclusive, after DBG (it needs
/// DBG's target built), and DBG no longer names it. Its tests open no database, but run-all-tests refuses to run without
/// the judge's server (gate_require_judge_server), so the row runs in a cluster of its own, as the other rows that call it.
#[test]
fn the_nli_clock_assertion_runs_alone_in_an_exclusive_row_with_its_own_cluster() {
    let table = read("scripts/gate-dag.txt");
    let row_of = |id: &str| -> String {
        table
            .lines()
            .find(|line| line.split_whitespace().nth(1) == Some(id))
            .unwrap_or_else(|| panic!("gate-dag.txt has no row {id}"))
            .to_string()
    };
    let nli = row_of("NLI");
    let tok: Vec<&str> = nli.split_whitespace().collect();
    assert_eq!(
        tok[2], "DBG",
        "NLI waits for DBG, which builds its binaries: {nli}"
    );
    assert_eq!(
        tok[4], "exclusive",
        "NLI measured 1199 ms under contention against a 500 ms assertion, so it runs alone: {nli}"
    );
    assert!(
        tok[3].split(',').any(|r| r == "cluster:C_nli"),
        "NLI runs with the judge's server, so its row names a cluster of its own (CI-J12): {nli}"
    );
    let gate = read("scripts/merge-gate.sh");
    let nli_body = step_body(&gate, "NLI").expect("merge-gate.sh has a step_NLI()");
    assert_eq!(
        lanes_in(&nli_body),
        vec!["NLI".to_string()],
        "step_NLI runs the NLI lane alone"
    );
    assert!(
        nli_body.iter().any(|line| line.contains("run_in_lane nli")),
        "step_NLI runs its lane in a cluster of its own (CI-J12): run-all-tests refuses to run without the judge's server"
    );
    let dbg_body = step_body(&gate, "DBG").expect("merge-gate.sh has a step_DBG()");
    assert_eq!(
        lanes_in(&dbg_body),
        ["CL", "D1", "D2", "D3"].map(String::from).to_vec(),
        "step_DBG no longer runs the NLI lane: it has its own row"
    );
    // CI-J15e (H9): there is no `NLI) step_NLI ;;` arm any more: the dispatch is declare -F "step_$id", and the contract
    // of the dispatch (`the_dispatch_of_a_step_is_derived_from_its_function_...`) holds that every row has its function.
}

#[test]
fn no_judge_script_names_the_gate_audit_outside_its_comments() {
    for script in ["scripts/merge-gate.sh", "scripts/run-all-tests.sh"] {
        for (number, line) in read(script).lines().enumerate() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            assert!(
                !line.contains("gate-audit.sh"),
                "{script}:{}: a judge script names gate-audit.sh outside a comment; it is a manual tool (D-A9)",
                number + 1
            );
        }
    }
}

// --- CI-J13 (D1): an exception of the judge has an owner and a day it expires -----------------------------

/// `YYYY-MM-DD` and nothing else, so that two of them compare as text.
fn is_iso_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(at, byte)| {
            if at == 4 || at == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
        && (1..=12).contains(&text[5..7].parse::<u32>().unwrap_or(0))
        && (1..=31).contains(&text[8..10].parse::<u32>().unwrap_or(0))
}

/// Whether an exception that expires on `expires` is expired on `today`: the day after its date, like
/// advisory-exceptions.txt. Both are `YYYY-MM-DD` (`is_iso_date`). The caller hands the date in, so nothing
/// that tests this depends on the clock.
fn expired(expires: &str, today: &str) -> bool {
    expires < today
}

/// What is wrong with the entries `(name, owner, expires)` of the list `list`: each problem names its entry,
/// and for a date, the date.
fn exception_problems(list: &str, entries: &[(&str, &str, &str)], today: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for &(name, owner, expires) in entries {
        if owner.trim().is_empty() {
            problems.push(format!("{list}: {name} has no owner"));
        }
        if !is_iso_date(expires) {
            problems.push(format!(
                "{list}: {name} expires on `{expires}`, which is not YYYY-MM-DD"
            ));
        } else if expired(expires, today) {
            problems.push(format!(
                "{list}: {name} expired on {expires} (today is {today})"
            ));
        }
    }
    problems
}

/// CI-J13. The day an exception expires is the last day it holds: the next one is red, across a month and a year.
#[test]
fn an_exception_expires_the_day_after_its_date() {
    assert!(expired("2026-11-08", "2026-11-09"));
    assert!(!expired("2026-11-09", "2026-11-09"));
    assert!(!expired("2026-11-10", "2026-11-09"));
    assert!(expired("2026-12-31", "2027-01-01"));
    assert!(!expired("2027-01-01", "2026-12-31"));
    assert!(is_iso_date("2026-11-09"));
    for not_a_date in [
        "",
        "2026-11-9",
        "2026-13-01",
        "2026-00-10",
        "2026-11-32",
        "09/11/2026",
        "2026-11-09x",
        "2026_11_09",
    ] {
        assert!(!is_iso_date(not_a_date), "`{not_a_date}` is not YYYY-MM-DD");
    }
}

/// CI-J13. The check of the lists is judged first, on entries that must and must not be red: an expired one, one
/// with no owner, one with a date that is not a date, and one that is fine.
#[test]
fn the_check_of_the_exceptions_can_fail() {
    let fixture = [
        ("OLD", "dueno", "2026-11-08"),
        ("NOBODY", "  ", "2027-01-01"),
        ("BADDATE", "dueno", "9/11/2026"),
        ("GOOD", "dueno", "2026-11-09"),
    ];
    let problems = exception_problems("FIXTURE", &fixture, "2026-11-09");
    assert_eq!(problems.len(), 3, "{problems:#?}");
    assert!(
        problems
            .iter()
            .any(|p| p.contains("OLD") && p.contains("2026-11-08")),
        "an expired entry is named with its date: {problems:#?}"
    );
    assert!(
        problems.iter().any(|p| p.contains("NOBODY")),
        "an entry with a blank owner is named: {problems:#?}"
    );
    assert!(
        problems.iter().any(|p| p.contains("BADDATE")),
        "an entry whose date is not a date is named: {problems:#?}"
    );
    assert!(
        !problems.iter().any(|p| p.contains("GOOD")),
        "an entry that expires today still holds: {problems:#?}"
    );
}

/// CI-J13 (verificacion.md: every exception has an owner and a day it expires). The exceptions this judge keeps
/// in its contracts, NOT_HANDED_DOWN and MODEL_NAMES_ELSEWHERE, are red when one expired by today's date or has
/// no owner. The stale-entry checks of each list stay where they are.
#[test]
fn the_exceptions_of_the_judge_have_an_owner_and_have_not_expired() {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let not_handed: Vec<(&str, &str, &str)> = NOT_HANDED_DOWN
        .iter()
        .map(|&(name, _, owner, expires)| (name, owner, expires))
        .collect();
    let elsewhere: Vec<(&str, &str, &str)> = MODEL_NAMES_ELSEWHERE
        .iter()
        .map(|&(file, _, owner, expires)| (file, owner, expires))
        .collect();
    assert!(
        !not_handed.is_empty() && !elsewhere.is_empty(),
        "a list of exceptions read as empty: the check would be green over nothing"
    );
    let mut problems = exception_problems("NOT_HANDED_DOWN", &not_handed, &today);
    problems.extend(exception_problems(
        "MODEL_NAMES_ELSEWHERE",
        &elsewhere,
        &today,
    ));
    assert!(
        problems.is_empty(),
        "an exception without an owner, or past its day, is not an exception any more: renew it with a \
         reason or remove it: {problems:#?}"
    );
}

// --- CI-J13 (D3): the fixtures of gate-audit.sh name rows that exist ---------------------------------------

/// What a line of the self-test of gate-audit.sh says of a row of the judge's table. Only the columns the
/// line spells out are set.
struct RowMention {
    line: usize,
    id: String,
    idx: Option<String>,
    deps: Option<String>,
    resources: Option<String>,
    class: Option<String>,
}

fn row_mention(line: usize, id: &str) -> RowMention {
    RowMention {
        line,
        id: id.to_string(),
        idx: None,
        deps: None,
        resources: None,
        class: None,
    }
}

/// A step id as the table writes it: `P`, `DBG`, `B3`, `E2E`.
fn is_step_id(word: &str) -> bool {
    let mut chars = word.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// The lines between the self-test markers of a script, the markers left out.
fn inside_self_test(body: &str) -> Vec<&str> {
    let mut inside = false;
    let mut out = Vec::new();
    for line in body.lines() {
        if line == SELF_TEST_BEGIN {
            inside = true;
        } else if line == SELF_TEST_END {
            inside = false;
        } else if inside {
            out.push(line);
        }
    }
    out
}

/// The rows of the table that the fixtures in `lines` name, and what they say of them: the `roots`, `steps` and
/// `overlap` heredocs of the standard table, the `au_tree` specs (`"<idx> <id> <deps> <recursos> <clase>"`),
/// `tr_lane` calls and the arms of its `case`, the `<idx>-<id>` directories of the audit, `--step <id>`, the
/// id the fake step is given and the `exit-<id>` file that makes it fail.
fn fixture_mentions(lines: &[&str]) -> Vec<RowMention> {
    let mut found = Vec::new();
    let mut heredoc: Option<String> = None;
    let mut joined = String::new();
    for (at, raw) in lines.iter().copied().enumerate() {
        let line = at + 1;
        let text = raw.trim();
        if let Some(tag) = &heredoc {
            if text == tag.as_str() {
                heredoc = None;
                continue;
            }
            let words: Vec<&str> = text.split_whitespace().collect();
            match (tag.as_str(), words.as_slice()) {
                ("ROOTS", ["run", id, ..]) => found.push(row_mention(line, id)),
                ("STEPS", [id, resources, ..]) => found.push(RowMention {
                    resources: Some(resources.to_string()),
                    ..row_mention(line, id)
                }),
                ("OVERLAP", [a, b]) => {
                    found.push(row_mention(line, a));
                    found.push(row_mention(line, b));
                }
                _ => {}
            }
            continue;
        }
        if let Some(rest) = text.split("<<'").nth(1) {
            heredoc = rest.split('\'').next().map(str::to_string);
            continue;
        }
        if let Some(head) = text.strip_suffix('\\') {
            joined.push_str(head);
            joined.push(' ');
            continue;
        }
        joined.push_str(text);
        let statement = std::mem::take(&mut joined);
        let words: Vec<&str> = statement.split_whitespace().collect();
        for (w, word) in words.iter().enumerate() {
            let next = words.get(w + 1).copied().unwrap_or("");
            let id = match *word {
                "tr_lane" => words.get(w + 3).copied().unwrap_or(""),
                "--step" => next,
                other if other.trim_matches('"').ends_with("fake-step.sh") => next,
                _ => continue,
            };
            if is_step_id(id) {
                found.push(row_mention(line, id));
            }
        }
        if let Some(arm) = text.split(')').next().filter(|_| text.contains(')')) {
            if is_step_id(arm) {
                found.push(row_mention(line, arm));
            }
        }
        if let Some(from) = statement.find("au_tree \"") {
            for spec in statement[from..].split('"').skip(1).step_by(2) {
                let cols: Vec<&str> = spec.split_whitespace().collect();
                if let [idx, id, deps, resources, class] = cols.as_slice() {
                    if idx.parse::<u32>().is_ok() {
                        found.push(RowMention {
                            idx: Some(idx.to_string()),
                            deps: Some(deps.to_string()),
                            resources: Some(resources.to_string()),
                            class: Some(class.to_string()),
                            ..row_mention(line, id)
                        });
                    }
                }
            }
        }
        for token in statement.split(|c: char| c == '/' || c == '"' || c.is_whitespace()) {
            if let Some(id) = token.strip_prefix("exit-").filter(|id| is_step_id(id)) {
                found.push(row_mention(line, id));
            }
            if let Some((idx, id)) = token.split_once('-') {
                if !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()) && is_step_id(id) {
                    found.push(RowMention {
                        idx: Some(idx.to_string()),
                        ..row_mention(line, id)
                    });
                }
            }
        }
    }
    found
}

/// What is wrong between the fixtures in `lines` and the rows of the table (`idx id deps recursos clase ...`, one
/// `Vec` of fields per row): a row that does not exist, or a column that the fixture spells another way.
fn fixture_problems(lines: &[&str], table: &[Vec<String>]) -> Vec<String> {
    let mut problems = Vec::new();
    for mention in fixture_mentions(lines) {
        let Some(row) = table.iter().find(|row| row[1] == mention.id) else {
            problems.push(format!(
                "self-test line {}: `{}` is not a row of gate-dag.txt",
                mention.line, mention.id
            ));
            continue;
        };
        for (column, said, real) in [
            ("idx", &mention.idx, &row[0]),
            ("deps", &mention.deps, &row[2]),
            ("recursos", &mention.resources, &row[3]),
            ("clase", &mention.class, &row[4]),
        ] {
            if let Some(said) = said.as_ref().filter(|said| *said != real) {
                problems.push(format!(
                    "self-test line {}: {} says {column} `{said}`, gate-dag.txt says `{real}`",
                    mention.line, mention.id
                ));
            }
        }
    }
    problems
}

/// CI-J13. The extraction is judged first, on a fixture with the ways to be stale: a row that does not exist, a
/// resource that the table spells another way, an idx that is not the row's, and a clean line to prove it does
/// not cry wolf.
#[test]
fn the_check_of_the_audit_fixtures_can_fail() {
    let table: Vec<Vec<String>> = [
        "20 DBG P target:T_dbg,cluster:C_dbg,models normal",
        "60 DC P target:T_docs,cluster:C_docs normal",
    ]
    .iter()
    .map(|row| row.split_whitespace().map(String::from).collect())
    .collect();
    let fixture = [
        "  au_std() {",
        "    cat > \"$1/steps\" <<'STEPS'",
        "DBG target:T_dbg,cluster:C_dbg,models merge-gate\\.sh --step DBG$",
        "GHOST target:T_dbg merge-gate\\.sh --step GHOST$",
        "STEPS",
        "    cat > \"$1/overlap\" <<'OVERLAP'",
        "DBG DC",
        "OVERLAP",
        "    tr_lane \"$d/tr\" 100 DC 101 beta-0123456789abcdef",
        "    local d; d=\"$(rd)\"; au_tree \"$d\" \"20 DBG P target:T_dbg,cluster:C_dbg,models normal\" \\",
        "      \"60 DC P target:T_dbg,cluster:C_docs normal\"",
        "    tr_open \"$d/fx/20-DBG\" 101 w x",
        "    tr_open \"$d/fx/50-DC\" 101 w x",
    ];
    let problems = fixture_problems(&fixture, &table);
    assert_eq!(problems.len(), 3, "{problems:#?}");
    assert!(
        problems.iter().any(|p| p.contains("GHOST")),
        "an id that is not a row is named: {problems:#?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("DC says recursos `target:T_dbg,cluster:C_docs`")),
        "a stale resource is named with both spellings: {problems:#?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("DC says idx `50`") && p.contains("`60`")),
        "a stale idx in a directory name is named: {problems:#?}"
    );
    let mentioned: Vec<String> = fixture_mentions(&fixture)
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(
        mentioned.iter().filter(|id| id.as_str() == "DC").count() >= 4,
        "DC is named by the overlap heredoc, tr_lane, au_tree and a directory: {mentioned:?}"
    );
}

/// CI-J13. The ids and columns that the fixtures of `gate-audit.sh --self-test` name are the rows of
/// `scripts/gate-dag.txt`: `DC target:T_dbg` and the D1, D2, R and E steps stayed in them after the table moved
/// on, and the audit went on passing over a table that no longer exists.
#[test]
fn the_fixtures_of_the_audit_name_rows_that_exist() {
    let body = read("scripts/gate-audit.sh").replace("\r\n", "\n");
    let lines = inside_self_test(&body);
    let table: Vec<Vec<String>> = dag_rows()
        .into_iter()
        .map(|(_, _, _, line)| line.split_whitespace().map(String::from).collect())
        .collect();
    let mentions = fixture_mentions(&lines);
    let named: std::collections::BTreeSet<&str> = mentions.iter().map(|m| m.id.as_str()).collect();
    assert!(
        named.len() >= 5
            && mentions.iter().any(|m| m.deps.is_some())
            && mentions
                .iter()
                .any(|m| m.resources.is_some() && m.deps.is_none()),
        "the scan of the self-test of gate-audit.sh found {named:?}: it no longer reads the heredocs and the \
         au_tree specs, so it would be green over nothing (the audit does not write <idx>-<id> directories any more, \
         since CI-J15a: it runs merge-gate.sh once and the planner starts the rows)"
    );
    let problems = fixture_problems(&lines, &table);
    assert!(
        problems.is_empty(),
        "the fixtures of gate-audit.sh --self-test are not the table of gate-dag.txt: {problems:#?}"
    );
}

/// CI-F5c (D-F5w, D-F5x): `memory-industry` and `cuba-memorys` are two `[[bin]]` of one file. Each gets its own test
/// harness unless it says `test = false`, and `main.rs` has no tests, so every harness is a compile that runs 0 tests.
/// G1 keeps the flag on both; G2 keeps `main.rs` from growing code that the flag would stop from ever running.
struct ManifestBin {
    name: String,
    path: String,
    test_false: bool,
}

fn manifest_bins(manifest: &str) -> Vec<ManifestBin> {
    let mut bins: Vec<ManifestBin> = Vec::new();
    let mut in_bin = false;
    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_bin = line == "[[bin]]";
            if in_bin {
                bins.push(ManifestBin {
                    name: String::new(),
                    path: String::new(),
                    test_false: false,
                });
            }
            continue;
        }
        let (Some(bin), Some((key, value))) =
            (bins.last_mut().filter(|_| in_bin), line.split_once('='))
        else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "name" => bin.name = value.to_string(),
            "path" => bin.path = value.to_string(),
            "test" => bin.test_false = value == "false",
            _ => {}
        }
    }
    bins
}

/// Every way the two bins can stop being what G1 requires, one line each. Empty means green.
fn bin_harness_problems(manifest: &str) -> Vec<String> {
    let bins = manifest_bins(manifest);
    if bins.len() != 2 {
        return vec![format!("found {} [[bin]], expected 2", bins.len())];
    }
    let mut problems = Vec::new();
    for (i, bin) in bins.iter().enumerate() {
        let other = &bins[1 - i];
        if bin.path != "src/main.rs" || other.path != "src/main.rs" {
            problems.push(format!(
                "[[bin]] {} has path {:?} and {} has {:?}: both must be src/main.rs, or this guard has nothing to guard",
                bin.name, bin.path, other.name, other.path
            ));
        } else if !bin.test_false {
            problems.push(format!(
                "{} shares {} with {} and has no test = false",
                bin.name, bin.path, other.name
            ));
        }
    }
    let mut names: Vec<&str> = bins.iter().map(|b| b.name.as_str()).collect();
    names.sort_unstable();
    if names != ["cuba-memorys", "memory-industry"] {
        problems.push(format!(
            "[[bin]] names are {names:?}, expected cuba-memorys and memory-industry"
        ));
    }
    problems
}

/// The attribute text of every `#[...]` on the line, with string literals cut out (`rename = "test"` is not a test).
fn attributes_in(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("#[") {
        let after = &rest[start + 2..];
        let end = after.find(']').unwrap_or(after.len());
        out.push(after[..end].split('"').step_by(2).collect::<String>());
        rest = &after[end..];
    }
    out
}

fn names_a_test(attribute: &str) -> bool {
    attribute
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|word| word == "test")
}

fn declares_out_of_line_module(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    if let Some(after_pub) = rest.strip_prefix("pub") {
        rest = after_pub.trim_start();
        if rest.starts_with('(') {
            rest = rest
                .split_once(')')
                .map_or("", |(_, after)| after)
                .trim_start();
        }
    }
    let name = rest.strip_prefix("mod ")?.trim().strip_suffix(';')?;
    Some(name.trim())
}

/// What a bin with `test = false` must not contain: test code and `mod x;` (its file would be one more place for tests).
fn test_code_problems(path: &str, source: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let number = index + 1;
        if line.trim_start().starts_with("//") {
            continue;
        }
        if attributes_in(line).iter().any(|a| names_a_test(a)) {
            problems.push(format!(
                "{path}:{number}: a test in a bin with test = false never runs"
            ));
        }
        if let Some(name) = declares_out_of_line_module(line) {
            problems.push(format!("{path}:{number}: mod {name}; would hide its tests"));
        }
    }
    problems
}

const TWO_BINS_WITH_THE_FLAG: &str = "\
[package]
name = \"memory-industry\"

[[bin]]
name = \"memory-industry\"
path = \"src/main.rs\"
test = false

[[bin]]
name = \"cuba-memorys\"
path = \"src/main.rs\"
test = false

[dependencies]
half = \"2\"
";

#[test]
fn los_bins_that_share_main_rs_all_carry_test_false() {
    let problems = bin_harness_problems(&read("rust/Cargo.toml"));
    assert!(
        problems.is_empty(),
        "rust/Cargo.toml: every [[bin]] compiled from src/main.rs needs test = false, or cargo builds a test harness per bin that runs 0 tests (CI-F5c):\n{}",
        problems.join("\n")
    );
}

#[test]
fn los_bins_with_test_false_hide_no_tests_in_their_source() {
    let manifest = read("rust/Cargo.toml");
    let bins = manifest_bins(&manifest);
    assert!(
        !bins.is_empty(),
        "rust/Cargo.toml has no [[bin]]: G2 would check nothing"
    );
    let mut paths: Vec<&str> = bins.iter().map(|b| b.path.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    let mut problems = Vec::new();
    for path in paths {
        let source = read(&format!("rust/{path}"));
        assert!(
            source.contains("fn main"),
            "rust/{path} is declared as a [[bin]] but has no `fn main`: G2 is reading the wrong file"
        );
        problems.extend(test_code_problems(path, &source));
    }
    assert!(
        problems.is_empty(),
        "a [[bin]] with test = false is never compiled as a test; this code would stop running:\n{}",
        problems.join("\n")
    );
}

#[test]
fn los_bins_guard_g1_goes_red_on_the_bin_that_lacks_test_false_and_only_on_it() {
    assert_eq!(
        bin_harness_problems(TWO_BINS_WITH_THE_FLAG),
        Vec::<String>::new()
    );
    let sabotaged = TWO_BINS_WITH_THE_FLAG.replacen(
        "path = \"src/main.rs\"\ntest = false\n\n[dependencies]",
        "path = \"src/main.rs\"\n\n[dependencies]",
        1,
    );
    assert_ne!(
        sabotaged, TWO_BINS_WITH_THE_FLAG,
        "the fixture sabotage must change the manifest"
    );
    assert_eq!(
        bin_harness_problems(&sabotaged),
        ["cuba-memorys shares src/main.rs with memory-industry and has no test = false"]
    );
    let flag_true = TWO_BINS_WITH_THE_FLAG.replacen("test = false", "test = true", 1);
    assert_eq!(
        bin_harness_problems(&flag_true),
        ["memory-industry shares src/main.rs with cuba-memorys and has no test = false"]
    );
}

#[test]
fn los_bins_guard_g1_never_goes_green_by_finding_fewer_than_two_bins() {
    let renamed = TWO_BINS_WITH_THE_FLAG.replacen("[[bin]]", "[[binx]]", 1);
    assert_eq!(
        bin_harness_problems(&renamed),
        ["found 1 [[bin]], expected 2"]
    );
    assert_eq!(
        bin_harness_problems("[package]\nname = \"x\"\n"),
        ["found 0 [[bin]], expected 2"]
    );
    let third = format!(
        "{TWO_BINS_WITH_THE_FLAG}\n[[bin]]\nname = \"third\"\npath = \"src/third.rs\"\ntest = false\n"
    );
    assert_eq!(
        bin_harness_problems(&third),
        ["found 3 [[bin]], expected 2"]
    );
    let moved =
        TWO_BINS_WITH_THE_FLAG.replacen("path = \"src/main.rs\"", "path = \"src/other.rs\"", 1);
    assert!(
        bin_harness_problems(&moved)
            .iter()
            .any(|p| p.contains("both must be src/main.rs")),
        "bins that no longer share src/main.rs leave G2 guarding the wrong file"
    );
}

#[test]
fn los_bins_guard_g2_goes_red_on_test_code_and_on_out_of_line_modules() {
    let source = "fn main() {}\n\n#[cfg(test)] mod t { #[test] fn x() {} }\n";
    assert_eq!(
        test_code_problems("src/main.rs", source),
        ["src/main.rs:3: a test in a bin with test = false never runs"]
    );
    let source = "fn main() {}\nmod extra;\npub mod more;\npub(crate) mod third;\n";
    assert_eq!(
        test_code_problems("src/main.rs", source),
        [
            "src/main.rs:2: mod extra; would hide its tests",
            "src/main.rs:3: mod more; would hide its tests",
            "src/main.rs:4: mod third; would hide its tests",
        ]
    );
    for attribute in [
        "#[test]",
        "#[tokio::test]",
        "#[tokio::test(flavor = \"multi_thread\")]",
        "#[cfg(all(test, unix))]",
        "#[cfg_attr(test, derive(Debug))]",
    ] {
        let source = format!("fn main() {{}}\n    {attribute}\nfn t() {{}}\n");
        assert_eq!(
            test_code_problems("src/main.rs", &source),
            ["src/main.rs:2: a test in a bin with test = false never runs"],
            "{attribute} must be caught"
        );
    }
}

#[test]
fn los_bins_guard_g2_lets_ordinary_code_through() {
    let source = "\
#[global_allocator]
static A: u8 = 0;
// #[test] in a comment, and mod gone;
/// #[cfg(test)]
#[cfg(unix)]
#[cfg(not(unix))]
#[serde(rename = \"test\")]
mod inline_is_fine {}
fn main() { let mod_x = 1; let test = 2; }
";
    assert_eq!(
        test_code_problems("src/main.rs", source),
        Vec::<String>::new()
    );
}

// --- CI-F3.4: the latency list, the resources of the rows and the release binary of the E2E ------------------------
// D-F34g..F34p. scripts/gate-latency.txt names, one line per file under rust/, every file whose tests assert on a
// wall clock, with the class the file needs in the judge: `exclusive` runs alone (the NLI row), `shared` can run
// beside other steps. The detector below is the one the list is kept against, and this file is read by it too.

/// The macro the detector looks for, in two pieces: a literal of it followed by an open parenthesis anywhere in this
/// file would be a wall-clock assert of its own, and the file would need a line in the list.
const ASSERT_MACRO: [&str; 2] = ["assert", "!"];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `word` is in `text` as a whole word.
fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    })
}

/// The names a line of `text` binds with `let <name> = … .elapsed() …`, up to the first `;` of the line: the form
/// `let per_call = t0.elapsed() / 5;` that a search of the assert alone does not see.
fn elapsed_bindings(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        for (at, _) in line.match_indices("let") {
            let rest = &line[at + 3..];
            let after_let = rest.trim_start();
            if after_let.len() == rest.len() {
                continue;
            }
            let after_mut = after_let
                .strip_prefix("mut")
                .filter(|more| more.starts_with(char::is_whitespace))
                .map_or(after_let, str::trim_start);
            let name: String = after_mut.chars().take_while(|c| is_word_char(*c)).collect();
            if name.is_empty() {
                continue;
            }
            let after_name = after_mut[name.len()..].trim_start();
            let value = if let Some(typed) = after_name.strip_prefix(':') {
                match typed.find('=') {
                    Some(equals) if equals > 0 => &typed[equals + 1..],
                    _ => continue,
                }
            } else if let Some(value) = after_name.strip_prefix('=') {
                value
            } else {
                continue;
            };
            if value.split(';').next().unwrap_or("").contains(".elapsed()") {
                names.push(name);
            }
        }
    }
    names
}

/// D-F34l: the 1-based lines of `text` where a wall-clock assert starts. A `debug_` or plain assert macro whose
/// argument, with its parentheses balanced, has `<` or `>` and either `.elapsed()` itself or a name bound on one line
/// to an `.elapsed()` in the same file. A timeout around a future and an `.elapsed()` that is only printed are not.
fn clock_asserts(text: &str) -> Vec<usize> {
    if !text.contains(".elapsed()") {
        return Vec::new();
    }
    let bound = elapsed_bindings(text);
    let needle = ASSERT_MACRO.concat();
    let mut lines = Vec::new();
    for (at, _) in text.match_indices(&needle) {
        let start = if text[..at].ends_with("debug_") {
            at - "debug_".len()
        } else {
            at
        };
        if text[..start].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let after = &text[at + needle.len()..];
        let args = after.trim_start();
        if !args.starts_with('(') {
            continue;
        }
        let body_start = at + needle.len() + (after.len() - args.len()) + 1;
        let rest = &text[body_start..];
        let mut depth = 1;
        let length = rest
            .bytes()
            .position(|byte| {
                match byte {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .map_or(rest.len(), |at| at + 1);
        let body = &rest[..length];
        let compares = body.contains('<') || body.contains('>');
        let on_a_clock =
            body.contains(".elapsed()") || bound.iter().any(|name| contains_word(body, name));
        if compares && on_a_clock {
            lines.push(text[..start].matches('\n').count() + 1);
        }
    }
    lines
}

/// The Rust files of `dir`, and of the folders under it when `deep`.
fn rust_files(dir: &Path, deep: bool) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if deep {
                files.extend(rust_files(&path, true));
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files
}

/// Every file with a wall-clock assert, as `tests/<file>.rs` or `src/<path>.rs` (relative to rust/), with its lines.
fn files_with_clock_asserts() -> std::collections::BTreeMap<String, Vec<usize>> {
    let rust = repo_root().join("rust");
    let mut found = std::collections::BTreeMap::new();
    for (folder, deep) in [("tests", false), ("src", true)] {
        for path in rust_files(&rust.join(folder), deep) {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let lines = clock_asserts(&text);
            if !lines.is_empty() {
                let name = path
                    .strip_prefix(&rust)
                    .expect("under rust/")
                    .to_string_lossy()
                    .replace('\\', "/");
                found.insert(name, lines);
            }
        }
    }
    found
}

/// One line of scripts/gate-latency.txt (D-F34k): file, class, evidence, owner, date, and the reason.
struct LatencyLine {
    number: usize,
    file: String,
    class: String,
    evidence: String,
    owner: String,
    date: String,
    reason: String,
}

fn latency_lines() -> Vec<LatencyLine> {
    read("scripts/gate-latency.txt")
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .map(|(index, line)| {
            let mut fields = line.split_whitespace();
            let mut next = |what: &str| {
                fields
                    .next()
                    .unwrap_or_else(|| {
                        panic!(
                            "scripts/gate-latency.txt:{}: no {what}; a line is `file class evidence owner date reason`: {line}",
                            index + 1
                        )
                    })
                    .to_string()
            };
            let file = next("file");
            let class = next("class");
            let evidence = next("evidence");
            let owner = next("owner");
            let date = next("date");
            let reason = fields.collect::<Vec<_>>().join(" ");
            LatencyLine {
                number: index + 1,
                file,
                class,
                evidence,
                owner,
                date,
                reason,
            }
        })
        .collect()
}

#[test]
fn the_latency_list_names_every_file_with_a_wall_clock_assert_and_only_those() {
    let found = files_with_clock_asserts();
    assert!(
        !found.is_empty(),
        "the detector found no file with a wall-clock assert under rust/tests and rust/src: it sees nothing, so a list kept against it proves nothing"
    );
    let listed = latency_lines();
    for file in found.keys() {
        assert!(
            listed.iter().any(|line| line.file == *file),
            "{file} has a wall-clock assert and no line in scripts/gate-latency.txt"
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    for line in &listed {
        assert!(
            repo_root().join("rust").join(&line.file).is_file(),
            "scripts/gate-latency.txt:{}: {} is not a file under rust/",
            line.number,
            line.file
        );
        assert!(
            found.contains_key(&line.file),
            "{} has no wall-clock assert, and scripts/gate-latency.txt:{} lists it",
            line.file,
            line.number
        );
        assert!(
            seen.insert(line.file.clone()),
            "scripts/gate-latency.txt:{}: {} is listed twice",
            line.number,
            line.file
        );
    }
}

/// D-F34l, the four forms. The macro is written ASSERT in the fixtures and put back before they are read, so this file
/// does not hold the text the detector looks for.
#[test]
fn the_clock_detector_sees_the_form_with_a_variable_and_not_what_is_only_printed() {
    let forms = [
        (
            "an elapsed inside the assert",
            "ASSERT!(started.elapsed() < Duration::from_secs(3), \"slow\");",
            true,
        ),
        (
            "binding form",
            "let took = t.elapsed();\nwork();\nASSERT!(\n    took < LIMIT,\n    \"slow\"\n);",
            true,
        ),
        (
            "a measure that is only printed",
            "let dt = t0.elapsed();\neprintln!(\"{dt:?}\");",
            false,
        ),
        (
            "an assert on a number",
            "assert_eq!(report.warmup_ms, 0.0);",
            false,
        ),
    ];
    for (name, text, want) in forms {
        let detected = !clock_asserts(&text.replace("ASSERT", "assert")).is_empty();
        assert_eq!(
            detected,
            want,
            "{name} {}: the detector of D-F34l has to {} it",
            if detected { "detected" } else { "not detected" },
            if want { "see" } else { "leave" }
        );
    }
}

/// The kind of evidence a line carries (D-F34k): a ratio with two decimals, `dag-verde:<n>` with n >= 1, or pending.
fn evidence_kind(evidence: &str) -> Option<&'static str> {
    let digits = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
    if evidence == "pendiente" {
        return Some("pending");
    }
    if let Some(runs) = evidence.strip_prefix("dag-verde:") {
        return (digits(runs) && runs.parse::<u32>().is_ok_and(|n| n >= 1)).then_some("dag");
    }
    let (whole, decimals) = evidence.split_once('.')?;
    (digits(whole) && decimals.len() == 2 && digits(decimals)).then_some("ratio")
}

fn latency_date(line: &LatencyLine) -> chrono::NaiveDate {
    assert!(
        line.date.len() == 10,
        "scripts/gate-latency.txt:{}: the date {} is not AAAA-MM-DD",
        line.number,
        line.date
    );
    chrono::NaiveDate::parse_from_str(&line.date, "%Y-%m-%d").unwrap_or_else(|e| {
        panic!(
            "scripts/gate-latency.txt:{}: the date {} is not AAAA-MM-DD: {e}",
            line.number, line.date
        )
    })
}

/// The last day a pending line may name (D-F34k): the lot that closes F3.
const PENDING_LAST_DAY: (i32, u32, u32) = (2026, 11, 30);

#[test]
fn every_latency_line_has_its_five_fields_and_a_pending_one_has_a_date_to_come() {
    let today = chrono::Utc::now().date_naive();
    let last =
        chrono::NaiveDate::from_ymd_opt(PENDING_LAST_DAY.0, PENDING_LAST_DAY.1, PENDING_LAST_DAY.2)
            .expect("a real day");
    let lines = latency_lines();
    assert!(
        !lines.is_empty(),
        "scripts/gate-latency.txt has no line: a list with nothing in it proves nothing"
    );
    for line in &lines {
        let at = format!("scripts/gate-latency.txt:{}", line.number);
        assert!(
            matches!(line.class.as_str(), "exclusive" | "shared"),
            "{at}: the class of {} is {}, not exclusive or shared",
            line.file,
            line.class
        );
        let kind = evidence_kind(&line.evidence).unwrap_or_else(|| {
            panic!(
                "{at}: the evidence of {} is {}: a ratio with two decimals, dag-verde:<n> with n of 1 or more, or pendiente",
                line.file, line.evidence
            )
        });
        assert!(
            !line.owner.is_empty() && !line.reason.is_empty(),
            "{at}: {} needs an owner and a reason",
            line.file
        );
        let date = latency_date(line);
        if kind == "pending" {
            assert!(
                date > today,
                "pending line {} expired on {} ({at}; today is {today})",
                line.file,
                line.date
            );
            assert!(
                date <= last,
                "pending line {} is dated {} ({at}), after the {last} the lot closes",
                line.file,
                line.date
            );
        }
    }
}

/// The ids of the `--lane` functions of run-all-tests.sh whose body names `--test <name>`.
fn lanes_naming_test(suite: &str, name: &str) -> Vec<String> {
    let outside: String = outside_self_test(suite)
        .iter()
        .map(|(_, line)| format!("{line}\n"))
        .collect();
    let mut lanes = Vec::new();
    for lane in ["CL", "D1", "D2", "D3", "NLI", "B3", "R", "E"] {
        let named = lane_body(&outside, lane).iter().any(|line| {
            !line.trim_start().starts_with('#')
                && line
                    .split("--test ")
                    .skip(1)
                    .any(|rest| rest.split_whitespace().next() == Some(name))
        });
        if named {
            lanes.push(lane.to_string());
        }
    }
    lanes
}

/// The body of `lane_<id>()` in run-all-tests.sh, as `step_body` does for merge-gate.sh.
fn lane_body(suite: &str, id: &str) -> Vec<String> {
    let header = format!("lane_{id}() {{");
    let mut lines = suite.lines();
    assert!(
        lines.any(|line| line.trim_end() == header),
        "run-all-tests.sh has no {header}"
    );
    lines
        .take_while(|line| *line != "}")
        .map(String::from)
        .collect()
}

/// The names in `RUN_ELSEWHERE=(…)` of lane_D2: the files its discovery leaves for their own sections.
fn run_elsewhere(suite: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut inside = false;
    for line in lane_body(suite, "D2") {
        if line.trim_start().starts_with("RUN_ELSEWHERE=(") {
            inside = true;
        }
        if inside {
            names.extend(
                line.replace("RUN_ELSEWHERE=(", " ")
                    .replace(')', " ")
                    .split_whitespace()
                    .map(String::from),
            );
            if line.contains(')') {
                break;
            }
        }
    }
    names
}

fn row_class(line: &str) -> String {
    line.split_whitespace()
        .nth(4)
        .unwrap_or_default()
        .to_string()
}

/// D-F34p: what keeps the file `file`, named `exclusive` in scripts/gate-latency.txt, from being coherent with the
/// judge. A src module is never exclusive (the lib runs in DBG, DC, CV and M). A test file is exclusive only if the
/// lanes that name it with --test are those of rows of the class exclusive (or boundary), the discovery of D2 leaves it
/// to them, and every test in it is #[ignore], so that the calls without --ignored of D1, DC and M only list it.
fn exclusive_problems(file: &str, gate: &str, suite: &str, source: &str) -> Vec<String> {
    if file.starts_with("src/") {
        return vec![format!(
            "{file}: a src module cannot be exclusive while lib runs in DBG, DC, CV and M"
        )];
    }
    let name = Path::new(file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("a file name");
    let rows = dag_rows();
    let rows_running = |lane: &str| -> Vec<(String, String)> {
        rows.iter()
            .filter(|(_, id, _, _)| {
                step_body(gate, id).is_some_and(|body| lanes_in(&body).iter().any(|l| l == lane))
            })
            .map(|(_, id, _, line)| (id.clone(), row_class(line)))
            .collect()
    };
    let mut problems = Vec::new();
    let mut lanes = lanes_naming_test(suite, name);
    if !run_elsewhere(suite).iter().any(|left| left == name) {
        lanes.push("D2".to_string());
    }
    for lane in lanes {
        for (row, class) in rows_running(&lane) {
            if !matches!(class.as_str(), "exclusive" | "boundary") {
                problems.push(format!(
                    "{file} is exclusive and runs in row {row} ({class}): lane {lane} of run-all-tests.sh runs it"
                ));
            }
        }
    }
    let tests = source
        .lines()
        .filter(|l| {
            l.trim_start().starts_with("#[test]") || l.trim_start().starts_with("#[tokio::test")
        })
        .count();
    let ignored = source
        .lines()
        .filter(|l| l.trim_start().starts_with("#[ignore"))
        .count();
    if tests != ignored {
        problems.push(format!(
            "{file} is exclusive and has {tests} tests and {ignored} #[ignore]: the calls without --ignored of D1, DC and M would run the rest beside other steps"
        ));
    }
    problems
}

#[test]
fn a_file_that_is_exclusive_in_the_latency_list_is_exclusive_in_the_judge() {
    let gate = read("scripts/merge-gate.sh");
    let suite = read("scripts/run-all-tests.sh");
    let exclusive: Vec<LatencyLine> = latency_lines()
        .into_iter()
        .filter(|line| line.class == "exclusive")
        .collect();
    assert!(
        !exclusive.is_empty(),
        "scripts/gate-latency.txt has no exclusive file: the NLI row asserts a clock and runs alone, so there is one"
    );
    for line in &exclusive {
        let source = if line.file.starts_with("src/") {
            String::new()
        } else {
            read(&format!("rust/{}", line.file))
        };
        let problems = exclusive_problems(&line.file, &gate, &suite, &source);
        assert!(
            problems.is_empty(),
            "scripts/gate-latency.txt:{}: {problems:#?}",
            line.number
        );
    }
}

/// The guard of the test above can fail: a file that the discovery of D2 runs in DBG, a src module, and a file with a
/// test that is not ignored are each refused, and the file the NLI row runs is not.
#[test]
fn the_exclusive_check_refuses_what_runs_beside_other_steps() {
    let gate = read("scripts/merge-gate.sh");
    let suite = read("scripts/run-all-tests.sh");
    let ok = exclusive_problems(
        "tests/nli_entailment.rs",
        &gate,
        &suite,
        &read("rust/tests/nli_entailment.rs"),
    );
    assert!(ok.is_empty(), "the file of the NLI row is refused: {ok:#?}");
    let in_dbg = exclusive_problems(
        "tests/v029_accounts_over_real_http.rs",
        &gate,
        &suite,
        &read("rust/tests/v029_accounts_over_real_http.rs"),
    );
    assert!(
        in_dbg
            .iter()
            .any(|p| p.contains("is exclusive and runs in row DBG (normal)")),
        "a file that D2 discovers in DBG is not refused as exclusive: {in_dbg:#?}"
    );
    let module = exclusive_problems("src/doctor.rs", &gate, &suite, "");
    assert!(
        module.iter().any(|p| p.contains("cannot be exclusive")),
        "a src module is not refused as exclusive: {module:#?}"
    );
    let half = "#[tokio::test]\n#[ignore]\nasync fn a() {}\n#[tokio::test]\nasync fn b() {}\n";
    let loose = exclusive_problems("tests/nli_entailment.rs", &gate, &suite, half);
    assert!(
        loose.iter().any(|p| p.contains("2 tests and 1 #[ignore]")),
        "a file with a test that is not ignored is not refused as exclusive: {loose:#?}"
    );
}

/// The value of `os.environ.get("GATE_DAG", "<default>")` in the planner.
fn gate_dag_default(planner: &str) -> String {
    let key = "os.environ.get(\"GATE_DAG\", \"";
    let line = planner
        .lines()
        .find(|line| line.contains(key))
        .expect("gate-dag.py reads GATE_DAG with a default");
    line.split(key)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("a default between quotes")
        .to_string()
}

/// D-F34n: the DAG is the default only with evidence, which is no pending line in the latency list.
fn activation_problem(default: &str, pending: usize) -> Option<String> {
    (default == "1" && pending > 0).then(|| {
        format!(
            "GATE_DAG defaults to 1 with {pending} pending line(s) in scripts/gate-latency.txt: the DAG is not the default before they have their evidence"
        )
    })
}

#[test]
fn the_dag_is_not_the_default_while_a_latency_line_is_pending() {
    let refused = activation_problem("1", 2);
    assert!(
        refused.is_some_and(|text| text.contains("GATE_DAG defaults to 1 with 2 pending line(s)")),
        "a default of 1 with two pending lines is not refused"
    );
    assert!(
        activation_problem("0", 2).is_none() && activation_problem("1", 0).is_none(),
        "the guard refuses a default of 0, or a list with nothing pending"
    );
    let pending = latency_lines()
        .iter()
        .filter(|line| line.evidence == "pendiente")
        .count();
    let default = gate_dag_default(&read("scripts/gate-dag.py"));
    if let Some(problem) = activation_problem(&default, pending) {
        panic!("{problem}");
    }
}

/// The cluster names a row declares in its resources: `cluster:C_b3` is `b3`.
fn clusters_of(line: &str) -> Vec<String> {
    resources_of(line)
        .iter()
        .filter_map(|r| r.strip_prefix("cluster:C_"))
        .map(String::from)
        .collect()
}

/// The resources of a row (its fourth field), without the `-` of a row that names none.
fn resources_of(line: &str) -> Vec<String> {
    line.split_whitespace()
        .nth(3)
        .unwrap_or_default()
        .split(',')
        .filter(|resource| *resource != "-")
        .map(String::from)
        .collect()
}

/// D-F34j: a row declares a cluster exactly when its step runs a lane in one. B3 builds the release binary in a lane
/// of its own and its row said nothing, which left the one-step-per-cluster rule with a hole.
#[test]
fn a_row_declares_a_cluster_exactly_when_its_step_runs_a_lane_in_one() {
    let gate = read("scripts/merge-gate.sh");
    for (_, id, _, line) in dag_rows() {
        let body =
            step_body(&gate, &id).unwrap_or_else(|| panic!("merge-gate.sh has no step_{id}()"));
        let lanes: Vec<String> = body
            .iter()
            .filter(|code| !code.trim_start().starts_with('#'))
            .filter_map(|code| code.split("run_in_lane ").nth(1))
            .filter_map(|rest| rest.split_whitespace().next())
            .map(String::from)
            .collect();
        let clusters = clusters_of(&line);
        for lane in &lanes {
            assert!(
                clusters.contains(lane),
                "step_{id} runs run_in_lane {lane} and its row declares no cluster:C_{lane}: {line}"
            );
        }
        for cluster in &clusters {
            assert!(
                lanes.contains(cluster),
                "row {id} declares cluster:C_{cluster} and step_{id} runs no run_in_lane {cluster}: {line}"
            );
        }
    }
}

/// D-F34f, D-F34g: the table is what the planner judges before it starts. A row names a target to build in, or reads
/// the target another row builds (`reader`, which only reads the binary on disk and waits for its producer). Two rows
/// in one target are ordered by their dependencies unless both read it; a row that is not a reader has a target of its
/// own; no cluster is shared; and every resource is of a kind the planner knows. REL and E2E read T_rel, B3's; NLI
/// reads T_dbg, DBG's.
#[test]
fn the_readers_read_the_target_of_their_producer_and_no_cluster_is_shared() {
    let mut rows = dag_rows();
    rows.sort_by_key(|row| row.0);
    let target_of = |line: &str| {
        resources_of(line)
            .iter()
            .find_map(|r| r.strip_prefix("target:").map(String::from))
    };
    let is_reader = |line: &str| resources_of(line).iter().any(|r| r == "reader");
    for (n, (_, a, _, line_a)) in rows.iter().enumerate() {
        let Some(target) = target_of(line_a) else {
            continue;
        };
        for (_, b, _, line_b) in &rows[n + 1..] {
            if target_of(line_b).as_deref() != Some(target.as_str()) {
                continue;
            }
            let both_read = is_reader(line_a) && is_reader(line_b);
            assert!(
                both_read || depends_on(&rows, b, a, 0) || depends_on(&rows, a, b, 0),
                "{a} and {b} share {target} and neither depends on the other"
            );
        }
    }
    let mut builders = std::collections::BTreeMap::new();
    for (_, id, _, line) in rows.iter().filter(|row| !is_reader(&row.3)) {
        if let Some(target) = target_of(line)
            && let Some(other) = builders.insert(target.clone(), id.clone())
        {
            panic!(
                "{other} and {id} both build in {target}: a row that is not a reader has a target of its own"
            );
        }
    }
    let mut readers: Vec<&str> = rows
        .iter()
        .filter(|(_, _, _, line)| is_reader(line))
        .map(|(_, id, _, _)| id.as_str())
        .collect();
    readers.sort_unstable();
    assert_eq!(
        readers,
        ["E2E", "NLI", "REL"],
        "the readers of the table are REL and E2E (T_rel) and NLI (T_dbg), and only them"
    );
    for (id, target) in [("REL", "T_rel"), ("E2E", "T_rel"), ("NLI", "T_dbg")] {
        let (_, _, _, line) = rows
            .iter()
            .find(|row| row.1 == id)
            .unwrap_or_else(|| panic!("gate-dag.txt has no row {id}"));
        assert!(
            is_reader(line) && target_of(line).as_deref() == Some(target),
            "row {id} is a reader of {target}: it reads the binary its producer built and builds nothing: {line}"
        );
        assert!(
            rows.iter().any(|(_, producer, _, other)| {
                !is_reader(other)
                    && target_of(other).as_deref() == Some(target)
                    && depends_on(&rows, id, producer, 0)
            }),
            "reader {id} of {target} has no row that builds {target} above it"
        );
    }
    let mut clusters = std::collections::BTreeMap::new();
    for (_, id, _, line) in &rows {
        for cluster in clusters_of(line) {
            if let Some(other) = clusters.insert(cluster.clone(), id.clone()) {
                panic!("rows {other} and {id} share cluster:C_{cluster}: one step per cluster");
            }
        }
        for resource in resources_of(line) {
            assert!(
                resource == "models"
                    || resource == "reader"
                    || resource.starts_with("target:")
                    || resource.starts_with("cluster:"),
                "row {id} names the resource {resource}, which is none of target:, cluster:, models or reader"
            );
        }
    }
}

/// D-F34h: the E2E lane does not compile. It provisions its database with the release binary B3 left, after checking
/// it against B3's sha256, and the provision that used to build a debug binary (gate_bin, in the target the readers of
/// REL hold) is not called from it.
#[test]
fn lane_e_provisions_with_the_release_binary_and_runs_no_cargo() {
    let suite = read("scripts/run-all-tests.sh");
    let body = lane_body(&suite, "E");
    let code: Vec<&String> = body
        .iter()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    for line in &code {
        assert!(
            !line.contains("cargo "),
            "lane_E runs cargo, and the E2E reads a binary that is already built: {line}"
        );
        assert!(
            !line.contains("gate_bin"),
            "lane_E calls gate_bin, which builds a debug binary: {line}"
        );
    }
    let provision = code
        .iter()
        .position(|line| line.contains("provision_gate_db"))
        .expect("lane_E provisions brain_gate when it is missing");
    assert!(
        code[provision].contains("provision_gate_db \"$REL_BIN\""),
        "lane_E provisions without the release binary: {}",
        code[provision]
    );
    let resolved = code
        .iter()
        .position(|line| line.trim_start().starts_with("REL_BIN="))
        .expect("lane_E resolves REL_BIN");
    assert!(
        resolved < provision,
        "lane_E provisions before it resolves REL_BIN"
    );
}

// --- CI-J15: the hygiene of the judge's scripts ------------------------------------------------------------------
// J15a: the audit judges the real table. J15b: the ORT pools of the table come from resources.rs. J15c: one lane grammar
// and one mark. J15d: gate-lock.sh is loaded once. J15e: copies that are derived. Each check is judged first on a fixture
// with the ways to be wrong (the lesson of CI-J13: a check that cannot fail is a paragraph).

/// The code of a gate script: the lines outside its self-test that are not comments, as (line number, line).
fn script_code(body: &str) -> Vec<(usize, &str)> {
    outside_self_test(body)
        .into_iter()
        .filter(|(_, line)| !line.trim_start().starts_with('#'))
        .collect()
}

/// CI-J15b (H4). The ORT pools the rows of the table name are the plan of production on the table's machine, and that plan
/// is in rust/src/resources.rs, not in a copy of it in this file.
#[test]
fn the_ort_pools_of_the_rows_are_the_plan_of_production() {
    let problems = pool_problems(
        &read("scripts/gate-dag.txt"),
        &read("rust/src/resources.rs"),
    );
    assert!(problems.is_empty(), "{problems:#?}");
}

/// CI-J15b. The sabotage of the spec: the ceiling of the reranker at 6 in resources.rs with the table at 8 is red, one
/// problem per row with models (DBG, NLI and REL), and the control with the real files is clean.
#[test]
fn the_check_of_the_ort_pools_can_fail() {
    let table = read("scripts/gate-dag.txt");
    let resources = read("rust/src/resources.rs");
    let control = pool_problems(&table, &resources);
    assert!(control.is_empty(), "the control is not clean: {control:#?}");
    let sabotaged = resources.replace(
        "const MAX_RERANK_INTRA_THREADS: usize = 8;",
        "const MAX_RERANK_INTRA_THREADS: usize = 6;",
    );
    assert_ne!(
        sabotaged, resources,
        "resources.rs no longer has `const MAX_RERANK_INTRA_THREADS: usize = 8;`: the sabotage changed nothing"
    );
    let problems = pool_problems(&table, &sabotaged);
    assert_eq!(
        problems.len(),
        3,
        "the rows with models (DBG, NLI, REL) name a reranker of 8 and the plan says 6: {problems:#?}"
    );
    assert!(
        problems
            .iter()
            .all(|p| p.contains("CUBA_RERANK_INTRA_THREADS") && p.contains("Some(\"6\")")),
        "the problem names the pool and what the plan says: {problems:#?}"
    );
}

/// The ids of the `step_<id>() {` functions of merge-gate.sh outside its self-test: the rows it can run.
fn step_functions(gate: &str) -> Vec<String> {
    script_code(gate)
        .into_iter()
        .filter_map(|(_, line)| {
            let id = line.strip_prefix("step_")?.strip_suffix("() {")?;
            Some(id.to_string())
        })
        .collect()
}

/// CI-J15e (H9). What is wrong with the dispatch of `--step` in merge-gate.sh: it runs the function `step_$id` when
/// `declare -F` says there is one, no `case` arm holds an id written by hand, and the functions and the rows of the table are
/// the same ids.
fn dispatch_problems(gate: &str, table_ids: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let code = script_code(gate);
    if !code
        .iter()
        .any(|(_, line)| line.contains("declare -F \"step_$id\""))
    {
        problems.push(
            "gate_step_main does not ask `declare -F \"step_$id\"` before it runs a step"
                .to_string(),
        );
    }
    for (number, line) in &code {
        let arm = line.trim();
        if let Some((id, rest)) = arm.split_once(") step_")
            && is_step_id(id)
            && rest.ends_with(" ;;")
        {
            problems.push(format!(
                "merge-gate.sh:{number}: `{arm}` is a case arm with an id written by hand; the dispatch is `declare -F \"step_$id\"`"
            ));
        }
    }
    let functions = step_functions(gate);
    for id in table_ids {
        if !functions.contains(id) {
            problems.push(format!(
                "row {id} of gate-dag.txt has no step_{id}() in merge-gate.sh"
            ));
        }
    }
    for function in &functions {
        if !table_ids.contains(function) {
            problems.push(format!(
                "step_{function}() of merge-gate.sh is no row of gate-dag.txt"
            ));
        }
    }
    problems
}

#[test]
fn the_dispatch_of_a_step_is_derived_from_its_function_and_the_table_holds_the_same_ids() {
    let ids: Vec<String> = dag_rows().into_iter().map(|(_, id, _, _)| id).collect();
    let problems = dispatch_problems(&read("scripts/merge-gate.sh"), &ids);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// CI-J15e. The check is judged first: an honest script is clean, a `case` with the ids by hand is named, a function that is
/// no row and a row that has no function are named.
#[test]
fn the_check_of_the_dispatch_can_fail() {
    let ids = ["P".to_string(), "DBG".to_string()];
    let honest = "step_P() {\n  :\n}\nstep_DBG() {\n  :\n}\n  declare -F \"step_$id\" >/dev/null || exit 2\n";
    let clean = dispatch_problems(honest, &ids);
    assert!(clean.is_empty(), "the control is not clean: {clean:#?}");
    let by_hand = "step_P() {\n}\nstep_DBG() {\n}\n  case \"$id\" in\n    P) step_P ;;\n    DBG) step_DBG ;;\n  esac\n";
    let problems = dispatch_problems(by_hand, &ids);
    assert!(
        problems.iter().any(|p| p.contains("declare -F")),
        "a dispatch with no declare -F is named: {problems:#?}"
    );
    assert_eq!(
        problems.iter().filter(|p| p.contains("case arm")).count(),
        2,
        "the two arms written by hand are named: {problems:#?}"
    );
    let ghost = format!("{honest}step_ZZ() {{\n}}\n");
    assert!(
        dispatch_problems(&ghost, &ids)
            .iter()
            .any(|p| p.contains("step_ZZ() of merge-gate.sh is no row")),
        "a function that is no row is named"
    );
    let missing = "step_P() {\n}\n  declare -F \"step_$id\" >/dev/null || exit 2\n";
    assert!(
        dispatch_problems(missing, &ids)
            .iter()
            .any(|p| p.contains("row DBG") && p.contains("no step_DBG()")),
        "a row with no function is named"
    );
}

/// CI-J15a (H1). What is wrong with gate-audit.sh outside its self-test: the key of its tree comes from the planner
/// (`gate-dag.py tree-key`), it hashes and resolves nothing itself, it takes the target root the planner takes
/// (CARGO_TARGET_DIR) and it launches no row of the table: merge-gate.sh runs them.
fn audit_problems(audit: &str) -> Vec<String> {
    let code = script_code(audit);
    let mut problems = Vec::new();
    if !code
        .iter()
        .any(|(_, line)| line.contains("gate-dag.py") && line.contains("tree-key"))
    {
        problems.push(
            "gate-audit.sh does not ask `gate-dag.py tree-key` for the key of its tree: the planner holds the one formula"
                .to_string(),
        );
    }
    for (number, line) in &code {
        for (needle, why) in [
            (
                "sha256sum",
                "hashes something itself: the key of a tree is the planner's",
            ),
            (
                "realpath",
                "resolves a path itself: the key of a tree is the planner's",
            ),
            (
                "GATE_TARGET_ROOT",
                "reads GATE_TARGET_ROOT, which the planner does not: its root is CARGO_TARGET_DIR",
            ),
            (
                "\"${words[@]}\"",
                "launches a row of the table by itself: merge-gate.sh runs them, with the ticket of the planner",
            ),
        ] {
            if line.contains(needle) {
                problems.push(format!("gate-audit.sh:{number}: {why}: `{}`", line.trim()));
            }
        }
    }
    problems
}

#[test]
fn the_audit_takes_its_tree_key_from_the_planner_and_launches_no_row() {
    let problems = audit_problems(&read("scripts/gate-audit.sh").replace("\r\n", "\n"));
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_check_of_the_audit_can_fail() {
    let honest = "key=\"$(python3 -I \"$GA_ROOT/scripts/gate-dag.py\" tree-key \"$GA_ROOT\")\"\n";
    let clean = audit_problems(honest);
    assert!(clean.is_empty(), "the control is not clean: {clean:#?}");
    let old = "tree_key=\"$(basename \"$GA_ROOT\")-$(realpath \"$GA_ROOT\" | sha256sum | cut -c1-12)\"\n\
               [[ -z \"${GATE_TARGET_ROOT:-}\" ]] || :\n\
               strace -- \"${words[@]}\"\n";
    let problems = audit_problems(old);
    for needle in [
        "tree-key",
        "sha256sum",
        "realpath",
        "GATE_TARGET_ROOT",
        "words[@]",
    ] {
        assert!(
            problems.iter().any(|p| p.contains(needle)),
            "the old audit is named for {needle}: {problems:#?}"
        );
    }
}

/// CI-J15c (H5). The lane grammar, the mark of the judge's servers and the encoding of a socket directory as a URL host are
/// each written once, in gate-lock.sh, and the scripts that need them use the names. `scripts` is (name, body).
fn judge_names_problems(scripts: &[(&str, &str)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut lane_re = 0;
    let mut mark = 0;
    let mut encoders = 0;
    for (name, body) in scripts {
        for (number, line) in script_code(body) {
            let trimmed = line.trim();
            if trimmed == "GATE_LANE_RE='[a-z0-9][a-z0-9-]*'" {
                lane_re += 1;
                continue;
            }
            if trimmed == "GATE_SERVER_MARK=memory-industry-gate" {
                mark += 1;
                continue;
            }
            for needle in [
                "[a-z0-9][a-z0-9-]*",
                "cluster_name=\"memory-industry-gate",
                "memory-industry-gate-$GATE_LANE",
                "'^memory-industry-gate",
            ] {
                if line.contains(needle) {
                    problems.push(format!(
                        "{name}:{number}: spells `{needle}` by hand; the lane grammar and the mark are GATE_LANE_RE and GATE_SERVER_MARK of gate-lock.sh: `{trimmed}`"
                    ));
                }
            }
            if line.contains("//\\//%2F}") {
                encoders += 1;
            }
        }
    }
    if lane_re != 1 {
        problems.push(format!(
            "GATE_LANE_RE='[a-z0-9][a-z0-9-]*' is assigned {lane_re} times, and it has to be once"
        ));
    }
    if mark != 1 {
        problems.push(format!(
            "GATE_SERVER_MARK=memory-industry-gate is assigned {mark} times, and it has to be once"
        ));
    }
    if encoders != 1 {
        problems.push(format!(
            "the encoding of a socket directory as a URL host (`${{x//\\//%2F}}`) is written {encoders} times, and it has to be once, as a function"
        ));
    }
    problems
}

#[test]
fn the_lane_grammar_the_mark_and_the_socket_encoding_are_written_once() {
    let names = [
        "scripts/gate-lock.sh",
        "scripts/gate-cluster.sh",
        "scripts/merge-gate.sh",
        "scripts/run-all-tests.sh",
    ];
    let bodies: Vec<String> = names
        .iter()
        .map(|name| read(name).replace("\r\n", "\n"))
        .collect();
    let scripts: Vec<(&str, &str)> = names
        .iter()
        .copied()
        .zip(bodies.iter().map(String::as_str))
        .collect();
    let problems = judge_names_problems(&scripts);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_check_of_the_judge_names_can_fail() {
    let lock = "GATE_SERVER_MARK=memory-industry-gate\nGATE_LANE_RE='[a-z0-9][a-z0-9-]*'\nencode() { printf '%s' \"${1//\\//%2F}\"; }\n";
    let clean = judge_names_problems(&[("gate-lock.sh", lock)]);
    assert!(clean.is_empty(), "the control is not clean: {clean:#?}");
    let cluster = "  [[ \"$lane\" =~ ^[a-z0-9][a-z0-9-]*$ ]] || exit 2\n  -c cluster_name=\"memory-industry-gate-$lane\"\n  url=\"${1//\\//%2F}\"\n";
    let problems = judge_names_problems(&[("gate-lock.sh", lock), ("gate-cluster.sh", cluster)]);
    assert!(
        problems
            .iter()
            .any(|p| p.contains("gate-cluster.sh:1") && p.contains("[a-z0-9][a-z0-9-]*")),
        "a lane grammar written by hand is named: {problems:#?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("gate-cluster.sh:2") && p.contains("cluster_name")),
        "a mark written by hand is named: {problems:#?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("socket directory") && p.contains("2 times")),
        "a second encoding of the socket host is named: {problems:#?}"
    );
}

/// CI-J15d (H6). What is wrong with the loading of gate-lock.sh: it has a guard that returns when it is already loaded,
/// before it assigns the lock; gate-cluster.sh does not assign ROOT or SELF in the shell that loads it; and merge-gate.sh reads
/// the GATE_LOCK it inherited before it sources the file that assigns its own.
fn load_guard_problems(lock: &str, cluster: &str, gate: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let lock_code = script_code(lock);
    let guard = lock_code
        .iter()
        .position(|(_, line)| line.contains("return 0"));
    let assigned_at = lock_code
        .iter()
        .position(|(_, line)| line.starts_with("GATE_LOCK_HELD="));
    match (guard, assigned_at) {
        (Some(guard), Some(assigned_at)) if guard < assigned_at => {}
        _ => problems.push(
            "gate-lock.sh has no load guard (a `return 0` for a library already loaded) before it assigns GATE_LOCK_HELD: loaded twice it empties the lock its caller holds"
                .to_string(),
        ),
    }
    for (number, line) in script_code(cluster) {
        if line.starts_with("ROOT=") || line.starts_with("SELF=") {
            problems.push(format!(
                "gate-cluster.sh:{number}: assigns `{line}` in the shell that loads it: a library does not take the names of its caller"
            ));
        }
    }
    let gate_code = script_code(gate);
    let saved = gate_code
        .iter()
        .position(|(_, line)| line.contains("GATE_LOCK"));
    let sourced = gate_code
        .iter()
        .position(|(_, line)| line.contains("source \"$ROOT/scripts/gate-lock.sh\""));
    match (saved, sourced) {
        (Some(saved), Some(sourced)) if saved < sourced => {}
        _ => problems.push(
            "merge-gate.sh has to read the GATE_LOCK it inherited before it sources gate-lock.sh, which assigns its own"
                .to_string(),
        ),
    }
    problems
}

#[test]
fn gate_lock_is_loaded_once_and_nobody_takes_the_names_of_their_caller() {
    let problems = load_guard_problems(
        &read("scripts/gate-lock.sh").replace("\r\n", "\n"),
        &read("scripts/gate-cluster.sh").replace("\r\n", "\n"),
        &read("scripts/merge-gate.sh").replace("\r\n", "\n"),
    );
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_check_of_the_load_guard_can_fail() {
    let lock =
        "if declare -F gate_lock_loaded >/dev/null; then\n  return 0\nfi\nGATE_LOCK_HELD=\"\"\n";
    let cluster = "source \"$(dirname \"${BASH_SOURCE[0]}\")/gate-lock.sh\"\n";
    let gate = "GATE_LOCK_INHERITED=\"${GATE_LOCK:-}\"\nsource \"$ROOT/scripts/gate-lock.sh\"\n";
    let clean = load_guard_problems(lock, cluster, gate);
    assert!(clean.is_empty(), "the control is not clean: {clean:#?}");
    let unguarded = load_guard_problems(
        "GATE_LOCK_HELD=\"\"\nf() {\n  return 0\n}\n",
        "ROOT=\"$(pwd)\"\nSELF=\"$ROOT/x\"\n",
        "source \"$ROOT/scripts/gate-lock.sh\"\nGATE_LOCK_INHERITED=\"${GATE_LOCK:-}\"\n",
    );
    assert!(
        unguarded.iter().any(|p| p.contains("no load guard")),
        "a library with no guard is named: {unguarded:#?}"
    );
    assert_eq!(
        unguarded
            .iter()
            .filter(|p| p.contains("gate-cluster.sh:"))
            .count(),
        2,
        "ROOT and SELF assigned by a library are named: {unguarded:#?}"
    );
    assert!(
        unguarded.iter().any(|p| p.contains("inherited")),
        "a GATE_LOCK read after the source is named: {unguarded:#?}"
    );
}
