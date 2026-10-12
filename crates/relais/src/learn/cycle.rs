//! The learning cycle (SPEC §17, relais ADR-0003): dataset build → train →
//! evaluate → promote when allowed, composed in-process from the functions
//! `relais dataset build`, `train`, `evaluate` and `promote` already use,
//! with the rollback check in front and the retention rules behind.
//!
//! The decisions are pure functions of values handed in ([`due`],
//! [`rollback_reason`], [`withheld`]); [`run_cycle`] and [`roll_back_active`]
//! are the shell that reads the ledger and the registry and writes the
//! `learning_cycles` row. The Training lease, the idle check and the exit
//! codes belong to the caller: a cycle only starts once it holds the lease.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::ids::RunId;
use crate::ledger::{
    LearningAction, LearningCycleFinish, LearningSource, LearningVerdict, Ledger, LedgerError,
};
use crate::outcome::OutcomeKind;
use crate::policy::RepoPolicy;
use crate::router::stats::clopper_pearson_upper;

use super::dataset::{self, Dataset, DatasetError, Labelling};
use super::evaluate::{self, EvalReport, EvalSettings, TrainError};
use super::registry::{Artifact, ArtifactError, Registry};
use super::revert;
use super::salvage::{self, BackfillReport};

/// A cycle that started less than this long ago makes the next one "not due".
pub const DUE_AFTER_HOURS: i64 = 20;
/// Runs a learned artifact must have routed before the acceptance bound
/// may roll it back.
pub const MIN_ROUTED_RUNS: u64 = 10;
/// Terminal runs that must be recorded after a rollback before anything is
/// promoted again.
pub const MIN_RUNS_AFTER_ROLLBACK: usize = 10;
/// Datasets kept on disk, newest first.
pub const KEEP_DATASETS: usize = 5;
/// Artifacts kept beside the ones the registry points at, newest first.
pub const KEEP_ARTIFACTS: usize = 5;
/// The one-sided 95% bound is the two-sided 90% interval's upper end.
const ONE_SIDED_95: f64 = 0.10;

// ---------------------------------------------------------------- log ----

/// The log is rotated when it reaches this size; one old file is kept.
pub const LOG_ROTATE_BYTES: u64 = 1024 * 1024;

/// `--log <path>`: relais opens the file itself, in append mode, created
/// 0600, so a detached spawn needs no shell redirection and the log cannot
/// be world readable.
pub struct CycleLog {
    file: std::fs::File,
}

impl CycleLog {
    /// Open `path` for appending, rotating it first when it has reached
    /// [`LOG_ROTATE_BYTES`]: the old file becomes `<path>.1`, replacing the
    /// one before it.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::metadata(path).is_ok_and(|meta| meta.len() >= LOG_ROTATE_BYTES) {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            std::fs::rename(path, PathBuf::from(old))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self {
            file: options.open(path)?,
        })
    }

    /// One line, written with a single `write_all`. A log that cannot be
    /// written to loses the line; it never fails the cycle.
    pub fn line(&mut self, text: &str) {
        use std::io::Write;
        let _ = self.file.write_all(format!("{text}\n").as_bytes());
    }
}

// ---------------------------------------------------------------- due ----

/// Whether `--if-due` has anything to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    Yes,
    No {
        /// One sentence naming why.
        reason: String,
        /// When the cycle is next due, in words.
        next_due: String,
    },
}

impl Due {
    /// The single line `--if-due` prints when there is nothing to do.
    pub fn line(&self) -> Option<String> {
        match self {
            Self::Yes => None,
            Self::No { reason, next_due } => Some(format!("not due: {reason}; {next_due}")),
        }
    }
}

/// Due when no cycle ever ran, or the last one is at least
/// [`DUE_AFTER_HOURS`] old AND a run reached a terminal state since it.
pub fn due(last_started_at: Option<&str>, terminal_runs_since: usize, now: DateTime<Utc>) -> Due {
    let Some(last) = last_started_at else {
        return Due::Yes;
    };
    let Ok(started) = DateTime::parse_from_rfc3339(last) else {
        // A start time nobody can read is no evidence of a recent cycle.
        return Due::Yes;
    };
    let next = started.with_timezone(&Utc) + Duration::hours(DUE_AFTER_HOURS);
    if now < next {
        return Due::No {
            reason: format!(
                "the last cycle started {} ago",
                age_in_words(now - started.with_timezone(&Utc))
            ),
            next_due: format!(
                "next due {} if a run has finished by then",
                next.format("%Y-%m-%dT%H:%MZ")
            ),
        };
    }
    if terminal_runs_since == 0 {
        return Due::No {
            reason: "no run reached a terminal state since the last cycle".into(),
            next_due: "due at the first run that does".into(),
        };
    }
    Due::Yes
}

fn age_in_words(age: Duration) -> String {
    let minutes = age.num_minutes().max(0);
    if minutes < 90 {
        format!("{minutes} min")
    } else {
        format!("{} h", age.num_hours())
    }
}

// ----------------------------------------------------------- rollback ----

/// How one run an artifact routed turned out, under the labelling policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutedOutcome {
    /// Its change was reverted or a regression was confirmed.
    Regressed,
    /// Accepted without escalation: the label the acceptance model predicts.
    Accepted,
    /// Failed, or accepted only after escalation.
    NotAccepted,
    /// No evidence-backed label (in flight, blocked, never shipped, ...).
    Unlabelled,
}

/// Why the active artifact is to be rolled back.
#[derive(Debug, Clone, PartialEq)]
pub enum RollbackReason {
    /// Any routed run was reverted or had a regression confirmed.
    Regressed { runs: usize },
    /// With at least [`MIN_ROUTED_RUNS`] labelled runs, even the upper end
    /// of the one-sided 95% interval of their acceptance is under the
    /// quality floor the artifact's evaluation used.
    AcceptanceBelowFloor {
        accepted: u64,
        runs: u64,
        upper: f64,
        floor: f64,
    },
}

impl std::fmt::Display for RollbackReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Regressed { runs } => write!(
                f,
                "{runs} run(s) it routed were reverted or had a regression confirmed"
            ),
            Self::AcceptanceBelowFloor {
                accepted,
                runs,
                upper,
                floor,
            } => write!(
                f,
                "it routed {runs} run(s) and {accepted} were accepted; even the 95% upper bound \
                 {upper:.2} is below the quality floor {floor:.2} its evaluation used"
            ),
        }
    }
}

/// The rollback decision for an artifact, from how its routed runs ended.
pub fn rollback_reason(outcomes: &[RoutedOutcome], floor: f64) -> Option<RollbackReason> {
    let regressed = outcomes
        .iter()
        .filter(|outcome| **outcome == RoutedOutcome::Regressed)
        .count();
    if regressed > 0 {
        return Some(RollbackReason::Regressed { runs: regressed });
    }
    let accepted = outcomes
        .iter()
        .filter(|outcome| **outcome == RoutedOutcome::Accepted)
        .count() as u64;
    let runs = accepted
        + outcomes
            .iter()
            .filter(|outcome| **outcome == RoutedOutcome::NotAccepted)
            .count() as u64;
    if runs < MIN_ROUTED_RUNS {
        return None;
    }
    let upper = clopper_pearson_upper(accepted, runs, ONE_SIDED_95);
    (upper < floor).then_some(RollbackReason::AcceptanceBelowFloor {
        accepted,
        runs,
        upper,
        floor,
    })
}

// ---------------------------------------------------------- anti-flap ----

/// What a rolled-back artifact was trained on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolledBackData {
    pub label_policy_version: Option<u32>,
    pub dataset_fingerprint: Option<String>,
}

/// Why a gate-passing artifact is not promoted this cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Withheld {
    /// Its dataset and label policy match an artifact that was rolled back.
    SameDataAsRolledBack,
    /// Its dataset and label policy match the artifact already active: there
    /// is nothing new to promote.
    SameDataAsActive,
    /// Fewer than [`MIN_RUNS_AFTER_ROLLBACK`] terminal runs since the last
    /// rollback.
    TooFewRunsSinceRollback { have: usize, need: usize },
}

