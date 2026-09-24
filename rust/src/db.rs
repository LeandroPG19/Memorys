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

    match retry_on_catalog_race(|| Box::pin(write_app_role_password(pool, role, password))).await {
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
        migrate_and_provision(pool).await?;
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

/// The migrations, then cuba_app's password, each tried again when it loses a
/// catalog race to another database of the same server (see
/// is_concurrent_catalog_update). Out of init_schema so that function keeps
/// its baseline CC: lizard counts a closure's `||` as a branch.
async fn migrate_and_provision(pool: &PgPool) -> Result<()> {
    retry_on_catalog_race(|| Box::pin(migrate(pool)))
        .await
        .context("failed to run sqlx migrations")?;

    tracing::info!("sqlx migrations applied");
    provision_app_role_from_pgpass(pool).await;
    Ok(())
}

/// The migrator, on a connection of its own that is closed when the run fails.
///
/// A failed run leaves its session holding the migration lock: run_direct
/// takes a session-level `pg_advisory_lock` first and returns on the error
/// without reaching its unlock (sqlx-core 0.8.6, migrate/migrator.rs:147-189).
/// Back in the pool, that connection would keep the lock while it idles, and a
/// retry on another connection would wait on it for as long as the pool keeps
/// it. Closing it ends the session, the lock and the failed migration's
/// transaction with it; nothing of that transaction was committed.
///
/// run_direct, not run: run on `&mut PgConnection` asks for `Acquire<'a>` for
/// every lifetime, which rustc refuses ("implementation of `Acquire` is not
/// general enough"); sqlx-core 0.8.6 keeps run_direct public for exactly that
/// (migrate/migrator.rs:140-145). Boxed so rustc proves Send here, with
/// concrete lifetimes: inferred, Migrator::run left create_pool's future !Send
/// (v043 spawns it) once provision_app_role took a tx.
async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    let mut conn = pool.acquire().await?;
    let outcome = {
        let run: Pin<Box<dyn Future<Output = Result<(), MigrateError>> + Send + '_>> =
            Box::pin(MIGRATOR.run_direct(&mut *conn));
        run.await
    };
    if outcome.is_err() {
        conn.close().await.ok();
    }
    outcome
}

/// Whether `error`, or an error under it, is PostgreSQL's `tuple concurrently
/// updated`: two sessions wrote one catalog row at once. Here that row is
/// cuba_app's in `pg_authid`, which belongs to the whole server. Migration 0041
/// creates or alters cuba_app and provision_app_role alters its password,
/// while sqlx's migration lock is per database, so two databases of one server
/// migrating together both write it and one of them loses. It is an `elog`
/// in PostgreSQL's simple_heap_update: SQLSTATE XX000, text never translated.
fn is_concurrent_catalog_update(error: &(dyn std::error::Error + 'static)) -> bool {
    std::iter::successors(Some(error), |e| e.source())
        .filter_map(|e| e.downcast_ref::<sqlx::Error>()?.as_database_error())
        .any(|db| {
            db.code().as_deref() == Some("XX000") && db.message() == "tuple concurrently updated"
        })
}

/// How many times a step that lost a catalog race is tried, the first included.
const CATALOG_RACE_ATTEMPTS: u32 = 5;

/// Runs `attempt` until it gives anything but a lost catalog race, at most
/// CATALOG_RACE_ATTEMPTS times, and returns what the last run gave. Any other
/// error goes out from the attempt that raised it.
///
/// Repeating what init_schema hands it is safe. A migration runs in one
/// transaction together with the insert of its `_sqlx_migrations` row
/// (sqlx-postgres 0.8.6, migrate.rs:214-225 and 275-298; none of ours starts
/// with `-- no-transaction`), so one that lost the race left no row, and no
/// row with `success = false`, which is what sqlx calls dirty; the next run
/// applies it from the start. write_app_role_password is one transaction too.
///
/// Each attempt comes boxed and Send so that create_pool's future stays Send.
async fn retry_on_catalog_race<'a, T, E>(
    mut attempt: impl FnMut() -> Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>,
) -> Result<T, E>
where
    E: std::error::Error + 'static,
{
    let mut tried = 1;
    loop {
        match attempt().await {
            Err(error) if tried < CATALOG_RACE_ATTEMPTS && is_concurrent_catalog_update(&error) => {
                // The low half of a v4 uuid, drawn from the OS generator, as in
                // bind_app_role: jitter without adding `rand` to the crate.
                let wait = catalog_race_backoff(tried, uuid::Uuid::new_v4().as_u128() as u64);
                tracing::warn!(
                    attempt = tried,
                    of = CATALOG_RACE_ATTEMPTS,
                    ?wait,
                    error = %error,
                    "lost a race for a catalog row the whole server shares; trying again"
                );
                tokio::time::sleep(wait).await;
                tried += 1;
            }
            outcome => return outcome,
        }
    }
}

