//! Contract for `scripts/gate_history.py`, the judge's history of times (CI-F3, F3.1), and for
//! the `--units` mode of `scripts/test-times.sh` that feeds it (D-F3s, D-F3r, D-F3t).
//!
//! The module is the only writer of the history. Its root is `CI_TIEMPOS_DIR` (or
//! `$HOME/.cache/ci-tiempos`) plus `memory-industry/`, with one folder per UTC day holding
//! `corridas.jsonl`, `pasos.jsonl` and `unidades.jsonl`, and `indice.jsonl` and `flaky.jsonl`
//! beside them. Writes go under `flock` on `.candado`. Every record has `repo`; the records of a
//! run carry its `run` id. Nothing here touches the real history or HOME: each test points
//! `CI_TIEMPOS_DIR` at a temporary folder, or at a plain file to make the writes fail.
//!
//! The API these tests drive, fixed here for pass 2 (standard library only, run with `python3 -I`):
//! - `run_start(modo, sha, huella, tabla) -> corrida`; `sha` and `huella` may be `None`, and then
//!   they are written as `null` and never take part in the flake mark.
//! - `step_start(corrida, idx, id) -> paso`.
//! - `step_end(corrida, paso, rc, *, inicio=None, fin=None, espera_s=None, motivo=None,
//!   parado=False, cpu_s=None, mem_peak=None, anon_max=None, oom_kill=None, psi=None)`. `inicio`
//!   and `fin` default to the module clock. Anything not measured is `None` and is written `null`.
//! - `units(corrida, paso, log_path)`: runs `bash scripts/test-times.sh --units LOG` and writes one
//!   `unidades.jsonl` line per binary with `clave`, `finished_in`, `tests` and `rc`. A binary
//!   with failures has `rc` 1. A binary red now that was green for the same `sha` and `huella`
//!   within 7 days gets a line in `flaky.jsonl` (diagnosis only: it never changes a verdict).
//! - `run_end(corrida, rc, *, T=None, LB_ref=None, LB_run=None, C=None, R=None)`: writes
//!   `corridas.jsonl` and the same line in `indice.jsonl`. `cpu_s` of the run is the sum of its
//!   steps' `cpu_s`, or `null` if any of them is `null`.
//! - CI-E4: `serie_final(intervalos)` over `(listo, inicio, fin)`; `step_end(..., sin_medida=None)` adds `nucleos` and
//!   `sin_medida` to the step; `run_end(..., mem_muestra=None, serie_final_s=None, serie_cola_s=None, sin_medida=None)`
//!   adds `nucleos`, those three and `sin_medida` to the run (the spec is the block of CI-E4 at the end of this file).
//! - `lower_bounds(records, table) -> {"LB": float, "cota": str}`, with
//!   `table = {"C": int, "R": bytes, "deps": {id: [ids]}}` and the step records of one run.
//!   `LB = max(camino critico, sum(cpu_s)/C, sum(anon_max*seg)/R, max seg)`; `cota` names the
//!   term that wins: `camino`, `cpu`, `ram` or `paso`.
//! - `history_root() -> <CI_TIEMPOS_DIR or ~/.cache/ci-tiempos>/memory-industry`.
//! - CLI: `python3 -I scripts/gate_history.py registrar` reads one JSON manifest on stdin and
//!   writes one run with its steps and the units of each log (the serial writer of F3.3).
//!
//! Scenario to test, with the sabotage each one catches:
//! - H1 (no `indice.jsonl` line) and the `repo`/`.candado` rules: `una_corrida_...`.
//! - H2 (a missing measure written as 0): `lo_que_no_se_puede_medir_es_null_y_no_cero`.
//! - H3 (a failed write changes the exit): `el_historial_nunca_cambia_un_veredicto_*`.
//! - H4 (a fixed default root): `la_raiz_por_omision_sale_de_home`. The J7b' self-test of the
//!   merge gate and gate-audit is F3.3's and is not in this file.
//! - H5 (a flake compared without `huella`): `la_marca_de_intermitente_*`.
//! - H6 and the `--units` sabotage (an awk that skips Doc-tests, or a Git Bash shape that breaks):
//!   `units_sale_igual_en_forma_linux_y_git_bash`, and the row U1 in `test-times.sh --self-test`.
//! - The bounds: `lower_bounds_da_la_cota_que_manda`.

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static SEQ: AtomicUsize = AtomicUsize::new(0);

const REPO: &str = "memory-industry";
const SHA: &str = "4f1c0a9b2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f70";
const HUELLA: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

/// Two binaries in cargo's Linux shape: the unit tests of the lib and a tests/ binary.
const LOG_DOS: &str = r#"
     Running unittests src/lib.rs (target/debug/deps/memory_industry-0123456789abcdef)

running 2 tests
test a ... ok
test b ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 21.51s

     Running tests/smoke.rs (target/debug/deps/smoke-0123456789abcdef)

running 1 test
test s ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
"#;

/// `LOG_DOS` plus a doc-test block, which an awk that ignores Doc-tests would drop.
const LOG_UNITS: &str = r#"
     Running unittests src/lib.rs (target/debug/deps/memory_industry-0123456789abcdef)

running 2 tests
test a ... ok
test b ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 21.51s

     Running tests/smoke.rs (target/debug/deps/smoke-0123456789abcdef)

running 1 test
test s ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s

   Doc-tests memory_industry

running 2 tests
test d1 ... ok
test d2 ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.27s
"#;

/// Three binaries, all red.
const LOG_RED: &str = r#"
     Running tests/smoke.rs (target/debug/deps/smoke-0123456789abcdef)

running 1 test
test s ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s

     Running tests/otra.rs (target/debug/deps/otra-0123456789abcdef)

running 1 test
test o ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s

     Running tests/vieja.rs (target/debug/deps/vieja-0123456789abcdef)

running 1 test
test v ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
"#;

const PRE: &str =
    "import os, sys, json\nsys.path.insert(0, sys.argv[1])\nimport gate_history as gh\n";

/// One run, three steps, the `tests` step with the log. argv: log, sha, huella.
const BODY_CORRIDA: &str = r#"
log, sha, huella = sys.argv[2:5]
corrida = gh.run_start(modo="serie", sha=sha, huella=huella, tabla="tabla-f31")
pasos = {}
for idx, nombre in [(10, "fmt"), (20, "tests"), (30, "e2e")]:
    paso = gh.step_start(corrida, idx, nombre)
    gh.step_end(corrida, paso, rc=0, espera_s=0.0, cpu_s=1.5, mem_peak=1048576, anon_max=524288)
    pasos[nombre] = paso
gh.units(corrida, pasos["tests"], log)
gh.run_end(corrida, rc=0)
"#;

/// One run whose only step was not measured at all. argv: sha, huella.
const BODY_NULOS: &str = r#"
sha, huella = sys.argv[2:4]
corrida = gh.run_start(modo="serie", sha=sha, huella=huella, tabla="tabla-f31")
paso = gh.step_start(corrida, 10, "fmt")
gh.step_end(corrida, paso, rc=0, espera_s=None, cpu_s=None, mem_peak=None, anon_max=None)
gh.run_end(corrida, rc=0)
"#;

/// Two steps, the second red, and a run red too. argv: sha, huella.
const BODY_VEREDICTO: &str = r#"
sha, huella = sys.argv[2:4]
corrida = gh.run_start(modo="serie", sha=sha, huella=huella, tabla="tabla-f31")
for idx, nombre, rc in [(10, "fmt", 0), (20, "tests", 1)]:
    paso = gh.step_start(corrida, idx, nombre)
    gh.step_end(corrida, paso, rc=rc, espera_s=0.0, cpu_s=0.5, mem_peak=1048576, anon_max=524288)
gh.run_end(corrida, rc=1)
"#;

/// Seeds the history two and nine days back, then a red run of `LOG_RED` on this tree, and a
/// second red run on a tree with no `huella`. argv: log, sha, huella.
const BODY_FLAKY: &str = r#"
from datetime import datetime, timedelta, timezone
log, sha, huella = sys.argv[2:5]
raiz = os.path.join(os.environ["CI_TIEMPOS_DIR"], "memory-industry")
def sembrar(dias, registro):
    dia = (datetime.now(timezone.utc) - timedelta(days=dias)).strftime("%Y%m%d")
    carpeta = os.path.join(raiz, dia)
    os.makedirs(carpeta, exist_ok=True)
    with open(os.path.join(carpeta, "unidades.jsonl"), "a", encoding="utf-8") as fh:
        fh.write(json.dumps(registro) + "\n")
sembrar(2, {"repo": "memory-industry", "id": "tests", "sha": sha, "huella": huella, "clave": "smoke", "rc": 0, "finished_in": 0.31, "tests": 1, "run": "sembrada-smoke"})
sembrar(2, {"repo": "memory-industry", "id": "tests", "sha": sha, "huella": "b" * 64, "clave": "otra", "rc": 0, "finished_in": 0.10, "tests": 1, "run": "sembrada-otra"})
sembrar(9, {"repo": "memory-industry", "id": "tests", "sha": sha, "huella": huella, "clave": "vieja", "rc": 0, "finished_in": 0.10, "tests": 1, "run": "sembrada-vieja"})
for huella_corrida in (huella, None):
    corrida = gh.run_start(modo="serie", sha=sha, huella=huella_corrida, tabla="tabla-f31")
    paso = gh.step_start(corrida, 20, "tests")
    gh.step_end(corrida, paso, rc=1, espera_s=0.0, cpu_s=0.5, mem_peak=1048576, anon_max=524288)
    gh.units(corrida, paso, log)
    gh.run_end(corrida, rc=1)
"#;

/// Three steps with the bounds' numbers: a -> b is a chain of 2.0 s, c is 1.5 s, 4 GiB of anon
/// each. With C=2 the chain wins; with C=1 the cpu sum (3.5 s) wins.
const BODY_COTA: &str = r#"
registros = [
    {"id": "a", "seg": 1.0, "cpu_s": 1.0, "anon_max": 4294967296},
    {"id": "b", "seg": 1.0, "cpu_s": 1.0, "anon_max": 4294967296},
    {"id": "c", "seg": 1.5, "cpu_s": 1.5, "anon_max": 4294967296},
]
deps = {"a": [], "b": ["a"], "c": []}
print(json.dumps([
    gh.lower_bounds(registros, {"C": 2, "R": 10737418240, "deps": deps}),
    gh.lower_bounds(registros, {"C": 1, "R": 10737418240, "deps": deps}),
]))
"#;

/// The same log as Git Bash prints it: backslashes, `.exe` and CRLF.
fn windows(log: &str) -> String {
    log.replace('/', "\\")
        .replace("0123456789abcdef)", "0123456789abcdef.exe)")
        .replace('\n', "\r\n")
}

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let base = scratch(name);
        let root = base.join("historial");
        let home = base.join("home");
        let tmp = base.join("tmp");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&tmp).expect("tmp");
        Fixture {
            base,
            root,
            home,
            tmp,
        }
    }

    fn log(&self, name: &str, body: &str) -> PathBuf {
        let path = self.base.join(name);
        write(&path, body);
        path
    }
}

/// `python3 -I -c <prelude + body> <scripts dir> <args…>`. `root` None removes CI_TIEMPOS_DIR.
fn py(fx: &Fixture, body: &str, args: &[&str], root: Option<&Path>) -> Output {
    py_en(fx, &scripts(), body, args, root)
}

/// `py` with the modules taken from `dir`. `-B` keeps bytecode out of `dir`: `-I` ignores the
/// PYTHON* variables, so PYTHONDONTWRITEBYTECODE alone writes `__pycache__` next to the scripts
/// (F3.3b-0, correction 7). The variable stays for the children that do not run under `-I`.
fn py_en(fx: &Fixture, dir: &Path, body: &str, args: &[&str], root: Option<&Path>) -> Output {
    let code = format!("{PRE}{body}");
    let mut cmd = Command::new("python3");
    cmd.arg("-I")
        .arg("-B")
        .arg("-c")
        .arg(code)
        .arg(dir)
        .args(args)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HOME", &fx.home)
        .env("TMPDIR", &fx.tmp);
    match root {
        Some(dir) => {
            cmd.env("CI_TIEMPOS_DIR", dir);
        }
        None => {
            cmd.env_remove("CI_TIEMPOS_DIR");
        }
    }
    cmd.output().expect("python3 -I -c gate_history")
}

/// `python3 -I scripts/gate_history.py registrar` with the manifest on stdin.
fn registrar(fx: &Fixture, root: Option<&Path>, manifest: &Value) -> Output {
    let mut cmd = Command::new("python3");
    cmd.arg("-I")
        .arg("-B")
        .arg(scripts().join("gate_history.py"))
        .arg("registrar")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HOME", &fx.home)
        .env("TMPDIR", &fx.tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match root {
        Some(dir) => {
            cmd.env("CI_TIEMPOS_DIR", dir);
        }
        None => {
            cmd.env_remove("CI_TIEMPOS_DIR");
        }
    }
    let mut child = cmd.spawn().expect("python3 -I gate_history.py registrar");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(manifest.to_string().as_bytes())
        .expect("the manifest on stdin");
    child.wait_with_output().expect("registrar output")
}

fn manifest(log: &Path) -> Value {
    json!({
        "modo": "serie",
        "sha": SHA,
        "huella": HUELLA,
        "tabla": "tabla-f31",
        "rc": 1,
        "pasos": [
            {"idx": 10, "id": "fmt", "inicio": 0.0, "fin": 0.4, "rc": 0, "cpu_s": 0.3,
             "mem_peak": 1048576, "anon_max": 524288, "espera_s": 0.0},
            {"idx": 20, "id": "tests", "inicio": 0.4, "fin": 1.0, "rc": 1, "cpu_s": 0.6,
             "mem_peak": 1048576, "anon_max": 524288, "espera_s": 0.0}
        ],
        "unidades": [{"id": "tests", "log": log.display().to_string()}]
    })
}

fn scripts() -> PathBuf {
    repo_root().join("scripts")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gate-history-{name}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write(path: &Path, body: &str) {
    fs::write(path, body).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn show(out: &Output) -> String {
    format!(
        "stdout:\n{}stderr:\n{}",
        text(&out.stdout),
        text(&out.stderr)
    )
}

fn assert_ok(out: &Output, label: &str) {
    assert!(
        out.status.success(),
        "{label}: exit {:?}, want 0\n{}",
        out.status.code(),
        show(out)
    );
}

/// The lines of stderr that start with `history:`; the contract wants one per process.
fn history_lines(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.starts_with("history:")).count()
}

fn files_named(dir: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                found.push(path);
            }
        }
    }
    found
}

