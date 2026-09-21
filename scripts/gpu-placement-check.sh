#!/usr/bin/env bash
# Does `doctor --json`'s `gpu` check agree with the machine it ran on?
#
# A CUDA build that falls back to the CPU is invisible from the outside: the
# daemon starts, every search answers, and the only difference is that the
# reranker costs 58x (measured in d7922fa: 20.669s per query against 0.356s).
# The gate has always built with --features cuda and then asserted nothing
# about where the work landed, which is how an installation can believe it is
# on a card for months.
#
# What this can and cannot prove: with no GPU on the merge machine nobody can
# show that a kernel executed on one. The *decision* is provable anywhere, and
# the decision is where the failure was. So this compares two independent
# readings of the same machine — what the filesystem says (an NVIDIA device,
# the ONNX Runtime provider libraries) and what doctor reports — and fails on
# the contradiction. A machine with no card reporting CPU is not a failure: it
# is the correct answer, and it has to stay green here or nobody will keep the
# step.
set -euo pipefail

usage() {
  cat <<'TXT'
usage:
  gpu-placement-check.sh <doctor-json-file>   judge one `doctor --json` output
  gpu-placement-check.sh --self-test          prove each guard can still fail

The JSON is read from a file rather than stdin because `doctor` exits 1 when
ANY of its checks fails, and the caller has to keep both the output and that
exit code to say what went wrong.
TXT
}

# ---------------------------------------------------------------- the decision

# verdict <can_gpu 0|1> <asked gpu|cpu|unset> <status> <detail> <hint>
#
# Pure: it decides from its arguments alone. That is what lets --self-test put
# a machine and a doctor answer side by side that no real machine would ever
# produce, and watch this refuse them.
#
# Prints the contradiction and returns 1, or returns 0 having said nothing.
verdict() {
  local can_gpu="$1" asked="$2" status="$3" detail="$4" hint="$5"
  local lower

  if [[ -z "$status" || -z "$detail" ]]; then
    echo "the gpu check carries no status or no detail (status='$status' detail='$detail')."
    echo "An unreadable check is indistinguishable from a healthy one, which is the"
    echo "whole failure mode this step exists for."
    return 1
  fi

  lower="$(printf '%s' "$detail" | tr '[:upper:]' '[:lower:]')"

  if [[ "$can_gpu" == "1" && "$status" != "ok" ]]; then
    echo "this machine has an NVIDIA device AND the ONNX Runtime GPU provider libraries,"
    echo "and the gpu check still reports '$status':"
    echo "  $detail"
    if [[ -n "$hint" ]]; then
      echo "  hint: $hint"
    fi
    echo "A build that lands on the CPU here is the silent degradation this step exists"
    echo "to make visible. Either the binary was not built with --features cuda (the gate"
    echo "builds it through scripts/build-gpu.sh), or the runtime cannot open the provider."
    return 1
  fi

  if [[ "$can_gpu" == "0" && "$lower" != *cpu* ]]; then
    echo "this machine has no usable GPU path — no NVIDIA device, or no"
    echo "onnxruntime_providers_* beside the runtime library — and the gpu check never"
    echo "names the CPU it is actually running on:"
    echo "  $detail"
    echo "A check that reports a device the machine does not have is the false green the"
    echo "rules of this repo forbid."
    return 1
  fi

  # `placement_summary()` in rust/src/gpu.rs writes `reranker=gpu|cpu` into the
  # detail. An operator who set the device variable by hand is entitled to see
  # their own answer come back; when the plan is the one choosing (the variable
  # is unset) there is nothing here to compare against and the block is skipped
  # out loud by the caller.
  if [[ "$asked" != "unset" && "$detail" == *"reranker="* && "$detail" != *"reranker=$asked"* ]]; then
    echo "the environment asked for reranker=$asked and the gpu check reports something else:"
    echo "  $detail"
    echo "An explicit device request that the running process does not honour is the"
    echo "defect that made an installation set CUBA_GPU_MEM_LIMIT_MB and never see it applied."
    return 1
  fi

  return 0
}

