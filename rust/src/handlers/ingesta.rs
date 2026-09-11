use anyhow::{Context, Result};
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("ingest");

    match action {
        "ingest" | "" => ingest(pool, &args).await,
        "parse" => parse(pool, &args).await,
        "auto_extract" => auto_extract(pool, &args).await,
        _ => anyhow::bail!("Invalid action: {action}. Use ingest/parse/auto_extract"),
    }
}

fn allow_secret(args: &Value) -> bool {
    args.get("allow_secret").and_then(Value::as_bool) == Some(true)
}

async fn auto_extract(pool: &PgPool, args: &Value) -> Result<Value> {
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        anyhow::bail!("text is required for auto_extract action");
    }

    crate::redact::refuse_secrets(args, "text", text)?;

    let hint = args
        .get("entity_hint")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = build_extraction_prompt(text, hint);

    let budget = args
        .get("budget_secs")
        .and_then(Value::as_u64)
        .filter(|&s| s > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or_else(extraction_budget);

    let (reply, backend) = match extraction_reply_within(&prompt, budget).await {
        Ok(pair) => pair,
        Err(why) => {
            return Ok(serde_json::json!({
                "action": "auto_extract",
                "extracted": 0,
                "added": 0,
                "relations_linked": 0,
                "degraded": true,
                "reason": match why {
                    NoExtraction::NoBackend => "no_backend",
                    NoExtraction::Failed(_) => "backend_failed",
                    NoExtraction::OutOfBudget(_, _) => "out_of_budget",
                },
                "note": why.note()
            }));
        }
    };

    let (items, relations) = parse_extraction_reply(&reply);
    let extracted = items.len();
    if extracted == 0 && relations.is_empty() {
        return Ok(serde_json::json!({
            "action": "auto_extract",
            "extracted": 0,
            "added": 0,
            "relations_linked": 0,
            "backend": backend,
            "note": "the model returned no durable facts worth remembering from this text"
        }));
    }

    let untrusted = args
        .get("untrusted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let relations_linked = if untrusted {
        0
    } else {
        link_entities(pool, &relations).await
    };

    if extracted == 0 {
        return Ok(serde_json::json!({
            "action": "auto_extract",
            "extracted": 0,
            "added": 0,
            "relations_linked": relations_linked,
            "backend": backend,
            "note": if untrusted && !relations.is_empty() {
                "no durable facts, and the relations the text implied were not written: this \
                 extraction is unattended and quarantined, and neither brain_entities nor \
                 brain_relations has a way to mark a row as unreviewed"
            } else {
                "no durable facts, but the text did support graph relations"
            }
        }));
    }

    let supersede_conflicts = args
        .get("supersede_conflicts")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ops = if supersede_conflicts {
        resolve_conflicts(pool, &items).await
    } else {
        crate::cognitive::memory_op::OpBreakdown::default()
    };

    let items = if untrusted {
        items
            .into_iter()
            .map(|mut i| {
                i["trust"] = serde_json::json!(crate::core::trust::QUARANTINED);
                i
            })
            .collect()
    } else {
        items
    };

    let ingest_args = serde_json::json!({
        "action": "ingest",
        "items": items,
        "allow_secret": allow_secret(args)
    });
    let result = ingest(pool, &ingest_args).await?;

    let mut response = result;
    if let Some(obj) = response.as_object_mut() {
        obj.insert("action".to_string(), serde_json::json!("auto_extract"));
        obj.insert("extracted".to_string(), serde_json::json!(extracted));
        obj.insert(
            "relations_linked".to_string(),
            serde_json::json!(relations_linked),
        );
        obj.insert("backend".to_string(), serde_json::json!(backend));
        if supersede_conflicts {
            obj.insert("superseded".to_string(), serde_json::json!(ops.update));
            obj.insert("operations".to_string(), ops.to_json());
        }
    }
    Ok(response)
}

const EXTRACTION_MAX_TOKENS: u32 = 1024;
const EXTRACTION_BUDGET_RATIO: f32 = 0.6;

pub fn extraction_budget() -> std::time::Duration {
    crate::protocol::handler_timeout().mul_f32(EXTRACTION_BUDGET_RATIO)
}

const RELATION_SCAN_DEFAULT_TIMEOUT_SECS: u64 = 90;