/// Every line of every `name` file under `dir`, as JSON.
fn records(dir: &Path, name: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for file in files_named(dir, name) {
        let body = fs::read_to_string(&file).unwrap_or_default();
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let record: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{name} is not JSON lines: {e}: {line}"));
            out.push(record);
        }
    }
    out
}

/// Spec H1: one line per run, per step and per binary, and the same run line in the index.
#[test]
fn una_corrida_deja_una_linea_por_corrida_por_paso_y_por_binario() {
    let fx = Fixture::new("una-corrida");
    let log = fx.log("tests.log", LOG_DOS);
    let out = py(
        &fx,
        BODY_CORRIDA,
        &[path_str(&log), SHA, HUELLA],
        Some(fx.root.as_path()),
    );
    assert_ok(&out, "one run with three steps and a log of two binaries");

    let corridas = records(&fx.root, "corridas.jsonl");
    assert_eq!(
        corridas.len(),
        1,
        "corridas.jsonl: one line per run: {corridas:?}"
    );
    let corrida = &corridas[0];
    for key in ["repo", "sha", "huella", "modo", "rc", "seg", "cpu_s", "run"] {
        assert!(
            corrida.get(key).is_some(),
            "corridas.jsonl has no `{key}`: {corrida}"
        );
    }
    assert_eq!(corrida["repo"].as_str(), Some(REPO));
    assert_eq!(corrida["sha"].as_str(), Some(SHA));
    assert_eq!(corrida["huella"].as_str(), Some(HUELLA));
    assert_eq!(corrida["modo"].as_str(), Some("serie"));
    assert_eq!(corrida["rc"].as_i64(), Some(0));
    assert!(
        corrida["seg"].as_f64().is_some(),
        "seg is not a number: {corrida}"
    );
    assert_eq!(
        corrida["cpu_s"].as_f64(),
        Some(4.5),
        "cpu_s of the run is the sum of its three steps (3 x 1.5): {corrida}"
    );

    let pasos = records(&fx.root, "pasos.jsonl");
    assert_eq!(pasos.len(), 3, "pasos.jsonl: one line per step: {pasos:?}");
    for paso in &pasos {
        for key in [
            "repo", "run", "idx", "id", "seg", "espera_s", "cpu_s", "mem_peak", "anon_max", "rc",
        ] {
            assert!(
                paso.get(key).is_some(),
                "pasos.jsonl has no `{key}`: {paso}"
            );
        }
        assert_eq!(paso["repo"].as_str(), Some(REPO));
        assert_eq!(paso["rc"].as_i64(), Some(0));
    }
    let ids: BTreeSet<&str> = pasos.iter().filter_map(|p| p["id"].as_str()).collect();
    assert_eq!(ids, BTreeSet::from(["e2e", "fmt", "tests"]));

    let unidades = records(&fx.root, "unidades.jsonl");
    assert_eq!(
        unidades.len(),
        2,
        "unidades.jsonl: one line per binary of the log: {unidades:?}"
    );
    for unidad in &unidades {
        for key in ["repo", "run", "id", "clave", "finished_in", "tests", "rc"] {
            assert!(
                unidad.get(key).is_some(),
                "unidades.jsonl has no `{key}`: {unidad}"
            );
        }
        assert_eq!(unidad["repo"].as_str(), Some(REPO));
    }
    let claves: BTreeSet<&str> = unidades
        .iter()
        .filter_map(|u| u["clave"].as_str())
        .collect();
    assert_eq!(claves, BTreeSet::from(["lib:memory_industry", "smoke"]));
    let lib = unidades
        .iter()
        .find(|u| u["clave"].as_str() == Some("lib:memory_industry"))
        .expect("the lib binary is a unit");
    assert_eq!(lib["tests"].as_u64(), Some(2));
    assert!(
        (lib["finished_in"].as_f64().unwrap_or(f64::NAN) - 21.51).abs() < 1e-9,
        "finished_in of the lib, 21.51 s: {lib}"
    );

    let indice = records(&fx.root, "indice.jsonl");
    assert_eq!(
        indice.len(),
        1,
        "indice.jsonl repeats the run line: {indice:?}"
    );
    assert_eq!(
        indice[0], corridas[0],
        "indice.jsonl does not repeat the run line"
    );
    assert!(
        !files_named(&fx.root, ".candado").is_empty(),
        "no .candado under the history root: the writes are not under flock"
    );
}

/// Spec H2: what was not measured is `null`, never 0.
#[test]
fn lo_que_no_se_puede_medir_es_null_y_no_cero() {
    let fx = Fixture::new("nulos");
    let out = py(&fx, BODY_NULOS, &[SHA, HUELLA], Some(fx.root.as_path()));
    assert_ok(&out, "a run whose only step was not measured");

    let pasos = records(&fx.root, "pasos.jsonl");
    assert_eq!(pasos.len(), 1, "pasos.jsonl: {pasos:?}");
    for key in ["espera_s", "cpu_s", "mem_peak", "anon_max"] {
        assert!(
            pasos[0][key].is_null(),
            "{key} of an unmeasured step must be null, not {}: {}",
            pasos[0][key],
            pasos[0]
        );
    }
    let corridas = records(&fx.root, "corridas.jsonl");
    assert_eq!(corridas.len(), 1, "corridas.jsonl: {corridas:?}");
    assert!(
        corridas[0]["cpu_s"].is_null(),
        "the run's cpu_s sums a step that is null: it must be null, not a number: {}",
        corridas[0]
    );
}

/// Spec H3, API: a history that cannot be written does not change the process exit, and the
/// process says so once on stderr.
#[test]
fn el_historial_nunca_cambia_un_veredicto_api() {
    let fx = Fixture::new("veredicto-api");
    let no_es_carpeta = fx.base.join("no-es-carpeta");
    write(&no_es_carpeta, "");
    let out = py(
        &fx,
        BODY_VEREDICTO,
        &[SHA, HUELLA],
        Some(no_es_carpeta.as_path()),
    );
    assert_ok(
        &out,
        "a red run whose history cannot be written: the exit is the run's own",
    );
    let err = text(&out.stderr);
    assert_eq!(
        history_lines(&err),
        1,
        "stderr must carry one `history:` line per process, not one per write: {}",
        show(&out)
    );
    assert!(
        !err.contains("Traceback"),
        "the failed write escaped as an exception: {}",
        show(&out)
    );
}

/// Spec H3, CLI: the same rule for the serial writer's command line.
#[test]
fn el_historial_nunca_cambia_un_veredicto_cli() {
    let fx = Fixture::new("veredicto-cli");
    let no_es_carpeta = fx.base.join("no-es-carpeta");
    write(&no_es_carpeta, "");
    let log = fx.log("tests.log", LOG_DOS);
    let out = registrar(&fx, Some(no_es_carpeta.as_path()), &manifest(&log));
    assert_ok(&out, "registrar with a history that cannot be written");
    let err = text(&out.stderr);
    assert_eq!(
        history_lines(&err),
        1,
        "stderr must carry one `history:` line per process: {}",
        show(&out)
    );
    assert!(
        !err.contains("Traceback"),
        "the failed write escaped as an exception: {}",
        show(&out)
    );
}

/// Spec: the serial writer writes the same run, steps and units as the API does.
#[test]
fn la_cli_de_la_serie_escribe_la_corrida_y_sus_pasos() {
    let fx = Fixture::new("cli-serie");
    let log = fx.log("tests.log", LOG_DOS);
    let out = registrar(&fx, Some(fx.root.as_path()), &manifest(&log));
    assert_ok(&out, "registrar with a writable history");

    let corridas = records(&fx.root, "corridas.jsonl");
    assert_eq!(corridas.len(), 1, "corridas.jsonl: {corridas:?}");
    assert_eq!(corridas[0]["modo"].as_str(), Some("serie"));
    assert_eq!(corridas[0]["rc"].as_i64(), Some(1));
    assert_eq!(
        records(&fx.root, "pasos.jsonl").len(),
        2,
        "one line per step of the manifest"
    );
    assert_eq!(
        records(&fx.root, "unidades.jsonl").len(),
        2,
        "one line per binary of the log"
    );
}

/// Spec H4: with CI_TIEMPOS_DIR unset the history lives under $HOME/.cache/ci-tiempos.
#[test]
fn la_raiz_por_omision_sale_de_home() {
    let fx = Fixture::new("raiz-omision");
    let log = fx.log("tests.log", LOG_DOS);
    let out = py(&fx, BODY_CORRIDA, &[path_str(&log), SHA, HUELLA], None);
    assert_ok(&out, "a run with CI_TIEMPOS_DIR unset");
    let raiz = fx.home.join(".cache").join("ci-tiempos");
    assert!(
        raiz.join(REPO).is_dir(),
        "without CI_TIEMPOS_DIR the history goes to $HOME/.cache/ci-tiempos/{REPO}: {}",
        show(&out)
    );
    assert_eq!(
        records(&raiz, "corridas.jsonl").len(),
        1,
        "the run line is under $HOME"
    );
}

/// Spec H5: a binary red now that was green for the same sha and huella within 7 days is a
/// flake, and only that one. Another tree (huella) or a green run 9 days old is not.
#[test]
fn la_marca_de_intermitente_compara_sha_y_huella() {
    let fx = Fixture::new("intermitente");
    let log = fx.log("rojo.log", LOG_RED);
    let out = py(
        &fx,
        BODY_FLAKY,
        &[path_str(&log), SHA, HUELLA],
        Some(fx.root.as_path()),
    );
    assert_ok(
        &out,
        "a red run over seeded history: the mark does not change the exit",
    );
    assert_eq!(
        history_lines(&text(&out.stderr)),
        0,
        "no write failed: {}",
        show(&out)
    );

    let corridas = records(&fx.root, "corridas.jsonl");
    let de_este_arbol: Vec<&Value> = corridas
        .iter()
        .filter(|c| c["huella"].as_str() == Some(HUELLA))
        .collect();
    assert_eq!(de_este_arbol.len(), 1, "one run on this tree: {corridas:?}");
    assert_eq!(
        de_este_arbol[0]["rc"].as_i64(),
        Some(1),
        "the run keeps its own rc"
    );
    let run = de_este_arbol[0]["run"].clone();

    let flaky = records(&fx.root, "flaky.jsonl");
    assert_eq!(
        flaky.len(),
        1,
        "exactly one flake: smoke. otra (another huella) and vieja (9 days) are not: {flaky:?}"
    );
    assert_eq!(flaky[0]["clave"].as_str(), Some("smoke"));
    assert_eq!(
        flaky[0]["run"], run,
        "the flake names the run that contradicts the green one"
    );
}

/// Spec H6 and the `--units` sabotage: the same binaries, keys, times, counts and failures in the
/// Linux shape and the Git Bash shape, Doc-tests included.
#[test]
fn units_sale_igual_en_forma_linux_y_git_bash() {
    let fx = Fixture::new("units-formas");
    let linux = fx.log("linux.log", LOG_UNITS);
    let git_bash = fx.log("git-bash.log", &windows(LOG_UNITS));
    let esperado = "lib:memory_industry 2151 2 0\nsmoke 31 1 0\ndoc:memory_industry 27 2 0";
    for log in [&linux, &git_bash] {
        let out = Command::new("bash")
            .arg(scripts().join("test-times.sh"))
            .arg("--units")
            .arg(log)
            .output()
            .expect("bash test-times.sh --units");
        assert!(
            out.status.success(),
            "--units {}: exit {:?}\n{}",
            log.display(),
            out.status.code(),
            show(&out)
        );
        assert_eq!(
            text(&out.stdout).trim_end(),
            esperado,
            "--units of {} must print `<key> <centiseconds> <tests> <failed>` per binary",
            log.display()
        );
    }
}

