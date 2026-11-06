//! Contract for `scripts/gate-dag.py`, the lane planner of the local judge (CI-F3, F3.2).
//!
//! Every step is fake: a shell script that prints a banner, sleeps the duration the table
//! names and exits with the code the test writes to `control/rc-<id>` (`kill` makes it kill
//! itself with SIGKILL, like a process the kernel killed). The cgroup tree is fake
//! (`GATE_CGROUP_ROOT`, `GATE_PROC_SELF_CGROUP`, D-J5a) and the history lives in a temporary
//! directory (`CI_TIEMPOS_DIR`, D-F3r). Nothing here touches the real judge, the network or HOME.
//!
//! What the planner has to do for these tests (the pass 2 implements it):
//! - `python3 -I scripts/gate-dag.py run --table <file>`; `GATE_DAG=0` is the series, `GATE_DAG=1` the DAG.
//! - An order that starts with `NAME=value` tokens asks for those variables for that step (D-F3j).
//! - stdout holds each step's block in idx order; `dag: …` and `note: …` lines; `FAIL` lines on
//!   stdout or stderr; `no verdict` on stderr when the judge gives no verdict (D-F3f).
//! - `pasos.jsonl` (D-F3s), one record per step and run, with `id`, `idx`, `inicio` and `fin`
//!   (seconds from the start of the run), `rc`, `parado`, `espera_s`, `cpu_s`. `corridas.jsonl`
//!   with `T`, `LB_ref` and `LB_run`.
//! - Exit code: 0 when green; the code of the red step with the lowest idx (D-F3d); 2 when there
//!   is no verdict (signal, OOM, a variable outside the closed list).
//! - A step runs in parallel only if its table has a serial run in the history (D-F3p). These
//!   tests seed that history with `GATE_DAG=0` on the same table before measuring.
//! - A scope without `cgroup.subtree_control` is not delegated: the planner runs in series (D-F3h).

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static SEQ: AtomicUsize = AtomicUsize::new(0);

const GIB: u64 = 1 << 30;
const MEM_OPEN: &str = "max";
/// memory.max of 14 GiB: R = 14 GiB minus the 4 GiB reserve = 10 GiB (D-J5c, D-F3p).
const MEM_R10: &str = "15032385536";
const CPU_FOUR: &str = "400000 100000";
const CPU_ONE: &str = "100000 100000";
/// One CPU-second per step, so a step of 0.3 s asks for more CPU than a 1-CPU scope has.
const CPU_SECOND: u64 = 1_000_000;
const ANON_A: u64 = 4_800_000_000;
const ANON_BC: u64 = 4 * GIB;
const ALLOWED: &[&str] = &[
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_JOBS",
    "TMPDIR",
    "DATABASE_URL",
    "GATE_SERVER_SYSTEM_ID",
    "CUBA_EMBED_INTRA_THREADS",
    "CUBA_NLI_INTRA_THREADS",
    "CUBA_RERANK_INTRA_THREADS",
    "MUTANTS_JOBS",
    "CUBA_GATE_LANE_TOKEN",
];

const PASO_SH: &str = r#"#!/bin/sh
# Fake step: $1 id, $2 seconds, $3 control directory.
id="$1"
control="$3"
env > "$control/env-$id.txt"
echo "=== $id banner"
sleep "$2"
echo "Running unittests fake $id"
rc=$(cat "$control/rc-$id" 2>/dev/null || echo 0)
if [ "$rc" = "kill" ]; then
    kill -9 $$
fi
if [ "$rc" = "0" ]; then
    echo "test result: ok. 1 passed; 0 failed"
else
    echo "test result: FAILED. 0 passed; 1 failed"
fi
exit "$rc"
"#;

/// (idx, id, deps, seconds, leading NAME=value tokens of the order)
type Row<'a> = (u32, &'a str, &'a str, &'a str, &'a str);

