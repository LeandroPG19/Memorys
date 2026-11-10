#!/usr/bin/env bash
# Weekly audit of the mutation judge (CI-F6, D-F6r): `quality-gate.sh --study full DIR` on the HEAD of the
# integration branch, in a throwaway worktree of its own, once per HEAD that has not had a verdict yet.
# The systemd timer that calls it (Sundays 02:00) is in docs/gate.md.
#   ./scripts/qg-weekly-audit.sh                run the audit
#   ./scripts/qg-weekly-audit.sh --self-test
# Environment:
#   QG_AUDIT_BRANCH     the integration branch (default main)
#   QG_AUDIT_BASE       the ref the study judges the branch against (QG_BASE of quality-gate.sh). Without it, the
#                       HEAD of the last audit with a verdict: the audit judges what changed since. The first
#                       audit has no last one, so it needs this.
#   QG_AUDIT_MAX_SECS   how long the study may run once it has the turn (default 21600)
# State, under ${XDG_STATE_HOME:-$HOME/.local/state}/memory-industry/qg-audit/:
#   last                `<sha> <verdict> <stamp>` of the last audit with a verdict (0 or 1)
#   <stamp>-<sha12>/    the output of one audit (audit.log), its study.tsv and its verdict
# Exit: 0 adopt (or unchanged), 1 the classes differ or the reference is red, 2 no verdict (the tool refused, died
# or did not finish, or this script could not set up), 75 pesado stayed busy for the whole wait. Only 0 and 1 move `last`.
set -euo pipefail
# CI-K9a: the judge does not use the interop of Windows. On Linux the PATH that WSL adds (/mnt/..., by 9p) goes before anything looks for a command: each one that is not found walks all of it. The same filter as gate_path_without_windows (gate-lock.sh), which this script cannot load first.
if [[ "$OSTYPE" == linux* ]]; then k9_kept=""; IFS=: read -ra k9_dirs <<<"$PATH"; for k9_d in "${k9_dirs[@]}"; do [[ "$k9_d" == /mnt/* ]] || k9_kept="${k9_kept:+$k9_kept:}$k9_d"; done; PATH="$k9_kept"; unset k9_dirs k9_d k9_kept; fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- self-test: the audit runs once per HEAD, under pesado, in a worktree that never outlives it ---
# Each row copies this script into a git repo of its own whose scripts/quality-gate.sh is a stand-in, and runs the
# copy with a stand-in `pesado` first on the PATH. Both stand-ins refuse what the real ones refuse: pesado a flag it
# does not know, a command it was not given and a wait that is not a number, and exits 75 when it is told the turn is
# taken; quality-gate.sh anything but `--study legacy|full DIR` with a DIR that is new or empty. HOME,
# XDG_CACHE_HOME and XDG_STATE_HOME are a directory of its own, so no row reaches the real state or the real pesado.
#   ./scripts/qg-weekly-audit.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  self="${BASH_SOURCE[0]}"
  # CI-J3: the judge hands its children a DATABASE_URL and the rest of its names; each row gives what it needs.
  unset DATABASE_URL QG_AUDIT_BRANCH QG_AUDIT_BASE QG_AUDIT_MAX_SECS FAKE_PESADO_BUSY FAKE_QG_EXIT FAKE_QG_SLEEP
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
  # D-J7b: hermetic. The fixture, the state of the audit and the stand-ins live under one directory removed on exit.
  hermetic="$(mktemp -d)"
  trap 'rm -rf "$hermetic"' EXIT
  mkdir -p "$hermetic/home" "$hermetic/cache" "$hermetic/state" "$hermetic/bin"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache" XDG_STATE_HOME="$hermetic/state"
  export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
  export GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid
  export GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  bin="$hermetic/bin"
  fx="$hermetic/repo"
  state="$hermetic/state/memory-industry/qg-audit"
  pesado_calls="$hermetic/pesado.calls"
  qg_calls="$hermetic/qg.calls"

  cat >"$bin/pesado" <<'STUB'
#!/usr/bin/env bash
# The flags, the `--` and the 75 of the real one; it runs the command like the real one does once it has the turn.
set -uo pipefail
MEM=32G ESPERA=0 NOMBRE=
while getopts "m:c:w:n:" o; do
  case "$o" in
    m) MEM="$OPTARG" ;;
    c) ;;
    w) ESPERA="$OPTARG" ;;
    n) NOMBRE="$OPTARG" ;;
    *) exit 2 ;;
  esac
done
shift $((OPTIND - 1))
[ "${1:-}" = "--" ] && shift
[ $# -ge 1 ] || exit 2
[[ "$ESPERA" =~ ^[0-9]+$ ]] || { echo "flock: invalid timeout value: '$ESPERA'" >&2; exit 64; }
printf 'm=%s|w=%s|n=%s|cmd=%s\n' "$MEM" "$ESPERA" "$NOMBRE" "$*" >>"$FAKE_PESADO_CALLS"
if [ -n "${FAKE_PESADO_BUSY:-}" ]; then
  echo "pesado: ocupado por fixture (pid 1, desde 00:00:00)" >&2
  exit 75
fi
"$@"
STUB
  chmod +x "$bin/pesado"

  mkdir -p "$fx/scripts"
  cat >"$fx/scripts/quality-gate.sh" <<'STUB'
#!/usr/bin/env bash
# `--study legacy|full DIR` with a DIR that is new or empty, else exit 2, like the real one (CI-F6, D-F6p). It says what
# it saw, writes the study.tsv the real one writes, and exits what FAKE_QG_EXIT says; with FAKE_QG_SLEEP it becomes a sleep.
printf 'args=%s|pwd=%s|head=%s|ref=%s|worktrees=%s|db=%s|base=%s\n' "$*" "$PWD" "$(git rev-parse HEAD)" \
  "$(git symbolic-ref -q HEAD || echo detached)" "$(git worktree list | wc -l)" "${DATABASE_URL-unset}" "${QG_BASE-unset}" \
  >>"$FAKE_QG_CALLS"
[[ "${1:-}" == --study ]] || exit 2
case "${2:-}" in legacy | full) ;; *) exit 2 ;; esac
[[ -n "${3:-}" ]] || exit 2
[[ -z "$(ls -A "$3" 2>/dev/null)" ]] || exit 2
mkdir -p "$3/ref" "$3/cover"
printf 'name\tref\tcover\tpass\tk\tref_test_s\tcover_test_s\n' >"$3/study.tsv"
echo "study: 1/1 identical — Q2 may be adopted"
[[ -z "${FAKE_QG_SLEEP:-}" ]] || exec sleep "$FAKE_QG_SLEEP"
exit "${FAKE_QG_EXIT:-0}"
STUB
  cp "$self" "$fx/scripts/qg-weekly-audit.sh"
  chmod +x "$fx/scripts/quality-gate.sh" "$fx/scripts/qg-weekly-audit.sh"
  git -C "$fx" init -q -b main
  git -C "$fx" add scripts
  git -C "$fx" commit -q -m "fixture"
  # commit_new NAME: a commit on the checked-out branch; its sha in $new.
  commit_new() {
    echo "$1" >>"$fx/changes.txt"
    git -C "$fx" add changes.txt
    git -C "$fx" commit -q -m "$1"
    new="$(git -C "$fx" rev-parse HEAD)"
  }
  h1="$(git -C "$fx" rev-parse HEAD)"
  [[ "$(PATH="$bin:$PATH" command -v pesado)" == "$bin/pesado" ]] || self_fail "the stand-in pesado is not the first on the PATH, so a row would take the real turn"

  # run_audit NAME [VAR=value]...: the fixture's copy of the script, in the fixture's environment; its exit in $rc
  # and its output in $hermetic/NAME.out. The DATABASE_URL of every run is the judge's foreign one.
  run_audit() {
    local name="$1"
    shift
    rc=0
    env -u QG_AUDIT_BRANCH -u QG_AUDIT_BASE -u QG_AUDIT_MAX_SECS -u FAKE_PESADO_BUSY -u FAKE_QG_EXIT -u FAKE_QG_SLEEP "$@" \
      PATH="$bin:$PATH" DATABASE_URL='postgresql://ajena:ajena@127.0.0.1:1/ajena' \
      FAKE_PESADO_CALLS="$pesado_calls" FAKE_QG_CALLS="$qg_calls" \
      bash "$fx/scripts/qg-weekly-audit.sh" >"$hermetic/$name.out" 2>&1 || rc=$?
  }
  lines() { if [[ -f "$1" ]]; then wc -l <"$1"; else echo 0; fi; }
  said() { grep -qF -- "$2" "$hermetic/$1.out"; }
  # field FILE N KEY: the value of KEY on the Nth line of a stand-in's log.
  field() { sed -n "$2p" "$1" | tr '|' '\n' | sed -n "s/^$3=//p"; }
  alive() { git -C "$fx" worktree list --porcelain | grep -c '^worktree '; }
  # gone ROW: no throwaway worktree is alive and nothing is left under the work directory.
  gone() {
    [[ "$(alive)" == 1 ]] || self_fail "$1: a throwaway worktree is still alive: $(git -C "$fx" worktree list)"
    [[ -z "$(ls -A "$state/work" 2>/dev/null)" ]] || self_fail "$1: the work directory is not empty: $(ls -A "$state/work")"
  }
  outdirs() { find "$state" -mindepth 1 -maxdepth 1 -type d -name '20*' | wc -l; }
  last_sha() { awk '{print $1}' "$state/last" 2>/dev/null || true; }
  last_verdict() { awk '{print $2}' "$state/last" 2>/dev/null || true; }
  # The dir of the audit of a sha: the last `<stamp>-<sha12>`.
  outdir_of() { find "$state" -mindepth 1 -maxdepth 1 -type d -name "20*-${1:0:12}" | sort | tail -n 1; }

  # F6r.1: a HEAD that already has a verdict is "unchanged": exit 0, nothing takes the turn, `last` is as it was.
  commit_new c2
  h2="$new"
  mkdir -p "$state"
  echo "$h2 0 2026-01-01T000000" >"$state/last"
  cp "$state/last" "$hermetic/last.seed"
  run_audit unchanged
  (( rc == 0 )) || self_fail "F6r.1: a HEAD with a verdict was not exit 0 (exit $rc): $(cat "$hermetic/unchanged.out")"
  said unchanged unchanged || self_fail "F6r.1: the audit of an unchanged HEAD did not say 'unchanged': $(cat "$hermetic/unchanged.out")"
  [[ ! -e "$pesado_calls" && ! -e "$qg_calls" ]] || self_fail "F6r.1: an unchanged HEAD asked for the turn or ran the study: $(cat "$pesado_calls" "$qg_calls" 2>/dev/null)"
  cmp -s "$state/last" "$hermetic/last.seed" || self_fail "F6r.1: an unchanged HEAD rewrote last: $(cat "$state/last")"
  [[ "$(outdirs)" == 0 ]] || self_fail "F6r.1: an unchanged HEAD left an audit directory"
  gone F6r.1

  # F6r.2: a HEAD that is new runs the study, whole, in a detached worktree of that HEAD, under pesado with the wait of
  # an hour, against the HEAD of the last audit, with no DATABASE_URL; the output and the study.tsv are kept, `last`
  # moves to the HEAD with verdict 0, and the worktree is gone. The study saw its own worktree alive (worktrees=2),
  # which is what makes "gone" mean something.
  echo "$h1 0 2026-01-01T000000" >"$state/last"
  run_audit fresh
  (( rc == 0 )) || self_fail "F6r.2: a new HEAD with an identical study was not exit 0 (exit $rc): $(cat "$hermetic/fresh.out")"
  [[ "$(lines "$pesado_calls")" == 1 && "$(lines "$qg_calls")" == 1 ]] \
    || self_fail "F6r.2: a new HEAD did not take the turn once and run the study once: pesado $(lines "$pesado_calls"), study $(lines "$qg_calls")"
  [[ "$(field "$pesado_calls" 1 w)" == 3600 ]] || self_fail "F6r.2: the wait for the turn was '$(field "$pesado_calls" 1 w)', not 3600: $(cat "$pesado_calls")"
  [[ "$(field "$pesado_calls" 1 n)" == qg-auditoria ]] || self_fail "F6r.2: pesado was not named qg-auditoria: $(cat "$pesado_calls")"
  [[ "$(field "$pesado_calls" 1 cmd)" == *" bash scripts/quality-gate.sh --study full $state/work/"*/study ]] \
    || self_fail "F6r.2: pesado did not run 'bash scripts/quality-gate.sh --study full <a new dir>': $(cat "$pesado_calls")"
  [[ "$(field "$qg_calls" 1 args)" == "--study full $state/work/"*/study ]] \
    || self_fail "F6r.2: the study was not --study full over a directory under the work directory: $(cat "$qg_calls")"
  [[ "$(field "$qg_calls" 1 head)" == "$h2" ]] || self_fail "F6r.2: the study ran on '$(field "$qg_calls" 1 head)', not on the HEAD $h2"
  [[ "$(field "$qg_calls" 1 ref)" == detached ]] || self_fail "F6r.2: the worktree of the audit is not detached: $(cat "$qg_calls")"
  [[ "$(field "$qg_calls" 1 pwd)" == "$state/work/"* && "$(field "$qg_calls" 1 pwd)" != "$fx" ]] \
    || self_fail "F6r.2: the study did not run in a worktree of its own: $(cat "$qg_calls")"
  [[ "$(field "$qg_calls" 1 worktrees)" == 2 ]] || self_fail "F6r.2: the study did not see its worktree next to the clone: $(cat "$qg_calls")"
  [[ "$(field "$qg_calls" 1 base)" == "$h1" ]] || self_fail "F6r.2: the study was not judged against the HEAD of the last audit $h1: $(cat "$qg_calls")"
  [[ "$(field "$qg_calls" 1 db)" == unset ]] || self_fail "F6r.2: the study inherited a DATABASE_URL: $(cat "$qg_calls")"
  gone F6r.2
  out2="$(outdir_of "$h2")"
  [[ -n "$out2" && -s "$out2/study.tsv" && -s "$out2/audit.log" && "$(cat "$out2/verdict" 2>/dev/null)" == 0 ]] \
    || self_fail "F6r.2: the study.tsv, the output and the verdict 0 are not kept in <stamp>-<sha12>: '${out2:-none}' $(ls -A "$out2" 2>/dev/null)"
  grep -qF 'study: 1/1 identical' "$out2/audit.log" || self_fail "F6r.2: the kept output is not the study's: $(cat "$out2/audit.log")"
  [[ "$(last_sha)" == "$h2" && "$(last_verdict)" == 0 ]] || self_fail "F6r.2: last did not move to $h2 with verdict 0: $(cat "$state/last")"

  # F6r.3: pesado busy after the wait is exit 75 and says so; `last` does not move, the study does not run, nothing is
  # kept (there was no audit), and the worktree is gone.
  commit_new c3
  h3="$new"
  cp "$state/last" "$hermetic/last.seed"
  run_audit busy FAKE_PESADO_BUSY=1
  (( rc == 75 )) || self_fail "F6r.3: pesado busy was not exit 75 (exit $rc): $(cat "$hermetic/busy.out")"
  said busy ocupado || self_fail "F6r.3: the audit did not say who had the turn: $(cat "$hermetic/busy.out")"
  cmp -s "$state/last" "$hermetic/last.seed" || self_fail "F6r.3: a busy pesado moved last: $(cat "$state/last")"
  [[ "$(lines "$pesado_calls")" == 2 && "$(lines "$qg_calls")" == 1 ]] || self_fail "F6r.3: a busy pesado ran the study anyway: pesado $(lines "$pesado_calls"), study $(lines "$qg_calls")"
  [[ "$(outdirs)" == 1 ]] || self_fail "F6r.3: a busy pesado left an audit directory ($(outdirs) in all)"
  gone F6r.3

  # F6r.4: the study exits 1 (the classes differ, or the reference is red): that is a verdict, so `last` moves with it
  # and the week has its answer; the exit is 1 and the study.tsv is kept. Judged against the HEAD that had the last verdict.
  run_audit red FAKE_QG_EXIT=1
  (( rc == 1 )) || self_fail "F6r.4: a study that exited 1 was not exit 1 (exit $rc): $(cat "$hermetic/red.out")"
  [[ "$(last_sha)" == "$h3" && "$(last_verdict)" == 1 ]] || self_fail "F6r.4: last did not move to $h3 with verdict 1: $(cat "$state/last")"
  out3="$(outdir_of "$h3")"
  [[ -s "$out3/study.tsv" && "$(cat "$out3/verdict" 2>/dev/null)" == 1 ]] || self_fail "F6r.4: the study.tsv and the verdict 1 are not kept: ${out3:-none}"
  [[ "$(field "$qg_calls" 2 base)" == "$h2" ]] || self_fail "F6r.4: the second audit was not judged against $h2, the HEAD of the first: $(cat "$qg_calls")"
  gone F6r.4

  # F6r.5: the study exits 2 (it refused, a tool is missing): no verdict. Exit 2, `last` stays, the output is kept
  # with the verdict 2 so the refusal can be read, and the worktree is gone.
  commit_new c4
  h4="$new"
  cp "$state/last" "$hermetic/last.seed"
  run_audit refused FAKE_QG_EXIT=2
  (( rc == 2 )) || self_fail "F6r.5: a study that exited 2 was not exit 2 (exit $rc): $(cat "$hermetic/refused.out")"
  cmp -s "$state/last" "$hermetic/last.seed" || self_fail "F6r.5: a study with no verdict moved last: $(cat "$state/last")"
  out4="$(outdir_of "$h4")"
  [[ -s "$out4/audit.log" && "$(cat "$out4/verdict" 2>/dev/null)" == 2 ]] || self_fail "F6r.5: the output and the verdict 2 are not kept: ${out4:-none}"
  gone F6r.5

  # F6r.6: a study that does not finish in QG_AUDIT_MAX_SECS is stopped: exit 2, `last` stays, the worktree is gone. A
  # sleep of 20 s against a limit of 1 s: the row is over in a few seconds only if the limit stops it.
  commit_new c5
  cp "$state/last" "$hermetic/last.seed"
  started=$SECONDS
  run_audit slow FAKE_QG_SLEEP=20 QG_AUDIT_MAX_SECS=1
  (( SECONDS - started < 15 )) || self_fail "F6r.6: a study that sleeps 20 s with a limit of 1 s took $((SECONDS - started)) s: the limit does not stop it"
  (( rc == 2 )) || self_fail "F6r.6: a study that did not finish was not exit 2 (exit $rc): $(cat "$hermetic/slow.out")"
  said slow 'did not finish' || self_fail "F6r.6: the audit did not say the study did not finish: $(cat "$hermetic/slow.out")"
  cmp -s "$state/last" "$hermetic/last.seed" || self_fail "F6r.6: a study that did not finish moved last: $(cat "$state/last")"
  gone F6r.6

  # F6r.7: the branch is QG_AUDIT_BRANCH and the base is QG_AUDIT_BASE, over the HEAD of the last audit.
  git -C "$fx" checkout -q -b integ
  commit_new c6
  h6="$new"
  git -C "$fx" checkout -q main
  run_audit branch QG_AUDIT_BRANCH=integ QG_AUDIT_BASE="$h1"
  (( rc == 0 )) || self_fail "F6r.7: the audit of another branch was not exit 0 (exit $rc): $(cat "$hermetic/branch.out")"
  [[ "$(field "$qg_calls" 5 head)" == "$h6" ]] || self_fail "F6r.7: QG_AUDIT_BRANCH=integ did not audit its HEAD $h6: $(cat "$qg_calls")"
  [[ "$(field "$qg_calls" 5 base)" == "$h1" ]] || self_fail "F6r.7: QG_AUDIT_BASE=$h1 was not the base of the study: $(cat "$qg_calls")"
  [[ "$(last_sha)" == "$h6" ]] || self_fail "F6r.7: last did not move to the HEAD of integ: $(cat "$state/last")"
  gone F6r.7

  # F6r.8: what the audit cannot set up is exit 2 before it asks for the turn: a branch that is not there, a base that is
  # not a commit, and a first audit (no last) with no base to judge against. `last` stays and nothing is left behind.
  cp "$state/last" "$hermetic/last.seed"
  before="$(lines "$pesado_calls")"
  run_audit nobranch QG_AUDIT_BRANCH=nada
  (( rc == 2 )) || self_fail "F6r.8: a branch that is not there was not exit 2 (exit $rc): $(cat "$hermetic/nobranch.out")"
  said nobranch nada || self_fail "F6r.8: the audit did not name the branch it did not find: $(cat "$hermetic/nobranch.out")"
  run_audit nobase QG_AUDIT_BASE=tampoco
  (( rc == 2 )) || self_fail "F6r.8: a base that is not a commit was not exit 2 (exit $rc): $(cat "$hermetic/nobase.out")"
  said nobase tampoco || self_fail "F6r.8: the audit did not name the base it did not find: $(cat "$hermetic/nobase.out")"
  rm -f "$state/last"
  run_audit first
  (( rc == 2 )) || self_fail "F6r.8: a first audit with no base was not exit 2 (exit $rc): $(cat "$hermetic/first.out")"
  said first QG_AUDIT_BASE || self_fail "F6r.8: the audit did not say it needs QG_AUDIT_BASE: $(cat "$hermetic/first.out")"
  [[ ! -e "$state/last" ]] || self_fail "F6r.8: an audit that could not start wrote last"
  [[ "$(lines "$pesado_calls")" == "$before" ]] || self_fail "F6r.8: an audit that could not start asked for the turn"
  gone F6r.8

  echo "OK  self-test: qg-weekly-audit skips a HEAD that has a verdict, runs a new one under pesado with a wait of an hour in a detached worktree that never outlives it, moves last only with a verdict and without a DATABASE_URL"
  exit 0
