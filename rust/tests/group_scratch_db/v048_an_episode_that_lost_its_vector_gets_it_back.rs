// needs-a-server: creates and drops its own scratch databases where DATABASE_URL points, and runs the real binary against them
//
//! CI-E22. An episode whose vector never landed (the process exited first, no model was loaded,
//! a `sync` import, or every episode after `scripts/migrate-embedding-dim.sh`) stays out of the
//! semantic half of `faro` for good unless something fills it. `reembed` (the tool and the
//! command) and the REM backfill read only observations, while the shutdown message sent the
//! user to `reembed`.
//!
//! The fixture is the state that matters, made by SQL in a database this file creates and drops
//! (`common::in_a_scratch_database`): an episode with `embedding IS NULL`. It is the same row
//! whether the process was killed, the model was absent or a bundle brought it, and killing a
//! process half way through the drain is not deterministic. "Not touched" is read from `xmin`:
//! an UPDATE makes a new version of the row with another `xmin`, whatever the product reports.
//! The binary gets its environment through `Command::env`; this process's own is never written.

use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::Duration;

use crate::common::in_a_scratch_database;
use memory_industry::embeddings::onnx::{current_model, embedding_dim, is_model_loaded};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

/// A guard against a hung child, not the verdict of any test.
const HUNG: Duration = Duration::from_secs(120);

/// A vector no model produces, so a row that still holds it was not re-embedded.
const KNOWN: f32 = 0.25;

const TEXT: &str = "El motor de busqueda embebe este texto con el contexto de su entidad";

const NEEDS_A_MODEL: &str = "this test needs the ONNX embedder loaded (ONNX_MODEL_PATH and \
     ORT_DYLIB_PATH, installed by `memory-industry models all`): without a model there is no \
     vector to write and the test could not tell a repair from nothing";

fn unique_name(prefix: &str) -> String {
    format!("{prefix}_{}", &Uuid::new_v4().to_string()[..8])
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(
        a.len(),
        b.len(),
        "two vectors of one space have one width, or the comparison below would cut the longer"
    );
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0_f32, f32::max)
}

fn the_known_vector() -> Vec<f32> {
    vec![KNOWN; embedding_dim()]
}

async fn connect_as_admin(url: &str) -> PgPool {
    PgPool::connect(url)
        .await
        .expect("connecting to the scratch database as its administrator")
}

async fn an_entity(pool: &PgPool, prefix: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'technology') RETURNING id",
    )
    .bind(unique_name(prefix))
    .fetch_one(pool)
    .await
    .expect("seeding an entity of type technology")
}

async fn an_observation(pool: &PgPool, entity: Uuid, content: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO brain_observations (entity_id, content, observation_type, source, importance)
         VALUES ($1, $2, 'fact', 'agent', 0.5) RETURNING id",
    )
    .bind(entity)
    .bind(content)
    .fetch_one(pool)
    .await
    .expect("seeding an observation without a vector")
}

async fn an_episode(pool: &PgPool, entity: Uuid, content: &str, importance: f64) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO brain_episodes (entity_id, content, importance) VALUES ($1, $2, $3)
         RETURNING id",
    )
    .bind(entity)
    .bind(content)
    .bind(importance)
    .fetch_one(pool)
    .await
    .expect("seeding an episode without a vector")
}

async fn give_the_known_vector(pool: &PgPool, episode: Uuid, model: &str) {
    sqlx::query(
        "UPDATE brain_episodes SET embedding = $1::vector, embedding_model = $2 WHERE id = $3",
    )
    .bind(pgvector::Vector::from(the_known_vector()))
    .bind(model)
    .bind(episode)
    .execute(pool)
    .await
    .expect("giving an episode the known vector");
}

struct Stored {
    vector: Option<Vec<f32>>,
    model: Option<String>,
}

impl Stored {
    fn expect_vector(&self, whose: &str) -> &[f32] {
        self.vector
            .as_deref()
            .unwrap_or_else(|| panic!("{whose} has no vector, and it was expected to have one"))
    }
}

