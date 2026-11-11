#!/usr/bin/env bash
# CI-D5c-C · Q7: el paso a la 0.31 ensayado sobre una COPIA del respaldo nocturno de la memoria, con el servicio
# reembebiendo solo (.cursor/handoffs/ci-rapido-D5c-C.spec.md §14 «Q7», §10 y §5). Fuera del juez y sin red.
#
#   Q7_BIN=<binario release 0.31> pesado -p memory_industry -- bash .cursor/qa/d5c/ensayo-q7.sh
#
# No compila. Variables:
#   Q7_BIN         (obligatoria) el binario memory-industry 0.31 del SHA que se ensaya
#   Q7_TOPE_S      segundos que se espera a que el reembebido automático llegue a `done` (3600)
#   Q7_ARRANQUE_S  segundos que se espera a que un daemon recién lanzado conteste /health (400)
#   Q7_CONSULTAS   fichero con las cinco búsquedas de §10 paso 1, una por línea (si falta, cinco genéricas)
#
# Lo que NO toca, y lo impone con guardas que salen 2 (no con este comentario):
#   - el servicio vivo (127.0.0.1:8787 y :8788) ni su base (puerto 5488): no se conecta a ninguno de los tres;
#   - ~/.cache/memory-industry, salvo LEER los modelos (models, reranker, models-nli); no lee pgpass;
#   - el volcado de ~/respaldos-memoria: solo se lee.
# El servidor es un carril desechable del juez (scripts/gate-cluster.sh, cluster_name memory-industry-gate-q7, por
# socket bajo /tmp); el binario corre con un HOME temporal y un entorno hecho de cero. Todo lo que arranca lo apaga.
#
# La base del ensayo es memoria real y privada: de aquí no sale el texto de ninguna fila ni de ninguna consulta,
# solo números, etiquetas de modelo y estados. Los ids de los resultados viven en el temporal y se borran con él.
#
# Cada fase anota [OK] o [FALLA] con su número; al final, el bloque `RESULTADO Q7`, que es lo que va al traspaso.
# Sale 0 si ninguna comprobación falló, 1 si alguna, 2 si una guarda lo paró.
set -uo pipefail

AQUI="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RAIZ="$(cd "$AQUI/../../.." && pwd)"

LANE=q7
BASE=d5c_ensayo
CACHE="$HOME/.cache/memory-industry"
RUNTIME_NUEVO="$HOME/.cache/memory-industry-gate/runtime-home/.cache/memory-industry/onnxruntime/libonnxruntime.so"
RUNTIME_VIEJO="$HOME/.cache/d5b-prueba/ort-1.26.0/libonnxruntime.so"
VERSION_NUEVA=1.31.1
VERSION_VIEJA=1.26.0
Q7_TOPE_S="${Q7_TOPE_S:-3600}"
Q7_ARRANQUE_S="${Q7_ARRANQUE_S:-400}"
# El ciclo REM (a los 300 s del arranque) rellena vectores que faltan y consolida: se deja como en el servicio vivo
# en la fase 2, que es la que mide, y se aplaza en las demás para que lo que se compara fila a fila no se mueva.
REM_APLAZADO=CUBA_REM_FIRST_DELAY_SECS=86400

FALLAS=0
SEGUIR=1
LIMPIO=""
T=""
EH=""
P=""
HTTP=""
URL=""
TOKEN=""
SOCKDIR=""
PGPW=""
BIN_REAL=""
VOLCADO=""
CONSULTAS=""
DAEMON_PID=""
DAEMON_ERR=""
CLUSTER=0
BASE_CREADA=0
PARADAS_FORZADAS=0
RESTORE_ERRORES=0
RESTORE_AVISOS=0
RESTORE_DE_ROL=0
CON_VECTOR=-1
SIN_VECTOR=-1
N_MUESTRAS=0
TAG_NUEVA=""
HUELLA_ANTES=""
LS_ANTES=""
VIVO_ANTES=""
INICIO="$(date +%s)"
declare -a RES=()

ok() { printf '[OK] fase %s: %s\n' "$1" "$2"; }
falla() {
  printf '[FALLA] fase %s: %s\n' "$1" "$2"
  FALLAS=$((FALLAS + 1))
}
nota() { printf '       %s\n' "$*"; }
anotar() { RES+=("$1"); }
guarda() {
  printf '[GUARDA] %s Nada más se toca.\n' "$1" >&2
  exit 2
}
# comprobar FASE CONDICIÓN(0|1) TEXTO
comprobar() {
  if (($2)); then ok "$1" "$3"; else falla "$1" "$3"; fi
}
py() { Q7_TOKEN="$TOKEN" python3 -I "$AQUI/faro.py" "$@"; }
campo() { py campo "$1" "$2"; }
entero() { [[ "${1:-}" =~ ^[0-9]+$ ]]; }
# Un número que falta vale -1: ninguna comprobación lo da por bueno y ninguna cuenta se rompe.
o_menos_uno() {
  if entero "${1:-}"; then printf '%s' "$1"; else printf '%s' -1; fi
}
num() { o_menos_uno "$(campo "$1" "$2")"; }

# --- lo que el ensayo usa del juez -------------------------------------------------------------------------------
# El PATH sin las rutas de Windows (gate-lock.sh lo hace al cargarse) y sin nada del llamante que mueva un carril.
unset TMPDIR DATABASE_URL GATE_DAG GATE_LANE GATE_SERVER_SYSTEM_ID
GATE_CLUSTER_ROOT="/tmp/mi-$LANE-gate-$(id -u)"
export GATE_CLUSTER_ROOT
[[ -f "$RAIZ/scripts/gate-cluster.sh" ]] || guarda "no encuentro $RAIZ/scripts/gate-cluster.sh."
# shellcheck source=/dev/null
source "$RAIZ/scripts/gate-cluster.sh" || guarda "scripts/gate-cluster.sh no cargó."

# --- guardas -----------------------------------------------------------------------------------------------------
# Ninguna URL de Postgres que el ensayo use lleva el puerto de la base viva, y todas son el socket del carril.
guarda_url() {
  local url="$1" tras_arroba puerto
  tras_arroba="${url##*@}"
  puerto="$(gate_url_port "$url")"
  [[ "$puerto" != "$GATE_LIVE_PORT" && "$puerto" != 5488 && "$tras_arroba" != *":$GATE_LIVE_PORT"* && "$tras_arroba" != *":5488"* ]] \
    || guarda "una URL del ensayo lleva el puerto $GATE_LIVE_PORT, que es la base viva."
  [[ "$url" == "postgresql://cuba:"* && "$tras_arroba" == "$(gate_encode_host "$GATE_CLUSTER_ROOT")%2F$LANE."*":5432/$BASE" ]] \
    || guarda "la URL del ensayo no es el socket del carril $LANE bajo $GATE_CLUSTER_ROOT con la base $BASE."
}

