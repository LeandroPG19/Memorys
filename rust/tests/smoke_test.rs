use serde_json::Value;

#[test]
fn test_all_tools_defined() {
    let tools = memory_industry::constants::tool_definitions();

    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t: &Value| t.get("name").and_then(|n: &Value| n.as_str()))
        .collect();

    let expected = [
        "cuba_alma",
        "cuba_cronica",
        "cuba_faro",
        "cuba_puente",
        "cuba_eco",
        "cuba_alarma",
        "cuba_remedio",
        "cuba_expediente",
        "cuba_jornada",
        "cuba_decreto",
        "cuba_vigia",
        "cuba_zafra",
        "cuba_forget",
        "cuba_reflexion",
        "cuba_hipotesis",
        "cuba_contradiccion",
        "cuba_centinela",
        "cuba_calibrar",
        "cuba_ingesta",
        "cuba_proyecto",
        "cuba_pre_compact",
        "cuba_sync",
        "cuba_juez",
        "cuba_pizarra",
        "cuba_archivo",
        "cuba_receta",
    ];

    for name in &expected {
        assert!(tool_names.contains(name), "Missing tool definition: {name}");
    }

    let mut seen = std::collections::HashSet::new();
    for name in &tool_names {
        assert!(seen.insert(*name), "duplicate tool definition: {name}");
    }
}

#[test]
fn test_tool_schema_structure() {
    let tools = memory_industry::constants::tool_definitions();

    for tool in tools.iter() {
        let name = tool
            .get("name")
            .and_then(|n: &Value| n.as_str())
            .unwrap_or("???");

        assert!(tool.get("name").is_some(), "{name}: missing 'name'");
        assert!(
            tool.get("description").is_some(),
            "{name}: missing 'description'"
        );
        assert!(
            tool.get("inputSchema").is_some(),
            "{name}: missing 'inputSchema'"
        );

        let schema = tool.get("inputSchema").unwrap();
        assert_eq!(
            schema.get("type").and_then(|t: &Value| t.as_str()),
            Some("object"),
            "{name}: inputSchema.type must be 'object'"
        );
        assert!(
            schema.get("properties").is_some(),
            "{name}: missing inputSchema.properties"
        );
    }
}

#[test]
fn test_threshold_invariants() {
    use memory_industry::constants::*;

    const _: () = assert!(PRED_ERROR_REINFORCE > DEDUP_THRESHOLD);
    const _: () = assert!(DEDUP_THRESHOLD > PRED_ERROR_UPDATE);
    const _: () = assert!(HEBBIAN_ACCESS_BOOST > 0.0 && HEBBIAN_ACCESS_BOOST < 0.1);
}

#[test]
fn advertised_tools_are_all_dispatchable() {
    let tools = memory_industry::constants::tool_definitions();

    for tool in tools.iter() {
        let name = tool
            .get("name")
            .and_then(|n: &Value| n.as_str())
            .expect("every tool definition has a name");

        assert!(
            memory_industry::handlers::is_known_tool(name),
            "{name} se anuncia en tools/list pero el dispatcher no la conoce: \
             un cliente que la llame recibiría 'unknown tool' en runtime"
        );

        assert!(name.starts_with("cuba_"), "{name}: prefijo inesperado");
        assert!(
            name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "{name}: los nombres de tool son snake_case en minúsculas"
        );
    }
}

#[test]
fn the_embedded_migrations_build_every_object_the_handlers_query() {
    let migrator = sqlx::migrate!("./migrations");
    let up: String = migrator
        .migrations
        .iter()
        .filter(|m| m.migration_type.is_up_migration())
        .map(|m| m.sql.as_ref())
        .collect::<Vec<&str>>()
        .join("\n");

    assert!(
        !up.is_empty(),
        "sqlx::migrate! resolved zero up-migrations, so db.rs would hand the handlers an \
         empty database and every query would fail at runtime"
    );

    for object in &[
        "brain_entities",
        "brain_observations",
        "brain_relations",
        "brain_errors",
        "brain_sessions",
        "brain_episodes",
        "brain_triggers",
        "brain_verify_log",
        "brain_audit_log",
        "brain_procedures",
        "brain_wm",
        "embedding_model",
        "session_id",
        "importance",
        "tags TEXT[]",
        "idx_obs_high_importance",
        "CREATE EXTENSION IF NOT EXISTS vector",
        "pg_trgm",
    ] {
        assert!(
            up.contains(object),
            "«{object}» is queried by the handlers but no migration creates it. This test \
             reads the set sqlx::migrate! embeds, which is the same one db.rs applies. There \
             used to be a second answer to «what is the schema» in src/schema.sql; it had \
             frozen at 8 tables against 31 and was deleted rather than repaired, because a \
             stale second answer is worse than one answer"
        );
    }
}

