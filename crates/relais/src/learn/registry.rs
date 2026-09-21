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

/// Version 2 added the cost model's observed log-cost range, which
/// inference reads to tell an estimate from an extrapolation. Version 3
/// adds the PROFILE IDENTITIES training observed per tier: without them
/// inference keyed coverage on the tier alone, so swapping the model
/// behind a tier inherited the old model's acceptance evidence (SPEC §17
/// forbids it). Neither field can be defaulted into an older artifact —
/// an absent range prices every tier at zero, an absent identity set
/// claims evidence for every model — so older artifacts are refused by
/// version rather than silently reinterpreted. Retrain to get a version-3
/// artifact; the previous one stays promotable only by the relais that
/// wrote it.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub schema_version: u32,
    pub artifact_id: String,
    pub feature_schema: FeatureSchema,
    pub standardization: super::features::Standardization,
    pub acceptance: LogisticModel,
    pub cost: CostModel,
    pub tiers_supported: Vec<crate::policy::Tier>,
    /// The profile identities training observed per tier. Inference
    /// abstains for a tier whose CURRENT identity is not in this set:
    /// evidence belongs to the profile that produced it, and a model swap
    /// starts with none.
    pub observed_identities: Vec<(crate::policy::Tier, Vec<super::features::ProfileIdentity>)>,
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
    /// The artifact carries no evaluation report at all.
    NoEvidence {
        artifact: String,
    },
    /// The report is evidence about a different artifact, a different
    /// dataset, or was written against another evaluation schema.
    EvidenceMismatch {
        artifact: String,
        field: &'static str,
        in_report: String,
        expected: String,
    },
    /// The report's own numbers do not earn promotion.
    GatesFailed {
        artifact: String,
        failures: Vec<super::evaluate::GateFailure>,
    },
    /// The report claims a verdict its own numbers do not produce: the
    /// stored `gates_passed` was edited, or written by a version whose
    /// gates were weaker. Either way it is not evidence.
    VerdictNotReproducible {
        artifact: String,
        claimed: bool,
    },
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
            Self::NoEvidence { artifact } => write!(
                f,
                "artifact {artifact} has no evaluation report; promotion requires evidence"
            ),
            Self::EvidenceMismatch {
                artifact,
                field,
                in_report,
                expected,
            } => write!(
                f,
                "artifact {artifact} carries an evaluation report whose {field} is {in_report}, \
                 not {expected}: it is not evidence about this artifact"
            ),
            Self::GatesFailed { artifact, failures } => {
                write!(f, "artifact {artifact} failed its evaluation gates:")?;
                for failure in failures {
                    write!(f, "\n  - {failure}")?;
                }
                Ok(())
            }
            Self::VerdictNotReproducible { artifact, claimed } => write!(
                f,
                "artifact {artifact} records gates_passed={claimed}, which its own numbers do not \
                 produce; promote refuses a verdict it cannot recompute"
            ),
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

    fn previous_path(&self) -> PathBuf {
        self.dir.join("previous.json")
    }

    /// An artifact pointer. Atomic: written to a temp file and renamed, so
    /// a crash never leaves a torn pointer (SPEC §21). Both pointers go
    /// through it — `previous.json` used to be written in place, so a
    /// crash mid-write left rollback pointing at half a filename.
    fn write_pointer(
        &self,
        path: &Path,
        artifact_id: &str,
    ) -> std::result::Result<(), ArtifactError> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "pointer".to_string());
        let tmp = self.dir.join(format!(".{name}.tmp"));
        std::fs::write(
            &tmp,
            serde_json::json!({ "artifact_id": artifact_id }).to_string(),
        )
        .map_err(ArtifactError::Io)?;
        std::fs::rename(&tmp, path).map_err(ArtifactError::Io)?;
        Ok(())
    }

    /// The artifact id a pointer file names, or `None` when there is no
    /// such pointer. ONLY a missing file is `None`: a pointer that cannot
    /// be read because of permissions or a failing disk is an error, not
    /// "nothing is active" — and "nothing is active" is what made
    /// promotion overwrite the rollback pointer and inference report a
    /// broken artifact as an absent one.
    fn pointer(&self, path: &Path) -> std::result::Result<Option<String>, ArtifactError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ArtifactError::Io(e)),
        };
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| ArtifactError::Malformed(e.to_string()))?;
        let id = value
            .get("artifact_id")
            .and_then(|id| id.as_str())
            .ok_or_else(|| {
                ArtifactError::Malformed(format!(
                    "{} has no artifact_id",
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                ))
            })?;
        Ok(Some(id.to_string()))
    }

    /// The active artifact, or `None` when none was ever promoted.
    pub fn active(&self) -> std::result::Result<Option<Artifact>, ArtifactError> {
        match self.pointer(&self.active_path())? {
            Some(id) => self.load(&id).map(Some),
            None => Ok(None),
        }
    }

    /// An artifact whose stored evidence has been checked against the
    /// artifact itself and found to earn promotion. The ONLY way to build
    /// one is this function, so `promote` cannot be handed an id and a
    /// hopeful blob (SPEC §17: promotion is evidence-gated).
    ///
    /// Checked here: the report names THIS artifact and the dataset it was
    /// fitted on, it was written against an evaluation schema this relais
    /// reads, and its gate verdict is RECOMPUTED from the report's own
    /// numbers rather than read out of the serialized `gates_passed`.
    pub fn evaluated(
        &self,
        artifact_id: &str,
    ) -> std::result::Result<EvaluatedArtifact, ArtifactError> {
        let artifact = self.load(artifact_id)?;
        let Some(evaluation) = &artifact.evaluation else {
            return Err(ArtifactError::NoEvidence {
                artifact: artifact_id.to_string(),
            });
        };
        let report: super::evaluate::EvalReport = serde_json::from_value(evaluation.clone())
            .map_err(|e| {
                ArtifactError::Malformed(format!(
                    "artifact {artifact_id} carries an evaluation report this relais cannot read: {e}"
                ))
            })?;
        for (field, in_report, expected) in [
            (
                "version",
                report.version.to_string(),
                super::evaluate::EVAL_SCHEMA_VERSION.to_string(),
            ),
            (
                "artifact_id",
                report.artifact_id.clone(),
                artifact.artifact_id.clone(),
            ),
            (
                "dataset_fingerprint",
                report.dataset_fingerprint.clone(),
                artifact.dataset_fingerprint.clone(),
            ),
        ] {
            if in_report != expected {
                return Err(ArtifactError::EvidenceMismatch {
                    artifact: artifact_id.to_string(),
                    field,
                    in_report,
                    expected,
                });
            }
        }
        let failures = report.gates.failures();
        if !failures.is_empty() {
            return Err(ArtifactError::GatesFailed {
                artifact: artifact_id.to_string(),
                failures,
            });
        }
        if !report.gates.gates_passed {
            return Err(ArtifactError::VerdictNotReproducible {
                artifact: artifact_id.to_string(),
                claimed: report.gates.gates_passed,
            });
        }
        Ok(EvaluatedArtifact { artifact, report })
    }

    /// Activate an evaluated artifact; the previous pointer is kept for
    /// immediate rollback (SPEC §17). Taking an `EvaluatedArtifact` is
    /// what makes "the evidence was checked" a fact about the type rather
    /// than a step a caller might skip.
    pub fn promote(&self, evaluated: &EvaluatedArtifact) -> std::result::Result<(), ArtifactError> {
        // What is active now must be READ, not guessed: an unreadable
        // pointer used to be treated as "nothing is active", which left
        // `previous.json` naming the artifact before it — so one rollback
        // went two artifacts back.
        if let Some(current) = self.pointer(&self.active_path())? {
            self.write_pointer(&self.previous_path(), &current)?;
        }
        self.write_pointer(&self.active_path(), evaluated.artifact_id())
    }

    pub fn rollback(&self) -> std::result::Result<Option<String>, ArtifactError> {
        let Some(id) = self.pointer(&self.previous_path())? else {
            return Ok(None);
        };
        self.write_pointer(&self.active_path(), &id)?;
        Ok(Some(id))
    }
}

