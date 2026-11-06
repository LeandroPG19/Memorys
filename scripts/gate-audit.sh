#!/usr/bin/env bash
# gate-audit.sh: auditoria de rutas compartidas del juez (CI-F3, F3.0).
#
# Lee trazas de strace de los pasos de test y dice que pares de pasos que el DAG puede
# solapar tocan lo mismo: rutas, puertos, clusteres de Postgres y la base viva.
# Esqueleto de la pasada 1: solo el autotest. Los modos --analyze y --run llegan en la
# pasada 2; hasta entonces todo lo que no sea --self-test sale 2.
set -euo pipefail

# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  SELF_TMP="$(mktemp -d)"
  trap 'rm -rf "$SELF_TMP"' EXIT
  cache="$SELF_TMP/cache"
  HOME="$SELF_TMP/home"; XDG_CACHE_HOME="$cache"; TMPDIR="$SELF_TMP/tmp"
  CI_TIEMPOS_DIR="$SELF_TMP/tiempos"; GATE_CLUSTER_ROOT="$SELF_TMP/clusters"
  mkdir -p "$HOME" "$cache" "$TMPDIR" "$CI_TIEMPOS_DIR" "$GATE_CLUSTER_ROOT" "$SELF_TMP/standins"
  export HOME XDG_CACHE_HOME TMPDIR CI_TIEMPOS_DIR GATE_CLUSTER_ROOT
  unset GATE_AUDIT_RUN GATE_AUDIT_MAX_GB
  # Un strace de pega: si una fila lo llama sin fingirlo, sale 97 y la fila queda roja.
  printf '#!/usr/bin/env bash\necho "stand-in strace used outside a row" >&2\nexit 97\n' > "$SELF_TMP/standins/strace"
  chmod +x "$SELF_TMP/standins/strace"
  export PATH="$SELF_TMP/standins:$PATH"
  GA="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
  RC=0

  row_want() { # <fila> <que> <obtenido> <esperado>
    [[ "$3" == "$4" ]] || { echo "FAIL self-test: $1: $2: got $3, want $4" >&2; return 1; }
  }
  row_match() { # <fila> <que> <obtenido> <patron glob>
    [[ "$3" == $4 ]] || { echo "FAIL self-test: $1: $2: got '$3', want $4" >&2; return 1; }
  }
  row_has() { # <fila> <que> <texto> <aguja>
    grep -qF -- "$4" <<<"$3" || { echo "FAIL self-test: $1: $2: no '$4' in the output" >&2; return 1; }
  }
  row_atleast() { # <fila> <que> <n> <minimo>
    (( $3 >= $4 )) || { echo "FAIL self-test: $1: $2: $3 found, want at least $4" >&2; return 1; }
  }
  rd() { mktemp -d "$SELF_TMP/row.XXXXXX"; }

  au_count() { # <fichero> <clase|*> <texto|""> -> lineas de hallazgo
    awk -F'\t' -v c="$2" -v p="$3" '(c == "*" || $1 == c) && (p == "" || index($0, p)) { n++ } END { print n + 0 }' "$1"
  }
  au_fcount() { # <fichero> <campo> <valor>
    awk -F'\t' -v k="$2" -v v="$3" '$k == v { n++ } END { print n + 0 }' "$1"
  }
  au_std() { # <d>: roots, steps, overlap y keys de la tabla estandar
    cat > "$1/roots" <<'ROOTS'
run D1 /r/run/tmp/D1
run DC /r/run/tmp/DC
target T_dbg /r/t/T_dbg
target T_rel /r/t/T_rel
cluster C_dbg /tc/dbg.Ab12
cluster C_docs /tc/docs.Cd34
ROOTS
    cat > "$1/steps" <<'STEPS'
D1 target:T_dbg,cluster:C_dbg run-all-tests\.sh --lane D1$
D2 target:T_dbg,cluster:C_dbg run-all-tests\.sh --lane D2$
DC target:T_dbg,cluster:C_docs run-all-tests\.sh --lane DC$
R target:T_rel run-all-tests\.sh --lane R$
E target:T_rel run-all-tests\.sh --lane E$
STEPS
    cat > "$1/overlap" <<'OVERLAP'
D1 DC
R E
OVERLAP
    cat > "$1/keys" <<'KEYS'
alfa-0123456789abcdef alfa
beta-0123456789abcdef beta
v029_bridge_against_the_real_daemon-0123456789abcdef v029_bridge_against_the_real_daemon
KEYS
  }
  au_run() { # <d> [args de --analyze]: RC, <d>/out y <d>/err
    local d="$1"; shift
    RC=0
    "$BASH" "$GA" --analyze "$d/tr" --roots "$d/roots" --steps "$d/steps" \
      --overlap "$d/overlap" --keys "$d/keys" "$@" >"$d/out" 2>"$d/err" || RC=$?
  }

  # Trazas de fixture, en la forma de `strace -ff -y -qq`. <trdir> es el directorio de los t.<pid>.
  tr_put() { mkdir -p "$1"; printf '%s\n' "$3" >> "$1/t.$2"; } # <trdir> <pid> <linea>
  tr_exec() { # <trdir> <pid> <binario> <argv...>
    local dir="$1" pid="$2" bin="$3" argv="" w
    shift 3
    for w in "$@"; do argv+="\"$w\", "; done
    tr_put "$dir" "$pid" "execve(\"$bin\", [${argv%, }], 0x7ffd /* 40 vars */) = 0"
  }
  tr_fork() { tr_put "$1" "$2" "clone3({flags=CLONE_VM|CLONE_FS, exit_signal=SIGCHLD}, 88) = $3"; } # <trdir> <padre> <hijo>
  tr_open() { # <trdir> <pid> <w|r> <ruta>
    local flags="O_RDONLY|O_CLOEXEC"
    [[ "$3" == w ]] && flags="O_WRONLY|O_CREAT|O_TRUNC|O_CLOEXEC"
    tr_put "$1" "$2" "openat(AT_FDCWD</r/t>, \"$4\", $flags, 0600) = 7</$4>"
  }
  tr_unix() { tr_put "$1" "$2" "connect(9<socket:[1]>, {sa_family=AF_UNIX, sun_path=\"$3\"}, 110) = $4"; } # <trdir> <pid> <ruta> <resultado>
  tr_tcp() { # <trdir> <pid> <AF_INET|AF_INET6> <puerto> <resultado>
    local campo=sin_port
    [[ "$3" == AF_INET6 ]] && campo=sin6_port
    tr_put "$1" "$2" "connect(9<socket:[2]>, {sa_family=$3, $campo=htons($4)}, 28) = $5"
  }
  tr_bind() { tr_put "$1" "$2" "bind(5<socket:[3]>, {sa_family=AF_INET, sin_port=htons($3), sin_addr=inet_addr(\"127.0.0.1\")}, 16) = 0"; } # <trdir> <pid> <puerto>
  tr_bind_unix() { tr_put "$1" "$2" "bind(5<socket:[4]>, {sa_family=AF_UNIX, sun_path=\"$3\"}, 110) = 0"; }
  tr_lane() { # <trdir> <pid-bash> <paso> <pid-test> <binario>: un paso con su unidad, que ancla una conexion a su cluster
    tr_exec "$1" "$2" /usr/bin/bash bash scripts/run-all-tests.sh --lane "$3"
    tr_fork "$1" "$2" "$4"
    tr_exec "$1" "$4" "/r/t/T_dbg/debug/deps/$5" "$5"
    case "$3" in
      D1) tr_unix "$1" "$4" /tc/dbg.Ab12/.s.PGSQL.5432 0 ;;
      DC) tr_unix "$1" "$4" /tc/docs.Cd34/.s.PGSQL.5432 0 ;;
    esac
  }

  # Arbol de fixture para --run: el guion, su tabla (<idx> <id> <deps> <recursos> <clase>), una orden falsa por paso y un centinela.
  au_fake_step() {
    cat > "$1" <<'STEP'
#!/usr/bin/env bash
printf '%s\n' "$1" >> "$FAKE_LLEGADA"
ex="$(dirname "$0")/exit-$1"
[[ -f "$ex" ]] && exit "$(cat "$ex")"
exit 0
STEP
  }
  au_fake_strace() { # <bindir>: anota su invocacion, copia las trazas de su paso y ejecuta la orden tras --
    mkdir -p "$1"
    cat > "$1/strace" <<'FAKE'
#!/usr/bin/env bash
if [[ "${1:-}" == "-V" ]]; then echo "strace -- version ${FAKE_STRACE_VERSION:-5.3}"; exit 0; fi
args=("$@")
out="" cmd=() after=0
while (( $# )); do
  if (( after )); then cmd+=("$1")
  elif [[ "$1" == "--" ]]; then after=1
  elif [[ "$1" == "-o" ]]; then out="$2"; shift
  fi
  shift
done
echo "${args[*]}" >> "$FAKE_STRACE_LOG"
fx="$FAKE_STRACE_FX/$(basename "$(dirname "$out")")"
if [[ -d "$fx" ]]; then
  for f in "$fx"/t.*; do [[ -e "$f" ]] && cp "$f" "$out.${f##*.}"; done
fi
exec "${cmd[@]}"
FAKE
    chmod +x "$1/strace"
  }
  au_tree() { # <d> "<idx> <id> <deps> <recursos> <clase>"...
    local d="$1" t="$1/tree" spec idx id deps rec cls
    shift
    mkdir -p "$t/scripts"
    cp "$GA" "$t/scripts/gate-audit.sh"
    au_fake_step "$t/scripts/fake-step.sh"
    printf 'SENTINEL\n' > "$t/scripts/gate-groups.txt"
    : > "$t/scripts/gate-dag.txt"
    for spec in "$@"; do
      read -r idx id deps rec cls <<<"$spec"
      printf '%s %s %s %s %s 1 1 1 bash %s/scripts/fake-step.sh %s\n' "$idx" "$id" "$deps" "$rec" "$cls" "$t" "$id" >> "$t/scripts/gate-dag.txt"
    done
  }
  au_fx13() { # <d>: trazas benignas de D1 y DC (cada una escribe solo en su raiz y ancla su cluster) para --run
    local d="$1" fx="$1/fx" t="$1/tree"
    tr_exec "$fx/40-D1" 100 /usr/bin/bash bash "$t/scripts/fake-step.sh" D1
    tr_fork "$fx/40-D1" 100 101
    tr_exec "$fx/40-D1" 101 /r/t/T_dbg/debug/deps/alfa-0123456789abcdef alfa-0123456789abcdef
    tr_unix "$fx/40-D1" 101 "$d/clusters/C_dbg.Ab12/.s.PGSQL.5432" 0
    tr_open "$fx/40-D1" 101 w "$d/run/tmp/D1/a"
    tr_exec "$fx/50-DC" 200 /usr/bin/bash bash "$t/scripts/fake-step.sh" DC
    tr_fork "$fx/50-DC" 200 201
    tr_exec "$fx/50-DC" 201 /r/t/T_dbg/debug/deps/beta-0123456789abcdef beta-0123456789abcdef
    tr_unix "$fx/50-DC" 201 "$d/clusters/C_docs.Cd34/.s.PGSQL.5432" 0
    tr_open "$fx/50-DC" 201 w "$d/run/tmp/DC/b"
  }
  au_go13() { # <d>: corre --run sobre el arbol de fixture; RC
    local d="$1"
    RC=0
    FAKE_STRACE_LOG="$d/strace.log" FAKE_STRACE_FX="$d/fx" FAKE_LLEGADA="$d/llegada.txt" \
      PATH="$d/bin:$PATH" GATE_AUDIT_RUN="$d/run" GATE_CLUSTER_ROOT="$d/clusters" \
      "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out" 2>"$d/err" || RC=$?
  }
  au_path_sin_strace() { # <d> -> <d>/nobin: enlaces a /usr/bin y /bin menos strace
    local f
    mkdir -p "$1/nobin"
    for f in /usr/bin/* /bin/*; do
      [[ -f "$f" && -x "$f" && "${f##*/}" != strace ]] && ln -sfn "$f" "$1/nobin/${f##*/}"
    done
    return 0
  }

  row_a0() { # el autotest es hermetico: corrido con HOME y XDG_CACHE_HOME vacios, sale 0 y los deja vacios
    [[ -z "${GATE_AUDIT_NESTED:-}" ]] || return 0
    local h x t out rc=0
    h="$(mktemp -d)"; x="$(mktemp -d)"; t="$(mktemp -d)"
    out="$(env -u CI_TIEMPOS_DIR -u GATE_CLUSTER_ROOT HOME="$h" XDG_CACHE_HOME="$x" TMPDIR="$t" \
      GATE_AUDIT_NESTED=1 "$BASH" "$GA" --self-test 2>&1)" || rc=$?
    row_want A0 exit "$rc" 0 || return 1
    row_match A0 "ultima linea" "$(tail -n 1 <<<"$out")" "OK  self-test: gate-audit" || return 1
    row_want A0 "HOME y XDG_CACHE_HOME vacios" "$(find "$h" "$x" -mindepth 1 | wc -l | tr -d ' ')" 0 || return 1
  }

  row_a0b() { # --run sin GATE_AUDIT_RUN: el informe va bajo TMPDIR y HOME y XDG_CACHE_HOME quedan vacios
    local d h x t
    d="$(rd)"; h="$(mktemp -d)"; x="$(mktemp -d)"; t="$(mktemp -d)"
    au_tree "$d" "40 D1 - target:T_dbg,cluster:C_dbg normal"
    au_fake_strace "$d/bin"
    au_fx13 "$d"
    env -u GATE_AUDIT_RUN HOME="$h" XDG_CACHE_HOME="$x" TMPDIR="$t" PATH="$d/bin:$PATH" \
      FAKE_STRACE_LOG="$d/strace.log" FAKE_STRACE_FX="$d/fx" FAKE_LLEGADA="$d/llegada.txt" \
      GATE_CLUSTER_ROOT="$d/clusters" "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out" 2>"$d/err" || true
    row_want A0b "HOME y XDG_CACHE_HOME vacios tras --run" "$(find "$h" "$x" -mindepth 1 | wc -l | tr -d ' ')" 0 || return 1
    row_atleast A0b "informe bajo TMPDIR" "$(find "$t" -path '*/memory-industry-audit.*/audit' -type d | wc -l | tr -d ' ')" 1 || return 1
  }

  row_a1() { # una unidad de D1 y otra de DC escriben la misma ruta: conflicto
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_lane "$d/tr" 200 DC 201 beta-0123456789abcdef
    tr_open "$d/tr" 101 w "$HOME/.cache/memory-industry/audit_key"
    tr_open "$d/tr" 201 w "$HOME/.cache/memory-industry/audit_key"
    au_run "$d"
    row_want A1 exit "$RC" 1 || return 1
    row_atleast A1 "path-conflict" "$(au_count "$d/out" path-conflict "$HOME/.cache/memory-industry/audit_key")" 1 || return 1
    row_match A1 "resumen" "$(tail -n 1 "$d/out")" "FAIL: audit: 1 red*" || return 1
  }

  row_a1b() { # la misma ruta en pasos que no se pueden solapar no es conflicto
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_lane "$d/tr" 300 D2 301 gamma-0123456789abcdef
    tr_open "$d/tr" 101 w "$HOME/.cache/x/shared"
    tr_open "$d/tr" 301 w "$HOME/.cache/x/shared"
    au_run "$d"
    row_want A1b exit "$RC" 0 || return 1
    row_want A1b "path-conflict" "$(au_count "$d/out" path-conflict "$HOME/.cache/x/shared")" 0 || return 1
    row_atleast A1b "home-write" "$(au_count "$d/out" home-write "$HOME/.cache/x/shared")" 1 || return 1
  }

  row_a1c() { # una escritura y una lectura de la misma ruta chocan; dos lecturas no
    local d out smoke; d="$(rd)"; au_std "$d"
    out="$HOME/repo/rust/tests/fixtures/out.json"
    smoke="$HOME/repo/rust/eval-datasets/smoke.jsonl"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_lane "$d/tr" 200 DC 201 beta-0123456789abcdef
    tr_open "$d/tr" 101 w "$out"
    tr_open "$d/tr" 201 r "$out"
    tr_open "$d/tr" 101 r "$smoke"
    tr_open "$d/tr" 201 r "$smoke"
    au_run "$d"
    row_want A1c exit "$RC" 1 || return 1
    row_atleast A1c "path-conflict out.json" "$(au_count "$d/out" path-conflict "$out")" 1 || return 1
    row_want A1c "path-conflict smoke.jsonl" "$(au_count "$d/out" path-conflict "$smoke")" 0 || return 1
  }

  row_a2() { # raices privadas: la suya, la de otro paso, el cluster y el target compartido
    local d tr; d="$(rd)"; au_std "$d"; tr="$d/tr"
    tr_lane "$tr" 100 D1 101 alfa-0123456789abcdef
    tr_fork "$tr" 100 111
    tr_exec "$tr" 111 /r/t/T_dbg/debug/deps/gamma-0123456789abcdef gamma-0123456789abcdef
    tr_lane "$tr" 300 R 301 ronda-0123456789abcdef
    tr_lane "$tr" 400 E 401 eco-0123456789abcdef
    tr_open "$tr" 101 w "/r/run/tmp/D1/a"
    tr_open "$tr" 111 w "/r/run/tmp/DC/b"
    tr_open "$tr" 101 w "/tc/dbg.Ab12/x"
    tr_open "$tr" 301 w "/r/t/T_rel/tmp/shared.lock"
    tr_open "$tr" 401 w "/r/t/T_rel/tmp/shared.lock"
    au_run "$d"
    row_want A2 exit "$RC" 1 || return 1
    row_atleast A2 "root-intrusion" "$(au_count "$d/out" root-intrusion /r/run/tmp/DC/b)" 1 || return 1
    row_atleast A2 "cluster-write" "$(au_count "$d/out" cluster-write /tc/dbg.Ab12/x)" 1 || return 1
    row_atleast A2 "path-conflict target" "$(au_count "$d/out" path-conflict /r/t/T_rel/tmp/shared.lock)" 1 || return 1
    row_want A2 "raiz propia sin hallazgo" "$(au_count "$d/out" '*' /r/run/tmp/D1/a)" 0 || return 1
  }

  row_a3() { # el puerto del corpus vivo es rojo siempre y no se puede permitir
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_tcp "$d/tr" 101 AF_INET 5488 "-1 ECONNREFUSED (Connection refused)"
    au_run "$d"
    row_want A3 exit "$RC" 1 || return 1
    row_atleast A3 live-port "$(au_count "$d/out" live-port 5488)" 1 || return 1
    au_run "$d" --pg-tcp-allowed 5488
    row_want A3 "con --pg-tcp-allowed 5488" "$RC" 2 || return 1
    row_has A3 "mensaje" "$(cat "$d/err")" 5488 || return 1
  }

  row_a4() { # TCP a Postgres en el rango 5400-5599: rojo salvo que se nombre el puerto
    local d e; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_tcp "$d/tr" 101 AF_INET 5489 0
    au_run "$d"
    row_want A4 "sin --pg-tcp-allowed" "$RC" 1 || return 1
    row_atleast A4 "pg-tcp 5489" "$(au_count "$d/out" pg-tcp 5489)" 1 || return 1
    au_run "$d" --pg-tcp-allowed 5489
    row_want A4 "con --pg-tcp-allowed 5489" "$RC" 0 || return 1
    row_atleast A4 "pg-connect 5489" "$(au_count "$d/out" pg-connect 5489)" 1 || return 1
    e="$(rd)"; au_std "$e"
    tr_lane "$e/tr" 100 D1 101 alfa-0123456789abcdef
    tr_tcp "$e/tr" 101 AF_INET6 5490 0
    au_run "$e"
    row_want A4 "AF_INET6 al 5490" "$RC" 1 || return 1
    row_atleast A4 "pg-tcp 5490" "$(au_count "$e/out" pg-tcp 5490)" 1 || return 1
  }

  row_a5() { # sockets de Postgres: el suyo, el de otro carril y uno de fuera
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_unix "$d/tr" 101 /tc/dbg.Ab12/.s.PGSQL.5432 0
    tr_unix "$d/tr" 101 /tc/docs.Cd34/.s.PGSQL.5432 0
    tr_unix "$d/tr" 101 /var/run/postgresql/.s.PGSQL.5432 0
    au_run "$d"
    row_atleast A5 "pg-connect del suyo" "$(au_count "$d/out" pg-connect /tc/dbg.Ab12)" 1 || return 1
    row_atleast A5 "pg-socket-cross" "$(au_count "$d/out" pg-socket-cross /tc/docs.Cd34)" 1 || return 1
    row_atleast A5 "pg-socket-outside" "$(au_count "$d/out" pg-socket-outside /var/run/postgresql)" 1 || return 1
  }

  row_a5b() { # sin ninguna conexion a Postgres, con clusteres declarados, no hay auditoria
    local d; d="$(rd)"; au_std "$d"
    tr_exec "$d/tr" 100 /usr/bin/bash bash scripts/run-all-tests.sh --lane D1
    tr_fork "$d/tr" 100 101
    tr_exec "$d/tr" 101 /r/t/T_dbg/debug/deps/alfa-0123456789abcdef alfa-0123456789abcdef
    tr_open "$d/tr" 101 r /r/x
    au_run "$d"
    row_want A5b exit "$RC" 2 || return 1
    row_has A5b "ancla" "$(cat "$d/err")" "the audit saw no connection" || return 1
  }

  row_a6() { # puertos fijos: conflicto entre pasos, informativo dentro de uno, el 0 no cuenta
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_bind "$d/tr" 101 8787
    tr_bind "$d/tr" 101 0
    tr_lane "$d/tr" 200 DC 201 beta-0123456789abcdef
    tr_bind "$d/tr" 201 8787
    au_run "$d"
    row_want A6 exit "$RC" 1 || return 1
    row_atleast A6 "port-conflict 8787" "$(au_count "$d/out" port-conflict 8787)" 1 || return 1
    row_want A6 "fixed-port informativos" "$(au_count "$d/out" fixed-port "")" 2 || return 1
  }

  row_a7() { # rutas relativas (chdir, cwd heredado) y descriptores con -y
    local d e; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_put "$d/tr" 101 "chdir(\"$HOME/repo/rust\") = 0"
    tr_put "$d/tr" 101 "openat(AT_FDCWD</$HOME/repo/rust>, \"tests/fixtures/out.json\", O_WRONLY|O_CREAT, 0644) = 4</$HOME/repo/rust/tests/fixtures/out.json>"
    tr_fork "$d/tr" 101 102
    tr_put "$d/tr" 102 "openat(3</$HOME/repo/rust/tests/fixtures>, \"out.json\", O_WRONLY|O_CREAT, 0644) = 5</$HOME/repo/rust/tests/fixtures/out.json>"
    au_run "$d"
    row_atleast A7 "home-write con la ruta absoluta" "$(au_count "$d/out" home-write "$HOME/repo/rust/tests/fixtures/out.json")" 1 || return 1
    e="$(rd)"; au_std "$e"
    tr_lane "$e/tr" 100 D1 101 alfa-0123456789abcdef
    tr_put "$e/tr" 101 "chdir(\"$HOME/repo/rust\") = 0"
    tr_fork "$e/tr" 101 102
    tr_put "$e/tr" 102 "openat(3, \"out.json\", O_WRONLY|O_CREAT, 0644) = 5"
    au_run "$e"
    row_want A7 "dirfd sin -y" "$RC" 2 || return 1
    row_has A7 "trace without -y" "$(cat "$e/err")" "trace without -y" || return 1
  }

  row_a7c() { # el hijo hereda la cwd del clone: su openat relativo, sin anotacion de cwd, sale de ella
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_put "$d/tr" 101 "chdir(\"$HOME/repo/rust\") = 0"
    tr_fork "$d/tr" 101 102
    tr_put "$d/tr" 102 'openat(AT_FDCWD, "tests/fixtures/out.json", O_WRONLY|O_CREAT, 0644) = 5'
    au_run "$d"
    row_want A7c exit "$RC" 0 || return 1
    row_atleast A7c "home-write del hijo en la cwd heredada" "$(au_count "$d/out" home-write "$HOME/repo/rust/tests/fixtures/out.json")" 1 || return 1
  }

  row_a8() { # de quien es cada escritura: la del daemon de un test es del test; la de rustc no tiene dueno
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_fork "$d/tr" 101 102
    tr_exec "$d/tr" 102 /r/t/T_dbg/debug/memory-industry memory-industry serve
    tr_bind_unix "$d/tr" 102 "$HOME/x.sock"
    tr_exec "$d/tr" 890 /usr/bin/cargo cargo test
    tr_fork "$d/tr" 890 900
    tr_exec "$d/tr" 900 /usr/bin/rustc rustc --crate-name x
    tr_open "$d/tr" 900 w "$HOME/repo/rust/x"
    au_run "$d"
    row_atleast A8 "el socket del daemon es de alfa" \
      "$(awk -F'\t' -v p="$HOME/x.sock" '$1 == "home-write" && index($0, p) && $3 == "alfa" { n++ } END { print n + 0 }' "$d/out")" 1 || return 1
    row_want A8 "rustc sin hallazgo" "$(au_count "$d/out" '*' "$HOME/repo/rust/x")" 0 || return 1
    row_atleast A8 "orphan-procs" "$(au_count "$d/out" orphan-procs "")" 1 || return 1
    row_want A8 "el daemon no es unidad propia" "$(au_count "$d/out" '*' memory-industry)" 0 || return 1
  }

  row_a8b() { # solo cargo y rustc: no hay unidad, no hay auditoria
    local d; d="$(rd)"; au_std "$d"
    tr_exec "$d/tr" 890 /usr/bin/cargo cargo test
    tr_fork "$d/tr" 890 900
    tr_exec "$d/tr" 900 /usr/bin/rustc rustc --crate-name x
    au_run "$d"
    row_want A8b exit "$RC" 2 || return 1
    row_has A8b "no unit" "$(cat "$d/err")" "no unit" || return 1
  }

  row_a9() { # lo que no se entiende no se ignora: sale 2 nombrando fichero y linea
    local d e; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_put "$d/tr" 101 'openat(AT_FDCWD</x>, 0x55d0'
    au_run "$d"
    row_want A9 "linea cortada" "$RC" 2 || return 1
    row_has A9 "fichero" "$(cat "$d/err")" "t.101" || return 1
    e="$(rd)"; au_std "$e"
    tr_lane "$e/tr" 100 D1 101 alfa-0123456789abcdef
    tr_put "$e/tr" 101 'openat(AT_FDCWD</x>, "/h/a", O_RDONLY <unfinished ...>'
    au_run "$e"
    row_want A9 "unfinished sin resumed" "$RC" 2 || return 1
    row_has A9 "fichero" "$(cat "$e/err")" "t.101" || return 1
  }

  row_a10() { # un openat de escritura que fallo no cuenta; el control de al lado, que si cuenta, prueba que la fila puede fallar
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_lane "$d/tr" 200 DC 201 beta-0123456789abcdef
    tr_put "$d/tr" 101 "openat(AT_FDCWD</r>, \"$HOME/.cache/x/out.json\", O_WRONLY|O_CREAT, 0644) = -1 ENOENT (No such file or directory)"
    tr_put "$d/tr" 201 "openat(AT_FDCWD</r>, \"$HOME/.cache/x/out.json\", O_WRONLY|O_CREAT, 0644) = -1 ENOENT (No such file or directory)"
    tr_open "$d/tr" 101 w "$HOME/.cache/x/ok.json"
    tr_open "$d/tr" 201 w "$HOME/.cache/x/ok.json"
    au_run "$d"
    row_atleast A10 "control: escritura exitosa compartida" "$(au_count "$d/out" path-conflict "$HOME/.cache/x/ok.json")" 1 || return 1
    row_want A10 "path-conflict de una llamada fallida" "$(au_count "$d/out" path-conflict "$HOME/.cache/x/out.json")" 0 || return 1
  }

  row_a11() { # la propuesta de grupos sale en el directorio del informe y el fichero versionado no cambia
    local d t prop; d="$(rd)"; au_tree "$d" "40 D1 - target:T_dbg,cluster:C_dbg normal" "50 DC - target:T_dbg,cluster:C_docs normal"
    t="$d/tree"; prop="$d/run/audit/groups.proposed.txt"
    au_fake_strace "$d/bin"
    au_fx13 "$d"
    tr_open "$d/fx/40-D1" 101 w "$HOME/.cache/x/shared"
    tr_open "$d/fx/50-DC" 201 w "$HOME/.cache/x/shared"
    tr_open "$d/fx/50-DC" 201 r "$t/models/emb.onnx"
    tr_bind "$d/fx/40-D1" 101 8787
    au_go13 "$d"
    row_want A11 "propuesta escrita" "$([[ -f "$prop" ]] && echo si || echo no)" si || return 1
    row_atleast A11 "grupo db" "$(awk '$2 ~ /(^|,)db(,|$)/ { n++ } END { print n + 0 }' "$prop")" 1 || return 1
    row_atleast A11 "grupo models" "$(awk '$2 ~ /(^|,)models(,|$)/ { n++ } END { print n + 0 }' "$prop")" 1 || return 1
    row_atleast A11 "grupo port:8787" "$(awk '$2 ~ /(^|,)port:8787(,|$)/ { n++ } END { print n + 0 }' "$prop")" 1 || return 1
    row_atleast A11 "cabecera path:" "$(grep -c '^# path:' "$prop" || true)" 1 || return 1
    row_want A11 "ninguna exclusive" "$(grep -c exclusive "$prop" || true)" 0 || return 1
    row_want A11 "gate-groups.txt intacto" "$(cat "$t/scripts/gate-groups.txt")" SENTINEL || return 1
  }

  row_a12() { # sin strace no se audita, y se dice; con un strace que dice 5.2 tambien sale 2
    local d rc=0; d="$(rd)"; au_tree "$d" "10 P - - normal" "40 D1 - target:T_dbg,cluster:C_dbg normal"
    au_path_sin_strace "$d"
    PATH="$d/nobin" GATE_AUDIT_RUN="$d/run" FAKE_LLEGADA="$d/llegada.txt" \
      "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out" 2>"$d/err" || rc=$?
    row_want A12 exit "$rc" 2 || return 1
    row_has A12 "motivo" "$(cat "$d/err")" "strace is not installed" || return 1
    row_has A12 "nunca" "$(cat "$d/err")" "never skipped" || return 1
    row_want A12 "ningun paso lanzado" "$([[ -e "$d/llegada.txt" ]] && echo si || echo no)" no || return 1
    au_fake_strace "$d/bin52"
    rc=0
    FAKE_STRACE_VERSION=5.2 FAKE_STRACE_LOG="$d/strace.log" FAKE_STRACE_FX="$d/fx" FAKE_LLEGADA="$d/llegada.txt" \
      PATH="$d/bin52:$PATH" GATE_AUDIT_RUN="$d/run" "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out2" 2>"$d/err2" || rc=$?
    row_want A12 "strace 5.2" "$rc" 2 || return 1
    row_has A12 "version 5.2" "$(cat "$d/err2")" "5.2" || return 1
  }

  row_a13() { # el strace falso recibe solo los pasos de tests, en orden de idx, y el exit es el de --analyze
    local d; d="$(rd)"; au_tree "$d" "10 P - - normal" "20 B1 - - normal" "40 D1 - target:T_dbg,cluster:C_dbg normal" \
      "50 DC - target:T_dbg,cluster:C_docs normal" "75 TB - - normal"
    au_fake_strace "$d/bin"
    au_fx13 "$d"
    au_go13 "$d"
    row_want A13 exit "$RC" 0 || return 1
    row_want A13 "orden de llegada" "$(paste -sd' ' "$d/llegada.txt")" "P B1 D1 DC TB" || return 1
    row_want A13 "invocaciones de strace" "$(wc -l < "$d/strace.log" | tr -d ' ')" 2 || return 1
    row_has A13 "D1 traced" "$(cat "$d/strace.log")" "audit/40-D1/t" || return 1
    row_has A13 "DC traced" "$(cat "$d/strace.log")" "audit/50-DC/t" || return 1
    row_want A13 "-ff en las dos" "$(grep -c -- ' -ff ' "$d/strace.log" || true)" 2 || return 1
    row_want A13 "-y en las dos" "$(grep -c -- ' -y ' "$d/strace.log" || true)" 2 || return 1
    row_want A13 "sin -v" "$(grep -c -- ' -v ' "$d/strace.log" || true)" 0 || return 1
  }

  row_a13b() { # un paso que sale distinto de 0 deja la auditoria incompleta: exit 2, el resto se escribe igual
    local d; d="$(rd)"; au_tree "$d" "10 P - - normal" "40 D1 - target:T_dbg,cluster:C_dbg normal" \
      "50 DC - target:T_dbg,cluster:C_docs normal"
    au_fake_strace "$d/bin"
    au_fx13 "$d"
    printf '101\n' > "$d/tree/scripts/exit-D1"
    au_go13 "$d"
    row_want A13b exit "$RC" 2 || return 1
    row_has A13b "incompleta" "$(cat "$d/out" "$d/err")" "incomplete: D1 exited 101" || return 1
    row_has A13b "DC corrio" "$(paste -sd' ' "$d/llegada.txt")" "DC" || return 1
    row_want A13b "report.tsv escrito" "$([[ -f "$d/run/audit/report.tsv" ]] && echo si || echo no)" si || return 1
  }

  row_a13c() { # trazas por encima de GATE_AUDIT_MAX_GB: exit 2 y sin truncar
    local d rc=0; d="$(rd)"; au_tree "$d" "40 D1 - target:T_dbg,cluster:C_dbg normal"
    au_fake_strace "$d/bin"
    au_fx13 "$d"
    head -c 1048576 /dev/zero | tr '\0' x > "$d/fx/40-D1/t.999"
    FAKE_STRACE_LOG="$d/strace.log" FAKE_STRACE_FX="$d/fx" FAKE_LLEGADA="$d/llegada.txt" \
      PATH="$d/bin:$PATH" GATE_AUDIT_RUN="$d/run" GATE_CLUSTER_ROOT="$d/clusters" GATE_AUDIT_MAX_GB=0 \
      "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out" 2>"$d/err" || rc=$?
    row_want A13c exit "$rc" 2 || return 1
    row_has A13c "tope" "$(cat "$d/err")" "GATE_AUDIT_MAX_GB" || return 1
    row_want A13c "trazas sin truncar" "$(wc -c < "$d/run/audit/40-D1/t.999" | tr -d ' ')" 1048576 || return 1
  }

  row_a14() { # formato exacto: cinco campos por linea, ordenadas, con la clave de --keys
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 D1 101 alfa-0123456789abcdef
    tr_lane "$d/tr" 200 DC 201 beta-0123456789abcdef
    tr_fork "$d/tr" 100 111
    tr_exec "$d/tr" 111 /r/t/T_dbg/debug/deps/v029_bridge_against_the_real_daemon-0123456789abcdef v029_bridge_against_the_real_daemon-0123456789abcdef
    tr_bind "$d/tr" 111 8787
    tr_bind "$d/tr" 201 8787
    tr_open "$d/tr" 101 w "$HOME/.cache/memory-industry/audit_key"
    tr_open "$d/tr" 201 w "$HOME/.cache/memory-industry/audit_key"
    au_run "$d"
    row_want A14 exit "$RC" 1 || return 1
    awk -F'\t' '$1 ~ /^(live-port|pg-tcp|pg-connect|pg-socket-outside|pg-socket-cross|cluster-write|root-intrusion|path-conflict|port-conflict|fixed-port|home-write|model-read|orphan-procs)$/' "$d/out" > "$d/findings"
    row_atleast A14 "hallazgos" "$(wc -l < "$d/findings" | tr -d ' ')" 3 || return 1
    row_want A14 "cinco campos" "$(awk -F'\t' '$1 ~ /^(live-port|pg-tcp|pg-connect|pg-socket-outside|pg-socket-cross|cluster-write|root-intrusion|path-conflict|port-conflict|fixed-port|home-write|model-read|orphan-procs)$/ && NF != 5 { n++ } END { print n + 0 }' "$d/out")" 0 || return 1
    local orden=0
    LC_ALL=C sort -t "$(printf '\t')" -k1,1 -k2,2 -k3,3 -k4,4 -c "$d/findings" 2>/dev/null || orden=$?
    row_want A14 "ordenadas por clase, paso, unidad y recurso" "$orden" 0 || return 1
    row_atleast A14 "clave de --keys" "$(au_fcount "$d/out" 3 v029_bridge_against_the_real_daemon)" 1 || return 1
  }

  row_a0 || exit 1
  row_a0b || exit 1
  row_a1 || exit 1
  row_a1b || exit 1
  row_a1c || exit 1
  row_a2 || exit 1
  row_a3 || exit 1
  row_a4 || exit 1
  row_a5 || exit 1
  row_a5b || exit 1
  row_a6 || exit 1
  row_a7 || exit 1
  row_a7c || exit 1
  row_a8 || exit 1
  row_a8b || exit 1
  row_a9 || exit 1
  row_a10 || exit 1
  row_a11 || exit 1
  row_a12 || exit 1
  row_a13 || exit 1
  row_a13b || exit 1
  row_a13c || exit 1
  row_a14 || exit 1
  echo "OK  self-test: gate-audit"
  exit 0
fi
# --- self-test end ---

echo "gate-audit: el modo no esta implementado todavia (pasada 1: solo el autotest)" >&2
exit 2
