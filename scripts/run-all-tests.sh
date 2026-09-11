#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUST_DIR="$ROOT/rust"
LIVE_DATABASE_URL="${DATABASE_URL:-postgresql://cuba:memorys2026@127.0.0.1:5488/brain}"
export CUBA_JUDGE="${CUBA_JUDGE:-heuristic}"

CACHE_NEW="${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry"
CACHE_OLD="${XDG_CACHE_HOME:-$HOME/.cache}/cuba-memorys"
# Prefer the cache that actually has the embedder. An empty memory-industry/
# dir (e.g. only audit_key) must not steal paths from a populated cuba-memorys/.
if [[ -f "$CACHE_NEW/models/model_quantized.onnx" ]]; then
  CACHE="$CACHE_NEW"
elif [[ -f "$CACHE_OLD/models/model_quantized.onnx" ]]; then
  CACHE="$CACHE_OLD"
elif [[ -d "$CACHE_NEW" ]]; then
  CACHE="$CACHE_NEW"
elif [[ -d "$CACHE_OLD" ]]; then
  CACHE="$CACHE_OLD"
else
  CACHE="$CACHE_NEW"
fi
# Paths must match `memory-industry models all` (see rust/src/models_cli.rs).
export ONNX_MODEL_PATH="${ONNX_MODEL_PATH:-$CACHE/models}"
if [[ -z "${ORT_DYLIB_PATH:-}" ]]; then
  if [[ -f "$CACHE/onnxruntime/libonnxruntime.so" ]]; then
    export ORT_DYLIB_PATH="$CACHE/onnxruntime/libonnxruntime.so"
  elif [[ -f "$CACHE/onnxruntime/onnxruntime.dll" ]]; then
    export ORT_DYLIB_PATH="$CACHE/onnxruntime/onnxruntime.dll"
  elif [[ -f "$CACHE_OLD/onnxruntime/onnxruntime.dll" ]]; then
    export ORT_DYLIB_PATH="$CACHE_OLD/onnxruntime/onnxruntime.dll"
  elif [[ -f "$CACHE_OLD/onnxruntime/libonnxruntime.so" ]]; then
    export ORT_DYLIB_PATH="$CACHE_OLD/onnxruntime/libonnxruntime.so"
  else
    export ORT_DYLIB_PATH="$CACHE/onnxruntime/libonnxruntime.so"
  fi
fi
export CUBA_RERANKER_PATH="${CUBA_RERANKER_PATH:-$CACHE/reranker}"
export CUBA_NLI_PATH="${CUBA_NLI_PATH:-$CACHE/models-nli}"
export CUBA_EMBEDDING_DIM="${CUBA_EMBEDDING_DIM:-384}"
export CUBA_EMBED_MODEL="${CUBA_EMBED_MODEL:-e5-small}"
export CUBA_POOLING="${CUBA_POOLING:-cls}"

# Disk, before anything compiles. A gate run of this repo linked with `ld` dying on
# SIGBUS and three test binaries reported as "could not compile" — the cause was a
# partition at 98% with target/ alone holding 64 GB, and nothing in that output said
# so. Measured 15-ago-2026. A gate that fails for a reason nobody can read is worse
# than one that refuses to start.
#
# target/debug/deps grows without bound because cargo never removes the binaries of
# earlier compilations: every edit produces a new hash and the old one stays. The
# sweep below is by age, not by size, so an artifact still in use is never touched.
MIN_FREE_GB="${CUBA_GATE_MIN_FREE_GB:-8}"
SWEEP_BELOW_GB="${CUBA_GATE_SWEEP_BELOW_GB:-20}"
SWEEP_OLDER_THAN_DAYS="${CUBA_GATE_SWEEP_DAYS:-7}"

free_gb() { df --output=avail -BG "$RUST_DIR" | tail -1 | tr -dc '0-9'; }

sweep_stale_artifacts() {
  local before afte
  before="$(free_gb)"
  echo "disk: ${before}G free — sweeping build artifacts older than ${SWEEP_OLDER_THAN_DAYS}d"
  rm -rf "$RUST_DIR/target/debug/incremental" 2>/dev/null || true
  find "$RUST_DIR/target/debug/deps" -maxdepth 1 -type f \
       -mtime "+$SWEEP_OLDER_THAN_DAYS" -delete 2>/dev/null || true
  find "$RUST_DIR/target/debug/.fingerprint" -maxdepth 1 -type d \
       -mtime "+$SWEEP_OLDER_THAN_DAYS" -exec rm -rf {} + 2>/dev/null || true
  after="$(free_gb)"
  echo "disk: ${after}G free after the sweep (was ${before}G)"
}

