#!/usr/bin/env bash
# CI-D5b: prueba real de ONNX Runtime 1.31.1 con una tarjeta NVIDIA, fuera del juez (D-D5b-i).
#
# Un solo guion para los dos sistemas: en WSL/Ubuntu corre los pasos W0-W9 (lo lanza el orquestador con
# `pesado -p memorys -w 3600 -- bash .cursor/qa/d5b/probar-gpu.sh`); en Windows, bajo Git Bash, los pasos
# V0-V9 (lo lanza Leandro con PROBAR-GPU.cmd). Lo decide $OSTYPE, no probar si existe un comando del otro.
#
# Cada paso anota OK o FALLA con su motivo y todo va a un log con fecha; al final imprime el bloque
# `RESULTADO D5b`, que es lo que se pega en el traspaso. Sale 0 solo si ningún paso salió FALLA.
#
# Lo que NO toca nunca: el daemon, ~/.cache/memory-industry ni ~/.cache/cuba-memorys (solo se leen los modelos
# y, en Windows, el runtime 1.26 de la caché vieja y las DLL de cuda12-runtime), el PATH del sistema y los
# servicios. Los runtimes de prueba viven en una carpeta de prueba: ~/.cache/d5b-prueba (WSL) o
# prueba/ junto a este guion (Windows). La red: GitHub (paquetes), crates.io (compilar), PyPI (ruedas de CUDA, WSL)
# y NuGet (solo `dml`).
#
#   probar-gpu.sh          los pasos del sistema
#   probar-gpu.sh dml      además V8, DirectML 1.24.4 (solo Windows; opcional, sin veredicto de producto)
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "${OSTYPE:-}" in
  msys* | cygwin*) WIN=1 EXE=.exe ;;
  *) WIN=0 EXE= ;;
esac

# En Windows el árbol lo deja el orquestador en src/ (git archive del SHA final) junto a este guion.
if [[ -d "$HERE/src/rust" ]]; then
  ROOT="$HERE/src"
else
  ROOT="$(cd "$HERE/../../.." && pwd)"
fi
RUST="$ROOT/rust"

if ((WIN)); then
  T="$HERE/prueba"
  LOGDIR="$HERE/logs"
  export CARGO_TARGET_DIR="$HERE/target"
  TARGET="$CARGO_TARGET_DIR"
else
  T="$HOME/.cache/d5b-prueba"
  LOGDIR="$T"
  TARGET="${CARGO_TARGET_DIR:-$RUST/target}"
fi
mkdir -p "$T" "$LOGDIR"
LOG="$LOGDIR/log-$(date +%Y%m%d-%H%M%S).txt"
BIN="$TARGET/release/memory-industry$EXE"
BENCH="$TARGET/release/examples/rerank_bench$EXE"
SHA="$(cat "$HERE/SHA.txt" 2>/dev/null || git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo desconocido)"

# Telemetría de ONNX Runtime apagada en todo lo que arranca esta prueba: los ejemplos y los tests no pasan por main.
export ORT_DISABLE_TELEMETRY=1
# cargo is also called from outside the tree (--manifest-path): without a default toolchain rustup cannot choose one.
RUSTUP_TOOLCHAIN="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$ROOT/rust-toolchain.toml" 2>/dev/null | head -1)"
[[ -n "$RUSTUP_TOOLCHAIN" ]] && export RUSTUP_TOOLCHAIN || unset RUSTUP_TOOLCHAIN
# doctor opens the database before any check: it gets a throwaway one (the judge's container), never the user's.
if [[ -z "${GPU_QA_DATABASE_URL:-}" && -r "$HERE/../juez-win-031/base-del-juez.env" ]]; then
  GPU_QA_DATABASE_URL="$(sed -n 's/^DATABASE_URL=//p' "$HERE/../juez-win-031/base-del-juez.env" | head -1)"
