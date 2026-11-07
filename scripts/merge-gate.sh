#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# One gate at a time, for the whole of this one: run-all-tests.sh is only its
# first half, and everything after it (clippy and tests under --features docs,
# deny, audit, codigo-muerto, crap-gate, mutants-gate) builds in the same target
# directory under the same cargo lock. Taken before anything else happens, so
# a second gate is refused before it has checked, backed up or built anything.
# run-all-tests.sh, launched below, is this gate's own child and inherits the
# lock through CUBA_GATE_LOCK_OWNER instead of refusing it.
#   ./scripts/run-all-tests.sh --self-test   (its fixtures drive this script too)
# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"
# shellcheck source=scripts/gate-cluster.sh
source "$ROOT/scripts/gate-cluster.sh"

# --- self-test: the exit file against the fixtures that have to stop it ------
# ~/.cache/cuba-gate/run.exit is what gets read when the session that launched
# a gate is gone. It used to be written by run-all-tests.sh, the first half of
# this gate, so a run whose tests passed said 0 there while deny, audit,
# codigo-muerto, crap-gate or mutants-gate could still fail; and a gate killed
# halfway (SIGKILL under memory pressure, twice on 2026-09-23) runs no trap, so
# the file kept whatever an earlier run had left in it. Each fixture runs a
# COPY of this script in a throwaway tree whose every step is a stand-in
# (pg_isready, cargo, node, run-all-tests.sh and the gate scripts after
# it), with HOME inside the tree: nothing here touches the cluster, the real
# lock or cargo. The exit file of every tree starts at 0, as an earlier green
# gate would have left it, which is exactly what a gate must not leave behind
# unless it is its own verdict.
#   ./scripts/merge-gate.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  # CI-F2 (D-F2i): the lane of a step is not inherited (JUDGE_ENVIRONMENT). The rows that make lanes
  # set GATE_LANE themselves, through the gate.
  # CI-J5: the model names are the judge's (JUDGE_ENVIRONMENT). The rows that read them set them.
  # F2b-fix: the root of the clusters is the judge's too; a row that needs one sets it, or takes the stand-in's default.
  unset GATE_LANE GATE_CLUSTER_ROOT ONNX_MODEL_PATH ORT_DYLIB_PATH CUBA_RERANKER_PATH CUBA_NLI_PATH CUBA_EMBEDDING_DIM \
    CUBA_EMBED_MODEL CUBA_POOLING
  # D-J7b (A2): this self-test is hermetic. HOME and XDG_CACHE_HOME are a directory of its own,
  # removed on exit, so a row that does not give them one never reaches the real cache.
  hermetic="$(mktemp -d)"
  mkdir -p "$hermetic/home" "$hermetic/cache"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache"
  tmp="$(mktemp -d)"
  sleep 300 &
  live=$!
  trap 'kill "$live" 2>/dev/null || true; rm -rf "$tmp" "$hermetic"' EXIT
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }

  # fake_tree NAME RUN_ALL_TESTS_BODY
  fake_tree() {
    local t="$tmp/$1" s
    mkdir -p "$t/scripts" "$t/rust" "$t/bin" "$t/home/.cache/cuba-gate"
    cp "$ROOT/scripts/merge-gate.sh" "$ROOT/scripts/gate-lock.sh" "$t/scripts/"
    # CI-F3.3b-2: the steps run through the planner (gate-dag.py) from the table of gate-dag.txt, which
    # reads gate_history.py beside it, so the stand-in tree carries all three.
    cp "$ROOT/scripts/gate-dag.py" "$ROOT/scripts/gate_history.py" "$ROOT/scripts/gate-dag.txt" "$t/scripts/"
    # CI-F2 (F2b-1): the library of the judge's lanes is a stand-in with the same functions and no
    # Postgres. The rows of this self-test prove the wiring of merge-gate.sh (what each step is given,
    # and in what order its lanes are made and taken down); the library's own rows are in
    # gate-cluster.sh --self-test. Every call goes in the steps, in order.
    cat >"$t/scripts/gate-cluster.sh" <<'FAKE'
GATE_CLUSTER_ROOT="${GATE_CLUSTER_ROOT:-$HOME/cluster-root}"
GATE_CLUSTER_URL=""
GATE_PG_BINDIR="${GATE_PG_BINDIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/pgbin}"
gate_cluster_check() {
  [ -x "$GATE_PG_BINDIR/initdb" ] || {
    echo "FAIL: $GATE_PG_BINDIR/initdb is not there: the judge's Postgres is not installed (stand-in). docs/gate.md, The judge's servers, says how to install it." >&2
    exit 2
  }
  echo check >>"$HOME/steps"
  echo "OK  postgres 18.6 + pgvector 0.8.7 (stand-in)"
}
gate_cluster_sweep() { echo sweep >>"$HOME/steps"; }
gate_cluster_up() {
  echo "up $1" >>"$HOME/steps"
  touch "$HOME/lanes.open"
  GATE_CLUSTER_URL="postgresql://cuba:fakepw@${GATE_CLUSTER_ROOT//\//%2F}%2F$1.fk0000:5432/brain"
  echo "OK  cluster $1 up in 1 ms (stand-in)"
}
gate_cluster_down() { echo "down $1" >>"$HOME/steps"; rm -f "$HOME/lanes.open"; echo "OK  cluster $1 down"; }
gate_cluster_down_all() {
  [ -e "$HOME/lanes.open" ] || return 0
  echo down-all >>"$HOME/steps"
  rm -f "$HOME/lanes.open"
}
FAKE
    printf '#!/bin/sh\n%s\n' "$2" >"$t/scripts/run-all-tests.sh"
    for s in codigo-muerto crap-gate; do
      printf '#!/bin/sh\necho %s >>"$HOME/steps"\necho "${DATABASE_URL-unset}" >>"$HOME/%s.urls"\necho "${GATE_LANE-unset} ${DATABASE_URL-unset} ${GATE_SERVER_SYSTEM_ID-unset}" >>"$HOME/%s.lanes"\nexit 0\n' "$s" "$s" "$s" >"$t/scripts/$s.sh"
    done
    # CI-F1: codigo-muerto alone takes FAKE_CODIGO_MUERTO_EXIT as its verdict, so a row can
    # make the cheap step red (D-F2a). The loop above keeps crap-gate as it was.
    printf '%s\n' '#!/bin/sh' 'echo codigo-muerto >>"$HOME/steps"' \
      'exit "${FAKE_CODIGO_MUERTO_EXIT:-0}"' >"$t/scripts/codigo-muerto.sh"
    # CI-F3.3b-2: the planner's red row makes crap-gate the red step: FAKE_CRAP_EXIT is its verdict.
    printf '%s\n' '#!/bin/sh' 'echo crap-gate >>"$HOME/steps"' \
      'echo "${DATABASE_URL-unset}" >>"$HOME/crap-gate.urls"' \
      'echo "${GATE_LANE-unset} ${DATABASE_URL-unset} ${GATE_SERVER_SYSTEM_ID-unset}" >>"$HOME/crap-gate.lanes"' \
      'exit "${FAKE_CRAP_EXIT:-0}"' >"$t/scripts/crap-gate.sh"
    # The gate asks mutants-gate.sh --refuse-targets before any step, so that
    # call is not the mutation step and does not go in the order.
    printf '%s\n' '#!/bin/sh' \
      '[ "${1:-}" = --refuse-targets ] || echo mutants-gate >>"$HOME/steps"' \
      '[ "${1:-}" = --refuse-targets ] || echo "${DATABASE_URL-unset}" >>"$HOME/mutants-gate.urls"' \
      '[ "${1:-}" = --refuse-targets ] || echo "${GATE_LANE-unset} ${DATABASE_URL-unset} ${GATE_SERVER_SYSTEM_ID-unset}" >>"$HOME/mutants-gate.lanes"' \
      'echo "mutants caught=9 missed=1 timeout=0 unviable=0 kill_rate=0.900 min=0.800"' \
      >"$t/scripts/mutants-gate.sh"
    # test-times.sh: what it was called with goes in the order, the log it was
    # told to judge is copied as the gate had written it by then, and
    # FAKE_TIMES_EXIT is its verdict.
    printf '%s\n' '#!/bin/sh' \
      'echo "test-times $*" >>"$HOME/steps"' \
      'echo "${DATABASE_URL-unset}" >>"$HOME/test-times.url"' \
      'log=""; untl=""; prev=""; for a in "$@"; do [ "$prev" = --judge ] && log="$a"; [ "$prev" = --until ] && untl="$a"; prev="$a"; done' \
      'awk -v u="$untl" '"'"'{ print } $0 == u { exit }'"'"' "$log" >"$HOME/times.seen"' \
      'exit "${FAKE_TIMES_EXIT:-0}"' >"$t/scripts/test-times.sh"
    # advisory-exceptions.sh (G6): what it was called with goes in the order, the
    # report it was told to judge is copied as the gate had made it, and
    # FAKE_ADVEX_EXIT is its verdict.
    printf '%s\n' '#!/bin/sh' \
      'echo "advisory-exceptions $*" >>"$HOME/steps"' \
      'prev=""; for a in "$@"; do [ "$prev" = --judge ] && cp "$a" "$HOME/advex.seen"; prev="$a"; done' \
      'exit "${FAKE_ADVEX_EXIT:-0}"' >"$t/scripts/advisory-exceptions.sh"
    # pg_isready: the row picks what it answers on stdout, the line it writes on
    # stderr and its exit code (FAKE_PG_ANSWER, FAKE_PG_EXIT, FAKE_PG_STDERR),
    # and each call leaves its arguments, one per line, in $HOME/pg_isready.args.
    # Unless the row says otherwise it answers as a server on the default port
    # does, and its stderr carries a word that no line of the gate's output may
    # carry: libpq quotes the piece of the URL it cannot read, and that piece
    # can be the password.
    printf '%s\n' '#!/bin/sh' \
      'for a in "$@"; do printf "%s\n" "$a"; done >>"$HOME/pg_isready.args"' \
      'echo "${FAKE_PG_STDERR-pg_isready: stderr noise carrying S3cr3tW2}" >&2' \
      'answer="${FAKE_PG_ANSWER-127.0.0.1:5489 - accepting connections}"' \
      '[ -z "$answer" ] || echo "$answer"' \
      'exit "${FAKE_PG_EXIT:-0}"' >"$t/bin/pg_isready"
    # psql: the identity query of the judge server (gate-lock.sh, D-J1a) reads
    # cluster_name and system_identifier from the maintenance database of the
    # URL. The stand-in answers "<cluster_name>|<system_identifier>" (the row
    # picks them with FAKE_PSQL_CLUSTER_NAME, empty when it says so, and
    # FAKE_PSQL_SYSID); FAKE_PSQL_EXIT and FAKE_PSQL_STDERR make it fail. Each
    # call leaves its arguments in $HOME/psql.args and its PGOPTIONS in
    # $HOME/psql.env. Unless the row says otherwise it answers as a judge server.
    printf '%s\n' '#!/bin/sh' \
      'for a in "$@"; do printf "%s\n" "$a"; done >>"$HOME/psql.args"' \
      'printf "PGOPTIONS=%s\n" "${PGOPTIONS-}" >>"$HOME/psql.env"' \
      'printf "PGPASSWORD=%s\n" "${PGPASSWORD-}" >>"$HOME/psql.env"' \
      'if [ -n "${FAKE_PSQL_EXIT:-}" ]; then echo "${FAKE_PSQL_STDERR:-psql: fake failure}" >&2; exit "$FAKE_PSQL_EXIT"; fi' \
      'lane="$(printf "%s" "$*" | sed -n "s/.*%2F\([a-z][a-z0-9-]*\)\.[A-Za-z0-9]\{6\}:5432.*/\1/p")"' \
      'if [ -n "$lane" ] && [ -z "${FAKE_PSQL_CLUSTER_NAME+x}" ]; then name="memory-industry-gate-$lane"; else name="${FAKE_PSQL_CLUSTER_NAME-memory-industry-gate}"; fi' \
      'echo "$name|${FAKE_PSQL_SYSID-7701731310526959658}"' \
      'exit 0' >"$t/bin/psql"
    # gate_linux_env reads the version of node, and package.json says which one
    # is the oldest the gate accepts.
    # CI-F1: node answers its version to gate_require_node and records any other call in
    # the order of the steps, exiting FAKE_NODE_EXIT (the row sets it to make npm red).
    printf '%s\n' '#!/bin/sh' \
      'case "${1:-}" in --version) echo v20.0.0 ;;' \
      '  *) echo "node $*" >>"$HOME/steps"; exit "${FAKE_NODE_EXIT:-0}" ;; esac' >"$t/bin/node"
    # CI-F1: nproc says how many cores the machine has. The gate takes the cgroup's CPUs
    # under it (D-F1a), so a row that gives a cgroup of 8 under 24 cores expects 8.
    printf '#!/bin/sh\necho "${FAKE_NPROC:-24}"\n' >"$t/bin/nproc"
    # CI-F2 (D-F2c, D-F2k): the judge's Postgres binaries, in a bindir of their own. initdb is a
    # stand-in that only records its call and fails, so a row that must not make a cluster can see
    # that none was made (A3), and the rows that do can see what came next.
    mkdir -p "$t/pgbin"
    printf '%s\n' '#!/bin/sh' 'echo initdb >>"$HOME/initdb.calls"' 'exit 1' >"$t/pgbin/initdb"
    chmod +x "$t/pgbin/initdb"
    printf '{"engines":{"node":">=18"}}\n' >"$t/package.json"
    # cargo fails the one subcommand named in FAIL_AT, and not its --version
    # probe, so the gate gets past "is cargo-deny installed" to the check. Each
    # call leaves its arguments in $HOME/steps, which is how a row reads the
    # order of the steps. cargo audit has two ways to fail that the gate has to
    # tell apart: a database it could not fetch (FAKE_AUDIT_NOFETCH=1, the three
    # lines cargo-audit 0.22.2 printed with no route to github.com) and an
    # advisory it found (FAIL_AT=audit). The report of the exceptions step is a third
    # audit call, `audit --json`: it leaves the directory it ran in (and whether an
    # audit.toml was there to be read from it) in $HOME, prints a marker the
    # stand-in judge copies, and exits FAKE_AUDIT_JSON_EXIT, 1 unless the row says
    # otherwise, which is what the real one does when it finds the excepted advisories.
    printf '%s\n' '#!/bin/sh' \
      'echo "cargo $*" >>"$HOME/steps"' \
      'echo "${DATABASE_URL-unset}" >>"$HOME/cargo.urls"' \
      'echo "${GATE_LANE-unset} ${DATABASE_URL-unset} ${GATE_SERVER_SYSTEM_ID-unset} $*" >>"$HOME/cargo.lanes"' \
      'echo "${CARGO_BUILD_JOBS-unset} $*" >>"$HOME/cargo.jobs"' \
      'echo "${ONNX_MODEL_PATH-unset} ${CUBA_EMBEDDING_DIM-unset} $*" >>"$HOME/cargo.models"' \
      'if [ "$1" = audit ] && [ "${2:-}" = --json ]; then' \
      '  echo "$PWD" >"$HOME/audit_json.cwd"' \
      '  if [ -e .cargo/audit.toml ]; then echo present >"$HOME/audit_json.audit_toml"; fi' \
      '  echo '"'"'{"marker":"g6-fake-report"}'"'" \
      '  [ -z "${FAKE_AUDIT_JSON_STDERR:-}" ] || echo "$FAKE_AUDIT_JSON_STDERR" >&2' \
      '  exit "${FAKE_AUDIT_JSON_EXIT:-1}"' \
      'fi' \
      'if [ "$1" = audit ] && [ "${FAKE_AUDIT_NOFETCH:-}" = 1 ]; then' \
      '  echo "error: couldn'"'"'t fetch advisory database: git operation failed: An IO error occurred when talking to the server" >&2' \
      '  echo "Caused by:" >&2' \
      '  echo "  -> error sending request for url (https://github.com/RustSec/advisory-db.git/info/refs?service=git-upload-pack)" >&2' \
      '  exit 1' \
      'fi' \
      'if [ "$1" = audit ] && [ "$FAIL_AT" = audit ]; then' \
      '  echo "stand-in cargo audit: 1 vulnerability found"' \
      '  exit 1' \
      'fi' \
      'if [ "$1" = "$FAIL_AT" ] && [ "$2" != "--version" ]; then exit 1; fi' \
      'exit 0' >"$t/bin/cargo"
    # timeout, as in GNU coreutils, takes the limit and then the command. It leaves
    # what it was given in $HOME/timeout.args, and FAKE_TIMEOUT_EXPIRES=1 makes
    # it say what coreutils says when the limit is reached (124) without running
    # anything, so no row waits 120 s.
    printf '%s\n' '#!/bin/sh' \
      'echo "$*" >>"$HOME/timeout.args"' \
      'if [ "${FAKE_TIMEOUT_EXPIRES:-}" = 1 ]; then exit 124; fi' \
      'shift' \
      'exec "$@"' >"$t/bin/timeout"
    # docker answers the two calls the readiness fallback makes and records them
    # in $HOME/docker.args. `ps` lists FAKE_DOCKER_NAMES (space separated, one
    # per line) when its publish= filter names FAKE_DOCKER_PORT, nothing for any
    # other port, and exits FAKE_DOCKER_PS_EXIT when that is defined. `exec`
    # answers as a ready Postgres for the names in FAKE_DOCKER_HEALTHY, and as
    # docker does for a container that is not there for any other.
    printf '%s\n' '#!/bin/sh' \
      'echo "$*" >>"$HOME/docker.args"' \
      'case "$1" in' \
      '  ps)' \
      '    [ -z "${FAKE_DOCKER_PS_EXIT+x}" ] || exit "$FAKE_DOCKER_PS_EXIT"' \
      '    port=' \
      '    for a in "$@"; do' \
      '      case "$a" in *publish=*) port="${a#*publish=}" ;; esac' \
      '    done' \
      '    if [ -n "$port" ] && [ "$port" = "${FAKE_DOCKER_PORT:-}" ]; then' \
      '      for n in $FAKE_DOCKER_NAMES; do echo "$n"; done' \
      '    fi' \
      '    exit 0 ;;' \
      '  exec)' \
      '    for n in ${FAKE_DOCKER_HEALTHY:-}; do' \
      '      if [ "$n" = "$2" ]; then echo "/var/run/postgresql:5432 - accepting connections"; exit 0; fi' \
      '    done' \
      '    echo "Error response from daemon: No such container: $2" >&2' \
      '    exit 1 ;;' \
      'esac' \
      'exit 0' >"$t/bin/docker"
    chmod +x "$t"/scripts/*.sh "$t"/bin/*
    printf '0\n' >"$t/home/.cache/cuba-gate/run.exit"
  }
  # run_copy NAME [FAIL_AT [VAR=value ...]]: the copy's exit code lands in $rc.
  # The braces keep bash's own "Killed" job notice, for the fixture that kills
  # the copy, off this script's stderr; everything the copy prints is in
  # NAME.out. The VAR=value pairs reach the copy as they are. DATABASE_URL is
  # never inherited from whoever runs this test, so that no row's verdict
  # depends on the shell it was started from: run_copy starts the copy with the
  # judge's test URL (a string that only the stand-ins read), a VAR=value that
  # names another one replaces it, VAR= defines it empty, and run_copy_as takes
  # the start URL as its first argument, "unset" to leave it out.
  # git's own directory is on the copy's PATH: on Git Bash it is /mingw64/bin,
  # not /usr/bin. It runs with a scratch global config, so nobody's signing or
  # hooks settings reach the fixture repos. PGPORT is not inherited either: it
  # is the port libpq falls back to, and the rows of the readiness fallback
  # that name none have to get the default, not the shell's.
  gitdir="$(dirname "$(command -v git)")"
  export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$tmp/gitconfig"
  : >"$GIT_CONFIG_GLOBAL"
  git config --global user.name fixture
  git config --global user.email fixture@example.invalid
  git config --global init.defaultBranch main
  git config --global core.autocrlf false
  judge_test_url='postgresql://gate:pw0@127.0.0.1:5489/brain'
  run_copy() { run_copy_as "$judge_test_url" "$@"; }
  run_copy_as() {
    local base="$1" name="$2" fail_at="${3:-}"
    local -a start=(DATABASE_URL="$base")
    [[ "$base" != unset ]] || start=()
    shift $(( $# > 2 ? 3 : 2 ))
    rc=0
    { env -u CUBA_GATE_LOCK_OWNER -u CUBA_GATE_EXIT_FILE -u DATABASE_URL -u PGPORT -u CARGO_BUILD_JOBS HOME="$tmp/$name/home" \
        PATH="$tmp/$name/bin:$gitdir:/usr/bin:/bin" SKIP_BACKUP=1 FAIL_AT="$fail_at" ${start[@]+"${start[@]}"} "$@" \
        "$BASH" "$tmp/$name/scripts/merge-gate.sh" >"$tmp/$name.out" 2>&1 || rc=$?; } 2>/dev/null
  }
  exit_file() { cat "$tmp/$1/home/.cache/cuba-gate/run.exit" 2>/dev/null || true; }
  # git_tree NAME RUN_ALL_TESTS_BODY: a fake_tree committed whole, its home
  # ignored the way the real HOME is outside the real tree.
  git_tree() {
    fake_tree "$1" "$2"
    printf 'home/\n' >"$tmp/$1/.gitignore"
    git -C "$tmp/$1" init -q
    git -C "$tmp/$1" add -A
    git -C "$tmp/$1" commit -q -m fixture
  }
  receipt_of() { cat "$tmp/$1/home/.cache/cuba-gate/receipts/$2" 2>/dev/null || true; }
  receipts_of() { ls "$tmp/$1/home/.cache/cuba-gate/receipts" 2>/dev/null || true; }
  # A tree where no receipt may appear, and the run has to say why in a line.
  no_receipt() {
    local what="$1" name="$2"
    (( rc == 0 )) || self_fail "$what exited $rc, and it passed: the receipt is not the verdict: $(cat "$tmp/$name.out")"
    [[ -z "$(receipts_of "$name")" ]] || self_fail "$what left a receipt: $(receipts_of "$name")"
    grep -q '^no receipt: ' "$tmp/$name.out" \
      || self_fail "$what wrote no receipt without saying why: $(cat "$tmp/$name.out")"
  }
  reads_while_running='echo "stand-in run-all-tests: run.exit reads: $(cat "$HOME/.cache/cuba-gate/run.exit")"'

  # The presence anchor: every step passes, so the file ends at 0, and while
  # the first half ran it said the gate was still running. A file that ends at
  # 0 proves nothing on its own: it started there.
  fake_tree green "$reads_while_running"
  run_copy green
  (( rc == 0 )) || self_fail "a gate whose every step passed exited $rc: $(cat "$tmp/green.out")"
  [[ "$(exit_file green)" == 0 ]] || self_fail "a green gate left '$(exit_file green)' in its exit file, not 0"
  grep -q 'run.exit reads: running pid=[0-9]' "$tmp/green.out" \
    || self_fail "while the gate ran, its exit file did not say so: $(grep 'run.exit reads' "$tmp/green.out")"

  # The first half passes and a step after it fails: the file is the gate's.
  fake_tree deny "$reads_while_running"
  run_copy deny deny
  (( rc == 1 )) || self_fail "a gate whose cargo deny failed exited $rc, not 1: $(cat "$tmp/deny.out")"
  [[ "$(exit_file deny)" == 1 ]] \
    || self_fail "a gate that failed at cargo deny, after its tests passed, left '$(exit_file deny)' in its exit file, not 1"

  # Killed in the middle, the way the kernel or Windows kills under memory
  # pressure: no trap runs, and the file must not read as a verdict.
  # CI-F3.3b-2: the steps run one level deeper (the planner starts each row), so the signal names the gate itself,
  # the pid of its lock record, and not the step's parent.
  fake_tree killed 'kill -9 "$(printf "%s\n" "$CUBA_GATE_LOCK_OWNER" | sed -n "s/^pid=\([0-9]*\) .*/\1/p")"; exit 0'
  run_copy killed
  (( rc != 0 )) || self_fail "a gate killed in the middle exited 0"
  [[ "$(exit_file killed)" == "running pid="* ]] \
    || self_fail "a gate killed in the middle left '$(exit_file killed)' in its exit file, not 'running pid=...'"

  # G9: stopped in the middle by a signal that CAN be caught (a timeout, a service
  # manager). Bash runs the EXIT trap for it with the status of the last command, and
  # between two steps that is a 0: found on 2026-10-20 by stopping a gate during cargo
  # test, run.exit read 0 over a run that had finished nothing. The file is what gets read
  # when the session that launched the gate is gone, so a stopped gate has to say it has no
  # verdict: 2, the status this judge keeps for «could not judge», in the file, and no
  # receipt, whatever the tree is. The process itself ends with the signal's status
  # (bash raises it again after the trap), so it is only required not to have passed.
  # Only SIGTERM is held here. SIGHUP takes the same trap, but a lineage started with
  # nohup (the gate itself, the way a long run is launched) has it ignored on entry, bash
  # cannot reset a signal ignored on entry, and a row that passes or fails by how its
  # runner was started is not a row: the first version of this one failed the real gate.
  # One signal, not one per lane: the signal waits for the gate's verdict in its exit trap, and a second one
  # would kill it there (CI-F3.3b-2: the planner runs more than one call of run-all-tests).
  git_tree stopped_term '[ -e "$HOME/signalled" ] || { touch "$HOME/signalled"; kill -TERM "$(printf "%s\n" "$CUBA_GATE_LOCK_OWNER" | sed -n "s/^pid=\([0-9]*\) .*/\1/p")"; }; exit 0'
  run_copy stopped_term
  (( rc != 0 )) || self_fail "G9: a gate stopped by SIGTERM exited 0: $(cat "$tmp/stopped_term.out")"
  [[ "$(exit_file stopped_term)" == 2 ]] \
    || self_fail "G9: a gate stopped by SIGTERM left '$(exit_file stopped_term)' in its exit file, not 2: an unfinished run read as a verdict"
  grep -q 'stopped before its last step' "$tmp/stopped_term.out" \
    || self_fail "G9: a gate stopped by SIGTERM did not say so: $(cat "$tmp/stopped_term.out")"
  [[ -z "$(receipts_of stopped_term)" ]] \
    || self_fail "G9: a gate stopped by SIGTERM over a clean tree left a receipt: $(receipts_of stopped_term)"

  # A second gate, refused by a live one's lock: the file belongs to that one.
  fake_tree refused 'exit 0'
  mkdir -p "$tmp/refused/home/.cache/cuba-gate/lock"
  owner_record "$live" fixture-live >"$tmp/refused/home/.cache/cuba-gate/lock/owner"
  run_copy refused
  grep -q "another gate is running: pid $live" "$tmp/refused.out" \
    || self_fail "the second gate was not refused by the live one (exit $rc): $(cat "$tmp/refused.out")"
  [[ "$(exit_file refused)" == 0 ]] \
    || self_fail "a refused second gate wrote '$(exit_file refused)' into the exit file of the gate that holds the lock"

  # The receipt scripts/release.sh reads instead of running the gate again.
  # The presence anchor: a green gate over a clean tree leaves one for its
  # commit, in the four lines release.sh copies into the tag.
  git_tree receipt 'exit 0'
  sha="$(git -C "$tmp/receipt" rev-parse HEAD)"
  run_copy receipt
  (( rc == 0 )) || self_fail "a green gate over a clean tree exited $rc: $(cat "$tmp/receipt.out")"
  r="$(receipt_of receipt "$sha")"
  [[ "$(sed -n 1p <<<"$r")" == "local-gate: MERGE GATE PASSED $sha" ]] \
    || self_fail "a green gate over a clean tree at $sha left no receipt for it ('$r', receipts: '$(receipts_of receipt)'): $(cat "$tmp/receipt.out")"
  [[ "$(wc -l <<<"$r")" -eq 4 ]] || self_fail "the receipt is not four lines: $r"
  grep -qE '^local-gate-date: [0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$' <<<"$r" \
    || self_fail "the receipt has no UTC date: $r"
  grep -qx 'local-gate-summary: mutants caught=9 missed=1 timeout=0 unviable=0 kill_rate=0.900 min=0.800' <<<"$r" \
    || self_fail "the receipt lost the gate's kill_rate line: $r"
  grep -q '^local-gate-summary: MERGE GATE PASSED' <<<"$r" || self_fail "the receipt lost the gate's verdict line: $r"

  # An untracked file when the gate starts, gone before it ends: what passed is
  # not a commit, and only the look at the start can tell.
  git_tree dirty 'rm -f stray.rs'
  echo x >"$tmp/dirty/stray.rs"
  run_copy dirty
  no_receipt "a gate over an untracked file" dirty

  # A tree dirtied while the gate ran.
  git_tree dirtied 'echo x >stray.rs'
  run_copy dirtied
  no_receipt "a gate whose tree changed while it ran" dirtied

  # HEAD moved while the gate ran: neither commit was judged whole.
  git_tree moved 'git commit -q --allow-empty -m "landed while the gate ran"'
  sha="$(git -C "$tmp/moved" rev-parse HEAD)"
  run_copy moved
  [[ "$(git -C "$tmp/moved" rev-parse HEAD)" != "$sha" ]] || self_fail "the fixture's HEAD never moved, so it proves nothing"
  no_receipt "a gate whose HEAD moved while it ran" moved

  # Exit 0 over a SKIPPED line: release.sh's own judge of the log says no.
  # No colon after the word: local_gate_contract.rs refuses that spelling
  # anywhere in this file, fixtures included.
  git_tree skipped 'echo "SKIPPED (tests that need the NLI model)"'
  run_copy skipped
  no_receipt "a gate that exited 0 over SKIPPED" skipped
  grep -q '^no receipt: .*SKIPPED' "$tmp/skipped.out" \
    || self_fail "the SKIPPED run did not say that was why: $(cat "$tmp/skipped.out")"

  # An earlier pass on this commit, then a run on it that fails: the receipt
  # goes, because the latest verdict on a commit is its verdict.
  git_tree reddened 'exit 0'
  sha="$(git -C "$tmp/reddened" rev-parse HEAD)"
  mkdir -p "$tmp/reddened/home/.cache/cuba-gate/receipts"
  printf 'local-gate: MERGE GATE PASSED %s\n' "$sha" >"$tmp/reddened/home/.cache/cuba-gate/receipts/$sha"
  run_copy reddened deny
  (( rc == 1 )) || self_fail "a gate whose cargo deny failed exited $rc, not 1: $(cat "$tmp/reddened.out")"
  [[ -z "$(receipts_of reddened)" ]] \
    || self_fail "a red gate on $sha left the receipt of an earlier pass on it: $(receipts_of reddened)"

  # The order of the steps, read from $HOME/steps (one line per cargo call, and
  # one for run-all-tests.sh). cargo audit is the only step that judges RustSec
  # advisories, and it refreshes the database it reads, so a judge idle for days
  # used to learn of a new advisory after ~20 min of tests, when 3 s would do. It
  # runs first, under `timeout 120`; a database it cannot refresh is exit 2 and
  # says what is missing, and an advisory it finds is exit 1 with cargo-audit's
  # own lines. Neither lets cargo deny or the tests start. cargo deny is the
  # next step, still before the tests.
  tests_step='echo run-all-tests >>"$HOME/steps"'
  steps_of() { cat "$tmp/$1/home/steps" 2>/dev/null || true; }
  # step_at NAME PATTERN: the line of $HOME/steps where PATTERN first appears, or nothing.
  step_at() {
    local hit
    hit="$(grep -nm1 -- "$2" "$tmp/$1/home/steps" 2>/dev/null || true)"
    printf '%s' "${hit%%:*}"
  }
  # fail_line_with NAME WORD...: some line of the gate's output starts with FAIL:
  # and holds every WORD.
  fail_line_with() {
    local name="$1" line word ok
    shift
    while IFS= read -r line; do
      ok=1
      for word in "$@"; do
        [[ "$line" == *"$word"* ]] || { ok=0; break; }
      done
      if (( ok )); then return 0; fi
    done < <(grep '^FAIL: ' "$tmp/$name.out" | tr -d '\r' || true)
    return 1
  }
  # adv_stopped NAME CODE: the gate stopped with CODE, in its exit file too, and
  # freed its lock.
  adv_stopped() {
    local name="$1" want="$2"
    (( rc == want )) || self_fail "$name: the gate exited $rc, not $want: $(cat "$tmp/$name.out")"
    [[ "$(exit_file "$name")" == "$want" ]] \
      || self_fail "$name: the exit file says '$(exit_file "$name")', not $want"
    [[ ! -e "$tmp/$name/home/.cache/cuba-gate/lock" ]] || self_fail "$name: the gate left its lock behind"
  }
  # adv_never NAME STEP...: no line of $HOME/steps starts with any STEP.
  adv_never() {
    local name="$1" step
    shift
    for step in "$@"; do
      if [[ -n "$(step_at "$name" "^$step")" ]]; then
        self_fail "$name: '$step' ran, and it must not have: $(steps_of "$name")"
      fi
    done
  }

  fake_tree adv_order "$tests_step"
  run_copy adv_order
  (( rc == 0 )) || self_fail "adv_order: a gate whose every step passed exited $rc: $(cat "$tmp/adv_order.out")"
  grep -qxF '120 cargo audit' "$tmp/adv_order/home/timeout.args" 2>/dev/null \
    || self_fail "adv_order: cargo audit was not run as 'timeout 120 cargo audit' (timeout was given: '$(cat "$tmp/adv_order/home/timeout.args" 2>/dev/null || true)')"
  audit_at="$(step_at adv_order '^cargo audit$')"
  deny_at="$(step_at adv_order '^cargo deny check')"
  tests_at="$(step_at adv_order '^run-all-tests$')"
  [[ -n "$audit_at" && -n "$deny_at" && -n "$tests_at" ]] \
    || self_fail "adv_order: a step is missing from the order (audit '$audit_at', deny '$deny_at', tests '$tests_at'): $(steps_of adv_order)"
  (( audit_at < deny_at && deny_at < tests_at )) \
    || self_fail "adv_order: the order is not cargo audit, then cargo deny, then the tests: $(steps_of adv_order)"

  # No route to github.com: cargo-audit fails to refresh its database. The gate
  # does not judge advisories against the copy on disk, and it says which
  # database and where it lives.
  fake_tree adv_nofetch "$tests_step"
  run_copy adv_nofetch "" FAKE_AUDIT_NOFETCH=1
  adv_stopped adv_nofetch 2
  [[ "$(steps_of adv_nofetch)" == "cargo audit" ]] \
    || self_fail "adv_nofetch: cargo was called for more than the audit: $(steps_of adv_nofetch)"
  fail_line_with adv_nofetch 'advisory database' 'https://github.com/RustSec/advisory-db' \
    || self_fail "adv_nofetch: no FAIL line names the advisory database and where it is fetched from: $(cat "$tmp/adv_nofetch.out")"
  if grep -q 'MERGE GATE PASSED' "$tmp/adv_nofetch.out"; then
    self_fail "adv_nofetch: a gate that could not refresh its advisories passed: $(cat "$tmp/adv_nofetch.out")"
  fi

  # A refresh that does not end within the ceiling is the same refusal, and it
  # names the ceiling.
  fake_tree adv_timeout "$tests_step"
  run_copy adv_timeout "" FAKE_TIMEOUT_EXPIRES=1
  adv_stopped adv_timeout 2
  adv_never adv_timeout 'cargo audit' 'cargo deny' 'run-all-tests'
  fail_line_with adv_timeout '120 s' 'advisory database' \
    || self_fail "adv_timeout: no FAIL line names the 120 s ceiling and the advisory database: $(cat "$tmp/adv_timeout.out")"

  # An advisory found is the verdict, not a missing database: exit 1, with what
  # cargo-audit itself said, and nothing after it.
  fake_tree adv_found "$tests_step"
  run_copy adv_found audit
  adv_stopped adv_found 1
  grep -q 'stand-in cargo audit: 1 vulnerability found' "$tmp/adv_found.out" \
    || self_fail "adv_found: the gate did not print the lines of cargo audit: $(cat "$tmp/adv_found.out")"
  adv_never adv_found 'cargo deny check' 'run-all-tests'
  if fail_line_with adv_found 'advisory database'; then
    self_fail "adv_found: an advisory that was found was reported as a database that could not be refreshed: $(cat "$tmp/adv_found.out")"
  fi

  # cargo deny judges licenses, bans and sources, and it is red before the tests.
  fake_tree deny_before_tests "$tests_step"
  run_copy deny_before_tests deny
  (( rc == 1 )) || self_fail "deny_before_tests: a gate whose cargo deny failed exited $rc, not 1: $(cat "$tmp/deny_before_tests.out")"
  audit_at="$(step_at deny_before_tests '^cargo audit$')"
  deny_at="$(step_at deny_before_tests '^cargo deny check')"
  [[ -n "$audit_at" && -n "$deny_at" ]] && (( audit_at < deny_at )) \
    || self_fail "deny_before_tests: cargo deny did not come after cargo audit: $(steps_of deny_before_tests)"
  adv_never deny_before_tests 'run-all-tests'

  # G6: the exceptions of cargo audit are judged right after it and before cargo deny,
  # from a report that second call makes without ignores. It has to run from a directory
  # of its own: rust/.cargo/audit.toml would ignore the very advisories the judge looks
  # for, and every exception would look unused. Its exit does not count (1 is what it
  # does when the exceptions are real), the judge's does, and a refused judge (2) is
  # explained by what the call wrote on stderr.
  fake_tree adv_exc_order "$tests_step"
  mkdir -p "$tmp/adv_exc_order/rust/.cargo" "$tmp/adv_exc_order/.cargo"
  : >"$tmp/adv_exc_order/rust/.cargo/audit.toml"
  : >"$tmp/adv_exc_order/.cargo/audit.toml"
  run_copy adv_exc_order
  (( rc == 0 )) || self_fail "adv_exc_order: a gate whose every step passed exited $rc: $(cat "$tmp/adv_exc_order.out")"
  audit_at="$(step_at adv_exc_order '^cargo audit$')"
  json_at="$(step_at adv_exc_order '^cargo audit --json --no-fetch -f .*rust/Cargo\.lock$')"
  exc_at="$(step_at adv_exc_order '^advisory-exceptions --judge ')"
  deny_at="$(step_at adv_exc_order '^cargo deny check')"
  tests_at="$(step_at adv_exc_order '^run-all-tests$')"
  [[ -n "$audit_at" && -n "$json_at" && -n "$exc_at" && -n "$deny_at" && -n "$tests_at" ]] \
    || self_fail "adv_exc_order: a step is missing (cargo audit '$audit_at', cargo audit --json --no-fetch -f <rust/Cargo.lock> '$json_at', advisory-exceptions --judge '$exc_at', cargo deny '$deny_at', tests '$tests_at'): $(steps_of adv_exc_order)"
  (( audit_at < json_at && json_at < exc_at && exc_at < deny_at && deny_at < tests_at )) \
    || self_fail "adv_exc_order: the order is not cargo audit, cargo audit --json, the exceptions, cargo deny, the tests: $(steps_of adv_exc_order)"
  json_cwd="$(cat "$tmp/adv_exc_order/home/audit_json.cwd" 2>/dev/null || true)"
  [[ -n "$json_cwd" ]] || self_fail "adv_exc_order: cargo audit --json left no directory: $(steps_of adv_exc_order)"
  case "$json_cwd" in
    "$tmp/adv_exc_order" | "$tmp/adv_exc_order/rust" | "$tmp/adv_exc_order/rust/"* | */rust)
      self_fail "adv_exc_order: cargo audit --json ran in '$json_cwd', not in a directory of its own" ;;
  esac
  [[ ! -e "$tmp/adv_exc_order/home/audit_json.audit_toml" ]] \
    || self_fail "adv_exc_order: cargo audit --json ran where an audit.toml was there to ignore advisories ('$json_cwd')"
  grep -q 'g6-fake-report' "$tmp/adv_exc_order/home/advex.seen" 2>/dev/null \
    || self_fail "adv_exc_order: the judge was not handed the report that cargo audit --json made: $(cat "$tmp/adv_exc_order/home/advex.seen" 2>/dev/null || true)"

  # The judge's 1 is the gate's, and the tests never start.
  fake_tree adv_exc_red "$tests_step"
  run_copy adv_exc_red "" FAKE_ADVEX_EXIT=1
  adv_stopped adv_exc_red 1
  [[ -n "$(step_at adv_exc_red '^advisory-exceptions --judge ')" ]] \
    || self_fail "adv_exc_red: the exceptions were not judged: $(steps_of adv_exc_red)"
  adv_never adv_exc_red 'cargo deny check' 'run-all-tests'
  if grep -q 'MERGE GATE PASSED' "$tmp/adv_exc_red.out"; then
    self_fail "adv_exc_red: a gate whose exceptions were red passed: $(cat "$tmp/adv_exc_red.out")"
  fi

  # Its 2 is the gate's too, and says why: what the call that made the report wrote on stderr.
  fake_tree adv_exc_refused "$tests_step"
  run_copy adv_exc_refused "" FAKE_ADVEX_EXIT=2 'FAKE_AUDIT_JSON_STDERR=stand-in audit json: S7derr'
  adv_stopped adv_exc_refused 2
  adv_never adv_exc_refused 'cargo deny check' 'run-all-tests'
  grep -q 'S7derr' "$tmp/adv_exc_refused.out" \
    || self_fail "adv_exc_refused: the gate did not print what cargo audit --json wrote on stderr: $(cat "$tmp/adv_exc_refused.out")"

  # An advisory cargo audit itself found is the verdict, and the exceptions are not judged.
  fake_tree adv_exc_after_audit "$tests_step"
  run_copy adv_exc_after_audit audit
  adv_stopped adv_exc_after_audit 1
  adv_never adv_exc_after_audit 'cargo audit --json' 'advisory-exceptions'

  # cargo audit --json exits 1 when it finds the excepted advisories, and 0 when it finds
  # none: neither is the verdict, so the exceptions are judged either way.
  for json_exit in 0 1; do
    fake_tree "adv_exc_json_exit$json_exit" "$tests_step"
    run_copy "adv_exc_json_exit$json_exit" "" FAKE_AUDIT_JSON_EXIT="$json_exit"
    (( rc == 0 )) || self_fail "adv_exc_json_exit$json_exit: cargo audit --json exited $json_exit and the gate $rc: $(cat "$tmp/adv_exc_json_exit$json_exit.out")"
    [[ -n "$(step_at "adv_exc_json_exit$json_exit" '^advisory-exceptions --judge ')" ]] \
      || self_fail "adv_exc_json_exit$json_exit: the exceptions were not judged: $(steps_of "adv_exc_json_exit$json_exit")"
  done

  # G4/A1: a cargo target inside rust/ is copied whole into every job of the mutation
  # step, the last one of this gate, and it used to be refused only there: ~20 min of
  # tests later. It is a look at the disk that takes milliseconds, so it comes first,
  # with cargo audit. The tree has the real mutants-gate.sh, since where the rule
  # lives is not this row's business, and a run-all-tests.sh that fails: a gate that
  # misses the refusal then stops at the tests with exit 1, and never reaches the real
  # mutation step. A target of CARGO_TARGET_DIR counts whether or not it exists yet:
  # cargo creates it at the first build.
  a1_tests='echo run-all-tests >>"$HOME/steps"; exit 1'
  a1_tree() {
    fake_tree "$1" "$a1_tests"
    cp "$ROOT/scripts/mutants-gate.sh" "$tmp/$1/scripts/mutants-gate.sh"
    chmod +x "$tmp/$1/scripts/mutants-gate.sh"
  }
  # a1_refused NAME WORD: exit 2, the exit file and the lock agree, no step ran
  # that needs the target, and the refusal names WORD.
  a1_refused() {
    adv_stopped "$1" 2
    adv_never "$1" 'cargo audit' 'cargo deny' 'run-all-tests'
    grep -qF -- "$2" "$tmp/$1.out" || self_fail "$1: the refusal does not name '$2': $(cat "$tmp/$1.out")"
    if grep -q 'MERGE GATE PASSED' "$tmp/$1.out"; then
      self_fail "$1: a gate with a cargo target inside rust/ passed: $(cat "$tmp/$1.out")"
    fi
  }

  a1_tree a1_tag
  mkdir -p "$tmp/a1_tag/rust/target-x"
  : >"$tmp/a1_tag/rust/target-x/CACHEDIR.TAG"
  run_copy a1_tag
  a1_refused a1_tag 'rust/target-x'

  a1_tree a1_var
  run_copy a1_var "" CARGO_TARGET_DIR="$tmp/a1_var/rust/target-sil"
  a1_refused a1_var 'target-sil'
  [[ ! -e "$tmp/a1_var/rust/target-sil" ]] || self_fail "a1_var: the gate created the target it refused"

  a1_tree a1_var_there
  mkdir -p "$tmp/a1_var_there/rust/target-sil"
  : >"$tmp/a1_var_there/rust/target-sil/x"
  run_copy a1_var_there "" CARGO_TARGET_DIR="$tmp/a1_var_there/rust/target-sil"
  a1_refused a1_var_there 'target-sil'

  # The presence anchor: rust/target is cargo-mutants' own to leave out of the copy, a
  # directory named like a target without the tag is not one, and a target outside
  # rust/ is nobody's business. The gate gets to cargo audit, and fails there on purpose.
  a1_tree a1_ok
  mkdir -p "$tmp/a1_ok/rust/target" "$tmp/a1_ok/rust/target-notes" "$tmp/a1_ok/outside-target"
  : >"$tmp/a1_ok/rust/target/CACHEDIR.TAG"
  run_copy a1_ok audit CARGO_TARGET_DIR="$tmp/a1_ok/rust/target"
  adv_stopped a1_ok 1
  [[ "$(steps_of a1_ok)" == "cargo audit" ]] \
    || self_fail "a1_ok: rust/target, a target-notes without the tag and the gate's own target were refused, or the order broke: $(steps_of a1_ok) / $(cat "$tmp/a1_ok.out")"
  rm -f "$tmp/a1_ok/home/steps"
  run_copy a1_ok audit CARGO_TARGET_DIR="$tmp/a1_ok/outside-target"
  adv_stopped a1_ok 1
  [[ "$(steps_of a1_ok)" == "cargo audit" ]] \
    || self_fail "a1_ok: a CARGO_TARGET_DIR outside rust/ was refused: $(steps_of a1_ok) / $(cat "$tmp/a1_ok.out")"

  # A step after the tests, with the first half past, still writes the whole
  # gate's verdict, and the file said running while the tests ran.
  fake_tree after_tests "$reads_while_running; $tests_step"
  run_copy after_tests clippy
  adv_stopped after_tests 1
  grep -q 'run.exit reads: running pid=[0-9]' "$tmp/after_tests.out" \
    || self_fail "after_tests: while the tests ran, the exit file did not say so: $(grep 'run.exit reads' "$tmp/after_tests.out")"

  # The time of every test binary is judged last, after mutants-gate.sh, over the
  # gate's own log up to the line printed after the --features docs step (G7, D-F2c):
  # a time red is drift and not broken behaviour, so the same run still gives the verdicts of codigo-muerto,
  # crap-gate and the mutation. What it says is the gate's verdict: a 1 or a 2
  # from it is the exit of the gate, in the exit file, with no receipt.
  fake_tree times_last "$tests_step"
  run_copy times_last
  (( rc == 0 )) || self_fail "times_last: a gate whose every step passed exited $rc: $(cat "$tmp/times_last.out")"
  [[ "$(exit_file times_last)" == 0 ]] || self_fail "times_last: the exit file says '$(exit_file times_last)', not 0"
  docs_at="$(step_at times_last '^cargo test --features docs$')"
  muerto_at="$(step_at times_last '^codigo-muerto$')"
  crap_at="$(step_at times_last '^crap-gate$')"
  mutants_at="$(step_at times_last '^mutants-gate$')"
  times_at="$(step_at times_last '^test-times ')"
  [[ -n "$docs_at" && -n "$muerto_at" && -n "$crap_at" && -n "$mutants_at" && -n "$times_at" ]] \
    || self_fail "times_last: a step is missing from the order (docs '$docs_at', codigo-muerto '$muerto_at', crap-gate '$crap_at', mutants-gate '$mutants_at', test-times '$times_at'): $(steps_of times_last)"
  (( muerto_at < docs_at && docs_at < crap_at && crap_at < mutants_at && mutants_at < times_at )) \
    || self_fail "times_last: the order is not codigo-muerto (before the first cargo call that compiles), cargo test --features docs, crap-gate, mutants-gate, test-times: $(steps_of times_last)"
  grep -qxF "test-times --judge $tmp/times_last/home/.cache/cuba-gate/merge-gate.log --until === end of timed steps ===" "$tmp/times_last/home/steps" \
    || self_fail "times_last: test-times was not called as --judge <the gate's own log> --until '=== end of timed steps ===': $(steps_of times_last)"
  for line in '=== cargo clippy/test --features docs ===' '=== end of timed steps ==='; do
    tr -d '\r' <"$tmp/times_last/home/times.seen" 2>/dev/null | grep -qxF "$line" \
      || self_fail "times_last: the log test-times was handed did not hold '$line' yet, so it judged a log cut before the boundary: $(cat "$tmp/times_last/home/times.seen" 2>/dev/null || true)"
  done
  mutants_out="$(grep -nm1 -F '=== mutants-gate ===' "$tmp/times_last.out" || true)"
  times_out="$(grep -nm1 -F '=== test times (scripts/test-time-ceilings.txt) ===' "$tmp/times_last.out" || true)"
  passed_out="$(grep -nm1 -F 'MERGE GATE PASSED' "$tmp/times_last.out" || true)"
  [[ -n "$mutants_out" && -n "$times_out" && -n "$passed_out" ]] \
    || self_fail "times_last: the output lacks the mutants-gate step, the test times step or the verdict: $(cat "$tmp/times_last.out")"
  (( ${mutants_out%%:*} < ${times_out%%:*} && ${times_out%%:*} < ${passed_out%%:*} )) \
    || self_fail "times_last: the test times step is not between mutants-gate and the verdict in what the gate printed: $(cat "$tmp/times_last.out")"

  # A time that is red is red for the gate, and every other verdict was already given.
  git_tree times_red "$tests_step"
  run_copy times_red "" FAKE_TIMES_EXIT=1
  adv_stopped times_red 1
  if grep -q 'MERGE GATE PASSED' "$tmp/times_red.out"; then
    self_fail "times_red: a gate whose test times were red passed: $(cat "$tmp/times_red.out")"
  fi
  [[ -z "$(receipts_of times_red)" ]] || self_fail "times_red: a red gate left a receipt: $(receipts_of times_red)"
  mutants_at="$(step_at times_red '^mutants-gate$')"
  times_at="$(step_at times_red '^test-times ')"
  [[ -n "$mutants_at" && -n "$times_at" ]] && (( mutants_at < times_at )) \
    || self_fail "times_red: the test times did not come after mutants-gate, so a red time hid the verdicts after it (mutants-gate '$mutants_at', test-times '$times_at'): $(steps_of times_red)"

  # A log it cannot read (exit 2 from it) stops the gate with 2, not with a pass.
  fake_tree times_cannot "$tests_step"
  run_copy times_cannot "" FAKE_TIMES_EXIT=2
  adv_stopped times_cannot 2
  if grep -q 'MERGE GATE PASSED' "$tmp/times_cannot.out"; then
    self_fail "times_cannot: a gate whose test times could not be judged passed: $(cat "$tmp/times_cannot.out")"
  fi

  # The readiness check: libpq reads DATABASE_URL, and the gate says which port
  # it answered on. With WSL2 in mirrored mode the localhost:5488 of Windows is
  # the live corpus of WSL, so a check that asked :5488 whatever the URL said
  # judged the wrong server and still printed an OK. Each row is a copy of the
  # gate whose run-all-tests.sh stand-in prints a marker, so "what came next" and
  # "it never ran" can be read, and whose pg_isready stand-in (fake_tree) answers
  # what the row says and records what it was asked.
  ran_marker='stand-in run-all-tests: the next step ran'
  pg_args() { cat "$tmp/$1/home/pg_isready.args" 2>/dev/null || true; }
  # pg_stopped NAME CODE [PASSWORD]: the gate stopped with CODE, never reached
  # the next step, wrote CODE in its own exit file, freed its lock, and nothing
  # it printed carries the stand-in's stderr word or the URL's password.
  pg_stopped() {
    local name="$1" want="$2" password="${3:-}"
    (( rc == want )) || self_fail "$name: the gate exited $rc, not $want: $(cat "$tmp/$name.out")"
    if grep -qF "$ran_marker" "$tmp/$name.out"; then
      self_fail "$name: the gate went on to run-all-tests.sh: $(cat "$tmp/$name.out")"
    fi
    [[ "$(exit_file "$name")" == "$want" ]] \
      || self_fail "$name: the exit file says '$(exit_file "$name")', not $want"
    [[ ! -e "$tmp/$name/home/.cache/cuba-gate/lock" ]] || self_fail "$name: the gate left its lock behind"
    # The readiness check comes before every step, cargo audit included: a gate
    # that asks cargo before it knows Postgres is up has the order wrong.
    if grep -q 'cargo' "$tmp/$name/home/steps" 2>/dev/null; then
      self_fail "$name: cargo was called before Postgres was known to be ready: $(cat "$tmp/$name/home/steps")"
    fi
    if grep -q 'S3cr3tW2' "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries S3cr3tW2: $(cat "$tmp/$name.out")"
    fi
    if [[ -n "$password" ]] && grep -qF "$password" "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the password of DATABASE_URL: $(cat "$tmp/$name.out")"
    fi
  }
  # pg_green NAME URL ANSWER PORT PASSWORD: URL is the DATABASE_URL the row starts
  # the gate with. pg_isready gets exactly -d and that URL, and the line after the
  # banner is the OK for PORT, the one pg_isready's ANSWER names, with nothing
  # printed in between. The OK of the judge server (its cluster_name read through
  # the identity query) comes next, and the line after it is the banner of cargo
  # audit, the first step that follows the readiness check and the judge.
  pg_green() {
    local name="$1" url="$2" answer="$3" port="$4" password="$5" i at=-1
    local -a lines
    fake_tree "$name" "echo \"$ran_marker\""
    run_copy_as "$url" "$name" "" FAKE_PG_ANSWER="$answer"
    (( rc == 0 )) || self_fail "$name: a gate whose pg_isready answered '$answer' exited $rc: $(cat "$tmp/$name.out")"
    [[ "$(pg_args "$name")" == $'-d\n'"$url" ]] \
      || self_fail "$name: pg_isready was called with '$(pg_args "$name" | tr '\n' ' ')', not with exactly -d and DATABASE_URL as it is ('$url')"
    mapfile -t lines < <(tr -d '\r' <"$tmp/$name.out")
    for i in "${!lines[@]}"; do
      if [[ "${lines[i]}" == "OK  Postgres :"* ]]; then
        at=$i
        break
      fi
    done
    (( at >= 2 )) || self_fail "$name: no 'OK  Postgres :<port>' line after the banner: $(cat "$tmp/$name.out")"
    [[ "${lines[at]}" == "OK  Postgres :$port" ]] \
      || self_fail "$name: the line said '${lines[at]}', not the port pg_isready answered on ($port)"
    [[ -z "${lines[at-1]}" && "${lines[at-2]}" == *"a second time."* ]] \
      || self_fail "$name: something was printed between the banner and the OK line: '${lines[at-2]}' / '${lines[at-1]}'"
    [[ "${lines[at+1]:-}" == "OK  judge server :$port (cluster_name=memory-industry-gate, system_identifier=7701731310526959658)" ]] \
      || self_fail "$name: the line after the OK line is '${lines[at+1]:-}', not the OK of the judge server on :$port"
    # CI-F3.3b-2: the planner names the row it starts (`dag: P env: ...`) between the OK and the banner of its block.
    planner_gap=0
    [[ "${lines[at+2]:-}" =~ ^dag:\ P\ env: ]] && planner_gap=1
    [[ "${lines[at+2+planner_gap]:-}" == "=== cargo audit ===" ]] \
      || self_fail "$name: the line after the judge's OK is '${lines[at+2]:-}', not the banner of cargo audit, the first step after the readiness check"
    if grep -qxF "$answer" "$tmp/$name.out"; then
      self_fail "$name: pg_isready's own answer is a line of the output: $(cat "$tmp/$name.out")"
    fi
    if [[ "$port" != 5488 ]] && grep -q ':5488' "$tmp/$name.out"; then
      self_fail "$name: the output names :5488, and the server answered on :$port: $(cat "$tmp/$name.out")"
    fi
    if grep -q 'S3cr3tW2' "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the stand-in's stderr: $(cat "$tmp/$name.out")"
    fi
    if [[ -n "$password" ]] && grep -qF "$password" "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the password of DATABASE_URL: $(cat "$tmp/$name.out")"
    fi
  }
  # judge_refused NAME URL ANSWER PASSWORD [VAR=value ...]: the gate stops with 2
  # before cargo audit, asks no step, and prints nothing that carries the password.
  judge_refused() {
    local name="$1" url="$2" answer="$3" password="$4"
    shift 4
    fake_tree "$name" "echo \"$ran_marker\""
    run_copy_as "$url" "$name" "" FAKE_PG_ANSWER="$answer" "$@"
    pg_stopped "$name" 2 "$password"
    if grep -qxF '=== cargo audit ===' "$tmp/$name.out"; then
      self_fail "$name: the banner of cargo audit was printed before the server was refused: $(cat "$tmp/$name.out")"
    fi
  }
  # judge_green NAME URL CLUSTER PORT PASSWORD: the server is marked CLUSTER on PORT.
  # The gate says so on the line after the OK of Postgres, asks the identity of the
  # same server read-only, from its maintenance database, and goes on to cargo audit.
  judge_green() {
    local name="$1" url="$2" cluster="$3" port="$4" password="$5" want at
    fake_tree "$name" "echo \"$ran_marker\""
    run_copy_as "$url" "$name" "" FAKE_PG_ANSWER="127.0.0.1:$port - accepting connections" FAKE_PSQL_CLUSTER_NAME="$cluster"
    (( rc == 0 )) || self_fail "$name: a server marked '$cluster' on :$port exited $rc: $(cat "$tmp/$name.out")"
    want="OK  judge server :$port (cluster_name=$cluster, system_identifier=7701731310526959658)"
    at="$(grep -nxF -- "$want" "$tmp/$name.out" | head -1 | cut -d: -f1)"
    [[ -n "$at" ]] || self_fail "$name: no line '$want': $(cat "$tmp/$name.out")"
    # CI-F3.3b-2: the planner names the row it starts (`dag: P env: ...`) between the OK and the banner of its block.
    next="$(sed -n "$((at+1))p" "$tmp/$name.out")"
    [[ "$next" =~ ^dag:\ P\ env: ]] && next="$(sed -n "$((at+2))p" "$tmp/$name.out")"
    [[ "$next" == "=== cargo audit ===" ]] \
      || self_fail "$name: the line after the judge's OK (and the planner's line for P) is not the banner of cargo audit: $(cat "$tmp/$name.out")"
    grep -qxF 'PGOPTIONS=-c default_transaction_read_only=on' "$tmp/$name/home/psql.env" 2>/dev/null \
      || self_fail "$name: the identity query did not run read-only: $(cat "$tmp/$name/home/psql.env" 2>/dev/null || true)"
    # A F2b: the password goes to libpq in PGPASSWORD, for that one call, and never in the argv of psql
    # (ps shows argv to every user). The maintenance database is asked of the same server, without it.
    grep -qxF "postgresql://gate@127.0.0.1:$port/postgres" "$tmp/$name/home/psql.args" 2>/dev/null \
      || self_fail "$name: the identity query was not asked of the maintenance database of the same server: $(cat "$tmp/$name/home/psql.args" 2>/dev/null || true)"
    grep -qxF "PGPASSWORD=$password" "$tmp/$name/home/psql.env" 2>/dev/null \
      || self_fail "$name: the identity query did not get the password in PGPASSWORD for that call: $(cat "$tmp/$name/home/psql.env" 2>/dev/null || true)"
    if grep -qF "$password" "$tmp/$name/home/psql.args" 2>/dev/null; then
      self_fail "$name: the password of DATABASE_URL is in the argv of psql: $(cat "$tmp/$name/home/psql.args")"
    fi
    if grep -qF "$password" "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the password of DATABASE_URL: $(cat "$tmp/$name.out")"
    fi
  }
  # J1.1, rewritten by CI-F2 (D-F2c, D-F2k): without DATABASE_URL, unset or empty, on Linux the
  # judge makes its own lanes, so J1.1's refusal moves to the step that makes them. Here the bindir
  # has no initdb (A2, "sin initdb"): the gate stops before pg_isready, psql or cargo, and its FAIL
  # line names initdb and docs/gate.md, never :5488. J1.1's intent (no URL, never the live corpus)
  # is kept by this row and by A3.
  for form in unset empty; do
    fake_tree "j11_$form" "echo \"$ran_marker\""
    if [[ "$form" == unset ]]; then
      run_copy_as unset "j11_$form" "" GATE_PG_BINDIR="$tmp/j11_$form/nopg"
    else
      run_copy_as "" "j11_$form" "" GATE_PG_BINDIR="$tmp/j11_$form/nopg"
    fi
    pg_stopped "j11_$form" 2
    [[ ! -e "$tmp/j11_$form/home/pg_isready.args" && ! -e "$tmp/j11_$form/home/psql.args" ]] \
      || self_fail "j11_$form: a server was asked with no DATABASE_URL and no initdb: $(pg_args "j11_$form") $(cat "$tmp/j11_$form/home/psql.args" 2>/dev/null || true)"
    fail_line_with "j11_$form" initdb 'docs/gate.md' \
      || self_fail "j11_$form: no FAIL line names initdb and docs/gate.md: $(cat "$tmp/j11_$form.out")"
    if grep -q '5488' "$tmp/j11_$form.out"; then
      self_fail "j11_$form: the refusal names :5488, the live corpus: $(cat "$tmp/j11_$form.out")"
    fi
  done
  # A3 (CI-F2): off Linux, without DATABASE_URL, the refusal is J1.1's: exit 2 naming DATABASE_URL and
  # docs/gate.md, and no initdb was called, whatever the bindir holds.
  fake_tree a3_not_linux "echo \"$ran_marker\""
  printf '%s\n' '#!/bin/sh' 'if [ "${1:-}" = -s ]; then echo MINGW64_NT-10.0; else exec /usr/bin/uname "$@"; fi' \
    >"$tmp/a3_not_linux/bin/uname"
  chmod +x "$tmp/a3_not_linux/bin/uname"
  run_copy_as unset a3_not_linux "" GATE_PG_BINDIR="$tmp/a3_not_linux/pgbin"
  pg_stopped a3_not_linux 2
  [[ ! -e "$tmp/a3_not_linux/home/initdb.calls" ]] \
    || self_fail "a3_not_linux: initdb was called off Linux: $(cat "$tmp/a3_not_linux/home/initdb.calls")"
  fail_line_with a3_not_linux DATABASE_URL 'docs/gate.md' \
    || self_fail "a3_not_linux: no FAIL line names DATABASE_URL and docs/gate.md: $(cat "$tmp/a3_not_linux.out")"
  # CI-F2 (F2b-1), the caller's rows. Without DATABASE_URL on Linux, merge-gate.sh makes one lane per
  # step (dbg, docs, cov, mut), each with its own URL and GATE_LANE, and takes it down after its step.
  # run-all-tests.sh records what it was given for the full run; the lanes' stand-in (fake_tree) records
  # every call in the steps. lane_body is the run-all-tests stand-in of every row below.
  lane_body='echo "run-all-tests${*:+ $*}" >>"$HOME/steps"; [ "${1:-}" = --check-models ] || echo "${GATE_LANE-unset} ${DATABASE_URL-unset} ${GATE_SERVER_SYSTEM_ID-unset}" >>"$HOME/run-all.lanes"'
  # A1: the cheap check and the sweep of the lanes come before the first cargo (the audit).
  fake_tree a1 "$lane_body"
  run_copy_as unset a1 ""
  (( rc == 0 )) || self_fail "A1: the gate without DATABASE_URL on Linux exited $rc: $(cat "$tmp/a1.out")"
  first_cargo="$(grep -n -m1 '^cargo ' "$tmp/a1/home/steps" 2>/dev/null | cut -d: -f1 || true)"
  first_check="$(grep -n -m1 '^check$' "$tmp/a1/home/steps" 2>/dev/null | cut -d: -f1 || true)"
  first_sweep="$(grep -n -m1 '^sweep$' "$tmp/a1/home/steps" 2>/dev/null | cut -d: -f1 || true)"
  [[ -n "$first_check" && -n "$first_cargo" && "$first_check" -lt "$first_cargo" ]] \
    || self_fail "A1: the lane check does not come before cargo (check at ${first_check:-none}, cargo at ${first_cargo:-none}): $(cat "$tmp/a1/home/steps")"
  [[ -n "$first_sweep" && "$first_sweep" -lt "$first_cargo" ]] \
    || self_fail "A1: the sweep of the cluster root does not come before cargo (sweep at ${first_sweep:-none}): $(cat "$tmp/a1/home/steps")"
  [[ "$(grep -n -m1 '^=== cargo audit ===$' "$tmp/a1.out" 2>/dev/null | cut -d: -f1 || true)" -gt "$(grep -n -m1 '^OK  postgres 18.6' "$tmp/a1.out" 2>/dev/null | cut -d: -f1 || true)" ]] 2>/dev/null \
    || self_fail "A1: the OK of the Postgres check is not printed before the banner of cargo audit: $(cat "$tmp/a1.out")"
  # C1: each step gets its own lane: the order of the lanes and the steps, and what each step saw.
  fake_tree c1 "$lane_body"
  run_copy_as unset c1 ""
  (( rc == 0 )) || self_fail "C1: the gate without DATABASE_URL on Linux exited $rc: $(cat "$tmp/c1.out")"
  # CI-F3.3b-1 (D-F3b, spec §4 F3.3b): the chains of the step table, in idx order, one process and one
  # cluster each: dbg (CL..D3), b3 (B3), rel (R), e2e (E). Then the --features docs step as before.
  lane_order='run-all-tests --check-models
