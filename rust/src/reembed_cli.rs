use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::pool::PoolConnection;
use uuid::Uuid;

use crate::embeddings::onnx::{RUNTIME_MARK, is_older_generation};

/// The advisory lock two reembeds (the command and `cuba_zafra action=reembed`) take, so that only
/// one rewrites vectors in a database at a time. A different number from `REM_LOCK` and
/// `SYNC_LOCK`: a REM cycle runs while a reembed does.
pub const REEMBED_LOCK: i64 = 0x0CBA_A0D1_7106_0051;

const SNAPSHOT_FORMAT: &str = "memory-industry-vectors/1";
const UNLABELLED: &str = "(sin etiqueta)";
const NO_SNAPSHOT: &str = "sin instantánea no reescribo";

/// Rows per part of the snapshot in the automatic re-embed: a part of 100 rows is about 0.45 MB
/// with 384 dimensions, so a cut loses little and the `fsync` of each part is short.
const AUTO_CHUNK: usize = 100;

/// What the encoder is asked for. The text of an observation, an episode or a chunk is embedded
/// with the entity it hangs from, as the write path does it; a procedure is `name. trigger` alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedInput {
    Contextual {
        text: String,
        entity_type: String,
        entity_name: String,
    },
    Bare {
        text: String,
    },
}

/// The encoder is injected: the command passes `onnx_encoder`, the tests a function that needs no model.
pub type Encoder = dyn Fn(EmbedInput) -> BoxFuture<'static, Result<Vec<f32>>> + Send + Sync;

pub fn onnx_encoder(input: EmbedInput) -> BoxFuture<'static, Result<Vec<f32>>> {
    Box::pin(async move {
        match input {
            EmbedInput::Contextual {
                text,
                entity_type,
                entity_name,
            } => {
                crate::embeddings::onnx::embed_passage_contextual(&text, &entity_type, &entity_name)
                    .await
            }
            EmbedInput::Bare { text } => crate::embeddings::onnx::embed_passage(&text).await,
        }
    })
}

/// What one run of the reembed does. `tag` is the label the new vectors get (the model and the
/// runtime loaded now); `snapshot_root` is the directory under which the snapshot of each
/// generation gets its own directory, and `node_id` is written in the header of each part.
/// `limit` caps the rows taken per table (the tool's `batch_size`), `chunk` is how many go into one
/// part of the snapshot, and `progress` prints to stderr where the snapshot is and how far it got.
/// With `forward_only` a row that has a vector is rewritten only if it is of an earlier generation
/// than `tag` (`is_older_generation`), so a vector of a newer runtime or of another model is never
/// put back: that is what the automatic re-embed does, and what the command and `cuba_zafra` do not
/// (what is asked for by hand rewrites everything that differs).
#[derive(Debug, Clone)]
pub struct Plan {
    pub tag: String,
    pub all: bool,
    pub chunk: usize,
    pub limit: Option<usize>,
    pub node_id: String,
    pub snapshot_root: PathBuf,
    pub progress: bool,
    pub forward_only: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TableReport {
    pub fetched: usize,
    pub rewritten: usize,
    pub changed: usize,
    pub failed: usize,
}

impl TableReport {
    fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Rewritten => self.rewritten += 1,
            Outcome::Changed => self.changed += 1,
            Outcome::Failed => self.failed += 1,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub observations: TableReport,
    pub episodes: TableReport,
    pub chunks: TableReport,
    pub procedures: TableReport,
    /// Rows this run added to the snapshot, and with which label each one was made.
    pub saved: usize,
    pub old_labels: BTreeMap<String, usize>,
    pub snapshot: PathBuf,
}

impl Report {
    pub fn total(&self) -> TableReport {
        [
            self.observations,
            self.episodes,
            self.chunks,
            self.procedures,
        ]
        .iter()
        .fold(TableReport::default(), |sum, table| TableReport {
            fetched: sum.fetched + table.fetched,
            rewritten: sum.rewritten + table.rewritten,
            changed: sum.changed + table.changed,
            failed: sum.failed + table.failed,
        })
    }

    fn table_mut(&mut self, table: Table) -> &mut TableReport {
        match table {
            Table::Observations => &mut self.observations,
            Table::Episodes => &mut self.episodes,
            Table::Chunks => &mut self.chunks,
            Table::Procedures => &mut self.procedures,
        }
    }
}

enum Outcome {
    Rewritten,
    Changed,
    Failed,
}

/// The four tables that carry a vector. Every statement is a literal: a table name is never
/// spliced into SQL. `pending_sql` reads the rows to rewrite after `$1` in id order (`$2` asks for
/// all of them, `$3` is the new label, `$4` the page size); `rewrite_sql` and `restore_sql` write
/// only if the row is still the one that was read, by label, by presence of a vector and by the
/// fingerprint of the text the vector is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Table {
    Observations,
    Episodes,
    Chunks,
    Procedures,
}

const TABLES: [Table; 4] = [
    Table::Observations,
    Table::Episodes,
    Table::Chunks,
    Table::Procedures,
];

impl Table {
    const fn name(self) -> &'static str {
        match self {
            Self::Observations => "brain_observations",
            Self::Episodes => "brain_episodes",
            Self::Chunks => "brain_observation_chunks",
            Self::Procedures => "brain_procedures",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        TABLES.into_iter().find(|table| table.name() == name)
    }

