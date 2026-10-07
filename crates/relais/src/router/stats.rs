//! The interval arithmetic the router's gates and report rest on: Wilson
//! score bounds, the exact (Clopper–Pearson) upper bound a zero-miss gate
//! needs, a median, and a seeded cluster bootstrap. Pure.

use crate::rng::SplitMix64;

/// The two-sided 95% normal quantile.
pub const Z95: f64 = 1.959_963_984_540_054;

/// The Wilson score interval for `successes` of `n` at quantile `z`.
/// `(0, 1)` when `n` is 0: no evidence bounds nothing.
pub fn wilson(successes: u64, n: u64, z: f64) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let n_f = n as f64;
    let p = successes.min(n) as f64 / n_f;
    let z2 = z * z;
    let denominator = 1.0 + z2 / n_f;
    let centre = (p + z2 / (2.0 * n_f)) / denominator;
    let half = z * (p * (1.0 - p) / n_f + z2 / (4.0 * n_f * n_f)).sqrt() / denominator;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// `ln C(n, k)`, by summing logs: exact enough for the sizes R3 has.
fn ln_choose(n: u64, k: u64) -> f64 {
    let k = k.min(n - k);
    (0..k)
        .map(|i| ((n - i) as f64).ln() - ((i + 1) as f64).ln())
        .sum()
}

/// `P(X ≤ k)` for `X ~ Binomial(n, p)`.
fn binomial_cdf(k: u64, n: u64, p: f64) -> f64 {
    if p <= 0.0 {
        return 1.0;
    }
    if p >= 1.0 {
        return if k >= n { 1.0 } else { 0.0 };
    }
    (0..=k.min(n))
        .map(|i| (ln_choose(n, i) + i as f64 * p.ln() + (n - i) as f64 * (1.0 - p).ln()).exp())
        .sum::<f64>()
        .min(1.0)
}

/// The exact Clopper–Pearson upper bound of a two-sided interval at
/// confidence `1 − alpha`: the `p` at which seeing `k` or fewer of `n` has
/// probability `alpha / 2`. At `k = 0` it is `1 − (alpha/2)^(1/n)`, which
/// for alpha 0.05 drops to 0.1 at n = 36 and not before — the plan's
/// missed-failure gate. The Wilson bound is anti-conservative at zero
/// misses (it would pass at 35), so this gate uses the exact one.
pub fn clopper_pearson_upper(k: u64, n: u64, alpha: f64) -> f64 {
    if n == 0 || k >= n {
        return 1.0;
    }
    let target = alpha / 2.0;
    if k == 0 {
        return 1.0 - target.powf(1.0 / n as f64);
    }
    // binomial_cdf(k; n, p) falls as p rises: bisect for the crossing.
    let (mut lo, mut hi) = (k as f64 / n as f64, 1.0);
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        if binomial_cdf(k, n, mid) > target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi
}

/// The median of `values`, the lower of the two middles for an even
/// count (a token count, so never a half). `None` when empty.
pub fn median(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[(values.len() - 1) / 2])
}

/// A cluster bootstrap of the ratio `Σ numerator / Σ denominator`, the
/// clusters (sessions) resampled with replacement: tasks are clustered in
/// sessions and the hold-out is drawn per session, so a task-level
/// resample would understate the interval. The `(1 − alpha)` percentile
/// interval over `resamples` draws from `SplitMix64(seed)`; resamples
/// whose denominator is 0 are skipped. `None` with no cluster, or when
/// every resample was skipped.
pub fn cluster_bootstrap_ratio(
    clusters: &[(f64, f64)],
    seed: u64,
    resamples: usize,
    alpha: f64,
) -> Option<(f64, f64)> {
    if clusters.is_empty() {
        return None;
    }
    let mut rng = SplitMix64::new(seed);
    let n = clusters.len() as u64;
    let mut ratios = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let (mut numerator, mut denominator) = (0.0, 0.0);
        for _ in 0..n {
            let (num, den) = clusters[rng.below(n) as usize];
            numerator += num;
            denominator += den;
        }
        if denominator > 0.0 {
            ratios.push(numerator / denominator);
        }
    }
    if ratios.is_empty() {
        return None;
    }
    ratios.sort_by(f64::total_cmp);
    let at = |q: f64| {
        let index = ((ratios.len() - 1) as f64 * q).round() as usize;
        ratios[index.min(ratios.len() - 1)]
    };
    Some((at(alpha / 2.0), at(1.0 - alpha / 2.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wilson_matches_the_plans_agreement_bound() {
        // 100 of 120 clears 0.75; 99 of 120 does not.
        assert!(
            wilson(100, 120, Z95).0 >= 0.75,
            "{:?}",
            wilson(100, 120, Z95)
        );
        assert!(wilson(99, 120, Z95).0 < 0.75, "{:?}", wilson(99, 120, Z95));
        assert_eq!(wilson(0, 0, Z95), (0.0, 1.0));
    }

    #[test]
    fn the_exact_upper_bound_needs_36_clean_tasks_for_ten_percent() {
        assert!(clopper_pearson_upper(0, 35, 0.05) > 0.1);
        assert!(clopper_pearson_upper(0, 36, 0.05) <= 0.1);
        // Wilson would already pass at 35, which is why it is not used.
        assert!(wilson(0, 35, Z95).1 <= 0.1);
        assert_eq!(clopper_pearson_upper(3, 3, 0.05), 1.0);
    }

    #[test]
    fn the_exact_upper_bound_is_continuous_with_its_closed_form() {
        // 1 of 10: the textbook bound is 0.4450.
        let upper = clopper_pearson_upper(1, 10, 0.05);
        assert!((upper - 0.4450).abs() < 1e-3, "{upper}");
        assert!(clopper_pearson_upper(1, 40, 0.05) > clopper_pearson_upper(0, 40, 0.05));
    }

    #[test]
    fn median_takes_the_lower_middle() {
        assert_eq!(median(&mut []), None);
        assert_eq!(median(&mut [5]), Some(5));
        assert_eq!(median(&mut [9, 1, 5, 3]), Some(3));
    }

    #[test]
    fn the_bootstrap_replays_exactly_from_its_seed_and_brackets_the_ratio() {
        let clusters: Vec<(f64, f64)> = (0..30).map(|i| (100.0 + i as f64, 2.0)).collect();
        let a = cluster_bootstrap_ratio(&clusters, 7, 500, 0.05).unwrap();
        let b = cluster_bootstrap_ratio(&clusters, 7, 500, 0.05).unwrap();
        assert_eq!(a, b);
        let point = clusters.iter().map(|c| c.0).sum::<f64>() / 60.0;
        assert!(a.0 <= point && point <= a.1, "{a:?} {point}");
        assert_eq!(cluster_bootstrap_ratio(&[], 7, 500, 0.05), None);
        assert_eq!(cluster_bootstrap_ratio(&[(1.0, 0.0)], 7, 50, 0.05), None);
    }
}
