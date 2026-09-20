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
