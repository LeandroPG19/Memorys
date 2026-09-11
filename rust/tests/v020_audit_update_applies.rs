use uuid::Uuid;

#[tokio::test]
#[ignore]
async fn an_admin_update_on_the_audit_log_actually_changes_the_row() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("gdpr_{}", &Uuid::new_v4().to_string()[..8]);
    let row: Option<(i64,)> = sqlx::query_as(
        "INSERT INTO brain_audit_log (action, payload, current_hash)
         VALUES ('test', jsonb_build_object('subject', $1::text), sha256($1::bytea))
         RETURNING id",
    )
    .bind(&marker)
    .fetch_optional(&pool)
    .await
    .expect("inserting an audit row");

    let (id,) = row.expect(
        "the INSERT ... RETURNING id produced no row. That is a broken audit table, not a \
         different shape, and reporting ok would hide it",
    );

    let affected = sqlx::query(
        "UPDATE brain_audit_log SET payload = jsonb_build_object('subject', 'redacted')
         WHERE id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .expect("the rectification must be permitted for an admin role")
    .rows_affected();
    assert_eq!(affected, 1, "postgres must report the row as updated");

    let after: String =
        sqlx::query_scalar("SELECT payload->>'subject' FROM brain_audit_log WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("reading the row back");

    assert_eq!(
        after, "redacted",
        "RETURN OLD in a BEFORE UPDATE trigger lets the statement proceed while writing the OLD \
         values back: postgres reports UPDATE 1 and the operator believes the personal data was \
         rectified when nothing changed. The value must be the new one."
    );

    sqlx::query("DELETE FROM brain_audit_log WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}