#[test]
fn test_cognitive_constants_valid() {
    use memory_industry::constants::*;

    const _: () = assert!(HEBBIAN_ACCESS_BOOST > 0.0 && HEBBIAN_ACCESS_BOOST < 1.0);
    const _: () = assert!(BCM_THROTTLE_SCALE > 0.0 && BCM_THROTTLE_SCALE <= 1.0);
}

#[test]
fn test_cache_constants_valid() {
    use memory_industry::constants::*;
    const _: () = assert!(CACHE_MAX_ENTRIES > 0);
    const _: () = assert!(CACHE_TTL_SECS > 0);
}

#[test]
fn test_valid_types_lists() {
    use memory_industry::constants::*;

    assert!(!VALID_ENTITY_TYPES.is_empty());
    assert!(!VALID_OBSERVATION_TYPES.is_empty());
    assert!(!VALID_SOURCES.is_empty());
    assert!(!VALID_RELATION_TYPES.is_empty());

    assert!(VALID_ENTITY_TYPES.contains(&"concept"));
    assert!(VALID_OBSERVATION_TYPES.contains(&"fact"));
    assert!(VALID_SOURCES.contains(&"agent"));
    assert!(VALID_RELATION_TYPES.contains(&"uses"));
}

#[test]
fn test_importance_priors() {
    use memory_industry::constants::importance_prior;

    assert!((importance_prior("decision", 0.5) - 0.8).abs() < f64::EPSILON);
    assert!((importance_prior("lesson", 0.5) - 0.75).abs() < f64::EPSILON);
    assert!((importance_prior("error", 0.5) - 0.7).abs() < f64::EPSILON);
    assert!((importance_prior("solution", 0.5) - 0.7).abs() < f64::EPSILON);

    let fact_high = importance_prior("fact", 1.0);
    let fact_low = importance_prior("fact", 0.2);
    assert!(
        fact_high > fact_low,
        "Higher density should yield higher importance"
    );
    assert!(fact_high <= 0.9, "fact importance capped at 0.9");

    let ctx = importance_prior("context", 1.0);
    assert!(ctx <= 0.7, "context importance capped at 0.7");

    assert!(importance_prior("fact", 0.0) >= 0.1);
    assert!(importance_prior("context", 0.0) >= 0.1);
}

static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn own_the_environment() -> std::sync::MutexGuard<'static, ()> {
    ENV_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn model_tag_follows_the_environment() {
    let _env = own_the_environment();
    unsafe { std::env::remove_var("CUBA_EMBED_MODEL") };
    assert_eq!(
        memory_industry::embeddings::onnx::current_model(),
        "multilingual-e5-small",
        "with nothing set, the default stands"
    );

    unsafe { std::env::set_var("CUBA_EMBED_MODEL", "bge-m3") };
    assert_eq!(
        memory_industry::embeddings::onnx::current_model(),
        "bge-m3",
        "CUBA_EMBED_MODEL must win — it is the only thing that knows which model ran"
    );

    unsafe { std::env::remove_var("CUBA_EMBED_MODEL") };
}