fi
# --- self-test end ---

# The study never touches a database, so the one the judge's shell may hold is not handed down.
unset DATABASE_URL
fail() { echo "qg-audit: FAIL: $*" >&2; exit 2; }

branch="${QG_AUDIT_BRANCH:-main}"
state="${XDG_STATE_HOME:-$HOME/.local/state}/memory-industry/qg-audit"
last="$state/last"
mkdir -p "$state/work" || fail "cannot create $state"

head="$(git -C "$ROOT" rev-parse --verify --quiet "$branch^{commit}")" || fail "the branch '$branch' is not in $ROOT"
last_sha="" last_verdict="" last_stamp=""
if [[ -f "$last" ]]; then read -r last_sha last_verdict last_stamp <"$last" || true; fi
if [[ "$last_sha" == "$head" ]]; then
  echo "qg-audit: unchanged: $branch is at ${head:0:12}, audited on ${last_stamp:-?} with verdict ${last_verdict:-?}"
  exit 0
fi

# What the study judges the branch against: the base it is told, else the HEAD of the last audit with a verdict.
base_ref="${QG_AUDIT_BASE:-$last_sha}"
[[ -n "$base_ref" ]] || fail "the first audit has no earlier one to judge against: set QG_AUDIT_BASE to a ref"
base="$(git -C "$ROOT" rev-parse --verify --quiet "$base_ref^{commit}")" || fail "the base '$base_ref' is not a commit of $ROOT"

