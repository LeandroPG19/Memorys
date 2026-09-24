#!/usr/bin/env bash
# Fail if a test is hidden from the local gate, if the ignore count moves
# without somebody saying so, or if unused crate deps are present.
# Missing tools are FAIL, never skip.
#
# The first guard used to be unable to fail. `run-all-tests.sh` discovers
# tests/*.rs by glob, so `has_discovery` was always 1, every file hit an empty
# `if` body and then `continue`d before reaching the counter, and `orphans`
# stayed 0 by construction. It printed OK over every commit for months.
#
# The glob is not where tests hide now. The exclusion lists are: a name in
# RUN_ELSEWHERE leaves the discovery loop and is only run again if some section
# names it, and a name in the CI workflow's MODEL_OR_CLI_ONLY is excluded there
# on the promise that the local gate covers it. Nothing checked either promise.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT/rust"
GATE="$ROOT/scripts/run-all-tests.sh"
CI="$ROOT/.github/workflows/ci.yml"

# Two-way ratchets. They fail when the number goes up *and* when it goes down
# without this line being updated, so removing an #[ignore] is a deliberate
# edit with a reviewer rather than slack somebody can spend later.
IGNORE_MAX=144
IGNORE_WITHOUT_REASON_MAX=142

# awk, not `sed -n '/start/,/end/p'`: sed looks for the range end starting at
# the line *after* the start, so a single-line RUN_ELSEWHERE=(a b) swallows the
# next line and invents names out of it. The list happens to span three lines
# today, which is the only reason that never showed.
deferred_names() {
  awk '/^RUN_ELSEWHERE=[(]/{f=1} f{print; if (/[)]/) exit}' "$1" |
    sed 's/RUN_ELSEWHERE=(//; s/)//' |
    tr -s ' 
' '
' | grep -v '^$' || true
}

ci_excluded_names() {
  sed -n 's/.*MODEL_OR_CLI_ONLY=(\(.*\)).*/\1/p' "$1" | tr ' ' '\n' | grep -v '^$' || true
}

# A name pulled out of the discovery loop has to be run by some other section,
# or it simply stopped running.
check_deferred_are_run_by_name() {
  local gate="$1" orphans=0 name
  while read -r name; do
    [[ -z "$name" ]] && continue
    if ! grep -qE -- "--test[[:space:]]+$name( |$|\")" "$gate"; then
      echo "FAIL: $name is deferred out of the discovery loop and no section runs it by name" >&2
      orphans=$((orphans + 1))
    fi
  done < <(deferred_names "$gate")
  echo "$orphans"
}

# "Excluded from GitHub Actions because the local gate covers it" is a claim,
# and this is the only thing that checks it.
check_ci_exclusions_are_covered_locally() {
  local ci="$1" gate="$2" uncovered=0 name
  while read -r name; do
    [[ -z "$name" ]] && continue
    if ! grep -q "$name" "$gate"; then
      echo "FAIL: ci.yml excludes $name and run-all-tests.sh never mentions it — that test runs nowhere" >&2
      uncovered=$((uncovered + 1))
    fi
  done < <(ci_excluded_names "$ci")
  echo "$uncovered"
}

# The second exclusion of ci.yml is not a list: a test file whose plain tests
# need a PostgreSQL server opens with this line, and the check job, which has
# no server, leaves out every file that does. Only the first line counts,
# because that is the only one ci.yml reads.
SERVER_MARKER='// needs-a-server:'

server_only_files() {
  local file
  for file in "$1"/*.rs; do
    [[ -f "$file" ]] || continue
    [[ "$(head -n 1 "$file" | tr -d '\r')" == "$SERVER_MARKER"* ]] && printf '%s\n' "$file"
  done
  return 0
}

# Those files carry no #[ignore], so no section of run-all-tests.sh names them:
# they run in its plain `cargo test` against the throwaway database, and that
# line is the whole of the promise. A marker with nothing after it is an
# exclusion nobody explained, and the date on the exclusion is read like every
# other exclusion date in this repo: past it, red. Which files have to carry the
# marker is rust/tests/ci_contract.rs's to judge, not this script's.
check_server_only_tests_run_locally() {
  local ci="$1" gate="$2" tests="$3" today="$4" uncovered=0 file reason expired
  local plain='^DATABASE_URL="\$GATE_DATABASE_URL" cargo test$'
  while read -r file; do
    [[ -z "$file" ]] && continue
    reason="$(head -n 1 "$file" | tr -d '\r')"
    reason="${reason#"$SERVER_MARKER"}"
    if [[ -z "${reason//[[:space:]]/}" ]]; then
      echo "FAIL: $file opens with '$SERVER_MARKER' and no reason after it" >&2
      uncovered=$((uncovered + 1))
    fi
    if ! grep -q "$plain" "$gate"; then
      echo "FAIL: ci.yml keeps $file out of the check job because run-all-tests.sh runs it in" >&2
      echo "      its plain cargo test against GATE_DATABASE_URL, and that line is gone" >&2
      uncovered=$((uncovered + 1))
    fi
  done < <(server_only_files "$tests")
  while read -r expired; do
    [[ -z "$expired" ]] && continue
    echo "FAIL: an exclusion in ci.yml expired on $expired. Renew it with a reason that is still true, or drop it" >&2
    uncovered=$((uncovered + 1))
  done < <(sed -nE 's/^[[:space:]]*#.*Expires: ([0-9]{4}-[0-9]{2}-[0-9]{2}).*/\1/p' "$ci" | awk -v t="$today" '$1 < t')
  echo "$uncovered"
}