    const fn pending_sql(self) -> &'static str {
        match self {
            Self::Observations => {
                "SELECT o.id, o.content AS text, md5(o.content) AS digest,
                        o.embedding_model AS label, o.embedding AS vector,
                        e.entity_type, e.name AS entity_name
                 FROM brain_observations o
                 JOIN brain_entities e ON e.id = o.entity_id
                 WHERE o.id > $1
                   AND ($2 OR o.embedding IS NULL OR o.embedding_model IS DISTINCT FROM $3)
                 ORDER BY o.id LIMIT $4"
            }
            Self::Episodes => {
                "SELECT ep.id, ep.content AS text, md5(ep.content) AS digest,
                        ep.embedding_model AS label, ep.embedding AS vector,
                        e.entity_type, e.name AS entity_name
                 FROM brain_episodes ep
                 JOIN brain_entities e ON e.id = ep.entity_id
                 WHERE ep.id > $1
                   AND ($2 OR ep.embedding IS NULL OR ep.embedding_model IS DISTINCT FROM $3)
                 ORDER BY ep.id LIMIT $4"
            }
            Self::Chunks => {
                "SELECT c.id, c.content AS text, md5(c.content) AS digest,
                        c.embedding_model AS label, c.embedding AS vector,
                        e.entity_type, e.name AS entity_name
                 FROM brain_observation_chunks c
                 JOIN brain_observations o ON o.id = c.observation_id
                 JOIN brain_entities e ON e.id = o.entity_id
                 WHERE c.id > $1
                   AND ($2 OR c.embedding IS NULL OR c.embedding_model IS DISTINCT FROM $3)
                 ORDER BY c.id LIMIT $4"
            }
            Self::Procedures => {
                "SELECT p.id, p.name || '. ' || p.trigger_context AS text,
                        md5(p.name || '. ' || p.trigger_context) AS digest,
                        p.embedding_model AS label, p.embedding AS vector,
                        ''::text AS entity_type, ''::text AS entity_name
                 FROM brain_procedures p
                 WHERE p.id > $1
                   AND ($2 OR p.embedding IS NULL OR p.embedding_model IS DISTINCT FROM $3)
                 ORDER BY p.id LIMIT $4"
            }
        }
    }

    /// `$1` vector, `$2` new label, `$3` id, `$4` label read, `$5` whether it had no vector, `$6` fingerprint.
    const fn rewrite_sql(self) -> &'static str {
        match self {
            Self::Observations => {
                "UPDATE brain_observations
                 SET embedding = $1::vector, embedding_model = $2, embedding_half = NULL
                 WHERE id = $3 AND embedding_model IS NOT DISTINCT FROM $4
                   AND (embedding IS NULL) = $5 AND md5(content) = $6"
            }
            Self::Episodes => {
                "UPDATE brain_episodes
                 SET embedding = $1::vector, embedding_model = $2
                 WHERE id = $3 AND embedding_model IS NOT DISTINCT FROM $4
                   AND (embedding IS NULL) = $5 AND md5(content) = $6"
            }
            Self::Chunks => {
                "UPDATE brain_observation_chunks
                 SET embedding = $1::vector, embedding_model = $2
                 WHERE id = $3 AND embedding_model IS NOT DISTINCT FROM $4
                   AND (embedding IS NULL) = $5 AND md5(content) = $6"
            }
            Self::Procedures => {
                "UPDATE brain_procedures
                 SET embedding = $1::vector, embedding_model = $2
                 WHERE id = $3 AND embedding_model IS NOT DISTINCT FROM $4
                   AND (embedding IS NULL) = $5 AND md5(name || '. ' || trigger_context) = $6"
            }
        }
    }

    /// `$1` old vector, `$2` old label, `$3` id, `$4` the label the reembed wrote, `$5` fingerprint.
    const fn restore_sql(self) -> &'static str {
        match self {
            Self::Observations => {
                "UPDATE brain_observations
                 SET embedding = $1::vector, embedding_model = $2, embedding_half = NULL
                 WHERE id = $3 AND embedding_model = $4 AND md5(content) = $5"
            }
            Self::Episodes => {
                "UPDATE brain_episodes
                 SET embedding = $1::vector, embedding_model = $2
                 WHERE id = $3 AND embedding_model = $4 AND md5(content) = $5"
            }
            Self::Chunks => {
                "UPDATE brain_observation_chunks
                 SET embedding = $1::vector, embedding_model = $2
                 WHERE id = $3 AND embedding_model = $4 AND md5(content) = $5"
            }
            Self::Procedures => {
                "UPDATE brain_procedures
                 SET embedding = $1::vector, embedding_model = $2
                 WHERE id = $3 AND embedding_model = $4
                   AND md5(name || '. ' || trigger_context) = $5"
            }
        }
    }

    fn input_of(self, row: &PendingRow) -> EmbedInput {
        match self {
            Self::Procedures => EmbedInput::Bare {
                text: row.text.clone(),
            },
            _ => EmbedInput::Contextual {
                text: row.text.clone(),
                entity_type: row.entity_type.clone(),
                entity_name: row.entity_name.clone(),
            },
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PendingRow {
    id: Uuid,
    text: String,
    digest: String,
    label: Option<String>,
    vector: Option<pgvector::Vector>,
    entity_type: String,
    entity_name: String,
}

#[derive(Serialize, Deserialize)]
struct Header {
    format: String,
    node_id: String,
    to: String,
    binary: String,
    created_at: String,
}

/// A row as it was before the reembed touched it: its vector, its label and the fingerprint of the
/// text the vector was made of, which is what tells a rollback that the text is still the same.
#[derive(Serialize, Deserialize)]
struct SavedRow {
    table: String,
    id: Uuid,
    md5: String,
    model: Option<String>,
    vector: Vec<f32>,
}

/// The directory of the snapshot of a generation: its label with everything that is not
/// `[a-z0-9._-]` turned into `-`, so no label can name another place.
fn snapshot_dir(root: &Path, tag: &str) -> Result<PathBuf> {
    let name: String = tag
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    anyhow::ensure!(
        !name.trim_matches('.').is_empty(),
        "la etiqueta `{tag}` no sirve de nombre de directorio"
    );
    Ok(root.join(name))
}

fn part_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut parts: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading the snapshot {}", dir.display()))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("part-") && name.ends_with(".jsonl"))
        })
        .collect();
    parts.sort();
    Ok(parts)
}

fn read_part(path: &Path) -> Result<(Header, Vec<SavedRow>)> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading the part {}", path.display()))?;
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next().context("the part is empty")?)
        .with_context(|| format!("the header of {} is not JSON", path.display()))?;
    anyhow::ensure!(
        header.format == SNAPSHOT_FORMAT,
        "{} is of format {}, and this version reads {SNAPSHOT_FORMAT}",
        path.display(),
        header.format
    );
    let rows = lines
        .map(serde_json::from_str::<SavedRow>)
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("a row of {} is not JSON", path.display()))?;
    Ok((header, rows))
}

/// The label every part of a snapshot says it was made for, and all its rows.
fn read_snapshot(dir: &Path) -> Result<(String, Vec<SavedRow>)> {
    let mut target: Option<String> = None;
    let mut rows = Vec::new();
    for part in part_files(dir)? {
        let (header, mut saved) = read_part(&part)?;
        anyhow::ensure!(
            target.as_ref().is_none_or(|to| *to == header.to),
            "the parts of {} are not of one generation",
            dir.display()
        );
        target = Some(header.to);
        rows.append(&mut saved);
    }
    let target = target.with_context(|| format!("{} holds no part", dir.display()))?;
    Ok((target, rows))
}

