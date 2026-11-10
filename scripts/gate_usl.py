#!/usr/bin/env python3
"""USL calibration of the local judge's jobs (CI-F5a, ci-rapido-F5.spec.md): `barrido`, `ajustar`, `recomendar`.

`python3 -I scripts/gate_usl.py <order>`. It edits nothing: `recomendar` prints a unified diff of
scripts/gate-dag.txt and the orchestrator reviews and commits it, because the table's hash is the key
of the history (D-F5r).

- `barrido --perfil {debug,release}`: compiles what a row of the judge compiles, at several
  CARGO_BUILD_JOBS (J), crate cold and without incremental, with the profile tokens of the row in
  scripts/gate-dag.txt (CI-K4), and writes one point per J and replica to `usl.jsonl` through
  gate_history.usl_point. The J are fractions of J_m, the jobs of the machine as the planner counts them (E7a-2).
  Exit 2 when the machine is busy, the table cannot be read, or the grid has fewer than five levels.
- `ajustar --perfil P`: fits X(J) = lambda*J / (1 + sigma*(J-1) + kappa*J*(J-1)) to the points by
  least squares on the relative error, lambda profiled, over a fixed grid (no randomness). Exit 3
  when the fit is not reliable (five criteria, the first one that fails is the reason).
- `recomendar`: slack of each row, the `juez-cuda` verdict, and the J that a simulation of the DAG
  (the admission, the order and the jobs of gate-dag.py, up to J_m) likes best. Exit 3 when an adjustment or a cold run
  is missing.

Standard library only. The cargo orders of each profile have to appear in scripts/run-all-tests.sh
(guarded by gate_usl_contract).
"""

from __future__ import annotations

import argparse
import difflib
import functools
import glob
import importlib.util
import itertools
import json
import math
import os
import re
import resource
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from typing import Any, NoReturn

# No bytecode next to the scripts (J7): the judge does not write __pycache__ into the tree.
sys.dont_write_bytecode = True
# -I leaves the script's own folder off sys.path: gate_history.py and gate-dag.py sit beside this file.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gate_history

GIB = 1 << 30
PSI_MAX = 15.0
PSI_ARCHIVO = "/proc/pressure/cpu"
PLAZO_ORDEN_S = 1800.0
MUESTREO_S = 0.25
RODILLA = 0.95
SIGMA_MAX = 0.99
U_MIN = -6.0
U_MAX = -1.0
KAPPA_MAX = 10**U_MAX
RECENTRADOS = 20
RMSE_MAX = 0.05
DISPERSION_MAX = 0.10
NIVELES_MIN = 5
PUNTOS_POR_NIVEL = 2
ESTABILIDAD = 2
ACERCAMIENTOS = 3
FRIO_B3_S = 60.0
CORRIDAS_MIN = 3
VENTANA = 5
SEGURIDAD_RAM = 1.25
CRITICA = 0.05
CASI_CRITICA = 0.20
GANANCIA_MIN_S = 10.0
GANANCIA_MIN_REL = 0.02
EMPATE_S = 5.0
EMPATE_REL = 0.01
BORDE = 1 - 1e-9

# Mapa fijo perfil -> fila (spec 3). CI-E11 (D-E11i): the debug profile is B1, the row that compiles T_dbg; DBG, CT, D2, D3 and NLI
# read what B1 built and compile nothing. The orders are the compilations of that row in run-all-tests.sh, in
# its order (D-F5f); `cargo test --no-run` is the compile-only form of the judge's `cargo test`. There is no
# profile of the E2E row: since CI-F3.4 it is a reader that runs the release binary B3 built and compiles
# nothing, and the release binary is compiled by B3, which `release` measures. `fracciones` are the (numerator,
# denominator) of J_m that the sweep measures (D-E7j): with 24 jobs they are the grids that the first sweeps used.
PERFILES: dict[str, dict[str, Any]] = {
    "debug": {
        "fila": "B1",
        "ordenes": [
            ["clippy", "--all-targets", "--", "-D", "warnings"],
            ["build", "--bin", "memory-industry"],
            ["test", "--no-run"],
        ],
        "fracciones": ((1, 12), (1, 6), (1, 3), (1, 2), (2, 3), (1, 1)),
    },
    "release": {
        "fila": "B3",
        "ordenes": [
            ["test", "--release", "--features", "docs,cuda", "--no-run", "--lib", "--test", "v017_rerank_gpu"],
            ["build", "--release", "--features", "docs,cuda", "--bins"],
        ],
        "fracciones": ((1, 6), (1, 4), (1, 3), (1, 2), (2, 3)),
    },
}
FILAS_EN_FRIO = frozenset(p["fila"] for p in PERFILES.values())


class Fallo(Exception):
    """An exit with a message: 2 for an input or environment problem, 3 for an adjustment that is not there."""

    def __init__(self, mensaje: str, codigo: int = 2) -> None:
        super().__init__(mensaje)
        self.codigo = codigo


def fallar(mensaje: str) -> NoReturn:
    raise Fallo(mensaje)


@functools.cache
def gate_dag() -> Any:
    """gate-dag.py has a hyphen: it is loaded by path, for its table parser, target_dir and capacities."""
    ruta = os.path.join(os.path.dirname(os.path.abspath(__file__)), "gate-dag.py")
    spec = importlib.util.spec_from_file_location("gate_dag", ruta)
    if spec is None or spec.loader is None:
        fallar(f"{ruta} cannot be loaded")
    modulo = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(modulo)
    return modulo


def raiz() -> str:
    return os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


# --- the model ----------------------------------------------------------------


def g(j: float, sigma: float, kappa: float) -> float:
    return j / (1 + sigma * (j - 1) + kappa * j * (j - 1))


def tiempo(j: float, lam: float, sigma: float, kappa: float) -> float:
    """T(J) = 1 / X(J), seconds."""
    return 1 / (lam * g(j, sigma, kappa))


def _error(js: list[float], xs: list[float], sigma: float, kappa: float) -> tuple[float, float]:
    """(S, lambda): lambda in closed form, S = sum of the squared relative errors (spec 2.2)."""
    suma = suma2 = 0.0
    for j, x in zip(js, xs):
        a = g(j, sigma, kappa) / x
        suma += a
        suma2 += a * a
    return max(0.0, len(js) - suma * suma / suma2), suma / suma2


def _reticula(centro: float, paso: float, minimo: float, maximo: float) -> list[float]:
    """21 values around a centre, `paso` apart, kept inside [minimo, maximo] (the edge is a value of the list)."""
    valores: list[float] = []
    for t in range(-10, 11):
        v = min(maximo, max(minimo, centro + t * paso))
        if not valores or v != valores[-1]:
            valores.append(v)
    return valores


def _barrer(js: list[float], xs: list[float], sigmas: list[float], us: list[float]) -> tuple[float, int, int | None]:
    """(S, index of sigma, index of log10 kappa or None for kappa = 0) at the lowest S; a tie goes to the first cell
    in the order sigma ascending, kappa = 0 first and then ascending."""
    mejor: tuple[float, int, int | None] = (math.inf, 0, None)
    for i, sigma in enumerate(sigmas):
        s, _ = _error(js, xs, sigma, 0.0)
        if s < mejor[0]:
            mejor = (s, i, None)
        for k, u in enumerate(us):
            s, _ = _error(js, xs, sigma, 10**u)
            if s < mejor[0]:
                mejor = (s, i, k)
    return mejor


