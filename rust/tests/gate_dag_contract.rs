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
//! - A step starts with SIGINT and SIGQUIT at their default, even when the planner was started with them
//!   ignored, as a background job of a non-interactive bash leaves them (CI-J10).

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
        self.run_env(gate_dag, &[])
    }

    /// `run` with more variables for the planner (CI-F3.3b-1b-i: the gate lock and its owner).
    fn run_env(&self, gate_dag: &str, extra: &[(&str, &str)]) -> Run {
        self.run_planner_at(&repo_root().join("scripts/gate-dag.py"), gate_dag, extra)
    }

    /// `run_env` with the planner of another tree (CI-F3.3c, D-F3g): each tree names its own targets.
    fn run_planner_at(&self, planner: &Path, gate_dag: &str, extra: &[(&str, &str)]) -> Run {
        let mut command = Command::new("python3");
        command
            .arg("-I")
            .arg(planner)
            .arg("run")
            .arg("--table")
            .arg(&self.table);
        self.run_command(command, gate_dag, extra)
    }

    /// The planner started the way merge-gate.sh starts it (CI-J10): a background job of a non-interactive bash,
    /// which gives its children SIGINT and SIGQUIT ignored on entry. `sh` sets the same two ignores before its exec.
    fn run_planner_ignoring_int(&self, gate_dag: &str, extra: &[(&str, &str)]) -> Run {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("trap '' INT QUIT; exec python3 -I \"$0\" run --table \"$1\"")
            .arg(repo_root().join("scripts/gate-dag.py"))
            .arg(&self.table);
        self.run_command(command, gate_dag, extra)
    }

    fn run_command(&self, mut command: Command, gate_dag: &str, extra: &[(&str, &str)]) -> Run {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let out_path = self.base.join(format!("salida-{n}.txt"));
        let err_path = self.base.join(format!("error-{n}.txt"));
        let mut child = command
            .env("GATE_DAG", gate_dag)
            .env("CI_TIEMPOS_DIR", &self.history)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("TMPDIR", &self.tmp)
            .envs(extra.iter().copied())
            .env("GATE_CGROUP_ROOT", &self.cgroups)
            .env("GATE_PROC_SELF_CGROUP", &self.proc_self)
            .stdout(Stdio::from(File::create(&out_path).expect("stdout file")))
            .stderr(Stdio::from(File::create(&err_path).expect("stderr file")))
            .spawn()
            .expect("the planner");
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

/// Rows that name their class (CI-F3.3c): (idx, id, class, seconds), with no resources and no environment.
fn table_with_class(fx: &mut Fixture, rows: &[(u32, &str, &str, &str)]) {
    let mut text = String::new();
    for &(idx, id, class, secs) in rows {
        text.push_str(&format!(
            "{idx} {id} - - {class} 1 5 {secs} /bin/sh {} {id} {secs} {}\n",
            fx.script.display(),
            fx.control.display()
        ));
        fx.steps.push((idx, id.to_string()));
    }
    write(&fx.table, &text);
    for (_, id) in &fx.steps {
        fx.write_defaults(id);
    }
}

/// Spec (CI-F3.3c, D-F3p): la CPU admitida no pasa de la CPU de la máquina, C, y no de 1,25·C. Tres pasos de unos 1,6
/// núcleos medidos sobre una máquina de 4 CPU: dos caben (3,2) y el tercero (4,8) no. Con la cota de 1,25·C (5) entraría.
#[test]
fn admission_never_goes_over_the_cpu_of_the_machine() {
    let mut fx = Fixture::new("cpu", CPU_FOUR, MEM_OPEN);
    fx.table(&[
        (10, "a", "-", "0.6", ""),
        (20, "b", "-", "0.6", ""),
        (30, "c", "-", "0.6", ""),
    ]);
    for id in ["a", "b", "c"] {
        fx.usage(id, CPU_SECOND);
    }
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    for i in &pasos {
        let start = num(i, "inicio");
        let cores: f64 = pasos
            .iter()
            .filter(|j| num(j, "inicio") <= start && start < num(j, "fin"))
            .map(|j| num(j, "cpu_s") / (num(j, "fin") - num(j, "inicio")))
            .sum();
        assert!(
            cores <= 4.0 + 0.25,
            "CPU over the machine at t={start}: {cores:.2} cores alive, the machine has 4"
        );
    }
}

/// Spec (CI-F3.3c, D-F3l): un paso exclusive no comparte tiempo con ningún otro, aunque esté listo antes que ellos.
/// Guarda: el planificador ya lo cumplía antes de F3.3c; el contrato de la tabla es lo que la fila NLI pide.
#[test]
fn an_exclusive_step_shares_no_time_with_another_step() {
    let mut fx = Fixture::new("exclusive", CPU_FOUR, MEM_OPEN);
    table_with_class(
        &mut fx,
        &[
            (10, "a", "normal", "0.5"),
            (20, "x", "exclusive", "0.5"),
            (30, "b", "normal", "0.5"),
            (40, "c", "normal", "0.5"),
        ],
    );
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    let x = by_id(&pasos, "x");
    for other in pasos.iter().filter(|j| j["id"] != "x") {
        assert!(
            !overlaps(x, other),
            "exclusive x overlaps {}: x=[{}, {}) other=[{}, {})",
            other["id"],
            num(x, "inicio"),
            num(x, "fin"),
            num(other, "inicio"),
            num(other, "fin")
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

/// CI-F3.3b-2 (spec F3.3b-1b-iii): with `--timed-end`, the planner prints the boundary line once, before the block of
/// the first row of class `untimed`, and after the blocks of the timed rows before it. merge-gate.sh judges the test
/// times up to that line, so the output of the untimed rows never reaches the judge.
#[test]
fn the_timed_end_line_is_printed_once_before_the_first_untimed_block() {
    let mut fx = Fixture::new("marca-no-cronometrada", CPU_FOUR, MEM_OPEN);
    fx.table(&[
        (10, "s10", "-", "0.1", ""),
        (20, "s20", "-", "0.1", ""),
        (30, "s30", "-", "0.1", ""),
    ]);
    // s20 and s30 are the untimed rows: their class is `untimed` in the table.
    let text: String = read(&fx.table)
        .lines()
        .map(|line| {
            if line.starts_with("20 s20 ") || line.starts_with("30 s30 ") {
                line.replacen(" normal ", " untimed ", 1)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    write(&fx.table, &text);
    let output = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("scripts/gate-dag.py"))
        .arg("run")
        .arg("--table")
        .arg(&fx.table)
        .arg("--timed-end")
        .arg("=== end of timed steps ===")
        .env("GATE_DAG", "0")
        .env("CI_TIEMPOS_DIR", &fx.history)
        .env("HOME", &fx.home)
        .env("XDG_CACHE_HOME", fx.home.join(".cache"))
        .env("TMPDIR", &fx.tmp)
        .env("GATE_CGROUP_ROOT", &fx.cgroups)
        .env("GATE_PROC_SELF_CGROUP", &fx.proc_self)
        .output()
        .expect("python3 -I scripts/gate-dag.py");
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "the planner exited {:?}: {out}{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let at = |needle: &str| {
        out.lines()
            .position(|line| line == needle)
            .unwrap_or_else(|| panic!("no line `{needle}` in the planner's stdout: {out}"))
    };
    let marks = out
        .lines()
        .filter(|line| *line == "=== end of timed steps ===")
        .count();
    assert_eq!(
        marks, 1,
        "the boundary line must be printed once, found {marks}: {out}"
    );
    let (p10, mark, p20) = (
        at("=== s10 banner"),
        at("=== end of timed steps ==="),
        at("=== s20 banner"),
    );
    assert!(
        p10 < mark && mark < p20,
        "the boundary (line {mark}) must sit between the timed block of s10 ({p10}) and the first untimed block of s20 ({p20})"
    );
}

/// CI-F3.3b-2 (D-F3e): a block reaches the pipe when its step ends, before the next step starts. Without the flush
/// the blocks of a series sit in python's buffer until the planner exits, and a gate cut between two steps shows
/// none of the steps it had already finished.
#[test]
fn a_finished_block_is_written_before_the_next_step_starts() {
    let mut fx = Fixture::new("flush-bloque", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "s10", "-", "0.1", ""), (20, "s20", "-", "4", "")]);
    let out_path = fx.base.join("salida-flush.txt");
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("scripts/gate-dag.py"))
        .arg("run")
        .arg("--table")
        .arg(&fx.table)
        .env("GATE_DAG", "0")
        .env("CI_TIEMPOS_DIR", &fx.history)
        .env("HOME", &fx.home)
        .env("XDG_CACHE_HOME", fx.home.join(".cache"))
        .env("TMPDIR", &fx.tmp)
        .env("GATE_CGROUP_ROOT", &fx.cgroups)
        .env("GATE_PROC_SELF_CGROUP", &fx.proc_self)
        .stdout(Stdio::from(File::create(&out_path).expect("stdout file")))
        .stderr(Stdio::null())
        .spawn()
        .expect("python3 -I scripts/gate-dag.py");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if read(&out_path).contains("=== s10 banner") {
            break;
        }
        if let Some(status) = child.try_wait().expect("try_wait") {
            panic!(
                "the planner ended (exit {:?}) before the block of s10 reached its stdout: {}",
                status.code(),
                read(&out_path)
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("the block of s10 never reached stdout: {}", read(&out_path));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        child.try_wait().expect("try_wait").is_none(),
        "the block of s10 reached stdout only when the planner had already ended, so it sat in the buffer \
         and a gate cut between two steps would have shown none of it"
    );
    let _ = child.wait();
}

// CI-F3.3b-1b-i (D-F3k): a step the planner starts under the gate's lock gets a one-use ticket in the
// lock's lanes directory, named <idx>-<id> and holding the token it passes in CUBA_GATE_LANE_TOKEN. The
// lane spends that ticket to inherit the lock (tests/gate_lane_token_contract.rs). A planner run with no
// lock writes nothing and passes no token, as before.
#[test]
fn a_step_under_the_gate_lock_gets_a_one_use_ticket_in_the_lock() {
    let mut fx = Fixture::new("lane-ticket", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "P", "-", "0", "")]);
    let lock = fx.base.join("lock");
    let owner = "pid=1 start=1 where=test since=test token=test";
    let run = fx.run_env(
        "0",
        &[
            ("GATE_LOCK", lock.to_str().expect("utf-8 path")),
            ("CUBA_GATE_LOCK_OWNER", owner),
        ],
    );
    assert_green(&run, "a step under the gate lock");
    let env = read(&fx.control.join("env-P.txt"));
    let token = env
        .lines()
        .find_map(|line| line.strip_prefix("CUBA_GATE_LANE_TOKEN="))
        .unwrap_or_else(|| panic!("the step got no CUBA_GATE_LANE_TOKEN: {env}"));
    assert!(!token.is_empty(), "the lane token is empty");
    let ticket = read(&lock.join("lanes").join("10-P"));
    assert_eq!(
        ticket, token,
        "the ticket in the lock's lanes directory must hold the token the step got"
    );
}

// --- CI-F3.3c (D-F3g): the target of a row is its own directory, per tree and per name ----------------------

/// A table whose rows name their resources (the `recursos` column of D-F3a), which `table` leaves at `-`.
/// Rows are (idx, id, resources, seconds); each is a step of its own, with no dependencies.
fn table_with_resources(fx: &mut Fixture, rows: &[(u32, &str, &str, &str)]) {
    let mut text = String::new();
    for &(idx, id, resources, secs) in rows {
        text.push_str(&format!(
            "{idx} {id} - {resources} normal 1 5 {secs} /bin/sh {} {id} {secs} {}\n",
            fx.script.display(),
            fx.control.display()
        ));
        fx.steps.push((idx, id.to_string()));
    }
    write(&fx.table, &text);
    for (_, id) in &fx.steps {
        fx.write_defaults(id);
    }
}

/// The CARGO_TARGET_DIR a step saw, from the environment PASO_SH dumped into its control directory.
fn target_seen_by(fx: &Fixture, id: &str) -> Option<String> {
    read(&fx.control.join(format!("env-{id}.txt")))
        .lines()
        .find_map(|line| line.strip_prefix("CARGO_TARGET_DIR=").map(str::to_string))
}

/// Spec D-F3g: a step that names a target builds in <CARGO_TARGET_DIR>/<tree key>/<target>, and the directory of the
/// target writes the realpath of its tree in .gate-tree. The root is the gate's own CARGO_TARGET_DIR.
#[test]
fn a_step_with_a_target_builds_in_its_own_directory_of_its_tree() {
    let mut fx = Fixture::new("d3g-propio", CPU_FOUR, MEM_OPEN);
    let root = fx.base.join("target-root");
    table_with_resources(&mut fx, &[(10, "DBG", "target:T_dbg", "1")]);
    let run = fx.run_env(
        "0",
        &[("CARGO_TARGET_DIR", root.to_str().expect("utf-8 path"))],
    );
    assert_green(&run, "a step that names a target");
    let seen = PathBuf::from(target_seen_by(&fx, "DBG").expect("the step got no CARGO_TARGET_DIR"));
    assert_eq!(
        seen.file_name().and_then(|name| name.to_str()),
        Some("T_dbg"),
        "the step must build in the directory of its target, not in the gate's root: {}",
        seen.display()
    );
    assert_eq!(
        seen.parent().and_then(Path::parent),
        Some(root.as_path()),
        "the tree key sits directly under the gate's CARGO_TARGET_DIR: {}",
        seen.display()
    );
    let named = read(&seen.join(".gate-tree"));
    assert_eq!(
        named.trim(),
        fs::canonicalize(repo_root())
            .expect("repo root")
            .display()
            .to_string(),
        "the target does not name the tree it belongs to: {named}"
    );
}

/// Spec C4 (D-F3g): two trees with the same directory name and one target root never share a target, and a target
/// that another tree marked is refused with 2 before cargo is called: the step does not start.
#[test]
fn two_trees_under_one_target_root_never_share_a_target() {
    let mut fx = Fixture::new("d3g-dos-arboles", CPU_FOUR, MEM_OPEN);
    let root = fx.base.join("target-root");
    table_with_resources(&mut fx, &[(10, "DBG", "target:T_dbg", "1")]);
    let env = [("CARGO_TARGET_DIR", root.to_str().expect("utf-8 path"))];
    let mut trees = Vec::new();
    for name in ["a", "b"] {
        let scripts = fx.base.join(name).join("memory-industry").join("scripts");
        fs::create_dir_all(&scripts).expect("tree");
        for file in ["gate-dag.py", "gate_history.py", "gate-lock.sh"] {
            fs::copy(repo_root().join("scripts").join(file), scripts.join(file))
                .expect("copy of the planner");
        }
        trees.push(scripts.parent().expect("tree root").to_path_buf());
    }
    let mut targets = Vec::new();
    for tree in &trees {
        let _ = fs::remove_file(fx.control.join("env-DBG.txt"));
        let run = fx.run_planner_at(&tree.join("scripts/gate-dag.py"), "0", &env);
        assert_green(&run, "a tree of its own, with a target root");
        targets.push(PathBuf::from(
            target_seen_by(&fx, "DBG").expect("the step got no CARGO_TARGET_DIR"),
        ));
    }
    assert_ne!(
        targets[0],
        targets[1],
        "two trees, one target: both trees build in {}",
        targets[0].display()
    );
    for (tree, target) in trees.iter().zip(&targets) {
        assert_eq!(
            read(&target.join(".gate-tree")).trim(),
            fs::canonicalize(tree).expect("tree").display().to_string(),
            "the target of {} does not name it",
            tree.display()
        );
    }
    let _ = fs::remove_file(fx.control.join("env-DBG.txt"));
    write(
        &targets[0].join(".gate-tree"),
        &format!("{}\n", fs::canonicalize(&trees[1]).expect("tree").display()),
    );
    let run = fx.run_planner_at(&trees[0].join("scripts/gate-dag.py"), "0", &env);
    assert_eq!(
        run.code,
        Some(2),
        "a target that another tree marked must be refused with 2 before cargo: {}",
        run.text()
    );
    assert!(
        !fx.control.join("env-DBG.txt").exists(),
        "the step started although its target belongs to another tree"
    );
}

/// The planner of D-F3g changes CARGO_TARGET_DIR only for a step that names a target: a step without one keeps the
/// gate's own value, as it did before.
#[test]
fn a_step_without_a_target_keeps_the_gates_cargo_target() {
    let mut fx = Fixture::new("d3g-sin-target", CPU_FOUR, MEM_OPEN);
    let root = fx.base.join("target-root");
    table_with_resources(&mut fx, &[(10, "P", "-", "1")]);
    let run = fx.run_env(
        "0",
        &[("CARGO_TARGET_DIR", root.to_str().expect("utf-8 path"))],
    );
    assert_green(&run, "a step without a target");
    assert_eq!(
        target_seen_by(&fx, "P").as_deref(),
        root.to_str(),
        "a step with no target got another CARGO_TARGET_DIR"
    );
}

/// A step that names a target with no CARGO_TARGET_DIR to root it under is refused before the step starts (D-F3g).
#[test]
fn a_target_without_a_cargo_target_root_is_refused_before_the_step() {
    let mut fx = Fixture::new("d3g-sin-raiz", CPU_FOUR, MEM_OPEN);
    table_with_resources(&mut fx, &[(10, "DBG", "target:T_dbg", "1")]);
    let run = fx.run_env("0", &[("CARGO_TARGET_DIR", "")]);
    assert_eq!(
        run.code,
        Some(2),
        "a target with no root must be refused with 2: {}",
        run.text()
    );
    assert!(
        !fx.control.join("env-DBG.txt").exists(),
        "the step started although it has no root for its target"
    );
}

/// D-F3g, the judge's names: a CARGO_TARGET_DIR that cannot be made (here, a file where a directory has to be) is
/// refused with 2 and a line that names it, never a traceback. The judge's environment can hand over a root that is
/// not one this user can write.
#[test]
fn a_target_root_that_cannot_be_made_is_refused_with_2_and_named() {
    let mut fx = Fixture::new("d3g-raiz-no-creable", CPU_FOUR, MEM_OPEN);
    let blocker = fx.base.join("blocker");
    write(&blocker, "a file, not a directory\n");
    let root = blocker.join("target");
    table_with_resources(&mut fx, &[(10, "DBG", "target:T_dbg", "1")]);
    let run = fx.run_env(
        "0",
        &[("CARGO_TARGET_DIR", root.to_str().expect("utf-8 path"))],
    );
    assert_eq!(
        run.code,
        Some(2),
        "a target root that cannot be made must exit 2: {}",
        run.text()
    );
    assert!(
        run.err.contains("cannot be made") && !run.err.contains("Traceback"),
        "the refusal must name the root and not be a traceback: {}",
        run.err
    );
    assert!(
        !fx.control.join("env-DBG.txt").exists(),
        "the step started although its target root cannot be made"
    );
}

// --- CI-J10: the steps the planner starts take SIGINT and SIGQUIT at their default --------------------------

/// SIGINT is signal 2, bit 1 of the SigIgn mask of /proc/<pid>/status; SIGQUIT is signal 3, bit 2.
const INT_AND_QUIT: u64 = (1 << 1) | (1 << 2);

/// The mask of a `SigIgn:` line of /proc/<pid>/status.
fn sig_ignored(line: &str) -> u64 {
    let hex = line
        .split_whitespace()
        .last()
        .unwrap_or_else(|| panic!("no SigIgn in {line:?}"));
    u64::from_str_radix(hex, 16).unwrap_or_else(|e| panic!("SigIgn {hex:?} is not hex: {e}"))
}

/// A one-step table (CI-J10): the step is `/bin/sh <script> <id> <args…> <control>`, with its defaults written.
fn one_step_table(fx: &mut Fixture, script: &Path, id: &str, args: &[&str]) {
    let mut order = vec![
        "/bin/sh".to_string(),
        script.display().to_string(),
        id.to_string(),
    ];
    order.extend(args.iter().map(|arg| arg.to_string()));
    order.push(fx.control.display().to_string());
    write(
        &fx.table,
        &format!("10 {id} - - normal 1 5 1 {}\n", order.join(" ")),
    );
    fx.steps.push((10, id.to_string()));
    fx.write_defaults(id);
}

/// The step records its own SigIgn and the one of its parent, the planner. $1 id, $2 control directory.
const SIGIGN_SH: &str = r#"#!/bin/sh
id="$1"
control="$2"
grep '^SigIgn' /proc/$$/status > "$control/sigign-$id.txt"
grep '^SigIgn' /proc/$PPID/status > "$control/sigign-parent-$id.txt"
"#;

/// The step of the D2 row seen through the planner: it runs the gate in the foreground, as cargo test runs the
/// gate copy, and sends that gate INT while it waits. $1 id, $2 the gate, $3 control directory.
const STEP_GATE_SH: &str = r#"#!/bin/sh
id="$1"
gate="$2"
control="$3"
( sleep 1; kill -INT "$(cat "$control/gate-$id.pid")" ) &
sh "$gate" "$control" "$id"
echo "$?" > "$control/gate-rc-$id.txt"
"#;

/// The gate: a trap that exits 2 on INT, then a wait. Without that INT it reports `survived` and exits 0 (D2).
/// $1 control directory, $2 id.
const GATE_SH: &str = r#"#!/bin/sh
control="$1"
id="$2"
trap 'echo trapped > "$control/gate-$id.txt"; exit 2' INT
echo "$$" > "$control/gate-$id.pid"
sleep 3 &
wait $!
echo survived > "$control/gate-$id.txt"
exit 0
"#;

#[test]
fn a_step_the_planner_starts_has_sigint_and_sigquit_at_their_default() {
    let mut fx = Fixture::new("j10-sigign", CPU_FOUR, MEM_OPEN);
    let step = fx.base.join("paso-sigign.sh");
    write(&step, SIGIGN_SH);
    one_step_table(&mut fx, &step, "P", &[]);
    let run = fx.run_planner_ignoring_int("0", &[]);
    assert_green(&run, "a planner started with INT and QUIT ignored");
    let parent = sig_ignored(&read(&fx.control.join("sigign-parent-P.txt")));
    let step_mask = sig_ignored(&read(&fx.control.join("sigign-P.txt")));
    assert_eq!(
        parent & INT_AND_QUIT,
        INT_AND_QUIT,
        "the precondition is not met: the planner does not ignore INT and QUIT (SigIgn {parent:#x})"
    );
    assert_eq!(
        step_mask & INT_AND_QUIT,
        0,
        "the step has INT or QUIT ignored (SigIgn {step_mask:#x}, its planner's {parent:#x}): \
         a trap of its gate cannot stop on INT"
    );
}

#[test]
fn a_gate_under_a_planner_step_stops_on_int_through_its_trap() {
    let mut fx = Fixture::new("j10-d2", CPU_FOUR, MEM_OPEN);
    let step = fx.base.join("paso-gate.sh");
    let gate = fx.base.join("gate.sh");
    write(&step, STEP_GATE_SH);
    write(&gate, GATE_SH);
    one_step_table(&mut fx, &step, "P", &[gate.to_str().expect("utf-8 path")]);
    let run = fx.run_planner_ignoring_int("0", &[]);
    assert_green(
        &run,
        "a planner started with INT and QUIT ignored, whose step runs a gate",
    );
    let trapped = read(&fx.control.join("gate-P.txt"));
    assert_eq!(
        trapped.trim(),
        "trapped",
        "the gate did not take INT through its trap (it wrote {trapped:?}): a D2 row would leave 0"
    );
    let rc = read(&fx.control.join("gate-rc-P.txt"));
    assert_eq!(
        rc.trim(),
        "2",
        "a gate stopped by INT must exit 2, not {rc:?}"
    );
}

// --- CI-F3.4: the resources of a row, the memory of the models and the plan the binary chose -------------------------
// D-F34b..F34f, D-F34o. Every step is fake, as above. The planner reads the plan from the log of the row (stdout and
// stderr of the step are one file), the memory from GATE_PROC_MEMINFO and the cgroup tree from GATE_CGROUP_ROOT.

/// The three pools of the serial judge (D-F3m, D-F34a): what the rows with models name.
const POOLS: &str =
    "CUBA_EMBED_INTRA_THREADS=4 CUBA_NLI_INTRA_THREADS=4 CUBA_RERANK_INTRA_THREADS=8";

/// The step of the row where the plan decides: a banner, what the test left in control/plan-<id>.txt (the lines the
/// production binary writes to stderr, which the planner finds in the log of the step) and exit 0.
/// $1 id, $2 seconds, $3 control directory.
const PLAN_SH: &str = r#"#!/bin/sh
id="$1"
control="$3"
echo "=== $id banner"
cat "$control/plan-$id.txt" 2>/dev/null
echo "Running unittests fake $id"
echo "test result: ok. 1 passed; 0 failed"
exit 0
"#;

/// A row of `table_of`: the fields of gate-dag.txt that these contracts vary, and the script that is its order.
struct StepRow<'a> {
    idx: u32,
    id: &'a str,
    deps: &'a str,
    resources: &'a str,
    class: &'a str,
    secs: &'a str,
    env: &'a str,
    script: PathBuf,
}

