#!/usr/bin/env python3
"""Lane planner of the local judge (CI-F3, F3.2).

`run --table <file>` runs the step table. GATE_DAG=0 (the default) is the series: the steps in
idx order, one at a time. GATE_DAG=1 plans the DAG: a step starts when its dependencies exited 0,
and it is admitted by CPU and RAM, recomputed on every event (a child exit, and a 1 s tick as a
backup). The verdict is the series one: the red with the lowest idx wins, the steps before it
finish, the ones after it are killed (D-F3d). A kernel kill (OOM, or a signal the planner did not
send) gives no verdict: exit 2 (D-F3f).

Not in F3.2: the model and cluster semaphores (D-F3q), the removal of old run directories (D-F3i) and
the per-binary launcher (D-F3u). `exclusive` and `boundary` run alone, after every lower idx: the
conservative form of D-F3l. The resource column names the target of a row (CI-F3.3c, D-F3g): the step
builds under CARGO_TARGET_DIR/<tree key>/<target>, and a target that another tree marked is refused
before the step starts. The other resources are parsed and ignored until F3.4.
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import math
import os
import re
import secrets
import select
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from typing import TYPE_CHECKING, Any, NoReturn

# No bytecode next to the scripts (J7): the judge does not write __pycache__ into the tree.
sys.dont_write_bytecode = True
# -I leaves the script's own folder off sys.path: gate_history.py sits beside this file.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gate_history  # noqa: E402

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable

GIB = 1 << 30
TICK_S = 1.0
SAFETY = 1.25
WINDOW = 5
MIN_RESERVE = 4 * GIB
# CI-F3.3b-2 (spec F3.3b-1b-iii): `untimed` schedules as `normal`; its block is past the judge's boundary line.
CLASSES = frozenset({"normal", "exclusive", "boundary", "untimed"})
EXCLUSIVE = frozenset({"exclusive", "boundary"})
ALLOWED_ENV = frozenset(
    {
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
    }
)
NAME_RE = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)=(.*)$")


class Refusal(Exception):
    """Exit 2: a table or environment problem, or a planner that cannot decide."""


def die(message: str) -> NoReturn:
    raise Refusal(message)


def repo_root() -> str:
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def read_text(path: str) -> str | None:
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return None


def read_kv(path: str) -> dict[str, int] | None:
    text = read_text(path)
    if text is None:
        return None
    values: dict[str, int] = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].isdigit():
            values[parts[0]] = int(parts[1])
    return values


def read_int(path: str) -> int | None:
    text = read_text(path)
    return int(text) if text is not None and text.strip().isdigit() else None


def psi_some_avg10(path: str) -> float | None:
    text = read_text(path)
    for line in (text or "").splitlines():
        if line.startswith("some"):
            for field in line.split():
                if field.startswith("avg10="):
                    return float(field[len("avg10=") :])
    return None


def p95(values: Iterable[float]) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(0.95 * len(ordered)) - 1)]


def sha256_of(path: str) -> str:
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def git_state(root: str) -> tuple[str | None, bool | None]:
    """The sha of HEAD and whether the tree is dirty (D-F3s). Where git cannot say, None: never a guess."""
    try:
        sha = subprocess.run(
            ["git", "-C", root, "rev-parse", "--verify", "-q", "HEAD"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        estado = subprocess.run(
            ["git", "-C", root, "status", "--porcelain"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return None, None
    return (sha or None), bool(estado.strip())


def loadavg() -> float | None:
    """The one-minute load of the machine, from /proc/loadavg; None where it cannot be read."""
    text = read_text("/proc/loadavg")
    try:
        return float(text.split()[0]) if text else None
    except (ValueError, IndexError):
        return None


def system_psi() -> dict[str, float | None]:
    """The system's pressure (some avg10) for cpu, memory and io, read at the end of the run."""
    return {name: psi_some_avg10(f"/proc/pressure/{name}") for name in ("cpu", "memory", "io")}


# --- the step table -----------------------------------------------------------