struct Fixture {
    base: PathBuf,
    script: PathBuf,
    control: PathBuf,
    cgroups: PathBuf,
    proc_self: PathBuf,
    history: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    table: PathBuf,
    steps: Vec<(u32, String)>,
}

struct Run {
    code: Option<i32>,
    timed_out: bool,
    out: String,
    err: String,
}

impl Run {
    fn text(&self) -> String {
        format!("{}\n{}", self.out, self.err)
    }
}

impl Fixture {
    fn new(name: &str, cpu_max: &str, mem_max: &str) -> Fixture {
        let base = scratch(name);
        let script = base.join("paso.sh");
        let control = base.join("control");
        let cgroups = base.join("cgroups");
        let scope = cgroups.join("scope");
        let history = base.join("historial");
        let home = base.join("home");
        let tmp = base.join("tmp");
        let table = base.join("tabla.txt");
        let proc_self = base.join("proc-self-cgroup");
        for dir in [&control, &scope, &history, &home, &tmp] {
            fs::create_dir_all(dir).expect("fixture dir");
        }
        write(&script, PASO_SH);
        write(&cgroups.join("cpu.max"), "max 100000");
        write(&cgroups.join("memory.max"), "max");
        write(&cgroups.join("memory.current"), "0");
        write(&cgroups.join("memory.stat"), "anon 0\ninactive_file 0\n");
        write(&scope.join("cpu.max"), cpu_max);
        write(&scope.join("memory.max"), mem_max);
        write(&scope.join("memory.current"), "0");
        write(&scope.join("memory.stat"), "anon 0\ninactive_file 0\n");
        write(&scope.join("cgroup.subtree_control"), "");
        write(&proc_self, "0::/scope\n");
        Fixture {
            base,
            script,
            control,
            cgroups,
            proc_self,
            history,
            home,
            tmp,
            table,
            steps: Vec::new(),
        }
    }

    fn table(&mut self, rows: &[Row]) {
        let mut text = String::new();
        for &(idx, id, deps, secs, pre) in rows {
            let mut line = vec![
                idx.to_string(),
                id.to_string(),
                deps.to_string(),
                "-".to_string(),
                "normal".to_string(),
                "1".to_string(),
                "5".to_string(),
                secs.to_string(),
            ];
            if !pre.is_empty() {
                line.push(pre.to_string());
            }
            line.extend([
                "/bin/sh".to_string(),
                self.script.display().to_string(),
                id.to_string(),
                secs.to_string(),
                self.control.display().to_string(),
            ]);
            text.push_str(&line.join(" "));
            text.push('\n');
            self.steps.push((idx, id.to_string()));
        }
        write(&self.table, &text);
        for (_, id) in &self.steps {
            self.write_defaults(id);
        }
    }

    fn write_defaults(&self, id: &str) {
        self.write_step(id, "memory.stat", "anon 1048576\ninactive_file 0\n");
        self.write_step(id, "cpu.stat", "usage_usec 200000\n");
        self.write_step(id, "memory.events", "oom 0\noom_kill 0\n");
        for pressure in ["cpu.pressure", "memory.pressure", "io.pressure"] {
            self.write_step(
                id,
                pressure,
                "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n",
            );
        }
    }

    fn step_dir(&self, id: &str) -> PathBuf {
        let idx = self
            .steps
            .iter()
            .find(|(_, step)| step.as_str() == id)
            .map(|(idx, _)| *idx)
            .unwrap_or_else(|| panic!("step {id} is not in the table"));
        self.cgroups.join("scope").join(format!("paso-{idx}-{id}"))
    }

    fn write_step(&self, id: &str, file: &str, body: &str) {
        let dir = self.step_dir(id);
        fs::create_dir_all(&dir).expect("step cgroup dir");
        write(&dir.join(file), body);
    }

    fn anon(&self, id: &str, bytes: u64) {
        self.write_step(
            id,
            "memory.stat",
            &format!("anon {bytes}\ninactive_file 0\n"),
        );
    }