impl Fixture {
    fn row<'a>(&self, idx: u32, id: &'a str, resources: &'a str, secs: &'a str) -> StepRow<'a> {
        StepRow {
            idx,
            id,
            deps: "-",
            resources,
            class: "normal",
            secs,
            env: "",
            script: self.script.clone(),
        }
    }

    /// A /proc/meminfo of its own (GATE_PROC_MEMINFO, D-F34d), with the fields of the real file around the one the
    /// planner reads.
    fn meminfo(&self, available_kib: u64) -> PathBuf {
        let path = self.base.join("meminfo");
        write(
            &path,
            &format!(
                "MemTotal:       49327104 kB\nMemFree:         1048576 kB\nMemAvailable:   {available_kib} kB\n\
                 Buffers:           65536 kB\nCached:          2097152 kB\nSwapTotal:       8388608 kB\n"
            ),
        );
        path
    }

    /// The `resource plan` lines the step of `id` writes into its log.
    fn plan(&self, id: &str, text: &str) {
        write(&self.control.join(format!("plan-{id}.txt")), text);
    }

    fn started(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.control)
            .expect("control dir")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("env-"))
            })
            .collect()
    }
}

/// Writes the table of `rows`, each `idx id deps resources class 1 5 secs [env] /bin/sh <script> id secs control`.
fn table_of(fx: &mut Fixture, rows: &[StepRow]) {
    let mut text = String::new();
    for row in rows {
        let mut line = vec![
            row.idx.to_string(),
            row.id.to_string(),
            row.deps.to_string(),
            row.resources.to_string(),
            row.class.to_string(),
            "1".to_string(),
            "5".to_string(),
            row.secs.to_string(),
        ];
        if !row.env.is_empty() {
            line.push(row.env.to_string());
        }
        line.extend([
            "/bin/sh".to_string(),
            row.script.display().to_string(),
            row.id.to_string(),
            row.secs.to_string(),
            fx.control.display().to_string(),
        ]);
        text.push_str(&line.join(" "));
        text.push('\n');
        fx.steps.push((row.idx, row.id.to_string()));
    }
    write(&fx.table, &text);
    for (_, id) in &fx.steps {
        fx.write_defaults(id);
    }
}

