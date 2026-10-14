#!/usr/bin/env bash
# Mutation testing on MemoryIndustry cores. Unkilled mutants = FAIL.
#   ./scripts/mutants-gate.sh                       the run, inside the SIL
#   ./scripts/mutants-gate.sh --check-builds FILE   judge the unviable builds of an
#                                                   outcomes.json (quality-gate.sh)
#   ./scripts/mutants-gate.sh --self-test           that judge against its fixtures
#   ./scripts/mutants-gate.sh --disk-plan ROOT FREE_KB PER_COPY_KB RESERVE_KB WANT
#                                                   how many jobs the free disk holds
#   ./scripts/mutants-gate.sh --prepare-scratch WANT
#                                                   the judge's scratch dir, swept and
#                                                   measured (quality-gate.sh)
#   ./scripts/mutants-gate.sh --refuse-targets      exit 2 if a cargo target is inside
#                                                   rust/ or CARGO_TARGET_DIR points
#                                                   there (merge-gate.sh, at the start)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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
export PYTHONUTF8=1 PYTHONIOENCODING=utf-8

# --- unviable has to mean "the compiler said no" -------------------------------
# On 2026-09-23 this gate printed
#     54 mutants tested in 6m: 3 caught, 51 unviable
#     mutants caught=3 missed=0 timeout=0 unviable=51 kill_rate=1.000 min=0.7
# and the SIL said MERGE GATE PASSED. The good run of the same commit was 53
# caught, 1 unviable, in 26 minutes. None of those 51 builds was refused by
# rustc. 44 ended with Failure(-1073741502), 0xC0000142 STATUS_DLL_INIT_FAILED:
# cargo.exe itself could not start under memory pressure. The other 7 ended with
# Failure(101), cargo's ordinary exit for a failed build, but their logs hold no
# compiler diagnostic at all: the rustc child died of the same 0xC0000142 and
# cargo reported that as `could not compile`. One of them replaces
# TtlLruCache<V>::clear with (), which compiles. The kill rate below leaves
# unviable mutants out of its denominator, as it should for a mutant that does
# not compile, so every build the machine killed left the denominator too, and
# the less the machine managed to judge the better the score looked.
#
# So an unviable mutant is genuine only when both of these hold:
#  - its Build phase exit status, as cargo-mutants records it in outcomes.json
#    (phase_results[].process_status, its `Exit` enum: "Success",
#    {"Failure": i32}, "Timeout", {"Signalled": i32} on Unix, "Other"), is a
#    process that ran to its own exit: Failure(1..255). An NTSTATUS arrives as a
#    negative i32 (every error NTSTATUS has its top bit set), a signal as
#    Signalled;
#  - its log (log_path, relative to mutants.out/) shows the compiler saying no
#    inside the build phase, between the `*** ... test --no-run` line and its
#    `*** result:`: an `error[E....]: ` or `error: ` line that is not cargo's
#    own summary (`could not compile`, `build failed`) and not a linker failure,
#    which is the machine too. No readable log is a failure: nothing is judged
#    blind.
#
# A disk that fills up is the machine too, and it starts its line with `error: `
# like a diagnostic does: on 2026-10-13 the step ran out of disk (os error 112)
# and `error: could not write output to ...: There is not enough space on the
# disk.` would have passed for rustc refusing a mutant. A build whose phase holds
# such a line is not judged by its diagnostics at all, and is counted apart so
# that the refusal can say to free disk and not memory.
#
# The file's own `unviable` total is checked against the records too: a format
# change that renamed `summary` would otherwise find zero unviable records,
# judge nothing, and pass.
check_builds() {
  "$PY" - "$1" <<'PY'
import json, os, re, sys

path = sys.argv[1]
data = json.load(open(path, encoding="utf-8"))
outcomes = data.get("outcomes")
if not isinstance(outcomes, list):
    print(f"FAIL: {path} carries no per-mutant outcomes, so nobody can say why a build failed", file=sys.stderr)
    sys.exit(1)
out_dir = os.path.dirname(os.path.abspath(path))

DIAGNOSTIC = re.compile(r"^error(\[E\d{4}\])?: ")
NOT_THE_COMPILER = ("error: could not compile", "error: build failed", "error: linking with")
DISK_FULL = re.compile(r"\(os error (?:112|28)\)|There is not enough space on the disk|No space left on device")
ANSI = re.compile(r"\x1b\[[0-9;]*m")

def exit_status(status):
    if isinstance(status, dict) and set(status) == {"Failure"} and isinstance(status["Failure"], int) and not isinstance(status["Failure"], bool):
        code = status["Failure"]
        if 1 <= code <= 255:
            return None
        if code < 0:
            return f"Failure({code}) = NTSTATUS 0x{code & 0xFFFFFFFF:08X}"
        return f"Failure({code})"
    if isinstance(status, dict) and set(status) == {"Signalled"}:
        return f"killed by signal {status['Signalled']}"
    return json.dumps(status)

def build_diagnostics(outcome):
    rel = outcome.get("log_path")
    if not isinstance(rel, str) or not rel:
        return None
    try:
        with open(os.path.join(out_dir, rel), encoding="utf-8", errors="replace") as log:
            lines = log.read().splitlines()
    except OSError:
        return None
    found, disk, in_build = [], [], False
    for line in (ANSI.sub("", raw) for raw in lines):
        if line.startswith("*** ") and " test --no-run" in line:
            in_build = True
        elif in_build and line.startswith("*** result:"):
            break
        elif in_build and DISK_FULL.search(line):
            disk.append(line)
        elif in_build and DIAGNOSTIC.match(line) and not line.startswith(NOT_THE_COMPILER):
            found.append(line)
    return found, disk

def disk_cause(line):
    code = re.search(r"os error (\d+)", line)
    return f"os error {code.group(1)}" if code else "no space left"

def mutant_name(outcome):
    scenario = outcome.get("scenario")
    if isinstance(scenario, dict) and isinstance(scenario.get("Mutant"), dict):
        return scenario["Mutant"].get("name", "?")
    return json.dumps(scenario)

unviable = [o for o in outcomes if o.get("summary") == "Unviable"]
caught = [o for o in outcomes if o.get("summary") == "CaughtMutant"]
for field, records in (("unviable", unviable), ("caught", caught)):
    declared = int(data.get(field) or 0)
    if len(records) != declared:
        print(f"FAIL: {path} declares {field}={declared} and holds {len(records)} matching record(s).", file=sys.stderr)
        print("      The file changed shape, and a check that finds no records judges nothing.", file=sys.stderr)
        sys.exit(1)

killed, out_of_disk = [], 0
for outcome in unviable:
    build = next((p for p in outcome.get("phase_results") or [] if p.get("phase") == "Build"), None)
    status = build.get("process_status") if build else "no Build phase recorded"
    why = exit_status(status)
    if why is None:
        read = build_diagnostics(outcome)
        if read is None:
            why = f"{json.dumps(status)}, no readable log at {outcome.get('log_path')!r}"
        elif read[1]:
            out_of_disk += 1
            why = f"{json.dumps(status)}, the build ran out of disk ({disk_cause(read[1][0])})"
        elif not read[0]:
            why = f"{json.dumps(status)} with no compiler diagnostic in the build phase"
    if why is not None:
        killed.append((mutant_name(outcome), why))

if killed:
    print(f"FAIL: the machine, not the code, stopped {len(killed)} of {len(unviable)} unviable build(s).", file=sys.stderr)
    print("      Their build ended with a status no compiler returns (an NTSTATUS on Windows, a", file=sys.stderr)
    print("      signal on Unix) or failed without the compiler saying why. They are not mutants", file=sys.stderr)
    print("      that fail to compile, and the kill rate leaves unviable mutants out, so counting", file=sys.stderr)
    print("      them scores a broken run high. Free memory or lower MUTANTS_JOBS and run it", file=sys.stderr)
    print("      again. First of them:", file=sys.stderr)
    for name, why in killed[:5]:
        print(f"        {why}  {name}", file=sys.stderr)
    if out_of_disk:
        print(f"      {out_of_disk} of them ran out of disk (os error 112/28): free disk, not memory, and run it again.", file=sys.stderr)
else:
    print(f"OK  unviable builds: all {len(unviable)} refused by the compiler with a diagnostic, none by the machine")

# The caught half, the same hole facing the other way. A mutant whose test run
# never started (cargo.exe ending in 0xC0000142 like the 44 builds above) is
# filed as CaughtMutant, and a caught mutant counts FOR the kill rate: the
# fewer test runs the machine managed to start, the better the score. A catch
# is a test run that exited on its own, 101 when tests fail, so its Test phase
# has to be Failure(1..255). One NTSTATUS is a catch too, and is named here
# rather than let through a range: 0xC00000FD STATUS_STACK_OVERFLOW, a mutant
# that recurses without end, dying of it where the tests run. Only Failure is
# judged here; a Test phase with any other shape is left as it was.
STACK_OVERFLOW = -1073741571  # 0xC00000FD as the i32 cargo-mutants records
stopped, overflowed = [], 0
for outcome in caught:
    test = next((p for p in reversed(outcome.get("phase_results") or []) if p.get("phase") == "Test"), None)
    status = test.get("process_status") if test else None
    if not (isinstance(status, dict) and set(status) == {"Failure"}):
        continue
    if status["Failure"] == STACK_OVERFLOW:
        overflowed += 1
        continue
    why = exit_status(status)
    if why is not None:
        stopped.append((mutant_name(outcome), why))

if stopped:
    print(f"FAIL: the machine, not the tests, stopped {len(stopped)} of {len(caught)} caught mutant(s).", file=sys.stderr)
    print("      Their test run ended with a status no test run returns: cargo could not start", file=sys.stderr)
    print("      or was killed, no test ran, and cargo-mutants still counts them as caught, which", file=sys.stderr)
    print("      scores a broken run high. Free memory or lower MUTANTS_JOBS and run it again.", file=sys.stderr)
    print("      First of them:", file=sys.stderr)
    for name, why in stopped[:5]:
        print(f"        {why}  {name}", file=sys.stderr)
else:
    note = ""
    if overflowed:
        note = (f"; {overflowed} of them died of 0xC00000FD STATUS_STACK_OVERFLOW, a mutant that"
                " recurses without end, which is its tests catching it")
    print(f"OK  caught mutants: all {len(caught)} ended their test run with its own exit code{note}")

if killed or stopped:
    sys.exit(1)
PY
}

