#!/usr/bin/env bash
# Same judge as merge-gate.sh. Do not treat this as a second CI.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
echo "=== quality-gate → scripts/merge-gate.sh (local CI, sole merge judge) ==="
echo "NO MIRA: nothing beyond what merge-gate.sh declares in its alcance block."
exec "$ROOT/scripts/merge-gate.sh" "$@"
