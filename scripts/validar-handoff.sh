#!/usr/bin/env bash
# Valida un handoff Swarm Forge. Exit 0 = ok, 1 = malformado o protocolo roto.
#
# Hasta 0.28 esto validaba LA FORMA del YAML y nada mas. La regla a la que
# pertenece (swarm-forge.md) dice «El codigo, sin tocar los tests:
# validar-handoff compara contra el commit rojo», y no lo hacia: el campo
# `commit` solo tenia que PARECER hexadecimal, nadie comprobaba que existiera y
# nada miraba los tests. O sea que el protocolo de dos pasadas dependia de que
# el agente fuera honesto, no de un guardia. En esta misma rama se corrieron dos
# ciclos de dos pasadas (dcb97ad->94ff39c y d624f2f->...) y este fichero no
# sujeto ninguno.
#
# Lo que decide ahora, ademas de la forma:
#
#   commit  tiene que EXISTIR en este repo, no solo parecer un sha.
#   tests   `written` o `frozen`, y de dos sentidos como los techos del gate:
#             written -> falla si NINGUNA region de test cambio desde `commit`.
#                        Una pasada roja que no movio un test no escribio nada
#                        que pueda ponerse rojo.
#             frozen  -> falla si ALGUNA cambio. Es el ajuste que las dos
#                        pasadas existen para impedir.
#           Obligatorio cuando commit != none, prohibido cuando commit == none.
#           Omitir el campo NO es una salida: esa era justo la fuga.
#
# Por que no se comparan rutas de git: el crate tiene 112 bloques #[cfg(test)]
# dentro de ficheros de produccion (medido 2026-09-22; 106 seguidos de `mod X {`
# y 6 sobre un item suelto de apoyo), y cero ficheros tests.rs hermanos. «No
# tocaste los tests» no se puede contestar con nombres de fichero, que es
# exactamente por lo que este guardia no existia. Hay que extraer las regiones.
#
# Y un .sh tambien tiene region de test: la que cae entre las dos lineas
# marcador `# --- self-test begin ---` y `# --- self-test end ---`, en columna 0.
# Hasta 0.31 un .sh en `paths:` se aceptaba y se ignoraba: repetido sobre el
# historial de G7, un `--self-test` nuevo de 295 lineas declarado `frozen` daba
# OK, y la pasada roja honesta declarada `written` daba rojo. Cuatro razones
# medidas para marcar en vez de detectar por patron:
#   - en validar-handoff.sh y gpu-placement-check.sh los ayudantes del test
#     (must_fail, must_pass) viven FUERA de self_test(): con la funcion como
#     region, volver un ayudante `return 0` pasaria `frozen`;
#   - el despacho (`--self-test) self_test ;;`) esta en el `case` de produccion:
#     reescribirlo para que imprima el OK sin correr nada dejaba verde la fila
#     que solo ancla en la etiqueta;
#   - un `fi` o un `}` en columna 0 dentro de un heredoc corta una region
#     detectada por patron EN SILENCIO (mk_repo ya tiene dos `}` asi); con
#     marcadores el corte deja un fichero desparejado, y eso es rojo;
#   - son dos patrones que crecen con cada forma nueva, frente a una regla.
# Un marcador mal puesto es rojo sea cual sea `tests` (marcas_sh), porque una
# region adivinada no vale para decidir written ni frozen.
#
# CI-E6, el coste de lo que se entrega. Tampoco hay campo nuevo (E6a): el
# especificador dice nivel, segundos, carril y porque del nivel caro de cada
# escenario en una TABLA de su spec (son N filas, y no caben en una clave de una
# linea), y el implementador dice los segundos que midio y el carril en dos lineas
# sangradas de `evidence: |`, que ya es la linea de la orden que pega el
# orquestador. Un repo que juzga solo seis claves no gana una septima; gana lo que
# `evidence` ya llevaba. Se juzga despues de written y frozen (E6h), asi que los
# rojos de siempre no cambian de mensaje ni de orden.
#
# `.cursor/handoffs/*` no se versiona, y un handoff no tiene fecha ni su mtime
# sobrevive a una copia: la unica identidad que conserva un historico es su
# CONTENIDO. Por eso los anteriores a CI-E6 se eximen por el sha256 de su
# contenido (sin CR) en scripts/handoffs-sin-coste.txt (E6f). Por fecha no se
# puede; con un marcador de formato, omitirlo seria la salida (la misma fuga que
# `tests`); por nombre, reescribir un historico pasaria; con el hash, editarlo lo
# vuelve nuevo. La lista es cerrada (un contrato de local_gate_contract.rs fija
# cuantas lineas tiene) y cada linea caduca: toda excepcion lleva motivo, dueño
# y fecha. Que una linea «ya no se usa» no se puede comprobar, porque los
# handoffs no se versionan.
set -euo pipefail
# CI-K9a: the judge does not use the interop of Windows. On Linux the PATH that WSL adds (/mnt/..., by 9p) goes before anything looks for a command: each one that is not found walks all of it. The same filter as gate_path_without_windows (gate-lock.sh), which this script cannot load first.
if [[ "$OSTYPE" == linux* ]]; then k9_kept=""; IFS=: read -ra k9_dirs <<<"$PATH"; for k9_d in "${k9_dirs[@]}"; do [[ "$k9_d" == /mnt/* ]] || k9_kept="${k9_kept:+$k9_kept:}$k9_d"; done; PATH="$k9_kept"; unset k9_dirs k9_d k9_kept; fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"

roles='especificador|implementador|mejorador|arquitecto|endurecedor|qa'

usage() {
  cat <<'TXT'
uso:
  validar-handoff.sh <fichero.yml>   juzga un handoff
  validar-handoff.sh --self-test     prueba que cada guardia puede fallar

Campos: from, to, type, task, commit, evidence, y `tests` cuando commit != none.
Opcional con commit != none: `paths`, la lista de rutas de la tajada; `tests`
se juzga entonces solo sobre los ficheros bajo ellas.
Region de test: un `#[cfg(test)]` de un .rs, un .rs entero bajo tests/, o en un
.sh lo que hay entre las lineas `# --- self-test begin ---` y
`# --- self-test end ---` (columna 0).

Coste (CI-E6), sin campo nuevo:
  Del especificador al implementador (cualquier type): `task` o `evidence` nombra
  su `.cursor/handoffs/<nombre>.spec.md`, junto al .yml, y esa spec trae una tabla
  con esta cabecera y una fila por cada `Escenario:` de sus bloques gherkin:
    | Escenario | Nivel | Segundos | Carril | Por qué no el barato |
    |---|---|---|---|---|
  Nivel: pura|guion|servicio|e2e. Segundos: un numero (. o ,). Carril: una fila
  de scripts/gate-dag.txt. Con un nivel que no es `pura`, la ultima celda dice por
  que el barato no lo demuestra.
  Del implementador con git_handoff y commit: `evidence: |` lleva, cada una en su
  linea sangrada, una `segundos: <S> s; <orden acotada>[; <su ultima linea>]` por
  orden que corrio los tests nuevos (con el `finished in Xs` de cargo, X es S) y,
  si la tajada toca scripts/, `carril: <id>; comparte: <recursos o nada>`.
  Los anteriores a CI-E6 estan en scripts/handoffs-sin-coste.txt, por el sha256 de
  su contenido, con caducidad, dueño y motivo.
TXT
}

# ------------------------------------------------------- extraer las regiones

# Sin contar llaves. Contarlas obliga a saber si una `{` esta dentro de una
# cadena, y un format!("{") en un test tumbaria la cuenta EN SILENCIO: la misma
# clase de fallo que estamos quitando. Con la forma medida del crate basta una
# maquina de tres estados sobre lineas en columna 0.
regiones_cfg_test() {
  awk '
    inblk == 0 && $0 == "#[cfg(test)]" { print; inblk = 1; next }
    inblk == 1 { print; if ($0 ~ /;[[:space:]]*$/) inblk = 0; else inblk = 2; next }
    inblk == 2 { print; if ($0 == "}") inblk = 0; next }
  '
}

marca_ini='# --- self-test begin ---'
marca_fin='# --- self-test end ---'

# Las lineas de cada par, marcadores incluidos, en orden. Sin anidamiento: un
# begin dentro de una region no abre otra (marcas_sh lo pone en rojo antes de
# que esto cuente).
regiones_self_test() {
  awk -v ini="$marca_ini" -v fin="$marca_fin" '
    $0 == ini { dentro = 1 }
    dentro { print }
    $0 == fin { dentro = 0 }
  '
}

filtrar_region() { # <ruta-relativa>, contenido ya normalizado por stdin
  case "$1" in
    # Un .rs bajo tests/ es test de arriba abajo. El crate tiene 81 y solo 3
    # llevan #[cfg(test)]: extraer por atributo dejaria 78 ficheros de test
    # invisibles para `frozen`, que es el agujero mas grande que cabria aqui.
    */tests/*.rs | tests/*.rs) cat ;;
    *.sh) regiones_self_test ;;
    *) regiones_cfg_test ;;
  esac
}

# El CR se quita AQUI, en un solo sitio y delante de todo.
#
# Medido 2026-09-22: con core.autocrlf=true y sin regla para *.rs, git dejaba
# el ARBOL en CRLF y el COMMIT en LF, y cada fichero diferia de su blob SOLO por
# los finales de linea: `frozen` acusaria a toda pasada verde honesta. Medido en
# el fixture: rust/tests/contrato.rs son 62 B en el arbol contra 59 B en el
# commit. Desde d4b2e2b .gitattributes lleva `*.rs text eol=lf`, pero el sed
# sigue haciendo falta: un clon hecho antes de esa regla conserva sus .rs en
# CRLF, porque git no reescribe un fichero del arbol al ponerle atributos.
#
# Va delante del awk, y no dentro, porque gawk (el de Git Bash) strippea el CR
# por su cuenta y mawk no: sin esto el guardia decidiria distinto segun el awk
# instalado, que es la clase de diferencia que nadie mira hasta que muerde.
region() { # <ruta-relativa>, contenido por stdin
  sed 's/\r$//' | filtrar_region "$1"
}

region_en_commit() { # <commit> <ruta>
  # Un fichero que no existia en ese commit da region vacia, no error.
  if git -C "$ROOT" cat-file -e "$1:$2" 2>/dev/null; then
    git -C "$ROOT" show "$1:$2" | region "$2"
  fi
}

region_en_arbol() { # <ruta>
  if [[ -f "$ROOT/$1" ]]; then
    region "$1" < "$ROOT/$1"
  fi
}

en_la_tajada() { # <fichero> [ruta...] -> 0 si el fichero cae dentro de alguna ruta
  local f="$1" p
  shift
  (( $# > 0 )) || return 0
  for p in "$@"; do
    [[ "$f" == "$p" || "$f" == "$p/"* ]] && return 0
  done
  return 1
}

# Una sola lista para las dos pasadas (la comparacion de regiones y el juicio de
# marcadores): si salieran de dos sitios, podrian dejar de mirar los mismos
# ficheros sin que nada lo notara.
ficheros_candidatos() { # <commit> [ruta...] -> .rs y .sh cambiados o sin versionar, dentro de la tajada
  local commit="$1" f
  shift
  # `git diff` no ve lo que no esta trackeado, y la pasada roja estrena ficheros
  # de test a menudo. Sin ls-files --others, `written` diria que no se escribio
  # ningun test justo el dia que se escribio uno nuevo.
  {
    git -C "$ROOT" diff --name-only "$commit" -- '*.rs' '*.sh' 2>/dev/null
    git -C "$ROOT" ls-files --others --exclude-standard -- '*.rs' '*.sh' 2>/dev/null
  } | sort -u | while IFS= read -r f; do
    # `if`, no `&&`: con pipefail un ultimo fichero fuera de la tajada dejaria el
    # bucle en 1 y mataria al llamante dentro de su sustitucion, sin decir nada.
    if [[ -n "$f" ]] && en_la_tajada "$f" "$@"; then
      printf '%s\n' "$f"
    fi
  done
}

# Imprime, con ruta:linea, cada marcador mal puesto de un .sh del ARBOL y cada
# despacho de --self-test que cae fuera de toda region. Devuelve 1 si imprimio.
marcas_sh() { # <ruta-relativa>
  local salida
  salida="$(sed 's/\r$//' < "$ROOT/$1" | awk -v ini="$marca_ini" -v fin="$marca_fin" -v ruta="$1" '
    function dice(n, txt) { print ruta ":" n ": " txt }
    $0 == ini {
      if (dentro) dice(NR, "\047" ini "\047 abre una region dentro de la que abrio la linea " abierta "; las regiones no se anidan")
      else { dentro = 1; abierta = NR }
      next
    }
    $0 == fin {
      if (dentro) dentro = 0
      else dice(NR, "\047" fin "\047 no cierra ninguna region")
      next
    }
    !dentro && (index($0, "== \"--self-test\"") || $0 ~ /^[ \t]*"?--self-test"?\)/) {
      dice(NR, "despacha --self-test fuera de toda region marcada, asi que sus tests son invisibles para written y frozen. Envuelve el self-test entero (el bloque o la funcion, sus ayudantes y el brazo del case que lo despacha) entre \047" ini "\047 y \047" fin "\047, en columna 0.")
    }
    END { if (dentro) dice(abierta, "\047" ini "\047 no tiene su \047" fin "\047") }
  ')" || true
  [[ -z "$salida" ]] && return 0
  printf '%s\n' "$salida"
  return 1
}

tests_tocados() { # <commit> <ficheros candidatos, uno por linea> -> imprime los que cambiaron de region
  local commit="$1" ficheros="$2" f a b
  [[ -n "$ficheros" ]] || return 0
  while IFS= read -r f; do
    [[ -n "$f" ]] || continue
    a="$(region_en_commit "$commit" "$f")"
    b="$(region_en_arbol "$f")"
    [[ "$a" == "$b" ]] || printf '%s\n' "$f"
  done <<< "$ficheros"
}

# ------------------------------------------------------------------ la tajada

# `paths:` en el handoff, una ruta por linea, como lista de YAML en bloque
#   paths:
#     - rust/tests/local_gate_contract.rs
#     - scripts
# o en linea: `paths: [rust/src/cosa.rs, scripts]`. Sin comillas ni `./`
# delante ni `/` detras, que se quitan aqui: son la misma ruta.
lista_paths() { # <fichero.yml>
  awk '
    function limpia(x) {
      gsub(/["\047\r]/, "", x)
      sub(/^[[:space:]]+/, "", x); sub(/[[:space:]]+$/, "", x)
      sub(/^\.\//, "", x); sub(/\/+$/, "", x)
      if (x != "") print x
    }
    /^paths:/ {
      resto = $0; sub(/^paths:[[:space:]]*/, "", resto)
      if (resto ~ /^\[/) {
        gsub(/[][]/, "", resto)
        n = split(resto, a, ",")
        for (i = 1; i <= n; i++) limpia(a[i])
        next
      }
      dentro = 1; next
    }
    dentro && /^[[:space:]]*-/ { x = $0; sub(/^[[:space:]]*-/, "", x); limpia(x); next }
    dentro && /^[[:space:]]*(#|$)/ { next }
    { dentro = 0 }
  ' "$1"
}

# ----------------------------------------------------- el coste (CI-E6)

recorta() { # <texto> -> sin los espacios de los bordes
  local x="$1"
  x="${x#"${x%%[![:space:]]*}"}"
  printf '%s' "${x%"${x##*[![:space:]]}"}"
}

# Las lineas de un `evidence: |`, sin su sangria. Una `evidence:` de una linea no
# tiene bloque, y lo que lleve no cuenta: ni segundos ni carril.
evidencia() { # <fichero.yml>
  awk '
    { sub(/\r$/, "") }
    dentro && ($0 ~ /^[[:space:]]+[^[:space:]]/ || $0 ~ /^[[:space:]]*$/) {
      sub(/^[[:space:]]+/, ""); print; next
    }
    { dentro = 0 }
    /^evidence:[[:space:]]*[|>][-+]?[[:space:]]*$/ { dentro = 1 }
  ' "$1"
}

filas_dag() { # los ids de las filas de scripts/gate-dag.txt de ESTE arbol, uno por linea
  [[ -f "$ROOT/scripts/gate-dag.txt" ]] || return 0
  awk '{ sub(/\r$/, "") } /^[[:space:]]*(#|$)/ { next } { print $2 }' "$ROOT/scripts/gate-dag.txt"
}

# Imprime lo que esta mal de un carril, o nada si es una fila de la tabla.
carril_problema() { # <id> <ids, uno por linea>
  local fila filas=""
  while IFS= read -r fila; do
    [[ -n "$fila" ]] || continue
    if [[ "$fila" == "$1" ]]; then return 0; fi
    filas="${filas:+$filas }$fila"
  done <<<"$2"
  echo "$1 no es una fila de scripts/gate-dag.txt (filas: ${filas:-ninguna, el fichero no existe o esta vacio})"
}

# Imprime lo que esta mal de un `segundos:`, o nada. `<S> s; <orden acotada>[; <su ultima linea>]`
segundos_problema() { # <lo que sigue a `segundos:`>
  local valor="$1" medido orden cola="" dicho
  medido="$(recorta "${valor%%;*}")"
  [[ "$valor" != *";"* ]] || cola="${valor#*;}"
  orden="$(recorta "${cola%%;*}")"
  if [[ ! "$medido" =~ ^([0-9]+([.,][0-9]+)?)[[:space:]]*s$ || -z "$orden" ]]; then
    echo "segundos invalido: se esperaba 'segundos: <S> s; <orden acotada>[; <su ultima linea>]' con S un numero, y llego 'segundos:$valor'"
    return 0
  fi
  medido="${BASH_REMATCH[1]}"
  if [[ "$cola" =~ finished\ in\ ([0-9]+([.,][0-9]+)?)s ]]; then
    dicho="${BASH_REMATCH[1]}"
    awk -v a="${medido/,/.}" -v b="${dicho/,/.}" 'BEGIN { exit !(a + 0 == b + 0) }' ||
      echo "segundos no cuadra: la linea dice $medido s y su 'finished in' dice ${dicho}s. El numero sale de la linea medida, no de memoria."
  fi
}

toca_el_juez() { # <commit> [ruta...] -> los ficheros de scripts/ cambiados o sin versionar, dentro de la tajada
  local commit="$1" f
  shift
  # Todo lo que hay bajo scripts/, sea .sh, .py o .txt: un paso del juez es una fila
  # de gate-dag.txt y lo que comparte vive ahi. `ficheros_candidatos` solo ve .rs y
  # .sh, y no sirve para esto.
  {
    git -C "$ROOT" diff --name-only "$commit" -- scripts 2>/dev/null
    git -C "$ROOT" ls-files --others --exclude-standard -- scripts 2>/dev/null
  } | sort -u | while IFS= read -r f; do
    if [[ -n "$f" ]] && en_la_tajada "$f" "$@"; then
      printf '%s\n' "$f"
    fi
  done
}

# El traspaso del implementador: sus segundos y, si toca el juez, su carril.
coste_traspaso() { # <fichero.yml> <commit> [ruta...] -> 0 si esta, 1 si falta o esta mal (lo explica)
  local path="$1" commit="$2" linea mal=0 valor problema id resto ids
  local -a lineas segundos=() carriles=() tocados
  shift 2
  mapfile -t lineas < <(evidencia "$path")
  for linea in "${lineas[@]}"; do
    case "$linea" in
      segundos:*) segundos+=("${linea#segundos:}") ;;
      carril:*) carriles+=("${linea#carril:}") ;;
    esac
  done

  if (( ${#segundos[@]} == 0 )); then
    echo "FALTA segundos: un traspaso del implementador con commit lleva, dentro de 'evidence: |' y en su propia linea sangrada,"
    echo "  segundos: <S> s; <orden acotada que corrio los tests nuevos>[; <su ultima linea>]"
    echo "En una 'evidence:' de una sola linea no cuenta, y en columna 0 seria una clave mas."
    mal=1
  fi
  for valor in "${segundos[@]}"; do
    problema="$(segundos_problema "$valor")"
    if [[ -n "$problema" ]]; then echo "$problema"; mal=1; fi
  done

  mapfile -t tocados < <(toca_el_juez "$commit" "$@")
  if (( ${#tocados[@]} > 0 )); then
    ids="$(filas_dag)"
    if (( ${#carriles[@]} == 0 )); then
      echo "FALTA carril: la tajada toca ${tocados[*]} y entonces 'evidence: |' lleva, en su propia linea sangrada,"
      echo "  carril: <id de una fila de scripts/gate-dag.txt>; comparte: <recursos de esa fila, o nada>"
      mal=1
    fi
    for valor in "${carriles[@]}"; do
      if [[ "$valor" =~ ^[[:space:]]*([^[:space:];]+)[[:space:]]*\;[[:space:]]*comparte:[[:space:]]*(.*[^[:space:]])[[:space:]]*$ ]]; then
        id="${BASH_REMATCH[1]}"
        problema="$(carril_problema "$id" "$ids")"
        if [[ -n "$problema" ]]; then echo "carril invalido: $problema"; mal=1; fi
      else
        resto="$(recorta "$valor")"
        echo "carril invalido: 'carril:$valor' no dice su '; comparte: <recursos>' (o 'comparte: nada'): $resto"
        mal=1
      fi
    done
  fi
  return "$mal"
}

specs_nombradas() { # <task> <evidence> <fichero.yml> -> los nombres de .cursor/handoffs/*.spec.md que nombran
  { printf '%s\n%s\n' "$1" "$2"; evidencia "$3"; } |
    { grep -Eo '\.cursor/handoffs/[A-Za-z0-9._-]+\.spec\.md' || true; } | sed 's|.*/||' | sort -u
}

cabecera_coste='| Escenario | Nivel | Segundos | Carril | Por qué no el barato |'

# Lo que lee de una spec, un registro por linea y separados por \037 (un blanco
# no, que colapsa los campos vacios): H cabecera de la tabla, E escenario de un
# bloque gherkin, R fila de cinco celdas, M fila que no las tiene.
registros_de_spec() { # <spec.md> <cabecera>
  # shellcheck disable=SC2016  # the backticks are the fence of the block, for awk to read
  awk -v cerca='```' -v cab="$2" '
    function celdas(l,    t, i) {
      t = l; sub(/^[[:space:]]*\|/, "", t); sub(/\|[[:space:]]*$/, "", t)
      n = split(t, c, "|")
      for (i = 1; i <= n; i++) { sub(/^[[:space:]]+/, "", c[i]); sub(/[[:space:]]+$/, "", c[i]) }
      return n
    }
    { sub(/\r$/, ""); l = $0; sub(/^[[:space:]]+/, "", l) }
    l == cerca "gherkin" { gh = 1; next }
    gh && l == cerca { gh = 0; next }
    gh {
      if (match($0, /^[[:space:]]*(Escenario|Esquema del escenario):[[:space:]]*/)) {
        nom = substr($0, RLENGTH + 1); sub(/[[:space:]]+$/, "", nom); print "E\037" nom
      }
      next
    }
    estado > 0 && l !~ /^\|/ { estado = 0 }
    l ~ /^\|/ {
      k = celdas($0)
      if (estado == 0) {
        if (k == 5 && c[1] == "Escenario" && c[2] == "Nivel" && c[3] == "Segundos" && c[4] == "Carril" && c[5] == "Por qué no el barato") {
          print "H"; estado = 1
        }
      } else if (estado == 1) {
        estado = (l ~ /^\|[-: |]+$/) ? 2 : 0
      } else if (k == 5) {
        print "R\037" c[1] "\037" c[2] "\037" c[3] "\037" c[4] "\037" c[5]
      } else {
        print "M\037" l
      }
    }
  ' "$1"
}

# Imprime lo que esta mal de la spec y devuelve 1 si algo lo esta.
spec_problemas() { # <spec.md> <nombre>
  local spec="$1" nombre="$2" ids tipo c1 c2 c3 c4 c5 mal=0 problema
  local hay_cabecera=0 n_esc=0 n_fil=0 k
  local -a orden_esc=() orden_fil=()
  local -A esc=() fil=()
  ids="$(filas_dag)"
  while IFS=$'\037' read -r tipo c1 c2 c3 c4 c5; do
    case "$tipo" in
      H) hay_cabecera=1 ;;
      E)
        n_esc=$(( n_esc + 1 ))
        [[ -n "${esc[$c1]:-}" ]] || orden_esc+=("$c1")
        esc[$c1]=$(( ${esc[$c1]:-0} + 1 ))
        ;;
      M)
        echo "$nombre: fila de la tabla de coste sin sus cinco celdas: $c1"
        mal=1
        ;;
      R)
        n_fil=$(( n_fil + 1 ))
        [[ -n "${fil[$c1]:-}" ]] || orden_fil+=("$c1")
        fil[$c1]=$(( ${fil[$c1]:-0} + 1 ))
        case "$c2" in
          pura | guion | servicio | e2e) ;;
          *) echo "$nombre: nivel invalido '$c2' en la fila '$c1' (pura|guion|servicio|e2e)"; mal=1 ;;
        esac
        if [[ ! "$c3" =~ ^[0-9]+([.,][0-9]+)?$ ]]; then
          echo "$nombre: segundos invalido '$c3' en la fila '$c1' (un numero, con . o , decimal)"
          mal=1
        fi
        problema="$(carril_problema "$c4" "$ids")"
        if [[ -n "$problema" ]]; then echo "$nombre: carril invalido en la fila '$c1': $problema"; mal=1; fi
        case "$c2" in
          guion | servicio | e2e)
            if [[ -z "$c5" || "$c5" == "-" || "$c5" == "—" ]]; then
              echo "$nombre: la fila '$c1' es de nivel $c2 y no dice por qué el barato no lo demuestra"
              mal=1
            fi
            ;;
        esac
        ;;
    esac
  done < <(registros_de_spec "$spec" "$cabecera_coste")

  if (( hay_cabecera == 0 )); then
    echo "$nombre: no tiene tabla de coste. Copia esta cabecera y pon una fila por escenario:"
    echo "$cabecera_coste"
    echo "|---|---|---|---|---|"
    return 1
  fi
  if (( n_esc == 0 && n_fil == 0 )); then
    echo "$nombre: sin escenarios (ni un 'Escenario:' en un bloque gherkin ni una fila en la tabla de coste)"
    return 1
  fi
  for k in "${orden_esc[@]}"; do
    if (( ${fil[$k]:-0} < ${esc[$k]} )); then echo "$nombre: escenario sin fila de coste: $k"; mal=1; fi
  done
  for k in "${orden_fil[@]}"; do
    if (( ${esc[$k]:-0} < ${fil[$k]} )); then echo "$nombre: fila de coste sin escenario: $k"; mal=1; fi
  done
  return "$mal"
}

# La spec hermana del handoff: se resuelve JUNTO al .yml juzgado y no bajo ROOT,
# porque las specs se escriben en un arbol y el guardia puede ser el de otro.
coste_spec() { # <fichero.yml> <task> <evidence> -> 0 si cada spec nombrada trae su tabla al dia
  local path="$1" dir nombres nombre mal=0
  dir="$(dirname "$path")"
  nombres="$(specs_nombradas "$2" "$3" "$path")"
  if [[ -z "$nombres" ]]; then
    echo "FALTA spec: un traspaso del especificador al implementador nombra, en 'task' o en 'evidence', su spec: .cursor/handoffs/<nombre>.spec.md"
    return 1
  fi
  while IFS= read -r nombre; do
    if [[ ! -f "$dir/$nombre" ]]; then
      echo "$nombre no existe junto al handoff ($dir): se nombra y no esta"
      mal=1
    else
      spec_problemas "$dir/$nombre" "$nombre" || mal=1
    fi
  done <<<"$nombres"
  return "$mal"
}

# Los historicos anteriores a CI-E6, por el hash de su contenido sin CR. Devuelve 0
# si este esta listado y vigente, 1 si no esta, 2 si la lista esta mal formada o su
# excepcion caduco (y lo dice). La lista se lee entera siempre: un fallo en una
# linea que no es la que casa tambien deja rojo.
exento_por_lista() { # <fichero.yml>
  local lista="$ROOT/scripts/handoffs-sin-coste.txt" hash hoy n=0 linea h nombre caduca motivo mal=0
  local casa="" casa_caduca="" casa_n=""
  [[ -f "$lista" ]] || return 1
  command -v sha256sum >/dev/null 2>&1 || { echo "falta sha256sum: no se puede juzgar scripts/handoffs-sin-coste.txt"; return 2; }
  hash="$(sed 's/\r$//' "$1" | sha256sum | cut -d' ' -f1)"
  hoy="$(date +%F)"
  while IFS= read -r linea || [[ -n "$linea" ]]; do
    n=$(( n + 1 ))
    linea="${linea%$'\r'}"
    if [[ -z "$(recorta "$linea")" || "$linea" =~ ^[[:space:]]*# ]]; then continue; fi
    read -r h nombre caduca _ motivo <<<"$linea"
    if [[ ! "$h" =~ ^[0-9a-f]{64}$ || ! "$caduca" =~ ^[0-9]{4}-(0[1-9]|1[0-2])-(0[1-9]|[12][0-9]|3[01])$ || -z "$motivo" ]]; then
      echo "scripts/handoffs-sin-coste.txt:$n: linea malformada: se esperaba '<sha256 de 64 hex> <nombre> <AAAA-MM-DD> <dueño> <motivo>'"
      mal=1
    elif [[ "$h" == "$hash" ]]; then
      casa="$nombre"; casa_caduca="$caduca"; casa_n="$n"
    fi
  done < "$lista"
  (( mal == 0 )) || return 2
  [[ -n "$casa" ]] || return 1
  if [[ "$casa_caduca" < "$hoy" ]]; then
    echo "la excepción de $(basename "$1") caducó el $casa_caduca (scripts/handoffs-sin-coste.txt:$casa_n): ya no exime, y el traspaso se juzga por su coste."
    return 2
  fi
  echo "exento del coste hasta $casa_caduca: $(basename "$1") es anterior a CI-E6 (scripts/handoffs-sin-coste.txt:$casa_n)"
  return 0
}

# Dos traspasos se juzgan por su coste: del especificador al implementador (su spec,
# de cualquier type) y del implementador con git_handoff y commit (sus segundos y,
# si toca el juez, su carril). La nota del implementador, el mejorador, el
# endurecedor y el especificador hacia otro rol no entregan tests que medir.
juzgar_coste() { # <fichero.yml> <from> <to> <type> <commit> <task> <evidence> [ruta...]
  local path="$1" from="$2" to="$3" tipo="$4" commit="$5" task="$6" evidence="$7" alcance="" salida rc=0 exencion ex=0
  shift 7
  if [[ "$from" == "especificador" && "$to" == "implementador" ]]; then
    alcance=spec
  elif [[ "$from" == "implementador" && "$tipo" == "git_handoff" && "$commit" != "none" ]]; then
    alcance=traspaso
  fi
  [[ -n "$alcance" ]] || return 0
  if [[ "$alcance" == "spec" ]]; then
    salida="$(coste_spec "$path" "$task" "$evidence")" || rc=$?
  else
    salida="$(coste_traspaso "$path" "$commit" "$@")" || rc=$?
  fi
  (( rc != 0 )) || return 0
  exencion="$(exento_por_lista "$path")" || ex=$?
  case "$ex" in
    0) printf '%s\n' "$exencion"; return 0 ;;
    1) printf '%s\n' "$salida"; return 1 ;;
    *) printf '%s\n%s\n' "$exencion" "$salida"; return 1 ;;
  esac
}

# -------------------------------------------------------------- el veredicto

juzgar() { # <fichero.yml>
  local path="$1"
  [[ -f "$path" ]] || { echo "no existe: $path"; return 1; }

  # El `|| true` no sobra. Con set -e y pipefail, un grep que no encuentra nada
  # mataba el script DENTRO de la asignacion, asi que el bucle de «FALTA campo»
  # de abajo no llego a imprimir nunca: el guardia salia 1 y mudo. Comprobado
  # 2026-09-22 contra un handoff sin `evidence`.
  val() { { grep -E "^$1:" "$path" || true; } | head -1 | sed "s/^$1:[[:space:]]*//;s/[\"']//g" | tr -d '\r'; }

  local from to tipo task commit evidence tests
  from=$(val from); to=$(val to); tipo=$(val type)
  task=$(val task); commit=$(val commit); evidence=$(val evidence)
  tests=$(val tests)

  # `paths`, opcional: la tajada que el handoff declara suya. Con varios
  # agentes en un mismo arbol, comparar el arbol entero rechaza la pasada
  # verde honesta de uno por el test que otro edito a la vez. Con `paths`,
  # written y frozen miran solo los ficheros bajo esas rutas; sin el campo,
  # el arbol entero, como siempre.
  local declara_paths=0
  local -a paths=()
  if grep -Eq '^paths:' "$path"; then
    declara_paths=1
    mapfile -t paths < <(lista_paths "$path")
  fi

  local k v
  for k in from to tipo task commit evidence; do
    eval "v=\${$k}"
    [[ -n "${v:-}" ]] || { echo "FALTA campo: $k"; return 1; }
  done
  echo "$from" | grep -Eqx "$roles" || { echo "from invalido: $from"; return 1; }
  echo "$to" | grep -Eqx "$roles" || { echo "to invalido: $to"; return 1; }
  [[ "$tipo" == "git_handoff" || "$tipo" == "note" ]] || { echo "type invalido"; return 1; }
  if [[ "$commit" != "none" && ! "$commit" =~ ^[0-9a-f]{7,40}$ ]]; then
    echo "commit invalido: $commit"; return 1
  fi

  if [[ "$commit" == "none" ]]; then
    # Un handoff sin commit no entrega codigo, asi que declarar sobre los tests
    # seria declarar sobre la nada. Prohibirlo es lo que impide que `tests` se
    # convierta en un adorno que se copia de un handoff a otro sin mirar.
    [[ -z "$tests" ]] || {
      echo "tests prohibido con commit: none — un handoff sin commit no entrega codigo"
      return 1
    }
    (( declara_paths == 0 )) || {
      echo "paths prohibido con commit: none — no hay codigo entregado del que acotar la tajada"
      return 1
    }
  else
    # Que el sha TENGA FORMA no prueba nada: la forma la cumple cualquier cosa
    # que uno se invente con los dedos sobre las teclas de la a a la f.
    git -C "$ROOT" cat-file -e "${commit}^{commit}" 2>/dev/null || {
      echo "commit no existe en este repo: $commit"
      return 1
    }
    # Una tajada vacia no tiene tests que puedan cambiar, y una ruta mal
    # escrita no casa con ninguno: en los dos casos `frozen` pasaria siempre,
    # sin haber mirado nada.
    if (( declara_paths == 1 )); then
      (( ${#paths[@]} > 0 )) || {
        echo "paths declarado sin ninguna ruta: una tajada vacia no tiene tests que mirar"
        return 1
      }
      local p
      for p in "${paths[@]}"; do
        [[ -e "$ROOT/$p" ]] || git -C "$ROOT" cat-file -e "$commit:$p" 2>/dev/null || {
          echo "paths: $p no existe ni en el arbol ni en $commit"
          return 1
        }
      done
    fi
    # Antes del case y fuera de la sustitucion de `cambiados`: con set -e un
    # fallo dentro de ella mataria el script sin decir nada (la leccion de
    # val()). Se juzga el .sh del ARBOL; el de `commit` es historia, y si no tenia
    # marcadores su region sale vacia y la comparacion lo cuenta como movido.
    local sh_f marcas mal=0 candidatos
    candidatos="$(ficheros_candidatos "$commit" "${paths[@]}")"
    while IFS= read -r sh_f; do
      [[ "$sh_f" == *.sh && -f "$ROOT/$sh_f" ]] || continue
      marcas="$(marcas_sh "$sh_f")" || { printf '%s\n' "$marcas"; mal=1; }
    done <<< "$candidatos"
    (( mal == 0 )) || {
      echo "regiones de self-test mal marcadas: no se juzga written ni frozen sobre una region adivinada."
      return 1
    }
    local cambiados
    cambiados="$(tests_tocados "$commit" "$candidatos")"
    case "$tests" in
      written)
        [[ -n "$cambiados" ]] || {
          echo "tests: written, pero NINGUNA region de test cambio desde $commit."
          echo "Una pasada roja que no movio un test no escribio nada que pueda"
          echo "ponerse rojo, y el commit test(rojo) no probaria nada."
          return 1
        }
        ;;
      frozen)
        [[ -z "$cambiados" ]] || {
          echo "tests: frozen, pero estas regiones de test cambiaron desde $commit:"
          printf '%s\n' "$cambiados" | sed 's/^/  /'
          echo "La pasada verde escribe codigo sin tocar un test. Si un test parece"
          echo "incorrecto se para y se devuelve un handoff type: note; no se ajusta."
          return 1
        }
        ;;
      "")
        echo "FALTA campo: tests — obligatorio con commit: $commit (written|frozen)."
        echo "Omitirlo era la fuga: el protocolo de dos pasadas dependia de la"
        echo "honestidad del agente en vez de un guardia."
        return 1
        ;;
      *)
        echo "tests invalido: $tests (written|frozen)"; return 1
        ;;
    esac
  fi

  # Despues de todo lo de arriba, written y frozen incluidos (D-E6h): los rojos y
  # los mensajes de siempre no cambian, y el coste se juzga sobre un traspaso que ya
  # es honesto en lo demas.
  juzgar_coste "$path" "$from" "$to" "$tipo" "$commit" "$task" "$evidence" "${paths[@]}" || return 1

  echo "OK $path $from->$to"
  return 0
}

