#!/usr/bin/env python3
"""Lane planner of the local judge (CI-F3, F3.2).

`run --table <file>` runs the step table. GATE_DAG=0 or serie (the default) is the series: the steps in
idx order, one at a time. GATE_DAG=1 or maximo plans the DAG: a step starts when its dependencies exited 0,
and it is admitted by CPU and RAM, recomputed on every event (a child exit, and a 1 s tick as a
backup). GATE_DAG=auto is the same DAG with at most as many rows alive as the machine has lanes, and the jobs of the
machine shared among them (CI-K4, D-K4m, D-K4n). The verdict is the series one: the red with the lowest idx wins, the steps before it
finish, the ones after it are stopped with TERM and, after a grace, KILL (D-F3d, J14g). A kernel kill (OOM, or a
signal the planner did not send) gives no verdict: exit 2 (D-F3f), and so does a failure of the planner itself (J14e).
A run is recorded with the mode that ran (J14a): GATE_DAG=1 with no delegated scope, with no serial history of this
table or with a DATABASE_URL in the environment runs in series, and is recorded as series. No step outlives the
planner: TERM and HUP stop it, and its exit stops every step and every `paso-*` cgroup of the scope (J14b).
The planner measures the machine before it starts (cores, RAM free, disk, battery) and prints its plan in one line,
`plan: ...` (CI-K4, D-K4q); a measure that cannot be read is a series with one job and the line says what (D-K4o).

Not in the planner: the removal of old run directories (D-F3i) and the per-binary launcher (D-F3u).
`exclusive` and `boundary` run alone, after every lower idx: the conservative form of D-F3l, and with a
delegated scope no other step's cgroup may still have processes when one starts or ends (D-F34o). The
resource column (CI-F3.3c, D-F3g, CI-F3.4): `target:<T>` is where the step builds, under
CARGO_TARGET_DIR/<tree key>/<T>, and a target that another tree marked is refused before the step starts;
`cluster:<C>` is a cluster of its own; `reader` reads the binary the target's builder left and builds
nothing; `models` marks a row that loads the ONNX models. The table is judged before anything starts
(D-F34f): a cluster in one row, rows in one target ordered by their dependencies unless both read it, a
reader with its builder above it, and no resource of another kind. At most two rows with models are alive
at once, and with another step alive one is admitted only with 4096 MiB of MemAvailable plus its own
estimate (D-F34d, D-F34e). A row with models that ends green is judged against the resource plan the
binary wrote in its log (D-F34b, D-F34c).
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import math
import os
import re
import secrets
import select
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
import traceback
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
# CI-J14b, CI-J14g: the signals that stop the planner; a step that is stopped gets TERM and, when GATE_DAG_TERM_GRACE_S
# (15 s) is over, KILL. A cgroup that still has processes after its step ended is killed and tried again for
# GATE_DAG_RMDIR_WAIT_S (5 s). The two variables are for the contract tests, which shorten the waits.
STOP_SIGNALS = frozenset({signal.SIGTERM, signal.SIGHUP})
TERM_GRACE_S = 15.0
RMDIR_WAIT_S = 5.0
# CI-F3.4 (D-F34d, D-F34e): the rows with models alive at once, and the MemAvailable a row with models needs on top of its own estimate.
MAX_MODELS = 2
MODELS_FLOOR = 4096 * (1 << 20)
POOLS = {
    "embed": "CUBA_EMBED_INTRA_THREADS",
    "rerank": "CUBA_RERANK_INTRA_THREADS",
    "nli": "CUBA_NLI_INTRA_THREADS",
}
PLAN_THREADS_RE = re.compile(r"hilos: embed=(\d+) rerank=(\d+) nli=(\d+)")
PLAN_TIER_RE = re.compile(r"nivel=(\S+)")
PLAN_GPU_RE = re.compile(r"reranker=\S+ \(GPU\)")
MEM_AVAILABLE_RE = re.compile(r"^MemAvailable:\s+(\d+) kB", re.MULTILINE)
# CI-F3.3b-2 (spec F3.3b-1b-iii): `untimed` schedules as `normal`; its block is past the judge's boundary line.
CLASSES = frozenset({"normal", "exclusive", "boundary", "untimed"})
EXCLUSIVE = frozenset({"exclusive", "boundary"})
ALLOWED_ENV = frozenset(
    {
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_JOBS",
        "CARGO_INCREMENTAL",
        "CARGO_PROFILE_DEV_DEBUG",
        "CARGO_PROFILE_DEV_STRIP",
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
# CI-K4 (D-K4n): the values of GATE_DAG and what each one asks for.
ASKED = {"0": "serie", "serie": "serie", "auto": "auto", "1": "maximo", "maximo": "maximo"}
# CI-K4 (D-K4m): the lanes of the rule of the judge, as (cores, GiB of free RAM) -> lanes, from the largest down.
LANES_TABLE = ((16, 48, 3), (8, 24, 2))


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


def seconds_from_env(name: str, default: float) -> float:
    """A wait in seconds from the environment; a value that is not a number, or is negative, is the default."""
    try:
        value = float(os.environ.get(name, default))
    except ValueError:
        return default
    return value if math.isfinite(value) and value >= 0 else default


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


def table_key(path: str) -> str:
    """The key of the history (J14k): the sha256 of the rows of the table, each with its spaces collapsed, with no comment
    and no blank line. A comment does not drop the history; a number of a row does."""
    with open(path, encoding="utf-8") as fh:
        rows = [" ".join(line.split()) for line in fh if line.strip() and not line.lstrip().startswith("#")]
    return hashlib.sha256("\n".join(rows).encode("utf-8")).hexdigest()


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
        self.term_at: float | None = None
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
        self.solape_s: float | None = None


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


def tree_key(tree: str) -> str:
    """CI-J15a: the key of a tree in the judge's targets: the name of its directory and 12 hex of the sha256 of its realpath,
    with no line break in what is hashed. `target_dir` builds the path with it and `tree-key` says it, so the audit and
    the planner ask the one formula. `tree` is already a realpath."""
    return f"{os.path.basename(tree)}-{hashlib.sha256(tree.encode()).hexdigest()[:12]}"


def target_dir(name: str) -> str:
    """CI-F3.3c (D-F3g): the target of a resource is <CARGO_TARGET_DIR>/<tree key>/<name>. The tree key is the name of
    the tree's directory and 12 hex of the sha256 of its realpath, so two trees under one root never share a target.
    The directory names its tree in .gate-tree, and one that names another tree is refused before cargo runs."""
    root = os.environ.get("CARGO_TARGET_DIR", "")
    if not root:
        die(f"target {name}: CARGO_TARGET_DIR is not set, so there is no root to put the target under (D-F3g)")
    tree = os.path.realpath(repo_root())
    path = os.path.join(root, tree_key(tree), name)
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
    for column, value in (("cpu", cpu), ("ram_gb", ram_gb), ("dur_s", dur)):
        if not (math.isfinite(value) and value > 0):
            die(f"table line {number}: {column} of {tok[1]} has to be a finite number above 0, not {value!r}")
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


def ancestors(step: Step, by_id: dict[str, Step]) -> set[str]:
    """The ids that `step` waits for, directly or through the steps it waits for."""
    found: set[str] = set()
    pending = list(step.deps)
    while pending:
        dep = pending.pop()
        if dep not in found:
            found.add(dep)
            pending.extend(by_id[dep].deps)
    return found


def target_of(step: Step) -> str | None:
    return next((r[len("target:") :] for r in step.recursos if r.startswith("target:")), None)


def check_resource_kinds(steps: list[Step]) -> None:
    """D-F34f: a resource is target:<T>, cluster:<C>, models or reader, and one cluster is one row's."""
    clusters: dict[str, str] = {}
    for step in steps:
        for resource in step.recursos:
            kind, _, name = resource.partition(":")
            if resource in ("models", "reader"):
                continue
            if kind not in ("target", "cluster") or not name:
                die(f"step {step.id} names the resource {resource}, which is none of target:<T>, cluster:<C>, models or reader (D-F34f)")
            if kind == "cluster":
                if name in clusters:
                    die(f"steps {clusters[name]} and {step.id} both name cluster:{name}: one step per cluster (D-F34f)")
                clusters[name] = step.id