/// Spec D-F3t: `LB` is the largest of the four bounds, and `cota` names the one that wins.
#[test]
fn lower_bounds_da_la_cota_que_manda() {
    let fx = Fixture::new("cotas");
    let out = py(&fx, BODY_COTA, &[], Some(fx.root.as_path()));
    assert_ok(&out, "lower_bounds over three steps");
    let v: Value = serde_json::from_str(text(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("lower_bounds did not print JSON: {e}: {}", show(&out)));
    assert!(
        (v[0]["LB"].as_f64().unwrap_or(f64::NAN) - 2.0).abs() < 1e-9,
        "with C=2 the chain a -> b (2.0 s) wins over cpu 3.5/2, ram 1.4 and max seg 1.5: {}",
        v[0]
    );
    assert_eq!(v[0]["cota"].as_str(), Some("camino"));
    assert!(
        (v[1]["LB"].as_f64().unwrap_or(f64::NAN) - 3.5).abs() < 1e-9,
        "with C=1 the cpu sum 3.5 wins over the chain 2.0: {}",
        v[1]
    );
    assert_eq!(v[1]["cota"].as_str(), Some("cpu"));
}

// --- F3.1 · correcciones de la revisión (7-nov, orquestador): H7 a H11 y los P2 con comportamiento ---

const TWO_GREEN: &str =
    "10 a - - normal 1 5 0.2 /bin/sleep 0.2\n20 b a - normal 1 5 0.2 /bin/sleep 0.2\n";
const TWO_FAST: &str = "10 a - - normal 1 5 0.05 /bin/true\n20 b a - normal 1 5 0.05 /bin/true\n";
const TWO_RED_LAST: &str =
    "10 a - - normal 1 5 0.2 /bin/sleep 0.2\n20 b a - normal 1 5 0.2 /bin/false\n";

/// One binary of the lib, red.
const LOG_LIB_RED: &str = r#"
     Running unittests src/lib.rs (target/debug/deps/memory_industry-0123456789abcdef)

running 1 test
test a ... FAILED

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 21.51s
"#;

/// Holds the history lock until killed: prints `locked` once it has it.
const CHILD_HOLDS_LOCK: &str = r#"
import fcntl, os, sys, time
mi = sys.argv[1]
os.makedirs(mi, exist_ok=True)
candado = open(os.path.join(mi, ".candado"), "a")
fcntl.flock(candado, fcntl.LOCK_EX)
print("locked", flush=True)
time.sleep(20)
"#;

/// H11: a value the module cannot write is a warning and exit 0, through the API.
const BODY_API_MALA: &str = r#"
sha, huella = sys.argv[2:4]
corrida = gh.run_start(modo="serie", sha=sha, huella=huella, tabla="tabla-f31")
paso = gh.step_start(corrida, 10, "fmt")
gh.step_end(corrida, paso, 0, inicio="tarde")
gh.run_end(corrida, 0)
"#;

/// P2: one unexpected line of the units output is warned about and skipped. argv: log, sha, huella.
const BODY_UNIDAD_RARA: &str = r#"
log, sha, huella = sys.argv[2:5]
corrida = gh.run_start(modo="serie", sha=sha, huella=huella, tabla="tabla-f31")
paso = gh.step_start(corrida, 10, "tests")
gh.step_end(corrida, paso, 0)
gh._test_times_units = lambda ruta: "lib:memory_industry notanumber 2 0\nsmoke 31 1 0\n"
gh.units(corrida, paso, log)
gh.run_end(corrida, 0)
"#;

/// P2: a RAM capacity of 0 leaves the RAM term out and the bound is still the chain. argv: none.
const BODY_R_CERO: &str = r#"
registros = [
    {"id": "a", "seg": 1.0, "cpu_s": 1.0, "anon_max": 4294967296},
    {"id": "b", "seg": 1.0, "cpu_s": 1.0, "anon_max": 4294967296},
    {"id": "c", "seg": 1.5, "cpu_s": 1.5, "anon_max": 4294967296},
]
deps = {"a": [], "b": ["a"], "c": []}
print(json.dumps(gh.lower_bounds(registros, {"C": 2, "R": 0, "deps": deps})))
"#;

/// P2: a dependency cycle is refused by name, not by a RecursionError. argv: none.
const BODY_CICLO: &str = r#"
registros = [
    {"id": "a", "seg": 1.0, "cpu_s": 1.0, "anon_max": 4294967296},
    {"id": "b", "seg": 1.0, "cpu_s": 1.0, "anon_max": 4294967296},
]
print(gh.lower_bounds(registros, {"C": 1, "R": 10737418240, "deps": {"a": ["b"], "b": ["a"]}}))
"#;

/// A scope the planner cannot delegate, so it runs in series; the cgroup files it reads are fakes.
fn fake_cgroup(base: &Path) -> (PathBuf, PathBuf) {
    let cgroups = base.join("cgroup");
    let scope = cgroups.join("scope");
    fs::create_dir_all(&scope).expect("scope");
    write(&scope.join("cpu.max"), "400000 100000");
    write(&scope.join("memory.max"), "max");
    write(&scope.join("memory.current"), "0");
    write(&scope.join("memory.stat"), "anon 0\ninactive_file 0\n");
    let proc_self = base.join("proc-self-cgroup");
    write(&proc_self, "0::/scope\n");
    (cgroups, proc_self)
}

fn table_file(base: &Path, name: &str, body: &str) -> PathBuf {
    fs::create_dir_all(base).expect("table dir");
    let path = base.join(name);
    write(&path, body);
    path
}

/// The planner's own run: its exit, stdout and stderr.
struct Plan {
    code: Option<i32>,
    out: String,
    err: String,
}

fn plan(base: &Path, table: &Path, root: &Path, script_dir: &Path, extra: &[(&str, &str)]) -> Plan {
    let (cgroups, proc_self) = fake_cgroup(base);
    let home = base.join("home");
    let tmp = base.join("tmp");
    fs::create_dir_all(&home).expect("home");
    fs::create_dir_all(&tmp).expect("tmp");
    let mut cmd = Command::new("python3");
    cmd.arg("-I")
        .arg(script_dir.join("gate-dag.py"))
        .arg("run")
        .arg("--table")
        .arg(table)
        .env("GATE_DAG", "0")
        .env("CI_TIEMPOS_DIR", root)
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("GATE_CGROUP_ROOT", &cgroups)
        .env("GATE_PROC_SELF_CGROUP", &proc_self);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("python3 -I gate-dag.py run");
    Plan {
        code: out.status.code(),
        out: text(&out.stdout),
        err: text(&out.stderr),
    }
}

/// `registrar` with a manifest given as text on stdin.
fn registrar_stdin(root: &Path, home: &Path, stdin: &str, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new("python3");
    cmd.arg("-I")
        .arg("-B")
        .arg(scripts().join("gate_history.py"))
        .arg("registrar")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("CI_TIEMPOS_DIR", root)
        .env("HOME", home)
        .env("TMPDIR", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().expect("python3 -I gate_history.py registrar");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(stdin.as_bytes())
        .expect("the manifest on stdin");
    child.wait_with_output().expect("registrar output")
}

fn registrar_with(root: &Path, home: &Path, manifest: &Value, env: &[(&str, &str)]) -> Output {
    registrar_stdin(root, home, &manifest.to_string(), env)
}

/// A manifest with one step of id `paso_id`, with rc `rc`, and the units of `log`.
fn manifest_paso(paso_id: &str, rc: i64, log: &Path) -> Value {
    json!({
        "modo": "serie",
        "sha": SHA,
        "huella": HUELLA,
        "tabla": "tabla-f31",
        "rc": rc,
        "pasos": [{"idx": 10, "id": paso_id, "inicio": 0.0, "fin": 0.4, "rc": rc, "cpu_s": 0.3,
                   "mem_peak": 1048576, "anon_max": 524288, "espera_s": 0.0}],
        "unidades": [{"id": paso_id, "log": log.display().to_string()}]
    })
}

/// Starts a process that holds the history lock of `root` until it is killed.
fn hold_lock(root: &Path) -> std::process::Child {
    let mi = root.join(REPO);
    let mut child = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(CHILD_HOLDS_LOCK)
        .arg(&mi)
        .stdout(Stdio::piped())
        .spawn()
        .expect("the process that holds the lock");
    let stdout = child.stdout.take().expect("its stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("the holder reports the lock");
    assert_eq!(line.trim(), "locked", "the holder did not take the lock");
    child
}

/// Replaces `seg` of every line of every `pasos.jsonl` under `root`.
fn set_seg(root: &Path, valor: Value) {
    for file in files_named(root, "pasos.jsonl") {
        let body = fs::read_to_string(&file).expect("pasos.jsonl");
        let mut out = String::new();
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            let mut record: Value = serde_json::from_str(line).expect("a JSON line");
            record["seg"] = valor.clone();
            out.push_str(&record.to_string());
            out.push('\n');
        }
        write(&file, &out);
    }
}

/// H7 (P0): one byte that is not UTF-8 in an old pasos.jsonl does not stop the planner. Its
/// verdict is the one it gives without that file.
#[test]
fn h7_un_byte_no_utf8_en_pasos_no_para_el_planificador() {
    let fx = Fixture::new("h7-planificador");
    let table = table_file(&fx.base, "tabla.txt", TWO_RED_LAST);
    let limpio = fx.base.join("limpio");
    let sin_historial = plan(&limpio, &table, &limpio.join("historial"), &scripts(), &[]);
    assert_eq!(
        sin_historial.code,
        Some(1),
        "the red step 20 gives exit 1: {}",
        sin_historial.err
    );

    let corrupto = fx.base.join("corrupto");
    let dia = corrupto.join("historial").join(REPO).join("19700101");
    fs::create_dir_all(&dia).expect("day folder");
    fs::write(dia.join("pasos.jsonl"), b"{\"id\": \"a\"}\n\xff\n").expect("corrupt pasos.jsonl");
    let con_byte_roto = plan(
        &corrupto,
        &table,
        &corrupto.join("historial"),
        &scripts(),
        &[],
    );
    assert_eq!(
        con_byte_roto.code, sin_historial.code,
        "a byte that is not UTF-8 changed the exit: {}",
        con_byte_roto.err
    );
    assert!(
        !con_byte_roto.err.contains("Traceback"),
        "the planner crashed reading the history: {}",
        con_byte_roto.err
    );
    assert!(
        con_byte_roto.out.contains("steps=2"),
        "the planner did not reach its summary: {}",
        con_byte_roto.out
    );
}

/// H7 (P0), registrar: a broken byte in unidades.jsonl does not stop the next run, which reads
/// that file to look for flakes.
#[test]
fn h7_registrar_con_una_linea_rota_en_unidades_sale_0() {
    let fx = Fixture::new("h7-registrar");
    let root = fx.base.join("historial");
    let log = fx.log("rojo.log", LOG_RED);
    let primera = registrar_with(&root, &fx.home, &manifest(&log), &[]);
    assert_ok(&primera, "the first run writes the history");
    for file in files_named(&root, "unidades.jsonl") {
        let mut fh = fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("unidades.jsonl to append to");
        fh.write_all(b"\xc3\n").expect("the broken line");
    }
    let segunda = registrar_with(&root, &fx.home, &manifest(&log), &[]);
    assert_ok(
        &segunda,
        "a broken byte in unidades.jsonl must not stop the run",
    );
    assert_eq!(
        history_lines(&text(&segunda.stderr)),
        0,
        "a broken line is skipped, not reported as a failed write: {}",
        show(&segunda)
    );
}

/// H8 (P1), registrar: a lock held by another process is a warning after its timeout, not a wait.
#[test]
fn h8_un_candado_tomado_no_cuelga_el_registro() {
    let fx = Fixture::new("h8-registrar");
    let root = fx.base.join("historial");
    let mut holder = hold_lock(&root);
    let log = fx.log("lib.log", LOG_DOS);
    let inicio = Instant::now();
    let out = registrar_with(
        &root,
        &fx.home,
        &manifest(&log),
        &[("GATE_HISTORY_LOCK_TIMEOUT_S", "0.2")],
    );
    let tardo = inicio.elapsed();
    let _ = holder.kill();
    let _ = holder.wait();
    assert_ok(
        &out,
        "a lock held past its timeout is a warning, not a verdict",
    );
    assert!(
        tardo < Duration::from_secs(3),
        "registrar waited {tardo:?} for a lock held past its timeout of 0.2 s: {}",
        show(&out)
    );
    assert_eq!(
        history_lines(&text(&out.stderr)),
        1,
        "one history: line: {}",
        show(&out)
    );
}

/// H8 (P1), planner: the same lock does not hang `gate-dag.py run`, which writes from drain() too.
/// With a timeout of 1 s it waits once and then skips the later writes: under 2,5 s.
#[test]
fn h8_un_candado_tomado_no_cuelga_el_planificador() {
    let fx = Fixture::new("h8-planificador");
    let root = fx.base.join("historial");
    let mut holder = hold_lock(&root);
    let table = table_file(&fx.base, "tabla.txt", TWO_FAST);
    let inicio = Instant::now();
    let p = plan(
        &fx.base,
        &table,
        &root,
        &scripts(),
        &[("GATE_HISTORY_LOCK_TIMEOUT_S", "1")],
    );
    let tardo = inicio.elapsed();
    let _ = holder.kill();
    let _ = holder.wait();
    assert_eq!(p.code, Some(0), "two green steps: {}", p.err);
    assert!(
        tardo < Duration::from_millis(2500),
        "the planner waited {tardo:?} with the lock held and a timeout of 1 s: the timeout is spent once, \
         so about 1.05 s; one timeout per write would be about 4.1 s (4 writes)"
    );
    assert_eq!(history_lines(&p.err), 1, "one history: line: {}", p.err);
}

/// H9 (P1): registrar invents no times. A step or a run with no times is written with null.
#[test]
fn h9_registrar_no_inventa_tiempos() {
    let fx = Fixture::new("h9-nulos");
    let root = fx.base.join("historial");
    let manifest_sin_tiempos = json!({
        "modo": "serie",
        "sha": SHA,
        "huella": HUELLA,
        "tabla": "tabla-f31",
        "rc": 0,
        "pasos": [{"idx": 10, "id": "fmt", "rc": 0}],
        "unidades": []
    });
    let out = registrar_with(&root, &fx.home, &manifest_sin_tiempos, &[]);
    assert_ok(&out, "a manifest with no times");

    let pasos = records(&root, "pasos.jsonl");
    assert_eq!(pasos.len(), 1, "pasos.jsonl: {pasos:?}");
    for key in ["inicio", "fin", "seg"] {
        assert!(
            pasos[0][key].is_null(),
            "{key} of a step with no times must be null, not {}: {}",
            pasos[0][key],
            pasos[0]
        );
    }
    let corridas = records(&root, "corridas.jsonl");
    assert_eq!(corridas.len(), 1, "corridas.jsonl: {corridas:?}");
    for key in ["T", "seg"] {
        assert!(
            corridas[0][key].is_null(),
            "{key} of a run with no T must be null, not {}: {}",
            corridas[0][key],
            corridas[0]
        );
    }
}

/// H9 (P1), planner: a seg that is null or 0 is not a sample of the history. The control shows
/// that a real seg is one, so the mark is not an accident of the setup.
#[test]
fn h9_un_seg_nulo_o_cero_no_es_muestra_de_historial() {
    let fx = Fixture::new("h9-muestra");
    let root = fx.base.join("historial");
    let table = table_file(&fx.base, "tabla.txt", TWO_GREEN);
    let siembra = plan(&fx.base, &table, &root, &scripts(), &[]);
    assert_eq!(siembra.code, Some(0), "the seeding run: {}", siembra.err);
    let control = plan(&fx.base, &table, &root, &scripts(), &[]);
    assert_eq!(control.code, Some(0), "the control run: {}", control.err);
    assert!(
        !control.out.contains("(no history)"),
        "the control has real seg and must learn from them: {}",
        control.out
    );
    for valor in [Value::Null, json!(0.0)] {
        set_seg(&root, valor.clone());
        let p = plan(&fx.base, &table, &root, &scripts(), &[]);
        assert_eq!(p.code, Some(0), "a run over a seg of {valor}: {}", p.err);
        assert!(
            p.out.contains("LB_ref=n/a (no history)"),
            "a seg of {valor} was taken as a sample of the history: {}",
            p.out
        );
    }
}

/// H10 (P1): the flake mark compares the step id too. `lib` and `lib-ignored` share the binary's
/// key and are not each other's flake; a red `lib` after a green `lib` still is.
#[test]
fn h10_la_marca_compara_el_id_del_paso() {
    let fx = Fixture::new("h10-id");
    let root = fx.base.join("historial");
    let verde = fx.log("lib-verde.log", LOG_DOS);
    let rojo = fx.log("lib-rojo.log", LOG_LIB_RED);

    let uno = registrar_with(&root, &fx.home, &manifest_paso("lib", 0, &verde), &[]);
    assert_ok(&uno, "the green lib");
    let dos = registrar_with(
        &root,
        &fx.home,
        &manifest_paso("lib-ignored", 1, &rojo),
        &[],
    );
    assert_ok(&dos, "the red lib-ignored");
    assert!(
        records(&root, "flaky.jsonl").is_empty(),
        "lib-ignored is not a flake of lib: they are two steps, not two runs of one: {:?}",
        records(&root, "flaky.jsonl")
    );

    let tres = registrar_with(&root, &fx.home, &manifest_paso("lib", 1, &rojo), &[]);
    assert_ok(&tres, "the red lib");
    let flaky = records(&root, "flaky.jsonl");
    assert_eq!(
        flaky.len(),
        1,
        "the control: a red lib after a green lib is a flake: {flaky:?}"
    );
    assert_eq!(flaky[0]["id"].as_str(), Some("lib"));
}

/// H11 (P1): a value the history cannot take is a warning, and the exit is 0.
#[test]
fn h11_registrar_con_un_valor_malo_sale_0_con_aviso() {
    let fx = Fixture::new("h11-registrar");
    let root = fx.base.join("historial");
    let log = fx.log("lib.log", LOG_DOS);
    let mut manifiesto = manifest_paso("fmt", 0, &log);
    manifiesto["pasos"][0]["inicio"] = json!("tarde");
    let out = registrar_with(&root, &fx.home, &manifiesto, &[]);
    assert_ok(&out, "a manifest with a value that is not a number");
    assert_eq!(
        history_lines(&text(&out.stderr)),
        1,
        "one history: line: {}",
        show(&out)
    );
}

/// H11 (P1), API: the same rule for the library calls.
#[test]
fn h11_la_api_no_sale_distinto_de_0_por_el_historial() {
    let fx = Fixture::new("h11-api");
    let root = fx.base.join("historial");
    let out = py(&fx, BODY_API_MALA, &[SHA, HUELLA], Some(root.as_path()));
    assert_ok(&out, "the API with a value that is not a number");
    assert_eq!(
        history_lines(&text(&out.stderr)),
        1,
        "one history: line: {}",
        show(&out)
    );
}

/// H11: only an illegible manifest is exit 2. Green before the fix too: it keeps that rule.
#[test]
fn h11_un_manifiesto_ilegible_es_2() {
    let fx = Fixture::new("h11-ilegible");
    let root = fx.base.join("historial");
    let out = registrar_stdin(&root, &fx.home, "{no es json", &[]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an illegible manifest is exit 2: {}",
        show(&out)
    );
    assert!(
        text(&out.stderr).contains("manifest"),
        "the refusal names the manifest: {}",
        show(&out)
    );
}

/// P2: the planner never writes bytecode next to its scripts (J7).
#[test]
fn el_planificador_no_escribe_pycache() {
    let fx = Fixture::new("p2-pycache");
    let copia = fx.base.join("scripts");
    fs::create_dir_all(&copia).expect("the copy of the scripts");
    for script in ["gate-dag.py", "gate_history.py", "test-times.sh"] {
        fs::copy(scripts().join(script), copia.join(script)).expect("copy a script");
    }
    let table = table_file(&fx.base, "tabla.txt", TWO_GREEN);
    let p = plan(&fx.base, &table, &fx.base.join("historial"), &copia, &[]);
    assert_eq!(p.code, Some(0), "two green steps: {}", p.err);
    assert!(
        !copia.join("__pycache__").exists(),
        "the planner wrote bytecode next to its scripts in {}",
        copia.display()
    );
}

/// P2: a RAM capacity of 0 leaves the RAM term out; the bound is the chain, not a ZeroDivisionError.
#[test]
fn lower_bounds_con_ram_cero_omite_el_termino() {
    let fx = Fixture::new("p2-ram-cero");
    let out = py(&fx, BODY_R_CERO, &[], Some(fx.root.as_path()));
    assert_ok(&out, "lower_bounds with R=0");
    let v: Value = serde_json::from_str(text(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("lower_bounds did not print JSON: {e}: {}", show(&out)));
    assert_eq!(
        v["cota"].as_str(),
        Some("camino"),
        "with R=0 the chain wins: {}",
        v
    );
    assert!(
        (v["LB"].as_f64().unwrap_or(f64::NAN) - 2.0).abs() < 1e-9,
        "with R=0 the bound is the chain of 2.0 s: {}",
        v
    );
}

/// P2: a dependency cycle is refused by name. The planner turns that refusal into exit 2; a
/// RecursionError is not a refusal.
#[test]
fn lower_bounds_nombra_un_ciclo() {
    let fx = Fixture::new("p2-ciclo");
    let out = py(&fx, BODY_CICLO, &[], Some(fx.root.as_path()));
    let err = text(&out.stderr);
    assert!(
        !out.status.success(),
        "a cycle must not give a bound: {}",
        show(&out)
    );
    assert!(
        err.contains("ciclo"),
        "the refusal names the cycle: {}",
        show(&out)
    );
    assert!(
        !err.contains("RecursionError"),
        "the cycle ended in a RecursionError: {}",
        show(&out)
    );
}

/// P2: one unexpected line of the units output is warned about and skipped; the lines around it
/// are still written.
#[test]
fn units_salta_una_linea_inesperada_y_sigue() {
    let fx = Fixture::new("p2-units-linea");
    let root = fx.base.join("historial");
    let log = fx.log("tests.log", LOG_DOS);
    let out = py(
        &fx,
        BODY_UNIDAD_RARA,
        &[path_str(&log), SHA, HUELLA],
        Some(root.as_path()),
    );
    assert_ok(&out, "units with one unexpected line");
    assert_eq!(
        history_lines(&text(&out.stderr)),
        1,
        "one history: line: {}",
        show(&out)
    );
    let unidades = records(&root, "unidades.jsonl");
    assert_eq!(
        unidades.len(),
        1,
        "only the good line is written: {unidades:?}"
    );
    assert_eq!(unidades[0]["clave"].as_str(), Some("smoke"));
}

/// Cargo's shape of a binary that a signal killed: the finished library binary, then a test binary
/// with no `test result` line, and the error cargo prints for it.
const LOG_SENAL: &str = r#"
     Running unittests src/lib.rs (target/debug/deps/memory_industry-0123456789abcdef)

running 2 tests
test a ... ok
test b ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 21.51s

     Running tests/smoke.rs (target/debug/deps/smoke-0123456789abcdef)

running 1 test
test s ... ok

error: process didn't exit successfully: `target/debug/deps/smoke-0123456789abcdef` (signal: 11, SIGSEGV: invalid memory reference)
"#;

/// One run with one step measured and every system figure given to `run_end` (F3.3b-0, correction 3).
const BODY_CIERRE: &str = r#"
corrida = gh.run_start(modo="dag", sha="s", huella="h", tabla="tabla-f33b0")
paso = gh.step_start(corrida, 10, "fmt")
gh.step_end(corrida, paso, rc=0, espera_s=0.0, cpu_s=1.0, mem_peak=1048576, anon_max=524288, oom_kill=0, psi={"cpu": 0.5, "memory": None, "io": 0.1})
gh.run_end(corrida, rc=0, T=10.0, LB_ref=4.0, LB_run=8.0, C=4, R=10737418240, mem_peak=2097152, psi={"cpu": 1.5, "memory": None, "io": 0.2}, load=3.5, hilos=24, oom_kill=0)
"#;

/// A run that measured nothing but its own clock. Every system figure is null.
const BODY_CIERRE_NULO: &str = r#"
corrida = gh.run_start(modo="serie", sha="s", huella="h", tabla="tabla-f33b0")
gh.run_end(corrida, rc=0)
"#;

/// Prints the huella of every path in argv as a JSON list, null where git cannot say.
const BODY_HUELLA: &str = r#"
print(json.dumps([gh.huella(ruta) for ruta in sys.argv[2:]]))
"#;

/// `git` in `dir` with the identity its commits need. Only the test's own repositories.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=contrato",
            "-c",
            "user.email=contrato@example.invalid",
        ])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        show(&out)
    );
}

