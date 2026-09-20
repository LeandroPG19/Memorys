use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let metric = args
        .get("metric")
        .and_then(|v| v.as_str())
        .unwrap_or("summary");

    match metric {
        "summary" => summary(pool).await,
        "health" => health(pool).await,
        "drift" => drift(pool).await,
        "communities" => communities(pool).await,
        "bridges" => bridges(pool).await,
        "structural" => structural(pool).await,
        _ => anyhow::bail!("Invalid metric: {metric}"),
    }
}

async fn structural(pool: &PgPool) -> Result<Value> {
    let centrality = crate::graph::closeness::compute_top(pool, 20).await?;
    let kcore = crate::graph::kcore::compute_top(pool, 20).await?;
    Ok(serde_json::json!({
        "metric": "structural",
        "closeness_harmonic": centrality.iter().map(|(name, h, c)| {
            serde_json::json!({"entity": name, "harmonic": h, "closeness": c})
        }).collect::<Vec<_>>(),
        "kcore": kcore.iter().map(|(name, k)| {
            serde_json::json!({"entity": name, "kcore": k})
        }).collect::<Vec<_>>(),
        "interpretation": "harmonic robust for disconnected graphs (Boldi-Vigna 2014); closeness uses Bavelas 1950; kcore identifies structural backbone (Seidman 1983)"
    }))
}

async fn summary(pool: &PgPool) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;

    let entities: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_entities
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    let observations: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_observations
         WHERE observation_type != 'superseded'
           AND ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    let relations: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_relations
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    let errors: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_errors
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    let sessions: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_sessions
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    let episodes: i64 = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM brain_episodes
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .map(|(c,)| c)
    .unwrap_or(0);
    let token_estimate = (observations.0 + episodes) * 50;

    Ok(serde_json::json!({
        "metric": "summary",
        "entities": entities.0,
        "observations": observations.0,
        "episodes": episodes,
        "relations": relations.0,
        "errors": errors.0,
        "sessions": sessions.0,
        "estimated_tokens": token_estimate,
        "project_scoped": project_id.is_some()
    }))
}

