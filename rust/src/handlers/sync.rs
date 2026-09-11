use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::sync::chunk::{
    ArtifactRow, Counts, EntityFile, EpisodeFile, ErrorFile, FactRow, MAX_EMBEDDING_DIM, Manifest,
    ObservationRow, ProcedureRow, ProjectRow, RelationRow, SCHEMA_VERSION, SourceTrustRow,
    payload_hash, payload_hash_bytes,
};
use crate::sync::paths::{ensure_within, resolve_dir, slug};

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let dir_arg = args.get("dir").and_then(|v| v.as_str());
    let scope = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("project");
    let with_embeddings = args
        .get("with_embeddings")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let conflict = args
        .get("conflict")
        .and_then(|v| v.as_str())
        .unwrap_or("merge");

    match action {
        "export" => export(pool, dir_arg, scope, with_embeddings).await,
        "import" => {
            let confirm = args
                .get("confirm")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            {
                let deletes_allowed = args
                    .get("deletes")
                    .and_then(Value::as_str)
                    .map(|d| d != "withhold")
                    .unwrap_or(true);
                import(pool, dir_arg, conflict, confirm, deletes_allowed).await
            }
        }
        "diff" => diff(pool, dir_arg).await,
        "status" => status(pool, dir_arg).await,
        "pull" => {
            let offset = args
                .get("offset")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(usize::MAX as u64) as usize;
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n.max(1) as usize);
            let vectors = args
                .get("with_embeddings")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            pull(pool, offset, limit, vectors).await
        }
        "notify" => notify(pool, &args).await,
        "conflicts" => conflicts(pool).await,
        "resolve" => resolve_conflict(pool, &args).await,
        "fetch" => {
            let confirm = args
                .get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            fetch(pool, &args, conflict, confirm).await
        }
        _ => anyhow::bail!(
            "Invalid action: {action}. Use \
             export/import/diff/status/pull/notify/fetch/conflicts/resolve"
        ),
    }
}

pub const SYNC_LOCK: i64 = 0x0CBA_A0D1_7106_0002;

#[derive(Clone, Copy)]
enum PruneScope {
    Everything,
    Project(Uuid),
}

impl PruneScope {
    fn may_delete(self, path: &Path) -> bool {
        match self {
            PruneScope::Everything => true,
            PruneScope::Project(exported) => declared_project_id(path) == Some(exported),
        }
    }
}

fn declared_project_id(path: &Path) -> Option<Uuid> {
    let bytes = std::fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("project_id")?
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
}

fn prune_stale_files(dir: &Path, keep: &HashSet<PathBuf>, scope: PruneScope) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        if !keep.contains(&path) && scope.may_delete(&path) {
            std::fs::remove_file(&path).with_context(|| format!("prune stale file {path:?}"))?;
        }
    }
    Ok(())
}