# --- the mutation step does not fill the disk ---------------------------------
# On 2026-10-13 the step ran out of disk (os error 112). cargo-mutants copies
# rust/ once per job and each copy compiles its own target/, and nothing looked at
# the disk before copying. Measured that day on Windows: a copy weighed 27.58 GB,
# 25.8 GB of it target-* directories of other builds that lived inside rust/, and
# its own build another 14.1 GB. quality-gate.sh calls the two modes below before
# its cargo mutants, and the main path of this script before its own.
#
#   --disk-plan ROOT FREE_KB PER_COPY_KB RESERVE_KB WANT   pure: no disk, no environment
#   --prepare-scratch WANT   the judge's scratch dir, the sweep, the free space, the plan

# Measured 2026-10-13 on Windows 11 under Git Bash: `cargo mutants --file
# src/search/rrf.rs -F <two mutants> --jobs 2 --leak-dirs --output <dir> -- --lib`
# with TMP, TEMP and TMPDIR on a scratch directory and CARGO_PROFILE_DEV_DEBUG=0:
# a finished copy weighed 8.94 GB, and ceil(8.94 x 1.25) = 12. Without debug info
# off it was 14.1 GB (54 % of it .pdb), which would have made 18. Linux is not
# measured: this is the Windows figure for both until it is.
MUTANTS_SCRATCH_GB_PER_COPY_MEASURED=12
MUTANTS_SWEEP_MINUTES=360
MUTANTS_KB_PER_GB=1048576

mutants_gb() { awk -v kb="$1" -v per="$MUTANTS_KB_PER_GB" 'BEGIN { printf "%.1f", kb / per }'; }

# What cargo-mutants reads as its temp dir: TMP/TEMP on Windows, TMPDIR on Unix.
mutants_system_temp() {
  local dir="${TMP:-${TEMP:-}}"
  if [[ -n "$dir" ]] && command -v cygpath >/dev/null 2>&1; then
    dir="$(cygpath -u "$dir")"
  else
    dir="${TMPDIR:-/tmp}"
  fi
  printf '%s\n' "${dir%/}"
}

# The scratch is always a directory of its own inside the one MUTANTS_SCRATCH_DIR
# names, never that directory: the sweep deletes what is old in the scratch, and
# MUTANTS_SCRATCH_DIR=D:\ (the advice of the disk FAIL) would have lost every entry of
# D:\ older than 6 h. The marker is what proves a directory was made by this script.
MUTANTS_MARKER=.memory-industry-mutants

mutants_scratch_dir() {
  local base="${MUTANTS_SCRATCH_DIR:-$(mutants_system_temp)}"
  base="$(cygpath -u "$base" 2>/dev/null || printf '%s' "$base")"
  printf '%s/memory-industry-mutants\n' "${base%/}"
}

# Is PATH_ the directory DIR or somewhere inside it? By identity, not by spelling,
# so that /c/ and C:/ or a symlink do not hide it; a DIR that does not exist holds nothing.
mutants_dir_holds() {
  local dir="$1" path_="$2"
  while [[ -n "$path_" && "$path_" != . ]]; do
    [[ "$path_" -ef "$dir" ]] && return 0
    [[ "$path_" != / ]] || return 1
    path_="$(dirname "$path_")"
  done
  return 1
}

# jobs= on stdout, the human lines on stderr. 1: not even one job fits. 2: a number
# is missing or not a number, and not knowing the free space is neither no room nor
# all the room there is.
mutants_disk_plan() {
  local root="$1" free="$2" per="$3" reserve="$4" want="$5" value jobs
  if ! [[ "$free" =~ ^[0-9]+$ ]]; then
    echo "FAIL: could not read the free space of $root (got '$free'), so nobody can say whether the mutation step fits." >&2
    return 2
  fi
  for value in "$per" "$reserve" "$want"; do
    [[ "$value" =~ ^[0-9]+$ ]] || { echo "FAIL: --disk-plan wants whole numbers for the size per copy, the reserve and the jobs (got '$value')." >&2; return 2; }
  done
  free=$((10#$free)) per=$((10#$per)) reserve=$((10#$reserve)) want=$((10#$want))
  (( per > 0 && want > 0 )) || { echo "FAIL: --disk-plan wants a size per copy and a number of jobs above 0." >&2; return 2; }
  if (( free < per + reserve )); then
    echo "FAIL: mutation needs $(mutants_gb $((per + reserve))) GB free on $root (1 job x $(mutants_gb "$per") GB per copy + $(mutants_gb "$reserve") GB reserve) and there are $(mutants_gb "$free") GB. Nothing was copied or built." >&2
    echo "      Free disk, or point MUTANTS_SCRATCH_DIR at a volume with room (the scratch is its memory-industry-mutants subdirectory)." >&2
    return 1
  fi
  jobs=$(( (free - reserve) / per ))
  (( jobs < want )) || jobs=$want
  printf 'jobs=%s\n' "$jobs"
  echo "disk: $root $(mutants_gb "$free") GB free, $(mutants_gb "$per") GB per copy, $(mutants_gb "$reserve") GB reserve -> $jobs job(s)" >&2
  (( jobs == want )) || echo "capped by disk: $want wanted, $jobs fit" >&2
}

# A cargo target inside rust/ is copied whole into every job: cargo-mutants leaves
# out only its own target/, and cargo writes a CACHEDIR.TAG in each. Instant, where
# measuring them with du would take minutes.
mutants_refuse_targets_in_tree() {
  local tag dir named=""
  while IFS= read -r tag; do
    dir="${tag%/CACHEDIR.TAG}"
    [[ "$dir" == "$ROOT/rust/target" ]] || named+=" ${dir#"$ROOT"/}"
  done < <(find -P "$ROOT/rust" -mindepth 2 -maxdepth 2 -name CACHEDIR.TAG 2>/dev/null | LC_ALL=C sort)
  [[ -n "$named" ]] || return 0
  echo "FAIL: cargo targets inside rust/:$named" >&2
  echo "      cargo-mutants copies every one of them into every job's copy (it leaves out only its" >&2
  echo "      own rust/target): on 2026-10-13 four of them were 25.8 GB of each copy and filled the" >&2
  echo "      disk. Delete them, or point CARGO_TARGET_DIR outside rust/. Nothing was copied or built." >&2
  return 2
}

# The same copy, before cargo has made it: a CARGO_TARGET_DIR inside rust/ is a
# target-to-be, and cargo writes it at the first build. Only merge-gate.sh asks
# (--refuse-targets): the mutation step itself unsets CARGO_TARGET_DIR, so there
# it names nothing that will be copied. A relative value is relative to rust/,
# where the gate runs cargo.
mutants_refuse_target_dir_in_tree() {
  local dir="${CARGO_TARGET_DIR:-}"
  [[ -n "$dir" ]] || return 0
  dir="$(cygpath -u "$dir" 2>/dev/null || printf '%s' "$dir")"
  case "$dir" in
    /* | [A-Za-z]:*) ;;
    *) dir="$ROOT/rust/$dir" ;;
  esac
  dir="${dir%/}"
  case "$dir" in
    "$ROOT/rust/target" | "$ROOT/rust/target/"*) return 0 ;;
    "$ROOT/rust" | "$ROOT/rust/"*) ;;
    *) return 0 ;;
  esac
  echo "FAIL: CARGO_TARGET_DIR points inside rust/: ${dir#"$ROOT"/}" >&2
  echo "      cargo would build there and cargo-mutants would copy it into every job's copy (it leaves" >&2
  echo "      out only its own rust/target), as it did with the four targets of 2026-10-13. Point" >&2
  echo "      CARGO_TARGET_DIR outside rust/. Nothing was copied or built." >&2
  return 2
}

mutants_remove() {
  local path="$1" kb
  kb="$(du -sk "$path" 2>/dev/null | cut -f1 || true)"
  rm -rf "$path" 2>/dev/null || true
  if [[ -e "$path" ]]; then
    echo "could not remove ${path##*/}" >&2
    return 0
  fi
  kb="${kb:-0}"
  echo "swept ${path##*/} ($(mutants_gb "$kb") GB)" >&2
  swept_n=$((swept_n + 1))
  swept_kb=$((swept_kb + kb))
}

# Another gate's copies are in use. Under merge-gate.sh the lock is this process's
# parent's, and inherit_gate_lock is what says so.
mutants_sweep_allowed() {
  local holder verdict=0
  holder="$(cat "$GATE_LOCK/owner" 2>/dev/null || true)"
  [[ -n "$holder" ]] || return 0
  inherit_gate_lock "$GATE_LOCK" >&2 && return 0
  owner_alive "$holder" || verdict=$?
  case "$verdict" in
    0) echo "another gate is running ($(describe_owner "$holder")): sweep skipped" >&2; return 1 ;;
    2) echo "the gate lock was taken on $(record_field where "$holder"), whose process cannot be looked up from here: sweep skipped" >&2; return 1 ;;
  esac
}

