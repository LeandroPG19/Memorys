//! Path pruning for graph context (PathRAG-lt: Chen et al., arXiv:2502.14902).
//! Score = product of edge strengths / (1 + depth). Keep K paths, at most N nodes.

#[derive(Debug, Clone, PartialEq)]
pub struct RelPath {
    pub nodes: Vec<String>,
    pub relations: Vec<String>,
    pub strengths: Vec<f64>,
    pub score: f64,
}

pub const PATHRAG_LT_N: usize = 20;
pub const PATHRAG_LT_K: usize = 5;

pub fn path_score(strengths: &[f64], depth: usize) -> f64 {
    if strengths.is_empty() {
        return 0.0;
    }
    let product: f64 = strengths
        .iter()
        .copied()
        .map(|s| s.clamp(0.01, 1.0))
        .product();
    product / (1.0 + depth as f64)
}

/// Keep the K highest-scoring paths, then order by score ascending so the
/// strongest path is last (Liu et al. 2024 lost-in-the-middle).
pub fn prune_paths(mut paths: Vec<RelPath>, n_nodes: usize, k: usize) -> Vec<RelPath> {
    let mut seen = 0usize;
    let mut kept = Vec::new();
    paths.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for p in paths {
        let new_nodes = p.nodes.len();
        if kept.len() >= k {
            break;
        }
        if seen + new_nodes > n_nodes && !kept.is_empty() {
            continue;
        }
        seen += new_nodes;
        kept.push(p);
    }
    kept.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hop(nodes: &[&str], rels: &[&str], strengths: &[f64]) -> RelPath {
        let depth = rels.len();
        RelPath {
            nodes: nodes.iter().map(|s| (*s).to_string()).collect(),
            relations: rels.iter().map(|s| (*s).to_string()).collect(),
            strengths: strengths.to_vec(),
            score: path_score(strengths, depth),
        }
    }

    #[test]
    fn longer_weaker_paths_score_lower() {
        let short = path_score(&[1.0], 1);
        let long = path_score(&[1.0, 1.0, 1.0], 3);
        assert!(short > long);
    }

    #[test]
    fn prune_keeps_k_and_puts_best_last() {
        let paths = vec![
            hop(&["A", "B"], &["rel"], &[0.9]),
            hop(&["A", "C"], &["rel"], &[0.2]),
            hop(&["A", "D"], &["rel"], &[0.5]),
        ];
        let out = prune_paths(paths, PATHRAG_LT_N, 2);
        assert_eq!(out.len(), 2);
        assert!(out[0].score <= out[1].score);
        assert!((out[1].score - path_score(&[0.9], 1)).abs() < 1e-9);
    }
}
