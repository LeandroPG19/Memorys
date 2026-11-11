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
//! CI-D5c, part C: the service re-embeds by itself when it starts, so what it rewrites and what a
//! sync import may overwrite are the same rule, «the generation of a row only moves forward»:
//!
//! - the automatic re-embed rewrites the rows that have no vector or that are of an earlier
//!   generation than the runtime loaded, and does not touch a newer runtime or another model, nor
//!   put them in the snapshot (C.11);
//! - it runs once, and says why it stopped: another re-embed holds the lock, or there is no place
//!   to keep the old vectors (C.12);
//! - an older vector that arrives by sync does not replace a newer one of the same text (C.13);
//! - as the role of the application and with the resting scope it sees every project in the four
//!   tables (C.14).
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
use memory_industry::handlers::sync::{VectorsRestored, restore_vectors};
use memory_industry::reembed_cli::{
    AutoSettings, AutoState, EmbedInput, Plan, auto_progress, drop_snapshot, forward_counts,
    reembed, reembed_is_running, rollback, run_automatic_reembed,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::Notify;
use uuid::Uuid;

const BIN: &str = env!("CARGO_BIN_EXE_memory-industry");

const OLD: &str = "multilingual-e5-small";
const OLD_RUNTIME: &str = "multilingual-e5-small@onnxruntime-1.26.0";
const NEW: &str = "multilingual-e5-small@onnxruntime-1.31.1";
const AHEAD: &str = "multilingual-e5-small@onnxruntime-1.40.0";
const OTHER_MODEL: &str = "e5-small@onnxruntime-1.26.0";
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
        forward_only: false,
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

    fn count_sql(self) -> &'static str {
        match self {
            Table::Observations => "SELECT count(*) FROM brain_observations WHERE id = $1",
            Table::Episodes => "SELECT count(*) FROM brain_episodes WHERE id = $1",
            Table::Chunks => "SELECT count(*) FROM brain_observation_chunks WHERE id = $1",
            Table::Procedures => "SELECT count(*) FROM brain_procedures WHERE id = $1",
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

/// `xmin` of each row: it moves with every write, so equal means nobody wrote the row.
async fn xmins_of(pool: &PgPool, rows: &[(Table, Uuid)]) -> Vec<String> {
    let mut xmins = Vec::new();
    for (table, id) in rows {
        xmins.push(stored(pool, *table, *id).await.xmin);
    }
    xmins
}

/// Five rows of one table, none with a vector yet, for the five kinds the automatic re-embed has
/// to tell apart (`give_each_kind`). A chunk hangs from `parent`, which is one of the observations
/// of the world, so the count of rows per table stays five.
async fn five_rows(pool: &PgPool, table: Table, entity: Uuid, parent: Option<Uuid>) -> [Uuid; 5] {
    let mut ids = Vec::new();
    for n in 0..5 {
        let text = format!("fila {n} de {}", table.name());
        ids.push(match table {
            Table::Observations => an_observation(pool, entity, &text).await,
            Table::Episodes => an_episode(pool, entity, &text).await,
            Table::Chunks => {
                let parent = parent.expect("a chunk hangs from an observation");
                a_chunk(pool, parent, n, &text).await
            }
            Table::Procedures => a_procedure(pool, &text, "cuando haga falta").await,
        });
    }
    ids.try_into().expect("five rows were made")
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

/// CI-D5c (B.12, as C changed it). A vector that arrives by sync carries the label it was made
/// with: under another one it would be a vector of one runtime that nothing could tell from a good
/// one. The world is the one the rule of C allows to move: the row here is of the old runtime
/// (1.26.0), the package comes from a machine on the new one (1.31.1), and the package wins and
/// keeps its label. (Before C the world was the other way round, and a package of an older runtime
/// won over a row of a newer one: that is the regression C.13 judges now.)
#[tokio::test]
async fn a_vector_that_arrives_by_sync_keeps_the_label_of_its_row() {
    in_a_scratch_database("v051_sync", sync_keeps_the_label).await;
}

/// The bundle of a machine: each observation with the `label` it has there and its vector (flat,
/// `0.5`) in `embeddings.bin.zst`, made by hand the way `v024_embedding_guard` makes one.
fn write_a_bundle(dir: &Path, entity: (Uuid, &str), observations: &[(Uuid, &str)], label: &str) {
    let dim = embedding_dim();
    std::fs::create_dir_all(dir.join("entities")).expect("the entities directory of the bundle");
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "manifest_hash": "irrelevant, the import recomputes it",
            "project_id": null, "project_name": null,
            "exported_at": "2026-08-01T00:00:00Z",
            "counts": {"entities": 1, "observations": observations.len(), "episodes": 0, "decisions": 0, "errors": 0, "relations": 0},
            "with_embeddings": true,
            "embedding_dim": dim,
            "embedding_model": memory_industry::embeddings::onnx::model_fingerprint()
        }))
        .expect("serialising the manifest"),
    )
    .expect("writing the manifest");
    let listed: Vec<Value> = observations
        .iter()
        .map(|(id, content)| {
            json!({
                "id": id, "content": content,
                "observation_type": "fact", "source": "agent", "importance": 0.5,
                "tags": [], "project_id": null, "session_id": null,
                "created_at": "2026-08-01T00:00:00Z",
                "embedding_model": label
            })
        })
        .collect();
    std::fs::write(
        dir.join("entities").join("v051.json"),
        serde_json::to_vec_pretty(&json!({
            "id": entity.0, "name": entity.1, "entity_type": "technology",
            "importance": 0.5, "access_count": 0, "project_id": null,
            "created_at": "2026-08-01T00:00:00Z",
            "observations": listed
        }))
        .expect("serialising the entity"),
    )
    .expect("writing the entity");
    let mut blob = Vec::new();
    for (id, _) in observations {
        blob.extend_from_slice(id.as_bytes());
        for value in flat(0.5) {
            blob.extend_from_slice(&value.to_le_bytes());
        }
    }
    std::fs::write(
        dir.join("embeddings.bin.zst"),
        memory_industry::sync::compressor::compress(&blob).expect("compressing the vectors"),
    )
    .expect("writing the vectors");
}

