#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT/rust"
export CUBA_JUDGE="${CUBA_JUDGE:-heuristic}"
# This script also runs alone, so it carries the same refusal as merge-gate.sh: a process it starts
# with no DATABASE_URL stops with exit 2 instead of reaching the developer's own database.
export MEMORY_INDUSTRY_REQUIRE_DATABASE_URL=1

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

# B3g (CI-F1, D-F3b): judges the JSON of `cargo build --release --features docs,cuda --bins
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

# --- the test counts (CI-J, D-J4b; CI-E1) -------------------------------------
# A section that runs a different number of tests than its line in
# scripts/test-section-counts.txt is red, in either direction: a test that stops
# being run is the same silence as one that is skipped. The count is passed + failed
# over the `test result:` lines of that section's cargo call. A section states either
# one total, `<section> <tests> <motive>`, or one line per test binary,
# `<section>/<linux|windows>/<binary> <tests> <motive>` (CI-E1: the plain pass), never both.
# The judgement is one function, section_count_verdict, which run_counted calls for every
# section and `--check-counts` calls over a log that was saved.
SECTION_COUNTS="$ROOT/scripts/test-section-counts.txt"
# The sections this gate ran and judged, in order (CI-J2, A7): run_counted appends each one. It is
# the only list of sections: check_section_lines compares the count file with it, both ways.
RAN_SECTIONS=""

# section_count_verdict SECTION COUNTS: COUNTS is what `test-times.sh --count` printed (`<binary> <tests>` per
# line). Prints the OK line and returns 0, or prints every FAIL line to stderr and returns 1: every difference at once,
# so one run says all of them. A section with no line at all is not judged here: check_section_lines says it.
section_count_verdict() {
  local section="$1" counts="$2" os=linux ran bins zero expected shape totals binaries
  gate_on_windows && os=windows
  if [[ -r "$SECTION_COUNTS" ]]; then
    shape="$(awk '!/^[[:space:]]*#/ && NF > 0 && index($1, "/") && !(NF >= 3 && $2 ~ /^[0-9]+$/ \
        && $1 ~ "^[a-z][a-z0-9-]*/(linux|windows)/(lib:|bin:|doc:)?[A-Za-z0-9_-]+$") \
        { printf "FAIL: scripts/test-section-counts.txt:%d: %c%s%c is not %c<section>/<linux|windows>/<binary> <tests> <motive>%c\n", NR, 39, $0, 39, 39, 39 }' "$SECTION_COUNTS")"
    if [[ -n "$shape" ]]; then
      printf '%s\n' "$shape" >&2
      return 1
    fi
    totals="$(awk -v s="$section" '!/^[[:space:]]*#/ && NF >= 2 && $1 == s { n++ } END { print n + 0 }' "$SECTION_COUNTS")"
    binaries="$(awk -v s="$section" '!/^[[:space:]]*#/ && NF >= 3 && index($1, s "/") == 1 { n++ } END { print n + 0 }' "$SECTION_COUNTS")"
  else
    totals=0
    binaries=0
  fi
  if (( totals > 0 && binaries > 0 )); then
    echo "FAIL: section $section has a total line and lines per binary in scripts/test-section-counts.txt: one or the other" >&2
    return 1
  fi
  if (( binaries > 0 )); then
    awk -v s="$section" -v os="$os" '
      function err(msg) { print "FAIL: section " s " on " os ": " msg > "/dev/stderr"; bad++ }
      BEGIN { prefix = s "/" os "/" }
      NR == FNR { ran[$1] = $2; order[++n] = $1; ranned += $2; next }
      !/^[[:space:]]*#/ && NF >= 3 && index($1, prefix) == 1 {
        key = substr($1, length(prefix) + 1); want[key] = $2; wanted[++m] = key; lines += $2
      }
      END {
        if (m == 0) {
          print "FAIL: section " s " has lines per binary and none for " os ": the first judge on " os " sets them from its log" > "/dev/stderr"
          exit 1
        }
        for (i = 1; i <= n; i++) {
          k = order[i]
          if (!(k in want)) err(k " ran " ran[k] " tests and has no line")
          else if (want[k] != ran[k]) err(sprintf("%s: expected %d, ran %d (%+d)", k, want[k], ran[k], ran[k] - want[k]))
        }
        for (j = 1; j <= m; j++) if (!(wanted[j] in ran)) err(wanted[j] " has a line (" want[wanted[j]] ") and did not run")
        if (bad > 0) {
          err(bad " binaries differ from scripts/test-section-counts.txt (lines " lines ", ran " ranned "). Change a line only by what the diff of this change adds or removes in that binary; a difference the diff does not explain is a test lost or gained, not a new number.")
          exit 1
        }
        print "OK  section " s ": " ranned " tests in " n " test binaries, each against its line for " os
      }' <(printf '%s\n' "$counts") "$SECTION_COUNTS"
    return
  fi
  read -r ran bins zero <<<"$(awk '{ ran += $2; bins++; if ($2 == 0) zero++ }
    END { printf "%d %d %d\n", ran + 0, bins + 0, zero + 0 }' <<<"$counts")"
  expected=""
  (( totals == 0 )) || expected="$(awk -v s="$section" '!/^[[:space:]]*#/ && NF >= 2 && $1 == s { print $2; exit }' "$SECTION_COUNTS")"
  if [[ -n "$expected" && ! "$expected" =~ ^[0-9]+$ ]]; then
    echo "FAIL: scripts/test-section-counts.txt says '$expected' for section $section, which is not a number." >&2
    return 1
  fi
  if [[ -n "$expected" && "$expected" != "$ran" ]]; then
    echo "FAIL: section $section: expected $expected, ran $ran tests ($bins test binaries). scripts/test-section-counts.txt says $expected: change that line only with the log that proves the new number." >&2
    return 1
  fi
  echo "OK  section $section: $ran tests in $bins test binaries"
}

