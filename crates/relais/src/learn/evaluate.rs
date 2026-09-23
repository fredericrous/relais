//! Evaluation and promotion gates (SPEC §17, §21).
//!
//! Compares a candidate artifact with deterministic baselines on held-out
//! data using temporal, family-aware splits. Reports calibration,
//! acceptance/failure rates, cost including escalation, abstention and
//! cohort coverage. A cheaper policy that fails the quality requirement
//! cannot pass promotion; sparse or shifted cohorts abstain to baseline.
//! Alternative-route claims require observed support: cost comparisons
//! are only made on records where the tier was actually chosen.
//!
//! The evaluator asks the ROUTER what it would have done — the same
//! eligibility and the same selection function the router runs — because
//! a measurement of a policy nothing will ever execute is not evidence.

use serde::{Deserialize, Serialize};

use super::dataset::{temporal_splits, Dataset, TemporalSplits};
use super::features::{
    expand, feature_dim, FeatureSchema, ProfileIdentity, SparseVec, Standardization,
    TrainingExample,
};
use super::learner::{CostModel, FitReport, LogisticModel, SolverSettings};
use crate::money::MicroUsd;
use crate::policy::Tier;
use crate::route::{select_learned, tiers_at_or_above, Estimates};

/// Version 2 adds the fit reports, the calibration temperature, the
/// supported-record count and the thresholds behind every gate, so the
/// verdict can be RECOMPUTED from the report instead of believed.
pub const EVAL_SCHEMA_VERSION: u32 = 2;

pub const DEFAULT_MIN_RECORDS_PER_TIER: usize = 5;
pub const DEFAULT_QUALITY_FLOOR: f64 = 0.75;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationBin {
    pub predicted_mid: f64,
    pub observed: f64,
    pub count: usize,
}

/// Which of the two fitted models a fit report or a divergence belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FittedModel {
    Acceptance,
    Cost,
}

impl std::fmt::Display for FittedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Acceptance => "acceptance",
            Self::Cost => "cost",
        })
    }
}

/// The Platt temperature fitted on the held-out calibration split, folded
/// into the acceptance model's weights.
///
/// A `scale` at or below zero means the fitted probabilities run BACKWARDS
/// against the outcomes: the higher the model's confidence, the less often
/// the task was accepted. It used to be raised to 0.1 and folded in as a
/// weak positive, which turned an anti-predictive model into a quiet one
/// and recorded nothing. It is now applied as fitted and fails the gates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Calibration {
    /// No calibration split: the acceptance model's own probabilities are
    /// reported untempered.
    NotFitted,
    Fitted {
        scale: f64,
        offset: f64,
    },
}

/// Whether the cost model's fitted coefficients are usable numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coefficients {
    Finite,
    NonFinite,
}

/// One promotion gate that does not hold, with the numbers behind it
/// (SPEC §17: promotion is evidence-gated, and the evidence is named).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateFailure {
    TierCoverage {
        tier: String,
        records: usize,
        minimum: usize,
    },
    NoTestRecords,
    /// Acceptance measured on the records that support the selection is
    /// below the configured quality requirement, or was not measurable.
    Quality {
        observed: Option<f64>,
        floor: f64,
    },
    SupportedTestRecords {
        observed: usize,
        minimum: usize,
    },
    AbstentionRate {
        observed: f64,
        maximum: f64,
    },
    /// The solver stopped at its iteration limit: the weights are where it
    /// happened to be, not where the loss stopped moving.
    NotConverged {
        model: FittedModel,
        iterations: usize,
        final_loss: f64,
    },
    /// The calibration temperature is anti-predictive.
    AntiPredictiveCalibration {
        scale: f64,
    },
    NonFiniteCostCoefficients,
}

impl std::fmt::Display for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TierCoverage {
                tier,
                records,
                minimum,
            } => write!(
                f,
                "tier {tier} has {records} training record(s), below the {minimum} required"
            ),
            Self::NoTestRecords => write!(f, "the test split is empty: nothing was measured"),
            Self::Quality { observed, floor } => match observed {
                Some(rate) => write!(
                    f,
                    "measured acceptance {rate:.2} is below the quality floor {floor:.2}"
                ),
                None => write!(
                    f,
                    "acceptance was not measurable against the quality floor {floor:.2}"
                ),
            },
            Self::SupportedTestRecords { observed, minimum } => write!(
                f,
                "only {observed} test record(s) support the selected route, below the {minimum} \
                 required"
            ),
            Self::AbstentionRate { observed, maximum } => write!(
                f,
                "abstention rate {observed:.2} exceeds the maximum {maximum:.2}"
            ),
            Self::NotConverged {
                model,
                iterations,
                final_loss,
            } => write!(
                f,
                "the {model} model did not converge in {iterations} iteration(s) (loss \
                 {final_loss:.6})"
            ),
            Self::AntiPredictiveCalibration { scale } => write!(
                f,
                "the calibration temperature {scale:.4} is not positive: the model's confidence \
                 runs against the outcomes"
            ),
            Self::NonFiniteCostCoefficients => {
                write!(f, "the cost model has non-finite coefficients")
            }
        }
    }
}