/// `memory-industry sync import --json --conflict <conflict>` of a bundle, on the database of
/// the test: the report the import printed.
async fn import_bundle(url: &str, bundle: &Path, conflict: &str) -> Value {
    let output = tokio::process::Command::new(BIN)
        .args(["sync", "import", "--json", "--conflict", conflict, "--dir"])
        .arg(bundle)
        .env("DATABASE_URL", url)
        .env("MEMORY_INDUSTRY_REQUIRE_DATABASE_URL", "1")
        .env("CUBA_SYNC_DIR", bundle)
        .output()
        .await
        .expect("running `memory-industry sync import`");
    assert!(
        output.status.success(),
        "the import of a bundle with vectors must succeed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<Value>(line).ok())
        .unwrap_or_else(|| {
            panic!(
                "the import printed no JSON report: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
}

async fn sync_keeps_the_label(url: String) {
    let admin = connect_as_admin(&url).await;
    let name = format!("v051_sync_{}", &Uuid::new_v4().to_string()[..8]);
    let entity = an_entity(&admin, &name).await;
    let observation = an_observation(&admin, entity, OBSERVATION_TEXT).await;
    // Here the observation is of the old runtime, with its own vector.
    stamp(&admin, Table::Observations, observation, 0.25, OLD_RUNTIME).await;

    let bundle = Scratch::new();
    write_a_bundle(
        &bundle.0,
        (entity, name.as_str()),
        &[(observation, OBSERVATION_TEXT)],
        NEW,
    );
    import_bundle(&url, &bundle.0, "merge").await;

    let now = stored(&admin, Table::Observations, observation).await;
    assert_eq!(
        now.vector.as_deref(),
        Some(flat(0.5).as_slice()),
        "the import writes the vector the bundle carries, which is of a newer runtime than the row"
    );
    assert_eq!(
        now.label.as_deref(),
        Some(NEW),
        "and the label of the row it came with: under another label it would be a vector of one runtime that no `doctor` \
         and no `reembed` could tell from a good one"
    );
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// C.11
// ---------------------------------------------------------------------------------------------

/// CI-D5c (C.11, as C2-S3 changed it). The automatic re-embed uses the core of `reembed` with
/// `forward_only`: it rewrites the rows that have a vector of an earlier generation than the
/// runtime loaded, and nothing else. A row with no vector at all is filled by the REM cycle, which
/// has its own limit and its own way to be turned off, and is not this one's. Five rows in each of
/// the four tables: the model alone (what every database of today holds), the old runtime, no
/// vector, a newer runtime (1.40.0) and another model. The first two end with the vector of the
/// encoder and the new label; the last three are not rewritten (`xmin` does not move) and are not
/// in the snapshot either: it keeps the old vector of what it rewrites, and these are not going to
/// be rewritten. The snapshot can then be dropped
/// while they remain, because it holds nothing that they need, and it cannot while a row that is
/// older than the generation is left.
#[tokio::test]
async fn the_automatic_reembed_rewrites_only_what_is_older_than_the_loaded_runtime() {
    in_a_scratch_database("v051_forward", rewrites_only_what_is_older).await;
}

/// The five kinds, in the order of `five_rows`, and the vector each one has (none for the third).
async fn give_each_kind(pool: &PgPool, table: Table, ids: [Uuid; 5]) {
    stamp(pool, table, ids[0], 0.125, OLD).await;
    stamp(pool, table, ids[1], 0.25, OLD_RUNTIME).await;
    stamp(pool, table, ids[3], 0.375, AHEAD).await;
    stamp(pool, table, ids[4], 0.625, OTHER_MODEL).await;
}

async fn rewrites_only_what_is_older(url: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "v051_forward").await;
    let observations = five_rows(&admin, Table::Observations, entity, None).await;
    let parent = Some(observations[2]);
    let mut kinds: Vec<(Table, [Uuid; 5])> = vec![(Table::Observations, observations)];
    for table in [Table::Episodes, Table::Chunks, Table::Procedures] {
        kinds.push((table, five_rows(&admin, table, entity, parent).await));
    }
    for (table, ids) in &kinds {
        give_each_kind(&admin, *table, *ids).await;
    }
    let left_alone: Vec<(Table, Uuid)> = kinds
        .iter()
        .flat_map(|(table, ids)| [(*table, ids[2]), (*table, ids[3]), (*table, ids[4])])
        .collect();
    let xmins_before = xmins_of(&admin, &left_alone).await;
    let coverage = vector_coverage(&admin, NEW)
        .await
        .expect("counting the vectors");
    assert_eq!(
        (coverage.stale(), coverage.missing()),
        (16, 4),
        "control: before, every row with a vector but one of the new generation is of another one"
    );

    let root = Scratch::new();
    let calls = Calls::default();
    let encode = calls.encoder();
    let forward = Plan {
        forward_only: true,
        ..plan(&root.0)
    };
    let report = reembed(&admin, &forward, &encode)
        .await
        .expect("the automatic re-embed of a database of mixed generations");
    assert_eq!(
        (report.total().fetched, report.total().rewritten),
        (8, 8),
        "two of the five kinds, in each of the four tables: {report:?}"
    );
    assert_eq!(
        calls.seen().len(),
        8,
        "and the encoder was asked for no more"
    );

    for (table, ids) in &kinds {
        for id in &ids[..2] {
            let now = stored(&admin, *table, *id).await;
            assert_eq!(
                now.vector.as_deref(),
                Some(flat(0.5).as_slice()),
                "{}: the model alone and the old runtime get the vector of the encoder",
                table.name()
            );
            assert_eq!(now.label.as_deref(), Some(NEW), "{}", table.name());
        }
    }
    assert_eq!(
        xmins_of(&admin, &left_alone).await,
        xmins_before,
        "a row with no vector, or with a vector of a newer runtime or of another model, was written: the service only moves a vector forward"
    );
    let (_, saved_rows) = saved(&report.snapshot);
    assert_eq!(
        saved_rows.len(),
        8,
        "the snapshot has the old vector of the rows that had one and were rewritten: two per table, \
         and not the one that had none: {saved_rows:?}"
    );
    for (table, id) in &left_alone {
        assert!(
            !saved_rows.iter().any(|row| row["id"] == id.to_string()),
            "{} {id} is not rewritten, so its vector is not in the snapshot",
            table.name()
        );
    }
    let after = vector_coverage(&admin, NEW)
        .await
        .expect("counting the vectors");
    assert_eq!(
        (after.stale(), after.missing()),
        (8, 4),
        "what is left is the two rows of the other generations in each table, and the row with no vector"
    );

    // A row of the old generation that turns up while the snapshot is there keeps it.
    let late = an_observation(&admin, entity, "una fila que llega despues").await;
    stamp(&admin, Table::Observations, late, 0.125, OLD).await;
    drop_snapshot(&admin, &root.0, NEW)
        .await
        .expect_err("a row older than the generation is left: the snapshot is the only place its vector is kept");
    assert!(report.snapshot.is_dir(), "and the refusal deleted nothing");
    let again = reembed(&admin, &forward, &encode)
        .await
        .expect("the second run");
    assert_eq!(
        again.total().rewritten,
        1,
        "the late row, and nothing else: {again:?}"
    );
    let dropped = drop_snapshot(&admin, &root.0, NEW).await.expect(
        "nothing older is left: the rows of a newer runtime and of another model do not need it",
    );
    assert!(!dropped.exists(), "the snapshot is gone");
    assert_eq!(
        xmins_of(&admin, &left_alone).await,
        xmins_before,
        "and the two runs left the rows of the other generations as they were"
    );
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// C.12
// ---------------------------------------------------------------------------------------------

/// CI-D5c (C.12). The automatic re-embed runs once and leaves in `auto_progress()` why it stopped.
/// Three worlds in one database, and the same four old rows in all of them. With another re-embed
/// holding the lock (a real one, stopped inside its encoder, which fails every row so that it
/// writes nothing) it does not compete or wait: `busy`, naming the other re-embed, no encoder call,
/// no snapshot, no row written. With a snapshot root that cannot be made it is `failed`, and says
/// the figure that is missing in the words of the error: how much room the snapshot of that many
/// vectors needs. With a good root it is `done`, with every row rewritten.
#[tokio::test]
async fn the_automatic_reembed_runs_once_and_says_why_it_stopped() {
    in_a_scratch_database("v051_auto", runs_once_and_says_why).await;
}

fn settings(root: PathBuf) -> AutoSettings {
    AutoSettings {
        snapshot_root: root,
        node_id: NODE.to_string(),
        tag: NEW.to_string(),
        chunk: 100,
    }
}

async fn runs_once_and_says_why(url: String) {
    let admin = connect_as_admin(&url).await;
    let world = a_world_of_the_old_runtime(&admin).await;
    let rows: Vec<(Table, Uuid)> = world
        .rows()
        .into_iter()
        .map(|(table, id, ..)| (table, id))
        .collect();
    let xmins_before = xmins_of(&admin, &rows).await;
    let root = Scratch::new();
    let calls = Calls::default();
    let encode = calls.encoder();

    // «The other re-embed» is a real one and holds the lock from inside its first encoder call. No
    // timing and no lock of the test: the barrier is the encoder.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let holding = {
        let (entered, release, stopped) = (entered.clone(), release.clone(), stopped.clone());
        move |_input: EmbedInput| -> BoxFuture<'static, Result<Vec<f32>>> {
            let (entered, release, stopped) = (entered.clone(), release.clone(), stopped.clone());
            async move {
                if !stopped.swap(true, Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                Err(anyhow::anyhow!(
                    "the one that holds the lock writes nothing"
                ))
            }
            .boxed()
        }
    };
    let holder = tokio::spawn({
        let (pool, plan) = (admin.clone(), plan(&root.0.join("holder")));
        async move { reembed(&pool, &plan, &holding).await }
    });
    entered.notified().await;

    let busy = run_automatic_reembed(&admin, &settings(root.0.join("auto")), &encode).await;
    assert_eq!(busy.state, AutoState::Busy, "{busy:?}");
    assert!(
        busy.last_error
            .as_deref()
            .is_some_and(|why| why.contains("otro reembebido")),
        "it says another re-embed holds the lock: {:?}",
        busy.last_error
    );
    assert_eq!(
        auto_progress().state,
        AutoState::Busy,
        "and leaves it where /health reads it"
    );
    assert!(calls.seen().is_empty(), "it encoded nothing");
    assert!(
        !root.0.join("auto").exists(),
        "and made no snapshot: it did nothing at all"
    );
    assert_eq!(
        xmins_of(&admin, &rows).await,
        xmins_before,
        "no row was written"
    );

    release.notify_one();
    let held = holder
        .await
        .expect("the re-embed that held the lock was not cut")
        .expect("and ends well once its encoder answers");
    assert_eq!(
        (held.total().rewritten, held.total().failed),
        (0, 4),
        "control: it was a re-embed like any other whose encoder failed, so the state above was about the lock: {held:?}"
    );

    // A root that cannot be made: its parent is a file.
    let blocker = Scratch::new();
    std::fs::create_dir_all(&blocker.0).expect("a directory for the blocker");
    let file = blocker.0.join("not-a-directory");
    std::fs::write(&file, b"x").expect("a file where the snapshot root should go");
    let failed = run_automatic_reembed(&admin, &settings(file.join("reembed")), &encode).await;
    assert_eq!(failed.state, AutoState::Failed, "{failed:?}");
    let why = failed.last_error.clone().unwrap_or_default();
    assert!(
        why.starts_with("sin instantánea no reescribo"),
        "it says why nothing was written first: {why}"
    );
    assert!(
        why.contains("hacen falta") && why.contains("MB") && why.contains("4 vectores"),
        "and how much room the snapshot of these four vectors needs: {why}"
    );
    assert_eq!(auto_progress().state, AutoState::Failed);
    assert!(
        calls.seen().is_empty(),
        "with no place for the old vectors it encoded nothing"
    );
    assert_eq!(
        xmins_of(&admin, &rows).await,
        xmins_before,
        "and wrote no row"
    );

    let done = run_automatic_reembed(&admin, &settings(root.0.join("good")), &encode).await;
    assert_eq!(done.state, AutoState::Done, "{done:?}");
    assert_eq!(
        (done.rewritten, done.changed, done.failed),
        (4, 0, 0),
        "the four old rows: {done:?}"
    );
    assert!(done.last_error.is_none(), "{:?}", done.last_error);
    let published = auto_progress();
    assert_eq!(
        (published.state, published.rewritten),
        (AutoState::Done, 4),
        "the progress that /health shows says the same"
    );
    assert_eq!(calls.seen().len(), 4);
    for (table, id) in &rows {
        let now = stored(&admin, *table, *id).await;
        assert_eq!(now.label.as_deref(), Some(NEW), "{}", table.name());
        assert_eq!(now.vector.as_deref(), Some(flat(0.5).as_slice()));
    }
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// C.13
// ---------------------------------------------------------------------------------------------

/// CI-D5c (C.13). `cuba_sync` exports every observation of an entity and the import writes the
/// vector of every one the package carries, so a package from a machine on an older runtime would
/// hand back to a row what this machine had just re-embedded, and the next start would re-embed it
/// again for ever. The import takes the incoming vector only if it is not older than the one the row
/// has. Two observations of the same entity that this machine has on the new runtime: one comes in
/// the package with the same text and the label of the model alone, and keeps its vector and its
/// label, and the report counts it; the other comes with another text under `--conflict
/// overwrite`, which takes the text and clears the vector, so the row has nothing to protect and
/// takes the vector and the label of the package.
#[tokio::test]
async fn an_older_vector_that_arrives_by_sync_does_not_replace_a_newer_one_of_the_same_text() {
    in_a_scratch_database("v051_newer", sync_keeps_the_newer_vector).await;
}

async fn sync_keeps_the_newer_vector(url: String) {
    const BEFORE: &str = "texto de antes de la correccion";
    const AFTER: &str = "texto de despues de la correccion";
    let admin = connect_as_admin(&url).await;
    let name = format!("v051_newer_{}", &Uuid::new_v4().to_string()[..8]);
    let entity = an_entity(&admin, &name).await;
    let same = an_observation(&admin, entity, OBSERVATION_TEXT).await;
    let edited = an_observation(&admin, entity, BEFORE).await;
    stamp(&admin, Table::Observations, same, 0.25, NEW).await;
    stamp(&admin, Table::Observations, edited, 0.25, NEW).await;

    let bundle = Scratch::new();
    write_a_bundle(
        &bundle.0,
        (entity, name.as_str()),
        &[(same, OBSERVATION_TEXT), (edited, AFTER)],
        OLD,
    );
    let report = import_bundle(&url, &bundle.0, "overwrite").await;

    let kept = stored(&admin, Table::Observations, same).await;
    assert_eq!(
        kept.vector.as_deref(),
        Some(flat(0.25).as_slice()),
        "the package carries a vector of the model alone for a text this machine already has on the new runtime: it keeps its own"
    );
    assert_eq!(
        kept.label.as_deref(),
        Some(NEW),
        "and the label that goes with it"
    );
    assert_eq!(
        report["embeddings_kept_newer"], 1,
        "the report counts what it did not take: {report}"
    );

    let content: String =
        sqlx::query_scalar("SELECT content FROM brain_observations WHERE id = $1")
            .bind(edited)
            .fetch_one(&admin)
            .await
            .expect("reading the text back");
    assert_eq!(content, AFTER, "control: the overwrite took the other text");
    let taken = stored(&admin, Table::Observations, edited).await;
    assert_eq!(
        taken.vector.as_deref(),
        Some(flat(0.5).as_slice()),
        "the vector the row had was of another text and was cleared: the one of the package is the only one that fits"
    );
    assert_eq!(
        taken.label.as_deref(),
        Some(OLD),
        "with the label of the package, which the next start of the service is going to move forward"
    );
    assert_eq!(
        report["embeddings_restored"], 1,
        "and it is the only one the import wrote: {report}"
    );
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// C.14
// ---------------------------------------------------------------------------------------------

/// CI-D5c (C.14). The service re-embeds without a client, so its pool has no project: the resting
/// scope is the empty one, which the policies of the four tables read as «all of them». And that
/// has to hold for the role the daemon really runs as, `cuba_app`, which does not bypass the row
/// policies (the owner of a scratch database does, and a test as the owner would prove nothing).
/// Two projects, one row of each in the four tables, all of the old runtime. First the anchor: as
/// `cuba_app` with the scope of one project, the other project's row is not seen in any table, so
/// the policy is in force here. Then the re-embed through a pool that enters as `cuba_app` and
/// sets the scope as the production pool does on every acquire, which is the resting one: the eight
/// rows get the new label.
#[tokio::test]
async fn as_the_app_role_the_automatic_reembed_sees_every_project_in_the_four_tables() {
    in_a_scratch_database("v051_rls", sees_every_project).await;
}

async fn sees_every_project(url: String) {
    let admin = connect_as_admin(&url).await;
    let mut projects: Vec<Uuid> = Vec::new();
    let mut rows: Vec<[(Table, Uuid); 4]> = Vec::new();
    for name in ["v051_rls_a", "v051_rls_b"] {
        let project: Uuid =
            sqlx::query_scalar("INSERT INTO brain_projects (name) VALUES ($1) RETURNING id")
                .bind(name)
                .fetch_one(&admin)
                .await
                .expect("seeding a project");
        let entity: Uuid = sqlx::query_scalar(
            "INSERT INTO brain_entities (name, entity_type, project_id)
             VALUES ($1, 'technology', $2) RETURNING id",
        )
        .bind(format!("{name}_entity"))
        .bind(project)
        .fetch_one(&admin)
        .await
        .expect("seeding an entity of the project");
        let observation: Uuid = sqlx::query_scalar(
            "INSERT INTO brain_observations
                 (entity_id, content, observation_type, source, importance, project_id)
             VALUES ($1, $2, 'fact', 'agent', 0.5, $3) RETURNING id",
        )
        .bind(entity)
        .bind(format!("observacion de {name}"))
        .bind(project)
        .fetch_one(&admin)
        .await
        .expect("seeding an observation of the project");
        let episode: Uuid = sqlx::query_scalar(
            "INSERT INTO brain_episodes (entity_id, content, importance, project_id)
             VALUES ($1, $2, 0.5, $3) RETURNING id",
        )
        .bind(entity)
        .bind(format!("episodio de {name}"))
        .bind(project)
        .fetch_one(&admin)
        .await
        .expect("seeding an episode of the project");
        let chunk: Uuid = sqlx::query_scalar(
            "INSERT INTO brain_observation_chunks (observation_id, chunk_index, content, project_id)
             VALUES ($1, 0, $2, $3) RETURNING id",
        )
        .bind(observation)
        .bind(format!("fragmento de {name}"))
        .bind(project)
        .fetch_one(&admin)
        .await
        .expect("seeding a chunk of the project");
        let procedure: Uuid = sqlx::query_scalar(
            "INSERT INTO brain_procedures (name, trigger_context, project_id)
             VALUES ($1, 'cuando haga falta', $2) RETURNING id",
        )
        .bind(format!("procedimiento de {name}"))
        .bind(project)
        .fetch_one(&admin)
        .await
        .expect("seeding a procedure of the project");
        let made = [
            (Table::Observations, observation),
            (Table::Episodes, episode),
            (Table::Chunks, chunk),
            (Table::Procedures, procedure),
        ];
        for (table, id) in made {
            stamp(&admin, table, id, 0.125, OLD).await;
        }
        projects.push(project);
        rows.push(made);
    }

    // The anchor: with the scope of the first project, `cuba_app` does not see the second one's rows.
    {
        let mut conn = admin.acquire().await.expect("a connection");
        sqlx::query("SET ROLE cuba_app")
            .execute(&mut *conn)
            .await
            .expect("cuba_app exists in a migrated database");
        sqlx::query("SELECT set_config('app.current_project', $1, false)")
            .bind(projects[0].to_string())
            .execute(&mut *conn)
            .await
            .expect("declaring the scope of a project");
        for (&(table, mine), &(_, theirs)) in rows[0].iter().zip(rows[1].iter()) {
            let seen = |id: Uuid| sqlx::query_scalar::<_, i64>(table.count_sql()).bind(id);
            let (own, other): (i64, i64) = (
                seen(mine).fetch_one(&mut *conn).await.expect("counting"),
                seen(theirs).fetch_one(&mut *conn).await.expect("counting"),
            );
            assert_eq!(
                (own, other),
                (1, 0),
                "{}: with the scope of a project the row of the other one is not seen, so the policy is in force for this role",
                table.name()
            );
        }
        sqlx::query("RESET ROLE")
            .execute(&mut *conn)
            .await
            .expect("leaving the role");
        sqlx::query("SELECT set_config('app.current_project', '', false)")
            .execute(&mut *conn)
            .await
            .expect("back to the resting scope");
    }

    let app = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET ROLE cuba_app").execute(&mut *conn).await?;
                sqlx::query("SELECT set_config('app.current_project', '', false)")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .before_acquire(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SELECT set_config('app.current_project', $1, false)")
                    .bind(memory_industry::project::rls_scope())
                    .execute(&mut *conn)
                    .await?;
                Ok(true)
            })
        })
        .connect(&url)
        .await
        .expect("connecting as the application role");
    let root = Scratch::new();
    let calls = Calls::default();
    let encode = calls.encoder();
    let forward = Plan {
        forward_only: true,
        ..plan(&root.0)
    };
    let report = reembed(&app, &forward, &encode)
        .await
        .expect("the automatic re-embed as the application role");
    assert_eq!(
        report.total().rewritten,
        8,
        "two projects, one row in each of the four tables: {report:?}"
    );
    for (table, id) in rows.iter().flatten() {
        let now = stored(&admin, *table, *id).await;
        assert_eq!(
            now.label.as_deref(),
            Some(NEW),
            "{}: the re-embed has no project and sees them all",
            table.name()
        );
    }
    app.close().await;
    admin.close().await;
}

// ---------------------------------------------------------------------------------------------
// C2: what the review of the first round found
// ---------------------------------------------------------------------------------------------

/// A real re-embed that holds the lock from inside its first encoder call and writes nothing (its
/// encoder fails every row). The barrier is the encoder, so there is no timing and no lock of the
/// test: `entered` says the lock is taken, `release` lets it end.
struct Holder {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    task: tokio::task::JoinHandle<Result<memory_industry::reembed_cli::Report>>,
}

fn a_reembed_that_holds_the_lock(pool: &PgPool, root: &Path) -> Holder {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let stopped = Arc::new(AtomicBool::new(false));
    let holding = {
        let (entered, release, stopped) = (entered.clone(), release.clone(), stopped.clone());
        move |_input: EmbedInput| -> BoxFuture<'static, Result<Vec<f32>>> {
            let (entered, release, stopped) = (entered.clone(), release.clone(), stopped.clone());
            async move {
                if !stopped.swap(true, Ordering::SeqCst) {
                    entered.notify_one();
                    release.notified().await;
                }
                Err(anyhow::anyhow!(
                    "the one that holds the lock writes nothing"
                ))
            }
            .boxed()
        }
    };
    let task = tokio::spawn({
        let (pool, plan) = (pool.clone(), plan(root));
        async move { reembed(&pool, &plan, &holding).await }
    });
    Holder {
        entered,
        release,
        task,
    }
}

// C2-A4 --------------------------------------------------------------------------------------

/// CI-D5c-C2 (A4). The import reads the label and the vector a row has, and writes the incoming
/// vector afterwards, with nobody holding the row between the two. The automatic re-embed can
/// rewrite that row in between, and the import would put over it the older vector it had decided
/// to take. The write repeats what the read saw (`WHERE` the label and the presence of a vector are
/// still those) and a row that no longer matches is counted as kept. The seam is the argument of
/// `restore_vectors` that runs between the read and the first write: here it is a second connection
/// that rewrites the contested row and commits, so the interleaving is the test's and not a
/// matter of timing. The other row is the control: nobody touches it and it takes the vector.
#[tokio::test]
async fn an_incoming_vector_never_overwrites_what_another_writer_changed_after_it_was_read() {
    in_a_scratch_database("v051_race_in", import_loses_the_race_cleanly).await;
}

async fn import_loses_the_race_cleanly(url: String) {
    let admin = connect_as_admin(&url).await;
    let entity = an_entity(&admin, "v051_race_in").await;
    let contested = an_observation(&admin, entity, "la fila que otro reescribe").await;
    let control = an_observation(&admin, entity, "la fila que nadie toca").await;
    stamp(&admin, Table::Observations, contested, 0.125, OLD).await;
    stamp(&admin, Table::Observations, control, 0.125, OLD).await;

    let mut raw = Vec::new();
    let mut labels = std::collections::HashMap::new();
    for id in [contested, control] {
        raw.extend_from_slice(id.as_bytes());
        for value in flat(0.5) {
            raw.extend_from_slice(&value.to_le_bytes());
        }
        labels.insert(id, Some(OLD.to_string()));
    }
    let other = admin.clone();
    let between = move || -> BoxFuture<'static, ()> {
        let other = other.clone();
        async move { stamp(&other, Table::Observations, contested, 0.875, NEW).await }.boxed()
    };

    let mut tx = admin.begin().await.expect("the transaction of the import");
    let done = restore_vectors(&mut tx, &raw, embedding_dim(), &labels, &between)
        .await
        .expect("restoring the vectors of a package");
    tx.commit().await.expect("committing the import");

    assert_eq!(
        done,
        VectorsRestored {
            restored: 1,
            kept_newer: 1
        },
        "the control took the vector; the contested row changed after it was read and is kept"
    );
    let kept = stored(&admin, Table::Observations, contested).await;
    assert_eq!(
        (kept.vector.as_deref(), kept.label.as_deref()),
        (Some(flat(0.875).as_slice()), Some(NEW)),
        "what the other writer put is still there: the import did not put the older vector over it"
    );
    let taken = stored(&admin, Table::Observations, control).await;
    assert_eq!(
        (taken.vector.as_deref(), taken.label.as_deref()),
        (Some(flat(0.5).as_slice()), Some(OLD)),
        "control: a row nobody touched takes the vector of the package, so the one above was kept for a reason"
    );
    admin.close().await;
}

// C2-S1 --------------------------------------------------------------------------------------

/// CI-D5c-C2 (S1). `doctor` says «reembebido en curso» from the advisory lock in `pg_locks`, and
/// `pg_locks` shows the locks of every database of the cluster. A re-embed that runs in another
/// database (a second install on the same server, or another test of the judge) is not this
/// database's: the question is asked about the current one. A real re-embed stopped inside its
/// encoder holds the lock here; the database where it runs sees it, a second scratch database of
/// the same cluster does not, and once it ends nobody does.
#[tokio::test]
async fn a_running_reembed_is_seen_in_its_own_database_and_not_in_another_of_the_cluster() {
    in_a_scratch_database("v051_running", sees_it_only_at_home).await;
}

async fn sees_it_only_at_home(url: String) {
    let admin = connect_as_admin(&url).await;
    let _world = a_world_of_the_old_runtime(&admin).await;
    let root = Scratch::new();
    assert!(
        !reembed_is_running(&admin).await,
        "control: nothing holds the lock yet"
    );

    let holder = a_reembed_that_holds_the_lock(&admin, &root.0);
    holder.entered.notified().await;
    assert!(
        reembed_is_running(&admin).await,
        "a re-embed holds the lock of this database"
    );
    in_a_scratch_database("v051_elsewhere", |other_url| async move {
        let other = connect_as_admin(&other_url).await;
        assert!(
            !reembed_is_running(&other).await,
            "the lock is held in another database of the cluster, and pg_locks shows it: it is not this one's"
        );
        other.close().await;
    })
    .await;

    holder.release.notify_one();
    holder
        .task
        .await
        .expect("the re-embed was not cut")
        .expect("and ends well once its encoder answers");
    assert!(
        !reembed_is_running(&admin).await,
        "it ended and let go of the lock"
    );
    admin.close().await;
}

// C2-S4 --------------------------------------------------------------------------------------

/// CI-D5c-C2 (S4). The count of what the automatic re-embed has to rewrite and the pages it reads
/// are the same rows: a row the pages never return (an episode whose entity is gone, which the
/// foreign key forbids and a restore or an import with checks off can leave) would count as older
/// forever and the run would end `done` with fewer rewritten than counted, and the REM cycle
/// would start it again each time. The orphan is made on a connection with the foreign keys
/// switched off, which is how one is made.
#[tokio::test]
async fn a_row_the_pages_cannot_read_is_not_counted_as_older() {
    in_a_scratch_database("v051_orphan", counts_what_the_pages_read).await;
}

async fn counts_what_the_pages_read(url: String) {
    let admin = connect_as_admin(&url).await;
    let _world = a_world_of_the_old_runtime(&admin).await;
    let orphan = {
        let mut conn = admin.acquire().await.expect("a connection");
        sqlx::query("SET session_replication_role = replica")
            .execute(&mut *conn)
            .await
            .expect("switching the foreign keys off for this connection");
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO brain_episodes (entity_id, content, importance) VALUES ($1, $2, 0.5)
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind("un episodio sin entidad")
        .fetch_one(&mut *conn)
        .await
        .expect("seeding an episode whose entity does not exist");
        sqlx::query("RESET session_replication_role")
            .execute(&mut *conn)
            .await
            .expect("switching them back on");
        id
    };
    stamp(&admin, Table::Episodes, orphan, 0.125, OLD).await;
    assert_eq!(
        vector_coverage(&admin, NEW)
            .await
            .expect("counting")
            .stale(),
        5,
        "control: five rows have an old vector, the orphan among them"
    );

    let counts = forward_counts(&admin, NEW)
        .await
        .expect("counting by generation");
    assert_eq!(
        (counts.older, counts.ahead),
        (4, 0),
        "the four rows of the world: the orphan is not read by any page, so it is not counted"
    );

    let root = Scratch::new();
    let calls = Calls::default();
    let encode = calls.encoder();
    let run = run_automatic_reembed(&admin, &settings(root.0.join("auto")), &encode).await;
    assert_eq!(run.state, AutoState::Done, "{run:?}");
    assert_eq!(
        (run.older, run.rewritten),
        (4, 4),
        "what it counted is what it rewrote: {run:?}"
    );
    admin.close().await;
}

// C2-A1 --------------------------------------------------------------------------------------

/// CI-D5c-C2 (A1). A write that cannot reach the database ends the batch. The encoder closes the
/// pool on its first call (the call that marks it closed does not wait: the connection that holds
/// the lock is still out), so the write of that row finds a closed pool, and every other row would
/// find it too and ask the encoder first. The run ends with an error that says the connection was
/// lost and the encoder was asked once, not four times; nothing is counted on a clock.
#[tokio::test]
async fn a_write_that_cannot_reach_the_database_ends_the_batch_instead_of_trying_every_row() {
    in_a_scratch_database("v051_gone", ends_the_batch).await;
}

async fn ends_the_batch(url: String) {
    let admin = connect_as_admin(&url).await;
    let world = a_world_of_the_old_runtime(&admin).await;
    let rows: Vec<(Table, Uuid)> = world
        .rows()
        .into_iter()
        .map(|(table, id, ..)| (table, id))
        .collect();
    let checker = connect_as_admin(&url).await;
    let xmins_before = xmins_of(&checker, &rows).await;
    let asked = Arc::new(AtomicUsize::new(0));
    let closing = {
        let (pool, asked) = (admin.clone(), asked.clone());
        move |_input: EmbedInput| -> BoxFuture<'static, Result<Vec<f32>>> {
            let (pool, asked) = (pool.clone(), asked.clone());
            async move {
                asked.fetch_add(1, Ordering::SeqCst);
                // `close()` marks the pool closed when it is called; polling its future once is
                // enough, and the rest of it waits for the connection of the lock, which is ours.
                let _waiting = futures::poll!(std::pin::pin!(pool.close()));
                anyhow::Ok(flat(0.5))
            }
            .boxed()
        }
    };
    let root = Scratch::new();

    let lost = reembed(&admin, &plan(&root.0), &closing)
        .await
        .expect_err("a write that cannot reach the database ends the run");
    let said = format!("{lost:#}");
    assert!(
        said.contains("se perdió la conexión"),
        "it says what happened: {said}"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the encoder was asked once: the other three rows were not tried"
    );
    assert_eq!(
        xmins_of(&checker, &rows).await,
        xmins_before,
        "and no row was written"
    );
    checker.close().await;
    admin.close().await;
}

