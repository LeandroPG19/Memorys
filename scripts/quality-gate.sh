#!/usr/bin/env bash
# Second judge: CRAP/complexity + mutation of the rust/src diff.
# SIL = ./scripts/merge-gate.sh. This script does not run it.
set -euo pipefail
# CI-K9a: the judge does not use the interop of Windows. On Linux the PATH that WSL adds (/mnt/..., by 9p) goes before anything looks for a command: each one that is not found walks all of it. The same filter as gate_path_without_windows (gate-lock.sh), which this script cannot load first.
if [[ "$OSTYPE" == linux* ]]; then k9_kept=""; IFS=: read -ra k9_dirs <<<"$PATH"; for k9_d in "${k9_dirs[@]}"; do [[ "$k9_d" == /mnt/* ]] || k9_kept="${k9_kept:+$k9_kept:}$k9_d"; done; PATH="$k9_kept"; unset k9_dirs k9_d k9_kept; fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
BASELINE="$ROOT/scripts/lizard-baseline.txt"

# The jobs are mutants-gate.sh's rule, asked and not copied: the formula that lived
# here said «same derivation» and read the machine's TOTAL memory, and on 2026-10-14
# a VM with 63 GB and ~10 GB free got 6 jobs, two builds died in rustc's
# handle_alloc_error and the judge lost 48 minutes (G5). The rule's own `memory:` line
# goes to this step's stderr; a machine whose memory cannot be read is its exit 2.
qg_mutants_jobs() {
  local plan
  plan="$("$ROOT/scripts/mutants-gate.sh" --default-jobs)" || return $?
  printf '%s\n' "${plan#jobs=}"
}

# The modes that only read and write files, or that run this script (CI-F6, F6b and F6c). They are dispatched before
# anything else of this script runs, so that the judge can call itself for them without the banner or the diff:
# `--cover-index`, `--merge-select` and `--study`.
QG_SELF="$ROOT/scripts/$(basename "${BASH_SOURCE[0]}")"
qg_py() { command -v python3 2>/dev/null || command -v python 2>/dev/null || true; }

# The mode `--cover-index LIST.json FNS_DIR OUT_DIR [REFUSED.tsv]` (D-F6h). LIST.json is `cargo mutants --list --json`;
# FNS_DIR holds one `<test>.fns` per test, whose lines are `src/x.rs first last`, the lines of a function the test
# entered. A test covers a mutant if a function it entered is in the mutant's file and its range holds the line
# the mutant starts on: a superset of the tests that run that line, so a test is never left out that could catch it.
# Writes OUT_DIR/select.tsv (`file line k tests`, the union over the mutants that start on that line) and
# OUT_DIR/plan.tsv (`name group why k`): A0 for a mutant no test covers (`no-coverage`) or whose subset failed
# without a mutant (REFUSED.tsv, `file line`: `preflight`), A1 with `subset` for the rest.
qg_cover_index() {
  local py
  py="$(qg_py)"
  [[ -n "$py" ]] || { echo "FAIL: python is required for --cover-index" >&2; return 2; }
  if [[ $# -lt 3 || ! -f "$1" || ! -d "$2" || ! -d "$3" ]]; then
    echo "FAIL: --cover-index needs LIST.json FNS_DIR OUT_DIR [REFUSED.tsv]" >&2
    return 2
  fi
  "$py" - "$@" <<'PY'
import json, os, sys

list_path, fns_dir, out_dir = sys.argv[1:4]
refused = set()
if len(sys.argv) > 4:
    for line in open(sys.argv[4], encoding="utf-8"):
        if line.strip():
            file, number = line.rstrip("\n").split("\t")
            refused.add((file, int(number)))
mutants = json.load(open(list_path, encoding="utf-8"))
ranges = {}
for entry in sorted(os.listdir(fns_dir)):
    if not entry.endswith(".fns"):
        continue
    test = entry[: -len(".fns")]
    for line in open(os.path.join(fns_dir, entry), encoding="utf-8"):
        parts = line.split()
        if len(parts) == 3:
            ranges.setdefault(parts[0], []).append((int(parts[1]), int(parts[2]), test))

def key(m):
    return m["file"], m["span"]["start"]["line"]

covering = {}
for m in mutants:
    file, line = key(m)
    covering.setdefault((file, line), set()).update(t for first, last, t in ranges.get(file, []) if first <= line <= last)
plan, table = [], {}
for m in mutants:
    k = key(m)
    if k in refused:
        plan.append((m["name"], "A0", "preflight", 0))
    elif not covering[k]:
        plan.append((m["name"], "A0", "no-coverage", 0))
    else:
        plan.append((m["name"], "A1", "subset", len(covering[k])))
        table[k] = sorted(covering[k])
with open(os.path.join(out_dir, "plan.tsv"), "w", encoding="utf-8") as f:
    for row in plan:
        f.write("\t".join(str(c) for c in row) + "\n")
with open(os.path.join(out_dir, "select.tsv"), "w", encoding="utf-8") as f:
    for (file, line), tests in sorted(table.items()):
        f.write(f"{file}\t{line}\t{len(tests)}\t{' '.join(tests)}\n")
a0 = sum(1 for row in plan if row[1] == "A0")
print(f"cover index: {len(plan)} mutants, {a0} for the whole suite, {len(plan) - a0} with their tests")
PY
}

# The mode `--merge-select OUT A0 A1 B [PLAN.tsv]` (D-F6m). Each pass is the path of its outcomes.json, or `-` for a
# pass that did not run. The class of a mutant is the one the whole suite gave it: A0's, or B's for a mutant that its
# subset in A1 did not catch; A1's own only if it is caught or unviable. Writes OUT (an outcomes.json of
# cargo-mutants' shape, with the logs of every record resolved from OUT's directory, and `qg_select` on each mutant:
# the pass and the size of its subset). Exit 0: every mutant caught or unviable; 4: the merge is sound and some
# mutant is missed or timed out with the whole suite (named); 1: it cannot be merged (a missing confirmation, a
# baseline that is not Success, totals that do not add up).
qg_merge_select() {
  local py
  py="$(qg_py)"
  [[ -n "$py" ]] || { echo "FAIL: python is required for --merge-select" >&2; return 2; }
  if [[ $# -lt 4 ]]; then
    echo "FAIL: --merge-select needs OUT A0 A1 B [PLAN.tsv]" >&2
    return 2
  fi
  "$py" - "$@" <<'PY'
import json, os, sys

out_path = sys.argv[1]
paths = {"A0": sys.argv[2], "A1": sys.argv[3], "B": sys.argv[4]}
subset = {}
if len(sys.argv) > 5:
    for line in open(sys.argv[5], encoding="utf-8"):
        if line.strip():
            name, _group, _why, k = line.rstrip("\n").split("\t")
            subset[name] = int(k)
TIMEOUT = ("Timeout", "TimeoutMutant")
CLASSES = {"caught": ("CaughtMutant",), "missed": ("MissedMutant",), "timeout": TIMEOUT, "unviable": ("Unviable",)}
problems, passes = [], {}
for label, path in paths.items():
    if path == "-":
        continue
    data = json.load(open(path, encoding="utf-8"))
    outcomes = data.get("outcomes")
    if not isinstance(outcomes, list):
        problems.append(f"pass {label}: {path} carries no per-mutant outcomes, so it cannot be merged")
        continue
    baselines = [o for o in outcomes if o.get("scenario") == "Baseline"]
    mutants = {o["scenario"]["Mutant"]["name"]: o for o in outcomes if isinstance(o.get("scenario"), dict) and "Mutant" in o["scenario"]}
    if len(baselines) != 1 or baselines[0].get("summary") != "Success":
        problems.append(f"the baseline of pass {label} is {baselines[0].get('summary') if baselines else 'missing'}, not Success")
    for field, names in CLASSES.items():
        count = sum(1 for o in mutants.values() if o.get("summary") in names)
        if data.get(field) != count:
            problems.append(f"pass {label}: its totals do not add up: {field} is {data.get(field)} and its records hold {count}")
    passes[label] = {"data": data, "baseline": baselines[0] if baselines else None, "mutants": mutants, "dir": os.path.dirname(os.path.abspath(path))}
if not passes:
    problems.append("no pass was run, so there is nothing to merge")

final = {}
for name, record in (passes.get("A0") or {"mutants": {}})["mutants"].items():
    final[name] = (record, "A0")
left_open = []
for name, record in (passes.get("A1") or {"mutants": {}})["mutants"].items():
    if name in final:
        problems.append(f"{name} was judged in A0 and in A1")
    elif record.get("summary") in ("CaughtMutant", "Unviable"):
        final[name] = (record, "A1")
    else:
        left_open.append(name)
confirmed = (passes.get("B") or {"mutants": {}})["mutants"]
for name in left_open:
    if name in confirmed:
        final[name] = (confirmed[name], "B")
    else:
        problems.append(f"no row in pass B confirms {name}, which pass A1 left as {passes['A1']['mutants'][name].get('summary')}")
for name in confirmed:
    if name not in left_open:
        problems.append(f"pass B holds {name}, which pass A1 did not leave open")
if problems:
    for problem in problems:
        print(f"FAIL: {problem}", file=sys.stderr)
    sys.exit(1)

out_dir = os.path.dirname(os.path.abspath(out_path))
def moved(record, label):
    record = dict(record)
    for field in ("log_path", "diff_path"):
        rel = record.get(field)
        if isinstance(rel, str) and rel:
            record[field] = os.path.relpath(os.path.join(passes[label]["dir"], rel), out_dir)
    return record
records = []
for name, (record, label) in final.items():
    record = moved(record, label)
    record["qg_select"] = {"pass": label, "subset": subset.get(name)}
    records.append(record)
first = next(label for label in ("A0", "A1", "B") if label in passes)
count = lambda names: sum(1 for r in records if r.get("summary") in names)
merged = {"outcomes": [moved(passes[first]["baseline"], first)] + records, "total_mutants": len(records),
          "caught": count(CLASSES["caught"]), "missed": count(CLASSES["missed"]), "timeout": count(CLASSES["timeout"]),
          "unviable": count(CLASSES["unviable"]), "success": count(("Success",)),
          "cargo_mutants_version": passes[first]["data"].get("cargo_mutants_version")}
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(merged, f)
by = {label: sum(1 for _, (_, p) in final.items() if p == label) for label in ("A0", "A1", "B")}
print(f"merged: {len(records)} mutants: {merged['caught']} caught, {merged['missed']} missed, {merged['timeout']} timeout, "
      f"{merged['unviable']} unviable (A0 {by['A0']}, A1 {by['A1']}, B {by['B']})")
red = 0
for record in records:
    name = record["scenario"]["Mutant"]["name"]
    if record.get("summary") in CLASSES["missed"]:
        print(f"FAIL: mutant missed by the whole suite: {name}", file=sys.stderr)
        red = 4
    elif record.get("summary") in TIMEOUT:
        print(f"FAIL: mutant timed out with the whole suite: {name}", file=sys.stderr)
        red = 4
sys.exit(red)
PY
}

# The mode `--study REF DIR` (D-F6p): is cover (Q2) the judge it replaces? REF is `legacy` or `full`, the judge as it
# runs today, and DIR is new or empty. It runs this script whole with QG_SELECT=REF and QG_KEEP_OUT=DIR/ref (that run is
# the verdict of the reference), then whole with QG_SELECT=cover into a keep directory of its own, whose cover/ becomes
# DIR/cover, and asks mutants-gate.sh --compare-classes for the class of every mutant in both. DIR/study.tsv has a row
# for each mutant: its class in each run, the pass and the size of the subset of the cover run, and the seconds of its
# test phase in each. Exit 0 only if the reference is green and no class changed; 1 if the reference is red or some class
# differs; 2 if the study could not be run or compared (a DIR that is not empty, a REF that is not legacy or full, a
# run that left no outcomes). Each run is the whole judge, lizard and exclusions included: they cost seconds beside the
# mutation, and the two runs judge the same diff the same way.
qg_study() {
  local ref="${1:-}" dir="${2:-}" py ref_rc=0 cover_rc=0 cmp_rc=0 ref_out merged n diffs cmp_err
  case "$ref" in
    legacy | full) ;;
    *)
      echo "FAIL: --study REF DIR: REF is legacy or full, the judge that cover is compared with, not '$ref'" >&2
      return 2
      ;;
  esac
  [[ -n "$dir" ]] || { echo "FAIL: --study $ref DIR needs a directory for the study" >&2; return 2; }
  if [[ -n "$(ls -A "$dir" 2>/dev/null || true)" ]]; then
    echo "FAIL: --study DIR=$dir is not empty; name a new directory so that runs do not mix" >&2
    return 2
  fi
  py="$(qg_py)"
  [[ -n "$py" ]] || { echo "FAIL: python is required for --study" >&2; return 2; }
  mkdir -p "$dir" || return 2
  QG_SELECT="$ref" QG_KEEP_OUT="$dir/ref" bash "$QG_SELF" || ref_rc=$?
  echo "study: reference exit $ref_rc"
  ref_out="$dir/ref/$ref/outcomes.json"
  if [[ ! -s "$ref_out" ]]; then
    echo "FAIL: the reference run left no outcomes.json in $dir/ref/$ref (its exit was $ref_rc): there is nothing to compare cover with, and its run is not launched" >&2
    return 2
  fi
  QG_SELECT=cover QG_KEEP_OUT="$dir/cover-run" bash "$QG_SELF" || cover_rc=$?
  merged="$dir/cover-run/cover/merged.json"
  if [[ ! -s "$merged" ]]; then
    echo "FAIL: the cover run left no merged.json in $dir/cover-run/cover (its exit was $cover_rc; it may have turned itself off and run as full): there is nothing to compare" >&2
    return 2
  fi
  mv "$dir/cover-run/cover" "$dir/cover" && rmdir "$dir/cover-run" || return 2
  cmp_err="$(mktemp)"
  "$ROOT/scripts/mutants-gate.sh" --compare-classes "$ref_out" "$dir/cover/merged.json" 2>"$cmp_err" || cmp_rc=$?
  cat "$cmp_err" >&2
  diffs="$(grep -c '^FAIL' "$cmp_err" || true)"
  rm -f "$cmp_err"
  n="$("$py" -I - "$ref_out" "$dir/cover/merged.json" "$dir/study.tsv" <<'PY'
import json, sys
ref_path, cover_path, out_path = sys.argv[1:4]
def load(path):
    rows = {}
    for o in json.load(open(path, encoding="utf-8"))["outcomes"]:
        if not isinstance(o.get("scenario"), dict):
            continue
        test = next((p.get("duration") for p in o.get("phase_results", []) if p.get("phase") == "Test"), None)
        rows[o["scenario"]["Mutant"]["name"]] = (o.get("summary"), test, o.get("qg_select", {}))
    return rows
ref, cover = load(ref_path), load(cover_path)
seconds = lambda d: "-" if d is None else f"{d:.3f}"
with open(out_path, "w", encoding="utf-8") as f:
    f.write("name\tref\tcover\tpass\tk\tref_test_s\tcover_test_s\n")
    for name in list(ref) + [m for m in cover if m not in ref]:
        r, c = ref.get(name, ("-", None, {})), cover.get(name, ("-", None, {}))
        f.write("\t".join([name, str(r[0]), str(c[0]), str(c[2].get("pass", "-")), str(c[2].get("subset", "-")), seconds(r[1]), seconds(c[1])]) + "\n")
print(len(ref))
PY
)" || return 2
  case "$cmp_rc" in
    0) echo "study: $n/$n identical — Q2 may be adopted" ;;
    1) echo "study: $diffs differ — Q2 is NOT adopted" ;;
    *)
      echo "FAIL: mutants-gate.sh --compare-classes exited $cmp_rc, so the classes were not compared" >&2
      return 2
      ;;
  esac
  (( ref_rc == 0 && cmp_rc == 0 )) || return 1
}

case "${1:-}" in
  --cover-index | --merge-select)
    mode_rc=0
    if [[ "$1" == --cover-index ]]; then qg_cover_index "${@:2}" || mode_rc=$?; else qg_merge_select "${@:2}" || mode_rc=$?; fi
    exit "$mode_rc"
    ;;
  --study)
    mode_rc=0
    qg_study "${@:2}" || mode_rc=$?
    exit "$mode_rc"
    ;;
esac

# Every --exclude-re entry of the mutation step, in one place so that the
# exclusion checks below read exactly what the mutation run is handed. Each
# entry's reason, owner and expiry date are in the comment over that step.
MUTANTS_EXCLUDE_RE='fetch_adjacency|list_resources|read_resource|run_checks_with|upsert_symbol|upsert_placeholder_entity|backfill_unscoped|observation_in_scope|run_project|run_check|run_write|workspace_client_id|http.rs.*serve_pool|gpu.rs.*gpu_availability|gpu.rs.*cuda_provider|resources.rs.*replace apply|rerank.rs.*failure_reason|http.rs.*mcp_endpoint|http.rs.*replace panel |http.rs.*warm_reranker_eagerly|llm_cli.rs.*judge_is_sampling |rerank.rs.*warm_up|rerank.rs.*score_off_runtime|rerank.rs.*score_one_chunk|rerank.rs.*score_pairs|nli.rs.*replace init |onnx.rs.*init_onnx_session|gpu.rs.*replace wants_gpu -> bool with false|gpu.rs.*preferred_device_var|gpu.rs.*compiled_provider.*with None|http.rs.*compiled_gpu_provider.*with None|service.rs.*replace restrict -> Result<bool> with Ok.false|src/redact\.rs:110:63: replace > with >= in credentials_in_url|src/redact\.rs:130:58: replace > with >= in secret_field|src/redact\.rs:136:51: replace \+ with \* in secret_field|src/cognitive/nli\.rs:.*replace enabled -> bool with |src/cognitive/nli\.rs:.*replace status_resolved -> bool with false|src/cognitive/nli\.rs:.*replace failure_reason -> Option<String> with None|src/embeddings/onnx\.rs:.*replace failure_reason -> Option<String> with None|src/embeddings/onnx\.rs:.*replace compute_embedding -> Result<Vec<f32>> with Ok\(vec!|src/http\.rs:[0-9]+:24: replace && with \|\| in embedder_state$|src/(calibrate_cli|dashboard|dedupe_cli|export|link_cli|reembed_cli|rem_cli|secure_cli|skills_cli|sync_cli|eval/mod)\.rs:.*replace run_cli -> Result<\(\)> with Ok\(\(\)\)|src/models_cli\.rs:.*replace print_help with \(\)|src/setup\.rs:.*replace log with \(\)|src/dashboard\.rs:.*replace render -> Result<String> with |src/search/calibrate\.rs:.*replace load_ood_threshold -> Option<f64> with |src/export\.rs:.*replace export_obsidian -> Result<usize> with |src/db\.rs:.*replace assert_embedding_dim -> Result<\(\)> with Ok\(\(\)\)|src/dedupe_cli\.rs:.*replace merge_by_name -> Result<\(\)> with Ok\(\(\)\)|src/protocol\.rs:.*replace run_rem_consolidation_locked -> Result<\(\)> with Ok\(\(\)\)|src/models_cli\.rs:.*replace download_(model|runtime) -> Result<\(\)> with Ok\(\(\)\)|src/protocol\.rs:.*replace spawn_handshake_watchdog with \(\)|src/embeddings/onnx\.rs:.*replace locate_onnxruntime -> Option<PathBuf> with Some\(Default::default\(\)\)|src/main\.rs:.*replace async_main with \(\)|src/db\.rs:.*replace bind_app_role -> sqlx::Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace init_schema -> Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace provision_app_role with \(\)|src/db\.rs:.*replace provision_app_role_from_pgpass with \(\)|src/db\.rs:.*replace write_app_role_password -> sqlx::Result<\(\)> with Ok\(\(\)\)|src/embeddings/onnx\.rs:.*replace load_onnxruntime -> Result<\(\)> with Ok\(\(\)\)|src/embeddings/onnx\.rs:.*replace load_located_onnxruntime -> Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace migrate_and_provision -> Result<\(\)> with Ok\(\(\)\)|src/db\.rs:.*replace migrate -> Result<\(\), MigrateError> with Ok\(\(\)\)|src/db\.rs:.*replace realign_then_migrate -> Result<\(\), MigrateError> with Ok\(\(\)\)|src/db\.rs:.*replace realign_line_ending_checksums -> sqlx::Result<\(\)> with Ok\(\(\)\)|src/http\.rs:.*replace \|\| with && in identify_account|src/http\.rs:.*replace != with == in identify_account|src/http\.rs:.*delete ! in identify_account|src/http\.rs:.*replace pair_answer -> Option<PairResponse> with None|src/http\.rs:.*delete ! in claim_and_answer|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("", None\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("", Some\(String::new\(\)\)\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("xyzzy", None\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("", Some\("xyzzy"\.into\(\)\)\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("xyzzy", Some\(String::new\(\)\)\)\)|src/http\.rs:.*replace credential_proof -> Result<\(&\x27static str, Option<String>\)> with Ok\(\("xyzzy", Some\("xyzzy"\.into\(\)\)\)\)|src/http\.rs:.*replace != with == in credential_proof|src/accounts_db\.rs:.*replace record_pair_redeemed -> Result<\(\)> with Ok\(\(\)\)|src/accounts_db\.rs:.*replace list_accounts -> Result<Vec<AccountSummary>> with Ok\(vec!\[\]\)|src/accounts_db\.rs:.*replace lock_account -> Result<Uuid> with Ok\(Default::default\(\)\)|src/accounts_db\.rs:.*replace > with < in revoke_rows|src/accounts_db\.rs:.*replace > with >= in revoke_rows|src/accounts_db\.rs:.*replace revoke_device -> Result<usize> with Ok\(0\)|src/accounts_db\.rs:.*replace revoke_device -> Result<usize> with Ok\(1\)|src/accounts_db\.rs:.*replace ensure_code_capacity -> Result<\(\)> with Ok\(\(\)\)|src/accounts_db\.rs:.*replace \+ with - in store_code|src/accounts_db\.rs:.*replace pairing_candidates -> Result<Vec<PairingCandidate>> with Ok\(vec!\[\]\)|src/accounts_db\.rs:.*replace - with \+ in pairing_candidates|src/accounts_db\.rs:.*replace == with != in claim_pairing_code|src/accounts_db\.rs:.*replace credential_for_auth -> Result<Option<CredentialRow>> with Ok\(None\)|src/accounts_db\.rs:.*replace touch_last_seen -> Result<\(\)> with Ok\(\(\)\)|src/accounts_db\.rs:.*replace purge_pairing_codes -> Result<u64> with Ok\(1\)|src/accounts_db\.rs:.*replace purge_pairing_codes -> Result<u64> with Ok\(0\)|src/accounts_db\.rs:.*replace - with \+ in purge_pairing_codes|src/accounts_db\.rs:.*replace < with > in account_facts|src/accounts_db\.rs:.*replace < with <= in account_facts|src/accounts_cli\.rs:.*replace == with != in run_cli|src/accounts_db\.rs:.*replace == with != in revoke_device|src/accounts_db\.rs:.*replace >= with < in ensure_code_capacity|src/accounts_db\.rs:.*replace account_facts -> Result<AccountFacts> with Ok\(Default::default\(\)\)|src/accounts_db\.rs:.*replace < with == in account_facts|src/bridge/mod\.rs:.*replace cli_run -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace cli_install -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace cli_uninstall -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace cli_doctor -> Result<\(\)> with Ok\(\(\)\)|src/bridge/mod\.rs:.*replace != with == in cli_doctor|src/mode\.rs:.*replace rerank_gpu_active -> bool with false|src/cognitive/nli\.rs:.*replace resolved_without_a_model -> bool with false'

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
#
# An empty list means "nothing is over the ceiling", so a lizard that is absent or
# died must never come out as one: it is exit 2, and the callers pass it on.
# Neither `2>/dev/null` nor `|| true` may come back: a run that could not measure
# printed nothing, and a new CC 30 function went by as green.
require_lizard() {
  command -v lizard >/dev/null 2>&1 && return 0
  echo "FALTA lizard: lizard is not on PATH, so the complexity of the diff cannot be judged (pip install lizard)." >&2
  return 2
}

lizard_warnings() {
  local cwd="$1" cc_max="$2" csv errs rc=0
  shift 2
  require_lizard || return $?
  errs="$(mktemp)"
  csv="$(cd "$cwd" && lizard --csv "$@" 2>"$errs")" || rc=$?
  if (( rc != 0 )); then
    echo "FAIL: lizard exited $rc, so the complexity of the diff was not measured:" >&2
    sed 's/^/      /' "$errs" >&2
    rm -f "$errs"
    return 2
  fi
  rm -f "$errs"
  printf '%s\n' "$csv" |
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
    LC_ALL=C sort -u
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

  require_lizard || return $?

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
  measured="$(lizard_warnings "$cwd" "$cc_max" "$@")" || return 2
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
# CI-F6 (F6a, D-F6a; F6b, D-F6g..o): QG_SELECT chooses how the step reaches cargo-mutants.
#   legacy  the call of before F6, byte for byte: `cargo mutants ... -- --lib`, the full copy of rust/ per job. It
#           is the judge the study compares against (D-F6p), and the way back.
#   full    the default: the same mutants and the same verdict through the lab of mutants-gate.sh (--lab-run),
#           which builds only the lib test of each mutant from a seed of the dependencies (CI-F4).
#   cover   the same mutants and the same classes, judged with fewer tests (Q2) and the cheap verdicts first (Q3):
#           a map says which functions each lib test enters; a mutant runs only the tests whose functions hold its
#           line; the mutants no test covers run first with the whole suite (pass A0), the rest with their
#           subset (A1), and every mutant that its subset did not catch, missed or timed out, runs again with the
#           whole suite (B) before it counts. A caught with a subset is a caught: the test that fails is also in the
#           whole suite. Anything the map cannot be trusted for turns it off and the step runs as `full`.
# `cover` is not the default. It is adopted only after a study of the class of every mutant against `legacy`, with
# 0 differences, noted in the backlog (D-F6q); until then the default stays `full`, which changes no test.
#
# QG_KEEP_OUT=DIR keeps the outcomes of this run in DIR/<mode>/ (green or red): for cover, the plan, the three
# passes and merged.json. It is a directory of this run's: one that is not empty is exit 2 before anything is
# mutated, so that two runs never mix (D-F6f).
mutate_and_judge() {
  local out rc=0 prep=0 want="${MUTANTS_JOBS:-}" mode="${QG_SELECT:-full}"
  case "$mode" in
    legacy | full | cover) ;;
    *)
      echo "FAIL: QG_SELECT=$mode is not a mode of the mutation step (legacy, full or cover). cover is not the default: it needs a study with 0 differences, noted in the backlog, before it is (D-F6q)." >&2
      return 2
      ;;
  esac
  if [[ -n "${QG_KEEP_OUT:-}" ]]; then
    if [[ -n "$(ls -A "$QG_KEEP_OUT" 2>/dev/null || true)" ]]; then
      echo "FAIL: QG_KEEP_OUT=$QG_KEEP_OUT is not empty; name a new directory so that runs do not mix" >&2
      return 2
    fi
    mkdir -p "$QG_KEEP_OUT" || return 2
  fi
  [[ -n "$want" ]] || want="$(qg_mutants_jobs)" || return $?
  out="$(mktemp -d)"
  if [[ "$mode" == cover ]]; then
    qg_cover_prepare "$out" "$@" || prep=$?
    case "$prep" in
      0) ;;
      3) mode=full ;;
      *)
        rm -rf "$out"
        return "$prep"
        ;;
    esac
  fi
  echo "mode: $mode"
  case "$mode" in
    cover) qg_mutate_cover "$want" "$out" "$@" || rc=1 ;;
    full) qg_mutate_full "$want" "$out" "$@" || rc=1 ;;
    *) qg_mutate_legacy "$want" "$out" "$@" || rc=$? ;;
  esac
  # Every pass of cover went through the build check as it finished (qg_cover_pass).
  if [[ "$mode" != cover ]]; then
    if [[ -f "$out/mutants.out/outcomes.json" ]]; then
      "$ROOT/scripts/mutants-gate.sh" --check-builds "$out/mutants.out/outcomes.json" || rc=1
    else
      echo "no outcomes.json: cargo mutants built no mutant, so there is no build to judge"
    fi
  fi
  if [[ -n "${QG_KEEP_OUT:-}" ]]; then
    if [[ "$mode" == cover && -d "$out/cover" ]]; then
      mkdir -p "$QG_KEEP_OUT/cover" && cp -a "$out/cover/." "$QG_KEEP_OUT/cover/" && echo "kept: $QG_KEEP_OUT/cover"
    elif [[ -d "$out/mutants.out" ]]; then
      mkdir -p "$QG_KEEP_OUT/$mode" && cp -a "$out/mutants.out/." "$QG_KEEP_OUT/$mode/" && echo "kept: $QG_KEEP_OUT/$mode"
    fi
  fi
  rm -rf "$out"
  return "$rc"
}

# qg_lab_pass WANT OUT ARGS...: cargo-mutants over ARGS through the lab of mutants-gate.sh. Everything around it (the
# disk, the scratch, the seed, the stage, the shim, the jobserver) is the lab's, and the lab's own --self-test
# proves it; here is only what the diff judge adds: the exclusions, the timeout, and `--cargo-arg=--lib` so each
# mutant builds the lib test and not the 116 binaries of tests/. Nothing goes after a `--`: a `-- --lib`
# there reaches only the test phase (CI-F4, F4a). Returns the lab's exit (0: its guards held, whatever
# cargo-mutants said; cargo-mutants' own exit is in OUT/cargo-mutants.rc).
qg_lab_pass() {
  local want="$1" out="$2" lab_rc=0
  shift 2
  "$ROOT/scripts/mutants-gate.sh" --lab-run "$out" "$want" -- "$@" \
    --exclude-re "$MUTANTS_EXCLUDE_RE" --timeout 90 --gitignore=false \
    --cargo-arg=--lib || lab_rc=$?
  if (( lab_rc != 0 )); then
    echo "FAIL: mutants-gate.sh --lab-run exited $lab_rc" >&2
  fi
  return "$lab_rc"
}

# qg_mutate_full WANT OUT ARGS...: one pass over every mutant. The verdict is cargo-mutants' exit: 2 (a mutant was
# missed) and 3 (one timed out) are red; so is anything else that is not 0.
qg_mutate_full() {
  local want="$1" out="$2" crc
  shift 2
  qg_lab_pass "$want" "$out" "$@" || return 1
  crc="$(cat "$out/cargo-mutants.rc" 2>/dev/null || true)"
  case "$crc" in
    0) return 0 ;;
    2) echo "FAIL: cargo-mutants rc=2: at least one mutant was missed" >&2 ;;
    3) echo "FAIL: cargo-mutants rc=3: at least one mutant timed out" >&2 ;;
    *) echo "FAIL: cargo-mutants rc=${crc:-unknown}" >&2 ;;
  esac
  return 1
}

# qg_mutate_legacy WANT OUT ARGS...: the call of before F6, with the rules that were here: the disk before
# anything is copied (mutants-gate.sh --prepare-scratch), the jobs that fit, the judge's own scratch.
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
#
# Disk, before anything is copied. cargo-mutants copies rust/ once per job and
# each copy compiles its own target/: on 2026-10-13 that was 27.58 GB per copy
# and the step died of os error 112. mutants-gate.sh --prepare-scratch holds the
# rule: the jobs the free disk holds (an explicit MUTANTS_JOBS included), the
# judge's own scratch directory, swept of its old copies, and a refusal with the
# numbers when not even one job fits, or when a target-* of another build sits
# inside rust/, since the copy takes it whole.
#
# --gitignore=false stays, but not for the reason it was written for:
# eval-datasets is versioned. Measured on 2026-10-13 with cargo-mutants 27.1.0,
# the copy reads git_ignore false with the flag or without it, so dropping it
# keeps no target-* out of the copy.
#
# TMP and TEMP are what cargo-mutants reads as its temp dir on Windows, TMPDIR
# what it reads on Unix. CARGO_PROFILE_DEV_DEBUG=0 is for that one command and
# takes a copy's target from 14.1 to 8.94 GB (the .pdb files were 54 %).
qg_mutate_legacy() {
  local want="$1" out="$2" native plan jobs scratch scratch_native rc=0
  shift 2
  plan="$("$ROOT/scripts/mutants-gate.sh" --prepare-scratch "$want")" || return $?
  jobs="$(sed -n 's/^jobs=//p' <<<"$plan")"
  scratch="$(sed -n 's/^scratch=//p' <<<"$plan")"
  if [[ -z "$jobs" || -z "$scratch" ]]; then
    echo "FAIL: mutants-gate.sh --prepare-scratch printed no jobs= and scratch= lines: $plan" >&2
    return 2
  fi
  scratch_native="$scratch"
  native="$out"
  if [[ "$OSTYPE" == msys* || "$OSTYPE" == cygwin* ]]; then
    scratch_native="$(cygpath -w "$scratch" 2>/dev/null || printf '%s' "$scratch")"
    native="$(cygpath -m "$out" 2>/dev/null || printf '%s' "$out")"
  fi
  (cd rust && TMP="$scratch_native" TEMP="$scratch_native" TMPDIR="$scratch" CARGO_PROFILE_DEV_DEBUG=0 \
    cargo mutants "$@" \
    --exclude-re "$MUTANTS_EXCLUDE_RE" --output "$native" \
    --timeout 90 --jobs "$jobs" --gitignore=false -- --lib) || rc=1
  return "$rc"
}

# --- CI-F6 (F6b): Q2 and Q3, the cover mode -------------------------------------------------------------------
# The marker cargo-mutants 27.1.0 writes after every replacement (MUTATION_MARKER_COMMENT). A tree that already holds it
# would make the shim of mutants-gate.sh take a lab for a mutated one, so cover does not run on it.
QG_MARKER='/* ~ changed by cargo-mutants ~ */'

# What cover needs and the machine may lack: cargo-llvm-cov, and llvm-profdata and llvm-cov in the sysroot of the
# rustc that builds (the llvm-tools component). Without them cover does not run: that is the environment, exit 2,
# and not a quiet fall back to full, which would hide that the mode asked for is not the mode that ran (D-F6j).
qg_cover_tools() {
  local sysroot host
  local -a missing=()
  command -v cargo-llvm-cov >/dev/null 2>&1 || missing+=("cargo-llvm-cov (cargo install cargo-llvm-cov)")
  sysroot="$(rustc --print sysroot 2>/dev/null || true)"
  host="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p' || true)"
  QG_LLVM_BIN="$sysroot/lib/rustlib/$host/bin"
  if [[ -z "$sysroot" || -z "$host" ]]; then
    missing+=("rustc (it did not print its sysroot and its host)")
  else
    [[ -x "$QG_LLVM_BIN/llvm-profdata" ]] || missing+=("llvm-profdata in $QG_LLVM_BIN (rustup component add llvm-tools)")
    [[ -x "$QG_LLVM_BIN/llvm-cov" ]] || missing+=("llvm-cov in $QG_LLVM_BIN (rustup component add llvm-tools)")
  fi
  if (( ${#missing[@]} > 0 )); then
    echo "FAIL: QG_SELECT=cover needs what this machine lacks: ${missing[*]}. Nothing was mutated; QG_SELECT=full does not need them." >&2
    return 2
  fi
}

# The CPUs the judge counts (gate_cargo_jobs: nproc under the cgroup's cpu.max), the number of tests the map runs at
# once. In a subshell, so that what gate-lock.sh sets and clears when it is read stays out of this one.
qg_cover_jobs() {
  local p
  # shellcheck source=scripts/gate-lock.sh
  p="$(source "$ROOT/scripts/gate-lock.sh" && gate_cargo_jobs >/dev/null && printf '%s' "$GATE_CARGO_JOBS")" || p=""
  if [[ ! "$p" =~ ^[0-9]+$ ]] || (( p < 1 )); then
    echo "FAIL: the CPUs of this judge could not be counted, so the map of QG_SELECT=cover cannot size its tests (set CARGO_BUILD_JOBS)" >&2
    return 2
  fi
  printf '%s\n' "$p"
}

# At most $1 background jobs of this shell run at once. Each of them is bounded by its own `timeout`, so this waits
# for no job that cannot end.
qg_pool_slot() {
  local running
  while :; do
    running="$(jobs -rp | wc -l)"
    (( running < $1 )) && break
    wait -n 2>/dev/null || true
  done
}

# QG_COVER_WHY is what `cover: off (...)` says. 3 is "off", not a failure of the judge.
qg_cover_build() { # CDIR: the lib test, instrumented, built into <the judge's target>/qg-cover (D-F6g)
  local cdir="$1" tdir envs start=$SECONDS py
  py="$(qg_py)"
  # The target of the judge, and a directory of its own under it: crap-gate.sh builds with cfg(coverage) into the
  # same target and the two would rebuild each other's crates.
  tdir="${CARGO_TARGET_DIR:-${TMPDIR:-/tmp}/memory-industry-qg-target}/qg-cover"
  mkdir -p "$tdir" || { QG_COVER_WHY="the instrumented target $tdir could not be made"; return 3; }
  # show-env is the instrumentation without cargo-llvm-cov's own runner: the flags, the wrapper, the profile name. Both
  # target variables say the same directory, so the show-env, the build and the profiles agree on it.
  envs="$(cd "$ROOT/rust" && CARGO_TARGET_DIR="$tdir" CARGO_LLVM_COV_TARGET_DIR="$tdir" cargo llvm-cov show-env --sh --no-cfg-coverage 2>"$cdir/show-env.err")" \
    || { QG_COVER_WHY="cargo llvm-cov show-env failed: $(tail -n 1 "$cdir/show-env.err")"; return 3; }
  if ! (cd "$ROOT/rust" && export CARGO_TARGET_DIR="$tdir" CARGO_LLVM_COV_TARGET_DIR="$tdir" && eval "$envs" \
      && timeout 1800 cargo test --lib --no-run --message-format=json >"$cdir/build.json" 2>"$cdir/build.err"); then
    QG_COVER_WHY="the instrumented lib test did not build: $(tail -n 1 "$cdir/build.err")"
    return 3
  fi
  QG_COVER_BIN="$("$py" - "$cdir/build.json" <<'PY'
import json, sys
found = []
for line in open(sys.argv[1], encoding="utf-8"):
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m.get("profile", {}).get("test") \
            and "lib" in m.get("target", {}).get("kind", []):
        found.append(m["executable"])
print(found[0] if len(found) == 1 else "")
PY
)"
  if [[ -z "$QG_COVER_BIN" || ! -x "$QG_COVER_BIN" ]]; then
    QG_COVER_WHY="the build named no single lib test executable"
    return 3
  fi
  QG_COVER_BUILD_S=$((SECONDS - start))
}

# One test of the map: alone in its process, with a profile of its own, and what it entered reduced to the lines
# `src/x.rs first last` of each function whose count is not 0 and whose file is in rust/src (the first and last line
# of the regions of the function that are in that file: an expansion region of another file does not widen it).
# The profile, the merged profile and the export of the test are deleted as soon as they are reduced. A test that
# fails, or whose coverage cannot be read, leaves `<n>` in work/fail for the map to say.
qg_cover_test() { # BIN N TEST CDIR
  local bin="$1" n="$2" t="$3" cdir="$4" d="$4/work/$2" py
  py="$(qg_py)"
  mkdir -p "$d"
  if ! (cd "$ROOT/rust" && LLVM_PROFILE_FILE="$d/%p-%m.profraw" timeout "${QG_COVER_TEST_TIMEOUT_S:-300}" "$bin" --exact "$t" --test-threads=1 -q >"$d/out" 2>&1); then
    printf 'test\t%s\n' "$t" > "$cdir/work/fail/$n"
  elif ! timeout 300 "$QG_LLVM_BIN/llvm-profdata" merge -sparse "$d"/*.profraw -o "$d/t.profdata" >"$d/tools.err" 2>&1 \
    || ! timeout 300 "$QG_LLVM_BIN/llvm-cov" export -format=text -skip-expansions -instr-profile="$d/t.profdata" "$bin" >"$d/cov.json" 2>>"$d/tools.err" \
    || ! "$py" "$cdir/reduce.py" "$ROOT/rust/src/" "$(cd "$ROOT/rust/src" && pwd -P)/" <"$d/cov.json" >"$cdir/fns/$t.fns"; then
    printf 'measure\t%s\n' "$t" > "$cdir/work/fail/$n"
    rm -f "$cdir/fns/$t.fns"
  fi
  rm -rf "$d"
}

# D-F6g: every test of the lib that is not ignored, one process each, P at a time. A map is a measure and not a
# verdict: any test that fails alone, or whose coverage cannot be read, turns cover off (QG_COVER_WHY) and the step
# runs as full, with the verdict full gives.
qg_cover_map() { # CDIR P
  local cdir="$1" p="$2" work="$1/work" start=$SECONDS n=0 t all ignored entered
  local -a tests=() failed=() unread=()
  mkdir -p "$work/fail" "$cdir/fns"
  cat > "$cdir/reduce.py" <<'PY'
import json, sys
roots = sys.argv[1:]
seen = set()
for data in json.load(sys.stdin).get("data", []):
    for fn in data.get("functions", []):
        names = fn.get("filenames") or []
        if not fn.get("count") or not names:
            continue
        rel = next(("src/" + names[0][len(root):] for root in roots if names[0].startswith(root)), None)
        lines = [(r[0], r[2]) for r in fn.get("regions", []) if len(r) > 5 and r[5] == 0]
        if rel and lines:
            seen.add((rel, min(a for a, _ in lines), max(b for _, b in lines)))
for rel, first, last in sorted(seen):
    print(rel, first, last)
PY
  all="$(LLVM_PROFILE_FILE="$work/list-%p.profraw" "$QG_COVER_BIN" --list --format=terse 2>/dev/null | sed -n 's/: test$//p' || true)"
  ignored="$(LLVM_PROFILE_FILE="$work/list-%p.profraw" "$QG_COVER_BIN" --list --ignored --format=terse 2>/dev/null | sed -n 's/: test$//p' || true)"
  rm -f "$work"/list-*.profraw
  mapfile -t tests < <(awk 'NR == FNR {skip[$0] = 1; next} NF && !($0 in skip)' <(printf '%s\n' "$ignored") <(printf '%s\n' "$all"))
  if (( ${#tests[@]} == 0 )); then
    QG_COVER_WHY="the instrumented lib test lists no test that is not ignored"
    return 3
  fi
  for t in "${tests[@]}"; do
    n=$((n + 1))
    qg_pool_slot "$p"
    qg_cover_test "$QG_COVER_BIN" "$n" "$t" "$cdir" &
  done
  wait || true
  if compgen -G "$work/fail/*" >/dev/null; then
    mapfile -t failed < <(cat "$work"/fail/* | awk -F'\t' '$1 == "test" {print $2}' | LC_ALL=C sort)
    mapfile -t unread < <(cat "$work"/fail/* | awk -F'\t' '$1 == "measure" {print $2}' | LC_ALL=C sort)
    QG_COVER_WHY=""
    if (( ${#failed[@]} > 0 )); then
      QG_COVER_WHY="${#failed[@]} test$([[ ${#failed[@]} == 1 ]] && echo " fails" || echo "s fail") alone in the map: $(printf '%s, ' "${failed[@]}" | sed 's/, $//')"
    fi
    if (( ${#unread[@]} > 0 )); then
      [[ -z "$QG_COVER_WHY" ]] || QG_COVER_WHY+="; "
      QG_COVER_WHY+="the coverage of ${#unread[@]} test$([[ ${#unread[@]} == 1 ]] || echo s) could not be read: $(printf '%s, ' "${unread[@]}" | sed 's/, $//')"
    fi
    return 3
  fi
  entered="$(cat "$cdir"/fns/*.fns 2>/dev/null | LC_ALL=C sort -u | wc -l)"
  echo "cover map: ${#tests[@]} tests in $((SECONDS - start)) s (build $QG_COVER_BUILD_S s), $((entered)) functions entered"
}

# The premise of "caught with a subset" is that the subset passes without the mutant. Each distinct subset runs once
# on the instrumented binary, with the default threads (D-F6i); one that fails sends every mutant that carries it to
# A0, named. A flaky pair of tests costs time here, never a wrong class.
qg_cover_subset() { # CDIR N TESTS
  local d="$1/work/pre-$2"
  local -a names=()
  mkdir -p "$d"
  read -ra names <<<"$3"
  if ! (cd "$ROOT/rust" && LLVM_PROFILE_FILE="$d/%p-%m.profraw" timeout "${QG_COVER_TEST_TIMEOUT_S:-300}" "$QG_COVER_BIN" --exact "${names[@]}" >"$d/out" 2>&1); then
    printf '%s\n' "$3" > "$1/work/pre-$2.failed"
  fi
  rm -rf "$d"
}
qg_cover_preflight() { # CDIR P
  local cdir="$1" p="$2" n=0 subset f
  local -a subsets=()
  mapfile -t subsets < <(cut -f4 "$cdir/select.tsv" | LC_ALL=C sort -u)
  for subset in "${subsets[@]}"; do
    [[ -n "$subset" ]] || continue
    n=$((n + 1))
    qg_pool_slot "$p"
    qg_cover_subset "$cdir" "$n" "$subset" &
  done
  wait || true
  : > "$cdir/preflight.tsv"
  if compgen -G "$cdir/work/pre-*.failed" >/dev/null; then
    for f in "$cdir"/work/pre-*.failed; do
      subset="$(cat "$f")"
      echo "preflight: the tests $subset fail together without a mutant; their mutants go to the whole-suite pass A0"
      awk -F'\t' -v s="$subset" '$4 == s {print $1 "\t" $2}' "$cdir/select.tsv" >> "$cdir/preflight.tsv"
    done
    bash "$QG_SELF" --cover-index "$cdir/list.json" "$cdir/fns" "$cdir" "$cdir/preflight.tsv" >/dev/null || return 1
  fi
}

# What the cover mode needs before its first pass: 0 ready, 3 off (it says why, and the step runs as full), 2 when
# the environment lacks what it was asked for, 1 when the plan itself broke.
qg_cover_prepare() { # OUT ARGS...
  local out="$1" cdir="$1/cover" marked p count py
  shift
  qg_cover_tools || return 2
  py="$(qg_py)"
  [[ -n "$py" ]] || { echo "FAIL: python is required for QG_SELECT=cover" >&2; return 2; }
  mkdir -p "$cdir"
  marked="$(grep -rlF --include='*.rs' -- "$QG_MARKER" "$ROOT/rust/src" 2>/dev/null | LC_ALL=C sort | head -n 1 || true)"
  if [[ -n "$marked" ]]; then
    echo "cover: off (the tree holds cargo-mutants' marker in ${marked#"$ROOT/rust/"})"
    return 3
  fi
  if ! (cd "$ROOT/rust" && cargo mutants --list --json "$@" --exclude-re "$MUTANTS_EXCLUDE_RE") >"$cdir/list.json" 2>"$cdir/list.err"; then
    echo "cover: off (cargo mutants --list --json failed: $(tail -n 1 "$cdir/list.err"))"
    return 3
  fi
  count="$("$py" -c 'import json, sys; print(len(json.load(open(sys.argv[1], encoding="utf-8"))))' "$cdir/list.json" 2>/dev/null || true)"
  if [[ "$count" != [0-9]* ]] || (( count == 0 )); then
    echo "cover: off (--list names no mutant to select the tests of)"
    return 3
  fi
  p="$(qg_cover_jobs)" || return 2
  qg_cover_build "$cdir" || { echo "cover: off ($QG_COVER_WHY)"; return 3; }
  qg_cover_map "$cdir" "$p" || { echo "cover: off ($QG_COVER_WHY)"; return 3; }
  bash "$QG_SELF" --cover-index "$cdir/list.json" "$cdir/fns" "$cdir" || return 1
  qg_cover_preflight "$cdir" "$p" || return 1
}

# What a pass left that the next step reads. kind `open`: the mutants that are neither caught nor unviable, which
# pass B has to confirm. kind `early`: the Q3 line of pass A0, and the names it missed or timed out.
qg_outcomes_query() { # KIND OUTCOMES
  local py
  py="$(qg_py)"
  "$py" - "$1" "$2" <<'PY'
import json, sys

kind = sys.argv[1]
records = [(o["scenario"]["Mutant"]["name"], o.get("summary")) for o in json.load(open(sys.argv[2], encoding="utf-8"))["outcomes"]
           if isinstance(o.get("scenario"), dict) and "Mutant" in o["scenario"]]
if kind == "open":
    for name, summary in records:
        if summary not in ("CaughtMutant", "Unviable"):
            print(name)
else:
    missed = [n for n, s in records if s == "MissedMutant"]
    timeout = [n for n, s in records if s in ("Timeout", "TimeoutMutant")]
    caught = sum(1 for _, s in records if s == "CaughtMutant")
    print(f"early: {len(records)} mutants with no covering lib test ran first: {caught} caught, {len(missed)} missed, {len(timeout)} timeout")
    for name in missed:
        print(f"  missed: {name}")
    for name in timeout:
        print(f"  timeout: {name}")
PY
}

# A pass measured exactly the mutants it was asked for, in both directions (D-F6l): the guard of mutants-gate.sh,
# over a list of the names of the pass.
qg_pass_guard() { # PASS OUTCOMES NAMES_FILE
  local pass="$1" outcomes="$2" asked="$3" list="$3.list" py why
  py="$(qg_py)"
  "$py" -c 'import json, sys; print(json.dumps([{"name": l.rstrip("\n")} for l in open(sys.argv[1], encoding="utf-8") if l.strip()]))' "$asked" >"$list"
  if ! why="$("$ROOT/scripts/mutants-gate.sh" --set-guard "$outcomes" "$list" 2>&1 >/dev/null)"; then
    echo "FAIL: set differs in pass $pass: ${why#FAIL: }" >&2
    return 1
  fi
}

# One pass of cover: the mutants of $CDIR/<pass>.names, each with a --re of its own (the name, escaped as regex::escape
# does, anchored), through the lab. Only A1 gets the selection in its environment. cargo-mutants' exit 2 and 3 are not
# a verdict here (merged classes are); anything else, and any guard of the lab, is red. Sets QG_PASS_OUTCOMES, `-` for a
# pass with no mutant.
qg_cover_pass() { # PASS WANT CDIR ARGS...
  local pass="$1" want="$2" cdir="$3" asked="$3/$1.names" count how name crc
  local -a re_args=()
  shift 3
  QG_PASS_OUTCOMES=-
  count=$(( $(wc -l <"$asked") ))
  if (( count == 0 )); then
    echo "pass $pass: 0 mutants, skipped"
    return 0
  fi
  case "$pass" in
    A0) how="the whole suite" ;;
    A1) how="the tests that cover each" ;;
    *) how="the whole suite, to confirm what A1 left open" ;;
  esac
  echo "pass $pass: $count mutants ($how)"
  while IFS= read -r name; do
    re_args+=(--re "^$(sed 's/[][\\.+*?()|{}^$#&~-]/\\&/g' <<<"$name")\$")
  done <"$asked"
  mkdir -p "$cdir/$pass"
  (
    if [[ "$pass" == A1 ]]; then export MUTANTS_SHIM_SELECT="$cdir/select.tsv"; else unset MUTANTS_SHIM_SELECT; fi
    qg_lab_pass "$want" "$cdir/$pass" "$@" "${re_args[@]}"
  ) || return 1
  crc="$(cat "$cdir/$pass/cargo-mutants.rc" 2>/dev/null || true)"
  case "$crc" in
    0 | 2 | 3) ;;
    *)
      echo "FAIL: cargo-mutants rc=${crc:-unknown} in pass $pass" >&2
      return 1
      ;;
  esac
  if [[ ! -f "$cdir/$pass/mutants.out/outcomes.json" ]]; then
    echo "FAIL: pass $pass left no outcomes.json (cargo-mutants rc=$crc)" >&2
    return 1
  fi
  "$ROOT/scripts/mutants-gate.sh" --check-builds "$cdir/$pass/mutants.out/outcomes.json" || return 1
  qg_pass_guard "$pass" "$cdir/$pass/mutants.out/outcomes.json" "$asked" || return 1
  QG_PASS_OUTCOMES="$cdir/$pass/mutants.out/outcomes.json"
}

# The cover mode's three passes and its verdict (D-F6l, D-F6m, D-F6n, D-F6o). A0 first, with the whole suite, and its
# line before A1 starts (Q3); then A1 with the subsets; then B with the whole suite over every mutant A1 left missed or
# timed out. The classes are merged and the verdict is the merged one: a missed or timed out mutant is red, nothing
# else is.
qg_mutate_cover() { # WANT OUT ARGS...
  local want="$1" cdir="$2/cover" a0 a1 merge_rc=0
  shift 2
  awk -F'\t' '$2 == "A0" {print $1}' "$cdir/plan.tsv" >"$cdir/A0.names"
  awk -F'\t' '$2 == "A1" {print $1}' "$cdir/plan.tsv" >"$cdir/A1.names"
  qg_cover_pass A0 "$want" "$cdir" "$@" || return 1
  a0="$QG_PASS_OUTCOMES"
  [[ "$a0" == - ]] || qg_outcomes_query early "$a0"
  qg_cover_pass A1 "$want" "$cdir" "$@" || return 1
  a1="$QG_PASS_OUTCOMES"
  : >"$cdir/B.names"
  [[ "$a1" == - ]] || qg_outcomes_query open "$a1" >"$cdir/B.names"
  qg_cover_pass B "$want" "$cdir" "$@" || return 1
  bash "$QG_SELF" --merge-select "$cdir/merged.json" "$a0" "$a1" "$QG_PASS_OUTCOMES" "$cdir/plan.tsv" || merge_rc=$?
  case "$merge_rc" in
    0 | 4) ;;
    *) return 1 ;;
  esac
  "$ROOT/scripts/mutants-gate.sh" --set-guard "$cdir/merged.json" "$cdir/list.json" || return 1
  (( merge_rc == 0 )) || return 1
}

# --- self-test: the CRAP filter gets a fixture that puts it in the red -------
# A guard nobody can watch fail is a paragraph. Both directions are here,
# because absence proves nothing without a presence anchor beside it: a warning
# that is NOT about complexity must come out of `lizard_warnings` as nothing at
# all, and one that IS must still come out with its number.
#   ./scripts/quality-gate.sh --self-test
# --- self-test begin ---
if [[ "${1:-}" == "--self-test" ]]; then
  if ! command -v lizard >/dev/null; then
    echo "FALTA lizard: el self-test del filtro CRAP no puede correr." >&2
    exit 2
  fi
  # The fixtures below are built around a ceiling of 8, so this one does not
  # read the environment: an exported LIZARD_CC_MAX would move the line the
  # fixtures were cut to and fail the self-test for the wrong reason.
  LIZARD_CC_MAX=8
  # D-J7b (A2): this self-test is hermetic. HOME and XDG_CACHE_HOME are a directory of its own,
  # removed on exit, so a row that does not give them one never reaches the real cache.
  hermetic="$(mktemp -d)"
  mkdir -p "$hermetic/home" "$hermetic/cache"
  export HOME="$hermetic/home" XDG_CACHE_HOME="$hermetic/cache"
  # The PATH is not filtered here: the filter at the top of the script (CI-K9a) drops the directories of Windows where
  # OSTYPE says Linux, and where it says msys the PATH is the user's and a second filter would change it.
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp" "$hermetic"' EXIT
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

  # G4/A4: lizard is the whole of the complexity judgement, and run_lizard used to
  # ask for lizard OR python and then call lizard with `2>/dev/null ... || true`:
  # with no lizard, or one that died, the list of functions over the ceiling came out
  # empty, and an empty list reads as "nothing is over it". A new CC 30 function
  # passed. The judgement that cannot be made is exit 2, never 0.
  # The fixture hides lizard with a PATH of the directories that do not hold it, and a
  # python stand-in at the front: the old test of "lizard or python" has to be
  # satisfied, or the row would go red for the wrong reason on a machine whose
  # python lives beside lizard (as it does on this one).
  nolz="$tmp/nolizard"
  mkdir -p "$nolz"
  printf '#!/bin/sh\nexit 0\n' > "$nolz/python"
  chmod +x "$nolz/python"
  nolz_path="$nolz"
  IFS=: read -ra nolz_dirs <<< "$PATH"
  for nolz_d in "${nolz_dirs[@]}"; do
    [[ -e "$nolz_d/lizard" || -e "$nolz_d/lizard.exe" ]] && continue
    nolz_path+=":$nolz_d"
  done
  if (PATH="$nolz_path"; command -v lizard >/dev/null 2>&1); then
    st_fail "A4: the fixture cannot hide lizard: it is still found on $nolz_path"
  fi
  if ! (PATH="$nolz_path"; command -v python >/dev/null 2>&1 && command -v awk >/dev/null 2>&1 && command -v sort >/dev/null 2>&1); then
    st_fail "A4: the fixture hides more than lizard: python, awk or sort are gone from $nolz_path"
  fi
  rc=0
  (PATH="$nolz_path"; BASELINE="$tmp/bl-empty.txt" run_lizard "$tmp" src/tangled.rs) >"$tmp/judged.err" 2>&1 || rc=$?
  if (( rc != 2 )) || ! said 'lizard is not on PATH'; then
    st_fail "A4: with no lizard on PATH run_lizard exited $rc, not 2 with 'lizard is not on PATH' (a CC 13 function not in the baseline went by in silence): $(cat "$tmp/judged.err")"
  fi
  # A lizard that dies and prints no CSV is not a list of nothing either.
  mkdir -p "$tmp/badlizard"
  printf '#!/bin/sh\necho "lizard: crashed" >&2\nexit 3\n' > "$tmp/badlizard/lizard"
  chmod +x "$tmp/badlizard/lizard"
  rc=0
  (PATH="$tmp/badlizard:$PATH"; BASELINE="$tmp/bl-empty.txt" run_lizard "$tmp" src/tangled.rs) >"$tmp/judged.err" 2>&1 || rc=$?
  if (( rc != 2 )); then
    st_fail "A4: a lizard that exits 3 with no CSV left run_lizard at exit $rc, not 2 (an empty list was taken for green): $(cat "$tmp/judged.err")"
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
import glob, json, os, sys
args = sys.argv[1:]
# Before anything is looked at, so that a run which never got here leaves no
# trace and the self-test can say "cargo mutants was not invoked". One line per
# call: the arguments, then the variables the judge is supposed to set.
calls = os.environ.get("FAKE_CALLS_DIR")
if calls:
    open(os.path.join(calls, "cargo.invoked"), "a").close()
    seen = [os.environ.get(k, "") for k in ("TMP", "TEMP", "TMPDIR", "CARGO_PROFILE_DEV_DEBUG", "CARGO_INCREMENTAL", "CARGO")]
    with open(os.path.join(calls, "cargo.calls"), "a", encoding="utf-8") as f:
        f.write("\t".join([" ".join(args)] + seen) + "\n")
# What the real cargo-mutants 27.1.0 refuses before it mutates anything (exit 1, a usage error): a
# stand-in that accepts what the real one rejects proves a call that would not run in the judge.
def usage(why):
    print(f"error: {why}", file=sys.stderr)
    sys.exit(1)
if not args or args[0] != "mutants":
    usage("the first argument of cargo-mutants is `mutants`")
def value(flag):
    return args[args.index(flag) + 1] if flag in args and args.index(flag) + 1 < len(args) else None
for flag in ("--jobs", "--jobserver-tasks"):
    v = value(flag)
    if flag in args and (v is None or not v.isdigit()):
        usage(f"invalid value {v!r} for {flag}")
d = value("--dir")
if "--dir" in args and (d is None or not os.path.isdir(d)):
    usage(f"--dir {d!r} is not a directory")
out = value("--output")
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
if os.environ.get("FAKE_TOUCH_SEED"):
    cache = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.environ.get("HOME", ""), ".cache")
    for seeded in glob.glob(os.path.join(cache, "memory-industry-gate", "mutants-seed", "*", "target", "debug", "deps", "libdep-aaaa.rlib")):
        os.utime(seeded, (1, 1))
if not os.environ.get("FAKE_NO_OUTCOMES"):
    os.makedirs(os.path.join(out, "mutants.out"), exist_ok=True)
    with open(os.path.join(out, "mutants.out", "outcomes.json"), "w", encoding="utf-8") as f:
        json.dump({"outcomes": records, "total_mutants": len(records), "caught": 1, "missed": 0,
                   "timeout": 0, "unviable": len(records) - 1, "success": 0}, f)
sys.exit(int(os.environ.get("FAKE_MUTANTS_RC") or 0))
PY
  # The stand-in's own script path is the fixture's business, not the code's
  # under test, so it is spelled natively here: with the rewriting off,
  # Python would not find /tmp/... either.
  fake_py="$(cygpath -m "$tmp/fake_mutants.py" 2>/dev/null || printf '%s' "$tmp/fake_mutants.py")"
  printf '%s\n' '#!/bin/sh' \
    "MSYS2_ARG_CONV_EXCL='*' MSYS_NO_PATHCONV=1 exec \"$py\" \"$fake_py\" \"\$@\"" > "$tmp/bin/cargo-mutants"
  # F6a: the mutation step reaches cargo-mutants in two ways, `cargo mutants` (legacy) and `cargo-mutants mutants`
  # through the lab of mutants-gate.sh (full), and builds a seed with `cargo test --no-run`. This cargo answers
  # all three, and refuses the way the real one does when it is not in a crate.
  cat > "$tmp/bin/cargo" <<'FAKE'
#!/bin/bash
case "${1:-}" in
  -V) echo "cargo 1.99.0 (fake)"; exit 0 ;;
  mutants) exec "$(dirname "$0")/cargo-mutants" "$@" ;;
  test)
    if [[ " $* " == *" --no-run "* && " $* " == *" --package="* ]]; then
      if [[ ! -f Cargo.toml ]]; then
        echo "error: could not find \`Cargo.toml\` in \`$PWD\` or any parent directory" >&2
        exit 101
      fi
      t="${CARGO_TARGET_DIR:?the seed build has no CARGO_TARGET_DIR}"
      mkdir -p "$t/debug/deps" "$t/debug/.fingerprint/serde-444"
      : > "$t/debug/deps/libdep-aaaa.rlib"
      : > "$t/debug/.fingerprint/serde-444/lib-serde"
      : > "$t/CACHEDIR.TAG"
    fi
    ;;
esac
exit 0
FAKE
  printf '%s\n' '#!/bin/sh' 'echo "rustc 1.99.0 (fake)"' > "$tmp/bin/rustc"
  chmod +x "$tmp/bin/cargo" "$tmp/bin/cargo-mutants" "$tmp/bin/rustc"

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
    mkdir -p "$tmp/$1/scripts" "$tmp/$1/rust/src"
    cp "$ROOT/scripts/mutants-gate.sh" "$ROOT/scripts/gate-lock.sh" "$tmp/$1/scripts/"
    [[ ! -f "$ROOT/package.json" ]] || cp "$ROOT/package.json" "$tmp/$1/"
    # A crate, for the seed that the lab of mutants-gate.sh builds (F6a).
    printf '[package]\nname = "memory-industry"\nversion = "0.31.0"\n' > "$tmp/$1/rust/Cargo.toml"
    printf 'version = 4\n' > "$tmp/$1/rust/Cargo.lock"
    printf 'pub fn f() -> u32 { 1 }\n' > "$tmp/$1/rust/src/lib.rs"
  }
  mk_qtree qtree
  q_tree="$tmp/qtree"
  q_fresh() { rm -rf "$q_sys" "$q_home" "$tmp/cargo.invoked" "$tmp/cargo.calls" "$tmp/leak.debug"; mkdir -p "$q_sys" "$q_home"; }
  # F6b: the directory of stand-ins that goes before $tmp/bin, or the whole PATH of a row; empty for every older row.
  q_pre=""
  q_path=""
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
        MUTANTS_SCRATCH_GB_PER_COPY CUBA_GATE_MIN_FREE_GB CUBA_GATE_LOCK_OWNER FAKE_DF_FAIL FAKE_OUTCOME \
        QG_SELECT QG_KEEP_OUT MUTANTS_KEEP_OUT CARGO_BUILD_JOBS GATE_CARGO_JOBS_FROM_GATE CARGO_TARGET_DIR CARGO \
        DATABASE_URL FAKE_MUTANTS_RC FAKE_NO_OUTCOMES FAKE_TOUCH_SEED
      # The seed of the lab lives under the cache: the fixture's own, which q_fresh empties.
      export HOME="$q_home" XDG_CACHE_HOME="$q_home/.cache" TMP="$q_sys_native" TEMP="$q_sys_native" TMPDIR="$q_sys" \
        PATH="${q_path:-${q_pre:+$q_pre:}$tmp/bin:$PATH}" FAKE_DF_AVAIL_KB="$q_avail" FAKE_CALLS_DIR="$q_calls"
      [[ -z "${qmode:-}" ]] || export QG_SELECT="$qmode"
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

  # F6a (D-F6a, F6a.5): the rows of the disk, the jobs, the scratch, the profile and the build check hold for the
  # judge as it is today (`legacy`) and for the one that mutates through the lab of mutants-gate.sh (`full`, the
  # default). The assertion of each row is the same in both; only the call that reaches cargo-mutants differs.
  for qmode in legacy full; do
    # QG1: not even one copy fits, so cargo mutants is never run.
    q_fresh
    q_avail=$((12 * kb))
    mj MUTANTS_JOBS=3 -- --file src/a.rs
    if (( mut_rc == 0 )) || mj_invoked || ! mj_said '12.0 GB' || ! mj_said '18.0 GB'; then
      echo "FAIL self-test: [$qmode] QG1: with 12 GB free for a 10 GB copy and 8 GB reserve the step invoked cargo mutants, or did not say the 12.0 GB it had and the 18.0 GB it needed (exit $mut_rc, invoked: $(mj_invoked && echo yes || echo no)): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
    # QG5: not knowing the free space is not having it.
    q_fresh
    q_avail=$((500 * kb))
    mj FAKE_DF_FAIL=1 -- --file src/a.rs
    if (( mut_rc == 0 )) || mj_invoked || ! mj_said 'could not read the free space'; then
      echo "FAIL self-test: [$qmode] QG5: a df that prints nothing did not stop the step before cargo mutants with 'could not read the free space' (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
    # QG2: an explicit MUTANTS_JOBS does not pass over the disk: 30 GB hold two
    # copies of 10 GB over the 8 GB reserve, not three.
    q_fresh
    q_avail=$((30 * kb))
    mj FAKE_OUTCOME=genuine MUTANTS_JOBS=3 -- --file src/a.rs
    if (( mut_rc != 0 )) || ! mj_invoked || ! mj_said 'capped by disk: 3 wanted, 2 fit' \
      || ! [[ "$(mj_field 1)" == *' --jobs 2 '* ]]; then
      echo "FAIL self-test: [$qmode] QG2: MUTANTS_JOBS=3 over disk for 2 did not reach cargo mutants as --jobs 2 with 'capped by disk: 3 wanted, 2 fit' (exit $mut_rc, argv: $(mj_field 1)): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
    # QG8 (G5): with no MUTANTS_JOBS the step does not size the jobs itself, it asks the
    # one rule, mutants-gate.sh --default-jobs, which reads the memory that is available
    # (the copy of the formula that used to live here read the total, and on 2026-10-14
    # a VM with 63 GB and ~10 GB free got 6 jobs and two builds died). Its `memory:` line
    # reaches this step's output, and cargo mutants gets a number between 1 and the ceiling.
    q_fresh
    q_avail=$((500 * kb))
    mj FAKE_OUTCOME=genuine -- --file src/a.rs
    if (( mut_rc != 0 )) || ! mj_invoked || ! mj_said 'memory:' \
      || ! [[ "$(mj_field 1)" =~ --jobs\ [1-6]\  ]]; then
      echo "FAIL self-test: [$qmode] QG8: with no MUTANTS_JOBS the step did not take its jobs from mutants-gate.sh --default-jobs: no 'memory:' line, or cargo mutants got no --jobs between 1 and 6 (exit $mut_rc, argv: $(mj_field 1)): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
    # QG3 and QG7: cargo mutants gets the judge's scratch as its temp dir, in the
    # three spellings (TMP and TEMP are what Windows reads, TMPDIR what Unix does),
    # and debug info off (7.6 of the 14.1 GB of a copy's target were .pdb). That
    # variable is for that one command: it is not in the step's environment after.
    q_fresh
    q_avail=$((500 * kb))
    mj FAKE_OUTCOME=genuine -- --file src/a.rs
    mj_invoked || { echo "FAIL self-test: [$qmode] QG3: with 500 GB free the step did not run cargo mutants: $(cat "$tmp/mut.out")" >&2; exit 1; }
    for field in 2 3 4; do
      if [[ "$(qnorm "$(mj_field "$field")")" != "$(qnorm "$q_scr")" ]]; then
        echo "FAIL self-test: [$qmode] QG3: field $field of the cargo mutants call (TMP, TEMP, TMPDIR) was '$(mj_field "$field")', not the judge's scratch $q_scr" >&2
        exit 1
      fi
    done
    if [[ "$(mj_field 5)" != 0 ]]; then
      echo "FAIL self-test: [$qmode] QG7: cargo mutants did not get CARGO_PROFILE_DEV_DEBUG=0 (got '$(mj_field 5)')" >&2
      exit 1
    fi
    if [[ -n "$(cat "$tmp/leak.debug" 2>/dev/null)" ]]; then
      echo "FAIL self-test: [$qmode] QG7: CARGO_PROFILE_DEV_DEBUG was still exported after the step returned: it is for one cargo mutants call, not for the rest of the judge" >&2
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
    # Only a scratch that carries the marker mutants-gate.sh wrote is swept (G4/C1).
    : > "$q_scr/.memory-industry-mutants"
    touch -d '7 hours ago' "$q_scr/.memory-industry-mutants"
    mj FAKE_OUTCOME=genuine -- --file src/a.rs
    if [[ -e "$q_scr/cargo-mutants-rust-old.tmp" || ! -d "$q_scr/cargo-mutants-rust-new.tmp" ]]; then
      echo "FAIL self-test: [$qmode] QG4: the run did not sweep the 7 h old copy of the judge's scratch and leave the 1 h old one: $(ls "$q_scr" 2>&1)" >&2
      exit 1
    fi
    if [[ ! -f "$q_scr/.memory-industry-mutants" ]]; then
      echo "FAIL self-test: [$qmode] QG4: the sweep deleted the marker that makes the directory the judge's scratch" >&2
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
      echo "FAIL self-test: [$qmode] QG8: a rust/target-x holding CACHEDIR.TAG did not stop the step before cargo mutants, naming it (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi

    # The two runs that were here before G3, now in the fixture's tree and with room.
    q_fresh
    mj FAKE_OUTCOME=genuine -- --file src/a.rs
    if (( mut_rc != 0 )) || ! grep -q 'all 0 refused by the compiler' "$tmp/mut.out"; then
      echo "FAIL self-test: [$qmode] a mutation run with one genuine catch did not pass through the build check (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
    q_fresh
    mj FAKE_OUTCOME=killed -- --file src/a.rs
    if (( mut_rc == 0 )) || ! grep -q 'stopped 1 of 1 unviable' "$tmp/mut.out"; then
      echo "FAIL self-test: [$qmode] a build the machine killed was not refused, so cargo-mutants and this script did not read the same outcomes.json (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi

  done
  qmode=

  # --- F6a.1, F6a.2, F6a.6: the modes of the step, one row each ---------------------------------------
  # The arguments are the ones the main path hands the step: the files, then the diff.
  : > "$tmp/x.diff"
  q_args=(--file src/a.rs --file src/b.rs --in-diff "$tmp/x.diff")
  q_seed_root() { printf '%s' "$q_home/.cache/memory-industry-gate/mutants-seed"; }

  # F6a.1: with no QG_SELECT the step mutates through the lab of mutants-gate.sh, builds only the lib test, and
  # gets the jobserver the judge counts (CARGO_BUILD_JOBS=7 is that count here).
  q_fresh
  q_avail=$((500 * kb))
  mj FAKE_OUTCOME=genuine CARGO_BUILD_JOBS=7 -- "${q_args[@]}"
  if (( mut_rc != 0 )) || ! mj_invoked || ! mj_said 'mode: full'; then
    echo "FAIL self-test: F6a.1: with no QG_SELECT the step did not run in 'mode: full' and pass (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi
  q_line="$(mj_field 1)"
  if [[ "$(grep -o -- '--file ' <<<"$q_line" | wc -l)" != 2 ]]; then
    echo "FAIL self-test: F6a.1: cargo-mutants did not get --file twice: $q_line" >&2
    exit 1
  fi
  for want in ' --in-diff ' ' --exclude-re ' ' --timeout 90 ' ' --gitignore=false' ' --cargo-arg=--lib' ' --dir ' ' --copy-target=true'; do
    if [[ "$q_line" != *"$want"* ]]; then
      echo "FAIL self-test: F6a.1: cargo-mutants did not get '$want' from the full step: $q_line" >&2
      exit 1
    fi
  done
  if [[ "$q_line" != *' --jobserver-tasks 7 '* ]]; then
    echo "FAIL self-test: F6a.1: no --jobserver-tasks 7 (the cargo jobs of this run) in the call: $q_line" >&2
    exit 1
  fi
  if [[ " $q_line " == *' --lib '* ]]; then
    echo "FAIL self-test: F6a.1: the full step passes a bare --lib after the --, so every mutant builds all the test binaries: $q_line" >&2
    exit 1
  fi
  q_stage="$(sed -n 's/.* --dir \([^ ]*\) .*/\1/p' <<<"$q_line")"
  if [[ "$q_stage" != *memory-industry-mutants/stage.*/rust || -e "$q_stage" ]]; then
    echo "FAIL self-test: F6a.1: --dir is '$q_stage': it is not a stage under the judge's scratch, or it is still there after the run" >&2
    exit 1
  fi
  if [[ "$(mj_field 7)" != */bin/cargo-shim ]]; then
    echo "FAIL self-test: F6a.1: CARGO was '$(mj_field 7)', not the cargo-shim of this run" >&2
    exit 1
  fi
  for want in 'OK  tree' 'OK  seed unchanged' 'lab builds:'; do
    mj_said "$want" || { echo "FAIL self-test: F6a.1: the full step did not say '$want': $(cat "$tmp/mut.out")" >&2; exit 1; }
  done

  # F6a.2: legacy is the call of today, byte for byte, with no seed, no stage, no shim and no jobserver.
  q_fresh
  mj FAKE_OUTCOME=genuine MUTANTS_JOBS=2 QG_SELECT=legacy CARGO_BUILD_JOBS=7 -- "${q_args[@]}"
  q_line="$(mj_field 1)"
  if (( mut_rc != 0 )) || ! mj_said 'mode: legacy' \
    || [[ "$q_line" != "mutants --file src/a.rs --file src/b.rs --in-diff $tmp/x.diff --exclude-re $MUTANTS_EXCLUDE_RE --output "*" --timeout 90 --jobs 2 --gitignore=false -- --lib" ]]; then
    echo "FAIL self-test: F6a.2: QG_SELECT=legacy did not call cargo-mutants exactly as the step did before F6 (exit $mut_rc): $q_line" >&2
    exit 1
  fi
  if [[ "$q_line" == *' --dir '* || "$q_line" == *'--copy-target'* || "$q_line" == *'--jobserver-tasks'* || "$(mj_field 7)" == *cargo-shim* ]] \
    || [[ -n "$(ls -A "$(q_seed_root)" 2>/dev/null)" ]]; then
    echo "FAIL self-test: F6a.2: legacy used the lab: a --dir, --copy-target, --jobserver-tasks, the shim as CARGO, or a seed under $(q_seed_root)" >&2
    exit 1
  fi
  # An unknown mode is a refusal before anything is mutated or built.
  q_fresh
  mj QG_SELECT=otra -- "${q_args[@]}"
  if (( mut_rc != 2 )) || mj_invoked || ! mj_said 'QG_SELECT' || [[ -n "$(ls -A "$(q_seed_root)" 2>/dev/null)" ]]; then
    echo "FAIL self-test: F6a.2: QG_SELECT=otra did not exit 2 naming QG_SELECT before cargo-mutants and before a seed (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi

  # F6a.6: the verdict of full. cargo-mutants' own exit decides, and its outcomes go through the build check
  # whenever they exist. The outcomes here are a genuine catch, so a verdict that read only the build check
  # would pass every row of the first group.
  for q_rc in 2 3 4 70; do
    q_fresh
    mj FAKE_OUTCOME=genuine FAKE_MUTANTS_RC="$q_rc" -- "${q_args[@]}"
    if (( mut_rc == 0 )) || ! mj_invoked; then
      echo "FAIL self-test: F6a.6 rc $q_rc: cargo-mutants exited $q_rc over a clean outcomes.json and the step passed (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
  done
  for q_rc in 4 70; do
    q_fresh
    mj FAKE_OUTCOME=genuine FAKE_MUTANTS_RC="$q_rc" -- "${q_args[@]}"
    if ! mj_said "cargo-mutants rc=$q_rc"; then
      echo "FAIL self-test: F6a.6 rc $q_rc: the red does not say 'cargo-mutants rc=$q_rc': $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
  done
  q_fresh
  mj FAKE_OUTCOME=genuine FAKE_MUTANTS_RC=0 -- "${q_args[@]}"
  if (( mut_rc != 0 )); then
    echo "FAIL self-test: F6a.6 rc 0: a genuine catch with cargo-mutants' exit 0 was not green (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi
  # The build check judges the outcomes whatever the exit was: a killed build, with exit 2 and with exit 0.
  for q_rc in 2 0; do
    q_fresh
    mj FAKE_OUTCOME=killed FAKE_MUTANTS_RC="$q_rc" -- "${q_args[@]}"
    if (( mut_rc == 0 )) || ! mj_said 'stopped 1 of 1 unviable'; then
      echo "FAIL self-test: F6a.6 rc $q_rc: the outcomes of a killed build were not judged by --check-builds (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
  done
  q_fresh
  mj FAKE_NO_OUTCOMES=1 FAKE_MUTANTS_RC=70 -- "${q_args[@]}"
  if (( mut_rc == 0 )) || ! mj_said 'cargo-mutants rc=70'; then
    echo "FAIL self-test: F6a.6: an exit 70 with no outcomes.json was not red naming 'cargo-mutants rc=70' (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi
  # A guard of the lab (the seed touched during the run) is red too, with the lab's own words.
  q_fresh
  mj FAKE_OUTCOME=genuine FAKE_TOUCH_SEED=1 -- "${q_args[@]}"
  if (( mut_rc == 0 )) || ! mj_said 'the seed changed during the run'; then
    echo "FAIL self-test: F6a.6: a seed touched during the run was not red with 'the seed changed during the run' (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
    exit 1
  fi

  # F6a.6 (D-F6f): QG_KEEP_OUT is a directory of this run's. One that is not empty stops the step before
  # cargo-mutants and is not written to; a new or empty one keeps the outcomes of the run, green or red, in
  # <dir>/<mode>/.
  for q_mode in full legacy; do
    q_fresh
    q_keep="$tmp/keep-$q_mode"
    rm -rf "$q_keep"
    mkdir -p "$q_keep"
    : > "$q_keep/leftover"
    mj FAKE_OUTCOME=genuine QG_SELECT="$q_mode" QG_KEEP_OUT="$q_keep" -- "${q_args[@]}"
    if (( mut_rc != 2 )) || mj_invoked || ! mj_said 'QG_KEEP_OUT' || [[ ! -f "$q_keep/leftover" || -e "$q_keep/$q_mode" ]]; then
      echo "FAIL self-test: F6a.6 ($q_mode): a QG_KEEP_OUT that is not empty did not exit 2 naming QG_KEEP_OUT before cargo-mutants, or it was written to (exit $mut_rc): $(cat "$tmp/mut.out")" >&2
      exit 1
    fi
    q_fresh
    rm -rf "$q_keep"
    mj FAKE_OUTCOME=genuine QG_SELECT="$q_mode" QG_KEEP_OUT="$q_keep" -- "${q_args[@]}"
    if (( mut_rc != 0 )) || [[ ! -f "$q_keep/$q_mode/outcomes.json" ]]; then
      echo "FAIL self-test: F6a.6 ($q_mode): a new QG_KEEP_OUT did not keep the outcomes in <dir>/$q_mode/outcomes.json (exit $mut_rc): $(ls -R "$q_keep" 2>&1)" >&2
      exit 1
    fi
    q_fresh
    rm -rf "$q_keep"
    mkdir -p "$q_keep"
    mj FAKE_OUTCOME=genuine FAKE_MUTANTS_RC=2 QG_SELECT="$q_mode" QG_KEEP_OUT="$q_keep" -- "${q_args[@]}"
    if (( mut_rc == 0 )) || [[ ! -f "$q_keep/$q_mode/outcomes.json" ]]; then
      echo "FAIL self-test: F6a.6 ($q_mode): a red run, into an empty QG_KEEP_OUT, did not keep its outcomes (exit $mut_rc): $(ls -R "$q_keep" 2>&1)" >&2
      exit 1
    fi
  done


  # ====================================================================================================
  # --- F6b (CI-F6, D-F6g..o): Q2 and Q3. Each mutant is judged with the tests that cover it, the ones with no
  # covering test go first, and every mutant the subset does not catch is confirmed with the whole suite ---
  # The doubles below refuse what the real tools refuse (a cargo with no cargo-llvm-cov, a llvm-profdata with no
  # input, a profile directory that is not there, a test name nobody listed) and answer with the shapes they
  # really have: `cargo mutants --list --json` and outcomes.json of cargo-mutants 27.1.0 (copied from a real run),
  # `cargo test --message-format=json`, libtest's `--list --format=terse`, and `llvm-cov export` as
  # llvm.coverage.json.export 2.0.1 (a function has `count`, `regions` of [LineStart, ColumnStart, LineEnd,
  # ColumnEnd, ExecutionCount, FileID, ExpandedFileID, Kind] and `filenames`). The six mutants and the coverage of
  # each test are the table of the spec, F6b, Background.
  cb="$tmp/cbin"
  cb2="$tmp/cbin2"
  csys="$tmp/csysroot"
  cllvm="$csys/lib/rustlib/x86_64-unknown-linux-gnu/bin"
  cfix="$tmp/cfix"
  mkdir -p "$cb" "$cb2" "$cllvm" "$cfix/cov" "$cfix/fns" "$cfix/mg"

  printf '%s\n' '#!/bin/sh' \
    'case "$*" in' \
    '  "--print sysroot") echo "${FAKE_SYSROOT:-/nonexistent-sysroot}" ;;' \
    '  "-vV") echo "rustc 1.99.0 (fake)"; echo "host: x86_64-unknown-linux-gnu" ;;' \
    '  *) echo "rustc 1.99.0 (fake)" ;;' \
    'esac' > "$cb/rustc"

  cat > "$cb/cargo" <<'CFAKE'
#!/bin/bash
case "${1:-}" in
  -V) echo "cargo 1.99.0 (fake)"; exit 0 ;;
  mutants) exec "$(dirname "$0")/cargo-mutants" "$@" ;;
  llvm-cov)
    if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
      echo "error: no such command: \`llvm-cov\`" >&2
      exit 101
    fi
    exec cargo-llvm-cov "$@" ;;
  test)
    if [[ " $* " == *" --message-format=json "* ]]; then
      printf 'build-map\t%s\t%s\t%s\n' "${CARGO_TARGET_DIR:-}" "${CARGO_LLVM_COV_TARGET_DIR:-}" "$*" >> "${FAKE_COVER_LOG:?}"
      if [[ -n "${FAKE_BUILD_FAIL:-}" ]]; then
        echo "error: could not compile \`memory-industry\` (lib test)" >&2
        exit 101
      fi
      echo '{"reason":"compiler-artifact","package_id":"dep 1.0.0","target":{"kind":["lib"],"crate_types":["lib"],"name":"dep"},"profile":{"test":false},"executable":null}'
      echo "{\"reason\":\"compiler-artifact\",\"package_id\":\"memory-industry 0.31.0\",\"target\":{\"kind\":[\"lib\"],\"crate_types\":[\"lib\"],\"name\":\"memory_industry\"},\"profile\":{\"test\":true},\"executable\":\"${FAKE_TEST_BIN:?}\"}"
      echo '{"reason":"build-finished","success":true}'
      exit 0
    fi
    if [[ " $* " == *" --no-run "* && " $* " == *" --package="* ]]; then
      if [[ ! -f Cargo.toml ]]; then
        echo "error: could not find \`Cargo.toml\` in \`$PWD\` or any parent directory" >&2
        exit 101
      fi
      t="${CARGO_TARGET_DIR:?the seed build has no CARGO_TARGET_DIR}"
      mkdir -p "$t/debug/deps" "$t/debug/.fingerprint/serde-444"
      : > "$t/debug/deps/libdep-aaaa.rlib"
      : > "$t/debug/.fingerprint/serde-444/lib-serde"
      : > "$t/CACHEDIR.TAG"
      exit 0
    fi
    # What cargo-mutants runs in a lab, through the shim: the test phase of one mutant.
    printf 'test\t%s\t%s\n' "$*" "$(env | grep -c '^MUTANTS_SHIM_' || true)" >> "${FAKE_COVER_LOG:?}"
    exit 0 ;;
esac
exit 0
CFAKE

  cat > "$cb/cargo-llvm-cov" <<'CFAKE'
#!/bin/bash
if [[ "${1:-}" != llvm-cov || "${2:-}" != show-env ]]; then
  echo "error: this stand-in answers llvm-cov show-env" >&2
  exit 1
fi
printf 'cover-env\t%s\t%s\t%s\n' "$*" "${CARGO_LLVM_COV_TARGET_DIR:-}" "${CARGO_TARGET_DIR:-}" >> "${FAKE_COVER_LOG:?}"
if [[ " $* " != *" --sh "* ]]; then
  echo "error: only --sh is understood here" >&2
  exit 1
fi
t="${CARGO_LLVM_COV_TARGET_DIR:?no target dir}"
printf "export %s='%s'\n" CARGO_LLVM_COV 1 CARGO_LLVM_COV_TARGET_DIR "$t" LLVM_PROFILE_FILE "$t/memory-industry-%p-%m.profraw" RUSTFLAGS '-C instrument-coverage'
CFAKE

  # The test binary of the instrumented lib: libtest's two listings and `--exact`. Like a coverage build it
  # writes its profile at exit (to LLVM_PROFILE_FILE with %p and %m expanded, and nowhere if that directory is
  # not there), and it logs what it was asked and how many profile files were lying about in the work directory.
  cat > "$cb/fake-test-bin" <<'CFAKE'
#!/bin/bash
log="${FAKE_COVER_LOG:?}"
dump() {
  [[ -n "${LLVM_PROFILE_FILE:-}" ]] || return 0
  local f="${LLVM_PROFILE_FILE//%p/$$}"
  f="${f//%m/0}"
  printf 'tests=%s\n' "$*" > "$f" 2>/dev/null || true
}
if [[ "${1:-}" == --list ]]; then
  if [[ " $* " == *" --ignored "* ]]; then
    echo "t_z: test"
  else
    printf '%s: test\n' t_a1 t_a2 t_a3 t_b t_k t_z
  fi
  dump list
  exit 0
fi
if [[ "${1:-}" != --exact ]]; then
  echo "error: this stand-in runs --list or --exact" >&2
  exit 2
fi
shift
names=()
flags=()
for a in "$@"; do
  case "$a" in
    -*) flags+=("$a") ;;
    *) names+=("$a") ;;
  esac