def check_target_order(steps: list[Step], by_id: dict[str, Step]) -> None:
    """D-F34f: rows in one target are ordered by their dependencies unless both are readers, and a reader has above it
    a row that is not a reader and builds the same target. With the dependencies, the overlap is impossible."""
    above = {s.id: ancestors(s, by_id) for s in steps}
    for n, first in enumerate(steps):
        target = target_of(first)
        if target is None:
            if "reader" in first.recursos:
                die(f"step {first.id} is a reader and names no target (D-F34f)")
            continue
        for second in steps[n + 1 :]:
            if target_of(second) != target or ("reader" in first.recursos and "reader" in second.recursos):
                continue
            if first.id not in above[second.id] and second.id not in above[first.id]:
                die(f"steps {first.id} and {second.id} share target {target} and neither depends on the other (D-F34f)")
        if "reader" in first.recursos and not any(
            target_of(by_id[a]) == target and "reader" not in by_id[a].recursos for a in above[first.id]
        ):
            die(f"step {first.id} reads target {target} and no step that builds {target} is above it (D-F34f)")


def profile_of(step: Step) -> dict[str, str]:
    """The tokens of the row that give its cargo calls their profile: it is in the hash of every unit (D-K4f)."""
    return {k: v for k, v in step.env.items() if k == "CARGO_INCREMENTAL" or k.startswith("CARGO_PROFILE_")}


