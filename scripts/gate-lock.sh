# shellcheck shell=bash
# Sourced, never run: the "one gate at a time" lock, shared by merge-gate.sh,
# which holds it for its whole run, and run-all-tests.sh, which takes it when
# it runs on its own and inherits it when merge-gate.sh is its parent. The
# exit file, at the end, belongs to whichever of the two took the lock. The
# receipt, last, is what merge-gate.sh leaves and scripts/release.sh reads.
#
# It lived inside run-all-tests.sh until merge-gate.sh needed it too. That
# script calls run-all-tests.sh and then keeps going — clippy and tests under
# --features docs, deny, audit, codigo-muerto, crap-gate, mutants-gate — and
# all of that ran after the lock was already released. A second gate started
# in that window compiled into the same target directory and queued on the
# same cargo lock as the first, which is the collision the lock exists for.
#
# --- one gate at a time ------------------------------------------------------
# On 2026-09-22 a SIL run went red with `database "brain_gate" does not exist`
# in tests/integration.rs, minutes after "OK throwaway database brain_gate
# ready". An earlier run had lost the session that launched it; its bash lived
# on as an orphan, and when it finished, its EXIT trap dropped brain_gate out
# from under the run that had just created it again. Relaunched with no orphan
# around, the same tree passed. The code was never the cause: the gate was, and
# nothing in "does not exist" leads from the symptom to "another gate is alive".
#
# Two guards, because either can be bypassed on its own:
#   the lock   a second run refuses at once, naming the first, before it has
#              swept, dropped or built anything. It does not wait and does not
#              retry: a gate queued behind another for twenty minutes is how an
#              orphan stays invisible. It lives here.
#   ownership  every database run-all-tests.sh creates carries the lock's
#              owner record as its COMMENT, and the EXIT trap drops only
#              databases whose comment is still that record. A run that
#              bypassed the lock (another HOME, an older copy of this script)
#              can therefore not tear down what somebody else created, and
#              provisioning refuses to drop a database whose creator is still
#              running. It lives in run-all-tests.sh, next to the databases.
#
# The lock is a directory that appears whole, by rename, with its owner record
# already inside it: mkdir-then-write leaves a window in which a crash makes a
# lock with no owner that nobody can judge. flock(1) would be the usual tool,
# and Git Bash does not ship it (measured on this machine: no flock, lockfile
# or setlock; mkdir, mv -T and /proc are there).
#
# A dead run's lock is recognised, not waited out. The record holds the pid AND
# that process's start time (/proc/<pid>/stat, field 22): a pid alone would go
# on looking alive once the number is reused, and a timeout would either
# expire under a gate that is still running (they take 20 minutes and more) or
# block for its whole length after one that died. On Git Bash, /proc lists the
# MSYS processes of every session on the machine, so an orphan left by another
# terminal is seen. A record from another host or another MSYS installation
# cannot be looked up from here, and is refused with the path to remove by hand
# rather than guessed at.
# shellcheck disable=SC2034 # read by the scripts that source this one
GATE_LOCK="$HOME/.cache/cuba-gate/lock"
GATE_LOCK_HELD=""
GATE_OWNER=""
# The live corpus port (D-A11): the one place the port is spelled as code. The scripts
# that must refuse it read this name, not the number.
# shellcheck disable=SC2034 # read by the scripts that source this one
GATE_LIVE_PORT=5488

# Field 22 of /proc/<pid>/stat, read after the last ')' because field 2 is the
# command name in parentheses and may hold spaces. Prints nothing for a pid that
# is gone. Measured on Git Bash: both this and /proc/<pid>/winpid change when a
# process execs, which the gate never does.
proc_start() {
  local stat fields
  stat="$(cat "/proc/$1/stat" 2>/dev/null)" || return 0
  read -ra fields <<<"${stat##*) }"
  printf '%s\n' "${fields[19]:-}"
}

gate_where() {
  local root
  root="$(cygpath -m / 2>/dev/null || echo /)"
  printf '%s:%s\n' "$HOSTNAME" "${root// /_}"
}

owner_record() {
  printf 'pid=%s start=%s where=%s since=%s token=%s\n' "$1" "$(proc_start "$1")" \
    "$(gate_where)" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$2"
}

record_field() {
  local key="$1" word words
  read -ra words <<<"$2"
  for word in "${words[@]}"; do
    if [[ "$word" == "$key="* ]]; then
      printf '%s\n' "${word#*=}"
      return 0
    fi
  done
}