fn sync_directory(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// A part reaches its name only when it is whole on disk: written under another name, synced,
/// renamed, the directory synced, and read back to count its rows.
fn write_part(dir: &Path, number: usize, plan: &Plan, rows: &[SavedRow]) -> Result<()> {
    let name = format!("part-{number:04}.jsonl");
    let unfinished = dir.join(format!("{name}.part"));
    let header = Header {
        format: SNAPSHOT_FORMAT.to_string(),
        node_id: plan.node_id.clone(),
        to: plan.tag.clone(),
        binary: format!("memory-industry {}", env!("CARGO_PKG_VERSION")),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let mut text = serde_json::to_string(&header)?;
    text.push('\n');
    for row in rows {
        text.push_str(&serde_json::to_string(row)?);
        text.push('\n');
    }
    let mut file = std::fs::File::create(&unfinished)
        .with_context(|| format!("creating {}", unfinished.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&unfinished, dir.join(&name))?;
    sync_directory(dir)?;
    let written = std::fs::read_to_string(dir.join(&name))?.lines().count();
    anyhow::ensure!(
        written == rows.len() + 1,
        "{name} has {written} lines on disk and {} were written",
        rows.len() + 1
    );
    Ok(())
}

/// What is already in the snapshot of a generation, and where the next part goes.
struct Snapshot {
    dir: PathBuf,
    saved: HashSet<(Table, Uuid)>,
    parts: usize,
}

impl Snapshot {
    fn open(dir: &Path) -> Result<Self> {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "part") {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing the unfinished {}", path.display()))?;
            }
        }
        let parts = part_files(dir)?;
        let mut saved = HashSet::new();
        for part in &parts {
            for row in read_part(part)?.1 {
                if let Some(table) = Table::from_name(&row.table) {
                    saved.insert((table, row.id));
                }
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            saved,
            parts: parts.len(),
        })
    }

    /// A row that an earlier run of this generation already handled: in the snapshot and already
    /// carrying the new label. Only `--all` needs to ask, because without it a row with the new
    /// label is not selected at all.
    fn is_done(&self, table: Table, row: &PendingRow, tag: &str) -> bool {
        self.saved.contains(&(table, row.id)) && row.label.as_deref() == Some(tag)
    }

    /// Writes one part with the rows that have a vector and are not in the snapshot yet. Nothing
    /// is rewritten before this returns.
    fn save(
        &mut self,
        plan: &Plan,
        table: Table,
        rows: &[PendingRow],
        report: &mut Report,
    ) -> Result<()> {
        let fresh: Vec<SavedRow> = rows
            .iter()
            .filter(|row| !self.saved.contains(&(table, row.id)))
            .filter_map(|row| {
                Some(SavedRow {
                    table: table.name().to_string(),
                    id: row.id,
                    md5: row.digest.clone(),
                    model: row.label.clone(),
                    vector: row.vector.as_ref()?.to_vec(),
                })
            })
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        write_part(&self.dir, self.parts + 1, plan, &fresh)?;
        self.parts += 1;
        for row in &fresh {
            self.saved.insert((table, row.id));
            let label = row.model.clone().unwrap_or_else(|| UNLABELLED.to_string());
            *report.old_labels.entry(label).or_default() += 1;
        }
        report.saved += fresh.len();
        Ok(())
    }
}

fn without_a_snapshot(why: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!("{NO_SNAPSHOT}: {why:#}")
}

/// The refusal of a second re-embed. A type of its own so that the automatic one can tell «another
/// holds the lock» from every other failure without reading the words of the message.
#[derive(Debug)]
struct AnotherReembed;

impl std::fmt::Display for AnotherReembed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "otro reembebido está en curso en esta base: esperá a que termine \
             (memory-industry reembed, cuba_zafra action=reembed, o el reembebido automático del servicio)",
        )
    }
}

impl std::error::Error for AnotherReembed {}

async fn hold_the_lock(pool: &PgPool) -> Result<PoolConnection<sqlx::Postgres>> {
    let mut conn = pool
        .acquire()
        .await
        .context("acquiring a connection to hold the reembed lock")?;
    let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(REEMBED_LOCK)
        .fetch_one(&mut *conn)
        .await
        .context("checking the reembed lock")?;
    if !taken {
        return Err(AnotherReembed.into());
    }
    Ok(conn)
}

async fn release_the_lock(mut conn: PoolConnection<sqlx::Postgres>) {
    if let Err(why) = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(REEMBED_LOCK)
        .execute(&mut *conn)
        .await
    {
        tracing::error!(error = %why, "could not release the reembed lock — it stays held until this connection closes");
    }
}

/// Rewrites the vectors that are not of the generation in `plan.tag`, and keeps the old ones.
///
/// Before a row is written its vector, its label and the fingerprint of its text are in a file on
/// disk (a part of the snapshot, synced and read back), and with no place to write it nothing is
/// rewritten. A row is rewritten only if it still is the one that was read: another process that
/// changed it meanwhile is left alone and counted. Stopped at any point, the next run finds the
/// snapshot, adds only what it lacks and skips what already has the new label. Two runs at once:
/// the second refuses.
pub async fn reembed(pool: &PgPool, plan: &Plan, encode: &Encoder) -> Result<Report> {
    anyhow::ensure!(
        plan.tag.contains(RUNTIME_MARK),
        "la etiqueta `{}` no nombra ningún ONNX Runtime: sin runtime cargado no hay generación que escribir",
        plan.tag
    );
    let lock = hold_the_lock(pool).await?;
    let outcome = rewrite_all(pool, plan, encode).await;
    release_the_lock(lock).await;
    outcome
}

async fn rewrite_all(pool: &PgPool, plan: &Plan, encode: &Encoder) -> Result<Report> {
    let dir = snapshot_dir(&plan.snapshot_root, &plan.tag)?;
    std::fs::create_dir_all(&dir)
        .map_err(|why| anyhow::anyhow!("{NO_SNAPSHOT}: no pude crear {}: {why}", dir.display()))?;
    let mut snapshot = Snapshot::open(&dir).map_err(without_a_snapshot)?;
    if plan.progress {
        eprintln!(
            "reembed: los vectores anteriores se guardan en {}",
            dir.display()
        );
    }
    let mut report = Report {
        snapshot: dir,
        ..Report::default()
    };
    for table in TABLES {
        rewrite_table(pool, plan, encode, &mut snapshot, table, &mut report).await?;
    }
    Ok(report)
}

async fn fetch_pending(
    pool: &PgPool,
    table: Table,
    after: Uuid,
    plan: &Plan,
    want: usize,
) -> Result<Vec<PendingRow>> {
    sqlx::query_as(table.pending_sql())
        .bind(after)
        .bind(plan.all)
        .bind(&plan.tag)
        .bind(want as i64)
        .fetch_all(pool)
        .await
        .with_context(|| {
            format!(
                "reading the rows of {} that need a new vector",
                table.name()
            )
        })
}

