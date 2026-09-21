//! `packaging/` is generated, and this is what keeps it that way.
//!
//! The defect: `packaging/cuba-memorys.service` was written by hand, pinned
//! `CUBA_GPU_MEM_LIMIT_MB=2048` at line 67, and `resources::set_if_absent` lets
//! an already-set variable win — so a number typed once beat the planner's
//! measurement on every machine with a card. The same file still carries a
//! database password and points at a repository that does not exist.
//!
//! These contracts need the repository tree, which is why they are here and not
//! in the library. The ones that have to kill mutants live in
//! `rust/src/service/tests.rs`, because the gate runs `cargo mutants -- --lib`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use memory_industry::resources::{self, Plan, Tier};
use memory_industry::service::{self, Profile, Target, Unit};

/// AGENTS.md: the `cuba-memorys` names survive one release. These two units
/// carry `CUBA_GPU_MEM_LIMIT_MB=2048` (:67) and a `DATABASE_URL` with a
/// password (:90) — they are the defect 0.27 is killing, and they go out with
/// the name they were born with.
///
/// Exempted BY NAME, with a reason and an expiry. Scanning only what the binary
/// generates would exempt by omission instead: a `packaging/install.ps1` added
/// by hand next month would be invisible to both hygiene contracts, which is
/// literally how `packaging/` came to hold a 2048 in the first place.
const LEGACY_PACKAGING: [&str; 2] = ["cuba-memorys.service", "cuba-memorys.socket"];
const LEGACY_DROPPED_IN: &str = "0.28.0";

/// Printed by every failure that means "the tree and the binary disagree".
const REGENERATE: &str = "memory-industry setup service --print --linux  --out packaging\n\
                          memory-industry setup service --print --windows --out packaging";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate sits under the repository root")
        .to_path_buf()
}

fn packaging() -> PathBuf {
    repo_root().join("packaging")
}

/// Windows checkouts may store CRLF; every comparison below is on `\n`, the
/// same normalisation `rust/tests/doc_contract.rs` does.
///
/// It also decodes UTF-16LE, because the task XML ships that way — `schtasks`
/// refuses any other encoding — and `read_to_string` would fail on it. Decoding
/// here rather than at each call site keeps every scan below about content.
/// The encoding itself is not this function's business and cannot be checked
/// through it: that is `the_versioned_task_xml_is_utf16_the_way_windows_reads_it`.
fn read_normalised(path: &Path) -> String {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let (pairs, odd) = bytes[2..].as_chunks::<2>();
        // Dropping the leftover byte silently would turn a corrupt file into a
        // content mismatch: the comparison downstream would report a differing
        // last character instead of saying the file is not UTF-16 any more.
        // Reachable here and not in the library's reader, because these are
        // files git, editors and any tool without `working-tree-encoding`
        // support have touched — the exact risk the .gitattributes note names.
        assert!(
            odd.is_empty(),
            "{} has an odd number of bytes after the BOM, so it is not valid UTF-16. Something \
             that does not understand the encoding rewrote it",
            path.display()
        );
        let units: Vec<u16> = pairs.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
        String::from_utf16(&units)
            .unwrap_or_else(|e| panic!("{} is not valid UTF-16: {e}", path.display()))
    } else {
        String::from_utf8(bytes)
            .unwrap_or_else(|e| panic!("{} is not valid UTF-8: {e}", path.display()))
    };
    text.replace("\r\n", "\n")
}

/// Every file under `packaging/`, at any depth, as (name, relative path, body).
fn every_packaging_file() -> Vec<(String, String, String)> {
    let root = packaging();
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("packaging/ is readable ({}): {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .file_name()
                .expect("a file has a name")
                .to_string_lossy()
                .into_owned();
            let relative = path
                .strip_prefix(&root)
                .expect("under packaging/")
                .to_string_lossy()
                .replace('\\', "/");
            out.push((name, relative, read_normalised(&path)));
        }
    }
    out.sort();
    out
}

