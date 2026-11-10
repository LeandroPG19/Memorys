#!/usr/bin/env bash
# gate-audit.sh: auditoria de rutas compartidas del juez (CI-F3, F3.0).
#
# Lee trazas de strace de los pasos de test y dice que pares de pasos que el DAG puede
# solapar tocan lo mismo: rutas, puertos, clusteres de Postgres y la base viva.
# Modos: --self-test (autotest hermetico), --analyze (solo lee trazas) y --run (lanza
# los pasos de test bajo strace, en serie, y analiza lo que sale). Sin strace, --run sale 2.
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
  unset GATE_AUDIT_RUN GATE_AUDIT_MAX_GB GATE_DAG CARGO_TARGET_DIR GATE_CLUSTER_TMP
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
    # La forma de scripts/gate-dag.txt: ids, recursos y orden reales (el contrato J13 lo comprueba).
    cat > "$1/roots" <<'ROOTS'
run DBG /r/run/tmp/DBG
run DC /r/run/tmp/DC
target T_dbg /r/t/T_dbg
target T_rel /r/t/T_rel
target T_docs /r/t/T_docs
cluster C_dbg /tc/dbg.Ab12
cluster C_nli /tc/nli.Ef56
cluster C_rel /tc/rel.Gh78
cluster C_e2e /tc/e2e.Ij90
cluster C_docs /tc/docs.Cd34
ROOTS
    cat > "$1/steps" <<'STEPS'
DBG target:T_dbg,cluster:C_dbg,models merge-gate\.sh --step DBG$
NLI target:T_dbg,cluster:C_nli,models,reader merge-gate\.sh --step NLI$
DC target:T_docs,cluster:C_docs merge-gate\.sh --step DC$
REL target:T_rel,cluster:C_rel,models,reader merge-gate\.sh --step REL$
E2E target:T_rel,cluster:C_e2e,models,reader merge-gate\.sh --step E2E$
STEPS
    cat > "$1/overlap" <<'OVERLAP'