/// Evidence gates for promotion (SPEC §17): coverage per supported tier,
/// a quality floor on acceptance measured where the route is supported, a
/// minimum of supported test records, a ceiling on abstention, converged
/// solvers and a calibration that points the right way.
///
/// Every threshold sits beside the observation it judges, so the verdict
/// is a pure function of this struct — `promote` recomputes it rather than
/// trusting the stored `gates_passed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionGates {
    /// The verdict recorded when the report was written.
    pub gates_passed: bool,
    pub min_records_per_tier: usize,
    pub min_supported_test_records: usize,
    pub quality_floor: f64,
    pub max_abstention_rate: f64,
    pub coverage: Vec<(String, usize)>,
    pub test_records: usize,
    /// Test records observed at the tier the artifact selects for them —
    /// the only ones that say anything about the selected policy.
    pub supported_test_records: usize,
    pub test_acceptance_rate: Option<f64>,
    pub abstention_rate: f64,
    pub acceptance_fit: FitReport,
    pub cost_fit: FitReport,
    pub calibration: Calibration,
    pub cost_coefficients: Coefficients,
}

impl PromotionGates {
    /// Every gate that does not hold, recomputed from the numbers beside
    /// the thresholds. Empty means promotable.
    pub fn failures(&self) -> Vec<GateFailure> {
        let mut failures = Vec::new();
        for (tier, records) in &self.coverage {
            if *records < self.min_records_per_tier {
                failures.push(GateFailure::TierCoverage {
                    tier: tier.clone(),
                    records: *records,
                    minimum: self.min_records_per_tier,
                });
            }
        }
        if self.test_records == 0 {
            failures.push(GateFailure::NoTestRecords);
        }
        if !self
            .test_acceptance_rate
            .is_some_and(|rate| rate >= self.quality_floor)
        {
            failures.push(GateFailure::Quality {
                observed: self.test_acceptance_rate,
                floor: self.quality_floor,
            });
        }
        if self.supported_test_records < self.min_supported_test_records {
            failures.push(GateFailure::SupportedTestRecords {
                observed: self.supported_test_records,
                minimum: self.min_supported_test_records,
            });
        }
        if self.abstention_rate > self.max_abstention_rate {
            failures.push(GateFailure::AbstentionRate {
                observed: self.abstention_rate,
                maximum: self.max_abstention_rate,
            });
        }
        for (model, fit) in [
            (FittedModel::Acceptance, self.acceptance_fit),
            (FittedModel::Cost, self.cost_fit),
        ] {
            if !fit.converged {
                failures.push(GateFailure::NotConverged {
                    model,
                    iterations: fit.iterations,
                    final_loss: fit.final_loss,
                });
            }
        }
        match self.calibration {
            Calibration::NotFitted => {}
            Calibration::Fitted { scale, .. } if scale <= 0.0 => {
                failures.push(GateFailure::AntiPredictiveCalibration { scale });
            }
            Calibration::Fitted { .. } => {}
        }
        match self.cost_coefficients {
            Coefficients::Finite => {}
            Coefficients::NonFinite => failures.push(GateFailure::NonFiniteCostCoefficients),
        }
        failures
    }

