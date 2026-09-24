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
set -euo pipefail

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

filtrar_region() { # <ruta-relativa>, contenido ya normalizado por stdin
  case "$1" in
    # Un .rs bajo tests/ es test de arriba abajo. El crate tiene 81 y solo 3
    # llevan #[cfg(test)]: extraer por atributo dejaria 78 ficheros de test
    # invisibles para `frozen`, que es el agujero mas grande que cabria aqui.
    */tests/*.rs | tests/*.rs) cat ;;
    *) regiones_cfg_test ;;
  esac
}

# El CR se quita AQUI, en un solo sitio y delante de todo.
#
# Medido 2026-09-22: en este arbol los blobs y los ficheros estan en LF, pero
# core.autocrlf=true y git mismo avisa «LF will be replaced by CRLF the next
# time Git touches it» — no hay linea *.rs en .gitattributes que lo impida. En
# cuanto git toque estos ficheros, el ARBOL queda en CRLF y el COMMIT en LF, y
# entonces cada fichero difiere de su blob SOLO por los finales de linea:
# `frozen` acusaria a toda pasada verde honesta. Medido en el fixture:
# rust/tests/contrato.rs son 62 B en el arbol contra 59 B en el commit.
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

tests_tocados() { # <commit> [ruta...] -> imprime los ficheros cuya region cambio
  local commit="$1" f a b ficheros
  shift
  # `git diff` no ve lo que no esta trackeado, y la pasada roja estrena ficheros
  # de test a menudo. Sin ls-files --others, `written` diria que no se escribio
  # ningun test justo el dia que se escribio uno nuevo.
  ficheros="$( {
    git -C "$ROOT" diff --name-only "$commit" -- '*.rs' 2>/dev/null
    git -C "$ROOT" ls-files --others --exclude-standard -- '*.rs' 2>/dev/null
  } | sort -u )"
  [[ -n "$ficheros" ]] || return 0
  while IFS= read -r f; do
    [[ -n "$f" ]] || continue
    en_la_tajada "$f" "$@" || continue
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
    local cambiados
    cambiados="$(tests_tocados "$commit" "${paths[@]}")"
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

  echo "OK $path $from->$to"
  return 0
}

# --------------------------------------------------------------- el self-test

# Un guardia que no se ve fallar es un parrafo, que es literalmente lo que este
# fichero era hasta hoy. Cada guardia contra un fixture construido para
# romperla, y cada caso honesto contra uno que debe pasar.

st_tmp=""
st_limpiar() { [[ -n "$st_tmp" && -d "$st_tmp" ]] && rm -rf "$st_tmp"; return 0; }

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

  # Rutas explicitas: `git add -A` arrastra CRLF fantasma y ficheros ajenos, y
  # en este repo lo bloquea la guardia de comandos.
  git -C "$d" add rust/src/cosa.rs rust/tests/contrato.rs scripts/validar-handoff.sh >/dev/null 2>&1
  git -C "$d" commit -q -m "base" >/dev/null 2>&1

  # El arbol se convierte a CRLF DESPUES del commit, que es como pasa de verdad:
  # se escribe CRLF, git lo normaliza al commitear y el arbol de trabajo queda
  # inconsistente con el blob. El commit se queda en LF y el arbol en CRLF.
  if [[ "$crlf" == "true" ]]; then
    sed -i 's/$/\r/' "$d/rust/src/cosa.rs" "$d/rust/tests/contrato.rs"
    grep -qU "$(printf '\r')" "$d/rust/tests/contrato.rs" || {
      echo "FAIL self-test: el fixture CRLF no tiene CRLF, asi que no prueba nada." >&2
      return 1
    }
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
    echo 'evidence: "fixture del self-test"'
    [[ "$t" == "-" ]] || echo "tests: $t"
  } > "$d/.cursor/handoffs/$nombre.yml"
  printf '%s\n' "$d/.cursor/handoffs/$nombre.yml"
}

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

case "${1:-}" in
  --self-test) self_test ;;
  -h | --help) usage ;;
  "") usage >&2; exit 1 ;;
  *) juzgar "$1" ;;
esac
