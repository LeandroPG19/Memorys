#[cfg(not(windows))]
use std::io::Write as _;
use uuid::Uuid;

const CLI_ENV_LOCK: i64 = 0x0CBA_A0D1_7106_0033;

async fn own_the_cli_env(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(CLI_ENV_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_JUEZ_CLI is process-global");
    tx
}

async fn pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database")
}

fn unique_name(prefix: &str) -> String {
    format!("{}_{}", prefix, &Uuid::new_v4().to_string()[..8])
}

fn write_cli_stub(reply: &str) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let stem = std::env::temp_dir().join(format!("cuba-memorys-v033-{}", Uuid::new_v4()));
        let json_path = stem.with_extension("json");
        let cmd_path = stem.with_extension("cmd");
        std::fs::write(&json_path, reply).expect("writing the CLI stub payload");
        std::fs::write(
            &cmd_path,
            format!("@echo off\r\ntype \"{}\"\r\n", json_path.display()),
        )
        .expect("writing the CLI stub launcher");
        cmd_path
    }
    #[cfg(not(windows))]
    {
        let path = std::env::temp_dir().join(format!("cuba-memorys-v033-{}", Uuid::new_v4()));
        let mut file = std::fs::File::create(&path).expect("creating the CLI stub");
        write!(
            file,
            "#!/bin/sh\ncat >/dev/null\ncat <<'CUBA_STUB_EOF'\n{reply}\nCUBA_STUB_EOF\n"
        )
        .expect("writing the CLI stub body");
        drop(file);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making the stub executable");
        path
    }
}

#[tokio::test]
#[ignore]
async fn an_untrusted_extraction_never_writes_the_relation_it_read() {
    let pool = pool().await;
    let _owns_cli = own_the_cli_env(&pool).await;

    let a = unique_name("UntrustedRelA");
    let b = unique_name("UntrustedRelB");
    let reply = format!(
        r#"{{"facts":[],"relations":[{{"from":"{a}","to":"{b}","relation_type":"depends_on"}}]}}"#
    );
    let stub = write_cli_stub(&reply);

    let previous_cli = std::env::var("CUBA_JUEZ_CLI").ok();
    let previous_mi = std::env::var("MEMORY_INDUSTRY_LLM_CLI").ok();
    let previous_provider = std::env::var("MEMORY_INDUSTRY_LLM_PROVIDER").ok();
    let previous_cuba_provider = std::env::var("CUBA_LLM_PROVIDER").ok();
    let previous_base = std::env::var("MEMORY_INDUSTRY_LLM_BASE_URL").ok();
    let previous_cuba_base = std::env::var("CUBA_LLM_BASE_URL").ok();
    unsafe {
        std::env::set_var("CUBA_JUEZ_CLI", &stub);
        std::env::remove_var("MEMORY_INDUSTRY_LLM_CLI");
        // Provider/URL win over CUBA_JUEZ_CLI; clear them so the stub is what runs.
        std::env::remove_var("MEMORY_INDUSTRY_LLM_PROVIDER");
        std::env::remove_var("CUBA_LLM_PROVIDER");
        std::env::remove_var("MEMORY_INDUSTRY_LLM_BASE_URL");
        std::env::remove_var("CUBA_LLM_BASE_URL");
    }

    let response = memory_industry::handlers::ingesta::handle(
        &pool,
        serde_json::json!({
            "action": "auto_extract",
            "text": "una nota cualquiera sobre el trabajo de hoy",
            "untrusted": true
        }),
    )
    .await;

    match previous_cli {
        Some(v) => unsafe { std::env::set_var("CUBA_JUEZ_CLI", v) },
        None => unsafe { std::env::remove_var("CUBA_JUEZ_CLI") },
    }
    match previous_mi {
        Some(v) => unsafe { std::env::set_var("MEMORY_INDUSTRY_LLM_CLI", v) },
        None => unsafe { std::env::remove_var("MEMORY_INDUSTRY_LLM_CLI") },
    }
    match previous_provider {
        Some(v) => unsafe { std::env::set_var("MEMORY_INDUSTRY_LLM_PROVIDER", v) },
        None => unsafe { std::env::remove_var("MEMORY_INDUSTRY_LLM_PROVIDER") },
    }
    match previous_cuba_provider {
        Some(v) => unsafe { std::env::set_var("CUBA_LLM_PROVIDER", v) },
        None => unsafe { std::env::remove_var("CUBA_LLM_PROVIDER") },
    }
    match previous_base {
        Some(v) => unsafe { std::env::set_var("MEMORY_INDUSTRY_LLM_BASE_URL", v) },
        None => unsafe { std::env::remove_var("MEMORY_INDUSTRY_LLM_BASE_URL") },
    }
    match previous_cuba_base {
        Some(v) => unsafe { std::env::set_var("CUBA_LLM_BASE_URL", v) },
        None => unsafe { std::env::remove_var("CUBA_LLM_BASE_URL") },
    }
    std::fs::remove_file(&stub).ok();
    #[cfg(windows)]
    std::fs::remove_file(stub.with_extension("json")).ok();

    let response = response.expect("the stub always answers, so auto_extract must not error");

    let count: (i64,) =
        sqlx::query_as("SELECT count(*) FROM brain_entities WHERE name = $1 OR name = $2")
            .bind(&a)
            .bind(&b)
            .fetch_one(&pool)
            .await
            .expect("counting endpoints");

    sqlx::query("DELETE FROM brain_entities WHERE name = $1 OR name = $2")
        .bind(&a)
        .bind(&b)
        .execute(&pool)
        .await
        .ok();

    assert_eq!(
        response["relations_linked"],
        serde_json::json!(0),
        "an unattended, quarantined extraction reported linking a relation it had no business \
         writing to a graph nobody reviewed: {response}"
    );
    assert_eq!(
        count.0, 0,
        "brain_entities has no trust column and brain_relations' provenance CHECK does not \
         allow a quarantined value, so the only way to keep an unreviewed REM relation out of \
         cuba_faro's associative search, PageRank and community detection is to never create \
         the row at all — found {} entities named {a} or {b}",
        count.0
    );
}
