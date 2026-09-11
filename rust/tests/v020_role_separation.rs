use sqlx::Row;

async fn admin_pool() -> sqlx::PgPool {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    sqlx::PgPool::connect(&url)
        .await
        .expect("connect as the admin role")
}

fn app_url() -> Option<String> {
    let admin = std::env::var("DATABASE_URL").ok()?;
    let password = memory_industry::setup::app_role_password()?;
    let tail = admin.split('@').nth(1)?.to_string();
    Some(format!("postgresql://cuba_app:{password}@{tail}"))
}

#[tokio::test]
#[ignore]
async fn the_application_role_is_not_a_superuser_and_cannot_bypass_rls() {
    let admin = admin_pool().await;
    let row = sqlx::query("SELECT rolsuper, rolbypassrls FROM pg_roles WHERE rolname = 'cuba_app'")
        .fetch_one(&admin)
        .await
        .expect("the migration must have created cuba_app");
    let is_super: bool = row.get(0);
    let bypasses: bool = row.get(1);

    assert!(
        !is_super,
        "a superuser evades row-level security unconditionally, so RLS would be decorative"
    );
    assert!(
        !bypasses,
        "BYPASSRLS defeats FORCE ROW LEVEL SECURITY just as thoroughly as superuser does"
    );
}

#[tokio::test]
#[ignore]
async fn the_audit_log_rejects_mutation_from_the_application_role() {
    let admin = admin_pool().await;
    sqlx::query(&format!(
        "ALTER ROLE cuba_app PASSWORD '{}'",
        memory_industry::setup::app_role_password().expect("an app password")
    ))
    .execute(&admin)
    .await
    .expect("setting the app role password");

    let marker = format!("rev_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO brain_audit_log (action, payload, current_hash)
         VALUES ('test', jsonb_build_object('subject', $1::text), sha256($1::bytea))
         RETURNING id",
    )
    .bind(&marker)
    .fetch_one(&admin)
    .await
    .expect("inserting an audit row as admin");

    let url = app_url().expect(
        "no app password available. This test is the one that proves cuba_app cannot rewrite \
         the audit log, and that role is now what the daemon actually connects as — skipping \
         it silently is skipping the check on a live security control",
    );
    let app = sqlx::PgPool::connect(&url)
        .await
        .expect("connect as cuba_app");

    let updated = sqlx::query("UPDATE brain_audit_log SET action = 'tampered' WHERE id = $1")
        .bind(id)
        .execute(&app)
        .await;
    assert!(
        updated.is_err(),
        "the application role must not be able to rewrite the audit chain — this is the \
         append-only guarantee, and while the runtime was a superuser it never held"
    );

    let deleted = sqlx::query("DELETE FROM brain_audit_log WHERE id = $1")
        .bind(id)
        .execute(&app)
        .await;
    assert!(
        deleted.is_err(),
        "nor delete from it: without a privilege REVOKE the trigger alone was the only guard"
    );

    sqlx::query("DELETE FROM brain_audit_log WHERE id = $1")
        .bind(id)
        .execute(&admin)
        .await
        .ok();
}
