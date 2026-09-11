use serde_json::Value;
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
async fn merge_says_out_loud_which_rows_it_refused_to_touch() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-merge-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("merge_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    let obs_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query("INSERT INTO brain_observations (id, entity_id, content) VALUES ($1, $2, $3)")
        .bind(obs_id)
        .bind(entity_id)
        .bind(format!("{marker} la version de esta maquina"))
        .execute(&pool)
        .await
        .expect("seed the observation");

    call(
        &pool,
        "cuba_sync",
        serde_json::json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let entities_dir = bundle.join("entities");
    let mut rewritten = 0;
    for entry in std::fs::read_dir(&entities_dir)
        .expect("the export wrote an entities directory")
        .flatten()
    {
        let path = entry.path();
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(mut file) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        if file.get("id").and_then(Value::as_str) != Some(&entity_id.to_string()) {
            continue;
        }
        for obs in file["observations"]
            .as_array_mut()
            .expect("an entity file carries its observations")
        {
            obs["content"] = Value::String(format!("{marker} la version corregida en la otra"));
            rewritten += 1;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&file).expect("serialise"))
            .expect("rewrite the bundle file");
    }
    assert_eq!(
        rewritten, 1,
        "the point of this test is a bundle whose copy of an existing row differs, and the \
         rewrite has to actually land or the import has nothing to disagree with"
    );

    let imported = call(
        &pool,
        "cuba_sync",
        serde_json::json!({
            "action": "import",
            "dir": bundle.display().to_string(),
            "conflict": "merge"
        }),
    )
    .await;

    assert_eq!(
        imported["diverged"].as_u64().unwrap_or(0),
        1,
        "conflict=merge maps to the same branch as conflict=skip — ON CONFLICT DO NOTHING — so \
         the row that already existed here is kept and the incoming one is dropped. That is \
         defensible; doing it without saying so is not, and it is the default the installed \
         git hook uses. Got: {imported}"
    );
    assert!(
        imported["divergence_note"]
            .as_str()
            .expect("a note")
            .contains("not a merge"),
        "and the note has to say plainly that merge does not merge, because the name promises \
         otherwise and a correction made on the other machine silently fails to arrive. \
         Got: {imported}"
    );

    let stored: String = sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
        .bind(obs_id)
        .fetch_one(&pool)
        .await
        .expect("read the row back");
    assert!(
        stored.contains("de esta maquina"),
        "merge must not have overwritten anything — reporting the divergence is the change, \
         changing the resolution is not"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
