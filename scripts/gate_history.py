#!/usr/bin/env python3
"""History of the local judge's runs (CI-F3, F3.1): the only writer of the JSONL history.

Root: `${CI_TIEMPOS_DIR:-$HOME/.cache/ci-tiempos}/memory-industry/`, created when missing. One
folder per UTC day holds `corridas.jsonl`, `pasos.jsonl`, `unidades.jsonl`, `indice.jsonl`, `flaky.jsonl`
and `usl.jsonl` (the points of the USL sweep of gate_usl.py, CI-F5a). The reports of `cargo --timings` that a
row saved go to `<day>/timings/<run>/<idx>-<id>/` (CI-K4, D-K4h). Writes go under flock on `.candado` in
the root. Every record has `repo`.

It never changes a verdict. The library calls and `registrar` turn any failure into one
`history: ...` line on stderr per process and exit 0; a history that cannot be written, a lock
held past its timeout, or a line that is not UTF-8 or not JSON is a warning or a skipped line. Only
an illegible manifest is exit 2. What was not measured is written as null, never 0. The flake mark
is diagnosis only.

Library use (the planner and the contract tests): run_start, step_start, step_end, units,
run_end, lower_bounds, camino_critico, leer_jsonl, usl_point, keep_timings, solape_s, serie_final, coste_de,
coste_base, comparar, lineas_de_coste. The planner writes
`unidades.jsonl` and `flaky.jsonl` itself (CI-E28): `record_step` calls `units` on the log of every row that ended, was not stopped and
shows a `Running` or `Doc-tests` header of cargo; a row that ran no test binary (fmt, clippy, the audits) asks nothing of `test-times.sh`. `anon_max` of a step is its anonymous memory plus the pages its
tmpfs holds (`shmem`): what is charged to the scope and cannot be reclaimed without swap. CLI (the serial writer): `registrar`, a JSON manifest on stdin.

CI-E4: a step and a run also carry `nucleos` (CPU seconds over the seconds of the step, over the T of the run) and
`sin_medida`, `{field: reason}` for every null of a field the planner measures (cpu_s, nucleos, mem_peak and anon_max of a step; those and
mem_muestra, serie_final_s and serie_cola_s of a run). The reason is the one the caller gave; this module derives only the one of
`nucleos` (the one of `cpu_s`, or a duration of 0 s) and never invents another. `serie_final` is the one function of the
seconds a run spent with one row alive and another ready, or with one row alive at the end.

CI-E2: a green run also carries `coste` (null when it has none), the cost per test and the share of its 10 slowest binaries,
from the units of the rows that are timed; `lineas_de_coste` gives the `cost:` lines the planner prints and the record,
against the median of the last 10 green runs of the same mode and table. Its alarms never change a verdict.
"""

from __future__ import annotations

import fcntl
import functools
import hashlib
import itertools
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from collections.abc import Callable
    from typing import ParamSpec, TextIO, TypeVar

    P = ParamSpec("P")
    R = TypeVar("R")

REPO = "memory-industry"
FLAKE_DAYS = 7
LOCK_TIMEOUT_S = 2.0
TEST_TIMES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "test-times.sh")
COTAS = ("camino", "cpu", "ram", "paso")
# CI-K4 (D-K4j): the reports of `--timings` are kept for 30 UTC days; the day folders and their jsonl files never go.
TIMINGS_DIAS = 30
# CI-K4 (D-K4h): what cargo writes on stderr when it saves a report, the only place that says which report a call made.
INFORME_RE = re.compile(r"^\s*Timing report saved to (.+?)\s*$")
# CI-E2 (D-E2e): the numbers of docs/ci/metodologia-de-escala.md:162-165, 170. The base is the last 10 green runs of the same mode
# and table; a cost per test over 1.10 x its median alarms, and so does a binary that enters the 10 slowest without having been
# in them in any of those runs. The `self-test:` lines of CI-E3 are seconds that `local_gate_contract` already holds.
COSTE_BASE = 10
COSTE_TOP = 10
COSTE_UMBRAL = 1.10
AUTOTEST = "self-test:"

_avisado = False
_candado_roto = False


class Corrida:
    def __init__(
        self,
        modo: str,
        sha: str | None,
        huella: str | None,
        tabla: str | None,
        run: str,
        arbol_sucio: bool | None,
    ) -> None:
        self.modo = modo
        self.sha = sha
        self.huella = huella
        self.tabla = tabla
        self.run = run
        self.arbol_sucio = arbol_sucio
        self.t0 = time.monotonic()
        self.cpus: list[float | None] = []


class Paso:
    def __init__(self, idx: int, paso_id: str, inicio: float) -> None:
        self.idx = idx
        self.id = paso_id
        self.inicio = inicio


def history_root() -> str:
    base = os.environ.get("CI_TIEMPOS_DIR") or os.path.expanduser("~/.cache/ci-tiempos")
    return os.path.join(base, REPO)


def huella(raiz: str) -> str | None:
    """The tree's huella (D-F3s): sha256 of `git diff HEAD --binary`, then the name and the bytes of
    every untracked file, in byte order of the name. None when git cannot say: a null is never a
    fingerprint."""
    raiz_b = os.fsencode(raiz)
    try:
        diff = subprocess.run(
            ["git", "-C", raiz, "diff", "HEAD", "--binary"], capture_output=True, check=True
        ).stdout
        sin_rastrear = subprocess.run(
            ["git", "-C", raiz, "ls-files", "--others", "--exclude-standard", "-z"],
            capture_output=True,
            check=True,
        ).stdout
        digest = hashlib.sha256(diff)
        for nombre in sorted(n for n in sin_rastrear.split(b"\0") if n):
            digest.update(b"\0untracked\0" + nombre + b"\0")
            with open(os.path.join(raiz_b, nombre), "rb") as fh:
                digest.update(fh.read())
    except (OSError, subprocess.CalledProcessError):
        return None
    return digest.hexdigest()


