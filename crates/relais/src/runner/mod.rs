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
use crate::context::{
    self, ContextError, ContextManifest, Guidance, GuidanceGap, GuidanceSource, RuleRef,
};
use crate::contract::{Review, TaskContract};
use crate::exhaustion;
use crate::ids::{derive_task_id, DispatchId, PackageId, RunId};
use crate::ledger::{EvidenceKind, Ledger, LedgerError, Transition, UsageEvent};
use crate::lifecycle::UsagePhase;
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::policy::{
    effective_authority, repo_key, BlockCode, BudgetClock, BudgetLimits, Budgets,
    EffectiveAuthority, EffortId, Exhausted, MachineSettings, RepoPolicy, Tier,
    VerificationProfile, WaitKind,
};
use crate::procs::Ended;
use crate::protocol::{AgentKind, Event, Events, Request, Summary, Wire};
use crate::route::{route, Recipe, RouteInputs, RoutePredictor, Routed, RoutedBy, RungIndex};
use crate::verify::{self};
use crate::workspace::{self, TaskWorktree, WorkspaceError};

mod candidate;
mod config;
mod error;
mod evidence;
pub mod live_trial;
pub mod machine;
mod prompt;
mod review;
pub mod scheduler;
mod setup_gate;
mod spend;
pub mod successor;
mod types;
mod verification;

pub use crate::workspace::OutOfScope;
use config::{retirement_detail, state_sibling};
pub use config::{verify_root, worktree_root, DesignSource, RunConfig};
use error::Finish;
pub use error::RunError;
pub use machine::{decide, AttemptKind, Budget, Limit, Next, Observation, Terminal, WorktreeEnd};
use prompt::{build_prompt, conflict_block, reviewer_guidance};
pub(crate) use prompt::{data_block, data_list_block, relevant_block};
use review::{read_patch, report_review_prompt, REVIEW_MIN_WALL, REVIEW_PATCH_BUDGET_BYTES};
pub(crate) use review::{
    review_record_of, review_verdict, reviewer_tier, ReviewOutcome, ReviewRequest,
};
use review::{ReviewPurpose, ReviewSeat};
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

/// How many times a finished dispatch asks for its write lease to be
/// taken back before the failure goes on the record (R3).
const RELEASE_WRITE_TRIES: u32 = 2;

/// How a failed admission call is reported. An unreachable coordinator
/// is an outage — the request is preserved and nothing was launched — and
/// anything else is a decision the coordinator made and answered with.
/// Reporting a refusal as `admission_unavailable` sent the operator
/// looking for a dead daemon that was answering perfectly well (A12).
fn admission_block(error: &GateError) -> (Reason, BlockCode) {
    if error.unavailable() {
        (
            Reason::AdmissionUnavailable,
            BlockCode::AdmissionUnavailable,
        )
    } else {
        (Reason::AdmissionRefused, BlockCode::AdmissionRefused)
    }
}

/// Record the first reason this dispatch lost its seat. The first is the
/// one that explains the rest, and a poisoned slot is recovered rather
/// than turning a lost seat into a second panic.
fn record_lost_seat(slot: &std::sync::Mutex<Option<String>>, detail: String) {
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    slot.get_or_insert(detail);
}

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

impl<'a> RunEngine<'a> {
    /// The phase an attempt of THIS run records: a root run and an
    /// ordinary package say which attempt it is; every attempt of an
    /// integration repair is integration, because that is what the
    /// money bought.
    fn attempt_phase(&self, kind: AttemptKind) -> UsagePhase {
        self.parent
            .as_ref()
            .map_or(UsagePhase::from(kind), |(_, _, role)| role.phase(kind))
    }

    fn new(
        config: &'a RunConfig<'a>,
        parent: Option<(RunId, PackageId, PackageRole)>,
    ) -> Result<Self, RunError> {
        let run_id = match &config.run_id {
            Some(run_id) if parent.is_none() => run_id.clone(),
            Some(_) | None => config.ids.run_id()?,
        };
        // How often this run's runner has started: a resumed run counts on
        // from there, so its heavy leases never reuse a settled id.
        let epoch = config.ledger.next_run_epoch(run_id.as_str())?;
        let artifacts = config.artifacts_dir.join(run_id.as_str());
        let worktrees = worktree_root(&config.artifacts_dir).join(run_id.as_str());
        let verify_dir = verify_root(&config.artifacts_dir).join(run_id.as_str());
        let events = Events::for_run_on(run_id.as_str(), &artifacts, config.wire.clone());
        if parent.is_none() {
            // The process's own stderr belongs to the root run.
            events.mirror_stderr();
        }
        Ok(Self {
            config,
            run_id,
            artifacts,
            worktrees,
            verify_dir,
            state: State::Prepared,
            parent,
            harness: None,
            own_write_leases: std::collections::BTreeSet::new(),
            inplace: None,
            last_snapshot: None,
            reviewer_gap: None,
            cancel_asked: Arc::new(AtomicBool::new(false)),
            budgets: Budgets::unbounded(),
            heavy_tries: verify::Tries::for_epoch(epoch),
            registration: None,
            events,
        })
    }

    /// Record a transition. The ledger is the record; a transition it
    /// cannot write is a runner failure, not something to carry on past.
    pub(crate) fn transition(
        &mut self,
        to: State,
        reason: Reason,
        detail: serde_json::Value,
    ) -> Result<(), RunError> {
        let from = self.state;
        let wrote = self.config.ledger.transition_if(
            from,
            &Transition {
                run_id: self.run_id.clone(),
                attempt_id: None,
                from_state: Some(from),
                to_state: to,
                reason: reason.as_str().to_string(),
                detail: Some(detail.clone()),
                at: self.config.ledger.now(),
            },
        )?;
        if !wrote {
            return Err(RunError::Superseded(self.run_id.clone()));
        }
        self.state = to;
        self.events.emit(Event::Phase {
            state: to.as_str().to_string(),
            reason: reason.as_str().to_string(),
            detail,
        });
        Ok(())
    }

