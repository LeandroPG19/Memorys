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
//! - CI-E4: every record of `pasos.jsonl` and `corridas.jsonl` has `nucleos` and `sin_medida` (the reason of every null of a
//!   field it covers); the run has `mem_muestra`, `serie_final_s` and `serie_cola_s`; stdout has `dag: resources:` after
//!   `dag: T=` and `note: not measured (...)` lines, and a scope that is not delegated is neither measured nor written.

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// CI-K4 (D-K4n): `run_env` leaves GATE_DAG out of the environment, to see what the planner does by default.
const UNSET: &str = "<unset>";

/// What the judge exports to its lanes and to the processes under them, one name each: the 32 that `JUDGE_ENVIRONMENT` in
/// local_gate_contract.rs has besides `CUBA_HANDLER_TIMEOUT_SECS` (33 there since CI-E14; only the row D3 names that one and
/// the judge does not export it) — the names `run_in_lane` of merge-gate.sh and the exports of merge-gate.sh and
/// run-all-tests.sh hand down, which each one of them hands to a self-test as foreign. The contracts of the judge run under that
/// environment (`lane_CT` of run-all-tests.sh runs them with `DATABASE_URL` set), so a planner started from here with what
/// its parent has would read the judge's: `DATABASE_URL` alone (gate-dag.py, J14d) puts every DAG in series. The planner of a
/// test sees only what the test gives it.
const JUDGE_LANE_ENV: [&str; 32] = [
    "DATABASE_URL",
    "GATE_LOCK",
    "GATE_SERVER_SYSTEM_ID",
    "GATE_CLUSTER_ROOT",
    "GATE_LANE",
    "CUBA_GATE_LOCK_OWNER",
    "CUBA_GATE_EXIT_FILE",
    "CARGO_BUILD_JOBS",
    "GATE_CARGO_JOBS_FROM_GATE",
    "CARGO_TARGET_DIR",
    "CUBA_PEER_DATABASE_URL",
    "CUBA_BINARY_PATH",
    "CUBA_JUDGE",
    "ONNX_MODEL_PATH",
    "ORT_DYLIB_PATH",
    "CUBA_RERANKER_PATH",
    "CUBA_NLI_PATH",
    "CUBA_EMBEDDING_DIM",
    "CUBA_EMBED_MODEL",
    "CUBA_POOLING",
    "CUBA_GATE_LANE_TOKEN",
    "CUBA_EMBED_INTRA_THREADS",
    "CUBA_NLI_INTRA_THREADS",
    "CUBA_RERANK_INTRA_THREADS",
    "MUTANTS_JOBS",
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_DEBUG",
    "CARGO_PROFILE_DEV_STRIP",
    "PYTHONUTF8",
    "PYTHONIOENCODING",
    "RUSTUP_TOOLCHAIN",
    "MEMORY_INDUSTRY_REQUIRE_DATABASE_URL",
];

/// `command` without any name of JUDGE_LANE_ENV, whatever the process that runs the test has. Called by every launcher
/// of the planner before its own variables, so what a test gives (`env`, `extra`) is applied after and stays.
fn without_judge_env(command: &mut Command) -> &mut Command {
    for name in JUDGE_LANE_ENV {
        command.env_remove(name);
    }
    command
}

/// `python3` for the planner, with the environment of `without_judge_env`.
fn python3_for_the_planner() -> Command {
    let mut command = Command::new("python3");
    without_judge_env(&mut command);
    command
}

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
        // CI-E7a (D-E7b): R is the RAM free less its reserve, so the planner reads MemAvailable on every run, and a test
        // that does not name a machine gets 64 GiB of its own and not the free memory of whoever runs `cargo test`.
        write(
            &base.join("meminfo-default"),
            "MemTotal:       67108864 kB\nMemFree:        67108864 kB\nMemAvailable:   67108864 kB\n",
        );
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
        if gate_dag == UNSET {
            command.env_remove("GATE_DAG");
        } else {
            command.env("GATE_DAG", gate_dag);
        }
        // CI-K4 (D-K4l): the jobs the planner counts are the ones of the machine the test describes, never the
        // CARGO_BUILD_JOBS of whoever runs `cargo test` (pesado exports one); and so with every name the judge exports to
        // its lanes (J14d: its DATABASE_URL would put the DAG a test asks for in series).
        let mut child = without_judge_env(&mut command)
            .env("CI_TIEMPOS_DIR", &self.history)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("TMPDIR", &self.tmp)
            .env("GATE_PROC_MEMINFO", self.base.join("meminfo-default"))
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

/// The names the planner accepts in the leading `NAME=value` tokens of a step (D-F3j), asked of the planner
/// itself: `ALLOWED_ENV` in gate-dag.py is the one source of the list, and this contract holds no copy of it
/// (CI-J13). The question has a deadline like every process of this file.
fn planner_allowed_env() -> Vec<String> {
    let base = scratch("allowed-env");
    let out_path = base.join("salida.txt");
    let err_path = base.join("error.txt");
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(repo_root().join("scripts/gate-dag.py"))
        .arg("allowed-env")
        .stdout(Stdio::from(File::create(&out_path).expect("stdout file")))
        .stderr(Stdio::from(File::create(&err_path).expect("stderr file")))
        .spawn()
        .expect("python3");
    let deadline = Instant::now() + Duration::from_secs(30);
    let code = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status.code();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`gate-dag.py allowed-env` did not answer in 30 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        code,
        Some(0),
        "`gate-dag.py allowed-env` has to print the closed list of the step environment and exit 0: {}",
        read(&err_path)
    );
    read(&out_path)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
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
                "GATE_PROC_MEMINFO",
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
    let allowed = planner_allowed_env();
    assert!(
        !allowed.is_empty(),
        "the planner named no variable of the closed list: the check below would let every name through"
    );
    let env = read(&fx.control.join("env-e.txt"));
    let unexpected: Vec<&str> = env
        .lines()
        .filter_map(|l| l.split_once('=').map(|(name, _)| name))
        .filter(|name| !known.contains(*name) && !allowed.iter().any(|a| a.as_str() == *name))
        .collect();
    assert!(
        unexpected.is_empty(),
        "names outside the closed list reached the step: {unexpected:?}"
    );
}

/// CI-J13: the closed list of the step environment has one source, `ALLOWED_ENV` in gate-dag.py, and the planner
/// says it (`gate-dag.py allowed-env`, one name per line). A contract that copied it by hand went on passing
/// when the planner added a name.
#[test]
fn the_planner_says_its_own_closed_list_of_step_variables() {
    let names = planner_allowed_env();
    assert!(
        names.len() >= 5,
        "the planner listed {} names: it reads as an empty list and the contracts that use it would pass over \
         nothing: {names:?}",
        names.len()
    );
    for anchor in [
        "CARGO_TARGET_DIR",
        "TMPDIR",
        "CUBA_GATE_LANE_TOKEN",
        "CARGO_INCREMENTAL",
        "CARGO_PROFILE_DEV_DEBUG",
        "CARGO_PROFILE_DEV_STRIP",
    ] {
        assert!(
            names.iter().any(|name| name == anchor),
            "{anchor} is not in the list the planner printed: {names:?}"
        );
    }
    // CI-J18 (D-J18c): the list is what a step may differ in, the union of what a row names and what the planner sets. No
    // row names a database and nobody sets one since F3.2, so neither is in it: a name nobody uses is not listed.
    for gone in ["DATABASE_URL", "GATE_SERVER_SYSTEM_ID"] {
        assert!(
            !names.iter().any(|name| name == gone),
            "{gone} is in the list the planner printed, and no row names it and the planner sets none: {names:?}"
        );
    }
    let unique: HashSet<&String> = names.iter().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "a name is listed twice: {names:?}"
    );
    for name in &names {
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit()),
            "`{name}` is not the name of an environment variable: {names:?}"
        );
    }
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
            .filter(|l| {
                !l.starts_with("dag:") && !l.starts_with("note:") && !l.starts_with("plan:")
            })
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
    let output = python3_for_the_planner()
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
    let mut child = python3_for_the_planner()
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
    fx.table(&[(10, "P", "-", "0.1", "")]);
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
    // The planner ranks by critical path with the durations of the history, so which of the three starts last is not
    // fixed: exactly one row waits for the semaphore, and the other two never name it as their reason.
    let waited: Vec<&str> = ["m1", "m2", "m3"]
        .into_iter()
        .filter(|id| {
            let record = by_id(&pasos, id);
            num(record, "espera_s") > 0.0 && record["motivo"].as_str() == Some("models")
        })
        .collect();
    let named_models = ["m1", "m2", "m3"]
        .into_iter()
        .filter(|id| by_id(&pasos, id)["motivo"].as_str() == Some("models"))
        .count();
    assert!(
        waited.len() == 1 && named_models == 1,
        "exactly one of the three rows with models must wait with espera_s > 0 and motivo models, and the other two \
         must not name models as their reason; got {} that waited and {named_models} that name models: {}",
        waited.len(),
        ["m1", "m2", "m3"]
            .into_iter()
            .map(|id| {
                let r = by_id(&pasos, id);
                format!(
                    "{id}(inicio={}, espera_s={}, motivo={})",
                    r["inicio"], r["espera_s"], r["motivo"]
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
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
/// the GPU or not, and the three pools, on the 24 logical cores of the 19th run.
fn plan_line(tier: &str, gpu: bool, embed: u32, rerank: u32, nli: u32) -> String {
    plan_line_on(Some(24), tier, gpu, embed, rerank, nli)
}

/// `plan_line` for a machine of `cores` logical cores (`cores_logical`, the field tracing writes beside the plan), or,
/// with `None`, for a line that does not carry them.
fn plan_line_on(
    cores: Option<u32>,
    tier: &str,
    gpu: bool,
    embed: u32,
    rerank: u32,
    nli: u32,
) -> String {
    let gpu = if gpu { " (GPU)" } else { "" };
    let cores = cores.map_or(String::new(), |n| format!("\"cores_logical\":{n},"));
    format!(
        "{{\"timestamp\":\"2026-11-06T16:10:54.000112Z\",\"level\":\"INFO\",\"fields\":{{\"message\":\"resource plan\",\
         \"ram_total_mb\":48171,\"ram_available_mb\":27785,\"cgroup_limit_mb\":36864,\"swap_total_mb\":8192,\
         {cores}\"cores_physical\":12,\"plan\":\"nivel={tier} (embebedor + reranker + NLI) · \
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

/// CI-E13 (D-E13a): the verdict does not depend on the CPUs the judge runs with. The plan of production gives each pool
/// half the logical cores, at least 1 and at most the table's number (rust/src/resources.rs: `half_cores.min(MAX_*)`), so
/// a shared turn of 12 CPUs plans rerank=6 and is the same judge as one of 24 that plans rerank=8: the 10-nov run with
/// 12 CPUs went red on `rerank=6, the rows with models set 8` with nothing wrong. What stays red is a plan that is not
/// what its own cores give, up or down; the reranker on a GPU is not compared (P6), and a line with no cores is compared
/// with the table's numbers as it is.
#[test]
fn the_pools_of_the_plan_follow_the_cpus_the_plan_saw() {
    // (cores_logical, gpu, embed, rerank, nli, exit, the line the block carries)
    let cases = [
        (Some(12), false, 4, 6, 4, 0, ""),
        (
            Some(12),
            false,
            4,
            8,
            4,
            1,
            "resource plan rerank=8, the rows with models set 8 and 12 CPUs give 6",
        ),
        (
            Some(12),
            false,
            4,
            4,
            4,
            1,
            "resource plan rerank=4, the rows with models set 8 and 12 CPUs give 6",
        ),
        (Some(6), false, 3, 3, 3, 0, ""),
        (
            Some(6),
            false,
            4,
            3,
            3,
            1,
            "resource plan embed=4, the rows with models set 4 and 6 CPUs give 3",
        ),
        (Some(1), false, 1, 1, 1, 0, ""),
        (Some(12), true, 4, 2, 4, 0, ""),
        (None, false, 4, 8, 4, 0, ""),
        (
            None,
            false,
            4,
            6,
            4,
            1,
            "resource plan rerank=6, the rows with models set 8",
        ),
    ];
    for (n, (cores, gpu, embed, rerank, nli, want, text)) in cases.into_iter().enumerate() {
        let mut fx = Fixture::new("e13-cpus", CPU_FOUR, MEM_OPEN);
        plan_table(
            &mut fx,
            &plan_line_on(cores, "full", gpu, embed, rerank, nli),
        );
        let run = fx.run("0");
        let label = format!(
            "case {} (cores {cores:?}, gpu {gpu}, embed={embed} rerank={rerank} nli={nli})",
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

// --- CI-J14: the planner holds on its first run in parallel -----------------------------------------------------------
// J14a (the mode that ran), J14b and J14g (no step outlives the planner, and a killed row cleans up after itself), J14c
// (the tmpfs pages of a step), J14d (a DATABASE_URL of the operator), J14e (an internal failure is no verdict), J14k and
// J14l (the key and the numbers of the table). The cgroup tree is fake, as above; a step that must be alive when the
// test talks to the planner is found by a duration no other test uses, which is in its command line.

/// A planner that runs in the background: the test signals it while it works and then collects its answer.
struct Live {
    child: std::process::Child,
    out_path: PathBuf,
    err_path: PathBuf,
}

impl Live {
    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("kill");
        assert!(status.success(), "kill -{name} {} failed", self.child.id());
    }

    /// The planner's exit, with a deadline of `secs`: a planner that does not stop is killed and reported as timed out.
    fn collect(mut self, secs: u64) -> Run {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut timed_out = false;
        let code = loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                break status.code();
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = self.child.kill();
                let _ = self.child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        Run {
            code,
            timed_out,
            out: read(&self.out_path),
            err: read(&self.err_path),
        }
    }
}

impl Fixture {
    /// `run_env` without waiting: the planner of this tree, started in the background.
    fn spawn(&self, gate_dag: &str, extra: &[(&str, &str)]) -> Live {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let out_path = self.base.join(format!("salida-{n}.txt"));
        let err_path = self.base.join(format!("error-{n}.txt"));
        let child = python3_for_the_planner()
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
            .env("GATE_PROC_MEMINFO", self.base.join("meminfo-default"))
            .envs(extra.iter().copied())
            .env("GATE_CGROUP_ROOT", &self.cgroups)
            .env("GATE_PROC_SELF_CGROUP", &self.proc_self)
            .stdout(Stdio::from(File::create(&out_path).expect("stdout file")))
            .stderr(Stdio::from(File::create(&err_path).expect("stderr file")))
            .spawn()
            .expect("the planner");
        Live {
            child,
            out_path,
            err_path,
        }
    }

    /// The text of the table as it is on disk, for the tests that rewrite it.
    fn table_text(&self) -> String {
        read(&self.table)
    }

    fn scope(&self) -> PathBuf {
        self.cgroups.join("scope")
    }
}

/// The pids of the processes whose command line holds `token`, this process excluded. A zombie has no command line, so
/// it is not one.
fn processes_with(token: &str) -> Vec<u32> {
    let me = std::process::id();
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != me)
        .filter(|pid| {
            fs::read(format!("/proc/{pid}/cmdline"))
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes)
                        .replace('\0', " ")
                        .contains(token)
                })
                .unwrap_or(false)
        })
        .collect()
}

/// True as soon as `check` holds, false after `secs` seconds.
fn within(secs: u64, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What is left of the processes with `token` after `secs` seconds, and then none: whatever survived is killed so a
/// red test does not leave a sleep of 37 s behind.
fn survivors_of(token: &str, secs: u64) -> Vec<u32> {
    let _ = within(secs, || processes_with(token).is_empty());
    let left = processes_with(token);
    for pid in &left {
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status();
    }
    left
}

/// The two-column rows of `bash_table`: (idx, id, seconds in the table, script).
type BashRow<'a> = (u32, &'a str, &'a str, &'a PathBuf);

/// A table whose steps run under bash, whose EXIT trap runs when TERM stops it (dash does not): `idx id - - normal 1 5
/// secs /bin/bash <script> id secs control`.
fn bash_table(fx: &mut Fixture, rows: &[BashRow]) {
    let mut text = String::new();
    for &(idx, id, secs, script) in rows {
        text.push_str(&format!(
            "{idx} {id} - - normal 1 5 {secs} /bin/bash {} {id} {secs} {}\n",
            script.display(),
            fx.control.display()
        ));
        fx.steps.push((idx, id.to_string()));
    }
    write(&fx.table, &text);
    for (_, id) in &fx.steps {
        fx.write_defaults(id);
    }
}

/// A step that leaves a mark when its shell exits, whichever way. What it sleeps and how it exits come from the
/// control directory (`sleep-<id>`, `rc-<id>`), so the series that seeds the history is short and the DAG run is long.
/// It says it is up (`up-<id>`) once its trap is set, and waits for the step named in `wait-for-<id>` to say it is up, so
/// a red that is only 0.1 s long cannot stop a step that has not set its trap yet, however loaded the machine is.
/// $1 id, $2 seconds of the table, $3 control directory.
const TRAP_SH: &str = r#"#!/bin/bash
id="$1"
control="$3"
trap 'echo exited > "$control/mark-$id"' EXIT
echo "=== $id banner"
echo up > "$control/up-$id"
if [ -f "$control/wait-for-$id" ]; then
    other=$(cat "$control/wait-for-$id")
    for _ in $(seq 400); do [ -e "$control/up-$other" ] && break; sleep 0.05; done
fi
rc=$(cat "$control/rc-$id" 2>/dev/null || echo 0)
secs=$(cat "$control/sleep-$id" 2>/dev/null || echo 0.2)
sleep "$secs" &
wait $!
exit "$rc"
"#;

/// A step that ignores TERM, and so does what it started: only SIGKILL stops it.
const DEAF_SH: &str = r#"#!/bin/bash
id="$1"
control="$3"
trap '' TERM
echo "=== $id banner"
echo up > "$control/up-$id"
secs=$(cat "$control/sleep-$id" 2>/dev/null || echo 0.2)
sleep "$secs" &
wait
"#;

fn records_of(fx: &Fixture, name: &str, before: &HashMap<PathBuf, usize>) -> Vec<Value> {
    appended(&fx.history, name, before)
}

/// J14a: `modo` is what ran. With GATE_DAG=1 and no serial history of this table the planner runs in series, so it
/// records `serie`; the next run of the same table is the first that has history to learn from, and runs as a DAG. A
/// run recorded as `dag` while it ran in series is never a serial run to learn from, so the DAG never started.
#[test]
fn a_run_is_recorded_with_the_mode_it_ran_in() {
    let mut fx = Fixture::new("j14a", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "p", "-", "0.4", ""), (20, "q", "-", "0.4", "")]);
    let before_steps = snapshot(&fx.history, "pasos.jsonl");
    let before_runs = snapshot(&fx.history, "corridas.jsonl");
    let first = fx.run("1");
    assert_green(&first, "GATE_DAG=1 with no history");
    assert!(
        first
            .text()
            .contains("note: no serial history for this table"),
        "the first run has no history and has to say it runs in series: {}",
        first.text()
    );
    let steps = records_of(&fx, "pasos.jsonl", &before_steps);
    let runs = records_of(&fx, "corridas.jsonl", &before_runs);
    assert_eq!(steps.len(), 2, "two steps ran: {steps:?}");
    for record in steps.iter().chain(runs.iter()) {
        assert_eq!(
            record["modo"].as_str(),
            Some("serie"),
            "a run that ran in series is recorded as serie, not by the flag: {record}"
        );
    }
    assert!(
        first.out.contains("mode=serie"),
        "the summary says the mode that ran: {}",
        first.out
    );

    let before_steps = snapshot(&fx.history, "pasos.jsonl");
    let before_runs = snapshot(&fx.history, "corridas.jsonl");
    let second = fx.run("1");
    assert_green(&second, "GATE_DAG=1 after one run in series");
    assert!(
        !second.text().contains("note: no serial history"),
        "the second run of the same table has history and runs as a DAG: {}",
        second.text()
    );
    let steps = records_of(&fx, "pasos.jsonl", &before_steps);
    assert!(
        overlaps(by_id(&steps, "p"), by_id(&steps, "q")),
        "the second run did not run p and q together: the first run was not counted as a serial run"
    );
    for record in steps
        .iter()
        .chain(records_of(&fx, "corridas.jsonl", &before_runs).iter())
    {
        assert_eq!(
            record["modo"].as_str(),
            Some("dag"),
            "a run in parallel is recorded as dag: {record}"
        );
    }
    assert!(second.out.contains("mode=dag"), "{}", second.out);
}

/// J14a: a scope that is not delegated runs in series whatever the flag says, so GATE_DAG=1 there is recorded as
/// `serie`; with the delegation back, the same table already has its serial run and goes in parallel.
#[test]
fn an_undelegated_run_is_recorded_as_series_and_counts_as_history() {
    let mut fx = Fixture::new("j14a-sin-delegar", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "p", "-", "0.4", ""), (20, "q", "-", "0.4", "")]);
    fx.undelegate();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let first = fx.run("1");
    assert_green(&first, "GATE_DAG=1 without delegation");
    assert!(
        first.text().contains("note: the scope is not delegated"),
        "{}",
        first.text()
    );
    for record in records_of(&fx, "pasos.jsonl", &before) {
        assert_eq!(
            record["modo"].as_str(),
            Some("serie"),
            "it ran in series: {record}"
        );
    }
    write(&fx.scope().join("cgroup.subtree_control"), "");
    let before = snapshot(&fx.history, "pasos.jsonl");
    let second = fx.run("1");
    assert_green(&second, "GATE_DAG=1 with the delegation back");
    let steps = records_of(&fx, "pasos.jsonl", &before);
    assert!(
        overlaps(by_id(&steps, "p"), by_id(&steps, "q")),
        "the run in series of the undelegated scope is not history to learn from"
    );
}