fn prune_stale_episode_files(dir: &Path, keep: &HashSet<PathBuf>, scope: PruneScope) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for month_entry in std::fs::read_dir(dir)? {
        let month_path = month_entry?.path();
        if !month_path.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&month_path)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            if !keep.contains(&path) && scope.may_delete(&path) {
                std::fs::remove_file(&path)
                    .with_context(|| format!("prune stale episode file {path:?}"))?;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct BundleDigest {
    entries: Vec<(String, String)>,
}

impl BundleDigest {
    fn record(&mut self, root: &Path, path: &Path, bytes: &[u8]) {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        self.entries.push((relative, payload_hash_bytes(bytes)));
    }

    fn finish(mut self, project_id: Option<Uuid>) -> String {
        self.entries.sort();
        let mut acc = format!(
            "{SCHEMA_VERSION}\n{}\n",
            project_id.map(|p| p.to_string()).unwrap_or_default()
        );
        for (relative, hash) in &self.entries {
            acc.push_str(relative);
            acc.push(' ');
            acc.push_str(hash);
            acc.push('\n');
        }
        payload_hash(&acc)
    }
}

fn write_bundle_file(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    digest: &mut BundleDigest,
) -> Result<()> {
    ensure_within(root, path)?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("write bundle file {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("publish bundle file {path:?}"))?;
    digest.record(root, path, bytes);
    Ok(())
}

fn embedding_record_size(dim: usize) -> Option<usize> {
    if dim == 0 || dim > MAX_EMBEDDING_DIM {
        return None;
    }
    dim.checked_mul(4)?.checked_add(16)
}

fn resolve(remapped: &HashMap<Uuid, Uuid>, id: Uuid) -> Uuid {
    remapped.get(&id).copied().unwrap_or(id)
}

fn trust_for_imported(
    content: &str,
    declared: Option<&str>,
) -> (&'static str, Option<&'static str>) {
    if let Some(pattern) = crate::redact::looks_like_secret(content) {
        return (crate::core::trust::QUARANTINED, Some(pattern));
    }
    if declared == Some(crate::core::trust::QUARANTINED) {
        return (crate::core::trust::QUARANTINED, None);
    }
    (crate::core::trust::TRUSTED, None)
}

#[derive(sqlx::FromRow)]
struct ExportedFact {
    fact_id: Uuid,
    subject: String,
    predicate: String,
    object: String,
    valid_from: chrono::DateTime<Utc>,
    observed_at: chrono::DateTime<Utc>,
    valid_to: Option<chrono::DateTime<Utc>>,
    subject_entity_id: Option<Uuid>,
    project_id: Option<Uuid>,
    confidence: Option<f64>,
    is_current: bool,
    created_at: chrono::DateTime<Utc>,
    layer_name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ExportedObservation {
    id: Uuid,
    content: String,
    observation_type: String,
    source: String,
    importance: f64,
    tags: Vec<String>,
    project_id: Option<Uuid>,
    session_id: Option<Uuid>,
    created_at: chrono::DateTime<Utc>,
    embedding_model: Option<String>,
    updated_at: chrono::DateTime<Utc>,
    version: i32,
    previous_versions: Value,
    origin_node: Option<String>,
    evidence: String,
    verification: Option<String>,
    verified_at: Option<chrono::DateTime<Utc>>,
    trust: String,
}

async fn export(
    pool: &PgPool,
    dir_arg: Option<&str>,
    scope: &str,
    with_embeddings: bool,
) -> Result<Value> {
    let root = resolve_dir(dir_arg)?;
    export_into(pool, &root, scope, with_embeddings).await
}

async fn export_into(
    pool: &PgPool,
    root: &Path,
    scope: &str,
    with_embeddings: bool,
) -> Result<Value> {
    let root = root.to_path_buf();
    let mut lock = pool.begin().await?;
    take_sync_lock(&mut lock).await?;

    let project_id = if scope == "all" {
        None
    } else {
        crate::project::current_project_id(pool).await?
    };
    let project_name: Option<String> = match project_id {
        Some(pid) => {
            sqlx::query_scalar("SELECT name FROM brain_projects WHERE id = $1")
                .bind(pid)
                .fetch_optional(pool)
                .await?
        }
        None => None,
    };

    let projects: Vec<ProjectRow> = sqlx::query_as::<_, (Uuid, String, chrono::DateTime<Utc>)>(
        "SELECT id, name, created_at FROM brain_projects",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(id, name, created_at)| ProjectRow {
        id,
        name,
        created_at,
    })
    .collect();

    type EntityCols = (
        Uuid,
        String,
        String,
        f64,
        i32,
        Option<Uuid>,
        chrono::DateTime<Utc>,
    );
    let entity_rows: Vec<EntityCols> = sqlx::query_as(
        "SELECT id, name, entity_type, importance::float8, access_count, project_id, created_at
         FROM brain_entities
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY name",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let prune_scope = match project_id {
        Some(pid) => PruneScope::Project(pid),
        None => PruneScope::Everything,
    };
    let mut digest = BundleDigest::default();

    let entities_dir = root.join("entities");
    std::fs::create_dir_all(&entities_dir).context("mkdir entities/")?;

    let mut entity_files = 0u32;
    let mut obs_count = 0u32;
    let mut emb_blob: Vec<u8> = Vec::new();
    let mut emb_dim: Option<usize> = None;
    let mut entity_paths: HashSet<PathBuf> = HashSet::new();

    for (id, name, entity_type, importance, access_count, p_id, created_at) in entity_rows {
        let observations: Vec<ObservationRow> = sqlx::query_as::<_, ExportedObservation>(
            "SELECT id, content, observation_type, source, importance::float8 AS importance, tags,
                    project_id, session_id, created_at, embedding_model,
                    updated_at, version, previous_versions, origin_node,
                    evidence, verification, verified_at, trust
             FROM brain_observations
             WHERE entity_id = $1
             ORDER BY created_at",
        )
        .bind(id)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|r| ObservationRow {
            id: r.id,
            content: r.content,
            observation_type: r.observation_type,
            source: r.source,
            importance: r.importance,
            tags: r.tags,
            project_id: r.project_id,
            session_id: r.session_id,
            created_at: r.created_at,
            embedding_model: r.embedding_model,
            updated_at: Some(r.updated_at),
            version: Some(r.version),
            previous_versions: Some(r.previous_versions),
            origin_node: r.origin_node,
            evidence: Some(r.evidence),
            verification: r.verification,
            verified_at: r.verified_at,
            trust: Some(r.trust),
            crdt_actor: None,
            crdt_counter: None,
        })
        .collect();
        obs_count += observations.len() as u32;

        if with_embeddings {
            for obs in &observations {
                let emb: Option<pgvector::Vector> =
                    sqlx::query_scalar("SELECT embedding FROM brain_observations WHERE id = $1")
                        .bind(obs.id)
                        .fetch_optional(pool)
                        .await
                        .ok()
                        .flatten();
                if let Some(v) = emb {
                    let floats: Vec<f32> = v.to_vec();
                    if emb_dim.is_none() {
                        emb_dim = Some(floats.len());
                    }
                    emb_blob.extend_from_slice(obs.id.as_bytes());
                    for f in floats {
                        emb_blob.extend_from_slice(&f.to_le_bytes());
                    }
                }
            }
        }

        let file = EntityFile {
            id,
            name: name.clone(),
            entity_type,
            importance,
            access_count,
            project_id: p_id,
            created_at,
            observations,
        };
        let basename = format!("{}-{}.json", slug(&name), &id.to_string()[..8]);
        let path = entities_dir.join(&basename);
        write_bundle_file(
            &root,
            &path,
            &serde_json::to_vec_pretty(&file)?,
            &mut digest,
        )?;
        entity_paths.insert(path);
        entity_files += 1;
    }
    prune_stale_files(&entities_dir, &entity_paths, prune_scope)?;

    type EpCols = (
        Uuid,
        Uuid,
        String,
        Vec<String>,
        Vec<String>,
        f64,
        Option<Uuid>,
        chrono::DateTime<Utc>,
        Option<chrono::DateTime<Utc>>,
    );
    let episode_rows: Vec<EpCols> = sqlx::query_as(
        "SELECT id, entity_id, content, actors, artifacts, importance::float8,
                project_id, started_at, ended_at
         FROM brain_episodes
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY started_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let mut episode_count = 0u32;
    let mut episode_paths: HashSet<PathBuf> = HashSet::new();
    for ep in episode_rows {
        let yyyymm = ep.7.format("%Y-%m").to_string();
        let dir = root.join("episodes").join(&yyyymm);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", ep.0));
        let f = EpisodeFile {
            id: ep.0,
            entity_id: ep.1,
            content: ep.2,
            actors: ep.3,
            artifacts: ep.4,
            importance: ep.5,
            project_id: ep.6,
            started_at: ep.7,
            ended_at: ep.8,
        };
        write_bundle_file(&root, &path, &serde_json::to_vec_pretty(&f)?, &mut digest)?;
        episode_paths.insert(path);
        episode_count += 1;
    }
    prune_stale_episode_files(&root.join("episodes"), &episode_paths, prune_scope)?;

    type ErrCols = (
        Uuid,
        String,
        String,
        Option<String>,
        bool,
        String,
        Option<Uuid>,
        chrono::DateTime<Utc>,
    );
    let error_rows: Vec<ErrCols> = sqlx::query_as(
        "SELECT id, error_type, error_message, solution, resolved, project, project_id, created_at
         FROM brain_errors
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY created_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let errors_dir = root.join("errors");
    std::fs::create_dir_all(&errors_dir)?;
    let mut err_count = 0u32;
    let mut error_paths: HashSet<PathBuf> = HashSet::new();
    for e in error_rows {
        let path = errors_dir.join(format!("{}.json", e.0));
        let f = ErrorFile {
            id: e.0,
            error_type: e.1,
            error_message: e.2,
            solution: e.3,
            resolved: e.4,
            project: e.5,
            project_id: e.6,
            created_at: e.7,
        };
        write_bundle_file(&root, &path, &serde_json::to_vec_pretty(&f)?, &mut digest)?;
        error_paths.insert(path);
        err_count += 1;
    }
    prune_stale_files(&errors_dir, &error_paths, prune_scope)?;

    let decisions: Vec<(Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, content, project_id FROM brain_observations
         WHERE observation_type = 'decision'
           AND ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY created_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let relation_rows: Vec<RelationRow> = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            Uuid,
            String,
            f64,
            bool,
            Option<Uuid>,
            chrono::DateTime<Utc>,
            String,
        ),
    >(
        "SELECT id, from_entity, to_entity, relation_type, strength::float8,
                bidirectional, project_id, created_at, provenance
         FROM brain_relations
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY created_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|t| RelationRow {
        id: t.0,
        from_entity: t.1,
        to_entity: t.2,
        relation_type: t.3,
        strength: t.4,
        bidirectional: t.5,
        project_id: t.6,
        created_at: t.7,
        provenance: t.8,
    })
    .collect();

    let fact_rows: Vec<FactRow> = sqlx::query_as::<_, ExportedFact>(
        "SELECT f.fact_id, f.subject, f.predicate, f.object, f.valid_from, f.observed_at,
                f.valid_to, f.subject_entity_id, f.project_id, f.confidence::float8 AS confidence,
                f.is_current, f.created_at, l.layer_name::text AS layer_name
         FROM brain_facts f
         LEFT JOIN brain_memory_layers l ON l.layer_id = f.layer_id
         WHERE ($1::uuid IS NULL OR f.project_id = $1 OR f.project_id IS NULL)
         ORDER BY f.observed_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| FactRow {
        fact_id: r.fact_id,
        subject: r.subject,
        predicate: r.predicate,
        object: r.object,
        valid_from: r.valid_from,
        observed_at: r.observed_at,
        valid_to: r.valid_to,
        subject_entity_id: r.subject_entity_id,
        project_id: r.project_id,
        confidence: r.confidence,
        is_current: Some(r.is_current),
        created_at: Some(r.created_at),
        layer_name: r.layer_name,
    })
    .collect();
    write_bundle_file(
        &root,
        &root.join("facts.json"),
        &serde_json::to_vec_pretty(&fact_rows)?,
        &mut digest,
    )?;

    let procedure_rows: Vec<ProcedureRow> = sqlx::query_as::<_, ProcedureRow>(
        "SELECT id, name, steps, created_at, updated_at, trigger_context, preconditions,
                verification, success_count, failure_count, last_outcome, last_used_at,
                project_id, embedding_model
         FROM brain_procedures
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY created_at",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    write_bundle_file(
        &root,
        &root.join("procedures.json"),
        &serde_json::to_vec_pretty(&procedure_rows)?,
        &mut digest,
    )?;

    let artifact_rows: Vec<ArtifactRow> = sqlx::query_as::<
        _,
        (
            uuid::Uuid,
            String,
            String,
            String,
            i64,
            Option<uuid::Uuid>,
            Option<String>,
            Option<String>,
            i64,
            chrono::DateTime<Utc>,
        ),
    >(
        "SELECT id, path, content, content_hash, version, project_id, origin_node,
                crdt_actor, crdt_counter, updated_at
         FROM brain_artifacts
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY path",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(
        |(
            id,
            path,
            content,
            content_hash,
            version,
            project_id,
            origin_node,
            crdt_actor,
            crdt_counter,
            updated_at,
        )| ArtifactRow {
            id,
            path,
            content,
            content_hash,
            version,
            project_id,
            origin_node,
            crdt_actor,
            crdt_counter,
            updated_at,
        },
    )
    .collect();
    let artifact_count = artifact_rows.len() as u32;
    write_bundle_file(
        &root,
        &root.join("artifacts.json"),
        &serde_json::to_vec_pretty(&artifact_rows)?,
        &mut digest,
    )?;

    let trust_rows: Vec<SourceTrustRow> = sqlx::query_as::<_, SourceTrustRow>(
        "SELECT source, alpha::float8 AS alpha, beta::float8 AS beta, updated_at
         FROM brain_source_trust ORDER BY source",
    )
    .fetch_all(pool)
    .await?;
    write_bundle_file(
        &root,
        &root.join("source_trust.json"),
        &serde_json::to_vec_pretty(&trust_rows)?,
        &mut digest,
    )?;

    let rel_count = relation_rows.len() as u32;
    write_bundle_file(
        &root,
        &root.join("relations.json"),
        &serde_json::to_vec_pretty(&relation_rows)?,
        &mut digest,
    )?;
    write_bundle_file(
        &root,
        &root.join("projects.json"),
        &serde_json::to_vec_pretty(&projects)?,
        &mut digest,
    )?;

    type TombstoneRow = (String, Uuid, chrono::DateTime<Utc>, Option<String>);
    let tombstones: Vec<TombstoneRow> = sqlx::query_as(
        "SELECT table_name, row_id, deleted_at, origin_node FROM brain_tombstones
         ORDER BY deleted_at",
    )
    .fetch_all(pool)
    .await?;
    let tombstone_rows: Vec<Value> = tombstones
        .into_iter()
        .map(|(table_name, row_id, deleted_at, origin_node)| {
            serde_json::json!({
                "table_name": table_name,
                "row_id": row_id,
                "deleted_at": deleted_at,
                "origin_node": origin_node,
            })
        })
        .collect();
    let tombstone_count = tombstone_rows.len() as u32;
    write_bundle_file(
        &root,
        &root.join("tombstones.json"),
        &serde_json::to_vec_pretty(&tombstone_rows)?,
        &mut digest,
    )?;

    if with_embeddings && !emb_blob.is_empty() {
        let compressed = crate::sync::compressor::compress(&emb_blob)?;
        let blob_path = root.join("embeddings.bin.zst");
        ensure_within(&root, &blob_path)?;
        std::fs::write(&blob_path, compressed)?;
        digest.record(&root, &blob_path, &emb_blob);
    }

    let counts = Counts {
        entities: entity_files,
        observations: obs_count,
        episodes: episode_count,
        decisions: decisions.len() as u32,
        errors: err_count,
        relations: rel_count,
        artifacts: artifact_count,
    };
    let manifest = Manifest {
        schema_version: SCHEMA_VERSION,
        manifest_hash: digest.finish(project_id),
        project_id,
        project_name,
        exported_at: Utc::now(),
        counts: counts.clone(),
        with_embeddings,
        embedding_dim: emb_dim,
        embedding_model: Some(crate::embeddings::onnx::model_fingerprint()),
        node_id: Some(crate::db::node_id(pool).await?),
    };
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    let warning = if entity_files > 5000 {
        Some(format!(
            "exported {entity_files} entity files; consider partitioning further"
        ))
    } else {
        None
    };

    lock.commit().await?;

    Ok(serde_json::json!({
        "action": "export",
        "dir": root.display().to_string(),
        "tombstones": tombstone_count,
        "manifest_hash": manifest.manifest_hash,
        "counts": counts,
        "with_embeddings": with_embeddings,
        "node_id": manifest.node_id,
        "warning": warning,
    }))
}

pub const OBSERVATION_TYPES: [&str; 9] = [
    "fact",
    "decision",
    "lesson",
    "preference",
    "error",
    "solution",
    "context",
    "tool_usage",
    "superseded",
];

pub const OBSERVATION_SOURCES: [&str; 5] = [
    "agent",
    "error_detection",
    "user",
    "consolidation",
    "inference",
];

pub const RELATION_PROVENANCES: [&str; 3] = ["extracted", "inferred", "predicted"];

type ConflictRow = (Uuid, Uuid, String, String, Option<String>, DateTime<Utc>);

async fn conflicts(pool: &PgPool) -> Result<Value> {
    let rows: Vec<ConflictRow> = sqlx::query_as(
        "SELECT id, observation_id, local_content, incoming_content, incoming_origin_node,
                detected_at
         FROM brain_sync_conflicts WHERE resolved_at IS NULL
         ORDER BY detected_at DESC LIMIT 100",
    )
    .fetch_all(pool)
    .await?;

    let open: Vec<Value> = rows
        .into_iter()
        .map(|(id, obs, ours, theirs, node, at)| {
            serde_json::json!({
                "id": id,
                "observation_id": obs,
                "ours": ours,
                "theirs": theirs,
                "their_node": node,
                "detected_at": at,
            })
        })
        .collect();

    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM brain_sync_conflicts WHERE resolved_at IS NULL")
            .fetch_one(pool)
            .await?;

    Ok(serde_json::json!({
        "action": "conflicts",
        "open": open.len(),
        "total_open": total,
        "conflicts": open,
        "note": "close one with cuba_sync action=resolve, id=<id>, keep=ours|theirs|both. \
                 'both' keeps what is here and records the other machine's text in \
                 previous_versions, which is the only choice that discards nothing."
    }))
}

async fn resolve_conflict(pool: &PgPool, args: &Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| {
            anyhow::anyhow!("resolve needs the id of a conflict from action=conflicts")
        })?;
    let keep = args.get("keep").and_then(Value::as_str).unwrap_or("both");
    if !matches!(keep, "ours" | "theirs" | "both") {
        anyhow::bail!("keep must be ours, theirs or both; got {keep:?}");
    }

    let mut tx = pool.begin().await?;
    let row: Option<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT observation_id, local_content, incoming_content, incoming_origin_node
         FROM brain_sync_conflicts WHERE id = $1 AND resolved_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((observation_id, ours, theirs, their_node)) = row else {
        anyhow::bail!("no open conflict with id {id}: it may already be resolved");
    };

    let loser = match keep {
        "theirs" => Some((ours.clone(), None)),
        "both" => Some((theirs.clone(), their_node.clone())),
        _ => None,
    };
    if let Some((text, node)) = loser {
        sqlx::query(
            "UPDATE brain_observations SET previous_versions = brain_append_version(
                 previous_versions,
                 jsonb_build_array(jsonb_build_object(
                     'content', $2::text,
                     'version', version,
                     'origin_node', $3::text,
                     'superseded_at', NOW()::text)))
             WHERE id = $1",
        )
        .bind(observation_id)
        .bind(&text)
        .bind(node.as_deref().unwrap_or("this node"))
        .execute(&mut *tx)
        .await?;
    }

    if keep == "theirs" {
        sqlx::query(
            "UPDATE brain_observations
             SET content = $2, version = version + 1, embedding = NULL, embedding_half = NULL
             WHERE id = $1",
        )
        .bind(observation_id)
        .bind(&theirs)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE brain_sync_conflicts SET resolved_at = NOW(), resolution = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(keep)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    crate::events::publish(
        "sync.conflict_resolved",
        serde_json::json!({
            "id": id,
            "observation_id": observation_id,
            "kept": keep,
        }),
    );

    Ok(serde_json::json!({
        "action": "resolve",
        "id": id,
        "observation_id": observation_id,
        "kept": keep,
        "note": match keep {
            "ours" => "the incoming text was already on the floor and stays there; this only \
                       closes the record so it stops being reported",
            "theirs" => "the incoming text is now the observation's content, the local one went \
                         into previous_versions, and the embedding was cleared because it \
                         described the text that is no longer there",
            _ => "this machine's text stays current and the other machine's went into \
                  previous_versions, where cuba_cronica reads it back",
        },
    }))
}

const PULL_PAGE_BYTES: usize = 3 * 1024 * 1024;

const MAX_NOTICE_CHARS: usize = 2000;

const PEER_INBOX: &str = ".peer-inbox";

fn peer_token_or_refuse() -> Result<String> {
    std::env::var("CUBA_PEER_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "CUBA_PEER_TOKEN is unset, so there is no secret to present to the other \
                 machine. It is the same string on both nodes: the one this daemon accepts \
                 from a peer, and the one it sends when it is the peer"
            )
        })
}

async fn ask_peer(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    arguments: Value,
) -> Result<Value> {
    let endpoint = format!("{}/mcp", url.trim_end_matches('/'));
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "cuba_sync", "arguments": arguments},
    });

    let response = tokio::time::timeout(
        crate::protocol::handler_timeout(),
        client.post(&endpoint).bearer_auth(token).json(&body).send(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("{endpoint} did not answer within the handler budget"))?
    .with_context(|| format!("reaching {endpoint}"))?;

    let status = response.status();
    let payload: Value = tokio::time::timeout(crate::protocol::handler_timeout(), response.json())
        .await
        .map_err(|_| anyhow::anyhow!("{endpoint} answered {status} and then stalled mid-body"))?
        .with_context(|| format!("{endpoint} answered {status} with something that is not JSON"))?;

    if let Some(error) = payload.get("error") {
        anyhow::bail!("{endpoint} refused: {error}");
    }
    let text = payload["result"]["content"][0]["text"]
        .as_str()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{endpoint} answered {status} without an MCP content envelope: {payload}"
            )
        })?;
    serde_json::from_str(text).with_context(|| format!("{endpoint} sent a body that is not JSON"))
}

