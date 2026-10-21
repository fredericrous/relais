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

mod attempt;
mod baseline;
mod candidate;
mod config;
mod engine;
mod error;
mod evidence;
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
use config::{retirement_detail, state_sibling};
pub use config::{verify_root, worktree_root, DesignSource, RunConfig};
pub(crate) use engine::{Launched, RunEngine};
use error::Finish;
pub use error::RunError;
use launch::admission_block;
pub use machine::{decide, AttemptKind, Budget, Limit, Next, Observation, Terminal, WorktreeEnd};
use prompt::{build_prompt, conflict_block, reviewer_guidance};
pub(crate) use prompt::{data_block, data_list_block, relevant_block};
use review::REVIEW_PATCH_BUDGET_BYTES;
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

#[cfg(test)]
mod tests;
