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
    # pg_isready: the row picks what it answers on stdout, the line it writes on
    # stderr and its exit code (FAKE_PG_ANSWER, FAKE_PG_EXIT, FAKE_PG_STDERR),
    # and each call leaves its arguments, one per line, in $HOME/pg_isready.args.
    # Unless the row says otherwise it answers as a server on the default port
    # does, and its stderr carries a word that no line of the gate's output may
    # carry: libpq quotes the piece of the URL it cannot read, and that piece
    # can be the password.
    printf '%s\n' '#!/bin/sh' \
      'for a in "$@"; do printf "%s\n" "$a"; done >>"$HOME/pg_isready.args"' \
      'echo "${FAKE_PG_STDERR-pg_isready: stderr noise carrying S3cr3tW2}" >&2' \
      'answer="${FAKE_PG_ANSWER-127.0.0.1:5488 - accepting connections}"' \
      '[ -z "$answer" ] || echo "$answer"' \
      'exit "${FAKE_PG_EXIT:-0}"' >"$t/bin/pg_isready"
    # gate_linux_env reads the version of node, and package.json says which one
    # is the oldest the gate accepts.
    printf '#!/bin/sh\necho v20.0.0\n' >"$t/bin/node"
    printf '{"engines":{"node":">=18"}}\n' >"$t/package.json"
    # cargo fails the one subcommand named in FAIL_AT, and not its --version
    # probe, so the gate gets past "is cargo-deny installed" to the check. Each
    # call leaves its arguments in $HOME/steps, which is how a row reads the
    # order of the steps. cargo audit has two ways to fail that the gate has to
    # tell apart: a database it could not fetch (FAKE_AUDIT_NOFETCH=1, the three
    # lines cargo-audit 0.22.2 printed with no route to github.com) and an
    # advisory it found (FAIL_AT=audit).
    printf '%s\n' '#!/bin/sh' \
      'echo "cargo $*" >>"$HOME/steps"' \
      'if [ "$1" = audit ] && [ "${FAKE_AUDIT_NOFETCH:-}" = 1 ]; then' \
      '  echo "error: couldn'"'"'t fetch advisory database: git operation failed: An IO error occurred when talking to the server" >&2' \
      '  echo "Caused by:" >&2' \
      '  echo "  -> error sending request for url (https://github.com/RustSec/advisory-db.git/info/refs?service=git-upload-pack)" >&2' \
      '  exit 1' \
      'fi' \
      'if [ "$1" = audit ] && [ "$FAIL_AT" = audit ]; then' \
      '  echo "stand-in cargo audit: 1 vulnerability found"' \
      '  exit 1' \
      'fi' \
      'if [ "$1" = "$FAIL_AT" ] && [ "$2" != "--version" ]; then exit 1; fi' \
      'exit 0' >"$t/bin/cargo"
    # timeout, as in GNU coreutils, takes the limit and then the command. It leaves
    # what it was given in $HOME/timeout.args, and FAKE_TIMEOUT_EXPIRES=1 makes
    # it say what coreutils says when the limit is reached (124) without running
    # anything, so no row waits 120 s.
    printf '%s\n' '#!/bin/sh' \
      'echo "$*" >>"$HOME/timeout.args"' \
      'if [ "${FAKE_TIMEOUT_EXPIRES:-}" = 1 ]; then exit 124; fi' \
      'shift' \
      'exec "$@"' >"$t/bin/timeout"
    # docker answers the two calls the readiness fallback makes and records them
    # in $HOME/docker.args. `ps` lists FAKE_DOCKER_NAMES (space separated, one
    # per line) when its publish= filter names FAKE_DOCKER_PORT, nothing for any
    # other port, and exits FAKE_DOCKER_PS_EXIT when that is defined. `exec`
    # answers as a ready Postgres for the names in FAKE_DOCKER_HEALTHY, and as
    # docker does for a container that is not there for any other.
    printf '%s\n' '#!/bin/sh' \
      'echo "$*" >>"$HOME/docker.args"' \
      'case "$1" in' \
      '  ps)' \
      '    [ -z "${FAKE_DOCKER_PS_EXIT+x}" ] || exit "$FAKE_DOCKER_PS_EXIT"' \
      '    port=' \
      '    for a in "$@"; do' \
      '      case "$a" in *publish=*) port="${a#*publish=}" ;; esac' \
      '    done' \
      '    if [ -n "$port" ] && [ "$port" = "${FAKE_DOCKER_PORT:-}" ]; then' \
      '      for n in $FAKE_DOCKER_NAMES; do echo "$n"; done' \
      '    fi' \
      '    exit 0 ;;' \
      '  exec)' \
      '    for n in ${FAKE_DOCKER_HEALTHY:-}; do' \
      '      if [ "$n" = "$2" ]; then echo "/var/run/postgresql:5432 - accepting connections"; exit 0; fi' \
      '    done' \
      '    echo "Error response from daemon: No such container: $2" >&2' \
      '    exit 1 ;;' \
      'esac' \
      'exit 0' >"$t/bin/docker"
    chmod +x "$t"/scripts/*.sh "$t"/bin/*
    printf '0\n' >"$t/home/.cache/cuba-gate/run.exit"
  }
  # run_copy NAME [FAIL_AT [VAR=value ...]]: the copy's exit code lands in $rc.
  # The braces keep bash's own "Killed" job notice, for the fixture that kills
  # the copy, off this script's stderr; everything the copy prints is in
  # NAME.out. The VAR=value pairs reach the copy as they are. DATABASE_URL is
  # never inherited from whoever runs this test, so that no row's verdict
  # depends on the shell it was started from: a row that needs one names it,
  # and VAR= defines it empty.
  # git's own directory is on the copy's PATH: on Git Bash it is /mingw64/bin,
  # not /usr/bin. It runs with a scratch global config, so nobody's signing or
  # hooks settings reach the fixture repos. PGPORT is not inherited either: it
  # is the port libpq falls back to, and the rows of the readiness fallback
  # that name none have to get the default, not the shell's.
  gitdir="$(dirname "$(command -v git)")"
  export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$tmp/gitconfig"
  : >"$GIT_CONFIG_GLOBAL"
  git config --global user.name fixture
  git config --global user.email fixture@example.invalid
  git config --global init.defaultBranch main
  git config --global core.autocrlf false
  run_copy() {
    local name="$1" fail_at="${2:-}"
    shift $(( $# > 1 ? 2 : 1 ))
    rc=0
    { env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u DATABASE_URL -u PGPORT HOME="$tmp/$name/home" \
        PATH="$tmp/$name/bin:$gitdir:/usr/bin:/bin" SKIP_BACKUP=1 FAIL_AT="$fail_at" "$@" \
        "$BASH" "$tmp/$name/scripts/merge-gate.sh" >"$tmp/$name.out" 2>&1 || rc=$?; } 2>/dev/null
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

  # The order of the steps, read from $HOME/steps (one line per cargo call, and
  # one for run-all-tests.sh). cargo audit is the only step that judges RustSec
  # advisories, and it refreshes the database it reads, so a judge idle for days
  # used to learn of a new advisory after ~20 min of tests, when 3 s would do. It
  # runs first, under `timeout 120`; a database it cannot refresh is exit 2 and
  # says what is missing, and an advisory it finds is exit 1 with cargo-audit's
  # own lines. Neither lets cargo deny or the tests start. cargo deny is the
  # next step, still before the tests.
  tests_step='echo run-all-tests >>"$HOME/steps"'
  steps_of() { cat "$tmp/$1/home/steps" 2>/dev/null || true; }
  # step_at NAME PATTERN: the line of $HOME/steps where PATTERN first appears, or nothing.
  step_at() {
    local hit
    hit="$(grep -nm1 -- "$2" "$tmp/$1/home/steps" 2>/dev/null || true)"
    printf '%s' "${hit%%:*}"
  }
  # fail_line_with NAME WORD...: some line of the gate's output starts with FAIL:
  # and holds every WORD.
  fail_line_with() {
    local name="$1" line word ok
    shift
    while IFS= read -r line; do
      ok=1
      for word in "$@"; do
        [[ "$line" == *"$word"* ]] || { ok=0; break; }
      done
      if (( ok )); then return 0; fi
    done < <(grep '^FAIL: ' "$tmp/$name.out" | tr -d '\r' || true)
    return 1
  }
  # adv_stopped NAME CODE: the gate stopped with CODE, in its exit file too, and
  # freed its lock.
  adv_stopped() {
    local name="$1" want="$2"
    (( rc == want )) || self_fail "$name: the gate exited $rc, not $want: $(cat "$tmp/$name.out")"
    [[ "$(exit_file "$name")" == "$want" ]] \
      || self_fail "$name: the exit file says '$(exit_file "$name")', not $want"
    [[ ! -e "$tmp/$name/home/.cache/cuba-gate/lock" ]] || self_fail "$name: the gate left its lock behind"
  }
  # adv_never NAME STEP...: no line of $HOME/steps starts with any STEP.
  adv_never() {
    local name="$1" step
    shift
    for step in "$@"; do
      if [[ -n "$(step_at "$name" "^$step")" ]]; then
        self_fail "$name: '$step' ran, and it must not have: $(steps_of "$name")"
      fi
    done
  }

  fake_tree adv_order "$tests_step"
  run_copy adv_order
  (( rc == 0 )) || self_fail "adv_order: a gate whose every step passed exited $rc: $(cat "$tmp/adv_order.out")"
  grep -qxF '120 cargo audit' "$tmp/adv_order/home/timeout.args" 2>/dev/null \
    || self_fail "adv_order: cargo audit was not run as 'timeout 120 cargo audit' (timeout was given: '$(cat "$tmp/adv_order/home/timeout.args" 2>/dev/null || true)')"
  audit_at="$(step_at adv_order '^cargo audit$')"
  deny_at="$(step_at adv_order '^cargo deny check')"
  tests_at="$(step_at adv_order '^run-all-tests$')"
  [[ -n "$audit_at" && -n "$deny_at" && -n "$tests_at" ]] \
    || self_fail "adv_order: a step is missing from the order (audit '$audit_at', deny '$deny_at', tests '$tests_at'): $(steps_of adv_order)"
  (( audit_at < deny_at && deny_at < tests_at )) \
    || self_fail "adv_order: the order is not cargo audit, then cargo deny, then the tests: $(steps_of adv_order)"

  # No route to github.com: cargo-audit fails to refresh its database. The gate
  # does not judge advisories against the copy on disk, and it says which
  # database and where it lives.
  fake_tree adv_nofetch "$tests_step"
  run_copy adv_nofetch "" FAKE_AUDIT_NOFETCH=1
  adv_stopped adv_nofetch 2
  [[ "$(steps_of adv_nofetch)" == "cargo audit" ]] \
    || self_fail "adv_nofetch: cargo was called for more than the audit: $(steps_of adv_nofetch)"
  fail_line_with adv_nofetch 'advisory database' 'https://github.com/RustSec/advisory-db' \
    || self_fail "adv_nofetch: no FAIL line names the advisory database and where it is fetched from: $(cat "$tmp/adv_nofetch.out")"
  if grep -q 'MERGE GATE PASSED' "$tmp/adv_nofetch.out"; then
    self_fail "adv_nofetch: a gate that could not refresh its advisories passed: $(cat "$tmp/adv_nofetch.out")"
  fi

  # A refresh that does not end within the ceiling is the same refusal, and it
  # names the ceiling.
  fake_tree adv_timeout "$tests_step"
  run_copy adv_timeout "" FAKE_TIMEOUT_EXPIRES=1
  adv_stopped adv_timeout 2
  adv_never adv_timeout 'cargo audit' 'cargo deny' 'run-all-tests'
  fail_line_with adv_timeout '120 s' 'advisory database' \
    || self_fail "adv_timeout: no FAIL line names the 120 s ceiling and the advisory database: $(cat "$tmp/adv_timeout.out")"

  # An advisory found is the verdict, not a missing database: exit 1, with what
  # cargo-audit itself said, and nothing after it.
  fake_tree adv_found "$tests_step"
  run_copy adv_found audit
  adv_stopped adv_found 1
  grep -q 'stand-in cargo audit: 1 vulnerability found' "$tmp/adv_found.out" \
    || self_fail "adv_found: the gate did not print the lines of cargo audit: $(cat "$tmp/adv_found.out")"
  adv_never adv_found 'cargo deny check' 'run-all-tests'
  if fail_line_with adv_found 'advisory database'; then
    self_fail "adv_found: an advisory that was found was reported as a database that could not be refreshed: $(cat "$tmp/adv_found.out")"
  fi

  # cargo deny judges licenses, bans and sources, and it is red before the tests.
  fake_tree deny_before_tests "$tests_step"
  run_copy deny_before_tests deny
  (( rc == 1 )) || self_fail "deny_before_tests: a gate whose cargo deny failed exited $rc, not 1: $(cat "$tmp/deny_before_tests.out")"
  audit_at="$(step_at deny_before_tests '^cargo audit$')"
  deny_at="$(step_at deny_before_tests '^cargo deny check')"
  [[ -n "$audit_at" && -n "$deny_at" ]] && (( audit_at < deny_at )) \
    || self_fail "deny_before_tests: cargo deny did not come after cargo audit: $(steps_of deny_before_tests)"
  adv_never deny_before_tests 'run-all-tests'

  # A step after the tests, with the first half past, still writes the whole
  # gate's verdict, and the file said running while the tests ran.
  fake_tree after_tests "$reads_while_running; $tests_step"
  run_copy after_tests clippy
  adv_stopped after_tests 1
  grep -q 'run.exit reads: running pid=[0-9]' "$tmp/after_tests.out" \
    || self_fail "after_tests: while the tests ran, the exit file did not say so: $(grep 'run.exit reads' "$tmp/after_tests.out")"

  # The readiness check: libpq reads DATABASE_URL, and the gate says which port
  # it answered on. With WSL2 in mirrored mode the localhost:5488 of Windows is
  # the live corpus of WSL, so a check that asked :5488 whatever the URL said
  # judged the wrong server and still printed an OK. Each row is a copy of the
  # gate whose run-all-tests.sh stand-in prints a marker, so "what came next" and
  # "it never ran" can be read, and whose pg_isready stand-in (fake_tree) answers
  # what the row says and records what it was asked.
  ran_marker='stand-in run-all-tests: the next step ran'
  pg_args() { cat "$tmp/$1/home/pg_isready.args" 2>/dev/null || true; }
  # pg_stopped NAME CODE [PASSWORD]: the gate stopped with CODE, never reached
  # the next step, wrote CODE in its own exit file, freed its lock, and nothing
  # it printed carries the stand-in's stderr word or the URL's password.
  pg_stopped() {
    local name="$1" want="$2" password="${3:-}"
    (( rc == want )) || self_fail "$name: the gate exited $rc, not $want: $(cat "$tmp/$name.out")"
    if grep -qF "$ran_marker" "$tmp/$name.out"; then
      self_fail "$name: the gate went on to run-all-tests.sh: $(cat "$tmp/$name.out")"
    fi
    [[ "$(exit_file "$name")" == "$want" ]] \
      || self_fail "$name: the exit file says '$(exit_file "$name")', not $want"
    [[ ! -e "$tmp/$name/home/.cache/cuba-gate/lock" ]] || self_fail "$name: the gate left its lock behind"
    # The readiness check comes before every step, cargo audit included: a gate
    # that asks cargo before it knows Postgres is up has the order wrong.
    if grep -q 'cargo' "$tmp/$name/home/steps" 2>/dev/null; then
      self_fail "$name: cargo was called before Postgres was known to be ready: $(cat "$tmp/$name/home/steps")"
    fi
    if grep -q 'S3cr3tW2' "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries S3cr3tW2: $(cat "$tmp/$name.out")"
    fi
    if [[ -n "$password" ]] && grep -qF "$password" "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the password of DATABASE_URL: $(cat "$tmp/$name.out")"
    fi
  }
  # pg_green NAME URL ANSWER PORT PASSWORD: URL is what the row defines as
  # DATABASE_URL, or `unset` and `empty` for the gate's own default. pg_isready
  # gets exactly -d and that URL, and the line after the banner is the OK for
  # PORT, the one pg_isready's ANSWER names, with nothing printed in between.
  # The line after the OK is the banner of cargo audit, the first step that
  # follows the readiness check.
  pg_green() {
    local name="$1" url="$2" answer="$3" port="$4" password="$5" i at=-1
    local default_url='postgresql://cuba:memorys2026@127.0.0.1:5488/brain' want="$2"
    local -a lines var=()
    case "$url" in
      unset) want="$default_url" ;;
      empty)
        want="$default_url"
        var=(DATABASE_URL=)
        ;;
      *) var=(DATABASE_URL="$url") ;;
    esac
    fake_tree "$name" "echo \"$ran_marker\""
    run_copy "$name" "" FAKE_PG_ANSWER="$answer" ${var[@]+"${var[@]}"}
    (( rc == 0 )) || self_fail "$name: a gate whose pg_isready answered '$answer' exited $rc: $(cat "$tmp/$name.out")"
    [[ "$(pg_args "$name")" == $'-d\n'"$want" ]] \
      || self_fail "$name: pg_isready was called with '$(pg_args "$name" | tr '\n' ' ')', not with exactly -d and DATABASE_URL as it is ('$want')"
    mapfile -t lines < <(tr -d '\r' <"$tmp/$name.out")
    for i in "${!lines[@]}"; do
      if [[ "${lines[i]}" == "OK  Postgres :"* ]]; then
        at=$i
        break
      fi
    done
    (( at >= 2 )) || self_fail "$name: no 'OK  Postgres :<port>' line after the banner: $(cat "$tmp/$name.out")"
    [[ "${lines[at]}" == "OK  Postgres :$port" ]] \
      || self_fail "$name: the line said '${lines[at]}', not the port pg_isready answered on ($port)"
    [[ -z "${lines[at-1]}" && "${lines[at-2]}" == *"a second time."* ]] \
      || self_fail "$name: something was printed between the banner and the OK line: '${lines[at-2]}' / '${lines[at-1]}'"
    [[ "${lines[at+1]:-}" == "=== cargo audit ===" ]] \
      || self_fail "$name: the line after the OK line is '${lines[at+1]:-}', not the banner of cargo audit, the first step after the readiness check"
    if grep -qxF "$answer" "$tmp/$name.out"; then
      self_fail "$name: pg_isready's own answer is a line of the output: $(cat "$tmp/$name.out")"
    fi
    if [[ "$port" != 5488 ]] && grep -q ':5488' "$tmp/$name.out"; then
      self_fail "$name: the output names :5488, and the server answered on :$port: $(cat "$tmp/$name.out")"
    fi
    if grep -q 'S3cr3tW2' "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the stand-in's stderr: $(cat "$tmp/$name.out")"
    fi
    if [[ -n "$password" ]] && grep -qF "$password" "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the password of DATABASE_URL: $(cat "$tmp/$name.out")"
    fi
  }
  pg_green pg_default unset "127.0.0.1:5488 - accepting connections" 5488 memorys2026
  pg_green pg_empty empty "127.0.0.1:5488 - accepting connections" 5488 memorys2026
  pg_green pg_port 'postgresql://gate:pw0@127.0.0.1:5575/brain' \
    "127.0.0.1:5575 - accepting connections" 5575 pw0
  # No port in the URL: it is libpq that puts 5432 (or PGPORT), not the gate.
  pg_green pg_postgres_scheme 'postgres://gate:pw0@db.example.invalid/brain' \
    "db.example.invalid:5432 - accepting connections" 5432 pw0
  pg_green pg_encoded 'postgresql://ga%40te:p%40ss%2Fw%3Frd@127.0.0.1:6543/brain' \
    "127.0.0.1:6543 - accepting connections" 6543 'p%40ss%2Fw%3Frd'
  pg_green pg_socket 'postgresql://gate@%2Frun%2Fpostgresql:5433/brain' \
    "/run/postgresql:5433 - accepting connections" 5433 ''
  pg_green pg_ipv6 'postgresql://gate:pw0@[::1]:5434/brain' \
    "::1:5434 - accepting connections" 5434 pw0

  # Nothing answers on the port of the URL: the gate says which one, with what
  # libpq said, and stops with 1.
  fake_tree pg_down "echo \"$ran_marker\""
  run_copy pg_down "" DATABASE_URL='postgresql://gate:pw0@127.0.0.1:5575/brain' \
    FAKE_PG_ANSWER='127.0.0.1:5575 - no response' FAKE_PG_EXIT=2
  pg_stopped pg_down 1 pw0
  grep -qxF 'FAIL: Postgres not ready on :5575 (pg_isready: 127.0.0.1:5575 - no response)' "$tmp/pg_down.out" \
    || self_fail "a Postgres that did not answer was not named with the port of the URL and libpq's own answer: $(cat "$tmp/pg_down.out")"
  if grep -q ':5488' "$tmp/pg_down.out"; then
    self_fail "the refusal for :5575 names :5488: $(cat "$tmp/pg_down.out")"
  fi

  # libpq cannot read the URL: pg_isready exits 3 and its stderr quotes the piece
  # it could not read, which here is the password. The gate says that it could
  # not read DATABASE_URL and how to see libpq's own words, and says nothing of
  # the URL.
  fake_tree pg_unreadable "echo \"$ran_marker\""
  run_copy pg_unreadable "" DATABASE_URL='postgresql://gate:S3cr3tW2%zz@127.0.0.1:5575/brain' \
    FAKE_PG_ANSWER= FAKE_PG_EXIT=3 \
    FAKE_PG_STDERR='pg_isready: error: invalid percent-encoded token: "S3cr3tW2%zz"'
  pg_stopped pg_unreadable 2
  grep -q '^FAIL: ' "$tmp/pg_unreadable.out" \
    || self_fail "a gate whose pg_isready could not read DATABASE_URL did not refuse in a FAIL line: $(cat "$tmp/pg_unreadable.out")"
  grep -qF 'pg_isready -d "$DATABASE_URL"' "$tmp/pg_unreadable.out" \
    || self_fail "the refusal did not say how to see libpq's own words: $(cat "$tmp/pg_unreadable.out")"

  # An answer with no port in it: the gate does not know which server answered,
  # and does not guess the one it used to ask.
  fake_tree pg_no_port "echo \"$ran_marker\""
  run_copy pg_no_port "" DATABASE_URL='postgresql://gate:pw0@127.0.0.1:abc/brain' \
    FAKE_PG_ANSWER='127.0.0.1:abc - no response' FAKE_PG_EXIT=2
  pg_stopped pg_no_port 2 pw0
  grep -Eq "[\"']127\.0\.0\.1:abc - no response[\"']" "$tmp/pg_no_port.out" \
    || self_fail "the refusal for an answer with no port did not quote the answer: $(cat "$tmp/pg_no_port.out")"
  if grep -q ':5488' "$tmp/pg_no_port.out"; then
    self_fail "an answer with no port was read as :5488: $(cat "$tmp/pg_no_port.out")"
  fi

  # A URL the gate cannot derive brain_gate from (run-all-tests.sh builds that
  # URL by replacing what follows the last slash): refused with 2 before
  # pg_isready is asked, and the reason carries none of the URL.
  # pg_refused NAME URL WORD...: every WORD is in what the gate printed.
  pg_refused() {
    local name="$1" url="$2" word
    shift 2
    fake_tree "$name" "echo \"$ran_marker\""
    run_copy "$name" "" DATABASE_URL="$url"
    pg_stopped "$name" 2
    grep -q '^FAIL: ' "$tmp/$name.out" \
      || self_fail "$name: the refusal is not a FAIL line: $(cat "$tmp/$name.out")"
    [[ ! -e "$tmp/$name/home/pg_isready.args" ]] \
      || self_fail "$name: pg_isready was asked about a URL the gate cannot derive brain_gate from: $(pg_args "$name")"
    for word in "$@"; do
      grep -qF -- "$word" "$tmp/$name.out" \
        || self_fail "$name: the refusal does not say '$word': $(cat "$tmp/$name.out")"
    done
  }
  pg_refused pg_query 'postgresql://gate:S3cr3tW2@/brain?host=%2Frun%2Fpostgresql' '%2F'
  pg_refused pg_no_db 'postgresql://gate:S3cr3tW2@127.0.0.1:5575' '%2F' '%3F' '%40'
  pg_refused pg_conninfo 'host=127.0.0.1 port=5575 dbname=brain password=S3cr3tW2' 'postgresql://'

  # More than one '@' before the host: libpq ends the user and the password at
  # the FIRST one and sqlx at the LAST, so the readiness check and the binary
  # would ask different servers, and libpq prints the rest of the password as
  # the host (measured with libpq 16.15 on 127.0.0.1:5499, nothing listening:
  # postgresql://u:x@S3cr3tW2@127.0.0.1:5499/brain answers
  # 'S3cr3tW2@127.0.0.1:5499 - no response'). The stand-in answers what libpq
  # did; the gate has to refuse with 2 before asking, and not print it.
  fake_tree pg_two_at "echo \"$ran_marker\""
  run_copy pg_two_at "" DATABASE_URL='postgresql://gate:x@S3cr3tW2@127.0.0.1:5575/brain' \
    FAKE_PG_ANSWER='S3cr3tW2@127.0.0.1:5575 - no response' FAKE_PG_EXIT=2
  pg_stopped pg_two_at 2
  [[ "$(grep -c '^FAIL: ' "$tmp/pg_two_at.out")" == 1 ]] \
    || self_fail "pg_two_at: the refusal is not one FAIL line: $(cat "$tmp/pg_two_at.out")"
  grep -qF '%40' "$tmp/pg_two_at.out" && grep -qF "more than one '@'" "$tmp/pg_two_at.out" \
    || self_fail "pg_two_at: the refusal does not say %40 and more than one '@': $(cat "$tmp/pg_two_at.out")"
  [[ ! -e "$tmp/pg_two_at/home/pg_isready.args" ]] \
    || self_fail "pg_two_at: pg_isready was asked about a URL with two '@': $(pg_args pg_two_at)"
  pg_refused pg_double_at 'postgresql://gate:S3cr3tW2@@127.0.0.1:5575/brain' '%40' "more than one '@'"

  # Without pg_isready the gate asks docker, and the container is the one that
  # publishes the port of DATABASE_URL, not one picked by its name: on Windows the
  # judge's container was memory-industry-dbwin-20261012 on :5489, and a check by
  # name judged memory-industry-db, a server DATABASE_URL did not name. A host
  # that is not this machine, or a gate with no docker, cannot be judged this way
  # and is refused with 2. Each row runs with a PATH of the tree's stand-ins and
  # of wrappers for every program the system has except pg_isready and docker:
  # a pg_isready of the machine running this test would turn every row green
  # (Debian puts one in /usr/bin).
  mkdir -p "$tmp/sysbin"
  for d in "$gitdir" /usr/bin /bin; do
    for f in "$d"/*; do
      [[ -f "$f" && -x "$f" ]] || continue
      tool="${f##*/}"
      tool="${tool%.exe}"
      case "$tool" in pg_isready* | docker*) continue ;; esac
      [[ -e "$tmp/sysbin/$tool" ]] && continue
      printf '#!/bin/sh\nexec "%s" "$@"\n' "$f" >"$tmp/sysbin/$tool"
    done
  done
  chmod +x "$tmp"/sysbin/*
  ran_body="echo run-all-tests >>\"\$HOME/steps\"; echo \"$ran_marker\""
  # dk_prepare NAME: the tree, with no pg_isready, and a check that the PATH of
  # the row finds none and still finds git.
  dk_prepare() {
    local name="$1" probe="$tmp/$1/bin:$tmp/sysbin"
    fake_tree "$name" "$ran_body"
    rm -f "$tmp/$name/bin/pg_isready"
    if (PATH="$probe"; command -v pg_isready >/dev/null 2>&1); then
      self_fail "the fixture cannot hide pg_isready: $name finds one on $probe"
    fi
    (PATH="$probe"; command -v git >/dev/null 2>&1) \
      || self_fail "the fixture hides git: $name finds none on $probe"
  }
  # dk_go NAME URL [VAR=value ...]: the gate with that DATABASE_URL.
  dk_go() {
    local name="$1" url="$2"
    shift 2
    if [[ ! -e "$tmp/$name/bin/docker" ]] && (PATH="$tmp/$name/bin:$tmp/sysbin"; command -v docker >/dev/null 2>&1); then
      self_fail "the fixture cannot hide docker: $name finds one"
    fi
    run_copy "$name" "" PATH="$tmp/$name/bin:$tmp/sysbin" DATABASE_URL="$url" "$@"
  }
  docker_args() { cat "$tmp/$1/home/docker.args" 2>/dev/null || true; }
  # dk_green NAME URL PORT CONTAINER [PASSWORD] [VAR=value ...]: the gate asked
  # docker which container publishes PORT, ran pg_isready in that one (and in none
  # picked by name), said which it was, and went on to cargo audit.
  dk_green() {
    local name="$1" url="$2" port="$3" container="$4" password="$5" i at=-1 asked
    local -a lines
    shift 5
    dk_prepare "$name"
    dk_go "$name" "$url" FAKE_DOCKER_PORT="$port" FAKE_DOCKER_NAMES="$container" \
      FAKE_DOCKER_HEALTHY="$container" "$@"
    (( rc == 0 )) || self_fail "$name: a gate whose container for :$port was ready exited $rc: $(cat "$tmp/$name.out")"
    asked="$(docker_args "$name")"
    grep -Eq "^ps .*publish=$port( |\$)" <<<"$asked" \
      || self_fail "$name: docker ps was not asked for the container that publishes $port: $asked"
    grep -q "^exec $container pg_isready" <<<"$asked" \
      || self_fail "$name: pg_isready was not run in $container: $asked"
    if grep -Eq '^exec (memory-industry-db|cuba-memorys-db)( |$)' <<<"$asked"; then
      self_fail "$name: a container was picked by its name: $asked"
    fi
    mapfile -t lines < <(tr -d '\r' <"$tmp/$name.out")
    for i in "${!lines[@]}"; do
      if [[ "${lines[i]}" == "OK  Postgres"* ]]; then
        at=$i
        break
      fi
    done
    (( at >= 0 )) || self_fail "$name: no 'OK  Postgres' line: $(cat "$tmp/$name.out")"
    [[ "${lines[at]}" == "OK  Postgres :$port (container $container)" ]] \
      || self_fail "$name: the line said '${lines[at]}', not the port and the container that answered"
    [[ "${lines[at+1]:-}" == "=== cargo audit ===" ]] \
      || self_fail "$name: the line after the OK line is '${lines[at+1]:-}', not the banner of cargo audit"
    if [[ -n "$password" ]] && grep -qF "$password" "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the password of DATABASE_URL: $(cat "$tmp/$name.out")"
    fi
  }
  # dk_refused NAME CODE PASSWORD WORD...: the gate stopped with CODE (pg_stopped),
  # asked docker no `exec`, and a FAIL line holds every WORD.
  dk_refused() {
    local name="$1" want="$2" password="$3"
    shift 3
    pg_stopped "$name" "$want" "$password"
    if grep -q '^exec ' <<<"$(docker_args "$name")"; then
      self_fail "$name: the gate ran something in a container it should not have judged: $(docker_args "$name")"
    fi
    fail_line_with "$name" "$@" \
      || self_fail "$name: no FAIL line holds all of: $* : $(cat "$tmp/$name.out")"
  }

  # The container is picked by the port, whatever it is called.
  dk_green dk_port 'postgresql://gate:pw0@127.0.0.1:5489/brain' 5489 memory-industry-dbwin-20261012 pw0
  # The port is read the way libpq reads it.
  dk_green dk_ipv6 'postgresql://gate:pw0@[::1]:5434/brain' 5434 c6 pw0
  dk_green dk_default_port 'postgresql://gate:pw0@localhost/brain' 5432 c4 pw0
  dk_green dk_pgport 'postgresql://gate:pw0@localhost/brain' 5433 c5 pw0 PGPORT=5433

  # The name the gate used to trust, ready, on another port: nothing publishes
  # :5489, so the gate says that and asks no container.
  dk_prepare dk_name_trap
  dk_go dk_name_trap 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PORT=5488 \
    FAKE_DOCKER_NAMES=memory-industry-db FAKE_DOCKER_HEALTHY="memory-industry-db cuba-memorys-db"
  pg_stopped dk_name_trap 1 pw0
  grep -qF 'no running container publishes :5489' "$tmp/dk_name_trap.out" \
    || self_fail "dk_name_trap: the gate did not say that no running container publishes :5489: $(cat "$tmp/dk_name_trap.out")"
  if grep -q '^exec ' <<<"$(docker_args dk_name_trap)"; then
    self_fail "dk_name_trap: a container was asked by its name: $(docker_args dk_name_trap)"
  fi

  # Two containers publish the port: the gate does not choose, it names both.
  dk_prepare dk_two
  dk_go dk_two 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PORT=5489 \
    FAKE_DOCKER_NAMES="pgone pgtwo" FAKE_DOCKER_HEALTHY="pgone pgtwo"
  dk_refused dk_two 2 pw0 pgone pgtwo

  # The one container is there and its Postgres does not answer.
  dk_prepare dk_not_ready
  dk_go dk_not_ready 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PORT=5489 \
    FAKE_DOCKER_NAMES=pgdown FAKE_DOCKER_HEALTHY=
  pg_stopped dk_not_ready 1 pw0
  fail_line_with dk_not_ready ':5489' pgdown \
    || self_fail "dk_not_ready: no FAIL line names :5489 and the container: $(cat "$tmp/dk_not_ready.out")"

  # A server that is not this machine, or a socket, is no container's to judge:
  # pg_isready is needed, and docker is not even asked.
  dk_prepare dk_remote
  dk_go dk_remote 'postgresql://gate:pw0@db.example.invalid:5489/brain'
  dk_refused dk_remote 2 pw0 pg_isready
  [[ ! -e "$tmp/dk_remote/home/docker.args" ]] \
    || self_fail "dk_remote: docker was asked about a server that is not this machine: $(docker_args dk_remote)"
  dk_prepare dk_socket
  dk_go dk_socket 'postgresql://gate@%2Frun%2Fpostgresql:5433/brain'
  dk_refused dk_socket 2 '' pg_isready
  [[ ! -e "$tmp/dk_socket/home/docker.args" ]] \
    || self_fail "dk_socket: docker was asked about a socket: $(docker_args dk_socket)"

  # No pg_isready and no docker: the refusal names both.
  dk_prepare dk_no_docker
  rm -f "$tmp/dk_no_docker/bin/docker"
  dk_go dk_no_docker 'postgresql://gate:pw0@127.0.0.1:5489/brain'
  dk_refused dk_no_docker 2 pw0 pg_isready docker

  # docker is there and does not answer: that is not "no container".
  dk_prepare dk_daemon_down
  dk_go dk_daemon_down 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PS_EXIT=1 \
    FAKE_DOCKER_PORT=5489 FAKE_DOCKER_NAMES=pgone FAKE_DOCKER_HEALTHY=pgone
  dk_refused dk_daemon_down 2 pw0 docker

  # gate_linux_env (gate-lock.sh): what a gate run on Linux hands to cargo, to the
  # mutation tool and to node. Each row runs it in a fresh bash with an empty
  # environment but HOME and a PATH of stand-ins, so every answer comes from the
  # function. uname, stat and node are stand-ins whose output the row picks
  # (FAKE_UNAME, FAKE_FS, FAKE_NODE); env/bare has the same without node.
  mkdir -p "$tmp/env/scripts" "$tmp/env/bin" "$tmp/env/bare" "$tmp/env/home"
  cp "$ROOT/scripts/gate-lock.sh" "$tmp/env/scripts/"
  printf '{"engines":{"node":">=18"}}\n' >"$tmp/env/package.json"
  printf '#!/bin/sh\necho "${FAKE_UNAME:-Linux}"\n' >"$tmp/env/bin/uname"
  printf '#!/bin/sh\necho "${FAKE_FS:-ext2/ext3}"\n' >"$tmp/env/bin/stat"
  printf '#!/bin/sh\necho "${FAKE_NODE:-v20.0.0}"\n' >"$tmp/env/bin/node"
  chmod +x "$tmp"/env/bin/*
  cp "$tmp/env/bin/uname" "$tmp/env/bin/stat" "$tmp/env/bare/"
  # Wrappers, not links: on Git Bash, without MSYS=winsymlinks, `ln -s` COPIES
  # the .exe, and an MSYS binary copied outside /usr/bin cannot find
  # msys-2.0.dll. dirname died, GATE_REPO_ROOT ended up as `/`, and the row
  # without node was refused for "//package.json" instead of for node (measured
  # on Windows, 2026-10-12). A PATH of the real directories would not do either:
  # on Linux /usr/bin may hold a node.
  for tool in dirname sed head mkdir; do
    path="$(command -v "$tool")"
    [[ "$path" == /* ]] || self_fail "env/bare needs $tool as a program on PATH, and command -v said '$path'"
    printf '#!/bin/sh\nexec "%s" "$@"\n' "$path" >"$tmp/env/bare/$tool"
  done
  chmod +x "$tmp"/env/bare/*
  # env_run NAME [VAR=value ...]: $rc is the exit code of gate_linux_env, NAME.out
  # what it printed and then the two variables it leaves behind.
  env_run() {
    local n="$1"
    shift
    rc=0
    env -i HOME="$tmp/env/home" PATH="$tmp/env/bin:/usr/bin:/bin" "$@" "$BASH" -c \
      'source "$1" && gate_linux_env && printf "CARGO_TARGET_DIR=%s\nTMPDIR=%s\n" "${CARGO_TARGET_DIR-unset}" "${TMPDIR-unset}"' \
      _ "$tmp/env/scripts/gate-lock.sh" >"$tmp/$n.out" 2>&1 || rc=$?
  }
  env_var() { sed -n "s/^$2=//p" "$tmp/$1.out"; }

  # The cargo target: left as it is when set, else absolute and outside the tree.
  env_run target_default
  (( rc == 0 )) || self_fail "gate_linux_env on Linux exited $rc: $(cat "$tmp/target_default.out")"
  [[ "$(env_var target_default CARGO_TARGET_DIR)" == "$tmp/env/home/.cache/cargo-target/memory-industry" ]] \
    || self_fail "with CARGO_TARGET_DIR unset the target was '$(env_var target_default CARGO_TARGET_DIR)', not under HOME/.cache/cargo-target"
  env_run target_xdg XDG_CACHE_HOME="$tmp/env/xdg-target"
  [[ "$(env_var target_xdg CARGO_TARGET_DIR)" == "$tmp/env/xdg-target/cargo-target/memory-industry" ]] \
    || self_fail "with XDG_CACHE_HOME set the target was '$(env_var target_xdg CARGO_TARGET_DIR)', not under it"
  env_run target_kept CARGO_TARGET_DIR="$tmp/env/mine"
  [[ "$(env_var target_kept CARGO_TARGET_DIR)" == "$tmp/env/mine" ]] \
    || self_fail "a CARGO_TARGET_DIR somebody set was changed to '$(env_var target_kept CARGO_TARGET_DIR)'"

  # TMPDIR: off tmpfs, because on this machine tmpfs is RAM and the mutation tool
  # copies the tree into it. Kept when set to something that is not tmpfs.
  rm -rf "$tmp/env/home/.cache"
  env_run tmp_default
  [[ "$(env_var tmp_default TMPDIR)" == "$tmp/env/home/.cache/memory-industry-gate/tmp" ]] \
    || self_fail "with TMPDIR unset it was '$(env_var tmp_default TMPDIR)', not under HOME/.cache/memory-industry-gate"
  [[ -d "$tmp/env/home/.cache/memory-industry-gate/tmp" ]] \
    || self_fail "gate_linux_env named a TMPDIR it never created"
  env_run tmp_xdg XDG_CACHE_HOME="$tmp/env/xdg-tmp"
  [[ "$(env_var tmp_xdg TMPDIR)" == "$tmp/env/xdg-tmp/memory-industry-gate/tmp" && -d "$tmp/env/xdg-tmp/memory-industry-gate/tmp" ]] \
    || self_fail "with XDG_CACHE_HOME set TMPDIR was '$(env_var tmp_xdg TMPDIR)', not a directory under it"
  mkdir -p "$tmp/env/disk" "$tmp/env/ram"
  env_run tmp_disk TMPDIR="$tmp/env/disk" FAKE_FS=ext2/ext3
  [[ "$(env_var tmp_disk TMPDIR)" == "$tmp/env/disk" ]] \
    || self_fail "a TMPDIR on a disk was changed to '$(env_var tmp_disk TMPDIR)'"
  env_run tmp_ram TMPDIR="$tmp/env/ram" FAKE_FS=tmpfs
  [[ "$(env_var tmp_ram TMPDIR)" == "$tmp/env/home/.cache/memory-industry-gate/tmp" ]] \
    || self_fail "a TMPDIR on tmpfs was left as '$(env_var tmp_ram TMPDIR)', which is RAM here"

  # node: engines.node of package.json is the oldest accepted, and a gate that
  # cannot say which node it ran with refuses with 2 and says how to fix it.
  env_run node_edge FAKE_NODE=v18.0.0
  (( rc == 0 )) || self_fail "node v18.0.0, the oldest engines.node allows, was refused (exit $rc): $(cat "$tmp/node_edge.out")"
  env_run node_absent PATH="$tmp/env/bare"
  (( rc == 2 )) || self_fail "gate_linux_env went on (exit $rc) with no node on PATH: $(cat "$tmp/node_absent.out")"
  grep -q 'node' "$tmp/node_absent.out" && grep -q 'nvm use' "$tmp/node_absent.out" \
    || self_fail "the refusal for a missing node did not say what is missing and how to fix it: $(cat "$tmp/node_absent.out")"
  env_run node_old FAKE_NODE=v16.0.0
  (( rc == 2 )) || self_fail "gate_linux_env went on (exit $rc) with node v16.0.0: $(cat "$tmp/node_old.out")"
  grep -q 'v16.0.0' "$tmp/node_old.out" && grep -q '18' "$tmp/node_old.out" && grep -q 'nvm use' "$tmp/node_old.out" \
    || self_fail "the refusal for an old node did not name its version, the one required and the fix: $(cat "$tmp/node_old.out")"
  env_run node_garbage FAKE_NODE=nonsense
  (( rc == 2 )) || self_fail "gate_linux_env went on (exit $rc) with a node whose version it cannot read: $(cat "$tmp/node_garbage.out")"

  # Not Linux: it does nothing, not even ask for node.
  env_run not_linux FAKE_UNAME=Darwin PATH="$tmp/env/bare"
  (( rc == 0 )) || self_fail "gate_linux_env refused a machine that is not Linux (exit $rc): $(cat "$tmp/not_linux.out")"
  [[ "$(env_var not_linux CARGO_TARGET_DIR)" == unset && "$(env_var not_linux TMPDIR)" == unset ]] \
    || self_fail "gate_linux_env set variables on a machine that is not Linux: $(cat "$tmp/not_linux.out")"

  echo "OK  self-test: the exit file says running while a gate runs and holds the whole"
  echo "    gate's verdict at the end, a step after the tests included; a gate killed"
  echo "    in the middle leaves 'running', never an old 0; a refused gate leaves it alone."
  echo "    A green gate over a clean tree leaves a receipt for its commit; an untracked"
  echo "    file, a tree or HEAD that changed during the run and an exit 0 over SKIPPED"
  echo "    leave none and say why; a red run on a commit removes its earlier receipt."
  echo "    gate_linux_env keeps a CARGO_TARGET_DIR that was set and otherwise puts it"
  echo "    outside the tree, moves a TMPDIR that is unset or tmpfs under the cache, keeps"
  echo "    one on a disk, refuses with 2 a node that is missing, older than engines.node"
  echo "    or unreadable, and does nothing off Linux. In the gate itself"
  echo "    the readiness check hands DATABASE_URL to pg_isready -d as it is, names the"
  echo "    port libpq answered on, stops with 1 when nothing answers and with 2 when"
  echo "    libpq cannot read the URL or its answer has no port, never prints what libpq"
  echo "    wrote about the URL, and refuses with 2, before asking, a URL that"
  echo "    brain_gate cannot be derived from"
  echo "    or that holds more than one '@' before the host."
  echo "    cargo audit runs first, before cargo deny and before the tests, under timeout 120;"
  echo "    a database it cannot refresh is exit 2, never a skip; without pg_isready the"
  echo "    readiness check asks the container that publishes the port of DATABASE_URL, never one by name."
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
echo "  · network to github.com cargo audit refreshes the RustSec advisory database first, at most 120 s"
echo "  · node                  >= engines.node of package.json, on PATH (nvm use)"
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

# The cargo target and TMPDIR outside the tree and off tmpfs, and a node that
# package.json accepts, before any step needs them (gate-lock.sh says why). It
# runs after the lock so that a second gate is refused first, and a refusal here
# is this gate's verdict: exit 2, in the exit file, with the lock released.
gate_linux_env

export DATABASE_URL="${DATABASE_URL:-postgresql://cuba:memorys2026@127.0.0.1:5488/brain}"
# A shape run-all-tests.sh cannot derive brain_gate from is refused here, before
# libpq is asked, for the same reason gate-lock.sh gives: it would judge one
# server ready and create the throwaway databases on another.
gate_require_database_url "$DATABASE_URL"

# Without pg_isready the server is asked through the container that publishes the
# port DATABASE_URL names, not through one picked by name: the name says nothing
# about which server the URL points at (the judge's container on Windows was
# memory-industry-dbwin-20261012 on :5489, and a memory-industry-db on another
# port would have been judged ready in its place). publish= matches the host
# port, not the container's (measured on Docker 29.7.2), and the role and the
# database do not change what pg_isready answers, so it is asked with neither.
# Nothing printed carries DATABASE_URL: the password lives in it.
gate_container_ready() {
  local authority hostport host port="" names name running=() answer rc=0
  authority="${1#*://}"
  authority="${authority%%/*}"
  hostport="${authority##*@}"
  if [[ "$hostport" == \[*\]* ]]; then
    host="${hostport%%]*}]"
    [[ "${hostport#*]}" == :* ]] && port="${hostport#*]:}"
  else
    host="${hostport%%:*}"
    [[ "$hostport" == *:* ]] && port="${hostport##*:}"
  fi
  port="${port:-${PGPORT:-5432}}"
  [[ "$port" =~ ^[0-9]+$ ]] \
    || gate_refuse "DATABASE_URL (or PGPORT) names a port that is not a number, so no container can be asked for it."
  case "$host" in
    127.0.0.1|localhost|"[::1]") ;;
    *) gate_refuse "pg_isready is not on PATH, and DATABASE_URL names a server that is not this machine, which no local container can answer for. Install the PostgreSQL client tools so pg_isready exists." ;;
  esac
  command -v docker >/dev/null 2>&1 \
    || gate_refuse "pg_isready and docker are both missing from PATH, so nothing can say whether the Postgres on :$port is ready. Install the PostgreSQL client tools (pg_isready) or docker."
  names="$(docker ps --filter "publish=$port" --format '{{.Names}}')" \
    || gate_refuse "docker did not answer 'docker ps', so it cannot say which container publishes :$port. Start docker or install pg_isready."
  while IFS= read -r name; do
    name="${name%$'\r'}"
    [[ -z "$name" ]] || running+=("$name")
  done <<<"$names"
  if (( ${#running[@]} == 0 )); then
    echo "FAIL: Postgres not ready on :$port (no pg_isready on PATH, and no running container publishes :$port)"
    exit 1
  elif (( ${#running[@]} > 1 )); then
    gate_refuse "more than one container publishes :$port (${running[*]}), so this gate will not choose one to judge. Stop the others or install pg_isready."
  fi
  name="${running[0]}"
  answer="$(docker exec "$name" pg_isready 2>&1)" || rc=$?
  answer="${answer//$'\r'/}"
  if (( rc != 0 )); then
    echo "FAIL: Postgres not ready on :$port (container $name: $answer)"
    exit 1
  fi
  echo "OK  Postgres :$port (container $name)"
}

if command -v pg_isready >/dev/null 2>&1; then
  # libpq reads the URL, so host, port, socket, IPv6 and PGPORT mean here what
  # they mean to psql, pg_dump and sqlx, and the port printed is the one libpq
  # went to ("<host>:<port> - <status>"; the port is after the LAST colon, for
  # IPv6). Its stderr is dropped: libpq quotes the piece of the URL it cannot
  # read, and that piece can be the password.
  pg_rc=0
  pg_answer="$(pg_isready -d "$DATABASE_URL" 2>/dev/null)" || pg_rc=$?
  if (( pg_rc == 3 )); then
    gate_refuse "pg_isready could not read DATABASE_URL, so no server was asked. Run pg_isready -d \"\$DATABASE_URL\" by hand to see libpq's own words (they can quote the password, so not in this log)."
  fi
  pg_port="${pg_answer% - *}"
  pg_port="${pg_port##*:}"
  [[ "$pg_answer" == *" - "* && "$pg_port" =~ ^[0-9]+$ ]] \
    || gate_refuse "pg_isready answered '$pg_answer', which names no port, so this gate cannot say which server it asked. It will not guess one."
  (( pg_rc == 0 )) \
    || { echo "FAIL: Postgres not ready on :$pg_port (pg_isready: $pg_answer)"; exit 1; }
  echo "OK  Postgres :$pg_port"
else
  gate_container_ready "$DATABASE_URL"
fi

# The advisories are judged before the 20-minute test run: a judge idle for days
# found its new advisory half an hour in, when 3 s would have said it. cargo audit
# is the only judge of them (cargo deny below reads licenses, bans and sources,
# never `advisories`): two judges would keep two ignore lists, and they already
# differ. It fetches its database itself, so the ceiling is on the whole run;
# measured here, 2.4 s with the database current, 11.6 s from a cold clone, and
# 20.2 s to give up on a server that does not answer. A database that could not
# be refreshed is exit 2, never a pass against the copy on disk and never a retry
# with --no-fetch or --stale.
echo "=== cargo audit ==="
audit_rc=0
audit_out="$(cd "$ROOT/rust" && timeout 120 cargo audit 2>&1)" || audit_rc=$?
printf '%s\n' "$audit_out"
if (( audit_rc == 124 )); then
  gate_refuse "cargo audit did not finish refreshing the advisory database from https://github.com/RustSec/advisory-db in 120 s. This gate does not judge advisories against the copy on disk; restore the network to github.com and run it again."
elif (( audit_rc != 0 )) && [[ "$audit_out" == *"couldn't fetch advisory database"* ]]; then
  gate_refuse "cargo audit could not fetch the advisory database from https://github.com/RustSec/advisory-db. This gate does not judge advisories against the copy on disk; restore the network to github.com and run it again."
elif (( audit_rc != 0 )); then
  echo "FAIL: cargo audit exited $audit_rc (its output is above; with no advisory listed, check that timeout is GNU coreutils)"
  exit 1
fi

echo "=== cargo deny ==="
if ! command -v cargo-deny >/dev/null 2>&1 && ! (cd "$ROOT/rust" && cargo deny --version >/dev/null 2>&1); then
  echo "FAIL: cargo-deny is not installed. Install it (cargo install cargo-deny) — the gate does not skip license policy." >&2
  exit 1
fi
(cd "$ROOT/rust" && cargo deny check licenses bans sources)

if [[ "${SKIP_BACKUP:-0}" != "1" ]]; then
  "$ROOT/scripts/backup-db.sh"
  echo "OK  Database backup"
fi

"$ROOT/scripts/run-all-tests.sh"

echo "=== cargo clippy/test --features docs ==="
(cd "$ROOT/rust" && cargo clippy --all-targets --features docs -- -D warnings)
(cd "$ROOT/rust" && cargo test --features docs)

echo "=== npm wrapper smoke ==="
node npm/install.test.js

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