/// Whether a row the page returned is one this run rewrites: not already done by an earlier run of
/// `--all`, and, when the plan only moves forward, one with no vector or of an earlier generation.
fn is_to_rewrite(plan: &Plan, snapshot: &Snapshot, table: Table, row: &PendingRow) -> bool {
    let moves_forward = !plan.forward_only
        || row.vector.is_none()
        || is_older_generation(row.label.as_deref(), &plan.tag);
    moves_forward && (!plan.all || !snapshot.is_done(table, row, &plan.tag))
}

async fn rewrite_table(
    pool: &PgPool,
    plan: &Plan,
    encode: &Encoder,
    snapshot: &mut Snapshot,
    table: Table,
    report: &mut Report,
) -> Result<()> {
    let mut after = Uuid::nil();
    let mut taken = 0usize;
    loop {
        let want = plan.limit.map_or(plan.chunk, |limit| {
            plan.chunk.min(limit.saturating_sub(taken))
        });
        if want == 0 {
            return Ok(());
        }
        let page = fetch_pending(pool, table, after, plan, want).await?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = last.id;
        let exhausted = page.len() < want;
        let rows: Vec<PendingRow> = page
            .into_iter()
            .filter(|row| is_to_rewrite(plan, snapshot, table, row))
            .collect();
        taken += rows.len();
        report.table_mut(table).fetched += rows.len();
        snapshot
            .save(plan, table, &rows, report)
            .map_err(without_a_snapshot)?;
        for row in &rows {
            let outcome = rewrite_row(pool, table, plan, encode, row).await;
            report.table_mut(table).record(outcome);
        }
        if plan.progress {
            let done = report.table_mut(table).rewritten;
            eprintln!("  {}: {done} reescritas", table.name());
        }
        if exhausted {
            return Ok(());
        }
    }
}

async fn rewrite_row(
    pool: &PgPool,
    table: Table,
    plan: &Plan,
    encode: &Encoder,
    row: &PendingRow,
) -> Outcome {
    let vector = match encode(table.input_of(row)).await {
        Ok(vector) => vector,
        Err(why) => {
            tracing::warn!(row_id = %row.id, table = table.name(), error = %why, "reembed: the encoder failed for a row");
            return Outcome::Failed;
        }
    };
    let written = sqlx::query(table.rewrite_sql())
        .bind(pgvector::Vector::from(vector))
        .bind(&plan.tag)
        .bind(row.id)
        .bind(row.label.as_deref())
        .bind(row.vector.is_none())
        .bind(&row.digest)
        .execute(pool)
        .await;
    match written {
        Ok(done) if done.rows_affected() == 1 => Outcome::Rewritten,
        Ok(_) => Outcome::Changed,
        Err(why) => {
            tracing::warn!(row_id = %row.id, table = table.name(), error = %why, "reembed: could not persist the vector of a row");
            Outcome::Failed
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RollbackReport {
    pub restored: usize,
    pub skipped: usize,
    pub snapshot: PathBuf,
}

fn snapshot_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn find_snapshot(root: &Path, from: Option<&str>) -> Result<PathBuf> {
    if let Some(tag) = from {
        let dir = snapshot_dir(root, tag)?;
        anyhow::ensure!(
            dir.is_dir(),
            "no hay instantánea de {tag} en {}",
            root.display()
        );
        return Ok(dir);
    }
    let mut dirs = snapshot_dirs(root);
    match dirs.len() {
        0 => anyhow::bail!("no hay ninguna instantánea en {}", root.display()),
        1 => Ok(dirs.remove(0)),
        n => anyhow::bail!(
            "hay {n} instantáneas en {}: decí cuál con `--rollback <etiqueta>`",
            root.display()
        ),
    }
}

async fn restore_rows(pool: &PgPool, target: &str, rows: &[SavedRow]) -> Result<(usize, usize)> {
    let (mut restored, mut skipped) = (0usize, 0usize);
    for row in rows {
        let table = Table::from_name(&row.table).with_context(|| {
            format!(
                "the snapshot names the table {}, which this version does not know",
                row.table
            )
        })?;
        let done = sqlx::query(table.restore_sql())
            .bind(pgvector::Vector::from(row.vector.clone()))
            .bind(row.model.as_deref())
            .bind(row.id)
            .bind(target)
            .bind(&row.md5)
            .execute(pool)
            .await
            .with_context(|| format!("giving back a vector of {}", table.name()))?;
        if done.rows_affected() == 1 {
            restored += 1;
        } else {
            skipped += 1;
        }
    }
    Ok((restored, skipped))
}

/// Gives the old vectors back to the rows that still carry what the reembed wrote, and the same
/// text: what was written afterwards, or a row whose text changed, has no old vector that belongs
/// to it and is left as it is. `loaded_tag` is the label of the runtime loaded now, and has to be
/// another one than the generation being undone, or the database would be left of another
/// generation than the one the process writes.
pub async fn rollback(
    pool: &PgPool,
    root: &Path,
    loaded_tag: &str,
    from: Option<&str>,
) -> Result<RollbackReport> {
    anyhow::ensure!(
        loaded_tag.contains(RUNTIME_MARK),
        "sin un runtime cargado no hay etiqueta con la que comparar la de la instantánea"
    );
    let dir = find_snapshot(root, from)?;
    let (target, rows) = read_snapshot(&dir)?;
    anyhow::ensure!(
        target != loaded_tag,
        "la instantánea se hizo al reembeber hacia {target}, que es el runtime cargado ahora: \
         volvé primero al runtime anterior (el directorio onnxruntime de antes, o ORT_DYLIB_PATH) y repetí --rollback"
    );
    let lock = hold_the_lock(pool).await?;
    let outcome = restore_rows(pool, &target, &rows).await;
    release_the_lock(lock).await;
    let (restored, skipped) = outcome?;
    Ok(RollbackReport {
        restored,
        skipped,
        snapshot: dir,
    })
}

/// Deletes a snapshot, once nothing needs it: refused while rows of an earlier generation than the
/// one it was made for are left, because the reembed that would finish them is the one that keeps
/// adding to it. A row of a newer runtime or of another model does not hold it: the automatic
/// re-embed does not rewrite it, so its old vector is not in the snapshot.
pub async fn drop_snapshot(pool: &PgPool, root: &Path, tag: &str) -> Result<PathBuf> {
    let dir = find_snapshot(root, Some(tag))?;
    let (target, _) = read_snapshot(&dir)?;
    let counts = forward_counts(pool, &target).await?;
    anyhow::ensure!(
        counts.older == 0,
        "quedan {} vectores anteriores a {target}: el reembebido no terminó, y la instantánea es \
         lo único que guarda sus vectores anteriores. Terminalo con `memory-industry reembed`",
        counts.older
    );
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        anyhow::ensure!(
            name.starts_with("part-"),
            "{} guarda algo que no es una parte de la instantánea ({name}): no lo borro",
            dir.display()
        );
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(dir)
}

/// The age in days of the oldest snapshot under `root`, counted from the last part written to it.
pub fn oldest_snapshot_age_days(root: &Path) -> Option<u64> {
    let now = std::time::SystemTime::now();
    snapshot_dirs(root)
        .iter()
        .filter_map(|dir| {
            part_files(dir)
                .ok()?
                .iter()
                .filter_map(|part| part.metadata().ok()?.modified().ok())
                .max()
        })
        .filter_map(|written| now.duration_since(written).ok())
        .map(|age| age.as_secs() / 86_400)
        .max()
}

/// Where the snapshots of this database go: `MEMORY_INDUSTRY_REEMBED_DIR` if the operator (or the
/// judge) chose, and otherwise the `reembed` directory of the cache, under the id of the database
/// so that two databases of one home do not share a snapshot.
pub fn snapshot_root(node_id: &str) -> Result<PathBuf> {
    let chosen = std::env::var("MEMORY_INDUSTRY_REEMBED_DIR")
        .ok()
        .filter(|dir| !dir.trim().is_empty());
    let base = match chosen {
        Some(dir) => PathBuf::from(dir),
        None => {
            let roots = crate::models_cli::CacheRoots::resolve()?;
            let cache = if roots.preferred.exists() || !roots.legacy.exists() {
                roots.preferred
            } else {
                roots.legacy
            };
            cache.join("reembed")
        }
    };
    Ok(base.join(node_id))
}

/// What the command prints when it is done: the first line is the one the scripts read, then what
/// was left alone, where the old vectors are and how to go back.
fn report_lines(report: &Report, tag: &str, dim: usize) -> Vec<String> {
    let (obs, eps, chunks, procs) = (
        report.observations,
        report.episodes,
        report.chunks,
        report.procedures,
    );
    let mut lines = vec![format!(
        "Reembebidas {} de {} observaciones y {} de {} episodios con {tag} ({dim}-d).",
        obs.rewritten, obs.fetched, eps.rewritten, eps.fetched
    )];
    if chunks.fetched + procs.fetched > 0 {
        lines.push(format!(
            "{} de {} fragmentos y {} de {} procedimientos.",
            chunks.rewritten, chunks.fetched, procs.rewritten, procs.fetched
        ));
    }
    let total = report.total();
    if total.changed > 0 {
        lines.push(format!(
            "{} cambiaron durante el reembebido y no se pisaron: son de quien las cambió.",
            total.changed
        ));
    }
    if total.failed > 0 {
        lines.push(format!("{} fallaron (ver los warnings).", total.failed));
    }
    if report.saved > 0 {
        let labels: Vec<String> = report
            .old_labels
            .iter()
            .map(|(label, rows)| format!("{label} ({rows})"))
            .collect();
        lines.push(format!(
            "Se guardaron {} vectores anteriores, de {}.",
            report.saved,
            labels.join(", ")
        ));
    }
    lines.push(format!("Instantánea: {}", report.snapshot.display()));
    lines.push("Vuelta atrás: memory-industry reembed --rollback".to_string());
    lines
}

/// What the command asks for beyond re-embedding.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    batch: i64,
    all: bool,
    rollback: Option<Option<String>>,
    drop_snapshot: Option<String>,
}

/// `None` when the caller asked for the help.
fn parse_args(args: &[String]) -> Result<Option<Args>> {
    let mut parsed = Args {
        batch: 500,
        ..Args::default()
    };
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--all" => parsed.all = true,
            "--batch" => {
                let n = it.next().context("--batch needs a number: --batch 500")?;
                parsed.batch = n.parse().context("--batch needs an integer")?;
            }
            "--rollback" => {
                parsed.rollback = Some(it.next_if(|next| !next.starts_with("--")).cloned());
            }
            "--drop-snapshot" => {
                let tag = it
                    .next()
                    .context("--drop-snapshot needs the label of the snapshot")?;
                parsed.drop_snapshot = Some(tag.clone());
            }
            other => {
                if let Some(n) = other.strip_prefix("--batch=") {
                    parsed.batch = n.parse().context("--batch needs an integer")?;
                } else {
                    anyhow::bail!("reembed: argumento desconocido `{other}` (probá --help)");
                }
            }
        }
    }
    Ok(Some(parsed))
}

