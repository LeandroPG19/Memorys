#!/usr/bin/env bash

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$ROOT/scripts/gate-cluster.sh"

# The judge's own Postgres, one cluster per lane (CI-F2, D-F2b to D-F2h, corrections K1-K10). Sourced by
# merge-gate.sh and run-all-tests.sh; run on its own it checks the binaries (--check) and its self-test is
# (--self-test).
#   ./scripts/gate-cluster.sh --self-test
#
# Rules for whoever loads this file:
#   - It sets no shell options. A `set -e` here would reach every caller, and each caller sets its own.
#   - Never a bare `wait`: a lane's postgres is a child of the shell that made it, and `wait` with no PID
#     waits for it too. Wait for the PID you own, and trust a PID only with its identity: its exe is
#     GATE_PG_BINDIR/postgres and its -D is its own data (gate_identity_refusal).
#   - A lane is made and removed by the same shell. The caller's TERM and EXIT traps call
#     gate_cluster_down_all, so no lane outlives it.
#   - The URL of a lane is a socket under GATE_CLUSTER_ROOT on port 5432, never TCP. Every call the library
#     makes drops the PG* variables of its caller (GATE_PG_ENV_UNSET), so a PGPORT or a PGHOSTADDR cannot
#     move a lane off its socket.
# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"

# --- self-test: the judge's Postgres, against the stand-ins that must refuse it ---
# The stand-in postgres is a real ELF compiled here with cc, so its exe is the file it is installed as and
# it takes the arguments a postgres takes. Each fixture is a bindir of its own (its state in pgbin/version,
# pgbin/mode: ready, stuck or die) and a bin/ with locale and stat. A row runs the library through
# `bash -c "source ..."` under timeout, so a row that waits is red and does not hang. Nothing here touches
# the real default root: D6 works on a temporary of its own (GATE_CLUSTER_TMP), listed before and after.
#   ./scripts/gate-cluster.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" && "${BASH_SOURCE[0]}" == "$0" ]]; then
  hermetic="$(mktemp -d)"
  mkdir -p "$hermetic/home" "$hermetic/cache"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache"
  tmp="$(mktemp -d)"
  centinela=""
  trap 'kill $(jobs -p) 2>/dev/null || true; pkill -KILL -f -- "$tmp/" 2>/dev/null || true; rm -rf -- "$tmp" "$hermetic"' EXIT
  export GATE_CLUSTER_TMP="$tmp/tmpdefault"
  mkdir -p "$GATE_CLUSTER_TMP"
  self_fail() { echo "FAIL self-test: $*" >&2; exit 1; }
  sleep 300 &
  live=$!

  # alive PID: a process that is running (a zombie has exited).
  alive() {
    local s st
    s="$(cat "/proc/$1/stat" 2>/dev/null)" || return 1
    read -r st _ <<<"${s##*) }"
    [[ -n "$st" && "$st" != Z && "$st" != X ]]
  }
  # own PID TOKEN FILE: an owner record of this pid namespace, as the library writes it.
  own() {
    printf '%s pidns=%s\n' "$(owner_record "$1" "$2")" "$(stat -L -c %i /proc/self/ns/pid)" >"$3"
  }

  # D6 (K7): the default root, ${GATE_CLUSTER_TMP}/memory-industry-gate-<uid>, where GATE_CLUSTER_TMP is a
  # temporary of this self-test, is listed in full, names and mtimes, before the rows and after them, with a
  # live lane of this shell in it. An entry added, removed or touched is red.
  default_root="$GATE_CLUSTER_TMP/memory-industry-gate-$(id -u)"
  centinela="$default_root/dbg.centi0"
  mkdir -p "$centinela/data"
  own "$$" centinela "$centinela/owner"
  default_seen() {
    stat -c '%n %Y' "$default_root" 2>/dev/null
    ls -A --full-time "$default_root" 2>/dev/null
  }
  default_before="$(default_seen)"

  # The stand-in postgres (K1, K6): an ELF, its handler for QUIT installed before it writes <-k dir>/ready.
  standin="$tmp/standin"
  cat >"$tmp/standin.c" <<'C_EOF'
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* A stand-in for the judge's postgres. Its exe is the file it is installed as, which is what the identity
   check of gate-cluster.sh reads. Its state comes from the files next to its exe: version (what --version
   says) and mode (ready: it answers pg_isready and writes <-k dir>/ready once its QUIT handler is in place;
   stuck: it never answers; die: a FATAL line and exit 1). With -k it writes its argv into <dir>/argv. */
static void on_quit(int sig) {
    (void)sig;
    _exit(0);
}

static int read_file(const char *path, char *buf, size_t size) {
    FILE *f = fopen(path, "r");
    size_t got;
    if (f == NULL) return 0;
    got = fread(buf, 1, size - 1, f);
    fclose(f);
    buf[got] = '\0';
    while (got > 0 && (buf[got - 1] == '\n' || buf[got - 1] == '\r')) buf[--got] = '\0';
    return 1;
}