pub async fn announce_to_peers(pool: &PgPool) -> Result<u32> {
    let peers: Vec<(String, String)> = sqlx::query_as("SELECT name, url FROM brain_sync_peers")
        .fetch_all(pool)
        .await?;
    if peers.is_empty() {
        return Ok(0);
    }

    let token = peer_token_or_refuse()?;
    let client = reqwest::Client::builder()
        .build()
        .context("building the peer client")?;
    let node_id = crate::db::node_id(pool).await.ok();
    let node_name = std::env::var("CUBA_NODE_NAME")
        .ok()
        .filter(|v| !v.trim().is_empty());

    let mut reached = 0u32;
    for (name, url) in peers {
        let told = ask_peer(
            &client,
            &url,
            &token,
            serde_json::json!({
                "action": "notify",
                "summary": "this node has memory the other side has not taken yet",
                "node_id": node_id,
                "node_name": node_name,
            }),
        )
        .await;
        match told {
            Ok(_) => reached += 1,
            Err(e) => tracing::warn!(peer = %name, error = %format!("{e:#}"), "could not announce"),
        }
    }
    Ok(reached)
}

async fn fetch(pool: &PgPool, args: &Value, conflict: &str, confirm: bool) -> Result<Value> {
    let deletes_allowed = args
        .get("deletes")
        .and_then(Value::as_str)
        .map(|d| d != "withhold")
        .unwrap_or(true);
    let name = args
        .get("peer")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) => u.to_string(),
        None => sqlx::query_scalar::<_, String>("SELECT url FROM brain_sync_peers WHERE name = $1")
            .bind(&name)
            .fetch_optional(pool)
            .await?
            .or_else(|| {
                std::env::var("CUBA_PEER_URL")
                    .ok()
                    .filter(|u| !u.is_empty())
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "no address for peer {name:?}: pass url, set CUBA_PEER_URL, or let a previous \
                 fetch record it"
                )
            })?,
    };
    let token = peer_token_or_refuse()?;

    let client = reqwest::Client::builder()
        .build()
        .context("building the peer client")?;

    let known: Option<String> =
        sqlx::query_scalar("SELECT last_manifest_hash FROM brain_sync_peers WHERE name = $1")
            .bind(&name)
            .fetch_optional(pool)
            .await?
            .flatten();

    let outcome = drain_peer(pool, &client, &url, &token, known.as_deref()).await;

    let (result, error) = match outcome {
        Ok(value) => (Some(value), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };

    let landed_hash = result
        .as_ref()
        .and_then(|v| v["manifest_hash"].as_str())
        .map(str::to_string);

    sqlx::query(
        "INSERT INTO brain_sync_peers (name, url, last_manifest_hash, last_synced_at, last_error)
         VALUES ($1, $2, $3, CASE WHEN $4::text IS NULL THEN NOW() ELSE NULL END, $4)
         ON CONFLICT (name) DO UPDATE SET
             url = EXCLUDED.url,
             last_manifest_hash = COALESCE(EXCLUDED.last_manifest_hash,
                                           brain_sync_peers.last_manifest_hash),
             last_synced_at = COALESCE(EXCLUDED.last_synced_at, brain_sync_peers.last_synced_at),
             last_error = EXCLUDED.last_error",
    )
    .bind(&name)
    .bind(&url)
    .bind(landed_hash.as_deref())
    .bind(error.as_deref())
    .execute(pool)
    .await
    .context("recording what this peer last handed over")?;

    let staged = match result {
        Some(v) => v,
        None => anyhow::bail!(
            "{}",
            error.unwrap_or_else(|| "the peer fetch failed".to_string())
        ),
    };

    if staged["unchanged"].as_bool() == Some(true) {
        return Ok(serde_json::json!({
            "action": "fetch",
            "peer": name,
            "url": url,
            "unchanged": true,
            "manifest_hash": staged["manifest_hash"],
            "note": "the peer is offering the same bundle this node already took, so nothing \
                     was written and no transaction was opened. Without this the cycle never \
                     converges in work: importance and access_count move with ordinary use, so \
                     every export produces a new hash and each side re-imports forever."
        }));
    }

    let landed = staged["dir"].as_str().unwrap_or_default().to_string();
    let imported = import(pool, Some(&landed), conflict, confirm, deletes_allowed).await;
    let _ = std::fs::remove_dir_all(&landed);
    let imported = imported?;

    let closed = match staged["node_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        Some(peer_node) => sqlx::query(
            "UPDATE brain_peer_notices SET resolved_at = NOW()
             WHERE resolved_at IS NULL AND node_id = $1",
        )
        .bind(peer_node)
        .execute(pool)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0),
        None => 0,
    };

    crate::events::publish(
        "sync.fetch",
        serde_json::json!({
            "peer": name,
            "url": url,
            "notices_closed": closed,
            "imported": imported.get("rows_inserted").cloned().unwrap_or(Value::Null),
            "manifest_hash": imported.get("manifest_hash").cloned().unwrap_or(Value::Null),
        }),
    );

    Ok(serde_json::json!({
        "action": "fetch",
        "peer": name,
        "url": url,
        "files_received": staged["files"],
        "notices_closed": closed,
        "imported": imported,
    }))
}

async fn drain_peer(
    _pool: &PgPool,
    client: &reqwest::Client,
    url: &str,
    token: &str,
    known: Option<&str>,
) -> Result<Value> {
    let inbox = resolve_dir(Some(PEER_INBOX))?;
    let mut offset = 0u64;
    let mut files = 0usize;
    let mut hash: Option<String> = None;
    let mut node_id: Option<String> = None;

    loop {
        let page = ask_peer(
            client,
            url,
            token,
            serde_json::json!({"action": "pull", "offset": offset}),
        )
        .await?;

        let page_hash = page["manifest_hash"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the peer sent a page with no manifest_hash: {page}"))?
            .to_string();

        if offset == 0 {
            if known == Some(page_hash.as_str()) {
                return Ok(serde_json::json!({
                    "unchanged": true,
                    "manifest_hash": page_hash,
                }));
            }
            let _ = std::fs::remove_dir_all(&inbox);
            std::fs::create_dir_all(&inbox).context("preparing the peer inbox")?;
        } else if hash.as_deref() != Some(page_hash.as_str()) {
            let _ = std::fs::remove_dir_all(&inbox);
            anyhow::bail!(
                "the peer changed underneath this transfer: page 0 described {} and this one \
                 describes {page_hash}. The pages no longer belong to one state, so they were \
                 discarded rather than imported — a torn bundle that happens to parse is the \
                 one failure that commits and records its hash forever",
                hash.unwrap_or_default()
            );
        }
        hash = Some(page_hash);
        if node_id.is_none() {
            node_id = page["node_id"].as_str().map(str::to_string);
        }

        for file in page["files"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("a page without files: {page}"))?
        {
            let relative = file["path"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("a file with no path: {file}"))?;
            let target = inbox.join(relative);
            ensure_within(&inbox, &target)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            match (file["text"].as_str(), file["hex"].as_str()) {
                (Some(text), _) => std::fs::write(&target, text)?,
                (None, Some(blob)) => std::fs::write(&target, hex::decode(blob)?)?,
                (None, None) => anyhow::bail!("a file arrived with neither text nor hex: {file}"),
            }
            files += 1;
        }

        if !page["has_more"].as_bool().unwrap_or(false) {
            break;
        }
        offset = page["next_offset"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("has_more with no next_offset: {page}"))?;
    }

    Ok(serde_json::json!({
        "unchanged": false,
        "manifest_hash": hash,
        "node_id": node_id,
        "files": files,
        "dir": inbox.display().to_string(),
    }))
}

async fn notify(pool: &PgPool, args: &Value) -> Result<Value> {
    let summary = args
        .get("summary")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "notify needs a summary: what the other machine learned, in the words the model \
                 there would use. A bell with no message tells the local model to pull and \
                 nothing about whether it should"
            )
        })?;
    if summary.chars().count() > MAX_NOTICE_CHARS {
        anyhow::bail!(
            "a notice is a signal, not a payload: {MAX_NOTICE_CHARS} characters at most, and \
             this one is {}. The memory travels through pull, which is validated and \
             quarantined; the notice is only there to say it is worth pulling",
            summary.chars().count()
        );
    }
    if let Some(pattern) = crate::redact::looks_like_secret(summary) {
        anyhow::bail!(
            "refusing a peer notice that carries what looks like a {pattern}. Every other \
             free-text entry rejects credentials at the door and this one arrives over the \
             network from another machine, which makes it the last place to make an exception"
        );
    }

    let node_id = args
        .get("node_id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok());
    let node_name = args.get("node_name").and_then(Value::as_str);
    let manifest_hash = args.get("manifest_hash").and_then(Value::as_str);

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO brain_peer_notices (node_id, node_name, summary, manifest_hash)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(node_id)
    .bind(node_name)
    .bind(summary)
    .bind(manifest_hash)
    .fetch_one(pool)
    .await
    .context("recording the peer notice")?;

    crate::events::publish(
        "peer.notice",
        serde_json::json!({
            "id": id,
            "from": node_name,
            "summary": summary,
            "manifest_hash": manifest_hash,
        }),
    );

    Ok(serde_json::json!({
        "action": "notify",
        "id": id,
        "recorded": true,
        "note": "the local model sees this the next time it opens a session or asks cuba_sync \
                 for status. Nothing here entered the graph: a peer rings the bell, it does not \
                 decide what this database remembers."
    }))
}

type NoticeRow = (
    Uuid,
    Option<String>,
    String,
    Option<String>,
    chrono::DateTime<Utc>,
);

pub async fn pending_notices(pool: &PgPool) -> Result<Vec<Value>> {
    let rows: Vec<NoticeRow> = sqlx::query_as(
        "SELECT id, node_name, summary, manifest_hash, created_at
             FROM brain_peer_notices WHERE resolved_at IS NULL
             ORDER BY created_at DESC LIMIT 20",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, node_name, summary, manifest_hash, created_at)| {
            serde_json::json!({
                "id": id,
                "from": node_name,
                "summary": summary,
                "manifest_hash": manifest_hash,
                "at": created_at,
            })
        })
        .collect())
}