DBG DC
REL E2E
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
  tr_mkdir() { tr_put "$1" "$2" "mkdir(\"$3\", 0700) = 0"; } # <trdir> <pid> <ruta>: el planificador hace su directorio de corrida
  tr_lane() { # <trdir> <pid-bash> <paso> <pid-test> <binario>: un paso con su unidad, que ancla una conexion a su cluster
    tr_exec "$1" "$2" /usr/bin/bash bash scripts/merge-gate.sh --step "$3"
    tr_fork "$1" "$2" "$4"
    tr_exec "$1" "$4" "/r/t/T_dbg/debug/deps/$5" "$5"
    case "$3" in
      DBG) tr_unix "$1" "$4" /tc/dbg.Ab12/.s.PGSQL.5432 0 ;;
      DC) tr_unix "$1" "$4" /tc/docs.Cd34/.s.PGSQL.5432 0 ;;
    esac
  }

  # Arbol de fixture para --run: lo que el audit necesita de un arbol de verdad (el guion, sus bibliotecas, el planificador y
  # la tabla con la forma de la real: NAME=value delante y la orden `bash scripts/merge-gate.sh --step <id>`), un merge-gate
  # falso que solo anota como lo lanzaron, un strace falso que entrega las trazas del fixture y un centinela. El audit no
  # lanza filas: lanza merge-gate.sh una vez y las filas las lanza el planificador.
  au_fake_merge_gate() {
    cat > "$1" <<'GATE'
#!/usr/bin/env bash
{
  printf 'GATE_DAG=%s\n' "${GATE_DAG-unset}"
  printf 'args=%s\n' "$*"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-unset}"
} >> "$FAKE_LLEGADA"
ex="$(dirname "$0")/exit-gate"
[[ -f "$ex" ]] && exit "$(cat "$ex")"
exit 0
GATE
  }
  au_fake_strace() { # <bindir>: anota su invocacion, copia las trazas de su directorio de salida y ejecuta la orden tras --
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
    local d="$1" t="$1/tree" spec idx id deps rec cls env f
    shift
    mkdir -p "$t/scripts"
    cp "$GA" "$t/scripts/gate-audit.sh"
    for f in gate-lock.sh gate-cluster.sh gate-dag.py gate_history.py; do cp "$(dirname "$GA")/$f" "$t/scripts/$f"; done
    au_fake_merge_gate "$t/scripts/merge-gate.sh"
    printf 'SENTINEL\n' > "$t/scripts/gate-groups.txt"
    : > "$t/scripts/gate-dag.txt"
    for spec in "$@"; do
      read -r idx id deps rec cls <<<"$spec"
      env=""
      [[ "$rec" != *target:* ]] || env="CARGO_BUILD_JOBS=8 "
      printf '%s %s %s %s %s 1 1 1 %sbash scripts/merge-gate.sh --step %s\n' "$idx" "$id" "$deps" "$rec" "$cls" "$env" "$id" >> "$t/scripts/gate-dag.txt"
    done
  }
  au_key() { # <arbol> -> <nombre>-<12 hex>: la clave de arbol con la formula del planificador, escrita aparte: sha256 del realpath SIN salto de linea
    local real
    real="$(realpath "$1")"
    printf '%s-%s' "$(basename "$real")" "$(printf '%s' "$real" | sha256sum | cut -c1-12)"
  }
  au_fx() { # <d>: trazas benignas de una corrida de merge-gate: el planificador hace su directorio y lanza DBG y DC, cada una con su cluster, su target y su raiz
    local d="$1" fx="$1/fx/gate" t="$1/tree" tt dag
    tt="$d/targets/$(au_key "$t")"
    dag="$d/tmp/memory-industry-dag.Ab12Cd"
    tr_exec "$fx" 100 /usr/bin/bash bash "$t/scripts/merge-gate.sh"
    tr_fork "$fx" 100 120
    tr_exec "$fx" 120 /usr/bin/python3 python3 -I "$t/scripts/gate-dag.py" run --table "$t/scripts/gate-dag.txt"
    tr_mkdir "$fx" 120 "$dag"
    tr_fork "$fx" 120 121
    tr_exec "$fx" 121 /usr/bin/bash bash scripts/merge-gate.sh --step DBG
    tr_fork "$fx" 121 122
    tr_exec "$fx" 122 "$tt/T_dbg/debug/deps/alfa-0123456789abcdef" alfa-0123456789abcdef
    tr_unix "$fx" 122 "$d/clusters/dbg.Ab12Cd/.s.PGSQL.5432" 0
    tr_open "$fx" 122 w "$dag/tmp/DBG/a"
    tr_open "$fx" 122 w "$tt/T_dbg/debug/out"
    tr_fork "$fx" 120 131
    tr_exec "$fx" 131 /usr/bin/bash bash scripts/merge-gate.sh --step DC
    tr_fork "$fx" 131 132
    tr_exec "$fx" 132 "$tt/T_docs/debug/deps/beta-0123456789abcdef" beta-0123456789abcdef
    tr_unix "$fx" 132 "$d/clusters/docs.Cd34Ef/.s.PGSQL.5432" 0
    tr_open "$fx" 132 w "$dag/tmp/DC/b"
    tr_open "$fx" 132 w "$tt/T_docs/debug/out"
  }
  au_go() { # <d> [VAR=value...]: corre --run sobre el arbol de fixture, con la raiz de target y la de clusteres del fixture; RC
    local d="$1"
    shift
    RC=0
    env FAKE_STRACE_LOG="$d/strace.log" FAKE_STRACE_FX="$d/fx" FAKE_LLEGADA="$d/llegada.txt" \
      PATH="$d/bin:$PATH" GATE_AUDIT_RUN="$d/run" GATE_CLUSTER_ROOT="$d/clusters" CARGO_TARGET_DIR="$d/targets" "$@" \
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

  row_a0b() { # --run sin GATE_AUDIT_RUN: el informe va bajo TMPDIR y HOME y XDG_CACHE_HOME quedan vacios (ni siquiera la raiz de target por omision se crea)
    local d h x t
    d="$(rd)"; h="$(mktemp -d)"; x="$(mktemp -d)"; t="$(mktemp -d)"
    au_tree "$d" "20 DBG P target:T_dbg,cluster:C_dbg,models normal"
    au_fake_strace "$d/bin"
    au_fx "$d"
    env -u GATE_AUDIT_RUN -u CARGO_TARGET_DIR HOME="$h" XDG_CACHE_HOME="$x" TMPDIR="$t" PATH="$d/bin:$PATH" \
      FAKE_STRACE_LOG="$d/strace.log" FAKE_STRACE_FX="$d/fx" FAKE_LLEGADA="$d/llegada.txt" \
      GATE_CLUSTER_ROOT="$d/clusters" "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out" 2>"$d/err" || true
    row_want A0b "HOME y XDG_CACHE_HOME vacios tras --run" "$(find "$h" "$x" -mindepth 1 | wc -l | tr -d ' ')" 0 || return 1
    row_atleast A0b "informe bajo TMPDIR" "$(find "$t" -path '*/memory-industry-audit.*/audit' -type d | wc -l | tr -d ' ')" 1 || return 1
  }

  row_a1() { # una unidad de DBG y otra de DC escriben la misma ruta: conflicto
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_lane "$d/tr" 300 NLI 301 gamma-0123456789abcdef
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
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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
    tr_lane "$tr" 100 DBG 101 alfa-0123456789abcdef
    tr_fork "$tr" 100 111
    tr_exec "$tr" 111 /r/t/T_dbg/debug/deps/gamma-0123456789abcdef gamma-0123456789abcdef
    tr_lane "$tr" 300 REL 301 ronda-0123456789abcdef
    tr_lane "$tr" 400 E2E 401 eco-0123456789abcdef
    tr_open "$tr" 101 w "/r/run/tmp/DBG/a"
    tr_open "$tr" 111 w "/r/run/tmp/DC/b"
    tr_open "$tr" 101 w "/tc/dbg.Ab12/x"
    tr_open "$tr" 301 w "/r/t/T_rel/tmp/shared.lock"
    tr_open "$tr" 401 w "/r/t/T_rel/tmp/shared.lock"
    au_run "$d"
    row_want A2 exit "$RC" 1 || return 1
    row_atleast A2 "root-intrusion" "$(au_count "$d/out" root-intrusion /r/run/tmp/DC/b)" 1 || return 1
    row_atleast A2 "cluster-write" "$(au_count "$d/out" cluster-write /tc/dbg.Ab12/x)" 1 || return 1
    row_atleast A2 "path-conflict target" "$(au_count "$d/out" path-conflict /r/t/T_rel/tmp/shared.lock)" 1 || return 1
    row_want A2 "raiz propia sin hallazgo" "$(au_count "$d/out" '*' /r/run/tmp/DBG/a)" 0 || return 1
  }

  row_a3() { # el puerto del corpus vivo es rojo siempre y no se puede permitir
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_tcp "$d/tr" 101 AF_INET 5489 0
    au_run "$d"
    row_want A4 "sin --pg-tcp-allowed" "$RC" 1 || return 1
    row_atleast A4 "pg-tcp 5489" "$(au_count "$d/out" pg-tcp 5489)" 1 || return 1
    au_run "$d" --pg-tcp-allowed 5489
    row_want A4 "con --pg-tcp-allowed 5489" "$RC" 0 || return 1
    row_atleast A4 "pg-connect 5489" "$(au_count "$d/out" pg-connect 5489)" 1 || return 1
    e="$(rd)"; au_std "$e"
    tr_lane "$e/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_tcp "$e/tr" 101 AF_INET6 5490 0
    au_run "$e"
    row_want A4 "AF_INET6 al 5490" "$RC" 1 || return 1
    row_atleast A4 "pg-tcp 5490" "$(au_count "$e/out" pg-tcp 5490)" 1 || return 1
  }

  row_a5() { # sockets de Postgres: el suyo, el de otro carril y uno de fuera
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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
    tr_exec "$d/tr" 100 /usr/bin/bash bash scripts/merge-gate.sh --step DBG
    tr_fork "$d/tr" 100 101
    tr_exec "$d/tr" 101 /r/t/T_dbg/debug/deps/alfa-0123456789abcdef alfa-0123456789abcdef
    tr_open "$d/tr" 101 r /r/x
    au_run "$d"
    row_want A5b exit "$RC" 2 || return 1
    row_has A5b "ancla" "$(cat "$d/err")" "the audit saw no connection" || return 1
  }

  row_a6() { # puertos fijos: conflicto entre pasos, informativo dentro de uno, el 0 no cuenta
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_put "$d/tr" 101 "chdir(\"$HOME/repo/rust\") = 0"
    tr_put "$d/tr" 101 "openat(AT_FDCWD</$HOME/repo/rust>, \"tests/fixtures/out.json\", O_WRONLY|O_CREAT, 0644) = 4</$HOME/repo/rust/tests/fixtures/out.json>"
    tr_fork "$d/tr" 101 102
    tr_put "$d/tr" 102 "openat(3</$HOME/repo/rust/tests/fixtures>, \"out.json\", O_WRONLY|O_CREAT, 0644) = 5</$HOME/repo/rust/tests/fixtures/out.json>"
    au_run "$d"
    row_atleast A7 "home-write con la ruta absoluta" "$(au_count "$d/out" home-write "$HOME/repo/rust/tests/fixtures/out.json")" 1 || return 1
    e="$(rd)"; au_std "$e"
    tr_lane "$e/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_put "$e/tr" 101 "chdir(\"$HOME/repo/rust\") = 0"
    tr_fork "$e/tr" 101 102
    tr_put "$e/tr" 102 "openat(3, \"out.json\", O_WRONLY|O_CREAT, 0644) = 5"
    au_run "$e"
    row_want A7 "dirfd sin -y" "$RC" 2 || return 1
    row_has A7 "trace without -y" "$(cat "$e/err")" "trace without -y" || return 1
  }

  row_a7c() { # el hijo hereda la cwd del clone: su openat relativo, sin anotacion de cwd, sale de ella
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_put "$d/tr" 101 "chdir(\"$HOME/repo/rust\") = 0"
    tr_fork "$d/tr" 101 102
    tr_put "$d/tr" 102 'openat(AT_FDCWD, "tests/fixtures/out.json", O_WRONLY|O_CREAT, 0644) = 5'
    au_run "$d"
    row_want A7c exit "$RC" 0 || return 1
    row_atleast A7c "home-write del hijo en la cwd heredada" "$(au_count "$d/out" home-write "$HOME/repo/rust/tests/fixtures/out.json")" 1 || return 1
  }

  row_a8() { # de quien es cada escritura: la del daemon de un test es del test; la de rustc no tiene dueno
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_put "$d/tr" 101 'openat(AT_FDCWD</x>, 0x55d0'
    au_run "$d"
    row_want A9 "linea cortada" "$RC" 2 || return 1
    row_has A9 "fichero" "$(cat "$d/err")" "t.101" || return 1
    e="$(rd)"; au_std "$e"
    tr_lane "$e/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_put "$e/tr" 101 'openat(AT_FDCWD</x>, "/h/a", O_RDONLY <unfinished ...>'
    au_run "$e"
    row_want A9 "unfinished sin resumed" "$RC" 2 || return 1
    row_has A9 "fichero" "$(cat "$e/err")" "t.101" || return 1
  }

  row_a10() { # un openat de escritura que fallo no cuenta; el control de al lado, que si cuenta, prueba que la fila puede fallar
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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
    local d t prop; d="$(rd)"; au_tree "$d" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" "60 DC P target:T_docs,cluster:C_docs normal"
    t="$d/tree"; prop="$d/run/audit/groups.proposed.txt"
    au_fake_strace "$d/bin"
    au_fx "$d"
    tr_open "$d/fx/gate" 122 w "$HOME/.cache/x/shared"
    tr_open "$d/fx/gate" 132 w "$HOME/.cache/x/shared"
    tr_open "$d/fx/gate" 132 r "$t/models/emb.onnx"
    tr_bind "$d/fx/gate" 122 8787
    au_go "$d"
    row_want A11 "propuesta escrita" "$([[ -f "$prop" ]] && echo si || echo no)" si || return 1
    row_atleast A11 "grupo db" "$(awk '$2 ~ /(^|,)db(,|$)/ { n++ } END { print n + 0 }' "$prop")" 1 || return 1
    row_atleast A11 "grupo models" "$(awk '$2 ~ /(^|,)models(,|$)/ { n++ } END { print n + 0 }' "$prop")" 1 || return 1
    row_atleast A11 "grupo port:8787" "$(awk '$2 ~ /(^|,)port:8787(,|$)/ { n++ } END { print n + 0 }' "$prop")" 1 || return 1
    row_atleast A11 "cabecera path:" "$(grep -c '^# path:' "$prop" || true)" 1 || return 1
    row_want A11 "ninguna exclusive" "$(grep -c exclusive "$prop" || true)" 0 || return 1
    row_want A11 "gate-groups.txt intacto" "$(cat "$t/scripts/gate-groups.txt")" SENTINEL || return 1
  }

  row_a12() { # sin strace no se audita, y se dice; con un strace que dice 5.2 tambien sale 2
    local d rc=0; d="$(rd)"; au_tree "$d" "10 P - - normal" "20 DBG P target:T_dbg,cluster:C_dbg,models normal"
    au_path_sin_strace "$d"
    PATH="$d/nobin" GATE_AUDIT_RUN="$d/run" FAKE_LLEGADA="$d/llegada.txt" \
      "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out" 2>"$d/err" || rc=$?
    row_want A12 exit "$rc" 2 || return 1
    row_has A12 "motivo" "$(cat "$d/err")" "strace is not installed" || return 1
    row_has A12 "nunca" "$(cat "$d/err")" "never skipped" || return 1
    row_want A12 "merge-gate no lanzado" "$([[ -e "$d/llegada.txt" ]] && echo si || echo no)" no || return 1
    au_fake_strace "$d/bin52"
    rc=0
    FAKE_STRACE_VERSION=5.2 FAKE_STRACE_LOG="$d/strace.log" FAKE_STRACE_FX="$d/fx" FAKE_LLEGADA="$d/llegada.txt" \
      PATH="$d/bin52:$PATH" GATE_AUDIT_RUN="$d/run" "$BASH" "$d/tree/scripts/gate-audit.sh" --run >"$d/out2" 2>"$d/err2" || rc=$?
    row_want A12 "strace 5.2" "$rc" 2 || return 1
    row_has A12 "version 5.2" "$(cat "$d/err2")" "5.2" || return 1
  }

  row_a13() { # el audit lanza merge-gate.sh UNA vez bajo strace, con GATE_DAG=0 y sin --step: las filas las lanza el planificador; el exit es el de --analyze
    local d; d="$(rd)"; au_tree "$d" "10 P - - normal" "30 B3 P target:T_rel,cluster:C_b3 normal" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" \
      "60 DC P target:T_docs,cluster:C_docs normal"
    au_fake_strace "$d/bin"
    au_fx "$d"
    au_go "$d"
    row_want A13 exit "$RC" 0 || return 1
    row_want A13 "invocaciones de strace" "$(wc -l < "$d/strace.log" | tr -d ' ')" 1 || return 1
    row_has A13 "trazas en audit/gate" "$(cat "$d/strace.log")" "audit/gate/t" || return 1
    row_has A13 "el merge-gate del arbol" "$(cat "$d/strace.log")" "$d/tree/scripts/merge-gate.sh" || return 1
    row_want A13 "ninguna fila lanzada por el audit" "$(grep -c -- '--step' "$d/strace.log" || true)" 0 || return 1
    row_want A13 "merge-gate lanzado una vez" "$(grep -c '^GATE_DAG=' "$d/llegada.txt" || true)" 1 || return 1
    row_want A13 "en serie" "$(sed -n 's/^GATE_DAG=//p' "$d/llegada.txt")" 0 || return 1
    row_want A13 "sin argumentos" "$(sed -n 's/^args=//p' "$d/llegada.txt")" "" || return 1
    row_want A13 "la raiz de target del analisis es la de merge-gate" "$(sed -n 's/^CARGO_TARGET_DIR=//p' "$d/llegada.txt")" "$d/targets" || return 1
    row_want A13 "-ff" "$(grep -c -- ' -ff ' "$d/strace.log" || true)" 1 || return 1
    row_want A13 "-y" "$(grep -c -- ' -y ' "$d/strace.log" || true)" 1 || return 1
    row_want A13 "-f" "$(grep -c -- ' -f ' "$d/strace.log" || true)" 1 || return 1
    row_want A13 "sin -v" "$(grep -c -- ' -v ' "$d/strace.log" || true)" 0 || return 1
  }

  row_a13b() { # un merge-gate que sale distinto de 0 deja la auditoria incompleta: exit 2, el resto se escribe igual
    local d; d="$(rd)"; au_tree "$d" "10 P - - normal" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" \
      "60 DC P target:T_docs,cluster:C_docs normal"
    au_fake_strace "$d/bin"
    au_fx "$d"
    printf '101\n' > "$d/tree/scripts/exit-gate"
    au_go "$d"
    row_want A13b exit "$RC" 2 || return 1
    row_has A13b "incompleta" "$(cat "$d/out" "$d/err")" "incomplete: merge-gate.sh exited 101" || return 1
    row_want A13b "report.tsv escrito" "$([[ -f "$d/run/audit/report.tsv" ]] && echo si || echo no)" si || return 1
  }

  row_a13c() { # trazas por encima de GATE_AUDIT_MAX_GB: exit 2 y sin truncar
    local d rc=0; d="$(rd)"; au_tree "$d" "20 DBG P target:T_dbg,cluster:C_dbg,models normal"
    au_fake_strace "$d/bin"
    au_fx "$d"
    head -c 1048576 /dev/zero | tr '\0' x > "$d/fx/gate/t.999"
    au_go "$d" GATE_AUDIT_MAX_GB=0
    rc="$RC"
    row_want A13c exit "$rc" 2 || return 1
    row_has A13c "tope" "$(cat "$d/err")" "GATE_AUDIT_MAX_GB" || return 1
    row_want A13c "trazas sin truncar" "$(wc -c < "$d/run/audit/gate/t.999" | tr -d ' ')" 1048576 || return 1
  }

  row_a14() { # formato exacto: cinco campos por linea, ordenadas, con la clave de --keys
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
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

  row_a15() { # las filas llevan NAME=value delante y la orden --step: el audit las reconoce por el argv de merge-gate.sh --step <id>
    local d line; d="$(rd)"; au_tree "$d" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" "60 DC P target:T_docs,cluster:C_docs normal"
    line="$(grep ' DBG ' "$d/tree/scripts/gate-dag.txt")"
    row_has A15 "la fila de la tabla lleva su entorno y su orden" "$line" "CARGO_BUILD_JOBS=8 bash scripts/merge-gate.sh --step DBG" || return 1
    au_fake_strace "$d/bin"
    au_fx "$d"
    tr_open "$d/fx/gate" 122 w "$HOME/.cache/x/shared"
    tr_open "$d/fx/gate" 132 w "$HOME/.cache/x/shared"
    au_go "$d"
    row_want A15 exit "$RC" 1 || return 1
    row_atleast A15 "path-conflict" "$(au_count "$d/out" path-conflict "$HOME/.cache/x/shared")" 1 || return 1
    row_want A15 "los pasos del conflicto son DBG y DC" \
      "$(awk -F'\t' -v p="$HOME/.cache/x/shared" '$1 == "path-conflict" && index($0, p) { print $2 }' "$d/out" | head -n 1)" "DBG,DC" || return 1
    row_want A15 "ninguna unidad sin paso" "$(awk -F'\t' 'index($2, "?") { n++ } END { print n + 0 }' "$d/out")" 0 || return 1
  }

  row_a16() { # la raiz de un target es <CARGO_TARGET_DIR>/<clave de arbol>/<T>, y la clave sale de gate-dag.py tree-key, la misma formula del planificador
    local d tt key; d="$(rd)"; au_tree "$d" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" "60 DC P target:T_docs,cluster:C_docs normal"
    key="$(au_key "$d/tree")"
    row_want A16 "gate-dag.py tree-key" "$(python3 -I "$d/tree/scripts/gate-dag.py" tree-key "$d/tree" 2>/dev/null || true)" "$key" || return 1
    au_fake_strace "$d/bin"
    au_fx "$d"
    tt="$d/targets/$key"
    tr_open "$d/fx/gate" 132 w "$tt/T_dbg/stolen"
    au_go "$d"
    row_want A16 exit "$RC" 1 || return 1
    row_atleast A16 "root-intrusion en el target de DBG" "$(au_count "$d/out" root-intrusion "$tt/T_dbg/stolen")" 1 || return 1
    row_has A16 "dice que target" "$(cat "$d/out")" "writes in target T_dbg without declaring it" || return 1
    row_want A16 "el target propio sin hallazgo" "$(au_count "$d/out" '*' "$tt/T_dbg/debug/out")" 0 || return 1
  }

  row_a17() { # el cluster de un carril vive en <GATE_CLUSTER_ROOT>/<carril>.XXXXXX, y el carril sale del nombre del recurso cluster:C_<carril>
    local d; d="$(rd)"; au_tree "$d" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" "60 DC P target:T_docs,cluster:C_docs normal"
    au_fake_strace "$d/bin"
    au_fx "$d"
    tr_unix "$d/fx/gate" 122 "$d/clusters/docs.Cd34Ef/.s.PGSQL.5432" 0
    au_go "$d"
    row_want A17 exit "$RC" 1 || return 1
    row_atleast A17 "pg-connect al cluster propio" "$(au_count "$d/out" pg-connect "$d/clusters/dbg.Ab12Cd")" 1 || return 1
    row_atleast A17 "pg-socket-cross al de otro carril" "$(au_count "$d/out" pg-socket-cross "$d/clusters/docs.Cd34Ef")" 1 || return 1
    row_want A17 "ningun pg-socket-outside" "$(au_count "$d/out" pg-socket-outside "")" 0 || return 1
  }

  row_a18() { # el directorio de corrida del planificador sale de la traza: la fila que escribe en el de otra es intrusion, y sin ese directorio no hay auditoria
    local d e dag; d="$(rd)"; au_tree "$d" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" "60 DC P target:T_docs,cluster:C_docs normal"
    au_fake_strace "$d/bin"
    au_fx "$d"
    dag="$d/tmp/memory-industry-dag.Ab12Cd"
    tr_open "$d/fx/gate" 132 w "$dag/tmp/DBG/stolen"
    au_go "$d"
    row_want A18 exit "$RC" 1 || return 1
    row_atleast A18 "root-intrusion en la raiz de DBG" "$(au_count "$d/out" root-intrusion "$dag/tmp/DBG/stolen")" 1 || return 1
    row_has A18 "dice de quien" "$(cat "$d/out")" "writes in the run directory of DBG" || return 1
    e="$(rd)"; au_tree "$e" "20 DBG P target:T_dbg,cluster:C_dbg,models normal" "60 DC P target:T_docs,cluster:C_docs normal"
    au_fake_strace "$e/bin"
    au_fx "$e"
    sed -i '/^mkdir(/d' "$e/fx/gate/t.120"
    au_go "$e"
    row_want A18 "sin directorio de corrida" "$RC" 2 || return 1
    row_has A18 "motivo" "$(cat "$e/err")" "run directory" || return 1
  }

  row_a19() { # strace alinea el resultado de una llamada corta a una columna (`vfork()      = 102`): se lee igual que el de una sola separacion
    local d; d="$(rd)"; au_std "$d"
    tr_lane "$d/tr" 100 DBG 101 alfa-0123456789abcdef
    tr_put "$d/tr" 101 'vfork()                                 = 102'
    tr_put "$d/tr" 101 'chdir("/r")                             = 0'
    tr_put "$d/tr" 102 "openat(AT_FDCWD</r>, \"$HOME/.cache/x/padded\", O_WRONLY|O_CREAT, 0644)    = 5</$HOME/.cache/x/padded>"
    au_run "$d"
    row_want A19 "una traza de strace con el resultado alineado se lee" "$RC" 0 || return 1
    row_atleast A19 "la escritura del hijo creado por vfork" "$(au_count "$d/out" home-write "$HOME/.cache/x/padded")" 1 || return 1
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
  row_a15 || exit 1
  row_a16 || exit 1
  row_a17 || exit 1
  row_a18 || exit 1
  row_a19 || exit 1
  echo "OK  self-test: gate-audit"
  exit 0
fi
# --- self-test end ---

# El puerto vivo sale de gate-lock.sh (D-A11). Va despues del despacho de --self-test: el autotest no lo necesita.
source "$(dirname "$0")/gate-lock.sh"

# --- analisis (D-A1 a D-A10): trazas de strace -> hallazgos -------------------
# Lee trazas, nunca conecta a nada (D-A5). El analizador es gawk: el awk de esta
# maquina y de Ubuntu con gawk instalado; mawk no tiene match() con captura.

ga_fail() { echo "FAIL: audit: $*" >&2; exit 2; }
ga_usage() {
  echo "uso: gate-audit.sh --self-test" >&2
  echo "     gate-audit.sh --analyze <trazas> --roots <f> --steps <f> [--overlap <f>] [--keys <f>] [--pg-tcp-allowed <puerto>]" >&2
  echo "     gate-audit.sh --run [--step <id>[,<id>...]]" >&2
  exit 2
}
ga_in() { local x; for x in "${@:2}"; do [[ "$x" == "$1" ]] && return 0; done; return 1; }
GA_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

read -r -d '' GA_AWK <<'AWK' || true
function basename(p,   n, a) { n = split(p, a, "/"); return a[n] }
function under(path, pre) {
  if (pre == "") return 0
  if (path == pre || index(path, pre "/") == 1) return 1
  return substr(pre, length(pre), 1) == "." && index(path, pre) == 1
}
function fatal(loc, msg) {
  printf "FAIL: audit: %s: %s\n", loc, msg > "/dev/stderr"
  bad = 1
  exit 2
}
function unq(t) { return substr(t, 2, length(t) - 2) }
function annot(tok) {
  if (match(tok, /<[^>]*>/)) return substr(tok, RSTART + 1, RLENGTH - 2)
  return ""
}
function isunit(bin,   nm) {
  nm = basename(bin)
  return (bin ~ /\/deps\/[^\/]+-[0-9a-f]{16}$/) || (nm in TOOLS)
}
function keyof(nm) { return (nm in keymap) ? keymap[nm] : nm }
function step_for(argv,   k) {
  for (k = 1; k <= nsteps; k++) if (argv ~ sere[k]) return sid[k]
  return ""
}
function overlap(a, b) {
  if (a == b) return 0
  if (a == "?" || b == "?") return 1
  if (ovall) return 1
  return ((a, b) in ov)
}
function emit(cls, p, u, r, d,   line) {
  line = cls "\t" p "\t" u "\t" r "\t" d
  if (line in seen) return
  seen[line] = 1
  nout++
  fout[nout] = line
  if (cls in RED) nred++
  else ninfo++
}
function addev(k, kind, a, b, c, d,   i) {
  nev[k]++
  i = nev[k]
  ek[k, i] = kind; ea[k, i] = a; eb[k, i] = b; ec[k, i] = c; ed[k, i] = d
  el[k, i] = FILENAME ":" FNR
}
function parse_line(line,   op, cl, name, args, res, toks, nt, i, t, flags, nps, pth, pdt, dtok, rw, isopen, fam, port, bin, argv, i1, i2, child, ok, m) {
  op = index(line, "(")
  if (op == 0) fatal(FILENAME ":" FNR, "not a syscall line: " line)
  name = substr(line, 1, op - 1)
  cl = index(line, ") = ")
  if (cl == 0) fatal(FILENAME ":" FNR, "a line with no result (cut?): " line)
  args = substr(line, op + 1, cl - op - 1)
  res = substr(line, cl + 4)
  if (name == "execve") {
    if (res !~ /^0/) return
    if (!match(args, /"[^"]*"/)) fatal(FILENAME ":" FNR, "execve with no path")
    bin = substr(args, RSTART + 1, RLENGTH - 2)
    i1 = index(args, "["); i2 = index(args, "]")
    if (i1 == 0 || i2 <= i1) fatal(FILENAME ":" FNR, "execve with no argv")
    argv = substr(args, i1 + 1, i2 - i1 - 1)
    gsub(/", "/, " ", argv); gsub(/"/, "", argv)
    addev(key, "X", bin, argv, "", "")
  } else if (name == "clone" || name == "clone3" || name == "fork" || name == "vfork") {
    if (res !~ /^[0-9]+/) return
    match(res, /^[0-9]+/); child = substr(res, 1, RLENGTH)
    addev(key, "F", dir "/t." child, "", "", "")
  } else if (name == "chdir" || name == "fchdir") {
    if (res !~ /^0/) return
    if (name == "chdir") {
      if (!match(args, /"[^"]*"/)) fatal(FILENAME ":" FNR, "chdir with no path")
      addev(key, "D", substr(args, RSTART + 1, RLENGTH - 2), "", "", "")
    } else {
      if (annot(args) == "") fatal(FILENAME ":" FNR, "trace without -y: fchdir with no path")
      addev(key, "D", annot(args), "", "", "")
    }
  } else if (name ~ /^(open|openat|creat|mkdir|mkdirat|unlink|unlinkat|rename|renameat|renameat2|link|linkat|symlink|symlinkat)$/) {
    isopen = (name == "open" || name == "openat" || name == "creat")
    if (isopen) { if (res !~ /^[0-9]/) return }
    else if (res !~ /^0/) return
    nt = split(args, toks, ", ")
    dtok = "AT_FDCWD"; flags = ""; nps = 0
    for (i = 1; i <= nt; i++) {
      t = toks[i]
      if (substr(t, 1, 1) == "\"") { nps++; pth[nps] = unq(t); pdt[nps] = dtok; dtok = "AT_FDCWD" }
      else if (t ~ /^(AT_FDCWD|[0-9])/) dtok = t
      else if (flags == "" && t ~ /O_/) flags = t
    }
    if (nps == 0) fatal(FILENAME ":" FNR, name " with no path")
    rw = "W"
    if (isopen && name != "creat" && flags !~ /O_(WRONLY|RDWR|CREAT|TRUNC|APPEND)/) rw = "R"
    for (i = 1; i <= nps; i++) addev(key, "P", pth[i], pdt[i], annot(pdt[i]), rw)
  } else if (name == "bind" || name == "connect") {
    if (!match(args, /sa_family=[A-Z0-9_]+/)) fatal(FILENAME ":" FNR, name " with no family")
    fam = substr(args, RSTART + 10, RLENGTH - 10)
    ok = (res ~ /^0/ || res ~ /^-1 EINPROGRESS/) ? "1" : "0"
    if (fam == "AF_UNIX") {
      if (!match(args, /sun_path="[^"]*"/)) fatal(FILENAME ":" FNR, name " AF_UNIX with no path")
      m = substr(args, RSTART + 10, RLENGTH - 11)
      if (name == "bind") { if (res ~ /^0/) addev(key, "P", m, "AT_FDCWD", "", "W") }
      else addev(key, "C", fam, m, ok, "")
    } else {
      if (!match(args, /htons\([0-9]+\)/)) fatal(FILENAME ":" FNR, name " " fam " with no port")
      port = substr(args, RSTART + 6, RLENGTH - 7)
      if (name == "bind") { if (res ~ /^0/) addev(key, "B", port, "", "", "") }
      else addev(key, "C", fam, port, ok, "")
    }
  } else fatal(FILENAME ":" FNR, "syscall not understood: " name)
}
function connect_ev(u, s, fam, val, ok,   st, port, cn, r) {
  st = (s == "" ? "?" : s)
  if (fam == "AF_UNIX") {
    if (ok != "1" || val !~ /\.s\.PGSQL\.[0-9]+$/) return
    cn = ""
    for (r = 1; r <= nroots; r++) if (rtype[r] == "cluster" && under(val, rpre[r])) { cn = rown[r]; break }
    if (cn != "" && ((st, cn) in sdecl_c)) { pgconn++; emit("pg-connect", st, uname[u], val, "cluster " cn) }
    else if (cn != "") emit("pg-socket-cross", st, uname[u], val, "cluster " cn " is not declared by " st)
    else emit("pg-socket-outside", st, uname[u], val, "no cluster root covers it")
    return
  }
  port = val + 0
  if (port == live_port + 0) { emit("live-port", st, uname[u], "tcp:" live_port, "connects to the live corpus, any result"); return }
  if (ok != "1" || port < 5400 || port > 5599) return
  if (port == pgallowed + 0) { pgconn++; emit("pg-connect", st, uname[u], "tcp:" port, "allowed by --pg-tcp-allowed") }
  else emit("pg-tcp", st, uname[u], "tcp:" port, "Postgres over TCP, not allowed")
}
function fixed(u, s, port,   st) {
  st = (s == "" ? "?" : s)
  emit("fixed-port", st, uname[u], "tcp:" port + 0, "bound by a test; informative")
  nb++
  bport[nb] = port + 0; bu[nb] = u; bs[nb] = st
}
function record(u, s, p, rw) {
  opn++
  opath[opn] = p; ou[opn] = u; os[opn] = (s == "" ? "?" : s); orw[opn] = rw
}
function walk(k, cwd, unit, step,   i, n, kind, p, base, abs, s2) {
  n = nev[k]
  for (i = 1; i <= n; i++) {
    kind = ek[k, i]
    if (kind == "X") {
      s2 = step_for(eb[k, i]); if (s2 != "") step = s2
      if (isunit(ea[k, i])) {
        unit = k
        if (!(k in uname)) { uname[k] = keyof(basename(ea[k, i])); ustep[k] = (step == "" ? "?" : step); nunits++ }
      }
    } else if (kind == "D") {
      p = ea[k, i]
      if (substr(p, 1, 1) == "/" || cwd == "") cwd = p
      else cwd = cwd "/" p
    } else if (kind == "F") {
      walk(ea[k, i], cwd, unit, step)
    } else if (kind == "P") {
      p = ea[k, i]
      if (substr(p, 1, 1) == "/") abs = p
      else if (eb[k, i] ~ /^AT_FDCWD/) {
        base = (cwd != "") ? cwd : ec[k, i]
        if (base == "") fatal(el[k, i], "relative path with no working directory: " p)
        abs = base "/" p
      } else {
        if (ec[k, i] == "") fatal(el[k, i], "trace without -y: a numeric dirfd with no path behind it (" p ")")
        abs = ec[k, i] "/" p
      }
      if (unit != "") record(unit, step, abs, ed[k, i])
    } else if (kind == "C") {
      if (unit != "") connect_ev(unit, step, ea[k, i], eb[k, i], ec[k, i])
    } else if (kind == "B") {
      if (unit != "" && ea[k, i] + 0 != 0) fixed(unit, step, ea[k, i])
    }
  }
  if (unit == "" && step == "") orphan++
}
BEGIN {
  RED["live-port"] = 1; RED["pg-tcp"] = 1; RED["pg-socket-outside"] = 1; RED["pg-socket-cross"] = 1
  RED["cluster-write"] = 1; RED["root-intrusion"] = 1; RED["path-conflict"] = 1; RED["port-conflict"] = 1
  TOOLS["e2e_all_tools.py"] = 1; TOOLS["mcp_live_session_test.py"] = 1; TOOLS["eval_seed.py"] = 1
  home = ENVIRON["HOME"]
  ovall = (ovfile == "")
  if (pgallowed == "") pgallowed = 0
  if (rootsf != "") while ((getline ln < rootsf) > 0) {
    if (split(ln, f, " ") < 3) continue
    nroots++; rtype[nroots] = f[1]; rown[nroots] = f[2]; rpre[nroots] = f[3]
    if (f[1] == "cluster") nrc++
  }
  if (stepsf != "") while ((getline ln < stepsf) > 0) {
    p1 = index(ln, " "); if (p1 == 0) continue
    id = substr(ln, 1, p1 - 1); rest = substr(ln, p1 + 1)
    p2 = index(rest, " "); if (p2 == 0) continue
    rec = substr(rest, 1, p2 - 1); ere = substr(rest, p2 + 1)
    nsteps++; sid[nsteps] = id; sere[nsteps] = ere
    np = split(rec, parts, ",")
    for (q = 1; q <= np; q++) {
      if (parts[q] ~ /^target:/) sdecl_t[id, substr(parts[q], 8)] = 1
      else if (parts[q] ~ /^cluster:/) sdecl_c[id, substr(parts[q], 9)] = 1
    }
  }
  if (ovfile != "") while ((getline ln < ovfile) > 0) {
    if (split(ln, f, " ") == 2) { ov[f[1], f[2]] = 1; ov[f[2], f[1]] = 1 }
  }
  if (keysf != "") while ((getline ln < keysf) > 0) {
    if (split(ln, f, " ") == 2) keymap[f[1]] = f[2]
  }
}
{
  key = FILENAME
  dir = FILENAME; sub(/\/[^\/]*$/, "", dir)
  line = $0
  if (line == "" || line ~ /^\+\+\+ / || line ~ /^--- /) next
  if (index(line, " <unfinished ...>") > 0) { pend[key] = substr(line, 1, index(line, " <unfinished ...>") - 1); next }
  if (index(line, " resumed>") > 0) {
    if (!(key in pend)) fatal(FILENAME ":" FNR, "<... resumed> with no <unfinished ...> before it")
    p = pend[key]; delete pend[key]
    line = p substr(line, index(line, "resumed>") + 8)
  }
  parse_line(line)
}
END {
  if (bad) exit 2
  for (k in pend) fatal(k, "<unfinished ...> with no <... resumed> after it")
  for (k in nev) for (i = 1; i <= nev[k]; i++) if (ek[k, i] == "F") ischild[ea[k, i]] = 1
  for (k in nev) if (!(k in ischild)) walk(k, "", "", "")
  if (nunits == 0) {
    printf "FAIL: audit: no unit: no trace execs a test binary (/deps/<name>-<16 hex>) or a named tool\n" > "/dev/stderr"
    exit 2
  }
  if (nrc > 0 && pgconn == 0) {
    printf "FAIL: audit: the audit saw no connection to any cluster of --roots: no trace reaches a Postgres socket or port\n" > "/dev/stderr"
    exit 2
  }
  for (i = 1; i <= opn; i++) {
    p = opath[i]; u = ou[i]; s = os[i]; rw = orw[i]; disp = uname[u]; h = "generic"
    for (r = 1; r <= nroots; r++) {
      if (rtype[r] == "run" && rw == "W" && under(p, rpre[r])) {
        if (s == rown[r]) h = "private"
        else { h = "intrusion"; emit("root-intrusion", s, disp, p, "writes in the run directory of " rown[r]) }
        break
      }
    }
    if (h == "generic") for (r = 1; r <= nroots; r++) {
      if (rtype[r] == "cluster" && rw == "W" && under(p, rpre[r])) { h = "cluster"; emit("cluster-write", s, disp, p, "writes under cluster " rown[r]); break }
    }
    if (h == "generic") for (r = 1; r <= nroots; r++) {
      if (rtype[r] == "target" && under(p, rpre[r])) {
        if (rw == "W" && !((s, rown[r]) in sdecl_t)) { h = "intrusion"; emit("root-intrusion", s, disp, p, "writes in target " rown[r] " without declaring it") }
        break
      }
    }
    if (h == "generic" && rw == "W" && home != "" && index(p, home "/") == 1) emit("home-write", s, disp, p, "writes under HOME")
    if (rw == "R" && p ~ /\.onnx$/) emit("model-read", s, disp, p, "reads a model")
    oh[i] = h
    gp[p] = (p in gp) ? gp[p] "," i : i
  }
  for (p in gp) {
    n = split(gp[p], idx, ",")
    for (a = 1; a <= n; a++) for (b = a + 1; b <= n; b++) {
      x = idx[a]; y = idx[b]
      if (oh[x] != "generic" || oh[y] != "generic") continue
      if (ou[x] == ou[y] || (orw[x] == "R" && orw[y] == "R")) continue
      if (!overlap(os[x], os[y])) continue
      s1 = os[x]; s2 = os[y]; if (s2 < s1) { t = s1; s1 = s2; s2 = t }
      u1 = uname[ou[x]]; u2 = uname[ou[y]]; if (u2 < u1) { t = u1; u1 = u2; u2 = t }
      emit("path-conflict", s1 "," s2, u1 "," u2, p, "both touch it and one of them writes")
    }
  }
  for (a = 1; a <= nb; a++) for (b = a + 1; b <= nb; b++) {
    if (bport[a] != bport[b] || bu[a] == bu[b] || !overlap(bs[a], bs[b])) continue
    s1 = bs[a]; s2 = bs[b]; if (s2 < s1) { t = s1; s1 = s2; s2 = t }
    u1 = uname[bu[a]]; u2 = uname[bu[b]]; if (u2 < u1) { t = u1; u1 = u2; u2 = t }
    emit("port-conflict", s1 "," s2, u1 "," u2, "tcp:" bport[a], "both bind the same port")
  }
  if (orphan > 0) emit("orphan-procs", "-", "-", "-", orphan " process(es) with no unit")
  for (i = 1; i <= nout; i++) print fout[i]
  for (k in uname) if (!(ustep[k] in sst)) { sst[ustep[k]] = 1; nst++ }
  printf "@@counts %d %d %d %d\n", nunits, nst, nred, ninfo
}
AWK

ga_depends() { # <a> <b>: a depende de b, directa o transitivamente (tabla de --run)
  local d
  for d in ${deps_of[$1]:-}; do
    [[ "$d" == "$2" ]] && return 0
    ga_depends "$d" "$2" && return 0
  done
  return 1
}

ga_analyze() { # <trazas> <roots> <steps> <overlap|""> <keys|""> <pg-tcp-allowed> <informe|"">
  local trdir="$1" roots="$2" steps="$3" overlap="$4" keys="$5" allowed="$6" report="$7"
  local -a files=()
  local f raw findings counts summary rc=0 nu ns nr ni
  [[ -d "$trdir" ]] || ga_fail "$trdir is not a directory of traces"
  [[ -f "$roots" ]] || ga_fail "--roots: no such file $roots"
  [[ -f "$steps" ]] || ga_fail "--steps: no such file $steps"
  [[ -z "$overlap" || -f "$overlap" ]] || ga_fail "--overlap: no such file $overlap"
  [[ -z "$keys" || -f "$keys" ]] || ga_fail "--keys: no such file $keys"
  while IFS= read -r f; do files+=("$f"); done < <(find "$trdir" -name 't.*' -type f | LC_ALL=C sort)
  (( ${#files[@]} > 0 )) || ga_fail "no trace files (t.<pid>) under $trdir"
  raw="$(awk -v rootsf="$roots" -v stepsf="$steps" -v ovfile="$overlap" -v keysf="$keys" -v pgallowed="$allowed" -v live_port="$GATE_LIVE_PORT" "$GA_AWK" "${files[@]}")" || rc=$?
  (( rc == 0 )) || exit "$rc"
  counts="$(printf '%s\n' "$raw" | grep '^@@counts' || true)"
  findings="$(printf '%s\n' "$raw" | grep -v -e '^@@counts' -e '^$' | LC_ALL=C sort || true)"
  read -r _ nu ns nr ni <<< "$counts"
  if (( nr == 0 )); then summary="OK  audit: $nu units in $ns steps; 0 red, $ni info"; rc=0
  else summary="FAIL: audit: $nr red finding(s) (above); $nu units in $ns steps"; rc=1
  fi
  [[ -z "$findings" ]] || printf '%s\n' "$findings"
  printf '%s\n' "$summary"
  if [[ -n "$report" ]]; then
    { [[ -z "$findings" ]] || printf '%s\n' "$findings"; printf '%s\n' "$summary"; } > "$report"
  fi
  return "$rc"
}

ga_analyze_cli() {
  local trdir="${1:-}" roots="" steps="" overlap="" keys="" allowed=""
  [[ -n "$trdir" ]] || ga_usage
  shift
  while (( $# )); do
    case "$1" in
      --roots) roots="${2:-}"; shift 2 ;;
      --steps) steps="${2:-}"; shift 2 ;;
      --overlap) overlap="${2:-}"; shift 2 ;;
      --keys) keys="${2:-}"; shift 2 ;;
      --pg-tcp-allowed) allowed="${2:-}"; shift 2 ;;
      *) ga_usage ;;
    esac
  done
  [[ -n "$roots" && -n "$steps" ]] || ga_usage
  if [[ -n "$allowed" ]]; then
    [[ "$allowed" =~ ^[0-9]+$ ]] || ga_fail "--pg-tcp-allowed wants a port number, got $allowed"
    (( allowed != GATE_LIVE_PORT )) || ga_fail "--pg-tcp-allowed $GATE_LIVE_PORT: that is the live corpus and is never allowed"
  fi
  ga_analyze "$trdir" "$roots" "$steps" "$overlap" "$keys" "${allowed:-0}" ""
}

ga_propose() { # <informe> <salida>: propuesta de grupos, fuera del fichero versionado (D-A8)
  local sha hoy
  sha="$(git -C "$GA_ROOT" rev-parse --short HEAD 2>/dev/null || echo none)"
  hoy="$(date +%F)"
  awk -F'\t' -v sha="$sha" -v hoy="$hoy" '
    function add(unit, g) {
      if (index("," grp[unit] ",", "," g ",") == 0) grp[unit] = (grp[unit] == "" ? g : grp[unit] "," g)
    }
    NF == 5 {
      cls = $1; u = $3; r = $4
      if (cls == "path-conflict") {
        if (!(r in pid)) { np++; pid[r] = "p" np; hdr[np] = "# path:" pid[r] " " r }
        n = split(u, us, ",")
        for (q = 1; q <= n; q++) add(us[q], "path:" pid[r])
      } else if (cls == "fixed-port") { split(r, t, ":"); add(u, "port:" t[2]) }
      else if (cls == "pg-connect") add(u, "db")
      else if (cls == "model-read") add(u, "models")
    }
    END {
      for (q = 1; q <= np; q++) print hdr[q]
      for (u in grp) print u " " grp[u] " " sha " " hoy " audit"
    }' "$1" | LC_ALL=C sort > "$2"
}

ga_run() { # --run: la auditoria de verdad, en serie y con la tabla de gate-dag.txt (D-A7, D-A8)
  local only="" run="" max used tblines incomplete="" line idx id deps rec cls order ere
  local tree_key t c arc=0 rc traced part gen
  local -a ids=() parts=() only_ids=() words=() rows=()
  local -A deps_of=() cls_of=()
  local -A traced_id=([DBG]=1 [NLI]=1 [REL]=1 [E2E]=1 [DC]=1 [CV]=1 [M]=1)
  while (( $# )); do
    case "$1" in
      --step) only="${2:-}"; shift 2 ;;
      *) ga_usage ;;
    esac
  done
  command -v strace >/dev/null 2>&1 || ga_fail "strace is not installed (command -v strace found nothing). The audit is never skipped: install it (sudo apt install strace) and run it again."
  local sv
  sv="$(strace -V 2>/dev/null | sed -n '1s/.*version \([0-9][0-9.]*\).*/\1/p')"
  [[ -n "$sv" ]] || ga_fail "cannot read the strace version (strace -V)"
  [[ "$(printf '%s\n' 5.3 "$sv" | LC_ALL=C sort -V | head -n1)" == 5.3 ]] || ga_fail "strace $sv is older than 5.3: --seccomp-bpf together with -y needs it"
  local table="$GA_ROOT/scripts/gate-dag.txt"
  [[ -f "$table" ]] || ga_fail "needs scripts/gate-dag.txt (F3.3)"
  max="${GATE_AUDIT_MAX_GB:-20}"
  [[ "$max" =~ ^[0-9]+$ ]] || ga_fail "GATE_AUDIT_MAX_GB must be a whole number of gigabytes, got $max"
  run="${GATE_AUDIT_RUN:-}"
  [[ -n "$run" ]] || run="$(mktemp -d "${TMPDIR:-/tmp}/memory-industry-audit.XXXXXX")"
  gen="$run/audit/gen"
  mkdir -p "$gen"
  tree_key="$(basename "$GA_ROOT")-$(realpath "$GA_ROOT" | sha256sum | cut -c1-12)"
  : > "$gen/steps"; : > "$gen/roots"; : > "$gen/overlap"
  tblines="$(grep -v -e '^#' -e '^[[:space:]]*$' "$table" | LC_ALL=C sort -k1,1n || true)"
  while IFS= read -r line; do
    [[ -n "$line" ]] || continue
    rows+=("$line")
    read -r idx id deps rec cls _ _ _ order <<< "$line"
    ids+=("$id"); deps_of[$id]="$deps"; cls_of[$id]="$cls"
    ere="$(printf '%s' "$order" | sed -e 's/[][\.*^$+?(){}|/]/\\&/g')"
    printf '%s %s %s\n' "$id" "$rec" "$ere" >> "$gen/steps"
    printf 'run %s %s/tmp/%s\n' "$id" "$run" "$id" >> "$gen/roots"
    IFS=',' read -r -a parts <<< "$rec"
    for part in "${parts[@]}"; do
      case "$part" in
        target:*) t="${part#target:}"; [[ -z "${GATE_TARGET_ROOT:-}" ]] || printf 'target %s %s/%s/%s\n' "$t" "$GATE_TARGET_ROOT" "$tree_key" "$t" >> "$gen/roots" ;;
        cluster:*) c="${part#cluster:}"; [[ -z "${GATE_CLUSTER_ROOT:-}" ]] || printf 'cluster %s %s/%s.\n' "$c" "$GATE_CLUSTER_ROOT" "$c" >> "$gen/roots" ;;
      esac
    done
  done <<< "$tblines"
  local i j a b
  for ((i = 0; i < ${#ids[@]}; i++)); do
    for ((j = i + 1; j < ${#ids[@]}; j++)); do
      a="${ids[i]}"; b="${ids[j]}"
      [[ "${cls_of[$a]}" == exclusive || "${cls_of[$b]}" == exclusive ]] && continue
      ga_depends "$a" "$b" && continue
      ga_depends "$b" "$a" && continue
      printf '%s %s\n' "$a" "$b" >> "$gen/overlap"
    done
  done
  if [[ -n "$only" ]]; then IFS=',' read -r -a only_ids <<< "$only"; fi
  for line in "${rows[@]}"; do
    read -r idx id deps rec cls _ _ _ order <<< "$line"
    read -r -a words <<< "$order"
    traced=0
    if [[ -n "${traced_id[$id]:-}" ]]; then
      if [[ -z "$only" ]] || ga_in "$id" "${only_ids[@]}"; then traced=1; fi
    fi
    mkdir -p "$run/audit/$idx-$id"
    rc=0
    if (( traced )); then
      strace --seccomp-bpf -f -ff -y -qq -e signal=none \
        -e trace=execve,clone,clone3,fork,vfork,chdir,fchdir,open,openat,creat,mkdir,mkdirat,rename,renameat,renameat2,unlink,unlinkat,link,symlink,bind,connect \
        -o "$run/audit/$idx-$id/t" -- "${words[@]}" >"$run/audit/$idx-$id.log" 2>&1 || rc=$?
    else
      "${words[@]}" >"$run/audit/$idx-$id.log" 2>&1 || rc=$?
    fi
    used="$(du -sb "$run/audit" | cut -f1)"
    (( used <= max * 1073741824 )) || ga_fail "the traces in $run/audit weigh $used bytes, over GATE_AUDIT_MAX_GB=$max; they are kept, not truncated"
    (( rc == 0 )) || incomplete+="$id exited $rc; "
  done
  ga_analyze "$run/audit" "$gen/roots" "$gen/steps" "$gen/overlap" "" 0 "$run/audit/report.tsv" || arc=$?
  (( arc != 2 )) || exit 2
  ga_propose "$run/audit/report.tsv" "$run/audit/groups.proposed.txt"
  if [[ -n "$incomplete" ]]; then
    echo "FAIL: audit: incomplete: $incomplete the findings above are what the audit could see, not a verdict" >&2
    exit 2
  fi
  exit "$arc"
}

case "${1:-}" in
  --analyze) shift; ga_analyze_cli "$@" ;;
  --run) shift; ga_run "$@" ;;
  *) ga_usage ;;
esac
