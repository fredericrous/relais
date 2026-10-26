//! Attempt lifecycle (SPEC §9, §19).
//!
//! States: prepared, running, verifying, repairing, escalating, accepted,
//! needs_review, needs_decision, blocked, failed, budget_exhausted,
//! cancelled, interrupted. Every transition carries a reason code,
//! timestamp and evidence references. Workers can propose completion or
//! blockage; only the runner assigns final state — a completion claim
//! never skips verification. Escalation uses a fresh context that labels
//! previous model claims unverified, never changes acceptance criteria,
//! and default ceilings are at most three attempts: initial, one repair,
//! one stronger attempt. A bounded planner may propose work packages
//! under explicit aggregate limits.
//!
//! The lifecycle's decisions live in `machine` as a pure function over
//! observations; this module is the interpreter: it observes (git,
//! SQLite, processes), asks the machine, records what it is told, and
//! performs the one effect each step names. A failure of the runner's
//! own machinery — a ledger that will not write, a filesystem that will
//! not — is a `RunError`, and ends the run as `interrupted` with the
//! error on record rather than as a panic with nothing on record.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::admission::{
    Decision, DispatchRequest, DispatchSource, Gate, GateError, Refusal, ReleaseWriteOutcome,
    ResourceClass, RunRegistration, WriteLeaseOutcome,
};
use crate::backend::{Backend, LaunchResult, LaunchSpec};
use crate::context::{self, ContextManifest, Guidance, GuidanceGap, GuidanceSource, RuleRef};
use crate::contract::{Review, TaskContract};
use crate::exhaustion;
use crate::ids::{DispatchId, PackageId, RunId};
use crate::ledger::{EvidenceKind, Ledger, LedgerError, Transition, UsageEvent};
use crate::lifecycle::UsagePhase;
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::policy::{
    BlockCode, BudgetClock, BudgetLimits, Budgets, EffectiveAuthority, EffortId, Exhausted,
    MachineSettings, RepoPolicy, Tier, VerificationProfile, WaitKind,
};
use crate::procs::Ended;
use crate::protocol::{AgentKind, Event, Events, Request, Summary, Wire};
use crate::route::{Recipe, RoutePredictor, RoutedBy, RungIndex};
use crate::verify::{self};
use crate::workspace::{self, TaskWorktree, WorkspaceError};

pub mod approval;
mod attempt;
mod baseline;
mod candidate;
mod config;
mod engine;
mod error;
mod evidence;
mod holdout_exposure;
mod holdouts;
mod launch;
pub mod live_trial;
pub mod machine;
mod preflight;
mod prompt;
mod review;
pub mod scheduler;
mod setup_gate;
mod spend;
pub mod successor;
mod types;
mod verification;
mod workspace_ops;

pub use crate::workspace::OutOfScope;
use baseline::{baseline_summary, setup_failure_detail};
pub use config::{holdout_root, verify_root, worktree_root, DesignSource, RunConfig};
use config::{retirement_detail, state_sibling};
use error::Finish;
pub use error::RunError;
pub use holdouts::sweep_ended_holdouts;
use launch::admission_block;
pub use machine::{
    decide, AttemptKind, Budget, HoldoutStop, Limit, Next, Observation, OverBudget, Terminal,
    WorktreeEnd,
};
use prompt::{build_prompt, build_review_repair_prompt, conflict_block, RepairFailure};
pub(crate) use prompt::{data_block, data_list_block, relevant_block};
use review::reviewer_guidance;
pub(crate) use review::{review_record_of, ReviewOutcome, ReviewRequest};
pub(crate) use spend::{effort_str, spend_limit, utc_day_start, Ceiling, RunSpend};
use spend::{fresh_progress, Progress};
pub use types::PackageRole;
use types::{
    cost_booked, dispatch_ended, harness_reading, split_profile_gaps, unpriced_native_model,
    AttemptContext, Attempted, Baseline, Candidate, CandidateConflicts, Dispatched, InPlaceRun,
    PendingEvidence, Preflight, PreparedCandidate, Snapshot, Step, Verified,
};
pub(crate) use types::{AttemptLabel, ManagedDispatch, Phase};

/// The lifecycle vocabulary lives in `crate::lifecycle`, a leaf module
/// the ledger, the report and the learning dataset can name without
/// depending on the runner. Re-exported here because a run's states and
/// reasons read as the runner's own from a caller's side.
pub use crate::lifecycle::{Reason, State};

/// One executed run's terminal result, carrying the run identity so the
/// CLI can point at the ledger and artifacts.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    pub run_id: RunId,
    pub terminal: Terminal,
}

impl RunOutcome {
    pub fn run_id(&self) -> &str {
        self.run_id.as_str()
    }

    pub fn state(&self) -> State {
        self.terminal.state()
    }

    pub fn detail(&self) -> &str {
        self.terminal.detail()
    }
}

/// Why a candidate could not be verified: the profile could not be carried
/// out as declared (blocked), or a command was never admitted.
pub(crate) enum VerifyFailure {
    Unavailable(String),
    Lease(verify::LeaseStop),
    /// A holdout store could not be fetched: the run ends blocked
    /// (`holdout_unfetchable`) with the candidate kept.
    HoldoutUnfetchable(String),
    /// relais's own write met a full disk: the run's to end on (SPEC §10),
    /// not a profile that could not be carried out.
    Full(RunError),
}

impl From<verify::VerifyError> for VerifyFailure {
    fn from(e: verify::VerifyError) -> Self {
        match e {
            verify::VerifyError::Lease(stop) => Self::Lease(stop),
            other if other.is_storage_full() => Self::Full(other.into()),
            other => Self::Unavailable(other.to_string()),
        }
    }
}

