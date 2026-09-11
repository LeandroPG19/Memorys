use serde_json::{Value, json};
use uuid::Uuid;

fn write_bundle(root: &std::path::Path, relations: Value) {
    std::fs::create_dir_all(root.join("entities")).expect("entities dir");
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "manifest_hash": Uuid::new_v4().to_string(),
            "project_id": null,
            "project_name": null,
            "exported_at": "2026-08-01T00:00:00Z",
            "counts": {"entities": 0, "observations": 0, "episodes": 0, "decisions": 0, "errors": 0, "relations": 1},
            "with_embeddings": false
        }))
        .expect("serialise"),
    )
    .expect("manifest");
    std::fs::write(
        root.join("relations.json"),
        serde_json::to_vec_pretty(&relations).expect("serialise"),
    )
    .expect("relations");
}

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
        .expect("CUBA_SYNC_DIR is process-global and this file holds two tests");
    tx
}

#[tokio::test]
#[ignore]
async fn a_relation_pointing_nowhere_is_refused_before_the_database_is_touched() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-valid-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_relations")
        .fetch_one(&pool)
        .await
        .expect("count");

    let ghost = Uuid::new_v4();
    write_bundle(
        &bundle,
        json!([{
            "id": Uuid::new_v4(),
            "from_entity": ghost,
            "to_entity": Uuid::new_v4(),
            "relation_type": "uses",
            "strength": 0.5,
            "bidirectional": false,
            "project_id": null,
            "created_at": "2026-08-01T00:00:00Z",
            "provenance": "extracted"
        }]),
    );

    let refused = memory_industry::handlers::dispatch(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string()}),
    )
    .await;

    let Err(failure) = refused else {
        panic!(
            "the import accepted a relation whose endpoints exist nowhere. Postgres would have \
             rejected it on the foreign key, and because the whole import is one transaction \
             that rejection takes every other row down with it — hundreds deep, with a bare \
             `violates foreign key constraint` and no clue which file it came from"
        );
    };
    let chain = format!("{failure:#}");
    assert!(
        chain.contains(&ghost.to_string()) && chain.contains("relations.json"),
        "the refusal has to name the file and the id, or it is the same unhelpful error one \
         layer earlier. Got: {chain}"
    );

    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_relations")
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(
        before, after,
        "and nothing may have been written: the point of validating first is that a bad \
         bundle costs no work and leaves no half-import"
    );

    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn the_closed_value_lists_match_what_the_database_actually_enforces() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    for (table, column, expected) in [
        (
            "brain_observations",
            "observation_type",
            memory_industry::handlers::sync::OBSERVATION_TYPES.to_vec(),
        ),
        (
            "brain_observations",
            "source",
            memory_industry::handlers::sync::OBSERVATION_SOURCES.to_vec(),
        ),
        (
            "brain_relations",
            "provenance",
            memory_industry::handlers::sync::RELATION_PROVENANCES.to_vec(),
        ),
    ] {
        let defs: Vec<String> = sqlx::query_scalar(
            "SELECT pg_get_constraintdef(oid) FROM pg_constraint
             WHERE contype = 'c' AND conrelid = $1::regclass",
        )
        .bind(table)
        .fetch_all(&pool)
        .await
        .expect("read the check constraints");
        let def = defs
            .iter()
            .find(|d| d.contains(&format!("({column} = ANY")))
            .unwrap_or_else(|| {
                panic!(
                    "no closed-value CHECK found for {table}.{column} — if it was dropped, \
                        this validator is now rejecting values the database would accept"
                )
            });
        for value in &expected {
            assert!(
                def.contains(&format!("'{value}'")),
                "the import validator allows {table}.{column} = {value:?} but the database \
                 constraint does not list it. A validator that drifts from the constraint it \
                 mirrors either rejects good bundles or waves through the exact rows it exists \
                 to stop. Constraint: {def}"
            );
        }
    }
}