up dbg
run-all-tests --lane CL --lane D1 --lane D2 --lane NLI --lane D3
down dbg
up b3
run-all-tests --lane B3
down b3
up rel
run-all-tests --lane R
down rel
up e2e
run-all-tests --lane E
down e2e
up docs
cargo clippy --all-targets --features docs -- -D warnings
cargo test --features docs
down docs
up cov
crap-gate
down cov
up mut
mutants-gate
down mut'
  [[ "$(grep -E '^(up |down |run-all-tests|crap-gate|mutants-gate|cargo .*--features docs)' "$tmp/c1/home/steps")" == "$lane_order" ]] \
    || self_fail "C1: the lanes and the steps are not in the order of D-F2c (up, its step, down, per lane): $(cat "$tmp/c1/home/steps")"
  grep -qxE 'dbg postgresql://cuba:fakepw@[^ ]*%2Fdbg\.fk0000:5432/brain 7701731310526959658' "$tmp/c1/home/run-all.lanes" \
    || self_fail "C1: the full run did not see the URL and GATE_SERVER_SYSTEM_ID of its lane dbg: $(cat "$tmp/c1/home/run-all.lanes" 2>/dev/null)"
  # CI-F3.3b-1: the e2e chain has a cluster of its own, and it sees its URL and its identity, not the dbg one.
  grep -qxE 'e2e postgresql://cuba:fakepw@[^ ]*%2Fe2e\.fk0000:5432/brain 7701731310526959658' "$tmp/c1/home/run-all.lanes" \
    || self_fail "C1: the e2e chain did not see the URL and GATE_SERVER_SYSTEM_ID of its lane e2e: $(cat "$tmp/c1/home/run-all.lanes" 2>/dev/null)"
  grep -qxE 'cov postgresql://cuba:fakepw@[^ ]*%2Fcov\.fk0000:5432/brain 7701731310526959658' "$tmp/c1/home/crap-gate.lanes" \
    || self_fail "C1: crap-gate did not see the URL of lane cov: $(cat "$tmp/c1/home/crap-gate.lanes" 2>/dev/null)"
  grep -qxE 'mut postgresql://cuba:fakepw@[^ ]*%2Fmut\.fk0000:5432/brain 7701731310526959658' "$tmp/c1/home/mutants-gate.lanes" \
    || self_fail "C1: mutants-gate did not see the URL of lane mut: $(cat "$tmp/c1/home/mutants-gate.lanes" 2>/dev/null)"
  # The environment of the gate itself carries no lane's URL when it reaches the last step (test-times).
  [[ -e "$tmp/c1/home/test-times.url" && ( "$(cat "$tmp/c1/home/test-times.url")" == unset || -z "$(cat "$tmp/c1/home/test-times.url")" ) ]] \
    || self_fail "C1: a lane's DATABASE_URL reached test-times, which runs after the lanes: $(cat "$tmp/c1/home/test-times.url")"
  docs_lines="$(grep -c -- '--features docs' "$tmp/c1/home/cargo.lanes" 2>/dev/null || true)"
  docs_ok="$(grep -cE '^docs postgresql://cuba:fakepw@[^ ]*%2Fdocs\.fk0000:5432/brain 7701731310526959658 (clippy|test) .*--features docs' "$tmp/c1/home/cargo.lanes" 2>/dev/null || true)"
  (( docs_lines >= 2 && docs_lines == docs_ok )) \
    || self_fail "C1: the docs steps did not all see the URL and GATE_LANE of lane docs: $(cat "$tmp/c1/home/cargo.lanes")"
  # F2b-fix (D-F2i): the step of a lane is judged by gate_require_judge_server, which refuses a lane's
  # URL unless GATE_CLUSTER_ROOT names the root of the clusters. The root is the gate's own shell variable
  # (not exported, as in the judge), so it reaches a step only if run_in_lane hands it over. The crap-gate
  # stand-in records what it was given; the root of this row is the stand-in library's default under its HOME.
  fake_tree f2bfix "$lane_body"
  printf '%s\n' '#!/bin/sh' 'echo "${GATE_CLUSTER_ROOT-unset}" >>"$HOME/crap-gate.roots"' 'exit 0' \
    >"$tmp/f2bfix/scripts/crap-gate.sh"
  f2bfix_root="$tmp/f2bfix/home/cluster-root"
  run_copy_as unset f2bfix ""
  (( rc == 0 )) || self_fail "F2b-fix: the gate without DATABASE_URL on Linux exited $rc: $(cat "$tmp/f2bfix.out")"
  grep -qxF "$f2bfix_root" "$tmp/f2bfix/home/crap-gate.roots" 2>/dev/null \
    || self_fail "F2b-fix: the step of lane cov did not see GATE_CLUSTER_ROOT=$f2bfix_root: a lane's step is refused without it (D-F2i), and run_in_lane must hand it over with the lane's URL. It got: $(cat "$tmp/f2bfix/home/crap-gate.roots" 2>/dev/null)"
  # CI-J5: the --features docs step reads the models the full run reads. The HOME of this row has the
  # embedder where the binary keeps it, and the stand-in cargo of each docs call records the two
  # variables it was given: ONNX_MODEL_PATH and CUBA_EMBEDDING_DIM.
  fake_tree j5 "$lane_body"
  mkdir -p "$tmp/j5/home/.cache/memory-industry/models"
  : >"$tmp/j5/home/.cache/memory-industry/models/model_quantized.onnx"
  run_copy_as unset j5 ""
  (( rc == 0 )) || self_fail "J5: the gate with the embedder under its HOME exited $rc: $(cat "$tmp/j5.out")"
  j5_docs="$(grep -c -- '--features docs' "$tmp/j5/home/cargo.models" 2>/dev/null || true)"
  j5_docs_ok="$(awk -v p="$tmp/j5/home/.cache/memory-industry/models" '/--features docs/ && $1 == p && $2 == "384"' \
    "$tmp/j5/home/cargo.models" 2>/dev/null | wc -l | tr -d ' ')"
  (( j5_docs >= 2 && j5_docs == j5_docs_ok )) \
    || self_fail "J5: the --features docs step did not get ONNX_MODEL_PATH=<HOME>/.cache/memory-industry/models and CUBA_EMBEDDING_DIM=384 as the full run does; it got: $(cat "$tmp/j5/home/cargo.models" 2>/dev/null || true)"
  # C5: a DATABASE_URL the operator gives still governs, as it does today: no lane is made, every step
  # gets that URL, and the gate says the lanes share that server.
  ext='postgresql://gate:pw0@127.0.0.1:5489/brain'
  fake_tree c5 "$lane_body"
  run_copy_as "$ext" c5 "" FAKE_PG_ANSWER='127.0.0.1:5489 - accepting connections' FAKE_PSQL_CLUSTER_NAME=memory-industry-gate
  (( rc == 0 )) || self_fail "C5: a gate with an external DATABASE_URL exited $rc: $(cat "$tmp/c5.out")"
  if grep -qE '^(up |down|check$|sweep$)' "$tmp/c5/home/steps"; then
    self_fail "C5: a lane was made or swept with an external DATABASE_URL: $(cat "$tmp/c5/home/steps")"
  fi
  awk -v u="$ext" '$2 != u { bad = 1 } END { exit bad }' "$tmp/c5/home/cargo.lanes" \
    || self_fail "C5: a step did not see the external DATABASE_URL: $(cat "$tmp/c5/home/cargo.lanes")"
  grep -q '^note: .*share' "$tmp/c5.out" \
    || self_fail "C5: the gate does not say that the lanes share the external server (note: ... share): $(cat "$tmp/c5.out")"
  # D1: a lane is taken down after its step, even when the step fails, and nothing after it runs.
  fake_tree d1 'echo "run-all-tests${*:+ $*}" >>"$HOME/steps"; [ "${1:-}" = --check-models ] && exit 0; exit 1'
  run_copy_as unset d1 ""
  (( rc == 1 )) || self_fail "D1: the gate whose run-all-tests failed exited $rc, not 1: $(cat "$tmp/d1.out")"
  [[ "$(exit_file d1)" == 1 ]] || self_fail "D1: the exit file says '$(exit_file d1)', not 1"
  grep -qx 'down dbg' "$tmp/d1/home/steps" || self_fail "D1: lane dbg was not taken down after its failed step: $(cat "$tmp/d1/home/steps")"
  grep -qx 'up docs' "$tmp/d1/home/steps" && self_fail "D1: the docs lane was made after the failed run-all-tests: $(cat "$tmp/d1/home/steps")"
  # D2: a gate stopped by TERM, HUP or INT, during its first lane, takes that lane down (down-all) and
  # leaves 2 in its exit file.
  # CI-F3.3b-2: the signal names the gate by its lock record (the steps run one level deeper, under the planner).
  d2_body='echo "run-all-tests${*:+ $*}" >>"$HOME/steps"; if [ "${1:-}" != --check-models ] && [ ! -e "$HOME/signalled" ]; then touch "$HOME/signalled"; kill -"$D2_SIG" "$(printf "%s\n" "$CUBA_GATE_LOCK_OWNER" | sed -n "s/^pid=\([0-9]*\) .*/\1/p")"; fi; sleep 2; exit 0'
  for sig in TERM HUP INT; do
    fake_tree "d2_$sig" "$d2_body"
    run_copy_as unset "d2_$sig" "" "D2_SIG=$sig"
    [[ "$(exit_file "d2_$sig")" == 2 ]] \
      || self_fail "D2: a gate stopped by $sig left '$(exit_file "d2_$sig")' in its exit file, not 2: $(cat "$tmp/d2_$sig.out")"
    grep -qx 'down-all' "$tmp/d2_$sig/home/steps" \
      || self_fail "D2: a gate stopped by $sig did not take its lane down (down-all): $(cat "$tmp/d2_$sig/home/steps" 2>/dev/null)"
  done
  # J1.2: a server that answers but is not marked is refused whatever its port,
  # and the refusal says the port, the cluster_name it read and the system_identifier.
  judge_refused j12_empty 'postgresql://gate:pw0@127.0.0.1:5488/brain' '127.0.0.1:5488 - accepting connections' pw0 FAKE_PSQL_CLUSTER_NAME=
  fail_line_with j12_empty ':5488' cluster_name 7701731310526959658 \
    || self_fail "j12_empty: no FAIL line names :5488, the cluster_name it read and the system_identifier: $(cat "$tmp/j12_empty.out")"
  judge_refused j12_other 'postgresql://gate:pw0@127.0.0.1:5489/brain' '127.0.0.1:5489 - accepting connections' pw0 FAKE_PSQL_CLUSTER_NAME=otro
  fail_line_with j12_other ':5489' otro 7701731310526959658 \
    || self_fail "j12_other: no FAIL line names :5489, 'otro' and the system_identifier: $(cat "$tmp/j12_other.out")"
  judge_green j12_marked_5488 'postgresql://gate:pw0@127.0.0.1:5488/brain' memory-industry-gate 5488 pw0
  # J1.3: a server marked memory-industry-gate, or one of its lanes, is accepted and named.
  judge_green j13_gate 'postgresql://gate:pw0@127.0.0.1:5489/brain' memory-industry-gate 5489 pw0
  judge_green j13_e2e 'postgresql://gate:pw0@127.0.0.1:5489/brain' memory-industry-gate-e2e 5489 pw0
  # J1.4: an identity query that fails is exit 2, says what it could not read, and
  # does not print the password.
  judge_refused j14_query 'postgresql://gate:pw0@127.0.0.1:5489/brain' '127.0.0.1:5489 - accepting connections' pw0 \
    FAKE_PSQL_EXIT=1 'FAKE_PSQL_STDERR=ERROR:  permission denied for function pg_control_system'
  fail_line_with j14_query pg_control_system \
    || self_fail "j14_query: no FAIL line names what could not be read (pg_control_system): $(cat "$tmp/j14_query.out")"
  pg_green pg_port 'postgresql://gate:pw0@127.0.0.1:5575/brain' \
    "127.0.0.1:5575 - accepting connections" 5575 pw0
  # No port in the URL: it is libpq that puts 5432 (or PGPORT), not the gate.
  pg_green pg_postgres_scheme 'postgres://gate:pw0@db.example.invalid/brain' \
    "db.example.invalid:5432 - accepting connections" 5432 pw0
  pg_green pg_encoded 'postgresql://ga%40te:p%40ss%2Fw%3Frd@127.0.0.1:6543/brain' \
    "127.0.0.1:6543 - accepting connections" 6543 'p%40ss%2Fw%3Frd'
  pg_green pg_socket 'postgresql://gate@%2Frun%2Fpostgresql:5433/brain' \
    "/run/postgresql:5433 - accepting connections" 5433 ''
  pg_green pg_ipv6 'postgresql://gate:pw0@[::1]:5434/brain' \
    "::1:5434 - accepting connections" 5434 pw0

  # Nothing answers on the port of the URL: the gate says which one, with what
  # libpq said, and stops with 1.
  fake_tree pg_down "echo \"$ran_marker\""
  run_copy pg_down "" DATABASE_URL='postgresql://gate:pw0@127.0.0.1:5575/brain' \
    FAKE_PG_ANSWER='127.0.0.1:5575 - no response' FAKE_PG_EXIT=2
  pg_stopped pg_down 1 pw0
  grep -qxF 'FAIL: Postgres not ready on :5575 (pg_isready: 127.0.0.1:5575 - no response)' "$tmp/pg_down.out" \
    || self_fail "a Postgres that did not answer was not named with the port of the URL and libpq's own answer: $(cat "$tmp/pg_down.out")"
  # J6.2: one call to pg_isready, with the URL as it is, and a line that says how
  # to start the judge's server.
  grep -qF 'docs/gate.md' "$tmp/pg_down.out" \
    || self_fail "a Postgres that did not answer did not say how to start the judge's server (docs/gate.md): $(cat "$tmp/pg_down.out")"
  [[ "$(wc -l <"$tmp/pg_down/home/pg_isready.args")" -eq 2 ]] \
    || self_fail "pg_down: pg_isready was asked more than once: $(pg_args pg_down | tr '\n' ' ')"
  if grep -q ':5488' "$tmp/pg_down.out"; then
    self_fail "the refusal for :5575 names :5488: $(cat "$tmp/pg_down.out")"
  fi

  # libpq cannot read the URL: pg_isready exits 3 and its stderr quotes the piece
  # it could not read, which here is the password. The gate says that it could
  # not read DATABASE_URL and how to see libpq's own words, and says nothing of
  # the URL.
  fake_tree pg_unreadable "echo \"$ran_marker\""
  run_copy pg_unreadable "" DATABASE_URL='postgresql://gate:S3cr3tW2%zz@127.0.0.1:5575/brain' \
    FAKE_PG_ANSWER= FAKE_PG_EXIT=3 \
    FAKE_PG_STDERR='pg_isready: error: invalid percent-encoded token: "S3cr3tW2%zz"'
  pg_stopped pg_unreadable 2
  grep -q '^FAIL: ' "$tmp/pg_unreadable.out" \
    || self_fail "a gate whose pg_isready could not read DATABASE_URL did not refuse in a FAIL line: $(cat "$tmp/pg_unreadable.out")"
  grep -qF 'pg_isready -d "$DATABASE_URL"' "$tmp/pg_unreadable.out" \
    || self_fail "the refusal did not say how to see libpq's own words: $(cat "$tmp/pg_unreadable.out")"

  # An answer with no port in it: the gate does not know which server answered,
  # and does not guess the one it used to ask.
  fake_tree pg_no_port "echo \"$ran_marker\""
  run_copy pg_no_port "" DATABASE_URL='postgresql://gate:pw0@127.0.0.1:abc/brain' \
    FAKE_PG_ANSWER='127.0.0.1:abc - no response' FAKE_PG_EXIT=2
  pg_stopped pg_no_port 2 pw0
  grep -Eq "[\"']127\.0\.0\.1:abc - no response[\"']" "$tmp/pg_no_port.out" \
    || self_fail "the refusal for an answer with no port did not quote the answer: $(cat "$tmp/pg_no_port.out")"
  if grep -q ':5488' "$tmp/pg_no_port.out"; then
    self_fail "an answer with no port was read as :5488: $(cat "$tmp/pg_no_port.out")"
  fi

  # A URL the gate cannot derive brain_gate from (run-all-tests.sh builds that
  # URL by replacing what follows the last slash): refused with 2 before
  # pg_isready is asked, and the reason carries none of the URL.
  # pg_refused NAME URL WORD...: every WORD is in what the gate printed.
  pg_refused() {
    local name="$1" url="$2" word
    shift 2
    fake_tree "$name" "echo \"$ran_marker\""
    run_copy "$name" "" DATABASE_URL="$url"
    pg_stopped "$name" 2
    grep -q '^FAIL: ' "$tmp/$name.out" \
      || self_fail "$name: the refusal is not a FAIL line: $(cat "$tmp/$name.out")"
    [[ ! -e "$tmp/$name/home/pg_isready.args" ]] \
      || self_fail "$name: pg_isready was asked about a URL the gate cannot derive brain_gate from: $(pg_args "$name")"
    for word in "$@"; do
      grep -qF -- "$word" "$tmp/$name.out" \
        || self_fail "$name: the refusal does not say '$word': $(cat "$tmp/$name.out")"
    done
  }
  pg_refused pg_query 'postgresql://gate:S3cr3tW2@/brain?host=%2Frun%2Fpostgresql' '%2F'
  pg_refused pg_no_db 'postgresql://gate:S3cr3tW2@127.0.0.1:5575' '%2F' '%3F' '%40'
  pg_refused pg_conninfo 'host=127.0.0.1 port=5575 dbname=brain password=S3cr3tW2' 'postgresql://'

  # More than one '@' before the host: libpq ends the user and the password at
  # the FIRST one and sqlx at the LAST, so the readiness check and the binary
  # would ask different servers, and libpq prints the rest of the password as
  # the host (measured with libpq 16.15 on 127.0.0.1:5499, nothing listening:
  # postgresql://u:x@S3cr3tW2@127.0.0.1:5499/brain answers
  # 'S3cr3tW2@127.0.0.1:5499 - no response'). The stand-in answers what libpq
  # did; the gate has to refuse with 2 before asking, and not print it.
  fake_tree pg_two_at "echo \"$ran_marker\""
  run_copy pg_two_at "" DATABASE_URL='postgresql://gate:x@S3cr3tW2@127.0.0.1:5575/brain' \
    FAKE_PG_ANSWER='S3cr3tW2@127.0.0.1:5575 - no response' FAKE_PG_EXIT=2
  pg_stopped pg_two_at 2
  [[ "$(grep -c '^FAIL: ' "$tmp/pg_two_at.out")" == 1 ]] \
    || self_fail "pg_two_at: the refusal is not one FAIL line: $(cat "$tmp/pg_two_at.out")"
  grep -qF '%40' "$tmp/pg_two_at.out" && grep -qF "more than one '@'" "$tmp/pg_two_at.out" \
    || self_fail "pg_two_at: the refusal does not say %40 and more than one '@': $(cat "$tmp/pg_two_at.out")"
  [[ ! -e "$tmp/pg_two_at/home/pg_isready.args" ]] \
    || self_fail "pg_two_at: pg_isready was asked about a URL with two '@': $(pg_args pg_two_at)"
  pg_refused pg_double_at 'postgresql://gate:S3cr3tW2@@127.0.0.1:5575/brain' '%40' "more than one '@'"

  # Without pg_isready the gate asks docker, and the container is the one that
  # publishes the port of DATABASE_URL, not one picked by its name: on Windows the
  # judge's container was memory-industry-dbwin-20261012 on :5489, and a check by
  # name judged memory-industry-db, a server DATABASE_URL did not name. A host
  # that is not this machine, or a gate with no docker, cannot be judged this way
  # and is refused with 2. Each row runs with a PATH of the tree's stand-ins and
  # of wrappers for every program the system has except pg_isready, docker and
  # psql: a pg_isready of the machine running this test would turn every row green
  # (Debian puts one in /usr/bin), and a real psql would ask the identity of a
  # server the row never named (the judge's guard, J1.4, is what must find none).
  mkdir -p "$tmp/sysbin"
  for d in "$gitdir" /usr/bin /bin; do
    for f in "$d"/*; do
      [[ -f "$f" && -x "$f" ]] || continue
      tool="${f##*/}"
      tool="${tool%.exe}"
      case "$tool" in pg_isready* | docker* | psql*) continue ;; esac
      [[ -e "$tmp/sysbin/$tool" ]] && continue
      printf '#!/bin/sh\nexec "%s" "$@"\n' "$f" >"$tmp/sysbin/$tool"
    done
  done
  chmod +x "$tmp"/sysbin/*
  ran_body="echo run-all-tests >>\"\$HOME/steps\"; echo \"$ran_marker\""
  # dk_prepare NAME: the tree, with no pg_isready, and a check that the PATH of
  # the row finds none and still finds git.
  dk_prepare() {
    local name="$1" probe="$tmp/$1/bin:$tmp/sysbin"
    fake_tree "$name" "$ran_body"
    rm -f "$tmp/$name/bin/pg_isready"
    if (PATH="$probe"; command -v pg_isready >/dev/null 2>&1); then
      self_fail "the fixture cannot hide pg_isready: $name finds one on $probe"
    fi
    (PATH="$probe"; command -v git >/dev/null 2>&1) \
      || self_fail "the fixture hides git: $name finds none on $probe"
  }
  # dk_go NAME URL [VAR=value ...]: the gate with that DATABASE_URL.
  dk_go() {
    local name="$1" url="$2"
    shift 2
    if [[ ! -e "$tmp/$name/bin/docker" ]] && (PATH="$tmp/$name/bin:$tmp/sysbin"; command -v docker >/dev/null 2>&1); then
      self_fail "the fixture cannot hide docker: $name finds one"
    fi
    run_copy "$name" "" PATH="$tmp/$name/bin:$tmp/sysbin" DATABASE_URL="$url" "$@"
  }
  docker_args() { cat "$tmp/$1/home/docker.args" 2>/dev/null || true; }
  # dk_green NAME URL PORT CONTAINER [PASSWORD] [VAR=value ...]: the gate asked
  # docker which container publishes PORT, ran pg_isready in that one (and in none
  # picked by name), said which it was, and went on to cargo audit.
  dk_green() {
    local name="$1" url="$2" port="$3" container="$4" password="$5" i at=-1 asked
    local -a lines
    shift 5
    dk_prepare "$name"
    dk_go "$name" "$url" FAKE_DOCKER_PORT="$port" FAKE_DOCKER_NAMES="$container" \
      FAKE_DOCKER_HEALTHY="$container" "$@"
    (( rc == 0 )) || self_fail "$name: a gate whose container for :$port was ready exited $rc: $(cat "$tmp/$name.out")"
    asked="$(docker_args "$name")"
    grep -Eq "^ps .*publish=$port( |\$)" <<<"$asked" \
      || self_fail "$name: docker ps was not asked for the container that publishes $port: $asked"
    grep -q "^exec $container pg_isready" <<<"$asked" \
      || self_fail "$name: pg_isready was not run in $container: $asked"
    if grep -Eq '^exec (memory-industry-db|cuba-memorys-db)( |$)' <<<"$asked"; then
      self_fail "$name: a container was picked by its name: $asked"
    fi
    mapfile -t lines < <(tr -d '\r' <"$tmp/$name.out")
    for i in "${!lines[@]}"; do
      if [[ "${lines[i]}" == "OK  Postgres"* ]]; then
        at=$i
        break
      fi
    done
    (( at >= 0 )) || self_fail "$name: no 'OK  Postgres' line: $(cat "$tmp/$name.out")"
    [[ "${lines[at]}" == "OK  Postgres :$port (container $container)" ]] \
      || self_fail "$name: the line said '${lines[at]}', not the port and the container that answered"
    # CI-F3.3b-2: the planner names the row it starts (`dag: P env: ...`) between the OK and the banner of its block.
    planner_gap=0
    [[ "${lines[at+1]:-}" =~ ^dag:\ P\ env: ]] && planner_gap=1
    [[ "${lines[at+1+planner_gap]:-}" == "=== cargo audit ===" ]] \
      || self_fail "$name: the line after the OK line is '${lines[at+1]:-}', not the banner of cargo audit"
    if [[ -n "$password" ]] && grep -qF "$password" "$tmp/$name.out"; then
      self_fail "$name: what the gate printed carries the password of DATABASE_URL: $(cat "$tmp/$name.out")"
    fi
  }
  # dk_refused NAME CODE PASSWORD WORD...: the gate stopped with CODE (pg_stopped),
  # asked docker no `exec`, and a FAIL line holds every WORD.
  dk_refused() {
    local name="$1" want="$2" password="$3"
    shift 3
    pg_stopped "$name" "$want" "$password"
    if grep -q '^exec ' <<<"$(docker_args "$name")"; then
      self_fail "$name: the gate ran something in a container it should not have judged: $(docker_args "$name")"
    fi
    fail_line_with "$name" "$@" \
      || self_fail "$name: no FAIL line holds all of: $* : $(cat "$tmp/$name.out")"
  }

  # The container is picked by the port, whatever it is called.
  dk_green dk_port 'postgresql://gate:pw0@127.0.0.1:5489/brain' 5489 memory-industry-dbwin-20261012 pw0
  # The port is read the way libpq reads it.
  dk_green dk_ipv6 'postgresql://gate:pw0@[::1]:5434/brain' 5434 c6 pw0
  dk_green dk_default_port 'postgresql://gate:pw0@localhost/brain' 5432 c4 pw0
  dk_green dk_pgport 'postgresql://gate:pw0@localhost/brain' 5433 c5 pw0 PGPORT=5433

  # The name the gate used to trust, ready, on another port: nothing publishes
  # :5489, so the gate says that and asks no container.
  dk_prepare dk_name_trap
  dk_go dk_name_trap 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PORT=5488 \
    FAKE_DOCKER_NAMES=memory-industry-db FAKE_DOCKER_HEALTHY="memory-industry-db cuba-memorys-db"
  pg_stopped dk_name_trap 1 pw0
  grep -qF 'no running container publishes :5489' "$tmp/dk_name_trap.out" \
    || self_fail "dk_name_trap: the gate did not say that no running container publishes :5489: $(cat "$tmp/dk_name_trap.out")"
  if grep -q '^exec ' <<<"$(docker_args dk_name_trap)"; then
    self_fail "dk_name_trap: a container was asked by its name: $(docker_args dk_name_trap)"
  fi

  # Two containers publish the port: the gate does not choose, it names both.
  dk_prepare dk_two
  dk_go dk_two 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PORT=5489 \
    FAKE_DOCKER_NAMES="pgone pgtwo" FAKE_DOCKER_HEALTHY="pgone pgtwo"
  dk_refused dk_two 2 pw0 pgone pgtwo

  # The one container is there and its Postgres does not answer.
  dk_prepare dk_not_ready
  dk_go dk_not_ready 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PORT=5489 \
    FAKE_DOCKER_NAMES=pgdown FAKE_DOCKER_HEALTHY=
  pg_stopped dk_not_ready 1 pw0
  fail_line_with dk_not_ready ':5489' pgdown \
    || self_fail "dk_not_ready: no FAIL line names :5489 and the container: $(cat "$tmp/dk_not_ready.out")"

  # A server that is not this machine, or a socket, is no container's to judge:
  # pg_isready is needed, and docker is not even asked.
  dk_prepare dk_remote
  dk_go dk_remote 'postgresql://gate:pw0@db.example.invalid:5489/brain'
  dk_refused dk_remote 2 pw0 pg_isready
  [[ ! -e "$tmp/dk_remote/home/docker.args" ]] \
    || self_fail "dk_remote: docker was asked about a server that is not this machine: $(docker_args dk_remote)"
  dk_prepare dk_socket
  dk_go dk_socket 'postgresql://gate@%2Frun%2Fpostgresql:5433/brain'
  dk_refused dk_socket 2 '' pg_isready
  [[ ! -e "$tmp/dk_socket/home/docker.args" ]] \
    || self_fail "dk_socket: docker was asked about a socket: $(docker_args dk_socket)"

  # No pg_isready and no docker: the refusal names both.
  dk_prepare dk_no_docker
  rm -f "$tmp/dk_no_docker/bin/docker"
  dk_go dk_no_docker 'postgresql://gate:pw0@127.0.0.1:5489/brain'
  dk_refused dk_no_docker 2 pw0 pg_isready docker

  # docker is there and does not answer: that is not "no container".
  dk_prepare dk_daemon_down
  dk_go dk_daemon_down 'postgresql://gate:pw0@127.0.0.1:5489/brain' FAKE_DOCKER_PS_EXIT=1 \
    FAKE_DOCKER_PORT=5489 FAKE_DOCKER_NAMES=pgone FAKE_DOCKER_HEALTHY=pgone
  dk_refused dk_daemon_down 2 pw0 docker

  # J1.4 without psql: the identity query has no program to run, so the gate
  # refuses with 2 before cargo and names psql. The PATH of dk_go has none.
  fake_tree j14_nopsql "echo \"$ran_marker\""
  rm -f "$tmp/j14_nopsql/bin/psql"
  dk_go j14_nopsql 'postgresql://gate:pw0@127.0.0.1:5489/brain'
  pg_stopped j14_nopsql 2 pw0
  fail_line_with j14_nopsql psql \
    || self_fail "j14_nopsql: no FAIL line names psql: $(cat "$tmp/j14_nopsql.out")"

  # J1.5: every step after the guard gets the verified URL, and the system_identifier
  # the guard read is handed to run-all-tests.sh as GATE_SERVER_SYSTEM_ID.
  j15_url='postgresql://gate:pw0@127.0.0.1:5489/brain'
  fake_tree j15 'echo "${DATABASE_URL-unset}" >>"$HOME/rat.url"; echo "${GATE_SERVER_SYSTEM_ID-unset}" >>"$HOME/rat.sysid"; echo run-all-tests >>"$HOME/steps"'
  run_copy_as "$j15_url" j15 ""
  (( rc == 0 )) || self_fail "j15: a gate whose every step passed exited $rc: $(cat "$tmp/j15.out")"
  for f in rat.url cargo.urls crap-gate.urls mutants-gate.urls; do
    [[ -s "$tmp/j15/home/$f" ]] || self_fail "j15: $f has no line, so the step that writes it did not run"
    [[ -z "$(grep -vxF -- "$j15_url" "$tmp/j15/home/$f")" ]] \
      || self_fail "j15: $f holds a DATABASE_URL other than the verified one: $(cat "$tmp/j15/home/$f")"
  done
  [[ "$(tail -n1 "$tmp/j15/home/rat.sysid" 2>/dev/null || true)" == 7701731310526959658 ]] \
    || self_fail "j15: run-all-tests did not get the system_identifier the guard read: '$(cat "$tmp/j15/home/rat.sysid" 2>/dev/null || true)'"

  # J1.6 (A2, D-J1c): a server whose system_identifier changes under the run is refused before
  # the --features docs step, which reads the database after run-all-tests.sh. The stand-in
  # answers the identity the guard read until run-all-tests.sh has run, and then another one.
  j2_url='postgresql://gate:pw0@127.0.0.1:5489/brain'
  fake_tree j2doc 'echo run-all-tests >>"$HOME/steps"; touch "$HOME/sysid.flip"'
  printf '%s\n' '#!/bin/sh' \
    'if [ -f "$HOME/sysid.flip" ]; then echo "memory-industry-gate|999"; else echo "memory-industry-gate|7701731310526959658"; fi' > "$tmp/j2doc/bin/psql"
  chmod +x "$tmp/j2doc/bin/psql"
  run_copy_as "$j2_url" j2doc ""
  (( rc == 2 )) || self_fail "J1.6: a gate whose server changed system_identifier under run-all-tests exited $rc, not 2: $(cat "$tmp/j2doc.out")"
  grep -qF 'system_identifier' "$tmp/j2doc.out" || self_fail "J1.6: the refusal does not name system_identifier: $(cat "$tmp/j2doc.out")"
  if grep -qF '=== cargo clippy/test --features docs ===' "$tmp/j2doc.out"; then
    self_fail "J1.6: the --features docs step ran after the server changed: $(cat "$tmp/j2doc.out")"
  fi

  # gate_linux_env (gate-lock.sh): what a gate run on Linux hands to cargo, to the
  # mutation tool and to node. Each row runs it in a fresh bash with an empty
  # environment but HOME and a PATH of stand-ins, so every answer comes from the
  # function. uname, stat and node are stand-ins whose output the row picks
  # (FAKE_UNAME, FAKE_FS, FAKE_NODE); env/bare has the same without node.
  mkdir -p "$tmp/env/scripts" "$tmp/env/bin" "$tmp/env/bare" "$tmp/env/home"
  cp "$ROOT/scripts/gate-lock.sh" "$tmp/env/scripts/"
  printf '{"engines":{"node":">=18"}}\n' >"$tmp/env/package.json"
  printf '#!/bin/sh\necho "${FAKE_UNAME:-Linux}"\n' >"$tmp/env/bin/uname"
  printf '#!/bin/sh\necho "${FAKE_FS:-ext2/ext3}"\n' >"$tmp/env/bin/stat"
  printf '#!/bin/sh\necho "${FAKE_NODE:-v20.0.0}"\n' >"$tmp/env/bin/node"
  chmod +x "$tmp"/env/bin/*
  cp "$tmp/env/bin/uname" "$tmp/env/bin/stat" "$tmp/env/bare/"
  # Wrappers, not links: on Git Bash, without MSYS=winsymlinks, `ln -s` COPIES
  # the .exe, and an MSYS binary copied outside /usr/bin cannot find
  # msys-2.0.dll. dirname died, GATE_REPO_ROOT ended up as `/`, and the row
  # without node was refused for "//package.json" instead of for node (measured
  # on Windows, 2026-10-12). A PATH of the real directories would not do either:
  # on Linux /usr/bin may hold a node.
  for tool in dirname sed head mkdir; do
    path="$(command -v "$tool")"
    [[ "$path" == /* ]] || self_fail "env/bare needs $tool as a program on PATH, and command -v said '$path'"
    printf '#!/bin/sh\nexec "%s" "$@"\n' "$path" >"$tmp/env/bare/$tool"
  done
  chmod +x "$tmp"/env/bare/*
  # env_run NAME [VAR=value ...]: $rc is the exit code of gate_linux_env, NAME.out
  # what it printed and then the two variables it leaves behind.
  env_run() {
    local n="$1"
    shift
    rc=0
    env -i HOME="$tmp/env/home" PATH="$tmp/env/bin:/usr/bin:/bin" "$@" "$BASH" -c \
      'source "$1" && gate_linux_env && printf "CARGO_TARGET_DIR=%s\nTMPDIR=%s\n" "${CARGO_TARGET_DIR-unset}" "${TMPDIR-unset}"' \
      _ "$tmp/env/scripts/gate-lock.sh" >"$tmp/$n.out" 2>&1 || rc=$?
  }
  env_var() { sed -n "s/^$2=//p" "$tmp/$1.out"; }

  # The cargo target: left as it is when set, else absolute and outside the tree.
  env_run target_default
  (( rc == 0 )) || self_fail "gate_linux_env on Linux exited $rc: $(cat "$tmp/target_default.out")"
  [[ "$(env_var target_default CARGO_TARGET_DIR)" == "$tmp/env/home/.cache/cargo-target/memory-industry" ]] \
    || self_fail "with CARGO_TARGET_DIR unset the target was '$(env_var target_default CARGO_TARGET_DIR)', not under HOME/.cache/cargo-target"
  env_run target_xdg XDG_CACHE_HOME="$tmp/env/xdg-target"
  [[ "$(env_var target_xdg CARGO_TARGET_DIR)" == "$tmp/env/xdg-target/cargo-target/memory-industry" ]] \
    || self_fail "with XDG_CACHE_HOME set the target was '$(env_var target_xdg CARGO_TARGET_DIR)', not under it"
  env_run target_kept CARGO_TARGET_DIR="$tmp/env/mine"
  [[ "$(env_var target_kept CARGO_TARGET_DIR)" == "$tmp/env/mine" ]] \
    || self_fail "a CARGO_TARGET_DIR somebody set was changed to '$(env_var target_kept CARGO_TARGET_DIR)'"

  # TMPDIR: off tmpfs, because on this machine tmpfs is RAM and the mutation tool
  # copies the tree into it. Kept when set to something that is not tmpfs.
  rm -rf "$tmp/env/home/.cache"
  env_run tmp_default
  [[ "$(env_var tmp_default TMPDIR)" == "$tmp/env/home/.cache/memory-industry-gate/tmp" ]] \
    || self_fail "with TMPDIR unset it was '$(env_var tmp_default TMPDIR)', not under HOME/.cache/memory-industry-gate"
  [[ -d "$tmp/env/home/.cache/memory-industry-gate/tmp" ]] \
    || self_fail "gate_linux_env named a TMPDIR it never created"
  env_run tmp_xdg XDG_CACHE_HOME="$tmp/env/xdg-tmp"
  [[ "$(env_var tmp_xdg TMPDIR)" == "$tmp/env/xdg-tmp/memory-industry-gate/tmp" && -d "$tmp/env/xdg-tmp/memory-industry-gate/tmp" ]] \
    || self_fail "with XDG_CACHE_HOME set TMPDIR was '$(env_var tmp_xdg TMPDIR)', not a directory under it"
  mkdir -p "$tmp/env/disk" "$tmp/env/ram"
  env_run tmp_disk TMPDIR="$tmp/env/disk" FAKE_FS=ext2/ext3
  [[ "$(env_var tmp_disk TMPDIR)" == "$tmp/env/disk" ]] \
    || self_fail "a TMPDIR on a disk was changed to '$(env_var tmp_disk TMPDIR)'"
  env_run tmp_ram TMPDIR="$tmp/env/ram" FAKE_FS=tmpfs
  [[ "$(env_var tmp_ram TMPDIR)" == "$tmp/env/home/.cache/memory-industry-gate/tmp" ]] \
    || self_fail "a TMPDIR on tmpfs was left as '$(env_var tmp_ram TMPDIR)', which is RAM here"

  # node: engines.node of package.json is the oldest accepted, and a gate that
  # cannot say which node it ran with refuses with 2 and says how to fix it.
  env_run node_edge FAKE_NODE=v18.0.0
  (( rc == 0 )) || self_fail "node v18.0.0, the oldest engines.node allows, was refused (exit $rc): $(cat "$tmp/node_edge.out")"
  env_run node_absent PATH="$tmp/env/bare"
  (( rc == 2 )) || self_fail "gate_linux_env went on (exit $rc) with no node on PATH: $(cat "$tmp/node_absent.out")"
  grep -q 'node' "$tmp/node_absent.out" && grep -q 'nvm use' "$tmp/node_absent.out" \
    || self_fail "the refusal for a missing node did not say what is missing and how to fix it: $(cat "$tmp/node_absent.out")"
  env_run node_old FAKE_NODE=v16.0.0
  (( rc == 2 )) || self_fail "gate_linux_env went on (exit $rc) with node v16.0.0: $(cat "$tmp/node_old.out")"
  grep -q 'v16.0.0' "$tmp/node_old.out" && grep -q '18' "$tmp/node_old.out" && grep -q 'nvm use' "$tmp/node_old.out" \
    || self_fail "the refusal for an old node did not name its version, the one required and the fix: $(cat "$tmp/node_old.out")"
  env_run node_garbage FAKE_NODE=nonsense
  (( rc == 2 )) || self_fail "gate_linux_env went on (exit $rc) with a node whose version it cannot read: $(cat "$tmp/node_garbage.out")"

  # Not Linux: it does nothing, not even ask for node.
  env_run not_linux FAKE_UNAME=Darwin PATH="$tmp/env/bare"
  (( rc == 0 )) || self_fail "gate_linux_env refused a machine that is not Linux (exit $rc): $(cat "$tmp/not_linux.out")"
  [[ "$(env_var not_linux CARGO_TARGET_DIR)" == unset && "$(env_var not_linux TMPDIR)" == unset ]] \
    || self_fail "gate_linux_env set variables on a machine that is not Linux: $(cat "$tmp/not_linux.out")"

  # --- CI-F1 rows (spec §4, F1 and F2): the cores the gate counts, the order of the cheap
  # steps, and the boundary test-times judges. cg_tree NAME CPU_MAX builds a cgroup v2 scope
  # by hand: its cpu.max, and the `0::` line /proc/self/cgroup would give. The rows read it
  # through GATE_CGROUP_ROOT and GATE_PROC_SELF_CGROUP (gate-lock.sh, D-J5a).
  cg_tree() {
    local d="$tmp/cg/$1"
    mkdir -p "$d/root/app/pesado.scope"
    printf '%s\n' "$2" >"$d/root/app/pesado.scope/cpu.max"
    printf '0::/app/pesado.scope\n' >"$d/self"
  }

  # F1.1 (D-F1a): the gate counts its cores once, before its first cargo call: the cgroup's
  # CPUs under nproc's (8 under 24 here), exports CARGO_BUILD_JOBS with them and says so.
  cg_tree f1jobs "800000 100000"
  fake_tree f1jobs "$reads_while_running"
  run_copy f1jobs "" GATE_CGROUP_ROOT="$tmp/cg/f1jobs/root" GATE_PROC_SELF_CGROUP="$tmp/cg/f1jobs/self"
  (( rc == 0 )) || self_fail "F1.1: a gate whose every step passed exited $rc: $(cat "$tmp/f1jobs.out")"
  grep -qE '^cargo jobs=8 \(nproc=24, cgroup cpu=8' "$tmp/f1jobs.out" \
    || self_fail "F1.1: the gate did not print 'cargo jobs=8 (nproc=24, cgroup cpu=8' for 24 cores under an 8-CPU cgroup: $(grep 'cargo jobs' "$tmp/f1jobs.out" || echo 'no such line')"
  jobs_at="$(grep -n '^cargo jobs=' "$tmp/f1jobs.out" | head -1 | cut -d: -f1)"
  audit_at="$(grep -nxF '=== cargo audit ===' "$tmp/f1jobs.out" | head -1 | cut -d: -f1)"
  [[ -n "$jobs_at" && -n "$audit_at" ]] && (( jobs_at < audit_at )) \
    || self_fail "F1.1: the line that names the jobs (line '${jobs_at:-none}') is not before the first cargo call (cargo audit, line '${audit_at:-none}')"
  [[ -s "$tmp/f1jobs/home/cargo.jobs" ]] \
    || self_fail "F1.1: no cargo call was recorded, so nothing shows the CARGO_BUILD_JOBS it saw"
  if grep -qv '^8 ' "$tmp/f1jobs/home/cargo.jobs"; then
    self_fail "F1.1: a cargo call did not see CARGO_BUILD_JOBS=8: $(cat "$tmp/f1jobs/home/cargo.jobs")"
  fi

  # F1.2 (D-F1a): a CARGO_BUILD_JOBS the operator set is kept, every cargo call sees it,
  # and the line that prints it says the operator named it.
  cg_tree f1operator "800000 100000"
  fake_tree f1operator "$reads_while_running"
  run_copy f1operator "" CARGO_BUILD_JOBS=5 GATE_CGROUP_ROOT="$tmp/cg/f1operator/root" GATE_PROC_SELF_CGROUP="$tmp/cg/f1operator/self"
  (( rc == 0 )) || self_fail "F1.2: a gate with CARGO_BUILD_JOBS=5 whose every step passed exited $rc: $(cat "$tmp/f1operator.out")"
  grep -qE '^cargo jobs=5 \(.*CARGO_BUILD_JOBS' "$tmp/f1operator.out" \
    || self_fail "F1.2: no 'cargo jobs=5 (...CARGO_BUILD_JOBS...' line says the operator named the jobs: $(grep 'cargo jobs' "$tmp/f1operator.out" || echo 'no such line')"
  [[ -s "$tmp/f1operator/home/cargo.jobs" ]] \
    || self_fail "F1.2: no cargo call was recorded with CARGO_BUILD_JOBS=5"
  if grep -qv '^5 ' "$tmp/f1operator/home/cargo.jobs"; then
    self_fail "F1.2: a cargo call did not see the CARGO_BUILD_JOBS=5 the operator set: $(cat "$tmp/f1operator/home/cargo.jobs")"
  fi

  # F2: the run-all-tests stand-in of the rows below answers --check-models (its verdict is
  # FAKE_CHECK_MODELS_EXIT) and records a full run as one step. Anything else is a full run.
  run_all_steps='case "${1:-}" in --check-models) echo "run-all-tests --check-models" >>"$HOME/steps"; exit "${FAKE_CHECK_MODELS_EXIT:-0}" ;; esac
echo run-all-tests >>"$HOME/steps"
exit 0'

  # F2.1 (D-F2a): fmt, codigo-muerto, npm and the models run after cargo deny and before the
  # first cargo call that compiles.
  fake_tree f2order "$run_all_steps"
  run_copy f2order ""
  (( rc == 0 )) || self_fail "F2.1: a gate whose every step passed exited $rc: $(cat "$tmp/f2order.out")"
  steps="$tmp/f2order/home/steps"
  deny_at="$(grep -nxF 'cargo deny check licenses bans sources' "$steps" | head -1 | cut -d: -f1)"
  build_at="$(grep -nE '^cargo (clippy|build|test|llvm-cov|mutants)' "$steps" | head -1 | cut -d: -f1)"
  for step in 'cargo fmt --check' 'codigo-muerto' 'node npm/install.test.js' 'run-all-tests --check-models'; do
    at="$(grep -nxF "$step" "$steps" | head -1 | cut -d: -f1)"
    [[ -n "$deny_at" && -n "$at" && -n "$build_at" ]] && (( at > deny_at && at < build_at )) \
      || self_fail "F2.1: '$step' is not after cargo deny (line '${deny_at:-none}') and before the first cargo call that compiles (line '${build_at:-none}'): $(cat "$steps")"
  done

  # F2.2 (D-F2a): a cheap step that fails stops the gate with exit 1 before any cargo call
  # that compiles, and before the full run of run-all-tests.
  f2_cheap_red() {
    local name="$1" fail_at="$2"
    shift 2
    fake_tree "$name" "$run_all_steps"
    run_copy "$name" "$fail_at" "$@"
    (( rc == 1 )) || self_fail "F2.2 $name: a gate whose cheap step failed exited $rc, not 1: $(cat "$tmp/$name.out")"
    if grep -qE '^cargo (clippy|build|test|llvm-cov|mutants)' "$tmp/$name/home/steps" 2>/dev/null; then
      self_fail "F2.2 $name: a cheap step failed and the gate compiled anyway: $(cat "$tmp/$name/home/steps")"
    fi
    if grep -qxF 'run-all-tests' "$tmp/$name/home/steps" 2>/dev/null; then
      self_fail "F2.2 $name: the full run of run-all-tests started after a cheap step failed"
    fi
  }
  f2_cheap_red f2fmt fmt
  f2_cheap_red f2codigo "" FAKE_CODIGO_MUERTO_EXIT=1
  f2_cheap_red f2node "" FAKE_NODE_EXIT=1
  f2_cheap_red f2models "" FAKE_CHECK_MODELS_EXIT=1

  # F2.3 (D-F2c): test-times stops at a line of its own, printed after the --features docs
  # step, and not at the npm banner, which now runs before the first build.
  fake_tree f2until "$run_all_steps"
  run_copy f2until ""
  (( rc == 0 )) || self_fail "F2.3: a gate whose every step passed exited $rc: $(cat "$tmp/f2until.out")"
  until_arg="$(sed -n 's/^test-times .*--until //p' "$tmp/f2until/home/steps" | tail -1)"
  [[ "$until_arg" == '=== end of timed steps ===' ]] \
    || self_fail "F2.3: test-times was told to stop at '$until_arg', not at the line the gate prints after the --features docs step ('=== end of timed steps ===')"
  seen="$tmp/f2until/home/times.seen"
  docs_at="$(grep -nxF '=== cargo clippy/test --features docs ===' "$seen" | head -1 | cut -d: -f1)"
  bound_at="$(grep -nxF -- "$until_arg" "$seen" | head -1 | cut -d: -f1)"
  [[ -n "$docs_at" && -n "$bound_at" ]] && (( bound_at > docs_at )) \
    || self_fail "F2.3: the boundary '$until_arg' is not a line of the log after the --features docs step (docs at '${docs_at:-none}', boundary at '${bound_at:-none}')"

  # CI-F3.3b-2 (D-F3b, spec F3.3b-1b): the steps of the judge are the rows of scripts/gate-dag.txt, run by the
  # planner (gate-dag.py) after the preamble and before the boundary line. Each row is `merge-gate.sh --step <id>`,
  # which the planner starts with its lane ticket (D-F3k).
  fake_tree planner "$run_all_steps"
  run_copy planner ""
  (( rc == 0 )) || self_fail "planner: a gate whose every step passed exited $rc: $(cat "$tmp/planner.out")"
  dag_order="$(sed -n 's/^dag: \([A-Za-z0-9]*\) env:.*/\1/p' "$tmp/planner.out" | tr '\n' ' ')"
  [[ "$dag_order" == "P DBG B3 REL E2E DC CV M " ]] \
    || self_fail "planner: the planner did not start the rows of gate-dag.txt in idx order (it started: '$dag_order')"
  # The boundary line is printed by the planner before the first untimed row (CV), after the block of docs and before
  # the block of crap-gate: the judge of the test times reads up to it, so CV and M are not judged (CI-F3.3b-2).
  dc_at="$(grep -nxF '=== cargo clippy/test --features docs ===' "$tmp/planner.out" | head -1 | cut -d: -f1)"
  bound_at="$(grep -nxF '=== end of timed steps ===' "$tmp/planner.out" | head -1 | cut -d: -f1)"
  crap_at="$(grep -nxF '=== crap-gate (coverage floor) ===' "$tmp/planner.out" | head -1 | cut -d: -f1)"
  mut_at="$(grep -nxF '=== mutants-gate ===' "$tmp/planner.out" | head -1 | cut -d: -f1)"
  sum_at="$(grep -n '^dag: T=' "$tmp/planner.out" | head -1 | cut -d: -f1)"
  times_at="$(grep -nxF '=== test times (scripts/test-time-ceilings.txt) ===' "$tmp/planner.out" | head -1 | cut -d: -f1)"
  [[ -n "$dc_at" && -n "$bound_at" && -n "$crap_at" && -n "$mut_at" && -n "$sum_at" && -n "$times_at" ]] \
    && (( dc_at < bound_at && bound_at < crap_at && crap_at < mut_at && mut_at < sum_at && sum_at < times_at )) \
    || self_fail "planner: the boundary is not printed between the docs block and the untimed rows (docs at ${dc_at:-none}, boundary at ${bound_at:-none}, crap-gate at ${crap_at:-none}, mutants-gate at ${mut_at:-none}, summary at ${sum_at:-none}, test times at ${times_at:-none})"
  # A red step stops the planner at its idx (D-F3d): the steps after it do not start, the gate exits with the step's
  # own code, and neither the mutation step nor the test times run.
  fake_tree planner_red "$run_all_steps"
  run_copy_as "$judge_test_url" planner_red "" FAKE_CRAP_EXIT=1
  (( rc == 1 )) || self_fail "planner red: crap-gate exited 1 and the gate exited $rc, not 1: $(cat "$tmp/planner_red.out")"
  grep -qxF 'mutants-gate' "$tmp/planner_red/home/steps" \
    && self_fail "planner red: mutants-gate started after crap-gate went red: $(steps_of planner_red)"
  grep -q '^test-times ' "$tmp/planner_red/home/steps" \
    && self_fail "planner red: the test times ran after a red step: $(steps_of planner_red)"
  grep -qF 'FAIL: CV exited 1' "$tmp/planner_red.out" \
    || self_fail "planner red: the planner does not name the red row CV: $(cat "$tmp/planner_red.out")"
  [[ "$(exit_file planner_red)" == 1 ]] \
    || self_fail "planner red: the exit file says '$(exit_file planner_red)', not 1"
  # CI-F3.3b-2 (spec F3.3b-1b-iii): the rows that are not timed are CV and M, the rows after the docs step. The judge of
  # the test times reads this gate's log only up to the boundary line the planner prints before the first of them, so a
  # crap-gate whose output carries a timed `finished in` line does not change the judgement, and the boundary is not the
  # end of the planner (that would judge CV and M too).
  fake_tree untimed "$run_all_steps"
  printf '%s\n' '#!/bin/sh' 'echo crap-gate >>"$HOME/steps"' \
    'echo "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 99.00s"' \
    'exit 0' >"$tmp/untimed/scripts/crap-gate.sh"
  run_copy untimed ""
  (( rc == 0 )) || self_fail "untimed: a gate whose every step passed exited $rc: $(cat "$tmp/untimed.out")"
  judged="$tmp/untimed/home/times.seen"
  until_arg="$(sed -n 's/^test-times .*--until //p' "$tmp/untimed/home/steps" | tail -1)"
  [[ "$until_arg" == '=== end of timed steps ===' ]] \
    || self_fail "untimed: the judge of the test times was told to stop at '$until_arg', not at the boundary line of the planner"
  grep -qxF '=== cargo clippy/test --features docs ===' "$judged" \
    || self_fail "untimed: the judge does not hold the docs block, the last timed row: $(cat "$judged" 2>/dev/null)"
  grep -qF 'finished in 99.00s' "$judged" \
    && self_fail "untimed: the output of an untimed row (crap-gate, a finished-in line) reached the judge of the test times"
  # The step mode is not a gate: it takes no lock and writes no verdict, and it runs only with the ticket the planner
  # hands it (D-F3k). Without one it exits 2 before any step, and says what it needs.
  fake_tree step_alone "$run_all_steps"
  step_rc=0
  env -i PATH="$tmp/step_alone/bin:$gitdir:/usr/bin:/bin" HOME="$tmp/step_alone/home" \
    "$BASH" "$tmp/step_alone/scripts/merge-gate.sh" --step DBG >"$tmp/step_alone.out" 2>&1 || step_rc=$?
  (( step_rc == 2 )) || self_fail "--step without a ticket exited $step_rc, not 2: $(cat "$tmp/step_alone.out")"
  grep -qF -- '--step' "$tmp/step_alone.out" \
    || self_fail "--step without a ticket does not say what it needs: $(cat "$tmp/step_alone.out")"
  [[ -z "$(steps_of step_alone)" ]] || self_fail "--step without a ticket ran a step: $(steps_of step_alone)"
  # The tree starts its exit file at 0 (fake_tree): a step that wrote the verdict would change it.
  [[ ! -e "$tmp/step_alone/home/.cache/cuba-gate/lock" && "$(exit_file step_alone)" == 0 ]] \
    || self_fail "--step without a ticket took the gate lock or wrote the exit file: '$(exit_file step_alone)'"
  # The planner's ticket, its owner alive and an ancestor of the step (this shell): the step checks the ticket, starts
  # its lane with the token in the environment of the lane's run-all-tests, and leaves the lock and the ticket as they
  # were, because the lane is the one that spends the ticket (gate-lock.sh, lane_token_check).
  step_run_all='echo "run-all-tests${*:+ $*} token=${CUBA_GATE_LANE_TOKEN-unset}" >>"$HOME/steps"'
  fake_tree step_ok "$step_run_all"
  step_lock="$tmp/step_ok/home/.cache/cuba-gate/lock"
  mkdir -p "$step_lock/lanes"
  owner_record "$$" fixture-owner >"$step_lock/owner"
  printf '%s' tok-planted >"$step_lock/lanes/20-DBG"
  step_owner_before="$(cat "$step_lock/owner")"
  step_rc=0
  env -i PATH="$tmp/step_ok/bin:$gitdir:/usr/bin:/bin" HOME="$tmp/step_ok/home" GATE_LOCK="$step_lock" \
    CUBA_GATE_LOCK_OWNER="$step_owner_before" CUBA_GATE_LANE_TOKEN=tok-planted \
    "$BASH" "$tmp/step_ok/scripts/merge-gate.sh" --step DBG >"$tmp/step_ok.out" 2>&1 || step_rc=$?
  (( step_rc == 0 )) || self_fail "--step with its ticket exited $step_rc, not 0: $(cat "$tmp/step_ok.out")"
  [[ "$(grep -E '^(up |down |run-all-tests)' "$tmp/step_ok/home/steps")" == "$(printf '%s\n' 'up dbg' 'run-all-tests --lane CL --lane D1 --lane D2 --lane NLI --lane D3 token=tok-planted' 'down dbg')" ]] \
    || self_fail "--step with its ticket did not run the lane of DBG with the token in its environment: $(steps_of step_ok)"
  [[ "$(cat "$step_lock/lanes/20-DBG")" == tok-planted ]] \
    || self_fail "--step spent the ticket its lane has to spend"
  [[ "$(cat "$step_lock/owner")" == "$step_owner_before" ]] \
    || self_fail "--step changed the owner record of the gate lock"
  [[ "$(exit_file step_ok)" == 0 ]] \
    || self_fail "--step wrote the exit file of the gate that owns the lock: '$(exit_file step_ok)'"
  # A ticket that is not the planner's (here, another token) is refused by name, before any lane starts.
  fake_tree step_foreign "$step_run_all"
  step_lock="$tmp/step_foreign/home/.cache/cuba-gate/lock"
  mkdir -p "$step_lock/lanes"
  owner_record "$$" fixture-owner >"$step_lock/owner"
  printf '%s' tok-planted >"$step_lock/lanes/20-DBG"
  step_rc=0
  env -i PATH="$tmp/step_foreign/bin:$gitdir:/usr/bin:/bin" HOME="$tmp/step_foreign/home" GATE_LOCK="$step_lock" \
    CUBA_GATE_LOCK_OWNER="$(cat "$step_lock/owner")" CUBA_GATE_LANE_TOKEN=tok-other \
    "$BASH" "$tmp/step_foreign/scripts/merge-gate.sh" --step DBG >"$tmp/step_foreign.out" 2>&1 || step_rc=$?
  (( step_rc == 2 )) || self_fail "--step with a foreign ticket exited $step_rc, not 2: $(cat "$tmp/step_foreign.out")"
  grep -qF 'lane token refused' "$tmp/step_foreign.out" \
    || self_fail "--step with a foreign ticket does not say it was refused: $(cat "$tmp/step_foreign.out")"
  [[ -z "$(steps_of step_foreign)" ]] || self_fail "--step with a foreign ticket ran a step: $(steps_of step_foreign)"
  [[ "$(cat "$step_lock/lanes/20-DBG")" == tok-planted ]] || self_fail "--step with a foreign ticket spent the planner's ticket"

  echo "OK  self-test: the exit file says running while a gate runs and holds the whole"
  echo "    gate's verdict at the end, a step after the tests included; a gate killed"
  echo "    in the middle leaves 'running', never an old 0; a refused gate leaves it alone."
  echo "    A green gate over a clean tree leaves a receipt for its commit; an untracked"
  echo "    file, a tree or HEAD that changed during the run and an exit 0 over SKIPPED"
  echo "    leave none and say why; a red run on a commit removes its earlier receipt."
  echo "    gate_linux_env keeps a CARGO_TARGET_DIR that was set and otherwise puts it"
  echo "    outside the tree, moves a TMPDIR that is unset or tmpfs under the cache, keeps"
  echo "    one on a disk, refuses with 2 a node that is missing, older than engines.node"
  echo "    or unreadable, and does nothing off Linux. In the gate itself"
  echo "    the readiness check hands DATABASE_URL to pg_isready -d as it is, names the"
  echo "    port libpq answered on, stops with 1 when nothing answers and with 2 when"
  echo "    libpq cannot read the URL or its answer has no port, never prints what libpq"
  echo "    wrote about the URL, and refuses with 2, before asking, a URL that"
  echo "    brain_gate cannot be derived from"
  echo "    or that holds more than one '@' before the host."
  echo "    a cargo target inside rust/ (a target-* with CACHEDIR.TAG, or a CARGO_TARGET_DIR there) is"
  echo "    refused with 2 before cargo audit, not after the tests;"
  echo "    cargo audit runs first, before cargo deny and before the tests, under timeout 120;"
  echo "    a database it cannot refresh is exit 2, never a skip; without pg_isready the"
  echo "    readiness check asks the container that publishes the port of DATABASE_URL, never one by name."
  echo "    the test times are judged last, over the gate's own log up to the npm step, and their red is the gate's"
  echo "    advisory exceptions are judged right after cargo audit, from a report made without ignores, and their red stops the gate before cargo deny;"
  echo "    a gate stopped by a signal it can catch (TERM) leaves 2 in the exit file, says so and leaves no receipt, never the 0 of its last command"
  exit 0