async fn episode_stored(pool: &PgPool, id: Uuid) -> Stored {
    let (vector, model): (Option<pgvector::Vector>, Option<String>) =
        sqlx::query_as("SELECT embedding, embedding_model FROM brain_episodes WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("reading an episode back");
    Stored {
        vector: vector.map(|v| v.to_vec()),
        model,
    }
}

async fn observation_stored(pool: &PgPool, id: Uuid) -> Stored {
    let (vector, model): (Option<pgvector::Vector>, Option<String>) =
        sqlx::query_as("SELECT embedding, embedding_model FROM brain_observations WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("reading an observation back");
    Stored {
        vector: vector.map(|v| v.to_vec()),
        model,
    }
}

async fn xmin_of(pool: &PgPool, episode: Uuid) -> String {
    sqlx::query_scalar("SELECT xmin::text FROM brain_episodes WHERE id = $1")
        .bind(episode)
        .fetch_one(pool)
        .await
        .expect("reading the xmin of an episode")
}

/// A directory with nothing in it. `ONNX_MODEL_PATH=""` would fall to the user's cache, where the
/// judge has a model; a directory that exists and holds none is what "no model" is.
struct EmptyDir(PathBuf);

impl EmptyDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("creating an empty directory");
        Self(dir)
    }
}

impl Drop for EmptyDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

enum Model {
    /// The model, the runtime and the width the judge exports, which the child inherits.
    Judges,
    Absent,
}