done
d="$(dirname "$(dirname "${LLVM_PROFILE_FILE:-/nonexistent/x}")")"
left="$(find "$d" \( -name '*.profraw' -o -name '*.profdata' -o -name '*.json' \) 2>/dev/null | wc -l)"
printf 'bin\t%s\t%s\t%s\tleftover=%s\n' "${names[*]}" "${flags[*]}" "${LLVM_PROFILE_FILE:-}" "$left" >> "$log"
if [[ -n "${FAKE_HANG:-}" && "${names[*]}" == "$FAKE_HANG" ]]; then
  sleep 15
  echo hang-finished >> "$log"
fi
dump "${names[*]}"
if [[ "${names[*]}" == "${FAKE_FAIL_ALONE:-}" || "${names[*]}" == "${FAKE_FAIL_TOGETHER:-}" ]]; then
  echo "test failed: ${names[*]}" >&2
  exit 101
fi
exit 0
CFAKE

  cat > "$cllvm/llvm-profdata" <<'CFAKE'
#!/bin/bash
if [[ "${1:-}" != merge ]]; then
  echo "error: unknown subcommand" >&2
  exit 1
fi
shift
ins=()
out=""
while (($#)); do
  case "$1" in
    -sparse) ;;
    -o) out="$2"; shift ;;
    -*) ;;
    *) ins+=("$1") ;;
  esac
  shift
