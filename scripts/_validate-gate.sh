#!/usr/bin/env bash
# Validate local merge gate to 100%: sabotage require_present, then full merge-gate.
set -euo pipefail
source "$HOME/.cargo/env"
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"

if ! command -v claude >/dev/null 2>&1; then
  mkdir -p "$HOME/.local/bin"
  printf '%s\n' '#!/usr/bin/env bash' 'exec /mnt/c/Users/leand/AppData/Roaming/npm/claude.cmd "$@"' >"$HOME/.local/bin/claude"
  chmod +x "$HOME/.local/bin/claude"
fi

# cargo-deny / machete / audit must exist (gate fails otherwise)
command -v cargo-deny >/dev/null 2>&1 || cargo install cargo-deny --locked
command -v cargo-machete >/dev/null 2>&1 || cargo install cargo-machete --locked
command -v cargo-audit >/dev/null 2>&1 || cargo install cargo-audit --locked

if pg_isready -h 127.0.0.1 -p 5488 -U cuba -d brain >/dev/null 2>&1; then
  export DATABASE_URL="postgresql://cuba:memorys2026@127.0.0.1:5488/brain"
else
  HOST_IP=$(grep -m1 nameserver /etc/resolv.conf | awk '{print $2}')
  export DATABASE_URL="postgresql://cuba:memorys2026@${HOST_IP}:5488/brain"
  pg_isready -h "$HOST_IP" -p 5488 -U cuba -d brain
fi
export SKIP_BACKUP=1

echo "=== preflight models ==="
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/cuba-memorys"
for p in \
  "$CACHE/models/model_quantized.onnx" \
  "$CACHE/onnxruntime/libonnxruntime.so" \
  "$CACHE/models-nli" \
  "$CACHE/reranker/model.onnx" \
  "$CACHE/reranker/model.onnx_data"
do
  [[ -e "$p" ]] || { echo "FAIL missing $p — run: cuba-memorys models all"; exit 1; }
  echo "OK  $p"
done
command -v claude
command -v node
command -v psql

echo "=== sabotage require_present ==="
set +e
bash -c '
require_present() {
  local what="$1" probe="$2"; shift 2
  if [[ ! -e "$probe" ]]; then
    echo "FAIL: $what — nothing at $probe" >&2
    exit 1
  fi
  "$@"
}
require_present "sabotage" /no/such/model.onnx true
'
RC=$?
set -e
[[ "$RC" -ne 0 ]] || { echo "FAIL: sabotage stayed green"; exit 1; }
echo "OK  sabotage rc=$RC"

cd /mnt/d/Proyectos/Memorys
echo "=== full merge-gate ==="
/usr/bin/time -f 'GATE_ELAPSED_SEC=%e' bash scripts/merge-gate.sh
echo "=== VALIDATION COMPLETE: merge-gate 100% ==="
