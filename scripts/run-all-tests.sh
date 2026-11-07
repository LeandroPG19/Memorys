#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT/rust"
export CUBA_JUDGE="${CUBA_JUDGE:-heuristic}"

# Every path this gate hands to another process is absolute, and the resolution
# happens here, once, before anything derives anything from it.
#
# A relative path survives every check that can be made from bash: `[[ -x ]]`
# says yes, and so does Python's os.path.exists(). It then fails inside
# subprocess.run() on Windows with `[WinError 2] cannot find the file`, forty
# subprocesses later, in a message that names neither the path nor the cause.
# Measured on this tree with CARGO_TARGET_DIR=rust/target-sil: all 25 E2E tool
# calls died that way while the binary sat exactly where the banner said.
abs_path() {
  local path="$1" base="$2"
  case "$path" in
    /*|[A-Za-z]:/*|[A-Za-z]:\\*) printf '%s\n' "$path" ;;
    *) printf '%s\n' "$base/$path" ;;
  esac
}

# cargo resolves CARGO_TARGET_DIR against the cwd of each process that reads
# it, and every cargo call in this gate runs after a `cd` into rust/
# (merge-gate.sh, this script, crap-gate.sh, mutants-gate.sh). A relative value
# therefore names two directories at once: cargo links into rust/rust/<value>
# while every consumer reading the variable from the repository root looks in
# <root>/<value>. Both existed on this machine, and the second one still held a
# binary from an earlier run — the E2E was one step away from validating a
# stale build in silence. The base is $ROOT and not the cwd of the moment,
# because the cwd of the moment is the trap.
if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  absolute_target_dir="$(abs_path "$CARGO_TARGET_DIR" "$ROOT")"
  if [[ "$absolute_target_dir" != "$CARGO_TARGET_DIR" ]]; then
    echo "note: CARGO_TARGET_DIR=$CARGO_TARGET_DIR is relative. This run uses" >&2
    echo "      $absolute_target_dir, so cargo, bash and Python all mean the same" >&2
    echo "      directory. Left relative, cargo would build in $ROOT/rust/$CARGO_TARGET_DIR" >&2
    echo "      and everything else would read $ROOT/$CARGO_TARGET_DIR." >&2
    export CARGO_TARGET_DIR="$absolute_target_dir"
  fi
fi

# Honor CARGO_TARGET_DIR (Cursor sandbox points it off-tree).
gate_target_dir() {
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$RUST_DIR/target"
  fi
}

# The release binary the E2E drives, absolute whatever CUBA_BINARY_PATH held.
# The base for a caller-supplied relative path is $RUST_DIR, which is the cwd
# the E2E subprocess itself runs with.
resolve_binary_path() {
  local cand td
  cand="${CUBA_BINARY_PATH:-}"
  if [[ -z "$cand" ]]; then
    td="$(gate_target_dir)"
    cand="$td/release/memory-industry"
  fi
  cand="$(abs_path "$cand" "$RUST_DIR")"
  # Prefer .exe even when Git Bash treats the extensionless name as existing
  # (PATHEXT). Native Python Path.is_file() does not.
  if [[ -f "${cand}.exe" ]]; then
    printf '%s\n' "${cand}.exe"
    return 0
  fi
  if [[ -f "$cand" ]]; then
    printf '%s\n' "$cand"
    return 0
  fi
  td="$(gate_target_dir)"
  cand="$(abs_path "$td/release/cuba-memorys" "$RUST_DIR")"
  if [[ -f "${cand}.exe" ]]; then
    printf '%s\n' "${cand}.exe"
    return 0
  fi
  printf '%s\n' "$cand"
}

# Answering this costs nothing and runs nothing, so a contract can drive the
# real script with a relative CARGO_TARGET_DIR and read back what it resolved.
# The test that was supposed to catch the bug above only asserted that this
# file contains the string "CARGO_TARGET_DIR", which it did throughout.
if [[ "${1:-}" == "--print-paths" ]]; then
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR:-$RUST_DIR/target}"
  printf 'target_dir=%s\n' "$(gate_target_dir)"
  printf 'binary=%s\n' "$(resolve_binary_path)"
  exit 0
fi

# B3g (CI-F1, D-F3b): judges the JSON of `cargo build --release --features cuda --bins
# --message-format=json`. Every compiler-artifact has to say fresh: a unit that compiled
# again is a binary the --no-run build did not leave. Only reads the file.
check_fresh() {
  local file="$1" line name units=0 stale=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" == *'"reason":"compiler-artifact"'* ]] || continue
    units=$((units + 1))
    if [[ "$line" != *'"fresh":true'* ]]; then
      stale=$((stale + 1))
      name="$(printf '%s\n' "$line" | sed -n 's/.*"target":{"name":"\([^"]*\)".*/\1/p')"
      echo "FAIL: B3g: ${name:-a unit} recompiled after the --no-run build (its compiler-artifact is not fresh), so the binary the E2E would judge is not the one the release build made." >&2
    fi
  done <"$file"
  if (( units == 0 )); then
    echo "FAIL: B3g: $file holds no compiler-artifact, so nothing says the bins are fresh." >&2
    return 1
  fi
  (( stale == 0 ))
}
if [[ "${1:-}" == "--check-fresh" ]]; then
  check_fresh "${2:?--check-fresh needs the JSON file}"
  exit 0
fi

# The eval judgement runs on the interpreter the gate uses everywhere else: the Windows
# WindowsApps stub is skipped. Resolved here, before the mode, so that --check-eval works on its own.
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

