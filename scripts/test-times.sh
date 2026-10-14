#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$ROOT/scripts/test-times.sh"

# --- self-test: the judge of the time of every test binary, against the logs it must refuse ---
# A ceiling the judge cannot break is a paragraph. Each row runs this script
# through its CLI (never as a function) over a log built the way cargo prints it
# on Windows (backslashes, .exe, CRLF) or on Linux, and a ceilings file built for
# the row. Every row passes --os, --today, --ceilings and --tests-dir, so no row
# depends on uname, on the date, on the real ceilings file or on rust/tests.
#   ./scripts/test-times.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  BOUND='=== end of the timed steps ==='
  TODAY=2026-10-14
  mkdir -p "$tmp/tests"
  for f in slow fast other_host gone_from_run; do : >"$tmp/tests/$f.rs"; done

  # emit OS LINE: one line of a log, with the CRLF a Windows tee leaves.
  emit() {
    if [[ "$1" == windows ]]; then printf '%s\r\n' "$2"; else printf '%s\n' "$2"; fi
  }
  # bin_block OS KEY SECONDS [ok|none]: what cargo prints for one binary. `none`
  # leaves out the `test result` line, as a log cut or interleaved does. The
  # unittests of the library and of the binary of one crate share a stem and
  # differ only in the source file and the hash.
  bin_block() {
    local os="$1" key="$2" secs="$3" result="${4:-ok}" sep='/' exe='' name head
    if [[ "$os" == windows ]]; then sep='\'; exe='.exe'; fi
    case "$key" in
      lib:*)
        name="${key#lib:}"
        head="     Running unittests src${sep}lib.rs (target${sep}debug${sep}deps${sep}${name}-0123456789abcdef${exe})" ;;
      bin:*)
        name="${key#bin:}"
        head="     Running unittests src${sep}main.rs (target${sep}debug${sep}deps${sep}${name}-fedcba9876543210${exe})" ;;
      doc:*)
        head="   Doc-tests ${key#doc:}" ;;
      *)
        head="     Running tests${sep}${key}.rs (target${sep}debug${sep}deps${sep}${key}-0123456789abcdef${exe})" ;;
    esac
    emit "$os" "$head"
    emit "$os" ""
    emit "$os" "running 1 test"
    emit "$os" "test t ... ok"
    emit "$os" ""
    if [[ "$result" != none ]]; then
      emit "$os" "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in ${secs}s"
      emit "$os" ""
    fi
  }
  # l1 OS [FAST_S]: the base log. slow runs in two steps (52.50 s), the lib and
  # the bin of memory_industry are two binaries, a stray result belongs to no
  # header, and what comes after the boundary is not timed. 74.59 s in 5 binaries.
  l1() {
    local os="$1" fast="${2:-0.31}"
    emit "$os" '=== cargo test (unit + smoke) ==='
    bin_block "$os" slow 40.00
    bin_block "$os" lib:memory_industry 21.51
    bin_block "$os" bin:memory_industry 0.00
    bin_block "$os" fast "$fast"
    emit "$os" "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 999.00s"
    emit "$os" '=== cargo clippy/test --features docs ==='
    bin_block "$os" slow 12.50
    bin_block "$os" doc:memory_industry 0.27
    emit "$os" "$BOUND"
    bin_block "$os" late 500.00
  }
  # simple_log OS KEY=SECONDS...: only those binaries, then the boundary.
  simple_log() {
    local os="$1" s
    shift
    for s in "$@"; do bin_block "$os" "${s%=*}" "${s##*=}"; done
    emit "$os" "$BOUND"
  }
  # c1 OS OTHER [LINE...]: the base ceilings, with LINE... after them (from line
  # 6 on). OTHER is the host that is not OS. CRLF for windows.
  c1() {
    local os="$1" other="$2" l
    shift 2
    {
      printf '%s\n' '# fixture' ''
      printf '%s\n' "$os slow 30 60 owner-a 2026-12-31 the fixture's slow binary"
      printf '%s\n' "$os lib:memory_industry 12 30 owner-a 2026-12-31 unit tests (no file to check: not a tests/ binary)"
      printf '%s\n' "$other other_host 1 11 owner-a 2026-12-31 a line of the other host"
      for l in "$@"; do printf '%s\n' "$l"; done
    } | if [[ "$os" == windows ]]; then awk '{ printf "%s\r\n", $0 }'; else cat; fi
  }

  # tt ID OS LOG CEILINGS [ARG...]: the judge over those files; its rc and the
  # whole of what it printed (stdout and stderr together) are $rc and $tmp/ID.out.
  tt() {
    local id="$1" os="$2" log="$3" ceil="$4"
    shift 4
    rc=0
    bash "$SELF" --judge "$log" --until "$BOUND" --os "$os" --today "$TODAY" \
      --ceilings "$ceil" --tests-dir "$tmp/tests" "$@" >"$tmp/$id.out" 2>&1 || rc=$?
  }
  want_rc() {
    (( rc == $2 )) || self_fail "$1 exited $rc, not $2: $(cat "$tmp/$1.out")"
  }
  # has ID WORD...: the output holds each WORD.
  has() {
    local id="$1" w
    shift
    for w in "$@"; do
      grep -qF -- "$w" "$tmp/$id.out" || self_fail "$id: the output has no '$w': $(cat "$tmp/$id.out")"
    done
  }
  # lacks ID WORD...: the output holds none of the WORDs.
  lacks() {
    local id="$1" w
    shift
    for w in "$@"; do
      if grep -qF -- "$w" "$tmp/$id.out"; then self_fail "$id: the output has '$w', and it must not: $(cat "$tmp/$id.out")"; fi
    done
  }
  # fail_with ID WORD...: some line of the output starts with FAIL: and holds every WORD.
  fail_with() {
    local id="$1" line word ok
    shift
    while IFS= read -r line; do
      ok=1
      for word in "$@"; do
        [[ "$line" == *"$word"* ]] || { ok=0; break; }
      done
      if (( ok )); then return 0; fi
    done < <(grep '^FAIL: ' "$tmp/$id.out" | tr -d '\r' || true)
    self_fail "$id: no FAIL: line holds all of [$*]: $(cat "$tmp/$id.out")"
  }
  # table_rows ID: the rows of the table of the slowest, in the order printed.
  table_rows() { grep -E '^ +[0-9]+\.[0-9]{2} s  ' "$tmp/$1.out" || true; }

  # TT1 and TT2: the same log in both shapes gives the same keys and seconds.
  # The presence anchor of every red row below: this one has to be green.
  tt_green() {
    local id="$1" os="$2" other="$3" first rows
    l1 "$os" >"$tmp/$id.log"
    c1 "$os" "$other" >"$tmp/$id.txt"
    tt "$id" "$os" "$tmp/$id.log" "$tmp/$id.txt"
    want_rc "$id" 0
    has "$id" "OK  test times on $os" '5 binaries' '74.59 s in all'
    # The rows go through a variable, not a pipe into grep -q: under pipefail a
    # grep that stops at its first hit can fail the pipe it leaves behind.
    rows="$(table_rows "$id")"
    first="$(sed -n 1p <<<"$rows")"
    [[ "$first" == *'52.50 s  slow'* ]] \
      || self_fail "$id: the first row of the table is not slow at 52.50 s, the sum of its two steps: '$first' / $(cat "$tmp/$id.out")"
    grep -qE ' 21\.51 s  lib:memory_industry' <<<"$rows" \
      || self_fail "$id: the table has no lib:memory_industry at 21.51 s: $(cat "$tmp/$id.out")"
    grep -qE ' 0\.00 s  bin:memory_industry' <<<"$rows" \
      || self_fail "$id: the table has no bin:memory_industry at 0.00 s: $(cat "$tmp/$id.out")"
    grep -qE ' 0\.27 s  doc:memory_industry' <<<"$rows" \
      || self_fail "$id: the table has no doc:memory_industry at 0.27 s: $(cat "$tmp/$id.out")"
    lacks "$id" late 999 FAIL
  }
  tt_green TT1 linux windows
  tt_green TT2 windows linux

  # TT3: over its ceiling. The table is printed all the same.
  simple_log linux slow=70.00 lib:memory_industry=21.51 >"$tmp/TT3.log"
  c1 linux windows >"$tmp/TT3.txt"
  tt TT3 linux "$tmp/TT3.log" "$tmp/TT3.txt"
  want_rc TT3 1
  fail_with TT3 slow 70.00 '60 s' linux
  has TT3 '70.00 s  slow'
  lacks TT3 'OK  test times'

  # TT4: a binary with no line has the default of 10 s, and 10.00 passes.
  l1 linux 10.01 >"$tmp/TT4.log"
  c1 linux windows >"$tmp/TT4.txt"
  tt TT4 linux "$tmp/TT4.log" "$tmp/TT4.txt"
  want_rc TT4 1
  fail_with TT4 fast 10.01 default
  l1 linux 10.00 >"$tmp/TT4b.log"
  tt TT4b linux "$tmp/TT4b.log" "$tmp/TT4.txt"
  want_rc TT4b 0

  # TT5: under its floor is red too, and the floor and the ceiling themselves pass.
  simple_log linux slow=29.99 lib:memory_industry=21.51 >"$tmp/TT5.log"
  c1 linux windows >"$tmp/TT5.txt"
  tt TT5 linux "$tmp/TT5.log" "$tmp/TT5.txt"
  want_rc TT5 1
  fail_with TT5 slow 29.99 floor '30 s'
  simple_log linux slow=30.00 lib:memory_industry=21.51 >"$tmp/TT5b.log"
  tt TT5b linux "$tmp/TT5b.log" "$tmp/TT5.txt"
  want_rc TT5b 0
  simple_log linux slow=60.00 lib:memory_industry=21.51 >"$tmp/TT5c.log"
  tt TT5c linux "$tmp/TT5c.log" "$tmp/TT5.txt"
  want_rc TT5c 0

  # TT6 and TT7: a line for a binary this run did not run, and a line for a test
  # file that is gone, are red.
  l1 linux >"$tmp/L1.linux.log"
  c1 linux windows 'linux gone_from_run 1 20 owner-a 2026-12-31 x' >"$tmp/TT6.txt"
  tt TT6 linux "$tmp/L1.linux.log" "$tmp/TT6.txt"
  want_rc TT6 1
  fail_with TT6 gone_from_run 'did not run'
  c1 linux windows 'windows deleted_test 1 20 owner-a 2026-12-31 x' >"$tmp/TT7.txt"
  tt TT7 linux "$tmp/L1.linux.log" "$tmp/TT7.txt"
  want_rc TT7 1
  fail_with TT7 deleted_test 'does not exist'

  # TT8: a line expires the day after its date, not on it.
  c1 linux windows | sed '3s/2026-12-31/2026-10-13/' >"$tmp/TT8.txt"
  tt TT8 linux "$tmp/L1.linux.log" "$tmp/TT8.txt"
  want_rc TT8 1
  fail_with TT8 slow expired 2026-10-13
  c1 linux windows | sed '3s/2026-12-31/2026-10-14/' >"$tmp/TT8b.txt"
  tt TT8b linux "$tmp/L1.linux.log" "$tmp/TT8b.txt"
  want_rc TT8b 0

  # TT9: a malformed line is red and says which line it is. In the rows about
  # slow, line 3 (the good line of slow) is a comment: left there, a line 6 that
  # the judge wrongly accepted would be flagged as a duplicate of it, and the row
  # would stay red for a reason that is not its own. Row g is the duplicate, so
  # it keeps line 3. The second word is what only that reason says.
  tt9() {
    local id="$1" word="$2" line="$3" keep3="$4"
    if [[ "$keep3" == keep ]]; then
      c1 linux windows "$line" >"$tmp/$id.txt"
    else
      c1 linux windows "$line" | sed '3s/.*/# no line for slow here/' >"$tmp/$id.txt"
    fi
    tt "$id" linux "$tmp/L1.linux.log" "$tmp/$id.txt"
    want_rc "$id" 1
    fail_with "$id" ':6:' "$word"
  }
  tt9 TT9a "not '" 'linux slow 30 60 owner-a 2026-12-31' drop
  tt9 TT9b "not '" 'linux slow 30 60.5 owner-a 2026-12-31 r' drop
  tt9 TT9c "not '" 'macos slow 30 60 owner-a 2026-12-31 r' drop
  tt9 TT9d 'at least 1' 'linux slow 60 60 owner-a 2026-12-31 r' drop
  tt9 TT9e 'at least 1' 'linux slow 0 60 owner-a 2026-12-31 r' drop
  tt9 TT9f 'default' 'linux fast 1 10 owner-a 2026-12-31 r' keep
  tt9 TT9g 'already has a line at 3' 'linux slow 30 60 owner-a 2026-12-31 r' keep
  tt9 TT9h "not '" 'linux slow 30 60 owner-a 2026/12/31 r' drop

  # TT10: what the judge cannot read is exit 2, never green.
  sed '/^=== end of the timed steps ===$/,$d' "$tmp/L1.linux.log" >"$tmp/TT10a.log"
  tt TT10a linux "$tmp/TT10a.log" "$tmp/TT3.txt"
  want_rc TT10a 2
  has TT10a 'has no line'

  { bin_block linux slow 5 none; bin_block linux fast 1.00; emit linux "$BOUND"; } >"$tmp/TT10b.log"
  tt TT10b linux "$tmp/TT10b.log" "$tmp/TT3.txt"
  want_rc TT10b 2
  has TT10b "no 'test result'"

  { bin_block linux slow 5 none; emit linux "$BOUND"; } >"$tmp/TT10c.log"
  tt TT10c linux "$tmp/TT10c.log" "$tmp/TT3.txt"
  want_rc TT10c 2
  has TT10c "no 'test result'"

  { echo hello; emit linux "$BOUND"; } >"$tmp/TT10d.log"
  tt TT10d linux "$tmp/TT10d.log" "$tmp/TT3.txt"
  want_rc TT10d 2
  has TT10d 'no test binary was timed'

  tt TT10e linux "$tmp/L1.linux.log" "$tmp/none.txt"
  want_rc TT10e 2
  has TT10e 'no ceilings file'

  tt TT10f darwin "$tmp/L1.linux.log" "$tmp/TT3.txt"
  want_rc TT10f 2
  has TT10f 'windows and linux'

  tt TT10g linux "$tmp/none.log" "$tmp/TT3.txt"
  want_rc TT10g 2
  has TT10g 'no log'

  rc=0
  bash "$SELF" --judge "$tmp/L1.linux.log" --os linux --today "$TODAY" \
    --ceilings "$tmp/TT3.txt" --tests-dir "$tmp/tests" >"$tmp/TT10h.out" 2>&1 || rc=$?
  want_rc TT10h 2
  has TT10h '--until'

  # TT11: twelve binaries, ten rows: the two fastest are not in the table.
  {
    for n in 01 02 03 04 05 06 07 08 09 10 11 12; do bin_block linux "t$n" "0.$n"; done
    emit linux "$BOUND"
  } >"$tmp/TT11.log"
  c1 linux windows | sed '3,4d' >"$tmp/TT11.txt"
  tt TT11 linux "$tmp/TT11.log" "$tmp/TT11.txt"
  want_rc TT11 0
  rows="$(table_rows TT11)"
  [[ "$(grep -c . <<<"$rows")" -eq 10 ]] \
    || self_fail "TT11: the table of the slowest has $(grep -c . <<<"$rows") rows, not 10: $(cat "$tmp/TT11.out")"
  [[ "$(sed -n 1p <<<"$rows")" == *' t12'* ]] || self_fail "TT11: the first row is not t12: $rows"
  [[ "$(sed -n '$p' <<<"$rows")" == *' t03'* ]] || self_fail "TT11: the last row is not t03: $rows"
  if grep -qE 't0[12]( |$)' <<<"$rows"; then self_fail "TT11: a row names t02 or t01: $rows"; fi

  # TT12: every red of the run in one run, not the first one.
  simple_log linux slow=70.00 fast=11.00 lib:memory_industry=21.51 >"$tmp/TT12.log"
  c1 linux windows | sed '3s/2026-12-31/2026-10-13/' >"$tmp/TT12.txt"
  tt TT12 linux "$tmp/TT12.log" "$tmp/TT12.txt"
  want_rc TT12 1
  fail_with TT12 expired 2026-10-13
  fail_with TT12 slow 70.00
  fail_with TT12 fast default

  echo "OK  self-test: test-times sums each binary's finished-in over both cargo shapes up to the boundary, prints the 10 slowest, and is red for a ceiling passed, the default passed, a floor undercut, a dead or expired line and a malformed one, all in one run; a log it cannot read is exit 2"
  exit 0