impl From<String> for VerifyFailure {
    fn from(detail: String) -> Self {
        Self::Unavailable(detail)
    }
}

impl From<WorkspaceError> for VerifyFailure {
    fn from(e: WorkspaceError) -> Self {
        if e.is_storage_full() {
            return Self::Full(e.into());
        }
        Self::Unavailable(e.to_string())
    }
}

/// Poll period while queued for admission. `pub(crate)`: `hook::respond`
/// polls a queued spawn at the same cadence, reusing this constant rather
/// than inventing a second one beside it. It lives with the coordinator's
/// types (`admission`), so the native backend polls at the same cadence
/// without depending on the runner.
pub(crate) use crate::admission::ADMISSION_POLL;

/// How many consecutive unanswered heartbeats make a coordinator
/// unreachable rather than slow (R6). Cancellation travels on the
/// heartbeat, so a run that has missed this many is no longer
/// supervised and ends rather than carrying on unheard.
const HEARTBEAT_FAILURES_ALLOWED: u32 = 3;

/// The supervised execution path (SPEC §3): preflight, route, then a
/// bounded sequence of attempts the runner — not a model — owns.
pub fn execute(config: &RunConfig<'_>) -> Result<RunOutcome, RunError> {
    let engine = RunEngine::new(config, None)?;
    let run_id = engine.run_id.clone();
    match engine.run() {
        Ok(outcome) => Ok(finished(config, outcome)),
        Err(error) => {
            // A run that could not be recorded is over all the same: the
            // coordinator must not keep its seat.
            if let Some(gate) = config.gate {
                let _ = gate.finish_run(run_id.as_str());
            }
            Err(error)
        }
    }
}

/// A work package's run (SPEC §19): the same lifecycle, attributed to
/// its root run in the ledger.
pub fn execute_child(
    config: &RunConfig<'_>,
    parent_run: &RunId,
    package_id: &PackageId,
    role: PackageRole,
) -> Result<RunOutcome, RunError> {
    let outcome =
        RunEngine::new(config, Some((parent_run.clone(), package_id.clone(), role)))?.run()?;
    Ok(finished(config, outcome))
}

/// Tell the coordinator the run is over, whatever its end: a registered
/// run keeps the daemon from idle-exiting (SPEC §23), so one that has
/// ended must say so. Best effort — the outcome is already decided and
/// recorded, and an unreachable coordinator reaps it on its own grace.
fn finished(config: &RunConfig<'_>, outcome: RunOutcome) -> RunOutcome {
    if let Some(gate) = config.gate {
        let _ = gate.finish_run(outcome.run_id());
    }
    outcome
}

/// What a managed launch produced: the worker's result, or the run's
/// end when admission or the launch itself decided it.
pub(crate) type Launched = Result<LaunchResult, RunOutcome>;

pub(crate) struct RunEngine<'a> {
    pub(crate) config: &'a RunConfig<'a>,
    pub(crate) run_id: RunId,
    pub(crate) artifacts: PathBuf,
    /// This run's worktrees, outside the artifact directory (B6): the
    /// task worktree the worker gets, or the scheduler's integration
    /// worktree — never both, since a decomposed run has no task
    /// worktree of its own.
    pub(crate) worktrees: PathBuf,
    /// This run's throwaway verification worktrees.
    pub(crate) verify_dir: PathBuf,
    pub(crate) state: State,
    pub(in crate::runner) parent: Option<(RunId, PackageId, PackageRole)>,
    /// `<backend> <version>` as probed at run start; unknown = `None`.
    pub(crate) harness: Option<String>,
    /// Write leases this run's own dispatches took and could not give
    /// back (R3). Verification never waits on one of these: the process
    /// that held it has ended, so the tree is still, and waiting would
    /// be the run waiting on itself until its own deadline.
    pub(in crate::runner) own_write_leases: std::collections::BTreeSet<String>,
    /// The reviewer's constraint statements that did not fit its prompt:
    /// known only once the review prompt is built, shown in the receipt.
    pub(in crate::runner) reviewer_gap: Option<GuidanceGap>,
    /// adr-lint's candidates for the candidate commit under review, what was
    /// put to the reviewer and what it ruled: fetched once per commit.
    pub(in crate::runner) candidates_run: Option<review::CandidatesRun>,
    /// The worker's full-text code rules that did not fit its prompt, or
    /// whose text was unavailable: known once the attempt's prompt is built.
    pub(in crate::runner) worker_code_rules_gap: Option<GuidanceGap>,
    /// Set once the coordinator reports this run cancelled while no
    /// dispatch heartbeat was running (a check, or between phases).
    pub(in crate::runner) cancel_asked: Arc<AtomicBool>,
    /// What this run may still spend of time (ADR-0010). Unbounded until
    /// preflight has the authority that sizes it.
    pub(crate) budgets: Budgets,
    /// Which try of each command's heavy lease this is, so a rerun gets a
    /// lease id the coordinator has not seen (SPEC §23, C8).
    pub(in crate::runner) heavy_tries: verify::Tries,
    /// What this run registered with the coordinator, kept to register
    /// again when a restarted coordinator no longer knows the run.
    pub(in crate::runner) registration: Option<RunRegistration>,
    /// Everything this run does, as events (SPEC §29).
    pub(crate) events: Events,
    /// The caller's worktree this run holds when it works in place
    /// (ADR-0008); `None` for every other workspace.
    pub(in crate::runner) inplace: Option<InPlaceRun>,
    /// The newest candidate snapshot this run made, `(attempt, commit)`:
    /// what the storage-full path names a ref for when the disk fills
    /// after the snapshot exists (SPEC §10).
    pub(in crate::runner) last_snapshot: Option<(u32, String)>,
}

#[cfg(test)]
mod tests;