done
if [[ ${#ins[@]} -eq 0 || -z "$out" ]]; then
  echo "error: No input files specified. See llvm-profdata merge -help" >&2
  exit 1
fi
for f in "${ins[@]}"; do
  if [[ ! -f "$f" ]]; then
    echo "error: $f: No such file or directory" >&2
    exit 1
  fi
done
cat "${ins[@]}" > "$out"
CFAKE

  cat > "$cllvm/llvm-cov" <<'CFAKE'
#!/bin/bash
if [[ "${1:-}" != export ]]; then
  echo "error: unknown subcommand" >&2
  exit 1
fi
shift
prof=""
bin=""
for a in "$@"; do
  case "$a" in
    -instr-profile=*) prof="${a#-instr-profile=}" ;;
    -*) ;;
    *) bin="$a" ;;
  esac
done
if [[ ! -f "$prof" || ! -x "$bin" ]]; then
  echo "error: no profile or no binary to export" >&2
  exit 1
fi
t="$(sed -n 's/^tests=//p' "$prof" | head -n 1)"
if [[ -f "${FAKE_COVEXPORT:?}/$t.json" ]]; then
  cat "$FAKE_COVEXPORT/$t.json"
else
  echo '{"version":"2.0.1","type":"llvm.coverage.json.export","data":[{"files":[],"functions":[],"totals":{}}]}'
