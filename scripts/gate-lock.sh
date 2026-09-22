# shellcheck shell=bash
# Sourced, never run: the "one gate at a time" lock, shared by merge-gate.sh,
# which holds it for its whole run, and run-all-tests.sh, which takes it when
# it runs on its own and inherits it when merge-gate.sh is its parent.
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
