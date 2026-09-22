#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT/rust"
LIVE_DATABASE_URL="${DATABASE_URL:-postgresql://cuba:memorys2026@127.0.0.1:5488/brain}"
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

# One gate at a time: the lock, its owner record and the rules for a dead or
# foreign holder are in gate-lock.sh, which merge-gate.sh sources too so that
# it can hold the lock for its whole run. What stays here is the half that is
# about databases: every one this run creates carries GATE_OWNER as its
# COMMENT, and the exit drops only those still carrying it.
# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"
GATE_OWNED_DBS=()

# The exit code is written here before anything else can overwrite $?. A 20-minute
# gate gets launched in the background, and then its result is read from whatever
# the wrapper reports — which is the exit code of the last command in the chain,
# not of the gate. That is how GATE_EXIT=101 was once reported as green. Reading
# this file is the only honest answer, and /tmp is swept on reboot, so it lives
# under ~/.cache. Written by the gate itself so that launching it correctly is not
# something the caller has to remember. A run refused by the lock never touches
# it: the file belongs to the run that is still going.
EXIT_FILE="${CUBA_GATE_EXIT_FILE:-$HOME/.cache/cuba-gate/run.exit}"

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

on_exit() {
  local code=$?
  release_owned_databases
  release_gate_lock
  mkdir -p "$(dirname "$EXIT_FILE")"
  echo "$code" > "$EXIT_FILE"
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
if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d)"
  sleep 300 &
  live=$!
  trap 'kill "$live" 2>/dev/null || true; rm -rf "$tmp"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }
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
  env -u CUBA_GATE_EXIT_FILE HOME="$tmp/home" PATH="/usr/bin:/bin" \
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
  env -u CUBA_GATE_LOCK_OWNER HOME="$tmp/home" PATH="/usr/bin:/bin" \
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
  # is still the one merge-gate.sh took. It then fails, which ends both.
  mkdir -p "$tmp/home2" "$tmp/bin"
  printf '#!/bin/sh\nexit 0\n' >"$tmp/bin/pg_isready"
  printf '#!/bin/sh\nexit 1\n' >"$tmp/bin/psql"
  printf '%s\n' '#!/bin/sh' \
    'if [ -n "$CUBA_GATE_LOCK_OWNER" ] && [ "$(cat "$HOME/.cache/cuba-gate/lock/owner" 2>/dev/null)" = "$CUBA_GATE_LOCK_OWNER" ]; then' \
    '  echo "stand-in cargo: the lock is still the one merge-gate took"' \
    'fi' \
    'exit 1' >"$tmp/bin/cargo"
  chmod +x "$tmp/bin/pg_isready" "$tmp/bin/psql" "$tmp/bin/cargo"
  held_exit=0
  env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE HOME="$tmp/home2" \
      PATH="$tmp/bin:/usr/bin:/bin" SKIP_BACKUP=1 \
      CUBA_GATE_SWEEP_BELOW_GB=0 CUBA_GATE_MIN_FREE_GB=0 \
      "$BASH" "$ROOT/scripts/merge-gate.sh" >"$tmp/merge-held.out" 2>&1 || held_exit=$?
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

  echo "OK  self-test: a second gate refuses at once and names the first, merge-gate"
  echo "    included, and the variable its children inherit is no pass for anyone"
  echo "    else; the run-all-tests.sh merge-gate launches works under its lock; a"
  echo "    dead or reused-pid lock is taken over, a foreign one is not; an exit"
  echo "    drops only the databases whose record is still its own"
  exit 0
fi

inherit_gate_lock "$GATE_LOCK" || acquire_gate_lock "$GATE_LOCK" || exit 1
rm -f "$EXIT_FILE"
trap on_exit EXIT

CACHE_NEW="${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry"
CACHE_OLD="${XDG_CACHE_HOME:-$HOME/.cache}/cuba-memorys"
# Prefer the cache that actually has the embedder. An empty memory-industry/
# dir (e.g. only audit_key) must not steal paths from a populated cuba-memorys/.
if [[ -f "$CACHE_NEW/models/model_quantized.onnx" ]]; then
  CACHE="$CACHE_NEW"
elif [[ -f "$CACHE_OLD/models/model_quantized.onnx" ]]; then
  CACHE="$CACHE_OLD"
elif [[ -d "$CACHE_NEW" ]]; then
  CACHE="$CACHE_NEW"
elif [[ -d "$CACHE_OLD" ]]; then
  CACHE="$CACHE_OLD"
else
  CACHE="$CACHE_NEW"