/// J14b: SIGHUP (a closed terminal) stops the planner as TERM does, and no step of the table outlives it. The step is
/// a `sleep` of 37.5101 s; the planner ends with 2, and writes `cgroup.kill` of every `paso-*` of the scope, the one it
/// started and the one it does not know (a step of an earlier planner that died).
#[test]
fn a_hangup_to_the_planner_leaves_no_step_alive() {
    let mut fx = Fixture::new("j14b-hup", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "37.5101", "")]);
    let known = fx.step_dir("a").join("cgroup.kill");
    let ghost = fx.scope().join("paso-99-ghost");
    fs::create_dir_all(&ghost).expect("the cgroup of a step the planner does not know");
    write(&known, "");
    write(&ghost.join("cgroup.kill"), "");
    let live = fx.spawn("0", &[]);
    assert!(
        within(10, || !processes_with("37.5101").is_empty()),
        "the step did not start in 10 s"
    );
    live.signal("HUP");
    let run = live.collect(10);
    let left = survivors_of("37.5101", 5);
    assert!(
        !run.timed_out,
        "the planner did not stop on SIGHUP in 10 s: {}{}",
        run.out, run.err
    );
    assert!(
        left.is_empty(),
        "SIGHUP to the planner left its step alive (pids {left:?}), and nobody owns it now"
    );
    assert_eq!(
        run.code,
        Some(2),
        "SIGHUP stops the planner with 2, like TERM (no verdict): {}{}",
        run.out,
        run.err
    );
    assert_eq!(
        read(&known).trim(),
        "1",
        "drain did not write cgroup.kill of the step it started"
    );
    assert_eq!(
        read(&ghost.join("cgroup.kill")).trim(),
        "1",
        "drain did not write cgroup.kill of a paso-* it does not know: a step that outlived another planner stays alive"
    );
}

/// J14b: a TERM in the window between `Popen` and the step being marked as running does not leave a started step that
/// drain cannot see. The hook `GATE_DAG_TEST_TERM_AFTER_POPEN=1` has the planner send itself TERM right after `Popen`.
#[test]
fn a_term_right_after_the_step_starts_leaves_no_step_alive() {
    let mut fx = Fixture::new("j14b-term", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "37.5102", "")]);
    let run = fx.run_env("0", &[("GATE_DAG_TEST_TERM_AFTER_POPEN", "1")]);
    let left = survivors_of("37.5102", 5);
    assert!(
        !run.timed_out,
        "the planner did not stop on the TERM it sent itself in 30 s: {}{}",
        run.out, run.err
    );
    assert!(
        left.is_empty(),
        "a TERM between Popen and the running state left the step alive (pids {left:?}): drain did not know it"
    );
    assert_eq!(
        run.code,
        Some(2),
        "TERM stops the planner with 2: {}{}",
        run.out,
        run.err
    );
}

/// J14b: a step that ended with processes still in its cgroup (a daemon that outlived it) gets `cgroup.kill`, the
/// planner tries the rmdir again for a while (GATE_DAG_RMDIR_WAIT_S, 5 s by default) and, if the cgroup stays
/// populated, names the orphans in a `note:`. The verdict is the step's own. With `populated 0` there is no note.
#[test]
fn a_step_that_leaves_processes_in_its_cgroup_is_named_an_orphan() {
    let mut fx = Fixture::new("j14b-orphan", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.3", "")]);
    let kill = fx.step_dir("a").join("cgroup.kill");
    write(&kill, "");
    fx.write_step("a", "cgroup.events", "populated 1\nfrozen 0\n");
    let run = fx.run_env("0", &[("GATE_DAG_RMDIR_WAIT_S", "0.3")]);
    assert_green(&run, "a step with an orphan");
    let note = run
        .text()
        .lines()
        .find(|line| line.starts_with("note:") && line.contains("orphan processes"))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no note names the orphan processes: {}", run.text()));
    assert!(
        note.contains("paso-10-a"),
        "the note does not name the cgroup: {note}"
    );
    assert_eq!(
        read(&kill).trim(),
        "1",
        "the planner did not write cgroup.kill of the step that left processes"
    );

    let mut fx = Fixture::new("j14b-sin-huerfanos", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.3", "")]);
    fx.write_step("a", "cgroup.events", "populated 0\nfrozen 0\n");
    let run = fx.run_env("0", &[("GATE_DAG_RMDIR_WAIT_S", "0.3")]);
    assert_green(&run, "a step with no orphan");
    assert!(
        !run.text().contains("orphan"),
        "a step that left nothing is named an orphan: {}",
        run.text()
    );
}

/// A fake step that leaves processes in its cgroup: when it ends it writes `control/procs-<id>` into the `cgroup.procs` of
/// its own fake cgroup (the planner writes the pid of the step there when it starts it, so a file written before the run
/// would be overwritten). $1 id, $2 seconds, $3 control directory; the cgroup of the row of idx 10 is next to `control`.
const ORPHAN_SH: &str = r#"#!/bin/sh
id="$1"
control="$3"
echo "=== $id banner"
sleep "$2"
cat "$control/procs-$id" > "$control/../cgroups/scope/paso-10-$id/cgroup.procs"
exit 0
"#;

/// CI-E15: the note of an orphan says what was in the cgroup. The cgroup lists three pids in `cgroup.procs`: two live `sleep`
/// and one that has already been reaped, which is skipped (its `/proc/<pid>/comm` is gone). `sleep x2` is what is said, and
/// the note stays a note: the run is green. A cgroup whose only pid is the step's own, already dead, keeps the sentence of
/// J14b with no parentheses.
#[test]
fn the_note_of_an_orphan_names_the_processes_that_were_in_the_cgroup() {
    let mut fx = Fixture::new("e15-huerfanos", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.3", "")]);
    write(&fx.script, ORPHAN_SH);
    write(&fx.step_dir("a").join("cgroup.kill"), "");
    fx.write_step("a", "cgroup.events", "populated 1\nfrozen 0\n");
    let mut sleepers: Vec<std::process::Child> = (0..2)
        .map(|_| {
            Command::new("sleep")
                .arg("43.5115")
                .spawn()
                .expect("a sleep for the cgroup")
        })
        .collect();
    let mut gone = Command::new("true").spawn().expect("true");
    gone.wait().expect("true ends");
    let procs = format!(
        "{}\n{}\n{}\n",
        sleepers[0].id(),
        gone.id(),
        sleepers[1].id()
    );
    write(&fx.control.join("procs-a"), &procs);
    let run = fx.run_env("0", &[("GATE_DAG_RMDIR_WAIT_S", "0.3")]);
    for sleeper in &mut sleepers {
        let _ = sleeper.kill();
        let _ = sleeper.wait();
    }
    assert_green(&run, "a step with two orphans that are `sleep`");
    let note = run
        .text()
        .lines()
        .find(|line| line.starts_with("note:") && line.contains("orphan processes"))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no note names the orphan processes: {}", run.text()));
    assert!(
        note.contains("paso-10-a ended with orphan processes in its cgroup (sleep x2);"),
        "the note has to name the cgroup and the two `sleep` (and not the pid that was already gone): {note}"
    );

    let mut fx = Fixture::new("e15-huerfanos-vacio", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.3", "")]);
    write(&fx.step_dir("a").join("cgroup.kill"), "");
    fx.write_step("a", "cgroup.events", "populated 1\nfrozen 0\n");
    let run = fx.run_env("0", &[("GATE_DAG_RMDIR_WAIT_S", "0.3")]);
    assert_green(&run, "a step whose cgroup lists only its own dead pid");
    assert!(
        run.out
            .contains("paso-10-a ended with orphan processes in its cgroup; they"),
        "with no process left to name the note keeps the sentence without parentheses: {}",
        run.text()
    );
}

/// A fake step that leaves a file in its own TMPDIR (where the planner put it) and says which one it is in
/// `control/tmpdir-<id>`; it exits with `control/rc-<id>`. $1 id, $2 seconds, $3 control directory.
const SCRATCH_SH: &str = r#"#!/bin/sh
id="$1"
control="$3"
echo "$TMPDIR" > "$control/tmpdir-$id"
echo "=== $id banner"
echo "left by $id" > "$TMPDIR/scratch"
rc=$(cat "$control/rc-$id" 2>/dev/null || echo 0)
echo "test result: ok. 1 passed; 0 failed"
exit "$rc"
"#;

/// The rows `ids` (idx 10, 20, …) of `fx`, each the step of `SCRATCH_SH`.
fn scratch_table(fx: &mut Fixture, ids: &[&'static str]) {
    let script = fx.base.join("paso-scratch.sh");
    write(&script, SCRATCH_SH);
    let rows: Vec<StepRow> = ids
        .iter()
        .zip(1u32..)
        .map(|(id, n)| StepRow {
            script: script.clone(),
            ..fx.row(10 * n, id, "-", "0.1")
        })
        .collect();
    table_of(fx, &rows);
}

/// The TMPDIR the planner gave the row `id`, as the row said it: `<run directory>/tmp/<id>`. A test that reads it has the
/// anchor of presence: the planner did make the directory, so that its absence afterwards is the planner's doing.
fn tmpdir_of(fx: &Fixture, id: &str) -> PathBuf {
    let said = read(&fx.control.join(format!("tmpdir-{id}")));
    let path = PathBuf::from(said.trim());
    assert!(
        path.starts_with(&fx.tmp)
            && path.to_string_lossy().contains("/memory-industry-dag.")
            && path.ends_with(format!("tmp/{id}")),
        "the row {id} was not given a TMPDIR under the run directory of the planner: {said:?}"
    );
    path
}

/// `<run directory>` from the TMPDIR of one of its rows.
fn run_dir_of(tmpdir: &Path) -> PathBuf {
    tmpdir
        .parent()
        .and_then(Path::parent)
        .expect("tmp/<id> has a run directory above it")
        .to_path_buf()
}

/// The run directories (`memory-industry-dag.*`) that are in the TMPDIR of the fixture.
fn run_dirs_in(fx: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(&fx.tmp)
        .expect("the TMPDIR of the fixture")
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("memory-industry-dag."))
        .collect();
    names.sort();
    names
}

/// CI-E15: a run that ends green with a verdict leaves nothing of itself in TMPDIR. Each row writes in its own TMPDIR (the
/// 65 MB of `tmp/CT` were the `scratch()` of the contracts) and the run directory, with its logs, stayed there for good.
#[test]
fn a_green_run_deletes_its_run_directory() {
    let mut fx = Fixture::new("e15-verde", CPU_FOUR, MEM_OPEN);
    scratch_table(&mut fx, &["a", "b"]);
    let run = fx.run("0");
    assert_green(&run, "two rows that leave a file in their TMPDIR");
    let tmp_a = tmpdir_of(&fx, "a");
    let tmp_b = tmpdir_of(&fx, "b");
    assert_eq!(
        run_dir_of(&tmp_a),
        run_dir_of(&tmp_b),
        "the two rows are of one run"
    );
    assert!(
        !run_dir_of(&tmp_a).exists(),
        "a green run left its run directory ({}): {:?}",
        run_dir_of(&tmp_a).display(),
        run_dirs_in(&fx)
    );
    assert_eq!(
        run_dirs_in(&fx),
        Vec::<String>::new(),
        "a green run left a run directory in TMPDIR"
    );
}

/// CI-E15: a red run keeps its run directory and names it, with the logs of every row and the TMPDIR of the row that went
/// red; the TMPDIR of the row that ended green (`a`) is gone from the moment it ended.
#[test]
fn a_red_run_keeps_its_run_directory_and_names_it() {
    let mut fx = Fixture::new("e15-rojo", CPU_FOUR, MEM_OPEN);
    scratch_table(&mut fx, &["a", "b"]);
    fx.rc("b", "1");
    let run = fx.run("0");
    assert!(!run.timed_out, "did not finish in 30 s");
    assert_eq!(
        run.code,
        Some(1),
        "the red row b decides the exit: {}{}",
        run.out,
        run.err
    );
    let tmp_a = tmpdir_of(&fx, "a");
    let tmp_b = tmpdir_of(&fx, "b");
    let dir = run_dir_of(&tmp_b);
    assert!(
        dir.is_dir(),
        "a red run lost its run directory {}",
        dir.display()
    );
    assert!(
        run.out
            .lines()
            .any(|line| line == format!("dag: run directory: {}", dir.display())),
        "a red run has to name the directory that holds its logs: {}",
        run.out
    );
    assert_eq!(
        read(&tmp_b.join("scratch")).trim(),
        "left by b",
        "the TMPDIR of the red row is where its diagnosis is"
    );
    assert!(
        !tmp_a.exists(),
        "the TMPDIR of a row that ended green stayed ({})",
        tmp_a.display()
    );
    for log in ["pasos/10-a.log", "pasos/20-b.log"] {
        assert!(
            read(&dir.join(log)).contains("banner"),
            "the log {log} of the run is not in the run directory"
        );
    }
}

/// CI-E15: a run with no verdict (here a row the kernel killed) keeps the run directory and the TMPDIR of the row it
/// could not judge, as before: `an_oom_kill_prints_the_blocks_and_names_the_run_directory` says it names it.
#[test]
fn a_run_with_no_verdict_keeps_the_tmpdir_of_the_row_it_could_not_judge() {
    let mut fx = Fixture::new("e15-sin-veredicto", CPU_FOUR, MEM_OPEN);
    scratch_table(&mut fx, &["a", "b"]);
    fx.oom("b");
    let run = fx.run("0");
    assert_eq!(run.code, Some(2), "{}{}", run.out, run.err);
    let tmp_b = tmpdir_of(&fx, "b");
    assert_eq!(
        read(&tmp_b.join("scratch")).trim(),
        "left by b",
        "the TMPDIR of the row the kernel killed was deleted: {}",
        run.text()
    );
    assert!(
        run.text().contains(&format!(
            "dag: run directory: {}",
            run_dir_of(&tmp_b).display()
        )),
        "the run directory is named: {}",
        run.text()
    );
}

/// CI-E15: the red runs of other days do not pile up. A run directory of more than 7 days whose planner is gone is deleted
/// when the next planner makes its own; one of 7 days or less stays, one whose `planner.pid` is a live process stays however
/// old it is, and what is not a run directory is not touched.
#[test]
fn a_planner_deletes_the_run_directories_of_more_than_seven_days_whose_planner_is_gone() {
    let mut fx = Fixture::new("e15-viejos", CPU_FOUR, MEM_OPEN);
    scratch_table(&mut fx, &["a"]);
    let mut dead = Command::new("true").spawn().expect("true");
    dead.wait().expect("true ends");
    let day = Duration::from_secs(24 * 3600);
    let aged = |name: &str, days: u32, pid: Option<u32>| {
        let dir = fx.tmp.join(name);
        fs::create_dir_all(dir.join("pasos")).expect("a run directory of another day");
        write(&dir.join("pasos/10-a.log"), "an old log\n");
        if let Some(pid) = pid {
            write(&dir.join("planner.pid"), &format!("{pid}\n"));
        }
        let when = std::time::SystemTime::now() - day * days;
        File::open(&dir)
            .and_then(|handle| handle.set_modified(when))
            .expect("the age of the directory");
        dir
    };
    let old = aged("memory-industry-dag.old000", 8, None);
    let old_dead = aged("memory-industry-dag.dead00", 9, Some(dead.id()));
    let young = aged("memory-industry-dag.young0", 6, None);
    let busy = aged("memory-industry-dag.busy00", 30, Some(std::process::id()));
    let other = aged("not-a-run-directory", 30, None);
    let run = fx.run("0");
    assert_green(&run, "a planner with run directories of other days around");
    assert!(
        !old.exists(),
        "a run directory of 8 days with no planner pid was not deleted"
    );
    assert!(
        !old_dead.exists(),
        "a run directory of 9 days whose planner is dead was not deleted"
    );
    assert!(
        young.exists(),
        "a run directory of 6 days was deleted: it is not old enough"
    );
    assert!(
        busy.exists(),
        "a run directory whose planner.pid is a live process was deleted"
    );
    assert!(
        other.exists(),
        "a directory that is not a run directory was deleted"
    );
}

/// J14c: `anon` does not see the pages a tmpfs holds (`shmem`), which the postgres of each lane writes under the
/// cluster root and which are charged to the scope. What the planner samples and records is anon + shmem: here 1 GiB
/// and 2 GiB are 3 GiB.
#[test]
fn the_memory_of_a_step_counts_its_tmpfs_pages() {
    let mut fx = Fixture::new("j14c-shmem", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.3", "")]);
    fx.write_step(
        "a",
        "memory.stat",
        &format!("anon {GIB}\nshmem {}\ninactive_file 0\n", 2 * GIB),
    );
    let before = snapshot(&fx.history, "pasos.jsonl");
    assert_green(&fx.run("0"), "a step with tmpfs pages");
    let record = by_id(&records_of(&fx, "pasos.jsonl", &before), "a").clone();
    assert_eq!(
        record["anon_max"].as_u64(),
        Some(3 * GIB),
        "anon 1 GiB + shmem 2 GiB are 3 GiB of memory the step holds: {record}"
    );
}

/// J14c: the estimate that admission reads is anon + shmem too. Two steps of 1 GiB of anon and 4 GiB of tmpfs each
/// are 6,25 GiB apiece with the margin, so they do not share a 10 GiB scope; counted by anon alone they would (2,5).
#[test]
fn admission_counts_the_tmpfs_pages_of_a_step() {
    let mut fx = Fixture::new("j14c-admision", CPU_FOUR, MEM_R10);
    fx.table(&[(10, "a", "-", "0.5", ""), (20, "b", "-", "0.5", "")]);
    for id in ["a", "b"] {
        fx.write_step(
            id,
            "memory.stat",
            &format!("anon {GIB}\nshmem {}\ninactive_file 0\n", 4 * GIB),
        );
    }
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG");
    let steps = records_of(&fx, "pasos.jsonl", &before);
    assert!(
        !overlaps(by_id(&steps, "a"), by_id(&steps, "b")),
        "two steps with 5 GiB each shared a scope of 10 GiB: the tmpfs pages were not counted"
    );
    // The two are the same size, so which of them starts first depends on the durations the seed measured: the one that
    // starts second is the one that waited.
    let later = if num(by_id(&steps, "a"), "inicio") <= num(by_id(&steps, "b"), "inicio") {
        "b"
    } else {
        "a"
    };
    let waiting = by_id(&steps, later);
    assert!(
        num(waiting, "espera_s") > 0.0 && waiting.to_string().contains("ram"),
        "{later} started second and waited for RAM, and has to say so: {waiting}"
    );
}

/// J14d: with a DATABASE_URL in the planner's environment every row shares one server (merge-gate makes no lanes), and
/// sqlx drops databases by name, so the planner runs in series and says why. The control, without it, is parallel.
#[test]
fn an_external_database_url_forces_the_series() {
    let mut fx = Fixture::new("j14d", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "p", "-", "0.4", ""), (20, "q", "-", "0.4", "")]);
    fx.seed();
    let before = snapshot(&fx.history, "pasos.jsonl");
    let control = fx.run("1");
    assert_green(&control, "the control, no DATABASE_URL");
    assert!(
        overlaps(
            by_id(&records_of(&fx, "pasos.jsonl", &before), "p"),
            by_id(&records_of(&fx, "pasos.jsonl", &before), "q")
        ),
        "the control did not run p and q together, so the scenario below proves nothing"
    );

    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run_env("1", &[("DATABASE_URL", "postgresql://x@h/b")]);
    assert_green(&run, "a DATABASE_URL of the operator");
    assert!(
        run.text()
            .lines()
            .any(|line| line.starts_with("note:") && line.contains("DATABASE_URL")),
        "the planner has to say that DATABASE_URL puts it in series: {}",
        run.text()
    );
    let steps = records_of(&fx, "pasos.jsonl", &before);
    assert!(
        !overlaps(by_id(&steps, "p"), by_id(&steps, "q")),
        "two rows shared the server of a DATABASE_URL: sqlx drops databases by name"
    );
}

/// J14d under the judge (tanda 9): the contracts run in the lane CT with the lane's `DATABASE_URL`, and every other name the
/// judge exports is there too. The planner of a test must not inherit them, or no DAG a test asks for runs as one (18 reds of
/// the first pass of every test with the environment of each lane, all with `runs=serie (DATABASE_URL names a server of the
/// operator)`). Both sides, with the environment of the judge put on the command the way a parent would give it:
/// 1. every name of JUDGE_LANE_ENV given to the launcher is gone for the planner and for its steps, and the DAG runs as a DAG;
/// 2. the same `DATABASE_URL` given as the test's own variable (`extra`, applied after) is still read, and puts it in series.
#[test]
fn the_planner_of_a_test_does_not_inherit_the_environment_of_the_judge() {
    let mut fx = Fixture::new("j14d-juez", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "p", "-", "0.4", ""), (20, "q", "-", "0.4", "")]);
    fx.seed();

    let mut command = Command::new("python3");
    command
        .arg("-I")
        .arg(repo_root().join("scripts/gate-dag.py"))
        .arg("run")
        .arg("--table")
        .arg(&fx.table);
    for name in JUDGE_LANE_ENV {
        command.env(name, "postgresql://ajena:ajena@127.0.0.1:1/ajena");
    }
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run_command(command, "1", &[]);
    assert_green(&run, "the environment of the judge, inherited");
    let line = plan_line_of(&run);
    assert!(
        line.contains(" asked=maximo runs=dag") && !line.contains("DATABASE_URL"),
        "the planner read the DATABASE_URL of the judge instead of the test's own environment: {line}"
    );
    let steps = records_of(&fx, "pasos.jsonl", &before);
    assert!(
        overlaps(by_id(&steps, "p"), by_id(&steps, "q")),
        "p and q did not run together, so the DAG was not a DAG: {line}"
    );
    for id in ["p", "q"] {
        let seen = read(&fx.control.join(format!("env-{id}.txt")));
        let leaked: Vec<&str> = JUDGE_LANE_ENV
            .into_iter()
            .filter(|name| seen.lines().any(|l| l.starts_with(&format!("{name}="))))
            .collect();
        assert!(
            leaked.is_empty(),
            "step {id} got names of the judge's environment through the planner: {leaked:?}"
        );
    }

    let run = fx.run_env("1", &[("DATABASE_URL", "postgresql://x@h/b")]);
    assert_green(&run, "a DATABASE_URL of the test itself");
    let line = plan_line_of(&run);
    assert!(
        line.contains(" asked=maximo runs=serie (DATABASE_URL names a server of the operator)"),
        "a DATABASE_URL the test names has to be read, or the J14d contract above proves nothing: {line}"
    );
}

/// J14e: a failure of the planner itself (here, the cgroup directory of the step is a file, so it cannot be made) is
/// exit 2 with the traceback on stderr, never the 1 of an uncaught exception, which merge-gate reads as a red test.
/// The run directory is named, because the logs of the steps are in it.
#[test]
fn an_internal_failure_is_no_verdict_not_red() {
    let mut fx = Fixture::new("j14e-interno", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.3", "")]);
    let dir = fx.step_dir("a");
    fs::remove_dir_all(&dir).expect("the cgroup directory of the step");
    write(&dir, "not a directory");
    let run = fx.run("0");
    assert!(!run.timed_out, "did not finish in 30 s");
    assert_eq!(
        run.code,
        Some(2),
        "an OSError of the planner is no verdict (2), not a red test (1): {}{}",
        run.out,
        run.err
    );
    assert!(
        run.err.contains("Traceback"),
        "the traceback has to reach stderr: {}",
        run.err
    );
    assert!(
        run.text()
            .contains(&format!("{}/memory-industry-dag.", fx.tmp.display())),
        "the run directory has to be named: {}",
        run.text()
    );
}

/// J14e: when the planner fails inside, the blocks of the steps that finished are printed in idx order and the run
/// directory is named. s10 (1 s) and s20 (0.1 s) run together, and s30, after s20, cannot start: s20's block was
/// waiting for s10's, and s10 is stopped by the failure, so both have to come out, in order.
#[test]
fn an_internal_failure_prints_the_blocks_that_finished() {
    let mut fx = Fixture::new("j14e-bloques", CPU_FOUR, MEM_OPEN);
    fx.table(&[
        (10, "s10", "-", "1.0", ""),
        (20, "s20", "-", "0.1", ""),
        (30, "s30", "s20", "0.1", ""),
    ]);
    fx.seed();
    let dir = fx.step_dir("s30");
    fs::remove_dir_all(&dir).expect("the cgroup directory of s30");
    write(&dir, "not a directory");
    let run = fx.run("1");
    assert!(!run.timed_out, "did not finish in 30 s");
    assert_eq!(run.code, Some(2), "{}{}", run.out, run.err);
    let p10 = run
        .out
        .find("=== s10 banner")
        .unwrap_or_else(|| panic!("the block of s10 was not printed: {}", run.out));
    let p20 = run.out.find("=== s20 banner").unwrap_or_else(|| {
        panic!(
            "the block of s20, which finished, was not printed: {}",
            run.out
        )
    });
    assert!(p10 < p20, "the blocks are not in idx order: {}", run.out);
    assert!(
        run.text()
            .contains(&format!("{}/memory-industry-dag.", fx.tmp.display())),
        "the run directory has to be named: {}",
        run.text()
    );
}

/// J14e: a step the kernel killed (OOM) gives no verdict, and its log is not lost: the blocks of the steps that
/// finished are printed in idx order (s10 is stopped by the planner, s20 died) and the run directory is named.
#[test]
fn an_oom_kill_prints_the_blocks_and_names_the_run_directory() {
    let mut fx = Fixture::new("j14e-oom", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "s10", "-", "1.0", ""), (20, "s20", "-", "0.1", "")]);
    fx.seed();
    fx.oom("s20");
    let run = fx.run("1");
    assert_eq!(run.code, Some(2), "{}{}", run.out, run.err);
    assert!(
        run.err.contains("no verdict") && run.err.contains("s20"),
        "{}",
        run.err
    );
    let p10 = run
        .out
        .find("=== s10 banner")
        .unwrap_or_else(|| panic!("the block of s10 was not printed: {}", run.out));
    let p20 = run.out.find("=== s20 banner").unwrap_or_else(|| {
        panic!(
            "the block of s20, the step the kernel killed, was not printed: {}",
            run.out
        )
    });
    assert!(p10 < p20, "the blocks are not in idx order: {}", run.out);
    assert!(
        run.text()
            .contains(&format!("{}/memory-industry-dag.", fx.tmp.display())),
        "the run directory has to be named: {}",
        run.text()
    );
}

