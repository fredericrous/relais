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

pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;

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
    IncompatibleSchema { found: u32, expected: u32 },
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
    pub fn promote(
        &self,
        artifact_id: &str,
        gates: &PromotionGates,
    ) -> std::result::Result<(), ArtifactError> {
        let artifact = self.load(artifact_id)?;
        let Some(evaluation) = &artifact.evaluation else {
            return Err(ArtifactError::Malformed(format!(
                "artifact {artifact_id} has no evaluation report; promotion requires evidence"
            )));
        };
        if evaluation.get("gates_passed") != Some(&serde_json::Value::Bool(true)) {
            return Err(ArtifactError::Malformed(format!(
                "artifact {artifact_id} failed its evaluation gates; promote rejects it"
            )));
        }
        let _ = gates;
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
    use crate::learn::features::{FeatureSchema, Standardization};
    use crate::policy::Tier;

    fn artifact(id: &str, evaluation: Option<serde_json::Value>) -> Artifact {
        Artifact {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            artifact_id: id.into(),
            feature_schema: FeatureSchema::standard(),
            standardization: Standardization {
                means: vec![0.0],
                stds: vec![1.0],
            },
            acceptance: LogisticModel {
                dim: 1,
                weights: vec![1.0],
                bias: 0.0,
            },
            cost: CostModel {
                dim: 1,
                weights: vec![0.5],
                bias: 0.0,
                cohort_means: vec![("implementation".into(), 120.0)],
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

    #[test]
    fn promotion_requires_evidence_and_preserves_rollback() {
        let (registry, dir) = temp_registry();
        let unevaluated = artifact("art-no-eval", None);
        registry.store(&unevaluated).expect("store");
        assert!(registry.promote("art-no-eval", &fake_gates()).is_err());

        let evaluated = artifact("art-eval", Some(serde_json::json!({"gates_passed": true})));
        registry.store(&evaluated).expect("store");
        registry
            .promote("art-eval", &fake_gates())
            .expect("promote");
        assert_eq!(
            registry.active().expect("active").unwrap().artifact_id,
            "art-eval"
        );

        let better = artifact(
            "art-better",
            Some(serde_json::json!({"gates_passed": true})),
        );
        registry.store(&better).expect("store");
        registry
            .promote("art-better", &fake_gates())
            .expect("promote");
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

    fn fake_gates() -> PromotionGates {
        PromotionGates {
            gates_passed: true,
            min_records_per_tier: 5,
            coverage: vec![("implementation".into(), 5)],
            test_acceptance_rate: Some(0.9),
            quality_floor: 0.75,
            abstention_rate: 0.1,
        }
    }
}
