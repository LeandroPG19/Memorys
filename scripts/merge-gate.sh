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
    for s in codigo-muerto crap-gate; do
      printf '#!/bin/sh\nexit 0\n' >"$t/scripts/$s.sh"
    done
    printf '%s\n' '#!/bin/sh' \
      'echo "mutants caught=9 missed=1 timeout=0 unviable=0 kill_rate=0.900 min=0.800"' \
      >"$t/scripts/mutants-gate.sh"
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
  # git's own directory is on the copy's PATH: on Git Bash it is /mingw64/bin,
  # not /usr/bin. It runs with a scratch global config, so nobody's signing or
  # hooks settings reach the fixture repos.
  gitdir="$(dirname "$(command -v git)")"
  export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$tmp/gitconfig"
  : >"$GIT_CONFIG_GLOBAL"
  git config --global user.name fixture
  git config --global user.email fixture@example.invalid
  git config --global init.defaultBranch main
  git config --global core.autocrlf false
  run_copy() {
    rc=0
    { env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE HOME="$tmp/$1/home" \
        PATH="$tmp/$1/bin:$gitdir:/usr/bin:/bin" SKIP_BACKUP=1 FAIL_AT="${2:-}" \
        "$BASH" "$tmp/$1/scripts/merge-gate.sh" >"$tmp/$1.out" 2>&1 || rc=$?; } 2>/dev/null
  }
  exit_file() { cat "$tmp/$1/home/.cache/cuba-gate/run.exit" 2>/dev/null || true; }
  # git_tree NAME RUN_ALL_TESTS_BODY: a fake_tree committed whole, its home
  # ignored the way the real HOME is outside the real tree.
  git_tree() {
    fake_tree "$1" "$2"
    printf 'home/\n' >"$tmp/$1/.gitignore"
    git -C "$tmp/$1" init -q
    git -C "$tmp/$1" add -A
    git -C "$tmp/$1" commit -q -m fixture
  }
  receipt_of() { cat "$tmp/$1/home/.cache/cuba-gate/receipts/$2" 2>/dev/null || true; }
  receipts_of() { ls "$tmp/$1/home/.cache/cuba-gate/receipts" 2>/dev/null || true; }
  # A tree where no receipt may appear, and the run has to say why in a line.
  no_receipt() {
    local what="$1" name="$2"
    (( rc == 0 )) || self_fail "$what exited $rc, and it passed: the receipt is not the verdict: $(cat "$tmp/$name.out")"
    [[ -z "$(receipts_of "$name")" ]] || self_fail "$what left a receipt: $(receipts_of "$name")"
    grep -q '^no receipt: ' "$tmp/$name.out" \
      || self_fail "$what wrote no receipt without saying why: $(cat "$tmp/$name.out")"
  }
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

  # The receipt scripts/release.sh reads instead of running the gate again.
  # The presence anchor: a green gate over a clean tree leaves one for its
  # commit, in the four lines release.sh copies into the tag.
  git_tree receipt 'exit 0'
  sha="$(git -C "$tmp/receipt" rev-parse HEAD)"
  run_copy receipt
  (( rc == 0 )) || self_fail "a green gate over a clean tree exited $rc: $(cat "$tmp/receipt.out")"
  r="$(receipt_of receipt "$sha")"
  [[ "$(sed -n 1p <<<"$r")" == "local-gate: MERGE GATE PASSED $sha" ]] \
    || self_fail "a green gate over a clean tree at $sha left no receipt for it ('$r', receipts: '$(receipts_of receipt)'): $(cat "$tmp/receipt.out")"
  [[ "$(wc -l <<<"$r")" -eq 4 ]] || self_fail "the receipt is not four lines: $r"
  grep -qE '^local-gate-date: [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' <<<"$r" \
    || self_fail "the receipt has no UTC date: $r"
  grep -qx 'local-gate-summary: mutants caught=9 missed=1 timeout=0 unviable=0 kill_rate=0.900 min=0.800' <<<"$r" \
    || self_fail "the receipt lost the gate's kill_rate line: $r"
  grep -q '^local-gate-summary: MERGE GATE PASSED' <<<"$r" || self_fail "the receipt lost the gate's verdict line: $r"

  # An untracked file when the gate starts, gone before it ends: what passed is
  # not a commit, and only the look at the start can tell.
  git_tree dirty 'rm -f stray.rs'
  echo x >"$tmp/dirty/stray.rs"
  run_copy dirty
  no_receipt "a gate over an untracked file" dirty

  # A tree dirtied while the gate ran.
  git_tree dirtied 'echo x >stray.rs'
  run_copy dirtied
  no_receipt "a gate whose tree changed while it ran" dirtied

  # HEAD moved while the gate ran: neither commit was judged whole.
  git_tree moved 'git commit -q --allow-empty -m "landed while the gate ran"'
  sha="$(git -C "$tmp/moved" rev-parse HEAD)"
  run_copy moved
  [[ "$(git -C "$tmp/moved" rev-parse HEAD)" != "$sha" ]] || self_fail "the fixture's HEAD never moved, so it proves nothing"
  no_receipt "a gate whose HEAD moved while it ran" moved

  # Exit 0 over a SKIPPED line: release.sh's own judge of the log says no.
  # No colon after the word: local_gate_contract.rs refuses that spelling
  # anywhere in this file, fixtures included.
  git_tree skipped 'echo "SKIPPED (tests that need the NLI model)"'
  run_copy skipped
  no_receipt "a gate that exited 0 over SKIPPED" skipped
  grep -q '^no receipt: .*SKIPPED' "$tmp/skipped.out" \
    || self_fail "the SKIPPED run did not say that was why: $(cat "$tmp/skipped.out")"

  # An earlier pass on this commit, then a run on it that fails: the receipt
  # goes, because the latest verdict on a commit is its verdict.
  git_tree reddened 'exit 0'
  sha="$(git -C "$tmp/reddened" rev-parse HEAD)"
  mkdir -p "$tmp/reddened/home/.cache/cuba-gate/receipts"
  printf 'local-gate: MERGE GATE PASSED %s\n' "$sha" >"$tmp/reddened/home/.cache/cuba-gate/receipts/$sha"
  run_copy reddened deny
  (( rc == 1 )) || self_fail "a gate whose cargo deny failed exited $rc, not 1: $(cat "$tmp/reddened.out")"
  [[ -z "$(receipts_of reddened)" ]] \
    || self_fail "a red gate on $sha left the receipt of an earlier pass on it: $(receipts_of reddened)"

  echo "OK  self-test: the exit file says running while a gate runs and holds the whole"
  echo "    gate's verdict at the end, a step after the tests included; a gate killed"
  echo "    in the middle leaves 'running', never an old 0; a refused gate leaves it alone."
  echo "    A green gate over a clean tree leaves a receipt for its commit; an untracked"
  echo "    file, a tree or HEAD that changed during the run and an exit 0 over SKIPPED"
  echo "    leave none and say why; a red run on a commit removes its earlier receipt"
  exit 0
