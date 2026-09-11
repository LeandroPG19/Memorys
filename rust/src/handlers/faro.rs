use crate::cognitive::dual_strength;
use crate::search::cache::TtlLruCache;
use crate::search::confidence as grounding;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

const DEFAULT_LIMIT: i64 = 10;
const MAX_LIMIT: i64 = 50;
const DEFAULT_MAX_TOKENS: i64 = 5000;
const GRAPHRAG_TOP_K: usize = 3;

#[derive(Clone)]
struct FusedResult {
    text_score: f64,
    vector_score: f64,
    bm25_score: f64,
    session_boosted: bool,
    total: f64,
    data: Value,
}

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        anyhow::bail!("query is required");
    }

    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("hybrid");
    let scope = args.get("scope").and_then(|v| v.as_str()).unwrap_or("all");
    let limit = requested_limit(&args);

    let max_tokens = args
        .get("max_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_MAX_TOKENS);

    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("compact");

    let tag_filter = args.get("tags").and_then(|v| v.as_str());

    let track_access = args
        .get("track_access")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let associative = args
        .get("associative")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let before = args.get("before").and_then(|v| v.as_str());
    let after = args.get("after").and_then(|v| v.as_str());
    let time_bounds = parse_time_bounds(before, after)?;

    let diversify = args
        .get("diversify")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mmr_lambda = args
        .get("mmr_lambda")
        .and_then(|v| v.as_f64())
        .unwrap_or(crate::search::mmr::DEFAULT_LAMBDA);

    let abstain_ood = args
        .get("abstain_ood")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ood_threshold = args.get("ood_threshold").and_then(|v| v.as_f64());

    let enable_bm25 = args
        .get("enable_bm25")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let enable_rerank = match args.get("rerank").and_then(|v| v.as_bool()) {
        Some(explicit) => explicit,
        None => {
            crate::mode::active().rerank_default()
                && tokio::task::spawn_blocking(crate::search::rerank::enabled)
                    .await
                    .context("reranker status task panicked")?
        }
    };

    let project_id = crate::project::current_project_id(pool).await?;

    if abstain_ood
        && mode == "hybrid"
        && let Some(answer) = check_ood(pool, query, ood_threshold, project_id).await
    {
        return Ok(answer);
    }

    let search_opts = SearchOpts {
        scope,
        limit,
        max_tokens,
        time_bounds,
        format,
        tag_filter,
        project_id,
        diversify,
        mmr_lambda,
        enable_bm25,
        enable_rerank,
        track_access,
        associative,
    };

    let mut response = match mode {
        "hybrid" => hybrid_search(pool, query, &search_opts).await,
        "verify" => verify_claim(pool, query, project_id).await,
        _ => anyhow::bail!("Invalid mode: {mode}. Use hybrid/verify"),
    }?;

    annotate_sync_freshness(pool, &mut response).await;

    Ok(response)
}

const SYNC_STALE_AFTER_SECS: i64 = 86_400;

async fn annotate_sync_freshness(pool: &PgPool, response: &mut Value) {
    let peers: Vec<(
        String,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    )> = match sqlx::query_as("SELECT name, last_synced_at, last_error FROM brain_sync_peers")
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows,
        Err(_) => return,
    };

    if peers.is_empty() {
        return;
    }

    let now = chrono::Utc::now();
    let peers_json: Vec<Value> = peers
        .into_iter()
        .map(|(name, last_synced_at, last_error)| {
            let age_seconds = last_synced_at.map(|t| (now - t).num_seconds().max(0));
            let age_desc = match age_seconds {
                Some(s) => format!("{s}s"),
                None => "nunca".to_string(),
            };

            let mut entry = serde_json::json!({ "name": name });
            if let Some(age) = age_seconds {
                entry["age_seconds"] = serde_json::json!(age);
            }

            let unresolved_error = last_error.filter(|e| !e.trim().is_empty());
            if let Some(err) = unresolved_error {
                entry["warning"] = serde_json::json!(format!(
                    "{name} no sincroniza hace {age_desc} y su último intento falló: {err}. \
                     No es solo que la vista esté vieja: nada dice que vaya a ponerse al día."
                ));
            } else if age_seconds.is_none_or(|age| age > SYNC_STALE_AFTER_SECS) {
                entry["warning"] = serde_json::json!(format!(
                    "{name} lleva {age_desc} sin sincronizar: lo que esta respuesta usa de esa \
                     máquina puede estar superado allí."
                ));
            }
            entry
        })
        .collect();

    response["sync_peers"] = serde_json::json!(peers_json);
}

fn requested_limit(args: &Value) -> i64 {
    args.get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
}

#[derive(Debug)]
struct TimeBounds {
    after: chrono::DateTime<chrono::Utc>,
    before: chrono::DateTime<chrono::Utc>,
}

fn parse_bound(field: &str, raw: &str) -> Result<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&chrono::Utc))
        .map_err(|e| {
            anyhow::anyhow!(
                "{field}={raw:?} is not an RFC 3339 timestamp ({e}). \
                 Use e.g. 2026-08-11T00:00:00Z. Rejected instead of ignored: a bound \
                 that silently defaults widens the window you asked to narrow."
            )
        })
}

fn parse_time_bounds(before: Option<&str>, after: Option<&str>) -> Result<TimeBounds> {
    let after_ts = match after {
        Some(raw) => parse_bound("after", raw)?,
        None => chrono::DateTime::from_timestamp(0, 0)
            .expect("the unix epoch is representable as a DateTime<Utc>"),
    };
    let before_ts = match before {
        Some(raw) => parse_bound("before", raw)?,
        None => chrono::Utc::now() + chrono::Duration::days(365),
    };
    Ok(TimeBounds {
        after: after_ts,
        before: before_ts,
    })
}

struct SearchOpts<'a> {
    scope: &'a str,
    limit: i64,
    max_tokens: i64,
    time_bounds: TimeBounds,
    format: &'a str,
    tag_filter: Option<&'a str>,
    project_id: Option<uuid::Uuid>,
    diversify: bool,
    mmr_lambda: f64,
    enable_bm25: bool,
    enable_rerank: bool,
    track_access: bool,
    associative: bool,
}

fn annotate_degradation(
    response: &mut Value,
    vector_failure: Option<&str>,
    bm25_failed: bool,
    reranker_failed: bool,
) {
    if let Some(reason) = vector_failure {
        let reason = crate::redact::redact_secrets(reason);
        response["degraded"] = serde_json::json!(true);
        response["degraded_reason"] = serde_json::json!(format!(
            "La búsqueda vectorial no se ejecutó: estos resultados son SOLO léxicos y el \
             recall está degradado. Causa: {reason}. Diagnosticá con `cuba-memorys doctor`."
        ));
    }
    if bm25_failed {
        response["bm25_degraded"] = serde_json::json!(true);
        response["bm25_degraded_reason"] = serde_json::json!(
            "BM25 falló: estos resultados salen sin la señal léxica exacta, así que una \
             consulta con términos raros pierde recall. Mirá los logs (nivel ERROR)."
        );
    }
    if reranker_failed {
        response["reranker_degraded"] = serde_json::json!(true);
        response["reranker_degraded_reason"] = serde_json::json!(
            "Pediste rerank y el cross-encoder falló: estos resultados vienen SIN reordenar, \
             tal cual los dejó RRF. Se pagó el tiempo de inferencia y no se aplicó nada. \
             Mirá los logs (nivel ERROR) para la causa."
        );
    }
}

