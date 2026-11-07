#!/usr/bin/env bash
# Coverage floor for MemoryIndustry cores (CRAP proxy until per-fn CRAP tables land).
# Missing cargo-llvm-cov = FAIL (no soft-skip).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- self-test: the floor is judged as before, and a server nobody marked is refused first ---
# Each row copies this script into a tree of its own, with a cargo that logs its arguments
# and writes the llvm-cov JSON it is asked for (50 % of lines), and a psql that answers the
# identity question the judge reads. A DATABASE_URL to a server with no judge marker is exit 2
# before cargo; with the judge's own marked server, or with no DATABASE_URL, it goes on.
#   ./scripts/crap-gate.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  # CI-J3: the judge hands its children GATE_SERVER_SYSTEM_ID, the identity of its real server. The
  # rows here answer with their own identity, so this self-test starts without the judge's.
  unset GATE_SERVER_SYSTEM_ID
  # D-J7b (A2): this self-test is hermetic. HOME and XDG_CACHE_HOME are a directory of its own,
  # removed on exit, so a row that does not give them one never reaches the real cache.
  hermetic="$(mktemp -d)"
  mkdir -p "$hermetic/home" "$hermetic/cache"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp" "$hermetic"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }
  mkdir -p "$tmp/t/scripts" "$tmp/t/rust" "$tmp/bin"
  cp "$ROOT/scripts/crap-gate.sh" "$ROOT/scripts/gate-lock.sh" "$tmp/t/scripts/"
  cat >"$tmp/bin/cargo" <<'STUB'
#!/bin/sh
echo "$*" >>"$FAKE_CALLS"
out=""; prev=""
for a in "$@"; do [ "$prev" = --output-path ] && out="$a"; prev="$a"; done
[ -z "$out" ] || printf '%s\n' '{"data":[{"totals":{"lines":{"percent":50}}}]}' >"$out"
exit 0
STUB
  cat >"$tmp/bin/psql" <<'STUB'
