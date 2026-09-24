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

# --- self-test: the exit file against the fixtures that have to stop it ------
# ~/.cache/cuba-gate/run.exit is what gets read when the session that launched
# a gate is gone. It used to be written by run-all-tests.sh, the first half of
# this gate, so a run whose tests passed said 0 there while deny, audit,
# codigo-muerto, crap-gate or mutants-gate could still fail; and a gate killed
# halfway (SIGKILL under memory pressure, twice on 2026-09-23) runs no trap, so
# the file kept whatever an earlier run had left in it. Each fixture runs a
# COPY of this script in a throwaway tree whose every step is a stand-in
# (pg_isready, cargo, node, run-all-tests.sh and the three gate scripts after
# it), with HOME inside the tree: nothing here touches the cluster, the real
# lock or cargo. The exit file of every tree starts at 0, as an earlier green
# gate would have left it, which is exactly what a gate must not leave behind
# unless it is its own verdict.
#   ./scripts/merge-gate.sh --self-test
if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d)"
  sleep 300 &
  live=$!
  trap 'kill "$live" 2>/dev/null || true; rm -rf "$tmp"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  # fake_tree NAME RUN_ALL_TESTS_BODY
  fake_tree() {
    local t="$tmp/$1" s
    mkdir -p "$t/scripts" "$t/rust" "$t/bin" "$t/home/.cache/cuba-gate"
    cp "$ROOT/scripts/merge-gate.sh" "$ROOT/scripts/gate-lock.sh" "$t/scripts/"
    printf '#!/bin/sh\n%s\n' "$2" >"$t/scripts/run-all-tests.sh"
    for s in codigo-muerto crap-gate mutants-gate; do
      printf '#!/bin/sh\nexit 0\n' >"$t/scripts/$s.sh"
    done
    printf '#!/bin/sh\nexit 0\n' >"$t/bin/pg_isready"
    printf '#!/bin/sh\nexit 0\n' >"$t/bin/node"
    # cargo fails the one subcommand named in FAIL_AT, and not its --version
    # probe, so the gate gets past "is cargo-deny installed" to the check.
    printf '%s\n' '#!/bin/sh' \
      'if [ "$1" = "$FAIL_AT" ] && [ "$2" != "--version" ]; then exit 1; fi' \
      'exit 0' >"$t/bin/cargo"
    chmod +x "$t"/scripts/*.sh "$t"/bin/*
    printf '0\n' >"$t/home/.cache/cuba-gate/run.exit"
  }
  # run_copy NAME [FAIL_AT]: the copy's exit code lands in $rc. The braces
  # keep bash's own "Killed" job notice, for the fixture that kills the copy,
  # off this script's stderr; everything the copy prints is in NAME.out.
  run_copy() {
    rc=0
    { env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE HOME="$tmp/$1/home" \
        PATH="$tmp/$1/bin:/usr/bin:/bin" SKIP_BACKUP=1 FAIL_AT="${2:-}" \
        "$BASH" "$tmp/$1/scripts/merge-gate.sh" >"$tmp/$1.out" 2>&1 || rc=$?; } 2>/dev/null
  }
  exit_file() { cat "$tmp/$1/home/.cache/cuba-gate/run.exit" 2>/dev/null || true; }
  reads_while_running='echo "stand-in run-all-tests: run.exit reads: $(cat "$HOME/.cache/cuba-gate/run.exit")"'

  # The presence anchor: every step passes, so the file ends at 0, and while
  # the first half ran it said the gate was still running. A file that ends at
  # 0 proves nothing on its own: it started there.
  fake_tree green "$reads_while_running"
  run_copy green
  (( rc == 0 )) || self_fail "a gate whose every step passed exited $rc: $(cat "$tmp/green.out")"
  [[ "$(exit_file green)" == 0 ]] || self_fail "a green gate left '$(exit_file green)' in its exit file, not 0"
  grep -q 'run.exit reads: running pid=[0-9]' "$tmp/green.out" \
    || self_fail "while the gate ran, its exit file did not say so: $(grep 'run.exit reads' "$tmp/green.out")"

  # The first half passes and a step after it fails: the file is the gate's.
  fake_tree deny "$reads_while_running"
  run_copy deny deny
  (( rc == 1 )) || self_fail "a gate whose cargo deny failed exited $rc, not 1: $(cat "$tmp/deny.out")"
  [[ "$(exit_file deny)" == 1 ]] \
    || self_fail "a gate that failed at cargo deny, after its tests passed, left '$(exit_file deny)' in its exit file, not 1"

  # Killed in the middle, the way the kernel or Windows kills under memory
  # pressure: no trap runs, and the file must not read as a verdict.
  fake_tree killed 'kill -9 "$PPID"; exit 0'
  run_copy killed
  (( rc != 0 )) || self_fail "a gate killed in the middle exited 0"
  [[ "$(exit_file killed)" == "running pid="* ]] \
    || self_fail "a gate killed in the middle left '$(exit_file killed)' in its exit file, not 'running pid=...'"

  # A second gate, refused by a live one's lock: the file belongs to that one.
  fake_tree refused 'exit 0'
  mkdir -p "$tmp/refused/home/.cache/cuba-gate/lock"
  owner_record "$live" fixture-live >"$tmp/refused/home/.cache/cuba-gate/lock/owner"
  run_copy refused
  grep -q "another gate is running: pid $live" "$tmp/refused.out" \
    || self_fail "the second gate was not refused by the live one (exit $rc): $(cat "$tmp/refused.out")"
  [[ "$(exit_file refused)" == 0 ]] \
    || self_fail "a refused second gate wrote '$(exit_file refused)' into the exit file of the gate that holds the lock"

  echo "OK  self-test: the exit file says running while a gate runs and holds the whole"
  echo "    gate's verdict at the end, a step after the tests included; a gate killed"
  echo "    in the middle leaves 'running', never an old 0; a refused gate leaves it alone"
  exit 0
fi

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