async fn hybrid_search(pool: &PgPool, query: &str, opts: &SearchOpts<'_>) -> Result<Value> {
    let query_entropy = crate::search::rrf::query_entropy(query);
    let (text_weight, vector_weight) = entropy_weights(query_entropy);
    let bm25_weight = text_weight;
    let query_class = crate::search::query_class::classify_query(query);
    let mentioned = mentioned_entities(pool, query).await;
    let scope = crate::search::entity_factoid::factoid_effective_scope(
        query_class,
        !mentioned.is_empty(),
        opts.scope,
    );

    let mut text_results = text_search(
        pool,
        query,
        scope,
        opts.limit * 2,
        &opts.time_bounds,
        opts.project_id,
    )
    .await?;

    if let Some(tag) = opts.tag_filter {
        let tagged_obs: Vec<(uuid::Uuid, String, String, String, f64, f64)> = sqlx::query_as(
            "SELECT o.id, e.name, o.content, o.observation_type, o.importance::float8,
                    (o.importance::float8 * 0.8 + 0.2)::float8 AS score
             FROM brain_observations o
             JOIN brain_entities e ON o.entity_id = e.id
             WHERE $1 = ANY(o.tags)
               AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
               AND ($3::uuid IS NULL OR o.project_id = $3 OR o.project_id IS NULL)
             ORDER BY o.importance DESC
             LIMIT $2",
        )
        .bind(tag)
        .bind(opts.limit)
        .bind(opts.project_id)
        .fetch_all(pool)
        .await
        .with_context(|| {
            format!(
                "looking up observations tagged {tag:?}. Failing loud instead of returning \
                 the untagged ranking: those results would be presented as if the tag had \
                 been applied and simply matched nothing."
            )
        })?;

        for (id, entity_name, content, obs_type, importance, score) in tagged_obs {
            let id_str = id.to_string();
            if !text_results
                .iter()
                .any(|r| r.get("id").and_then(|v| v.as_str()) == Some(&id_str))
            {
                text_results.push(serde_json::json!({
                    "id": id_str,
                    "type": "observation",
                    "entity_name": entity_name,
                    "content": content,
                    "observation_type": obs_type,
                    "importance": importance,
                    "score": score,
                    "matched_tag": tag
                }));
            }
        }
    }

    let vector_results = vector_search(
        pool,
        query,
        scope,
        opts.limit * 2,
        &opts.time_bounds,
        opts.project_id,
    )
    .await;

    let rrf_k = crate::search::rrf::RRF_K;

    let mut fused_scores: HashMap<String, FusedResult> = HashMap::new();

    for (rank, result) in text_results.iter().enumerate() {
        let id = result
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let rrf_score = text_weight / (rrf_k + rank as f64 + 1.0);
        fused_scores.insert(
            id,
            FusedResult {
                text_score: rrf_score,
                vector_score: 0.0,
                bm25_score: 0.0,
                session_boosted: false,
                total: rrf_score,
                data: result.clone(),
            },
        );
    }

    let vector_failure: Option<String> = match &vector_results {
        Ok(_) => None,
        Err(e) => {
            tracing::error!(
                error = %e,
                "VECTOR SEARCH FAILED — hybrid retrieval degraded to lexical only."
            );
            Some(e.to_string())
        }
    };

    if let Ok(vec_results) = vector_results {
        for (rank, result) in vec_results.iter().enumerate() {
            let id = result
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let rrf_score = vector_weight / (rrf_k + rank as f64 + 1.0);
            fused_scores
                .entry(id.clone())
                .and_modify(|fr| {
                    fr.vector_score = rrf_score;
                    fr.total += rrf_score;
                })
                .or_insert(FusedResult {
                    text_score: 0.0,
                    vector_score: rrf_score,
                    bm25_score: 0.0,
                    session_boosted: false,
                    total: rrf_score,
                    data: result.clone(),
                });
        }
    }

    let mut bm25_failed = false;
    if opts.enable_bm25 {
        let bm25_results = match crate::search::bm25::bm25_search(
            pool,
            query,
            scope,
            opts.limit * 2,
            opts.project_id,
        )
        .await
        {
            Ok(results) => results,
            Err(e) => {
                bm25_failed = true;
                tracing::error!(
                    error = %format!("{e:#}"),
                    "BM25 SEARCH FAILED — hybrid retrieval lost its lexical half. \
                     Usually a query that cuba_or_tsquery could not parse, a missing GIN \
                     index, or the pool timing out. Run `cuba-memorys doctor`."
                );
                Vec::new()
            }
        };
        for (rank, result) in bm25_results.iter().enumerate() {
            let id = result
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let rrf_score = bm25_weight / (rrf_k + rank as f64 + 1.0);
            fused_scores
                .entry(id.clone())
                .and_modify(|fr| {
                    fr.bm25_score = rrf_score;
                    fr.total += rrf_score;
                })
                .or_insert(FusedResult {
                    text_score: 0.0,
                    vector_score: 0.0,
                    bm25_score: rrf_score,
                    session_boosted: false,
                    total: rrf_score,
                    data: result.clone(),
                });
        }
    }

    if crate::search::entity_factoid::entity_factoid_enabled()
        && query_class == crate::search::query_class::QueryClass::Factoid
        && !mentioned.is_empty()
    {
        let entity_leg =
            entity_factoid_observations(pool, query, &mentioned, opts.limit * 2, opts.project_id)
                .await;
        for (rank, result) in entity_leg.iter().enumerate() {
            let id = result
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let rrf_score = 1.0 / (rrf_k + rank as f64 + 1.0);
            fused_scores
                .entry(id.clone())
                .and_modify(|fr| {
                    fr.total += rrf_score;
                })
                .or_insert(FusedResult {
                    text_score: 0.0,
                    vector_score: 0.0,
                    bm25_score: 0.0,
                    session_boosted: false,
                    total: rrf_score,
                    data: result.clone(),
                });
        }
    }

    if opts.associative {
        associative_expand(pool, query, opts.project_id, &mut fused_scores).await;
    }

    let mut results: Vec<(String, FusedResult)> = fused_scores.into_iter().collect();
    results.sort_by(|a, b| {
        b.1.total
            .partial_cmp(&a.1.total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    const FUSION_DEDUP: f64 = 0.85;
    let mut deduped: Vec<(String, FusedResult)> = Vec::with_capacity(results.len());
    for (id, fr) in results {
        let content = fr
            .data
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let duplicate = deduped.iter().any(|(_, existing)| {
            let other = existing
                .data
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            crate::search::rrf::content_overlap(content, other) > FUSION_DEDUP
        });
        if !duplicate {
            deduped.push((id, fr));
        }
    }
    results = deduped;

    let pool_size = if opts.enable_rerank {
        50.min(results.len())
    } else if opts.diversify {
        (opts.limit as usize * 5).min(results.len())
    } else {
        (opts.limit as usize).min(results.len())
    };
    results.truncate(pool_size);

    let mut reranker_failed = false;
    if opts.enable_rerank && results.len() > 1 {
        let contents: Vec<&str> = results
            .iter()
            .map(|(_, fr)| {
                fr.data
                    .get("content")
                    .or_else(|| fr.data.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            })
            .collect();
        let have_model = tokio::task::spawn_blocking(crate::search::rerank::enabled)
            .await
            .context("reranker status task panicked")?;

        let rerank_budget = std::time::Duration::from_secs(
            std::env::var("CUBA_RERANK_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(20),
        );
        let rerank_result = match tokio::time::timeout(
            rerank_budget,
            crate::search::rerank::rerank(query, &contents),
        )
        .await
        {
            Ok(inner) => inner,
            Err(_) => {
                tracing::warn!(
                    secs = rerank_budget.as_secs(),
                    "reranker excedió su presupuesto — se devuelve el ranking RRF"
                );
                Err(anyhow::anyhow!("reranker timeout"))
            }
        };
        match rerank_result {
            Ok(reranked) => {
                let original = results.clone();
                results = reranked
                    .into_iter()
                    .filter_map(|(idx, score)| {
                        let mut entry = original.get(idx).cloned()?;
                        if have_model {
                            entry.1.total = score;
                        } else {
                            entry.1.total += score * 0.0001;
                        }
                        Some(entry)
                    })
                    .collect();
            }
            Err(e) => {
                tracing::error!(
                    error = %format!("{e:#}"),
                    "RERANK FAILED — el ranking se devuelve SIN reordenar. El modelo se \
                     cargó y se gastó el tiempo de inferencia, pero sus scores se \
                     descartaron: los resultados son los de RRF."
                );
                reranker_failed = true;
            }
        }
    }

    let matched_obs_ids: Vec<uuid::Uuid> = results
        .iter()
        .filter_map(|(_, fr)| {
            fr.data
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<uuid::Uuid>().ok())
        })
        .collect();

    if opts.track_access
        && !matched_obs_ids.is_empty()
        && let Err(e) = dual_strength::on_search_match(pool, &matched_obs_ids).await
    {
        tracing::warn!(error = %e, "failed to apply Testing Effect boost");
    }

    let (session_boost, session_started) = session_context(pool).await.unwrap_or_default();
    if !session_boost.is_empty() {
        for (_, fr) in &mut results {
            let content = fr
                .data
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let entity_name = fr
                .data
                .get("entity_name")
                .or_else(|| fr.data.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let (boosted, flagged) = crate::search::entity_factoid::apply_session_entity_boost(
                fr.total,
                entity_name,
                content,
                &session_boost,
            );
            if flagged {
                fr.total = boosted;
                fr.session_boosted = true;
            }
        }
        results.sort_by(|a, b| {
            b.1.total
                .partial_cmp(&a.1.total)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
    }

    if opts.diversify && results.len() > 1 {
        let n = results.len();
        let relevance: Vec<f64> = results.iter().map(|(_, fr)| fr.total).collect();
        let token_sets: Vec<std::collections::HashSet<String>> = results
            .iter()
            .map(|(_, fr)| {
                fr.data
                    .get("content")
                    .or_else(|| fr.data.get("name"))
                    .and_then(|v| v.as_str())
                    .map(|s| {
                        s.to_lowercase()
                            .split(|c: char| !c.is_alphanumeric())
                            .filter(|w| w.len() > 2)
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        let mut pairwise = vec![vec![0.0_f64; n]; n];
        for i in 0..n {
            pairwise[i][i] = 1.0;
            for j in (i + 1)..n {
                let inter = token_sets[i].intersection(&token_sets[j]).count();
                let union = token_sets[i].union(&token_sets[j]).count();
                let jaccard = if union == 0 {
                    0.0
                } else {
                    inter as f64 / union as f64
                };
                pairwise[i][j] = jaccard;
                pairwise[j][i] = jaccard;
            }
        }
        let picks = crate::search::mmr::mmr_select(
            &relevance,
            &pairwise,
            opts.mmr_lambda,
            opts.limit as usize,
        );
        let reordered: Vec<(String, FusedResult)> = picks
            .into_iter()
            .filter_map(|i| results.get(i).cloned())
            .collect();
        results = reordered;
    } else {
        results.truncate(opts.limit as usize);
    }

    let graphrag_context = enrich_graphrag(pool, &results, GRAPHRAG_TOP_K).await;

    let mut results_json: Vec<Value> = results
        .iter()
        .map(|(_, fr)| {
            let mut r = fr.data.clone();
            if let Some(obj) = r.as_object_mut() {
                obj.insert("fused_score".to_string(), serde_json::json!(fr.total));
                obj.insert("text_score".to_string(), serde_json::json!(fr.text_score));
                obj.insert(
                    "vector_score".to_string(),
                    serde_json::json!(fr.vector_score),
                );
                obj.insert("bm25_score".to_string(), serde_json::json!(fr.bm25_score));
                obj.insert(
                    "session_boosted".to_string(),
                    serde_json::json!(fr.session_boosted),
                );
            }
            r
        })
        .collect();

    let mut ppr_names: Vec<String> = Vec::new();
    let mut graph_paths_json: Option<Value> = None;
    let mut graph_backend: Option<&'static str> = None;
    let mut graph_context_tokens = 0usize;
    let mut graph_hop_ball_tokens = 0usize;
    if query_class == crate::search::query_class::QueryClass::MultiHop {
        let mentioned = mentioned_entities(pool, query).await;
        let seeds = merge_seed_names(mentioned, entity_names_from_list(&results_json));
        if !seeds.is_empty() {
            if let Ok(ranked) = crate::graph_db::ppr_around_seeds(&seeds, 3, 15)
                && !ranked.is_empty()
            {
                graph_backend = Some("falkor");
                ppr_names = ranked.iter().take(5).map(|n| n.name.clone()).collect();
                let mut existing: HashSet<String> = results_json
                    .iter()
                    .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string))
                    .collect();
                let mut extras: Vec<Value> = Vec::new();
                for node in &ranked {
                    if extras.len() >= 3 {
                        break;
                    }
                    for obs in
                        observations_for_entities(pool, std::slice::from_ref(&node.name), 2).await
                    {
                        let Some(id) = obs.get("id").and_then(|v| v.as_str()) else {
                            continue;
                        };
                        if !existing.insert(id.to_string()) {
                            continue;
                        }
                        extras.push(obs);
                        if extras.len() >= 3 {
                            break;
                        }
                    }
                }
                if !extras.is_empty() {
                    let keep = (opts.limit as usize).saturating_sub(extras.len());
                    results_json.truncate(keep);
                    results_json.extend(extras);
                }
            }
            if let Some(seed) = seeds.first()
                && let Ok((hops, paths)) = crate::graph_db::traverse_falkor_with_paths(seed, 3)
            {
                if !hops.is_empty() || !paths.is_empty() {
                    graph_backend = Some("falkor");
                }
                if !paths.is_empty() {
                    graph_paths_json = Some(serde_json::json!(
                        paths
                            .iter()
                            .map(crate::graph_db::compact_rel_path)
                            .collect::<Vec<_>>()
                    ));
                }
                graph_hop_ball_tokens = crate::search::budget::count_tokens(
                    &crate::graph_db::hop_ball_payload(&hops).to_string(),
                );
                let compact_ctx = serde_json::json!({
                    "paths": graph_paths_json.clone().unwrap_or(serde_json::json!([])),
                    "ppr": ppr_names,
                });
                graph_context_tokens =
                    crate::search::budget::count_tokens(&compact_ctx.to_string());
            }
        }
    }

    use crate::search::budget::{count_tokens, truncate_to_budget};

    let shaped: Vec<Value> = if opts.format == "compact" {
        results_json.iter().map(compact_result).collect()
    } else {
        results_json
    };
    let text_key = if opts.format == "compact" {
        "c"
    } else {
        "content"
    };

    let mut token_budget = opts.max_tokens;
    let mut final_results: Vec<Value> = Vec::with_capacity(shaped.len());
    for mut r in shaped {
        if token_budget <= 0 {
            break;
        }
        let row_tokens = count_tokens(&r.to_string()) as i64;
        if row_tokens > token_budget {
            let truncated: Option<String> = r
                .get(text_key)
                .and_then(|v| v.as_str())
                .map(|s| truncate_to_budget(s, token_budget.max(0) as usize));
            if let (Some(obj), Some(t)) = (r.as_object_mut(), truncated) {
                obj.insert(text_key.to_string(), serde_json::json!(t));
            }
            final_results.push(r);
            break;
        }
        token_budget -= row_tokens;
        final_results.push(r);
    }

    annotate_evidence(pool, &mut final_results).await;

    let fresh = match session_started {
        Some(since) => written_since(pool, &final_results, since).await,
        None => Vec::new(),
    };

    let mut response = serde_json::json!({
        "mode": "hybrid",
        "query": query,
        "query_class": match query_class {
            crate::search::query_class::QueryClass::Factoid => "factoid",
            crate::search::query_class::QueryClass::MultiHop => "multi_hop",
            crate::search::query_class::QueryClass::Global => "global",
        },
        "results": final_results,
        "count": final_results.len(),
        "graphrag_context": graphrag_context
    });
    if let Some(backend) = graph_backend {
        response["graph_backend"] = serde_json::json!(backend);
    }
    if !ppr_names.is_empty() {
        response["ppr_entities"] = serde_json::json!(ppr_names);
    }
    if let Some(paths) = graph_paths_json {
        response["graph_paths"] = paths;
    }
    if graph_context_tokens > 0 || graph_hop_ball_tokens > 0 {
        response["graph_context_tokens"] = serde_json::json!(graph_context_tokens);
        response["graph_hop_ball_tokens"] = serde_json::json!(graph_hop_ball_tokens);
        if graph_hop_ball_tokens > 0 {
            response["graph_token_ratio"] =
                serde_json::json!(graph_context_tokens as f64 / graph_hop_ball_tokens as f64);
        }
    }

    if !fresh.is_empty() {
        response["new_since_you_started"] = serde_json::json!(fresh);
        response["new_since_you_started_note"] = serde_json::json!(
            "those positions in results were written after this session opened — by another \
             agent working the same project, or by you. Indices and not ids on purpose: one \
             uuid costs as many tokens as a whole timestamp, and this list rides on every \
             search."
        );
    }

    annotate_degradation(
        &mut response,
        vector_failure.as_deref(),
        bm25_failed,
        reranker_failed,
    );

    Ok(response)
}

async fn mentioned_entities(pool: &PgPool, query: &str) -> Vec<String> {
    let names: Vec<(String,)> =
        match sqlx::query_as("SELECT name FROM brain_entities WHERE char_length(name) >= 4")
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows,
            Err(_) => return Vec::new(),
        };
    crate::search::mentions::names_in_query(query, names.iter().map(|(n,)| n.as_str()))
}

fn merge_seed_names(mentioned: Vec<String>, from_results: Vec<String>) -> Vec<String> {
    let mut out = mentioned;
    let mut seen: HashSet<String> = out.iter().map(|s| s.to_ascii_lowercase()).collect();
    for n in from_results {
        if seen.insert(n.to_ascii_lowercase()) {
            out.push(n);
        }
    }
    out.truncate(8);
    out
}

fn entity_names_from_list(results: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for r in results {
        for key in ["entity_name", "e", "name", "entity"] {
            if let Some(s) = r.get(key).and_then(|v| v.as_str()) {
                let t = s.trim();
                if !t.is_empty() && seen.insert(t.to_ascii_lowercase()) {
                    out.push(t.to_string());
                }
                break;
            }
        }
    }
    out
}

async fn entity_factoid_observations(
    pool: &PgPool,
    query: &str,
    names: &[String],
    limit: i64,
    project_id: Option<uuid::Uuid>,
) -> Vec<Value> {
    if names.is_empty() || limit <= 0 {
        return Vec::new();
    }
    let rows: Vec<(uuid::Uuid, String, String, String, f64, f64)> = match sqlx::query_as(
        "SELECT o.id, e.name, o.content, o.observation_type, o.importance::float8,
                (  (ts_rank(o.search_vector, cuba_or_tsquery($1))
                  + similarity(o.content, $1)) * 0.7
                 + o.importance::float8 * 0.3
                )::float8 AS score
         FROM brain_observations o
         JOIN brain_entities e ON o.entity_id = e.id
         WHERE e.name = ANY($2)
           AND o.observation_type != 'superseded'
           AND o.trust = 'trusted'
           AND ($4::uuid IS NULL OR o.project_id = $4 OR o.project_id IS NULL)
         ORDER BY score DESC
         LIMIT $3",
    )
    .bind(query)
    .bind(names)
    .bind(limit)
    .bind(project_id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, "entity-factoid observation fetch failed");
            return Vec::new();
        }
    };
    rows.into_iter()
        .map(|(id, entity_name, content, obs_type, importance, score)| {
            serde_json::json!({
                "id": id.to_string(),
                "type": "observation",
                "entity_name": entity_name,
                "content": content,
                "observation_type": obs_type,
                "importance": importance,
                "score": score,
                "entity_factoid": true
            })
        })
        .collect()
}

async fn observations_for_entities(pool: &PgPool, names: &[String], limit: i64) -> Vec<Value> {
    if names.is_empty() || limit <= 0 {
        return Vec::new();
    }
    let rows: Vec<(uuid::Uuid, String, String, String, f64)> = match sqlx::query_as(
        "SELECT o.id, e.name, o.content, o.observation_type, o.importance::float8
         FROM brain_observations o
         JOIN brain_entities e ON o.entity_id = e.id
         WHERE e.name = ANY($1)
           AND o.observation_type != 'superseded'
           AND o.trust = 'trusted'
         ORDER BY o.importance DESC
         LIMIT $2",
    )
    .bind(names)
    .bind(limit)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, "ppr observation expand failed");
            return Vec::new();
        }
    };
    rows.into_iter()
        .map(|(id, entity_name, content, obs_type, importance)| {
            serde_json::json!({
                "id": id.to_string(),
                "type": "observation",
                "entity_name": entity_name,
                "content": content,
                "observation_type": obs_type,
                "importance": importance,
                "score": 0.0,
                "ppr_expanded": true
            })
        })
        .collect()
}

fn compact_chars() -> usize {
    std::env::var("CUBA_COMPACT_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1200)
}

async fn annotate_evidence(pool: &PgPool, results: &mut [Value]) {
    let ids: Vec<uuid::Uuid> = results
        .iter()
        .filter_map(|r| r.get("id").or_else(|| r.get("i")))
        .filter_map(|v| v.as_str())
        .filter_map(|s| uuid::Uuid::parse_str(s).ok())
        .collect();
    if ids.is_empty() {
        return;
    }

    type Row = (uuid::Uuid, String, Option<chrono::DateTime<chrono::Utc>>);
    let stronger: Vec<Row> = sqlx::query_as(
        "SELECT id, evidence, verified_at FROM brain_observations
         WHERE id = ANY($1) AND evidence <> 'asserted'",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if stronger.is_empty() {
        return;
    }

    let by_id: std::collections::HashMap<String, Row> = stronger
        .into_iter()
        .map(|row| (row.0.to_string(), row))
        .collect();

    for r in results.iter_mut() {
        let Some(id) = r
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|id| by_id.contains_key(id))
        else {
            continue;
        };
        let (_, evidence, verified_at) = &by_id[&id];
        let Some(obj) = r.as_object_mut() else {
            continue;
        };
        obj.insert("evidence".to_string(), serde_json::json!(evidence));
        if let Some(at) = verified_at {
            obj.insert(
                "verified_at".to_string(),
                serde_json::json!(at.to_rfc3339()),
            );
        }
    }
}

fn compact_result(r: &Value) -> Value {
    let content = r
        .get("content")
        .and_then(|v| v.as_str())
        .map(|s| crate::handlers::zafra::safe_truncate(s, compact_chars()));
    serde_json::json!({
        "id": r.get("id"),
        "e": r.get("entity_name").or_else(|| r.get("name")),
        "c": content,
        "s": r.get("fused_score"),
        "t": r.get("type").or_else(|| r.get("observation_type")),
        "i": r.get("importance")
    })
}

async fn verify_claim(pool: &PgPool, claim: &str, project_id: Option<uuid::Uuid>) -> Result<Value> {
    use std::collections::HashMap;

    let trigram_evidence: Vec<(uuid::Uuid, String, f64, String, String)> = sqlx::query_as(
        "SELECT o.id, o.content, similarity(o.content, $1)::float8 AS sim,
                o.observation_type, e.name AS entity_name
         FROM brain_observations o
         JOIN brain_entities e ON o.entity_id = e.id
         WHERE similarity(o.content, $1) > 0.3
           AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
           AND ($2::uuid IS NULL OR o.project_id = $2 OR o.project_id IS NULL)
         ORDER BY sim DESC
         LIMIT 10",
    )
    .bind(claim)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let semantic_evidence: Vec<(uuid::Uuid, String, f64, String, String)> =
        if crate::embeddings::onnx::is_model_loaded() {
            match crate::embeddings::onnx::embed(claim).await {
                Ok(emb) => sqlx::query_as(
                    "SELECT o.id, o.content,
                                (1.0 - (o.embedding <=> $1::vector))::float8 AS sim,
                                o.observation_type, e.name AS entity_name
                         FROM brain_observations o
                         JOIN brain_entities e ON o.entity_id = e.id
                         WHERE o.embedding IS NOT NULL
                           AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
                           AND (o.embedding <=> $1::vector) < 0.8
                           AND ($2::uuid IS NULL OR o.project_id = $2 OR o.project_id IS NULL)
                         ORDER BY o.embedding <=> $1::vector
                         LIMIT 10",
                )
                .bind(pgvector::Vector::from(emb))
                .bind(project_id)
                .fetch_all(pool)
                .await
                .unwrap_or_default(),
                Err(e) => {
                    tracing::warn!(error = %e, "embedding failed during verify — skipping semantic evidence");
                    vec![]
                }
            }
        } else {
            vec![]
        };

    let mut merged: HashMap<uuid::Uuid, (String, f64, String, String)> = HashMap::new();
    for (id, content, sim, obs_type, entity_name) in
        trigram_evidence.iter().chain(semantic_evidence.iter())
    {
        merged
            .entry(*id)
            .and_modify(|(_, existing_sim, _, _)| *existing_sim = existing_sim.max(*sim))
            .or_insert((content.clone(), *sim, obs_type.clone(), entity_name.clone()));
    }

    let mut evidence_list: Vec<(uuid::Uuid, String, f64, String, String)> = merged
        .into_iter()
        .map(|(id, (content, sim, obs_type, entity_name))| {
            (id, content, sim, obs_type, entity_name)
        })
        .collect();
    evidence_list.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    evidence_list.retain(|(_, _, sim, _, _)| *sim >= grounding::MIN_EVIDENCE_SIMILARITY);
    evidence_list.truncate(10);

    let top_entity: Option<String> = evidence_list.first().map(|(_, _, _, _, e)| e.clone());

    let judge = crate::cognitive::judge::resolve_judge();
    let max_judged = crate::cognitive::judge::default_max_pairs();
    let to_judge: Vec<(usize, &String, f64)> = evidence_list
        .iter()
        .take(max_judged)
        .enumerate()
        .map(|(i, (_, content, sim, _, _))| (i, content, *sim))
        .collect();

    let judgments = futures::future::join_all(to_judge.iter().map(|(i, content, sim)| {
        let judge = &judge;
        async move { (*i, *sim, judge.judge_claim(claim, content).await) }
    }))
    .await;

    let mut judged: Vec<grounding::JudgedEvidence> = Vec::with_capacity(judgments.len());
    let mut verdicts: HashMap<usize, Value> = HashMap::with_capacity(judgments.len());

    for (i, sim, result) in judgments {
        let judgment = match result {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    backend = judge.backend_name(),
                    "judge unavailable for this evidence — it will not count as support"
                );
                continue;
            }
        };
        verdicts.insert(
            i,
            serde_json::json!({
                "verdict": judgment.verdict,
                "reason": judgment.reason,
            }),
        );
        judged.push(grounding::JudgedEvidence {
            similarity: sim,
            verdict: judgment.verdict,
            judge_confidence: judgment.confidence,
        });
    }

    let (confidence, level) = grounding::compute_grounding_judged(&judged);

    sqlx::query(
        "INSERT INTO brain_verify_log (claim, entity_name, confidence, grounding_level)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(claim)
    .bind(top_entity.as_deref())
    .bind(confidence)
    .bind(level)
    .execute(pool)
    .await
    .ok();

    let calibration: Option<(f64,)> = sqlx::query_as(
        "SELECT (COUNT(*) FILTER (WHERE outcome = 'correct') + 1)::float8 /
                (COUNT(*) FILTER (WHERE outcome = 'correct') + COUNT(*) FILTER (WHERE outcome = 'incorrect') + 2)::float8
         FROM brain_verify_log
         WHERE grounding_level = $1 AND outcome != 'pending'"
    )
    .bind(level)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    let evidence_json: Vec<Value> = evidence_list
        .iter()
        .enumerate()
        .map(|(i, (_, content, sim, obs_type, entity_name))| {
            let mut e = serde_json::json!({
                "content": content,
                "similarity": sim,
                "type": obs_type,
                "entity": entity_name
            });
            if let Some(v) = verdicts.get(&i) {
                e["verdict"] = v["verdict"].clone();
                e["reason"] = v["reason"].clone();
            }
            e
        })
        .collect();

    let interpretation = match level {
        "contradicted" => "The stored evidence CONTRADICTS this claim.",
        "verified" => "The stored evidence supports this claim.",
        "partial" | "weak" => "Partial support — some evidence backs this claim, not decisively.",
        _ if judged.is_empty() && evidence_list.is_empty() => {
            "Nothing in memory relates to this claim. No grounding — treat as unverified."
        }
        _ => {
            "Memory holds related material, but none of it asserts this claim. \
             No grounding — being on-topic is not support."
        }
    };

    let mut response = serde_json::json!({
        "mode": "verify",
        "claim": claim,
        "confidence": confidence,
        "grounding": level,
        "interpretation": interpretation,
        "evidence": evidence_json,
        "evidence_count": evidence_json.len(),
        "judged_by": judge.backend_name(),
        "verdicts_reached": judged.len()
    });

    if let Some((cal,)) = calibration {
        response["calibrated_accuracy"] = serde_json::json!(cal);
    }

    Ok(response)
}

async fn text_search(
    pool: &PgPool,
    query: &str,
    scope: &str,
    limit: i64,
    tb: &TimeBounds,
    project_id: Option<uuid::Uuid>,
) -> Result<Vec<Value>> {
    let mut results = Vec::new();

    if scope == "all" || scope == "entities" {
        let rows: Vec<(uuid::Uuid, String, String, f64, f64)> = sqlx::query_as(
            "SELECT id, name, entity_type, importance::float8,
                    (  (ts_rank(search_vector, cuba_or_tsquery($1))
                      + similarity(name, $1)) * 0.7
                     + importance::float8 * 0.3
                    )::float8 AS score
             FROM brain_entities
             WHERE (search_vector @@ cuba_or_tsquery($1)
                OR similarity(name, $1) > 0.3)
               AND created_at >= $3 AND created_at <= $4
               AND ($5::uuid IS NULL OR project_id = $5 OR project_id IS NULL)
             ORDER BY score DESC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .bind(tb.after)
        .bind(tb.before)
        .bind(project_id)
        .fetch_all(pool)
        .await?;
        results.extend(
            rows.into_iter()
                .map(|(id, name, entity_type, importance, score)| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "type": "entity",
                        "name": name,
                        "entity_type": entity_type,
                        "importance": importance,
                        "score": score
                    })
                }),
        );
    }

    if scope == "all" || scope == "observations" {
        let rows: Vec<(uuid::Uuid, String, String, String, f64, f64)> = sqlx::query_as(
            "SELECT o.id, e.name, o.content, o.observation_type, o.importance::float8,
                    (  (ts_rank(o.search_vector, cuba_or_tsquery($1))
                      + similarity(o.content, $1)) * 0.7
                     + o.importance::float8 * 0.3
                    )::float8 AS score
             FROM brain_observations o
             JOIN brain_entities e ON o.entity_id = e.id
             WHERE (o.search_vector @@ cuba_or_tsquery($1)
                OR similarity(o.content, $1) > 0.3)
               AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
               AND o.created_at >= $3 AND o.created_at <= $4
               AND ($5::uuid IS NULL OR o.project_id = $5 OR o.project_id IS NULL)
             ORDER BY score DESC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .bind(tb.after)
        .bind(tb.before)
        .bind(project_id)
        .fetch_all(pool)
        .await?;
        results.extend(rows.into_iter().map(
            |(id, entity_name, content, obs_type, importance, score)| {
                serde_json::json!({
                    "id": id.to_string(),
                    "type": "observation",
                    "entity_name": entity_name,
                    "content": content,
                    "observation_type": obs_type,
                    "importance": importance,
                    "score": score
                })
            },
        ));
    }

    if scope == "all" || scope == "errors" {
        let rows: Vec<(uuid::Uuid, String, String, bool, f64)> = sqlx::query_as(
            "SELECT id, error_type, error_message, resolved,
                    (ts_rank(search_vector, cuba_or_tsquery($1)) +
                    similarity(error_message, $1))::float8 AS score
             FROM brain_errors
             WHERE (search_vector @@ cuba_or_tsquery($1)
                OR similarity(error_message, $1) > 0.3)
               AND created_at >= $3 AND created_at <= $4
               AND ($5::uuid IS NULL OR project_id = $5 OR project_id IS NULL)
             ORDER BY score DESC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .bind(tb.after)
        .bind(tb.before)
        .bind(project_id)
        .fetch_all(pool)
        .await?;
        results.extend(
            rows.into_iter()
                .map(|(id, error_type, error_message, resolved, score)| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "type": "error",
                        "error_type": error_type,
                        "error_message": error_message,
                        "resolved": resolved,
                        "score": score
                    })
                }),
        );
    }

    if scope == "all" {
        let rows: Vec<(uuid::Uuid, String, String, f64, f64)> = sqlx::query_as(
            "SELECT ep.id, e.name, ep.content, ep.importance::float8,
                    (  (ts_rank(ep.search_vector, cuba_or_tsquery($1))
                      + similarity(ep.content, $1)) * 0.7
                     + ep.importance::float8 * 0.3
                    )::float8 AS score
             FROM brain_episodes ep
             JOIN brain_entities e ON ep.entity_id = e.id
             WHERE (ep.search_vector @@ cuba_or_tsquery($1)
                OR similarity(ep.content, $1) > 0.3)
               AND ep.created_at >= $3 AND ep.created_at <= $4
               AND ($5::uuid IS NULL OR ep.project_id = $5 OR ep.project_id IS NULL)
               AND ep.trust = 'trusted'
             ORDER BY score DESC
             LIMIT $2",
        )
        .bind(query)
        .bind(limit)
        .bind(tb.after)
        .bind(tb.before)
        .bind(project_id)
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| {
            tracing::error!(
                error = %e,
                "EPISODE LEXICAL SEARCH FAILED — the ranking carries observations only. \
                 No episode can surface for this query and nothing in the response says so."
            );
            Vec::new()
        });
        results.extend(
            rows.into_iter()
                .map(|(id, entity_name, content, importance, score)| {
                    serde_json::json!({
                        "id": id.to_string(),
                        "type": "episode",
                        "entity_name": entity_name,
                        "content": content,
                        "importance": importance,
                        "score": score
                    })
                }),
        );
    }

    Ok(results)
}