/// A repository with one commit. `sin_rastrear` writes an untracked src/x.rs; `tocado` changes the
/// committed file without committing it.
fn repo_con(dir: &Path, sin_rastrear: Option<&str>, tocado: Option<&str>) {
    fs::create_dir_all(dir).expect("the repository folder");
    git(dir, &["init", "-q"]);
    write(&dir.join("base.txt"), "uno\n");
    git(dir, &["add", "base.txt"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    if let Some(texto) = tocado {
        write(&dir.join("base.txt"), texto);
    }
    if let Some(texto) = sin_rastrear {
        fs::create_dir_all(dir.join("src")).expect("the src folder");
        write(&dir.join("src/x.rs"), texto);
    }
}

/// Spec F3.3b-0, correction 1 (F3.1, «para F3.3»): a unit that names a step the manifest does not
/// have is one `history:` line that names it. The run, its steps and its index line are written as usual.
#[test]
fn registrar_avisa_la_unidad_huerfana_y_escribe_la_corrida() {
    let fx = Fixture::new("huerfana");
    let log = fx.log("tests.log", LOG_DOS);
    let mut manifiesto = manifest(&log);
    manifiesto["unidades"] = json!([
        {"id": "tests", "log": log.display().to_string()},
        {"id": "NOPE", "log": log.display().to_string()}
    ]);
    let out = registrar(&fx, Some(fx.root.as_path()), &manifiesto);
    assert_ok(
        &out,
        "registrar with a unit that names no step of its manifest",
    );
    let err = text(&out.stderr);
    assert!(
        err.contains("NOPE"),
        "the orphan unit is not named on stderr: {}",
        show(&out)
    );
    assert_eq!(
        records(&fx.root, "corridas.jsonl").len(),
        1,
        "the run line is missing: {}",
        show(&out)
    );
    assert_eq!(
        records(&fx.root, "indice.jsonl").len(),
        1,
        "the index line is missing: {}",
        show(&out)
    );
    assert_eq!(
        records(&fx.root, "pasos.jsonl").len(),
        2,
        "the step lines are missing: {}",
        show(&out)
    );
    assert_eq!(
        records(&fx.root, "unidades.jsonl").len(),
        2,
        "the units of the known step are missing: {}",
        show(&out)
    );
}

/// Spec F3.3b-0, correction 2: the huella is the sha256 of `git diff HEAD --binary` and of every
/// untracked file. Two trees that differ only by an untracked `.rs` have different huellas; the same
/// bytes in another folder have the same one, so the path is not part of it.
#[test]
fn la_huella_mira_los_no_rastreados_y_los_cambios_sin_commit() {
    let fx = Fixture::new("huella");
    let limpio = fx.base.join("limpio");
    let con_a = fx.base.join("con-a");
    let con_b = fx.base.join("con-b");
    let con_a_en_otra_carpeta = fx.base.join("otra").join("con-a");
    let tocado = fx.base.join("tocado");
    repo_con(&limpio, None, None);
    repo_con(&con_a, Some("fn a() {}\n"), None);
    repo_con(&con_b, Some("fn b() {}\n"), None);
    repo_con(&con_a_en_otra_carpeta, Some("fn a() {}\n"), None);
    repo_con(&tocado, None, Some("dos\n"));
    let rutas: Vec<String> = [&limpio, &con_a, &con_b, &con_a_en_otra_carpeta, &tocado]
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    let args: Vec<&str> = rutas.iter().map(String::as_str).collect();
    let out = py_en(&fx, &scripts(), BODY_HUELLA, &args, None);
    assert_ok(&out, "gate_history.huella over five repositories");
    let listas: Vec<Option<String>> = serde_json::from_str(&text(&out.stdout))
        .unwrap_or_else(|e| panic!("huella printed no JSON list ({e}): {}", show(&out)));
    let h: Vec<String> = listas
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            v.unwrap_or_else(|| panic!("the huella of repository {i} is null: {}", show(&out)))
        })
        .collect();
    assert!(
        h.iter().all(|v| v.len() == 64),
        "a huella is not a sha256 in hex: {h:?}"
    );
    assert_ne!(
        h[0], h[1],
        "an untracked src/x.rs did not change the huella: the clean tree and con-a are equal"
    );
    assert_ne!(
        h[1], h[2],
        "two untracked .rs with other bytes gave the same huella: {h:?}"
    );
    assert_eq!(
        h[1], h[3],
        "the same tree in another folder gave another huella: the path must not take part"
    );
    assert_ne!(
        h[0], h[4],
        "a change to a tracked file that is not committed did not change the huella"
    );
}

/// Spec F3.3b-0, correction 2: outside a repository the huella is null, never a guessed value.
#[test]
fn la_huella_es_null_fuera_de_git() {
    let fx = Fixture::new("huella-nula");
    let carpeta = fx.base.join("sin-git");
    fs::create_dir_all(&carpeta).expect("a folder that is no repository");
    let ruta = carpeta.display().to_string();
    let out = py_en(&fx, &scripts(), BODY_HUELLA, &[ruta.as_str()], None);
    assert_ok(&out, "gate_history.huella outside git");
    assert_eq!(
        text(&out.stdout).trim(),
        "[null]",
        "a folder outside git must give a null huella: {}",
        show(&out)
    );
}

/// Spec F3.3b-0, correction 3: the run line carries the system's pressure, the load, the threads,
/// the two gaps to the bounds, the run's peak and its OOM kills.
#[test]
fn run_end_registra_presion_carga_hilos_brechas_y_oom() {
    let fx = Fixture::new("cierre");
    let out = py(&fx, BODY_CIERRE, &[], Some(fx.root.as_path()));
    assert_ok(&out, "a run with its system figures");
    let corridas = records(&fx.root, "corridas.jsonl");
    let linea = corridas
        .first()
        .unwrap_or_else(|| panic!("no run line: {}", show(&out)));
    assert_eq!(linea["brecha_ref"], json!(2.5), "T/LB_ref is 10/4: {linea}");
    assert_eq!(
        linea["brecha_run"],
        json!(1.25),
        "T/LB_run is 10/8: {linea}"
    );
    assert_eq!(linea["mem_peak"], json!(2097152), "{linea}");
    assert_eq!(
        linea["psi"],
        json!({"cpu": 1.5, "memory": null, "io": 0.2}),
        "the pressure of the system is not the run's: {linea}"
    );
    assert_eq!(linea["load"], json!(3.5), "{linea}");
    assert_eq!(linea["hilos"], json!(24), "{linea}");
    assert_eq!(linea["oom_kill"], json!(0), "{linea}");
}

/// Spec F3.3b-0, correction 3: what nobody measured is null, never 0 and never an invented value.
#[test]
fn run_end_sin_medidas_escribe_null_y_no_cero() {
    let fx = Fixture::new("cierre-nulo");
    let out = py(&fx, BODY_CIERRE_NULO, &[], Some(fx.root.as_path()));
    assert_ok(&out, "a run that measured nothing but its clock");
    let linea = records(&fx.root, "corridas.jsonl")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no run line: {}", show(&out)));
    for campo in [
        "mem_peak",
        "psi",
        "load",
        "hilos",
        "oom_kill",
        "brecha_ref",
        "brecha_run",
    ] {
        assert!(
            linea.get(campo).is_some_and(Value::is_null),
            "{campo} must be a key of the run line and null when nobody measured it: {linea}"
        );
    }
}

