#!/usr/bin/env bash
# Second judge: CRAP/complexity + mutation of the rust/src diff.
# SIL = ./scripts/merge-gate.sh. This script does not run it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
BASELINE="$ROOT/scripts/lizard-baseline.txt"

# Same derivation as mutants-gate.sh: a number tuned on one machine and used
# on every other one is the defect this release removed from the GPU path.
qg_mutants_jobs() {
  local cores ram half
  cores=$(nproc 2>/dev/null || echo 4)
  if [[ -r /proc/meminfo ]]; then
    ram=$(awk '/MemTotal/ {printf "%d", $2/1048576}' /proc/meminfo)
  else
    ram=8
  fi
  half=$(( cores / 2 ))
  (( half < 1 )) && half=1
  (( ram / 4 < half )) && half=$(( ram / 4 ))
  (( half > 6 )) && half=6
  (( half < 1 )) && half=1
  echo "$half"
}

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


# `key cc` per function over the ceiling, e.g. `src/protocol.rs::run_mcp:281 27`.
#
# The key carries the start line lizard already prints, and it is not decoration.
# Without it the key was `file::name`; judge.rs holds two `run_prompt`, and the
# baseline lookup below takes the first match in file order: the CC 9 one was
# judged against the CC 10 ceiling of the other and could gain a point in
# silence, which is the one failure a ratchet must not have. With the start line
# the key is unique by construction, since two functions cannot begin on the same
# line of the same file. The cost is that moving a function invalidates its key
# and the gate then calls it new work — wrong, but wrong out loud, which is the
# trade this made on purpose.
#
# The awk is the whole point of the word "over". `lizard -w` warns on
# `length > 1000` and on `nloc` too, not only on CC, and this function used to
# forward every warning it could parse a CCN out of. That is how the gate
# printed, on a real run:
#     CRAP: src/service.rs::keys_offered is at CC 6, over the ceiling of 8,
#           and is not in the baseline.
# A verdict that contradicts itself in its own sentence, about a function whose
# complexity was never the problem (it was 13 lines; lizard's Rust tokenizer had
# lost its place and thought it ran to the end of the file). Whoever read that
# line had two ways to make it go away and only one of them was true. The test
# is `> cc_max` and nothing else, which is also lizard's own criterion for `-C`:
# it warns when CCN is strictly over the ceiling, so the two halves agree.
lizard_warnings() {
  local cwd="$1" cc_max="$2"
  shift 2
  (cd "$cwd" && lizard -C "$cc_max" -w "$@" 2>/dev/null) |
    sed -nE 's|^(.*):([0-9]+): warning: ([A-Za-z0-9_:<>]+) has [0-9]+ NLOC, ([0-9]+) CCN.*|\1::\3:\2 \4|p' |
    awk -v max="$cc_max" '($2 + 0) > (max + 0)' |
    sed 's|\\|/|g' | sort -u || true
}

