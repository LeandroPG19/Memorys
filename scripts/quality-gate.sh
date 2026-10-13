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

# Every --exclude-re entry of the mutation step, in one place so that the
# exclusion checks below read exactly what the mutation run is handed. Each
# entry's reason, owner and expiry date are in the comment over that step.
MUTANTS_EXCLUDE_RE='fetch_adjacency|list_resources|read_resource|run_checks_with|upsert_symbol|upsert_placeholder_entity|backfill_unscoped|observation_in_scope|run_project|run_check|run_write|workspace_client_id|http.rs.*serve_pool|gpu.rs.*gpu_availability|gpu.rs.*cuda_provider|resources.rs.*replace apply|rerank.rs.*failure_reason|http.rs.*mcp_endpoint|http.rs.*replace panel |http.rs.*warm_reranker_eagerly|llm_cli.rs.*judge_is_sampling |rerank.rs.*warm_up|rerank.rs.*score_off_runtime|rerank.rs.*score_one_chunk|rerank.rs.*score_pairs|nli.rs.*replace init |onnx.rs.*init_onnx_session|gpu.rs.*replace wants_gpu -> bool with false|gpu.rs.*preferred_device_var|gpu.rs.*compiled_provider.*with None|http.rs.*compiled_gpu_provider.*with None|service.rs.*replace restrict -> Result<bool> with Ok.false|src/redact\.rs:110:63: replace > with >= in credentials_in_url|src/redact\.rs:130:58: replace > with >= in secret_field|src/redact\.rs:136:51: replace \+ with \* in secret_field|src/cognitive/nli\.rs:.*replace enabled -> bool with |src/cognitive/nli\.rs:.*replace status_resolved -> bool with false|src/cognitive/nli\.rs:.*replace failure_reason -> Option<String> with None|src/embeddings/onnx\.rs:.*replace failure_reason -> Option<String> with None|src/embeddings/onnx\.rs:.*replace compute_embedding -> Result<Vec<f32>> with Ok\(vec!|src/http\.rs:[0-9]+:24: replace && with \|\| in embedder_state$|src/(calibrate_cli|dashboard|dedupe_cli|export|link_cli|reembed_cli|rem_cli|secure_cli|skills_cli|sync_cli|eval/mod)\.rs:.*replace run_cli -> Result<\(\)> with Ok\(\(\)\)|src/models_cli\.rs:.*replace print_help with \(\)|src/setup\.rs:.*replace log with \(\)|src/dashboard\.rs:.*replace render -> Result<String> with |src/search/calibrate\.rs:.*replace load_ood_threshold -> Option<f64> with |src/export\.rs:.*replace export_obsidian -> Result<usize> with |src/db\.rs:.*replace assert_embedding_dim -> Result<\(\)> with Ok\(\(\)\)|src/dedupe_cli\.rs:.*replace merge_by_name -> Result<\(\)> with Ok\(\(\)\)|src/protocol\.rs:.*replace run_rem_consolidation_locked -> Result<\(\)> with Ok\(\(\)\)|src/models_cli\.rs:.*replace download_(model|runtime) -> Result<\(\)> with Ok\(\(\)\)|src/protocol\.rs:.*replace spawn_handshake_watchdog with \(\)|src/embeddings/onnx\.rs:.*replace locate_onnxruntime -> Option<PathBuf> with Some\(Default::default\(\)\)|src/main\.rs:.*replace async_main with \(\)|src/db\.rs:.*replace bind_app_role -> sqlx::Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace init_schema -> Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace provision_app_role with \(\)|src/db\.rs:.*replace provision_app_role_from_pgpass with \(\)|src/db\.rs:.*replace write_app_role_password -> sqlx::Result<\(\)> with Ok\(\(\)\)|src/embeddings/onnx\.rs:.*replace load_onnxruntime -> Result<\(\)> with Ok\(\(\)\)|src/embeddings/onnx\.rs:.*replace load_located_onnxruntime -> Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace migrate_and_provision -> Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace migrate -> Result<\(\), MigrateError> with Ok\(\(\)\)|src/db\.rs:.*replace realign_then_migrate -> Result<\(\), MigrateError> with Ok\(\(\)\)|src/db\.rs:.*replace realign_line_ending_checksums -> sqlx::Result<\(\)> with Ok\(\(\)\)|src/http\.rs:.*replace \|\| with && in identify_account|src/http\.rs:.*replace != with == in identify_account|src/http\.rs:.*delete ! in identify_account|src/http\.rs:.*replace pair_answer -> Option<PairResponse> with None|src/http\.rs:.*delete ! in claim_and_answer|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("", None\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("", Some\(String::new\(\)\)\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("xyzzy", None\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("", Some\("xyzzy"\.into\(\)\)\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("xyzzy", Some\(String::new\(\)\)\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("xyzzy", Some\("xyzzy"\.into\(\)\)\)\)|src/http\.rs:.*replace != with == in credential_proof|src/accounts_db\.rs:.*replace record_pair_redeemed -> Result<\(\)> with Ok\(\(\)\)|src/accounts_db\.rs:.*replace list_accounts -> Result<Vec<AccountSummary>> with Ok\(vec!\[\]\)|src/accounts_db\.rs:.*replace lock_account -> Result<Uuid> with Ok\(Default::default\(\)\)|src/accounts_db\.rs:.*replace > with < in revoke_rows|src/accounts_db\.rs:.*replace > with >= in revoke_rows|src/accounts_db\.rs:.*replace revoke_device -> Result<usize> with Ok\(0\)|src/accounts_db\.rs:.*replace revoke_device -> Result<usize> with Ok\(1\)|src/accounts_db\.rs:.*replace ensure_code_capacity -> Result<\(\)> with Ok\(\(\)\)|src/accounts_db\.rs:.*replace \+ with - in store_code|src/accounts_db\.rs:.*replace pairing_candidates -> Result<Vec<PairingCandidate>> with Ok\(vec!\[\]\)|src/accounts_db\.rs:.*replace - with \+ in pairing_candidates|src/accounts_db\.rs:.*replace == with != in claim_pairing_code|src/accounts_db\.rs:.*replace credential_for_auth -> Result<Option<CredentialRow>> with Ok\(None\)|src/accounts_db\.rs:.*replace touch_last_seen -> Result<\(\)> with Ok\(\(\)\)|src/accounts_db\.rs:.*replace purge_pairing_codes -> Result<u64> with Ok\(1\)|src/accounts_db\.rs:.*replace purge_pairing_codes -> Result<u64> with Ok\(0\)|src/accounts_db\.rs:.*replace - with \+ in purge_pairing_codes|src/accounts_db\.rs:.*replace < with > in account_facts|src/accounts_db\.rs:.*replace < with <= in account_facts|src/accounts_cli\.rs:.*replace == with != in run_cli|src/accounts_db\.rs:.*replace == with != in revoke_device|src/accounts_db\.rs:.*replace >= with < in ensure_code_capacity|src/accounts_db\.rs:.*replace account_facts -> Result<AccountFacts> with Ok\(Default::default\(\)\)|src/accounts_db\.rs:.*replace < with == in account_facts|src/bridge/mod\.rs:.*replace cli_run -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace cli_install -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace cli_uninstall -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace cli_doctor -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace != with == in cli_doctor'

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


# `key cc` per function over the ceiling, e.g. `src/protocol.rs::run_mcp#1 27`.
#
# The key is path::name#n, where n counts, from 1, every function of that name
# in the file in the order they begin. It needs the ordinal because the bare
# `file::name` is not unique: judge.rs holds two `run_prompt`, and a baseline
# lookup that takes the first match in file order judged the CC 9 one against the
# CC 10 ceiling of the other, so it could gain a point in silence, which is the
# one failure a ratchet must not have. The ordinal is not a line number on
# purpose: the key used to carry the start line, and any insertion above a
# function renamed it for the baseline, so the gate called a function that only
# moved new work and the baseline was regenerated by hand three times in 0.29.
#
# Three details are load-bearing:
#  - n counts every function of the name, not only those over the ceiling. If it
#    counted only those, a small twin crossing the ceiling would renumber the big
#    one, which is a red for a function nobody touched.
#  - `#n` is always there, even for a unique name. A suffix that appears only
#    when there is a twin would rename a function the day somebody adds a twin
#    to the file.
#  - n follows the start line, not the order lizard prints. lizard emits a
#    function when it closes it, so a nested `fn walk` or a trait's declaration
#    comes out before the function that begins above it.
#
# One `lizard --csv` run gives every function with its CCN, name and start, so
# the ordinal and the CC come from the same list. `-w` would hand over only the
# warnings, and those also fire on `length > 1000` and on `nloc`: the gate once
# printed, on a real run:
#     CRAP: src/service.rs::keys_offered is at CC 6, over the ceiling of 8,
#           and is not in the baseline.
# A verdict that contradicts itself in its own sentence, about a function whose
# complexity was never the problem (it was 13 lines; lizard's Rust tokenizer had
# lost its place and thought it ran to the end of the file). Reading the CCN from
# its own column keeps that filter structural. The test is `> cc_max` and nothing
# else, which is also lizard's own criterion for `-C`.
#
# In the csv, columns 1-8 are safe to split on commas ($2 CCN, $7 file, $8 name,
# both quoted) and the 9th, the signature, can carry commas, so the start is read
# from the end: $(NF-1). A name outside [A-Za-z0-9_:<>] (empty ones included) is
# dropped, as it always was.
lizard_warnings() {
  local cwd="$1" cc_max="$2"
  shift 2
  (cd "$cwd" && lizard --csv "$@" 2>/dev/null) |
    awk -F, -v max="$cc_max" '
      NF >= 11 && $2 ~ /^[0-9]+$/ {
        file = $7; name = $8
        gsub(/"/, "", file); gsub(/"/, "", name)
        gsub(/\\/, "/", file)
        if (name !~ /^[A-Za-z0-9_:<>]+$/) next
        n++
        f[n] = file; nm[n] = name; start[n] = $(NF - 1) + 0; ccn[n] = $2 + 0
        key = file SUBSEP name
        group[key] = group[key] " " n
      }
      END {
        for (i = 1; i <= n; i++) {
          if (ccn[i] <= max + 0) continue
          m = split(group[f[i] SUBSEP nm[i]], idx, " ")
          ord = 1
          for (k = 1; k <= m; k++) {
            j = idx[k]
            if (start[j] < start[i] || (start[j] == start[i] && j < i)) ord++
          }
          printf "%s::%s#%d %d\n", f[i], nm[i], ord, ccn[i]
        }
      }' |
    LC_ALL=C sort -u || true
}

# The header's last line. Everything above it in the baseline is this
# script's and gets rewritten; everything below it is the entries, each with
# the comment that sits over it.
BASELINE_HEADER_END="# Regenerate with: ./scripts/quality-gate.sh --update-lizard-baseline"

