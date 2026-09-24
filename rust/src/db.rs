use anyhow::{Context, Result};
use sqlx::migrate::MigrateError;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, PgPool};
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

fn connect_options(database_url: &str) -> Result<PgConnectOptions> {
    Ok(PgConnectOptions::from_str(database_url)
        .context("invalid DATABASE_URL")?
        .log_statements(tracing::log::LevelFilter::Debug)
        .log_slow_statements(tracing::log::LevelFilter::Warn, Duration::from_secs(1)))
}

pub const APP_ROLE: &str = "cuba_app";

/// Gives `role` the password `password` when the role exists; one that does
/// not is left for `secure` to create. A failure is logged, not returned: the
/// daemon still runs, on the admin connection.
///
/// The role comes in by name so a test can hand it one of its own: roles
/// belong to the whole server, and APP_ROLE is the one the user's daemon logs
/// in as. init_schema passes APP_ROLE and the password in pgpass_app.
///
/// sqlx::Executor's own method for the lookup, as in write_app_role_password:
/// with the role borrowed instead of 'static, the query type's generic async fn
/// is the shape that left create_pool's future !Send (v043 spawns it).
pub async fn provision_app_role(pool: &PgPool, role: &str, password: &str) {
    let lookup = sqlx::query("SELECT 1 FROM pg_roles WHERE rolname = $1").bind(role);
    let exists = sqlx::Executor::fetch_optional(pool, lookup)
        .await
        .ok()
        .flatten();
    if exists.is_none() {
        return;
    }

    match write_app_role_password(pool, role, password).await {
        Ok(()) => tracing::info!(role, "application role provisioned"),
        Err(why) => {
            tracing::warn!(error = %why, "could not set the application role password")
        }
    }
}

/// APP_ROLE onto the password in this machine's pgpass_app, the one the daemon
/// is about to log in with. With no home to keep pgpass_app in there is none,
/// and the role is left as it is.
async fn provision_app_role_from_pgpass(pool: &PgPool) {
    if let Some(password) = crate::setup::app_role_password() {
        provision_app_role(pool, APP_ROLE, &password).await;
    }
}

const ALTER_APP_ROLE_PASSWORD: &str = "DO $$ BEGIN EXECUTE format('ALTER ROLE %I PASSWORD %L', \
     current_setting('memory_industry.app_role'), \
     current_setting('memory_industry.app_password')); END $$";

// sqlx::Executor's own methods, not RawSql::execute: those return a BoxFuture that is
// already Send, while the generic async fn left create_pool's future !Send (v043 spawns it).
async fn write_app_role_password(pool: &PgPool, role: &str, password: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    bind_app_role(&mut tx, role, password).await?;
    sqlx::Executor::execute(&mut *tx, sqlx::raw_sql(ALTER_APP_ROLE_PASSWORD)).await?;
    tx.commit().await
}

/// The one way this crate hands a role name and a password to SQL. A `DO`
/// block takes no bind parameters, so both travel as transaction-local
/// settings bound here and reach the statement through `format()` with `%I`
/// and `%L`. The password used to be spliced into `ALTER ROLE` with
/// `format!` behind an alphanumeric filter, and that statement text is what
/// the statement logger writes out.
///
/// What travels is the password's SCRAM-SHA-256 verifier with a fresh salt,
/// never the password: log_statement logs bound parameters too, and
/// pg_stat_statements keeps the `ALTER ROLE` the `DO` block runs. PostgreSQL
/// stores a verifier handed to `PASSWORD` as it is (CREATE ROLE: «If the
/// presented password string is already in MD5-encrypted or SCRAM-encrypted
/// format, then it is stored as-is regardless of password_encryption»).
pub(crate) async fn bind_app_role(
    conn: &mut sqlx::PgConnection,
    role: &str,
    password: &str,
) -> sqlx::Result<()> {
    // A v4 uuid is 16 bytes from the OS generator, 122 of them random: the
    // salt length PostgreSQL uses, without adding `rand` to the crate.
    let verifier = scram_sha_256_verifier(password, uuid::Uuid::new_v4().as_bytes());
    let settings = sqlx::query(
        "SELECT set_config('memory_industry.app_role', $1, true), \
                set_config('memory_industry.app_password', $2, true)",
    )
    .bind(role)
    .bind(verifier);
    sqlx::Executor::execute(conn, settings).await.map(drop)
}

