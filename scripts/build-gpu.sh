#!/usr/bin/env bash
# Release build with GPU support, under a memory cap.
#
# Two reasons this exists as a script instead of a line in the README:
#
#   1. Without --features cuda the query embedding runs on CPU. bge-m3 is 568M
#      parameters and that alone takes cuba_faro's median from 0.451s to 4.23s
#      — 9.4x, on every single search. A plain `cargo build --release` silently
#      throws that away.
#   2. A release link with lto="fat" and codegen-units=1 pulls several GB at
#      once. Run unconstrained on a 14.9 GB laptop with editors open, it takes
#      the machine down; it has done so twice.
set -euo pipefail

MEM_MAX="${CUBA_BUILD_MEM_MAX:-5G}"
CPU_QUOTA="${CUBA_BUILD_CPU_QUOTA:-400%}"
JOBS="${CUBA_BUILD_JOBS:-3}"

cd "$(dirname "$0")/../rust"

if ! command -v nvidia-smi >/dev/null 2>&1; then
    echo "warning: no nvidia-smi on PATH — building with the cuda feature anyway."
    echo "         The binary still runs on CPU; it just carries the provider."
fi

echo "building release + cuda (MemoryMax=$MEM_MAX, jobs=$JOBS)"

if command -v systemd-run >/dev/null 2>&1; then
    systemd-run --user --scope -q \
        -p MemoryMax="$MEM_MAX" -p CPUQuota="$CPU_QUOTA" \
        nice -n 15 cargo build --release --features cuda -j "$JOBS"
else
    echo "note: systemd-run unavailable, building without a memory cap"
    nice -n 15 cargo build --release --features cuda -j "$JOBS"
fi

if [[ -f target/release/memory-industry.exe ]]; then
    BIN="target/release/memory-industry.exe"
elif [[ -f target/release/memory-industry ]]; then
    BIN="target/release/memory-industry"
elif [[ -f target/release/cuba-memorys.exe ]]; then
    BIN="target/release/cuba-memorys.exe"
else
    BIN="target/release/cuba-memorys"
fi
echo
"$BIN" --version
echo
echo "point your MCP client at: $(pwd)/$BIN"
echo "confirm the GPU is live with: $BIN doctor | grep -i gpu"