# keep_baseline_comments OLD_BASELINE NOTES_FILE < FRESH_KEYS: the fresh
# `key cc` lines, each with the comment that sat over the same key in the old
# baseline, and any comment after the last key at the end.
#
# Regenerating used to write the file whole, and every comment in it went with
# the old numbers: 20 lines of reason and owner, each one the why of a
# deliberate deviation. A comment belongs to the key right under it, which is
# how every one in the file is written. A key that is gone (its function
# deleted or renamed, or a twin added above it) takes its comment with it, and
# NOTES_FILE names both, so it can be put back by hand over the new key if the
# function was only renamed. The old file is read with getline
# rather than as awk's first input, because an empty first input makes
# FNR == NR true for the second file too.
keep_baseline_comments() {
  awk -v oldfile="$1" -v notes="$2" -v header_end="$BASELINE_HEADER_END" '
    BEGIN {
      while ((getline line < oldfile) > 0) { sub(/\r$/, "", line); old[++n] = line }
      start = 1
      for (i = 1; i <= n; i++) if (old[i] == header_end) { start = i + 1; break }
      for (i = start; i <= n; i++) {
        if (old[i] ~ /^#/ || old[i] ~ /^[[:space:]]*$/) { pending = pending old[i] "\n"; continue }
        split(old[i], f, /[[:space:]]+/)
        if (pending != "") { comment[f[1]] = pending; keys[++k] = f[1] }
        pending = ""
      }
    }
    {
      split($0, f, /[[:space:]]+/)
      if (f[1] in comment) { printf "%s", comment[f[1]]; placed[f[1]] = 1 }
      print
    }
    END {
      printf "%s", pending
      for (i = 1; i <= k; i++) {
        if (keys[i] in placed) continue
        printf "%s left the baseline, and the comment over it went with it:\n", keys[i] > notes
        c = comment[keys[i]]
        sub(/\n$/, "", c)
        gsub(/\n/, "\n      ", c)
        printf "      %s\n", c > notes
      }
    }'
}

if [[ "${1:-}" == "--update-lizard-baseline" ]]; then
  cc_max="${LIZARD_CC_MAX:-8}"
  mapfile -t all < <(cd rust && find src -name '*.rs' | sort)
  regenerated="$(mktemp)"
  dropped="$(mktemp)"
  {
    echo "# Functions already over CC $cc_max when this line was drawn."
    echo "# The key is path::function#n, where n counts every function of that name in"
    echo "# the file, over the ceiling or not, in the order they begin. The n is what"
    echo "# keeps two of the same name apart: judge.rs holds two run_prompt, and one"
    echo "# key for both let the CC 9 one gain a point behind the ceiling of the CC 10."
    echo "# Moving a function, or growing or shrinking anything above it, keeps its key."
    echo "# Renaming it, or adding or removing a function of the same name above it in"
    echo "# that file, changes the key."
    echo "# Inside the diff the gate is red for: a function that is NOT here, one here"
    echo "# that got worse, one that got simpler while this file stayed behind, and an"
    echo "# entry here whose function is no longer measured over the ceiling (it fell"
    echo "# to it, was renamed or deleted, or lizard lost it inside a neighbour: read"
    echo "# lizard --csv on the file before removing the line). Any other shape of"
    echo "# line in this file is red too."
    echo "# Every comment below sits over the key it explains, carrying the reason and"
    echo "# the owner of a deliberate deviation, and regenerating keeps it there. A key"
    echo "# that leaves the baseline takes its comment with it, and regenerating names"
    echo "# both. Everything above the next line is rewritten by the script."
    echo "$BASELINE_HEADER_END"
    lizard_warnings rust "$cc_max" "${all[@]}" | keep_baseline_comments "$BASELINE" "$dropped"
  } > "$regenerated"
  # Copied over, not moved: a mktemp file would bring its 0600 mode with it.
  cat "$regenerated" > "$BASELINE"
  rm -f "$regenerated"
  if [[ -s "$dropped" ]]; then
    echo "note: regenerating dropped the comments of keys that are no longer over CC $cc_max." >&2
    echo "      If the function was only renamed, put the comment back over its new key." >&2
    sed 's/^/      /' "$dropped" >&2
  fi
  rm -f "$dropped"
  echo "wrote $BASELINE ($(grep -vc '^#' "$BASELINE") functions over CC $cc_max)"
  exit 0
fi


# The ceiling alone is unusable on a repo that already has complex functions:
# touching one line of a CC 30 function would fail the whole diff, and the
# first thing anybody would do is put the `|| true` back. The baseline records
# what is already over the line, so the gate only fails something that is new
# or got worse. Regenerate deliberately: ./scripts/quality-gate.sh --update-lizard-baseline
#
# The ratchet is exact in both directions, because a ceiling that is looser than
# the number it was drawn for is room to grow back into in silence. So a
# function whose CC went down is red until the baseline follows it, and so is an
# entry of a judged file that is no longer measured over the ceiling: nothing in
# the loop over the measured functions would ever look at it. The entries of
# files that are not in this run are left alone.
run_lizard() {
  local cwd="$1"
  shift
  if [[ "$#" -eq 0 ]]; then return 0; fi
  local cc_max="${LIZARD_CC_MAX:-8}"

  if ! command -v lizard >/dev/null && ! command -v python >/dev/null; then
    echo "FALTA lizard (CRAP/cc). Puerta CRAP 6; CC <= 4 orienta."
    return 2
  fi

  local worse=0 key cc line was
  local -A base_cc=()
  local shape='^[^[:space:]]+::[^[:space:]]+#[1-9][0-9]* [0-9]+$'

  # Every line is read, the shape first: a line in the old `path::name:line`
  # shape would otherwise sit in the file doing nothing, or worse, match by
  # accident. A comment is `#` in column 0 and nothing else, since the key itself
  # carries a `#` in the middle.
  if [[ -f "$BASELINE" ]]; then
    while IFS= read -r line || [[ -n "$line" ]]; do
      line="${line%$'\r'}"
      [[ -z "$line" || "$line" == '#'* ]] && continue
      if [[ ! "$line" =~ $shape ]]; then
        echo "CRAP: baseline line '$line' is not '<path>::<function>#<n> <cc>'." >&2
        echo "      The baseline is keyed by function name and ordinal, never by line;" >&2
        echo "      regenerate it on purpose with --update-lizard-baseline." >&2
        worse=$((worse + 1))
        continue
      fi
      key="${line% *}"
      [[ -n "${base_cc[$key]:-}" ]] || base_cc[$key]="${line##* }"
    done < "$BASELINE"
  fi

  local measured
  measured="$(lizard_warnings "$cwd" "$cc_max" "$@")"
  while read -r key cc; do
    [[ -z "$key" ]] && continue
    was="${base_cc[$key]:-}"
    if [[ -z "$was" ]]; then
      echo "CRAP: $key is at CC $cc, over the ceiling of $cc_max, and is not in the baseline." >&2
      echo "      A function this tangled is new work. Split it, or explain it and update" >&2
      echo "      the baseline on purpose. If it was only renamed, its old key shows up" >&2
      echo "      below as a dead entry, and regenerating is the answer, not splitting a" >&2
      echo "      function that was already accounted for." >&2
      worse=$((worse + 1))
    elif (( cc > was )); then
      echo "CRAP: $key went from CC $was to $cc. It was already over the ceiling; making it" >&2
      echo "      worse is the direction this gate exists to stop." >&2
      worse=$((worse + 1))
    elif (( cc < was )); then
      echo "CRAP: $key went down from CC $was to $cc, and the baseline still says $was." >&2
      echo "      A ceiling above the real number is room to grow back into with nobody" >&2
      echo "      saying anything: regenerate it with --update-lizard-baseline." >&2
      worse=$((worse + 1))
    fi
  done <<< "$measured"

  # An entry of a judged file that is no longer measured over the ceiling gets
  # no verdict from the loop above, which only walks what is measured. The
  # entries of files outside this run are not judged: that keeps the step as
  # cheap as the rest of this half, and the full tree is checked by regenerating.
  local judged_files measured_keys path
  judged_files="$(printf '%s\n' "$@" | tr '\\' '/')"
  measured_keys="$(printf '%s\n' "$measured" | cut -d' ' -f1)"
  if (( ${#base_cc[@]} > 0 )); then
    while read -r key; do
      was="${base_cc[$key]}"
      (( was > cc_max )) || continue
      path="${key%%::*}"
      grep -qxF -- "$path" <<< "$judged_files" || continue
      grep -qxF -- "$key" <<< "$measured_keys" && continue
      echo "CRAP: $key is in the baseline at CC $was, but is no longer measured over the ceiling of $cc_max." >&2
      echo "      It fell to the ceiling or below, was renamed or deleted (with its file," >&2
      echo "      or a twin of the same name was added or removed above it), or lizard lost" >&2
      echo "      it inside a neighbouring function: read lizard --csv on $path" >&2
      echo "      before removing the line. Then regenerate with --update-lizard-baseline." >&2
      worse=$((worse + 1))
    done < <(printf '%s\n' "${!base_cc[@]}" | LC_ALL=C sort)
  fi

  (( worse == 0 )) || return 1
  return 0
}

# --- the exclusions judge themselves ------------------------------------------
# verificacion.md: every exception carries a reason, an owner and an expiry
# date, and goes red when it expires or stops being used. The dates were in the
# comments over the mutation step from 0.27 on, and nothing read them: the day
# one passed, nothing would have happened.
#
# A comment line that names an owner has to carry the date on that same line.
# One group had the date wrapped onto the next line, and a reader that only
# looks for dates would walk straight past a group whose date got lost. So a
# missing date is a finding, not a line to skip. Expiring today is still valid.
exclusion_expiry_problems() {
  local file="$1" today="$2"
  awk -v today="$today" '
    /^[[:space:]]*#/ && /Owner:|Expires:/ {
      text = $0
      sub(/^[[:space:]]*#[[:space:]]*/, "", text)
      if (match($0, /Expires: [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]/)) {
        date = substr($0, RSTART + 9, 10)
        if (date < today) printf "%s:%d: expired on %s: %s\n", FILENAME, FNR, date, text
      } else {
        printf "%s:%d: no YYYY-MM-DD expiry date on this line: %s\n", FILENAME, FNR, text
      }
    }' "$file"
}

# One alternative per line, split at the top-level `|` only: the `|` inside
# (model|runtime) and the escaped one in \|\| belong to their alternative.
# The regex reaches awk through the environment, because `awk -v` would
# process its backslashes first and turn \| into |.
exclusion_alternatives() {
  EXCLUDE_RE="$1" awk 'BEGIN {
    re = ENVIRON["EXCLUDE_RE"]; depth = 0; in_class = 0; cur = ""
    for (i = 1; i <= length(re); i++) {
      c = substr(re, i, 1)
      if (c == "\\") { cur = cur c substr(re, i + 1, 1); i++; continue }
      if (in_class) { if (c == "]") in_class = 0 }
      else if (c == "[") in_class = 1
      else if (c == "(") depth++
      else if (c == ")") depth--
      else if (c == "|" && depth == 0) { print cur; cur = ""; continue }
      cur = cur c
    }
    print cur
  }'
}

# The other half of the rule: an exclusion that matches no mutant any more is
# not excluding anything, and it reads like a cover. A pattern anchored on a
# line:col goes this way the day its line moves. `cargo mutants --list` names
# every mutant of the crate without building anything, so this costs seconds.
# grep -P is the closest thing in bash to the regex crate cargo-mutants uses:
# both search, unanchored, with the same escapes for every entry here.
unused_exclusions() {
  local re="$1" list="$2" alt
  while IFS= read -r alt; do
    grep -qP -e "$alt" "$list" || printf '%s\n' "$alt"
  done < <(exclusion_alternatives "$re")
}

# mutate_and_judge ARGS...: `cargo mutants` over ARGS (the --file and --in-diff
# set) with the exclusions above, and its outcomes through the SIL's build check.
#
# cargo mutants exits 0 when every mutant it could not build is unviable,
# whatever stopped the build, and a machine out of memory stops all of them
# (0xC0000142 on Windows, 44 of 51 on 2026-09-23). Its outcomes go to a fresh
# directory, so what gets judged is this run and never a mutants.out left
# behind by an earlier one, and then to the same check the SIL uses: the rule
# and its --self-test live in mutants-gate.sh, not in a copy here.
#
# That directory is named to cargo-mutants in the spelling cygpath -m gives.
# cargo-mutants is a Windows program under Git Bash, and MSYS rewrites an
# argument that looks like a POSIX path when it launches one — measured on
# 2026-09-23: `--output /tmp/tmp.X` as two arguments arrives as
# C:/Users/<user>/AppData/Local/Temp/tmp.X. Not with MSYS_NO_PATHCONV or
# MSYS2_ARG_CONV_EXCL set in the shell: then cargo-mutants gets /tmp/tmp.X, writes under \tmp on the current drive, this
# script finds no outcomes.json in its /tmp, and a run nobody judged reads as
# one with nothing to judge. Off Windows there is no cygpath and nothing to
# rewrite.
mutate_and_judge() {
  local out native rc=0
  out="$(mktemp -d)"
  native="$(cygpath -m "$out" 2>/dev/null || printf '%s' "$out")"
  (cd rust && cargo mutants "$@" \
    --exclude-re "$MUTANTS_EXCLUDE_RE" --output "$native" \
    --timeout 90 --jobs "${MUTANTS_JOBS:-$(qg_mutants_jobs)}" --gitignore=false -- --lib) || rc=1
  if [[ -f "$out/mutants.out/outcomes.json" ]]; then
    "$ROOT/scripts/mutants-gate.sh" --check-builds "$out/mutants.out/outcomes.json" || rc=1
  else
    echo "no outcomes.json: cargo mutants built no mutant, so there is no build to judge"
  fi
  rm -rf "$out"
  return "$rc"
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
  if [[ "$got" != "src/tangled.rs::tangled#1 13" ]]; then
    echo "FAIL self-test: the filter lost a CC 13 function (got '$got')" >&2
    exit 1
  fi
  : > "$tmp/bl-empty.txt"
  if BASELINE="$tmp/bl-empty.txt" run_lizard "$tmp" src/tangled.rs 2>/dev/null; then
    echo "FAIL self-test: run_lizard passed a CC 13 function that is not in the baseline" >&2
    exit 1
  fi

  # The fixture for the reason the key carries an ordinal, in the shape the
  # real defect had: judge.rs holds two `run_prompt`, one per judge, and once
  # both collapsed onto `src/cognitive/judge.rs::run_prompt`. The lookup in
  # run_lizard takes the first match in file order, so the CC 9 one was
  # ratcheted against the CC 10 ceiling of the other and could gain a point
  # with the gate still green. The ordinal (`#1`, `#2`, by start line, over every
  # function of that name in the file) makes the key unique without tying it to
  # a line number, so moving a function does not change it.
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
  want="$(printf '%s\n' 'src/twins.rs::run_prompt#1 9' 'src/twins.rs::run_prompt#2 10')"
  if [[ "$got" != "$want" ]]; then
    echo "FAIL self-test: the two run_prompt were not keyed #1 and #2 in start order (got '$got')" >&2
    exit 1
  fi

  # From here on each case hands run_lizard its own baseline: run_lizard reads
  # $BASELINE when it is called, so a fixture never depends on the real
  # scripts/lizard-baseline.txt and that file cannot move one of them.
  st_fail() {
    echo "FAIL self-test: $*" >&2
    exit 1
  }
  # judged BASELINE_FILE FILE...: the exit status lands in $rc and what
  # run_lizard said in $tmp/judged.err.
  judged() {
    local bl="$1"
    shift
    rc=0
    BASELINE="$bl" run_lizard "$tmp" "$@" 2>"$tmp/judged.err" || rc=$?
  }
  said() { grep -qF -- "$1" "$tmp/judged.err"; }
  # fn_with_ifs NAME K: a function of CC K+1, as the fixtures above write them.
  fn_with_ifs() {
    echo "fn $1(n: u32) -> u32 {"
    echo '    let mut t = 0;'
    for i in $(seq "$2"); do echo "    if n == $i { t += $i; }"; done
    echo '    t'
    echo '}'
  }
  # moved_file K: `steady` of CC K+1 under a 31-line `filler` of CC 1, so that
  # it begins on line 32 and not on line 1.
  moved_file() {
    {
      echo 'fn filler(n: u32) -> u32 {'
      echo '    let mut t = n;'
      for _ in $(seq 27); do echo '    t = t + 1;'; done
      echo '    t'
      echo '}'
      fn_with_ifs steady "$1"
    } > "$tmp/src/moved.rs"
  }

  # A function that only moved keeps its key and its verdict. The key used to
  # carry the start line, so 31 lines of unrelated code above `steady` made the
  # gate call it new work, and the third time that happened somebody
  # regenerated the baseline by hand.
  printf '%s\n' 'src/moved.rs::steady#1 13' > "$tmp/bl-moved.txt"
  fn_with_ifs steady 12 > "$tmp/src/moved.rs"
  at_top="$(lizard_warnings "$tmp" 8 src/moved.rs)"
  moved_file 12
  pushed_down="$(lizard_warnings "$tmp" 8 src/moved.rs)"
  if [[ "$at_top" != "src/moved.rs::steady#1 13" || "$pushed_down" != "$at_top" ]]; then
    st_fail "a function pushed down 31 lines changed its key (line 1: '$at_top', line 32: '$pushed_down')"
  fi
  judged "$tmp/bl-moved.txt" src/moved.rs
  if (( rc != 0 )) || said 'CRAP:'; then
    st_fail "a function that only moved was judged (exit $rc): $(cat "$tmp/judged.err")"
  fi

  # The ratchet is exact in both directions. Up was always red; down and
  # "no longer over the ceiling" were silent, which leaves slack for the
  # function to grow back into with nobody saying anything.
  moved_file 13
  judged "$tmp/bl-moved.txt" src/moved.rs
  if (( rc == 0 )) || ! said 'src/moved.rs::steady#1 went from CC 13 to 14'; then
    st_fail "a function that got worse was not reported as 'went from CC 13 to 14' (exit $rc): $(cat "$tmp/judged.err")"
  fi
  moved_file 11
  judged "$tmp/bl-moved.txt" src/moved.rs
  if (( rc == 0 )) || ! said 'src/moved.rs::steady#1 went down from CC 13 to 12'; then
    st_fail "a function that got simpler, with the baseline left at 13, was not reported (exit $rc): $(cat "$tmp/judged.err")"
  fi
  # CC 8 is not over the ceiling of 8, so it leaves lizard_warnings altogether:
  # its baseline entry is dead, not lower.
  moved_file 7
  judged "$tmp/bl-moved.txt" src/moved.rs
  if (( rc == 0 )) || ! said 'src/moved.rs::steady#1 is in the baseline at CC 13'; then
    st_fail "an entry whose function fell to the ceiling was not reported as dead (exit $rc): $(cat "$tmp/judged.err")"
  fi

  # A rename leaves both verdicts, and an entry of a file that is not being
  # judged says nothing: the dead-entry check has the scope of the diff.
  printf '%s\n' 'src/moved.rs::steady#1 13' 'src/not_in_diff.rs::untouched#1 20' > "$tmp/bl-renamed.txt"
  fn_with_ifs settled 12 > "$tmp/src/moved.rs"
  judged "$tmp/bl-renamed.txt" src/moved.rs
  if (( rc == 0 )) \
    || ! said 'src/moved.rs::settled#1 is at CC 13, over the ceiling of 8, and is not in the baseline' \
    || ! said 'src/moved.rs::steady#1 is in the baseline at CC 13' \
    || said 'not_in_diff'; then
    st_fail "a renamed function did not give both reds, or an entry outside the judged files was named (exit $rc): $(cat "$tmp/judged.err")"
  fi

  # A deleted file is still in the diff, and what its entries said is gone.
  printf '%s\n' 'src/deleted.rs::old#1 20' > "$tmp/bl-deleted.txt"
  judged "$tmp/bl-deleted.txt" src/deleted.rs
  if (( rc == 0 )) || ! said 'src/deleted.rs::old#1 is in the baseline at CC 20'; then
    st_fail "the entries of a deleted file were not reported as dead (exit $rc): $(cat "$tmp/judged.err")"
  fi

  # The ordinal counts every function of the name, not only those over the
  # ceiling. Counting only those would renumber the CC 10 one the moment its
  # small twin crossed 8, and that is a red for a function nobody touched.
  twins2() {
    echo 'struct Cli;'
    echo 'struct Http;'
    echo 'impl Cli {'
    echo '    fn run_prompt(&self, n: u32) -> u32 {'
    echo '        let mut t = 0;'
    for i in $(seq "$1"); do echo "        if n == $i { t += $i; }"; done
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
  }
  printf '%s\n' 'src/twins2.rs::run_prompt#2 10' > "$tmp/bl-twins2.txt"
  twins2 2 > "$tmp/src/twins2.rs"
  got="$(lizard_warnings "$tmp" 8 src/twins2.rs)"
  if [[ "$got" != 'src/twins2.rs::run_prompt#2 10' ]]; then
    st_fail "the second run_prompt was not #2 while the first was under the ceiling (got '$got')"
  fi
  twins2 8 > "$tmp/src/twins2.rs"
  got="$(lizard_warnings "$tmp" 8 src/twins2.rs)"
  want="$(printf '%s\n' 'src/twins2.rs::run_prompt#1 9' 'src/twins2.rs::run_prompt#2 10')"
  if [[ "$got" != "$want" ]]; then
    st_fail "the second run_prompt was renumbered when the first crossed the ceiling (got '$got')"
  fi
  judged "$tmp/bl-twins2.txt" src/twins2.rs
  if (( rc == 0 )) || ! said 'src/twins2.rs::run_prompt#1 is at CC 9' || said 'run_prompt#2'; then
    st_fail "the first run_prompt crossing the ceiling was not the only thing reported (exit $rc): $(cat "$tmp/judged.err")"
  fi

  # lizard prints a function when it closes it, so the inner `walk` (lines 2-4)
  # comes out before the outer (1-16) that begins first. The ordinal follows
  # the start line, not that order.
  {
    echo 'fn walk(n: u32) -> u32 {'
    echo '    fn walk(m: u32) -> u32 {'
    echo '        m'
    echo '    }'
    echo '    let mut t = walk(n);'
    for i in $(seq 9); do echo "    if n == $i { t += $i; }"; done
    echo '    t'
    echo '}'
  } > "$tmp/src/nested.rs"
  got="$(lizard_warnings "$tmp" 8 src/nested.rs)"
  if [[ "$got" != 'src/nested.rs::walk#1 10' ]]; then
    st_fail "a nested function of the same name took the outer one's ordinal (got '$got')"
  fi

  # A baseline line in the old shape has to be red even though its key still
  # matches what is measured today; and the guard must not refuse a good one.
  printf '%s\n' 'src/tangled.rs::tangled:1 13' > "$tmp/bl-shape-old.txt"
  printf '%s\n' 'src/tangled.rs::tangled#1 13' > "$tmp/bl-shape-new.txt"
  judged "$tmp/bl-shape-old.txt" src/tangled.rs
  if (( rc == 0 )) || ! said "baseline line 'src/tangled.rs::tangled:1 13'"; then
    st_fail "a baseline line in the old <path>::<function>:<line> shape was accepted (exit $rc): $(cat "$tmp/judged.err")"
  fi
  judged "$tmp/bl-shape-new.txt" src/tangled.rs
  if (( rc != 0 )) || said 'CRAP:'; then
    st_fail "the shape check refused a well-formed baseline (exit $rc): $(cat "$tmp/judged.err")"
  fi

  # The expiry reader, judged as if today were 2026-09-22. One line per shape
  # it has to tell apart: expired yesterday (red), expires today (still valid),
  # a date wrapped onto the next line (red, on the line that lost it), a line
  # that is only that wrapped date (nothing to judge), an owner with no date
  # at all (red), and a string in code that only looks like one (ignored).
  printf '%s\n' \
    '# Owner: a. Expires: 2026-09-21.' \
    '  # Owner: b. Expires: 2026-09-22.' \
    '# Owner: c. Expires:' \
    '# 2099-01-01.' \
    '# Owner: d.' \
    'echo "# Owner: e. Expires: 2020-01-01."' > "$tmp/expiry.sh"
  got="$(exclusion_expiry_problems "$tmp/expiry.sh" 2026-09-22)"
  flagged="$(printf '%s\n' "$got" | sed -nE 's|.*expiry\.sh:([0-9]+):.*|\1|p' | tr '\n' ' ')"
  if [[ "$flagged" != "1 3 5 " ]]; then
    echo "FAIL self-test: the expiry reader flagged lines '$flagged', not '1 3 5 ' (got '$got')" >&2
    exit 1
  fi
  if [[ "$got" != *"expired on 2026-09-21: Owner: a."* ]]; then
    echo "FAIL self-test: an expired exclusion was reported without its date and owner (got '$got')" >&2
    exit 1
  fi
  if [[ "$got" != *"no YYYY-MM-DD expiry date on this line: Owner: c. Expires:"* ]]; then
    echo "FAIL self-test: a date wrapped onto the next line was not reported as missing (got '$got')" >&2
    exit 1
  fi

  # The unused-pattern check. The list is three mutants in the shape
  # `cargo mutants --list` prints them; the regex has five alternatives, two of
  # which match nothing: one whose line moved (4:9 against the mutant at 3:9)
  # and one whose function is gone. The other three carry the two kinds of `|`
  # that must not split an alternative, a group and an escape.
  printf '%s\n' \
    'src/a.rs:10:5: replace f -> bool with false' \
    'src/b.rs:3:9: replace && with || in g' \
    'src/c.rs:1:1: replace download_model -> Result<()> with Ok(())' > "$tmp/mutants.txt"
  re='src/a\.rs:.*replace f -> bool with false|src/b\.rs:3:9: replace && with \|\| in g|src/c\.rs:.*replace download_(model|runtime) -> Result<\(\)> with Ok\(\(\)\)|src/b\.rs:4:9: replace && with \|\| in g|gone_function'
  alternatives=$(exclusion_alternatives "$re" | wc -l)
  if (( alternatives != 5 )); then
    echo "FAIL self-test: a five-entry --exclude-re split into $alternatives alternative(s)" >&2
    exit 1
  fi
  got="$(unused_exclusions "$re" "$tmp/mutants.txt")"
  want="$(printf '%s\n' 'src/b\.rs:4:9: replace && with \|\| in g' 'gone_function')"
  if [[ "$got" != "$want" ]]; then
    echo "FAIL self-test: the unused-exclusion check reported '$got', not the two dead entries" >&2
    exit 1
  fi

  # Regenerating the baseline keeps the why of every entry. It used to write
  # the file whole, and the 20 lines of reason and owner in it went with the
  # old numbers. A copy of this script in a throwaway tree, over two tangled
  # functions, regenerates a baseline where a comment sits over each of three
  # keys: one whose CC moved, one unchanged, and one whose function is gone.
  # The first two keep their comment over them; the third's leaves with it,
  # out loud. The header above the "Regenerate with" line is the script's own
  # and is rewritten, which is why the fixture's is a stale one.
  bt="$tmp/baseline-tree"
  mkdir -p "$bt/scripts" "$bt/rust/src"
  cp "$ROOT/scripts/quality-gate.sh" "$bt/scripts/"
  for spec in kept:12 grew:10; do
    {
      echo "fn ${spec%:*}(n: u32) -> u32 {"
      echo '    let mut t = 0;'
      for i in $(seq "${spec#*:}"); do echo "    if n == $i { t += $i; }"; done
      echo '    t'
      echo '}'
    } > "$bt/rust/src/${spec%:*}.rs"
  done
  marker='# Regenerate with: ./scripts/quality-gate.sh --update-lizard-baseline'
  printf '%s\n' \
    '# A stale header, which regenerating replaces.' \
    "$marker" \
    '# grew: two lines of reason,' \
    '# and an owner. Owner: fixture.' \
    'src/grew.rs::grew#1 9' \
    '# gone: the reason of a function that no longer exists.' \
    'src/gone.rs::gone#1 20' \
    '# kept: one line. Owner: fixture.' \
    'src/kept.rs::kept#1 13' > "$bt/scripts/lizard-baseline.txt"
  regen_rc=0
  bash "$bt/scripts/quality-gate.sh" --update-lizard-baseline >"$tmp/regen.out" 2>&1 || regen_rc=$?
  if (( regen_rc != 0 )); then
    echo "FAIL self-test: --update-lizard-baseline exited $regen_rc: $(cat "$tmp/regen.out")" >&2
    exit 1
  fi
  if [[ "$(grep -cxF "$marker" "$bt/scripts/lizard-baseline.txt")" != 1 ]]; then
    echo "FAIL self-test: the regenerated baseline does not carry the header's last line exactly once:" >&2
    cat "$bt/scripts/lizard-baseline.txt" >&2
    exit 1
  fi
  got="$(sed -n "\|^$marker\$|,\$p" "$bt/scripts/lizard-baseline.txt" | tail -n +2)"
  want="$(printf '%s\n' \
    '# grew: two lines of reason,' \
    '# and an owner. Owner: fixture.' \
    'src/grew.rs::grew#1 11' \
    '# kept: one line. Owner: fixture.' \
    'src/kept.rs::kept#1 13')"
  if [[ "$got" != "$want" ]]; then
    echo "FAIL self-test: regenerating the baseline did not keep each comment over its key." >&2
    echo "      wanted:" >&2; printf '        %s\n' "$want" >&2
    echo "      got:" >&2; printf '        %s\n' "$got" >&2
    exit 1
  fi
  if grep -q 'stale header' "$bt/scripts/lizard-baseline.txt"; then
    echo "FAIL self-test: the old header survived regeneration next to the new one" >&2
    exit 1
  fi
  if ! grep -q 'src/gone.rs::gone#1' "$tmp/regen.out" || ! grep -q 'gone: the reason of a function' "$tmp/regen.out"; then
    echo "FAIL self-test: a key that left the baseline took its comment with it in silence: $(cat "$tmp/regen.out")" >&2
    exit 1
  fi
  # The key no longer carries a start line, so the header that explains the
  # file must not teach that it does.
  if grep -qiE 'start[ -]line' "$bt/scripts/lizard-baseline.txt"; then
    echo "FAIL self-test: the regenerated header still talks about the start line as the key:" >&2
    cat "$bt/scripts/lizard-baseline.txt" >&2
    exit 1
  fi
  # Regenerating twice over the same tree is a fixed point. Anything else would
  # make every regeneration a diff, and hide the change that was meant.
  cp "$bt/scripts/lizard-baseline.txt" "$tmp/regen-first.txt"
  regen_rc=0
  bash "$bt/scripts/quality-gate.sh" --update-lizard-baseline >"$tmp/regen2.out" 2>&1 || regen_rc=$?
  if (( regen_rc != 0 )) || ! cmp -s "$tmp/regen-first.txt" "$bt/scripts/lizard-baseline.txt"; then
    echo "FAIL self-test: a second --update-lizard-baseline changed the file (exit $regen_rc):" >&2
    diff "$tmp/regen-first.txt" "$bt/scripts/lizard-baseline.txt" >&2 || true
    exit 1
  fi

  # The mutation step has to hand cargo-mutants, a Windows program under Git
  # Bash, a path that program can see. MSYS rewrites `--output /tmp/tmp.X` to
  # C:/.../Temp/tmp.X when it launches one, but not with MSYS_NO_PATHCONV or
  # MSYS2_ARG_CONV_EXCL set; then cargo-mutants writes under \tmp on the
  # current drive, this script finds no outcomes.json in /tmp, and nothing is
  # judged. The stand-in cargo passes its arguments to Python, a native
  # program, with the rewriting off, so it receives exactly the bytes this
  # script wrote. It writes an outcomes.json only into a directory it can
  # see: a genuine catch (must pass) or a build the machine killed (must fail
  # through mutants-gate.sh --check-builds). Off Windows there is nothing to
  # rewrite, and both pass through as they are.
  py="$(command -v python3 || command -v python || true)"
  if [[ -z "$py" ]]; then
    echo "FALTA python: el self-test del paso de mutación no puede correr." >&2
    exit 2
  fi
  mkdir -p "$tmp/bin"
  cat > "$tmp/fake_mutants.py" <<'PY'
import json, os, sys
args = sys.argv[1:]
# Before anything is looked at, so that a run which never got here leaves no
# trace and the self-test can say "cargo mutants was not invoked". One line per
# call: the arguments, then the variables the judge is supposed to set.
calls = os.environ.get("FAKE_CALLS_DIR")
if calls:
    open(os.path.join(calls, "cargo.invoked"), "a").close()
    seen = [os.environ.get(k, "") for k in ("TMP", "TEMP", "TMPDIR", "CARGO_PROFILE_DEV_DEBUG", "CARGO_INCREMENTAL")]
    with open(os.path.join(calls, "cargo.calls"), "a", encoding="utf-8") as f:
        f.write("\t".join([" ".join(args)] + seen) + "\n")
out = args[args.index("--output") + 1] if "--output" in args else None
if not out or not os.path.isdir(out):
    print(f"stand-in cargo-mutants: --output {out!r} is not a directory this process can see")
    sys.exit(0)
# The key is spelled in two halves on purpose: local_gate_contract.rs holds
# that this file never reads an exit status itself (that rule lives in
# mutants-gate.sh), and it checks by looking for the key's name.
STATUS = "process" + "_status"
def mutant(name, summary, phases):
    return {"scenario": {"Mutant": {"name": name}}, "summary": summary, "log_path": "log/m.log",
            "phase_results": [{"phase": p, STATUS: s} for p, s in phases]}
caught = mutant("src/a.rs:1:1: replace f with g", "CaughtMutant", [("Build", "Success"), ("Test", {"Failure": 101})])
killed = mutant("src/a.rs:2:1: replace h with i", "Unviable", [("Build", {"Failure": -1073741502})])
records = [caught] if os.environ.get("FAKE_OUTCOME") == "genuine" else [caught, killed]
os.makedirs(os.path.join(out, "mutants.out"), exist_ok=True)
with open(os.path.join(out, "mutants.out", "outcomes.json"), "w", encoding="utf-8") as f:
    json.dump({"outcomes": records, "total_mutants": len(records), "caught": 1, "missed": 0,
               "timeout": 0, "unviable": len(records) - 1, "success": 0}, f)
PY
  # The stand-in's own script path is the fixture's business, not the code's
  # under test, so it is spelled natively here: with the rewriting off,
  # Python would not find /tmp/... either.
  fake_py="$(cygpath -m "$tmp/fake_mutants.py" 2>/dev/null || printf '%s' "$tmp/fake_mutants.py")"
  printf '%s\n' '#!/bin/sh' \
    "MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 exec \"$py\" \"$fake_py\" \"\$@\"" > "$tmp/bin/cargo"
  chmod +x "$tmp/bin/cargo"

  # --- G3: the mutation step does not fill the disk -------------------------------
  # On 2026-10-13 it ran out of disk (os error 112): every job copied rust/ with the
  # cargo targets inside it, and nothing looked at the disk first. The rule is
  # mutants-gate.sh --disk-plan / --prepare-scratch, whose own --self-test proves
  # it; what is held here is that THIS step asks before it runs cargo mutants, hands
  # it the jobs that fit and the judge's own scratch as its temp dir, and gives the
  # df and the cargo of this fixture nothing to hide behind.
  # The doubles are on PATH, as merge-gate.sh does with stat (FAKE_FS).
  printf '%s\n' '#!/bin/sh' \
    '[ "${FAKE_DF_FAIL:-}" = 1 ] && exit 1' \
    'for last; do :; done' \
    "echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'" \
    'echo "fake 0 0 ${FAKE_DF_AVAIL_KB} 0% $last"' > "$tmp/bin/df"
  chmod +x "$tmp/bin/df"
  kb=1048576   # one GB, in the KB that df -Pk prints
  q_home="$tmp/qhome"
  q_sys="$tmp/qsystemp"
  q_scr="$q_sys/memory-industry-mutants"
  q_sys_native="$(cygpath -w "$q_sys" 2>/dev/null || printf '%s' "$q_sys")"
  q_calls="$(cygpath -m "$tmp" 2>/dev/null || printf '%s' "$tmp")"
  q_avail=$((500 * kb))
  q_per=10
  # The judge runs in a throwaway copy of its two scripts over an empty rust/:
  # mutate_and_judge asks mutants-gate.sh to look at $ROOT/rust and at the temp
  # dir, and the real checkout may hold a target-* a developer left behind, which
  # would turn every row red for a reason that is not the row's. package.json
  # comes along because gate_linux_env reads engines.node from it.
  mk_qtree() {
    mkdir -p "$tmp/$1/scripts" "$tmp/$1/rust"
    cp "$ROOT/scripts/mutants-gate.sh" "$ROOT/scripts/gate-lock.sh" "$tmp/$1/scripts/"
    [[ ! -f "$ROOT/package.json" ]] || cp "$ROOT/package.json" "$tmp/$1/"
  }
  mk_qtree qtree
  q_tree="$tmp/qtree"
  q_fresh() { rm -rf "$q_sys" "$q_home" "$tmp/cargo.invoked" "$tmp/cargo.calls" "$tmp/leak.debug"; mkdir -p "$q_sys" "$q_home"; }
  # mj [VAR=value]... -- ARGS...: mutate_and_judge ARGS in that tree, with the
  # fixture's HOME and temp dir and nothing of the caller's environment that
  # the step is meant to set itself. Its output lands in $tmp/mut.out and its
  # status in $mut_rc. It runs in a subshell, which is also where a variable the
  # step exported would show: leak.debug is what CARGO_PROFILE_DEV_DEBUG was
  # once the step had returned.
  mj() {
    local -a extra=()
    while [[ "${1:-}" != -- ]]; do extra+=("$1"); shift; done
    shift
    mut_rc=0
    (
      ROOT="$q_tree"
      cd "$q_tree"
      unset CARGO_PROFILE_DEV_DEBUG CARGO_INCREMENTAL MUTANTS_JOBS MUTANTS_SCRATCH_DIR \
        MUTANTS_SCRATCH_GB_PER_COPY CUBA_GATE_MIN_FREE_GB CUBA_GATE_LOCK_OWNER FAKE_DF_FAIL FAKE_OUTCOME
      export HOME="$q_home" TMP="$q_sys_native" TEMP="$q_sys_native" TMPDIR="$q_sys" \
        PATH="$tmp/bin:$PATH" FAKE_DF_AVAIL_KB="$q_avail" FAKE_CALLS_DIR="$q_calls"
      [[ -z "$q_per" ]] || export MUTANTS_SCRATCH_GB_PER_COPY="$q_per"
      for kv in ${extra[@]+"${extra[@]}"}; do export "${kv?}"; done
      inner=0
      mutate_and_judge "$@" || inner=$?
      printf '%s' "${CARGO_PROFILE_DEV_DEBUG:-}" > "$tmp/leak.debug"
      exit "$inner"
    ) >"$tmp/mut.out" 2>&1 || mut_rc=$?
  }
  mj_said() { grep -qF -- "$1" "$tmp/mut.out"; }
  mj_invoked() { [[ -f "$tmp/cargo.invoked" ]]; }
  # Field N of the first call of cargo mutants the stand-in saw (tab-separated, and
  # some are empty, which `read` would collapse).
  mj_field() { grep -m1 '^mutants' "$tmp/cargo.calls" | awk -F'\t' -v n="$1" '{print $n}'; }
  qnorm() { cygpath -m "$1" 2>/dev/null || printf '%s' "$1"; }

  # QG1: not even one copy fits, so cargo mutants is never run.
  q_fresh
  q_avail=$((12 * kb))
  mj MUTANTS_JOBS=3 -- --file src/a.rs
  if (( mut_rc == 0 )) || mj_invoked || ! mj_said '12.0 GB' || ! mj_said '18.0 GB'; then
    echo "FAIL self-test: QG1: with 12 GB free for a 10 GB copy and 8 GB reserve the step invoked cargo mutants, or did not say the 12.0 GB it had and the 18.0 GB it needed (exit $mut_rc, invoked: $(mj_invoked && echo yes || echo no)): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi
  # QG5: not knowing the free space is not having it.
  q_fresh
  q_avail=$((500 * kb))
  mj FAKE_DF_FAIL=1 -- --file src/a.rs
  if (( mut_rc == 0 )) || mj_invoked || ! mj_said 'could not read the free space'; then
    echo "FAIL self-test: QG5: a df that prints nothing did not stop the step before cargo mutants with 'could not read the free space' (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi
  # QG2: an explicit MUTANTS_JOBS does not pass over the disk: 30 GB hold two
  # copies of 10 GB over the 8 GB reserve, not three.
  q_fresh
  q_avail=$((30 * kb))
  mj FAKE_OUTCOME=genuine MUTANTS_JOBS=3 -- --file src/a.rs
  if (( mut_rc != 0 )) || ! mj_invoked || ! mj_said 'capped by disk: 3 wanted, 2 fit' \
    || ! [[ "$(mj_field 1)" == *' --jobs 2 '* ]]; then
    echo "FAIL self-test: QG2: MUTANTS_JOBS=3 over disk for 2 did not reach cargo mutants as --jobs 2 with 'capped by disk: 3 wanted, 2 fit' (exit $mut_rc, argv: $(mj_field 1)): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi
  # QG3 and QG7: cargo mutants gets the judge's scratch as its temp dir, in the
  # three spellings (TMP and TEMP are what Windows reads, TMPDIR what Unix does),
  # and debug info off (7.6 of the 14.1 GB of a copy's target were .pdb). That
  # variable is for that one command: it is not in the step's environment after.
  q_fresh
  q_avail=$((500 * kb))
  mj FAKE_OUTCOME=genuine -- --file src/a.rs
  mj_invoked || { echo "FAIL self-test: QG3: with 500 GB free the step did not run cargo mutants: $(cat "$tmp/mut.out")" >&2; exit 1; }
  for field in 2 3 4; do
    if [[ "$(qnorm "$(mj_field "$field")")" != "$(qnorm "$q_scr")" ]]; then
      echo "FAIL self-test: QG3: field $field of the cargo mutants call (TMP, TEMP, TMPDIR) was '$(mj_field "$field")', not the judge's scratch $q_scr" >&2
      exit 1
    fi
  done
  if [[ "$(mj_field 5)" != 0 ]]; then
    echo "FAIL self-test: QG7: cargo mutants did not get CARGO_PROFILE_DEV_DEBUG=0 (got '$(mj_field 5)')" >&2
    exit 1
  fi
  if [[ -n "$(cat "$tmp/leak.debug" 2>/dev/null)" ]]; then
    echo "FAIL self-test: QG7: CARGO_PROFILE_DEV_DEBUG was still exported after the step returned: it is for one cargo mutants call, not for the rest of the judge" >&2
    exit 1
  fi
  # QG4: the judge's own scratch is swept of what is older than 6 h before the
  # disk is measured, and what is newer stays.
  q_fresh
  mkdir -p "$q_scr/cargo-mutants-rust-old.tmp" "$q_scr/cargo-mutants-rust-new.tmp"
  : > "$q_scr/cargo-mutants-rust-old.tmp/x"
  : > "$q_scr/cargo-mutants-rust-new.tmp/x"
  touch -d '7 hours ago' "$q_scr/cargo-mutants-rust-old.tmp"
  touch -d '1 hour ago' "$q_scr/cargo-mutants-rust-new.tmp"
  mj FAKE_OUTCOME=genuine -- --file src/a.rs
  if [[ -e "$q_scr/cargo-mutants-rust-old.tmp" || ! -d "$q_scr/cargo-mutants-rust-new.tmp" ]]; then
    echo "FAIL self-test: QG4: the run did not sweep the 7 h old copy of the judge's scratch and leave the 1 h old one: $(ls "$q_scr" 2>&1)" >&2
    exit 1
  fi
  # QG8: a cargo target inside rust/ other than target/ is copied whole by
  # cargo-mutants (25.8 GB of four of them on 2026-10-13), so the step stops
  # before it, naming the directory.
  mk_qtree qtaint
  mkdir -p "$tmp/qtaint/rust/target-x"
  : > "$tmp/qtaint/rust/target-x/CACHEDIR.TAG"
  q_tree="$tmp/qtaint"
  q_fresh
  mj FAKE_OUTCOME=genuine -- --file src/a.rs
  q_tree="$tmp/qtree"
  if (( mut_rc == 0 )) || mj_invoked || ! mj_said 'target-x'; then
    echo "FAIL self-test: QG8: a rust/target-x holding CACHEDIR.TAG did not stop the step before cargo mutants, naming it (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi

  # The two runs that were here before G3, now in the fixture's tree and with room.
  q_fresh
  mj FAKE_OUTCOME=genuine -- --file src/a.rs
  if (( mut_rc != 0 )) || ! grep -q 'all 0 refused by the compiler' "$tmp/mut.out"; then
    echo "FAIL self-test: a mutation run with one genuine catch did not pass through the build check (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi
  q_fresh
  mj FAKE_OUTCOME=killed -- --file src/a.rs
  if (( mut_rc == 0 )) || ! grep -q 'stopped 1 of 1 unviable' "$tmp/mut.out"; then
    echo "FAIL self-test: a build the machine killed was not refused, so cargo-mutants and this script did not read the same outcomes.json (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi

  echo "OK  self-test: the CRAP half reports CC violations, only CC violations, and"
  echo "    keys each function by name and ordinal, so moving one does not move its"
  echo "    key and two same-named functions in one file keep two; the ratchet is exact"
  echo "    in both directions and red on entries that are dead or in the wrong shape;"
  echo "    an exclusion goes red when it expires, loses its date, or matches no mutant;"
  echo "    regenerating the baseline keeps each comment over its key and is idempotent; the mutation"
  echo "    step hands cargo-mutants an output path it can see, and judges what it wrote; it asks"
  echo "    for the jobs the disk holds before cargo mutants, an explicit MUTANTS_JOBS included,"
  echo "    runs it in the judge's own scratch with debug info off, and stops at a cargo target"
  echo "    inside rust/"
  exit 0
fi

# TMPDIR off tmpfs and the target outside the tree, before cargo mutants copies
# anything: gate-lock.sh says why. After the self-test, which needs neither.
# shellcheck source=scripts/gate-lock.sh
source "$ROOT/scripts/gate-lock.sh"
gate_linux_env

# Before the diff, and whatever the diff holds: an exclusion expires by the
# calendar, not because somebody touched the file it covers.
echo "=== exclusions of the mutation step: expiry dates and patterns still in use ==="
expiry="$(exclusion_expiry_problems "${BASH_SOURCE[0]}" "$(date +%F)")"
if [[ -n "$expiry" ]]; then
  echo "FAIL: an exclusion of the mutation step expired, or its date cannot be read:" >&2
  printf '      %s\n' "$expiry" >&2
  echo "      Renew it with a reason that is still true today, or delete the pattern." >&2
  fail=1
else
  echo "OK  no exclusion past its date ($(date +%F))"
fi
if command -v cargo-mutants >/dev/null || (cd rust && cargo mutants --version >/dev/null 2>&1); then
  mutant_list="$(mktemp)"
  if (cd rust && cargo mutants --list) >"$mutant_list" 2>/dev/null && [[ -s "$mutant_list" ]]; then
    unused="$(unused_exclusions "$MUTANTS_EXCLUDE_RE" "$mutant_list")"
    if [[ -n "$unused" ]]; then
      echo "FAIL: these --exclude-re entries match none of the $(wc -l <"$mutant_list") mutants of the crate:" >&2
      printf '      %s\n' "$unused" >&2
      echo "      An exclusion that excludes nothing reads like a cover. If its line moved," >&2
      echo "      re-anchor it; if its function is gone, delete it with its reason." >&2
      fail=1
    else
      echo "OK  every exclusion matches at least one of the $(wc -l <"$mutant_list") mutants of the crate"
    fi
  else
    echo "FAIL: cargo mutants --list printed nothing, so nobody can say which exclusions still match." >&2
    fail=1
  fi
  rm -f "$mutant_list"
else
  echo "FALTA cargo-mutants: no se puede saber si cada exclusión sigue casando con algún mutante."
  [[ $fail -eq 0 ]] && fail=2
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
          # The entries of MUTANTS_EXCLUDE_RE (top of this file) are not reachable by
          # `cargo mutants -- --lib`, each for its own reason, and every one of
          # them has its decision tested somewhere the mutation CAN reach.
          #
          # A group zero: twelve of the thirteen that 11cdc57 (0.26) put in the
          # regex with no reason written anywhere. The commit message gives the
          # class ("--lib never awaits an async handler that needs a
          # database"); the reason of each was read in the code on
          # 2026-09-23, one by one. The thirteenth, builtin_retrieval_set, had
          # none: a pure function --lib could always call. It got its unit
          # test in 0.28 (the_builtin_set_is_one_the_harness_can_score, in
          # eval/datasets.rs) and left the regex. They are bare names, so each hides every
          # mutant of its function, the body included, and anything whose
          # name contains it (run_check also covers run_checks_with). "Judged
          # by" is where the SIL kills the mutant that empties the function;
          # "judged NOWHERE" is a hole this exclusion does not close.
          # Owner: endurecedor 0.26 (11cdc57). Expires: 2027-03-22.
          #
          #   Needs a database, which --lib has no pool for:
          #   fetch_adjacency   the two in graph/kcore.rs and graph/closeness.rs.
          #                     Reached only through cuba_vigia metric=structural,
          #                     which neither the E2E nor any rust/tests file
          #                     asks for. Judged NOWHERE. The halves that decide,
          #                     compute_in_memory in both files, have unit tests.
          #   list_resources, read_resource
          #                     protocol.rs, the MCP resources/list and
          #                     resources/read methods. No test and no E2E call
          #                     sends either method. Judged NOWHERE.
          #   backfill_unscoped project.rs, behind `proyecto backfill` and
          #                     `memory-industry project backfill`. No test sends
          #                     that action. Judged NOWHERE.
          #   observation_in_scope
          #                     project.rs, the scope check of every cuba_eco
          #                     action. The E2E's test_cuba_eco calls it on an
          #                     observation of the current scope; whether a
          #                     refusal fails that test was not traced, and no
          #                     test hands eco an observation of another project,
          #                     so `with Ok(true)` is judged NOWHERE.
          #
          #   Dormant today: their files are in the CLI/doctor case above, so
          #   they never reach --file. They matter the day that case shrinks,
          #   and are written so that day does not arrive without a reason:
          #   run_checks_with   doctor.rs, needs a pool. Judged by
          #                     v032_reranker_says_why_its_off (--ignored), which
          #                     expects the reranker check in what it returns.
          #   upsert_symbol, upsert_placeholder_entity
          #                     codegraph_cli.rs, need a connection. upsert_symbol
          #                     is exercised by
          #                     v017_codegraph_hardening::rebuilding_after_an_edit_refreshes_the_row_instead_of_orphaning_it
          #                     (--ignored), whose first assertion wants the symbol
          #                     recorded (reasoned from the code, not run against
          #                     the mutant). upsert_placeholder_entity: no
          #                     assertion found that reads what it writes.
          #   run_project       cli.rs, `memory-industry project`. `with Ok(())`
          #                     is judged by
          #                     cli_contract::every_listed_command_is_actually_dispatched:
          #                     the usage line of `project --help` is printed
          #                     from inside the body. The rest of the body needs
          #                     a database: judged NOWHERE.
          #   run_check, run_write, workspace_client_id
          #                     setup_agent.rs, `memory-industry setup`. They read
          #                     and write the MCP configs under the real home
          #                     (~/.claude.json, ~/.cursor/mcp.json) and read the
          #                     cwd, which no --lib test can move without moving
          #                     it for every other test in the binary. Judged
          #                     NOWHERE.
          #
          # This first group of five came in with b4db21c, after 11cdc57 set the
          # version to 0.26.0, so it is 0.27 work, like the third group below.
          # Owner: endurecedor 0.27 (b4db21c). Expires: 2027-03-22.
          #
          #   serve_pool       binds a socket and serves forever. --lib never
          #                     starts a daemon. Same class as the handlers.
          #   gpu_availability  two cfg variants, and this build now calls it from
          #                     both configure and status. Neither observes the
          #                     answer. In configure, wants_gpu is false here, so
          #                     cpu_reason returns on its first branch and discards
          #                     the other two arguments. In status, every value of
          #                     the pair yields one of the four sentences
          #                     status_from can honestly produce, and
          #                     status_reports_the_provider_this_binary_was_compiled_with
          #                     accepts all four on purpose: it judges the provider
          #                     argument, not the measurement. The two halves it
          #                     composes are judged apart, which is what this
          #                     release changed. runtime_has_gpu_provider is
          #                     compiled in this build now and has
          #                     a_provider_library_beside_the_runtime_is_what_makes_it_a_gpu_runtime,
          #                     which puts a provider library on disk and takes it
          #                     away. nvidia_driver_present has nothing under --lib:
          #                     it reads PATH and /proc, and a test that moved PATH
          #                     would move it for every other test in the binary,
          #                     including the ones that spawn git. It is judged by
          #                     scripts/gpu-placement-check.sh, which reads the same
          #                     two places on the same machine and fails the gate on
          #                     the contradiction — a real judge, but not one the
          #                     mutation build can reach. Declared, not covered.
          #   cuda_provider     #[cfg(feature = "cuda")]; not compiled here.
          #   apply             writes the process-wide environment, which every
          #                     other test in the binary reads. That is exactly
          #                     why plan_env is pure and tested; what is left is
          #                     three lines of set_if_absent.
          #   failure_reason    reads a OnceLock that --lib cannot populate
          #                     without loading 1.1 GB of model. reason_of, the
          #                     half that decides, has a table. In the SIL,
          #                     v045_health_names_the_model_that_would_not_open
          #                     fails it through the daemon's own warm-up and
          #                     wants `failed` in /health; `with None` reads
          #                     `loaded` there. --lib never runs that file, so the
          #                     exclusion stays.
          #
          # A second group, all of the same two shapes. Nothing here is
          # unexamined: the half of each that decides something was pulled out
          # and given a table where the mutation can reach it. All of it came in
          # with a6dffbb except `panel`, which caeadc7 added beside mcp_endpoint;
          # 7475681 gave `panel` and `judge_is_sampling` their trailing space.
          # Owner: endurecedor 0.27 (a6dffbb, caeadc7). Expires: 2027-03-22.
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
          # the platform this judge runs on.
          # Owner: endurecedor 0.27. Expires: 2027-03-21.
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
          #
          # A fifth group: the 41 survivors of QG_BASE=73da2d8 (the 0.28 work)
          # that no --lib test can kill.
          # Owner: endurecedor 0.28. Expires: 2027-03-22.
          # Every pattern starts at `src/` and names the file in
          # full: `db.rs` alone also matches src/graph_db.rs. Every function
          # pattern carries ` -> ` or ` with ` after the name, for the prefix
          # rule above, and ends in the replacement that survived, so the
          # mutants inside each body and the siblings that die today stay
          # judged. Where a pattern names a line:col it is a claim about one
          # expression: when the line moves the pattern stops matching and
          # the mutant comes back as MISSED, which is the loud direction.
          # "Judged by" names the test that kills the mutant in the SIL
          # (scripts/run-all-tests.sh). "Judged nowhere" is a hole this
          # exclusion does not close; it only stops the hole reading as new.
          #
          #   Equivalent, with the proof:
          #   src/redact.rs:110:63 `at_sign > *end` to `>=`
          #                     at_sign is the index of a '@' and end the index
          #                     of the ':' that starts "://". One byte cannot be
          #                     both, so the two are never equal and >= decides
          #                     exactly what > does.
          #   src/redact.rs:130:58 `*sep > 0` to `>=`
          #                     the only new case is sep == 0, where the key is
          #                     "" and secret_field_pattern("") ends_with none of
          #                     SECRET_FIELD_NAMES, so the next `?` returns the
          #                     same None the filter did.
          #   src/redact.rs:136:51 `sep + 1` to `sep * 1` in the value slice
          #                     the slice gains one char, always the separator
          #                     ('=' or ':'), and value_is_opaque starts with
          #                     trim_matches(|c| !c.is_alphanumeric()), which
          #                     strips it before counting. The line:col is
          #                     load-bearing: the `sep + 1` on line 133 is a
          #                     different mutant and is not equivalent.
          #
          #   A process-wide OnceLock that --lib cannot put in a known state
          #   without depending on test order, and that loads a model on a
          #   machine that has one (precedent: rerank failure_reason, nli init):
          #   nli.rs enabled (true, false)
          #                     judged by tests/nli_entailment.rs in the NLI
          #                     section: `with false` fails require_nli_model,
          #                     `with true` skips the load and entails() errs on
          #                     "sesión NLI no inicializada".
          #   nli.rs failure_reason with None, onnx.rs failure_reason with None
          #                     judged by
          #                     v045_health_names_the_model_that_would_not_open,
          #                     in the plain `cargo test` of the SIL: it points
          #                     every model at a runtime and a model that are not
          #                     one, forces NLI through nli::enabled() in the
          #                     daemon's process, and wants `failed` with a reason
          #                     and no path in /health. With None the embedder
          #                     reads `fallback` and NLI `loaded` or `absent`. That
          #                     test lives in rust/tests/, and `cargo mutants --
          #                     --lib` never runs rust/tests/, so here both would
          #                     still come back MISSED: the exclusion stays for
          #                     --lib only.
          #   nli.rs status_resolved with false
          #                     judged NOWHERE through the live cell. Only a
          #                     `loaded` NLI reads it, and `loaded` takes a
          #                     session that opens, which takes the real model:
          #                     the SIL loads it in nli_entailment, behind
          #                     require_present, and that file never reads
          #                     /health. A /health test beside it would load the
          #                     1 GB model once more for this one mutant, and v045
          #                     cannot, since the cell it needs broken is the
          #                     same one. The pure half, nli_state_from, has a
          #                     table.
          #   onnx.rs compute_embedding (the four vec! values)
          #                     a one-line wrapper over compute_embedding_with,
          #                     which is tabled. Judged by
          #                     v016_chunking::a_long_observation_becomes_reachable_past_the_truncation_limit:
          #                     any of the four vectors fails the UPDATE into
          #                     vector(384).
          #   http.rs col 24 `ready && is_model_loaded()` to `||`
          #                     a REAL mutant, not an equivalent: the doc comment
          #                     over embedder_state says that && is the guard
          #                     that keeps /health from loading the model. With
          #                     || the_embedder_is_warming_while_the_models_are_still_loading
          #                     still reads "warming" and only pays the load.
          #                     Judged NOWHERE. The anchors are the column (:24:)
          #                     and the end of the name (`$`): `in embedder_state`
          #                     is a prefix of `in embedder_state_from`, and
          #                     without the `$` the pattern would also hide the
          #                     mutants of that function. The line number is NOT
          #                     an anchor (`[0-9]+`): every edit above the
          #                     function in http.rs moves it (W6 moved 1457
          #                     an hour after it was re-anchored there). Other
          #                     && on col 24 of another function are still
          #                     judged, since the name has to end in
          #                     embedder_state.
          #
          #   CLI entry points (precedent: serve_pool and the handlers). What
          #   runs these is cli_contract::every_listed_command_is_actually_dispatched,
          #   in the plain `cargo test` of the SIL (scripts/run-all-tests.sh).
          #   Since 38e8894 it asks every command in cli::COMMANDS for
          #   `<cmd> --help` and wants exit 0, a line that starts with
          #   `memory-industry <cmd>`, and no connection to PostgreSQL. It is an
          #   integration test that spawns the binary, and this judge runs
          #   `cargo mutants -- --lib`, so the kill happens where the mutation
          #   cannot see it. Read in the code at 2b61223, one by one: main.rs
          #   hands `<cmd> --help` straight to the excluded body, and the usage
          #   line is printed from inside that body, so a body replaced by
          #   Ok(()) prints nothing and fails the contract:
          #   run_cli in calibrate_cli, dashboard, export, link_cli, reembed_cli,
          #   rem_cli, secure_cli, skills_cli, sync_cli, eval/mod
          #                     judged by that contract. The eval smoke of the
          #                     SIL also runs eval/mod.rs run_cli, but reads
          #                     only its exit code, which Ok(()) keeps.
          #   dedupe_cli.rs run_cli
          #                     judged by that contract too, and by the #[ignore]
          #                     lib tests
          #                     a_hand_verified_merge_moves_everything_and_leaves_an_alias
          #                     and merge_without_into_refuses_instead_of_guessing.
          #   models_cli.rs print_help
          #                     `models --help` reaches it through the
          #                     "" | "-h" | "--help" | "help" arm of run_cli, and
          #                     it prints the only line that starts with
          #                     `memory-industry models`: judged by that contract.
          #   setup.rs log
          #                     not the help of any command: it prints the setup
          #                     messages of resolve_database_url, which the
          #                     contract forbids --help to reach. Judged NOWHERE.
          #
          #   Needs a database, which --lib has no pool for (precedent:
          #   fetch_adjacency, upsert_symbol, backfill_unscoped):
          #   dedupe_cli.rs merge_by_name
          #                     judged by a_hand_verified_merge_moves_everything_and_leaves_an_alias.
          #   protocol.rs run_rem_consolidation_locked
          #                     judged by v031_rem_wiring::community_detection_assigns_every_entity_a_community_after_one_rem_cycle
          #                     and v037_a_cycle_that_dies_is_recorded_by_the_wrapper.
          #   The next four were judged nowhere until 54dd63b. Their tests carry
          #   no #[ignore] and each creates and drops its own database, so they
          #   run in the plain `DATABASE_URL="$GATE_DATABASE_URL" cargo test` of
          #   the SIL (scripts/run-all-tests.sh), with the model paths exported.
          #   This judge runs `cargo mutants -- --lib`, which never builds a
          #   file under rust/tests/: the kill happens in the SIL, out of the
          #   mutation's sight, and that is why the exclusions stay.
          #   dashboard.rs render
          #                     judged by v043_the_dashboard_reads_the_base_it_is_pointed_at,
          #                     its one test. run_cli writes whatever render
          #                     returns, and neither "" nor "xyzzy" carries the
          #                     observation and the entity it seeded just before.
          #   export.rs export_obsidian
          #                     judged by v043_calibration_and_export_read_what_was_written::the_obsidian_export_reports_the_notes_it_actually_wrote.
          #                     The mutant writes no note. Ok(1) then disagrees
          #                     with the empty directory; Ok(0) agrees with it,
          #                     and dies on the first of the two seeded entities,
          #                     whose note is not there to read.
          #   search/calibrate.rs load_ood_threshold
          #                     judged by the_calibrated_threshold_comes_back_only_for_the_dimension_it_was_measured_on,
          #                     same file. It wants None before anything is stored,
          #                     which kills Some(0.0), Some(1.0) and Some(-1.0), and
          #                     exactly Some(0.4375) after, which kills None too.
          #   db.rs assert_embedding_dim
          #                     judged by v043_a_model_of_another_width_does_not_start,
          #                     which wants an Err for a vector column twice the
          #                     model's width, and gets Ok(()) from the mutant.
          #                     Only with the ONNX embedder loaded: without one
          #                     the original returns Ok(()) before reading any
          #                     column, which is why that test asserts the model
          #                     is loaded before anything else.
          #
          #   Network or a live process:
          #   models_cli.rs download_model, download_runtime
          #                     download from the internet: judged NOWHERE.
          #   protocol.rs spawn_handshake_watchdog
          #                     sleeps 60 s and then calls process::exit, which
          #                     would end the test binary. Judged NOWHERE.
          #
          #   The timeout:
          #   onnx.rs locate_onnxruntime with Some(Default::default())
          #                     the mutant builds a state the function cannot
          #                     reach: it only returns a path it saw exist(), and
          #                     "" never does. The three locate tests in
          #                     onnx.rs would reject it; the run hangs instead,
          #                     for 90 s. That an impossible "" hangs instead of
          #                     failing is a robustness finding and a separate
          #                     follow-up; it is not hidden here, and a test
          #                     could not change the result, since libtest waits
          #                     for the test that hangs.
          #
          # A sixth group of one: the body of the binary, which entered the
          # diff when 2b61223 touched a line of it.
          # Owner: endurecedor 0.28 (2b61223). Expires: 2027-03-22.
          #
          #   src/main.rs async_main with ()
          #                     the whole of the binary, the same class as
          #                     serve_pool: --lib never starts a binary. With
          #                     the body gone the binary exits 0 and prints
          #                     nothing, whatever it is asked. Judged by
          #                     cli_contract::every_listed_command_is_actually_dispatched
          #                     (no usage line for any command) and by the E2E
          #                     of the SIL, tests/e2e_all_tools.py, whose every
          #                     call gets an empty stdout. The pattern ends in
          #                     the replacement on purpose: `in async_main`
          #                     alone would match, by substring, every future
          #                     mutant of the body. What that body decides is
          #                     pulled out where --lib reaches it, as
          #                     cli::asks_for_help did for `serve --help`.
          #                     Today the case above already keeps src/main.rs
          #                     out of the --file set, so this pattern only
          #                     starts to matter the day that skip goes; it is
          #                     written now so that day does not arrive with
          #                     the reason missing.
          #
          # A seventh group: the survivors of the head of 0.27
          # (feat/0.27-despliegue-y-trinquetes at c34dd07) that no --lib test
          # can kill. Same shape as the fifth: file in full, ending in the
          # replacement, so the siblings and the mutants inside each body stay
          # judged. The four of secure_cli.rs that were MISSED there (summary
          # and derive_app_url, which 0.28 moved to setup.rs) are not here: they
          # die in secure_cli's mod tests.
          # Owner: endurecedor 0.27 (c34dd07). Expires: 2027-03-22.
          #
          #   Needs a database (precedent: assert_embedding_dim, merge_by_name):
          #   db.rs bind_app_role with Ok(())
          #                     judged by two tests of
          #                     v044_a_new_app_role_does_not_get_a_published_password.
          #                     Without the two settings the DO block of
          #                     create-app-role.sql raises «memory_industry.app_role
          #                     is not set» before it reads or touches any role
          #                     (a060438 took away its cuba_app default), so
          #                     ensure_app_role errs and
          #                     a_role_created_by_secure_does_not_open_with_the_password_in_the_repo
          #                     fails creating the probe role and
          #                     a_role_that_already_exists_keeps_the_password_its_daemon_uses
          #                     fails running the setup over it. Red whether or
          #                     not the server has a cuba_app, and that role is
          #                     left alone either way.
          #   db.rs init_schema with Ok(())
          #                     judged by
          #                     v043_the_dashboard_reads_the_base_it_is_pointed_at::the_dashboard_shows_an_observation_it_read_from_the_base:
          #                     its scratch database starts empty, create_pool is
          #                     what migrates it, and without the migrator the
          #                     cuba_cronica seed finds no table.
          #   db.rs provision_app_role with (), write_app_role_password with Ok(())
          #                     the rotation of a role's password, which the
          #                     daemon applies to cuba_app when it starts as
          #                     admin. Judged by
          #                     v044_a_new_app_role_does_not_get_a_published_password::provisioning_moves_an_existing_role_onto_the_new_password_and_off_the_old_one,
          #                     in the SIL: it hands provision_app_role a
          #                     throwaway role of its own and wants it to log in
          #                     with the new password and not with the old one,
          #                     which both no-ops fail. That test lives in
          #                     rust/tests/, and `cargo mutants -- --lib` never
          #                     runs rust/tests/, so here both would still come
          #                     back MISSED: the exclusion stays for --lib only.
          #   db.rs provision_app_role_from_pgpass with ()
          #                     the step that hands provision_app_role this
          #                     machine's pgpass_app and the real cuba_app, pulled
          #                     out of init_schema in 0.28. Judged NOWHERE in
          #                     practice: v044_secure_runs_again_on_an_install_it_secured
          #                     fails on it only while the gate's server holds
          #                     cuba_app on a password other than this HOME's
          #                     pgpass_app, and every test that calls create_pool
          #                     puts that one there, so after one run the no-op is
          #                     invisible. A test would have to rewrite the
          #                     server-wide cuba_app to another password, which
          #                     is what v044 avoids on purpose.
          #                     Owner: implementador 0.28 (tajada B). Expires: 2027-03-22.
          #
          #   Detected, and then the run cannot finish on this platform:
          #   onnx.rs load_onnxruntime with Ok(()), load_located_onnxruntime with Ok(())
          #                     both are caught: the first by
          #                     an_empty_runtime_path_is_refused_by_name_and_never_handed_to_ort,
          #                     the second by
          #                     with_no_runtime_anywhere_loading_the_located_one_says_how_to_get_it.
          #                     But with `ort` never initialised, the first later
          #                     test that opens a session loads the default
          #                     onnxruntime.dll, which on this Windows machine is
          #                     the Windows ML 1.17 build in C:\Windows\System32,
          #                     and hangs loading it; every test waiting on
          #                     GLOBAL_STATE_GUARD queues behind it and the mutant
          #                     ends as TIMEOUT, not CAUGHT. Measured in the
          #                     mutants.out logs of the directed run over c34dd07.
          #                     On Linux the same mutant should fail fast: the day
          #                     this judge runs there, re-measure and delete both
          #                     patterns if they come back CAUGHT.
          #
          # An eighth group: the four mutants of the migration path of db.rs
          # that no --lib test can kill. Same shape as the fifth and seventh:
          # file in full, ` -> ` after the name for the prefix rule (`replace
          # migrate -> ` does not hide migrate_and_provision), and ending in
          # the replacement, so the siblings stay judged.
          # Owner: endurecedor 0.28. Expires: 2027-03-22.
          #
          #   Needs a database, which --lib has no pool for (precedent:
          #   init_schema, bind_app_role):
          #   db.rs migrate_and_provision -> Result<()> with Ok(()),
          #   migrate, realign_then_migrate -> Result<(), MigrateError> with Ok(())
          #                     the first three leave the scratch database
          #                     without `_sqlx_migrations`. Judged by
          #                     v044_a_migration_recorded_in_crlf_still_starts::a_record_in_the_other_line_endings_is_realigned_and_no_other_is,
          #                     in the plain `cargo test` of the SIL
          #                     (merge-gate.sh through run-all-tests.sh): it
          #                     panics reading the checksum of migration 63
          #                     from a table that is not there.
          #   db.rs realign_line_ending_checksums -> sqlx::Result<()> with Ok(())
          #                     the mutant leaves the CRLF record alone, and
          #                     the restart dies on sqlx's «previously applied
          #                     but has been modified». Judged by the same
          #                     test. That test lives in rust/tests/, and
          #                     `cargo mutants -- --lib` never runs
          #                     rust/tests/, so here all four would still come
          #                     back MISSED: the exclusion stays for --lib only.
          #
          # A ninth group: the twelve survivors of the 0.29 head in rust/src/http.rs
          # that need a database. Each sits in an async function that asks
          # accounts_db (brain_account_credentials, brain_pairing_codes) and has
          # no pool under --lib, so no unit test reaches the branch that decides.
          # The tests that kill them are in rust/tests/v029_accounts_over_real_http.rs:
          # they start a real daemon over a scratch database and check what a
          # client sees (status, body, audit events). The SIL's plain `cargo test`
          # runs that file with DATABASE_URL set (scripts/run-all-tests.sh, line
          # with `cargo test`, no --ignored). This judge runs `cargo mutants -- --lib`,
          # which never builds a file under rust/tests/, so here all twelve would
          # still come back MISSED: the exclusion stays for --lib only.
          # Every pattern names the function and the replacement that survived, not
          # the line:col, so the sibling mutants of the same function stay judged
          # where a unit test reaches them. The `'` of `&'static str` is written
          # `\x27`: the pattern sits inside a single-quoted bash string.
          # Owner: endurecedor 0.29. Expires: 2027-03-22.
          #
          #   identify_account, three mutants: `||` to `&&` on the credential state
          #   check, `!=` to `==` on it, and `!` deleted from `!accounts::ct_eq`.
          #                     Judged by
          #                     v029_accounts_over_real_http::g10_a_revoked_device_or_account_stops_on_the_very_next_request:
          #                     a revoked device with its right secret must be a 401
          #                     with «invalid bearer token» on the very next request,
          #                     and every live credential must still answer 200 before
          #                     the revocation. The `||` to `&&` mutant admits the
          #                     revoked device; the `!=` and `!` mutants refuse the
          #                     live ones. Also judged by
          #                     v029_accounts_over_real_http::g11_every_kind_of_bad_credential_is_a_401_that_counts_toward_the_brake,
          #                     which wants a 401 for an existing id with the wrong secret.
          #
          #   pair_answer with None
          #                     the host's signed answer to a redemption. With None a
          #                     valid code answers 503 where the proof belongs.
          #                     Judged by
          #                     v029_accounts_over_real_http::g4_pairing_creates_the_credential_and_the_host_proves_it_knew_the_code:
          #                     the valid code must answer 200 with a MAC that verify_mac
          #                     accepts under the key derived from the code.
          #
          #   claim_and_answer, `!` deleted from `if !created`
          #                     a first redemption must record exactly one
          #                     account.pair_redeemed audit event, and a replay none.
          #                     Without the `!` the first redemption answers as a
          #                     replay and records nothing. Judged by the same g4 test,
          #                     which counts the events for the one redemption it makes.
          #
          #   credential_proof, seven mutants: six replacements of the whole return
          #   value (`Ok(("", None))` and the other five), and `!=` to `==` on the
          #   state check. The /identity verdict and proof come only from the
          #   database. Judged by
          #   v029_accounts_over_real_http::g2_g3_identity_proves_knowledge_of_an_active_credential_and_only_that:
          #   an active credential must answer "active" with the proof the client
          #   computes over the host, the credential and the nonce; a revoked one
          #   "revoked" with no proof; a credential nobody issued "unknown" with no
          #   proof. Each replacement answers a verdict that is none of those.
          #
          # The same shape as the groups above: each entry says what the mutant breaks, which test
          # kills it, and why --lib cannot see that test. Nothing here is a cover. Every test named was
          # read in rust/tests/ and asserts the observable the mutant changes.
          #
          # Needs a database, which --lib has no pool for (precedent: the db.rs groups above). Judged by
          # rust/tests/v029_accounts_storage.rs. That file creates and drops its own database and carries
          # no #[ignore], so the plain DATABASE_URL cargo test of run-all-tests.sh runs it. `cargo mutants
          # -- --lib` never builds a file under rust/tests/, so each of these would still come back MISSED
          # under --lib: the kill happens in the SIL, out of the mutation's sight.
          #   accounts_db.rs list_accounts with Ok(vec![])
          #                     the_list_is_ordered_by_name_and_carries_each_device_without_a_secret
          #                     wants ["ana", "zed"] back from list_accounts and gets nothing.
          #   accounts_db.rs lock_account with Ok(Uuid::default())
          #                     revoking_an_account_revokes_its_devices_deletes_its_unused_codes_and_can_be_repeated
          #                     wants report.credentials_revoked == 3; the nil id revokes nothing.
          #   accounts_db.rs revoke_rows, `>` with `<`
          #                     the same test: it wants an account.revoke audit event naming ana.
          #   accounts_db.rs revoke_rows, `>` with `>=`
          #                     the same test: after the repeat it wants exactly one account.revoke
          #                     event (assert_eq! on events.len() == 1), so a second write is caught.
          #   accounts_db.rs revoke_device with Ok(0) and with Ok(1)
          #                     revoking_a_device_matches_its_label_in_any_case_and_only_that_account
          #                     wants revoked == 2 and an error for an unmatched label.
          #   accounts_db.rs ensure_code_capacity with Ok(())
          #                     at_most_50_codes_are_active_at_once_and_expired_ones_do_not_count
          #                     wants the 51st active code refused, saying too many active pairing codes.
          #   accounts_db.rs store_code, `+` with `-`
          #                     a_pairing_code_expires_after_exactly_24_hours_and_is_stored_only_as_a_key
          #                     wants issued.expires_at == issue instant + 24 hours.
          #   accounts_db.rs credential_for_auth with Ok(None)
          #                     the_authentication_lookup_returns_the_hash_the_account_the_label_and_the_state
          #                     expects the credential that was paired to be found.
          #   accounts_db.rs touch_last_seen with Ok(())
          #                     last_seen_is_written_at_most_once_a_minute
          #                     wants the first touch recorded as last_seen_at.
          #   accounts_db.rs purge_pairing_codes with Ok(1), with Ok(0), and `-` with `+`
          #                     the_purge_removes_codes_used_or_expired_over_a_week_ago_and_keeps_the_rest
          #                     wants purged == 2 and the right rows kept.
          #
          # Owner: endurecedor 0.29. Expires: 2027-03-22.
          #
          # Needs a database, which --lib has no pool for. Judged by rust/tests/v029_accounts_redeem.rs,
          # plain DATABASE_URL run, no #[ignore]. The same reason as the group above: --lib never builds
          # rust/tests/.
          #
          #   accounts_db.rs pairing_candidates with Ok(vec![])
          #   accounts_db.rs pairing_candidates, `-` with `+`
          #                     used_and_expired_codes_stay_listed_for_a_week_so_they_can_be_told_apart
          #                     wants the code listed 6 days after issue and not listed 8 days after.
          #   accounts_db.rs claim_pairing_code, `==` with `!=`
          #                     a_code_is_used_once_and_the_second_claim_creates_nothing
          #                     wants the first claim of a valid code to be Created.
          #
          # Owner: endurecedor 0.29. Expires: 2027-03-22.
          #
          # Needs a database and a running daemon. Judged by rust/tests/v029_accounts_over_real_http.rs,
          # plain DATABASE_URL run, no #[ignore]. --lib cannot start the real daemon on its loopback ports.
          #
          #   accounts_db.rs record_pair_redeemed with Ok(())
          #                     g4_pairing_creates_the_credential_and_the_host_proves_it_knew_the_code
          #                     wants exactly one account.pair_redeemed event, naming ana and pc-lab-1.
          #   accounts_db.rs account_facts, `<` with `>`
          #                     g16_a_database_without_the_account_tables_is_a_doctor_failure_not_a_crash
          #                     wants account_facts to answer on a database without the tables.
          #   accounts_db.rs account_facts, `<` with `<=`
          #                     g16_account_facts_count_what_the_database_holds_and_doctor_prints_them
          #                     wants tables_present true on a migrated database.
          #
          # Owner: endurecedor 0.29. Expires: 2027-03-22.
          #
          # Not an exclusion: a change of production code, so there is no mutant to exclude.
          # accounts_db.rs account_facts used to end in `..AccountFacts::default()`, a functional update
          # that makes cargo-mutants emit five `delete field X from struct AccountFacts` mutants, and
          # --exclude-re does not match those (27.1.0). The struct literal now writes all eight fields out.
          # The three added fields (require_accounts, shared_token, lan_bind) are false, which is what
          # the derived Default gave them, so the values did not change. The mutants no longer exist.
          #
          # A tenth group: the bridge CLI survivors of 0.29 (src/bridge/mod.rs), the four functions that
          # the `bridge` subcommands call, and the comparison in cli_doctor that decides its exit code.
          # Each one is judged by rust/tests/v029_bridge_cli.rs: it starts the real binary with HOME in a
          # scratch directory, carries no #[ignore] and no needs-a-server line, and needs no database, so
          # the plain `DATABASE_URL=... cargo test` of the SIL (scripts/run-all-tests.sh) builds and runs
          # it. `cargo mutants -- --lib` never builds a file under rust/tests/, and the unit tests of
          # src/bridge/mod.rs call run_cli only with arguments that parse() refuses before any cli_* runs,
          # so here these would still come back MISSED: the kill happens in the SIL, out of the mutation's sight.
          # Owner: endurecedor 0.29. Expires: 2027-03-22.
          #
          #   cli_run with Ok(())
          #                     run_without_a_pairing_refuses_before_it_opens_a_port wants a failing exit
          #                     and the NOT_PAIRED line on stderr; the mutant exits 0.
          #                     run_with_a_pairing_prints_its_loopback_address_and_keeps_serving_until_stopped
          #                     waits for the start line `bridge listening on <addr>`; the mutant prints
          #                     nothing, the pipe closes and the test fails at once.
          #   cli_install with Ok(())
          #                     install_print_names_the_task_it_would_register runs `install --print` and
          #                     wants `schtasks`, `/Create` and `MemoryIndustry Bridge` on stdout.
          #   cli_doctor with Ok(())
          #                     doctor_without_a_pairing_refuses_and_says_how_to_pair wants a failing exit
          #                     and NOT_PAIRED on stderr; doctor_finds_a_live_host_on_loopback_and_exits_zero
          #                     wants the report line `madre: <addr> (huella <fingerprint>)` on stdout.
          #   cli_doctor, `!=` with `==`
          #                     doctor_finds_a_live_host_on_loopback_and_exits_zero: a verified host gives
          #                     exit_code 0, so `== 0` bails and exits 1 where the test wants 0.
          #
          #   cli_uninstall with Ok(())
          #                     uninstall_ends_then_deletes_the_task_and_says_what_it_did puts a fake schtasks
          #                     first on PATH and reads the calls it logged. The mutant never runs schtasks, so
          #                     the log is never written: the test fails at its read, and its stdout check
          #                     (the two `ok:` lines) fails before that.
          #
          # An eleventh group: the survivors of the 0.29 head in rust/src/accounts_cli.rs and
          # rust/src/accounts_db.rs (round 2) that need a database, and that a test in rust/tests/
          # kills. Each entry names the function and the replacement, not the line:col, so a sibling
          # mutant of the same function stays judged. Each test named was read in rust/tests/ and
          # asserts the observable the mutant changes. None of them has #[ignore].
          #
          #   Needs a database, which --lib has no pool for (precedent: the groups above). Judged by
          #   rust/tests/, in the plain DATABASE_URL cargo test of the SIL. `cargo mutants -- --lib`
          #   never builds a file under rust/tests/, so here each one would still come back MISSED:
          #   the exclusion stays for --lib only.
          #
          #   accounts_cli.rs run_cli, `==` with `!=`
          #                     v029_accounts_cli::add_creates_the_account_in_lowercase_and_a_repeat_fails_saying_so
          #                     runs the real binary with `account add Ana`, and wants the account
          #                     created. With `!=` every command that is not help prints the usage
          #                     and returns, so nothing is created and the output has no "ana".
          #   accounts_db.rs revoke_device, `==` with `!=`
          #                     v029_accounts_storage::revoking_a_device_matches_its_label_in_any_case_and_only_that_account
          #                     wants revoke_device to return 2 for dan's two pc-lab-1 devices.
          #                     With `!=` it bails there, and the expect fails.
          #   accounts_db.rs ensure_code_capacity, `>=` with `<`
          #                     v029_accounts_storage::at_most_50_codes_are_active_at_once_and_expired_ones_do_not_count
          #                     wants the first of the 50 codes issued on an empty table. With `<`
          #                     the first one is refused as too many active codes.
          #   accounts_db.rs account_facts with Ok(Default::default())
          #                     v029_accounts_over_real_http::g16_account_facts_count_what_the_database_holds_and_doctor_prints_them
          #                     wants tables_present true and the counts of a migrated database.
          #   accounts_db.rs account_facts, `<` with `==`
          #                     v029_accounts_over_real_http::g16_a_database_without_the_account_tables_is_a_doctor_failure_not_a_crash
          #                     wants account_facts to answer on a database without the tables. With
          #                     `==` it queries the missing tables and fails instead. The migrated
          #                     database of g16_account_facts_count_... also answers the default.
          #
          # Owner: endurecedor 0.29. Expires: 2027-03-22.
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
          mutate_and_judge "${files[@]}" "${in_diff[@]}" || fail=1
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
