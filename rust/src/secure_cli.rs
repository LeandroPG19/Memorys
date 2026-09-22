use anyhow::{Context, Result};
use sqlx::Executor;

const CREATE_APP_ROLE_SQL: &str = include_str!("../embed/create-app-role.sql");

pub async fn run_cli(args: &[String]) -> Result<()> {
    // This command runs DDL as a superuser, so an argument it does not
    // understand is refused instead of ignored: ignoring them is how
    // `secure --help` used to create the role.
    match args.first().map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            eprintln!(
                "usage: memory-industry secure\n\n\
                     Crea el rol de app para que RLS y el audit append-only apliquen de verdad.\n\
                     No acepta argumentos: actúa sobre DATABASE_URL, que tiene que ser de un\n\
                     rol superuser (el owner, cuba); con otro rol se niega sin tocar nada.\n\n\
                     Ejecuta embed/create-app-role.sql, idempotente:\n\
                     \x20 - si no existe, CREATE ROLE cuba_app LOGIN con la contraseña fija\n\
                     \x20   app2026, NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS;\n\
                     \x20   si existe, le reimpone esos atributos sin cambiar la contraseña.\n\
                     \x20 - GRANT USAGE en el schema public; SELECT, INSERT, UPDATE, DELETE en\n\
                     \x20   todas sus tablas; USAGE, SELECT en sus secuencias; EXECUTE en sus\n\
                     \x20   funciones. Ni DDL ni ownership.\n\
                     \x20 - los mismos privilegios por defecto sobre lo que cuba cree después.\n\n\
                     Al terminar imprime el DATABASE_URL de cuba_app para el runtime."
            );
            return Ok(());
        }
        Some(other) => anyhow::bail!(
            "argumento desconocido `{other}`: `secure` no acepta argumentos y no \
             ejecuta nada si recibe uno (probá --help)"
        ),
    }

    let admin_url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&admin_url)
        .await
        .context("conectando como admin para crear el rol de app")?;

    let is_super: Option<(bool,)> =
        sqlx::query_as("SELECT rolsuper FROM pg_roles WHERE rolname = current_user")
            .fetch_optional(&pool)
            .await?;
    if !matches!(is_super, Some((true,))) {
        anyhow::bail!(
            "`secure` tiene que correr como un rol admin (superuser) para crear cuba_app.\n\
             Corré esto con el DATABASE_URL del owner (cuba), no del rol de app."
        );
    }

    pool.execute(CREATE_APP_ROLE_SQL)
        .await
        .context("ejecutando scripts/create-app-role.sql")?;

    let app_url = derive_app_url(&admin_url);

    println!("Rol cuba_app creado (NOSUPERUSER, NOBYPASSRLS) con permisos de lectura/escritura.");
    println!();
    println!("Ahora RLS y el audit append-only sí aplican. Apuntá el runtime al rol de app:");
    println!();
    println!("  export DATABASE_URL=\"{app_url}\"");
    println!("  export CUBA_SKIP_MIGRATIONS=1");
    println!();
    println!("Las migraciones ya corrieron como admin; el runtime como cuba_app no las necesita.");
    println!("Verificá con: memory-industry doctor");
    Ok(())
}

fn derive_app_url(admin_url: &str) -> String {
    if let Some((scheme, rest)) = admin_url.split_once("://")
        && let Some((_creds, host)) = rest.split_once('@')
    {
        return format!("{scheme}://cuba_app:app2026@{host}");
    }
    "postgresql://cuba_app:app2026@127.0.0.1:5488/brain".to_string()
}