pub fn relation_scan_budget() -> std::time::Duration {
    let secs = std::env::var("CUBA_REM_SCAN_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&s| s > 0)
        .unwrap_or(RELATION_SCAN_DEFAULT_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

pub enum NoExtraction {
    NoBackend,
    Failed(&'static str),
    OutOfBudget(&'static str, u64),
}

impl NoExtraction {
    fn note(&self) -> String {
        match self {
            Self::NoBackend => "no LLM reachable: the client advertises no MCP sampling \
                 capability and no local CLI was found on PATH. Install the Claude Code CLI \
                 (or set CUBA_JUEZ_CLI), or use action='parse' for a heuristic paragraph split."
                .to_string(),
            Self::Failed(backend) => format!(
                "the {backend} backend was found and reachable, but the call failed. This is \
                 not a missing CLI — installing one will not help. The error is in the log for \
                 this request; action='parse' still works as a heuristic split."
            ),
            Self::OutOfBudget(backend, secs) => format!(
                "the {backend} backend answered too slowly and ran past its {secs}s budget. \
                 Nothing is wrong with the install: the model was slow for this text. Retry, \
                 shorten the text, raise the handler timeout, or use action='parse'."
            ),
        }
    }
}

async fn extraction_reply_within(
    prompt: &str,
    budget: std::time::Duration,
) -> Result<(String, &'static str), NoExtraction> {
    if crate::protocol::client_supports_sampling() {
        match crate::protocol::request_sampling_max(prompt, EXTRACTION_MAX_TOKENS).await {
            Ok(reply) => return Ok((reply, "mcp_sampling")),
            Err(why) => {
                tracing::warn!(error = %why, "MCP sampling failed, falling back to a local LLM CLI")
            }
        }
    }

    let Some(backend) = crate::cognitive::judge::resolve_offline_llm_within(Some(budget)) else {
        return Err(NoExtraction::NoBackend);
    };
    let name = backend.backend_name();
    match tokio::time::timeout(budget, backend.run_prompt(prompt)).await {
        Ok(Ok(raw)) => Ok((crate::cognitive::judge::unwrap_cli_reply(&raw), name)),
        Ok(Err(why)) => {
            tracing::warn!(error = %why, backend = name, "LLM extraction failed");
            Err(NoExtraction::Failed(name))
        }
        Err(_) => {
            tracing::warn!(
                backend = name,
                budget_secs = budget.as_secs(),
                "LLM extraction ran out of its time budget"
            );
            Err(NoExtraction::OutOfBudget(name, budget.as_secs()))
        }
    }
}

async fn resolve_conflicts(
    pool: &PgPool,
    items: &[Value],
) -> crate::cognitive::memory_op::OpBreakdown {
    use crate::cognitive::memory_op::{MemoryOp, OpBreakdown};

    const REL_LO: f64 = 0.30;
    const REL_HI: f64 = 0.85;
    const CONF_FLOOR: f64 = 0.5;

    let judge = crate::cognitive::judge::resolve_judge();
    let mut ops = OpBreakdown::default();

    for item in items {
        let (Some(entity_name), Some(content)) = (
            item.get("entity_name").and_then(|v| v.as_str()),
            item.get("content").and_then(|v| v.as_str()),
        ) else {
            continue;
        };

        let candidate: Option<(uuid::Uuid, String, f64)> = sqlx::query_as(
            "SELECT o.id, o.content, similarity(o.content, $2)::float8 AS sim
             FROM brain_observations o
             JOIN brain_entities e ON e.id = o.entity_id
             WHERE e.name = $1 AND o.observation_type != 'superseded'
             ORDER BY sim DESC
             LIMIT 1",
        )
        .bind(entity_name)
        .bind(content)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

        let Some((old_id, old_content, sim)) = candidate else {
            ops.record(MemoryOp::Add);
            continue;
        };
        if !(REL_LO..REL_HI).contains(&sim) {
            ops.record(MemoryOp::Add);
            continue;
        }

        let op = match judge.judge(content, &old_content).await {
            Ok(j) => MemoryOp::from_judgment(&j.verdict, j.confidence, CONF_FLOOR),
            Err(e) => {
                tracing::warn!(error = %e, "auto_extract: judge failed — treating as NOOP");
                MemoryOp::Noop
            }
        };

        if op.supersedes_old() {
            let done = sqlx::query(
                "UPDATE brain_observations SET observation_type = 'superseded', updated_at = NOW()
                 WHERE id = $1 AND observation_type != 'superseded'",
            )
            .bind(old_id)
            .execute(pool)
            .await;
            match done {
                Ok(r) if r.rows_affected() > 0 => {
                    tracing::info!(old_id = %old_id, op = op.as_str(), "auto_extract superseded a stale observation");
                }
                Ok(_) => {
                    ops.record(MemoryOp::Noop);
                    continue;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "auto_extract: supersede failed — treating as NOOP");
                    ops.record(MemoryOp::Noop);
                    continue;
                }
            }
        }
        ops.record(op);
    }
    ops
}

fn build_extraction_prompt(text: &str, hint: &str) -> String {
    let hint_line = if hint.is_empty() {
        String::new()
    } else {
        format!("\nThe main subject is likely: \"{hint}\". Prefer it as entity_name when it fits.")
    };
    let rel_types = crate::constants::VALID_RELATION_TYPES.join(", ");
    format!(
        "You extract durable, reusable memories from an AI coding agent's work log.\n\
         From the text below, extract two things:\n\n\
         1. FACTS worth remembering across sessions — decisions made, lessons learned, \
         errors and their fixes, stable preferences, key technical facts. Ignore chit-chat, \
         transient state, and anything that will not matter next week.\n\
         2. RELATIONS between the entities those facts are about — how they connect. \
         Only state a relation the text actually supports; do not invent plausible-sounding \
         links. Both endpoints should be things the text genuinely talks about.{hint_line}\n\n\
         Return STRICT JSON, exactly this shape:\n\
         {{\"facts\": [{{\"entity_name\": <the project/technology/concept the fact is about>, \
         \"content\": <one self-contained sentence>, \
         \"observation_type\": one of [fact, decision, lesson, preference, error, solution, context, tool_usage]}}],\n\
         \x20\"relations\": [{{\"from\": <entity name>, \"to\": <entity name>, \
         \"relation_type\": one of [{rel_types}]}}]}}\n\n\
         Use [] for either list when there is nothing worth recording. \
         No prose, no markdown — just the JSON object.\n\n\
         TEXT:\n{text}"
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedRelation {
    pub from: String,
    pub to: String,
    pub relation_type: String,
}

fn parse_extraction_reply(reply: &str) -> (Vec<Value>, Vec<ExtractedRelation>) {
    if let Some(obj) = extract_json_object(reply) {
        let facts = obj.get("facts").map(items_from_array).unwrap_or_default();
        let relations = obj
            .get("relations")
            .map(relations_from_array)
            .unwrap_or_default();
        return (facts, relations);
    }
    (parse_extracted_items(reply), Vec::new())
}

fn extract_json_object(reply: &str) -> Option<Value> {
    let open = reply.find('{')?;
    if reply.find('[').is_some_and(|bracket| bracket < open) {
        return None;
    }
    let close = reply.rfind('}')?;
    if close <= open {
        return None;
    }
    let parsed: Value = serde_json::from_str(&reply[open..=close]).ok()?;
    parsed.is_object().then_some(parsed)
}

fn relations_from_array(value: &Value) -> Vec<ExtractedRelation> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| {
            let from = item.get("from").and_then(|v| v.as_str())?.trim();
            let to = item.get("to").and_then(|v| v.as_str())?.trim();
            let relation_type = item
                .get("relation_type")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|t| crate::constants::VALID_RELATION_TYPES.contains(t))
                .unwrap_or("related_to");
            if from.is_empty() || to.is_empty() || from.eq_ignore_ascii_case(to) {
                return None;
            }
            Some(ExtractedRelation {
                from: from.to_string(),
                to: to.to_string(),
                relation_type: relation_type.to_string(),
            })
        })
        .collect()
}

pub async fn link_relations_from_reply(pool: &PgPool, reply: &str) -> Result<u32> {
    let (_, relations) = parse_extraction_reply(reply);
    Ok(link_entities(pool, &relations).await)
}

#[derive(Default)]
pub struct ExtractionOutcome {
    pub added: u32,
    pub relations_linked: u32,
}

fn rem_extraction_args(content: &str) -> Value {
    serde_json::json!({
        "action": "auto_extract",
        "text": content,
        "untrusted": true,
        "budget_secs": relation_scan_budget().as_secs(),
    })
}

pub async fn rem_extract_observation(
    pool: &PgPool,
    id: uuid::Uuid,
    content: &str,
) -> Result<ExtractionOutcome> {
    let args = rem_extraction_args(content);

    if let Err(why) = crate::redact::refuse_secrets(&args, "text", content.trim()) {
        mark_extracted(pool, id).await;
        tracing::warn!(
            error = %format!("{why:#}"),
            observation = %id,
            "auto-extraction will never succeed on this observation, marking it done"
        );
        return Ok(ExtractionOutcome::default());
    }

    let result = auto_extract(pool, &args).await?;

    if result.get("degraded").and_then(Value::as_bool) == Some(true) {
        anyhow::bail!(
            "auto_extract degraded: {}",
            result
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        );
    }

    mark_extracted(pool, id).await;

    Ok(ExtractionOutcome {
        added: result.get("added").and_then(Value::as_u64).unwrap_or(0) as u32,
        relations_linked: result
            .get("relations_linked")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    })
}

async fn mark_extracted(pool: &PgPool, id: uuid::Uuid) {
    if let Err(e) = sqlx::query("UPDATE brain_observations SET extracted_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
    {
        tracing::error!(
            error = %e,
            observation = %id,
            "failed to mark this observation as extracted — it will be reprocessed next cycle"
        );
    }
}

pub async fn observations_awaiting_extraction(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<(uuid::Uuid, String)>> {
    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id, content FROM brain_observations
         WHERE extracted_at IS NULL AND trust = 'trusted'
         ORDER BY extraction_attempts ASC, created_at ASC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("listing observations awaiting extraction")?;
    Ok(rows)
}

pub async fn mark_extraction_attempt_failed(pool: &PgPool, id: uuid::Uuid) {
    if let Err(e) = sqlx::query(
        "UPDATE brain_observations SET extraction_attempts = extraction_attempts + 1
         WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await
    {
        tracing::error!(
            error = %e,
            observation = %id,
            "failed to record this auto-extraction attempt — it stays at the same priority \
             and may starve the queue behind it again"
        );
    }
}

const RELATION_SCAN_MAX_OBSERVATIONS: i64 = 12;
const RELATION_SCAN_MAX_CHARS: usize = 4000;
const RELATION_SCAN_NEIGHBOURS: i64 = 60;

pub fn build_relation_scan_prompt(
    entity: &str,
    entity_type: &str,
    observations: &[String],
    known: &[String],
) -> String {
    let rel_types = crate::constants::VALID_RELATION_TYPES.join(", ");
    let mut body = String::new();
    for obs in observations {
        body.push_str("- ");
        body.push_str(obs);
        body.push('\n');
        if body.chars().count() > RELATION_SCAN_MAX_CHARS {
            break;
        }
    }
    let known_line = if known.is_empty() {
        String::new()
    } else {
        format!(
            "\nEntities already in the graph — reuse these names verbatim when one of them is \
             what the notes mean:\n{}\n",
            known.join(", ")
        )
    };

    format!(
        "You are mapping how one entity connects to others in a knowledge graph built from an \
         AI coding agent's notes.\n\n\
         ENTITY: \"{entity}\" (type: {entity_type})\n\n\
         WHAT THE NOTES SAY ABOUT IT:\n{body}{known_line}\n\
         List the relations these notes actually support, with \"{entity}\" as one endpoint. \
         State only what the notes assert — do not invent plausible-sounding links, and do not \
         relate the entity to itself. If the notes support nothing, return an empty list; that \
         is a valid and common answer.\n\n\
         Return STRICT JSON, exactly this shape:\n\
         {{\"relations\": [{{\"from\": <entity name>, \"to\": <entity name>, \
         \"relation_type\": one of [{rel_types}]}}]}}\n\n\
         No prose, no markdown — just the JSON object."
    )
}

pub async fn scan_entity_relations(pool: &PgPool, entity_id: uuid::Uuid) -> Result<u32> {
    let entity: Option<(String, String)> =
        sqlx::query_as("SELECT name, entity_type FROM brain_entities WHERE id = $1")
            .bind(entity_id)
            .fetch_optional(pool)
            .await
            .context("reading the entity to scan")?;
    let Some((name, entity_type)) = entity else {
        return Ok(0);
    };

    let observations: Vec<(String,)> = sqlx::query_as(
        "SELECT content FROM brain_observations
         WHERE entity_id = $1 AND trust = 'trusted'
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(entity_id)
    .bind(RELATION_SCAN_MAX_OBSERVATIONS)
    .fetch_all(pool)
    .await
    .context("reading observations for the relation scan")?;

    if observations.is_empty() {
        mark_relations_scanned(pool, entity_id).await;
        return Ok(0);
    }

    let known: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM brain_entities
         WHERE id <> $1
         ORDER BY (SELECT count(*) FROM brain_relations r
                   WHERE r.from_entity = brain_entities.id OR r.to_entity = brain_entities.id) DESC,
                  created_at DESC
         LIMIT $2",
    )
    .bind(entity_id)
    .bind(RELATION_SCAN_NEIGHBOURS)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let prompt = build_relation_scan_prompt(
        &name,
        &entity_type,
        &observations.into_iter().map(|(c,)| c).collect::<Vec<_>>(),
        &known.into_iter().map(|(n,)| n).collect::<Vec<_>>(),
    );

    let Ok((reply, _)) = extraction_reply_within(&prompt, relation_scan_budget()).await else {
        anyhow::bail!("no LLM reachable for the relation scan");
    };

    let linked = link_relations_from_reply(pool, &reply).await?;
    mark_relations_scanned(pool, entity_id).await;
    Ok(linked)
}

async fn mark_relations_scanned(pool: &PgPool, entity_id: uuid::Uuid) {
    let _ = sqlx::query("UPDATE brain_entities SET relations_scanned_at = NOW() WHERE id = $1")
        .bind(entity_id)
        .execute(pool)
        .await;
}

pub async fn mark_relation_scan_attempt_failed(pool: &PgPool, entity_id: uuid::Uuid) {
    if let Err(e) = sqlx::query(
        "UPDATE brain_entities SET relation_scan_attempts = relation_scan_attempts + 1
         WHERE id = $1",
    )
    .bind(entity_id)
    .execute(pool)
    .await
    {
        tracing::error!(
            error = %e,
            entity = %entity_id,
            "failed to record this relation scan attempt — it stays at the same priority \
             and may starve the queue behind it again"
        );
    }
}

pub async fn entities_awaiting_relation_scan(pool: &PgPool, limit: i64) -> Result<Vec<uuid::Uuid>> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT e.id FROM brain_entities e
         WHERE EXISTS (SELECT 1 FROM brain_observations o
                       WHERE o.entity_id = e.id AND o.trust = 'trusted')
           AND NOT EXISTS (SELECT 1 FROM brain_relations r
                           WHERE r.from_entity = e.id OR r.to_entity = e.id)
           AND (e.relations_scanned_at IS NULL
                OR EXISTS (SELECT 1 FROM brain_observations o
                           WHERE o.entity_id = e.id
                             AND o.created_at > e.relations_scanned_at))
         ORDER BY e.relation_scan_attempts ASC,
                  e.relations_scanned_at ASC NULLS FIRST,
                  (SELECT count(*) FROM brain_observations o WHERE o.entity_id = e.id) DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("listing entities awaiting a relation scan")?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

async fn link_entities(pool: &PgPool, relations: &[ExtractedRelation]) -> u32 {
    let project_id = crate::project::current_project_id(pool)
        .await
        .ok()
        .flatten();
    let mut created = 0u32;

    for rel in relations {
        let from_id = upsert_entity(pool, &rel.from, project_id).await;
        let to_id = upsert_entity(pool, &rel.to, project_id).await;
        let (Some(from_id), Some(to_id)) = (from_id, to_id) else {
            continue;
        };

        let done = sqlx::query(
            "INSERT INTO brain_relations
                (from_entity, to_entity, relation_type, project_id, provenance)
             VALUES ($1, $2, $3, $4, 'inferred')
             ON CONFLICT (from_entity, to_entity, relation_type)
             DO UPDATE SET strength = LEAST(brain_relations.strength + 0.1, 1.0),
                           last_traversed = NOW()",
        )
        .bind(from_id)
        .bind(to_id)
        .bind(&rel.relation_type)
        .bind(project_id)
        .execute(pool)
        .await;

        match done {
            Ok(r) if r.rows_affected() > 0 => created += 1,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(error = %e, from = %rel.from, to = %rel.to, "auto_extract: could not write relation")
            }
        }
    }
    created
}

async fn upsert_entity(
    pool: &PgPool,
    name: &str,
    project_id: Option<uuid::Uuid>,
) -> Option<uuid::Uuid> {
    let row: Result<(uuid::Uuid,), _> = sqlx::query_as(
        "INSERT INTO brain_entities (name, entity_type, project_id)
         VALUES ($1, 'concept', $2)
         ON CONFLICT (name) DO UPDATE SET updated_at = NOW()
         RETURNING id",
    )
    .bind(name)
    .bind(project_id)
    .fetch_one(pool)
    .await;
    match row {
        Ok((id,)) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, entity = %name, "auto_extract: could not upsert entity");
            None
        }
    }
}

fn parse_extracted_items(reply: &str) -> Vec<Value> {
    let slice = match (reply.find('['), reply.rfind(']')) {
        (Some(a), Some(b)) if b > a => &reply[a..=b],
        _ => return Vec::new(),
    };
    let parsed: Value = match serde_json::from_str(slice) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    items_from_array(&parsed)
}

fn items_from_array(value: &Value) -> Vec<Value> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };

    const VALID_TYPES: &[&str] = &[
        "fact",
        "decision",
        "lesson",
        "preference",
        "error",
        "solution",
        "context",
        "tool_usage",
    ];

    arr.iter()
        .filter_map(|item| {
            let entity_name = item.get("entity_name").and_then(|v| v.as_str())?.trim();
            let content = item.get("content").and_then(|v| v.as_str())?.trim();
            if entity_name.is_empty() || content.is_empty() {
                return None;
            }
            let obs_type = item
                .get("observation_type")
                .and_then(|v| v.as_str())
                .filter(|t| VALID_TYPES.contains(t))
                .unwrap_or("fact");
            Some(serde_json::json!({
                "entity_name": entity_name,
                "content": content,
                "observation_type": obs_type,
                "source": "inference"
            }))
        })
        .collect()
}

async fn ingest(pool: &PgPool, args: &Value) -> Result<Value> {
    let items = args
        .get("items")
        .and_then(|v| v.as_array())
        .context("'items' array is required for ingest action")?;

    if items.is_empty() {
        anyhow::bail!("items array is empty");
    }
    if items.len() > 200 {
        anyhow::bail!("ingest limit is 200 items per call (got {})", items.len());
    }

    for (index, item) in items.iter().enumerate() {
        if let Some(content) = item.get("content").and_then(|v| v.as_str()) {
            crate::redact::refuse_secrets(args, &format!("items[{index}].content"), content)?;
        }
    }

    let observations: Vec<Value> = items
        .iter()
        .filter_map(|item| {
            let entity_name = item.get("entity_name").and_then(|v| v.as_str())?;
            let content = item.get("content").and_then(|v| v.as_str())?;
            if entity_name.is_empty() || content.is_empty() {
                return None;
            }
            let obs_type = item
                .get("observation_type")
                .and_then(|v| v.as_str())
                .unwrap_or("fact");
            let source = item
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("agent");
            let mut row = serde_json::json!({
                "entity_name": entity_name,
                "content": content,
                "observation_type": obs_type,
                "source": source
            });
            if let Some(trust) = item.get("trust").and_then(|v| v.as_str()) {
                row["trust"] = serde_json::json!(trust);
            }
            Some(row)
        })
        .collect();

    let skipped = items.len() - observations.len();

    let batch_args = serde_json::json!({
        "action": "batch_add",
        "observations": observations,
        "allow_secret": allow_secret(args)
    });

    let result = super::cronica::handle(pool, batch_args).await?;

    let mut response = result;
    if let Some(obj) = response.as_object_mut() {
        obj.insert("action".to_string(), serde_json::json!("ingest"));
        obj.insert("skipped_invalid".to_string(), serde_json::json!(skipped));
        obj.insert("total_items".to_string(), serde_json::json!(items.len()));
    }

    Ok(response)
}

async fn parse(pool: &PgPool, args: &Value) -> Result<Value> {
    let entity_name = args
        .get("entity_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if entity_name.is_empty() {
        anyhow::bail!("entity_name is required for parse action");
    }

    let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
    if text.is_empty() {
        anyhow::bail!("text is required for parse action");
    }

    crate::redact::refuse_secrets(args, "text", text)?;

    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(|p| p.trim())
        .filter(|p| p.len() > 10)
        .collect();

    if paragraphs.is_empty() {
        return Ok(serde_json::json!({
            "action": "parse",
            "entity_name": entity_name,
            "parsed_count": 0,
            "note": "No substantial paragraphs found (min 10 chars after split on double-newline)"
        }));
    }

    let items: Vec<Value> = paragraphs
        .iter()
        .map(|p| {
            let obs_type = classify_paragraph(p);
            serde_json::json!({
                "entity_name": entity_name,
                "content": p,
                "observation_type": obs_type,
                "source": "agent"
            })
        })
        .collect();

    let parsed_count = items.len();

    let ingest_args = serde_json::json!({
        "action": "ingest",
        "items": items,
        "allow_secret": allow_secret(args)
    });

    let result = ingest(pool, &ingest_args).await?;

    let mut response = result;
    if let Some(obj) = response.as_object_mut() {
        obj.insert("action".to_string(), serde_json::json!("parse"));
        obj.insert("parsed_count".to_string(), serde_json::json!(parsed_count));
    }

    Ok(response)
}

fn classify_paragraph(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    if lower.contains("decided") || lower.contains("decision") || lower.contains("chose") {
        "decision"
    } else if lower.contains("learned") || lower.contains("lesson") || lower.contains("takeaway") {
        "lesson"
    } else if lower.contains("error") || lower.contains("bug") || lower.contains("failed") {
        "error"
    } else if lower.contains("fix") || lower.contains("solution") || lower.contains("resolved") {
        "solution"
    } else if lower.contains("prefer")
        || lower.contains("preference")
        || lower.contains("always use")
    {
        "preference"
    } else {
        "fact"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pool_that_cannot_connect() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(250))
            .connect_lazy("postgres://ingesta-test:unused@127.0.0.1:63999/does-not-exist")
            .expect("connect_lazy only parses the URL, it does not dial the network")
    }

    #[tokio::test]
    async fn bulk_ingestion_refuses_the_one_item_that_carries_a_credential() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = json!({
            "action": "ingest",
            "items": [
                {"entity_name": "deploy", "content": "el pipeline corre en GitHub Actions"},
                {"entity_name": "deploy", "content": "el runner usa ghp_abcdefghijklmnop"}
            ]
        });

        let Err(failure) = handle(&pool, args).await else {
            panic!(
                "ingest answered Ok on a pool that cannot connect: nothing but the secret gate \
                 could have answered, and the gate must refuse"
            );
        };

        let chain = format!("{failure:#}");
        assert!(
            chain.contains("github token") && chain.contains("items[1].content"),
            "a bulk write is the highest-volume door into the graph and nobody rereads 200 \
             items one by one: the refusal has to name which item carries the credential. Got: \
             {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    #[tokio::test]
    async fn splitting_a_raw_dump_into_paragraphs_does_not_launder_a_credential() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = json!({
            "action": "parse",
            "entity_name": "deploy",
            "text": "El despliegue quedó documentado.\n\nLa cabecera era ghp_abcdefghijklmnop y \
                     por eso fallaba."
        });

        let chain = format!(
            "{:#}",
            handle(&pool, args)
                .await
                .expect_err("a github token pasted into a raw dump must not be storable")
        );
        assert!(
            chain.contains("github token") && chain.contains("text"),
            "parse chops the dump into observations and writes every piece: refusing per \
             paragraph would name a field the caller never wrote, so the gate has to sit on the \
             text it was handed. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    #[tokio::test]
    async fn a_work_log_with_a_credential_in_it_never_reaches_the_extracting_model() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = json!({
            "action": "auto_extract",
            "text": "Estuvimos toda la tarde con el deploy; al final entró con ghp_abcdefghijklmnop."
        });

        let chain = format!(
            "{:#}",
            handle(&pool, args)
                .await
                .expect_err("a work log with a live token in it must not be extractable")
        );
        assert!(
            chain.contains("github token") && chain.contains("text"),
            "auto_extract hands this text to an LLM — an MCP sampling round trip or a CLI \
             subprocess — before anything is stored. Gating only what comes back would leak the \
             credential out of the process first, which no later refusal can undo. Got: {chain}"
        );
        assert!(
            !chain.contains("ghp_abcdefghijklmnop"),
            "the refusal repeated the secret, and refusals get logged: {chain}"
        );
    }

    #[tokio::test]
    async fn the_override_survives_the_hop_into_the_observation_writer() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let pool = pool_that_cannot_connect();

        let args = json!({
            "action": "ingest",
            "allow_secret": true,
            "items": [{"entity_name": "aws", "content": "AKIAIOSFODNN7EXAMPLE es el de la doc"}]
        });

        let chain = format!(
            "{:#}",
            handle(&pool, args)
                .await
                .expect_err("the pool cannot connect, so this call always ends in an error")
        );
        assert!(
            !chain.contains("Remove it and store a pointer"),
            "ingest hands its rows to cronica batch_add, which runs the same gate on arguments \
             ingest builds itself: dropping allow_secret on that hop leaves the escape hatch \
             visible in the schema and unreachable in practice. Got: {chain}"
        );
    }

    use super::parse_extracted_items;

    #[test]
    fn parses_clean_json_array() {
        let reply = r#"[{"entity_name":"cuba-memorys","content":"uses pgvector","observation_type":"fact"}]"#;
        let items = parse_extracted_items(reply);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["entity_name"], "cuba-memorys");
        assert_eq!(items[0]["observation_type"], "fact");
        assert_eq!(items[0]["source"], "inference");
    }

    #[test]
    fn recovers_json_from_markdown_fences_and_prose() {
        let reply = "Sure! Here are the facts:\n```json\n[{\"entity_name\":\"X\",\"content\":\"did Y\",\"observation_type\":\"decision\"}]\n```\nHope that helps.";
        let items = parse_extracted_items(reply);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["observation_type"], "decision");
    }

    #[test]
    fn drops_invalid_rows_and_normalizes_bad_type() {
        let reply = r#"[
            {"entity_name":"","content":"no entity","observation_type":"fact"},
            {"entity_name":"A","content":"","observation_type":"fact"},
            {"entity_name":"B","content":"good","observation_type":"nonsense"}
        ]"#;
        let items = parse_extracted_items(reply);
        assert_eq!(items.len(), 1, "only the one with entity+content survives");
        assert_eq!(items[0]["entity_name"], "B");
        assert_eq!(
            items[0]["observation_type"], "fact",
            "unknown type falls back to fact"
        );
    }

    #[test]
    fn empty_or_no_array_yields_nothing() {
        assert!(parse_extracted_items("[]").is_empty());
        assert!(parse_extracted_items("I couldn't find any facts.").is_empty());
        assert!(parse_extracted_items("").is_empty());
    }

    use super::parse_extraction_reply;

    #[test]
    fn parses_facts_and_relations_from_the_object_shape() {
        let reply = r#"{
            "facts": [{"entity_name":"cuba-memorys","content":"uses pgvector","observation_type":"fact"}],
            "relations": [{"from":"cuba-memorys","to":"PostgreSQL","relation_type":"depends_on"}]
        }"#;
        let (facts, relations) = parse_extraction_reply(reply);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["entity_name"], "cuba-memorys");
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].from, "cuba-memorys");
        assert_eq!(relations[0].to, "PostgreSQL");
        assert_eq!(relations[0].relation_type, "depends_on");
    }

    #[test]
    fn a_bare_array_still_yields_its_facts() {
        let reply = r#"[{"entity_name":"X","content":"did Y","observation_type":"decision"}]"#;
        let (facts, relations) = parse_extraction_reply(reply);
        assert_eq!(facts.len(), 1, "old-format facts must still land");
        assert!(relations.is_empty());
    }

    #[test]
    fn an_invalid_relation_type_falls_back_instead_of_dropping_the_edge() {
        let reply =
            r#"{"facts":[],"relations":[{"from":"A","to":"B","relation_type":"invented_type"}]}"#;
        let (_, relations) = parse_extraction_reply(reply);
        assert_eq!(relations.len(), 1);
        assert_eq!(
            relations[0].relation_type, "related_to",
            "an unknown type must degrade to the generic one, not violate the DB CHECK"
        );
    }

    #[test]
    fn self_loops_and_empty_endpoints_are_dropped() {
        let reply = r#"{"facts":[],"relations":[
            {"from":"A","to":"A","relation_type":"uses"},
            {"from":"a","to":"A","relation_type":"uses"},
            {"from":"","to":"B","relation_type":"uses"},
            {"from":"C","to":"","relation_type":"uses"},
            {"from":"D","to":"E","relation_type":"uses"}
        ]}"#;
        let (_, relations) = parse_extraction_reply(reply);
        assert_eq!(relations.len(), 1, "only D->E survives");
        assert_eq!(relations[0].from, "D");
    }

    #[test]
    fn missing_relations_key_is_not_an_error() {
        let reply = r#"{"facts":[{"entity_name":"X","content":"y","observation_type":"fact"}]}"#;
        let (facts, relations) = parse_extraction_reply(reply);
        assert_eq!(facts.len(), 1);
        assert!(relations.is_empty());
    }

    #[test]
    fn prose_and_fences_around_the_object_are_tolerated() {
        let reply = "Here you go:\n```json\n{\"facts\":[],\"relations\":[{\"from\":\"A\",\"to\":\"B\",\"relation_type\":\"causes\"}]}\n```\nDone.";
        let (_, relations) = parse_extraction_reply(reply);
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].relation_type, "causes");
    }

    #[test]
    fn every_relation_type_the_prompt_offers_is_accepted() {
        for t in crate::constants::VALID_RELATION_TYPES {
            let reply = format!(
                r#"{{"facts":[],"relations":[{{"from":"A","to":"B","relation_type":"{t}"}}]}}"#
            );
            let (_, relations) = parse_extraction_reply(&reply);
            assert_eq!(relations.len(), 1, "type {t} was rejected");
            assert_eq!(relations[0].relation_type, *t);
        }
    }

    #[test]
    fn rem_extraction_always_forces_quarantine() {
        let args = rem_extraction_args("cualquier observación de prueba");
        assert_eq!(
            args["untrusted"],
            json!(true),
            "the REM cycle is the only fully unattended writer this graph has: nothing a human \
             reviewed gates what it produces, so what it produces must land quarantined \
             regardless of what an operator's CUBA_QUARANTINE_INFERENCE happens to be set to"
        );
        assert_eq!(args["action"], json!("auto_extract"));
    }

    #[test]
    fn rem_extraction_gets_the_same_budget_as_the_scan_it_rides_along_with() {
        let asked = rem_extraction_args("cualquier observación de prueba")["budget_secs"]
            .as_u64()
            .expect("the REM cycle must state a budget instead of inheriting the handler's");

        assert_eq!(asked, relation_scan_budget().as_secs());
        assert!(
            asked > extraction_budget().as_secs(),
            "extraction_budget is 60% of the MCP handler timeout — 18s — because a caller is \
             waiting on the other end. Nobody waits on the REM cycle, and the CLI it shells out \
             to takes longer than that: measured in production on 2026-08-16, every single \
             attempt died with 'claude CLI timed out after 18.000000715s', so the cycle logged \
             extraction_failed=2 and facts_extracted=0 forever while the relation scan, which \
             calls the same CLI with 90s, failed zero times. Asked for {asked}s against an \
             extraction default of {}s",
            extraction_budget().as_secs()
        );
    }

    #[tokio::test]
    async fn a_degraded_extraction_is_an_error_and_leaves_the_observation_unmarked() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let previous_cli = std::env::var("CUBA_JUEZ_CLI").ok();
        unsafe { std::env::set_var("CUBA_JUEZ_CLI", "cuba-memorys-no-such-cli-on-this-machine") };

        let pool = pool_that_cannot_connect();
        let result =
            rem_extract_observation(&pool, uuid::Uuid::new_v4(), "una nota cualquiera").await;

        match previous_cli {
            Some(v) => unsafe { std::env::set_var("CUBA_JUEZ_CLI", v) },
            None => unsafe { std::env::remove_var("CUBA_JUEZ_CLI") },
        }

        assert!(
            result.is_err(),
            "with no CLI on PATH and no MCP sampling client attached, extraction has no \
             backend at all: rem_extract_observation must report that as an error, not as an \
             empty success, or the REM cycle would stamp extracted_at on an observation that \
             was never actually looked at and it would silently drop out of the queue forever \
             the very first time the backend was unavailable"
        );
    }

    fn write_extraction_stub(reply: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cuba-memorys-extraction-stub-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let script =
            format!("#!/bin/sh\ncat >/dev/null\ncat <<'CUBA_STUB_EOF'\n{reply}\nCUBA_STUB_EOF\n");
        std::fs::write(&path, script).expect("writing the extraction CLI stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("making the stub executable");
        }
        path
    }

    #[tokio::test]
    async fn a_db_failure_while_writing_is_not_treated_as_a_permanent_refusal() {
        let _one_at_a_time = crate::session::GLOBAL_STATE_GUARD.lock().await;
        let stub = write_extraction_stub(
            r#"{"facts":[{"entity_name":"CleanFactSubject","content":"a clean fact with no secret in it","observation_type":"fact"}],"relations":[]}"#,
        );
        let previous_cli = std::env::var("CUBA_JUEZ_CLI").ok();
        unsafe { std::env::set_var("CUBA_JUEZ_CLI", &stub) };

        let pool = pool_that_cannot_connect();
        let result = rem_extract_observation(
            &pool,
            uuid::Uuid::new_v4(),
            "a clean note with nothing sensitive in it",
        )
        .await;

        match previous_cli {
            Some(v) => unsafe { std::env::set_var("CUBA_JUEZ_CLI", v) },
            None => unsafe { std::env::remove_var("CUBA_JUEZ_CLI") },
        }
        std::fs::remove_file(&stub).ok();

        assert!(
            result.is_err(),
            "the extraction backend answered cleanly and the write gate had nothing to \
             refuse: the only thing that failed is the pool, which this test points at a \
             closed port — exactly the shape of a pool exhausted or a commit that failed. \
             Treating that the same as a poisoned observation stamps extracted_at on a fact \
             the graph never actually received; the previous code caught every Err from \
             auto_extract in one branch and returned Ok(ExtractionOutcome::default()) here"
        );
    }
}