if [[ "$(free_gb)" -lt "$SWEEP_BELOW_GB" ]]; then
  sweep_stale_artifacts
fi

if [[ "$(free_gb)" -lt "$MIN_FREE_GB" ]]; then
  echo "FAIL: only $(free_gb)G free on the filesystem holding $RUST_DIR, and this run needs more." >&2
  echo '      Linking is what breaks first, and it breaks as SIGBUS inside ld with no' >&2
  echo "      mention of disk — that is an hour of looking for a bug that is not there." >&2
  echo "      target/ currently holds: $(du -sh "$RUST_DIR/target" 2>/dev/null | cut -f1)" >&2
  echo "      Free it with: cargo clean --manifest-path $RUST_DIR/Cargo.toml" >&2
  echo "      (or raise the bar with CUBA_GATE_MIN_FREE_GB if you know what you are doing)" >&2
  exit 1
fi

require_present() {
  local what="$1" probe="$2"
  shift 2
  if [[ ! -e "$probe" ]]; then
    echo "FAIL: $what — nothing at $probe" >&2
    echo "      This gate does not skip model/CLI coverage. Install with" >&2
    echo "      \`memory-industry models all\` (and a generative LLM — see require_generative_llm)." >&2
    exit 1
  fi
  echo "=== $what ==="
  "$@"
}

require_generative_llm() {
  local what="$1"
  shift
  local base="${MEMORY_INDUSTRY_LLM_BASE_URL:-${CUBA_LLM_BASE_URL:-}}"
  local provider="${MEMORY_INDUSTRY_LLM_PROVIDER:-${CUBA_LLM_PROVIDER:-}}"
  local ok=0
  if [[ -n "$base" ]]; then
    base="${base%/}"
    if curl -fsS --max-time 5 "${base}/models" >/dev/null 2>&1 \
      || curl -fsS --max-time 8 -X POST "${base}/chat/completions" \
           -H "Content-Type: application/json" \
           -H "Authorization: Bearer ${MEMORY_INDUSTRY_LLM_API_KEY:-${CUBA_LLM_API_KEY:-local}}" \
           -d '{"model":"'"${MEMORY_INDUSTRY_LLM_MODEL:-ping}"'","messages":[{"role":"user","content":"ping"}],"max_tokens":1}' \
           >/dev/null 2>&1; then
      ok=1
      echo "OK  generative LLM via OpenAI-compat at $base"
    else
      echo "FAIL: $what — MEMORY_INDUSTRY_LLM_BASE_URL=$base is set but probe failed." >&2
      echo "      Fix the endpoint/API key — the gate does not soft-skip." >&2
      exit 1
    fi
  fi
  if [[ "$ok" -eq 0 && -n "$provider" ]]; then
    # Provider presets are resolved in Rust; presence of provider + any common key is enough to proceed.
    if [[ -n "${MEMORY_INDUSTRY_LLM_API_KEY:-${CUBA_LLM_API_KEY:-${OPENAI_API_KEY:-${DEEPSEEK_API_KEY:-${DASHSCOPE_API_KEY:-${MOONSHOT_API_KEY:-${ZHIPU_API_KEY:-${SILICONFLOW_API_KEY:-${OPENROUTER_API_KEY:-}}}}}}}}}" ]] \
      || [[ "$provider" == "ollama" || "$provider" == "lmstudio" || "$provider" == "vllm" ]]; then
      ok=1
      echo "OK  generative LLM via MEMORY_INDUSTRY_LLM_PROVIDER=$provider"
    else
      echo "FAIL: $what — provider=$provider set but no API key found." >&2
      echo "      Export MEMORY_INDUSTRY_LLM_API_KEY or the vendor key (DEEPSEEK_API_KEY, DASHSCOPE_API_KEY, …)." >&2
      exit 1
    fi
  fi
  if [[ "$ok" -eq 0 ]] && command -v claude >/dev/null 2>&1; then
    if claude -p "reply with the single word ok" --output-format text >/dev/null 2>&1; then
      ok=1
      echo "OK  generative LLM via authenticated claude CLI"
    fi
  fi
  if [[ "$ok" -eq 0 ]] && command -v gemini >/dev/null 2>&1; then
    if gemini -p "reply with the single word ok" --output-format text >/dev/null 2>&1; then
      ok=1
      echo "OK  generative LLM via authenticated gemini CLI"
    fi
  fi
  if [[ "$ok" -eq 0 ]]; then
    echo "FAIL: $what — no generative LLM ready." >&2
    echo "      Set MEMORY_INDUSTRY_LLM_PROVIDER=deepseek|qwen|moonshot|zhipu|openai|ollama|…" >&2
    echo "      (+ vendor API key), or MEMORY_INDUSTRY_LLM_BASE_URL to any OpenAI-compat /v1," >&2
    echo "      or authenticate \`claude\`/\`gemini\`, or use MCP sampling." >&2
    exit 1
  fi
  echo "=== $what ==="
  "$@"
}