/// J14g: a row that fail-fast stops gets TERM before KILL, so the EXIT trap of its shell runs (the stage of the
/// mutants, the clusters of a lane). s10 goes red as soon as s20 says it is up; s20, which sleeps 30 s, is stopped and
/// its trap writes its mark. With KILL straight away the mark is not there.
#[test]
fn a_row_the_planner_stops_runs_its_exit_trap() {
    let mut fx = Fixture::new("j14g-trap", CPU_FOUR, MEM_OPEN);
    let script = fx.base.join("trap.sh");
    write(&script, TRAP_SH);
    bash_table(
        &mut fx,
        &[(10, "s10", "0.3", &script), (20, "s20", "0.3", &script)],
    );
    fx.seed();
    for id in ["s10", "s20"] {
        let _ = fs::remove_file(fx.control.join(format!("mark-{id}")));
        let _ = fs::remove_file(fx.control.join(format!("up-{id}")));
    }
    fx.rc("s10", "1");
    write(&fx.control.join("wait-for-s10"), "s20");
    write(&fx.control.join("sleep-s10"), "0.1");
    write(&fx.control.join("sleep-s20"), "30");
    let run = fx.run("1");
    assert!(
        !run.timed_out,
        "did not finish in 30 s: {}{}",
        run.out, run.err
    );
    assert_eq!(
        run.code,
        Some(1),
        "the red of s10 is the verdict: {}{}",
        run.out,
        run.err
    );
    assert_eq!(
        read(&fx.control.join("mark-s10")).trim(),
        "exited",
        "the red step's own trap did not run: the scenario proves nothing"
    );
    assert_eq!(
        read(&fx.control.join("mark-s20")).trim(),
        "exited",
        "s20 was stopped without TERM, so its EXIT trap did not run and it cleaned up nothing"
    );
}

/// J14g: a row that ignores TERM dies by KILL when the wait ends (GATE_DAG_TERM_GRACE_S, 15 s by default, 1 s here),
/// and the planner does not hang on it: nothing of it is left and the run ends well before the 30 s of the contract.
#[test]
fn a_row_that_ignores_term_dies_by_kill_when_the_grace_ends() {
    let mut fx = Fixture::new("j14g-sordo", CPU_FOUR, MEM_OPEN);
    let trap = fx.base.join("trap.sh");
    let deaf = fx.base.join("sordo.sh");
    write(&trap, TRAP_SH);
    write(&deaf, DEAF_SH);
    bash_table(
        &mut fx,
        &[(10, "s10", "0.3", &trap), (20, "s20", "0.3", &deaf)],
    );
    fx.seed();
    for id in ["s10", "s20"] {
        let _ = fs::remove_file(fx.control.join(format!("up-{id}")));
    }
    fx.rc("s10", "1");
    write(&fx.control.join("wait-for-s10"), "s20");
    write(&fx.control.join("sleep-s10"), "0.1");
    write(&fx.control.join("sleep-s20"), "34.2718");
    let started = Instant::now();
    let run = fx.run_env("1", &[("GATE_DAG_TERM_GRACE_S", "1")]);
    let took = started.elapsed();
    let left = survivors_of("34.2718", 5);
    assert!(
        !run.timed_out && took < Duration::from_secs(12),
        "the planner waited for a row that ignores TERM ({took:?}): {}{}",
        run.out,
        run.err
    );
    assert_eq!(run.code, Some(1), "{}{}", run.out, run.err);
    assert!(
        left.is_empty(),
        "the row that ignores TERM was not killed when the grace ended (pids {left:?})"
    );
}

/// J14k: the key of the history is the rows of the table, normalised: a comment, a blank line or wider spacing does
/// not change it, so the history stays; a number of a row does, and then there is none. The key is the `tabla` field.
#[test]
fn a_comment_keeps_the_history_of_a_table_and_a_number_does_not() {
    let mut fx = Fixture::new("j14k", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "p", "-", "0.4", ""), (20, "q", "-", "0.4", "")]);
    fx.seed();
    let seeded = appended(&fx.history, "pasos.jsonl", &HashMap::new());
    let key = by_id(&seeded, "p")["tabla"].clone();
    assert!(
        key.is_string(),
        "the series records the key of the table: {key}"
    );

    let original = fx.table_text();
    write(
        &fx.table,
        &format!(
            "# a comment above the rows\n\n{}\n# and one below\n",
            original.replace(' ', "  ")
        ),
    );
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "the table with a comment");
    assert!(
        !run.text().contains("note: no serial history"),
        "a comment dropped the history: {}",
        run.text()
    );
    let steps = records_of(&fx, "pasos.jsonl", &before);
    assert_eq!(
        by_id(&steps, "p")["tabla"],
        key,
        "a comment, a blank line and wider spacing changed the key of the table"
    );
    assert!(
        overlaps(by_id(&steps, "p"), by_id(&steps, "q")),
        "with its history kept the run is a DAG"
    );

    // CI-J18 (D-J18d): the rows in another order are the same table (`parse_table` sorts them by idx), so the key is the same
    // and the history stays. The seeded run has the key of the rows in their order of idx.
    let reversed: String = original
        .lines()
        .rev()
        .map(|row| format!("{row}\n"))
        .collect();
    assert_ne!(
        reversed, original,
        "the fixture has to have its rows in another order"
    );
    write(&fx.table, &reversed);
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "the table with its rows in reverse order");
    assert!(
        !run.text().contains("note: no serial history"),
        "the same rows in another order dropped the history: {}",
        run.text()
    );
    let steps = records_of(&fx, "pasos.jsonl", &before);
    assert_eq!(
        by_id(&steps, "p")["tabla"],
        key,
        "the same rows in another order changed the key of the table"
    );
    assert!(
        overlaps(by_id(&steps, "p"), by_id(&steps, "q")),
        "with its history kept the reversed table is a DAG"
    );

    write(
        &fx.table,
        &original.replacen("normal 1 5 ", "normal 2 5 ", 1),
    );
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "the table with another number");
    assert!(
        run.text().contains("note: no serial history"),
        "a number of a row changed and the history of the old table was used: {}",
        run.text()
    );
    assert_ne!(
        by_id(&records_of(&fx, "pasos.jsonl", &before), "p")["tabla"],
        key,
        "a number of a row did not change the key of the table"
    );
}

/// J14l: the numbers of a row are finite and above 0. `nan`, `inf`, a negative or a 0 in cpu, ram_gb or dur_s is
/// exit 2 naming the line, the step and the column, before anything starts; a row of good numbers is green.
#[test]
fn a_row_with_a_number_that_is_not_finite_and_above_zero_is_refused() {
    let cases = [
        ("cpu", "nan"),
        ("cpu", "inf"),
        ("cpu", "-1"),
        ("cpu", "0"),
        ("ram_gb", "nan"),
        ("ram_gb", "-2"),
        ("ram_gb", "0"),
        ("dur_s", "inf"),
        ("dur_s", "0"),
    ];
    for (column, bad) in cases {
        let fx = Fixture::new("j14l", CPU_FOUR, MEM_OPEN);
        let (cpu, ram, dur) = match column {
            "cpu" => (bad, "5", "0.05"),
            "ram_gb" => ("1", bad, "0.05"),
            _ => ("1", "5", bad),
        };
        write(
            &fx.table,
            &format!("10 rowx - - normal {cpu} {ram} {dur} /bin/true\n"),
        );
        let run = fx.run("0");
        assert!(!run.timed_out, "{column}={bad}: did not finish in 30 s");
        assert_eq!(
            run.code,
            Some(2),
            "{column}={bad}: exit {:?}, want 2\n{}{}",
            run.code,
            run.out,
            run.err
        );
        assert!(
            run.err.contains("table line 1")
                && run.err.contains("rowx")
                && run.err.contains(column),
            "{column}={bad}: the refusal does not name the line, the step and the column: {}",
            run.err
        );
    }
    let fx = Fixture::new("j14l-verde", CPU_FOUR, MEM_OPEN);
    write(&fx.table, "10 rowx - - normal 1 5 0.05 /bin/true\n");
    assert_green(&fx.run("0"), "a row of good numbers");
}

// --- CI-J15a (H1): the key of a tree has one formula, and the planner says it -------------------------------------------

/// `gate-dag.py tree-key <tree>` of `planner`, with the deadline every process of this file has: (exit code, stdout, stderr).
fn planner_tree_key(planner: &Path, tree: &Path) -> (Option<i32>, String, String) {
    let base = scratch("tree-key");
    let out_path = base.join("salida.txt");
    let err_path = base.join("error.txt");
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(planner)
        .arg("tree-key")
        .arg(tree)
        .stdout(Stdio::from(File::create(&out_path).expect("stdout file")))
        .stderr(Stdio::from(File::create(&err_path).expect("stderr file")))
        .spawn()
        .expect("python3");
    let deadline = Instant::now() + Duration::from_secs(30);
    let code = loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            break status.code();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`gate-dag.py tree-key` did not answer in 30 s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (code, read(&out_path), read(&err_path))
}

/// The key of a tree as the judge's targets name it, written apart from the planner: the name of the directory of its
/// realpath, a dash and the first 12 hex of the sha256 of that realpath, with no line break in what is hashed.
fn expected_tree_key(tree: &Path) -> String {
    let real = fs::canonicalize(tree).expect("the tree exists");
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("sha256sum");
    let mut stdin = child.stdin.take().expect("sha256sum stdin");
    std::io::Write::write_all(&mut stdin, real.to_string_lossy().as_bytes()).expect("write");
    drop(stdin);
    let out = child.wait_with_output().expect("sha256sum output");
    let digest = String::from_utf8_lossy(&out.stdout).to_string();
    let name = real
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the tree has a name");
    format!("{name}-{}", &digest[..12])
}

/// CI-J15a: `gate-dag.py tree-key <tree>` prints `<name>-<12 hex>` of the realpath of the tree and nothing else, the same
/// for a link to it. The gate-audit.sh asked it for the key of its tree, so the audit and the planner cannot disagree again:
/// the audit used the sha256 of the realpath WITH its line break (`82dd32511c99` against `46cf9e6d38af`, measured on 9-nov).
#[test]
fn the_planner_says_the_key_of_a_tree_and_the_key_is_of_its_realpath() {
    let base = scratch("tree-key-tree");
    let tree = base.join("memory-industry");
    fs::create_dir_all(&tree).expect("tree");
    let planner = repo_root().join("scripts/gate-dag.py");
    let (code, out, err) = planner_tree_key(&planner, &tree);
    assert_eq!(
        code,
        Some(0),
        "`gate-dag.py tree-key <tree>` has to print the key of the tree and exit 0: {err}"
    );
    assert_eq!(
        out,
        format!("{}\n", expected_tree_key(&tree)),
        "the key is <name>-<12 hex of the sha256 of the realpath, with no line break>, and the only thing printed"
    );
    let link = base.join("link-to-the-tree");
    std::os::unix::fs::symlink(&tree, &link).expect("symlink");
    let (code, linked, err) = planner_tree_key(&planner, &link);
    assert_eq!(code, Some(0), "the key of a link to a tree: {err}");
    assert_eq!(
        linked, out,
        "a link to a tree is the tree: the key is of the realpath"
    );
}

/// CI-J15a: a tree that does not exist has no key: exit 2 with a FAIL line and nothing on stdout, so the audit that asks
/// for it stops and does not hash the empty string.
#[test]
fn a_tree_that_does_not_exist_has_no_key() {
    let base = scratch("tree-key-missing");
    let (code, out, err) = planner_tree_key(
        &repo_root().join("scripts/gate-dag.py"),
        &base.join("not-there"),
    );
    assert_eq!(code, Some(2), "exit {code:?}, want 2: {err}");
    assert!(
        out.is_empty(),
        "a key was printed for a tree that does not exist: {out}"
    );
    assert!(err.contains("FAIL"), "the refusal says nothing: {err}");
}

/// CI-J15a (D-F3g): the target of a row is <CARGO_TARGET_DIR>/<the key tree-key says>/<T>. The planner builds the path with
/// the same formula it prints, so there is one formula and two ways to ask it.
#[test]
fn the_target_of_a_row_is_under_the_key_the_planner_says() {
    let mut fx = Fixture::new("tree-key-target", CPU_FOUR, MEM_OPEN);
    let root = fx.base.join("target-root");
    table_with_resources(&mut fx, &[(10, "DBG", "target:T_dbg", "1")]);
    let run = fx.run_env(
        "0",
        &[("CARGO_TARGET_DIR", root.to_str().expect("utf-8 path"))],
    );
    assert_green(&run, "a step that names a target");
    let (code, key, err) = planner_tree_key(&repo_root().join("scripts/gate-dag.py"), &repo_root());
    assert_eq!(code, Some(0), "tree-key of the tree of the planner: {err}");
    let seen = PathBuf::from(target_seen_by(&fx, "DBG").expect("the step got no CARGO_TARGET_DIR"));
    assert_eq!(
        seen,
        root.join(key.trim()).join("T_dbg"),
        "the target of the row is not under the key that tree-key says"
    );
}