fn target_root(fx: &Fixture) -> String {
    fx.base
        .join("target-root")
        .to_str()
        .expect("utf-8 path")
        .to_string()
}

/// Spec P1 (D-F34f): the table is judged before anything starts. Two rows in one cluster, two rows that build in one
/// target with no order between them, a reader with no row that builds its target above it, and a resource that is
/// none of target:, cluster:, models or reader: each is exit 2, the line names it, and no step started. The target
/// root is set, so a table that is not judged would start its steps (and not be refused for lack of a root).
#[test]
fn a_table_that_breaks_a_resource_rule_is_refused_before_any_step_starts() {
    let cases = [
        (
            "two rows in one cluster",
            [
                ("a", "-", "target:T_a,cluster:C_x"),
                ("b", "-", "target:T_b,cluster:C_x"),
            ],
            "C_x",
        ),
        (
            "two rows that build in one target with no order between them",
            [("a", "-", "target:T_x"), ("b", "-", "target:T_x")],
            "T_x",
        ),
        (
            "a reader of a target no row builds above it",
            [("p", "-", "target:T_p"), ("r", "-", "target:T_x,reader")],
            "T_x",
        ),
        (
            "a resource of a kind the planner does not know",
            [("a", "-", "foo:bar"), ("b", "-", "-")],
            "foo:bar",
        ),
    ];
    for (defect, rows, named) in cases {
        let mut fx = Fixture::new("f34-tabla", CPU_FOUR, MEM_OPEN);
        let table: Vec<StepRow> = rows
            .into_iter()
            .zip([10, 20])
            .map(|((id, deps, resources), idx)| StepRow {
                deps,
                ..fx.row(idx, id, resources, "0.1")
            })
            .collect();
        table_of(&mut fx, &table);
        let root = target_root(&fx);
        let run = fx.run_env("0", &[("CARGO_TARGET_DIR", root.as_str())]);
        assert_eq!(
            run.code,
            Some(2),
            "{defect}: exit {:?}, want 2\n{}{}",
            run.code,
            run.out,
            run.err
        );
        assert!(
            run.err.contains(named),
            "{defect}: the refusal does not name {named}: {}",
            run.err
        );
        assert!(
            fx.started().is_empty(),
            "{defect}: a step started although the table is refused: {:?}",
            fx.started()
        );
    }
}

