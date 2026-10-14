#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$ROOT/scripts/advisory-exceptions.sh"

# --- self-test: the judge of the ignored advisories, against the files it must refuse ---
# An exception list the judge cannot break is a paragraph. Each row runs this
# script through its CLI (never as a function) over a list of exceptions, the two
# TOML mirrors and a `cargo audit --json` report built for the row, and always
# passes --today, --exceptions, --audit-toml and --deny-toml, so no row depends
# on the date, on the real files or on the network. A row that must be red has
# its green twin (the same files with the one change undone) beside it: a red
# that a green twin does not show is a judge that refuses everything.
#   ./scripts/advisory-exceptions.sh --self-test
if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  TODAY=2026-10-14
  OWN=orquestador-0.31
  A=RUSTSEC-2023-0071
  B=RUSTSEC-2025-0119
  C=RUSTSEC-2024-0436
  D=RUSTSEC-2026-0002
  X=RUSTSEC-2026-0346
  LA="$A $OWN 2026-11-30 rsa is in the lockfile only through sqlx-mysql, which no feature builds"
  LB="$B $OWN 2026-11-30 number_prefix is unmaintained and only formats progress output"
  LC="$C $OWN 2026-11-30 paste is a proc-macro that runs at build time only"
  LD="$D $OWN 2026-11-30 lru is long gone from the lockfile"
  mkdir -p "$tmp/bin"

  # ex_file LINE...: a list of exceptions, with the two lines before the first
  # one that a real list has (a comment and a blank), so the first LINE is line 3.
  ex_file() {
    printf '# fixture\n\n'
    local l
    for l in "$@"; do printf '%s\n' "$l"; done
  }
  # audit_toml ID...: the mirror cargo-audit reads, bare IDs only.
  audit_toml() {
    local i
    printf '[advisories]\nignore = [\n'
    for i in "$@"; do printf '    "%s",\n' "$i"; done
    printf ']\n'
  }
  # den ID: an entry of deny.toml as the real file writes it. A is a bare
  # string and every other ID a table, so a judge that reads one shape only is
  # red on the base files.
  den() {
    if [[ "$1" == "$A" ]]; then
      printf '"%s"' "$1"
    else
      printf '{ id = "%s", reason = "owner, expiry and why: scripts/advisory-exceptions.txt" }' "$1"
    fi
  }
  # deny_toml ENTRY...: a deny.toml whose ignore list is those entries, each one
  # already TOML.
  deny_toml() {
    local e
    printf '[licenses]\nallow = ["MIT"]\n\n[advisories]\nignore = [\n'
    for e in "$@"; do printf '    %s,\n' "$e"; done
    printf ']\n'
  }
  # Each advisory has a package of its own in the report, so that a row can tell
  # which one a line of the judge's output was written from.
  pkg_of() { printf 'pkg-%s' "${1#RUSTSEC-}"; }
  ver_of() { printf '1.0.%d' "$((10#${1##*-}))"; }
  # report VULN_IDS UNMAINT_IDS [IGNORE_IDS]: a cargo audit --json report in the
  # shape cargo-audit 0.22.2 prints, with a yanked entry whose advisory is null,
  # which a reader that walks every warning has to survive.
  report() {
    local id lv='' lu='' li='' count=0
    for id in $1; do
      lv+="${lv:+,}{\"advisory\":{\"id\":\"$id\",\"title\":\"t\"},\"versions\":{\"patched\":[]},\"package\":{\"name\":\"$(pkg_of "$id")\",\"version\":\"$(ver_of "$id")\"}}"
      count=$((count + 1))
    done
    for id in $2; do
      lu+="${lu:+,}{\"kind\":\"unmaintained\",\"package\":{\"name\":\"$(pkg_of "$id")\",\"version\":\"$(ver_of "$id")\"},\"advisory\":{\"id\":\"$id\"}}"
    done
    for id in ${3:-}; do li+="${li:+,}\"$id\""; done
    local found=false
    if (( count > 0 )); then found=true; fi
    printf '{"database":{},"lockfile":{"dependency-count":438},'
    printf '"settings":{"target_arch":[],"target_os":[],"severity":null,"ignore":[%s],"informational_warnings":["unmaintained","unsound","notice"]},' "$li"
    printf '"vulnerabilities":{"found":%s,"count":%d,"list":[%s]},' "$found" "$count" "$lv"
    printf '"warnings":{"unmaintained":[%s],"yanked":[{"kind":"yanked","package":{"name":"spin","version":"0.9.8"},"advisory":null}]}}\n' "$lu"
  }
  # row ID: the directory of a row, holding the base files: A, B and C in the
  # list, in both mirrors and in the report.
  row() {
    local d="$tmp/$1"
    mkdir -p "$d"
    ex_file "$LA" "$LB" "$LC" >"$d/ex.txt"
    audit_toml "$A" "$B" "$C" >"$d/audit.toml"
    deny_toml "$(den "$A")" "$(den "$B")" "$(den "$C")" >"$d/deny.toml"
    report "$A $X" "$B $C" >"$d/report.json"
  }
  # judge ID [ARG...]: the script over the row's files. Its exit code is $rc and
  # whatever it printed, stdout and stderr together and without the CR that
  # Python on Windows writes, is $tmp/ID.out.
  judge() {
    local id="$1"
    shift
    rc=0
    bash "$SELF" --judge "$tmp/$id/report.json" --today "$TODAY" \
      --exceptions "$tmp/$id/ex.txt" --audit-toml "$tmp/$id/audit.toml" \
      --deny-toml "$tmp/$id/deny.toml" "$@" >"$tmp/$id.raw" 2>&1 || rc=$?
    tr -d '\r' <"$tmp/$id.raw" >"$tmp/$id.out"
  }
  want_rc() {
    (( rc == $2 )) || self_fail "$1 exited $rc, not $2: $(cat "$tmp/$1.out")"
  }
  has() {
    local id="$1" w
    shift
    for w in "$@"; do
      grep -qF -- "$w" "$tmp/$id.out" || self_fail "$id: the output has no '$w': $(cat "$tmp/$id.out")"
    done
  }
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
    done < <(grep '^FAIL: ' "$tmp/$id.out" || true)
    self_fail "$id: no FAIL: line holds all of [$*]: $(cat "$tmp/$id.out")"
  }
  # fail_lacks ID WORD...: no line starting with FAIL: holds any WORD.
  fail_lacks() {
    local id="$1" w
    shift
    for w in "$@"; do
      if grep '^FAIL: ' "$tmp/$id.out" | grep -qF -- "$w"; then
        self_fail "$id: a FAIL: line names '$w', and it must not: $(cat "$tmp/$id.out")"
      fi
    done
  }
  # fail_count ID N: N lines start with FAIL:, the closing summary among them.
  fail_count() {
    local n
    n="$(grep -c '^FAIL: ' "$tmp/$1.out" || true)"
    (( n == $2 )) || self_fail "$1: $n lines start with FAIL:, not $2: $(cat "$tmp/$1.out")"
  }

  # AE1: three exceptions in order. The presence anchor of every red row below:
  # this one has to be green. Each `ok` line names the kind, the package and the
  # version of the report that carries it, which is what tells which report
  # section the judge read; the advisory nobody ignores is not its business.
  row AE1
  judge AE1
  want_rc AE1 0
  [[ "$(grep -c '^ok  ' "$tmp/AE1.out" || true)" == 3 ]] \
    || self_fail "AE1: not three lines start with 'ok  ': $(cat "$tmp/AE1.out")"
  ok_line_of() {
    local id="$1" kind="$2" line
    line="$(grep "^ok  $id " "$tmp/AE1.out" || true)"
    [[ "$line" == *"$kind $(pkg_of "$id") $(ver_of "$id")"* ]] \
      || self_fail "AE1: the ok line of $id does not say '$kind $(pkg_of "$id") $(ver_of "$id")': '$line' / $(cat "$tmp/AE1.out")"
  }
  ok_line_of "$A" vulnerability
  ok_line_of "$B" unmaintained
  ok_line_of "$C" unmaintained
  lacks AE1 "$X" FAIL
  has AE1 'OK  advisory exceptions: 3'

  # AE1b: the list as a Windows editor leaves it. A CR in the date is not a
  # malformed date.
  row AE1b
  ex_file "$LA" "$LB" "$LC" | awk '{ printf "%s\r\n", $0 }' >"$tmp/AE1b/ex.txt"
  judge AE1b
  want_rc AE1b 0

  # AE2: the day after the expiry is red, the day itself is not.
  row AE2
  ex_file "$LA" "$LB" "$C $OWN 2026-10-13 why c" >"$tmp/AE2/ex.txt"
  judge AE2
  want_rc AE2 1
  fail_with AE2 "$C" expired 2026-10-13
  row AE2b
  ex_file "$LA" "$LB" "$C $OWN 2026-10-14 why c" >"$tmp/AE2b/ex.txt"
  judge AE2b
  want_rc AE2b 0

  # AE3: an expiry 181 days ahead is not an expiry; 180 is.
  row AE3
  ex_file "$LA" "$LB" "$C $OWN 2027-04-13 why c" >"$tmp/AE3/ex.txt"
  judge AE3
  want_rc AE3 1
  fail_with AE3 "$C" 'more than 180 days'
  row AE3b
  ex_file "$LA" "$LB" "$C $OWN 2027-04-12 why c" >"$tmp/AE3b/ex.txt"
  judge AE3b
  want_rc AE3b 0

  # AE4: a malformed line is red, with its line number. The line is in A's place
  # and both mirrors lack A, so it brings no ID with it and the one red left is
  # its shape. 2026-02-30 is the one that only a calendar can refuse.
  n=0
  for bad in \
    "$A $OWN 2026-11-30" \
    "RUSTSEC-2023-071 $OWN 2026-11-30 why" \
    "$A $OWN 2026-02-30 why" \
    "$A $OWN 30-11-2026 why" \
    "$A orq/uestador 2026-11-30 why"; do
    n=$((n + 1))
    row "AE4$n"
    ex_file "$bad" "$LB" "$LC" >"$tmp/AE4$n/ex.txt"
    audit_toml "$B" "$C" >"$tmp/AE4$n/audit.toml"
    deny_toml "$(den "$B")" "$(den "$C")" >"$tmp/AE4$n/deny.toml"
    judge "AE4$n"
    want_rc "AE4$n" 1
    fail_with "AE4$n" malformed ':3:'
    fail_count "AE4$n" 2
  done

  # AE5: the same advisory on two lines.
  row AE5
  ex_file "$LA" "$LB" "$LC" "$LA" >"$tmp/AE5/ex.txt"
  judge AE5
  want_rc AE5 1
  fail_with AE5 "$A" 'listed twice'

  # AE6: D is in the list and in both mirrors and the report does not carry it:
  # it matches nothing in Cargo.lock any more. The other three are in order, so
  # no line names them.
  row AE6
  ex_file "$LA" "$LB" "$LC" "$LD" >"$tmp/AE6/ex.txt"
  audit_toml "$A" "$B" "$C" "$D" >"$tmp/AE6/audit.toml"
  deny_toml "$(den "$A")" "$(den "$B")" "$(den "$C")" "$(den "$D")" >"$tmp/AE6/deny.toml"
  judge AE6
  want_rc AE6 1
  fail_with AE6 "$D" 'matches no advisory'
  fail_lacks AE6 "$A" "$B" "$C"

  # AE7 to AE11: the mirrors say what the list says, in both directions, and an
  # entry of deny.toml that is a crate and not an advisory is one more.
  row AE7
  audit_toml "$A" "$C" >"$tmp/AE7/audit.toml"
  judge AE7
  want_rc AE7 1
  fail_with AE7 audit.toml "$B" 'does not ignore'

  row AE8
  audit_toml "$A" "$B" "$C" "$D" >"$tmp/AE8/audit.toml"
  judge AE8
  want_rc AE8 1
  fail_with AE8 audit.toml "$D" 'no owner'

  row AE9
  deny_toml "$(den "$A")" "$(den "$B")" >"$tmp/AE9/deny.toml"
  judge AE9
  want_rc AE9 1
  fail_with AE9 deny.toml "$C"

  row AE10
  deny_toml "$(den "$A")" "$(den "$B")" "$(den "$C")" "{ id = \"$D\", reason = \"r\" }" >"$tmp/AE10/deny.toml"
  judge AE10
  want_rc AE10 1
  fail_with AE10 deny.toml "$D"

  row AE11
  deny_toml "$(den "$A")" "$(den "$B")" "$(den "$C")" '{ crate = "spin@0.9.8", reason = "r" }' >"$tmp/AE11/deny.toml"
  judge AE11
  want_rc AE11 1
  fail_with AE11 deny.toml spin@0.9.8

  # AE12: no exceptions at all, and mirrors that say nothing in the three ways a
  # file can say nothing, is green with zero.
  row AE12
  ex_file >"$tmp/AE12/ex.txt"
  printf '[advisories]\n' >"$tmp/AE12/audit.toml"
  printf '[licenses]\nallow = ["MIT"]\n' >"$tmp/AE12/deny.toml"
  judge AE12
  want_rc AE12 0
  has AE12 'OK  advisory exceptions: 0'

  # AE13: every red at once, and the summary counts them: C expired, D matching
  # nothing, and audit.toml missing B.
  row AE13
  ex_file "$LA" "$LB" "$C $OWN 2026-10-13 why c" "$LD" >"$tmp/AE13/ex.txt"
  audit_toml "$A" "$C" "$D" >"$tmp/AE13/audit.toml"
  deny_toml "$(den "$A")" "$(den "$B")" "$(den "$C")" "$(den "$D")" >"$tmp/AE13/deny.toml"
  judge AE13
  want_rc AE13 1
  fail_with AE13 "$C" expired
  fail_with AE13 "$D" 'matches no advisory'
  fail_with AE13 audit.toml "$B" 'does not ignore'
  fail_count AE13 4
  has AE13 '3 problem(s)'

  # AE14: what is not a cargo audit report is not judged, exit 2 and not 1.
  row AE14a
  rm -f "$tmp/AE14a/report.json"
  judge AE14a
  want_rc AE14a 2
  has AE14a 'not a cargo audit'
  row AE14b
  printf 'not json\n' >"$tmp/AE14b/report.json"
  judge AE14b
  want_rc AE14b 2
  has AE14b 'not a cargo audit'
  row AE14c
  printf '{"settings":{"ignore":[]},"warnings":{}}\n' >"$tmp/AE14c/report.json"
  judge AE14c
  want_rc AE14c 2
  has AE14c 'not a cargo audit'

  # AE15: a report made with ignores has dropped the ignored advisories, so
  # every exception would look unused. A is out of the lists here, as cargo-audit
  # leaves it, and the judge must refuse before it says so.
  row AE15
  report "$X" "$B $C" "$A" >"$tmp/AE15/report.json"
  judge AE15
  want_rc AE15 2
  has AE15 'made with ignores' "$A"
  lacks AE15 'matches no advisory'

  # AE16: a list that is not there is not an empty list.
  row AE16
  rm -f "$tmp/AE16/ex.txt"
  judge AE16
  want_rc AE16 2
  has AE16 ex.txt

  # AE17: a mirror that is not TOML is named, whichever one it is.
  row AE17a
  printf 'ignore = [\n' >"$tmp/AE17a/audit.toml"
  judge AE17a
  want_rc AE17a 2
  has AE17a audit.toml
  lacks AE17a deny.toml
  row AE17b
  printf '[advisories\n' >"$tmp/AE17b/deny.toml"
  judge AE17b
  want_rc AE17b 2
  has AE17b deny.toml
  lacks AE17b audit.toml

  # AE18: a Python without tomllib (before 3.11) is not a reason to read the
  # mirrors some other way. The stand-in answers the probe the way such a Python does.
  printf '%s\n' '#!/bin/sh' \
    'case "$*" in' \
    '  *tomllib*) echo "ModuleNotFoundError: No module named '"'"'tomllib'"'"'" >&2; exit 1 ;;' \
    'esac' \
    'exit 0' >"$tmp/bin/python-without-tomllib"
  chmod +x "$tmp/bin/python-without-tomllib"
  row AE18
  PYTHON_BIN="$tmp/bin/python-without-tomllib" judge AE18
  want_rc AE18 2
  has AE18 tomllib 3.11

  # AE19: an argument it does not know is exit 2.
  rc=0
  bash "$SELF" --nope >"$tmp/AE19.out" 2>&1 || rc=$?
  want_rc AE19 2
  has AE19 "unknown argument '--nope'"

  echo "OK  self-test: advisory exceptions are green for three owned, dated ones that match the report, with a CR in the list too, and red for an expiry past, an expiry beyond 180 days, a line malformed or repeated, one that matches no advisory of the report, and an ID that one of the two mirrors has and the list has not, or the other way round, all in one run; a report that is not cargo audit's, one made with ignores, a file that is missing or not TOML and a Python without tomllib are exit 2"
  exit 0
