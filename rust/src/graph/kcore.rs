use anyhow::Result;
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

pub fn compute_in_memory(adj: &HashMap<Uuid, Vec<Uuid>>) -> HashMap<Uuid, u32> {
    let n = adj.len();
    if n == 0 {
        return HashMap::new();
    }

    let mut degree: HashMap<Uuid, usize> = HashMap::with_capacity(n);
    let mut undirected: HashMap<Uuid, std::collections::HashSet<Uuid>> = HashMap::with_capacity(n);
    for (&v, neighbors) in adj {
        let entry = undirected.entry(v).or_default();
        for &u in neighbors {
            if u != v {
                entry.insert(u);
            }
        }
    }
    let snapshot: Vec<(Uuid, Vec<Uuid>)> = undirected
        .iter()
        .map(|(k, v)| (*k, v.iter().copied().collect()))
        .collect();
    for (v, neighbors) in snapshot {
        for u in neighbors {
            undirected.entry(u).or_default().insert(v);
        }
    }
    for (k, v) in &undirected {
        degree.insert(*k, v.len());
    }

    let mut kcore: HashMap<Uuid, u32> = HashMap::with_capacity(n);
    let mut removed: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    let mut running_max: u32 = 0;
    while removed.len() < n {
        let (&min_v, &min_d) = degree
            .iter()
            .filter(|(k, _)| !removed.contains(k))
            .min_by_key(|(_, d)| **d)
            .expect("non-empty active set");
        running_max = running_max.max(min_d as u32);
        kcore.insert(min_v, running_max);
        removed.insert(min_v);
        if let Some(nbrs) = undirected.get(&min_v) {
            for n in nbrs {
                if !removed.contains(n)
                    && let Some(d) = degree.get_mut(n)
                {
                    *d = d.saturating_sub(1);
                }
            }
        }
    }
    kcore
}

async fn fetch_adjacency(pool: &PgPool) -> Result<HashMap<Uuid, Vec<Uuid>>> {
    let project_id = crate::project::current_project_id(pool).await?;
    let edges: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT from_entity, to_entity FROM brain_relations
         WHERE ($1::uuid IS NULL OR project_id = $1)",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let mut adj: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (a, b) in edges {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    Ok(adj)
}

pub async fn compute_top(pool: &PgPool, top_n: usize) -> Result<Vec<(String, u32)>> {
    let adj = fetch_adjacency(pool).await?;
    let kcore = compute_in_memory(&adj);
    let mut ranked: Vec<(Uuid, u32)> = kcore.into_iter().collect();
    ranked.sort_by_key(|b| std::cmp::Reverse(b.1));
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
        .map(|(id, k)| {
            let name = name_map.get(&id).cloned().unwrap_or_else(|| id.to_string());
            (name, k)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> HashMap<Uuid, Vec<Uuid>> {
        let a = Uuid::from_bytes([1; 16]);
        let b = Uuid::from_bytes([2; 16]);
        let c = Uuid::from_bytes([3; 16]);
        let mut adj = HashMap::new();
        adj.insert(a, vec![b, c]);
        adj.insert(b, vec![a, c]);
        adj.insert(c, vec![a, b]);
        adj
    }

    #[test]
    fn triangle_is_2_core() {
        let kc = compute_in_memory(&triangle());
        for k in kc.values() {
            assert_eq!(*k, 2, "every node in K3 has k-core 2");
        }
    }

    #[test]
    fn star_leaves_are_1_core() {
        let center = Uuid::from_bytes([0; 16]);
        let leaves: Vec<Uuid> = (1..=4)
            .map(|i| {
                let mut b = [0u8; 16];
                b[15] = i;
                Uuid::from_bytes(b)
            })
            .collect();
        let mut adj = HashMap::new();
        adj.insert(center, leaves.clone());
        for l in leaves.iter() {
            adj.insert(*l, vec![center]);
        }
        let kc = compute_in_memory(&adj);
        for l in &leaves {
            assert_eq!(kc[l], 1);
        }
        assert!(kc[&center] <= 1);
    }

    #[test]
    fn empty_graph_returns_empty() {
        assert!(compute_in_memory(&HashMap::new()).is_empty());
    }

    #[test]
    fn isolated_node_has_kcore_zero() {
        let mut adj = HashMap::new();
        let v = Uuid::nil();
        adj.insert(v, vec![]);
        let kc = compute_in_memory(&adj);
        assert_eq!(kc[&v], 0);
    }
}