fn bundle_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

async fn pull(
    pool: &PgPool,
    offset: usize,
    limit: Option<usize>,
    with_embeddings: bool,
) -> Result<Value> {
    let staging = std::env::temp_dir().join(format!("cuba-pull-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&staging).context("staging directory for a peer pull")?;

    let served = pull_from_staging(pool, &staging, offset, limit, with_embeddings).await;
    let _ = std::fs::remove_dir_all(&staging);
    served
}

async fn pull_from_staging(
    pool: &PgPool,
    staging: &Path,
    offset: usize,
    limit: Option<usize>,
    with_embeddings: bool,
) -> Result<Value> {
    let summary = export_into(pool, staging, "all", with_embeddings).await?;
    let paths = bundle_files(staging)?;
    let total = paths.len();

    let mut files = Vec::new();
    let mut bytes = 0usize;
    for path in paths.iter().skip(offset) {
        let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let relative = path
            .strip_prefix(staging)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let encoded = match String::from_utf8(raw.clone()) {
            Ok(text) => serde_json::json!({"path": relative, "text": text}),
            Err(_) => serde_json::json!({"path": relative, "hex": hex::encode(&raw)}),
        };
        let size = encoded.to_string().len();
        let full = bytes + size > PULL_PAGE_BYTES || limit.is_some_and(|n| files.len() >= n);
        if !files.is_empty() && full {
            break;
        }
        bytes += size;
        files.push(encoded);
    }

    let delivered = offset + files.len();
    Ok(serde_json::json!({
        "action": "pull",
        "manifest_hash": summary["manifest_hash"],
        "node_id": summary["node_id"],
        "schema_version": crate::sync::chunk::SCHEMA_VERSION,
        "with_embeddings": with_embeddings,
        "total_files": total,
        "offset": offset,
        "files": files,
        "has_more": delivered < total,
        "next_offset": delivered,
        "note": "every page re-exports the whole bundle, so manifest_hash has to be identical \
                 across the pages of one pull. If it changes, this node was written to \
                 mid-transfer and the pages no longer describe one state: discard them and \
                 start again. A torn bundle that happens to parse is the one failure mode that \
                 commits and records its hash forever."
    }))
}

fn recompute_digest(root: &Path, project_id: Option<Uuid>) -> Result<String> {
    let mut digest = BundleDigest::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("reading {}", dir.display()))?
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "manifest.json" {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            if name == "embeddings.bin.zst" {
                let raw = crate::sync::compressor::decompress(&bytes)?;
                digest.record(root, &path, &raw);
            } else if path.extension().is_some_and(|e| e == "json") {
                digest.record(root, &path, &bytes);
            }
        }
    }
    Ok(digest.finish(project_id))
}

pub const SYNC_LOCK_WAIT: &str = "10s";
pub const SYNC_STATEMENT_BUDGET: &str = "300s";

async fn take_sync_lock(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<()> {
    sqlx::query(&format!("SET LOCAL lock_timeout = '{SYNC_LOCK_WAIT}'"))
        .execute(&mut **tx)
        .await
        .context("bounding how long this sync may wait for the lock")?;
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = '{SYNC_STATEMENT_BUDGET}'"
    ))
    .execute(&mut **tx)
    .await
    .context("bounding how long a single sync statement may run")?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(SYNC_LOCK)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "another sync is holding the lock and this one waited {SYNC_LOCK_WAIT} for it. \
                 The export writes files and prunes the ones it did not write, and the import \
                 runs in one transaction, so the two must never overlap. Nothing was written. \
                 Underlying: {e}"
            )
        })?;
    Ok(())
}

pub const TOMBSTONED_TABLES: [(&str, &str); 8] = [
    ("brain_entities", "id"),
    ("brain_observations", "id"),
    ("brain_episodes", "id"),
    ("brain_errors", "id"),
    ("brain_relations", "id"),
    ("brain_projects", "id"),
    ("brain_procedures", "id"),
    ("brain_facts", "fact_id"),
];

fn tombstone_key(table: &str) -> Option<&'static str> {
    TOMBSTONED_TABLES
        .iter()
        .find(|(t, _)| *t == table)
        .map(|(_, k)| *k)
}

const TOMBSTONE_ALARM_RATIO: f64 = 0.10;
const TOMBSTONE_ALARM_FLOOR: i64 = 25;

#[derive(Default)]
struct Applied {
    deleted: HashMap<String, u32>,
    withheld: Vec<String>,
    buried: HashSet<Uuid>,
}

async fn apply_tombstones(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    root: &Path,
    confirm: bool,
    deletes_allowed: bool,
) -> Result<Applied> {
    let path = root.join("tombstones.json");
    let mut applied = Applied::default();
    if !path.exists() {
        return Ok(applied);
    }
    let rows: Vec<Value> = serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("parse {}", path.display()))?;
    if rows.is_empty() {
        return Ok(applied);
    }

    let mut by_table: HashMap<String, Vec<Uuid>> = HashMap::new();
    for row in &rows {
        let (Some(table), Some(id)) = (
            row.get("table_name").and_then(Value::as_str),
            row.get("row_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok()),
        ) else {
            continue;
        };
        if tombstone_key(table).is_none() {
            anyhow::bail!(
                "tombstones.json names table {table:?}, which this build does not delete from. \
                 A tombstone is a licence to destroy rows; honouring one for a table nobody \
                 vetted is how a sync turns into a wipe."
            );
        }
        applied.buried.insert(id);
        by_table.entry(table.to_string()).or_default().push(id);
    }

    if !deletes_allowed {
        let named: usize = by_table.values().map(Vec::len).sum();
        if named > 0 {
            applied.withheld.push(format!(
                "{named} tombstone(s) were not applied because this caller asked for no \
                 deletions. Below the alarm threshold a sync deletes without asking — that is \
                 deliberate, because a guard that trips on ordinary curation is one everybody \
                 learns to pass confirm=true through. It is the wrong default for a button, so \
                 the control panel takes this path and a deletion stays something you ask for \
                 on purpose"
            ));
        }
        applied.buried.clear();
        return Ok(applied);
    }

    let mut would_delete = 0i64;
    for (table, ids) in &by_table {
        let key = tombstone_key(table).expect("checked above");
        let present: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE {key} = ANY($1)"
        ))
        .bind(ids)
        .fetch_one(&mut **tx)
        .await?;
        would_delete += present;
    }
    if would_delete > 0 && !confirm {
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM brain_observations")
            .fetch_one(&mut **tx)
            .await?;
        let ratio = would_delete as f64 / (total.max(1)) as f64;
        if would_delete >= TOMBSTONE_ALARM_FLOOR && ratio > TOMBSTONE_ALARM_RATIO {
            anyhow::bail!(
                "refusing to apply {would_delete} deletions, which is {:.0}% of the \
                 {total} observations on this machine. A bundle that deletes a tenth of your \
                 memory is either a mistake or a peer you should not be trusting — a remote \
                 wipe looks exactly like this. Re-run with confirm=true if you meant it. \
                 (Below {TOMBSTONE_ALARM_FLOOR} rows this never fires: a guard that trips on \
                 ordinary curation is one everybody learns to pass confirm=true through, and \
                 then it guards nothing.)",
                ratio * 100.0
            );
        }
    }

    for (table, ids) in by_table.iter().filter(|(t, _)| *t != "brain_entities") {
        let key = tombstone_key(table).expect("checked above");
        let done = sqlx::query(&format!("DELETE FROM {table} WHERE {key} = ANY($1)"))
            .bind(ids)
            .execute(&mut **tx)
            .await?;
        if done.rows_affected() > 0 {
            applied
                .deleted
                .insert(table.clone(), done.rows_affected() as u32);
        }
    }

    if let Some(ids) = by_table.get("brain_entities") {
        for id in ids {
            let children: i64 = sqlx::query_scalar(
                "SELECT (SELECT count(*) FROM brain_observations WHERE entity_id = $1)
                      + (SELECT count(*) FROM brain_episodes WHERE entity_id = $1)",
            )
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
            if children > 0 {
                applied.withheld.push(format!(
                    "brain_entities {id}: {children} row(s) here were never named by the \
                     sender, and deleting the entity would cascade them away"
                ));
                continue;
            }
            let done = sqlx::query("DELETE FROM brain_entities WHERE id = $1")
                .bind(id)
                .execute(&mut **tx)
                .await?;
            if done.rows_affected() > 0 {
                *applied
                    .deleted
                    .entry("brain_entities".to_string())
                    .or_insert(0) += 1;
            }
        }
    }

    Ok(applied)
}

async fn validate_bundle(pool: &PgPool, root: &Path) -> Result<()> {
    let mut offences: Vec<String> = Vec::new();
    let mut known: HashSet<Uuid> = HashSet::new();
    let mut names: Vec<String> = Vec::new();

    let entities_dir = root.join("entities");
    if entities_dir.exists() {
        for entry in std::fs::read_dir(&entities_dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let file: EntityFile = serde_json::from_slice(&std::fs::read(&path)?)
                .with_context(|| format!("parse {}", path.display()))?;
            known.insert(file.id);
            names.push(file.name.clone());
            for obs in &file.observations {
                if !OBSERVATION_TYPES.contains(&obs.observation_type.as_str()) {
                    offences.push(format!(
                        "{}: observation {} has observation_type {:?}",
                        path.display(),
                        obs.id,
                        obs.observation_type
                    ));
                }
                if !OBSERVATION_SOURCES.contains(&obs.source.as_str()) {
                    offences.push(format!(
                        "{}: observation {} has source {:?}",
                        path.display(),
                        obs.id,
                        obs.source
                    ));
                }
            }
        }
    }

    let local: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM brain_entities")
        .fetch_all(pool)
        .await?;
    known.extend(local);
    let local_named: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM brain_entities WHERE name = ANY($1)")
            .bind(&names)
            .fetch_all(pool)
            .await?;
    known.extend(local_named);

    let relations_path = root.join("relations.json");
    if relations_path.exists() {
        let rels: Vec<RelationRow> = serde_json::from_slice(&std::fs::read(&relations_path)?)
            .with_context(|| format!("parse {}", relations_path.display()))?;
        for rel in &rels {
            for (side, id) in [
                ("from_entity", rel.from_entity),
                ("to_entity", rel.to_entity),
            ] {
                if !known.contains(&id) {
                    offences.push(format!(
                        "relations.json: relation {} {side} {id} exists nowhere",
                        rel.id
                    ));
                }
            }
            if !RELATION_PROVENANCES.contains(&rel.provenance.as_str()) {
                offences.push(format!(
                    "relations.json: relation {} has provenance {:?}",
                    rel.id, rel.provenance
                ));
            }
        }
    }

    let episodes_root = root.join("episodes");
    if episodes_root.exists() {
        for month in std::fs::read_dir(&episodes_root)? {
            let month = month?.path();
            if !month.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&month)? {
                let path = entry?.path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let f: EpisodeFile = serde_json::from_slice(&std::fs::read(&path)?)
                    .with_context(|| format!("parse {}", path.display()))?;
                if !known.contains(&f.entity_id) {
                    offences.push(format!(
                        "{}: episode {} hangs off entity {} which exists nowhere",
                        path.display(),
                        f.id,
                        f.entity_id
                    ));
                }
            }
        }
    }

    if offences.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "refusing to import: {} row(s) would be rejected by the database, and the import runs \
         in one transaction — the first rejection takes every other row down with it, hundreds \
         deep, with a bare Postgres error. Nothing was written. Fix the bundle and retry.\n  {}",
        offences.len(),
        offences
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n  ")
    )
}