# ------------------------------------------------------- reading this machine

# Mirrors runtime_dir() in rust/src/gpu.rs: the providers live beside the
# runtime library, and ORT_DYLIB_PATH is what run-all-tests.sh exports.
ort_runtime_dir() {
  local cache
  if [[ -n "${ORT_DYLIB_PATH:-}" ]]; then
    dirname "$ORT_DYLIB_PATH"
    return 0
  fi
  cache="${XDG_CACHE_HOME:-${HOME:-${USERPROFILE:-}}/.cache}"
  if [[ -d "$cache/memory-industry/onnxruntime" ]]; then
    printf '%s\n' "$cache/memory-industry/onnxruntime"
    return 0
  fi
  printf '%s\n' "$cache/cuba-memorys/onnxruntime"
}

provider_lib_present() {
  local dir name
  dir="$(ort_runtime_dir)"
  for name in "$@"; do
    if [[ -e "$dir/$name" ]]; then
      return 0
    fi
  done
  return 1
}

nvidia_device_present() {
  if [[ -e /proc/driver/nvidia/version ]]; then
    return 0
  fi
  if command -v nvidia-smi >/dev/null 2>&1; then
    return 0
  fi
  return 1
}

# Same shape as gpu_availability() in rust/src/gpu.rs, for both feature sets:
# CUDA needs the provider libraries and a card, DirectML needs only its own.
machine_can_gpu() {
  if provider_lib_present libonnxruntime_providers_cuda.so \
                          onnxruntime_providers_cuda.dll \
                          libonnxruntime_providers_cuda.dylib \
                          onnxruntime_providers_cuda.so; then
    if nvidia_device_present; then
      printf '1\n'
      return 0
    fi
  fi
  if provider_lib_present onnxruntime_providers_dml.dll \
                          DirectML.dll \
                          libonnxruntime_providers_dml.so \
                          onnxruntime_providers_dml.so; then
    printf '1\n'
    return 0
  fi
  printf '0\n'
}

asked_device() {
  local raw normalized
  raw="${MEMORY_INDUSTRY_RERANK_DEVICE:-${CUBA_RERANK_DEVICE:-}}"
  normalized="$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$normalized" in
    gpu|cuda|directml) printf 'gpu\n' ;;
    cpu) printf 'cpu\n' ;;
    # An unrecognised value falls back to the model's own default inside
    # wants_gpu(), so there is no request here to hold anyone to.
    *) printf 'unset\n' ;;
  esac
}

# ------------------------------------------------------------ reading the JSON

# One check per line, so a field lookup cannot cross an object boundary. No
# early-exit filter at the end of the pipe: with `pipefail` on, a `head -1` that
# closes the pipe can SIGPIPE its producer and hand back status 141 for a line
# it did read, which would read here as "there is no gpu check".
gpu_check_object() {
  printf '%s' "$1" | tr -d '\n' | sed 's/},{/}\n{/g' | grep -F '"name":"gpu"'
}

json_string_field() {
  printf '%s' "$1" | sed -nE "s/.*\"$2\":\"([^\"]*)\".*/\1/p"
}