fi
# --- self-test end ---

# The receipt scripts/release.sh publishes from instead of running this gate a
# second time (gate-lock.sh says why). It names one commit, so it is written
# only when that commit is all that was judged: the tree clean, untracked files
# included, when the gate starts and when it ends, and HEAD where it started.
# The log it is judged from is this run's own output, teed below, and the judge
# is gate_log_verdict, the one release.sh uses on its own log.
GATE_LOG="$HOME/.cache/cuba-gate/merge-gate.log"
GATE_START_HEAD=""
GATE_NO_RECEIPT=""
GATE_TEE=""

write_receipt() {
  local status receipt
  if [[ -n "$GATE_NO_RECEIPT" ]]; then
    echo "no receipt: $GATE_NO_RECEIPT"
    return 0
  fi
  if [[ "$(git -C "$ROOT" rev-parse --verify -q HEAD 2>/dev/null || true)" != "$GATE_START_HEAD" ]]; then
    echo "no receipt: HEAD moved from $GATE_START_HEAD while the gate ran, so no one commit was judged whole"
    return 0
  fi
  if ! status="$(git -C "$ROOT" status --porcelain)" || [[ -n "$status" ]]; then
    echo "no receipt: the working tree changed while the gate ran, so what passed is not $GATE_START_HEAD"
    return 0
  fi
  if ! gate_log_verdict "$GATE_LOG"; then
    echo "no receipt: $GATE_LOG_PROBLEM"
    return 0
  fi
  receipt="$GATE_RECEIPTS/$GATE_START_HEAD"
  if mkdir -p "$GATE_RECEIPTS" \
    && gate_receipt_lines "$GATE_START_HEAD" >"$receipt.new.$$" \
    && mv -f "$receipt.new.$$" "$receipt"; then
    echo "receipt: $receipt (scripts/release.sh publishes $GATE_START_HEAD from it without running this gate again)"
  else
    rm -f "$receipt.new.$$"
    echo "no receipt: $receipt could not be written"
  fi
}