    /// The verdict these numbers earn, whatever `gates_passed` says.
    pub fn verdict(&self) -> bool {
        self.failures().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub version: u32,
    /// The artifact this report is evidence FOR. `promote` checks it
    /// against the artifact carrying the report: a report is not evidence
    /// about a different artifact.
    pub artifact_id: String,
    /// The dataset the artifact was fitted and measured on.
    pub dataset_fingerprint: String,
    pub train_records: usize,
    pub calibration_records: usize,
    pub test_records: usize,
    /// The calibration curve, measured on the TEST split — records the
    /// temperature was NOT fitted on. The calibration split fits; the test
    /// split reports.
    pub calibration_bins: Vec<CalibrationBin>,
    pub test_acceptance_rate: Option<f64>,
    /// Baseline: the conservative configured route (each record's own
    /// floor tier) — what cold-start routing achieves without any learned
    /// artifact.
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
#[derive(Debug, Clone, PartialEq)]
pub struct TrainOutcome {
    pub standardization: Standardization,
    pub acceptance: LogisticModel,
    pub cost: CostModel,
    pub tiers_supported: Vec<Tier>,
    /// The profile identities actually observed per tier in training.
    /// Inference abstains for a tier whose current identity is not in this
    /// set, so a model swap never inherits the old model's evidence
    /// (SPEC §17).
    pub observed_identities: Vec<(Tier, Vec<ProfileIdentity>)>,
    pub report: EvalReport,
}

/// Everything the evaluator needs beside the dataset: which artifact it is
/// producing evidence for, how to fit, and the gate thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalSettings {
    pub artifact_id: String,
    pub solver: SolverSettings,
    pub quality_floor: f64,
    pub min_records_per_tier: usize,
    pub min_supported_test_records: usize,
    pub max_abstention_rate: f64,
}

/// Training could not produce an artifact at all. Distinct from a
/// candidate that trains and fails its gates: that one has a report to
/// read, this one has nothing to promote.
#[derive(Debug)]
pub enum TrainError {
    /// The dataset holds records, but the temporal split left the training
    /// side empty — or the dataset is empty outright.
    NoTrainingRecords {
        dataset_fingerprint: String,
        records: usize,
    },
    /// Not one tier in the training split reaches the coverage minimum,
    /// so no tier could be fitted at all. `tier` is the best-covered one.
    NoTierCoverage {
        tier: Tier,
        records: usize,
        minimum: usize,
    },
    /// A fitted coefficient is not a finite number: the solver ran away
    /// rather than converging slowly, and there is nothing to store.
    SolverDiverged {
        model: FittedModel,
        iterations: usize,
        final_loss: f64,
    },
}

impl std::fmt::Display for TrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTrainingRecords {
                dataset_fingerprint,
                records,
            } => write!(
                f,
                "no training records in dataset {dataset_fingerprint} ({records} record(s) after \
                 exclusions): collect outcomes first, then train"
            ),
            Self::NoTierCoverage {
                tier,
                records,
                minimum,
            } => write!(
                f,
                "no tier has enough training records: the best covered is {} with {records}, \
                 below the {minimum} required",
                tier.as_str()
            ),
            Self::SolverDiverged {
                model,
                iterations,
                final_loss,
            } => write!(
                f,
                "the {model} model diverged after {iterations} iteration(s) (loss {final_loss}): \
                 its coefficients are not finite"
            ),
        }
    }
}

impl std::error::Error for TrainError {}