class Step:
    def __init__(
        self,
        idx: int,
        step_id: str,
        deps: list[str],
        recursos: list[str],
        clase: str,
        cpu: float,
        ram_gb: float,
        dur: float,
        env: dict[str, str],
        argv: list[str],
    ) -> None:
        self.idx = idx
        self.id = step_id
        self.deps = deps
        self.recursos = recursos
        self.clase = clase
        self.cpu_tab = cpu
        self.ram_tab = ram_gb
        self.dur_tab = dur
        self.env = env
        self.argv = argv
        self.state = "pending"
        self.proc: subprocess.Popen[bytes] | None = None
        self.log: str | None = None
        self.cg: str | None = None
        self.rc: int | None = None
        self.parado = False
        self.motivo: str | None = None
        self.t_ready: float | None = None
        self.t_start: float | None = None
        self.t_end: float | None = None
        self.dur = dur
        self.c = cpu
        self.r = ram_gb * GIB
        self.anon_now: int | None = None
        self.anon_max: int | None = None
        self.cpu_s: float | None = None
        self.mem_peak: int | None = None
        self.oom_kill: int | None = None
        self.psi: dict[str, float | None] | None = None
        self.paso: gate_history.Paso | None = None


def parse_env(rest: list[str], step_id: str) -> tuple[dict[str, str], list[str]]:
    env: dict[str, str] = {}
    while rest:
        match = NAME_RE.match(rest[0])
        if not match:
            break
        name, value = match.groups()
        if name not in ALLOWED_ENV:
            die(f"{name} is not in the closed list of the step environment (D-F3j): step {step_id} not started")
        env[name] = value
        rest = rest[1:]
    if not rest:
        die(f"step {step_id} has no order")
    return env, rest


def target_dir(name: str) -> str:
    """CI-F3.3c (D-F3g): the target of a resource is <CARGO_TARGET_DIR>/<tree key>/<name>. The tree key is the name of
    the tree's directory and 12 hex of the sha256 of its realpath, so two trees under one root never share a target.
    The directory names its tree in .gate-tree, and one that names another tree is refused before cargo runs."""
    root = os.environ.get("CARGO_TARGET_DIR", "")
    if not root:
        die(f"target {name}: CARGO_TARGET_DIR is not set, so there is no root to put the target under (D-F3g)")
    tree = os.path.realpath(repo_root())
    key = f"{os.path.basename(tree)}-{hashlib.sha256(tree.encode()).hexdigest()[:12]}"
    path = os.path.join(root, key, name)
    marker = os.path.join(path, ".gate-tree")
    try:
        os.makedirs(path, exist_ok=True)
        named = read_text(marker)
        if named is None:
            with open(marker, "w", encoding="utf-8") as fh:
                fh.write(f"{tree}\n")
    except OSError as err:
        die(f"target {path} cannot be made or marked ({err}); CARGO_TARGET_DIR has to be a directory this user can write (D-F3g)")
    named = read_text(marker)
    if named is not None and named.strip() != tree:
        die(f"target {path} belongs to {named.strip()}, not to {tree}: refused before cargo runs (D-F3g)")
    return path


def parse_row(line: str, number: int) -> Step:
    tok = line.split()
    if len(tok) < 9:
        die(f"table line {number}: needs idx id deps recursos clase cpu ram_gb dur_s and an order")
    try:
        idx, cpu, ram_gb, dur = int(tok[0]), float(tok[5]), float(tok[6]), float(tok[7])
    except ValueError:
        die(f"table line {number}: idx, cpu, ram_gb and dur_s have to be numbers")
    if tok[4] not in CLASSES:
        die(f"table line {number}: class {tok[4]} is not normal, exclusive or boundary")
    deps = [] if tok[2] == "-" else tok[2].split(",")
    recursos = [] if tok[3] == "-" else tok[3].split(",")
    if sum(r.startswith("target:") for r in recursos) > 1:
        die(f"table line {number}: a row names one target, and {tok[1]} names more (D-F3g)")
    env, argv = parse_env(tok[8:], tok[1])
    return Step(idx, tok[1], deps, recursos, tok[4], cpu, ram_gb, dur, env, argv)


def unique_steps(steps: list[Step]) -> dict[str, Step]:
    """The steps by id, refusing a repeated id or idx in table order."""
    by_id: dict[str, Step] = {}
    idxs: set[int] = set()
    for step in steps:
        if step.id in by_id:
            die(f"step id {step.id} appears twice")
        if step.idx in idxs:
            die(f"idx {step.idx} appears twice")
        by_id[step.id] = step
        idxs.add(step.idx)
    return by_id


def validate_table(steps: list[Step]) -> None:
    if not steps:
        die("the table has no steps")
    by_id = unique_steps(steps)
    for step in steps:
        for dep in step.deps:
            if dep not in by_id or by_id[dep].idx >= step.idx:
                die(f"step {step.id} depends on {dep}, which is not a step with a lower idx")