# --check-counts SECTION LOG judges a cargo log that was saved against the lines of the section, with the function the
# judge itself uses. It opens no database, takes no lock and runs no cargo: 0 green, 1 red, 2 when it cannot read.
# Only the lines of this host's system are judged (gate_on_windows), whichever system wrote them.
if [[ "${1:-}" == "--check-counts" ]]; then
  [[ $# -eq 3 ]] || { echo "FAIL: --check-counts takes a section and a cargo log: --check-counts SECTION LOG" >&2; exit 2; }
  [[ -f "$3" ]] || { echo "FAIL: --check-counts: no log at $3" >&2; exit 2; }
  counts="$("$ROOT/scripts/test-times.sh" --count "$3")" \
    || { echo "FAIL: --check-counts: test-times.sh --count could not read $3 (see above)." >&2; exit 2; }
  grep -Eq "^${2}[[:space:]/]" "$SECTION_COUNTS" 2>/dev/null \
    || { echo "FAIL: scripts/test-section-counts.txt has no line for the section $2." >&2; exit 1; }
  section_count_verdict "$2" "$counts" || exit 1
  exit 0
fi

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
  # CI-J9: nor the lane token. A row that spends a ticket sets it itself; a second gate that inherits
  # one is refused by name, and the self-test is red for a reason that is not the row's.
  unset GATE_SERVER_SYSTEM_ID GATE_LANE CUBA_BINARY_PATH ONNX_MODEL_PATH CUBA_RERANKER_PATH CUBA_NLI_PATH \
    CUBA_EMBEDDING_DIM CUBA_GATE_LANE_TOKEN
  # D-J7b (A2): this self-test is hermetic. HOME and XDG_CACHE_HOME are a directory of its own,
  # removed on exit, so a row that does not give them one never reaches the real cache.
  hermetic="$(mktemp -d)"
  mkdir -p "$hermetic/home" "$hermetic/cache"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache"
  # CI-K10 (H1): the rows below launch a copy of merge-gate.sh, which runs the planner (gate-dag.py) for real, and a planner
  # delegates the cgroup it runs in (CI-J17): read from the real /proc/self/cgroup it would make a `juez` inside the scope of
  # whoever launched this self-test and move that scope's processes into it. This self-test gets a cgroup tree of its own with
  # no `0::` line, so the planner has no scope to delegate; a row that needs a cgroup (F1, below) passes its own pair.
  # local_gate_contract.rs gives the same pair to every self-test it launches; this one is for whoever launches it by hand.
  mkdir -p "$hermetic/sin-cgroup/raiz"
  : >"$hermetic/sin-cgroup/propio"
  export GATE_CGROUP_ROOT="$hermetic/sin-cgroup/raiz" GATE_PROC_SELF_CGROUP="$hermetic/sin-cgroup/propio"
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
  env -u CUBA_GATE_LOCK_OWNER -u DATABASE_URL -u CARGO_TARGET_DIR HOME="$tmp/home" PATH="/usr/bin:/bin" \
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
    # CI-F3.3b-2: merge-gate.sh runs its steps through the planner, which reads this table and gate_history.py.
    cp "$ROOT/scripts/gate-dag.py" "$ROOT/scripts/gate_history.py" "$ROOT/scripts/gate-dag.txt" "$t/scripts/"
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
    'echo "stand-in cargo: scopes in the cgroup of the planner: $(grep -c "^0::" "${GATE_PROC_SELF_CGROUP:-/proc/self/cgroup}" 2>/dev/null)"' \
    'exit 1' >"$tmp/bin/cargo"
  chmod +x "$tmp/bin/pg_isready" "$tmp/bin/psql" "$tmp/bin/cargo" "$tmp/bin/node"
  held_exit=0
  # CI-F3.3c (the judge's names): the gate of this tree takes its target root from its own HOME, not from the judge's
  # CARGO_TARGET_DIR, which no user can make (gate-dag.py refuses it with 2 by name).
  env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u CARGO_TARGET_DIR HOME="$tmp/home2" DATABASE_URL="$rat_url" \
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
  # CI-K10 (H1): the planner of this row ran with no scope to delegate. The stand-in reads the cgroup file the planner's step was
  # given: a line `0::` there means the row reached a real cgroup, and the planner may have made a `juez` in it.
  grep -q "stand-in cargo: scopes in the cgroup of the planner: 0$" "$tmp/merge-held.out" \
    || self_fail "the planner of the row that launches merge-gate.sh saw a cgroup scope it could delegate: $(grep 'scopes in the cgroup' "$tmp/merge-held.out")"
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
  env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u CARGO_TARGET_DIR HOME="$tmp/home3" DATABASE_URL="$rat_url" \
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
  { env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u CARGO_TARGET_DIR HOME="$tmp/home4" DATABASE_URL="$rat_url" \
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
    # CI-E11 (D-E11e): the files of tests/ that the judge calls contracts, as the script under test names them, so that
    # lanes D1 and CT find every name of JUDGE_CONTRACTS as a file. A script with no such line yet gets the spec's eight, and
    # the rows that read the partition are red for their own reason.
    e11_contracts="$(sed -n 's/^JUDGE_CONTRACTS=(\(.*\))$/\1/p' "$ROOT/scripts/run-all-tests.sh")"
    [[ -n "$e11_contracts" ]] || e11_contracts="ci_contract gate_dag_contract gate_history_contract gate_usl_contract group_no_server local_gate_contract packaging_contract v047_without_a_database_url_the_judge_reaches_no_default"
    for e11_name in $e11_contracts; do : >"$t/rust/tests/$e11_name.rs"; done
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
      'case "$*" in *--timings*) [ -n "${FAKE_CARGO_NO_TIMING_LINE:-}" ] || echo "   Timing report saved to $CARGO_TARGET_DIR/cargo-timings/cargo-timing-20261109T120000Z.html" >&2 ;; esac' \
      'case "$*" in *--release*) [ -e "$CARGO_TARGET_DIR/release/memory-industry" ] || cp "$(cat "$HOME/standin.path")" "$CARGO_TARGET_DIR/release/memory-industry" ;; esac' \
      '[ -z "${FAKE_CHANGE_RELEASE_ON:-}" ] || case "$*" in *"$FAKE_CHANGE_RELEASE_ON"*) echo changed >"$CARGO_TARGET_DIR/release/memory-industry" ;; esac' \
      'if [ "$1" = build ]; then' \
      '  [ -e "$CARGO_TARGET_DIR/debug/memory-industry" ] || cp "$(cat "$HOME/standin.path")" "$CARGO_TARGET_DIR/debug/memory-industry"' \
      '  case "$*" in *--release*) fresh="${FAKE_CARGO_FRESH:-true}" ;; *) fresh="${FAKE_CARGO_FRESH_DEBUG:-true}" ;; esac' \
      '  case "$*" in *--message-format=*) printf "%s\n" "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"memory-industry\"},\"fresh\":$fresh,\"executable\":\"${FAKE_CARGO_BUILD_EXE:-$CARGO_TARGET_DIR/debug/memory-industry}\"}" ;; esac' \
      'fi' \
      'if [ "$1" = test ]; then' \
      '  n="${FAKE_CARGO_TEST_COUNT:-3}"' \
      '  names=""; prev=""; lib=""; doc=""' \
      '  for a in "$@"; do [ "$prev" = --test ] && names="$names $a"; case "$a" in --lib) lib=1 ;; --doc) doc=1 ;; esac; prev="$a"; done' \
      '  [ -n "$names$lib$doc" ] || names=fixture' \
      '  case "$*" in *--no-run*) ;; *) [ -z "${FAKE_CARGO_COMPILING:-}" ] || echo "   Compiling memory-industry v0.31.0 ($PWD)" >&2 ;; esac' \
      '  if [ -n "$lib" ]; then' \
      '    echo "     Running unittests src/lib.rs (target/debug/deps/memory_industry-0123456789abcdef)"' \
      '    echo "running $n tests"' \
      '    echo "test result: ok. $n passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"' \
      '  fi' \
      '  for t in $names; do' \
      '    m="$n"; [ "$t" = "${FAKE_CARGO_ZERO_TEST-}" ] && m=0' \
      '    echo "     Running tests/$t.rs (target/debug/deps/$t-0000)"' \
      '    echo "running $m tests"' \
      '    echo "test result: ok. $m passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"' \
      '  done' \
      '  if [ -n "$doc" ]; then' \
      '    echo "   Doc-tests memory_industry"' \
      '    echo "running 0 tests"' \
      '    echo "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s"' \
      '  fi' \
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
      '  *"count(*) FROM pg_database"*) if grep -q "CREATE DATABASE brain_gate -c" "$HOME/psql.calls"; then echo 1; else echo 0; fi ;;' \
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
  full_run j21w OSTYPE=msys FAKE_CARGO_BUILD_EXE='C:\\Users\\x\\target\\debug\\memory-industry.exe'
  if grep -q 'reported no executable' "$tmp/j21w.out"; then
    self_fail "J2.1 (P1-c): a Windows-form executable that cargo reported was refused: $(cat "$tmp/j21w.out")"
  fi
  grep -qF "$tmp/j21w/standin/memory-industry doctor" "$tmp/j21w/home/bin.calls" 2>/dev/null \
    || self_fail "J2.1 (P1-c): doctor did not run with the path cygpath -u maps the Windows one to: $(cat "$tmp/j21w/home/bin.calls" 2>/dev/null || echo 'no doctor call')"

  # K9a.2: on Linux nobody asks cygpath. The same stand-in, which notes every call it gets, sits first on PATH and the
  # shell says it is not Windows (OSTYPE is bash's own here): the Windows-form executable is refused as it is, and the
  # stand-in was never called. Looking for cygpath on PATH decided "Windows" by what the PATH held, and on this
  # machine the PATH of Windows is 68 directories of 9p.
  full_tree j21l
  printf '#!/bin/sh\necho called >>"%s"\necho %s\n' "$tmp/j21l/home/cygpath.calls" "$tmp/j21l/standin/memory-industry" >"$tmp/j21l/bin/cygpath"
  chmod +x "$tmp/j21l/bin/cygpath"
  full_run j21l FAKE_CARGO_BUILD_EXE='C:\\Users\\x\\target\\debug\\memory-industry.exe'
  [[ ! -s "$tmp/j21l/home/cygpath.calls" ]] \
    || self_fail "K9a.2: on Linux run-all-tests.sh asked cygpath $(wc -l <"$tmp/j21l/home/cygpath.calls") time(s) to map the executable of cargo"
  grep -q 'reported no executable' "$tmp/j21l.out" \
    || self_fail "K9a.2: on Linux a Windows-form executable that is no file here was not refused as 'reported no executable': $(cat "$tmp/j21l.out")"

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

  # K4g (CI-K4, D-K4g): every cargo call that compiles (build, test, clippy) asks for `--timings`, before the `--` that
  # hands the rest to the test binary, and a call that compiles nothing (fmt) asks for none. The stand-in cargo prints on
  # stderr the line cargo prints when it is given the flag, `Timing report saved to <path>`, which lands in the logs that
  # run_counted reads. The counts of a run with that line are the counts of a run without it (FAKE_CARGO_NO_TIMING_LINE).
  full_tree k4g
  full_run k4g
  (( rc == 0 )) || self_fail "K4g: a full run whose every step passed exited $rc: $(cat "$tmp/k4g.out")"
  [[ -s "$tmp/k4g/home/cargo.calls" ]] || self_fail "K4g: no cargo call was recorded"
  for k4_sub in build test clippy fmt; do
    grep -q "^$k4_sub" "$tmp/k4g/home/cargo.calls" \
      || self_fail "K4g: the run made no cargo $k4_sub call, so the rule is not read over it: $(cat "$tmp/k4g/home/cargo.calls")"
  done
  while IFS= read -r k4_call; do
    k4_sub="${k4_call%% *}"
    k4_before="${k4_call%% -- *}"
    case "$k4_sub" in
      build|test|clippy)
        case " $k4_before " in
          *" --timings "*) ;;
          *) self_fail "K4g: the cargo call '$k4_call' compiles and carries no --timings before its --" ;;
        esac ;;
      *)
        case " $k4_call " in
          *" --timings "*) self_fail "K4g: the cargo call '$k4_call' compiles nothing and was given --timings" ;;
        esac ;;
    esac
  done <"$tmp/k4g/home/cargo.calls"
  grep -q 'Timing report saved to' "$tmp/k4g.out" \
    || self_fail "K4g: no call asked for --timings, so the line cargo prints for it never reached the log: $(cat "$tmp/k4g.out")"
  full_tree k4quiet
  full_run k4quiet FAKE_CARGO_NO_TIMING_LINE=1
  (( rc == 0 )) || self_fail "K4g: a full run with no timing line exited $rc: $(cat "$tmp/k4quiet.out")"
  grep -q 'Timing report saved to' "$tmp/k4quiet.out" \
    && self_fail "K4g: the run that was to print no timing line printed one: $(grep 'Timing report' "$tmp/k4quiet.out")"
  [[ -n "$(grep '^OK  section ' "$tmp/k4quiet.out")" ]] || self_fail "K4g: the run printed no OK  section line"
  [[ "$(grep '^OK  section ' "$tmp/k4g.out")" == "$(grep '^OK  section ' "$tmp/k4quiet.out")" ]] \
    || self_fail "K4g: the counts of the sections are not the same with and without the timing line: $(diff <(grep '^OK  section ' "$tmp/k4g.out") <(grep '^OK  section ' "$tmp/k4quiet.out") || true)"

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
  # CI-E11: lane B1 compiles the debug tests of T_dbg with its own `--no-run` call, so the release one is the --no-run call
  # with --release in it.
  noRun="$(grep -- '--no-run' "$tmp/f31/home/cargo.calls" 2>/dev/null | grep -- '--release' || true)"
  [[ "$(grep -- '--no-run' "$tmp/f31/home/cargo.calls" 2>/dev/null | grep -c -- '--release' || true)" == 1 ]] \
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
  # CI-E28: an argument that is not one of the modes was ignored and the whole judge started. Now it is exit 2 naming it and
  # the ones the script takes, before the lock, a lane or cargo; after a list of lanes too.
  lane_run arg_unknown --lan D2
  (( rc == 2 )) || self_fail "CI-E28: an unknown first argument exits $rc, not 2: $(cat "$tmp/arg_unknown.out")"
  grep -q 'unknown argument: --lan ' "$tmp/arg_unknown.out" \
    || self_fail "CI-E28: an unknown first argument is not named ('unknown argument: --lan'): $(cat "$tmp/arg_unknown.out")"
  grep -q -- '--lane <id>' "$tmp/arg_unknown.out" \
    || self_fail "CI-E28: the refusal does not name --lane <id>, which the script takes: $(cat "$tmp/arg_unknown.out")"
  grep -q -- '--check-models' "$tmp/arg_unknown.out" \
    || self_fail "CI-E28: the refusal does not name --check-models, which the script takes: $(cat "$tmp/arg_unknown.out")"
  [[ ! -e "$tmp/arg_unknown/home/cargo.calls" && ! -e "$tmp/arg_unknown/home/order.log" ]] \
    || self_fail "CI-E28: a refused argument touched cargo or a lane: $(cat "$tmp/arg_unknown/home/cargo.calls" "$tmp/arg_unknown/home/order.log" 2>/dev/null)"
  lane_run arg_after_lane --lane D2 --bogus
  (( rc == 2 )) || self_fail "CI-E28: an unknown argument after a lane exits $rc, not 2: $(cat "$tmp/arg_after_lane.out")"
  grep -q 'unknown argument: --bogus ' "$tmp/arg_after_lane.out" \
    || self_fail "CI-E28: an unknown argument after a lane is not named: $(cat "$tmp/arg_after_lane.out")"
  [[ ! -e "$tmp/arg_after_lane/home/cargo.calls" ]] || self_fail "CI-E28: an unknown argument after a lane still ran the lane"
  # CI-E28: the disk below the floor is the machine, exit 2 and not the 1 of a red of the product.
  full_tree lowdisk
  full_run lowdisk FAKE_DF_GB=3 CUBA_GATE_MIN_FREE_GB=8
  (( rc == 2 )) || self_fail "CI-E28: with 3G free and a floor of 8G the run exits $rc, not 2: $(cat "$tmp/lowdisk.out")"
  grep -q 'only 3G free' "$tmp/lowdisk.out" \
    || self_fail "CI-E28: the refusal for the disk does not say 'only 3G free': $(cat "$tmp/lowdisk.out")"
  [[ ! -e "$tmp/lowdisk/home/cargo.calls" ]] || self_fail "CI-E28: a run with no disk reached cargo"
  lane_run lane_d2 --lane D2
  (( rc == 0 )) || self_fail "F3.3a: --lane D2 exits $rc, not 0: $(cat "$tmp/lane_d2.out")"
  grep -q -- '--ignored' "$tmp/lane_d2/home/cargo.calls" 2>/dev/null \
    || self_fail "F3.3a: --lane D2 ran none of its --ignored tests: $(cat "$tmp/lane_d2/home/cargo.calls" 2>/dev/null)"
  grep -Eq -- 'fmt|clippy|--release' "$tmp/lane_d2/home/cargo.calls" 2>/dev/null \
    && self_fail "F3.3a: --lane D2 ran another lane's cargo call: $(cat "$tmp/lane_d2/home/cargo.calls")"
  grep -q 'All tests passed' "$tmp/lane_d2.out" \
    && self_fail "F3.3a: --lane D2 printed the verdict of the whole run"

  # CI-F3.3b-1 (D-F3b, spec §4 F3.3b): several --lane in one call run in the order they are given, in one
  # process: CL and D1 share one throwaway database, made once. A refused list touches neither cargo nor a lane.
  # release_run NAME ARGS...: the whole script in NAME's tree with one call's arguments, and the release binary
  # that B3 leaves under the target (so R and E find it as they would after B3 in the same judge).
  release_run() {
    local name="$1" t="$tmp/$1"
    shift
    full_tree "$name"
    cp "$t/standin/memory-industry" "$t/target/release/memory-industry"
    # CI-F3.4 (D-F34i): the record B3 leaves next to the binary, which R and E read when they run in a process of their own.
    sha256sum "$t/target/release/memory-industry" >"$t/target/release/.gate-release.sha256"
    printf '{"content":"fake observation"}\n%.0s' 1 2 3 4 5 >"$t/rust/eval-datasets/smoke-corpus.jsonl"
    rc=0
    env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u PGPORT HOME="$t/home" \
      PATH="$t/bin:/usr/bin:/bin" DATABASE_URL="$rat_url" CARGO_TARGET_DIR="$t/target" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      ONNX_MODEL_PATH="$t/models" CUBA_RERANKER_PATH="$t/reranker" CUBA_NLI_PATH="$t/nli-model" \
      MEMORY_INDUSTRY_LLM_PROVIDER=ollama \
      "$BASH" "$t/scripts/run-all-tests.sh" "$@" >"$tmp/$name.out" 2>&1 || rc=$?
  }
  release_run lane_pair --lane CL --lane D1
  (( rc == 0 )) || self_fail "F3.3b-1: --lane CL --lane D1 exits $rc, not 0: $(cat "$tmp/lane_pair.out")"
  pair_clippy="$(grep -n -m1 'clippy' "$tmp/lane_pair/home/cargo.calls" 2>/dev/null | cut -d: -f1 || true)"
  pair_test="$(grep -n -m1 '^test --lib .*--timings$' "$tmp/lane_pair/home/cargo.calls" 2>/dev/null | cut -d: -f1 || true)"
  [[ -n "$pair_clippy" && -n "$pair_test" && "$pair_clippy" -lt "$pair_test" ]] \
    || self_fail "F3.3b-1: --lane CL --lane D1 did not run CL and then D1 in that order (clippy at ${pair_clippy:-none}, test at ${pair_test:-none}): $(cat "$tmp/lane_pair/home/cargo.calls" 2>/dev/null)"
  [[ "$(grep -c 'CREATE DATABASE brain_gate -c' "$tmp/lane_pair/home/psql.calls" 2>/dev/null || true)" == 1 ]] \
    || self_fail "F3.3b-1: the two lanes of one call did not share one database (CREATE DATABASE brain_gate seen $(grep -c 'CREATE DATABASE brain_gate -c' "$tmp/lane_pair/home/psql.calls" 2>/dev/null || echo 0) times)"
  grep -q 'All tests passed' "$tmp/lane_pair.out" \
    && self_fail "F3.3b-1: a list of lanes printed the verdict of the whole run"
  release_run lane_pair_bad --lane D2 --lane NOPE
  (( rc == 2 )) || self_fail "F3.3b-1: a list with an unknown lane in its second place exits $rc, not 2: $(cat "$tmp/lane_pair_bad.out")"
  grep -q 'no such lane: NOPE' "$tmp/lane_pair_bad.out" \
    || self_fail "F3.3b-1: the refused list does not name the lane it refuses: $(cat "$tmp/lane_pair_bad.out")"
  [[ ! -e "$tmp/lane_pair_bad/home/cargo.calls" && ! -e "$tmp/lane_pair_bad/home/order.log" ]] \
    || self_fail "F3.3b-1: a refused list touched cargo or a lane: $(cat "$tmp/lane_pair_bad/home/cargo.calls" "$tmp/lane_pair_bad/home/order.log" 2>/dev/null)"
  release_run lane_pair_bare --lane D2 --lane
  (( rc == 2 )) || self_fail "F3.3b-1: a list ending in --lane with no id exits $rc, not 2: $(cat "$tmp/lane_pair_bare.out")"
  grep -q 'needs a lane id' "$tmp/lane_pair_bare.out" \
    || self_fail "F3.3b-1: a list ending in --lane with no id does not say so: $(cat "$tmp/lane_pair_bare.out")"
  [[ ! -e "$tmp/lane_pair_bare/home/cargo.calls" ]] || self_fail "F3.3b-1: a list ending in --lane with no id ran cargo"

  # CI-F3.3b-1 (D-F3q): R in its own process reads REL_BIN and REL_SHA from the release binary on disk, as E
  # already does. Before this, R used REL_BIN from a process that had run B3, and alone it died on it unset.
  release_run lane_r_alone --lane R
  (( rc == 0 )) || self_fail "F3.3b-1: --lane R alone exits $rc, not 0 (it has no REL_BIN of its own): $(cat "$tmp/lane_r_alone.out")"
  grep -q 'unbound variable' "$tmp/lane_r_alone.out" \
    && self_fail "F3.3b-1: --lane R alone died on a variable it does not set: $(cat "$tmp/lane_r_alone.out")"
  grep -q 'sha256 changed' "$tmp/lane_r_alone.out" \
    && self_fail "F3.3b-1: --lane R alone reports a sha256 change with nothing changed: $(cat "$tmp/lane_r_alone.out")"

  # CI-F3.3b-1 (D-F3b, D-F3r of the spec §1.1): E in its own process provisions brain_gate when the database is
  # not there, the same provision D1 makes. D1 and E in one call share it, so E makes it only when it is missing.
  release_run lane_e_alone --lane E
  (( rc == 0 )) || self_fail "F3.3b-1: --lane E alone exits $rc, not 0: $(cat "$tmp/lane_e_alone.out")"
  [[ "$(grep -c 'CREATE DATABASE brain_gate -c' "$tmp/lane_e_alone/home/psql.calls" 2>/dev/null || true)" == 1 ]] \
    || self_fail "F3.3b-1: --lane E alone did not provision brain_gate (CREATE DATABASE brain_gate seen $(grep -c 'CREATE DATABASE brain_gate -c' "$tmp/lane_e_alone/home/psql.calls" 2>/dev/null || echo 0) times): $(cat "$tmp/lane_e_alone/home/psql.calls" 2>/dev/null)"

  # CI-F3.4 (D-F34i, R1): B3 leaves the sha256 of the release binary next to it (`<sha>  <path>`), and R and E, each in a
  # process of its own, hold the binary against it. A binary rewritten after B3 is refused by E before it provisions
  # anything, with the sha256 named and the step the record comes from.
  full_tree r1
  printf '{"content":"fake observation"}\n%.0s' 1 2 3 4 5 >"$tmp/r1/rust/eval-datasets/smoke-corpus.jsonl"
  run_args r1 --lane B3
  (( rc == 0 )) || self_fail "F3.4 R1: --lane B3 exits $rc, not 0: $(cat "$tmp/r1.out")"
  r1_bin="$tmp/r1/target/release/memory-industry"
  r1_record="$tmp/r1/target/release/.gate-release.sha256"
  [[ -f "$r1_record" ]] \
    || self_fail "F3.4 R1: B3 left no .gate-release.sha256 next to the release binary: $(ls -A "$tmp/r1/target/release")"
  [[ "$(cut -d' ' -f1 "$r1_record")" == "$(sha256sum "$r1_bin" | cut -d' ' -f1)" ]] \
    || self_fail "F3.4 R1: the record of B3 does not hold the sha256 of the release binary: $(cat "$r1_record")"
  [[ "$(cut -d' ' -f3- "$r1_record")" == "$r1_bin" ]] \
    || self_fail "F3.4 R1: the record of B3 does not name the release binary after its sha256 (<sha>  <path>): $(cat "$r1_record")"
  run_args r1 --lane R
  (( rc == 0 )) || self_fail "F3.4 R1: --lane R after B3, in its own process, exits $rc, not 0: $(cat "$tmp/r1.out")"
  run_args r1 --lane E
  (( rc == 0 )) || self_fail "F3.4 R1: --lane E after B3 and R, in its own process, exits $rc, not 0: $(cat "$tmp/r1.out")"
  # A comment after the stand-in's last command: the sha256 changes and the binary still runs, so an E that took the binary
  # without holding it against B3's record would end green here.
  echo "# rewritten after B3" >>"$r1_bin"
  run_args r1 --lane E
  (( rc == 1 )) \
    || self_fail "F3.4 R1: E accepted a release binary that changed after B3 (exit $rc, want 1): $(cat "$tmp/r1.out")"
  grep -qF 'sha256 changed' "$tmp/r1.out" && grep -qF 'since B3' "$tmp/r1.out" \
    || self_fail "F3.4 R1: E refused the changed binary without saying 'sha256 changed' and 'since B3': $(cat "$tmp/r1.out")"

  # CI-F3.4 (D-F34i, R2): without B3's record R and E do not guess. The binary is on disk and nothing says it is the one B3
  # built, so R refuses, naming what is missing.
  full_tree r2
  cp "$tmp/r2/standin/memory-industry" "$tmp/r2/target/release/memory-industry"
  run_args r2 --lane R
  (( rc == 1 )) \
    || self_fail "F3.4 R2: R ran without B3's record (exit $rc, want 1): $(cat "$tmp/r2.out")"
  grep -qF 'B3 left no sha256 record' "$tmp/r2.out" \
    || self_fail "F3.4 R2: R refused without saying 'B3 left no sha256 record': $(cat "$tmp/r2.out")"

  # CI-F3.4 (D-F34h, R3): E provisions brain_gate with the release binary and compiles nothing: the fake cargo logs every
  # call, and there is none. The provision is the `doctor` the binary runs with no arguments, the one whose line the
  # stand-in logs without --json (the placement check logs `doctor --json`).
  release_run lane_e_nocargo --lane E
  (( rc == 0 )) || self_fail "F3.4 R3: --lane E alone exits $rc, not 0: $(cat "$tmp/lane_e_nocargo.out")"
  [[ ! -e "$tmp/lane_e_nocargo/home/cargo.calls" ]] \
    || self_fail "F3.4 R3: lane E ran cargo: $(cat "$tmp/lane_e_nocargo/home/cargo.calls")"
  [[ "$(grep -c 'CREATE DATABASE brain_gate -c' "$tmp/lane_e_nocargo/home/psql.calls" 2>/dev/null || true)" == 1 ]] \
    || self_fail "F3.4 R3: --lane E alone did not provision brain_gate once: $(cat "$tmp/lane_e_nocargo/home/psql.calls" 2>/dev/null)"
  grep -qxF "$tmp/lane_e_nocargo/target/release/memory-industry doctor" "$tmp/lane_e_nocargo/home/bin.calls" 2>/dev/null \
    || self_fail "F3.4 R3: the provision did not call the release binary's doctor: $(cat "$tmp/lane_e_nocargo/home/bin.calls" 2>/dev/null)"

  # CI-J14i: the probes of the authenticated CLIs (`claude -p`, `gemini -p`, lane D3) have a deadline, 60 s unless
  # GATE_LLM_PROBE_TIMEOUT_S says another (the rows say 1), and a CLI that does not answer in it is exit 2 naming the CLI,
  # not a gate that waits for ever. One that fails at once is not a timeout: the next probe still runs. The `timeout` of the
  # tree is a shim that records its arguments and runs the real one, so the default of 60 s is read, not waited for.
  # llm_run NAME CLAUDE_BODY GEMINI_BODY [VAR=value ...]: lane D3 in NAME's tree with those CLIs (an empty body is no CLI)
  # and no provider, under a deadline of its own so a row that waits is red and does not hang.
  llm_run() {
    local name="$1" claude_body="$2" gemini_body="$3" t="$tmp/$1"
    shift 3
    full_tree "$name"
    printf '{"content":"fake observation"}\n%.0s' 1 2 3 4 5 >"$t/rust/eval-datasets/smoke-corpus.jsonl"
    printf '%s\n' '#!/bin/sh' 'echo "$*" >>"$HOME/timeout.args"' 'exec /usr/bin/timeout "$@"' >"$t/bin/timeout"
    [[ -z "$claude_body" ]] || printf '#!/bin/sh\n%s\n' "$claude_body" >"$t/bin/claude"
    [[ -z "$gemini_body" ]] || printf '#!/bin/sh\n%s\n' "$gemini_body" >"$t/bin/gemini"
    chmod +x "$t"/bin/*
    rc=0
    SECONDS=0
    timeout 40 env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u PGPORT -u MEMORY_INDUSTRY_LLM_PROVIDER \
      -u CUBA_LLM_PROVIDER -u MEMORY_INDUSTRY_LLM_BASE_URL -u CUBA_LLM_BASE_URL HOME="$t/home" \
      PATH="$t/bin:/usr/bin:/bin" DATABASE_URL="$rat_url" CARGO_TARGET_DIR="$t/target" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      ONNX_MODEL_PATH="$t/models" CUBA_RERANKER_PATH="$t/reranker" CUBA_NLI_PATH="$t/nli-model" "$@" \
      "$BASH" "$t/scripts/run-all-tests.sh" --lane D3 >"$tmp/$name.out" 2>&1 || rc=$?
    llm_seconds=$SECONDS
  }
  llm_run j14i_claude 'exec sleep 300' '' GATE_LLM_PROBE_TIMEOUT_S=1
  (( rc == 2 )) \
    || self_fail "J14i: claude -p that does not answer left the gate with $rc after ${llm_seconds}s, not 2 (124 is the deadline of this row): $(cat "$tmp/j14i_claude.out")"
  grep -qF 'claude -p did not answer in 1 s' "$tmp/j14i_claude.out" \
    || self_fail "J14i: the refusal does not say that claude -p did not answer in 1 s: $(cat "$tmp/j14i_claude.out")"
  llm_run j14i_gemini '' 'exec sleep 300' GATE_LLM_PROBE_TIMEOUT_S=1
  (( rc == 2 )) \
    || self_fail "J14i: gemini -p that does not answer left the gate with $rc after ${llm_seconds}s, not 2: $(cat "$tmp/j14i_gemini.out")"
  grep -qF 'gemini -p did not answer in 1 s' "$tmp/j14i_gemini.out" \
    || self_fail "J14i: the refusal does not say that gemini -p did not answer in 1 s: $(cat "$tmp/j14i_gemini.out")"
  # The probes that answer, and the one that fails at once, are as they were; the deadline is 60 s unless a row says another.
  llm_run j14i_claude_ok 'exit 0' ''
  grep -qxF 'OK  generative LLM via authenticated claude CLI' "$tmp/j14i_claude_ok.out" \
    || self_fail "J14i: a claude -p that answers is not an LLM any more: $(cat "$tmp/j14i_claude_ok.out")"
  grep -qE '^60 claude -p ' "$tmp/j14i_claude_ok/home/timeout.args" 2>/dev/null \
    || self_fail "J14i: claude -p is not run under timeout 60: $(cat "$tmp/j14i_claude_ok/home/timeout.args" 2>/dev/null)"
  llm_run j14i_gemini_ok 'exit 1' 'exit 0'
  grep -qxF 'OK  generative LLM via authenticated gemini CLI' "$tmp/j14i_gemini_ok.out" \
    || self_fail "J14i: a claude -p that fails at once stopped the probe of gemini -p: $(cat "$tmp/j14i_gemini_ok.out")"
  grep -qE '^60 gemini -p ' "$tmp/j14i_gemini_ok/home/timeout.args" 2>/dev/null \
    || self_fail "J14i: gemini -p is not run under timeout 60: $(cat "$tmp/j14i_gemini_ok/home/timeout.args" 2>/dev/null)"

  # CI-E1 (D-E1a, D-E1b, D-E1e, D-E1f): the plain pass is the section `plain`, counted per binary in both directions.
  # The two lane rows run `--lane D1` in a tree whose counts file has no `plain` total and says `plain/<os>/<binary> N e1`;
  # CI-E11: the call is the library and the files of tests/ that are not contracts of the judge (the tree has one, t1), and the
  # fake cargo prints each with 3 tests, so N=3 is green and N=4 on t1 is -1.
  # The rows of `--check-counts SECTION LOG` judge a cargo log written here, in the form of Linux, with the same
  # function run_counted uses: nothing here compiles, and the tree they run in is a tree of this self-test (a script
  # that does not know the mode starts the whole fake judge there, and the row is red).
  e1_counts() {
    local name="$1"
    shift
    { awk '$1 != "plain"' "$calib_file"; printf '%s\n' "$@"; } >"$tmp/$name/scripts/test-section-counts.txt"
  }
  full_tree e1ok
  e1_os=linux
  # The line is written with the system in a variable on purpose: a chain of lanes calls a line an orphan when its name is
  # nowhere in this script, and this very file is a script that would say it, so the literal must not be written out.
  e1_counts e1ok "plain/$e1_os/lib:memory_industry 3 e1" "plain/$e1_os/t1 3 e1"
  run_args e1ok --lane D1
  (( rc == 0 )) || self_fail "E1: --lane D1 with a line of 3 for each of the 6 tests it runs exited $rc, not 0: $(cat "$tmp/e1ok.out")"
  grep -qF 'OK  section plain: 6 tests in 2 test binaries' "$tmp/e1ok.out" \
    || self_fail "E1: the plain pass is not counted by run_counted (no 'OK  section plain: 6 tests in 2 test binaries'): $(cat "$tmp/e1ok.out")"
  { [[ "$(grep -c '^test' "$tmp/e1ok/home/cargo.calls" 2>/dev/null || true)" == 1 ]] \
    && grep -qx 'test --lib --test t1 --timings' "$tmp/e1ok/home/cargo.calls"; } \
    || self_fail "E1: the plain pass did not run cargo test once, as 'test --lib --test t1 --timings' (CI-E11): $(cat "$tmp/e1ok/home/cargo.calls" 2>/dev/null)"
  full_tree e1off
  e1_counts e1off "plain/$e1_os/lib:memory_industry 3 e1" "plain/$e1_os/t1 4 e1"
  run_args e1off --lane D1
  (( rc == 1 )) || self_fail "E1: a line of 4 against 3 tests run exited $rc, not 1: $(cat "$tmp/e1off.out")"
  grep -qF 'section plain on linux: t1: expected 4, ran 3 (-1)' "$tmp/e1off.out" \
    || self_fail "E1: the binary that differs is not named with both numbers and the difference: $(cat "$tmp/e1off.out")"

  # e1_log FILE KEY=TESTS...: a cargo log in the form of Linux, one block per binary. KEY is the judge's name for it:
  # a file of tests/, or lib:, bin: and doc: with the crate.
  e1_log() {
    local file="$1" spec key n header
    shift
    : >"$file"
    for spec in "$@"; do
      key="${spec%=*}"
      n="${spec##*=}"
      case "$key" in
        lib:*) header="     Running unittests src/lib.rs (target/debug/deps/${key#lib:}-0123456789abcdef)" ;;
        bin:*) header="     Running unittests src/main.rs (target/debug/deps/${key#bin:}-fedcba9876543210)" ;;
        doc:*) header="   Doc-tests ${key#doc:}" ;;
        *) header="     Running tests/$key.rs (target/debug/deps/$key-0123456789abcdef)" ;;
      esac
      printf '%s\n\nrunning %s tests\ntest result: ok. %s passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n' \
        "$header" "$n" "$n" >>"$file"
    done
  }
  # e1_mode: --check-counts plain over the log of the tree e1c, with its counts file as the row left it; the system
  # is Linux unless E1_OS says another: bash takes OSTYPE from the environment, which is how a row says msys.
  e1_mode() {
    OSTYPE="${E1_OS:-linux-gnu}" run_args e1c --check-counts plain "$tmp/e1c/plain.log"
  }
  full_tree e1c
  e1_log "$tmp/e1c/plain.log" lib:memory_industry=5 bin:memory_industry=0 doc:memory_industry=0 grupo=4 sube=3 vacio=0 nuevo=3
  # The lines and the log of the scenario "todas las diferencias": 5 binaries differ and both sides sum 15.
  printf '%s\n' \
    'plain/linux/lib:memory_industry 5 x' 'plain/linux/bin:memory_industry 0 x' 'plain/linux/doc:memory_industry 0 x' \
    'plain/linux/grupo 5 x' 'plain/linux/sube 2 x' 'plain/linux/vacio 2 x' 'plain/linux/fantasma 1 x' \
    >"$tmp/e1c/scripts/test-section-counts.txt"
  e1_mode
  (( rc == 1 )) || self_fail "E1: lines and log that differ in five binaries exited $rc, not 1: $(cat "$tmp/e1c.out")"
  for want in 'grupo: expected 5, ran 4 (-1)' 'sube: expected 2, ran 3 (+1)' 'vacio: expected 2, ran 0 (-2)' \
    'nuevo ran 3 tests and has no line' 'fantasma has a line (1) and did not run' '5 binaries differ'; do
    grep -qF "$want" "$tmp/e1c.out" \
      || self_fail "E1: the comparison per binary does not say '$want', and it says every difference at once and in both directions (the sums of the two sides are 15 and 15, so a total would not see it): $(cat "$tmp/e1c.out")"
  done
  [[ ! -e "$tmp/e1c/home/cargo.calls" ]] || self_fail "E1: --check-counts ran cargo: $(cat "$tmp/e1c/home/cargo.calls")"

  # The lines that match the log are green, zero tests and all. The windows lines are there and are not read on Linux.
  printf '%s\n' \
    'plain/linux/lib:memory_industry 5 x' 'plain/linux/bin:memory_industry 0 x' 'plain/linux/doc:memory_industry 0 x' \
    'plain/linux/grupo 4 x' 'plain/linux/sube 3 x' 'plain/linux/vacio 0 x' 'plain/linux/nuevo 3 x' \
    'plain/windows/lib:memory_industry 5 x' 'plain/windows/bin:memory_industry 0 x' 'plain/windows/doc:memory_industry 0 x' \
    'plain/windows/grupo 3 x' 'plain/windows/sube 3 x' 'plain/windows/vacio 0 x' 'plain/windows/nuevo 3 x' \
    >"$tmp/e1c/scripts/test-section-counts.txt"
  e1_mode
  (( rc == 0 )) || self_fail "E1: lines that equal the log, zero-test binaries included, exited $rc, not 0 (the windows lines are not for Linux): $(cat "$tmp/e1c.out")"
  grep -qF 'OK  section plain: 15 tests in 7 test binaries, each against its line for linux' "$tmp/e1c.out" \
    || self_fail "E1: the green verdict is not 'OK  section plain: 15 tests in 7 test binaries, each against its line for linux': $(cat "$tmp/e1c.out")"
  # The same counts file on Windows: the windows lines are the ones read, and grupo ran 4 against its 3.
  E1_OS=msys e1_mode
  (( rc == 1 )) || self_fail "E1: on Windows, the line of grupo (3) against 4 tests run exited $rc, not 1: $(cat "$tmp/e1c.out")"
  grep -qF 'section plain on windows: grupo: expected 3, ran 4 (+1)' "$tmp/e1c.out" \
    || self_fail "E1: on Windows the verdict is not about the windows lines, 'section plain on windows: grupo: expected 3, ran 4 (+1)': $(cat "$tmp/e1c.out")"
  # With no windows lines at all there is nothing to judge a Windows host by, and that is red.
  printf '%s\n' \
    'plain/linux/lib:memory_industry 5 x' 'plain/linux/bin:memory_industry 0 x' 'plain/linux/doc:memory_industry 0 x' \
    'plain/linux/grupo 4 x' 'plain/linux/sube 3 x' 'plain/linux/vacio 0 x' 'plain/linux/nuevo 3 x' \
    >"$tmp/e1c/scripts/test-section-counts.txt"
  E1_OS=msys e1_mode
  (( rc == 1 )) || self_fail "E1: on Windows with lines for linux only exited $rc, not 1: $(cat "$tmp/e1c.out")"
  grep -qF 'has lines per binary and none for windows' "$tmp/e1c.out" \
    || self_fail "E1: no windows lines is red without saying 'has lines per binary and none for windows': $(cat "$tmp/e1c.out")"
  # A section is a total or lines per binary, not both.
  printf '%s\n' 'plain 15 total' >>"$tmp/e1c/scripts/test-section-counts.txt"
  e1_mode
  (( rc == 1 )) || self_fail "E1: a total and lines per binary for one section exited $rc, not 1: $(cat "$tmp/e1c.out")"
  grep -qF 'a total line and lines per binary' "$tmp/e1c.out" \
    || self_fail "E1: the two forms of one section are red without saying 'a total line and lines per binary': $(cat "$tmp/e1c.out")"
  # A line of the per-binary form that is not of the form is red, with its number and its text.
  printf '%s\n' \
    'plain/linux/lib:memory_industry 5 x' 'plain/linux/bin:memory_industry 0 x' 'plain/linux/doc:memory_industry 0 x' \
    'plain/linux/grupo 4 x' 'plain/linux/sube 3 x' 'plain/linux/vacio 0 x' 'plain/linux/nuevo 3 x' \
    'plain/macos/grupo 4 x' 'plain/linux/sube tres x' \
    >"$tmp/e1c/scripts/test-section-counts.txt"
  e1_mode
  (( rc == 1 )) || self_fail "E1: two malformed lines of the per-binary form exited $rc, not 1: $(cat "$tmp/e1c.out")"
  { grep -qF "test-section-counts.txt:8: 'plain/macos/grupo 4 x'" "$tmp/e1c.out" \
    && grep -qF "test-section-counts.txt:9: 'plain/linux/sube tres x'" "$tmp/e1c.out"; } \
    || self_fail "E1: the two malformed lines are not named with their number and their text (8 and 9): $(cat "$tmp/e1c.out")"
  # A log that is not there cannot be read: 2, not the verdict of a count of nothing.
  run_args e1c --check-counts plain "$tmp/e1c/no-such.log"
  (( rc == 2 )) || self_fail "E1: --check-counts over a log that is not there exited $rc, not 2: $(cat "$tmp/e1c.out")"


  # CI-E11 (D-E11d, D-E11e, D-E11f, D-E11h, spec §4 E11-2 and E11-5 to E11-9). The plain pass is the library and the files of
  # tests/ that are not contracts of the judge (lane D1, the row DBG); the contracts and the doctests run in lane CT, which
  # opens no database; D2 and D3 provision their database if it is missing; a lane that runs tests compiles nothing.
  # e11_run NAME [VAR=value ...] -- ARG...: the script with ARG... in NAME's tree, under the environment run_args gives it and
  # the VAR=value given, which win over it ($rc and $tmp/NAME.out as run_args leaves them).
  e11_run() {
    local name="$1" t="$tmp/$1"
    local -a vars=()
    shift
    while (( $# > 0 )) && [[ "$1" != -- ]]; do vars+=("$1"); shift; done
    shift
    rc=0
    env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u PGPORT HOME="$t/home" \
      PATH="$t/bin:/usr/bin:/bin" DATABASE_URL="$rat_url" CARGO_TARGET_DIR="$t/target" \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      ONNX_MODEL_PATH="$t/models" CUBA_RERANKER_PATH="$t/reranker" CUBA_NLI_PATH="$t/nli-model" \
      MEMORY_INDUSTRY_LLM_PROVIDER=ollama ${vars[@]+"${vars[@]}"} \
      "$BASH" "$t/scripts/run-all-tests.sh" "$@" >"$tmp/$name.out" 2>&1 || rc=$?
  }
  # e11_counts NAME: the counts file of NAME's tree, one line per binary of each section of D-E11e, with the 3 tests the
  # stand-in cargo prints for each (the doctests have none). It is written from the files of the tree and from
  # $e11_contracts, the names of the contracts, so the expected partition is not read from the script under test.
  e11_counts() {
    local t="$tmp/$1" f k
    {
      awk '$1 != "plain" && $1 != "contracts" && $1 != "doc"' "$calib_file"
      printf '%s\n' 'plain/linux/lib:memory_industry 3 e11'
      for f in "$t"/rust/tests/*.rs; do
        k="$(basename "$f" .rs)"
        if [[ " $e11_contracts " == *" $k "* ]]; then printf 'contracts/linux/%s 3 e11\n' "$k"; else printf 'plain/linux/%s 3 e11\n' "$k"; fi
      done
      printf '%s\n' 'doc/linux/doc:memory_industry 0 e11'
    } >"$t/scripts/test-section-counts.txt"
  }
  # e11_tests_of FILE: the names given to --test by the cargo calls in FILE (cargo.calls), one per line, sorted.
  e11_tests_of() { grep -o -- '--test [A-Za-z0-9_]*' "$1" 2>/dev/null | cut -d' ' -f2 | sort || true; }

  # E11-2: each file of tests/ runs without --ignored in exactly one of the two lanes, D1 or CT, each in its own process; the
  # library only in D1 and the doctests only in CT. The tree has two files besides the contracts and t1.
  full_tree e11p
  for e11_name in smoke_test nli_entailment; do : >"$tmp/e11p/rust/tests/$e11_name.rs"; done
  e11_counts e11p
  e11_run e11p -- --lane D1
  (( rc == 0 )) || self_fail "E11-2: --lane D1 exits $rc, not 0: $(cat "$tmp/e11p.out")"
  cp "$tmp/e11p/home/cargo.calls" "$tmp/e11p.d1.calls"
  : >"$tmp/e11p/home/cargo.calls"
  e11_run e11p -- --lane CT
  (( rc == 0 )) || self_fail "E11-2: --lane CT exits $rc, not 0: $(cat "$tmp/e11p.out")"
  cp "$tmp/e11p/home/cargo.calls" "$tmp/e11p.ct.calls"
  [[ "$(grep -c -- '--lib' "$tmp/e11p.d1.calls" || true)" == 1 && "$(grep -c -- '--lib' "$tmp/e11p.ct.calls" || true)" == 0 ]] \
    || self_fail "E11-2: --lib is not in D1 alone: D1 $(grep -c -- '--lib' "$tmp/e11p.d1.calls" || true), CT $(grep -c -- '--lib' "$tmp/e11p.ct.calls" || true)"
  [[ "$(grep -c -- '--doc' "$tmp/e11p.ct.calls" || true)" == 1 && "$(grep -c -- '--doc' "$tmp/e11p.d1.calls" || true)" == 0 ]] \
    || self_fail "E11-2: --doc is not in CT alone: CT $(grep -c -- '--doc' "$tmp/e11p.ct.calls" || true), D1 $(grep -c -- '--doc' "$tmp/e11p.d1.calls" || true)"
  e11_d1="$(e11_tests_of "$tmp/e11p.d1.calls")"
  e11_ct="$(e11_tests_of "$tmp/e11p.ct.calls")"
  for e11_file in "$tmp"/e11p/rust/tests/*.rs; do
    e11_name="$(basename "$e11_file" .rs)"
    e11_in_d1="$(grep -cx "$e11_name" <<<"$e11_d1" || true)"
    e11_in_ct="$(grep -cx "$e11_name" <<<"$e11_ct" || true)"
    (( e11_in_d1 + e11_in_ct > 0 )) \
      || self_fail "E11-2: $e11_name runs in no row (D1: $(tr '\n' ' ' <<<"$e11_d1")| CT: $(tr '\n' ' ' <<<"$e11_ct"))"
    (( e11_in_d1 == 0 || e11_in_ct == 0 )) \
      || self_fail "E11-2: $e11_name runs in D1 and CT"
    (( e11_in_d1 + e11_in_ct == 1 )) \
      || self_fail "E11-2: $e11_name runs $(( e11_in_d1 + e11_in_ct )) times in one row"
  done
  read -ra e11_names <<<"$e11_contracts"
  [[ "$e11_ct" == "$(printf '%s\n' "${e11_names[@]}" | sort)" ]] \
    || self_fail "E11-2: CT does not run the files of JUDGE_CONTRACTS and nothing else: $(tr '\n' ' ' <<<"$e11_ct")"
  # A name of JUDGE_CONTRACTS that is not a file of tests/ is red in both lanes and says which one, before any test runs.
  for e11_lane in D1 CT; do
    full_tree "e11bad$e11_lane"
    rm -f "$tmp/e11bad$e11_lane/rust/tests/local_gate_contract.rs"
    e11_run "e11bad$e11_lane" -- --lane "$e11_lane"
    (( rc == 1 )) || self_fail "E11-2: JUDGE_CONTRACTS names local_gate_contract, which is no file, and --lane $e11_lane exited $rc, not 1: $(cat "$tmp/e11bad$e11_lane.out")"
    grep -q 'JUDGE_CONTRACTS.*local_gate_contract\|local_gate_contract.*JUDGE_CONTRACTS' "$tmp/e11bad$e11_lane.out" \
      || self_fail "E11-2: the refusal of --lane $e11_lane does not say which name of JUDGE_CONTRACTS is no file: $(cat "$tmp/e11bad$e11_lane.out")"
    grep -q '^test' "$tmp/e11bad$e11_lane/home/cargo.calls" 2>/dev/null \
      && self_fail "E11-2: --lane $e11_lane ran tests after it found a name of JUDGE_CONTRACTS that is no file: $(cat "$tmp/e11bad$e11_lane/home/cargo.calls")"
  done

  # E11-5 (D-E11d): a section that runs tests and compiled is red and says what it compiled; without the line, green.
  full_tree e11c5
  e11_run e11c5 FAKE_CARGO_COMPILING=1 -- --lane D2
  (( rc == 1 )) || self_fail "E11-5: --lane D2 whose cargo compiled memory-industry exited $rc, not 1: $(cat "$tmp/e11c5.out")"
  grep -qF 'cargo compiled memory-industry in a section that runs tests' "$tmp/e11c5.out" \
    || self_fail "E11-5: a section that compiled is not refused with 'cargo compiled memory-industry in a section that runs tests': $(cat "$tmp/e11c5.out")"
  full_tree e11c5ok
  e11_run e11c5ok -- --lane D2
  (( rc == 0 )) || self_fail "E11-5: --lane D2 whose cargo compiled nothing exited $rc, not 0: $(cat "$tmp/e11c5ok.out")"

  # E11-6 (D-E11d): the provision of a lane that reads (D3 alone) does not compile the binary: a unit of the build that is not
  # fresh is exit 1 before the database is made, and says which. The lanes of B1 compile, so they are green with the same cargo.
  full_tree e11c6
  e11_run e11c6 FAKE_CARGO_FRESH_DEBUG=false -- --lane D3
  (( rc == 1 )) || self_fail "E11-6: --lane D3 with a build that was not fresh exited $rc, not 1: $(cat "$tmp/e11c6.out")"
  grep -qF 'cargo compiled memory-industry' "$tmp/e11c6.out" \
    || self_fail "E11-6: the unit that was not fresh is not named ('cargo compiled memory-industry'): $(cat "$tmp/e11c6.out")"
  grep -q 'CREATE DATABASE' "$tmp/e11c6/home/psql.calls" 2>/dev/null \
    && self_fail "E11-6: the database was made after the build of a reader was not fresh: $(cat "$tmp/e11c6/home/psql.calls")"
  full_tree e11c6b
  e11_run e11c6b FAKE_CARGO_FRESH_DEBUG=false -- --lane CL --lane B1
  (( rc == 0 )) || self_fail "E11-6: --lane CL --lane B1, which compile, exited $rc, not 0, with a build that was not fresh: $(cat "$tmp/e11c6b.out")"

  # E11-7 (D-E11f): D2 and D3 make brain_gate if it is missing, once per process: alone, and after D1 in one call.
  e11_made() { grep -c 'CREATE DATABASE brain_gate -c' "$tmp/$1/home/psql.calls" 2>/dev/null || true; }
  full_tree e11d2
  e11_run e11d2 -- --lane D2
  (( rc == 0 )) || self_fail "E11-7: --lane D2 alone exits $rc, not 0: $(cat "$tmp/e11d2.out")"
  [[ "$(e11_made e11d2)" == 1 ]] || self_fail "E11-7: D2 alone did not provision brain_gate (made $(e11_made e11d2) times): $(cat "$tmp/e11d2/home/psql.calls" 2>/dev/null)"
  full_tree e11d3
  e11_run e11d3 -- --lane D3
  (( rc == 0 )) || self_fail "E11-7: --lane D3 alone exits $rc, not 0: $(cat "$tmp/e11d3.out")"
  [[ "$(e11_made e11d3)" == 1 ]] || self_fail "E11-7: D3 alone did not provision brain_gate (made $(e11_made e11d3) times): $(cat "$tmp/e11d3/home/psql.calls" 2>/dev/null)"
  full_tree e11d12
  e11_run e11d12 -- --lane D1 --lane D2
  (( rc == 0 )) || self_fail "E11-7: --lane D1 --lane D2 exits $rc, not 0: $(cat "$tmp/e11d12.out")"
  [[ "$(e11_made e11d12)" == 1 ]] || self_fail "E11-7: D1 and D2 in one process made brain_gate $(e11_made e11d12) times, not once: $(cat "$tmp/e11d12/home/psql.calls" 2>/dev/null)"

  # E11-8 (D-E11e, D-E11f): CT opens no database and builds no binary: its cargo calls are the eight contracts, then the doctests.
  full_tree e11ct
  e11_run e11ct -- --lane CT
  (( rc == 0 )) || self_fail "E11-8: --lane CT alone exits $rc, not 0: $(cat "$tmp/e11ct.out")"
  grep -q 'CREATE DATABASE' "$tmp/e11ct/home/psql.calls" 2>/dev/null \
    && self_fail "E11-8: CT made a database: $(cat "$tmp/e11ct/home/psql.calls")"
  grep -q '^build' "$tmp/e11ct/home/cargo.calls" 2>/dev/null \
    && self_fail "E11-8: CT built the binary of the database: $(cat "$tmp/e11ct/home/cargo.calls")"
  [[ "$(grep -c '^test' "$tmp/e11ct/home/cargo.calls" || true)" == 2 ]] \
    || self_fail "E11-8: CT did not call cargo test twice (the contracts, then the doctests): $(cat "$tmp/e11ct/home/cargo.calls")"
  { [[ "$(sed -n 1p "$tmp/e11ct/home/cargo.calls" | grep -o -- '--test ' | wc -l | tr -d ' ')" == "${#e11_names[@]}" ]] \
    && sed -n 2p "$tmp/e11ct/home/cargo.calls" | grep -qx 'test --doc --timings'; } \
    || self_fail "E11-8: cargo test is not called with the eight --test and then with --doc: $(cat "$tmp/e11ct/home/cargo.calls")"

  # E11-9 (D-E11h): only a call that compiles (no --lane, or CL, B1 or B3 in the list) sweeps the artifacts of more than 7 days
  # of the target when the disk is below CUBA_GATE_SWEEP_BELOW_GB: another lane may be running a binary that old.
  full_tree e11sw
  mkdir -p "$tmp/e11sw/target/debug/deps"
  : >"$tmp/e11sw/target/debug/deps/old-artifact"
  touch -d '10 days ago' "$tmp/e11sw/target/debug/deps/old-artifact"
  e11_run e11sw FAKE_DF_GB=10 CUBA_GATE_SWEEP_BELOW_GB=20 -- --lane D2
  (( rc == 0 )) || self_fail "E11-9: --lane D2 with the disk below the sweep threshold exits $rc, not 0: $(cat "$tmp/e11sw.out")"
  [[ -e "$tmp/e11sw/target/debug/deps/old-artifact" ]] \
    || self_fail "E11-9: --lane D2, which compiles nothing, swept an artifact of 10 days: another lane may be running it: $(cat "$tmp/e11sw.out")"
  e11_run e11sw FAKE_DF_GB=10 CUBA_GATE_SWEEP_BELOW_GB=20 -- --lane CL --lane B1
  (( rc == 0 )) || self_fail "E11-9: --lane CL --lane B1 with the disk below the sweep threshold exits $rc, not 0: $(cat "$tmp/e11sw.out")"
  [[ ! -e "$tmp/e11sw/target/debug/deps/old-artifact" ]] \
    || self_fail "E11-9: --lane CL --lane B1, which compile, did not sweep the artifact of 10 days: $(cat "$tmp/e11sw.out")"

  echo "OK  self-test: a second gate refuses at once and names the first, merge-gate"
  echo "    included, and the variable its children inherit is no pass for anyone"
  echo "    else; the run-all-tests.sh merge-gate launches works under its lock and"
  echo "    leaves the exit file to it; a dead or reused-pid lock is taken over, a"
  echo "    foreign one is not; an exit drops only the databases whose record is"
  echo "    still its own; a run on its own"
  echo "    refuses a DATABASE_URL it cannot derive brain_gate from, before any step;"
  echo "    the models are looked for under HOME/.cache, where the"
  echo "    binary keeps them, whatever XDG_CACHE_HOME says;"
  echo "    claude -p and gemini -p run under a deadline (60 s, GATE_LLM_PROBE_TIMEOUT_S), and one that does not answer is exit 2 naming it (J14i)"
  exit 0
fi
# --- self-test end ---

# CI-F3.3a (D-F3y, spec §4): the lanes of this script in idx order, as scripts/gate-dag.txt names them.
# --lane <id> runs the lane it names; several --lane run in the order given, in this one process, with one
# database (CI-F3.3b-1). A missing or unknown id, in any place of the list, is refused here, before the lock.
DAG_LANES=(CL B1 D1 CT D2 D3 NLI B3 R E)
run_lanes=()
while [[ "${1:-}" == --lane ]]; do
  [[ -n "${2:-}" ]] || { echo "FAIL: --lane needs a lane id, one of: ${DAG_LANES[*]}" >&2; exit 2; }
  case " ${DAG_LANES[*]} " in
    *" $2 "*) run_lanes+=("$2") ;;
    *) echo "FAIL: no such lane: $2 (the lanes are: ${DAG_LANES[*]})" >&2; exit 2 ;;
  esac
  shift 2
done
# CI-E28: what is left after the lanes is nothing, or --check-models. Anything else used to start the whole judge, and the
# argument was ignored: a misspelt --lane ran every lane. The modes with their own arguments (--print-paths, --check-fresh,
# --check-eval, --check-counts, --self-test) are answered above, before this point.
if [[ $# -gt 0 && "$1" != --check-models ]]; then
  echo "FAIL: unknown argument: $1 (this script takes --lane <id> (repeatable, ids: ${DAG_LANES[*]}), --check-models, --print-paths, --check-fresh FILE, --check-eval JSON QUERIES_LINES, --check-counts SECTION LOG, --self-test, or nothing to run every lane)" >&2
  exit 2
fi

# CI-F3.3b-1b-i: a lane the planner started presents its token and inherits, or exits 2 (gate-lock.sh).
inherit_or_acquire_gate_lock "$GATE_LOCK" || exit $?
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

# CI-E11 (D-E11h): the sweep is by the mtime of the artifacts, and a lane that runs tests (D1, CT, D2, D3, NLI, R, E) runs
# binaries that another lane of the same target may have left a week ago and run now. Only a call that compiles sweeps: with no
# --lane, or with CL, B1 or B3 among them. The floor of MIN_FREE_GB below stays for every call.
call_compiles() {
  local lane
  (( ${#run_lanes[@]} > 0 )) || return 0
  for lane in "${run_lanes[@]}"; do
    case "$lane" in CL|B1|B3) return 0 ;; esac
  done
  return 1
}
if [[ "$(free_gb)" -lt "$SWEEP_BELOW_GB" ]] && call_compiles; then
  sweep_stale_artifacts
fi

if [[ "$(free_gb)" -lt "$MIN_FREE_GB" ]]; then
  echo "FAIL: only $(free_gb)G free on the filesystem holding $(gate_target_dir), and this run needs more." >&2
  echo '      Linking is what breaks first, and it breaks as SIGBUS inside ld with no' >&2
  echo "      mention of disk — that is an hour of looking for a bug that is not there." >&2
  echo "      target directory currently holds: $(du -sh "$(gate_target_dir)" 2>/dev/null | cut -f1)" >&2
  echo "      Free it with: cargo clean --manifest-path $RUST_DIR/Cargo.toml" >&2
  echo "      (or raise the bar with CUBA_GATE_MIN_FREE_GB if you know what you are doing)" >&2
  # 2, like every refusal of the machine of this judge (gate_refuse): it is the environment, not a red of the product (CI-E28).
  exit 2
fi

# --- the test counts (CI-J, D-J4b): the judgement is section_count_verdict, above --------
# What a lane that runs tests says when cargo compiled a unit for it (CI-E11, D-E11d): the line of run_counted and of gate_bin.
say_compiled() {
  echo "FAIL: $1: cargo compiled $2 in a section that runs tests: the tests run what lane B1 (T_dbg) or B3 (T_rel) built; run that lane first (CI-E11, D-E11d)." >&2
}

# run_counted SECTION NAMED <command>: runs the call with its output shown and kept,
# judges the count, and exits with the call's own code when the call failed. The count
# is test-times.sh --count's, the one parse of a cargo log the judge has (D-J4d).
run_counted() {
  local section="$1" named="$2" log counts verdict rc=0 want=0 arg bins zero unit
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
  # CI-E11 (D-E11d): a section that runs tests compiles nothing. B1 (T_dbg) and B3 (T_rel) built what it runs; a unit it
  # compiles again is written over a binary that a lane beside it may be running.
  unit="$(sed -n 's/^[[:space:]]*Compiling \([^ ]*\) .*/\1/p' "$log" | head -n 1)"
  if [[ -n "$unit" ]]; then
    say_compiled "section $section" "$unit"
    rm -f "$log"
    exit 1
  fi
  if ! counts="$("$ROOT/scripts/test-times.sh" --count "$log")"; then
    echo "FAIL: section $section: test-times.sh --count could not read the cargo log (see above)." >&2
    rm -f "$log"
    exit 1
  fi
  rm -f "$log"
  verdict="$(section_count_verdict "$section" "$counts")" || exit 1
  read -r bins zero <<<"$(awk '{ bins++; if ($2 == 0) zero++ } END { printf "%d %d\n", bins + 0, zero + 0 }' <<<"$counts")"
  if [[ "$named" == 1 ]] && { (( bins != want )) || (( zero > 0 )); }; then
    echo "FAIL: section $section: $bins test binaries reported for $want named --test, $zero of them ran 0 tests. Each named binary has to run at least one test." >&2
    exit 1
  fi
  RAN_SECTIONS+=" $section"
  echo "$verdict"
}