# The eval judgement (CI-J2, A4, A5, D-J3c, D-J3d): the JSON of `eval --json` against the lines
# of smoke.jsonl. Each key is read by its path under metrics, so a per_ability block in front of
# it (with its own ndcg_at_k) does not stand for the eval. Green: the scored queries
# (metrics.per_query_ndcg) and the loaded ones (metrics.sample_count) are the query lines, and
# metrics.ndcg_at_k is a JSON number above 0. Reads the file only, no database: 0 green, 1 red,
# 2 arguments it cannot use. The interpreter is $PY, resolved above (the Windows judge too).
check_eval() {
  local json="$1" lines="$2" judged scored sample ndcg
  [[ "$lines" =~ ^[1-9][0-9]*$ ]] \
    || { echo "FAIL: --check-eval wants the lines of smoke.jsonl as a whole number above 0 (got '$lines')." >&2; return 2; }
  judged="$("$PY" -I -c 'import json, sys
try:
    m = json.load(open(sys.argv[1], encoding="utf-8")).get("metrics", {})
except (OSError, ValueError, AttributeError):
    print("unreadable\tunreadable\tunreadable"); sys.exit(0)
def show(key):
    if key not in m:
        return "missing"
    v = m[key]
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return "bad"
    return repr(v)
q = m.get("per_query_ndcg")
scored = "missing" if q is None else (str(len(q)) if isinstance(q, list) else "bad")
print("\t".join([scored, show("sample_count"), show("ndcg_at_k")]))' "$json" 2>/dev/null)" \
    || judged="unreadable	unreadable	unreadable"
  IFS=$'\t' read -r scored sample ndcg <<<"$judged"
  if [[ "$scored" == unreadable ]]; then
    echo "FAIL: the eval --json cannot be read as JSON, so its metrics cannot be counted against smoke.jsonl ($lines lines)." >&2
    return 1
  fi
  if ! [[ "$scored" =~ ^[0-9]+$ ]]; then
    echo "FAIL: the eval --json has no per_query_ndcg list, so its scored queries cannot be counted ($lines lines in smoke.jsonl)." >&2
    return 1
  fi
  if (( scored != lines )); then
    echo "FAIL: the eval scored $scored queries (metrics.per_query_ndcg) and smoke.jsonl has $lines lines." >&2
    return 1
  fi
  if ! [[ "$sample" =~ ^[0-9]+$ ]] || (( sample != lines )); then
    echo "FAIL: the eval --json reports ${sample} queries loaded (metrics.sample_count) and smoke.jsonl has $lines lines." >&2
    return 1
  fi
  if ! [[ "$ndcg" =~ ^[0-9.eE+-]+$ ]] || ! awk -v x="$ndcg" 'BEGIN { exit !(x + 0 > 0) }'; then
    echo "FAIL: the eval nDCG (metrics.ndcg_at_k) is ${ndcg} over $lines queries: nothing was retrieved for the queries it scored." >&2
    return 1
  fi
  echo "OK  eval smoke: nDCG@10=$ndcg over $lines queries (metrics.per_query_ndcg, metrics.sample_count)"
}
if [[ "${1:-}" == "--check-eval" ]]; then
  [[ $# -eq 3 ]] || { echo "FAIL: --check-eval takes the eval --json file and the lines of smoke.jsonl: --check-eval JSON QUERIES_LINES" >&2; exit 2; }
  check_eval "$2" "$3"
  exit $?
fi

# One gate at a time: the lock, its owner record and the rules for a dead or
# foreign holder are in gate-lock.sh, which merge-gate.sh sources too so that
# it can hold the lock for its whole run. What stays here is the half that is
# about databases: every one this run creates carries GATE_OWNER as its
# COMMENT, and the exit drops only those still carrying it.
# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"
# shellcheck source=scripts/gate-cluster.sh
source "$ROOT/scripts/gate-cluster.sh"
GATE_OWNED_DBS=()

# The admin connection and psql_url are defined further down; these three only
# run after them. They are the whole of what the ownership rule says to the
# server, which is also what lets the self-test put a catalog in their place.
db_owner() {
  psql_url "$ADMIN_DATABASE_URL" -Atc \
    "SELECT coalesce(shobj_description(oid, 'pg_database'), '') FROM pg_database WHERE datname = '$1'"
}

db_drop() {
  psql_url "$ADMIN_DATABASE_URL" -q -c "DROP DATABASE IF EXISTS $1 WITH (FORCE)" >/dev/null
}

db_create_owned() {
  psql_url "$ADMIN_DATABASE_URL" -q \
    -c "CREATE DATABASE $1" \
    -c "COMMENT ON DATABASE $1 IS '${GATE_OWNER//\'/\'\'}'" >/dev/null
}

# A database with no record, or one whose creator is gone, is left over and
# gets dropped: that is every database made by a version of this script older
# than the rule, and the one an orphan never got to clean up.
claim_database() {
  local db="$1" holder verdict=1
  holder="$(db_owner "$db")"
  if [[ "$holder" == *token=* ]]; then
    verdict=0
    owner_alive "$holder" || verdict=$?
  fi
  if (( verdict == 0 )); then
    echo "FAIL: database $db belongs to a gate that is still running: $(describe_owner "$holder")" >&2
    echo "      Dropping it would do to that run exactly what an orphan did on 2026-09-22." >&2
    return 1
  fi
  if (( verdict == 2 )); then
    echo "FAIL: database $db was created by a gate on $(record_field where "$holder")," >&2
    echo "      which cannot be looked up from $(gate_where). If it is not running," >&2
    echo "      drop $db by hand." >&2
    return 1
  fi
  # Listed before it exists: release checks the comment before it drops, so
  # a name that never got created, or got taken, costs nothing.
  GATE_OWNED_DBS+=("$db")
  db_drop "$db"
  db_create_owned "$db"
}

release_owned_databases() {
  local db holder
  for db in "${GATE_OWNED_DBS[@]}"; do
    holder="$(db_owner "$db" 2>/dev/null || true)"
    if [[ "$holder" == "$GATE_OWNER" ]]; then
      db_drop "$db" 2>/dev/null || true
    elif [[ -n "$holder" ]]; then
      echo "note: left $db in place: it now belongs to $(describe_owner "$holder"), not to this run." >&2
    fi
  done
  return 0
}

# $? is read before anything else can overwrite it. The exit file is written
# only by a run that took the lock itself (gate-lock.sh says why): under
# merge-gate.sh this is the first half of a gate, and the verdict is the
# parent's to write.
on_exit() {
  local code=$?
  gate_cluster_down_all
  [[ -z "$GATE_LOCK_HELD" ]] || exit_file_verdict "$code"
  release_owned_databases
  release_gate_lock
}

# --- self-test: each guard above gets the fixture that has to stop it ---------
# Live and dead owners are real processes (a `sleep`), not made-up pids, so
# the liveness check runs against /proc exactly as it does in a gate. The
# database half runs against a catalog kept in this shell in place of the three
# db_* functions: nothing here connects to PostgreSQL. The last fixture runs
# this script for real, as a second gate, against a lock held by a live
# process, with HOME in a scratch directory and cargo and psql off PATH, so a
# lock that failed to refuse would stop at "psql is not on PATH" instead of
# building anything.
#   ./scripts/run-all-tests.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  # CI-J3: the judge hands its children these names (JUDGE_ENVIRONMENT in local_gate_contract.rs).
  # The rows here read the ones that change their answer, so this self-test starts without them:
  # a row that needs one sets it itself.
  # CI-F2 (D-F2i): the lane of a step is not inherited either (JUDGE_ENVIRONMENT).
  unset GATE_SERVER_SYSTEM_ID GATE_LANE CUBA_BINARY_PATH ONNX_MODEL_PATH CUBA_RERANKER_PATH CUBA_NLI_PATH \
    CUBA_EMBEDDING_DIM
  # D-J7b (A2): this self-test is hermetic. HOME and XDG_CACHE_HOME are a directory of its own,
  # removed on exit, so a row that does not give them one never reaches the real cache.
  hermetic="$(mktemp -d)"
  mkdir -p "$hermetic/home" "$hermetic/cache"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache"
  tmp="$(mktemp -d)"
  sleep 300 &
  live=$!
  trap 'kill "$live" 2>/dev/null || true; rm -rf "$tmp" "$hermetic"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }
  # The judge's test URL: a string that only the psql stand-in reads. Its marker
  # and identity come from that stand-in, never from a server.
  rat_url='postgresql://gate:pw0@127.0.0.1:5489/brain'
  lock="$tmp/lock"
  live_record="$(owner_record "$live" fixture-live)"
  [[ -n "$(record_field start "$live_record")" ]] \
    || self_fail "no start time for a live process in /proc/$live/stat; nothing below can be judged"

  acquire_gate_lock "$lock" || self_fail "a free lock was refused"
  [[ "$(cat "$lock/owner")" == "$GATE_OWNER" ]] || self_fail "the lock does not name the run that took it"
  release_gate_lock
  [[ ! -e "$lock" ]] || self_fail "the run that held the lock did not release it"

  mkdir -p "$lock"
  printf '%s\n' "$live_record" >"$lock/owner"
  if acquire_gate_lock "$lock" 2>"$tmp/refused"; then
    self_fail "a second run took the lock of a live gate"
  fi
  grep -q "another gate is running: pid $live, running since " "$tmp/refused" \
    || self_fail "the refusal does not name the live gate: $(cat "$tmp/refused")"
  [[ "$(cat "$lock/owner")" == "$live_record" ]] || self_fail "a refused run changed the live gate's lock"
  [[ -z "$(compgen -G "$lock.new.*" || true)" ]] || self_fail "a refused run left its staging directory behind"

  GATE_LOCK_HELD="$lock"
  release_gate_lock 2>/dev/null
  [[ -e "$lock" ]] || self_fail "a run released a lock that another gate holds"
  GATE_LOCK_HELD=""

  sleep 300 &
  dead=$!
  printf '%s\n' "$(owner_record "$dead" fixture-dead)" >"$lock/owner"
  kill "$dead"
  wait "$dead" 2>/dev/null || true
  acquire_gate_lock "$lock" >/dev/null || self_fail "the lock of a dead gate blocked the next one"
  [[ "$(cat "$lock/owner")" == "$GATE_OWNER" ]] || self_fail "a dead gate's lock was not taken over"
  release_gate_lock

  mkdir -p "$lock"
  sed 's/ start=[^ ]*/ start=1/' <<<"$live_record" >"$lock/owner"
  acquire_gate_lock "$lock" >/dev/null \
    || self_fail "a lock whose pid was reused by another process blocked the next gate"
  release_gate_lock

  # Another machine's record, on a pid and start time that are dead here: read
  # as if it were local, it would look gone and be taken over.
  mkdir -p "$lock"
  sed 's/ start=[^ ]*/ start=1/; s/ where=[^ ]*/ where=another-host:\//' <<<"$live_record" >"$lock/owner"
  if acquire_gate_lock "$lock" 2>/dev/null; then
    self_fail "a lock from another machine was taken over without being judged"
  fi
  rm -rf "$lock"

  # The database half. `drops` is every name the ownership rule let go of.
  declare -A catalog=()
  drops=()
  db_owner() { printf '%s\n' "${catalog[$1]:-}"; }
  db_drop() { drops+=("$1"); unset "catalog[$1]"; }
  db_create_owned() { catalog[$1]="$GATE_OWNER"; }
  GATE_OWNER="$(owner_record "$$" fixture-self)"

  GATE_OWNED_DBS=()
  catalog[brain_gate]="$live_record"
  release_owned_databases
  (( ${#drops[@]} == 0 )) || self_fail "an exit that created nothing dropped ${drops[*]}"

  if claim_database brain_gate 2>/dev/null; then
    self_fail "provisioning took a database whose creator is still running"
  fi
  (( ${#drops[@]} == 0 )) || self_fail "a refused claim still dropped ${drops[*]}"

  catalog[brain_gate]="pid=999999 start=1 where=$(gate_where) since=x token=gone"
  catalog[brain_gate_peer]=""
  claim_database brain_gate || self_fail "a database left by a dead gate blocked provisioning"
  claim_database brain_gate_peer || self_fail "a database with no record blocked provisioning"
  [[ "${drops[*]}" == "brain_gate brain_gate_peer" ]] \
    || self_fail "leftover databases were not dropped before being created (dropped: ${drops[*]})"
  [[ "${catalog[brain_gate]}" == "$GATE_OWNER" ]] || self_fail "a created database does not carry its creator's record"

  # The incident itself: this run created brain_gate, then another gate took
  # it over. This run's exit must leave that one's database alone and still
  # drop the one that is still its own.
  drops=()
  catalog[brain_gate]="$live_record"
  release_owned_databases 2>/dev/null
  [[ "${drops[*]}" == "brain_gate_peer" ]] \
    || self_fail "an exiting run dropped '${drops[*]}', not only the database that was still its own"

  # The whole script, launched as a second gate while the first is alive. It is
  # handed the live gate's record in CUBA_GATE_LOCK_OWNER as well, the way
  # every descendant of merge-gate.sh is: the variable alone must not be a
  # pass, because the live gate is not this run's parent.
  mkdir -p "$tmp/home/.cache/cuba-gate/lock"
  printf '%s\n' "$live_record" >"$tmp/home/.cache/cuba-gate/lock/owner"
  second_exit=0
  env -u CUBA_GATE_EXIT_FILE -u DATABASE_URL HOME="$tmp/home" PATH="/usr/bin:/bin" \
      CUBA_GATE_LOCK_OWNER="$live_record" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      "$BASH" "$ROOT/scripts/run-all-tests.sh" >"$tmp/second.out" 2>&1 || second_exit=$?
  grep -q "another gate is running: pid $live" "$tmp/second.out" \
    || self_fail "a second gate did not refuse at once, naming the first (exit $second_exit): $(cat "$tmp/second.out")"
  (( second_exit == 1 )) || self_fail "a refused second gate exited $second_exit, not 1"
  [[ ! -e "$tmp/home/.cache/cuba-gate/run.exit" ]] \
    || self_fail "a refused second gate wrote the exit file that belongs to the first"

  # merge-gate.sh as the second gate: refused before it has checked, backed
  # up or built anything.
  second_exit=0
  env -u CUBA_GATE_LOCK_OWNER -u DATABASE_URL HOME="$tmp/home" PATH="/usr/bin:/bin" \
      "$BASH" "$ROOT/scripts/merge-gate.sh" >"$tmp/merge-second.out" 2>&1 || second_exit=$?
  grep -q "another gate is running: pid $live" "$tmp/merge-second.out" \
    || self_fail "a second merge-gate did not refuse at once, naming the first (exit $second_exit): $(cat "$tmp/merge-second.out")"
  (( second_exit == 1 )) || self_fail "a refused second merge-gate exited $second_exit, not 1"
  if grep -q "Postgres" "$tmp/merge-second.out"; then
    self_fail "a refused second merge-gate went on to check Postgres: $(cat "$tmp/merge-second.out")"
  fi

  # merge-gate.sh holding the lock, and the run-all-tests.sh it launches going
  # past it instead of refusing its own parent. pg_isready, psql and cargo are
  # stand-ins: the first two let both scripts reach their first cargo call,
  # and the cargo one reports, from inside the child's work, whether the lock
  # is still the one merge-gate.sh took, and what the exit file says while the
  # child works. It then fails, which ends both.
  #
  # The exit file starts at 0, as an earlier green gate would have left it.
  # The child is only merge-gate.sh's first half, so it neither clears the
  # file nor writes its own code into it: merge-gate.sh says "running" when
  # it starts and writes the whole gate's verdict when it ends.
  #
  # The rows that launch merge-gate.sh to reach this script's own steps run a
  # copy of the gate in a tree of their own, with advisory-exceptions.sh swapped
  # for a stand-in that passes: since G6 the real one judges the report of a
  # real cargo audit and refuses (exit 2) the empty one the stand-in cargo
  # leaves, so the gate would stop before reaching this script, which is what
  # these rows are about. The judge of the advisories has its own self-test.
  gate_tree() {
    local t="$tmp/$1"
    mkdir -p "$t/scripts" "$t/rust"
    cp "$ROOT/scripts/merge-gate.sh" "$ROOT/scripts/gate-lock.sh" "$ROOT/scripts/gate-cluster.sh" \
       "$ROOT/scripts/run-all-tests.sh" "$ROOT/scripts/mutants-gate.sh" "$t/scripts/"
    cp "$ROOT/package.json" "$t/"
    printf '#!/bin/sh\nexit 0\n' >"$t/scripts/advisory-exceptions.sh"
    chmod +x "$t/scripts/advisory-exceptions.sh"
  }
  gate_tree tree2
  gate_tree tree4
  # CI-F1 (1b-bis): merge-gate runs its cheap steps before run-all-tests, so this tree
  # gives them what they need: a codigo-muerto that passes, and the three models on the
  # HOME this run looks for them under, so run-all-tests --check-models passes too.
  printf '#!/bin/sh\nexit 0\n' >"$tmp/tree2/scripts/codigo-muerto.sh"
  chmod +x "$tmp/tree2/scripts/codigo-muerto.sh"
  mkdir -p "$tmp/home2/.cache/memory-industry/models" "$tmp/home2/.cache/memory-industry/reranker"
  : >"$tmp/home2/.cache/memory-industry/models/model_quantized.onnx"
  : >"$tmp/home2/.cache/memory-industry/models-nli"
  : >"$tmp/home2/.cache/memory-industry/reranker/model.onnx"
  mkdir -p "$tmp/home2/.cache/cuba-gate" "$tmp/bin"
  printf '0\n' >"$tmp/home2/.cache/cuba-gate/run.exit"
  # merge-gate.sh reads the answer of pg_isready for the port it prints, so the
  # stand-in answers the way a server on the default port does.
  printf '#!/bin/sh\necho "127.0.0.1:5489 - accepting connections"\nexit 0\n' >"$tmp/bin/pg_isready"
  printf '#!/bin/sh\necho "memory-industry-gate|7701731310526959658"\nexit 0\n' >"$tmp/bin/psql"
  # merge-gate.sh asks for a node as new as package.json says before its first step.
  printf '#!/bin/sh\necho v20.0.0\n' >"$tmp/bin/node"
  # merge-gate.sh runs cargo audit and cargo deny before this script, so those
  # two pass here and the first cargo that fails is still this script's own.
  printf '%s\n' '#!/bin/sh' \
    'case "$1" in audit|deny) exit 0;; esac' \
    'if [ "$1" = fmt ] && [ -n "$CUBA_GATE_LOCK_OWNER" ]; then n="$(cat "$HOME/fmt.calls" 2>/dev/null || echo 0)"; echo "$((n + 1))" >"$HOME/fmt.calls"; [ "$n" -eq 0 ] && exit 0; fi' \
    'if [ -n "$CUBA_GATE_LOCK_OWNER" ] && [ "$(cat "$HOME/.cache/cuba-gate/lock/owner" 2>/dev/null)" = "$CUBA_GATE_LOCK_OWNER" ]; then' \
    '  echo "stand-in cargo: the lock is still the one merge-gate took"' \
    'fi' \
    'echo "stand-in cargo: run.exit reads: $(cat "$HOME/.cache/cuba-gate/run.exit" 2>/dev/null)"' \
    'exit 1' >"$tmp/bin/cargo"
  chmod +x "$tmp/bin/pg_isready" "$tmp/bin/psql" "$tmp/bin/cargo" "$tmp/bin/node"
  held_exit=0
  env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE HOME="$tmp/home2" DATABASE_URL="$rat_url" \
      PATH="$tmp/bin:/usr/bin:/bin" SKIP_BACKUP=1 \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      "$BASH" "$tmp/tree2/scripts/merge-gate.sh" >"$tmp/merge-held.out" 2>&1 || held_exit=$?
  if grep -q "another gate is running" "$tmp/merge-held.out"; then
    self_fail "run-all-tests.sh refused the merge-gate that launched it: $(cat "$tmp/merge-held.out")"
  fi
  grep -q "running under the gate that holds the lock" "$tmp/merge-held.out" \
    || self_fail "run-all-tests.sh under merge-gate did not inherit its lock (exit $held_exit): $(cat "$tmp/merge-held.out")"
  grep -q "stand-in cargo: the lock is still the one merge-gate took" "$tmp/merge-held.out" \
    || self_fail "while run-all-tests.sh worked, the lock was not merge-gate's: $(cat "$tmp/merge-held.out")"
  (( held_exit == 1 )) || self_fail "merge-gate exited $held_exit where the stand-in cargo fails it with 1"
  [[ ! -e "$tmp/home2/.cache/cuba-gate/lock" ]] || self_fail "merge-gate exited and left its lock behind"
  if grep -q "stopped naming this run" "$tmp/merge-held.out"; then
    self_fail "the lock was released or taken from under merge-gate while it ran: $(cat "$tmp/merge-held.out")"
  fi
  grep -q "stand-in cargo: run.exit reads: running pid=[0-9]" "$tmp/merge-held.out" \
    || self_fail "while run-all-tests.sh worked under merge-gate, the exit file did not say the gate was running: $(grep 'run.exit reads' "$tmp/merge-held.out")"
  [[ "$(cat "$tmp/home2/.cache/cuba-gate/run.exit" 2>/dev/null)" == 1 ]] \
    || self_fail "merge-gate exited 1 and its exit file says '$(cat "$tmp/home2/.cache/cuba-gate/run.exit" 2>/dev/null)'"

  # The same script on its own takes the lock itself, so the exit file is its
  # own: "running" while it works, its code when it ends.
  mkdir -p "$tmp/home3/.cache/cuba-gate"
  printf '0\n' >"$tmp/home3/.cache/cuba-gate/run.exit"
  alone_exit=0
  env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE HOME="$tmp/home3" DATABASE_URL="$rat_url" \
      PATH="$tmp/bin:/usr/bin:/bin" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      "$BASH" "$ROOT/scripts/run-all-tests.sh" >"$tmp/alone.out" 2>&1 || alone_exit=$?
  (( alone_exit == 1 )) || self_fail "run-all-tests.sh alone exited $alone_exit where the stand-in cargo fails it with 1: $(cat "$tmp/alone.out")"
  grep -q "stand-in cargo: run.exit reads: running pid=[0-9]" "$tmp/alone.out" \
    || self_fail "while run-all-tests.sh ran on its own, its exit file did not say so: $(grep 'run.exit reads' "$tmp/alone.out")"
  [[ "$(cat "$tmp/home3/.cache/cuba-gate/run.exit" 2>/dev/null)" == 1 ]] \
    || self_fail "run-all-tests.sh alone exited 1 and its exit file says '$(cat "$tmp/home3/.cache/cuba-gate/run.exit" 2>/dev/null)'"

  # merge-gate.sh killed while its child works, the way memory pressure killed
  # two gates on 2026-09-23. The child finishes on its own afterwards, and its
  # exit must not write a verdict over "running": it only knows how the first
  # half went. The stand-in cargo kills the gate named in the record the child
  # inherited, leaves its own parent's pid (the child's) for the wait below,
  # and fails.
  mkdir -p "$tmp/home4/.cache/cuba-gate" "$tmp/bin4"
  printf '0\n' >"$tmp/home4/.cache/cuba-gate/run.exit"
  cp "$tmp/bin/pg_isready" "$tmp/bin/psql" "$tmp/bin/node" "$tmp/bin4/"
  printf '%s\n' '#!/bin/sh' \
    'case "$1" in audit|deny) exit 0;; esac' \
    'echo "$PPID" >"$HOME/child.pid"' \
    'kill -9 "$(printf "%s\n" "$CUBA_GATE_LOCK_OWNER" | sed -n "s/^pid=\([0-9]*\) .*/\1/p")"' \
    'exit 1' >"$tmp/bin4/cargo"
  chmod +x "$tmp/bin4/cargo"
  { env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE HOME="$tmp/home4" DATABASE_URL="$rat_url" \
      PATH="$tmp/bin4:/usr/bin:/bin" SKIP_BACKUP=1 \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      "$BASH" "$tmp/tree4/scripts/merge-gate.sh" >"$tmp/merge-killed.out" 2>&1 || true; } 2>/dev/null
  child="$(cat "$tmp/home4/child.pid" 2>/dev/null || true)"
  [[ -n "$child" ]] || self_fail "the stand-in cargo never ran under the killed gate: $(cat "$tmp/merge-killed.out")"
  for _ in $(seq 100); do [[ -e "/proc/$child" ]] || break; sleep 0.1; done
  [[ ! -e "/proc/$child" ]] || self_fail "the child of the killed gate was still running after 10 s"
  [[ "$(cat "$tmp/home4/.cache/cuba-gate/run.exit" 2>/dev/null)" == "running pid="* ]] \
    || self_fail "a gate killed while its child worked left '$(cat "$tmp/home4/.cache/cuba-gate/run.exit" 2>/dev/null)' in its exit file: the child wrote a verdict that was not its to write"

  # Where the gate looks for the models has to be where the binary keeps them:
  # envs::home() joins `.cache` to HOME (or USERPROFILE), and nothing in the
  # crate reads XDG_CACHE_HOME. The only embedder is in the legacy cache under
  # HOME and a decoy sits under XDG_CACHE_HOME, so both halves of the choice
  # below — the preferred cache and the legacy one — have to stop reading the
  # variable for ONNX_MODEL_PATH to come out right. The stand-in cargo reports
  # what the script exported and fails, which ends the run at `cargo fmt`.
  mkdir -p "$tmp/home5/.cache/cuba-memorys/models" "$tmp/xdg5/memory-industry/models" "$tmp/bin5"
  : >"$tmp/home5/.cache/cuba-memorys/models/model_quantized.onnx"
  : >"$tmp/xdg5/memory-industry/models/model_quantized.onnx"
  cp "$tmp/bin/pg_isready" "$tmp/bin/psql" "$tmp/bin5/"
  printf '%s\n' '#!/bin/sh' \
    'echo "stand-in cargo: ONNX_MODEL_PATH=$ONNX_MODEL_PATH"' \
    'exit 1' >"$tmp/bin5/cargo"
  chmod +x "$tmp/bin5/cargo"
  cache_exit=0
  env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u ONNX_MODEL_PATH \
      HOME="$tmp/home5" XDG_CACHE_HOME="$tmp/xdg5" DATABASE_URL="$rat_url" \
      PATH="$tmp/bin5:/usr/bin:/bin" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      "$BASH" "$ROOT/scripts/run-all-tests.sh" >"$tmp/cache.out" 2>&1 || cache_exit=$?
  grep -qxF "stand-in cargo: ONNX_MODEL_PATH=$tmp/home5/.cache/cuba-memorys/models" "$tmp/cache.out" \
    || self_fail "with XDG_CACHE_HOME set, the gate looked for the models where the binary does not (exit $cache_exit): $(grep 'ONNX_MODEL_PATH=' "$tmp/cache.out" || cat "$tmp/cache.out")"

  # The script launched on its own derives brain_gate, brain_gate_peer and
  # postgres from DATABASE_URL by replacing what follows its last slash. A URL
  # that ends in a query (here the socket given as ?host=) puts those names on
  # another server, or on the live corpus, so it is refused with 2 before any
  # step runs: the stand-in psql and cargo record every call, and neither may
  # have been called. The exit file starts at 0, as an earlier green gate would
  # have left it, and must end at 2 with the lock released.
  mkdir -p "$tmp/home6/.cache/cuba-gate" "$tmp/bin6"
  printf '0\n' >"$tmp/home6/.cache/cuba-gate/run.exit"
  printf '%s\n' '#!/bin/sh' 'echo psql >>"$HOME/tools.calls"' 'exit 1' >"$tmp/bin6/psql"
  printf '%s\n' '#!/bin/sh' 'echo cargo >>"$HOME/tools.calls"' 'exit 1' >"$tmp/bin6/cargo"
  chmod +x "$tmp/bin6/psql" "$tmp/bin6/cargo"
  query_exit=0
  env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE \
      HOME="$tmp/home6" PATH="$tmp/bin6:/usr/bin:/bin" \
      DATABASE_URL='postgresql://gate:S3cr3tW2@/brain?host=%2Frun%2Fpostgresql' \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      "$BASH" "$ROOT/scripts/run-all-tests.sh" >"$tmp/query.out" 2>&1 || query_exit=$?
  (( query_exit == 2 )) \
    || self_fail "run-all-tests.sh alone exited $query_exit, not 2, over a DATABASE_URL that ends in a query: $(cat "$tmp/query.out")"
  grep -q '^FAIL: ' "$tmp/query.out" \
    || self_fail "the refusal of a DATABASE_URL with a query is not a FAIL line: $(cat "$tmp/query.out")"
  grep -qF '%2F' "$tmp/query.out" \
    || self_fail "the refusal does not say how the socket is written without a query (%2F in the host): $(cat "$tmp/query.out")"
  [[ ! -e "$tmp/home6/tools.calls" ]] \
    || self_fail "a DATABASE_URL it cannot derive brain_gate from reached a step, and called: $(cat "$tmp/home6/tools.calls")"
  if grep -q 'S3cr3tW2' "$tmp/query.out"; then
    self_fail "the refusal carries the password of DATABASE_URL: $(cat "$tmp/query.out")"
  fi
  [[ "$(cat "$tmp/home6/.cache/cuba-gate/run.exit" 2>/dev/null)" == 2 ]] \
    || self_fail "the refusal exited 2 and its exit file says '$(cat "$tmp/home6/.cache/cuba-gate/run.exit" 2>/dev/null)'"
  [[ ! -e "$tmp/home6/.cache/cuba-gate/lock" ]] || self_fail "the refusal left its lock behind"

  # J1 (CI-J), run-all-tests.sh on its own: a DATABASE_URL that is unset, empty, or
  # that names a server without the judge's marker stops the script with 2 before
  # any section. Its cargo and psql stand-ins record every call; none may run after
  # the refusal (the unmarked server is asked its identity, and that is all). A
  # system_identifier other than the one merge-gate exported in GATE_SERVER_SYSTEM_ID
  # is the server changed under the run, and is refused the same way.
  rat_refused() {
    local name="$1" url="$2" cluster="$3" exported="$4"
    local -a start=(DATABASE_URL="$url") extra=()
    [[ "$url" != unset ]] || start=()
    [[ -z "$exported" ]] || extra=(GATE_SERVER_SYSTEM_ID="$exported")
    mkdir -p "$tmp/$name/home/.cache/cuba-gate" "$tmp/$name/bin"
    printf '0\n' >"$tmp/$name/home/.cache/cuba-gate/run.exit"
    printf '%s\n' '#!/bin/sh' 'echo cargo >>"$HOME/tools.calls"' 'exit 1' >"$tmp/$name/bin/cargo"
    printf '%s\n' '#!/bin/sh' 'echo psql >>"$HOME/tools.calls"' \
      'echo "$FAKE_RAT_CLUSTER|7701731310526959658"' 'exit 0' >"$tmp/$name/bin/psql"
    # pg_isready and docker are stand-ins too: a real one on /usr/bin could reach
    # the live server. Each records its call.
    printf '%s\n' '#!/bin/sh' 'echo pg_isready >>"$HOME/tools.calls"' \
      'echo "127.0.0.1:5489 - accepting connections"' 'exit 0' >"$tmp/$name/bin/pg_isready"
    printf '%s\n' '#!/bin/sh' 'echo docker >>"$HOME/tools.calls"' 'exit 1' >"$tmp/$name/bin/docker"
    chmod +x "$tmp/$name/bin/cargo" "$tmp/$name/bin/psql" "$tmp/$name/bin/pg_isready" "$tmp/$name/bin/docker"
    rat_exit=0
    env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u DATABASE_URL -u GATE_SERVER_SYSTEM_ID \
        HOME="$tmp/$name/home" PATH="$tmp/$name/bin:/usr/bin:/bin" \
        ${start[@]+"${start[@]}"} ${extra[@]+"${extra[@]}"} FAKE_RAT_CLUSTER="$cluster" \
        CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
        "$BASH" "$ROOT/scripts/run-all-tests.sh" >"$tmp/$name.out" 2>&1 || rat_exit=$?
  }
  rat_stopped() {
    local name="$1" want="$2"
    (( rat_exit == want )) || self_fail "$name: run-all-tests.sh exited $rat_exit, not $want: $(cat "$tmp/$name.out")"
    if grep -qx cargo "$tmp/$name/home/tools.calls" 2>/dev/null; then
      self_fail "$name: cargo ran after the refusal: $(cat "$tmp/$name.out")"
    fi
  }
  # j16_unset and j16_empty (the refusal with no DATABASE_URL) are retired on Linux: they are the D5
  # rows, after calibrate, where the tree and the lane's stand-in exist (CI-F2, F2b-1).
  rat_refused j16_unmarked "$rat_url" '' ''
  rat_stopped j16_unmarked 2
  grep -qx psql "$tmp/j16_unmarked/home/tools.calls" \
    || self_fail "j16_unmarked: the server was not asked its identity before the refusal"
  grep -q '^FAIL: .*cluster_name' "$tmp/j16_unmarked.out" \
    || self_fail "j16_unmarked: no FAIL line names the cluster_name it read: $(cat "$tmp/j16_unmarked.out")"
  rat_refused j15_changed "$rat_url" memory-industry-gate 1111
  rat_stopped j15_changed 2
  grep -q 'the server changed' "$tmp/j15_changed.out" \
    || self_fail "j15_changed: a system_identifier other than the exported one was not refused as 'the server changed': $(cat "$tmp/j15_changed.out")"
  rat_refused j15_same "$rat_url" memory-industry-gate 7701731310526959658
  (( rat_exit != 2 )) && ! grep -q 'the server changed' "$tmp/j15_same.out" \
    || self_fail "j15_same: the same system_identifier as the exported one was refused (exit $rat_exit): $(cat "$tmp/j15_same.out")"

  # The calibrated counts file every tree copies once calibrate has set it (empty until then).
  calib_file=""
  # J2, J3, J4, J6 (CI-J): full_tree copies this script into a tree of its own,
  # with stand-ins for everything it runs before the eval, so a row reads what the
  # real script hands to each one and the exit it reaches. cargo logs every call;
  # FAKE_CARGO_FAIL names the subcommand that fails (FAKE_CARGO_FAIL_EXIT is its
  # code), FAKE_CARGO_TEST_COUNT is what each test run reports, and
  # FAKE_CARGO_BUILD_EXE is the executable a build reports with --message-format
  # (by default the binary the build put under CARGO_TARGET_DIR). cargo test prints
  # what cargo prints: a Running header and a test result line per binary.
  # full_run writes FAKE_OBS lines as the fixture smoke-corpus.jsonl, and psql counts
  # FAKE_SEEDED of them (by default the fixture's lines). FAKE_PSQL_* set the judge's
  # identity, which psql answers for pg_control_system. df reports FAKE_DF_GB. The
  # binary logs its doctor and eval calls; eval reports FAKE_NDCG and FAKE_SCORED.
  # Nothing here reaches a server.
  full_tree() {
    local name="$1" t="$tmp/$1"
    mkdir -p "$t/scripts" "$t/rust/tests" "$t/rust/eval-datasets" "$t/rust/target/debug/deps" \
      "$t/home/.cache/cuba-gate" "$t/bin" "$t/standin" "$t/target/debug" "$t/target/release" \
      "$t/models" "$t/reranker"
    cp "$ROOT/scripts/run-all-tests.sh" "$ROOT/scripts/gate-lock.sh" "$ROOT/scripts/test-times.sh" "$t/scripts/"
    # CI-F2 (F2b-1): the judge's lane library as a stand-in with the same functions and no Postgres.
    # order.log records its calls in the order they came (the D5 rows read it).
    cat >"$t/scripts/gate-cluster.sh" <<'FAKE'
GATE_CLUSTER_ROOT="${GATE_CLUSTER_ROOT:-$HOME/cluster-root}"
GATE_CLUSTER_URL=""
gate_cluster_check() { :; }
gate_cluster_sweep() { :; }
gate_cluster_up() {
  echo "up $1" >>"$HOME/order.log"
  touch "$HOME/lanes.open"
  GATE_CLUSTER_URL="postgresql://cuba:fakepw@${GATE_CLUSTER_ROOT//\//%2F}%2F$1.fk0000:5432/brain"
  echo "OK  cluster $1 up in 1 ms (stand-in)"
}
gate_cluster_down() { echo "down $1" >>"$HOME/order.log"; rm -f "$HOME/lanes.open"; echo "OK  cluster $1 down"; }
gate_cluster_down_all() {
  [ -e "$HOME/lanes.open" ] || return 0
  echo down-all >>"$HOME/order.log"
  rm -f "$HOME/lanes.open"
}
FAKE
    [[ -z "${calib_file:-}" ]] || cp "$calib_file" "$t/scripts/test-section-counts.txt"
    cp "$ROOT/rust/eval-datasets/smoke.jsonl" "$t/rust/eval-datasets/"
    : >"$t/rust/tests/t1.rs"
    : >"$t/nli-model"
    : >"$t/models/model_quantized.onnx"
    : >"$t/reranker/model.onnx"
    # CI-F1: a call of build-gpu.sh leaves its mark, so a row can say the judge did not make it.
    printf '%s\n' '#!/bin/sh' 'echo build-gpu >>"$HOME/gpu.calls"' 'exit 0' >"$t/scripts/build-gpu.sh"
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$t/scripts/gpu-placement-check.sh"
    printf '%s\n' 'raise SystemExit(0)' >"$t/scripts/mcp_live_session_test.py"
    printf '%s\n' 'raise SystemExit(0)' >"$t/rust/tests/e2e_all_tools.py"
    printf '%s\n' '#!/bin/sh' \
      'echo "$0 $*" >>"$HOME/bin.calls"' \
      'case "$1" in' \
      '  eval)' \
      '    echo "DATABASE_URL=${DATABASE_URL-unset} $*" >>"$HOME/eval.calls"' \
      '    list=""; i=0; while [ "$i" -lt "${FAKE_SCORED-5}" ]; do list="${list:+$list, }0.8"; i=$((i + 1)); done' \
      '    if [ -n "${FAKE_NO_PER_QUERY-}" ]; then' \
      '      printf "{\"metrics\": {\"ndcg_at_k\": %s, \"sample_count\": %s}}\n" "${FAKE_NDCG-0.8}" "${FAKE_LOADED-5}"' \
      '    else' \
      '      printf "{\"metrics\": {\"ndcg_at_k\": %s, \"sample_count\": %s, \"per_query_ndcg\": [%s]}}\n" "${FAKE_NDCG-0.8}" "${FAKE_LOADED-5}" "$list"' \
      '    fi ;;' \
      '  doctor) [ "${2:-}" = --json ] && echo "{\"checks\":[]}" ;;' \
      'esac' \
      'exit 0' >"$t/standin/memory-industry"
    printf '%s\n' '#!/bin/sh' \
      'echo "$*" >>"$HOME/cargo.calls"' \
      'echo "${CARGO_BUILD_JOBS-unset} $*" >>"$HOME/cargo.jobs"' \
      'if [ "$1" = "${FAKE_CARGO_FAIL:-none}" ]; then exit "${FAKE_CARGO_FAIL_EXIT:-1}"; fi' \
      'mkdir -p "$CARGO_TARGET_DIR/debug" "$CARGO_TARGET_DIR/release"' \
      'case "$*" in *--release*) [ -e "$CARGO_TARGET_DIR/release/memory-industry" ] || cp "$(cat "$HOME/standin.path")" "$CARGO_TARGET_DIR/release/memory-industry" ;; esac' \
      '[ -z "${FAKE_CHANGE_RELEASE_ON:-}" ] || case "$*" in *"$FAKE_CHANGE_RELEASE_ON"*) echo changed >"$CARGO_TARGET_DIR/release/memory-industry" ;; esac' \
      'if [ "$1" = build ]; then' \
      '  [ -e "$CARGO_TARGET_DIR/debug/memory-industry" ] || cp "$(cat "$HOME/standin.path")" "$CARGO_TARGET_DIR/debug/memory-industry"' \
      '  case "$*" in *--message-format=*) printf "%s\n" "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"memory-industry\"},\"fresh\":${FAKE_CARGO_FRESH:-true},\"executable\":\"${FAKE_CARGO_BUILD_EXE:-$CARGO_TARGET_DIR/debug/memory-industry}\"}" ;; esac' \
      'fi' \
      'if [ "$1" = test ]; then' \
      '  n="${FAKE_CARGO_TEST_COUNT:-3}"' \
      '  names=""; prev=""' \
      '  for a in "$@"; do [ "$prev" = --test ] && names="$names $a"; prev="$a"; done' \
      '  [ -n "$names" ] || names=fixture' \
      '  for t in $names; do' \
      '    m="$n"; [ "$t" = "${FAKE_CARGO_ZERO_TEST-}" ] && m=0' \
      '    echo "     Running tests/$t.rs (target/debug/deps/$t-0000)"' \
      '    echo "running $m tests"' \
      '    echo "test result: ok. $m passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"' \
      '  done' \
      'fi' \
      'exit 0' >"$t/bin/cargo"
    printf '%s\n' '#!/bin/sh' \
      'echo "$*" >>"$HOME/psql.calls"' \
      'case "$*" in' \
      '  *pg_control_system*)' \
      '    lane="$(printf "%s" "$*" | sed -n "s/.*%2F\([a-z][a-z0-9-]*\)\.[A-Za-z0-9]\{6\}:5432.*/\1/p")"' \
      '    name="${FAKE_PSQL_CLUSTER_NAME-memory-industry-gate}"; [ -n "$lane" ] && [ -z "${FAKE_PSQL_CLUSTER_NAME+x}" ] && name="memory-industry-gate-$lane"' \
      '    echo "$name|${FAKE_PSQL_SYSID-7701731310526959658}" ;;' \
      '  *atttypmod*) echo 384 ;;' \
      '  *information_schema.tables*) echo 20 ;;' \
      '  *shobj_description*) echo "" ;;' \
      '  *brain_observations*) echo "${FAKE_SEEDED-${FAKE_OBS-5}}" ;;' \
      'esac' \
      'exit 0' >"$t/bin/psql"
    printf '%s\n' '#!/bin/sh' \
      'echo "$*" >>"$HOME/df.calls"' \
      'printf "Avail\n%sG\n" "${FAKE_DF_GB-100}"' >"$t/bin/df"
    printf '%s\n' '#!/bin/sh' 'echo "docker $*" >>"$HOME/docker.calls"' 'exit 1' >"$t/bin/docker"
    printf '%s' "$t/standin/memory-industry" >"$t/home/standin.path"
    # The seeder of the eval (scripts/eval_seed.py, D-J3a). It seeds nothing: it records its
    # arguments and prints the count it claims to have seeded, `seeded N observations`, which
    # the judge checks against what psql counts in the database. Both are FAKE_SEEDED, by
    # default FAKE_OBS, so the claim and the database agree unless a row makes them differ.
    printf '%s\n' 'import os, sys' \
      'open(os.environ["HOME"] + "/seed.calls", "a").write(" ".join(sys.argv[1:]) + "\n")' \
      'print("seeded %s observations" % os.environ.get("FAKE_SEEDED", os.environ.get("FAKE_OBS", "5")))' \
      >"$t/scripts/eval_seed.py"
    chmod +x "$t"/scripts/*.sh "$t"/bin/* "$t"/standin/*
  }
  # full_run NAME [VAR=value ...]: the whole script in NAME's tree, against the judge's
  # test URL, with the sweep and the free-space floor off unless a row says otherwise.
  # Its exit lands in $rc and what it printed in $tmp/NAME.out.
  full_run() {
    local name="$1" t="$tmp/$1" obs=5 arg n
    shift
    for arg in "$@"; do
      case "$arg" in FAKE_OBS=*) obs="${arg#FAKE_OBS=}" ;; esac
    done
    : >"$t/rust/eval-datasets/smoke-corpus.jsonl"
    for ((n = 1; n <= obs; n++)); do
      printf '{"content":"fake observation %d"}\n' "$n" >>"$t/rust/eval-datasets/smoke-corpus.jsonl"
    done
    rc=0
    env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u PGPORT HOME="$t/home" \
      PATH="$t/bin:/usr/bin:/bin" DATABASE_URL="$rat_url" CARGO_TARGET_DIR="$t/target" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      ONNX_MODEL_PATH="$t/models" CUBA_RERANKER_PATH="$t/reranker" CUBA_NLI_PATH="$t/nli-model" \
      MEMORY_INDUSTRY_LLM_PROVIDER=ollama "$@" \
      "$BASH" "$t/scripts/run-all-tests.sh" >"$tmp/$name.out" 2>&1 || rc=$?
  }

  # CALIBRATION (D-J4e, spec §10 C3). The rows that run the whole script (J2, J3, J6) run
  # on a tree whose scripts/test-section-counts.txt is calibrated, not written by hand. The
  # counts the fake cargo produces follow how each section calls cargo (one binary per
  # --test, FAKE_CARGO_TEST_COUNT tests each), and a hand-written number would have to copy
  # that structure. The calibration reads what the script's own counter says: the judge
  # prints `OK  section <name>: <N> tests in …` once a section passes its count, so an empty
  # file lets every section pass its check and state its number. The file is written from
  # those lines. It isolates J2, J3 and J6 from the counts, which TT13 and the counts
  # contract test; the J4 rows start from this file and perturb it. It runs once per
  # self-test, not once per row.
  calibrate() {
    local counts="$tmp/calib/scripts/test-section-counts.txt" lines
    full_tree calib
    : >"$counts"
    full_run calib
    lines="$(sed -n 's/^OK  section \([^:]*\): \([0-9][0-9]*\) tests in .*$/\1 \2 calibrated/p' "$tmp/calib.out")"
    [[ -n "$lines" ]] \
      || self_fail "calibration found no OK  section lines: the judge prints one per section that passes its count (run_counted). $(cat "$tmp/calib.out")"
    printf '%s\n' "$lines" >"$tmp/calibrated-counts.txt"
    calib_file="$tmp/calibrated-counts.txt"
  }
  calibrate

  # D5 (CI-F2, F2b-1), run-all-tests.sh on its own: with no DATABASE_URL, unset or empty, on Linux, it
  # makes its dbg lane, runs its tests against that lane's URL, and takes the lane down when it leaves.
  # The rows replace j16_unset and j16_empty, which expected a refusal; on Linux that refusal is gone
  # (D-F2c, D-F2k). order.log is what the lane stand-in was asked, in order.
  d5_run() {
    local name="$1" t="$tmp/$1"
    shift
    printf '%s\n' '{"content":"fake observation 1"}' '{"content":"fake observation 2"}' \
      '{"content":"fake observation 3"}' '{"content":"fake observation 4"}' \
      '{"content":"fake observation 5"}' >"$t/rust/eval-datasets/smoke-corpus.jsonl"
    rc=0
    env -u DATABASE_URL -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u PGPORT HOME="$t/home" \
      PATH="$t/bin:/usr/bin:/bin" CARGO_TARGET_DIR="$t/target" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      ONNX_MODEL_PATH="$t/models" CUBA_RERANKER_PATH="$t/reranker" CUBA_NLI_PATH="$t/nli-model" \
      MEMORY_INDUSTRY_LLM_PROVIDER=ollama "$@" \
      "$BASH" "$t/scripts/run-all-tests.sh" >"$tmp/$name.out" 2>&1 || rc=$?
  }
  for form in unset empty; do
    full_tree "j16_$form"
    if [[ "$form" == unset ]]; then d5_run "j16_$form"; else d5_run "j16_$form" DATABASE_URL=; fi
    (( rc == 0 )) || self_fail "j16_$form: run-all-tests with no DATABASE_URL on Linux exited $rc, not 0: $(cat "$tmp/j16_$form.out")"
    [[ "$(head -n 1 "$tmp/j16_$form/home/order.log" 2>/dev/null)" == "up dbg" ]] \
      || self_fail "j16_$form: the first thing the judge asked its lane library was not 'up dbg': $(cat "$tmp/j16_$form/home/order.log" 2>/dev/null)"
    [[ "$(tail -n 1 "$tmp/j16_$form/home/order.log" 2>/dev/null)" =~ ^(down\ dbg|down-all)$ ]] \
      || self_fail "j16_$form: lane dbg was not taken down when run-all-tests left: $(cat "$tmp/j16_$form/home/order.log" 2>/dev/null)"
    grep -qE '^DATABASE_URL=postgresql://cuba:fakepw@[^ ]*%2Fdbg\.fk0000:5432/[^ ]+ ' "$tmp/j16_$form/home/eval.calls" 2>/dev/null \
      || self_fail "j16_$form: the tests did not run against the URL of lane dbg: $(cat "$tmp/j16_$form/home/eval.calls" 2>/dev/null)"
    grep -q fakepw "$tmp/j16_$form.out" \
      && self_fail "j16_$form: the output carries the password of the lane: $(cat "$tmp/j16_$form.out")"
  done

  # J2.1: brain_gate is prepared with the binary cargo has just reported. A stale
  # binary planted where the old lookup looked is never run, and nothing is
  # looked for under release/ or under the cuba-memorys name.
  full_tree j21
  printf '%s\n' '#!/bin/sh' 'echo planted >>"$HOME/planted.ran"' 'exit 0' >"$tmp/j21/target/debug/memory-industry"
  printf '%s\n' '#!/bin/sh' 'echo planted >>"$HOME/planted-cuba.ran"' 'exit 0' >"$tmp/j21/target/debug/cuba-memorys"
  mkdir -p "$tmp/j21/fresh"
  cp "$tmp/j21/standin/memory-industry" "$tmp/j21/fresh/memory-industry"
  chmod +x "$tmp/j21/fresh/memory-industry" "$tmp/j21/target/debug/memory-industry" "$tmp/j21/target/debug/cuba-memorys"
  full_run j21 FAKE_CARGO_BUILD_EXE="$tmp/j21/fresh/memory-industry"
  grep -q '^build --bin memory-industry --message-format=' "$tmp/j21/home/cargo.calls" 2>/dev/null \
    || self_fail "J2.1: cargo was not asked for 'build --bin memory-industry --message-format=...': $(cat "$tmp/j21/home/cargo.calls" 2>/dev/null || true)"
  [[ ! -e "$tmp/j21/home/planted.ran" ]] \
    || self_fail "J2.1: the stale binary planted under target/debug ran: $(cat "$tmp/j21.out")"
  [[ ! -e "$tmp/j21/home/planted-cuba.ran" ]] \
    || self_fail "J2.1: the cuba-memorys name under target/debug was looked for and ran"
  grep -qxF "$tmp/j21/fresh/memory-industry doctor" "$tmp/j21/home/bin.calls" 2>/dev/null \
    || self_fail "J2.1: doctor did not run with the executable cargo reported: $(cat "$tmp/j21/home/bin.calls" 2>/dev/null || true)"

  # J2.2: a build that fails prepares nothing: exit 1 with a FAIL line, no database
  # claimed, and the stale binary left under target/debug not run. The stale binary
  # is what makes a build that fails silently (cargo ... || true) visible here.
  full_tree j22
  printf '%s\n' '#!/bin/sh' 'echo stale >>"$HOME/stale.ran"' 'exit 0' >"$tmp/j22/target/debug/memory-industry"
  chmod +x "$tmp/j22/target/debug/memory-industry"
  full_run j22 FAKE_CARGO_FAIL=build FAKE_CARGO_FAIL_EXIT=101
  (( rc == 1 )) || self_fail "J2.2: a build that failed with 101 made run-all-tests exit $rc, not 1: $(cat "$tmp/j22.out")"
  grep -q '^FAIL: ' "$tmp/j22.out" || self_fail "J2.2: the failed build did not print a FAIL line: $(cat "$tmp/j22.out")"
  [[ ! -e "$tmp/j22/home/stale.ran" ]] || self_fail "J2.2: the stale binary under target/debug ran after its build failed"
  if grep -q 'CREATE DATABASE' "$tmp/j22/home/psql.calls" 2>/dev/null; then
    self_fail "J2.2: a database was created after the build failed: $(cat "$tmp/j22/home/psql.calls")"
  fi

  # J2.1 (P1-c, D-J2e): Git Bash reports the executable in Windows form, with the escapes
  # cargo writes in JSON (C:\\Users\\x\\target\\debug\\memory-industry.exe). gate_bin takes it,
  # maps it with cygpath -u (a stand-in here, which maps to this tree's binary) and runs doctor
  # on the mapped path: it does not refuse with "reported no executable".
  full_tree j21w
  printf '#!/bin/sh\necho %s\n' "$tmp/j21w/standin/memory-industry" >"$tmp/j21w/bin/cygpath"
  chmod +x "$tmp/j21w/bin/cygpath"
  full_run j21w FAKE_CARGO_BUILD_EXE='C:\\Users\\x\\target\\debug\\memory-industry.exe'
  if grep -q 'reported no executable' "$tmp/j21w.out"; then
    self_fail "J2.1 (P1-c): a Windows-form executable that cargo reported was refused: $(cat "$tmp/j21w.out")"
  fi
  grep -qF "$tmp/j21w/standin/memory-industry doctor" "$tmp/j21w/home/bin.calls" 2>/dev/null \
    || self_fail "J2.1 (P1-c): doctor did not run with the path cygpath -u maps the Windows one to: $(cat "$tmp/j21w/home/bin.calls" 2>/dev/null || echo 'no doctor call')"

  # J3.1: the eval reads the scratch database brain_gate_eval, and asks for --json.
  full_tree j31
  full_run j31
  grep -q 'brain_gate_eval ' "$tmp/j31/home/eval.calls" 2>/dev/null \
    || self_fail "J3.1: the eval was not pointed at brain_gate_eval: $(cat "$tmp/j31/home/eval.calls" 2>/dev/null || echo 'no eval call')"
  grep -q -- ' --json' "$tmp/j31/home/eval.calls" 2>/dev/null \
    || self_fail "J3.1: the eval was not asked for --json: $(cat "$tmp/j31/home/eval.calls" 2>/dev/null || echo 'no eval call')"

  # J3.2: a number that measures nothing is red. nDCG 0; fewer scored queries than
  # smoke.jsonl has lines; no observation seeded.
  full_tree j32a
  full_run j32a FAKE_NDCG=0.0
  (( rc == 1 )) || self_fail "J3.2: an eval with ndcg_at_k 0.0 exited $rc, not 1: $(cat "$tmp/j32a.out")"
  grep -qi '^FAIL: .*ndcg' "$tmp/j32a.out" || self_fail "J3.2: no FAIL line names the nDCG of 0: $(cat "$tmp/j32a.out")"
  full_tree j32b
  full_run j32b FAKE_SCORED=3
  (( rc == 1 )) || self_fail "J3.2: an eval that scored 3 of the 5 smoke queries exited $rc, not 1: $(cat "$tmp/j32b.out")"
  grep -q '^FAIL: .*smoke.jsonl' "$tmp/j32b.out" || self_fail "J3.2: no FAIL line names the queries smoke.jsonl has: $(cat "$tmp/j32b.out")"
  # J3.2 (P1-a, D-J3d): the scored queries are the entries of metrics.per_query_ndcg, not
  # sample_count, which counts the queries loaded (harness.rs). Five loaded with one scored is red.
  full_tree j32f
  full_run j32f FAKE_SCORED=1
  (( rc == 1 )) || self_fail "J3.2 (P1-a): an eval with sample_count 5 and one per-query score exited $rc, not 1: $(cat "$tmp/j32f.out")"
  grep -q '^FAIL: .*smoke.jsonl' "$tmp/j32f.out" || self_fail "J3.2 (P1-a): no FAIL line names the queries smoke.jsonl has, for one scored entry: $(cat "$tmp/j32f.out")"
  # J3.2 (P1-a): an eval --json without per_query_ndcg cannot say what it scored: red.
  full_tree j32g
  full_run j32g FAKE_NO_PER_QUERY=1
  (( rc == 1 )) || self_fail "J3.2 (P1-a): an eval --json without per_query_ndcg exited $rc, not 1: $(cat "$tmp/j32g.out")"
  grep -q '^FAIL: .*per_query_ndcg' "$tmp/j32g.out" || self_fail "J3.2 (P1-a): no FAIL line names the missing per_query_ndcg: $(cat "$tmp/j32g.out")"
  # An empty scratch database: the seeder claims 0 and psql counts 0 over a five-line fixture.
  # A zero-line fixture is not used, because the judge's count of the fixture's lines
  # (grep -c, under set -e) ends the script with no message on an empty file: a production
  # defect of pass 2, reported to the coordinator, not hidden here.
  full_tree j32c
  full_run j32c FAKE_SEEDED=0
  (( rc == 1 )) || self_fail "J3.2: an empty scratch database exited $rc, not 1: $(cat "$tmp/j32c.out")"
  grep -q '^FAIL: .*0 observations' "$tmp/j32c.out" || self_fail "J3.2: no FAIL line says 0 observations: $(cat "$tmp/j32c.out")"
  # J3.2 (N4, D-J3c): the observations the judge counts in the scratch database are the
  # lines of the fixture. Two seeded for a five-line fixture is red.
  full_tree j32d
  full_run j32d FAKE_SEEDED=2
  (( rc == 1 )) || self_fail "J3.2: a scratch database with 2 observations for a 5-line fixture exited $rc, not 1: $(cat "$tmp/j32d.out")"
  grep -q '^FAIL: .*fixture' "$tmp/j32d.out" || self_fail "J3.2: no FAIL line names the mismatch with the fixture: $(cat "$tmp/j32d.out")"

  # J4 (N5, D-J4b): the counts of each section against its line, both ways. The rows start
  # from the calibrated file and move one line, or one binary, so the message names the
  # section and the two numbers. admin-role is the section that names two --test binaries.
  admin_ran="$(awk '$1 == "admin-role" { print $2 }' "$calib_file")"
  [[ "$admin_ran" =~ ^[0-9]+$ ]] && (( admin_ran > 0 )) \
    || self_fail "J4: the calibrated file says admin-role runs '$admin_ran' tests, so the rows have no number to move"
  # J4.1: a line one more than the count is red, and the message names the section and both numbers.
  full_tree j41
  awk -v n="$((admin_ran + 1))" '$1 == "admin-role" { $2 = n } { print }' "$calib_file" \
    >"$tmp/j41/scripts/test-section-counts.txt"
  full_run j41
  (( rc == 1 )) || self_fail "J4.1: admin-role ran $admin_ran tests against a line of $((admin_ran + 1)) and the script exited $rc, not 1: $(cat "$tmp/j41.out")"
  grep -q "admin-role.*expected $((admin_ran + 1)), ran $admin_ran" "$tmp/j41.out" \
    || self_fail "J4.1: the mismatch is not named with its section and both numbers: $(cat "$tmp/j41.out")"
  # J4.2: the count is checked in both directions, and a section with no line is red.
  full_tree j42a
  awk -v n="$((admin_ran - 1))" '$1 == "admin-role" { $2 = n } { print }' "$calib_file" \
    >"$tmp/j42a/scripts/test-section-counts.txt"
  full_run j42a
  (( rc == 1 )) || self_fail "J4.2: a line of $((admin_ran - 1)) against $admin_ran run tests exited $rc, not 1: $(cat "$tmp/j42a.out")"
  grep -q "expected $((admin_ran - 1)), ran $admin_ran" "$tmp/j42a.out" \
    || self_fail "J4.2: a count too low is not named: $(cat "$tmp/j42a.out")"
  full_tree j42b
  awk '$1 != "admin-role"' "$calib_file" >"$tmp/j42b/scripts/test-section-counts.txt"
  full_run j42b
  (( rc == 1 )) || self_fail "J4.2: a section that ran with no line in the file exited $rc, not 1: $(cat "$tmp/j42b.out")"
  grep -q '^FAIL: .*admin-role' "$tmp/j42b.out" \
    || self_fail "J4.2: no FAIL line names the section with no line: $(cat "$tmp/j42b.out")"
  # J4.2 (orphan): a line for a section the script does not have is red, and it is named.
  full_tree j42c
  { cat "$calib_file"; printf '%s\n' 'ghost-section 1 a line for a section this script does not have'; } \
    >"$tmp/j42c/scripts/test-section-counts.txt"
  full_run j42c
  (( rc == 1 )) || self_fail "J4.2: a line for no section of the script exited $rc, not 1: $(cat "$tmp/j42c.out")"
  grep -q '^FAIL: .*ghost-section' "$tmp/j42c.out" \
    || self_fail "J4.2: no FAIL line names the line for no section: $(cat "$tmp/j42c.out")"
  # J4.3: a named --test binary that runs no test is the same silence. The fake cargo gives
  # v020_role_separation zero tests, so admin-role runs one binary's worth fewer than its line.
  full_tree j43
  full_run j43 FAKE_CARGO_ZERO_TEST=v020_role_separation
  (( rc == 1 )) || self_fail "J4.3: a named --test that ran 0 tests exited $rc, not 1: $(cat "$tmp/j43.out")"
  grep -q "admin-role.*expected $admin_ran, ran $((admin_ran - 3))" "$tmp/j43.out" \
    || self_fail "J4.3: the section that lost a binary is not named with its two counts: $(cat "$tmp/j43.out")"

  # J6.1: the sweep works on CARGO_TARGET_DIR, the target the build writes to, not on
  # rust/target. Old artifacts of CARGO_TARGET_DIR go, recent ones stay, and the
  # sentinel under rust/target stays. df is asked about CARGO_TARGET_DIR.
  full_tree j61
  ct="$tmp/j61/ctarget"
  mkdir -p "$ct/debug/deps" "$ct/debug/.fingerprint/old-fp" "$ct/debug/incremental"
  printf x >"$ct/debug/deps/old-artifact"
  printf x >"$ct/debug/deps/new-artifact"
  printf x >"$ct/debug/incremental/old-inc"
  printf x >"$tmp/j61/rust/target/debug/deps/centinel"
  touch -d '30 days ago' "$ct/debug/deps/old-artifact" "$ct/debug/incremental/old-inc" \
    "$ct/debug/.fingerprint/old-fp" "$tmp/j61/rust/target/debug/deps/centinel"
  full_run j61 FAKE_CARGO_FAIL=fmt CARGO_TARGET_DIR="$ct" CUBA_GATE_SWEEP_BELOW_GB=50 FAKE_DF_GB=10
  [[ ! -e "$ct/debug/deps/old-artifact" ]] \
    || self_fail "J6.1: an artifact 30 days old in CARGO_TARGET_DIR survived the sweep: $(cat "$tmp/j61.out")"
  [[ -e "$ct/debug/deps/new-artifact" ]] || self_fail "J6.1: the sweep took a recent artifact of CARGO_TARGET_DIR"
  [[ -e "$tmp/j61/rust/target/debug/deps/centinel" ]] \
    || self_fail "J6.1: the sweep went into rust/target and took the sentinel there"
  grep -qF "$ct" "$tmp/j61/home/df.calls" 2>/dev/null \
    || self_fail "J6.1: df was not asked about CARGO_TARGET_DIR: $(cat "$tmp/j61/home/df.calls" 2>/dev/null || echo 'df never ran')"

  # J6.1 (N6, D-J6c): debug/incremental is swept by the same age rule as the rest of the
  # target. Incremental state from 30 days ago goes; the recent one stays.
  full_tree j61i
  ct="$tmp/j61i/ctarget"
  mkdir -p "$ct/debug/incremental/old-session" "$ct/debug/incremental/new-session"
  printf x >"$ct/debug/incremental/old-session/old-inc"
  printf x >"$ct/debug/incremental/new-session/new-inc"
  touch -d '30 days ago' "$ct/debug/incremental/old-session/old-inc" "$ct/debug/incremental/old-session"
  full_run j61i FAKE_CARGO_FAIL=fmt CARGO_TARGET_DIR="$ct" CUBA_GATE_SWEEP_BELOW_GB=50 FAKE_DF_GB=10
  [[ ! -e "$ct/debug/incremental/old-session/old-inc" ]] \
    || self_fail "J6.1: incremental state 30 days old survived the sweep of debug/incremental: $(cat "$tmp/j61i.out")"
  [[ -e "$ct/debug/incremental/new-session/new-inc" ]] \
    || self_fail "J6.1: the sweep took the recent incremental state of debug/incremental"

  # --- CI-J2 rows (spec §11, A4, A5, P5; D-J3c, D-J3d). `run-all-tests.sh --check-eval JSON
  # QUERIES_LINES` judges the eval's --json the way the eval section does, with no database:
  # the queries scored are metrics.per_query_ndcg, metrics.sample_count is the queries loaded,
  # and metrics.ndcg_at_k is a number above 0. Each key is read by its path: a per_ability block
  # in front of metrics (with its own ndcg_at_k) does not stand for the eval's.
  ce() { rc=0; env -u DATABASE_URL bash "$ROOT/scripts/run-all-tests.sh" --check-eval "$1" "$2" >"$tmp/ce.out" 2>&1 || rc=$?; }
  printf '%s\n' '{"per_ability":[{"ndcg_at_k":0.0}],"metrics":{"ndcg_at_k":0.7,"sample_count":5,"per_query_ndcg":[0.1,0.2,0.3,0.4,0.5]}}' >"$tmp/ce.a4ok.json"
  ce "$tmp/ce.a4ok.json" 5
  (( rc == 0 )) || self_fail "A4: an eval whose per_ability ndcg_at_k is 0.0 in front of metrics.ndcg_at_k 0.7, 5 loaded and 5 scored, was not judged green (exit $rc): $(cat "$tmp/ce.out")"
  printf '%s\n' '{"per_ability":[{"ndcg_at_k":0.9}],"metrics":{"ndcg_at_k":0.0,"sample_count":5,"per_query_ndcg":[0.1,0.2,0.3,0.4,0.5]}}' >"$tmp/ce.a4red.json"
  ce "$tmp/ce.a4red.json" 5
  (( rc == 1 )) || self_fail "A4: an eval whose metrics.ndcg_at_k is 0.0, behind a per_ability ndcg_at_k of 0.9, was not red (exit $rc, want 1): $(cat "$tmp/ce.out")"
  grep -q 'ndcg' "$tmp/ce.out" || self_fail "A4: the red nDCG names no ndcg: $(cat "$tmp/ce.out")"

  # P5 (D-J3c, D-J3d, seed 20261106, printed on a FAIL): 200 generated evals over 1 to 6 query
  # lines. Green exactly when sample_count and the length of per_query_ndcg are the query lines
  # and ndcg_at_k is a JSON number above 0. A string, null or an absent ndcg_at_k is red, and a
  # per_ability block (with a 0.0 of its own) always sits in front of metrics.
  p5_seed=20261106
  RANDOM=$p5_seed
  p5_nds=(0.0 1e-9 0.3 null '"0.5"' absent)
  p5_green=0
  for ((p5_n = 0; p5_n < 200; p5_n++)); do
    p5_lines=$(( RANDOM % 6 + 1 ))
    p5_sample=$(( RANDOM % 7 ))
    p5_scored=$(( RANDOM % 7 ))
    p5_nd="${p5_nds[RANDOM % ${#p5_nds[@]}]}"
    p5_q=""
    for ((p5_i = 0; p5_i < p5_scored; p5_i++)); do p5_q+="${p5_q:+,}0.5"; done
    p5_m="\"sample_count\":$p5_sample,\"per_query_ndcg\":[$p5_q]"
    [[ "$p5_nd" == absent ]] || p5_m="\"ndcg_at_k\":$p5_nd,$p5_m"
    printf '{"per_ability":[{"ndcg_at_k":0.0,"sample_count":9}],"metrics":{%s}}\n' "$p5_m" >"$tmp/ce.p5.json"
    p5_want=1
    if (( p5_sample == p5_lines && p5_scored == p5_lines )) && [[ "$p5_nd" == 1e-9 || "$p5_nd" == 0.3 ]]; then
      p5_want=0
      p5_green=$(( p5_green + 1 ))
    fi
    ce "$tmp/ce.p5.json" "$p5_lines"
    (( rc == p5_want )) \
      || self_fail "P5 seed $p5_seed, case $p5_n: ${p5_lines} query lines, sample_count $p5_sample, $p5_scored scored, ndcg_at_k $p5_nd: judged $rc, want $p5_want: $(cat "$tmp/ce.out") / $(cat "$tmp/ce.p5.json")"
  done
  (( p5_green > 0 )) || self_fail "P5 seed $p5_seed: none of the 200 generated evals was green, so the green half of the property is not tested"

  # --- CI-F1 rows (spec §4, F1, F2, F3; D-F1b, D-F2b, D-F3a/b). run_args NAME ARG...: the
  # script with ARG... in NAME's tree, under the environment full_run gives it; $rc and
  # $tmp/NAME.out as full_run leaves them. cg_tree NAME CPU_MAX MEMORY_MAX MEMORY_CURRENT
  # builds a cgroup v2 scope by hand, read through GATE_CGROUP_ROOT and GATE_PROC_SELF_CGROUP.
  run_args() {
    local name="$1" t="$tmp/$1"
    shift
    rc=0
    env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u PGPORT HOME="$t/home" \
      PATH="$t/bin:/usr/bin:/bin" DATABASE_URL="$rat_url" CARGO_TARGET_DIR="$t/target" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      ONNX_MODEL_PATH="$t/models" CUBA_RERANKER_PATH="$t/reranker" CUBA_NLI_PATH="$t/nli-model" \
      MEMORY_INDUSTRY_LLM_PROVIDER=ollama \
      "$BASH" "$t/scripts/run-all-tests.sh" "$@" >"$tmp/$name.out" 2>&1 || rc=$?
  }
  cg_tree() {
    local scope="$tmp/cg/$1/root/app/pesado.scope"
    mkdir -p "$scope"
    printf '%s\n' "$2" >"$scope/cpu.max"
    printf '%s\n' "$3" >"$scope/memory.max"
    printf '%s\n' "$4" >"$scope/memory.current"
    printf '0::/app/pesado.scope\n' >"$tmp/cg/$1/self"
  }

  # F1.1 (D-F1b): N is the CARGO_BUILD_JOBS merge-gate counted, 8 here. The release calls take
  # min(N, floor(memory margin in GB / 4)): the cgroup leaves 12 GiB, so 3. Every other call takes N.
  cg_tree f1rel "800000 100000" 17179869184 4294967296
  full_tree f1rel
  full_run f1rel CARGO_BUILD_JOBS=8 GATE_CGROUP_ROOT="$tmp/cg/f1rel/root" GATE_PROC_SELF_CGROUP="$tmp/cg/f1rel/self"
  (( rc == 0 )) || self_fail "F1.1 release: a full run whose every step passed exited $rc: $(cat "$tmp/f1rel.out")"
  [[ -s "$tmp/f1rel/home/cargo.jobs" ]] || self_fail "F1.1 release: no cargo call was recorded"
  grep -q -- ' --release' "$tmp/f1rel/home/cargo.jobs" || self_fail "F1.1 release: no release call was recorded"
  if grep -- ' --release' "$tmp/f1rel/home/cargo.jobs" | grep -qv '^3 '; then
    self_fail "F1.1 release: a release call did not take min(8, floor(12 GiB / 4)) = 3 jobs: $(grep -- ' --release' "$tmp/f1rel/home/cargo.jobs")"
  fi
  if grep -v -- ' --release' "$tmp/f1rel/home/cargo.jobs" | grep -qv '^8 '; then
    self_fail "F1.1 release: a call that is not a release one did not take the 8 jobs: $(grep -v -- ' --release' "$tmp/f1rel/home/cargo.jobs")"
  fi

  # F1.2 (D-F1a, D-F1b): the jobs the operator named are kept on every call that is not a release
  # one. The release calls still take min(N, floor(margin / 4)): a 16 GiB cap with 4 GiB in use
  # leaves 12 GiB, so 3 of the 5 named.
  cg_tree f1op "800000 100000" 17179869184 4294967296
  full_tree f1op
  full_run f1op CARGO_BUILD_JOBS=5 GATE_CGROUP_ROOT="$tmp/cg/f1op/root" GATE_PROC_SELF_CGROUP="$tmp/cg/f1op/self"
  (( rc == 0 )) || self_fail "F1.2: a full run with CARGO_BUILD_JOBS=5 whose every step passed exited $rc: $(cat "$tmp/f1op.out")"
  [[ -s "$tmp/f1op/home/cargo.jobs" ]] || self_fail "F1.2: no cargo call was recorded"
  if grep -- ' --release' "$tmp/f1op/home/cargo.jobs" | grep -qv '^3 '; then
    self_fail "F1.2: a release call did not take min(5, floor(12 GiB / 4)) = 3 jobs: $(grep -- ' --release' "$tmp/f1op/home/cargo.jobs")"
  fi
  if grep -v -- ' --release' "$tmp/f1op/home/cargo.jobs" | grep -qv '^5 '; then
    self_fail "F1.2: a call that is not a release one did not keep the CARGO_BUILD_JOBS=5 the operator set: $(grep -v -- ' --release' "$tmp/f1op/home/cargo.jobs")"
  fi

  # F2.2 (D-F2b): --check-models asks the paths the tests ask, runs no cargo, and names what is missing
  # with the text of require_present.
  full_tree f2mok
  run_args f2mok --check-models
  (( rc == 0 )) || self_fail "F2.2: --check-models with every model on disk exited $rc: $(cat "$tmp/f2mok.out")"
  [[ ! -e "$tmp/f2mok/home/cargo.calls" ]] || self_fail "F2.2: --check-models ran cargo: $(cat "$tmp/f2mok/home/cargo.calls")"
  full_tree f2mgone
  rm -f "$tmp/f2mgone/models/model_quantized.onnx"
  run_args f2mgone --check-models
  (( rc == 1 )) || self_fail "F2.2: --check-models with the embedding model missing exited $rc, not 1: $(cat "$tmp/f2mgone.out")"
  grep -qF "nothing at $tmp/f2mgone/models/model_quantized.onnx" "$tmp/f2mgone.out" \
    || self_fail "F2.2: --check-models did not say what is missing, with the text of require_present: $(cat "$tmp/f2mgone.out")"
  [[ ! -e "$tmp/f2mgone/home/cargo.calls" ]] || self_fail "F2.2: --check-models ran cargo before it found the model missing"

  # F3.1 (D-F3a): one release call compiles the bins and the arneses, with --no-run; no step runs
  # build-gpu.sh.
  full_tree f31
  full_run f31
  (( rc == 0 )) || self_fail "F3.1: a full run whose every step passed exited $rc: $(cat "$tmp/f31.out")"
  [[ ! -e "$tmp/f31/home/gpu.calls" ]] || self_fail "F3.1: the gate ran build-gpu.sh: $(cat "$tmp/f31/home/gpu.calls")"
  noRun="$(grep -- '--no-run' "$tmp/f31/home/cargo.calls" 2>/dev/null || true)"
  [[ "$(grep -c -- '--no-run' "$tmp/f31/home/cargo.calls" 2>/dev/null || true)" == 1 ]] \
    || self_fail "F3.1: expected exactly one --no-run release call, found: '$noRun'"
  [[ "$noRun" == *' --release '* && "$noRun" == *' --lib '* && "$noRun" == *'--test v017_rerank_gpu'* && "$noRun" != *'--bins'* ]] \
    || self_fail "F3.1: the --no-run call must be --release with --lib and --test v017_rerank_gpu and no --bins: $noRun"

  # F3.2 (D-F3b): --check-fresh judges the JSON of the release build: every compiler-artifact fresh,
  # or exit 1 naming the unit that was compiled again. It runs no cargo.
  printf '%s\n' \
    '{"reason":"build-script-executed","package_id":"memory-industry 0.31.0"}' \
    '{"reason":"compiler-artifact","package_id":"memory-industry 0.31.0","target":{"name":"memory-industry","kind":["bin"]},"fresh":true,"filenames":[]}' \
    '{"reason":"build-finished","success":true}' >"$tmp/fresh-ok.json"
  printf '%s\n' \
    '{"reason":"compiler-artifact","package_id":"memory-industry 0.31.0","target":{"name":"memory-industry","kind":["bin"]},"fresh":false,"filenames":[]}' \
    '{"reason":"build-finished","success":true}' >"$tmp/fresh-bad.json"
  full_tree fresh
  run_args fresh --check-fresh "$tmp/fresh-ok.json"
  (( rc == 0 )) || self_fail "F3.2: --check-fresh over a build in which every unit was fresh exited $rc: $(cat "$tmp/fresh.out")"
  [[ ! -e "$tmp/fresh/home/cargo.calls" ]] || self_fail "F3.2: --check-fresh ran cargo, which only judges a file: $(cat "$tmp/fresh/home/cargo.calls")"
  run_args fresh --check-fresh "$tmp/fresh-bad.json"
  (( rc == 1 )) || self_fail "F3.2: --check-fresh over a unit compiled again exited $rc, not 1: $(cat "$tmp/fresh.out")"
  grep -qF 'memory-industry' "$tmp/fresh.out" \
    || self_fail "F3.2: --check-fresh did not name the unit it found compiled again: $(cat "$tmp/fresh.out")"

  # F3.2 (B3g, D-F3b): a release build that compiled memory-industry again after the --no-run is
  # refused with exit 1 by name, before the E2E.
  full_tree f3b
  full_run f3b FAKE_CARGO_FRESH=false
  (( rc == 1 )) || self_fail "F3.2 B3g: a release build that compiled memory-industry again exited $rc, not 1: $(cat "$tmp/f3b.out")"
  grep -qF 'memory-industry recompiled' "$tmp/f3b.out" \
    || self_fail "F3.2 B3g: the gate did not say 'memory-industry recompiled': $(cat "$tmp/f3b.out")"
  if grep -qF '=== E2E' "$tmp/f3b.out"; then
    self_fail "F3.2 B3g: the E2E ran after the build was refused"
  fi

  # F3.3 (D-F3b): the binary the E2E judges is the one the --no-run built. A change to it, made by
  # whichever step, is refused before the E2E with its sha256 named.
  sha_row() {
    full_tree "$1"
    full_run "$1" FAKE_CHANGE_RELEASE_ON="$2"
    (( rc == 1 )) || self_fail "F3.3 ($2): the release binary changed under the gate and the run exited $rc, not 1: $(cat "$tmp/$1.out")"
    grep -qF 'sha256 changed' "$tmp/$1.out" \
      || self_fail "F3.3 ($2): the change was not named by its sha256: $(cat "$tmp/$1.out")"
    if grep -qF '=== E2E' "$tmp/$1.out"; then
      self_fail "F3.3 ($2): the E2E ran on a binary that had changed"
    fi
  }
  # F3.3 row for a change made inside the --no-run call is not in this file on purpose
  # (F-b, 6-nov): that call is the one that creates the binary and D-F3b takes the sha256
  # baseline after it, so a change made there is the baseline and cannot be seen as a change.
  # --bins (inside B3g) and gpu:: (after the --no-run, before the E2E) cover the order.
  sha_row f33b --bins
  sha_row f33c 'gpu::'
  grep -qF 'sha256 changed before E2E' "$tmp/f33c.out" \
    || self_fail "F3.3: a change after the gpu:: release call is not named 'sha256 changed before E2E': $(cat "$tmp/f33c.out")"

  # CI-F3.3a (D-F3y, spec §4 F3.3a): --lane <id> runs one step of the table and nothing else. A missing
  # or unknown id is refused before the lock, a lane or cargo is touched. Each row runs the script in a
  # fake tree (full_tree), so it reads what the real script does, with cargo, psql and the lane library
  # as stand-ins.
  lane_run() {
    local name="$1" t="$tmp/$1"
    shift
    full_tree "$name"
    rc=0
    env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u PGPORT HOME="$t/home" \
      PATH="$t/bin:/usr/bin:/bin" DATABASE_URL="$rat_url" CARGO_TARGET_DIR="$t/target" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      ONNX_MODEL_PATH="$t/models" CUBA_RERANKER_PATH="$t/reranker" CUBA_NLI_PATH="$t/nli-model" \
      MEMORY_INDUSTRY_LLM_PROVIDER=ollama \
      "$BASH" "$t/scripts/run-all-tests.sh" "$@" >"$tmp/$name.out" 2>&1 || rc=$?
  }
  lane_run lane_unknown --lane NOPE
  (( rc == 2 )) || self_fail "F3.3a: --lane NOPE exits $rc, not 2: $(cat "$tmp/lane_unknown.out")"
  grep -q 'no such lane: NOPE' "$tmp/lane_unknown.out" \
    || self_fail "F3.3a: --lane NOPE does not name the lane it refuses: $(cat "$tmp/lane_unknown.out")"
  [[ ! -e "$tmp/lane_unknown/home/cargo.calls" && ! -e "$tmp/lane_unknown/home/order.log" ]] \
    || self_fail "F3.3a: a refused --lane touched cargo or a lane: $(cat "$tmp/lane_unknown/home/cargo.calls" "$tmp/lane_unknown/home/order.log" 2>/dev/null)"
  lane_run lane_bare --lane
  (( rc == 2 )) || self_fail "F3.3a: --lane with no id exits $rc, not 2: $(cat "$tmp/lane_bare.out")"
  grep -q 'needs a lane id' "$tmp/lane_bare.out" \
    || self_fail "F3.3a: --lane with no id does not say so: $(cat "$tmp/lane_bare.out")"
  [[ ! -e "$tmp/lane_bare/home/cargo.calls" ]] || self_fail "F3.3a: --lane with no id ran cargo"
  lane_run lane_d2 --lane D2
  (( rc == 0 )) || self_fail "F3.3a: --lane D2 exits $rc, not 0: $(cat "$tmp/lane_d2.out")"
  grep -q -- '--ignored' "$tmp/lane_d2/home/cargo.calls" 2>/dev/null \
    || self_fail "F3.3a: --lane D2 ran none of its --ignored tests: $(cat "$tmp/lane_d2/home/cargo.calls" 2>/dev/null)"
  grep -Eq -- 'fmt|clippy|--release' "$tmp/lane_d2/home/cargo.calls" 2>/dev/null \
    && self_fail "F3.3a: --lane D2 ran another lane's cargo call: $(cat "$tmp/lane_d2/home/cargo.calls")"
  grep -q 'All tests passed' "$tmp/lane_d2.out" \
    && self_fail "F3.3a: --lane D2 printed the verdict of the whole run"

  echo "OK  self-test: a second gate refuses at once and names the first, merge-gate"
  echo "    included, and the variable its children inherit is no pass for anyone"
  echo "    else; the run-all-tests.sh merge-gate launches works under its lock and"
  echo "    leaves the exit file to it; a dead or reused-pid lock is taken over, a"
  echo "    foreign one is not; an exit drops only the databases whose record is"
  echo "    still its own; a run on its own"
  echo "    refuses a DATABASE_URL it cannot derive brain_gate from, before any step;"
  echo "    the models are looked for under HOME/.cache, where the"
  echo "    binary keeps them, whatever XDG_CACHE_HOME says"
  exit 0
fi
# --- self-test end ---

# CI-F3.3a (D-F3y, spec §4): the lanes of this script in idx order, as scripts/gate-dag.txt names them.
# --lane <id> runs the one lane it names; a missing or unknown id is refused here, before the lock.
DAG_LANES=(CL D1 D2 NLI D3 B3 R E)
run_lane=""
if [[ "${1:-}" == --lane ]]; then
  [[ -n "${2:-}" ]] || { echo "FAIL: --lane needs a lane id, one of: ${DAG_LANES[*]}" >&2; exit 2; }
  case " ${DAG_LANES[*]} " in
    *" $2 "*) run_lane="$2" ;;
    *) echo "FAIL: no such lane: $2 (the lanes are: ${DAG_LANES[*]})" >&2; exit 2 ;;
  esac
fi

inherit_gate_lock "$GATE_LOCK" || acquire_gate_lock "$GATE_LOCK" || exit 1
[[ -z "$GATE_LOCK_HELD" ]] || exit_file_running
trap on_exit EXIT

# The judge runs only against its own server (CI-J, D-J1a): a server that does not
# carry the cluster_name mark is refused here, whatever its port. Under merge-gate.sh
# this repeats the check, which costs nothing. The system_identifier merge-gate.sh
# read must be the one this run reads (D-J1c).
# D5 (CI-F2, D-F2c): run on its own, with no DATABASE_URL, on Linux, this script makes its dbg lane after
# taking the lock, runs against that lane, and takes it down on exit (on_exit). Under merge-gate.sh the
# lock is inherited (CUBA_GATE_LOCK_OWNER) and merge-gate.sh has made the lane of the step: nothing here.
# --check-models asks no server, so it makes no lane and needs none.
if [[ -z "${DATABASE_URL:-}" && -z "${CUBA_GATE_LOCK_OWNER:-}" && "${1:-}" != --check-models && "$(uname -s)" == Linux ]]; then
  unset GATE_SERVER_SYSTEM_ID
  gate_cluster_check
  gate_cluster_sweep
  gate_cluster_up dbg
  export DATABASE_URL="$GATE_CLUSTER_URL" GATE_LANE=dbg
fi
if [[ "${1:-}" != --check-models || -n "${DATABASE_URL:-}" ]]; then
  gate_require_judge_server "${DATABASE_URL:-}"
fi

# The model paths are chosen once, in gate_model_env (gate-lock.sh, CI-J5); merge-gate.sh calls it too.
gate_model_env

# D-F2b (CI-F1): merge-gate.sh asks for the models before its first compile, with the paths
# exported above, so a missing model is seconds and not the first section. Nothing compiles.
require_model() {
  if [[ ! -e "$2" ]]; then
    echo "FAIL: $1 - nothing at $2" >&2
    return 1
  fi
}
check_models() {
  local rc=0
  require_model "tests that need the embedding model" "$ONNX_MODEL_PATH/model_quantized.onnx" || rc=1
  require_model "tests that need the NLI model" "$CUBA_NLI_PATH" || rc=1
  require_model "the reranker tests" "$CUBA_RERANKER_PATH/model.onnx" || rc=1
  return "$rc"
}
if [[ "${1:-}" == "--check-models" ]]; then
  check_models
  echo "OK  models on disk: embedding, NLI and reranker"
  exit 0
fi

# Disk, before anything compiles. A gate run of this repo linked with `ld` dying on
# SIGBUS and three test binaries reported as "could not compile" — the cause was a
# partition at 98% with target/ alone holding 64 GB, and nothing in that output said
# so. Measured 15-ago-2026. A gate that fails for a reason nobody can read is worse
# than one that refuses to start.
#
# target/debug/deps grows without bound because cargo never removes the binaries of
# earlier compilations: every edit produces a new hash and the old one stays. The
# sweep below is by age, not by size, so an artifact still in use is never touched.
MIN_FREE_GB="${CUBA_GATE_MIN_FREE_GB:-8}"
SWEEP_BELOW_GB="${CUBA_GATE_SWEEP_BELOW_GB:-20}"
SWEEP_OLDER_THAN_DAYS="${CUBA_GATE_SWEEP_DAYS:-7}"

# The filesystem the target directory lives on, measured where it exists yet: a
# CARGO_TARGET_DIR that has not been created is measured by its nearest parent.
gate_existing_target_dir() {
  local d
  d="$(gate_target_dir)"
  while [[ ! -e "$d" && "$d" != / ]]; do d="$(dirname "$d")"; done
  printf '%s\n' "$d"
}

free_gb() { df --output=avail -BG "$(gate_existing_target_dir)" | tail -1 | tr -dc '0-9'; }

sweep_stale_artifacts() {
  local before after td
  td="$(gate_target_dir)"
  before="$(free_gb)"
  echo "disk: ${before}G free — sweeping build artifacts older than ${SWEEP_OLDER_THAN_DAYS}d under $td"
  find "$td/debug/incremental" -mindepth 1 -maxdepth 1 \
       -mtime "+$SWEEP_OLDER_THAN_DAYS" -exec rm -rf {} + 2>/dev/null || true
  find "$td/debug/deps" -maxdepth 1 -type f \
       -mtime "+$SWEEP_OLDER_THAN_DAYS" -delete 2>/dev/null || true
  find "$td/debug/.fingerprint" -maxdepth 1 -type d \
       -mtime "+$SWEEP_OLDER_THAN_DAYS" -exec rm -rf {} + 2>/dev/null || true
  after="$(free_gb)"
  echo "disk: ${after}G free after the sweep (was ${before}G)"
}

if [[ "$(free_gb)" -lt "$SWEEP_BELOW_GB" ]]; then
  sweep_stale_artifacts
fi

if [[ "$(free_gb)" -lt "$MIN_FREE_GB" ]]; then
  echo "FAIL: only $(free_gb)G free on the filesystem holding $(gate_target_dir), and this run needs more." >&2
  echo '      Linking is what breaks first, and it breaks as SIGBUS inside ld with no' >&2
  echo "      mention of disk — that is an hour of looking for a bug that is not there." >&2
  echo "      target directory currently holds: $(du -sh "$(gate_target_dir)" 2>/dev/null | cut -f1)" >&2
  echo "      Free it with: cargo clean --manifest-path $RUST_DIR/Cargo.toml" >&2
  echo "      (or raise the bar with CUBA_GATE_MIN_FREE_GB if you know what you are doing)" >&2
  exit 1
fi

# --- the test counts (CI-J, D-J4b) -------------------------------------------
# A section that runs a different number of tests than its line in
# scripts/test-section-counts.txt is red, in either direction: a test that stops
# being run is the same silence as one that is skipped. The count is passed + failed
# over the `test result:` lines of that section's cargo call. A named section (one
# that names its --test binaries) must also run every binary at least one test.
SECTION_COUNTS="$ROOT/scripts/test-section-counts.txt"
# The sections this gate ran and judged, in order (CI-J2, A7): run_counted appends each one. It is
# the only list of sections: check_section_lines compares the count file with it, both ways.
RAN_SECTIONS=""

# run_counted SECTION NAMED <command>: runs the call with its output shown and kept,
# judges the count, and exits with the call's own code when the call failed. The count
# is test-times.sh --count's, the one parse of a cargo log the judge has (D-J4d).
run_counted() {
  local section="$1" named="$2" log counts rc=0 want=0 arg expected ran bins zero
  shift 2
  for arg in "$@"; do
    [[ "$arg" == --test ]] && want=$((want + 1))
  done
  log="$(mktemp)"
  "$@" 2>&1 | tee "$log" || rc=$?
  if (( rc != 0 )); then
    rm -f "$log"
    exit "$rc"
  fi
  if ! counts="$("$ROOT/scripts/test-times.sh" --count "$log")"; then
    echo "FAIL: section $section: test-times.sh --count could not read the cargo log (see above)." >&2
    rm -f "$log"
    exit 1
  fi
  rm -f "$log"
  read -r ran bins zero <<<"$(awk '{ ran += $2; bins++; if ($2 == 0) zero++ }
    END { printf "%d %d %d\n", ran + 0, bins + 0, zero + 0 }' <<<"$counts")"
  expected="$(awk -v s="$section" '!/^[[:space:]]*#/ && NF >= 2 && $1 == s { print $2; exit }' "$SECTION_COUNTS" 2>/dev/null || true)"
  if [[ -n "$expected" && ! "$expected" =~ ^[0-9]+$ ]]; then
    echo "FAIL: scripts/test-section-counts.txt says '$expected' for section $section, which is not a number." >&2
    exit 1
  fi
  if [[ -n "$expected" && "$expected" != "$ran" ]]; then
    echo "FAIL: section $section: expected $expected, ran $ran tests ($bins test binaries). scripts/test-section-counts.txt says $expected: change that line only with the log that proves the new number." >&2
    exit 1
  fi
  if [[ "$named" == 1 ]] && { (( bins != want )) || (( zero > 0 )); }; then
    echo "FAIL: section $section: $bins test binaries reported for $want named --test, $zero of them ran 0 tests. Each named binary has to run at least one test." >&2
    exit 1
  fi
  RAN_SECTIONS+=" $section"
  echo "OK  section $section: $ran tests in $bins test binaries"
}

# Every section that runs tests has its line, and every line is a section that runs.
check_section_lines() {
  local listed name missing="" orphan=""
  listed="$(awk '!/^[[:space:]]*#/ && NF >= 2 { print $1 }' "$SECTION_COUNTS" 2>/dev/null || true)"
  for name in $RAN_SECTIONS; do
    grep -qxF "$name" <<<"$listed" || missing+=" $name"
  done
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    case " $RAN_SECTIONS " in
      *" $name "*) ;;
      *) orphan+=" $name" ;;
    esac
  done <<<"$listed"
  if [[ -n "$missing" ]]; then
    echo "FAIL: scripts/test-section-counts.txt has no line for the section(s):$missing. Every section that runs tests states its count there, with its reason." >&2
    exit 1
  fi
  if [[ -n "$orphan" ]]; then
    echo "FAIL: scripts/test-section-counts.txt has a line for no section of this script:$orphan." >&2
    exit 1
  fi
}

require_present() {
  local what="$1" probe="$2"
  shift 2
  if [[ ! -e "$probe" ]]; then
    echo "FAIL: $what — nothing at $probe" >&2
    echo "      This gate does not skip model/CLI coverage. Install with" >&2
    echo "      \`memory-industry models all\` (and a generative LLM — see require_generative_llm)." >&2
    exit 1
  fi
  echo "=== $what ==="
  "$@"
}

require_generative_llm() {
  local what="$1"
  shift
  local base="${MEMORY_INDUSTRY_LLM_BASE_URL:-${CUBA_LLM_BASE_URL:-}}"
  local provider="${MEMORY_INDUSTRY_LLM_PROVIDER:-${CUBA_LLM_PROVIDER:-}}"
  local ok=0
  if [[ -n "$base" ]]; then
    base="${base%/}"
    if curl -fsS --max-time 5 "${base}/models" >/dev/null 2>&1 \
      || curl -fsS --max-time 8 -X POST "${base}/chat/completions" \
           -H "Content-Type: application/json" \
           -H "Authorization: Bearer ${MEMORY_INDUSTRY_LLM_API_KEY:-${CUBA_LLM_API_KEY:-local}}" \
           -d '{"model":"'"${MEMORY_INDUSTRY_LLM_MODEL:-ping}"'","messages":[{"role":"user","content":"ping"}],"max_tokens":1}' \
           >/dev/null 2>&1; then
      ok=1
      echo "OK  generative LLM via OpenAI-compat at $base"
    else
      echo "FAIL: $what — MEMORY_INDUSTRY_LLM_BASE_URL=$base is set but probe failed." >&2
      echo "      Fix the endpoint/API key — the gate does not soft-skip." >&2
      exit 1
    fi
  fi
  if [[ "$ok" -eq 0 && -n "$provider" ]]; then
    # Provider presets are resolved in Rust; presence of provider + any common key is enough to proceed.
    if [[ -n "${MEMORY_INDUSTRY_LLM_API_KEY:-${CUBA_LLM_API_KEY:-${OPENAI_API_KEY:-${DEEPSEEK_API_KEY:-${DASHSCOPE_API_KEY:-${MOONSHOT_API_KEY:-${ZHIPU_API_KEY:-${SILICONFLOW_API_KEY:-${OPENROUTER_API_KEY:-}}}}}}}}}" ]] \
      || [[ "$provider" == "ollama" || "$provider" == "lmstudio" || "$provider" == "vllm" ]]; then
      ok=1
      echo "OK  generative LLM via MEMORY_INDUSTRY_LLM_PROVIDER=$provider"
    else
      echo "FAIL: $what — provider=$provider set but no API key found." >&2
      echo "      Export MEMORY_INDUSTRY_LLM_API_KEY or the vendor key (DEEPSEEK_API_KEY, DASHSCOPE_API_KEY, …)." >&2
      exit 1
    fi
  fi
  if [[ "$ok" -eq 0 ]] && command -v claude >/dev/null 2>&1; then
    if claude -p "reply with the single word ok" --output-format text >/dev/null 2>&1; then
      ok=1
      echo "OK  generative LLM via authenticated claude CLI"
    fi
  fi
  if [[ "$ok" -eq 0 ]] && command -v gemini >/dev/null 2>&1; then
    if gemini -p "reply with the single word ok" --output-format text >/dev/null 2>&1; then
      ok=1
      echo "OK  generative LLM via authenticated gemini CLI"
    fi
  fi
  if [[ "$ok" -eq 0 ]]; then
    echo "FAIL: $what — no generative LLM ready." >&2
    echo "      Set MEMORY_INDUSTRY_LLM_PROVIDER=deepseek|qwen|moonshot|zhipu|openai|ollama|…" >&2
    echo "      (+ vendor API key), or MEMORY_INDUSTRY_LLM_BASE_URL to any OpenAI-compat /v1," >&2
    echo "      or authenticate \`claude\`/\`gemini\`, or use MCP sampling." >&2
    exit 1
  fi
  echo "=== $what ==="
  "$@"
}

GATE_DB="${GATE_DB:-brain_gate}"
GATE_DATABASE_URL="${DATABASE_URL%/*}/$GATE_DB"
PEER_DB="${PEER_DB:-brain_gate_peer}"
PEER_DATABASE_URL="${DATABASE_URL%/*}/$PEER_DB"
ADMIN_DATABASE_URL="${DATABASE_URL%/*}/postgres"
export CUBA_PEER_DATABASE_URL="$PEER_DATABASE_URL"

# Windows PostgreSQL 16 client: `psql URI -c SQL` prints
# "extra command-line argument -c ignored" and never runs the SQL.
# Measured 2026-09-17, Git Bash + C:\Program Files\PostgreSQL\16\bin\psql.exe.
# The URI must be -d, not argv[1].
psql_url() {
  local url="$1"
  shift
  psql -d "$url" "$@"
}

if ! command -v psql >/dev/null; then
  echo "FAIL: psql is not on PATH, and the gate needs it to provision its throwaway databases." >&2
  echo "      It used to reach the server with 'docker exec <container> psql', which makes psql" >&2
  echo "      a child of the postmaster: when that psql exits non-zero the postmaster reads it" >&2
  echo "      as a crashed backend and restarts the whole cluster, taking the live brain" >&2
  echo "      database into recovery with it. Measured on 2026-08-14. Install postgresql-client." >&2
  exit 1
fi

cd "$RUST_DIR"

# The binary the judge runs is the one cargo reports for this tree, built now: never a
# copy left under target/ by an earlier build, and never a name looked up under
# release/ or cuba-memorys (CI-J, D-J2a). A build that fails prepares nothing (J2.2).
gate_bin() {
  local out exe
  out="$(cargo build --bin memory-industry --message-format=json-render-diagnostics)" \
    || { echo "FAIL: cargo build --bin memory-industry failed: no database is prepared from a binary that did not build." >&2; exit 1; }
  exe="$(printf '%s\n' "$out" \
    | sed -n 's/.*"reason":"compiler-artifact".*"executable":"\([^"]*\)".*/\1/p' \
    | grep -E '[/\\]memory-industry(\.exe)?$' | tail -n 1 || true)"
  exe="${exe//\\\\/\\}"
  if [[ -n "$exe" ]] && command -v cygpath >/dev/null 2>&1; then
    exe="$(cygpath -u "$exe")"
  fi
  [[ -n "$exe" && -x "$exe" ]] \
    || { echo "FAIL: cargo build --bin memory-industry reported no executable for memory-industry (it reported: ${exe:-nothing})." >&2; exit 1; }
  printf '%s\n' "$exe"
}

provision_gate_db() {
  local bin
  bin="$(gate_bin)"
  claim_database "$GATE_DB" || exit 1
  # doctor exits non-zero when ONNX_MODEL_PATH is empty (policy checks). Migrations
  # still apply first — trust the table count below, not the exit code alone.
  local doctor_log
  doctor_log="$(mktemp)"
  DATABASE_URL="$GATE_DATABASE_URL" CUBA_APP_ROLE=0 ONNX_MODEL_PATH="" \
    "$bin" doctor >"$doctor_log" 2>&1 || true
  local tables
  tables="$(psql_url "$GATE_DATABASE_URL" -Atc \
    "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")"
  if ((tables < 20)); then
    echo "FAIL: could not migrate the throwaway database (only $tables tables)." >&2
    echo "      doctor via $bin did not apply schema. Its output:" >&2
    cat "$doctor_log" >&2 || true
    rm -f "$doctor_log"
    exit 1
  fi
  rm -f "$doctor_log"
  if [[ "$CUBA_EMBEDDING_DIM" != "384" ]]; then
    DATABASE_URL="$GATE_DATABASE_URL" "$ROOT/scripts/migrate-embedding-dim.sh" \
      "$CUBA_EMBEDDING_DIM" >/dev/null 2>&1 || {
        echo "FAIL: could not retype the throwaway database to vector($CUBA_EMBEDDING_DIM)." >&2
        exit 1
      }
  fi
  local dim
  dim="$(psql_url "$GATE_DATABASE_URL" -Atc \
    "SELECT atttypmod FROM pg_attribute WHERE attrelid='brain_observations'::regclass AND attname='embedding'")"
  if [[ "$dim" != "$CUBA_EMBEDDING_DIM" ]]; then
    echo "FAIL: throwaway database is vector($dim) but the model produces $CUBA_EMBEDDING_DIM." >&2
    echo "      Every embedding write would fail with 'expected $dim dimensions'." >&2
    exit 1
  fi
  echo "OK  throwaway database $GATE_DB ready ($tables tables, vector($dim))"

  claim_database "$PEER_DB" || exit 1
  DATABASE_URL="$PEER_DATABASE_URL" CUBA_APP_ROLE=0 ONNX_MODEL_PATH="" \
    "$bin" doctor >/dev/null 2>&1 || true
  if [[ "$CUBA_EMBEDDING_DIM" != "384" ]]; then
    DATABASE_URL="$PEER_DATABASE_URL" "$ROOT/scripts/migrate-embedding-dim.sh" \
      "$CUBA_EMBEDDING_DIM" >/dev/null 2>&1 || true
  fi
  local peer_tables
  peer_tables="$(psql_url "$PEER_DATABASE_URL" -Atc \
    "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")"
  if ((peer_tables < 20)); then
    echo "FAIL: could not migrate the second node's database (only $peer_tables tables)." >&2
    echo "      The two-node test would then skip, and a skipped test that reports green is" >&2
    echo "      how a machine claims two nodes converge without ever having run two." >&2
    exit 1
  fi
  echo "OK  second node database $PEER_DB ready ($peer_tables tables)"
}

gate_cargo_jobs

lane_CL() {
echo "=== cargo fmt --check ==="
cargo fmt --check

echo "=== cargo clippy (--all-targets: without it, tests/ is never linted) ==="
cargo clippy --all-targets -- -D warnings
}

lane_D1() {
echo "=== throwaway database for every mutating step ==="
provision_gate_db

echo "=== cargo test (unit + smoke) ==="
DATABASE_URL="$GATE_DATABASE_URL" cargo test
}

lane_D2() {
echo "=== DB integration tests (--ignored) ==="
export DATABASE_URL="$GATE_DATABASE_URL"
# Every tests/*.rs is discovered, not listed. The list used to be written by hand
# and had drifted to 30 of 52 files: fifteen integration tests written for the
# sync and panel work had never once run in the gate, while the gate reported
# green over the very commits that added them. A gate whose coverage depends on
# somebody remembering to edit it is a gate that quietly shrinks.
RUN_ELSEWHERE=(v020_audit_update_applies v020_role_separation v016_chunking nli_entailment nli_cost nli_probe
               v016_extract_without_sampling v017_relation_scan v017_rerank_gpu)
DISCOVERED=()
DEFERRED_SECTIONS=()
for file in tests/*.rs; do
  name="$(basename "$file" .rs)"
  if printf '%s\n' "${RUN_ELSEWHERE[@]}" | grep -qx "$name"; then
    DEFERRED_SECTIONS+=("$name")
  else
    DISCOVERED+=(--test "$name")
  fi
done
echo "deferred ${#DEFERRED_SECTIONS[@]} file(s) to their own sections: ${DEFERRED_SECTIONS[*]}"
echo "running $(( ${#DISCOVERED[@]} / 2 )) discovered test file(s)"
run_counted discovered-ignored 0 cargo test "${DISCOVERED[@]}" -- --ignored --nocapture

run_counted lib-ignored 0 cargo test --lib -- --ignored --nocapture

echo "=== admin-role tests (they rewrite roles and the audit log) ==="
CUBA_APP_ROLE=0 run_counted admin-role 1 cargo test --test v020_audit_update_applies --test v020_role_separation \
           -- --ignored --nocapture

require_present "tests that need the embedding model" "$ONNX_MODEL_PATH/model_quantized.onnx" \
  run_counted embedding-model 1 cargo test --test v016_chunking -- --ignored --nocapture
}

lane_NLI() {
require_present "tests that need the NLI model" "$CUBA_NLI_PATH" \
  run_counted nli 1 cargo test --test nli_entailment --test nli_cost --test nli_probe -- --ignored --nocapture
}

lane_D3() {
require_generative_llm "tests that need a generative LLM (extract / relation-scan)" \
  run_counted generative-llm 1 cargo test --test v016_extract_without_sampling --test v017_relation_scan \
             -- --ignored --nocapture
}

# The release build carries --features cuda. Without it, gpu::wants_gpu() returns false
# unconditionally, CUBA_RERANK_DEVICE=gpu goes inert, and the reranker runs on CPU at
# 20.669s per query against 0.356s — 58x, measured in d7922fa. The E2E allows 15s per call,
# so every one of its 40 calls times out and the gate can never pass on a machine that has
# a GPU configured.
#
# CI-F1 (D-F3a, D-F3b): one release compile for the whole judge, with --no-run, so it leaves
# the binary the E2E judges and runs no test. build-gpu.sh is not called: it capped its own
# memory and nested systemd-run and nice inside pesado's scope. --bins is not in this call
# (it would compile main.rs's harness with LTO): B3g adds it and must find every unit fresh.
# The sha256 of the binary is taken here and checked after B3g, before the E2E and before
# the doctor, so no step between them can change the binary without the gate refusing it.
release_sha() {
  sha256sum "$1" | cut -d' ' -f1
}
sha_unchanged() {
  local now
  now="$(release_sha "$REL_BIN")"
  if [[ "$now" != "$REL_SHA" ]]; then
    echo "FAIL: sha256 changed $1: $REL_BIN was $REL_SHA after the --no-run build and is $now now. The E2E would judge a binary this gate did not build." >&2
    exit 1
  fi
}

lane_B3() {
echo "=== release build (one cargo call with --no-run, the feature set production runs) ==="
export CARGO_BUILD_JOBS="$GATE_RELEASE_JOBS"
cargo test --release --features cuda --no-run --lib --test v017_rerank_gpu
REL_BIN="$(resolve_binary_path)"
[[ -f "$REL_BIN" ]] || { echo "FAIL: the release build left no binary at $REL_BIN." >&2; exit 1; }
REL_SHA="$(release_sha "$REL_BIN")"

echo "=== B3g (every unit of the bins is fresh after the --no-run build) ==="
b3g_json="$(mktemp)"
cargo build --release --features cuda --bins --message-format=json >"$b3g_json" \
  || { rm -f "$b3g_json"; echo "FAIL: the release build of the bins failed (B3g)." >&2; exit 1; }
check_fresh "$b3g_json" || { rm -f "$b3g_json"; exit 1; }
rm -f "$b3g_json"
sha_unchanged "after B3g"
}

# --features cuda here too. The feature set is part of what cargo fingerprints,
# so a second release call asking for a different one rebuilds this same target
# directory and relinks the binary the E2E and the placement check below read.
# It fires at COMPILE time, not at run time, which is what makes it invisible:
# reproduced with `-- --list`, which executes no test at all, and doctor went
# from `ok — cuda · reranker=gpu` to `warn — built without support`. The E2E
# then drove a CPU binary, and this test, whose expectations are themselves
# `cfg!(feature = "cuda")`, flipped them to the CPU answer and passed without
# entering a single CUDA branch. Matching the two steps also stops the gate
# linking release twice (3m06s + 3m03s, last measured run).
# `-- --ignored` selects ONLY the ignored tests, so the four plain #[test]s in
# that same file — the ones that say what decides the reranker's fixed batch
# shape — still went out with the debug `cargo test` far above, without the
# feature. There gpu::wants_gpu() returns false on the cfg! check before it ever
# reads the device variable, so every assertion that names a device is true and
# unable to fail. ba0bf97 made them COMPILE with cuda; this call is what makes
# them run under it. Same feature set as the release build above, so nothing rebuilds.
#
# Not `--include-ignored` on the call below, which would look cheaper: that puts
# is_configured_reports_whether_a_model_is_on_disk_without_loading_it, which
# points CUBA_RERANKER_PATH at a directory with no model in it, in the same
# process as the two tests that load the real one.
#
# Outside require_present on purpose: these four need nothing on disk, and a
# machine with no reranker installed must still hear whether its placement
# contracts hold. The model-bound ones stay behind the guard, where a missing
# model is a FAIL and never a skip.
lane_R() {
echo "=== reranker placement contracts (GPU feature live, no model needed) ==="
run_counted reranker-placement 1 cargo test --release --features cuda --test v017_rerank_gpu -- --nocapture

# The library half of the same hole. The cfg! that actually decides placement is
# in gpu::wants_gpu, which returns false before reading the device variable when
# no GPU feature is compiled in, and the table that judges it is
# gpu::placement_tests — five tests that both `cargo test` calls above compile
# without the feature, so the device knob is inert there and half of that table
# is false == false for the same reason v017_rerank_gpu was.
#
# Release and not debug, for cost: the release build above already built this
# dependency graph with cuda, so the only thing compiled here is the lib test
# unit itself. In debug it would be a third feature set (debug bare, release
# cuda, debug cuda) and a full rebuild of everything under it.
echo "=== placement table under the feature that changes it ==="
run_counted placement-table 0 cargo test --release --features cuda --lib gpu:: -- --nocapture

require_present "reranker tests (release: 387s in debug, seconds here)" \
  "$CUBA_RERANKER_PATH/model.onnx" \
  run_counted reranker-model 1 cargo test --release --features cuda --test v017_rerank_gpu -- --ignored --nocapture

check_section_lines

sha_unchanged "before E2E"
}

lane_E() {
export DATABASE_URL="$GATE_DATABASE_URL"
REL_BIN="${REL_BIN:-$(resolve_binary_path)}"
REL_SHA="${REL_SHA:-$(release_sha "$REL_BIN")}"
echo "=== E2E (25 MCP tools, subprocess per call) ==="
CUBA_BINARY_PATH="$(resolve_binary_path)"
export CUBA_BINARY_PATH
echo "E2E binary: $CUBA_BINARY_PATH"
# Prefer PYTHON_BIN (set by run-gate on Windows). Never use the Microsoft Store
# python3.exe stub under WindowsApps — it prints install text and exits 49.
echo "python=$PY"
export PYTHONUTF8=1
export PYTHONIOENCODING=utf-8
"$PY" tests/e2e_all_tools.py

# The gate has always built with --features cuda (build-gpu.sh, above) and then
# asserted nothing about where the work landed. A CUDA build that degrades to
# the CPU answers every call correctly and only costs 58x, so nothing about a
# green run distinguishes it from one on a card.
#
# What is asserted here is the DECISION, not the kernel: no merge machine
# without a GPU can show that a kernel executed on one. doctor's `gpu` check is
# compared against what this machine independently says it has. A machine with
# no card reporting CPU is the correct answer and stays green — if it did not,
# nobody would keep the step.
echo "=== GPU placement (doctor --json must agree with this machine) ==="
if [[ ! -f "$CUBA_BINARY_PATH" ]]; then
  echo "FAIL: no binary at $CUBA_BINARY_PATH, so there is nothing to ask about placement." >&2
  echo "      This step reads the release build this run just made (--features cuda)" >&2
  echo "      (--features cuda). Judging any other binary would report on a build that" >&2
  echo "      never had GPU support compiled in and call the answer honest." >&2
  exit 1
fi
sha_unchanged "before the doctor"
doctor_json="$(mktemp)"
doctor_err="$(mktemp)"
doctor_exit=0
DATABASE_URL="$GATE_DATABASE_URL" "$CUBA_BINARY_PATH" doctor --json \
  >"$doctor_json" 2>"$doctor_err" || doctor_exit=$?
if [[ ! -s "$doctor_json" ]]; then
  echo "FAIL: $CUBA_BINARY_PATH doctor --json printed nothing (exit $doctor_exit)." >&2
  echo "      Its stderr:" >&2
  cat "$doctor_err" >&2
  rm -f "$doctor_json" "$doctor_err"
  exit 1
fi
# doctor exits 1 when ANY of its checks fails, and this step judges one check on
# its own. The exit code is reported, not obeyed — and the JSON, not the code,
# is what decides below.
echo "doctor --json exited $doctor_exit ($(wc -c <"$doctor_json") bytes)"
"$ROOT/scripts/gpu-placement-check.sh" "$doctor_json"
rm -f "$doctor_json" "$doctor_err"

echo "=== MCP live session (single process, initialize + tools/list + calls) ==="
"$PY" "$ROOT/scripts/mcp_live_session_test.py"

echo "=== eval harness smoke (a scratch database seeded from smoke-corpus.jsonl) ==="
EVAL_DB=brain_gate_eval
EVAL_DATABASE_URL="${DATABASE_URL%/*}/brain_gate_eval"
EVAL_FIXTURE="$RUST_DIR/eval-datasets/smoke-corpus.jsonl"
EVAL_QUERIES="$RUST_DIR/eval-datasets/smoke.jsonl"
EVAL_BIN="$(resolve_binary_path)"
[[ -x "$EVAL_BIN" ]] \
  || { echo "FAIL: no release binary at $EVAL_BIN for the eval; the release build above writes it." >&2; exit 1; }
# resolve_binary_path also takes the release/cuba-memorys alias: the eval judges the bytes the
# --no-run build left, whatever name they came under (CI-J2, D-F3b).
[[ "$(release_sha "$EVAL_BIN")" == "$REL_SHA" ]] \
  || { echo "FAIL: the eval binary $EVAL_BIN is not the one the release build left (its sha256 differs from $REL_BIN): nothing is judged." >&2; exit 1; }
claim_database "$EVAL_DB" || exit 1
DATABASE_URL="$EVAL_DATABASE_URL" CUBA_APP_ROLE=0 ONNX_MODEL_PATH="" \
  "$EVAL_BIN" doctor >/dev/null 2>&1 || true
if [[ "$CUBA_EMBEDDING_DIM" != "384" ]]; then
  DATABASE_URL="$EVAL_DATABASE_URL" "$ROOT/scripts/migrate-embedding-dim.sh" \
    "$CUBA_EMBEDDING_DIM" >/dev/null 2>&1 || {
      echo "FAIL: could not retype the eval database to vector($CUBA_EMBEDDING_DIM)." >&2
      exit 1
    }
fi
eval_tables="$(psql_url "$EVAL_DATABASE_URL" -Atc \
  "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")"
if ! [[ "$eval_tables" =~ ^[0-9]+$ ]] || (( eval_tables < 20 )); then
  echo "FAIL: could not migrate the eval database (only ${eval_tables:-no} tables)." >&2
  exit 1
fi

seed_out="$(mktemp)"
seed_exit=0
DATABASE_URL="$EVAL_DATABASE_URL" "$PY" -I "$ROOT/scripts/eval_seed.py" --binary "$EVAL_BIN" \
  --fixture "$EVAL_FIXTURE" >"$seed_out" 2>&1 || seed_exit=$?
if (( seed_exit != 0 )); then
  echo "FAIL: the eval seeding exited $seed_exit: $(cat "$seed_out")" >&2
  rm -f "$seed_out"
  exit 1
fi
seeded="$(sed -n 's/^seeded \([0-9][0-9]*\) observations$/\1/p' "$seed_out")"
rm -f "$seed_out"
fixture_lines="$(grep -c . "$EVAL_FIXTURE" || true)"
observed="$(psql_url "$EVAL_DATABASE_URL" -Atc "SELECT count(*) FROM brain_observations")"
if ! [[ "$observed" =~ ^[0-9]+$ ]]; then
  echo "FAIL: could not count the observations of the eval database." >&2
  exit 1
fi
if (( observed == 0 )); then
  echo "FAIL: the eval database has 0 observations after seeding $EVAL_FIXTURE (the seeder said: ${seeded:-no count})." >&2
  exit 1
fi
if [[ "$seeded" != "$fixture_lines" ]] || (( observed != fixture_lines )); then
  echo "FAIL: the eval seeded ${seeded:-no count} observations and the database holds $observed; the fixture has $fixture_lines lines." >&2
  exit 1
fi
echo "OK  eval database seeded: $observed observations, the fixture has $fixture_lines lines"

eval_json="$(mktemp)"
DATABASE_URL="$EVAL_DATABASE_URL" "$EVAL_BIN" eval \
  --dataset "$RUST_DIR/eval-datasets/smoke.jsonl" --k 10 --json >"$eval_json" || {
  eval_rc=$?
  echo "FAIL: eval exited $eval_rc (its output is above)." >&2
  rm -f "$eval_json"
  exit 1
}
queries_lines="$(grep -c . "$EVAL_QUERIES" || true)"
check_eval "$eval_json" "$queries_lines" || { rm -f "$eval_json"; exit 1; }
rm -f "$eval_json"
}

# D-F3y: without arguments the lanes run in idx order; --lane <id> runs the one named above.
if [[ -n "$run_lane" ]]; then
  "lane_$run_lane"
  exit 0
fi
for lane in "${DAG_LANES[@]}"; do
  "lane_$lane"
done

echo ""
echo "All tests passed."
