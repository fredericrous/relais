//! The ledger's error type and the parsers that turn stored text into typed values.

use super::*;

#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// A stored row this binary cannot read: an unknown state string, a
    /// receipt that is not JSON. A ledger is external state — truncated,
    /// hand-edited or written by another version — so reading one is a
    /// fallible operation, never an assertion about what SQLite holds.
    Corrupt {
        what: String,
        detail: String,
    },
    /// The ledger carries more applied migrations than this binary knows:
    /// a newer relais wrote it, and writing it back could lose what that
    /// version recorded.
    SchemaAhead {
        found: u64,
        known: u64,
    },
    /// A read-only open met a ledger on another schema than this binary's,
    /// older or newer. A reader that cannot migrate cannot tell what the
    /// columns mean, so it does not read.
    SchemaMismatch {
        found: u64,
        known: u64,
    },
}

impl LedgerError {
    /// Does this error say the disk is full (SPEC §10)?
    pub fn is_storage_full(&self) -> bool {
        match self {
            LedgerError::Io(e) => crate::storage_full::is_full(e),
            LedgerError::Sqlite(e) => crate::storage_full::sqlite_full(e),
            _ => false,
        }
    }
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Sqlite(e) => write!(f, "ledger: {e}"),
            LedgerError::Io(e) => write!(f, "ledger: {e}"),
            LedgerError::Corrupt { what, detail } => {
                write!(f, "ledger: {what} cannot be read: {detail}")
            }
            LedgerError::SchemaAhead { found, known } => write!(
                f,
                "ledger: {found} applied migration(s), this relais knows {known} — \
                 a newer relais wrote this ledger; upgrade relais or point \
                 RELAIS_STATE_DIR at another one"
            ),
            LedgerError::SchemaMismatch { found, known } => write!(
                f,
                "ledger: {found} applied migration(s), this relais reads {known} — \
                 not read; the next relais that writes it migrates it"
            ),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<rusqlite::Error> for LedgerError {
    fn from(e: rusqlite::Error) -> Self {
        LedgerError::Sqlite(e)
    }
}

impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e)
    }
}

pub(super) type Result<T> = std::result::Result<T, LedgerError>;

/// A state string as the ledger stored it. An unrecognised one is a
/// corrupt row — a value from a newer relais, a truncated write, a hand
/// edit — and the caller is told, rather than the state silently reading
/// as "no such run" or the process aborting mid-report.
pub(super) fn parse_state(stored: &str) -> Result<State> {
    State::parse(stored).map_err(|unknown| LedgerError::Corrupt {
        what: unknown.what.into(),
        detail: unknown.to_string(),
    })
}

/// A tier string as an attempt row stored it.
pub(super) fn parse_tier(stored: &str) -> Result<Tier> {
    Tier::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "attempt tier".into(),
        detail: format!("`{stored}` is not a tier this relais knows"),
    })
}

/// A `routed_by` string as a dispatch row stored it. An unparseable value
/// is a corrupt row, never silently read as `ConservativeBaseline` — that
/// would assert a decision was actually made when the string just names
/// nothing this relais knows.
pub(super) fn parse_routed_by(stored: &str) -> Result<RoutedBy> {
    RoutedBy::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "dispatch routed_by".into(),
        detail: format!("`{stored}` is not a routing decision this relais knows"),
    })
}

/// A run purpose string as a run row stored it.
pub(super) fn parse_run_purpose(stored: &str) -> Result<RunPurpose> {
    RunPurpose::parse(stored).map_err(|unknown| LedgerError::Corrupt {
        what: unknown.what.into(),
        detail: unknown.to_string(),
    })
}

/// A cost completeness string as a trial row stored it — the same
/// quoted-JSON spelling `record_usage` writes (SPEC §11): `NULL` means
/// "not settled yet", never `Unknown`, which is a positive claim that
/// settlement happened and reported no usage.
pub(super) fn parse_cost_completeness(stored: &str) -> Result<CostCompleteness> {
    serde_json::from_str(stored).map_err(|e| LedgerError::Corrupt {
        what: "trial cost completeness".into(),
        detail: e.to_string(),
    })
}