// --- CI-K4: the profile of the judge, the reports of --timings and the plan of the machine ----------------------------
// D-K4a, D-K4f (the profile), D-K4g..D-K4j (the reports of `cargo --timings` that a row leaves in its target, and what
// the planner does with them), D-K4k..D-K4r (the machine the planner measures, the plan it prints, the jobs it hands
// out, the overlap it measures). CI-E7a-1 (D-E7a..D-E7f) changed three of them: the jobs are all the threads of the cap,
// there is no table of lanes (`auto` is the DAG of `maximo`) and the admission counts with the measure of the plan. The machine is the test's: a `nproc` that answers what the test says,
// the MemAvailable of its own /proc/meminfo (GATE_PROC_MEMINFO), the cgroup tree of the fixture and a root of power
// supplies that the test makes (GATE_POWER_SUPPLY_ROOT). Nothing here reads the real one, and nothing sleeps to pass.

/// The tokens DBG, NLI and DC carry in gate-dag.txt (D-K4a).
const PROFILE: &str =
    "CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_DEV_STRIP=symbols";
const PROFILE_NAMES: [&str; 3] = [
    "CARGO_INCREMENTAL",
    "CARGO_PROFILE_DEV_DEBUG",
    "CARGO_PROFILE_DEV_STRIP",
];
const KIB_PER_GIB: u64 = 1 << 20;

/// A machine the test describes: the `nproc` it answers (first on PATH), its /proc/meminfo and its power supplies.
struct Machine {
    path: String,
    meminfo: String,
    power: String,
}

impl Machine {
    fn env(&self) -> Vec<(&str, &str)> {
        vec![
            ("PATH", self.path.as_str()),
            ("GATE_PROC_MEMINFO", self.meminfo.as_str()),
            ("GATE_POWER_SUPPLY_ROOT", self.power.as_str()),
        ]
    }
}

/// `nproc` prints `nproc` (a number, or whatever a test wants to be refused), MemAvailable is `mem_kib`, and the
/// machine has no power supply at all (a root that does not exist). `tag` keeps two machines of one fixture apart.
fn machine(fx: &Fixture, tag: &str, nproc: &str, mem_kib: u64) -> Machine {
    let bin = fx.base.join(format!("bin-{tag}"));
    fs::create_dir_all(&bin).expect("bin dir");
    let fake = bin.join("nproc");
    write(&fake, &format!("#!/bin/sh\necho {nproc}\n"));
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).expect("nproc executable");
    let meminfo = fx.base.join(format!("meminfo-{tag}"));
    fs::copy(fx.meminfo(mem_kib), &meminfo).expect("meminfo of the machine");
    Machine {
        path: format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
        meminfo: meminfo.display().to_string(),
        power: fx.base.join("sin-baterias").display().to_string(),
    }
}

/// The first line of the planner's stdout that starts with `plan:` (D-K4q).
fn plan_line_of(run: &Run) -> String {
    run.out
        .lines()
        .find(|line| line.starts_with("plan:"))
        .unwrap_or_else(|| {
            panic!(
                "the planner printed no `plan:` line:\n{}{}",
                run.out, run.err
            )
        })
        .to_string()
}

/// The word that follows `key` in `text`, up to the next space.
fn word_after<'a>(text: &'a str, key: &str) -> &'a str {
    text.split(key)
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("")
}

/// Spec K4-A, D-K4a: a row starts with the profile its row names, over the one it inherits, and the line that names the
/// variables it changed lists the three names and no value.
#[test]
fn a_row_gets_the_profile_of_the_judge_over_the_one_it_inherits() {
    let mut fx = Fixture::new("k4-perfil", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "p", "-", "0.1", PROFILE)]);
    let run = fx.run_env(
        "0",
        &[
            ("CARGO_INCREMENTAL", "1"),
            ("CARGO_PROFILE_DEV_DEBUG", "2"),
            ("CARGO_PROFILE_DEV_STRIP", "none"),
        ],
    );
    assert_green(&run, "a row that names the profile of the judge");
    let env = read(&fx.control.join("env-p.txt"));
    for want in [
        "CARGO_INCREMENTAL=0",
        "CARGO_PROFILE_DEV_DEBUG=0",
        "CARGO_PROFILE_DEV_STRIP=symbols",
    ] {
        assert!(
            env.lines().any(|line| line == want),
            "the step did not see {want}: {env}"
        );
    }
    let named = run
        .out
        .lines()
        .find(|line| line.starts_with("dag: p env:"))
        .unwrap_or_else(|| panic!("no `dag: p env:` line: {}", run.out));
    for name in PROFILE_NAMES {
        assert!(
            named.split_whitespace().any(|word| word == name),
            "`{named}` does not name {name}"
        );
    }
    assert!(
        !named.contains('='),
        "the line of the changed variables carries a value: {named}"
    );
}

/// Spec K4-A: the closed list of the step environment holds the three names of the profile, asked of the planner.
#[test]
fn the_closed_list_of_the_step_environment_holds_the_names_of_the_profile() {
    let names = planner_allowed_env();
    for name in PROFILE_NAMES {
        assert!(
            names.iter().any(|listed| listed == name),
            "{name} is not in the list the planner printed: {names:?}"
        );
    }
}

/// Spec K4-A, D-K4f: a reader has exactly the profile tokens of the row that builds its target, because the profile is
/// in the hash of every unit and a reader with another one would build inside a row that builds nothing. A table where
/// it does not is exit 2 before any step starts, naming the reader, the producer, the variable and the decision. The
/// same tokens, in any order, are accepted.
#[test]
fn a_reader_with_another_profile_than_its_producer_is_refused_before_any_step_starts() {
    let producer = "CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_STRIP=symbols";
    let cases = [
        (
            "a reader without a token its producer has",
            producer,
            "CARGO_PROFILE_DEV_STRIP=symbols",
            "CARGO_INCREMENTAL",
        ),
        (
            "a reader with another value of the same variable",
            producer,
            "CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_STRIP=debuginfo",
            "CARGO_PROFILE_DEV_STRIP",
        ),
        (
            "a reader with a token its producer lacks",
            "CARGO_INCREMENTAL=0",
            producer,
            "CARGO_PROFILE_DEV_STRIP",
        ),
    ];
    for (defect, builds, reads, variable) in cases {
        let mut fx = Fixture::new("k4-lector", CPU_FOUR, MEM_OPEN);
        let rows = [
            StepRow {
                env: builds,
                ..fx.row(10, "DBG", "target:T_dbg,cluster:C_dbg", "0.1")
            },
            StepRow {
                deps: "DBG",
                env: reads,
                ..fx.row(20, "NLI", "target:T_dbg,cluster:C_nli,reader", "0.1")
            },
        ];
        table_of(&mut fx, &rows);
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
        for word in ["FAIL:", "NLI", "DBG", variable, "D-K4f"] {
            assert!(
                run.err.contains(word),
                "{defect}: the refusal does not say {word}: {}",
                run.err
            );
        }
        assert!(
            fx.started().is_empty(),
            "{defect}: a step started although the table is refused: {:?}",
            fx.started()
        );
    }
    let mut fx = Fixture::new("k4-lector-igual", CPU_FOUR, MEM_OPEN);
    let rows = [
        StepRow {
            env: producer,
            ..fx.row(10, "DBG", "target:T_dbg,cluster:C_dbg", "0.1")
        },
        StepRow {
            deps: "DBG",
            env: "CARGO_PROFILE_DEV_STRIP=symbols CARGO_INCREMENTAL=0",
            ..fx.row(20, "NLI", "target:T_dbg,cluster:C_nli,reader", "0.1")
        },
    ];
    table_of(&mut fx, &rows);
    let root = target_root(&fx);
    let run = fx.run_env("0", &[("CARGO_TARGET_DIR", root.as_str())]);
    assert_green(
        &run,
        "a reader with the same profile tokens as its producer",
    );
}

/// Spec K4-B, D-K4q, E7a.2 (D-E7c): the plan is one line of the planner, before the first block, and the same plan is kept
/// in the line of the run in corridas.jsonl. The machine: 24 cores, a cgroup of 16 CPUs, MemAvailable 40 GiB and a cgroup
/// that leaves 31 GiB (the lower one is the RAM free), no power supply. Jobs: min(24, 16, 31 / 2) = 15. The plan has no
/// lanes: neither the word in the line nor the key in the record.
#[test]
fn the_plan_of_the_machine_is_printed_before_the_first_block_and_recorded() {
    let mut fx = Fixture::new("k4-plan", "1600000 100000", "34359738368");
    write(
        &fx.cgroups.join("scope").join("memory.current"),
        "1073741824",
    );
    fx.table(&[(10, "a", "-", "0.1", ""), (20, "b", "-", "0.1", "")]);
    let host = machine(&fx, "plan", "24", 40 * KIB_PER_GIB);
    let ghost = fx
        .base
        .join("target-aun-sin-hacer")
        .join("mas")
        .join("hondo");
    let ghost = ghost.display().to_string();
    let mut env = host.env();
    env.push(("CARGO_TARGET_DIR", ghost.as_str()));
    let before = snapshot(&fx.history, "corridas.jsonl");
    let run = fx.run_env("0", &env);
    assert_green(&run, "a green table on a measured machine");
    let lines: Vec<&str> = run.out.lines().collect();
    let first_block = lines
        .iter()
        .position(|line| line.contains("=== a banner"))
        .expect("the block of the first row");
    let plan_at = lines
        .iter()
        .position(|line| line.starts_with("plan:"))
        .unwrap_or_else(|| panic!("no `plan:` line:\n{}{}", run.out, run.err));
    assert!(
        plan_at < first_block,
        "the plan (line {plan_at}) is not before the block of the first row (line {first_block}): {}",
        run.out
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("plan:"))
            .count(),
        1,
        "the plan is one line: {}",
        run.out
    );
    let line = lines[plan_at];
    assert!(
        line.starts_with("plan: nproc=24 cgroup_cpu=16 ram_free=31.0 GiB disk_free="),
        "{line}"
    );
    assert!(
        line.contains(" GB battery=none jobs=15 (gate) asked=serie runs=serie"),
        "{line}"
    );
    assert!(!line.contains("lanes="), "the plan has no lanes: {line}");
    assert!(
        word_after(line, "disk_free=").parse::<u64>().is_ok(),
        "the free disk of a target that does not exist yet is the one of its first ancestor that does: {line}"
    );
    let runs = appended(&fx.history, "corridas.jsonl", &before);
    let plan = &runs
        .first()
        .unwrap_or_else(|| panic!("no run line: {}{}", run.out, run.err))["plan"];
    assert_eq!(plan["nproc"].as_u64(), Some(24), "{plan}");
    assert_eq!(plan["cgroup_cpu"].as_u64(), Some(16), "{plan}");
    assert!(
        plan["ram_free"]
            .as_f64()
            .is_some_and(|gib| (gib - 31.0).abs() < 0.05),
        "ram_free of the plan is in GiB, the lower of MemAvailable and the cgroup's margin: {plan}"
    );
    assert!(plan["disk_free"].is_u64(), "{plan}");
    assert_eq!(plan["bateria"].as_str(), Some("none"), "{plan}");
    assert_eq!(plan["jobs"].as_u64(), Some(15), "{plan}");
    assert_eq!(plan["jobs_origen"].as_str(), Some("gate"), "{plan}");
    assert!(
        plan.as_object()
            .is_some_and(|fields| !fields.contains_key("carriles")),
        "the plan of the record has no lanes: {plan}"
    );
    assert_eq!(plan["pedido"].as_str(), Some("serie"), "{plan}");
    assert_eq!(plan["corre"].as_str(), Some("serie"), "{plan}");
    assert!(
        plan.as_object()
            .is_some_and(|fields| fields.get("fallo").is_some_and(Value::is_null)),
        "what was measured and did not fail leaves `fallo` as null: {plan}"
    );
}

/// Spec K4-B, D-K4l: a CARGO_BUILD_JOBS of the operator is the jobs of the plan, and the line says whose they are.
#[test]
fn the_jobs_of_the_operator_are_the_jobs_of_the_plan_and_the_line_says_so() {
    let mut fx = Fixture::new("k4-plan-operador", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "a", "-", "0.1", "")]);
    let host = machine(&fx, "operador", "24", 100 * KIB_PER_GIB);
    let mut env = host.env();
    env.push(("CARGO_BUILD_JOBS", "5"));
    let run = fx.run_env("0", &env);
    assert_green(&run, "an operator that names 5 jobs");
    let line = plan_line_of(&run);
    assert!(line.contains(" jobs=5 (operator) "), "{line}");
}

/// Spec E7a.3 (D-E7b): the admission counts with the measure of the plan, and the run records what it counted as C and
/// R. C is min(nproc, the CPUs of the cgroup); R is the RAM free (the lower of MemAvailable and what the cgroup leaves)
/// less max(4 GiB, 10 % of it). By hand: 10 GiB free less max(4 GiB, 1 GiB) is 6 GiB. The machine is the fixture's, so the
/// 7, the 3 and the 6 GiB are the same on a PC of 24 threads and 47 GiB.
#[test]
fn the_admission_counts_with_the_machine_the_plan_measured() {
    // (nproc, cpu.max, memory.max of the cgroup, MemAvailable in GiB, C, R in GiB)
    let cases = [
        ("7", "max 100000", MEM_OPEN, 10, 7, 6),
        ("7", "300000 100000", MEM_OPEN, 10, 3, 6),
        ("2", "300000 100000", "32212254720", 10, 2, 6),
    ];
    for (nproc, cpu_max, mem_max, mem_gib, cores, ram_gib) in cases {
        let mut fx = Fixture::new("e7a-admision", cpu_max, mem_max);
        fx.table(&[(10, "a", "-", "0.1", "")]);
        let host = machine(&fx, "admision", nproc, mem_gib * KIB_PER_GIB);
        let before = snapshot(&fx.history, "corridas.jsonl");
        let run = fx.run_env("serie", &host.env());
        assert_green(&run, "a green table on a machine the test describes");
        let runs = appended(&fx.history, "corridas.jsonl", &before);
        let record = runs
            .first()
            .unwrap_or_else(|| panic!("no run line: {}{}", run.out, run.err));
        let want = format!("{nproc} cores, {cpu_max}, memory.max {mem_max}, {mem_gib} GiB free");
        assert_eq!(record["C"].as_u64(), Some(cores), "C with {want}: {record}");
        assert_eq!(
            record["R"].as_u64(),
            Some(ram_gib * GIB),
            "R with {want}: {record}"
        );
    }
}

/// Spec K4-B, D-K4o: a measure that fails is not a refusal. The planner runs in series, with one job, and says what it
/// could not measure, in the plan line and in the run's own record. The history is there and the mode is auto, so it
/// is the failure and not the lack of history that puts it in series. Three ways to fail: a cgroup file that is not a
/// number, a MemAvailable that is not there, and a `nproc` that is not a number.
#[test]
fn a_measure_that_fails_falls_back_to_series_and_says_so() {
    let seeded = |name: &str| {
        let mut fx = Fixture::new(name, CPU_FOUR, MEM_OPEN);
        fx.table(&[(10, "a", "-", "0.2", ""), (20, "b", "-", "0.2", "")]);
        fx.seed();
        fx
    };
    let check = |fx: &Fixture, env: &[(&str, &str)], why: &str| {
        let before_steps = snapshot(&fx.history, "pasos.jsonl");
        let before_runs = snapshot(&fx.history, "corridas.jsonl");
        let run = fx.run_env("auto", env);
        assert_green(&run, why);
        let line = plan_line_of(&run);
        assert!(line.contains(" runs=serie"), "{why}: {line}");
        assert!(line.contains(" jobs=1 (failed)"), "{why}: {line}");
        let failed = line
            .split(" failed=")
            .nth(1)
            .unwrap_or_else(|| panic!("{why}: the plan line says nothing failed: {line}"));
        assert!(failed.contains(why), "{why}: `{failed}` does not name it");
        let steps = appended(&fx.history, "pasos.jsonl", &before_steps);
        assert!(
            !overlaps(by_id(&steps, "a"), by_id(&steps, "b")),
            "{why}: the rows ran together although the measure failed"
        );
        let runs = appended(&fx.history, "corridas.jsonl", &before_runs);
        let record = runs.first().expect("the run line");
        assert_eq!(record["modo"].as_str(), Some("serie"), "{why}: {record}");
        assert_eq!(record["plan"]["jobs"].as_u64(), Some(1), "{why}: {record}");
        assert!(
            record["plan"]["fallo"]
                .as_str()
                .is_some_and(|text| text.contains(why)),
            "{why}: plan.fallo does not say it: {record}"
        );
    };
    let fx = seeded("k4-falla-cgroup");
    write(&fx.cgroups.join("scope").join("memory.max"), "basura");
    check(&fx, &[], "memory.max");
    let fx = seeded("k4-falla-memoria");
    let without = fx.base.join("sin-memavailable");
    write(
        &without,
        "MemTotal:       49327104 kB\nMemFree:         1048576 kB\n",
    );
    let without = without.display().to_string();
    check(
        &fx,
        &[("GATE_PROC_MEMINFO", without.as_str())],
        "MemAvailable",
    );
    let fx = seeded("k4-falla-nproc");
    let broken = machine(&fx, "roto", "x", 40 * KIB_PER_GIB);
    check(&fx, &broken.env(), "nproc");
}

/// A step of the barrier of E7a.5. It marks that it entered (`in/<id>`, never removed during a run) and does not leave until
/// `want` rows have entered, or until `ticks` x 0.1 s pass (both are in the control directory; absent, 0, for the run that
/// seeds the history). Nobody leaves before `want` have entered, so when the `want`-th enters the `want` are alive together,
/// whatever the load of the machine; a row that enters after the others left finds `want` marks already and goes on. Before
/// leaving it writes how many rows were alive (`in` less `out`, read in that order, so it never counts more than are) to
/// `counts`, and then its mark in `out/<id>`. The deadline only runs out if the planner could not start the rows that
/// the barrier waits for. $1 id, $2 unused, $3 control directory.
const BARRIER_SH: &str = r#"#!/bin/sh
id="$1"
control="$3"
mkdir -p "$control/in" "$control/out"
echo "=== $id banner"
: > "$control/in/$id"
want=$(cat "$control/want" 2>/dev/null || echo 0)
limit=$(cat "$control/ticks" 2>/dev/null || echo 0)
i=0
while [ "$(ls "$control/in" | wc -l | tr -d ' ')" -lt "$want" ] && [ "$i" -lt "$limit" ]; do
    sleep 0.1
    i=$((i + 1))
done
entered=$(ls "$control/in" | wc -l | tr -d ' ')
left=$(ls "$control/out" | wc -l | tr -d ' ')
echo $((entered - left)) >> "$control/counts"
: > "$control/out/$id"
echo "Running unittests fake $id"
echo "test result: ok. 1 passed; 0 failed"
"#;