# --- self-test begin ---
# --------------------------------------------------------------- el self-test

# Un guardia que no se ve fallar es un parrafo, que es literalmente lo que este
# fichero era hasta hoy. Cada guardia contra un fixture construido para
# romperla, y cada caso honesto contra uno que debe pasar.

st_tmp=""
st_limpiar() { [[ -n "$st_tmp" && -d "$st_tmp" ]] && rm -rf "$st_tmp"; return 0; }

# Los marcadores de los scripts de fixture se generan, no se escriben: una linea
# de este fichero que fuera literalmente un marcador cortaria SU PROPIA region, y
# el contrato de local_gate_contract.rs lo pondria rojo. Estas asignaciones no
# son marcadores, y @@INI@@ / @@FIN@@ de los heredocs se sustituyen con sed.
st_ini='# --- self-test begin ---'
st_fin='# --- self-test end ---'

# Vuelve el repo `sh` a su base sin tocar git: las tres filas que editan un
# script se encadenan sobre la misma copia, y cada una arranca limpia.
st_restaurar() { # <dir>
  cp "$st_tmp/pristino/"*.sh "$1/scripts/"
  cp "$st_tmp/pristino/contrato.rs" "$1/rust/tests/contrato.rs"
  rm -f "$1/scripts/nuevo.sh"
}