# 0 alive, 1 gone, 2 cannot be judged from this machine.
owner_alive() {
  local record="$1" pid start
  [[ "$(record_field where "$record")" == "$(gate_where)" ]] || return 2
  pid="$(record_field pid "$record")"
  start="$(record_field start "$record")"
  [[ -n "$pid" && -n "$start" && "$(proc_start "$pid")" == "$start" ]] && return 0
  return 1
}

describe_owner() {
  local pid cmd
  pid="$(record_field pid "$1")"
  cmd="$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)"
  cmd="${cmd:0:160}"
  printf 'pid %s, running since %s%s' "$pid" "$(record_field since "$1")" "${cmd:+: $cmd}"
}

acquire_gate_lock() {
  local lock="$1" staging stale holder verdict
  GATE_OWNER="$(owner_record "$$" "$$-$RANDOM$RANDOM")"
  if [[ -z "$(record_field start "$GATE_OWNER")" ]]; then
    echo "FAIL: /proc/$$/stat gives no start time, so this run cannot name itself in a" >&2
    echo "      lock that the next run could judge. One gate at a time is not enforceable" >&2
    echo "      here, and running without it is how a gate drops another gate's database." >&2
    return 1
  fi
  mkdir -p "$(dirname "$lock")"
  staging="$lock.new.$$"
  rm -rf "$staging"
  mkdir "$staging"
  printf '%s\n' "$GATE_OWNER" >"$staging/owner"
  for _ in 1 2 3; do
    if mv -T "$staging" "$lock" 2>/dev/null; then
      GATE_LOCK_HELD="$lock"
      return 0
    fi
    holder="$(cat "$lock/owner" 2>/dev/null || true)"
    verdict=1
    if [[ -n "$holder" ]]; then
      verdict=0
      owner_alive "$holder" || verdict=$?
    fi
    if (( verdict == 0 )); then
      rm -rf "$staging"
      echo "FAIL: another gate is running: $(describe_owner "$holder")" >&2
      echo "      Two gates on one machine share brain_gate, the ports and cargo's lock, and" >&2
      echo "      each one's exit drops the database the other is testing against. This run" >&2
      echo "      has swept, dropped and built nothing. Wait for that one, or stop it:" >&2
      echo "      kill $(record_field pid "$holder")    (lock: $lock)" >&2
      return 1
    fi
    if (( verdict == 2 )); then
      rm -rf "$staging"
      echo "FAIL: the gate lock was taken on $(record_field where "$holder")," >&2
      echo "      and this is $(gate_where). Its process cannot be looked up from here." >&2
      echo "      If that gate is not running, remove the lock by hand: rm -r $lock" >&2
      return 1
    fi
    stale="$lock.stale.$$"
    rm -rf "$stale"
    if mv -T "$lock" "$stale" 2>/dev/null; then
      if [[ "$(cat "$stale/owner" 2>/dev/null || true)" != "$holder" ]]; then
        # Another run reclaimed the same dead lock between the read and the
        # rename, so what moved is its live lock. Give it back and look again.
        mv -T "$stale" "$lock" 2>/dev/null || true
        continue
      fi
      rm -rf "$stale"
      echo "note: took over the lock of a gate that is no longer running (${holder:-no owner record})"
    fi
  done
  rm -rf "$staging"
  echo "FAIL: the gate lock at $lock changed hands three times while this run was" >&2
  echo "      taking it. Another gate is starting right now; run this one after it." >&2
  return 1
}

# The holder hands its record down in CUBA_GATE_LOCK_OWNER, and a run inherits
# the lock only when all three hold: the record is still the lock's owner, its
# process is alive, and that process is this run's PARENT. The last one is what
# keeps the variable from being a pass: it is exported, so every descendant of
# merge-gate.sh sees it — cargo, the tests, a script a test launches — and so
# does a shell somebody copied it into. Only the direct child the holder
# launched itself is the same gate. Inheriting does not set GATE_LOCK_HELD, so
# the child's exit leaves the lock to the parent that took it.
inherit_gate_lock() {
  local lock="$1" record="${CUBA_GATE_LOCK_OWNER:-}"
  [[ -n "$record" ]] || return 1
  [[ "$(record_field pid "$record")" == "$PPID" ]] || return 1
  [[ "$(cat "$lock/owner" 2>/dev/null || true)" == "$record" ]] || return 1
  owner_alive "$record" || return 1
  GATE_OWNER="$record"
  echo "note: running under the gate that holds the lock: $(describe_owner "$record")"
}