async fn vector_search(
    pool: &PgPool,
    query: &str,
    _scope: &str,
    limit: i64,
    tb: &TimeBounds,
    project_id: Option<uuid::Uuid>,
) -> Result<Vec<Value>> {
    if !crate::embeddings::onnx::is_model_loaded() {
        anyhow::bail!(
            "no hay modelo de embeddings cargado (ONNX_MODEL_PATH ausente o sin librería de \
             ONNX Runtime); la búsqueda vectorial no puede ejecutarse"
        );
    }

    let embedding = crate::embeddings::onnx::embed(query)
        .await
        .context("ONNX embed failed in vector_search")?;

    let observations: Vec<(uuid::Uuid, String, String, f64, f64)> = sqlx::query_as(
        "WITH direct AS (
             SELECT o.id, e.name, o.content, o.importance::float8 AS importance,
                    1.0 - (o.embedding <=> $1::vector) AS cosine_sim
             FROM brain_observations o
             JOIN brain_entities e ON o.entity_id = e.id
             WHERE o.embedding IS NOT NULL
               AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
               AND o.created_at >= $3 AND o.created_at <= $4
               AND ($5::uuid IS NULL OR o.project_id = $5 OR o.project_id IS NULL)
             ORDER BY o.embedding <=> $1::vector
             LIMIT $2
         ),
         via_chunk AS (
             SELECT DISTINCT ON (o.id)
                    o.id, e.name, o.content, o.importance::float8 AS importance,
                    1.0 - (c.embedding <=> $1::vector) AS cosine_sim
             FROM brain_observation_chunks c
             JOIN brain_observations o ON o.id = c.observation_id
             JOIN brain_entities e ON o.entity_id = e.id
             WHERE c.embedding IS NOT NULL
               AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
               AND o.created_at >= $3 AND o.created_at <= $4
               AND ($5::uuid IS NULL OR o.project_id = $5 OR o.project_id IS NULL)
             ORDER BY o.id, c.embedding <=> $1::vector
         )
         SELECT id, name, content, importance, cosine_sim FROM (
             SELECT DISTINCT ON (id) id, name, content, importance, cosine_sim
             FROM (SELECT * FROM direct UNION ALL SELECT * FROM via_chunk) merged
             ORDER BY id, cosine_sim DESC
         ) deduped
         ORDER BY cosine_sim DESC
         LIMIT $2",
    )
    .bind(pgvector::Vector::from(embedding.clone()))
    .bind(limit)
    .bind(tb.after)
    .bind(tb.before)
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let mut results: Vec<Value> = observations
        .iter()
        .map(|(id, entity_name, content, importance, sim)| {
            serde_json::json!({
                "id": id.to_string(),
                "type": "observation",
                "entity_name": entity_name,
                "content": content,
                "importance": importance,
                "cosine_similarity": sim
            })
        })
        .collect();

    let episodes: Vec<(uuid::Uuid, String, String, f64)> = sqlx::query_as(
        "SELECT ep.id, e.name, ep.content,
                1.0 - (ep.embedding <=> $1::vector) AS cosine_sim
         FROM brain_episodes ep
         JOIN brain_entities e ON ep.entity_id = e.id
         WHERE ep.embedding IS NOT NULL
           AND ep.created_at >= $3 AND ep.created_at <= $4
           AND ($5::uuid IS NULL OR ep.project_id = $5 OR ep.project_id IS NULL)
           AND ep.trust = 'trusted'
         ORDER BY ep.embedding <=> $1::vector
         LIMIT $2",
    )
    .bind(pgvector::Vector::from(embedding))
    .bind(limit)
    .bind(tb.after)
    .bind(tb.before)
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_else(|e| {
        tracing::error!(
            error = %e,
            "EPISODE VECTOR SEARCH FAILED — the semantic half of the ranking carries \
             observations only. Usual cause is the same as for observations: the episode \
             embedding column disagrees with the model's dimension. Run `cuba-memorys doctor`."
        );
        Vec::new()
    });

    results.extend(episodes.iter().map(|(id, entity_name, content, sim)| {
        serde_json::json!({
            "id": id.to_string(),
            "type": "episode",
            "entity_name": entity_name,
            "content": content,
            "cosine_similarity": sim
        })
    }));

    Ok(results)
}