const USAGE: &str = "usage: memory-industry reembed [--all] [--batch N] [--rollback [LABEL]] [--drop-snapshot LABEL]\n\n\
     Re-encodes the vectors that need it: missing, or labelled with another model or another ONNX\n\
     Runtime than the one loaded now (observations, episodes, chunks and procedures). Run it after\n\
     changing the runtime (memory-industry models runtime), CUBA_EMBED_MODEL / CUBA_EMBEDDING_DIM /\n\
     ONNX_MODEL_PATH — and after scripts/migrate-embedding-dim.sh has retyped the column.\n\n\
     Before it writes a row it keeps the old vector in a snapshot on disk (MEMORY_INDUSTRY_REEMBED_DIR,\n\
     or the reembed directory of the cache); with nowhere to keep it, it writes nothing. A run that\n\
     stops is repeated with the same command. Two at once: the second refuses.\n\n\
     --all                 re-encode every row, current or not\n\
     --batch N             rows per part of the snapshot (default 500)\n\
     --rollback [LABEL]    give the old vectors back where the text did not change (with the previous\n\
     \x20                     runtime loaded: restore its directory first)\n\
     --drop-snapshot LABEL delete the snapshot of that generation, once nothing is left to rewrite\n\n\
     Refuses to run without a real ONNX model: re-embedding with the hash\n\
     fallback would overwrite meaningful vectors with noise.";

