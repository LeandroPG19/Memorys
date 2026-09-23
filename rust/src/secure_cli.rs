use anyhow::{Context, Result};
use sqlx::PgPool;

const CREATE_APP_ROLE_SQL: &str = include_str!("../embed/create-app-role.sql");

#[derive(Debug, PartialEq, Eq)]
pub enum AppRole {
    Created,
    AlreadyExisted,
}

impl AppRole {
    fn summary(&self) -> &'static str {
        match self {
            AppRole::Created => {
                "Rol cuba_app creado (NOSUPERUSER, NOBYPASSRLS) con permisos de lectura/escritura \
                 y la contraseña de pgpass_app."
            }
            AppRole::AlreadyExisted => {
                "Rol cuba_app ya existía: se le reimpusieron NOSUPERUSER y NOBYPASSRLS y los \
                 permisos. secure no cambia su contraseña: la de pgpass_app se la pone el daemon \
                 cuando arranca como admin y migra."
            }
        }
    }
}

/// Creates `role` with `fresh_password`, or, when it already exists,
/// reimposes its attributes and leaves its password alone: the daemon of an
/// install that upgrades is connecting with that one. Grants run either way.
///
/// sqlx::Executor's own methods, not the query types' generic async fns: those return
/// a BoxFuture that is already Send, and without it this future was !Send (v044 spawns it).
pub async fn ensure_app_role(pool: &PgPool, role: &str, fresh_password: &str) -> Result<AppRole> {
    let mut tx = pool.begin().await?;
    let exists = sqlx::query("SELECT 1 FROM pg_roles WHERE rolname = $1").bind(role);
    let existed = sqlx::Executor::fetch_optional(&mut *tx, exists)
        .await?
        .is_some();
    crate::db::bind_app_role(&mut tx, role, fresh_password).await?;
    sqlx::Executor::execute(&mut *tx, sqlx::raw_sql(CREATE_APP_ROLE_SQL))
        .await
        .context("ejecutando embed/create-app-role.sql")?;
    tx.commit().await?;
    Ok(if existed {
        AppRole::AlreadyExisted
    } else {
        AppRole::Created
    })
}

pub async fn run_cli(args: &[String]) -> Result<()> {
    // This command runs DDL as a superuser, so an argument it does not
    // understand is refused instead of ignored: ignoring them is how
    // `secure --help` used to create the role.
    let first = args.first().map(String::as_str);
    if crate::cli::asks_for_help(first) {
        eprintln!(
            "usage: memory-industry secure\n\n\
                     Crea el rol de app para que RLS y el audit append-only apliquen de verdad.\n\
                     No acepta argumentos: actúa sobre DATABASE_URL, que tiene que ser de un\n\
                     rol superuser (el owner, cuba); con otro rol se niega sin tocar nada.\n\n\
                     Ejecuta embed/create-app-role.sql, idempotente:\n\
                     \x20 - si no existe, CREATE ROLE cuba_app LOGIN NOSUPERUSER NOCREATEDB\n\
                     \x20   NOCREATEROLE NOBYPASSRLS, con la contraseña aleatoria que el daemon\n\
                     \x20   lee de ~/.cache/memory-industry/pgpass_app (la genera si falta);\n\
                     \x20   si existe, le reimpone esos atributos sin cambiar la contraseña.\n\
                     \x20 - GRANT USAGE en el schema public; SELECT, INSERT, UPDATE, DELETE en\n\
                     \x20   todas sus tablas; USAGE, SELECT en sus secuencias; EXECUTE en sus\n\
                     \x20   funciones. Ni DDL ni ownership.\n\
                     \x20 - los mismos privilegios por defecto sobre lo que cuba cree después.\n\n\
                     Al terminar imprime el DATABASE_URL de cuba_app para el runtime, con esa\n\
                     contraseña ya puesta."
        );
        return Ok(());
    }
    if let Some(other) = first {
        anyhow::bail!(
            "argumento desconocido `{other}`: `secure` no acepta argumentos y no \
             ejecuta nada si recibe uno (probá --help)"
        );
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

    let password = crate::setup::app_role_password().context(
        "no pude leer ni crear ~/.cache/memory-industry/pgpass_app, que es de donde el daemon \
         lee la contraseña de cuba_app; sin ella no se crea el rol",
    )?;
    let outcome = ensure_app_role(&pool, crate::db::APP_ROLE, &password).await?;

    let app_url = derive_app_url(&admin_url, &password);

    println!("{}", outcome.summary());
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

fn derive_app_url(admin_url: &str, password: &str) -> String {
    let role = crate::db::APP_ROLE;
    if let Some((scheme, rest)) = admin_url.split_once("://")
        && let Some((_creds, host)) = rest.split_once('@')
    {
        return format!("{scheme}://{role}:{password}@{host}");
    }
    format!("postgresql://{role}:{password}@127.0.0.1:5488/brain")
}
