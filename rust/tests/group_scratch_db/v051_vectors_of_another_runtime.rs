// needs-a-server: creates and drops its own scratch databases where DATABASE_URL points, and runs the real binary once
//
//! CI-D5c, part B. ONNX Runtime 1.31.1 does not compute the numbers 1.26.0 did (up to 1.4e-3 in
//! `1 - cos`), so a stored vector now carries the model AND the runtime that made it, and every
//! vector of an earlier runtime is «of another generation» until `reembed` rewrites it. The memory
//! of a machine must survive that change, so `reembed` is the one place that writes over a vector
//! that cannot be recomputed from anywhere else, and these tests are about what it does not do:
//!
//! - it does not write a row before the old vector is in a file on disk (B.6, B.7);
//! - it does not lose its place when it stops, and does not make a second snapshot (B.8);
//! - it does not overwrite a row somebody changed while it was encoding (B.9);
//! - it does not run twice at once (B.10);
//! - it can give the old vectors back, and only where the text is still the one they were made of
//!   (B.11);
//! - a vector that arrives by sync keeps the label of the row it came with (B.12).
//!
//! The encoder is injected: a function that returns a flat vector (`0.5` in every dimension) and
//! writes down what it was asked, so nothing here loads ONNX and the expected values are literals
//! of this file. The old vectors are flat too, each of a value that is exact in binary (`0.125`,
//! `0.25`, ...), so a vector that went through a file or through pgvector comes back equal. Every
//! database is one `common::in_a_scratch_database` clones and drops; the snapshot of each test is
//! in a directory of its own under the temp directory and is removed when the test ends.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::common::in_a_scratch_database;
use anyhow::Result;
use futures::FutureExt;
use futures::future::BoxFuture;
use memory_industry::embeddings::backfill::vector_coverage;
use memory_industry::embeddings::onnx::embedding_dim;
use memory_industry::reembed_cli::{EmbedInput, Plan, reembed, rollback};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Notify;
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

const OLD: &str = "multilingual-e5-small";
const OLD_RUNTIME: &str = "multilingual-e5-small@onnxruntime-1.26.0";
const NEW: &str = "multilingual-e5-small@onnxruntime-1.31.1";
const NODE: &str = "00000000-0000-4000-8000-000000000051";

const OBSERVATION_TEXT: &str = "texto de la observacion";
const EPISODE_TEXT: &str = "texto del episodio";
const CHUNK_TEXT: &str = "texto del fragmento";
const PROCEDURE_TEXT: &str = "desplegar. cuando se publica";

fn flat(value: f32) -> Vec<f32> {
    vec![value; embedding_dim()]
}

/// A directory of this test under the temp directory, removed when the test ends. It is not
/// created: `reembed` makes what it needs, and a test that wants it absent can say so.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("v051-{}", Uuid::new_v4())))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn plan(root: &Path) -> Plan {
    Plan {
        tag: NEW.to_string(),
        all: false,
        chunk: 500,
        limit: None,
        node_id: NODE.to_string(),
        snapshot_root: root.to_path_buf(),
        progress: false,
    }
}

/// The four tables that carry a vector, and the one literal statement each of these tests needs.
#[derive(Clone, Copy)]
enum Table {
    Observations,
    Episodes,
    Chunks,
    Procedures,
}

impl Table {
    const ALL: [Table; 4] = [
        Table::Observations,
        Table::Episodes,
        Table::Chunks,
        Table::Procedures,
    ];

    fn name(self) -> &'static str {
        match self {
            Table::Observations => "brain_observations",
            Table::Episodes => "brain_episodes",
            Table::Chunks => "brain_observation_chunks",
            Table::Procedures => "brain_procedures",
        }
    }

    fn stamp_sql(self) -> &'static str {
        match self {
            Table::Observations => {
                "UPDATE brain_observations SET embedding = $1::vector, embedding_model = $2 WHERE id = $3"
            }
            Table::Episodes => {
                "UPDATE brain_episodes SET embedding = $1::vector, embedding_model = $2 WHERE id = $3"
            }
            Table::Chunks => {
                "UPDATE brain_observation_chunks SET embedding = $1::vector, embedding_model = $2 WHERE id = $3"
            }
            Table::Procedures => {
                "UPDATE brain_procedures SET embedding = $1::vector, embedding_model = $2 WHERE id = $3"
            }
        }
    }

    fn read_sql(self) -> &'static str {
        match self {
            Table::Observations => {
                "SELECT embedding, embedding_model, xmin::text FROM brain_observations WHERE id = $1"
            }
            Table::Episodes => {
                "SELECT embedding, embedding_model, xmin::text FROM brain_episodes WHERE id = $1"
            }
            Table::Chunks => {
                "SELECT embedding, embedding_model, xmin::text FROM brain_observation_chunks WHERE id = $1"
            }
            Table::Procedures => {
                "SELECT embedding, embedding_model, xmin::text FROM brain_procedures WHERE id = $1"
            }
        }
    }
}