fi

# Same rule as mutants-gate.sh and test-times.sh: PYTHON_BIN, else the first python
# that is not the Microsoft Store stub, which exits 49 and prints nothing useful.
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

# --- --judge: pure. It reads the four files it is given and calls nothing ---
# The report is `cargo audit --json -f rust/Cargo.lock` made from a directory with
# no audit.toml (merge-gate.sh does it), so every advisory the lockfile carries is
# in it, the excepted ones included. Exit 1 is a verdict, with every problem at
# once; exit 2 is "cannot judge", and says what is missing.
#   ./scripts/advisory-exceptions.sh --judge REPORT [--today YYYY-MM-DD]
#       [--exceptions F] [--audit-toml F] [--deny-toml F]
if [[ "${1:-}" == "--judge" ]]; then
  shift
  report="${1:-}"
  if [[ -z "$report" ]]; then
    echo "FAIL: --judge needs the path of a cargo audit --json report" >&2
    exit 2
  fi
  shift
  today=""
  exceptions="$ROOT/scripts/advisory-exceptions.txt"
  audit_toml="$ROOT/rust/.cargo/audit.toml"
  deny_toml="$ROOT/rust/deny.toml"
  while (( $# > 0 )); do
    case "$1" in
      --today | --exceptions | --audit-toml | --deny-toml)
        if (( $# < 2 )); then
          echo "FAIL: $1 needs a value" >&2
          exit 2
        fi
        case "$1" in
          --today) today="$2" ;;
          --exceptions) exceptions="$2" ;;
          --audit-toml) audit_toml="$2" ;;
          --deny-toml) deny_toml="$2" ;;
        esac
        shift 2
        ;;
      *)
        echo "FAIL: unknown argument '$1'" >&2
        exit 2
        ;;
    esac
  done
  [[ -n "$today" ]] || today="$(date +%F)"

  PY="$(resolve_python)"
  export PYTHONUTF8=1 PYTHONIOENCODING=utf-8
  # The mirrors are TOML: no Python before 3.11 reads them, and a hand-made
  # reader of them would be one more thing that can disagree with cargo.
  if ! "$PY" -c 'import tomllib' >/dev/null 2>&1; then
    echo "FAIL: advisory-exceptions.sh needs Python 3.11+ (tomllib); $PY has none" >&2
    exit 2
  fi

  # An exception of the interpreter is exit 2, never the 1 of an uncaught
  # traceback, which would read as a verdict.
  rc=0
  "$PY" -I - "$report" "$today" "$exceptions" "$audit_toml" "$deny_toml" <<'PYEOF' || rc=$?
