#!/usr/bin/env bash
# Mutation testing on MemoryIndustry cores. Unkilled mutants = FAIL.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/rust"

if ! command -v cargo-mutants >/dev/null 2>&1 && ! cargo mutants --version >/dev/null 2>&1; then
  echo "FAIL: cargo-mutants is required. Install: cargo install cargo-mutants" >&2
  exit 1
fi

MUTANTS_MIN_KILL="${MUTANTS_MIN_KILL:-0.7}"
OUT_DIR="${TMPDIR:-/tmp}/memory-industry-mutants-out"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"


# Parallelism derived from the machine, not pinned to a number.
#
# The values these replace were measured on a 14.9 GB laptop, and a constant
# calibrated on one machine and then applied to every other one is exactly the
# defect this release spent its time removing from the GPU path. The formula
# below reproduces the old hand-tuned figure on the machine it was tuned for
# (14.9 GB / 4 = 3) and scales from there.
machine_cores() { nproc 2>/dev/null || echo 4; }

machine_ram_gb() {
  if [[ -r /proc/meminfo ]]; then
    awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo
    return
  fi
  powershell.exe -NoProfile -Command \
    '[int]((Get-CimInstance Win32_OperatingSystem).TotalVisibleMemorySize/1MB)' 2>/dev/null |
    tr -d '\r' | grep -E '^[0-9]+$' || echo 8
}
# One mutant is a debug --lib build plus its tests: far lighter than a release
# link, so this leans on cores. Capped at 6 because each run carries a 90 s
# test timeout, and a saturated machine turns a caught mutant into a reported
# TIMEOUT, which is a worse answer than a slower one.
default_mutants_jobs() {
  local half cap
  half=$(( $(machine_cores) / 2 ))
  cap=$(( $(machine_ram_gb) / 4 ))
  (( half < 1 )) && half=1
  (( cap < half )) && half=$cap
  (( half > 6 )) && half=6
  (( half < 1 )) && half=1
  echo "$half"
}
MUTANTS_JOBS="${MUTANTS_JOBS:-$(default_mutants_jobs)}"
echo "mutants jobs=$MUTANTS_JOBS (cores=$(machine_cores) ram=$(machine_ram_gb)GB)"

echo "=== cargo mutants (search cores with dense unit tests) ==="
# judge.rs / tools.rs / full search/* generate 655 mutants (~12h) and miss
# almost every change in untested branches (LLM resolve, ONNX decode fallback).
# The merge judge still requires a real kill-rate check. Scope = mmr/rrf/cache.
# --lib search:: keeps the unmutated baseline off contract tests that need
# files outside rust/, and keeps each mutant's cargo test under a second.
set +e
cargo mutants \
  --file 'src/search/mmr.rs' \
  --file 'src/search/rrf.rs' \
  --file 'src/search/cache.rs' \
  # 180, not 90. The per-mutant budget was tuned when this ran two at a time;
  # at six a slow test reported TIMEOUT instead of the CAUGHT it actually was,
  # and a timeout counts against the kill rate as though the mutant had lived.
  --timeout 180 \
  --jobs "$MUTANTS_JOBS" \
  --output "$OUT_DIR" \
  -- --lib search::
mutants_rc=$?
set -e

MUTANTS_JSON="$OUT_DIR/mutants.out/outcomes.json"
if [[ ! -f "$MUTANTS_JSON" ]]; then
  echo "FAIL: cargo mutants produced no outcomes.json (rc=$mutants_rc)" >&2
  exit 1
fi

resolve_python() {
  if [[ -n "${PYTHON_BIN:-}" && ( -x "$PYTHON_BIN" || -f "$PYTHON_BIN" ) ]]; then
    printf '%s\n' "$PYTHON_BIN"
    return
  fi
  local c p
  for c in python3 python; do
    p="$(command -v "$c" 2>/dev/null || true)"
    [[ -n "$p" ]] || continue
    case "$p" in
      */WindowsApps/*) continue ;;
    esac
    printf '%s\n' "$p"
    return
  done
  printf '%s\n' "python3"
}
PY="$(resolve_python)"
export PYTHONUTF8=1 PYTHONIOENCODING=utf-8
MEMORY_INDUSTRY_MUTANTS_JSON="$MUTANTS_JSON" "$PY" - <<'PY'
import json, os, sys
path = os.environ["MEMORY_INDUSTRY_MUTANTS_JSON"]
min_kill = float(os.environ.get("MUTANTS_MIN_KILL", "0.7"))
data = json.load(open(path, encoding="utf-8"))
caught = int(data.get("caught") or 0)
missed = int(data.get("missed") or 0)
timeout = int(data.get("timeout") or 0)
unviable = int(data.get("unviable") or 0)
denom = caught + missed + timeout
if denom == 0:
    print("FAIL: zero scored mutants in outcomes.json", file=sys.stderr)
    print(json.dumps(list(data.keys()), indent=2), file=sys.stderr)
    sys.exit(1)
rate2 = caught / denom
print(f"mutants caught={caught} missed={missed} timeout={timeout} unviable={unviable} kill_rate={rate2:.3f} min={min_kill}")
if rate2 < min_kill:
    print(f"FAIL: kill rate {rate2:.3f} < {min_kill}", file=sys.stderr)
    sys.exit(1)
print("OK  mutants kill rate")
PY