/// The counterpart of P1: the shapes the real table has are not refused. A row that builds its target and two readers
/// of it that wait for it, each with its own cluster, and the row of the models without a target at all.
#[test]
fn a_table_whose_resources_are_in_order_runs() {
    let mut fx = Fixture::new("f34-tabla-buena", CPU_FOUR, MEM_OPEN);
    let rows = [
        StepRow {
            resources: "target:T_x,cluster:C_p",
            ..fx.row(10, "p", "", "0.1")
        },
        StepRow {
            deps: "p",
            resources: "target:T_x,cluster:C_r1,models,reader",
            env: POOLS,
            ..fx.row(20, "r1", "", "0.1")
        },
        StepRow {
            deps: "p",
            resources: "target:T_x,cluster:C_r2,models,reader",
            env: POOLS,
            ..fx.row(30, "r2", "", "0.1")
        },
    ];
    table_of(&mut fx, &rows);
    let root = target_root(&fx);
    let run = fx.run_env("0", &[("CARGO_TARGET_DIR", root.as_str())]);
    assert_green(&run, "a table whose resources are in order");
}

/// Spec P2 (D-F34e): as many as two rows with models alive. Three rows with models and the three pools, ready at once,
/// with CPU and RAM to spare and a MemAvailable that is plenty: the third waits for one of the others, and says why.
#[test]
fn no_more_than_two_rows_with_models_are_alive_at_once() {
    let mut fx = Fixture::new("f34-models", CPU_FOUR, MEM_OPEN);
    let rows = ["m1", "m2", "m3"]
        .into_iter()
        .zip([10, 20, 30])
        .map(|(id, idx)| StepRow {
            env: POOLS,
            ..fx.row(idx, id, "models", "1")
        })
        .collect::<Vec<_>>();
    table_of(&mut fx, &rows);
    let plenty = fx.meminfo(64 * 1024 * 1024);
    let env = [("GATE_PROC_MEMINFO", plenty.to_str().expect("utf-8 path"))];
    assert_green(&fx.run_env("0", &env), "the series that seeds the history");
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run_env("1", &env);
    assert_green(&run, "DAG");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    for record in &pasos {
        let start = num(record, "inicio");
        let alive = pasos
            .iter()
            .filter(|other| num(other, "inicio") <= start && start < num(other, "fin"))
            .count();
        assert!(
            alive <= 2,
            "three steps with models alive at t={start}: {alive} of the rows with models were running"
        );
    }
    let third = by_id(&pasos, "m3");
    assert!(
        num(third, "espera_s") > 0.0 && third["motivo"].as_str() == Some("models"),
        "the third row with models must wait with espera_s > 0 and motivo models: {third}"
    );
}

