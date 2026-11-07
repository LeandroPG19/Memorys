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
    let code = format!("{PRE}{body}");
    let mut cmd = Command::new("python3");
    cmd.arg("-I")
        .arg("-c")
        .arg(code)
        .arg(scripts())
        .args(args)
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
        .arg(scripts().join("gate_history.py"))
        .arg("registrar")
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
        .arg(scripts().join("gate_history.py"))
        .arg("registrar")
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
