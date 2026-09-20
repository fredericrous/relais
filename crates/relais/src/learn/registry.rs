//! The local artifact registry (SPEC §16, §17, §21).
//!
//! Artifacts live outside worker write authority, carry their dataset
//! fingerprint, feature schema, label policy, solver settings, supported
//! cohorts and evaluation report. Loading rejects incompatible schemas
//! and non-finite values. Activation is atomic with the previous artifact
//! preserved for rollback; promotion is evidence-gated. No artifact is
//! loaded during a running task — each run pins what it read.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::features::FeatureSchema;
use super::learner::{CostModel, LogisticModel, SolverSettings};

/// Version 2 adds the cost model's observed log-cost range, which
/// inference reads to tell an estimate from an extrapolation. A version-1
/// artifact carries no range; read with a defaulted (0, 0) one it would
/// price every tier at zero — the cheapest possible — so those artifacts
/// are refused by version rather than silently reinterpreted. Retrain to
/// get a version-2 artifact; the previous one stays promotable only by
/// the relais that wrote it.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub schema_version: u32,
    pub artifact_id: String,
    pub feature_schema: FeatureSchema,
    pub standardization: super::features::Standardization,
    pub acceptance: LogisticModel,
    pub cost: CostModel,
    pub tiers_supported: Vec<crate::policy::Tier>,
    pub cohorts: Vec<String>,
    pub dataset_fingerprint: String,
    pub solver: SolverSettings,
    pub trained_at: String,
    pub relais_version: String,
    /// Promotion requires evidence: the evaluation report of the run that
    /// produced this artifact (SPEC §17).
    pub evaluation: Option<serde_json::Value>,
}