fi
case "${GPU_QA_DATABASE_URL:-}" in
  *:5488/* | *:5488) echo "GUARDA: GPU_QA_DATABASE_URL apunta al puerto 5488 (la base del usuario)" >&2; exit 2 ;;
esac
if [[ -n "${GPU_QA_DATABASE_URL:-}" ]]; then
  DBENV=("DATABASE_URL=$GPU_QA_DATABASE_URL" MEMORY_INDUSTRY_REQUIRE_DATABASE_URL=1)
else
  DBENV=(-u DATABASE_URL)
fi

# ----------------------------------------------------------------------------- registro de pasos
R_ID=() R_STATE=() R_WHY=()
log() { printf '%s\n' "$*" | tee -a "$LOG"; }
note() {
  R_ID+=("$1") R_STATE+=("$2") R_WHY+=("$3")
  log "[$2] $1: $3"
}
ok() { note "$1" OK "$2"; }
fail() { note "$1" FALLA "$2"; }

# Un comando cuya salida va al log y cuyo código se devuelve.
logged() { "$@" >>"$LOG" 2>&1; }

# runenv VAR=valor ... [-u VAR ...] -- comando args: el comando con esas variables cambiadas.
runenv() {
  local -a vars=()
  while [[ "${1:-}" != -- ]]; do
    vars+=("$1")
    shift
  done
  shift
  # env wants its options before the assignments: `env A=1 -u B cmd` takes `-u` for the command (Git Bash, 2026-11-11).
  local -a opts=() sets=()
  local i=0
  while ((i < ${#vars[@]})); do
    if [[ "${vars[i]}" == -u ]]; then
      opts+=(-u "${vars[i + 1]}")
      i=$((i + 2))
    else
      sets+=("${vars[i]}")
      i=$((i + 1))
    fi
  done
  env "${opts[@]}" "${sets[@]}" "$@"
}

# La ruta tal como la ve un programa nativo: en Windows, la de Windows.
nat() { if ((WIN)); then cygpath -w "$1"; else printf '%s' "$1"; fi; }

# Directorios de bibliotecas de CUDA de una carpeta de ruedas de pip (WSL), separados por ':'.
cu_dirs() { find "$1" -type d -name lib -path '*/nvidia/*' 2>/dev/null | sort | paste -sd: -; }

# PATH sin /usr/lib/wsl/lib (donde WSL deja nvidia-smi): el binario tiene que encontrar la tarjeta él solo.
path_without_wsl() {
  local out="" d
  local -a parts
  IFS=: read -ra parts <<<"$PATH"
  for d in "${parts[@]}"; do
    [[ "$d" == /usr/lib/wsl/lib ]] || out="${out:+$out:}$d"
  done
  printf '%s' "$out"
}

# ----------------------------------------------------------------------------- los modelos de la caché real (solo lectura)
find_models() {
  local home_dir root
  home_dir="${USERPROFILE:-$HOME}"
  if ((WIN)); then home_dir="$(cygpath -u "$home_dir")"; fi
  for root in "$home_dir/.cache/memory-industry" "$home_dir/.cache/cuba-memorys"; do
    if [[ -f "$root/reranker/model.onnx" && -f "$root/models/model_quantized.onnx" ]]; then
      MODELS="$root"
      return 0
    fi
  done
  MODELS=""
  return 1
}

models_env() {
  # Las variables que el juez exporta (gate_model_env), apuntando a la caché real y sin escribir en ella.
  printf '%s\n' "ONNX_MODEL_PATH=$(nat "$MODELS/models")" \
    "CUBA_RERANKER_PATH=$(nat "$MODELS/reranker")" \
    "CUBA_NLI_PATH=$(nat "$MODELS/models-nli")" \
    "CUBA_EMBEDDING_DIM=384" "CUBA_EMBED_MODEL=e5-small" "CUBA_POOLING=cls"
}

# ----------------------------------------------------------------------------- el comparador (no es código del producto)
cat >"$T/comparar.py" <<'PY'
import sys as _s; _s.stdout.reconfigure(encoding="utf-8", errors="replace")
import re, sys

def ranking(path):
    out, on = [], False
    for line in open(path, encoding="utf-8", errors="replace"):
        if line.startswith("top-8 ranking"):
            on = True
            continue
        if on:
            m = re.match(r"\s+(\d+):(-?[0-9.eE+-]+)\s*$", line)
            if not m:
                break
            out.append((int(m.group(1)), float(m.group(2))))
    return out

def main():
    a_path, b_path, tol = sys.argv[1], sys.argv[2], float(sys.argv[3])
    a, b = ranking(a_path), ranking(b_path)
    if len(a) != 8 or len(b) != 8:
        print(f"falta el bloque 'top-8 ranking': {a_path} tiene {len(a)} filas y {b_path} tiene {len(b)}")
        return 1
    problems = []
    sb = dict(b)
    for idx, score in a:
        if idx in sb and abs(score - sb[idx]) > tol:
            problems.append(f"índice {idx}: {score:.6f} contra {sb[idx]:.6f} (|Δ| {abs(score - sb[idx]):.2e} > {tol:g})")
    sa = dict(a)
    floor_a, floor_b = min(s for _, s in a), min(s for _, s in b)
    for idx, score in b:
        if idx not in sa and abs(score - floor_a) > tol:
            problems.append(f"índice {idx} solo está en {b_path} con {score:.6f}; el octavo de {a_path} vale {floor_a:.6f}")
    for idx, score in a:
        if idx not in sb and abs(score - floor_b) > tol:
            problems.append(f"índice {idx} solo está en {a_path} con {score:.6f}; el octavo de {b_path} vale {floor_b:.6f}")
    # El orden: pares contiguos de a que difieren menos que tol son un solo grupo, y b no puede volver atrás de un grupo a otro.
    group, last, g = {}, None, 0
    for idx, score in a:
        if last is not None and last - score >= tol:
            g += 1
        group[idx] = g
        last = score
    seen = [group[idx] for idx, _ in b if idx in group]
    if seen != sorted(seen):
        problems.append(f"el orden cambia: {[i for i, _ in a]} contra {[i for i, _ in b]}")
    if problems:
        print("\n".join(problems))
        return 1
    print(f"mismo orden y |Δ| ≤ {tol:g} en el top-8")
    return 0

sys.exit(main())
PY

cat >"$T/nli.py" <<'PY'
import sys as _s; _s.stdout.reconfigure(encoding="utf-8", errors="replace")
import re, sys

def rows(path):
    out = []
    for line in open(path, encoding="utf-8", errors="replace"):
        m = re.match(r"\[(\d\.\d+) (\d\.\d+) (\d\.\d+)\] (?:->|~\?) (\S+)", line)
        if m:
            out.append((m.group(4), [float(m.group(i)) for i in (1, 2, 3)]))
    return out

a, b, tol = rows(sys.argv[1]), rows(sys.argv[2]), float(sys.argv[3])
if not a or len(a) != len(b):
    print(f"filas de NLI: {len(a)} contra {len(b)}")
    sys.exit(1)
bad = [f"fila {i}: {x[0]} {x[1]} contra {y[0]} {y[1]}" for i, (x, y) in enumerate(zip(a, b))
       if x[0] != y[0] or any(abs(p - q) > tol for p, q in zip(x[1], y[1]))]
print("\n".join(bad) if bad else f"{len(a)} filas con la misma etiqueta y |Δp| ≤ {tol:g}")
sys.exit(1 if bad else 0)
PY

cat >"$T/doctor.py" <<'PY'
import sys as _s; _s.stdout.reconfigure(encoding="utf-8", errors="replace")
import json, sys

text = open(sys.argv[1], encoding="utf-8", errors="replace").read()
# The report is the one line that parses as JSON and carries "checks": its first key is not fixed (2026-11-11, Windows).
report = None
for l in text.splitlines():
    if l.startswith("{") and '"checks"' in l:
        try:
            report = json.loads(l)
        except ValueError:
            continue
        if isinstance(report.get("checks"), list):
            break
        report = None
if report is None:
    print("")
    sys.exit(0)
checks = {c["name"]: c for c in report["checks"]}
print((checks.get(sys.argv[2]) or {}).get(sys.argv[3]) or "")
PY

field() { python3 -I "$T/doctor.py" "$@"; }

# ----------------------------------------------------------------------------- bloque final
finish() {
  local worst=0 i system
  if ((WIN)); then system=Windows; else system="Ubuntu/WSL"; fi
  # Fuera del pipe: un `worst=1` dentro de `{ …; } | tee` se pierde con el subshell y el guion saldría 0 con pasos en FALLA.
  for i in "${!R_ID[@]}"; do
    [[ "${R_STATE[$i]}" == OK ]] || worst=1
  done
  {
    echo
    echo "=== RESULTADO D5b ($system, SHA $SHA) ==="
    for i in "${!R_ID[@]}"; do
      printf '%-6s %-8s %s\n' "${R_STATE[$i]}" "${R_ID[$i]}" "${R_WHY[$i]}"
    done
    echo "log: $LOG"
    echo "=== fin ==="
  } | tee -a "$LOG"
  exit "$worst"
}

log "CI-D5b probar-gpu.sh — $(date -Is) — SHA $SHA — árbol $ROOT — prueba $T"

# ----------------------------------------------------------------------------- 0. la tarjeta
if ((WIN)); then id0=V0; else id0=W0; fi
SMI="$(command -v nvidia-smi 2>/dev/null || true)"
if [[ -z "$SMI" && $WIN -eq 0 ]]; then SMI=/usr/lib/wsl/lib/nvidia-smi; fi
if [[ -z "$SMI" || ! -e "$SMI" ]]; then
  fail "$id0" "no encuentro nvidia-smi: sin tarjeta visible no hay nada que probar"
  finish
fi
card="$("$SMI" --query-gpu=name,driver_version,compute_cap,memory.total,memory.free --format=csv,noheader,nounits 2>&1 | head -1)"
free_mb="$(awk -F', *' '{print $5}' <<<"$card")"
if [[ "$free_mb" =~ ^[0-9]+$ ]] && ((free_mb >= 5000)); then
  ok "$id0" "tarjeta: $card (MiB libres ≥ 5000; el reranker pide ~4300)"
else
  fail "$id0" "tarjeta ilegible o con menos de 5 GB libres: $card"
  finish
fi
if ((WIN)); then
  log "  rustc: $(rustc --version 2>&1 | head -1); CUDA_PATH=${CUDA_PATH:-<sin definir>}"
  if [[ -z "${CUDA_PATH:-}" ]]; then
    fail V0 "CUDA_PATH no está definida: no sé de dónde sacar las DLL de CUDA"
    finish
  fi
  if ! command -v cargo >/dev/null; then
    fail V0 "no encuentro cargo"
    finish
  fi
fi

if ! find_models; then
  fail "$id0" "no encuentro los modelos (reranker/model.onnx y models/model_quantized.onnx) en la caché de memory-industry ni en la vieja: corre \`memory-industry models all\`"
  finish
fi
log "  modelos en: $MODELS"
mapfile -t MODELS_ENV < <(models_env)

# ----------------------------------------------------------------------------- 1. compilar
if ((WIN)); then
  id1=V1 features="docs,cuda,directml"
else
  id1=W1 features="docs,cuda"
fi
if (cd "$RUST" && logged cargo build --release --features "$features" --bin memory-industry --example rerank_bench) \
  && (cd "$RUST" && logged cargo test --release --features "$features" --test v017_rerank_gpu --test nli_probe --no-run) \
  && [[ -x "$BIN" && -x "$BENCH" ]]; then
  ok "$id1" "compila (--features $features): memory-industry y rerank_bench"
else
  fail "$id1" "no compila o faltan $BIN y $BENCH; mira el log"
  finish
fi

# ----------------------------------------------------------------------------- 2-3. models runtime --gpu
# models_runtime <carpeta-home> <argumentos...>: la salida va al log y a <carpeta>.out; devuelve el código de salida.
models_runtime() {
  local home_dir="$1" out="$1.out"
  shift
  mkdir -p "$home_dir"
  runenv HOME="$(nat "$home_dir")" USERPROFILE="$(nat "$home_dir")" PATH="$(path_without_wsl)" -- "$BIN" models runtime "$@" >"$out" 2>&1
  local rc=$?
  cat "$out" >>"$LOG"
  return $rc
}
record_of() { cat "$1/.cache/memory-industry/onnxruntime/runtime.json" 2>/dev/null; }
runtime_lib() {
  local name=libonnxruntime.so
  if ((WIN)); then name=onnxruntime.dll; fi
  printf '%s' "$1/.cache/memory-industry/onnxruntime/$name"
}

H13="$T/g13"
H12="$T/g12"
if ((WIN)); then idA=V2 idB=V6; else idA=W2 idB=W3; fi

rm -rf "${H13:?}" "${H13:?}.out"
if models_runtime "$H13" --gpu && [[ "$(record_of "$H13")" == *'"flavor":"cuda13"'* && "$(record_of "$H13")" == *'"version":"1.31.1"'* ]] \
  && grep -q "checksum" "$H13.out" && ! grep -q "no veo driver" "$H13.out"; then
  ok "$idA" "models runtime --gpu eligió cuda13 con la suma comprobada y la tarjeta leída: $(record_of "$H13")"
else
  fail "$idA" "models runtime --gpu no dejó cuda13 1.31.1 con la suma comprobada y la tarjeta leída (¿dice «no veo driver»?): $(tail -3 "$H13.out" | tr '\n' ' ')"
fi
if ((WIN)); then
  home_has="$(ls "$H13/.cache/memory-industry/onnxruntime" 2>/dev/null | tr '\n' ' ')"
  if [[ "$home_has" == *onnxruntime.dll* && "$home_has" == *onnxruntime_providers_cuda.dll* && "$home_has" == *onnxruntime_providers_shared.dll* && "$home_has" == *runtime.json* ]]; then
    ok V2b "la carpeta tiene onnxruntime.dll, los dos proveedores y runtime.json"
  else
    fail V2b "la carpeta de V2 tiene: $home_has"
  fi
fi

rm -rf "${H12:?}" "${H12:?}.out"
if models_runtime "$H12" --gpu --cuda 12 && [[ "$(record_of "$H12")" == *'"flavor":"cuda12"'* ]] && grep -q "checksum" "$H12.out"; then
  ok "$idB" "models runtime --gpu --cuda 12: cuda12 con la suma comprobada"
else
  fail "$idB" "--cuda 12 no dejó cuda12 con la suma comprobada: $(tail -3 "$H12.out" | tr '\n' ' ')"
fi

# ----------------------------------------------------------------------------- 4. las bibliotecas de CUDA de usuario
if ((WIN)); then
  CU13="$(cygpath -u "$CUDA_PATH")/bin/x64"
  CU12="$(cygpath -u "${USERPROFILE}")/.cache/cuda12-runtime"
  if [[ ! -d "$CU13" ]]; then CU13="$(cygpath -u "$CUDA_PATH")/bin"; fi
  if [[ -d "$CU13" && -d "$CU12" ]]; then
    ok V3pre "CUDA 13 en $CU13 (sin cuDNN) y CUDA 12 en $CU12 (solo lectura)"
  else
    fail V3pre "faltan $CU13 o $CU12"
  fi
else
  CU13_ROOT="${D5B_CU13:-$T/cu13}"
  CU12_ROOT="${D5B_CU12:-$T/cu12}"
  log "  (antes de instalar ruedas: socket depscore de nvidia-cuda-runtime, nvidia-cublas, nvidia-curand y sus -cu12; D5B_CU13 / D5B_CU12 apuntan a carpetas ya instaladas)"
  if [[ -z "$(cu_dirs "$CU13_ROOT")" ]]; then
    logged python3 -m pip install --quiet --target "$CU13_ROOT" nvidia-cuda-runtime nvidia-cublas nvidia-curand
  fi
  if [[ -z "$(cu_dirs "$CU12_ROOT")" ]]; then
    logged python3 -m pip install --quiet --target "$CU12_ROOT" nvidia-cuda-runtime-cu12 nvidia-cublas-cu12 nvidia-curand-cu12
  fi
  CU13="$(cu_dirs "$CU13_ROOT")"
  CU12="$(cu_dirs "$CU12_ROOT")"
  if find "$CU13_ROOT" -name 'libcublasLt.so.13*' | grep -q . && find "$CU12_ROOT" -name 'libcublasLt.so.12*' | grep -q . \
    && ! find "$CU13_ROOT" "$CU12_ROOT" -name 'libcudnn*' | grep -q .; then
    ok W4 "CUDA de usuario sin cuDNN; ruedas 13: $(find "$CU13_ROOT" -maxdepth 1 -name '*.dist-info' -printf '%f ' | tr -d '\n'); ruedas 12: $(find "$CU12_ROOT" -maxdepth 1 -name '*.dist-info' -printf '%f ' | tr -d '\n')"
  else
    fail W4 "las ruedas no dejaron cuBLASLt 13 y 12, o dejaron cuDNN (no debe estar): $CU13_ROOT $CU12_ROOT"
  fi
fi

# with_cuda <dirs> VAR=valor ... -- comando: el comando con las bibliotecas de CUDA en la ruta del cargador.
with_cuda() {
  local dirs="$1"
  shift
  if ((WIN)); then
    runenv "PATH=$dirs:$PATH" "$@"
  else
    runenv "LD_LIBRARY_PATH=$dirs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" "$@"
  fi
}

# doctor_of <home> <dirs-de-cuda> <salida.json>: doctor --json --deep, sin base de datos (los chequeos de base fallan y el JSON sale igual).
doctor_of() {
  local home_dir="$1" dirs="$2" out="$3"
  with_cuda "$dirs" HOME="$(nat "$home_dir")" USERPROFILE="$(nat "$home_dir")" \
    ORT_DYLIB_PATH="$(nat "$(runtime_lib "$home_dir")")" "${MODELS_ENV[@]}" "${DBENV[@]}" -- \
    "$BIN" doctor --json --deep >"$out" 2>>"$LOG"
  true
}
placement_of() { runenv HOME="$(nat "$1")" USERPROFILE="$(nat "$1")" -- bash "$ROOT/scripts/gpu-placement-check.sh" "$2" >>"$LOG" 2>&1; }

# ----------------------------------------------------------------------------- 5. doctor y la colocación
if ((WIN)); then idD=V3; else idD=W5; fi
DIRS13="$CU13"
doctor_of "$H13" "$DIRS13" "$T/doctor-g13.json"
gpu_status="$(field "$T/doctor-g13.json" gpu status)"
gpu_detail="$(field "$T/doctor-g13.json" gpu detail)"
rt_status="$(field "$T/doctor-g13.json" onnx_runtime status)"
rt_detail="$(field "$T/doctor-g13.json" onnx_runtime detail)"
placement_of "$H13" "$T/doctor-g13.json"
rc=$?
if [[ "$gpu_status" == ok && "$gpu_detail" == *"reranker=gpu"* && "$rt_status" == ok && "$rt_detail" == *"1.31.1"* && "$rt_detail" == *cuda13* && $rc -eq 0 ]]; then
  ok "$idD" "doctor: gpu ok («$gpu_detail»), onnx_runtime ok («$rt_detail»); gpu-placement-check sale 0"
else
  fail "$idD" "doctor gpu=$gpu_status «$gpu_detail», onnx_runtime=$rt_status «$rt_detail», gpu-placement-check=$rc"
fi

if ((WIN)); then
  doctor_of "$H12" "$CU12" "$T/doctor-g12.json"
  s12="$(field "$T/doctor-g12.json" gpu status)"
  d12="$(field "$T/doctor-g12.json" gpu detail)"
  r12="$(field "$T/doctor-g12.json" onnx_runtime detail)"
  if [[ "$s12" == ok && "$d12" == *"reranker=gpu"* && "$r12" == *cuda12* ]]; then
    ok V6b "CUDA 12 con cuda12-runtime: gpu ok («$d12»), onnx_runtime «$r12»"
  else
    fail V6b "CUDA 12: gpu=$s12 «$d12», onnx_runtime «$r12»"
  fi
else
  # Sabotaje S12: el cargador sin el directorio de cuBLAS.
  NO_CUBLAS="$(tr ':' '\n' <<<"$CU13" | grep -v cublas | paste -sd: -)"
  doctor_of "$H13" "$NO_CUBLAS" "$T/doctor-g13-sin-cublas.json"
  sg="$(field "$T/doctor-g13-sin-cublas.json" gpu status)"
  sd="$(field "$T/doctor-g13-sin-cublas.json" gpu detail)"
  sh="$(field "$T/doctor-g13-sin-cublas.json" gpu hint)"
  placement_of "$H13" "$T/doctor-g13-sin-cublas.json"
  rcs=$?
  if [[ "$sg" == warn && "$sd$sh" == *cublas* && $rcs -eq 1 ]]; then
    ok W5s "S12 sin cuBLAS: gpu=$sg nombrando la biblioteca que falta («${sd:0:160}…»); gpu-placement-check sale 1"
  else
    fail W5s "S12 sin cuBLAS esperaba gpu warn nombrando cuBLAS y gpu-placement-check 1; vi gpu=$sg «${sd:0:160}» y $rcs"
  fi
fi

# ----------------------------------------------------------------------------- 6. el test del juez con tarjeta
if ((WIN)); then idT=V4; else idT=W6; fi
used_before="$("$SMI" --query-gpu=memory.used --format=csv,noheader,nounits 2>/dev/null | head -1)"
: >"$T/smi-muestras.txt"
(while :; do "$SMI" --query-gpu=memory.used --format=csv,noheader,nounits 2>/dev/null | head -1 >>"$T/smi-muestras.txt"; sleep 1; done) &
sampler=$!
with_cuda "$DIRS13" HOME="$(nat "$H13")" USERPROFILE="$(nat "$H13")" ORT_DYLIB_PATH="$(nat "$(runtime_lib "$H13")")" "${MODELS_ENV[@]}" -- \
  cargo test --manifest-path "$RUST/Cargo.toml" --release --features "$features" --test v017_rerank_gpu -- --ignored --nocapture >>"$LOG" 2>&1
rc6=$?
kill "$sampler" 2>/dev/null
wait "$sampler" 2>/dev/null
used_max="$(sort -n "$T/smi-muestras.txt" | tail -1)"
if [[ $rc6 -eq 0 ]]; then
  ok "$idT" "v017_rerank_gpu --ignored pasa con CUDA 13 y la tarjeta"
else
  fail "$idT" "v017_rerank_gpu --ignored salió $rc6; mira el log"
fi
if [[ "$used_before" =~ ^[0-9]+$ && "$used_max" =~ ^[0-9]+$ ]] && ((used_max - used_before >= 1000)); then
  ok "${idT}-smi" "la memoria de la tarjeta subió de $used_before a $used_max MiB mientras corría: el trabajo está en la tarjeta"
else
  fail "${idT}-smi" "la memoria de la tarjeta no subió ≥ 1000 MiB mientras corría (antes $used_before, máximo $used_max)"
fi

# ----------------------------------------------------------------------------- 7. el reranker, CPU y GPU, entre versiones
# bench <nombre> <dirs-de-cuda|-> VAR=valor ...: rerank_bench 8 1 y 50 3 con esas variables.
bench() {
  local name="$1" dirs="$2"
  shift 2
  local -a vars=("$@" "${MODELS_ENV[@]}")
  if [[ "$dirs" == - ]]; then
    runenv "${vars[@]}" -- "$BENCH" 8 1 >"$T/bench-$name-8.txt" 2>>"$LOG"
    runenv "${vars[@]}" -- "$BENCH" 50 3 >"$T/bench-$name-50.txt" 2>>"$LOG"
  else
    with_cuda "$dirs" "${vars[@]}" -- "$BENCH" 8 1 >"$T/bench-$name-8.txt" 2>>"$LOG"
    with_cuda "$dirs" "${vars[@]}" -- "$BENCH" 50 3 >"$T/bench-$name-50.txt" 2>>"$LOG"
  fi
  log "  $name: $(grep -h 'mean' "$T/bench-$name-50.txt" | tail -1)"
}
compare() { # <id> <a> <b> <tolerancia> <qué compara>
  local out
  if out="$(python3 -I "$T/comparar.py" "$T/bench-$2-8.txt" "$T/bench-$3-8.txt" "$4" 2>&1)"; then
    ok "$1" "$5: $out"
  else
    fail "$1" "$5: $out"
  fi
}

if ((WIN)); then idC=V5; else idC=W7; fi
OLD_CPU="$T/ort-1.26.0"
mkdir -p "$T/h-neutro"
if ((WIN)); then
  OLD_GPU_LIB="$(cygpath -u "$USERPROFILE")/.cache/cuba-memorys/onnxruntime/onnxruntime.dll"
  OLD_GPU_DIRS="$CU12"
else
  OLD_GPU_DIR="$T/ort-1.26.0-cuda13"
  OLD_GPU_LIB="$OLD_GPU_DIR/libonnxruntime.so"
  OLD_GPU_DIRS="$CU13"
  if [[ ! -e "$OLD_GPU_LIB" ]]; then
    # El paquete de 1.26.0 con CUDA 13, para la comparación entre versiones: su suma sale de la API de GitHub, no de este guion.
    mkdir -p "$OLD_GPU_DIR"
    asset="onnxruntime-linux-x64-gpu_cuda13-1.26.0.tgz"
    want="$(curl -fsSL https://api.github.com/repos/microsoft/onnxruntime/releases/tags/v1.26.0 \
      | python3 -I -c "import json,sys; print(next(a['digest'] for a in json.load(sys.stdin)['assets'] if a['name']=='$asset'))" 2>>"$LOG")"
    if [[ "$want" == sha256:* ]] && logged curl -fsSL -o "$T/$asset" "https://github.com/microsoft/onnxruntime/releases/download/v1.26.0/$asset" \
      && [[ "sha256:$(sha256sum "$T/$asset" | cut -d' ' -f1)" == "$want" ]]; then
      tar -xzf "$T/$asset" -C "$OLD_GPU_DIR" --strip-components=2 --wildcards '*/lib/*' >>"$LOG" 2>&1
    else
      log "  no pude bajar o comprobar $asset (sale de la comparación GPU 1.26)"
    fi
    rm -f "${T:?}/${asset:?}"
  fi
fi

HAVE_OLD_CPU=0
if [[ -e "$OLD_CPU/libonnxruntime.so" || -e "$OLD_CPU/onnxruntime.dll" ]]; then HAVE_OLD_CPU=1; fi
if ((WIN)); then
  bench cpu131 - ORT_DYLIB_PATH="$(nat "$(runtime_lib "$H13")")" CUBA_RERANK_DEVICE=cpu HOME="$(nat "$H13")" USERPROFILE="$(nat "$H13")"
else
  if ((HAVE_OLD_CPU)); then
    bench cpu126 - ORT_DYLIB_PATH="$OLD_CPU/libonnxruntime.so" CUBA_RERANK_DEVICE=cpu HOME="$T/h-neutro"
  fi
  bench cpu131 - ORT_DYLIB_PATH="$(runtime_lib "$H13")" CUBA_RERANK_DEVICE=cpu HOME="$T/h-neutro"
fi
if [[ -e "$OLD_GPU_LIB" ]]; then
  bench gpu126 "$OLD_GPU_DIRS" ORT_DYLIB_PATH="$(nat "$OLD_GPU_LIB")" CUBA_RERANK_DEVICE=gpu HOME="$(nat "$T/h-neutro")" USERPROFILE="$(nat "$T/h-neutro")"
fi
bench gpu131c13 "$DIRS13" ORT_DYLIB_PATH="$(nat "$(runtime_lib "$H13")")" CUBA_RERANK_DEVICE=gpu HOME="$(nat "$H13")" USERPROFILE="$(nat "$H13")"
bench gpu131c12 "$CU12" ORT_DYLIB_PATH="$(nat "$(runtime_lib "$H12")")" CUBA_RERANK_DEVICE=gpu HOME="$(nat "$H12")" USERPROFILE="$(nat "$H12")"

if ((WIN)); then
  if [[ -e "$OLD_GPU_LIB" ]]; then
    compare "${idC}-a" gpu131c13 gpu126 0.005 "GPU 1.31.1 CUDA 13 contra GPU 1.26.0 CUDA 12 (su runtime actual, solo leído)"
  else
    fail "${idC}-a" "no hay runtime 1.26.0 en la caché vieja para comparar"
  fi
  compare "${idC}-b" cpu131 gpu131c13 0.005 "CPU 1.31.1 contra GPU 1.31.1 CUDA 13"
else
  if ((HAVE_OLD_CPU)); then
    compare "${idC}-a" cpu126 cpu131 0.0001 "CPU 1.26.0 contra CPU 1.31.1"
  else
    fail "${idC}-a" "falta $OLD_CPU (paso 3 de §11: copiar el runtime 1.26.0 del juez antes de actualizarlo)"
  fi
  compare "${idC}-b" cpu131 gpu131c13 0.005 "CPU 1.31.1 contra GPU 1.31.1 CUDA 13"
  if [[ -e "$OLD_GPU_LIB" ]]; then
    compare "${idC}-c" gpu126 gpu131c13 0.005 "GPU 1.26.0 contra GPU 1.31.1, los dos CUDA 13"
  else
    fail "${idC}-c" "no hay GPU 1.26.0 CUDA 13 para comparar"
  fi
fi
compare "${idC}-d" gpu131c13 gpu131c12 0.005 "GPU 1.31.1 CUDA 13 contra CUDA 12"

# Sabotaje S13: el comparador con la salida de otra consulta tiene que salir 1 y decir el índice.
runenv BENCH_UNIFORM=1 "${MODELS_ENV[@]}" ORT_DYLIB_PATH="$(nat "$(runtime_lib "$H13")")" CUBA_RERANK_DEVICE=cpu HOME="$(nat "$H13")" USERPROFILE="$(nat "$H13")" -- \
  "$BENCH" 8 1 >"$T/bench-otra-8.txt" 2>>"$LOG"
if out="$(python3 -I "$T/comparar.py" "$T/bench-cpu131-8.txt" "$T/bench-otra-8.txt" 0.005 2>&1)"; then
  fail "${idC}s" "S13: el comparador aceptó la salida de otra consulta: $out"
elif [[ "$out" == *índice* || "$out" == *orden* ]]; then
  ok "${idC}s" "S13: el comparador sale 1 con otra consulta y dice: ${out%%$'\n'*}"
else
  fail "${idC}s" "S13: salió 1 pero no dijo el índice: $out"
fi

# ----------------------------------------------------------------------------- 8. NLI en GPU (opcional)
if ((WIN)); then idN=V5n; else idN=W8; fi
nli_run() { # <dispositivo> <salida>
  with_cuda "$DIRS13" HOME="$(nat "$H13")" USERPROFILE="$(nat "$H13")" ORT_DYLIB_PATH="$(nat "$(runtime_lib "$H13")")" "${MODELS_ENV[@]}" CUBA_NLI_DEVICE="$1" -- \
    cargo test --manifest-path "$RUST/Cargo.toml" --release --features "$features" --test nli_probe -- --ignored --nocapture >"$2" 2>&1
}
if nli_run cpu "$T/nli-cpu.txt" && nli_run gpu "$T/nli-gpu.txt"; then
  if out="$(python3 -I "$T/nli.py" "$T/nli-cpu.txt" "$T/nli-gpu.txt" 0.01 2>&1)"; then
    ok "$idN" "NLI en GPU contra CPU (1.31.1): $out"
  else
    fail "$idN" "NLI en GPU contra CPU: $out"
  fi
else
  fail "$idN" "nli_probe no corrió con CPU o con GPU; mira $T/nli-cpu.txt y $T/nli-gpu.txt"
fi

# ----------------------------------------------------------------------------- Windows: actualización con el daemon vivo (V7) y DirectML (V8)
if ((WIN)); then
  # Un proceso con onnxruntime.dll cargada desde una copia de la carpeta de V2 hace de daemon vivo.
  HV7="$T/v7"
  rm -rf "${HV7:?}" "${HV7:?}.out"
  mkdir -p "$HV7"
  cp -r "$H13/.cache" "$HV7/.cache"
  runenv "${MODELS_ENV[@]}" ORT_DYLIB_PATH="$(nat "$(runtime_lib "$HV7")")" CUBA_RERANK_DEVICE=gpu HOME="$(nat "$HV7")" USERPROFILE="$(nat "$HV7")" \
    "PATH=$CU13:$PATH" -- "$BENCH" 50 30 >"$T/v7-bench.txt" 2>&1 &
  holder=$!
  sleep 20
  before="$(record_of "$HV7")"
  if models_runtime "$HV7" --gpu --cuda 12; then
    kill "$holder" 2>/dev/null
    # Since CI-D5c-A the swap moves the old directory aside and leaves it for the next run when a file is in use.
    if grep -q "no pude borrar el runtime anterior, en uso" "$HV7.out" && [[ "$(record_of "$HV7")" == *'"flavor":"cuda12"'* ]]; then
      ok V7 "con la DLL en uso el runtime nuevo quedó puesto y el anterior apartado para borrarlo en la siguiente corrida (lo dice)"
    else
      fail V7 "el reemplazo con la DLL en uso salió 0 sin decir que el anterior sigue en uso: $(tail -3 "$HV7.out" | tr '\n' ' ')"
    fi
  elif grep -q "en uso: para el daemon y vuelve a correr" "$HV7.out" \
    && [[ "$(record_of "$HV7")" == "$before" && -f "$HV7/.cache/memory-industry/onnxruntime.downloading/runtime.json" ]]; then
    kill "$holder" 2>/dev/null
    wait "$holder" 2>/dev/null
    sleep 2
    if models_runtime "$HV7" --gpu --cuda 12 && ! grep -q "descargando" "$HV7.out" && [[ "$(record_of "$HV7")" == *'"flavor":"cuda12"'* ]]; then
      ok V7 "en uso: la carpeta quedó intacta y la descarga completa; al soltar la DLL la repetición reemplazó sin volver a bajar"
    else
      fail V7 "la repetición tras soltar la DLL no reemplazó sin volver a bajar: $(tail -3 "$HV7.out" | tr '\n' ' ')"
    fi
  else
    kill "$holder" 2>/dev/null
    fail V7 "con la DLL en uso esperaba «en uso: para el daemon y vuelve a correr», la carpeta intacta y onnxruntime.downloading completa: $(tail -3 "$HV7.out" | tr '\n' ' ')"
  fi

  if [[ "${1:-}" == dml ]]; then
    HD="$T/dml"
    mkdir -p "$HD"
    nuget() { curl -fsSL -o "$2" "https://api.nuget.org/v3-flatcontainer/$1"; }
    if nuget "microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg" "$HD/ort.zip"; then
      python3 -I -c '
import sys, zipfile
d = sys.argv[1]
with zipfile.ZipFile(d + "/ort.zip") as z:
    for n in z.namelist():
        if n.endswith("runtimes/win-x64/native/onnxruntime.dll"):
            open(d + "/onnxruntime.dll", "wb").write(z.read(n))
' "$HD" >>"$LOG" 2>&1
      dmlv="$(curl -fsSL https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/index.json | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["versions"][-1])')"
      if nuget "microsoft.ai.directml/$dmlv/microsoft.ai.directml.$dmlv.nupkg" "$HD/dml.zip"; then
        python3 -I -c '
import sys, zipfile
d = sys.argv[1]
with zipfile.ZipFile(d + "/dml.zip") as z:
    for n in z.namelist():
        if n.endswith("bin/x64-win/DirectML.dll"):
            open(d + "/DirectML.dll", "wb").write(z.read(n))
' "$HD" >>"$LOG" 2>&1
      fi
      with_cuda "$CU13" HOME="$(nat "$H13")" USERPROFILE="$(nat "$H13")" ORT_DYLIB_PATH="$(nat "$HD/onnxruntime.dll")" CUBA_RERANK_DEVICE=gpu "${MODELS_ENV[@]}" "${DBENV[@]}" -- \
        "$BIN" doctor --json --deep >"$T/doctor-dml.json" 2>>"$LOG"
      ok V8 "DirectML 1.24.4 (se anota, sin veredicto de producto): onnx_runtime=$(field "$T/doctor-dml.json" onnx_runtime status) «$(field "$T/doctor-dml.json" onnx_runtime detail)»; pista «$(field "$T/doctor-dml.json" onnx_runtime hint)»; gpu «$(field "$T/doctor-dml.json" gpu detail)»"
    else
      fail V8 "no pude bajar el paquete de DirectML 1.24.4 de NuGet"
    fi
  fi
fi

# ----------------------------------------------------------------------------- 9. limpiar
if ((WIN)); then
  rm -rf "${T:?}"
  ok V9 "la carpeta de prueba se borró; queda el log en $LOGDIR"
else
  log "  W9: la carpeta $T se deja para reutilizar las descargas (~2,3 GB); se borra al cerrar la tajada (§11 paso 10)."
fi

finish
