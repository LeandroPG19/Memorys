use std::process::Command;
use std::time::Duration;

const CONTAINER_NAME: &str = "memory-industry-db";
const LEGACY_CONTAINER_NAME: &str = "cuba-memorys-db"; // pre-rebrand
const VOLUME_NAME: &str = "memory_industry_data";
const PG_IMAGE: &str = "pgvector/pgvector:pg18";
const PG_USER: &str = "cuba";
const PG_PASSWORD: &str = "memorys2026";
const PG_DB: &str = "brain";
const PG_PORT: u16 = 5488;

pub async fn resolve_database_url() -> String {
    if let Ok(url) = std::env::var("DATABASE_URL")
        && !url.is_empty()
    {
        return url;
    }

    if crate::mode::active().is_cloud() {
        log("CUBA_MODE=red (nube) pero DATABASE_URL no está seteada.");
        log("El modo red usa una base compartida en la nube — poné la URL de tu");
        log("Postgres gestionado (Supabase/Neon/…), con TLS:");
        log("  export DATABASE_URL=\"postgresql://user:pass@host/db?sslmode=require\"");
        std::process::exit(1);
    }
    if matches!(get_container_state(), ContainerState::Running) {
        return build_url();
    }
    if matches!(get_container_state(), ContainerState::Unknown) && port_answers() {
        return build_url();
    }

    log("DATABASE_URL not set. Attempting automatic PostgreSQL setup...");

    if !is_docker_available() {
        log("");
        log("=== MemoryIndustry Setup Required ===");
        log("");
        log("PostgreSQL with pgvector is needed but DATABASE_URL is not set");
        log("and Docker is not available for automatic setup.");
        log("");
        log("Option 1: Install Docker and restart (recommended)");
        log("  https://docs.docker.com/get-docker/");
        log("");
        log("Option 2: Set up PostgreSQL manually:");
        log("  1. Install PostgreSQL 15+ with pgvector extension");
        log("  2. Create a database: CREATE DATABASE brain;");
        log("  3. Set the environment variable:");
        log("     export DATABASE_URL=\"postgresql://user:pass@localhost:5432/brain\"");
        log("");
        std::process::exit(1);
    }

    match get_container_state() {
        ContainerState::Running => {
            log("PostgreSQL container 'memory-industry-db' is already running.");
            return build_url();
        }
        ContainerState::Stopped => {
            log("Starting existing PostgreSQL container 'memory-industry-db'...");
            docker_start();
        }
        ContainerState::Unknown => {
            if port_answers() {
                log("Docker no responde, pero PostgreSQL contesta en el puerto — se usa.");
                return build_url();
            }
            log("ERROR: el daemon de Docker no responde (docker ps falló).");
            log("En Windows esto suele ser WSL2 sin arrancar: revisá que la");
            log("'Plataforma de máquina virtual' esté activada y Docker Desktop corriendo.");
            log("Diagnóstico: docker info");
            std::process::exit(1);
        }
        ContainerState::NotFound => {
            log("");
            log("=== MemoryIndustry Automatic Setup ===");
            log("");
            log("This will create a local PostgreSQL database for AI memory storage.");
            log("A Docker container 'memory-industry-db' will be created with:");
            log(&format!("  - Image:    {PG_IMAGE}"));
            log(&format!(
                "  - Port:     {PG_PORT} (mapped to container 5432)"
            ));
            log(&format!("  - Database: {PG_DB}"));
            log(&format!("  - User:     {PG_USER}"));
            log(&format!(
                "  - Volume:   {VOLUME_NAME} (persistent across restarts)"
            ));
            log("");
            log("Creating and starting PostgreSQL container...");
            docker_create_and_start();
        }
    }

    log("Waiting for PostgreSQL to accept connections...");
    if wait_for_healthy(Duration::from_secs(60)).await {
        tokio::time::sleep(Duration::from_secs(2)).await;
        log("PostgreSQL is ready.");
        log(&format!("DATABASE_URL: {}", build_url()));
        log("");
    } else {
        log("ERROR: PostgreSQL did not become ready within 60 seconds.");
        log("Check Docker logs: docker logs memory-industry-db");
        std::process::exit(1);
    }

    build_url()
}

fn log(msg: &str) {
    eprintln!("[MemoryIndustry] {msg}");
}

fn build_url() -> String {
    format!(
        "postgresql://{PG_USER}:{}@127.0.0.1:{PG_PORT}/{PG_DB}",
        resolve_password()
    )
}

fn app_password_file() -> Option<std::path::PathBuf> {
    password_file().map(|p| p.with_file_name("pgpass_app"))
}