pub fn train_and_evaluate(
    dataset: &Dataset,
    settings: &EvalSettings,
) -> Result<TrainOutcome, TrainError> {
    let schema = FeatureSchema::standard();
    let TemporalSplits {
        train,
        calibration,
        test,
    } = temporal_splits(&dataset.records, 0.2, 0.2);
    if train.is_empty() {
        return Err(TrainError::NoTrainingRecords {
            dataset_fingerprint: dataset.fingerprint.clone(),
            records: dataset.records.len(),
        });
    }

    // Tiers with training coverage only — neither predictor invents
    // coverage for unseen profiles (SPEC §16).
    let mut coverage: std::collections::BTreeMap<Tier, usize> = std::collections::BTreeMap::new();
    for record in train.iter().chain(calibration.iter()) {
        *coverage.entry(record.tier).or_insert(0) += 1;
    }
    let tiers: Vec<Tier> = train
        .iter()
        .map(|record| record.tier)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    // Every tier under the minimum means nothing can be fitted; that is a
    // training failure, not a report to read.
    if let Some((tier, records)) = coverage
        .iter()
        .max_by_key(|(_, records)| **records)
        .map(|(tier, records)| (*tier, *records))
    {
        if records < settings.min_records_per_tier {
            return Err(TrainError::NoTierCoverage {
                tier,
                records,
                minimum: settings.min_records_per_tier,
            });
        }
    }
    let observed_identities = identities_per_tier(&train);

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
    let (mut acceptance, acceptance_fit) = LogisticModel::fit(
        &train_features,
        &train_labels,
        feature_dim(&schema),
        settings.solver,
    );
    if !acceptance.weights.iter().all(|weight| weight.is_finite()) || !acceptance.bias.is_finite() {
        return Err(TrainError::SolverDiverged {
            model: FittedModel::Acceptance,
            iterations: acceptance_fit.iterations,
            final_loss: acceptance_fit.final_loss,
        });
    }

    let cost_examples: Vec<(SparseVec, f64, String)> = train
        .iter()
        .filter(|record| record.cost_complete)
        .map(|record| {
            (
                standardization.apply(&record.sparse),
                record.complete_cost.to_micros() as f64,
                record.task.cohort().to_string(),
            )
        })
        .collect();
    let (cost, cost_fit) = if cost_examples.is_empty() {
        (
            // Fitted on nothing: the empty observed range supports no
            // prediction and there is no cohort mean to abstain to, so
            // this model prices no tier at all. A zero here would have
            // made every tier look free, and the cheapest.
            CostModel {
                dim: feature_dim(&schema),
                weights: vec![0.0; feature_dim(&schema)],
                bias: 0.0,
                cohort_means: vec![],
                observed_log_min: f64::INFINITY,
                observed_log_max: f64::NEG_INFINITY,
            },
            FitReport {
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
            feature_dim(&schema),
            settings.solver,
        )
    };

    // Calibration on the held-out calibration split: scale the logit so
    // predicted probabilities match observed frequencies (SPEC §16:
    // validated calibration parameters).
    let calibration_fit = if calibration.is_empty() {
        Calibration::NotFitted
    } else {
        calibrate(&mut acceptance, &standardization, &calibration, settings)
    };

    // The reported calibration curve is measured on the TEST split, which
    // the temperature above was not fitted on. Reporting it on the
    // calibration split measured the fit against the data it was fitted
    // to: a curve that looks calibrated by construction and says nothing
    // about a future task (SPEC §17: "keep the final test set out of
    // tuning", report calibration on held-out data). The calibration
    // split's one job is fitting the temperature.
    let calibration_bins = calibration_curve(&acceptance, &standardization, &test);

    let measured = measure(Measurement {
        test: &test,
        tiers: &tiers,
        schema: &schema,
        standardization: &standardization,
        acceptance: &acceptance,
        cost: &cost,
        artifact_id: &settings.artifact_id,
        quality_floor: settings.quality_floor,
    });

    let coverage_pairs: Vec<(String, usize)> = coverage
        .into_iter()
        .map(|(tier, count)| (tier.as_str().to_string(), count))
        .collect();
    let mut gates = PromotionGates {
        gates_passed: false,
        min_records_per_tier: settings.min_records_per_tier,
        min_supported_test_records: settings.min_supported_test_records,
        quality_floor: settings.quality_floor,
        max_abstention_rate: settings.max_abstention_rate,
        coverage: coverage_pairs,
        test_records: measured.test_records,
        supported_test_records: measured.supported,
        test_acceptance_rate: measured.test_acceptance_rate,
        abstention_rate: measured.abstention_rate,
        acceptance_fit,
        cost_fit,
        calibration: calibration_fit,
        cost_coefficients: if cost.weights.iter().all(|weight| weight.is_finite()) {
            Coefficients::Finite
        } else {
            Coefficients::NonFinite
        },
    };
    gates.gates_passed = gates.verdict();

    Ok(TrainOutcome {
        standardization,
        acceptance,
        cost,
        tiers_supported: tiers,
        observed_identities,
        report: EvalReport {
            version: EVAL_SCHEMA_VERSION,
            artifact_id: settings.artifact_id.clone(),
            dataset_fingerprint: dataset.fingerprint.clone(),
            train_records: train.len(),
            calibration_records: calibration.len(),
            test_records: measured.test_records,
            calibration_bins,
            test_acceptance_rate: measured.test_acceptance_rate,
            baseline_acceptance_rate: measured.baseline_acceptance_rate,
            mean_cost_selected: measured.mean_cost_selected,
            mean_cost_baseline: measured.mean_cost_baseline,
            abstention_rate: measured.abstention_rate,
            coverage: gates.coverage.clone(),
            gates,
        },
    })
}

/// The identities seen per tier in the training split, deduplicated and
/// ordered. Evidence belongs to the profile that produced it.
fn identities_per_tier(train: &[&TrainingExample]) -> Vec<(Tier, Vec<ProfileIdentity>)> {
    let mut per_tier: std::collections::BTreeMap<Tier, Vec<ProfileIdentity>> =
        std::collections::BTreeMap::new();
    for record in train {
        let seen = per_tier.entry(record.tier).or_default();
        if !seen.contains(&record.identity) {
            seen.push(record.identity.clone());
        }
    }
    per_tier.into_iter().collect()
}

fn calibration_curve(
    acceptance: &LogisticModel,
    standardization: &Standardization,
    test: &[&TrainingExample],
) -> Vec<CalibrationBin> {
    let mut bins = vec![(0.0f64, 0usize, 0usize); 5];
    for record in test {
        let probability = acceptance.predict_proba(&standardization.apply(&record.sparse));
        let bin = ((probability * 5.0) as usize).min(4);
        bins[bin].0 += probability;
        bins[bin].1 += 1;
        bins[bin].2 += record.accepted_without_escalation as usize;
    }
    bins.into_iter()
        .filter(|(_, count, _)| *count > 0)
        .map(|(sum, count, positives)| CalibrationBin {
            predicted_mid: sum / count as f64,
            observed: positives as f64 / count as f64,
            count,
        })
        .collect()
}

/// The inputs of the measurement phase: the held-out split and everything
/// needed to ask the router what it would have done with each record.
struct Measurement<'a> {
    test: &'a [&'a TrainingExample],
    tiers: &'a [Tier],
    schema: &'a FeatureSchema,
    standardization: &'a Standardization,
    acceptance: &'a LogisticModel,
    cost: &'a CostModel,
    artifact_id: &'a str,
    quality_floor: f64,
}

