use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the product builds SQL at run time and says so with `AssertSqlSafe`, and how many times.
/// A new one is a new line here, written by whoever has read where its dynamic part comes from.
const SQL_BUILT_AT_RUN_TIME: [(&str, usize); 7] = [
    ("db.rs", 4),
    ("dedupe_cli.rs", 1),
    ("handlers/calibrar.rs", 1),
    ("handlers/proyecto.rs", 1),
    ("handlers/sync.rs", 9),
    ("project.rs", 2),
    ("protocol.rs", 2),
];

const CALL: &str = "AssertSqlSafe(";
const REASON: &str = "// sql-safe:";

fn rust_files_under(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("src/ is readable")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files_under(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn every_sql_built_at_run_time_is_asserted_safe_counted_and_says_why() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files_under(&src, &mut files);
    assert!(
        files.len() >= 100,
        "the scan read {} .rs files under src/ and there are about 140; a scan that read almost \
         none proves nothing",
        files.len()
    );

    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    let mut without_reason: Vec<String> = Vec::new();
    for path in &files {
        let name = path
            .strip_prefix(&src)
            .expect("under src/")
            .to_string_lossy()
            .replace('\\', "/");
        let body = std::fs::read_to_string(path).expect("a source file is readable");
        let lines: Vec<&str> = body.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") || !line.contains(CALL) {
                continue;
            }
            *found.entry(name.clone()).or_default() += line.matches(CALL).count();
            let reason = lines[..index]
                .iter()
                .rev()
                .map(|line| line.trim())
                .find(|line| !line.is_empty())
                .and_then(|line| line.strip_prefix(REASON))
                .is_some_and(|rest| !rest.trim().is_empty());
            if !reason {
                without_reason.push(format!(
                    "src/{name}:{}: the line before it is not `{REASON} <why the dynamic part \
                     cannot come from outside>`",
                    index + 1
                ));
            }
        }
    }

    let expected: BTreeMap<String, usize> = SQL_BUILT_AT_RUN_TIME
        .iter()
        .map(|(name, count)| (name.to_string(), *count))
        .collect();
    let miscounted: Vec<String> = expected
        .keys()
        .chain(found.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let (want, have) = (
                expected.get(name).copied().unwrap_or(0),
                found.get(name).copied().unwrap_or(0),
            );
            (want != have)
                .then(|| format!("src/{name}: the list says {want} `{CALL}`, the file has {have}"))
        })
        .collect();

    assert!(
        miscounted.is_empty() && without_reason.is_empty(),
        "sqlx 0.9 takes only a `&'static str` or an `{CALL}...)` as SQL. Every place that builds \
         SQL at run time wraps it, says with `{REASON}` on the line above why its dynamic part \
         is the code's own and never the caller's, and is counted in SQL_BUILT_AT_RUN_TIME so \
         that a new one is a decision somebody wrote down.\ncount differs: {miscounted:#?}\nno \
         reason: {without_reason:#?}"
    );
}