pub fn app_role_password() -> Option<String> {
    let path = app_password_file()?;
    if let Ok(stored) = std::fs::read_to_string(&path) {
        let stored = stored.trim();
        if !stored.is_empty() {
            return Some(stored.to_string());
        }
    }
    let password = generate_password();
    if let Err(why) = store_password(&path, &password) {
        log(&format!("no pude guardar la credencial de la app: {why}"));
        return None;
    }
    Some(password)
}

/// The URL the daemon steps down to: the admin's, as the application role with
/// the pgpass_app password, built as `secure` builds the line it prints. With
/// no pgpass_app it is the admin URL itself, which create_pool reads as «stay
/// on the admin connection». This used to cut the admin URL at its first `@`
/// and splice the password in raw, so an `@` in the admin's password or any
/// `/`, `@` or `%` in pgpass_app sent the daemon somewhere else, and an admin
/// URL without credentials never stepped down at all.
pub fn runtime_database_url(admin_url: &str) -> String {
    match app_role_password() {
        Some(password) => derive_app_url(admin_url, &password),
        None => admin_url.to_string(),
    }
}

/// Credentials percent-encoded for a URL. The RFC 3986 unreserved characters
/// pass as they are, so a hex password prints unchanged; every other byte is
/// escaped. Unescaped, `/` ended the credentials for every client, `%41` was
/// read as `A`, and `@` split differently in sqlx (last `@`) and libpq (first
/// `@`), so the line that worked in the daemon failed in psql.
fn percent_encoded(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The admin URL with the application role and its password in place of the
/// admin's credentials, and nothing else changed: host, port, database and
/// query stay as the operator wrote them. `secure` prints it and the daemon
/// steps down to it (runtime_database_url): one function, so the two cannot
/// disagree about where cuba_app logs in.
///
/// The authority ends at the first `/`, `?` or `#` and its credentials at the
/// LAST `@` inside it, which is how sqlx (the `url` crate) read the admin URL
/// both of them connected with. A `user=` or `password=` in the query is
/// dropped: both clients let it override the authority. With no host (a Unix
/// socket named by `?host=`), the credentials go in the query, because sqlx
/// refuses credentials in front of an empty host. This used to split at the
/// first `@` and, with no `@` at all, print 127.0.0.1:5488/brain whatever the
/// admin had.
pub(crate) fn derive_app_url(admin_url: &str, password: &str) -> String {
    let role = percent_encoded(crate::db::APP_ROLE);
    let password = percent_encoded(password);
    let Some((scheme, rest)) = admin_url.split_once("://") else {
        return format!("postgresql://{role}:{password}@127.0.0.1:5488/brain");
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path_and_query) = rest.split_at(authority_end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (path, query) = path_and_query
        .split_once('?')
        .unwrap_or((path_and_query, ""));
    let mut params: Vec<String> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !matches!(pair.split('=').next(), Some("user" | "password")))
        .map(str::to_owned)
        .collect();
    if host.is_empty() {
        params.extend([format!("user={role}"), format!("password={password}")]);
        return format!("{scheme}://{path}?{}", params.join("&"));
    }
    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    format!("{scheme}://{role}:{password}@{host}{path}{query}")
}

pub fn listen_address() -> String {
    std::env::var("CUBA_PG_BIND")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

fn password_file() -> Option<std::path::PathBuf> {
    // `.ok()?`: `resolve_password` reads None as «use the compiled-in
    // constant». An error here would stop `setup` on a machine that has simply
    // never defined either name.
    let cache = crate::envs::home().ok()?.join(".cache");
    let preferred = cache.join("memory-industry").join("pgpass");
    let legacy = cache.join("cuba-memorys").join("pgpass");
    if preferred.exists() || !legacy.exists() {
        Some(preferred)
    } else {
        Some(legacy)
    }
}

fn generate_password() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn store_password(path: &std::path::Path, password: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, password)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn resolve_password() -> String {
    let Some(path) = password_file() else {
        return PG_PASSWORD.to_string();
    };

    if let Ok(stored) = std::fs::read_to_string(&path) {
        let stored = stored.trim();
        if !stored.is_empty() {
            return stored.to_string();
        }
    }

    let password = if matches!(
        get_container_state(),
        ContainerState::Running | ContainerState::Stopped
    ) {
        log("contenedor preexistente: conservo la credencial anterior para no dejarte");
        log("fuera de tu propia base. Rotala con: memory-industry setup --rotate-password");
        PG_PASSWORD.to_string()
    } else {
        generate_password()
    };

    if let Err(why) = store_password(&path, &password) {
        log(&format!("no pude guardar la credencial en {path:?}: {why}"));
    }
    password
}

fn is_docker_available() -> bool {
    Command::new("docker")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn port_answers() -> bool {
    use std::net::TcpStream;
    use std::time::Duration;
    let addr = format!("127.0.0.1:{PG_PORT}");
    TcpStream::connect_timeout(
        &addr.parse().expect("host:port literal is valid"),
        Duration::from_millis(500),
    )
    .is_ok()
}

fn name_already_in_use() -> bool {
    matches!(
        get_container_state(),
        ContainerState::Running | ContainerState::Stopped
    )
}

enum ContainerState {
    Running,
    Stopped,
    NotFound,
    Unknown,
}

fn get_container_state() -> ContainerState {
    let primary = inspect_container(CONTAINER_NAME);
    if !matches!(primary, ContainerState::NotFound) {
        return primary;
    }
    inspect_container(LEGACY_CONTAINER_NAME)
}

fn inspect_container(name: &str) -> ContainerState {
    let output = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name=^{name}$"),
            "--format",
            "{{.Status}}",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => parse_container_status(&String::from_utf8_lossy(&o.stdout)),
        _ => ContainerState::Unknown,
    }
}

fn active_container_name() -> &'static str {
    match inspect_container(CONTAINER_NAME) {
        ContainerState::NotFound => {
            if matches!(
                inspect_container(LEGACY_CONTAINER_NAME),
                ContainerState::Running | ContainerState::Stopped
            ) {
                LEGACY_CONTAINER_NAME
            } else {
                CONTAINER_NAME
            }
        }
        _ => CONTAINER_NAME,
    }
}

fn parse_container_status(stdout: &str) -> ContainerState {
    let status = stdout.trim();
    if status.is_empty() {
        ContainerState::NotFound
    } else if status.starts_with("Up") {
        ContainerState::Running
    } else {
        ContainerState::Stopped
    }
}

fn docker_start() {
    let name = active_container_name();
    let status = Command::new("docker").args(["start", name]).status();

    if let Ok(s) = status
        && !s.success()
    {
        log(&format!(
            "ERROR: Failed to start container. Run: docker start {name}"
        ));
        std::process::exit(1);
    }
}

fn docker_create_and_start() {
    let status = Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            CONTAINER_NAME,
            "-e",
            &format!("POSTGRES_USER={PG_USER}"),
            "-e",
            &format!("POSTGRES_PASSWORD={}", resolve_password()),
            "-e",
            &format!("POSTGRES_DB={PG_DB}"),
            "-p",
            &format!("{}:{PG_PORT}:5432", listen_address()),
            "-v",
            &format!("{VOLUME_NAME}:/var/lib/postgresql"),
            "--health-cmd",
            &format!("pg_isready -U {PG_USER} -d {PG_DB}"),
            "--health-interval",
            "2s",
            "--health-timeout",
            "3s",
            "--health-retries",
            "15",
            "--restart",
            "unless-stopped",
            PG_IMAGE,
        ])
        .status();

    match status {
        Ok(s) if s.success() => {
            log("Container created successfully.");
        }
        _ if name_already_in_use() => {
            log("El contenedor ya existía — se reutiliza en vez de recrearlo.");
            docker_start();
        }
        _ => {
            log("ERROR: Failed to create Docker container.");
            log("Make sure Docker is running: docker info");
            std::process::exit(1);
        }
    }
}