import datetime
import json
import re
import sys
import tomllib


class Refused(Exception):
    pass


ID_RE = re.compile(r"RUSTSEC-[0-9]{4}-[0-9]{4}")
OWNER_RE = re.compile(r"[A-Za-z0-9._-]+")
DATE_RE = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}")
MAX_DAYS = 180
SHAPE = "<advisory> <owner> <expires> <reason>"


def parse_date(text):
    if not DATE_RE.fullmatch(text):
        return None
    try:
        return datetime.date(int(text[:4]), int(text[5:7]), int(text[8:10]))
    except ValueError:
        return None


def read_text(path):
    try:
        with open(path, "rb") as handle:
            return handle.read().decode("utf-8")
    except (OSError, UnicodeDecodeError) as err:
        raise Refused(f"cannot read {path}: {err}")


def not_a_report(path, why):
    return Refused(f"{path} is not a cargo audit --json report ({why})")


def load_report(path):
    try:
        with open(path, "rb") as handle:
            data = json.loads(handle.read().decode("utf-8"))
    except (OSError, ValueError) as err:
        raise not_a_report(path, f"cannot read it as JSON: {err}")
    if not isinstance(data, dict):
        raise not_a_report(path, "its top level is not an object")
    settings = data.get("settings")
    if not isinstance(settings, dict) or not isinstance(settings.get("ignore"), list):
        raise not_a_report(path, "no settings.ignore list")
    vulns = data.get("vulnerabilities")
    if not isinstance(vulns, dict) or not isinstance(vulns.get("list"), list):
        raise not_a_report(path, "no vulnerabilities.list")
    warnings = data.get("warnings")
    if not isinstance(warnings, dict) or not all(isinstance(v, list) for v in warnings.values()):
        raise not_a_report(path, "no warnings object of lists")
    if settings["ignore"]:
        ids = ", ".join(str(i) for i in settings["ignore"])
        raise Refused(
            f"{path} was made with ignores ({ids}), so an ignored exception would look unused; "
            "make it from a directory with no .cargo/audit.toml and no ~/.cargo/audit.toml"
        )
    entries = [("vulnerability", e) for e in vulns["list"]]
    for kind, items in warnings.items():
        entries += [(kind, e) for e in items]
    used = {}
    for kind, entry in entries:
        if not isinstance(entry, dict):
            raise not_a_report(path, f"a {kind} entry is not an object")
        advisory = entry.get("advisory")
        if advisory is None:
            continue
        if not isinstance(advisory, dict):
            raise not_a_report(path, f"the advisory of a {kind} entry is not an object")
        package = entry.get("package")
        package = package if isinstance(package, dict) else {}
        found = (kind, str(package.get("name", "?")), str(package.get("version", "?")))
        hits = used.setdefault(advisory.get("id"), [])
        if found not in hits:
            hits.append(found)
    return used