#!/bin/sh
printf '%s %s\n' "$*" "${PGOPTIONS:-}" >>"$FAKE_PSQL_CALLS"
printf '%s|%s\n' "${FAKE_PSQL_CLUSTER_NAME-memory-industry-gate}" "${FAKE_PSQL_SYSID-7701731310526959658}"
STUB
  chmod +x "$tmp/bin/cargo" "$tmp/bin/psql"

  # run_crap NAME [VAR=value...]: the copy of the script, in the fixture's environment;
  # its rc in $rc and its output in $tmp/NAME.out.
  run_crap() {
    local name="$1"
    shift
    rc=0
    env -u DATABASE_URL -u FAKE_PSQL_CLUSTER_NAME -u FAKE_PSQL_SYSID "$@" \
      PATH="$tmp/bin:$PATH" FAKE_CALLS="$tmp/$name.calls" FAKE_PSQL_CALLS="$tmp/$name.psql" \
      TMPDIR="$tmp" bash "$tmp/t/scripts/crap-gate.sh" >"$tmp/$name.out" 2>&1 || rc=$?
  }

  # J1.9 (D-J1d): a DATABASE_URL to a server nobody marked is exit 2 before cargo, names the
  # marker it read, and does not print the password.
  run_crap unmarked DATABASE_URL='postgresql://cuba:S3cr3tW2@127.0.0.1:5488/brain' FAKE_PSQL_CLUSTER_NAME=
  (( rc == 2 )) || self_fail "J1.9: crap-gate with a DATABASE_URL to a server with no judge marker exited $rc, not 2: $(cat "$tmp/unmarked.out")"
  [[ ! -e "$tmp/unmarked.calls" ]] || self_fail "J1.9: crap-gate called cargo before judging the server: $(cat "$tmp/unmarked.calls")"
  grep -q 'cluster_name' "$tmp/unmarked.out" || self_fail "J1.9: the refusal does not name cluster_name: $(cat "$tmp/unmarked.out")"
  if grep -q 'S3cr3tW2' "$tmp/unmarked.out"; then self_fail "J1.9: the refusal carries the password of DATABASE_URL: $(cat "$tmp/unmarked.out")"; fi

  # The judge's own marked server goes on to the floor, asked read-only.
  run_crap marked DATABASE_URL='postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_PSQL_CLUSTER_NAME=memory-industry-gate
  (( rc == 0 )) || self_fail "J1.9: crap-gate on the judge's marked server exited $rc, not 0: $(cat "$tmp/marked.out")"
  grep -q -- '--lib' "$tmp/marked.calls" 2>/dev/null || self_fail "J1.9: crap-gate did not run cargo llvm-cov on the marked server: $(cat "$tmp/marked.out")"
  grep -q 'default_transaction_read_only=on' "$tmp/marked.psql" 2>/dev/null \
    || self_fail "J1.9: the marked server was not asked in read-only mode: $(cat "$tmp/marked.psql" 2>/dev/null || echo 'psql was not called')"

  # With no DATABASE_URL the floor is judged as it always was, and nothing asks a server.
  run_crap none
  (( rc == 0 )) || self_fail "J1.9: crap-gate with no DATABASE_URL exited $rc, not 0: $(cat "$tmp/none.out")"
  grep -q -- '--lib' "$tmp/none.calls" 2>/dev/null || self_fail "J1.9: crap-gate with no DATABASE_URL did not run cargo llvm-cov: $(cat "$tmp/none.out")"
  [[ ! -e "$tmp/none.psql" ]] || self_fail "J1.9: crap-gate with no DATABASE_URL asked a server: $(cat "$tmp/none.psql")"

  # J1.10 (A2, D-J1c): the server this step judges is the one merge-gate.sh read. The identity
  # merge-gate exported is 7701731310526959658; a server that now answers 999 is exit 2 before
  # cargo, and the refusal names system_identifier.
  run_crap sysid DATABASE_URL='postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_PSQL_CLUSTER_NAME=memory-industry-gate FAKE_PSQL_SYSID=999 GATE_SERVER_SYSTEM_ID=7701731310526959658
  (( rc == 2 )) || self_fail "J1.10: crap-gate over a server whose system_identifier changed exited $rc, not 2: $(cat "$tmp/sysid.out")"
  [[ ! -e "$tmp/sysid.calls" ]] || self_fail "J1.10: crap-gate called cargo after the server changed: $(cat "$tmp/sysid.calls")"
  grep -q 'system_identifier' "$tmp/sysid.out" || self_fail "J1.10: the refusal does not name system_identifier: $(cat "$tmp/sysid.out")"

  # P4 (D-J1a, D-J1e, seed 20261106, printed on a FAIL): gate_require_database_url over 400
  # generated URLs, half from the shape of a URL and half from a random tail, all from the
  # alphabet «ab:@/?%[]#9.». Every URL it accepts (exit 0, nothing printed) keeps what the gate
  # derives brain_gate from: no '?', at most one '@' before the host, one non-empty database
  # segment after the host, and the maintenance URL ${url%/*} on the same authority. The property
  # is not vacuous: the 400 must include accepted and refused URLs.
  p4_seed=20261106
  RANDOM=$p4_seed
  p4_alpha='ab:@/?%[]#9.'
  # The chunk is set by name, not echoed: a $(...) would draw RANDOM in a subshell, and every
  # chunk of one URL would be the same.
  p4_chunk() {
    local s="" i
    for ((i = RANDOM % 5; i > 0; i--)); do s+="${p4_alpha:RANDOM % ${#p4_alpha}:1}"; done
    printf -v "$1" '%s' "$s"
  }
  p4_accepted=0; p4_refused=0
  for ((p4_n = 0; p4_n < 400; p4_n++)); do
    p4_chunk p4_c1; p4_chunk p4_c2; p4_chunk p4_c3; p4_chunk p4_c4
    if (( p4_n % 2 )); then
      p4_url="postgresql://$p4_c1:$p4_c2@$p4_c3/$p4_c4"
    else
      p4_url="postgresql://$p4_c1$p4_c2$p4_c3"
    fi
    rc=0
    ( source "$ROOT/scripts/gate-lock.sh"; gate_require_database_url "$p4_url" ) >"$tmp/p4.out" 2>&1 || rc=$?
    if (( rc == 0 )) && [[ ! -s "$tmp/p4.out" ]]; then
      p4_accepted=$(( p4_accepted + 1 ))
      p4_rest="${p4_url#*://}"
      p4_auth="${p4_rest%%/*}"
      p4_db="${p4_rest#*/}"
      p4_base="${p4_url%/*}"
      if [[ "$p4_url" == *\?* || "${p4_auth//[^@]/}" == @@* || "$p4_rest" != */* \
            || -z "$p4_db" || "$p4_db" == */* || "${p4_base#*://}" != "$p4_auth" ]]; then
        self_fail "P4 seed $p4_seed, case $p4_n: the guard accepted a URL that breaks an invariant of the derived databases: '$p4_url'"
      fi
    elif (( rc == 2 )); then
      p4_refused=$(( p4_refused + 1 ))
    else
      self_fail "P4 seed $p4_seed, case $p4_n: gate_require_database_url exited $rc, not 0 or 2, for a generated URL: $(cat "$tmp/p4.out")"
    fi
  done
  (( p4_accepted > 0 && p4_refused > 0 )) \
    || self_fail "P4 seed $p4_seed: of 400 URLs $p4_accepted were accepted and $p4_refused refused; the property needs both to say anything"

  echo "OK  self-test: crap-gate refuses a DATABASE_URL to a server with no judge marker before cargo, and judges the floor as before"
  exit 0
