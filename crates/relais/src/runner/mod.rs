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

mod baseline;
mod candidate;
mod config;
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

    /// What a dispatch's usage is: its cost is always tokens times the
    /// price table, known or not, never API spend somebody reported.
    pub(crate) fn usage_cost_kind(&self) -> CostKind {
        CostKind::EstimatedApiEquivalent
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

    /// A step of the run the pane shows (`protocol::Event::Step`).
    pub(crate) fn step(&self, name: &str, detail: String) {
        self.events.emit(Event::Step {
            name: name.to_string(),
            detail,
            max_attempts: None,
        });
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

#[cfg(test)]
mod tests;
