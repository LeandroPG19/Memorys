use std::collections::HashSet;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

fn ci_yaml() -> String {
    let path = repo_root().join(".github/workflows/ci.yml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn test_file_stems() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .map(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .expect("test file has a utf8 name")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

fn bash_array(yaml: &str, name: &str) -> Vec<String> {
    let marker = format!("{name}=(");
    let start = yaml
        .find(&marker)
        .unwrap_or_else(|| panic!("ci.yml must declare `{marker}...)`"));
    let body = &yaml[start + marker.len()..];
    let end = body
        .find(')')
        .unwrap_or_else(|| panic!("`{marker}` is opened in ci.yml but never closed"));
    body[..end].split_whitespace().map(str::to_string).collect()
}

fn literal_test_names(yaml: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = yaml;
    while let Some(pos) = rest.find("--test ") {
        let after = &rest[pos + "--test ".len()..];
        rest = after;
        if after.starts_with('"') || after.starts_with('$') {
            continue;
        }
        let end = after
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\\' || c == ')')
            .unwrap_or(after.len());
        names.push(after[..end].to_string());
    }
    names
}

/// The events under the workflow's top-level `on:`, in either YAML form: the
/// inline `on: [push, pull_request]` / `on: push`, or the block with one key per
/// event. Only the keys at the first indentation of the block are events; what
/// sits deeper (`branches:`, `inputs:`) belongs to them.
fn triggers(yaml: &str) -> Vec<String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let start = lines
        .iter()
        .position(|l| {
            let head = l.trim_end();
            ["on:", "\"on\":", "'on':"]
                .iter()
                .any(|key| head.starts_with(key))
        })
        .expect("ci.yml must have a top-level `on:`");
    let inline = lines[start]
        .split_once(':')
        .map(|(_, rest)| rest.split('#').next().unwrap_or("").trim())
        .unwrap_or("");
    if !inline.is_empty() {
        return inline
            .trim_matches(|c| c == '[' || c == ']')
            .split(',')
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
            .collect();
    }
    let mut events = Vec::new();
    let mut indent: Option<usize> = None;
    for line in &lines[start + 1..] {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let depth = line.len() - trimmed.len();
        if depth == 0 {
            break;
        }
        let first = *indent.get_or_insert(depth);
        if depth == first {
            let key = trimmed.split(':').next().unwrap_or("").trim();
            events.push(key.to_string());
        }
    }
    events
}

/// GitHub Actions runs this workflow when somebody asks it to, and never on its
/// own. It used to run on every push and pull request to main, which made its
/// badge read like a merge verdict; AGENTS.md says the badge is not one, and
/// publish.yml used to consult it before releasing. The judge is
/// ./scripts/merge-gate.sh on the merge machine.
#[test]
fn ci_yml_runs_only_when_dispatched_by_hand() {
    let events = triggers(&ci_yaml());
    assert!(
        !events.is_empty(),
        "no event could be read out of ci.yml's `on:`. A scan that found nothing would pass \
         the assertion below for any workflow at all"
    );
    assert_eq!(
        events,
        vec!["workflow_dispatch".to_string()],
        "ci.yml is triggered by {events:?}. It runs only by hand: a run on push or pull_request \
         turns its badge back into something that looks like the merge judge, and the judge is \
         ./scripts/merge-gate.sh, locally"
    );
}

#[test]
fn the_trigger_reader_sees_push_in_both_yaml_forms() {
    let block =
        "name: CI\n\non:\n  push:\n    branches: [main]\n  workflow_dispatch:\n\njobs:\n  a:\n";
    assert_eq!(triggers(block), vec!["push", "workflow_dispatch"]);
    assert_eq!(
        triggers("on: [push, workflow_dispatch]\njobs:\n"),
        vec!["push", "workflow_dispatch"]
    );
    assert_eq!(triggers("on: push\njobs:\n"), vec!["push"]);
}

#[test]
fn github_actions_declares_it_is_not_the_merge_judge() {
    let yaml = ci_yaml();
    assert!(
        yaml.contains("This workflow is not mergeable"),
        "ci.yml must say out loud that a green badge is not mergeable — the judge is merge-gate.sh"
    );
    assert!(
        yaml.contains("merge-gate.sh"),
        "ci.yml must name the local SIL so a reader is not sent to the badge"
    );
}

