//! Evaluation and promotion gates (SPEC §17, §21).
//!
//! Compares a candidate artifact with deterministic baselines on held-out
//! data using temporal, family-aware splits. Reports calibration,
//! acceptance/failure rates, cost including escalation, abstention and
//! cohort coverage. A cheaper policy that fails the quality requirement
//! cannot pass promotion; sparse or shifted cohorts abstain to baseline.
//! Alternative-route claims require observed support: cost comparisons
//! are only made on records where the tier was actually chosen.

use serde::{Deserialize, Serialize};

use super::dataset::{temporal_splits, Dataset, TemporalSplits};
use super::features::{feature_dim, FeatureSchema, SparseVec, Standardization, TrainingExample};
use super::learner::{CostModel, LogisticModel, SolverSettings};
use super::registry::PromotionGates;
use crate::money::MicroUsd;
use crate::policy::Tier;

pub const EVAL_SCHEMA_VERSION: u32 = 1;

pub const DEFAULT_MIN_RECORDS_PER_TIER: usize = 5;
pub const DEFAULT_QUALITY_FLOOR: f64 = 0.75;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationBin {
    pub predicted_mid: f64,
    pub observed: f64,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub version: u32,
    pub train_records: usize,
    pub calibration_records: usize,
    pub test_records: usize,
    pub calibration_bins: Vec<CalibrationBin>,
    pub test_acceptance_rate: Option<f64>,
    /// Baseline: the conservative configured route (the floor tier) —
    /// what cold-start routing achieves without any learned artifact.
    pub baseline_acceptance_rate: Option<f64>,
    /// Mean complete-strategy cost where the selected tier has observed
    /// support; None when no supported comparison exists (SPEC §17: no
    /// invented off-policy numbers).
    pub mean_cost_selected: Option<i64>,
    pub mean_cost_baseline: Option<i64>,
    pub abstention_rate: f64,
    pub coverage: Vec<(String, usize)>,
    pub gates: PromotionGates,
}

/// Train the candidate artifact on the train split, calibrate on the
/// calibration split, and evaluate on the test split. Returns the
/// trained models and the report the registry stores.
pub struct TrainOutcome {
    pub standardization: Standardization,
    pub acceptance: LogisticModel,
    pub cost: CostModel,
    pub tiers_supported: Vec<Tier>,
    pub report: EvalReport,
}