#[derive(Debug)]
pub enum ArtifactError {
    Malformed(String),
    IncompatibleSchema {
        found: u32,
        expected: u32,
    },
    /// A coefficient array, or a standardization array, whose length is
    /// not the feature schema's dimension. Predicting anyway means
    /// silently reading zeros for every coordinate the artifact has no
    /// weight for.
    DimensionMismatch {
        field: &'static str,
        found: usize,
        expected: usize,
    },
    NonFinite,
    Io(std::io::Error),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "malformed artifact: {detail}"),
            Self::IncompatibleSchema { found, expected } => write!(
                f,
                "artifact schema {found} is not understood (this relais reads {expected})"
            ),
            Self::DimensionMismatch {
                field,
                found,
                expected,
            } => write!(
                f,
                "artifact {field} has {found} entries but its feature schema has {expected} \
                 dimension(s); it cannot predict over this feature space"
            ),
            Self::NonFinite => write!(f, "artifact contains non-finite values"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ArtifactError {}

impl Artifact {
    /// Loading validates schema compatibility and finiteness before any
    /// prediction is made from the artifact (SPEC §16, §21).
    pub fn validate(&self) -> Result<(), ArtifactError> {
        if self.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(ArtifactError::IncompatibleSchema {
                found: self.schema_version,
                expected: ARTIFACT_SCHEMA_VERSION,
            });
        }
        if self.feature_schema.version != super::features::FEATURE_SCHEMA_VERSION {
            return Err(ArtifactError::Malformed(
                "feature schema version mismatch inside artifact".into(),
            ));
        }
        // Shape, before finiteness: every array in the artifact must have
        // exactly the schema's dimension. `dot` reads a missing weight as
        // zero and `Standardization::apply` a missing scale as one, so an
        // eight-weight artifact would happily "predict" over a
        // three-hundred-dimension space — a confident number computed from
        // eight of the features and silence about the rest, with no
        // abstention anywhere (SPEC §21: malformed artifacts are
        // rejected).
        let expected = super::features::feature_dim(&self.feature_schema);
        for (field, found) in [
            ("acceptance.dim", self.acceptance.dim),
            ("acceptance.weights", self.acceptance.weights.len()),
            ("cost.dim", self.cost.dim),
            ("cost.weights", self.cost.weights.len()),
            ("standardization.means", self.standardization.means.len()),
            ("standardization.stds", self.standardization.stds.len()),
        ] {
            if found != expected {
                return Err(ArtifactError::DimensionMismatch {
                    field,
                    found,
                    expected,
                });
            }
        }
        let finite = |value: f64| value.is_finite();
        if !self.acceptance.weights.iter().copied().all(finite)
            || !self.cost.weights.iter().copied().all(finite)
            || !finite(self.acceptance.bias)
            || !finite(self.cost.bias)
            || !self.standardization.means.iter().copied().all(finite)
            || !self.standardization.stds.iter().copied().all(finite)
            || self.standardization.stds.iter().any(|std| *std <= 0.0)
        {
            return Err(ArtifactError::NonFinite);
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("artifact serializes")
    }

    pub fn from_json(text: &str) -> Result<Self, ArtifactError> {
        let artifact: Artifact =
            serde_json::from_str(text).map_err(|e| ArtifactError::Malformed(e.to_string()))?;
        artifact.validate()?;
        Ok(artifact)
    }
}

pub struct Registry {
    dir: PathBuf,
    artifacts_dir: PathBuf,
}

impl Registry {
    pub fn open(dir: &Path) -> std::result::Result<Self, ArtifactError> {
        let artifacts_dir = dir.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).map_err(ArtifactError::Io)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            artifacts_dir,
        })
    }

    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts_dir
    }

    pub fn store(&self, artifact: &Artifact) -> std::result::Result<(), ArtifactError> {
        let dir = self.artifacts_dir();
        std::fs::create_dir_all(dir).map_err(ArtifactError::Io)?;
        let path = dir.join(format!("{}.json", artifact.artifact_id));
        let tmp = dir.join(format!(".{}.tmp", artifact.artifact_id));
        std::fs::write(&tmp, artifact.to_json()).map_err(ArtifactError::Io)?;
        std::fs::rename(&tmp, &path).map_err(ArtifactError::Io)?;
        Ok(())
    }

    pub fn load(&self, artifact_id: &str) -> std::result::Result<Artifact, ArtifactError> {
        let path = self.artifacts_dir().join(format!("{artifact_id}.json"));
        let text = std::fs::read_to_string(path).map_err(ArtifactError::Io)?;
        Artifact::from_json(&text)
    }

    pub fn list(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Ok(entries) = std::fs::read_dir(self.artifacts_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(id) = name.strip_suffix(".json") {
                    if !id.starts_with('.') {
                        ids.push(id.to_string());
                    }
                }
            }
        }
        ids.sort();
        ids
    }

    fn active_path(&self) -> PathBuf {
        self.dir.join("active.json")
    }

    /// The active artifact pointer. Atomic: written to a temp file and
    /// renamed, so a crash never leaves a torn pointer (SPEC §21).
    fn write_active(&self, artifact_id: &str) -> std::result::Result<(), ArtifactError> {
        let tmp = self.dir.join(".active.tmp");
        std::fs::write(
            &tmp,
            serde_json::json!({ "artifact_id": artifact_id }).to_string(),
        )
        .map_err(ArtifactError::Io)?;
        std::fs::rename(&tmp, self.active_path()).map_err(ArtifactError::Io)?;
        Ok(())
    }

    pub fn active(&self) -> std::result::Result<Option<Artifact>, ArtifactError> {
        let text = match std::fs::read_to_string(self.active_path()) {
            Ok(text) => text,
            Err(_) => return Ok(None),
        };
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| ArtifactError::Malformed(e.to_string()))?;
        let id = value
            .get("artifact_id")
            .and_then(|id| id.as_str())
            .ok_or_else(|| ArtifactError::Malformed("active.json has no artifact_id".into()))?;
        self.load(id).map(Some)
    }

    /// Promotion requires the artifact to carry an evaluation whose gates
    /// passed; the previous pointer is kept for immediate rollback
    /// (SPEC §17).
    /// Activate an evaluated artifact. The evidence is the evaluator's own
    /// report, deserialized as the type the evaluator wrote — a `Value`
    /// probe for a top-level `gates_passed` used to look one level too
    /// high and refused every artifact `relais train` ever produced.
    pub fn promote(&self, artifact_id: &str) -> std::result::Result<(), ArtifactError> {
        let artifact = self.load(artifact_id)?;
        let Some(evaluation) = &artifact.evaluation else {
            return Err(ArtifactError::Malformed(format!(
                "artifact {artifact_id} has no evaluation report; promotion requires evidence"
            )));
        };
        let report: super::evaluate::EvalReport = serde_json::from_value(evaluation.clone())
            .map_err(|e| {
                ArtifactError::Malformed(format!(
                    "artifact {artifact_id} carries an evaluation report this relais cannot read: {e}"
                ))
            })?;
        if !report.gates.gates_passed {
            return Err(ArtifactError::Malformed(format!(
                "artifact {artifact_id} failed its evaluation gates; promote rejects it"
            )));
        }
        if let Some(current) = self.active().ok().flatten() {
            let rollback = self.dir.join("previous.json");
            std::fs::write(
                &rollback,
                serde_json::json!({ "artifact_id": current.artifact_id }).to_string(),
            )
            .map_err(ArtifactError::Io)?;
        }
        self.write_active(artifact_id)
    }

    pub fn rollback(&self) -> std::result::Result<Option<String>, ArtifactError> {
        let previous = self.dir.join("previous.json");
        let text = match std::fs::read_to_string(&previous) {
            Ok(text) => text,
            Err(_) => return Ok(None),
        };
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| ArtifactError::Malformed(e.to_string()))?;
        let id = value
            .get("artifact_id")
            .and_then(|id| id.as_str())
            .map(String::from);
        if let Some(id) = &id {
            self.write_active(id)?;
        }
        Ok(id)
    }
}