#[test]
fn the_integration_step_discovers_tests_by_glob_not_by_a_hand_written_list() {
    let yaml = ci_yaml();
    assert!(
        yaml.contains("for file in tests/*.rs"),
        "the DB integration step in ci.yml must discover rust/tests/*.rs by glob. It used \
         to enumerate one --test flag per file by hand, and that list drifted to 25 of 62 \
         files: the other 37 never ran while the job reported green over the commits that \
         added them. A glob is what makes a newly added test file run without anyone \
         remembering to edit this file"
    );
}

/// Why each file in `MODEL_OR_CLI_ONLY` is out of the GitHub job, one sentence
/// each, and what runs it instead.
///
/// The list and the reasons move together on purpose. An exclusion list is how
/// coverage leaves a workflow without anybody deciding to let it: one name
/// appended in a hurry and that file stops running on every push, with the job
/// still green over the commit that stopped it — which is exactly what
/// happened to the `--test` flags this discovery loop replaced. Pairing each
/// name with the thing the runner does not have makes growing the list an
/// edit somebody has to mean.
const MODEL_OR_CLI_ONLY_REASONS: [(&str, &str); 7] = [
    (
        "v016_chunking",
        "hard-asserts embeddings::onnx::is_model_loaded(); the runner has no ONNX embedder \
         (ONNX_MODEL_PATH is unset there). Locally: require_present \"tests that need the \
         embedding model\" in run-all-tests.sh",
    ),
    (
        "v016_extract_without_sampling",
        "hard-asserts judge::resolve_offline_llm().is_some(); the runner has no generative \
         LLM and no authenticated claude/gemini. Locally: require_generative_llm",
    ),
    (
        "v017_relation_scan",
        "the relation scan drives a local LLM subprocess and hard-asserts the same \
         resolve_offline_llm(). Locally: require_generative_llm",
    ),
    (
        "v017_rerank_gpu",
        "two halves and the runner can do neither: the ignored tests hard-assert a reranker \
         model on disk, and the rest run --release --features cuda (387s in debug) on a \
         runner that has no model and no CUDA. Locally both halves run: an unguarded \
         --release --features cuda call for the placement contracts, which need nothing on \
         disk, and require_present \"reranker tests\" against $CUBA_RERANKER_PATH/model.onnx \
         for the two that load the model",
    ),
    (
        "nli_entailment",
        "hard-asserts nli::available() && nli::enabled(); the runner has no NLI model. It \
         used to self-skip through nli::available() and now refuses to, because the local \
         gate forbids a soft skip. Locally: require_present \"tests that need the NLI model\"",
    ),
    (
        "nli_cost",
        "same NLI model hard-assert; it is a measurement of cost by premise length, which \
         says nothing at all without the model. Locally: the same require_present",
    ),
    (
        "nli_probe",
        "same NLI model hard-assert, and it reads tokenizer.json out of the model directory \
         directly. Locally: the same require_present",
    ),
];