/// Spec F3.3b-0, correction 3 through the serial writer: `registrar` hands the bounds, the pressure
/// and the threads of the manifest to the run line.
#[test]
fn registrar_pasa_cotas_presion_y_hilos_a_la_corrida() {
    let fx = Fixture::new("registrar-cierre");
    let log = fx.log("tests.log", LOG_DOS);
    let mut manifiesto = manifest(&log);
    manifiesto["T"] = json!(10.0);
    manifiesto["LB_ref"] = json!(4.0);
    manifiesto["LB_run"] = json!(8.0);
    manifiesto["C"] = json!(4);
    manifiesto["R"] = json!(10737418240u64);
    manifiesto["mem_peak"] = json!(2097152);
    manifiesto["psi"] = json!({"cpu": 1.5, "memory": null, "io": 0.2});
    manifiesto["load"] = json!(3.5);
    manifiesto["hilos"] = json!(24);
    manifiesto["oom_kill"] = json!(0);
    let out = registrar(&fx, Some(fx.root.as_path()), &manifiesto);
    assert_ok(&out, "registrar with the figures of the run");
    let linea = records(&fx.root, "corridas.jsonl")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no run line: {}", show(&out)));
    assert_eq!(linea["brecha_ref"], json!(2.5), "{linea}");
    assert_eq!(linea["brecha_run"], json!(1.25), "{linea}");
    assert_eq!(linea["load"], json!(3.5), "{linea}");
    assert_eq!(linea["hilos"], json!(24), "{linea}");
    assert_eq!(linea["psi"]["cpu"], json!(1.5), "{linea}");
}

/// Spec F3.3b-0, correction 5: a binary that a signal killed keeps its unit, with `rc` the signal
/// as a negative number and no `finished_in`. The binary that finished keeps its own line.
#[test]
fn un_binario_muerto_por_senal_deja_su_unidad_con_rc_y_sin_finished_in() {
    let fx = Fixture::new("senal");
    let log = fx.log("senal.log", LOG_SENAL);
    let out = registrar(&fx, Some(fx.root.as_path()), &manifest(&log));
    assert_ok(
        &out,
        "registrar over a log with a binary killed by a signal",
    );
    assert!(
        !text(&out.stderr).contains("units not read"),
        "the units of the whole step were lost: {}",
        show(&out)
    );
    let unidades = records(&fx.root, "unidades.jsonl");
    assert_eq!(
        unidades.len(),
        2,
        "want the finished binary and the killed one: {unidades:?}"
    );
    let muerto = unidades
        .iter()
        .find(|u| u["clave"] == json!("smoke"))
        .unwrap_or_else(|| panic!("no unit for smoke: {unidades:?}"));
    assert_eq!(
        muerto["rc"],
        json!(-11),
        "a binary killed by signal 11 has rc -11: {muerto}"
    );
    assert!(
        muerto["finished_in"].is_null()
            && muerto["tests"].is_null()
            && muerto["fallados"].is_null(),
        "a killed binary has no time and no count: {muerto}"
    );
    let vivo = unidades
        .iter()
        .find(|u| u["clave"] == json!("lib:memory_industry"))
        .unwrap_or_else(|| panic!("no unit for lib:memory_industry: {unidades:?}"));
    assert_eq!(vivo["finished_in"], json!(21.51), "{vivo}");
    assert_eq!(vivo["rc"], json!(0), "{vivo}");
}

/// Spec F3.3b-0, correction 7: a contract that imports the modules writes no bytecode next to them.
/// `-I` alone wrote `__pycache__`; PYTHONDONTWRITEBYTECODE does nothing under `-I`, so the launcher
/// passes `-B`.
#[test]
fn el_lanzador_de_los_contratos_no_escribe_pycache() {
    let fx = Fixture::new("pycache-lanzador");
    let copia = fx.base.join("scripts");
    fs::create_dir_all(&copia).expect("the copy of the scripts");
    fs::copy(
        scripts().join("gate_history.py"),
        copia.join("gate_history.py"),
    )
    .expect("copy gate_history.py");
    let out = py_en(&fx, &copia, "print(gh.REPO)\n", &[], None);
    assert_ok(&out, "importing gate_history from a copy of the scripts");
    assert!(
        !copia.join("__pycache__").exists(),
        "the import wrote bytecode next to the module in {}",
        copia.display()
    );
}

/// Spec F3.3b-0, correction 4: a planner run records the sha of HEAD and the huella of the tree,
/// not nulls.
#[test]
fn el_planificador_registra_el_sha_y_la_huella_del_arbol() {
    let base = scratch("sha-huella");
    let table = table_file(&base, "tabla.txt", "10 a - - normal 1 5 0.05 /bin/true\n");
    let root = base.join("historial");
    let p = plan(&base, &table, &root, &scripts(), &[]);
    assert_eq!(p.code, Some(0), "one green step: {}{}", p.out, p.err);
    let linea = records(&root, "corridas.jsonl")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no run line: {}", p.err));
    let git_head = Command::new("git")
        .arg("-C")
        .arg(repo_root())
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .expect("git rev-parse");
    let head = String::from_utf8_lossy(&git_head.stdout).trim().to_string();
    assert_eq!(
        linea["sha"].as_str(),
        Some(head.as_str()),
        "the run's sha is not HEAD: {linea}"
    );
    let huella = linea["huella"]
        .as_str()
        .unwrap_or_else(|| panic!("the run's huella is null: {linea}"));
    assert_eq!(huella.len(), 64, "the huella is not a sha256: {huella}");
}

/// Spec F3.3b-0, correction 6: a run that the history could not start is None for the planner. The
/// planner does not hand that None to the history's functions, so no `AttributeError` reaches stderr,
/// and the verdict is the steps' own.
#[test]
fn sin_corrida_de_historia_el_planificador_no_pasa_none() {
    let base = scratch("sin-corrida");
    let table = table_file(&base, "tabla.txt", "10 a - - normal 1 5 0.05 /bin/true\n");
    let root = base.join("historial");
    let (cgroups, proc_self) = fake_cgroup(&base);
    let home = base.join("home");
    fs::create_dir_all(&home).expect("home");
    let out = Command::new("python3")
        .arg("-I")
        .arg("-B")
        .arg("-c")
        .arg(CORRIDA_NONE)
        .arg(scripts())
        .arg(&table)
        .env("GATE_DAG", "0")
        .env("CI_TIEMPOS_DIR", &root)
        .env("HOME", &home)
        .env("TMPDIR", &base)
        .env("GATE_CGROUP_ROOT", &cgroups)
        .env("GATE_PROC_SELF_CGROUP", &proc_self)
        .output()
        .expect("python3 -I -c the planner with no run");
    assert_eq!(
        out.status.code(),
        Some(0),
        "a green table with no history run: {}",
        show(&out)
    );
    let err = text(&out.stderr);
    assert!(
        !err.contains("AttributeError"),
        "the planner handed None to the history, which failed on it: {err}"
    );
}

/// Runs the planner with `run_start` replaced by None. argv: scripts dir, table.
const CORRIDA_NONE: &str = r#"
import importlib.util
import os
import sys
sys.path.insert(0, sys.argv[1])
import gate_history
gate_history.run_start = lambda *args, **kwargs: None
spec = importlib.util.spec_from_file_location("gate_dag", os.path.join(sys.argv[1], "gate-dag.py"))
gate_dag = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate_dag)
sys.exit(gate_dag.main(["run", "--table", sys.argv[2]]))
"#;

// --- CI-J14: the history keeps a half line apart, one function for the critical path, and the properties of the
// planner. The properties run in process (`python3 -I -B -c`, like the rest of this file), with `random` seeded with a
// fixed number and no sleeps: they take milliseconds, and the same run is the same run on every machine.

/// J14f: a history that ends in a line cut halfway, with no `\n`, gets a `\n` before the next record, so the record
/// that follows is read whole and the cut line is only itself. argv: none. Prints the path of the file it wrote.
const BODY_LINEA_A_MEDIAS: &str = r#"
import datetime
day = os.path.join(gh.history_root(), datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%d"))
os.makedirs(day, exist_ok=True)
with open(os.path.join(day, "pasos.jsonl"), "w", encoding="utf-8") as fh:
    fh.write('{"a":1')
corrida = gh.run_start(modo="serie", sha=None, huella=None, tabla="tabla-j14f")
for idx, nombre in [(10, "fmt"), (20, "tests")]:
    paso = gh.step_start(corrida, idx, nombre)
    gh.step_end(corrida, paso, 0, inicio=0.0, fin=0.1)
"#;

/// J14f: the file is the three lines it should be: the cut one, and the two records, each whole.
#[test]
fn j14f_una_linea_a_medias_no_se_come_el_registro_siguiente() {
    let fx = Fixture::new("j14f");
    let out = py(&fx, BODY_LINEA_A_MEDIAS, &[], Some(fx.root.as_path()));
    assert_ok(&out, "two steps after a line cut halfway");
    let files = files_named(&fx.root, "pasos.jsonl");
    assert_eq!(files.len(), 1, "one day, one pasos.jsonl: {files:?}");
    let body = fs::read_to_string(&files[0]).expect("pasos.jsonl");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "the cut line and the two records are three lines; the first record was glued to the cut one: {body:?}"
    );
    assert_eq!(
        lines[0], "{\"a\":1",
        "the cut line is kept as it was: {body:?}"
    );
    for (line, id) in lines[1..].iter().zip(["fmt", "tests"]) {
        let record: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("the record of {id} is not read whole ({e}): {line:?}"));
        assert_eq!(record["id"].as_str(), Some(id), "{record}");
    }
    assert!(
        body.ends_with('\n') && !body.contains("\n\n"),
        "a history that ended in a newline gets no blank line: {body:?}"
    );
}

/// J14j: the critical path is one function, in the history module (the low level), and it returns `(length, chain)`:
/// the length the bound `LB` is made of and the chain the planner prints. Ties go to the first in the order of `seg`
/// (the end of the chain) and to the first of the list `deps` (each step before it). A dependency that has no record
/// is skipped. argv: none.
const BODY_CAMINO_CRITICO: &str = r#"
casos = [
    ({"a": 1.0, "b": 1.0, "c": 1.5}, {"a": [], "b": ["a"], "c": []}),
    ({"a": 1.0, "b": 1.0, "c": 2.0}, {"c": ["a", "b"]}),
    ({"a": 1.0, "b": 2.0}, {"b": ["falta"]}),
    ({"a": 1.0, "b": 1.0}, {}),
]
print(json.dumps([list(gh.camino_critico(seg, deps)) for seg, deps in casos]))
"#;

/// J14j: a dependency cycle is a ValueError that names it, as `lower_bounds` does today. argv: none.
const BODY_CAMINO_CICLO: &str = r#"
try:
    gh.camino_critico({"a": 1.0, "b": 1.0}, {"a": ["b"], "b": ["a"]})
except ValueError as err:
    print("ValueError", err)
"#;

#[test]
fn j14j_el_camino_critico_es_una_funcion_que_da_la_longitud_y_la_cadena() {
    let fx = Fixture::new("j14j");
    let out = py(&fx, BODY_CAMINO_CRITICO, &[], Some(fx.root.as_path()));
    assert_ok(&out, "gate_history.camino_critico");
    let v: Value = serde_json::from_str(text(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("camino_critico did not print JSON: {e}: {}", show(&out)));
    assert_eq!(
        v,
        json!([
            [2.0, ["a", "b"]],
            [3.0, ["a", "c"]],
            [2.0, ["b"]],
            [1.0, ["a"]]
        ]),
        "length and chain of the four cases (the chain from its first step to its last)"
    );
    let out = py(&fx, BODY_CAMINO_CICLO, &[], Some(fx.root.as_path()));
    assert_ok(&out, "a cycle in camino_critico");
    assert!(
        text(&out.stdout).starts_with("ValueError") && text(&out.stdout).contains("ciclo"),
        "a cycle is a ValueError that names it: {}",
        show(&out)
    );
}

/// The two modules and a seeded generator, for the properties. argv: none.
const PRELUDIO_PROPIEDADES: &str = r#"
import importlib.util, random, tempfile, time
spec = importlib.util.spec_from_file_location("gate_dag", os.path.join(sys.argv[1], "gate-dag.py"))
dag = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dag)
GIB = 1 << 30
rng = random.Random(20261109)

def random_dag(n, pesos):
    ids = [f"s{i}" for i in range(n)]
    deps = {sid: (rng.sample(ids[:i], rng.randint(0, min(i, 3))) if i else []) for i, sid in enumerate(ids)}
    seg = {sid: float(rng.choice(pesos)) for sid in ids}
    return ids, deps, seg

def longest(ids, deps, seg):
    best = {}
    for sid in ids:
        best[sid] = seg[sid] + max((best[d] for d in deps[sid]), default=0.0)
    return max(best.values())
"#;

/// J14m P1: for any DAG, the bound LB and the length of the chain the planner prints are the same number, and the
/// chain is a path of the graph. The oracle is a dynamic program written here; the planner's text is parsed.
const BODY_P1: &str = r#"
def run_p1():
    trials = 0
    for pesos in ([0.5, 1.0, 2.5, 4.0], [1, 2, 3]):
        for n in range(1, 11):
            for _ in range(30):
                ids, deps, seg = random_dag(n, pesos)
                want = longest(ids, deps, seg)
                records = [{"id": s, "seg": seg[s], "cpu_s": None, "anon_max": None} for s in ids]
                lb = gh.lower_bounds(records, {"C": 4, "R": 10 * GIB, "deps": deps})
                assert abs(lb["LB"] - want) < 1e-9, f"LB {lb} is not the longest path {want}: {deps} {seg}"
                steps = [dag.Step(10 * (i + 1), s, deps[s], [], "normal", 1.0, 1.0, seg[s], {}, ["true"]) for i, s in enumerate(ids)]
                planner = dag.Planner.__new__(dag.Planner)
                planner.steps = steps
                planner.cpu = 4
                planner.ram = 10 * GIB
                value, texto = planner.bound(seg, {}, {})
                assert abs(value - want) < 1e-9, f"the planner's LB {value} is not {want}"
                assert texto.startswith("critical path: "), f"no chain in {texto!r}"
                chain = texto[len("critical path: "):].split(" -> ")
                assert all(s in seg for s in chain), f"a step that is not in the table: {chain}"
                assert all(a in deps[b] for a, b in zip(chain, chain[1:])), f"{chain} is not a path of {deps}"
                assert not deps[chain[0]], f"the chain starts at {chain[0]}, which has dependencies: {deps}"
                length = sum(seg[s] for s in chain)
                assert abs(length - lb["LB"]) < 1e-9, f"the chain {chain} is {length} long and LB is {lb['LB']}: two functions"
                trials += 1
    return trials