stamp="$(date +%Y-%m-%dT%H%M%S)"
tag="$stamp-${head:0:12}"
work="$state/work/$tag"
tree="$work/tree"
study="$work/study"
log="$work/audit.log"
final="" created=""
# Whatever happens, the worktree goes, and an exit that no branch of this script chose is no verdict.
cleanup() {
  local rc=$?
  trap - EXIT
  if [[ -n "$created" ]]; then
    git -C "$ROOT" worktree remove --force "$tree" >/dev/null 2>&1 || echo "qg-audit: could not remove the worktree $tree" >&2
    git -C "$ROOT" worktree prune >/dev/null 2>&1 || true
  fi
  rm -rf "$work"
  [[ -z "$final" ]] || rc="$final"
  [[ -n "$final" || "$rc" == 0 ]] || rc=2
  exit "$rc"
}
finish() { final="$1"; exit "$1"; }
trap cleanup EXIT
trap 'exit 143' TERM INT HUP

mkdir -p "$work" || fail "cannot create $work"
git -C "$ROOT" worktree add --detach --quiet "$tree" "$head" >/dev/null || fail "cannot make the worktree of ${head:0:12}"
created=1

echo "qg-audit: $branch at ${head:0:12}, judged against ${base:0:12}"
max_secs="${QG_AUDIT_MAX_SECS:-21600}"
rc=0
(
  cd "$tree"
  export QG_BASE="$base"
  pesado -m 36G -w 3600 -n qg-auditoria -- \
    timeout --kill-after=120 "$max_secs" bash scripts/quality-gate.sh --study full "$study"
) >"$log" 2>&1 || rc=$?
tail -n 20 "$log"

if (( rc == 75 )); then
  echo "qg-audit: pesado stayed busy for the whole wait: no audit, last unchanged"
  finish 75
fi
verdict=2
case "$rc" in
  0 | 1) verdict="$rc" ;;
  124) echo "qg-audit: the study did not finish in $max_secs s: no verdict" ;;
  *) echo "qg-audit: the study exited $rc: no verdict" ;;
esac

out="$state/$tag"
mkdir -p "$out"
cp "$log" "$out/audit.log"
[[ ! -f "$study/study.tsv" ]] || cp "$study/study.tsv" "$out/study.tsv"
echo "$verdict" >"$out/verdict"
if (( verdict < 2 )); then
  echo "${head} $verdict $stamp" >"$last.new" && mv -f "$last.new" "$last"
fi
echo "qg-audit: verdict $verdict, kept in $out"
finish "$verdict"