# A holder that finds its lock gone or renamed says so: for some stretch of
# this run a second gate could have started beside it, and whatever went red
# in that stretch may not be this run's fault.
release_gate_lock() {
  local holder
  [[ -n "$GATE_LOCK_HELD" ]] || return 0
  holder="$(cat "$GATE_LOCK_HELD/owner" 2>/dev/null || true)"
  if [[ "$holder" == "$GATE_OWNER" ]]; then
    rm -rf "$GATE_LOCK_HELD"
  else
    echo "note: the gate lock at $GATE_LOCK_HELD stopped naming this run before it ended" >&2
    echo "      (now: ${holder:-nothing}). Another gate may have run beside this one." >&2
  fi
  return 0
}

# --- the exit file -----------------------------------------------------------
# A 20-minute gate gets launched in the background, and then its result is read
# from whatever the wrapper reports — which is the exit code of the last
# command in the chain, not of the gate. That is how GATE_EXIT=101 was once
# reported as green. Reading this file is the only honest answer, and /tmp is
# swept on reboot, so it lives under ~/.cache. The gate writes it itself so
# that launching it correctly is not something the caller has to remember.
#
# It belongs to the run that TOOK the lock, and to nobody else: merge-gate.sh
# for a whole gate, run-all-tests.sh only when it runs on its own. It used to
# be written by run-all-tests.sh whatever launched it, so under merge-gate.sh
# a run whose tests passed wrote 0 and then deny, audit, codigo-muerto,
# crap-gate or mutants-gate could still fail with the file saying 0. A run
# refused by the lock never touches it: it belongs to the run still going.
#
# Two states. `running <owner record>` from the moment the lock is taken, so
# a gate killed in the middle — SIGKILL runs no trap, and twice on 2026-09-23
# memory pressure did exactly that — leaves "running" and not whatever an
# earlier run left; the record's pid and start time say whether that run is
# still alive (owner_alive above). Then the exit code, written by the EXIT
# trap BEFORE the lock is released: released first, a gate started in between
# could write its own "running" and have this run's code land on top of it.
# shellcheck disable=SC2034 # read by the scripts that source this one
GATE_EXIT_FILE="${CUBA_GATE_EXIT_FILE:-$HOME/.cache/cuba-gate/run.exit}"

exit_file_running() {
  mkdir -p "$(dirname "$GATE_EXIT_FILE")"
  printf 'running %s\n' "$GATE_OWNER" >"$GATE_EXIT_FILE"
}

exit_file_verdict() {
  mkdir -p "$(dirname "$GATE_EXIT_FILE")"
  printf '%s\n' "$1" >"$GATE_EXIT_FILE"
}

# --- the receipt -------------------------------------------------------------
# Publishing used to run this gate twice on one commit: merge-gate.sh green
# before the merge to main, then scripts/release.sh ran it again on that same
# commit to write its verdict into the tag. On this machine a run takes over an
# hour, and on 2026-09-23 two runs back to back were both killed for memory.
# So merge-gate.sh leaves the verdict itself, in ~/.cache/cuba-gate/receipts/
# <full sha>, and release.sh copies it into the tag instead of judging again.
#
# One judge of "a clean log" for both scripts, sourced from here, so that what
# lets merge-gate.sh write a receipt is exactly what lets release.sh tag.
# shellcheck disable=SC2034 # read by the scripts that source this one
RECEIPT_PREFIX="local-gate: MERGE GATE PASSED"
# shellcheck disable=SC2034
GATE_RECEIPTS="$HOME/.cache/cuba-gate/receipts"
GATE_LOG_PROBLEM=""
GATE_PASSED_LINE=""
GATE_KILL_LINE=""

# Exit 0 is necessary and not sufficient. AGENTS.md defines mergeable as exit
# 0 with a clean log. The banner prints "missing = FAIL, never SKIPPED" on every
# run, so that phrase is the one SKIPPED that does not count; any other is a
# hole the exit code hid. Returns 1 with the reason in GATE_LOG_PROBLEM, or 0
# with the two lines a receipt carries in GATE_PASSED_LINE and GATE_KILL_LINE.
gate_log_verdict() {
  local log="$1" clean skipped failed
  GATE_LOG_PROBLEM=""
  clean="$(tr -d '\r' <"$log")"
  skipped="$(grep -n 'SKIPPED' <<<"$clean" | grep -v 'never SKIPPED' || true)"
  if [[ -n "$skipped" ]]; then
    GATE_LOG_PROBLEM="merge-gate.sh exited 0 with SKIPPED in its log, which is not a pass:
$skipped"
    return 1
  fi
  failed="$(grep -n 'test result: FAILED' <<<"$clean" || true)"
  if [[ -n "$failed" ]]; then
    GATE_LOG_PROBLEM="merge-gate.sh exited 0 with a failed test run in its log:
$failed"
    return 1
  fi
  GATE_PASSED_LINE="$(grep -m1 'MERGE GATE PASSED' <<<"$clean" | sed 's/║//g; s/^[[:space:]]*//; s/[[:space:]]*$//' || true)"
  if [[ -z "$GATE_PASSED_LINE" ]]; then
    GATE_LOG_PROBLEM="merge-gate.sh exited 0 without printing MERGE GATE PASSED. Log: $log"
    return 1
  fi
  GATE_KILL_LINE="$(grep -m1 'kill_rate=' <<<"$clean" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//' || true)"
  if [[ -z "$GATE_KILL_LINE" ]]; then
    GATE_LOG_PROBLEM="merge-gate.sh exited 0 and mutants-gate.sh never reported a kill_rate. Log: $log"
    return 1
  fi
}

