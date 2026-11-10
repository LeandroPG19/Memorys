//! `doctor` for the automatic LAN mode (0.29, slice A7): it understands `lan:<port>`, probes
//! the loopback half of it, and says the mode, the address the daemon serves on now and
//! whether the UDP announcement answers.

use std::path::PathBuf;

use memory_industry::doctor::{self, Check, LanFacts, ServiceFacts, Status};
use serde_json::json;

const TOKEN: &str = "an-installed-token-0123456789abcdef0123456789abcdef";

fn installed(env_text: String) -> ServiceFacts {
    ServiceFacts {
        env_path: PathBuf::from("/srv/memory-industry/memory-industry.env"),
        env_text: Some(env_text),
        process_addr: None,
        task_exists: None,
        log_tail: None,
    }
}

fn named<'a>(checks: &'a [Check], name: &str) -> &'a Check {
    checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no {name} check among {checks:?}"))
}

fn says(check: &Check, needle: &str) -> bool {
    check.detail.contains(needle) || check.hint.as_deref().is_some_and(|h| h.contains(needle))
}

#[test]
fn an_installed_lan_auto_address_is_accepted_and_called_automatic() {
    let facts = installed(format!(
        "CUBA_HTTP_ADDR=lan:8787\nCUBA_HTTP_TOKEN={TOKEN}\n"
    ));

    let checks = doctor::service_checks(&facts);
    let address = named(&checks, "service_address");

    assert_eq!(
        address.status,
        Status::Ok,
        "lan:8787 with a real token is what `setup service` writes; doctor failing it sends the \
         operator to fix a file that is right: {address:?}"
    );
    assert!(says(address, "lan:8787"), "{address:?}");
    assert!(
        says(address, "automátic"),
        "doctor names the mode: {address:?}"
    );
}

#[test]
fn a_lan_auto_install_with_a_short_token_fails_naming_the_floor() {
    let facts = installed("CUBA_HTTP_ADDR=lan:8787\nCUBA_HTTP_TOKEN=corto\n".to_string());

    let checks = doctor::service_checks(&facts);
    let address = named(&checks, "service_address");

    assert_eq!(address.status, Status::Fail, "{address:?}");
    assert!(
        says(address, "32"),
        "the daemon refuses a short token in lan mode as in a fixed LAN, and doctor says the \
         length it needs: {address:?}"
    );
}

#[test]
fn the_port_check_of_lan_auto_looks_at_loopback() {
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let check = doctor::http_port_check(Some(&format!("lan:{free}")));
    assert_eq!(
        check.status,
        Status::Ok,
        "nothing holds 127.0.0.1:{free}, and `lan:` is not a host name to resolve: {check:?}"
    );
    assert!(says(&check, &format!("127.0.0.1:{free}")), "{check:?}");

    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let check = doctor::http_port_check(Some(&format!("lan:{port}")));
    assert_eq!(check.status, Status::Warn, "{check:?}");
    assert!(
        says(&check, &format!("127.0.0.1:{port}")),
        "the port a lan daemon always holds is the loopback one: {check:?}"
    );
}

fn lan(configured: &str, health: Option<serde_json::Value>, beacon: Option<bool>) -> Check {
    doctor::lan_check(&LanFacts {
        configured: configured.to_string(),
        health,
        beacon,
    })
}

#[test]
fn an_automatic_daemon_on_its_ip_with_the_announcement_up_is_ok() {
    let check = lan(
        "lan:8787",
        Some(
            json!({"mode": "auto", "address": "192.168.1.5:8787", "since": "2026-10-10T08:00:00Z"}),
        ),
        Some(true),
    );

    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(says(&check, "automátic"), "the mode: {check:?}");
    assert!(
        says(&check, "192.168.1.5:8787"),
        "the address it serves on now: {check:?}"
    );
    assert!(
        says(&check, "UDP"),
        "whether the announcement answers: {check:?}"
    );
}

#[test]
fn an_automatic_daemon_without_a_private_ip_is_a_warning() {
    let check = lan(
        "lan:8787",
        Some(json!({"mode": "auto", "address": null, "since": "2026-10-10T08:00:00Z"})),
        Some(true),
    );

    assert_eq!(
        check.status,
        Status::Warn,
        "serving only on loopback means no other machine reaches it: {check:?}"
    );
    assert!(check.hint.is_some(), "a warning says what to do: {check:?}");
}

#[test]
fn a_silent_announcement_is_a_warning_that_names_udp() {
    let check = lan(
        "lan:8787",
        Some(
            json!({"mode": "auto", "address": "192.168.1.5:8787", "since": "2026-10-10T08:00:00Z"}),
        ),
        Some(false),
    );

    assert_eq!(check.status, Status::Warn, "{check:?}");
    assert!(says(&check, "UDP"), "{check:?}");
}

#[test]
fn a_daemon_that_does_not_answer_is_a_warning_that_names_the_configuration() {
    let check = lan("lan:8787", None, None);

    assert_eq!(check.status, Status::Warn, "{check:?}");
    assert!(says(&check, "lan:8787"), "{check:?}");
}

#[test]
fn a_fixed_lan_address_is_reported_as_fixed() {
    let check = lan(
        "192.168.0.10:8787",
        Some(
            json!({"mode": "fixed", "address": "192.168.0.10:8787", "since": "2026-10-10T08:00:00Z"}),
        ),
        Some(true),
    );

    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(says(&check, "fija"), "the mode: {check:?}");
    assert!(says(&check, "192.168.0.10:8787"), "{check:?}");
}

#[test]
fn a_loopback_install_has_nothing_to_announce_and_is_ok() {
    let check = lan(
        "127.0.0.1:8787",
        Some(
            json!({"mode": "fixed", "address": "127.0.0.1:8787", "since": "2026-10-10T08:00:00Z"}),
        ),
        None,
    );

    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(says(&check, "loopback"), "{check:?}");
}
