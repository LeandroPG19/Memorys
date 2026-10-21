use anyhow::Result;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

pub struct ActivationPropagation {
    decay: f32,
    max_hops: usize,
}

impl Default for ActivationPropagation {
    fn default() -> Self {
        Self {
            decay: 0.85,
            max_hops: 3,
        }
    }
}

impl ActivationPropagation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_hops(mut self, hops: usize) -> Self {
        self.max_hops = hops.clamp(1, 8);
        self
    }

    pub fn propagate_on_graph(
        &self,
        neighbors: &[Vec<(usize, f32)>],
        seed_nodes: &[usize],
    ) -> Vec<f32> {
        let n = neighbors.len();
        let mut activations = vec![0.0_f32; n];
        for &node in seed_nodes {
            if node < n {
                activations[node] = 1.0;
            }
        }

        for _ in 0..self.max_hops {
            let mut next = activations.clone();
            for (i, nbrs) in neighbors.iter().enumerate() {
                if activations[i] <= f32::EPSILON {
                    continue;
                }
                let spread = activations[i] * self.decay;
                for &(j, w) in nbrs {
                    if j < n {
                        next[j] = next[j].max(spread * w);
                    }
                }
            }
            activations = next;
        }
        activations
    }

    pub fn propagate(&self, seed_nodes: &[usize], graph_size: usize) -> Vec<f32> {
        let mut activations = vec![0.0_f32; graph_size];
        for &node in seed_nodes {
            if node < graph_size {
                activations[node] = 1.0;
            }
        }
        activations.iter().map(|&v| v * self.decay).collect()
    }
}

pub async fn spread_from_entities(
    pool: &PgPool,
    seed_ids: &[Uuid],
    max_hops: usize,
) -> Result<Vec<(Uuid, f32)>> {
    if seed_ids.is_empty() {
        return Ok(vec![]);
    }

    let edges: Vec<(Uuid, Uuid, f64)> =
        sqlx::query_as("SELECT from_entity, to_entity, strength FROM brain_relations")
            .fetch_all(pool)
            .await?;

    let entities: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM brain_entities")
        .fetch_all(pool)
        .await?;

    let mut id_to_idx: HashMap<Uuid, usize> = HashMap::new();
    let mut idx_to_id: Vec<Uuid> = Vec::new();
    for (id,) in entities {
        let idx = idx_to_id.len();
        id_to_idx.insert(id, idx);
        idx_to_id.push(id);
    }

    let n = idx_to_id.len();
    if n == 0 {
        return Ok(vec![]);
    }

    let mut neighbors: Vec<Vec<(usize, f32)>> = vec![vec![]; n];
    for (from, to, strength) in edges {
        if let (Some(&fi), Some(&ti)) = (id_to_idx.get(&from), id_to_idx.get(&to)) {
            let w = (strength as f32).clamp(0.01, 1.0);
            neighbors[fi].push((ti, w));
            neighbors[ti].push((fi, w));
        }
    }

    let seeds: Vec<usize> = seed_ids
        .iter()
        .filter_map(|id| id_to_idx.get(id).copied())
        .collect();

    let ap = ActivationPropagation::default().with_max_hops(max_hops);
    let activations = ap.propagate_on_graph(&neighbors, &seeds);

    let mut scored: Vec<(Uuid, f32)> = idx_to_id
        .into_iter()
        .zip(activations)
        .filter(|(_, a)| *a > f32::EPSILON)
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(scored)
}

pub async fn activated_neighbor_names(
    pool: &PgPool,
    entity_name: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let seed: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM brain_entities WHERE name = $1 LIMIT 1")
            .bind(entity_name)
            .fetch_optional(pool)
            .await?;
    let Some((seed_id,)) = seed else {
        return Ok(vec![]);
    };

    let spread = spread_from_entities(pool, &[seed_id], 2).await?;
    let mut names = Vec::new();
    for (id, _) in spread.into_iter().take(limit + 1) {
        if id == seed_id {
            continue;
        }
        if let Ok((name,)) =
            sqlx::query_as::<_, (String,)>("SELECT name FROM brain_entities WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
        {
            names.push(name);
        }
    }
    Ok(names.into_iter().take(limit).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multi_hop_spread() {
        let neighbors = vec![vec![(1, 1.0)], vec![(0, 1.0), (2, 0.5)], vec![(1, 0.5)]];
        let ap = ActivationPropagation::default().with_max_hops(2);
        let out = ap.propagate_on_graph(&neighbors, &[0]);
        assert!(out[2] > 0.0);
        assert!(out[0] >= out[2]);
    }

    #[test]
    fn test_propagate_decay() {
        let ap = ActivationPropagation::new();
        let out = ap.propagate(&[0, 2], 4);
        assert!((out[0] - 0.85).abs() < 1e-6);
        assert_eq!(out[1], 0.0);
    }
}
