#!/usr/bin/env python3
"""History of the local judge's runs (CI-F3, F3.1): the only writer of the JSONL history.

Root: `${CI_TIEMPOS_DIR:-$HOME/.cache/ci-tiempos}/memory-industry/`, created when missing. One
folder per UTC day holds `corridas.jsonl`, `pasos.jsonl`, `unidades.jsonl`, `indice.jsonl` and
`flaky.jsonl`. Writes go under flock on `.candado` in the root. Every record has `repo`.

It never changes a verdict. The library calls and `registrar` turn any failure into one
`history: ...` line on stderr per process and exit 0; a history that cannot be written, a lock
held past its timeout, or a line that is not UTF-8 or not JSON is a warning or a skipped line. Only
an illegible manifest is exit 2. What was not measured is written as null, never 0. The flake mark
is diagnosis only.

Library use (the planner and the contract tests): run_start, step_start, step_end, units,
run_end, lower_bounds, leer_jsonl. CLI (the serial writer): `registrar`, a JSON manifest on stdin.
"""

from __future__ import annotations

import fcntl
import functools
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timedelta, timezone
from typing import Any, Callable

REPO = "memory-industry"
FLAKE_DAYS = 7
LOCK_TIMEOUT_S = 2.0
TEST_TIMES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "test-times.sh")
COTAS = ("camino", "cpu", "ram", "paso")

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


def _aviso(texto: str) -> None:
    global _avisado
    if not _avisado:
        _avisado = True
        sys.stderr.write(f"history: {texto}; the verdict does not change\n")


def _sin_veredicto(fn: Callable[..., Any]) -> Callable[..., Any]:
    """The history never gives a verdict: any failure in it is one warning and no exit change."""

    @functools.wraps(fn)
    def dentro(*args: Any, **kwargs: Any) -> Any:
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


def _tomar(candado: Any) -> bool:
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
            with open(os.path.join(carpeta, nombre), "a", encoding="utf-8") as fh:
                fh.write(json.dumps(registro, ensure_ascii=False, sort_keys=True) + "\n")
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
) -> None:
    """A step's line. A time nobody gave is null: the registrar never invents one."""
    corrida.cpus.append(cpu_s)
    seg = None if ini is None or end is None else end - ini
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
            "mem_peak": mem_peak,
            "anon_max": anon_max,
            "oom_kill": oom_kill,
            "psi": psi,
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
) -> None:
    """The library's own step: a missing inicio or fin is the module clock's."""
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
    )


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


def _verde_reciente(clave: str, paso_id: str, sha: str, huella: str) -> bool:
    """A green run of this binary, of this step, for the same sha and huella within FLAKE_DAYS."""
    limite = datetime.now(timezone.utc).date() - timedelta(days=FLAKE_DAYS)
    try:
        carpetas = os.listdir(history_root())
    except OSError:
        return False
    for carpeta in carpetas:
        try:
            dia = datetime.strptime(carpeta, "%Y%m%d").date()
        except ValueError:
            continue
        if dia < limite:
            continue
        for reg in leer_jsonl(os.path.join(history_root(), carpeta, "unidades.jsonl")):
            if (
                reg.get("clave") == clave
                and reg.get("id") == paso_id
                and reg.get("sha") == sha
                and reg.get("huella") == huella
                and reg.get("rc") == 0
            ):
                return True
    return False


@_sin_veredicto
def units(corrida: Corrida, paso: Paso, log_path: str) -> None:
    salida = _test_times_units(log_path)
    if salida is None:
        return
    registros: list[dict[str, Any]] = []
    for linea in salida.splitlines():
        try:
            clave, cs, tests, fallados = linea.rsplit(" ", 3)
            registros.append(
                {
                    "repo": REPO,
                    "modo": corrida.modo,
                    "tabla": corrida.tabla,
                    "run": corrida.run,
                    "id": paso.id,
                    "idx": paso.idx,
                    "sha": corrida.sha,
                    "huella": corrida.huella,
                    "clave": clave,
                    "finished_in": round(int(cs) / 100, 2),
                    "tests": int(tests),
                    "fallados": int(fallados),
                    "rc": 0 if int(fallados) == 0 else 1,
                }
            )
        except ValueError:
            _aviso(f"units: the line {linea!r} of {log_path} is not <key> <cs> <tests> <failed>; skipped")
    for reg in registros:
        if (
            reg["rc"] != 0
            and corrida.sha is not None
            and corrida.huella is not None
            and _verde_reciente(reg["clave"], paso.id, corrida.sha, corrida.huella)
        ):
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


def _cerrar(
    corrida: Corrida,
    rc: int,
    total: float | None,
    LB_ref: float | None,
    LB_run: float | None,
    C: int | None,
    R: int | None,
) -> None:
    cpus = corrida.cpus
    cpu_total = sum(cpus) if cpus and all(c is not None for c in cpus) else None
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
        "LB_ref": _r(LB_ref),
        "LB_run": _r(LB_run),
        "C": C,
        "R": R,
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
) -> None:
    """The library's own run end: a missing T is the module clock's."""
    _cerrar(corrida, rc, _reloj(corrida) if T is None else T, LB_ref, LB_run, C, R)


def _camino(
    paso_id: str,
    seg: dict[str, float],
    deps: dict[str, list[str]],
    memo: dict[str, float],
    pila: tuple[str, ...] = (),
) -> float:
    if paso_id in pila:
        raise ValueError("ciclo en las dependencias: " + " -> ".join(pila + (paso_id,)))
    if paso_id not in memo:
        previos = [_camino(d, seg, deps, memo, pila + (paso_id,)) for d in deps.get(paso_id, []) if d in seg]
        memo[paso_id] = seg[paso_id] + max(previos, default=0.0)
    return memo[paso_id]


def lower_bounds(records: list[dict[str, Any]], table: dict[str, Any]) -> dict[str, Any]:
    """LB = max(camino critico, sum(cpu_s)/C, sum(anon_max*seg)/R, max seg) over one run's steps.

    A term that needs a value nobody measured, or a capacity of 0, is left out; the answer is never
    a 0 for it. A dependency cycle raises ValueError naming it.
    """
    todos_seg = all(r.get("seg") is not None for r in records)
    seg = {r["id"]: float(r["seg"]) for r in records if r.get("seg") is not None}
    cotas: dict[str, float | None] = {nombre: None for nombre in COTAS}
    if records and todos_seg:
        memo: dict[str, float] = {}
        cotas["camino"] = max(_camino(i, seg, table["deps"], memo) for i in seg)
        cotas["paso"] = max(seg.values())
    cpus = [r.get("cpu_s") for r in records]
    if records and table.get("C") and all(c is not None for c in cpus):
        cotas["cpu"] = sum(cpus) / table["C"]
    if records and todos_seg and table.get("R") and all(r.get("anon_max") is not None for r in records):
        cotas["ram"] = sum(r["anon_max"] * seg[r["id"]] for r in records) / table["R"]
    LB: float | None = None
    cota: str | None = None
    for nombre in COTAS:
        valor = cotas[nombre]
        if valor is not None and (LB is None or valor > LB):
            LB, cota = valor, nombre
    return {"LB": LB, "cota": cota}


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
        )
        pasos[p["id"]] = paso
    for u in manifiesto.get("unidades", []):
        units(corrida, pasos[u["id"]], u["log"])
    _cerrar(corrida, manifiesto["rc"], manifiesto.get("T"), None, None, None, None)


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