fi
# --- self-test end ---

# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"

# The judge's server, when DATABASE_URL names one (D-J1d): asked before cargo, and a
# server with no judge marker is exit 2. With no DATABASE_URL the floor runs as it did.
if [[ -n "${DATABASE_URL:-}" ]]; then
  gate_require_judge_server "$DATABASE_URL"
fi

cd "$ROOT/rust"

MIN_COV="${CRAP_MIN_LINE_COV:-15}"

if ! command -v cargo-llvm-cov >/dev/null 2>&1 && ! cargo llvm-cov --version >/dev/null 2>&1; then
  echo "FAIL: cargo-llvm-cov is required for crap-gate." >&2
  echo "      cargo install cargo-llvm-cov && rustup component add llvm-tools-preview" >&2
  exit 1
fi

echo "=== crap-gate: llvm-cov --lib (min line ${MIN_COV}%) ==="
COV_JSON="$(mktemp "${TMPDIR:-/tmp}/memory-industry-cov.XXXXXX")"
trap 'rm -f "$COV_JSON"' EXIT
cargo llvm-cov --lib --json --summary-only --output-path "$COV_JSON"

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
MEMORY_INDUSTRY_COV_JSON="$COV_JSON" "$PY" - <<'PY'
import json, os, sys
min_cov = float(os.environ.get("CRAP_MIN_LINE_COV", "15"))
path = os.environ["MEMORY_INDUSTRY_COV_JSON"]
data = json.load(open(path, encoding="utf-8"))
# Prefer totals.lines.percent — a naive walk hits per-file zeros first.
try:
    pct = float(data["data"][0]["totals"]["lines"]["percent"])
except Exception:
    print("FAIL: could not parse llvm-cov JSON for totals.lines.percent", file=sys.stderr)
    sys.exit(1)
print(f"line_coverage={pct:.2f}% min={min_cov}")
if pct + 1e-9 < min_cov:
    print(f"FAIL: line coverage {pct:.2f}% < {min_cov}%", file=sys.stderr)
    sys.exit(1)
print("OK  crap-gate coverage floor")
PY