fn without_the_legacy(files: Vec<(String, String, String)>) -> Vec<(String, String, String)> {
    files
        .into_iter()
        .filter(|(name, _, _)| !LEGACY_PACKAGING.contains(&name.as_str()))
        .collect()
}

/// The canonical render, for both targets, as (relative path, body).
fn golden_render(target: Target) -> Vec<(String, String)> {
    let unit = Unit::documented(Profile::Loopback, target);
    // The launcher sits beside the binary in the canonical install root, which
    // is exactly where `--out` puts it.
    let launcher = unit.exe.with_extension("cmd");
    service::rendered_files(&unit, target.is_windows(), &launcher)
}

/// The keys `resources::plan_env` can emit, computed here from the planner
/// itself rather than from `service`, so this contract stays an independent
/// opinion: the union over a plan with everything on and one with everything
/// off.
fn keys_the_planner_can_emit() -> BTreeSet<String> {
    let base = Plan {
        tier: Tier::Full,
        embedder: true,
        reranker: true,
        reranker_on_gpu: true,
        nli: true,
        embed_intra_threads: 4,
        rerank_intra_threads: 2,
        nli_intra_threads: 2,
        rerank_chunk: 16,
        gpu_mem_limit_mb: Some(5388),
        gpu_mem_floor_mb: Some(2938),
        worker_threads: 4,
        max_blocking_threads: 8,
        db_max_connections: 10,
        ood_fit_limit: 500,
        budget_mb: 16384,
        committed_mb: 4096,
    };
    let everything_off = Plan {
        tier: Tier::Minimal,
        embedder: false,
        reranker: false,
        reranker_on_gpu: false,
        nli: false,
        gpu_mem_limit_mb: None,
        gpu_mem_floor_mb: None,
        ..base.clone()
    };

    [base, everything_off]
        .iter()
        .flat_map(|p| resources::plan_env(p))
        .map(|(key, _)| key.to_string())
        .collect()
}