def load_toml(path):
    try:
        return tomllib.loads(read_text(path))
    except tomllib.TOMLDecodeError as err:
        raise Refused(f"cannot read {path}: {err}")


def table(data, key, path):
    value = data.get(key, {})
    if not isinstance(value, dict):
        raise Refused(f"cannot read {path}: [{key}] is not a table")
    return value


def ignore_list(path):
    value = table(load_toml(path), "advisories", path).get("ignore", [])
    if not isinstance(value, list):
        raise Refused(f"cannot read {path}: advisories.ignore is not a list")
    return value


def audit_ids(path):
    ids = []
    for item in ignore_list(path):
        if not isinstance(item, str):
            raise Refused(f"cannot read {path}: cargo-audit takes only bare advisory IDs in ignore, not {item!r}")
        ids.append(item)
    return ids


def deny_ids(path):
    ids = []
    for item in ignore_list(path):
        if isinstance(item, str):
            ids.append(item)
        elif isinstance(item, dict) and isinstance(item.get("id"), str):
            ids.append(item["id"])
        elif isinstance(item, dict) and isinstance(item.get("crate"), str):
            ids.append(item["crate"])
        else:
            raise Refused(f"cannot read {path}: an ignore entry is neither an ID nor a table with id or crate: {item!r}")
    return ids


