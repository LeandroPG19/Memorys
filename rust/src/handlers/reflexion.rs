use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("analyze");
    match action {
        "analyze" => analyze(pool).await,
        _ => anyhow::bail!("Invalid action: {action}. Use 'analyze'"),
    }
}

async fn analyze(pool: &PgPool) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;

    let (isolated, underconnected, type_silos, obs_gaps, density_anomalies) = tokio::try_join!(
        find_isolated(pool, project_id),
        find_underconnected(pool, project_id),
        find_type_silos(pool, project_id),
        find_observation_gaps(pool, project_id),
        find_density_anomalies(pool, project_id),
    )?;

    let mut recommendations: Vec<String> = Vec::new();

    for (name, entity_type) in &isolated {
        recommendations.push(format!(
            "Entity '{}' ({}) has no relations. Consider connecting it to related concepts.",
            name, entity_type
        ));
    }

    for (name, importance, degree) in &underconnected {
        recommendations.push(format!(
            "Entity '{}' has importance {:.2} but only {} relation(s). Consider expanding its connections.",
            name, importance, degree
        ));
    }

    for (type_a, type_b) in &type_silos {
        recommendations.push(format!(
            "Entity types '{}' and '{}' have no cross-connections. Consider linking related entities.",
            type_a, type_b
        ));
    }

    for (name, facts, decisions, lessons) in &obs_gaps {
        if *decisions == 0 && *lessons == 0 {
            recommendations.push(format!(
                "Entity '{}' has {} facts but 0 decisions and 0 lessons. Consider recording key decisions/lessons.",
                name, facts
            ));
        } else if *decisions == 0 {
            recommendations.push(format!(
                "Entity '{}' has {} facts but no decisions recorded.",
                name, facts
            ));
        }
    }

    let summary = format!(
        "Found {} isolated entities, {} underconnected hubs, {} type silos, {} observation gaps, {} density anomalies.",
        isolated.len(),
        underconnected.len(),
        type_silos.len(),
        obs_gaps.len(),
        density_anomalies.len()
    );

    Ok(serde_json::json!({
        "action": "analyze",
        "gaps": {
            "isolated_entities": isolated.iter().map(|(n, t)| serde_json::json!({
                "name": n, "entity_type": t
            })).collect::<Vec<_>>(),
            "underconnected_high_importance": underconnected.iter().map(|(n, i, d)| serde_json::json!({
                "name": n, "importance": i, "degree": d
            })).collect::<Vec<_>>(),
            "type_silos": type_silos.iter().map(|(a, b)| serde_json::json!({
                "type_a": a, "type_b": b
            })).collect::<Vec<_>>(),
            "observation_gaps": obs_gaps.iter().map(|(n, f, d, l)| serde_json::json!({
                "name": n, "facts": f, "decisions": d, "lessons": l
            })).collect::<Vec<_>>(),
            "density_anomalies": density_anomalies.iter().map(|(n, o, r, oz, rz)| serde_json::json!({
                "name": n, "obs_count": o, "rel_count": r,
                "obs_zscore": oz, "rel_zscore": rz
            })).collect::<Vec<_>>()
        },
        "summary": summary,
        "recommendations": recommendations
    }))
}

