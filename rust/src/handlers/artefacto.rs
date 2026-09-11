use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

const DEFAULT_LOCK_SECS: i64 = 60;
const MAX_CONTENT_BYTES: usize = 2 * 1024 * 1024;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "list" => list(pool, &args).await,
        "get" => get(pool, &args).await,
        "put" => put(pool, &args).await,
        "patch" => patch(pool, &args).await,
        "lock" => lock(pool, &args).await,
        "unlock" => unlock(pool, &args).await,
        "watch_hint" => Ok(serde_json::json!({
            "action": "watch_hint",
            "events_url": "/events",
            "kinds": [
                "artifact.updated",
                "artifact.locked",
                "artifact.unlocked",
                "artifact.conflict",
                "peer.notice",
                "sync.import",
                "sync.fetch",
                "sync.conflict_resolved",
                "crdt.*"
            ]
        })),
        _ => {
            anyhow::bail!("Invalid action: {action}. Use list/get/put/patch/lock/unlock/watch_hint")
        }
    }
}

fn actor() -> String {
    crate::session::current_client().unwrap_or_else(|| "anonymous".into())
}

fn content_hash(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    hex::encode(h.finalize())
}

async fn list(pool: &PgPool, args: &Value) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;
    let prefix = args.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(100)
        .clamp(1, 500);

    type Row = (
        Uuid,
        String,
        String,
        i64,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
        chrono::DateTime<chrono::Utc>,
        Option<String>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, path, content_hash, version, locked_by, lock_until, updated_at, origin_node
         FROM brain_artifacts
         WHERE (
                ($1::uuid IS NULL AND project_id IS NULL)
             OR ($1::uuid IS NOT NULL AND (project_id IS NOT DISTINCT FROM $1 OR project_id IS NULL))
           )
           AND ($2 = '' OR path LIKE $2 || '%')
         ORDER BY path
         LIMIT $3",
    )
    .bind(project_id)
    .bind(prefix)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("list artifacts")?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, path, hash, version, locked_by, lock_until, updated_at, origin_node)| {
                serde_json::json!({
                    "id": id.to_string(),
                    "path": path,
                    "content_hash": hash,
                    "version": version,
                    "locked_by": locked_by,
                    "lock_until": lock_until.map(|t| t.to_rfc3339()),
                    "updated_at": updated_at.to_rfc3339(),
                    "origin_node": origin_node,
                })
            },
        )
        .collect();

    Ok(serde_json::json!({ "action": "list", "artifacts": items, "count": items.len() }))
}

async fn get(pool: &PgPool, args: &Value) -> Result<Value> {
    let path = require_path(args)?;
    let project_id = crate::project::current_project_id(pool).await?;

    type Row = (
        Uuid,
        String,
        String,
        String,
        i64,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
        chrono::DateTime<chrono::Utc>,
        Option<String>,
    );
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, path, content, content_hash, version, locked_by, lock_until, updated_at, origin_node
         FROM brain_artifacts
         WHERE path = $1
           AND (
                ($2::uuid IS NULL AND project_id IS NULL)
             OR ($2::uuid IS NOT NULL AND (project_id IS NOT DISTINCT FROM $2 OR project_id IS NULL))
           )
         ORDER BY updated_at DESC
         LIMIT 1",
    )
    .bind(path)
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .context("get artifact")?;

    let Some((id, path, content, hash, version, locked_by, lock_until, updated_at, origin_node)) =
        row
    else {
        anyhow::bail!("artifact not found: {path}");
    };

    Ok(serde_json::json!({
        "action": "get",
        "id": id.to_string(),
        "path": path,
        "content": content,
        "content_hash": hash,
        "version": version,
        "locked_by": locked_by,
        "lock_until": lock_until.map(|t| t.to_rfc3339()),
        "updated_at": updated_at.to_rfc3339(),
        "origin_node": origin_node,
    }))
}

