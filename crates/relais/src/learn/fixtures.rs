//! Artifact and evaluation-report fixtures (SPEC §17) for the tests of the
//! registry, the learning cycle and the CLI. Nothing here runs on a
//! production path; like `test_support` it is `pub` rather than
//! `#[cfg(test)]` because an integration suite builds against this crate as
//! an ordinary dependency.

use super::evaluate::{Calibration, Coefficients, EvalReport, PromotionGates, EVAL_SCHEMA_VERSION};
use super::features::{feature_dim, FeatureSchema, RecipeIdentity, Standardization};
use super::learner::{CostModel, FitReport, LogisticModel, SolverSettings};
use super::registry::{Artifact, ARTIFACT_SCHEMA_VERSION};
use crate::policy::Tier;

pub fn artifact(id: &str, evaluation: Option<serde_json::Value>) -> Artifact {
    // Every array is the schema's dimension, as a real artifact's is.
    let dim = feature_dim(&FeatureSchema::standard());
    Artifact {
        schema_version: ARTIFACT_SCHEMA_VERSION,
        artifact_id: id.into(),
        feature_schema: FeatureSchema::standard(),
        standardization: Standardization {
            means: vec![0.0; dim],
            stds: vec![1.0; dim],
        },
        acceptance: LogisticModel {
            dim,
            weights: vec![1.0; dim],
            bias: 0.0,
        },
        cost: CostModel {
            dim,
            weights: vec![0.5; dim],
            bias: 0.0,
            cohort_means: vec![("change".into(), 120.0)],
            observed_log_min: 0.0,
            observed_log_max: 10.0,
        },
        tiers_supported: vec![Tier::Implementation],
        observed_identities: vec![(
            Tier::Implementation,
            vec![RecipeIdentity {
                model: "sonnet".into(),
                effort: None,
                harness: None,
                recipe_id: None,
            }],
        )],
        cohorts: vec!["change".into()],
        label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
        dataset_fingerprint: "abc".into(),
        solver: SolverSettings::default(),
        trained_at: "2026-09-18".into(),
        relais_version: crate::version().into(),
        evaluation,
    }
}

/// The evaluator's report, as `relais train` stores it — the ONLY
/// shape promotion may read. A hand-written `{"gates_passed": true}`
/// here once let promotion pass its test while refusing every real
/// artifact.
pub fn evaluation(artifact_id: &str, gates_passed: bool) -> serde_json::Value {
    let gates = PromotionGates {
        gates_passed,
        min_records_per_tier: 5,
        min_supported_test_records: 20,
        quality_floor: 0.75,
        max_abstention_rate: 0.5,
        coverage: vec![("implementation".into(), 5)],
        test_records: 40,
        supported_test_records: if gates_passed { 25 } else { 1 },
        test_acceptance_rate: Some(0.9),
        abstention_rate: 0.1,
        acceptance_fit: FitReport {
            iterations: 40,
            converged: true,
            final_loss: 0.1,
        },
        cost_fit: FitReport {
            iterations: 40,
            converged: true,
            final_loss: 0.1,
        },
        calibration: Calibration::Fitted {
            scale: 1.1,
            offset: 0.0,
        },
        cost_coefficients: Coefficients::Finite,
    };
    let report = EvalReport {
        version: EVAL_SCHEMA_VERSION,
        artifact_id: artifact_id.into(),
        dataset_fingerprint: "abc".into(),
        train_records: 10,
        calibration_records: 2,
        test_records: 40,
        calibration_bins: vec![],
        by_label_source: vec![],
        floor_sweep: vec![],
        examples_by_origin: Default::default(),
        counterfactual_gap: None,
        test_acceptance_rate: Some(0.9),
        baseline_acceptance_rate: Some(0.8),
        mean_cost_selected: None,
        mean_cost_baseline: None,
        abstention_rate: 0.1,
        coverage: vec![("implementation".into(), 5)],
        gates,
    };
    serde_json::to_value(report).expect("serializes")
}