impl std::fmt::Display for Withheld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SameDataAsRolledBack => write!(
                f,
                "withheld: it was trained on the same data and label policy as an artifact that \
                 was rolled back"
            ),
            Self::SameDataAsActive => write!(
                f,
                "withheld: the active artifact was trained on the same data and label policy"
            ),
            Self::TooFewRunsSinceRollback { have, need } => write!(
                f,
                "withheld: {have} of the {need} terminal runs needed since the last rollback"
            ),
        }
    }
}

/// The anti-flap rule. `runs_since_rollback` is `None` when nothing was
/// ever rolled back.
pub fn withheld(
    fingerprint: &str,
    label_policy_version: u32,
    rolled_back: &[RolledBackData],
    active: Option<(&str, u32)>,
    runs_since_rollback: Option<usize>,
) -> Option<Withheld> {
    let same = |data: &RolledBackData| {
        data.dataset_fingerprint.as_deref() == Some(fingerprint)
            && data.label_policy_version == Some(label_policy_version)
    };
    if rolled_back.iter().any(same) {
        return Some(Withheld::SameDataAsRolledBack);
    }
    if active.is_some_and(|(active_fingerprint, policy)| {
        active_fingerprint == fingerprint && policy == label_policy_version
    }) {
        return Some(Withheld::SameDataAsActive);
    }
    match runs_since_rollback {
        Some(have) if have < MIN_RUNS_AFTER_ROLLBACK => Some(Withheld::TooFewRunsSinceRollback {
            have,
            need: MIN_RUNS_AFTER_ROLLBACK,
        }),
        _ => None,
    }
}

// -------------------------------------------------------------- shell ----

/// Where a cycle is, for the progress line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Build,
    Train,
    Evaluate,
    Promote,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Train => "train",
            Self::Evaluate => "evaluate",
            Self::Promote => "promote",
        }
    }
}

/// Why a stage of the cycle stopped.
#[derive(Debug)]
pub enum CycleError {
    Ledger(LedgerError),
    Dataset(DatasetError),
    Artifact(ArtifactError),
    Io {
        what: &'static str,
        path: PathBuf,
        cause: std::io::Error,
    },
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(e) => write!(f, "{e}"),
            Self::Dataset(e) => write!(f, "{e}"),
            Self::Artifact(e) => write!(f, "{e}"),
            Self::Io { what, path, cause } => write!(f, "{what} {}: {cause}", path.display()),
        }
    }
}

impl std::error::Error for CycleError {}

impl From<LedgerError> for CycleError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}
impl From<DatasetError> for CycleError {
    fn from(e: DatasetError) -> Self {
        Self::Dataset(e)
    }
}
impl From<ArtifactError> for CycleError {
    fn from(e: ArtifactError) -> Self {
        Self::Artifact(e)
    }
}

/// The promotion gates' thresholds, machine settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateSettings {
    pub quality_floor: f64,
    pub min_supported_test_records: usize,
    pub max_abstention_rate: f64,
}

/// Everything a cycle reads, handed in.
pub struct CycleInputs<'a> {
    pub ledger: &'a Ledger,
    pub registry: &'a Registry,
    pub repo: &'a RepoPolicy,
    pub datasets_dir: &'a Path,
    pub gates: GateSettings,
    /// The `[learning]` envelope is recorded: promote on passing gates.
    pub auto_promote: bool,
    /// The classifier input day files (ADR-0002): the cycle deletes those
    /// past their 90 days, as every write of one does.
    pub classifier_dir: Option<PathBuf>,
    /// Where amont-agent's reviewer outcomes are read at the start of the
    /// cycle (or why they could not be located); `None` skips the ingest.
    pub review_outcomes: Option<Result<crate::review_outcome::Settings, String>>,
    /// `[learning] revert_detection_off` is not set.
    pub detect_reverts: bool,
    pub now: DateTime<Utc>,
}

/// What a cycle did, for the person and for `--json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CycleReport {
    pub cycle_id: i64,
    pub action: LearningAction,
    pub artifact_id: Option<String>,
    pub dataset_fingerprint: Option<String>,
    pub label_policy_version: Option<u32>,
    pub examples_by_class: BTreeMap<String, u64>,
    pub examples_by_source: BTreeMap<String, u64>,
    pub cohorts: Vec<String>,
    pub verdict: Option<LearningVerdict>,
    /// Cycles left unfinished by a dead process and closed now.
    pub interrupted_closed: usize,
    pub note: Option<String>,
    pub error: Option<String>,
    /// The line telling the person how to promote, when the cycle stopped
    /// at `awaiting_promotion`.
    pub promote_hint: Option<String>,
}

/// The dataset, built the way `relais dataset build` builds it.
pub fn build_dataset(
    ledger: &Ledger,
    repo: &RepoPolicy,
    detect_reverts: bool,
    now: DateTime<Utc>,
) -> Result<(Dataset, BackfillReport, revert::RevertReport), CycleError> {
    let backfill = salvage::backfill(ledger)?;
    // Before the dataset is read: a detected revert flips its task's label.
    let reverts = if detect_reverts {
        revert::detect(ledger, now)?
    } else {
        revert::RevertReport::off()
    };
    let contract_of = |run_id: &str| -> Result<Option<dataset::ContractMaterial>, LedgerError> {
        let run = RunId::from_stored(run_id);
        let Some((contract, tier)) = ledger.run_contract_and_tier(&run)? else {
            return Ok(None);
        };
        let objective = contract.objective.clone();
        Ok(Some((contract, objective, tier.as_str().to_string())))
    };
    Ok((
        dataset::build(ledger, &contract_of, repo)?,
        backfill,
        reverts,
    ))
}

/// Write `dataset` to `dir/<timestamp>.json` through a temporary file and a
/// rename: a process killed halfway leaves no partial dataset a later
/// `train` could read.
pub fn write_dataset(
    dir: &Path,
    dataset: &Dataset,
    now: DateTime<Utc>,
) -> Result<PathBuf, CycleError> {
    let io = |what, path: &Path| {
        let path = path.to_path_buf();
        move |cause| CycleError::Io { what, path, cause }
    };
    std::fs::create_dir_all(dir).map_err(io("cannot create", dir))?;
    let path = dir.join(format!("{}.json", now.format("%Y%m%dT%H%M%S")));
    let text = serde_json::to_string_pretty(dataset).expect("a dataset is plain owned data");
    crate::fsutil::write_atomic(&path, &text).map_err(io("cannot write", &path))?;
    Ok(path)
}

/// Fit the artifact on `dataset` and evaluate it, as `relais train` does.
pub fn train_artifact(
    dataset: &Dataset,
    artifact_id: &str,
    gates: GateSettings,
) -> Result<(Artifact, EvalReport), TrainError> {
    let settings = EvalSettings {
        artifact_id: artifact_id.to_string(),
        solver: super::learner::SolverSettings::default(),
        quality_floor: gates.quality_floor,
        min_records_per_tier: evaluate::DEFAULT_MIN_RECORDS_PER_TIER,
        min_supported_test_records: gates.min_supported_test_records,
        max_abstention_rate: gates.max_abstention_rate,
    };
    let outcome = evaluate::train_and_evaluate(dataset, &settings)?;
    let artifact = Artifact {
        schema_version: super::registry::ARTIFACT_SCHEMA_VERSION,
        artifact_id: artifact_id.to_string(),
        feature_schema: super::features::FeatureSchema::standard(),
        standardization: outcome.standardization,
        acceptance: outcome.acceptance,
        cost: outcome.cost,
        tiers_supported: outcome.tiers_supported,
        observed_identities: outcome.observed_identities,
        cohorts: vec!["change".into(), "inspect".into()],
        label_policy_version: dataset.label_policy_version,
        dataset_fingerprint: dataset.fingerprint.clone(),
        solver: settings.solver,
        trained_at: crate::ledger::now_rfc3339(),
        relais_version: crate::version().into(),
        evaluation: Some(
            serde_json::to_value(&outcome.report)
                .expect("an EvalReport serializes: owned strings and finite f64"),
        ),
    };
    Ok((artifact, outcome.report))
}