fn entropy_weights(entropy: f64) -> (f64, f64) {
    let midpoint = 2.75;
    let k = 2.0;
    let t = 1.0 / (1.0 + (-k * (entropy - midpoint)).exp());
    let text_w = 0.7 - 0.4 * t;
    let vector_w = 0.3 + 0.4 * t;
    (text_w, vector_w)
}

async fn associative_expand(
    pool: &PgPool,
    query: &str,
    project_id: Option<uuid::Uuid>,
    fused: &mut HashMap<String, FusedResult>,
) {
    const SEED_K: i64 = 5;
    const EXPAND_ENTITIES: usize = 8;
    const OBS_PER_ENTITY: i64 = 2;
    const MAX_HOPS: usize = 2;
    const ASSOC_WEIGHT: f64 = 0.02;

    let seeds: Vec<(uuid::Uuid,)> = match sqlx::query_as(
        "SELECT id FROM brain_entities
         WHERE (search_vector @@ cuba_or_tsquery($1) OR similarity(name, $1) > 0.3)
           AND ($2::uuid IS NULL OR project_id = $2 OR project_id IS NULL)
         ORDER BY importance DESC
         LIMIT $3",
    )
    .bind(query)
    .bind(project_id)
    .bind(SEED_K)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "associative: seed query failed — skipping");
            return;
        }
    };
    if seeds.is_empty() {
        return;
    }
    let seed_ids: Vec<uuid::Uuid> = seeds.iter().map(|(id,)| *id).collect();
    let seed_set: std::collections::HashSet<uuid::Uuid> = seed_ids.iter().copied().collect();

    let activated =
        match crate::graph::activation::spread_from_entities(pool, &seed_ids, MAX_HOPS).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "associative: activation spread failed — skipping");
                return;
            }
        };

    for (entity_id, activation) in activated
        .into_iter()
        .filter(|(id, _)| !seed_set.contains(id))
        .take(EXPAND_ENTITIES)
    {
        let obs: Vec<(uuid::Uuid, String, String, String, f64)> = match sqlx::query_as(
            "SELECT o.id, e.name, o.content, o.observation_type, o.importance::float8
             FROM brain_observations o
             JOIN brain_entities e ON e.id = o.entity_id
             WHERE o.entity_id = $1
               AND o.observation_type != 'superseded'
               AND o.trust = 'trusted'
               AND ($2::uuid IS NULL OR o.project_id = $2 OR o.project_id IS NULL)
             ORDER BY o.importance DESC
             LIMIT $3",
        )
        .bind(entity_id)
        .bind(project_id)
        .bind(OBS_PER_ENTITY)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "associative: obs fetch failed — skipping entity");
                continue;
            }
        };

        for (id, entity_name, content, obs_type, importance) in obs {
            let id_str = id.to_string();
            if fused.contains_key(&id_str) {
                continue;
            }
            let assoc_score = ASSOC_WEIGHT * activation as f64 * importance;
            fused.insert(
                id_str.clone(),
                FusedResult {
                    text_score: 0.0,
                    vector_score: 0.0,
                    bm25_score: 0.0,
                    session_boosted: false,
                    total: assoc_score,
                    data: serde_json::json!({
                        "id": id_str,
                        "type": "observation",
                        "entity_name": entity_name,
                        "content": content,
                        "observation_type": obs_type,
                        "importance": importance,
                        "score": assoc_score,
                        "associative": true
                    }),
                },
            );
        }
    }
}