# The exit file is this gate's whole verdict, not its first half's: the rules
# are in gate-lock.sh, next to the lock whose holder owns the file. The tee is
# drained first, so the log the receipt is judged from holds the last line. The
# wait is bounded: a process the gate started and left behind (a server the
# E2E launched) keeps the pipe open and tee would never see its end, and an
# unbounded wait would hang the exit of a gate that has already finished.
#
# A signal bash can catch (TERM from a timeout or a service manager, HUP from a closed
# terminal) runs this trap with the status of the last command, and a gate stopped
# between two steps has a 0 there: the exit file would read as a pass over a run that
# finished nothing (found on 2026-10-20, stopping a gate during cargo test). Only the
# last line of the script sets GATE_REACHED_END, so an exit of 0 without it is not a
# verdict, and 2 is what this judge says when it could not judge. SIGKILL runs no trap
# at all and leaves `running`, which is why the file starts there. The status of the
# process itself is the signal's (143, 129): bash raises it again after this trap, so
# the verdict is the file, and the process just has not passed.
GATE_REACHED_END=""
on_exit() {
  local code=$? tenths=0 stopped=0
  gate_cluster_down_all
  if (( code == 0 )) && [[ -z "$GATE_REACHED_END" ]]; then
    code=2
    stopped=1
  fi
  if [[ -n "$GATE_TEE" ]]; then
    exec 1>&3 2>&4 3>&- 4>&-
    while kill -0 "$GATE_TEE" 2>/dev/null && (( tenths < 300 )); do
      sleep 0.1
      tenths=$((tenths + 1))
    done
    if kill -0 "$GATE_TEE" 2>/dev/null; then
      echo "note: something this gate started still holds its output open; $GATE_LOG was judged as it stood after 30 s"
    fi
  fi
  (( stopped == 0 )) || echo "FAIL: the gate was stopped before its last step (a signal), so it has no verdict: exit 2, no receipt."
  (( code != 0 )) || write_receipt || echo "no receipt: writing it failed"
  exit_file_verdict "$code"
  release_gate_lock
}

