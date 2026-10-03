#!/usr/bin/env bash
# Coverage floor for MemoryIndustry cores (CRAP proxy until per-fn CRAP tables land).
# Missing cargo-llvm-cov = FAIL (no soft-skip).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT/rust"

MIN_COV="${CRAP_MIN_LINE_COV:-15}"

if ! command -v cargo-llvm-cov >/dev/null 2>&1 && ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "FAIL: cargo-llvm-cov is required for crap-gate." >&2
  echo "      cargo install cargo-llvm-cov && rustup component add llvm-tools-preview" >&2
  exit 1
fi

echo "=== crap-gate: llvm-cov --lib (min line ${MIN_COV}%) ==="
COV_JSON="${TMPDIR:-/tmp}/memory-industry-cov.json"
cargo llvm-cov --lib --json --summary-only --output-path "$COV_JSON"

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
MEMORY_INDUSTRY_COV_JSON="$COV_JSON" "$PY" - <<'PY'
import json, os, sys
min_cov = float(os.environ.get("CRAP_MIN_LINE_COV", "15"))
path = os.environ["MEMORY_INDUSTRY_COV_JSON"]
data = json.load(open(path, encoding="utf-8"))
# Prefer totals.lines.percent — a naive walk hits per-file zeros first.
try:
    pct = float(data["data"][0]["totals"]["lines"]["percent"])
except Exception:
    print("FAIL: could not parse llvm-cov JSON for totals.lines.percent", file=sys.stderr)
    sys.exit(1)
print(f"line_coverage={pct:.2f}% min={min_cov}")
if pct + 1e-9 < min_cov:
    print(f"FAIL: line coverage {pct:.2f}% < {min_cov}%", file=sys.stderr)
    sys.exit(1)
print("OK  crap-gate coverage floor")
PY