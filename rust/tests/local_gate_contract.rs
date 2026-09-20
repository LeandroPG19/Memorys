//! Anti-drift for the **local** merge judge only (not GitHub Actions).
//! If someone removes require_present / deny / docs / codigo-muerto from the
//! scripts, this fails before a soft gate can look green again.

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