acquire_gate_lock "$GATE_LOCK" || exit 1
exit_file_running
trap on_exit EXIT
# bash does not run the EXIT trap for an INT that arrives while a child is running (measured in the
# D2 rows): this trap makes the verdict 2 and lets on_exit take the lanes down and write the exit file.
trap 'exit 2' INT
export CUBA_GATE_LOCK_OWNER="$GATE_OWNER"
# CI-F3.3b-1b-i: the planner writes its lane tickets under this lock's directory (gate-dag.py).
export GATE_LOCK

# Whatever this commit's earlier receipt said, this run is its verdict now: a
# red run on it takes it away, and a green one writes it again. A dirty start
# judges no commit, so it leaves every receipt alone.
GATE_START_HEAD="$(git -C "$ROOT" rev-parse --verify -q HEAD 2>/dev/null || true)"
if [[ -z "$GATE_START_HEAD" ]]; then
  GATE_NO_RECEIPT="$ROOT has no commit git can name, so a pass here is not a pass of any commit"
elif ! gate_start_status="$(git -C "$ROOT" status --porcelain)" || [[ -n "$gate_start_status" ]]; then
  GATE_NO_RECEIPT="the working tree was not clean when the gate started, so what it judges is not $GATE_START_HEAD"
else
  rm -f "$GATE_RECEIPTS/$GATE_START_HEAD"