struct Stored {
    vector: Option<Vec<f32>>,
    label: Option<String>,
    xmin: String,
}

async fn stored(pool: &PgPool, table: Table, id: Uuid) -> Stored {
    let (vector, label, xmin): (Option<pgvector::Vector>, Option<String>, String) =
        sqlx::query_as(table.read_sql())
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|why| panic!("reading a row of {} back: {why}", table.name()));
    Stored {
        vector: vector.map(|v| v.to_vec()),
        label,
        xmin,
    }
}

async fn stamp(pool: &PgPool, table: Table, id: Uuid, value: f32, label: &str) {
    sqlx::query(table.stamp_sql())
        .bind(pgvector::Vector::from(flat(value)))
        .bind(label)
        .bind(id)
        .execute(pool)
        .await
        .unwrap_or_else(|why| panic!("giving a row of {} its vector: {why}", table.name()));
}

async fn an_entity(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO brain_entities (name, entity_type) VALUES ($1, 'technology') RETURNING id",
    )
    .bind(name)
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

async fn an_episode(pool: &PgPool, entity: Uuid, content: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO brain_episodes (entity_id, content, importance) VALUES ($1, $2, 0.5)
         RETURNING id",
    )
    .bind(entity)
    .bind(content)
    .fetch_one(pool)
    .await
    .expect("seeding an episode without a vector")
}

async fn a_chunk(pool: &PgPool, observation: Uuid, index: i32, content: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO brain_observation_chunks (observation_id, chunk_index, content)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(observation)
    .bind(index)
    .bind(content)
    .fetch_one(pool)
    .await
    .expect("seeding a chunk without a vector")
}

async fn a_procedure(pool: &PgPool, name: &str, trigger: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO brain_procedures (name, trigger_context) VALUES ($1, $2) RETURNING id",
    )
    .bind(name)
    .bind(trigger)
    .fetch_one(pool)
    .await
    .expect("seeding a procedure without a vector")
}

async fn md5_of(pool: &PgPool, text: &str) -> String {
    sqlx::query_scalar("SELECT md5($1)")
        .bind(text)
        .fetch_one(pool)
        .await
        .expect("asking Postgres for the md5 of a text")
}

async fn half_is_null(pool: &PgPool, observation: Uuid) -> bool {
    sqlx::query_scalar("SELECT embedding_half IS NULL FROM brain_observations WHERE id = $1")
        .bind(observation)
        .fetch_one(pool)
        .await
        .expect("reading embedding_half")
}

/// One stale row in each of the four tables, every vector flat and of the model alone (what
/// every database written before 0.31 holds), and the halfvec of the observation filled, as the
/// REM cycle leaves it.
struct World {
    entity_name: String,
    observation: Uuid,
    episode: Uuid,
    chunk: Uuid,
    procedure: Uuid,
}