    fn usage(&self, id: &str, usec: u64) {
        self.write_step(id, "cpu.stat", &format!("usage_usec {usec}\n"));
    }

    fn oom(&self, id: &str) {
        self.write_step(id, "memory.events", "oom 1\noom_kill 1\n");
    }

    fn rc(&self, id: &str, code: &str) {
        write(&self.control.join(format!("rc-{id}")), code);
    }

    fn undelegate(&self) {
        fs::remove_file(self.cgroups.join("scope").join("cgroup.subtree_control"))
            .expect("subtree_control");
    }

    fn run(&self, gate_dag: &str) -> Run {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let out_path = self.base.join(format!("salida-{n}.txt"));
        let err_path = self.base.join(format!("error-{n}.txt"));
        let mut child = Command::new("python3")
            .arg("-I")
            .arg(repo_root().join("scripts/gate-dag.py"))
            .arg("run")
            .arg("--table")
            .arg(&self.table)
            .env("GATE_DAG", gate_dag)
            .env("CI_TIEMPOS_DIR", &self.history)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("TMPDIR", &self.tmp)
            .env("GATE_CGROUP_ROOT", &self.cgroups)
            .env("GATE_PROC_SELF_CGROUP", &self.proc_self)
            .stdout(Stdio::from(File::create(&out_path).expect("stdout file")))
            .stderr(Stdio::from(File::create(&err_path).expect("stderr file")))
            .spawn()
            .expect("python3 -I scripts/gate-dag.py");
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut timed_out = false;
        let code = loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                break status.code();
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        Run {
            code,
            timed_out,
            out: read(&out_path),
            err: read(&err_path),
        }
    }