fi
mkdir -p "$(dirname "$GATE_LOG")"
exec 3>&1 4>&2
exec > >(tee "$GATE_LOG") 2>&1
GATE_TEE=$!
[[ -z "$GATE_NO_RECEIPT" ]] || echo "note: this run will leave no receipt for scripts/release.sh: $GATE_NO_RECEIPT"
# D-F2k: a DATABASE_URL of the operator is the server of every step, and no lane is made. Said before the
# banner, so the lines the readiness check prints stay where they were.
if [[ -n "${DATABASE_URL:-}" ]]; then
  echo "note: DATABASE_URL names a server this judge did not make, so every step shares that server and no lane is made (D-F2k)"
fi

# CARGO_BUILD_JOBS for every cargo call of this run, counted once (CI-F1, D-F1a): the CPUs
# of the cgroup this gate runs in, under nproc's. Before the banner, because the banner
# and the OK lines of the judge server are read as one block (the first cargo call is
# still after all of them).
gate_cargo_jobs
# The value the function settled on, exported here as well: the gate's own export is the
# judge's line for every cargo call of the run, so it reads here and not only in gate-lock.sh.
export CARGO_BUILD_JOBS="$GATE_CARGO_JOBS"

echo "╔══════════════════════════════════════════════════════════╗"
echo "║  CUBA-MEMORYS MERGE GATE (local CI — sole merge judge)   ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""
echo "WHAT THIS GATE DOES NOT CHECK — read before trusting a green run:"
echo "  · the reranker in E2E   e2e_all_tools.py forces CUBA_RERANKER_PATH at an"
echo "                          empty dir so its calls exercise the identity"
echo "                          fallback. mcp_live_session_test.py inherits your"
echo "                          shell and DOES use the real reranker. Two suites,"
echo "                          opposite policies — both must stay."
echo "  · GPU placement         only the DECISION, not the kernel. After the E2E,"
echo "                          scripts/gpu-placement-check.sh reads doctor --json"
echo "                          and fails when the gpu check disagrees with this"
echo "                          machine: a fallback to CPU where an NVIDIA device"
echo "                          AND the provider libraries are both present, or a"
echo "                          check that never names the CPU on a machine that"
echo "                          has neither. That a kernel really executed on the"
echo "                          GPU cannot be proven without a card — there is"
echo "                          none in CI — and is still not covered here."
echo "  · retrieval quality     the eval step is a smoke run. It proves the"
echo "                          harness executes; it asserts no nDCG threshold."
echo "  · other platforms       this judge runs on the merge machine (typically"
echo "                          Linux x64). Cross-targets are not exercised here."
echo ""
echo "WHAT IT REQUIRES (missing = FAIL, never SKIPPED):"
echo "  · ONNX embed + ORT      ~/.cache/memory-industry/models + onnxruntime"
echo "                          (legacy ~/.cache/cuba-memorys still resolved)"
echo "  · NLI + reranker        models-nli + reranker (memory-industry models all)"
echo "  · generative LLM        MEMORY_INDUSTRY_LLM_PROVIDER=deepseek|qwen|moonshot|…"
echo "                          (+ API key) OR MEMORY_INDUSTRY_LLM_BASE_URL=/v1"
echo "                          (any OpenAI-compat: CN/US/EU clouds or Ollama)"
echo "                          OR authenticated claude/gemini CLI OR MCP sampling"
echo "  · cargo-deny, machete   install on the merge machine"
echo "  · network to github.com cargo audit refreshes the RustSec advisory database first, at most 120 s"
echo "  · node                  >= engines.node of package.json, on PATH (nvm use)"
echo ""
echo "WHERE IT WRITES:"
echo "  · every mutating step  a throwaway database (GATE_DB, default brain_gate),"
echo "                         created before and dropped after. Your real corpus"
echo "                         is never a test fixture."
echo "  · the eval step        a throwaway database (brain_gate_eval), seeded from the"
echo "                         synthetic fixture rust/eval-datasets/smoke-corpus.jsonl."
echo "                         It never reads the real corpus."
echo "  · its own verdict      ~/.cache/cuba-gate/merge-gate.log, and on a green"
echo "                         run over a clean tree receipts/<sha> next to it,"
echo "                         which scripts/release.sh tags from instead of"
echo "                         running this gate a second time."
echo ""