def _aviso(texto: str) -> None:
    global _avisado
    if not _avisado:
        _avisado = True
        sys.stderr.write(f"history: {texto}; the verdict does not change\n")


def _sin_veredicto(fn: Callable[P, R]) -> Callable[P, R | None]:
    """The history never gives a verdict: any failure in it is one warning and no exit change."""

    @functools.wraps(fn)
    def dentro(*args: P.args, **kwargs: P.kwargs) -> R | None:
        try:
            return fn(*args, **kwargs)
        except Exception as err:
            _aviso(f"{fn.__name__} failed ({type(err).__name__}: {err})")
            return None

    return dentro


def _r(valor: float | None, digitos: int = 3) -> float | None:
    return None if valor is None else round(valor, digitos)


def _reloj(corrida: Corrida) -> float:
    return time.monotonic() - corrida.t0


def _todos_medidos(valores: list[Any]) -> list[Any] | None:
    """The values when there is at least one and none is null. A sum over steps that one of them
    did not measure is not a smaller number: it is no number."""
    return valores if valores and all(v is not None for v in valores) else None


def _columna(registros: list[dict[str, Any]], campo: str) -> list[Any] | None:
    return _todos_medidos([r.get(campo) for r in registros])


def _con_nucleos(
    cpu_s: float | None, duracion: float | None, que: str, sin_medida: dict[str, str] | None
) -> tuple[float | None, dict[str, str]]:
    """D-E4b: the cores a CPU total and a duration give, and the `sin_medida` that goes with them. A null takes the reason
    the caller gave for `cpu_s`, or says that `que` took 0 s; with no reason given and none of ours, it has none."""
    motivos = dict(sin_medida or {})
    if cpu_s is None:
        if "cpu_s" in motivos:
            motivos.setdefault("nucleos", motivos["cpu_s"])
        return None, motivos
    if duracion is None:
        return None, motivos
    if duracion <= 0:
        motivos.setdefault("nucleos", f"{que} took 0 s")
        return None, motivos
    return _r(cpu_s / duracion), motivos


def leer_jsonl(path: str) -> list[dict[str, Any]]:
    """Every JSON object of a JSONL file. A line that is not UTF-8 or not JSON is skipped: an old
    history never stops a run."""
    registros: list[dict[str, Any]] = []
    try:
        with open(path, "rb") as fh:
            for linea in fh:
                try:
                    registro = json.loads(linea.decode("utf-8"))
                except ValueError:
                    continue
                if isinstance(registro, dict):
                    registros.append(registro)
    except OSError:
        pass
    return registros


def _plazo_candado() -> float:
    try:
        return max(0.0, float(os.environ.get("GATE_HISTORY_LOCK_TIMEOUT_S", LOCK_TIMEOUT_S)))
    except ValueError:
        return LOCK_TIMEOUT_S


