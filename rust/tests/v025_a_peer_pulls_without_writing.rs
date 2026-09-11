use memory_industry::session::{Scope, with_scope};
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
    let text = envelope["content"][0]["text"]
        .as_str()
        .expect("dispatch wraps every handler result in the MCP content envelope");
    serde_json::from_str(text).expect("the result is JSON inside that envelope")
}

async fn pull_page(pool: &sqlx::PgPool, offset: u64) -> Value {
    let envelope = with_scope(
        Scope::Peer,
        memory_industry::handlers::dispatch(
            pool,
            "cuba_sync",
            json!({"action": "pull", "offset": offset, "limit": 1, "with_embeddings": false}),
        ),
    )
    .await
    .unwrap_or_else(|e| panic!("a peer pull failed: {e:#}"));
    let text = envelope["content"][0]["text"].as_str().expect("envelope");
    serde_json::from_str(text).expect("json")
}

#[tokio::test]
#[ignore]
async fn a_peer_pull_writes_nothing_and_still_hands_over_the_whole_bundle() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-pl-{}", Uuid::new_v4()));
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let marker = format!("pull_{}", &Uuid::new_v4().to_string()[..8]);
    let entity_id = Uuid::new_v4();
    sqlx::query("INSERT INTO brain_entities (id, name, entity_type) VALUES ($1, $2, 'concept')")
        .bind(entity_id)
        .bind(&marker)
        .execute(&pool)
        .await
        .expect("seed the entity");
    sqlx::query("INSERT INTO brain_observations (entity_id, content) VALUES ($1, $2)")
        .bind(entity_id)
        .bind(format!(
            "{marker} algo que la otra maquina tiene que recibir"
        ))
        .execute(&pool)
        .await
        .expect("seed the observation");

    let before = std::fs::read_dir(&bundle)
        .expect("the sync directory exists")
        .count();

    let mut offset = 0u64;
    let mut received: Vec<Value> = Vec::new();
    let mut hashes: Vec<String> = Vec::new();
    loop {
        let page = pull_page(&pool, offset).await;
        assert!(
            page["node_id"].as_str().is_some(),
            "every page has to name the node it came from, or the receiving side cannot tell \
             two peers apart and cannot close a notice by origin. It was null on every page \
             for a while: the manifest carried it to disk and export's JSON result did not, so \
             pull was reading a key that was never set. Got: {page}"
        );
        hashes.push(
            page["manifest_hash"]
                .as_str()
                .expect("every page names the state it came from")
                .to_string(),
        );
        received.extend(
            page["files"]
                .as_array()
                .expect("a page carries files")
                .clone(),
        );
        if !page["has_more"].as_bool().unwrap_or(false) {
            break;
        }
        offset = page["next_offset"].as_u64().expect("a next offset");
        assert!(
            offset as usize == received.len(),
            "the cursor has to advance by exactly what was delivered or a page is skipped in \
             silence, which is the shape of a sync that reports success and loses rows"
        );
    }

    assert!(
        hashes.len() > 1,
        "with limit=1 the bundle has to arrive in more than one page, or the paging path this \
         test exists to exercise never ran and its green says nothing about it"
    );
    assert!(
        hashes.windows(2).all(|w| w[0] == w[1]),
        "the pages of one pull have to describe one state. Differing hashes mean the node was \
         written to mid-transfer: {hashes:?}"
    );
    assert!(
        received.len() >= 4,
        "a bundle is at least a manifest, an entity, the projects and the tombstones, and the \
         pull returned {} file(s) — a green result from an empty transfer proves nothing",
        received.len()
    );

    let after = std::fs::read_dir(&bundle)
        .expect("the sync directory still exists")
        .count();
    assert_eq!(
        before, after,
        "a peer pull must not touch the sync directory. export writes files and prunes the ones \
         it did not write, so serving a peer through export would let a read-only token delete \
         this machine's bundle"
    );

    let names: Vec<&str> = received.iter().filter_map(|f| f["path"].as_str()).collect();
    assert!(
        names.contains(&"manifest.json"),
        "without the manifest the receiving side cannot check the schema version, the embedding \
         model or the node it came from. Got: {names:?}"
    );

    let text_of_entity_files: String = received
        .iter()
        .filter(|f| {
            f["path"]
                .as_str()
                .is_some_and(|p| p.starts_with("entities/"))
        })
        .filter_map(|f| f["text"].as_str())
        .collect();
    assert!(
        text_of_entity_files.contains(&marker),
        "the row seeded a moment ago has to be in what the peer received, or the pull is serving \
         something other than this database"
    );

    sqlx::query("DELETE FROM brain_entities WHERE id = $1")
        .bind(entity_id)
        .execute(&pool)
        .await
        .ok();
    let _ = std::fs::remove_dir_all(&bundle);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}

#[tokio::test]
#[ignore]
async fn what_a_peer_pulls_is_a_bundle_the_import_accepts() {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL env var required for integration tests");
    let bundle = std::env::temp_dir().join(format!("cuba-pl2-{}", Uuid::new_v4()));
    let landed = bundle.join("from-peer");
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to test database");
    let _owns = own_the_sync_dir(&pool).await;
    std::fs::create_dir_all(&bundle).expect("a scratch bundle directory");
    std::fs::create_dir_all(&landed).expect("a directory for what the peer received");
    unsafe { std::env::set_var("CUBA_SYNC_DIR", &bundle) };

    let mut offset = 0u64;
    loop {
        let page = pull_page(&pool, offset).await;
        for file in page["files"].as_array().expect("files") {
            let relative = file["path"].as_str().expect("a path");
            let target = landed.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            match (file["text"].as_str(), file["hex"].as_str()) {
                (Some(text), _) => std::fs::write(&target, text).expect("write"),
                (None, Some(blob)) => {
                    std::fs::write(&target, hex::decode(blob).expect("hex")).expect("write")
                }
                (None, None) => panic!("a file arrived with neither text nor hex: {file}"),
            }
        }
        if !page["has_more"].as_bool().unwrap_or(false) {
            break;
        }
        offset = page["next_offset"].as_u64().expect("next offset");
    }

    let imported = call(
        &pool,
        "cuba_sync",
        json!({"action": "import", "dir": landed.display().to_string(), "conflict": "skip"}),
    )
    .await;
    assert!(
        imported.get("error").is_none(),
        "the whole point of serving the same file shapes the export writes is that the receiving \
         side needs no new merge logic: what a peer pulls has to be exactly what import already \
         validates. Got: {imported}"
    );

    let _ = std::fs::remove_dir_all(&bundle);
    let _ = std::fs::remove_dir_all(&landed);
    unsafe { std::env::remove_var("CUBA_SYNC_DIR") };
}