/// PostgreSQL's own default for `scram_iterations`.
const SCRAM_ITERATIONS: u32 = 4096;

/// The SCRAM-SHA-256 verifier of `password` with `salt` (RFC 5802 §3,
/// RFC 7677), in the form pg_authid keeps and `PASSWORD '...'` takes:
/// `SCRAM-SHA-256$<iterations>:<salt>$<StoredKey>:<ServerKey>`, each part in
/// Base64. The password goes in as its bytes. PostgreSQL runs SASLprep on it
/// first, which leaves ASCII untouched, and the password `setup` generates is
/// hex; a non-ASCII one written into pgpass_app by hand may not log in.
pub fn scram_sha_256_verifier(password: &str, salt: &[u8]) -> String {
    use sha2::Digest;
    let salted = pbkdf2_hmac_sha256(password.as_bytes(), salt, SCRAM_ITERATIONS);
    let stored_key = sha2::Sha256::digest(hmac_sha256(&salted, b"Client Key"));
    let server_key = hmac_sha256(&salted, b"Server Key");
    format!(
        "SCRAM-SHA-256${SCRAM_ITERATIONS}:{}${}:{}",
        base64(salt),
        base64(&stored_key),
        base64(&server_key)
    )
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    let mut mac =
        <Hmac<sha2::Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts a key of any length");
    mac.update(message);
    let mut out = [0; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// PBKDF2 (RFC 8018 §5.2) with HMAC-SHA-256, first block only: SCRAM wants a
/// 32-byte key and that is exactly one block.
fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut block = hmac_sha256(password, &[salt, &1u32.to_be_bytes()[..]].concat());
    let mut key = block;
    for _ in 1..iterations {
        block = hmac_sha256(password, &block);
        key.iter_mut().zip(block).for_each(|(k, b)| *k ^= b);
    }
    key
}

/// Standard Base64 with padding (RFC 4648 §4), the encoding of the verifier.
/// Written here because `base64` is not a dependency of this crate.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |group, (i, &b)| {
            group | (u32::from(b) << (16 - 8 * i))
        });
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(
                    ALPHABET[((group >> (18 - 6 * i)) & 63) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub async fn is_superuser(pool: &PgPool) -> Option<bool> {
    sqlx::query_scalar("SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

const DEFAULT_RANDOM_PAGE_COST: f64 = 1.1;
const DEFAULT_IO_CONCURRENCY: u32 = 200;

pub fn random_page_cost() -> String {
    std::env::var("CUBA_RANDOM_PAGE_COST")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| (0.1..=10.0).contains(v))
        .unwrap_or(DEFAULT_RANDOM_PAGE_COST)
        .to_string()
}

pub fn effective_io_concurrency() -> String {
    std::env::var("CUBA_IO_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|v| *v <= 1000)
        .unwrap_or(DEFAULT_IO_CONCURRENCY)
        .to_string()
}

fn pool_options() -> PgPoolOptions {
    let node_name = std::env::var("CUBA_NODE_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_default();

    PgPoolOptions::new()
        .max_connections(crate::resources::db_max_connections())
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(600))
        .max_lifetime(Duration::from_secs(1800))
        .after_connect(move |conn, _meta| {
            let node = node_name.clone();
            Box::pin(async move {
                sqlx::query("SET timezone TO 'UTC'")
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("SET hnsw.ef_search = 100")
                    .execute(&mut *conn)
                    .await
                    .ok();
                sqlx::query("SELECT set_config('random_page_cost', $1, false)")
                    .bind(random_page_cost())
                    .execute(&mut *conn)
                    .await
                    .ok();
                sqlx::query("SELECT set_config('effective_io_concurrency', $1, false)")
                    .bind(effective_io_concurrency())
                    .execute(&mut *conn)
                    .await
                    .ok();
                sqlx::query("SELECT set_config('app.current_project', '', false)")
                    .execute(&mut *conn)
                    .await
                    .ok();
                sqlx::query("SELECT set_config('cuba.node_name', $1, false)")
                    .bind(&node)
                    .execute(&mut *conn)
                    .await
                    .ok();
                Ok(())
            })
        })
        .before_acquire(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SELECT set_config('app.current_project', $1, false)")
                    .bind(crate::project::rls_scope())
                    .execute(&mut *conn)
                    .await?;
                Ok(true)
            })
        })
        .after_release(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SELECT set_config('app.current_project', '', false)")
                    .execute(&mut *conn)
                    .await
                    .ok();
                Ok(true)
            })
        })
}