async fn health(pool: &PgPool) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;

    let avg_importance: (Option<f64>,) = sqlx::query_as(
        "SELECT AVG(importance)::float8 FROM brain_entities
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    let stale_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_observations
         WHERE last_accessed < NOW() - INTERVAL '30 days'
           AND ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    let unused_entities: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_entities
         WHERE access_count = 0
           AND ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    let db_size: (String,) =
        sqlx::query_as("SELECT pg_size_pretty(pg_database_size(current_database()))")
            .fetch_one(pool)
            .await?;

    let type_counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT entity_type, COUNT(*) FROM brain_entities
         WHERE ($1::uuid IS NULL OR project_id = $1)
         GROUP BY entity_type",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let entity_entropy = compute_entropy(&type_counts);
    let max_entity_entropy = if type_counts.is_empty() {
        1.0
    } else {
        (type_counts.len() as f64).log2().max(0.001)
    };

    let obs_counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT observation_type, COUNT(*) FROM brain_observations
         WHERE ($1::uuid IS NULL OR project_id = $1)
         GROUP BY observation_type",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let obs_entropy = compute_entropy(&obs_counts);
    let max_obs_entropy = if obs_counts.is_empty() {
        1.0
    } else {
        (obs_counts.len() as f64).log2().max(0.001)
    };

    let diversity_score =
        ((entity_entropy / max_entity_entropy) + (obs_entropy / max_obs_entropy)) / 2.0;

    let err_stats: (i64, i64, Option<f64>) = sqlx::query_as(
        "SELECT \
            (SELECT COUNT(*) FROM brain_errors WHERE resolved \
              AND ($1::uuid IS NULL OR project_id = $1)), \
            (SELECT COUNT(*) FROM brain_errors \
              WHERE ($1::uuid IS NULL OR project_id = $1)), \
            (SELECT AVG(EXTRACT(EPOCH FROM (resolved_at - created_at)))::float8 \
             FROM brain_errors WHERE resolved AND resolved_at IS NOT NULL \
               AND ($1::uuid IS NULL OR project_id = $1))",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await?;

    let resolution_rate = if err_stats.1 > 0 {
        err_stats.0 as f64 / err_stats.1 as f64
    } else {
        0.0
    };

    let null_embeddings: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM brain_observations
         WHERE embedding IS NULL AND observation_type != 'superseded'
           AND ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_one(pool)
    .await
    .unwrap_or((0,));

    let active_triggers: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM brain_triggers WHERE active = TRUE")
            .fetch_one(pool)
            .await
            .unwrap_or((0,));

    let obs_table_size: (String,) =
        sqlx::query_as("SELECT pg_size_pretty(pg_total_relation_size('brain_observations'))")
            .fetch_one(pool)
            .await
            .unwrap_or(("unknown".to_string(),));

    let entities_table_size: (String,) =
        sqlx::query_as("SELECT pg_size_pretty(pg_total_relation_size('brain_entities'))")
            .fetch_one(pool)
            .await
            .unwrap_or(("unknown".to_string(),));

    Ok(serde_json::json!({
        "metric": "health",
        "avg_importance": avg_importance.0,
        "stale_observations": stale_count.0,
        "unused_entities": unused_entities.0,
        "database_size": db_size.0,
        "null_embeddings": null_embeddings.0,
        "active_triggers": active_triggers.0,
        "table_sizes": {
            "brain_observations": obs_table_size.0,
            "brain_entities": entities_table_size.0
        },
        "embedding_model": crate::embeddings::onnx::current_model(),
        "entropy": {
            "entity_types": (entity_entropy * 1000.0).round() / 1000.0,
            "observation_types": (obs_entropy * 1000.0).round() / 1000.0,
            "knowledge_diversity_score": (diversity_score * 1000.0).round() / 1000.0,
            "interpretation": "Higher entropy = more diverse knowledge (score 0-1)"
        },
        "error_metrics": {
            "resolution_rate": (resolution_rate * 1000.0).round() / 1000.0,
            "mttr_minutes": err_stats.2.map(|s| (s / 60.0 * 10.0).round() / 10.0)
        }
    }))
}

async fn drift(pool: &PgPool) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;

    let recent: Vec<(String, i64)> = sqlx::query_as(
        "SELECT error_type, COUNT(*) FROM brain_errors \
         WHERE created_at > NOW() - INTERVAL '7 days' \
           AND ($1::uuid IS NULL OR project_id = $1) \
         GROUP BY error_type",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let historical: Vec<(String, f64)> = sqlx::query_as(
        "SELECT error_type, COUNT(*)::float8 / 4.0 FROM brain_errors \
         WHERE created_at BETWEEN NOW() - INTERVAL '37 days' AND NOW() - INTERVAL '7 days' \
           AND ($1::uuid IS NULL OR project_id = $1) \
         GROUP BY error_type",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let hist_map: std::collections::HashMap<String, f64> = historical.into_iter().collect();
    let mut chi_squared = 0.0;
    let mut categories = 0u32;

    for (error_type, observed) in &recent {
        if let Some(&expected) = hist_map.get(error_type)
            && expected > 0.0
        {
            chi_squared += (*observed as f64 - expected).powi(2) / expected;
            categories += 1;
        }
    }

    let df = (categories as i32 - 1).max(1);
    let p_value = chi2_survival(chi_squared, df as f64);

    let drift_data: Vec<Value> = recent
        .iter()
        .map(|(t, c)| serde_json::json!({"error_type": t, "count": c}))
        .collect();

    Ok(serde_json::json!({
        "metric": "drift",
        "chi_squared": (chi_squared * 10000.0).round() / 10000.0,
        "degrees_of_freedom": df,
        "p_value": (p_value * 1000000.0).round() / 1000000.0,
        "categories": categories,
        "drift_detected": p_value < 0.05,
        "error_distribution": drift_data
    }))
}

async fn communities(pool: &PgPool) -> Result<Value> {
    match crate::graph::community::detect_and_persist(pool).await {
        Ok((communities, nodes_tagged)) => {
            let community_json: Vec<Value> = communities
                .iter()
                .map(|(id, members)| {
                    serde_json::json!({
                        "community_id": id,
                        "size": members.len(),
                        "members": members
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "metric": "communities",
                "algorithm": "leiden",
                "persisted": true,
                "nodes_tagged": nodes_tagged,
                "communities": community_json,
                "count": communities.len()
            }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "Leiden community detection failed, using fallback");
            let project_id = crate::project::current_project_id(pool).await?;
            let components: Vec<(String, i64)> = sqlx::query_as(
                "SELECT e.entity_type, COUNT(*) FROM brain_entities e \
                 WHERE ($1::uuid IS NULL OR e.project_id = $1) \
                 GROUP BY e.entity_type ORDER BY COUNT(*) DESC",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await?;
            let communities: Vec<Value> = components
                .iter()
                .map(|(t, c)| serde_json::json!({"type": t, "size": c}))
                .collect();
            Ok(serde_json::json!({
                "metric": "communities",
                "algorithm": "fallback_groupby",
                "communities": communities
            }))
        }
    }
}

async fn bridges(pool: &PgPool) -> Result<Value> {
    match crate::graph::centrality::compute_bridges(pool, 10).await {
        Ok(ranked) => {
            let bridge_list: Vec<Value> = ranked
                .iter()
                .map(|(name, centrality)| {
                    serde_json::json!({
                        "entity": name,
                        "centrality": (centrality * 10000.0).round() / 10000.0
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "metric": "bridges",
                "algorithm": "brandes_betweenness",
                "bridge_entities": bridge_list,
                "interpretation": "High centrality = bridge connecting different knowledge areas"
            }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "Brandes centrality failed, using SQL fallback");
            let project_id = crate::project::current_project_id(pool).await?;
            let bridges: Vec<(String, i64)> = sqlx::query_as(
                "SELECT e.name, COUNT(r.id) as connection_count FROM brain_entities e
                 LEFT JOIN brain_relations r ON e.id = r.from_entity OR e.id = r.to_entity
                 WHERE ($1::uuid IS NULL OR e.project_id = $1)
                 GROUP BY e.name HAVING COUNT(r.id) > 2
                 ORDER BY connection_count DESC LIMIT 10",
            )
            .bind(project_id)
            .fetch_all(pool)
            .await?;
            let bridge_list: Vec<Value> = bridges
                .iter()
                .map(|(n, c)| serde_json::json!({"entity": n, "connections": c}))
                .collect();
            Ok(serde_json::json!({
                "metric": "bridges",
                "algorithm": "sql_connection_count",
                "bridge_entities": bridge_list
            }))
        }
    }
}

fn compute_entropy(counts: &[(String, i64)]) -> f64 {
    let total: f64 = counts.iter().map(|(_, c)| *c as f64).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let mut entropy = 0.0;
    for (_, count) in counts {
        let p = *count as f64 / total;
        if p > 0.0 {
            entropy -= p * p.log2();
        }
    }
    entropy
}

fn chi2_survival(x: f64, df: f64) -> f64 {
    if x <= 0.0 || df <= 0.0 {
        return 1.0;
    }
    let z = ((x / df).powf(1.0 / 3.0) - (1.0 - 2.0 / (9.0 * df))) / (2.0 / (9.0 * df)).sqrt();
    0.5 * erfc_approx(z / std::f64::consts::SQRT_2)
}

fn erfc_approx(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let poly = t
        * (0.254829592
            + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let result = poly * (-x * x).exp();
    if x >= 0.0 { result } else { 2.0 - result }
}