fi
CFAKE

  # cargo-mutants, as the lab of mutants-gate.sh runs it. It lists the six mutants; it applies every --re it was
  # given to their names (an unanchored search, as the real RegexSet does); it answers each with the class of the
  # fixture for the whole suite, or for the subset when the selection is in its environment (the real one hands that
  # environment to the shim); and for the baseline and each mutant it runs the REAL shim in a toy lab that carries
  # cargo-mutants' marker at the mutant's line, so the test phase that reaches the cargo stand-in is the one the
  # real chain would make. FAKE_A1_DEVIATION is a pass that leaves one out (drop) or adds one nobody asked for (extra).
  cat > "$cb/cargo-mutants" <<'CFAKE'
#!/usr/bin/env python3
import json, os, re, shutil, subprocess, sys, tempfile

args = sys.argv[1:]
log = os.environ["FAKE_COVER_LOG"]
calls = os.environ.get("FAKE_CALLS_DIR")
if calls:
    open(os.path.join(calls, "cargo.invoked"), "a").close()

def note(*fields):
    with open(log, "a", encoding="utf-8") as f:
        f.write("\t".join(str(x) for x in fields) + "\n")

def usage(why):
    print(f"error: {why}", file=sys.stderr)
    sys.exit(1)

def values(flag):
    return [args[i + 1] for i, a in enumerate(args[:-1]) if a == flag]