impl World {
    fn rows(&self) -> [(Table, Uuid, &'static str, f32); 4] {
        [
            (
                Table::Observations,
                self.observation,
                OBSERVATION_TEXT,
                0.125,
            ),
            (Table::Episodes, self.episode, EPISODE_TEXT, 0.25),
            (Table::Chunks, self.chunk, CHUNK_TEXT, 0.375),
            (Table::Procedures, self.procedure, PROCEDURE_TEXT, 0.625),
        ]
    }
}

async fn a_world_of_the_old_runtime(pool: &PgPool) -> World {
    let entity_name = format!("v051_{}", &Uuid::new_v4().to_string()[..8]);
    let entity = an_entity(pool, &entity_name).await;
    let observation = an_observation(pool, entity, OBSERVATION_TEXT).await;
    let episode = an_episode(pool, entity, EPISODE_TEXT).await;
    let chunk = a_chunk(pool, observation, 0, CHUNK_TEXT).await;
    let procedure = a_procedure(pool, "desplegar", "cuando se publica").await;
    let world = World {
        entity_name,
        observation,
        episode,
        chunk,
        procedure,
    };
    for (table, id, _, value) in world.rows() {
        stamp(pool, table, id, value, OLD).await;
    }
    sqlx::query("UPDATE brain_observations SET embedding_half = embedding::halfvec WHERE id = $1")
        .bind(observation)
        .execute(pool)
        .await
        .expect("quantizing the observation the way the REM cycle does");
    assert!(
        !half_is_null(pool, observation).await,
        "control: the fixture has a halfvec, so the NULL asserted after the rewrite is a change"
    );
    world
}

/// What the injected encoder was asked, in order.
#[derive(Clone, Default)]
struct Calls(Arc<Mutex<Vec<EmbedInput>>>);

impl Calls {
    fn seen(&self) -> Vec<EmbedInput> {
        self.0.lock().expect("the list of calls").clone()
    }

    /// `'static` said out loud: in edition 2024 the opaque type would otherwise hold the borrow of
    /// `&self`, and `reembed` takes an encoder that owns what it needs.
    fn encoder(
        &self,
    ) -> impl Fn(EmbedInput) -> BoxFuture<'static, Result<Vec<f32>>> + Send + Sync + 'static {
        let calls = self.0.clone();
        move |input| {
            calls.lock().expect("the list of calls").push(input);
            async { anyhow::Ok(flat(0.5)) }.boxed()
        }
    }
}

fn snapshot_parts(dir: &Path) -> Vec<PathBuf> {
    let mut parts: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|why| panic!("reading the snapshot {}: {why}", dir.display()))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("part-") && name.ends_with(".jsonl"))
        })
        .collect();
    parts.sort();
    parts
}

/// The rows of every part of a snapshot, and the header of the first one.
fn saved(dir: &Path) -> (Value, Vec<Value>) {
    let mut header = Value::Null;
    let mut rows = Vec::new();
    for part in snapshot_parts(dir) {
        let text = std::fs::read_to_string(&part)
            .unwrap_or_else(|why| panic!("reading {}: {why}", part.display()));
        let mut lines = text.lines();
        let first: Value = serde_json::from_str(lines.next().expect("a part has its header line"))
            .expect("the header is JSON");
        if header.is_null() {
            header = first;
        }
        for line in lines {
            rows.push(serde_json::from_str(line).expect("a saved row is JSON"));
        }
    }
    (header, rows)
}

fn no_unfinished_part_is_left(dir: &Path) {
    let unfinished: Vec<String> = std::fs::read_dir(dir)
        .expect("reading the snapshot")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".part"))
        .collect();
    assert!(
        unfinished.is_empty(),
        "a part is written under a name that is not a part and renamed when it is on disk: {unfinished:?}"
    );
}

async fn connect_as_admin(url: &str) -> PgPool {
    PgPool::connect(url)
        .await
        .expect("connecting to the scratch database as its administrator")
}

// ---------------------------------------------------------------------------------------------
// B.3
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_vectors_of_another_generation_are_counted_in_the_four_tables() {
    in_a_scratch_database("v051_count", counts_each_table).await;
}

async fn counts_each_table(url: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "v051_count").await;
    let parent = an_observation(&admin, entity, "la observacion de los fragmentos").await;
    for table in Table::ALL {
        // Three rows per table: one of another generation, one of this one, one with no vector.
        let mut ids = Vec::new();
        for n in 0..3 {
            let id = match table {
                Table::Observations => an_observation(&admin, entity, &format!("fila {n}")).await,
                Table::Episodes => an_episode(&admin, entity, &format!("fila {n}")).await,
                Table::Chunks => a_chunk(&admin, parent, n + 1, &format!("fila {n}")).await,
                Table::Procedures => a_procedure(&admin, &format!("procedimiento {n}"), "x").await,
            };
            ids.push(id);
        }
        stamp(&admin, table, ids[0], 0.125, OLD).await;
        stamp(&admin, table, ids[1], 0.25, NEW).await;
    }
    // The parent observation is a fourth observation, and it has no vector.

    let coverage = vector_coverage(&admin, NEW)
        .await
        .expect("counting the vectors");
    assert_eq!(coverage.tag, NEW);
    for (name, counted, total, missing) in [
        ("observations", &coverage.observations, 4, 2),
        ("episodes", &coverage.episodes, 3, 1),
        ("chunks", &coverage.chunks, 3, 1),
        ("procedures", &coverage.procedures, 3, 1),
    ] {
        assert_eq!(
            (counted.total, counted.missing, counted.stale),
            (total, missing, 1),
            "{name}: one vector of the old label, one of the new one, and the rest without a vector"
        );
    }
    assert_eq!(coverage.stale(), 4, "one of another generation per table");
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// B.6
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_reembed_keeps_the_old_vectors_before_it_writes_one() {
    in_a_scratch_database("v051_keep", keeps_the_old_vectors).await;
}

