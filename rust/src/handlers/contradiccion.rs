use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;

const NEGATION_MARKERS: &[&str] = &[
    "no ",
    "not ",
    "never ",
    "don't",
    "doesn't",
    "isn't",
    "wasn't",
    "ya no",
    "no es",
    "no usa",
    "no tiene",
    "reemplazado",
    "deprecated",
    "replaced",
    "removed",
    "instead of",
    "en vez de",
    "eliminado",
];

pub async fn handle(pool: &PgPool, args: Value) -> Result<Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let entity_name = args
        .get("entity_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match action {
        "scan" => scan(pool, entity_name).await,
        _ => anyhow::bail!("Invalid action: {action}. Use scan"),
    }
}

async fn scan(pool: &PgPool, entity_name: &str) -> Result<Value> {
    if entity_name.is_empty() {
        return scan_top_entities(pool).await;
    }
    scan_entity(pool, entity_name).await
}

async fn scan_entity(pool: &PgPool, entity_name: &str) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;

    type PairRow = (uuid::Uuid, String, uuid::Uuid, String, f64);
    let pairs: Vec<PairRow> = sqlx::query_as(
        "SELECT a.id, a.content, b.id, b.content,
                (1.0 - (a.embedding <=> b.embedding))::float8 AS cosine_sim
         FROM brain_observations a
         JOIN brain_observations b ON a.entity_id = b.entity_id AND a.id < b.id
         WHERE a.embedding IS NOT NULL AND b.embedding IS NOT NULL
           AND a.observation_type NOT IN ('superseded', 'tool_usage')
           AND b.observation_type NOT IN ('superseded', 'tool_usage')
           AND (1.0 - (a.embedding <=> b.embedding)) BETWEEN 0.3 AND 0.85
           AND a.entity_id = (SELECT id FROM brain_entities WHERE name = $1)
           AND ($2::uuid IS NULL OR a.project_id = $2)
           AND ($2::uuid IS NULL OR b.project_id = $2)
         ORDER BY cosine_sim DESC
         LIMIT 20",
    )
    .bind(entity_name)
    .bind(project_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let contradictions = score_pairs(&pairs);
    let count = contradictions.len();

    Ok(serde_json::json!({
        "action": "scan",
        "entity_name": entity_name,
        "contradictions": contradictions,
        "count": count
    }))
}

async fn scan_top_entities(pool: &PgPool) -> Result<Value> {
    let project_id = crate::project::current_project_id(pool).await?;

    let entities: Vec<(String,)> = sqlx::query_as(
        "SELECT e.name
         FROM brain_entities e
         JOIN brain_observations o ON o.entity_id = e.id
         WHERE o.observation_type NOT IN ('superseded', 'tool_usage')
           AND o.embedding IS NOT NULL
           AND ($1::uuid IS NULL OR o.project_id = $1)
         GROUP BY e.name
         HAVING COUNT(*) >= 2
         ORDER BY COUNT(*) DESC
         LIMIT 20",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;

    let mut all_contradictions: Vec<Value> = Vec::new();

    for (name,) in &entities {
        type PairRow = (uuid::Uuid, String, uuid::Uuid, String, f64);
        let pairs: Vec<PairRow> = sqlx::query_as(
            "SELECT a.id, a.content, b.id, b.content,
                    (1.0 - (a.embedding <=> b.embedding))::float8 AS cosine_sim
             FROM brain_observations a
             JOIN brain_observations b ON a.entity_id = b.entity_id AND a.id < b.id
             WHERE a.embedding IS NOT NULL AND b.embedding IS NOT NULL
               AND a.observation_type NOT IN ('superseded', 'tool_usage')
               AND b.observation_type NOT IN ('superseded', 'tool_usage')
               AND (1.0 - (a.embedding <=> b.embedding)) BETWEEN 0.3 AND 0.85
               AND a.entity_id = (SELECT id FROM brain_entities WHERE name = $1)
               AND ($2::uuid IS NULL OR a.project_id = $2)
               AND ($2::uuid IS NULL OR b.project_id = $2)
             ORDER BY cosine_sim DESC
             LIMIT 5",
        )
        .bind(name)
        .bind(project_id)
        .fetch_all(pool)
        .await
        .unwrap_or_default();

        let mut scored = score_pairs(&pairs);
        for item in &mut scored {
            if let Some(obj) = item.as_object_mut() {
                obj.insert("entity_name".to_string(), serde_json::json!(name));
            }
        }
        all_contradictions.extend(scored);
    }

    all_contradictions.sort_by(|a, b| {
        let sa = a
            .get("contradiction_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let sb = b
            .get("contradiction_score")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    all_contradictions.truncate(20);

    let count = all_contradictions.len();
    Ok(serde_json::json!({
        "action": "scan",
        "entity_name": null,
        "contradictions": all_contradictions,
        "count": count
    }))
}

fn has_negation_conflict(a: &str, b: &str) -> bool {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    for marker in NEGATION_MARKERS {
        if (a_lower.contains(marker) && !b_lower.contains(marker))
            || (!a_lower.contains(marker) && b_lower.contains(marker))
        {
            return true;
        }
    }
    false
}

pub fn heuristic_conflict(a: &str, b: &str) -> bool {
    has_negation_conflict(a, b)
}

fn score_pairs(pairs: &[(uuid::Uuid, String, uuid::Uuid, String, f64)]) -> Vec<Value> {
    pairs
        .iter()
        .map(|(id_a, content_a, id_b, content_b, cosine_sim)| {
            let negation = has_negation_conflict(content_a, content_b);
            let multiplier = if negation { 1.5 } else { 0.7 };
            let score = (cosine_sim * multiplier).clamp(0.0, 1.0);

            serde_json::json!({
                "observation_a": {"id": id_a.to_string(), "content": content_a},
                "observation_b": {"id": id_b.to_string(), "content": content_b},
                "cosine_similarity": cosine_sim,
                "negation_detected": negation,
                "contradiction_score": score
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_negation_conflict_detected() {
        assert!(has_negation_conflict(
            "Rust uses async runtime",
            "Rust does not use async runtime"
        ));
        assert!(has_negation_conflict(
            "Uses uvicorn",
            "Replaced uvicorn with granian"
        ));
    }

    #[test]
    fn test_no_negation_conflict() {
        assert!(!has_negation_conflict("Rust is fast", "Rust is safe"));
    }

    #[test]
    fn test_score_pairs_clamps() {
        let pairs = vec![(
            uuid::Uuid::nil(),
            "not X".to_string(),
            uuid::Uuid::nil(),
            "Y is great".to_string(),
            0.8,
        )];
        let scored = score_pairs(&pairs);
        let score = scored[0]
            .get("contradiction_score")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!(score <= 1.0, "score should be clamped to 1.0, got {score}");
    }
}