/// Spec E7a.5 (D-E7c, D-E7e): the jobs of the rows alive never add up past the jobs of the machine, and `auto` and `maximo`
/// are one DAG. Three rows with no dependency and no shared resource, each naming 8 jobs, with almost no CPU or RAM in the
/// history, so that only the jobs can hold one back. With J = 16 two fit (8 + 8) and the third waits with `jobs`, in
/// `auto` and in `maximo`; with J = 24 the three are inside together and none waits for `lanes` or `jobs`. The barrier is
/// a directory of marks that no row leaves before `want` have entered, with a deadline of 20 s that runs out only if the
/// planner cannot start the rows, so what the test says does not depend on how long a row takes to start. Where only two
/// fit, nobody can see the third not coming in by waiting for it: the planner says it, with the reason `jobs` of the row
/// that waited and the instants of its records (the last row to start started after the first one to end).
#[test]
fn the_jobs_of_the_rows_alive_never_pass_the_jobs_of_the_machine_and_auto_is_maximo() {
    // 20 s of barrier, under the 30 s `run_command` gives a planner: it is spent only if the rows the barrier waits for cannot
    // be inside together, which is what a red of this test is. `want` is how many rows must be inside together to leave.
    let drive = |name: &str, cpu_max: &str, nproc: &str, mem_gib: u64, want: &str| {
        let mut fx = Fixture::new(name, cpu_max, MEM_OPEN);
        let script = fx.base.join("barrera.sh");
        write(&script, BARRIER_SH);
        let rows = ["a", "b", "c"]
            .into_iter()
            .zip([10, 20, 30])
            .map(|(id, idx)| StepRow {
                script: script.clone(),
                env: "CARGO_BUILD_JOBS=8",
                ..fx.row(idx, id, "-", "1")
            })
            .collect::<Vec<_>>();
        table_of(&mut fx, &rows);
        for id in ["a", "b", "c"] {
            fx.usage(id, 10_000);
        }
        fx.seed();
        write(&fx.control.join("want"), want);
        write(&fx.control.join("ticks"), "200");
        let host = machine(&fx, "jobs-vivos", nproc, mem_gib * KIB_PER_GIB);
        (fx, host)
    };
    let go = |fx: &Fixture, host: &Machine, mode: &str| -> (u32, Vec<Value>, String) {
        let _ = fs::remove_file(fx.control.join("counts"));
        for name in ["in", "out"] {
            let _ = fs::remove_dir_all(fx.control.join(name));
        }
        let before = snapshot(&fx.history, "pasos.jsonl");
        let run = fx.run_env(mode, &host.env());
        assert_green(&run, mode);
        let most_inside = read(&fx.control.join("counts"))
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .max()
            .unwrap_or(0);
        let steps = appended(&fx.history, "pasos.jsonl", &before);
        assert_eq!(
            steps.len(),
            3,
            "{mode}: the three rows ran green: {steps:?}"
        );
        (most_inside, steps, plan_line_of(&run))
    };
    let reasons = |steps: &[Value]| -> Vec<String> {
        steps
            .iter()
            .filter_map(|record| record["motivo"].as_str().map(str::to_string))
            .collect()
    };

    let (fx, host) = drive("e7a-jobs-16", "1600000 100000", "16", 40, "2");
    for mode in ["auto", "maximo"] {
        let (inside, steps, line) = go(&fx, &host, mode);
        assert!(
            line.contains(&format!(" jobs=16 (gate) asked={mode} runs=dag")),
            "{mode}: 16 threads and 40 GiB are 16 jobs and the history lets the DAG run: {line}"
        );
        assert_eq!(
            inside,
            2,
            "{mode} with J = 16: two rows of 8 jobs are inside together and never three (the counts each row saw: {})",
            read(&fx.control.join("counts"))
        );
        let why = reasons(&steps);
        assert!(
            why.iter().any(|reason| reason == "jobs"),
            "{mode}: no row waited with the reason `jobs`: {steps:?}"
        );
        assert!(
            why.iter().all(|reason| reason != "lanes"),
            "{mode}: there are no lanes to wait for: {steps:?}"
        );
        let first_end = steps
            .iter()
            .map(|record| num(record, "fin"))
            .fold(f64::MAX, f64::min);
        let last_start = steps
            .iter()
            .map(|record| num(record, "inicio"))
            .fold(f64::MIN, f64::max);
        assert!(
            last_start >= first_end,
            "{mode}: the three rows were alive at once, the last started at {last_start} and the first ended at {first_end}: {steps:?}"
        );
        let overlapped = steps
            .iter()
            .filter(|record| record["solape_s"].as_f64().is_some_and(|s| s > 0.0))
            .count();
        assert!(
            overlapped >= 2,
            "{mode}: two rows were inside together and their solape_s is above 0 ({overlapped} are): {steps:?}"
        );
    }

    let (fx, host) = drive("e7a-jobs-24", "max 100000", "24", 64, "3");
    let (inside, steps, line) = go(&fx, &host, "auto");
    assert!(
        line.contains(" jobs=24 (gate) asked=auto runs=dag"),
        "24 threads and 64 GiB are 24 jobs: {line}"
    );
    assert_eq!(
        inside,
        3,
        "with J = 24 the three rows of 8 jobs are inside together (the counts each row saw: {})",
        read(&fx.control.join("counts"))
    );
    assert!(
        reasons(&steps)
            .iter()
            .all(|reason| reason != "lanes" && reason != "jobs"),
        "with J = 24 no row waits for lanes or jobs: {steps:?}"
    );
}

/// Spec E7a.4 (D-E7d): a row that names its jobs (12) gets min(12, J) in every mode: the number it names is its ceiling,
/// and `maximo` does not leave it above the jobs of the machine. J = 16 on a machine of 16 threads and 40 GiB (room for
/// 20), J = 7 on one of 7 threads. An operator that names 1 job leaves the row 1.
#[test]
fn a_row_that_names_its_jobs_gets_the_lower_of_them_and_the_jobs_of_the_machine_in_every_mode() {
    let mut fx = Fixture::new("k4-reparto", "max 100000", MEM_OPEN);
    fx.table(&[(10, "w", "-", "0.2", "CARGO_BUILD_JOBS=12")]);
    let seen = |fx: &Fixture| -> String {
        read(&fx.control.join("env-w.txt"))
            .lines()
            .find_map(|line| line.strip_prefix("CARGO_BUILD_JOBS=").map(str::to_string))
            .unwrap_or_else(|| "unset".to_string())
    };
    let wide = machine(&fx, "ancha", "16", 40 * KIB_PER_GIB);
    let narrow = machine(&fx, "estrecha", "7", 40 * KIB_PER_GIB);
    for (host, jobs) in [(&wide, "12"), (&narrow, "7")] {
        let run = fx.run_env("serie", &host.env());
        assert_green(&run, "serie");
        assert_eq!(seen(&fx), jobs, "serie: min(12, J)");
        for mode in ["auto", "maximo"] {
            let run = fx.run_env(mode, &host.env());
            assert_green(&run, mode);
            assert!(
                plan_line_of(&run).contains(&format!(" asked={mode} runs=dag")),
                "{}",
                run.out
            );
            assert_eq!(seen(&fx), jobs, "{mode}: min(12, J)");
        }
    }
    let mut one = wide.env();
    one.push(("CARGO_BUILD_JOBS", "1"));
    let run = fx.run_env("auto", &one);
    assert_green(&run, "auto with the 1 job of the operator");
    assert_eq!(seen(&fx), "1", "auto: min(12, 1)");
}

/// Spec K4-B, D-K4n, D-F3x, E7a.6 (D-E7c): 0 and serie are the series, 1, maximo and auto are the DAG, and the plan says
/// what was asked and has no lanes. Anything else is exit 2 with `GATE_DAG is`, and no row starts. Without GATE_DAG the
/// default is the series (the default is `serie`, not `auto`: the DAG is activated in a commit of its own).
#[test]
fn gate_dag_takes_serie_auto_maximo_and_their_numbers_and_nothing_else() {
    let mut fx = Fixture::new("k4-valores", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "s10", "-", "0.3", ""), (20, "s20", "-", "0.3", "")]);
    fx.seed();
    for (value, asked, runs) in [
        ("0", "serie", "serie"),
        ("serie", "serie", "serie"),
        ("1", "maximo", "dag"),
        ("maximo", "maximo", "dag"),
        ("auto", "auto", "dag"),
        (UNSET, "serie", "serie"),
    ] {
        let run = fx.run(value);
        assert_green(&run, value);
        let line = plan_line_of(&run);
        assert!(
            line.contains(&format!(" asked={asked} runs={runs}")),
            "GATE_DAG={value}: {line}"
        );
        assert!(
            !line.contains("lanes="),
            "GATE_DAG={value}: the plan has no lanes: {line}"
        );
    }
    for bad in ["2", "Serie", "dag"] {
        let mut fx = Fixture::new("k4-valor-malo", CPU_FOUR, MEM_OPEN);
        fx.table(&[(10, "s10", "-", "0.1", "")]);
        let run = fx.run(bad);
        assert_eq!(run.code, Some(2), "GATE_DAG={bad}: {}{}", run.out, run.err);
        assert!(
            run.err.contains("GATE_DAG is"),
            "GATE_DAG={bad}: the refusal does not say `GATE_DAG is`: {}",
            run.err
        );
        assert!(
            fx.started().is_empty(),
            "GATE_DAG={bad}: a row started: {:?}",
            fx.started()
        );
    }
}

/// Spec K4-B, E7a-V (the verdict does not depend on the machine): a table with a red in the row of idx 30, between greens,
/// gives in `serie` and in `auto` on a machine of 4 threads and on one of 24 the same exit, the same `FAIL: <id> exited`
/// and the same blocks for the rows before the red, and each row up to the red prints its `Running` block once in the
/// three. The rows after the red are not compared: where the DAG started them before the red ended, they were stopped
/// or they finished, and that depends on the clock. The machines do differ: the plan says 4 jobs and 24.
#[test]
fn the_same_red_is_reported_in_series_and_on_a_narrow_and_a_wide_machine() {
    let mut fx = Fixture::new("k4-veredicto", "max 100000", MEM_OPEN);
    fx.table(&[
        (10, "s10", "-", "0.3", ""),
        (20, "s20", "-", "0.3", ""),
        (30, "s30", "-", "0.3", ""),
        (40, "s40", "-", "0.3", ""),
        (50, "s50", "-", "0.3", ""),
    ]);
    fx.seed();
    fx.rc("s30", "3");
    let narrow = machine(&fx, "estrecha", "4", 40 * KIB_PER_GIB);
    let wide = machine(&fx, "ancha", "24", 64 * KIB_PER_GIB);
    let serie = fx.run_env("serie", &narrow.env());
    let few = fx.run_env("auto", &narrow.env());
    let many = fx.run_env("auto", &wide.env());
    for (run, jobs) in [(&few, "4"), (&many, "24")] {
        assert!(
            plan_line_of(run).contains(&format!(" jobs={jobs} (gate) asked=auto runs=dag")),
            "the run of {jobs} jobs: {}",
            run.out
        );
    }
    let before_the_red = |run: &Run| -> String {
        let kept: Vec<&str> = run
            .out
            .lines()
            .filter(|line| {
                !line.starts_with("dag:")
                    && !line.starts_with("note:")
                    && !line.starts_with("plan:")
            })
            .collect();
        let red_at = kept
            .iter()
            .position(|line| *line == "=== s30 banner")
            .unwrap_or_else(|| panic!("no block of s30: {}", run.out));
        kept[..red_at].join("\n")
    };
    for (label, run) in [
        ("serie", &serie),
        ("4 threads", &few),
        ("24 threads", &many),
    ] {
        assert!(!run.timed_out, "{label}: did not finish in 30 s");
        assert_eq!(run.code, Some(3), "{label}: {}{}", run.out, run.err);
        assert!(
            run.out.contains("FAIL: s30 exited 3"),
            "{label}: no `FAIL: s30 exited 3`: {}",
            run.out
        );
        for id in ["s10", "s20", "s30"] {
            assert_eq!(
                run.out
                    .matches(&format!("Running unittests fake {id}"))
                    .count(),
                1,
                "{label}: the block of {id} is not printed once: {}",
                run.out
            );
        }
    }
    let blocks = before_the_red(&serie);
    assert!(
        blocks.contains("=== s10 banner") && blocks.contains("=== s20 banner"),
        "the blocks before the red are not in the output of the series: {blocks}"
    );
    for (label, run) in [("4 threads", &few), ("24 threads", &many)] {
        assert_eq!(
            blocks,
            before_the_red(run),
            "the blocks before the red differ between the series and {label}"
        );
    }
}

/// Spec K4-B, D-K4r: in series no row overlaps another, so every `solape_s` is 0, and after the line of the summary
/// (`dag: T=`) there is one `dag: step <id> seg=… wait=… overlap=… mem_peak=… cpu_s=…` per row that ran, in idx order.
#[test]
fn in_series_nothing_overlaps_and_each_row_gets_its_line_after_the_summary() {
    let mut fx = Fixture::new("k4-solape-serie", CPU_FOUR, MEM_OPEN);
    fx.table(&[
        (10, "s10", "-", "0.1", ""),
        (20, "s20", "-", "0.1", ""),
        (30, "s30", "-", "0.1", ""),
    ]);
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("0");
    assert_green(&run, "series");
    let steps = appended(&fx.history, "pasos.jsonl", &before);
    assert_eq!(steps.len(), 3, "{steps:?}");
    for record in &steps {
        assert_eq!(
            record["solape_s"].as_f64(),
            Some(0.0),
            "a row of the series overlapped another: {record}"
        );
    }
    let after: Vec<&str> = run
        .out
        .lines()
        .skip_while(|line| !line.starts_with("dag: T="))
        .skip(1)
        .filter(|line| line.starts_with("dag: step "))
        .collect();
    assert_eq!(
        after.len(),
        3,
        "one line per row after the summary: {}",
        run.out
    );
    for (line, id) in after.iter().zip(["s10", "s20", "s30"]) {
        assert!(
            line.starts_with(&format!("dag: step {id} seg=")),
            "`{line}` is not the line of {id}, in idx order"
        );
        for key in [" wait=", " overlap=", " mem_peak=", " cpu_s="] {
            assert!(line.contains(key), "`{line}` lacks{key}");
        }
        assert_eq!(
            word_after(line, "overlap=").parse::<f64>().ok(),
            Some(0.0),
            "`{line}`"
        );
    }
}

// --- CI-E4: CPU, cores, the peak split in what it is made of, and the seconds in series ----------------------------------
// D-E4b to D-E4m. Every step is a bash script that writes into the fake scope what the real kernel would hold while a
// step runs, so a run has numbers to read and the contract knows each one by hand. The history and the scope are the
// fixture's; nothing here touches the real cgroup (CI-J17).

/// The step of CI-E4. $1 id, $2 seconds, $3 control directory, $4 the fake scope, $5 the fake /proc/self/cgroup.
/// `p` only records the first five bytes of `memory.peak` when it starts: `reset` if the planner zeroed it, `99999`
/// if it did not. `a` records them too and then plays what a step does to a scope: it holds 2 GiB (its `memory.stat` is
/// written first and replaced whole, so a read in between never sees the new `current` with the old stat), writes its
/// peak where the planner's descriptor sees it (same inode, `>`), puts another file in the path (what a new `open` would
/// read: the peak since the scope was born) and rewrites /proc/self/cgroup as the kernel does after the planner moves
/// itself under `juez`. `b` holds 1 GiB and, when `control/b-after-a` exists, waits for `control/done-a` so that it ends
/// after `a` whatever the load of the machine.
const MEDIDA_SH: &str = r#"#!/bin/bash
id="$1"; secs="$2"; control="$3"; scope="$4"; self_cgroup="$5"
trap 'echo done > "$control/done-$id"' EXIT
put() { printf "$2" > "$1.new" && mv "$1.new" "$1"; }
case "$id" in
p)
    head -c 5 "$scope/memory.peak" > "$control/peak-reset-p"
    ;;
a)
    head -c 5 "$scope/memory.peak" > "$control/peak-reset"
    put "$scope/memory.stat" 'anon 1073741824\nfile 805306368\nshmem 268435456\nkernel 268435456\n'
    put "$scope/memory.current" '2147483648'
    printf '3221225472\n' > "$scope/memory.peak"
    put "$scope/memory.peak" '999999999999\n'
    printf '0::/scope/juez\n' > "$self_cgroup"
    ;;
b)
    put "$scope/memory.stat" 'anon 536870912\nfile 268435456\nshmem 0\nkernel 268435456\n'
    put "$scope/memory.current" '1073741824'
    if [ -e "$control/b-after-a" ]; then
        for _ in $(seq 400); do [ -e "$control/done-a" ] && break; sleep 0.05; done
    fi
    ;;
esac
sleep "$secs"
"#;

/// The table of CI-E4: `p` (idx 5, 0.1 s), `a` (idx 10, 0.3 s) and `b` (idx 20, 0.3 s) after it, a delegated scope of 4 CPUs
/// whose `usage_usec` are 0.1, 0.6 and 0.3 CPU-seconds and whose `memory.peak` is 2 MiB, and in the scope the files a peak needs: `memory.peak` of
/// 999999999999 (the peak since the scope was born), no memory held with a `memory.stat` that has the four fields, and
/// a `juez` with other numbers (1 MiB), which is what a planner that looked the scope up again after delegating would read.
fn e4_fixture(name: &str) -> Fixture {
    let mut fx = Fixture::new(name, CPU_FOUR, MEM_OPEN);
    let script = fx.base.join("medida.sh");
    write(&script, MEDIDA_SH);
    let scope = fx.cgroups.join("scope");
    let mut text = String::new();
    for (idx, id, deps, secs) in [
        (5, "p", "-", "0.1"),
        (10, "a", "p", "0.3"),
        (20, "b", "p", "0.3"),
    ] {
        text.push_str(&format!(
            "{idx} {id} {deps} - normal 1 5 {secs} /bin/bash {} {id} {secs} {} {} {}\n",
            script.display(),
            fx.control.display(),
            scope.display(),
            fx.proc_self.display()
        ));
        fx.steps.push((idx, id.to_string()));
    }
    write(&fx.table, &text);
    for (_, id) in &fx.steps {
        fx.write_defaults(id);
        fx.write_step(id, "memory.peak", "2097152\n");
    }
    for (id, usec) in [("p", 100_000), ("a", 600_000), ("b", 300_000)] {
        fx.usage(id, usec);
    }
    write(&scope.join("memory.peak"), "999999999999\n");
    write(&scope.join("memory.current"), "0");
    write(
        &scope.join("memory.stat"),
        "anon 0\nfile 0\nshmem 0\nkernel 0\n",
    );
    let juez = scope.join("juez");
    fs::create_dir_all(&juez).expect("juez dir");
    write(&juez.join("memory.peak"), "1048576");
    write(&juez.join("memory.current"), "1048576");
    write(
        &juez.join("memory.stat"),
        "anon 1048576\nfile 0\nshmem 0\nkernel 0\n",
    );
    fx
}

const E4_STEP_FIELDS: [&str; 4] = ["cpu_s", "nucleos", "mem_peak", "anon_max"];
const E4_RUN_FIELDS: [&str; 6] = [
    "cpu_s",
    "nucleos",
    "mem_peak",
    "mem_muestra",
    "serie_final_s",
    "serie_cola_s",
];

/// D-E4h in both directions: of the fields a record covers, each one that is null has a reason in `sin_medida`, and each
/// reason is of a field that is null and covered.
fn e4_null_iff_reason(record: &Value, covered: &[&str], label: &str) {
    let reasons = record["sin_medida"]
        .as_object()
        .unwrap_or_else(|| panic!("{label}: `sin_medida` is not an object: {record}"));
    for field in covered {
        assert_eq!(
            record[*field].is_null(),
            reasons.contains_key(*field),
            "{label}: `{field}` is {} and has {} reason: {record}",
            record[*field],
            if reasons.contains_key(*field) {
                "a"
            } else {
                "no"
            }
        );
    }
    for key in reasons.keys() {
        assert!(
            covered.contains(&key.as_str()),
            "{label}: `sin_medida` names `{key}`, which is not a field of this record: {record}"
        );
    }
}

/// Runs the table of `fx` with `gate_dag`, green, and returns the run, the three records of its steps and its one line of
/// corridas.jsonl (what the history grew by since this call).
fn e4_run(fx: &Fixture, gate_dag: &str) -> (Run, Vec<Value>, Value) {
    let before_steps = snapshot(&fx.history, "pasos.jsonl");
    let before_runs = snapshot(&fx.history, "corridas.jsonl");
    let run = fx.run(gate_dag);
    assert_green(&run, "CI-E4 run");
    let steps = appended(&fx.history, "pasos.jsonl", &before_steps);
    let mut runs = appended(&fx.history, "corridas.jsonl", &before_runs);
    assert_eq!(steps.len(), 3, "{steps:?}\n{}", run.text());
    assert_eq!(runs.len(), 1, "{runs:?}\n{}", run.text());
    (run, steps, runs.remove(0))
}

fn e4_close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

