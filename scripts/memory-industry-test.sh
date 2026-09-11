#!/usr/bin/env bash
# Unified test CLI for MemoryIndustry — same judge as merge-gate (no second CI).
# Usage: scripts/memory-industry-test.sh [unit|contracts|e2e|crap|mutants|all]
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CMD="${1:-all}"

case "$CMD" in
  all)
    exec "$ROOT/scripts/merge-gate.sh"
    ;;
  unit)
    cd "$ROOT/rust"
    cargo test --lib
    ;;
  contracts)
    cd "$ROOT/rust"
    cargo test --test local_gate_contract --test cli_contract --test doc_contract --test migrations_contract
    ;;
  e2e)
    cd "$ROOT/rust"
    export CUBA_BINARY_PATH="${CUBA_BINARY_PATH:-$ROOT/rust/target/release/memory-industry}"
    python3 tests/e2e_all_tools.py
    python3 "$ROOT/scripts/mcp_live_session_test.py"
    ;;
  crap)
    exec "$ROOT/scripts/crap-gate.sh"
    ;;
  mutants)
    exec "$ROOT/scripts/mutants-gate.sh"
    ;;
  *)
    echo "usage: $0 [unit|contracts|e2e|crap|mutants|all]" >&2
    exit 2
    ;;
esac
