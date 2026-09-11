//! Contract: connect/SSE surface stays wired without needing Postgres.

#[test]
fn connect_html_is_embedded_and_mentions_ticket_flow() {
    let html = include_str!("../src/panel/connect.html");
    assert!(html.contains("MemoryIndustry"));
    assert!(html.contains("/events/ticket"));
    assert!(html.contains("EventSource"));
    assert!(html.contains("mi_token"));
}

#[test]
fn tool_catalogue_lists_coordination_tools() {
    let names: Vec<String> = memory_industry::constants::tool_definitions()
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(str::to_string))
        .collect();
    for required in ["cuba_whoami", "cuba_artefacto", "cuba_contexto"] {
        assert!(
            names.iter().any(|n| n == required),
            "missing {required} from tool catalogue"
        );
    }
}

#[test]
fn sse_ticket_round_trip() {
    let ticket = memory_industry::events::issue_ticket();
    assert!(memory_industry::events::ticket_valid(&ticket));
    assert!(!memory_industry::events::ticket_valid("not-a-ticket"));
}