# Everything in the judge's own scratch older than 6 h, marker excepted (sweeping
# it would delete the proof that the directory is the judge's); in the system temp
# only the leftovers from before the scratch existed, and only what proves to be
# the judge's. A scratch without the marker is left whole: the directory may
# have been somebody's before this script ever named it.
mutants_sweep() {
  local scr="$1" sys="$2" path name kb
  swept_n=0 swept_kb=0
  mutants_sweep_allowed || return 0
  if [[ -f "$scr/$MUTANTS_MARKER" ]]; then
    while IFS= read -r -d '' path; do
      mutants_remove "$path"
    done < <(find -P "$scr" -mindepth 1 -maxdepth 1 ! -name "$MUTANTS_MARKER" -mmin +"$MUTANTS_SWEEP_MINUTES" -print0 2>/dev/null)
  else
    echo "$scr is not provably the gate's scratch, left alone" >&2
  fi
  while IFS= read -r -d '' path; do
    name="${path##*/}"
    if grep -q '^name = "memory-industry"[[:space:]]*$' "$path/Cargo.toml" 2>/dev/null; then
      mutants_remove "$path"
    else
      kb="$(du -sk "$path" 2>/dev/null | cut -f1 || true)"
      echo "not provably the gate's, left alone: $name ($(mutants_gb "${kb:-0}") GB)" >&2
    fi
  done < <(find -P "$sys" -mindepth 1 -maxdepth 1 -type d -name 'cargo-mutants-rust-*.tmp' -mmin +"$MUTANTS_SWEEP_MINUTES" -print0 2>/dev/null)
  while IFS= read -r -d '' path; do
    mutants_remove "$path"
  done < <(find -P "$sys" -mindepth 1 -maxdepth 1 -type d -name 'cuba-attrs-*' -mmin +"$MUTANTS_SWEEP_MINUTES" -print0 2>/dev/null)
  while IFS= read -r -d '' path; do
    [[ ! -d "$path/mutants.out" ]] || mutants_remove "$path"
  done < <(find -P "$sys" -mindepth 1 -maxdepth 1 -type d -name 'tmp.*' -mmin +"$MUTANTS_SWEEP_MINUTES" -print0 2>/dev/null)
  echo "sweep: freed $(mutants_gb "$swept_kb") GB in $swept_n entries" >&2
}

# Sets MUTANTS_PLANNED_JOBS and MUTANTS_SCRATCH, and returns the rule's own code.
# Sourced here and not at the top because the modes that judge a file need no lock.
mutants_prepare_scratch() {
  local want="$1" sys scr free per_gb reserve_gb value plan rc=0
  # shellcheck source=scripts/gate-lock.sh
  source "$ROOT/scripts/gate-lock.sh"
  mutants_refuse_targets_in_tree || return $?
  sys="$(mutants_system_temp)"
  scr="$(mutants_scratch_dir)"
  # Everything old in the scratch is swept, so it cannot be a directory of anybody else's,
  # nor one that holds the repo or HOME: the sweep would reach into them.
  if [[ "$scr" -ef "$sys" || "$scr" -ef "$HOME" || "$scr" -ef / ]]; then
    echo "FAIL: the scratch directory $scr is a directory of everybody's, and the judge sweeps everything in its own that is older than 6 h. Point MUTANTS_SCRATCH_DIR at one of its own." >&2
    return 2
  fi
  if mutants_dir_holds "$scr" "$ROOT" || mutants_dir_holds "$scr" "$HOME"; then
    echo "FAIL: the scratch directory $scr holds the repo ($ROOT) or HOME ($HOME), and the judge sweeps what is old in it. Point MUTANTS_SCRATCH_DIR somewhere that holds neither." >&2
    return 2
  fi
  mkdir -p "$scr" || { echo "FAIL: could not create the scratch directory $scr" >&2; return 2; }
  # The marker is written only into a directory that is empty, which is to say
  # one this script just made or one nobody has used.
  if [[ -z "$(ls -A "$scr" 2>/dev/null)" ]]; then
    : > "$scr/$MUTANTS_MARKER" || { echo "FAIL: could not write the marker into the scratch directory $scr" >&2; return 2; }
  fi
  mutants_sweep "$scr" "$sys"
  free="$(df -Pk "$scr" 2>/dev/null | awk 'NR==2 {print $4}' || true)"
  per_gb="${MUTANTS_SCRATCH_GB_PER_COPY:-$MUTANTS_SCRATCH_GB_PER_COPY_MEASURED}"
  reserve_gb="${CUBA_GATE_MIN_FREE_GB:-8}"
  for value in "$per_gb" "$reserve_gb"; do
    [[ "$value" =~ ^[0-9]+$ ]] || { echo "FAIL: MUTANTS_SCRATCH_GB_PER_COPY and CUBA_GATE_MIN_FREE_GB are whole numbers of GB (got '$value')." >&2; return 2; }
  done
  plan="$(mutants_disk_plan "$scr" "$free" $((10#$per_gb * MUTANTS_KB_PER_GB)) $((10#$reserve_gb * MUTANTS_KB_PER_GB)) "$want")" || rc=$?
  (( rc == 0 )) || return "$rc"
  MUTANTS_PLANNED_JOBS="${plan#jobs=}"
  MUTANTS_SCRATCH="$scr"
}