def parse_list(path):
    lines = []
    for number, raw in enumerate(read_text(path).split("\n"), 1):
        line = raw.rstrip("\r")
        if line.strip() and not line.lstrip().startswith("#"):
            lines.append((number, line))
    return lines


def judge(report_path, today_text, ex_path, audit_path, deny_path):
    today = parse_date(today_text)
    if today is None:
        raise Refused(f"--today wants YYYY-MM-DD, not '{today_text}'")
    used = load_report(report_path)
    lines = parse_list(ex_path)
    audit = audit_ids(audit_path)
    deny = deny_ids(deny_path)

    problems = []
    entries = []
    first_at = {}
    for number, line in lines:
        fields = line.split(None, 3)
        expires = parse_date(fields[2]) if len(fields) == 4 else None
        if not (len(fields) == 4 and ID_RE.fullmatch(fields[0])
                and OWNER_RE.fullmatch(fields[1]) and expires is not None):
            problems.append(f"{ex_path}:{number}: malformed line, want '{SHAPE}': {line}")
            continue
        advisory, owner, expires_text, _ = fields
        if advisory in first_at:
            problems.append(f"{ex_path}:{number}: {advisory} is listed twice (first at line {first_at[advisory]})")
            continue
        first_at[advisory] = number
        entries.append((number, advisory, owner, expires_text))
        if today > expires:
            problems.append(
                f"{ex_path}:{number}: {advisory} expired on {expires_text} (owner {owner}): "
                "renew it with a new reason and date, or move past the advisory"
            )
        elif (expires - today).days > MAX_DAYS:
            problems.append(
                f"{ex_path}:{number}: {advisory} expires on {expires_text}, "
                f"more than {MAX_DAYS} days after {today_text}"
            )
        if advisory not in used:
            problems.append(
                f"{ex_path}:{number}: {advisory} matches no advisory cargo audit reports for "
                "rust/Cargo.lock: remove it here, from rust/.cargo/audit.toml and from rust/deny.toml"
            )

    for path, ids in ((audit_path, audit), (deny_path, deny)):
        for advisory in first_at:
            if advisory not in ids:
                problems.append(f"{path}: does not ignore {advisory}, which {ex_path} lists")
        for ignored in dict.fromkeys(ids):
            if ignored not in first_at:
                problems.append(
                    f"{path}: ignores {ignored}, which {ex_path} does not list, "
                    "so it has no owner and no expiry"
                )

    if problems:
        return 1, [], problems + [f"{len(problems)} problem(s) with the advisory exceptions in {ex_path}"]
    ok = []
    for _, advisory, owner, expires_text in entries:
        hits = ", ".join(" ".join(hit) for hit in used[advisory])
        ok.append(f"ok  {advisory} {owner} until {expires_text}: {hits}")
    ok.append(
        f"OK  advisory exceptions: {len(entries)}, each owned, in date and matching an advisory of "
        "rust/Cargo.lock; rust/.cargo/audit.toml and rust/deny.toml ignore exactly these"
    )
    return 0, ok, []


def emit(stream, lines, prefix):
    stream.buffer.write("".join(f"{prefix}{line}\n" for line in lines).encode("utf-8"))
    stream.flush()


try:
    code, out_lines, fail_lines = judge(*sys.argv[1:6])
except Refused as err:
    code, out_lines, fail_lines = 2, [], [str(err)]
except Exception as err:
    code, out_lines, fail_lines = 2, [], [f"advisory-exceptions.sh could not judge: {type(err).__name__}: {err}"]
emit(sys.stdout, out_lines, "")
emit(sys.stderr, fail_lines, "FAIL: ")
sys.exit(code)
PYEOF
  exit "$rc"
fi

echo "FAIL: unknown argument '${1:-}'" >&2
exit 2