/// An artifact the registry has checked against its own evaluation report
/// and found promotable. Its fields are private: outside this module the
/// only way to hold one is `Registry::evaluated`.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluatedArtifact {
    artifact: Artifact,
    report: super::evaluate::EvalReport,
}

impl EvaluatedArtifact {
    pub fn artifact_id(&self) -> &str {
        &self.artifact.artifact_id
    }

    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    /// The evidence that earned it: the report `evaluated` verified.
    pub fn report(&self) -> &super::evaluate::EvalReport {
        &self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::evaluate::{
        Calibration, Coefficients, EvalReport, PromotionGates, EVAL_SCHEMA_VERSION,
    };
    use crate::learn::features::{feature_dim, FeatureSchema, ProfileIdentity, Standardization};
    use crate::learn::learner::FitReport;
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
            observed_identities: vec![(
                Tier::Implementation,
                vec![ProfileIdentity {
                    model: "sonnet".into(),
                    effort: None,
                    harness: None,
                }],
            )],
            cohorts: vec!["change".into()],
            dataset_fingerprint: "abc".into(),
            solver: SolverSettings::default(),
            trained_at: "2026-09-18".into(),
            relais_version: crate::version().into(),
            evaluation,
        }
    }

    /// A registry directory nobody else can collide with, pre-cleaned so
    /// a crashed earlier run cannot make this one pass: the process owns
    /// the pid, the counter orders the directories within it. A thread id
    /// is reused the moment a thread ends, so two tests in one run shared
    /// a registry — and one of them saw the other's active pointer.
    fn temp_registry(name: &str) -> (Registry, PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "relais-registry-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // Best effort: usually absent, and `Registry::open` reports any
        // directory it cannot create.
        std::fs::remove_dir_all(&dir).ok();
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
    fn evaluation(artifact_id: &str, gates_passed: bool) -> serde_json::Value {
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

    #[test]
    fn promotion_requires_evidence_and_preserves_rollback() {
        let (registry, dir) = temp_registry("promotion");
        let unevaluated = artifact("art-no-eval", None);
        registry.store(&unevaluated).expect("store");
        assert!(matches!(
            registry.evaluated("art-no-eval"),
            Err(ArtifactError::NoEvidence { .. })
        ));
        let failed = artifact("art-failed", Some(evaluation("art-failed", false)));
        registry.store(&failed).expect("store");
        assert!(
            matches!(
                registry.evaluated("art-failed"),
                Err(ArtifactError::GatesFailed { .. })
            ),
            "failed gates refuse"
        );
        let top_level_probe =
            artifact("art-probe", Some(serde_json::json!({"gates_passed": true})));
        registry.store(&top_level_probe).expect("store");
        assert!(
            registry.evaluated("art-probe").is_err(),
            "a document that is not the evaluator's report is not evidence"
        );

        let evaluated = artifact("art-eval", Some(evaluation("art-eval", true)));
        registry.store(&evaluated).expect("store");
        let checked = registry.evaluated("art-eval").expect("evidence holds");
        registry.promote(&checked).expect("promote");
        assert_eq!(
            registry.active().expect("active").unwrap().artifact_id,
            "art-eval"
        );

        let better = artifact("art-better", Some(evaluation("art-better", true)));
        registry.store(&better).expect("store");
        let checked = registry.evaluated("art-better").expect("evidence holds");
        registry.promote(&checked).expect("promote");
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

    /// L9: the report must be evidence about THIS artifact and THIS
    /// dataset, written against a schema this relais reads.
    #[test]
    fn evidence_about_another_artifact_is_not_evidence() {
        let (registry, dir) = temp_registry("evidence");
        let borrowed = artifact("art-a", Some(evaluation("art-b", true)));
        registry.store(&borrowed).expect("store");
        assert!(
            matches!(
                registry.evaluated("art-a"),
                Err(ArtifactError::EvidenceMismatch {
                    field: "artifact_id",
                    ..
                })
            ),
            "a report naming another artifact is not evidence about this one"
        );

        let mut other_dataset = artifact("art-c", Some(evaluation("art-c", true)));
        other_dataset.dataset_fingerprint = "a-different-dataset".into();
        registry.store(&other_dataset).expect("store");
        assert!(matches!(
            registry.evaluated("art-c"),
            Err(ArtifactError::EvidenceMismatch {
                field: "dataset_fingerprint",
                ..
            })
        ));

        let mut older_schema = evaluation("art-d", true);
        older_schema["version"] = serde_json::json!(EVAL_SCHEMA_VERSION - 1);
        let old = artifact("art-d", Some(older_schema));
        registry.store(&old).expect("store");
        assert!(matches!(
            registry.evaluated("art-d"),
            Err(ArtifactError::EvidenceMismatch {
                field: "version",
                ..
            })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// L9: the stored verdict is recomputed, never believed. A report
    /// whose `gates_passed` was flipped to true does not promote.
    #[test]
    fn a_hand_edited_verdict_does_not_promote() {
        let (registry, dir) = temp_registry("verdict");
        let mut tampered = evaluation("art-tampered", false);
        tampered["gates"]["gates_passed"] = serde_json::json!(true);
        registry
            .store(&artifact("art-tampered", Some(tampered)))
            .expect("store");
        assert!(
            matches!(
                registry.evaluated("art-tampered"),
                Err(ArtifactError::GatesFailed { .. })
            ),
            "the numbers decide, not the flag"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// L6, L12: an unreadable pointer is an error, not "nothing is
    /// active". Reading it as absence let promotion overwrite the
    /// rollback pointer, so one rollback went two artifacts back.
    #[test]
    fn an_unreadable_active_pointer_is_an_error_not_an_absence() {
        let (registry, dir) = temp_registry("pointer");
        assert!(
            registry
                .active()
                .expect("no pointer is not an error")
                .is_none(),
            "a registry that never promoted anything has no active artifact"
        );
        std::fs::write(dir.join("active.json"), "{ not json").expect("write");
        assert!(matches!(
            registry.active(),
            Err(ArtifactError::Malformed(_))
        ));
        std::fs::write(dir.join("active.json"), serde_json::json!({}).to_string()).expect("write");
        assert!(matches!(
            registry.active(),
            Err(ArtifactError::Malformed(_))
        ));
        assert!(
            registry.rollback().expect("no previous pointer").is_none(),
            "nothing to roll back to is not an error either"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