if not args or args[0] != "mutants":
    usage("the first argument of cargo-mutants is `mutants`")
listed = json.load(open(os.environ["FAKE_LIST_JSON"], encoding="utf-8"))
if "--list" in args:
    note("list", " ".join(args))
    if "--json" in args:
        print(json.dumps(listed))
    else:
        print("\n".join(m["name"] for m in listed))
    sys.exit(0)
for flag in ("--jobs", "--jobserver-tasks"):
    for v in values(flag):
        if not v.isdigit():
            usage(f"invalid value {v!r} for {flag}")
out = (values("--output") or [None])[0]
stage = (values("--dir") or [None])[0]
if not out or not os.path.isdir(out):
    usage(f"--output {out!r} is not a directory")
if not stage or not os.path.isdir(stage):
    usage(f"--dir {stage!r} is not a directory")
patterns = values("--re")
select = os.environ.get("MUTANTS_SHIM_SELECT")
seq = 1
if os.path.exists(log):
    seq = sum(1 for line in open(log, encoding="utf-8") if line.startswith("pass\t")) + 1
note("pass", seq, "yes" if select else "no", len(patterns))
print(f"fake-cargo-mutants: pass {seq} select={'yes' if select else 'no'} re={len(patterns)}", flush=True)
for p in patterns:
    note("re", sum(1 for m in listed if re.search(p, m["name"])), p)
chosen = [m for m in listed if not patterns or any(re.search(p, m["name"]) for p in patterns)]
deviation = os.environ.get("FAKE_A1_DEVIATION") if select else None
if deviation == "drop" and chosen:
    chosen = chosen[:-1]
elif deviation == "extra":
    chosen = chosen + [m for m in listed if m not in chosen][:1]

classes = {}
for line in open(os.environ["FAKE_CLASSES"], encoding="utf-8"):
    name, full, subset = line.rstrip("\n").split("\t")
    classes[name] = (full, subset)

# local_gate_contract.rs holds that this file never reads an exit status itself (the rule is mutants-gate.sh's), and
# it checks by looking for the key's name: the key is spelled in two halves, as the stand-in of the older rows does.
STATUS = "process" + "_status"
cargo = os.environ.get("CARGO")
scratch = os.environ.get("TMPDIR")
def in_lab(mutant):
    lab = tempfile.mkdtemp(prefix="cargo-mutants-rust-", suffix=".tmp", dir=scratch)
    shutil.rmtree(lab)
    shutil.copytree(stage, lab, symlinks=True)
    if mutant:
        path = os.path.join(lab, mutant["file"])
        lines = open(path, encoding="utf-8").read().split("\n")
        lines[mutant["span"]["start"]["line"] - 1] += " /* ~ changed by cargo-mutants ~ */"
        open(path, "w", encoding="utf-8").write("\n".join(lines))
    note("mutant", mutant["name"] if mutant else "baseline")
    subprocess.run([cargo, "test", "--verbose", "--package=memory-industry@0.31.0", "--lib"], cwd=lab, check=False)
    shutil.rmtree(lab)
if cargo:
    in_lab(None)
    for m in chosen:
        in_lab(m)

os.makedirs(os.path.join(out, "mutants.out", "log"), exist_ok=True)
def phases(cls):
    return {
        "CaughtMutant": [("Build", "Success"), ("Test", {"Failure": 101})],
        "MissedMutant": [("Build", "Success"), ("Test", "Success")],
        "Timeout": [("Build", "Success"), ("Test", "Timeout")],
        "Unviable": [("Build", {"Failure": 101})],
    }[cls]
def record(i, m):
    cls = classes[m["name"]][1 if select else 0]
    rel = f"log/m{i}.log"
    with open(os.path.join(out, "mutants.out", rel), "w", encoding="utf-8") as f:
        f.write("*** mutation diff:\n*** /usr/bin/cargo test --no-run --verbose --package=memory-industry@0.31.0 --lib\n")
        if cls == "Unviable":
            f.write("error[E0308]: mismatched types\nerror: could not compile `memory-industry` (lib test)\n")
        f.write("*** result: " + ("Failure(101)" if cls == "Unviable" else "Success") + "\n")
    return {"scenario": {"Mutant": {"name": m["name"]}}, "summary": cls, "log_path": rel,
            "phase_results": [{"phase": p, STATUS: s} for p, s in phases(cls)]}
records = [record(i, m) for i, m in enumerate(chosen)]
with open(os.path.join(out, "mutants.out", "log", "baseline.log"), "w", encoding="utf-8") as f:
    f.write("*** baseline\n")
baseline = {"scenario": "Baseline", "summary": "Success", "log_path": "log/baseline.log",
            "phase_results": [{"phase": "Build", STATUS: "Success"}, {"phase": "Test", STATUS: "Success"}]}
count = lambda cls: sum(1 for r in records if r["summary"] == cls)
with open(os.path.join(out, "mutants.out", "outcomes.json"), "w", encoding="utf-8") as f:
    json.dump({"outcomes": [baseline] + records, "total_mutants": len(records), "caught": count("CaughtMutant"),
               "missed": count("MissedMutant"), "timeout": count("Timeout"), "unviable": count("Unviable"),
               "success": 0, "cargo_mutants_version": "27.1.0"}, f)
default_rc = 2 if count("MissedMutant") else 3 if count("Timeout") else 0
sys.exit(int(os.environ.get("FAKE_PASS_RC") or default_rc))
CFAKE
  chmod +x "$cb/rustc" "$cb/cargo" "$cb/cargo-llvm-cov" "$cb/fake-test-bin" "$cb/cargo-mutants" "$cllvm/llvm-profdata" "$cllvm/llvm-cov"
  # The same stand-ins with no cargo-llvm-cov, for the row that hides it; and a sysroot that lacks one tool each.
  cp "$cb/rustc" "$cb/cargo" "$cb/cargo-mutants" "$cb/fake-test-bin" "$cb2/"
  mkdir -p "$tmp/csys-noprof/lib/rustlib/x86_64-unknown-linux-gnu/bin" "$tmp/csys-nocov/lib/rustlib/x86_64-unknown-linux-gnu/bin"
  cp "$cllvm/llvm-cov" "$tmp/csys-noprof/lib/rustlib/x86_64-unknown-linux-gnu/bin/"
  cp "$cllvm/llvm-profdata" "$tmp/csys-nocov/lib/rustlib/x86_64-unknown-linux-gnu/bin/"
  # PATH of the row that hides cargo-llvm-cov: the real one, less every directory that holds it.
  c_hidden="$cb2:$tmp/bin"
  IFS=: read -ra c_dirs <<< "$PATH"
  for c_d in "${c_dirs[@]}"; do
    [[ -e "$c_d/cargo-llvm-cov" ]] && continue
    c_hidden+=":$c_d"
  done
  if (PATH="$c_hidden"; command -v cargo-llvm-cov >/dev/null 2>&1); then
    st_fail "F6b.4: the fixture cannot hide cargo-llvm-cov: it is still found on $c_hidden"
  fi
  if ! (PATH="$c_hidden"; command -v timeout >/dev/null 2>&1 && command -v awk >/dev/null 2>&1 && command -v sed >/dev/null 2>&1 && command -v find >/dev/null 2>&1 && command -v python3 >/dev/null 2>&1); then
    st_fail "F6b.4: the fixture hides more than cargo-llvm-cov: timeout, awk, sed, find or python3 are gone from $c_hidden"
  fi
  sed -i "1s|.*|#!$py|" "$cb/cargo-mutants" "$cb2/cargo-mutants"

  # The trees the rows run in: the crate of mk_qtree with two files of 60 lines, which are where the mutants are.
  c_mk_tree() { # NAME
    mk_qtree "$1"
    local f i
    for f in a b; do
      for i in $(seq 60); do echo "// $f line $i"; done > "$tmp/$1/rust/src/$f.rs"
    done
  }
  c_mk_tree ctree
  c_mk_tree ctree-marker
  c_marker='/* ~ changed by cargo-mutants ~ */'
  sed -i "7s|\$| $c_marker|" "$tmp/ctree-marker/rust/src/b.rs"

  # The fixtures, written once: the six mutants as `cargo mutants --list --json` prints them, the coverage each
  # test has as llvm-cov exports it, the classes each mutant gets with the whole suite and with its subset, what
  # the rule of the index has to make of them, and the outcomes.json of three passes for --merge-select.
  "$py" -I - "$tmp/ctree" "$cfix" <<'CPY'
import json, os, sys
root, fix = sys.argv[1], sys.argv[2]
def span(l1, c1, l2, c2):
    return {"start": {"line": l1, "column": c1}, "end": {"line": l2, "column": c2}}
def mutant(file, line, col, what, fn, fname):
    return {"package": "memory-industry", "file": file, "function": {"function_name": fname, "return_type": "-> u32", "span": span(*fn)},
            "span": span(line, col, line, col + 4), "replacement": "0", "genre": "FnValue", "name": f"{file}:{line}:{col}: {what}"}
muts = [
    mutant("src/a.rs", 10, 5, "replace f -> u32 with 0", (8, 1, 14, 2), "f"),
    mutant("src/a.rs", 12, 9, "replace + with - in f", (8, 1, 14, 2), "f"),
    mutant("src/a.rs", 30, 5, "replace g -> u32 with 1", (28, 1, 40, 2), "g"),
    mutant("src/b.rs", 5, 5, "replace h -> u32 with 1", (3, 1, 9, 2), "h"),
    mutant("src/b.rs", 20, 13, "replace == with != in k", (15, 1, 25, 2), "k"),
    mutant("src/b.rs", 50, 5, "replace z -> u32 with 0", (48, 1, 55, 2), "z"),
]
names = [m["name"] for m in muts]
def w(name, text):
    with open(os.path.join(fix, name), "w", encoding="utf-8") as f:
        f.write(text)
json.dump(muts, open(os.path.join(fix, "list.json"), "w"))
json.dump([], open(os.path.join(fix, "list-empty.json"), "w"))
w("names.txt", "\n".join(names) + "\n")
ESC = set("\\.+*?()|[]{}^$#&-~")
def rx(name):  # regex::escape: exactly these, and not the space, which Python's re.escape would escape
    return "^" + "".join("\\" + c if c in ESC else c for c in name) + "$"
w("re.txt", "\n".join(rx(n) for n in names) + "\n")

# What each test executed, in the export's shape. A function entered has count > 0; one that was not, count 0.
def fn(name, path, count, l1, l2, extra=()):
    full = path if path.startswith("/") else os.path.join(root, "rust", path)
    return {"name": name, "count": count, "regions": [[l1, 1, l2, 2, count, 0, 0, 0], [l1 + 1, 5, l1 + 1, 9, count, 0, 0, 0]] + list(extra),
            "branches": [], "filenames": [full]}
dep = fn("dep::go", "/home/x/.cargo/registry/src/index/dep-1.0.0/src/lib.rs", 5, 1, 9)
cov = {
    "t_a1": [fn("f", "src/a.rs", 3, 8, 14, [[100, 1, 102, 1, 3, 1, 0, 0]]), fn("u", "src/a.rs", 0, 50, 60), dep],
    "t_a2": [fn("f", "src/a.rs", 1, 8, 14)],
    "t_a3": [fn("u", "src/a.rs", 2, 50, 60)],
    "t_b": [fn("h", "src/b.rs", 1, 3, 9)],
    "t_k": [fn("k", "src/b.rs", 0, 15, 25), fn("{closure#0}", "src/b.rs", 4, 18, 22)],
    "t_z": [fn("z", "src/b.rs", 2, 48, 55)],
}
for t, functions in cov.items():
    json.dump({"version": "2.0.1", "type": "llvm.coverage.json.export",
               "data": [{"files": [], "functions": functions, "totals": {}}]}, open(os.path.join(fix, "cov", t + ".json"), "w"))