pub async fn create_pool(database_url: &str) -> Result<PgPool> {
    let pool = create_admin_pool(database_url).await?;

    match downgrade_to_app_role(database_url).await {
        Some(app_pool) => {
            pool.close().await;
            Ok(app_pool)
        }
        None => Ok(pool),
    }
}

/// The connection `database_url` names, migrated, and never stepped down to
/// the application role. `secure` needs it: it creates and alters that role,
/// so it has to stay superuser, and create_pool hands back cuba_app's pool as
/// soon as cuba_app logs in.
pub async fn create_admin_pool(database_url: &str) -> Result<PgPool> {
    let pool = pool_options()
        .min_connections(1)
        .connect_with(connect_options(database_url)?)
        .await
        .context("failed to connect to PostgreSQL")?;

    tracing::info!("connected to PostgreSQL");

    init_schema(&pool).await?;
    Ok(pool)
}

async fn downgrade_to_app_role(admin_url: &str) -> Option<PgPool> {
    if matches!(
        std::env::var("CUBA_APP_ROLE").as_deref(),
        Ok("0") | Ok("off") | Ok("false")
    ) {
        return None;
    }

    let runtime_url = crate::setup::runtime_database_url(admin_url);
    if runtime_url == admin_url {
        return None;
    }

    let options = connect_options(&runtime_url).ok()?;
    let pool = pool_options()
        .min_connections(1)
        .connect_with(options)
        .await;

    match pool {
        Ok(pool) => match is_superuser(&pool).await {
            Some(false) => {
                tracing::info!(
                    role = APP_ROLE,
                    "runtime downgraded to a non-superuser role — RLS and the append-only \
                     audit trigger now actually apply"
                );
                Some(pool)
            }
            _ => {
                pool.close().await;
                None
            }
        },
        Err(why) => {
            tracing::warn!(
                error = %why,
                role = APP_ROLE,
                "could not connect as the application role — staying on the admin connection"
            );
            None
        }
    }
}

pub fn create_lazy_pool(database_url: &str) -> PgPool {
    let options = connect_options(database_url).unwrap_or_else(|_| {
        PgConnectOptions::new()
            .log_statements(tracing::log::LevelFilter::Debug)
            .log_slow_statements(tracing::log::LevelFilter::Warn, Duration::from_secs(1))
    });
    pool_options().connect_lazy_with(options)
}

static NODE_ID: std::sync::OnceLock<uuid::Uuid> = std::sync::OnceLock::new();

pub async fn node_id(pool: &PgPool) -> Result<uuid::Uuid> {
    if let Some(id) = NODE_ID.get() {
        return Ok(*id);
    }
    let id: uuid::Uuid = sqlx::query_scalar("SELECT node_id FROM brain_node_identity")
        .fetch_one(pool)
        .await
        .context("brain_node_identity holds exactly one row, created by migration 0046")?;
    Ok(*NODE_ID.get_or_init(|| id))
}