async fn put(pool: &PgPool, args: &Value) -> Result<Value> {
    let path = require_path(args)?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("content is required"))?;
    if content.len() > MAX_CONTENT_BYTES {
        anyhow::bail!("content exceeds {MAX_CONTENT_BYTES} bytes");
    }
    crate::redact::refuse_secrets(args, "content", content)?;

    let base_version = args.get("base_version").and_then(|v| v.as_i64());
    let project_id = crate::project::current_project_id(pool).await?;
    let who = actor();
    let hash = content_hash(content);
    let node = crate::db::node_id(pool).await.ok().map(|id| id.to_string());

    let mut tx = pool.begin().await?;

    type ArtifactLockRow = (
        Uuid,
        i64,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
    );
    let existing: Option<ArtifactLockRow> = sqlx::query_as(
        "SELECT id, version, locked_by, lock_until FROM brain_artifacts
             WHERE path = $1
               AND ($2::uuid IS NULL OR project_id IS NOT DISTINCT FROM $2)
             FOR UPDATE",
    )
    .bind(path)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some((id, version, locked_by, lock_until)) = existing {
        if let (Some(by), Some(until)) = (&locked_by, lock_until)
            && *by != who
            && until > chrono::Utc::now()
        {
            anyhow::bail!("artifact locked by {by} until {}", until.to_rfc3339());
        }
        let base = base_version.ok_or_else(|| {
            anyhow::anyhow!(
                "base_version is required to update {path} (current version is {version})"
            )
        })?;
        if base != version {
            let current: (String, i64, String) = sqlx::query_as(
                "SELECT content, version, content_hash FROM brain_artifacts WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            crate::events::publish(
                "artifact.conflict",
                serde_json::json!({
                    "path": path,
                    "base_version": base,
                    "current_version": current.1,
                    "locked_by": locked_by,
                }),
            );
            anyhow::bail!(
                "version conflict on {path}: base_version={base} current_version={} \
                 content_hash={} (refetch with get and retry)",
                current.1,
                current.2
            );
        }

        let row: (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
            "UPDATE brain_artifacts
             SET content = $2, content_hash = $3, version = version + 1,
                 updated_at = NOW(), origin_node = $4, crdt_actor = $5,
                 crdt_counter = crdt_counter + 1
             WHERE id = $1
             RETURNING version, updated_at",
        )
        .bind(id)
        .bind(content)
        .bind(&hash)
        .bind(node.as_deref())
        .bind(&who)
        .fetch_one(&mut *tx)
        .await
        .context("update artifact")?;

        tx.commit().await?;
        crate::events::publish(
            "artifact.updated",
            serde_json::json!({
                "path": path,
                "version": row.0,
                "content_hash": hash,
                "by": who,
            }),
        );
        let _ = crate::graph_db::project_artifact(path, row.0).await;

        return Ok(serde_json::json!({
            "action": "put",
            "id": id.to_string(),
            "path": path,
            "version": row.0,
            "content_hash": hash,
            "updated_at": row.1.to_rfc3339(),
        }));
    }

    if base_version.is_some_and(|v| v != 0) {
        anyhow::bail!("artifact not found for base_version; create with base_version=0 or omit");
    }

    let row: (Uuid, i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "INSERT INTO brain_artifacts
            (project_id, path, content, content_hash, version, origin_node, crdt_actor, crdt_counter)
         VALUES ($1, $2, $3, $4, 1, $5, $6, 1)
         RETURNING id, version, updated_at",
    )
    .bind(project_id)
    .bind(path)
    .bind(content)
    .bind(&hash)
    .bind(node.as_deref())
    .bind(&who)
    .fetch_one(&mut *tx)
    .await
    .context("insert artifact")?;

    tx.commit().await?;
    crate::events::publish(
        "artifact.updated",
        serde_json::json!({
            "path": path,
            "version": row.1,
            "content_hash": hash,
            "by": who,
        }),
    );
    let _ = crate::graph_db::project_artifact(path, row.1).await;

    Ok(serde_json::json!({
        "action": "put",
        "id": row.0.to_string(),
        "path": path,
        "version": row.1,
        "content_hash": hash,
        "updated_at": row.2.to_rfc3339(),
    }))
}