/// Spec P3 (D-F34d): with another step alive, a row with models starts only if MemAvailable is at least 4096 MiB plus
/// its own estimate. With 2 GiB it waits for the step that is alive. The control: with memory to spare, the same
/// table starts both at once, so the wait of the first run is the memory's and nothing else's.
#[test]
fn a_row_with_models_waits_for_memory_when_another_step_is_alive() {
    let mut fx = Fixture::new("f34-memoria", CPU_FOUR, MEM_OPEN);
    let rows = [
        fx.row(10, "A", "-", "2"),
        StepRow {
            env: POOLS,
            ..fx.row(20, "B", "models", "1")
        },
    ];
    table_of(&mut fx, &rows);
    fx.anon("B", GIB);
    let scarce = fx.meminfo(2 * 1024 * 1024);
    let scarce_env = [("GATE_PROC_MEMINFO", scarce.to_str().expect("utf-8 path"))];
    assert_green(
        &fx.run_env("0", &scarce_env),
        "the series that seeds the history",
    );
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run_env("1", &scarce_env);
    assert_green(&run, "DAG with 2 GiB available");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    let (a, b) = (by_id(&pasos, "A"), by_id(&pasos, "B"));
    assert!(
        num(b, "inicio") >= num(a, "fin"),
        "B started with MemAvailable under 4096 MiB + r: B.inicio={} A.fin={}",
        num(b, "inicio"),
        num(a, "fin")
    );
    assert!(
        num(b, "espera_s") > 0.0 && b["motivo"].as_str() == Some("ram"),
        "B must wait with espera_s > 0 and motivo ram: {b}"
    );
    let plenty = fx.meminfo(64 * 1024 * 1024);
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run_env(
        "1",
        &[("GATE_PROC_MEMINFO", plenty.to_str().expect("utf-8 path"))],
    );
    assert_green(&run, "DAG with memory to spare");
    let pasos = appended(&fx.history, "pasos.jsonl", &before);
    assert!(
        overlaps(by_id(&pasos, "A"), by_id(&pasos, "B")),
        "with memory to spare B must start beside A, or the wait above was not the memory's"
    );
}