/// A trial outcome string as a trial row stored it.
pub(super) fn parse_trial_outcome(stored: &str) -> Result<TrialOutcome> {
    TrialOutcome::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "trial outcome".into(),
        detail: format!("`{stored}` is not a trial outcome this relais knows"),
    })
}

/// An evidence kind string as an evidence row stored it.
pub(super) fn parse_evidence_kind(stored: &str) -> Result<EvidenceKind> {
    EvidenceKind::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "evidence kind".into(),
        detail: format!("`{stored}` is not an evidence kind this relais knows"),
    })
}

/// An `outcomes` row as columns, parsed into a [`StoredOutcome`] (P6): an
/// unknown kind, a missing task id or a detail that will not parse as
/// [`OutcomeDetail`] is a `Corrupt` row the caller reports, never a
/// silently dropped or defaulted outcome.
pub(super) fn parse_outcome_row(
    run_id: String,
    task_id: Option<String>,
    kind: String,
    detail_json: Option<String>,
    at: String,
) -> Result<StoredOutcome> {
    let kind = OutcomeKind::parse(&kind).ok_or_else(|| LedgerError::Corrupt {
        what: format!("the outcome kind of run {run_id}"),
        detail: format!("`{kind}` is not an outcome kind this relais knows"),
    })?;
    let task_id = task_id.ok_or_else(|| LedgerError::Corrupt {
        what: format!("the outcome of run {run_id}"),
        detail: "no task_id was recorded".into(),
    })?;
    let detail_json = detail_json.ok_or_else(|| LedgerError::Corrupt {
        what: format!("the outcome of run {run_id}"),
        detail: "no detail was recorded".into(),
    })?;
    let detail: OutcomeDetail =
        serde_json::from_str(&detail_json).map_err(|e| LedgerError::Corrupt {
            what: format!("the outcome detail of run {run_id}"),
            detail: e.to_string(),
        })?;
    Ok(StoredOutcome {
        run_id: RunId::from_stored(run_id),
        task_id: TaskId::from_stored(task_id),
        outcome: Outcome { kind, detail },
        at,
    })
}

/// A task origin string as the ledger stored it.
pub(super) fn parse_task_origin(stored: &str) -> Result<TaskOrigin> {
    TaskOrigin::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "task origin".into(),
        detail: format!("`{stored}` is not a task origin this relais knows"),
    })
}