# --- self-test begin ---
# --- self-test: the build judge gets the fixtures that have to stop it --------
# Each fixture is an outcomes.json in the shape cargo-mutants 27.1.0 writes, cut
# down to what the judge reads, with a log per unviable mutant cut from real
# ones, and each also holds a caught mutant: the old kill-rate formula scores
# every one of them 1.000, which is the point.
self_test() {
  local rc
  # Global, not local: the EXIT trap reads it after this function has exited.
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  # write_log FILE KIND: the build phase of a real log, ending one of seven ways.
  #   e0308    rustc refuses the mutant with an error code
  #   plain    rustc refuses it with a plain `error: ` (a let-chain `||`)
  #   silent   the 2026-09-23 shape: cargo's summary and no diagnostic, plus an
  #            error line after `*** result:`, where it proves nothing
  #   outside  silent, with an error line in the mutation diff instead, before
  #            the build phase starts
  #   linker   the only error is link.exe dying of 0xc0000142: three of the
  #            seven 101s of 2026-09-23 read exactly like this
  #   nospace112 / nospace28   the only error is the disk filling up (Windows
  #            os error 112, Unix os error 28): rustc starts the line with
  #            `error: ` like a diagnostic, and says nothing about the source
  write_log() {
    local file="$1" kind="$2"
    {
      echo '*** src/search/cache.rs:69:9: replace TtlLruCache<V>::clear with ()'
      echo
      echo '*** mutation diff:'
      [[ "$kind" == outside ]] && echo 'error[E0308]: mismatched types'
      echo '*** C:\\Users\\x\\.cargo\\bin\\cargo.exe test --no-run --verbose --package=memory-industry@0.27.0'
      echo '     Running `rustc --crate-name memory_industry --edition=2024 src\lib.rs`'
      case "$kind" in
        e0308) echo 'error[E0308]: mismatched types' ;;
        plain) echo 'error: `||` operators are not supported in let chain conditions' ;;
        linker) echo 'error: linking with `link.exe` failed: exit code: 0xc0000142' ;;
        nospace112) echo 'error: could not write output to C:\Users\x\AppData\Local\Temp\memory-industry-mutants\cargo-mutants-rust-Ab12.tmp\target\debug\deps\memory_industry-0a1b.rlib: There is not enough space on the disk. (os error 112)' ;;
        nospace28) echo 'error: failed to write /tmp/x/target/debug/deps/libmemory_industry-0a1b.rlib: No space left on device (os error 28)' ;;
      esac
      echo 'error: could not compile `memory-industry` (lib test)'
      echo
      echo 'Caused by:'
      echo 'warning: build failed, waiting for other jobs to finish...'
      echo
      echo '*** result: Failure(101)'
      [[ "$kind" == silent ]] && echo 'error[E0308]: after the build phase'
      true
    } > "$file"
  }

  # fixture NAME DECLARED_UNVIABLE STATUS:LOG...: one Unviable record per
  # argument, whose log is written as LOG, or left missing when LOG is none.
  # Writes $tmp/NAME/outcomes.json, with its logs under $tmp/NAME/log/.
  # The one caught mutant's Test phase ends with $caught_status, 101 unless a
  # fixture sets it, and the file declares $caught_declared caught (1).
  fixture() {
    local dir="$tmp/$1" declared="$2" records spec status log n=0
    local caught='{"Failure":101}' caught_total=1
    [[ -n "${caught_status:-}" ]] && caught="$caught_status"
    [[ -n "${caught_declared:-}" ]] && caught_total="$caught_declared"
    shift 2
    mkdir -p "$dir/log"
    records='{"scenario":"Baseline","summary":"Success","log_path":"log/baseline.log","phase_results":[{"phase":"Build","process_status":"Success"},{"phase":"Test","process_status":"Success"}]}'
    records+=',{"scenario":{"Mutant":{"name":"src/search/rrf.rs:9:9: replace f -> u32 with 0"}},"summary":"CaughtMutant","log_path":"log/caught.log","phase_results":[{"phase":"Build","process_status":"Success"},{"phase":"Test","process_status":'"$caught"'}]}'
    for spec in "$@"; do
      n=$((n + 1))
      status="${spec%:*}"
      log="${spec##*:}"
      [[ "$log" == none ]] || write_log "$dir/log/m$n.log" "$log"
      records+=",{\"scenario\":{\"Mutant\":{\"name\":\"src/search/cache.rs:$n:1: replace g with h\"}},\"summary\":\"Unviable\",\"log_path\":\"log/m$n.log\",\"phase_results\":[{\"phase\":\"Build\",\"process_status\":$status}]}"
    done
    printf '{"outcomes":[%s],"total_mutants":%d,"caught":%d,"missed":0,"timeout":0,"unviable":%d,"success":0,"cargo_mutants_version":"27.1.0"}\n' \
      "$records" $(( $# + 1 )) "$caught_total" "$declared" > "$dir/outcomes.json"
  }
  judge() { rc=0; check_builds "$tmp/$1/outcomes.json" >"$tmp/out" 2>"$tmp/err" || rc=$?; }

  # The presence anchor: genuine compile errors pass, and say so. Without this
  # one, a judge that refused everything would pass the red fixtures below.
  fixture genuine 2 '{"Failure":101}:e0308' '{"Failure":1}:plain'
  judge genuine
  (( rc == 0 )) || self_fail "two builds rustc refused with a diagnostic were refused: $(cat "$tmp/err")"
  grep -q 'all 2 refused by the compiler' "$tmp/out" || self_fail "a genuine pass did not say what it judged: $(cat "$tmp/out")"

  # The 44 of 2026-09-23 in miniature: one genuine, one cargo.exe that Windows
  # could not start. Red, naming the NTSTATUS and the count.
  fixture ntstatus 2 '{"Failure":101}:e0308' '{"Failure":-1073741502}:silent'
  judge ntstatus
  (( rc != 0 )) || self_fail "STATUS_DLL_INIT_FAILED was accepted as an unviable mutant"
  grep -q 'stopped 1 of 2 unviable' "$tmp/err" || self_fail "the refusal did not count the machine's builds: $(cat "$tmp/err")"
  grep -q '0xC0000142' "$tmp/err" || self_fail "the refusal did not name the NTSTATUS: $(cat "$tmp/err")"

  # The other 7: cargo exits 101 as it would for a compile error, but rustc
  # never said a word. The exit code alone would call this genuine.
  fixture silent 2 '{"Failure":101}:e0308' '{"Failure":101}:silent'
  judge silent
  (( rc != 0 )) || self_fail "a Failure(101) with no compiler diagnostic was accepted as an unviable mutant"
  grep -q 'no compiler diagnostic in the build phase' "$tmp/err" || self_fail "the refusal did not say what was missing: $(cat "$tmp/err")"

  # The linker is not the compiler: rustc said nothing about the source.
  fixture linker 1 '{"Failure":101}:linker'
  judge linker
  (( rc != 0 )) || self_fail "a link.exe that could not start was taken for a compile error"

  # A diagnostic before the build phase says nothing about the build.
  fixture outside 1 '{"Failure":101}:outside'
  judge outside
  (( rc != 0 )) || self_fail "an error line in the mutation diff was taken for the compiler's"

  # No log, no verdict: a missing file is not a compile error.
  fixture nolog 1 '{"Failure":101}:none'
  judge nolog
  (( rc != 0 )) || self_fail "a Failure(101) whose log is missing was accepted as an unviable mutant"
  grep -q 'no readable log' "$tmp/err" || self_fail "the refusal did not say the log was missing: $(cat "$tmp/err")"

  # The Unix twin: an OOM killer's SIGKILL is not a compile error either.
  fixture signal 1 '{"Signalled":9}:e0308'
  judge signal
  (( rc != 0 )) || self_fail "a build killed by signal 9 was accepted as an unviable mutant"

  # A record that says nothing about its build's exit proves nothing about it.
  fixture timeout 1 '"Timeout":e0308'
  judge timeout
  (( rc != 0 )) || self_fail "an unviable build that timed out was accepted as a compile error"

  # The file's total disagrees with its records: the shape changed under us.
  fixture miscount 3 '{"Failure":101}:e0308'
  judge miscount
  (( rc != 0 )) || self_fail "unviable=3 over one Unviable record was accepted"

  # The caught half. Every fixture above already holds one caught mutant
  # whose tests failed with 101, and they pass: that is its presence anchor.
  # A cargo that could not start its test run, as 44 builds could not on
  # 2026-09-23: no test ran, and cargo-mutants still files it as caught.
  caught_status='{"Failure":-1073741502}' fixture caught_ntstatus 0
  judge caught_ntstatus
  (( rc != 0 )) || self_fail "a caught mutant whose test run ended with 0xC0000142 was accepted"
  grep -q 'stopped 1 of 1 caught' "$tmp/err" || self_fail "the refusal did not count the machine's catches: $(cat "$tmp/err")"
  grep -q '0xC0000142' "$tmp/err" || self_fail "the refusal did not name the NTSTATUS: $(cat "$tmp/err")"

  # A test process exits 0..255, whatever it does; 256 is not one of them.
  caught_status='{"Failure":256}' fixture caught_range 0
  judge caught_range
  (( rc != 0 )) || self_fail "a caught mutant whose test run ended with Failure(256) was accepted"

  # The one NTSTATUS that is the tests catching a mutant: it recursed without
  # end and the test process died of it. Accepted, and said out loud.
  caught_status='{"Failure":-1073741571}' fixture caught_overflow 0
  judge caught_overflow
  (( rc == 0 )) || self_fail "a caught mutant that overflowed its stack was refused: $(cat "$tmp/err")"
  grep -q 'STATUS_STACK_OVERFLOW' "$tmp/out" || self_fail "the stack overflow was accepted in silence: $(cat "$tmp/out")"

  # The file's caught total disagrees with its records.
  caught_declared=2 fixture caught_miscount 0
  judge caught_miscount
  (( rc != 0 )) || self_fail "caught=2 over one CaughtMutant record was accepted"

  # --- G3: the mutation step does not fill the disk ----------------------------
  # On 2026-10-13 the step ran out of disk (os error 112) because every job copied
  # rust/ with the cargo targets inside it (25.8 GB of other people's targets, plus
  # the 14.1 GB build of each copy) and nothing looked at the disk before copying.
  # Two modes carry the rule, and the two judges call them before cargo mutants:
  #   --disk-plan ROOT FREE_KB PER_COPY_KB RESERVE_KB WANT   pure: how many jobs fit
  #   --prepare-scratch WANT   the judge's own scratch dir, the sweep, the free
  #                            space read, the plan; stdout is `jobs=` and `scratch=`
  # They are called through the CLI, not as functions, so that before they exist
  # the failure is `unknown argument` and not `command not found`.
  # Doubles are put on PATH like merge-gate.sh does with stat (FAKE_FS): no
  # variable of the code under test is invented for them.
  kb=1048576   # one GB, in the KB that df -Pk prints
  mkdir -p "$tmp/bin"
  printf '%s\n' '#!/bin/sh' \
    '[ "${FAKE_DF_FAIL:-}" = 1 ] && exit 1' \
    'for last; do :; done' \
    "echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'" \
    'echo "fake 0 0 ${FAKE_DF_AVAIL_KB} 0% $last"' > "$tmp/bin/df"
  printf '%s\n' '#!/bin/sh' \
    'printf "%s\t%s\t%s\t%s\t%s\t%s\n" "$*" "${TMP:-}" "${TEMP:-}" "${TMPDIR:-}" "${CARGO_PROFILE_DEV_DEBUG:-}" "${CARGO_INCREMENTAL:-}" >> "$FAKE_CALLS"' \
    'exit 0' > "$tmp/bin/cargo"
  chmod +x "$tmp/bin/df" "$tmp/bin/cargo"

  # The pure rule.
  plan() {
    rc=0
    bash "$ROOT/scripts/mutants-gate.sh" --disk-plan /scratch-root "$@" >"$tmp/plan.out" 2>"$tmp/plan.err" || rc=$?
  }
  plan_said() { grep -qF -- "$1" "$tmp/plan.out" "$tmp/plan.err"; }
  plan_jobs() { grep -qx "jobs=$1" "$tmp/plan.out"; }

  plan $((200 * kb)) $((20 * kb)) $((8 * kb)) 3
  if (( rc != 0 )) || ! plan_jobs 3 || plan_said capped; then
    self_fail "P1: 200 GB free for 3 jobs of 20 GB and 8 GB reserve was not jobs=3 with nothing capped (exit $rc): $(cat "$tmp/plan.out" "$tmp/plan.err")"
  fi

  plan $((100 * kb)) $((41 * kb)) $((8 * kb)) 3
  if (( rc != 0 )) || ! plan_jobs 2 || ! plan_said 'capped by disk: 3 wanted, 2 fit'; then
    self_fail "P2: 100 GB free, 41 GB per copy, 8 GB reserve did not cut 3 jobs to 2 out loud (exit $rc): $(cat "$tmp/plan.out" "$tmp/plan.err")"
  fi

  plan $((45 * kb)) $((41 * kb)) $((8 * kb)) 2
  if (( rc != 1 )); then
    self_fail "P3: 45 GB free for one 41 GB copy and 8 GB reserve exited $rc, not 1: $(cat "$tmp/plan.out" "$tmp/plan.err")"
  fi
  for want in '45.0 GB' '41.0 GB per copy' '8.0 GB reserve' '49.0 GB' '/scratch-root' 'Nothing was copied or built'; do
    grep -qF -- "$want" "$tmp/plan.err" || self_fail "P3: the refusal did not say '$want': $(cat "$tmp/plan.err")"
  done
  ! grep -q '^jobs=' "$tmp/plan.out" || self_fail "P3: a refusal still printed a jobs= line: $(cat "$tmp/plan.out")"

  # Fits exactly: <=, not <. One KB less does not.
  plan $((49 * kb)) $((41 * kb)) $((8 * kb)) 1
  if (( rc != 0 )) || ! plan_jobs 1; then
    self_fail "P4: exactly 49 GB for 41 GB + 8 GB reserve was refused (exit $rc): $(cat "$tmp/plan.out" "$tmp/plan.err")"
  fi
  plan $((49 * kb - 1)) $((41 * kb)) $((8 * kb)) 1
  (( rc == 1 )) || self_fail "P5: 1 KB under the 49 GB it needs exited $rc, not 1: $(cat "$tmp/plan.out" "$tmp/plan.err")"

  # Not knowing the free space is neither zero nor infinite. Exit 2, and it must
  # not claim that nothing was copied: nothing was measured either.
  for free in '' abc; do
    plan "$free" $((41 * kb)) $((8 * kb)) 2
    if (( rc != 2 )) || ! plan_said 'could not read the free space' || grep -qF 'Nothing was copied' "$tmp/plan.err"; then
      self_fail "P6: free space '$free' did not exit 2 with 'could not read the free space' (exit $rc): $(cat "$tmp/plan.out" "$tmp/plan.err")"
    fi
  done

  plan $((100 * kb)) 0 $((8 * kb)) 2
  (( rc == 2 )) || self_fail "P7: a size per copy of 0 exited $rc, not 2: $(cat "$tmp/plan.out" "$tmp/plan.err")"
  plan $((100 * kb)) $((41 * kb)) $((8 * kb)) 0
  (( rc == 2 )) || self_fail "P7: 0 jobs wanted exited $rc, not 2: $(cat "$tmp/plan.out" "$tmp/plan.err")"

  # The impure half runs in a throwaway copy of the scripts over an empty rust/.
  # It acts on $ROOT/rust (the guard against a cargo target inside the tree below)
  # and on the machine's temp dir, and the real checkout may hold anything at the
  # moment this runs: a target-* a developer left in rust/ would turn every row
  # red for a reason that is not the row's. HOME, TMP, TEMP and TMPDIR are the
  # fixture's, so the real temp dir and the real gate lock are never looked at.
  # package.json comes along because gate_linux_env reads engines.node from it.
  mk_tree() {
    mkdir -p "$tmp/$1/scripts" "$tmp/$1/rust"
    cp "$ROOT/scripts/mutants-gate.sh" "$ROOT/scripts/gate-lock.sh" "$tmp/$1/scripts/"
    [[ ! -f "$ROOT/package.json" ]] || cp "$ROOT/package.json" "$tmp/$1/"
  }
  norm() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }
  home="$tmp/home"
  sys="$tmp/systemp"
  scr="$sys/memory-industry-mutants"
  # G4/C1: the sweep deletes what is old in the scratch, so the scratch is never
  # the directory MUTANTS_SCRATCH_DIR names (D:\ would be swept of everything older
  # than 6 h): it is that directory's own subdirectory, and only one that carries
  # the marker the script wrote when it made it is swept.
  marker=".memory-industry-mutants"
  sys_native="$(cygpath -w "$sys" 2>/dev/null || printf '%s' "$sys")"
  avail=$((500 * kb))
  per_copy=10
  fresh() { rm -rf "$sys" "$home" "$tmp/cargo.calls"; mkdir -p "$sys" "$home"; }
  # in_tree TREE [VAR=value]... -- ARGS...: the script of that tree, in the
  # fixture's environment, its stdout and stderr in $tmp/run.out and run.err.
  in_tree() {
    local t="$1"
    local -a extra=()
    shift
    while [[ "${1:-}" != -- ]]; do extra+=("$1"); shift; done
    shift
    local -a e=(HOME="$home" TMP="$sys_native" TEMP="$sys_native" TMPDIR="$sys"
                PATH="$tmp/bin:$PATH" FAKE_DF_AVAIL_KB="$avail" FAKE_CALLS="$tmp/cargo.calls")
    [[ -z "$per_copy" ]] || e+=(MUTANTS_SCRATCH_GB_PER_COPY="$per_copy")
    rc=0
    env -u MUTANTS_SCRATCH_GB_PER_COPY -u MUTANTS_SCRATCH_DIR -u CUBA_GATE_LOCK_OWNER \
        -u CUBA_GATE_MIN_FREE_GB -u MUTANTS_JOBS -u FAKE_DF_FAIL \
        -u CARGO_PROFILE_DEV_DEBUG -u CARGO_INCREMENTAL \
        "${e[@]}" ${extra[@]+"${extra[@]}"} \
        bash "$tmp/$t/scripts/mutants-gate.sh" "$@" >"$tmp/run.out" 2>"$tmp/run.err" || rc=$?
  }
  run_said() { grep -qF -- "$1" "$tmp/run.err"; }
  # mk_entry DIR AGE: a directory with something in it, whose own mtime is AGE
  # old, set last so that creating the contents does not make it new again.
  mk_entry() {
    mkdir -p "$1"
    : > "$1/x"
    touch -d "$2" "$1"
  }
  # mk_marker DIR AGE: the marker that makes DIR the judge's scratch. It is made
  # old on purpose: a sweep that took the marker for one more old entry would
  # delete the proof that the directory is its own, and the next run would sweep nothing.
  mk_marker() {
    mkdir -p "$1"
    : > "$1/$marker"
    touch -d "$2" "$1/$marker"
  }
  ran_mutants() { [[ -f "$tmp/cargo.calls" ]] && grep -q '^mutants --file' "$tmp/cargo.calls"; }

  mk_tree clean

  # W1: the judge's own scratch is swept of what is older than 6 h, and says so.
  fresh
  mk_entry "$scr/cargo-mutants-rust-old.tmp" '7 hours ago'
  mk_entry "$scr/cargo-mutants-rust-new.tmp" '1 hour ago'
  mk_entry "$scr/cuba-attrs-x-1" '7 hours ago'
  mk_marker "$scr" '7 hours ago'
  in_tree clean -- --prepare-scratch 2
  if (( rc != 0 )) || ! grep -qx 'jobs=2' "$tmp/run.out" || [[ "$(wc -l <"$tmp/run.out")" -ne 2 ]]; then
    self_fail "W1: --prepare-scratch 2 over 500 GB free did not print jobs=2 and scratch= alone on stdout (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  got="$(sed -n 's/^scratch=//p' "$tmp/run.out")"
  [[ "$(norm "$got")" == "$(norm "$scr")" ]] || self_fail "W1: scratch= was '$got', not the judge's own directory under the system temp ($scr)"
  [[ ! -e "$scr/cargo-mutants-rust-old.tmp" && ! -e "$scr/cuba-attrs-x-1" ]] || self_fail "W1: an entry of 7 h in the judge's scratch was not swept"
  [[ -d "$scr/cargo-mutants-rust-new.tmp" ]] || self_fail "W1: an entry of 1 h in the judge's scratch was swept"
  [[ -f "$scr/$marker" ]] || self_fail "W1: the sweep took the marker of the judge's scratch for one more old entry and deleted it"
  run_said 'swept cargo-mutants-rust-old.tmp' || self_fail "W1: the sweep did not say 'swept cargo-mutants-rust-old.tmp': $(cat "$tmp/run.err")"

  # C1/S1: the scratch is MUTANTS_SCRATCH_DIR's own subdirectory, never the directory
  # itself. The first version swept whatever the variable named, and the FAIL for a
  # full disk tells people to point it at a volume with room: D:\ would lose every
  # entry older than 6 h. The old entry beside the scratch is somebody else's.
  xdir="$tmp/ex"
  fresh
  mk_entry "$xdir/viejo" '7 hours ago'
  : > "$xdir/viejo.log"
  touch -d '7 hours ago' "$xdir/viejo.log"
  mk_entry "$xdir/memory-industry-mutants/cargo-mutants-rust-old.tmp" '7 hours ago'
  mk_entry "$xdir/memory-industry-mutants/cargo-mutants-rust-new.tmp" '1 hour ago'
  mk_marker "$xdir/memory-industry-mutants" '7 hours ago'
  in_tree clean MUTANTS_SCRATCH_DIR="$xdir" -- --prepare-scratch 2
  got="$(sed -n 's/^scratch=//p' "$tmp/run.out")"
  if (( rc != 0 )) || [[ "$(norm "$got")" != "$(norm "$xdir/memory-industry-mutants")" ]]; then
    self_fail "S1: with MUTANTS_SCRATCH_DIR=X the scratch was '$got', not X/memory-industry-mutants (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  if [[ ! -d "$xdir/viejo" || ! -f "$xdir/viejo.log" ]]; then
    self_fail "S1: an entry of 7 h that sits in X and not in X/memory-industry-mutants was swept: $(ls -A "$xdir" 2>&1)"
  fi
  [[ ! -e "$xdir/memory-industry-mutants/cargo-mutants-rust-old.tmp" && -d "$xdir/memory-industry-mutants/cargo-mutants-rust-new.tmp" ]] \
    || self_fail "S1: inside the marked scratch the 7 h entry was not swept, or the 1 h one was: $(ls -A "$xdir/memory-industry-mutants" 2>&1)"

  # S2: a directory with the scratch's name and no marker is not provably the
  # judge's, whatever is in it. It is left alone, said out loud, and not adopted by
  # writing the marker into it (the next run would then sweep what was never ours).
  fresh
  rm -rf "$xdir"
  mk_entry "$xdir/memory-industry-mutants/cargo-mutants-rust-old.tmp" '7 hours ago'
  mk_entry "$xdir/memory-industry-mutants/mis-fotos" '7 hours ago'
  in_tree clean MUTANTS_SCRATCH_DIR="$xdir" -- --prepare-scratch 2
  if [[ ! -d "$xdir/memory-industry-mutants/cargo-mutants-rust-old.tmp" || ! -d "$xdir/memory-industry-mutants/mis-fotos" ]]; then
    self_fail "S2: a directory named like the scratch but without the marker was swept: $(ls -A "$xdir/memory-industry-mutants" 2>&1)"
  fi
  run_said "not provably the gate's scratch, left alone" || self_fail "S2: it did not say \"not provably the gate's scratch, left alone\": $(cat "$tmp/run.err")"
  [[ ! -e "$xdir/memory-industry-mutants/$marker" ]] || self_fail "S2: a directory that was not the judge's was given the marker"

  # S3/S4: the scratch that results may not contain the repo or HOME, since the
  # sweep would then reach into them. Refused with 2 before anything is swept: the
  # old entry beside is in a marked scratch, which is what would be swept otherwise.
  mk_tree "rooted/memory-industry-mutants/repo"
  fresh
  mk_entry "$tmp/rooted/memory-industry-mutants/otro" '7 hours ago'
  mk_marker "$tmp/rooted/memory-industry-mutants" '7 hours ago'
  in_tree "rooted/memory-industry-mutants/repo" MUTANTS_SCRATCH_DIR="$tmp/rooted" -- --prepare-scratch 2
  if (( rc != 2 )) || grep -q '^jobs=' "$tmp/run.out" || [[ ! -d "$tmp/rooted/memory-industry-mutants/otro" ]]; then
    self_fail "S3: a scratch that contains the repo did not exit 2 before sweeping (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  fresh
  mkdir -p "$tmp/hrooted/memory-industry-mutants/home"
  mk_entry "$tmp/hrooted/memory-industry-mutants/otro" '7 hours ago'
  mk_marker "$tmp/hrooted/memory-industry-mutants" '7 hours ago'
  in_tree clean HOME="$tmp/hrooted/memory-industry-mutants/home" MUTANTS_SCRATCH_DIR="$tmp/hrooted" -- --prepare-scratch 2
  if (( rc != 2 )) || grep -q '^jobs=' "$tmp/run.out" || [[ ! -d "$tmp/hrooted/memory-industry-mutants/otro" ]]; then
    self_fail "S4: a scratch that contains HOME did not exit 2 before sweeping (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi

  # L1: in the system temp, only what proves to be the judge's, and only when old.
  fresh
  mk_entry "$sys/cargo-mutants-rust-a.tmp" '7 hours ago'
  printf 'name = "memory-industry"\n' > "$sys/cargo-mutants-rust-a.tmp/Cargo.toml"
  touch -d '7 hours ago' "$sys/cargo-mutants-rust-a.tmp"
  mk_entry "$sys/cargo-mutants-rust-b.tmp" '7 hours ago'
  printf 'name = "somebody-else"\n' > "$sys/cargo-mutants-rust-b.tmp/Cargo.toml"
  touch -d '7 hours ago' "$sys/cargo-mutants-rust-b.tmp"
  mk_entry "$sys/cargo-mutants-rust-c.tmp" '7 hours ago'
  mk_entry "$sys/cuba-attrs-test-0a1b2c3d" '7 hours ago'
  mk_entry "$sys/cuba-attrs-fresh" '1 hour ago'
  mkdir -p "$sys/tmp.AAAA/mutants.out"
  touch -d '7 hours ago' "$sys/tmp.AAAA"
  mk_entry "$sys/tmp.BBBB" '7 hours ago'
  mk_entry "$sys/memory-industry-other" '7 hours ago'
  in_tree clean -- --prepare-scratch 2
  (( rc == 0 )) || self_fail "L1: --prepare-scratch over a system temp with old entries exited $rc: $(cat "$tmp/run.out" "$tmp/run.err")"
  for gone in cargo-mutants-rust-a.tmp cuba-attrs-test-0a1b2c3d tmp.AAAA; do
    [[ ! -e "$sys/$gone" ]] || self_fail "L1: $gone, of 7 h and provably the judge's, was not swept"
  done
  for stays in cargo-mutants-rust-b.tmp cargo-mutants-rust-c.tmp cuba-attrs-fresh tmp.BBBB memory-industry-other; do
    [[ -e "$sys/$stays" ]] || self_fail "L1: $stays was swept, and nothing proves it is the judge's or old enough"
  done
  run_said "not provably the gate's, left alone: cargo-mutants-rust-c.tmp" || self_fail "L1: a half-made copy that was left alone was not listed: $(cat "$tmp/run.err")"

  # K1: another gate is running. Its copies are in use: nothing is swept, and the
  # disk is still measured.
  fresh
  mk_entry "$scr/cargo-mutants-rust-old.tmp" '7 hours ago'
  mk_marker "$scr" '7 hours ago'
  sleeper=""
  trap 'kill "$sleeper" 2>/dev/null || true; rm -rf "$tmp"' EXIT
  sleep 300 &
  sleeper=$!
  mkdir -p "$home/.cache/cuba-gate/lock"
  # The record is the one gate-lock.sh writes, so it is made by its own function. The
  # path goes through a variable: local_gate_contract.rs looks for the first line that
  # sources gate-lock.sh to know where the gate's environment is set up, and it has
  # to find the gate's, not this one.
  lock_lib="$ROOT/scripts/gate-lock.sh"
  # shellcheck source=scripts/gate-lock.sh
  (source "$lock_lib"; owner_record "$sleeper" k1) > "$home/.cache/cuba-gate/lock/owner"
  in_tree clean -- --prepare-scratch 2
  kill "$sleeper" 2>/dev/null || true
  (( rc == 0 )) || self_fail "K1: --prepare-scratch beside a live gate exited $rc: $(cat "$tmp/run.out" "$tmp/run.err")"
  [[ -d "$scr/cargo-mutants-rust-old.tmp" ]] || self_fail "K1: an entry was swept while another gate held the lock"
  if ! run_said 'another gate is running' || ! run_said 'sweep skipped' || ! grep -q '^jobs=' "$tmp/run.out"; then
    self_fail "K1: with another gate running it did not say 'another gate is running' and 'sweep skipped', or stopped measuring the disk: $(cat "$tmp/run.out" "$tmp/run.err")"
  fi

  # K2: under merge-gate.sh the lock is the PARENT's, and the sweep is its own
  # business. This shell is the parent: the script is its direct child, not a
  # child of a $( ) subshell, whose pid would not be the one the record names.
  fresh
  mk_entry "$scr/cargo-mutants-rust-old.tmp" '7 hours ago'
  mk_marker "$scr" '7 hours ago'
  mkdir -p "$home/.cache/cuba-gate/lock"
  # shellcheck source=scripts/gate-lock.sh
  holder="$(source "$lock_lib"; owner_record "$$" k2)"
  printf '%s\n' "$holder" > "$home/.cache/cuba-gate/lock/owner"
  in_tree clean CUBA_GATE_LOCK_OWNER="$holder" -- --prepare-scratch 2
  (( rc == 0 )) || self_fail "K2: --prepare-scratch under the gate that holds the lock exited $rc: $(cat "$tmp/run.out" "$tmp/run.err")"
  [[ ! -e "$scr/cargo-mutants-rust-old.tmp" ]] || self_fail "K2: the parent that holds the lock did not sweep: $(cat "$tmp/run.err")"

  # Q1..Q5: the free space decides, and it is read from the scratch's own volume.
  fresh
  avail=$((12 * kb))
  in_tree clean -- --prepare-scratch 2
  if (( rc != 1 )) || ! run_said '12.0 GB' || ! run_said '18.0 GB' || ! run_said 'memory-industry-mutants'; then
    self_fail "Q1: 12 GB free for a 10 GB copy and 8 GB reserve was not a refusal with both numbers and the scratch path (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  avail=$((500 * kb))
  fresh
  in_tree clean FAKE_DF_FAIL=1 -- --prepare-scratch 2
  if (( rc != 2 )) || ! run_said 'could not read the free space'; then
    self_fail "Q2: a df that prints nothing was not exit 2 'could not read the free space' (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  # The size per copy that ships is the one measured on 2026-10-13 (A3).
  fresh
  per_copy=""
  in_tree clean -- --prepare-scratch 2
  per_copy=10
  if (( rc != 0 )) || ! grep -qE 'disk:.*12\.0 GB per copy' "$tmp/run.err"; then
    self_fail "Q3: without MUTANTS_SCRATCH_GB_PER_COPY the disk line did not say '12.0 GB per copy' (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  fresh
  in_tree clean MUTANTS_SCRATCH_DIR="$tmp/otro" -- --prepare-scratch 2
  got="$(sed -n 's/^scratch=//p' "$tmp/run.out")"
  if (( rc != 0 )) || [[ "$(norm "$got")" != "$(norm "$tmp/otro/memory-industry-mutants")" ]] || [[ ! -f "$tmp/otro/memory-industry-mutants/$marker" ]]; then
    self_fail "Q4: MUTANTS_SCRATCH_DIR=X did not make X/memory-industry-mutants, with its marker, the scratch (scratch='$got', exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  # The reserve is CUBA_GATE_MIN_FREE_GB, the variable run-all-tests.sh reads:
  # 29 GB fits one 10 GB copy over the default 8 GB reserve, and not over 20 GB.
  fresh
  avail=$((29 * kb))
  in_tree clean CUBA_GATE_MIN_FREE_GB=20 -- --prepare-scratch 1
  avail=$((500 * kb))
  if (( rc != 1 )) || ! run_said '20.0 GB reserve'; then
    self_fail "Q5: CUBA_GATE_MIN_FREE_GB=20 did not become the reserve (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi

  # A4: a cargo target inside rust/ other than target/ is copied whole by
  # cargo-mutants (it excludes only its own target). On 2026-10-13 four of them
  # made 25.8 GB of every copy. Instant to see, since cargo writes a
  # CACHEDIR.TAG in each; du would take minutes on them.
  mk_tree tainted
  mkdir -p "$tmp/tainted/rust/target-x"
  : > "$tmp/tainted/rust/target-x/CACHEDIR.TAG"
  fresh
  in_tree tainted -- --prepare-scratch 2
  if (( rc != 2 )) || grep -q '^jobs=' "$tmp/run.out"; then
    self_fail "A4: a rust/target-x holding CACHEDIR.TAG did not stop the step with exit 2 and no jobs= (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  for want in 'target-x' 'cargo-mutants copies' '25.8 GB' 'CARGO_TARGET_DIR'; do
    run_said "$want" || self_fail "A4: the refusal did not say '$want': $(cat "$tmp/run.err")"
  done
  # The step itself stops before cargo mutants, not only the mode.
  fresh
  in_tree tainted MUTANTS_JOBS=2 --
  if (( rc == 0 )) || ran_mutants || ! run_said 'target-x'; then
    self_fail "A4: a tree with a target inside rust/ still reached cargo mutants, or the step did not name it (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi
  # The presence anchor: rust/target is cargo-mutants' own to exclude, and a
  # directory named like a target but without the tag is not one.
  mk_tree plain
  mkdir -p "$tmp/plain/rust/target" "$tmp/plain/rust/src" "$tmp/plain/rust/target-notes"
  : > "$tmp/plain/rust/target/CACHEDIR.TAG"
  fresh
  in_tree plain -- --prepare-scratch 2
  if (( rc != 0 )) || ! grep -qx 'jobs=2' "$tmp/run.out"; then
    self_fail "A4: rust/target holding its own CACHEDIR.TAG, and a target-notes without one, were taken for a target to refuse (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err")"
  fi

  # M1/M2: the step itself. Out of disk, cargo mutants is never run.
  fresh
  avail=$((12 * kb))
  in_tree clean MUTANTS_JOBS=2 --
  avail=$((500 * kb))
  if (( rc == 0 )) || ran_mutants || ! run_said '12.0 GB'; then
    self_fail "M1: with 12 GB free the step ran cargo mutants, or did not name the 12.0 GB it had (exit $rc): $(cat "$tmp/run.out" "$tmp/run.err" "$tmp/cargo.calls" 2>&1)"
  fi
  # With room, an explicit MUTANTS_JOBS goes through, and cargo mutants gets the
  # judge's scratch as its temp dir in the three spellings (TMP and TEMP are what
  # Windows reads, TMPDIR what Unix does) and debug info off (D4: 7.6 of the
  # 14.1 GB of a copy's target were .pdb, and with it off the copy was 37 % smaller).
  fresh
  in_tree clean MUTANTS_JOBS=9 --
  ran_mutants || self_fail "M2: with 500 GB free the step did not run cargo mutants: $(cat "$tmp/run.out" "$tmp/run.err")"
  # The fields are tab-separated and some may be empty, which `read` would collapse.
  call_field() { grep -m1 '^mutants --file' "$tmp/cargo.calls" | awk -F'\t' -v n="$1" '{print $n}'; }
  argv="$(call_field 1)" t_tmp="$(call_field 2)" t_temp="$(call_field 3)" t_tmpdir="$(call_field 4)" t_debug="$(call_field 5)"
  [[ "$argv" == *' --jobs 9 '* ]] || self_fail "M2: MUTANTS_JOBS=9 did not reach cargo mutants as --jobs 9 ($argv)"
  if [[ "$(norm "$t_tmp")" != "$(norm "$t_tmpdir")" || "$(norm "$t_temp")" != "$(norm "$t_tmpdir")" || "$t_tmpdir" != *memory-industry-mutants ]]; then
    self_fail "M2: cargo mutants did not get TMP, TEMP and TMPDIR on the judge's scratch (TMP=$t_tmp TEMP=$t_temp TMPDIR=$t_tmpdir)"
  fi
  # On Linux gate_linux_env may move TMPDIR off a RAM-backed /tmp first, so the
  # exact place is only held where nothing moves it.
  if [[ "$(uname -s)" != Linux && "$(norm "$t_tmpdir")" != "$(norm "$scr")" ]]; then
    self_fail "M2: the scratch cargo mutants got was $t_tmpdir, not $scr"
  fi
  [[ "$t_debug" == 0 ]] || self_fail "M2: cargo mutants did not get CARGO_PROFILE_DEV_DEBUG=0 (got '$t_debug')"

  # D9: an out-of-disk build is the machine, not a mutant that fails to compile.
  # `error: could not write output to ...: (os error 112)` starts like a rustc
  # diagnostic, and used to be taken for one.
  fixture nospace112 1 '{"Failure":101}:nospace112'
  judge nospace112
  (( rc != 0 )) || self_fail "N1: a build that died of os error 112 was accepted as an unviable mutant"
  for want in 'stopped 1 of 1 unviable' 'os error 112' 'disk'; do
    grep -qF -- "$want" "$tmp/err" || self_fail "N1: the refusal did not say '$want': $(cat "$tmp/err")"
  done
  fixture nospace28 1 '{"Failure":101}:nospace28'
  judge nospace28
  (( rc != 0 )) || self_fail "N2: a build that died of os error 28 was accepted as an unviable mutant"
  grep -qE 'os error (28|112/28)' "$tmp/err" || self_fail "N2: the refusal did not name os error 28: $(cat "$tmp/err")"
  grep -qF 'ran out of disk' "$tmp/err" || self_fail "N2: the refusal did not say the build ran out of disk: $(cat "$tmp/err")"
  # Only the out-of-disk build is the machine's: the genuine one beside it is not counted.
  fixture nospace_mixed 2 '{"Failure":101}:e0308' '{"Failure":101}:nospace112'
  judge nospace_mixed
  if (( rc == 0 )) || ! grep -qF 'stopped 1 of 2 unviable' "$tmp/err" || ! grep -qF '1 of them ran out of disk' "$tmp/err"; then
    self_fail "N4: one out-of-disk build beside one compile error was not '1 of 2' and '1 of them ran out of disk' (exit $rc): $(cat "$tmp/err")"
  fi

  echo "OK  self-test: a build rustc refused with a diagnostic stays unviable; an NTSTATUS,"
  echo "    a signal, a timeout, a 101 whose build phase holds no rustc diagnostic, a missing log,"
  echo "    a build out of disk and a file whose total disagrees with its records are refused; a caught mutant"
  echo "    counts only when its test run exited 1..255, or overflowed its stack; the mutation step"
  echo "    plans its jobs from the free disk before copying, refuses when not even one fits, reads"
  echo "    an unreadable free space as an error, sweeps only what is the judge's and old, leaves a"
  echo "    running gate's copies alone, refuses a cargo target inside rust/, and gives cargo mutants"
  echo "    its own scratch directory: always MUTANTS_SCRATCH_DIR's subdirectory memory-industry-mutants,"
  echo "    swept only when it carries the marker, and refused if it holds the repo or HOME"
  exit 0
}
# --- self-test end ---