// C2-A3 --------------------------------------------------------------------------------------

/// CI-D5c-C2 (A3). The automatic re-embed does its work through `side_pool(&pool, 2)` so that the
/// connections of the daemon stay for its requests. The pool that has none left is the case that
/// matters: a main pool of one connection, held, and a side pool that has to answer anyway, with
/// two connections of its own, none of them the main one's. The proof is the backend each one is:
/// if the side pool were the main one, its first connection would be the held one or none, and the
/// 2 seconds of the acquire timeout of the main pool are only the end of a failure.
#[tokio::test]
async fn the_side_pool_has_connections_of_its_own_when_the_main_one_has_none_left() {
    in_a_scratch_database("v051_side", side_pool_is_independent).await;
}

async fn side_pool_is_independent(url: String) {
    let main = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect(&url)
        .await
        .expect("a main pool of one connection");
    let mut held = main.acquire().await.expect("its only connection");
    let held_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *held)
        .await
        .expect("the backend of the held connection");

    let side = memory_industry::db::side_pool(&main, 2);
    let mut first = side
        .acquire()
        .await
        .expect("the side pool answers with the main one exhausted");
    let mut second = side
        .acquire()
        .await
        .expect("and has a second connection of its own");
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *first)
        .await
        .expect("the backend of the first");
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *second)
        .await
        .expect("the backend of the second");
    assert!(
        held_pid != first_pid && held_pid != second_pid && first_pid != second_pid,
        "three connections, three backends: {held_pid} (main), {first_pid} and {second_pid} (side)"
    );
    drop((first, second, held));
    side.close().await;
    main.close().await;
}