if [[ "${1:-}" == "--update-lizard-baseline" ]]; then
  cc_max="${LIZARD_CC_MAX:-8}"
  mapfile -t all < <(cd rust && find src -name '*.rs' | sort)
  {
    echo "# Functions already over CC $cc_max when this line was drawn."
    echo "# The gate fails a function that is NOT here, or one here that got worse."
    echo "# The key is path::function:start-line. The start line is what makes it"
    echo "# unique: judge.rs holds two run_prompt, the lookup took the first one in"
    echo "# file order, and so the CC 9 one sat behind the CC 10 ceiling of the other"
    echo "# and could gain a point with nobody saying anything."
    echo "# The price, accepted: moving a function changes its key, and the gate then"
    echo "# says it is not in the baseline. That verdict is loud and obviously wrong,"
    echo "# which is the trade for the one it replaces: silent and invisibly wrong."
    echo "# Regenerating REWRITES this file whole, so every comment below - each one"
    echo "# carrying the reason and the owner of a deliberate deviation - is dropped."
    echo "# Put them back, or the next reader inherits the numbers without the why."
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
      echo "      the baseline on purpose. If instead it only MOVED, its key moved with" >&2
      echo "      it — the baseline is keyed by start line — and regenerating is the" >&2
      echo "      answer, not splitting a function that was already accounted for." >&2
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

# --- self-test: the CRAP filter gets a fixture that puts it in the red -------
# A guard nobody can watch fail is a paragraph. Both directions are here,
# because absence proves nothing without a presence anchor beside it: a warning
# that is NOT about complexity must come out of `lizard_warnings` as nothing at
# all, and one that IS must still come out with its number.
#   ./scripts/quality-gate.sh --self-test
if [[ "${1:-}" == "--self-test" ]]; then
  if ! command -v lizard >/dev/null; then
    echo "FALTA lizard: el self-test del filtro CRAP no puede correr." >&2
    exit 2
  fi
  # The fixtures below are built around a ceiling of 8, so this one does not
  # read the environment: an exported LIZARD_CC_MAX would move the line the
  # fixtures were cut to and fail the self-test for the wrong reason.
  LIZARD_CC_MAX=8
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/src"

  # 1014 lines, one statement each: lizard measures it at CCN 1 and warns all
  # the same, because `lizard -w` also fires on `length > 1000`. Before the awk in
  # lizard_warnings this forwarded only_long with a CCN of 1 and run_lizard
  # turned it into "is at CC 1, over the ceiling of 8".
  {
    echo 'fn only_long(n: u32) -> u32 {'
    echo '    let mut t = n;'
    for _ in $(seq 1010); do echo '    t = t + 1;'; done
    echo '    t'
    echo '}'
  } > "$tmp/src/only_long.rs"
  got="$(lizard_warnings "$tmp" 8 src/only_long.rs)"
  if [[ -n "$got" ]]; then
    echo "FAIL self-test: a length warning was reported as a CC violation ($got)" >&2
    exit 1
  fi
  if ! run_lizard "$tmp" src/only_long.rs; then
    echo "FAIL self-test: run_lizard failed a function whose CC is 1" >&2
    exit 1
  fi

  # The presence anchor: twelve `if`s, so CCN 13, five over the ceiling. This
  # one has to survive the filter with its number intact and has to fail
  # run_lizard, since no fixture is ever in the baseline.
  {
    echo 'fn tangled(n: u32) -> u32 {'
    echo '    let mut t = 0;'
    for i in $(seq 12); do echo "    if n == $i { t += $i; }"; done
    echo '    t'
    echo '}'
  } > "$tmp/src/tangled.rs"
  got="$(lizard_warnings "$tmp" 8 src/tangled.rs)"
  if [[ "$got" != "src/tangled.rs::tangled:1 13" ]]; then
    echo "FAIL self-test: the filter lost a CC 13 function (got '$got')" >&2
    exit 1
  fi
  if run_lizard "$tmp" src/tangled.rs 2>/dev/null; then
    echo "FAIL self-test: run_lizard passed a CC 13 function that is not in the baseline" >&2
    exit 1
  fi

  # The fixture for the reason the key carries a start line, in the shape the
  # real defect had: judge.rs holds two `run_prompt`, one per judge, and until
  # this release both collapsed onto `src/cognitive/judge.rs::run_prompt`. The
  # lookup in run_lizard takes the first match in file order, so the CC 9 one
  # was ratcheted against the CC 10 ceiling of the other and could gain a point
  # with the gate still green. Two functions cannot begin on the same line of
  # the same file, so the start line is what makes a key unique.
  {
    echo 'struct Cli;'
    echo 'struct Http;'
    echo 'impl Cli {'
    echo '    fn run_prompt(&self, n: u32) -> u32 {'
    echo '        let mut t = 0;'
    for i in $(seq 8); do echo "        if n == $i { t += $i; }"; done
    echo '        t'
    echo '    }'
    echo '}'
    echo 'impl Http {'
    echo '    fn run_prompt(&self, n: u32) -> u32 {'
    echo '        let mut t = 0;'
    for i in $(seq 9); do echo "        if n == $i { t += $i; }"; done
    echo '        t'
    echo '    }'
    echo '}'
  } > "$tmp/src/twins.rs"
  got="$(lizard_warnings "$tmp" 8 src/twins.rs)"
  twins=$(printf '%s\n' "$got" | wc -l)
  keys=$(printf '%s\n' "$got" | cut -d' ' -f1 | sort -u | wc -l)
  if (( twins != 2 )) || (( keys != 2 )); then
    echo "FAIL self-test: two same-named functions in one file came out as $twins line(s)" >&2
    echo "                on $keys key(s); the baseline needs one key each (got '$got')" >&2
    exit 1
  fi

  echo "OK  self-test: the CRAP half reports CC violations, only CC violations, and"
  echo "    keeps two same-named functions in one file on two separate keys"
  exit 0
fi

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
          #
          # The entries added to --exclude-re below are not reachable by
          # `cargo mutants -- --lib`, each for its own reason, and every one of
          # them has its decision tested somewhere the mutation CAN reach:
          #
          #   serve_pool        binds a socket and serves forever. --lib never
          #                     starts a daemon. Same class as the handlers.
          #   gpu_availability  two cfg variants. configure does call it, but on
          #                     the gate's build wants_gpu is false and
          #                     cpu_reason discards its other two arguments in
          #                     that first branch, so nothing observes the
          #                     answer. cpu_reason itself has a table.
          #   cuda_provider     #[cfg(feature = "cuda")]; not compiled here.
          #   apply             writes the process-wide environment, which every
          #                     other test in the binary reads. That is exactly
          #                     why plan_env is pure and tested; what is left is
          #                     three lines of set_if_absent.
          #   failure_reason    reads a OnceLock that --lib cannot populate
          #                     without loading 1.1 GB of model. reason_of, the
          #                     half that decides, has a table.
          #
          # A second group, all of the same two shapes. Nothing here is
          # unexamined: the half of each that decides something was pulled out
          # and given a table where the mutation can reach it.
          #
          #   Needs a live ONNX session or a model on disk, which --lib has
          #   neither of: warm_up, score_off_runtime, score_one_chunk,
          #   score_pairs, nli::init, init_onnx_session. Their decisions live
          #   in needs_padding, ranked, budget_from and the deadline tests.
          #
          #   A one-line environment lookup whose pure half is tested:
          #   warm_reranker_eagerly -> warm_eagerly_from,
          #   judge_is_sampling -> judge_is_sampling_from.
          #
          #   mcp_endpoint and panel are axum handlers, the same class as the
          #   src/handlers/* already skipped above. Everything they decide now
          #   lives in origin_allowed_with, auth_brake, next_failure,
          #   brake_for, record_auth_failure, clear_auth_failures,
          #   refuse_foreign_origin, note_activity and panel_route_enabled, and
          #   every one of those has its own test.
          #
          #   The trailing space on `replace panel `, `judge_is_sampling ` and
          #   `replace init ` is load-bearing. --exclude-re matches by
          #   substring, so a bare name also hides every longer name that
          #   starts with it: `panel` hid panel_enabled, panel_route_enabled
          #   and panel_allows_forwarded, and `judge_is_sampling` hid
          #   judge_is_sampling_from — the pure half named above as the reason
          #   that exclusion is honest. Nobody chose either cover, and
          #   panel_route_enabled, which draws the line between a panel on
          #   loopback and one served to the whole LAN, sat outside the judge
          #   from the day it was written. A name that is a prefix of another
          #   name needs the space. The service.rs entry in the fourth group
          #   below obeys the same rule by other means: `replace restrict`
          #   keeps its signature and its `Ok.false` tail, so it can cover
          #   neither a future restrict_* nor the `Ok(true)` sibling that
          #   dies today.
          #
          #   The opposite trap of this same argument is a pattern that
          #   covers nothing. --exclude-re (27.1.0) does not match `delete
          #   field X from struct Y expression` mutants at all — not by
          #   field name, not by struct name, not by file name — so a
          #   pattern aimed at one of those is dead the day it is written,
          #   and reads like a cover where there is none. A survivor of that
          #   shape is not answered with a better pattern: it is answered by
          #   deleting the `..base` functional update that makes
          #   cargo-mutants emit it, which is what src/service.rs did. The
          #   note lives there, next to the literal someone would fold back
          #   up.
          #
          # A third group, every one of them from the same line of this build.
          # The gate compiles without --features cuda, and gpu.rs:48 returns
          # before anything below it is reached:
          #     if !cfg!(any(feature = "cuda", feature = "directml")) { return false; }
          # Owner: endurecedor 0.27. Expires: 2027-03-21.
          #
          #   gpu.rs.*replace wants_gpu -> bool with false
          #                     without the feature the function IS the constant
          #                     false, so this mutant is the same program. The
          #                     tail is load-bearing: the bare name would also
          #                     hide `with true`, which dies today and has to
          #                     stay under judgement.
          #   gpu.rs.*preferred_device_var
          #                     its only caller is gpu.rs:51, after that return.
          #                     Nothing in this build ever executes it.
          #   gpu.rs.*compiled_provider.*with None
          #                     without the feature the original already returns
          #                     None. The `.*with None` tail is load-bearing:
          #                     the bare name would also hide Some("") and
          #                     Some("xyzzy"), which
          #                     status_reports_the_provider_this_binary_was_compiled_with
          #                     kills. Delete that test and those two go red.
          #   gpu.rs.*runtime_dir
          #                     carries #[cfg(any(feature = "cuda", feature =
          #                     "directml"))], so it is not compiled here at
          #                     all. It entered the diff only because this
          #                     release added a comment inside it.
          #   http.rs.*compiled_gpu_provider.*with None
          #                     the same case in the other file, and the tail is
          #                     load-bearing for the same reason: its two
          #                     siblings die in
          #                     the_gpu_build_field_names_a_provider_or_says_nothing.
          #
          # Where those decisions ARE judged: scripts/run-all-tests.sh runs
          #     cargo test --release --features cuda --lib gpu::
          # which executes gpu::placement_tests with the feature alive — the
          # device table, including the MEMORY_INDUSTRY_/CUBA_ precedence rows,
          # status_reports_the_provider_this_binary_was_compiled_with and
          # a_runtime_downloaded_under_the_old_name_survives_the_rename. If that
          # line ever leaves run-all-tests.sh, every exclusion in this group
          # stops being an exclusion and becomes a cover: delete them the same
          # day.
          #
          # A fourth group of one, and it is not about a feature flag: it is
          # the platform this judge runs on. Owner: endurecedor 0.27.
          # Expires: 2027-03-21.
          #
          #   service.rs.*replace restrict -> Result<bool> with Ok.false
          #                     one function with the cfg inside the body: on
          #                     unix it chmods 0600 and returns Ok(true),
          #                     anywhere else it is a declared no-op that
          #                     returns Ok(false). This gate runs on Windows,
          #                     where the original already returns Ok(false), so
          #                     the mutant is the same program. Both tails are
          #                     load-bearing. `Ok.false` (the dot is the paren)
          #                     must not also swallow the sibling `with
          #                     Ok(true)`, which dies here under
          #                     a_secret_file_on_windows_reports_that_it_did_not_restrict_anything
          #                     — that death is the evidence that this exclusion
          #                     hides nothing. And ` -> Result<bool> with` after
          #                     the name is the prefix rule above: a future
          #                     restrict_something is not covered by it.
          #                     The unix half is judged on unix:
          #                     a_secret_file_is_unreadable_to_other_users
          #                     asserts mode 0o600 and restricted: true under
          #                     #[cfg(unix)]. The day this judge also runs on
          #                     Linux, this stops being an exclusion and becomes
          #                     a cover: delete it then.
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
            --exclude-re 'fetch_adjacency|list_resources|read_resource|run_checks_with|upsert_symbol|upsert_placeholder_entity|builtin_retrieval_set|backfill_unscoped|observation_in_scope|run_project|run_check|run_write|workspace_client_id|http.rs.*serve_pool|gpu.rs.*gpu_availability|gpu.rs.*cuda_provider|resources.rs.*replace apply|rerank.rs.*failure_reason|http.rs.*mcp_endpoint|http.rs.*replace panel |http.rs.*warm_reranker_eagerly|llm_cli.rs.*judge_is_sampling |rerank.rs.*warm_up|rerank.rs.*score_off_runtime|rerank.rs.*score_one_chunk|rerank.rs.*score_pairs|nli.rs.*replace init |onnx.rs.*init_onnx_session|gpu.rs.*replace wants_gpu -> bool with false|gpu.rs.*preferred_device_var|gpu.rs.*compiled_provider.*with None|gpu.rs.*runtime_dir|http.rs.*compiled_gpu_provider.*with None|service.rs.*replace restrict -> Result<bool> with Ok.false' \
            --timeout 90 --jobs "${MUTANTS_JOBS:-$(qg_mutants_jobs)}" --gitignore=false -- --lib) || fail=1
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
