#!/usr/bin/env bash
# Second judge: CRAP/complexity + mutation of the rust/src diff.
# SIL = ./scripts/merge-gate.sh. This script does not run it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
BASELINE="$ROOT/scripts/lizard-baseline.txt"

echo "=== quality-gate (MemoryIndustry — CRAP/lizard + mutación del diff) ==="
echo "NO MIRA: SIL (fmt, clippy -D, tests --ignored, e2e, deny, audit, codigo-muerto, crap-gate floor, mutants-gate mmr/rrf/cache)."
echo "NO CORRE: ./scripts/merge-gate.sh"
echo "SIL: ./scripts/merge-gate.sh  (alias: ./scripts/como-el-ci.sh todo)"
echo "HAY scripts/merge-gate.sh — este script NO lo sustituye."

# QG_BASE lets this judge a branch. Without it the diff is whatever is still
# uncommitted, so running it after committing the work it was meant to judge
# printed "SIN DIFF" and exited 0 - a clean pass over nothing at all, which
# reads exactly like a real one in a log.
base="${QG_BASE:-}"
if [[ -n "$base" ]]; then
  git rev-parse --verify --quiet "$base" >/dev/null || { echo "FAIL: QG_BASE=$base is not a ref" >&2; exit 2; }
  diff=$(git diff --name-only "$base...HEAD" 2>/dev/null || true)
  echo "base: $base"
else
  diff=$(git diff --name-only HEAD 2>/dev/null || true)
  [[ -n "$diff" ]] || diff=$(git diff --name-only --cached 2>/dev/null || true)
fi
echo "diff: $diff"
if [[ -z "$diff" ]]; then
  echo "SIN DIFF: no hay archivos tocados. No se afirma NADA sobre ningun cambio."
  echo "         Para juzgar una rama ya commiteada: QG_BASE=main $0"
fi

fail=0

