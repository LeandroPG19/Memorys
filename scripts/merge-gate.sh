#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# One gate at a time, for the whole of this one: run-all-tests.sh is only its
# first half, and everything after it (clippy and tests under --features docs,
# deny, audit, codigo-muerto, crap-gate, mutants-gate) builds in the same target
# directory under the same cargo lock. Taken before anything else happens, so
# a second gate is refused before it has checked, backed up or built anything.
# run-all-tests.sh, launched below, is this gate's own child and inherits the
# lock through CUBA_GATE_LOCK_OWNER instead of refusing it.
#   ./scripts/run-all-tests.sh --self-test   (its fixtures drive this script too)
# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"
acquire_gate_lock "$GATE_LOCK" || exit 1
trap release_gate_lock EXIT
export CUBA_GATE_LOCK_OWNER="$GATE_OWNER"

echo "╔══════════════════════════════════════════════════════════╗"
echo "║  CUBA-MEMORYS MERGE GATE (local CI — sole merge judge)   ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""
echo "WHAT THIS GATE DOES NOT CHECK — read before trusting a green run:"
echo "  · the reranker in E2E   e2e_all_tools.py forces CUBA_RERANKER_PATH at an"
echo "                          empty dir so its calls exercise the identity"
echo "                          fallback. mcp_live_session_test.py inherits your"
echo "                          shell and DOES use the real reranker. Two suites,"
echo "                          opposite policies — both must stay."
echo "  · GPU placement         only the DECISION, not the kernel. After the E2E,"
echo "                          scripts/gpu-placement-check.sh reads doctor --json"
echo "                          and fails when the gpu check disagrees with this"
echo "                          machine: a fallback to CPU where an NVIDIA device"
echo "                          AND the provider libraries are both present, or a"
echo "                          check that never names the CPU on a machine that"
echo "                          has neither. That a kernel really executed on the"
echo "                          GPU cannot be proven without a card — there is"
echo "                          none in CI — and is still not covered here."
echo "  · retrieval quality     the eval step is a smoke run. It proves the"
echo "                          harness executes; it asserts no nDCG threshold."
echo "  · other platforms       this judge runs on the merge machine (typically"
echo "                          Linux x64). Cross-targets are not exercised here."
echo "  · migrations on old DBs the throwaway database is created from scratch, so"
echo "                          a migration that only works on an EXISTING schema"
echo "                          is still not covered here."
echo ""
echo "WHAT IT REQUIRES (missing = FAIL, never SKIPPED):"
echo "  · ONNX embed + ORT      ~/.cache/memory-industry/models + onnxruntime"
echo "                          (legacy ~/.cache/cuba-memorys still resolved)"
echo "  · NLI + reranker        models-nli + reranker (memory-industry models all)"
echo "  · generative LLM        MEMORY_INDUSTRY_LLM_PROVIDER=deepseek|qwen|moonshot|…"
echo "                          (+ API key) OR MEMORY_INDUSTRY_LLM_BASE_URL=/v1"
echo "                          (any OpenAI-compat: CN/US/EU clouds or Ollama)"
echo "                          OR authenticated claude/gemini CLI OR MCP sampling"
echo "  · cargo-deny, machete   install on the merge machine"
echo ""
echo "WHERE IT WRITES:"
echo "  · every mutating step  a throwaway database (GATE_DB, default brain_gate),"
echo "                         created before and dropped after. Your real corpus"
echo "                         is never a test fixture."
echo "  · the eval step        reads the REAL database, because a smoke run against"
echo "                         an empty corpus would prove nothing. Read-only."
echo ""

export DATABASE_URL="${DATABASE_URL:-postgresql://cuba:memorys2026@127.0.0.1:5488/brain}"
if command -v pg_isready >/dev/null 2>&1; then
  pg_isready -h 127.0.0.1 -p 5488 -U cuba -d brain >/dev/null \
    || { echo "FAIL: Postgres not ready on :5488"; exit 1; }
  echo "OK  Postgres :5488"
else
  docker exec memory-industry-db pg_isready -U cuba -d brain >/dev/null \
    || docker exec cuba-memorys-db pg_isready -U cuba -d brain >/dev/null \
    || { echo "FAIL: memory-industry-db / cuba-memorys-db container not healthy"; exit 1; }
  echo "OK  Postgres (docker)"
fi

if [[ "${SKIP_BACKUP:-0}" != "1" ]]; then
  "$ROOT/scripts/backup-db.sh"
  echo "OK  Database backup"
fi

"$ROOT/scripts/run-all-tests.sh"

echo "=== cargo clippy/test --features docs ==="
(cd "$ROOT/rust" && cargo clippy --all-targets --features docs -- -D warnings)
(cd "$ROOT/rust" && cargo test --features docs)

echo "=== cargo deny ==="
if ! command -v cargo-deny >/dev/null 2>&1 && ! (cd "$ROOT/rust" && cargo deny --version >/dev/null 2>&1); then
  echo "FAIL: cargo-deny is not installed. Install it (cargo install cargo-deny) — the gate does not skip license policy." >&2
  exit 1
fi
(cd "$ROOT/rust" && cargo deny check licenses bans sources)

echo "=== npm wrapper smoke ==="
node npm/install.test.js

echo "=== cargo audit ==="
(cd "$ROOT/rust" && cargo audit)

echo "=== codigo-muerto ==="
"$ROOT/scripts/codigo-muerto.sh"

echo "=== crap-gate (coverage floor) ==="
"$ROOT/scripts/crap-gate.sh"

echo "=== mutants-gate ==="
"$ROOT/scripts/mutants-gate.sh"

echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  MERGE GATE PASSED — safe to merge (local CI 100%)       ║"
echo "╚══════════════════════════════════════════════════════════╝"