# El guardia resuelve ROOT desde su propia ubicacion, asi que para juzgarlo
# contra un repo desechable hay que copiarlo DENTRO de el.
mk_repo() { # <dir> <autocrlf true|false> -> imprime el sha base
  local d="$1" crlf="$2"
  mkdir -p "$d/rust/src" "$d/rust/tests" "$d/scripts" "$d/.cursor/handoffs"
  cp "$SELF" "$d/scripts/validar-handoff.sh"
  chmod +x "$d/scripts/validar-handoff.sh"
  git -C "$d" init -q
  git -C "$d" config user.email "self-test@example.invalid"
  git -C "$d" config user.name "validar-handoff self-test"
  git -C "$d" config commit.gpgsign false
  # autocrlf=false a proposito: con true, git normaliza el arbol antes de
  # diffear y los ficheros CRLF ni siquiera saldrian como modificados, asi que
  # el fixture no llegaria a la comparacion que queremos probar.
  git -C "$d" config core.autocrlf false

  cat > "$d/rust/src/cosa.rs" <<'RS'
pub fn respuesta() -> u32 {
    41
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_respuesta_es_la_respuesta() {
        assert_eq!(respuesta(), 42);
    }
}
RS

  cat > "$d/rust/tests/contrato.rs" <<'RS'
#[test]
fn el_contrato_se_sostiene() {
    assert!(true);
}
RS

  # Un script del juez en cada forma que hay en scripts/: el bloque `if`
  # (juez.sh), la funcion con su ayudante FUERA de self_test() y el brazo del
  # `case` que la despacha, cada cosa con su par de marcadores (juez-fn.sh), y
  # uno sin modo --self-test (plano.sh), que no necesita marcadores.
  cat > "$d/scripts/juez.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
@@INI@@
if [[ "${1:-}" == "--self-test" ]]; then
  [[ "$(bash "$0" doble 2)" == "4" ]] || { echo "FAIL self-test: doble" >&2; exit 1; }
  echo "OK  self-test: doble"
  exit 0
fi
@@FIN@@
case "${1:-}" in
  doble) echo $(( $2 * 2 )) ;;