async fn keeps_the_old_vectors(url: String) {
    let admin = connect_as_admin(&url).await;
    let world = a_world_of_the_old_runtime(&admin).await;
    let root = Scratch::new();
    let calls = Calls::default();
    let encode = calls.encoder();

    let report = reembed(&admin, &plan(&root.0), &encode)
        .await
        .expect("reembedding a database of the old runtime");

    assert_eq!(
        (
            report.observations.rewritten,
            report.episodes.rewritten,
            report.chunks.rewritten,
            report.procedures.rewritten
        ),
        (1, 1, 1, 1),
        "one stale row in each of the four tables, and `reembed` used to read two of them: {report:?}"
    );

    // What the encoder was asked: the text of the row with the entity it hangs from, as the write
    // path embeds it, and the procedure as `name. trigger` without entity.
    let seen = calls.seen();
    let contextual = |text: &str| EmbedInput::Contextual {
        text: text.to_string(),
        entity_type: "technology".to_string(),
        entity_name: world.entity_name.clone(),
    };
    for wanted in [
        contextual(OBSERVATION_TEXT),
        contextual(EPISODE_TEXT),
        contextual(CHUNK_TEXT),
        EmbedInput::Bare {
            text: PROCEDURE_TEXT.to_string(),
        },
    ] {
        assert!(
            seen.contains(&wanted),
            "the encoder was never asked for {wanted:?}; it saw {seen:?}"
        );
    }

    // The snapshot: every row with its old vector, its old label and the fingerprint of the text.
    let (header, rows) = saved(&report.snapshot);
    no_unfinished_part_is_left(&report.snapshot);
    assert_eq!(header["format"], "memory-industry-vectors/1");
    assert_eq!(header["node_id"], NODE);
    assert_eq!(header["to"], NEW);
    assert_eq!(rows.len(), 4, "one saved row per rewritten row: {rows:?}");
    for (table, id, text, value) in world.rows() {
        let row = rows
            .iter()
            .find(|row| row["table"] == table.name() && row["id"] == id.to_string())
            .unwrap_or_else(|| panic!("{} {id} is not in the snapshot: {rows:?}", table.name()));
        assert_eq!(
            row["md5"],
            md5_of(&admin, text).await,
            "the fingerprint of the text the vector was made of, in {}",
            table.name()
        );
        assert_eq!(row["model"], OLD, "the label it had, in {}", table.name());
        assert_eq!(
            row["vector"],
            json!(flat(value)),
            "the vector it had, in {}",
            table.name()
        );
    }

    // And the rows themselves: the vector of the encoder, the new label, no halfvec of the old one.
    for (table, id, ..) in world.rows() {
        let now = stored(&admin, table, id).await;
        assert_eq!(
            now.vector.as_deref(),
            Some(flat(0.5).as_slice()),
            "{} has the vector of the encoder",
            table.name()
        );
        assert_eq!(now.label.as_deref(), Some(NEW), "{}", table.name());
    }
    assert!(
        half_is_null(&admin, world.observation).await,
        "the halfvec of the observation was computed from the old vector: it is cleared, and the REM cycle quantizes the new one"
    );
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// B.7
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn without_a_snapshot_no_row_is_rewritten() {
    in_a_scratch_database("v051_nosnap", refuses_without_a_snapshot).await;
}

async fn refuses_without_a_snapshot(url: String) {
    let admin = connect_as_admin(&url).await;
    let world = a_world_of_the_old_runtime(&admin).await;
    let before: Vec<String> = {
        let mut xmins = Vec::new();
        for (table, id, ..) in world.rows() {
            xmins.push(stored(&admin, table, id).await.xmin);
        }
        xmins
    };

    // A root that cannot be made: its parent is a file, which no user, root included, can write under.
    let blocker = Scratch::new();
    std::fs::create_dir_all(&blocker.0).expect("a directory for the blocker");
    let file = blocker.0.join("not-a-directory");
    std::fs::write(&file, b"x").expect("a file where the snapshot root should go");
    let calls = Calls::default();
    let encode = calls.encoder();

    let refusal = reembed(&admin, &plan(&file.join("reembed")), &encode)
        .await
        .expect_err("with no place to keep the old vectors the reembed must not run");
    let said = format!("{refusal:#}");
    assert!(
        said.contains("sin instantánea no reescribo"),
        "the refusal has to say why nothing was written: {said}"
    );
    assert!(
        calls.seen().is_empty(),
        "it did not even encode: there was nowhere to put the old vector of a row it would rewrite"
    );
    for ((table, id, ..), xmin) in world.rows().into_iter().zip(&before) {
        let now = stored(&admin, table, id).await;
        assert_eq!(
            &now.xmin,
            xmin,
            "{} was written though the snapshot could not be",
            table.name()
        );
        assert_eq!(now.label.as_deref(), Some(OLD));
    }
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// B.8
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_reembed_that_stops_goes_on_in_the_next_run_with_one_snapshot() {
    in_a_scratch_database("v051_resume", resumes_with_one_snapshot).await;
}

async fn resumes_with_one_snapshot(url: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "v051_resume").await;
    let mut rows: Vec<(Table, Uuid)> = Vec::new();
    for n in 0..3 {
        let id = an_observation(&admin, entity, &format!("observacion {n}")).await;
        stamp(&admin, Table::Observations, id, 0.125, OLD).await;
        rows.push((Table::Observations, id));
    }
    for n in 0..2 {
        let id = an_episode(&admin, entity, &format!("episodio {n}")).await;
        stamp(&admin, Table::Episodes, id, 0.25, OLD).await;
        rows.push((Table::Episodes, id));
    }
    let root = Scratch::new();

    // The model goes away on the third row it is asked for.
    let asked = Arc::new(AtomicUsize::new(0));
    let failing = {
        let asked = asked.clone();
        move |_input: EmbedInput| -> BoxFuture<'static, Result<Vec<f32>>> {
            let n = asked.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if n == 3 {
                    Err(anyhow::anyhow!("the model went away on row {n}"))
                } else {
                    Ok(flat(0.5))
                }
            }
            .boxed()
        }
    };
    let first = reembed(&admin, &plan(&root.0), &failing)
        .await
        .expect("a row the encoder could not do does not end the run");
    assert_eq!(
        (first.total().rewritten, first.total().failed),
        (4, 1),
        "four rows done and the one the model failed on: {first:?}"
    );
    let mut pending = Vec::new();
    for (table, id) in &rows {
        if stored(&admin, *table, *id).await.label.as_deref() != Some(NEW) {
            pending.push((*table, *id));
        }
    }
    assert_eq!(
        pending.len(),
        1,
        "exactly the failed row is still of the old label"
    );
    assert_eq!(
        saved(&first.snapshot).1.len(),
        5,
        "the five rows were saved before any was written, the failed one included"
    );

    // The next run, with a model that does not fail, does the one row that is left and nothing else.
    let calls = Calls::default();
    let encode = calls.encoder();
    let second = reembed(&admin, &plan(&root.0), &encode)
        .await
        .expect("the second run");
    assert_eq!(second.total().rewritten, 1, "{second:?}");
    assert_eq!(
        calls.seen().len(),
        1,
        "the rows of the first run are not encoded again: {:?}",
        calls.seen()
    );
    for (table, id) in &rows {
        assert_eq!(
            stored(&admin, *table, *id).await.label.as_deref(),
            Some(NEW),
            "every row ends with the new label"
        );
    }

    assert_eq!(
        second.snapshot, first.snapshot,
        "the same generation, the same snapshot"
    );
    let (_, all) = saved(&second.snapshot);
    let distinct: HashSet<(String, String)> = all
        .iter()
        .map(|row| (row["table"].to_string(), row["id"].to_string()))
        .collect();
    assert_eq!(
        (all.len(), distinct.len()),
        (5, 5),
        "each row is in the snapshot once: {all:?}"
    );
    let snapshots = std::fs::read_dir(&root.0)
        .expect("reading the snapshot root")
        .flatten()
        .count();
    assert_eq!(
        snapshots, 1,
        "two runs of one generation make one snapshot directory, not two"
    );
    no_unfinished_part_is_left(&second.snapshot);
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// B.9
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_row_that_changes_while_it_is_encoded_is_not_overwritten() {
    in_a_scratch_database("v051_race", leaves_the_row_somebody_changed).await;
}