# Anchored to the start of the line, so an attribute counts and a mention of
# one does not. The first version of this counted any occurrence of the text
# and went red on a doc comment that merely explained the attribute.
count_ignores() {
  grep -rnE '^[[:space:]]*#\[ignore' "$1" --include='*.rs' 2>/dev/null | wc -l | tr -d ' '
}

count_ignores_without_reason() {
  grep -rnE '^[[:space:]]*#\[ignore\]' "$1" --include='*.rs' 2>/dev/null | wc -l | tr -d ' '
}

# --- self-test: every guard above gets a fixture that puts it in the red -----
# Without this the guards are unfalsifiable again the moment somebody edits
# them, which is exactly how the original one died.
if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT

  printf 'RUN_ELSEWHERE=(orphan_fixture kept_fixture)\n--test kept_fixture\n' > "$tmp/gate.sh"
  got="$(check_deferred_are_run_by_name "$tmp/gate.sh" 2>/dev/null | tail -1)"
  [[ "$got" == "1" ]] || { echo "FAIL self-test: a deferred name with no section was not caught (got $got)" >&2; exit 1; }

  printf 'MODEL_OR_CLI_ONLY=(covered_fixture missing_fixture)\n' > "$tmp/ci.yml"
  printf -- '--test covered_fixture\n' > "$tmp/gate2.sh"
  got="$(check_ci_exclusions_are_covered_locally "$tmp/ci.yml" "$tmp/gate2.sh" 2>/dev/null | tail -1)"
  [[ "$got" == "1" ]] || { echo "FAIL self-test: a CI exclusion covered nowhere was not caught (got $got)" >&2; exit 1; }

  # The server marker: two marked files (one with CRLF, as a Windows checkout
  # leaves it), one whose marker sits below the first line and one without it,
  # first against a gate that runs them, then against one whose cargo test is
  # no longer plain, then the day after the exclusion's date, then with a
  # marker that gives no reason.
  mkdir -p "$tmp/tests"
  printf '%s\n' '// needs-a-server: creates its own database' 'mod common;' > "$tmp/tests/marked.rs"
  printf '%s\r\n' '// needs-a-server: creates its own database' 'mod common;' > "$tmp/tests/marked_crlf.rs"
  printf '%s\n' '//! a file' '// needs-a-server: too late for ci.yml to read' > "$tmp/tests/late.rs"
  printf '%s\n' '#[test]' 'fn plain() {}' > "$tmp/tests/plain.rs"
  printf '%s\n' '# Owner: x. Expires: 2026-09-22.' > "$tmp/ci3.yml"
  printf '%s\n' 'DATABASE_URL="$GATE_DATABASE_URL" cargo test' > "$tmp/gate3.sh"
  got="$(check_server_only_tests_run_locally "$tmp/ci3.yml" "$tmp/gate3.sh" "$tmp/tests" 2026-09-22 2>/dev/null | tail -1)"
  [[ "$got" == "0" ]] || { echo "FAIL self-test: marked files a plain cargo test runs were reported (got $got)" >&2; exit 1; }
  printf '%s\n' 'DATABASE_URL="$GATE_DATABASE_URL" cargo test --lib' > "$tmp/gate4.sh"
  got="$(check_server_only_tests_run_locally "$tmp/ci3.yml" "$tmp/gate4.sh" "$tmp/tests" 2026-09-22 2>/dev/null | tail -1)"
  [[ "$got" == "2" ]] || { echo "FAIL self-test: the two marked files with no plain cargo test left to run them were not both caught, or the one marked below the first line was (got $got)" >&2; exit 1; }
  got="$(check_server_only_tests_run_locally "$tmp/ci3.yml" "$tmp/gate3.sh" "$tmp/tests" 2026-09-23 2>/dev/null | tail -1)"
  [[ "$got" == "1" ]] || { echo "FAIL self-test: a CI exclusion past its date was not caught (got $got)" >&2; exit 1; }
  printf '%s\n' '// needs-a-server:   ' 'mod common;' > "$tmp/tests/unexplained.rs"
  got="$(check_server_only_tests_run_locally "$tmp/ci3.yml" "$tmp/gate3.sh" "$tmp/tests" 2026-09-22 2>/dev/null | tail -1)"
  [[ "$got" == "1" ]] || { echo "FAIL self-test: a server marker with no reason after it was not caught (got $got)" >&2; exit 1; }

  mkdir -p "$tmp/src"
  # The comment line is the fixture that matters: the first version of this
  # counted any occurrence of the text, so a doc comment that merely explained
  # the attribute pushed the count over its ceiling and failed the gate.
  {
    printf '/// this guard once miscounted a #[ignore] written in prose\n'
    printf '#[ignore]\n'
    printf '#[ignore = "why"]\n'
  } > "$tmp/src/a.rs"
  got="$(count_ignores "$tmp/src")"
  [[ "$got" == "2" ]] || { echo "FAIL self-test: the ignore count counts prose, not attributes (got $got)" >&2; exit 1; }
  got="$(count_ignores_without_reason "$tmp/src")"
  [[ "$got" == "1" ]] || { echo "FAIL self-test: the bare-ignore count is wrong (got $got)" >&2; exit 1; }

  echo "OK  self-test: every guard in this script failed against a fixture built to break it"
  exit 0