/// The id a cycle gives its candidate artifact.
pub fn artifact_id_at(now: DateTime<Utc>) -> String {
    format!("artifact-{}", now.format("%Y%m%dT%H%M%S"))
}

fn class_counts(dataset: &Dataset) -> BTreeMap<String, u64> {
    let labels = dataset.acceptance_labels();
    BTreeMap::from([
        (
            "accepted_without_escalation".to_string(),
            labels.accepted_without_escalation as u64,
        ),
        ("other".to_string(), labels.other as u64),
    ])
}

fn source_counts(dataset: &Dataset) -> BTreeMap<String, u64> {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for record in &dataset.records {
        *counts
            .entry(record.label_source.as_str().to_string())
            .or_default() += 1;
    }
    counts
}

fn cohorts_covered(dataset: &Dataset) -> Vec<String> {
    let mut cohorts: Vec<String> = dataset
        .records
        .iter()
        .map(|record| record.task.cohort().to_string())
        .collect();
    cohorts.sort();
    cohorts.dedup();
    cohorts
}

/// How a run the active artifact routed ended, under the labelling policy.
pub fn routed_outcome(ledger: &Ledger, run: &RunId) -> Result<RoutedOutcome, LedgerError> {
    let Some(state) = ledger.run_status(run)? else {
        return Ok(RoutedOutcome::Unlabelled);
    };
    let Some(task) = ledger.task_of_run(run)? else {
        return Ok(RoutedOutcome::Unlabelled);
    };
    let outcome = ledger
        .latest_outcome(&task)?
        .map(|stored| stored.outcome.kind);
    if matches!(
        outcome,
        Some(OutcomeKind::Reverted | OutcomeKind::ConfirmedRegression)
    ) {
        return Ok(RoutedOutcome::Regressed);
    }
    let decision = ledger.decision_of_run(run)?;
    let salvage = salvage::salvage_evidence_of(decision.as_ref());
    Ok(match dataset::labelling_of(state, outcome, salvage) {
        Labelling::Accepted | Labelling::AcceptedViaSalvage => {
            let approved = decision.and_then(|record| record.resolution)
                == Some(crate::lifecycle::Reason::DecisionApproved);
            if ledger.escalation_attempted(run)? || approved {
                RoutedOutcome::NotAccepted
            } else {
                RoutedOutcome::Accepted
            }
        }
        Labelling::Failed => RoutedOutcome::NotAccepted,
        Labelling::Excluded(_) => RoutedOutcome::Unlabelled,
    })
}

/// Why an artifact's own quality floor could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FloorFallback {
    /// The artifact carries no evaluation report.
    Missing,
    /// It carries one that does not parse; the parse error.
    Unparseable(String),
}

/// The quality floor an artifact's own evaluation used.
fn evaluation_floor(artifact: &Artifact) -> Result<f64, FloorFallback> {
    let value = artifact.evaluation.clone().ok_or(FloorFallback::Missing)?;
    let report: EvalReport =
        serde_json::from_value(value).map_err(|e| FloorFallback::Unparseable(e.to_string()))?;
    Ok(report.gates.quality_floor)
}

/// The trace line for a floor that fell back to the default.
fn floor_note(artifact_id: &str, why: &FloorFallback) -> String {
    let default = super::evaluate::DEFAULT_QUALITY_FLOOR;
    match why {
        FloorFallback::Missing => format!(
            "active artifact {artifact_id} has no evaluation report; the default quality floor {default} was used"
        ),
        FloorFallback::Unparseable(e) => format!(
            "evaluation report of active artifact {artifact_id} is unreadable ({e}); the default quality floor {default} was used"
        ),
    }
}

/// Append `note` to the report's note.
fn add_note(report: &mut CycleReport, note: String) {
    report.note = Some(match report.note.take() {
        Some(existing) => format!("{existing}; {note}"),
        None => note,
    });
}

/// What rolling back did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RolledBack {
    /// The artifact that was active and no longer is.
    pub from: String,
    /// The artifact that is active now.
    pub to: String,
    pub reason: String,
    /// What the artifact rolled back from was trained on, for the row that
    /// records it (the anti-flap rule compares against it).
    pub label_policy_version: u32,
    pub dataset_fingerprint: String,
}

/// Why nothing was rolled back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NothingToRollBack {
    NoActiveArtifact,
    /// Nothing was promoted over a predecessor, or the predecessor is
    /// already what is active.
    NoPreviousArtifact,
}

impl std::fmt::Display for NothingToRollBack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoActiveArtifact => write!(f, "no learned artifact is active"),
            Self::NoPreviousArtifact => write!(
                f,
                "the active artifact has no previous artifact to return to"
            ),
        }
    }
}

/// Roll the active task artifact back through `Registry::rollback` and
/// record it as a `learning_cycles` row. `open_cycle` is the row a running
/// cycle holds: that cycle writes the row once, when it finishes, so its
/// other notes (the floor, retention) land in it too; `None` inserts a row
/// here. The row names the artifact rolled
/// back FROM and the data it was trained on, which is what the anti-flap
/// rule later compares against.
pub fn roll_back_active(
    ledger: &Ledger,
    registry: &Registry,
    source: LearningSource,
    reason: &str,
    open_cycle: Option<i64>,
) -> Result<Result<RolledBack, NothingToRollBack>, CycleError> {
    let Some(active) = registry.active()? else {
        return Ok(Err(NothingToRollBack::NoActiveArtifact));
    };
    let Some(previous) = registry.previous_id()? else {
        return Ok(Err(NothingToRollBack::NoPreviousArtifact));
    };
    if previous == active.artifact_id {
        return Ok(Err(NothingToRollBack::NoPreviousArtifact));
    }
    let Some(restored) = registry.rollback()? else {
        return Ok(Err(NothingToRollBack::NoPreviousArtifact));
    };
    let finish = LearningCycleFinish {
        label_policy_version: Some(active.label_policy_version),
        dataset_fingerprint: Some(active.dataset_fingerprint.clone()),
        artifact_id: Some(active.artifact_id.clone()),
        action: Some(LearningAction::RolledBack),
        note: Some(format!("restored {restored}: {reason}")),
        ..LearningCycleFinish::default()
    };
    if open_cycle.is_none() {
        ledger.record_learning_rollback(source, &finish)?;
    }
    Ok(Ok(RolledBack {
        from: active.artifact_id,
        to: restored,
        reason: reason.to_string(),
        label_policy_version: active.label_policy_version,
        dataset_fingerprint: active.dataset_fingerprint,
    }))
}

/// What checking the active artifact found.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveCheck {
    /// The reason to roll it back, if there is one.
    pub reason: Option<RollbackReason>,
    /// Said when the check fell back to the default quality floor.
    pub floor_note: Option<String>,
}

/// Check the active artifact against the runs it routed.
pub fn active_artifact_check(
    ledger: &Ledger,
    registry: &Registry,
) -> Result<ActiveCheck, CycleError> {
    let nothing = ActiveCheck {
        reason: None,
        floor_note: None,
    };
    let Some(active) = registry.active()? else {
        return Ok(nothing);
    };
    // With no predecessor there is nothing to return to; the evidence is
    // not collected for a rollback that cannot happen.
    match registry.previous_id()? {
        Some(previous) if previous != active.artifact_id => {}
        _ => return Ok(nothing),
    }
    let (floor, floor_note) = match evaluation_floor(&active) {
        Ok(floor) => (floor, None),
        Err(why) => (
            super::evaluate::DEFAULT_QUALITY_FLOOR,
            Some(floor_note(&active.artifact_id, &why)),
        ),
    };
    let mut outcomes = Vec::new();
    for run in ledger.runs_routed_by_artifact(&active.artifact_id)? {
        outcomes.push(routed_outcome(ledger, &run)?);
    }
    Ok(ActiveCheck {
        reason: rollback_reason(&outcomes, floor),
        floor_note,
    })
}

