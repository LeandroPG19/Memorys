#!/usr/bin/env bash
# Alias del SIL de MemoryIndustry. El juez es merge-gate.sh.
#   ./scripts/como-el-ci.sh          # todo ≡ merge-gate.sh
#   ./scripts/como-el-ci.sh todo
#   ./scripts/como-el-ci.sh extra    # ≡ todo (no hay extra más ligero)
#   ./scripts/como-el-ci.sh quality  # segundo juez: CRAP/mutación del diff
#   ./scripts/como-el-ci.sh unit|contracts|e2e|crap|mutants
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
QUE="${1:-todo}"

case "$QUE" in
  todo|all|extra|"")
    exec "$ROOT/scripts/merge-gate.sh"
    ;;
  quality)
    exec "$ROOT/scripts/quality-gate.sh"
    ;;
  unit|contracts|e2e|crap|mutants)
    exec "$ROOT/scripts/memory-industry-test.sh" "$QUE"
    ;;
  *)
    echo "usage: $0 [todo|extra|quality|unit|contracts|e2e|crap|mutants]" >&2
    echo "SIL (merge judge): todo|extra → ./scripts/merge-gate.sh" >&2
    echo "second judge:      quality    → ./scripts/quality-gate.sh" >&2
    exit 2
    ;;
esac