/// A `trials` row as columns, parsed into a [`TrialRow`]: an
/// unrecognised outcome or cost completeness is a `Corrupt` row the
/// caller reports, never silently normalised into a settled-looking one.
pub(super) fn parse_trial_row(columns: TrialColumns) -> Result<TrialRow> {
    let (
        trial_id,
        task_id,
        source_run_id,
        incumbent_recipe_id,
        arm_recipe_id,
        arm_index,
        assignment_probability,
        seed,
        base_sha,
        contract_hash,
        verification_profile_hash,
        workspace_isolation,
        outcome,
        accepted_without_escalation,
        cost_micros,
        cost_completeness,
        duration_ms,
        created_at,
        arm_run_id,
        design_sha256,
        ground_truth,
        ground_truth_note,
        ground_truth_at,
    ) = columns;
    let ground_truth = match (ground_truth, ground_truth_note, ground_truth_at) {
        (None, None, None) => None,
        (Some(stored), Some(note), Some(at)) => Some(GroundTruth {
            verdict: GroundTruthVerdict::parse(&stored).ok_or_else(|| LedgerError::Corrupt {
                what: "trials.ground_truth".into(),
                detail: format!("`{stored}` is not a verdict this relais knows"),
            })?,
            note,
            at,
        }),
        _ => {
            return Err(LedgerError::Corrupt {
                what: "trials.ground_truth".into(),
                detail: "the verdict, its note and its time are not all present".into(),
            })
        }
    };
    Ok(TrialRow {
        trial_id: TrialId::from_stored(trial_id),
        task_id: TaskId::from_stored(task_id),
        source_run_id: RunId::from_stored(source_run_id),
        incumbent_recipe_id,
        arm_recipe_id,
        // `as u32` turned a stored -1 into 4294967295 and 4294967296
        // into 0 — a corrupt row read back as a plausible arm. `seed`
        // needs no such care: u64 <-> i64 round-trips every bit.
        arm_index: u32::try_from(arm_index).map_err(|_| LedgerError::Corrupt {
            what: "trials.arm_index".into(),
            detail: format!("{arm_index} is not an arm index"),
        })?,
        assignment_probability,
        seed: seed as u64,
        base_sha,
        contract_hash,
        verification_profile_hash,
        workspace_isolation,
        outcome: outcome.as_deref().map(parse_trial_outcome).transpose()?,
        accepted_without_escalation: accepted_without_escalation.map(|v| v != 0),
        // The figure and its completeness are paired HERE, through the
        // one constructor that refuses an impossible pair, so a row
        // written by an older binary — or by hand — that says "0, but
        // unknown" is Corrupt rather than a trial that reads as free.
        cost: parse_trial_cost(cost_micros, cost_completeness.as_deref())?,
        duration_ms,
        created_at,
        arm_run_id: arm_run_id.map(RunId::from_stored),
        design_sha256,
        ground_truth,
    })
}

/// Pair a stored cost with its stored completeness, refusing the two
/// combinations that say contradictory things. An unsettled trial has
/// neither and reads `None`; a settled one must have a coherent pair.
pub(super) fn parse_trial_cost(
    cost_micros: Option<i64>,
    completeness: Option<&str>,
) -> Result<Option<TrialCost>> {
    match (cost_micros, completeness) {
        (None, None) => Ok(None),
        (cost, Some(stored)) => {
            let completeness = parse_cost_completeness(stored)?;
            TrialCost::new(cost.map(MicroUsd::from_micros), completeness)
                .map(Some)
                .map_err(|e| LedgerError::Corrupt {
                    what: "trials.cost_micros/cost_completeness".into(),
                    detail: e.to_string(),
                })
        }
        (Some(cost), None) => Err(LedgerError::Corrupt {
            what: "trials.cost_completeness".into(),
            detail: format!("a cost of {cost} micros with no completeness beside it"),
        }),
    }
}

/// A `decisions` row as columns, parsed into a [`DecisionRecord`] (P6): an
/// unknown state or resolution is a `Corrupt` row the caller reports,
/// never a silently dropped or mismatched decision. `now` is the wait
/// clock for a row still open; a resolved row uses its own `resolved_at`
/// instead, so re-reading it later never changes what it says.
pub(super) fn parse_decision_row(row: DecisionRow, now: &str) -> Result<DecisionRecord> {
    let (
        run,
        task,
        raised_at,
        raised_state,
        raised_reason,
        resolved_at,
        resolution,
        actor,
        note,
        successor_run,
        candidate_contained,
    ) = row;
    let resolution = resolution
        .map(|stored| {
            Reason::parse(&stored).map_err(|unknown| LedgerError::Corrupt {
                what: unknown.what.into(),
                detail: unknown.to_string(),
            })
        })
        .transpose()?;
    let waited_seconds = seconds_between(&raised_at, resolved_at.as_deref().unwrap_or(now))?;
    Ok(DecisionRecord {
        run: RunId::from_stored(run),
        task: TaskId::from_stored(task),
        raised_at,
        raised_state: parse_state(&raised_state)?,
        raised_reason,
        resolved_at,
        resolution,
        actor,
        note,
        successor_run: successor_run.map(RunId::from_stored),
        candidate_contained,
        waited_seconds,
    })
}
