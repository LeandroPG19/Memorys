use anyhow::Result;
use sqlx::PgPool;
use std::collections::HashMap;

const DAMPING: f64 = 0.85;
const ITERATIONS: usize = 20;
const CONVERGENCE_THRESHOLD: f64 = 1e-6;

pub async fn compute_and_store(pool: &PgPool) -> Result<usize> {
    let edges: Vec<(uuid::Uuid, uuid::Uuid, f64)> = sqlx::query_as(
        r#"
        SELECT r.from_entity, r.to_entity,
               r.strength / LN(1.0 + COALESCE(deg.degree, 1)) AS dampened_weight
        FROM brain_relations r
        LEFT JOIN (
            SELECT from_entity, COUNT(*) AS degree
            FROM brain_relations
            GROUP BY from_entity
        ) deg ON r.from_entity = deg.from_entity
        "#,
    )
    .fetch_all(pool)
    .await?;

    if edges.is_empty() {
        return Ok(0);
    }

    let mut nodes: HashMap<uuid::Uuid, usize> = HashMap::new();
    let mut node_list: Vec<uuid::Uuid> = Vec::new();

    for (from, to, _) in &edges {
        for id in [from, to] {
            if !nodes.contains_key(id) {
                let idx = node_list.len();
                nodes.insert(*id, idx);
                node_list.push(*id);
            }
        }
    }

    let n = node_list.len();
    if n == 0 {
        return Ok(0);
    }

    let mut outgoing: Vec<Vec<(usize, f64)>> = vec![vec![]; n];
    let mut out_weight_sum: Vec<f64> = vec![0.0; n];

    for (from, to, weight) in &edges {
        let from_idx = nodes[from];
        let to_idx = nodes[to];
        outgoing[from_idx].push((to_idx, *weight));
        out_weight_sum[from_idx] += weight;
    }

    let init_rank = 1.0 / n as f64;
    let mut ranks: Vec<f64> = vec![init_rank; n];
    let mut new_ranks: Vec<f64> = vec![0.0; n];

    for _iter in 0..ITERATIONS {
        new_ranks.fill((1.0 - DAMPING) / n as f64);

        let mut dangling_sum = 0.0;
        for i in 0..n {
            if out_weight_sum[i] > 0.0 {
                for &(j, weight) in &outgoing[i] {
                    new_ranks[j] += DAMPING * ranks[i] * weight / out_weight_sum[i];
                }
            } else {
                dangling_sum += DAMPING * ranks[i];
            }
        }
        if dangling_sum > 0.0 {
            let share = dangling_sum / n as f64;
            for r in new_ranks.iter_mut() {
                *r += share;
            }
        }

        let delta: f64 = ranks
            .iter()
            .zip(new_ranks.iter())
            .map(|(old, new)| (old - new).abs())
            .sum();

        std::mem::swap(&mut ranks, &mut new_ranks);

        if delta < CONVERGENCE_THRESHOLD {
            tracing::info!(iteration = _iter, delta = %delta, "PageRank converged");
            break;
        }
    }

    let ids = node_list;

    let min_r = ranks.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_r = ranks.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    if (max_r - min_r) < 1e-10 {
        tracing::info!(
            entities = n,
            "PageRank blend skipped: uniform distribution (no structural differentiation signal)"
        );
        return Ok(n);
    }

    let range = max_r - min_r;
    let normalized: Vec<f64> = ranks.iter().map(|r| (r - min_r) / range).collect();

    sqlx::query(
        r#"
        UPDATE brain_entities AS e
        SET importance = LEAST(0.3 * v.rank_norm + 0.7 * e.importance, 1.0),
            updated_at = NOW()
        FROM (SELECT UNNEST($1::uuid[]) AS id, UNNEST($2::float8[]) AS rank_norm) AS v
        WHERE e.id = v.id
        "#,
    )
    .bind(&ids)
    .bind(&normalized)
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO brain_node_metrics (node_id, pagerank_score, last_calculated)
        SELECT UNNEST($1::uuid[]), UNNEST($2::float8[]), NOW()
        ON CONFLICT (node_id) DO UPDATE SET
            pagerank_score = EXCLUDED.pagerank_score,
            last_calculated = NOW()
        "#,
    )
    .bind(&ids)
    .bind(&normalized)
    .execute(pool)
    .await?;

    tracing::info!(entities = n, "PageRank blended (α=0.3, P1 batch)");
    Ok(n)
}