GATE_DB="${GATE_DB:-brain_gate}"
GATE_DATABASE_URL="${LIVE_DATABASE_URL%/*}/$GATE_DB"
PEER_DB="${PEER_DB:-brain_gate_peer}"
PEER_DATABASE_URL="${LIVE_DATABASE_URL%/*}/$PEER_DB"
ADMIN_DATABASE_URL="${LIVE_DATABASE_URL%/*}/postgres"
export CUBA_PEER_DATABASE_URL="$PEER_DATABASE_URL"

if ! command -v psql >/dev/null; then
  echo "FAIL: psql is not on PATH, and the gate needs it to provision its throwaway databases." >&2
  echo "      It used to reach the server with 'docker exec <container> psql', which makes psql" >&2
  echo "      a child of the postmaster: when that psql exits non-zero the postmaster reads it" >&2
  echo "      as a crashed backend and restarts the whole cluster, taking the live brain" >&2
  echo "      database into recovery with it. Measured on 2026-08-14. Install postgresql-client." >&2
  exit 1
fi

cd "$RUST_DIR"

# Prefer an already-built binary. `cargo run` under timeout used to spend the
# whole budget recompiling and leave the throwaway DB with zero tables while
# `|| true` hid the failure (exit 124).
gate_bin() {
  if [[ -x "$RUST_DIR/target/debug/memory-industry" ]]; then
    echo "$RUST_DIR/target/debug/memory-industry"
  elif [[ -x "$RUST_DIR/target/release/memory-industry" ]]; then
    echo "$RUST_DIR/target/release/memory-industry"
  elif [[ -x "$RUST_DIR/target/debug/cuba-memorys" ]]; then
    echo "$RUST_DIR/target/debug/cuba-memorys"
  elif [[ -x "$RUST_DIR/target/release/cuba-memorys" ]]; then
    echo "$RUST_DIR/target/release/cuba-memorys"
  else
    cargo build --quiet --bin memory-industry >&2
    echo "$RUST_DIR/target/debug/memory-industry"
  fi
}