/// Spec D-E4-1: a delegated scope in series leaves, with no null and no reason, the CPU of each step and of the run, the
/// cores (CPU over the seconds of the step, CPU over the T of the run), the peak of the run read through a descriptor of
/// its own that the planner zeroed before the first step, the sample of the highest `memory.current` split into what it
/// is made of, and the seconds in series: `a` (the second row alive alone while `b` was ready) and `b` (the tail). `p`
/// does not count: nothing else was ready while it ran.
#[test]
fn a_run_leaves_cpu_cores_the_split_peak_and_the_seconds_in_series_with_no_null_e4() {
    let fx = e4_fixture("e4-completa");
    let (run, steps, record) = e4_run(&fx, "0");
    for id in ["peak-reset", "peak-reset-p"] {
        assert_eq!(
            read(&fx.control.join(id)),
            "reset",
            "{id}: the peak of the scope was not zeroed before the first step started"
        );
    }
    for (id, cpu) in [("p", 0.1), ("a", 0.6), ("b", 0.3)] {
        let step = by_id(&steps, id);
        assert!(e4_close(num(step, "cpu_s"), cpu, 1e-9), "{id}: {step}");
        assert!(
            e4_close(num(step, "nucleos") * num(step, "seg"), cpu, 0.005),
            "{id}: cores times seconds is the CPU: {step}"
        );
        assert_eq!(step["sin_medida"], json!({}), "{id}: {step}");
        e4_null_iff_reason(step, &E4_STEP_FIELDS, id);
    }
    assert!(e4_close(num(&record, "cpu_s"), 1.0, 1e-9), "{record}");
    assert!(
        e4_close(num(&record, "nucleos") * num(&record, "T"), 1.0, 0.005),
        "the cores of the run are its CPU over its T: {record}"
    );
    assert_eq!(record["sin_medida"], json!({}), "{record}");
    e4_null_iff_reason(&record, &E4_RUN_FIELDS, "run");
    assert_eq!(
        record["mem_peak"].as_u64(),
        Some(3_221_225_472),
        "the peak is what the descriptor of the planner reads (3 GiB), not what a path says now (999999999999): {record}"
    );
    let muestra = &record["mem_muestra"];
    for (key, bytes) in [
        ("current", 2_147_483_648_u64),
        ("anon", 1_073_741_824),
        ("file", 805_306_368),
        ("shmem", 268_435_456),
        ("kernel", 268_435_456),
    ] {
        assert_eq!(
            muestra[key].as_u64(),
            Some(bytes),
            "the sample of the highest memory.current has {key} = {bytes}: {record}"
        );
    }
    let (a, b) = (by_id(&steps, "a"), by_id(&steps, "b"));
    let t = num(muestra, "t");
    assert!(
        num(a, "inicio") <= t && t <= num(b, "inicio"),
        "the highest sample is the one of a step `a` holding 2 GiB, between its start and the start of b: {muestra}"
    );
    assert!(
        muestra["muestras"].as_u64().is_some_and(|n| n >= 5),
        "at the start, at the end of p, a and b, and at the close: {muestra}"
    );
    assert!(
        e4_close(
            num(&record, "serie_final_s"),
            num(a, "seg") + num(b, "seg"),
            0.002
        ),
        "the seconds in series are those of a and b: {record}"
    );
    assert!(
        e4_close(num(&record, "serie_cola_s"), num(b, "seg"), 0.002),
        "the tail is b alone: {record}"
    );
    let lines: Vec<&str> = run.out.lines().collect();
    let t_at = lines
        .iter()
        .position(|l| l.starts_with("dag: T="))
        .unwrap_or_else(|| panic!("no summary line: {}", run.out));
    let resources = lines.get(t_at + 1).copied().unwrap_or("");
    assert!(
        resources.starts_with("dag: resources:"),
        "the line `dag: resources:` goes right after `dag: T=`: {resources:?}"
    );
    let words: Vec<&str> = resources.split_whitespace().collect();
    for want in [
        "cpu_s=1.00",
        "mem_peak=3.00",
        "current=2.00",
        "anon=1.00",
        "shmem=0.25",
        "cache=0.50",
        "kernel=0.25",
    ] {
        assert!(words.contains(&want), "`{resources}` lacks {want}");
    }
    for key in [
        "cores=",
        "at_t=",
        "samples=",
        "serie_final=",
        "tail=",
        "serie_share=",
    ] {
        assert!(
            words
                .iter()
                .any(|w| w.starts_with(key) && !w.ends_with("n/a")),
            "`{resources}` lacks a measured {key}"
        );
    }
    let step_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| l.starts_with("dag: step "))
        .collect();
    assert_eq!(step_lines.len(), 3, "{}", run.out);
    for line in step_lines {
        assert!(line.contains(" cores="), "`{line}` lacks cores=");
    }
    assert!(
        !lines.iter().any(|l| l.starts_with("note: not measured")),
        "everything was measured, so there is no note: {}",
        run.out
    );
}

/// Spec D-E4-2: in a DAG, `a` and `b` enter together and `b` ends after `a` (it waits for `done-a`, so the order does not
/// depend on the load). The only second in series before the tail is the one in which the first row of the pair has
/// started and the second has not: the spec says "entered together", and the gap is the start of one row after the
/// other, which the records give as the difference of their `inicio`. The tail is what `b` does alone after `a`.
#[test]
fn in_a_dag_the_rows_that_enter_together_leave_only_the_tail_in_series_e4() {
    let fx = e4_fixture("e4-dag");
    fx.usage("a", 100_000);
    fx.usage("b", 100_000);
    write(&fx.control.join("b-after-a"), "");
    fx.seed();
    // The seeding run is over: `a` left /proc/self/cgroup under `juez` and `done-a` behind, and the DAG run starts as if it
    // were a new one, in the scope and with no mark of the first.
    write(&fx.proc_self, "0::/scope\n");
    fs::remove_file(fx.control.join("done-a")).expect("done-a of the seeding run");
    let (_, steps, record) = e4_run(&fx, "1");
    assert_eq!(record["modo"], json!("dag"), "{record}");
    let (a, b) = (by_id(&steps, "a"), by_id(&steps, "b"));
    assert!(overlaps(a, b), "a and b did not run together: {a} {b}");
    assert!(
        num(b, "fin") > num(a, "fin"),
        "b has to end after a: {a} {b}"
    );
    let tail = num(b, "fin") - num(a, "fin");
    assert!(
        e4_close(num(&record, "serie_cola_s"), tail, 0.002),
        "the tail is what b does alone after a: {tail} {record}"
    );
    let gap = (num(a, "inicio") - num(b, "inicio")).abs();
    let wait = num(&record, "serie_final_s") - num(&record, "serie_cola_s");
    assert!(
        e4_close(wait, gap, 0.002),
        "nobody waited but the row that was admitted second, for the {gap} s it started after the first: {record}"
    );
    assert!(
        wait < 0.25,
        "two rows that enter together do not spend seconds in series before the tail: {record}"
    );
}

/// Spec D-E4-3: a scope that is not delegated is not measured and the planner does not write a byte in it. In series: the
/// note says it with its reason, every number of the scope is null with a reason that says `not delegated`, the seconds
/// in series are still there (they come from the intervals, not from the cgroup), and one `note: not measured (` line
/// names the fields. The first five bytes `p` and `a` read from `memory.peak` are `99999`: no `reset` was written.
#[test]
fn an_undelegated_scope_is_not_measured_says_so_in_series_and_is_not_touched_e4() {
    let fx = e4_fixture("e4-sin-delegar");
    fx.undelegate();
    let (run, steps, record) = e4_run(&fx, "0");
    assert!(
        run.out.contains("note: the scope is not delegated ("),
        "the note of a scope that is not delegated goes out in series too, with its reason: {}",
        run.out
    );
    assert!(
        run.out
            .lines()
            .any(|l| l.starts_with("note: not measured (")),
        "one `note: not measured (` line names the fields: {}",
        run.out
    );
    for step in &steps {
        for field in E4_STEP_FIELDS {
            assert!(step[field].is_null(), "{field} of a step: {step}");
            assert!(
                step["sin_medida"][field]
                    .as_str()
                    .is_some_and(|why| why.contains("not delegated")),
                "{field} of a step has the reason `not delegated`: {step}"
            );
        }
        e4_null_iff_reason(step, &E4_STEP_FIELDS, "undelegated step");
    }
    for field in ["cpu_s", "nucleos", "mem_peak", "mem_muestra"] {
        assert!(record[field].is_null(), "{field} of the run: {record}");
        assert!(
            record["sin_medida"][field].is_string(),
            "{field} of the run has a reason: {record}"
        );
    }
    for field in ["serie_final_s", "serie_cola_s"] {
        assert!(
            record[field].as_f64().is_some(),
            "{field} comes from the intervals and is measured: {record}"
        );
    }
    e4_null_iff_reason(&record, &E4_RUN_FIELDS, "undelegated run");
    for id in ["peak-reset", "peak-reset-p"] {
        assert_eq!(
            read(&fx.control.join(id)),
            "99999",
            "{id}: the planner wrote in the memory.peak of a scope that is not its own"
        );
    }
    assert_eq!(
        read(&fx.cgroups.join("scope").join("memory.peak")),
        "999999999999\n"
    );
}

/// Spec D-E4-4: what cannot be read is null with its path, never 0. The `cpu.stat` of `b` is gone and `memory.peak` cannot
/// be opened for writing: `b` has no CPU and no cores and the reason names its file, `a` is measured, the run has no CPU
/// and no cores (the reason names `b`) and no peak (`cannot be reset`: it does not fall back to reading the path, which
/// would give the peak since the scope was born), and the sample is still there, because it does not need the descriptor.
#[test]
fn a_file_that_cannot_be_read_is_null_with_its_path_never_0_e4() {
    let fx = e4_fixture("e4-no-lee");
    fs::remove_file(fx.step_dir("b").join("cpu.stat")).expect("cpu.stat of b");
    let peak = fx.cgroups.join("scope").join("memory.peak");
    fs::set_permissions(&peak, fs::Permissions::from_mode(0o444)).expect("memory.peak 0444");
    assert!(
        fs::OpenOptions::new().write(true).open(&peak).is_err(),
        "this test needs a user that file permissions bind, not root"
    );
    let (run, steps, record) = e4_run(&fx, "0");
    let b = by_id(&steps, "b");
    assert!(b["cpu_s"].is_null() && b["nucleos"].is_null(), "{b}");
    assert!(
        b["sin_medida"]["cpu_s"]
            .as_str()
            .is_some_and(|why| why.contains("paso-20-b/cpu.stat")),
        "the reason names the file of b: {b}"
    );
    let a = by_id(&steps, "a");
    assert!(e4_close(num(a, "cpu_s"), 0.6, 1e-9), "{a}");
    assert!(
        record["cpu_s"].is_null() && record["nucleos"].is_null(),
        "{record}"
    );
    assert!(
        record["sin_medida"]["cpu_s"]
            .as_str()
            .is_some_and(|why| why.contains('b')),
        "the reason of the run names the step that was not measured: {record}"
    );
    assert!(
        record["mem_peak"].is_null(),
        "the peak of a scope that cannot be zeroed is not read from its path: {record}"
    );
    assert!(
        record["sin_medida"]["mem_peak"]
            .as_str()
            .is_some_and(|why| why.contains("cannot be reset")),
        "{record}"
    );
    assert!(
        !record["mem_muestra"].is_null(),
        "the sample does not depend on the descriptor of the peak: {record}"
    );
    for step in &steps {
        e4_null_iff_reason(step, &E4_STEP_FIELDS, "step");
    }
    e4_null_iff_reason(&record, &E4_RUN_FIELDS, "run");
    assert!(
        run.out
            .lines()
            .any(|l| l.starts_with("note: not measured (") && l.contains("cannot be reset")),
        "a `note: not measured (` line for the peak: {}",
        run.out
    );
}

/// (tag, the power supply the root holds as (type, status), the `battery=` the plan has to say), per case of the battery.
type BatteryCase<'a> = (&'a str, Option<(&'a str, &'a str)>, &'a str);

/// Spec K4-B, D-K4p: the battery and the disk are measured, printed and saved, and change nothing. A battery that
/// discharges says `discharging`, one that does not says `ac`, a root with no battery or none at all says `none`, a
/// `type` that cannot be read says `unknown`. In every case the jobs and the mode are those of the machine without a
/// battery (16 jobs: 24 cores under a cgroup of 16 CPUs, with 20 GiB of room), and the plan has no `failed=` and no lanes.
#[test]
fn the_battery_is_measured_and_changes_nothing() {
    let cases: [BatteryCase; 5] = [
        ("sin-ninguna", None, "none"),
        (
            "descargando",
            Some(("Battery", "Discharging")),
            "discharging",
        ),
        ("cargando", Some(("Battery", "Charging")), "ac"),
        ("solo-red", Some(("Mains", "")), "none"),
        ("type-ilegible", Some(("", "")), "unknown"),
    ];
    let mut tails = Vec::new();
    for (tag, supply, want) in cases {
        let mut fx = Fixture::new("k4-bateria", "1600000 100000", MEM_OPEN);
        fx.table(&[(10, "a", "-", "0.1", "")]);
        let mut host = machine(&fx, tag, "24", 40 * KIB_PER_GIB);
        if let Some((kind, status)) = supply {
            let root = fx.base.join("power_supply");
            let unit = root.join("BAT0");
            fs::create_dir_all(&unit).expect("a power supply");
            if kind.is_empty() {
                fs::create_dir_all(unit.join("type")).expect("a type that is a directory");
            } else {
                write(&unit.join("type"), &format!("{kind}\n"));
            }
            if !status.is_empty() {
                write(&unit.join("status"), &format!("{status}\n"));
            }
            host.power = root.display().to_string();
        }
        let run = fx.run_env("0", &host.env());
        assert_green(&run, tag);
        let line = plan_line_of(&run);
        assert_eq!(word_after(&line, "battery="), want, "{tag}: {line}");
        assert!(
            !line.contains(" failed="),
            "{tag}: a battery that cannot be read is not a failed measure: {line}"
        );
        tails.push((tag, line.split(" jobs=").nth(1).unwrap_or("").to_string()));
    }
    let (first_tag, first) = &tails[0];
    for (tag, tail) in &tails {
        assert_eq!(
            tail, first,
            "{tag} gave other jobs or mode than {first_tag}: the battery only gets measured"
        );
    }
    assert_eq!(
        first, "16 (gate) asked=serie runs=serie",
        "the jobs and the mode of the plan of a machine of 24 threads under 16 CPUs, with no lanes"
    );
}

/// A step that builds: it leaves four reports in the cargo-timings directory of the target the planner gave it (4, 6, 2
/// and 2 bytes: two that its log will name, one it will not, and the copy of the last one that cargo keeps) and says in
/// its log which it saved, as cargo does on stderr: the lines are in `saved-<id>.txt` of the control directory, with
/// `@D@` for the directory. $1 id, $2 seconds, $3 control directory.
const REPORTS_SH: &str = r#"#!/bin/sh
id="$1"
control="$3"
d="$CARGO_TARGET_DIR/cargo-timings"
mkdir -p "$d"
echo "$d" > "$control/timings-dir-$id"
echo "=== $id banner"
printf 'AAAA' > "$d/cargo-timing-20261109T120000Z.html"
printf 'BBBBBB' > "$d/cargo-timing-20261109T120001Z.html"
printf 'CC' > "$d/cargo-timing-20261109T120002Z.html"
printf 'DD' > "$d/cargo-timing.html"
if [ -f "$control/saved-$id.txt" ]; then
    sed "s|@D@|$d|g" "$control/saved-$id.txt" >&2
fi
echo "Running unittests fake $id"
echo "test result: ok. 1 passed; 0 failed"
"#;

const SAVED_TWO: &str = "   Compiling fake v0.0.0\n   Timing report saved to @D@/cargo-timing-20261109T120000Z.html\n   Timing report saved to @D@/cargo-timing-20261109T120001Z.html\n    Finished `dev` profile\n";
const REPORT_NAMES: [&str; 4] = [
    "cargo-timing-20261109T120000Z.html",
    "cargo-timing-20261109T120001Z.html",
    "cargo-timing-20261109T120002Z.html",
    "cargo-timing.html",
];

/// A one-row table whose row `b` builds in the target T_x and leaves the four reports; `saved` is what its log says.
fn reports_fixture(name: &str, saved: &str) -> Fixture {
    let mut fx = Fixture::new(name, CPU_FOUR, MEM_OPEN);
    let script = fx.base.join("informes.sh");
    write(&script, REPORTS_SH);
    write(&fx.control.join("saved-b.txt"), saved);
    let rows = [StepRow {
        resources: "target:T_x",
        script,
        ..fx.row(10, "b", "", "0.2")
    }];
    table_of(&mut fx, &rows);
    fx
}

/// The cargo-timings directory of the target the row `b` of `reports_fixture` was given.
fn reports_dir(fx: &Fixture) -> PathBuf {
    PathBuf::from(read(&fx.control.join("timings-dir-b")).trim())
}

/// `reports_fixture` run in series with a target root of its own; the record of `b` in pasos.jsonl.
fn run_reports(fx: &Fixture) -> (Run, Value) {
    let root = target_root(fx);
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run_env("0", &[("CARGO_TARGET_DIR", root.as_str())]);
    assert_green(&run, "a row that leaves reports");
    let steps = appended(&fx.history, "pasos.jsonl", &before);
    let record = by_id(&steps, "b").clone();
    (run, record)
}

/// Spec K4-A, D-K4h: the reports a row's log names go from its target to the history, in the folder of its day, its
/// run and its row, and the planner counts them with their exact weight. What the log does not name (a report of another
/// call, cargo-timing.html, the copy of the last one) stays where it was.
#[test]
fn the_reports_a_row_names_move_from_its_target_to_the_history_with_its_row() {
    let fx = reports_fixture("k4-informes", SAVED_TWO);
    let (_, record) = run_reports(&fx);
    let target = reports_dir(&fx);
    let timings = &record["timings"];
    assert!(timings.is_object(), "no timings in the record: {record}");
    let dir = timings["dir"]
        .as_str()
        .unwrap_or_else(|| panic!("timings.dir is not text: {record}"));
    let parts: Vec<&str> = dir.split('/').collect();
    assert_eq!(
        parts.len(),
        4,
        "dir is <day>/timings/<run>/<idx>-<id>, relative to the root of the history: {dir}"
    );
    assert!(
        parts[0].len() == 8 && parts[0].chars().all(|c| c.is_ascii_digit()),
        "{dir}"
    );
    assert_eq!(parts[1], "timings", "{dir}");
    assert_eq!(Some(parts[2]), record["run"].as_str(), "{dir}");
    assert_eq!(parts[3], "10-b", "{dir}");
    let history = fx.history.join("memory-industry");
    for (name, body) in [(REPORT_NAMES[0], "AAAA"), (REPORT_NAMES[1], "BBBBBB")] {
        assert_eq!(
            read(&history.join(dir).join(name)),
            body,
            "{name} is not in the history, with its content"
        );
        assert!(
            !target.join(name).exists(),
            "{name} was copied, not moved: it is still in the target"
        );
    }
    for kept in [REPORT_NAMES[2], REPORT_NAMES[3]] {
        assert!(
            target.join(kept).exists(),
            "{kept} is not named by the log and was taken out of the target"
        );
    }
    assert_eq!(timings["n"].as_u64(), Some(2), "{timings}");
    assert_eq!(timings["bytes"].as_u64(), Some(10), "{timings}");
    assert_eq!(timings["perdidos"].as_u64(), Some(0), "{timings}");
    assert_eq!(timings["faltan"].as_u64(), Some(0), "{timings}");
    assert!(
        history.join(parts[0]).join("pasos.jsonl").is_file(),
        "the reports are not in the folder of the day of the record"
    );
}

/// Spec K4-A, D-K4h: a report named twice counts once and is `perdidos`; one the log names and the target lacks is
/// `faltan`. The row is green and stays green.
#[test]
fn a_report_named_twice_or_missing_is_counted_and_changes_no_verdict() {
    let saved = "   Timing report saved to @D@/cargo-timing-20261109T120000Z.html\n   Timing report saved to @D@/cargo-timing-20261109T120000Z.html\n   Timing report saved to @D@/cargo-timing-20269999T000000Z.html\n";
    let fx = reports_fixture("k4-informes-cuenta", saved);
    let (run, record) = run_reports(&fx);
    assert_eq!(run.code, Some(0), "{}{}", run.out, run.err);
    assert_eq!(record["rc"].as_i64(), Some(0), "{record}");
    let timings = &record["timings"];
    assert_eq!(timings["n"].as_u64(), Some(1), "{record}");
    assert_eq!(timings["perdidos"].as_u64(), Some(1), "{record}");
    assert_eq!(timings["faltan"].as_u64(), Some(1), "{record}");
}

/// Spec K4-A, D-K4h: a row whose log names no report has `timings` null, written as a key and not left out: a row that
/// builds and names none, and a row with no target at all. What a row built stays in its target.
#[test]
fn a_row_that_names_no_report_has_timings_null_and_loses_nothing() {
    let fx = reports_fixture("k4-sin-informes", "");
    let (_, record) = run_reports(&fx);
    assert!(
        record
            .as_object()
            .is_some_and(|fields| fields.get("timings").is_some_and(Value::is_null)),
        "timings has to be a key of the record and null: {record}"
    );
    let target = reports_dir(&fx);
    for name in REPORT_NAMES {
        assert!(
            target.join(name).exists(),
            "{name} was taken out of the target"
        );
    }
    let mut fx = Fixture::new("k4-sin-informes-sin-target", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "q", "-", "0.1", "")]);
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("0");
    assert_green(&run, "a row with no target");
    let steps = appended(&fx.history, "pasos.jsonl", &before);
    assert!(
        by_id(&steps, "q")
            .as_object()
            .is_some_and(|fields| fields.get("timings").is_some_and(Value::is_null)),
        "{steps:?}"
    );
}