pub async fn run_cli(args: &[String]) -> Result<()> {
    let Some(args) = parse_args(args)? else {
        eprintln!("{USAGE}");
        return Ok(());
    };

    if !crate::embeddings::onnx::model_loaded().await {
        anyhow::bail!(
            "no hay modelo ONNX cargado. Reembeber con el fallback de hash reemplazaría \
             los vectores por ruido sin que nada lo avise.\n\
             Comprobá ONNX_MODEL_PATH y ORT_DYLIB_PATH (memory-industry doctor lo diagnostica)."
        );
    }

    let url = crate::setup::resolve_database_url().await;
    let pool = crate::db::create_pool(&url)
        .await
        .context("connecting to database for reembed")?;

    let tag = crate::embeddings::onnx::vector_tag();
    let node_id = crate::db::node_id(&pool).await?.to_string();
    let root = snapshot_root(&node_id)?;

    if let Some(from) = args.rollback {
        let report = rollback(&pool, &root, &tag, from.as_deref()).await?;
        println!(
            "Devueltos {} vectores anteriores; {} no, porque su texto o su etiqueta cambió después.",
            report.restored, report.skipped
        );
        println!("Instantánea: {}", report.snapshot.display());
        return Ok(());
    }
    if let Some(label) = args.drop_snapshot {
        let dir = drop_snapshot(&pool, &root, &label).await?;
        println!("Instantánea borrada: {}", dir.display());
        return Ok(());
    }

    let dim = crate::embeddings::onnx::embedding_dim();
    eprintln!("reembed: modelo={tag} dim={dim}");
    check_column_width(&pool, dim).await?;

    let plan = Plan {
        tag: tag.clone(),
        all: args.all,
        chunk: args.batch.clamp(1, 5000) as usize,
        limit: None,
        node_id,
        snapshot_root: root,
        progress: true,
        forward_only: false,
    };
    let report = reembed(&pool, &plan, &onnx_encoder).await?;
    if report.total().fetched == 0 {
        println!("Nada que reembeber: todo el corpus ya está en {tag} ({dim}-d).");
        return Ok(());
    }
    for line in report_lines(&report, &tag, dim) {
        println!("{line}");
    }
    let left = crate::embeddings::backfill::vector_coverage(&pool, &tag).await?;
    println!("Pendientes: {}", left.stale() + left.missing());
    if args.all || report.total().fetched > 100 {
        println!(
            "\nEl umbral de abstención estaba calibrado para el modelo anterior y ya no vale:"
        );
        println!("  memory-industry calibrate --dataset <dataset.jsonl> --apply");
    }
    Ok(())
}

async fn check_column_width(pool: &PgPool, dim: usize) -> Result<()> {
    let col: String = sqlx::query_scalar(
        "SELECT format_type(atttypid, atttypmod) FROM pg_attribute
         WHERE attrelid = 'brain_observations'::regclass AND attname = 'embedding'",
    )
    .fetch_one(pool)
    .await
    .context("reading the embedding column type")?;
    let expected = format!("vector({dim})");
    if col != expected {
        anyhow::bail!(
            "la columna es {col} y el modelo produce {expected}.\n\
             Corré primero:  DATABASE_URL=… scripts/migrate-embedding-dim.sh {dim}"
        );
    }
    Ok(())
}

/// How the rows that need a vector divide for the automatic re-embed: `older` are the ones it
/// rewrites (no vector, or of an earlier generation than the label asked about) and `ahead` the
/// ones it leaves alone (of a newer runtime, or of another model).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ForwardCounts {
    pub older: usize,
    pub ahead: usize,
}

impl Table {
    /// Whether it has a vector, its label and how many, for the rows that differ from the label
    /// `$1` or have no vector: the same rows `pending_sql` reads, counted by what the Rust rule
    /// then sorts into older and ahead.
    const fn forward_sql(self) -> &'static str {
        match self {
            Self::Observations => {
                "SELECT embedding IS NOT NULL, embedding_model, count(*)::bigint
                 FROM brain_observations
                 WHERE embedding IS NULL OR embedding_model IS DISTINCT FROM $1
                 GROUP BY 1, 2"
            }
            Self::Episodes => {
                "SELECT embedding IS NOT NULL, embedding_model, count(*)::bigint
                 FROM brain_episodes
                 WHERE embedding IS NULL OR embedding_model IS DISTINCT FROM $1
                 GROUP BY 1, 2"
            }
            Self::Chunks => {
                "SELECT embedding IS NOT NULL, embedding_model, count(*)::bigint
                 FROM brain_observation_chunks
                 WHERE embedding IS NULL OR embedding_model IS DISTINCT FROM $1
                 GROUP BY 1, 2"
            }
            Self::Procedures => {
                "SELECT embedding IS NOT NULL, embedding_model, count(*)::bigint
                 FROM brain_procedures
                 WHERE embedding IS NULL OR embedding_model IS DISTINCT FROM $1
                 GROUP BY 1, 2"
            }
        }
    }
}

/// The rows of the four tables that differ from `tag`, split into the ones the automatic re-embed
/// rewrites and the ones it does not.
pub async fn forward_counts(pool: &PgPool, tag: &str) -> Result<ForwardCounts> {
    let mut counts = ForwardCounts::default();
    for table in TABLES {
        let rows: Vec<(bool, Option<String>, i64)> = sqlx::query_as(table.forward_sql())
            .bind(tag)
            .fetch_all(pool)
            .await
            .with_context(|| format!("counting the vectors of {} by generation", table.name()))?;
        for (has_vector, label, count) in rows {
            let count = usize::try_from(count).unwrap_or_default();
            if !has_vector || is_older_generation(label.as_deref(), tag) {
                counts.older += count;
            } else {
                counts.ahead += count;
            }
        }
    }
    Ok(counts)
}

/// `false` only for `0`, `off`, `false` or `no`, in any case and with spaces around: the
/// automatic re-embed is on unless the operator turned it off.
pub fn auto_reembed_enabled_from(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|raw| raw.trim().to_lowercase()).as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

fn auto_reembed_enabled() -> bool {
    auto_reembed_enabled_from(
        std::env::var("MEMORY_INDUSTRY_AUTO_REEMBED")
            .ok()
            .as_deref(),
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AutoState {
    Off,
    #[default]
    Idle,
    Running,
    Done,
    Busy,
    Failed,
}

impl AutoState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Done => "done",
            Self::Busy => "busy",
            Self::Failed => "failed",
        }
    }
}

/// Where the automatic re-embed of this process is, for `/health`. `older` and `ahead` are what
/// it counted when it started, `encoded` rows sent to the encoder (it grows while it runs),
/// `rewritten`, `changed` and `failed` what `reembed` reports when it ends.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AutoProgress {
    pub state: AutoState,
    pub older: usize,
    pub ahead: usize,
    pub encoded: usize,
    pub rewritten: usize,
    pub changed: usize,
    pub failed: usize,
    pub last_error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub snapshot: Option<PathBuf>,
}

static AUTO: std::sync::Mutex<AutoProgress> = std::sync::Mutex::new(AutoProgress {
    state: AutoState::Idle,
    older: 0,
    ahead: 0,
    encoded: 0,
    rewritten: 0,
    changed: 0,
    failed: 0,
    last_error: None,
    started_at: None,
    finished_at: None,
    snapshot: None,
});

pub fn set_auto_progress(progress: AutoProgress) {
    if let Ok(mut slot) = AUTO.lock() {
        *slot = progress;
    }
}

pub fn auto_progress() -> AutoProgress {
    AUTO.lock().map(|slot| (*slot).clone()).unwrap_or_default()
}