fi
# --- self-test end ---

# --- the judge -------------------------------------------------------------------
# Why it exists: 0.30 took v029_accounts_over_real_http from 553 s to 10.9 s by
# hand, and nothing stopped it from drifting back. This pins the time of every
# test binary, in both directions, over the log merge-gate.sh already writes.
#
# Why it reads text: stable cargo test has no machine-readable timing (libtest's
# --format json needs -Z unstable-options, and nextest cannot be the verdict: it
# splits a binary into processes and this crate keeps global state behind a
# Mutex). The `finished in` of the summary line is the one stable source. That
# bends the ci.md rule that no deciding number comes from text for humans; the
# repo's AGENTS.md wins, and the parser is strict to pay for it: what it cannot
# pair is exit 2, never green. Revert when libtest stabilises --format json.
#
# What it does not time: the instrumented run of crap-gate.sh and the mutation
# step (coverage and mutants, not tests), which sit after the boundary.
#
#   test-times.sh --judge LOG --until LINE [--os windows|linux] [--today YYYY-MM-DD]
#                 [--ceilings FILE] [--tests-dir DIR]
#   exit 0 green, 1 a time or a ceilings line is red, 2 it could not judge.

# 10 s: every regression 0.30 fixed measured more than that on both hosts (the
# smallest, 10.13 s), and the slowest binary with no line measures 6.7 s on
# Windows and 6.8 s on Linux.
DEFAULT_CEILING_S=10
SLOWEST_N=10