case "${1:-}" in
# --- self-test begin ---
  "--self-test") self_test ;;
# --- self-test end ---
  "--check-builds")
    [[ -n "${2:-}" && -f "$2" ]] || { echo "FAIL: --check-builds needs an outcomes.json (got '${2:-}')" >&2; exit 1; }
    check_builds "$2"
    exit $?
    ;;
  "--disk-plan")
    mutants_disk_plan "${2:-}" "${3:-}" "${4:-}" "${5:-}" "${6:-}"
    exit $?
    ;;
  "--refuse-targets")
    mutants_refuse_targets_in_tree || exit $?
    mutants_refuse_target_dir_in_tree || exit $?
    exit 0
    ;;
  "--prepare-scratch")
    mutants_prepare_scratch "${2:-}" || exit $?
    printf 'jobs=%s\nscratch=%s\n' "$MUTANTS_PLANNED_JOBS" "$MUTANTS_SCRATCH"
    exit 0
    ;;
  "") ;;
  *) echo "FAIL: unknown argument '$1'" >&2; exit 2 ;;
esac

# Where each mutant's scratch copy lands is TMPDIR, so it has to be a disk and not
# tmpfs (RAM here) before the output directory below is named: gate-lock.sh says
# why. The modes above judge a file and need neither.
# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"
gate_linux_env