type SessionContext = (Vec<String>, Option<DateTime<Utc>>);

async fn session_context(pool: &PgPool) -> Result<SessionContext> {
    let Some(sid) = crate::session::session_id() else {
        return Ok((Vec::new(), None));
    };
    let row: Option<(serde_json::Value, DateTime<Utc>)> = sqlx::query_as(
        "SELECT goals, started_at FROM brain_sessions WHERE id = $1 AND ended_at IS NULL",
    )
    .bind(sid)
    .fetch_optional(pool)
    .await?;

    let (mut boosts, started) = match row {
        Some((goals, started_at)) => (
            serde_json::from_value(goals).unwrap_or_default(),
            Some(started_at),
        ),
        None => (Vec::new(), None),
    };
    let wm: Vec<(String,)> = sqlx::query_as(
        "SELECT content FROM brain_wm
         WHERE expires_at > NOW()
           AND session_id IS NOT DISTINCT FROM $1
         ORDER BY created_at DESC
         LIMIT 20",
    )
    .bind(sid)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    for (content,) in wm {
        if !content.trim().is_empty() {
            boosts.push(content);
        }
    }
    Ok((boosts, started))
}

async fn written_since(pool: &PgPool, results: &[Value], since: DateTime<Utc>) -> Vec<usize> {
    let ids: Vec<uuid::Uuid> = results
        .iter()
        .filter_map(|r| r.get("id").or_else(|| r.get("i")))
        .filter_map(|v| v.as_str())
        .filter_map(|s| uuid::Uuid::parse_str(s).ok())
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }

    let fresh: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM brain_observations WHERE id = ANY($1) AND created_at > $2
         UNION ALL
         SELECT id FROM brain_episodes WHERE id = ANY($1) AND started_at > $2
         UNION ALL
         SELECT id FROM brain_entities WHERE id = ANY($1) AND created_at > $2",
    )
    .bind(&ids)
    .bind(since)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if fresh.is_empty() {
        return Vec::new();
    }

    let fresh: std::collections::HashSet<String> =
        fresh.into_iter().map(|(id,)| id.to_string()).collect();
    results
        .iter()
        .enumerate()
        .filter_map(|(at, r)| {
            let id = r.get("id").or_else(|| r.get("i"))?.as_str()?;
            fresh.contains(id).then_some(at)
        })
        .collect()
}

