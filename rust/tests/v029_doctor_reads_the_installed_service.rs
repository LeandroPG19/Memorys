//! D1/D2 through the real `run_checks_with`: the unit tests prove `service_checks`
//! and `effective_http_addr` are right, and nothing else proves doctor feeds them
//! the env file the service loads instead of its own process environment.
//!
//! 192.0.2.10 is TEST-NET-1 (RFC 5737), documentation space that no machine owns,
//! so the assertions do not depend on what this box has assigned.

use memory_industry::service_apply::{CommandOutcome, CommandRunner};

/// Answers every question with success, so the `service_task` check never depends on whether the
/// machine running the test has a scheduled task of that name.
struct TaskExists;

impl CommandRunner for TaskExists {
    fn run(&mut self, _program: &str, _args: &[String]) -> std::io::Result<CommandOutcome> {
        Ok(CommandOutcome {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}

#[tokio::test]
#[ignore]
async fn doctor_reports_the_address_the_installed_env_file_holds_not_its_own_environment() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    // Both variables doctor resolves the install root from point inside the
    // scratch directory on either platform, so doctor never reads the developer's
    // own profile: HOME for Linux (and `envs::home()`, which Windows reads first),
    // LOCALAPPDATA for Windows.
    let root = std::env::temp_dir().join(format!("mi-doctor-{}", uuid::Uuid::new_v4()));
    let home = root.join("home");
    let local_app_data = root.join("local");
    // Spelled out by hand, not through `service::install_root`: with the product's
    // own function a sabotage of it would move the reader and the fixture together
    // and the test would stay green.
    let install = if cfg!(windows) {
        local_app_data.join("MemoryIndustry")
    } else {
        home.join(".config").join("memory-industry")
    };
    let env_file = install.join("memory-industry.env");
    std::fs::create_dir_all(&install).expect("scratch install root is creatable");
    std::fs::write(
        &env_file,
        format!(
            "CUBA_HTTP_ADDR=192.0.2.10:8787\nCUBA_HTTP_TOKEN={}\n",
            "a".repeat(64)
        ),
    )
    .expect("scratch env file is writable");

    let saved_home = std::env::var("HOME").ok();
    let saved_local = std::env::var("LOCALAPPDATA").ok();
    let saved_addr = std::env::var("CUBA_HTTP_ADDR").ok();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("LOCALAPPDATA", &local_app_data);
        std::env::remove_var("CUBA_HTTP_ADDR");
    }
    let checks =
        memory_industry::doctor::run_checks_with(&pool, &url, false, &mut TaskExists).await;
    unsafe {
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_local {
            Some(v) => std::env::set_var("LOCALAPPDATA", v),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        match saved_addr {
            Some(v) => std::env::set_var("CUBA_HTTP_ADDR", v),
            None => std::env::remove_var("CUBA_HTTP_ADDR"),
        }
    }
    let _ = std::fs::remove_dir_all(&root);

    let installed = checks
        .iter()
        .find(|c| c.name == "service_install")
        .expect("doctor always reports service_install");
    assert!(
        installed.detail.contains(&env_file.display().to_string()),
        "doctor has to read the env file this test installed ({}); naming another path means it is judging the installation of the machine running the test: {}",
        env_file.display(),
        installed.detail
    );
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
    if cfg!(windows) {
        assert!(
            checks.iter().any(|c| c.name == "service_task"),
            "on Windows doctor asks schtasks and always reports the answer as a service_task check, whether the task exists (ok) or not (warn); this test does not create it, so it asserts presence and not which: {checks:?}"
        );
    } else {
        assert!(
            !checks.iter().any(|c| c.name == "service_task"),
            "on Linux there is no scheduler and so no service_task check: {checks:?}"
        );
    }
}