print("p1 trials=%d" % run_p1())
"#;

/// J14m P2: LB is a lower bound of a greedy scheduler that respects the dependencies, the cores and the memory
/// (T >= LB); raising a `seg` or a `cpu_s`, or adding a dependency, never lowers it; raising C or R never raises it.
const BODY_P2: &str = r#"
C = 4
R = 16 * GIB

def instance(n):
    ids, deps, seg = random_dag(n, [1, 2, 3, 5, 8])
    cores = {s: float(rng.choice([1, 2, 4])) for s in ids}
    mem = {s: rng.choice([1, 2, 4, 8]) * GIB for s in ids}
    return ids, deps, seg, {s: cores[s] * seg[s] for s in ids}, mem

def bound(ids, deps, seg, cpu_s, mem, cap=C, ram=R):
    records = [{"id": s, "seg": seg[s], "cpu_s": cpu_s[s], "anon_max": mem[s]} for s in ids]
    return gh.lower_bounds(records, {"C": cap, "R": ram, "deps": deps})["LB"]

def greedy(ids, deps, seg, cpu_s, mem):
    t = 0.0
    done = set()
    running = {}
    pending = list(ids)
    while pending or running:
        moved = True
        while moved:
            moved = False
            for s in list(pending):
                cores = cpu_s[s] / seg[s]
                if all(d in done for d in deps[s]) \
                        and sum(cpu_s[r] / seg[r] for r in running) + cores <= C + 1e-12 \
                        and sum(mem[r] for r in running) + mem[s] <= R:
                    running[s] = t + seg[s]
                    pending.remove(s)
                    moved = True
        assert running, f"the greedy scheduler is stuck with {pending} pending"
        t = min(running.values())
        for s in [r for r, end in running.items() if end <= t + 1e-12]:
            del running[s]
            done.add(s)
    return t

def run_p2():
    trials = 0
    for _ in range(300):
        ids, deps, seg, cpu_s, mem = instance(rng.randint(1, 9))
        lb = bound(ids, deps, seg, cpu_s, mem)
        total = greedy(ids, deps, seg, cpu_s, mem)
        assert total >= lb - 1e-9, f"a greedy schedule of {total} s is under the bound {lb}: {deps} {seg} {cpu_s} {mem}"
        s = rng.choice(ids)
        longer = dict(seg, **{s: seg[s] + rng.choice([1, 3])})
        assert bound(ids, deps, longer, cpu_s, mem) >= lb - 1e-9, f"a longer {s} lowered the bound"
        busier = dict(cpu_s, **{s: cpu_s[s] + rng.choice([1.0, 7.0])})
        assert bound(ids, deps, seg, busier, mem) >= lb - 1e-9, f"more cpu_s in {s} lowered the bound"
        heavier = dict(mem, **{s: mem[s] + rng.choice([1, 4]) * GIB})
        assert bound(ids, deps, seg, cpu_s, heavier) >= lb - 1e-9, f"more memory in {s} lowered the bound"
        pairs = [(a, b) for i, b in enumerate(ids) for a in ids[:i] if a not in deps[b]]
        if pairs:
            a, b = rng.choice(pairs)
            linked = dict(deps, **{b: deps[b] + [a]})
            assert bound(ids, linked, seg, cpu_s, mem) >= lb - 1e-9, f"a new dependency {a} -> {b} lowered the bound"
        assert bound(ids, deps, seg, cpu_s, mem, cap=C + rng.choice([1, 4])) <= lb + 1e-9, "more cores raised the bound"
        assert bound(ids, deps, seg, cpu_s, mem, ram=R + rng.choice([1, 8]) * GIB) <= lb + 1e-9, "more memory raised the bound"
        trials += 1
    return trials

print("p2 trials=%d" % run_p2())
"#;

/// J14m P3: admission and verdict of the planner with no processes (the start of a step is a stand-in that marks it as
/// running; a finish goes through the real `finish`). On random tables: the planner is never stuck; an exclusive step
/// runs alone; with two or more alive the cores add up to no more than C and the memory to no more than R; no step
/// starts before its dependencies exited 0; in series at most one is alive; the verdict is the red of the lowest idx,
/// and every step before it exited 0.
const BODY_P3: &str = r#"
CORES = 8
RAM_GIB = 24

def random_steps(n):
    steps = []
    for i in range(n):
        previous = [s.id for s in steps]
        deps = rng.sample(previous, rng.randint(0, min(len(previous), 2))) if previous else []
        clase = "exclusive" if rng.random() < 0.15 else "normal"
        cpu = round(rng.uniform(0.5, CORES * 1.25), 2)
        ram_gb = round(rng.uniform(0.5, RAM_GIB * 0.7), 2)
        steps.append(dag.Step(10 * (i + 1), f"s{i}", deps, [], clase, cpu, ram_gb, 1.0, {}, ["true"]))
    return steps

def make_planner(steps, solo):
    p = dag.Planner.__new__(dag.Planner)
    p.steps = steps
    p.by_id = {s.id: s for s in steps}
    p.dag = True
    p.solo = solo
    p.rank = {s.id: rng.random() for s in steps}
    p.t0 = time.monotonic()
    p.delegado = False
    p.scope = None
    p.cpu_cap = CORES
    p.ram = RAM_GIB * GIB
    p.red = None
    p.no_verdict = None
    p.corrida = None

    def start(step):
        for d in step.deps:
            assert p.by_id[d].state == "done" and p.by_id[d].rc == 0, f"{step.id} started before {d} exited 0"
        step.state = "running"
        step.t_start = p.now()

    p.start = start
    p.kill = lambda step: setattr(step, "parado", True)
    return p

def check_alive(p):
    alive = p.running()
    if any(s.clase in dag.EXCLUSIVE for s in alive):
        assert len(alive) == 1, f"an exclusive step shares the machine with {[s.id for s in alive]}"
    if p.solo:
        assert len(alive) <= 1, f"in series, {[s.id for s in alive]} are alive"
    if len(alive) >= 2:
        assert sum(s.c for s in alive) <= p.cpu_cap + 1e-9, f"{[s.id for s in alive]} ask for more cores than {p.cpu_cap}"
        assert sum(max(s.anon_now or 0, s.r) for s in alive) <= p.ram, f"{[s.id for s in alive]} ask for more memory than R"

def run_p3():
    trials = 0
    for _ in range(300):
        steps = random_steps(rng.randint(1, 10))
        p = make_planner(steps, rng.random() < 0.3)
        rcs = {s.id: rng.choice([0, 0, 0, 0, 0, 1, 2]) for s in steps}
        guard = 0
        while p.unfinished():
            guard += 1
            assert guard < 200, "the planner does not end"
            p.admit()
            assert not p.no_verdict, p.no_verdict
            alive = p.running()
            assert alive, f"stuck: nothing runs and these are left: {[(s.id, s.state) for s in steps]}"
            check_alive(p)
            step = rng.choice(alive)
            p.finish(step, -9 if step.parado else rcs[step.id])
        failed = [s for s in steps if s.state == "done" and not s.parado and s.rc != 0]
        if failed:
            lowest = min(failed, key=lambda s: s.idx)
            assert p.red is lowest, f"the red is {p.red and p.red.id}, the lowest idx that failed is {lowest.id}"
            for s in steps:
                if s.idx < lowest.idx:
                    assert s.state == "done" and s.rc == 0 and not s.parado, f"{s.id} is before the red {lowest.id} and did not exit 0"
        else:
            assert p.red is None, f"no step failed and the red is {p.red.id}"
        trials += 1
    return trials

print("p3 trials=%d" % run_p3())
"#;

/// J14m P4: `parse_table` over the real table with random mutations (a token changed to nan, inf, a negative, a 0, an
/// overflow or text; a token or a row dropped, repeated or swapped) answers with a list of steps or a Refusal, never
/// with another exception, and every step it accepts has numbers that are finite and above 0.
const BODY_P4: &str = r#"
import math
real = open(os.path.join(sys.argv[1], "gate-dag.txt"), encoding="utf-8").read().splitlines()
rows = [line for line in real if line.strip() and not line.startswith(chr(35))]
assert len(rows) >= 9, f"the real table has {len(rows)} rows"
work = tempfile.mkdtemp(dir=os.environ.get("TMPDIR"))
TOKENS = ["nan", "inf", "-inf", "-1", "0", "1e999", "abc", "", "1,2", "-", "target:", "cluster:x", "models", "reader",
          "normal", "exclusive", "boundary", "untimed", "BOGUS", "FOO=bar", "CARGO_TARGET_DIR=/x", "99999999999999999999",
          "1e-9", "0.0", "-0"]

def mutate(lines):
    lines = list(lines)
    for _ in range(rng.randint(1, 3)):
        op = rng.choice(["token", "delete", "dup", "swap", "drop", "append"])
        i = rng.randrange(len(lines))
        tok = lines[i].split()
        if op == "token" and tok:
            tok[rng.randrange(len(tok))] = rng.choice(TOKENS)
            lines[i] = " ".join(tok)
        elif op == "delete" and tok:
            del tok[rng.randrange(len(tok))]
            lines[i] = " ".join(tok)
        elif op == "dup":
            lines.insert(i, lines[i])
        elif op == "swap":
            j = rng.randrange(len(lines))
            lines[i], lines[j] = lines[j], lines[i]
        elif op == "drop" and len(lines) > 1:
            del lines[i]
        else:
            lines[i] += " " + rng.choice(TOKENS)
    return lines

def parse(lines, name):
    path = os.path.join(work, name)
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")
    return dag.parse_table(path)

assert len(parse(rows, "real.txt")) == len(rows), "the real table does not parse"
accepted = refused = 0
for n in range(600):
    lines = mutate(rows)
    try:
        steps = parse(lines, f"m{n}.txt")
    except dag.Refusal:
        refused += 1
        continue
    except Exception as err:
        raise AssertionError(f"parse_table raised {type(err).__name__}: {err} for {lines}")
    accepted += 1
    for s in steps:
        for name, value in (("cpu", s.cpu_tab), ("ram_gb", s.ram_tab), ("dur_s", s.dur_tab)):
            assert math.isfinite(value) and value > 0, f"{s.id} was accepted with {name}={value}: {lines}"
assert accepted > 0 and refused > 0, f"the mutations were all accepted or all refused ({accepted}/{refused})"
print("p4 trials=600 accepted=%d refused=%d" % (accepted, refused))
"#;

/// The properties run as one process each, with the prelude of the generator.
fn propiedad(nombre: &str, cuerpo: &str, intentos: &str) {
    let fx = Fixture::new(nombre);
    let codigo = format!("{PRELUDIO_PROPIEDADES}{cuerpo}");
    let out = py(&fx, &codigo, &[], Some(fx.root.as_path()));
    assert_ok(&out, nombre);
    assert!(
        text(&out.stdout).contains(intentos),
        "{nombre} did not say it ran its trials ({intentos}): {}",
        show(&out)
    );
}

#[test]
fn j14m_p1_el_lb_y_la_cadena_impresa_son_la_misma_cuenta() {
    propiedad("j14m-p1", BODY_P1, "p1 trials=600");
}

#[test]
fn j14m_p2_el_lb_es_una_cota_inferior_y_monotona() {
    propiedad("j14m-p2", BODY_P2, "p2 trials=300");
}

#[test]
fn j14m_p3_la_admision_y_el_veredicto_cumplen_sus_invariantes() {
    propiedad("j14m-p3", BODY_P3, "p3 trials=300");
}

#[test]
fn j14m_p4_la_tabla_da_una_lista_o_una_negativa_y_nunca_otra_excepcion() {
    propiedad("j14m-p4", BODY_P4, "p4 trials=600");
}

// --- CI-K4: the reports of `cargo --timings`, the overlap of the rows and the plan of the machine ---------------------
// D-K4h (keep_timings moves the reports a row's log names, from its target to the day's folder of the history), D-K4j
// (the folders of days older than 30 UTC days lose their timings/ and nothing else), D-K4q (`plan` in the run's line),
// D-K4r (`solape_s`). The API these tests fix for pass 2:
// - `keep_timings(corrida, paso, log_path) -> dict | None`: reads the lines `Timing report saved to <path>` of the log of
//   the row and moves each report that exists to `<root>/<AAAAMMDD>/timings/<run>/<idx>-<id>/`. It returns
//   `{dir, n, bytes, perdidos, faltan}` (`dir` relative to the root of the history; `perdidos` the times a path is named
//   again, `faltan` the paths that are not there) or None: when the log names none, and when the history cannot take
//   them (then one `history:` line, and the reports stay where they were). It also prunes the retention.
// - `solape_s(intervalos, i)`: seconds of the interval `i` in which another interval of `intervalos` is alive.
// - `step_end(..., timings=None, solape_s=None)` and `run_end(..., plan=None)` write those fields, null when not given.

/// Four reports in `<objetivo>/cargo-timings` (4, 6, 2 and 2 bytes) and the function that keeps what a log names.
/// argv: the directory of the target. `guardar(lineas)` writes the log, keeps, and returns what the day was before and
/// after the call (a run that crosses midnight UTC names either) and what `keep_timings` returned.
const K4_PRE: &str = r#"
from datetime import datetime, timedelta, timezone
objetivo = sys.argv[2]
informes = objetivo + "/cargo-timings"
os.makedirs(informes, exist_ok=True)
for nombre, cuerpo in [("cargo-timing-20261109T120000Z.html", b"AAAA"), ("cargo-timing-20261109T120001Z.html", b"BBBBBB"),
                       ("cargo-timing-20261109T120002Z.html", b"CC"), ("cargo-timing.html", b"DD")]:
    with open(informes + "/" + nombre, "wb") as fh:
        fh.write(cuerpo)
def nombrado(nombre):
    return "   Timing report saved to " + informes + "/" + nombre
def dia():
    return datetime.now(timezone.utc).strftime("%Y%m%d")