def _en_borde(valores: list[float], i: int, bajo: float, alto: float) -> bool:
    """The best cell is the end of a list that is not the end of the fit domain: the optimum may lie beyond."""
    return (i == 0 and valores[0] > bajo) or (i == len(valores) - 1 and valores[-1] < alto)


def _buscar(js: list[float], xs: list[float]) -> tuple[float, float, float, float]:
    """(sigma, kappa, lambda, S) at the minimum of a fixed grid and three zooms of 21 x 21 cells around the best one, each
    a tenth of the step of the one before. A best cell on the edge of its zoom is searched again, centred on it, at the
    same step (at most RECENTRADOS times): the valley of sigma and kappa runs diagonally and a box of one cell can miss it."""
    sigmas = [SIGMA_MAX * i / 60 for i in range(61)]
    us = [U_MIN + (U_MAX - U_MIN) * i / 59 for i in range(60)]
    paso_s, paso_u = SIGMA_MAX / 60, (U_MAX - U_MIN) / 59
    for zoom in range(ACERCAMIENTOS + 1):
        mejor = _barrer(js, xs, sigmas, us)
        for _ in range(RECENTRADOS if zoom else 0):
            fuera_s = _en_borde(sigmas, mejor[1], 0.0, SIGMA_MAX)
            fuera_u = mejor[2] is not None and _en_borde(us, mejor[2], U_MIN, U_MAX)
            if not (fuera_s or fuera_u):
                break
            sigmas = _reticula(sigmas[mejor[1]], paso_s, 0.0, SIGMA_MAX)
            us = _reticula(U_MIN if mejor[2] is None else us[mejor[2]], paso_u, U_MIN, U_MAX)
            mejor = _barrer(js, xs, sigmas, us)
        if zoom < ACERCAMIENTOS:
            paso_s, paso_u = paso_s / 10, paso_u / 10
            sigmas = _reticula(sigmas[mejor[1]], paso_s, 0.0, SIGMA_MAX)
            us = _reticula(U_MIN if mejor[2] is None else us[mejor[2]], paso_u, U_MIN, U_MAX)
    sigma = sigmas[mejor[1]]
    kappa = 0.0 if mejor[2] is None else 10 ** us[mejor[2]]
    s, lam = _error(js, xs, sigma, kappa)
    return sigma, kappa, lam, s


def n_estrella(sigma: float, kappa: float) -> float | None:
    """The J of maximum throughput: dX/dJ = 0 <=> 1 - sigma = kappa*J^2. None when kappa is 0."""
    return math.sqrt((1 - sigma) / kappa) if kappa > 0 else None


def rodilla(lam: float, sigma: float, kappa: float, j_min: int, j_max: int, c: int) -> int:
    """The smallest J in [J_min, J_top] whose X reaches 95 % of X(J_top); J_top = min(C, floor(N*), J_max)."""
    tope = min(c, j_max)
    n = n_estrella(sigma, kappa)
    if n is not None:
        tope = min(tope, math.floor(n))
    tope = max(tope, j_min)
    x_tope = lam * g(tope, sigma, kappa)
    for j in range(j_min, tope + 1):
        if lam * g(j, sigma, kappa) >= RODILLA * x_tope:
            return j
    return tope


def _tope(sigma: float, kappa: float, j_max: int, c: int) -> int:
    n = n_estrella(sigma, kappa)
    return min(c, j_max) if n is None else min(c, j_max, math.floor(n))


def _filtrar(puntos: list[dict[str, Any]]) -> tuple[list[dict[str, Any]], dict[str, int]]:
    validos: list[dict[str, Any]] = []
    excluidos = {"rc": 0, "cargado": 0}
    for p in puntos:
        seg = p.get("seg")
        if (
            not isinstance(seg, (int, float))
            or isinstance(seg, bool)
            or seg <= 0
            or not isinstance(p.get("J"), (int, float))
        ):
            fallar(f"a point has no J or no seg above 0: {p}")
        if p.get("rc") != 0:
            excluidos["rc"] += 1
        elif p.get("cargado"):
            excluidos["cargado"] += 1
        else:
            validos.append(p)
    return validos, excluidos


def _recta_ram(validos: list[dict[str, Any]]) -> dict[str, float] | None:
    """Ordinary least squares of rss_pico on J: r(J) = r0 + r1*J, bytes."""
    medidos = [
        (p["J"], p["rss_pico"])
        for p in validos
        if isinstance(p.get("rss_pico"), (int, float)) and not isinstance(p.get("rss_pico"), bool)
    ]
    if len({j for j, _ in medidos}) < 2:
        return None
    mx = statistics.fmean(j for j, _ in medidos)
    my = statistics.fmean(r for _, r in medidos)
    r1 = sum((j - mx) * (r - my) for j, r in medidos) / sum((j - mx) ** 2 for j, _ in medidos)
    return {"r0": my - r1 * mx, "r1": r1}


def _dispersion(validos: list[dict[str, Any]]) -> tuple[float, int | None]:
    """The largest (max T - min T) / median T of a level, and the level."""
    por_nivel: dict[int, list[float]] = {}
    for p in validos:
        por_nivel.setdefault(int(p["J"]), []).append(float(p["seg"]))
    peor, nivel = 0.0, None
    for j, segs in sorted(por_nivel.items()):
        if len(segs) >= 2:
            d = (max(segs) - min(segs)) / statistics.median(segs)
            if d > peor:
                peor, nivel = d, j
    return peor, nivel


def _ajuste_modelo(validos: list[dict[str, Any]]) -> tuple[float, float, float, float]:
    js = [float(p["J"]) for p in validos]
    xs = [1 / float(p["seg"]) for p in validos]
    return _buscar(js, xs)


def _inestable(
    validos: list[dict[str, Any]], niveles: list[int], plena: int, j_min: int, j_max: int, c: int
) -> str | None:
    """Criterion 5: dropping a whole level, J_rodilla (with the bounds of the full sweep) moves at most 2."""
    for nivel in niveles:
        resto = [p for p in validos if int(p["J"]) != nivel]
        sigma, kappa, lam, _ = _ajuste_modelo(resto)
        otra = rodilla(lam, sigma, kappa, j_min, j_max, c)
        if abs(otra - plena) > ESTABILIDAD:
            return f"unstable: J_rodilla is {plena} and becomes {otra} without J={nivel} (limit +-{ESTABILIDAD})"
    return None