esac
SH

  cat > "$d/scripts/juez-fn.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
triple() { echo $(( $1 * 3 )); }
@@INI@@
must_be() { [[ "$1" == "$2" ]] || { echo "FAIL self-test: $1 != $2" >&2; exit 1; }; }
self_test() {
  must_be "$(triple 2)" 6
  echo "OK  self-test: triple"
}
@@FIN@@
case "${1:-}" in
@@INI@@
  --self-test) self_test ;;
@@FIN@@
  triple) triple "$2" ;;
esac
SH

  printf '%s\n' '#!/usr/bin/env bash' 'echo plano' > "$d/scripts/plano.sh"

  # CI-E6 (D-E6i): the judge of the cost reads the rows of scripts/gate-dag.txt of
  # the tree it judges, so the fixture has a table of two rows, in the base: a
  # carril is a row of it, and a file that is not .rs or .sh (the table is neither)
  # is how a slice touches the judge without any region of test moving.
  printf '%s\n' '# fixture del self-test: las filas de scripts/gate-dag.txt' \
    '10 P - - normal 1 1 1 true' \
    '20 DBG P target:T_dbg normal 1 1 1 true' > "$d/scripts/gate-dag.txt"
  sed -i -e "s/^@@INI@@\$/$st_ini/" -e "s/^@@FIN@@\$/$st_fin/" \
    "$d/scripts/juez.sh" "$d/scripts/juez-fn.sh"

  # Rutas explicitas: `git add -A` arrastra CRLF fantasma y ficheros ajenos, y
  # en este repo lo bloquea la guardia de comandos.
  git -C "$d" add rust/src/cosa.rs rust/tests/contrato.rs scripts/validar-handoff.sh \
    scripts/juez.sh scripts/juez-fn.sh scripts/plano.sh scripts/gate-dag.txt >/dev/null 2>&1
  git -C "$d" commit -q -m "base" >/dev/null 2>&1

  # El arbol se convierte a CRLF DESPUES del commit, que es como pasa de verdad:
  # se escribe CRLF, git lo normaliza al commitear y el arbol de trabajo queda
  # inconsistente con el blob. El commit se queda en LF y el arbol en CRLF.
  if [[ "$crlf" == "true" ]]; then
    sed -i 's/$/\r/' "$d/rust/src/cosa.rs" "$d/rust/tests/contrato.rs" \
      "$d/scripts/juez.sh" "$d/scripts/juez-fn.sh" "$d/scripts/plano.sh"
    local f
    for f in rust/tests/contrato.rs scripts/juez.sh; do
      grep -qU "$(printf '\r')" "$d/$f" || {
        echo "FAIL self-test: el fixture CRLF no tiene CRLF en $f, asi que no prueba nada." >&2
        return 1
      }
    done
  fi

  git -C "$d" rev-parse HEAD
}