async fn import(
    pool: &PgPool,
    dir_arg: Option<&str>,
    conflict: &str,
    confirm: bool,
    deletes_allowed: bool,
) -> Result<Value> {
    let root = resolve_dir(dir_arg)?;
    let manifest_path = root.join("manifest.json");
    if !manifest_path.exists() {
        anyhow::bail!("no manifest.json at {}", root.display());
    }
    let manifest_bytes = std::fs::read(&manifest_path)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse manifest.json")?;

    if manifest.schema_version > SCHEMA_VERSION + 1 {
        anyhow::bail!(
            "manifest schema_version {} is too new (this build supports {})",
            manifest.schema_version,
            SCHEMA_VERSION
        );
    }

    validate_bundle(pool, &root).await?;

    let overwrite = match conflict {
        "skip" | "merge" => false,
        "overwrite" => true,
        _ => anyhow::bail!("invalid conflict policy: {conflict}"),
    };

    let mut tx = pool.begin().await?;
    take_sync_lock(&mut tx).await?;

    let scoped = match manifest.project_id {
        Some(pid) => {
            sqlx::query("SELECT set_config('app.current_project', $1, true)")
                .bind(pid.to_string())
                .execute(&mut *tx)
                .await
                .context("declaring the scope this bundle is allowed to write into")?;
            true
        }
        None => false,
    };

    let tombstones = apply_tombstones(&mut tx, &root, confirm, deletes_allowed).await?;

    if manifest.with_embeddings {
        let local_dim = crate::embeddings::onnx::embedding_dim();
        let dim = manifest.embedding_dim.unwrap_or(local_dim);
        if dim != local_dim {
            anyhow::bail!(
                "this bundle carries {dim}-dimensional vectors and this machine produces \
                 {local_dim}. Writing them would either abort the whole import on the first \
                 UPDATE or, if the record size happened to divide evenly, fill the index with \
                 vectors from a space these queries do not live in. Re-export without \
                 embeddings and run `cuba-memorys reembed` here, which is cheaper than it \
                 sounds and correct by construction."
            );
        }
        if let Some(theirs) = manifest.embedding_model.as_deref() {
            let ours = crate::embeddings::onnx::model_fingerprint();
            if theirs != ours {
                anyhow::bail!(
                    "this bundle's vectors come from {theirs} and this machine runs {ours}. \
                     Same dimension is not the same space: cosine similarity between two \
                     models' embeddings is not a similarity at all, and nothing downstream \
                     would notice — the searches would just quietly get worse. Re-export \
                     without embeddings and reembed here."
                );
            }
        }
    }

    let on_disk = recompute_digest(&root, manifest.project_id)?;
    let tampered = on_disk != manifest.manifest_hash;

    let already: Option<(i32,)> =
        sqlx::query_as("SELECT rows_inserted FROM brain_sync_state WHERE manifest_hash = $1")
            .bind(&on_disk)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((prev,)) = already {
        return Ok(serde_json::json!({
            "action": "import",
            "skipped": true,
            "reason": "these exact files were already imported",
            "previous_rows_inserted": prev,
        }));
    }

    let mut inserted = 0u32;
    let mut diverged: Vec<Uuid> = Vec::new();
    let mut remapped: HashMap<Uuid, Uuid> = HashMap::new();
    let mut superseded_facts = 0u32;
    let mut quarantined = 0u32;
    let mut quarantine_reasons: HashMap<&'static str, u32> = HashMap::new();

    let projects_path = root.join("projects.json");
    if projects_path.exists() {
        let projects: Vec<ProjectRow> = serde_json::from_slice(&std::fs::read(projects_path)?)?;
        let mut seen: HashSet<Uuid> = HashSet::new();
        let batch: Vec<Value> = projects
            .iter()
            .filter(|p| seen.insert(p.id))
            .map(|p| serde_json::json!({"id": p.id, "name": p.name, "created_at": p.created_at}))
            .collect();

        if !batch.is_empty() {
            let r = sqlx::query(&format!(
                "INSERT INTO brain_projects (id, name, created_at)
                 SELECT u.id, u.name, u.created_at
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     id uuid, name text, created_at timestamptz)
                 ON CONFLICT (id) DO {}",
                if overwrite {
                    "UPDATE SET name = EXCLUDED.name, created_at = EXCLUDED.created_at"
                } else {
                    "NOTHING"
                }
            ))
            .bind(Value::Array(batch))
            .execute(&mut *tx)
            .await?;
            inserted += r.rows_affected() as u32;
        }
    }

    let entities_dir = root.join("entities");
    if entities_dir.exists() {
        let mut files: Vec<EntityFile> = Vec::new();
        for entry in std::fs::read_dir(entities_dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let file: EntityFile = serde_json::from_slice(&std::fs::read(&path)?)?;
            if tombstones.buried.contains(&file.id) {
                continue;
            }
            files.push(file);
        }

        let names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
        let existing: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, name FROM brain_entities WHERE name = ANY($1)")
                .bind(&names)
                .fetch_all(&mut *tx)
                .await?;
        let by_name: HashMap<String, Uuid> = existing.into_iter().map(|(id, n)| (n, id)).collect();

        let mut entity_rows: Vec<Value> = Vec::with_capacity(files.len());
        let mut seen_entities: HashSet<Uuid> = HashSet::new();
        for file in &files {
            let entity_id = match by_name.get(&file.name) {
                Some(existing) if *existing != file.id => {
                    remapped.insert(file.id, *existing);
                    *existing
                }
                _ => file.id,
            };
            if !seen_entities.insert(entity_id) {
                continue;
            }
            entity_rows.push(serde_json::json!({
                "id": entity_id,
                "name": file.name,
                "entity_type": file.entity_type,
                "importance": file.importance,
                "access_count": file.access_count,
                "project_id": file.project_id,
                "created_at": file.created_at,
            }));
        }

        if !entity_rows.is_empty() {
            let r = sqlx::query(&format!(
                "INSERT INTO brain_entities
                    (id, name, entity_type, importance, access_count, project_id, created_at)
                 SELECT u.id, u.name, u.entity_type, u.importance, u.access_count,
                        u.project_id, u.created_at
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     id uuid, name text, entity_type text, importance float8,
                     access_count int, project_id uuid, created_at timestamptz)
                 ON CONFLICT (id) DO {}",
                if overwrite {
                    "UPDATE SET name = EXCLUDED.name, entity_type = EXCLUDED.entity_type, \
                     importance = GREATEST(brain_entities.importance, EXCLUDED.importance), \
                     access_count = GREATEST(brain_entities.access_count, EXCLUDED.access_count), \
                     project_id = EXCLUDED.project_id, created_at = EXCLUDED.created_at"
                } else {
                    "UPDATE SET \
                     importance = GREATEST(brain_entities.importance, EXCLUDED.importance), \
                     access_count = GREATEST(brain_entities.access_count, EXCLUDED.access_count)"
                }
            ))
            .bind(Value::Array(entity_rows))
            .execute(&mut *tx)
            .await?;
            inserted += r.rows_affected() as u32;
        }

        let mut obs_rows: Vec<Value> = Vec::new();
        let mut reasons: HashMap<Uuid, &'static str> = HashMap::new();
        let mut seen_obs: HashSet<Uuid> = HashSet::new();
        for file in &files {
            let entity_id = resolve(&remapped, file.id);
            for obs in &file.observations {
                if tombstones.buried.contains(&obs.id) || !seen_obs.insert(obs.id) {
                    continue;
                }
                let (trust, reason) = trust_for_imported(&obs.content, obs.trust.as_deref());
                if let Some(pattern) = reason {
                    reasons.insert(obs.id, pattern);
                }
                obs_rows.push(serde_json::json!({
                    "id": obs.id,
                    "entity_id": entity_id,
                    "content": obs.content,
                    "observation_type": obs.observation_type,
                    "source": obs.source,
                    "importance": obs.importance,
                    "tags": obs.tags,
                    "session_id": obs.session_id,
                    "project_id": obs.project_id,
                    "embedding_model": obs.embedding_model,
                    "created_at": obs.created_at,
                    "trust": trust,
                    "updated_at": obs.updated_at,
                    "version": obs.version,
                    "previous_versions": obs.previous_versions,
                    "origin_node": obs.origin_node,
                    "evidence": obs.evidence,
                    "verification": obs.verification,
                    "verified_at": obs.verified_at,
                }));
            }
        }

        if !obs_rows.is_empty() {
            let batch = Value::Array(obs_rows);

            if !overwrite {
                let already: Vec<Uuid> = sqlx::query_scalar(
                    "INSERT INTO brain_sync_conflicts
                        (observation_id, local_content, incoming_content,
                         incoming_origin_node, manifest_hash)
                     SELECT o.id, o.content, u.content, u.origin_node, $2
                     FROM jsonb_to_recordset($1::jsonb) AS u(
                         id uuid, content text, origin_node text)
                     JOIN brain_observations o ON o.id = u.id
                     WHERE o.content IS DISTINCT FROM u.content
                     ON CONFLICT (observation_id) WHERE resolved_at IS NULL
                     DO UPDATE SET incoming_content = EXCLUDED.incoming_content,
                                   local_content = EXCLUDED.local_content,
                                   incoming_origin_node = EXCLUDED.incoming_origin_node,
                                   manifest_hash = EXCLUDED.manifest_hash,
                                   detected_at = NOW()
                     RETURNING observation_id",
                )
                .bind(&batch)
                .bind(&manifest.manifest_hash)
                .fetch_all(&mut *tx)
                .await?;

                // Leave conflicts OPEN until cuba_sync action=resolve. Auto keep-both
                // was hiding them from action=conflicts (v026) and skipping the operator.
                if !already.is_empty() {
                    crate::crdt::merge_event(
                        "concurrent",
                        serde_json::json!({
                            "kind": "observation",
                            "count": already.len(),
                            "policy": "await_resolve",
                        }),
                    );
                }
                diverged.extend(already);
            }

            let landed: Vec<Uuid> = sqlx::query_scalar(&format!(
                "INSERT INTO brain_observations
                    (id, entity_id, content, observation_type, source, importance,
                     tags, session_id, project_id, embedding_model, created_at, trust,
                     updated_at, version, previous_versions, origin_node,
                     evidence, verification, verified_at)
                 SELECT u.id, u.entity_id, u.content, u.observation_type, u.source,
                        u.importance, COALESCE(u.tags, '{{}}'), u.session_id, u.project_id,
                        u.embedding_model, u.created_at, u.trust,
                        COALESCE(u.updated_at, NOW()), COALESCE(u.version, 1),
                        COALESCE(u.previous_versions, '[]'::jsonb), u.origin_node,
                        COALESCE(u.evidence, 'asserted'), u.verification, u.verified_at
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     id uuid, entity_id uuid, content text, observation_type text,
                     source text, importance float8, tags text[], session_id uuid,
                     project_id uuid, embedding_model text, created_at timestamptz,
                     trust text, updated_at timestamptz, version int,
                     previous_versions jsonb, origin_node text, evidence text,
                     verification text, verified_at timestamptz)
                 ON CONFLICT (id) DO {}
                 RETURNING id",
                if overwrite {
                    "UPDATE SET \
                     previous_versions = brain_append_version( \
                         brain_observations.previous_versions, \
                         jsonb_build_array(jsonb_build_object( \
                             'content', brain_observations.content, \
                             'version', brain_observations.version, \
                             'origin_node', brain_observations.origin_node, \
                             'superseded_at', NOW()::text)) \
                     ), \
                     entity_id = EXCLUDED.entity_id, content = EXCLUDED.content, \
                     observation_type = EXCLUDED.observation_type, source = EXCLUDED.source, \
                     importance = GREATEST(brain_observations.importance, \
                         EXCLUDED.importance), tags = EXCLUDED.tags, \
                     session_id = EXCLUDED.session_id, project_id = EXCLUDED.project_id, \
                     embedding_model = EXCLUDED.embedding_model, \
                     created_at = EXCLUDED.created_at, trust = EXCLUDED.trust, \
                     embedding = CASE WHEN brain_observations.content IS DISTINCT FROM \
                         EXCLUDED.content THEN NULL ELSE brain_observations.embedding END, \
                     embedding_half = CASE WHEN brain_observations.content IS DISTINCT FROM \
                         EXCLUDED.content THEN NULL ELSE brain_observations.embedding_half END \
                     WHERE brain_observations.content IS DISTINCT FROM EXCLUDED.content"
                } else {
                    "NOTHING"
                }
            ))
            .bind(&batch)
            .fetch_all(&mut *tx)
            .await?;

            inserted += landed.len() as u32;
            for id in &landed {
                if let Some(pattern) = reasons.get(id) {
                    quarantined += 1;
                    *quarantine_reasons.entry(pattern).or_insert(0) += 1;
                }
            }
        }
    }

    let episodes_root = root.join("episodes");
    if episodes_root.exists() {
        let mut batch: Vec<Value> = Vec::new();
        let mut reasons: HashMap<Uuid, &'static str> = HashMap::new();
        let mut seen: HashSet<Uuid> = HashSet::new();
        for month_entry in std::fs::read_dir(episodes_root)? {
            let month = month_entry?.path();
            if !month.is_dir() {
                continue;
            }
            for ep_entry in std::fs::read_dir(month)? {
                let path = ep_entry?.path();
                if path.extension().is_none_or(|e| e != "json") {
                    continue;
                }
                let f: EpisodeFile = serde_json::from_slice(&std::fs::read(&path)?)?;
                if !seen.insert(f.id) {
                    continue;
                }
                let (trust, reason) = trust_for_imported(&f.content, None);
                if let Some(pattern) = reason {
                    reasons.insert(f.id, pattern);
                }
                batch.push(serde_json::json!({
                    "id": f.id,
                    "entity_id": resolve(&remapped, f.entity_id),
                    "content": f.content,
                    "actors": f.actors,
                    "artifacts": f.artifacts,
                    "importance": f.importance,
                    "project_id": f.project_id,
                    "started_at": f.started_at,
                    "ended_at": f.ended_at,
                    "trust": trust,
                }));
            }
        }

        if !batch.is_empty() {
            let landed: Vec<Uuid> = sqlx::query_scalar(&format!(
                "INSERT INTO brain_episodes
                    (id, entity_id, content, actors, artifacts, importance,
                     project_id, started_at, ended_at, trust)
                 SELECT u.id, u.entity_id, u.content, COALESCE(u.actors, '{{}}'),
                        COALESCE(u.artifacts, '{{}}'), u.importance, u.project_id,
                        u.started_at, u.ended_at, u.trust
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     id uuid, entity_id uuid, content text, actors text[], artifacts text[],
                     importance float8, project_id uuid, started_at timestamptz,
                     ended_at timestamptz, trust text)
                 ON CONFLICT (id) DO {}
                 RETURNING id",
                if overwrite {
                    "UPDATE SET entity_id = EXCLUDED.entity_id, content = EXCLUDED.content, \
                     actors = EXCLUDED.actors, artifacts = EXCLUDED.artifacts, \
                     importance = EXCLUDED.importance, project_id = EXCLUDED.project_id, \
                     started_at = EXCLUDED.started_at, ended_at = EXCLUDED.ended_at, \
                     trust = EXCLUDED.trust"
                } else {
                    "NOTHING"
                }
            ))
            .bind(Value::Array(batch))
            .fetch_all(&mut *tx)
            .await?;
            inserted += landed.len() as u32;
            for id in &landed {
                if let Some(pattern) = reasons.get(id) {
                    quarantined += 1;
                    *quarantine_reasons.entry(pattern).or_insert(0) += 1;
                }
            }
        }
    }

    let errors_dir = root.join("errors");
    if errors_dir.exists() {
        let mut batch: Vec<Value> = Vec::new();
        let mut reasons: HashMap<Uuid, &'static str> = HashMap::new();
        let mut seen: HashSet<Uuid> = HashSet::new();
        for entry in std::fs::read_dir(errors_dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let e: ErrorFile = serde_json::from_slice(&std::fs::read(&path)?)?;
            if !seen.insert(e.id) {
                continue;
            }
            let searchable = format!(
                "{}\n{}",
                e.error_message,
                e.solution.as_deref().unwrap_or("")
            );
            let (trust, reason) = trust_for_imported(&searchable, None);
            if let Some(pattern) = reason {
                reasons.insert(e.id, pattern);
            }
            batch.push(serde_json::json!({
                "id": e.id,
                "error_type": e.error_type,
                "error_message": e.error_message,
                "solution": e.solution,
                "resolved": e.resolved,
                "project": e.project,
                "project_id": e.project_id,
                "created_at": e.created_at,
                "trust": trust,
            }));
        }

        if !batch.is_empty() {
            let landed: Vec<Uuid> = sqlx::query_scalar(&format!(
                "INSERT INTO brain_errors
                    (id, error_type, error_message, solution, resolved,
                     project, project_id, created_at, trust)
                 SELECT u.id, u.error_type, u.error_message, u.solution, u.resolved,
                        u.project, u.project_id, u.created_at, u.trust
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     id uuid, error_type text, error_message text, solution text,
                     resolved boolean, project text, project_id uuid,
                     created_at timestamptz, trust text)
                 ON CONFLICT (id) DO {}
                 RETURNING id",
                if overwrite {
                    "UPDATE SET error_type = EXCLUDED.error_type, \
                     error_message = EXCLUDED.error_message, solution = EXCLUDED.solution, \
                     resolved = EXCLUDED.resolved, project = EXCLUDED.project, \
                     project_id = EXCLUDED.project_id, created_at = EXCLUDED.created_at, \
                     trust = EXCLUDED.trust"
                } else {
                    "NOTHING"
                }
            ))
            .bind(Value::Array(batch))
            .fetch_all(&mut *tx)
            .await?;
            inserted += landed.len() as u32;
            for id in &landed {
                if let Some(pattern) = reasons.get(id) {
                    quarantined += 1;
                    *quarantine_reasons.entry(pattern).or_insert(0) += 1;
                }
            }
        }
    }

    let facts_path = root.join("facts.json");
    if facts_path.exists() {
        let facts: Vec<FactRow> = serde_json::from_slice(&std::fs::read(&facts_path)?)?;
        let batch: Vec<Value> = facts
            .iter()
            .filter(|f| !tombstones.buried.contains(&f.fact_id))
            .map(|f| {
                serde_json::json!({
                    "fact_id": f.fact_id,
                    "subject": f.subject,
                    "predicate": f.predicate,
                    "object": f.object,
                    "valid_from": f.valid_from,
                    "observed_at": f.observed_at,
                    "valid_to": f.valid_to,
                    "subject_entity_id": f.subject_entity_id.map(|id| resolve(&remapped, id)),
                    "project_id": f.project_id,
                    "confidence": f.confidence,
                    "is_current": f.is_current,
                    "created_at": f.created_at,
                    "layer_name": f.layer_name,
                })
            })
            .collect();

        if !batch.is_empty() {
            let payload = Value::Array(batch);
            let landed: Vec<Uuid> = sqlx::query_scalar(
                "INSERT INTO brain_facts
                    (fact_id, subject, predicate, object, valid_from, observed_at, valid_to,
                     subject_entity_id, project_id, confidence, is_current, created_at, layer_id)
                 SELECT u.fact_id, u.subject, u.predicate, u.object, u.valid_from, u.observed_at,
                        u.valid_to,
                        (SELECT id FROM brain_entities WHERE id = u.subject_entity_id),
                        (SELECT id FROM brain_projects WHERE id = u.project_id),
                        COALESCE(u.confidence, 0.5), COALESCE(u.is_current, TRUE),
                        COALESCE(u.created_at, NOW()), brain_layer_by_name(u.layer_name)
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     fact_id uuid, subject text, predicate text, object text,
                     valid_from timestamptz, observed_at timestamptz, valid_to timestamptz,
                     subject_entity_id uuid, project_id uuid, confidence float8,
                     is_current boolean, created_at timestamptz, layer_name text)
                 ON CONFLICT (fact_id) DO NOTHING
                 RETURNING fact_id",
            )
            .bind(&payload)
            .fetch_all(&mut *tx)
            .await?;
            inserted += landed.len() as u32;

            let arrived: HashSet<Uuid> = landed.into_iter().collect();
            let current: Vec<Value> = facts
                .iter()
                .filter(|f| arrived.contains(&f.fact_id) && f.is_current.unwrap_or(true))
                .map(|f| {
                    serde_json::json!({
                        "fact_id": f.fact_id,
                        "subject": f.subject,
                        "predicate": f.predicate,
                        "observed_at": f.observed_at,
                    })
                })
                .collect();

            if !current.is_empty() {
                let claims = Value::Array(current);
                let closed = sqlx::query(
                    "UPDATE brain_facts f SET is_current = FALSE, valid_to = u.observed_at
                     FROM jsonb_to_recordset($1::jsonb) AS u(
                         fact_id uuid, subject text, predicate text, observed_at timestamptz)
                     WHERE f.subject = u.subject AND f.predicate = u.predicate
                       AND f.fact_id <> u.fact_id AND f.is_current
                       AND f.observed_at <= u.observed_at",
                )
                .bind(&claims)
                .execute(&mut *tx)
                .await?;
                superseded_facts += closed.rows_affected() as u32;

                let self_closed = sqlx::query(
                    "UPDATE brain_facts f SET is_current = FALSE, valid_to = u.observed_at
                     FROM jsonb_to_recordset($1::jsonb) AS u(
                         fact_id uuid, subject text, predicate text, observed_at timestamptz)
                     WHERE f.fact_id = u.fact_id AND f.is_current
                       AND EXISTS (SELECT 1 FROM brain_facts n
                                   WHERE n.subject = u.subject AND n.predicate = u.predicate
                                     AND n.fact_id <> u.fact_id AND n.is_current
                                     AND n.observed_at > u.observed_at)",
                )
                .bind(&claims)
                .execute(&mut *tx)
                .await?;
                superseded_facts += self_closed.rows_affected() as u32;
            }
        }
    }

    let artifacts_path = root.join("artifacts.json");
    if artifacts_path.exists() {
        let arts: Vec<ArtifactRow> = serde_json::from_slice(&std::fs::read(&artifacts_path)?)?;
        for a in arts {
            let local: Option<(i64, Option<String>, Option<i64>)> = sqlx::query_as(
                "SELECT version, crdt_actor, crdt_counter FROM brain_artifacts
                 WHERE path = $1 AND project_id IS NOT DISTINCT FROM $2",
            )
            .bind(&a.path)
            .bind(a.project_id)
            .fetch_optional(&mut *tx)
            .await?;
            let take = match local {
                None => true,
                Some((ver, actor, counter)) => {
                    let loc = crate::crdt::clock_from_row(actor.as_deref(), counter, "local");
                    let inc = crate::crdt::clock_from_row(
                        a.crdt_actor.as_deref(),
                        Some(a.crdt_counter),
                        "incoming",
                    );
                    match crate::crdt::pick_lww(&loc, &inc) {
                        crate::crdt::MergePick::TakeIncoming => true,
                        crate::crdt::MergePick::KeepLocal => false,
                        crate::crdt::MergePick::Concurrent => a.version > ver,
                    }
                }
            };
            if !take {
                continue;
            }
            sqlx::query(
                "INSERT INTO brain_artifacts
                    (id, project_id, path, content, content_hash, version, origin_node,
                     crdt_actor, crdt_counter, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 ON CONFLICT (project_id, path) DO UPDATE SET
                    content = EXCLUDED.content,
                    content_hash = EXCLUDED.content_hash,
                    version = EXCLUDED.version,
                    origin_node = EXCLUDED.origin_node,
                    crdt_actor = EXCLUDED.crdt_actor,
                    crdt_counter = EXCLUDED.crdt_counter,
                    updated_at = EXCLUDED.updated_at
                 WHERE brain_artifacts.version < EXCLUDED.version
                    OR (brain_artifacts.crdt_counter, brain_artifacts.crdt_actor)
                       < (EXCLUDED.crdt_counter, EXCLUDED.crdt_actor)",
            )
            .bind(a.id)
            .bind(a.project_id)
            .bind(&a.path)
            .bind(&a.content)
            .bind(&a.content_hash)
            .bind(a.version)
            .bind(a.origin_node.as_deref())
            .bind(a.crdt_actor.as_deref())
            .bind(a.crdt_counter)
            .bind(a.updated_at)
            .execute(&mut *tx)
            .await?;
            inserted += 1;
            crate::crdt::merge_event(
                "merged",
                serde_json::json!({"kind": "artifact", "path": a.path, "version": a.version}),
            );
        }
    }

    let procedures_path = root.join("procedures.json");
    if procedures_path.exists() {
        let procedures: Vec<ProcedureRow> =
            serde_json::from_slice(&std::fs::read(&procedures_path)?)?;
        let mut seen: HashSet<Uuid> = HashSet::new();
        let batch: Vec<Value> = procedures
            .iter()
            .filter(|p| !tombstones.buried.contains(&p.id) && seen.insert(p.id))
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "steps": p.steps,
                    "created_at": p.created_at,
                    "updated_at": p.updated_at,
                    "trigger_context": p.trigger_context,
                    "preconditions": p.preconditions,
                    "verification": p.verification,
                    "success_count": p.success_count,
                    "failure_count": p.failure_count,
                    "last_outcome": p.last_outcome,
                    "last_used_at": p.last_used_at,
                    "project_id": p.project_id,
                    "embedding_model": p.embedding_model,
                })
            })
            .collect();

        if !batch.is_empty() {
            let r = sqlx::query(&format!(
                "INSERT INTO brain_procedures
                    (id, name, steps, created_at, updated_at, trigger_context, preconditions,
                     verification, success_count, failure_count, last_outcome, last_used_at,
                     project_id, embedding_model)
                 SELECT u.id, u.name, u.steps, u.created_at, u.updated_at, u.trigger_context,
                        u.preconditions, u.verification, u.success_count, u.failure_count,
                        u.last_outcome, u.last_used_at,
                        (SELECT id FROM brain_projects WHERE id = u.project_id),
                        u.embedding_model
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     id uuid, name text, steps jsonb, created_at timestamptz,
                     updated_at timestamptz, trigger_context text, preconditions text,
                     verification text, success_count int, failure_count int,
                     last_outcome text, last_used_at timestamptz, project_id uuid,
                     embedding_model text)
                 ON CONFLICT (id) DO {}",
                if overwrite {
                    "UPDATE SET name = EXCLUDED.name, steps = EXCLUDED.steps, \
                     trigger_context = EXCLUDED.trigger_context, \
                     preconditions = EXCLUDED.preconditions, \
                     verification = EXCLUDED.verification, \
                     success_count = GREATEST(brain_procedures.success_count, \
                         EXCLUDED.success_count), \
                     failure_count = GREATEST(brain_procedures.failure_count, \
                         EXCLUDED.failure_count), \
                     updated_at = EXCLUDED.updated_at"
                } else {
                    "UPDATE SET \
                     success_count = GREATEST(brain_procedures.success_count, \
                         EXCLUDED.success_count), \
                     failure_count = GREATEST(brain_procedures.failure_count, \
                         EXCLUDED.failure_count)"
                }
            ))
            .bind(Value::Array(batch))
            .execute(&mut *tx)
            .await?;
            inserted += r.rows_affected() as u32;
        }
    }

    let trust_path = root.join("source_trust.json");
    if trust_path.exists() {
        let trust: Vec<SourceTrustRow> = serde_json::from_slice(&std::fs::read(&trust_path)?)?;
        let mut seen: HashSet<String> = HashSet::new();
        let batch: Vec<Value> = trust
            .iter()
            .filter(|t| seen.insert(t.source.clone()))
            .map(|t| serde_json::json!({"source": t.source, "alpha": t.alpha, "beta": t.beta}))
            .collect();

        if !batch.is_empty() {
            let r = sqlx::query(
                "INSERT INTO brain_source_trust (source, alpha, beta)
                 SELECT u.source, u.alpha, u.beta
                 FROM jsonb_to_recordset($1::jsonb) AS u(source text, alpha float8, beta float8)
                 ON CONFLICT (source) DO UPDATE SET
                     alpha = GREATEST(brain_source_trust.alpha, EXCLUDED.alpha),
                     beta = GREATEST(brain_source_trust.beta, EXCLUDED.beta),
                     updated_at = NOW()",
            )
            .bind(Value::Array(batch))
            .execute(&mut *tx)
            .await?;
            inserted += r.rows_affected() as u32;
        }
    }

    let relations_path = root.join("relations.json");
    if relations_path.exists() {
        let rels: Vec<RelationRow> = serde_json::from_slice(&std::fs::read(relations_path)?)?;
        let mut seen: HashSet<(Uuid, Uuid, String)> = HashSet::new();
        let mut batch: Vec<Value> = Vec::with_capacity(rels.len());
        for rel in &rels {
            let from = resolve(&remapped, rel.from_entity);
            let to = resolve(&remapped, rel.to_entity);
            if !seen.insert((from, to, rel.relation_type.clone())) {
                continue;
            }
            batch.push(serde_json::json!({
                "id": rel.id,
                "from_entity": from,
                "to_entity": to,
                "relation_type": rel.relation_type,
                "strength": rel.strength,
                "bidirectional": rel.bidirectional,
                "project_id": rel.project_id,
                "created_at": rel.created_at,
                "provenance": rel.provenance,
            }));
        }

        if !batch.is_empty() {
            let r = sqlx::query(&format!(
                "INSERT INTO brain_relations
                    (id, from_entity, to_entity, relation_type, strength,
                     bidirectional, project_id, created_at, provenance)
                 SELECT u.id, u.from_entity, u.to_entity, u.relation_type, u.strength,
                        u.bidirectional, u.project_id, u.created_at, u.provenance
                 FROM jsonb_to_recordset($1::jsonb) AS u(
                     id uuid, from_entity uuid, to_entity uuid, relation_type text,
                     strength float8, bidirectional boolean, project_id uuid,
                     created_at timestamptz, provenance text)
                 ON CONFLICT (from_entity, to_entity, relation_type) DO {}",
                if overwrite {
                    "UPDATE SET \
                     strength = GREATEST(brain_relations.strength, EXCLUDED.strength), \
                     bidirectional = EXCLUDED.bidirectional, project_id = EXCLUDED.project_id, \
                     created_at = EXCLUDED.created_at, provenance = EXCLUDED.provenance"
                } else {
                    "UPDATE SET \
                     strength = GREATEST(brain_relations.strength, EXCLUDED.strength)"
                }
            ))
            .bind(Value::Array(batch))
            .execute(&mut *tx)
            .await?;
            inserted += r.rows_affected() as u32;
        }
    }

    let mut embeddings_restored = 0u32;
    let blob_path = root.join("embeddings.bin.zst");
    if manifest.with_embeddings && blob_path.exists() {
        let compressed = std::fs::read(&blob_path)?;
        let raw = crate::sync::compressor::decompress(&compressed)?;
        let dim = manifest
            .embedding_dim
            .unwrap_or(crate::embeddings::onnx::embedding_dim());
        let rec_size = embedding_record_size(dim).filter(|size| raw.len() % size == 0);
        if let Some(rec_size) = rec_size {
            for chunk in raw.chunks_exact(rec_size) {
                let id_bytes: [u8; 16] = chunk[..16]
                    .try_into()
                    .expect("embedding_record_size rejects any dim whose record is not longer than the 16-byte uuid");
                let id = Uuid::from_bytes(id_bytes);
                let mut floats = Vec::with_capacity(dim);
                for f_chunk in chunk[16..].as_chunks::<4>().0 {
                    floats.push(f32::from_le_bytes(*f_chunk));
                }
                let v = pgvector::Vector::from(floats);
                let r = sqlx::query(
                    "UPDATE brain_observations
                     SET embedding = $1::vector, embedding_half = NULL
                     WHERE id = $2",
                )
                .bind(v)
                .bind(id)
                .execute(&mut *tx)
                .await?;
                embeddings_restored += r.rows_affected() as u32;
            }
        } else {
            tracing::warn!(
                "embeddings blob of {} bytes declares dim {} — skipping",
                raw.len(),
                dim
            );
        }
    }

    if tombstones.withheld.is_empty() {
        sqlx::query(
            "INSERT INTO brain_sync_state (manifest_hash, project_id, rows_inserted, source_path)
             VALUES ($1, $2, $3, $4) ON CONFLICT (manifest_hash) DO NOTHING",
        )
        .bind(&on_disk)
        .bind(manifest.project_id)
        .bind(inserted as i32)
        .bind(root.display().to_string())
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    let notices_closed = sqlx::query(
        "UPDATE brain_peer_notices SET resolved_at = NOW()
         WHERE resolved_at IS NULL AND manifest_hash = $1",
    )
    .bind(&on_disk)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);

    let quarantine_note = (quarantined > 0).then(|| {
        format!(
            "{quarantined} imported rows look like they carry a credential and were written \
             with trust=quarantined: observations and episodes are withheld from cuba_faro and \
             errors from cuba_expediente. Read them with cuba_eco action=pending and accept \
             them with cuba_eco action=promote"
        )
    });

    let divergence_note = (!diverged.is_empty()).then(|| {
        format!(
            "{} observations already existed here with different content and were LEFT \
             UNCHANGED. conflict=merge is not a merge: it inserts what is missing and keeps \
             whatever was here first, so a correction made on the other machine after this \
             row was created does not arrive. Until per-row causality lands, the choices are \
             conflict=overwrite — which takes the incoming version and discards this one — or \
             reconciling these by hand. Ids: {}",
            diverged.len(),
            diverged
                .iter()
                .take(20)
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    });

    let result = serde_json::json!({
        "action": "import",
        "manifest_hash": manifest.manifest_hash,
        "rows_inserted": inserted,
        "diverged": diverged.len(),
        "edited_since_export": tampered,
        "facts_superseded": superseded_facts,
        "tombstones_applied": tombstones.deleted,
        "tombstones_withheld": tombstones.withheld,
        "divergence_note": divergence_note,
        "embeddings_restored": embeddings_restored,
        "quarantined": quarantined,
        "quarantine_reasons": quarantine_reasons,
        "quarantine_note": quarantine_note,
        "peer_notices_closed": notices_closed,
        "scope_enforced": scoped,
        "scope_note": if scoped {
            Value::Null
        } else {
            serde_json::json!(
                "this bundle declares no project, so app.current_project stayed empty and the \
                 row-level policies treat that as no filter — measured, not assumed: with a \
                 project set an INSERT naming a different one is rejected, and with it empty \
                 the same INSERT lands. A scope=all bundle crosses projects by definition, so \
                 the database cannot be the guard here and the bundle's own project_id values \
                 are taken at face value."
            )
        },
        "from": root.display().to_string(),
    });

    crate::events::publish(
        "sync.import",
        serde_json::json!({
            "manifest_hash": manifest.manifest_hash,
            "rows_inserted": inserted,
            "diverged": diverged.len(),
            "quarantined": quarantined,
            "peer_notices_closed": notices_closed,
        }),
    );

    Ok(result)
}

