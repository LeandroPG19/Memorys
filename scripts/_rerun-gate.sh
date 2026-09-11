#!/usr/bin/env bash
set -euo pipefail
source "$HOME/.cargo/env"
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
if ! command -v claude >/dev/null 2>&1; then
  mkdir -p "$HOME/.local/bin"
  printf '%s\n' '#!/usr/bin/env bash' 'exec /mnt/c/Users/leand/AppData/Roaming/npm/claude.cmd "$@"' > "$HOME/.local/bin/claude"
  chmod +x "$HOME/.local/bin/claude"
fi
if pg_isready -h 127.0.0.1 -p 5488 -U cuba -d brain >/dev/null 2>&1; then
  export DATABASE_URL="postgresql://cuba:memorys2026@127.0.0.1:5488/brain"
else
  HOST_IP=$(grep -m1 nameserver /etc/resolv.conf | awk '{print $2}')
  export DATABASE_URL="postgresql://cuba:memorys2026@${HOST_IP}:5488/brain"
fi
export SKIP_BACKUP=1
cd /mnt/d/Proyectos/Memorys
/usr/bin/time -f 'GATE_ELAPSED_SEC=%e' bash scripts/merge-gate.sh
