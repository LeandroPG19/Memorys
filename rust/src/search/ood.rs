use nalgebra::{DMatrix, DVector};

#[derive(Debug, Clone)]
pub struct OodStats {
    pub mean: DVector<f64>,
    pub inverse_covariance: DMatrix<f64>,
    pub n_samples: usize,
    pub shrinkage: f64,
}

impl OodStats {
    pub fn fit(embeddings: &[Vec<f32>]) -> Option<Self> {
        let n = embeddings.len();
        if n < 2 {
            return None;
        }
        let d = embeddings[0].len();
        if d == 0 || embeddings.iter().any(|e| e.len() != d) {
            return None;
        }

        let mut mean = DVector::<f64>::zeros(d);
        for e in embeddings {
            for (i, &v) in e.iter().enumerate() {
                mean[i] += v as f64;
            }
        }
        mean /= n as f64;

        let mut cov = DMatrix::<f64>::zeros(d, d);
        let mut centered: Vec<DVector<f64>> = Vec::with_capacity(n);
        for e in embeddings {
            let mut diff = DVector::<f64>::zeros(d);
            for (i, &v) in e.iter().enumerate() {
                diff[i] = v as f64 - mean[i];
            }
            cov.syger(1.0, &diff, &diff, 1.0);
            centered.push(diff);
        }
        cov.fill_upper_triangle_with_lower_triangle();
        cov /= (n - 1).max(1) as f64;

        let mu = (0..d).map(|i| cov[(i, i)]).sum::<f64>() / d as f64;

        let mut target_dist = 0.0;
        for i in 0..d {
            for j in 0..d {
                let t = if i == j { mu } else { 0.0 };
                let diff = cov[(i, j)] - t;
                target_dist += diff * diff;
            }
        }
        target_dist /= d as f64;

        let b_bar = mean_squared_outer_deviation(&centered, &cov) / (n * n) as f64;
        let b2 = b_bar.min(target_dist);

        let intensity = if target_dist > f64::EPSILON {
            (b2 / target_dist).clamp(0.0, 1.0)
        } else {
            0.0
        };

        for i in 0..d {
            for j in 0..d {
                let target = if i == j { mu } else { 0.0 };
                cov[(i, j)] = intensity * target + (1.0 - intensity) * cov[(i, j)];
            }
        }

        let floor = (mu * 1e-6).max(f64::MIN_POSITIVE);
        for i in 0..d {
            cov[(i, i)] += floor;
        }

        let inverse_covariance = cov.try_inverse()?;

        Some(Self {
            mean,
            inverse_covariance,
            n_samples: n,
            shrinkage: intensity,
        })
    }

    pub fn mahalanobis(&self, query: &[f32]) -> Option<f64> {
        if query.len() != self.mean.len() {
            return None;
        }
        let mut diff = DVector::<f64>::zeros(query.len());
        for (i, &v) in query.iter().enumerate() {
            diff[i] = v as f64 - self.mean[i];
        }
        let m2 = (diff.transpose() * &self.inverse_covariance * &diff)[(0, 0)];
        Some(m2.max(0.0).sqrt())
    }

    pub fn is_ood(&self, query: &[f32], threshold: f64) -> bool {
        self.mahalanobis(query).is_some_and(|d| d > threshold)
    }
}

fn mean_squared_outer_deviation(centered: &[DVector<f64>], cov: &DMatrix<f64>) -> f64 {
    let d = cov.nrows() as f64;
    let cov_frobenius_sq = cov.norm_squared();
    centered
        .iter()
        .map(|x| {
            let norm_sq = x.norm_squared();
            let quadratic = x.dot(&(cov * x));
            (norm_sq * norm_sq - 2.0 * quadratic + cov_frobenius_sq) / d
        })
        .sum()
}

const Z_99: f64 = 2.326_347_9;

pub fn default_threshold(dim: usize) -> f64 {
    if dim == 0 {
        return 0.0;
    }
    let d = dim as f64;
    let a = 2.0 / (9.0 * d);
    let chi2 = d * (1.0 - a + Z_99 * a.sqrt()).powi(3);
    chi2.max(0.0).sqrt()
}