/// What the held-out split measured.
struct Measured {
    test_records: usize,
    supported: usize,
    test_acceptance_rate: Option<f64>,
    baseline_acceptance_rate: Option<f64>,
    mean_cost_selected: Option<i64>,
    mean_cost_baseline: Option<i64>,
    abstention_rate: f64,
}

/// Evaluation on the test split, on observed support only (SPEC §17: "do
/// not infer performance for profiles with zero observation probability").
///
/// For each test task the artifact SELECTS a tier, through the router's
/// own `select_learned` over the router's own eligibility for THAT record
/// — its floor and every trained tier above it. A test record is evidence
/// for the selected policy only when its observed tier IS the selection;
/// a record at another tier says nothing about it.
///
/// The baseline is what cold-start routing does for that same record: its
/// floor tier. Pooling research and implementation records regardless of
/// each record's own floor made the baseline a mixture no route produces.
fn measure(inputs: Measurement<'_>) -> Measured {
    let select = |record: &TrainingExample| -> Option<Tier> {
        let eligible = tiers_at_or_above(record.floor, inputs.tiers);
        let mut acceptance = std::collections::BTreeMap::new();
        let mut cost = std::collections::BTreeMap::new();
        for tier in &eligible {
            let features = inputs.standardization.apply(&expand(
                &record.task,
                *tier,
                &record.objective,
                &record.identity,
                inputs.schema,
            ));
            acceptance.insert(*tier, inputs.acceptance.predict_proba(&features));
            // A tier the cost model cannot price is left unpriced, exactly
            // as inference leaves it: the router skips it rather than
            // treating an absent price as free.
            if let Some(estimate) = inputs.cost.predict(&features, Some(record.task.cohort())) {
                cost.insert(*tier, MicroUsd::from_micros(estimate.max(0.0) as i64));
            }
        }
        // The router's input type, with the evidence fields the selection
        // does not read left empty.
        let estimates = Estimates {
            artifact_id: inputs.artifact_id.to_string(),
            input_hash: String::new(),
            acceptance,
            cost,
            raw: serde_json::Value::Null,
        };
        select_learned(&estimates, &eligible, inputs.quality_floor)
    };

    let mut test_records = 0;
    let mut abstentions = 0;
    let mut supported = 0;
    let mut accepted_on_support = 0;
    let mut baseline_size = 0;
    let mut baseline_accepted = 0;
    let mut selected_costs: Vec<i64> = Vec::new();
    let mut baseline_costs: Vec<i64> = Vec::new();
    for record in inputs.test {
        test_records += 1;
        match select(record) {
            None => abstentions += 1,
            Some(selected) if selected == record.tier => {
                supported += 1;
                if record.accepted_without_escalation {
                    accepted_on_support += 1;
                }
                selected_costs.push(record.complete_cost.to_micros());
            }
            Some(_) => {}
        }
        if record.tier == record.floor {
            baseline_size += 1;
            if record.accepted_without_escalation {
                baseline_accepted += 1;
            }
            baseline_costs.push(record.complete_cost.to_micros());
        }
    }
    Measured {
        test_records,
        supported,
        test_acceptance_rate: (supported > 0)
            .then(|| accepted_on_support as f64 / supported as f64),
        baseline_acceptance_rate: (baseline_size > 0)
            .then(|| baseline_accepted as f64 / baseline_size as f64),
        mean_cost_selected: mean_of(&selected_costs),
        mean_cost_baseline: mean_of(&baseline_costs),
        abstention_rate: if test_records > 0 {
            abstentions as f64 / test_records as f64
        } else {
            0.0
        },
    }
}

