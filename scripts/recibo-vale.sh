#!/usr/bin/env bash
# Does the receipt of merge-gate.sh still hold for what is in this tree? (CI-E29, D-E29h, D-E29i)
#
#   bash scripts/recibo-vale.sh [root]                  0 holds · 1 does not hold · 2 could not check
#   bash scripts/recibo-vale.sh --judge-fingerprint [root]
#   bash scripts/recibo-vale.sh --is-doc PATH...        0 if every PATH is documentation; 1 with one line per PATH that is not
#   bash scripts/recibo-vale.sh --dirty [root]          the paths that differ from HEAD and are not documentation
#   bash scripts/recibo-vale.sh --write-pointer [root]  .gate/ultima-corrida.json for the stop hook, only if the receipt holds
#   bash scripts/recibo-vale.sh --self-test
#
# Pass 1 of CI-E29 (tests: written): only the self-test below exists. The rest of the script is a body that
# says "not implemented" and exits 2, so every row of the self-test is red for what the script does not do
# and not because the file is missing. Pass 2 writes the script after the closing marker and leaves the
# region as it is.
set -euo pipefail
# CI-K9a: the judge does not use the interop of Windows. On Linux the PATH that WSL adds (/mnt/..., by 9p) goes before anything looks for a command: each one that is not found walks all of it. The same filter as gate_path_without_windows (gate-lock.sh), which this script cannot load first.
if [[ "$OSTYPE" == linux* ]]; then k9_kept=""; IFS=: read -ra k9_dirs <<<"$PATH"; for k9_d in "${k9_dirs[@]}"; do [[ "$k9_d" == /mnt/* ]] || k9_kept="${k9_kept:+$k9_kept:}$k9_d"; done; PATH="$k9_kept"; unset k9_dirs k9_d k9_kept; fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# --- self-test: every verdict against the repo that has to give it ------------------------------------------
# Each row builds a throwaway git repo with a copy of this script, of gate-lock.sh and of a docs-only list of
# its own (the content of D-E29j), and a HOME of its own where the receipts live
# ($HOME/.cache/cuba-gate/receipts/<sha>, the four lines merge-gate.sh writes and .started for a run that did not
# pass). Nothing here reads the real receipts or the real list.
#   ./scripts/recibo-vale.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  # D-J7b (A2): this self-test is hermetic. HOME and XDG_CACHE_HOME are a directory of its own,
  # removed on exit, so a row that does not give them one never reaches the real cache.
  hermetic="$(mktemp -d)"
  mkdir -p "$hermetic/home" "$hermetic/cache"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp" "$hermetic"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  # git runs with a scratch global config, so nobody's signing or hooks settings reach the fixture repos.
  export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$tmp/gitconfig"
  : >"$GIT_CONFIG_GLOBAL"
  git config --global user.name fixture
  git config --global user.email fixture@example.invalid
  git config --global init.defaultBranch main
  git config --global core.autocrlf false
  git config --global advice.detachedHead false

  # The list of D-E29j, as a fixture: what is documentation, and the three kinds of `.md` and path that are not.
  list_lines=('backlog/*.md' 'docs/*.md' '!docs/gate.md' '!docs/despliegue-lan.md' '!docs/visita-pc-madre.md' '!*rust-toolchain*')

  # mkrepo NAME [LIST_LINE]: a repo with the files the rows use, in one commit; LIST_LINE is one more line of its list.
  mkrepo() {
    local r="$tmp/$1"
    mkdir -p "$r/scripts" "$r/rust/src" "$r/backlog" "$r/docs"
    cp "$ROOT/scripts/recibo-vale.sh" "$ROOT/scripts/gate-lock.sh" "$r/scripts/"
    { printf '%s\n' '# fixture list' "${list_lines[@]}"; [[ -z "${2:-}" ]] || printf '%s\n' "$2"; } >"$r/scripts/recibo-lista-blanca.txt"
    echo 'fn main() {}' >"$r/rust/src/x.rs"
    echo a >"$r/backlog/a.md"
    echo gate >"$r/docs/gate.md"
    echo notes >"$r/docs/notes.md"
    echo readme >"$r/README.md"
    printf '.gate/\n' >"$r/.gitignore"
    git -C "$r" init -q
    git -C "$r" add -A
    git -C "$r" commit -q -m c0
  }
  head_of() { git -C "$tmp/$1" rev-parse HEAD; }
  # commit_of NAME FILE...: appends a line to each FILE (or makes it) and commits them.
  commit_of() {
    local name="$1" file
    shift
    for file in "$@"; do
      mkdir -p "$(dirname "$tmp/$name/$file")"
      echo change >>"$tmp/$name/$file"
    done
    git -C "$tmp/$name" add -A
    git -C "$tmp/$name" commit -q -m "edit $*"
  }
  # receipt_text SHA: the four lines gate_receipt_lines writes, written here by hand.
  receipt_text() {
    printf 'local-gate: MERGE GATE PASSED %s\n' "$1"
    printf 'local-gate-date: 2026-11-10T12:00:00Z\n'
    printf 'local-gate-summary: mutants caught=9 missed=1 timeout=0 unviable=0 kill_rate=0.900 min=0.800\n'
    printf 'local-gate-summary: MERGE GATE PASSED — safe to merge (local CI 100%%)\n'
  }
  # give_receipt NAME SHA [SAID]: the file receipts/SHA of the HOME of NAME; SAID is the sha its first line names.
  give_receipt() {
    local dir="$tmp/$1.home/.cache/cuba-gate/receipts"
    mkdir -p "$dir"
    receipt_text "${3:-$2}" >"$dir/$2"
  }
  # give_mark NAME SHA: the mark of a run on SHA that did not end green.
  give_mark() {
    local dir="$tmp/$1.home/.cache/cuba-gate/receipts"
    mkdir -p "$dir"
    printf 'local-gate: STARTED %s 2026-11-10T12:30:00Z pid=1\n' "$2" >"$dir/$2.started"
  }
  # vale NAME ARGS...: the copy of the script inside the repo NAME, run from its root with the HOME of NAME. Its
  # exit lands in $rc and what it said, stdout and stderr, in NAME.out.
  vale() {
    local name="$1"
    shift
    mkdir -p "$tmp/$name.home"
    rc=0
    (cd "$tmp/$name" && HOME="$tmp/$name.home" XDG_CACHE_HOME="$tmp/$name.home/xdg" timeout -k 5 60 "$BASH" scripts/recibo-vale.sh "$@") >"$tmp/$name.out" 2>&1 || rc=$?
    (( rc != 124 )) || self_fail "$name: the script was still running after 60 s: $(tail -n 5 "$tmp/$name.out")"
  }
  # says NAME CODE TEXT: the last vale of NAME exited CODE and its output holds TEXT.
  says() {
    local name="$1" want="$2" text="$3"
    (( rc == want )) || self_fail "$name: exited $rc, not $want: $(cat "$tmp/$name.out")"
    grep -qF -- "$text" "$tmp/$name.out" || self_fail "$name: did not say '$text': $(cat "$tmp/$name.out")"
  }
  # field FILE KEY: the JSON value of KEY, as JSON (so a string and a number are told apart).
  field() { python3 -I -c 'import json, sys; print(json.dumps(json.load(open(sys.argv[1]))[sys.argv[2]]))' "$1" "$2"; }

  # E29b.r1: the receipt of HEAD over a clean tree holds, and says which commit it is.
  mkrepo r1
  c="$(head_of r1)"
  give_receipt r1 "$c"
  vale r1
  says r1 0 "holds: merge-gate.sh passed on ${c:0:12}"

  # E29b.r2: a commit that only touches backlog on top of the judged commit leaves its receipt valid.
  mkrepo r2
  c="$(head_of r2)"
  give_receipt r2 "$c"
  commit_of r2 backlog/a.md
  vale r2
  says r2 0 "holds by the docs-only list: 1 paths since ${c:0:12}"

  # E29b.r3: documentation not committed, edited and untracked, counts the same: two paths.
  mkrepo r3
  c="$(head_of r3)"
  give_receipt r3 "$c"
  echo edited >>"$tmp/r3/backlog/a.md"
  echo new >"$tmp/r3/backlog/b.md"
  vale r3
  says r3 0 "holds by the docs-only list: 2 paths since ${c:0:12}"

  # E29b.r4: code changed since the receipt does not hold, and the path is named.
  mkrepo r4
  c="$(head_of r4)"
  give_receipt r4 "$c"
  commit_of r4 rust/src/x.rs
  vale r4
  says r4 1 "rust/src/x.rs is under rust/"

  # E29b.r5: the floor goes before the list. This list says scripts/* and an untracked scripts/nuevo.sh is still the judge.
  mkrepo r5 'scripts/*'
  c="$(head_of r5)"
  give_receipt r5 "$c"
  printf '#!/bin/sh\n' >"$tmp/r5/scripts/nuevo.sh"
  vale r5
  says r5 1 "scripts/nuevo.sh is under scripts/: the judge is never documentation"

  # E29b.r6: changing the list invalidates the receipt, even when the same commit edits a file the new list would call documentation.
  mkrepo r6
  c="$(head_of r6)"
  give_receipt r6 "$c"
  echo 'README.md' >>"$tmp/r6/scripts/recibo-lista-blanca.txt"
  echo more >>"$tmp/r6/README.md"
  git -C "$tmp/r6" add -A
  git -C "$tmp/r6" commit -q -m "README.md is documentation now"
  vale r6
  says r6 1 "the docs-only list changed since ${c:0:12}"

  # E29b.r7: a `.md` the judge reads is not documentation (docs/gate.md is negated by the list; CHANGELOG.md is not on it).
  mkrepo r7
  c="$(head_of r7)"
  give_receipt r7 "$c"
  commit_of r7 docs/gate.md
  vale r7
  says r7 1 "docs/gate.md is not documentation"
  git -C "$tmp/r7" reset -q --hard "$c"
  commit_of r7 CHANGELOG.md
  vale r7
  says r7 1 "CHANGELOG.md"

  # E29b.r8: a rust-toolchain under backlog is a file the judge's walk of the tree would find: negated by the list.
  mkrepo r8
  c="$(head_of r8)"
  give_receipt r8 "$c"
  commit_of r8 backlog/rust-toolchain.md
  vale r8
  says r8 1 "backlog/rust-toolchain.md"

  # E29b.r9: a red run after the receipt, on a commit with the same code, wins over the receipt.
  mkrepo r9
  c="$(head_of r9)"
  give_receipt r9 "$c"
  commit_of r9 backlog/a.md
  d="$(head_of r9)"
  give_mark r9 "$d"
  commit_of r9 backlog/a.md
  vale r9
  says r9 1 "the last run on ${d:0:12} did not pass"

  # E29b.r10: a receipt of a commit that is not on the first-parent line of HEAD is not used. First a commit of
  # another branch that HEAD never merged, then the merge that makes it a second parent: an ancestor, and still not used.
  mkrepo r10
  git -C "$tmp/r10" checkout -q -b side
  commit_of r10 backlog/s.md
  s="$(head_of r10)"
  git -C "$tmp/r10" checkout -q main
  commit_of r10 backlog/m.md
  give_receipt r10 "$s"
  vale r10
  says r10 1 "no receipt for HEAD or for its last 200 first-parent ancestors"
  git -C "$tmp/r10" merge -q --no-ff side -m "merge side"
  git -C "$tmp/r10" merge-base --is-ancestor "$s" HEAD || self_fail "r10: the fixture's merge did not make the receipt's commit an ancestor of HEAD, so it proves nothing"
  vale r10
  says r10 1 "no receipt for HEAD or for its last 200 first-parent ancestors"

  # E29b.r11: a file named after HEAD whose first line names another commit is not a receipt of HEAD.
  mkrepo r11
  c="$(head_of r11)"
  give_receipt r11 "$c" "$(printf '%040d' 1)"
  vale r11
  says r11 1 "is not in the shape merge-gate.sh writes"

  # E29b.r12: outside a git repository there is nothing to compare.
  mkrepo r12
  mkdir "$tmp/nogit"
  vale r12 "$tmp/nogit"
  says r12 2 "could not check"

  # E29b.r13: is-doc says, for each path, whether it is documentation and, if not, why: the list negates it, it is under
  # scripts/, under rust/, or it is not on the list.
  mkrepo r13
  vale r13 --is-doc backlog/a.md docs/notes.md
  (( rc == 0 )) || self_fail "r13: two documentation paths exited $rc, not 0: $(cat "$tmp/r13.out")"
  vale r13 --is-doc docs/gate.md scripts/x.sh rust/y.rs README.md
  (( rc == 1 )) || self_fail "r13: four paths that are not documentation exited $rc, not 1: $(cat "$tmp/r13.out")"
  mapfile -t lines < <(grep -v '^$' "$tmp/r13.out" || true)
  (( ${#lines[@]} == 4 )) || self_fail "r13: want one line for each of the four paths, got ${#lines[@]}: $(cat "$tmp/r13.out")"
  # One line for each path, in the order they were given, each with the words of its reason.
  line_says() {
    local line="$1" word
    shift
    for word in "$@"; do
      [[ "$line" == *"$word"* ]] || self_fail "r13: the line '$line' lacks '$word': $(cat "$tmp/r13.out")"
    done
  }
  line_says "${lines[0]}" docs/gate.md "not documentation" list
  line_says "${lines[1]}" scripts/x.sh "under scripts/"
  line_says "${lines[2]}" rust/y.rs "under rust/"
  line_says "${lines[3]}" README.md "not documentation" list
  motive_negated="${lines[0]#*docs/gate.md}"
  motive_missing="${lines[3]#*README.md}"
  [[ "$motive_negated" != "$motive_missing" ]] \
    || self_fail "r13: a path the list negates and one it does not name get the same motive ('$motive_negated'), so the line does not say which of the two it is"

  # E29b.r14: the fingerprint of the judge follows scripts/, tracked or not, and not the documentation.
  mkrepo r14
  vale r14 --judge-fingerprint
  (( rc == 0 )) || self_fail "r14: --judge-fingerprint exited $rc: $(cat "$tmp/r14.out")"
  h0="$(tr -d '\n' <"$tmp/r14.out")"
  [[ "$h0" =~ ^[0-9a-f]{64}$ ]] || self_fail "r14: the fingerprint is not 64 hex digits: '$h0'"
  echo edited >>"$tmp/r14/backlog/a.md"
  vale r14 --judge-fingerprint
  [[ "$(tr -d '\n' <"$tmp/r14.out")" == "$h0" ]] || self_fail "r14: editing a documentation file changed the fingerprint of the judge: $(cat "$tmp/r14.out")"
  echo '# edited' >>"$tmp/r14/scripts/gate-lock.sh"
  vale r14 --judge-fingerprint
  [[ "$(tr -d '\n' <"$tmp/r14.out")" != "$h0" ]] || self_fail "r14: editing a tracked file of scripts/ left the fingerprint as it was"
  git -C "$tmp/r14" checkout -q -- scripts/gate-lock.sh
  vale r14 --judge-fingerprint
  [[ "$(tr -d '\n' <"$tmp/r14.out")" == "$h0" ]] || self_fail "r14: putting the tracked file back did not give the first fingerprint back: $(cat "$tmp/r14.out")"
  printf '#!/bin/sh\n' >"$tmp/r14/scripts/n.sh"
  vale r14 --judge-fingerprint
  [[ "$(tr -d '\n' <"$tmp/r14.out")" != "$h0" ]] || self_fail "r14: an untracked scripts/n.sh left the fingerprint as it was"

  # E29b.r15: dirty gives the paths that differ from HEAD and are not documentation, tracked or not, one per line.
  mkrepo r15
  echo edited >>"$tmp/r15/backlog/a.md"
  vale r15 --dirty
  [[ -z "$(cat "$tmp/r15.out")" ]] || self_fail "r15: a tree that only has documentation edited listed paths: $(cat "$tmp/r15.out")"
  echo edited >>"$tmp/r15/rust/src/x.rs"
  printf '#!/bin/sh\n' >"$tmp/r15/scripts/n.sh"
  vale r15 --dirty
  [[ "$(sort "$tmp/r15.out")" == "$(printf '%s\n' rust/src/x.rs scripts/n.sh)" ]] \
    || self_fail "r15: want rust/src/x.rs and scripts/n.sh and not backlog/a.md, got: $(cat "$tmp/r15.out")"

  # E29b.r16: write-pointer writes the file the stop hook reads, from a receipt that holds, and nothing when it does not.
  mkrepo r16
  c="$(head_of r16)"
  give_receipt r16 "$c"
  commit_of r16 backlog/a.md
  vale r16 --write-pointer
  (( rc == 0 )) || self_fail "r16: --write-pointer over a receipt that holds exited $rc: $(cat "$tmp/r16.out")"
  pointer="$tmp/r16/.gate/ultima-corrida.json"
  [[ -f "$pointer" ]] || self_fail "r16: no .gate/ultima-corrida.json was written: $(cat "$tmp/r16.out")"
  vale r16 --judge-fingerprint
  fingerprint="$(tr -d '\n' <"$tmp/r16.out")"
  for pair in 'modo|"todo"' 'salida|0' "head|\"$c\"" 'formula|2' "juez_sha256|\"$fingerprint\"" 'diff_sha256|"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"'; do
    [[ "$(field "$pointer" "${pair%%|*}")" == "${pair#*|}" ]] \
      || self_fail "r16: the key ${pair%%|*} of the pointer is $(field "$pointer" "${pair%%|*}"), not ${pair#*|}: $(cat "$pointer")"
  done
  mkrepo r16b
  c="$(head_of r16b)"
  give_receipt r16b "$c"
  commit_of r16b rust/src/x.rs
  mkdir "$tmp/r16b/.gate"
  printf '{"previo": true}\n' >"$tmp/r16b/.gate/ultima-corrida.json"
  cp "$tmp/r16b/.gate/ultima-corrida.json" "$tmp/r16b.before"
  vale r16b --write-pointer
  (( rc == 1 )) || self_fail "r16: --write-pointer over code changed since the receipt exited $rc, not 1: $(cat "$tmp/r16b.out")"
  cmp -s "$tmp/r16b/.gate/ultima-corrida.json" "$tmp/r16b.before" \
    || self_fail "r16: a receipt that does not hold rewrote the pointer that was there: $(cat "$tmp/r16b/.gate/ultima-corrida.json")"

  echo "OK  self-test: the receipt of HEAD holds over a clean tree; a commit, an edit or a new file of documentation"
  echo "    on top of the judged commit leave it valid and say how many paths and since which commit; code under"
  echo "    rust/, anything under scripts/ whatever the list says, a change of the list, a .md the judge reads and a"
  echo "    rust-toolchain in backlog do not; a red run after the receipt wins over it; a receipt of a commit outside the"
  echo "    first-parent line of HEAD, or one that names another commit inside, is not used; outside git it says it could not"
  echo "    check; is-doc says why a path is not documentation; the fingerprint of the judge follows scripts/ and not the"
  echo "    documentation; dirty lists what is not documentation; write-pointer writes the file of the stop hook only when the receipt holds"
  exit 0
fi
# --- self-test end ---

echo "recibo-vale.sh: not implemented (CI-E29 pass 1: only --self-test exists)" >&2
exit 2