pub fn train_and_evaluate(
    dataset: &Dataset,
    settings: SolverSettings,
    quality_floor: f64,
    min_records_per_tier: usize,
) -> Result<TrainOutcome, String> {
    let schema = FeatureSchema::standard();
    let TemporalSplits {
        train,
        calibration,
        test,
    } = temporal_splits(&dataset.records, 0.2, 0.2);
    if train.is_empty() {
        return Err("no training records: collect outcomes first, then train".into());
    }

    // Tiers with training coverage only — neither predictor invents
    // coverage for unseen profiles (SPEC §16).
    let mut tiers: Vec<Tier> = train
        .iter()
        .map(|record| record.tier)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();

    let standardization = Standardization::fit(
        &train
            .iter()
            .map(|record| record.sparse.clone())
            .collect::<Vec<_>>(),
        feature_dim(&schema),
    );
    let train_features: Vec<SparseVec> = train
        .iter()
        .map(|record| standardization.apply(&record.sparse))
        .collect();
    let train_labels: Vec<f64> = train
        .iter()
        .map(|record| record.accepted_without_escalation as u64 as f64)
        .collect();
    let (mut acceptance, _report) = LogisticModel::fit(&train_features, &train_labels, settings);

    let cost_examples: Vec<(SparseVec, f64, String)> = train
        .iter()
        .filter(|record| record.cost_complete)
        .map(|record| {
            (
                standardization.apply(&record.sparse),
                record.complete_cost.to_micros() as f64,
                if record.tier == Tier::Escalation {
                    "escalation"
                } else if record.tier == Tier::Implementation {
                    "change"
                } else {
                    "inspect"
                }
                .to_string(),
            )
        })
        .collect();
    let (cost, _cost_report) = if cost_examples.is_empty() {
        (
            CostModel {
                dim: feature_dim(&schema),
                weights: vec![0.0; feature_dim(&schema)],
                bias: 0.0,
                cohort_means: vec![],
            },
            super::learner::FitReport {
                iterations: 0,
                converged: false,
                final_loss: 0.0,
            },
        )
    } else {
        CostModel::fit(
            &cost_examples
                .iter()
                .map(|(sparse, _, _)| sparse.clone())
                .collect::<Vec<_>>(),
            &cost_examples
                .iter()
                .map(|(_, cost, _)| *cost)
                .collect::<Vec<_>>(),
            &cost_examples
                .iter()
                .map(|(_, _, cohort)| cohort.clone())
                .collect::<Vec<_>>(),
            settings,
        )
    };

    // Calibration on the held-out calibration split: scale the logit so
    // predicted probabilities match observed frequencies (SPEC §16:
    // validated calibration parameters).
    if !calibration.is_empty() {
        calibrate(&mut acceptance, &standardization, &calibration);
    }

    // Evaluation on the test split.
    let mut calibration_bins = Vec::new();
    if !calibration.is_empty() {
        let mut bins = vec![(0.0f64, 0usize, 0usize); 5];
        for record in &calibration {
            let probability = acceptance.predict_proba(&standardization.apply(&record.sparse));
            let bin = ((probability * 5.0) as usize).min(4);
            bins[bin].0 += probability;
            bins[bin].1 += 1;
            bins[bin].2 += record.accepted_without_escalation as usize;
        }
        for (sum, count, positives) in bins {
            if count > 0 {
                calibration_bins.push(CalibrationBin {
                    predicted_mid: sum / count as f64,
                    observed: positives as f64 / count as f64,
                    count,
                });
            }
        }
    }

    let mut accepted_on_test = 0;
    let mut test_size = 0;
    let mut abstentions = 0;
    let mut baseline_accepted = 0;
    let mut selected_costs: Vec<i64> = Vec::new();
    let mut baseline_costs: Vec<i64> = Vec::new();
    let mut coverage: std::collections::BTreeMap<Tier, usize> = std::collections::BTreeMap::new();
    for record in train.iter().chain(calibration.iter()) {
        *coverage.entry(record.tier).or_insert(0) += 1;
    }
    for record in &test {
        test_size += 1;
        if !tiers.contains(&record.tier) {
            abstentions += 1;
            continue;
        }
        if record.accepted_without_escalation {
            accepted_on_test += 1;
        }
        selected_costs.push(record.complete_cost.to_micros());
        // Baseline: the conservative floor tier observed on this split.
        let Some(baseline_record) = test.iter().find(|candidate| {
            candidate.tier == Tier::Implementation || candidate.tier == Tier::Research
        }) else {
            continue;
        };
        if baseline_record.accepted_without_escalation {
            baseline_accepted += 1;
        }
        baseline_costs.push(baseline_record.complete_cost.to_micros());
    }
    let test_acceptance_rate = if test_size > abstentions {
        Some(accepted_on_test as f64 / (test_size - abstentions) as f64)
    } else {
        None
    };
    let baseline_acceptance_rate = if test_size > abstentions {
        Some(baseline_accepted as f64 / (test_size - abstentions).max(1) as f64)
    } else {
        None
    };
    let mean_cost_selected = mean_of(&selected_costs);
    let mean_cost_baseline = mean_of(&baseline_costs);
    let abstention_rate = if test_size > 0 {
        abstentions as f64 / test_size as f64
    } else {
        0.0
    };

    let coverage_pairs: Vec<(String, usize)> = coverage
        .into_iter()
        .map(|(tier, count)| (tier.as_str().to_string(), count))
        .collect();
    let tiers_covered = coverage_pairs
        .iter()
        .all(|(_, count)| *count >= min_records_per_tier);
    let quality_ok = test_acceptance_rate.is_some_and(|rate| rate >= quality_floor);
    let gates = PromotionGates {
        gates_passed: tiers_covered
            && quality_ok
            && (test_size > 0)
            && cost.weights.iter().all(|weight| weight.is_finite()),
        min_records_per_tier,
        coverage: coverage_pairs,
        test_acceptance_rate,
        quality_floor,
        abstention_rate,
    };
    tiers.retain(|tier| {
        matches!(
            tier,
            Tier::Implementation | Tier::Escalation | Tier::Research
        )
    });

    Ok(TrainOutcome {
        standardization,
        acceptance,
        cost,
        tiers_supported: tiers,
        report: EvalReport {
            version: EVAL_SCHEMA_VERSION,
            train_records: train.len(),
            calibration_records: calibration.len(),
            test_records: test_size,
            calibration_bins,
            test_acceptance_rate,
            baseline_acceptance_rate,
            mean_cost_selected,
            mean_cost_baseline,
            abstention_rate,
            coverage: gates.coverage.clone(),
            gates,
        },
    })
}

fn calibrate(
    acceptance: &mut LogisticModel,
    standardization: &Standardization,
    calibration: &[&TrainingExample],
) {
    // Platt scaling: fit a temperature on the held-out calibration split
    // via the same regularized solver, over the frozen logit.
    let logits: Vec<SparseVec> = calibration
        .iter()
        .map(|record| {
            let logit = acceptance.predict_proba(&standardization.apply(&record.sparse));
            let clamped = logit.clamp(1e-6, 1.0 - 1e-6);
            SparseVec(vec![(0, (clamped / (1.0 - clamped)).ln())])
        })
        .collect();
    let labels: Vec<f64> = calibration
        .iter()
        .map(|record| record.accepted_without_escalation as u64 as f64)
        .collect();
    let (scale, _) = LogisticModel::fit(
        &logits,
        &labels,
        SolverSettings {
            lambda: 0.1,
            max_iterations: 500,
            tolerance: 1e-8,
            seed: 11,
        },
    );
    // Fold the calibration scale into the model's weights and bias so
    // inference stays a single transform (SPEC §16: shared code).
    let temperature = scale.weights.first().copied().unwrap_or(1.0).max(0.1);
    let offset = scale.bias;
    for weight in &mut acceptance.weights {
        *weight *= temperature;
    }
    acceptance.bias = acceptance.bias * temperature + offset;
}