    /// Runs the table once in series with every step green, so the planner has a serial
    /// history of this exact table (D-F3p). Later runs use the same table text.
    fn seed(&self) {
        let run = self.run("0");
        assert!(
            !run.timed_out && run.code == Some(0),
            "seeding run in series: exit {:?}, want 0\n{}{}",
            run.code,
            run.out,
            run.err
        );
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "gate-dag-{name}-{}-{}",
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

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
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

/// Byte length of every `name` file under `dir` right now, so that a later read sees only
/// the lines the next run appended.
fn snapshot(dir: &Path, name: &str) -> HashMap<PathBuf, usize> {
    files_named(dir, name)
        .into_iter()
        .map(|file| {
            let len = fs::read(&file).map(|b| b.len()).unwrap_or(0);
            (file, len)
        })
        .collect()
}

fn appended(dir: &Path, name: &str, before: &HashMap<PathBuf, usize>) -> Vec<Value> {
    let mut records = Vec::new();
    for file in files_named(dir, name) {
        let bytes = fs::read(&file).unwrap_or_default();
        let start = before.get(&file).copied().unwrap_or(0).min(bytes.len());
        let tail = String::from_utf8_lossy(&bytes[start..]).to_string();
        for line in tail.lines().filter(|l| !l.trim().is_empty()) {
            let record: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{name} is not JSON lines: {e}: {line}"));
            records.push(record);
        }
    }
    records
}

fn by_id<'a>(records: &'a [Value], id: &str) -> &'a Value {
    records
        .iter()
        .find(|r| r["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("no pasos.jsonl record for {id}"))
}

fn num(record: &Value, key: &str) -> f64 {
    record[key]
        .as_f64()
        .unwrap_or_else(|| panic!("{key} missing in {record}"))
}

fn overlaps(a: &Value, b: &Value) -> bool {
    num(a, "inicio") < num(b, "fin") && num(b, "inicio") < num(a, "fin")
}

fn demand(id: &str) -> f64 {
    let anon = if id == "a" { ANON_A } else { ANON_BC };
    anon as f64 * 1.25
}

fn assert_green(run: &Run, label: &str) {
    assert!(!run.timed_out, "{label}: did not finish in 30 s");
    assert_eq!(
        run.code,
        Some(0),
        "{label}: exit {:?}, want 0\n{}{}",
        run.code,
        run.out,
        run.err
    );
}

/// The number that follows `key` in `text`, digits and dots only.
fn after(text: &str, key: &str) -> String {
    text.split(key)
        .nth(1)
        .unwrap_or("")
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect()
}

/// Spec: un paso solo arranca cuando sus dependencias salieron 0 (D-F3a, D-F3p).
#[test]
fn a_step_starts_only_after_its_dependencies_exit_0() {
    let mut fx = Fixture::new("deps", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.5", ""), (20, "b", "a", "0.2", "")]);
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    let a = by_id(&pasos, "a");
    let b = by_id(&pasos, "b");
    assert!(
        num(b, "inicio") >= num(a, "fin"),
        "B started before A ended: B.inicio={} A.fin={}",
        num(b, "inicio"),
        num(a, "fin")
    );
}

/// Spec: el log unido sale en orden de idx aunque terminen al revés (D-F3e).
#[test]
fn the_joined_log_is_in_idx_order_even_when_steps_finish_out_of_order() {
    let mut fx = Fixture::new("orden-log", CPU_FOUR, MEM_OPEN);
    fx.table(&[
        (10, "s10", "-", "0.6", ""),
        (20, "s20", "-", "0.3", ""),
        (30, "s30", "-", "0.1", ""),
    ]);
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    assert!(
        num(by_id(&pasos, "s30"), "fin") < num(by_id(&pasos, "s10"), "fin"),
        "s30 did not finish before s10: the scenario proves nothing about the order of the log"
    );
    let pos = |id: &str| {
        run.out
            .find(&format!("=== {id} banner"))
            .unwrap_or_else(|| panic!("no banner for {id} in stdout"))
    };
    let (p10, p20, p30) = (pos("s10"), pos("s20"), pos("s30"));
    assert!(
        p10 < p20 && p20 < p30,
        "block order is not idx order: s10={p10} s20={p20} s30={p30}"
    );
    let block = &run.out[p10..p20];
    assert!(
        block.contains("Running unittests fake s10") && block.contains("test result"),
        "block 10 is cut before block 20 starts: {block:?}"
    );
    assert!(
        !block.contains("fake s20"),
        "block 20 is inside block 10: {block:?}"
    );
}

/// Spec: el rojo informado es el del orden en serie (D-F3d). Esquema con tres filas.
#[test]
fn the_red_reported_is_the_one_the_series_would_report() {
    // (dur s20, rc s20, dur s30, rc s30, want exit, red step, s30 must be stopped)
    let cases = [
        ("0.5", "1", "1.0", "0", 1, "s20", true),
        ("1.0", "0", "0.1", "101", 101, "s30", false),
        ("0.8", "1", "0.1", "101", 1, "s20", false),
    ];
    for (d20, rc20, d30, rc30, want, red, stop_30) in cases {
        let mut fx = Fixture::new("rojo-serie", CPU_FOUR, MEM_OPEN);
        fx.table(&[
            (10, "s10", "-", "0.4", ""),
            (20, "s20", "-", d20, ""),
            (30, "s30", "-", d30, ""),
        ]);
        fx.seed();
        fx.rc("s20", rc20);
        fx.rc("s30", rc30);
        let before = snapshot(&fx.history, "pasos.jsonl");
        let run = fx.run("1");
        let label = format!("red {red}");
        assert_eq!(
            run.code,
            Some(want),
            "{label}: exit {:?}, want {want}",
            run.code
        );
        let fails: Vec<String> = run
            .text()
            .lines()
            .filter(|l| l.starts_with("FAIL"))
            .map(str::to_string)
            .collect();
        assert!(
            fails.iter().any(|l| l.contains(red)),
            "{label}: the FAIL line does not name {red}: {fails:?}"
        );
        for other in ["s10", "s20", "s30"].into_iter().filter(|s| *s != red) {
            assert!(
                !fails.iter().any(|l| l.contains(other)),
                "{label}: reported {other}, want {red}: {fails:?}"
            );
        }
        let pasos = appended(&fx.history, "pasos.jsonl", &before);
        let red_idx: u64 = if red == "s20" { 20 } else { 30 };
        for record in pasos.iter().filter(|r| r["idx"].as_u64() < Some(red_idx)) {
            assert!(
                record["parado"] != Value::Bool(true),
                "{label}: a step before the red one was stopped: {record}"
            );
        }
        if stop_30 {
            assert_eq!(
                by_id(&pasos, "s30")["parado"],
                Value::Bool(true),
                "{label}: s30 was running after s20 failed and should be stopped (D-F3d)"
            );
        }
    }
}

/// Spec: un proceso muerto por el kernel no da veredicto, OOM (D-F3f).
#[test]
fn an_oom_kill_gives_no_verdict() {
    let mut fx = Fixture::new("oom", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "s10", "-", "0.3", ""), (20, "s20", "-", "0.3", "")]);
    fx.seed();
    fx.oom("s20");
    let run = fx.run("1");
    assert_eq!(
        run.code,
        Some(2),
        "exit {:?}, want 2 (no verdict)",
        run.code
    );
    assert!(
        run.err.contains("no verdict") && run.err.contains("s20"),
        "stderr must say no verdict and name s20: {}",
        run.err
    );
}

/// Spec: un proceso que sale por señal que no mandó el planificador no da veredicto (D-F3f).
#[test]
fn a_signal_the_planner_did_not_send_gives_no_verdict() {
    let mut fx = Fixture::new("senal", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "s10", "-", "0.3", ""), (20, "s20", "-", "0.3", "")]);
    fx.seed();
    fx.rc("s20", "kill");
    let run = fx.run("1");
    assert_eq!(
        run.code,
        Some(2),
        "exit {:?}, want 2 (no verdict)",
        run.code
    );
    assert!(
        run.err.contains("no verdict") && run.err.contains("s20"),
        "stderr must say no verdict and name s20: {}",
        run.err
    );
}