mk_handoff() { # <dir> <nombre> <commit> <tests|-> [rol-from]
  local d="$1" nombre="$2" commit="$3" t="$4" desde="${5:-implementador}"
  {
    echo "from: $desde"
    echo "to: mejorador"
    echo "type: git_handoff"
    echo 'task: "fixture del self-test"'
    echo "commit: $commit"
    echo 'evidence: |'
    echo '  fixture del self-test'
    echo '  segundos: 0.01 s; bash scripts/validar-handoff.sh --self-test'
    echo '  carril: DBG; comparte: target:T_dbg'
    [[ "$t" == "-" ]] || echo "tests: $t"
  } > "$d/.cursor/handoffs/$nombre.yml"
  printf '%s\n' "$d/.cursor/handoffs/$nombre.yml"
}

# CI-E6: un .yml con todos sus campos a la vista, para los casos del coste. El
# evidence va tal cual despues de la clave: `"texto"` en una linea, o lo que
# imprime ev_bloque.
mk_yml() { # <ruta.yml> <from> <to> <type> <commit> <task> <evidence> [tests]
  {
    echo "from: $2"
    echo "to: $3"
    echo "type: $4"
    echo "task: \"$6\""
    echo "commit: $5"
    echo "evidence: $7"
    [[ -z "${8:-}" ]] || echo "tests: $8"
  } > "$1"
}

ev_bloque() { # <linea>... -> el valor de un `evidence: |` con cada linea sangrada
  local l
  printf '%s\n' '|'
  for l in "$@"; do printf '  %s\n' "$l"; done
}

# Una spec de fixture (§3 del CI-E6): dos escenarios, A y B, y la tabla de coste
# con una fila por cada uno. Cada parametro es lo que un caso rompe.
mk_spec() { # <ruta.spec.md> [fila de A] [fila de B] [escenario de mas]
  local fila_a="${2:-| A | pura | 0,01 | DBG | — |}"
  local fila_b="${3:-| B | guion | 0.06 | DBG | la guarda solo existe en bash |}"
  {
    echo '# spec de fixture'
    echo
    echo '```gherkin'
    echo 'Característica: fixture'
    echo
    echo '  Escenario: A'
    echo '    Dado algo'
    echo
    echo '  Esquema del escenario: B'
    echo '    Dado <x>'
    echo '    Ejemplos:'
    echo '      | x |'
    echo '      | 1 |'
    [[ -z "${4:-}" ]] || printf '%s\n' "$4"
    echo '```'
    echo
    echo '| Escenario | Nivel | Segundos | Carril | Por qué no el barato |'
    echo '|---|---|---|---|---|'
    printf '%s\n' "$fila_a" "$fila_b"
  } > "$1"
}

# El sha256 de un handoff como lo calcula el guardia: sin los CR.
st_hash() { sed 's/\r$//' "$1" | sha256sum | cut -d' ' -f1; }

must_fail() { # <que> <dir> <yml>
  local que="$1" dir="$2" yml="$3" out rc=0
  out="$(bash "$dir/scripts/validar-handoff.sh" "$yml" 2>&1)" || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    echo "FAIL self-test: $que — el guardia lo acepto." >&2
    printf '%s\n' "$out" | sed 's/^/      /' >&2
    return 1
  fi
  echo "  rojo, como debe: $que"
  printf '%s\n' "$out" | sed 's/^/      /'
  return 0
}

must_pass() { # <que> <dir> <yml>
  local que="$1" dir="$2" yml="$3" out rc=0
  out="$(bash "$dir/scripts/validar-handoff.sh" "$yml" 2>&1)" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "FAIL self-test: $que — el guardia rechazo un handoff honesto:" >&2
    printf '%s\n' "$out" | sed 's/^/      /' >&2
    return 1
  fi
  echo "  verde, como debe: $que"
  return 0
}