async fn enrich_graphrag(pool: &PgPool, results: &[(String, FusedResult)], top_k: usize) -> Value {
    let mut entity_names: Vec<String> = Vec::new();
    for (_, fr) in results {
        let Some(name) = fr
            .data
            .get("entity_name")
            .or_else(|| fr.data.get("name"))
            .and_then(|v: &Value| v.as_str())
        else {
            continue;
        };
        if entity_names.iter().any(|n| n == name) {
            continue;
        }
        entity_names.push(name.to_string());
        if entity_names.len() == top_k {
            break;
        }
    }

    if entity_names.is_empty() {
        return serde_json::json!([]);
    }

    let rows: Vec<(String, String, String, f64)> = match sqlx::query_as(
        "WITH src AS (SELECT id, name FROM brain_entities WHERE name = ANY($1)),
              ranked AS (
                  SELECT src.name AS source, e.name AS neighbor, r.relation_type,
                         e.importance::float8 AS importance,
                         ROW_NUMBER() OVER (
                             PARTITION BY src.name ORDER BY r.strength DESC
                         ) AS rn
                  FROM brain_relations r
                  JOIN src ON r.from_entity = src.id OR r.to_entity = src.id
                  JOIN brain_entities e ON e.id = CASE
                      WHEN r.from_entity = src.id THEN r.to_entity
                      ELSE r.from_entity
                  END
              )
         SELECT source, neighbor, relation_type, importance FROM ranked WHERE rn <= 5",
    )
    .bind(&entity_names)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                error = %e,
                entities = ?entity_names,
                "graphrag neighbours query failed — the response will carry an empty context,                  which reads exactly like these entities having no neighbours"
            );
            return serde_json::json!([]);
        }
    };

    let mut by_entity: HashMap<String, Vec<Value>> = HashMap::new();
    for (source, neighbor, relation_type, importance) in rows {
        by_entity
            .entry(source)
            .or_default()
            .push(serde_json::json!({
                "name": neighbor, "relation": relation_type, "importance": importance
            }));
    }

    let context: Vec<Value> = entity_names
        .into_iter()
        .filter_map(|name| {
            by_entity
                .remove(&name)
                .map(|neighbors| serde_json::json!({"entity": name, "neighbors": neighbors}))
        })
        .collect();

    serde_json::json!(context)
}