# The receipt: the sha, the clock and two lines the gate printed, which is also
# every line of the tag message under its title. Nothing from the environment
# goes in: no path, no URL, no user.
gate_receipt_lines() {
  printf '%s %s\n' "$RECEIPT_PREFIX" "$1"
  printf 'local-gate-date: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf 'local-gate-summary: %s\n' "$GATE_KILL_LINE"
  printf 'local-gate-summary: %s\n' "$GATE_PASSED_LINE"
}

# --- the Linux environment of a gate run -------------------------------------
# Written for Git Bash on Windows, this gate now runs on Linux (WSL2, ext4), and
# three things that were true there are not true here:
#
#   the cargo target   `cargo mutants` copies the tree it mutates into a scratch
#                      directory. With CARGO_TARGET_DIR inside rust/ (rust/target-sil)
#                      it copied 27.5 GB, and the baseline of the mutants failed
#                      from binaries of two builds mixed. So the target goes outside
#                      the tree, in an absolute path.
#   TMPDIR             /tmp is tmpfs here, and tmpfs counts as RAM against the
#                      memory cap the gate runs under (`pesado -m`). The scratch
#                      copy of every mutant would be RAM. TMPDIR goes to a directory
#                      on the disk.
#   node               `node npm/install.test.js` is a step of merge-gate.sh and the
#                      one on PATH comes from nvm: whichever the shell last selected.
#                      package.json says the oldest one the package supports.
#
# Whoever set CARGO_TARGET_DIR, or a TMPDIR that is not tmpfs, keeps it. A node that
# is missing or too old is refused with exit 2 and the way to fix it; going on
# would fail forty minutes later in a step that does not say why. Off Linux this
# does nothing: the Windows branches of the scripts are still there for that.
GATE_REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

gate_refuse() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 2
}

# run-all-tests.sh derives brain_gate, brain_gate_peer and postgres from
# DATABASE_URL by replacing what follows its last slash (${url%/*}), which is only
# right for postgresql://user:pass@host:port/<database> and nothing after it. A
# `?host=` socket would send those names to another server, a `dbname=` in the
# query would make the live corpus the throwaway database, and a URL with no
# database would turn the host into one. So merge-gate.sh and a run-all-tests.sh
# on its own refuse any other shape, with exit 2, before a single step runs. A second
# '@' before the host is refused too (rule 4): libpq and sqlx end the user and the
# password at different '@'. The reason never carries the URL or a piece of it: the
# password can be in it.
gate_require_database_url() {
  local url="${1:-}" rest database
  case "$url" in
    postgresql://*|postgres://*) ;;
    *) gate_refuse "DATABASE_URL has to be a postgresql:// (or postgres://) URL: the gate reads it with libpq and derives brain_gate from it, which a key=value string or another scheme does not allow." ;;
  esac
  if [[ "$url" == *\?* ]]; then
    gate_refuse "DATABASE_URL carries a query (a '?'), and brain_gate is derived by replacing what follows its last '/', so the query would point the throwaway databases at another server or at the live one. Write a socket as the encoded host (postgresql://cuba@%2Frun%2Fpostgresql/brain) and the options in the environment (PGSSLMODE, ...), which libpq and sqlx both read."
  fi
  rest="${url#*://}"
  database="${rest#*/}"
  if [[ "$rest" != */* || -z "$database" || "$database" == */* ]]; then
    gate_refuse "DATABASE_URL has to end in exactly one non-empty segment after the host, the database (postgresql://user:password@host:port/<database>). A '/', '?' or '@' inside the user or the password has to be encoded (%2F, %3F, %40)."
  fi
  # Rule 4: at most one '@' in the authority. libpq ends the user and the password
  # at the FIRST '@' and takes the rest as the host, so with a second '@' it prints
  # the tail of the password in the readiness line. sqlx (WHATWG) ends them at the
  # LAST '@', so the check and the binary would also ask different servers. An '@'
  # in the database name is after the first '/', outside the authority, and is fine.
  local authority="${rest%%/*}" ats
  ats="${authority//[^@]/}"
  if (( ${#ats} > 1 )); then
    gate_refuse "DATABASE_URL has more than one '@' before the host: libpq ends the user and the password at the first one and sqlx at the last, so the readiness check and the binary would ask different servers, and libpq would print the rest of the password as the host. Write an '@' inside the user or the password as %40."
  fi
}

gate_is_ram_backed() {
  case "$(stat -f -c %T "$1" 2>/dev/null || true)" in
    tmpfs|ramfs) return 0 ;;
    *) return 1 ;;
  esac
}