die2() { echo "FAIL: $*" >&2; exit 2; }

# fmt_cs CENTISECONDS: 5250 -> 52.50. Times are summed as whole centiseconds so a
# ceiling is compared exactly, with no float on the edge of a line.
fmt_cs() { printf '%d.%02d' $(($1 / 100)) $(($1 % 100)); }

# parse_log LOG UNTIL: one record per line, tab separated.
#   H key cs      a binary and its summed time, in order of first appearance
#   F 0|1         whether the boundary line was found
#   U line key    the first header with no `test result` before the next header
#                 or the boundary
# A `test result` with no pending header (a test that prints one with
# --nocapture) is ignored.
parse_log() {
  UNTIL="$2" awk '
    BEGIN { until = ENVIRON["UNTIL"]; OFS = "\t" }
    {
      sub(/\r$/, "")
      if ($0 == until) { found = 1; exit }
    }
    /^ +Running (unittests )?[^ ]+ \(.+\)$/ {
      line = $0
      sub(/^ +Running /, "", line)
      unit = 0
      if (line ~ /^unittests /) { unit = 1; sub(/^unittests /, "", line) }
      src = line;  sub(/ \(.*$/, "", src);  gsub(/\\/, "/", src)
      path = line; sub(/^[^ ]+ \(/, "", path); sub(/\)$/, "", path); gsub(/\\/, "/", path)
      n = split(path, parts, "/")
      key = parts[n]
      sub(/\.exe$/, "", key)
      sub(/-[0-9a-f]+$/, "", key)
      if (unit) key = ((src ~ /(^|\/)lib\.rs$/) ? "lib:" : "bin:") key
      if (pending != "" && !unpaired) { unpaired = 1; ulineno = pline; ukey = pending }
      pending = key; pline = NR
      next
    }
    /^ +Doc-tests [A-Za-z0-9_]+$/ {
      key = $0
      sub(/^ +Doc-tests /, "", key)
      if (pending != "" && !unpaired) { unpaired = 1; ulineno = pline; ukey = pending }
      pending = "doc:" key; pline = NR
      next
    }
    /^test result: (ok|FAILED)\. .*finished in [0-9]+(\.[0-9]+)?s$/ {
      if (pending == "") next
      t = $0
      sub(/^.*finished in /, "", t); sub(/s$/, "", t)
      cs = int(t * 100 + 0.5)
      if (!(pending in sum)) { order[++count] = pending; sum[pending] = 0 }
      sum[pending] += cs
      pending = ""
    }
    END {
      if (pending != "" && !unpaired) { unpaired = 1; ulineno = pline; ukey = pending }
      for (i = 1; i <= count; i++) print "H", order[i], sum[order[i]]
      print "F", found + 0
      if (unpaired) print "U", ulineno, ukey
    }
  ' "$1"
}

if [[ "${1:-}" == "--judge" ]]; then
  shift
  LOG="${1:-}"; [[ $# -gt 0 ]] && shift
  UNTIL=""
  OS=""
  TODAY=""
  CEILINGS="$ROOT/scripts/test-time-ceilings.txt"
  TESTS_DIR="$ROOT/rust/tests"
  have_until=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --until|--os|--today|--ceilings|--tests-dir)
        [[ $# -ge 2 ]] || die2 "$1 needs a value"
        case "$1" in
          --until) UNTIL="$2"; have_until=1 ;;
          --os) OS="$2" ;;
          --today) TODAY="$2" ;;
          --ceilings) CEILINGS="$2" ;;
          --tests-dir) TESTS_DIR="$2" ;;
        esac
        shift 2 ;;
      *) die2 "unknown argument '$1'" ;;
    esac
  done

  (( have_until )) || die2 "--judge needs --until <the line that ends the timed steps>"
  if [[ -z "$OS" ]]; then
    case "$(uname -s)" in
      MINGW*|MSYS*|CYGWIN*) OS=windows ;;
      Linux) OS=linux ;;
      *) OS="$(uname -s)" ;;
    esac
  fi
  [[ "$OS" == windows || "$OS" == linux ]] || die2 "test times are judged on windows and linux, and this is '$OS'"
  [[ -n "$TODAY" ]] || TODAY="$(date +%F)"
  [[ -f "$LOG" ]] || die2 "no log at $LOG"
  [[ -f "$CEILINGS" ]] || die2 "no ceilings file at $CEILINGS"

  parsed="$(parse_log "$LOG" "$UNTIL")"
  declare -A MEAS=()
  KEYS=()
  found=0
  unpaired=""
  while IFS=$'\t' read -r kind a b; do
    case "$kind" in
      H) KEYS+=("$a"); MEAS["$a"]="$b" ;;
      F) found="$a" ;;
      U) unpaired="$a $b" ;;
    esac
  done <<<"$parsed"
  (( found )) || die2 "$LOG has no line '$UNTIL', so it was cut or not yet written; no time is judged"
  if [[ -n "$unpaired" ]]; then
    die2 "$LOG:${unpaired%% *}: ${unpaired#* } has no 'test result' line before the next binary or the boundary; the log is cut or interleaved, and the judge will not guess"
  fi
  (( ${#KEYS[@]} > 0 )) || die2 "no test binary was timed in $LOG before '$UNTIL': nothing timed is not a pass"

  # Slowest first, ties by name: what the table shows and the order of the reds.
  total=0
  sorted=()
  while IFS=$'\t' read -r cs key; do
    sorted+=("$key")
    total=$((total + cs))
  done < <(for k in "${KEYS[@]}"; do printf '%s\t%s\n' "${MEAS[$k]}" "$k"; done | LC_ALL=C sort -t$'\t' -k1,1nr -k2,2)

  # The ceilings file: lint every line, whatever its host, and keep the good ones.
  declare -A LINE_FLOOR=() LINE_CEIL=() LINE_AT=()
  reds=()
  red() { reds+=("$1"); }
  n=0
  host_lines=0
  blank_or_comment='^[[:space:]]*(#|$)'
  shape='<os> <binary> <floor_s> <ceiling_s> <owner> <expires YYYY-MM-DD> <reason>'
  while IFS= read -r line || [[ -n "$line" ]]; do
    n=$((n + 1))
    line="${line%$'\r'}"
    [[ "$line" =~ $blank_or_comment ]] && continue
    read -r l_os l_bin l_floor l_ceil l_owner l_exp l_reason <<<"$line"
    if [[ ! "$l_os" =~ ^(windows|linux)$ || ! "$l_bin" =~ ^(lib:|bin:|doc:)?[A-Za-z0-9_-]+$ \
      || ! "$l_floor" =~ ^[0-9]+$ || ! "$l_ceil" =~ ^[0-9]+$ || -z "$l_owner" \
      || ! "$l_exp" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ || -z "$l_reason" ]]; then
      red "$CEILINGS:$n: not '$shape': $line"
      continue
    fi
    f=$((10#$l_floor)); c=$((10#$l_ceil))
    if (( f < 1 || f >= c )); then
      red "$CEILINGS:$n: $l_bin on $l_os: the floor ($f s) has to be at least 1 and under the ceiling ($c s)"
      continue
    fi
    if (( c <= DEFAULT_CEILING_S )); then
      red "$CEILINGS:$n: $l_bin on $l_os: a ceiling of $c s is not above the default $DEFAULT_CEILING_S s, so the line changes nothing. Remove it."
      continue
    fi
    if [[ -n "${LINE_AT[$l_os $l_bin]:-}" ]]; then
      red "$CEILINGS:$n: $l_bin on $l_os already has a line at ${LINE_AT[$l_os $l_bin]}"
      continue
    fi
    LINE_AT["$l_os $l_bin"]="$n"; LINE_FLOOR["$l_os $l_bin"]="$f"; LINE_CEIL["$l_os $l_bin"]="$c"
    [[ "$l_os" == "$OS" ]] && host_lines=$((host_lines + 1))
    if [[ "$l_exp" < "$TODAY" ]]; then
      red "$CEILINGS:$n: the line of $l_bin on $l_os expired on $l_exp. Renew it with a reason that is still true, or remove it."
    fi
    if [[ "$l_bin" != *:* && ! -f "$TESTS_DIR/$l_bin.rs" ]]; then
      red "$CEILINGS:$n: $l_bin names $TESTS_DIR/$l_bin.rs, which does not exist. Remove the line."
    fi
    if [[ "$l_os" == "$OS" && -z "${MEAS[$l_bin]:-}" ]]; then
      red "$CEILINGS:$n: $l_bin has a line for $OS and this run did not run it. Remove the line."
    fi
  done <"$CEILINGS"

  shown=$SLOWEST_N
  (( ${#sorted[@]} < shown )) && shown=${#sorted[@]}
  echo "test times on $OS: ${#KEYS[@]} binaries, $(fmt_cs "$total") s in all; the $shown slowest:"
  rank=0
  for key in "${sorted[@]}"; do
    rank=$((rank + 1))
    (( rank > SLOWEST_N )) && break
    at="${LINE_AT[$OS $key]:-}"
    if [[ -n "$at" ]]; then note="ceiling ${LINE_CEIL[$OS $key]} s, floor ${LINE_FLOOR[$OS $key]} s"
    else note="default ceiling $DEFAULT_CEILING_S s"; fi
    printf '  %9s s  %-52s %s\n' "$(fmt_cs "${MEAS[$key]}")" "$key" "$note"
  done

  # The judgement of the run: every binary measured, not only the ten shown.
  for key in "${sorted[@]}"; do
    cs="${MEAS[$key]}"
    at="${LINE_AT[$OS $key]:-}"
    if [[ -n "$at" ]]; then
      ceil_s="${LINE_CEIL[$OS $key]}"; floor_s="${LINE_FLOOR[$OS $key]}"
      if (( cs > ceil_s * 100 )); then
        red "$key took $(fmt_cs "$cs") s on $OS, over its ceiling of $ceil_s s ($CEILINGS:$at). Bring it back under, or raise the line with the run that measured it; first check that this run was alone on the machine."
      elif (( cs < floor_s * 100 )); then
        red "$key took $(fmt_cs "$cs") s on $OS, under the floor of $floor_s s of its line ($CEILINGS:$at): the ceiling of $ceil_s s no longer describes it. Lower the line (floor = half the fastest run, ceiling = 1.3 x the slowest), or remove it if the binary is under $DEFAULT_CEILING_S s now."
      fi
    elif (( cs > DEFAULT_CEILING_S * 100 )); then
      red "$key took $(fmt_cs "$cs") s on $OS, over the default ceiling of $DEFAULT_CEILING_S s, and has no line in $CEILINGS. Bring it back under, or add a line: $OS $key <floor> <ceiling> <owner> <expires> <reason>."
    fi
  done

  if (( ${#reds[@]} > 0 )); then
    for r in "${reds[@]}"; do echo "FAIL: $r" >&2; done
    echo "FAIL: test times: ${#reds[@]} problem(s) on $OS; $CEILINGS says how a line is set" >&2
    exit 1
  fi
  echo "OK  test times on $OS: ${#KEYS[@]} binaries within their ceilings ($host_lines lines for $OS, default $DEFAULT_CEILING_S s)"
  exit 0
fi

echo "FAIL: unknown argument '${1:-}'" >&2; exit 2