/// Check the active artifact against the runs it routed; the reason to
/// roll it back, if there is one.
pub fn active_artifact_verdict(
    ledger: &Ledger,
    registry: &Registry,
) -> Result<Option<RollbackReason>, CycleError> {
    Ok(active_artifact_check(ledger, registry)?.reason)
}

/// Run one cycle. The caller holds the Training lease and has decided the
/// cycle is wanted; this opens the row, closes the ones a dead process left,
/// and never leaves its own row unfinished unless the process is killed.
pub fn run_cycle(
    inputs: &CycleInputs<'_>,
    progress: &mut dyn FnMut(Stage),
) -> Result<CycleReport, LedgerError> {
    let ledger = inputs.ledger;
    let cycle_id = ledger.begin_learning_cycle()?;
    let interrupted_closed = ledger.close_unfinished_learning_cycles(cycle_id)?;
    let mut report = CycleReport {
        cycle_id,
        action: LearningAction::None,
        artifact_id: None,
        dataset_fingerprint: None,
        label_policy_version: None,
        examples_by_class: BTreeMap::new(),
        examples_by_source: BTreeMap::new(),
        cohorts: Vec::new(),
        verdict: None,
        interrupted_closed,
        note: None,
        error: None,
        promote_hint: None,
    };
    // Reviewer labels are a side record: whatever happens to them, the
    // cycle goes on and only its note says so.
    if let Some(settings) = &inputs.review_outcomes {
        if let Some(note) = crate::review_outcome::cycle_note(ledger, settings) {
            add_note(&mut report, note);
        }
    }
    if let Err(e) = cycle_body(inputs, progress, &mut report) {
        if let CycleError::Ledger(ledger_error) = e {
            return Err(ledger_error);
        }
        report.action = LearningAction::Error;
        report.error = Some(e.to_string());
    }
    // Retention is housekeeping: failing at it is worth saying, not worth
    // losing the cycle's record over. It runs before the row closes so that
    // the row's note carries what it found.
    if let Err(e) = retain(inputs.datasets_dir, inputs.registry) {
        add_note(&mut report, format!("retention: {e}"));
    }
    if let Some(dir) = &inputs.classifier_dir {
        if let Err(e) = crate::router::capture::purge_expired(dir, inputs.now) {
            add_note(&mut report, format!("classifier inputs: {e}"));
        }
    }
    ledger.finish_learning_cycle(
        cycle_id,
        &LearningCycleFinish {
            label_policy_version: report.label_policy_version,
            dataset_fingerprint: report.dataset_fingerprint.clone(),
            examples_by_class: report.examples_by_class.clone(),
            examples_by_source: report.examples_by_source.clone(),
            cohorts: report.cohorts.clone(),
            artifact_id: report.artifact_id.clone(),
            evaluate: report.verdict.clone(),
            action: Some(report.action),
            error: report.error.clone(),
            note: report.note.clone(),
        },
    )?;
    Ok(report)
}

fn cycle_body(
    inputs: &CycleInputs<'_>,
    progress: &mut dyn FnMut(Stage),
    report: &mut CycleReport,
) -> Result<(), CycleError> {
    let ledger = inputs.ledger;
    let registry = inputs.registry;

    // The active artifact is judged first: a regression is acted on before
    // anything new is trained over it.
    let check = active_artifact_check(ledger, registry)?;
    if let Some(note) = check.floor_note {
        add_note(report, note);
    }
    if let Some(reason) = check.reason {
        match roll_back_active(
            ledger,
            registry,
            LearningSource::Cycle,
            &reason.to_string(),
            Some(report.cycle_id),
        )? {
            Ok(rolled) => {
                report.action = LearningAction::RolledBack;
                report.artifact_id = Some(rolled.from.clone());
                report.label_policy_version = Some(rolled.label_policy_version);
                report.dataset_fingerprint = Some(rolled.dataset_fingerprint.clone());
                add_note(
                    report,
                    format!(
                        "restored {}: {} (rolled back {})",
                        rolled.to, rolled.reason, rolled.from
                    ),
                );
                // `run_cycle` writes the row once, with every note.
                return Ok(());
            }
            Err(nothing) => {
                add_note(report, format!("rollback wanted ({reason}) but {nothing}"));
            }
        }
    }

    progress(Stage::Build);
    let (dataset, backfill, reverts) =
        build_dataset(ledger, inputs.repo, inputs.detect_reverts, inputs.now)?;
    let dataset_path = write_dataset(inputs.datasets_dir, &dataset, inputs.now)?;
    report.dataset_fingerprint = Some(dataset.fingerprint.clone());
    report.label_policy_version = Some(dataset.label_policy_version);
    report.examples_by_class = class_counts(&dataset);
    report.examples_by_source = source_counts(&dataset);
    report.cohorts = cohorts_covered(&dataset);
    let origin_count = |origin| {
        dataset
            .records
            .iter()
            .filter(|record| record.origin == origin)
            .count()
    };
    let salvage_note = format!(
        "{} excluded; salvage containment: {} computed, {} unreachable; {}; origins: live {}, \
         replay {}; counterfactual gap: {} task(s) (dataset {})",
        dataset.exclusions.len(),
        backfill.recorded,
        backfill.unreachable,
        reverts.summary(),
        origin_count(crate::learn::features::Origin::Live),
        origin_count(crate::learn::features::Origin::Replay),
        dataset.counterfactual_gap.tasks,
        dataset_path.display()
    );

    progress(Stage::Train);
    let artifact_id = artifact_id_at(inputs.now);
    let (artifact, eval_report) = match train_artifact(&dataset, &artifact_id, inputs.gates) {
        Ok(trained) => trained,
        // Nothing to learn from yet is a result, not a failure.
        Err(e @ (TrainError::NoTrainingRecords { .. } | TrainError::NoTierCoverage { .. })) => {
            progress(Stage::Evaluate);
            report.verdict = Some(LearningVerdict {
                passed: false,
                failed_gates: vec![e.to_string()],
            });
            add_note(report, salvage_note);
            return Ok(());
        }
        Err(e) => {
            report.error = Some(e.to_string());
            report.action = LearningAction::Error;
            add_note(report, salvage_note);
            return Ok(());
        }
    };
    registry.store(&artifact)?;
    report.artifact_id = Some(artifact_id.clone());

    progress(Stage::Evaluate);
    let failures = eval_report.gates.failures();
    report.verdict = Some(LearningVerdict {
        passed: failures.is_empty(),
        failed_gates: failures.iter().map(ToString::to_string).collect(),
    });
    if !failures.is_empty() {
        add_note(report, salvage_note);
        return Ok(());
    }
    progress(Stage::Promote);
    promote_candidate(inputs, report, &artifact_id, &salvage_note)
}

/// The last step: a stored artifact whose evidence is checked by
/// `Registry::evaluated` (every gate recomputed from its report) is promoted
/// through `Registry::promote` only when the `[learning]` envelope is
/// recorded and the anti-flap rule allows it. Otherwise it waits.
pub fn promote_candidate(
    inputs: &CycleInputs<'_>,
    report: &mut CycleReport,
    artifact_id: &str,
    note: &str,
) -> Result<(), CycleError> {
    let ledger = inputs.ledger;
    let registry = inputs.registry;
    let evaluated = registry.evaluated(artifact_id)?;
    let candidate = evaluated.artifact();
    let rollbacks = ledger.learning_rollbacks()?;
    let rolled_back: Vec<RolledBackData> = rollbacks
        .iter()
        .map(|row| RolledBackData {
            label_policy_version: row.label_policy_version,
            dataset_fingerprint: row.dataset_fingerprint.clone(),
        })
        .collect();
    let runs_since_rollback = match rollbacks.first() {
        Some(last) => Some(
            ledger
                .terminal_root_runs_since(last.ended_at.as_deref().unwrap_or(&last.started_at))?,
        ),
        None => None,
    };
    let active = registry.active()?;
    let active_data = active
        .as_ref()
        .map(|a| (a.dataset_fingerprint.as_str(), a.label_policy_version));
    if let Some(why) = withheld(
        &candidate.dataset_fingerprint,
        candidate.label_policy_version,
        &rolled_back,
        active_data,
        runs_since_rollback,
    ) {
        add_note(report, format!("{why}; {note}"));
        return Ok(());
    }
    if inputs.auto_promote {
        registry.promote(&evaluated)?;
        report.action = LearningAction::Promoted;
        add_note(report, format!("promoted {artifact_id}; {note}"));
    } else {
        report.action = LearningAction::AwaitingPromotion;
        report.promote_hint = Some(format!("relais promote {artifact_id}"));
        add_note(report, note.to_string());
    }
    Ok(())
}

