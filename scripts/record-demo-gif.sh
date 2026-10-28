#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CAST="${ROOT}/assets/demo.cast"
GIF="${ROOT}/assets/demo-v0.30.gif"
export CUBA_BINARY_PATH="${CUBA_BINARY_PATH:-$ROOT/rust/target/release/memory-industry}"
export RECORD_DEMO=1

if [[ ! -x "$CUBA_BINARY_PATH" ]]; then
  echo "Building release binary..."
  (cd "$ROOT/rust" && cargo build --release)
fi

command -v asciinema >/dev/null || { echo "asciinema required"; exit 1; }
command -v agg >/dev/null || { echo "agg required (asciinema-agg)"; exit 1; }
command -v docker >/dev/null || { echo "docker required (demo.sh starts its own postgres)"; exit 1; }

mkdir -p "$ROOT/assets"
rm -f "$CAST"

echo "Recording terminal demo → $CAST"
asciinema rec "$CAST" \
  --command "bash -lc 'export TERM=xterm-256color; export RECORD_DEMO=1; bash \"$ROOT/scripts/demo.sh\"'" \
  --idle-time-limit 1.2 \
  --overwrite \
  --title "memory-industry demo"

echo "Rendering GIF → $GIF"
agg "$CAST" "$GIF" \
  --cols 100 \
  --rows 40 \
  --theme monokai \
  --font-family "JetBrains Mono,Fira Code,Consolas,DejaVu Sans Mono" \
  --speed 0.55 \
  --fps-cap 8 \
  --last-frame-duration 5 \
  --no-loop

rm -f "$CAST"
ls -lh "$GIF"
echo "Done. Commit assets/demo-v0.30.gif"