# The cargo target and TMPDIR outside the tree and off tmpfs, and a node that
# package.json accepts, before any step needs them (gate-lock.sh says why). It
# runs after the lock so that a second gate is refused first, and a refusal here
# is this gate's verdict: exit 2, in the exit file, with the lock released.
gate_linux_env

# A cargo target inside rust/ (or a CARGO_TARGET_DIR that will be one) is copied
# whole into every job of the mutation step, the last of this gate: it used to be
# refused there, ~20 min of tests later. Nothing is printed unless it refuses; the
# rule lives in mutants-gate.sh, which is also where the step applies it.
refuse_rc=0
refuse_out="$("$ROOT/scripts/mutants-gate.sh" --refuse-targets 2>&1)" || refuse_rc=$?
if (( refuse_rc != 0 )); then
  printf '%s\n' "$refuse_out" >&2
  exit "$refuse_rc"
fi

# No default server (D-J1e): the URL is the operator's. An empty one is refused
# here, before anybody is asked, and a shape run-all-tests.sh cannot derive
# brain_gate from is refused before libpq is asked, for the reason gate-lock.sh
# gives: it would judge one server ready and create the throwaway databases on another.
export DATABASE_URL="${DATABASE_URL:-}"
GATE_LANES=""
if [[ -z "$DATABASE_URL" && "$(uname -s)" == Linux ]]; then
  GATE_LANES=1