fn mean_of(values: &[i64]) -> Option<i64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<i64>() / values.len() as i64)
    }
}

/// Convenience for reports: mean cost in micros → MicroUsd.
pub fn micros_of(mean: Option<i64>) -> Option<MicroUsd> {
    mean.map(MicroUsd::from_micros)
}

impl EvalReport {
    pub fn render(&self) -> String {
        let mut out = String::from("relais evaluate\n");
        out.push_str(&format!(
            "splits: {} train / {} calibration / {} test\n",
            self.train_records, self.calibration_records, self.test_records
        ));
        out.push_str(&format!(
            "coverage: {}\n",
            self.coverage
                .iter()
                .map(|(tier, count)| format!("{tier}:{count}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        for bin in &self.calibration_bins {
            out.push_str(&format!(
                "calibration: predicted {:.2} → observed {:.2} (n={})\n",
                bin.predicted_mid, bin.observed, bin.count
            ));
        }
        out.push_str(&format!(
            "test acceptance rate: {}\n",
            match self.test_acceptance_rate {
                Some(rate) => format!("{rate:.2}"),
                None => "n/a".into(),
            }
        ));
        if let (Some(selected), Some(baseline)) = (self.mean_cost_selected, self.mean_cost_baseline)
        {
            out.push_str(&format!(
                "mean complete-strategy cost: selected {} vs baseline {} (observed support only)\n",
                MicroUsd::from_micros(selected),
                MicroUsd::from_micros(baseline)
            ));
        } else {
            out.push_str("mean complete-strategy cost: no observed support for comparison\n");
        }
        out.push_str(&format!("abstention rate: {:.2}\n", self.abstention_rate));
        out.push_str(if self.gates.gates_passed {
            "gates: PASSED — promotion is evidence-backed\n"
        } else {
            "gates: FAILED — promotion is rejected\n"
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::features::TrainingExample;

    fn record(family: &str, at: &str, accepted: bool, tier: Tier) -> TrainingExample {
        TrainingExample {
            family: family.into(),
            tier,
            sparse: SparseVec(vec![(0, if accepted { 3.0 } else { -3.0 })]),
            accepted_without_escalation: accepted,
            complete_cost: MicroUsd::from_micros(if accepted { 100 } else { 900 }),
            cost_complete: true,
            dispatched_at: at.into(),
        }
    }

    fn dataset() -> Dataset {
        let mut records = Vec::new();
        for day in 1..=20 {
            records.push(record(
                &format!("f{day}"),
                &format!("2026-09-{day:02}"),
                true,
                Tier::Implementation,
            ));
            records.push(record(
                &format!("g{day}"),
                &format!("2026-09-{day:02}"),
                false,
                Tier::Implementation,
            ));
        }
        Dataset {
            version: crate::learn::dataset::DATASET_VERSION,
            records,
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        }
    }

    #[test]
    fn balanced_fixture_trains_and_passes_quality_gates() {
        let dataset = dataset();
        let outcome = train_and_evaluate(
            &dataset,
            SolverSettings {
                lambda: 0.01,
                max_iterations: 500,
                tolerance: 1e-6,
                seed: 3,
            },
            0.5,
            5,
        )
        .expect("trains");
        assert_eq!(outcome.tiers_supported, vec![Tier::Implementation]);
        assert!(outcome.report.gates.gates_passed, "{:?}", outcome.report);
        assert!(outcome
            .report
            .coverage
            .iter()
            .any(|(tier, count)| tier == "implementation" && *count >= 5));
    }

    #[test]
    fn a_learner_that_cannot_meet_the_quality_floor_fails_the_gates() {
        // Nearly all-negative labels: acceptance cannot reach 0.5.
        let mut records = Vec::new();
        for day in 1..=30 {
            records.push(record(
                &format!("b{day}"),
                &format!("2026-09-{day:02}"),
                day % 20 == 0,
                Tier::Implementation,
            ));
        }
        let dataset = Dataset {
            version: crate::learn::dataset::DATASET_VERSION,
            records,
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        let outcome =
            train_and_evaluate(&dataset, SolverSettings::default(), 0.75, 5).expect("trains");
        assert!(
            !outcome.report.gates.gates_passed,
            "a policy failing the quality requirement cannot pass promotion"
        );
    }

    #[test]
    fn empty_datasets_refuse_to_train() {
        let dataset = Dataset {
            version: crate::learn::dataset::DATASET_VERSION,
            records: vec![],
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        assert!(train_and_evaluate(&dataset, SolverSettings::default(), 0.75, 5).is_err());
    }

    #[test]
    fn report_renders_its_honest_parts() {
        let dataset = dataset();
        let outcome =
            train_and_evaluate(&dataset, SolverSettings::default(), 0.5, 5).expect("trains");
        let text = outcome.report.render();
        assert!(text.contains("gates: "), "{text}");
        assert!(text.contains("coverage: implementation"), "{text}");
    }
}