def parse_table(path: str) -> list[Step]:
    steps: list[Step] = []
    with open(path, encoding="utf-8") as fh:
        for number, line in enumerate(fh, 1):
            line = line.strip()
            if line and not line.startswith("#"):
                steps.append(parse_row(line, number))
    validate_table(steps)
    return sorted(steps, key=lambda s: s.idx)


# --- cgroups and capacity -----------------------------------------------------


def scope_dir() -> str | None:
    root = os.environ.get("GATE_CGROUP_ROOT", "/sys/fs/cgroup")
    self_path = os.environ.get("GATE_PROC_SELF_CGROUP", "/proc/self/cgroup")
    text = read_text(self_path) or ""
    for line in text.splitlines():
        if line.startswith("0::"):
            return root + line[3:].strip().rstrip("/")
    return None


def read_pids(directory: str) -> set[int]:
    text = read_text(os.path.join(directory, "cgroup.procs")) or ""
    return {int(p) for p in text.split() if p.isdigit()}


def move_pid(destination: str, pid: int) -> None:
    try:
        with open(os.path.join(destination, "cgroup.procs"), "w", encoding="utf-8") as fh:
            fh.write(str(pid))
    except OSError:
        pass


def delegate(scope: str | None) -> bool:
    """Moves the planner and its scope's processes to scope/juez, then enables controllers."""
    if scope is None or not os.access(os.path.join(scope, "cgroup.subtree_control"), os.W_OK):
        return False
    juez = os.path.join(scope, "juez")
    try:
        os.makedirs(juez, exist_ok=True)
        for pid in sorted(read_pids(scope) | {os.getpid()}):
            move_pid(juez, pid)
        with open(os.path.join(scope, "cgroup.subtree_control"), "w", encoding="utf-8") as fh:
            fh.write("+cpu +memory +pids")
    except OSError:
        return False
    return True


def cgroup_limits() -> tuple[str, str]:
    lock = os.path.join(repo_root(), "scripts", "gate-lock.sh")
    order = 'source "$1"; gate_cgroup_read; printf "%s|%s" "$GATE_CG_MEM_KB" "$GATE_CG_CPUS"'
    res = subprocess.run(
        ["bash", "-c", order, "gate-dag", lock], capture_output=True, text=True, check=False
    )
    if res.returncode != 0:
        die("gate_cgroup_read refused to read the limits: " + res.stderr.strip())
    mem_kb, _, cpus = res.stdout.partition("|")
    return mem_kb.strip(), cpus.strip()


def total_memory() -> int:
    return os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")