/// What the automatic re-embed needs besides the pool: where the snapshots go, the id of this
/// database, the label the new vectors get and how many rows go into one part of the snapshot.
#[derive(Debug, Clone)]
pub struct AutoSettings {
    pub snapshot_root: PathBuf,
    pub node_id: String,
    pub tag: String,
    pub chunk: usize,
}

/// `onnx_encoder` for the re-embed that nobody waits for: it gives way to the searches, leaves the
/// cache of queries alone and counts each row it is asked for in the progress `/health` shows.
pub fn background_encoder(input: EmbedInput) -> BoxFuture<'static, Result<Vec<f32>>> {
    Box::pin(async move {
        if let Ok(mut slot) = AUTO.lock() {
            slot.encoded += 1;
        }
        match input {
            EmbedInput::Contextual {
                text,
                entity_type,
                entity_name,
            } => {
                crate::embeddings::onnx::embed_passage_contextual_background(
                    &text,
                    &entity_type,
                    &entity_name,
                )
                .await
            }
            EmbedInput::Bare { text } => {
                crate::embeddings::onnx::embed_passage_background(&text).await
            }
        }
    })
}

/// Publishes how the run ended. `encoded` is read back from the published progress because the
/// encoder, not this function, is what counts it.
fn finish(
    mut progress: AutoProgress,
    state: AutoState,
    last_error: Option<String>,
) -> AutoProgress {
    progress.encoded = auto_progress().encoded;
    progress.state = state;
    progress.last_error = last_error;
    progress.finished_at = Some(chrono::Utc::now().to_rfc3339());
    set_auto_progress(progress.clone());
    progress
}

/// The figure the operator needs when there is no room for the snapshot: a part is a JSON line of
/// about `dim · 11 + 200` bytes per vector.
fn room_for(rows: usize, root: &Path) -> String {
    let bytes = rows as f64 * (crate::embeddings::onnx::embedding_dim() as f64 * 11.0 + 200.0);
    format!(
        "hacen falta ≈ {:.1} MB para {rows} vectores en {}",
        (bytes / 1e6).max(0.1),
        root.display()
    )
}

/// The state a failed run leaves and what it says: another re-embed holding the lock is `busy` and
/// the rest is `failed`, and a snapshot that could not be written says how much room it needed.
fn why_it_stopped(
    why: &anyhow::Error,
    settings: &AutoSettings,
    older: usize,
) -> (AutoState, String) {
    let said = format!("{why:#}");
    if why.downcast_ref::<AnotherReembed>().is_some() {
        return (AutoState::Busy, said);
    }
    if said.starts_with(NO_SNAPSHOT) {
        let room = room_for(older, &settings.snapshot_root);
        return (AutoState::Failed, format!("{said}; {room}"));
    }
    (AutoState::Failed, said)
}

/// One run of the re-embed the service does by itself, and where it ended, which is also what
/// `auto_progress()` says afterwards. It counts what is older and what is ahead; with nothing
/// older it is `idle`. Otherwise it is `reembed` with `forward_only`: with the lock taken by
/// another it is `busy` and touches nothing, with no place for the snapshot it is `failed` before
/// the first row is written, and a row the encoder could not do is counted and left for the next
/// start. It never tries again: the next start is the retry, and the command is always there.
pub async fn run_automatic_reembed(
    pool: &PgPool,
    settings: &AutoSettings,
    encode: &Encoder,
) -> AutoProgress {
    let mut progress = AutoProgress {
        state: AutoState::Running,
        started_at: Some(chrono::Utc::now().to_rfc3339()),
        ..AutoProgress::default()
    };
    set_auto_progress(progress.clone());
    match forward_counts(pool, &settings.tag).await {
        Ok(counts) => {
            progress.older = counts.older;
            progress.ahead = counts.ahead;
        }
        Err(why) => return finish(progress, AutoState::Failed, Some(format!("{why:#}"))),
    }
    if progress.older == 0 {
        return finish(progress, AutoState::Idle, None);
    }
    set_auto_progress(progress.clone());
    let plan = Plan {
        tag: settings.tag.clone(),
        all: false,
        chunk: settings.chunk,
        limit: None,
        node_id: settings.node_id.clone(),
        snapshot_root: settings.snapshot_root.clone(),
        progress: false,
        forward_only: true,
    };
    match reembed(pool, &plan, encode).await {
        Ok(report) => {
            let total = report.total();
            progress.rewritten = total.rewritten;
            progress.changed = total.changed;
            progress.failed = total.failed;
            progress.snapshot = Some(report.snapshot);
            finish(progress, AutoState::Done, None)
        }
        Err(why) => {
            let (state, said) = why_it_stopped(&why, settings, progress.older);
            finish(progress, state, Some(said))
        }
    }
}

async fn auto_settings(pool: &PgPool, tag: String) -> Result<AutoSettings> {
    let node_id = crate::db::node_id(pool)
        .await
        .context("reading the id of this database")?
        .to_string();
    let snapshot_root = snapshot_root(&node_id)?;
    Ok(AutoSettings {
        snapshot_root,
        node_id,
        tag,
        chunk: AUTO_CHUNK,
    })
}

