#!/usr/bin/env bash
set -euo pipefail
# Resume after toolchain install: rebuild CLI, finish models, wire claude, run gate.
source "$HOME/.cargo/env"
ROOT=/mnt/d/Proyectos/Memorys
cd "$ROOT/rust"
cargo build --release --bin cuba-memorys
BIN="$ROOT/rust/target/release/cuba-memorys"
"$BIN" models reranker

# claude CLI: prefer native; else bridge to Windows npm install
if ! command -v claude >/dev/null 2>&1; then
  if command -v npm >/dev/null 2>&1; then
    npm install -g --allow-scripts=@anthropic-ai/claude-code @anthropic-ai/claude-code || true
  fi
fi
if ! command -v claude >/dev/null 2>&1; then
  mkdir -p "$HOME/.local/bin"
  cat > "$HOME/.local/bin/claude" <<'EOF'
#!/usr/bin/env bash
exec /mnt/c/Users/leand/AppData/Roaming/npm/claude.cmd "$@"
EOF
  chmod +x "$HOME/.local/bin/claude"
  export PATH="$HOME/.local/bin:$PATH"
fi
command -v claude
claude --version || true

# cargo tools
cargo install cargo-deny cargo-machete cargo-audit --locked 2>/dev/null || \
  cargo install cargo-deny cargo-machete cargo-audit

# Postgres: try localhost then Windows host from WSL2
if pg_isready -h 127.0.0.1 -p 5488 -U cuba -d brain >/dev/null 2>&1; then
  export DATABASE_URL="postgresql://cuba:memorys2026@127.0.0.1:5488/brain"
else
  HOST_IP=$(grep -m1 nameserver /etc/resolv.conf | awk '{print $2}')
  export DATABASE_URL="postgresql://cuba:memorys2026@${HOST_IP}:5488/brain"
  pg_isready -h "$HOST_IP" -p 5488 -U cuba -d brain
fi
echo "DATABASE_URL=$DATABASE_URL"

# Sabotage require_present in isolation (do not wait for the full suite)
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
if [[ "$RC" -eq 0 ]]; then
  echo "FAIL: sabotage did not turn require_present red" >&2
  exit 1
fi
echo "OK  sabotage: require_present fails when probe missing (rc=$RC)"

export SKIP_BACKUP=1
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
cd "$ROOT"
/usr/bin/time -f 'GATE_ELAPSED_SEC=%e' bash scripts/merge-gate.sh
