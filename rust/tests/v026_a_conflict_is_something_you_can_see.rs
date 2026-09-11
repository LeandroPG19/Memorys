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

async fn a_disagreement(pool: &sqlx::PgPool, bundle: &std::path::Path, marker: &str) -> Uuid {
    let entity_id = Uuid::new_v4();
    let obs_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(marker)
        .execute(pool)
        .await
        .expect("seed the entity");
    sqlx::query("INSERT INTO brain_observations (id, entity_id, content) VALUES ($1, $2, $3)")
        .bind(obs_id)
        .bind(entity_id)
        .bind(format!("{marker} lo que dice esta maquina"))
        .execute(pool)
        .await
        .expect("seed the observation");

    call(
        pool,
        "cuba_sync",
        json!({"action": "export", "scope": "all", "dir": bundle.display().to_string()}),
    )
    .await;

    let mut rewritten = 0;
    for entry in std::fs::read_dir(bundle.join("entities"))
        .expect("entities dir")
        .flatten()
    {
        let path = entry.path();
        let Ok(mut file) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .ok_or(())
        else {
            continue;
        };
        if file.get("id").and_then(Value::as_str) != Some(&entity_id.to_string()) {
            continue;
        }
        for obs in file["observations"].as_array_mut().expect("observations") {
            obs["content"] = Value::String(format!("{marker} lo que dice la OTRA maquina"));
            obs["origin_node"] = Value::String("portatil".to_string());
            rewritten += 1;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&file).expect("serialise"))
            .expect("rewrite");
    }
    assert_eq!(
        rewritten, 1,
        "the rewrite has to land or there is no disagreement to record"
    );

    call(
        pool,
        "cuba_sync",
        json!({"action": "import", "dir": bundle.display().to_string(), "conflict": "merge"}),
    )
    .await;
    obs_id
}

#[tokio::test]
#[ignore]
async fn what_the_import_dropped_is_still_there_to_read_afterwards() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-cf-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("scratch");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };
    sqlx::query("DELETE FROM brain_sync_conflicts")
        .execute(&pool)
        .await
        .expect("start from no conflicts");

    let marker = format!("cf_{}", &Uuid::new_v4().to_string()[..8]);
    let obs_id = a_disagreement(&pool, &bundle, &marker).await;

    let listed = call(&pool, "cuba_sync", json!({"action": "conflicts"})).await;
    let entry = listed["conflicts"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|c| c["observation_id"].as_str() == Some(&obs_id.to_string()))
        .unwrap_or_else(|| {
            panic!(
                "the import counted a divergence and then threw the evidence away. Until now \
                 'reported as a divergence' meant printed once into a tool result that nobody \
                 keeps: by the next session there was nothing left to look at. Got: {listed}"
            )
        })
        .clone();

    assert!(
        entry["ours"]
            .as_str()
            .is_some_and(|t| t.contains("esta maquina")),
        "both texts have to be kept, because a conflict record that only names the loser is a \
         pointer to something already gone: {entry}"
    );
    assert!(
        entry["theirs"]
            .as_str()
            .is_some_and(|t| t.contains("OTRA maquina")),
        "and the incoming text is the half the database does not have anywhere else: {entry}"
    );
    assert_eq!(
        entry["their_node"].as_str(),
        Some("portatil"),
        "which machine said it is what makes the two readable as a disagreement rather than \
         two strings: {entry}"
    );

    let resolved = call(
        &pool,
        "cuba_sync",
        json!({"action": "resolve", "id": entry["id"], "keep": "both"}),
    )
    .await;
    assert_eq!(resolved["kept"].as_str(), Some("both"), "{resolved}");

    let history: Value =
        sqlx::query_scalar("SELECT previous_versions FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_one(&pool)
            .await
            .expect("read the history");
    assert!(
        serde_json::to_string(&history)
            .expect("serialise")
            .contains("OTRA maquina"),
        "keep=both is the default because it is the only choice that discards nothing: the \
         other machine's text has to end up in previous_versions, where cuba_cronica reads it \
         back. Got: {history}"
    );

    let content: String =
        sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
            .bind(obs_id)
            .fetch_one(&pool)
            .await
            .expect("read the content");
    assert!(
        content.contains("esta maquina"),
        "and 'both' must not change what is current — that is what 'theirs' is for: {content}"
    );

    let after = call(&pool, "cuba_sync", json!({"action": "conflicts"})).await;
    assert!(
        !after["conflicts"]
            .as_array()
            .expect("a list")
            .iter()
            .any(|c| c["observation_id"].as_str() == Some(&obs_id.to_string())),
        "a resolved conflict must stop being reported, or the list becomes noise nobody reads \
         and the next real one hides in it. Got: {after}"
    );

    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&marker)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn keeping_theirs_takes_the_text_and_files_ours_instead_of_dropping_it() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-cf2-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("scratch");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };
    sqlx::query("DELETE FROM brain_sync_conflicts")
        .execute(&pool)
        .await
        .expect("clean");

    let marker = format!("cf2_{}", &Uuid::new_v4().to_string()[..8]);
    let obs_id = a_disagreement(&pool, &bundle, &marker).await;
    sqlx::query("UPDATE brain_observations SET embedding = $2 WHERE id = $1")
        .bind(obs_id)
        .bind(pgvector::Vector::from(vec![
            0.1f32;
            memory_industry::embeddings::onnx::embedding_dim()
        ]))
        .execute(&pool)
        .await
        .expect("give it a vector so clearing one can be observed");

    let listed = call(&pool, "cuba_sync", json!({"action": "conflicts"})).await;
    let entry = listed["conflicts"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|c| c["observation_id"].as_str() == Some(&obs_id.to_string()))
        .expect("the conflict is there")
        .clone();

    call(
        &pool,
        "cuba_sync",
        json!({"action": "resolve", "id": entry["id"], "keep": "theirs"}),
    )
    .await;

    let (content, history, embedding): (String, Value, Option<pgvector::Vector>) = sqlx::query_as(
        "SELECT content, previous_versions, embedding FROM brain_observations WHERE id = $1",
    )
    .bind(obs_id)
    .fetch_one(&pool)
    .await
    .expect("read the row back");

    assert!(
        content.contains("OTRA maquina"),
        "keep=theirs has to actually take the incoming text: {content}"
    );
    assert!(
        serde_json::to_string(&history)
            .expect("serialise")
            .contains("esta maquina"),
        "and the local text goes into previous_versions rather than being dropped — that is the \
         difference between resolving a conflict and losing an argument: {history}"
    );
    assert!(
        embedding.is_none(),
        "the vector described the text that is no longer current, so it has to be cleared or \
         vector search keeps returning this row for the old meaning, silently"
    );

    sqlx::query("DELETE FROM brain_entities WHERE name = $1")
        .bind(&marker)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