pub const MIN_SAMPLES_FOR_OOD: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;

    fn covariance_the_old_way(embeddings: &[Vec<f32>], mean: &DVector<f64>) -> DMatrix<f64> {
        let d = mean.len();
        let mut cov = DMatrix::<f64>::zeros(d, d);
        for e in embeddings {
            let mut diff = DVector::<f64>::zeros(d);
            for (i, &v) in e.iter().enumerate() {
                diff[i] = v as f64 - mean[i];
            }
            cov += &diff * diff.transpose();
        }
        cov
    }

    fn covariance_with_syger(embeddings: &[Vec<f32>], mean: &DVector<f64>) -> DMatrix<f64> {
        let d = mean.len();
        let mut cov = DMatrix::<f64>::zeros(d, d);
        for e in embeddings {
            let mut diff = DVector::<f64>::zeros(d);
            for (i, &v) in e.iter().enumerate() {
                diff[i] = v as f64 - mean[i];
            }
            cov.syger(1.0, &diff, &diff, 1.0);
        }
        cov.fill_upper_triangle_with_lower_triangle();
        cov
    }

    fn centered_and_covariance(embeddings: &[Vec<f32>]) -> (Vec<DVector<f64>>, DMatrix<f64>) {
        let n = embeddings.len();
        let d = embeddings[0].len();
        let mut mean = DVector::<f64>::zeros(d);
        for e in embeddings {
            for (i, &v) in e.iter().enumerate() {
                mean[i] += v as f64;
            }
        }
        mean /= n as f64;

        let mut cov = DMatrix::<f64>::zeros(d, d);
        let mut centered = Vec::with_capacity(n);
        for e in embeddings {
            let mut diff = DVector::<f64>::zeros(d);
            for (i, &v) in e.iter().enumerate() {
                diff[i] = v as f64 - mean[i];
            }
            cov.syger(1.0, &diff, &diff, 1.0);
            centered.push(diff);
        }
        cov.fill_upper_triangle_with_lower_triangle();
        cov /= (n - 1).max(1) as f64;
        (centered, cov)
    }

    fn outer_deviation_by_the_naive_double_loop(
        centered: &[DVector<f64>],
        cov: &DMatrix<f64>,
    ) -> f64 {
        let d = cov.nrows();
        let mut total = 0.0;
        for x in centered {
            let mut acc = 0.0;
            for i in 0..d {
                for j in 0..d {
                    let outer = x[i] * x[j];
                    let diff = outer - cov[(i, j)];
                    acc += diff * diff;
                }
            }
            total += acc / d as f64;
        }
        total
    }

    #[test]
    fn closed_form_outer_deviation_matches_the_naive_double_loop() {
        let center: Vec<f32> = (0..96).map(|i| (i as f32) * 0.011 - 0.5).collect();
        let samples = random_around(&center, 150, 0.3);
        let (centered, cov) = centered_and_covariance(&samples);

        let naive = outer_deviation_by_the_naive_double_loop(&centered, &cov);
        let closed = mean_squared_outer_deviation(&centered, &cov);
        let relative = (naive - closed).abs() / naive.abs().max(f64::MIN_POSITIVE);

        assert!(
            relative < 1e-12,
            "closed form gave {closed} against {naive} from the double loop, relative difference \
             {relative:e}. The identity sum_ij (xi*xj - Cij)^2 = ||x||^4 - 2*x'Cx + ||C||_F^2 is \
             exact in real arithmetic but NOT bit for bit in f64: the closed form sums d dot \
             products where the loop sums d^2 scalars, so the rounding order differs. Anything \
             above 1e-12 is not rounding, it is a wrong term"
        );
    }

    #[test]
    fn shrinkage_intensity_survives_the_closed_form_rewrite() {
        let center: Vec<f32> = (0..80).map(|i| (i as f32) * 0.017 - 0.6).collect();
        let samples = random_around(&center, 200, 0.25);
        let (centered, cov) = centered_and_covariance(&samples);
        let n = samples.len();
        let d = cov.nrows();

        let mu = (0..d).map(|i| cov[(i, i)]).sum::<f64>() / d as f64;
        let mut target_dist = 0.0;
        for i in 0..d {
            for j in 0..d {
                let t = if i == j { mu } else { 0.0 };
                let diff = cov[(i, j)] - t;
                target_dist += diff * diff;
            }
        }
        target_dist /= d as f64;

        let intensity_of = |sum: f64| {
            let b2 = (sum / (n * n) as f64).min(target_dist);
            (b2 / target_dist).clamp(0.0, 1.0)
        };

        let naive = intensity_of(outer_deviation_by_the_naive_double_loop(&centered, &cov));
        let fast = intensity_of(mean_squared_outer_deviation(&centered, &cov));
        let stats = OodStats::fit(&samples).expect("fit should succeed");

        assert!(
            fast > 0.0 && fast < 1.0,
            "intensity is saturated at {fast}, so this fixture cannot tell the two \
             implementations apart: b2 = b_bar.min(target_dist) would clamp any b_bar to the same \
             number. Pick a sample count and dimension that leave it strictly inside (0, 1)"
        );
        assert!(
            (naive - fast).abs() < 1e-12,
            "shrinkage intensity moved from {naive} to {fast} with the closed form. This value \
             scales the covariance towards its diagonal target, so it propagates to the inverse, \
             to every Mahalanobis distance and to the abstention verdict"
        );
        assert!(
            (stats.shrinkage - fast).abs() < 1e-12,
            "fit() reported shrinkage {} but the closed form computed here gives {fast}: fit is \
             no longer using this term the way the test reproduces it",
            stats.shrinkage
        );
    }

    #[test]
    fn syger_accumulates_the_same_covariance_bit_for_bit() {
        let center: Vec<f32> = (0..64).map(|i| (i as f32) * 0.013 - 0.4).collect();
        let samples = random_around(&center, 120, 0.35);

        let d = samples[0].len();
        let mut mean = DVector::<f64>::zeros(d);
        for e in &samples {
            for (i, &v) in e.iter().enumerate() {
                mean[i] += v as f64;
            }
        }
        mean /= samples.len() as f64;

        let old = covariance_the_old_way(&samples, &mean);
        let new = covariance_with_syger(&samples, &mean);

        for i in 0..d {
            for j in 0..d {
                assert_eq!(
                    old[(i, j)],
                    new[(i, j)],
                    "cov[{i},{j}] diverged: {} vs {}. syger must reproduce the outer-product \
                     accumulation exactly, including the upper triangle it does not write",
                    old[(i, j)],
                    new[(i, j)]
                );
            }
        }
    }

    #[test]
    fn fit_inverts_a_symmetric_matrix() {
        let center: Vec<f32> = (0..48).map(|i| (i as f32) * 0.021 - 0.5).collect();
        let stats = OodStats::fit(&random_around(&center, 90, 0.4)).expect("fit should succeed");

        let inv = &stats.inverse_covariance;
        let d = inv.nrows();
        for i in 0..d {
            for j in (i + 1)..d {
                let (a, b) = (inv[(i, j)], inv[(j, i)]);
                assert!(
                    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0),
                    "inverse_covariance[{i},{j}]={a} but [{j},{i}]={b}. The covariance was not \
                     symmetric before inversion — the upper triangle syger skips was never \
                     filled in, so half the matrix is zeros"
                );
            }
        }
    }

    fn random_around(center: &[f32], n: usize, jitter: f32) -> Vec<Vec<f32>> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let mut v = center.to_vec();
            for (j, x) in v.iter_mut().enumerate() {
                let h = ((i * 7 + j) as u32).wrapping_mul(2654435761);
                let f = ((h % 1000) as f32 / 1000.0 - 0.5) * 2.0 * jitter;
                *x += f;
            }
            out.push(v);
        }
        out
    }

    #[test]
    fn fit_returns_none_on_insufficient_samples() {
        assert!(OodStats::fit(&[]).is_none());
        assert!(OodStats::fit(&[vec![1.0, 2.0, 3.0]]).is_none());
    }

    #[test]
    fn fit_returns_none_on_inconsistent_dims() {
        let bad = vec![vec![1.0, 2.0], vec![3.0, 4.0, 5.0]];
        assert!(OodStats::fit(&bad).is_none());
    }

    #[test]
    fn threshold_tracks_sqrt_of_dimension() {
        let t384 = default_threshold(384);
        assert!(
            (t384 - 21.25).abs() < 0.1,
            "tau(384) should be ~21.25, got {t384}"
        );
        let t1024 = default_threshold(1024);
        assert!(
            (t1024 - 33.63).abs() < 0.15,
            "tau(1024) should be ~33.63, got {t1024}"
        );
        assert_eq!(default_threshold(0), 0.0);
    }

    #[test]
    fn typical_in_distribution_point_is_not_ood() {
        let d = 64;
        let center = vec![0.5_f32; d];
        let samples = random_around(&center, 400, 0.05);
        let stats = OodStats::fit(&samples).expect("fit");
        let tau = default_threshold(d);

        let flagged = samples.iter().filter(|s| stats.is_ood(s, tau)).count();
        let rate = flagged as f64 / samples.len() as f64;
        assert!(
            rate < 0.05,
            "at most ~1% of in-distribution samples should be OOD at p=0.99, got {:.1}%",
            rate * 100.0
        );

        let flagged_old = samples.iter().filter(|s| stats.is_ood(s, 5.0)).count();
        assert!(
            flagged_old > samples.len() / 2,
            "sanity: the legacy tau=5.0 over-abstains (that was the bug)"
        );
    }

    #[test]
    fn out_of_distribution_query_above_threshold() {
        let d = 16;
        let center = vec![0.0_f32; d];
        let samples = random_around(&center, 200, 0.01);
        let stats = OodStats::fit(&samples).expect("fit");
        let ood = vec![10.0_f32; d];
        let dist = stats.mahalanobis(&ood).expect("mahalanobis");
        assert!(
            dist > default_threshold(d),
            "OOD query should exceed threshold, got {dist}"
        );
    }

    // Worked by hand, with no nalgebra in the arithmetic. The four points have mean 0 and sample covariance
    // diag(2/3, 2/3); that is already its own shrinkage target mu*I (mu = 2/3), so the intensity is 0 and
    // what the fit adds is the floor mu*1e-6 on the diagonal: c = (2/3)(1 + 1e-6). The inverse is
    // diag(1/c, 1/c), so d(2,0)^2 = 4/c and d(1,1)^2 = 2/c, that is sqrt(6/1.000001) and sqrt(3/1.000001),
    // evaluated to 40 digits and rounded to the nearest f64.
    #[test]
    fn a_hand_solved_case_gives_the_distance_worked_out_on_paper() {
        let samples = vec![
            vec![1.0_f32, 0.0],
            vec![-1.0, 0.0],
            vec![0.0, 1.0],
            vec![0.0, -1.0],
        ];
        let stats = Some(samples.as_slice())
            .and_then(OodStats::fit)
            .expect("four points in the plane make a model");

        for (query, expected) in [
            ([2.0_f32, 0.0], 2.4494885180392254_f64),
            ([1.0, 1.0], 1.732049941544123),
        ] {
            let got = stats
                .mahalanobis(&query)
                .expect("same dimension as the model");
            let relative = (got - expected).abs() / expected;
            assert!(
                relative < 1e-12,
                "the distance to {query:?} is {got} and the hand-solved one is {expected}, relative \
                 error {relative:e}. This number is the abstention verdict of faro: a move of the \
                 linear algebra, or of the floor, that shifts it shifts which queries are refused"
            );
        }
    }

    #[test]
    fn wrong_dim_query_returns_none() {
        let center = vec![0.0_f32; 8];
        let samples = random_around(&center, 50, 0.1);
        let stats = OodStats::fit(&samples).expect("fit");
        assert!(stats.mahalanobis(&[0.0_f32; 16]).is_none());
    }
}
