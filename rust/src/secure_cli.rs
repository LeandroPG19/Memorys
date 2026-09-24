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

fn derive_app_url(admin_url: &str, password: &str) -> String {
    let role = percent_encoded(crate::db::APP_ROLE);
    let password = percent_encoded(password);
    if let Some((scheme, rest)) = admin_url.split_once("://")
        && let Some((_creds, host)) = rest.split_once('@')
    {
        return format!("{scheme}://{role}:{password}@{host}");
    }
    format!("postgresql://{role}:{password}@127.0.0.1:5488/brain")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `secure` prints first. The two sentences are not decoration: the
    /// second one is the only place an operator who re-runs `secure` on an
    /// upgraded install is told that the password was left alone on purpose.
    #[test]
    fn each_outcome_tells_the_operator_what_happened_to_the_role() {
        assert_eq!(
            AppRole::Created.summary(),
            "Rol cuba_app creado (NOSUPERUSER, NOBYPASSRLS) con permisos de lectura/escritura \
             y la contraseña de pgpass_app.",
            "a created role has to say where its password came from, or the operator goes \
             looking for one that was never printed"
        );
        assert_eq!(
            AppRole::AlreadyExisted.summary(),
            "Rol cuba_app ya existía: se le reimpusieron NOSUPERUSER y NOBYPASSRLS y los \
             permisos. secure no cambia su contraseña: la de pgpass_app se la pone el daemon \
             cuando arranca como admin y migra.",
            "an existing role has to say its password was NOT changed: the daemon of an install \
             that upgrades is connecting with the old one"
        );
        assert_ne!(
            AppRole::Created.summary(),
            AppRole::AlreadyExisted.summary(),
            "the two outcomes answer differently about the password, so they cannot print the \
             same line"
        );
    }

    /// The URL `secure` prints for the runtime: the admin's host, port and
    /// database, with the application role and its password in place of the
    /// admin's credentials.
    #[test]
    fn the_runtime_url_keeps_the_admin_host_and_database_and_swaps_only_the_credentials() {
        assert_eq!(
            derive_app_url(
                "postgresql://cuba:admin-secret@db.planta.local:5433/brain_prod",
                "9f8e7d6c5b4a39281706f5e4d3c2b1a0",
            ),
            "postgresql://cuba_app:9f8e7d6c5b4a39281706f5e4d3c2b1a0@db.planta.local:5433/brain_prod",
            "the printed URL has to reach the same server and database the admin did, as \
             cuba_app with the pgpass_app password, and carry nothing of the admin's secret"
        );
        assert_eq!(
            derive_app_url("postgres://cuba:x@127.0.0.1:5488/brain", "0123abcd"),
            "postgres://cuba_app:0123abcd@127.0.0.1:5488/brain",
            "the scheme the operator wrote is kept as written: `postgres://` stays `postgres://`"
        );
    }

    /// Percent-decoding, the step both clients below apply to the credentials of
    /// a URL. Written here because neither `url` nor `percent-encoding` is a
    /// dependency of this crate. An escape that is not two hex digits stays as
    /// written, as the `url` crate leaves it.
    fn percent_decoded(text: &str) -> String {
        let mut bytes = Vec::with_capacity(text.len());
        let mut rest = text.as_bytes();
        while let Some((&first, tail)) = rest.split_first() {
            if first == b'%'
                && let Some(escape) = tail.get(..2)
                && let Ok(decoded) = hex::decode(escape)
            {
                bytes.extend(decoded);
                rest = &tail[2..];
            } else {
                bytes.push(first);
                rest = tail;
            }
        }
        String::from_utf8(bytes).expect("a percent-decoded password is not UTF-8")
    }

    /// The password the daemon logs in with: sqlx parses DATABASE_URL with
    /// `PgConnectOptions::from_str`. sqlx-postgres 0.8.6 has no getter for the
    /// password, so it is read back through `to_url_lossy`, which rebuilds the
    /// URL from the parsed options (`build_url` percent-encodes the stored
    /// password with NON_ALPHANUMERIC); decoding that is exactly what was stored.
    fn password_the_daemon_uses(url: &str) -> Result<Option<String>, String> {
        use sqlx::ConnectOptions;
        use std::str::FromStr;
        let options = sqlx::postgres::PgConnectOptions::from_str(url).map_err(|e| e.to_string())?;
        Ok(options.to_url_lossy().password().map(percent_decoded))
    }

    /// The password psql logs in with, following libpq's
    /// `conninfo_uri_parse_options` (src/interfaces/libpq/fe-connect.c): the
    /// credentials end at the FIRST `@` or `/`, the user name at the first `:`
    /// inside them, and the rest is the password, percent-decoded. sqlx cuts at
    /// the LAST `@` instead, so a URL both of them read the same is one whose
    /// password carries no raw `@`.
    fn password_psql_uses(url: &str) -> Option<String> {
        let (_, rest) = url.split_once("://")?;
        let end = rest.find(['@', '/'])?;
        if !rest[end..].starts_with('@') {
            return None;
        }
        let (_, password) = rest[..end].split_once(':')?;
        Some(percent_decoded(password))
    }

    /// The line `secure` prints is the one the operator pastes into the daemon's
    /// DATABASE_URL and into psql. Whatever the password holds, both have to
    /// log in with that password and not with another one. The hex password is
    /// the sane case: the test above fixes its URL byte for byte.
    #[test]
    fn the_runtime_url_hands_the_daemon_and_psql_the_password_it_was_given() {
        let wrong: Vec<String> = ["ab/cd", "p@ss", "a%41b", "us:er", "9f8e7d6c5b4a3928"]
            .into_iter()
            .filter_map(|password| {
                let url = derive_app_url(
                    "postgresql://cuba:admin-secret@db.planta.local:5433/brain_prod",
                    password,
                );
                let daemon = password_the_daemon_uses(&url);
                let psql = password_psql_uses(&url);
                let both_right =
                    daemon == Ok(Some(password.to_owned())) && psql.as_deref() == Some(password);
                (!both_right).then(|| {
                    format!(
                        "  {password:?} printed as {url} -> the daemon reads {daemon:?}, \
                         psql reads {psql:?}"
                    )
                })
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "the URL `secure` prints does not carry the password it was given, so the daemon \
             or psql logs in with another one or cannot parse the line at all. The password \
             has to be percent-encoded into the URL:\n{}",
            wrong.join("\n")
        );
    }

    /// Where sqlx sends a connection, read through its own getters: everything
    /// the URL decides except who logs in. The user comes back apart.
    fn where_the_daemon_connects(url: &str) -> Result<(String, String), String> {
        use std::str::FromStr;
        let options = sqlx::postgres::PgConnectOptions::from_str(url).map_err(|e| e.to_string())?;
        let place = format!(
            "host={:?} port={} socket={:?} database={:?} sslmode={:?} application_name={:?} \
             options={:?}",
            options.get_host(),
            options.get_port(),
            options.get_socket(),
            options.get_database(),
            options.get_ssl_mode(),
            options.get_application_name(),
            options.get_options(),
        );
        Ok((options.get_username().to_owned(), place))
    }

    /// The user and password psql logs in with, following libpq's
    /// `conninfo_uri_parse_options`: the credentials of the authority, cut as
    /// in `password_psql_uses`, and then every `key=value` of the query, each
    /// stored over what came before (`conninfo_storeval`), so a `user=` or a
    /// `password=` in the query wins.
    fn credentials_psql_uses(url: &str) -> (Option<String>, Option<String>) {
        let (mut user, mut password) = (None, None);
        let Some((_, rest)) = url.split_once("://") else {
            return (user, password);
        };
        if let Some(end) = rest.find(['@', '/'])
            && rest[end..].starts_with('@')
        {
            match rest[..end].split_once(':') {
                Some((name, secret)) => {
                    user = Some(percent_decoded(name));
                    password = Some(percent_decoded(secret));
                }
                None => user = Some(percent_decoded(&rest[..end])),
            }
        }
        let query = rest.split_once('?').map_or("", |(_, query)| query);
        for (key, value) in query.split('&').filter_map(|pair| pair.split_once('=')) {
            match key {
                "user" => user = Some(percent_decoded(value)),
                "password" => password = Some(percent_decoded(value)),
                _ => {}
            }
        }
        (user, password)
    }

    /// The `key=value` pairs of the query, in order, without the two that name
    /// who logs in.
    fn query_except_credentials(url: &str) -> Vec<&str> {
        let query = url.split_once('?').map_or("", |(_, query)| query);
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .filter(|pair| !matches!(pair.split('=').next(), Some("user" | "password")))
            .collect()
    }

    /// Every admin URL `secure` can have connected with, since it connected
    /// through sqlx: with and without credentials, a raw `@` in the admin's
    /// password (sqlx cuts the authority at the last one), with and without a
    /// port, IPv6, with and without a database, with a query, with credentials
    /// in the query, and a Unix socket both in the query and in the host.
    fn admin_urls() -> Vec<String> {
        let mut urls = Vec::new();
        for credentials in ["", "cuba@", "cuba:admin-secret@", "cuba:p@ss@"] {
            for host in ["localhost", "db.planta.local", "127.0.0.1", "[::1]"] {
                for port in ["", ":5433"] {
                    for database in ["", "/brain"] {
                        for query in [
                            "",
                            "?sslmode=require",
                            "?sslmode=disable&application_name=mi-app&connect_timeout=10",
                            "?user=cuba&password=admin-secret",
                            "?password=admin-secret&sslmode=require",
                        ] {
                            urls.push(format!(
                                "postgresql://{credentials}{host}{port}{database}{query}"
                            ));
                        }
                    }
                }
            }
        }
        urls.extend(
            [
                "postgres:///brain?host=/var/run/postgresql",
                "postgresql:///brain?host=/var/run/postgresql&sslmode=disable",
                "postgres:///brain?host=/tmp&user=cuba&password=admin-secret",
                "postgres://%2Fvar%2Frun%2Fpostgresql/brain",
                "postgres://cuba:admin-secret@%2Fvar%2Frun%2Fpostgresql:5433/brain",
            ]
            .map(String::from),
        );
        urls
    }

    /// What is wrong with where `runtime` sends the daemon: sqlx has to read it,
    /// log in as the application role and land at `admin_place`.
    fn where_the_runtime_lands(runtime: &str, admin_place: &str) -> Vec<String> {
        let mut problems = Vec::new();
        match where_the_daemon_connects(runtime) {
            Err(e) => problems.push(format!("sqlx cannot read it: {e}")),
            Ok((user, place)) => {
                if user != crate::db::APP_ROLE {
                    problems.push(format!("the daemon logs in as {user:?}"));
                }
                if place != admin_place {
                    problems.push(format!("it lands at {place}, not at {admin_place}"));
                }
            }
        }
        problems
    }

    /// What is wrong with who the daemon and psql log in as from `runtime`:
    /// both have to be the application role with `password`.
    fn who_the_runtime_logs_in_as(runtime: &str, password: &str) -> Vec<String> {
        let mut problems = Vec::new();
        let daemon = password_the_daemon_uses(runtime);
        if daemon != Ok(Some(password.to_owned())) {
            problems.push(format!("the daemon's password is {daemon:?}"));
        }
        let psql = credentials_psql_uses(runtime);
        if psql
            != (
                Some(crate::db::APP_ROLE.to_owned()),
                Some(password.to_owned()),
            )
        {
            problems.push(format!("psql logs in with {psql:?}"));
        }
        problems
    }

    /// What `runtime` changed of `admin` besides who logs in: its query, the
    /// credentials aside, or its scheme.
    fn what_else_the_runtime_changed(admin: &str, runtime: &str) -> Vec<String> {
        let scheme = |url: &str| url.split_once("://").map(|(scheme, _)| scheme.to_owned());
        let mut problems = Vec::new();
        if query_except_credentials(runtime) != query_except_credentials(admin) {
            problems.push("its query is not the admin's, credentials aside".to_owned());
        }
        if scheme(runtime) != scheme(admin) {
            problems.push("its scheme is not the admin's".to_owned());
        }
        problems
    }

    /// `secure` prints a URL for the runtime built from the admin's. It has to
    /// land where the admin's did, with nothing changed but who logs in. With
    /// no credentials in the admin URL (`postgres://localhost:5432/brain`, a
    /// socket) it used to print `127.0.0.1:5488/brain` instead: another server
    /// and another database, pasted by the operator into the daemon's config.
    #[test]
    fn the_runtime_url_lands_where_the_admin_url_did_whatever_its_form() {
        let mut wrong = Vec::new();
        for admin in admin_urls() {
            let (_, admin_place) = where_the_daemon_connects(&admin).unwrap_or_else(|e| {
                panic!(
                    "the table holds an admin URL sqlx cannot read, so `secure` could not \
                     have connected with it: {admin}: {e}"
                )
            });
            for password in ["9f8e7d6c5b4a39281706f5e4d3c2b1a0", "a&b=c+d/e@f%41"] {
                let runtime = derive_app_url(&admin, password);
                let mut problems = where_the_runtime_lands(&runtime, &admin_place);
                problems.extend(who_the_runtime_logs_in_as(&runtime, password));
                problems.extend(what_else_the_runtime_changed(&admin, &runtime));
                if !problems.is_empty() {
                    wrong.push(format!(
                        "  {admin}\n    printed {runtime}\n    {}",
                        problems.join("; ")
                    ));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "for these admin URLs the one `secure` prints does not reach the same server, port, \
             database and query as {} with the password it was given. It may only change who \
             logs in:\n{}",
            crate::db::APP_ROLE,
            wrong.join("\n")
        );
    }
}