# El servidor al que se va a escribir dice ser el carril del juez, por socket y en el 5432 del socket.
guarda_servidor() {
  local nombre resto
  [[ -n "$SOCKDIR" && "$SOCKDIR" == "$GATE_CLUSTER_ROOT/$LANE."* && -S "$SOCKDIR/.s.PGSQL.5432" ]] \
    || guarda "no hay socket del carril $LANE bajo $GATE_CLUSTER_ROOT."
  nombre="$(pg psql -X -At -d postgres -c 'show cluster_name' 2>/dev/null)" || nombre=""
  [[ "$nombre" == "$GATE_SERVER_MARK-$LANE" ]] \
    || guarda "el servidor dice cluster_name='$nombre' y el ensayo solo escribe en '$GATE_SERVER_MARK-$LANE'."
  resto="$(pg psql -X -At -d postgres -c "select current_setting('port') || '|' || coalesce(host(inet_server_addr()), 'socket')" 2>/dev/null)" || resto=""
  [[ "$resto" == "5432|socket" && "$resto" != "$GATE_LIVE_PORT|"* ]] \
    || guarda "el servidor contesta '$resto' (puerto|vía) y el ensayo solo usa 5432|socket; el $GATE_LIVE_PORT es la base viva."
}

# El entorno con el que se lanza el binario: HOME y la instantánea dentro del temporal, un runtime que no es el del
# servicio vivo, un puerto que no es el suyo y la URL del carril.
guarda_entorno() {
  local ort="$1" reembed="$2"
  [[ "$T" == /tmp/q7-ensayo.* && -d "$T" && "$EH" == "$T/ensayo" && "$EH" != "$HOME" && "$EH" != "$HOME/"* ]] \
    || guarda "el HOME del binario ('$EH') no es el temporal del ensayo."
  [[ "$reembed" == "$EH/"* ]] || guarda "MEMORY_INDUSTRY_REEMBED_DIR ('$reembed') no está dentro del HOME del ensayo."
  [[ "$ort" == "$RUNTIME_NUEVO" || "$ort" == "$RUNTIME_VIEJO" ]] || guarda "ORT_DYLIB_PATH ('$ort') no es ninguno de los dos runtimes del ensayo."
  ORT_DYLIB_PATH="$ort" gate_refuse_the_users_runtime
  [[ "$P" =~ ^[0-9]+$ && "$P" != 8787 && "$P" != 8788 && "$P" != "$GATE_LIVE_PORT" ]] \
    || guarda "el puerto del daemon del ensayo ('$P') no vale: 8787 y 8788 son del servicio vivo."
  guarda_url "$URL"
}

# --- Postgres del carril -----------------------------------------------------------------------------------------
# pg HERRAMIENTA ARGS: una herramienta del bindir del juez contra el socket del carril, sin PG* del llamante ni pgpass.
pg() {
  local herramienta="$1"
  shift
  env "${GATE_PG_ENV_UNSET[@]}" PGPASSFILE=/dev/null PGCONNECT_TIMEOUT=10 PGPASSWORD="$PGPW" \
    "$GATE_PG_BINDIR/$herramienta" -h "$SOCKDIR" -p 5432 -U cuba "$@"
}
# sql [-v var=valor]: una consulta por la entrada estándar contra la base del ensayo.
sql() { pg psql -X -At -q -v ON_ERROR_STOP=1 -d "$BASE" "$@"; }

# tabla|etiqueta|filas de las filas con vector: solo la etiqueta del modelo y su recuento.
etiquetas() {
  sql <<'SQL'
select t, coalesce(m, '(sin etiqueta)'), count(*) from (
  select 'brain_observations' t, embedding_model m from brain_observations where embedding is not null
  union all select 'brain_episodes', embedding_model from brain_episodes where embedding is not null
  union all select 'brain_observation_chunks', embedding_model from brain_observation_chunks where embedding is not null
  union all select 'brain_procedures', embedding_model from brain_procedures where embedding is not null
) s group by 1, 2 order by 1, 2;
SQL
}
sin_vector() {
  sql <<'SQL'
select (select count(*) from brain_observations where embedding is null)
     + (select count(*) from brain_episodes where embedding is null)
     + (select count(*) from brain_observation_chunks where embedding is null)
     + (select count(*) from brain_procedures where embedding is null);
SQL
}
# Las filas con vector de otra etiqueta que TAG, con los mismos joins que el reembebido usa para contar y leer.
de_otra_etiqueta() {
  sql -v tag="$1" <<'SQL'
select (select count(*) from brain_observations o join brain_entities e on e.id = o.entity_id
         where o.embedding is not null and o.embedding_model is distinct from :'tag')
     + (select count(*) from brain_episodes ep join brain_entities e on e.id = ep.entity_id
         where ep.embedding is not null and ep.embedding_model is distinct from :'tag')
     + (select count(*) from brain_observation_chunks c join brain_observations o on o.id = c.observation_id
          join brain_entities e on e.id = o.entity_id
         where c.embedding is not null and c.embedding_model is distinct from :'tag')
     + (select count(*) from brain_procedures p
         where p.embedding is not null and p.embedding_model is distinct from :'tag');
SQL
}
# tabla|id|md5 del vector: sin ids, elige 20 filas repartidas por md5(id); con ids ({a,b,...}), esas mismas.
muestras() {
  sql -v ids="${1:-}" <<'SQL'
select t, id, md5(embedding::text) from (
  select 'brain_observations' t, id, embedding from brain_observations where embedding is not null
  union all select 'brain_episodes', id, embedding from brain_episodes where embedding is not null
  union all select 'brain_observation_chunks', id, embedding from brain_observation_chunks where embedding is not null
  union all select 'brain_procedures', id, embedding from brain_procedures where embedding is not null
) s
where :'ids' = '' or id = any (nullif(:'ids', '')::uuid[])
order by case when :'ids' = '' then md5(id::text) else t || id::text end
limit 20;
SQL
}
# Cuántas de las muestras de la fase 0 tienen ahora el mismo vector (md5 de su texto).
muestras_iguales() {
  local ids
  ids="{$(cut -d'|' -f2 "$T/muestras.f0" | paste -sd,)}"
  muestras "$ids" | sort >"$T/muestras.ahora" || return 1
  comm -12 <(sort "$T/muestras.f0") "$T/muestras.ahora" | wc -l
}