gate_target_dir_outside_the_tree() {
  [[ -n "${CARGO_TARGET_DIR:-}" ]] && return 0
  export CARGO_TARGET_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/cargo-target/memory-industry"
}

gate_tmpdir_on_disk() {
  if [[ -n "${TMPDIR:-}" ]] && ! gate_is_ram_backed "$TMPDIR"; then
    return 0
  fi
  export TMPDIR="${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry-gate/tmp"
  mkdir -p "$TMPDIR"
}

# The major of engines.node: ">=18" is 18. Empty when package.json does not say.
gate_node_required() {
  sed -n 's/.*"node"[[:space:]]*:[[:space:]]*">=[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
    "$GATE_REPO_ROOT/package.json" 2>/dev/null | head -n 1 || true
}

gate_require_node() {
  local need version major
  need="$(gate_node_required)"
  [[ "$need" =~ ^[0-9]+$ ]] \
    || gate_refuse "cannot read engines.node (\">=N\") from $GATE_REPO_ROOT/package.json, so the oldest node the gate accepts is unknown."
  command -v node >/dev/null 2>&1 \
    || gate_refuse "node is not on PATH, and merge-gate.sh runs node npm/install.test.js. Put one >= $need first on PATH (nvm use $need, or export PATH=<dir of node>:\$PATH)."
  version="$(node --version 2>/dev/null || true)"
  major="${version#v}"
  major="${major%%.*}"
  [[ "$major" =~ ^[0-9]+$ ]] \
    || gate_refuse "node --version printed '$version', which is not a version, so this run cannot say which node it uses (need >= $need): nvm use $need."
  (( major >= need )) \
    || gate_refuse "node $version is older than engines.node >=$need in package.json. nvm use $need, or put a newer node first on PATH."
}

gate_linux_env() {
  [[ "$(uname -s)" == Linux ]] || return 0
  gate_target_dir_outside_the_tree
  gate_tmpdir_on_disk
  gate_require_node
}

# --- the models of a gate run (CI-J5) ----------------------------------------
gate_model_env() {
  local CACHE_NEW CACHE_OLD CACHE
  # Under HOME/.cache and nowhere else, because that is where the binary keeps
  # them: envs::home() joins `.cache` to HOME, and nothing in the crate reads
  # XDG_CACHE_HOME. This used to honour it, as gpu-placement-check.sh did until
  # it was fixed for the same reason, and on a machine that sets it the gate
  # pointed ONNX_MODEL_PATH at a directory `models all` never wrote.
  CACHE_NEW="$HOME/.cache/memory-industry"
  CACHE_OLD="$HOME/.cache/cuba-memorys"
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
}

# --- the judge's server (CI-J, D-J1a) ----------------------------------------
# The judge runs only against a Postgres that says it is the judge's: one started
# with -c cluster_name=memory-industry-gate (or memory-industry-gate-<lane>). The
# mark decides, not the port and not the container's name: the live corpus has no
# mark, so any server without one is refused whatever it is called. The identity
# is read read-only from the postgres database of that same server, so the answer
# is about the server DATABASE_URL names and about nothing else. Sets
# GATE_JUDGE_SYSID to the system_identifier it read.
GATE_JUDGE_SYSID=""

gate_url_port() {
  local rest="${1#*://}" authority hostport
  authority="${rest%%/*}"
  hostport="${authority##*@}"
  if [[ "$hostport" == \[*\] || "$hostport" != *:* ]]; then
    printf '%s' "${PGPORT:-5432}"
  else
    printf '%s' "${hostport##*:}"
  fi
}