async fn leaves_the_row_somebody_changed(url: String) {
    let admin = connect_as_admin(&url).await;
    let world = a_world_of_the_old_runtime(&admin).await;
    let root = Scratch::new();

    // The encoder is the point where the two processes meet: while it is working on the
    // observation, another connection rewrites that row (other text, other vector, other label).
    // No timing: the write happens inside the call, before the encoder answers.
    let other = admin.clone();
    let observation = world.observation;
    let encode = move |input: EmbedInput| -> BoxFuture<'static, Result<Vec<f32>>> {
        let other = other.clone();
        async move {
            if matches!(&input, EmbedInput::Contextual { text, .. } if text == OBSERVATION_TEXT) {
                sqlx::query(
                    "UPDATE brain_observations
                     SET content = $1, embedding = $2::vector, embedding_model = $3 WHERE id = $4",
                )
                .bind("texto cambiado por otro proceso")
                .bind(pgvector::Vector::from(flat(0.875)))
                .bind("otro-proceso")
                .bind(observation)
                .execute(&other)
                .await?;
            }
            anyhow::Ok(flat(0.5))
        }
        .boxed()
    };

    let report = reembed(&admin, &plan(&root.0), &encode)
        .await
        .expect("reembedding while another connection writes");

    let now = stored(&admin, Table::Observations, world.observation).await;
    assert_eq!(
        now.vector.as_deref(),
        Some(flat(0.875).as_slice()),
        "the vector the other connection wrote was overwritten with one made of the old text"
    );
    assert_eq!(now.label.as_deref(), Some("otro-proceso"));
    let content: String =
        sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
            .bind(world.observation)
            .fetch_one(&admin)
            .await
            .expect("reading the content back");
    assert_eq!(content, "texto cambiado por otro proceso");
    assert_eq!(
        (report.observations.changed, report.observations.rewritten),
        (1, 0),
        "the row is counted as changed during the reembed, not as done: {report:?}"
    );
    assert_eq!(
        report.episodes.rewritten, 1,
        "control: a row nobody touched is rewritten, so the one above was left for a reason"
    );
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// B.10
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn two_reembeds_at_once_the_second_refuses_and_touches_nothing() {
    in_a_scratch_database("v051_lock", the_second_one_refuses).await;
}