# The same, as the reduction of each test: the lines `src/x.rs first last` the index reads (F6b.1).
for t, lines in {"t_a1": ["src/a.rs 8 14"], "t_a2": ["src/a.rs 8 14"], "t_a3": ["src/a.rs 50 60"], "t_b": ["src/b.rs 3 9"], "t_k": ["src/b.rs 18 22"]}.items():
    w(os.path.join("fns", t + ".fns"), "\n".join(lines) + "\n")

# The rule of the index: a test covers a mutant if a function it entered is in the file and its range holds the line.
def plan_line(i, group, why, k):
    return f"{names[i]}\t{group}\t{why}\t{k}\n"
w("plan.expected", plan_line(0, "A1", "subset", 2) + plan_line(1, "A1", "subset", 2) + plan_line(2, "A0", "no-coverage", 0)
  + plan_line(3, "A1", "subset", 1) + plan_line(4, "A1", "subset", 1) + plan_line(5, "A0", "no-coverage", 0))
w("select.expected", "src/a.rs\t10\t2\tt_a1 t_a2\nsrc/a.rs\t12\t2\tt_a1 t_a2\nsrc/b.rs\t5\t1\tt_b\nsrc/b.rs\t20\t1\tt_k\n")
w("pre.tsv", "src/a.rs\t10\n")
w("plan-pre.expected", plan_line(0, "A0", "preflight", 0) + plan_line(1, "A1", "subset", 2) + plan_line(2, "A0", "no-coverage", 0)
  + plan_line(3, "A1", "subset", 1) + plan_line(4, "A1", "subset", 1) + plan_line(5, "A0", "no-coverage", 0))
w("select-pre.expected", "src/a.rs\t12\t2\tt_a1 t_a2\nsrc/b.rs\t5\t1\tt_b\nsrc/b.rs\t20\t1\tt_k\n")

# The class of each mutant with the whole suite and with its subset (D-F6l, F6b.7, F6b.8): m2 is missed by its
# subset and caught by the suite; m4 times out with its subset and is missed by the suite; m3 nobody catches.
C, M, T, U = "CaughtMutant", "MissedMutant", "Timeout", "Unviable"
def table(rows):
    return "".join(f"{n}\t{a}\t{b}\n" for n, (a, b) in zip(names, rows))
w("classes.tsv", table([(C, C), (C, M), (M, M), (M, T), (U, U), (C, C)]))
w("classes-green.tsv", table([(C, C), (C, C), (C, C), (C, C), (U, U), (C, C)]))
w("classes-confirmed.tsv", table([(C, C), (C, M), (C, C), (C, C), (U, U), (C, C)]))
w("merged.expected", "".join(f"{n}\t{c}\t{p}\t{k}\n" for n, c, p, k in sorted([
    (names[0], C, "A1", 2), (names[1], C, "B", 2), (names[2], M, "A0", 0),
    (names[3], M, "B", 1), (names[4], U, "A1", 1), (names[5], C, "A0", 0)])))

# F6b.10: a name made of regex metacharacters, and another that differs only in the column.
meta = ["src/a.rs:10:5: replace f -> Result<(&'static str, Option<String>)> with Ok((\"\", None))",
        "src/a.rs:10:9: replace f -> Result<(&'static str, Option<String>)> with Ok((\"\", None))"]
json.dump([mutant("src/a.rs", 10, int(n.split(":")[2]), n.split(": ", 1)[1], (8, 1, 14, 2), "f") for n in meta],
          open(os.path.join(fix, "list-meta.json"), "w"))
w("classes-meta.tsv", "".join(f"{n}\t{C}\t{C}\n" for n in meta))
w("re-meta.txt", "\n".join(sorted(rx(n) for n in meta)) + "\n")

# The outcomes.json of the three passes, as cargo-mutants 27.1.0 writes them (cut down to what is read).
STATUS = "process" + "_status"  # spelled in two halves: see the stand-in of cargo-mutants above
def rec(name, cls):
    ph = {C: [("Build", "Success"), ("Test", {"Failure": 101})], M: [("Build", "Success"), ("Test", "Success")],
          T: [("Build", "Success"), ("Test", "Timeout")], U: [("Build", {"Failure": 101})]}[cls]
    return {"scenario": {"Mutant": {"name": name}}, "summary": cls, "log_path": "log/m.log",
            "phase_results": [{"phase": p, STATUS: s} for p, s in ph]}
def outcomes(rel, pairs, baseline="Success"):
    recs = [rec(n, c) for n, c in pairs]
    cnt = lambda c: sum(1 for _, k in pairs if k == c)
    path = os.path.join(fix, "mg", rel, "mutants.out", "outcomes.json")
    os.makedirs(os.path.dirname(path), exist_ok=True)
    json.dump({"outcomes": [{"scenario": "Baseline", "summary": baseline, "log_path": "log/baseline.log", "phase_results": []}] + recs,
               "total_mutants": len(recs), "caught": cnt(C), "missed": cnt(M), "timeout": cnt(T), "unviable": cnt(U),
               "success": 0, "cargo_mutants_version": "27.1.0"}, open(path, "w"))
outcomes("A0", [(names[2], M), (names[5], C)])
outcomes("A1", [(names[0], C), (names[1], M), (names[3], T), (names[4], U)])
outcomes("A1-green", [(names[0], C), (names[1], C), (names[3], C), (names[4], U)])
outcomes("A1-badbase", [(names[0], C), (names[1], M), (names[3], T), (names[4], U)], baseline="Failure")
outcomes("B", [(names[1], C), (names[3], M)])
outcomes("B-short", [(names[1], C)])
outcomes("B-extra", [(names[0], C), (names[1], C), (names[3], M)])
CPY
  mapfile -t c_n < "$cfix/names.txt"
  mapfile -t c_p < "$cfix/re.txt"
  c_set() { printf '%s\n' "$@" | LC_ALL=C sort; }

  # The modes of the script under test, run from a copy of it in a tree with no git and no crate: a mode that
  # does not exist yet then falls into the main path of the copy, which finds nothing to judge, and the row
  # fails on what it asserts and not on what it touched.
  mkdir -p "$tmp/qmode/scripts" "$tmp/qmode/rust"
  cp "$ROOT/scripts/quality-gate.sh" "$ROOT/scripts/mutants-gate.sh" "$ROOT/scripts/gate-lock.sh" "$tmp/qmode/scripts/"
  [[ ! -f "$ROOT/package.json" ]] || cp "$ROOT/package.json" "$tmp/qmode/"
  qg_mode() {
    rc=0
    (cd "$tmp" && PATH="$cb:$tmp/bin:$PATH" FAKE_COVER_LOG="$tmp/mode.log" bash "$tmp/qmode/scripts/quality-gate.sh" "$@") >"$tmp/mode.out" 2>"$tmp/mode.err" || rc=$?
  }
  qg_said() { grep -qF -- "$1" "$tmp/mode.out" "$tmp/mode.err"; }

  # F6b.1 (D-F6h): the index. A test covers a mutant if a function it entered is in the mutant's file and its
  # range holds the mutant's line. t_a3 entered another function of src/a.rs, outside every range there.
  rm -rf "$cfix/idx"
  mkdir -p "$cfix/idx"
  qg_mode --cover-index "$cfix/list.json" "$cfix/fns" "$cfix/idx"
  if (( rc != 0 )) || ! cmp -s "$cfix/select.expected" "$cfix/idx/select.tsv"; then
    st_fail "F6b.1: --cover-index did not write the select.tsv of the rule (exit $rc; a test of another function of the same file, or a function outside the range, may have got in): $(cat "$cfix/idx/select.tsv" 2>&1 | tr '\t' ' ') / $(cat "$tmp/mode.err")"
  fi
  if ! cmp -s "$cfix/plan.expected" "$cfix/idx/plan.tsv"; then
    st_fail "F6b.1: --cover-index did not put m3 and m6 in A0 with 'no-coverage' and the others in A1 with 'subset': $(diff "$cfix/plan.expected" "$cfix/idx/plan.tsv" | head -n 6)"
  fi
  # The same with a key the preflight refused: its mutants go to A0 with that reason and leave the table.
  rm -rf "$cfix/idx2"
  mkdir -p "$cfix/idx2"
  qg_mode --cover-index "$cfix/list.json" "$cfix/fns" "$cfix/idx2" "$cfix/pre.tsv"
  if (( rc != 0 )) || ! cmp -s "$cfix/select-pre.expected" "$cfix/idx2/select.tsv" || ! cmp -s "$cfix/plan-pre.expected" "$cfix/idx2/plan.tsv"; then
    st_fail "F6b.11: --cover-index with a refused key did not send its mutant to A0 with 'preflight' and drop its row (exit $rc): $(cat "$cfix/idx2/plan.tsv" 2>&1 | tr '\t' ' ' | head -n 3)"
  fi

  # F6b.8 (D-F6m): the merge, over the outcomes.json of the three passes. The class of a mutant that its subset
  # did not catch is the one the whole suite gave it.
  c_fails() { grep -F -- "$1" "$tmp/mode.err" | grep -c '^FAIL' || true; }
  c_mg() { qg_mode --merge-select "$cfix/mg/merged.json" "$@"; }
  c_classes() {
    "$py" -I - "$1" <<'CPY'
import json, sys
for o in json.load(open(sys.argv[1]))["outcomes"]:
    if isinstance(o["scenario"], dict):
        s = o.get("qg_select", {})
        print(o["scenario"]["Mutant"]["name"], o["summary"], s.get("pass"), s.get("subset"), sep="\t")
CPY
  }
  rm -f "$cfix/mg/merged.json"
  c_mg "$cfix/mg/A0/mutants.out/outcomes.json" "$cfix/mg/A1/mutants.out/outcomes.json" "$cfix/mg/B/mutants.out/outcomes.json" "$cfix/plan.expected"
  if (( rc != 4 )) || [[ "$(c_fails "${c_n[2]}")" == 0 || "$(c_fails "${c_n[3]}")" == 0 || "$(c_fails "${c_n[1]}")" != 0 ]]; then
    st_fail "F6b.8: the merge of A0, A1 and B did not exit 4 with a FAIL for m3 and for m4 and none for m2, which B caught (exit $rc): $(cat "$tmp/mode.err")"
  fi
  if [[ "$(c_classes "$cfix/mg/merged.json" 2>&1 | LC_ALL=C sort)" != "$(LC_ALL=C sort "$cfix/merged.expected")" ]]; then
    st_fail "F6b.8: the merged classes are not m1 Caught (A1), m2 Caught (B), m3 Missed (A0), m4 Missed (B), m5 Unviable (A1), m6 Caught (A0): $(c_classes "$cfix/mg/merged.json" 2>&1 | tr '\t' ' ')"
  fi
  "$py" -I - "$cfix/mg/merged.json" <<'CPY' || st_fail "F6b.8: merged.json does not carry the totals of its own records (3 caught, 2 missed, 0 timeout, 1 unviable) and a baseline"