/// Keep the [`KEEP_DATASETS`] newest datasets, and every artifact the
/// registry points at plus the [`KEEP_ARTIFACTS`] newest.
pub fn retain(datasets_dir: &Path, registry: &Registry) -> Result<(), CycleError> {
    // A datasets failure is kept, not returned: the artifacts are trimmed
    // regardless, and the failure is reported after.
    let datasets = trim_datasets(datasets_dir);
    let mut ids = registry.list();
    ids.sort();
    let excess = ids.len().saturating_sub(KEEP_ARTIFACTS);
    for old in ids.into_iter().take(excess) {
        // Refuses the ids the registry still points at.
        registry.remove_unreferenced(&old)?;
    }
    datasets
}

fn trim_datasets(datasets_dir: &Path) -> Result<(), CycleError> {
    let io = |what, path: &Path| {
        let path = path.to_path_buf();
        move |cause| CycleError::Io { what, path, cause }
    };
    let entries = match std::fs::read_dir(datasets_dir) {
        Ok(entries) => entries,
        // No datasets directory yet: nothing to trim.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io("cannot read", datasets_dir)(e)),
    };
    let mut datasets: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    datasets.sort();
    let excess = datasets.len().saturating_sub(KEEP_DATASETS);
    for old in datasets.into_iter().take(excess) {
        std::fs::remove_file(&old).map_err(io("cannot remove", &old))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DispatchId, TaskId};
    use crate::learn::dataset::LABEL_POLICY_VERSION;
    use crate::learn::fixtures::{artifact, evaluation};
    use crate::lifecycle::{State, UsagePhase};
    use crate::outcome::{Outcome, OutcomeDetail, Strategy};
    use crate::policy::Tier;
    use crate::route::RoutedBy;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("a timestamp")
            .with_timezone(&Utc)
    }

    // ------------------------------------------------------------ due ----

    #[test]
    fn a_machine_that_never_ran_a_cycle_is_due() {
        assert_eq!(due(None, 0, at("2026-10-10T12:00:00Z")), Due::Yes);
    }

    #[test]
    fn within_twenty_hours_it_is_not_due_and_names_the_reason_and_the_next_time() {
        let now = at("2026-10-10T12:00:00Z");
        let verdict = due(Some("2026-10-10T00:00:00+00:00"), 50, now);
        let line = verdict.line().expect("not due");
        assert!(line.starts_with("not due: "), "{line}");
        assert!(line.contains("started 12 h ago"), "{line}");
        assert!(line.contains("next due 2026-10-10T20:00Z"), "{line}");
        // Just inside the window, however much has finished since.
        assert!(due(Some("2026-10-09T16:00:01Z"), 50, now).line().is_some());
    }

    #[test]
    fn after_twenty_hours_it_is_due_only_when_a_run_finished_since() {
        let now = at("2026-10-10T12:00:00Z");
        let last = Some("2026-10-09T16:00:00Z");
        let quiet = due(last, 0, now).line().expect("nothing to learn from");
        assert!(quiet.contains("no run reached a terminal state"), "{quiet}");
        assert_eq!(due(last, 1, now), Due::Yes);
    }

    // ------------------------------------------------------- rollback ----

    /// `accepted` accepted and `rest` not accepted runs.
    fn mix(accepted: usize, rest: usize) -> Vec<RoutedOutcome> {
        let mut outcomes = vec![RoutedOutcome::Accepted; accepted];
        outcomes.extend(vec![RoutedOutcome::NotAccepted; rest]);
        outcomes
    }

    #[test]
    fn at_ten_runs_an_upper_bound_below_the_floor_rolls_back_and_above_does_not() {
        let floor = 0.75;
        // 4 of 10: the one-sided 95% upper bound is about 0.71.
        let upper = clopper_pearson_upper(4, 10, ONE_SIDED_95);
        assert!(upper < floor, "{upper}");
        assert!(matches!(
            rollback_reason(&mix(4, 6), floor),
            Some(RollbackReason::AcceptanceBelowFloor {
                accepted: 4,
                runs: 10,
                ..
            })
        ));
        // 5 of 10: the bound is about 0.78, so the data cannot rule the floor out.
        let upper = clopper_pearson_upper(5, 10, ONE_SIDED_95);
        assert!(upper >= floor, "{upper}");
        assert_eq!(rollback_reason(&mix(5, 5), floor), None);
    }

    #[test]
    fn at_thirty_runs_the_same_rule_holds_with_a_tighter_bound() {
        let floor = 0.75;
        // 17 of 30 (57%): the bound is about 0.72; 19 of 30 (63%): about 0.78.
        let low = clopper_pearson_upper(17, 30, ONE_SIDED_95);
        let high = clopper_pearson_upper(19, 30, ONE_SIDED_95);
        assert!(low < floor && floor <= high, "{low} {high}");
        assert!(rollback_reason(&mix(17, 13), floor).is_some());
        assert_eq!(rollback_reason(&mix(19, 11), floor), None);
    }

    #[test]
    fn under_ten_labelled_runs_the_bound_is_not_consulted() {
        // 0 of 9 would be far below any floor, and is still not enough runs.
        assert_eq!(rollback_reason(&mix(0, 9), 0.75), None);
        // Unlabelled runs do not count toward the ten.
        let mut outcomes = mix(0, 9);
        outcomes.extend(vec![RoutedOutcome::Unlabelled; 20]);
        assert_eq!(rollback_reason(&outcomes, 0.75), None);
    }

    #[test]
    fn one_regressed_run_rolls_back_whatever_the_rest_did() {
        let mut outcomes = mix(3, 0);
        outcomes.push(RoutedOutcome::Regressed);
        assert_eq!(
            rollback_reason(&outcomes, 0.75),
            Some(RollbackReason::Regressed { runs: 1 })
        );
    }

    // ------------------------------------------------------ anti-flap ----

    #[test]
    fn data_matching_a_rolled_back_artifact_is_withheld() {
        let rolled_back = [RolledBackData {
            label_policy_version: Some(2),
            dataset_fingerprint: Some("fp".into()),
        }];
        assert_eq!(
            withheld("fp", 2, &rolled_back, None, Some(500)),
            Some(Withheld::SameDataAsRolledBack)
        );
        // Another fingerprint, or another label policy, is new data.
        assert_eq!(withheld("other", 2, &rolled_back, None, Some(500)), None);
        assert_eq!(withheld("fp", 3, &rolled_back, None, Some(500)), None);
    }

    #[test]
    fn nothing_is_promoted_until_ten_runs_ended_after_a_rollback() {
        assert_eq!(
            withheld("fp", 2, &[], None, Some(9)),
            Some(Withheld::TooFewRunsSinceRollback { have: 9, need: 10 })
        );
        assert_eq!(withheld("fp", 2, &[], None, Some(10)), None);
        assert_eq!(
            withheld("fp", 2, &[], None, None),
            None,
            "never rolled back"
        );
    }

    #[test]
    fn the_active_artifacts_own_data_is_nothing_new() {
        assert_eq!(
            withheld("fp", 2, &[], Some(("fp", 2)), None),
            Some(Withheld::SameDataAsActive)
        );
    }

    // ------------------------------------------------------------ log ----

    #[test]
    fn the_log_is_created_0600_and_appended() {
        let dir = crate::test_support::temp_dir("cycle-log");
        let path = dir.join("nested").join("learn-cycle.log");
        CycleLog::open(&path).expect("opens").line("first");
        CycleLog::open(&path).expect("reopens").line("second");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_log_of_one_mebibyte_is_rotated_keeping_one_old_file() {
        let dir = crate::test_support::temp_dir("cycle-log-rotate");
        let path = dir.join("learn-cycle.log");
        let mut old = path.as_os_str().to_owned();
        old.push(".1");
        let old = PathBuf::from(old);

        let big = "x".repeat(LOG_ROTATE_BYTES as usize);
        std::fs::write(&path, &big).unwrap();
        std::fs::write(&old, "stale").unwrap();
        CycleLog::open(&path).expect("rotates").line("fresh");
        assert_eq!(std::fs::read_to_string(&old).unwrap(), big, "one old file");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");

        // Just under the limit is not rotated.
        std::fs::write(&path, "y".repeat(LOG_ROTATE_BYTES as usize - 1)).unwrap();
        CycleLog::open(&path).expect("opens").line("z");
        assert_eq!(std::fs::read_to_string(&old).unwrap(), big);
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("yz\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, &big).unwrap();
            CycleLog::open(&path).expect("rotates");
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "the new file is 0600 too");
        }
    }

    // --------------------------------------------------------- ledger ----

    fn repo_policy() -> RepoPolicy {
        RepoPolicy::from_toml_str(
            r#"schema_version = 1
[models.implementation]
id = "sonnet"
[[verification.profiles.p.commands]]
argv = ["true"]
"#,
        )
        .expect("policy")
    }

    fn contract() -> crate::contract::TaskContract {
        crate::contract::TaskContract::from_json_str(
            r#"{"schema_version":1,"kind":"change","objective":"Fix the escaping",
                "base_ref":"HEAD","write_scope":["crates/**"],"acceptance":["parses"],
                "verification_profile":"p"}"#,
        )
        .expect("contract")
    }

    struct World {
        /// Removes the directory when the test ends.
        _dir: crate::test_support::TempDir,
        datasets: PathBuf,
        ledger: Ledger,
        registry: Registry,
    }

    impl World {
        fn new(name: &str) -> Self {
            let dir = crate::test_support::temp_dir(&format!("cycle-{name}"));
            let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
            let registry = Registry::open(&dir.join("registry")).expect("registry");
            Self {
                datasets: dir.join("datasets"),
                _dir: dir,
                ledger,
                registry,
            }
        }

        fn inputs<'a>(&'a self, repo: &'a RepoPolicy, auto_promote: bool) -> CycleInputs<'a> {
            CycleInputs {
                ledger: &self.ledger,
                registry: &self.registry,
                repo,
                datasets_dir: &self.datasets,
                gates: GateSettings {
                    quality_floor: 0.75,
                    min_supported_test_records: 20,
                    max_abstention_rate: 0.5,
                },
                auto_promote,
                classifier_dir: None,
                review_outcomes: None,
                detect_reverts: true,
                now: Utc::now(),
            }
        }

        /// A stored artifact that passes every gate, promoted.
        fn promote(&self, id: &str, fingerprint: &str) {
            let mut stored = artifact(id, Some(evaluation(id, true)));
            stored.dataset_fingerprint = fingerprint.into();
            let mut report = evaluation(id, true);
            report["dataset_fingerprint"] = serde_json::json!(fingerprint);
            stored.evaluation = Some(report);
            self.registry.store(&stored).expect("store");
            let checked = self.registry.evaluated(id).expect("evidence holds");
            self.registry.promote(&checked).expect("promote");
        }

        /// A run the artifact routed, ended in `state` with `outcome`.
        fn routed_run(
            &self,
            artifact_id: &str,
            name: &str,
            state: State,
            outcome: Option<OutcomeKind>,
        ) {
            let run = RunId::from_stored(name);
            let task = TaskId::from_stored(format!("task-{name}"));
            let contract = contract();
            let json = serde_json::to_string(&contract.canonical_value()).expect("json");
            self.ledger
                .insert_run(&run, "/r", None, &task, "rk")
                .expect("run");
            let revision = self
                .ledger
                .insert_contract_revision(&run, &contract.hash(), &json, "HEAD", None)
                .expect("revision");
            let attempt = self
                .ledger
                .insert_attempt(
                    &run,
                    revision,
                    1,
                    "implementation",
                    UsagePhase::parse("initial").expect("phase"),
                )
                .expect("attempt");
            self.ledger
                .record_dispatch_intent(
                    &DispatchId::from_stored(format!("{name}-d1")),
                    &run,
                    Some(attempt),
                    &serde_json::json!({"model": "sonnet", "effort": "medium"}),
                    0,
                    RoutedBy::LearnedArtifact,
                )
                .expect("intent");
            self.ledger
                .record_prediction(&run, artifact_id, "input", &serde_json::json!({}))
                .expect("prediction");
            self.ended(name, state);
            if let Some(kind) = outcome {
                let outcome = Outcome::new(
                    kind,
                    OutcomeDetail {
                        candidate_sha: Some("abc123".into()),
                        strategy: Strategy {
                            tier: Tier::Implementation,
                            models: vec!["sonnet".into()],
                            escalated: false,
                        },
                        correction_magnitude: None,
                        evidence: vec![],
                        actor: "a person".into(),
                        note: None,
                        source: None,
                    },
                )
                .expect("outcome");
                self.ledger
                    .record_outcome(&run, &task, &outcome)
                    .expect("record");
            }
        }

        fn ended(&self, name: &str, state: State) {
            self.ended_at(name, state, &crate::ledger::now_rfc3339());
        }

        fn ended_at(&self, name: &str, state: State, stamp: &str) {
            self.ledger
                .record_transition(&crate::ledger::Transition {
                    run_id: RunId::from_stored(name),
                    attempt_id: None,
                    from_state: Some(State::Verifying),
                    to_state: state,
                    reason: "test".into(),
                    detail: None,
                    at: stamp.into(),
                })
                .expect("transition");
        }

        /// A root run that reached a terminal state long after any rollback.
        fn later_terminal_run(&self, name: &str) {
            self.ledger
                .insert_run(
                    &RunId::from_stored(name),
                    "/r",
                    None,
                    &TaskId::from_stored(format!("task-{name}")),
                    "rk",
                )
                .expect("run");
            self.ended_at(name, State::Failed, "2999-01-01T00:00:00+00:00");
        }

        fn rows(&self) -> Vec<crate::ledger::LearningCycleRow> {
            self.ledger.learning_cycles().expect("rows")
        }
    }

    fn silent() -> impl FnMut(Stage) {
        |_| {}
    }

    #[test]
    fn a_cycle_with_nothing_to_learn_from_records_one_row_and_says_why() {
        let world = World::new("empty");
        let repo = repo_policy();
        let report = run_cycle(&world.inputs(&repo, true), &mut silent()).expect("cycle");
        assert_eq!(report.action, LearningAction::None);
        assert!(report.verdict.is_some_and(|v| !v.passed));
        let rows = world.rows();
        assert_eq!(rows.len(), 1, "exactly one learning_cycles row");
        assert!(rows[0].ended_at.is_some());
        assert_eq!(rows[0].source, LearningSource::Cycle);
        assert_eq!(rows[0].action, LearningAction::None);
    }

    #[test]
    fn a_cycle_deletes_classifier_day_files_past_their_ninety_days() {
        let world = World::new("classifier-ttl");
        let dir = world._dir.join("classifier");
        std::fs::create_dir_all(&dir).expect("dir");
        let now = Utc::now();
        let day = |ago: i64| {
            format!(
                "{}.jsonl",
                (now.date_naive() - Duration::days(ago)).format("%Y-%m-%d")
            )
        };
        for ago in [0, 90, 91, 200] {
            std::fs::write(dir.join(day(ago)), "{}\n").expect("file");
        }
        let repo = repo_policy();
        let mut inputs = world.inputs(&repo, false);
        inputs.classifier_dir = Some(dir.clone());
        run_cycle(&inputs, &mut silent()).expect("cycle");
        assert!(dir.join(day(0)).exists() && dir.join(day(90)).exists());
        assert!(!dir.join(day(91)).exists() && !dir.join(day(200)).exists());
    }

    #[test]
    fn an_unfinished_row_is_closed_as_interrupted_by_the_next_cycle() {
        let world = World::new("interrupted");
        let dead = world.ledger.begin_learning_cycle().expect("begin");
        assert!(world.rows()[0].ended_at.is_none(), "killed before it ended");
        let repo = repo_policy();
        let report = run_cycle(&world.inputs(&repo, true), &mut silent()).expect("cycle");
        assert_eq!(report.interrupted_closed, 1);
        let rows = world.rows();
        let closed = rows.iter().find(|row| row.id == dead).expect("the row");
        assert_eq!(closed.action, LearningAction::Interrupted);
        assert!(closed.ended_at.is_some());
        assert!(
            rows.iter().all(|row| row.ended_at.is_some()),
            "no row is left open"
        );
    }

    /// `accepted` accepted and `rest` failed runs, all routed by `art-b`.
    fn seed_routed(world: &World, accepted: usize, rest: usize) {
        for index in 0..accepted {
            world.routed_run("art-b", &format!("ok-{index}"), State::Accepted, None);
        }
        for index in 0..rest {
            world.routed_run("art-b", &format!("bad-{index}"), State::Failed, None);
        }
    }

    fn two_artifacts(world: &World) {
        world.promote("art-a", "fp-a");
        world.promote("art-b", "fp-b");
    }

    fn assert_rolled_back_by_the_cycle(world: &World) {
        let repo = repo_policy();
        let report = run_cycle(&world.inputs(&repo, true), &mut silent()).expect("cycle");
        assert_eq!(report.action, LearningAction::RolledBack, "{report:?}");
        assert_eq!(
            world.registry.active().unwrap().unwrap().artifact_id,
            "art-a"
        );
        let rows = world.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].action, LearningAction::RolledBack);
        assert_eq!(rows[0].source, LearningSource::Cycle);
        assert_eq!(rows[0].artifact_id.as_deref(), Some("art-b"));
        assert_eq!(rows[0].dataset_fingerprint.as_deref(), Some("fp-b"));
        assert!(rows[0].ended_at.is_some());
        assert_eq!(
            world.ledger.last_cycle_started().unwrap().map(|row| row.id),
            Some(rows[0].id),
            "a cycle that rolled back is still a cycle for --if-due"
        );
    }

    #[test]
    fn a_reverted_learner_routed_run_rolls_the_artifact_back() {
        let world = World::new("reverted");
        two_artifacts(&world);
        seed_routed(&world, 3, 0);
        world.routed_run(
            "art-b",
            "reverted",
            State::Accepted,
            Some(OutcomeKind::Reverted),
        );
        assert_rolled_back_by_the_cycle(&world);
    }

    #[test]
    fn a_confirmed_regression_of_a_learner_routed_run_rolls_back_too() {
        let world = World::new("regression");
        two_artifacts(&world);
        world.routed_run(
            "art-b",
            "regressed",
            State::Accepted,
            Some(OutcomeKind::ConfirmedRegression),
        );
        assert_rolled_back_by_the_cycle(&world);
    }

    #[test]
    fn ten_routed_runs_four_accepted_roll_back_and_five_do_not() {
        let below = World::new("n10-below");
        two_artifacts(&below);
        seed_routed(&below, 4, 6);
        assert_rolled_back_by_the_cycle(&below);

        let above = World::new("n10-above");
        two_artifacts(&above);
        seed_routed(&above, 5, 5);
        assert_eq!(
            active_artifact_verdict(&above.ledger, &above.registry).unwrap(),
            None
        );
    }

    #[test]
    fn thirty_routed_runs_seventeen_accepted_roll_back_and_nineteen_do_not() {
        let below = World::new("n30-below");
        two_artifacts(&below);
        seed_routed(&below, 17, 13);
        assert!(matches!(
            active_artifact_verdict(&below.ledger, &below.registry).unwrap(),
            Some(RollbackReason::AcceptanceBelowFloor { runs: 30, .. })
        ));

        let above = World::new("n30-above");
        two_artifacts(&above);
        seed_routed(&above, 19, 11);
        assert_eq!(
            active_artifact_verdict(&above.ledger, &above.registry).unwrap(),
            None
        );
    }

    #[test]
    fn runs_another_artifact_routed_do_not_count_against_the_active_one() {
        let world = World::new("other-artifact");
        two_artifacts(&world);
        world.routed_run("art-a", "old", State::Accepted, Some(OutcomeKind::Reverted));
        assert_eq!(
            active_artifact_verdict(&world.ledger, &world.registry).unwrap(),
            None
        );
    }

    #[test]
    fn an_artifact_with_no_predecessor_is_never_rolled_back() {
        let world = World::new("sole");
        world.promote("art-a", "fp-a");
        world.routed_run(
            "art-a",
            "reverted",
            State::Accepted,
            Some(OutcomeKind::Reverted),
        );
        assert_eq!(
            active_artifact_verdict(&world.ledger, &world.registry).unwrap(),
            None
        );
        assert_eq!(
            roll_back_active(
                &world.ledger,
                &world.registry,
                LearningSource::Person,
                "x",
                None
            )
            .unwrap(),
            Err(NothingToRollBack::NoPreviousArtifact)
        );
        assert!(
            world.rows().is_empty(),
            "nothing was rolled back, nothing recorded"
        );
    }

    #[test]
    fn a_malformed_evaluation_on_a_rollbackable_artifact_says_the_default_floor_was_used() {
        let world = World::new("floor-malformed");
        two_artifacts(&world);
        let mut active = world.registry.active().unwrap().unwrap();
        active.evaluation = Some(serde_json::json!({"bogus": 1}));
        world.registry.store(&active).unwrap();
        let check = active_artifact_check(&world.ledger, &world.registry).unwrap();
        let note = check.floor_note.expect("a floor note");
        assert!(note.contains("unreadable"), "{note}");
        assert!(note.contains("default quality floor"), "{note}");
        // The cycle keeps it in the row even when it later errors.
        let repo = repo_policy();
        let report = run_cycle(&world.inputs(&repo, true), &mut silent()).expect("cycle");
        let row = &world.rows()[0];
        assert!(
            row.note
                .as_deref()
                .is_some_and(|n| n.contains("unreadable")),
            "{report:?} {row:?}"
        );
    }

    #[test]
    fn a_rollback_row_keeps_the_floor_note_and_what_was_rolled_back() {
        let world = World::new("rollback-notes");
        two_artifacts(&world);
        let mut active = world.registry.active().unwrap().unwrap();
        active.evaluation = Some(serde_json::json!({"bogus": 1}));
        world.registry.store(&active).unwrap();
        seed_routed(&world, 0, 12);
        let repo = repo_policy();
        let report = run_cycle(&world.inputs(&repo, true), &mut silent()).expect("cycle");
        assert_eq!(report.action, LearningAction::RolledBack, "{report:?}");
        let row = &world.rows()[0];
        let note = row.note.clone().unwrap_or_default();
        assert!(note.contains("unreadable"), "{row:?}");
        assert!(note.contains("restored "), "{row:?}");
        assert_eq!(row.action, LearningAction::RolledBack, "{row:?}");
        assert_eq!(
            row.dataset_fingerprint,
            Some(active.dataset_fingerprint.clone())
        );
        assert_eq!(row.label_policy_version, Some(active.label_policy_version));
    }

    #[test]
    fn a_missing_evaluation_is_told_apart_from_a_malformed_one() {
        let world = World::new("floor-missing");
        two_artifacts(&world);
        let mut active = world.registry.active().unwrap().unwrap();
        active.evaluation = None;
        world.registry.store(&active).unwrap();
        let note = active_artifact_check(&world.ledger, &world.registry)
            .unwrap()
            .floor_note
            .expect("a floor note");
        assert!(note.contains("has no evaluation report"), "{note}");
    }

    #[test]
    fn no_predecessor_means_no_floor_note() {
        let world = World::new("floor-sole");
        world.promote("art-a", "fp-a");
        let mut active = world.registry.active().unwrap().unwrap();
        active.evaluation = Some(serde_json::json!({"bogus": 1}));
        world.registry.store(&active).unwrap();
        let check = active_artifact_check(&world.ledger, &world.registry).unwrap();
        assert_eq!(check.floor_note, None);
        let repo = repo_policy();
        run_cycle(&world.inputs(&repo, true), &mut silent()).expect("cycle");
        let note = world.rows()[0].note.clone().unwrap_or_default();
        assert!(!note.contains("quality floor"), "{note}");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_datasets_directory_is_noted_and_artifacts_are_still_trimmed() {
        use std::os::unix::fs::PermissionsExt;
        let world = World::new("retain-unreadable");
        for index in 0..8 {
            stored_candidate(&world, &format!("old-{index}"), &format!("fp-{index}"));
        }
        std::fs::create_dir_all(&world.datasets).unwrap();
        std::fs::set_permissions(&world.datasets, std::fs::Permissions::from_mode(0o000)).unwrap();
        let outcome = retain(&world.datasets, &world.registry);
        std::fs::set_permissions(&world.datasets, std::fs::Permissions::from_mode(0o755)).unwrap();
        if std::fs::read_dir(&world.datasets).is_ok() && outcome.is_ok() {
            // Running with privileges that ignore the mode: nothing to test.
            return;
        }
        let error = outcome.expect_err("the read error is reported");
        assert!(error.to_string().contains("cannot read"), "{error}");
        assert_eq!(
            world.registry.list().len(),
            KEEP_ARTIFACTS,
            "trimming ran after the datasets error"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_cycle_over_an_unreadable_datasets_directory_notes_the_retention_error() {
        use std::os::unix::fs::PermissionsExt;
        let world = World::new("retain-note");
        for index in 0..8 {
            stored_candidate(&world, &format!("old-{index}"), &format!("fp-{index}"));
        }
        std::fs::create_dir_all(&world.datasets).unwrap();
        std::fs::set_permissions(&world.datasets, std::fs::Permissions::from_mode(0o000)).unwrap();
        let repo = repo_policy();
        let ignored = std::fs::read_dir(&world.datasets).is_ok();
        let report = run_cycle(&world.inputs(&repo, true), &mut silent());
        std::fs::set_permissions(&world.datasets, std::fs::Permissions::from_mode(0o755)).unwrap();
        if ignored {
            // Running with privileges that ignore the mode: nothing to test.
            return;
        }
        let report = report.expect("cycle");
        let row = &world.rows()[0];
        assert!(
            row.note.as_deref().is_some_and(|n| n.contains("retention")),
            "{report:?} {row:?}"
        );
        // Eight were stored; the cycle adds at most its own candidate.
        const _: () = assert!(8 > KEEP_ARTIFACTS + 1);
        assert!(
            world.registry.list().len() <= KEEP_ARTIFACTS + 1,
            "old unreferenced artifacts were removed: {:?}",
            world.registry.list()
        );
    }

    #[test]
    fn a_persons_rollback_writes_a_row_with_source_person() {
        let world = World::new("person");
        two_artifacts(&world);
        let rolled = roll_back_active(
            &world.ledger,
            &world.registry,
            LearningSource::Person,
            "rolled back by a person",
            None,
        )
        .unwrap()
        .expect("rolled back");
        assert_eq!(
            (rolled.from.as_str(), rolled.to.as_str()),
            ("art-b", "art-a")
        );
        let rows = world.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, LearningSource::Person);
        assert_eq!(rows[0].action, LearningAction::RolledBack);
        assert_eq!(rows[0].artifact_id.as_deref(), Some("art-b"));
        assert!(rows[0].ended_at.is_some());
        assert_eq!(
            world.ledger.last_cycle_started().unwrap(),
            None,
            "a person's rollback starts no cycle"
        );
        // A second rollback has nowhere to go.
        assert_eq!(
            roll_back_active(
                &world.ledger,
                &world.registry,
                LearningSource::Person,
                "x",
                None
            )
            .unwrap(),
            Err(NothingToRollBack::NoPreviousArtifact)
        );
        assert_eq!(world.rows().len(), 1);
    }

    // -------------------------------------------------------- promote ----

    fn stored_candidate(world: &World, id: &str, fingerprint: &str) {
        let mut candidate = artifact(id, None);
        candidate.dataset_fingerprint = fingerprint.into();
        let mut report = evaluation(id, true);
        report["dataset_fingerprint"] = serde_json::json!(fingerprint);
        candidate.evaluation = Some(report);
        world.registry.store(&candidate).expect("store");
    }

    fn promote_it(world: &World, auto: bool, id: &str) -> CycleReport {
        let repo = repo_policy();
        let inputs = world.inputs(&repo, auto);
        let cycle_id = world.ledger.begin_learning_cycle().expect("begin");
        let mut report = CycleReport {
            cycle_id,
            action: LearningAction::None,
            artifact_id: Some(id.into()),
            dataset_fingerprint: None,
            label_policy_version: None,
            examples_by_class: BTreeMap::new(),
            examples_by_source: BTreeMap::new(),
            cohorts: Vec::new(),
            verdict: None,
            interrupted_closed: 0,
            note: None,
            error: None,
            promote_hint: None,
        };
        promote_candidate(&inputs, &mut report, id, "note").expect("promote step");
        report
    }

    #[test]
    fn without_the_envelope_a_passing_artifact_waits_and_names_the_command() {
        let world = World::new("awaiting");
        stored_candidate(&world, "art-new", "fp-new");
        let report = promote_it(&world, false, "art-new");
        assert_eq!(report.action, LearningAction::AwaitingPromotion);
        assert_eq!(
            report.promote_hint.as_deref(),
            Some("relais promote art-new")
        );
        assert!(world.registry.active().unwrap().is_none());
    }

    #[test]
    fn inside_the_envelope_a_passing_artifact_is_promoted() {
        let world = World::new("auto");
        stored_candidate(&world, "art-new", "fp-new");
        let report = promote_it(&world, true, "art-new");
        assert_eq!(report.action, LearningAction::Promoted);
        assert_eq!(
            world.registry.active().unwrap().unwrap().artifact_id,
            "art-new"
        );
    }

    fn person_rollback_of(world: &World, fingerprint: &str) {
        world
            .ledger
            .record_learning_rollback(
                LearningSource::Person,
                &LearningCycleFinish {
                    label_policy_version: Some(LABEL_POLICY_VERSION),
                    dataset_fingerprint: Some(fingerprint.into()),
                    artifact_id: Some("art-bad".into()),
                    ..LearningCycleFinish::default()
                },
            )
            .expect("rollback row");
    }

    #[test]
    fn an_artifact_trained_on_a_rolled_back_artifacts_data_is_not_promoted() {
        let world = World::new("flap-same");
        person_rollback_of(&world, "fp-new");
        for index in 0..12 {
            world.later_terminal_run(&format!("later-{index}"));
        }
        stored_candidate(&world, "art-new", "fp-new");
        let report = promote_it(&world, true, "art-new");
        assert_eq!(report.action, LearningAction::None);
        assert!(world.registry.active().unwrap().is_none());
        let note = report.note.expect("says why");
        assert!(note.contains("same data and label policy"), "{note}");
    }

    #[test]
    fn nothing_is_promoted_until_ten_runs_ended_after_the_rollback() {
        let world = World::new("flap-runs");
        person_rollback_of(&world, "fp-old");
        stored_candidate(&world, "art-new", "fp-new");
        for index in 0..9 {
            world.later_terminal_run(&format!("later-{index}"));
        }
        let held = promote_it(&world, true, "art-new");
        assert_eq!(held.action, LearningAction::None);
        assert!(held.note.expect("why").contains("9 of the 10"));
        assert!(world.registry.active().unwrap().is_none());

        world.later_terminal_run("later-9");
        let promoted = promote_it(&world, true, "art-new");
        assert_eq!(promoted.action, LearningAction::Promoted);
    }
}
