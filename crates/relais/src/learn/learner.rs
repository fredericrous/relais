//! The owned learners (SPEC §16): regularized logistic regression for
//! acceptance without escalation, and a separate regularized cost
//! estimator over complete execution strategies. No upstream pretrained
//! router weights or scores are loaded (SPEC §21). Numerically stable
//! loss, explicit convergence criteria, seeded ordering, bounded
//! iterations, reproducibility through recorded solver settings and
//! seed.

use serde::{Deserialize, Serialize};

use super::features::{dot, SparseVec};
use crate::rng::SplitMix64;

pub const LEARNER_SCHEMA_VERSION: u32 = 1;

/// Numerically stable logistic loss contribution for one sample:
/// max(z,0) − y·z + log1p(exp(−|z|)) — never computes log(sigmoid) on a
/// saturated argument.
fn logistic_loss(z: f64, y: f64) -> f64 {
    let positive = if z > 0.0 { z } else { 0.0 };
    positive - y * z + (1.0 + (-z.abs()).exp()).ln()
}

fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SolverSettings {
    pub lambda: f64,
    pub max_iterations: usize,
    pub tolerance: f64,
    pub seed: u64,
}

impl Default for SolverSettings {
    fn default() -> Self {
        Self {
            lambda: 0.01,
            max_iterations: 2_000,
            tolerance: 1e-6,
            seed: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogisticModel {
    pub dim: usize,
    pub weights: Vec<f64>,
    pub bias: f64,
}

impl LogisticModel {
    /// Full-batch gradient descent with backtracking line search, seeded
    /// sample order, bounded iterations, and convergence declared only
    /// when the gradient norm and the relative loss change fall below
    /// tolerance. Reproducible: settings + seed are recorded in the
    /// artifact.
    pub fn fit(
        features: &[SparseVec],
        labels: &[f64],
        settings: SolverSettings,
    ) -> (Self, FitReport) {
        assert_eq!(features.len(), labels.len(), "features and labels align");
        let dim = features
            .iter()
            .map(|features| {
                features
                    .0
                    .iter()
                    .map(|(index, _)| index + 1)
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        let mut rng = SplitMix64::new(settings.seed);
        let mut order: Vec<usize> = (0..features.len()).collect();
        rng.shuffle(&mut order);

        let mut weights = vec![0.0f64; dim];
        let mut bias = 0.0f64;
        let n = features.len() as f64;

        let loss_at = |weights: &[f64], bias: f64| -> f64 {
            let mut total = 0.0;
            for (features, label) in features.iter().zip(labels) {
                total += logistic_loss(dot(features, weights) + bias, *label);
            }
            total / n + settings.lambda * weights.iter().map(|w| w * w).sum::<f64>() / 2.0
        };

        let mut report = FitReport {
            iterations: 0,
            converged: false,
            final_loss: loss_at(&weights, bias),
        };
        let mut step = 0.5;
        for iteration in 0..settings.max_iterations {
            report.iterations = iteration + 1;
            let mut gradient = vec![0.0f64; dim];
            let mut gradient_bias = 0.0f64;
            for &sample in &order {
                let error = sigmoid(dot(&features[sample], &weights) + bias) - labels[sample];
                for (index, value) in &features[sample].0 {
                    if let Some(slot) = gradient.get_mut(*index) {
                        *slot += error * value;
                    }
                }
                gradient_bias += error;
            }
            for (slot, weight) in gradient.iter_mut().zip(&weights) {
                *slot = *slot / n + settings.lambda * weight;
            }
            gradient_bias /= n;

            let grad_norm: f64 =
                gradient.iter().map(|g| g * g).sum::<f64>() + gradient_bias * gradient_bias;
            if grad_norm.sqrt() < settings.tolerance {
                report.converged = true;
                report.final_loss = loss_at(&weights, bias);
                break;
            }

            let previous = loss_at(&weights, bias);
            // Backtracking line search: never accept an uphill step.
            let accepted = 'accepted: {
                for _ in 0..20 {
                    let candidate: Vec<f64> = gradient
                        .iter()
                        .zip(&weights)
                        .map(|(g, w)| w - step * g)
                        .collect();
                    let candidate_bias = bias - step * gradient_bias;
                    if loss_at(&candidate, candidate_bias) <= previous {
                        weights = candidate;
                        bias = candidate_bias;
                        break 'accepted true;
                    }
                    step *= 0.5;
                }
                false
            };
            let current = loss_at(&weights, bias);
            if !accepted
                || (previous - current).abs() < settings.tolerance * previous.abs().max(1e-12)
            {
                report.converged = accepted;
                report.final_loss = current;
                break;
            }
            step = (step * 1.2).min(1.0);
        }
        report.final_loss = loss_at(&weights, bias);
        (Self { dim, weights, bias }, report)
    }

    pub fn predict_proba(&self, features: &SparseVec) -> f64 {
        sigmoid(dot(features, &self.weights) + self.bias)
    }
}

/// Regularized cost estimator over complete execution strategies, fit in
/// log1p space and predicted back with expm1: skewed costs do not drag a
/// least-squares fit into negative predictions (SPEC §16). Empirical
/// cohort estimates are retained as a baseline the evaluator compares
/// against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostModel {
    pub dim: usize,
    pub weights: Vec<f64>,
    pub bias: f64,
    /// Empirical per-cohort means, the retained baseline (SPEC §16).
    pub cohort_means: Vec<(String, f64)>,
}

impl CostModel {
    pub fn fit(
        features: &[SparseVec],
        costs_micros: &[f64],
        cohorts: &[String],
        settings: SolverSettings,
    ) -> (Self, FitReport) {
        let log_targets: Vec<f64> = costs_micros
            .iter()
            .map(|c| (c.max(0.0) + 1.0).ln())
            .collect();
        let mut cohort_means: Vec<(String, f64)> = Vec::new();
        let mut counts: Vec<(String, usize)> = Vec::new();
        for (cohort, cost) in cohorts.iter().zip(costs_micros) {
            match counts.iter_mut().find(|(name, _)| name == cohort) {
                Some((_, count)) => {
                    *count += 1;
                    let total = cohort_means
                        .iter_mut()
                        .find(|(name, _)| name == cohort)
                        .expect("counts and means stay aligned");
                    total.1 += cost;
                }
                None => {
                    counts.push((cohort.clone(), 1));
                    cohort_means.push((cohort.clone(), *cost));
                }
            }
        }
        for (mean, (_, count)) in cohort_means.iter_mut().zip(&counts) {
            mean.1 /= (*count).max(1) as f64;
        }
        cohort_means.sort_by(|a, b| a.0.cmp(&b.0));

        let dim = features
            .iter()
            .map(|features| {
                features
                    .0
                    .iter()
                    .map(|(index, _)| index + 1)
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        let n = features.len() as f64;
        let mut weights = vec![0.0f64; dim];
        let mut bias = 0.0f64;
        let mut rng = SplitMix64::new(settings.seed);
        let mut order: Vec<usize> = (0..features.len()).collect();
        rng.shuffle(&mut order);
        let mut step = 0.5;
        let mut report = FitReport {
            iterations: 0,
            converged: false,
            final_loss: 0.0,
        };
        let predict =
            |weights: &[f64], bias: f64, features: &SparseVec| dot(features, weights) + bias;
        for iteration in 0..settings.max_iterations {
            report.iterations = iteration + 1;
            let mut gradient = vec![0.0f64; dim];
            let mut gradient_bias = 0.0f64;
            for &sample in &order {
                let error = predict(&weights, bias, &features[sample]) - log_targets[sample];
                for (index, value) in &features[sample].0 {
                    if let Some(slot) = gradient.get_mut(*index) {
                        *slot += error * value;
                    }
                }
                gradient_bias += error;
            }
            for (slot, weight) in gradient.iter_mut().zip(&weights) {
                *slot = *slot / n + settings.lambda * weight;
            }
            gradient_bias /= n;
            let grad_norm: f64 =
                gradient.iter().map(|g| g * g).sum::<f64>() + gradient_bias * gradient_bias;
            if grad_norm.sqrt() < settings.tolerance {
                report.converged = true;
                break;
            }
            let previous = weights
                .iter()
                .zip(&gradient)
                .map(|(w, g)| w - step * g)
                .collect::<Vec<f64>>();
            let candidate_bias = bias - step * gradient_bias;
            let previous_loss: f64 = (0..features.len())
                .map(|sample| {
                    let error = predict(&weights, bias, &features[sample]) - log_targets[sample];
                    error * error
                })
                .sum::<f64>()
                / n;
            let candidate_loss: f64 = (0..features.len())
                .map(|sample| {
                    let error =
                        predict(&previous, candidate_bias, &features[sample]) - log_targets[sample];
                    error * error
                })
                .sum::<f64>()
                / n;
            if candidate_loss <= previous_loss {
                weights = previous;
                bias = candidate_bias;
                if (previous_loss - candidate_loss).abs()
                    < settings.tolerance * previous_loss.abs().max(1e-12)
                {
                    report.converged = true;
                    break;
                }
                step = (step * 1.2).min(1.0);
            } else {
                step *= 0.5;
                if step < 1e-9 {
                    break;
                }
            }
            report.final_loss = candidate_loss;
        }
        (
            Self {
                dim,
                weights,
                bias,
                cohort_means,
            },
            report,
        )
    }

    pub fn predict(&self, features: &SparseVec, cohort: Option<&str>) -> f64 {
        let log_prediction = dot(features, &self.weights) + self.bias;
        let prediction = log_prediction.clamp(-20.0, 20.0).exp_m1();
        if prediction.is_finite() {
            prediction.max(0.0)
        } else {
            cohort
                .and_then(|cohort| {
                    self.cohort_means
                        .iter()
                        .find(|(name, _)| name == cohort)
                        .map(|(_, mean)| *mean)
                })
                .unwrap_or(0.0)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FitReport {
    pub iterations: usize,
    pub converged: bool,
    pub final_loss: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::features::{
        expand, feature_dim, FeatureSchema, Standardization, TaskFeatures,
    };

    /// Two well-separated classes in one feature: the learner must reach
    /// separation and predict with correct sides.
    fn make_data(positive: f64, negative: f64, count: usize) -> (Vec<SparseVec>, Vec<f64>) {
        let mut features = Vec::new();
        let mut labels = Vec::new();
        for i in 0..count {
            features.push(SparseVec(vec![(0, negative)]));
            labels.push(0.0);
            features.push(SparseVec(vec![(0, positive)]));
            labels.push(1.0);
            let _ = i;
        }
        (features, labels)
    }

    #[test]
    fn logistic_fit_converges_on_a_separable_fixture() {
        let (features, labels) = make_data(3.0, -3.0, 8);
        // Regularized: the unique finite minimizer lets the gradient-norm
        // criterion actually fire (unregularized separable data has its
        // optimum at infinity and keeps improving until the iteration
        // bound, which the report states honestly).
        let (model, report) = LogisticModel::fit(
            &features,
            &labels,
            SolverSettings {
                lambda: 0.01,
                max_iterations: 5_000,
                tolerance: 1e-6,
                seed: 7,
            },
        );
        assert!(report.converged, "regularized separable data must converge");
        assert!(model.predict_proba(&features[0]) < 0.2);
        assert!(model.predict_proba(&features[1]) > 0.8);
    }

    #[test]
    fn logistic_loss_is_finite_at_extreme_logits() {
        assert!(logistic_loss(500.0, 1.0).is_finite());
        assert!(logistic_loss(-500.0, 1.0).is_finite());
        assert!(sigmoid(-500.0) < 1e-100);
        assert!(1.0 - sigmoid(500.0) < 1e-10);
    }

    #[test]
    fn objective_and_gradient_agree_numerically() {
        // The gradient of the stable loss equals the error-weighted sum:
        // checked against a finite-difference of the loss.
        let (features, labels) = make_data(2.0, -2.0, 4);
        let settings = SolverSettings {
            lambda: 0.05,
            max_iterations: 200,
            tolerance: 1e-8,
            seed: 1,
        };
        let (model, report) = LogisticModel::fit(&features, &labels, settings);
        let eps = 1e-4;
        let mut weights = model.weights.clone();
        let base = {
            let mut total = 0.0;
            for (features, label) in features.iter().zip(&labels) {
                total += logistic_loss(dot(features, &weights) + model.bias, *label);
            }
            total / features.len() as f64
                + settings.lambda * weights.iter().map(|w| w * w).sum::<f64>() / 2.0
        };
        weights[0] += eps;
        let bumped = {
            let mut total = 0.0;
            for (features, label) in features.iter().zip(&labels) {
                total += logistic_loss(dot(features, &weights) + model.bias, *label);
            }
            total / features.len() as f64
                + settings.lambda * weights.iter().map(|w| w * w).sum::<f64>() / 2.0
        };
        let numeric_gradient = (bumped - base) / eps;
        // At convergence the analytic gradient is ~0; the numeric gradient
        // of the same loss must also be ~0. Both statements agree only if
        // the analytic gradient used by the solver is the true one.
        assert!(
            numeric_gradient.abs() < 0.05,
            "numeric gradient at the fitted point: {numeric_gradient}"
        );
        assert!(report.final_loss.is_finite());
    }

    #[test]
    fn cost_model_stays_nonnegative_on_skewed_costs() {
        let features = vec![
            SparseVec(vec![(0, 1.0)]),
            SparseVec(vec![(0, 2.0)]),
            SparseVec(vec![(0, 10.0)]),
        ];
        let costs = vec![1.0, 100.0, 1_000_000.0];
        let cohorts = vec!["a".to_string(); 3];
        let (model, _report) =
            CostModel::fit(&features, &costs, &cohorts, SolverSettings::default());
        for (feature, cost) in features.iter().zip(&costs) {
            let prediction = model.predict(feature, Some("a"));
            assert!(prediction.is_finite() && prediction >= 0.0);
            let _ = cost;
        }
        // Cohort means are retained as the baseline.
        assert!(model.cohort_means.iter().any(|(name, _)| name == "a"));
    }

    #[test]
    fn fits_are_seed_reproducible() {
        let (features, labels) = make_data(2.0, -2.0, 6);
        let settings = SolverSettings {
            lambda: 0.01,
            max_iterations: 50,
            tolerance: 1e-10,
            seed: 42,
        };
        let (a, _) = LogisticModel::fit(&features, &labels, settings);
        let (b, _) = LogisticModel::fit(&features, &labels, settings);
        assert_eq!(a, b, "same seed, same fit");
    }

    #[test]
    fn standardization_applies_identically_at_train_and_inference() {
        let schema = FeatureSchema::standard();
        let task = TaskFeatures {
            kind_change: 1.0,
            kind_inspect: 0.0,
            scope_patterns: 1.0,
            scope_wildcards: 1.0,
            read_hints: 2.0,
            acceptance_count: 3.0,
            risk_hints: 0.0,
            verification_commands: 1.0,
            verification_amont_checks: 0.0,
            architecture_keys: 0.0,
            objective_len: 42.0,
        };
        let a = expand(
            &task,
            crate::policy::Tier::Implementation,
            "some objective",
            &schema,
        );
        let standardization = Standardization::fit(std::slice::from_ref(&a), feature_dim(&schema));
        let applied = standardization.apply(&a);
        let again = standardization.apply(&expand(
            &task,
            crate::policy::Tier::Implementation,
            "some objective",
            &schema,
        ));
        assert_eq!(applied, again);
    }
}
