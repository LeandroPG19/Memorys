//! D1/D2 through the real `run_checks_with`: the unit tests prove `service_checks`
//! and `effective_http_addr` are right, and nothing else proves doctor feeds them
//! the env file the service loads instead of its own process environment.
//!
//! 192.0.2.10 is TEST-NET-1 (RFC 5737), documentation space that no machine owns,
//! so the assertions do not depend on what this box has assigned.

#[tokio::test]
#[ignore]
async fn doctor_reports_the_address_the_installed_env_file_holds_not_its_own_environment() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let home = std::env::temp_dir().join(format!("mi-doctor-home-{}", uuid::Uuid::new_v4()));
    let install = home.join(".config").join("memory-industry");
    std::fs::create_dir_all(&install).expect("scratch home is creatable");
    std::fs::write(
        install.join("memory-industry.env"),
        format!(
            "CUBA_HTTP_ADDR=192.0.2.10:8787\nCUBA_HTTP_TOKEN={}\n",
            "a".repeat(64)
        ),
    )
    .expect("scratch env file is writable");

    let saved_home = std::env::var("HOME").ok();
    let saved_addr = std::env::var("CUBA_HTTP_ADDR").ok();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::remove_var("CUBA_HTTP_ADDR");
    }
    let checks = memory_industry::doctor::run_checks_with(&pool, &url, false).await;
    unsafe {
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        if let Some(v) = saved_addr {
            std::env::set_var("CUBA_HTTP_ADDR", v);
        }
    }
    let _ = std::fs::remove_dir_all(&home);

    let address = checks
        .iter()
        .find(|c| c.name == "service_address")
        .expect("with an env file installed doctor reports service_address");
    assert!(
        address.detail.contains("192.0.2.10:8787"),
        "doctor has to report the address the installed env file holds: {}",
        address.detail
    );
    let port = checks
        .iter()
        .find(|c| c.name == "http_port")
        .expect("doctor always reports http_port");
    assert!(
        port.detail.contains("192.0.2.10:8787"),
        "the port probe has to follow the address the service will bind, not the default doctor's own environment gives: {}",
        port.detail
    );
    assert!(
        !checks.iter().any(|c| c.name == "service_task"),
        "on Linux there is no scheduler and so no service_task check: {checks:?}"
    );
}