async fn find_isolated(
    pool: &PgPool,
    project_id: Option<uuid::Uuid>,
) -> Result<Vec<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT e.name, e.entity_type
         FROM brain_entities e
         LEFT JOIN brain_relations r ON e.id = r.from_entity OR e.id = r.to_entity
         WHERE r.id IS NULL
           AND ($1::uuid IS NULL OR e.project_id = $1)
         ORDER BY e.importance DESC
         LIMIT 20",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn find_underconnected(
    pool: &PgPool,
    project_id: Option<uuid::Uuid>,
) -> Result<Vec<(String, f64, i64)>> {
    let rows: Vec<(String, f64, i64)> = sqlx::query_as(
        "WITH entity_degree AS (
            SELECT e.id, e.name, e.importance::float8,
                   COUNT(r.id) AS degree
            FROM brain_entities e
            LEFT JOIN brain_relations r ON e.id = r.from_entity OR e.id = r.to_entity
            WHERE ($1::uuid IS NULL OR e.project_id = $1)
            GROUP BY e.id, e.name, e.importance
        )
        SELECT name, importance, degree
        FROM entity_degree
        WHERE importance > 0.4 AND degree < 3
        ORDER BY importance DESC
        LIMIT 10",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn find_type_silos(
    pool: &PgPool,
    project_id: Option<uuid::Uuid>,
) -> Result<Vec<(String, String)>> {
    let types: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT entity_type FROM brain_entities
         WHERE ($1::uuid IS NULL OR project_id = $1)
         ORDER BY entity_type",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let connected: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT LEAST(e1.entity_type, e2.entity_type),
                         GREATEST(e1.entity_type, e2.entity_type)
         FROM brain_relations r
         JOIN brain_entities e1 ON r.from_entity = e1.id
         JOIN brain_entities e2 ON r.to_entity = e2.id
         WHERE e1.entity_type != e2.entity_type
           AND ($1::uuid IS NULL OR e1.project_id = $1)
           AND ($1::uuid IS NULL OR e2.project_id = $1)",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let mut silos: Vec<(String, String)> = Vec::new();
    for (i, (t1,)) in types.iter().enumerate() {
        for (t2,) in types.iter().skip(i + 1) {
            let key_a = t1.min(t2).clone();
            let key_b = t1.max(t2).clone();
            if !connected.iter().any(|(a, b)| a == &key_a && b == &key_b) {
                silos.push((t1.clone(), t2.clone()));
            }
        }
    }
    silos.truncate(10);
    Ok(silos)
}

async fn find_observation_gaps(
    pool: &PgPool,
    project_id: Option<uuid::Uuid>,
) -> Result<Vec<(String, i64, i64, i64)>> {
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT e.name,
                COUNT(*) FILTER (WHERE o.observation_type = 'fact') AS facts,
                COUNT(*) FILTER (WHERE o.observation_type = 'decision') AS decisions,
                COUNT(*) FILTER (WHERE o.observation_type = 'lesson') AS lessons
         FROM brain_entities e
         JOIN brain_observations o ON e.id = o.entity_id
         WHERE o.observation_type != 'superseded'
           AND ($1::uuid IS NULL OR e.project_id = $1)
         GROUP BY e.id, e.name
         HAVING COUNT(*) FILTER (WHERE o.observation_type = 'fact') > 3
            AND (COUNT(*) FILTER (WHERE o.observation_type = 'decision') = 0
             OR  COUNT(*) FILTER (WHERE o.observation_type = 'lesson') = 0)
         ORDER BY facts DESC
         LIMIT 10",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

async fn find_density_anomalies(
    pool: &PgPool,
    project_id: Option<uuid::Uuid>,
) -> Result<Vec<(String, i64, i64, f64, f64)>> {
    let rows: Vec<(String, i64, i64, f64, f64)> = sqlx::query_as(
        "WITH stats AS (
            SELECT e.id, e.name,
                COUNT(DISTINCT o.id) AS obs_count,
                COUNT(DISTINCT r.id) AS rel_count
            FROM brain_entities e
            LEFT JOIN brain_observations o ON e.id = o.entity_id
                AND o.observation_type != 'superseded'
            LEFT JOIN brain_relations r ON e.id = r.from_entity OR e.id = r.to_entity
            WHERE ($1::uuid IS NULL OR e.project_id = $1)
            GROUP BY e.id, e.name
        ),
        global AS (
            SELECT AVG(obs_count)::float8 AS avg_obs,
                   STDDEV_POP(obs_count)::float8 AS std_obs,
                   AVG(rel_count)::float8 AS avg_rel,
                   STDDEV_POP(rel_count)::float8 AS std_rel
            FROM stats
        )
        SELECT name, obs_count, rel_count, obs_zscore, rel_zscore FROM (
            SELECT s.name, s.obs_count, s.rel_count,
                   (s.obs_count - g.avg_obs) / GREATEST(g.std_obs, 1.0) AS obs_zscore,
                   (s.rel_count - g.avg_rel) / GREATEST(g.std_rel, 1.0) AS rel_zscore
            FROM stats s, global g
            WHERE ABS((s.obs_count - g.avg_obs) / GREATEST(g.std_obs, 1.0)) > 2.0
               OR ABS((s.rel_count - g.avg_rel) / GREATEST(g.std_rel, 1.0)) > 2.0
        ) sub
        ORDER BY ABS(obs_zscore) + ABS(rel_zscore) DESC
        LIMIT 10",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
