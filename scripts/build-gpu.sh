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

# Parallelism derived from the machine, not pinned to a number.
#
# The values these replace were measured on a 14.9 GB laptop, and a constant
# calibrated on one machine and then applied to every other one is exactly the
# defect this release spent its time removing from the GPU path. The formula
# below reproduces the old hand-tuned figure on the machine it was tuned for
# (14.9 GB / 4 = 3) and scales from there.
machine_cores() { nproc 2>/dev/null || echo 4; }

machine_ram_gb() {
  if [[ -r /proc/meminfo ]]; then
    awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo
    return
  fi
  powershell.exe -NoProfile -Command \
    '[int]((Get-CimInstance Win32_OperatingSystem).TotalVisibleMemorySize/1MB)' 2>/dev/null |
    tr -d '\r' | grep -E '^[0-9]+$' || echo 8
}
# A release link with lto="fat" is the memory peak and it is single-threaded
# anyway; the parallelism buys the several hundred dependency crates. Four GB
# per lane is the headroom that keeps the peak from meeting the ceiling.
default_build_jobs() {
  local cores ram per_ram
  cores=$(machine_cores)
  ram=$(machine_ram_gb)
  per_ram=$(( ram / 4 ))
  (( per_ram < 1 )) && per_ram=1
  (( cores < per_ram )) && { echo "$cores"; return; }
  echo "$per_ram"
}
JOBS="${CUBA_BUILD_JOBS:-$(default_build_jobs)}"

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

# cargo writes here when Cursor/sandbox sets CARGO_TARGET_DIR. A hardcoded
# rust/target/release then 127s on `cuba-memorys` after a six-minute link.
td="${CARGO_TARGET_DIR:-target}"
BIN=""
for cand in \
  "$td/release/memory-industry.exe" \
  "$td/release/memory-industry" \
  "$td/release/cuba-memorys.exe" \
  "$td/release/cuba-memorys"
do
  if [[ -f "$cand" ]]; then
    BIN="$cand"
    break
  fi
done
if [[ -z "$BIN" ]]; then
  echo "FAIL: no release binary under $td/release after cargo build." >&2
  exit 1
fi
echo
"$BIN" --version
echo
echo "point your MCP client at: $BIN"
echo "confirm the GPU is live with: $BIN doctor | grep -i gpu"