/// Evidence gates for promotion (SPEC §17): coverage per supported tier,
/// quality floor on acceptance, no silent relabelling. The evaluator
/// computes them; the registry enforces their presence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionGates {
    pub gates_passed: bool,
    pub min_records_per_tier: usize,
    pub coverage: Vec<(String, usize)>,
    pub test_acceptance_rate: Option<f64>,
    pub quality_floor: f64,
    pub abstention_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::features::{feature_dim, FeatureSchema, Standardization};
    use crate::policy::Tier;

    fn artifact(id: &str, evaluation: Option<serde_json::Value>) -> Artifact {
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
            cohorts: vec!["change".into()],
            dataset_fingerprint: "abc".into(),
            solver: SolverSettings::default(),
            trained_at: "2026-09-18".into(),
            relais_version: crate::version().into(),
            evaluation,
        }
    }

    fn temp_registry() -> (Registry, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "relais-registry-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let registry = Registry::open(&dir).expect("registry");
        (registry, dir)
    }

    #[test]
    fn round_trip_and_validation() {
        let artifact = artifact("art-1", None);
        let parsed = Artifact::from_json(&artifact.to_json()).expect("round trip");
        assert_eq!(parsed, artifact);
    }

    #[test]
    fn incompatible_schemas_and_non_finite_values_are_rejected() {
        let mut broken_schema = artifact("art-2", None);
        broken_schema.schema_version = 99;
        assert!(matches!(
            Artifact::from_json(&broken_schema.to_json()),
            Err(ArtifactError::IncompatibleSchema { .. })
        ));
        let mut nan_weights = artifact("art-3", None);
        nan_weights.acceptance.weights[0] = f64::NAN;
        // NaN never survives JSON (it would parse as null, a Malformed
        // rejection); validate() is where non-finite coefficients are
        // caught in memory, and both paths reject the artifact.
        assert!(matches!(
            nan_weights.validate(),
            Err(ArtifactError::NonFinite)
        ));
        let text = serde_json::to_string(&nan_weights).expect("serializes");
        assert!(
            Artifact::from_json(&text).is_err(),
            "NaN artifacts cannot round-trip"
        );
        let mut bad_std = artifact("art-4", None);
        bad_std.standardization.stds[0] = 0.0;
        assert!(Artifact::from_json(&serde_json::to_string(&bad_std).unwrap()).is_err());
    }

    /// D1: an artifact whose arrays are shorter than the feature space it
    /// claims to predict over was accepted, and predicted with confidence
    /// from the handful of coordinates it had weights for.
    #[test]
    fn arrays_shorter_than_the_feature_schema_are_rejected() {
        let dim = feature_dim(&FeatureSchema::standard());
        assert!(dim > 8, "the schema is wider than the truncated fixture");

        let eight_weights = |mut artifact: Artifact| -> Artifact {
            artifact.acceptance.dim = 8;
            artifact.acceptance.weights = vec![0.1; 8];
            artifact
        };
        let short_acceptance = eight_weights(artifact("art-dim-1", None));
        assert!(
            matches!(
                short_acceptance.validate(),
                Err(ArtifactError::DimensionMismatch {
                    field: "acceptance.dim",
                    found: 8,
                    ..
                })
            ),
            "an 8-weight acceptance model may not predict over {dim} dimensions"
        );
        assert!(Artifact::from_json(&short_acceptance.to_json()).is_err());

        // A `dim` that agrees with the schema while the weights do not is
        // the same lie one field further in.
        let mut lying_dim = artifact("art-dim-2", None);
        lying_dim.acceptance.weights.truncate(8);
        assert!(matches!(
            lying_dim.validate(),
            Err(ArtifactError::DimensionMismatch {
                field: "acceptance.weights",
                found: 8,
                ..
            })
        ));

        let mut short_cost = artifact("art-dim-3", None);
        short_cost.cost.dim = 8;
        short_cost.cost.weights = vec![0.01; 8];
        assert!(matches!(
            short_cost.validate(),
            Err(ArtifactError::DimensionMismatch {
                field: "cost.dim",
                ..
            })
        ));

        let mut short_stds = artifact("art-dim-4", None);
        short_stds.standardization.stds.truncate(dim - 1);
        assert!(matches!(
            short_stds.validate(),
            Err(ArtifactError::DimensionMismatch {
                field: "standardization.stds",
                ..
            })
        ));
        let mut short_means = artifact("art-dim-5", None);
        short_means.standardization.means.pop();
        assert!(matches!(
            short_means.validate(),
            Err(ArtifactError::DimensionMismatch {
                field: "standardization.means",
                ..
            })
        ));

        // The well-shaped fixture still loads.
        assert!(artifact("art-dim-ok", None).validate().is_ok());
    }

    /// The evaluator's report, as `relais train` stores it — the ONLY
    /// shape promotion may read. A hand-written `{"gates_passed": true}`
    /// here once let promotion pass its test while refusing every real
    /// artifact.
    fn evaluation(gates_passed: bool) -> serde_json::Value {
        let report = super::super::evaluate::EvalReport {
            version: super::super::evaluate::EVAL_SCHEMA_VERSION,
            train_records: 10,
            calibration_records: 2,
            test_records: 2,
            calibration_bins: vec![],
            test_acceptance_rate: Some(0.9),
            baseline_acceptance_rate: Some(0.8),
            mean_cost_selected: None,
            mean_cost_baseline: None,
            abstention_rate: 0.1,
            coverage: vec![("implementation".into(), 5)],
            gates: PromotionGates {
                gates_passed,
                min_records_per_tier: 5,
                coverage: vec![("implementation".into(), 5)],
                test_acceptance_rate: Some(0.9),
                quality_floor: 0.75,
                abstention_rate: 0.1,
            },
        };
        serde_json::to_value(report).expect("serializes")
    }

    #[test]
    fn promotion_requires_evidence_and_preserves_rollback() {
        let (registry, dir) = temp_registry();
        let unevaluated = artifact("art-no-eval", None);
        registry.store(&unevaluated).expect("store");
        assert!(registry.promote("art-no-eval").is_err());
        let failed = artifact("art-failed", Some(evaluation(false)));
        registry.store(&failed).expect("store");
        assert!(
            registry.promote("art-failed").is_err(),
            "failed gates refuse"
        );
        let top_level_probe =
            artifact("art-probe", Some(serde_json::json!({"gates_passed": true})));
        registry.store(&top_level_probe).expect("store");
        assert!(
            registry.promote("art-probe").is_err(),
            "a document that is not the evaluator's report is not evidence"
        );

        let evaluated = artifact("art-eval", Some(evaluation(true)));
        registry.store(&evaluated).expect("store");
        registry.promote("art-eval").expect("promote");
        assert_eq!(
            registry.active().expect("active").unwrap().artifact_id,
            "art-eval"
        );

        let better = artifact("art-better", Some(evaluation(true)));
        registry.store(&better).expect("store");
        registry.promote("art-better").expect("promote");
        assert_eq!(
            registry.active().expect("active").unwrap().artifact_id,
            "art-better"
        );
        assert_eq!(
            registry.rollback().expect("rollback"),
            Some("art-eval".to_string()),
            "the previous artifact survives for immediate rollback"
        );
        assert_eq!(
            registry.active().expect("active").unwrap().artifact_id,
            "art-eval"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