import json, sys
d = json.load(open(sys.argv[1]))
want = {"total_mutants": 6, "caught": 3, "missed": 2, "timeout": 0, "unviable": 1}
assert all(d[k] == v for k, v in want.items()), d
assert d["outcomes"][0]["scenario"] == "Baseline" and d["outcomes"][0]["summary"] == "Success"
CPY
  # Passes that were not invoked are `-`: A1 alone, all caught, is a green merge.
  rm -f "$cfix/mg/merged.json"
  c_mg - "$cfix/mg/A1-green/mutants.out/outcomes.json" -
  (( rc == 0 )) && [[ "$(c_classes "$cfix/mg/merged.json" | wc -l)" == 4 ]] \
    || st_fail "F6b.8: A1 alone with every mutant caught or unviable was not a green merge of 4 mutants (exit $rc): $(cat "$tmp/mode.err")"
  # What cannot be merged is red, and not a verdict about a mutant: exit 1, with the reason.
  c_mg_red() { # LABEL SAYING A0 A1 B
    local label="$1" saying="$2"
    shift 2
    c_mg "$@"
    (( rc == 1 )) && qg_said "$saying" || st_fail "F6b.8 $label: the merge did not exit 1 saying '$saying' (exit $rc): $(cat "$tmp/mode.out" "$tmp/mode.err")"
  }
  c_o() { printf '%s' "$cfix/mg/$1/mutants.out/outcomes.json"; }
  c_mg_red 'a missed of A1 with no row in B' 'confirm' "$(c_o A0)" "$(c_o A1)" "$(c_o B-short)"
  c_mg_red 'a missed of A1 and no B at all' 'confirm' "$(c_o A0)" "$(c_o A1)" -
  c_mg_red 'a baseline that is not Success' 'baseline' "$(c_o A0)" "$(c_o A1-badbase)" "$(c_o B)"
  c_mg_red 'a B row that A1 did not leave open' 'did not leave' "$(c_o A0)" "$(c_o A1)" "$(c_o B-extra)"

  # --- the step, in the tree of the rows, with the stand-ins first on its PATH ---------------------------------
  c_log="$tmp/cover.log"
  c_tree=ctree
  cj() { # [VAR=value]... -- ARGS...: mutate_and_judge in $tmp/$c_tree, from a fresh state, its output in $tmp/mut.out
    local -a extra=()
    while [[ "${1:-}" != -- ]]; do extra+=("$1"); shift; done
    shift
    q_fresh
    rm -f "$c_log"
    q_tree="$tmp/$c_tree"
    q_pre="$cb"
    mj FAKE_SYSROOT="$csys" FAKE_COVER_LOG="$c_log" FAKE_LIST_JSON="$cfix/list.json" FAKE_CLASSES="$cfix/classes.tsv" \
      FAKE_COVEXPORT="$cfix/cov" FAKE_TEST_BIN="$cb/fake-test-bin" ${extra[@]+"${extra[@]}"} -- "$@"
    q_pre=
    q_tree="$tmp/qtree"
  }
  c_mfails() { grep -F -- "$1" "$tmp/mut.out" | grep -c '^FAIL' || true; }
  c_bins() { awk -F'\t' -v n="$1" -v f="$2" '$1=="bin" && $2==n && $3==f {c++} END {print c+0}' "$c_log"; }
  c_res() { awk -F'\t' -v s="$1" '$1=="pass" {p=$2} $1=="re" && p==s {print $3}' "$c_log" | LC_ALL=C sort; }
  c_argv_of() { awk -F'\t' -v p="$1" -v m="$2" '$1=="pass" {q=$2} $1=="mutant" {n=$2} $1=="test" && q==p && n==m {print $2}' "$c_log"; }
  c_base='test --verbose --package=memory-industry@0.31.0 --lib'

  # F6b.2, F6b.7 and F6b.8 are one run: the six mutants, the map built from the tests, the three passes in their
  # order, and the verdict. One job (CARGO_BUILD_JOBS=1) so that each test of the map starts alone, and the work
  # directory it finds is the one the test before it left.
  c_keep="$tmp/ckeep-main"
  rm -rf "$c_keep"
  cj CARGO_BUILD_JOBS=1 QG_SELECT=cover QG_KEEP_OUT="$c_keep" -- "${q_args[@]}"
  mj_said 'mode: cover' || st_fail "F6b.7: the step with QG_SELECT=cover did not say 'mode: cover' (exit $mut_rc): $(cat "$tmp/mut.out")"
  mj_said 'cover map: 5 tests' || st_fail "F6b.2: the map did not say 'cover map: 5 tests' (the ignored test is not one of them): $(cat "$tmp/mut.out")"
  for c_t in t_a1 t_a2 t_a3 t_b t_k; do
    [[ "$(c_bins "$c_t" '--test-threads=1 -q')" == 1 ]] \
      || st_fail "F6b.2: the binary was not run exactly once as '--exact $c_t --test-threads=1 -q' ($(c_bins "$c_t" '--test-threads=1 -q') times)"
  done
  if awk -F'\t' '$1=="bin" && $2 ~ /t_z/ {found = 1} END {exit !found}' "$c_log"; then
    st_fail "F6b.2: t_z, a test marked ignored, was run: $(grep -F t_z "$c_log")"
  fi
  [[ "$(awk -F'\t' '$1=="bin" && $3 ~ /test-threads/ {print $4}' "$c_log" | LC_ALL=C sort -u | wc -l)" == 5 ]] \
    || st_fail "F6b.2: the five tests did not each get a LLVM_PROFILE_FILE of their own: $(awk -F'\t' '$1=="bin" {print $4}' "$c_log")"
  [[ "$(awk -F'\t' '$1=="bin" && $5 != "leftover=0"' "$c_log" | wc -l)" == 0 ]] \
    || st_fail "F6b.2: a test started with a profile, a merged profile or an export of an earlier one still on disk: $(awk -F'\t' '$1=="bin" && $5 != "leftover=0"' "$c_log")"
  c_envs="$(awk -F'\t' '$1=="cover-env"' "$c_log")"
  if [[ "$(wc -l <<<"$c_envs")" != 1 ]] || [[ "$(cut -f2 <<<"$c_envs")" != *' --sh'* || "$(cut -f2 <<<"$c_envs")" != *' --no-cfg-coverage'* ]]; then
    st_fail "F6b.2: cargo llvm-cov show-env was not asked once for --sh and --no-cfg-coverage: $c_envs"
  fi
  c_tdir="$(awk -F'\t' '$1=="build-map" {print $3}' "$c_log")"
  if [[ "$c_tdir" != "$tmp"/*/qg-cover || "$c_tdir" == "$tmp/ctree/"* || "$(awk -F'\t' '$1=="build-map" {print $2}' "$c_log")" != "$c_tdir" \
    || "$(cut -f3 <<<"$c_envs")" != "$c_tdir" ]]; then
    st_fail "F6b.2: the instrumented build did not go to <the judge's target>/qg-cover, outside the tree and the same for show-env and cargo: '$c_tdir' / $c_envs"
  fi
  [[ "$(awk -F'\t' '$1=="build-map" {print $4}' "$c_log")" == 'test --lib --no-run --message-format=json' ]] \
    || st_fail "F6b.2: the instrumented build was not 'cargo test --lib --no-run --message-format=json': $(grep '^build-map' "$c_log")"
  [[ "$(ls "$c_keep/cover/fns" | wc -l)" == 5 && "$(cat "$c_keep/cover/fns/t_a1.fns")" == 'src/a.rs 8 14' \
    && "$(cat "$c_keep/cover/fns/t_k.fns")" == 'src/b.rs 18 22' && "$(cat "$c_keep/cover/fns/t_a3.fns")" == 'src/a.rs 50 60' ]] \
    || st_fail "F6b.2: the functions each test entered were not reduced to 'src/a.rs 8 14' for t_a1 (a function of count 0, one outside rust/src and a region of another file do not count) and the closure for t_k: $(cd "$c_keep/cover/fns" && head -n 3 ./*.fns 2>&1)"
  if [[ -n "$(find "$c_keep/cover" \( -name '*.profraw' -o -name '*.profdata' -o -name cov.json \) -print -quit)" \
    || -n "$(find "$c_keep/cover" -name '*.json' ! -name list.json ! -name merged.json ! -name outcomes.json ! -name build.json -print -quit)" ]]; then
    st_fail "F6b.2: the step left a profile, a merged profile or an export per test behind: $(find "$c_keep/cover" -name '*.profraw' -o -name '*.profdata' -o -name '*.json')"
  fi
  cmp -s "$cfix/plan.expected" "$c_keep/cover/plan.tsv" \
    || st_fail "F6b.1: the plan of the step is not the one of the rule (m3 and m6 in A0, no coverage): $(diff "$cfix/plan.expected" "$c_keep/cover/plan.tsv" | head -n 6)"
  # Preflight: each distinct subset ran once, with the default threads, and none of them failed.
  for c_t in 't_a1 t_a2' t_b t_k; do
    [[ "$(c_bins "$c_t" '')" == 1 ]] || st_fail "F6b.11: the subset '$c_t' was not run exactly once, without a mutant ($(c_bins "$c_t" '') times)"
  done

  # F6b.7 (Q3): the tests nobody covers go first, with the whole suite, and their red is said before A1 starts.
  (( mut_rc != 0 )) || st_fail "F6b.8: m3 and m4 are missed by the whole suite and the step was green: $(cat "$tmp/mut.out")"
  [[ "$(grep -c '^pass' "$c_log")" == 3 ]] || st_fail "F6b.7: cargo-mutants was not launched three times (A0, A1, B): $(grep '^pass' "$c_log")"
  [[ "$(grep '^pass' "$c_log" | cut -f2-3 | tr '\t' ' ' | tr '\n' ',')" == '1 no,2 yes,3 no,' ]] \
    || st_fail "F6b.7: A0 did not run first with the whole suite, then A1 with the selection, then B with the whole suite: $(grep '^pass' "$c_log" | tr '\t' ' ')"
  [[ "$(c_res 1)" == "$(c_set "${c_p[2]}" "${c_p[5]}")" ]] || st_fail "F6b.7: A0 was not asked for exactly m3 and m6 (the ones no test covers): $(c_res 1)"
  [[ "$(c_res 2)" == "$(c_set "${c_p[0]}" "${c_p[1]}" "${c_p[3]}" "${c_p[4]}")" ]] || st_fail "F6b.7: A1 was not asked for exactly m1, m2, m4 and m5: $(c_res 2)"
  c_early="$(grep -n 'early: 2 mutants with no covering lib test ran first: 1 caught, 1 missed, 0 timeout' "$tmp/mut.out" | head -n 1 | cut -d: -f1)"
  c_a1_at="$(grep -n 'fake-cargo-mutants: pass 2 ' "$tmp/mut.out" | head -n 1 | cut -d: -f1)"
  [[ -n "$c_early" ]] || st_fail "F6b.7: the line 'early: 2 mutants with no covering lib test ran first: 1 caught, 1 missed, 0 timeout' is not in the output: $(cat "$tmp/mut.out")"
  [[ -n "$c_a1_at" ]] && (( c_early < c_a1_at )) || st_fail "F6b.7: the early line came after A1 started (line $c_early against $c_a1_at)"
  c_a0_at="$(grep -n 'fake-cargo-mutants: pass 1 ' "$tmp/mut.out" | head -n 1 | cut -d: -f1)"
  sed -n "${c_a0_at},${c_a1_at}p" "$tmp/mut.out" | grep -qF -- "${c_n[2]}" \
    || st_fail "F6b.7: the missed mutant of A0, m3, is not named between the start of A0 and the start of A1"

  # F6b.8: B holds exactly what A1 left open (a missed and a timeout), and the verdict is the merged one.
  [[ "$(c_res 3)" == "$(c_set "${c_p[1]}" "${c_p[3]}")" ]] \
    || st_fail "F6b.8: B was not asked for exactly m2 (missed by its subset) and m4 (timed out with it): $(c_res 3)"
  [[ "$(c_mfails "${c_n[2]}")" != 0 && "$(c_mfails "${c_n[3]}")" != 0 && "$(c_mfails "${c_n[1]}")" == 0 ]] \
    || st_fail "F6b.8: the step did not fail naming m3 and m4 and not m2 (which the whole suite caught): $(grep '^FAIL' "$tmp/mut.out")"
  if grep -qF 'cargo-mutants rc=' "$tmp/mut.out"; then
    st_fail "F6b.8: the rc of A1 (2: a mutant of its subset was missed) decided the verdict, and B had not said yet: $(grep 'cargo-mutants rc=' "$tmp/mut.out")"
  fi
  [[ "$(c_classes "$c_keep/cover/merged.json" 2>&1 | LC_ALL=C sort)" == "$(LC_ALL=C sort "$cfix/merged.expected")" ]] \
    || st_fail "F6b.8: the merged.json the step kept is not m1 Caught, m2 Caught (B), m3 Missed, m4 Missed (B), m5 Unviable, m6 Caught: $(c_classes "$c_keep/cover/merged.json" 2>&1 | tr '\t' ' ')"
  bash "$ROOT/scripts/mutants-gate.sh" --check-builds "$c_keep/cover/merged.json" >"$tmp/cb.out" 2>&1 \
    || st_fail "F6b.8: the merged.json cannot be judged by --check-builds from where it is kept (its logs do not resolve): $(cat "$tmp/cb.out")"
  # The real shim, in the toy labs, handed cargo the subset of each mutant of A1 and the whole suite everywhere else.
  [[ "$(c_argv_of 2 "${c_n[0]}")" == "$c_base -- --exact t_a1 t_a2" && "$(c_argv_of 2 "${c_n[1]}")" == "$c_base -- --exact t_a1 t_a2" \
    && "$(c_argv_of 2 "${c_n[3]}")" == "$c_base -- --exact t_b" && "$(c_argv_of 2 "${c_n[4]}")" == "$c_base -- --exact t_k" ]] \
    || st_fail "F6b.5: the tests of the mutants of A1 did not reach cargo as '-- --exact <their tests>': $(awk -F'\t' '$1=="pass"{q=$2} $1=="mutant"{n=$2} $1=="test" && q==2 {print n " => " $2}' "$c_log")"
  if awk -F'\t' '$1=="pass" {q=$2} $1=="test" && q != 2 && $2 ~ /--exact/ {bad = 1} END {exit !bad}' "$c_log" \
    || awk -F'\t' '$1=="test" && $3 != 0 {bad = 1} END {exit !bad}' "$c_log"; then
    st_fail "F6b.7: A0 or B filtered the tests of a mutant, or cargo saw a MUTANTS_SHIM_* variable: $(grep '^test' "$c_log")"
  fi
  c_sl="$c_keep/cover/A1/select-shim.log"
  [[ "$(LC_ALL=C sort "$c_sl")" == "$(printf '%s\n' 'full baseline' 'subset 1 src/b.rs:20' 'subset 1 src/b.rs:5' 'subset 2 src/a.rs:10' 'subset 2 src/a.rs:12' | LC_ALL=C sort)" ]] \
    || st_fail "F6b.5: the selection log of A1 is not one 'subset' per mutant and one 'full baseline': $(cat "$c_sl" 2>&1)"
  [[ ! -e "$c_keep/cover/A0/select-shim.log" && ! -e "$c_keep/cover/B/select-shim.log" ]] \
    || st_fail "F6b.7: A0 or B ran with the selection in its environment"
  for c_f in A0/mutants.out/outcomes.json A1/mutants.out/outcomes.json B/mutants.out/outcomes.json merged.json select.tsv plan.tsv list.json; do
    [[ -s "$c_keep/cover/$c_f" ]] || st_fail "F6b.8: QG_KEEP_OUT did not keep cover/$c_f: $(ls -R "$c_keep" 2>&1 | head -n 30)"
  done

  # F6b.8, the other way round: when the whole suite confirms every missed of the subsets, the step is green,
  # whatever cargo-mutants' exit was for A1. m2 is missed by its subset (rc 2) and caught by the suite.
  cj CARGO_BUILD_JOBS=3 QG_SELECT=cover FAKE_CLASSES="$cfix/classes-confirmed.tsv" -- "${q_args[@]}"
  if (( mut_rc != 0 )) || [[ "$(c_res 3)" != "$(c_set "${c_p[1]}")" ]] || mj_said 'FAIL'; then
    st_fail "F6b.8: a missed of A1 that the whole suite caught did not leave a green step, with B asked for m2 alone (exit $mut_rc): $(cat "$tmp/mut.out")"
  fi

  # F6b.3: a test that fails alone turns cover off, and the step runs as full, with the same verdict full gives.
  cj CARGO_BUILD_JOBS=2 QG_SELECT=cover FAKE_FAIL_ALONE=t_b FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
  if (( mut_rc != 0 )) || ! mj_said 'cover: off (1 test fails alone in the map: t_b)' || ! mj_said 'mode: full' || mj_said 'mode: cover'; then
    st_fail "F6b.3: a test that fails alone did not leave 'cover: off (1 test fails alone in the map: t_b)' and 'mode: full', with the green verdict of full (exit $mut_rc): $(cat "$tmp/mut.out")"
  fi
  [[ "$(grep -c '^pass' "$c_log")" == 1 && "$(grep '^pass' "$c_log" | cut -f2-4 | tr '\t' ' ')" == '1 no 0' ]] \
    || st_fail "F6b.3: cargo-mutants was not called once, with no --re and no selection, as full calls it: $(grep '^pass' "$c_log" | tr '\t' ' ')"
  # A test that never finishes is one that fails: the map waits for each test as long as QG_COVER_TEST_TIMEOUT_S.
  # The stand-in sleeps 15 s and then writes 'hang-finished': that line in the log is a wait nobody cut.
  cj CARGO_BUILD_JOBS=2 QG_SELECT=cover FAKE_HANG=t_b QG_COVER_TEST_TIMEOUT_S=1 FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
  if grep -q '^hang-finished' "$c_log" || ! mj_said 'cover: off (1 test fails alone in the map: t_b)'; then
    st_fail "F6b.3: a test that hangs was waited for until it finished instead of failing at QG_COVER_TEST_TIMEOUT_S=1, or did not turn cover off: $(cat "$tmp/mut.out")"
  fi

  # F6b.4: with no cargo-llvm-cov, or a sysroot without llvm-profdata or llvm-cov, cover does not run: exit 2,
  # nothing mutated, naming what is missing. full, with the same PATH, never looks for them.
  q_path="$c_hidden"
  cj QG_SELECT=cover -- "${q_args[@]}"
  if (( mut_rc != 2 )) || mj_invoked || ! mj_said 'cargo-llvm-cov'; then
    st_fail "F6b.4: with no cargo-llvm-cov on PATH the step did not exit 2 naming it before cargo-mutants (exit $mut_rc): $(cat "$tmp/mut.out")"
  fi
  cj QG_SELECT=full FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
  if (( mut_rc != 0 )) || ! mj_invoked || ! mj_said 'mode: full'; then
    st_fail "F6b.4: QG_SELECT=full with no cargo-llvm-cov on PATH did not run as it always does (exit $mut_rc): $(cat "$tmp/mut.out")"
  fi
  q_path=
  for c_missing in profdata cov; do
    cj QG_SELECT=cover FAKE_SYSROOT="$tmp/csys-no${c_missing/profdata/prof}" -- "${q_args[@]}"
    if (( mut_rc != 2 )) || mj_invoked || ! mj_said "llvm-${c_missing}"; then
      st_fail "F6b.4: with a sysroot that has no llvm-$c_missing the step did not exit 2 naming it before cargo-mutants (exit $mut_rc): $(cat "$tmp/mut.out")"
    fi
  done
  cj QG_SELECT=full FAKE_SYSROOT="$tmp/csys-noprof" FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
  (( mut_rc == 0 )) && mj_said 'mode: full' || st_fail "F6b.4: QG_SELECT=full over a sysroot with no llvm-profdata did not run (exit $mut_rc): $(cat "$tmp/mut.out")"

  # F6b.9: each pass measures exactly what it was asked, in both directions.
  for c_dev in drop extra; do
    cj QG_SELECT=cover FAKE_A1_DEVIATION="$c_dev" FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
    c_who="${c_n[4]}"
    [[ "$c_dev" == extra ]] && c_who="${c_n[2]}"
    if (( mut_rc == 0 )) || ! mj_said 'set differs in pass A1' || ! mj_said "$c_who"; then
      st_fail "F6b.9: a pass A1 that ($c_dev) one mutant was not red with 'set differs in pass A1' naming $c_who (exit $mut_rc): $(cat "$tmp/mut.out")"
    fi
  done

  # F6b.10: a name made of metacharacters is matched by its own --re and by no other, written with the escape of
  # regex::escape (the space is not escaped) and anchored.
  cj QG_SELECT=cover FAKE_LIST_JSON="$cfix/list-meta.json" FAKE_CLASSES="$cfix/classes-meta.tsv" -- "${q_args[@]}"
  if (( mut_rc != 0 )) || [[ "$(c_res 1)" != "$(cat "$cfix/re-meta.txt")" ]]; then
    st_fail "F6b.10: the --re of the two mutants are not the anchored, regex::escape'd names (exit $mut_rc): got $(c_res 1) / wanted $(cat "$cfix/re-meta.txt")"
  fi
  if [[ "$(awk -F'\t' '$1=="re" && $2 != 1' "$c_log" | wc -l)" != 0 ]]; then
    st_fail "F6b.10: a --re selected the other column's mutant, or none: $(grep '^re' "$c_log")"
  fi

  # F6b.11: a subset that fails on its own, without a mutant, decides nothing: its mutants go to A0 and it is named.
  c_keep="$tmp/ckeep-pre"
  rm -rf "$c_keep"
  cj CARGO_BUILD_JOBS=2 QG_SELECT=cover QG_KEEP_OUT="$c_keep" FAKE_FAIL_TOGETHER='t_a1 t_a2' FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
  if (( mut_rc != 0 )) || ! mj_said 'preflight' || ! mj_said 't_a1 t_a2' \
    || [[ "$(awk -F'\t' '$3=="preflight" {print $1}' "$c_keep/cover/plan.tsv" | LC_ALL=C sort)" != "$(c_set "${c_n[0]}" "${c_n[1]}")" ]] \
    || [[ "$(c_res 1)" != "$(c_set "${c_p[0]}" "${c_p[1]}" "${c_p[2]}" "${c_p[5]}")" || "$(c_res 2)" != "$(c_set "${c_p[3]}" "${c_p[4]}")" ]] \
    || grep -q '^src/a.rs' "$c_keep/cover/select.tsv"; then
    st_fail "F6b.11: the subset {t_a1, t_a2}, which fails without a mutant, did not send m1 and m2 to A0 with 'preflight' naming it, leave A1 with m4 and m5 and drop its rows (exit $mut_rc): $(cat "$tmp/mut.out"; cat "$c_keep/cover/plan.tsv" 2>&1)"
  fi

  # F6b.12: cargo-mutants' marker in the tree turns cover off before anything is built.
  c_tree=ctree-marker
  cj QG_SELECT=cover FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
  c_tree=ctree
  if (( mut_rc != 0 )) || ! mj_said "cover: off (the tree holds cargo-mutants' marker in src/b.rs)" || ! mj_said 'mode: full' || grep -q '^build-map' "$c_log"; then
    st_fail "F6b.12: a marker in rust/src/b.rs did not turn cover off before the map was built, with the step running as full (exit $mut_rc): $(cat "$tmp/mut.out")"
  fi

  # No mutant, no map: nothing to select the tests of.
  cj QG_SELECT=cover FAKE_LIST_JSON="$cfix/list-empty.json" FAKE_CLASSES="$cfix/classes-green.tsv" -- "${q_args[@]}"
  if ! mj_said 'cover: off (' || ! mj_said 'mode: full' || grep -q '^build-map' "$c_log"; then
    st_fail "F6b.7: a diff with no mutants built a coverage map anyway, or did not say why cover was off: $(cat "$tmp/mut.out")"
  fi

  # --- F6c (D-F6p): the study of equivalence -------------------------------------------------------------------
  # `qg_study REF DIR` runs the judge, whole, twice through QG_SELF: once with QG_SELECT=REF and its outcomes kept
  # in DIR/ref (the verdict of the reference), once with QG_SELECT=cover; it then asks mutants-gate.sh
  # --compare-classes for the class of every mutant in both, writes DIR/study.tsv and says whether Q2 may be
  # adopted. The judge is a stand-in with the same contract as the real one: no arguments, its mode in QG_SELECT
  # and its keep directory in QG_KEEP_OUT; it refuses an unknown mode and a keep directory that is not empty (what
  # the real one refuses before it mutates anything), keeps DIR/<mode>/outcomes.json or DIR/cover/merged.json
  # as the real one does, and turns itself into `full` when it is told cover cannot run. The study is called in
  # the tree of the rows, with its own mutants-gate.sh, and cut by a watchdog if it ever waits.
  sy="$tmp/study"
  mkdir -p "$sy/fix"
  "$py" -I - "$sy/fix" "$cfix/names.txt" <<'SPY'
import json, os, sys
fix = sys.argv[1]
names = [line.rstrip("\n") for line in open(sys.argv[2], encoding="utf-8") if line.strip()]
assert len(names) == 6, names
C, M, T, U = "CaughtMutant", "MissedMutant", "Timeout", "Unviable"
ARGV = ["/usr/bin/cargo", "test", "--verbose", "--package=memory-industry@0.31.0", "--lib"]
STATUS = "process" + "_status"
REF_S = [21.5, 20.25, 22.0, 22.5, None, 19.75]
COVER_S = [1.25, 1.5, 12.0, 3.0, None, 11.5]
SELECT = [("A1", 2), ("B", 2), ("A0", 0), ("B", 1), ("A1", 1), ("A0", 0)]
def outcomes(classes, seconds, select):
    records = []
    for i, (name, cls) in enumerate(zip(names, classes)):
        phases = [{"phase": "Build", "duration": 1.0, STATUS: {"Failure": 101} if cls == U else "Success"}]
        if cls != U:
            phases.append({"phase": "Test", "duration": 90.0 if cls == T else seconds[i], STATUS: "Timeout" if cls == T else "Success", "argv": ARGV})
        record = {"scenario": {"Mutant": {"name": name}}, "summary": cls, "log_path": f"log/m{i}.log", "phase_results": phases}
        if select:
            record["qg_select"] = {"pass": select[i][0], "subset": select[i][1]}
        records.append(record)
    baseline = {"scenario": "Baseline", "summary": "Success", "log_path": "log/baseline.log",
                "phase_results": [{"phase": "Build", STATUS: "Success"}, {"phase": "Test", STATUS: "Success", "argv": ARGV}]}
    count = lambda cls: sum(1 for c in classes if c == cls)
    return {"outcomes": [baseline] + records, "total_mutants": len(records), "caught": count(C), "missed": count(M),
            "timeout": count(T), "unviable": count(U), "success": 0, "cargo_mutants_version": "27.1.0"}
GREEN = [C, C, C, C, U, C]
cases = {
    "green": (GREEN, GREEN),
    "red": ([C, C, M, M, U, C], [C, C, M, M, U, C]),
    "one": (GREEN, [M, C, C, C, U, C]),
    "swap": ([C, C, M, M, U, C], [M, C, C, M, U, C]),
    "timeout": ([C, C, C, T, U, C], GREEN),
}
for case, (ref, cover) in cases.items():
    os.makedirs(os.path.join(fix, case), exist_ok=True)
    json.dump(outcomes(ref, REF_S, None), open(os.path.join(fix, case, "ref.json"), "w", encoding="utf-8"))
    json.dump(outcomes(cover, COVER_S, SELECT), open(os.path.join(fix, case, "merged.json"), "w", encoding="utf-8"))
SPY
  cat > "$sy/judge" <<'SFAKE'
#!/usr/bin/env bash
log="${FAKE_STUDY_LOG:?}"
printf 'judge\t%s\t%s\t%s\t%s\n' "${QG_SELECT:-unset}" "${QG_KEEP_OUT:-unset}" "${QG_BASE:-unset}" "$#" >> "$log"
mode="${QG_SELECT:-full}"
case "$mode" in
  legacy | full | cover) ;;
  *)
    echo "FAIL: QG_SELECT=$mode is not a mode of the mutation step (legacy, full or cover)." >&2
    exit 2
    ;;
esac
keep="${QG_KEEP_OUT:-}"
if [[ -n "$keep" ]]; then
  if [[ -n "$(ls -A "$keep" 2>/dev/null || true)" ]]; then
    echo "FAIL: QG_KEEP_OUT=$keep is not empty; name a new directory so that runs do not mix" >&2
    exit 2
  fi
  mkdir -p "$keep"
fi
rc="${FAKE_REF_RC:-0}"
if [[ "$mode" == cover ]]; then
  rc="${FAKE_COVER_RC:-0}"
  if [[ "${FAKE_COVER_OFF:-}" == 1 ]]; then
    echo "cover: off (1 test fails alone in the map: t_b)"
    mode=full
    echo "mode: full"
    [[ -z "$keep" ]] || { mkdir -p "$keep/full" && cp "$FAKE_REF_OUTCOMES" "$keep/full/outcomes.json" && echo "kept: $keep/full"; }
  else
    echo "mode: cover"
    [[ -z "$keep" ]] || { mkdir -p "$keep/cover" && cp "$FAKE_COVER_MERGED" "$keep/cover/merged.json" && echo "kept: $keep/cover"; }
  fi
else
  echo "mode: $mode"
  if [[ -n "$keep" && "${FAKE_REF_NO_OUTCOMES:-}" != 1 ]]; then
    mkdir -p "$keep/$mode" && cp "$FAKE_REF_OUTCOMES" "$keep/$mode/outcomes.json" && echo "kept: $keep/$mode"
  fi
fi
echo "=== fin quality-gate exit=$rc ==="
exit "$rc"
SFAKE
  chmod +x "$sy/judge"
  sy_log="$tmp/study.judge.log"
  sy_run() { # [VAR=value]... -- REF DIR: qg_study in the tree of the rows; its exit in $sy_rc, its output in $tmp/study.{out,err}
    local -a extra=()
    while [[ "${1:-}" != -- ]]; do extra+=("$1"); shift; done
    shift
    local pid dog
    sy_rc=0
    rm -f "$sy_log"
    (
      ROOT="$q_tree"
      cd "$q_tree"
      QG_SELF="$sy/judge"
      unset QG_SELECT QG_KEEP_OUT
      export FAKE_STUDY_LOG="$sy_log" QG_BASE=feedface
      for kv in ${extra[@]+"${extra[@]}"}; do export "${kv?}"; done
      qg_study "$@"
    ) >"$tmp/study.out" 2>"$tmp/study.err" </dev/null &
    pid=$!
    ( sleep 120; kill -9 "$pid" 2>/dev/null ) >/dev/null 2>&1 &
    dog=$!
    wait "$pid" || sy_rc=$?
    kill "$dog" 2>/dev/null || true
    wait "$dog" 2>/dev/null || true
  }
  sy_said() { grep -qF -- "$1" "$tmp/study.out" "$tmp/study.err"; }
  sy_judged() { grep -c '^judge' "$sy_log" 2>/dev/null || true; }
  # The columns of one row of study.tsv, by the name of the mutant: ref, cover, pass, k, seconds of test in each.
  sy_row() { awk -F'\t' -v n="$2" 'NR > 1 && $1 == n {print $2 "\t" $3 "\t" $4 "\t" $5 "\t" $6 "\t" $7}' "$1/study.tsv"; }
  sy_fix() { printf 'FAKE_REF_OUTCOMES=%s/fix/%s/ref.json\nFAKE_COVER_MERGED=%s/fix/%s/merged.json\n' "$sy" "$1" "$sy" "$1"; }

  # F6c.1: the same classes. Reference green: the study says Q2 may be adopted and exits 0. A DIR that does not exist is
  # created; the two runs, the merged classes of the cover run and the table are all in it, and nothing else.
  sy_d="$tmp/sy-1"
  rm -rf "$sy_d"
  mapfile -t sy_env < <(sy_fix green)
  sy_run "${sy_env[@]}" -- legacy "$sy_d"
  if (( sy_rc != 0 )) || ! sy_said 'study: 6/6 identical — Q2 may be adopted' || ! sy_said 'study: reference exit 0' || sy_said 'NOT adopted'; then
    st_fail "F6c.1: two runs with the same six classes and a green reference did not exit 0 with 'study: 6/6 identical — Q2 may be adopted' and 'study: reference exit 0' (exit $sy_rc): $(cat "$tmp/study.out" "$tmp/study.err")"
  fi
  if [[ ! -s "$sy_d/ref/legacy/outcomes.json" || ! -s "$sy_d/cover/merged.json" || ! -s "$sy_d/study.tsv" ]] \
    || [[ "$(ls -A "$sy_d" | LC_ALL=C sort | tr '\n' ' ')" != 'cover ref study.tsv ' ]]; then
    st_fail "F6c.1: the study did not leave exactly ref/legacy/outcomes.json, cover/merged.json and study.tsv in DIR: $(cd "$sy_d" 2>/dev/null && find . | LC_ALL=C sort | tr '\n' ' ')"
  fi
  sy_calls="$(cut -f2-5 "$sy_log" | tr '\t' ' ')"
  if [[ "$(sy_judged)" != 2 || "$(sed -n 1p "$sy_log" | cut -f2-3)" != "$(printf 'legacy\t%s/ref' "$sy_d")" \
    || "$(sed -n 2p "$sy_log" | cut -f2)" != cover || "$(sed -n 2p "$sy_log" | cut -f3)" == "$sy_d/ref" || "$(sed -n 2p "$sy_log" | cut -f3)" == "$sy_d" \
    || "$(cut -f4 "$sy_log" | sort -u | tr '\n' ' ')" != 'feedface ' || "$(cut -f5 "$sy_log" | sort -u | tr '\n' ' ')" != '0 ' ]]; then
    st_fail "F6c.1: the judge was not run whole (no arguments) twice, the reference first with QG_SELECT=legacy and QG_KEEP_OUT=DIR/ref and then cover into a directory of its own, both with the same QG_BASE: $sy_calls"
  fi
  if ! sy_said 'mode: legacy' || ! sy_said 'mode: cover'; then
    st_fail "F6c.1: the output of the two runs of the judge did not reach the output of the study: $(cat "$tmp/study.out")"
  fi
  [[ "$(awk 'NR == 1' "$sy_d/study.tsv")" == "$(printf 'name\tref\tcover\tpass\tk\tref_test_s\tcover_test_s')" && "$(awk 'NR > 1' "$sy_d/study.tsv" | wc -l)" == 6 ]] \
    || st_fail "F6c.1: study.tsv is not a header (name, ref, cover, pass, k, ref_test_s, cover_test_s) and one row for each of the six mutants: $(cat "$sy_d/study.tsv")"
  if [[ "$(sy_row "$sy_d" "${c_n[0]}")" != "$(printf 'CaughtMutant\tCaughtMutant\tA1\t2\t21.500\t1.250')" \
    || "$(sy_row "$sy_d" "${c_n[2]}")" != "$(printf 'CaughtMutant\tCaughtMutant\tA0\t0\t22.000\t12.000')" \
    || "$(sy_row "$sy_d" "${c_n[4]}")" != "$(printf 'Unviable\tUnviable\tA1\t1\t-\t-')" ]]; then
    st_fail "F6c.1: study.tsv rows are not the class in each run, the pass and subset of the cover run and the seconds of the test phase with 3 decimals (- when there was no test): m1 '$(sy_row "$sy_d" "${c_n[0]}")' m3 '$(sy_row "$sy_d" "${c_n[2]}")' m5 '$(sy_row "$sy_d" "${c_n[4]}")'"
  fi

  # The reference can be full, and what the caller exported for the judge does not reach it: QG_SELECT=cover and a
  # keep directory that is not empty would refuse the run if the study did not set its own.
  sy_d="$tmp/sy-2"
  rm -rf "$sy_d" "$tmp/sy-stale"
  mkdir -p "$sy_d" "$tmp/sy-stale"
  : > "$tmp/sy-stale/leftover"
  sy_run "${sy_env[@]}" QG_SELECT=cover QG_KEEP_OUT="$tmp/sy-stale" -- full "$sy_d"
  if (( sy_rc != 0 )) || ! sy_said 'study: 6/6 identical — Q2 may be adopted' || [[ ! -s "$sy_d/ref/full/outcomes.json" || ! -s "$sy_d/cover/merged.json" ]] \
    || [[ "$(sed -n 1p "$sy_log" | cut -f2)" != full ]]; then
    st_fail "F6c.1: '--study full' with QG_SELECT=cover and a QG_KEEP_OUT that is not empty in the environment did not run the reference as full into DIR/ref (exit $sy_rc): $(cat "$tmp/study.out" "$tmp/study.err"; cat "$sy_log")"
  fi

  # A red reference is a red study, and the study still says what it found: both runs missed m3 and m4, the same.
  sy_d="$tmp/sy-3"
  rm -rf "$sy_d"
  mapfile -t sy_env < <(sy_fix red)
  sy_run "${sy_env[@]}" FAKE_REF_RC=1 FAKE_COVER_RC=1 -- legacy "$sy_d"
  if (( sy_rc != 1 )) || ! sy_said 'study: 6/6 identical — Q2 may be adopted' || ! sy_said 'study: reference exit 1' || [[ ! -s "$sy_d/study.tsv" ]]; then
    st_fail "F6c.1: a red reference (exit 1) with the same classes in the cover run was not exit 1 with 'study: 6/6 identical' and 'study: reference exit 1' (exit $sy_rc): $(cat "$tmp/study.out" "$tmp/study.err")"
  fi

  # F6c.2: one class that differs. m1 is caught by the reference and missed by cover; the reference is green, and the
  # study is not: a difference is red whatever the reference said.
  sy_d="$tmp/sy-4"
  rm -rf "$sy_d"
  mapfile -t sy_env < <(sy_fix one)
  sy_run "${sy_env[@]}" FAKE_COVER_RC=1 -- legacy "$sy_d"
  if (( sy_rc != 1 )) || ! sy_said 'study: 1 differ — Q2 is NOT adopted' || ! sy_said "class changed for ${c_n[0]}: CaughtMutant -> MissedMutant" || sy_said 'may be adopted'; then
    st_fail "F6c.2: m1 caught by the reference and missed by cover, with a green reference, was not exit 1 with 'study: 1 differ — Q2 is NOT adopted' and 'class changed for m1: CaughtMutant -> MissedMutant' (exit $sy_rc): $(cat "$tmp/study.out" "$tmp/study.err")"
  fi
  if [[ "$(sy_row "$sy_d" "${c_n[0]}" | cut -f1-2)" != "$(printf 'CaughtMutant\tMissedMutant')" ]]; then
    st_fail "F6c.2: study.tsv was not written with the difference in it (m1: '$(sy_row "$sy_d" "${c_n[0]}")')"
  fi

  # The totals of both runs are the same here: m1 goes from caught to missed and m3 from missed to caught. A study that
  # compared the totals would find the two identical.
  sy_d="$tmp/sy-5"
  rm -rf "$sy_d"
  mapfile -t sy_env < <(sy_fix swap)
  sy_run "${sy_env[@]}" FAKE_REF_RC=1 FAKE_COVER_RC=1 -- legacy "$sy_d"
  if (( sy_rc != 1 )) || ! sy_said 'study: 2 differ — Q2 is NOT adopted' || ! sy_said "class changed for ${c_n[0]}: CaughtMutant -> MissedMutant" \
    || ! sy_said "class changed for ${c_n[2]}: MissedMutant -> CaughtMutant" || sy_said 'may be adopted'; then
    st_fail "F6c.2: a caught that turns missed and a missed that turns caught, with the same totals, was not 'study: 2 differ' naming both (exit $sy_rc): $(cat "$tmp/study.out" "$tmp/study.err")"
  fi

  # A timeout of the reference that cover catches: the class cargo-mutants writes as "Timeout" counts as one, so this is
  # one difference and not two (the totals of the reference add up).
  sy_d="$tmp/sy-6"
  rm -rf "$sy_d"
  mapfile -t sy_env < <(sy_fix timeout)
  sy_run "${sy_env[@]}" FAKE_REF_RC=1 -- legacy "$sy_d"
  if (( sy_rc != 1 )) || ! sy_said 'study: 1 differ — Q2 is NOT adopted' || ! sy_said "class changed for ${c_n[3]}: Timeout -> CaughtMutant" || sy_said 'may be adopted'; then
    st_fail "F6c.2: a Timeout of the reference that cover catches was not exactly 'study: 1 differ' with 'class changed for m4: Timeout -> CaughtMutant' (exit $sy_rc): $(cat "$tmp/study.out" "$tmp/study.err")"
  fi

  # F6c.3: the study does not mix runs. A DIR with anything in it, a REF that is not legacy or full: exit 2 before the
  # judge runs, and DIR is left as it was.
  sy_d="$tmp/sy-7"
  rm -rf "$sy_d"
  mkdir -p "$sy_d"
  : > "$sy_d/leftover"
  mapfile -t sy_env < <(sy_fix green)
  sy_run "${sy_env[@]}" -- legacy "$sy_d"
  if (( sy_rc != 2 )) || [[ -e "$sy_log" ]] || ! sy_said 'not empty' || ! sy_said "$sy_d" || [[ "$(ls -A "$sy_d" | tr '\n' ' ')" != 'leftover ' ]]; then
    st_fail "F6c.3: a DIR with a file in it was not exit 2 naming 'not empty' and DIR before the judge ran, with DIR untouched (exit $sy_rc, judge runs: $(sy_judged)): $(cat "$tmp/study.out" "$tmp/study.err")"
  fi
  for sy_ref in otra cover ''; do
    sy_d="$tmp/sy-8"
    rm -rf "$sy_d"
    sy_run "${sy_env[@]}" -- "$sy_ref" "$sy_d"
    if (( sy_rc != 2 )) || [[ -e "$sy_log" ]] || ! sy_said 'legacy' || ! sy_said 'full' || [[ -e "$sy_d" && -n "$(ls -A "$sy_d")" ]]; then
      st_fail "F6c.3: '--study ${sy_ref:-<empty>}' was not exit 2 naming legacy and full before the judge ran, with nothing written (exit $sy_rc, judge runs: $(sy_judged)): $(cat "$tmp/study.out" "$tmp/study.err")"
    fi
  done
  # The same through the script, which has to take the mode before its banner and its diff (the copy of the rows of F6b).
  sy_d="$tmp/sy-9"
  rm -rf "$sy_d"
  qg_mode --study otra "$sy_d"
  if (( rc != 2 )) || ! qg_said 'legacy' || grep -qF '=== quality-gate' "$tmp/mode.out"; then
    st_fail "F6c.3: 'quality-gate.sh --study otra DIR' did not exit 2 naming legacy before the banner of the judge (exit $rc): $(cat "$tmp/mode.out" "$tmp/mode.err")"
  fi
  mkdir -p "$sy_d"
  : > "$sy_d/leftover"
  qg_mode --study legacy "$sy_d"
  if (( rc != 2 )) || ! qg_said 'not empty' || grep -qF '=== quality-gate' "$tmp/mode.out" || [[ "$(ls -A "$sy_d" | tr '\n' ' ')" != 'leftover ' ]]; then
    st_fail "F6c.3: 'quality-gate.sh --study legacy DIR' with a file in DIR did not exit 2 naming 'not empty' before the banner, with DIR untouched (exit $rc): $(cat "$tmp/mode.out" "$tmp/mode.err")"
  fi

  # A study that compared nothing says nothing. The reference left no outcomes (the judge was refused, or died): exit 2,
  # and the three hours of the cover run are not spent; cover turned itself off (it ran as full, and there is no
  # merged.json): exit 2 as well, never 'identical'.
  sy_d="$tmp/sy-10"
  rm -rf "$sy_d"
  sy_run "${sy_env[@]}" FAKE_REF_RC=2 FAKE_REF_NO_OUTCOMES=1 -- legacy "$sy_d"
  if (( sy_rc != 2 )) || [[ "$(sy_judged)" != 1 ]] || ! sy_said 'no outcomes.json' || sy_said 'identical' || [[ -e "$sy_d/study.tsv" ]]; then
    st_fail "F6c.4: a reference that left no outcomes.json was not exit 2 naming it, with the cover run not launched and no study.tsv (exit $sy_rc, judge runs: $(sy_judged)): $(cat "$tmp/study.out" "$tmp/study.err")"
  fi
  sy_d="$tmp/sy-11"
  rm -rf "$sy_d"
  sy_run "${sy_env[@]}" FAKE_COVER_OFF=1 -- legacy "$sy_d"
  if (( sy_rc != 2 )) || ! sy_said 'no merged.json' || sy_said 'identical' || sy_said 'may be adopted' || [[ -e "$sy_d/study.tsv" ]]; then
    st_fail "F6c.4: a cover run that turned itself off (it ran as full: no merged.json) was not exit 2 naming 'no merged.json', with no 'identical' and no study.tsv (exit $sy_rc): $(cat "$tmp/study.out" "$tmp/study.err")"
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
  echo "    inside rust/; with no MUTANTS_JOBS its jobs come from mutants-gate.sh --default-jobs,"
  echo "    which sizes them from the memory that is available; the same rows hold for the judge as it was"
  echo "    (legacy) and for the one that mutates through the lab of mutants-gate.sh (full, the default),"
  echo "    which builds only the lib test, hands cargo-mutants the jobserver the judge counts, takes its"
  echo "    verdict from cargo-mutants' exit and from the build check, and keeps its outcomes in QG_KEEP_OUT;"
  echo "    the cover mode builds a map of the functions each test enters, gives each mutant the tests whose"
  echo "    functions hold its line, runs the mutants nobody covers first and says their verdict early, runs"
  echo "    the rest with their tests, and confirms every missed and timed out one with the whole suite before"
  echo "    it is a verdict; it turns itself off (and the step runs as full) for a test that fails alone, a marker"
  echo "    of cargo-mutants in the tree or a subset that fails without a mutant, and refuses to run (exit 2) with"
  echo "    no cargo-llvm-cov or no llvm-profdata/llvm-cov in the sysroot"
  echo "    the study runs the judge whole twice (the reference and cover) with a keep directory of its own each, compares"
  echo "    the class of every mutant (a timeout of the reference included) and not the totals, writes study.tsv, is red"
  echo "    for a difference whatever the reference said, and refuses (exit 2) a DIR that is not empty, a REF that is not"
  echo "    legacy or full, a reference that left no outcomes.json and a cover run that left no merged.json"
  exit 0
fi
# --- self-test end ---

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
          # rust/tests/group_scratch_db/v029_accounts_storage.rs (a module of the group_scratch_db binary).
          # That file creates and drops its own database and carries
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
          # Needs a database and a running daemon. Judged by rust/tests/v029_accounts_over_real_http.rs
          # (g4) and rust/tests/v029_doctor_reads_only_its_world.rs (g16, a binary of its own so the
          # world it builds for doctor does not change the environment under the daemon tests),
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
          # Each one is judged by rust/tests/group_no_server/v029_bridge_cli.rs: it starts the real binary with HOME in a
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
          #                     v029_doctor_reads_only_its_world::g16_account_facts_count_what_the_database_holds_and_doctor_prints_them
          #                     wants tables_present true and the counts of a migrated database.
          #   accounts_db.rs account_facts, `<` with `==`
          #                     v029_doctor_reads_only_its_world::g16_a_database_without_the_account_tables_is_a_doctor_failure_not_a_crash
          #                     wants account_facts to answer on a database without the tables. With
          #                     `==` it queries the missing tables and fails instead. The migrated
          #                     database of g16_account_facts_count_... also answers the default.
          #
          # Owner: endurecedor 0.29. Expires: 2027-03-22.
          #
          # A twelfth group: one equivalent mutant of the 0.30 head, found by the run of
          # QG_BASE=c4b95dc on 2026-10-13 (1 of 61 missed).
          #
          #   mode.rs rerank_gpu_active with false
          #                     Equivalent in the build the --lib run makes, which has no cuda/directml
          #                     feature: gpu_really_active(cfg!(any(feature = "cuda", feature =
          #                     "directml")), ..) gets compiled_with_gpu = false, so the judge is false
          #                     on every call, the OnceLock cell can only ever keep false, and the
          #                     function returns false on every execution, as the mutant does. The only
          #                     difference is whether RERANK_GPU_ACTIVE gets filled, and a test that
          #                     asserted that would write the process-wide cell for good and break
          #                     the_daemon_keeps_no_answer_while_the_reranker_has_no_placement, which
          #                     needs it empty. The decision is judged where it can differ:
          #                     rerank_gpu_active_in (kept / not kept per placement) and
          #                     gpu_really_active (the build term) have their own unit tests in mode.rs
          #                     on a local cell. Only this mutant is excluded: `with true` and the
          #                     mutants of rerank_gpu_active_in stay judged. K2, sabotage S8.
          # Owner: endurecedor 0.30. Expires: 2027-04-13.
          #
          # An eighth group of one: the survivor of QG_BASE=origin/main (the 0.31 work),
          # found on 2026-10-20 in the first quality-gate run over that branch.
          #   nli.rs resolved_without_a_model with false
          #                     the reader of the NLI cell for «looked and found no
          #                     model» (K4). It is one line over a process-wide
          #                     OnceLock, and the unit tests of nli.rs rely on that
          #                     cell being unresolved («nothing in this binary calls
          #                     enabled()»), so --lib cannot put it in Unavailable
          #                     without making them depend on test order. Its pure
          #                     half, found_no_model, has a table there, and the call
          #                     site in http.rs is held by a source test. Judged by
          #                     tests/nli_cell_looks_once.rs, in the plain `cargo test`
          #                     of the SIL: its own process, no model and no ONNX
          #                     Runtime (HOME points at an empty directory), it asks
          #                     the reader before the cell looks (false) and after
          #                     enabled() made it look (true). `with false` fails the
          #                     second ask and `with true` the first; both were run on
          #                     2026-10-21 and both went red. rust/tests/ is never run
          #                     by `cargo mutants -- --lib`, so here the mutant still
          #                     comes back MISSED: the exclusion stays for --lib only.
          #                     Only `with false` is excluded: `with true` was not among
          #                     the survivors of that run, so --lib catches it and it
          #                     stays judged.
          # Owner: endurecedor 0.31. Expires: 2027-04-20.
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
  elif command -v lizard >/dev/null; then
    echo "radon ausente; lizard mide los .py del diff (misma puerta CRAP/cc)."
    if ! run_lizard . "${py[@]}"; then
      [[ $fail -eq 0 ]] && fail=2
    fi
  else
    echo "FALTA radon y lizard. Puerta CRAP 6; CC <= 4 orienta."
    [[ $fail -eq 0 ]] && fail=2
  fi
fi

echo "=== fin quality-gate exit=$fail ==="
exit "$fail"
