#!/usr/bin/env bash
# Valida un YAML de handoff Swarm Forge. Exit 0 = ok, 1 = malformado.
set -euo pipefail
path="${1:?uso: validar-handoff.sh <fichero.yml>}"
[[ -f "$path" ]] || { echo "no existe: $path"; exit 1; }

roles='especificador|implementador|mejorador|arquitecto|endurecedor|qa'
val() { grep -E "^$1:" "$path" | head -1 | sed "s/^$1:[[:space:]]*//;s/[\"']//g" | tr -d '\r'; }

from=$(val from); to=$(val to); typ=$(val type)
task=$(val task); commit=$(val commit); evidence=$(val evidence)
for k in from to type task commit evidence; do
  eval "v=\$$k"
  [[ -n "${v:-}" ]] || { echo "FALTA campo: $k"; exit 1; }
done
echo "$from" | grep -Eqx "$roles" || { echo "from invalido: $from"; exit 1; }
echo "$to" | grep -Eqx "$roles" || { echo "to invalido: $to"; exit 1; }
[[ "$typ" == "git_handoff" || "$typ" == "note" ]] || { echo "type invalido"; exit 1; }
if [[ "$commit" != "none" && ! "$commit" =~ ^[0-9a-f]{7,40}$ ]]; then
  echo "commit invalido: $commit"; exit 1
fi
echo "OK $path $from->$to"