/// Runs the binary against `url`, writes `input` to its stdin and closes it, and waits for it.
async fn run_binary(
    url: &str,
    model: Model,
    args: &[&str],
    input: &str,
    more_env: &[(&str, &str)],
) -> Output {
    let mut command = tokio::process::Command::new(BIN);
    command
        .args(args)
        .env("DATABASE_URL", url)
        .env("MEMORY_INDUSTRY_REQUIRE_DATABASE_URL", "1")
        .env("RUST_LOG", "memory_industry=info")
        .envs(more_env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let _empty_directories = match model {
        Model::Judges => None,
        Model::Absent => {
            let embedder = EmptyDir::new();
            let reranker = EmptyDir::new();
            command
                .env("ONNX_MODEL_PATH", &embedder.0)
                .env("CUBA_RERANKER_PATH", &reranker.0);
            Some((embedder, reranker))
        }
    };

    let mut child = command
        .spawn()
        .unwrap_or_else(|why| panic!("starting {BIN} {args:?}: {why}"));
    let mut stdin = child.stdin.take().expect("the child has a piped stdin");
    stdin
        .write_all(input.as_bytes())
        .await
        .unwrap_or_else(|why| panic!("writing the request to `{args:?}`: {why}"));
    drop(stdin);

    tokio::time::timeout(HUNG, child.wait_with_output())
        .await
        .unwrap_or_else(|_| panic!("`memory-industry {args:?}` did not finish within {HUNG:?}"))
        .unwrap_or_else(|why| panic!("waiting for `memory-industry {args:?}`: {why}"))
}

/// What a tool answered over stdio: the JSON inside `result.content[0].text` of the reply to id 1.
fn the_tool_reply(stdout: &[u8]) -> Value {
    let stdout = text(stdout);
    let envelope = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message["id"] == json!(1))
        .unwrap_or_else(|| panic!("no JSON-RPC reply to id 1 in the stdout: {stdout:?}"));
    let content = envelope["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("the reply carries no result.content[0].text: {envelope}"));
    serde_json::from_str(content)
        .unwrap_or_else(|why| panic!("the tool's text is not JSON ({why}): {content}"))
}

#[tokio::test]
async fn reembed_gives_an_episode_its_vector_by_the_rule_of_observations() {
    assert!(is_model_loaded(), "{NEEDS_A_MODEL}");
    let model = current_model();
    in_a_scratch_database("e22_reembed", |url| {
        reembed_repairs_episodes_like_observations(url, model)
    })
    .await;
}

async fn reembed_repairs_episodes_like_observations(url: String, model: String) {
    let admin = connect_as_admin(&url).await;
    let a = an_entity(&admin, "e22_a").await;
    let b = an_entity(&admin, "e22_b").await;
    let observation_a = an_observation(&admin, a, TEXT).await;
    let observation_b = an_observation(&admin, b, TEXT).await;
    let lost = an_episode(&admin, a, TEXT, 0.9).await;
    let current = an_episode(&admin, a, TEXT, 0.5).await;
    give_the_known_vector(&admin, current, &model).await;
    let other_model = an_episode(&admin, a, TEXT, 0.5).await;
    give_the_known_vector(&admin, other_model, "e22-old-model").await;
    let current_xmin = xmin_of(&admin, current).await;

    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("opening the pool the way the daemon does");
    let first = memory_industry::handlers::zafra::handle(&pool, json!({"action": "reembed"}))
        .await
        .expect("cuba_zafra action=reembed");

    // E22a.1
    let lost_after = episode_stored(&admin, lost).await;
    assert!(
        lost_after.vector.is_some(),
        "the episode had no vector and reembed left it that way: it reads observations only, so \
         an episode that lost its vector stays out of the semantic half of faro. Response: {first}"
    );
    assert_eq!(
        lost_after.model.as_deref(),
        Some(model.as_str()),
        "a vector was written, so its label has to name the model that produced it"
    );
    let from_the_twin = max_abs_diff(
        lost_after.expect_vector("the repaired episode"),
        observation_stored(&admin, observation_a)
            .await
            .expect_vector("the observation of the same entity and text"),
    );
    assert!(
        from_the_twin < 1e-5,
        "the episode has to be embedded the way the write path embeds it (text, entity type and \
         entity name), which is what its twin observation went through: max abs diff {from_the_twin}"
    );
    let from_the_other_entity = max_abs_diff(
        lost_after.expect_vector("the repaired episode"),
        observation_stored(&admin, observation_b)
            .await
            .expect_vector("the observation of the other entity"),
    );
    assert!(
        from_the_other_entity > 1e-4,
        "the same text under another entity gets another vector, so an episode that matches it \
         was embedded without its own entity's context: max abs diff {from_the_other_entity}"
    );

    // E22a.2, first half
    let current_after = episode_stored(&admin, current).await;
    assert_eq!(
        xmin_of(&admin, current).await,
        current_xmin,
        "an episode that already carries the current model's vector is not written again"
    );
    assert!(
        max_abs_diff(
            current_after.expect_vector("the current episode"),
            &the_known_vector()
        ) < 1e-6,
        "and its vector is still the known one"
    );

    // E22a.3
    let other_after = episode_stored(&admin, other_model).await;
    assert_eq!(
        other_after.model.as_deref(),
        Some(model.as_str()),
        "an episode with the vector of another model is pending, and re-embedding retags it"
    );
    assert!(
        max_abs_diff(
            other_after.expect_vector("the retagged episode"),
            &the_known_vector()
        ) > 1e-4,
        "the label changed but the vector is still the known one: it was relabelled, not re-embedded"
    );

    // E22a.4
    assert_eq!(
        first["episodes"],
        json!({"fetched": 2, "updated": 2}),
        "two episodes were pending (no vector, and another model's): {first}"
    );
    assert_eq!(
        first["observations"],
        json!({"fetched": 2, "updated": 2}),
        "two observations were pending: {first}"
    );
    assert_eq!(
        first["total_fetched"],
        json!(4),
        "the total fetched adds both tables: {first}"
    );
    assert_eq!(
        first["updated"],
        json!(4),
        "the total updated adds both tables: {first}"
    );

    // E22a.2, second half
    let noted = [
        xmin_of(&admin, lost).await,
        xmin_of(&admin, current).await,
        xmin_of(&admin, other_model).await,
    ];
    let second = memory_industry::handlers::zafra::handle(&pool, json!({"action": "reembed"}))
        .await
        .expect("cuba_zafra action=reembed, again");
    assert_eq!(
        second["episodes"],
        json!({"fetched": 0, "updated": 0}),
        "everything is up to date after the first call, so a second one fetches no episode: {second}"
    );
    assert_eq!(
        second["observations"],
        json!({"fetched": 0, "updated": 0}),
        "nor any observation: {second}"
    );
    let after_the_second = [
        xmin_of(&admin, lost).await,
        xmin_of(&admin, current).await,
        xmin_of(&admin, other_model).await,
    ];
    assert_eq!(
        after_the_second, noted,
        "a second reembed rewrote a row that was already current"
    );

    pool.close().await;
    admin.close().await;
}

#[tokio::test]
async fn without_a_model_nothing_writes_a_vector() {
    in_a_scratch_database("e22_nomodel", without_a_model_the_episode_stays_as_it_was).await;
}

async fn without_a_model_the_episode_stays_as_it_was(url: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "e22_nomodel").await;
    let observation = an_observation(&admin, entity, TEXT).await;
    let episode = an_episode(&admin, entity, TEXT, 0.9).await;
    let episode_xmin = xmin_of(&admin, episode).await;

    // E22a.5
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "cuba_zafra", "arguments": {"action": "reembed"}}
    });
    let reembed = run_binary(&url, Model::Absent, &[], &format!("{request}\n"), &[]).await;
    assert!(
        reembed.status.success(),
        "stdio ends on EOF with exit 0, got {:?}: {}",
        reembed.status.code(),
        text(&reembed.stderr)
    );
    assert_eq!(
        the_tool_reply(&reembed.stdout),
        json!({
            "action": "reembed",
            "updated": 0,
            "error": "ONNX model not loaded — set ONNX_MODEL_PATH to enable reembed"
        }),
        "without a model the answer is what it was before episodes entered reembed, to the field"
    );
    let stored = episode_stored(&admin, episode).await;
    assert!(
        stored.vector.is_none() && xmin_of(&admin, episode).await == episode_xmin,
        "without a model the tool has nothing to write and must not touch the episode"
    );

    // E22b.2
    let command = run_binary(&url, Model::Absent, &["reembed"], "", &[]).await;
    assert_eq!(
        command.status.code(),
        Some(1),
        "`memory-industry reembed` without a model refuses with exit 1: {}",
        text(&command.stderr)
    );
    assert!(
        text(&command.stderr).contains("no hay modelo ONNX cargado"),
        "and says why: {}",
        text(&command.stderr)
    );
    let stored = episode_stored(&admin, episode).await;
    assert!(
        stored.vector.is_none() && xmin_of(&admin, episode).await == episode_xmin,
        "the refusal comes before any write: the episode is as it was"
    );

    // E22c.2
    let rem = run_binary(
        &url,
        Model::Absent,
        &["rem"],
        "",
        &[
            ("CUBA_REM_AUTOLINK", "0"),
            ("CUBA_REM_RELATION_BATCH", "0"),
            ("CUBA_REM_EXTRACTION_BATCH", "0"),
        ],
    )
    .await;
    assert!(
        rem.status.success(),
        "`memory-industry rem` exits 0, got {:?}: {}",
        rem.status.code(),
        text(&rem.stderr)
    );
    let report: Value = serde_json::from_slice(&rem.stdout).unwrap_or_else(|why| {
        panic!(
            "the stdout of `rem` is not one JSON ({why}): {}",
            text(&rem.stdout)
        )
    });
    assert_eq!(
        report,
        json!({
            "action": "rem",
            "embeddings_missing_before": 2,
            "embeddings_missing_after": 2
        }),
        "an observation and an episode are missing a vector, a REM cycle without a model writes \
         none, and the count has to measure what the backfill reads: both tables"
    );
    assert!(
        observation_stored(&admin, observation)
            .await
            .vector
            .is_none(),
        "the observation got a vector from a REM cycle with no model: that is the hash fallback \
         stored under the real model's name"
    );
    let stored = episode_stored(&admin, episode).await;
    assert!(
        stored.vector.is_none() && xmin_of(&admin, episode).await == episode_xmin,
        "the episode is as it was after the cycle"
    );

    admin.close().await;
}