rel_under() {
  local prefix="$1"
  shift
  local f
  for f in "$@"; do
    f="${f//\\//}"
    case "$f" in
      "$prefix"/*) printf '%s\n' "${f#"$prefix"/}" ;;
    esac
  done
}


# `key cc` per function over the ceiling, e.g. `src/protocol.rs::run_mcp 27`.
lizard_warnings() {
  local cwd="$1" cc_max="$2"
  shift 2
  (cd "$cwd" && lizard -C "$cc_max" -w "$@" 2>/dev/null) |
    sed -nE 's|^(.*):[0-9]+: warning: ([A-Za-z0-9_:<>]+) has [0-9]+ NLOC, ([0-9]+) CCN.*|\1::\2 \3|p' |
    sed 's|\\|/|g' | sort -u || true
}

if [[ "${1:-}" == "--update-lizard-baseline" ]]; then
  cc_max="${LIZARD_CC_MAX:-8}"
  mapfile -t all < <(cd rust && find src -name '*.rs' | sort)
  {
    echo "# Functions already over CC $cc_max when this line was drawn."
    echo "# The gate fails a function that is NOT here, or one here that got worse."
    echo "# Regenerate with: ./scripts/quality-gate.sh --update-lizard-baseline"
    lizard_warnings rust "$cc_max" "${all[@]}"
  } > "$BASELINE"
  echo "wrote $BASELINE ($(grep -vc '^#' "$BASELINE") functions over CC $cc_max)"
  exit 0
fi


# The ceiling alone is unusable on a repo that already has complex functions:
# touching one line of a CC 30 function would fail the whole diff, and the
# first thing anybody would do is put the `|| true` back. The baseline records
# what is already over the line, so the gate only fails something that is new
# or got worse. Regenerate deliberately: ./scripts/quality-gate.sh --update-lizard-baseline
run_lizard() {
  local cwd="$1"
  shift
  if [[ "$#" -eq 0 ]]; then return 0; fi
  local cc_max="${LIZARD_CC_MAX:-8}"

  if ! command -v lizard >/dev/null && ! command -v python >/dev/null; then
    echo "FALTA lizard (CRAP/cc). Puerta CRAP 6; CC <= 4 orienta."
    return 2
  fi

  local worse=0 key cc base
  while read -r key cc; do
    [[ -z "$key" ]] && continue
    base=$(awk -v k="$key" '$1 == k {print $2}' "$BASELINE" 2>/dev/null | head -1)
    if [[ -z "$base" ]]; then
      echo "CRAP: $key is at CC $cc, over the ceiling of $cc_max, and is not in the baseline." >&2
      echo "      A function this tangled is new work. Split it, or explain it and update" >&2
      echo "      the baseline on purpose." >&2
      worse=$((worse + 1))
    elif (( cc > base )); then
      echo "CRAP: $key went from CC $base to $cc. It was already over the ceiling; making it" >&2
      echo "      worse is the direction this gate exists to stop." >&2
      worse=$((worse + 1))
    fi
  done < <(lizard_warnings "$cwd" "$cc_max" "$@")

  (( worse == 0 )) || return 1
  return 0
}

rs=()
py=()
while IFS= read -r line; do
  [[ -z "$line" ]] && continue
  case "$line" in
    *.rs) rs+=("$line") ;;
    *.py) py+=("$line") ;;
  esac
done < <(printf '%s\n' $diff)

if [[ ${#rs[@]} -gt 0 ]]; then
  if [[ ! -f rust/Cargo.toml ]]; then
    echo "FALTA rust/Cargo.toml"
    fail=1
  else
    src_rs=()
    while IFS= read -r rel; do
      [[ -z "$rel" ]] && continue
      case "$rel" in
        src/*) src_rs+=("$rel") ;;
      esac
    done < <(rel_under rust "${rs[@]}")
    if [[ ${#src_rs[@]} -gt 0 ]]; then
      if ! run_lizard rust "${src_rs[@]}"; then
        [[ $fail -eq 0 ]] && fail=2
      fi
      if command -v cargo-mutants >/dev/null || (cd rust && cargo mutants --version >/dev/null 2>&1); then
        lib_rs=()
        for f in "${src_rs[@]}"; do
          case "$f" in
            src/main.rs)
              echo "skip $f — bin-only; cargo mutants -- --lib never executes it"
              ;;
            src/handlers/*)
              # Measured 2026-09-18: 362 MISSED under --lib, 245 in reflexion alone.
              # Handlers are async+DB; --lib never awaits them. SIL --ignored + e2e do.
              echo "skip $f — MCP handler; cargo mutants -- --lib never awaits them (SIL --ignored + e2e)"
              ;;
            src/cli.rs|src/codegraph_cli.rs|src/recall_cli.rs|src/doctor.rs|src/setup_agent.rs)
              echo "skip $f — CLI/doctor surface; cargo mutants -- --lib does not drive these entrypoints"
              ;;
            *) lib_rs+=("$f") ;;
          esac
        done
        if [[ ${#lib_rs[@]} -eq 0 ]]; then
          echo "diff rust/src is bin-only — cargo mutants --lib does not apply."
        else
          files=()
          for f in "${lib_rs[@]}"; do
            files+=(--file "$f")
          done
          # Cursor/sandbox points CARGO_TARGET_DIR at a shared cache. cargo-mutants
          # then compiles every scratch copy there; leftover mutant artifacts
          # (rrf.rs from mutants-gate) make the unmutated baseline fail tests that
          # pass on a clean target. Each scratch dir must use its own target/.
          unset CARGO_TARGET_DIR
          # --file alone mutates the whole file (2315 mutants on the 0.26 tree).
          # --in-diff keeps the second judge on the changed lines.
          diff_file="$(mktemp)"
          # Same base as the file list above, or the two halves of this judge
          # would disagree about what "the change" is.
          if [[ -n "$base" ]]; then
            git diff --relative=rust "$base...HEAD" -- rust/src >"$diff_file" || true
          else
            git diff --relative=rust HEAD -- rust/src >"$diff_file" || true
            if [[ ! -s "$diff_file" ]]; then
              git diff --relative=rust --cached -- rust/src >"$diff_file" || true
            fi
          fi
          in_diff=()
          if [[ -s "$diff_file" ]]; then
            in_diff=(--in-diff "$diff_file")
          fi
          (cd rust && cargo mutants "${files[@]}" "${in_diff[@]}" \
            --exclude-re 'fetch_adjacency|list_resources|read_resource|run_checks_with|upsert_symbol|upsert_placeholder_entity|builtin_retrieval_set|backfill_unscoped|observation_in_scope|run_project|run_check|run_write|workspace_client_id' \
            --timeout 90 --jobs 2 --gitignore=false -- --lib) || fail=1
          rm -f "$diff_file"
        fi
      else
        echo "FALTA cargo-mutants. Endurecedor no puede cerrar."
        [[ $fail -eq 0 ]] && fail=2
      fi
    else
      echo "diff .rs fuera de rust/src — lizard/mutants del producto no aplican."
    fi
  fi
fi

if [[ ${#py[@]} -gt 0 ]]; then
  if command -v radon >/dev/null; then
    radon cc "${py[@]}" -s -n D || true
  elif command -v lizard >/dev/null || command -v python >/dev/null; then
    echo "radon ausente; lizard mide los .py del diff (misma puerta CRAP/cc)."
    if ! run_lizard . "${py[@]}"; then
      [[ $fail -eq 0 ]] && fail=2
    fi
  else
    echo "FALTA radon. Puerta CRAP 6; CC <= 4 orienta."
    [[ $fail -eq 0 ]] && fail=2
  fi
fi

echo "=== fin quality-gate exit=$fail ==="
exit "$fail"