async fn the_second_one_refuses(url: String) {
    let admin = connect_as_admin(&url).await;
    let world = a_world_of_the_old_runtime(&admin).await;
    let root = Scratch::new();
    let first_root = root.0.join("first");
    let second_root = root.0.join("second");

    // «The other reembed» is a real one: it runs in a task and stops inside its encoder, on the
    // first row, with the lock taken and nothing written. The barrier is the encoder itself, so
    // the test holds no lock of its own and waits for no time.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let pausing = {
        let (entered, release, stopped) = (entered.clone(), release.clone(), stopped.clone());
        move |_input: EmbedInput| -> BoxFuture<'static, Result<Vec<f32>>> {
            let (entered, release, stopped) = (entered.clone(), release.clone(), stopped.clone());
            async move {
                if !stopped.swap(true, Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                anyhow::Ok(flat(0.5))
            }
            .boxed()
        }
    };
    let first = tokio::spawn({
        let (pool, plan) = (admin.clone(), plan(&first_root));
        async move { reembed(&pool, &plan, &pausing).await }
    });
    entered.notified().await;

    let before: Vec<String> = {
        let mut xmins = Vec::new();
        for (table, id, ..) in world.rows() {
            xmins.push(stored(&admin, table, id).await.xmin);
        }
        xmins
    };
    let calls = Calls::default();
    let encode = calls.encoder();
    let refusal = reembed(&admin, &plan(&second_root), &encode)
        .await
        .expect_err("a reembed while another one runs has to refuse");
    let said = format!("{refusal:#}");
    assert!(
        said.contains("otro reembebido"),
        "the refusal says another one is running: {said}"
    );
    assert!(
        !second_root.exists(),
        "it made no snapshot: the second one did nothing at all"
    );
    assert!(calls.seen().is_empty(), "and encoded nothing");
    for ((table, id, ..), xmin) in world.rows().into_iter().zip(&before) {
        assert_eq!(
            &stored(&admin, table, id).await.xmin,
            xmin,
            "{} was written by a reembed that did not hold the lock",
            table.name()
        );
    }

    release.notify_one();
    let done = first
        .await
        .expect("the first reembed was not cut")
        .expect("the first reembed ends well once its encoder answers");
    assert_eq!(
        done.total().rewritten,
        4,
        "control: the first one was a reembed like any other, so the refusal was about the lock: {done:?}"
    );
    let after = reembed(&admin, &plan(&second_root), &encode)
        .await
        .expect("with the lock free the same second reembed runs");
    assert_eq!(
        after.total().fetched,
        0,
        "and finds nothing left of the old generation: {after:?}"
    );
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// B.11
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_rollback_gives_back_the_old_vectors_only_where_the_text_did_not_change() {
    in_a_scratch_database("v051_back", rolls_back).await;
}