/// Personalized PageRank (HippoRAG / Gutiérrez et al. 2024). Teleport mass
/// stays on `seeds`. Same damping as the global ranker (0.85).
pub fn personalized(outgoing: &[Vec<(usize, f64)>], seeds: &[usize]) -> Vec<f64> {
    let n = outgoing.len();
    if n == 0 || seeds.is_empty() {
        return vec![0.0; n];
    }
    let mut out_sum = vec![0.0; n];
    for (i, edges) in outgoing.iter().enumerate() {
        out_sum[i] = edges.iter().map(|(_, w)| *w).sum();
    }
    let teleport = 1.0 / seeds.len() as f64;
    let init = 1.0 / n as f64;
    let mut ranks = vec![init; n];
    let mut next = vec![0.0; n];
    for _ in 0..ITERATIONS {
        next.fill(0.0);
        for &s in seeds {
            if s < n {
                next[s] += (1.0 - DAMPING) * teleport;
            }
        }
        let mut dangling = 0.0;
        for i in 0..n {
            if out_sum[i] > 0.0 {
                for &(j, w) in &outgoing[i] {
                    next[j] += DAMPING * ranks[i] * w / out_sum[i];
                }
            } else {
                dangling += DAMPING * ranks[i];
            }
        }
        if dangling > 0.0 {
            let share = dangling * teleport;
            for &s in seeds {
                if s < n {
                    next[s] += share;
                }
            }
        }
        let delta: f64 = ranks
            .iter()
            .zip(next.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        std::mem::swap(&mut ranks, &mut next);
        if delta < CONVERGENCE_THRESHOLD {
            break;
        }
    }
    ranks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pagerank_convergence_constants() {
        const _: () = assert!(DAMPING > 0.0 && DAMPING < 1.0);
        const _: () = assert!(ITERATIONS > 0);
        const _: () = assert!(CONVERGENCE_THRESHOLD > 0.0);
    }

    #[test]
    fn test_minmax_normalization() {
        let ranks = [0.001, 0.005, 0.002, 0.010];
        let min_r = ranks.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_r = ranks.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = (max_r - min_r).max(1e-12);
        let normalized: Vec<f64> = ranks.iter().map(|r| (r - min_r) / range).collect();

        assert!((normalized[0] - 0.0).abs() < 1e-6, "min should map to 0.0");
        assert!((normalized[3] - 1.0).abs() < 1e-6, "max should map to 1.0");
        for v in &normalized {
            assert!(*v >= 0.0 && *v <= 1.0, "normalized value out of range: {v}");
        }
    }

    #[test]
    fn test_blend_preserves_existing_importance() {
        let existing = 0.8;
        let rank_norm = 0.2;
        let blended = 0.3 * rank_norm + 0.7 * existing;
        assert!(
            blended > 0.5,
            "blend should preserve existing importance: got {blended}"
        );
        assert!(
            blended < existing,
            "blend should incorporate PageRank signal"
        );
    }

    #[test]
    fn test_uniform_ranks_trigger_early_return() {
        let ranks = [0.25_f64; 4];
        let min_r = ranks.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_r = ranks.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = max_r - min_r;
        assert!(
            range < 1e-10,
            "uniform ranks must trigger early return: range={range}"
        );
        let existing = 0.8;
        let decay_with_zero_norm = 0.3 * 0.0 + 0.7 * existing;
        assert!(
            decay_with_zero_norm < existing,
            "without guard, importance decays from {existing} to {decay_with_zero_norm}"
        );
    }

    #[test]
    fn personalized_mass_stays_near_the_seed() {
        // 0 → 1 → 2, seed = 0. Rank[0] must beat rank[2].
        let outgoing = vec![vec![(1, 1.0)], vec![(2, 1.0)], vec![]];
        let ranks = personalized(&outgoing, &[0]);
        assert_eq!(ranks.len(), 3);
        assert!(
            ranks[0] > ranks[2],
            "seed 0={:.4} leaf 2={:.4}",
            ranks[0],
            ranks[2]
        );
        let sum: f64 = ranks.iter().sum();
        assert!(
            (sum - 1.0).abs() < 0.05,
            "PPR should be nearly stochastic: sum={sum}"
        );
    }

    #[test]
    fn test_non_uniform_ranks_proceed_to_blend() {
        let ranks = [0.01_f64, 0.05, 0.02, 0.10];
        let min_r = ranks.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_r = ranks.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = max_r - min_r;
        assert!(
            range >= 1e-10,
            "non-uniform ranks should not trigger early return: range={range}"
        );
    }
}