provision_gate_db() {
  local bin
  bin="$(gate_bin)"
  psql "$ADMIN_DATABASE_URL" -q \
    -c "DROP DATABASE IF EXISTS $GATE_DB WITH (FORCE)" \
    -c "CREATE DATABASE $GATE_DB" >/dev/null
  # doctor exits non-zero when ONNX_MODEL_PATH is empty (policy checks). Migrations
  # still apply first — trust the table count below, not the exit code alone.
  DATABASE_URL="$GATE_DATABASE_URL" CUBA_APP_ROLE=0 ONNX_MODEL_PATH="" \
    "$bin" doctor >/dev/null 2>&1 || true
  if [[ "$CUBA_EMBEDDING_DIM" != "384" ]]; then
    DATABASE_URL="$GATE_DATABASE_URL" "$ROOT/scripts/migrate-embedding-dim.sh" \
      "$CUBA_EMBEDDING_DIM" >/dev/null 2>&1 || {
        echo "FAIL: could not retype the throwaway database to vector($CUBA_EMBEDDING_DIM)." >&2
        exit 1
      }
  fi
  local dim
  dim="$(psql "$GATE_DATABASE_URL" -Atc \
    "SELECT atttypmod FROM pg_attribute WHERE attrelid='brain_observations'::regclass AND attname='embedding'")"
  if [[ "$dim" != "$CUBA_EMBEDDING_DIM" ]]; then
    echo "FAIL: throwaway database is vector($dim) but the model produces $CUBA_EMBEDDING_DIM." >&2
    echo "      Every embedding write would fail with 'expected $dim dimensions'." >&2
    exit 1
  fi
  local tables
  tables="$(psql "$GATE_DATABASE_URL" -Atc \
    "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")"
  if ((tables < 20)); then
    echo "FAIL: could not migrate the throwaway database (only $tables tables)." >&2
    echo "      doctor via $bin did not apply schema. Re-run with that binary visible." >&2
    exit 1
  fi
  echo "OK  throwaway database $GATE_DB ready ($tables tables, vector($dim))"

  psql "$ADMIN_DATABASE_URL" -q \
    -c "DROP DATABASE IF EXISTS $PEER_DB WITH (FORCE)" \
    -c "CREATE DATABASE $PEER_DB" >/dev/null
  DATABASE_URL="$PEER_DATABASE_URL" CUBA_APP_ROLE=0 ONNX_MODEL_PATH="" \
    "$bin" doctor >/dev/null 2>&1 || true
  if [[ "$CUBA_EMBEDDING_DIM" != "384" ]]; then
    DATABASE_URL="$PEER_DATABASE_URL" "$ROOT/scripts/migrate-embedding-dim.sh" \
      "$CUBA_EMBEDDING_DIM" >/dev/null 2>&1 || true
  fi
  local peer_tables
  peer_tables="$(psql "$PEER_DATABASE_URL" -Atc \
    "SELECT count(*) FROM information_schema.tables WHERE table_schema='public'")"
  if ((peer_tables < 20)); then
    echo "FAIL: could not migrate the second node's database (only $peer_tables tables)." >&2
    echo "      The two-node test would then skip, and a skipped test that reports green is" >&2
    echo "      how a machine claims two nodes converge without ever having run two." >&2
    exit 1
  fi
  echo "OK  second node database $PEER_DB ready ($peer_tables tables)"
}

# The exit code is written here before anything else can overwrite $?. A 20-minute
# gate gets launched in the background, and then its result is read from whateve
# the wrapper reports — which is the exit code of the last command in the chain,
# not of the gate. That is how GATE_EXIT=101 was once reported as green. Reading
# this file is the only honest answer, and /tmp is swept on reboot, so it lives
# under ~/.cache. Written by the gate itself so that launching it correctly is not
# something the caller has to remember.
EXIT_FILE="${CUBA_GATE_EXIT_FILE:-$HOME/.cache/cuba-gate/run.exit}"
rm -f "$EXIT_FILE"

on_exit() {
  local code=$?
  psql "$ADMIN_DATABASE_URL" -q \
    -c "DROP DATABASE IF EXISTS $GATE_DB WITH (FORCE)" \
    -c "DROP DATABASE IF EXISTS $PEER_DB WITH (FORCE)" >/dev/null 2>&1 || true
  mkdir -p "$(dirname "$EXIT_FILE")"
  echo "$code" > "$EXIT_FILE"
}
trap on_exit EXIT

echo "=== cargo fmt --check ==="
cargo fmt --check

echo "=== cargo clippy (--all-targets: without it, tests/ is never linted) ==="
cargo clippy --all-targets -- -D warnings

echo "=== throwaway database for every mutating step ==="
provision_gate_db

echo "=== cargo test (unit + smoke) ==="
DATABASE_URL="$GATE_DATABASE_URL" cargo test

echo "=== DB integration tests (--ignored) ==="
export DATABASE_URL="$GATE_DATABASE_URL"
# Every tests/*.rs is discovered, not listed. The list used to be written by hand
# and had drifted to 30 of 52 files: fifteen integration tests written for the
# sync and panel work had never once run in the gate, while the gate reported
# green over the very commits that added them. A gate whose coverage depends on
# somebody remembering to edit it is a gate that quietly shrinks.
RUN_ELSEWHERE=(v020_audit_update_applies v020_role_separation v020_audit_hmac
               v020_embed_cache_key v016_chunking nli_entailment nli_cost nli_probe
               v016_extract_without_sampling v017_relation_scan v017_rerank_gpu)