/// Spec P4 (D-F34d): the condition is for a row that has someone beside it. A single row with models, with 2 GiB
/// available, runs and the planner ends, in the DAG and in the series.
#[test]
fn a_row_with_models_that_has_nobody_beside_it_starts_with_little_memory() {
    let mut fx = Fixture::new("f34-sin-bloqueo", CPU_FOUR, MEM_OPEN);
    let rows = [StepRow {
        env: POOLS,
        ..fx.row(10, "B", "models", "0.3")
    }];
    table_of(&mut fx, &rows);
    fx.anon("B", GIB);
    let scarce = fx.meminfo(2 * 1024 * 1024);
    let env = [("GATE_PROC_MEMINFO", scarce.to_str().expect("utf-8 path"))];
    for mode in ["0", "1"] {
        let run = fx.run_env(mode, &env);
        assert!(
            !run.timed_out && run.code == Some(0),
            "deadlock: GATE_DAG={mode}, a lone row with models never ran with 2 GiB available (exit {:?}, timed out {})\n{}",
            run.code,
            run.timed_out,
            run.text()
        );
    }
}

/// The production line of the plan, as tracing writes it (the nested form of the 19th run), for a tier, a reranker on
/// the GPU or not, and the three pools.
fn plan_line(tier: &str, gpu: bool, embed: u32, rerank: u32, nli: u32) -> String {
    let gpu = if gpu { " (GPU)" } else { "" };
    format!(
        "{{\"timestamp\":\"2026-11-06T16:10:54.000112Z\",\"level\":\"INFO\",\"fields\":{{\"message\":\"resource plan\",\
         \"ram_total_mb\":48171,\"ram_available_mb\":27785,\"cgroup_limit_mb\":36864,\"swap_total_mb\":8192,\
         \"cores_logical\":24,\"cores_physical\":12,\"plan\":\"nivel={tier} (embebedor + reranker + NLI) · \
         presupuesto=27017 MiB · reserva=3320 MiB · modelos: embebedor=true reranker=true{gpu} nli=true · \
         hilos: embed={embed} rerank={rerank} nli={nli} · tokio: workers=4 blocking=16 · pool=4 · chunk=4 · \
         ood_fit=5000\"}}}}\n"
    )
}

