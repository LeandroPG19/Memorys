#!/usr/bin/env python3
"""Lane planner of the local judge (CI-F3, F3.2).

`run --table <file>` runs the step table. GATE_DAG=0 (the default) is the series: the steps in
idx order, one at a time. GATE_DAG=1 plans the DAG: a step starts when its dependencies exited 0,
and it is admitted by CPU and RAM, recomputed on every event (a child exit, and a 1 s tick as a
backup). The verdict is the series one: the red with the lowest idx wins, the steps before it
finish, the ones after it are killed (D-F3d). A kernel kill (OOM, or a signal the planner did not
send) gives no verdict: exit 2 (D-F3f).

Not in F3.2: the model and cluster semaphores (D-F3q), the lane token (D-F3k), the target per lane
(D-F3g), the removal of old run directories (D-F3i) and the per-binary launcher (D-F3u). `exclusive`
and `boundary` run alone, after every lower idx: the conservative form of D-F3l. The resource
column is parsed and ignored until F3.4.
"""

from __future__ import annotations

import argparse
import fcntl
import glob
import hashlib
import json
import math
import os
import re
import select
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone

GIB = 1 << 30
REPO = "memory-industry"
TICK_S = 1.0
SAFETY = 1.25
WINDOW = 5
MIN_RESERVE = 4 * GIB
CLASSES = frozenset({"normal", "exclusive", "boundary"})
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
_warned = False


class Refusal(Exception):
    """Exit 2: a table or environment problem, or a planner that cannot decide."""


def die(message):
    raise Refusal(message)


def repo_root():
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def rounded(value, digits=3):
    return None if value is None else round(value, digits)


def read_text(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        return None


def read_kv(path):
    text = read_text(path)
    if text is None:
        return None
    values = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[1].isdigit():
            values[parts[0]] = int(parts[1])
    return values


def read_int(path):
    text = read_text(path)
    return int(text) if text is not None and text.strip().isdigit() else None


def psi_some_avg10(path):
    text = read_text(path)
    for line in (text or "").splitlines():
        if line.startswith("some"):
            for field in line.split():
                if field.startswith("avg10="):
                    return float(field[len("avg10=") :])
    return None


def p95(values):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(0.95 * len(ordered)) - 1)]