fi

# The receipt scripts/release.sh publishes from instead of running this gate a
# second time (gate-lock.sh says why). It names one commit, so it is written
# only when that commit is all that was judged: the tree clean, untracked files
# included, when the gate starts and when it ends, and HEAD where it started.
# The log it is judged from is this run's own output, teed below, and the judge
# is gate_log_verdict, the one release.sh uses on its own log.
GATE_LOG="$HOME/.cache/cuba-gate/merge-gate.log"
GATE_START_HEAD=""
GATE_NO_RECEIPT=""
GATE_TEE=""

write_receipt() {
  local status receipt
  if [[ -n "$GATE_NO_RECEIPT" ]]; then
    echo "no receipt: $GATE_NO_RECEIPT"
    return 0
  fi
  if [[ "$(git -C "$ROOT" rev-parse --verify -q HEAD 2>/dev/null || true)" != "$GATE_START_HEAD" ]]; then
    echo "no receipt: HEAD moved from $GATE_START_HEAD while the gate ran, so no one commit was judged whole"
    return 0
  fi
  if ! status="$(git -C "$ROOT" status --porcelain)" || [[ -n "$status" ]]; then
    echo "no receipt: the working tree changed while the gate ran, so what passed is not $GATE_START_HEAD"
    return 0
  fi
  if ! gate_log_verdict "$GATE_LOG"; then
    echo "no receipt: $GATE_LOG_PROBLEM"
    return 0
  fi
  receipt="$GATE_RECEIPTS/$GATE_START_HEAD"
  if mkdir -p "$GATE_RECEIPTS" \
    && gate_receipt_lines "$GATE_START_HEAD" >"$receipt.new.$$" \
    && mv -f "$receipt.new.$$" "$receipt"; then
    echo "receipt: $receipt (scripts/release.sh publishes $GATE_START_HEAD from it without running this gate again)"
  else
    rm -f "$receipt.new.$$"
    echo "no receipt: $receipt could not be written"
  fi
}