def ajustar(puntos: list[dict[str, Any]], c: int) -> dict[str, Any]:
    """The fit of one profile's points; `c` is the cores of the machine that measured them."""
    validos, excluidos = _filtrar(puntos)
    niveles = sorted({int(p["J"]) for p in validos})
    res: dict[str, Any] = {
        "puntos": len(validos),
        "excluidos": excluidos,
        "niveles": niveles,
        "lambda": None,
        "sigma": None,
        "kappa": None,
        "N_estrella": None,
        "J_top": None,
        "J_rodilla": None,
        "rmse_rel": None,
        "dispersion_max": None,
        "T_hat": {},
        "ram": _recta_ram(validos),
        "fiable": False,
        "motivo": None,
    }
    if len(niveles) >= 3:
        sigma, kappa, lam, s = _ajuste_modelo(validos)
        j_min, j_max = niveles[0], niveles[-1]
        res.update(
            {
                "lambda": lam,
                "sigma": sigma,
                "kappa": kappa,
                "N_estrella": n_estrella(sigma, kappa),
                "J_top": _tope(sigma, kappa, j_max, c),
                "J_rodilla": rodilla(lam, sigma, kappa, j_min, j_max, c),
                "rmse_rel": math.sqrt(s / len(validos)),
                "T_hat": {str(j): tiempo(j, lam, sigma, kappa) for j in niveles},
            }
        )
    dispersion, nivel_peor = _dispersion(validos)
    res["dispersion_max"] = dispersion
    res["motivo"] = _motivo(res, validos, niveles, dispersion, nivel_peor, c)
    res["fiable"] = res["motivo"] is None
    return res


def _motivo(
    res: dict[str, Any],
    validos: list[dict[str, Any]],
    niveles: list[int],
    dispersion: float,
    nivel_peor: int | None,
    c: int,
) -> str | None:
    """The first criterion that fails (spec 2.3), or None."""
    if len(niveles) < NIVELES_MIN:
        return f"{len(niveles)} J levels, need {NIVELES_MIN}"
    for nivel in niveles:
        cuantos = sum(1 for p in validos if int(p["J"]) == nivel)
        if cuantos < PUNTOS_POR_NIVEL:
            return f"J={nivel} has {cuantos} valid point(s), need {PUNTOS_POR_NIVEL}"
    if dispersion > DISPERSION_MAX:
        return f"spread {dispersion * 100:.1f} % at J={nivel_peor}, limit {DISPERSION_MAX * 100:.0f} %"
    if res["sigma"] >= SIGMA_MAX * BORDE or res["kappa"] >= KAPPA_MAX * BORDE:
        return f"domain: sigma {res['sigma']:.4g}, kappa {res['kappa']:.4g} at the edge of the fit domain (sigma < {SIGMA_MAX}, kappa < {KAPPA_MAX})"
    if res["rmse_rel"] > RMSE_MAX:
        return f"rmse_rel {res['rmse_rel']:.3f} above {RMSE_MAX}"
    return _inestable(validos, niveles, res["J_rodilla"], niveles[0], niveles[-1], c)


# --- the sweep ----------------------------------------------------------------


def _ppids() -> dict[int, int]:
    pares: dict[int, int] = {}
    for nombre in os.listdir("/proc"):
        if nombre.isdigit():
            try:
                with open(f"/proc/{nombre}/stat", encoding="utf-8", errors="replace") as fh:
                    pares[int(nombre)] = int(fh.read().rpartition(")")[2].split()[1])
            except (OSError, ValueError, IndexError):
                continue
    return pares


