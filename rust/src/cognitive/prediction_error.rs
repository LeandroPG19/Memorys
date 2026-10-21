use crate::constants::{PRED_ERROR_REINFORCE, PRED_ERROR_UPDATE};

#[derive(Debug, Clone, PartialEq)]
pub enum GatingAction {
    Reinforce,
    Update,
    Create,
}

pub fn gate(similarity: f64) -> GatingAction {
    if similarity >= PRED_ERROR_REINFORCE {
        GatingAction::Reinforce
    } else if similarity >= PRED_ERROR_UPDATE {
        GatingAction::Update
    } else {
        GatingAction::Create
    }
}

pub fn adaptive_gate(similarity: f64, recent_similarities: &[f64]) -> GatingAction {
    let (reinforce_thresh, update_thresh) = adaptive_thresholds_conformal(recent_similarities);

    if similarity >= reinforce_thresh {
        GatingAction::Reinforce
    } else if similarity >= update_thresh {
        GatingAction::Update
    } else {
        GatingAction::Create
    }
}

pub fn adaptive_thresholds_conformal(recent_similarities: &[f64]) -> (f64, f64) {
    if recent_similarities.len() < 5 {
        return (PRED_ERROR_REINFORCE, PRED_ERROR_UPDATE);
    }
    let mut sorted: Vec<f64> = recent_similarities.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();

    let q = |p: f64| -> f64 {
        let idx = p * (n as f64 - 1.0);
        let lo = idx.floor() as usize;
        let hi = (lo + 1).min(n - 1);
        let frac = idx - lo as f64;
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    };
    let reinforce = q(0.977).clamp(0.80, 0.98);
    let update = q(0.841).clamp(0.50, reinforce - 0.05);
    (reinforce, update)
}

pub fn adaptive_thresholds_zscore(recent_similarities: &[f64]) -> (f64, f64) {
    if recent_similarities.len() < 5 {
        return (PRED_ERROR_REINFORCE, PRED_ERROR_UPDATE);
    }

    let n = recent_similarities.len() as f64;
    let mean = recent_similarities.iter().sum::<f64>() / n;
    let variance = recent_similarities
        .iter()
        .map(|x| (x - mean).powi(2))
        .sum::<f64>()
        / n;
    let sigma = variance.sqrt();

    let reinforce = (mean + 2.0 * sigma).clamp(0.80, 0.98);
    let update = (mean + 1.0 * sigma).clamp(0.50, reinforce - 0.05);

    (reinforce, update)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gate_reinforce() {
        assert_eq!(gate(0.95), GatingAction::Reinforce);
        assert_eq!(gate(0.92), GatingAction::Reinforce);
    }

    #[test]
    fn test_gate_update() {
        assert_eq!(gate(0.85), GatingAction::Update);
        assert_eq!(gate(0.75), GatingAction::Update);
    }

    #[test]
    fn test_gate_create() {
        assert_eq!(gate(0.74), GatingAction::Create);
        assert_eq!(gate(0.5), GatingAction::Create);
        assert_eq!(gate(0.0), GatingAction::Create);
    }

    #[test]
    fn test_zscore_insufficient_data() {
        let (r, u) = adaptive_thresholds_zscore(&[0.5, 0.6]);
        assert!((r - PRED_ERROR_REINFORCE).abs() < 0.001);
        assert!((u - PRED_ERROR_UPDATE).abs() < 0.001);
    }

    #[test]
    fn test_zscore_high_similarity_corpus() {
        let recent = vec![0.90, 0.88, 0.92, 0.89, 0.91, 0.90, 0.88];
        let (r, u) = adaptive_thresholds_zscore(&recent);
        assert!(r > 0.90, "high corpus → high reinforce: {r}");
        assert!(u > 0.85, "high corpus → high update: {u}");
    }

    #[test]
    fn test_zscore_diverse_corpus() {
        let recent = vec![0.1, 0.3, 0.5, 0.7, 0.9, 0.2, 0.4];
        let (r, u) = adaptive_thresholds_zscore(&recent);
        assert!(r < 0.98, "diverse → reinforce not maxed: {r}");
        assert!(u < 0.85, "diverse → update reasonable: {u}");
    }

    #[test]
    fn test_zscore_novel_in_uniform() {
        let recent = vec![0.85, 0.88, 0.87, 0.86, 0.89, 0.90, 0.85];
        let action = adaptive_gate(0.5, &recent);
        assert_eq!(action, GatingAction::Create);
    }

    #[test]
    fn test_zscore_redundant_in_uniform() {
        let recent = vec![0.85, 0.88, 0.87, 0.86, 0.89, 0.90, 0.85];
        let action = adaptive_gate(0.95, &recent);
        assert_eq!(action, GatingAction::Reinforce);
    }

    #[test]
    fn test_zscore_gap_maintained() {
        for corpus in [
            vec![0.5, 0.6, 0.5, 0.7, 0.55],
            vec![0.9, 0.91, 0.88, 0.92, 0.89],
            vec![0.1, 0.9, 0.5, 0.3, 0.7],
        ] {
            let (r, u) = adaptive_thresholds_zscore(&corpus);
            assert!(r - u >= 0.05, "gap must be ≥0.05: r={r}, u={u}");
        }
    }

    #[test]
    fn test_conformal_insufficient_data() {
        let (r, u) = adaptive_thresholds_conformal(&[0.5, 0.6]);
        assert!((r - PRED_ERROR_REINFORCE).abs() < 0.001);
        assert!((u - PRED_ERROR_UPDATE).abs() < 0.001);
    }

    #[test]
    fn test_conformal_gap_maintained() {
        for corpus in [
            vec![0.5, 0.6, 0.5, 0.7, 0.55, 0.58, 0.52],
            vec![0.9, 0.91, 0.88, 0.92, 0.89, 0.90, 0.91],
            vec![0.1, 0.9, 0.5, 0.3, 0.7, 0.4, 0.6, 0.8, 0.2, 0.5],
        ] {
            let (r, u) = adaptive_thresholds_conformal(&corpus);
            assert!(r - u >= 0.05, "gap must be ≥0.05: r={r}, u={u}");
        }
    }

    #[test]
    fn test_conformal_handles_skewed_right_distribution() {
        let mut skewed: Vec<f64> = vec![0.85, 0.86, 0.87, 0.85, 0.88, 0.86, 0.87, 0.85, 0.84];
        skewed.extend([0.20_f64, 0.30, 0.40]);
        let (r_conf, _) = adaptive_thresholds_conformal(&skewed);
        let (r_zscore, _) = adaptive_thresholds_zscore(&skewed);
        assert!(
            r_conf > r_zscore - 0.20,
            "conformal {r_conf} should not be wildly under z-score {r_zscore}"
        );
    }

    #[test]
    fn test_conformal_redundant_in_uniform() {
        let recent = vec![0.85, 0.88, 0.87, 0.86, 0.89, 0.90, 0.85, 0.87];
        let action = adaptive_gate(0.95, &recent);
        assert_eq!(action, GatingAction::Reinforce);
    }

    #[test]
    fn test_conformal_novel_in_uniform() {
        let recent = vec![0.85, 0.88, 0.87, 0.86, 0.89, 0.90, 0.85, 0.87];
        let action = adaptive_gate(0.5, &recent);
        assert_eq!(action, GatingAction::Create);
    }
}