def guardar(lineas):
    log = objetivo + "/fila.log"
    with open(log, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lineas) + "\n")
    corrida = gh.run_start(modo="serie", sha=None, huella=None, tabla="t", run="run-k4")
    paso = gh.step_start(corrida, 10, "DBG")
    antes = dia()
    res = gh.keep_timings(corrida, paso, log)
    return {"res": res, "antes": antes, "despues": dia()}
"#;

const K4_NOMBRES: [&str; 4] = [
    "cargo-timing-20261109T120000Z.html",
    "cargo-timing-20261109T120001Z.html",
    "cargo-timing-20261109T120002Z.html",
    "cargo-timing.html",
];

const BODY_K4_GUARDAR: &str = r#"
print(json.dumps(guardar(["   Compiling x v0.1.0", nombrado("cargo-timing-20261109T120000Z.html"),
                          nombrado("cargo-timing-20261109T120001Z.html"), "    Finished `dev` profile"])))
"#;

const BODY_K4_CUENTA: &str = r#"
print(json.dumps(guardar([nombrado("cargo-timing-20261109T120000Z.html"), nombrado("cargo-timing-20261109T120000Z.html"),
                          nombrado("cargo-timing-20269999T000000Z.html")])))
"#;

const BODY_K4_NADA: &str = r#"
print(json.dumps(guardar(["   Compiling x v0.1.0", "    Finished `dev` profile"])))
"#;

/// Folders of days 31, 30 and 29 days ago, and one that is no day, each with a timings/ and a pasos.jsonl; then a row
/// that keeps a report today. The names of the folders come back with the answer.
const BODY_K4_RETENCION: &str = r#"
raiz = os.path.join(os.environ["CI_TIEMPOS_DIR"], "memory-industry")
def sembrar(nombre):
    carpeta = os.path.join(raiz, nombre)
    os.makedirs(carpeta + "/timings/run-vieja/10-a", exist_ok=True)
    with open(carpeta + "/timings/run-vieja/10-a/cargo-timing-vieja.html", "w") as fh:
        fh.write("v")
    with open(carpeta + "/pasos.jsonl", "w") as fh:
        fh.write('{"id": "a"}\n')
    return nombre
hoy = datetime.now(timezone.utc)
dias = {str(n): sembrar((hoy - timedelta(days=n)).strftime("%Y%m%d")) for n in (31, 30, 29)}
dias["ninguno"] = sembrar("no-es-un-dia")
resultado = guardar([nombrado("cargo-timing-20261109T120000Z.html")])
resultado["dias"] = dias
print(json.dumps(resultado))
"#;