async fn wait_for_healthy(timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(500);

    while start.elapsed() < timeout {
        let ok = Command::new("docker")
            .args([
                "exec",
                active_container_name(),
                "pg_isready",
                "-U",
                PG_USER,
                "-d",
                PG_DB,
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if ok {
            return true;
        }

        tokio::time::sleep(poll_interval).await;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envs::{ScopedEnv, scratch_root};

    #[test]
    fn container_status_is_read_correctly() {
        assert!(
            matches!(
                parse_container_status("Up 2 hours (healthy)"),
                ContainerState::Running
            ),
            "«Up …» es Running"
        );
        assert!(matches!(
            parse_container_status("Up 3 seconds"),
            ContainerState::Running
        ));
        assert!(
            matches!(
                parse_container_status("Exited (0) 5 minutes ago"),
                ContainerState::Stopped
            ),
            "«Exited …» es Stopped, no NotFound: existe, hay que arrancarlo"
        );
        assert!(matches!(
            parse_container_status("Created"),
            ContainerState::Stopped
        ));
        assert!(
            matches!(parse_container_status(""), ContainerState::NotFound),
            "sin fila, el nombre no existe"
        );
        assert!(
            matches!(parse_container_status("  \n"), ContainerState::NotFound),
            "solo espacios = ninguna fila"
        );
    }

    /// Where the generated Postgres password is kept, pinned before this home
    /// resolution moves to `envs::home()`.
    ///
    /// `resolve_password` falls back to the compiled-in constant on `None`, so
    /// a site that resolved a different root would not fail: it would generate
    /// a second password, store it somewhere else, and leave the operator with
    /// a container that no longer accepts the URL the daemon builds.
    #[tokio::test]
    async fn the_postgres_password_file_hangs_off_the_home_and_is_none_without_one() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let root = scratch_root("pgpass");
        let cache = root.join(".cache");
        let preferred = cache.join("memory-industry").join("pgpass");
        let legacy = cache.join("cuba-memorys").join("pgpass");
        // Never created: if USERPROFILE were read first the block below would
        // resolve under it instead, and the assertion would name that path.
        let never_created = root.join("userprofile-only");

        {
            let _h = ScopedEnv::set("HOME", &root.display().to_string());
            let _u = ScopedEnv::set("USERPROFILE", &never_created.display().to_string());
            assert_eq!(
                password_file().as_ref(),
                Some(&preferred),
                "HOME is read first, and with nothing stored yet the answer is the documented \
                 path"
            );

            std::fs::create_dir_all(cache.join("cuba-memorys"))
                .expect("the test owns this directory");
            std::fs::write(&legacy, "stored-before-the-rename").expect("temp dir is writable");
            assert_eq!(
                password_file().as_ref(),
                Some(&legacy),
                "the password an install generated before the rename is the one its container \
                 was created with. Preferring the new path here stores a second password the \
                 running Postgres does not accept"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::set("USERPROFILE", &root.display().to_string());
            assert_eq!(
                password_file().as_ref(),
                Some(&legacy),
                "the same answer from USERPROFILE, which is the only one of the two a Windows \
                 service ever has"
            );
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::cleared("USERPROFILE");
            assert_eq!(
                password_file(),
                None,
                "no home is «nowhere to keep it», and `resolve_password` reads that as «use \
                 the compiled-in constant». An error here would stop `setup` on a machine that \
                 has simply never defined either name"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The URL the daemon steps down to when it starts as admin: the admin's,
    /// as cuba_app with the password in pgpass_app. It used to be built apart
    /// from the line `secure` prints: cut at the first `@` of the admin URL and
    /// with the password spliced in raw. That sent the daemon to host `ss` for
    /// an admin whose password holds an `@`, broke on a `/` or `%` in
    /// pgpass_app, and never stepped down from an admin URL without credentials.
    #[tokio::test]
    async fn the_daemon_steps_down_as_cuba_app_to_the_admin_server_whatever_its_url_holds() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;

        let root = scratch_root("pgpass-app");
        let cache = root.join(".cache").join("memory-industry");
        std::fs::create_dir_all(&cache).expect("the test owns this directory");
        let stored = cache.join("pgpass_app");
        std::fs::write(&stored, "p@ss/w0rd%41\n").expect("temp dir is writable");

        let table = [
            (
                "postgresql://cuba:admin-secret@db.planta.local:5433/brain",
                "postgresql://cuba_app:p%40ss%2Fw0rd%2541@db.planta.local:5433/brain",
                "the admin's host, port and database, as cuba_app with pgpass_app \
                 percent-encoded: raw, its `/` ends the credentials and its `%41` reads as `A`",
            ),
            (
                "postgresql://cuba:p@ss@db.planta.local:5433/brain",
                "postgresql://cuba_app:p%40ss%2Fw0rd%2541@db.planta.local:5433/brain",
                "an `@` in the admin's password: sqlx connected the admin at the LAST `@`, so \
                 the server is db.planta.local, not `ss`",
            ),
            (
                "postgres://localhost:5432/brain",
                "postgres://cuba_app:p%40ss%2Fw0rd%2541@localhost:5432/brain",
                "an admin URL with no credentials steps down too, to the same server and \
                 database; handed back unchanged, the daemon stays superuser in silence",
            ),
        ];
        {
            let _h = ScopedEnv::set("HOME", &root.display().to_string());
            for (admin, expected, why) in table {
                assert_eq!(runtime_database_url(admin), expected, "{why}");
            }
        }
        {
            let _h = ScopedEnv::cleared("HOME");
            let _u = ScopedEnv::cleared("USERPROFILE");
            let admin = "postgresql://cuba:admin-secret@db.planta.local:5433/brain";
            assert_eq!(
                runtime_database_url(admin),
                admin,
                "with no home there is no pgpass_app, and the admin URL comes back as it is: \
                 create_pool reads that as «stay on the admin connection»"
            );
        }

        let _ = std::fs::remove_dir_all(&root);
    }
}
