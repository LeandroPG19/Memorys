use serde_json::{Value, json};
use uuid::Uuid;

async fn call(pool: &sqlx::PgPool, tool: &str, args: Value) -> Value {
    let envelope = memory_industry::handlers::dispatch(pool, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} failed: {e:#}"));
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the result is JSON inside that envelope")
}

#[tokio::test]
#[ignore]
async fn an_overwriting_import_keeps_the_version_it_replaced() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-hist-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let entity = Uuid::new_v4();
    let obs = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity)
        .bind(format!("hist_{}", &entity.to_string()[..8]))
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query(
        "INSERT INTO brain_observations (id, entity_id, content, embedding_model)
         VALUES ($1, $2, 'what this machine believes', 'bge-m3')",
    )
    .bind(obs)
    .bind(entity)
    .execute(&pool)
    .await
    .expect("seed the observation");

    let dim: i32 = sqlx::query_scalar(
        "SELECT atttypmod FROM pg_attribute
         WHERE attrelid = 'brain_observations'::regclass AND attname = 'embedding'",
    )
    .fetch_one(&pool)
    .await
    .expect("the embedding column declares its dimension");
    let vector = format!(
        "[{}]",
        (0..dim).map(|_| "0.1").collect::<Vec<_>>().join(",")
    );
    sqlx::query("UPDATE brain_observations SET embedding = $2::vector WHERE id = $1")
        .bind(obs)
        .bind(&vector)
        .execute(&pool)
        .await
        .expect(
            "give it a vector, or the assertion below cannot tell a cleared one from one \
                 that was never there",
        );

    call(
        &pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    for entry in std::fs::read_dir(bundle.join("entities"))
        .expect("entities dir")
        .flatten()
    {
        let path = entry.path();
        let Ok(mut file) = serde_json::from_slice::<Value>(&std::fs::read(&path).expect("read"))
        else {
            continue;
        };
        if file.get("id").and_then(Value::as_str) != Some(&entity.to_string()) {
            continue;
        }
        for o in file["observations"].as_array_mut().expect("observations") {
            o["content"] = Value::String("what the other machine believes".into());
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&file).expect("serialise")).expect("write");
    }

    call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "overwrite"}),
    )
    .await;

    type Row = (String, Value, i32, Option<Vec<u8>>);
    let (content, history, version, embedding): Row = sqlx::query_as(
        "SELECT content, previous_versions, version, embedding::text::bytea
         FROM brain_observations WHERE id = $1",
    )
    .bind(obs)
    .fetch_one(&pool)
    .await
    .expect("read the row back");

    assert_eq!(content, "what the other machine believes");
    let kept = history.as_array().expect("previous_versions is an array");
    assert_eq!(
        kept.len(),
        1,
        "the import rewrote content without touching previous_versions before this, so \
         --conflict overwrite replaced a correction made here with the peer's version and left \
         nothing behind. The entire conflict design rests on the loser being kept, and the one \
         path most likely to hit a conflict did not keep it. Got: {history}"
    );
    assert_eq!(
        kept[0]["content"].as_str(),
        Some("what this machine believes"),
        "and what is kept has to be the version that was replaced"
    );
    assert!(
        version >= 2,
        "replacing the content is a change the clock has to see"
    );
    assert!(
        embedding.is_none(),
        "the stored vector describes the text that was just replaced. Leaving it makes the row \
         retrievable by a meaning it no longer carries, silently — reembed is the only repair \
         and nothing would have told anyone to run it"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