    /// Run `work` while a watcher asks the coordinator, every
    /// `heartbeat_every`, whether this run is cancelled: a cancel reaches
    /// a dispatch on its heartbeat, but a verification check has none. The
    /// flag handed to `work` turns true on a cancel and stays true.
    fn while_watching_cancel<T>(&self, work: impl FnOnce(Option<&AtomicBool>) -> T) -> T {
        let Some(gate) = self.config.gate else {
            return work(None);
        };
        let done = AtomicBool::new(false);
        let every = self.config.heartbeat_every;
        let (run_id, asked) = (self.run_id.as_str(), &*self.cancel_asked);
        std::thread::scope(|scope| {
            let done = &done;
            scope.spawn(move || {
                let mut last: Option<Instant> = None;
                while !done.load(Ordering::SeqCst) {
                    if last.is_none_or(|last| last.elapsed() >= every) {
                        last = Some(Instant::now());
                        if gate.run_cancelled(run_id).unwrap_or(false) {
                            asked.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let result = work(Some(asked));
            done.store(true, Ordering::SeqCst);
            result
        })
    }

    /// Whether the coordinator has cancelled this run: the watcher saw it,
    /// or the gate says so now.
    fn cancel_requested(&self) -> bool {
        self.cancel_asked.load(Ordering::SeqCst)
            || self
                .config
                .gate
                .is_some_and(|gate| gate.run_cancelled(self.run_id.as_str()).unwrap_or(false))
    }

    /// Between phases: a run cancelled at the coordinator ends cancelled
    /// here, through the same transition as any other end.
    fn end_if_cancelled(&mut self) -> Result<Option<RunOutcome>, RunError> {
        if !self.cancel_requested() {
            return Ok(None);
        }
        self.cancel_asked.store(true, Ordering::SeqCst);
        let detail =
            "the run was cancelled through the coordinator; its evidence is kept".to_string();
        Ok(Some(self.finish(
            Reason::CancelledByUser,
            serde_json::json!({ "detail": detail }),
            Terminal::Cancelled { detail },
        )?))
    }

    /// Record a terminal transition and end the run with it.
    pub(crate) fn finish(
        &mut self,
        reason: Reason,
        detail: serde_json::Value,
        terminal: Terminal,
    ) -> Result<RunOutcome, RunError> {
        self.transition(terminal.state(), reason, detail)?;
        Ok(RunOutcome {
            run_id: self.run_id.clone(),
            terminal,
        })
    }

    /// Ask the machine, record what it decided, hand back the step.
    pub(crate) fn decide(
        &mut self,
        budget: &Budget,
        observation: Observation,
    ) -> Result<Next, RunError> {
        let decision = decide(budget, observation);
        let what = match &decision.next {
            Next::Attempt { kind, .. } => kind.as_str(),
            Next::Accept => "accept",
            Next::Stop(_) => "stop",
        };
        self.events.emit(Event::Decision {
            what: what.to_string(),
            reason: decision.reason.as_str().to_string(),
        });
        self.transition(decision.state, decision.reason, decision.detail)?;
        Ok(decision.next)
    }

    pub(crate) fn fail_preflight(
        &mut self,
        code: BlockCode,
        detail: String,
    ) -> Result<RunOutcome, RunError> {
        self.block(Reason::BlockedPreflight, code, detail)
    }

    pub(crate) fn block(
        &mut self,
        reason: Reason,
        code: BlockCode,
        detail: String,
    ) -> Result<RunOutcome, RunError> {
        self.finish(
            reason,
            serde_json::json!({ "code": code, "detail": detail }),
            Terminal::Blocked { code, detail },
        )
    }

    /// Marks a dispatch row `native_run`, with the agent once one is bound:
    /// from the moment the row exists, so it never disagrees with what the
    /// coordinator is told.
    pub(crate) fn record_native_row(
        &self,
        dispatch_id: &DispatchId,
        agent_id: Option<&str>,
    ) -> Result<(), LedgerError> {
        self.config
            .ledger
            .record_native_dispatch(dispatch_id, agent_id)
    }

    /// What a dispatch's usage is: its cost is always tokens times the
    /// price table, known or not, never API spend somebody reported.
    pub(crate) fn usage_cost_kind(&self) -> CostKind {
        CostKind::EstimatedApiEquivalent
    }

    /// Books a dispatch's usage. A native dispatch's goes in with its
    /// agent's message ids, so an older relais's usage import skips them;
    /// without the ids the usage is booked anyway and the run says so.
    pub(crate) fn book_usage(
        &self,
        event: &UsageEvent,
        result: &LaunchResult,
    ) -> Result<(), LedgerError> {
        // Only what the dispatch's per-turn bookings have not already
        // booked: `native usage` may have booked most of it.
        let booked = self.config.ledger.record_usage_delta(
            event,
            &result.booked_message_ids,
            crate::ledger::Booking::Final,
        )?;
        if let Some(booked) = booked {
            self.events.emit(cost_booked(&booked, &result.unpriced));
        }
        if result.session_id.is_some() && result.booked_message_ids.is_empty() {
            self.events.emit(Event::Decision {
                what: "rollback_ids_missing".into(),
                reason: event.event_id.clone(),
            });
        }
        Ok(())
    }

    /// Managed dispatch (SPEC §23): admission before launch, heartbeats
    /// during, release and settlement after. A coordinator outage blocks
    /// the launch rather than making it unmanaged; a queue wait counts
    /// against the run's wall clock; refusals on budget, depth or the
    /// aggregate agent cap are budget exhaustion, and a run cancelled
    /// while queued or running ends cancelled with its evidence kept.
    ///
    /// `ManagedDispatch::write_lease` names the worktree this dispatch
    /// will write, when it writes one: the lease is taken after
    /// admission and before the process exists, and given back when the
    /// process has ended. A worktree somebody else is writing refuses
    /// the launch outright — two writers in one tree is the one thing
    /// scope checks cannot catch (SPEC §23: "scope checks alone are not
    /// filesystem isolation"). Readers — the reviewer, the planner —
    /// pass `None`.
    pub(crate) fn managed_launch(
        &mut self,
        dispatch: ManagedDispatch<'_>,
    ) -> Result<Launched, RunError> {
        let dispatch_id = dispatch.spec.dispatch_id.clone();
        self.events.emit(Event::DispatchStarted {
            dispatch: dispatch_id.clone(),
            agent_kind: dispatch.spec.agent,
            attempt: dispatch.attempt,
            model: dispatch.spec.model.clone(),
            effort: effort_str(dispatch.spec.effort.as_ref()),
        });
        let launched = self.launch_admitted(dispatch)?;
        self.events.emit(dispatch_ended(dispatch_id, &launched));
        Ok(launched)
    }

    fn launch_admitted(&mut self, dispatch: ManagedDispatch<'_>) -> Result<Launched, RunError> {
        let ManagedDispatch {
            mut spec,
            depth,
            parent,
            reserve_micros,
            budget,
            write_lease,
            ..
        } = dispatch;
        // The dispatch is `launched` in the ledger BEFORE the process
        // exists (SPEC §12): a runner crash from here on leaves a live
        // dispatch for `resume` to reconcile, never a silent gap.
        self.config.ledger.attach_dispatch_process(
            &DispatchId::from_stored(spec.dispatch_id.clone()),
            None,
            Some(&self.config.session_id),
        )?;
        let Some(gate) = self.config.gate else {
            return Ok(match self.config.backend.launch(&spec) {
                Ok(result) => Ok(result),
                Err(e) => {
                    Err(self.block(Reason::BlockedPreflight, e.block_code(), e.to_string())?)
                }
            });
        };
        let request = DispatchRequest {
            disk: None,
            dispatch_id: spec.dispatch_id.clone(),
            run_id: self.run_id.as_str().to_string(),
            session_id: self.config.session_id.clone(),
            parent_dispatch: parent.map(str::to_string),
            depth,
            resource: ResourceClass::ModelWork,
            reserve_micros,
            source: DispatchSource::NativeRun,
            // A managed dispatch self-reports its parent through
            // `parent_dispatch` above; `caller_agent_id` exists only for
            // the hook path, which has a caller's agent id and no
            // dispatch id of its own to report.
            caller_agent_id: None,
            weight: 1,
        };
        // Time spent queued is charged to `admission_wait_seconds` and to
        // the total, never to the work budget (ADR-0010).
        let mut queued = self.budgets.wait(WaitKind::Admission);
        loop {
            match gate.admit(&request) {
                Err(e) => {
                    let (reason, code) = admission_block(&e);
                    return Ok(Err(self.block(
                        reason,
                        code,
                        format!("{e}; the request is preserved and nothing was launched"),
                    )?));
                }
                Ok(Decision::Granted) => break,
                Ok(Decision::AlreadyAdmitted) => {
                    // Somebody already holds this ID: launching would
                    // duplicate an agent. Stop and let resume reconcile.
                    return Ok(Err(self.finish(
                        Reason::DuplicateDispatch,
                        serde_json::json!({ "dispatch_id": spec.dispatch_id }),
                        Terminal::Interrupted {
                            detail: format!(
                                "dispatch {} was already admitted elsewhere; not launching a duplicate",
                                spec.dispatch_id
                            ),
                        },
                    )?));
                }
                Ok(decision @ (Decision::Queued { .. } | Decision::WaitingForDisk { .. })) => {
                    // Model work asks nothing of the disk, so the second
                    // form is not expected; it waits like a queue.
                    let position = match decision {
                        Decision::Queued { position } => position,
                        _ => 0,
                    };
                    if let Some(exhausted) = queued.tick() {
                        // Best effort: nothing was launched, and a
                        // request the coordinator still holds expires
                        // with the queue entry it never granted.
                        let _ = gate.withdraw(&spec.dispatch_id);
                        self.events.emit(Event::Decision {
                            what: "admission_queue_ended".into(),
                            reason: format!("still queued at position {position}"),
                        });
                        return Ok(Err(self.end_on_exhausted(exhausted, budget)?));
                    }
                    std::thread::sleep(ADMISSION_POLL);
                }
                Ok(Decision::Refused { code, detail }) => {
                    return Ok(Err(match code {
                        Refusal::RunCancelled => {
                            self.stop(budget, Observation::Cancelled(detail))?
                        }
                        Refusal::BudgetExceeded | Refusal::RunAgentCap | Refusal::DepthExceeded => {
                            self.stop(
                                budget,
                                Observation::LimitReached(Limit::Admission {
                                    code: code.as_str().to_string(),
                                    detail,
                                }),
                            )?
                        }
                        Refusal::UnknownRun | Refusal::WeightOutOfRange => self.block(
                            Reason::AdmissionRefused,
                            BlockCode::AdmissionRefused,
                            detail,
                        )?,
                        // C8: this dispatch ID already ran and settled.
                        // Launching it again duplicates an agent, so it
                        // ends exactly where `AlreadyAdmitted` does.
                        Refusal::AlreadyFinished => self.finish(
                            Reason::DuplicateDispatch,
                            serde_json::json!({ "dispatch_id": spec.dispatch_id }),
                            Terminal::Interrupted { detail },
                        )?,
                    }));
                }
            }
        }

        drop(queued);
        let lease_key = write_lease.map(|path| path.to_string_lossy().into_owned());
        if let Some(key) = lease_key.as_deref() {
            match gate.acquire_write(&spec.dispatch_id, key) {
                Ok(WriteLeaseOutcome::Taken) => {
                    self.own_write_leases.insert(spec.dispatch_id.clone());
                }
                Ok(WriteLeaseOutcome::HeldBy { holder }) => {
                    // Best effort: the seat is given back so the refusal
                    // does not hold one, and an unwithdrawn request
                    // expires on its own.
                    let _ = gate.withdraw(&spec.dispatch_id);
                    return Ok(Err(self.block(
                        Reason::AdmissionRefused,
                        BlockCode::AdmissionRefused,
                        format!(
                            "worktree {key} is being written by {holder}; a second writer is \
                             never launched into one tree (SPEC §23)"
                        ),
                    )?));
                }
                Err(e) => {
                    // Best effort, and to the same coordinator that just
                    // failed: the run is ending on that failure anyway.
                    let _ = gate.withdraw(&spec.dispatch_id);
                    let (reason, code) = admission_block(&e);
                    return Ok(Err(self.block(
                        reason,
                        code,
                        format!("{e}; the write lease could not be taken and nothing was launched"),
                    )?));
                }
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        spec.cancel = Some(Arc::clone(&cancel));
        let stop = AtomicBool::new(false);
        let heartbeat_every = self.config.heartbeat_every;
        // Cancellation reaches a worker only on the heartbeat (SPEC §23).
        // A coordinator that stops answering therefore makes `relais
        // cancel` a no-op, silently, for as long as the worker runs —
        // so the errors are counted and the run ends on the record (R6).
        let heartbeat_lost: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
        let launched = std::thread::scope(|scope| {
            let dispatch_id = spec.dispatch_id.clone();
            let cancel = Arc::clone(&cancel);
            let stop = &stop;
            let heartbeat_lost = &heartbeat_lost;
            let events = &self.events;
            scope.spawn(move || {
                let mut last: Option<Instant> = None;
                let mut last_relay = Instant::now();
                let mut heartbeat_errors: u32 = 0;
                while !stop.load(Ordering::SeqCst) {
                    if last.is_none_or(|last| last.elapsed() >= heartbeat_every) {
                        last = Some(Instant::now());
                        match gate.heartbeat(&dispatch_id) {
                            Ok(status) => {
                                // One answer clears the count: a single
                                // dropped call is a blip, not an outage.
                                heartbeat_errors = 0;
                                if status.cancelled {
                                    cancel.store(true, Ordering::SeqCst);
                                    // The seat and the reservation go
                                    // now, not at lease grace (C5).
                                    // Best effort: the worker is already
                                    // stopping and the coordinator reaps
                                    // the seat on its own lease grace.
                                    let _ = gate.acknowledge_cancel(&dispatch_id);
                                }
                            }
                            Err(e) => {
                                heartbeat_errors += 1;
                                if heartbeat_errors >= HEARTBEAT_FAILURES_ALLOWED {
                                    record_lost_seat(
                                        heartbeat_lost,
                                        format!(
                                            "{HEARTBEAT_FAILURES_ALLOWED} consecutive heartbeats \
                                             for dispatch {dispatch_id} went unanswered ({e})"
                                        ),
                                    );
                                }
                            }
                        }
                    }
                    // Usage the worker reports through `relais native
                    // usage` reaches the sequenced stream while it runs.
                    if last_relay.elapsed() >= Duration::from_millis(200) {
                        last_relay = Instant::now();
                        events.relay_usage();
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            });
            let result = self.config.backend.launch(&spec);
            stop.store(true, Ordering::SeqCst);
            result
        });
        // The last report may have landed after the final tick.
        self.events.relay_usage();

        // The process has ended: whatever it wrote is written. The lease
        // goes back before the seat, so verification never waits on a
        // writer that is already gone (SPEC §23).
        if let Some(key) = lease_key.as_deref() {
            self.release_write_lease(gate, &spec.dispatch_id, key)?;
        }
        let result = match launched {
            Ok(result) => result,
            Err(e) => {
                // Best effort: the launch never produced a process, and
                // the coordinator reaps an unbound seat and its
                // reservation on its own lease grace.
                let _ = gate.release(&spec.dispatch_id);
                let _ = gate.settle(&spec.dispatch_id, None);
                return Ok(Err(self.block(
                    Reason::BlockedPreflight,
                    e.block_code(),
                    e.to_string(),
                )?));
            }
        };
        // Bind, free the seat, settle the reservation: unknown usage
        // settles as unknown, never zero (SPEC §23). Failures here are
        // the coordinator's to reconcile; the work already happened and
        // an agent ID it cannot attach costs nothing.
        let _ = gate.bind(&spec.dispatch_id, result.session_id.as_deref(), None);
        let _ = gate.release(&spec.dispatch_id);
        let spent = result.usage.cost.micros().map(MicroUsd::to_micros);
        let _ = gate.settle(&spec.dispatch_id, spent);
        // The worker ran unheard: whatever it produced is on disk and in
        // the ledger, but nothing could have stopped it, so the run ends
        // interrupted rather than judging a candidate it could not
        // supervise (R6).
        if let Some(detail) = heartbeat_lost
            .into_inner()
            .unwrap_or_else(|e| e.into_inner())
        {
            return Ok(Err(self.finish(
                Reason::CoordinatorUnreachable,
                serde_json::json!({
                    "dispatch_id": spec.dispatch_id,
                    "detail": detail,
                }),
                Terminal::Interrupted {
                    detail: format!(
                        "{detail}; a cancellation could not have reached the worker, so the run \
                         is not supervised and its evidence is preserved"
                    ),
                },
            )?));
        }
        Ok(Ok(result))
    }

    /// Give a worktree's write lease back when the process that held it
    /// has ended.
    ///
    /// A discarded failure here is not cosmetic: leases have no expiry
    /// (`admission`), so this run's own dispatch stays on record as the
    /// writer and `wait_for_writers` waits for it until the run's wall
    /// clock runs out — an acceptable candidate ending `interrupted`
    /// because of one dropped socket call (R3). One retry, then the
    /// failure goes on the record and the lease is remembered as this
    /// run's own so verification does not wait on itself.
    fn release_write_lease(
        &mut self,
        gate: &(dyn Gate + Sync),
        dispatch_id: &str,
        key: &str,
    ) -> Result<(), RunError> {
        let mut failure = None;
        for _ in 0..RELEASE_WRITE_TRIES {
            match gate.release_write(dispatch_id, key) {
                // Released, or somebody else holds the tree (or nobody
                // does): our lease is not there to give back either
                // way, and a real holder is exactly what verification
                // must wait for.
                Ok(ReleaseWriteOutcome::Released | ReleaseWriteOutcome::NotTheHolder) => {
                    self.own_write_leases.remove(dispatch_id);
                    return Ok(());
                }
                Err(e) => failure = Some(e),
            }
        }
        let detail = failure.map_or_else(
            || "the write lease could not be released".to_string(),
            |e| e.to_string(),
        );
        self.transition(
            self.state,
            Reason::WriteLeaseNotReleased,
            serde_json::json!({
                "worktree": key,
                "dispatch_id": dispatch_id,
                "error": detail,
                "detail": "the lease is this run's own and its process has ended, so \
                           verification treats the tree as still",
            }),
        )
    }

    /// Observe something terminal, record the machine's decision and end
    /// the run. For observations the table can only ever stop on.
    pub(crate) fn stop(
        &mut self,
        budget: &Budget,
        observation: Observation,
    ) -> Result<RunOutcome, RunError> {
        match self.decide(budget, observation)? {
            Next::Stop(terminal) => Ok(RunOutcome {
                run_id: self.run_id.clone(),
                terminal,
            }),
            other => Err(RunError::Other(format!(
                "the machine answered {other:?} to a terminal observation"
            ))),
        }
    }

    /// Take the caller's worktree for this run and snapshot it (ADR-0008).
    /// The snapshot, not `HEAD`, becomes the base of every diff, scope
    /// check and verification tree, so uncommitted work the person had
    /// is neither the worker's nor lost. A refusal carries its block code.
    fn enter_in_place(&mut self, base_sha: String) -> Result<String, (BlockCode, String)> {
        use crate::workspace::inplace::{start_snapshot, Lease, LeaseRefusal};
        let lease = Lease::acquire(self.config.repo_dir, self.run_id.as_str()).map_err(|e| match e {
            LeaseRefusal::Busy { holder } => (
                BlockCode::WorkspaceBusy,
                format!(
                    "run {} holds this worktree; wait for its outcome (relais status {})",
                    holder.as_deref().unwrap_or("?"),
                    holder.as_deref().unwrap_or("<run>")
                ),
            ),
            LeaseRefusal::Interrupted { run } => (
                BlockCode::WorkspaceHasInterruptedRun,
                format!(
                    "run {run} was interrupted here with edits applied; run: relais resume --retire {run}"
                ),
            ),
            LeaseRefusal::UnreadableMarker { path, why } => (
                BlockCode::WorkspaceHasInterruptedRun,
                format!(
                    "a run died in this worktree and left {}, which cannot be read ({why}); look at the tree, and delete that file once you have",
                    path.display()
                ),
            ),
            LeaseRefusal::Io(e) => (BlockCode::WorktreeUnavailable, e.to_string()),
        })?;
        let start = start_snapshot(&lease).map_err(|e| {
            (
                BlockCode::WorktreeUnavailable,
                format!("the start snapshot failed: {e}"),
            )
        })?;
        if start.head_sha != base_sha {
            return Err((
                BlockCode::BaseUnresolvable,
                format!(
                    "the contract's base resolves to {} but this worktree is at {}; an in-place run works from the worktree's own HEAD",
                    &base_sha[..base_sha.len().min(12)],
                    &start.head_sha[..start.head_sha.len().min(12)]
                ),
            ));
        }
        self.events.emit(Event::Decision {
            what: "workspace_in_place".into(),
            reason: format!(
                "in place in {}: do not edit it or stage anything in it until the outcome arrives (lease held){}",
                self.config.repo_dir.display(),
                if start.dirty.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; the start snapshot includes {} uncommitted path(s)",
                        start.dirty.len()
                    )
                }
            ),
        });
        let sha = start.sha.clone();
        self.inplace = Some(InPlaceRun {
            lease: Some(lease),
            start,
        });
        Ok(sha)
    }

    /// The run's worker tree when it works in place: the caller's own
    /// worktree, against a private index. The marker is written here,
    /// before anything can edit the tree, so a run that dies from now on
    /// is found and finished by `resume --retire`.
    fn take_in_place_worktree(&mut self) -> Option<Result<TaskWorktree, String>> {
        use crate::workspace::inplace::{Marker, Stage};
        let state = self.inplace.as_ref()?;
        let lease = state.lease.as_ref()?;
        let start = &state.start;
        let marker = Marker {
            run: self.run_id.as_str().to_string(),
            stage: Stage::Running,
            head_symbolic: start.head_symbolic.clone(),
            head_sha: start.head_sha.clone(),
            start: start.sha.clone(),
            index_listing: start.index_listing.clone(),
            target: None,
            exported: None,
        };
        Some(
            lease
                .write_marker(&marker)
                .map(|()| TaskWorktree {
                    path: self.config.repo_dir.to_path_buf(),
                    base_sha: start.sha.clone(),
                    index: Some(lease.relais_index()),
                })
                .map_err(|e| format!("the in-place marker could not be written: {e}")),
        )
    }

    /// The end of an in-place run: decide what the worktree must be (the
    /// accepted change, or how it was found), then export what no
    /// candidate holds, apply it, and release the worktree. The directory
    /// is never removed. An interrupted run keeps its marker and its
    /// edits for `relais resume --retire`.
    fn finish_in_place(
        &mut self,
        worktree: &TaskWorktree,
        outcome: &RunOutcome,
    ) -> Result<(), RunError> {
        use crate::workspace::inplace::{
            finalize, ApplyError, IndexOutcome, IndexPolicy, Outcome, Target, TargetKind,
            Unrestored,
        };
        let Some(lease) = self.inplace.as_mut().and_then(|state| state.lease.take()) else {
            return Ok(());
        };
        if matches!(outcome.terminal.worktree_end(), WorktreeEnd::Keep) {
            self.events.emit(Event::Stderr {
                text: format!(
                    "worktree {} kept with the run's edits in it; `relais resume --retire {}` finishes it",
                    worktree.path.display(),
                    self.run_id
                ),
            });
            return Ok(());
        }
        let tree_of = |rev: &str| {
            self.config
                .git
                .run(&worktree.path, &["rev-parse", &format!("{rev}^{{tree}}")])
                .map(|tree| tree.trim().to_string())
        };
        let target = match &outcome.terminal {
            Terminal::Accepted(receipt) => tree_of(&receipt.candidate_sha).map(|tree| Target {
                kind: TargetKind::Accepted,
                tree,
            }),
            _ => tree_of(&worktree.base_sha).map(|tree| Target {
                kind: TargetKind::Start,
                tree,
            }),
        };
        let target = match target {
            Ok(target) => target,
            Err(e) => {
                return self.transition(
                    self.state,
                    Reason::WorktreeNotReleased,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "error": e.to_string(),
                        "detail": "the tree the worktree must end as could not be read; it is as the run left it",
                    }),
                );
            }
        };
        let kind = target.kind;
        let artifacts = self.artifacts.clone();
        let run_id = self.run_id.as_str().to_string();
        let mut export = || {
            workspace::export_unkept(worktree, &run_id, &artifacts)
                .map(|exported| {
                    exported
                        .map(|exported| exported.reference)
                        .unwrap_or_else(|| "none".to_string())
                })
                .map_err(std::io::Error::other)
        };
        match finalize(lease, target, IndexPolicy::Restore, &mut export) {
            Ok(Outcome::Applied { index }) => {
                self.say_index(&index, worktree);
                self.transition(
                    self.state,
                    Reason::WorktreeRetired,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "in_place": true,
                        "index": match &index {
                            IndexOutcome::Untouched => "untouched",
                            IndexOutcome::Restored => "restored",
                            IndexOutcome::Changed(_) => "changed",
                        },
                        "restore_error": match &index {
                            IndexOutcome::Changed(Unrestored::Failed(why)) => Some(why.as_str()),
                            _ => None,
                        },
                        "left": match kind {
                            TargetKind::Accepted => "the accepted change",
                            TargetKind::Start => "the tree as it was found",
                        },
                        "detail": "the directory is kept",
                    }),
                )
            }
            Ok(Outcome::HeadMoved { was, now }) => {
                self.events.emit(Event::Stderr {
                    text: format!(
                        "HEAD moved from {} to {} during the run; relais did not touch the branch, review it yourself",
                        &was[..was.len().min(12)],
                        &now[..now.len().min(12)]
                    ),
                });
                self.transition(
                    self.state,
                    Reason::WorktreeNotReleased,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "code": BlockCode::InPlaceHeadMoved.as_str(),
                        "was": was,
                        "now": now,
                        "detail": "a worker committed on the person's branch; the tree and the branch were left as they are",
                    }),
                )
            }
            Err(e) => {
                let code = match &e {
                    ApplyError::Collisions(_) => BlockCode::RestoreBlockedIgnoredCollision,
                    _ => BlockCode::WorktreeUnavailable,
                };
                self.transition(
                    self.state,
                    Reason::WorktreeNotReleased,
                    serde_json::json!({
                        "worktree": worktree.path.to_string_lossy(),
                        "code": code.as_str(),
                        "error": e.to_string(),
                        "detail": "the worktree is as the run left it; `relais resume --retire` retries once the paths named are dealt with",
                    }),
                )
            }
        }
    }

    /// Say, as a decision on the run, what became of the person's git index.
    fn say_index(&self, index: &crate::workspace::inplace::IndexOutcome, worktree: &TaskWorktree) {
        use crate::workspace::inplace::{IndexOutcome, Unrestored};
        let (what, reason) = match index {
            IndexOutcome::Untouched => return,
            IndexOutcome::Restored => (
                "real_index_restored",
                format!(
                    "the worker changed your git index in {}; it is back as it was when the run began",
                    worktree.path.display()
                ),
            ),
            IndexOutcome::Changed(unrestored) => (
                "real_index_changed",
                format!(
                    "the worker changed your git index in {} and it could not be restored{}; the original is kept in the git directory as {}.index.orig",
                    worktree.path.display(),
                    match unrestored {
                        Unrestored::Failed(why) => format!(" ({why})"),
                        Unrestored::NotAsked => String::new(),
                    },
                    self.run_id
                ),
            ),
        };
        self.events.emit(Event::Decision {
            what: what.into(),
            reason,
        });
    }

    /// The tree the worker edits, when it exists: the caller's own worktree
    /// for an in-place run, else this run's task worktree. Candidate
    /// verification trees are seeded from it.
    fn task_tree(&self) -> Option<PathBuf> {
        if self.inplace.is_some() {
            return Some(self.config.repo_dir.to_path_buf());
        }
        Some(self.worktrees.join("task")).filter(|task| task.is_dir())
    }

    /// Set a tree up before the profile's commands run in it. `Isolated` is
    /// exactly [`verify::run_setup`]. A `Fork` or `InPlace` run first seeds
    /// `dir` with the dependency directories of `seed_from` copy-on-write,
    /// then runs every step, skipping only one that declared `reuse` and
    /// proved its outputs (ADR-0008).
    fn setup_tree(
        &self,
        dir: &Path,
        seed_from: Option<&Path>,
        profile: &crate::policy::VerificationProfile,
        logs_dir: &Path,
        prefix: &str,
        cancel: Option<&AtomicBool>,
    ) -> Result<Vec<verify::CheckOutcome>, verify::VerifyError> {
        use crate::workspace::reuse::{StampStore, Workspace};
        if self.config.workspace == Workspace::Isolated {
            return verify::run_setup_gated_within(
                dir,
                profile,
                logs_dir,
                prefix,
                &self.events,
                cancel,
                &mut verify::RunEveryStep,
                &self.heavy(),
            );
        }
        let source = seed_from.filter(|source| *source != dir);
        if let Some(source) = source {
            let own_run = self.inplace.is_some().then(|| self.run_id.as_str());
            setup_gate::seed_tree(self.config.repo_policy, source, dir, own_run, &self.events);
        }
        let state = self
            .config
            .artifacts_dir
            .parent()
            .unwrap_or(&self.config.artifacts_dir);
        let stamps = StampStore::new(state);
        let mut gate = setup_gate::ReuseGate::new(dir, source, &stamps, &self.events);
        verify::run_setup_gated_within(
            dir,
            profile,
            logs_dir,
            prefix,
            &self.events,
            cancel,
            &mut gate,
            &self.heavy(),
        )
    }

    /// A step of the run the pane shows (`protocol::Event::Step`).
    pub(crate) fn step(&self, name: &str, detail: String) {
        self.events.emit(Event::Step {
            name: name.to_string(),
            detail,
            max_attempts: None,
        });
    }

    /// Say, once, that the commands this repository runs changed since the
    /// last run that saw them (ADR-0007): a line on stderr and a
    /// `commands_changed` decision on the run. Whether it is said once
    /// is the store's: a recording one (`relais run`) stores what it saw,
    /// so the next run is silent. A replay never gets here. A store that
    /// fails is said and the run goes on, since a warning that cannot be
    /// kept must not stop work.
    fn say_command_changes(&self, repo_key: &str) {
        match crate::trust::observe(self.config.declarations, repo_key, self.config.repo_policy) {
            Ok(Some(change)) => {
                eprintln!("{}", change.text());
                self.events.emit(Event::Decision {
                    what: "commands_changed".into(),
                    reason: change.summary(),
                });
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("relais: the commands this repository ran under were not kept: {e}")
            }
        }
    }

    fn run(mut self) -> Result<RunOutcome, RunError> {
        let outcome = match self.run_to_outcome() {
            Finish::Own(outcome) => outcome,
            Finish::Exit(error) => return Err(error),
            // Another process ended the run (a reap, a workerless cancel)
            // and wrote its one outcome event. This driver writes none: a
            // second would contradict it. The plugin reading this stream
            // is still told the run is over, with the state the ledger
            // holds now.
            Finish::Superseded(outcome) => {
                if self.config.purpose != Some(crate::lifecycle::RunPurpose::Replay)
                    && self.parent.is_none()
                {
                    let (code, detail) = outcome.terminal.named_reason();
                    let _ = self.config.wire.send(&Request::Done {
                        run: Some(self.run_id.as_str()),
                        outcome: outcome.state().as_str(),
                        receipt: None,
                        summary: None,
                        trial: None,
                        code: code.as_deref(),
                        detail,
                    });
                }
                return Ok(outcome);
            }
        };
        // Whatever `persist_receipt` wrote, whichever way the run ended: an
        // accepted run's receipt, and the pending one a run stopped only by
        // an unmet sign-off stores (`store_pending_receipt`). The artifacts
        // directory is this run's own, so the file is this run's receipt.
        let written = self.artifacts.join("receipt.json");
        let receipt = written
            .is_file()
            .then(|| written.to_string_lossy().into_owned());
        let summary = Summary::of_candidate(&self.artifacts);
        self.events.emit(Event::Outcome {
            state: outcome.state().as_str().to_string(),
            receipt: receipt.clone(),
            summary,
            reason: None,
        });
        // A replay's `done` waits for its trial to be recorded: its caller
        // sends it (`relais dataset replay`).
        let announces = match self.config.purpose {
            Some(crate::lifecycle::RunPurpose::Replay) => false,
            Some(crate::lifecycle::RunPurpose::TrialArm) | None => self.parent.is_none(),
        };
        if announces {
            let (code, detail) = outcome.terminal.named_reason();
            let done = Request::Done {
                run: Some(self.run_id.as_str()),
                outcome: outcome.state().as_str(),
                receipt: receipt.as_deref(),
                summary,
                trial: None,
                code: code.as_deref(),
                detail,
            };
            // Dropped on failure: the plugin that was reading is gone, the
            // run's outcome is in the ledger and in `events.jsonl`.
            let _ = self.config.wire.send(&done);
        }
        Ok(outcome)
    }

    fn run_to_outcome(&mut self) -> Finish {
        match self.run_inner() {
            Ok(outcome) => Finish::Own(outcome),
            Err(RunError::Superseded(_)) => Finish::Superseded(self.superseded_outcome()),
            Err(error) if error.is_storage_full() => self.on_storage_full(&error),
            Err(error) => {
                // The runner itself failed. Say so on the record if the
                // record will still take it; the outcome says so either
                // way, and nothing is retried (SPEC §12).
                let detail = format!(
                    "the runner could not continue: {error}; the run is interrupted and \
                     whatever was written is preserved"
                );
                match self.transition(
                    State::Interrupted,
                    Reason::RunnerFailure,
                    serde_json::json!({ "error": error.to_string() }),
                ) {
                    Err(RunError::Superseded(_)) => Finish::Superseded(self.superseded_outcome()),
                    // Not recorded (the ledger refused it): the outcome
                    // still says the run is interrupted, and why.
                    Ok(()) | Err(_) => Finish::Own(RunOutcome {
                        run_id: self.run_id.clone(),
                        terminal: Terminal::Interrupted { detail },
                    }),
                }
            }
        }
    }

    /// relais's own write met a full disk (SPEC §10). The error has
    /// already unwound out of whatever was running, so no new launch
    /// starts and the running check's process group is gone. What is left
    /// is to persist, in order, the little that can still be saved: the
    /// ballast is deleted first and must have raised the available space,
    /// else nothing is written (a local snapshot can keep its blocks) and
    /// the run exits on its own code; then the candidate's ref, the
    /// `blocked (relais_storage_full)` transition and the follow-up note.
    /// A step that fails ends the same way, and `relais resume` reconciles
    /// from the last durable ledger state.
    fn on_storage_full(&mut self, error: &RunError) -> Finish {
        let probe = crate::storage_full::probe();
        let volume = self.tightest_volume(probe.as_ref());
        let exit = || {
            Finish::Exit(RunError::StorageFull {
                volume: volume.clone(),
            })
        };
        let ballast = state_sibling(
            &self.config.artifacts_dir,
            crate::storage_full::BALLAST_FILE,
        );
        if crate::storage_full::free_ballast(&ballast, probe.as_ref())
            != crate::storage_full::Freed::Rose
        {
            return exit();
        }
        // 1. The candidate's ref, when a snapshot exists. When the ref
        // cannot be written the outcome says so; the tree stays for resume.
        let mut notes = Vec::new();
        if let Some((index, sha)) = self.last_snapshot.clone() {
            if let Err(e) =
                workspace::name_candidate(self.config.repo_dir, self.run_id.as_str(), index, &sha)
            {
                notes.push(format!(
                    "the candidate {sha} could not be given its ref ({e}); its worktree is kept \
                     for `relais resume`"
                ));
            }
        }
        // 2. The ledger transition.
        let detail = format!(
            "relais's own storage on {volume} is full ({error}); new launches stopped and \
             running checks were killed. {}Free space on {volume}, then `relais resume {}` \
             reconciles the run",
            notes.iter().map(|n| format!("{n}. ")).collect::<String>(),
            self.run_id
        );
        let recorded = self.transition(
            State::Blocked,
            Reason::RelaisStorageFull,
            serde_json::json!({
                "volume": volume,
                "error": error.to_string(),
                "candidate": self.last_snapshot.as_ref().map(|(_, sha)| sha),
            }),
        );
        match recorded {
            Ok(()) => {}
            Err(RunError::Superseded(_)) => return Finish::Superseded(self.superseded_outcome()),
            Err(_) => return exit(),
        }
        // 3. The follow-up intent.
        let note = serde_json::json!({
            "intent": "resume",
            "run": self.run_id.as_str(),
            "command": format!("relais resume {}", self.run_id),
            "volume": volume,
        });
        let follow_up = self.artifacts.join("follow-up.json");
        if crate::fsutil::write_atomic(&follow_up, &note.to_string()).is_err() {
            return exit();
        }
        Finish::Own(RunOutcome {
            run_id: self.run_id.clone(),
            terminal: Terminal::BlockedBy {
                reason: Reason::RelaisStorageFull,
                detail,
            },
        })
    }

    /// The volume with the least space among those this run writes: the
    /// likeliest one to be full.
    fn tightest_volume(&self, probe: &dyn crate::disk::DiskProbe) -> String {
        crate::disk::volumes(probe, &self.disk_paths())
            .into_iter()
            .min_by_key(|volume| volume.available)
            .map_or_else(|| "unknown volume".to_string(), |volume| volume.mount)
    }

    /// The outcome of a run another process ended: what the ledger holds
    /// now, `cancelled` or else `interrupted`.
    fn superseded_outcome(&self) -> RunOutcome {
        let detail = format!(
            "run {} was ended by another process (reaped or cancelled)",
            self.run_id.as_str()
        );
        let terminal = match self.config.ledger.run_status(&self.run_id) {
            Ok(Some(State::Cancelled)) => Terminal::Cancelled { detail },
            Ok(_) | Err(_) => Terminal::Interrupted { detail },
        };
        RunOutcome {
            run_id: self.run_id.clone(),
            terminal,
        }
    }

    /// The run, by phase: preflight, baseline, then either the
    /// decomposed path or one owned worktree and a bounded attempt loop
    /// (SPEC §3, §9). Each phase either hands the next one what it
    /// established, or ends the run.
    fn run_inner(&mut self) -> Result<RunOutcome, RunError> {
        // The run's clock starts here, on the ledger's own: preflight
        // counts against `total_seconds`.
        let clock: Arc<dyn BudgetClock> = Arc::new(LedgerBudgetClock(self.config.ledger.clock()));
        let run_started = clock.now_millis();
        self.step("preflight", String::new());
        let preflight = match self.preflight()? {
            Phase::Ended(outcome) => return Ok(outcome),
            Phase::Ready(preflight) => preflight,
        };
        self.budgets = Budgets::started_at(
            clock,
            budget_limits(
                &preflight.authority,
                &self.config.contract.limits,
                &self.config.machine.resources,
            ),
            run_started,
        );
        self.step("baseline", String::new());
        let baseline = match self.baseline(&preflight)? {
            Phase::Ended(outcome) => return Ok(outcome),
            Phase::Ready(baseline) => baseline,
        };
        self.step("baseline", baseline_summary(&baseline));

        // The baseline is over: what follows draws on the work budget.
        self.budgets.begin_work();
        if let Some(exhausted) = self.budgets.exhausted() {
            let nominal = fresh_progress(&preflight).budget;
            return self.end_on_exhausted(exhausted, &nominal);
        }
        // The harness identity every dispatch of this run records, probed
        // once (SPEC §16: model/effort/harness identity are features).
        self.harness = self.config.backend.probe().map(|capabilities| {
            format!(
                "{} {}",
                self.config.backend.name(),
                capabilities.version.as_deref().unwrap_or("?")
            )
        });

        // Bounded decomposition (SPEC §19): work packages as runs of their
        // own, an assembled candidate verified independently. A planner
        // may answer "single worker", in which case the ordinary path
        // continues below.
        if let Some(decomposition) = &self.config.contract.decomposition {
            let root = scheduler::RootContext {
                authority: &preflight.authority,
                base_sha: &preflight.base_sha,
                contract_hash: &preflight.contract_hash,
                manifest: &preflight.manifest,
                decision: &preflight.decision,
                baseline_failures: &baseline.failures,
                integration_gaps: &preflight.integration_gaps,
                baseline_cached: baseline.cached,
                baseline_cache_refused: &baseline.cache_refused,
                logs_dir: &baseline.logs_dir,
            };
            match scheduler::run_decomposed(self, &root, decomposition)? {
                scheduler::Decomposed::Outcome(outcome) => return Ok(outcome),
                scheduler::Decomposed::SingleWorker => {}
            }
        }

        // One owned worktree for the whole run: repairs continue from a
        // candidate whose scope and integrity passed; a scope violation
        // stops everything (SPEC §8, §9).
        let worktree_path = self.worktrees.join("task");
        self.step("worktree · setup", String::new());
        let worktree = match self.take_in_place_worktree() {
            Some(Ok(worktree)) => worktree,
            Some(Err(e)) => return self.fail_preflight(BlockCode::WorktreeUnavailable, e),
            None => match workspace::create_worktree(
                self.config.repo_dir,
                &preflight.base_sha,
                &worktree_path,
            ) {
                Ok(worktree) => worktree,
                Err(e) => {
                    return self.fail_preflight(BlockCode::WorktreeUnavailable, e.to_string())
                }
            },
        };
        let worktree_path = worktree.path.clone();
        // A verify's tree is the merge result computed in preflight, not a
        // second merge: what conflict detection judged is what is checked.
        if let Some(prepared) = &preflight.candidate {
            let ended = match self.recheck_candidate(prepared)? {
                Some(outcome) => Some(outcome),
                None => match prepared.check_out(&worktree) {
                    Ok(()) => None,
                    Err(e) => Some(self.fail_preflight(
                        BlockCode::WorktreeUnavailable,
                        format!("the candidate's tree could not be checked out: {e}"),
                    )?),
                },
            };
            if let Some(outcome) = ended {
                self.retire_worktree(&worktree, &outcome)?;
                return Ok(outcome);
            }
        }
        // The same setup in the worker's own tree, so the worker can run
        // the checks it is told to pass. This is also the one place the
        // setup is proven when the baseline came from the cache: a
        // cached verdict skipped the base's copy, not the machine.
        let setup = self.while_watching_cancel(|cancel| {
            self.setup_tree(
                &worktree_path,
                Some(self.config.repo_dir),
                &preflight.authority.verification_profile,
                &baseline.logs_dir,
                "task",
                cancel,
            )
        })?;
        self.record_logs(None, EvidenceKind::SetupLog, &setup)?;
        if let Some(outcome) = self.end_if_cancelled()? {
            self.retire_worktree(&worktree, &outcome)?;
            return Ok(outcome);
        }
        let outcome = match verify::setup_failure(&setup) {
            // The worktree exists and no worker has touched it; whatever
            // the setup left there — an installed `node_modules` the
            // tree ignores, or a generated file it does not — is
            // retired like any run's tree: kept if in no patch, then gone.
            Some(failed) => self.fail_preflight(
                BlockCode::VerificationSetupFailed,
                setup_failure_detail(failed, "the task worktree"),
            )?,
            None => {
                let ctx = AttemptContext {
                    preflight: &preflight,
                    baseline: &baseline,
                    worktree: &worktree,
                    worktree_path: &worktree_path,
                };
                if self.config.contract.is_verify() {
                    self.verify_applied(&ctx)?
                } else {
                    self.attempt_loop(&ctx)?
                }
            }
        };
        self.retire_worktree(&worktree, &outcome)?;
        Ok(outcome)
    }

    /// Everything that must hold before a single worker is launched
    /// (SPEC §4–§7): the run is on record, the base is clean and
    /// resolved, authority and integrations allow the work, the
    /// coordinator knows the run's budget, the context package is
    /// assembled and the route is decided.
    fn preflight(&mut self) -> Result<Phase<Preflight>, RunError> {
        std::fs::create_dir_all(&self.artifacts)?;
        let ledger = self.config.ledger;
        // The repository half of this run's task identity (P2's
        // `RepoIdentity`, hashed the way a trust grant is): resolved
        // here, ahead of `insert_run`/`insert_child_run`, because both
        // need it to place the run under its task.
        let repo_identity = crate::repo::identity(self.config.repo_dir);
        let this_repo_key = repo_key(&repo_identity);
        match &self.parent {
            None => {
                // A fresh task identity is DERIVED, not minted: the same
                // repository and the same first contract always land on
                // the same task, so a re-run of this task reuses the row
                // its first run created. `task_override` is the CLI's
                // already-confirmed answer to `--revise` or a declared
                // `task_id` — set, it names the task directly rather
                // than deriving one.
                let task = match self.config.task_override {
                    Some(task) => task.clone(),
                    None => derive_task_id(&this_repo_key, &self.config.contract.hash()),
                };
                let inserted = ledger.insert_typed_run(
                    &self.run_id,
                    &self.config.repo_dir.to_string_lossy(),
                    Some(&self.config.session_id),
                    &task,
                    &this_repo_key,
                    self.config.contract.task_type,
                );
                // Before anything can be dispatched. A purpose written
                // after the run finishes is absent for every path that
                // does not reach the end — an error, a kill, a ceiling —
                // and those are exactly the runs whose spend most needs
                // accounting for.
                if let Some(purpose) = self.config.purpose {
                    ledger.set_run_purpose(&self.run_id, purpose)?;
                }
                inserted?;
                // The run is launched (its row exists) and nothing has
                // told the caller its id yet.
                #[cfg(test)]
                crate::ledger::failpoint::hit(crate::ledger::failpoint::AFTER_LAUNCH);
            }
            Some((parent_run, package_id, _role)) => {
                // A work package is not a task of its own (SPEC §19): it
                // inherits the root's, which the root's own preflight
                // already put on record before any package is scheduled.
                let task = ledger.task_of_run(parent_run)?.ok_or_else(|| {
                    RunError::Other(format!(
                        "run {parent_run} has no task on record; its own preflight must \
                         complete before a work package can inherit it"
                    ))
                })?;
                ledger.insert_child_run(
                    &self.run_id,
                    &self.config.repo_dir.to_string_lossy(),
                    Some(&self.config.session_id),
                    crate::ledger::ChildOf {
                        parent_run,
                        package_id,
                    },
                    &task,
                    &this_repo_key,
                )?
            }
        }

        // Preflight: dirty base is explicit, never copied (SPEC §8). A
        // status check that cannot RUN is not a clean tree: it is no
        // answer at all, and proceeding would be the silent copy §8
        // forbids, performed on an unknown tree (audit B11). The refusal
        // is the same one a dirty tree gets, with the reason the check
        // gave.
        // An in-place run starts from the tree as it is: uncommitted work
        // is part of the start snapshot, not a refusal (ADR-0008).
        // A verify applies a merge result into a tree of its own; it never
        // rewrites the person's worktree.
        let in_place = self.config.workspace == crate::workspace::reuse::Workspace::InPlace
            && self.parent.is_none()
            && !self.config.contract.is_verify();
        match workspace::dirty_paths(self.config.repo_dir).map(|dirty| {
            if in_place {
                Vec::new()
            } else {
                dirty
            }
        }) {
            Ok(dirty) if dirty.is_empty() => {}
            Ok(dirty) => {
                return Ok(Phase::Ended(self.fail_preflight(
                    BlockCode::DirtyBase,
                    format!(
                        "working tree has uncommitted changes ({}); commit or stash first",
                        dirty.join(", ")
                    ),
                )?));
            }
            Err(e) => {
                return Ok(Phase::Ended(self.fail_preflight(
                    BlockCode::DirtyBase,
                    format!(
                        "the working tree's status could not be read ({e}); relais cannot show \
                         that the base is clean and will not run against a tree it cannot see"
                    ),
                )?));
            }
        }

        // Effective authority is the intersection; blockers stop dispatch
        // (SPEC §5, §6). `repo_identity` is the same value the task
        // identity above was keyed on, resolved once at the top of this
        // function because this is where the run meets the disk; `policy`
        // decides from the value and looks at nothing.
        let authority = effective_authority(
            self.config.repo_policy,
            self.config.machine,
            self.config.contract,
            &repo_identity,
        );
        if let Some(first) = authority.blockers.first() {
            let (code, detail) = (first.code, first.detail.clone());
            return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
        }
        // Which grant admitted this run, kept on the run: `relais doctor`
        // counts the runs a legacy grant still admits (ADR-0007).
        ledger.set_run_trust_status(&self.run_id, authority.trust_status.as_str())?;
        // A pending command change is `relais run`'s to say: a replay
        // neither says nor records it, so a batch of replays does not
        // repeat the warning once per run (`plan`, `doctor` and `trust
        // show` report it).
        if self.parent.is_none()
            && self.config.purpose != Some(crate::lifecycle::RunPurpose::Replay)
        {
            self.say_command_changes(&this_repo_key);
        }
        if self.config.workspace != crate::workspace::reuse::Workspace::Isolated {
            self.events.emit(Event::Decision {
                what: "workspace_resolved".into(),
                reason: format!(
                    "{} in {}",
                    self.config.workspace.as_str(),
                    self.config.repo_dir.display()
                ),
            });
        }
        // Integrations (SPEC §5): a missing required one blocks execution;
        // optional gaps travel in the receipt and are never passes.
        if let Some(blocker) = crate::tooling::probe_integrations(self.config.repo_policy).first() {
            let (code, detail) = (blocker.code, blocker.detail.clone());
            return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
        }
        let integration_gaps = verify::integration_gaps(
            &self.config.repo_policy.integrations,
            &crate::tooling::binary_available,
        );

        // A weight no machine setting can grant is a profile that cannot
        // run here: refused now, whether or not a coordinator is up.
        let heavy_cap = verify::heavy_cap(
            self.config.machine.concurrency.max_heavy_commands,
            verify::cores(),
        );
        let profile = &authority.verification_profile;
        if let Some(detail) =
            verify::overweight(profile.setup.iter().chain(&profile.commands), heavy_cap)
        {
            return Ok(Phase::Ended(
                self.fail_preflight(BlockCode::AdmissionRefused, detail)?,
            ));
        }

        // Managed runs register their root budget and agent-tree limits
        // before any dispatch; children can only narrow them (SPEC §23).
        if let Some(gate) = self.config.gate {
            let registration = RunRegistration {
                run_id: self.run_id.as_str().to_string(),
                session_id: self.config.session_id.clone(),
                budget_micros: self
                    .config
                    .machine
                    .spending
                    .per_run_micros
                    .map(MicroUsd::to_micros),
                max_agents: Some(authority.max_agents_total),
                max_depth: Some(authority.max_agent_depth),
            };
            if let Err(e) = gate.register_run(&registration) {
                let (reason, code) = admission_block(&e);
                return Ok(Phase::Ended(self.block(
                    reason,
                    code,
                    format!("{e}; nothing was launched"),
                )?));
            }
            self.registration = Some(registration);
        }

        // The base resolves once (SPEC §4).
        let base_sha =
            match workspace::resolve_base(self.config.repo_dir, &self.config.contract.base_ref) {
                Ok(sha) => sha,
                Err(e) => {
                    return Ok(Phase::Ended(
                        self.fail_preflight(BlockCode::BaseUnresolvable, e.to_string())?,
                    ))
                }
            };

        let base_sha = if in_place {
            match self.enter_in_place(base_sha) {
                Ok(sha) => sha,
                Err((code, detail)) => {
                    return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
                }
            }
        } else {
            base_sha
        };

        let contract_hash = self.config.contract.hash();
        let revision_id = ledger.insert_contract_revision(
            &self.run_id,
            &contract_hash,
            &serde_json::to_string(&self.config.contract.canonical_value())
                .expect("a validated contract serializes"),
            &self.config.contract.base_ref,
            Some(&base_sha),
        )?;

        // A candidate is resolved once and checked for conflicts here, in
        // the object store, before anything runs: a conflicting candidate
        // costs no baseline (plan A1, A2).
        let candidate = match self.config.contract.source_candidate() {
            None => None,
            Some(wanted) => match self.prepare_candidate(wanted, &this_repo_key, &base_sha)? {
                Phase::Ended(outcome) => return Ok(Phase::Ended(outcome)),
                Phase::Ready(prepared) => Some(prepared),
            },
        };

        let predecessor =
            ledger
                .intent_of_run(&self.run_id)?
                .map(|intent| crate::contract::Predecessor {
                    run: intent.predecessor.as_str().to_string(),
                    intent_id: intent.intent_id,
                    scope_delta: intent.scope_delta,
                });

        // One probe: the version, the turn-ceiling capability and the
        // efforts the CLI accepts are the same answer about the same
        // installed harness.
        let capabilities = self.config.backend.probe();
        let manifest = match self.assemble_context(
            &authority,
            &contract_hash,
            &base_sha,
            capabilities.as_ref(),
        )? {
            Phase::Ended(outcome) => return Ok(Phase::Ended(outcome)),
            Phase::Ready(manifest) => manifest,
        };

        // Route (SPEC §6). The catalogs are resolved here, from the probe
        // and machine.toml, so `route` stays pure.
        let catalogs = crate::route::resolve_catalogs(
            self.config.repo_policy,
            &authority,
            self.config.machine,
            capabilities.as_ref().map(|c| &c.accepted_efforts),
        );
        // The declared type, else the ledger's classified one. Only a run
        // the replay command started (`RunPurpose::Replay`, set nowhere
        // else) is admitted below the implementation tier for a change.
        let task = ledger.task_of_run(&self.run_id)?;
        let task_type = match &task {
            Some(task) => ledger.routing_type(task, self.config.contract.task_type)?,
            None => self.config.contract.task_type,
        };
        let decision = match route(RouteInputs {
            draw_key: task.as_ref().map(|task| crate::route::DrawKey {
                task: task.as_str(),
                run: self.run_id.as_str(),
            }),
            task_type,
            has_design: self.config.design.is_present(self.config.contract),
            admission: if self.config.purpose == Some(crate::lifecycle::RunPurpose::Replay) {
                crate::route::Admission::Replay
            } else {
                crate::route::Admission::Live
            },
            contract: self.config.contract,
            repo: self.config.repo_policy,
            machine: self.config.machine,
            authority: &authority,
            predictor: self.config.predictor,
            catalogs: &catalogs,
        }) {
            Routed::Route(route) => route,
            Routed::Blocked(blocked) => {
                let first = blocked.first();
                let (code, detail) = (first.code, first.detail.clone());
                return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
            }
        };
        // A verify dispatches no worker: there is nothing to predict.
        let estimates = decision
            .estimates
            .as_ref()
            .filter(|_| !self.config.contract.is_verify());
        if let Some(estimates) = estimates {
            ledger.record_prediction(
                &self.run_id,
                &estimates.artifact_id,
                &estimates.input_hash,
                &estimates.raw,
            )?;
        }
        std::fs::write(self.artifacts.join("route.txt"), decision.explain())?;
        let rung = &decision.rung;
        let effort = effort_str(rung.effort.id())
            .map(|effort| format!("@{effort}"))
            .unwrap_or_default();
        self.events.emit(Event::Step {
            name: "preflight".to_string(),
            detail: format!("route {} · {}{effort}", decision.tier.as_str(), rung.model),
            max_attempts: Some(decision.max_attempts),
        });

        Ok(Phase::Ready(Preflight {
            authority,
            integration_gaps,
            base_sha,
            contract_hash,
            revision_id,
            manifest,
            decision,
            candidate,
            predecessor,
        }))
    }

    /// Resolve the candidate a verify names, persist it, and merge it into
    /// the target base in the object store. A refusal or a conflict ends
    /// the run `blocked` before the baseline.
    fn prepare_candidate(
        &mut self,
        wanted: &crate::contract::CandidateRef,
        repo_key: &str,
        base_sha: &str,
    ) -> Result<Phase<PreparedCandidate>, RunError> {
        let ledger = self.config.ledger;
        if let Err(detail) = workspace::merge_tree_supported() {
            return Ok(Phase::Ended(
                self.fail_preflight(BlockCode::VerificationUnavailable, detail)?,
            ));
        }
        let source = candidate::Source {
            repo_dir: self.config.repo_dir,
            artifacts_dir: &self.config.artifacts_dir,
        };
        let resolved = match ledger.candidate_input(&self.run_id)? {
            // Resolved once: a run that already has its row applies it.
            Some(stored) => {
                if stored.target_base_sha != base_sha {
                    let detail = format!(
                        "the candidate was resolved against base {} but this run's base is {base_sha}; \
                         detection and application must use one base",
                        stored.target_base_sha
                    );
                    return Ok(Phase::Ended(
                        self.fail_preflight(BlockCode::BaseUnresolvable, detail)?,
                    ));
                }
                stored
            }
            None => match candidate::resolve(ledger, &source, repo_key, base_sha, wanted)? {
                candidate::Resolution::Refused(refusal) => {
                    return Ok(Phase::Ended(self.refuse_candidate(refusal)?));
                }
                candidate::Resolution::Resolved(resolved) => {
                    ledger.record_candidate_input(&self.run_id, &resolved)?;
                    resolved
                }
            },
        };
        self.events.emit(Event::Decision {
            what: "candidate_resolved".into(),
            reason: format!(
                "{}/{} at {}",
                resolved.source_run, resolved.index, resolved.candidate_commit
            ),
        });
        let merged = workspace::merge_candidate(
            self.config.repo_dir,
            &resolved.source_base_sha,
            &resolved.target_base_sha,
            &resolved.candidate_commit,
        );
        match merged {
            Ok(workspace::Merged::Clean { tree }) => Ok(Phase::Ready(PreparedCandidate {
                resolved,
                tree,
                conflicts: None,
            })),
            // A verify judges the candidate as it is: it cannot be asked to
            // resolve a merge. A change that continues from one is handed
            // the merge instead (plan A4).
            Ok(workspace::Merged::Conflicted {
                tree,
                files,
                index_info,
            }) => {
                if self.config.contract.is_verify() {
                    let detail = candidate::conflict_detail(&resolved, &files);
                    return Ok(Phase::Ended(self.refuse_candidate(candidate::Refusal {
                        reason: Reason::CandidateDoesNotApply,
                        detail,
                    })?));
                }
                self.events.emit(Event::Decision {
                    what: "candidate_conflicts".into(),
                    reason: format!(
                        "{}/{} conflicts with the base in {}; the worker resolves them first",
                        resolved.source_run,
                        resolved.index,
                        files.join(", ")
                    ),
                });
                Ok(Phase::Ready(PreparedCandidate {
                    resolved,
                    tree,
                    conflicts: Some(CandidateConflicts { files, index_info }),
                }))
            }
            Err(e) => Ok(Phase::Ended(
                self.fail_preflight(BlockCode::SnapshotFailed, e.to_string())?,
            )),
        }
    }

    /// End the run `blocked` under the reason a candidate refusal names.
    fn refuse_candidate(&mut self, refusal: candidate::Refusal) -> Result<RunOutcome, RunError> {
        let candidate::Refusal { reason, detail } = refusal;
        self.finish(
            reason,
            serde_json::json!({ "detail": detail }),
            Terminal::BlockedBy { reason, detail },
        )
    }

    /// Before a verify's tree is built: the identity resolved in preflight
    /// still holds (the object, the derived digest, the patch files). A
    /// file changed between validation and now ends the run
    /// `candidate_digest_mismatch`.
    fn recheck_candidate(
        &mut self,
        prepared: &PreparedCandidate,
    ) -> Result<Option<RunOutcome>, RunError> {
        let source = candidate::Source {
            repo_dir: self.config.repo_dir,
            artifacts_dir: &self.config.artifacts_dir,
        };
        match candidate::cross_check(self.config.ledger, &source, &prepared.resolved)? {
            Some(refusal) => Ok(Some(self.refuse_candidate(refusal)?)),
            None => Ok(None),
        }
    }

    /// Context: verdicts and tool failures are distinct, contradictions
    /// block, missing answers gate dependents only (SPEC §7). The
    /// assembled package is evidence, written next to the run and
    /// referenced by hash (SPEC §7, §12).
    fn assemble_context(
        &mut self,
        authority: &EffectiveAuthority,
        contract_hash: &str,
        base_sha: &str,
        capabilities: Option<&crate::backend::Capabilities>,
    ) -> Result<Phase<ContextManifest>, RunError> {
        // Read hints are fingerprinted from the base tree; one that
        // resolves to nothing would point the worker at a path this
        // revision does not have, which is a preflight problem (V15).
        let fingerprints = match context::fingerprint_hints(
            self.config.git,
            self.config.repo_dir,
            base_sha,
            &self.config.contract.read_hints,
        ) {
            Ok(fingerprints) => fingerprints,
            Err(unresolvable) => {
                return Ok(Phase::Ended(self.fail_preflight(
                    BlockCode::ReadHintUnresolvable,
                    unresolvable.to_string(),
                )?))
            }
        };
        // The design is read from the base revision, like the hints, and an
        // unreadable one refuses the contract before any worker is paid for.
        let design = match (self.config.design, &self.config.contract.design) {
            (DesignSource::Given(design), _) => Some(design.clone()),
            (DesignSource::Contract, None) => None,
            (DesignSource::Contract, Some(path)) => {
                match context::read_design(self.config.git, self.config.repo_dir, base_sha, path) {
                    Ok(design) => Some(design),
                    Err(e) => {
                        return Ok(Phase::Ended(
                            self.fail_preflight(BlockCode::DesignUnreadable, e.to_string())?,
                        ))
                    }
                }
            }
        };
        let (turn_ceiling, warning) = harness_reading(
            capabilities,
            capabilities
                .is_none()
                .then(|| self.config.backend.probe_timed_out())
                .flatten(),
        );
        if let Some(warning) = warning {
            self.events.emit(warning);
        }
        let assembled = context::assemble(context::ContextInputs {
            contract: self.config.contract,
            repo: self.config.repo_policy,
            contract_hash,
            base_sha,
            policy_hash: &authority.authority_hash,
            fingerprints,
            tool_versions: context::ToolVersions {
                relais: crate::version().to_string(),
                aval: crate::tooling::integration_version("aval"),
                amont: crate::tooling::integration_version("amont"),
                claude_code: capabilities.and_then(|capabilities| capabilities.version.clone()),
            },
            // Recorded so a receipt says whether a turn ceiling was
            // enforceable at all on this harness (SPEC §11).
            turn_ceiling,
            resolver: self.config.aval_resolver,
            design,
        });
        match assembled {
            Ok(manifest) => {
                for gap in &manifest.guidance.gaps {
                    self.emit_guidance_gap(gap);
                }
                let manifest_path = self.artifacts.join("manifest.json");
                std::fs::write(
                    &manifest_path,
                    serde_json::to_string_pretty(&manifest).expect("a manifest serializes"),
                )?;
                self.config.ledger.record_evidence(
                    &self.run_id,
                    None,
                    EvidenceKind::ContextManifest,
                    &manifest_path,
                    Some(&context::manifest_hash(&manifest)),
                )?;
                Ok(Phase::Ready(manifest))
            }
            Err(ContextError::ContradictionBlocked { key, heads }) => {
                Ok(Phase::Ended(self.block(
                    Reason::ArchitectureContradiction,
                    BlockCode::ArchitectureContradiction,
                    format!("aval contradiction on `{key}` ({heads} heads)"),
                )?))
            }
            Err(ContextError::NeedsDecision { key, verdict }) => {
                let detail = format!("the task depends on `{key}` but aval answers {verdict:?}");
                Ok(Phase::Ended(self.finish(
                    Reason::ArchitectureUnresolved,
                    serde_json::json!({ "key": key, "verdict": format!("{verdict:?}") }),
                    Terminal::NeedsDecision {
                        reason: Reason::ArchitectureUnresolved,
                        detail,
                    },
                )?))
            }
            Err(ContextError::ToolFailure { key, detail }) => Ok(Phase::Ended(
                self.fail_preflight(BlockCode::AvalToolFailure, format!("`{key}`: {detail}"))?,
            )),
            Err(ContextError::SizingProblem { .. }) => Ok(Phase::Ended(self.fail_preflight(
                BlockCode::ContextSizing,
                "required context exceeds the budget".to_string(),
            )?)),
        }
    }

    /// A guidance gap is an event of the run, never a failure of it.
    fn emit_guidance_gap(&self, gap: &GuidanceGap) {
        self.events.emit(Event::Decision {
            what: "guidance".to_string(),
            reason: gap.describe(),
        });
    }

    /// The receipt's guidance note: one line, and only when guidance had
    /// a gap, the reviewer's included.
    fn guidance_note(&self, manifest: &ContextManifest) -> Option<String> {
        let mut gaps = manifest.guidance.gaps.clone();
        gaps.extend(self.reviewer_gap.clone());
        context::guidance::gap_line(&gaps)
    }

    /// Baseline verification at the base SHA: pre-existing failures are
    /// visible from the start (SPEC §10); cached only when the profile
    /// opts in (SPEC §18).
    fn baseline(&mut self, preflight: &Preflight) -> Result<Phase<Baseline>, RunError> {
        let authority = &preflight.authority;
        let logs_dir = self.artifacts.join("logs");
        let baseline_cache = verify::BaselineCache::new(
            &self
                .config
                .artifacts_dir
                .parent()
                .unwrap_or(&self.config.artifacts_dir)
                .join("baseline-cache"),
        );
        // The key names the toolchain the checks actually run on: a
        // `rustup update` changes the verdict, and the old key would
        // have reported a real regression as pre-existing (SPEC §18,
        // audit V2). A toolchain that cannot be identified refuses the
        // cache and says so in the receipt.
        let toolchain = verify::resolve_toolchain(&authority.verification_profile, &|program| {
            crate::tooling::program_version(program, None)
        });
        // The base's content, not its commit: the same files reached by a
        // different commit (an empty one between, an in-place start
        // snapshot) have the same verdict (ADR-0008). A tree git cannot
        // name falls back to the commit, which only narrows the cache.
        let base_tree = self
            .config
            .git
            .run(
                self.config.repo_dir,
                &["rev-parse", &format!("{}^{{tree}}", preflight.base_sha)],
            )
            .map(|tree| tree.trim().to_string())
            .unwrap_or_else(|_| preflight.base_sha.clone());
        let baseline_key = match &toolchain {
            Ok(toolchain) => Some(verify::baseline_key(
                &base_tree,
                &authority.verification_profile,
                &preflight.manifest.tool_versions,
                toolchain,
            )),
            Err(_) => None,
        };
        let cache_refused = toolchain.err();
        let cacheable = authority.verification_profile.cache_baseline;
        let cached = match (cacheable, &baseline_key) {
            (true, Some(key)) => baseline_cache.get(key, &logs_dir),
            _ => None,
        };
        let was_cached = cached.is_some();
        // The baseline's check outcomes are kept: a candidate identical to
        // the base has exactly these results, without re-running them.
        let (failures, checks) = match cached {
            // A hit that kept the outcomes has written their logs into this
            // run's `logs/`: they are this run's evidence like any check's.
            Some(hit) => {
                if let Some(checks) = &hit.checks {
                    self.record_logs(None, EvidenceKind::CheckLog, checks)?;
                }
                (hit.failures, hit.checks)
            }
            None => match verify::verification_worktree(
                self.config.git,
                self.config.repo_dir,
                &preflight.base_sha,
                &self.verify_dir.join("verify-base"),
            ) {
                Ok(mut baseline) => {
                    // The declared setup first, in the base's own copy:
                    // its logs are evidence whatever happens next, and a
                    // setup that did not succeed leaves nothing for the
                    // commands to run against — blocked, before any
                    // worker is bought (SPEC §10).
                    let setup = self.while_watching_cancel(|cancel| {
                        self.setup_tree(
                            baseline.path(),
                            Some(self.config.repo_dir),
                            &authority.verification_profile,
                            &logs_dir,
                            "base",
                            cancel,
                        )
                    });
                    let setup = match setup {
                        Ok(setup) => setup,
                        Err(verify::VerifyError::Lease(stop)) => {
                            if let Err(e) = baseline.release() {
                                eprintln!("relais: {e}");
                            }
                            let nominal = fresh_progress(preflight).budget;
                            return Ok(Phase::Ended(self.end_on_lease_stop(stop, &nominal)?));
                        }
                        Err(e) => return Err(e.into()),
                    };
                    self.record_logs(None, EvidenceKind::SetupLog, &setup)?;
                    if let Some(outcome) = self.end_if_cancelled()? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    if let Some(outcome) = self.end_on_baseline_exhaustion(&setup)? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    if let Some(failed) = verify::setup_failure(&setup) {
                        let detail = setup_failure_detail(failed, "the base revision");
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(
                            self.fail_preflight(BlockCode::VerificationSetupFailed, detail)?,
                        ));
                    }
                    let checks = self.while_watching_cancel(|cancel| {
                        verify::run_profile_within(
                            baseline.path(),
                            &authority.verification_profile,
                            &logs_dir,
                            "base",
                            &self.events,
                            cancel,
                            &self.heavy(),
                        )
                    });
                    let checks = match checks {
                        Ok(checks) => checks,
                        Err(verify::VerifyError::Lease(stop)) => {
                            if let Err(e) = baseline.release() {
                                eprintln!("relais: {e}");
                            }
                            let nominal = fresh_progress(preflight).budget;
                            return Ok(Phase::Ended(self.end_on_lease_stop(stop, &nominal)?));
                        }
                        Err(e) => return Err(e.into()),
                    };
                    self.record_logs(None, EvidenceKind::CheckLog, &checks)?;
                    if let Some(outcome) = self.end_if_cancelled()? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    // Exhaustion is evidence, never a red base: a failed
                    // check that shows a full disk or no memory gave no
                    // verdict (SPEC §10).
                    if let Some(outcome) = self.end_on_baseline_exhaustion(&checks)? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    // A check that gave no verdict at the base — the
                    // program was not found (exit 127), or the wall clock
                    // cut it off before it reported a status — leaves
                    // nothing to compare a candidate against, for exactly
                    // the same reason a missing program does: nothing to
                    // cache, and no worker can fix code that never ran.
                    // Blocked, with the remedy named — which is not
                    // always "add a setup". A candidate that times out is
                    // still judged as a failure (verify::CheckOutcome::
                    // failed); this reclassification happens only here,
                    // where the base is judged.
                    let unrunnable: Vec<&verify::CheckOutcome> = checks
                        .iter()
                        .filter(|check| {
                            verify::unrunnable(check)
                                || matches!(
                                    check.ended.recorded(),
                                    Some(Ended::TimedOut | Ended::Cancelled | Ended::Signalled)
                                        | None
                                )
                        })
                        .collect();
                    if !unrunnable.is_empty() {
                        let detail = unrunnable_baseline_detail(
                            self.config.repo_dir,
                            &self.config.contract.verification_profile,
                            &authority.verification_profile,
                            &preflight.base_sha,
                            &unrunnable,
                        );
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(
                            self.fail_preflight(BlockCode::BaselineUnrunnable, detail)?,
                        ));
                    }
                    let failures: Vec<String> = checks
                        .iter()
                        .filter(|outcome| outcome.failed())
                        .map(|outcome| outcome.label.clone())
                        .collect();
                    if let (true, Some(key)) = (cacheable, &baseline_key) {
                        let stored = baseline_cache.put(key, &failures, &checks);
                        let what = if stored == verify::Stored::NotCached {
                            "baseline_not_cached"
                        } else {
                            "baseline_cache_outcomes_skipped"
                        };
                        let skipped = match stored {
                            verify::Stored::WithOutcomes => None,
                            verify::Stored::NotCached => Some(
                                "the base has a failed check; a base is cached only when every check passed"
                                    .to_string(),
                            ),
                            verify::Stored::OverLimit { bytes } => Some(format!(
                                "the base's logs came to {} MiB; only its green verdict is cached, without its logs",
                                bytes / (1024 * 1024)
                            )),
                            verify::Stored::LogsUnreadable => Some(
                                "a base log could not be read back to match its hash; only its green verdict is cached, without its logs"
                                    .to_string(),
                            ),
                            verify::Stored::NotWritten => Some(
                                "the baseline cache could not be written; the next run of this base runs its checks again"
                                    .to_string(),
                            ),
                        };
                        if let Some(reason) = skipped {
                            self.events.emit(Event::Decision {
                                what: what.into(),
                                reason,
                            });
                        }
                    }
                    if let Err(e) = baseline.release() {
                        // Best effort: the verdict is already in hand and
                        // the throwaway tree holds nothing but a checkout
                        // of the base revision.
                        eprintln!("relais: {e}");
                    }
                    (failures, Some(checks))
                }
                Err(e) => {
                    return Ok(Phase::Ended(self.fail_preflight(
                        BlockCode::BaselineVerificationFailed,
                        e.to_string(),
                    )?));
                }
            },
        };
        Ok(Phase::Ready(Baseline {
            failures,
            checks,
            cached: was_cached,
            cache_refused,
            logs_dir,
        }))
    }

    /// A failed check among `checks` that shows exhaustion, with what the
    /// machine had at failure time (SPEC §10). Read from the failed check's
    /// output only.
    fn exhaustion_in(
        &self,
        checks: &[verify::CheckOutcome],
    ) -> Option<(String, exhaustion::Exhaustion, exhaustion::Measured)> {
        let (label, found) = exhaustion::first_exhaustion(checks)?;
        let probe = crate::storage_full::probe();
        let measured = exhaustion::measure(
            found.resource,
            probe.as_ref(),
            &self.disk_paths(),
            self.config.machine.resources.min_free_bytes(),
        );
        Some((label, found, measured))
    }

    /// The base's failed check showed exhaustion: `blocked
    /// (baseline_resource_exhausted)`, never a red base.
    fn end_on_baseline_exhaustion(
        &mut self,
        checks: &[verify::CheckOutcome],
    ) -> Result<Option<RunOutcome>, RunError> {
        let Some((label, found, measured)) = self.exhaustion_in(checks) else {
            return Ok(None);
        };
        let detail = exhaustion::baseline_text(&label, &found, &measured);
        self.finish(
            Reason::BaselineResourceExhausted,
            exhaustion::detail_json(&label, &found, &measured, None),
            Terminal::BlockedBy {
                reason: Reason::BaselineResourceExhausted,
                detail,
            },
        )
        .map(Some)
    }

    /// The candidate's failed check showed exhaustion: `needs_decision
    /// (resource_exhausted)` with the candidate preserved and no repair
    /// (SPEC §10). Exhaustion is observed, not proof the code is innocent.
    fn end_on_candidate_exhaustion(
        &mut self,
        ctx: &AttemptContext<'_>,
        candidate: &Candidate,
        exhausted: (String, exhaustion::Exhaustion, exhaustion::Measured),
    ) -> Result<RunOutcome, RunError> {
        let (label, found, measured) = exhausted;
        self.config.ledger.finish_attempt(
            candidate.attempt_id,
            State::NeedsDecision,
            Some(&ctx.worktree_path.to_string_lossy()),
            Some(&candidate.sha),
        )?;
        let detail = exhaustion::candidate_text(&label, &found, &measured, self.run_id.as_str());
        self.finish(
            Reason::ResourceExhausted,
            exhaustion::detail_json(&label, &found, &measured, Some(&candidate.sha)),
            Terminal::NeedsDecision {
                reason: Reason::ResourceExhausted,
                detail,
            },
        )
    }

    /// The bounded sequence of attempts the runner — not a model — owns
    /// (SPEC §9): ceilings first, then one dispatch, its candidate, and
    /// the machine's verdict on it.
    fn attempt_loop(&mut self, ctx: &AttemptContext<'_>) -> Result<RunOutcome, RunError> {
        let mut progress = fresh_progress(ctx.preflight);

        loop {
            if let Some(outcome) = self.end_if_cancelled()? {
                return Ok(outcome);
            }
            if let Some(outcome) = self.ceiling_reached(&progress)? {
                return Ok(outcome);
            }
            let dispatched = match self.dispatch_attempt(ctx, &mut progress)? {
                Phase::Ended(outcome) => return Ok(outcome),
                Phase::Ready(dispatched) => dispatched,
            };
            let judged = match self.snapshot_attempt(ctx, &progress, dispatched)? {
                Snapshot::Ended(outcome) => return Ok(outcome),
                Snapshot::Ready(candidate) => {
                    self.judge_candidate(ctx, &mut progress, candidate)?
                }
                Snapshot::Unresolved { attempt_id, paths } => {
                    self.fail_unresolved(ctx, &mut progress, attempt_id, &paths)?
                }
            };
            match judged {
                Step::Ended(outcome) => return Ok(outcome),
                // The attempt is booked and judged; only a further dispatch
                // is replaced by the block.
                Step::Again => {
                    if let Some(model) = progress.unpriced_model.take() {
                        let machine_toml = crate::paths::machine_settings_path()
                            .unwrap_or_else(|_| PathBuf::from("machine.toml")); // detail only names the file
                        return self.block(
                            Reason::BlockedPreflight,
                            BlockCode::NativeUnpriced,
                            crate::native_pricing::backstop_detail(&model, &machine_toml),
                        );
                    }
                }
            }
        }
    }

    /// A `kind: verify` run after its tree is built: no worker, no repair
    /// and no escalation. The applied tree is snapshotted as the run's one
    /// attempt and goes through the pipeline a worker's candidate goes
    /// through: scope check, verification, review. A failed check ends the
    /// run `failed`, with the evidence.
    fn verify_applied(&mut self, ctx: &AttemptContext<'_>) -> Result<RunOutcome, RunError> {
        if let Some(outcome) = self.end_if_cancelled()? {
            return Ok(outcome);
        }
        let mut progress = fresh_progress(ctx.preflight);
        progress.budget.attempts_used = 1;
        let tier = ctx.preflight.decision.tier;
        let attempt_id = self.config.ledger.insert_attempt(
            &self.run_id,
            ctx.preflight.revision_id,
            1,
            tier.as_str(),
            self.attempt_phase(AttemptKind::Initial),
        )?;
        let candidate = match self.snapshot_tree(
            ctx,
            &progress,
            Attempted {
                attempt_id,
                index: 1,
                tier,
                // No model wrote this candidate here: the reviewer, when
                // one is required, is independent of every worker.
                model: "relais-verify".to_string(),
            },
            None,
        )? {
            Snapshot::Ended(outcome) => return Ok(outcome),
            Snapshot::Ready(candidate) => candidate,
            // A verify is refused in preflight when the merge conflicts.
            Snapshot::Unresolved { .. } => return Err(RunError::Other(
                "a verify run has an unresolved conflict: it is refused before its tree is built"
                    .into(),
            )),
        };
        match self.judge_candidate(ctx, &mut progress, candidate)? {
            Step::Ended(outcome) => Ok(outcome),
            Step::Again => Err(RunError::Other(
                "a verify run was offered another attempt: it has no worker to repair with".into(),
            )),
        }
    }

    /// End the run on the budget that ran out (ADR-0010, SPEC §11): the
    /// work and total budgets are `budget_exhausted`; a wait budget is a
    /// block, because the machine, not the work, was what ran short.
    /// The heavy-command admission of this run (SPEC §23, ADR-0010).
    pub(crate) fn heavy(&self) -> verify::HeavyAdmission<'_> {
        let cores = verify::cores();
        verify::HeavyAdmission {
            gate: self.config.gate,
            run_id: self.run_id.as_str().to_string(),
            session_id: self.config.session_id.clone(),
            budgets: self.budgets.clone(),
            events: self.events.clone(),
            cap: verify::heavy_cap(self.config.machine.concurrency.max_heavy_commands, cores),
            cores,
            slots: self
                .config
                .machine
                .resources
                .shared_host_slots
                .as_ref()
                .map(PathBuf::from),
            ancestor_holds_slots: verify::ancestor_holds_slots(
                std::env::var(verify::HOST_SLOT_ENV).ok().as_deref(),
            ),
            poll: ADMISSION_POLL,
            announce_every: verify::ANNOUNCE_EVERY,
            heartbeat_every: verify::LEASE_HEARTBEAT_EVERY,
            sleeper: Arc::new(verify::ThreadSleeper),
            registration: self.registration.clone(),
            tries: self.heavy_tries.clone(),
            exhausted: |ended, tail| crate::exhaustion::classify(ended, tail).is_some(),
            disk: crate::disk::DiskCheck {
                floor: self.config.machine.resources.min_free_bytes(),
                probe: Arc::new(crate::disk::StatvfsProbe),
                paths: self.disk_paths(),
                growth: self.config.ledger,
            },
        }
    }

    /// The places this run writes (SPEC §23): the state directory, the
    /// repository, the worktrees and `TMPDIR`.
    fn disk_paths(&self) -> Vec<PathBuf> {
        vec![
            self.config.artifacts_dir.clone(),
            self.config.repo_dir.to_path_buf(),
            worktree_root(&self.config.artifacts_dir),
            verify_root(&self.config.artifacts_dir),
            std::env::temp_dir(),
        ]
    }

    /// End the run on a command that was never admitted: a budget that
    /// ran out in the queue ends on its own name, a cancellation as a
    /// cancellation, and a coordinator that is down or refuses as
    /// admission does for model work.
    pub(crate) fn end_on_lease_stop(
        &mut self,
        stop: verify::LeaseStop,
        budget: &Budget,
    ) -> Result<RunOutcome, RunError> {
        use verify::LeaseStop;
        let text = stop.to_string();
        match stop {
            LeaseStop::Exhausted(exhausted) => self.end_on_exhausted(exhausted, budget),
            LeaseStop::Cancelled
            | LeaseStop::Refused {
                code: Refusal::RunCancelled,
                ..
            } => self.stop(
                budget,
                Observation::Cancelled("the run was cancelled while a command was queued".into()),
            ),
            LeaseStop::Gate(e) => {
                let (reason, code) = admission_block(&e);
                self.block(reason, code, format!("{e}; the command was not started"))
            }
            LeaseStop::Refused { detail, .. } => self.block(
                Reason::AdmissionRefused,
                BlockCode::AdmissionRefused,
                detail,
            ),
            LeaseStop::Weight { .. } | LeaseStop::HostSlots { .. } => {
                self.block(Reason::AdmissionRefused, BlockCode::AdmissionRefused, text)
            }
            LeaseStop::Duplicate(id) => self.finish(
                Reason::DuplicateDispatch,
                serde_json::json!({ "dispatch_id": id }),
                Terminal::Interrupted {
                    detail: format!("lease {id} was already admitted elsewhere"),
                },
            ),
        }
    }

    pub(crate) fn end_on_exhausted(
        &mut self,
        exhausted: Exhausted,
        budget: &Budget,
    ) -> Result<RunOutcome, RunError> {
        let limits = self.budgets.limits();
        match exhausted {
            Exhausted::Work => self.stop(budget, Observation::LimitReached(Limit::WallClock)),
            Exhausted::Total => self.stop(budget, Observation::LimitReached(Limit::Total)),
            Exhausted::AdmissionWait => {
                let detail = format!(
                    "queued for a lease for {}s, the whole admission wait budget \
                     (`[resources] admission_wait_seconds`); nothing was launched",
                    limits.admission_wait.as_secs()
                );
                self.finish(
                    Reason::AdmissionWaitExhausted,
                    serde_json::json!({ "detail": detail }),
                    Terminal::BlockedBy {
                        reason: Reason::AdmissionWaitExhausted,
                        detail,
                    },
                )
            }
            Exhausted::DiskWait => {
                let detail = format!(
                    "waited {}s for free disk space, the whole disk wait budget \
                     (`[resources] disk_wait_seconds`)",
                    limits.disk_wait.as_secs()
                );
                self.finish(
                    Reason::DiskLow,
                    serde_json::json!({ "detail": detail }),
                    Terminal::BlockedBy {
                        reason: Reason::DiskLow,
                        detail,
                    },
                )
            }
        }
    }

    /// Budget ceilings are checked BEFORE admitting more work (SPEC §9,
    /// §11). `Some(outcome)` means one was reached and the run ended.
    fn ceiling_reached(&mut self, progress: &Progress) -> Result<Option<RunOutcome>, RunError> {
        if progress.budget.attempts_used >= progress.budget.max_attempts {
            return Ok(Some(self.stop(
                &progress.budget,
                Observation::LimitReached(Limit::Attempts {
                    max: progress.budget.max_attempts,
                    last_failures: progress.last_failures.clone().unwrap_or_default(),
                }),
            )?));
        }
        if let Some(exhausted) = self.budgets.exhausted() {
            return Ok(Some(self.end_on_exhausted(exhausted, &progress.budget)?));
        }
        if let Some(ceiling) = self.config.machine.spending.per_run_micros {
            if progress.spend.total >= ceiling {
                return Ok(Some(self.stop(
                    &progress.budget,
                    Observation::LimitReached(spend_limit(
                        progress.spend.total,
                        ceiling,
                        Ceiling::PerRun,
                    )),
                )?));
            }
        }
        // The machine's day, not this run's (SPEC §11: stop admitting
        // work once the dollar control is exhausted). Every settled
        // attempt of this run is already a usage row, so the day's
        // recorded spend includes it — adding the run's own total
        // again would count it twice.
        if let Some(ceiling) = self.config.machine.spending.per_day_micros {
            let today = self.spent_today(self.config.ledger)?;
            if today >= ceiling {
                return Ok(Some(self.stop(
                    &progress.budget,
                    Observation::LimitReached(spend_limit(today, ceiling, Ceiling::PerDay)),
                )?));
            }
        }
        Ok(None)
    }

    /// One attempt: its ledger row, its prompt, its managed dispatch and
    /// the accounting that follows — usage, cost and the model the
    /// harness actually ran (SPEC §6, §11, §12).
    fn dispatch_attempt(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
    ) -> Result<Phase<Dispatched>, RunError> {
        let ledger = self.config.ledger;
        let authority = &ctx.preflight.authority;
        progress.budget.attempts_used += 1;
        let index = progress.budget.attempts_used;
        let rung_index = progress.budget.rung;
        let rung = ctx
            .preflight
            .decision
            .ladder
            .rung(rung_index)
            .ok_or_else(|| {
                RunError::Other(format!(
                    "internal error: the ladder has no rung {} for attempt {index}",
                    rung_index.get()
                ))
            })?;
        let tier = rung.tier;
        let kind = progress.kind;
        let attempt_id = ledger.insert_attempt(
            &self.run_id,
            ctx.preflight.revision_id,
            index as i64,
            tier.as_str(),
            self.attempt_phase(kind),
        )?;

        // The ladder's rung at the index the machine named, exactly as
        // `route()` resolved and validated it: tier, model and effort.
        let model_profile = rung.profile();
        let mut prompt = build_prompt(
            self.config.contract,
            &ctx.preflight.manifest,
            authority.verification_profile.commands.len(),
            progress.last_failures.as_deref(),
            kind,
        );
        // A continued candidate that conflicted: what is still open is the
        // worker's first job (plan A4). A path it already resolved is not
        // listed again.
        let conflicted = ctx.preflight.conflicted_paths();
        let open = ctx
            .worktree
            .unresolved_conflicts(conflicted)
            .unwrap_or_else(|_| conflicted.to_vec());
        prompt.push_str(&conflict_block(&open));

        // Dispatch intent is persisted BEFORE the process exists
        // (SPEC §12), keyed so retries cannot duplicate agents.
        let dispatch_id = self.config.ids.dispatch_id()?;
        // The whole resolved ladder rides on the run's first worker
        // dispatch, so the intent alone says what every later rung was.
        let ladder = (index == 1).then_some(&ctx.preflight.decision.ladder);
        ledger.record_dispatch_intent(
            &dispatch_id,
            &self.run_id,
            Some(attempt_id),
            &serde_json::json!({
                "model": model_profile.id,
                "effort": model_profile.effort,
                "harness": self.harness,
                "tier": tier.as_str(),
                "rung": rung_index,
                "ladder": ladder,
                "kind": kind.as_str(),
                "prompt_bytes": prompt.len(),
                "recipe_id": ctx.preflight.decision.covering.as_ref().map(|r| &r.recipe_id),
                "recipe_name": ctx.preflight.decision.covering.as_ref().map(|r| &r.name),
                "recipe_revision": ctx.preflight.decision.covering.as_ref().map(|r| r.revision),
                "task_type_draw": ctx.preflight.decision.type_draw,
            }),
            0,
            ctx.preflight.decision.routed_by,
        )?;
        // The row says what the coordinator is told (`NativeRun`) from the
        // moment it exists, so the two never disagree during the wait, and
        // a launch that fails before any agent binds keeps the right kind.
        self.record_native_row(&dispatch_id, None)?;
        ledger.record_features(
            &dispatch_id,
            &serde_json::json!({
                "attempt_index": index,
                "tier": tier.as_str(),
                "model": model_profile.id,
                "kind": kind.as_str(),
            }),
        )?;

        // Every worker dispatch is a transition into `running`, whatever
        // kind of attempt it is (SPEC §9): the chain used to skip
        // straight from `prepared` to `verifying`, so a run ten minutes
        // into its first attempt still read as not yet started.
        self.transition(
            State::Running,
            Reason::WorkerDispatched,
            serde_json::json!({
                "dispatch_id": dispatch_id.as_str(),
                "attempt_index": index,
                "tier": tier.as_str(),
                "kind": kind.as_str(),
            }),
        )?;
        // Re-taken after `transition(&mut self)`: the shared reference the
        // function opened with cannot outlive that mutable call.
        let ledger = self.config.ledger;

        let remaining_wall = self.budgets.remaining().max(Duration::from_secs(1));
        // What this attempt may still spend, not the whole ceiling
        // again: the budget is the run's, not the attempt's.
        // Saturating: a wrapped remainder would read as a budget
        // nothing bounds (P7).
        let remaining_budget = self
            .config
            .machine
            .spending
            .per_run_micros
            .map(|ceiling| ceiling.remaining_after(progress.spend.total).to_micros());
        let spec = LaunchSpec {
            dispatch_id: dispatch_id.as_str().to_string(),
            prompt,
            model: model_profile.id.clone(),
            effort: model_profile.effort.clone(),
            max_turns: None,
            budget_micros: remaining_budget,
            disallowed_tools: authority.disallowed_tools.clone(),
            work_dir: ctx.worktree_path.to_path_buf(),
            wall_timeout: remaining_wall,
            cancel: None,
            agent: AgentKind::Worker,
        };

        let requested_model = model_profile.id.clone();
        let requested_effort = effort_str(model_profile.effort.as_ref());
        // Around the launch, monotonic: the dispatch's own elapsed time,
        // never derived from the `at` timestamps `record_usage` stamps.
        let dispatch_start = Instant::now();
        let result = match self.managed_launch(ManagedDispatch {
            spec,
            attempt: Some(index),
            depth: 0,
            parent: None,
            reserve_micros: remaining_budget.unwrap_or(0),
            budget: &progress.budget,
            write_lease: Some(ctx.worktree_path),
        })? {
            Ok(result) => result,
            Err(outcome) => {
                ledger.finish_dispatch(&dispatch_id, "launch_failed")?;
                ledger.finish_attempt(attempt_id, outcome.state(), None, None)?;
                return Ok(Phase::Ended(outcome));
            }
        };
        let duration_ms = dispatch_start.elapsed().as_millis() as i64;
        ledger.finish_dispatch(&dispatch_id, "completed")?;
        ledger.record_native_dispatch(&dispatch_id, result.session_id.as_deref())?;
        // What the backend says it did differently from what was asked is
        // evidence of the attempt.
        if !result.stderr.is_empty() {
            let note_path = self
                .artifacts
                .join(format!("attempt-{index}-native-note.txt"));
            self.record_artifact(
                Some(attempt_id),
                EvidenceKind::NativeNote,
                &note_path,
                &result.stderr,
            )?;
        }

        // Usage is recorded even when the attempt went nowhere: all
        // recorded cost, failed runs included (SPEC §11).
        let usage = &result.usage;
        let event = UsageEvent {
            event_id: dispatch_id.as_str().to_string(),
            run_id: self.run_id.clone(),
            attempt_id: Some(attempt_id),
            parent_event_id: None,
            model: result.effective_model.clone(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cost: usage.cost.micros(),
            // An attempt's cost is always tokens times the price table,
            // known or not: an unpriced one is still an estimate that could
            // not be made, never API spend somebody reported.
            cost_kind: self.usage_cost_kind(),
            completeness: usage.cost.completeness(),
            inclusive: usage.cost.inclusive(),
            at: self.config.ledger.now(),
            phase: Some(self.attempt_phase(kind)),
            duration_ms: Some(duration_ms),
            requested_model: Some(requested_model.clone()),
            requested_effort,
            harness: self.harness.clone(),
        };
        self.book_usage(&event, &result)?;
        progress.spend.fold(event.cost, usage.cost.completeness());
        progress.unpriced_model = unpriced_native_model(&result);

        if let Some(model) = &result.effective_model {
            if !progress.models_used.contains(model) {
                progress.models_used.push(model.clone());
            }
        }

        // An unapproved substitution stops further dispatch and
        // invalidates any claim that the requested route was tested
        // (SPEC §6). A harness that named NO model leaves the
        // question open, which is a recorded gap and stops dispatch
        // just the same: the run cannot say the route was tested
        // (audit V3). A substitution `machine.toml` approved in advance
        // is neither: dispatch carries on exactly as a match would — the
        // usage event just recorded above already carries both the
        // requested model and the effective one that ran, so the money
        // is attributed to the model that actually ran and the route's
        // own request is not lost.
        match crate::backend::verify_model(
            &requested_model,
            result.effective_model.as_deref(),
            &self.config.machine.routing.approved_substitutions,
        ) {
            crate::backend::ModelVerification::Matches => {}
            // Recorded, not waved through. The usage event above already
            // carries both identities, so the money is attributed
            // correctly either way — but that pair is two columns a
            // reader would have to compare deliberately, and nothing in
            // the run's own story would say a substitution happened.
            // This is a same-state transition, the idiom this runner
            // already uses for a fact that changes no state.
            crate::backend::ModelVerification::Approved {
                requested,
                effective,
            } => {
                let state = self.state;
                self.transition(
                    state,
                    Reason::ModelSubstitutionApproved,
                    serde_json::json!({
                        "requested": requested,
                        "effective": effective,
                        "dispatch": dispatch_id.as_str(),
                    }),
                )?;
            }
            crate::backend::ModelVerification::Substituted {
                requested,
                effective,
            } => {
                return Ok(Phase::Ended(self.stop(
                    &progress.budget,
                    Observation::UnapprovedSubstitution {
                        requested,
                        effective,
                    },
                )?));
            }
            // A dispatch that produced no terminal result — killed by
            // the wall clock, cancelled, crashed — could not have
            // reported a model either. The interruption below is the
            // outcome; calling it an unverified model would hide it.
            crate::backend::ModelVerification::Unverified { .. }
                if result.terminal_result_missing() => {}
            crate::backend::ModelVerification::Unverified { requested } => {
                return Ok(Phase::Ended(self.block(
                    Reason::UnapprovedSubstitution,
                    BlockCode::ModelUnverified,
                    format!(
                        "the harness reported no effective model, so nothing establishes that \
                         `{requested}` ran; dispatch stops rather than assume it"
                    ),
                )?));
            }
        }
        Ok(Phase::Ready(Dispatched {
            attempt_id,
            index,
            tier,
            model: model_profile.id.clone(),
            result,
        }))
    }

    /// What the worker left behind, turned into a candidate: the
    /// immutable snapshot, its name, its patch and its scope (SPEC §8).
    /// The ways an attempt ends without one — cancelled, interrupted,
    /// blocked, a tree still being written, a scope violation — end the
    /// run here.
    fn snapshot_attempt(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &Progress,
        dispatched: Dispatched,
    ) -> Result<Snapshot, RunError> {
        let ledger = self.config.ledger;
        let Dispatched {
            attempt_id,
            index,
            tier,
            model,
            result,
        } = dispatched;
        let worktree_path = ctx.worktree_path;
        let held_worktree = worktree_path.to_string_lossy().into_owned();

        // Cancelled through the coordinator: the worktree and
        // evidence stay; nothing else is dispatched (SPEC §23).
        if result.ended == Ended::Cancelled {
            ledger.finish_attempt(attempt_id, State::Cancelled, Some(&held_worktree), None)?;
            return Ok(Snapshot::Ended(self.stop(
                &progress.budget,
                Observation::Cancelled(
                    "the dispatch was cancelled through the coordinator; what the worker wrote \
                     is kept as a named candidate"
                        .into(),
                ),
            )?));
        }

        // Missing terminal result = interrupted, not failed (SPEC §9).
        if result.terminal_result_missing() {
            ledger.finish_attempt(attempt_id, State::Interrupted, Some(&held_worktree), None)?;
            return Ok(Snapshot::Ended(self.stop(
                &progress.budget,
                Observation::TerminalResultMissing {
                    timed_out: result.ended == Ended::TimedOut,
                    detail: result.failure_detail.clone().unwrap_or_default(),
                },
            )?));
        }

        // The worker's answer is evidence — for an inspect task it is
        // the whole deliverable (SPEC §4) — recorded before anything
        // is judged about it.
        if let Some(text) = result.result_text.as_deref() {
            let result_path = self.artifacts.join(format!("attempt-{index}-result.txt"));
            self.record_artifact(
                Some(attempt_id),
                EvidenceKind::WorkerResult,
                &result_path,
                text,
            )?;
        }

        // A worker blockage proposal is recorded as evidence and the
        // runner assigns blocked — the environment is never escalated
        // to a stronger model (SPEC §9).
        let claimed_blockage = result.worker_claims_blockage;
        if claimed_blockage {
            ledger.finish_attempt(attempt_id, State::Blocked, None, None)?;
            return Ok(Snapshot::Ended(self.stop(
                &progress.budget,
                Observation::WorkerBlockage(result.result_text.unwrap_or_default()),
            )?));
        }

        // Nothing is snapshotted from a tree still being written: the
        // worker's own lease went back when its process ended, so
        // any holder here is a straggler another tab or run owns
        // (SPEC §23).
        if let Some(holder) = self.wait_for_writers(worktree_path)? {
            ledger.finish_attempt(attempt_id, State::Interrupted, Some(&held_worktree), None)?;
            return Ok(Snapshot::Ended(
                self.stop_on_held_lease(worktree_path, &holder)?,
            ));
        }

        self.snapshot_tree(
            ctx,
            progress,
            Attempted {
                attempt_id,
                index,
                tier,
                model,
            },
            result.result_text,
        )
    }

    /// Snapshot the worktree as attempt `index`'s candidate, name it,
    /// export its patch and judge its scope. Shared by a worker's attempt
    /// and a verify run's applied tree: both go through the same
    /// pipeline.
    fn snapshot_tree(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &Progress,
        attempted: Attempted,
        result_text: Option<String>,
    ) -> Result<Snapshot, RunError> {
        let ledger = self.config.ledger;
        let Attempted {
            attempt_id,
            index,
            tier,
            model,
        } = attempted;
        let worktree_path = ctx.worktree_path;
        let held_worktree = worktree_path.to_string_lossy().into_owned();
        // The candidate snapshot is recorded outside model control,
        // added files included (SPEC §8).
        let sha = match ctx.worktree.snapshot_resolved(
            &format!("run {} attempt {index}", self.run_id),
            ctx.preflight.conflicted_paths(),
        ) {
            Ok(sha) => sha,
            Err(WorkspaceError::UnresolvedConflict(paths)) => {
                return Ok(Snapshot::Unresolved { attempt_id, paths });
            }
            Err(e) => {
                ledger.finish_attempt(attempt_id, State::Interrupted, None, None)?;
                return Ok(Snapshot::Ended(
                    self.fail_preflight(BlockCode::SnapshotFailed, e.to_string())?,
                ));
            }
        };
        self.last_snapshot = Some((index, sha.clone()));
        // `commit-tree` leaves a dangling object: the candidate the
        // receipt names, the patch's other side and the worktree's
        // history would all go in the next `git gc`. A ref under
        // `refs/relais/candidates/` keeps it reachable without
        // putting a branch in the user's namespace (audit B15).
        // Best effort: a ref that could not be written is not fatal —
        // the object exists and this run verifies it from the object —
        // and the retirement at the run's end names the tree `…/final`
        // itself when no ref holds it, so nothing is lost either way.
        // A full disk is the one failure of it that is not shrugged off:
        // relais's own write failing stops the run (SPEC §10).
        let named = crate::storage_full::at(crate::storage_full::Site::Ref)
            .map_err(WorkspaceError::Io)
            .and_then(|()| {
                workspace::name_candidate(self.config.repo_dir, self.run_id.as_str(), index, &sha)
            });
        if let Err(e) = named {
            if e.is_storage_full() {
                return Err(e.into());
            }
        }
        ledger.finish_attempt(
            attempt_id,
            State::Verifying,
            Some(&held_worktree),
            Some(&sha),
        )?;
        crate::storage_full::at(crate::storage_full::Site::Snapshot)?;
        std::fs::write(self.artifacts.join(format!("candidate-{index}.sha")), &sha)?;
        let patch_path = self.artifacts.join(format!("candidate-{index}.patch"));
        ctx.worktree.export_patch(&sha, &patch_path)?;
        // The reviewer's prompt names `candidate-latest.patch`, so the
        // copy is part of producing the candidate, not a convenience:
        // a failure here stops the run instead of handing a reviewer a
        // path that is not there (R4).
        let latest_patch = self.artifacts.join("candidate-latest.patch");
        std::fs::copy(&patch_path, &latest_patch)?;
        ledger.record_evidence(
            &self.run_id,
            Some(attempt_id),
            EvidenceKind::CandidatePatch,
            &patch_path,
            Some(&workspace::sha256_file(&patch_path)?),
        )?;

        // Write scope is checked on the actual diff; a violation can
        // never be accepted (SPEC §8, §9).
        match workspace::check_scope(ctx.worktree, &sha, self.config.contract) {
            Ok(_) => {}
            Err(WorkspaceError::ScopeViolation(paths)) => {
                ledger.finish_attempt(attempt_id, State::NeedsDecision, None, Some(&sha))?;
                return Ok(Snapshot::Ended(self.stop(
                    &progress.budget,
                    Observation::ScopeViolation(workspace::out_of_scope(
                        self.config.repo_dir,
                        &ctx.preflight.base_sha,
                        &sha,
                        paths,
                    )),
                )?));
            }
            Err(e) => {
                return Ok(Snapshot::Ended(
                    self.fail_preflight(BlockCode::ScopeCheckFailed, e.to_string())?,
                ));
            }
        }

        Ok(Snapshot::Ready(Candidate {
            attempt_id,
            index,
            tier,
            model,
            sha,
            latest_patch,
            result_text,
        }))
    }

    /// The attempt left a conflicted path unresolved: it produced no
    /// candidate, so it fails like a red check does (SPEC §9): a repair,
    /// then an escalation, then the attempt ceiling.
    fn fail_unresolved(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        attempt_id: i64,
        paths: &[String],
    ) -> Result<Step, RunError> {
        self.config
            .ledger
            .finish_attempt(attempt_id, State::Failed, None, None)?;
        let failures = vec![format!("unresolved_conflict: {}", paths.join(", "))];
        let same_failures = progress.last_failures.as_deref() == Some(failures.as_slice());
        progress.last_failures = Some(failures.clone());
        self.follow(
            ctx,
            progress,
            Observation::VerificationFailed {
                failures,
                unchanged_candidate: false,
                same_failures,
                all_preexisting: false,
            },
        )
    }

    /// Judge one candidate (SPEC §9, §10): what it changes about
    /// verification, whether the worker could act at all, the profile's
    /// checks, and what the machine says about the failures.
    fn judge_candidate(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        candidate: Candidate,
    ) -> Result<Step, RunError> {
        let ledger = self.config.ledger;
        let authority = &ctx.preflight.authority;
        let worktree_path = ctx.worktree_path;
        let held_worktree = worktree_path.to_string_lossy().into_owned();

        // A candidate that changes what verification IS gets neither
        // a silent pass nor one uniform answer (SPEC §9, §10).
        // Editing the profile's own inputs — build manifests,
        // lockfiles, the commands' programs — is "changes protected
        // verification": §9's table sends that to the user as
        // `needs_decision`, because no reviewer can decide on the
        // user's behalf that a loosened build is what was wanted.
        // Editing the TEST TREE is work §10 explicitly invites, so
        // it stays what it was: explicit review, whatever the route
        // said (audit B8).
        let touched_inputs = verify::classify_verification_inputs(
            &authority.verification_profile,
            &ctx.worktree.changed_paths_in(&candidate.sha)?,
        )?;
        if !touched_inputs.policy.is_empty() {
            let detail = format!(
                "the candidate changes what verification is: {}; these are the profile's own \
                 inputs, so passing its checks would not mean what the policy says it means. \
                 It is not a reviewer's to decide — the candidate and its patch are \
                 preserved",
                touched_inputs.policy.join(", ")
            );
            ledger.finish_attempt(
                candidate.attempt_id,
                State::NeedsDecision,
                Some(&held_worktree),
                Some(&candidate.sha),
            )?;
            return Ok(Step::Ended(self.finish(
                Reason::VerificationInputsChanged,
                serde_json::json!({
                    "paths": touched_inputs.policy,
                    "candidate": candidate.sha,
                }),
                Terminal::NeedsDecision {
                    reason: Reason::VerificationInputsChanged,
                    detail,
                },
            )?));
        }
        let verification_inputs_changed = touched_inputs.tests;
        let review_required = ctx.preflight.decision.review >= Review::Required
            || !verification_inputs_changed.is_empty();

        // A candidate that carries the base tree has the baseline's
        // results by identity: the commands are not spent again, and
        // the receipt says so.
        let identical = match ctx.worktree.same_tree_as_base(&candidate.sha) {
            Ok(same) => same,
            Err(e) => {
                return Ok(Step::Ended(
                    self.fail_preflight(BlockCode::SnapshotFailed, e.to_string())?,
                ));
            }
        };
        // An unchanged candidate is measured against the attempt before it
        // once there is one: from attempt 2 the previous attempt's work is
        // already in the tree, so comparing to the BASE says every repair
        // worker produced something (R2).
        //
        // An inspection's tree is always unchanged: what it produces is
        // its final message, and only an empty one is nothing.
        let inspecting = self.config.contract.kind() == crate::contract::Kind::Inspect;
        let unchanged = match progress.last_candidate.as_deref() {
            Some(previous) => previous == candidate.sha,
            None => identical,
        };
        let empty_report = inspecting
            && candidate
                .result_text
                .as_deref()
                .is_none_or(|text| text.trim().is_empty());
        // Entered only once the candidate is to be verified.
        if !verification_inputs_changed.is_empty()
            && ctx.preflight.decision.review < Review::Required
        {
            self.transition(
                State::Verifying,
                Reason::VerificationInputsChanged,
                serde_json::json!({ "paths": verification_inputs_changed }),
            )?;
        }

        // Verification against an immutable copy of the candidate
        // (SPEC §10). Entering `verifying` is a transition like every
        // other: assigned straight to the field, the ledger never
        // showed the state it then recorded as the next row's origin
        // (R7).
        self.transition(
            State::Verifying,
            Reason::VerificationStarted,
            serde_json::json!({ "candidate": candidate.sha, "attempt": candidate.index }),
        )?;
        let reuse = if identical {
            self.transition(
                State::Verifying,
                Reason::CandidateIdenticalToBase,
                serde_json::json!({ "candidate": candidate.sha }),
            )?;
            ctx.baseline.checks.as_deref()
        } else {
            None
        };
        let verify::Verified {
            checks,
            mut gaps,
            acceptance_gaps,
            amont_bypasses,
            amont_downgrades,
            gate_coverage,
        } = match self.verify_candidate(
            &candidate.sha,
            authority,
            &ctx.baseline.logs_dir,
            AttemptLabel::Attempt(candidate.index),
            reuse,
        ) {
            Ok(result) => result,
            Err(VerifyFailure::Lease(stop)) => {
                return Ok(Step::Ended(self.end_on_lease_stop(stop, &progress.budget)?));
            }
            Err(VerifyFailure::Unavailable(e)) => {
                return Ok(Step::Ended(
                    self.fail_preflight(BlockCode::VerificationUnavailable, e)?,
                ));
            }
            Err(VerifyFailure::Full(e)) => return Err(e),
        };
        if let Some(outcome) = self.end_if_cancelled()? {
            return Ok(Step::Ended(outcome));
        }
        // A check that timed out because a budget ran out under it says
        // nothing about the code: the run ends on the budget (ADR-0010),
        // with the candidate kept, rather than repairing a clock.
        if checks
            .iter()
            .any(|check| matches!(check.ended.recorded(), Some(Ended::TimedOut)))
        {
            if let Some(exhausted) = self.budgets.exhausted() {
                return Ok(Step::Ended(
                    self.end_on_exhausted(exhausted, &progress.budget)?,
                ));
            }
        }
        // A failed check that shows a full disk or no memory is evidence
        // for a person: no repair, no escalation, the candidate kept.
        if let Some(exhausted) = self.exhaustion_in(&checks) {
            return Ok(Step::Ended(
                self.end_on_candidate_exhaustion(ctx, &candidate, exhausted)?,
            ));
        }
        // An inspection judges its report: a gap about the profile's own
        // checks is recorded, not a stop. The acceptance-evidence gaps
        // keep their handling below.
        let mut gaps_not_judged = Vec::new();
        if inspecting {
            (gaps, gaps_not_judged) = split_profile_gaps(gaps, &acceptance_gaps);
        }
        let sign_off_only = !acceptance_gaps.is_empty()
            && acceptance_gaps.len() == gaps.len()
            && acceptance_gaps
                .iter()
                .all(|gap| gap.missing == verify::MissingEvidence::SignOffUnrecorded);
        // An inspection with only sign-offs open is judged first: its
        // report may still be unmet, which a person's answer cannot fix.
        let judged_first = inspecting && sign_off_only;
        if !gaps.is_empty() && !judged_first {
            // Every other gap here is either unfixable from `relais
            // decide` (a missing/undefined check) or already a second
            // problem alongside one; a human-sign-off gap is the one
            // kind a person's OWN later answer can clear (SPEC §10). When
            // that is the only kind present, the checks, tests and review
            // this attempt already ran are worth keeping: store the
            // receipt they would have produced now, `needs_decision`,
            // rather than losing that work and making the eventual
            // approval re-derive it from nothing.
            if sign_off_only {
                self.store_pending_receipt(
                    ctx,
                    progress,
                    &candidate,
                    PendingEvidence {
                        checks: checks.clone(),
                        gaps: gaps.clone(),
                        gaps_not_judged: gaps_not_judged.clone(),
                        notes: Vec::new(),
                        gate_coverage: gate_coverage.clone(),
                        verdict: None,
                    },
                )?;
            }
            return Ok(Step::Ended(
                self.stop(&progress.budget, Observation::VerificationGap(gaps))?,
            ));
        }
        // An inspection changes nothing, so a check already failing at
        // the base is not its to repair (SPEC §10).
        let mut failures: Vec<String> = if inspecting {
            verify::new_failure_labels(&checks, &ctx.baseline.failures)
        } else {
            checks
                .iter()
                .filter(|check| check.failed())
                .map(|check| check.label.clone())
                .collect()
        };
        // An inspection's deliverable is its report: an empty one is a
        // failure the worker may repair once, as an empty candidate is
        // for a change.
        if empty_report {
            failures.push("empty_report".into());
        }
        // A change task whose candidate changes nothing has not met
        // its objective, whatever the baseline says: a behavioural
        // failure the worker can repair, never an acceptance.
        if identical
            && self.config.contract.kind() == crate::contract::Kind::Change
            && !self.config.contract.is_verify()
        {
            failures.push("empty_candidate".into());
        }

        if !failures.is_empty() && self.config.contract.is_verify() {
            // No repair and no escalation: the candidate was verified and
            // it failed.
            let detail = format!(
                "the verified candidate fails: {}; no worker was dispatched and nothing was \
                 repaired",
                failures.join(", ")
            );
            return Ok(Step::Ended(self.finish(
                Reason::BehavioralFailure,
                serde_json::json!({ "failures": failures, "candidate": candidate.sha }),
                Terminal::Failed { detail },
            )?));
        }
        if !failures.is_empty() {
            let same_failures = progress.last_failures.as_deref() == Some(failures.as_slice());
            progress.last_candidate = Some(candidate.sha.clone());
            let all_preexisting = failures
                .iter()
                .all(|label| ctx.baseline.failures.contains(label));
            progress.last_failures = Some(failures.clone());
            // Plan C1: complete JUnit on both sides says every failing
            // test already fails at the base and nothing targets it. A
            // cached base keeps no failing checks, so it never applies.
            if let Some(tests) = ctx.baseline.checks.as_deref().and_then(|base| {
                verify::guarded_preexisting(
                    &failures,
                    base,
                    &checks,
                    &ctx.preflight.authority.verification_profile,
                    &self.config.contract.acceptance,
                )
            }) {
                return self.follow(
                    ctx,
                    progress,
                    Observation::PreexistingTestFailures { failures, tests },
                );
            }
            return self.follow(
                ctx,
                progress,
                Observation::VerificationFailed {
                    failures,
                    unchanged_candidate: unchanged,
                    same_failures,
                    all_preexisting,
                },
            );
        }

        let verified = Verified {
            checks,
            gaps,
            gaps_not_judged,
            notes: Vec::new(),
            amont_bypasses,
            amont_downgrades,
            verification_inputs_changed,
            review_required,
            gate_coverage,
        };
        if inspecting {
            return self.judge_inspection(ctx, progress, &candidate, verified);
        }
        self.accept_candidate(ctx, progress, &candidate, verified, None)
            .map(Step::Ended)
    }

    /// Record the machine's decision on an observation and follow it:
    /// another attempt, or the end of the run.
    fn follow(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        observation: Observation,
    ) -> Result<Step, RunError> {
        match self.decide(&progress.budget, observation)? {
            Next::Attempt { kind, rung } => {
                if kind == AttemptKind::Repair {
                    progress.budget.repairs_used += 1;
                }
                progress.kind = kind;
                progress.budget.rung = rung;
                // The ladder is read again at dispatch; the tier the
                // budget carries is only the rung's, kept for the
                // limits that name it.
                // A rung the ladder lacks is never substituted: the
                // attempt ceiling ends the run before dispatch, and
                // dispatch refuses an index out of the ladder.
                if let Some(at) = ctx.preflight.decision.ladder.rung(rung) {
                    progress.budget.tier = at.tier;
                }
                Ok(Step::Again)
            }
            Next::Accept => Err(RunError::Other(
                "the machine accepted a failing candidate".into(),
            )),
            Next::Stop(terminal) => Ok(Step::Ended(RunOutcome {
                run_id: self.run_id.clone(),
                terminal,
            })),
        }
    }

    /// An inspection's verification: one read-only dispatch that checks
    /// the worker's report against the files in the candidate worktree,
    /// on the worker's own tier and model. `Err` is the reason it could
    /// not be had, which ends the run `needs_review`, never accepted.
    fn review_report(
        &mut self,
        request: &ReviewRequest<'_>,
        candidate: &Candidate,
        worktree: &Path,
        tree: Option<&workspace::TrackedTree>,
        spend: &mut RunSpend,
    ) -> Result<verify::ReportVerdict, String> {
        let Some(profile) = request
            .authority
            .models
            .get(&request.candidate_tier)
            .cloned()
        else {
            return Err(format!(
                "no model configured at the {} tier to review the report",
                request.candidate_tier.as_str()
            ));
        };
        if let Some(exhausted) = self.review_spend_blocked(spend.total) {
            return Err(exhausted);
        }
        let statements = self.config.contract.acceptance_statements();
        let report = candidate.result_text.as_deref().unwrap_or_default();
        let prompt =
            report_review_prompt(&self.config.contract.objective, &statements, report, tree);
        let seat = ReviewSeat {
            profile: &profile,
            tier: request.candidate_tier,
            dir: worktree.to_path_buf(),
            purpose: ReviewPurpose::Report,
        };
        let result = match self.dispatch_reviewer(request, seat, prompt, spend) {
            Ok(result) => result,
            Err(ReviewOutcome::Unavailable(detail)) => return Err(detail),
            Err(ReviewOutcome::NoFindings | ReviewOutcome::Findings(_)) => {
                return Err("the report review dispatch answered with a verdict".into());
            }
        };
        let text = result.result_text.unwrap_or_default();
        self.record_artifact(
            Some(candidate.attempt_id),
            EvidenceKind::ReviewResult,
            &self
                .artifacts
                .join(format!("report-review-{}.txt", candidate.index)),
            &text,
        )
        .map_err(|e| format!("the report review could not be recorded as evidence: {e}"))?;
        Ok(verify::ReportVerdict::parse(&text, statements.len()))
    }

    /// Is a spending ceiling already reached, so the review must not be
    /// dispatched? The message names the ceiling; the caller turns it
    /// into `needs_review`, never an acceptance.
    pub(crate) fn review_spend_blocked(&self, total_cost: MicroUsd) -> Option<String> {
        let spending = &self.config.machine.spending;
        if let Some(ceiling) = spending.per_run_micros {
            if total_cost >= ceiling {
                let (spent, ceiling) = Ceiling::PerRun.render(total_cost, ceiling);
                return Some(format!(
                    "the spend ceiling was reached before the review could be dispatched \
                     ({spent} of {ceiling}); the candidate is unreviewed"
                ));
            }
        }
        if let Some(ceiling) = spending.per_day_micros {
            let today = match self.spent_today(self.config.ledger) {
                Ok(today) => today,
                Err(e) => return Some(format!("the day's spend could not be read: {e}")),
            };
            if today >= ceiling {
                let (spent, ceiling) = Ceiling::PerDay.render(today, ceiling);
                return Some(format!(
                    "the spend ceiling was reached before the review could be dispatched \
                     ({spent} of {ceiling}); the candidate is unreviewed"
                ));
            }
        }
        None
    }

    /// What this machine has spent since the start of the current UTC
    /// day, by the ledger's own clock — the figure a `per_day_micros`
    /// ceiling bounds. Every run on this machine counts, because the
    /// ceiling is the machine's.
    pub(crate) fn spent_today(&self, ledger: &Ledger) -> Result<MicroUsd, RunError> {
        let now = ledger.now();
        // The clock is the ledger's own, so an unparseable stamp is a
        // defect, not input: refusing to run beats running with a
        // ceiling that silently cannot be applied.
        let day_start = utc_day_start(&now).ok_or_else(|| {
            RunError::Other(format!(
                "the ledger clock answered {now:?}, which is not an RFC3339 timestamp; \
                 the daily spend ceiling has no day to measure"
            ))
        })?;
        Ok(ledger.spend_since(&day_start)?)
    }

    /// Where a reviewer reads the candidate from: the run's own task
    /// worktree, the assembled integration worktree when the run was
    /// decomposed, and the repository itself when neither exists (a
    /// reviewer with a working directory that is not there cannot even
    /// be launched).
    pub(crate) fn review_dir(&self) -> PathBuf {
        for candidate in [
            self.worktrees.join("task"),
            self.worktrees.join("integration"),
        ] {
            if candidate.is_dir() {
                return candidate;
            }
        }
        self.config.repo_dir.to_path_buf()
    }

    /// Retire the run's worktree at its terminal state (SPEC §8): a run
    /// delivers a NAMED candidate — the ref and the patch — and the
    /// directory goes once everything tracked is exported, which is
    /// what `workspace::retire` guarantees before it removes anything.
    /// An accepted run's tree is already its accepted candidate, so
    /// nothing more is exported; a run that ended any other way keeps
    /// whatever its last worker wrote under `…/final`. One permanent
    /// `git worktree list` entry per run, and the build output every
    /// worker leaves behind, are what used to accumulate (audit B15).
    ///
    /// An interrupted run's state is uncertain (SPEC §12): a writer may
    /// still be in the tree. Its worktree is retired here only once
    /// every dispatch the ledger still holds live is provably gone —
    /// the judgement `relais resume --retire` makes, made at the end
    /// instead of by a person later. A run whose own worker ended
    /// (its dispatch closed before the run was judged) has no live
    /// dispatch and is retired like any other end; one whose dispatch
    /// may still write — a live process, a native agent of a session
    /// whose plugin still says hello, an outcome nothing knows — keeps
    /// its tree for `resume --retire`. Two interrupted runs of
    /// 2026-10-08 kept 6.8 GB of installed dependencies for a day this
    /// way, with nothing left writing them.
    ///
    /// A retirement that fails is on the record and changes nothing
    /// about how the run ended: the directory is still there, and so
    /// is everything in it.
    pub(crate) fn retire_worktree(
        &mut self,
        worktree: &TaskWorktree,
        outcome: &RunOutcome,
    ) -> Result<(), RunError> {
        if self.inplace.is_some() {
            return self.finish_in_place(worktree, outcome);
        }
        match outcome.terminal.worktree_end() {
            WorktreeEnd::Keep => {
                // A write lease on the tree is a writer, whatever the
                // ledger says of dispatches (SPEC §23): a straggler that
                // held it past the deadline is still in there.
                let key = worktree.path.to_string_lossy().into_owned();
                let lease_holder = self
                    .config
                    .gate
                    .and_then(|gate| gate.write_lease_holder(&key).ok().flatten());
                let judged = self.judge_live_dispatches()?;
                let may_still_write = match lease_holder {
                    Some(holder) => Some(format!("the write lease is held by {holder}")),
                    None if !judged.all_provably_dead() => Some(judged.detail()),
                    None => None,
                };
                if let Some(why) = may_still_write {
                    self.events.emit(Event::Stderr {
                        text: format!(
                            "worktree {} kept: a dispatch may still write it ({why}); `relais \
                             resume --retire {}` retires it once that is settled",
                            worktree.path.display(),
                            self.run_id
                        ),
                    });
                    return Ok(());
                }
            }
            WorktreeEnd::Retire => {}
        }
        match workspace::retire(worktree, self.run_id.as_str(), &self.artifacts) {
            Ok(retirement) => self.transition(
                self.state,
                Reason::WorktreeRetired,
                retirement_detail(&worktree.path, &retirement),
            ),
            Err(e) => self.transition(
                self.state,
                Reason::WorktreeNotReleased,
                serde_json::json!({
                    "worktree": worktree.path.to_string_lossy(),
                    "error": e.to_string(),
                    "detail": "the worktree could not be retired; whatever it holds is still in it",
                }),
            ),
        }
    }

    /// One separate review call per candidate requiring it (SPEC §9, §10).
    /// The reviewer cannot edit or waive anything; findings are triaged,
    /// and "no findings" is recorded as evidence, not proof. A reviewer
    /// the runner cannot dispatch or record is `Unavailable`: the run
    /// ends needs_review, not accepted.
    ///
    /// Four steps, each its own function: who reviews and what they are
    /// asked (`review_prompt`), the dispatch, the accounting and the
    /// evidence, and the verdict read out of the answer.
    pub(crate) fn review_candidate(
        &mut self,
        request: &ReviewRequest<'_>,
        spend: &mut RunSpend,
    ) -> ReviewOutcome {
        // A reviewer gap belongs to the review that made it: a later
        // review that ends before it builds its prompt carries none.
        self.reviewer_gap = None;
        let Some((reviewer_tier, same_model)) = reviewer_tier(
            request.authority,
            request.candidate_tier,
            request.candidate_model,
        ) else {
            return ReviewOutcome::Unavailable(
                "no reviewer model is configured at any tier".into(),
            );
        };
        // The review is a dispatch and costs money like any other. It is
        // reached after the loop-top check, with the attempt that
        // produced this candidate already settled, so the run can be
        // exhausted here even though it was not when the loop last
        // looked: without this the reviewer launched with a budget of
        // zero and the run was accepted on a review nobody paid for
        // (SPEC §11: stop admitting work once the ceiling is reached).
        if let Some(exhausted) = self.review_spend_blocked(spend.total) {
            return ReviewOutcome::Unavailable(exhausted);
        }
        self.step("review", format!("{} tier", reviewer_tier.as_str()));
        let Some(profile) = request.authority.models.get(&reviewer_tier).cloned() else {
            return ReviewOutcome::Unavailable(format!(
                "no reviewer model configured at the {} tier",
                reviewer_tier.as_str()
            ));
        };
        // "A separate reviewer" (SPEC §10) is separate in fact, not just
        // in dispatch: when the model that wrote the candidate is the one
        // reviewing it — an escalated run, or a recipe that put the
        // escalation model on the worker's tier — asking it for findings
        // asks it about its own work. The review still happens — a second
        // opinion from the same model is worth more than none — but the
        // run records that it was not independent (audit B14).
        if same_model {
            if let Err(e) = self.transition(
                State::Verifying,
                Reason::ReviewerSameModel,
                serde_json::json!({
                    "reviewer_same_model": true,
                    "model": profile.id,
                    "tier": reviewer_tier.as_str(),
                    "candidate": request.candidate_sha,
                }),
            ) {
                return ReviewOutcome::Unavailable(format!(
                    "the ledger refused the reviewer-tier record: {e}"
                ));
            }
        }
        let review_dir = self.review_dir();
        let (prompt, reviewer_gap) = self.review_prompt(request, &review_dir);
        if let Some(gap) = &reviewer_gap {
            self.emit_guidance_gap(gap);
        }
        self.reviewer_gap = reviewer_gap;
        let seat = ReviewSeat {
            profile: &profile,
            tier: reviewer_tier,
            dir: review_dir,
            purpose: ReviewPurpose::Patch,
        };
        let result = match self.dispatch_reviewer(request, seat, prompt, spend) {
            Ok(result) => result,
            Err(unavailable) => return unavailable,
        };
        let text = result.result_text.unwrap_or_default();
        // The artifact and its evidence row are one operation: a review
        // the run cannot record is a review it cannot show, and a run
        // that cannot show its review does not accept (X5).
        if let Err(e) = self.record_artifact(
            None,
            EvidenceKind::ReviewResult,
            &self.artifacts.join("review.txt"),
            &text,
        ) {
            return ReviewOutcome::Unavailable(format!(
                "the review could not be recorded as evidence: {e}"
            ));
        }
        review_verdict(&text)
    }

    /// What the reviewer is asked. Every piece of project text — the
    /// objective, the criteria, the constraints, the changed paths — is
    /// quoted as data, never as instructions (SPEC §16).
    fn review_prompt(
        &self,
        request: &ReviewRequest<'_>,
        review_dir: &Path,
    ) -> (String, Option<GuidanceGap>) {
        let mut prompt = String::from(
            "You are a semantic reviewer. You cannot edit or waive checks; you report findings only.\n\
             Report each finding with file/range, the violated acceptance criterion, evidence, and suggested verification.\n\
             You may only run read-only commands (reading files, git log/diff/show, searching). \
             Do not build, test or run checks: the runner has run the profile's checks already, \
             and a command you could not run is not a finding.\n\
             For every fact this change RECORDS, ask where else that same fact is already \
             written — a ledger row and a file on disk, a run's state and its receipt, a \
             transition and a column derived from it — and whether the change keeps them in \
             step. A test that exercises only the in-memory value passes while the stored \
             copies disagree, so the passing checks above are not evidence about this.\n\
             Ask the same of what it READS: a record appended after a run is already terminal \
             (a retired worktree writes a same-state transition) means the NEWEST row matching \
             a state is usually not the event that caused it.\n",
        );
        prompt.push('\n');
        prompt.push_str(&data_block("objective", &self.config.contract.objective));
        prompt.push_str(&data_list_block(
            "acceptance criteria",
            &self.config.contract.acceptance_statements(),
        ));
        if !request.manifest.constraints.is_empty() {
            prompt.push_str(&data_list_block(
                "architectural constraints",
                &request.manifest.constraints,
            ));
        }
        // The reviewer judges the change against the same design the
        // worker was handed.
        if let Some(design) = &request.manifest.design {
            prompt.push_str(&data_block("program design", design));
        }
        // The patch is read first: the constraint statements share its
        // budget, and the patch is what the reviewer cannot do without.
        let patch = read_patch(&request.patch_path, REVIEW_PATCH_BUDGET_BYTES);
        let patch_bytes = patch.as_ref().map_or(0, String::len);
        let (guidance_text, reviewer_gap) = reviewer_guidance(
            &request.manifest.guidance,
            REVIEW_PATCH_BUDGET_BYTES.saturating_sub(patch_bytes),
        );
        prompt.push_str(&guidance_text);
        if !request.verification_inputs_changed.is_empty() {
            prompt.push_str(
                "this candidate CHANGES THE TEST TREE (tests or fixtures the checks execute). \
                 Adding regression tests is expected work; judge whether each change weakens \
                 what the acceptance criteria verify — a deleted, skipped or loosened test is a \
                 finding.\n",
            );
            // Paths come out of the candidate's diff: worker-chosen text,
            // quoted like every other piece the runner did not write.
            prompt.push_str(&data_list_block(
                "changed verification inputs",
                request.verification_inputs_changed,
            ));
        }
        prompt.push_str(&format!("\ncandidate commit: {}\n", request.candidate_sha));
        // The patch travels IN the prompt. A reviewer's agent definition
        // gives it Read, Grep and Glob only — it reports, it does not act —
        // and a path it cannot open is a review that cannot happen: the first time this
        // prompt told a reviewer to read a file, it spent its whole wall
        // clock being refused and answered nothing at all.
        match patch {
            Ok(patch) => prompt.push_str(&data_block("candidate patch", &patch)),
            Err(e) => {
                // Say so in the prompt rather than pretending there is no
                // diff: a reviewer that cannot see the change must report
                // that, and the runner reads the missing verdict as
                // `Unavailable` rather than as approval.
                prompt.push_str(&format!(
                    "\nthe candidate patch could not be read ({e}); review what the \
                     objective and the criteria demand, and report that the diff was \
                     unavailable to you\n"
                ));
            }
        }
        prompt.push_str(&format!("source to inspect: {}\n", review_dir.display()));
        prompt.push_str(
            "\nFinish your answer with exactly one of these, and nothing after it: the line \
             `FINDINGS: none` when there are no findings, or the line `FINDINGS:` followed by \
             the list of findings.\n",
        );
        (prompt, reviewer_gap)
    }

    /// The review as a managed dispatch with its own seat, and the
    /// accounting that follows it: its cost is the run's (SPEC §9, §11).
    /// `Err` carries the `Unavailable` the caller returns — every way a
    /// reviewer fails to produce an answer the runner can record.
    fn dispatch_reviewer(
        &mut self,
        request: &ReviewRequest<'_>,
        seat: ReviewSeat<'_>,
        prompt: String,
        spend: &mut RunSpend,
    ) -> Result<LaunchResult, ReviewOutcome> {
        let ReviewSeat {
            profile,
            tier: reviewer_tier,
            dir: review_dir,
            purpose,
        } = seat;
        // A reviewer the runner cannot even name is a reviewer it
        // cannot dispatch: `Unavailable`, never a silent acceptance.
        let dispatch_id = match self.config.ids.dispatch_id() {
            Ok(id) => id,
            Err(e) => return Err(ReviewOutcome::Unavailable(e.to_string())),
        };
        let remaining_budget = self
            .config
            .machine
            .spending
            .per_run_micros
            .map(|ceiling| ceiling.remaining_after(spend.total).to_micros());
        let spec = LaunchSpec {
            dispatch_id: dispatch_id.as_str().to_string(),
            prompt,
            model: profile.id.clone(),
            effort: profile.effort.clone(),
            max_turns: None,
            budget_micros: remaining_budget,
            disallowed_tools: request.authority.disallowed_tools.clone(),
            work_dir: review_dir,
            // The review is part of acceptance, so it gets a floor of its
            // own rather than whatever the worker left of the run's wall
            // clock: a reviewer handed the one-second remainder is killed
            // before it can answer, and the run ends `needs_review` with
            // the whole attempt's spend already paid.
            wall_timeout: self.budgets.remaining().max(REVIEW_MIN_WALL),
            cancel: None,
            agent: AgentKind::Reviewer,
        };
        let recorded = self.config.ledger.record_dispatch_intent(
            &dispatch_id,
            &self.run_id,
            None,
            &serde_json::json!({
                "model": profile.id,
                "effort": profile.effort,
                "harness": self.harness,
                "tier": reviewer_tier.as_str(),
                "kind": purpose.intent_kind(),
                "recipe_id": request.covering.as_ref().map(|r| &r.recipe_id),
                "recipe_name": request.covering.as_ref().map(|r| &r.name),
                "recipe_revision": request.covering.as_ref().map(|r| r.revision),
            }),
            remaining_budget.unwrap_or(0),
            request.routed_by,
        );
        let recorded = recorded.and_then(|_| self.record_native_row(&dispatch_id, None));
        if let Err(e) = recorded {
            return Err(ReviewOutcome::Unavailable(format!(
                "the ledger refused the review intent: {e}"
            )));
        }
        // A review dispatch ends the run only through `Unavailable`, so
        // the budget it observes with is nominal.
        let budget = Budget {
            attempts_used: 0,
            max_attempts: request.authority.max_attempts,
            repairs_used: 0,
            max_repairs: 0,
            tier: reviewer_tier,
            rung: RungIndex::INITIAL,
            escalation_tier: None,
        };
        // Around the launch, monotonic: the dispatch's own elapsed time.
        let dispatch_start = Instant::now();
        let result = match self.managed_launch(ManagedDispatch {
            spec,
            attempt: None,
            depth: 0,
            parent: None,
            reserve_micros: remaining_budget.unwrap_or(0),
            budget: &budget,
            // The reviewer reads; it writes no worktree and takes no
            // lease.
            write_lease: None,
        }) {
            Ok(Ok(result)) => result,
            Ok(Err(outcome)) => {
                let closed = self
                    .config
                    .ledger
                    .finish_dispatch(&dispatch_id, "launch_failed");
                let detail = match closed {
                    Ok(()) => String::new(),
                    Err(e) => format!("; the ledger also refused the dispatch record: {e}"),
                };
                return Err(ReviewOutcome::Unavailable(format!(
                    "reviewer dispatch ended {}: {}{detail}",
                    outcome.state(),
                    outcome.detail()
                )));
            }
            Err(e) => {
                return Err(ReviewOutcome::Unavailable(format!(
                    "reviewer dispatch failed: {e}"
                )));
            }
        };
        if let Err(e) = self
            .config
            .ledger
            .finish_dispatch(&dispatch_id, "completed")
            .and_then(|()| self.record_native_row(&dispatch_id, result.session_id.as_deref()))
        {
            return Err(ReviewOutcome::Unavailable(format!(
                "the ledger refused the review record: {e}"
            )));
        }
        let event = UsageEvent {
            event_id: dispatch_id.as_str().to_string(),
            run_id: self.run_id.clone(),
            attempt_id: None,
            parent_event_id: None,
            model: result.effective_model.clone(),
            input_tokens: result.usage.input_tokens,
            output_tokens: result.usage.output_tokens,
            cache_read_tokens: result.usage.cache_read_tokens,
            cache_write_tokens: result.usage.cache_write_tokens,
            cost: result.usage.cost.micros(),
            cost_kind: self.usage_cost_kind(),
            completeness: result.usage.cost.completeness(),
            inclusive: result.usage.cost.inclusive(),
            at: self.config.ledger.now(),
            phase: Some(purpose.usage_phase()),
            duration_ms: Some(dispatch_start.elapsed().as_millis() as i64),
            requested_model: Some(profile.id.clone()),
            requested_effort: effort_str(profile.effort.as_ref()),
            harness: self.harness.clone(),
        };
        if let Err(e) = self.book_usage(&event, &result) {
            return Err(ReviewOutcome::Unavailable(format!(
                "the ledger refused the review usage: {e}"
            )));
        }
        spend.fold(event.cost, result.usage.cost.completeness());
        if result.terminal_result_missing() {
            return Err(ReviewOutcome::Unavailable(
                "the reviewer ended without a terminal result".into(),
            ));
        }
        Ok(result)
    }
}