/// A table with a row of models that names the pools (4, 4 and 8) and prints nothing, and the row where the plan
/// decides: models and no pools (E2E in the real table), whose step prints `lines`.
fn plan_table(fx: &mut Fixture, lines: &str) {
    let plan_script = fx.base.join("plan.sh");
    write(&plan_script, PLAN_SH);
    let rows = [
        StepRow {
            env: POOLS,
            ..fx.row(10, "d", "models", "0.1")
        },
        StepRow {
            script: plan_script,
            ..fx.row(20, "e", "models", "0.1")
        },
    ];
    table_of(fx, &rows);
    fx.plan("e", lines);
}

/// The text of the block of `e` after its banner: from the line `=== e banner` to the verdict the planner prints.
fn block_of_e(run: &Run) -> String {
    let start = run
        .out
        .find("=== e banner")
        .unwrap_or_else(|| panic!("no block for e in stdout:\n{}", run.text()));
    let end = run.out[start..]
        .find("FAIL: e exited")
        .map_or(run.out.len(), |at| start + at);
    run.out[start..end].to_string()
}

/// Spec P5 (D-F34b): a plan under the full tier is red, with the row's own line at the end of its block. The judge
/// says it proves the three models, and a plan that set one aside is a judge that proves less. Both shapes of the
/// line: the nested one tracing writes and the flat one the spec names.
#[test]
fn a_plan_below_the_full_tier_is_red() {
    let flat = "{\"message\":\"resource plan\",\"plan\":\"nivel=standard (embebedor + reranker) · presupuesto=9000 MiB · \
                modelos: embebedor=true reranker=true nli=false · hilos: embed=4 rerank=8 nli=4 · pool=4\"}\n";
    for (shape, lines) in [
        ("nested", plan_line("standard", false, 4, 8, 4)),
        ("flat", flat.to_string()),
    ] {
        let mut fx = Fixture::new("f34-nivel", CPU_FOUR, MEM_OPEN);
        plan_table(&mut fx, &lines);
        let run = fx.run("0");
        assert!(!run.timed_out, "{shape}: did not finish in 30 s");
        assert_eq!(
            run.code,
            Some(1),
            "{shape}: exit {:?}, want 1\n{}{}",
            run.code,
            run.out,
            run.err
        );
        let block = block_of_e(&run);
        assert!(
            block
                .trim_end()
                .ends_with("FAIL: e: resource plan tier standard, the judge needs full"),
            "{shape}: the block of e does not end in the FAIL line for the tier: {block:?}"
        );
    }
}