def capacities() -> tuple[int, int]:
    """Returns (C cores, R bytes): R is the scope's memory less max(4 GiB, 10 %)."""
    mem_kb, cpus = cgroup_limits()
    margin = int(mem_kb) * 1024 if mem_kb.isdigit() else total_memory()
    ram = margin - max(MIN_RESERVE, margin // 10)
    cpu = int(cpus) if cpus.isdigit() and int(cpus) > 0 else (os.cpu_count() or 1)
    return cpu, ram


# --- history ------------------------------------------------------------------


def is_serial_run(record: dict[str, Any], tabla: str) -> bool:
    return record.get("modo") == "serie" and record.get("tabla") == tabla and record.get("rc") == 0


def is_duration(seg: Any) -> bool:
    """A number of seconds above 0: a bool or a 0 is not one to learn from (H9)."""
    return isinstance(seg, (int, float)) and not isinstance(seg, bool) and seg > 0


def is_step_sample(record: dict[str, Any], tabla: str) -> bool:
    """A step of a green serial run of this table, with a duration to learn from."""
    return is_serial_run(record, tabla) and is_duration(record.get("seg"))


def load_history(tabla: str) -> tuple[dict[Any, list[dict[str, Any]]], list[dict[str, Any]]]:
    steps_by_id: dict[Any, list[dict[str, Any]]] = {}
    runs: list[dict[str, Any]] = []
    for day in sorted(glob.glob(os.path.join(gate_history.history_root(), "*"))):
        for record in gate_history.leer_jsonl(os.path.join(day, "pasos.jsonl")):
            if is_step_sample(record, tabla):
                steps_by_id.setdefault(record.get("id"), []).append(record)
        runs += [r for r in gate_history.leer_jsonl(os.path.join(day, "corridas.jsonl")) if is_serial_run(r, tabla)]
    return steps_by_id, runs


# --- critical path ------------------------------------------------------------


def ranks(steps: list[Step], dur: dict[str, float]) -> tuple[dict[str, float], dict[str, Step | None]]:
    """rank_u(j) = dur(j) + max rank_u(successor), and the successor that gives it."""
    successors: dict[str, list[Step]] = {s.id: [] for s in steps}
    for step in steps:
        for dep in step.deps:
            successors[dep].append(step)
    rank: dict[str, float] = {}
    nxt: dict[str, Step | None] = {}
    for step in sorted(steps, key=lambda s: -s.idx):
        best = max(successors[step.id], key=lambda t: rank[t.id], default=None)
        nxt[step.id] = best
        rank[step.id] = dur[step.id] + (rank[best.id] if best else 0.0)
    return rank, nxt


def critical_chain(
    steps: list[Step], rank: dict[str, float], nxt: dict[str, Step | None]
) -> tuple[float, str]:
    top = max(steps, key=lambda s: rank[s.id])
    chain: list[str] = []
    cur: Step | None = top
    while cur is not None:
        chain.append(cur.id)
        cur = nxt[cur.id]
    return rank[top.id], " -> ".join(chain)


# --- the planner --------------------------------------------------------------


def make_run_dir() -> str:
    """This run's scratch: a fresh directory with its pasos/ (the logs) and tmp/ (each TMPDIR)."""
    run_dir = tempfile.mkdtemp(prefix="memory-industry-dag.")
    for sub in ("pasos", "tmp"):
        os.makedirs(os.path.join(run_dir, sub), exist_ok=True)
    return run_dir


# The cores and the anonymous memory each earlier run of a step used: the samples its
# estimate is drawn from. A record without the number, or of a step that took no time, says nothing.
def observed_cores(previous: list[dict[str, Any]]) -> list[float]:
    return [r["cpu_s"] / r["seg"] for r in previous if r.get("cpu_s") is not None and r["seg"] > 0]


def observed_anon(previous: list[dict[str, Any]]) -> list[int]:
    return [r["anon_max"] for r in previous if r.get("anon_max") is not None]


def known_values(done: list[Step], attr: str) -> dict[str, Any]:
    return {s.id: getattr(s, attr) for s in done if getattr(s, attr) is not None}


NOMBRE_COTA = {"paso": "longest step", "cpu": "CPU", "ram": "RAM"}


class Planner:
    def __init__(self, steps: list[Step], dag: bool, tabla: str, timed_end: str | None = None) -> None:
        self.steps = steps
        self.by_id = {s.id: s for s in steps}
        self.dag = dag
        self.tabla = tabla
        # CI-F3.3b-2: the line the judge of the test times reads up to; printed once, before the first untimed block.
        self.timed_end = timed_end
        self.timed_end_printed = False
        self.modo = "dag" if dag else "serie"
        self.t0 = time.monotonic()
        self.run_dir = make_run_dir()
        self.run_id = os.path.basename(self.run_dir)
        root = repo_root()
        sha, arbol_sucio = git_state(root)
        self.corrida: gate_history.Corrida | None = gate_history.run_start(
            self.modo,
            sha,
            gate_history.huella(root),
            tabla,
            run=self.run_id,
            arbol_sucio=arbol_sucio,
        )
        self.hist_steps, self.hist_runs = load_history(tabla)
        self.scope = scope_dir()
        self.delegado = delegate(self.scope)
        self.cpu, self.ram = capacities()
        self.cpu_cap = math.floor(SAFETY * self.cpu)
        self.red: Step | None = None
        self.no_verdict: str | None = None
        self.cursor = 0
        self.last_sample = 0.0
        self.lb_ref: float | None = None
        self.lb_run: float | None = None
        self.wake_r: int | None = None
        self.wake_w: int | None = None
        self.rank, self.nxt = self.estimated_ranks()
        self.solo = not (dag and self.delegado and self.hist_runs)
        self.notes()

    def estimated_ranks(self) -> tuple[dict[str, float], dict[str, Step | None]]:
        """Each step's estimate from its history, then the critical-path ranks over those durations."""
        for step in self.steps:
            self.estimate(step)
        return ranks(self.steps, {s.id: s.dur for s in self.steps})

    def notes(self) -> None:
        if self.dag and not self.delegado:
            print("note: the scope is not delegated: nothing is measured or killed per step, so the planner runs in series")
        elif self.dag and not self.hist_runs:
            print(f"note: no serial history for this table ({self.tabla[:12]}): the planner runs in series")

    def estimate(self, step: Step) -> None:
        previous = self.hist_steps.get(step.id, [])[-WINDOW:]
        if not previous:
            return
        step.dur = statistics.median(r["seg"] for r in previous)
        cores = observed_cores(previous)
        if cores:
            step.c = p95(cores)
        rams = observed_anon(previous)
        if rams:
            step.r = p95(rams) * SAFETY

    def now(self) -> float:
        return time.monotonic() - self.t0

    def running(self) -> list[Step]:
        return [s for s in self.steps if s.state == "running"]

    def unfinished(self) -> bool:
        return any(s.state in ("pending", "running") for s in self.steps)

    # admission

    def deps_green(self, step: Step) -> bool:
        return all(self.by_id[d].state == "done" and self.by_id[d].rc == 0 for d in step.deps)

    def ready(self) -> list[Step]:
        ready: list[Step] = []
        for step in self.steps:
            if step.state != "pending":
                continue
            if self.deps_green(step):
                if step.t_ready is None:
                    step.t_ready = self.now()
                ready.append(step)
        if self.solo:
            return ready
        return sorted(ready, key=lambda s: (-self.rank[s.id], s.idx))

    def admit(self) -> None:
        for step in self.ready():
            reason = self.wait_reason(step)
            if reason is None:
                self.start(step)
            else:
                step.motivo = reason

    def wait_reason(self, step: Step) -> str | None:
        alive = self.running()
        if self.exclusive_wait(step, alive):
            return "exclusive"
        if not alive:
            return None
        if self.solo:
            return "serie"
        return self.capacity_wait(step, alive)

    def exclusive_wait(self, step: Step, alive: list[Step]) -> bool:
        if step.clase in EXCLUSIVE:
            return bool(alive) or any(o.idx < step.idx and o.state == "pending" for o in self.steps)
        return any(o.clase in EXCLUSIVE for o in alive)

    def capacity_wait(self, step: Step, alive: list[Step]) -> str | None:
        if sum(o.c for o in alive) + step.c > self.cpu_cap:
            return "cpu"
        if sum(max(o.anon_now or 0, o.r) for o in alive) + step.r > self.ram:
            return "ram"
        return None

    # steps

    def lane_ticket(self, step: Step) -> str | None:
        """CI-F3.3b-1b-i (D-F3k): under the gate's lock, a one-use ticket for the step in the lock's lanes."""
        lock = os.environ.get("GATE_LOCK")
        if not lock or not os.environ.get("CUBA_GATE_LOCK_OWNER"):
            return None
        token = secrets.token_hex(16)
        lanes = os.path.join(lock, "lanes")
        os.makedirs(lanes, exist_ok=True)
        with open(os.path.join(lanes, f"{step.idx}-{step.id}"), "w", encoding="utf-8") as fh:
            fh.write(token)
        return token

    def step_env(self, step: Step, token: str | None = None) -> dict[str, str]:
        tmp = os.path.join(self.run_dir, "tmp", step.id)
        os.makedirs(tmp, exist_ok=True)
        env = dict(os.environ)
        env["TMPDIR"] = tmp
        env.update(step.env)
        targets = [r[len("target:"):] for r in step.recursos if r.startswith("target:")]
        if targets:
            if "CARGO_TARGET_DIR" in step.env:
                die(f"step {step.id} names CARGO_TARGET_DIR and a target: the target sets it (D-F3g)")
            env["CARGO_TARGET_DIR"] = target_dir(targets[0])
        env.pop("CUBA_GATE_LANE_TOKEN", None)
        if token is not None:
            env["CUBA_GATE_LANE_TOKEN"] = token
        changed = sorted(k for k, v in env.items() if os.environ.get(k) != v)
        outside = [k for k in changed if k not in ALLOWED_ENV]
        if outside:
            die(f"step {step.id} would change {', '.join(outside)}, outside the closed list (D-F3j)")
        print(f"dag: {step.id} env: {' '.join(changed)}")
        return env

    def cgroup_for(self, step: Step) -> str | None:
        if not self.delegado:
            return None
        path = os.path.join(self.scope, f"paso-{step.idx}-{step.id}")
        os.makedirs(path, exist_ok=True)
        return path

    def start(self, step: Step) -> None:
        env = self.step_env(step, self.lane_ticket(step))
        step.cg = self.cgroup_for(step)
        step.log = os.path.join(self.run_dir, "pasos", f"{step.idx}-{step.id}.log")
        with open(step.log, "wb") as log:
            step.proc = subprocess.Popen(
                step.argv,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
                preexec_fn=step_entry(step.cg),
            )
        step.t_start = self.now()
        if self.corrida is not None:
            step.paso = gate_history.step_start(self.corrida, step.idx, step.id)
        step.state = "running"
        self.sample_step(step)

    def anon_of(self, step: Step) -> int | None:
        if not step.cg:
            return None
        stat = read_kv(os.path.join(step.cg, "memory.stat"))
        return stat.get("anon") if stat else None

    def sample_step(self, step: Step) -> None:
        value = self.anon_of(step)
        if value is not None:
            step.anon_now = value
            step.anon_max = max(step.anon_max or 0, value)

    def sample(self) -> None:
        now = self.now()
        if now - self.last_sample < TICK_S:
            return
        self.last_sample = now
        for step in self.running():
            self.sample_step(step)

    def wait_event(self) -> None:
        select.select([self.wake_r], [], [], TICK_S)
        try:
            while os.read(self.wake_r, 512):
                pass
        except BlockingIOError:
            pass

    def reap(self) -> None:
        for step in self.running():
            rc = step.proc.poll()
            if rc is not None:
                self.finish(step, rc)
                if self.no_verdict:
                    return

    def finish(self, step: Step, rc: int) -> None:
        step.rc = rc
        step.t_end = self.now()
        step.state = "done"
        self.measure(step)
        self.record_step(step)
        remove_cgroup(step.cg)
        if step.parado:
            return
        if rc < 0 or (step.oom_kill or 0) > 0:
            self.no_verdict = self.no_verdict_text(step, rc)
            return
        if rc != 0:
            self.mark_red(step)

    def no_verdict_text(self, step: Step, rc: int) -> str:
        if step.oom_kill:
            return f"no verdict: {step.id} was killed by the kernel (memory.events oom_kill {step.oom_kill}); the judge exits 2"
        return f"no verdict: {step.id} ended by signal {-rc} that the planner did not send; the judge exits 2"

    def measure(self, step: Step) -> None:
        if not step.cg:
            return
        cpu = read_kv(os.path.join(step.cg, "cpu.stat"))
        step.cpu_s = cpu["usage_usec"] / 1e6 if cpu and "usage_usec" in cpu else None
        step.mem_peak = read_int(os.path.join(step.cg, "memory.peak"))
        events = read_kv(os.path.join(step.cg, "memory.events"))
        step.oom_kill = events.get("oom_kill") if events else None
        self.sample_step(step)
        step.psi = {
            name: psi_some_avg10(os.path.join(step.cg, f"{name}.pressure"))
            for name in ("cpu", "memory", "io")
        }

    def record_step(self, step: Step) -> None:
        if self.corrida is None or step.paso is None:
            return
        gate_history.step_end(
            self.corrida,
            step.paso,
            step.rc,
            inicio=step.t_start,
            fin=step.t_end,
            espera_s=step.t_start - step.t_ready,
            motivo=step.motivo,
            parado=step.parado,
            cpu_s=step.cpu_s,
            mem_peak=step.mem_peak,
            anon_max=step.anon_max,
            oom_kill=step.oom_kill,
            psi=step.psi,
        )

    # verdict

    def mark_red(self, step: Step) -> None:
        if self.red is not None and step.idx >= self.red.idx:
            return
        self.red = step
        self.stop_after(step.idx)

    def stop_after(self, idx: int) -> None:
        """What comes after idx in the serial order: not started if pending, killed if running."""
        for other in self.steps:
            if other.idx > idx and other.state == "pending":
                other.state = "stopped"
        for other in self.running():
            if other.idx > idx:
                self.kill(other)

    def kill(self, step: Step) -> None:
        step.parado = True
        if step.cg and os.path.exists(os.path.join(step.cg, "cgroup.kill")):
            try:
                with open(os.path.join(step.cg, "cgroup.kill"), "w", encoding="utf-8") as fh:
                    fh.write("1")
            except OSError:
                pass
        try:
            os.killpg(step.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def drain(self) -> None:
        """Kills and reaps whatever is still running, however the loop ended."""
        alive = self.running()
        for step in alive:
            self.kill(step)
        for step in alive:
            self.finish(step, step.proc.wait())

    def flush(self) -> None:
        """Prints each block in idx order, once every lower idx has finished."""
        while self.cursor < len(self.steps):
            step = self.steps[self.cursor]
            if self.red is not None and step.idx > self.red.idx:
                print(f"dag: {step.id} stopped: step {self.red.idx} before it in the serial order failed")
            elif step.state == "done":
                self.emit_block(step)
            else:
                return
            self.cursor += 1

    def emit_block(self, step: Step) -> None:
        if step.clase == "untimed" and self.timed_end and not self.timed_end_printed:
            print(self.timed_end)
            self.timed_end_printed = True
        text = read_text(step.log) or ""
        sys.stdout.write(text)
        if text and not text.endswith("\n"):
            sys.stdout.write("\n")
        # CI-F3.3b-2 (D-F3e): the block reaches the pipe when its step ends, not when the planner does.
        sys.stdout.flush()

    # loop

    def loop(self) -> None:
        while self.unfinished():
            self.admit()
            if not self.running():
                if self.unfinished():
                    die("planner stuck: no step is running and none can start")
                break
            self.wait_event()
            self.reap()
            if self.no_verdict:
                return
            self.sample()
            self.flush()

    def setup_signal(self) -> None:
        self.wake_r, self.wake_w = os.pipe()
        os.set_blocking(self.wake_r, False)
        os.set_blocking(self.wake_w, False)
        signal.signal(signal.SIGCHLD, lambda _n, _f: None)
        signal.set_wakeup_fd(self.wake_w)

    def teardown_signal(self) -> None:
        signal.set_wakeup_fd(-1)
        signal.signal(signal.SIGCHLD, signal.SIG_DFL)
        os.close(self.wake_r)
        os.close(self.wake_w)

    def execute(self) -> int:
        self.setup_signal()
        try:
            self.loop()
        finally:
            self.drain()
            self.teardown_signal()
        return self.close()

    def close(self) -> int:
        if self.no_verdict:
            sys.stderr.write(self.no_verdict + "\n")
            self.annotate_run(2)
            return 2
        self.flush()
        code = 0
        if self.red is not None:
            print(f"FAIL: {self.red.id} exited {self.red.rc} (lowest index that failed; the serial order stops here)")
            code = self.red.rc
        self.summary()
        self.annotate_run(code)
        return code

    # summary

    def measured(self) -> tuple[dict[str, float], dict[str, Any], dict[str, Any]]:
        done = [s for s in self.steps if s.state == "done"]
        return (
            {s.id: s.t_end - s.t_start for s in done},
            known_values(done, "cpu_s"),
            known_values(done, "anon_max"),
        )

    def reference(self, key: str) -> dict[str, Any]:
        values: dict[str, Any] = {}
        for step in self.steps:
            previous = [r[key] for r in self.hist_steps.get(step.id, [])[-WINDOW:] if r.get(key) is not None]
            if previous:
                values[step.id] = statistics.median(previous)
        return values

    def all_measured(self, values: dict[str, Any]) -> bool:
        return all(values.get(s.id) is not None for s in self.steps)

    def lower_bounds(self, dur: dict[str, Any], cpu: dict[str, Any], anon: dict[str, Any]) -> dict[str, Any]:
        """gate_history.lower_bounds over this table's steps; a dependency cycle is a refusal."""
        records = [
            {"id": s.id, "seg": dur[s.id], "cpu_s": cpu.get(s.id), "anon_max": anon.get(s.id)}
            for s in self.steps
        ]
        table = {"C": self.cpu, "R": self.ram, "deps": {s.id: s.deps for s in self.steps}}
        try:
            return gate_history.lower_bounds(records, table)
        except ValueError as err:
            die(str(err))

    def bound_text(self, cota: str | None, dur: dict[str, Any]) -> str:
        """The term that gave the bound, the critical path spelled out step by step."""
        if cota == "camino":
            rank, nxt = ranks(self.steps, dur)
            _, chain_text = critical_chain(self.steps, rank, nxt)
            return f"critical path: {chain_text}"
        return NOMBRE_COTA.get(cota or "", "")

    def bound(self, dur: dict[str, Any], cpu: dict[str, Any], anon: dict[str, Any]) -> tuple[float | None, str]:
        """LB over the run's steps, from gate_history.lower_bounds; None while a duration is unmeasured."""
        if not self.all_measured(dur):
            return None, ""
        result = self.lower_bounds(dur, cpu, anon)
        return result["LB"], self.bound_text(result["cota"], dur)

    def summary(self) -> None:
        total = self.now()
        self.lb_ref, ref_text = self.bound(
            self.reference("seg"), self.reference("cpu_s"), self.reference("anon_max")
        )
        dur, cpu, anon = self.measured()
        self.lb_run, run_text = self.bound(dur, cpu, anon)
        print(
            f"dag: T={total:.2f}; LB_ref={fmt(self.lb_ref)} ({ref_text or 'no history'}); "
            f"T/LB_ref={ratio(total, self.lb_ref)}; LB_run={fmt(self.lb_run)} ({run_text or 'not measured'}); "
            f"T/LB_run={ratio(total, self.lb_run)}; steps={len(self.steps)}; C={self.cpu}; "
            f"R={self.ram / GIB:.2f} GiB; mode={self.modo}"
        )

    def ran_steps(self) -> list[Step]:
        return [s for s in self.steps if s.rc is not None]

    def annotate_run(self, code: int) -> None:
        if self.corrida is None:
            return
        picos = [s.mem_peak for s in self.ran_steps()]
        oom = [s.oom_kill for s in self.ran_steps()]
        gate_history.run_end(
            self.corrida,
            code,
            T=self.now(),
            LB_ref=self.lb_ref,
            LB_run=self.lb_run,
            C=self.cpu,
            R=self.ram,
            mem_peak=max(picos) if picos and all(p is not None for p in picos) else None,
            psi=system_psi(),
            load=loadavg(),
            hilos=self.cpu,
            oom_kill=sum(oom) if oom and all(o is not None for o in oom) else None,
        )


def remove_cgroup(path: str | None) -> None:
    """A finished step's own cgroup goes away; one that cannot be removed is left, unjudged."""
    if path:
        try:
            os.rmdir(path)
        except OSError:
            pass


def fmt(value: float | None) -> str:
    return "n/a" if value is None or value <= 0 else f"{value:.2f}"


def ratio(total: float, bound: float | None) -> str:
    return "n/a" if bound is None or bound <= 0 else f"{total / bound:.2f}"


def cgroup_entry(path: str) -> Callable[[], None]:
    """Runs in the child before exec: the step is in its own cgroup before any of its code runs."""

    def enter() -> None:
        try:
            with open(os.path.join(path, "cgroup.procs"), "w", encoding="utf-8") as fh:
                fh.write(str(os.getpid()))
        except OSError:
            pass

    return enter


def step_entry(path: str | None) -> Callable[[], None]:
    """Runs in the child before exec (CI-J10): SIGINT and SIGQUIT back to their default, then the step's cgroup if it
    has one. The planner was started as a background job of merge-gate.sh, where bash ignores both on entry and cannot
    reset them, so every step and everything under it (a gate's trap for INT, D2) would inherit them ignored. The
    planner's own disposition is not touched: stop_on_term is its TERM."""
    enter_cgroup = cgroup_entry(path) if path else None

    def enter() -> None:
        signal.signal(signal.SIGINT, signal.SIG_DFL)
        signal.signal(signal.SIGQUIT, signal.SIG_DFL)
        if enter_cgroup is not None:
            enter_cgroup()

    return enter


def stop_on_term(_signum: int, _frame: object) -> NoReturn:
    """CI-F3.3b-2: the gate's exit trap sends TERM here; the exit unwinds through execute, which kills the steps."""
    raise SystemExit(2)


def run_table(path: str, timed_end: str | None = None) -> int:
    mode = os.environ.get("GATE_DAG", "0")
    if mode not in ("0", "1"):
        die(f"GATE_DAG is {mode!r}; it has to be 0 (series) or 1 (DAG)")
    signal.signal(signal.SIGTERM, stop_on_term)
    steps = parse_table(path)
    return Planner(steps, mode == "1", sha256_of(path), timed_end).execute()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Lane planner of the local judge (CI-F3, F3.2).")
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="run a step table: GATE_DAG=0 series, GATE_DAG=1 DAG")
    run.add_argument("--table", required=True)
    run.add_argument("--timed-end", default=None, help="the line printed before the first untimed block (CI-F3.3b-2)")
    args = parser.parse_args(argv)
    try:
        return run_table(args.table, args.timed_end)
    except Refusal as err:
        sys.stderr.write(f"FAIL: {err}\n")
        return 2


if __name__ == "__main__":
    sys.exit(main())