def _rss(pid: int) -> int | None:
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8", errors="replace") as fh:
            for linea in fh:
                if linea.startswith("VmRSS:"):
                    return int(linea.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        pass
    return None


def _rss_arbol(raiz_pid: int) -> int | None:
    """The sum of VmRSS of a process and its descendants, B. It counts shared pages twice: conservative."""
    hijos: dict[int, list[int]] = {}
    for pid, padre in _ppids().items():
        hijos.setdefault(padre, []).append(pid)
    total: int | None = None
    pila = [raiz_pid]
    while pila:
        pid = pila.pop()
        valor = _rss(pid)
        if valor is not None:
            total = (total or 0) + valor
        pila += hijos.get(pid, [])
    return total


def _plazo_orden() -> float:
    try:
        return float(os.environ.get("GATE_USL_PLAZO_S", PLAZO_ORDEN_S))
    except ValueError:
        fallar(f"GATE_USL_PLAZO_S is not a number of seconds: {os.environ['GATE_USL_PLAZO_S']!r}")


def _correr(orden: list[str], entorno: dict[str, str], cwd: str, plazo_s: float) -> tuple[int, int | None]:
    """One cargo order: (rc, peak RSS of its tree). A cargo past its deadline is killed with its group: exit 2."""
    with tempfile.TemporaryFile() as salida:
        try:
            proc = subprocess.Popen(
                ["cargo", *orden],
                cwd=cwd,
                env=entorno,
                stdin=subprocess.DEVNULL,
                stdout=salida,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        except OSError as err:
            fallar(f"cargo {' '.join(orden)} cannot start ({err})")
        limite = time.monotonic() + plazo_s
        pico: int | None = None
        while True:
            actual = _rss_arbol(proc.pid)
            if actual is not None and (pico is None or actual > pico):
                pico = actual
            try:
                rc = proc.wait(timeout=MUESTREO_S)
                break
            except subprocess.TimeoutExpired:
                if time.monotonic() >= limite:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait()
                    fallar(f"cargo {' '.join(orden)} timed out after {plazo_s:g} s and was killed")
        if rc != 0:
            salida.seek(0)
            cola = salida.read().decode("utf-8", errors="replace").splitlines()[-20:]
            sys.stderr.write(f"cargo {' '.join(orden)} exited {rc}:\n" + "\n".join(cola) + "\n")
    return rc, pico


def rejilla_de(perfil: dict[str, Any], j_m: int) -> list[int]:
    """D-E7j: the J of a sweep, J_m times each fraction of the profile rounded half up (integer arithmetic), at least 1,
    with no repeats."""
    return sorted({max(1, (2 * j_m * num + den) // (2 * den)) for num, den in perfil["fracciones"]})


def _zigzag(rejilla: list[int]) -> list[int]:
    """[J0, Jk, J1, Jk-1, ...] over the sorted grid: neighbours in J are far apart in time."""
    ordenada = sorted(rejilla)
    orden: list[int] = []
    i, j = 0, len(ordenada) - 1
    while i <= j:
        orden.append(ordenada[i])
        i += 1
        if i <= j:
            orden.append(ordenada[j])
            j -= 1
    return orden


def _parse_j(texto: str) -> list[int]:
    try:
        niveles = sorted({int(t) for t in texto.split(",")})
    except ValueError:
        fallar(f"--j has to be a list of integers like 2,4,8: {texto!r}")
    if not niveles or niveles[0] < 1:
        fallar(f"--j has to hold jobs of 1 or more: {texto!r}")
    return niveles


def _cpu_hijos() -> float:
    uso = resource.getrusage(resource.RUSAGE_CHILDREN)
    return uso.ru_utime + uso.ru_stime


def _enfriar(lib: str) -> None:
    """The crate is cold for cargo: a newer mtime, the same bytes (the huella does not move)."""
    ahora = max(time.time_ns(), os.stat(lib).st_mtime_ns + 1_000_000)
    os.utime(lib, ns=(ahora, ahora))


def _punto(
    perfil: dict[str, Any], j: int, base: dict[str, str], cwd: str, psi_archivo: str, plazo_s: float
) -> dict[str, Any]:
    dag = gate_dag()
    _enfriar(os.path.join(cwd, "src", "lib.rs"))
    entorno = {**base, "CARGO_BUILD_JOBS": str(j)}
    psi_ini = dag.psi_some_avg10(psi_archivo)
    cpu0, t0 = _cpu_hijos(), time.monotonic()
    rc, pico = 0, None
    for orden in perfil["ordenes"]:
        rc, rss = _correr(orden, entorno, cwd, plazo_s)
        if rss is not None and (pico is None or rss > pico):
            pico = rss
        if rc != 0:
            break
    seg, cpu_s = time.monotonic() - t0, _cpu_hijos() - cpu0
    psi_fin = dag.psi_some_avg10(psi_archivo)
    return {
        "seg": round(seg, 3),
        "cpu_s": round(cpu_s, 3),
        "rss_pico": pico,
        "rc": rc,
        "psi_ini": psi_ini,
        "psi_fin": psi_fin,
        "load": dag.loadavg(),
        "cargado": psi_fin is None or psi_fin > PSI_MAX,
    }


def _fila_de_la_tabla(id_: str) -> Any:
    """The row of scripts/gate-dag.txt that a profile sweeps: its tokens are the profile of the sweep (D-K4f)."""
    dag = gate_dag()
    tabla = os.path.join(raiz(), "scripts", "gate-dag.txt")
    try:
        pasos = dag.parse_table(tabla)
    except (OSError, dag.Refusal) as err:
        fallar(f"the table {tabla} cannot be read ({err}): the sweep cannot know the profile of {id_}")
    fila = next((p for p in pasos if p.id == id_), None)
    if fila is None:
        fallar(f"the table {tabla} has no row {id_}: the sweep cannot know its profile")
    return fila


def cmd_barrido(args: argparse.Namespace) -> int:
    perfil = PERFILES.get(args.perfil)
    if perfil is None:
        fallar(f"unknown profile {args.perfil!r}: one of {', '.join(PERFILES)}")
    if not os.environ.get("CARGO_TARGET_DIR"):
        fallar("CARGO_TARGET_DIR is not set: the sweep builds in the target of the judge's row (D-F5h)")
    pedida = _parse_j(args.j) if args.j else None
    if args.reps < 1:
        fallar("--reps has to be 1 or more")
    plazo_s = _plazo_orden()
    dag = gate_dag()
    fila = _fila_de_la_tabla(perfil["fila"])
    psi_archivo = os.environ.get("GATE_USL_PSI", PSI_ARCHIVO)
    psi = dag.psi_some_avg10(psi_archivo)
    if psi is None:
        fallar(f"the pressure cannot be read from {psi_archivo}: the machine cannot be called quiet")
    if psi > PSI_MAX:
        fallar(f"busy: some avg10 of {psi_archivo} is {psi:.2f} %, above {PSI_MAX:.0f} %; nothing was run")
    medida = dag.measure_machine()
    rejilla = pedida or rejilla_de(perfil, medida.jobs)
    if pedida is None and len(rejilla) < NIVELES_MIN:
        razon = f" ({medida.why})" if medida.why else ""
        fallar(
            f"J_m={medida.jobs}{razon}: the grid of {args.perfil} has {len(rejilla)} levels ({','.join(map(str, rejilla))}), "
            f"need {NIVELES_MIN}; nothing was run"
        )
    destino = dag.target_dir(dag.target_of(fila))
    cwd = os.path.join(raiz(), "rust")
    sha, sucio = dag.git_state(raiz())
    huella_ini = gate_history.huella(raiz())
    cores = dag.capacities(medida)[0]
    # The profile of the row is in the hash of every unit (D-K4f): without its tokens the sweep would measure another build
    # and leave a second set of artifacts in the target of the judge.
    # CARGO_INCREMENTAL=0 stays a literal: B1, the readers of T_dbg and DC carry it in their rows, B3 and REL (release) do not, and the sweep of
    # release has always built with it (CI-K10, H5).
    base = {**os.environ, **dag.profile_of(fila), "CARGO_INCREMENTAL": "0", "CARGO_TARGET_DIR": destino}
    barrido = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S") + f"-{os.getpid()}"
    for orden in perfil["ordenes"]:
        # the warm-up leaves the dependencies built; it writes nothing
        rc, _ = _correr(orden, {**base, "CARGO_BUILD_JOBS": str(max(rejilla))}, cwd, plazo_s)
        if rc != 0:
            fallar(f"the warm-up (cargo {' '.join(orden)}) exited {rc}: the tree does not build, nothing was measured")
    cargados = rojos = total = 0
    for rep in range(args.reps):
        orden_j = _zigzag(rejilla) if rep % 2 == 0 else _zigzag(rejilla)[::-1]
        for posicion, j in enumerate(orden_j):
            registro = {
                "repo": gate_history.REPO,
                "barrido": barrido,
                "perfil": args.perfil,
                "fila": perfil["fila"],
                "J": j,
                "rep": rep,
                "orden": posicion,
                **_punto(perfil, j, base, cwd, psi_archivo, plazo_s),
                "sha": sha,
                "huella": huella_ini,
                "arbol_sucio": sucio,
                "C": cores,
                "fecha": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            }
            gate_history.usl_point(registro)
            print(json.dumps(registro, sort_keys=True), flush=True)
            total += 1
            cargados += registro["cargado"]
            rojos += registro["rc"] != 0
    print(f"barrido {barrido}: {total} puntos, {cargados} cargados, {rojos} rojos", flush=True)
    if gate_history.huella(raiz()) != huella_ini:
        fallar("the huella of the tree changed during the sweep: the points do not measure one tree")
    return 0


# --- the history --------------------------------------------------------------


def _dias() -> list[str]:
    return sorted(glob.glob(os.path.join(gate_history.history_root(), "*")))


def _leer_historial(nombre: str) -> list[dict[str, Any]]:
    registros: list[dict[str, Any]] = []
    for dia in _dias():
        registros += gate_history.leer_jsonl(os.path.join(dia, nombre))
    return registros


def _ultimo_barrido(perfil: str) -> list[dict[str, Any]]:
    puntos = [p for p in _leer_historial("usl.jsonl") if p.get("perfil") == perfil]
    if not puntos:
        return []
    ultimo = puntos[-1].get("barrido")
    return [p for p in puntos if p.get("barrido") == ultimo]


def _cores_de(puntos: list[dict[str, Any]]) -> int:
    declarados = [p["C"] for p in puntos if isinstance(p.get("C"), int) and not isinstance(p.get("C"), bool)]
    return min(declarados) if declarados else gate_dag().capacities()[0]


def _leer_puntos(ruta: str) -> list[dict[str, Any]]:
    try:
        with open(ruta, encoding="utf-8") as fh:
            return [json.loads(linea) for linea in fh if linea.strip()]
    except (OSError, ValueError) as err:
        fallar(f"the points in {ruta} cannot be read as JSON lines ({err})")


def _ajuste_del_historial(perfil: str) -> dict[str, Any] | None:
    puntos = _ultimo_barrido(perfil)
    if not puntos:
        return None
    res = ajustar(puntos, _cores_de(puntos))
    res["perfil"], res["barrido"], res["sha"] = perfil, puntos[-1].get("barrido"), puntos[-1].get("sha")
    return res


def cmd_ajustar(args: argparse.Namespace) -> int:
    if args.perfil not in PERFILES:
        fallar(f"unknown profile {args.perfil!r}: one of {', '.join(PERFILES)}")
    if args.puntos:
        puntos = _leer_puntos(args.puntos)
        res = ajustar(puntos, _cores_de(puntos))
        res["perfil"], res["barrido"], res["sha"] = args.perfil, None, None
    else:
        res = _ajuste_del_historial(args.perfil)
        if res is None:
            fallar(
                f"the history has no sweep of the profile {args.perfil}: run `python3 -I scripts/gate_usl.py barrido --perfil {args.perfil}`"
            )
    if args.json:
        print(json.dumps(res, sort_keys=True))
    else:
        _imprimir_ajuste(res)
    return 0 if res["fiable"] else 3


def _imprimir_ajuste(res: dict[str, Any]) -> None:
    print(
        f"perfil {res['perfil']}: {res['puntos']} puntos en {len(res['niveles'])} niveles de J, excluidos {res['excluidos']}"
    )
    if res["sigma"] is not None:
        n = "sin tope (kappa 0)" if res["N_estrella"] is None else f"{res['N_estrella']:.1f}"
        print(f"  sigma {res['sigma']:.4f}  kappa {res['kappa']:.6f}  lambda {res['lambda']:.6g} 1/s  N* {n}")
        print(
            f"  J_top {res['J_top']}  J_rodilla {res['J_rodilla']}  rmse_rel {res['rmse_rel']:.4f}  dispersion_max {res['dispersion_max']:.4f}"
        )
    print("  fiable" if res["fiable"] else f"  no fiable: {res['motivo']}")


# --- slack and simulation -----------------------------------------------------


def holguras(filas: list[dict[str, Any]], duraciones: dict[str, float]) -> dict[str, Any]:
    """L_in, L_out, L_j and h_j = L - L_j of the DAG (spec 2.4). A row is `critica` at h/L <= 5 %,
    `casi_critica` up to 20 %."""
    ordenadas = sorted(filas, key=lambda f: f["idx"])
    entra: dict[str, float] = {}
    for f in ordenadas:
        entra[f["id"]] = duraciones[f["id"]] + max((entra[d] for d in f["deps"]), default=0.0)
    sale: dict[str, float] = {}
    for f in reversed(ordenadas):
        sucesores = [s for s in ordenadas if f["id"] in s["deps"]]
        sale[f["id"]] = duraciones[f["id"]] + max((sale[s["id"]] for s in sucesores), default=0.0)
    camino = {f["id"]: entra[f["id"]] + sale[f["id"]] - duraciones[f["id"]] for f in ordenadas}
    largo = max(camino.values())
    resultado: dict[str, Any] = {}
    for f in ordenadas:
        h = largo - camino[f["id"]]
        relativa = h / largo if largo > 0 else 0.0
        estado = "critica" if relativa <= CRITICA else "casi_critica" if relativa <= CASI_CRITICA else "holgada"
        resultado[f["id"]] = {
            "d": duraciones[f["id"]],
            "L_j": camino[f["id"]],
            "h": h,
            "h_L": relativa,
            "estado": estado,
        }
    return {"L": largo, "filas": resultado}


def _paso(f: dict[str, Any]) -> Any:
    """The step of the planner that a row of the simulation stands for: what `dag.admission` reads of it."""
    dag = gate_dag()
    env = {"CARGO_BUILD_JOBS": str(f["jobs"])} if f["jobs"] else {}
    paso = dag.Step(f["idx"], f["id"], f["deps"], f["recursos"], f["clase"], f["c"], 0.0, f["d"], env, [])
    paso.r = f["r"]
    return paso


def simular(
    filas: list[dict[str, Any]],
    cpu_cap: float,
    ram: float,
    jobs_cap: int,
    arranques: list[tuple[float, str]] | None = None,
) -> float:
    """The clock of the DAG by list scheduling with the planner's own admission and order (D-E7f, D-E7g): the ready rows by
    (-rank, idx) of `dag.ranks`, each one started when `dag.admission` lets it in with the RAM the live rows leave. A row
    is a dict with id, idx, deps, clase, recursos, d, c, r (bytes) and jobs (the CARGO_BUILD_JOBS it names, 0 for none).
    Contention is not modelled. Each start is appended to `arranques` as (instant, id) when a list is given."""
    dag = gate_dag()
    pasos = {f["id"]: _paso(f) for f in filas}
    duracion = {f["id"]: f["d"] for f in filas}
    rango = dag.ranks(list(pasos.values()), duracion)[0]
    pendientes = set(pasos)
    vivas: dict[str, float] = {}
    hechas: set[str] = set()
    ahora = fin = 0.0
    while pendientes or vivas:
        listas = sorted(
            (pasos[i] for i in pendientes if all(d in hechas for d in pasos[i].deps)),
            key=lambda p: (-rango[p.id], p.idx),
        )
        for p in listas:
            en_marcha = [pasos[i] for i in vivas]
            antes = any(pasos[i].idx < p.idx for i in pendientes if i != p.id)
            libre = ram - sum(o.r for o in en_marcha)
            if dag.admission(p, en_marcha, antes, cpu_cap, ram, jobs_cap, libre) is None:
                vivas[p.id] = ahora + duracion[p.id]
                pendientes.discard(p.id)
                if arranques is not None:
                    arranques.append((ahora, p.id))
        if not vivas:
            break
        ahora = min(vivas.values())
        for id_ in [i for i, t in vivas.items() if t <= ahora + 1e-9]:
            hechas.add(id_)
            del vivas[id_]
        fin = max(fin, ahora)
    return fin


# --- the recommendation -------------------------------------------------------


class Historial:
    """The green serial runs of the history and the steps of each one."""

    def __init__(self) -> None:
        self.corridas: list[dict[str, Any]] = []
        vistas: set[str] = set()
        for c in _leer_historial("corridas.jsonl"):
            if (
                c.get("modo") == "serie"
                and c.get("rc") == 0
                and isinstance(c.get("run"), str)
                and c["run"] not in vistas
            ):
                vistas.add(c["run"])
                self.corridas.append(c)
        runs = {c["run"] for c in self.corridas}
        self.pasos: dict[str, dict[str, dict[str, Any]]] = {}
        for p in _leer_historial("pasos.jsonl"):
            seg = p.get("seg")
            if (
                p.get("run") in runs
                and p.get("modo") == "serie"
                and p.get("rc") == 0
                and isinstance(seg, (int, float))
                and seg > 0
            ):
                self.pasos.setdefault(p["run"], {})[p["id"]] = p
        b3 = PERFILES["release"]["fila"]
        self.frias = [
            c["run"] for c in self.corridas if self.pasos.get(c["run"], {}).get(b3, {}).get("seg", 0) >= FRIO_B3_S
        ]

    def _de_la_maquina(self) -> dict[str, Any]:
        """The last green serial run with C and R: the machine the simulation counts with."""
        for c in reversed(self.corridas):
            if isinstance(c.get("C"), int) and isinstance(c.get("R"), int):
                return c
        fallar("the history has no green serial run with C and R: R is missing")

    def capacidades(self) -> tuple[int, int]:
        corrida = self._de_la_maquina()
        return corrida["C"], corrida["R"]

    def jobs_de_la_maquina(self, notas: list[str]) -> int:
        """D-E7h: J_m, the `plan.jobs` of the run that C and R come from; C when that run has no plan."""
        corrida = self._de_la_maquina()
        jobs = _jobs_del_plan(corrida)
        if jobs is None:
            notas.append(f"the last green serial run has no plan: J_m is C ({corrida['C']})")
            return int(corrida["C"])
        return jobs

    def corrida(self, run: str) -> dict[str, Any]:
        return next(c for c in self.corridas if c["run"] == run)

    def muestras(self, fila: str, runs: list[str]) -> list[dict[str, Any]]:
        return [self.pasos[r][fila] for r in runs if fila in self.pasos.get(r, {})][-VENTANA:]


def _jobs_del_plan(corrida: dict[str, Any]) -> int | None:
    """The jobs the machine gave a run (`plan.jobs`, D-K4q); None when the run has no plan or a plan with no jobs."""
    plan = corrida.get("plan")
    jobs = plan.get("jobs") if isinstance(plan, dict) else None
    return jobs if isinstance(jobs, int) and not isinstance(jobs, bool) and jobs > 0 else None


def _jobs_de(contenido: bytes, fila: str) -> int | None:
    """CARGO_BUILD_JOBS of a row of a table as git has it; None when the row or the variable is not there."""
    dag = gate_dag()
    for n, linea in enumerate(contenido.decode("utf-8", errors="replace").splitlines(), 1):
        linea = linea.strip()
        if linea and not linea.startswith("#"):
            try:
                paso = dag.parse_row(linea, n)
            except dag.Refusal:
                continue
            if paso.id == fila:
                valor = paso.env.get("CARGO_BUILD_JOBS", "")
                return int(valor) if valor.isdigit() else None
    return None


def _tablas_conocidas(tabla: str) -> dict[str, bytes]:
    """{key: content} of the table as of today and of every commit that changed it (D-F5s). The key is the one the planner
    writes in the history (gate-dag.py table_key_of, K9c), not the sha256 of the bytes: a comment does not change it."""
    clave = gate_dag().table_key_of
    with open(tabla, "rb") as fh:
        actual = fh.read()
    conocidas = {clave(actual): actual}
    real, base = os.path.realpath(tabla), os.path.realpath(raiz())
    relativa = os.path.relpath(real, base)
    if relativa.startswith(".."):
        return conocidas
    log = subprocess.run(
        ["git", "-C", base, "log", "--format=%H", "--", relativa], capture_output=True, text=True, check=False
    )
    for commit in log.stdout.split():
        contenido = subprocess.run(
            ["git", "-C", base, "show", f"{commit}:{relativa}"], capture_output=True, check=False
        )
        if contenido.returncode == 0:
            conocidas.setdefault(clave(contenido.stdout), contenido.stdout)
    return conocidas


def _p95(valores: list[float]) -> float:
    return gate_dag().p95(valores)


def _base_filas(pasos: list[Any], hist: Historial) -> list[dict[str, Any]]:
    """Rows with the duration, cores and RAM of the history: the cold-sensitive rows only from the cold runs."""
    dag = gate_dag()
    todas = [c["run"] for c in hist.corridas]
    filas: list[dict[str, Any]] = []
    for p in pasos:
        runs = hist.frias if p.id in FILAS_EN_FRIO else todas
        muestras = hist.muestras(p.id, runs)
        d = statistics.median(m["seg"] for m in muestras) if muestras else p.dur_tab
        nucleos = [m["cpu_s"] / m["seg"] for m in muestras if m.get("cpu_s") is not None]
        anon = [m["anon_max"] for m in muestras if m.get("anon_max") is not None]
        fila = {
            "id": p.id,
            "idx": p.idx,
            "deps": p.deps,
            "clase": p.clase,
            "recursos": p.recursos,
            "jobs": dag.named_jobs(p),
            "d": float(d),
            "c": _p95(nucleos) if nucleos else p.cpu_tab,
            "r": _p95(anon) * SEGURIDAD_RAM if anon else p.ram_tab * GIB,
        }
        filas.append(fila)
    return filas


def _veredicto_juez_cuda(hist: Historial, holgura: dict[str, Any]) -> str:
    n = len(hist.corridas)
    if n < CORRIDAS_MIN:
        return f"not evaluable ({n} run{'' if n == 1 else 's'}, need {CORRIDAS_MIN})"
    if not hist.frias:
        return "not evaluable (no run with a cold build: B3 under 60 s in all of them)"
    b3 = holgura["filas"][PERFILES["release"]["fila"]]
    cuerpo = f"(B3 slack {b3['h']:.1f} s, {b3['h_L'] * 100:.1f} %)"
    return f"yes {cuerpo}" if b3["h_L"] <= CRITICA else f"no {cuerpo}"


class Calibrada:
    """A row with a reliable fit: its candidate J, and the duration each one would have (D-F5s, 2.5)."""

    def __init__(self, fila: str, perfil: str, ajuste: dict[str, Any], actual: int, pool: int, j_maquina: int) -> None:
        self.fila, self.perfil, self.ajuste = fila, perfil, ajuste
        self.actual, self.pool, self.j_maquina = actual, pool, j_maquina
        self.tope_j = min(j_maquina, ajuste["J_rodilla"])
        self.usadas: list[tuple[str, int, float]] = []

    def efectivo(self, j: int) -> int:
        """The jobs a row that names `j` gets on this machine (D-E7d)."""
        return gate_dag().row_jobs(j, self.j_maquina)

    def t(self, j: int) -> float:
        a = self.ajuste
        return tiempo(j, a["lambda"], a["sigma"], a["kappa"])

    def duracion(self, j: int) -> float:
        return max(1.0, statistics.median(seg - self.t(j_hist) + self.t(j) for _, j_hist, seg in self.usadas))

    def ram(self, j: int) -> float | None:
        recta = self.ajuste["ram"]
        return None if recta is None else SEGURIDAD_RAM * (recta["r0"] + recta["r1"] * j)

    def candidatos(self, ram_max: float) -> list[int]:
        medidos = {j for j in self.ajuste["niveles"] if j <= self.tope_j}
        cabe = [j for j in medidos if self.ram(j) is None or self.ram(j) <= ram_max]
        return sorted(set(cabe) | {self.efectivo(self.actual)})


def _usar_corridas(cal: Calibrada, hist: Historial, conocidas: dict[str, bytes], notas: list[str]) -> None:
    """The cold runs of a table that git knows, each with the J its table gave the row and its machine gave the run: the
    row got row_jobs(J of the table, plan.jobs of the run) (U16, D-E7h). A run with no plan counts the J of the table."""
    avisadas: set[str] = set()
    jobs: dict[str, int | None] = {}
    for run in hist.frias:
        tabla = str(hist.corrida(run).get("tabla"))
        if tabla not in conocidas:
            if tabla not in avisadas:
                avisadas.add(tabla)
                notas.append(f"table {tabla[:12]} not in git: its runs are not used for {cal.fila}")
            continue
        if tabla not in jobs:
            jobs[tabla] = _jobs_de(conocidas[tabla], cal.fila)
        if jobs[tabla] is not None and cal.fila in hist.pasos.get(run, {}):
            j_tabla = int(jobs[tabla] or 0)
            j_corrida = _jobs_del_plan(hist.corrida(run))
            if j_corrida is None:
                notas.append(f"run {run} has no plan: the J of the table ({j_tabla}) is used for {cal.fila}")
                recibido = j_tabla
            else:
                recibido = gate_dag().row_jobs(j_tabla, j_corrida)
            cal.usadas.append((run, recibido, float(hist.pasos[run][cal.fila]["seg"])))
    cal.usadas = cal.usadas[-VENTANA:]


def _estado_de(
    base: list[dict[str, Any]], cals: dict[str, Calibrada], eleccion: dict[str, int]
) -> list[dict[str, Any]]:
    """The rows with the duration, cores and RAM of the chosen J of each calibrated row."""
    filas: list[dict[str, Any]] = []
    for f in base:
        nueva = dict(f)
        cal = cals.get(f["id"])
        if cal is not None:
            j = eleccion[f["id"]]
            efectivo = cal.efectivo(j)
            nueva["d"] = cal.duracion(efectivo)
            nueva["jobs"] = efectivo
            if j != cal.actual:
                nueva["c"] = float(max(efectivo, cal.pool))
            ram = cal.ram(efectivo)
            if ram is not None:
                nueva["r"] = max(f["r"], ram)
        filas.append(nueva)
    return filas


def _elegir(
    base: list[dict[str, Any]], cals: dict[str, Calibrada], cpu_cap: float, ram: float, j_m: int
) -> tuple[dict[str, int], dict[str, int], float, float]:
    """The combination of J with the shortest simulated clock; a tie (< 5 s or < 1 %) goes to the smaller J."""
    ids = sorted(cals, key=lambda i: next(f["idx"] for f in base if f["id"] == i))
    listas = [cals[i].candidatos(ram) for i in ids]
    reloj: dict[tuple[int, ...], float] = {}
    for combo in itertools.product(*listas):
        reloj[combo] = simular(_estado_de(base, cals, dict(zip(ids, combo))), cpu_cap, ram, j_m)
    mejor = min(reloj.values())
    margen = max(EMPATE_S, EMPATE_REL * mejor)
    elegido = min((c for c, t in reloj.items() if t < mejor + margen), key=lambda c: (sum(c), c))
    actual = {i: cals[i].actual for i in ids}
    return dict(zip(ids, elegido)), actual, reloj[elegido], simular(_estado_de(base, cals, actual), cpu_cap, ram, j_m)


def _fila_nueva(linea: str, j: int, cpu: int, ram_gb: int | None) -> str:
    trozos = re.split(r"(\s+)", linea.rstrip("\n"))
    campos = [i for i, t in enumerate(trozos) if t and not t.isspace()]
    trozos[campos[5]] = str(cpu)
    if ram_gb is not None:
        trozos[campos[6]] = str(ram_gb)
    return re.sub(r"CARGO_BUILD_JOBS=\d+", f"CARGO_BUILD_JOBS={j}", "".join(trozos)) + "\n"


def _diff(tabla: str, cambios: dict[str, tuple[int, int, int | None]]) -> str:
    with open(tabla, encoding="utf-8") as fh:
        antes = fh.readlines()
    despues = []
    for linea in antes:
        campos = linea.split()
        if len(campos) > 5 and not linea.startswith("#") and campos[1] in cambios:
            j, cpu, ram_gb = cambios[campos[1]]
            linea = _fila_nueva(linea, j, cpu, ram_gb)
        despues.append(linea)
    relativa = os.path.relpath(os.path.realpath(tabla), os.path.realpath(raiz()))
    return "".join(difflib.unified_diff(antes, despues, f"a/{relativa}", f"b/{relativa}"))


def _ram_gb_nueva(cal: Calibrada, j: int, actual_gb: float) -> int | None:
    ram = cal.ram(j)
    if ram is None or ram <= actual_gb * GIB:
        return None
    return math.ceil(ram / GIB)


def recomendar(args: argparse.Namespace) -> dict[str, Any]:
    dag = gate_dag()
    tabla = args.tabla or os.path.join(raiz(), "scripts", "gate-dag.txt")
    try:
        pasos = dag.parse_table(tabla)
    except (OSError, dag.Refusal) as err:
        fallar(f"the table {tabla} cannot be read ({err})")
    hist = Historial()
    c, ram = hist.capacidades()
    notas: list[str] = []
    j_m = hist.jobs_de_la_maquina(notas)
    faltan: list[str] = []
    if not hist.frias:
        faltan.append("a green serial run after a code change (B3 of 60 s or more): none in the history")
    base = _base_filas(pasos, hist)
    plana = holguras(base, {f["id"]: f["d"] for f in base})
    res: dict[str, Any] = {
        "corridas": len(hist.corridas),
        "frias": len(hist.frias),
        "C": c,
        "R": ram,
        "J_maquina": j_m,
        "supone": "dag",
        "L": plana["L"],
        "filas": plana["filas"],
        "juez_cuda": _veredicto_juez_cuda(hist, plana),
        "faltan": faltan,
        "calibradas": {},
        "T_sim_actual": simular(base, c, ram, j_m),
        "T_sim_propuesto": None,
        "cpu_cap": {"C": c, "N_estrella": None, "propuesto": c},
        "diff": None,
        "se_conserva": None,
        "notas": notas,
    }
    ajustes: dict[str, dict[str, Any]] = {}
    for nombre, perfil in PERFILES.items():
        if plana["filas"].get(perfil["fila"], {}).get("estado") not in ("critica", "casi_critica"):
            continue
        ajuste = _ajuste_del_historial(nombre)
        if ajuste is None or not ajuste["fiable"]:
            faltan.append(f"python3 -I scripts/gate_usl.py barrido --perfil {nombre}")
            if ajuste is not None:
                notas.append(f"the last sweep of {nombre} is not reliable: {ajuste['motivo']}")
        else:
            ajustes[nombre] = ajuste
    if faltan:
        return res
    _calibrar(res, pasos, hist, base, ajustes, tabla, notas)
    return res


def _calibrar(
    res: dict[str, Any],
    pasos: list[Any],
    hist: Historial,
    base: list[dict[str, Any]],
    ajustes: dict[str, dict[str, Any]],
    tabla: str,
    notas: list[str],
) -> None:
    c, ram, j_m = res["C"], res["R"], res["J_maquina"]
    conocidas = _tablas_conocidas(tabla)
    por_paso = {p.id: p for p in pasos}
    cals: dict[str, Calibrada] = {}
    for nombre, ajuste in ajustes.items():
        fila = PERFILES[nombre]["fila"]
        paso = por_paso[fila]
        actual = paso.env.get("CARGO_BUILD_JOBS", "")
        if not actual.isdigit():
            fallar(f"the row {fila} of the table has no CARGO_BUILD_JOBS to calibrate")
        pool = max((int(v) for k, v in paso.env.items() if k.endswith("_INTRA_THREADS") and v.isdigit()), default=0)
        cal = Calibrada(fila, nombre, ajuste, int(actual), pool, j_m)
        _usar_corridas(cal, hist, conocidas, notas)
        if not cal.usadas:
            res["faltan"].append(f"a cold green serial run of a table that git knows, for {fila}")
        cals[fila] = cal
    if res["faltan"]:
        return
    L = res["L"]
    umbral = max(GANANCIA_MIN_S, GANANCIA_MIN_REL * L)
    if cals:
        eleccion, _, _, t_actual = _elegir(base, cals, c, ram, j_m)
        ganancias = _revertir(base, cals, eleccion, c, ram, j_m, umbral)
        final = simular(_estado_de(base, cals, eleccion), c, ram, j_m)
        res["T_sim_actual"], res["T_sim_propuesto"] = t_actual, final
    else:
        eleccion, ganancias = {}, {}
        res["T_sim_propuesto"] = res["T_sim_actual"]
    cambios: dict[str, tuple[int, int, int | None]] = {}
    for fila, cal in cals.items():
        j = eleccion[fila]
        propone = j != cal.actual
        if propone:
            cambios[fila] = (j, max(j, cal.pool), _ram_gb_nueva(cal, j, por_paso[fila].ram_tab))
        res["calibradas"][fila] = {
            "perfil": cal.perfil,
            "J_actual": cal.actual,
            "J_propuesto": j,
            "J_efectivo": cal.efectivo(j),
            "propone": propone,
            "ganancia_s": ganancias[fila],
            "umbral_s": umbral,
            "J_hist": sorted({j_hist for _, j_hist, _ in cal.usadas}),
            "runs": [r for r, _, _ in cal.usadas],
            "d_hist": statistics.median(seg for _, _, seg in cal.usadas),
            "d_por_J": {str(x): cal.duracion(cal.efectivo(x)) for x in cal.candidatos(ram)},
        }
    debug = ajustes.get("debug")
    n = None if debug is None else debug["N_estrella"]
    res["cpu_cap"] = {"C": c, "N_estrella": n, "propuesto": c if n is None else min(c, math.floor(n))}
    if cambios:
        res["diff"] = _diff(tabla, cambios)
    else:
        res["se_conserva"] = f"no calibrated row gains {umbral:.1f} s of simulated clock"


def _revertir(
    base: list[dict[str, Any]],
    cals: dict[str, Calibrada],
    eleccion: dict[str, int],
    c: float,
    ram: float,
    j_m: int,
    umbral: float,
) -> dict[str, float]:
    """A row keeps its new J only if putting the old one back costs the simulated clock the threshold (D-F5q).
    Returns the gain of each row: what its change buys, 0 where nothing changes."""
    ganancias = {f: 0.0 for f in cals}
    for fila in sorted(cals, key=lambda i: next(f["idx"] for f in base if f["id"] == i)):
        cal = cals[fila]
        if eleccion[fila] == cal.actual:
            continue
        con = simular(_estado_de(base, cals, eleccion), c, ram, j_m)
        sin = simular(_estado_de(base, cals, {**eleccion, fila: cal.actual}), c, ram, j_m)
        ganancias[fila] = sin - con
        if sin - con < umbral:
            eleccion[fila] = cal.actual
    return ganancias


def _imprimir_recomendacion(res: dict[str, Any]) -> None:
    print(
        f"corridas verdes en serie: {res['corridas']} ({res['frias']} en frío); C = {res['C']}, R = {res['R'] / GIB:.1f} GiB"
    )
    print(
        f"supone: DAG con la admisión del planificador (exclusividad, modelos, CPU, RAM, jobs <= J={res['J_maquina']}), "
        "sin contención; en serie el reloj es la suma de las filas (no calibrado: E7b)"
    )
    print(f"holgura por fila (L = {res['L']:.3f} s):")
    for id_, f in res["filas"].items():
        print(f"  {id_:<4} d = {f['d']:9.3f} s  h = {f['h']:9.3f} s  {f['h_L'] * 100:5.1f} %  {f['estado']}")
    print(f"juez-cuda: {res['juez_cuda']}")
    if res["faltan"]:
        print("faltan:")
        for orden in res["faltan"]:
            print(f"  {orden}")
        return
    for fila, c in res["calibradas"].items():
        accion = "propone" if c["propone"] else f"se conserva (ganancia por debajo de {c['umbral_s']:.1f} s)"
        print(f"{fila}: J {c['J_actual']} -> {c['J_propuesto']}, ganancia simulada {c['ganancia_s']:.1f} s: {accion}")
    print(f"T_sim: actual {res['T_sim_actual']:.1f} s, propuesto {res['T_sim_propuesto']:.1f} s")
    cap = res["cpu_cap"]
    n = "sin tope (kappa 0)" if cap["N_estrella"] is None else f"{cap['N_estrella']:.1f}"
    print(
        f"cpu_cap: C = {cap['C']}, N* de debug = {n}, min(C, N*) = {cap['propuesto']} (informativo: el simulador no modela la contención)"
    )
    print(res["diff"] if res["diff"] else f"se conserva: {res['se_conserva']}")


def cmd_recomendar(args: argparse.Namespace) -> int:
    res = recomendar(args)
    for nota in res["notas"]:
        sys.stderr.write(f"note: {nota}\n")
    if args.json:
        print(json.dumps(res, sort_keys=True))
    else:
        _imprimir_recomendacion(res)
    return 3 if res["faltan"] else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="USL calibration of the local judge (CI-F5a).")
    ordenes = parser.add_subparsers(dest="orden", required=True)
    b = ordenes.add_parser("barrido", help="measure a profile at several CARGO_BUILD_JOBS")
    b.add_argument("--perfil", required=True)
    b.add_argument("--j", default=None, help="comma separated jobs; default: the profile's fractions of J_m")
    b.add_argument("--reps", type=int, default=2)
    b.set_defaults(func=cmd_barrido)
    a = ordenes.add_parser("ajustar", help="fit the USL curve to the points of a profile")
    a.add_argument("--perfil", required=True)
    a.add_argument("--puntos", default=None, help="a JSON lines file; default: the last sweep in the history")
    a.add_argument("--json", action="store_true")
    a.set_defaults(func=cmd_ajustar)
    r = ordenes.add_parser("recomendar", help="slack, juez-cuda verdict and the J that the DAG simulation prefers")
    r.add_argument("--tabla", default=None)
    r.add_argument("--json", action="store_true")
    r.set_defaults(func=cmd_recomendar)
    args = parser.parse_args(argv)
    try:
        return int(args.func(args))
    except Fallo as err:
        sys.stderr.write(f"FAIL: {err}\n")
        return err.codigo
    except gate_dag().Refusal as err:
        sys.stderr.write(f"FAIL: {err}\n")
        return 2


if __name__ == "__main__":
    sys.exit(main())
