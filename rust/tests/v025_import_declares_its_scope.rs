use serde_json::{Value, json};
use uuid::Uuid;

const SYNC_DIR_LOCK: i64 = 0x0CBA_A0D1_7106_0027;

async fn own_the_sync_dir(pool: &sqlx::PgPool) -> sqlx::Transaction<'_, sqlx::Postgres> {
    let mut tx = pool
        .begin()
        .await
        .expect("begin the serialising transaction");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_DIR_LOCK)
        .execute(&mut *tx)
        .await
        .expect("CUBA_SYNC_DIR is process-global");
    tx
}

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

#[tokio::test]
#[ignore]
async fn a_project_scoped_bundle_makes_the_database_the_guard() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-sc-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let project = format!("scope_{}", &Uuid::new_v4().to_string()[..8]);
    call(
        &pool,
        "cuba_jornada",
        json!({"action": "start", "name": "scoped export", "project": project}),
    )
    .await;

    let marker = format!("scoped_{}", &Uuid::new_v4().to_string()[..8]);
    call(
        &pool,
        "cuba_cronica",
        json!({"action": "add", "entity_name": marker, "content": format!("{marker} algo del proyecto")}),
    )
    .await;

    let exported = call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "project", "dir": bundle.display().to_string()}),
    )
    .await;
    assert!(
        exported["manifest_hash"].is_string(),
        "the export has to produce a bundle first: {exported}"
    );

    let manifest_path = bundle.join("manifest.json");
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read the manifest"))
            .expect("parse the manifest");
    assert!(
        manifest["project_id"].as_str().is_some(),
        "a project-scoped export has to declare which project, or there is no scope for the \
         import to enforce and this test proves nothing. Got: {manifest}"
    );

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "skip"}),
    )
    .await;
    assert_eq!(
        imported["scope_enforced"].as_bool(),
        Some(true),
        "the import has to set app.current_project from the manifest. Measured on this \
         database: with it set, an INSERT naming a different project is rejected by the policy; \
         with it empty — which is what the sync path did until now, because it never opens a \
         session — the same INSERT lands. Setting it is what makes the database the guard \
         instead of trusting the project_id in a file that arrived over the network. \
         Got: {imported}"
    );
    assert!(
        imported["scope_note"].is_null(),
        "and when the scope is enforced there is nothing to warn about: {imported}"
    );

    let all = call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    assert!(all["manifest_hash"].is_string(), "re-export everything");

    let wide = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "skip"}),
    )
    .await;
    assert_eq!(
        wide["scope_enforced"].as_bool(),
        Some(false),
        "a scope=all bundle crosses projects by definition, so no single scope can be declared \
         and the policy cannot filter. Got: {wide}"
    );
    assert!(
        wide["scope_note"]
            .as_str()
            .is_some_and(|n| n.contains("no filter")),
        "and that has to be said in the result rather than left as a silent difference between \
         two imports that look identical from outside. Got: {wide}"
    );

    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