fi
if [[ -n "$GATE_LANES" ]]; then
  # A lane reads its own identity when it is made: an inherited GATE_SERVER_SYSTEM_ID is not this run's.
  unset GATE_SERVER_SYSTEM_ID
  # D-F2c, D-F2j: with no DATABASE_URL on Linux, each step gets a lane this gate makes under the cluster
  # root. The library's cheap checks come before the audit (a judge without the binaries or the locale is
  # refused in seconds), and the sweep takes what a judge that died left behind, before the audit too.
  gate_cluster_check
  gate_cluster_sweep
else
  gate_require_given_database_url "$DATABASE_URL"

# Without pg_isready the server is asked through the container that publishes the
# port DATABASE_URL names, not through one picked by name: the name says nothing
# about which server the URL points at (the judge's container on Windows was
# memory-industry-dbwin-20261012 on :5489, and a memory-industry-db on another
# port would have been judged ready in its place). publish= matches the host
# port, not the container's (measured on Docker 29.7.2), and the role and the
# database do not change what pg_isready answers, so it is asked with neither.
# Nothing printed carries DATABASE_URL: the password lives in it.
gate_container_ready() {
  local authority hostport host port="" names name running=() answer rc=0
  authority="${1#*://}"
  authority="${authority%%/*}"
  hostport="${authority##*@}"
  if [[ "$hostport" == \[*\]* ]]; then
    host="${hostport%%]*}]"
    [[ "${hostport#*]}" == :* ]] && port="${hostport#*]:}"
  else
    host="${hostport%%:*}"
    [[ "$hostport" == *:* ]] && port="${hostport##*:}"
  fi
  port="${port:-${PGPORT:-5432}}"
  [[ "$port" =~ ^[0-9]+$ ]] \
    || gate_refuse "DATABASE_URL (or PGPORT) names a port that is not a number, so no container can be asked for it."
  case "$host" in
    127.0.0.1|localhost|"[::1]") ;;
    *) gate_refuse "pg_isready is not on PATH, and DATABASE_URL names a server that is not this machine, which no local container can answer for. Install the PostgreSQL client tools so pg_isready exists." ;;
  esac
  command -v docker >/dev/null 2>&1 \
    || gate_refuse "pg_isready and docker are both missing from PATH, so nothing can say whether the Postgres on :$port is ready. Install the PostgreSQL client tools (pg_isready) or docker."
  names="$(docker ps --filter "publish=$port" --format '{{.Names}}')" \
    || gate_refuse "docker did not answer 'docker ps', so it cannot say which container publishes :$port. Start docker or install pg_isready."
  while IFS= read -r name; do
    name="${name%$'\r'}"
    [[ -z "$name" ]] || running+=("$name")
  done <<<"$names"
  if (( ${#running[@]} == 0 )); then
    echo "FAIL: Postgres not ready on :$port (no pg_isready on PATH, and no running container publishes :$port)"
    exit 1
  elif (( ${#running[@]} > 1 )); then
    gate_refuse "more than one container publishes :$port (${running[*]}), so this gate will not choose one to judge. Stop the others or install pg_isready."
  fi
  name="${running[0]}"
  answer="$(docker exec "$name" pg_isready 2>&1)" || rc=$?
  answer="${answer//$'\r'/}"
  if (( rc != 0 )); then
    echo "FAIL: Postgres not ready on :$port (container $name: $answer)"
    exit 1
  fi
  echo "OK  Postgres :$port (container $name)"
}

# A server identity from an earlier run must not satisfy the first guard: the one exported
# below is the one this run reads (D-J1c).
unset GATE_SERVER_SYSTEM_ID
if command -v pg_isready >/dev/null 2>&1; then
  # libpq reads the URL, so host, port, socket, IPv6 and PGPORT mean here what
  # they mean to psql, pg_dump and sqlx, and the port printed is the one libpq
  # went to ("<host>:<port> - <status>"; the port is after the LAST colon, for
  # IPv6). Its stderr is dropped: libpq quotes the piece of the URL it cannot
  # read, and that piece can be the password.
  pg_rc=0
  pg_answer="$(pg_isready -d "$DATABASE_URL" 2>/dev/null)" || pg_rc=$?
  if (( pg_rc == 3 )); then
    gate_refuse "pg_isready could not read DATABASE_URL, so no server was asked. Run pg_isready -d \"\$DATABASE_URL\" by hand to see libpq's own words (they can quote the password, so not in this log)."
  fi
  pg_port="${pg_answer% - *}"
  pg_port="${pg_port##*:}"
  [[ "$pg_answer" == *" - "* && "$pg_port" =~ ^[0-9]+$ ]] \
    || gate_refuse "pg_isready answered '$pg_answer', which names no port, so this gate cannot say which server it asked. It will not guess one."
  (( pg_rc == 0 )) \
    || {
      echo "FAIL: Postgres not ready on :$pg_port (pg_isready: $pg_answer)"
      echo "     the judge's server is started as docs/gate.md says (cluster_name=memory-industry-gate); this gate does not try another one"
      exit 1
    }
  echo "OK  Postgres :$pg_port"
  gate_require_judge_server "$DATABASE_URL"
else
  # Without pg_isready the judge is asked first: the container's own readiness line
  # comes after its OK, so the line after "OK  Postgres" is the next step.
  gate_require_judge_server "$DATABASE_URL"
  gate_container_ready "$DATABASE_URL"
fi
# The server DATABASE_URL names is the judge's only if it says so: cluster_name
# marked by the judge's start command, read read-only from the same server (D-J1a).
# Before cargo audit, the first step that uses the network. The system_identifier it
# read goes to run-all-tests.sh, which reads it again and refuses a server that changed (D-J1c).
export GATE_SERVER_SYSTEM_ID="$GATE_JUDGE_SYSID"
fi

# One lane per step (D-F2c): the step's server is made here, and its URL, GATE_LANE and
# GATE_SERVER_SYSTEM_ID are in the environment of that step only. The lane is taken down after its step,
# whatever the step said. With a DATABASE_URL of the operator there are no lanes: the step runs as it did.
gate_lane_up() {
  gate_cluster_up "$1"
  GATE_LANE="$1" gate_require_judge_server "$GATE_CLUSTER_URL"
  LANE_URL="$GATE_CLUSTER_URL"
  LANE_SYSID="$GATE_JUDGE_SYSID"
}
run_in_lane() {
  local lane="$1" rc=0
  shift
  if [[ -z "$GATE_LANES" ]]; then
    "$@"
    return
  fi
  gate_lane_up "$lane"
  env DATABASE_URL="$LANE_URL" GATE_LANE="$lane" GATE_SERVER_SYSTEM_ID="$LANE_SYSID" GATE_CLUSTER_ROOT="$GATE_CLUSTER_ROOT" "$@" || rc=$?
  gate_cluster_down "$lane"
  return "$rc"
}

# The advisories are judged before the 20-minute test run: a judge idle for days
# found its new advisory half an hour in, when 3 s would have said it. cargo audit
# is the only judge of them (cargo deny below reads licenses, bans and sources,
# never `advisories`): two judges would keep two ignore lists, and they already
# differ. It fetches its database itself, so the ceiling is on the whole run;
# measured here, 2.4 s with the database current, 11.6 s from a cold clone, and
# 20.2 s to give up on a server that does not answer. A database that could not
# be refreshed is exit 2, never a pass against the copy on disk and never a retry
# with --no-fetch or --stale.
echo "=== cargo audit ==="
audit_rc=0
audit_out="$(cd "$ROOT/rust" && timeout 120 cargo audit 2>&1)" || audit_rc=$?
printf '%s\n' "$audit_out"
if (( audit_rc == 124 )); then
  gate_refuse "cargo audit did not finish refreshing the advisory database from https://github.com/RustSec/advisory-db in 120 s. This gate does not judge advisories against the copy on disk; restore the network to github.com and run it again."
elif (( audit_rc != 0 )) && [[ "$audit_out" == *"couldn't fetch advisory database"* ]]; then
  gate_refuse "cargo audit could not fetch the advisory database from https://github.com/RustSec/advisory-db. This gate does not judge advisories against the copy on disk; restore the network to github.com and run it again."
elif (( audit_rc != 0 )); then
  echo "FAIL: cargo audit exited $audit_rc (its output is above; with no advisory listed, check that timeout is GNU coreutils)"
  exit 1
fi

# The ignored advisories are judged next, while the database the call above
# refreshed is still current. cargo audit only says what is NOT ignored, so the
# judge needs a second report made without ignores, from a directory of its own
# so that rust/.cargo/audit.toml does not apply: every excepted advisory is in it.
# That call takes 0.42 s measured and reads the lockfile alone, so --no-fetch is
# right here and nowhere else: a second fetch would add a network failure and no
# information. Its exit does not count (without ignores it exits 1 exactly when the
# exceptions are real); the judge's does, and what the call wrote on stderr is
# printed when the judge cannot read the report.
echo "=== advisory exceptions (scripts/advisory-exceptions.txt) ==="
adv_dir="$(mktemp -d)"
adv_lock="$(cygpath -m "$ROOT/rust/Cargo.lock" 2>/dev/null || printf '%s' "$ROOT/rust/Cargo.lock")"
(cd "$adv_dir" && timeout 120 cargo audit --json --no-fetch -f "$adv_lock" >audit.json 2>audit.err) || true
adv_rc=0
"$ROOT/scripts/advisory-exceptions.sh" --judge "$adv_dir/audit.json" || adv_rc=$?
if (( adv_rc == 2 )); then
  cat "$adv_dir/audit.err" >&2
fi
rm -rf "$adv_dir"
(( adv_rc == 0 )) || exit "$adv_rc"

echo "=== cargo deny ==="
if ! command -v cargo-deny >/dev/null 2>&1 && ! (cd "$ROOT/rust" && cargo deny --version >/dev/null 2>&1); then
  echo "FAIL: cargo-deny is not installed. Install it (cargo install cargo-deny) — the gate does not skip license policy." >&2
  exit 1
fi
(cd "$ROOT/rust" && cargo deny check licenses bans sources)

# The cheap steps before the first compile (CI-F1, D-F2a and D-F2b): a red here is seconds,
# not the twenty minutes of run-all-tests.sh. fmt runs again in run-all-tests.sh, a second
# look of one second that keeps that script whole when it runs alone. The models are asked
# by run-all-tests.sh --check-models, with the paths its own run exports.
echo "=== cargo fmt --check ==="
(cd "$ROOT/rust" && cargo fmt --check)

echo "=== codigo-muerto ==="
"$ROOT/scripts/codigo-muerto.sh"

echo "=== npm wrapper smoke ==="
node npm/install.test.js

echo "=== models (run-all-tests.sh --check-models) ==="
"$ROOT/scripts/run-all-tests.sh" --check-models

# The copy of the real corpus is not a judge step (P1 a): respaldo-memoria.timer,
# outside this repo, makes it. The judge never touches the live server.

# CI-F3.3b-1 (D-F3b): the --lane steps of scripts/gate-dag.txt, as chains, in idx order. A chain is one
# process and one cluster: dbg (CL..D3) shares one throwaway database; b3 builds the release binary; rel and
# e2e find that binary on disk again (lane_R and lane_E in run-all-tests.sh), so no variable crosses processes.
run_in_lane dbg "$ROOT/scripts/run-all-tests.sh" --lane CL --lane D1 --lane D2 --lane NLI --lane D3
run_in_lane b3 "$ROOT/scripts/run-all-tests.sh" --lane B3
run_in_lane rel "$ROOT/scripts/run-all-tests.sh" --lane R
run_in_lane e2e "$ROOT/scripts/run-all-tests.sh" --lane E

# The identity is asked again before the --features docs step, which reads the database
# after run-all-tests.sh: a server that changed under the run is refused here (D-J1c, A2).
# A lane is judged when it is made, so only the server of the operator is asked again.
[[ -n "$GATE_LANES" ]] || gate_require_judge_server "$DATABASE_URL"
# The models of the --features docs step are the ones of the full run (CI-J5): the same function sets them.
gate_model_env
echo "=== cargo clippy/test --features docs ==="
run_in_lane docs "$BASH" -c 'cd "$1" && cargo clippy --all-targets --features docs -- -D warnings && cargo test --features docs' gate "$ROOT/rust"

# The line where the timed steps end (D-F2c): test-times.sh sums the `finished in` of the
# test calls above it (run-all-tests.sh and --features docs) and nothing after. It is printed
# here, after the --features docs step, and not at the npm step, which runs before the first
# compile now. One source for the step that prints it and for the judge that stops at it.
TIMED_STEPS_END='=== end of timed steps ==='
echo "$TIMED_STEPS_END"

echo "=== crap-gate (coverage floor) ==="
run_in_lane cov "$ROOT/scripts/crap-gate.sh"

echo "=== mutants-gate ==="
run_in_lane mut "$ROOT/scripts/mutants-gate.sh"

echo "=== test times (scripts/test-time-ceilings.txt) ==="
# Last, so a time red (drift, not broken behaviour) still leaves the verdicts of
# codigo-muerto, crap-gate and mutants-gate in the same run. The log it reads is
# this gate's own, written through tee: wait (30 s at most) for the boundary line
# to reach the file, or a fast run reads it cut and the judge refuses it with 2.
# grep -c reads to the end: with grep -q the early exit hands tr a SIGPIPE, and
# under pipefail a log of a few MB would read as "not there yet" for the 30 s.
for _ in $(seq 300); do
  (( $(tr -d '\r' <"$GATE_LOG" 2>/dev/null | grep -cxF "$TIMED_STEPS_END") )) && break
  sleep 0.1
done
"$ROOT/scripts/test-times.sh" --judge "$GATE_LOG" --until "$TIMED_STEPS_END"

GATE_REACHED_END=1
echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║  MERGE GATE PASSED — safe to merge (local CI 100%)       ║"
echo "╚══════════════════════════════════════════════════════════╝"
