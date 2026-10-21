//! What a client really receives from `initialize` and `tools/list`, read from the binary.
//!
//! `constants::tools_for_profile` used to run every catalogue through `tools_with_memory_aliases`,
//! which doubles each `cuba_*` tool into a `memory_*` twin «for one release». The release passed,
//! the doubling stayed, and the README kept saying «31 tools» and «15 of 31 under `lean`» while a
//! client was handed 64 and 32 (found by the 0.32 audit, 2026-10-21). A client that defers MCP
//! tools until it searches for them pays for every name it is shown, and the twins carry the same
//! schema again. The unit tests could not see it: `tools_list_announces_memory_aliases` called
//! the alias function directly, so it pinned the mechanism and not what goes over the wire.
//!
//! The server is started the way a client starts it (stdio, no arguments) against the database
//! that is down on purpose: `initialize` and `tools/list` never touch it, and the test must not
//! depend on a Postgres being there. HOME points at an empty directory for the same reason as in
//! `nli_cell_looks_once`: with no model in any cache the server has nothing to load, and a
//! session ends in about a second instead of the ~20 s the reranker takes to open on a CPU
//! (measured: 19.8 s with the models, 11.1 s with a refused port and none, which is `dead_db`'s
//! reason for hanging up instead).

use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

mod dead_db;

/// Sends `requests` on stdin, closes it, and returns every JSON line the server printed.
/// Killed after `budget`: a server that ignores EOF would otherwise hang the suite.
fn talk(envs: &[(&str, &str)], requests: &[Value]) -> Vec<Value> {
    static SESSION: AtomicU32 = AtomicU32::new(0);
    let home = std::env::temp_dir().join(format!(
        "memory-industry-catalogue-{}-{}",
        std::process::id(),
        SESSION.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&home).expect("the test owns this directory");

    let mut command = Command::new(env!("CARGO_BIN_EXE_memory-industry"));
    command
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("DATABASE_URL", dead_db::url())
        .env_remove("CUBA_TOOL_PROFILE")
        .env_remove("CUBA_TOOL_ALIASES")
        .env_remove("ONNX_MODEL_PATH")
        .env_remove("ORT_DYLIB_PATH")
        .env_remove("CUBA_RERANKER_PATH")
        .env_remove("CUBA_NLI_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in envs {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("binary runs");

    let mut stdin = child.stdin.take().expect("stdin is piped");
    for request in requests {
        writeln!(stdin, "{request}").expect("the server reads its stdin");
    }
    drop(stdin);

    let mut pipe = child.stdout.take().expect("stdout is piped");
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        pipe.read_to_string(&mut out).ok();
        out
    });

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if child.try_wait().expect("child can be polled").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            child.wait().ok();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let printed = reader.join().expect("stdout reader");
    let _ = std::fs::remove_dir_all(&home);
    printed
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "mcp-catalogue-test", "version": "0"}
        }
    })
}

fn list_tools() -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
}

fn reply(lines: &[Value], id: u64) -> &Value {
    lines
        .iter()
        .find(|l| l.get("id").and_then(Value::as_u64) == Some(id))
        .unwrap_or_else(|| panic!("the server never answered request {id}: {lines:?}"))
}

fn names(listing: &Value) -> Vec<String> {
    listing
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools/list answers with result.tools")
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from))
        .collect()
}

#[test]
fn the_default_catalogue_lists_each_tool_once() {
    let lines = talk(&[], &[initialize(), list_tools()]);
    let listed = names(reply(&lines, 2));

    let distinct: HashSet<&String> = listed.iter().collect();
    assert_eq!(
        distinct.len(),
        listed.len(),
        "tools/list repeats a name: {listed:?}"
    );

    let twins: Vec<&String> = listed.iter().filter(|n| n.starts_with("memory_")).collect();
    assert!(
        twins.is_empty(),
        "tools/list hands every client a `memory_*` twin of each tool by default ({} of {} \
         names). Each twin repeats a whole schema in a catalogue the client must hold or \
         search, and the README counts 31 tools. The aliases still dispatch; listing them is \
         the opt-in `CUBA_TOOL_ALIASES=1`. Twins: {twins:?}",
        twins.len(),
        listed.len()
    );

    // + cuba_tools and cuba_call, the two that are not in `tool_definitions`.
    let expected = memory_industry::constants::tool_definitions().len() + 2;
    assert_eq!(
        listed.len(),
        expected,
        "the full profile is every tool plus the two discovery tools: {listed:?}"
    );
}

#[test]
fn lean_is_what_the_readme_says_it_is() {
    let lines = talk(
        &[("CUBA_TOOL_PROFILE", "lean")],
        &[initialize(), list_tools()],
    );
    let listed = names(reply(&lines, 2));
    assert_eq!(
        listed.len(),
        15,
        "the README promises «15 of 31» under CUBA_TOOL_PROFILE=lean; the wire says {listed:?}"
    );
}

#[test]
fn the_aliases_are_listed_when_asked_and_each_one_dispatches() {
    // Same yes-words as the other switches (`CUBA_SKIP_MIGRATIONS`, ...).
    for on in ["true", "TRUE", "yes"] {
        let lines = talk(&[("CUBA_TOOL_ALIASES", on)], &[initialize(), list_tools()]);
        assert!(
            names(reply(&lines, 2)).iter().any(|n| n == "memory_whoami"),
            "CUBA_TOOL_ALIASES={on:?} is a yes and must list the twins"
        );
    }

    let lines = talk(&[("CUBA_TOOL_ALIASES", "1")], &[initialize(), list_tools()]);
    let listed = names(reply(&lines, 2));

    for must in ["memory_whoami", "memory_artifact", "memory_context"] {
        assert!(
            listed.iter().any(|n| n == must),
            "CUBA_TOOL_ALIASES=1 must bring `{must}` back: {listed:?}"
        );
    }
    for alias in listed.iter().filter(|n| n.starts_with("memory_")) {
        let canonical = memory_industry::handlers::canonicalize_tool_name(alias);
        assert!(
            canonical.starts_with("cuba_") && listed.iter().any(|n| n == canonical),
            "`{alias}` is listed but resolves to `{canonical}`, which is not a listed tool"
        );
        assert!(
            memory_industry::handlers::is_known_tool(alias),
            "`{alias}` is listed but the dispatcher does not know it"
        );
    }

    for off in ["0", "", "no", "false"] {
        let lines = talk(&[("CUBA_TOOL_ALIASES", off)], &[initialize(), list_tools()]);
        let listed = names(reply(&lines, 2));
        assert!(
            !listed.iter().any(|n| n.starts_with("memory_")),
            "CUBA_TOOL_ALIASES={off:?} is not a yes and must not list the twins"
        );
    }
}

#[test]
fn initialize_tells_the_client_what_this_server_is_for() {
    let lines = talk(&[], &[initialize()]);
    let result = reply(&lines, 1)
        .get("result")
        .expect("initialize answers with a result");
    let instructions = result.get("instructions").and_then(Value::as_str).expect(
        "initialize carries no `instructions`. A client that defers MCP tools until it \
             searches for them knows this server only by its name and that text: without it \
             nothing says «search memory before answering, report errors here»",
    );
    assert!(
        instructions.len() <= 2000,
        "instructions are {} bytes; clients truncate server instructions, and the part that \
         gets cut is the end, where the rules for when to call go",
        instructions.len()
    );
    assert!(
        instructions.contains("cuba_faro"),
        "the one tool every session should call first must be named: {instructions}"
    );
}
