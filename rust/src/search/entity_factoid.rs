//! Entity-centric factoid retrieval (Mem0-style entity boost, no extra LLM).
//! Extra RRF leg, k=60. Off with MEMORY_INDUSTRY_ENTITY_FACTOID=off.

use crate::search::query_class::QueryClass;
use crate::search::rrf::RRF_K;
use std::collections::HashMap;

pub fn entity_factoid_enabled() -> bool {
    match std::env::var("MEMORY_INDUSTRY_ENTITY_FACTOID") {
        Ok(v) => {
            let t = v.trim();
            !(t.eq_ignore_ascii_case("off") || t == "0" || t.eq_ignore_ascii_case("false"))
        }
        Err(_) => true,
    }
}

pub fn factoid_effective_scope(class: QueryClass, has_mentions: bool, requested: &str) -> &str {
    if entity_factoid_enabled()
        && class == QueryClass::Factoid
        && has_mentions
        && requested == "all"
    {
        "observations"
    } else {
        requested
    }
}

pub fn rrf_rank_score(rank: usize) -> f64 {
    1.0 / (RRF_K + rank as f64 + 1.0)
}

/// Fuse ranked id lists with equal-weight RRF (Cormack et al., SIGIR 2009).
pub fn fuse_ranked_legs(legs: &[Vec<String>]) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    for leg in legs {
        for (rank, id) in leg.iter().enumerate() {
            *scores.entry(id.clone()).or_default() += rrf_rank_score(rank);
        }
    }
    let mut out: Vec<(String, f64)> = scores.into_iter().collect();
    out.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

pub fn apply_session_entity_boost(
    base: f64,
    entity_name: &str,
    content: &str,
    goals: &[String],
) -> (f64, bool) {
    if goals.is_empty() {
        return (base, false);
    }
    if goals
        .iter()
        .any(|g| crate::search::mentions::name_mentioned_in_query(entity_name, g))
    {
        return (base * 1.3, true);
    }
    let content_words: std::collections::HashSet<String> = content
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 1)
        .map(String::from)
        .collect();
    for goal in goals {
        let goal_words: std::collections::HashSet<String> = goal
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 1)
            .map(String::from)
            .collect();
        let overlap = content_words.intersection(&goal_words).count();
        if overlap > 0 {
            let match_ratio = overlap as f64 / goal_words.len().max(1) as f64;
            return (base * (1.0 + 0.3 * match_ratio), true);
        }
    }
    (base, false)
}

pub fn embedding_status(model_loaded: bool, persist_ok: bool) -> &'static str {
    if model_loaded && persist_ok {
        "ready"
    } else {
        "pending"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg_gold() -> Vec<String> {
        vec![
            "pg-obs-port".into(),
            "pg-obs-alembic".into(),
            "pg-obs-user".into(),
        ]
    }

    fn web_noise() -> Vec<String> {
        (0..10).map(|i| format!("mapupita-web-{i}")).collect()
    }

    #[test]
    fn entity_leg_lifts_postgres_gold_into_top_10() {
        let mut hybrid = web_noise();
        hybrid.extend(pg_gold());
        let fused = fuse_ranked_legs(&[hybrid, pg_gold()]);
        let top: Vec<&str> = fused.iter().take(10).map(|(id, _)| id.as_str()).collect();
        assert!(
            top.contains(&"pg-obs-port") && top.contains(&"pg-obs-alembic"),
            "hechos sobre PostgreSQL must surface that entity's gold, not Mapupita-Web: {top:?}"
        );
        assert!(
            !top.iter().all(|id| id.starts_with("mapupita-web")),
            "entity boost must not leave the top-10 as only Mapupita-Web"
        );
    }

    #[test]
    fn no_mention_keeps_hybrid_order() {
        let hybrid = web_noise();
        let fused = fuse_ranked_legs(std::slice::from_ref(&hybrid));
        let ids: Vec<String> = fused.into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids, hybrid,
            "without an entity mention the hybrid ranking must not change"
        );
    }

    #[test]
    fn factoid_with_mention_uses_observation_scope() {
        assert_eq!(
            factoid_effective_scope(QueryClass::Factoid, true, "all"),
            "observations"
        );
        assert_eq!(
            factoid_effective_scope(QueryClass::Factoid, false, "all"),
            "all",
            "generic factoid without a mention stays on the hybrid pipeline"
        );
        assert_eq!(
            factoid_effective_scope(QueryClass::MultiHop, true, "all"),
            "all"
        );
        assert_eq!(
            factoid_effective_scope(QueryClass::Factoid, true, "errors"),
            "errors",
            "an explicit scope is respected"
        );
    }

    #[test]
    fn session_goal_mentioning_entity_does_not_drop_its_obs() {
        let other = 0.60;
        let e_base = 0.50;
        let (e_boosted, flagged) = apply_session_entity_boost(
            e_base,
            "PostgreSQL",
            "migraciones en alembic",
            &["arreglar hechos sobre PostgreSQL".into()],
        );
        assert!(flagged);
        assert!(
            e_boosted >= other,
            "obs of the mentioned entity must not fall behind an unrelated higher base: {e_boosted} vs {other}"
        );
    }

    #[test]
    fn embedding_status_does_not_lie() {
        assert_eq!(embedding_status(true, true), "ready");
        assert_eq!(embedding_status(false, true), "pending");
        assert_eq!(embedding_status(true, false), "pending");
    }
}