# Un rojo mudo no le dice a nadie que arreglar (la leccion de `val()`): ademas
# del exit distinto de 0, la salida tiene que nombrar la causa.
must_fail_saying() { # <que> <aguja> <dir> <yml>
  local que="$1" aguja="$2" dir="$3" yml="$4" out rc=0
  out="$(bash "$dir/scripts/validar-handoff.sh" "$yml" 2>&1)" || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    echo "FAIL self-test: $que — el guardia lo acepto." >&2
    printf '%s\n' "$out" | sed 's/^/      /' >&2
    return 1
  fi
  if [[ "$out" != *"$aguja"* ]]; then
    echo "FAIL self-test: $que — rechazo sin decir '$aguja':" >&2
    printf '%s\n' "$out" | sed 's/^/      /' >&2
    return 1
  fi
  echo "  rojo, como debe: $que"
  printf '%s\n' "$out" | sed 's/^/      /'
  return 0
}

# Lo mismo con varias agujas: un rechazo del coste tiene que nombrar la causa y
# lo que se le pide al que entrega, y una sola aguja no ata las dos cosas.
must_fail_saying_all() { # <que> <dir> <yml> <aguja>...
  local que="$1" dir="$2" yml="$3" out rc=0 aguja
  shift 3
  out="$(bash "$dir/scripts/validar-handoff.sh" "$yml" 2>&1)" || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    echo "FAIL self-test: $que — el guardia lo acepto." >&2
    printf '%s\n' "$out" | sed 's/^/      /' >&2
    return 1
  fi
  for aguja in "$@"; do
    if [[ "$out" != *"$aguja"* ]]; then
      echo "FAIL self-test: $que — rechazo sin decir '$aguja':" >&2
      printf '%s\n' "$out" | sed 's/^/      /' >&2
      return 1
    fi
  done
  echo "  rojo, como debe: $que"
  printf '%s\n' "$out" | sed 's/^/      /'
  return 0
}

