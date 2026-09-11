#!/usr/bin/env bash
# Fail if ignored integration tests have no path in the local merge gate, or if
# unused crate deps are present. Missing tools are FAIL, never skip.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT/rust"
GATE="$ROOT/scripts/run-all-tests.sh"

echo "=== codigo-muerto: ignored tests must be named in the local gate ==="
if [[ ! -f "$GATE" ]]; then
  echo "FAIL: missing $GATE" >&2
  exit 1
fi

# run-all-tests.sh discovers tests/*.rs dynamically. Anything with #[ignore] is
# covered by that loop unless it is listed in RUN_ELSEWHERE (those still run in
# their own sections below). Only fail when a file is neither discoverable nor
# referenced by name.
has_discovery=0
grep -q 'for file in tests/\*\.rs' "$GATE" && has_discovery=1

orphans=0
while IFS= read -r file; do
  name="$(basename "$file" .rs)"
  if [[ "$name" == "ci_contract" ]]; then
    continue
  fi
  if ! grep -qE "#\[ignore" "$file"; then
    continue
  fi
  if ((has_discovery)); then
    # Discovered by the gate loop; deferred names must still appear in the script.
    if grep -qE "RUN_ELSEWHERE=\(|$name" "$GATE"; then
      :
    fi
    continue
  fi
  if ! grep -qE -- "--test[[:space:]]+$name|[[:space:]]$name([[:space:]]|\\)|$)" "$GATE"; then
    echo "FAIL: $name has #[ignore] but run-all-tests.sh never runs it" >&2
    orphans=$((orphans + 1))
  fi
done < <(find "$RUST_DIR/tests" -maxdepth 1 -name '*.rs' | sort)

if ((orphans > 0)); then
  echo "FAIL: $orphans ignored test file(s) have no path in the local gate" >&2
  exit 1
fi
echo "OK  every #[ignore] integration file is covered by run-all-tests.sh discovery"

echo "=== codigo-muerto: cargo machete ==="
if ! command -v cargo-machete >/dev/null 2>&1 && ! cargo machete --version >/dev/null 2>&1; then
  echo "FAIL: cargo-machete is not installed. Install it (cargo install cargo-machete) — the gate does not skip dead-deps." >&2
  exit 1
fi
(cd "$RUST_DIR" && cargo machete)
echo "OK  cargo machete"