/// The names an env file offers, commented or not.
fn keys_offered(body: &str) -> BTreeSet<String> {
    body.lines()
        .map(|l| l.trim_start().trim_start_matches('#').trim())
        .filter_map(|l| l.split('=').next())
        .filter(|w| {
            !w.is_empty()
                && w.starts_with(|c: char| c.is_ascii_uppercase())
                && w.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
        .map(str::to_string)
        .collect()
}

fn first_difference(expected: &str, found: &str) -> String {
    for (n, (want, got)) in expected.lines().zip(found.lines()).enumerate() {
        if want != got {
            return format!("line {}:\n  esperado: {want}\n  encontrado: {got}", n + 1);
        }
    }
    format!(
        "the files agree line by line for {} lines and then one of them ends: expected {} \
         line(s), found {}",
        expected.lines().count().min(found.lines().count()),
        expected.lines().count(),
        found.lines().count()
    )
}

// --- 1 · The tree is what the binary renders --------------------------------

#[test]
fn the_versioned_units_are_what_the_binary_renders() {
    let root = packaging();
    let mut checked = 0usize;

    for target in [Target::Linux, Target::Windows] {
        for (relative, body) in golden_render(target) {
            let path = root.join(&relative);
            assert!(
                path.exists(),
                "packaging/{relative} is not in the tree. The installation living outside the \
                 repository is the thing this slice exists to end — regenerate it:\n{REGENERATE}"
            );

            let golden = read_normalised(&path);
            let rendered = body.replace("\r\n", "\n");
            assert_eq!(
                golden,
                rendered,
                "packaging/{relative} is not what the binary renders.\n{}\n\nRegenerate it — \
                 and note that the test does NOT do this for you, on purpose: a test that \
                 rewrites its own expectation goes green after an `rm` and two runs, and then \
                 it is the diff, not the gate, that has to catch a hand-edited \
                 unit.\n{REGENERATE}",
                first_difference(&golden, &rendered)
            );
            checked += 1;
        }
    }

    // Anchor before absence: a render that produced nothing would satisfy every
    // loop above without comparing a single byte.
    assert!(
        checked >= 6,
        "only {checked} file(s) were compared; the two targets produce three each"
    );
}

// --- 2 · The golden cannot rot away from the live render --------------------

#[test]
fn the_golden_env_file_offers_every_key_the_live_render_sets() {
    let planner = keys_the_planner_can_emit();

    // Anchor: if the planner's key set came back empty this would pass while
    // proving nothing at all.
    assert!(
        planner.len() >= 7,
        "the planner emits {} key(s), which means this scan is broken and not the file: \
         {planner:?}",
        planner.len()
    );

    for relative in [
        "memory-industry.env.example",
        "windows/memory-industry.env.example",
    ] {
        let path = packaging().join(relative);
        assert!(
            path.exists(),
            "packaging/{relative} is missing:\n{REGENERATE}"
        );

        let offered = keys_offered(&read_normalised(&path));
        let missing: Vec<&String> = planner.difference(&offered).collect();
        assert!(
            missing.is_empty(),
            "packaging/{relative} never mentions {missing:?}. The values are allowed to \
             differ from any one machine — the SHAPE is not. Without this the golden is a \
             second source of truth for the defaults, which is exactly what put a \
             hand-written 2048 in a unit file.\n{REGENERATE}"
        );
    }
}

// --- 3 · The installer says no at install time ------------------------------

#[test]
fn a_lan_profile_refuses_to_render_without_a_token() {
    let routable = "192.168.0.10:8787";

    let refused = Unit::from_env(Profile::Lan, Target::Linux, Some(routable), Some("corto"))
        .expect_err("a five-character token on a routable address must not render");
    let said = refused.to_string();
    assert!(
        said.contains("32"),
        "the refusal has to name the length the daemon demands, or the operator tries a \
         slightly longer guess: {said}"
    );

    // With no `--token` at all the installer generates one rather than leaving
    // the daemon open: the refusal is for a token that is too weak, not for the
    // operator having failed to invent one.
    let generated = Unit::from_env(Profile::Lan, Target::Linux, Some(routable), None)
        .expect("no --token means the installer generates one");
    let token = generated
        .token
        .as_deref()
        .expect("a routable install always ends up with a token");
    assert_eq!(
        service::refuse_unsafe(Profile::Lan, &generated.addr, Some(token)),
        None,
        "the installer generated a token its own check rejects"
    );
}

// --- 4 · Windows: the chain from the task to the binary ---------------------

#[test]
fn the_windows_task_names_the_binary_that_exists() {
    let unit = Unit::from_env(Profile::Loopback, Target::Windows, None, None)
        .expect("a loopback windows render needs nothing from the operator");

    let root = std::env::temp_dir().join(format!("mi-packaging-{}", uuid::Uuid::new_v4()));
    let written =
        service::write_all(&unit, Target::Windows, &root).expect("writes into a scratch root");

    let cmd_path = written
        .iter()
        .map(|(path, _)| path)
        .find(|path| path.extension().is_some_and(|e| e == "cmd"))
        .expect("the windows render writes a .cmd launcher")
        .clone();
    let xml_path = written
        .iter()
        .map(|(path, _)| path)
        .find(|path| path.extension().is_some_and(|e| e == "xml"))
        .expect("the windows render writes a task XML")
        .clone();

    let xml = read_normalised(&xml_path);
    let commanded = xml
        .split("<Command>")
        .nth(1)
        .and_then(|rest| rest.split("</Command>").next())
        .expect("the task declares a <Command>")
        .trim()
        .trim_matches('"')
        .to_string();

    assert_eq!(
        commanded,
        cmd_path.display().to_string(),
        "the task's <Command> is not the launcher that was just written. On Linux a bad \
         ExecStart is a 203/EXEC in the journal; on Windows nothing reports it — the task \
         'runs' and the only symptom is a daemon that never appears"
    );
    assert!(cmd_path.exists(), "{} was not written", cmd_path.display());

    // Asserted against `unit.exe` rather than by picking a quoted chunk out of
    // the file: under `cargo test` `current_exe()` is this test's own binary, so
    // any selector keyed on the product name is true in production and false
    // here — which would make the assertion fire on a launcher that is fine.
    let launcher = read_normalised(&cmd_path);
    let exe = unit.exe.display().to_string();
    assert!(
        launcher.contains(&format!("\"{exe}\"")),
        "the launcher does not run the quoted binary this render names:\n{launcher}"
    );
    assert!(
        unit.exe.exists(),
        "the launcher starts {exe}, which is not on disk"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// --- 5 and 6 · packaging/ does not hide the defect again --------------------

/// Why this line is a secret, if it is.
fn secret_in(line: &str) -> Option<&'static str> {
    if let Some((_, after)) = line.split_once("://")
        && let Some((authority, _)) = after.split_once('@')
        && authority.contains(':')
    {
        return Some("a connection URL with a password in it");
    }
    if looks_like_an_api_key(line) {
        return Some("an API key");
    }
    if let Some((_, value)) = line.split_once("CUBA_HTTP_TOKEN=")
        && !value.trim().is_empty()
    {
        return Some("a bearer token");
    }
    None
}

/// An `sk-` key, in the shape vendors actually issue them.
///
/// Two conditions, and both earn their place.
///
/// The run after `sk-` has to allow `-` and `_`, because what is emitted today
/// is `sk-proj-…` and `sk-ant-api03-…`. A run of plain alphanumerics stops at
/// the first dash and counts `proj` — four — so the detector knew only the old
/// shape and would have reported "no secrets" over a file holding a current
/// key. It took the presence anchor to catch that: the scan and the shell
/// command used to double-check it were two copies of the same wrong rule,
/// agreeing with each other about finding nothing.
///
/// And `sk-` only counts at the start of a token, or `task-scheduler` and
/// `disk-space` become API keys — words this repository writes on almost every
/// page about Windows.
fn looks_like_an_api_key(line: &str) -> bool {
    line.match_indices("sk-").any(|(at, _)| {
        let starts_a_token = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        starts_a_token
            && line[at + 3..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .count()
                >= 8
    })
}

/// Whether this line pins the GPU ceiling to a number by hand — commented or
/// not. `#` is one keystroke away from gone, and B1 computes this ceiling from
/// the free VRAM: any number written here is either redundant or the bug back.
fn pins_a_gpu_ceiling(line: &str) -> bool {
    line.split_once("GPU_MEM_LIMIT_MB=")
        .is_some_and(|(_, value)| value.starts_with(|c: char| c.is_ascii_digit()))
}

#[test]
fn nothing_under_packaging_carries_a_secret() {
    // Positive control with a fixture of its own, never with the legacy file:
    // if somebody deletes the legacy unit, the control has to stay standing.
    // Safe as a literal here because the scan below only ever reads
    // `packaging/`, never this source file.
    for canary in [
        "Environment=DATABASE_URL=postgresql://cuba:memorys2026@127.0.0.1:5488/brain",
        "OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwx",
        "CUBA_HTTP_TOKEN=deadbeefdeadbeefdeadbeefdeadbeef",
    ] {
        assert!(
            secret_in(canary).is_some(),
            "the detector does not fire on {canary}, so every absence it reports below means \
             nothing"
        );
    }
    for innocent in [
        "CUBA_HTTP_TOKEN=",
        "Environment=DATABASE_URL=postgresql://localhost:5488/brain",
        "Documentation=https://github.com/LeandroPG19/Memorys",
    ] {
        assert!(
            secret_in(innocent).is_none(),
            "the detector fires on {innocent}, which would make it useless the first time it \
             cried wolf"
        );
    }

    let files = without_the_legacy(every_packaging_file());
    assert!(
        !files.is_empty(),
        "the scan read no file at all under packaging/ outside the legacy list. A scanner \
         that finds nothing passes every absence test ever written"
    );

    let mut found = Vec::new();
    for (_, relative, body) in &files {
        for (n, line) in body.lines().enumerate() {
            if let Some(why) = secret_in(line) {
                found.push(format!("packaging/{relative}:{}: {why}", n + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "packaging/ is versioned, so anything here is in every clone: {found:#?}"
    );
}

#[test]
fn nothing_under_packaging_pins_a_gpu_ceiling_by_hand() {
    assert!(
        pins_a_gpu_ceiling("Environment=CUBA_GPU_MEM_LIMIT_MB=2048"),
        "the detector does not fire on the exact line this slice exists to remove"
    );
    assert!(
        pins_a_gpu_ceiling("#Environment=CUBA_GPU_MEM_LIMIT_MB=2048"),
        "a commented ceiling has to count too: `#` is one keystroke"
    );
    assert!(
        !pins_a_gpu_ceiling("# CUBA_GPU_MEM_LIMIT_MB="),
        "offering the key with no value is the whole point of the generated env file"
    );

    let files = without_the_legacy(every_packaging_file());
    assert!(
        !files.is_empty(),
        "the scan read no file under packaging/ outside the legacy list, so it proves nothing"
    );

    let mut found = Vec::new();
    for (_, relative, body) in &files {
        for (n, line) in body.lines().enumerate() {
            if pins_a_gpu_ceiling(line) {
                found.push(format!("packaging/{relative}:{}: {line}", n + 1));
            }
        }
    }
    assert!(
        found.is_empty(),
        "a hand-written ceiling beats the measurement for good, because `set_if_absent` \
         leaves an already-set variable alone. That is the defect, by name: {found:#?}"
    );
}

// --- 7 · The units point somewhere that exists ------------------------------

#[test]
fn the_units_point_at_the_repository_that_exists() {
    let manifest = std::fs::read_to_string(repo_root().join("package.json")).expect("package.json");
    let parsed: serde_json::Value = serde_json::from_str(&manifest).expect("package.json parses");
    let declared = parsed["repository"]["url"]
        .as_str()
        .expect("package.json declares repository.url")
        .trim_end_matches(".git")
        .to_string();

    assert_eq!(
        service::REPO_URL,
        declared,
        "service::REPO_URL and package.json name different repositories, so whichever one \
         the reader follows is a coin toss"
    );

    // Every file, legacy included. The dead URL is NOT exempt: `Documentation=`
    // is a line no systemd reads to start anything, so fixing it costs one line
    // per file and risks nothing.
    let mut urls = 0usize;
    let mut wrong = Vec::new();
    for (_, relative, body) in every_packaging_file() {
        for (n, line) in body.lines().enumerate() {
            let Some((_, after)) = line.split_once("https://github.com/") else {
                continue;
            };
            let url = format!(
                "https://github.com/{}",
                after
                    .split(|c: char| c.is_whitespace() || c == '"' || c == '<' || c == '#')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(&['.', ',', ')'][..])
            );
            urls += 1;
            if url.trim_end_matches(".git") != declared && !url.starts_with(&declared) {
                wrong.push(format!("packaging/{relative}:{}: {url}", n + 1));
            }
        }
    }

    assert!(
        urls > 0,
        "no github URL was found anywhere under packaging/, so this scan is broken"
    );
    assert!(
        wrong.is_empty(),
        "these point at a repository that does not exist — the reader who follows one gets a \
         404 and concludes the project is gone: {wrong:#?}. Expected {declared}"
    );
}

// --- 8 · The exemption expires on its own -----------------------------------

#[test]
fn the_legacy_packaging_exemption_expires_on_schedule() {
    fn semver(s: &str) -> (u64, u64, u64) {
        let mut parts = s.split('.').map(|p| {
            p.trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<u64>()
                .unwrap_or(0)
        });
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    }

    let now = semver(env!("CARGO_PKG_VERSION"));
    let expires = semver(LEGACY_DROPPED_IN);
    assert!(
        expires > (0, 0, 0),
        "LEGACY_DROPPED_IN does not parse as a version, so this ratchet can never fire"
    );

    let still_here: Vec<&str> = LEGACY_PACKAGING
        .iter()
        .copied()
        .filter(|name| packaging().join(name).exists())
        .collect();

    if now >= expires {
        assert!(
            still_here.is_empty(),
            "the compatibility window closed at {LEGACY_DROPPED_IN} and {still_here:?} are \
             still in packaging/, carrying the database password and the hand-pinned GPU \
             ceiling they were exempted for. Delete them, and delete LEGACY_PACKAGING with \
             them. An exception with no expiry that enforces itself is a good intention"
        );
    }
}

// --- 10 · The versioned XML is the one an operator can hand to schtasks -----

#[test]
fn the_versioned_task_xml_is_utf16_the_way_windows_reads_it() {
    let path = packaging().join("windows/memory-industry-task.xml");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

    // `the_versioned_units_are_what_the_binary_renders` compares decoded text,
    // so it cannot see this: a golden re-saved as UTF-8 by an editor would pass
    // it and still be a file `schtasks /Create /XML` refuses at (1,40), before
    // it parses anything. The whole reason this file is versioned is that an
    // operator can hand it to the scheduler straight out of the clone; in any
    // other encoding it goes back to being an artifact that lives outside the
    // repository, which is the defect F2 exists to close.
    assert!(
        bytes.starts_with(&[0xFF, 0xFE]),
        "packaging/windows/memory-industry-task.xml does not begin with the UTF-16LE BOM \
         (first bytes: {:?}). If `.gitattributes` lost its \
         `working-tree-encoding=UTF-16LE-BOM` line, git checked it out as the UTF-8 it stores \
         and the file in this clone is not installable",
        &bytes[..bytes.len().min(4)]
    );
    assert!(
        read_normalised(&path).contains("encoding=\"UTF-16\""),
        "the bytes are UTF-16 and the declaration is not, which schtasks rejects just as \
         loudly as the other way round"
    );
}

// --- 9 · Generated is committable, written is not ---------------------------

#[test]
fn the_generated_files_are_committable_and_the_written_ones_are_not() {
    fn ignored(relative: &str) -> bool {
        std::process::Command::new("git")
            .args(["check-ignore", "-q", "--", relative])
            .current_dir(repo_root())
            .status()
            .expect("git check-ignore runs")
            .success()
    }

    // Both directions. A golden git ignores stays on the machine that generated
    // it and never reaches a clone — that is what happened to docs/gate.md
    // (local_gate_contract.rs:197-214). An env file git does NOT ignore is a
    // committable bearer token.
    let mut checked = 0usize;
    for target in [Target::Linux, Target::Windows] {
        for (relative, _) in golden_render(target) {
            let tracked_path = format!("packaging/{relative}");
            assert!(
                repo_root().join(&tracked_path).exists(),
                "{tracked_path} is not in the tree, so this contract would pass on a file \
                 that does not exist:\n{REGENERATE}"
            );
            assert!(
                !ignored(&tracked_path),
                "git ignores {tracked_path}. A golden that never reaches a clone is not a \
                 golden, and the gate would compare the binary against a file only one \
                 machine has"
            );
            checked += 1;
        }
    }
    assert!(checked >= 6, "only {checked} golden(s) were checked");

    assert!(
        ignored("packaging/memory-industry.env"),
        "git does not ignore packaging/memory-industry.env — the file `setup service --apply` \
         writes, holding the bearer token for the whole graph and the DATABASE_URL. One \
         `--out packaging` by mistake leaves it committable. Note `.env.*` does NOT cover it: \
         that pattern only matches names that START with `.env.`"
    );
    assert!(
        !ignored("packaging/memory-industry.env.example"),
        "the example is what this repo publishes and it has to reach a clone"
    );
}