self_test() {
  local bad=0 base d yml
  st_tmp="$(mktemp -d)"
  trap st_limpiar EXIT

  echo "=== validar-handoff --self-test ==="

  # -- 1 y 2: produccion tocada, tests intactos -------------------------------
  d="$st_tmp/produccion"; base="$(mk_repo "$d" false)"
  sed -i 's/^    41$/    42/' "$d/rust/src/cosa.rs"
  yml="$(mk_handoff "$d" frozen-ok "$base" frozen)"
  must_pass "produccion tocada y tests intactos, declarado frozen" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" written-miente "$base" written)"
  must_fail "ese mismo arbol declarado written, sin haber escrito un test" "$d" "$yml" || bad=1

  # -- 3 y 4: un test editado -------------------------------------------------
  d="$st_tmp/test-editado"; base="$(mk_repo "$d" false)"
  sed -i 's/respuesta(), 42/respuesta(), 41/' "$d/rust/src/cosa.rs"
  yml="$(mk_handoff "$d" frozen-miente "$base" frozen)"
  must_fail "un test ajustado al codigo y declarado frozen" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" written-ok "$base" written)"
  must_pass "ese mismo arbol declarado written" "$d" "$yml" || bad=1

  # -- el agujero de los 78 ficheros de tests/ --------------------------------
  d="$st_tmp/tests-dir"; base="$(mk_repo "$d" false)"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  yml="$(mk_handoff "$d" frozen-integracion "$base" frozen)"
  must_fail "un .rs bajo tests/ editado (sin ningun #[cfg(test)]) y declarado frozen" "$d" "$yml" || bad=1

  # -- CRLF: el fixture que separa este guardia de uno histerico ---------------
  # Arbol en CRLF, commit en LF. El caso HONESTO es el que tiene dientes: sin
  # normalizar, rust/tests/contrato.rs difiere del blob solo por los CR (62 B
  # contra 59 B, medido) y este frozen legitimo se pondria rojo acusando a quien
  # no toco un test.
  d="$st_tmp/crlf"; base="$(mk_repo "$d" true)"
  sed -i 's/^    41/    42/' "$d/rust/src/cosa.rs"
  yml="$(mk_handoff "$d" frozen-crlf-ok "$base" frozen)"
  must_pass "arbol en CRLF y commit en LF, produccion tocada, declarado frozen" "$d" "$yml" || bad=1

  d="$st_tmp/crlf-miente"; base="$(mk_repo "$d" true)"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  yml="$(mk_handoff "$d" frozen-crlf "$base" frozen)"
  must_fail "un test editado en un arbol CRLF y declarado frozen" "$d" "$yml" || bad=1

  # -- paths: la tajada, en un arbol con varios agentes ------------------------
  # Dos agentes en un mismo arbol: este toco produccion en cosa.rs y otro, a la
  # vez, edito el test de contrato.rs. Sin `paths` el guardia mira el arbol
  # entero y rechaza la pasada verde honesta del primero por el trabajo del
  # segundo. Con `paths` mira solo lo que el handoff declara suyo.
  d="$st_tmp/paths"; base="$(mk_repo "$d" false)"
  sed -i 's/^    41$/    42/' "$d/rust/src/cosa.rs"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  yml="$(mk_handoff "$d" arbol-entero "$base" frozen)"
  must_fail "frozen sin paths, en un arbol donde otro agente edito un test" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" tajada-frozen "$base" frozen)"
  printf '%s\n' 'paths:' '  - rust/src/cosa.rs' >> "$yml"
  must_pass "frozen con paths limitado a la tajada propia, que no toco un test" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" tajada-en-linea "$base" frozen)"
  echo 'paths: [rust/src/cosa.rs]' >> "$yml"
  must_pass "la misma tajada escrita como lista en linea" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" tajada-con-test "$base" frozen)"
  printf '%s\n' 'paths:' '  - rust/src/cosa.rs' '  - rust/tests/contrato.rs' >> "$yml"
  must_fail "frozen con paths que incluyen un test editado" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" tajada-written "$base" written)"
  printf '%s\n' 'paths:' '  - rust/tests' >> "$yml"
  must_pass "written con paths a un directorio donde un test cambio" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" tajada-written-sin-test "$base" written)"
  printf '%s\n' 'paths:' '  - rust/src/cosa.rs' >> "$yml"
  must_fail "written con paths a una tajada donde ningun test cambio" "$d" "$yml" || bad=1
  # Una ruta mal escrita no casa con nada, y una tajada vacia no tiene tests
  # que puedan cambiar: `frozen` pasaria siempre. Las dos se rechazan.
  yml="$(mk_handoff "$d" tajada-inventada "$base" frozen)"
  printf '%s\n' 'paths:' '  - rust/src/no-existe.rs' >> "$yml"
  must_fail "paths con una ruta que no existe ni en el arbol ni en el commit" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" tajada-vacia "$base" frozen)"
  echo 'paths:' >> "$yml"
  must_fail "paths declarado sin ninguna ruta" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" none-con-paths none - especificador)"
  printf '%s\n' 'paths:' '  - rust/src/cosa.rs' >> "$yml"
  must_fail "commit: none con paths, sobre un codigo que no entrega" "$d" "$yml" || bad=1

  # -- 5: el campo omitido, que era la fuga -----------------------------------
  d="$st_tmp/omitido"; base="$(mk_repo "$d" false)"
  sed -i 's/^    41$/    42/' "$d/rust/src/cosa.rs"
  yml="$(mk_handoff "$d" sin-tests "$base" -)"
  must_fail "commit presente y tests omitido" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" tests-basura "$base" quizas)"
  must_fail "un valor de tests que no es ni written ni frozen" "$d" "$yml" || bad=1

  # -- 6: un sha con forma valida que no existe -------------------------------
  yml="$(mk_handoff "$d" sha-inventado deadbeefdeadbeefdeadbeefdeadbeefdeadbeef frozen)"
  must_fail "un sha con forma valida que no existe en el repo" "$d" "$yml" || bad=1

  # -- 7: la forma sigue siendo forma -----------------------------------------
  yml="$(mk_handoff "$d" rol-inventado "$base" frozen guardian-planta)"
  must_fail "un rol inventado" "$d" "$yml" || bad=1

  # -- commit: none, los dos sentidos -----------------------------------------
  yml="$(mk_handoff "$d" none-con-tests none frozen especificador)"
  must_fail "commit: none declarando sobre unos tests que no entrega" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" none-limpio none - especificador)"
  must_pass "un handoff de especificador: commit none y sin campo tests" "$d" "$yml" || bad=1

  # -- que los diagnosticos salgan, no solo el exit ---------------------------
  # El guardia anterior moria dentro de la asignacion y salia 1 MUDO, asi que su
  # bucle de «FALTA campo» era codigo muerto. Un exit sin frase no le dice a
  # nadie que arreglar.
  sed -i '/^evidence:/d' "$d/.cursor/handoffs/none-limpio.yml"
  local out rc=0
  out="$(bash "$d/scripts/validar-handoff.sh" "$d/.cursor/handoffs/none-limpio.yml" 2>&1)" || rc=$?
  if [[ "$rc" -eq 0 || "$out" != *"FALTA campo: evidence"* ]]; then
    echo "FAIL self-test: un campo que falta tiene que NOMBRARSE, no solo dar exit 1." >&2
    echo "      exit=$rc salida='$out'" >&2
    bad=1
  else
    echo "  rojo, como debe: un campo que falta se nombra en la salida"
  fi

  # -- el self-test de un script del juez es una region de test (G8) -----------
  # Hasta G8 un .sh en `paths` se aceptaba y se ignoraba: frozen daba OK sobre un
  # --self-test escrito de cero y written daba rojo sobre la pasada roja honesta.
  # Un repo solo para toda la seccion; antes de cada grupo se restaura desde una
  # copia, porque un repo por fila cuesta ~5 s en Windows y esto tiene 15.
  d="$st_tmp/sh"; base="$(mk_repo "$d" false)"
  mkdir -p "$st_tmp/pristino"
  cp "$d"/scripts/juez.sh "$d"/scripts/juez-fn.sh "$d"/scripts/plano.sh \
    "$d/rust/tests/contrato.rs" "$st_tmp/pristino/"

  # V1-V4: una linea del bloque de juez.sh cambia.
  sed -i 's/== "4"/== "5"/' "$d/scripts/juez.sh"
  yml="$(mk_handoff "$d" v1 "$base" frozen)"
  printf '%s\n' 'paths:' '  - scripts/juez.sh' >> "$yml"
  must_fail "V1: el self-test de un .sh editado, declarado frozen" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" v2 "$base" written)"
  printf '%s\n' 'paths:' '  - scripts/juez.sh' >> "$yml"
  must_pass "V2: ese mismo arbol declarado written: la pasada roja de un .sh" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" v3 "$base" frozen)"
  must_fail "V3: frozen sin paths, el arbol entero incluye los .sh" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" v4 "$base" frozen)"
  printf '%s\n' 'paths:' '  - scripts/juez-fn.sh' >> "$yml"
  must_pass "V4: frozen con paths a otro script, que no responde del editado" "$d" "$yml" || bad=1

  # V5-V6: solo produccion, fuera de toda region.
  st_restaurar "$d"
  sed -i 's/\* 2 ))/* 20 ))/' "$d/scripts/juez.sh"
  sed -i 's/\* 3 ))/* 30 ))/' "$d/scripts/juez-fn.sh"
  echo 'echo mas' >> "$d/scripts/plano.sh"
  yml="$(mk_handoff "$d" v5 "$base" frozen)"
  printf '%s\n' 'paths:' '  - scripts' >> "$yml"
  must_pass "V5: produccion de tres scripts tocada fuera de las regiones, frozen" "$d" "$yml" || bad=1
  yml="$(mk_handoff "$d" v6 "$base" written)"
  printf '%s\n' 'paths:' '  - scripts' >> "$yml"
  must_fail "V6: ese mismo arbol declarado written, sin haber escrito un test" "$d" "$yml" || bad=1

  # V7-V8: el ayudante y el despacho son parte del test, aunque no esten
  # dentro de self_test().
  st_restaurar "$d"
  sed -i 's/^must_be() {.*$/must_be() { return 0; }/' "$d/scripts/juez-fn.sh"
  yml="$(mk_handoff "$d" v7 "$base" frozen)"
  printf '%s\n' 'paths:' '  - scripts/juez-fn.sh' >> "$yml"
  must_fail "V7: el ayudante de la funcion pasa a return 0, declarado frozen" "$d" "$yml" || bad=1
  st_restaurar "$d"
  sed -i 's/^  --self-test) self_test ;;$/  --self-test) echo "OK  self-test: triple" ;;/' "$d/scripts/juez-fn.sh"
  yml="$(mk_handoff "$d" v8 "$base" frozen)"
  printf '%s\n' 'paths:' '  - scripts/juez-fn.sh' >> "$yml"
  must_fail "V8: el brazo del case imprime el OK sin correr nada, declarado frozen" "$d" "$yml" || bad=1

  # V9-V10: un script nuevo, sin versionar. El sin marcar es rojo AUNQUE otro
  # test se movio, y tiene que decir por que.
  st_restaurar "$d"
  grep -vxF -e "$st_ini" -e "$st_fin" "$d/scripts/juez.sh" > "$d/scripts/nuevo.sh"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  yml="$(mk_handoff "$d" v9 "$base" written)"
  printf '%s\n' 'paths:' '  - scripts/nuevo.sh' '  - rust/tests/contrato.rs' >> "$yml"
  must_fail_saying "V9: un script nuevo con --self-test sin marcar, aunque un test se movio" \
    "fuera de toda region marcada" "$d" "$yml" || bad=1
  st_restaurar "$d"
  cp "$d/scripts/juez.sh" "$d/scripts/nuevo.sh"
  yml="$(mk_handoff "$d" v10 "$base" written)"
  printf '%s\n' 'paths:' '  - scripts/nuevo.sh' >> "$yml"
  must_pass "V10: un script nuevo con su self-test marcado, declarado written" "$d" "$yml" || bad=1

  # V11-V14: marcadores mal puestos, con un test movido a proposito: sin la
  # regla, written se cumpliria y la fila fallaria por casualidad.
  st_restaurar "$d"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  sed -i "/^$st_fin\$/d" "$d/scripts/juez.sh"
  yml="$(mk_handoff "$d" v11 "$base" written)"
  printf '%s\n' 'paths:' '  - scripts/juez.sh' '  - rust/tests/contrato.rs' >> "$yml"
  must_fail "V11: un begin sin su end, aunque un test se movio" "$d" "$yml" || bad=1
  st_restaurar "$d"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  sed -i -e "s/^$st_ini\$/@@T@@/" -e "s/^$st_fin\$/$st_ini/" -e "s/^@@T@@\$/$st_fin/" "$d/scripts/juez.sh"
  must_fail "V12: el end va antes que el begin, aunque un test se movio" "$d" "$yml" || bad=1
  st_restaurar "$d"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  sed -i "/^$st_ini\$/a $st_ini" "$d/scripts/juez.sh"
  must_fail "V13: un segundo begin dentro de la region, aunque un test se movio" "$d" "$yml" || bad=1
  st_restaurar "$d"
  sed -i 's/assert!(true)/assert!(false)/' "$d/rust/tests/contrato.rs"
  sed -i "/^case /,\$ { /^$st_ini\$/d; /^$st_fin\$/d; }" "$d/scripts/juez-fn.sh"
  yml="$(mk_handoff "$d" v14 "$base" written)"
  printf '%s\n' 'paths:' '  - scripts/juez-fn.sh' '  - rust/tests/contrato.rs' >> "$yml"
  must_fail "V14: el brazo del case fuera de toda region, aunque un test se movio" "$d" "$yml" || bad=1

  # -- CI-E6: el coste del traspaso y de la spec ------------------------------
  # Un solo repo para toda la seccion, restaurado entre grupos desde copias como la
  # de `sh`. Cada fila arranca de un traspaso honesto y rompe UNA cosa, para que
  # el rojo sea de esa y de ninguna otra. Va antes de V15.
  d="$st_tmp/e6"; base="$(mk_repo "$d" false)"
  mkdir -p "$st_tmp/pristino-e6" "$st_tmp/otro-arbol/.cursor/handoffs"
  cp "$d/scripts/juez.sh" "$d/scripts/gate-dag.txt" "$d/rust/src/cosa.rs" "$st_tmp/pristino-e6/"
  local seg='segundos: 0.84 s; cargo test --workspace --test local_gate_contract e6_; test result: ok. 3 passed; 0 failed; finished in 0.84s'
  local carril='carril: DBG; comparte: target:T_dbg'
  local h="$d/.cursor/handoffs"

  # Implementador, tocando un script del juez fuera de su region.
  sed -i 's/\* 2 ))/* 20 ))/' "$d/scripts/juez.sh"
  mk_yml "$h/c1.yml" implementador mejorador git_handoff "$base" "fixture" "$(ev_bloque "$seg" "$carril")" frozen
  must_pass "E6: un traspaso honesto del implementador que toca el juez, con segundos y carril" "$d" "$h/c1.yml" || bad=1
  mk_yml "$h/c2.yml" implementador mejorador git_handoff "$base" "fixture" "$(ev_bloque "$carril")" frozen
  must_fail_saying_all "E6: sin linea segundos" "$d" "$h/c2.yml" "FALTA segundos" || bad=1
  mk_yml "$h/c3.yml" implementador mejorador git_handoff "$base" "fixture" '"segundos: 0.84 s; cargo test --test x"' frozen
  must_fail_saying_all "E6: segundos dentro de un evidence de una linea no cuenta" "$d" "$h/c3.yml" \
    "FALTA segundos" "evidence: |" || bad=1
  mk_yml "$h/c4.yml" implementador mejorador git_handoff "$base" "fixture" "$(ev_bloque 'segundos: rapido s; cargo test --test x' "$carril")" frozen
  must_fail_saying_all "E6: segundos sin numero" "$d" "$h/c4.yml" "segundos invalido" || bad=1
  mk_yml "$h/c5.yml" implementador mejorador git_handoff "$base" "fixture" \
    "$(ev_bloque 'segundos: 0.5 s; cargo test --test x; test result: ok. 1 passed; finished in 0.84s' "$carril")" frozen
  must_fail_saying_all "E6: segundos que no cuadra con su finished in" "$d" "$h/c5.yml" "no cuadra" "0.5" "0.84" || bad=1
  mk_yml "$h/c6.yml" implementador mejorador git_handoff "$base" "fixture" "$(ev_bloque "$seg" 'carril: XYZ; comparte: nada')" frozen
  must_fail_saying_all "E6: un carril que no es una fila de gate-dag.txt" "$d" "$h/c6.yml" \
    "XYZ no es una fila de scripts/gate-dag.txt" "P DBG" || bad=1
  mk_yml "$h/c7.yml" implementador mejorador git_handoff "$base" "fixture" "$(ev_bloque "$seg" 'carril: DBG')" frozen
  must_fail_saying_all "E6: un carril sin comparte" "$d" "$h/c7.yml" "comparte" || bad=1

  # Implementador que solo toca la tabla del juez: no es .rs ni .sh, y ningun
  # test se mueve. Con la lista de candidatos de written/frozen esto pasaria.
  cp "$st_tmp/pristino-e6/juez.sh" "$d/scripts/juez.sh"
  echo '30 X P - normal 1 1 1 true' >> "$d/scripts/gate-dag.txt"
  mk_yml "$h/c8.yml" implementador mejorador git_handoff "$base" "fixture" "$(ev_bloque "$seg")" frozen
  must_fail_saying_all "E6: tocar scripts/ sin carril" "$d" "$h/c8.yml" "FALTA carril" "scripts/gate-dag.txt" || bad=1

  # Implementador que no toca scripts/: sin carril esta bien.
  cp "$st_tmp/pristino-e6/gate-dag.txt" "$d/scripts/gate-dag.txt"
  sed -i 's/^    41$/    42/' "$d/rust/src/cosa.rs"
  mk_yml "$h/c9.yml" implementador mejorador git_handoff "$base" "fixture" "$(ev_bloque "$seg")" frozen
  must_pass "E6: un traspaso que no toca scripts/ no necesita carril" "$d" "$h/c9.yml" || bad=1

  # Fuera del alcance: una nota del implementador y el traspaso del mejorador.
  mk_yml "$h/c10.yml" implementador mejorador note "$base" "fixture" '"un test parece incorrecto"' frozen
  must_pass "E6: una nota del implementador no se juzga por coste" "$d" "$h/c10.yml" || bad=1
  mk_yml "$h/c11.yml" mejorador arquitecto git_handoff "$base" "fixture" '"sin segundos"' frozen
  must_pass "E6: el traspaso del mejorador no se juzga por coste" "$d" "$h/c11.yml" || bad=1

  # La spec. El .yml honesto y su spec van FUERA de ROOT/.cursor/handoffs: se
  # resuelve junto al .yml juzgado, no bajo la raiz del guardia.
  local o="$st_tmp/otro-arbol/.cursor/handoffs"
  mk_yml "$o/fuera.yml" especificador implementador git_handoff none \
    "CI-E6 fixture. Spec: .cursor/handoffs/fuera.spec.md" '"none (solo lectura)"'
  mk_spec "$o/fuera.spec.md"
  must_pass "E6: una spec honesta, junto a su .yml y fuera de la raiz del guardia" "$d" "$o/fuera.yml" || bad=1
  mk_yml "$h/s1.yml" especificador implementador note none "fixture sin spec" '"none (solo lectura)"'
  must_fail_saying_all "E6: un traspaso del especificador sin spec" "$d" "$h/s1.yml" "FALTA spec" || bad=1
  mk_yml "$h/s2.yml" especificador implementador git_handoff none \
    "fixture. Spec: .cursor/handoffs/otra.spec.md" '"none (solo lectura)"'
  must_fail_saying_all "E6: una spec nombrada que no existe junto al handoff" "$d" "$h/s2.yml" "otra.spec.md" "no existe" || bad=1
  mk_yml "$h/e6.yml" especificador implementador git_handoff none \
    "fixture. Spec: .cursor/handoffs/e6.spec.md" '"none (solo lectura)"'
  mk_spec "$h/e6.spec.md"
  sed -i '/^| Escenario /,$d' "$h/e6.spec.md"
  must_fail_saying_all "E6: una spec sin tabla de coste" "$d" "$h/e6.yml" \
    "tabla de coste" "| Escenario | Nivel | Segundos | Carril | Por qué no el barato |" || bad=1
  mk_spec "$h/e6.spec.md" "" "" "  Escenario: C"
  must_fail_saying_all "E6: un escenario sin fila" "$d" "$h/e6.yml" "escenario sin fila de coste: C" || bad=1
  mk_spec "$h/e6.spec.md"
  echo '| D | pura | 0,01 | DBG | — |' >> "$h/e6.spec.md"
  must_fail_saying_all "E6: una fila sin escenario" "$d" "$h/e6.yml" "fila de coste sin escenario: D" || bad=1
  mk_spec "$h/e6.spec.md" '| A | unitario | 0,01 | DBG | — |'
  must_fail_saying_all "E6: un nivel fuera de la lista" "$d" "$h/e6.yml" "nivel invalido" "pura|guion|servicio|e2e" || bad=1
  mk_spec "$h/e6.spec.md" "" '| B | guion | 0.06 | DBG | — |'
  must_fail_saying_all "E6: el nivel caro sin su porqué" "$d" "$h/e6.yml" "'B'" "por qué el barato no lo demuestra" || bad=1
  mk_spec "$h/e6.spec.md" '| A | pura | poco | DBG | — |'
  must_fail_saying_all "E6: segundos invalidos en una fila" "$d" "$h/e6.yml" "segundos invalido" "'A'" || bad=1
  mk_spec "$h/e6.spec.md" '| A | pura | 0,01 | XYZ | — |'
  must_fail_saying_all "E6: un carril invalido en una fila" "$d" "$h/e6.yml" "XYZ no es una fila de scripts/gate-dag.txt" || bad=1
  mk_spec "$h/e6.spec.md"
  # shellcheck disable=SC2016  # the backticks are the fence of the block, for sed to read
  sed -i -e '/^[`]\{3\}gherkin$/,/^[`]\{3\}$/d' -e '/^| [AB] |/d' "$h/e6.spec.md"
  must_fail_saying_all "E6: una spec sin escenarios" "$d" "$h/e6.yml" "sin escenarios" || bad=1

  # Los historicos: una lista cerrada por el hash de su contenido sin CR.
  local lista="$d/scripts/handoffs-sin-coste.txt" cab='# fixture: sha256 nombre caducidad dueño motivo'
  mk_yml "$h/h1.yml" implementador mejorador git_handoff "$base" "historico" '"sin segundos"' frozen
  printf '%s\n' "$cab" "$(st_hash "$h/h1.yml") h1.yml 9999-12-31 orquestador anterior a CI-E6" > "$lista"
  must_pass "E6: un traspaso historico del implementador, listado, pasa sin coste" "$d" "$h/h1.yml" || bad=1
  mk_yml "$h/h2.yml" especificador implementador git_handoff none "historico sin spec" '"none"'
  printf '%s\n' "$cab" "$(st_hash "$h/h2.yml") h2.yml 9999-12-31 orquestador anterior a CI-E6" > "$lista"
  must_pass "E6: un traspaso historico del especificador, listado, pasa sin spec" "$d" "$h/h2.yml" || bad=1
  sed 's/$/\r/' "$h/h1.yml" > "$h/h3.yml"
  printf '%s\n' "$cab" "$(st_hash "$h/h1.yml") h3.yml 9999-12-31 orquestador anterior a CI-E6" > "$lista"
  must_pass "E6: el mismo historico en CRLF, listado por su hash LF" "$d" "$h/h3.yml" || bad=1
  sed -i 's/historico/historica/' "$h/h1.yml"
  printf '%s\n' "$cab" "$(st_hash "$h/h3.yml") h1.yml 9999-12-31 orquestador anterior a CI-E6" > "$lista"
  must_fail_saying_all "E6: un historico editado deja de ser historico" "$d" "$h/h1.yml" "FALTA segundos" || bad=1
  mk_yml "$h/h1.yml" implementador mejorador git_handoff "$base" "historico" '"sin segundos"' frozen
  printf '%s\n' "$cab" "$(st_hash "$h/h1.yml") h1.yml 2000-01-01 orquestador anterior a CI-E6" > "$lista"
  must_fail_saying_all "E6: una excepcion caducada no exime" "$d" "$h/h1.yml" "caducó el 2000-01-01" || bad=1
  printf '%s\n' "$cab" "$(st_hash "$h/h1.yml") h1.yml 9999-12-31 orquestador anterior a CI-E6" \
    "$(printf '%064d' 7) otro.yml 9999-12-31" > "$lista"
  must_fail_saying_all "E6: una linea malformada de la lista, despues de la que casa" "$d" "$h/h1.yml" \
    "handoffs-sin-coste.txt:3:" "malformada" || bad=1

  # V15: el mismo V1 en el arbol CRLF. Va aqui y no junto a frozen-crlf-ok para
  # que la primera fila roja de un guardia viejo sea V1; el repo crlf sigue ahi,
  # y su frozen-crlf-ok ya paso antes.
  d="$st_tmp/crlf"
  sed -i 's/== "4"/== "5"/' "$d/scripts/juez.sh"
  yml="$(mk_handoff "$d" v15 "$(git -C "$d" rev-parse HEAD)" frozen)"
  printf '%s\n' 'paths:' '  - scripts/juez.sh' >> "$yml"
  must_fail "V15: el self-test de un .sh editado en un arbol CRLF, declarado frozen" "$d" "$yml" || bad=1

  if [[ "$bad" -ne 0 ]]; then
    echo "FAIL self-test: al menos un guardia ya no decide nada." >&2
    return 1
  fi
  # El contrato se ancla en esta etiqueta, no en la frase: un veredicto que
  # nombra sus fixtures se reescribe cada vez que se anade uno, y eso es lo que
  # puso en rojo el mismo assert sobre la mitad CRAP con un guion que pasaba.
  echo "OK  self-test: cada guardia de validar-handoff fallo contra un fixture construido"
  echo "    para romperla, y cada handoff honesto paso"
  return 0
}
# --- self-test end ---

case "${1:-}" in
# --- self-test begin ---
  --self-test) self_test ;;
# --- self-test end ---
  -h | --help) usage ;;
  "") usage >&2; exit 1 ;;
  *) juzgar "$1" ;;
esac
