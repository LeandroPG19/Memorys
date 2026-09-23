#!/usr/bin/env bash
# Mutation testing on MemoryIndustry cores. Unkilled mutants = FAIL.
#   ./scripts/mutants-gate.sh                       the run, inside the SIL
#   ./scripts/mutants-gate.sh --check-builds FILE   judge the unviable builds of an
#                                                   outcomes.json (quality-gate.sh)
#   ./scripts/mutants-gate.sh --self-test           that judge against its fixtures
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
    found, in_build = [], False
    for line in (ANSI.sub("", raw) for raw in lines):
        if line.startswith("*** ") and " test --no-run" in line:
            in_build = True
        elif in_build and line.startswith("*** result:"):
            break
        elif in_build and DIAGNOSTIC.match(line) and not line.startswith(NOT_THE_COMPILER):
            found.append(line)
    return found

def mutant_name(outcome):
    scenario = outcome.get("scenario")
    if isinstance(scenario, dict) and isinstance(scenario.get("Mutant"), dict):
        return scenario["Mutant"].get("name", "?")
    return json.dumps(scenario)

unviable = [o for o in outcomes if o.get("summary") == "Unviable"]
declared = int(data.get("unviable") or 0)
if len(unviable) != declared:
    print(f"FAIL: {path} declares unviable={declared} and holds {len(unviable)} Unviable record(s).", file=sys.stderr)
    print("      The file changed shape, and a check that finds no records judges nothing.", file=sys.stderr)
    sys.exit(1)

killed = []
for outcome in unviable:
    build = next((p for p in outcome.get("phase_results") or [] if p.get("phase") == "Build"), None)
    status = build.get("process_status") if build else "no Build phase recorded"
    why = exit_status(status)
    if why is None:
        diagnostics = build_diagnostics(outcome)
        if diagnostics is None:
            why = f"{json.dumps(status)}, no readable log at {outcome.get('log_path')!r}"
        elif not diagnostics:
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
    sys.exit(1)
print(f"OK  unviable builds: all {len(unviable)} refused by the compiler with a diagnostic, none by the machine")
PY
}

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

  # write_log FILE KIND: the build phase of a real log, ending one of five ways.
  #   e0308    rustc refuses the mutant with an error code
  #   plain    rustc refuses it with a plain `error: ` (a let-chain `||`)
  #   silent   the 2026-09-23 shape: cargo's summary and no diagnostic, plus an
  #            error line after `*** result:`, where it proves nothing
  #   outside  silent, with an error line in the mutation diff instead, before
  #            the build phase starts
  #   linker   the only error is link.exe dying of 0xc0000142: three of the
  #            seven 101s of 2026-09-23 read exactly like this
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
  fixture() {
    local dir="$tmp/$1" declared="$2" records spec status log n=0
    shift 2
    mkdir -p "$dir/log"
    records='{"scenario":"Baseline","summary":"Success","log_path":"log/baseline.log","phase_results":[{"phase":"Build","process_status":"Success"},{"phase":"Test","process_status":"Success"}]}'
    records+=',{"scenario":{"Mutant":{"name":"src/search/rrf.rs:9:9: replace f -> u32 with 0"}},"summary":"CaughtMutant","log_path":"log/caught.log","phase_results":[{"phase":"Build","process_status":"Success"},{"phase":"Test","process_status":{"Failure":101}}]}'
    for spec in "$@"; do
      n=$((n + 1))
      status="${spec%:*}"
      log="${spec##*:}"
      [[ "$log" == none ]] || write_log "$dir/log/m$n.log" "$log"
      records+=",{\"scenario\":{\"Mutant\":{\"name\":\"src/search/cache.rs:$n:1: replace g with h\"}},\"summary\":\"Unviable\",\"log_path\":\"log/m$n.log\",\"phase_results\":[{\"phase\":\"Build\",\"process_status\":$status}]}"
    done
    printf '{"outcomes":[%s],"total_mutants":%d,"caught":1,"missed":0,"timeout":0,"unviable":%d,"success":0,"cargo_mutants_version":"27.1.0"}\n' \
      "$records" $(( $# + 1 )) "$declared" > "$dir/outcomes.json"
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

  echo "OK  self-test: a build rustc refused with a diagnostic stays unviable; an NTSTATUS,"
  echo "    a signal, a timeout, a 101 whose build phase holds no rustc diagnostic, a missing log"
  echo "    and a file whose total disagrees with its records are refused"
  exit 0
}

case "${1:-}" in
  "--self-test") self_test ;;
  "--check-builds")
    [[ -n "${2:-}" && -f "$2" ]] || { echo "FAIL: --check-builds needs an outcomes.json (got '${2:-}')" >&2; exit 1; }
    check_builds "$2"
    exit $?
    ;;
  "") ;;
  *) echo "FAIL: unknown argument '$1'" >&2; exit 2 ;;
esac

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