pub async fn init_schema(pool: &PgPool) -> Result<()> {
    let skip = std::env::var("CUBA_SKIP_MIGRATIONS")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false);

    if skip {
        let applied: Option<(i64,)> =
            sqlx::query_as("SELECT MAX(version) FROM _sqlx_migrations WHERE success = TRUE")
                .fetch_optional(pool)
                .await
                .context(
                    "CUBA_SKIP_MIGRATIONS is set but _sqlx_migrations is unreadable — \
             run migrations once as an admin role before starting the app",
                )?;
        let embedded = MIGRATOR
            .iter()
            .map(|m| m.version)
            .max()
            .expect("the binary embeds at least one migration");
        match applied.map(|(v,)| v) {
            Some(v) if v < embedded => anyhow::bail!(
                "this database is at migration {v} and this binary expects {embedded}. \
                 CUBA_SKIP_MIGRATIONS is set, so nothing will bring it forward and the \
                 first query naming a column added after {v} takes down whatever \
                 transaction it is in — an import gets through hundreds of rows before \
                 dying. Run the binary once with CUBA_SKIP_MIGRATIONS unset under an \
                 admin role, then start it again."
            ),
            Some(v) => tracing::warn!(
                latest_migration = v,
                "CUBA_SKIP_MIGRATIONS active — skipping migrator (non-superuser runtime)"
            ),
            None => anyhow::bail!(
                "CUBA_SKIP_MIGRATIONS is set but no migrations are applied — \
                 initialize the database with an admin role first"
            ),
        }
    } else {
        // Boxed so rustc proves Send here, with concrete lifetimes: inferred, Migrator::run
        // left create_pool's future !Send (v043 spawns it) once provision_app_role took a tx.
        let migrate: Pin<Box<dyn Future<Output = Result<(), MigrateError>> + Send + '_>> =
            Box::pin(MIGRATOR.run(pool));
        migrate.await.context("failed to run sqlx migrations")?;

        tracing::info!("sqlx migrations applied");
        provision_app_role_from_pgpass(pool).await;
    }

    sqlx::query("SET timezone TO 'UTC'")
        .execute(pool)
        .await
        .context("failed to set timezone to UTC")?;

    tracing::info!("schema initialized (timezone=UTC)");

    let pgvector_check: Option<(String,)> =
        sqlx::query_as("SELECT extname::text FROM pg_extension WHERE extname = 'vector'")
            .fetch_optional(pool)
            .await?;

    if pgvector_check.is_some() {
        tracing::info!("pgvector extension detected");
        sqlx::query("SET hnsw.ef_search = 100")
            .execute(pool)
            .await
            .ok();
    } else {
        tracing::warn!("pgvector extension NOT found — vector search disabled");
    }

    Ok(())
}

