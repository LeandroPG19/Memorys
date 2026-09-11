//! Every MCP tool name advertised in code must appear in the E2E suite source.
//! Soft-skips in e2e / live are forbidden (string contract).

use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn e2e_source_mentions_every_advertised_tool() {
    let e2e = std::fs::read_to_string(repo_root().join("rust/tests/e2e_all_tools.py"))
        .expect("e2e_all_tools.py");
    let live = std::fs::read_to_string(repo_root().join("scripts/mcp_live_session_test.py"))
        .expect("mcp_live_session_test.py");
    assert!(
        !e2e.contains("SKIP: Could not get"),
        "e2e soft-skips were removed — do not bring them back"
    );
    assert!(
        !live.contains("SKIP_LIVE = {\"cuba_forget\"}"),
        "live soft-skip of cuba_forget was removed"
    );

    let names: Vec<String> = memory_industry::constants::tool_definitions()
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    assert!(
        names.len() >= 20,
        "tool_definitions too small: {}",
        names.len()
    );

    let missing: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !e2e.contains(n) && !live.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "these tools are advertised but never mentioned in e2e/live sources: {:?}",
        missing
    );
}