# Every section that runs tests has its line, and every line is a section that runs.
check_section_lines() {
  local listed name missing="" orphan=""
  # A line `<section>/<os>/<binary>` belongs to its section, once however many binaries it states (CI-E1).
  listed="$(awk '!/^[[:space:]]*#/ && NF >= 2 { sub(/\/.*/, "", $1); if (!seen[$1]++) print $1 }' "$SECTION_COUNTS" 2>/dev/null || true)"
  for name in $RAN_SECTIONS; do
    grep -qxF "$name" <<<"$listed" || missing+=" $name"
  done
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    if (( ${#run_lanes[@]} == 0 )); then
      case " $RAN_SECTIONS " in
        *" $name "*) ;;
        *) orphan+=" $name" ;;
      esac
    elif ! grep -qw -- "$name" "$ROOT/scripts/run-all-tests.sh"; then
      # CI-F3.3b-1: a chain runs only some sections, so a line is an orphan when no section of the script has its name.
      orphan+=" $name"
    fi
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

# CI-J14i: `claude -p` and `gemini -p` are probes of a service that can sit on a login nobody sees, and lane D3 waited
# for it for ever. They run under GATE_LLM_PROBE_TIMEOUT_S (60 s; the self-test says 1). One that does not answer is exit
# 2 naming it; one that fails at once is only a probe that did not pass, and the next one runs.
llm_cli_answers() {
  local cli="$1" what="$2" limit="${GATE_LLM_PROBE_TIMEOUT_S:-60}" rc=0
  timeout "$limit" "$cli" -p "reply with the single word ok" --output-format text >/dev/null 2>&1 || rc=$?
  if (( rc == 124 )); then
    echo "FAIL: $what — $cli -p did not answer in $limit s (timeout), and the gate does not wait for it." >&2
    exit 2
  fi
  (( rc == 0 ))
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
    if llm_cli_answers claude "$what"; then
      ok=1
      echo "OK  generative LLM via authenticated claude CLI"
    fi
  fi
  if [[ "$ok" -eq 0 ]] && command -v gemini >/dev/null 2>&1; then
    if llm_cli_answers gemini "$what"; then
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
#
# CI-E11 (D-E11d): `gate_bin read` is the form of a lane that reads T_dbg (D1, D2, D3): the binary is B1's, and a unit of
# this call that is not fresh says cargo compiled something there, which is exit 1 before the database is made. B1 calls it
# with no argument and builds.
gate_bin() {
  local out exe stale
  out="$(cargo build --bin memory-industry --message-format=json-render-diagnostics --timings)" \
    || { echo "FAIL: cargo build --bin memory-industry failed: no database is prepared from a binary that did not build." >&2; exit 1; }
  if [[ "${1:-}" == read ]]; then
    stale="$(printf '%s\n' "$out" | grep '"reason":"compiler-artifact"' | grep -v '"fresh":true' \
      | sed -n 's/.*"target":{[^}]*"name":"\([^"]*\)".*/\1/p' | head -n 1 || true)"
    if [[ -n "$stale" ]]; then
      say_compiled "the provision of the database" "$stale"
      exit 1
    fi
  fi
  exe="$(printf '%s\n' "$out" \
    | sed -n 's/.*"reason":"compiler-artifact".*"executable":"\([^"]*\)".*/\1/p' \
    | grep -E '[/\\]memory-industry(\.exe)?$' | tail -n 1 || true)"
  exe="${exe//\\\\/\\}"
  if [[ -n "$exe" ]] && gate_on_windows; then
    exe="$(gate_native_path u "$exe")"
  fi
  [[ -n "$exe" && -x "$exe" ]] \
    || { echo "FAIL: cargo build --bin memory-industry reported no executable for memory-industry (it reported: ${exe:-nothing})." >&2; exit 1; }
  printf '%s\n' "$exe"
}

# CI-F3.4 (D-F34h): with a binary, the one the caller gives; without one, the debug build of gate_bin. The E2E lane
# gives the release binary B3 left, so it compiles nothing and the database is migrated by the binary it judges.
provision_gate_db() {
  local bin="${1:-}"
  [[ -n "$bin" ]] || bin="$(gate_bin read)"
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

# CI-E11 (D-E11f): the lanes that run tests against brain_gate make it when it is not there. D2 and D3 run in a cluster of their
# own (their rows are separate processes) and E in another; in one process (a run with no --lane, or D1 and D2 in one call) the
# first to need it makes it and the rest find it. With a binary, the one given; without one, the debug binary B1 built.
# The check is a `[[ ]] ||` and not an `if` so that lane D1 holds the only call to provision_gate_db that starts a line, the one the
# unit tests of the gate come after (doc_contract.rs).
ensure_gate_db() {
  local present
  present="$(psql_url "$ADMIN_DATABASE_URL" -Atc "SELECT count(*) FROM pg_database WHERE datname = '$GATE_DB'")"
  [[ "$present" == 1 ]] || provision_gate_db "${1:-}"
}

# CI-E11 (D-E11e): the files of tests/ that need no server and are contracts of the judge. They run in lane CT, in the row CT,
# against no database; the library and every other file of tests/ run in lane D1. Every file of tests/ runs without --ignored
# in exactly one of the two, because the split is made here, by name, and never with --skip or `test = false`.
JUDGE_CONTRACTS=(ci_contract gate_dag_contract gate_history_contract gate_usl_contract group_no_server local_gate_contract packaging_contract v047_without_a_database_url_the_judge_reaches_no_default)

# split_tests: PLAIN_TESTS and CONTRACT_TESTS, each a list of `--test <name>` pairs, from tests/*.rs and JUDGE_CONTRACTS. A name of
# JUDGE_CONTRACTS that is no file is refused by name before any test runs: it would be a contract that runs nowhere.
split_tests() {
  local name file
  for name in "${JUDGE_CONTRACTS[@]}"; do
    [[ -f "tests/$name.rs" ]] \
      || { echo "FAIL: JUDGE_CONTRACTS names $name and rust/tests/$name.rs is not a file (CI-E11, D-E11e)." >&2; exit 1; }
  done
  PLAIN_TESTS=()
  CONTRACT_TESTS=()
  for file in tests/*.rs; do
    name="$(basename "$file" .rs)"
    if printf '%s\n' "${JUDGE_CONTRACTS[@]}" | grep -qx "$name"; then
      CONTRACT_TESTS+=(--test "$name")
    else
      PLAIN_TESTS+=(--test "$name")
    fi
  done
}

gate_cargo_jobs

lane_CL() {
echo "=== cargo fmt --check ==="
cargo fmt --check

echo "=== cargo clippy (--all-targets: without it, tests/ is never linted) ==="
cargo clippy --all-targets --timings -- -D warnings
}

# CI-E11 (D-E11b): B1 compiles T_dbg once, with `--no-run`, and the lanes that read it compile nothing: the debug binary of the
# database, built here in compile mode, is the one their provision reads (`gate_bin read`).
lane_B1() {
echo "=== cargo test --no-run (every test binary of T_dbg, built once; the lanes that run them compile nothing) ==="
cargo test --no-run --timings

echo "=== the debug binary of the database ==="
gate_bin >/dev/null
}

lane_D1() {
split_tests
echo "=== throwaway database for every mutating step ==="
provision_gate_db

echo "=== cargo test (the library and the files of tests/ that are not contracts of the judge) ==="
DATABASE_URL="$GATE_DATABASE_URL" run_counted plain 0 cargo test --lib "${PLAIN_TESTS[@]}" --timings
}

# CI-E11 (D-E11e): the contracts of the judge and the doctests, which open no database: the cluster of this lane has no
# brain_gate and nothing here makes it.
lane_CT() {
split_tests
echo "=== the contracts of the judge (no server) ==="
DATABASE_URL="$GATE_DATABASE_URL" run_counted contracts 1 cargo test "${CONTRACT_TESTS[@]}" --timings

echo "=== the doctests ==="
DATABASE_URL="$GATE_DATABASE_URL" run_counted doc 0 cargo test --doc --timings
}

lane_D2() {
ensure_gate_db
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
run_counted discovered-ignored 0 cargo test "${DISCOVERED[@]}" --timings -- --ignored --nocapture

run_counted lib-ignored 0 cargo test --lib --timings -- --ignored --nocapture

echo "=== admin-role tests (they rewrite roles and the audit log) ==="
CUBA_APP_ROLE=0 run_counted admin-role 1 cargo test --test v020_audit_update_applies --test v020_role_separation --timings \
           -- --ignored --nocapture

require_present "tests that need the embedding model" "$ONNX_MODEL_PATH/model_quantized.onnx" \
  run_counted embedding-model 1 cargo test --test v016_chunking --timings -- --ignored --nocapture
}

lane_NLI() {
require_present "tests that need the NLI model" "$CUBA_NLI_PATH" \
  run_counted nli 1 cargo test --test nli_entailment --test nli_cost --test nli_probe --timings -- --ignored --nocapture
}

lane_D3() {
ensure_gate_db
export DATABASE_URL="$GATE_DATABASE_URL"
require_generative_llm "tests that need a generative LLM (extract / relation-scan)" \
  run_counted generative-llm 1 cargo test --test v016_extract_without_sampling --test v017_relation_scan --timings \
             -- --ignored --nocapture
}

# The release build carries --features docs,cuda, the set that publish.yml publishes on Linux x64 (CI-K8, D-K8e, D-K8f:
# written out, so the build does not depend on what the default is, and a contract holds it equal to the published one).
# Without cuda, gpu::wants_gpu() returns false
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

# CI-F3.4 (D-F34i): a lane in a process of its own reads the sha256 B3 recorded next to the binary and holds the binary
# against it. A lane that runs in B3's process has REL_SHA already. Without the record nothing says the binary on disk is
# the one B3 built, so it is not judged.
b3_record_sha() {
  local record
  [[ -z "${REL_SHA:-}" ]] || return 0
  record="$(dirname "$REL_BIN")/.gate-release.sha256"
  [[ -f "$record" ]] \
    || { echo "FAIL: B3 left no sha256 record at $record: the binary $REL_BIN is not known to be the one it built (run --lane B3 first)." >&2; exit 1; }
  REL_SHA="$(cut -d' ' -f1 "$record")"
  [[ "$REL_SHA" =~ ^[0-9a-f]{64}$ ]] \
    || { echo "FAIL: the sha256 record of B3 at $record does not start with a sha256: $(cat "$record")" >&2; exit 1; }
  sha_unchanged "since B3"
}

lane_B3() {
echo "=== release build (one cargo call with --no-run, the feature set production runs) ==="
export CARGO_BUILD_JOBS="$GATE_RELEASE_JOBS"
cargo test --release --features docs,cuda --no-run --lib --test v017_rerank_gpu --timings
REL_BIN="$(resolve_binary_path)"
[[ -f "$REL_BIN" ]] || { echo "FAIL: the release build left no binary at $REL_BIN." >&2; exit 1; }
REL_SHA="$(release_sha "$REL_BIN")"

echo "=== B3g (every unit of the bins is fresh after the --no-run build) ==="
b3g_json="$(mktemp)"
cargo build --release --features docs,cuda --bins --message-format=json --timings >"$b3g_json" \
  || { rm -f "$b3g_json"; echo "FAIL: the release build of the bins failed (B3g)." >&2; exit 1; }
check_fresh "$b3g_json" || { rm -f "$b3g_json"; exit 1; }
rm -f "$b3g_json"
sha_unchanged "after B3g"
# CI-F3.4 (D-F34i): what R and E, each maybe in a process of its own, hold the binary against.
printf '%s  %s\n' "$REL_SHA" "$REL_BIN" >"$(dirname "$REL_BIN")/.gate-release.sha256"
}

# --features docs,cuda here too. The feature set is part of what cargo fingerprints,
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
# CI-F3.3b-1 (D-F3q): B3 may have run in another process. R reads the release binary it left on disk, as
# lane_E does, and checks it against its sha256 at the end.
REL_BIN="${REL_BIN:-$(resolve_binary_path)}"
[[ -f "$REL_BIN" ]] || { echo "FAIL: no release binary at $REL_BIN: the B3 lane builds it (run --lane B3 first)." >&2; exit 1; }
b3_record_sha
echo "=== reranker placement contracts (GPU feature live, no model needed) ==="
run_counted reranker-placement 1 cargo test --release --features docs,cuda --test v017_rerank_gpu --timings -- --nocapture

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
run_counted placement-table 0 cargo test --release --features docs,cuda --lib gpu:: --timings -- --nocapture

require_present "reranker tests (release: 387s in debug, seconds here)" \
  "$CUBA_RERANKER_PATH/model.onnx" \
  run_counted reranker-model 1 cargo test --release --features docs,cuda --test v017_rerank_gpu --timings -- --ignored --nocapture

check_section_lines

sha_unchanged "before E2E"
}

lane_E() {
export DATABASE_URL="$GATE_DATABASE_URL"
REL_BIN="${REL_BIN:-$(resolve_binary_path)}"
[[ -f "$REL_BIN" ]] || { echo "FAIL: no release binary at $REL_BIN: the B3 lane builds it (run --lane B3 first)." >&2; exit 1; }
b3_record_sha
# CI-F3.3b-1: the e2e chain runs in a process of its own, with a cluster that has no brain_gate: E makes
# it with provision_gate_db, through ensure_gate_db (CI-E11, D-E11f), and only when it is missing: D2 and D3 do the same with
# the debug binary B1 left, E with the release binary of B3, and in one process (no --lane) the first to need it makes it.
# CI-F3.4 (D-F34h): E compiles nothing. The provision runs the release binary B3 left, already held against its record.
ensure_gate_db "$REL_BIN"
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

# The gate has always built with --features cuda (docs,cuda since CI-K8; build-gpu.sh, above) and then
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
  echo "      This step reads the release build this run just made (--features docs,cuda)" >&2
  echo "      (the cuda feature). Judging any other binary would report on a build that" >&2
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

# D-F3y: without arguments the lanes run in idx order; --lane <id>... runs the ones named above, in their order.
if (( ${#run_lanes[@]} > 0 )); then
  for lane in "${run_lanes[@]}"; do
    "lane_$lane"
  done
  # CI-F3.3b-1: the counts of the sections this process ran are checked here too, since a chain does not reach lane_R's check.
  check_section_lines
  exit 0
fi
for lane in "${DAG_LANES[@]}"; do
  "lane_$lane"
done

echo ""
echo "All tests passed."
