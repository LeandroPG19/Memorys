use anyhow::Result;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CentralityScores {
    pub harmonic: f64,
    pub closeness: f64,
}

pub fn compute_in_memory(adj: &HashMap<Uuid, Vec<Uuid>>) -> HashMap<Uuid, CentralityScores> {
    let nodes: Vec<Uuid> = adj.keys().copied().collect();
    let mut out: HashMap<Uuid, CentralityScores> = HashMap::with_capacity(nodes.len());

    for &source in &nodes {
        let dists = bfs_distances(source, adj);
        let mut harmonic = 0.0_f64;
        let mut closeness_inv_sum = 0.0_f64;
        let mut reachable = 0usize;
        for (&v, &d) in &dists {
            if v == source {
                continue;
            }
            harmonic += 1.0 / d as f64;
            closeness_inv_sum += d as f64;
            reachable += 1;
        }
        let closeness = if closeness_inv_sum > 0.0 && reachable > 0 {
            reachable as f64 / closeness_inv_sum
        } else {
            0.0
        };
        out.insert(
            source,
            CentralityScores {
                harmonic,
                closeness,
            },
        );
    }
    out
}

fn bfs_distances(source: Uuid, adj: &HashMap<Uuid, Vec<Uuid>>) -> HashMap<Uuid, u32> {
    let mut dist: HashMap<Uuid, u32> = HashMap::new();
    dist.insert(source, 0);
    let mut frontier = vec![source];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for node in &frontier {
            let d = dist[node];
            if let Some(neighbors) = adj.get(node) {
                for &n in neighbors {
                    if let std::collections::hash_map::Entry::Vacant(e) = dist.entry(n) {
                        e.insert(d + 1);
                        next.push(n);
                    }
                }
            }
        }
        frontier = next;
    }
    dist
}

async fn fetch_adjacency(pool: &PgPool) -> Result<HashMap<Uuid, Vec<Uuid>>> {
    let project_id = crate::project::current_project_id(pool).await?;
    let edges: Vec<(Uuid, Uuid, bool)> = sqlx::query_as(
        "SELECT from_entity, to_entity, bidirectional
         FROM brain_relations
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let mut adj: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (a, b, bi) in edges {
        adj.entry(a).or_default().push(b);
        if bi {
            adj.entry(b).or_default().push(a);
        }
    }
    Ok(adj)
}

pub async fn compute_top(pool: &PgPool, top_n: usize) -> Result<Vec<(String, f64, f64)>> {
    let adj = fetch_adjacency(pool).await?;
    let scores = compute_in_memory(&adj);
    let mut ranked: Vec<(Uuid, &CentralityScores)> = scores.iter().map(|(k, v)| (*k, v)).collect();
    ranked.sort_by(|a, b| {
        let sa = a.1.harmonic + a.1.closeness * 0.5;
        let sb = b.1.harmonic + b.1.closeness * 0.5;
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(top_n);

    let ids: Vec<Uuid> = ranked.iter().map(|(id, _)| *id).collect();
    let names: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM brain_entities WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(pool)
            .await?;
    let name_map: HashMap<Uuid, String> = names.into_iter().collect();

    Ok(ranked
        .into_iter()
        .map(|(id, sc)| {
            let name = name_map.get(&id).cloned().unwrap_or_else(|| id.to_string());
            (name, sc.harmonic, sc.closeness)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn star_adj() -> HashMap<Uuid, Vec<Uuid>> {
        let center = Uuid::nil();
        let leaves: Vec<Uuid> = (1..=4)
            .map(|i| {
                let mut b = [0u8; 16];
                b[15] = i;
                Uuid::from_bytes(b)
            })
            .collect();
        let mut adj: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        adj.insert(center, leaves.clone());
        for l in leaves {
            adj.insert(l, vec![center]);
        }
        adj
    }

    #[test]
    fn star_center_has_max_centrality() {
        let adj = star_adj();
        let scores = compute_in_memory(&adj);
        let center = Uuid::nil();
        let center_h = scores[&center].harmonic;
        let mut leaf_id = [0u8; 16];
        leaf_id[15] = 1;
        let leaf = Uuid::from_bytes(leaf_id);
        let leaf_h = scores[&leaf].harmonic;
        assert!(
            center_h > leaf_h,
            "center {center_h} should have higher harmonic than leaf {leaf_h}"
        );
    }

    #[test]
    fn isolated_node_has_zero_centrality() {
        let mut adj = HashMap::new();
        let isolated = Uuid::new_v4();
        adj.insert(isolated, vec![]);
        let scores = compute_in_memory(&adj);
        assert_eq!(scores[&isolated].harmonic, 0.0);
        assert_eq!(scores[&isolated].closeness, 0.0);
    }

    #[test]
    fn disconnected_components_handled_via_harmonic() {
        let a = Uuid::from_bytes([1; 16]);
        let b = Uuid::from_bytes([2; 16]);
        let c = Uuid::from_bytes([3; 16]);
        let d = Uuid::from_bytes([4; 16]);
        let mut adj = HashMap::new();
        adj.insert(a, vec![b]);
        adj.insert(b, vec![a]);
        adj.insert(c, vec![d]);
        adj.insert(d, vec![c]);
        let scores = compute_in_memory(&adj);
        assert!((scores[&a].harmonic - 1.0).abs() < 1e-9);
        assert!((scores[&c].harmonic - 1.0).abs() < 1e-9);
    }
}