async fn patch(pool: &PgPool, args: &Value) -> Result<Value> {
    let path = require_path(args)?;
    let project_id = crate::project::current_project_id(pool).await?;
    let existing: Option<(String, i64)> = sqlx::query_as(
        "SELECT content, version FROM brain_artifacts
         WHERE path = $1
           AND (
                ($2::uuid IS NULL AND project_id IS NULL)
             OR ($2::uuid IS NOT NULL AND (project_id IS NOT DISTINCT FROM $2 OR project_id IS NULL))
           )
         LIMIT 1",
    )
    .bind(path)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;

    let Some((content, version)) = existing else {
        anyhow::bail!("artifact not found: {path}");
    };

    let find = args
        .get("find")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("find is required for patch"))?;
    let replace = args
        .get("replace")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("replace is required for patch"))?;

    let patched = crate::crdt::ot_line_patch(&content, find, replace).map_err(|c| {
        anyhow::anyhow!(
            "OT patch refused on {path}: {} (base_len={}, find_len={})",
            c.reason,
            c.base_len,
            c.find.len()
        )
    })?;
    let mut put_args = args.clone();
    if let Some(obj) = put_args.as_object_mut() {
        obj.insert("content".into(), Value::String(patched));
        obj.insert("base_version".into(), Value::from(version));
    }
    put(pool, &put_args).await
}

async fn lock(pool: &PgPool, args: &Value) -> Result<Value> {
    let path = require_path(args)?;
    let secs = args
        .get("ttl_seconds")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_LOCK_SECS)
        .clamp(5, 3600);
    let who = actor();
    let project_id = crate::project::current_project_id(pool).await?;

    let mut tx = pool.begin().await?;

    let row: Option<(Option<String>, Option<chrono::DateTime<chrono::Utc>>, i64)> = sqlx::query_as(
        "SELECT locked_by, lock_until, version FROM brain_artifacts
             WHERE path = $1
               AND ($2::uuid IS NULL OR project_id IS NOT DISTINCT FROM $2)
             FOR UPDATE",
    )
    .bind(path)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((locked_by, lock_until, version)) = row else {
        anyhow::bail!("artifact not found: {path}");
    };
    if let (Some(by), Some(until)) = (&locked_by, lock_until)
        && *by != who
        && until > chrono::Utc::now()
    {
        anyhow::bail!("already locked by {by} until {}", until.to_rfc3339());
    }

    let until: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "UPDATE brain_artifacts
         SET locked_by = $2, lock_until = NOW() + make_interval(secs => $3::double precision)
         WHERE path = $1
           AND ($4::uuid IS NULL OR project_id IS NOT DISTINCT FROM $4)
         RETURNING lock_until",
    )
    .bind(path)
    .bind(&who)
    .bind(secs as f64)
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await
    .context("lock artifact")?;

    tx.commit().await?;

    crate::events::publish(
        "artifact.locked",
        serde_json::json!({ "path": path, "locked_by": who, "lock_until": until.to_rfc3339() }),
    );

    Ok(serde_json::json!({
        "action": "lock",
        "path": path,
        "version": version,
        "locked_by": who,
        "lock_until": until.to_rfc3339(),
        "ttl_seconds": secs,
    }))
}

async fn unlock(pool: &PgPool, args: &Value) -> Result<Value> {
    let path = require_path(args)?;
    let who = actor();
    let project_id = crate::project::current_project_id(pool).await?;

    let mut tx = pool.begin().await?;

    let row: Option<(Option<String>, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT locked_by, lock_until FROM brain_artifacts
         WHERE path = $1
           AND ($2::uuid IS NULL OR project_id IS NOT DISTINCT FROM $2)
         FOR UPDATE",
    )
    .bind(path)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((locked_by, lock_until)) = row else {
        anyhow::bail!("artifact not found: {path}");
    };
    if let (Some(by), Some(until)) = (&locked_by, lock_until)
        && *by != who
        && until > chrono::Utc::now()
    {
        anyhow::bail!("locked by {by}; only they (or wait until expiry) can unlock");
    }

    sqlx::query(
        "UPDATE brain_artifacts SET locked_by = NULL, lock_until = NULL
         WHERE path = $1
           AND ($2::uuid IS NULL OR project_id IS NOT DISTINCT FROM $2)",
    )
    .bind(path)
    .bind(project_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    crate::events::publish(
        "artifact.unlocked",
        serde_json::json!({ "path": path, "by": who }),
    );

    Ok(serde_json::json!({ "action": "unlock", "path": path }))
}

fn require_path(args: &Value) -> Result<&str> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("path is required"))?;
    if path.contains('\0') || path.len() > 512 {
        anyhow::bail!("invalid path");
    }
    Ok(path)
}