/// The answer of a K4 body: the JSON line it printed.
fn k4_answer(out: &Output) -> Value {
    serde_json::from_str(text(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("the body printed no JSON: {e}\n{}", show(out)))
}

/// A target directory of the fixture for the K4 bodies, and where the reports are in it.
fn k4_target(fx: &Fixture) -> PathBuf {
    let dir = fx.base.join("objetivo");
    fs::create_dir_all(&dir).expect("target dir");
    dir
}

/// Spec K4-A, D-K4h: `keep_timings` moves what the log names, with the exact count and the exact weight, to the folder
/// of the day, the run and the row, and `dir` is relative to the root of the history. What the log does not name stays.
#[test]
fn keep_timings_mueve_los_informes_que_nombra_el_log_y_cuenta_exacto() {
    let fx = Fixture::new("k4-guardar");
    let target = k4_target(&fx);
    let out = py(
        &fx,
        &format!("{K4_PRE}{BODY_K4_GUARDAR}"),
        &[path_str(&target)],
        Some(fx.root.as_path()),
    );
    assert_ok(&out, "keep_timings of a log that names two reports");
    let answer = k4_answer(&out);
    let res = &answer["res"];
    let dir = res["dir"]
        .as_str()
        .unwrap_or_else(|| panic!("keep_timings gave no dir: {answer}\n{}", show(&out)));
    let parts: Vec<&str> = dir.split('/').collect();
    assert_eq!(parts.len(), 4, "<day>/timings/<run>/<idx>-<id>: {dir}");
    assert!(
        [&answer["antes"], &answer["despues"]]
            .iter()
            .any(|day| day.as_str() == Some(parts[0])),
        "the day folder is the UTC day of the call: {dir} / {answer}"
    );
    assert_eq!(&parts[1..], ["timings", "run-k4", "10-DBG"], "{dir}");
    assert_eq!(res["n"], json!(2), "{answer}");
    assert_eq!(res["bytes"], json!(10), "{answer}");
    assert_eq!(res["perdidos"], json!(0), "{answer}");
    assert_eq!(res["faltan"], json!(0), "{answer}");
    let kept = fx.root.join(REPO).join(dir);
    for (name, body) in [(K4_NOMBRES[0], "AAAA"), (K4_NOMBRES[1], "BBBBBB")] {
        assert_eq!(
            fs::read_to_string(kept.join(name)).unwrap_or_default(),
            body,
            "{name} is not in {}",
            kept.display()
        );
        assert!(
            !target.join("cargo-timings").join(name).exists(),
            "{name} was copied and not moved"
        );
    }
    for stays in [K4_NOMBRES[2], K4_NOMBRES[3]] {
        assert!(
            target.join("cargo-timings").join(stays).exists(),
            "{stays} is not named by the log and left the target"
        );
    }
    assert_eq!(history_lines(&text(&out.stderr)), 0, "{}", show(&out));
}

/// Spec K4-A, D-K4h: a report named twice counts once and is a `perdido`; a path named that is not there is a `falta`.
#[test]
fn keep_timings_cuenta_lo_perdido_y_lo_que_falta() {
    let fx = Fixture::new("k4-cuenta");
    let target = k4_target(&fx);
    let out = py(
        &fx,
        &format!("{K4_PRE}{BODY_K4_CUENTA}"),
        &[path_str(&target)],
        Some(fx.root.as_path()),
    );
    assert_ok(
        &out,
        "keep_timings of a log with a repeated and a missing report",
    );
    let answer = k4_answer(&out);
    let res = &answer["res"];
    assert_eq!(res["n"], json!(1), "{res}");
    assert_eq!(res["bytes"], json!(4), "{res}");
    assert_eq!(res["perdidos"], json!(1), "{res}");
    assert_eq!(res["faltan"], json!(1), "{res}");
}

/// Spec K4-A, D-K4h: a log that names no report gives None, moves nothing and says nothing.
#[test]
fn keep_timings_de_un_log_sin_informes_es_none_y_no_mueve_nada() {
    let fx = Fixture::new("k4-nada");
    let target = k4_target(&fx);
    let out = py(
        &fx,
        &format!("{K4_PRE}{BODY_K4_NADA}"),
        &[path_str(&target)],
        Some(fx.root.as_path()),
    );
    assert_ok(&out, "keep_timings of a log with no report");
    let answer = k4_answer(&out);
    assert!(
        answer.get("res").is_some_and(Value::is_null),
        "no report named, so None: {answer}"
    );
    for name in K4_NOMBRES {
        assert!(
            target.join("cargo-timings").join(name).exists(),
            "{name} left the target although the log named none"
        );
    }
    assert!(
        files_named(&fx.root, "cargo-timing-20261109T120000Z.html").is_empty(),
        "a report reached the history"
    );
    assert_eq!(history_lines(&text(&out.stderr)), 0, "{}", show(&out));
}

/// Spec K4-A, D-K4h: a history that cannot be written is None, one `history:` line and the reports where they were.
/// The control is the same call with a history that can be written, which moves them.
#[test]
fn keep_timings_con_un_historial_que_no_se_escribe_es_none_avisa_y_deja_los_informes() {
    let fx = Fixture::new("k4-sin-escritura");
    let target = k4_target(&fx);
    let control = py(
        &fx,
        &format!("{K4_PRE}{BODY_K4_GUARDAR}"),
        &[path_str(&target)],
        Some(fx.root.as_path()),
    );
    assert_ok(&control, "control");
    assert!(
        !target.join("cargo-timings").join(K4_NOMBRES[0]).exists(),
        "control: with a history that can be written the report moves"
    );
    let fx = Fixture::new("k4-sin-escritura-roto");
    let target = k4_target(&fx);
    let blocker = fx.base.join("no-es-un-directorio");
    write(&blocker, "x");
    let broken = blocker.join("historial");
    let out = py(
        &fx,
        &format!("{K4_PRE}{BODY_K4_GUARDAR}"),
        &[path_str(&target)],
        Some(broken.as_path()),
    );
    assert_ok(&out, "keep_timings over a history that cannot be written");
    assert!(
        k4_answer(&out).get("res").is_some_and(Value::is_null),
        "a history that cannot take the reports gives None: {}",
        show(&out)
    );
    assert_eq!(
        history_lines(&text(&out.stderr)),
        1,
        "one `history:` line: {}",
        show(&out)
    );
    for name in K4_NOMBRES {
        assert!(
            target.join("cargo-timings").join(name).exists(),
            "{name} left the target although the history could not take it"
        );
    }
}

/// Spec K4-A, D-K4j: when it keeps reports, `keep_timings` removes the timings/ of the day folders older than 30 UTC
/// days (the one of 31 days), and only that: the one of exactly 30 days and the one of 29 stay whole, the `pasos.jsonl`
/// of the old one stays as it was, and a folder that is no day is not touched.
#[test]
fn keep_timings_poda_los_timings_de_mas_de_30_dias_y_nada_mas() {
    let fx = Fixture::new("k4-retencion");
    let target = k4_target(&fx);
    let out = py(
        &fx,
        &format!("{K4_PRE}{BODY_K4_RETENCION}"),
        &[path_str(&target)],
        Some(fx.root.as_path()),
    );
    assert_ok(&out, "keep_timings with old day folders");
    let answer = k4_answer(&out);
    let root = fx.root.join(REPO);
    let day = |key: &str| root.join(answer["dias"][key].as_str().expect("a seeded folder"));
    let old = day("31");
    assert!(
        !old.join("timings").exists(),
        "the timings/ of a folder of 31 days is still there: {}",
        show(&out)
    );
    assert_eq!(
        fs::read_to_string(old.join("pasos.jsonl")).unwrap_or_default(),
        "{\"id\": \"a\"}\n",
        "the pasos.jsonl of the old folder changed: the history is never pruned, only its reports"
    );
    for (key, why) in [
        ("30", "30 days is not more than 30"),
        ("29", "29 days"),
        ("ninguno", "a folder that is no day"),
    ] {
        assert!(
            day(key)
                .join("timings/run-vieja/10-a/cargo-timing-vieja.html")
                .is_file(),
            "the timings/ of the folder of {why} was removed"
        );
    }
    assert!(
        answer["res"]["dir"].is_string(),
        "the reports of today were kept: {answer}"
    );
}

/// Spec K4-B, D-K4r, by hand: the seconds of the interval `i` in which another is alive, the union and not the sum.
/// [(0,10),(2,4),(3,6)] is 4, 2 and 3; two intervals that touch do not overlap.
#[test]
fn solape_s_da_los_ejemplos_a_mano() {
    let fx = Fixture::new("k4-solape-mano");
    let out = py(
        &fx,
        r#"
casos = [[(0, 10), (2, 4), (3, 6)], [(0, 5), (5, 9)], [(0, 5), (0, 5)], [(1, 2)]]
print(json.dumps([[gh.solape_s(c, i) for i in range(len(c))] for c in casos]))
"#,
        &[],
        Some(fx.root.as_path()),
    );
    assert_ok(&out, "solape_s over the examples");
    let got: Vec<Vec<f64>> = serde_json::from_str(text(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("{e}: {}", show(&out)));
    assert_eq!(
        got,
        vec![
            vec![4.0, 2.0, 3.0],
            vec![0.0, 0.0],
            vec![5.0, 5.0],
            vec![0.0]
        ]
    );
}

/// Spec K4-B, D-K4r: for every combination of three integer intervals with 0 <= start < end <= 6, `solape_s` is what
/// counting the unit cells of an interval that another one covers gives. The expectation is the count, not the formula.
#[test]
fn solape_s_coincide_con_contar_las_celdas_que_otro_cubre() {
    let fx = Fixture::new("k4-solape-celdas");
    let out = py(
        &fx,
        r#"
import itertools
intervalos = [(a, b) for a in range(7) for b in range(a + 1, 7)]
print(json.dumps([[gh.solape_s(list(c), i) for i in range(3)] for c in itertools.product(intervalos, repeat=3)]))
"#,
        &[],
        Some(fx.root.as_path()),
    );
    assert_ok(&out, "solape_s over every combination");
    let shown = show(&out);
    let got: Vec<Vec<f64>> = serde_json::from_str(text(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("{e}: {}", &shown[..shown.len().min(300)]));
    let intervals: Vec<(u32, u32)> = (0..7)
        .flat_map(|a| (a + 1..7).map(move |b| (a, b)))
        .collect();
    let mut n = 0;
    for a in &intervals {
        for b in &intervals {
            for c in &intervals {
                let all = [*a, *b, *c];
                for (i, own) in all.iter().enumerate() {
                    let covered = (own.0..own.1)
                        .filter(|cell| {
                            all.iter()
                                .enumerate()
                                .any(|(j, other)| j != i && other.0 <= *cell && *cell < other.1)
                        })
                        .count() as f64;
                    assert_eq!(
                        got[n][i], covered,
                        "solape_s({all:?}, {i}) is {} and the cells say {covered}",
                        got[n][i]
                    );
                }
                n += 1;
            }
        }
    }
    assert_eq!(got.len(), n, "the enumerations do not have the same size");
}

/// Spec K4-A, K4-B: `step_end` takes `timings` and `solape_s`, and `run_end` takes `plan`; the records carry them as
/// given, and as null (a key, not a gap) when nobody gave them. The plan is also in the index.
#[test]
fn step_end_acepta_timings_y_solape_s_y_run_end_acepta_el_plan() {
    let fx = Fixture::new("k4-campos");
    let out = py(
        &fx,
        r#"
corrida = gh.run_start(modo="serie", sha=None, huella=None, tabla="t", run="r1")
p1 = gh.step_start(corrida, 10, "a")
gh.step_end(corrida, p1, rc=0, timings={"dir": "20261109/timings/r1/10-a", "n": 2, "bytes": 10, "perdidos": 0, "faltan": 0}, solape_s=1.5)
p2 = gh.step_start(corrida, 20, "b")
gh.step_end(corrida, p2, rc=0)
gh.run_end(corrida, rc=0, plan={"nproc": 24, "jobs": 15, "carriles": 2, "fallo": None})
otra = gh.run_start(modo="serie", sha=None, huella=None, tabla="t", run="r2")
gh.run_end(otra, rc=0)
"#,
        &[],
        Some(fx.root.as_path()),
    );
    assert_ok(
        &out,
        "step_end with timings and solape_s, run_end with a plan",
    );
    assert_eq!(history_lines(&text(&out.stderr)), 0, "{}", show(&out));
    let steps = records(&fx.root, "pasos.jsonl");
    assert_eq!(
        steps[0]["timings"],
        json!({"dir": "20261109/timings/r1/10-a", "n": 2, "bytes": 10, "perdidos": 0, "faltan": 0}),
        "{}",
        steps[0]
    );
    assert_eq!(steps[0]["solape_s"], json!(1.5), "{}", steps[0]);
    for campo in ["timings", "solape_s"] {
        assert!(
            steps[1].get(campo).is_some_and(Value::is_null),
            "{campo} has to be a key of the record and null when nobody gave it: {}",
            steps[1]
        );
    }
    let plan = json!({"nproc": 24, "jobs": 15, "carriles": 2, "fallo": null});
    for name in ["corridas.jsonl", "indice.jsonl"] {
        let lines = records(&fx.root, name);
        let first = lines.iter().find(|l| l["run"] == json!("r1"));
        let second = lines.iter().find(|l| l["run"] == json!("r2"));
        assert_eq!(first.map(|l| &l["plan"]), Some(&plan), "{name}: {lines:?}");
        assert!(
            second.is_some_and(|l| l.get("plan").is_some_and(Value::is_null)),
            "a run that gave no plan has `plan` as a key and null in {name}: {lines:?}"
        );
    }
}

// --- CI-E4: the seconds in series and the cores of a step and of a run ----------------------------------------------
// D-E4b, D-E4h, D-E4k, D-E4l. The API these tests fix for pass 2:
// - `serie_final(intervalos) -> (serie_final_s, serie_cola_s) | None`, over tuples `(listo, inicio, fin)`: the seconds in
//   which exactly one row is alive and another is ready to start (the wait) or no row will start after it (the tail).
// - `step_end(..., sin_medida=None)`: the record adds `nucleos` (cpu_s over fin - inicio, null when cpu_s is null or the
//   step took 0 s) and `sin_medida` ({} by default). A null `nucleos` takes the reason the caller gave for `cpu_s`, or
//   "the step took 0 s"; a reason nobody gave is not invented.
// - `run_end(..., mem_muestra=None, serie_final_s=None, serie_cola_s=None, sin_medida=None)`: the record (and the index)
//   adds `nucleos` (the sum of the cpu_s of the steps over T), the three values as given and `sin_medida`.

/// The nine rows of the 19th run of the history (pasos.jsonl of 8-nov-2026, `listo = inicio - espera_s`): P, DBG, NLI,
/// B3, REL, E2E, DC, CV and M, as `(listo, inicio, fin)`. Worked out in the spec of CI-E4 (section 0.3): the wait is
/// 1006.421 s and the tail 146.363 s, so 1152.784 s of the 1157.205 s of the run.
const E4_RUN19: &str = "[(0.054, 0.057, 4.257), (4.259, 4.262, 395.298), (395.358, 395.363, 432.871), \
     (4.259, 432.875, 434.568), (434.568, 434.571, 537.101), (434.568, 537.104, 717.785), \
     (4.258, 717.788, 942.179), (4.259, 942.254, 1010.836), (4.258, 1010.840, 1157.203)]";

/// Spec H-E4-1: `serie_final` on the examples worked out by hand (the comments say which seconds count) and on the 19th run.
const BODY_E4_SERIE_MANO: &str = r#"
casos = [
    [(0, 0, 10), (0, 0, 4), (4, 6, 8)],
    [(0, 0, 5), (5, 5, 8), (0, 0, 2)],
    [(0, 0, 3), (0, 3, 5)],
    [(0, 0, 2), (2.5, 3, 4)],
    [(0, 0, 5), (0, 0, 5)],
    [(0, 0, 7)],
    [],
    @RUN19@,
]
print(json.dumps([gh.serie_final(c) for c in casos]))
"#;

/// Spec H-E4-1. By hand, second by second:
/// 1. [4,6) one alive with the third ready = 2; [8,10) the first alone and nobody starts after = 2: (4, 2).
/// 2. [2,5) one alive and the second blocked by its dependencies does not count; [5,8) the tail = 3: (3, 3).
/// 3. [0,3) the first alone with the second ready = 3; [3,5) the tail = 2: (5, 2).
/// 4. [0,2) the first alone, the second not ready yet: nothing; [2,3) nobody alive; [3,4) the tail = 1: (1, 1).
/// 5. two alive all the time: (0, 0).  6. one row alone: it is the tail: (7, 7).  7. no row: None.
#[test]
fn serie_final_da_los_ejemplos_a_mano_y_la_corrida_19_e4() {
    let fx = Fixture::new("e4-serie-mano");
    let body = BODY_E4_SERIE_MANO.replace("@RUN19@", E4_RUN19);
    let out = py(&fx, &body, &[], Some(fx.root.as_path()));
    assert_ok(&out, "serie_final over the examples");
    let got: Vec<Value> = serde_json::from_str(text(&out.stdout).trim())
        .unwrap_or_else(|e| panic!("{e}: {}", show(&out)));
    let want = [
        json!([4.0, 2.0]),
        json!([3.0, 3.0]),
        json!([5.0, 2.0]),
        json!([1.0, 1.0]),
        json!([0.0, 0.0]),
        json!([7.0, 7.0]),
        Value::Null,
    ];
    assert_eq!(got.len(), 8, "{got:?}");
    assert_eq!(&got[..7], &want[..], "the seven worked examples: {got:?}");
    let (total, tail) = (got[7][0].as_f64(), got[7][1].as_f64());
    assert!(
        total.is_some_and(|s| (s - 1152.784).abs() <= 0.001),
        "the 19th run is 1152.784 s in series: {:?}",
        got[7]
    );
    assert!(
        tail.is_some_and(|s| (s - 146.363).abs() <= 0.001),
        "the tail of the 19th run is 146.363 s: {:?}",
        got[7]
    );
}

/// Spec H-E4-2: 500 random runs of one to six rows with integer times 0 <= listo <= inicio < fin <= 20. The oracle is
/// in the body: it counts each cell [k, k+1) with the definition (one alive and another ready: the wait; one alive and
/// no start after it: the tail) and `serie_final` has to give that sum exactly, within the bounds of any run.
const BODY_E4_SERIE_CELDAS: &str = r#"
import random
rng = random.Random(20261110)
casos = 0
for _ in range(500):
    filas = []
    for _ in range(rng.randint(1, 6)):
        listo = rng.randint(0, 19)
        inicio = rng.randint(listo, 19)
        filas.append((listo, inicio, rng.randint(inicio + 1, 20)))
    espera = cola = 0
    for k in range(20):
        vivos = [f for f in filas if f[1] <= k < f[2]]
        esperando = [f for f in filas if f[0] <= k < f[1]]
        despues = [f for f in filas if f[1] > k]
        if len(vivos) == 1 and esperando:
            espera += 1
        elif len(vivos) == 1 and not despues:
            cola += 1
    total, la_cola = gh.serie_final(filas)
    assert total == espera + cola, f"serie_final of {filas} is {total} and the cells say {espera + cola}"
    assert la_cola == cola, f"the tail of {filas} is {la_cola} and the cells say {cola}"
    assert 0 <= la_cola <= total <= max(f[2] for f in filas) - min(f[1] for f in filas), f"{filas}: {total} {la_cola}"
    casos += 1
print("e4 casos=%d" % casos)
"#;

#[test]
fn serie_final_coincide_con_contar_las_celdas_e4() {
    let fx = Fixture::new("e4-serie-celdas");
    let out = py(&fx, BODY_E4_SERIE_CELDAS, &[], Some(fx.root.as_path()));
    assert_ok(&out, "serie_final over 500 random runs");
    assert!(
        text(&out.stdout).contains("e4 casos=500"),
        "the property did not say it ran its 500 cases: {}",
        show(&out)
    );
}

/// Spec H-E4-3: four steps. `a` has cpu_s 1.5 over 3 s of its own, with a second of wait that does not divide (0.5 and not
/// 0.375); `b` has no cpu_s and the reason the planner gave; `c` took 0 s; `d` is a call of the old shape (no reason).
const BODY_E4_NUCLEOS_PASO: &str = r#"
corrida = gh.run_start(modo="serie", sha=None, huella=None, tabla="t", run="r-e4-3")
a = gh.step_start(corrida, 10, "a")
gh.step_end(corrida, a, rc=0, inicio=2.0, fin=5.0, espera_s=1.0, cpu_s=1.5)
b = gh.step_start(corrida, 20, "b")
gh.step_end(corrida, b, rc=0, inicio=2.0, fin=5.0, espera_s=1.0, cpu_s=None, sin_medida={"cpu_s": "r"})
c = gh.step_start(corrida, 30, "c")
gh.step_end(corrida, c, rc=0, inicio=3.0, fin=3.0, cpu_s=1.0)
d = gh.step_start(corrida, 40, "d")
gh.step_end(corrida, d, rc=0, cpu_s=None)
"#;

#[test]
fn los_nucleos_de_un_paso_son_cpu_entre_seg_y_null_con_su_motivo_e4() {
    let fx = Fixture::new("e4-nucleos-paso");
    let out = py(&fx, BODY_E4_NUCLEOS_PASO, &[], Some(fx.root.as_path()));
    assert_ok(&out, "step_end with cores and reasons");
    assert_eq!(history_lines(&text(&out.stderr)), 0, "{}", show(&out));
    let steps = records(&fx.root, "pasos.jsonl");
    assert_eq!(steps.len(), 4, "{steps:?}");
    assert_eq!(steps[0]["nucleos"], json!(0.5), "{}", steps[0]);
    assert_eq!(steps[0]["sin_medida"], json!({}), "{}", steps[0]);
    assert!(
        steps[1]["cpu_s"].is_null() && steps[1]["nucleos"].is_null(),
        "{}",
        steps[1]
    );
    assert_eq!(
        steps[1]["sin_medida"],
        json!({"cpu_s": "r", "nucleos": "r"}),
        "a null of cores takes the reason of the cpu_s that was not measured: {}",
        steps[1]
    );
    assert!(steps[2]["nucleos"].is_null(), "{}", steps[2]);
    assert!(
        steps[2]["sin_medida"]["nucleos"]
            .as_str()
            .is_some_and(|why| why.contains("0 s")),
        "a step of 0 s gives no cores and says why: {}",
        steps[2]
    );
    assert_eq!(steps[2]["cpu_s"], json!(1.0), "{}", steps[2]);
    assert!(steps[3]["nucleos"].is_null(), "{}", steps[3]);
    assert_eq!(
        steps[3]["sin_medida"],
        json!({}),
        "a call with no `sin_medida` writes its line with an empty one, and invents no reason: {}",
        steps[3]
    );
}

/// Spec H-E4-4: three runs. r1: three steps of 1.5 CPU-seconds, T = 9 s and every new argument. r2: the second step is
/// not measured, with its reason. r3: a run of the old shape.
const BODY_E4_NUCLEOS_CORRIDA: &str = r#"
def corrida_de(run, cpus, **extra):
    corrida = gh.run_start(modo="serie", sha=None, huella=None, tabla="t", run=run)
    for idx, (nombre, cpu, motivos) in enumerate(cpus, 1):
        paso = gh.step_start(corrida, 10 * idx, nombre)
        gh.step_end(corrida, paso, rc=0, inicio=0.0, fin=1.0, cpu_s=cpu, sin_medida=motivos)
    gh.run_end(corrida, rc=0, **extra)

corrida_de("r1", [("a", 1.5, None), ("b", 1.5, None), ("c", 1.5, None)], T=9.0,
           mem_muestra={"t": 1.5, "current": 7, "anon": 4, "file": 2, "shmem": 1, "kernel": 1, "muestras": 5},
           serie_final_s=4.0, serie_cola_s=2.0, sin_medida={"mem_peak": "m"})
corrida_de("r2", [("a", 1.5, None), ("b", None, {"cpu_s": "step b: x"})], T=9.0, sin_medida={"cpu_s": "step b: x"})
corrida_de("r3", [("a", 1.5, None)], T=3.0)
"#;

#[test]
fn los_nucleos_y_los_campos_nuevos_de_la_corrida_e4() {
    let fx = Fixture::new("e4-nucleos-corrida");
    let out = py(&fx, BODY_E4_NUCLEOS_CORRIDA, &[], Some(fx.root.as_path()));
    assert_ok(&out, "run_end with cores and the new fields");
    assert_eq!(history_lines(&text(&out.stderr)), 0, "{}", show(&out));
    let muestra = json!({"t": 1.5, "current": 7, "anon": 4, "file": 2, "shmem": 1, "kernel": 1, "muestras": 5});
    for name in ["corridas.jsonl", "indice.jsonl"] {
        let lines = records(&fx.root, name);
        let run = |id: &str| -> &Value {
            lines
                .iter()
                .find(|l| l["run"] == json!(id))
                .unwrap_or_else(|| panic!("{name} has no run {id}: {lines:?}"))
        };
        let (r1, r2, r3) = (run("r1"), run("r2"), run("r3"));
        assert_eq!(r1["cpu_s"], json!(4.5), "{name}: {r1}");
        assert_eq!(
            r1["nucleos"],
            json!(0.5),
            "{name}: the cores of a run are its CPU over its T (9 s), not over the seconds its steps add up to: {r1}"
        );
        assert_eq!(r1["mem_muestra"], muestra, "{name}: {r1}");
        assert_eq!(r1["serie_final_s"], json!(4.0), "{name}: {r1}");
        assert_eq!(r1["serie_cola_s"], json!(2.0), "{name}: {r1}");
        assert_eq!(r1["sin_medida"], json!({"mem_peak": "m"}), "{name}: {r1}");
        assert!(
            r2["cpu_s"].is_null() && r2["nucleos"].is_null(),
            "{name}: {r2}"
        );
        assert_eq!(
            r2["sin_medida"],
            json!({"cpu_s": "step b: x", "nucleos": "step b: x"}),
            "{name}: the cores of a run that is missing a step take the reason of its cpu_s: {r2}"
        );
        assert_eq!(r3["nucleos"], json!(0.5), "{name}: {r3}");
        for campo in ["mem_muestra", "serie_final_s", "serie_cola_s"] {
            assert!(
                r3.get(campo).is_some_and(Value::is_null),
                "{name}: a run of the old shape has `{campo}` as a key and null: {r3}"
            );
        }
        assert_eq!(r3["sin_medida"], json!({}), "{name}: {r3}");
    }
}