/// Spec P6 (D-F34b): the pools of the plan are the table's, except the reranker's when the plan puts it on the GPU
/// (there the intra-op pool of the CPU does not do the work, and comparing it would turn the judge of a machine with
/// a card red for nothing). The table's rows with models name 4, 4 and 8.
#[test]
fn the_threads_of_the_plan_are_the_tables_except_the_reranker_on_a_gpu() {
    // (gpu, embed, rerank, nli, exit, the line the block carries)
    let cases = [
        (false, 4, 8, 4, 0, ""),
        (
            false,
            4,
            2,
            4,
            1,
            "resource plan rerank=2, the rows with models set 8",
        ),
        (true, 4, 2, 4, 0, ""),
        (
            false,
            2,
            8,
            4,
            1,
            "resource plan embed=2, the rows with models set 4",
        ),
    ];
    for (n, (gpu, embed, rerank, nli, want, text)) in cases.into_iter().enumerate() {
        let mut fx = Fixture::new("f34-hilos", CPU_FOUR, MEM_OPEN);
        plan_table(&mut fx, &plan_line("full", gpu, embed, rerank, nli));
        let run = fx.run("0");
        let label = format!(
            "case {} (gpu {gpu}, embed={embed} rerank={rerank} nli={nli})",
            n + 1
        );
        assert!(!run.timed_out, "{label}: did not finish in 30 s");
        assert_eq!(
            run.code,
            Some(want),
            "{label}: exit {:?}, want {want}\n{}{}",
            run.code,
            run.out,
            run.err
        );
        if want == 1 {
            let block = block_of_e(&run);
            assert!(
                block.contains(&format!("FAIL: e: {text}")),
                "{label}: the block does not say {text:?}: {block:?}"
            );
        }
    }
}

/// Spec P7 (D-F34c): the row where the plan decides has to show it, and a plan line that is not JSON is never skipped.
/// A plan line is one that starts with `{` and says `"message":"resource plan"`, as tracing writes it; one that is cut
/// halfway (two writers on one stderr) is the unreadable case. A line of prose that only mentions the resource plan is
/// not a plan line: the logs of the tests that print their assertions are full of them.
#[test]
fn the_row_where_the_plan_decides_has_to_show_it() {
    let mut fx = Fixture::new("f34-ancla", CPU_FOUR, MEM_OPEN);
    plan_table(&mut fx, "");
    let run = fx.run("0");
    assert!(!run.timed_out, "no line: did not finish in 30 s");
    assert_eq!(
        run.code,
        Some(1),
        "a row that shows no plan: exit {:?}, want 1\n{}{}",
        run.code,
        run.out,
        run.err
    );
    assert!(
        run.text().contains("no resource plan line"),
        "the verdict does not say no resource plan line: {}",
        run.text()
    );

    let mut fx = Fixture::new("f34-ancla-ilegible", CPU_FOUR, MEM_OPEN);
    let whole = plan_line("full", false, 4, 8, 4);
    let cut = &whole[..whole.find("hilos:").expect("the plan has its threads")];
    plan_table(&mut fx, &format!("{cut}\n"));
    let run = fx.run("0");
    assert!(!run.timed_out, "unreadable line: did not finish in 30 s");
    assert_eq!(
        run.code,
        Some(1),
        "an unreadable plan line: exit {:?}, want 1\n{}{}",
        run.code,
        run.out,
        run.err
    );
    assert!(
        run.text().contains("unreadable resource plan line") && run.text().contains(cut),
        "the verdict does not name the unreadable line: {}",
        run.text()
    );

    let mut fx = Fixture::new("f34-ancla-prosa", CPU_FOUR, MEM_OPEN);
    let prose =
        "note: the resource plan above is the one the binary chose, a line that says so in prose\n";
    plan_table(
        &mut fx,
        &format!("{}{prose}", plan_line("full", false, 4, 8, 4)),
    );
    assert_green(&fx.run("0"), "a plan line and a line of prose about it");
}

/// Spec P8 (D-F34o): while a step is exclusive no other step's cgroup has processes. A step `a` that ended and left
/// its cgroup with `populated 1` (a daemon that outlived it; the rmdir of the planner fails in silence) is no verdict
/// for the exclusive step after it, and with `populated 0` the same table is green. The scope is delegated, as the
/// real one is under pesado.
#[test]
fn an_exclusive_step_does_not_start_beside_a_cgroup_that_still_has_processes() {
    let mut fx = Fixture::new("f34-populated", CPU_FOUR, MEM_OPEN);
    let rows = [
        fx.row(10, "a", "-", "0.3"),
        StepRow {
            deps: "a",
            class: "exclusive",
            ..fx.row(20, "x", "-", "0.3")
        },
    ];
    table_of(&mut fx, &rows);
    fx.seed();
    fx.write_step("a", "cgroup.events", "populated 1\nfrozen 0\n");
    let run = fx.run("1");
    assert!(!run.timed_out, "populated 1: did not finish in 30 s");
    assert_eq!(
        run.code,
        Some(2),
        "populated 1: exit {:?}, want 2\n{}{}",
        run.code,
        run.out,
        run.err
    );
    assert!(
        run.err
            .contains("no verdict: x is exclusive and paso-10-a still had processes"),
        "the verdict does not name the cgroup that still had processes: {}",
        run.err
    );
    fx.write_step("a", "cgroup.events", "populated 0\nfrozen 0\n");
    assert_green(&fx.run("1"), "populated 0");
}