/// What the daemon does once it listens: counts the vectors of another generation and says so
/// and, unless `MEMORY_INDUSTRY_AUTO_REEMBED` turned it off, re-embeds the ones of an earlier
/// generation in the background, through a pool of its own so that the connections of the daemon
/// stay for its requests. Once per start. It does nothing without a runtime loaded (there is no
/// generation to write), and nothing that happens in it stops the daemon: an error or a panic is
/// left in the progress `/health` shows.
pub async fn start_the_automatic_reembed(pool: PgPool) {
    let enabled = auto_reembed_enabled();
    if !enabled {
        set_auto_progress(AutoProgress {
            state: AutoState::Off,
            ..AutoProgress::default()
        });
    }
    let coverage = match crate::embeddings::backfill::refresh_coverage(&pool).await {
        Ok(Some(coverage)) => coverage,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "could not count the vectors of another generation");
            return;
        }
    };
    if coverage.stale() > 0 {
        tracing::warn!(
            stale = coverage.stale(),
            missing = coverage.missing(),
            current = %coverage.tag,
            automatic = enabled,
            "hay vectores de otra generación: el servicio reembebe solo los anteriores al runtime cargado \
             (MEMORY_INDUSTRY_AUTO_REEMBED=0 lo apaga); `memory-industry reembed` los rehace a mano \
             (reanudable; guarda antes los actuales; `--rollback` los devuelve)"
        );
    }
    if !enabled || !coverage.tag.contains(RUNTIME_MARK) {
        return;
    }
    let settings = match auto_settings(&pool, coverage.tag).await {
        Ok(settings) => settings,
        Err(e) => {
            finish(auto_progress(), AutoState::Failed, Some(format!("{e:#}")));
            return;
        }
    };
    let side = crate::db::side_pool(&pool, 2);
    let task = tokio::spawn({
        let (side, settings) = (side.clone(), settings.clone());
        async move { run_automatic_reembed(&side, &settings, &background_encoder).await }
    });
    let progress = match task.await {
        Ok(progress) => progress,
        Err(e) => finish(
            auto_progress(),
            AutoState::Failed,
            Some(format!("la tarea de fondo terminó sin avisar: {e}")),
        ),
    };
    side.close().await;
    if let Err(e) = crate::embeddings::backfill::refresh_coverage(&pool).await {
        tracing::warn!(error = %format!("{e:#}"), "could not count the vectors after the automatic re-embed");
    }
    match progress.state {
        AutoState::Busy | AutoState::Failed => tracing::warn!(
            state = progress.state.as_str(),
            error = progress.last_error.as_deref().unwrap_or_default(),
            "el reembebido automático se detuvo: `memory-industry reembed` lo termina a mano"
        ),
        AutoState::Done => tracing::info!(
            rewritten = progress.rewritten,
            changed = progress.changed,
            failed = progress.failed,
            ahead = progress.ahead,
            snapshot = ?progress.snapshot,
            "reembebido automático terminado"
        ),
        AutoState::Off | AutoState::Idle | AutoState::Running => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::onnx::is_older_generation;
    use crate::handlers::sync::incoming_vector_wins;
    use std::collections::HashMap;

    const MODEL: &str = "multilingual-e5-small";
    const RUNTIME_1_31: &str = "multilingual-e5-small@onnxruntime-1.31.1";
    const RUNTIME_1_26: &str = "multilingual-e5-small@onnxruntime-1.26.0";

    /// One machine of the simulation: the runtime it has loaded and the label of the vector of
    /// each of its rows. `None` is a vector from before the labels. Every row has a vector: what is
    /// being counted is how many times a vector is rewritten, and a row without one is rewritten
    /// once by whoever finds it.
    struct Machine {
        tag: &'static str,
        rows: HashMap<&'static str, Option<String>>,
    }

    impl Machine {
        fn new(tag: &'static str, rows: &[(&'static str, Option<&str>)]) -> Self {
            Self {
                tag,
                rows: rows
                    .iter()
                    .map(|(id, label)| (*id, label.map(str::to_string)))
                    .collect(),
            }
        }

        /// What the service does when it starts: the rows that the production rule calls older are
        /// rewritten with the label of the runtime loaded. It returns how many.
        fn starts(&mut self) -> usize {
            let tag = self.tag;
            let mut rewritten = 0;
            for label in self.rows.values_mut() {
                if is_older_generation(label.as_deref(), tag) {
                    *label = Some(tag.to_string());
                    rewritten += 1;
                }
            }
            rewritten
        }

        /// What a sync import does with the vectors of a package that has every row of the other
        /// machine: a row that was not here arrives with its vector and its label, and one that was
        /// takes the incoming vector only if the production rule says it wins.
        fn imports_from(&mut self, other: &Machine) {
            for (id, incoming) in &other.rows {
                match self.rows.get_mut(id) {
                    None => {
                        self.rows.insert(*id, incoming.clone());
                    }
                    Some(local) => {
                        if incoming_vector_wins(local.as_deref(), true, incoming.as_deref()) {
                            *local = incoming.clone();
                        }
                    }
                }
            }
        }
    }

    /// CI-D5c (C.4). Two machines that sync, one on ONNX Runtime 1.31.1 and one on 1.26.0, must
    /// not re-embed each other for ever. `cuba_sync` exports every observation of an entity and the
    /// import rewrites the vector of every row of the package, so without a rule the second import
    /// of A from B hands back to 1.26.0 what A had just re-embedded, and the next start of A does it
    /// all again. With the two halves of the rule (the service only rewrites what is older than its
    /// runtime, the import only takes a vector that is not older than the one it would replace) A
    /// rewrites each row once, B never rewrites a row that came from A, and both end on 1.31.1.
    /// The numbers were worked out on paper, round by round: A rewrites r1, r2 and the new row of B
    /// (3), then nothing (0); in each of the next two rounds the one new row (1), then nothing (0);
    /// B has nothing older than 1.26.0 at any start. A start is made after each import.
    #[test]
    fn two_machines_on_different_runtimes_stop_rewriting_each_other_after_the_first_round() {
        let mut a = Machine::new(RUNTIME_1_31, &[("r1", Some(MODEL))]);
        let mut b = Machine::new(
            RUNTIME_1_26,
            &[("r1", Some(MODEL)), ("r2", Some(RUNTIME_1_26))],
        );
        let (mut a_starts, mut b_starts) = (Vec::new(), Vec::new());
        for new_row in ["n1", "n2", "n3"] {
            b.rows.insert(new_row, Some(RUNTIME_1_26.to_string()));
            a.imports_from(&b);
            a_starts.push(a.starts());
            a.imports_from(&b);
            a_starts.push(a.starts());
            b.imports_from(&a);
            b_starts.push(b.starts());
        }
        assert_eq!(
            a_starts,
            [3, 0, 1, 0, 1, 0],
            "rows A re-embedded at each of its six starts"
        );
        assert_eq!(
            b_starts,
            [0, 0, 0],
            "B never rewrites what came from A: it is of a newer runtime than B has"
        );
        for (name, machine) in [("A", &a), ("B", &b)] {
            assert_eq!(machine.rows.len(), 5, "{name} has the five rows");
            for (id, label) in &machine.rows {
                assert_eq!(
                    label.as_deref(),
                    Some(RUNTIME_1_31),
                    "{name}: {id} ends on the newest runtime"
                );
            }
        }
    }

    /// CI-D5c (C.5). The service re-embeds by itself unless the operator says no: only `0`, `off`,
    /// `false` and `no` turn it off, in any case and with spaces around, and nothing else does
    /// (an unset variable, `1`, `on`, `yes` or a typo leave it on).
    #[test]
    fn the_automatic_reembed_is_on_unless_the_variable_turns_it_off() {
        for on in [None, Some("1"), Some("on"), Some("yes"), Some("whatever")] {
            assert!(auto_reembed_enabled_from(on), "{on:?} leaves it on");
        }
        for off in [
            Some("0"),
            Some("off"),
            Some("OFF"),
            Some(" false "),
            Some("no"),
        ] {
            assert!(!auto_reembed_enabled_from(off), "{off:?} turns it off");
        }
    }
}