# No default server (D-J1e): an empty URL is refused before anybody is asked, and then
# its shape, as gate_require_database_url judges it. merge-gate.sh asks this before
# pg_isready, and gate_require_judge_server asks it again: one refusal, one text.
gate_require_given_database_url() {
  local url="${1:-}"
  [[ -n "$url" ]] \
    || gate_refuse "DATABASE_URL is not set, or is empty. The judge has no default server: it runs only against its own Postgres, started with -c cluster_name=memory-industry-gate, as docs/gate.md says. Export DATABASE_URL with that server."
  gate_require_database_url "$url"
}

gate_require_judge_server() {
  local url="${1:-}" port identity cn sysid
  local identity_re='^[^|]*[|][0-9]+$' mark_re='^memory-industry-gate(-[A-Za-z0-9]+)?$'
  gate_require_given_database_url "$url"
  command -v psql >/dev/null 2>&1 \
    || gate_refuse "psql is not on PATH, so the judge cannot read which server DATABASE_URL names (cluster_name and system_identifier). Install the PostgreSQL client tools."
  port="$(gate_url_port "$url")"
  # The password goes to libpq in PGPASSWORD, for this one call, and never in the argv of psql (A F2b):
  # ps shows the argv to every user. The maintenance database is asked without it, and the value is
  # decoded here, since libpq reads PGPASSWORD raw and the URL carries it percent-encoded.
  local base head hostpart user dsn userinfo pw
  base="${url%/*}/postgres"
  head="${base%%:*}"
  hostpart="${base#*@}"
  userinfo="${base#*://}"
  userinfo="${userinfo%%@*}"
  user="${userinfo%%:*}"
  dsn="${head}://${user}@${hostpart}"
  pw=""
  if [[ "$userinfo" == *:* ]]; then
    pw="${userinfo#*:}"
    pw="$(printf '%b' "${pw//%/\\x}")"
  fi
  identity="$(PGPASSWORD="$pw" PGOPTIONS='-c default_transaction_read_only=on' psql -X -At -F '|' -d "$dsn" \
    -c "select current_setting('cluster_name'), system_identifier from pg_control_system()" 2>/dev/null)" || identity=""
  identity="${identity%$'\r'}"
  [[ "$identity" =~ $identity_re ]] \
    || gate_refuse "could not read the identity of the server on :$port (cluster_name and system_identifier from pg_control_system(), read-only, through its postgres database). Nothing is judged; check that the server answers and that its role can read the identity."
  cn="${identity%%|*}"
  sysid="${identity##*|}"
  [[ "$cn" =~ $mark_re ]] \
    || gate_refuse "the server on :$port is not the judge's: its cluster_name is '$cn' (system_identifier $sysid), and the judge runs only against a server marked cluster_name=memory-industry-gate. Never the live corpus. docs/gate.md says how to start the judge's server."
  # merge-gate.sh exports the system_identifier it read (D-J1c): a server that changed under
  # the run is refused here, at every step that asks, and not only in run-all-tests.sh.
  if [[ -n "${GATE_SERVER_SYSTEM_ID:-}" && "$GATE_SERVER_SYSTEM_ID" != "$sysid" ]]; then
    gate_refuse "the server changed under this run: merge-gate.sh read system_identifier $GATE_SERVER_SYSTEM_ID and this run reads $sysid on the same DATABASE_URL. Nothing is judged; find out what answers on that port."
  fi
  # D-F2i: a lane (GATE_LANE set) is judged by its own name and by its own socket under this root. Its
  # cluster_name is memory-industry-gate-<lane> and nothing else, and its URL is never TCP.
  if [[ -n "${GATE_LANE:-}" ]]; then
    [[ "$cn" == "memory-industry-gate-$GATE_LANE" ]] \
      || gate_refuse "lane $GATE_LANE is served by '$cn' (system_identifier $sysid), not by memory-industry-gate-$GATE_LANE: lanes do not cross (D-F2i)."
    [[ -n "${GATE_CLUSTER_ROOT:-}" && "${url#*@}" == "${GATE_CLUSTER_ROOT//\//%2F}%2F"* ]] \
      || gate_refuse "lane $GATE_LANE has a URL that is not a socket under ${GATE_CLUSTER_ROOT:-no root} (D-F2i): a lane is never TCP."
    GATE_JUDGE_SYSID="$sysid"
    local sockdir="${url#*@}"
    sockdir="${sockdir%:*}"
    sockdir="${sockdir%/*}"
    echo "OK  judge server $GATE_LANE socket $(printf '%b' "${sockdir//%/\\x}") (cluster_name=$cn, system_identifier=$sysid)"
    return 0
  fi
  GATE_JUDGE_SYSID="$sysid"
  echo "OK  judge server :$port (cluster_name=$cn, system_identifier=$sysid)"
}

# --- the cgroup the gate runs in (CI-J, D-J5a) -------------------------------
# NAME VALUE: VALUE goes into the variable NAME when NAME is empty or holds more.
gate_cg_keep_tighter() {
  local name="$1" v="$2"
  if [[ -z "${!name}" ]] || (( v < ${!name} )); then printf -v "$name" '%s' "$v"; fi
}

# D-J5c: the page cache a cgroup holds is memory it gives back under pressure, so its
# inactive part is not in use. Prints memory.stat's inactive_file of DIR as written
# (the last such line, unchecked), and 0 when the file is absent or has no such line.
gate_cgroup_inactive_file() {
  local dir="$1" inactive=0 key val
  if [[ -r "$dir/memory.stat" ]]; then
    while read -r key val; do
      if [[ "$key" == inactive_file ]]; then inactive="$val"; fi
    done <"$dir/memory.stat"
  fi
  printf '%s' "$inactive"
}

# The memory one cgroup DIR leaves: memory.max - (memory.current - inactive_file), in KB,
# into GATE_CG_MEM_KB when it is the tightest yet. No memory.max, or `max`, is no limit.
gate_cgroup_memory_limit() {
  local dir="$1" v cur inactive
  [[ -r "$dir/memory.max" ]] || return 0
  v="$(<"$dir/memory.max")"
  [[ "$v" != max ]] || return 0
  [[ "$v" =~ ^(0|[1-9][0-9]*)$ ]] \
    || gate_refuse "$dir/memory.max holds '$v', which is neither a number of bytes nor max, so the memory the cgroup leaves cannot be read.${GATE_CG_HINT:+ $GATE_CG_HINT}"
  [[ -r "$dir/memory.current" ]] \
    || gate_refuse "$dir/memory.max is a limit and $dir/memory.current is not readable, so the memory the cgroup leaves cannot be read.${GATE_CG_HINT:+ $GATE_CG_HINT}"
  cur="$(<"$dir/memory.current")"
  [[ "$cur" =~ ^(0|[1-9][0-9]*)$ ]] \
    || gate_refuse "$dir/memory.current holds '$cur', which is not a number of bytes, so the memory the cgroup leaves cannot be read.${GATE_CG_HINT:+ $GATE_CG_HINT}"
  inactive="$(gate_cgroup_inactive_file "$dir")"
  [[ "$inactive" =~ ^(0|[1-9][0-9]*)$ ]] \
    || gate_refuse "$dir/memory.stat holds '$inactive' as inactive_file, which is not a number of bytes.${GATE_CG_HINT:+ $GATE_CG_HINT}"
  cur=$(( cur - inactive ))
  (( cur >= 0 )) || cur=0
  v=$(( (v - cur) / 1024 ))
  (( v >= 0 )) || v=0
  gate_cg_keep_tighter GATE_CG_MEM_KB "$v"
}

# The CPUs one cgroup DIR leaves: ceil(quota/period) of cpu.max, into GATE_CG_CPUS when it
# is the tightest yet. No cpu.max, or a quota of `max`, is no limit.
gate_cgroup_cpu_limit() {
  local dir="$1" q p
  [[ -r "$dir/cpu.max" ]] || return 0
  read -r q p <"$dir/cpu.max" || true
  [[ "$q" != max ]] || return 0
  [[ "$q" =~ ^(0|[1-9][0-9]*)$ && "$p" =~ ^(0|[1-9][0-9]*)$ ]] && (( p > 0 )) \
    || gate_refuse "$dir/cpu.max holds '$q ${p:-}', which is not '<quota> <period>' in numbers, so the CPUs the cgroup leaves cannot be read.${GATE_CG_HINT:+ $GATE_CG_HINT}"
  gate_cg_keep_tighter GATE_CG_CPUS "$(( (q + p - 1) / p ))"
}

# The path of the cgroup v2 line (`0::<path>`) of SELF, the last one if there are several,
# with no trailing '/'. Status 1 when SELF cannot be read or has no such line.
gate_cgroup_v2_path() {
  local self="$1" line rel="" found=""
  [[ -r "$self" ]] || return 1
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" == 0::* ]]; then
      rel="${line#0::}"
      found=1
    fi
  done <"$self"
  [[ -n "$found" ]] || return 1
  printf '%s' "${rel%/}"
}

# pesado runs the gate in a scope inside a slice, and the limit that holds is the
# tightest of the chain: the scope's, each ancestor's up to the root's. Walks
# <root>/<path> and its ancestors, from /proc/self/cgroup's `0::` line. Sets
# GATE_CG_MEM_KB (the smallest memory.max - memory.current, in KB; empty when no
# file in the chain says a number), GATE_CG_CPUS (the smallest ceil(quota/period)
# of cpu.max, in cores; empty when none) and GATE_CG_NOTE (set when there is no
# cgroup v2 line: a cgroup v1 shell, Git Bash). A value that is neither a number
# nor `max` is the refusal, not a limit that goes unread. Must be called directly,
# not in a subshell: the refusal is an exit of this shell.
gate_cgroup_read() {
  local self="${GATE_PROC_SELF_CGROUP:-/proc/self/cgroup}" root="${GATE_CGROUP_ROOT:-/sys/fs/cgroup}"
  local dir
  GATE_CG_MEM_KB=""
  GATE_CG_CPUS=""
  GATE_CG_NOTE=""
  if ! dir="$(gate_cgroup_v2_path "$self")"; then
    GATE_CG_NOTE="no cgroup v2 line in $self, so no limit is read from a cgroup"
    return 0
  fi
  while :; do
    gate_cgroup_memory_limit "$root$dir"
    gate_cgroup_cpu_limit "$root$dir"
    [[ -n "$dir" ]] || break
    if [[ "$dir" == */* ]]; then dir="${dir%/*}"; else dir=""; fi
  done
}

# --- the cargo jobs of a gate run (CI-F1, D-F1a/b, F-c) ----------------------
# N is the number of CPUs the gate may hand cargo: the cgroup's (gate_cgroup_read) under
# nproc's, so a judge in a scope of 8 CPUs on a 24-core machine asks for 8 jobs, not 24.
# The release build takes min(N, floor(margin GB / 4)), at least 1, with the margin the
# cgroup leaves (MemAvailable where it sets no memory limit): the 4 GB per job build-gpu.sh
# used to cap with. The release margin is the cgroup's and not MemAvailable's because
# pesado's MemoryMax is what kills the build, and that is the limit that holds here.
# A CARGO_BUILD_JOBS the operator set is kept and said; one this gate exported (the
# GATE_CARGO_JOBS_FROM_GATE marker, which the children inherit) is not named as the operator's.
# Sets GATE_CARGO_JOBS and GATE_RELEASE_JOBS. Must be called directly, not in a subshell:
# the refusals are exits of this shell.
gate_cargo_jobs() {
  local cores cg margin_kb margin_gb
  GATE_CG_HINT="Set CARGO_BUILD_JOBS=<n> by hand."
  gate_cgroup_read
  cores="$(nproc)"
  [[ "$cores" =~ ^[0-9]+$ ]] \
    || gate_refuse "nproc printed '$cores', which is not a number of CPUs, so the cargo jobs cannot be counted. Set CARGO_BUILD_JOBS=<n> by hand."
  cg="$GATE_CG_CPUS"
  GATE_CARGO_JOBS="$cores"
  if [[ -n "$cg" ]] && (( cg < GATE_CARGO_JOBS )); then GATE_CARGO_JOBS="$cg"; fi
  if [[ -n "${CARGO_BUILD_JOBS:-}" && "$CARGO_BUILD_JOBS" != "${GATE_CARGO_JOBS_FROM_GATE:-}" ]]; then
    echo "cargo jobs=$CARGO_BUILD_JOBS (CARGO_BUILD_JOBS set by the operator; nproc=$cores, cgroup cpu=${cg:-none})"
  elif [[ -z "${CARGO_BUILD_JOBS:-}" ]]; then
    export CARGO_BUILD_JOBS="$GATE_CARGO_JOBS" GATE_CARGO_JOBS_FROM_GATE="$GATE_CARGO_JOBS"
    echo "cargo jobs=$GATE_CARGO_JOBS (nproc=$cores, cgroup cpu=${cg:-none})"
  fi
  GATE_CARGO_JOBS="$CARGO_BUILD_JOBS"
  margin_kb="${GATE_CG_MEM_KB:-}"
  if [[ -z "$margin_kb" ]]; then
    margin_kb="$(awk '$1 == "MemAvailable:" { print $2; exit }' /proc/meminfo 2>/dev/null || true)"
  fi
  GATE_RELEASE_JOBS="$GATE_CARGO_JOBS"
  if [[ "$margin_kb" =~ ^[0-9]+$ ]]; then
    margin_gb=$(( margin_kb / 1048576 ))
    if (( margin_gb / 4 < GATE_RELEASE_JOBS )); then GATE_RELEASE_JOBS=$(( margin_gb / 4 )); fi
  fi
  (( GATE_RELEASE_JOBS >= 1 )) || GATE_RELEASE_JOBS=1
}