/// Growing the exclusion list has to be a deliberate edit, and the reason has
/// to arrive with the name.
///
/// Two-sided on purpose, as everything with a baseline in this repo is: this
/// fails when the list grows AND when it shrinks without the table being
/// updated, because a name silently dropped from the workflow while its reason
/// stays here is the same drift pointing the other way.
#[test]
fn the_exclusion_list_grows_only_by_a_deliberate_edit_that_says_why() {
    let yaml = ci_yaml();
    let excluded = bash_array(&yaml, "MODEL_OR_CLI_ONLY");

    assert!(
        !excluded.is_empty(),
        "MODEL_OR_CLI_ONLY parsed as empty. A green result from a scan that found nothing \
         proves nothing, and every assertion below would be vacuous"
    );

    let reason_for = |name: &str| -> Option<&'static str> {
        MODEL_OR_CLI_ONLY_REASONS
            .iter()
            .find(|entry| entry.0 == name)
            .map(|entry| entry.1)
    };

    let paste_ready: String = excluded
        .iter()
        .map(|name| {
            let reason = reason_for(name.as_str())
                .unwrap_or("WHAT does the runner not have that this file hard-asserts?");
            format!("    (\n        \"{name}\",\n        \"{reason}\",\n    ),\n")
        })
        .collect();
    let paste_ready = format!(
        "const MODEL_OR_CLI_ONLY_REASONS: [(&str, &str); {}] = [\n{paste_ready}];",
        excluded.len()
    );

    assert_eq!(
        excluded.len(),
        MODEL_OR_CLI_ONLY_REASONS.len(),
        "ci.yml excludes {} test file(s) from the GitHub job and this table explains {}. \
         Paste this over MODEL_OR_CLI_ONLY_REASONS in rust/tests/ci_contract.rs and write \
         the missing sentence(s):\n\n{paste_ready}\n",
        excluded.len(),
        MODEL_OR_CLI_ONLY_REASONS.len()
    );

    for name in &excluded {
        let reason = reason_for(name.as_str()).unwrap_or_else(|| {
            panic!(
                "ci.yml excludes `{name}` from the GitHub job and nothing here says what the \
                 runner is missing. Add it with its reason:\n\n{paste_ready}\n"
            )
        });
        assert!(
            reason.len() > 30,
            "`{name}` is excluded with `{reason}` beside it. A placeholder is the same list \
             with extra ceremony: say which model or CLI the runner does not have, and name \
             the guard in run-all-tests.sh that runs the file locally instead"
        );
    }

    for entry in MODEL_OR_CLI_ONLY_REASONS {
        assert!(
            excluded.iter().any(|actual| actual.as_str() == entry.0),
            "this table explains why `{}` is out of the GitHub job, and ci.yml no longer \
             excludes it. Either it runs there now — delete the entry in the same edit — or \
             somebody dropped it from the list and the reason outlived the fact",
            entry.0
        );
    }
}

#[test]
fn every_excluded_test_file_actually_exists() {
    let yaml = ci_yaml();
    let files: HashSet<String> = test_file_stems().into_iter().collect();

    let model_or_cli_only = bash_array(&yaml, "MODEL_OR_CLI_ONLY");
    let role0 = bash_array(&yaml, "ROLE0");

    for name in model_or_cli_only.iter().chain(role0.iter()) {
        assert!(
            files.contains(name),
            "ci.yml names `{name}` in an exclusion list, but rust/tests/{name}.rs does not \
             exist. A stale entry does not hide a file that runs somewhere else — it hides \
             the fact that nobody checked what is actually on disk"
        );
    }
}

#[test]
fn no_test_file_is_named_with_a_bare_dash_dash_test_outside_the_role0_step() {
    let yaml = ci_yaml();
    let role0: HashSet<String> = bash_array(&yaml, "ROLE0").into_iter().collect();

    let offenders: Vec<String> = literal_test_names(&yaml)
        .into_iter()
        .filter(|name| !role0.contains(name))
        .collect();

    assert!(
        offenders.is_empty(),
        "these names are wired in with a literal --test flag outside the CUBA_APP_ROLE=0 \
         step: {offenders:?}. That is exactly the shape of the bug this contract exists to \
         catch — a file hand-listed once and never touched again while tests/ kept growing. \
         New tests must be picked up by the tests/*.rs glob, not appended to a list"
    );
}

#[test]
fn tests_that_need_a_second_node_get_one_from_the_workflow() {
    let yaml = ci_yaml();
    let needing: Vec<String> =
        std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests"))
            .expect("tests/ is readable")
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
            .filter(|e| e.file_name() != std::ffi::OsStr::new("ci_contract.rs"))
            .filter(|e| {
                std::fs::read_to_string(e.path())
                    .unwrap_or_default()
                    .contains("CUBA_PEER_DATABASE_URL")
            })
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

    assert!(
        !needing.is_empty(),
        "the scan found no test asking for a second node, and there are three. A green result \
         from a scan that found nothing proves nothing"
    );
    assert!(
        yaml.lines().any(|l| {
            let t = l.trim();
            t.starts_with("CUBA_PEER_DATABASE_URL:") && t.contains("postgres")
        }),
        "these files .expect() a second node and refuse to skip without one, because a two-node \
         test that quietly passes on a single node proves nothing: {needing:?}. The local gate \
         provisions that database and exports the variable; when this workflow switched to \
         discovering tests/*.rs by glob it inherited the files without the environment the gate \
         builds around them, which is nine failures on every push. Whoever removes the variable \
         from ci.yml has to remove these files from discovery in the same edit"
    );
}