/// Spec K4-A, D-K4h: a history that cannot be written does not change the verdict: the planner exits 0, says it once
/// with a `history:` line, and the reports stay in the target. The control is the same row with a history that can be
/// written, which moves them: without it the second half would pass over a planner that never moved anything. The
/// unwritable history is a path under a regular file, which no user can make.
#[test]
fn a_history_that_cannot_be_written_changes_no_verdict_and_leaves_the_reports_in_the_target() {
    let control = reports_fixture("k4-escritura-control", SAVED_TWO);
    run_reports(&control);
    assert!(
        !reports_dir(&control).join(REPORT_NAMES[0]).exists(),
        "control: with a history that can be written the report is not in the target any more"
    );
    let fx = reports_fixture("k4-escritura", SAVED_TWO);
    let blocker = fx.base.join("no-es-un-directorio");
    write(&blocker, "x");
    let unwritable = blocker.join("historial").display().to_string();
    let root = target_root(&fx);
    let run = fx.run_env(
        "0",
        &[
            ("CARGO_TARGET_DIR", root.as_str()),
            ("CI_TIEMPOS_DIR", unwritable.as_str()),
        ],
    );
    assert_green(&run, "a history that cannot be written");
    let said = run
        .err
        .lines()
        .filter(|line| line.starts_with("history:"))
        .count();
    assert_eq!(said, 1, "one `history:` line, no more: {}", run.err);
    let target = reports_dir(&fx);
    for name in REPORT_NAMES {
        assert!(
            target.join(name).exists(),
            "{name} left the target although the history could not take it"
        );
    }
}

/// What `gate_cargo_jobs` (gate-lock.sh) prints and exports, in a bash of its own, on the machine of the test.
struct Jobs {
    code: Option<i32>,
    line: String,
    jobs: String,
    release: String,
    err: String,
}

/// `source gate-lock.sh; gate_cargo_jobs` under a `timeout` of 20 s, with the fixture's cgroup tree, `nproc` and
/// MemAvailable of `host`, and the CARGO_BUILD_JOBS of the operator only if `operator` names one.
fn cargo_jobs(fx: &Fixture, host: &Machine, operator: Option<&str>) -> Jobs {
    let order = r#"source "$1" && gate_cargo_jobs && printf 'JOBS=%s RELEASE=%s\n' "$CARGO_BUILD_JOBS" "$GATE_RELEASE_JOBS""#;
    let mut command = Command::new("timeout");
    without_judge_env(&mut command)
        .args(["20", "bash", "-c", order, "gate-lock"])
        .arg(repo_root().join("scripts").join("gate-lock.sh"))
        .env("HOME", &fx.home)
        .env("TMPDIR", &fx.tmp)
        .env("GATE_CGROUP_ROOT", &fx.cgroups)
        .env("GATE_PROC_SELF_CGROUP", &fx.proc_self)
        .envs(host.env());
    if let Some(jobs) = operator {
        command.env("CARGO_BUILD_JOBS", jobs);
    }
    let output = command.output().expect("bash sourcing gate-lock.sh");
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    let probe = out
        .lines()
        .find(|line| line.starts_with("JOBS="))
        .unwrap_or("");
    let value = |key: &str| -> String {
        probe
            .split_whitespace()
            .find_map(|word| word.strip_prefix(key))
            .unwrap_or("")
            .to_string()
    };
    Jobs {
        code: output.status.code(),
        line: out
            .lines()
            .find(|line| line.starts_with("cargo jobs="))
            .unwrap_or("")
            .to_string(),
        jobs: value("JOBS="),
        release: value("RELEASE="),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Spec K4-B, D-K4k, D-K4l, E7a.1 (D-E7a): the jobs are min(nproc, the CPUs of the cgroup, free RAM / 2 GiB), at least 1:
/// all the threads of the cap, with none kept back, and the line that says so names the three measures. The RAM free is
/// the lower of MemAvailable and what the cgroup leaves: a cgroup that leaves 6 GiB of 100 GiB of MemAvailable gives 3.
/// The jobs of each case are by hand: 24 of 24 and 100 GiB / 2 = 50; the cgroup's 8 and 12; 10 GiB / 2 = 5; nproc 2 and
/// 1; 3 GiB / 2 = 1 under 16 CPUs; 6 GiB / 2 = 3.
#[test]
fn the_cargo_jobs_are_all_the_threads_of_the_cap_under_the_cgroup_and_two_gib_a_job() {
    let cases = [
        ("24", "max 100000", MEM_OPEN, 100, "24", "none"),
        ("24", "800000 100000", MEM_OPEN, 100, "8", "8"),
        ("24", "1200000 100000", MEM_OPEN, 100, "12", "12"),
        ("24", "max 100000", MEM_OPEN, 10, "5", "none"),
        ("2", "max 100000", MEM_OPEN, 100, "2", "none"),
        ("1", "max 100000", MEM_OPEN, 100, "1", "none"),
        ("24", "1600000 100000", MEM_OPEN, 3, "1", "16"),
        ("24", "max 100000", "6442450944", 100, "3", "none"),
    ];
    for (nproc, cpu_max, mem_max, mem_gib, jobs, cgroup) in cases {
        let fx = Fixture::new("k4-trabajos", cpu_max, mem_max);
        let host = machine(&fx, "trabajos", nproc, mem_gib * KIB_PER_GIB);
        let got = cargo_jobs(&fx, &host, None);
        let free = if mem_max == MEM_OPEN { mem_gib } else { 6 };
        let want = format!(
            "cargo jobs={jobs} (nproc={nproc}, cgroup cpu={cgroup}, ram free={free}.0 GiB)"
        );
        assert_eq!(got.code, Some(0), "{want}: {}", got.err);
        assert_eq!(got.line, want, "{nproc} cores, {cpu_max}, {mem_gib} GiB");
        assert_eq!(got.jobs, jobs, "CARGO_BUILD_JOBS: {want}");
    }
}

/// Spec K4-B: a CARGO_BUILD_JOBS the operator set is kept, and the line says so (F1.2).
#[test]
fn the_jobs_the_operator_names_are_kept_and_the_line_says_so() {
    let fx = Fixture::new("k4-trabajos-operador", "800000 100000", MEM_OPEN);
    let host = machine(&fx, "operador", "24", 100 * KIB_PER_GIB);
    let got = cargo_jobs(&fx, &host, Some("5"));
    assert_eq!(got.code, Some(0), "{}", got.err);
    assert_eq!(got.jobs, "5", "{}", got.line);
    assert!(
        got.line
            .starts_with("cargo jobs=5 (CARGO_BUILD_JOBS set by the operator"),
        "{}",
        got.line
    );
}

/// Spec K4-B, D-K4o: a measure that fails does not stop the gate. `gate_cargo_jobs` exports one job, says what it could
/// not measure and names it, and the release builds take one too. Three ways to fail, as for the planner.
#[test]
fn a_measure_that_fails_gives_one_job_and_says_what_it_could_not_read() {
    let fx = Fixture::new("k4-trabajos-falla-cgroup", "max 100000", "basura");
    let host = machine(&fx, "falla", "24", 100 * KIB_PER_GIB);
    let by_cgroup = cargo_jobs(&fx, &host, None);
    let fx = Fixture::new("k4-trabajos-falla-memoria", "max 100000", MEM_OPEN);
    let host = machine_without_memavailable(&fx);
    let by_memory = cargo_jobs(&fx, &host, None);
    let fx = Fixture::new("k4-trabajos-falla-nproc", "max 100000", MEM_OPEN);
    let host = machine(&fx, "falla", "x", 100 * KIB_PER_GIB);
    let by_nproc = cargo_jobs(&fx, &host, None);
    for (got, why) in [
        (by_cgroup, "memory.max"),
        (by_memory, "MemAvailable"),
        (by_nproc, "nproc"),
    ] {
        assert_eq!(
            got.code,
            Some(0),
            "{why}: gate_cargo_jobs stopped the shell: {}",
            got.err
        );
        assert!(
            got.line
                .starts_with("cargo jobs=1 (resource measure failed: ")
                && got.line.ends_with("; series with one job)")
                && got.line.contains(why),
            "{why}: {}",
            got.line
        );
        assert_eq!(got.jobs, "1", "{why}: {}", got.line);
        assert_eq!(got.release, "1", "{why}: {}", got.line);
    }
}

/// A machine whose /proc/meminfo has no MemAvailable.
fn machine_without_memavailable(fx: &Fixture) -> Machine {
    let host = machine(fx, "sin-memavailable", "24", 100 * KIB_PER_GIB);
    write(
        Path::new(&host.meminfo),
        "MemTotal:       49327104 kB\nMemFree:         1048576 kB\n",
    );
    host
}

// --- CI-E7a-1 (D-E7d, D-E7f): row_jobs and the admission, as properties --------------------------------------------------
// There is no `proptest` in this crate (D-E7l): like the properties of gate_history_contract.rs, the generator is the
// `random` of Python seeded with a fixed number (20261110), inside `python3 -I -B -c`, and what is small is enumerated.

/// The planner as a module and a seeded generator. argv: the scripts directory.
const PRELUDE_ADMISSION: &str = r#"
import importlib.util, os, random, sys
spec = importlib.util.spec_from_file_location("gate_dag", os.path.join(sys.argv[1], "gate-dag.py"))
dag = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dag)
GIB = 1 << 30
rng = random.Random(20261110)

def step(i, c, r_gib, named, models=False, exclusive=False):
    """A row of the table: `named` is its CARGO_BUILD_JOBS (0 names none)."""
    env = {"CARGO_BUILD_JOBS": str(named)} if named else {}
    return dag.Step(10 * (i + 1), f"s{i}", [], ["models"] if models else [], "exclusive" if exclusive else "normal",
                    c, r_gib, 1.0, env, ["true"])

def claimed(s, jobs):
    return dag.row_jobs(int(s.env["CARGO_BUILD_JOBS"]), jobs) if "CARGO_BUILD_JOBS" in s.env else 0

def random_step(i, models_p=0.15, exclusive_p=0.05, named_from=(0, 1, 2, 4, 8, 12, 16)):
    return step(i, rng.choice([0.25, 0.5, 1, 2, 4]), rng.choice([0.5, 1, 2, 4, 8]), rng.choice(named_from),
                models=rng.random() < models_p, exclusive=rng.random() < exclusive_p)
"#;

/// P6 (E7a.P6), row_jobs: min(named, jobs) with a floor of 1, bounded by both and monotone in both, over every pair with
/// 1 <= named <= 32 and 1 <= jobs <= 32; and the literal answers of a few pairs, worked out by hand.
const BODY_ROW_JOBS: &str = r#"
for named in range(1, 33):
    for jobs in range(1, 33):
        value = dag.row_jobs(named, jobs)
        assert 1 <= value <= min(named, jobs), f"row_jobs({named}, {jobs}) = {value} is outside 1..{min(named, jobs)}"
        if named < 32:
            assert dag.row_jobs(named + 1, jobs) >= value, f"row_jobs fell when named went from {named} to {named + 1} at {jobs}"
        if jobs < 32:
            assert dag.row_jobs(named, jobs + 1) >= value, f"row_jobs fell when jobs went from {jobs} to {jobs + 1} at {named}"
for named, jobs, want in [(12, 16, 12), (12, 7, 7), (1, 24, 1), (8, 24, 8), (32, 1, 1), (5, 5, 5)]:
    assert dag.row_jobs(named, jobs) == want, f"row_jobs({named}, {jobs}) is not {want}"
print("row_jobs pairs=1024")
"#;

/// P6 (E7a.P6), admission, three properties over seeded states:
/// 1. monotone: a candidate that is admitted with some caps is still admitted with any cap that is greater or equal on
///    each axis (CPU, RAM, jobs, the free memory of the models rule), one axis at a time and all of them together;
/// 2. admitting one by one with `admission`, the jobs of the rows alive never add up past the jobs cap once two are alive;
/// 3. a candidate with nobody alive and no exclusive row pending is admitted, whatever the caps.
///
/// Then the answers of a few states with the reason by hand, which also fix the order of the reasons: exclusive, models
/// (count, then free memory), CPU, jobs, RAM.
const BODY_ADMISSION: &str = r#"
admitted = waited = 0
for _ in range(2000):
    alive = [random_step(i) for i in range(rng.randint(1, 4))]
    for row in alive:
        row.anon_now = rng.choice([None, int(rng.uniform(0, 6) * GIB)])
    candidate = random_step(len(alive))
    earlier = rng.random() < 0.3
    caps = [rng.uniform(1, 30), rng.randint(2, 60) * GIB, rng.randint(1, 40), rng.randint(0, 12) * GIB]
    if dag.admission(candidate, alive, earlier, *caps) is not None:
        waited += 1
        continue
    admitted += 1
    for axis in range(4):
        more = list(caps)
        more[axis] += [rng.uniform(0, 10), rng.randint(0, 10) * GIB, rng.randint(0, 10), rng.randint(0, 10) * GIB][axis]
        assert dag.admission(candidate, alive, earlier, *more) is None, f"admitted with {caps}, not with {more}"
    more = [c + e for c, e in zip(caps, [rng.uniform(0, 10), rng.randint(0, 10) * GIB, rng.randint(0, 10), rng.randint(0, 10) * GIB])]
    assert dag.admission(candidate, alive, earlier, *more) is None, f"admitted with {caps}, not with {more}"
assert admitted >= 300 and waited >= 300, f"the states are not mixed: {admitted} admitted, {waited} waited"

filled = held = 0
for _ in range(500):
    jobs = rng.randint(1, 32)
    alive = []
    for i in range(rng.randint(2, 8)):
        row = random_step(i, models_p=0.0, exclusive_p=0.0, named_from=(1, 2, 4, 8, 12, 16, 24))
        if dag.admission(row, alive, False, 1e9, 1e15, jobs, 1e15) is None:
            alive.append(row)
        else:
            held += 1
        if len(alive) >= 2:
            filled += 1
            total = sum(claimed(r, jobs) for r in alive)
            assert total <= jobs, f"{len(alive)} rows alive claim {total} jobs of {jobs}"
assert filled >= 300 and held >= 100, f"the fill is not mixed: {filled} states with two alive, {held} rows held"

for _ in range(300):
    candidate = random_step(0, exclusive_p=0.5)
    caps = [rng.uniform(0, 2), rng.randint(0, 2) * GIB, rng.randint(1, 3), rng.randint(0, 2) * GIB]
    assert dag.admission(candidate, [], False, *caps) is None, f"a lone row is held: {caps}"

lone = step(0, 1, 1, 8, exclusive=True)
assert dag.admission(lone, [], False, 0, 0, 1, 0) is None
assert dag.admission(lone, [], True, 16, 40 * GIB, 16, 64 * GIB) == "exclusive"
assert dag.admission(step(0, 1, 1, 8), [], True, 0, 0, 1, 0) is None

a, b = step(0, 1, 1, 8), step(1, 1, 1, 8)
assert dag.admission(b, [a], False, 16, 40 * GIB, 16, 64 * GIB) is None, "8 + 8 jobs fit in 16"
assert dag.admission(b, [a], False, 16, 40 * GIB, 15, 64 * GIB) == "jobs", "8 + 8 jobs do not fit in 15"
assert dag.admission(b, [a], False, 1.5, 40 * GIB, 15, 64 * GIB) == "cpu", "the CPU is judged before the jobs: 1 + 1 > 1.5"
assert dag.admission(b, [a], False, 16, 1.5 * GIB, 15, 64 * GIB) == "jobs", "the jobs are judged before the RAM"
assert dag.admission(b, [a], False, 16, 1.5 * GIB, 16, 64 * GIB) == "ram", "1 + 1 GiB do not fit in 1.5 GiB"
a.anon_now = 3 * GIB
assert dag.admission(b, [a], False, 16, 3.5 * GIB, 16, 64 * GIB) == "ram", "the RAM of a live row is max(its anon now, its estimate)"
assert dag.admission(b, [a], False, 16, 4 * GIB, 16, 64 * GIB) is None
assert dag.admission(step(1, 1, 1, 0), [step(0, 1, 1, 0)], False, 16, 40 * GIB, 1, 64 * GIB) is None, "rows that name no jobs claim none"
assert dag.admission(step(1, 1, 1, 8), [a, step(2, 1, 1, 0)], False, 16, 40 * GIB, 16, 64 * GIB) is None
ex = step(1, 1, 1, 8, exclusive=True)
assert dag.admission(b, [ex], False, 16, 40 * GIB, 16, 64 * GIB) == "exclusive", "nothing starts beside an exclusive row"
assert dag.admission(ex, [a], False, 16, 40 * GIB, 16, 64 * GIB) == "exclusive", "an exclusive row starts alone"
m1, m2, m3 = (step(i, 1, 1, 1, models=True) for i in range(3))
assert dag.admission(m3, [m1, m2], False, 16, 40 * GIB, 16, 64 * GIB) == "models", "no more than two rows with models"
assert dag.admission(m2, [m1], False, 16, 40 * GIB, 16, 5 * GIB - 1) == "ram", "4 GiB of MemAvailable on top of its 1 GiB"
assert dag.admission(m2, [m1], False, 16, 40 * GIB, 16, 5 * GIB) is None
print("admission admitted=%d waited=%d filled=%d held=%d" % (admitted, waited, filled, held))
"#;

/// `python3 -I -B -c <prelude + body> <scripts dir>` under a `timeout` of 60 s, with HOME and TMPDIR of the fixture. The
/// planner module is loaded, not run: nothing here starts a step or reads the machine.
fn property_of(body: &str) -> String {
    let fx = Fixture::new("e7a-propiedades", CPU_FOUR, MEM_OPEN);
    let mut command = Command::new("timeout");
    let output = without_judge_env(&mut command)
        .args(["60", "python3", "-I", "-B", "-c"])
        .arg(format!("{PRELUDE_ADMISSION}{body}"))
        .arg(repo_root().join("scripts"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("HOME", &fx.home)
        .env("TMPDIR", &fx.tmp)
        .output()
        .expect("python3 -I -B -c gate-dag");
    assert!(
        output.status.success(),
        "the property failed (exit {:?}): {}{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Spec E7a.P6 (D-E7d): `row_jobs(named, jobs)` is a function of the module, between 1 and min(named, jobs) and monotone in
/// both arguments, for every pair up to 32.
#[test]
fn row_jobs_is_the_lower_of_the_two_numbers_with_a_floor_of_one() {
    let out = property_of(BODY_ROW_JOBS);
    assert!(out.contains("row_jobs pairs=1024"), "{out}");
}

/// Spec E7a.P6 (D-E7e, D-E7f): the admission is a pure function of the module, monotone in each cap, it never lets the jobs
/// of the rows alive pass the jobs cap, and it admits a row that has nobody beside it.
#[test]
fn the_admission_is_monotone_and_the_jobs_alive_never_pass_the_cap() {
    let out = property_of(BODY_ADMISSION);
    assert!(out.starts_with("admission admitted="), "{out}");
}

// --- CI-J17: a planner inside another one does not delegate a second `juez` -----------------------------------------------
// The planner delegates the cgroup it runs in: `<scope>/juez` for itself and the processes of the scope. A planner that
// starts inside that `juez` (the self-test of the gate and the contracts start real ones) went down one level more each
// time, and on 2026-11-10 the scope of the judge ended in `…/juez/juez/juez/…` with hundreds of levels.

/// Every directory under `root`, the way a walk of the cgroup tree would see it.
fn dirs_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(dirs_under(&path));
            found.push(path);
        }
    }
    found
}

/// A cgroup directory of the fake tree the way cgroupfs makes one: with its own `cgroup.subtree_control`, writable.
fn make_fake_cgroup(dir: &Path) {
    fs::create_dir_all(dir).expect("fake cgroup dir");
    write(&dir.join("cgroup.subtree_control"), "");
}

/// J17 (scenario 1): a planner whose cgroup is already called `juez` does not make another one inside it, does not move
/// the processes of that `juez` and does not touch its controllers; it says the scope is not delegated and runs in series.
#[test]
fn a_planner_in_a_scope_called_juez_does_not_delegate_it_again() {
    let mut fx = Fixture::new("j17-juez", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "s10", "-", "0.1", "")]);
    let juez = fx.cgroups.join("scope").join("juez");
    make_fake_cgroup(&juez);
    write(&juez.join("cgroup.procs"), "4242\n");
    write(&fx.proc_self, "0::/scope/juez\n");
    let run = fx.run("1");
    assert_green(&run, "a planner started inside a `juez`");
    assert!(
        !juez.join("juez").exists(),
        "the planner made {}: a `juez` inside a `juez`",
        juez.join("juez").display()
    );
    assert_eq!(
        read(&juez.join("cgroup.procs")),
        "4242\n",
        "the planner moved processes of the `juez` it runs in"
    );
    assert_eq!(
        read(&juez.join("cgroup.subtree_control")),
        "",
        "the planner enabled controllers in the `juez` it runs in"
    );
    assert!(
        run.out.contains("the scope is not delegated"),
        "the planner did not say that a `juez` is not delegated again: {}",
        run.out
    );
}