judge_file() {
  local path="$1" raw object status detail hint can_gpu asked
  if [[ ! -f "$path" ]]; then
    echo "FAIL: no doctor JSON at $path" >&2
    return 1
  fi
  raw="$(cat "$path")"
  object=""
  if ! object="$(gpu_check_object "$raw")"; then
    object=""
  fi
  object="${object%%$'\n'*}"
  if [[ -z "$object" ]]; then
    echo "FAIL: the doctor JSON has no check named \"gpu\"." >&2
    echo "      Without it this step asserts nothing at all, which is worse than not" >&2
    echo "      running: it prints a line that reads like a pass. What was read:" >&2
    printf '%.2000s\n' "$raw" >&2
    return 1
  fi

  status="$(json_string_field "$object" status)"
  detail="$(json_string_field "$object" detail)"
  hint="$(json_string_field "$object" hint)"
  can_gpu="$(machine_can_gpu)"
  asked="$(asked_device)"

  echo "machine: gpu-capable=$can_gpu (runtime dir $(ort_runtime_dir))"
  echo "asked:   reranker device=$asked"
  echo "doctor:  gpu=$status — $detail"
  if [[ "$asked" == "unset" || "$detail" != *"reranker="* ]]; then
    echo "note:    no explicit device request to compare, or no placement in the detail;"
    echo "         only the agreement between doctor and this machine is judged."
  fi

  local message rc=0
  message="$(verdict "$can_gpu" "$asked" "$status" "$detail" "$hint")" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "FAIL: doctor's gpu check contradicts this machine." >&2
    printf '%s\n' "$message" | sed 's/^/      /' >&2
    return 1
  fi
  echo "OK  gpu placement agrees with this machine"
  return 0
}

# ---------------------------------------------------------------- the self-test

must_fail() {
  local what="$1"
  shift
  local out rc=0
  out="$(verdict "$@")" || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    echo "SELF-TEST FAIL: $what — the checker accepted it." >&2
    return 1
  fi
  echo "  red, as it must be: $what"
  printf '%s\n' "$out" | sed 's/^/      /'
  return 0
}

must_pass() {
  local what="$1"
  shift
  local out rc=0
  out="$(verdict "$@")" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    echo "SELF-TEST FAIL: $what — the checker rejected an honest machine:" >&2
    printf '%s\n' "$out" >&2
    return 1
  fi
  echo "  green, as it must be: $what"
  return 0
}

self_test() {
  local bad=0
  local gpu_live="cuda — runtime GPU y GPU detectados · colocación: embedder=cpu reranker=gpu nli=cpu"
  local no_card="compilado con cuda, pero no detecté GPU NVIDIA → corriendo en CPU"
  local no_runtime="compilado con cuda, pero el runtime instalado es el de CPU → corriendo en CPU"
  local cpu_build="cpu (compilado sin soporte GPU)"

  echo "=== gpu-placement-check --self-test ==="

  must_fail "a card and the provider libraries are both there and doctor still says degraded" \
    1 unset warn "$no_card" "revisá el driver (nvidia-smi)" || bad=1
  must_fail "a machine with neither card nor libraries whose check never names the CPU" \
    0 unset ok "cuda — runtime GPU y GPU detectados · colocación: embedder=gpu reranker=gpu nli=gpu" "" || bad=1
  must_fail "an explicit CUBA_RERANK_DEVICE=cpu that the process did not honour" \
    1 cpu ok "$gpu_live" "" || bad=1
  must_fail "a gpu check with an empty detail" \
    1 unset ok "" "" || bad=1

  must_pass "a machine with no card, on a build without GPU support, reporting CPU" \
    0 unset ok "$cpu_build" "" || bad=1
  must_pass "a CUDA build on a machine whose runtime was installed without the providers" \
    0 gpu warn "$no_runtime" "memory-industry models runtime --gpu" || bad=1
  must_pass "a real GPU machine placing the reranker where it was asked to" \
    1 gpu ok "$gpu_live" "" || bad=1

  if [[ "$bad" -ne 0 ]]; then
    echo "SELF-TEST FAILED: at least one guard no longer decides anything." >&2
    return 1
  fi
  echo "OK  every guard in gpu-placement-check failed against a fixture built to break it,"
  echo "    and every honest machine passed"
  return 0
}

case "${1:-}" in
  --self-test) self_test ;;
  -h|--help) usage ;;
  "") usage >&2; exit 1 ;;
  *) judge_file "$1" ;;
esac