#[tokio::test]
async fn the_reembed_command_repairs_the_episodes() {
    assert!(is_model_loaded(), "{NEEDS_A_MODEL}");
    let model = current_model();
    in_a_scratch_database("e22_cmd_all", |url| {
        the_command_repairs_an_episode_next_to_an_observation(url, model)
    })
    .await;
    in_a_scratch_database(
        "e22_cmd_one",
        the_command_repairs_an_episode_with_no_observation_pending,
    )
    .await;
}

async fn the_command_repairs_an_episode_next_to_an_observation(url: String, model: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "e22_cmd").await;
    an_observation(&admin, entity, TEXT).await;
    let lost = an_episode(&admin, entity, TEXT, 0.9).await;
    let current = an_episode(&admin, entity, TEXT, 0.5).await;
    give_the_known_vector(&admin, current, &model).await;
    let current_xmin = xmin_of(&admin, current).await;

    // E22b.1
    let run = run_binary(&url, Model::Judges, &["reembed"], "", &[]).await;
    assert!(
        run.status.success(),
        "`memory-industry reembed` exits 0, got {:?}: {}",
        run.status.code(),
        text(&run.stderr)
    );
    let expected = format!(
        "Reembebidas 1 de 1 observaciones y 1 de 1 episodios con {model} ({}-d).",
        embedding_dim()
    );
    assert!(
        text(&run.stdout).contains(&expected),
        "the command says what it repaired in each table. Wanted {expected:?}, stdout: {}",
        text(&run.stdout)
    );
    let repaired = episode_stored(&admin, lost).await;
    assert!(
        repaired.vector.is_some() && repaired.model.as_deref() == Some(model.as_str()),
        "the episode has to leave with a vector labelled {model}, got model {:?}",
        repaired.model
    );
    assert_eq!(
        xmin_of(&admin, current).await,
        current_xmin,
        "an episode that was already current is not written again"
    );

    admin.close().await;
}

