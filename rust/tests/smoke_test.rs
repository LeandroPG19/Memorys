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

    for entry in std::fs::read_dir(&dir)
        .expect("tests/ is readable")
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("readable");
        if !body.contains("std::env::set_var(\"CUBA_") {
            continue;
        }
        checked += 1;
        let tests = body.matches("#[tokio::test]").count() + body.matches("#[test]").count();
        let taken = body.matches("pg_advisory_xact_lock").count()
            + body.matches("ENV_GUARD.lock()").count()
            + body.matches("own_the_environment()").count()
            + body.matches("own_the_sync_dir(").count()
            + body.matches("own_the_process(").count();
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
        checked >= 8,
        "the scan found only {checked} file(s) that move CUBA_SYNC_DIR, and there are more than \
         that. A green result from a scan that found almost nothing proves nothing"
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
         itself. The moment a second one lands in the same file they share the variable"
    );
}