def check_reader_profile(steps: list[Step], by_id: dict[str, Step]) -> None:
    """D-K4f: a reader has exactly the profile tokens of the row that builds its target. The profile is in the hash of every
    unit of a build, so a reader with another one would rebuild the target of its producer inside a row that builds nothing."""
    for step in steps:
        target = target_of(step)
        if "reader" not in step.recursos or target is None:
            continue
        for name in sorted(ancestors(step, by_id), key=lambda a: by_id[a].idx):
            producer = by_id[name]
            if target_of(producer) != target or "reader" in producer.recursos:
                continue
            built, read = profile_of(producer), profile_of(step)
            differ = sorted(k for k in built.keys() | read.keys() if built.get(k) != read.get(k))
            if differ:
                die(
                    f"step {step.id} reads target {target} that {producer.id} builds, and they do not name the same profile "
                    f"({', '.join(differ)}): a reader has exactly the CARGO_INCREMENTAL and CARGO_PROFILE_* tokens of its "
                    f"producer, or it would rebuild the target in a row that builds nothing (D-K4f)"
                )


def validate_table(steps: list[Step]) -> None:
    if not steps:
        die("the table has no steps")
    by_id = unique_steps(steps)
    for step in steps:
        for dep in step.deps:
            if dep not in by_id or by_id[dep].idx >= step.idx:
                die(f"step {step.id} depends on {dep}, which is not a step with a lower idx")
    ordered = sorted(steps, key=lambda s: s.idx)
    check_resource_kinds(ordered)
    check_target_order(ordered, by_id)
    check_reader_profile(ordered, by_id)


def parse_table(path: str) -> list[Step]:
    steps: list[Step] = []
    with open(path, encoding="utf-8") as fh:
        for number, line in enumerate(fh, 1):
            line = line.strip()
            if line and not line.startswith("#"):
                steps.append(parse_row(line, number))
    validate_table(steps)
    return sorted(steps, key=lambda s: s.idx)


# --- the memory of the machine and the plan of the binary --------------------------


def mem_available() -> int:
    """MemAvailable in bytes, from GATE_PROC_MEMINFO (the fixtures') or /proc/meminfo (D-F34d). Unreadable is a refusal."""
    path = os.environ.get("GATE_PROC_MEMINFO", "/proc/meminfo")
    match = MEM_AVAILABLE_RE.search(read_text(path) or "")
    if match is None:
        die(f"no MemAvailable in {path}: the planner cannot admit a row with models without it (D-F34d)")
    return int(match.group(1)) * 1024


def pool_reference(steps: list[Step]) -> dict[str, int]:
    """What the rows with models name for the three pools (D-F3m): the plan of production on this machine."""
    for step in steps:
        if "models" in step.recursos and all(name in step.env for name in POOLS.values()):
            return {pool: int(step.env[name]) for pool, name in POOLS.items() if step.env[name].isdigit()}
    return {}


def decides_the_plan(step: Step) -> bool:
    """A row with models that names no pool of its own is where the plan decides (D-F34c)."""
    return "models" in step.recursos and not all(name in step.env for name in POOLS.values())


def plan_lines(text: str) -> list[str]:
    """The lines the binary wrote with tracing's JSON, `"message":"resource plan"`, as the log of a step holds them."""
    return [line for line in text.splitlines() if line.lstrip().startswith("{") and '"message":"resource plan"' in line]


def plan_text(line: str) -> str | None:
    """The `plan` field of a resource plan line, nested in `fields` as tracing writes it or at the top; None if unreadable."""
    try:
        obj = json.loads(line)
    except ValueError:
        return None
    fields = obj.get("fields", obj) if isinstance(obj, dict) else None
    plan = fields.get("plan") if isinstance(fields, dict) else None
    return plan if isinstance(plan, str) else None


def plan_problem(line: str, reference: dict[str, int]) -> str | None:
    """D-F34b: why this plan line is not the judge's: below the full tier, or other pools than the rows set (the
    reranker's only when the plan keeps it off the GPU, where the pool of the CPU does not do the work)."""
    plan = plan_text(line)
    threads = PLAN_THREADS_RE.search(plan or "")
    tier = PLAN_TIER_RE.search(plan or "")
    if plan is None or threads is None or tier is None:
        return f"unreadable resource plan line: {line}"
    if tier.group(1) != "full":
        return f"resource plan tier {tier.group(1)}, the judge needs full"
    on_gpu = PLAN_GPU_RE.search(plan) is not None
    for pool, value in zip(("embed", "rerank", "nli"), threads.groups()):
        if pool in reference and int(value) != reference[pool] and not (pool == "rerank" and on_gpu):
            return f"resource plan {pool}={value}, the rows with models set {reference[pool]}"
    return None


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