/// Spec: admisión por RAM recalculada en cada evento (D-F3p). R = 10 GiB.
#[test]
fn admission_never_goes_over_the_ram_cap() {
    let mut fx = Fixture::new("ram", CPU_FOUR, MEM_R10);
    fx.table(&[
        (10, "a", "-", "0.8", ""),
        (20, "b", "-", "0.6", ""),
        (30, "c", "-", "0.6", ""),
    ]);
    fx.anon("a", ANON_A);
    fx.anon("b", ANON_BC);
    fx.anon("c", ANON_BC);
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    let r_cap = 10.0 * GIB as f64;
    for i in &pasos {
        let start = num(i, "inicio");
        let alive: Vec<&Value> = pasos
            .iter()
            .filter(|j| num(j, "inicio") <= start && start < num(j, "fin"))
            .collect();
        let sum: f64 = alive
            .iter()
            .map(|j| demand(j["id"].as_str().unwrap_or("")))
            .sum();
        assert!(
            sum <= r_cap + 1.0,
            "ram over R: {} steps alive at t={start} with r sum {sum} > R {r_cap}",
            alive.len()
        );
    }
    let a = by_id(&pasos, "a");
    let b = by_id(&pasos, "b");
    let c = by_id(&pasos, "c");
    assert!(
        num(b, "inicio") - num(a, "fin") < 1.0,
        "B waited for the tick: B.inicio={} A.fin={}",
        num(b, "inicio"),
        num(a, "fin")
    );
    for waiting in [b, c] {
        assert!(
            num(waiting, "espera_s") > 0.0 && waiting.to_string().contains("ram"),
            "a step that waited for RAM must say espera_s > 0 with motivo ram: {waiting}"
        );
    }
}