def sha256_of(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


# --- the step table -----------------------------------------------------------


class Step:
    def __init__(self, idx, step_id, deps, clase, cpu, ram_gb, dur, env, argv):
        self.idx = idx
        self.id = step_id
        self.deps = deps
        self.clase = clase
        self.cpu_tab = cpu
        self.ram_tab = ram_gb
        self.dur_tab = dur
        self.env = env
        self.argv = argv
        self.state = "pending"
        self.proc = None
        self.log = None
        self.cg = None
        self.rc = None
        self.parado = False
        self.motivo = None
        self.t_ready = None
        self.t_start = None
        self.t_end = None
        self.dur = dur
        self.c = cpu
        self.r = ram_gb * GIB
        self.anon_now = None
        self.anon_max = None
        self.cpu_s = None
        self.mem_peak = None
        self.oom_kill = None
        self.psi = None


def parse_env(rest, step_id):
    env = {}
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


def parse_row(line, number):
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
    env, argv = parse_env(tok[8:], tok[1])
    return Step(idx, tok[1], deps, tok[4], cpu, ram_gb, dur, env, argv)


def validate_table(steps):
    if not steps:
        die("the table has no steps")
    by_id = {}
    idxs = set()
    for step in steps:
        if step.id in by_id:
            die(f"step id {step.id} appears twice")
        if step.idx in idxs:
            die(f"idx {step.idx} appears twice")
        by_id[step.id] = step
        idxs.add(step.idx)
    for step in steps:
        for dep in step.deps:
            if dep not in by_id or by_id[dep].idx >= step.idx:
                die(f"step {step.id} depends on {dep}, which is not a step with a lower idx")


def parse_table(path):
    steps = []
    with open(path, encoding="utf-8") as fh:
        for number, line in enumerate(fh, 1):
            line = line.strip()
            if line and not line.startswith("#"):
                steps.append(parse_row(line, number))
    validate_table(steps)
    return sorted(steps, key=lambda s: s.idx)


# --- cgroups and capacity -----------------------------------------------------


def scope_dir():
    root = os.environ.get("GATE_CGROUP_ROOT", "/sys/fs/cgroup")
    self_path = os.environ.get("GATE_PROC_SELF_CGROUP", "/proc/self/cgroup")
    text = read_text(self_path) or ""
    for line in text.splitlines():
        if line.startswith("0::"):
            return root + line[3:].strip().rstrip("/")
    return None


def read_pids(directory):
    text = read_text(os.path.join(directory, "cgroup.procs")) or ""
    return {int(p) for p in text.split() if p.isdigit()}


def move_pid(destination, pid):
    try:
        with open(os.path.join(destination, "cgroup.procs"), "w", encoding="utf-8") as fh:
            fh.write(str(pid))
    except OSError:
        pass


def delegate(scope):
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


def cgroup_limits():
    lock = os.path.join(repo_root(), "scripts", "gate-lock.sh")
    order = 'source "$1"; gate_cgroup_read; printf "%s|%s" "$GATE_CG_MEM_KB" "$GATE_CG_CPUS"'
    res = subprocess.run(
        ["bash", "-c", order, "gate-dag", lock], capture_output=True, text=True, check=False
    )
    if res.returncode != 0:
        die("gate_cgroup_read refused to read the limits: " + res.stderr.strip())
    mem_kb, _, cpus = res.stdout.partition("|")
    return mem_kb.strip(), cpus.strip()


def total_memory():
    return os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")


def capacities():
    """Returns (C cores, R bytes): R is the scope's memory less max(4 GiB, 10 %)."""
    mem_kb, cpus = cgroup_limits()
    margin = int(mem_kb) * 1024 if mem_kb.isdigit() else total_memory()
    ram = margin - max(MIN_RESERVE, margin // 10)
    cpu = int(cpus) if cpus.isdigit() and int(cpus) > 0 else (os.cpu_count() or 1)
    return cpu, ram


# --- history ------------------------------------------------------------------


def history_root():
    return os.environ.get("CI_TIEMPOS_DIR") or os.path.expanduser("~/.cache/ci-tiempos")


def read_jsonl(path):
    records = []
    try:
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                try:
                    record = json.loads(line)
                except ValueError:
                    continue
                if isinstance(record, dict):
                    records.append(record)
    except OSError:
        pass
    return records


def is_serial_run(record, tabla):
    return record.get("modo") == "serie" and record.get("tabla") == tabla and record.get("rc") == 0


def load_history(tabla):
    steps_by_id = {}
    runs = []
    for day in sorted(glob.glob(os.path.join(history_root(), REPO, "*"))):
        for record in read_jsonl(os.path.join(day, "pasos.jsonl")):
            if is_serial_run(record, tabla) and isinstance(record.get("seg"), (int, float)):
                steps_by_id.setdefault(record.get("id"), []).append(record)
        runs += [r for r in read_jsonl(os.path.join(day, "corridas.jsonl")) if is_serial_run(r, tabla)]
    return steps_by_id, runs


def annotate(name, record):
    """Appends one record under flock. A failed write never changes the verdict."""
    global _warned
    day = datetime.now(timezone.utc).strftime("%Y%m%d")
    folder = os.path.join(history_root(), REPO, day)
    try:
        os.makedirs(folder, exist_ok=True)
        with open(os.path.join(folder, name), "a", encoding="utf-8") as fh:
            fcntl.flock(fh, fcntl.LOCK_EX)
            fh.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
            fh.flush()
    except OSError as err:
        if not _warned:
            _warned = True
            sys.stderr.write(f"note: history not written in {folder} ({err}); the verdict does not change\n")


# --- critical path ------------------------------------------------------------


def ranks(steps, dur):
    """rank_u(j) = dur(j) + max rank_u(successor), and the successor that gives it."""
    successors = {s.id: [] for s in steps}
    for step in steps:
        for dep in step.deps:
            successors[dep].append(step)
    rank, nxt = {}, {}
    for step in sorted(steps, key=lambda s: -s.idx):
        best = max(successors[step.id], key=lambda t: rank[t.id], default=None)
        nxt[step.id] = best
        rank[step.id] = dur[step.id] + (rank[best.id] if best else 0.0)
    return rank, nxt


def critical_chain(steps, rank, nxt):
    top = max(steps, key=lambda s: rank[s.id])
    chain, cur = [], top
    while cur is not None:
        chain.append(cur.id)
        cur = nxt[cur.id]
    return rank[top.id], " -> ".join(chain)


# --- the planner --------------------------------------------------------------


class Planner:
    def __init__(self, steps, dag, tabla):
        self.steps = steps
        self.by_id = {s.id: s for s in steps}
        self.dag = dag
        self.tabla = tabla
        self.modo = "dag" if dag else "serie"
        self.t0 = time.monotonic()
        self.run_dir = tempfile.mkdtemp(prefix="memory-industry-dag.")
        self.run_id = os.path.basename(self.run_dir)
        for sub in ("pasos", "tmp"):
            os.makedirs(os.path.join(self.run_dir, sub), exist_ok=True)
        self.hist_steps, self.hist_runs = load_history(tabla)
        self.scope = scope_dir()
        self.delegado = delegate(self.scope)
        self.cpu, self.ram = capacities()
        self.cpu_cap = math.floor(SAFETY * self.cpu)
        self.red = None
        self.no_verdict = None
        self.cursor = 0
        self.last_sample = 0.0
        self.lb_ref = None
        self.lb_run = None
        self.wake_r = self.wake_w = None
        for step in steps:
            self.estimate(step)
        self.rank, self.nxt = ranks(steps, {s.id: s.dur for s in steps})
        self.solo = not (dag and self.delegado and self.hist_runs)
        self.notes()

    def notes(self):
        if self.dag and not self.delegado:
            print("note: the scope is not delegated: nothing is measured or killed per step, so the planner runs in series")
        elif self.dag and not self.hist_runs:
            print(f"note: no serial history for this table ({self.tabla[:12]}): the planner runs in series")

    def estimate(self, step):
        previous = self.hist_steps.get(step.id, [])[-WINDOW:]
        if not previous:
            return
        step.dur = statistics.median(r["seg"] for r in previous)
        cores = [r["cpu_s"] / r["seg"] for r in previous if r.get("cpu_s") is not None and r["seg"] > 0]
        if cores:
            step.c = p95(cores)
        rams = [r["anon_max"] for r in previous if r.get("anon_max") is not None]
        if rams:
            step.r = p95(rams) * SAFETY

    def now(self):
        return time.monotonic() - self.t0

    def running(self):
        return [s for s in self.steps if s.state == "running"]

    def unfinished(self):
        return any(s.state in ("pending", "running") for s in self.steps)

    # admission

    def ready(self):
        ready = []
        for step in self.steps:
            if step.state != "pending":
                continue
            if all(self.by_id[d].state == "done" and self.by_id[d].rc == 0 for d in step.deps):
                if step.t_ready is None:
                    step.t_ready = self.now()
                ready.append(step)
        if self.solo:
            return ready
        return sorted(ready, key=lambda s: (-self.rank[s.id], s.idx))

    def admit(self):
        for step in self.ready():
            reason = self.wait_reason(step)
            if reason is None:
                self.start(step)
            else:
                step.motivo = reason

    def wait_reason(self, step):
        alive = self.running()
        if self.exclusive_wait(step, alive):
            return "exclusive"
        if not alive:
            return None
        if self.solo:
            return "serie"
        return self.capacity_wait(step, alive)

    def exclusive_wait(self, step, alive):
        if step.clase in EXCLUSIVE:
            return bool(alive) or any(o.idx < step.idx and o.state == "pending" for o in self.steps)
        return any(o.clase in EXCLUSIVE for o in alive)

    def capacity_wait(self, step, alive):
        if sum(o.c for o in alive) + step.c > self.cpu_cap:
            return "cpu"
        if sum(max(o.anon_now or 0, o.r) for o in alive) + step.r > self.ram:
            return "ram"
        return None

    # steps

    def step_env(self, step):
        tmp = os.path.join(self.run_dir, "tmp", step.id)
        os.makedirs(tmp, exist_ok=True)
        env = dict(os.environ)
        env["TMPDIR"] = tmp
        env.update(step.env)
        changed = sorted(k for k, v in env.items() if os.environ.get(k) != v)
        outside = [k for k in changed if k not in ALLOWED_ENV]
        if outside:
            die(f"step {step.id} would change {', '.join(outside)}, outside the closed list (D-F3j)")
        print(f"dag: {step.id} env: {' '.join(changed)}")
        return env

    def cgroup_for(self, step):
        if not self.delegado:
            return None
        path = os.path.join(self.scope, f"paso-{step.idx}-{step.id}")
        os.makedirs(path, exist_ok=True)
        return path

    def start(self, step):
        env = self.step_env(step)
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
                preexec_fn=cgroup_entry(step.cg) if step.cg else None,
            )
        step.t_start = self.now()
        step.state = "running"
        self.sample_step(step)

    def anon_of(self, step):
        if not step.cg:
            return None
        stat = read_kv(os.path.join(step.cg, "memory.stat"))
        return stat.get("anon") if stat else None

    def sample_step(self, step):
        value = self.anon_of(step)
        if value is not None:
            step.anon_now = value
            step.anon_max = max(step.anon_max or 0, value)

    def sample(self):
        now = self.now()
        if now - self.last_sample < TICK_S:
            return
        self.last_sample = now
        for step in self.running():
            self.sample_step(step)

    def wait_event(self):
        select.select([self.wake_r], [], [], TICK_S)
        try:
            while os.read(self.wake_r, 512):
                pass
        except BlockingIOError:
            pass

    def reap(self):
        for step in self.running():
            rc = step.proc.poll()
            if rc is not None:
                self.finish(step, rc)
                if self.no_verdict:
                    return

    def finish(self, step, rc):
        step.rc = rc
        step.t_end = self.now()
        step.state = "done"
        self.measure(step)
        self.record_step(step)
        if step.cg:
            try:
                os.rmdir(step.cg)
            except OSError:
                pass
        if step.parado:
            return
        if rc < 0 or (step.oom_kill or 0) > 0:
            self.no_verdict = self.no_verdict_text(step, rc)
            return
        if rc != 0:
            self.mark_red(step)

    def no_verdict_text(self, step, rc):
        if step.oom_kill:
            return f"no verdict: {step.id} was killed by the kernel (memory.events oom_kill {step.oom_kill}); the judge exits 2"
        return f"no verdict: {step.id} ended by signal {-rc} that the planner did not send; the judge exits 2"

    def measure(self, step):
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

    def record_step(self, step):
        annotate(
            "pasos.jsonl",
            {
                "repo": REPO,
                "modo": self.modo,
                "tabla": self.tabla,
                "run": self.run_id,
                "id": step.id,
                "idx": step.idx,
                "inicio": rounded(step.t_start),
                "fin": rounded(step.t_end),
                "seg": rounded(step.t_end - step.t_start),
                "rc": step.rc,
                "parado": step.parado,
                "espera_s": rounded(step.t_start - step.t_ready),
                "motivo": step.motivo,
                "cpu_s": rounded(step.cpu_s),
                "mem_peak": step.mem_peak,
                "anon_max": step.anon_max,
                "oom_kill": step.oom_kill,
                "psi": step.psi,
            },
        )

    # verdict

    def mark_red(self, step):
        if self.red is not None and step.idx >= self.red.idx:
            return
        self.red = step
        for other in self.steps:
            if other.idx > step.idx and other.state == "pending":
                other.state = "stopped"
        for other in self.running():
            if other.idx > step.idx:
                self.kill(other)

    def kill(self, step):
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

    def drain(self):
        """Kills and reaps whatever is still running, however the loop ended."""
        alive = self.running()
        for step in alive:
            self.kill(step)
        for step in alive:
            self.finish(step, step.proc.wait())

    def flush(self):
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

    def emit_block(self, step):
        text = read_text(step.log) or ""
        sys.stdout.write(text)
        if text and not text.endswith("\n"):
            sys.stdout.write("\n")

    # loop

    def loop(self):
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

    def setup_signal(self):
        self.wake_r, self.wake_w = os.pipe()
        os.set_blocking(self.wake_r, False)
        os.set_blocking(self.wake_w, False)
        signal.signal(signal.SIGCHLD, lambda _n, _f: None)
        signal.set_wakeup_fd(self.wake_w)

    def teardown_signal(self):
        signal.set_wakeup_fd(-1)
        signal.signal(signal.SIGCHLD, signal.SIG_DFL)
        os.close(self.wake_r)
        os.close(self.wake_w)

    def execute(self):
        self.setup_signal()
        try:
            self.loop()
        finally:
            self.drain()
            self.teardown_signal()
        return self.close()

    def close(self):
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

    def measured(self):
        done = [s for s in self.steps if s.state == "done"]
        return (
            {s.id: s.t_end - s.t_start for s in done},
            {s.id: s.cpu_s for s in done if s.cpu_s is not None},
            {s.id: s.anon_max for s in done if s.anon_max is not None},
        )

    def reference(self, key):
        values = {}
        for step in self.steps:
            previous = [r[key] for r in self.hist_steps.get(step.id, [])[-WINDOW:] if r.get(key) is not None]
            if previous:
                values[step.id] = statistics.median(previous)
        return values

    def bound(self, dur, cpu, anon):
        """LB = max(critical path, sum(cpu_s)/C, sum(anon*seg)/R, longest step); None if unmeasured."""
        if any(dur.get(s.id) is None for s in self.steps):
            return None, ""
        rank, nxt = ranks(self.steps, dur)
        chain_len, chain = critical_chain(self.steps, rank, nxt)
        terms = {"critical path": chain_len, "longest step": max(dur.values())}
        if all(cpu.get(s.id) is not None for s in self.steps):
            terms["CPU"] = sum(cpu[s.id] for s in self.steps) / self.cpu
        if self.ram > 0 and all(anon.get(s.id) is not None for s in self.steps):
            terms["RAM"] = sum(anon[s.id] * dur[s.id] for s in self.steps) / self.ram
        which = max(terms, key=terms.get)
        return terms[which], (f"critical path: {chain}" if which == "critical path" else which)

    def summary(self):
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

    def annotate_run(self, code):
        annotate(
            "corridas.jsonl",
            {
                "repo": REPO,
                "modo": self.modo,
                "tabla": self.tabla,
                "run": self.run_id,
                "rc": code,
                "T": rounded(self.now()),
                "LB_ref": rounded(self.lb_ref),
                "LB_run": rounded(self.lb_run),
                "C": self.cpu,
                "R": self.ram,
                "fecha": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            },
        )


def fmt(value):
    return "n/a" if value is None or value <= 0 else f"{value:.2f}"


def ratio(total, bound):
    return "n/a" if bound is None or bound <= 0 else f"{total / bound:.2f}"


def cgroup_entry(path):
    """Runs in the child before exec: the step is in its own cgroup before any of its code runs."""

    def enter():
        try:
            with open(os.path.join(path, "cgroup.procs"), "w", encoding="utf-8") as fh:
                fh.write(str(os.getpid()))
        except OSError:
            pass

    return enter


def run_table(path):
    mode = os.environ.get("GATE_DAG", "0")
    if mode not in ("0", "1"):
        die(f"GATE_DAG is {mode!r}; it has to be 0 (series) or 1 (DAG)")
    steps = parse_table(path)
    return Planner(steps, mode == "1", sha256_of(path)).execute()


def main(argv=None):
    parser = argparse.ArgumentParser(description="Lane planner of the local judge (CI-F3, F3.2).")
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="run a step table: GATE_DAG=0 series, GATE_DAG=1 DAG")
    run.add_argument("--table", required=True)
    args = parser.parse_args(argv)
    try:
        return run_table(args.table)
    except Refusal as err:
        sys.stderr.write(f"FAIL: {err}\n")
        return 2


if __name__ == "__main__":
    sys.exit(main())
