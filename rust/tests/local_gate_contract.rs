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

/// One gate at a time, held by something that can refuse.
///
/// On 2026-09-22 the SIL went red with `database "brain_gate" does not exist`
/// in tests/integration.rs, minutes after the gate had created it. An orphaned
/// earlier run had finished and its EXIT trap dropped the database from under
/// the new one. The rule "one gate at a time" was a sentence; a second run is
/// now refused by a lock that names the first, and an exit drops only the
/// databases that still carry its own record. The `--self-test` builds each
/// case with real processes, and ends by launching the script itself as a
/// second gate against a live one.
#[test]
fn a_second_gate_is_refused_and_an_exit_drops_only_its_own_databases() {
    let out = std::process::Command::new(git_bash())
        .args(["scripts/run-all-tests.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

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

    let out = std::process::Command::new(git_bash())
        .args(["scripts/mutants-gate.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

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
    let out = std::process::Command::new(git_bash())
        .args(["scripts/release.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

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
/// the gate is killed in the middle, and after a second gate is refused.
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

    let out = std::process::Command::new(git_bash())
        .args(["scripts/merge-gate.sh", "--self-test"])
        .current_dir(repo_root())
        .output()
        .expect("a POSIX shell has to be reachable: every gate script here is a shell script");

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
    ("scripts/merge-gate.sh", r#"command -v pg_isready"#),
    (
        "scripts/quality-gate.sh",
        r#"echo "=== exclusions of the mutation step"#,
    ),
    ("scripts/mutants-gate.sh", r#"OUT_DIR="${TMPDIR"#),
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
    let expected = binary.to_string_lossy().into_owned();

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
    assert_eq!(
        found, expected,
        "with CARGO_TARGET_DIR={target} and a release binary there, the E2E was handed `{found}`. \
         The entry point fixes rust/target/release/memory-industry, a directory the gate on this \
         machine never builds into"
    );
}
