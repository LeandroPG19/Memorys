use anyhow::{Context, Result};
use sqlx::Executor;

const CREATE_APP_ROLE_SQL: &str = include_str!("../embed/create-app-role.sql");

pub async fn run_cli(_args: &[String]) -> Result<()> {
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
    println!("Verificá con: cuba-memorys doctor");
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