cd "$ROOT/rust"

if ! command -v cargo-mutants >/dev/null 2>&1 && ! cargo mutants --version >/dev/null 2>&1; then
  echo "FAIL: cargo-mutants is required. Install: cargo install cargo-mutants" >&2
  exit 1
fi

MUTANTS_MIN_KILL="${MUTANTS_MIN_KILL:-0.7}"
OUT_DIR="${TMPDIR:-/tmp}/memory-industry-mutants-out"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"


# Parallelism derived from the machine, not pinned to a number.
#
# The values these replace were measured on a 14.9 GB laptop, and a constant
# calibrated on one machine and then applied to every other one is exactly the
# defect this release spent its time removing from the GPU path. The formula
# below reproduces the old hand-tuned figure on the machine it was tuned for
# (14.9 GB / 4 = 3) and scales from there.
machine_cores() { nproc 2>/dev/null || echo 4; }

machine_ram_gb() {
  if [[ -r /proc/meminfo ]]; then
    awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo
    return
  fi
  powershell.exe -NoProfile -Command \
    '[int]((Get-CimInstance Win32_OperatingSystem).TotalVisibleMemorySize/1MB)' 2>/dev/null |
    tr -d '\r' | grep -E '^[0-9]+$' || echo 8
}
# One mutant is a debug --lib build plus its tests: far lighter than a release
# link, so this leans on cores. Capped at 6 because each run carries a 90 s
# test timeout, and a saturated machine turns a caught mutant into a reported
# TIMEOUT, which is a worse answer than a slower one.
default_mutants_jobs() {
  local half cap
  half=$(( $(machine_cores) / 2 ))
  cap=$(( $(machine_ram_gb) / 4 ))
  (( half < 1 )) && half=1
  (( cap < half )) && half=$cap
  (( half > 6 )) && half=6
  (( half < 1 )) && half=1
  echo "$half"
}
MUTANTS_JOBS="${MUTANTS_JOBS:-$(default_mutants_jobs)}"
echo "mutants jobs=$MUTANTS_JOBS (cores=$(machine_cores) ram=$(machine_ram_gb)GB)"