class Measure:
    """What the planner knows of the machine (CI-K4): the cores, the memory and the jobs that gate-lock.sh counts, with the
    reason when it could not read them (D-K4o)."""

    def __init__(self) -> None:
        self.nproc: int | None = None
        self.cgroup_cpu: int | None = None
        self.cgroup_mem_kb: str = ""
        self.free_ram_kb: int | None = None
        self.jobs: int = 1
        self.why: str = ""


# One bash does the counting of gate-lock.sh, so the jobs of `gate_cargo_jobs` and the jobs of the plan are one number:
# its fields, in the order of MEASURE_FIELDS, NUL separated.
MEASURE_ORDER = (
    'source "$1"; gate_cargo_jobs >/dev/null; '
    'printf "%s\\0%s\\0%s\\0%s\\0%s\\0%s" "$GATE_NPROC" "$GATE_CG_CPUS" "$GATE_CG_MEM_KB" "$GATE_FREE_RAM_KB" '
    '"$GATE_CARGO_JOBS" "$GATE_RESOURCE_WHY"'
)


def measure_machine() -> Measure:
    """The machine as gate-lock.sh counts it. A bash that fails, or a number that is not one, is a measure that failed: one
    job and the reason, never an exit (D-K4o)."""
    measure = Measure()
    lock = os.path.join(repo_root(), "scripts", "gate-lock.sh")
    res = subprocess.run(["bash", "-c", MEASURE_ORDER, "gate-dag", lock], capture_output=True, text=True, check=False)
    fields = res.stdout.split("\0")
    if res.returncode != 0 or len(fields) != 6:
        measure.why = "the resource measure of gate-lock.sh failed: " + (res.stderr.strip() or f"exit {res.returncode}")
        return measure
    nproc, cpus, mem_kb, free_kb, jobs, why = fields
    measure.cgroup_cpu = int(cpus) if cpus.isdigit() else None
    measure.cgroup_mem_kb = mem_kb.strip()
    measure.nproc = int(nproc) if nproc.isdigit() else None
    measure.free_ram_kb = int(free_kb) if free_kb.isdigit() else None
    measure.jobs = int(jobs) if jobs.isdigit() and int(jobs) > 0 else 1
    measure.why = why.strip()
    return measure


def total_memory() -> int:
    return os.sysconf("SC_PHYS_PAGES") * os.sysconf("SC_PAGE_SIZE")