fi

echo "=== codigo-muerto: tests must not be hidden from the local gate ==="
for f in "$GATE" "$CI"; do
  [[ -f "$f" ]] || { echo "FAIL: missing $f" >&2; exit 1; }
done

orphans="$(check_deferred_are_run_by_name "$GATE" | tail -1)"
uncovered="$(check_ci_exclusions_are_covered_locally "$CI" "$GATE" | tail -1)"
server_only="$(check_server_only_tests_run_locally "$CI" "$GATE" "$RUST_DIR/tests" "$(date +%F)" | tail -1)"
if (( orphans > 0 || uncovered > 0 || server_only > 0 )); then
  echo "FAIL: $orphans deferred test(s), $uncovered CI-excluded test(s) and $server_only server-only" >&2
  echo "      exclusion problem(s): a test with no path in the local gate, or a cover" >&2
  exit 1
fi
echo "OK  every deferred and CI-excluded test file is run by name in run-all-tests.sh, and"
echo "    every server-only file ($(server_only_files "$RUST_DIR/tests" | xargs -r -n 1 basename | tr '\n' ' ')) runs in its plain cargo test"

echo "=== codigo-muerto: the ignore ceiling ==="
ignores="$(count_ignores "$RUST_DIR")"
bare="$(count_ignores_without_reason "$RUST_DIR")"
if [[ "$ignores" != "$IGNORE_MAX" ]]; then
  echo "FAIL: $ignores #[ignore] in rust/, the ceiling says $IGNORE_MAX." >&2
  echo "      Up: an ignored test proves nothing, so justify it. Down: good — set" >&2
  echo "      IGNORE_MAX=$ignores in this script so the ground you gained is kept." >&2
  exit 1
fi
if [[ "$bare" != "$IGNORE_WITHOUT_REASON_MAX" ]]; then
  echo "FAIL: $bare bare #[ignore] in rust/, the ceiling says $IGNORE_WITHOUT_REASON_MAX." >&2
  echo "      Prefer #[ignore = \"why\"]. If you added a reason to one, set" >&2
  echo "      IGNORE_WITHOUT_REASON_MAX=$bare here." >&2
  exit 1
fi
echo "OK  $ignores #[ignore] ($bare without a reason), both at their ratchet"

echo "=== codigo-muerto: cargo machete ==="
if ! command -v cargo-machete >/dev/null 2>&1 && ! cargo machete --version >/dev/null 2>&1; then
  echo "FAIL: cargo-machete is not installed. Install it (cargo install cargo-machete) — the gate does not skip dead-deps." >&2
  exit 1
fi
(cd "$RUST_DIR" && cargo machete)
echo "OK  cargo machete"