#[test]
fn a_request_without_an_id_parses_as_a_notification() {
    use memory_industry::protocol::JsonRpcRequest;

    let notification: JsonRpcRequest = serde_json::from_str(
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    )
    .expect("a notification is a valid envelope: params is #[serde(default)] and id is optional");
    assert!(
        notification.id.is_none(),
        "an absent id is the only thing separating a notification from a call. Parsed with \
         one, the server would answer something nobody is waiting for and the client would \
         see an orphan response"
    );
    assert_eq!(notification.method, "notifications/initialized");

    let call: JsonRpcRequest =
        serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#)
            .expect("a call with no params is valid");
    assert_eq!(call.id, Some(serde_json::json!(7)));

    assert!(
        serde_json::from_str::<JsonRpcRequest>(r#"{"jsonrpc":"2.0","id":1}"#).is_err(),
        "with no method there is nothing to dispatch: it has to die in the parser and come \
         back as -32600, not reach a handler"
    );
}

#[test]
fn a_session_setting_is_never_applied_to_a_pooled_connection() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src/ is readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let mut files = Vec::new();
    walk(std::path::Path::new("src"), &mut files);
    assert!(
        files.len() > 20,
        "the walk found almost nothing, so it proves nothing"
    );

    let mut offenders = Vec::new();
    for path in &files {
        let body = std::fs::read_to_string(path).expect("a source file is readable");
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if !line.contains("SET LOCAL") {
                continue;
            }
            let window = lines[i..(i + 6).min(lines.len())].join("\n");
            if !window.contains("*tx") {
                offenders.push(format!("{}:{}", path.display(), i + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "SET LOCAL lives and dies with a transaction. Run it through a pool and it applies to \
         an implicit single-statement transaction that commits immediately, so the next query \
         — which may not even land on the same connection — runs with the default. faro.rs \
         carried `SET LOCAL hnsw.ef_search = 200` on a pool for exactly that reason: measured \
         in another transaction the setting reads back empty, the .ok() swallowed any \
         complaint, and EXPLAIN showed a Seq Scan anyway. A knob that cannot move is worse \
         than no knob, because it reads as tuned. The window looks for `*tx` rather than \
         `&mut *tx` because a helper that takes the transaction by reference writes \
         `&mut **tx`, and a detector that only knew one spelling would have flagged correct \
         code — which teaches people to widen the exception instead of the check. \
         Offenders: {offenders:?}"
    );
}

#[test]
fn no_mcp_path_can_claim_an_evidence_level_it_did_not_earn() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir)
            .expect("the directory is readable")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let root = std::path::Path::new("src");
    let mut handlers = Vec::new();
    walk(&root.join("handlers"), &mut handlers);
    assert!(
        handlers.len() > 20,
        "the walk found {} handler files, too few to conclude anything from their silence",
        handlers.len()
    );

    let mut claimants = Vec::new();
    for path in &handlers {
        let body = std::fs::read_to_string(path).expect("readable");
        for (offset, _) in body.match_indices("evidence") {
            let lead = &body[offset.saturating_sub(220)..offset];
            if !lead.contains("INSERT INTO") && !lead.contains("SET ") {
                continue;
            }
            claimants.push(format!(
                "{}:{}",
                path.display(),
                body[..offset].matches('\n').count() + 1
            ));
        }
    }

    assert!(
        claimants.is_empty(),
        "an evidence level is worth exactly as much as the rule about who may write it. Every \
         file here is reachable over MCP, which means reachable by a model, and a model that \
         can write `verified` has turned the level into a synonym for `asserted` with extra \
         characters. The rule is structural rather than agreed: the column defaults to \
         asserted and no handler names it in a write, so the MCP path cannot produce another \
         value even by mistake. Naming the column is the only route — a bind value on its own \
         cannot reach it, because binds are positional and the statement still has to say \
         `evidence`. Two earlier versions of this check were wrong in opposite directions: one \
         matched only SQL-quoted level names and missed a Rust string, the other matched the \
         bare word and accused faro, where `verified` is a grounding level in mode=verify and \
         has nothing to do with this column. Claimants: {claimants:?}"
    );

    let codegraph = std::fs::read_to_string(root.join("codegraph_cli.rs")).expect("readable");
    assert!(
        codegraph.contains("'observed'") && codegraph.contains("evidence"),
        "and the scan has to be able to see the column in a write when one is there, or its \
         silence over the handlers means nothing. codegraph is the CLI that parses a real \
         tree-sitter AST, which is what earns `observed`"
    );
}

#[test]
fn every_test_that_moves_a_process_wide_variable_serialises() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut checked = 0;
    let mut reached_without_the_cuba_prefix = 0;

    for entry in std::fs::read_dir(&dir)
        .expect("tests/ is readable")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        // The one file the scan cannot judge is the one it lives in: every
        // marker below appears here as a string literal, so scanning itself
        // would credit it for guards it never takes. Its single environment
        // test is `model_tag_follows_the_environment`, and it takes
        // `own_the_environment()` on its first line.
        if path.ends_with("smoke_test.rs") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("readable");
        // Any write to the process environment, whatever the variable is
        // called. Matching `set_var("CUBA_` left two holes at once. The prefix:
        // this release added the MEMORY_INDUSTRY_* namespace, so a file that
        // only writes the new spelling was invisible here — v040_write_ahead.rs
        // sets MEMORY_INDUSTRY_GRAPH_DB from three tests and this scan has
        // never once looked at it. And the shape: reading the name out of the
        // call means a file that sets the variable through a helper escapes,
        // because the name is no longer on the same line as the call. Both
        // holes are the same mistake — asking what is being set instead of
        // whether anything is.
        if !body.contains("env::set_var(") && !body.contains("env::remove_var(") {
            continue;
        }
        checked += 1;
        if !body.contains("std::env::set_var(\"CUBA_") {
            reached_without_the_cuba_prefix += 1;
        }
        let tests = body.matches("#[tokio::test]").count() + body.matches("#[test]").count();
        // Declarations do not count either, for the same reason the markers are
        // takings: a file that defines `own_the_sync_dir` and never calls it
        // used to score one and walk out.
        let taken: usize = body
            .lines()
            .filter(|line| {
                let line = line.trim_start();
                !line.starts_with("fn ")
                    && !line.starts_with("async fn ")
                    && !line.starts_with("pub fn ")
                    && !line.starts_with("pub async fn ")
            })
            .map(|line| {
                line.matches("pg_advisory_xact_lock").count()
                    + line.matches("ENV_GUARD.lock()").count()
                    + line.matches("own_the_environment(").count()
                    + line.matches("own_the_sync_dir(").count()
                    + line.matches("own_the_process(").count()
            })
            .sum();
        if tests > 1 && taken == 0 {
            offenders.push(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    assert!(
        checked >= 30,
        "the scan is in scope for only {checked} test file(s) that write to the process \
         environment, and there are more than that. A green result from a scan that found \
         almost nothing proves nothing. The floor was 8 while the real number was 35, which is \
         twenty-seven files of coverage this could have lost without a word — and losing \
         coverage silently is exactly what it turned out to be doing. Measured 2026-09-21: 36"
    );
    assert!(
        reached_without_the_cuba_prefix >= 1,
        "all {checked} file(s) in scope write a `CUBA_` name inline, which is precisely what \
         this scan saw back when it matched `set_var(\"CUBA_` and nothing else. If no file \
         arrives by another route, the widening is decoration and the MEMORY_INDUSTRY_* \
         namespace is unwatched again without anyone editing a line. Two arrive that way \
         today: one writes only MEMORY_INDUSTRY_GRAPH_DB, the other sets its variables \
         through a helper"
    );
    assert!(
        offenders.is_empty(),
        "An environment variable belongs to the whole process, so two tests in the same binary \
         setting one at once read each other's value. It has bitten twice: CUBA_SYNC_DIR made \
         the exporter fail with `path traversal blocked`, and CUBA_PANEL made a test that \
         asserts the panel is NOT served get a 200, because a sibling test had switched it on \
         a millisecond earlier. Neither is flaky code — both are one variable with two owners. \
         Serialise on an advisory lock (for something shared beyond the process, like a \
         directory) or a static Mutex (for the environment, which is per-process). \
         Offenders: {offenders:?}.\n\nIt counts guards TAKEN, not declared, and that is not \
         pedantry: the first version of this check looked for the declaration, so a sabotage \
         that deleted every `.lock()` and left the `static` line passed green. A guard nobody \
         takes is a comment. Running them with --test-threads=1 hides it, which is \
         exactly what happened — they passed locally under that flag and went red in the gate, \
         which does not pass it.\n\nOne test per file is exempt and that is not a loophole: \
         cargo gives every tests/*.rs its own process, so a lone test cannot collide with \
         itself. The moment a second one lands in the same file they share the variable.\
         \n\nThis scan got wider on 2026-09-21: it no longer asks for the `CUBA_` prefix, nor \
         for the variable name to sit inside the call. A file that only writes \
         MEMORY_INDUSTRY_* names, or that writes through a helper, is judged here for the \
         first time — so a name appearing today is not new code, it is code this check was \
         never able to see"
    );
}

/// The hosts a test could bind or dial a daemon on. A port written next to any of
/// them is a port somebody chose.
const HOSTS: [&str; 4] = ["127.0.0.1", "0.0.0.0", "localhost", "[::1]"];

const G1_CONST: &str = "G1 const u16";
const G2_ADDRESS: &str = "G2 address literal";
const G3_URL: &str = "G3 url literal";
const G4_CALL: &str = "G4 call with a literal port";
const G5_BIND: &str = "G5 bind literal";

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// An integer in 1..=65535, with the `_` separators and the `u16` suffix a Rust
/// literal may carry. `99999` is not a port, which is why an address that is
/// invalid on purpose passes.
fn is_port_literal(text: &str) -> bool {
    let digits = text.strip_suffix("u16").unwrap_or(text).replace('_', "");
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && digits
            .parse::<u32>()
            .is_ok_and(|n| (1..=65535).contains(&n))
}

fn is_host_and_port(text: &str) -> bool {
    HOSTS.iter().any(|&host| {
        text.strip_prefix(host)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(is_port_literal)
    })
}

/// Whether the file brings a daemon up, in this process or as a child. Only such a
/// file can lose a port to another one, so only it is held to G1 through G4.
fn starts_a_daemon(body: &str) -> bool {
    body.contains("http::serve_pool(")
        || body.contains("[\"serve\"")
        || body.contains("arg(\"serve\")")
}

/// The contents of every double-quoted string on the line that has no escape in it.
fn quoted_strings(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(start) = line[from..].find('"').map(|at| from + at) {
        let mut end = start + 1;
        while end < bytes.len() && bytes[end] != b'"' && bytes[end] != b'\\' {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'"' {
            found.push(&line[start + 1..end]);
            from = end + 1;
        } else {
            from = start + 1;
        }
    }
    found
}

/// The number in `const NAME: u16 = <number>;`, when the line is exactly that.
fn const_u16_value(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let rest = rest.strip_prefix("pub ").unwrap_or(rest).trim_start();
    let rest = rest.strip_prefix("const ")?.trim_start();
    let name_len = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
    if name_len == 0 {
        return None;
    }
    let rest = rest[name_len..]
        .trim_start()
        .strip_prefix(':')?
        .trim_start();
    let rest = rest
        .strip_prefix("u16")?
        .trim_start()
        .strip_prefix('=')?
        .trim_start();
    let value_len = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '_'))
        .unwrap_or(rest.len());
    let (value, tail) = rest.split_at(value_len);
    tail.trim_start().starts_with(';').then_some(value)
}

/// A line where a quoted address is being declared or handed to a daemon. A bare
/// `"127.0.0.1:11434"` in an assertion is not: it is text being looked for.
fn declares_or_serves(line: &str) -> bool {
    let item = line.trim_start();
    let item = item.strip_prefix("pub ").unwrap_or(item);
    item.starts_with("const ")
        || item.starts_with("static ")
        || line.contains("serve_pool(")
        || line.contains("\"serve\"")
        || line.contains("CUBA_HTTP_ADDR")
}

/// `http://<host>:<port>` with the host glued to the scheme. A URL with a user in
/// front, like the canary one, is a different server and does not match.
fn has_a_url_with_a_port(line: &str) -> bool {
    HOSTS.iter().any(|&host| {
        let prefix = format!("http://{host}:");
        line.match_indices(&prefix).any(|(at, _)| {
            let digits: String = line[at + prefix.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            is_port_literal(&digits)
        })
    })
}

/// `bind("<host>:<port>"` and `bind((<expr>, <port>))`.
fn binds_a_literal_port(line: &str) -> bool {
    line.match_indices("bind(").any(|(at, _)| {
        let args = line[at + "bind(".len()..].trim_start();
        if let Some(rest) = args.strip_prefix('"') {
            return rest
                .find('"')
                .is_some_and(|end| is_host_and_port(&rest[..end]));
        }
        let Some(tuple) = args.strip_prefix('(') else {
            return false;
        };
        let Some((host, rest)) = tuple.split_once(',') else {
            return false;
        };
        if host.trim().is_empty() || host.contains(['(', ')']) {
            return false;
        }
        let rest = rest.trim_start();
        let value_len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '_'))
            .unwrap_or(rest.len());
        let (value, tail) = rest.split_at(value_len);
        let Some(tail) = tail.trim_start().strip_prefix(')') else {
            return false;
        };
        tail.trim_start().starts_with(')') && is_port_literal(value)
    })
}