/// Spec: un paso que no cabe ni en una máquina vacía corre solo (D-F3p). Sin bloqueo.
#[test]
fn a_step_bigger_than_the_machine_runs_alone() {
    let mut fx = Fixture::new("grande", CPU_FOUR, MEM_R10);
    fx.table(&[
        (10, "chico", "-", "0.2", ""),
        (20, "grande", "-", "0.3", ""),
    ]);
    fx.anon("grande", 20 * GIB);
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert!(
        !run.timed_out,
        "deadlock: the DAG did not finish in 30 s with a step that does not fit"
    );
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    assert!(
        !overlaps(by_id(&pasos, "grande"), by_id(&pasos, "chico")),
        "the step that does not fit shared the machine with another step"
    );
}

/// Spec: prioridad por camino crítico (D-F3o).
#[test]
fn the_critical_path_goes_before_a_smaller_idx() {
    let mut fx = Fixture::new("prioridad", CPU_ONE, MEM_OPEN);
    fx.table(&[
        (10, "y", "-", "0.3", ""),
        (20, "x", "-", "0.5", ""),
        (30, "z", "x", "0.5", ""),
    ]);
    for id in ["y", "x", "z"] {
        fx.usage(id, CPU_SECOND);
    }
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    let x = by_id(&pasos, "x");
    let y = by_id(&pasos, "y");
    assert!(
        num(x, "inicio") < num(y, "inicio"),
        "Y before X: X.inicio={} Y.inicio={} (rank X = 1.0 s, rank Y = 0.3 s)",
        num(x, "inicio"),
        num(y, "inicio")
    );
}

/// Spec: sin historial no se paraleliza (D-F3p).
#[test]
fn without_serial_history_nothing_runs_in_parallel() {
    let fx = {
        let mut fx = Fixture::new("sin-historial", CPU_FOUR, MEM_OPEN);
        fx.table(&[(10, "p", "-", "0.4", ""), (20, "q", "-", "0.4", "")]);
        fx
    };
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    assert!(
        run.text()
            .contains("note: no serial history for this table"),
        "the planner must say why it runs in series: {}",
        run.text()
    );
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    assert!(
        !overlaps(by_id(&pasos, "p"), by_id(&pasos, "q")),
        "two steps alive without history"
    );
}

/// Spec: sin delegación de cgroups, en serie (D-F3h).
#[test]
fn an_undelegated_scope_runs_in_series() {
    let mut fx = Fixture::new("sin-delegar", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "p", "-", "0.4", ""), (20, "q", "-", "0.4", "")]);
    fx.seed();
    fx.undelegate();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    assert!(
        run.text().contains("note: the scope is not delegated"),
        "the planner must say the scope is not delegated: {}",
        run.text()
    );
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    assert!(
        !overlaps(by_id(&pasos, "p"), by_id(&pasos, "q")),
        "the planner ran in parallel without being able to measure"
    );
}

/// Spec: el entorno de un paso es el de la serie más la lista cerrada (D-F3j).
#[test]
fn a_step_environment_differs_only_by_the_closed_list() {
    let mut fx = Fixture::new("entorno", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "e", "-", "0.2", "")]);
    fx.seed();
    let run = fx.run("1");
    assert_green(&run, "DAG");
    assert!(
        run.out
            .lines()
            .any(|l| l.starts_with("dag: e env:") && l.contains("TMPDIR") && !l.contains('=')),
        "the planner must print `dag: e env:` with the names that changed and no values: {}",
        run.out
    );
    let known: HashSet<String> = std::env::vars()
        .map(|(name, _)| name)
        .chain(
            [
                "GATE_DAG",
                "CI_TIEMPOS_DIR",
                "HOME",
                "XDG_CACHE_HOME",
                "TMPDIR",
                "GATE_CGROUP_ROOT",
                "GATE_PROC_SELF_CGROUP",
                "PWD",
                "OLDPWD",
                "SHLVL",
                "_",
            ]
            .map(|s| s.to_string()),
        )
        .collect();
    let env = read(&fx.control.join("env-e.txt"));
    let unexpected: Vec<&str> = env
        .lines()
        .filter_map(|l| l.split_once('=').map(|(name, _)| name))
        .filter(|name| !known.contains(*name) && !ALLOWED.contains(name))
        .collect();
    assert!(
        unexpected.is_empty(),
        "names outside the closed list reached the step: {unexpected:?}"
    );
}