async fn the_command_repairs_an_episode_with_no_observation_pending(url: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "e22_cmd_only").await;
    let lost = an_episode(&admin, entity, TEXT, 0.9).await;

    // E22b.3
    let run = run_binary(&url, Model::Judges, &["reembed"], "", &[]).await;
    assert!(
        run.status.success(),
        "`memory-industry reembed` exits 0, got {:?}: {}",
        run.status.code(),
        text(&run.stderr)
    );
    assert!(
        text(&run.stdout).contains("Reembebidas 0 de 0 observaciones y 1 de 1 episodios"),
        "with no observation pending the command still has an episode to repair, and must not stop \
         at \"Nada que reembeber\". stdout: {}",
        text(&run.stdout)
    );
    assert!(
        episode_stored(&admin, lost).await.vector.is_some(),
        "the episode has no vector after the command that says it repaired it"
    );

    admin.close().await;
}

#[tokio::test]
async fn backfill_fills_the_episodes_and_does_not_repeat() {
    assert!(is_model_loaded(), "{NEEDS_A_MODEL}");
    let model = current_model();
    in_a_scratch_database("e22_backfill", |url| {
        the_backfill_takes_its_limit_from_each_table(url, model)
    })
    .await;
}

async fn the_backfill_takes_its_limit_from_each_table(url: String, model: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "e22_backfill").await;
    let observation = an_observation(&admin, entity, TEXT).await;
    let first_episode = an_episode(&admin, entity, TEXT, 0.9).await;
    let last_episode = an_episode(&admin, entity, TEXT, 0.1).await;
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("opening the pool the way the daemon does");

    // E22c.1
    let first = memory_industry::embeddings::backfill::backfill_missing(&pool, 1)
        .await
        .expect("the first backfill");
    assert_eq!(
        (first.attempted, first.embedded, first.failed),
        (2, 2, 0),
        "limit 1 is per table: one observation and one episode, the most important of each. {first:?}"
    );
    for (whose, stored) in [
        (
            "the observation",
            observation_stored(&admin, observation).await,
        ),
        (
            "the important episode",
            episode_stored(&admin, first_episode).await,
        ),
    ] {
        assert!(
            stored.vector.is_some() && stored.model.as_deref() == Some(model.as_str()),
            "{whose} has to carry a vector labelled {model}, got model {:?}",
            stored.model
        );
    }
    assert!(
        episode_stored(&admin, last_episode).await.vector.is_none(),
        "the other episode is past the limit of this pass and waits for the next"
    );

    let second = memory_industry::embeddings::backfill::backfill_missing(&pool, 1)
        .await
        .expect("the second backfill");
    assert_eq!(
        (second.attempted, second.embedded, second.failed),
        (1, 1, 0),
        "only the episode left is pending, and the filled rows are not attempted again. {second:?}"
    );
    assert!(
        episode_stored(&admin, last_episode).await.vector.is_some(),
        "the episode that waited got its vector in the second pass"
    );

    let noted = [
        xmin_of(&admin, first_episode).await,
        xmin_of(&admin, last_episode).await,
    ];
    let third = memory_industry::embeddings::backfill::backfill_missing(&pool, 1)
        .await
        .expect("the third backfill");
    assert_eq!(
        (third.attempted, third.embedded, third.failed),
        (0, 0, 0),
        "nothing is pending any more. {third:?}"
    );
    assert_eq!(
        [
            xmin_of(&admin, first_episode).await,
            xmin_of(&admin, last_episode).await,
        ],
        noted,
        "a backfill with nothing pending rewrote a row"
    );

    pool.close().await;
    admin.close().await;
}