/// Names of the functions in the file whose first parameter is `port: u16`.
fn fns_taking_a_port_first(body: &str) -> Vec<&str> {
    let mut names: Vec<&str> = body
        .match_indices("fn ")
        .filter(|&(at, _)| !body[..at].chars().next_back().is_some_and(is_word))
        .filter_map(|(at, _)| {
            let rest = body[at + "fn ".len()..].trim_start();
            let name_len = rest.find(|c: char| !is_word(c)).unwrap_or(rest.len());
            let (name, after) = rest.split_at(name_len);
            let params = after
                .trim_start()
                .strip_prefix('(')?
                .trim_start()
                .strip_prefix("port")?
                .trim_start()
                .strip_prefix(':')?
                .trim_start();
            (!name.is_empty() && params.starts_with("u16")).then_some(name)
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Lines (from 1) where `name(` is called with a port literal as its first argument.
/// The argument may sit on a later line than the call: `serve(\n    18861,`.
fn calls_with_a_literal_port(body: &str, name: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    for (at, _) in body.match_indices(name) {
        let before = &body[..at];
        if before.chars().next_back().is_some_and(is_word) || before.ends_with("fn ") {
            continue;
        }
        let Some(args) = body[at + name.len()..].strip_prefix('(') else {
            continue;
        };
        let args = args.trim_start();
        let value_len = args
            .find(|c: char| !(c.is_ascii_digit() || c == '_'))
            .unwrap_or(args.len());
        let (value, tail) = args.split_at(value_len);
        let tail = tail.strip_prefix("u16").unwrap_or(tail).trim_start();
        if (tail.starts_with(',') || tail.starts_with(')')) && is_port_literal(value) {
            lines.push(before.matches('\n').count() + 1);
        }
    }
    lines
}

/// Every place the file writes a port down, as (line from 1, shape), sorted. G1 to
/// G4 apply only to a file that starts a daemon; G5 applies to every file, because
/// a listener anywhere in tests/ on a number somebody chose can be the one that is
/// busy when the other host's judge ran a minute earlier.
fn fixed_ports(body: &str) -> Vec<(usize, &'static str)> {
    let lines: Vec<&str> = body.lines().collect();
    let daemon = starts_a_daemon(body);
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if is_comment(line) {
            continue;
        }
        let number = index + 1;
        if daemon {
            if const_u16_value(line).is_some_and(is_port_literal) {
                found.push((number, G1_CONST));
            }
            if declares_or_serves(line) && quoted_strings(line).into_iter().any(is_host_and_port) {
                found.push((number, G2_ADDRESS));
            }
            if has_a_url_with_a_port(line) {
                found.push((number, G3_URL));
            }
        }
        if binds_a_literal_port(line) {
            found.push((number, G5_BIND));
        }
    }
    if daemon {
        for name in fns_taking_a_port_first(body) {
            for number in calls_with_a_literal_port(body, name) {
                if !is_comment(lines[number - 1]) {
                    found.push((number, G4_CALL));
                }
            }
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// The first integer literal of 1024 or more on the line, as text.
fn a_literal_of_1024_or_more(line: &str) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        if !chars[at].is_ascii_digit() || (at > 0 && is_word(chars[at - 1])) {
            at += 1;
            continue;
        }
        let mut end = at;
        while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '_') {
            end += 1;
        }
        let digits: String = chars[at..end].iter().filter(|c| **c != '_').collect();
        if !digits.parse::<u64>().is_ok_and(|n| n < 1024) {
            return Some(digits);
        }
        at = end;
    }
    None
}

/// What is missing from the one file a test may take its daemon's port from: it
/// has to ask the OS (`bind("127.0.0.1:0")`), read the answer (`.local_addr()`),
/// and hold no number a person chose.
fn ephemeral_helper_problems(body: &str) -> Vec<String> {
    let mut problems = Vec::new();
    if !body.contains("bind(\"127.0.0.1:0\")") {
        problems.push(
            "never binds \"127.0.0.1:0\", so the OS is not the one choosing the port".to_string(),
        );
    }
    if !body.contains(".local_addr()") {
        problems.push("never reads .local_addr(), so it cannot know which port the OS gave".into());
    }
    for (index, line) in body.lines().enumerate() {
        if is_comment(line) {
            continue;
        }
        if let Some(number) = a_literal_of_1024_or_more(line) {
            problems.push(format!(
                "line {}: the literal {number} is a port a person wrote down",
                index + 1
            ));
        }
    }
    problems
}

#[test]
fn the_fixed_port_scan_sees_every_shape_and_lets_the_chosen_ports_through() {
    const DAEMON: &str = "http::serve_pool(&addr, pool, connected);\n";
    let in_a_daemon_file = |rest: &str| fixed_ports(&format!("{DAEMON}{rest}"));

    assert_eq!(
        in_a_daemon_file("const PORT: u16 = 18854;\n"),
        [(2, G1_CONST)],
        "a const u16 holding a port, in a file that starts a daemon, is G1"
    );
    assert_eq!(
        in_a_daemon_file("pub const PEER_ADDR: &str = \"127.0.0.1:18797\";\n"),
        [(2, G2_ADDRESS)],
        "an address literal on a const line is G2"
    );
    assert_eq!(
        in_a_daemon_file("    .args([\"serve\", \"127.0.0.1:18000\"])\n"),
        [(2, G2_ADDRESS)],
        "an address literal handed to the `serve` subcommand is G2"
    );
    assert_eq!(
        in_a_daemon_file("std::env::set_var(\"CUBA_HTTP_ADDR\", \"0.0.0.0:8787\");\n"),
        [(2, G2_ADDRESS)],
        "an address literal given to CUBA_HTTP_ADDR is G2"
    );
    assert_eq!(
        in_a_daemon_file("    .get(\"http://127.0.0.1:18812/panel\")\n"),
        [(2, G3_URL)],
        "a URL with the host glued to the scheme and a port is G3"
    );
    assert_eq!(
        in_a_daemon_file("async fn daemon(port: u16) {}\ndaemon(18853).await;\n"),
        [(3, G4_CALL)],
        "G4 marks the call, never the declaration of the function that takes the port"
    );
    assert_eq!(
        in_a_daemon_file("fn serve(port: u16, pool: Pool) {}\nserve(\n    18861,\n    pool)\n"),
        [(3, G4_CALL)],
        "G4 sees a literal that sits on a later line than the call, and names the line of \
         `serve(`: v029_accounts_over_real_http writes most of its 74 calls that way"
    );
    assert_eq!(
        fixed_ports("let l = std::net::TcpListener::bind(\"127.0.0.1:18000\");\n"),
        [(1, G5_BIND)],
        "a bind on an address literal is G5, even in a file that starts no daemon"
    );
    assert_eq!(
        fixed_ports("let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 18000));\n"),
        [(1, G5_BIND)],
        "a bind on a (ip, literal) tuple is G5, even in a file that starts no daemon"
    );

    for chosen in [
        "let l = TcpListener::bind(\"127.0.0.1:0\");\n",
        "http::serve_pool(&format!(\"127.0.0.1:{port}\"), pool, connected);\n",
        "async fn daemon(port: u16) {}\ndaemon(port).await;\ndaemon(ephemeral::port()).await;\n",
    ] {
        let hits = in_a_daemon_file(chosen);
        assert!(
            hits.is_empty(),
            "a port the OS chose, or one held in a variable, is not written down: {chosen:?} \
             gave {hits:?}"
        );
    }
    for data in [
        "assert!(llm.contains(\"127.0.0.1:11434\"));\n\
         const CANARY: &str = \"http://canaryuser:canarypass-llm@127.0.0.1:11434/v1\";\n",
        "    .args([\"serve\", \"127.0.0.1:99999\"])\n",
        "async fn daemon(port: u16) {}\nlet at = Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0);\n",
        "async fn daemon(port: u16) {}\n// daemon(18853)\n/// const PORT: u16 = 18854;\n",
    ] {
        let hits = in_a_daemon_file(data);
        assert!(
            hits.is_empty(),
            "a number that is data and not a bind, or a comment, must pass: {data:?} gave \
             {hits:?}"
        );
    }
    let a_port_in_a_file_that_serves_nothing =
        fixed_ports("const HOST_PORT: u16 = 8787;\nfn me(port: u16) {}\nme(8787);\n");
    assert!(
        a_port_in_a_file_that_serves_nothing.is_empty(),
        "bridge.json contents and an announced port are data in a file that starts no daemon: \
         {a_port_in_a_file_that_serves_nothing:?}"
    );

    assert!(
        ephemeral_helper_problems(
            "pub fn port() -> u16 {\n    let probe = std::net::TcpListener::bind(\"127.0.0.1:0\")\n        \
             .expect(\"a free loopback port\");\n    probe.local_addr().expect(\"its address\").port()\n}\n"
        )
        .is_empty(),
        "a helper that binds :0 and reads local_addr is the shape the daemon tests rely on"
    );
    assert!(
        !ephemeral_helper_problems("pub fn port() -> u16 {\n    18853\n}\n").is_empty(),
        "a helper that returns a number somebody chose is the bug this guard exists for"
    );
}

#[test]
fn no_daemon_test_listens_on_a_port_it_wrote_down() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .expect("tests/ is readable")
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            let helper = path.join("mod.rs");
            if helper.is_file() {
                files.push(helper);
            }
            continue;
        }
        // This file's fixtures contain every shape on purpose, the same reason the
        // environment scan above skips it.
        if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("smoke_test.rs") {
            files.push(path);
        }
    }
    files.sort();

    let mut daemons = 0;
    let mut written_down = 0;
    let mut problems: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for path in &files {
        let body = std::fs::read_to_string(path).expect("readable");
        if starts_a_daemon(&body) {
            daemons += 1;
        }
        let name = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for (line, shape) in fixed_ports(&body) {
            written_down += 1;
            problems
                .entry(name.clone())
                .or_default()
                .push(format!("{line} {shape}"));
        }
    }

    assert!(
        daemons >= 12,
        "the scan found {daemons} file(s) under tests/ that start a daemon, and there are 12 \
         (measured 2026-10-14). A scan that finds no daemons proves nothing: it would be green \
         because it looked at nothing"
    );

    let helper_problems = match std::fs::read_to_string(dir.join("ephemeral").join("mod.rs")) {
        Ok(body) => ephemeral_helper_problems(&body),
        Err(_) => vec![
            "rust/tests/ephemeral/mod.rs does not exist: it is the only place a test may take \
             the port of its daemon from"
                .to_string(),
        ],
    };

    let report = problems
        .iter()
        .map(|(file, items)| format!("  {file} ({}): {}", items.len(), items.join(", ")))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        problems.is_empty() && helper_problems.is_empty(),
        "{written_down} line(s) in {} file(s) write down a port that a test then listens or \
         dials on, or the helper that should replace them is missing or wrong. Per file, \
         `line shape`:\n{report}\n\nThe helper: {helper_problems:?}\n\nWhy it matters. A fixed port collides by construction \
         between the two judges, not by chance: on 2026-10-14 the Windows judge left \
         127.0.0.1:18853 in TIME_WAIT (the daemon closes first, so the wait is on its side) and \
         the Linux judge, which runs next over the same shared port table (WSL \
         networkingMode=mirrored), got `os error 98` for ~70 s. The ephemeral ranges do not \
         overlap (Windows 49152-65535, WSL 44620-48715), so a port asked of the OS can never \
         land on the other host's.\n\nThe rule. Ask the OS through `ephemeral::port()` \
         (tests/ephemeral/mod.rs) on the line right before the `tokio::spawn` of `serve_pool`, \
         inside the section that already serialises daemon starts in that binary (ENV_GUARD or \
         own_the_process), and return the port from the helper. The test never writes or \
         receives a number: two daemons of one binary then cannot be handed the same port, \
         which is the only way a test ends up talking to the wrong daemon",
        problems.len()
    );
}
