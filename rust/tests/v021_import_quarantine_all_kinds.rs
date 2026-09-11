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

fn plant_credential_in_every_json(dir: &std::path::Path, field: &str, token: &str) -> usize {
    let mut planted = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let rows = match value.as_array_mut() {
                Some(rows) => rows,
                None => std::slice::from_mut(&mut value),
            };
            let mut touched = false;
            for row in rows.iter_mut() {
                if let Some(existing) = row.get(field).and_then(Value::as_str) {
                    let poisoned = format!("{existing} {token}");
                    row[field] = Value::String(poisoned);
                    touched = true;
                }
            }
            if touched {
                std::fs::write(&path, serde_json::to_vec_pretty(&value).expect("serialise"))
                    .expect("rewrite the bundle file");
                planted += 1;
            }
        }
    }
    planted
}

#[tokio::test]
#[ignore]
async fn an_imported_episode_or_error_carrying_a_credential_is_quarantined_and_withheld() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-kinds-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");

    let marker = format!("kinds_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query("INSERT INTO brain_episodes (entity_id, content) VALUES ($1, $2)")
        .bind(entity_id)
        .bind(format!("{marker} desplegamos el servicio y quedó estable"))
        .execute(&pool)
        .await
        .expect("seed the episode");
    sqlx::query(
        "INSERT INTO brain_errors (error_type, error_message, solution, resolved)
         VALUES ('ConnectionError', $1, 'reintentar con backoff', true)",
    )
    .bind(format!("{marker} el pool se quedó sin conexiones"))
    .execute(&pool)
    .await
    .expect("seed the error");

    let exported = call(
        &pool,
        "cuba_sync",
        serde_json::json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;
    assert!(
        exported["manifest_hash"].is_string(),
        "the export has to produce a bundle before there is anything to poison: {exported}"
    );

    const TOKEN: &str = "ghp_abcdefghijklmnop";
    let episodes = plant_credential_in_every_json(&bundle.join("episodes"), "content", TOKEN);
    let errors = plant_credential_in_every_json(&bundle.join("errors"), "error_message", TOKEN);
    assert!(
        episodes > 0 && errors > 0,
        "the point of this test is a hand-edited bundle, and import does not verify the manifest \
         hash against the files — deliberately, because the bundle lives in a git repository and \
         is meant to be edited there. That is exactly why quarantine has to be the defence: you \
         cannot checksum a file people are supposed to change. Planted in {episodes} episode and \
         {errors} error files"
    );

    let imported = call(
        &pool,
        "cuba_sync",
        serde_json::json!({
            "action": "import",
            "dir": bundle.display().to_string(),
            "conflict": "overwrite"
        }),
    )
    .await;

    assert!(
        imported["quarantined"].as_u64().expect("a count") >= 2,
        "before this change trust existed only on brain_observations, so an imported episode or \
         error carrying a credential was stored trusted and served back by faro and expediente \
         with nothing able to hold it. Got: {imported}"
    );
    assert_eq!(
        imported["quarantine_reasons"]["github token"]
            .as_u64()
            .unwrap_or(0),
        imported["quarantined"].as_u64().expect("a count"),
        "and the reason has to name the pattern, because a count with no reason is something the \
         operator cannot act on. Got: {imported}"
    );

    let quarantined_episodes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_episodes WHERE trust = 'quarantined' AND content LIKE $1",
    )
    .bind(format!("%{marker}%"))
    .fetch_one(&pool)
    .await
    .expect("count quarantined episodes");
    let quarantined_errors: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM brain_errors WHERE trust = 'quarantined' AND error_message LIKE $1",
    )
    .bind(format!("%{marker}%"))
    .fetch_one(&pool)
    .await
    .expect("count quarantined errors");
    assert_eq!(
        (quarantined_episodes, quarantined_errors),
        (1, 1),
        "the rows are stored, not dropped: refusing the bundle would lose whatever else it \
         carried, and dropping the row silently is data loss with a friendlier name"
    );

    let found = call(
        &pool,
        "cuba_faro",
        serde_json::json!({"query": marker, "limit": 20}),
    )
    .await;
    assert!(
        !serde_json::to_string(&found)
            .expect("serialise")
            .contains(TOKEN),
        "a quarantined episode must not come back from cuba_faro, or the quarantine is a column \
         nobody reads. Got: {found}"
    );

    let diagnosed = call(
        &pool,
        "cuba_expediente",
        serde_json::json!({"query": marker}),
    )
    .await;
    assert!(
        !serde_json::to_string(&diagnosed)
            .expect("serialise")
            .contains(TOKEN),
        "and neither must a quarantined error come back from cuba_expediente. Got: {diagnosed}"
    );

    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