fn env_threshold() -> Option<f64> {
    std::env::var("CUBA_OOD_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|t| t.is_finite() && *t > 0.0)
}

fn threshold_cache() -> &'static std::sync::Mutex<TtlLruCache<Option<f64>>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<TtlLruCache<Option<f64>>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(TtlLruCache::new()))
}

fn cached_threshold(dim: usize) -> Option<Option<f64>> {
    threshold_cache().lock().ok()?.get(&dim.to_string())
}

fn remember_threshold(dim: usize, value: Option<f64>) {
    if let Ok(mut cache) = threshold_cache().lock() {
        cache.put(dim.to_string(), value);
    }
}

async fn calibrated_threshold(pool: &PgPool, dim: usize) -> Option<f64> {
    if let Some(hit) = cached_threshold(dim) {
        return hit;
    }
    let loaded = crate::search::calibrate::load_ood_threshold(pool, dim).await;
    remember_threshold(dim, loaded);
    loaded
}

fn ood_abstain_json(query: &str, threshold: f64, dist: f64) -> Option<Value> {
    if dist <= threshold {
        return None;
    }
    Some(serde_json::json!({
        "mode": "hybrid",
        "query": query,
        "results": [],
        "count": 0,
        "ood": true,
        "mahalanobis_distance": dist,
        "ood_threshold": threshold,
        "abstain_reason": format!(
            "Query is out of distribution (distance {:.2} > threshold {:.2}). \
             No relevant memory found — consider rephrasing or adding the topic explicitly.",
            dist, threshold
        ),
        "graphrag_context": []
    }))
}

fn fit_semaphore() -> &'static tokio::sync::Semaphore {
    static SEM: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SEM.get_or_init(|| tokio::sync::Semaphore::new(1))
}

async fn fit_ood_stats(embeddings: Vec<Vec<f32>>) -> Option<crate::search::ood::OodStats> {
    tokio::task::spawn_blocking(move || crate::search::ood::OodStats::fit(&embeddings))
        .await
        .inspect_err(|e| {
            tracing::error!(
                error = %e,
                "the OOD fit task died — abstention is off for this query, which answers \
                 instead of abstaining"
            );
        })
        .ok()?
}