pub async fn assert_embedding_dim(pool: &PgPool) -> Result<()> {
    if !crate::embeddings::onnx::is_model_loaded() {
        return Ok(());
    }
    let runtime_dim = crate::embeddings::onnx::embedding_dim();
    let expected = format!("vector({runtime_dim})");

    let columns: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod)::text
         FROM pg_attribute a
         JOIN pg_class c ON c.oid = a.attrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public'
           AND c.relkind = 'r'
           AND a.attnum > 0 AND NOT a.attisdropped
           AND format_type(a.atttypid, a.atttypmod) LIKE 'vector(%'
         ORDER BY c.relname",
    )
    .fetch_all(pool)
    .await
    .context("reading the vector column types")?;

    if columns.is_empty() {
        return Ok(());
    }

    let mismatched: Vec<String> = columns
        .iter()
        .filter(|(_, _, ty)| ty != &expected)
        .map(|(t, c, ty)| format!("  {t}.{c} es {ty}"))
        .collect();

    if !mismatched.is_empty() {
        anyhow::bail!(
            "el modelo de embeddings produce {expected}, pero estas columnas no coinciden:\n\
             {}\n\n\
             El servidor NO arranca así: las escrituras a esas tablas fallarían, y la búsqueda\n\
             vectorial devolvería resultados solo léxicos sin avisar de nada.\n\n\
             Si cambiaste de modelo:  scripts/migrate-embedding-dim.sh {runtime_dim}  y después  memory-industry reembed\n\
             Si no querías cambiarlo: revisá CUBA_EMBEDDING_DIM y ONNX_MODEL_PATH en la config del cliente MCP.",
            mismatched.join("\n")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrator_loaded() {
        let count = MIGRATOR.iter().count();
        assert!(
            count >= 25,
            "expected at least 25 migrations (0001-0025), got {count}"
        );
    }

    #[test]
    fn migrations_in_order() {
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        let mut sorted = versions.clone();
        sorted.sort();
        assert_eq!(versions, sorted, "migrations must be in sorted order");
    }

    #[tokio::test]
    async fn create_lazy_pool_survives_a_malformed_database_url() {
        for bad_url in ["not a url", "", "://nope", "🦀🦀🦀"] {
            let pool = create_lazy_pool(bad_url);
            assert_eq!(
                pool.size(),
                0,
                "a lazy pool must not have connected to anything yet for input {bad_url:?}"
            );
        }
    }

    #[tokio::test]
    #[ignore]
    async fn released_connection_does_not_leak_app_current_project() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL env var required for integration tests");

        let pool = pool_options()
            .max_connections(1)
            .connect_with(connect_options(&url).expect("valid DATABASE_URL"))
            .await
            .expect("connect to test database");

        crate::session::clear();
        let (empty,): (String,) =
            sqlx::query_as("SELECT current_setting('app.current_project', true)")
                .fetch_one(&pool)
                .await
                .expect("read app.current_project with no session");
        assert_eq!(
            empty, "",
            "with no active session the pool must hand out an unscoped connection"
        );

        let project = uuid::Uuid::new_v4();
        crate::session::set(uuid::Uuid::new_v4(), Some(project));

        let (scoped,): (String,) =
            sqlx::query_as("SELECT current_setting('app.current_project', true)")
                .fetch_one(&pool)
                .await
                .expect("read app.current_project with a session");
        assert_eq!(
            scoped,
            project.to_string(),
            "the query has to SEE the project. Setting the GUC through .execute(pool) \
             put it on a connection that was returned to the pool and wiped by \
             after_release before any handler query ran, so tenant_isolation always \
             matched the empty case and returned every row in the table. before_acquire \
             is what makes the second wall exist"
        );

        crate::session::clear();
        let (cleared,): (String,) =
            sqlx::query_as("SELECT current_setting('app.current_project', true)")
                .fetch_one(&pool)
                .await
                .expect("read app.current_project after clearing the session");
        assert_eq!(
            cleared, "",
            "the same physical connection must not carry one request's project into \
             the next — this pool holds exactly one"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn a_database_behind_the_binary_is_refused_instead_of_dying_mid_import() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let url = std::env::var("DATABASE_URL").expect("DATABASE_URL required");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("connect");

        let embedded: i64 = MIGRATOR
            .iter()
            .map(|m| m.version)
            .max()
            .expect("the binary embeds migrations");
        let removed: (i64, String, Vec<u8>, bool, i64) = sqlx::query_as(
            "DELETE FROM _sqlx_migrations WHERE version = $1
             RETURNING version, description, checksum, success, execution_time",
        )
        .bind(embedded)
        .fetch_one(&pool)
        .await
        .expect("the newest migration row is what this test borrows");

        unsafe { std::env::set_var("CUBA_SKIP_MIGRATIONS", "1") };
        let verdict = init_schema(&pool).await;
        unsafe { std::env::remove_var("CUBA_SKIP_MIGRATIONS") };

        sqlx::query(
            "INSERT INTO _sqlx_migrations
                (version, description, installed_on, checksum, success, execution_time)
             VALUES ($1, $2, NOW(), $3, $4, $5)",
        )
        .bind(removed.0)
        .bind(&removed.1)
        .bind(&removed.2)
        .bind(removed.3)
        .bind(removed.4)
        .execute(&pool)
        .await
        .expect("put the migration row back");

        let Err(failure) = verdict else {
            panic!(
                "startup accepted a database one migration behind the binary. \
                 CUBA_SKIP_MIGRATIONS is the recommended runtime mode, and it only checked \
                 that SOME migration had been applied — never which. The failure that \
                 follows is not at startup: it is the first query naming a column added \
                 later, hundreds of rows into a transaction that then loses all of it"
            );
        };
        let chain = format!("{failure:#}");
        assert!(
            chain.contains(&embedded.to_string()),
            "the refusal has to name the version the binary expects, or the operator cannot \
             tell which side is stale. Got: {chain}"
        );
    }
}
