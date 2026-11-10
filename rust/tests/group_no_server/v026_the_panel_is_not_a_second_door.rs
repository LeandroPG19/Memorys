use std::collections::BTreeSet;

fn panel_html() -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/panel/index.html");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn panel_script() -> String {
    let html = panel_html();
    let after = html
        .split_once("<script>")
        .expect("the panel has a script block")
        .1;
    after
        .split_once("</script>")
        .expect("that block is closed")
        .0
        .to_string()
}

#[test]
fn the_panel_javascript_parses() {
    let script = panel_script();
    assert!(
        script.lines().count() > 100,
        "the extractor pulled {} lines out of the panel, so a green result would prove nothing",
        script.lines().count()
    );

    let mut file = std::env::temp_dir();
    file.push(format!("cuba-panel-check-{}.js", std::process::id()));
    std::fs::write(&file, &script).expect("write the script out");

    let checked = std::process::Command::new("node")
        .arg("--check")
        .arg(&file)
        .output();
    let _ = std::fs::remove_file(&file);

    let Ok(out) = checked else {
        eprintln!("node is not installed; the panel's syntax was not checked");
        return;
    };
    assert!(
        out.status.success(),
        "the panel is one page of hand-written JavaScript compiled into the binary with \
         include_str!, so nothing in the Rust build looks at it: a stray parenthesis ships a \
         dead page that still returns 200. That is not hypothetical — the first version of this \
         file had one, and only this check found it. node says:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_panel_pulls_everything_through_the_authenticated_endpoint() {
    let script = panel_script();

    let fetches: BTreeSet<&str> = script
        .split("fetch(\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    assert_eq!(
        fetches,
        BTreeSet::from(["/mcp"]),
        "the whole point of putting the panel inside the daemon is that it talks to the same \
         authenticated JSON-RPC endpoint as any other MCP client — same origin, same bearer, \
         same transport. A second endpoint would be a second thing to protect. Found: {fetches:?}"
    );
    assert!(
        script.contains("\"authorization\": \"Bearer \""),
        "and every one of those calls has to carry the bearer token"
    );

    assert!(
        !script.contains("localStorage"),
        "the token goes in sessionStorage, which dies with the tab. localStorage would leave \
         an admin token on disk for anything else on that origin to read"
    );

    assert!(
        !script.contains("http://") && !script.contains("https://"),
        "the page must not reach any absolute address: the CSP allows connect-src 'self' only, \
         so an external call would fail silently and look like the daemon was down"
    );
}

#[test]
fn the_panel_offers_no_button_that_destroys_anything() {
    let script = panel_script();
    for destructive in [
        "cuba_forget",
        "cuba_zafra",
        "\"prune\"",
        "confirm: true",
        "action: \"import\"",
    ] {
        assert!(
            !script.contains(destructive),
            "the panel acts only on what is reversible, and {destructive} is not: a click one \
             tap away from a cascading delete should not exist in a tab somebody leaves open. \
             Resolving a conflict, fetching from a peer and promoting a quarantined row all \
             keep their history; forgetting and pruning do not"
        );
    }

    let allowed = ["cuba_sync"];
    for call in script.split("callTool(\"").skip(1) {
        let tool = call.split('"').next().unwrap_or("");
        assert!(
            allowed.contains(&tool),
            "the panel called {tool}, which is not in the reviewed list {allowed:?}. Every write \
             the panel can make goes through a tool that already exists and already guards \
             itself — the panel adds no write path of its own"
        );
    }
}

#[test]
fn the_admin_surface_is_not_advertised_to_models() {
    let names: BTreeSet<String> = memory_industry::constants::tool_definitions()
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    assert!(
        names.len() > 20,
        "the tool list came back with {} entries, so this proves nothing",
        names.len()
    );

    for method in memory_industry::admin::METHODS {
        assert!(
            !names.contains(method),
            "{method} is in the tool catalogue. The catalogue travels in the context of every \
             model on every request, and an administration surface is not something a model \
             should be invoking — that is the entire reason these are JSON-RPC methods rather \
             than tools"
        );
        assert!(
            memory_industry::admin::is_admin_method(method),
            "{method} is listed in METHODS but is_admin_method says otherwise, so the guard in \
             the HTTP layer would let it through to the tool dispatcher"
        );
    }

    assert!(
        !memory_industry::admin::is_admin_method("cuba_faro"),
        "a matcher that says yes to everything would route ordinary tools into the admin \
         handler and refuse them for peers"
    );
}