/// J17 (scenario 3): the planner of the judge starts a second planner from one of its rows, as the self-test and the
/// contracts do. The second one sees the cgroup the first one moved its processes to, `scope/juez`: its `/proc/self/cgroup`
/// is a file of the fixture that says so, and its `scope/juez` has the `cgroup.subtree_control` cgroupfs gives every directory.
#[test]
fn two_planners_one_inside_the_other_leave_no_juez_inside_a_juez() {
    let mut fx = Fixture::new("j17-anidado", CPU_FOUR, MEM_OPEN);
    fx.table(&[(10, "anidado", "-", "0.1", "")]);
    let juez = fx.cgroups.join("scope").join("juez");
    let inner_self = fx.base.join("proc-self-cgroup-dentro");
    write(&inner_self, "0::/scope/juez\n");
    let inner_table = fx.base.join("tabla-dentro.txt");
    write(
        &inner_table,
        &format!(
            "10 hoja - - normal 1 5 0.05 /bin/sh {} hoja 0.05 {}\n",
            fx.script.display(),
            fx.control.display()
        ),
    );
    let launcher = fx.base.join("planificador-dentro.sh");
    write(
        &launcher,
        "#!/bin/sh\n# $1 juez of the outer planner, $2 /proc/self/cgroup of the inner one, $3 planner, $4 its table\n\
         : > \"$1/cgroup.subtree_control\"\n\
         GATE_PROC_SELF_CGROUP=\"$2\" exec python3 -I \"$3\" run --table \"$4\"\n",
    );
    write(
        &fx.table,
        &format!(
            "10 anidado - - normal 1 5 0.3 /bin/sh {} {} {} {} {}\n",
            launcher.display(),
            juez.display(),
            inner_self.display(),
            repo_root().join("scripts/gate-dag.py").display(),
            inner_table.display()
        ),
    );
    let run = fx.run("0");
    assert_green(&run, "a planner that starts a planner");
    assert!(
        juez.is_dir(),
        "the outer planner did not make its `juez`, so there was nothing to nest in: {}",
        run.out
    );
    assert!(
        fx.control.join("env-hoja.txt").is_file(),
        "the inner planner did not run its row: {}{}",
        run.out,
        run.err
    );
    let nested: Vec<_> = dirs_under(&fx.cgroups)
        .into_iter()
        .filter(|dir| dir.ends_with("juez/juez"))
        .collect();
    assert!(
        nested.is_empty(),
        "a `juez` inside a `juez`: {nested:?}\n{}{}",
        run.out,
        run.err
    );
}

// --- CI-E9 (D-E9a): E2E waits for B3 and not for DBG --------------------------------------------------------------------

/// The step of the rows of CI-E9: it marks that it started (`started-<id>`). DBG does not end until E2E has started or
/// `dbg-ticks` x 0.1 s pass, and leaves `dbg-saw` = yes or no. $1 id, $3 control.
const E9_PASO_SH: &str = r#"#!/bin/sh
id="$1"
control="$3"
echo "=== $id banner"
: > "$control/started-$id"
if [ "$id" = DBG ]; then
    limit=$(cat "$control/dbg-ticks" 2>/dev/null || echo 0)
    i=0
    while [ ! -e "$control/started-E2E" ] && [ "$i" -lt "$limit" ]; do
        sleep 0.1
        i=$((i + 1))
    done
    if [ -e "$control/started-E2E" ]; then echo yes > "$control/dbg-saw"; else echo no > "$control/dbg-saw"; fi
fi
echo "Running unittests fake $id"
echo "test result: ok. 1 passed; 0 failed"
"#;

/// The rows P, DBG, B3 and E2E of the real table, in miniature. Only the dependencies can hold a row back: the history has
/// almost no CPU (100 us a step), and DBG is the longest row in it (the series that seeds it makes DBG wait 0.3 s, since E2E
/// starts after it there), so the planner starts DBG before B3 and E2E, as the real table's critical path does.
/// `e2e_deps` is what E2E waits for; `dbg_ticks` is how many tenths of a second DBG waits for E2E to start in the DAG run.
fn e9_fixture(name: &str, e2e_deps: &str, dbg_ticks: &str) -> Fixture {
    let mut fx = Fixture::new(name, CPU_FOUR, MEM_OPEN);
    let script = fx.base.join("paso-e9.sh");
    write(&script, E9_PASO_SH);
    let rows: Vec<StepRow> = [
        (10, "P", "-"),
        (20, "DBG", "P"),
        (30, "B3", "P"),
        (50, "E2E", e2e_deps),
    ]
    .into_iter()
    .map(|(idx, id, deps)| StepRow {
        deps,
        script: script.clone(),
        ..fx.row(idx, id, "-", "0.1")
    })
    .collect();
    table_of(&mut fx, &rows);
    for id in ["P", "DBG", "B3", "E2E"] {
        fx.usage(id, 100);
    }
    write(&fx.control.join("dbg-ticks"), "3");
    fx.seed();
    for id in ["P", "DBG", "B3", "E2E"] {
        let _ = fs::remove_file(fx.control.join(format!("started-{id}")));
    }
    let _ = fs::remove_file(fx.control.join("dbg-saw"));
    write(&fx.control.join("dbg-ticks"), dbg_ticks);
    fx
}

/// The records of a DAG run of `fx` (the one it has history for), green, and whether DBG saw E2E start before it ended.
fn e9_dag_run(fx: &Fixture) -> (Vec<Value>, String) {
    let before = snapshot(&fx.history, "pasos.jsonl");
    let run = fx.run("1");
    assert_green(&run, "DAG with room for DBG and E2E beside each other");
    let saw = read(&fx.control.join("dbg-saw")).trim().to_string();
    (appended(&fx.history, "pasos.jsonl", &before), saw)
}

/// CI-E9 (D-E9a, S-E9-3). With E2E waiting for B3 only, DBG does not end until E2E has started (a barrier of files, with 20 s
/// to wait that run out only if E2E cannot start), so it ends having seen E2E and the instants the planner records say E2E
/// started after B3 ended and before DBG ended: an order of events, with no duration in it. The control, with the old
/// dependency (B3 and DBG), gives DBG 1 s: E2E cannot start while DBG lives, so DBG does not see it and E2E starts after DBG
/// ended. The control is what shows that the first half could fail.
#[test]
fn e2e_starts_when_b3_ends_while_dbg_still_runs_and_after_dbg_with_the_old_dependency() {
    let ((pasos, saw), (viejo, viejo_saw)) = std::thread::scope(|scope| {
        let solo_b3 = scope.spawn(|| e9_dag_run(&e9_fixture("e9-solo-b3", "B3", "200")));
        let viejo = scope.spawn(|| e9_dag_run(&e9_fixture("e9-b3-y-dbg", "B3,DBG", "10")));
        (
            solo_b3.join().expect("the fixture with B3 only"),
            viejo.join().expect("the fixture with B3,DBG"),
        )
    });
    let (dbg, b3, e2e) = (
        by_id(&pasos, "DBG"),
        by_id(&pasos, "B3"),
        by_id(&pasos, "E2E"),
    );
    assert_eq!(
        saw,
        "yes",
        "DBG waited 20 s and E2E did not start: it depends on B3 only and B3 ended at {}",
        num(b3, "fin")
    );
    assert!(
        num(dbg, "inicio") < num(e2e, "inicio") && num(e2e, "inicio") >= num(b3, "fin"),
        "the order of events is DBG starts, B3 ends, E2E starts: DBG.inicio={} B3.fin={} E2E.inicio={}",
        num(dbg, "inicio"),
        num(b3, "fin"),
        num(e2e, "inicio")
    );
    assert!(
        num(e2e, "inicio") < num(dbg, "fin"),
        "E2E started after DBG ended, and DBG saw it: E2E.inicio={} DBG.fin={}",
        num(e2e, "inicio"),
        num(dbg, "fin")
    );

    let (dbg, e2e) = (by_id(&viejo, "DBG"), by_id(&viejo, "E2E"));
    assert_eq!(
        viejo_saw, "no",
        "control: with B3,DBG, DBG saw E2E start while it lived"
    );
    assert!(
        num(e2e, "inicio") >= num(dbg, "fin"),
        "control: with B3,DBG, E2E started before DBG ended: E2E.inicio={} DBG.fin={}",
        num(e2e, "inicio"),
        num(dbg, "fin")
    );
}

/// The critical path of the real table with the durations of the 19th run (8-nov-2026, 220355b), the `seg` of its rows in
/// `ci-tiempos/memory-industry/20261108/pasos.jsonl`: P 4.201, DBG 391.036 (its two halves, before CI-E11: the plain pass
/// 164.87 and the rest), NLI 37.508, B3 1.693 (a warm build), REL 102.530, E2E 180.680, DC 224.391, CV 68.583, M 146.363;
/// and, for the rows CI-E11 made, the figures of its spec (ci-rapido-E11.spec.md, §3): B1 5 (warm), CT 83.3 (the contracts),
/// D2 111 (107.86 and its provision) and D3 93 (30.1 and the probe of `claude -p`). By hand, the chains of the table now:
///   P -> DC          = 4.201 + 224.391           = 228.592
///   P -> B3 -> E2E   = 4.201 +   1.693 + 180.680 = 186.574   (391.055 with the cold B3 of 206.174 that gate_usl_contract uses)
///   P -> B1 -> DBG   = 4.201 +   5.000 + 164.870 = 174.071
///   P -> B1 -> D2    = 4.201 +   5.000 + 111.000 = 120.201
///   P -> B1 -> D3    = 102.201; P -> B1 -> CT = 92.501; P -> B1 -> NLI = 46.709
/// With E11 the chain of 432.745 that went through DBG and NLI is gone (the spec's §3 stops at P -> B3 -> E2E and leaves DC
/// out, which the 19th run says is longer, 228.592): the path is P, DC; with the cold B3 it is P, B3, E2E (391.055). B1 ranks
/// itself plus the longest reader behind it (5 + 164.87 = 169.87), DBG ranks its own 164.87 (nothing waits behind it any more,
/// NLI waits for B1), and E2E is a sink (180.680).
const BODY_CRITICAL_PATH_19: &str = r#"
steps = dag.parse_table(os.path.join(sys.argv[1], "gate-dag.txt"))

def critical(b3):
    dur = {"P": 4.201, "B1": 5.0, "DBG": 164.87, "CT": 83.3, "D2": 111.0, "D3": 93.0, "NLI": 37.508, "B3": b3,
           "REL": 102.530, "E2E": 180.680, "DC": 224.391, "CV": 68.583, "M": 146.363}
    rank, nxt = dag.ranks(steps, dur)
    cur = max(steps, key=lambda s: rank[s.id])
    length, path = rank[cur.id], []
    while cur:
        path.append(cur.id)
        cur = nxt[cur.id]
    return path, length, rank

path, length, rank = critical(1.693)
assert path == ["P", "DC"], f"the critical path with a warm B3 is {path}, not P, DC"
assert abs(length - 228.592) < 1e-6, f"its length is {length}, not 4.201 + 224.391 = 228.592"
assert abs(rank["B1"] - 169.87) < 1e-6, f"B1 ranks {rank['B1']}, not 5 + 164.87: the longest reader behind it is DBG"
assert abs(rank["DBG"] - 164.87) < 1e-6, f"DBG ranks {rank['DBG']}, not its own 164.87: something waits behind it"
assert abs(rank["E2E"] - 180.680) < 1e-6, f"E2E ranks {rank['E2E']}, not its own 180.680: something waits behind it"

path, length, rank = critical(206.174)
assert path == ["P", "B3", "E2E"], f"the critical path with a cold B3 is {path}, not P, B3, E2E"
assert abs(length - 391.055) < 1e-6, f"its length is {length}, not 4.201 + 206.174 + 180.680 = 391.055"
print("critical path P>DC 228.592; cold B3 P>B3>E2E 391.055")
"#;

/// CI-E9 (D-E9a, S-E9-1) and CI-E11 (D-E11b): in the real table E2E is no longer behind DBG and DBG is no longer behind a
/// 391 s chain: with the readers of T_dbg beside each other the critical path of the 19th run is P, DC (and P, B3, E2E with a
/// cold B3), and it no longer goes through DBG or NLI.
#[test]
fn the_critical_path_of_the_real_table_with_the_19th_run_is_p_dc_and_no_longer_goes_through_dbg() {
    let out = property_of(BODY_CRITICAL_PATH_19);
    assert!(
        out.contains("critical path P>DC 228.592; cold B3 P>B3>E2E 391.055"),
        "{out}"
    );
}

// --- CI-J18: the table has one reader, and it refuses what it does not understand -------------------------------------
// D-J18a: `gate-dag.py rows --table <file>` says the rows and the pairs of rows that can be alive together, and the audit
// asks it instead of reading the table again. D-J18b: a row that is not what the grammar says is refused with the line
// before anything starts, and a target that leaves its root writes nothing outside it.

impl Fixture {
    /// `gate-dag.py rows --table <this table>`, with the environment `run_command` gives every planner of this file.
    fn rows(&self, extra: &[(&str, &str)]) -> Run {
        let mut command = Command::new("python3");
        command
            .arg("-I")
            .arg(repo_root().join("scripts/gate-dag.py"))
            .arg("rows")
            .arg("--table")
            .arg(&self.table);
        self.run_command(command, "0", extra)
    }
}

/// The `FAIL:` line of the stderr of a planner: its reason, the one thing `rows` and `run` have to say alike.
fn fail_line(run: &Run) -> String {
    run.err
        .lines()
        .find(|line| line.starts_with("FAIL:"))
        .unwrap_or("")
        .to_string()
}

/// CI-J18 J18a.1 (D-J18a). The table, in the order of lines 40, 10, 20, 25, 30: C waits for P and is `untimed`; A waits for
/// P and names its jobs; X waits for A and is `exclusive`; B waits for A. `rows` says them in idx order, with the order of a
/// row without the `NAME=value` of its environment, and then the pairs that can be alive together, `a` before `b` by idx. By
/// hand, with the rule "neither is exclusive and neither waits for the other, directly or through others":
///   P with A, X, B and C: every one of them waits for P, through A or directly: no pair.
///   A with X: X is exclusive. A with B: B waits for A. A with C: C waits for P only, so they can be alive together.
///   X with B and X with C: X is exclusive. B with C: nothing joins them.
/// So the pairs are A-C and B-C, and `untimed` is not a reason for either to wait.
#[test]
fn j18a_1_rows_says_the_rows_in_idx_order_and_the_pairs_that_can_be_alive_together() {
    let mut fx = Fixture::new("j18a1", CPU_FOUR, MEM_OPEN);
    let rows = [
        StepRow {
            deps: "P",
            class: "untimed",
            ..fx.row(40, "C", "-", "0.1")
        },
        fx.row(10, "P", "-", "0.1"),
        StepRow {
            deps: "P",
            env: "CARGO_BUILD_JOBS=2",
            ..fx.row(20, "A", "-", "0.1")
        },
        StepRow {
            deps: "A",
            class: "exclusive",
            ..fx.row(25, "X", "-", "0.1")
        },
        StepRow {
            deps: "A",
            ..fx.row(30, "B", "-", "0.1")
        },
    ];
    table_of(&mut fx, &rows);
    let run = fx.rows(&[]);
    assert!(!run.timed_out, "`rows` did not answer in 30 s");
    assert_eq!(
        run.code,
        Some(0),
        "`gate-dag.py rows --table` has to say the table and exit 0: {}{}",
        run.out,
        run.err
    );
    let order = |id: &str| {
        format!(
            "/bin/sh {} {id} 0.1 {}",
            fx.script.display(),
            fx.control.display()
        )
    };
    let mut want: Vec<String> = ["P", "A", "X", "B", "C"]
        .into_iter()
        .map(|id| format!("row {id} - {}", order(id)))
        .collect();
    want.extend(["overlap A C", "overlap B C"].map(String::from));
    let said: Vec<&str> = run.out.lines().collect();
    assert_eq!(
        said, want,
        "the rows in idx order, then the pairs that can be alive together"
    );
    assert!(
        !run.out.contains("CARGO_BUILD_JOBS"),
        "the order of a row is its argv without the NAME=value of its environment: {}",
        run.out
    );
}

/// CI-J18 J18a.2 (D-J18a). A table that `run` refuses, `rows` refuses with the same line: exit 2, nothing on stdout, and the
/// `FAIL:` line of stderr is the same text. The row names a target that is not a name (`../x`); `CARGO_TARGET_DIR` is empty, so
/// a planner that did not judge the target would be refused for another reason and the lines would differ.
#[test]
fn j18a_2_rows_refuses_what_run_refuses_with_the_same_line() {
    let mut fx = Fixture::new("j18a2", CPU_FOUR, MEM_OPEN);
    let rows = [StepRow {
        resources: "target:../x",
        ..fx.row(10, "s10", "-", "0.1")
    }];
    table_of(&mut fx, &rows);
    let said = fx.rows(&[("CARGO_TARGET_DIR", "")]);
    let ran = fx.run_env("0", &[("CARGO_TARGET_DIR", "")]);
    assert!(
        !said.timed_out && !ran.timed_out,
        "a planner did not answer in 30 s"
    );
    assert_eq!(
        (said.code, ran.code),
        (Some(2), Some(2)),
        "`rows` and `run` have to refuse the table with 2\nrows: {}{}\nrun: {}{}",
        said.out,
        said.err,
        ran.out,
        ran.err
    );
    assert!(
        said.out.is_empty(),
        "`rows` printed something for a table it refuses: {}",
        said.out
    );
    let line = fail_line(&said);
    assert!(
        line.contains("../x"),
        "`rows` has to say the FAIL line that names the target: {}",
        said.err
    );
    assert_eq!(
        line,
        fail_line(&ran),
        "`rows` and `run` are one reader and say the same reason"
    );
}

/// CI-J18 J18b.2 (D-J18b). The second row names `target:../../../escape` and the root of the targets is three levels below the
/// fixture's directory, so the path `<root>/<key>/../../../escape` ends inside the fixture (`mundo/escape`) and a planner
/// that built it would write there. The planner refuses the table with 2, names the target, starts nothing (the first row
/// writes its mark when it starts, and does not) and leaves no `.gate-tree` and no `escape` in the fixture.
#[test]
fn j18b_2_a_target_that_leaves_its_root_writes_nothing_outside_and_starts_nothing() {
    let mut fx = Fixture::new("j18b2", CPU_FOUR, MEM_OPEN);
    let rows = [
        fx.row(10, "s10", "-", "0.1"),
        StepRow {
            deps: "s10",
            resources: "target:../../../escape",
            ..fx.row(20, "s20", "-", "0.1")
        },
    ];
    table_of(&mut fx, &rows);
    let root = fx.base.join("mundo/dos/target-root");
    let run = fx.run_env(
        "0",
        &[("CARGO_TARGET_DIR", root.to_str().expect("utf-8 path"))],
    );
    assert!(!run.timed_out, "the planner did not answer in 30 s");
    assert_eq!(
        run.code,
        Some(2),
        "a target that leaves its root has to be refused with 2\n{}{}",
        run.out,
        run.err
    );
    assert!(
        run.text().contains("../../../escape"),
        "the refusal has to name the target: {}",
        run.text()
    );
    assert!(
        fx.started().is_empty(),
        "a step started although the table is refused: {:?}",
        fx.started()
    );
    assert!(
        files_named(&fx.base, ".gate-tree").is_empty(),
        "a .gate-tree was written: {:?}",
        files_named(&fx.base, ".gate-tree")
    );
    assert!(
        !fx.base.join("mundo/escape").exists(),
        "the target `../../../escape` was made outside its root"
    );
}