# The disk has the last word on the jobs, an explicit MUTANTS_JOBS included, and
# decides before anything is copied.
mutants_prepare_scratch "$MUTANTS_JOBS" || exit $?
MUTANTS_JOBS="$MUTANTS_PLANNED_JOBS"
SCRATCH_NATIVE="$(cygpath -w "$MUTANTS_SCRATCH" 2>/dev/null || printf '%s' "$MUTANTS_SCRATCH")"

echo "=== cargo mutants (search cores with dense unit tests) ==="
# judge.rs / tools.rs / full search/* generate 655 mutants (~12h) and miss
# almost every change in untested branches (LLM resolve, ONNX decode fallback).
# The merge judge still requires a real kill-rate check. Scope = mmr/rrf/cache.
# --lib search:: keeps the unmutated baseline off contract tests that need
# files outside rust/, and keeps each mutant's cargo test under a second.
set +e
# Each scratch copy must build into its own target/, so unset whatever points
# at a shared one. quality-gate.sh has done this since it was written, and its
# comment names the exact symptom: leftover mutant artifacts, rrf.rs among
# them, make a later unmutated build fail tests that pass on a clean target.
# This script never did it, so running inside the SIL it left its scratch
# artifacts in the SIL's target and the *next* run started poisoned - four
# tests red in rrf.rs and eval/datasets.rs, on sources nobody had touched.
unset CARGO_TARGET_DIR