async fn diff(pool: &PgPool, dir_arg: Option<&str>) -> Result<Value> {
    let root = resolve_dir(dir_arg)?;
    let project_id = crate::project::current_project_id(pool).await?;

    let mut on_disk: HashMap<Uuid, String> = HashMap::new();
    let entities_dir = root.join("entities");
    if entities_dir.exists() {
        for entry in std::fs::read_dir(&entities_dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let f: EntityFile = serde_json::from_slice(&std::fs::read(&path)?)?;
            on_disk.insert(f.id, f.name);
        }
    }

    let db_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM brain_entities
         WHERE ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let in_db: HashMap<Uuid, String> = db_rows.into_iter().collect();

    let only_disk: Vec<Value> = on_disk
        .iter()
        .filter(|(id, _)| !in_db.contains_key(id))
        .map(|(id, name)| serde_json::json!({"id": id.to_string(), "name": name}))
        .collect();
    let only_db: Vec<Value> = in_db
        .iter()
        .filter(|(id, _)| !on_disk.contains_key(id))
        .map(|(id, name)| serde_json::json!({"id": id.to_string(), "name": name}))
        .collect();

    Ok(serde_json::json!({
        "action": "diff",
        "dir": root.display().to_string(),
        "only_in_disk": only_disk,
        "only_in_db": only_db,
        "common_count": on_disk.len() - only_disk.len(),
    }))
}

