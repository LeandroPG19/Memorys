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

async fn provision_app_role(pool: &PgPool) {
    let Some(password) = crate::setup::app_role_password() else {
        return;
    };
    let exists: Option<(bool,)> =
        sqlx::query_as("SELECT true FROM pg_roles WHERE rolname = $1 LIMIT 1")
            .bind(APP_ROLE)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    if exists.is_none() {
        return;
    }

    match write_app_role_password(pool, APP_ROLE, &password).await {
        Ok(()) => tracing::info!(role = APP_ROLE, "application role provisioned"),
        Err(why) => {
            tracing::warn!(error = %why, "could not set the application role password")
        }
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
pub(crate) async fn bind_app_role(
    conn: &mut sqlx::PgConnection,
    role: &str,
    password: &str,
) -> sqlx::Result<()> {
    let settings = sqlx::query(
        "SELECT set_config('memory_industry.app_role', $1, true), \
                set_config('memory_industry.app_password', $2, true)",
    )
    .bind(role)
    .bind(password);
    sqlx::Executor::execute(conn, settings).await.map(drop)
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
    let pool = pool_options()
        .min_connections(1)
        .connect_with(connect_options(database_url)?)
        .await
        .context("failed to connect to PostgreSQL")?;

    tracing::info!("connected to PostgreSQL");

    init_schema(&pool).await?;

    match downgrade_to_app_role(database_url).await {
        Some(app_pool) => {
            pool.close().await;
            Ok(app_pool)
        }
        None => Ok(pool),
    }
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
        provision_app_role(pool).await;
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

    fn sha384(text: &str) -> Vec<u8> {
        <sha2::Sha384 as sha2::Digest>::digest(text.as_bytes()).to_vec()
    }

    #[test]
    fn a_recorded_checksum_is_realigned_only_when_it_differs_by_line_endings() {
        let lf = "CREATE TABLE t (id int);\nCOMMENT ON TABLE t IS 'x';\n";
        let crlf = "CREATE TABLE t (id int);\r\nCOMMENT ON TABLE t IS 'x';\r\n";
        let edited = "CREATE TABLE t (id bigint);\r\nCOMMENT ON TABLE t IS 'x';\r\n";
        let doubled = "CREATE TABLE t (id int);\r\r\nCOMMENT ON TABLE t IS 'x';\r\r\n";
        let cases = [
            ("recorded as embedded, both LF", lf, lf, None),
            ("recorded as embedded, both CRLF", crlf, crlf, None),
            (
                "LF binary over a CRLF record: the base that failed",
                lf,
                crlf,
                Some(lf),
            ),
            ("CRLF binary over an LF record", crlf, lf, Some(crlf)),
            ("a CRLF record of other content", lf, edited, None),
            ("a record of CRLF converted twice", crlf, doubled, None),
        ];
        for (case, embedded_sql, recorded_sql, expected_sql) in cases {
            let embedded = sha384(embedded_sql);
            let got = realigned_checksum(embedded_sql, &embedded, &sha384(recorded_sql));
            assert_eq!(
                got.map(<[u8]>::to_vec),
                expected_sql.map(sha384),
                "{case}: only a record that is the same SQL in the other line endings may be \
                 rewritten, and only to the checksum this binary embeds"
            );
        }
    }

    /// Pieces that include `\n`, `\r\n` and a lone `\r`, chosen by a xorshift
    /// seeded per case: the crate has no proptest, and this needs no new dependency.
    fn text_with_mixed_line_endings(seed: u64) -> String {
        const PIECES: [&str; 8] = ["\n", "\r\n", "\r", "SELECT 1;", " ", "$$", "é", "--"];
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let len = state % 48;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                PIECES[(state % PIECES.len() as u64) as usize]
            })
            .collect()
    }

    #[test]
    fn over_generated_texts_only_the_other_line_endings_are_forgiven() {
        for seed in 0..2_000 {
            let text = text_with_mixed_line_endings(seed);
            let lf = text.replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            let embedded = sha384(&text);
            let forgiven = |recorded: &str| {
                realigned_checksum(&text, &embedded, &sha384(recorded)).map(<[u8]>::to_vec)
            };
            assert_eq!(
                forgiven(&text),
                None,
                "a record equal to the embedded checksum was rewritten: {text:?}"
            );
            assert_eq!(
                forgiven(&crlf),
                (crlf != text).then_some(embedded.clone()),
                "the CRLF variant of {text:?}"
            );
            assert_eq!(
                forgiven(&lf),
                (lf != text).then_some(embedded.clone()),
                "the LF variant of {text:?}"
            );
            assert_eq!(
                forgiven(&format!("{lf}x")),
                None,
                "a record of different content was forgiven: {text:?}"
            );
        }
    }

    #[test]
    fn every_up_migration_is_found_again_from_its_other_line_endings() {
        for up in MIGRATOR
            .iter()
            .filter(|m| !m.migration_type.is_down_migration())
        {
            let lf = up.sql.replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            let other = if sha384(&lf) == *up.checksum {
                crlf
            } else {
                lf
            };
            assert_eq!(
                realigned_checksum_for(up.version, &sha384(&other)),
                Some(&*up.checksum),
                "migration {} recorded in the other line endings was not realigned to what \
                 this binary embeds",
                up.version
            );
            assert_eq!(
                realigned_checksum_for(up.version, &up.checksum),
                None,
                "migration {} is recorded exactly as embedded and was still rewritten",
                up.version
            );
        }
    }

    #[test]
    fn a_down_migration_is_never_what_a_record_is_realigned_to() {
        for down in MIGRATOR
            .iter()
            .filter(|m| m.migration_type.is_down_migration())
        {
            let lf = down.sql.replace("\r\n", "\n");
            let crlf = lf.replace('\n', "\r\n");
            for variant in [lf, crlf] {
                assert_eq!(
                    realigned_checksum_for(down.version, &sha384(&variant)),
                    None,
                    "_sqlx_migrations records up migrations; the down SQL of {} must not be \
                     the one a record is compared against",
                    down.version
                );
            }
        }
    }
}