# --timeout is 180, not 90. That budget was set when this ran two at a time;
# at six, a slow test reported TIMEOUT instead of the CAUGHT it actually was,
# and a timeout counts against the kill rate as though the mutant had lived.
#
# The comment lives here and not inside the invocation below: a comment line
# between two backslash continuations ends the command, and the shell then
# tries to run `--timeout` as a program. That is rc=127, and it is how this
# script failed the gate once already.
#
# The scratch is the judge's own directory, in the three spellings of the temp
# dir: TMP and TEMP are what cargo-mutants reads on Windows (TMPDIR moves
# nothing there), TMPDIR what it reads on Unix. Debug info is off for this
# command only: on 2026-10-13 7.6 of the 14.1 GB of a copy's target were .pdb,
# and with it off the copy was 8.94 GB, 37 % smaller. Prefix assignments, so
# nothing else in the gate sees them.
TMP="$SCRATCH_NATIVE" TEMP="$SCRATCH_NATIVE" TMPDIR="$MUTANTS_SCRATCH" CARGO_PROFILE_DEV_DEBUG=0 \
cargo mutants \
  --file 'src/search/mmr.rs' \
  --file 'src/search/rrf.rs' \
  --file 'src/search/cache.rs' \
  --timeout 180 \
  --jobs "$MUTANTS_JOBS" \
  --output "$OUT_DIR" \
  -- --lib search::
mutants_rc=$?
set -e

MUTANTS_JSON="$OUT_DIR/mutants.out/outcomes.json"
if [[ ! -f "$MUTANTS_JSON" ]]; then
  echo "FAIL: cargo mutants produced no outcomes.json (rc=$mutants_rc)" >&2
  exit 1
fi

check_builds "$MUTANTS_JSON"

MUTANTS_MIN_KILL="$MUTANTS_MIN_KILL" "$PY" - "$MUTANTS_JSON" <<'PY'
import json, os, sys
path = sys.argv[1]
min_kill = float(os.environ.get("MUTANTS_MIN_KILL", "0.7"))
data = json.load(open(path, encoding="utf-8"))
caught = int(data.get("caught") or 0)
missed = int(data.get("missed") or 0)
timeout = int(data.get("timeout") or 0)
unviable = int(data.get("unviable") or 0)
denom = caught + missed + timeout
if denom == 0:
    print("FAIL: zero scored mutants in outcomes.json", file=sys.stderr)
    print(json.dumps(list(data.keys()), indent=2), file=sys.stderr)
    sys.exit(1)
rate2 = caught / denom
print(f"mutants caught={caught} missed={missed} timeout={timeout} unviable={unviable} kill_rate={rate2:.3f} min={min_kill}")
if rate2 < min_kill:
    print(f"FAIL: kill rate {rate2:.3f} < {min_kill}", file=sys.stderr)
    sys.exit(1)
print("OK  mutants kill rate")
PY