fi
# Paths must match `memory-industry models all` (see rust/src/models_cli.rs).
export ONNX_MODEL_PATH="${ONNX_MODEL_PATH:-$CACHE/models}"
if [[ -z "${ORT_DYLIB_PATH:-}" ]]; then
  if [[ -f "$CACHE/onnxruntime/libonnxruntime.so" ]]; then
    export ORT_DYLIB_PATH="$CACHE/onnxruntime/libonnxruntime.so"
  elif [[ -f "$CACHE/onnxruntime/onnxruntime.dll" ]]; then
    export ORT_DYLIB_PATH="$CACHE/onnxruntime/onnxruntime.dll"
  elif [[ -f "$CACHE_OLD/onnxruntime/onnxruntime.dll" ]]; then
    export ORT_DYLIB_PATH="$CACHE_OLD/onnxruntime/onnxruntime.dll"
  elif [[ -f "$CACHE_OLD/onnxruntime/libonnxruntime.so" ]]; then
    export ORT_DYLIB_PATH="$CACHE_OLD/onnxruntime/libonnxruntime.so"
  else
    export ORT_DYLIB_PATH="$CACHE/onnxruntime/libonnxruntime.so"
  fi
fi
export CUBA_RERANKER_PATH="${CUBA_RERANKER_PATH:-$CACHE/reranker}"
export CUBA_NLI_PATH="${CUBA_NLI_PATH:-$CACHE/models-nli}"
export CUBA_EMBEDDING_DIM="${CUBA_EMBEDDING_DIM:-384}"
export CUBA_EMBED_MODEL="${CUBA_EMBED_MODEL:-e5-small}"
export CUBA_POOLING="${CUBA_POOLING:-cls}"

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

free_gb() { df --output=avail -BG "$RUST_DIR" | tail -1 | tr -dc '0-9'; }

sweep_stale_artifacts() {
  local before after
  before="$(free_gb)"
  echo "disk: ${before}G free — sweeping build artifacts older than ${SWEEP_OLDER_THAN_DAYS}d"
  rm -rf "$RUST_DIR/target/debug/incremental" 2>/dev/null || true
  find "$RUST_DIR/target/debug/deps" -maxdepth 1 -type f \
       -mtime "+$SWEEP_OLDER_THAN_DAYS" -delete 2>/dev/null || true
  find "$RUST_DIR/target/debug/.fingerprint" -maxdepth 1 -type d \
       -mtime "+$SWEEP_OLDER_THAN_DAYS" -exec rm -rf {} + 2>/dev/null || true
  after="$(free_gb)"
  echo "disk: ${after}G free after the sweep (was ${before}G)"
}

if [[ "$(free_gb)" -lt "$SWEEP_BELOW_GB" ]]; then
  sweep_stale_artifacts
fi

if [[ "$(free_gb)" -lt "$MIN_FREE_GB" ]]; then
  echo "FAIL: only $(free_gb)G free on the filesystem holding $RUST_DIR, and this run needs more." >&2
  echo '      Linking is what breaks first, and it breaks as SIGBUS inside ld with no' >&2
  echo "      mention of disk — that is an hour of looking for a bug that is not there." >&2
  echo "      target/ currently holds: $(du -sh "$RUST_DIR/target" 2>/dev/null | cut -f1)" >&2
  echo "      Free it with: cargo clean --manifest-path $RUST_DIR/Cargo.toml" >&2
  echo "      (or raise the bar with CUBA_GATE_MIN_FREE_GB if you know what you are doing)" >&2
  exit 1
fi

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
GATE_DATABASE_URL="${LIVE_DATABASE_URL%/*}/$GATE_DB"
PEER_DB="${PEER_DB:-brain_gate_peer}"
PEER_DATABASE_URL="${LIVE_DATABASE_URL%/*}/$PEER_DB"
ADMIN_DATABASE_URL="${LIVE_DATABASE_URL%/*}/postgres"
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