async fn rolls_back(url: String) {
    let admin = connect_as_admin(&url).await;
    let world = a_world_of_the_old_runtime(&admin).await;
    let entity: Uuid = sqlx::query_scalar("SELECT entity_id FROM brain_observations WHERE id = $1")
        .bind(world.observation)
        .fetch_one(&admin)
        .await
        .expect("the entity of the fixture");
    let edited = an_observation(&admin, entity, "texto que cambiara despues").await;
    stamp(&admin, Table::Observations, edited, 0.0625, OLD).await;
    let root = Scratch::new();
    let calls = Calls::default();
    let encode = calls.encoder();
    reembed(&admin, &plan(&root.0), &encode)
        .await
        .expect("the reembed that is going to be undone");

    // Afterwards: the text of one row changes, and a row is born under the new label.
    sqlx::query("UPDATE brain_observations SET content = 'texto que cambio despues' WHERE id = $1")
        .bind(edited)
        .execute(&admin)
        .await
        .expect("editing a row after the reembed");
    let born = an_observation(&admin, entity, "fila nacida despues").await;
    stamp(&admin, Table::Observations, born, 0.875, NEW).await;
    let untouched = [(Table::Observations, edited), (Table::Observations, born)];
    let mut before = Vec::new();
    for (table, id) in untouched {
        before.push(stored(&admin, table, id).await.xmin);
    }
    let mut all_before = Vec::new();
    for (table, id, ..) in world.rows() {
        all_before.push(stored(&admin, table, id).await.xmin);
    }

    // With the new runtime still loaded there is nothing to go back to: refused, nothing moves.
    let refusal = rollback(&admin, &root.0, NEW, Some(NEW))
        .await
        .expect_err("the rollback has to be run with the previous runtime loaded");
    let said = format!("{refusal:#}");
    assert!(
        said.contains("volvé primero al runtime anterior"),
        "the refusal says how to continue: {said}"
    );
    for ((table, id, ..), xmin) in world.rows().into_iter().zip(&all_before) {
        assert_eq!(
            &stored(&admin, table, id).await.xmin,
            xmin,
            "a refused rollback wrote {}",
            table.name()
        );
    }

    // With the old runtime loaded the old vectors come back.
    let report = rollback(&admin, &root.0, OLD_RUNTIME, Some(NEW))
        .await
        .expect("the rollback with the previous runtime loaded");
    assert_eq!(
        (report.restored, report.skipped),
        (4, 1),
        "four rows of the snapshot are as the reembed left them; the one whose text changed is skipped: {report:?}"
    );
    for (table, id, _, value) in world.rows() {
        let now = stored(&admin, table, id).await;
        assert_eq!(
            now.vector.as_deref(),
            Some(flat(value).as_slice()),
            "{} has its old vector back",
            table.name()
        );
        assert_eq!(now.label.as_deref(), Some(OLD), "and its old label");
    }
    assert!(
        half_is_null(&admin, world.observation).await,
        "the halfvec is cleared again: the REM cycle quantizes the vector that is there"
    );
    let edited_now = stored(&admin, Table::Observations, edited).await;
    assert_eq!(
        (edited_now.vector.as_deref(), edited_now.label.as_deref()),
        (Some(flat(0.5).as_slice()), Some(NEW)),
        "the row whose text changed keeps what the reembed wrote: the old vector is of a text it no longer has"
    );
    assert_eq!(&edited_now.xmin, &before[0]);
    let born_now = stored(&admin, Table::Observations, born).await;
    assert_eq!(
        (born_now.vector.as_deref(), born_now.label.as_deref()),
        (Some(flat(0.875).as_slice()), Some(NEW)),
        "a row born under the new label has no old vector to give back"
    );
    assert_eq!(&born_now.xmin, &before[1]);
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// B.12
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_vector_that_arrives_by_sync_keeps_the_label_of_its_row() {
    in_a_scratch_database("v051_sync", sync_keeps_the_label).await;
}

/// The bundle of a machine on the old runtime: the observation with the label it had there and
/// its vector in `embeddings.bin.zst`, made by hand the way `v024_embedding_guard` makes one.
fn write_a_bundle(dir: &Path, entity: (Uuid, &str), observation: (Uuid, &str)) {
    let dim = embedding_dim();
    std::fs::create_dir_all(dir.join("entities")).expect("the entities directory of the bundle");
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "manifest_hash": "irrelevant, the import recomputes it",
            "project_id": null, "project_name": null,
            "exported_at": "2026-08-01T00:00:00Z",
            "counts": {"entities": 1, "observations": 1, "episodes": 0, "decisions": 0, "errors": 0, "relations": 0},
            "with_embeddings": true,
            "embedding_dim": dim,
            "embedding_model": memory_industry::embeddings::onnx::model_fingerprint()
        }))
        .expect("serialising the manifest"),
    )
    .expect("writing the manifest");
    std::fs::write(
        dir.join("entities").join("v051.json"),
        serde_json::to_vec_pretty(&json!({
            "id": entity.0, "name": entity.1, "entity_type": "technology",
            "importance": 0.5, "access_count": 0, "project_id": null,
            "created_at": "2026-08-01T00:00:00Z",
            "observations": [{
                "id": observation.0, "content": observation.1,
                "observation_type": "fact", "source": "agent", "importance": 0.5,
                "tags": [], "project_id": null, "session_id": null,
                "created_at": "2026-08-01T00:00:00Z",
                "embedding_model": OLD
            }]
        }))
        .expect("serialising the entity"),
    )
    .expect("writing the entity");
    let mut blob = observation.0.as_bytes().to_vec();
    for value in flat(0.5) {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(
        dir.join("embeddings.bin.zst"),
        memory_industry::sync::compressor::compress(&blob).expect("compressing the vectors"),
    )
    .expect("writing the vectors");
}