# Crea la base del ensayo y restaura el volcado. Deja RESTORE_ERRORES y RESTORE_DE_ROL. Devuelve 1 si no quedó.
restaurar() {
  local rc=0
  guarda_servidor
  guarda_url "$URL"
  pg dropdb --if-exists --force "$BASE" >/dev/null 2>&1
  pg createdb "$BASE" >/dev/null 2>&1 || return 1
  BASE_CREADA=1
  # Las concesiones y las políticas del volcado nombran al rol de la aplicación; el carril es nuevo y no lo tiene.
  pg psql -X -q -d "$BASE" -c "do \$\$ begin if not exists (select from pg_roles where rolname = 'cuba_app') then create role cuba_app nologin; end if; end \$\$" >/dev/null 2>&1
  pg pg_restore --no-owner --jobs 4 -d "$BASE" "$VOLCADO" >"$T/restore.out" 2>"$T/restore.err" || rc=$?
  # El stderr de pg_restore puede citar una línea de COPY: se cuenta, no se imprime.
  RESTORE_ERRORES="$(grep -c 'pg_restore: error:' "$T/restore.err" || true)"
  RESTORE_AVISOS="$(grep -c 'pg_restore: warning:' "$T/restore.err" || true)"
  RESTORE_DE_ROL="$(grep -c 'role "[^"]*" does not exist' "$T/restore.err" || true)"
  RESTORE_ERRORES="$(o_menos_uno "$RESTORE_ERRORES")"
  RESTORE_AVISOS="$(o_menos_uno "$RESTORE_AVISOS")"
  RESTORE_DE_ROL="$(o_menos_uno "$RESTORE_DE_ROL")"
  # Un pg_restore que sale distinto de 0 solo se acepta si todos sus errores son de un rol que el carril no tiene.
  if ((rc != 0)) && ((RESTORE_ERRORES <= 0 || RESTORE_ERRORES > RESTORE_DE_ROL)); then
    return 1
  fi
}

# --- el binario --------------------------------------------------------------------------------------------------
# Dentro de un subshell: borra TODO el entorno heredado, deja solo el del ensayo y ejecuta el binario. Así el token
# y la URL no pasan por los argumentos de nadie, y nada del llamante (PG*, CUBA_*, XDG_*, DBUS) llega al binario.
#   exec_binario ORT REEMBED [VAR=valor ...] ARGS...
exec_binario() {
  local ort="$1" reembed="$2" v
  local -a extras=()
  shift 2
  while [[ "${1:-}" == *=* ]]; do
    extras+=("$1")
    shift
  done
  while read -r v; do unset "$v" 2>/dev/null || true; done < <(compgen -e)
  export PATH=/usr/bin:/bin HOME="$EH" LANG=C.UTF-8 \
    DATABASE_URL="$URL" MEMORY_INDUSTRY_REQUIRE_DATABASE_URL=1 \
    ONNX_MODEL_PATH="$CACHE/models" CUBA_RERANKER_PATH="$CACHE/reranker" CUBA_NLI_PATH="$CACHE/models-nli" \
    ORT_DYLIB_PATH="$ort" ORT_DISABLE_TELEMETRY=1 MEMORY_INDUSTRY_REEMBED_DIR="$reembed" \
    CUBA_HTTP_TOKEN="$TOKEN" CUBA_HTTP_ADDR="127.0.0.1:$P"
  for v in ${extras[@]+"${extras[@]}"}; do export "${v?}"; done
  cd "$EH" || exit 97
  exec "$BIN_REAL" "$@"
}

# lanzar NOMBRE ORT REEMBED [VAR=valor ...]: el daemon del ensayo, y espera a que /health conteste.
lanzar() {
  local nombre="$1" ort="$2" reembed="$3" rc=0
  shift 3
  guarda_entorno "$ort" "$reembed"
  [[ -z "$DAEMON_PID" ]] || guarda "ya hay un daemon del ensayo vivo (pid $DAEMON_PID)."
  DAEMON_ERR="$T/daemon-$nombre.err"
  (exec_binario "$ort" "$reembed" "$@" serve "127.0.0.1:$P") </dev/null >"$T/daemon-$nombre.out" 2>"$DAEMON_ERR" &
  DAEMON_PID=$!
  py salud --url "$HTTP" --tope "$Q7_ARRANQUE_S" --pid "$DAEMON_PID" >"$T/salud-$nombre.json" || rc=$?
  if ((rc != 0)); then
    nota "el daemon '$nombre' no contestó /health (motivo $rc: 3 tope de ${Q7_ARRANQUE_S}s, 4 murió); su registro: $(py registro "$DAEMON_ERR")"
    parar_daemon
    return 1
  fi
}

# correr NOMBRE ORT REEMBED ARGS...: una orden del binario sin daemon, con el mismo entorno. Deja $T/NOMBRE.out|.err.
correr() {
  local nombre="$1" ort="$2" reembed="$3"
  shift 3
  guarda_entorno "$ort" "$reembed"
  [[ -z "$DAEMON_PID" ]] || guarda "esta orden va sin daemon y hay uno vivo (pid $DAEMON_PID)."
  (exec_binario "$ort" "$reembed" "$@") </dev/null >"$T/$nombre.out" 2>"$T/$nombre.err"
}

es_mi_daemon() { [[ -n "$BIN_REAL" && "$(readlink -f "/proc/$1/exe" 2>/dev/null)" == "$BIN_REAL" ]]; }

# TERM al daemon del ensayo (solo si su exe es el binario del ensayo), hasta 60 s, y KILL si no se va.
parar_daemon() {
  local pid="$DAEMON_PID" i
  [[ -n "$pid" ]] || return 0
  DAEMON_PID=""
  if es_mi_daemon "$pid"; then
    kill -TERM "$pid" 2>/dev/null || true
    for ((i = 0; i < 600; i++)); do
      gate_pid_alive "$pid" || break
      sleep 0.1
    done
    if gate_pid_alive "$pid" && es_mi_daemon "$pid"; then
      kill -KILL "$pid" 2>/dev/null || true
      PARADAS_FORZADAS=$((PARADAS_FORZADAS + 1))
    fi
  fi
  wait "$pid" 2>/dev/null || true
}

# Los `serve` del binario instalado (el servicio vivo), como pid:arranque. Solo se leen de /proc: sirve para ver que
# son los mismos antes y después.
servicio_vivo() {
  local p exe
  for p in /proc/[0-9]*; do
    exe="$(readlink -f "$p/exe" 2>/dev/null)" || continue
    [[ "${exe##*/}" == memory-industry* || "${exe##*/}" == cuba-memorys* ]] || continue
    [[ "$exe" != "$BIN_REAL" ]] || continue
    grep -qa serve "$p/cmdline" 2>/dev/null || continue
    printf '%s:%s\n' "${p##*/}" "$(proc_start "${p##*/}")"
  done | sort
}

# La caché del servicio vivo: el runtime por contenido, el directorio por nombres y los modelos por tamaño y fecha.
huella() {
  {
    sha256sum "$CACHE"/onnxruntime/* 2>&1
    ls -A "$CACHE" 2>&1
    find "$CACHE/models" "$CACHE/reranker" "$CACHE/models-nli" -type f -printf '%p %s %T@\n' 2>&1 | sort
  } | sha256sum | cut -d' ' -f1
}

limpiar() {
  [[ -z "$LIMPIO" ]] || return 0
  LIMPIO=1
  parar_daemon
  if ((BASE_CREADA)) && [[ "$SOCKDIR" == "$GATE_CLUSTER_ROOT/$LANE."* ]]; then pg dropdb --if-exists --force "$BASE" >/dev/null 2>&1 || true; fi
  if ((CLUSTER)); then gate_cluster_down_all >/dev/null 2>&1 || true; fi
  rmdir "$GATE_CLUSTER_ROOT" 2>/dev/null || true
  if [[ -n "$T" && "$T" == /tmp/q7-ensayo.* && -d "$T" ]]; then rm -rf -- "$T"; fi
}
trap limpiar EXIT
trap 'exit 130' INT TERM

# --- fase 0: guardas y restauración ------------------------------------------------------------------------------
fase0() {
  local h version libre n
  for h in python3 sha256sum comm paste find; do
    command -v "$h" >/dev/null 2>&1 || guarda "falta $h en el PATH."
  done
  for h in psql pg_restore createdb dropdb; do
    [[ -x "$GATE_PG_BINDIR/$h" ]] || guarda "falta $GATE_PG_BINDIR/$h."
  done
  [[ -f "$AQUI/faro.py" ]] || guarda "falta $AQUI/faro.py."

  [[ -n "${Q7_BIN:-}" && -f "$Q7_BIN" && -x "$Q7_BIN" ]] || guarda "Q7_BIN tiene que ser el binario release 0.31 (este guion no compila)."
  BIN_REAL="$(readlink -f "$Q7_BIN")"
  [[ "$BIN_REAL" != "$(readlink -f "$HOME/.local/bin/memory-industry" 2>/dev/null)" && "$BIN_REAL" != "$HOME/.local/bin/"* ]] \
    || guarda "Q7_BIN es el binario instalado del servicio vivo."
  for h in /proc/[0-9]*; do
    [[ "$(readlink -f "$h/exe" 2>/dev/null)" != "$BIN_REAL" ]] || guarda "ya corre un proceso de $BIN_REAL (pid ${h##*/}): no sabría cuál es el del ensayo."
  done
  [[ "$Q7_TOPE_S" =~ ^[0-9]+$ && "$Q7_ARRANQUE_S" =~ ^[0-9]+$ ]] || guarda "Q7_TOPE_S y Q7_ARRANQUE_S son segundos enteros."

  T="$(mktemp -d /tmp/q7-ensayo.XXXXXX)" || T=""
  [[ -n "$T" && -d "$T" ]] || guarda "mktemp no pudo crear el temporal."
  chmod 700 "$T"
  EH="$T/ensayo"
  mkdir -p "$EH"
  export GATE_CLUSTER_LOGS="$T/clusters"

  version="$(env -i PATH=/usr/bin:/bin HOME="$EH" "$BIN_REAL" --version 2>/dev/null)" || version=""
  [[ "$version" == "memory-industry 0.31."* ]] || guarda "Q7_BIN dice '$version' y el ensayo es de la 0.31."

  for h in "$RUNTIME_NUEVO" "$RUNTIME_VIEJO"; do
    [[ -f "$h" ]] || guarda "no está el runtime $h."
    ORT_DYLIB_PATH="$h" gate_refuse_the_users_runtime
  done
  grep -q "\"version\":[[:space:]]*\"$VERSION_NUEVA\"" "${RUNTIME_NUEVO%/*}/runtime.json" 2>/dev/null \
    || guarda "el runtime del juez (${RUNTIME_NUEVO%/*}/runtime.json) no dice $VERSION_NUEVA."
  [[ -f "$RUNTIME_VIEJO.$VERSION_VIEJA" ]] || guarda "junto a $RUNTIME_VIEJO no está la copia .$VERSION_VIEJA."
  [[ -f "$CACHE/models/model_quantized.onnx" && -d "$CACHE/reranker" && -d "$CACHE/models-nli" ]] \
    || guarda "faltan los modelos en $CACHE (models/model_quantized.onnx, reranker, models-nli); solo se leen."

  VOLCADO="$(find "$HOME/respaldos-memoria" -maxdepth 1 -type f -name 'brain_*.dump' 2>/dev/null | sort | tail -n 1)"
  [[ -n "$VOLCADO" && -r "$VOLCADO" ]] || guarda "no hay ningún $HOME/respaldos-memoria/brain_*.dump."

  CONSULTAS="$T/consultas.txt"
  if [[ -n "${Q7_CONSULTAS:-}" ]]; then
    [[ -r "$Q7_CONSULTAS" ]] || guarda "Q7_CONSULTAS ('$Q7_CONSULTAS') no se puede leer."
    grep -v '^[[:space:]]*$' "$Q7_CONSULTAS" >"$CONSULTAS"
  else
    printf '%s\n' \
      'candado pesado y turno de la máquina compartida' \
      'Postgres del juez con la zona horaria de planta' \
      'reembebido automático al cambiar de ONNX Runtime' \
      'validar-handoff del implementador en Swarm Forge' \
      'disco con 500 GB libres y cuidar-disco' >"$CONSULTAS"
  fi
  [[ "$(wc -l <"$CONSULTAS")" == 5 ]] || guarda "hacen falta cinco consultas (una por línea) y hay $(wc -l <"$CONSULTAS")."

  TOKEN="$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  [[ ${#TOKEN} == 48 ]] || guarda "no pude generar el token del ensayo."

  HUELLA_ANTES="$(huella)"
  LS_ANTES="$(ls -A "$CACHE" 2>&1)"
  VIVO_ANTES="$(servicio_vivo)"
  ok 0 "guardas previas: binario $version, runtimes $VERSION_NUEVA y $VERSION_VIEJA fuera de la caché del servicio, modelos solo lectura"
  nota "caché del servicio antes: huella ${HUELLA_ANTES:0:16}, $(wc -l <<<"$LS_ANTES") entradas; procesos serve del servicio vivo: $(grep -c . <<<"$VIVO_ANTES" || true)"

  if [[ -f "$VOLCADO.sha256" ]]; then
    n=0
    [[ "$(sha256sum "$VOLCADO" | cut -d' ' -f1)" == "$(cut -d' ' -f1 "$VOLCADO.sha256")" ]] && n=1
    comprobar 0 "$n" "el volcado ${VOLCADO##*/} ($(stat -c %s "$VOLCADO") bytes) coincide con su .sha256"
  else
    nota "el volcado ${VOLCADO##*/} no tiene .sha256 al lado: no se comprobó"
  fi
  anotar "volcado_bytes=$(stat -c %s "$VOLCADO")"

  # shellcheck disable=SC2034 # lo lee gate-cluster.sh: es quien dice de quién es el carril
  GATE_OWNER="$(owner_record "$$" "q7-$$")"
  CLUSTER=1
  # Si el carril no levanta, gate-cluster.sh lo dice (sin la contraseña) y sale 2 por su cuenta.
  gate_cluster_up "$LANE"
  SOCKDIR="${GATE_CLUSTER_DIR[$LANE]}"
  PGPW="$(<"$SOCKDIR/pw")"
  URL="${GATE_CLUSTER_URL%/*}/$BASE"
  guarda_url "$URL"
  guarda_servidor
  GATE_LANE="$LANE" gate_require_judge_server "$URL" >/dev/null
  ok 0 "servidor desechable: cluster_name=$GATE_SERVER_MARK-$LANE por socket bajo $GATE_CLUSTER_ROOT (nunca $GATE_LIVE_PORT)"

  nota "volcado: $("$GATE_PG_BINDIR/pg_restore" -l "$VOLCADO" 2>/dev/null | sed -n 's/^; *Dumped from database version: *//p' | head -n 1); servidor: $(pg psql -X -At -d postgres -c 'show server_version' 2>/dev/null)"
  restaurar || {
    falla 0 "pg_restore --no-owner no dejó la base (errores $RESTORE_ERRORES, de rol $RESTORE_DE_ROL)"
    return 1
  }
  ok 0 "pg_restore --no-owner: $RESTORE_ERRORES errores ($RESTORE_DE_ROL por un rol que el carril no tiene), $RESTORE_AVISOS avisos"
  anotar "restore_errores=$RESTORE_ERRORES restore_errores_de_rol=$RESTORE_DE_ROL restore_avisos=$RESTORE_AVISOS"

  etiquetas >"$T/etiquetas.f0" || {
    falla 0 "no pude contar los vectores de la base restaurada"
    return 1
  }
  CON_VECTOR="$(awk -F'|' '{s += $3} END {print s + 0}' "$T/etiquetas.f0")"
  SIN_VECTOR="$(o_menos_uno "$(sin_vector)")"
  nota "filas con vector, por tabla y etiqueta:"
  while IFS='|' read -r h version n; do nota "  $h · $version · $n"; done <"$T/etiquetas.f0"
  nota "filas sin vector (las cuatro tablas): $SIN_VECTOR"
  comprobar 0 "$((CON_VECTOR > 0))" "la base restaurada tiene $CON_VECTOR filas con vector"
  anotar "filas_con_vector=$CON_VECTOR filas_sin_vector=$SIN_VECTOR etiquetas_distintas=$(cut -d'|' -f2 "$T/etiquetas.f0" | sort -u | wc -l)"

  muestras | sort >"$T/muestras.f0"
  N_MUESTRAS="$(wc -l <"$T/muestras.f0")"
  comprobar 0 "$((N_MUESTRAS > 0))" "$N_MUESTRAS vectores de muestra fijados por id (md5 guardado en el temporal)"

  libre="$(py puerto)" || libre=""
  P="$libre"
  HTTP="http://127.0.0.1:$P"
  guarda_entorno "$RUNTIME_NUEVO" "$EH/reembed"
  ((CON_VECTOR > 0 && N_MUESTRAS > 0))
}

# --- fase 1: línea base, sin el automático -----------------------------------------------------------------------
fase1() {
  local estado n
  lanzar base "$RUNTIME_NUEVO" "$EH/reembed-f1" MEMORY_INDUSTRY_AUTO_REEMBED=0 "$REM_APLAZADO" || {
    falla 1 "el daemon de la línea base no arrancó"
    return 1
  }
  nota "daemon $(campo "$T/salud-base.json" version), listo=$(campo "$T/salud-base.json" ready)"
  py base --url "$HTTP" --consultas "$CONSULTAS" --llamadas 20 --salida "$T/base.json" || {
    falla 1 "las 20 llamadas memory_faro de la línea base no se pudieron hacer"
    parar_daemon
    return 1
  }
  py salud --url "$HTTP" --tope 30 --pid "$DAEMON_PID" >"$T/salud-base-fin.json" || true
  estado="$(campo "$T/salud-base-fin.json" state)"
  parar_daemon
  n="$(num "$T/base.json" fallos)"
  comprobar 1 "$((n == 0))" "línea base: $(campo "$T/base.json" llamadas) llamadas memory_faro, $n falladas, mediana $(campo "$T/base.json" mediana_ms) ms, p95 $(campo "$T/base.json" p95_ms) ms"
  n="$(num "$T/base.json" sin_resultados)"
  comprobar 1 "$((n == 0))" "las cinco consultas devuelven resultados ($n vacías; una vacía no compara nada: cambiar Q7_CONSULTAS)"
  [[ "$estado" == off ]] && n=1 || n=0
  comprobar 1 "$n" "con MEMORY_INDUSTRY_AUTO_REEMBED=0 /health dice runtime.vectors.auto.state=off ('${estado:-nada}', etiqueta $(campo "$T/salud-base-fin.json" tag))"
  [[ -z "$(find "$EH/reembed-f1" -type f 2>/dev/null | head -n 1)" ]] && cmp -s <(etiquetas) "$T/etiquetas.f0" && n=1 || n=0
  comprobar 1 "$n" "apagado no reembebe: ninguna instantánea y las etiquetas de la base iguales que al restaurar"
  anotar "base_llamadas=$(campo "$T/base.json" llamadas) base_fallos=$(campo "$T/base.json" fallos) base_mediana_ms=$(campo "$T/base.json" mediana_ms) base_p95_ms=$(campo "$T/base.json" p95_ms) base_degradadas=$(campo "$T/base.json" degradadas)"
}

# --- fase 2: con el automático -----------------------------------------------------------------------------------
fase2() {
  local rc=0 estado older rewritten failed seg fallos pool bytes quedan tag
  lanzar auto "$RUNTIME_NUEVO" "$EH/reembed" || {
    falla 2 "el daemon con el reembebido automático no arrancó"
    return 1
  }
  py vigilar --url "$HTTP" --consultas "$CONSULTAS" --tope "$Q7_TOPE_S" --cada-salud 5 --cada-faro 2 \
    --pid "$DAEMON_PID" --salida "$T/auto.json" || rc=$?
  parar_daemon
  estado="$(campo "$T/auto.json" state)"
  older="$(num "$T/auto.json" older_inicial)"
  rewritten="$(num "$T/auto.json" rewritten)"
  failed="$(num "$T/auto.json" failed)"
  seg="$(campo "$T/auto.json" segundos_servicio)"
  tag="$(campo "$T/auto.json" tag)"
  if ((rc == 0)) && [[ "$estado" == "done" ]]; then
    ok 2 "runtime.vectors.auto.state=done: older=$older, rewritten=$rewritten, changed=$(campo "$T/auto.json" changed), failed=$failed, ahead=$(campo "$T/auto.json" ahead)"
  else
    falla 2 "el automático no llegó a done (motivo $rc: 3 tope de ${Q7_TOPE_S}s, 4 el daemon murió): último estado '${estado:-ninguno}', encoded=$(campo "$T/auto.json" encoded) de older=$older, con_error=$(campo "$T/auto.json" con_error) $(campo "$T/auto.json" last_error)"
  fi
  comprobar 2 "$((older > 0))" "había algo que reembeber: older inicial $older (con vector al restaurar: $CON_VECTOR)"
  comprobar 2 "$((failed == 0))" "ninguna fila falló en el codificador (failed=$failed)"
  fallos="$(num "$T/auto.json" fallos)"
  comprobar 2 "$((fallos == 0))" "memory_faro cada 2 s durante el reembebido: $(campo "$T/auto.json" llamadas) llamadas, $fallos falladas, $(campo "$T/auto.json" degradadas) degradadas, mediana $(campo "$T/auto.json" mediana_ms) ms, p95 $(campo "$T/auto.json" p95_ms) ms (base: $(campo "$T/base.json" mediana_ms) / $(campo "$T/base.json" p95_ms))"
  py registro "$DAEMON_ERR" >"$T/registro-auto.json"
  pool="$(num "$T/registro-auto.json" pool_timed_out)"
  comprobar 2 "$((pool == 0))" "stderr del daemon: $pool líneas con 'pool timed out' ($(campo "$T/registro-auto.json" errores) de nivel ERROR, $(campo "$T/registro-auto.json" avisos) WARN)"
  bytes="$(du -sb "$EH/reembed" 2>/dev/null | cut -f1)"
  py partes "$EH/reembed" >"$T/partes-auto.json"
  quedan=""
  [[ -n "$tag" ]] && quedan="$(de_otra_etiqueta "$tag")"
  nota "instantánea: ${bytes:-0} bytes en $(campo "$T/partes-auto.json" partes) partes, $(campo "$T/partes-auto.json" filas) filas; quedan ${quedan:-?} vectores de otra etiqueta que la nueva"
  nota "top-10 tras el reembebido frente a la base (informativo: aquí corrió el ciclo REM):"
  py comparar "$T/base.json" "$T/auto.json" | sed 's/^/         /' || true
  anotar "auto_estado=${estado:-ninguno} auto_older=$older auto_ahead=$(campo "$T/auto.json" ahead) auto_rewritten=$rewritten auto_changed=$(campo "$T/auto.json" changed) auto_failed=$failed"
  anotar "auto_D_s=${seg:-} auto_reloj_s=$(campo "$T/auto.json" segundos_reloj) $(awk -v n="$rewritten" -v s="${seg:-0}" 'BEGIN { if (n > 0 && s > 0) printf "auto_filas_por_s=%.1f auto_ms_por_fila=%.1f auto_min_por_10000=%.1f", n / s, 1000 * s / n, s / n * 10000 / 60; else printf "auto_filas_por_s= auto_ms_por_fila= auto_min_por_10000=" }')"
  anotar "auto_M_bytes=${bytes:-0} auto_M_MB=$(awk -v b="${bytes:-0}" 'BEGIN { printf "%.1f", b / 1048576 }') auto_partes=$(campo "$T/partes-auto.json" partes) auto_quedan=${quedan:-}"
  anotar "auto_faro_llamadas=$(campo "$T/auto.json" llamadas) auto_faro_fallos=$fallos auto_faro_degradadas=$(campo "$T/auto.json" degradadas) auto_faro_mediana_ms=$(campo "$T/auto.json" mediana_ms) auto_faro_p95_ms=$(campo "$T/auto.json" p95_ms) auto_pool_timed_out=$pool"
  anotar "$(awk -v m="$(campo "$T/auto.json" mediana_ms)" -v p="$(campo "$T/auto.json" p95_ms)" -v bm="$(campo "$T/base.json" mediana_ms)" -v bp="$(campo "$T/base.json" p95_ms)" 'BEGIN { if (bm > 0 && bp > 0) printf "auto_mediana_sobre_base=%.2f auto_p95_sobre_base=%.2f", m / bm, p / bp; else printf "auto_mediana_sobre_base= auto_p95_sobre_base=" }')"
  return 0
}

# --- fase 3: reinicio a mitad ------------------------------------------------------------------------------------
fase3() {
  local rc=0 older1 encoded tag ahead faltan esperado older2 estado t0 parada n iguales forzadas="$PARADAS_FORZADAS"
  restaurar || {
    falla 3 "la segunda restauración no dejó la base (errores $RESTORE_ERRORES, de rol $RESTORE_DE_ROL)"
    return 1
  }
  iguales="$(muestras_iguales)"
  cmp -s <(etiquetas) "$T/etiquetas.f0" && [[ "$iguales" == "$N_MUESTRAS" ]] && n=1 || n=0
  comprobar 3 "$n" "restaurada de nuevo: mismas etiquetas y $iguales de $N_MUESTRAS vectores de muestra iguales que en la fase 0"
  ((n)) || return 1

  lanzar corte "$RUNTIME_NUEVO" "$EH/reembed-f3" "$REM_APLAZADO" || {
    falla 3 "el daemon no arrancó tras restaurar"
    return 1
  }
  py vigilar --url "$HTTP" --consultas "$CONSULTAS" --tope "$Q7_TOPE_S" --cada-salud 1 --cada-faro 2 --mitad \
    --pid "$DAEMON_PID" --salida "$T/mitad.json" || rc=$?
  older1="$(num "$T/mitad.json" older)"
  encoded="$(num "$T/mitad.json" encoded)"
  tag="$(campo "$T/mitad.json" tag)"
  ahead="$(num "$T/mitad.json" ahead)"
  if ((rc != 0)); then
    falla 3 "no se pudo cortar a mitad (motivo $rc: 3 tope, 4 murió, 5 terminó antes de la mitad): estado '$(campo "$T/mitad.json" state)', encoded=$encoded de older=$older1"
    parar_daemon
    return 1
  fi
  t0="$(date +%s%3N)"
  parar_daemon
  parada="$(($(date +%s%3N) - t0))"
  comprobar 3 "$((PARADAS_FORZADAS == forzadas))" "SIGTERM con encoded=$encoded de older=$older1: el daemon salió solo en $parada ms (sin KILL)"
  faltan="$(o_menos_uno "$(de_otra_etiqueta "$tag")")"
  esperado=-1
  ((faltan >= 0 && ahead >= 0)) && esperado="$((faltan - ahead))"
  py partes "$EH/reembed-f3" >"$T/partes-corte.json"
  nota "tras el corte: quedan $esperado anteriores en la base; instantánea con $(campo "$T/partes-corte.json" filas) filas en $(campo "$T/partes-corte.json" partes) partes, $(campo "$T/partes-corte.json" a_medias) a medias"
  comprobar 3 "$((esperado > 0 && esperado < older1))" "el corte dejó trabajo hecho y trabajo por hacer ($esperado de $older1)"

  rc=0
  lanzar reanuda "$RUNTIME_NUEVO" "$EH/reembed-f3" "$REM_APLAZADO" || {
    falla 3 "el daemon no arrancó tras el corte"
    return 1
  }
  py vigilar --url "$HTTP" --consultas "$CONSULTAS" --tope "$Q7_TOPE_S" --cada-salud 2 --cada-faro 2 \
    --pid "$DAEMON_PID" --salida "$T/reanuda.json" || rc=$?
  parar_daemon
  estado="$(campo "$T/reanuda.json" state)"
  older2="$(num "$T/reanuda.json" older)"
  comprobar 3 "$((esperado > 0 && older2 == esperado))" "reanuda: older del segundo arranque ($older2) = lo que faltaba ($esperado)"
  if ((rc == 0)) && [[ "$estado" == "done" ]]; then
    ok 3 "el segundo arranque llegó a done: rewritten=$(campo "$T/reanuda.json" rewritten), changed=$(campo "$T/reanuda.json" changed), failed=$(campo "$T/reanuda.json" failed)"
  else
    falla 3 "el segundo arranque no llegó a done (motivo $rc): último estado '${estado:-ninguno}', encoded=$(campo "$T/reanuda.json" encoded) de older=$older2, con_error=$(campo "$T/reanuda.json" con_error) $(campo "$T/reanuda.json" last_error)"
  fi
  py partes "$EH/reembed-f3" >"$T/partes-fin.json"
  [[ "$(campo "$T/partes-fin.json" nodos)" == 1 && "$(campo "$T/partes-fin.json" instantaneas)" == 1 && "$(campo "$T/partes-fin.json" destinos)" == 1 ]] && n=1 || n=0
  comprobar 3 "$n" "un solo directorio de instantánea ($(campo "$T/partes-fin.json" instantaneas)) de una sola generación"
  comprobar 3 "$(($(num "$T/partes-fin.json" repetidas) == 0 && $(num "$T/partes-fin.json" a_medias) == 0 && $(num "$T/partes-fin.json" partes) > 0))" "sus $(campo "$T/partes-fin.json" partes) partes no repiten filas: $(campo "$T/partes-fin.json" filas) filas, $(campo "$T/partes-fin.json" unicas) ids únicos, $(campo "$T/partes-fin.json" a_medias) partes a medias"
  comprobar 3 "$(($(num "$T/partes-fin.json" unicas) == older1))" "la instantánea guarda los $older1 vectores anteriores ($(campo "$T/partes-fin.json" unicas))"
  faltan="$(o_menos_uno "$(de_otra_etiqueta "$tag")")"
  comprobar 3 "$((faltan >= 0 && faltan - ahead == 0))" "no queda ningún vector anterior en la base (de otra etiqueta: $faltan; adelantados: $ahead)"
  n="$(($(num "$T/mitad.json" fallos) + $(num "$T/reanuda.json" fallos)))"
  comprobar 3 "$((n == 0))" "memory_faro durante el corte y la reanudación: $n llamadas falladas (un número negativo es un recuento que falta)"
  anotar "corte_older_1=$older1 corte_encoded=$encoded corte_parada_ms=$parada corte_faltaban=$esperado corte_older_2=$older2 corte_estado=${estado:-ninguno}"
  anotar "corte_instantaneas=$(campo "$T/partes-fin.json" instantaneas) corte_partes=$(campo "$T/partes-fin.json" partes) corte_filas=$(campo "$T/partes-fin.json" filas) corte_repetidas=$(campo "$T/partes-fin.json" repetidas) corte_faro_fallos=$n"
  TAG_NUEVA="$tag"
  [[ "$estado" == "done" ]]
}

# --- fase 4: doctor sin daemon y las cinco consultas -------------------------------------------------------------
fase4() {
  local rc=0 estado stale missing tag n
  correr doctor4 "$RUNTIME_NUEVO" "$EH/reembed-f3" doctor --json || rc=$?
  py doctor "$T/doctor4.out" >"$T/doctor4.json" 2>/dev/null || echo '{}' >"$T/doctor4.json"
  estado="$(campo "$T/doctor4.json" estado)"
  stale="$(campo "$T/doctor4.json" stale)"
  missing="$(campo "$T/doctor4.json" missing)"
  tag="$(campo "$T/doctor4.json" tag)"
  nota "doctor salió $rc con $(campo "$T/doctor4.json" fallos_de_doctor) fallos y $(campo "$T/doctor4.json" avisos_de_doctor) avisos en total (HOME vacío: solo se juzga embedding_coverage)"
  # «ok» exige además que no falte ningún vector; los que el volcado ya traía sin vector no son del automático
  # (los rellena el ciclo REM), así que con ellos la fila es warn con 0 de otra generación.
  if [[ "$estado" == ok ]] || [[ "$estado" == warn && "$stale" == 0 && "${missing:-x}" == "$SIN_VECTOR" ]]; then n=1; else n=0; fi
  comprobar 4 "$n" "doctor embedding_coverage=$estado: ${stale:-?} de otra generación, ${missing:-?} sin vector (al restaurar: $SIN_VECTOR)"
  [[ "$tag" == *"@onnxruntime-$VERSION_NUEVA" && "$tag" == "$TAG_NUEVA" ]] && n=1 || n=0
  comprobar 4 "$n" "la generación que cuenta doctor es la de /health y acaba en @onnxruntime-$VERSION_NUEVA"
  rc=0
  py comparar "$T/base.json" "$T/reanuda.json" >"$T/comparar.txt" || rc=$?
  sed 's/^/       /' "$T/comparar.txt"
  comprobar 4 "$((rc == 0))" "las cinco consultas antes y después coinciden en al menos 8 de 10 ids"
  anotar "doctor_estado=${estado:-ninguno} doctor_stale=${stale:-} doctor_missing=${missing:-} $(sed -n 's/^consulta \([0-9]\): \([0-9]*\) de \([0-9]*\).*/top10_\1=\2\/\3/p' "$T/comparar.txt" | paste -sd' ')"
  return 0
}

# --- fase 5: vuelta atrás ----------------------------------------------------------------------------------------
fase5() {
  local rc=0 devueltos no estado stale tag esperado iguales n
  correr rollback "$RUNTIME_VIEJO" "$EH/reembed-f3" reembed --rollback || rc=$?
  devueltos="$(sed -n 's/^Devueltos \([0-9]*\) vectores anteriores; \([0-9]*\) no.*/\1/p' "$T/rollback.out")"
  no="$(sed -n 's/^Devueltos \([0-9]*\) vectores anteriores; \([0-9]*\) no.*/\2/p' "$T/rollback.out")"
  if ((rc == 0)) && entero "$devueltos"; then
    ok 5 "reembed --rollback con ORT_DYLIB_PATH $VERSION_VIEJA: devueltos $devueltos, no devueltos ${no:-?}"
  else
    falla 5 "reembed --rollback con ORT_DYLIB_PATH $VERSION_VIEJA salió $rc sin la línea «Devueltos»: $(grep -c . "$T/rollback.err" || true) líneas en su stderr, la última de la orden: $(grep '^reembed error:' "$T/rollback.err" | tail -n 1 | cut -c1-300)"
  fi
  rc=0
  correr doctor5 "$RUNTIME_VIEJO" "$EH/reembed-f3" doctor --json || rc=$?
  py doctor "$T/doctor5.out" >"$T/doctor5.json" 2>/dev/null || echo '{}' >"$T/doctor5.json"
  estado="$(campo "$T/doctor5.json" estado)"
  stale="$(campo "$T/doctor5.json" stale)"
  tag="$(campo "$T/doctor5.json" tag)"
  [[ "$tag" == *"@onnxruntime-$VERSION_VIEJA" ]] && n=1 || n=0
  comprobar 5 "$n" "doctor corre con el runtime $VERSION_VIEJA cargado (su etiqueta acaba en @onnxruntime-$VERSION_VIEJA)"
  # Lo que doctor tiene que volver a contar sale del recuento hecho al restaurar, no de la base de ahora.
  esperado="$(awk -F'|' -v tag="$tag" '$2 != tag {s += $3} END {print s + 0}' "$T/etiquetas.f0")"
  [[ "$estado" == warn && "$stale" == "$esperado" ]] && n=1 || n=0
  comprobar 5 "$n" "doctor vuelve a contar los anteriores: ${stale:-ninguno} de otra generación (al restaurar: $esperado)"
  cmp -s <(etiquetas) "$T/etiquetas.f0" && n=1 || n=0
  comprobar 5 "$n" "las etiquetas de la base, tabla por tabla, son otra vez las de la restauración"
  iguales="$(muestras_iguales)"
  comprobar 5 "$((${iguales:-0} == N_MUESTRAS))" "md5 de los vectores de muestra: ${iguales:-0} de $N_MUESTRAS iguales que al restaurar"
  anotar "vuelta_devueltos=${devueltos:-} vuelta_no_devueltos=${no:-} vuelta_doctor_stale=${stale:-} vuelta_esperado=$esperado vuelta_md5_iguales=${iguales:-0}/$N_MUESTRAS"
  return 0
}

# --- fase 6: limpieza y huellas ----------------------------------------------------------------------------------
fase6() {
  local huella_despues ls_despues vivo_despues procesos carril
  limpiar
  procesos=0
  [[ -n "$BIN_REAL" ]] && procesos="$(pgrep -f -- "$BIN_REAL serve 127.0.0.1:${P:-}" | wc -l)"
  carril="$(pgrep -f -- "-D $GATE_CLUSTER_ROOT/" | wc -l)"
  nota "pgrep '$BIN_REAL serve 127.0.0.1:${P:-}': $procesos; postgres del carril: $carril; temporal: $([[ -n "$T" && -e "$T" ]] && echo sigue || echo borrado); raíz del carril: $([[ -e "$GATE_CLUSTER_ROOT" ]] && echo sigue || echo borrada)"
  comprobar 6 "$((procesos == 0 && carril == 0))" "no queda ningún proceso del ensayo (KILL forzados en todo el ensayo: $PARADAS_FORZADAS)"
  if [[ -n "$T" && -e "$T" ]] || [[ -e "$GATE_CLUSTER_ROOT" ]]; then falla 6 "queda el temporal o la raíz del carril"; else ok 6 "base, carril y temporal borrados"; fi
  if [[ -n "${HUELLA_ANTES:-}" ]]; then
    huella_despues="$(huella)"
    ls_despues="$(ls -A "$CACHE" 2>&1)"
    vivo_despues="$(servicio_vivo)"
    comprobar 6 "$([[ "$huella_despues" == "$HUELLA_ANTES" && "$ls_despues" == "$LS_ANTES" ]] && echo 1 || echo 0)" "la caché del servicio vivo no cambió: huella ${huella_despues:0:16} (antes ${HUELLA_ANTES:0:16}), $(wc -l <<<"$ls_despues") entradas"
    comprobar 6 "$([[ "$vivo_despues" == "$VIVO_ANTES" ]] && echo 1 || echo 0)" "los procesos serve del servicio vivo son los mismos que al empezar ($(grep -c . <<<"$vivo_despues" || true))"
    anotar "limpieza_procesos=$((procesos + carril)) cache_igual=$([[ "$huella_despues" == "$HUELLA_ANTES" && "$ls_despues" == "$LS_ANTES" ]] && echo 1 || echo 0) servicio_vivo_igual=$([[ "$vivo_despues" == "$VIVO_ANTES" ]] && echo 1 || echo 0) kill_forzados=$PARADAS_FORZADAS"
  else
    falla 6 "no hay huella de antes con la que comparar"
  fi
}

for n in 0 1 2 3 4 5; do
  if ((SEGUIR)); then
    "fase$n" || SEGUIR=0
  else
    falla "$n" "no corrió: una fase anterior no dejó con qué"
  fi
done
fase6

echo
echo "RESULTADO Q7"
for linea in ${RES[@]+"${RES[@]}"}; do printf '  %s\n' "$linea"; done
printf '  fallas=%s segundos_totales=%s\n' "$FALLAS" "$(($(date +%s) - INICIO))"
((FALLAS == 0))