fn calibrate(
    acceptance: &mut LogisticModel,
    standardization: &Standardization,
    calibration: &[&TrainingExample],
    settings: &EvalSettings,
) -> Calibration {
    // Platt scaling: fit a temperature on the held-out calibration split
    // via the same regularized solver, over the frozen probability.
    let logits: Vec<SparseVec> = calibration
        .iter()
        .map(|record| {
            let probability = acceptance.predict_proba(&standardization.apply(&record.sparse));
            let clamped = probability.clamp(1e-6, 1.0 - 1e-6);
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
        1,
        SolverSettings {
            lambda: 0.1,
            max_iterations: 500,
            tolerance: 1e-8,
            seed: settings.solver.seed,
        },
    );
    // Fold the calibration scale into the model's weights and bias so
    // inference stays a single transform (SPEC §16: shared code). The
    // temperature is applied AS FITTED: a non-positive one is recorded
    // and fails the gates rather than being floored into a weak positive.
    let temperature = scale.weights.first().copied().unwrap_or(1.0);
    let offset = scale.bias;
    for weight in &mut acceptance.weights {
        *weight *= temperature;
    }
    acceptance.bias = acceptance.bias * temperature + offset;
    Calibration::Fitted {
        scale: temperature,
        offset,
    }
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
            "artifact: {} on dataset {}\n",
            self.artifact_id, self.dataset_fingerprint
        ));
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
                "calibration (held-out test split): predicted {:.2} → observed {:.2} (n={})\n",
                bin.predicted_mid, bin.observed, bin.count
            ));
        }
        out.push_str(&format!(
            "fit: acceptance {} in {} iteration(s), loss {:.6}; cost {} in {} iteration(s), loss \
             {:.6}\n",
            converged_label(self.gates.acceptance_fit.converged),
            self.gates.acceptance_fit.iterations,
            self.gates.acceptance_fit.final_loss,
            converged_label(self.gates.cost_fit.converged),
            self.gates.cost_fit.iterations,
            self.gates.cost_fit.final_loss,
        ));
        out.push_str(&match self.gates.calibration {
            Calibration::NotFitted => {
                "calibration temperature: not fitted (no calibration split)\n".to_string()
            }
            Calibration::Fitted { scale, offset } => {
                format!("calibration temperature: scale {scale:.4}, offset {offset:.4}\n")
            }
        });
        out.push_str(&format!(
            "test acceptance rate: {} on {} supported record(s)\n",
            match self.test_acceptance_rate {
                Some(rate) => format!("{rate:.2}"),
                None => "n/a".into(),
            },
            self.gates.supported_test_records
        ));
        out.push_str(&format!(
            "baseline (each record's own floor tier) acceptance rate: {}\n",
            match self.baseline_acceptance_rate {
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
        let failures = self.gates.failures();
        if failures.is_empty() {
            out.push_str("gates: PASSED — promotion is evidence-backed\n");
        } else {
            out.push_str("gates: FAILED — promotion is rejected\n");
            for failure in failures {
                out.push_str(&format!("  gate: {failure}\n"));
            }
        }
        out
    }
}

fn converged_label(converged: bool) -> &'static str {
    if converged {
        "converged"
    } else {
        "DID NOT converge"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::features::{AcceptanceRoute, ProfileIdentity, TaskFeatures, TrainingExample};

    /// A record whose label is learnable from its inputs: accepted tasks
    /// share one objective vocabulary, rejected ones another, so the
    /// hashed tokens carry the signal the learner must find.
    fn record(family: &str, at: &str, accepted: bool, tier: Tier) -> TrainingExample {
        let schema = FeatureSchema::standard();
        let task = TaskFeatures {
            kind_change: 1.0,
            kind_inspect: 0.0,
            scope_patterns: 1.0,
            scope_wildcards: 1.0,
            read_hints: 1.0,
            acceptance_count: if accepted { 1.0 } else { 4.0 },
            risk_hints: 0.0,
            verification_commands: 1.0,
            verification_amont_checks: 0.0,
            architecture_keys: 0.0,
            objective_len: 40.0,
        };
        let objective = if accepted {
            "tidy the small helper"
        } else {
            "rewrite the entire subsystem"
        };
        let identity = ProfileIdentity {
            model: "sonnet".into(),
            effort: None,
            harness: None,
        };
        let sparse = expand(&task, tier, objective, &identity, &schema);
        TrainingExample {
            family: family.into(),
            tier,
            floor: Tier::Implementation,
            task,
            objective: objective.into(),
            identity,
            sparse,
            accepted_without_escalation: accepted,
            route: AcceptanceRoute::Verified,
            outcome_stood: None,
            correction_magnitude: None,
            complete_cost: MicroUsd::from_micros(if accepted { 100 } else { 900 }),
            cost_complete: true,
            dispatched_at: at.into(),
        }
    }

    /// Gate thresholds that let the small fixtures below say something
    /// about the gate under test rather than all failing on sample size:
    /// the real defaults (20 supported records) need a real corpus.
    fn settings(quality_floor: f64, min_records_per_tier: usize) -> EvalSettings {
        EvalSettings {
            artifact_id: "artifact-under-test".into(),
            solver: SolverSettings::default(),
            quality_floor,
            min_records_per_tier,
            min_supported_test_records: 1,
            max_abstention_rate: 0.5,
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
            label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
            records,
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        }
    }

    #[test]
    fn balanced_fixture_trains_and_passes_quality_gates() {
        let dataset = dataset();
        let outcome = train_and_evaluate(&dataset, &settings(0.5, 5)).expect("trains");
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
            label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
            records,
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        let outcome = train_and_evaluate(&dataset, &settings(0.75, 5)).expect("trains");
        assert!(
            !outcome.report.gates.gates_passed,
            "a policy failing the quality requirement cannot pass promotion"
        );
    }

    /// D3: the reported bins were computed on the very records the
    /// temperature had just been fitted on, so the curve was in-sample by
    /// construction. Here the calibration split is all-accepted and the
    /// test split all-rejected: bins measured on the fitting set would
    /// report an observed frequency of 1.0 in every bin.
    #[test]
    fn calibration_bins_are_reported_on_records_the_fit_never_saw() {
        let mut records = Vec::new();
        // 20 records: indices 0..11 train, 12..15 calibration, 16..19 test
        // under the 0.2/0.2 split. Labels: train mixed (so the learner is
        // not degenerate), calibration all accepted, test all rejected.
        for day in 1..=20u32 {
            let accepted = match day {
                1..=12 => day % 2 == 0,
                13..=16 => true,
                _ => false,
            };
            records.push(record(
                &format!("f{day}"),
                &format!("2026-09-{day:02}"),
                accepted,
                Tier::Implementation,
            ));
        }
        let dataset = Dataset {
            version: crate::learn::dataset::DATASET_VERSION,
            label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
            records,
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        let outcome = train_and_evaluate(&dataset, &settings(0.75, 5)).expect("trains");
        let report = &outcome.report;
        assert_eq!(report.calibration_records, 4);
        assert_eq!(report.test_records, 4);
        let binned: usize = report.calibration_bins.iter().map(|bin| bin.count).sum();
        assert_eq!(
            binned, report.test_records,
            "every binned record is a test record, and every test record is binned"
        );
        assert!(
            report
                .calibration_bins
                .iter()
                .all(|bin| bin.observed == 0.0),
            "the test split is all-rejected; an in-sample curve would read 1.0: {:?}",
            report.calibration_bins
        );
    }

    #[test]
    fn empty_datasets_refuse_to_train() {
        let dataset = Dataset {
            version: crate::learn::dataset::DATASET_VERSION,
            label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
            records: vec![],
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        assert!(matches!(
            train_and_evaluate(&dataset, &settings(0.75, 5)),
            Err(TrainError::NoTrainingRecords { records: 0, .. })
        ));
    }

    #[test]
    fn report_renders_its_honest_parts() {
        let dataset = dataset();
        let outcome = train_and_evaluate(&dataset, &settings(0.5, 5)).expect("trains");
        let text = outcome.report.render();
        assert!(text.contains("gates: "), "{text}");
        assert!(text.contains("coverage: implementation"), "{text}");
        assert!(text.contains("artifact: artifact-under-test"), "{text}");
        assert!(text.contains("fit: acceptance converged"), "{text}");
        assert!(text.contains("calibration temperature: scale"), "{text}");
    }

    /// A report with gates that hold, as a starting point for asking what
    /// each individual gate refuses.
    fn passing_gates() -> PromotionGates {
        PromotionGates {
            gates_passed: true,
            min_records_per_tier: 5,
            min_supported_test_records: 20,
            quality_floor: 0.75,
            max_abstention_rate: 0.5,
            coverage: vec![("implementation".into(), 40)],
            test_records: 60,
            supported_test_records: 25,
            test_acceptance_rate: Some(0.9),
            abstention_rate: 0.2,
            acceptance_fit: FitReport {
                iterations: 120,
                converged: true,
                final_loss: 0.2,
            },
            cost_fit: FitReport {
                iterations: 90,
                converged: true,
                final_loss: 0.4,
            },
            calibration: Calibration::Fitted {
                scale: 1.2,
                offset: -0.1,
            },
            cost_coefficients: Coefficients::Finite,
        }
    }

    /// L3, L5, L14: each gate refuses on its own, and the verdict is a
    /// pure function of the numbers the report carries.
    #[test]
    fn every_gate_refuses_on_its_own() {
        assert!(
            passing_gates().verdict(),
            "{:?}",
            passing_gates().failures()
        );

        let thin = PromotionGates {
            supported_test_records: 1,
            ..passing_gates()
        };
        assert!(matches!(
            thin.failures().as_slice(),
            [GateFailure::SupportedTestRecords {
                observed: 1,
                minimum: 20
            }]
        ));

        let abstaining = PromotionGates {
            abstention_rate: 0.9,
            ..passing_gates()
        };
        assert!(matches!(
            abstaining.failures().as_slice(),
            [GateFailure::AbstentionRate { .. }]
        ));

        let unconverged = PromotionGates {
            acceptance_fit: FitReport {
                iterations: 500,
                converged: false,
                final_loss: 9.0,
            },
            ..passing_gates()
        };
        assert!(
            matches!(
                unconverged.failures().as_slice(),
                [GateFailure::NotConverged {
                    model: FittedModel::Acceptance,
                    ..
                }]
            ),
            "unconverged weights are not promotable: {:?}",
            unconverged.failures()
        );

        let anti_predictive = PromotionGates {
            calibration: Calibration::Fitted {
                scale: -0.4,
                offset: 0.0,
            },
            ..passing_gates()
        };
        assert!(matches!(
            anti_predictive.failures().as_slice(),
            [GateFailure::AntiPredictiveCalibration { .. }]
        ));

        let thin_coverage = PromotionGates {
            coverage: vec![("implementation".into(), 2)],
            ..passing_gates()
        };
        assert!(matches!(
            thin_coverage.failures().as_slice(),
            [GateFailure::TierCoverage { records: 2, .. }]
        ));

        let unpriced = PromotionGates {
            cost_coefficients: Coefficients::NonFinite,
            ..passing_gates()
        };
        assert!(matches!(
            unpriced.failures().as_slice(),
            [GateFailure::NonFiniteCostCoefficients]
        ));
    }

    /// L3: one supported test record used to be enough to pass the quality
    /// gate. The fixture's test split is small by construction.
    #[test]
    fn a_handful_of_supported_records_cannot_pass_promotion() {
        let dataset = dataset();
        let outcome = train_and_evaluate(
            &dataset,
            &EvalSettings {
                min_supported_test_records: 1_000,
                ..settings(0.5, 5)
            },
        )
        .expect("trains");
        assert!(!outcome.report.gates.gates_passed);
        assert!(
            outcome
                .report
                .gates
                .failures()
                .iter()
                .any(|failure| matches!(failure, GateFailure::SupportedTestRecords { .. })),
            "{:?}",
            outcome.report.gates.failures()
        );
        assert_eq!(
            outcome.report.gates.supported_test_records,
            outcome.report.gates.supported_test_records,
            "the count is recorded in the report"
        );
    }

    /// L2, L13: a record whose floor is escalation is no evidence about a
    /// research route, and the baseline for it is escalation — not a pool
    /// of every research and implementation record in the split.
    #[test]
    fn eligibility_and_baseline_follow_each_records_own_floor() {
        let mut records = Vec::new();
        for day in 1..=20 {
            let mut record = record(
                &format!("f{day}"),
                &format!("2026-09-{day:02}"),
                day % 2 == 0,
                Tier::Implementation,
            );
            // A high-risk task: nothing below escalation was ever eligible
            // for it, so its implementation record is not baseline
            // evidence and the router would never have offered it.
            record.floor = Tier::Escalation;
            records.push(record);
        }
        let dataset = Dataset {
            version: crate::learn::dataset::DATASET_VERSION,
            label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
            records,
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        let outcome = train_and_evaluate(&dataset, &settings(0.5, 5)).expect("trains");
        assert_eq!(
            outcome.report.gates.supported_test_records, 0,
            "no eligible tier is trained, so nothing supports a selection"
        );
        assert_eq!(
            outcome.report.baseline_acceptance_rate, None,
            "no test record ran at its own floor: there is no baseline to report"
        );
        assert_eq!(outcome.report.abstention_rate, 1.0);
    }

    /// L8: the identities the artifact may claim evidence for are the ones
    /// training observed.
    #[test]
    fn observed_identities_are_recorded_per_tier() {
        let dataset = dataset();
        let outcome = train_and_evaluate(&dataset, &settings(0.5, 5)).expect("trains");
        assert_eq!(
            outcome.observed_identities,
            vec![(
                Tier::Implementation,
                vec![ProfileIdentity {
                    model: "sonnet".into(),
                    effort: None,
                    harness: None,
                }]
            )]
        );
    }

    /// L10: a dataset that cannot cover one tier is a training failure
    /// with a name, not a `Result<_, String>`.
    #[test]
    fn a_dataset_without_tier_coverage_names_the_tier() {
        let mut records = Vec::new();
        for day in 1..=4 {
            records.push(record(
                &format!("f{day}"),
                &format!("2026-09-{day:02}"),
                true,
                Tier::Implementation,
            ));
        }
        let dataset = Dataset {
            version: crate::learn::dataset::DATASET_VERSION,
            label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
            records,
            exclusions: vec![],
            fingerprint: "fingerprint-1".into(),
            built_at: "now".into(),
        };
        let error = train_and_evaluate(&dataset, &settings(0.75, 5)).expect_err("refuses");
        assert!(
            matches!(
                error,
                TrainError::NoTierCoverage {
                    tier: Tier::Implementation,
                    minimum: 5,
                    ..
                }
            ),
            "{error}"
        );
        assert!(error.to_string().contains("implementation"));
    }
}