def capacities(measure: Measure | None = None) -> tuple[int, int]:
    """Returns (C cores, R bytes): R is the scope's memory less max(4 GiB, 10 %). A cgroup that could not be read leaves the
    machine's own cores and memory."""
    measure = measure or measure_machine()
    mem_kb = measure.cgroup_mem_kb
    margin = int(mem_kb) * 1024 if mem_kb.isdigit() else total_memory()
    ram = margin - max(MIN_RESERVE, margin // 10)
    cpu = measure.cgroup_cpu if measure.cgroup_cpu else (os.cpu_count() or 1)
    return cpu, ram


def disk_free_gb() -> int | None:
    """D-K4p: the GiB free where the targets live: CARGO_TARGET_DIR, or its first ancestor that exists (the repo's own
    filesystem without one). Only measured: unreadable is None, not a failure."""
    path = os.environ.get("CARGO_TARGET_DIR") or repo_root()
    while path and not os.path.exists(path):
        parent = os.path.dirname(path)
        if parent == path:
            break
        path = parent
    try:
        return shutil.disk_usage(path).free // GIB
    except OSError:
        return None


def battery_state() -> str:
    """D-K4p: `discharging` when a battery discharges, `ac` when there is one and it does not, `none` when there is none (no
    root, as in WSL), `unknown` when a supply cannot be read. Only measured: it changes no decision."""
    root = os.environ.get("GATE_POWER_SUPPLY_ROOT", "/sys/class/power_supply")
    try:
        units = sorted(os.listdir(root))
    except (FileNotFoundError, NotADirectoryError):
        return "none"
    except OSError:
        return "unknown"
    found, unreadable = False, False
    for unit in units:
        try:
            with open(os.path.join(root, unit, "type"), encoding="utf-8") as fh:
                kind = fh.read().strip()
            if kind != "Battery":
                continue
            found = True
            with open(os.path.join(root, unit, "status"), encoding="utf-8") as fh:
                if fh.read().strip() == "Discharging":
                    return "discharging"
        except FileNotFoundError:
            continue
        except OSError:
            unreadable = True
    if unreadable:
        return "unknown"
    return "ac" if found else "none"


def lanes_for(cores: int, free_ram_kb: int | None) -> int:
    """D-K4m: the lanes of the rule: 3 from 16 cores and 48 GiB, 2 from 8 cores and 24 GiB, 1 below. What does not reach a
    step stays in the one below it."""
    if free_ram_kb is None:
        return 1
    gib = free_ram_kb / (1 << 20)
    for min_cores, min_gib, lanes in LANES_TABLE:
        if cores >= min_cores and gib >= min_gib:
            return lanes
    return 1


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
    # D-K4n: the rows alive at once that `auto` allows; None is no cap. A class default, so that a planner made without
    # __init__ (the property tests of the admission) has none.
    lanes: int | None = None

    def __init__(self, steps: list[Step], asked: str, tabla: str, timed_end: str | None = None) -> None:
        self.steps = steps
        self.by_id = {s.id: s for s in steps}
        self.asked = asked
        self.dag = asked != "serie"
        self.tabla = tabla
        # CI-F3.3b-2: the line the judge of the test times reads up to; printed once, before the first untimed block.
        self.timed_end = timed_end
        self.timed_end_printed = False
        self.t0 = time.monotonic()
        self.run_dir = make_run_dir()
        self.run_id = os.path.basename(self.run_dir)
        self.hist_steps, self.hist_runs = load_history(tabla)
        self.scope = scope_dir()
        self.delegado = delegate(self.scope)
        # CI-J14d: a DATABASE_URL of the operator is one server for every row, and sqlx drops databases by name.
        self.shared_server = bool(os.environ.get("DATABASE_URL"))
        # CI-K4 (D-K4o): a measure that could not be read puts the planner in series, with one job, and the plan says what.
        self.machine = measure_machine()
        # CI-J14a: what runs in series is recorded as series, so the next run of this table has a serial run to learn from.
        self.solo = not (self.dag and self.delegado and self.hist_runs and not self.shared_server and not self.machine.why)
        self.modo = "serie" if self.solo else "dag"
        self.plan = self.make_plan()
        self.lanes = self.plan["carriles"] if asked == "auto" else None
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
        self.cpu, self.ram = capacities(self.machine)
        # CI-F3.3c: the cores admitted never add up past the machine's (C), not 1,25·C: SAFETY is for RAM only.
        self.cpu_cap = self.cpu
        self.pools = pool_reference(steps)
        self.red: Step | None = None
        self.no_verdict: str | None = None
        self.cursor = 0
        self.last_sample = 0.0
        self.lb_ref: float | None = None
        self.lb_run: float | None = None
        self.wake_r: int | None = None
        self.wake_w: int | None = None
        self.rank, self.nxt = self.estimated_ranks()
        self.print_plan()
        self.notes()

    def jobs_origin(self) -> str:
        """Whose the jobs of the plan are: the operator's CARGO_BUILD_JOBS (not the one this gate exported), a failed measure
        (one job), or the gate's own count."""
        named = os.environ.get("CARGO_BUILD_JOBS")
        if named and named != os.environ.get("GATE_CARGO_JOBS_FROM_GATE"):
            return "operator"
        return "failed" if self.machine.why else "gate"

    def make_plan(self) -> dict[str, Any]:
        """D-K4q: what the planner measured of the machine and decided of it, with None for what it could not measure."""
        machine = self.machine
        cores = min(c for c in (machine.nproc, machine.cgroup_cpu) if c) if (machine.nproc or machine.cgroup_cpu) else None
        free = machine.free_ram_kb
        return {
            "nproc": machine.nproc,
            "cgroup_cpu": machine.cgroup_cpu,
            "ram_free": None if free is None else round(free / (1 << 20), 1),
            "disk_free": disk_free_gb(),
            "bateria": battery_state(),
            "jobs": machine.jobs,
            "jobs_origen": self.jobs_origin(),
            "carriles": lanes_for(cores, free) if cores else 1,
            "pedido": self.asked,
            "corre": self.modo,
            "fallo": machine.why or None,
        }

    def runs_reason(self) -> str:
        """Why a DAG that was asked for runs in series, in the words of the notes; empty when it does not."""
        if not self.dag or not self.solo or self.machine.why:
            return ""
        if not self.delegado:
            return "the scope is not delegated"
        if not self.hist_runs:
            return "no serial history for this table"
        return "DATABASE_URL names a server of the operator"

    def print_plan(self) -> None:
        """D-K4q: the plan, in one line, before the first block."""
        plan = self.plan
        ram = "unknown" if plan["ram_free"] is None else f"{plan['ram_free']:.1f}"
        disk = "unknown" if plan["disk_free"] is None else str(plan["disk_free"])
        reason = self.runs_reason()
        line = (
            f"plan: nproc={plan['nproc'] if plan['nproc'] is not None else 'unknown'} "
            f"cgroup_cpu={plan['cgroup_cpu'] if plan['cgroup_cpu'] is not None else 'none'} "
            f"ram_free={ram} GiB disk_free={disk} GB battery={plan['bateria']} "
            f"jobs={plan['jobs']} ({plan['jobs_origen']}) lanes={plan['carriles']} "
            f"asked={self.asked} runs={self.modo}"
        )
        if reason:
            line += f" ({reason})"
        if plan["fallo"]:
            line += f" failed={plan['fallo']}"
        print(line)

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
        if self.dag and self.shared_server:
            print("note: DATABASE_URL names a server this judge did not make, so every row shares it and sqlx drops databases by name: the planner runs in series")
        if self.dag and self.machine.why:
            print(f"note: the machine could not be measured ({self.machine.why}): the planner runs in series with one job")

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
            if reason is not None:
                step.motivo = reason
            elif self.leftover(step):
                return
            else:
                self.start(step)

    def leftover(self, step: Step) -> bool:
        """D-F34o: an exclusive step starts and ends with no other step's cgroup holding processes. A step that left
        its cgroup populated (a daemon outlived it; the rmdir of the planner fails in silence) gives no verdict. Only
        in a delegated scope: without one the planner runs in series and the exclusivity holds by construction."""
        if step.clase not in EXCLUSIVE or not self.delegado or not self.scope:
            return False
        own = f"paso-{step.idx}-{step.id}"
        for path in sorted(glob.glob(os.path.join(glob.escape(self.scope), "paso-*"))):
            events = read_kv(os.path.join(path, "cgroup.events"))
            if os.path.basename(path) != own and events and events.get("populated") == 1:
                self.no_verdict = f"no verdict: {step.id} is exclusive and {os.path.basename(path)} still had processes"
                return True
        return False

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
        if self.lanes is not None and len(alive) >= self.lanes:
            return "lanes"
        if "models" in step.recursos:
            if sum("models" in o.recursos for o in alive) >= MAX_MODELS:
                return "models"
            if mem_available() < MODELS_FLOOR + step.r:
                return "ram"
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
        if step.env.get("CARGO_BUILD_JOBS", "").isdigit():
            env["CARGO_BUILD_JOBS"] = str(self.row_jobs(int(step.env["CARGO_BUILD_JOBS"])))
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

    def row_jobs(self, named: int) -> int:
        """D-K4n: what the row names is its ceiling, not its share. In series it takes min(its number, the jobs of the machine);
        in `auto`, with the rows alive at once, min(its number, max(1, the jobs / the lanes)); in `maximo`, its own number."""
        if self.asked == "maximo":
            return named
        jobs = self.plan["jobs"]
        if self.asked == "serie" or self.solo:
            return min(named, jobs)
        return min(named, max(1, jobs // self.plan["carriles"]))

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
        # CI-J14b: a stop signal between Popen and the step being marked as running would leave a step started that
        # drain does not see. Blocked from before Popen until it is running, it arrives when it is unblocked, and drain
        # knows the step. The step itself starts with them unblocked (step_entry).
        before = signal.pthread_sigmask(signal.SIG_BLOCK, STOP_SIGNALS)
        try:
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
            if os.environ.get("GATE_DAG_TEST_TERM_AFTER_POPEN") == "1":
                os.kill(os.getpid(), signal.SIGTERM)
            step.t_start = self.now()
            if self.corrida is not None:
                step.paso = gate_history.step_start(self.corrida, step.idx, step.id)
            step.state = "running"
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, before)
        self.sample_step(step)

    def anon_of(self, step: Step) -> int | None:
        """The memory a step holds that the scope cannot give back (J14c): anon plus the pages its tmpfs holds (shmem),
        which `anon` does not see and the postgres of each lane writes under the cluster root."""
        if not step.cg:
            return None
        stat = read_kv(os.path.join(step.cg, "memory.stat"))
        return stat["anon"] + stat.get("shmem", 0) if stat and "anon" in stat else None

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
        if rc == 0 and not step.parado:
            step.rc = rc = self.plan_rc(step)
        self.record_step(step)
        self.remove_cgroup(step)
        if step.parado:
            return
        if rc < 0 or (step.oom_kill or 0) > 0:
            self.no_verdict = self.no_verdict_text(step, rc)
            return
        if self.leftover(step):
            return
        if rc != 0:
            self.mark_red(step)

    def plan_rc(self, step: Step) -> int:
        """D-F34b, D-F34c: a green row with models is judged against the plan lines of its log. The problem is the last
        line of its block, `FAIL: <id>: …`, and the row is red (rc 1), so D-F3d decides which red is reported."""
        if "models" not in step.recursos:
            return 0
        text = read_text(step.log) or ""
        lines = plan_lines(text)
        problems = [plan_problem(line, self.pools) for line in lines]
        problem = next((p for p in problems if p), None)
        if problem is None and not lines and decides_the_plan(step):
            problem = "no resource plan line (the row has models and names no pools, so its log has to show the plan the binary chose)"
        if problem is None:
            return 0
        newline = "" if not text or text.endswith("\n") else "\n"
        with open(step.log, "a", encoding="utf-8") as log:
            log.write(f"{newline}FAIL: {step.id}: {problem}\n")
        return 1

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

    def remove_cgroup(self, step: Step) -> None:
        """A finished step's own cgroup goes away. One that cannot be removed because processes are still in it (a
        daemon the step left) is killed and tried again for GATE_DAG_RMDIR_WAIT_S, and its processes are named as orphans
        in a note (J14b); one that cannot be removed for any other reason is left, unjudged."""
        path = step.cg
        if not path or self.try_rmdir(path):
            return
        if not populated(path):
            return
        killed = kill_cgroup(path)
        limit = seconds_from_env("GATE_DAG_RMDIR_WAIT_S", RMDIR_WAIT_S) if killed else 0.0
        deadline = time.monotonic() + limit
        while populated(path) and time.monotonic() < deadline:
            time.sleep(0.05)
            if self.try_rmdir(path):
                break
        if not step.parado:
            outcome = "they were killed" if not os.path.isdir(path) or not populated(path) else "they are still alive"
            print(f"note: {os.path.basename(path)} ended with orphan processes in its cgroup; {outcome}")

    @staticmethod
    def try_rmdir(path: str) -> bool:
        try:
            os.rmdir(path)
        except OSError:
            return False
        return True

    def overlap_of(self, step: Step) -> float:
        """D-K4r: the seconds of this row in which another one ran: the union, over the rows that started. A row still
        running is counted up to the end of this one, which is as far as its interval can touch it."""
        started = [s for s in self.steps if s.t_start is not None]
        intervals = [(s.t_start, s.t_end if s.t_end is not None else step.t_end) for s in started]
        return gate_history.solape_s(intervals, started.index(step))

    def record_step(self, step: Step) -> None:
        step.solape_s = self.overlap_of(step)
        if self.corrida is None or step.paso is None:
            return
        timings = gate_history.keep_timings(self.corrida, step.paso, step.log) if step.log else None
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
            timings=timings,
            solape_s=step.solape_s,
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
        """Stops a step: TERM to its group, so the EXIT trap of its shell runs (the clusters of its lane, the stage of the
        mutants), and KILL when the grace is over (J14g). The loop reaps it; `enforce_grace` does the KILL."""
        step.parado = True
        if step.term_at is None:
            step.term_at = time.monotonic()
            self.signal_group(step, signal.SIGTERM)

    def signal_group(self, step: Step, signum: int) -> None:
        try:
            os.killpg(step.proc.pid, signum)
        except ProcessLookupError:
            pass

    def hard_kill(self, step: Step) -> None:
        """KILL for the whole cgroup of the step, if it has one, and for its group."""
        if step.cg:
            kill_cgroup(step.cg)
        self.signal_group(step, signal.SIGKILL)

    def enforce_grace(self) -> None:
        """A step that was sent TERM and is still alive when the grace ends is killed."""
        limit = seconds_from_env("GATE_DAG_TERM_GRACE_S", TERM_GRACE_S)
        for step in self.running():
            if step.term_at is not None and step.proc.poll() is None and time.monotonic() - step.term_at >= limit:
                self.hard_kill(step)

    def drain(self) -> None:
        """Stops and reaps whatever is still running, however the loop ended: TERM, the grace, then KILL. After it, with a
        delegated scope, every `paso-*` cgroup of the scope is killed, the ones this planner does not know too (J14b)."""
        alive = self.running()
        for step in alive:
            self.kill(step)
        limit = seconds_from_env("GATE_DAG_TERM_GRACE_S", TERM_GRACE_S)
        for step in alive:
            remaining = max(0.0, step.term_at + limit - time.monotonic())
            try:
                rc = step.proc.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                self.hard_kill(step)
                rc = step.proc.wait()
            self.finish(step, rc)
        self.kill_scope_cgroups()

    def kill_scope_cgroups(self) -> None:
        if not (self.delegado and self.scope):
            return
        for path in sorted(glob.glob(os.path.join(glob.escape(self.scope), "paso-*"))):
            kill_cgroup(path)

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
            if self.no_verdict:
                return
            if not self.running():
                if self.unfinished():
                    die("planner stuck: no step is running and none can start")
                break
            self.wait_event()
            self.enforce_grace()
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
        finished = False
        try:
            self.loop()
            finished = True
        finally:
            try:
                self.drain()
            finally:
                self.teardown_signal()
                if not finished:
                    self.leave_logs()
        return self.close()

    def leave_logs(self) -> None:
        """J14e: the planner stopped before its verdict (an exception, a signal, a kernel kill): the blocks of the steps
        that finished are printed in idx order, and the run directory, where all the logs are, is named."""
        try:
            self.flush()
            print(f"dag: run directory: {self.run_dir}")
            sys.stdout.flush()
        except (OSError, ValueError):
            pass

    def close(self) -> int:
        if self.no_verdict:
            self.leave_logs()
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
            _, chain = gate_history.camino_critico(
                {s.id: float(dur[s.id]) for s in self.steps}, {s.id: s.deps for s in self.steps}
            )
            return "critical path: " + " -> ".join(chain)
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
        for step in self.ran_steps():
            print(self.step_line(step))

    @staticmethod
    def step_line(step: Step) -> str:
        """D-K4r: the numbers of one row, as the history keeps them."""
        peak = "n/a" if step.mem_peak is None else f"{step.mem_peak / GIB:.2f}"
        cpu = "n/a" if step.cpu_s is None else f"{step.cpu_s:.2f}"
        return (
            f"dag: step {step.id} seg={step.t_end - step.t_start:.2f} wait={step.t_start - step.t_ready:.2f} "
            f"overlap={step.solape_s or 0.0:.2f} mem_peak={peak} cpu_s={cpu}"
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
            plan=self.plan,
        )


def kill_cgroup(path: str) -> bool:
    """Writes 1 to cgroup.kill of the cgroup at `path`; False where it has none or the write is refused."""
    target = os.path.join(path, "cgroup.kill")
    if not os.path.exists(target):
        return False
    try:
        with open(target, "w", encoding="utf-8") as fh:
            fh.write("1")
    except OSError:
        return False
    return True


def populated(path: str) -> bool:
    events = read_kv(os.path.join(path, "cgroup.events"))
    return bool(events and events.get("populated") == 1)


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
    planner's own disposition is not touched: stop_on_signal is its TERM and its HUP."""
    enter_cgroup = cgroup_entry(path) if path else None

    def enter() -> None:
        # The planner blocks TERM and HUP while it starts a step (J14b), and a child keeps the mask of its parent.
        signal.pthread_sigmask(signal.SIG_UNBLOCK, STOP_SIGNALS)
        signal.signal(signal.SIGINT, signal.SIG_DFL)
        signal.signal(signal.SIGQUIT, signal.SIG_DFL)
        if enter_cgroup is not None:
            enter_cgroup()

    return enter


def stop_on_signal(_signum: int, _frame: object) -> NoReturn:
    """CI-F3.3b-2: the gate's exit trap sends TERM here, and a closed terminal sends HUP (J14b); the exit unwinds through
    execute, which stops the steps."""
    raise SystemExit(2)


def run_table(path: str, timed_end: str | None = None) -> int:
    mode = os.environ.get("GATE_DAG", "0")
    if mode not in ASKED:
        die(f"GATE_DAG is {mode!r}; it has to be 0 or serie (series), auto (DAG with lanes), or 1 or maximo (DAG)")
    for signum in STOP_SIGNALS:
        signal.signal(signum, stop_on_signal)
    steps = parse_table(path)
    return Planner(steps, ASKED[mode], table_key(path), timed_end).execute()


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Lane planner of the local judge (CI-F3, F3.2).")
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="run a step table: GATE_DAG=0 or serie, auto, or 1 or maximo")
    run.add_argument("--table", required=True)
    run.add_argument("--timed-end", default=None, help="the line printed before the first untimed block (CI-F3.3b-2)")
    commands.add_parser("allowed-env", help="print the closed list of a step's environment, one name per line (CI-J13)")
    key = commands.add_parser("tree-key", help="print the key of a tree in the judge's targets, <name>-<12 hex> of its realpath (CI-J15a)")
    key.add_argument("tree")
    args = parser.parse_args(argv)
    if args.command == "allowed-env":
        sys.stdout.write("".join(f"{name}\n" for name in sorted(ALLOWED_ENV)))
        return 0
    if args.command == "tree-key":
        if not os.path.isdir(args.tree):
            sys.stderr.write(f"FAIL: tree-key: {args.tree} is not a directory, so it has no key\n")
            return 2
        sys.stdout.write(f"{tree_key(os.path.realpath(args.tree))}\n")
        return 0
    try:
        return run_table(args.table, args.timed_end)
    except Refusal as err:
        sys.stderr.write(f"FAIL: {err}\n")
        return 2
    except Exception:
        # CI-J14e: a failure of the planner itself is no verdict. An uncaught one would exit 1, which merge-gate reads as a
        # test that went red.
        traceback.print_exc()
        sys.stderr.write("FAIL: the planner failed inside (the traceback is above): no verdict, exit 2\n")
        return 2


if __name__ == "__main__":
    sys.exit(main())