/// A route's requested effort, spelled the way a usage event stores it.
/// `None` for a model without effort control — omitted, never guessed.
/// What the pane says of a baseline: how many checks ran and how many
/// were already red on the base.
/// The ledger's clock, read as the budgets' (ADR-0010).
struct LedgerBudgetClock(Arc<dyn crate::ledger::Clock>);

impl BudgetClock for LedgerBudgetClock {
    fn now_millis(&self) -> i64 {
        chrono::DateTime::parse_from_rfc3339(&self.0.now_rfc3339()).map_or_else(
            |_| chrono::Utc::now().timestamp_millis(),
            |at| at.timestamp_millis(),
        )
    }
}

/// The size of each budget for one run: the work budget is the effective
/// `max_wall_seconds` (the repository's, narrowed by `limits.wall_seconds`);
/// the waits come from machine settings; the total is the contract's
/// `limits.total_seconds` or the sum of everything the run may spend
/// (ADR-0010).
fn budget_limits(
    authority: &EffectiveAuthority,
    limits: &crate::contract::Limits,
    resources: &crate::policy::ResourceSettings,
) -> BudgetLimits {
    let profile = &authority.verification_profile;
    let baseline_seconds = profile
        .setup
        .iter()
        .chain(&profile.commands)
        .fold(0u64, |sum, command| {
            sum.saturating_add(command.timeout_seconds)
        });
    BudgetLimits::resolve(
        authority.max_wall_seconds,
        resources.admission_wait_seconds(),
        resources.disk_wait_seconds(),
        limits.total_seconds,
        baseline_seconds,
    )
}