int main(int argc, char **argv) {
    char self[PATH_MAX], path[PATH_MAX + 32], version[64] = "18.6", mode[32] = "ready";
    const char *dir = NULL;
    ssize_t n = readlink("/proc/self/exe", self, sizeof self - 1);
    char *slash;
    FILE *f;
    int i;
    if (n <= 0) return 2;
    self[n] = '\0';
    slash = strrchr(self, '/');
    if (slash != NULL) *slash = '\0';
    snprintf(path, sizeof path, "%s/version", self);
    read_file(path, version, sizeof version);
    if (argc > 1 && strcmp(argv[1], "--version") == 0) {
        printf("postgres (PostgreSQL) %s (fixture)\n", version);
        return 0;
    }
    snprintf(path, sizeof path, "%s/mode", self);
    read_file(path, mode, sizeof mode);
    for (i = 1; i + 1 < argc; i++)
        if (strcmp(argv[i], "-k") == 0) dir = argv[i + 1];
    if (dir != NULL) {
        snprintf(path, sizeof path, "%s/argv", dir);
        f = fopen(path, "w");
        if (f != NULL) {
            for (i = 1; i < argc; i++) fprintf(f, "%s\n", argv[i]);
            fclose(f);
        }
    }
    if (strcmp(mode, "die") == 0) {
        fputs("FATAL: fixture postgres, the server never starts\n", stderr);
        return 1;
    }
    signal(SIGQUIT, on_quit);
    fputs("fixture postgres: started\n", stderr);
    if (dir != NULL && strcmp(mode, "ready") == 0) {
        snprintf(path, sizeof path, "%s/ready", dir);
        f = fopen(path, "w");
        if (f != NULL) fclose(f);
    }
    for (;;) pause();
}
C_EOF
  cc -O0 -Wall -o "$standin" "$tmp/standin.c" || self_fail "the stand-in postgres does not compile: is cc installed?"

  # fixture NAME VERSION VECTOR LOCALES FSTYPE: a bindir with the stand-in postgres, initdb (keeps the password
  # file it was given in $FAKE_SEEN), pg_isready (answers when <socket dir>/ready exists, and records the PG*
  # variables it was run with in its bindir), psql, and pg_config (a sharedir with vector.control); a bin/
  # with locale -a (LOCALES, one per word) and stat -f (FSTYPE).
  fixture() {
    local f="$tmp/$1"
    mkdir -p "$f/pgbin" "$f/bin" "$f/share/extension" "$f/root"
    printf "default_version = '%s'\n" "$3" >"$f/share/extension/vector.control"
    printf '%s\n' "$2" >"$f/pgbin/version"
    printf '%s\n' ready >"$f/pgbin/mode"
    cp "$standin" "$f/pgbin/postgres"
    printf '%s\n' '#!/bin/sh' \
      "case \"\${1:-}\" in --sharedir) echo \"$f/share\" ;; *) exit 1 ;; esac" >"$f/pgbin/pg_config"
    printf '%s\n' '#!/bin/sh' \
      'printf "%s %s\n" "${PGPORT-unset}" "${PGHOSTADDR-unset}" >"$(dirname "$0")/env.seen"' \
      'h=""; while [ $# -gt 0 ]; do [ "$1" = -h ] && h="$2"; shift; done' \
      'if [ -n "$h" ] && [ -e "$h/ready" ]; then exit 0; fi' 'exit 2' >"$f/pgbin/pg_isready"
    printf '%s\n' '#!/bin/sh' \
      'd=""; while [ $# -gt 0 ]; do [ "$1" = -D ] && d="$2"; [ "$1" = --pwfile ] && cp "$2" "$FAKE_SEEN"; shift; done' \
      'mkdir -p "$d"; exit 0' >"$f/pgbin/initdb"
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$f/pgbin/psql"
    { echo '#!/bin/sh'; echo 'cat <<LOCALES'; for l in $4; do echo "$l"; done; echo LOCALES; } >"$f/bin/locale"
    { echo '#!/bin/sh'; echo "case \"\$*\" in *-f*) echo $5 ;; *) exec /usr/bin/stat \"\$@\" ;; esac"; } >"$f/bin/stat"
    chmod +x "$f"/pgbin/* "$f"/bin/*
  }

  # check NAME [VAR=value ...]: --check against fixture NAME; rc and out are set.
  rc=0
  out=""
  check() {
    local n="$1"
    shift
    rc=0
    env PATH="$tmp/$n/bin:$PATH" GATE_PG_BINDIR="$tmp/$n/pgbin" GATE_CLUSTER_ROOT="$tmp/$n/root" "$@" \
      bash "$SELF" --check >"$tmp/$n.out" 2>&1 || rc=$?
    out="$(cat "$tmp/$n.out")"
  }

  # lib NAME FIXTURE CODE [VAR=value ...]: CODE in a shell that sources the library, with the fixture's bindir
  # and bin/, a root of its own, and a GATE_OWNER of its own. Under timeout 60: a row that waits is red.
  lib() {
    local n="$1" fx="$2" code="$3" owner
    shift 3
    owner="$(owner_record "$$" "$n")"
    rc=0
    env PATH="$tmp/$fx/bin:$PATH" GATE_PG_BINDIR="$tmp/$fx/pgbin" GATE_CLUSTER_ROOT="$tmp/$n/root" \
      FAKE_SEEN="$tmp/$n.seen" LIB_OWNER="$owner" "$@" \
      timeout 60 bash -c "source '$SELF'; GATE_OWNER=\"\$LIB_OWNER\"; $code" >"$tmp/$n.out" 2>&1 || rc=$?
    out="$(cat "$tmp/$n.out")"
  }

  # sweep ROOT BINDIR NAME: the sweep of ROOT with GATE_PG_BINDIR=BINDIR; rc and out are set.
  sweep() {
    local root="$1" bin="$2" n="$3"
    rc=0
    env GATE_CLUSTER_ROOT="$root" GATE_PG_BINDIR="$bin" timeout 60 bash -c "source '$SELF'; gate_cluster_sweep" \
      >"$tmp/$n.out" 2>&1 || rc=$?
    out="$(cat "$tmp/$n.out")"
  }

  # A1: the versions and the locale of D-F2b, in the one line the spec names, exit 0.
  fixture ok 18.6 0.8.7 "C.utf8 en_US.utf8" tmpfs
  check ok
  [[ "$rc" == 0 ]] || self_fail "A1: a host with postgres 18.6, pgvector 0.8.7 and en_US.utf8 is refused with $rc: $out"
  [[ "$out" == *"OK  postgres 18.6 + pgvector 0.8.7 ($tmp/ok/pgbin, locale en_US.utf8)"* ]] \
    || self_fail "A1: the OK line is not the one the spec names: $out"

  # A2: whatever is missing or does not match is exit 2 with a FAIL line that names it and docs/gate.md.
  fixture no_initdb 18.6 0.8.7 "en_US.utf8" tmpfs
  rm "$tmp/no_initdb/pgbin/initdb"
  check no_initdb
  [[ "$rc" == 2 ]] || self_fail "A2 initdb: a bindir without initdb exits $rc, not 2: $out"
  [[ "$out" == *FAIL* && "$out" == *initdb* && "$out" == *docs/gate.md* ]] \
    || self_fail "A2 initdb: no FAIL line names initdb and docs/gate.md: $out"

  fixture no_psql 18.6 0.8.7 "en_US.utf8" tmpfs
  rm "$tmp/no_psql/pgbin/psql"
  check no_psql
  [[ "$rc" == 2 && "$out" == *"psql is not there"* ]] || self_fail "A2 psql: a bindir without psql exits $rc, not 2 naming it: $out"

  fixture v187 18.7 0.8.7 "en_US.utf8" tmpfs
  check v187
  [[ "$rc" == 2 ]] || self_fail "A2 18.7: a postgres that says 18.7 exits $rc, not 2: $out"
  [[ "$out" == *18.6* && "$out" == *18.7* ]] || self_fail "A2 18.7: the FAIL line does not name 18.6 and 18.7: $out"

  fixture vec 18.6 0.8.1 "en_US.utf8" tmpfs
  check vec
  [[ "$rc" == 2 ]] || self_fail "A2 vector: a vector.control with default_version 0.8.1 exits $rc, not 2: $out"
  [[ "$out" == *0.8.7* && "$out" == *0.8.1* ]] || self_fail "A2 vector: the FAIL line does not name 0.8.7 and 0.8.1: $out"

  fixture noloc 18.6 0.8.7 "C.utf8 POSIX" tmpfs
  check noloc
  [[ "$rc" == 2 ]] || self_fail "A2 locale: a host without en_US.utf8 exits $rc, not 2: $out"
  [[ "$out" == *en_US.utf8* ]] || self_fail "A2 locale: the FAIL line does not name en_US.utf8: $out"

  fixture disk 18.6 0.8.7 "en_US.utf8" ext2/ext3
  check disk
  [[ "$rc" == 2 ]] || self_fail "A2 disk: a root on ext2/ext3 exits $rc, not 2: $out"
  [[ "$out" == *tmpfs* ]] || self_fail "A2 disk: the FAIL line does not say the root must be tmpfs: $out"

  fixture space 18.6 0.8.7 "en_US.utf8" tmpfs
  check space GATE_CLUSTER_ROOT="$tmp/space/ro ot"
  [[ "$rc" == 2 ]] || self_fail "A2 path: a root with a space exits $rc, not 2: $out"
  [[ "$out" == *"A-Za-z0-9"* ]] || self_fail "A2 path: the FAIL line does not give the allowed characters: $out"

  fixture relroot 18.6 0.8.7 "en_US.utf8" tmpfs
  check relroot GATE_CLUSTER_ROOT=relative/root
  [[ "$rc" == 2 && "$out" == *"not an absolute path"* ]] || self_fail "A2 relative: a relative root exits $rc, not 2 naming the absolute path: $out"

  fixture linkroot 18.6 0.8.7 "en_US.utf8" tmpfs
  ln -s "$tmp/linkroot/real" "$tmp/linkroot/link" 2>/dev/null || true
  mkdir -p "$tmp/linkroot/real"
  ln -s "$tmp/linkroot/real" "$tmp/linkroot/rootlink"
  check linkroot GATE_CLUSTER_ROOT="$tmp/linkroot/rootlink"
  [[ "$rc" == 2 && "$out" == *"is a symlink"* ]] || self_fail "A2 symlink: a root that is a link exits $rc, not 2 naming the symlink: $out"

  fixture nochmod 18.6 0.8.7 "en_US.utf8" tmpfs
  printf '%s\n' '#!/bin/sh' 'exit 1' >"$tmp/nochmod/bin/chmod"
  chmod +x "$tmp/nochmod/bin/chmod"
  check nochmod
  [[ "$rc" == 2 && "$out" == *chmod* ]] || self_fail "A2 chmod: a chmod that fails exits $rc, not 2 naming chmod: $out"

  # B4 and B5: a postgres that dies at its start is exit 2 in seconds, its log tail is printed, and the
  # password the initdb stand-in was given is not in the output.
  fixture crash 18.6 0.8.7 "en_US.utf8" tmpfs
  printf '%s\n' die >"$tmp/crash/pgbin/mode"
  t0="$(date +%s)"
  lib crash crash "gate_cluster_up dbg"
  t1="$(date +%s)"
  [[ "$rc" == 2 ]] || self_fail "B5: a postgres that dies at its start exits $rc, not 2: $out"
  (( t1 - t0 < 3 )) || self_fail "B5: a postgres that dies at its start took $(( t1 - t0 )) s to say so, not under 3 s"
  [[ "$out" == *"FATAL: fixture postgres"* ]] || self_fail "B5: the last lines of server.log are not in the output: $out"
  [[ -s "$tmp/crash.seen" ]] || self_fail "B4: the initdb stand-in was never given a password file, so the row proves nothing"
  [[ "$out" != *"$(cat "$tmp/crash.seen")"* ]] || self_fail "B4: the output carries the password of the cluster: $out"
  [[ -z "$(ls -A "$tmp/crash/root")" ]] || self_fail "B5: a refused cluster left its directory behind: $(ls -A "$tmp/crash/root")"

  # B4, the whole output of a cluster that comes up, the OK line included (K5): no password in it.
  fixture good 18.6 0.8.7 "en_US.utf8" tmpfs
  lib good good "gate_cluster_up dbg"
  [[ "$rc" == 0 && "$out" == *"OK  cluster dbg up in"* ]] || self_fail "B4 up: a cluster that should come up exits $rc without its OK line: $out"
  [[ -s "$tmp/good.seen" ]] || self_fail "B4 up: the initdb stand-in was never given a password file"
  [[ "$out" != *"$(cat "$tmp/good.seen")"* ]] || self_fail "B4 up: the output of a cluster that came up carries its password: $out"

  # B5 never (K9): a postgres that stays alive and never answers is exit 2 once the wall-clock timeout runs out.
  fixture stuck 18.6 0.8.7 "en_US.utf8" tmpfs
  printf '%s\n' stuck >"$tmp/stuck/pgbin/mode"
  t0="$(date +%s)"
  lib stuck stuck "gate_cluster_up dbg" GATE_CLUSTER_READY_TIMEOUT_S=1
  t1="$(date +%s)"
  [[ "$rc" == 2 ]] || self_fail "B5 never: a postgres that never answers exits $rc, not 2: $out"
  (( t1 - t0 < 10 )) || self_fail "B5 never: a 1 s timeout took $(( t1 - t0 )) s"
  [[ "$out" == *"did not answer in 1 s"* ]] || self_fail "B5 never: the FAIL line does not give the timeout: $out"
  pgrep -f "$tmp/stuck/root" >/dev/null && self_fail "B5 never: its postgres is still running after the refusal"

  # B5b (K9): a pg_isready that takes 3 s a try, with a 1 s wall-clock timeout: the cluster fails in under 5 s.
  fixture slowi 18.6 0.8.7 "en_US.utf8" tmpfs
  printf '%s\n' '#!/bin/sh' 'sleep 3' 'exit 2' >"$tmp/slowi/pgbin/pg_isready"
  chmod +x "$tmp/slowi/pgbin/pg_isready"
  t0="$(date +%s)"
  lib slowi slowi "gate_cluster_up dbg" GATE_CLUSTER_READY_TIMEOUT_S=1
  t1="$(date +%s)"
  [[ "$rc" == 2 ]] || self_fail "B5b: a pg_isready that never says yes exits $rc, not 2: $out"
  (( t1 - t0 < 5 )) || self_fail "B5b: a 1 s timeout over a 3 s pg_isready took $(( t1 - t0 )) s"

  # K8: a PGPORT and a PGHOSTADDR inherited from the caller do not reach the library's calls, and the lane is
  # started on port 5432 and its URL says so.
  fixture k8 18.6 0.8.7 "en_US.utf8" tmpfs
  lib k8 k8 'gate_cluster_up dbg >/dev/null || exit 3; cat "${GATE_CLUSTER_DIR[dbg]}/argv" >"$K8ARGV"; printf "%s" "$GATE_CLUSTER_URL" >"$K8URL"; gate_cluster_down dbg >/dev/null' \
    PGPORT=5488 PGHOSTADDR=127.0.0.1 K8ARGV="$tmp/k8.argv" K8URL="$tmp/k8.url"
  [[ "$rc" == 0 ]] || self_fail "K8: a lane with an inherited PGPORT exits $rc: $out"
  [[ "$(cat "$tmp/k8/pgbin/env.seen")" == "unset unset" ]] \
    || self_fail "K8: pg_isready ran with PGPORT and PGHOSTADDR from the caller: '$(cat "$tmp/k8/pgbin/env.seen")'"
  grep -qx -- "port=5432" "$tmp/k8.argv" || self_fail "K8: the postgres of the lane was not started with port=5432: $(cat "$tmp/k8.argv")"
  [[ "$(cat "$tmp/k8.url")" == *":5432/brain" ]] || self_fail "K8: the URL of the lane does not carry port 5432: $(cat "$tmp/k8.url")"

  # D1 (K6, K7): gate_cluster_down ends the lane by QUIT in under 3 s, whatever the caller's exit code: its
  # postgres is gone, its directory is gone, and its log is kept in its own directory of logs.
  fixture stay 18.6 0.8.7 "en_US.utf8" tmpfs
  for code in 0 1; do
    t0="$(date +%s%3N)"
    lib stay$code stay "gate_cluster_up dbg >/dev/null || exit 3; gate_cluster_down dbg >/dev/null; exit $code" \
      GATE_CLUSTER_LOGS="$tmp/logs$code"
    t1="$(date +%s%3N)"
    [[ "$rc" == "$code" ]] || self_fail "D1: a caller that exits $code exits $rc: $out"
    (( t1 - t0 < 3000 )) || self_fail "D1: a lane up and down took $(( t1 - t0 )) ms (caller exit $code): the QUIT did not end the postgres"
    [[ -z "$(ls -A "$tmp/stay$code/root")" ]] || self_fail "D1: the lane directory is still there after its down (caller exit $code)"
    pgrep -f "$tmp/stay$code/root" >/dev/null && self_fail "D1: its postgres is still running after its down (caller exit $code)"
    grep -qx "fixture postgres: started" "$tmp/logs$code/dbg.log" 2>/dev/null \
      || self_fail "D1: the log of the lane is not in its own directory of logs (caller exit $code)"
  done

  # D2 (library side, K6): a caller whose TERM trap runs gate_cluster_down_all stops with no postgres and no
  # directory left when TERM reaches it in the middle of its step, and it exits 2, in under 5 s.
  cat >"$tmp/d2.sh" <<'D2EOF'
source "$SELF"
GATE_OWNER="$(cat "$OWNER_FILE")"
gate_cluster_up dbg >/dev/null || exit 3
sp=""
trap 'kill "$sp" 2>/dev/null; gate_cluster_down_all; exit 2' TERM
touch "$READY_FILE"
sleep 30 &
sp=$!
wait "$sp"
D2EOF
  owner_record "$$" d2 >"$tmp/d2.owner"
  mkdir -p "$tmp/d2/root"
  env PATH="$tmp/stay/bin:$PATH" GATE_PG_BINDIR="$tmp/stay/pgbin" GATE_CLUSTER_ROOT="$tmp/d2/root" \
    SELF="$SELF" OWNER_FILE="$tmp/d2.owner" READY_FILE="$tmp/d2.ready" GATE_CLUSTER_LOGS="$tmp/logs-d2" \
    bash "$tmp/d2.sh" >"$tmp/d2.out" 2>&1 &
  caller=$!
  for _ in $(seq 100); do [[ -e "$tmp/d2.ready" ]] && break; sleep 0.1; done
  t0="$(date +%s%3N)"
  kill -TERM "$caller" 2>/dev/null || true
  rc=0
  wait "$caller" || rc=$?
  t1="$(date +%s%3N)"
  [[ "$rc" == 2 ]] || self_fail "D2: a caller stopped by TERM exits $rc, not 2: $(cat "$tmp/d2.out")"
  (( t1 - t0 < 5000 )) || self_fail "D2: the TERM trap took $(( t1 - t0 )) ms to take the lane down"
  [[ -z "$(ls -A "$tmp/d2/root")" ]] || self_fail "D2: a lane directory is left after TERM: $(cat "$tmp/d2.out")"
  pgrep -f "$tmp/d2/root" >/dev/null && self_fail "D2: a lane postgres is left after TERM"

  # K1: a pid registered for a lane that is not its postgres is never signalled: a live sleep stays alive, the
  # caller gets a note, and the directory stays.
  sleep 60 &
  k1pid=$!
  mkdir -p "$tmp/k1/root/dbg.k1aaaa/data"
  lib k1 stay "GATE_CLUSTER_DIR[dbg]=\"$tmp/k1/root/dbg.k1aaaa\"; GATE_CLUSTER_PID[dbg]=$k1pid; gate_cluster_down dbg; echo \"after rc \$?\""
  alive "$k1pid" || self_fail "K1: a down signalled a pid that is not its postgres: the sleep is gone: $out"
  [[ "$out" == *"is not the postgres of"* && "$out" == *"after rc 1"* ]] \
    || self_fail "K1: the down of a foreign pid does not say so and return 1: $out"
  [[ -d "$tmp/k1/root/dbg.k1aaaa" ]] || self_fail "K1: the directory of a foreign pid was removed"
  kill "$k1pid" 2>/dev/null || true

  # K2: an empty pid is not alive, and the down of a lane with none does not wait its 10 s.
  lib k2a stay "GATE_CLUSTER_DIR[dbg]=\"$tmp/k2/root/dbg.k2aaaa\"; GATE_CLUSTER_PID[dbg]=; t0=\$(date +%s%3N); gate_cluster_down dbg >/dev/null; t1=\$(date +%s%3N); echo \"ms \$((t1-t0))\""
  [[ "$(sed -n 's/^ms //p' <<<"$out")" -lt 1000 ]] || self_fail "K2: the down of a lane with an empty pid took $out, not under 1 s"
  lib k2b stay "gate_pid_alive '' && echo ALIVE || echo DEAD"
  [[ "$out" == DEAD ]] || self_fail "K2: an empty pid is alive for gate_pid_alive: $out"

  # K2 (window): a TERM that reaches the caller before its pid is recorded still ends the postgres, found by
  # its postmaster.pid with its identity.
  d="$tmp/k2c/root/dbg.k2cccc"
  mkdir -p "$d/data"
  "$tmp/stay/pgbin/postgres" -D "$d/data" -k "$d" </dev/null >/dev/null 2>&1 &
  k2c=$!
  printf '%s\n%s\n' "$k2c" "$d/data" >"$d/data/postmaster.pid"
  lib k2c stay "GATE_CLUSTER_DIR[dbg]=\"$d\"; gate_cluster_down_all; echo done"
  [[ "$out" == *done* ]] || self_fail "K2 window: the down_all did not finish: $out"
  for _ in $(seq 30); do alive "$k2c" || break; sleep 0.1; done
  alive "$k2c" && { kill -KILL "$k2c" 2>/dev/null; self_fail "K2 window: the postgres named by postmaster.pid survived the down_all"; }

  # K3: an owner of another pid namespace is not judged: its postgres is not signalled, its directory stays, and
  # the sweep says so and exits 0.
  mkdir -p "$tmp/k3/root/dbg.dead03/data"
  printf '%s pidns=%s\n' "$(owner_record 999999 deadtok)" 0 >"$tmp/k3/root/dbg.dead03/owner"
  "$tmp/stay/pgbin/postgres" -D "$tmp/k3/root/dbg.dead03/data" -k "$tmp/k3/root/dbg.dead03" </dev/null >/dev/null 2>&1 &
  k3pid=$!
  printf '%s\n%s\n' "$k3pid" "$tmp/k3/root/dbg.dead03/data" >"$tmp/k3/root/dbg.dead03/data/postmaster.pid"
  sweep "$tmp/k3/root" "$tmp/stay/pgbin" k3
  alive "$k3pid" || self_fail "K3: the sweep signalled the postgres of an owner of another pid namespace"
  [[ "$rc" == 0 && "$out" == *"another pid namespace"* ]] || self_fail "K3: the sweep does not leave a foreign owner with a note and exit 0: rc $rc: $out"
  [[ -d "$tmp/k3/root/dbg.dead03" ]] || self_fail "K3: the directory of an owner of another pid namespace was removed"
  kill -KILL "$k3pid" 2>/dev/null || true

  # K4: the identity of the sweep, one check per row, each with its own refusal. D4a: a pid file that names a
  # live sleep. D4b: a postgres of this bindir with another -D. D4c: a postgres outside this bindir.
  mkdir -p "$tmp/d4a/root/docs.xxxxxx/data"
  own 999999 deadtok "$tmp/d4a/root/docs.xxxxxx/owner"
  sleep 60 &
  d4apid=$!
  printf '%s\n%s\n' "$d4apid" "$tmp/d4a/root/docs.xxxxxx/data" >"$tmp/d4a/root/docs.xxxxxx/data/postmaster.pid"
  sweep "$tmp/d4a/root" "$tmp/stay/pgbin" d4a
  [[ "$rc" == 2 && "$out" == *"exe is not $tmp/stay/pgbin/postgres"* && -d "$tmp/d4a/root/docs.xxxxxx" ]] \
    || self_fail "D4 sleep: a pid file that names a sleep exits $rc, not 2 with 'exe is not': $out"
  alive "$d4apid" || self_fail "D4 sleep: the sleep the pid file named was killed"
  kill "$d4apid" 2>/dev/null || true

  mkdir -p "$tmp/d4b/root/dbg.dbbbbb/data" "$tmp/d4b/other/data"
  own 999999 deadtok "$tmp/d4b/root/dbg.dbbbbb/owner"
  "$tmp/stay/pgbin/postgres" -D "$tmp/d4b/other/data" </dev/null >/dev/null 2>&1 &
  d4bpid=$!
  printf '%s\n%s\n' "$d4bpid" "$tmp/d4b/root/dbg.dbbbbb/data" >"$tmp/d4b/root/dbg.dbbbbb/data/postmaster.pid"
  sweep "$tmp/d4b/root" "$tmp/stay/pgbin" d4b
  [[ "$rc" == 2 && "$out" == *"-D is not $tmp/d4b/root/dbg.dbbbbb/data"* ]] \
    || self_fail "D4 -D: a postgres of this bindir with another -D exits $rc, not 2 with '-D is not': $out"
  alive "$d4bpid" || self_fail "D4 -D: the postgres with another -D was killed"
  kill -KILL "$d4bpid" 2>/dev/null || true

  mkdir -p "$tmp/d4c/root/dbg.dccccc/data" "$tmp/outside"
  own 999999 deadtok "$tmp/d4c/root/dbg.dccccc/owner"
  cp "$standin" "$tmp/outside/postgres"
  "$tmp/outside/postgres" -D "$tmp/d4c/root/dbg.dccccc/data" </dev/null >/dev/null 2>&1 &
  d4cpid=$!
  printf '%s\n%s\n' "$d4cpid" "$tmp/d4c/root/dbg.dccccc/data" >"$tmp/d4c/root/dbg.dccccc/data/postmaster.pid"
  sweep "$tmp/d4c/root" "$tmp/stay/pgbin" d4c
  [[ "$rc" == 2 && "$out" == *"exe is not $tmp/stay/pgbin/postgres"* ]] \
    || self_fail "D4 exe: a postgres outside this bindir exits $rc, not 2 with 'exe is not': $out"
  alive "$d4cpid" || self_fail "D4 exe: the postgres outside this bindir was killed"
  kill -KILL "$d4cpid" 2>/dev/null || true

  # D4 owner: a directory with no owner file is exit 2 with its own path, and nothing in it is removed.
  mkdir -p "$tmp/d4d/root/mut.nowner/data"
  sweep "$tmp/d4d/root" "$tmp/stay/pgbin" d4d
  [[ "$rc" == 2 && "$out" == *"has no owner file"* ]] || self_fail "D4 owner: a directory with no owner file exits $rc, not 2 with 'has no owner file': $out"
  [[ -d "$tmp/d4d/root/mut.nowner" ]] || self_fail "D4 owner: the directory without an owner was removed"

  # K10 (sweep): a name that is not <lane>.XXXXXX is a note and is left as it is; the sweep goes on past it and
  # past a refusal, so a dead lane after both is still removed, and the refusal is exit 2.
  mkdir -p "$tmp/k10/root/stray" "$tmp/k10/root/a-refused.aaaaaa/data" "$tmp/k10/root/dbg.dead01/data"
  own 999999 deadtok "$tmp/k10/root/a-refused.aaaaaa/owner"
  sleep 60 &
  k10pid=$!
  printf '%s\n%s\n' "$k10pid" "$tmp/k10/root/a-refused.aaaaaa/data" >"$tmp/k10/root/a-refused.aaaaaa/data/postmaster.pid"
  own 999999 deadtok "$tmp/k10/root/dbg.dead01/owner"
  "$tmp/stay/pgbin/postgres" -D "$tmp/k10/root/dbg.dead01/data" </dev/null >/dev/null 2>&1 &
  k10dead=$!
  printf '%s\n%s\n' "$k10dead" "$tmp/k10/root/dbg.dead01/data" >"$tmp/k10/root/dbg.dead01/data/postmaster.pid"
  sweep "$tmp/k10/root" "$tmp/stay/pgbin" k10
  for _ in $(seq 30); do alive "$k10dead" || break; sleep 0.1; done
  [[ "$rc" == 2 ]] || self_fail "K10 sweep: a refusal in the sweep exits $rc, not 2: $out"
  [[ "$out" == *"is not a lane directory"* ]] || self_fail "K10 sweep: the name that is not a lane gives no note: $out"
  [[ -d "$tmp/k10/root/stray" ]] || self_fail "K10 sweep: a name that is not a lane was removed"
  [[ ! -e "$tmp/k10/root/dbg.dead01" ]] || self_fail "K10 sweep: the sweep stopped at the refusal and left the dead lane after it: $out"
  alive "$k10pid" || self_fail "K10 sweep: the sleep named by a refused directory was killed"
  kill "$k10pid" 2>/dev/null || true

  # D3 (K6): a dead owner's postgres gets QUIT, and its directory goes in under 3 s; a live owner's stays.
  d3="$tmp/d3"
  mkdir -p "$d3/dbg.dead02/data" "$d3/cov.live02/data"
  own 999999 deadtok "$d3/dbg.dead02/owner"
  own "$$" livetok "$d3/cov.live02/owner"
  "$tmp/stay/pgbin/postgres" -D "$d3/dbg.dead02/data" </dev/null >/dev/null 2>&1 &
  dpid=$!
  printf '%s\n%s\n' "$dpid" "$d3/dbg.dead02/data" >"$d3/dbg.dead02/data/postmaster.pid"
  "$tmp/stay/pgbin/postgres" -D "$d3/cov.live02/data" </dev/null >/dev/null 2>&1 &
  lpid=$!
  printf '%s\n%s\n' "$lpid" "$d3/cov.live02/data" >"$d3/cov.live02/data/postmaster.pid"
  t0="$(date +%s%3N)"
  sweep "$d3" "$tmp/stay/pgbin" d3
  t1="$(date +%s%3N)"
  for _ in $(seq 30); do alive "$dpid" || break; sleep 0.1; done
  alive "$dpid" && { kill -KILL "$dpid" 2>/dev/null; self_fail "D3: the postgres of the dead owner was still running 3 s after the sweep"; }
  wait "$dpid" 2>/dev/null || true
  [[ "$rc" == 0 ]] || self_fail "D3: a sweep with a dead owner and a live one exits $rc, not 0: $out"
  (( t1 - t0 < 3000 )) || self_fail "D3: the sweep took $(( t1 - t0 )) ms: the QUIT did not end the postgres of the dead owner"
  [[ ! -e "$d3/dbg.dead02" ]] || self_fail "D3: the directory of the dead owner is still there"
  [[ -d "$d3/cov.live02" ]] || self_fail "D3: the directory of the live owner was removed"
  alive "$lpid" || self_fail "D3: the postgres of the live owner was killed"
  [[ "$out" == *"note: removed the cluster of a gate that is no longer running (dbg"* ]] \
    || self_fail "D3: no note names the dead lane: $out"
  kill -KILL "$lpid" 2>/dev/null || true

  # D-F2b / D-F2g: the URL of a lane is a socket under its directory, the directory encoded as the host (%2F),
  # on port 5432, and the database is brain. It carries a password, so it is assigned to the variable the caller
  # names and never printed: a log keeps what is printed. The row reads the variable back from a file it writes,
  # and stdout must stay empty. A name that is not an identifier is exit 2.
  rc=0
  bash -c "source '$SELF'; gate_cluster_url '$tmp/root/dbg.abc' abc123 url_seen; printf '%s' \"\$url_seen\" >'$tmp/url.seen'" \
    >"$tmp/url.stdout" 2>&1 || rc=$?
  url="$(cat "$tmp/url.seen" 2>/dev/null)"
  want="postgresql://cuba:abc123@${tmp//\//%2F}%2Froot%2Fdbg.abc:5432/brain"
  [[ "$rc" == 0 ]] || self_fail "URL: gate_cluster_url refused its arguments (rc $rc): $(cat "$tmp/url.stdout")"
  [[ ! -s "$tmp/url.stdout" ]] || self_fail "URL: gate_cluster_url printed something, and a URL with a password must not be printed: $(cat "$tmp/url.stdout")"
  [[ "$url" == "$want" ]] || self_fail "URL: the variable holds '$url', not '$want'"
  rc=0
  bash -c "source '$SELF'; gate_cluster_url '$tmp/root/dbg.abc' abc123 'bad-name'" >"$tmp/url2.out" 2>&1 || rc=$?
  [[ "$rc" == 2 && "$(cat "$tmp/url2.out")" == *"is not a variable name"* ]] \
    || self_fail "URL name: a name that is not an identifier exits $rc, not 2 naming it: $(cat "$tmp/url2.out")"

  # K11: the default root is ${GATE_CLUSTER_TMP:-/tmp}/memory-industry-gate-<uid>. It is computed, and the
  # computing creates nothing.
  calc="$tmp/calc"
  calc_root="$(env -u GATE_CLUSTER_ROOT GATE_CLUSTER_TMP="$calc" bash -c "source '$SELF'; printf %s \"\$GATE_CLUSTER_ROOT\"")"
  [[ "$calc_root" == "$calc/memory-industry-gate-$(id -u)" ]] \
    || self_fail "K11 root: with GATE_CLUSTER_TMP the default root is '$calc_root', not under it"
  [[ ! -e "$calc" ]] || self_fail "K11 root: computing the default root created $calc"
  plain_root="$(env -u GATE_CLUSTER_ROOT -u GATE_CLUSTER_TMP bash -c "source '$SELF'; printf %s \"\$GATE_CLUSTER_ROOT\"")"
  [[ "$plain_root" == "/tmp/memory-industry-gate-$(id -u)" ]] \
    || self_fail "K11 root: without GATE_CLUSTER_TMP the default root is '$plain_root', not /tmp/memory-industry-gate-<uid>"

  # K7 (logs): without GATE_CLUSTER_LOGS a lane's log goes under XDG_CACHE_HOME, never under HOME/.cache.
  mkdir -p "$tmp/xdg-row" "$tmp/home-row"
  lib xdgrow stay "gate_cluster_up dbg >/dev/null || exit 3; gate_cluster_down dbg >/dev/null" \
    XDG_CACHE_HOME="$tmp/xdg-row" HOME="$tmp/home-row"
  [[ "$rc" == 0 ]] || self_fail "K7 logs: a lane that comes up and goes down exits $rc: $out"
  [[ -s "$tmp/xdg-row/memory-industry-gate/clusters/dbg.log" ]] \
    || self_fail "K7 logs: the log of a lane is not under XDG_CACHE_HOME/memory-industry-gate/clusters"
  [[ ! -e "$tmp/home-row/.cache" ]] || self_fail "K7 logs: a lane's log went under HOME/.cache"

  # K10: each rule of the library has its row. A lane name that is not [a-z0-9][a-z0-9-]*, a GATE_OWNER that is
  # empty, and a mktemp that fails are exit 2 before a lane directory exists.
  lib badlane stay "gate_cluster_up Dbg_1"
  [[ "$rc" == 2 && "$out" == *"lane name 'Dbg_1'"* ]] || self_fail "K10 lane: a lane name with a capital and an underscore exits $rc, not 2 naming it: $out"
  lib noowner stay "gate_cluster_up dbg" LIB_OWNER=
  [[ "$rc" == 2 && "$out" == *"GATE_OWNER is empty"* ]] || self_fail "K10 owner: an empty GATE_OWNER exits $rc, not 2 naming it: $out"
  fixture nomk 18.6 0.8.7 "en_US.utf8" tmpfs
  printf '%s\n' '#!/bin/sh' 'exit 1' >"$tmp/nomk/bin/mktemp"
  chmod +x "$tmp/nomk/bin/mktemp"
  lib nomk nomk "gate_cluster_up dbg"
  [[ "$rc" == 2 && "$out" == *"FAIL: mktemp could not"* ]] || self_fail "K10 mktemp: a mktemp that fails exits $rc, not 2 with its FAIL line: $out"

  # K7 (D6): the default root, listed in full before and after the rows, with its live lane, is the same.
  default_after="$(default_seen)"
  [[ "$default_after" == "$default_before" ]] \
    || self_fail "D6: the default cluster root $default_root changed while the rows ran: $(diff <(printf '%s\n' "$default_before") <(printf '%s\n' "$default_after") | tr '\n' ' ')"

  echo "OK  self-test: gate-cluster refuses a missing initdb, psql, a postgres 18.7, a pgvector 0.8.1, a locale without en_US.utf8, a root on disk, a root with a space, a relative root, a root that is a link, a chmod that fails, a socket path over 107 bytes; a postgres that dies at its start or never answers is exit 2 at the wall-clock timeout, without the password, and a postgres that takes 3 s a try does not stretch it; the whole output of a lane that comes up has no password; a lane does not inherit PGPORT or PGHOSTADDR, and it is on port 5432; down ends a lane by QUIT in under 3 s and keeps its log per run; TERM takes every lane down; a pid that is not the lane's postgres is never signalled; an empty pid is not alive; the sweep does not signal an owner of another pid namespace, judges each directory by its identity (exe and -D) with its own refusal, and goes on past a note and a refusal; a lane name, an empty GATE_OWNER and a failing mktemp are exit 2; the default cluster root, computed from GATE_CLUSTER_TMP and never created, is listed the same before and after the rows; a lane's log goes under XDG_CACHE_HOME; a lane's URL is a socket under its directory on port 5432"
  exit 0
fi
# --- self-test end ---

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  case "${1:-}" in
    --check) gate_cluster_check ;;
    *) echo "usage: $0 --check | --self-test" >&2; exit 2 ;;
  esac
fi