DISCOVERED=()
DEFERRED_SECTIONS=()
for file in tests/*.rs; do
  name="$(basename "$file" .rs)"
  if printf '%s\n' "${RUN_ELSEWHERE[@]}" | grep -qx "$name"; then
    DEFERRED_SECTIONS+=("$name")
  else
    DISCOVERED+=(--test "$name")
  fi
done
echo "deferred ${#DEFERRED_SECTIONS[@]} file(s) to their own sections: ${DEFERRED_SECTIONS[*]}"
echo "running $(( ${#DISCOVERED[@]} / 2 )) discovered test file(s)"
cargo test "${DISCOVERED[@]}" -- --ignored --nocapture

cargo test --lib -- --ignored --nocapture

echo "=== admin-role tests (they rewrite roles and the audit log) ==="
CUBA_APP_ROLE=0 cargo test --test v020_audit_update_applies --test v020_role_separation \
           -- --ignored --nocapture

echo "=== audit HMAC + embed cache key ==="
cargo test --test v020_audit_hmac --test v020_embed_cache_key -- --ignored --nocapture

require_present "tests that need the embedding model" "$ONNX_MODEL_PATH/model_quantized.onnx" \
  cargo test --test v016_chunking -- --ignored --nocapture

require_present "tests that need the NLI model" "$CUBA_NLI_PATH" \
  cargo test --test nli_entailment --test nli_cost --test nli_probe -- --ignored --nocapture

require_generative_llm "tests that need a generative LLM (extract / relation-scan)" \
  cargo test --test v016_extract_without_sampling --test v017_relation_scan \
             -- --ignored --nocapture

# build-gpu.sh, not a bare `cargo build --release`. Without --features cuda,
# gpu::wants_gpu() returns false unconditionally, CUBA_RERANK_DEVICE=gpu goes
# inert, and the reranker runs on CPU at 20.669s per query against 0.356s — 58x,
# measured in d7922fa. The E2E allows 15s per call, so every one of its 40 calls
# times out and the gate can never pass on a machine that has a GPU configured.
# It also overwrote the developer's GPU binary at this exact path.
echo "=== release build (same feature set production runs) ==="
"$ROOT/scripts/build-gpu.sh"

require_present "reranker tests (release: 387s in debug, seconds here)" \
  "$CUBA_RERANKER_PATH/model.onnx" \
  cargo test --release --test v017_rerank_gpu -- --ignored --nocapture

echo "=== E2E (25 MCP tools, subprocess per call) ==="
export CUBA_BINARY_PATH="${CUBA_BINARY_PATH:-$RUST_DIR/target/release/memory-industry}"
if [[ ! -f "$CUBA_BINARY_PATH" && -f "${CUBA_BINARY_PATH}.exe" ]]; then
  CUBA_BINARY_PATH="${CUBA_BINARY_PATH}.exe"
fi
if [[ ! -f "$CUBA_BINARY_PATH" ]]; then
  CUBA_BINARY_PATH="$RUST_DIR/target/release/cuba-memorys"
  if [[ ! -f "$CUBA_BINARY_PATH" && -f "${CUBA_BINARY_PATH}.exe" ]]; then
    CUBA_BINARY_PATH="${CUBA_BINARY_PATH}.exe"
  fi
fi
# Prefer PYTHON_BIN (set by run-gate on Windows). Never use the Microsoft Store
# python3.exe stub under WindowsApps — it prints install text and exits 49.
resolve_python() {
  if [[ -n "${PYTHON_BIN:-}" && ( -x "$PYTHON_BIN" || -f "$PYTHON_BIN" ) ]]; then
    printf '%s\n' "$PYTHON_BIN"
    return
  fi
  local c p
  for c in python3 python; do
    p="$(command -v "$c" 2>/dev/null || true)"
    [[ -n "$p" ]] || continue
    case "$p" in
      */WindowsApps/*) continue ;;
    esac
    printf '%s\n' "$p"
    return
  done
  printf '%s\n' "python3"
}
PY="$(resolve_python)"
echo "python=$PY"
export PYTHONUTF8=1
export PYTHONIOENCODING=utf-8
"$PY" tests/e2e_all_tools.py

echo "=== MCP live session (single process, initialize + tools/list + calls) ==="
"$PY" "$ROOT/scripts/mcp_live_session_test.py"

echo "=== eval harness smoke (read-only, so it runs against the real corpus) ==="
DATABASE_URL="$LIVE_DATABASE_URL" \
  "$CUBA_BINARY_PATH" eval \
  --dataset "$RUST_DIR/eval-datasets/smoke.jsonl" --k 10

echo ""
echo "All tests passed."