fn baseline_summary(baseline: &Baseline) -> String {
    let red = match baseline.failures.len() {
        0 => "green on base".to_string(),
        n => format!("{n} red on base"),
    };
    match &baseline.checks {
        _ if baseline.cached => format!("cached · {red}"),
        Some(checks) if checks.len() == 1 => format!("1 check · {red}"),
        Some(checks) => format!("{} checks · {red}", checks.len()),
        None => red,
    }
}

/// What a failed setup command says on the run's record: which command,
/// how it ended, where its log is, and where it was tried.
fn setup_failure_detail(failed: &verify::CheckOutcome, where_: &str) -> String {
    format!(
        "the declared setup `{}` did not succeed in {where_} ({}); the profile's commands \
         did not run — see {}",
        failed.argv.join(" "),
        failed.ended.describe(),
        failed.log_path
    )
}

/// The wall limit a profile declared for the command behind this
/// label, if the label still names one of the profile's commands.
fn command_timeout_seconds(profile: &VerificationProfile, label: &str) -> Option<u64> {
    profile
        .commands
        .iter()
        .find(|command| verify::check_label(command) == label)
        .map(|command| command.timeout_seconds)
}

/// What an unrunnable baseline says, and what to do about it. A check
/// that never reported a status splits into two different remedies: a
/// missing program (exit 127) depends on what was declared — with no
/// setup and a lockfile at the root, the missing program is most likely
/// the tree's own dependencies and the block that installs them is
/// spelled out; with a setup that succeeded and a program still missing,
/// adding another install step is the wrong move. A check the wall clock
/// or a signal cut off names the limit it ran against instead, so a
/// reader is pointed at contention or at a limit that is too low, not at
/// a base that is broken.
fn unrunnable_baseline_detail(
    repo_dir: &Path,
    profile_name: &str,
    profile: &VerificationProfile,
    base_sha: &str,
    checks: &[&verify::CheckOutcome],
) -> String {
    let short = base_sha.get(..8).unwrap_or(base_sha);
    let mut not_found_labels: Vec<&str> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    // Told apart because the remedy differs: a wall limit can be raised,
    // a kill cannot.
    let mut timed_out = false;
    let mut cut_off = false;
    for check in checks {
        match check.ended.recorded() {
            Some(Ended::Exited(_)) => {
                not_found_labels.push(&check.label);
                lines.push(format!(
                    "{} exited {} (command not found)",
                    check.label,
                    verify::COMMAND_NOT_FOUND
                ));
            }
            // The wall limit belongs to the ending that hit it. A
            // check the OOM killer took, or one that crashed, did not
            // run out of time, and quoting a limit it never reached
            // sends a reader to raise a number that was not the cause.
            Some(Ended::TimedOut) => {
                timed_out = true;
                let limit = command_timeout_seconds(profile, &check.label)
                    .map(|seconds| format!("{seconds}s wall limit"))
                    .unwrap_or_else(|| "no known wall limit".to_string());
                lines.push(format!("{} timed out ({limit})", check.label));
            }
            Some(Ended::Signalled | Ended::Cancelled) | None => {
                cut_off = true;
                lines.push(format!("{} {}", check.label, check.ended.describe()));
            }
        }
    }
    let mut detail = format!(
        "{} at the base revision {short}: the base has no verdict to compare a candidate \
         against. A verification worktree holds the repository's files and nothing else.",
        lines.join("; ")
    );
    if !not_found_labels.is_empty() {
        detail.push_str(
            " A missing program is not something a worker can put there; the exited-127 \
             check(s) above name it.",
        );
    }
    if timed_out {
        detail.push_str(
            " A check that did not finish before its wall limit is not a failure in the \
             code: either this machine was contended when it ran, or the limit is too low \
             for this check. A timeout that reproduces consistently against this exact base \
             revision, run alone, points at the limit; one that does not points at \
             contention.",
        );
    }
    if cut_off {
        detail.push_str(
            " A check that died without reporting a status did not run out of time: it was \
             killed or it crashed. The out-of-memory killer is the common cause on a \
             contended machine, and the check's own log is where the evidence is.",
        );
    }
    if not_found_labels.is_empty() {
        return detail;
    }
    if profile.setup.is_empty() {
        let found = crate::repo::lockfiles(repo_dir);
        if found.is_empty() {
            detail.push_str(
                " The profile declares no setup; if the commands need dependencies installed \
                 in the tree, declare the install step under \
                 [[verification.profiles.<name>.setup]] in relais.toml (this changes the policy \
                 hash: review it and re-grant trust from `relais plan`).",
            );
        } else {
            let suggestions = found
                .iter()
                .map(|ecosystem| {
                    format!(
                        "{} is present, suggesting:\n{}",
                        ecosystem.lockfile,
                        ecosystem.setup_block(profile_name)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            detail.push_str(&format!(
                " The profile declares no setup and {suggestions}\nA setup step is executable \
                 authority: adding it changes the policy hash, so review it and re-grant trust \
                 from `relais plan`."
            ));
        }
    } else {
        detail.push_str(&format!(
            " The profile's declared setup ({}) succeeded and the program is still not found: \
             the setup does not provide it, or it is not on PATH in a verification worktree. \
             Check what the setup installs rather than adding another install step.",
            profile
                .setup
                .iter()
                .map(|command| command.argv.join(" "))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    detail
}

#[cfg(test)]
mod tests;