async fn sync_keeps_the_label(url: String) {
    let admin = connect_as_admin(&url).await;
    let name = format!("v051_sync_{}", &Uuid::new_v4().to_string()[..8]);
    let entity = an_entity(&admin, &name).await;
    let observation = an_observation(&admin, entity, OBSERVATION_TEXT).await;
    // Here the observation is already of the new generation, with its own vector.
    stamp(&admin, Table::Observations, observation, 0.25, NEW).await;

    let bundle = Scratch::new();
    write_a_bundle(
        &bundle.0,
        (entity, name.as_str()),
        (observation, OBSERVATION_TEXT),
    );
    let output = tokio::process::Command::new(BIN)
        .args(["sync", "import", "--json", "--dir"])
        .arg(&bundle.0)
        .env("DATABASE_URL", &url)
        .env("MEMORY_INDUSTRY_REQUIRE_DATABASE_URL", "1")
        .env("CUBA_SYNC_DIR", &bundle.0)
        .output()
        .await
        .expect("running `memory-industry sync import`");
    assert!(
        output.status.success(),
        "the import of a bundle with vectors must succeed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let now = stored(&admin, Table::Observations, observation).await;
    assert_eq!(
        now.vector.as_deref(),
        Some(flat(0.5).as_slice()),
        "the import writes the vector the bundle carries"
    );
    assert_eq!(
        now.label.as_deref(),
        Some(OLD),
        "and the label of the row it came with: under the new label it would be a vector of the old runtime that no `doctor` \
         and no `reembed` could tell from a good one"
    );
    admin.close().await;
}