# Prefer an already-built binary. `cargo run` under timeout used to spend the
# whole budget recompiling and leave the throwaway DB with zero tables while
# `|| true` hid the failure (exit 124).
#
# gate_target_dir is defined at the top of this file, with the rest of the path
# resolution. On Windows the file is memory-industry.exe; `[[ -x
# memory-industry ]]` is false in Git Bash.
gate_bin() {
  local td cand
  td="$(gate_target_dir)"
  for cand in \
    "$td/debug/memory-industry.exe" \
    "$td/debug/memory-industry" \
    "$td/release/memory-industry.exe" \
    "$td/release/memory-industry" \
    "$td/debug/cuba-memorys.exe" \
    "$td/debug/cuba-memorys" \
    "$td/release/cuba-memorys.exe" \
    "$td/release/cuba-memorys"
  do
    if [[ -x "$cand" ]]; then
      printf '%s\n' "$cand"
      return 0
    fi
  done
  cargo build --quiet --bin memory-industry >&2
  td="$(gate_target_dir)"
  for cand in "$td/debug/memory-industry.exe" "$td/debug/memory-industry"; do
    if [[ -x "$cand" ]]; then
      printf '%s\n' "$cand"
      return 0
    fi
  done
  echo "FAIL: cargo build --bin memory-industry produced no binary under $td/debug." >&2
  exit 1
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

echo "=== cargo fmt --check ==="
cargo fmt --check

echo "=== cargo clippy (--all-targets: without it, tests/ is never linted) ==="
cargo clippy --all-targets -- -D warnings

echo "=== throwaway database for every mutating step ==="
provision_gate_db

echo "=== cargo test (unit + smoke) ==="
DATABASE_URL="$GATE_DATABASE_URL" cargo test

echo "=== DB integration tests (--ignored) ==="
export DATABASE_URL="$GATE_DATABASE_URL"
# Every tests/*.rs is discovered, not listed. The list used to be written by hand
# and had drifted to 30 of 52 files: fifteen integration tests written for the
# sync and panel work had never once run in the gate, while the gate reported
# green over the very commits that added them. A gate whose coverage depends on
# somebody remembering to edit it is a gate that quietly shrinks.
RUN_ELSEWHERE=(v020_audit_update_applies v020_role_separation v020_audit_hmac
               v020_embed_cache_key v016_chunking nli_entailment nli_cost nli_probe
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
cargo test "${DISCOVERED[@]}" -- --ignored --nocapture

cargo test --lib -- --ignored --nocapture

echo "=== admin-role tests (they rewrite roles and the audit log) ==="
CUBA_APP_ROLE=0 cargo test --test v020_audit_update_applies --test v020_role_separation \
           -- --ignored --nocapture

echo "=== audit HMAC + embed cache key ==="
cargo test --test v020_audit_hmac --test v020_embed_cache_key -- --ignored --nocapture

require_present "tests that need the embedding model" "$ONNX_MODEL_PATH/model_quantized.onnx" \
  cargo test --test v016_chunking -- --ignored --nocapture

require_present "tests that need the NLI model" "$CUBA_NLI_PATH" \
  cargo test --test nli_entailment --test nli_cost --test nli_probe -- --ignored --nocapture

require_generative_llm "tests that need a generative LLM (extract / relation-scan)" \
  cargo test --test v016_extract_without_sampling --test v017_relation_scan \
             -- --ignored --nocapture

# build-gpu.sh, not a bare `cargo build --release`. Without --features cuda,
# gpu::wants_gpu() returns false unconditionally, CUBA_RERANK_DEVICE=gpu goes
# inert, and the reranker runs on CPU at 20.669s per query against 0.356s — 58x,
# measured in d7922fa. The E2E allows 15s per call, so every one of its 40 calls
# times out and the gate can never pass on a machine that has a GPU configured.
# It also overwrote the developer's GPU binary at this exact path.
echo "=== release build (same feature set production runs) ==="
"$ROOT/scripts/build-gpu.sh"

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
# them run under it. Same feature set as build-gpu.sh, so nothing rebuilds.
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
echo "=== reranker placement contracts (GPU feature live, no model needed) ==="
cargo test --release --features cuda --test v017_rerank_gpu -- --nocapture

# The library half of the same hole. The cfg! that actually decides placement is
# in gpu::wants_gpu, which returns false before reading the device variable when
# no GPU feature is compiled in, and the table that judges it is
# gpu::placement_tests — five tests that both `cargo test` calls above compile
# without the feature, so the device knob is inert there and half of that table
# is false == false for the same reason v017_rerank_gpu was.
#
# Release and not debug, for cost: build-gpu.sh above already built this
# dependency graph with cuda, so the only thing compiled here is the lib test
# unit itself. In debug it would be a third feature set (debug bare, release
# cuda, debug cuda) and a full rebuild of everything under it.
echo "=== placement table under the feature that changes it ==="
cargo test --release --features cuda --lib gpu:: -- --nocapture

require_present "reranker tests (release: 387s in debug, seconds here)" \
  "$CUBA_RERANKER_PATH/model.onnx" \
  cargo test --release --features cuda --test v017_rerank_gpu -- --ignored --nocapture

echo "=== E2E (25 MCP tools, subprocess per call) ==="
CUBA_BINARY_PATH="$(resolve_binary_path)"
export CUBA_BINARY_PATH
echo "E2E binary: $CUBA_BINARY_PATH"
# Prefer PYTHON_BIN (set by run-gate on Windows). Never use the Microsoft Store
# python3.exe stub under WindowsApps — it prints install text and exits 49.
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
  echo "      This step reads the release build scripts/build-gpu.sh just produced" >&2
  echo "      (--features cuda). Judging any other binary would report on a build that" >&2
  echo "      never had GPU support compiled in and call the answer honest." >&2
  exit 1
fi
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

echo "=== eval harness smoke (read-only, so it runs against the real corpus) ==="
DATABASE_URL="$LIVE_DATABASE_URL" \
  "$CUBA_BINARY_PATH" eval \
  --dataset "$RUST_DIR/eval-datasets/smoke.jsonl" --k 10

echo ""
echo "All tests passed."
