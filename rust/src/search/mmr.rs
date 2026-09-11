pub fn mmr_select(
    relevance: &[f64],
    pairwise_sim: &[Vec<f64>],
    lambda: f64,
    top_k: usize,
) -> Vec<usize> {
    let n = relevance.len();
    let k = top_k.min(n);
    if k == 0 {
        return Vec::new();
    }
    debug_assert!(
        pairwise_sim.len() == n && pairwise_sim.iter().all(|row| row.len() == n),
        "pairwise_sim must be N×N where N = relevance.len()"
    );
    let lambda = lambda.clamp(0.0, 1.0);

    let mut selected: Vec<usize> = Vec::with_capacity(k);
    let mut remaining: Vec<usize> = (0..n).collect();

    while selected.len() < k && !remaining.is_empty() {
        let mut best_idx = 0;
        let mut best_score = f64::NEG_INFINITY;
        for (pos, &cand) in remaining.iter().enumerate() {
            let max_sim_to_selected = if selected.is_empty() {
                0.0
            } else {
                selected
                    .iter()
                    .map(|&s| pairwise_sim[cand][s])
                    .fold(f64::NEG_INFINITY, f64::max)
            };
            let score = lambda * relevance[cand] - (1.0 - lambda) * max_sim_to_selected;
            if score > best_score {
                best_score = score;
                best_idx = pos;
            }
        }
        let chosen = remaining.swap_remove(best_idx);
        selected.push(chosen);
    }
    selected
}

pub const DEFAULT_LAMBDA: f64 = 0.7;

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_sim(n: usize) -> Vec<Vec<f64>> {
        let mut m = vec![vec![0.0; n]; n];
        for (i, row) in m.iter_mut().enumerate().take(n) {
            row[i] = 1.0;
        }
        m
    }

    #[test]
    fn lambda_one_returns_top_k_by_relevance() {
        let rel = vec![0.1, 0.9, 0.5, 0.3];
        let sim = identity_sim(4);
        let picks = mmr_select(&rel, &sim, 1.0, 3);
        assert_eq!(picks, vec![1, 2, 3], "λ=1 should sort purely by relevance");
    }

    #[test]
    fn lambda_zero_diversifies_with_redundant_candidates() {
        let rel = vec![0.9, 0.85, 0.5, 0.4];
        let mut sim = identity_sim(4);
        sim[0][1] = 0.95;
        sim[1][0] = 0.95;
        let picks = mmr_select(&rel, &sim, 0.0, 2);
        assert_eq!(
            picks[0], 0,
            "first pick maximizes -max_sim, all equal at start"
        );
        assert_ne!(
            picks[1], 1,
            "second pick should NOT be the near-duplicate of 0"
        );
    }

    #[test]
    fn balanced_lambda_picks_diverse_top() {
        let rel = vec![0.9, 0.88, 0.7];
        let mut sim = identity_sim(3);
        sim[0][1] = 0.99;
        sim[1][0] = 0.99;
        let picks = mmr_select(&rel, &sim, 0.5, 2);
        assert_eq!(picks[0], 0, "top relevance wins first");
        assert_eq!(picks[1], 2, "diversity should beat near-duplicate");
    }

    #[test]
    fn empty_inputs_return_empty() {
        assert!(mmr_select(&[], &[], 0.5, 5).is_empty());
    }

    #[test]
    fn k_larger_than_n_clamps() {
        let rel = vec![0.5, 0.6];
        let sim = identity_sim(2);
        let picks = mmr_select(&rel, &sim, 1.0, 10);
        assert_eq!(picks.len(), 2);
    }

    #[test]
    fn zero_k_returns_empty_even_when_there_are_candidates() {
        let rel = vec![0.9, 0.1];
        let sim = identity_sim(2);
        assert!(mmr_select(&rel, &sim, 1.0, 0).is_empty());
    }

    #[test]
    fn lambda_above_one_is_clamped_to_pure_relevance() {
        let rel = vec![0.1, 0.9, 0.5];
        let sim = identity_sim(3);
        assert_eq!(mmr_select(&rel, &sim, 8.0, 2), vec![1, 2]);
    }

    #[test]
    fn diversity_weight_is_one_minus_lambda_not_one_over_lambda() {
        // λ=0.5 → (1-λ)=0.5. Replacing the `-` inside `(1.0 - lambda)` with `/`
        // yields 1/λ=2.0 and flips the second pick.
        let rel = vec![0.90, 0.80, 0.70];
        let mut sim = identity_sim(3);
        sim[0][1] = 0.10;
        sim[1][0] = 0.10;
        sim[0][2] = 0.02;
        sim[2][0] = 0.02;
        let picks = mmr_select(&rel, &sim, 0.5, 2);
        assert_eq!(
            picks,
            vec![0, 1],
            "second pick must keep the higher-relevance neighbour when (1-λ) is 0.5"
        );
    }
}