/// Spec: una variable fuera de la lista da exit 2 nombrándola (D-F3j).
#[test]
fn a_variable_outside_the_closed_list_is_refused() {
    let fx = {
        let mut fx = Fixture::new("fuera-lista", CPU_FOUR, MEM_OPEN);
        fx.table(&[(10, "f", "-", "0.2", "FOO=1")]);
        fx
    };
    let run = fx.run("1");
    assert_eq!(
        run.code,
        Some(2),
        "exit {:?}, want 2 naming FOO (FOO is not in the closed list)",
        run.code
    );
    assert!(
        run.text().contains("FOO"),
        "the refusal must name FOO: {}",
        run.text()
    );
    assert!(
        !run.out.contains("=== f banner"),
        "the step started although its environment was refused"
    );
}

/// Spec: el resumen da la brecha T/LB (D-F3t).
#[test]
fn the_summary_reports_the_gap_between_t_and_lb() {
    let mut fx = Fixture::new("brecha", CPU_FOUR, MEM_OPEN);
    fx.table(&[
        (10, "a", "-", "1.0", ""),
        (20, "b", "a", "1.0", ""),
        (30, "c", "-", "1.5", ""),
    ]);
    fx.seed();
    let before = snapshot(&fx.history, "corridas.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let line = run
        .out
        .lines()
        .rev()
        .find(|l| l.contains("LB_run="))
        .unwrap_or_else(|| panic!("no LB_run in the summary: {}", run.out))
        .to_string();
    let lb: f64 = after(&line, "LB_run=")
        .parse()
        .unwrap_or_else(|_| panic!("LB_run is not a number: {line}"));
    assert!(
        (1.8..=2.5).contains(&lb),
        "LB_run {lb}, want about 2.0 (the chain a -> b); a sum of durations would give 3.5"
    );
    let ratio = after(&line, "T/LB_run=");
    let decimals = ratio.split('.').nth(1).map_or(0, |d| d.len());
    assert_eq!(decimals, 2, "T/LB_run must have two decimals: {line}");
    let corridas = appended(&fx.history, "corridas.jsonl", &before);
    assert!(
        corridas.iter().any(|r| r.get("T").is_some()
            && r.get("LB_ref").is_some()
            && r.get("LB_run").is_some()),
        "corridas.jsonl must keep T, LB_ref and LB_run: {corridas:?}"
    );
}

/// Spec: con GATE_DAG=0 corre exactamente lo mismo que la serie (D-F3x, D-F3e).
#[test]
fn gate_dag_zero_runs_exactly_the_series() {
    let mut fx = Fixture::new("serie", CPU_FOUR, MEM_OPEN);
    fx.table(&[
        (10, "s10", "-", "0.3", ""),
        (20, "s20", "-", "0.3", ""),
        (30, "s30", "-", "0.3", ""),
    ]);
    fx.seed();
    let serie = fx.run("0");
    assert_green(&serie, "GATE_DAG=0");
    let dag = fx.run("1");
    assert_green(&dag, "GATE_DAG=1");
    let blocks = |text: &str| -> String {
        text.lines()
            .filter(|l| !l.starts_with("dag:") && !l.starts_with("note:"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        blocks(&serie.out),
        blocks(&dag.out),
        "GATE_DAG=0 and GATE_DAG=1 print different blocks"
    );
    for id in ["s10", "s20", "s30"] {
        assert_eq!(
            serie.out.matches(&format!("=== {id} banner")).count(),
            1,
            "{id} did not run exactly once in series"
        );
    }
}