/// The wait before retry number `retry` (1 is the first): a point in the upper
/// half of a ceiling that doubles from 100 ms and stops at 400 ms, picked by
/// `random`, so 50-400 ms in all. Random so that the sessions that collided do
/// not come back in step and collide again.
fn catalog_race_backoff(retry: u32, random: u64) -> Duration {
    let ceiling_ms: u64 = 50 << retry.min(3);
    Duration::from_millis(ceiling_ms / 2 + random % (ceiling_ms / 2 + 1))
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

    /// What PostgreSQL hands sqlx, built by hand: the tests below need the very
    /// error the server raises when two sessions write one catalog row, and no
    /// server raises it on demand. `attempt` is not part of what the server
    /// says; it only lets a test tell which call an error came from.
    #[derive(Debug)]
    struct ServerSaid {
        sqlstate: &'static str,
        message: String,
        attempt: u32,
    }

    impl std::fmt::Display for ServerSaid {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.message)
        }
    }

    impl std::error::Error for ServerSaid {}

    impl sqlx::error::DatabaseError for ServerSaid {
        fn message(&self) -> &str {
            &self.message
        }

        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.sqlstate))
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn server_said(sqlstate: &'static str, message: &str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(ServerSaid {
            sqlstate,
            message: message.to_owned(),
            attempt: 0,
        }))
    }

    /// Two sessions altering cuba_app's row in pg_authid at once: `elog(ERROR)`
    /// in PostgreSQL's simple_heap_update, which is XX000 and never translated.
    fn catalog_race_on(attempt: u32) -> sqlx::Error {
        sqlx::Error::Database(Box::new(ServerSaid {
            sqlstate: "XX000",
            message: "tuple concurrently updated".to_owned(),
            attempt,
        }))
    }

    fn attempt_of(error: &sqlx::Error) -> Option<u32> {
        error
            .as_database_error()?
            .try_downcast_ref::<ServerSaid>()
            .map(|said| said.attempt)
    }

    #[test]
    fn the_catalog_race_is_recognised_in_every_shape_it_reaches_init_schema() {
        assert!(
            is_concurrent_catalog_update(&MigrateError::ExecuteMigration(catalog_race_on(0), 41)),
            "the shape measured on two scratch databases of one server: `while executing \
             migration 41: error returned from database: tuple concurrently updated`"
        );
        assert!(
            is_concurrent_catalog_update(&MigrateError::Execute(catalog_race_on(0))),
            "the same race surfacing at the migration's COMMIT comes wrapped as Execute"
        );
        assert!(
            is_concurrent_catalog_update(&catalog_race_on(0)),
            "the ALTER ROLE ... PASSWORD of provision_app_role fails with a bare sqlx::Error"
        );
    }

    #[test]
    fn nothing_but_the_catalog_race_is_taken_for_it() {
        let verdicts = [
            (
                "the same text under another SQLSTATE",
                is_concurrent_catalog_update(&server_said("40001", "tuple concurrently updated")),
            ),
            (
                "XX000 with another text",
                is_concurrent_catalog_update(&server_said(
                    "XX000",
                    "cache lookup failed for relation 16384",
                )),
            ),
            (
                "migration 41 failing for another reason",
                is_concurrent_catalog_update(&MigrateError::ExecuteMigration(
                    server_said("42P01", "relation \"brain_audit_log\" does not exist"),
                    41,
                )),
            ),
            (
                "an error that never reached the server",
                is_concurrent_catalog_update(&sqlx::Error::PoolTimedOut),
            ),
            (
                "a migration edited after it shipped",
                is_concurrent_catalog_update(&MigrateError::VersionMismatch(41)),
            ),
        ];
        let taken: Vec<&str> = verdicts
            .iter()
            .filter(|(_, retried)| *retried)
            .map(|(what, _)| *what)
            .collect();
        assert!(
            taken.is_empty(),
            "taken for the catalog race and retried, which only delays the same failure by \
             a second and a half: {taken:?}"
        );
    }

    /// Runs the retry over an attempt that fails with `error(n)` on its first
    /// `failures` calls and then answers with the call number, and counts the
    /// calls: what init_schema hands it, without a server.
    async fn retried_after(
        failures: u32,
        error: fn(u32) -> sqlx::Error,
    ) -> (Result<u32, sqlx::Error>, u32) {
        let mut calls = 0;
        let outcome = retry_on_catalog_race(|| {
            calls += 1;
            let call = calls;
            Box::pin(async move {
                if call <= failures {
                    Err(error(call))
                } else {
                    Ok(call)
                }
            })
        })
        .await;
        (outcome, calls)
    }

    #[tokio::test]
    async fn a_catalog_race_that_clears_before_the_fifth_attempt_is_retried_until_it_passes() {
        for failures in [1, 4] {
            let (outcome, calls) = retried_after(failures, catalog_race_on).await;
            let answered_on = outcome.unwrap_or_else(|e| {
                panic!("{failures} catalog races and then success came out as an error: {e}")
            });
            assert_eq!(
                (answered_on, calls),
                (failures + 1, failures + 1),
                "after {failures} races the attempt that passes is call {}, and nothing runs \
                 after it",
                failures + 1
            );
        }
    }

    #[tokio::test]
    async fn a_catalog_race_on_every_attempt_gives_up_after_five_with_the_last_error() {
        let (outcome, calls) = retried_after(u32::MAX, catalog_race_on).await;
        assert_eq!(calls, 5, "five attempts in all, then the error goes out");
        let error = outcome.expect_err("every attempt raced, so the run cannot come back Ok");
        assert_eq!(
            attempt_of(&error),
            Some(5),
            "the error that goes out is the fifth attempt's, not an earlier one: {error}"
        );
    }

    #[tokio::test]
    async fn any_other_error_comes_out_on_the_first_attempt() {
        fn not_a_race(_: u32) -> sqlx::Error {
            sqlx::Error::PoolTimedOut
        }
        let (outcome, calls) = retried_after(u32::MAX, not_a_race).await;
        assert_eq!(
            calls, 1,
            "an error that is not the catalog race is not retried"
        );
        assert!(
            matches!(outcome, Err(sqlx::Error::PoolTimedOut)),
            "the error goes out as it came: {outcome:?}"
        );
    }

    #[test]
    fn the_wait_before_a_retry_is_random_between_50_and_400_ms_and_grows() {
        for retry in 1..=4 {
            for random in [0, 1, 7, 12_345, u64::MAX] {
                let wait = catalog_race_backoff(retry, random).as_millis();
                assert!(
                    (50..=400).contains(&wait),
                    "retry {retry} with random {random} waits {wait} ms, outside 50..=400"
                );
            }
        }
        let shortest: Vec<u128> = (1..=4)
            .map(|retry| catalog_race_backoff(retry, 0).as_millis())
            .collect();
        let longest: Vec<u128> = [(1, 50), (2, 100), (3, 200), (4, 200)]
            .into_iter()
            .map(|(retry, random)| catalog_race_backoff(retry, random).as_millis())
            .collect();
        assert_eq!(
            (shortest, longest),
            (vec![50, 100, 200, 200], vec![100, 200, 400, 400]),
            "each retry waits in the upper half of a ceiling that doubles from 100 ms up to \
             400 ms, the point in it picked by the random number"
        );
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