# The exit file is this gate's whole verdict, not its first half's: the rules
# are in gate-lock.sh, next to the lock whose holder owns the file. The tee is
# drained first, so the log the receipt is judged from holds the last line. The
# wait is bounded: a process the gate started and left behind (a server the
# E2E launched) keeps the pipe open and tee would never see its end, and an
# unbounded wait would hang the exit of a gate that has already finished.
on_exit() {
  local code=$? tenths=0
  if [[ -n "$GATE_TEE" ]]; then
    exec 1>&3 2>&4 3>&- 4>&-
    while kill -0 "$GATE_TEE" 2>/dev/null && (( tenths < 300 )); do
      sleep 0.1
      tenths=$((tenths + 1))
    done
    if kill -0 "$GATE_TEE" 2>/dev/null; then
      echo "note: something this gate started still holds its output open; $GATE_LOG was judged as it stood after 30 s"
    fi
  fi
  (( code != 0 )) || write_receipt || echo "no receipt: writing it failed"
  exit_file_verdict "$code"
  release_gate_lock
}

acquire_gate_lock "$GATE_LOCK" || exit 1
exit_file_running
trap on_exit EXIT
export CUBA_GATE_LOCK_OWNER="$GATE_OWNER"

# Whatever this commit's earlier receipt said, this run is its verdict now: a
# red run on it takes it away, and a green one writes it again. A dirty start
# judges no commit, so it leaves every receipt alone.
GATE_START_HEAD="$(git -C "$ROOT" rev-parse --verify -q HEAD 2>/dev/null || true)"
if [[ -z "$GATE_START_HEAD" ]]; then
  GATE_NO_RECEIPT="$ROOT has no commit git can name, so a pass here is not a pass of any commit"
elif ! gate_start_status="$(git -C "$ROOT" status --porcelain)" || [[ -n "$gate_start_status" ]]; then
  GATE_NO_RECEIPT="the working tree was not clean when the gate started, so what it judges is not $GATE_START_HEAD"
else
  rm -f "$GATE_RECEIPTS/$GATE_START_HEAD"
fi
mkdir -p "$(dirname "$GATE_LOG")"
exec 3>&1 4>&2
exec > >(tee "$GATE_LOG") 2>&1
GATE_TEE=$!
[[ -z "$GATE_NO_RECEIPT" ]] || echo "note: this run will leave no receipt for scripts/release.sh: $GATE_NO_RECEIPT"

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
echo "  · its own verdict      ~/.cache/cuba-gate/merge-gate.log, and on a green"
echo "                         run over a clean tree receipts/<sha> next to it,"
echo "                         which scripts/release.sh tags from instead of"
echo "                         running this gate a second time."
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