def _tomar(candado: TextIO) -> bool:
    """Takes the lock without waiting past GATE_HISTORY_LOCK_TIMEOUT_S (2 s by default)."""
    limite = time.monotonic() + _plazo_candado()
    while True:
        try:
            fcntl.flock(candado, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return True
        except BlockingIOError:
            if time.monotonic() >= limite:
                return False
            time.sleep(0.02)


def _salto_pendiente(path: str) -> str:
    """A newline when the file ends in a line cut halfway (a process killed in the middle of a write), so the next record
    does not glue itself to it and get lost with it. Nothing for a file that is absent, empty or ends in a newline."""
    try:
        with open(path, "rb") as fh:
            fh.seek(-1, os.SEEK_END)
            return "" if fh.read(1) == b"\n" else "\n"
    except OSError:
        return ""


def _escribir(nombre: str, registro: dict[str, Any]) -> None:
    """Once the lock timed out in this process, the later writes skip it without waiting: the
    timeout is spent once, and the one warning already said what happened."""
    global _candado_roto
    if _candado_roto:
        return
    raiz = history_root()
    carpeta = os.path.join(raiz, datetime.now(timezone.utc).strftime("%Y%m%d"))
    candado_path = os.path.join(raiz, ".candado")
    try:
        os.makedirs(carpeta, exist_ok=True)
        with open(candado_path, "a") as candado:
            if not _tomar(candado):
                _candado_roto = True
                _aviso(f"{nombre} not written: {candado_path} is held by another process for more than {_plazo_candado()} s; the later writes of this process skip it")
                return
            destino = os.path.join(carpeta, nombre)
            with open(destino, "a", encoding="utf-8") as fh:
                fh.write(_salto_pendiente(destino) + json.dumps(registro, ensure_ascii=False, sort_keys=True) + "\n")
    except OSError as err:
        _aviso(f"{nombre} not written under {carpeta} ({err})")


@_sin_veredicto
def run_start(
    modo: str,
    sha: str | None,
    huella: str | None,
    tabla: str | None,
    *,
    run: str | None = None,
    arbol_sucio: bool | None = None,
) -> Corrida | None:
    if run is None:
        run = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S") + f"-{os.getpid()}"
    return Corrida(modo, sha, huella, tabla, run, arbol_sucio)


@_sin_veredicto
def step_start(corrida: Corrida, idx: int, paso_id: str) -> Paso | None:
    return Paso(idx, paso_id, _reloj(corrida))


def _paso(
    corrida: Corrida,
    paso: Paso,
    rc: int,
    ini: float | None,
    end: float | None,
    espera_s: float | None,
    motivo: str | None,
    parado: bool,
    cpu_s: float | None,
    mem_peak: int | None,
    anon_max: int | None,
    oom_kill: int | None,
    psi: dict[str, float | None] | None,
    timings: dict[str, Any] | None = None,
    solape_s: float | None = None,
    sin_medida: dict[str, str] | None = None,
    jobs: int | None = None,
) -> None:
    """A step's line. A time nobody gave is null: the registrar never invents one."""
    corrida.cpus.append(cpu_s)
    seg = None if ini is None or end is None else end - ini
    nucleos, motivos = _con_nucleos(cpu_s, seg, "the step", sin_medida)
    _escribir(
        "pasos.jsonl",
        {
            "repo": REPO,
            "modo": corrida.modo,
            "tabla": corrida.tabla,
            "run": corrida.run,
            "id": paso.id,
            "idx": paso.idx,
            "inicio": _r(ini),
            "fin": _r(end),
            "seg": _r(seg),
            "rc": rc,
            "parado": parado,
            "espera_s": _r(espera_s),
            "motivo": motivo,
            "cpu_s": _r(cpu_s),
            "nucleos": nucleos,
            "mem_peak": mem_peak,
            "anon_max": anon_max,
            "oom_kill": oom_kill,
            "psi": psi,
            "timings": timings,
            "solape_s": _r(solape_s),
            "jobs": jobs,
            "sin_medida": motivos,
        },
    )


@_sin_veredicto
def step_end(
    corrida: Corrida,
    paso: Paso,
    rc: int,
    *,
    inicio: float | None = None,
    fin: float | None = None,
    espera_s: float | None = None,
    motivo: str | None = None,
    parado: bool = False,
    cpu_s: float | None = None,
    mem_peak: int | None = None,
    anon_max: int | None = None,
    oom_kill: int | None = None,
    psi: dict[str, float | None] | None = None,
    timings: dict[str, Any] | None = None,
    solape_s: float | None = None,
    sin_medida: dict[str, str] | None = None,
    jobs: int | None = None,
) -> None:
    """The library's own step: a missing inicio or fin is the module clock's. `sin_medida` is the reason of each null the caller
    knows (D-E4h). `jobs` is the CARGO_BUILD_JOBS the step was given, None for a row that names none (D-F3s)."""
    _paso(
        corrida,
        paso,
        rc,
        paso.inicio if inicio is None else inicio,
        _reloj(corrida) if fin is None else fin,
        espera_s,
        motivo,
        parado,
        cpu_s,
        mem_peak,
        anon_max,
        oom_kill,
        psi,
        timings,
        solape_s,
        sin_medida,
        jobs,
    )


def _informes_nombrados(log_path: str) -> list[str]:
    """The paths `Timing report saved to <path>` names in the log of a row, in order and with repeats."""
    nombrados: list[str] = []
    with open(log_path, encoding="utf-8", errors="replace") as fh:
        for linea in fh:
            coincide = INFORME_RE.match(linea)
            if coincide:
                nombrados.append(coincide.group(1))
    return nombrados


def _podar_informes(raiz: str) -> None:
    """D-K4j: the timings/ of the day folders older than TIMINGS_DIAS UTC days go; the folder and its jsonl stay. A name
    that is not a day is not touched, and a folder that cannot be read is not a failure of the run."""
    hoy = datetime.now(timezone.utc).date()
    try:
        carpetas = os.listdir(raiz)
    except OSError:
        return
    for carpeta in carpetas:
        try:
            dia = datetime.strptime(carpeta, "%Y%m%d").date()
        except ValueError:
            continue
        if (hoy - dia).days > TIMINGS_DIAS:
            shutil.rmtree(os.path.join(raiz, carpeta, "timings"), ignore_errors=True)


@_sin_veredicto
def keep_timings(corrida: Corrida, paso: Paso, log_path: str) -> dict[str, Any] | None:
    """D-K4h: the reports the log of a row names go from where cargo left them to
    `<root>/<day>/timings/<run>/<idx>-<id>/`, moved and not copied, and the answer is what was kept:
    `{dir, n, bytes, perdidos, faltan}`, with `dir` relative to the root of the history, `perdidos` the times a path is
    named again and `faltan` the paths that are not there. The log is the only source: a report it does not name (the
    one of another call, `cargo-timing.html`) stays where it is. None when the log names none; and None, with the one
    `history:` line, when the history cannot take them (they stay in the target). Keeping also prunes the retention."""
    nombrados = _informes_nombrados(log_path)
    if not nombrados:
        return None
    unicos = list(dict.fromkeys(nombrados))
    existentes = [ruta for ruta in unicos if os.path.isfile(ruta)]
    raiz = history_root()
    relativo = "/".join((datetime.now(timezone.utc).strftime("%Y%m%d"), "timings", corrida.run, f"{paso.idx}-{paso.id}"))
    peso = 0
    if existentes:
        destino = os.path.join(raiz, *relativo.split("/"))
        os.makedirs(destino, exist_ok=True)
        for ruta in existentes:
            peso += os.path.getsize(ruta)
            shutil.move(ruta, os.path.join(destino, os.path.basename(ruta)))
    _podar_informes(raiz)
    return {
        "dir": relativo if existentes else None,
        "n": len(existentes),
        "bytes": peso,
        "perdidos": len(nombrados) - len(unicos),
        "faltan": len(unicos) - len(existentes),
    }


def solape_s(intervalos: list[tuple[float, float]], i: int) -> float:
    """D-K4r: the seconds of the interval `i` in which at least one other interval of `intervalos` is alive: the union of
    the others over `i`, and not the sum of their intersections. Two intervals that touch do not overlap."""
    inicio, fin = intervalos[i]
    otros = sorted(
        (max(a, inicio), min(b, fin))
        for j, (a, b) in enumerate(intervalos)
        if j != i and a < fin and b > inicio
    )
    total = 0.0
    hasta = inicio
    for a, b in otros:
        if b > hasta:
            total += b - max(a, hasta)
            hasta = b
    return total


def _vivos(intervalos: list[tuple[float, float, float]], t: float) -> int:
    return sum(1 for _, inicio, fin in intervalos if inicio <= t < fin)


def _listo_sin_empezar(intervalos: list[tuple[float, float, float]], t: float) -> bool:
    return any(listo <= t < inicio for listo, inicio, _ in intervalos)


def serie_final(intervalos: list[tuple[float, float, float]]) -> tuple[float, float] | None:
    """D-E4k, D-E4l: `(serie_final_s, serie_cola_s)` of a run from the `(listo, inicio, fin)` of each row that started. In a
    stretch where exactly one row is alive, the seconds count as the wait if another row is ready (its dependencies are
    green, it has not started) and as the tail if no row starts after them; the first is `serie_final_s` with the second.
    One row alive while the others are blocked by their dependencies is the critical path, and does not count; nor does a
    stretch with nobody alive. A row ends where the next stretch begins, so two that touch are not alive together. None
    when no row started: a 0 is a run with everything in parallel."""
    if not intervalos:
        return None
    ultimo_inicio = max(inicio for _, inicio, _ in intervalos)
    puntos = sorted(set(itertools.chain.from_iterable(intervalos)))
    espera = cola = 0.0
    for desde, hasta in zip(puntos, puntos[1:]):
        if _vivos(intervalos, desde) != 1:
            continue
        if _listo_sin_empezar(intervalos, desde):
            espera += hasta - desde
        elif ultimo_inicio <= desde:
            cola += hasta - desde
    return round(espera + cola, 3), round(cola, 3)


def _test_times_units(log_path: str) -> str | None:
    try:
        res = subprocess.run(
            ["bash", TEST_TIMES, "--units", log_path],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as err:
        _aviso(f"units not read from {log_path} ({err})")
        return None
    if res.returncode != 0:
        _aviso(f"units not read from {log_path} (test-times exit {res.returncode})")
        return None
    return res.stdout


def _dias_recientes() -> list[str]:
    """The day folders of the history within FLAKE_DAYS; a name that is not a day is not one."""
    limite = datetime.now(timezone.utc).date() - timedelta(days=FLAKE_DAYS)
    raiz = history_root()
    try:
        carpetas = os.listdir(raiz)
    except OSError:
        return []
    recientes: list[str] = []
    for carpeta in carpetas:
        try:
            dia = datetime.strptime(carpeta, "%Y%m%d").date()
        except ValueError:
            continue
        if dia >= limite:
            recientes.append(os.path.join(raiz, carpeta))
    return recientes


def _verde_reciente(clave: str, paso_id: str, sha: str, huella: str) -> bool:
    """A green run of this binary, of this step, for the same sha and huella within FLAKE_DAYS."""
    buscado = {"clave": clave, "id": paso_id, "sha": sha, "huella": huella, "rc": 0}
    return any(
        all(reg.get(campo) == valor for campo, valor in buscado.items())
        for carpeta in _dias_recientes()
        for reg in leer_jsonl(os.path.join(carpeta, "unidades.jsonl"))
    )


def _es_flaky(corrida: Corrida, paso: Paso, reg: dict[str, Any]) -> bool:
    """A red binary that was green for the same sha, huella and step: diagnosis only."""
    return (
        reg["rc"] != 0
        and corrida.sha is not None
        and corrida.huella is not None
        and _verde_reciente(reg["clave"], paso.id, corrida.sha, corrida.huella)
    )


def _registro_unidad(
    corrida: Corrida,
    paso: Paso,
    clave: str,
    finished_in: float | None,
    tests: int | None,
    fallados: int | None,
    rc: int,
) -> dict[str, Any]:
    return {
        "repo": REPO,
        "modo": corrida.modo,
        "tabla": corrida.tabla,
        "run": corrida.run,
        "id": paso.id,
        "idx": paso.idx,
        "sha": corrida.sha,
        "huella": corrida.huella,
        "clave": clave,
        "finished_in": finished_in,
        "tests": tests,
        "fallados": fallados,
        "rc": rc,
    }


def _unidades(corrida: Corrida, paso: Paso, salida: str, log_path: str) -> list[dict[str, Any]]:
    registros: list[dict[str, Any]] = []
    for linea in salida.splitlines():
        try:
            clave, cs, tests, fallados = linea.rsplit(" ", 3)
            if cs == "-":
                # A binary a signal killed (D-F3s): no time and no count, and the fourth field is -signal.
                registros.append(_registro_unidad(corrida, paso, clave, None, None, None, int(fallados)))
                continue
            registros.append(
                _registro_unidad(
                    corrida,
                    paso,
                    clave,
                    round(int(cs) / 100, 2),
                    int(tests),
                    int(fallados),
                    0 if int(fallados) == 0 else 1,
                )
            )
        except ValueError:
            _aviso(f"units: the line {linea!r} of {log_path} is not <key> <cs> <tests> <failed>; skipped")
    return registros


@_sin_veredicto
def units(corrida: Corrida, paso: Paso, log_path: str) -> list[dict[str, Any]] | None:
    """The records it wrote, for the planner to cost (CI-E2); None when the log could not be read: a row with no units is not a
    row with no tests."""
    salida = _test_times_units(log_path)
    if salida is None:
        return None
    registros = _unidades(corrida, paso, salida, log_path)
    for reg in registros:
        if _es_flaky(corrida, paso, reg):
            _escribir(
                "flaky.jsonl",
                {
                    "repo": REPO,
                    "run": corrida.run,
                    "id": paso.id,
                    "clave": reg["clave"],
                    "sha": corrida.sha,
                    "huella": corrida.huella,
                    "rc": reg["rc"],
                },
            )
    for reg in registros:
        _escribir("unidades.jsonl", reg)
    return registros


def _por_test(centesimas: int, tests: int) -> float | None:
    return centesimas / 100 / tests if tests else None


def _cuota(parte: float, total: float) -> float:
    return parte / total if total else 0.0


def coste_de(unidades: list[dict[str, Any]], cronometradas: set[str]) -> dict[str, Any]:
    """CI-E2 (D-E2c): the cost of one run from its unit records. Only the units of a row in `cronometradas` count, and never
    the `self-test:` ones. `seg` and `por_test` are the seconds and the seconds per test of the run and of each row; `top10`
    and `cuota_top10`, the slowest binaries (a binary in two rows adds up, as in test-times.sh) and their share of `seg`;
    `claves`, the seconds of every binary. Centiseconds are added as integers, so the order of the units changes nothing."""
    por_clave: dict[str, int] = {}
    por_fila: dict[str, list[int]] = {}
    for unidad in unidades:
        if unidad["id"] not in cronometradas or unidad["clave"].startswith(AUTOTEST):
            continue
        centesimas = round(unidad["finished_in"] * 100)
        por_clave[unidad["clave"]] = por_clave.get(unidad["clave"], 0) + centesimas
        fila = por_fila.setdefault(unidad["id"], [unidad.get("idx", 0), 0, 0])
        fila[1] += centesimas
        fila[2] += unidad["tests"]
    total = sum(por_clave.values())
    tests = sum(fila[2] for fila in por_fila.values())
    top = sorted(por_clave.items(), key=lambda par: (-par[1], par[0]))[:COSTE_TOP]
    return {
        "tests": tests,
        "seg": total / 100,
        "por_test": _por_test(total, tests),
        "cuota_top10": _cuota(sum(cs for _, cs in top), total),
        "filas": {
            fila_id: {"tests": fila[2], "seg": fila[1] / 100, "por_test": _por_test(fila[1], fila[2])}
            for fila_id, fila in sorted(por_fila.items(), key=lambda par: (par[1][0], par[0]))
        },
        "top10": [{"clave": clave, "seg": cs / 100} for clave, cs in top],
        "claves": {clave: cs / 100 for clave, cs in por_clave.items()},
    }


def _dias_del_historial() -> list[str]:
    """Every day folder of the history, oldest first; a name that is not a day is not one."""
    try:
        carpetas = os.listdir(history_root())
    except OSError:
        return []
    dias = []
    for carpeta in carpetas:
        try:
            datetime.strptime(carpeta, "%Y%m%d")
        except ValueError:
            continue
        dias.append(carpeta)
    return sorted(dias)


def coste_base(modo: str, tabla: str | None, run: str, cronometradas: set[str]) -> list[dict[str, Any]]:
    """CI-E2 (D-E2d): the `coste_de` of each of the last COSTE_BASE runs with rc 0 of this mode and table in `corridas.jsonl`
    (without `run`), oldest first. Their units are read from the day before the oldest of those runs onward, because a run
    that crosses midnight UTC writes in two folders. A run with no units of a timed row (one from before CI-E28) is no base."""
    raiz = history_root()
    dias = _dias_del_historial()
    verdes = [
        (dia, reg["run"])
        for dia in dias
        for reg in leer_jsonl(os.path.join(raiz, dia, "corridas.jsonl"))
        if reg.get("modo") == modo and reg.get("tabla") == tabla and reg.get("rc") == 0 and reg.get("run") != run
    ][-COSTE_BASE:]
    if not verdes:
        return []
    desde = (datetime.strptime(verdes[0][0], "%Y%m%d") - timedelta(days=1)).strftime("%Y%m%d")
    corridas = {corrida_run for _, corrida_run in verdes}
    unidades: dict[str, list[dict[str, Any]]] = {}
    for dia in (d for d in dias if d >= desde):
        for unidad in leer_jsonl(os.path.join(raiz, dia, "unidades.jsonl")):
            if unidad.get("run") in corridas:
                unidades.setdefault(unidad["run"], []).append(unidad)
    costes = [coste_de(unidades.get(corrida_run, []), cronometradas) for _, corrida_run in verdes]
    return [coste for coste in costes if coste["filas"]]


def _mediana(valores: list[float | None]) -> float | None:
    """The median of the values of the base, or None while there are not COSTE_BASE of them: fewer is no median (D-E2d)."""
    medidos = [v for v in valores if v is not None]
    return statistics.median(medidos) if len(medidos) >= COSTE_BASE else None


def _razon(valor: float | None, mediana: float | None) -> float | None:
    return None if valor is None or not mediana else valor / mediana


def _txt(valor: float | None, formato: str) -> str:
    return "n/a" if valor is None else format(valor, formato)


def _alarma_por_test(que: str, valor: float | None, mediana: float | None) -> str | None:
    """D-E2e, A1 and A2: strictly over COSTE_UMBRAL x the median, on the numbers as they are and not as they are printed."""
    if valor is None or not mediana or valor <= COSTE_UMBRAL * mediana:
        return None
    return (
        f"cost: ALARM {que} per_test {valor:.4f} is {valor / mediana:.2f} x its median {mediana:.4f} "
        f"(alarm over {COSTE_UMBRAL:.2f} x); the verdict does not change"
    )


def _lineas_de_filas(actual: dict[str, Any], base: list[dict[str, Any]]) -> tuple[list[str], list[str]]:
    lineas: list[str] = []
    alarmas: list[str] = []
    for fila_id, fila in actual["filas"].items():
        mediana = _mediana([c["filas"][fila_id]["por_test"] for c in base if fila_id in c["filas"]])
        lineas.append(
            f"cost: row {fila_id} tests={fila['tests']} seg={fila['seg']:.2f} per_test={_txt(fila['por_test'], '.4f')} "
            f"median={_txt(mediana, '.4f')} ratio={_txt(_razon(fila['por_test'], mediana), '.2f')}"
        )
        alarma = _alarma_por_test(f"row {fila_id}", fila["por_test"], mediana)
        if alarma:
            alarmas.append(alarma)
    return lineas, alarmas


def _lineas_del_top(actual: dict[str, Any], base: list[dict[str, Any]], modo: str) -> tuple[list[str], list[str]]:
    """D-E2e, A3: with a full base, a binary of the top that is in the top of none of its runs alarms and its line ends in ` new`."""
    estuvieron = {t["clave"] for c in base for t in c["top10"]}
    lineas: list[str] = []
    alarmas: list[str] = []
    for rango, binario in enumerate(actual["top10"], 1):
        clave, seg = binario["clave"], binario["seg"]
        mediana = _mediana([c["claves"][clave] for c in base if clave in c["claves"]])
        nuevo = len(base) >= COSTE_BASE and clave not in estuvieron
        lineas.append(
            f"cost: top {rango} {clave} seg={seg:.2f} share={_cuota(seg, actual['seg']):.3f} "
            f"median={_txt(mediana, '.2f')} ratio={_txt(_razon(seg, mediana), '.2f')}" + (" new" if nuevo else "")
        )
        if nuevo:
            alarmas.append(
                f"cost: ALARM top {rango} {clave} entered the {COSTE_TOP} slowest: it was in none of the {COSTE_TOP} slowest "
                f"of the last {COSTE_BASE} green {modo} runs of this table; the verdict does not change"
            )
    return lineas, alarmas


def comparar(actual: dict[str, Any], base: list[dict[str, Any]], modo: str) -> tuple[list[str], list[str]]:
    """CI-E2 (D-E2d to D-E2f): the `cost:` lines of a run (a `coste_de`) against its base (the `coste_base`), and apart the
    ones that are alarms, which are also in the lines. The run first, then its rows, then its top, then the alarms."""
    k = len(base)
    por_test = _mediana([c["por_test"] for c in base])
    cuota = _mediana([c["cuota_top10"] for c in base])
    lineas = [
        f"cost: run mode={modo} tests={actual['tests']} seg={actual['seg']:.2f} per_test={_txt(actual['por_test'], '.4f')} "
        f"median={_txt(por_test, '.4f')} ratio={_txt(_razon(actual['por_test'], por_test), '.2f')} "
        f"top10_share={actual['cuota_top10']:.3f} median_share={_txt(cuota, '.3f')} baseline={k}/{COSTE_BASE}"
    ]
    if k < COSTE_BASE:
        lineas.append(f"cost: no median yet: {k} of {COSTE_BASE} green {modo} runs of this table")
    alarma = _alarma_por_test("run", actual["por_test"], por_test)
    alarmas = [alarma] if alarma else []
    for lineas_de, alarmas_de in (_lineas_de_filas(actual, base), _lineas_del_top(actual, base, modo)):
        lineas += lineas_de
        alarmas += alarmas_de
    return lineas + alarmas, alarmas


def _registro_de_coste(actual: dict[str, Any], base: int, alarmas: list[str]) -> dict[str, Any]:
    """D-E2g: what `corridas.jsonl` keeps of the cost. The per-binary seconds of the run are not kept, only its top."""
    return {
        "tests": actual["tests"],
        "seg": _r(actual["seg"], 2),
        "por_test": _r(actual["por_test"], 4),
        "cuota_top10": _r(actual["cuota_top10"], 3),
        "filas": {
            fila_id: {"tests": fila["tests"], "seg": _r(fila["seg"], 2), "por_test": _r(fila["por_test"], 4)}
            for fila_id, fila in actual["filas"].items()
        },
        "top10": [{"clave": t["clave"], "seg": _r(t["seg"], 2)} for t in actual["top10"]],
        "base": base,
        "alarmas": alarmas,
    }


@_sin_veredicto
def lineas_de_coste(
    corrida: Corrida,
    unidades_por_fila: dict[str, list[dict[str, Any]] | None],
    cronometradas: set[str],
    code: int,
) -> tuple[list[str], dict[str, Any] | None, str | None]:
    """CI-E2 (D-E2a, D-E2h): `(lineas, coste, motivo)` for the planner, the one caller. `unidades_por_fila` is what `units` gave
    for each row that ran test binaries (None: it could not be read). A red run and a row whose units are unread have no cost,
    and `motivo` says why for `sin_medida`. A run that ran no test binary has none either, with no `motivo`: it is not a field
    the planner measures. A failure inside is the one `history:` line and None: the verdict is the rows'."""
    if code != 0:
        motivo = f"the run is red (exit {code}); a red run stops rows before their end"
        return [f"cost: not computed: {motivo}"], None, motivo
    for fila_id, registros in unidades_por_fila.items():
        if fila_id in cronometradas and registros is None:
            motivo = f"the units of row {fila_id} could not be read"
            return [f"cost: not computed: {motivo}"], None, motivo
    actual = coste_de(
        [u for fila_id, registros in unidades_por_fila.items() if fila_id in cronometradas for u in registros or []],
        cronometradas,
    )
    if not actual["filas"]:
        return ["cost: not computed: no timed row ran a test binary"], None, None
    base = coste_base(corrida.modo, corrida.tabla, corrida.run, cronometradas)
    lineas, alarmas = comparar(actual, base, corrida.modo)
    return lineas, _registro_de_coste(actual, len(base), alarmas), None


def _brecha(total: float | None, cota: float | None) -> float | None:
    """T over a lower bound, the gap D-F3t prints; null while either is unmeasured or the bound is 0."""
    if total is None or cota is None or cota <= 0:
        return None
    return _r(total / cota, 3)


def _cerrar(
    corrida: Corrida,
    rc: int,
    total: float | None,
    LB_ref: float | None,
    LB_run: float | None,
    C: int | None,
    R: int | None,
    *,
    mem_peak: int | None = None,
    psi: dict[str, float | None] | None = None,
    load: float | None = None,
    hilos: int | None = None,
    oom_kill: int | None = None,
    plan: dict[str, Any] | None = None,
    mem_muestra: dict[str, Any] | None = None,
    serie_final_s: float | None = None,
    serie_cola_s: float | None = None,
    sin_medida: dict[str, str] | None = None,
    coste: dict[str, Any] | None = None,
) -> None:
    cpus = _todos_medidos(corrida.cpus)
    cpu_total = None if cpus is None else sum(cpus)
    nucleos, motivos = _con_nucleos(cpu_total, total, "the run", sin_medida)
    registro = {
        "repo": REPO,
        "modo": corrida.modo,
        "tabla": corrida.tabla,
        "run": corrida.run,
        "sha": corrida.sha,
        "huella": corrida.huella,
        "arbol_sucio": corrida.arbol_sucio,
        "rc": rc,
        "T": _r(total),
        "seg": _r(total),
        "cpu_s": _r(cpu_total),
        "nucleos": nucleos,
        "LB_ref": _r(LB_ref),
        "LB_run": _r(LB_run),
        "brecha_ref": _brecha(total, LB_ref),
        "brecha_run": _brecha(total, LB_run),
        "C": C,
        "R": R,
        "mem_peak": mem_peak,
        "psi": psi,
        "load": _r(load, 2),
        "hilos": hilos,
        "oom_kill": oom_kill,
        "plan": plan,
        "mem_muestra": mem_muestra,
        "serie_final_s": _r(serie_final_s),
        "serie_cola_s": _r(serie_cola_s),
        "sin_medida": motivos,
        "coste": coste,
        "fecha": datetime.now(timezone.utc).isoformat(timespec="seconds"),
    }
    _escribir("corridas.jsonl", registro)
    _escribir("indice.jsonl", registro)


@_sin_veredicto
def run_end(
    corrida: Corrida,
    rc: int,
    *,
    T: float | None = None,
    LB_ref: float | None = None,
    LB_run: float | None = None,
    C: int | None = None,
    R: int | None = None,
    mem_peak: int | None = None,
    psi: dict[str, float | None] | None = None,
    load: float | None = None,
    hilos: int | None = None,
    oom_kill: int | None = None,
    plan: dict[str, Any] | None = None,
    mem_muestra: dict[str, Any] | None = None,
    serie_final_s: float | None = None,
    serie_cola_s: float | None = None,
    sin_medida: dict[str, str] | None = None,
    coste: dict[str, Any] | None = None,
) -> None:
    """The library's own run end: a missing T is the module clock's. What nobody measured is None. `plan` is what the
    planner measured of the machine and decided (D-K4q); `mem_muestra`, the two `serie_*` and `sin_medida` are the planner's
    (CI-E4), as given. `coste` is the planner's `lineas_de_coste` record (CI-E2), null when the run has none."""
    _cerrar(
        corrida,
        rc,
        _reloj(corrida) if T is None else T,
        LB_ref,
        LB_run,
        C,
        R,
        mem_peak=mem_peak,
        psi=psi,
        load=load,
        hilos=hilos,
        oom_kill=oom_kill,
        plan=plan,
        mem_muestra=mem_muestra,
        serie_final_s=serie_final_s,
        serie_cola_s=serie_cola_s,
        sin_medida=sin_medida,
        coste=coste,
    )


@_sin_veredicto
def usl_point(registro: dict[str, Any]) -> None:
    """One point of a USL sweep (CI-F5a, D-F5l) into `usl.jsonl`, with `repo`, under the same lock. Like every
    writer here it never raises: a point that cannot be written is the one `history:` warning."""
    _escribir("usl.jsonl", {"repo": REPO, **registro})


def camino_critico(seg: dict[str, float], deps: dict[str, list[str]]) -> tuple[float, list[str]]:
    """The longest chain of dependencies, by the durations in `seg`: its length (the term `camino` of the bound) and the
    chain itself, from its first step to its last. The one function of the critical path: the planner prints the chain
    and the history gives the number from it. A dependency with no duration is skipped. A tie goes to the first step of
    `seg` for the end of the chain and to the first of `deps[id]` for each step before it. A cycle raises ValueError
    naming it."""
    memo: dict[str, tuple[float, str | None]] = {}

    def hasta(paso_id: str, pila: tuple[str, ...] = ()) -> tuple[float, str | None]:
        if paso_id in pila:
            raise ValueError("ciclo en las dependencias: " + " -> ".join(pila + (paso_id,)))
        if paso_id not in memo:
            previo: str | None = None
            mejor = 0.0
            for d in deps.get(paso_id, []):
                if d in seg:
                    largo = hasta(d, pila + (paso_id,))[0]
                    if previo is None or largo > mejor:
                        previo, mejor = d, largo
            memo[paso_id] = (seg[paso_id] + mejor, previo)
        return memo[paso_id]

    if not seg:
        return 0.0, []
    fin = max(seg, key=lambda i: hasta(i)[0])
    cadena: list[str] = []
    paso: str | None = fin
    while paso is not None:
        cadena.append(paso)
        paso = memo[paso][1]
    cadena.reverse()
    return memo[fin][0], cadena


def _cota_cpu(records: list[dict[str, Any]], table: dict[str, Any]) -> float | None:
    cpus = _columna(records, "cpu_s")
    if cpus is None or not table.get("C"):
        return None
    return sum(cpus) / table["C"]


def _cota_ram(records: list[dict[str, Any]], seg: dict[str, float], table: dict[str, Any]) -> float | None:
    if _columna(records, "seg") is None or _columna(records, "anon_max") is None or not table.get("R"):
        return None
    return sum(r["anon_max"] * seg[r["id"]] for r in records) / table["R"]


def _la_mayor(cotas: dict[str, float | None]) -> dict[str, Any]:
    """The largest measured term; on a tie, the first one in COTAS."""
    LB: float | None = None
    cota: str | None = None
    for nombre in COTAS:
        valor = cotas[nombre]
        if valor is not None and (LB is None or valor > LB):
            LB, cota = valor, nombre
    return {"LB": LB, "cota": cota}


def lower_bounds(records: list[dict[str, Any]], table: dict[str, Any]) -> dict[str, Any]:
    """LB = max(camino critico, sum(cpu_s)/C, sum(anon_max*seg)/R, max seg) over one run's steps.

    A term that needs a value nobody measured, or a capacity of 0, is left out; the answer is never
    a 0 for it. A dependency cycle raises ValueError naming it.
    """
    seg = {r["id"]: float(r["seg"]) for r in records if r.get("seg") is not None}
    cotas: dict[str, float | None] = dict.fromkeys(COTAS)
    if _columna(records, "seg") is not None:
        cotas["camino"] = camino_critico(seg, table["deps"])[0]
        cotas["paso"] = max(seg.values())
    cotas["cpu"] = _cota_cpu(records, table)
    cotas["ram"] = _cota_ram(records, seg, table)
    return _la_mayor(cotas)


@_sin_veredicto
def registrar(manifiesto: dict[str, Any]) -> None:
    corrida = run_start(
        manifiesto["modo"],
        manifiesto.get("sha"),
        manifiesto.get("huella"),
        manifiesto.get("tabla"),
        run=manifiesto.get("run"),
        arbol_sucio=manifiesto.get("arbol_sucio"),
    )
    if corrida is None:
        return
    pasos: dict[str, Paso] = {}
    for p in manifiesto["pasos"]:
        paso = Paso(p["idx"], p["id"], 0.0)
        _paso(
            corrida,
            paso,
            p["rc"],
            p.get("inicio"),
            p.get("fin"),
            p.get("espera_s"),
            p.get("motivo"),
            p.get("parado", False),
            p.get("cpu_s"),
            p.get("mem_peak"),
            p.get("anon_max"),
            p.get("oom_kill"),
            p.get("psi"),
            p.get("timings"),
            p.get("solape_s"),
        )
        pasos[p["id"]] = paso
    for u in manifiesto.get("unidades", []):
        paso = pasos.get(u.get("id"))
        if paso is None:
            # A unit with no step of the manifest: named, skipped, and the run is written as usual (F3.3b-0).
            sys.stderr.write(
                f"history: unit {u.get('id')!r} names a step the manifest does not have; "
                f"its log {u.get('log')!r} is not read; the verdict does not change\n"
            )
            continue
        units(corrida, paso, u["log"])
    _cerrar(
        corrida,
        manifiesto["rc"],
        manifiesto.get("T"),
        manifiesto.get("LB_ref"),
        manifiesto.get("LB_run"),
        manifiesto.get("C"),
        manifiesto.get("R"),
        mem_peak=manifiesto.get("mem_peak"),
        psi=manifiesto.get("psi"),
        load=manifiesto.get("load"),
        hilos=manifiesto.get("hilos"),
        oom_kill=manifiesto.get("oom_kill"),
        plan=manifiesto.get("plan"),
    )


def main(argv: list[str]) -> int:
    if argv != ["registrar"]:
        sys.stderr.write("usage: gate_history.py registrar < manifest.json\n")
        return 2
    try:
        manifiesto = json.load(sys.stdin)
    except ValueError as err:
        sys.stderr.write(f"FAIL: the manifest is not JSON: {err}\n")
        return 2
    registrar(manifiesto)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