async fn status(pool: &PgPool, dir_arg: Option<&str>) -> Result<Value> {
    let root = resolve_dir(dir_arg)?;
    let manifest_path = root.join("manifest.json");

    let mut on_disk: Option<Manifest> = None;
    if manifest_path.exists() {
        on_disk = serde_json::from_slice(&std::fs::read(&manifest_path)?).ok();
    }

    let imported: Vec<(String, Option<Uuid>, chrono::DateTime<Utc>, i32)> = sqlx::query_as(
        "SELECT manifest_hash, project_id, imported_at, rows_inserted
         FROM brain_sync_state ORDER BY imported_at DESC LIMIT 20",
    )
    .fetch_all(pool)
    .await?;

    let imported_json: Vec<Value> = imported
        .iter()
        .map(|(h, p, ts, n)| {
            serde_json::json!({
                "manifest_hash": h,
                "project_id": p.map(|p| p.to_string()),
                "imported_at": ts.to_rfc3339(),
                "rows_inserted": n,
            })
        })
        .collect();

    let pending = match &on_disk {
        Some(m) => {
            let already: Option<(i32,)> = sqlx::query_as(
                "SELECT rows_inserted FROM brain_sync_state WHERE manifest_hash = $1",
            )
            .bind(&m.manifest_hash)
            .fetch_optional(pool)
            .await?;
            already.is_none()
        }
        None => false,
    };

    let notices = pending_notices(pool).await.unwrap_or_default();

    Ok(serde_json::json!({
        "action": "status",
        "dir": root.display().to_string(),
        "current_manifest": on_disk,
        "pending_import": pending,
        "recent_imports": imported_json,
        "peer_notices": notices,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cuba-sync-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("the test needs a scratch directory");
        dir
    }

    fn write_json(dir: &Path, name: &str, body: Value) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&body).expect("a Value serialises"),
        )
        .expect("writing the fixture");
        path
    }

    #[test]
    fn a_project_scoped_export_never_prunes_a_file_owned_by_another_project() {
        let dir = scratch("prune-other-project");
        let mine = Uuid::new_v4();
        let theirs = Uuid::new_v4();

        let kept = write_json(
            &dir,
            "mine-kept.json",
            serde_json::json!({"project_id": mine}),
        );
        let stale = write_json(
            &dir,
            "mine-stale.json",
            serde_json::json!({"project_id": mine}),
        );
        let other = write_json(
            &dir,
            "theirs.json",
            serde_json::json!({"project_id": theirs}),
        );
        let global = write_json(&dir, "global.json", serde_json::json!({"project_id": null}));

        let keep: HashSet<PathBuf> = [kept.clone()].into_iter().collect();
        prune_stale_files(&dir, &keep, PruneScope::Project(mine)).expect("pruning must not fail");

        assert!(kept.exists(), "a file this export just wrote must survive");
        assert!(
            !stale.exists(),
            "a file of the exported project that the export no longer produced is genuinely \
             stale and pruning it is the whole point of the function"
        );
        assert!(
            other.exists(),
            "scope=project is the DEFAULT of cuba_sync export, and the keep set only ever holds \
             this project's files: deleting another project's export left the user with nothing \
             but whatever git happened to have committed"
        );
        assert!(
            global.exists(),
            "an entity with project_id NULL is shared by every project; a single project's \
             export has no standing to delete it"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_full_export_still_owns_and_prunes_the_whole_directory() {
        let dir = scratch("prune-everything");
        let kept = write_json(&dir, "kept.json", serde_json::json!({"project_id": null}));
        let stale = write_json(
            &dir,
            "stale.json",
            serde_json::json!({"project_id": Uuid::new_v4()}),
        );
        let opaque = dir.join("opaque.json");
        std::fs::write(&opaque, b"not json at all").expect("writing the fixture");

        let keep: HashSet<PathBuf> = [kept.clone()].into_iter().collect();
        prune_stale_files(&dir, &keep, PruneScope::Everything).expect("pruning must not fail");

        assert!(kept.exists());
        assert!(
            !stale.exists() && !opaque.exists(),
            "scope=all exports every row there is, so anything left over really was deleted from \
             the graph — narrowing this case would turn the export directory into an attic"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_that_does_not_declare_its_project_is_left_alone_by_a_project_export() {
        let dir = scratch("prune-unattributable");
        let legacy = write_json(&dir, "legacy.json", serde_json::json!({"id": "x"}));
        let corrupt = dir.join("corrupt.json");
        std::fs::write(&corrupt, b"{ truncated").expect("writing the fixture");

        prune_stale_files(&dir, &HashSet::new(), PruneScope::Project(Uuid::new_v4()))
            .expect("pruning must not fail");

        assert!(
            legacy.exists() && corrupt.exists(),
            "deletion needs positive evidence of ownership: a file written by an older version, \
             or one the user is mid-edit on, must not be destroyed because it failed to parse"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn episode_pruning_is_scoped_the_same_way_as_every_other_directory() {
        let dir = scratch("prune-episodes");
        let month = dir.join("2026-08");
        std::fs::create_dir_all(&month).expect("the test needs the month directory");
        let mine = Uuid::new_v4();
        let stale = write_json(&month, "mine.json", serde_json::json!({"project_id": mine}));
        let other = write_json(
            &month,
            "theirs.json",
            serde_json::json!({"project_id": Uuid::new_v4()}),
        );

        prune_stale_episode_files(&dir, &HashSet::new(), PruneScope::Project(mine))
            .expect("pruning must not fail");

        assert!(!stale.exists());
        assert!(
            other.exists(),
            "episodes live one directory deeper but the ownership rule is identical: a project \
             export deletes only what it produced"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_bundles_with_the_same_file_names_but_different_content_hash_differently() {
        let root = Path::new("/bundle");
        let project = Some(Uuid::new_v4());

        let mut before = BundleDigest::default();
        before.record(
            root,
            &root.join("entities/a.json"),
            b"{\"content\":\"original\"}",
        );
        before.record(root, &root.join("relations.json"), b"[]");

        let mut after = BundleDigest::default();
        after.record(
            root,
            &root.join("entities/a.json"),
            b"{\"content\":\"edited\"}",
        );
        after.record(root, &root.join("relations.json"), b"[]");

        assert_ne!(
            before.finish(project),
            after.finish(project),
            "the hash used to cover only the row COUNTS, so hand-editing a .json — the \
             git-friendly workflow the README sells — kept the same hash and the import on the \
             other machine answered skipped:true and discarded the edit in silence"
        );
    }

    #[test]
    fn the_bundle_hash_does_not_depend_on_the_order_the_files_were_written() {
        let root = Path::new("/bundle");
        let project = Some(Uuid::new_v4());

        let mut one = BundleDigest::default();
        one.record(root, &root.join("entities/a.json"), b"a");
        one.record(root, &root.join("entities/b.json"), b"b");

        let mut other = BundleDigest::default();
        other.record(root, &root.join("entities/b.json"), b"b");
        other.record(root, &root.join("entities/a.json"), b"a");

        assert_eq!(
            one.finish(project),
            other.finish(project),
            "row order comes out of Postgres and directory order comes out of the filesystem: if \
             either leaked into the hash, the same bundle would look new on every machine and \
             the already-imported check would never fire"
        );
    }

    #[test]
    fn the_bundle_hash_still_separates_a_project_export_from_a_full_one() {
        let root = Path::new("/bundle");
        let mut project_scoped = BundleDigest::default();
        project_scoped.record(root, &root.join("projects.json"), b"[]");
        let mut full = BundleDigest::default();
        full.record(root, &root.join("projects.json"), b"[]");

        assert_ne!(
            project_scoped.finish(Some(Uuid::new_v4())),
            full.finish(None),
            "two exports of identical files still describe different slices of the graph"
        );
    }

    #[test]
    fn a_hostile_embedding_dim_can_never_produce_a_record_shorter_than_the_uuid_it_starts_with() {
        const HOSTILE: usize = 4611686018427387903;

        assert_eq!(
            16usize.wrapping_add(HOSTILE.wrapping_mul(4)),
            12,
            "this is the arithmetic the import used to do on a manifest.json that arrives \
             through a shared git repo: a 12-byte record passed the `rec_size == 16` guard, and \
             then chunk[..16] panicked on a 12-byte slice"
        );
        assert_eq!(embedding_record_size(HOSTILE), None);
        assert_eq!(embedding_record_size(usize::MAX), None);
        assert_eq!(
            embedding_record_size(0),
            None,
            "dim 0 is the case the old `rec_size == 16` guard did catch, and it must stay caught"
        );
        assert_eq!(
            embedding_record_size(MAX_EMBEDDING_DIM + 1),
            None,
            "pgvector cannot store more than {MAX_EMBEDDING_DIM} dimensions, so a larger dim is \
             never a real export"
        );

        assert_eq!(embedding_record_size(384), Some(1552));
        assert_eq!(embedding_record_size(1024), Some(4112));
        for dim in [1, 2, 384, 1024, MAX_EMBEDDING_DIM] {
            let size = embedding_record_size(dim).expect("a real dimension is accepted");
            assert!(
                size > 16,
                "the expect() in the import reads chunk[..16] and states that records are longer \
                 than a uuid; dim {dim} produced {size}"
            );
        }
    }

    #[test]
    fn an_imported_observation_that_carries_a_credential_is_quarantined_instead_of_dropped() {
        let (trust, reason) = trust_for_imported("el deploy usa ghp_abcdefghijklmnop", None);
        assert_eq!(
            trust,
            crate::core::trust::QUARANTINED,
            "cuba_sync import reads JSON out of a repository anyone with push can write to, and \
             a git pull followed by an import puts it in the graph with nobody reading it"
        );
        assert_eq!(
            reason,
            Some("github token"),
            "the import has to report WHY a row was held back, or the user cannot judge whether \
             to promote it"
        );

        let (trust, reason) = trust_for_imported("el bug era que la password no se validaba", None);
        assert_eq!(
            trust,
            crate::core::trust::TRUSTED,
            "prose about credentials is not a credential; quarantining it would hide the ordinary \
             content of an import behind a review queue"
        );
        assert_eq!(reason, None);
    }
}

#[cfg(test)]
mod quarantine_travels_tests {
    use super::trust_for_imported;

    #[test]
    fn a_quarantined_observation_stays_quarantined_on_the_other_machine() {
        let (trust, reason) = trust_for_imported(
            "granian depende de uvicorn segun el modelo",
            Some(crate::core::trust::QUARANTINED),
        );
        assert_eq!(
            trust,
            crate::core::trust::QUARANTINED,
            "the bundle carries the trust the writing machine assigned, and the import used to \
             throw it away and recompute from the text alone: anything that did not look like a \
             credential landed trusted. What the REM cycle extracts is quarantined precisely \
             because a model wrote it, not because it looks dangerous — so one sync turned every \
             hallucination on machine A into searchable memory on machine B. Reason: {reason:?}"
        );
    }

    #[test]
    fn a_trusted_observation_that_carries_a_secret_is_still_caught() {
        let (trust, reason) = trust_for_imported(
            "el deploy usa ghp_abcdefghijklmnop",
            Some(crate::core::trust::TRUSTED),
        );
        assert_eq!(
            trust,
            crate::core::trust::QUARANTINED,
            "respecting the declared trust must not weaken the credential check: the result is \
             the stricter of the two, never the one the sender claims"
        );
        assert!(reason.is_some(), "and the pattern is named");
    }
}