async fn check_ood(
    pool: &PgPool,
    query: &str,
    threshold: Option<f64>,
    project_id: Option<uuid::Uuid>,
) -> Option<Value> {
    use crate::search::ood::{MIN_SAMPLES_FOR_OOD, default_threshold};

    if !crate::embeddings::onnx::is_model_loaded() {
        return None;
    }
    let query_emb = crate::embeddings::onnx::embed_passage(query).await.ok()?;
    let tau = match threshold.or_else(env_threshold) {
        Some(t) => t,
        None => calibrated_threshold(pool, query_emb.len())
            .await
            .unwrap_or_else(|| default_threshold(query_emb.len())),
    };

    if let Some(stats) = crate::search::ood_cache::get(project_id)
        && let Some(dist) = stats.mahalanobis(&query_emb)
    {
        return ood_abstain_json(query, tau, dist);
    }

    let _permit = fit_semaphore().acquire().await.ok()?;
    if let Some(stats) = crate::search::ood_cache::get(project_id)
        && let Some(dist) = stats.mahalanobis(&query_emb)
    {
        return ood_abstain_json(query, tau, dist);
    }

    let raw: Vec<(pgvector::Vector,)> = sqlx::query_as(
        "SELECT embedding FROM brain_observations
         WHERE embedding IS NOT NULL AND observation_type != 'superseded'
           AND trust = 'trusted'
           AND ($1::uuid IS NULL OR project_id = $1 OR project_id IS NULL)
         ORDER BY id
         LIMIT $2",
    )
    .bind(project_id)
    .bind(crate::resources::ood_fit_limit())
    .fetch_all(pool)
    .await
    .ok()?;

    if raw.len() < MIN_SAMPLES_FOR_OOD {
        return None;
    }
    let embeddings: Vec<Vec<f32>> = raw.into_iter().map(|(v,)| v.to_vec()).collect();
    let stats = std::sync::Arc::new(fit_ood_stats(embeddings).await?);
    crate::search::ood_cache::store(project_id, std::sync::Arc::clone(&stats));
    let dist = stats.mahalanobis(&query_emb)?;
    ood_abstain_json(query, tau, dist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_branch_that_silently_degrades_says_so_in_the_response() {
        let mut clean = serde_json::json!({"results": []});
        annotate_degradation(&mut clean, None, false, false);
        assert_eq!(
            clean,
            serde_json::json!({"results": []}),
            "a healthy search must not carry degradation noise"
        );

        for (vector, bm25, rerank, key) in [
            (Some("no model loaded"), false, false, "degraded"),
            (None, true, false, "bm25_degraded"),
            (None, false, true, "reranker_degraded"),
        ] {
            let mut response = serde_json::json!({"results": []});
            annotate_degradation(&mut response, vector, bm25, rerank);
            assert_eq!(
                response.get(key).and_then(serde_json::Value::as_bool),
                Some(true),
                "{key} must reach the caller. A retrieval branch that fails and returns an \
                 empty list is indistinguishable from a corpus that had no answer, and the \
                 caller will treat a degraded search as a confident miss"
            );
            let reason = format!("{key}_reason");
            assert!(
                response
                    .get(&reason)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|r| !r.trim().is_empty()),
                "{reason} must explain what was lost, not just that something was"
            );
        }
    }

    #[test]
    fn a_negative_limit_is_clamped_to_one_row_instead_of_reaching_postgres() {
        assert_eq!(
            requested_limit(&serde_json::json!({"limit": -5})),
            1,
            "limit=-5 must never leave this function: Postgres rejects `LIMIT -5` outright, \
             and bm25_search truncates with `limit as usize`, which turns -5 into \
             18446744073709551611 and truncates nothing"
        );
        assert_eq!(
            requested_limit(&serde_json::json!({"limit": 0})),
            1,
            "limit=0 must return one row, not an empty ranking the caller cannot tell from \
             a genuine miss"
        );
    }

    #[test]
    fn the_limit_clamp_still_honours_the_default_and_the_ceiling() {
        assert_eq!(
            requested_limit(&serde_json::json!({})),
            DEFAULT_LIMIT,
            "an absent limit must keep using DEFAULT_LIMIT"
        );
        assert_eq!(
            requested_limit(&serde_json::json!({"limit": 999})),
            MAX_LIMIT,
            "the upper bound is what keeps one client from paging the whole corpus into a \
             single response"
        );
        assert_eq!(
            requested_limit(&serde_json::json!({"limit": 7})),
            7,
            "a limit inside the range must pass through untouched"
        );
    }

    #[test]
    fn an_unparseable_after_bound_is_rejected_instead_of_silently_widening_the_window() {
        let err = parse_time_bounds(None, Some("ayer"))
            .expect_err("`ayer` is not RFC 3339 and must not be accepted");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ayer"),
            "the error must quote the offending value so the caller sees what it sent: {msg}"
        );
        assert!(
            msg.contains("RFC 3339"),
            "the error must name the expected format, otherwise the caller retries blind: {msg}"
        );
    }

    #[test]
    fn an_unparseable_before_bound_is_rejected_too() {
        let err = parse_time_bounds(Some("2026-13-45"), None)
            .expect_err("month 13 / day 45 is not a date and must not be accepted");
        assert!(
            format!("{err:#}").contains("2026-13-45"),
            "an out-of-range date used to fall back to now+365d, which is an unbounded \
             search dressed up as a bounded one"
        );
    }

    #[test]
    fn valid_bounds_parse_and_absent_bounds_stay_open() {
        let bounds = parse_time_bounds(Some("2026-08-11T00:00:00Z"), Some("2026-08-01T00:00:00Z"))
            .expect("both values are RFC 3339");
        assert_eq!(bounds.after.to_rfc3339(), "2026-08-01T00:00:00+00:00");
        assert_eq!(bounds.before.to_rfc3339(), "2026-08-11T00:00:00+00:00");

        let open = parse_time_bounds(None, None).expect("absent bounds are not an error");
        assert_eq!(
            open.after.timestamp(),
            0,
            "no `after` still means the whole history"
        );
        assert!(
            open.before > chrono::Utc::now(),
            "no `before` still means the open future"
        );
    }

    #[test]
    fn a_threshold_cached_for_one_embedding_dimension_never_answers_for_another() {
        remember_threshold(384, Some(21.25));
        assert_eq!(
            cached_threshold(384),
            Some(Some(21.25)),
            "the dimension it was measured for must hit the cache"
        );
        assert_eq!(
            cached_threshold(1024),
            None,
            "a Mahalanobis threshold is a distance in the space it was calibrated on: serving \
             the 384-d value to a 1024-d query (e5-small vs the bge-m3 production model) \
             abstains on everything or on nothing"
        );
    }

    #[test]
    fn a_failed_threshold_load_is_cached_as_a_miss_not_as_an_absent_entry() {
        remember_threshold(777, None);
        assert_eq!(
            cached_threshold(777),
            Some(None),
            "a load that found no calibration row must be distinguishable from a cold cache, \
             or every query re-queries brain_calibration"
        );
    }

    fn deterministic_embeddings(n: usize, d: usize) -> Vec<Vec<f32>> {
        (0..n)
            .map(|i| {
                (0..d)
                    .map(|j| (((i * 31 + j * 17) % 97) as f32) / 97.0)
                    .collect()
            })
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn fitting_ood_stats_leaves_the_single_runtime_worker_free_for_other_tasks() {
        let embeddings = deterministic_embeddings(400, 128);

        let (fitted, fit_elapsed, probe_latency) = tokio::spawn(async move {
            let spawned_at = std::time::Instant::now();
            let probe = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                spawned_at.elapsed()
            });

            let started = std::time::Instant::now();
            let stats = fit_ood_stats(embeddings).await;
            let fit_elapsed = started.elapsed();
            (
                stats.is_some(),
                fit_elapsed,
                probe.await.expect("the probe task must not panic"),
            )
        })
        .await
        .expect("the fitting task must not panic");

        assert!(fitted, "400 samples of 128 dimensions must fit");
        assert!(
            fit_elapsed > std::time::Duration::from_millis(50),
            "the fit has to stay slow enough for a stalled worker to be measurable; it took \
             {fit_elapsed:?}, so raise the sample size or this test cannot fail"
        );
        assert!(
            probe_latency * 2 < fit_elapsed,
            "a 10 ms sleep spawned before the fit resolved after {probe_latency:?} while the \
             fit ran for {fit_elapsed:?}: the fit is running on the runtime worker instead of \
             spawn_blocking, so every other task on this runtime waits for it (measured 11,4 s \
             with n=1811 in production)"
        );
    }

    #[test]
    fn test_entropy_weights_sum_to_one() {
        for &e in &[0.0f64, 1.0, 2.0, 2.75, 3.5, 5.0, 10.0] {
            let (tw, vw) = entropy_weights(e);
            assert!(
                (tw + vw - 1.0).abs() < 1e-12,
                "weights must sum to 1.0 at entropy={e}: got {tw}+{vw}={:.15}",
                tw + vw
            );
        }
    }

    #[test]
    fn test_entropy_weights_monotone() {
        let entropies = [0.0f64, 0.5, 1.0, 1.5, 2.0, 2.5, 2.75, 3.0, 3.5, 4.0, 5.0];
        let weights: Vec<(f64, f64)> = entropies.iter().map(|&e| entropy_weights(e)).collect();
        for i in 1..weights.len() {
            let (tw_prev, vw_prev) = weights[i - 1];
            let (tw_curr, vw_curr) = weights[i];
            assert!(
                tw_curr < tw_prev,
                "text_w must decrease: at e={} got {tw_curr} >= prev {tw_prev}",
                entropies[i]
            );
            assert!(
                vw_curr > vw_prev,
                "vector_w must increase: at e={} got {vw_curr} <= prev {vw_prev}",
                entropies[i]
            );
        }
    }

    #[test]
    fn test_entropy_weights_asymptotes() {
        let (tw_low, vw_low) = entropy_weights(0.0);
        assert!(
            tw_low > 0.68,
            "low entropy should be text-heavy: got text_w={tw_low}"
        );
        assert!(
            vw_low < 0.32,
            "low entropy should minimize vector_w: got {vw_low}"
        );

        let (tw_high, vw_high) = entropy_weights(10.0);
        assert!(
            tw_high < 0.32,
            "high entropy should minimize text_w: got {tw_high}"
        );
        assert!(
            vw_high > 0.68,
            "high entropy should be vector-heavy: got {vw_high}"
        );

        let (tw_mid, vw_mid) = entropy_weights(2.75);
        assert!(
            (tw_mid - 0.5).abs() < 1e-10,
            "midpoint should give text_w=0.5: got {tw_mid}"
        );
        assert!(
            (vw_mid - 0.5).abs() < 1e-10,
            "midpoint should give vector_w=0.5: got {vw_mid}"
        );
    }

    #[test]
    fn test_entropy_weights_no_discontinuity() {
        for &threshold in &[2.0f64, 3.5] {
            let (tw_before, _) = entropy_weights(threshold - 0.1);
            let (tw_after, _) = entropy_weights(threshold + 0.1);
            let jump = (tw_before - tw_after).abs();
            assert!(
                jump < 0.05,
                "sigmoid should be smooth: jump of {jump:.4} at threshold {threshold} (V2 had ~0.20)"
            );
        }
    }
}
