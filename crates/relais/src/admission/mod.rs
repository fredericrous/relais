//! Admission control (SPEC §23): the pure state machine the coordinator
//! serves and the runner consults.
//!
//! Separate limits for active remote model work, local heavy commands,
//! indexing and training; global, per-session and per-run caps; fair
//! scheduling with queue aging so one tab cannot starve the others. Managed
//! dispatch reserves capacity and budget atomically before launch, and a
//! retry with the same dispatch ID cannot create duplicate agents. Parents
//! waiting on children relinquish active-execution capacity so all slots
//! cannot be held by waiters.
//!
//! The state machine touches no socket, no clock and no process table:
//! every method takes `now`, and liveness is a callback. That is what makes
//! the §23 concurrency scenarios testable without three real Claude Code
//! tabs. The gate implementations at the end of the file are the boundary
//! that supplies both.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::policy::ConcurrencyLimits;

/// A queued entry older than this jumps the session round-robin (SPEC
/// §23: queue aging). It never jumps a cap.
pub const AGING_AFTER: Duration = Duration::from_secs(30);

/// A lease without a heartbeat for this long gets its process checked
/// before anything is freed. Expiry alone never proves a worker died
/// (SPEC §23).
pub const LEASE_GRACE: Duration = Duration::from_secs(300);

/// A registered run that has admitted nothing for this long, and has no
/// dispatch or queued request left, is presumed abandoned — its root
/// runner died without cancelling — and stops holding the daemon open.
/// Deliberately far longer than any verification profile (repo policy's
/// `max_wall_seconds` defaults to 1200 s): a run between dispatches is
/// verifying, and reaping it early is exactly the bug that killed
/// in-flight runs. The proper end of a run is `finish_run`.
pub const RUN_ABANDON_GRACE: Duration = Duration::from_secs(3600);

/// A lease that is past grace and has never had a process bound is kept
/// and re-checked for this many grace periods. After that it is
/// provably unbindable — the launcher died between `Granted` and the
/// bind, or never reached `bind` at all — and its seat is freed.
///
/// The seat is real and the worker is not: nothing can ever arrive to
/// claim it, because a bind only happens in the same call that launched
/// the process. Three killed runs used to take half the seats for the
/// life of the daemon (C2).
pub const UNBINDABLE_AFTER: u32 = 3;

/// How long a cancelled worker has to honour `terminate` before the
/// coordinator escalates to a hard kill. One grace period, once; after
/// the kill nothing more is sent (C5: the old reconcile re-sent SIGTERM
/// to the same PID every 15 s, for ever, with no escalation).
pub const CANCEL_ESCALATE_AFTER: Duration = LEASE_GRACE;

/// How many settled dispatch IDs are remembered so a repeat is refused
/// rather than admitted a second time (C8). Bounded and FIFO: a
/// coordinator that runs for days cannot grow this without limit, and
/// forgetting the oldest is what a restart would do anyway.
pub const TERMINAL_MEMORY: usize = 4096;

/// Resource classes are scheduled separately: a lightweight remote
/// research agent does not consume the class a compiler or test
/// container does (SPEC §23).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceClass {
    ModelWork,
    HeavyLocal,
    Indexing,
    Training,
}

impl ResourceClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelWork => "model_work",
            Self::HeavyLocal => "heavy_local",
            Self::Indexing => "indexing",
            Self::Training => "training",
        }
    }

    /// The cap this class is scheduled against.
    fn cap_group(self) -> CapGroup {
        match self {
            Self::ModelWork => CapGroup::Remote,
            Self::HeavyLocal | Self::Indexing => CapGroup::Local,
            Self::Training => CapGroup::Training,
        }
    }
}

/// Classes that share one machine limit are scheduled as ONE group. A
/// cap is a cap on the group, never on each member separately: heavy
/// local commands and indexing both come out of `max_heavy_commands`,
/// and counting them apart admitted twice the configured number (A3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CapGroup {
    /// Remote model work: `max_active_agents`, and the only group with a
    /// per-session cap.
    Remote,
    /// Local machine load — compilers, test containers, index builds:
    /// `max_heavy_commands`.
    Local,
    /// Training jobs: `max_training_jobs`.
    Training,
}

impl CapGroup {
    /// Every group, for the totals a status snapshot reports.
    const ALL: [CapGroup; 3] = [CapGroup::Remote, CapGroup::Local, CapGroup::Training];
}

/// What a root runner registers before its first dispatch. Limits and
/// budget come from the run's effective authority; a later registration
/// of the same run can only narrow them (SPEC §23: a child's self-reported
/// budget or permissions cannot enlarge its grant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRegistration {
    pub run_id: String,
    pub session_id: String,
    /// Root money budget in micro-USD; `None` = no money ceiling.
    pub budget_micros: Option<i64>,
    /// Aggregate invocation cap for the whole run's agent tree.
    pub max_agents: Option<u32>,
    pub max_depth: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchRequest {
    pub dispatch_id: String,
    pub run_id: String,
    pub session_id: String,
    /// The admitted dispatch this one descends from; `None` for a run's
    /// root worker. An unknown parent is recorded as unknown, not guessed.
    pub parent_dispatch: Option<String>,
    /// 0 for a root worker; each managed child is one deeper.
    pub depth: u32,
    pub resource: ResourceClass,
    /// Micro-USD to reserve from the run's root budget BEFORE launch, so
    /// concurrent children cannot spend the same allowance twice. Zero
    /// when the run has no money ceiling.
    pub reserve_micros: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Capacity and budget are reserved; launch now. Exactly one caller
    /// ever sees this for a given dispatch ID.
    Granted,
    /// At a cap; queued fairly. Poll again with the same dispatch ID.
    Queued { position: usize },
    /// This dispatch ID was already granted to a caller: launching again
    /// would duplicate an agent.
    AlreadyAdmitted,
    /// Never admissible under current policy; not queued.
    Refused { code: Refusal, detail: String },
}

/// Why a request is refused outright rather than queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    /// The root runner has not registered this run.
    UnknownRun,
    RunCancelled,
    DepthExceeded,
    /// The run's aggregate agent cap is used up; no package resets it.
    RunAgentCap,
    /// The reservation does not fit the run's remaining root budget.
    BudgetExceeded,
    /// This dispatch ID already ran and settled. Admitting it again
    /// would launch a second agent for work that is over (C8).
    AlreadyFinished,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnknownRun => "unknown_run",
            Self::RunCancelled => "run_cancelled",
            Self::DepthExceeded => "depth_exceeded",
            Self::RunAgentCap => "run_agent_cap",
            Self::BudgetExceeded => "budget_exceeded",
            Self::AlreadyFinished => "already_finished",
        }
    }
}

/// What a heartbeat answers: whether the coordinator still knows the
/// dispatch, and whether its run or subtree was cancelled meanwhile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatStatus {
    pub known: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RunStatus {
    pub session_id: String,
    pub budget_micros: Option<i64>,
    /// Outstanding, unsettled reservations.
    pub reserved_micros: i64,
    /// Settled actual spend, with unknown usage carried at its
    /// reservation as a lower bound.
    pub settled_micros: i64,
    /// Dispatches whose usage settled as unknown: the totals above are a
    /// lower bound, not a figure (SPEC §23).
    pub uncertain_settlements: u32,
    pub admitted_total: u32,
    pub active: u32,
    pub waiting: u32,
    pub queued: u32,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StatusSnapshot {
    pub limits: ConcurrencyLimits,
    pub active_by_class: BTreeMap<String, u32>,
    pub waiting: u32,
    pub queued: u32,
    /// Dispatches whose lease is past grace without a checkable process.
    pub stale_leases: u32,
    pub sessions: Vec<String>,
    pub runs: BTreeMap<String, RunStatus>,
    /// Active seats beyond a cap, from parents that resumed after
    /// waiting: bounded by the number of waiters, by `max_over_admitted`,
    /// and visible.
    pub over_admitted: u32,
    /// Every dispatch with a process bound to it, and how old the
    /// liveness check behind that binding is (A13). Nothing portable
    /// proves a PID still names the process it named at bind time (see
    /// `procs::alive`), so the age of the evidence is what can honestly
    /// be shown — and a signal is only ever sent to a PID that is still
    /// alive now.
    #[serde(default)]
    pub bound_processes: BTreeMap<String, BoundProcess>,
    /// Exclusive write leases by worktree path, each naming its holder
    /// (SPEC §23). Root verification waits for the relevant ones to be
    /// gone.
    #[serde(default)]
    pub write_leases: BTreeMap<String, String>,
}

/// A process bound to a dispatch, and how stale the check that bound it
/// is (C6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundProcess {
    pub pid: u32,
    /// Seconds since this PID was checked alive and recorded. The window
    /// in which the OS could have recycled the number starts there.
    pub bound_for_secs: u64,
}

/// What the caller should send a cancelled worker. The state machine
/// never signals anything itself — it decides, the coordinator sends
/// (`procs::terminate` / `procs::kill`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    /// Ask the worker to stop. Sent once per cancelled dispatch.
    Terminate,
    /// It ignored the request for a whole grace period. Sent once,
    /// and nothing is sent after it.
    Kill,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminate => "terminate",
            Self::Kill => "kill",
        }
    }
}

/// A signal the state machine decided on, for the caller to deliver
/// once it has let go of the admission lock. Signalling under the lock
/// blocks every other connection for the length of a syscall on a
/// process that may be stopped (A6).
///
/// `bound_at` is when that PID was last checked alive: the age of the
/// evidence the signal rests on. The deliverer re-checks liveness before
/// sending, which is the only portable narrowing of the C6 window there
/// is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSignal {
    pub dispatch_id: String,
    pub pid: u32,
    pub signal: Signal,
    pub bound_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReconcileReport {
    /// Leases freed because their process is provably gone.
    pub dropped: Vec<String>,
    /// Past grace with no process to check; kept, flagged.
    pub stale: Vec<String>,
    /// Leases freed because they were past grace with no process bound
    /// for `UNBINDABLE_AFTER` grace periods: nothing can bind them now
    /// (C2). Reported separately from `dropped`, which had a corpse.
    pub unbindable: Vec<String>,
    /// Bound processes of cancelled dispatches that the caller should
    /// signal, and with what; each dispatch appears at most twice in its
    /// life — once to terminate, once to kill.
    pub to_signal: Vec<PendingSignal>,
}

/// What `bind` did. A bind is the one moment the coordinator can check
/// that the PID a client claims is a process that exists (C6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindOutcome {
    Bound,
    /// No such dispatch: nothing was bound.
    UnknownDispatch,
    /// The PID is not a live process. Binding it would make the lease
    /// reconcilable against a process that never existed, or worse,
    /// against whatever the OS gives that number next.
    PidNotAlive,
}

impl BindOutcome {
    pub fn bound(self) -> bool {
        self == Self::Bound
    }
}

/// What `resume` did for a parent whose children finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeOutcome {
    Resumed,
    UnknownDispatch,
    /// Resuming would take the class more than `max_over_admitted` seats
    /// beyond its cap. The parent stays waiting and polls again; the
    /// alternative — granting every resume — is how a cap becomes
    /// advisory under enough nesting (C8).
    OverAdmitted {
        over: u32,
        max: u32,
    },
}

impl ResumeOutcome {
    pub fn resumed(self) -> bool {
        self == Self::Resumed
    }
}

/// What `mark_waiting` did. A seat is given back on a claim the
/// coordinator can at least partly check, and never on a bare word (C8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitOutcome {
    Waiting,
    UnknownDispatch,
    /// Nothing is bound, or the bound process is gone: it is not
    /// waiting, it is over.
    NoLiveProcess,
    /// The dispatch has already reached an end — its seat is back, or
    /// its usage has settled — so there is nothing to relinquish.
    Ended,
}

impl WaitOutcome {
    pub fn waiting(self) -> bool {
        self == Self::Waiting
    }
}

/// What a lifecycle call — `release`, `settle`, `acknowledge_cancel`,
/// `finish_run` — did. Either it applied to the subject it named, or the
/// coordinator has no such subject; there is no partial application, and
/// duplicate lifecycle events are harmless (SPEC §23).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOutcome {
    Applied,
    /// No such dispatch or run. Worth knowing: after a coordinator
    /// re-election this is what a worker's own lease looks like.
    Unknown,
}

impl LifecycleOutcome {
    pub fn applied(self) -> bool {
        self == Self::Applied
    }
}

/// What `withdraw` did with a request its caller gave up on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithdrawOutcome {
    /// Dropped from the queue, or — for an entry drained in while the
    /// caller was polling — freed with nothing admitted and nothing
    /// spent (A14).
    Withdrawn,
    /// The caller already received `Granted` for this ID: its launch may
    /// be in flight, so abandoning the reservation would leave a worker
    /// running against nothing.
    Claimed,
    UnknownDispatch,
}

/// What `acquire_write` did (SPEC §23: one writer per worktree).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WriteLeaseOutcome {
    Taken,
    /// Somebody else is writing that tree, and is named: the caller must
    /// not launch a second writer into it.
    HeldBy {
        holder: String,
    },
}

impl WriteLeaseOutcome {
    pub fn taken(&self) -> bool {
        matches!(self, Self::Taken)
    }
}

/// What `release_write` did. Only a lease's holder can release it:
/// silently freeing somebody else's is how two writers end up in one
/// worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseWriteOutcome {
    Released,
    /// Nobody holds that worktree, or somebody else does.
    NotTheHolder,
}

/// What a gate can actually enforce, for reports (SPEC §23: observed-only
/// paths are labelled, never claimed as guarantees).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// One process holds the state: caps bind within this process, and
    /// say nothing about a second tab.
    InProcess,
    /// The shared per-user coordinator: caps bind across every tab of
    /// this OS user.
    Coordinator,
}

impl Enforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "managed (in-process)",
            Self::Coordinator => "managed (coordinator)",
        }
    }
}

impl std::fmt::Display for Enforcement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a dispatch is between admission and oblivion.
///
/// The two ends — the seat coming back and the usage settling — arrive
/// in either order and independently, so they are one state with two
/// spellings rather than two booleans: a dispatch that has reached BOTH
/// is removed, which is why "released and settled" has no variant here.
/// Every combination the old eight booleans could spell but never meant
/// (`released && !claimed`, `settled && !released`, `waiting` with no
/// moment) is now unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lifecycle {
    /// Admitted — drained from the queue — but no caller has received
    /// `Granted` for it yet. It holds a seat and can still be withdrawn.
    Unclaimed,
    /// A caller holds the grant and the seat.
    Claimed,
    /// Awaiting children, with the seat given back (SPEC §23). A waiting
    /// parent is a claim the coordinator cannot verify — see
    /// `mark_waiting` — so the moment it was made is kept and shown.
    Waiting { since: Instant },
    /// The seat is back; the reservation stands until usage settles.
    SeatFreed,
    /// Usage has settled; the seat is held until the caller frees it.
    UsageSettled,
}

impl Lifecycle {
    /// Does this dispatch occupy a seat in its cap group?
    fn holds_seat(self) -> bool {
        match self {
            Self::Unclaimed | Self::Claimed | Self::UsageSettled => true,
            Self::Waiting { .. } | Self::SeatFreed => false,
        }
    }

    /// Has a caller been told to launch?
    fn claimed(self) -> bool {
        match self {
            Self::Unclaimed => false,
            Self::Claimed | Self::Waiting { .. } | Self::SeatFreed | Self::UsageSettled => true,
        }
    }

    fn waiting(self) -> bool {
        matches!(self, Self::Waiting { .. })
    }

    fn released(self) -> bool {
        matches!(self, Self::SeatFreed)
    }

    fn settled(self) -> bool {
        matches!(self, Self::UsageSettled)
    }
}

/// How far a cancellation has got. The ladder is walked once and only
/// forwards, so `kill_sent && !terminate_sent` — and a cancellation
/// moment on a dispatch nobody cancelled — cannot be spelled (C5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cancellation {
    None,
    /// Cancelled at `since`; nothing has been handed to the caller yet.
    Requested {
        since: Instant,
    },
    /// A `Terminate` was handed over. The escalation grace runs from
    /// `since` — when the cancellation happened, not when the signal
    /// went — so a slow reconcile cannot extend it.
    Terminated {
        since: Instant,
    },
    /// The escalation was handed over. Nothing more will be.
    Killed {
        since: Instant,
    },
}

impl Cancellation {
    fn cancelled(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// What the coordinator knows about a dispatch's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Binding {
    /// No process bound. `rounds` counts consecutive reconciles that
    /// found the lease past grace with nothing to check (C2).
    Unbound { rounds: u32 },
    /// A PID that was a live process when it was bound. The window in
    /// which the OS could have recycled the number starts at `at`, and
    /// nothing portable closes it (see `procs::alive`).
    Bound { pid: u32, at: Instant },
}

impl Binding {
    fn pid(self) -> Option<u32> {
        match self {
            Self::Unbound { .. } => None,
            Self::Bound { pid, .. } => Some(pid),
        }
    }
}

/// Where a dispatch sits in its run's agent tree.
///
/// `Option<Option<(String, bool)>>` said the same thing and no caller
/// could read it: three states spelled as two nested `Option`s and a
/// flag (`types.variants-as-unions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parentage {
    /// It named no parent: the run's first agent.
    Root,
    /// It named a parent this coordinator holds, so the depth and the
    /// budget it inherits are evidence.
    Known { parent: String },
    /// It named a parent this coordinator has never seen — an adopted
    /// orphan, or a parent that settled before the child bound. The
    /// depth it claims cannot be checked against anything.
    Unrecognised { parent: String },
}

#[derive(Debug, Clone)]
struct Dispatch {
    session_id: String,
    run_id: String,
    parent: Option<String>,
    parent_known: bool,
    depth: u32,
    class: ResourceClass,
    reserved: i64,
    lifecycle: Lifecycle,
    binding: Binding,
    cancellation: Cancellation,
    agent_id: Option<String>,
    last_heartbeat: Instant,
}

/// One worktree's exclusive write lease (SPEC §23: "concurrent writers
/// need separate worktrees or explicitly non-overlapping write leases;
/// scope checks alone are not filesystem isolation").
#[derive(Debug, Clone, PartialEq, Eq)]
struct WriteLease {
    holder: String,
    since: Instant,
}

#[derive(Debug, Clone)]
struct Run {
    session_id: String,
    budget: Option<i64>,
    settled: i64,
    uncertain: u32,
    admitted_total: u32,
    max_agents: Option<u32>,
    max_depth: Option<u32>,
    cancelled: bool,
    /// The run reached an end state — cancelled, or reported finished by
    /// its root runner. Only a terminal run lets the daemon idle-exit; a
    /// registered, non-terminal run with no dispatch in flight is a run
    /// between dispatches (verifying), not an idle coordinator.
    terminal: bool,
    /// Last registration, admission or settlement for this run. The
    /// backstop for a root runner that died without cancelling.
    last_activity: Instant,
}

#[derive(Debug, Clone)]
struct Session {
    last_served: Option<Instant>,
}

#[derive(Debug, Clone)]
struct Queued {
    request: DispatchRequest,
    enqueued_at: Instant,
}

/// The whole admission state for one coordinator. Single-threaded by
/// construction; the coordinator wraps it in a mutex.
#[derive(Debug)]
pub struct AdmissionState {
    limits: ConcurrencyLimits,
    sessions: BTreeMap<String, Session>,
    runs: BTreeMap<String, Run>,
    dispatches: BTreeMap<String, Dispatch>,
    queue: Vec<Queued>,
    /// Dispatch IDs that reached a terminal state, newest last, capped
    /// at `TERMINAL_MEMORY`. Kept as a queue for the eviction order and
    /// a set for the lookup; the two always hold the same IDs.
    finished_order: VecDeque<String>,
    finished: BTreeSet<String>,
    /// Exclusive write leases by worktree path (SPEC §23).
    write_leases: BTreeMap<String, WriteLease>,
    /// Ceiling on seats held beyond a class cap by resumed parents.
    max_over_admitted: Option<u32>,
}

impl AdmissionState {
    pub fn new(limits: ConcurrencyLimits) -> Self {
        Self {
            // A resumed parent may overshoot its class cap; by default no
            // more than one cap's worth of seats over, which keeps the
            // published limit a limit rather than a suggestion.
            max_over_admitted: limits.max_active_agents,
            limits,
            sessions: BTreeMap::new(),
            runs: BTreeMap::new(),
            dispatches: BTreeMap::new(),
            queue: Vec::new(),
            finished_order: VecDeque::new(),
            finished: BTreeSet::new(),
            write_leases: BTreeMap::new(),
        }
    }

    pub fn limits(&self) -> &ConcurrencyLimits {
        &self.limits
    }

    /// Configure the over-admission ceiling. `None` = no ceiling (the
    /// pre-C8 behaviour: every resume is granted). Machine settings own
    /// this; `new` defaults it to `max_active_agents`.
    pub fn set_max_over_admitted(&mut self, max: Option<u32>) {
        self.max_over_admitted = max;
    }

    pub fn max_over_admitted(&self) -> Option<u32> {
        self.max_over_admitted
    }

    /// Idempotent (SPEC §23: joins/resumes register idempotently).
    pub fn register_session(&mut self, session_id: &str) {
        self.sessions
            .entry(session_id.to_string())
            .or_insert(Session { last_served: None });
    }

    /// Idempotent; a repeat can only narrow limits and budget. Takes the
    /// clock like every other mutator: the abandonment backstop is timed
    /// from `last_activity`, and a state machine that reads the clock
    /// itself cannot have that backstop tested (A10).
    pub fn register_run(&mut self, registration: &RunRegistration, now: Instant) {
        self.register_session(&registration.session_id);
        match self.runs.get_mut(&registration.run_id) {
            Some(run) => {
                run.budget = min_opt_i64(run.budget, registration.budget_micros);
                run.max_agents = min_opt(run.max_agents, registration.max_agents);
                run.max_depth = min_opt(run.max_depth, registration.max_depth);
                // A resumed run is live again, whatever it was before.
                run.terminal = false;
                run.last_activity = now;
            }
            None => {
                self.runs.insert(
                    registration.run_id.clone(),
                    Run {
                        session_id: registration.session_id.clone(),
                        budget: registration.budget_micros,
                        settled: 0,
                        uncertain: 0,
                        admitted_total: 0,
                        max_agents: registration.max_agents,
                        max_depth: registration.max_depth,
                        cancelled: false,
                        terminal: false,
                        last_activity: now,
                    },
                );
            }
        }
    }

    /// Reserve capacity and budget atomically, or queue, or refuse. The
    /// first call that finds the dispatch admitted returns `Granted`
    /// exactly once, whether it was admitted directly or drained from
    /// the queue; every later call returns `AlreadyAdmitted`.
    pub fn request(&mut self, request: &DispatchRequest, now: Instant) -> Decision {
        // Idempotency does not end at settlement (C8): a dispatch ID that
        // already ran is refused, not re-admitted. `AlreadyAdmitted` is
        // the answer while the entry lives; once it is gone, the entry
        // cannot say so, and the terminal set does.
        if self.finished.contains(&request.dispatch_id) {
            return Decision::Refused {
                code: Refusal::AlreadyFinished,
                detail: format!(
                    "dispatch {} already ran and settled; re-admitting it would launch a second \
                     agent for finished work (the last {} settled IDs are remembered)",
                    request.dispatch_id, TERMINAL_MEMORY
                ),
            };
        }
        if let Some(existing) = self.dispatches.get(&request.dispatch_id) {
            if existing.lifecycle.claimed() {
                return Decision::AlreadyAdmitted;
            }
            // Drained from the queue while the caller was polling. The
            // hard limits were checked when it queued and are NOT
            // re-checked here, so cancellation has to be (A4): between
            // the drain and this poll the run can have been cancelled,
            // and handing out `Granted` would launch a worker for a run
            // that is over.
            let run_cancelled = self
                .runs
                .get(&existing.run_id)
                .is_some_and(|run| run.cancelled);
            if run_cancelled || existing.cancellation.cancelled() {
                // It was never claimed, so nothing can be in flight: give
                // the seat and the reservation back now rather than
                // waiting out lease grace on a worker that never existed.
                self.discard_unclaimed(&request.dispatch_id, now);
                return Decision::Refused {
                    code: Refusal::RunCancelled,
                    detail: format!(
                        "dispatch {} was cancelled while it waited for a seat; nothing is launched",
                        request.dispatch_id
                    ),
                };
            }
            if let Some(existing) = self.dispatches.get_mut(&request.dispatch_id) {
                existing.lifecycle = Lifecycle::Claimed;
            }
            return Decision::Granted;
        }
        if let Some(position) = self
            .queue
            .iter()
            .position(|queued| queued.request.dispatch_id == request.dispatch_id)
        {
            // Still queued: try to drain (a slot may have opened), then
            // answer with the current position.
            self.drain(now);
            if self.dispatches.contains_key(&request.dispatch_id) {
                return self.request(request, now);
            }
            return Decision::Queued {
                position: self
                    .queue
                    .iter()
                    .position(|queued| queued.request.dispatch_id == request.dispatch_id)
                    .unwrap_or(position)
                    + 1,
            };
        }
        if let Some(refusal) = self.check_hard_limits(request) {
            return refusal;
        }
        self.register_session(&request.session_id);
        if self.admissible(request) {
            self.admit(request.clone(), now, Lifecycle::Claimed);
            return Decision::Granted;
        }
        self.queue.push(Queued {
            request: request.clone(),
            enqueued_at: now,
        });
        Decision::Queued {
            position: self.queue.len(),
        }
    }

    /// Limits that never queue: an unknown run, depth, the run's
    /// aggregate agent cap, and the money budget.
    fn check_hard_limits(&self, request: &DispatchRequest) -> Option<Decision> {
        let Some(run) = self.runs.get(&request.run_id) else {
            return Some(Decision::Refused {
                code: Refusal::UnknownRun,
                detail: format!(
                    "run {} is not registered; the root runner registers before dispatch",
                    request.run_id
                ),
            });
        };
        if run.cancelled {
            return Some(Decision::Refused {
                code: Refusal::RunCancelled,
                detail: format!("run {} was cancelled", request.run_id),
            });
        }
        let max_depth = min_opt(run.max_depth, self.limits.max_agent_depth);
        let depth = self.effective_depth(request);
        if max_depth.is_some_and(|max| depth > max) {
            return Some(Decision::Refused {
                code: Refusal::DepthExceeded,
                detail: format!(
                    "depth {} exceeds the effective maximum {}",
                    depth,
                    max_depth.unwrap_or(0)
                ),
            });
        }
        let queued_for_run = self
            .queue
            .iter()
            .filter(|queued| queued.request.run_id == request.run_id)
            .count() as u32;
        let max_agents = min_opt(run.max_agents, self.limits.max_agents_per_run);
        if max_agents.is_some_and(|max| run.admitted_total + queued_for_run >= max) {
            return Some(Decision::Refused {
                code: Refusal::RunAgentCap,
                detail: format!(
                    "run {} has used its aggregate agent cap ({}); no work package resets it",
                    request.run_id,
                    max_agents.unwrap_or(0)
                ),
            });
        }
        // `reserve_micros` arrives from the socket: a client can send any
        // i64. A negative reservation would credit the run instead of
        // debiting it, so it is refused rather than clamped silently.
        if request.reserve_micros < 0 {
            return Some(Decision::Refused {
                code: Refusal::BudgetExceeded,
                detail: format!(
                    "reserve_micros {} is negative; a reservation debits the run budget and is never below zero",
                    request.reserve_micros
                ),
            });
        }
        if let Some(budget) = run.budget {
            let committed = run
                .settled
                .saturating_add(self.outstanding_reservations(&request.run_id));
            let reserve = request.reserve_micros;
            // Saturating throughout: `i64::MAX` off the wire wraps in
            // release and panics in debug, which poisons the mutex and
            // takes the coordinator down for every later connection.
            if committed.saturating_add(reserve) > budget {
                return Some(Decision::Refused {
                    code: Refusal::BudgetExceeded,
                    detail: format!(
                        "reserving {reserve} on top of {committed} committed exceeds the run budget {budget}"
                    ),
                });
            }
        }
        None
    }

    /// The depth to enforce. `request.depth` is a self-report; when the
    /// named parent is a dispatch this coordinator admitted, its recorded
    /// depth plus one is the fact, and a child claiming 0 under a deep
    /// parent does not reset the cap. With no parent, or a parent this
    /// coordinator never saw, the self-report is all there is.
    fn effective_depth(&self, request: &DispatchRequest) -> u32 {
        request
            .parent_dispatch
            .as_deref()
            .and_then(|parent| self.depth_of(parent))
            .map_or(request.depth, |parent_depth| parent_depth.saturating_add(1))
    }

    fn outstanding_reservations(&self, run_id: &str) -> i64 {
        self.dispatches
            .values()
            .filter(|dispatch| dispatch.run_id == run_id)
            .fold(0i64, |total, dispatch| {
                total.saturating_add(dispatch.reserved)
            })
    }

    /// Seats held in a cap GROUP: every class the cap covers, counted
    /// together. Counting one class at a time let `heavy_local` and
    /// `indexing` each fill `max_heavy_commands` on their own, and the
    /// machine ran twice the configured load (A3).
    fn active_count(&self, group: CapGroup, session: Option<&str>) -> u32 {
        self.dispatches
            .values()
            .filter(|dispatch| {
                dispatch.class.cap_group() == group
                    && dispatch.lifecycle.holds_seat()
                    && session.is_none_or(|session| dispatch.session_id == session)
            })
            .count() as u32
    }

    fn group_cap(&self, group: CapGroup) -> Option<u32> {
        match group {
            CapGroup::Remote => self.limits.max_active_agents,
            CapGroup::Local => self.limits.max_heavy_commands,
            CapGroup::Training => self.limits.max_training_jobs,
        }
    }

    /// Capacity caps only; hard limits were checked before queueing.
    fn admissible(&self, request: &DispatchRequest) -> bool {
        let group = request.resource.cap_group();
        if self
            .group_cap(group)
            .is_some_and(|cap| self.active_count(group, None) >= cap)
        {
            return false;
        }
        if group == CapGroup::Remote {
            if let Some(per_session) = self.limits.max_active_agents_per_session {
                if self.active_count(group, Some(&request.session_id)) >= per_session {
                    return false;
                }
            }
        }
        true
    }

    fn admit(&mut self, request: DispatchRequest, now: Instant, lifecycle: Lifecycle) {
        let parent_known = request
            .parent_dispatch
            .as_ref()
            .is_none_or(|parent| self.dispatches.contains_key(parent));
        // Record the derived depth, not the self-report: a grandchild
        // derives from what is recorded here.
        let depth = self.effective_depth(&request);
        if let Some(run) = self.runs.get_mut(&request.run_id) {
            run.admitted_total += 1;
            run.terminal = false;
            run.last_activity = now;
        }
        if let Some(session) = self.sessions.get_mut(&request.session_id) {
            session.last_served = Some(now);
        }
        self.dispatches.insert(
            request.dispatch_id.clone(),
            Dispatch {
                session_id: request.session_id,
                run_id: request.run_id,
                parent: request.parent_dispatch,
                parent_known,
                depth,
                class: request.resource,
                reserved: request.reserve_micros.max(0),
                lifecycle,
                binding: Binding::Unbound { rounds: 0 },
                cancellation: Cancellation::None,
                agent_id: None,
                last_heartbeat: now,
            },
        );
    }

    /// Drop a dispatch that was admitted but never claimed, giving back
    /// everything it took: the seat, the reservation and the agent-tree
    /// slot. Nothing settles and nothing is remembered as finished,
    /// because nothing ran — a withdrawn request that consumed a run's
    /// agent cap for ever was A14.
    fn discard_unclaimed(&mut self, dispatch_id: &str, now: Instant) {
        let Some(dispatch) = self.dispatches.remove(dispatch_id) else {
            return;
        };
        if let Some(run) = self.runs.get_mut(&dispatch.run_id) {
            run.admitted_total = run.admitted_total.saturating_sub(1);
            run.last_activity = now;
        }
        self.write_leases
            .retain(|_, lease| lease.holder != dispatch_id);
        self.drain(now);
    }

    /// Fair drain: among admissible queued entries, an aged entry goes
    /// first; otherwise the session served least recently, then queue
    /// order. Caps are never jumped — aging reorders, it does not
    /// over-admit.
    fn drain(&mut self, now: Instant) {
        loop {
            let mut candidates: Vec<(bool, Option<Instant>, Instant, usize)> = self
                .queue
                .iter()
                .enumerate()
                .filter(|(_, queued)| self.admissible(&queued.request))
                .map(|(index, queued)| {
                    let aged = now.saturating_duration_since(queued.enqueued_at) >= AGING_AFTER;
                    let last_served = self
                        .sessions
                        .get(&queued.request.session_id)
                        .and_then(|session| session.last_served);
                    (!aged, last_served, queued.enqueued_at, index)
                })
                .collect();
            if candidates.is_empty() {
                return;
            }
            candidates.sort();
            let index = candidates[0].3;
            let queued = self.queue.remove(index);
            // Re-check the hard limits at admission time: the budget may
            // have moved while the entry waited.
            if let Some(Decision::Refused { .. }) = self.check_hard_limits(&queued.request) {
                continue;
            }
            self.admit(queued.request, now, Lifecycle::Unclaimed);
        }
    }

    /// Bind the harness agent/session identity and process to the
    /// reservation once the launch happened (SPEC §23).
    ///
    /// The PID arrives over a socket, from a client that is trusted to
    /// name its own child and nothing else. It is checked against the
    /// process table here (C6): binding a PID that is not running would
    /// give the lease a reconciliation target that is either nothing at
    /// all or, once the OS reuses the number, an unrelated process the
    /// coordinator would later terminate as a cancelled worker.
    ///
    /// What this does NOT prove: that the live PID is the client's own
    /// child, or that it is still the same process later in the lease.
    /// Both need a process start time, which has no portable API (see
    /// `procs::alive`). So the moment the check was true is recorded,
    /// reported in `StatusSnapshot::bound_processes` and carried on every
    /// `PendingSignal`: the window cannot be closed, but its age is on
    /// the record rather than implied (A13).
    pub fn bind(
        &mut self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
        now: Instant,
        alive: &dyn Fn(u32) -> bool,
    ) -> BindOutcome {
        if !self.dispatches.contains_key(dispatch_id) {
            return BindOutcome::UnknownDispatch;
        }
        if let Some(pid) = pid {
            if !alive(pid) {
                return BindOutcome::PidNotAlive;
            }
        }
        self.bind_checked(dispatch_id, agent_id, pid, now);
        BindOutcome::Bound
    }

    /// The bind itself, for a PID this coordinator already established
    /// is live: adoption from the ledger, which checks liveness before
    /// adopting at all.
    fn bind_checked(
        &mut self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
        now: Instant,
    ) -> bool {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return false;
        };
        if agent_id.is_some() {
            dispatch.agent_id = agent_id.map(str::to_string);
        }
        if let Some(pid) = pid {
            dispatch.binding = Binding::Bound { pid, at: now };
        }
        true
    }

    pub fn heartbeat(&mut self, dispatch_id: &str, now: Instant) -> HeartbeatStatus {
        match self.dispatches.get_mut(dispatch_id) {
            Some(dispatch) => {
                dispatch.last_heartbeat = now;
                // Somebody is alive on the other end of this lease, so
                // the unbindable count starts over (C2).
                if let Binding::Unbound { rounds } = &mut dispatch.binding {
                    *rounds = 0;
                }
                let run_cancelled = self
                    .runs
                    .get(&dispatch.run_id)
                    .is_some_and(|run| run.cancelled);
                HeartbeatStatus {
                    known: true,
                    cancelled: dispatch.cancellation.cancelled() || run_cancelled,
                }
            }
            None => HeartbeatStatus {
                known: false,
                cancelled: false,
            },
        }
    }

    /// A parent blocked awaiting its children gives its seat back so the
    /// children can run (SPEC §23: no all-waiters deadlock).
    ///
    /// SPEC §23 relinquishes the seat "where waiting can be reliably
    /// observed". It cannot be, here: no harness reports "this agent is
    /// blocked on a child", and a self-report over a socket is a claim,
    /// not an observation — a client that says "waiting" while it keeps
    /// burning a core gets its seat back for free. What IS checkable is
    /// that the claimant is a real, live, bound process, so that is what
    /// is required (C8): a dispatch with no bound PID, or one whose PID
    /// is gone, cannot mark itself waiting. The moment of the claim is
    /// recorded in `waiting_since` and shown in status, so a "waiting"
    /// parent that never resumes is visible rather than free.
    pub fn mark_waiting(
        &mut self,
        dispatch_id: &str,
        now: Instant,
        alive: &dyn Fn(u32) -> bool,
    ) -> WaitOutcome {
        let Some(dispatch) = self.dispatches.get(dispatch_id) else {
            return WaitOutcome::UnknownDispatch;
        };
        let Some(pid) = dispatch.binding.pid() else {
            return WaitOutcome::NoLiveProcess;
        };
        if !alive(pid) {
            return WaitOutcome::NoLiveProcess;
        }
        // A dispatch that already gave its seat back, or whose seat is
        // gone for good, has nothing to relinquish.
        match dispatch.lifecycle {
            Lifecycle::Unclaimed | Lifecycle::Claimed => {}
            Lifecycle::Waiting { .. } => return WaitOutcome::Waiting,
            Lifecycle::SeatFreed | Lifecycle::UsageSettled => return WaitOutcome::Ended,
        }
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return WaitOutcome::UnknownDispatch;
        };
        dispatch.lifecycle = Lifecycle::Waiting { since: now };
        dispatch.last_heartbeat = now;
        self.drain(now);
        WaitOutcome::Waiting
    }

    /// The waiting parent's children finished: it takes its seat back
    /// immediately, rather than re-queueing behind the work it is
    /// waiting on — which is what would deadlock.
    ///
    /// That overshoots the class cap by design, and the overshoot is
    /// bounded twice over: by the number of waiters, and by
    /// `max_over_admitted` (default: one cap's worth), beyond which the
    /// resume is REFUSED and the parent stays waiting (C8). Status shows
    /// `over_admitted` either way.
    pub fn resume(&mut self, dispatch_id: &str, now: Instant) -> ResumeOutcome {
        let Some(dispatch) = self.dispatches.get(dispatch_id) else {
            return ResumeOutcome::UnknownDispatch;
        };
        if dispatch.lifecycle.waiting() {
            let group = dispatch.class.cap_group();
            let over = self.group_cap(group).map_or(0, |cap| {
                self.active_count(group, None)
                    .saturating_add(1)
                    .saturating_sub(cap)
            });
            if let Some(max) = self.max_over_admitted {
                if over > max {
                    return ResumeOutcome::OverAdmitted { over, max };
                }
            }
        }
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return ResumeOutcome::UnknownDispatch;
        };
        // A dispatch that is not waiting has nothing to take back, and a
        // duplicate resume is harmless (SPEC §23). One that has already
        // ended keeps its end.
        if dispatch.lifecycle.waiting() {
            dispatch.lifecycle = Lifecycle::Claimed;
        }
        dispatch.last_heartbeat = now;
        ResumeOutcome::Resumed
    }

    /// Free the seat. The reservation stays until settled, so a release
    /// before settlement cannot let a sibling spend the same money; the
    /// entry itself goes when both ends have arrived.
    pub fn release(&mut self, dispatch_id: &str, now: Instant) -> LifecycleOutcome {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return LifecycleOutcome::Unknown;
        };
        // Usage already settled: this is the second end, so the entry is
        // over once the seat is back.
        let both_ends = dispatch.lifecycle.settled();
        dispatch.lifecycle = Lifecycle::SeatFreed;
        if both_ends {
            self.forget_settled(dispatch_id);
        }
        self.drain(now);
        LifecycleOutcome::Applied
    }

    /// Settle a reservation with actual spend. `None` = unknown usage:
    /// the reservation stands in as a lower bound and the run is marked
    /// uncertain; never zero (SPEC §23).
    pub fn settle(
        &mut self,
        dispatch_id: &str,
        spent_micros: Option<i64>,
        now: Instant,
    ) -> LifecycleOutcome {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return LifecycleOutcome::Unknown;
        };
        if dispatch.lifecycle.settled() {
            // Duplicate lifecycle events are harmless (SPEC §23).
            return LifecycleOutcome::Applied;
        }
        // The seat is already back: settling is the second end, so the
        // entry goes once the money is booked.
        let both_ends = dispatch.lifecycle.released();
        dispatch.lifecycle = Lifecycle::UsageSettled;
        let reserved = std::mem::take(&mut dispatch.reserved);
        let run_id = dispatch.run_id.clone();
        if let Some(run) = self.runs.get_mut(&run_id) {
            // Saturating: `spent_micros` is reported by the caller and
            // `reserved` came off the wire; a total that wraps would read
            // as a run that has spent nothing.
            match spent_micros {
                Some(spent) => run.settled = run.settled.saturating_add(spent.max(0)),
                None => {
                    run.settled = run.settled.saturating_add(reserved.max(0));
                    run.uncertain += 1;
                }
            }
            run.last_activity = now;
        }
        if both_ends {
            self.forget_settled(dispatch_id);
        }
        self.drain(now);
        LifecycleOutcome::Applied
    }

    /// The caller gave up on a request it never launched: drop it from
    /// the queue, or, if it was drained meanwhile but never claimed,
    /// give back everything it took. A claimed dispatch is not
    /// withdrawable — its launch may be in flight.
    ///
    /// A withdrawn request never ran, so it is not a settlement and not
    /// a finished dispatch ID: it does not consume one of the run's
    /// agent-tree slots, and the same ID may be requested again (A14).
    pub fn withdraw(&mut self, dispatch_id: &str, now: Instant) -> WithdrawOutcome {
        let before = self.queue.len();
        self.queue
            .retain(|queued| queued.request.dispatch_id != dispatch_id);
        if self.queue.len() != before {
            return WithdrawOutcome::Withdrawn;
        }
        match self.dispatches.get(dispatch_id) {
            None => WithdrawOutcome::UnknownDispatch,
            Some(dispatch) if dispatch.lifecycle.claimed() => WithdrawOutcome::Claimed,
            Some(_) => {
                self.discard_unclaimed(dispatch_id, now);
                WithdrawOutcome::Withdrawn
            }
        }
    }

    /// Both ends have arrived: the entry goes, its worktree leases with
    /// it, and its ID is remembered as finished.
    fn forget_settled(&mut self, dispatch_id: &str) {
        self.dispatches.remove(dispatch_id);
        // A worker that is over does not still hold a worktree
        // (SPEC §23: verification waits for write leases to be
        // released, and a lease nobody can release never is).
        self.write_leases
            .retain(|_, lease| lease.holder != dispatch_id);
        self.remember_terminal(dispatch_id);
    }

    /// Remember a settled dispatch ID, evicting the oldest past the cap.
    fn remember_terminal(&mut self, dispatch_id: &str) {
        if !self.finished.insert(dispatch_id.to_string()) {
            return;
        }
        self.finished_order.push_back(dispatch_id.to_string());
        while self.finished_order.len() > TERMINAL_MEMORY {
            if let Some(oldest) = self.finished_order.pop_front() {
                self.finished.remove(&oldest);
            }
        }
    }

    /// Take the exclusive write lease on a worktree for a dispatch
    /// (SPEC §23: "concurrent writers need separate worktrees or
    /// explicitly non-overlapping write leases"). Idempotent for the
    /// holder; false when somebody else holds it.
    ///
    /// The runner takes it for every candidate-writing dispatch after
    /// admission and before the process exists, releases it when the
    /// process has ended, and verification waits for the worktree's
    /// holder to be gone before snapshotting (`RunEngine::wait_for_writers`).
    /// Each writing attempt still gets its own worktree — the lease is
    /// the coordinator-side record that lets a second writer be refused
    /// and a straggler be waited for, across tabs.
    pub fn acquire_write(
        &mut self,
        worktree: &str,
        dispatch_id: &str,
        now: Instant,
    ) -> WriteLeaseOutcome {
        match self.write_leases.get(worktree) {
            Some(lease) if lease.holder == dispatch_id => WriteLeaseOutcome::Taken,
            Some(lease) => WriteLeaseOutcome::HeldBy {
                holder: lease.holder.clone(),
            },
            None => {
                self.write_leases.insert(
                    worktree.to_string(),
                    WriteLease {
                        holder: dispatch_id.to_string(),
                        since: now,
                    },
                );
                WriteLeaseOutcome::Taken
            }
        }
    }

    /// Release a write lease. Only its holder can: another dispatch
    /// asking is a bug or a race, and silently freeing somebody else's
    /// lease is how two writers end up in one worktree.
    pub fn release_write(&mut self, worktree: &str, dispatch_id: &str) -> ReleaseWriteOutcome {
        match self.write_leases.get(worktree) {
            Some(lease) if lease.holder == dispatch_id => {
                self.write_leases.remove(worktree);
                ReleaseWriteOutcome::Released
            }
            Some(_) | None => ReleaseWriteOutcome::NotTheHolder,
        }
    }

    /// How many dispatches are writing this worktree: what root
    /// verification waits to reach zero (SPEC §23).
    pub fn writers_active(&self, worktree: &str) -> u32 {
        u32::from(self.write_leases.contains_key(worktree))
    }

    /// Who holds a worktree's write lease, and since when.
    pub fn write_lease_holder(&self, worktree: &str) -> Option<(&str, Instant)> {
        self.write_leases
            .get(worktree)
            .map(|lease| (lease.holder.as_str(), lease.since))
    }

    fn descendants(&self, root: &str) -> Vec<String> {
        let mut out = vec![root.to_string()];
        let mut index = 0;
        while index < out.len() {
            let current = out[index].clone();
            for (id, dispatch) in &self.dispatches {
                if dispatch.parent.as_deref() == Some(current.as_str()) && !out.contains(id) {
                    out.push(id.clone());
                }
            }
            index += 1;
        }
        out
    }

    /// Cancel one agent subtree: the dispatch, its managed descendants
    /// and their queued requests. Siblings, other runs and other tabs are
    /// untouched. Returns the bound processes to signal.
    pub fn cancel_dispatch(&mut self, dispatch_id: &str, now: Instant) -> Vec<PendingSignal> {
        let subtree = self.descendants(dispatch_id);
        let mut to_signal = Vec::new();
        for id in &subtree {
            if let Some(dispatch) = self.dispatches.get_mut(id) {
                mark_cancelled(dispatch, now);
                if let Some(pending) = hand_over_terminate(id, dispatch) {
                    to_signal.push(pending);
                }
            }
        }
        self.queue.retain(|queued| {
            !subtree.contains(&queued.request.dispatch_id)
                && queued
                    .request
                    .parent_dispatch
                    .as_ref()
                    .is_none_or(|parent| !subtree.contains(parent))
        });
        self.drain(now);
        to_signal
    }

    /// Cancel one run: every dispatch of that run, queued or active. Other
    /// runs in the same session stay operational (SPEC §23).
    pub fn cancel_run(&mut self, run_id: &str, now: Instant) -> Vec<PendingSignal> {
        let mut to_signal = Vec::new();
        if let Some(run) = self.runs.get_mut(run_id) {
            run.cancelled = true;
            run.terminal = true;
        }
        for (id, dispatch) in self.dispatches.iter_mut() {
            if dispatch.run_id == run_id {
                mark_cancelled(dispatch, now);
                if let Some(pending) = hand_over_terminate(id, dispatch) {
                    to_signal.push(pending);
                }
            }
        }
        self.queue.retain(|queued| queued.request.run_id != run_id);
        self.drain(now);
        to_signal
    }

    /// Cancel one session's runs, never another tab's.
    pub fn cancel_session(&mut self, session_id: &str, now: Instant) -> Vec<PendingSignal> {
        let runs: Vec<String> = self
            .runs
            .iter()
            .filter(|(_, run)| run.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        let mut to_signal = Vec::new();
        for run_id in runs {
            to_signal.extend(self.cancel_run(&run_id, now));
        }
        to_signal
    }

    /// The root runner reports its run over — accepted, failed, blocked,
    /// whatever: no further dispatch is coming. Nothing is signalled and
    /// nothing is cancelled; the run simply stops holding the daemon
    /// open. Returns false for a run this coordinator does not know.
    pub fn finish_run(&mut self, run_id: &str) -> LifecycleOutcome {
        match self.runs.get_mut(run_id) {
            Some(run) => {
                run.terminal = true;
                LifecycleOutcome::Applied
            }
            None => LifecycleOutcome::Unknown,
        }
    }

    /// A cancelled dispatch whose caller acknowledged the cancellation:
    /// freed with unknown usage unless settled.
    pub fn acknowledge_cancel(&mut self, dispatch_id: &str, now: Instant) -> LifecycleOutcome {
        if !self.dispatches.contains_key(dispatch_id) {
            return LifecycleOutcome::Unknown;
        }
        self.settle(dispatch_id, None, now);
        self.release(dispatch_id, now)
    }

    /// Lease reconciliation. `alive(pid)` is the process table.
    ///
    /// - A stale lease with a dead bound process is dropped; its
    ///   reservation settles as unknown.
    /// - A stale lease with no process bound is kept and flagged — for
    ///   `UNBINDABLE_AFTER` grace periods. Nothing can bind it after the
    ///   launcher died, so it is then freed and reported `unbindable`
    ///   (C2), instead of holding a seat for the daemon's lifetime.
    /// - A cancelled dispatch with a live process is handed back to be
    ///   signalled: `Terminate` once, `Kill` once a grace period later,
    ///   then nothing (C5). The state machine sends neither.
    pub fn reconcile(&mut self, now: Instant, alive: &dyn Fn(u32) -> bool) -> ReconcileReport {
        let mut report = ReconcileReport::default();
        let ids: Vec<String> = self.dispatches.keys().cloned().collect();
        for id in ids {
            let (stale, binding, cancelled) = {
                let dispatch = &self.dispatches[&id];
                (
                    now.saturating_duration_since(dispatch.last_heartbeat) >= LEASE_GRACE,
                    dispatch.binding,
                    dispatch.cancellation.cancelled(),
                )
            };
            let pid = binding.pid();
            if cancelled {
                if let Binding::Bound { pid, at } = binding {
                    if alive(pid) {
                        if let Some(signal) = self.escalate(&id, now) {
                            report.to_signal.push(PendingSignal {
                                dispatch_id: id.clone(),
                                pid,
                                signal,
                                bound_at: at,
                            });
                        }
                    }
                }
            }
            if !stale {
                if let Some(dispatch) = self.dispatches.get_mut(&id) {
                    if let Binding::Unbound { rounds } = &mut dispatch.binding {
                        *rounds = 0;
                    }
                }
                continue;
            }
            match pid {
                Some(pid) if !alive(pid) => {
                    self.settle(&id, None, now);
                    self.release(&id, now);
                    report.dropped.push(id);
                }
                Some(_) => {}
                None => {
                    let rounds = match self.dispatches.get_mut(&id) {
                        Some(dispatch) => {
                            let Binding::Unbound { rounds } = &mut dispatch.binding else {
                                continue;
                            };
                            *rounds = rounds.saturating_add(1);
                            *rounds
                        }
                        None => continue,
                    };
                    if rounds >= UNBINDABLE_AFTER {
                        // Past grace, never bound, and a bind only ever
                        // happens in the call that launched the process:
                        // there is no worker to find and none can arrive.
                        self.settle(&id, None, now);
                        self.release(&id, now);
                        report.unbindable.push(id);
                    } else {
                        report.stale.push(id);
                    }
                }
            }
        }
        self.reap_runs(now);
        report
    }

    /// The next signal a cancelled dispatch has coming, and `None` once
    /// the ladder is used up. Walking the ladder is what records it: the
    /// step is marked handed over here and given back by
    /// `signal_undelivered` if the OS would not take it.
    fn escalate(&mut self, dispatch_id: &str, now: Instant) -> Option<Signal> {
        let dispatch = self.dispatches.get_mut(dispatch_id)?;
        match dispatch.cancellation {
            Cancellation::None => None,
            Cancellation::Requested { since } => {
                dispatch.cancellation = Cancellation::Terminated { since };
                Some(Signal::Terminate)
            }
            Cancellation::Terminated { since } => {
                if now.saturating_duration_since(since) < CANCEL_ESCALATE_AFTER {
                    return None;
                }
                dispatch.cancellation = Cancellation::Killed { since };
                Some(Signal::Kill)
            }
            Cancellation::Killed { .. } => None,
        }
    }

    /// The caller could not deliver a signal the ladder handed it, and
    /// the failure was one that could succeed later: the step is given
    /// back so the next reconcile offers it again.
    ///
    /// A process that is GONE, or one that answers `EPERM` because the
    /// PID now belongs to somebody else, is NOT given back: there is
    /// nothing left to deliver to in either case, and retrying would
    /// mean signalling a stranger every fifteen seconds for ever.
    pub fn signal_undelivered(&mut self, dispatch_id: &str, signal: Signal) {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return;
        };
        dispatch.cancellation = match (dispatch.cancellation, signal) {
            (Cancellation::Terminated { since }, Signal::Terminate) => {
                Cancellation::Requested { since }
            }
            (Cancellation::Killed { since }, Signal::Kill) => Cancellation::Terminated { since },
            (other, _) => other,
        };
    }

    /// Drop run rows that hold nothing: a terminal run with no dispatch
    /// and no queued request, or a run whose root runner has been silent
    /// past `RUN_ABANDON_GRACE`. Everything else — including a run that
    /// is merely between dispatches — is kept, because a registered run
    /// is what stops the daemon from idling out under a live run.
    fn reap_runs(&mut self, now: Instant) {
        let busy: std::collections::BTreeSet<String> = self
            .dispatches
            .values()
            .map(|dispatch| dispatch.run_id.clone())
            .chain(
                self.queue
                    .iter()
                    .map(|queued| queued.request.run_id.clone()),
            )
            .collect();
        self.runs.retain(|run_id, run| {
            if busy.contains(run_id) {
                return true;
            }
            let abandoned = now.saturating_duration_since(run.last_activity) >= RUN_ABANDON_GRACE;
            !(run.terminal || abandoned)
        });
    }

    /// Adopt a dispatch recorded as live in the ledger after a coordinator
    /// restart, without a fresh admission decision: it is already running
    /// (SPEC §23: coordinator restart reconciles without duplicate live
    /// workers). Unknown parents are recorded as unknown.
    pub fn adopt(
        &mut self,
        request: &DispatchRequest,
        pid: Option<u32>,
        agent_id: Option<&str>,
        now: Instant,
    ) {
        if self.dispatches.contains_key(&request.dispatch_id) {
            return;
        }
        if !self.runs.contains_key(&request.run_id) {
            self.register_run(
                &RunRegistration {
                    run_id: request.run_id.clone(),
                    session_id: request.session_id.clone(),
                    budget_micros: None,
                    max_agents: None,
                    max_depth: None,
                },
                now,
            );
        }
        self.admit(request.clone(), now, Lifecycle::Claimed);
        // The adopter checked this PID against the process table before
        // adopting at all (`Coordinator::start`): re-checking here would
        // only widen the window, not narrow it.
        self.bind_checked(&request.dispatch_id, agent_id, pid, now);
    }

    pub fn status(&self, now: Instant) -> StatusSnapshot {
        let mut active_by_class: BTreeMap<String, u32> = BTreeMap::new();
        let mut bound_processes: BTreeMap<String, BoundProcess> = BTreeMap::new();
        let mut waiting = 0;
        let mut stale_leases = 0;
        for (id, dispatch) in &self.dispatches {
            if let Binding::Bound { pid, at } = dispatch.binding {
                bound_processes.insert(
                    id.clone(),
                    BoundProcess {
                        pid,
                        bound_for_secs: now.saturating_duration_since(at).as_secs(),
                    },
                );
            }
            if dispatch.lifecycle.released() {
                continue;
            }
            if dispatch.lifecycle.waiting() {
                waiting += 1;
                continue;
            }
            *active_by_class
                .entry(dispatch.class.as_str().to_string())
                .or_default() += 1;
            if now.saturating_duration_since(dispatch.last_heartbeat) >= LEASE_GRACE
                && dispatch.binding.pid().is_none()
            {
                stale_leases += 1;
            }
        }
        // Per cap GROUP, not per class: two classes sharing one cap
        // overshoot it together, and counting them apart hid half of it
        // (A3).
        let over_admitted = CapGroup::ALL
            .into_iter()
            .map(|group| {
                let active = self.active_count(group, None);
                self.group_cap(group)
                    .map_or(0, |cap| active.saturating_sub(cap))
            })
            .sum();
        let runs = self
            .runs
            .iter()
            .map(|(run_id, run)| {
                let dispatches: Vec<&Dispatch> = self
                    .dispatches
                    .values()
                    .filter(|dispatch| &dispatch.run_id == run_id)
                    .collect();
                (
                    run_id.clone(),
                    RunStatus {
                        session_id: run.session_id.clone(),
                        budget_micros: run.budget,
                        reserved_micros: dispatches.iter().map(|d| d.reserved).sum(),
                        settled_micros: run.settled,
                        uncertain_settlements: run.uncertain,
                        admitted_total: run.admitted_total,
                        active: dispatches
                            .iter()
                            .filter(|d| d.lifecycle.holds_seat())
                            .count() as u32,
                        waiting: dispatches.iter().filter(|d| d.lifecycle.waiting()).count() as u32,
                        queued: self
                            .queue
                            .iter()
                            .filter(|queued| &queued.request.run_id == run_id)
                            .count() as u32,
                        cancelled: run.cancelled,
                    },
                )
            })
            .collect();
        StatusSnapshot {
            limits: self.limits.clone(),
            active_by_class,
            waiting,
            queued: self.queue.len() as u32,
            stale_leases,
            sessions: self.sessions.keys().cloned().collect(),
            runs,
            over_admitted,
            bound_processes,
            write_leases: self
                .write_leases
                .iter()
                .map(|(worktree, lease)| (worktree.clone(), lease.holder.clone()))
                .collect(),
        }
    }

    /// Parent evidence for a dispatch. `None` is a dispatch this
    /// coordinator does not hold; the three states of a dispatch it does
    /// are [`Parentage`]'s variants.
    pub fn parentage(&self, dispatch_id: &str) -> Option<Parentage> {
        self.dispatches.get(dispatch_id).map(|dispatch| {
            match (dispatch.parent.clone(), dispatch.parent_known) {
                (None, _) => Parentage::Root,
                (Some(parent), true) => Parentage::Known { parent },
                (Some(parent), false) => Parentage::Unrecognised { parent },
            }
        })
    }

    pub fn depth_of(&self, dispatch_id: &str) -> Option<u32> {
        self.dispatches
            .get(dispatch_id)
            .map(|dispatch| dispatch.depth)
    }

    /// True when nothing is registered as active, waiting or queued AND
    /// every registered run has reached an end state — the coordinator's
    /// idle-exit condition.
    ///
    /// The run clause is load-bearing. A run spends minutes between
    /// dispatches — snapshotting, verifying the candidate — with no
    /// dispatch and nothing queued. Judging idleness on dispatches alone
    /// made the daemon unlink its socket and lock mid-run, and the run's
    /// next `admit` came back `blocked:admission_unavailable` or
    /// `UnknownRun` because `RemoteGate` never re-elects: idle-exit
    /// killed the very run it was counting as absent.
    pub fn is_idle(&self) -> bool {
        self.dispatches.is_empty()
            && self.queue.is_empty()
            && self.runs.values().all(|run| run.terminal)
    }
}

/// Mark a dispatch cancelled, keeping the first cancellation's moment:
/// the escalation clock starts when the cancellation did, not when a
/// later cancel of the same subtree passed through.
fn mark_cancelled(dispatch: &mut Dispatch, now: Instant) {
    if !dispatch.cancellation.cancelled() {
        dispatch.cancellation = Cancellation::Requested { since: now };
    }
}

/// Hand the first `Terminate` of a cancellation to the caller that
/// requested it, if there is a process to send it to. Reconcile must not
/// send it a second time, only escalate past it (C5), so the ladder step
/// is recorded here.
fn hand_over_terminate(dispatch_id: &str, dispatch: &mut Dispatch) -> Option<PendingSignal> {
    let Binding::Bound { pid, at } = dispatch.binding else {
        return None;
    };
    match dispatch.cancellation {
        Cancellation::Requested { since } => {
            dispatch.cancellation = Cancellation::Terminated { since };
            Some(PendingSignal {
                dispatch_id: dispatch_id.to_string(),
                pid,
                signal: Signal::Terminate,
                bound_at: at,
            })
        }
        // Already asked, already killed, or not cancelled at all.
        Cancellation::None | Cancellation::Terminated { .. } | Cancellation::Killed { .. } => None,
    }
}

fn min_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn min_opt_i64(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// How the runner talks to admission: in-process for tests and observed
/// mode, over the coordinator socket for managed runs. Every call can
/// fail with the coordinator unreachable, which a strict run reports as
/// unavailable admission rather than proceeding unmanaged (SPEC §23).
pub trait Gate {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError>;
    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError>;
    /// Bind the agent identity and the launched process to the
    /// reservation. The outcome is the caller's to act on: a bind the
    /// coordinator does not recognise means this worker holds no seat
    /// and no reservation — after a re-election, for instance — and
    /// running on is exactly the unmanaged launch SPEC §23 forbids.
    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<BindOutcome, GateError>;
    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError>;
    /// The worker saw `cancelled` on a heartbeat and is stopping. The
    /// seat and the reservation go now, instead of waiting out the lease
    /// grace with a worker that is already leaving (C5). Idempotent, and
    /// a no-op by default so a gate that tracks no lifetime still
    /// compiles.
    fn acknowledge_cancel(&self, _dispatch_id: &str) -> Result<LifecycleOutcome, GateError> {
        Ok(LifecycleOutcome::Applied)
    }
    fn mark_waiting(&self, dispatch_id: &str) -> Result<WaitOutcome, GateError>;
    fn resume(&self, dispatch_id: &str) -> Result<ResumeOutcome, GateError>;
    fn release(&self, dispatch_id: &str) -> Result<LifecycleOutcome, GateError>;
    fn settle(
        &self,
        dispatch_id: &str,
        spent_micros: Option<i64>,
    ) -> Result<LifecycleOutcome, GateError>;
    /// Abandon a request that was never launched.
    fn withdraw(&self, dispatch_id: &str) -> Result<WithdrawOutcome, GateError>;
    /// The run reached an end state and will dispatch nothing more. Until
    /// a root runner says so, a registered run keeps the coordinator from
    /// idling out (see `AdmissionState::is_idle`), so a runner that owns a
    /// run's lifecycle should call this on every terminal path. The
    /// default is a no-op for gates that do not track run lifetime.
    fn finish_run(&self, _run_id: &str) -> Result<LifecycleOutcome, GateError> {
        Ok(LifecycleOutcome::Applied)
    }
    /// Take the exclusive write lease on a worktree for a dispatch that
    /// is about to write it (SPEC §23). `HeldBy` names the other writer;
    /// the caller must not launch a second one into that tree. Gates that
    /// track no leases grant every request.
    fn acquire_write(
        &self,
        _dispatch_id: &str,
        _worktree: &str,
    ) -> Result<WriteLeaseOutcome, GateError> {
        Ok(WriteLeaseOutcome::Taken)
    }
    /// The dispatch has stopped writing the worktree. Only the holder can
    /// release; a mismatched release frees nothing and says so, rather
    /// than somebody else's lease being freed.
    fn release_write(
        &self,
        _dispatch_id: &str,
        _worktree: &str,
    ) -> Result<ReleaseWriteOutcome, GateError> {
        Ok(ReleaseWriteOutcome::Released)
    }
    /// Who holds a worktree's write lease, if anyone: what verification
    /// waits to become `None` before it snapshots (SPEC §23: "root
    /// verification waits for all relevant write leases to be released").
    fn write_lease_holder(&self, _worktree: &str) -> Result<Option<String>, GateError> {
        Ok(None)
    }
    /// What this gate can enforce, for reports (SPEC §23: observed-only
    /// paths are labelled, never claimed as guarantees).
    fn enforcement(&self) -> Enforcement;
}

/// Why an admission call produced no answer this caller can act on.
///
/// The distinction that matters to a run is whether the coordinator was
/// REACHABLE: an outage means the request is preserved and nothing was
/// launched, and a refusal is a decision the run can report as such.
/// Reporting a refusal as "admission unavailable" sends the operator
/// looking for a dead daemon that is answering perfectly well (A12).
#[derive(Debug)]
pub enum GateError {
    /// The coordinator could not be reached, or the call failed in
    /// transit: no daemon answering, a stale endpoint, a timeout.
    Unavailable {
        operation: &'static str,
        socket: String,
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The coordinator answered and deliberately did not apply the call.
    Refused {
        operation: &'static str,
        entity: String,
        detail: String,
    },
    /// Resuming would hold more seats beyond a cap than the ceiling
    /// allows. A delay, not a failure: the parent stays waiting and
    /// polls again (C8).
    OverCeiling { entity: String, over: u32, max: u32 },
    /// The reply was not one this version of relais can act on — a
    /// daemon left running from another install, or a corrupted line.
    Protocol {
        operation: &'static str,
        detail: String,
    },
}

impl GateError {
    /// Was the coordinator out of reach? A caller that blocks a run on
    /// `admission_unavailable` should say so only when this is true.
    pub fn unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    /// Which admission call this is about.
    pub fn operation(&self) -> &'static str {
        match self {
            Self::Unavailable { operation, .. }
            | Self::Refused { operation, .. }
            | Self::Protocol { operation, .. } => operation,
            Self::OverCeiling { .. } => "resume",
        }
    }
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable {
                operation,
                socket,
                cause,
            } => write!(
                f,
                "admission unavailable: {operation} could not reach the coordinator at {socket}: {cause}"
            ),
            Self::Refused {
                operation,
                entity,
                detail,
            } => write!(f, "admission refused: {operation} on {entity}: {detail}"),
            Self::OverCeiling { entity, over, max } => write!(
                f,
                "admission refused: resuming {entity} would hold {over} seats beyond the class \
                 cap, past the configured maximum of {max}; it stays waiting and can poll again"
            ),
            Self::Protocol { operation, detail } => {
                write!(f, "admission protocol: {operation}: {detail}")
            }
        }
    }
}

impl std::error::Error for GateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unavailable { cause, .. } => Some(cause.as_ref()),
            Self::Refused { .. } | Self::OverCeiling { .. } | Self::Protocol { .. } => None,
        }
    }
}

/// In-process gate over a shared state: tests, and single-process
/// scheduling that does not need cross-tab coordination.
pub struct LocalGate {
    state: std::sync::Mutex<AdmissionState>,
}

impl LocalGate {
    pub fn new(limits: ConcurrencyLimits) -> Self {
        Self {
            state: std::sync::Mutex::new(AdmissionState::new(limits)),
        }
    }

    /// The admission state, recovering a lock a panicking caller
    /// poisoned instead of propagating it. One failed call must not
    /// wedge every later one: the state machine mutates one field at a
    /// time and every write path is total, so the worst a recovered lock
    /// carries is one half-applied lifecycle event — against a gate that
    /// answers nothing at all. The coordinator recovers the same way.
    fn admission(&self) -> std::sync::MutexGuard<'_, AdmissionState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn status(&self) -> StatusSnapshot {
        self.admission().status(Instant::now())
    }

    pub fn cancel_run(&self, run_id: &str) {
        self.admission().cancel_run(run_id, Instant::now());
    }
}

impl Gate for LocalGate {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError> {
        self.admission().register_run(registration, Instant::now());
        Ok(())
    }

    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError> {
        Ok(self.admission().request(request, Instant::now()))
    }

    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<BindOutcome, GateError> {
        // Every outcome comes back as an outcome, exactly as it does
        // over the socket: what a refused bind means is the caller's to
        // decide, and the two gates must not disagree about it.
        Ok(self.admission().bind(
            dispatch_id,
            agent_id,
            pid,
            Instant::now(),
            &crate::procs::alive,
        ))
    }

    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError> {
        Ok(self.admission().heartbeat(dispatch_id, Instant::now()))
    }

    fn acknowledge_cancel(&self, dispatch_id: &str) -> Result<LifecycleOutcome, GateError> {
        Ok(self
            .admission()
            .acknowledge_cancel(dispatch_id, Instant::now()))
    }

    fn mark_waiting(&self, dispatch_id: &str) -> Result<WaitOutcome, GateError> {
        Ok(self
            .admission()
            .mark_waiting(dispatch_id, Instant::now(), &crate::procs::alive))
    }

    fn resume(&self, dispatch_id: &str) -> Result<ResumeOutcome, GateError> {
        Ok(self.admission().resume(dispatch_id, Instant::now()))
    }

    fn release(&self, dispatch_id: &str) -> Result<LifecycleOutcome, GateError> {
        Ok(self.admission().release(dispatch_id, Instant::now()))
    }

    fn settle(
        &self,
        dispatch_id: &str,
        spent_micros: Option<i64>,
    ) -> Result<LifecycleOutcome, GateError> {
        Ok(self
            .admission()
            .settle(dispatch_id, spent_micros, Instant::now()))
    }

    fn withdraw(&self, dispatch_id: &str) -> Result<WithdrawOutcome, GateError> {
        Ok(self.admission().withdraw(dispatch_id, Instant::now()))
    }

    fn finish_run(&self, run_id: &str) -> Result<LifecycleOutcome, GateError> {
        Ok(self.admission().finish_run(run_id))
    }

    fn acquire_write(
        &self,
        dispatch_id: &str,
        worktree: &str,
    ) -> Result<WriteLeaseOutcome, GateError> {
        Ok(self
            .admission()
            .acquire_write(worktree, dispatch_id, Instant::now()))
    }

    fn release_write(
        &self,
        dispatch_id: &str,
        worktree: &str,
    ) -> Result<ReleaseWriteOutcome, GateError> {
        Ok(self.admission().release_write(worktree, dispatch_id))
    }

    fn write_lease_holder(&self, worktree: &str) -> Result<Option<String>, GateError> {
        Ok(self
            .admission()
            .write_lease_holder(worktree)
            .map(|(holder, _)| holder.to_string()))
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::InProcess
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ConcurrencyLimits {
        ConcurrencyLimits {
            max_active_agents: Some(4),
            max_active_agents_per_session: Some(2),
            max_heavy_commands: Some(1),
            max_training_jobs: Some(1),
            max_agent_depth: Some(2),
            max_agents_per_run: Some(6),
            training_when_idle: false,
        }
    }

    fn state() -> AdmissionState {
        let mut state = AdmissionState::new(limits());
        for (run, session) in [("run-a", "tab-a"), ("run-b", "tab-b"), ("run-c", "tab-c")] {
            state.register_run(
                &RunRegistration {
                    run_id: run.into(),
                    session_id: session.into(),
                    budget_micros: None,
                    max_agents: None,
                    max_depth: None,
                },
                Instant::now(),
            );
        }
        state
    }

    fn req(dispatch: &str, run: &str, session: &str) -> DispatchRequest {
        DispatchRequest {
            dispatch_id: dispatch.into(),
            run_id: run.into(),
            session_id: session.into(),
            parent_dispatch: None,
            depth: 0,
            resource: ResourceClass::ModelWork,
            reserve_micros: 0,
        }
    }

    fn child(
        dispatch: &str,
        run: &str,
        session: &str,
        parent: &str,
        depth: u32,
    ) -> DispatchRequest {
        DispatchRequest {
            parent_dispatch: Some(parent.into()),
            depth,
            ..req(dispatch, run, session)
        }
    }

    fn granted(decision: Decision) -> bool {
        decision == Decision::Granted
    }

    /// A process table that says yes. Liveness is a callback precisely so
    /// the §23 scenarios need no real processes.
    fn alive(_pid: u32) -> bool {
        true
    }

    /// What a reconcile handed over to signal, as a comparable triple.
    fn signalled(report: &ReconcileReport) -> Vec<(String, u32, Signal)> {
        report
            .to_signal
            .iter()
            .map(|pending| (pending.dispatch_id.clone(), pending.pid, pending.signal))
            .collect()
    }

    /// Bind a live process to a dispatch. `mark_waiting` needs one (C8),
    /// and so does anything that reconciles against the process table.
    fn bind_live(state: &mut AdmissionState, dispatch_id: &str, pid: u32, now: Instant) {
        assert_eq!(
            state.bind(dispatch_id, None, Some(pid), now, &alive),
            BindOutcome::Bound,
            "{dispatch_id} binds"
        );
    }

    /// Claim the waiting seat-back, with the bind it now requires.
    fn wait_bound(state: &mut AdmissionState, dispatch_id: &str, pid: u32, now: Instant) {
        bind_live(state, dispatch_id, pid, now);
        assert_eq!(
            state.mark_waiting(dispatch_id, now, &alive),
            WaitOutcome::Waiting,
            "{dispatch_id} waits"
        );
    }

    // SPEC §23 acceptance: three simultaneous sessions, multiple agents
    // each, global/per-session limits, fair progress.
    #[test]
    fn three_sessions_observe_limits_and_progress_fairly() {
        let mut state = state();
        let t0 = Instant::now();
        // Each tab asks for three agents; the per-session cap is 2 and
        // the global cap is 4.
        let mut outcomes = Vec::new();
        for tab in ["a", "b", "c"] {
            for n in 0..3 {
                let decision = state.request(
                    &req(
                        &format!("d-{tab}-{n}"),
                        &format!("run-{tab}"),
                        &format!("tab-{tab}"),
                    ),
                    t0,
                );
                outcomes.push((tab, n, decision));
            }
        }
        let status = state.status(t0);
        assert_eq!(status.active_by_class["model_work"], 4, "global cap binds");
        // tab-a got 2, tab-b got 2 (global cap reached), tab-c got 0.
        assert!(granted(outcomes[0].2.clone()) && granted(outcomes[1].2.clone()));
        assert!(
            matches!(outcomes[2].2, Decision::Queued { .. }),
            "per-session cap"
        );
        assert!(granted(outcomes[3].2.clone()) && granted(outcomes[4].2.clone()));
        assert!(
            matches!(outcomes[6].2, Decision::Queued { .. }),
            "global cap"
        );
        assert_eq!(status.queued, 5);

        // tab-a finishes one agent: fairness serves the tab that has
        // been served least recently (tab-c, never served), not tab-a's
        // own queued child.
        state.release("d-a-0", t0 + Duration::from_secs(1));
        state.settle("d-a-0", Some(0), t0 + Duration::from_secs(1));
        let poll_c = state.request(&req("d-c-0", "run-c", "tab-c"), t0 + Duration::from_secs(1));
        assert_eq!(poll_c, Decision::Granted, "the starved tab is served first");
        let poll_a = state.request(&req("d-a-2", "run-a", "tab-a"), t0 + Duration::from_secs(1));
        assert!(matches!(poll_a, Decision::Queued { .. }));
    }

    #[test]
    fn a_drained_entry_is_granted_exactly_once_then_already_admitted() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        assert!(granted(state.request(&req("d2", "run-a", "tab-a"), t0)));
        assert!(matches!(
            state.request(&req("d3", "run-a", "tab-a"), t0),
            Decision::Queued { position: 1 }
        ));
        state.release("d1", t0);
        state.settle("d1", Some(0), t0);
        // The queued entry was drained in; the caller's next poll is the
        // one and only Granted.
        assert_eq!(
            state.request(&req("d3", "run-a", "tab-a"), t0),
            Decision::Granted
        );
        assert_eq!(
            state.request(&req("d3", "run-a", "tab-a"), t0),
            Decision::AlreadyAdmitted,
            "a retry cannot create a duplicate agent"
        );
        assert_eq!(
            state.request(&req("d2", "run-a", "tab-a"), t0),
            Decision::AlreadyAdmitted
        );
    }

    #[test]
    fn aging_reorders_but_never_jumps_a_cap() {
        let mut state = state();
        let t0 = Instant::now();
        // Fill the global cap from tabs a and b.
        for d in ["a-0", "a-1", "b-0", "b-1"] {
            let tab = &d[..1];
            assert!(granted(state.request(
                &req(
                    &format!("d-{d}"),
                    &format!("run-{tab}"),
                    &format!("tab-{tab}")
                ),
                t0
            )));
        }
        // tab-c queues first, tab-a's third queues later.
        assert!(matches!(
            state.request(&req("d-c-0", "run-c", "tab-c"), t0),
            Decision::Queued { .. }
        ));
        let later = t0 + Duration::from_secs(5);
        assert!(matches!(
            state.request(&req("d-a-2", "run-a", "tab-a"), later),
            Decision::Queued { .. }
        ));
        // Long after: both are aged; nothing was released, so nothing
        // drains — aging does not over-admit.
        let much_later = t0 + Duration::from_secs(120);
        state.heartbeat("d-a-0", much_later);
        assert!(matches!(
            state.request(&req("d-c-0", "run-c", "tab-c"), much_later),
            Decision::Queued { .. }
        ));
        assert_eq!(state.status(much_later).active_by_class["model_work"], 4);
        assert_eq!(state.status(much_later).over_admitted, 0);
    }

    // SPEC §23 acceptance: nested and resumed agents keep parentage and
    // root-budget attribution without duplicate usage or dispatch.
    #[test]
    fn nested_agents_keep_parentage_and_share_the_root_budget() {
        let mut state = AdmissionState::new(limits());
        let t0 = Instant::now();
        state.register_run(
            &RunRegistration {
                run_id: "run-a".into(),
                session_id: "tab-a".into(),
                budget_micros: Some(1_000),
                max_agents: Some(10),
                max_depth: Some(3),
            },
            t0,
        );
        let mut root = req("root", "run-a", "tab-a");
        root.reserve_micros = 600;
        assert!(granted(state.request(&root, t0)));
        wait_bound(&mut state, "root", 101, t0);
        let mut kid = child("kid", "run-a", "tab-a", "root", 1);
        kid.reserve_micros = 300;
        assert!(granted(state.request(&kid, t0)));
        assert_eq!(
            state.parentage("kid"),
            Some(Parentage::Known {
                parent: "root".into()
            })
        );
        // A grandchild that would overspend the ROOT budget is refused:
        // children cannot independently spend the same allowance.
        let mut grandkid = child("grandkid", "run-a", "tab-a", "kid", 2);
        grandkid.reserve_micros = 200;
        assert!(matches!(
            state.request(&grandkid, t0),
            Decision::Refused {
                code: Refusal::BudgetExceeded,
                ..
            }
        ));
        // Settling the child below its reservation frees the difference.
        state.release("kid", t0);
        state.settle("kid", Some(50), t0);
        assert!(granted(state.request(&grandkid, t0)));
        let status = state.status(t0);
        let run = &status.runs["run-a"];
        assert_eq!(run.settled_micros, 50);
        assert_eq!(run.reserved_micros, 800);
        assert_eq!(run.admitted_total, 3);
        // A re-registration cannot enlarge the budget.
        state.register_run(
            &RunRegistration {
                run_id: "run-a".into(),
                session_id: "tab-a".into(),
                budget_micros: Some(1_000_000),
                max_agents: Some(1_000),
                max_depth: Some(99),
            },
            t0,
        );
        assert_eq!(state.status(t0).runs["run-a"].budget_micros, Some(1_000));
    }

    #[test]
    fn depth_and_aggregate_agent_caps_are_hard_limits() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(matches!(
            state.request(&child("deep", "run-a", "tab-a", "nobody", 3), t0),
            Decision::Refused {
                code: Refusal::DepthExceeded,
                ..
            }
        ));
        // max_agents_per_run = 6 counts every admission of the run's life,
        // released or not: no work package resets the root budget.
        for n in 0..6 {
            let id = format!("d{n}");
            let decision = state.request(&req(&id, "run-a", "tab-a"), t0);
            assert!(
                !matches!(decision, Decision::Refused { .. }),
                "{n}: {decision:?}"
            );
            state.release(&id, t0);
            state.settle(&id, Some(0), t0);
        }
        assert!(matches!(
            state.request(&req("d6", "run-a", "tab-a"), t0),
            Decision::Refused {
                code: Refusal::RunAgentCap,
                ..
            }
        ));
        assert!(matches!(
            state.request(&req("x", "run-unknown", "tab-a"), t0),
            Decision::Refused {
                code: Refusal::UnknownRun,
                ..
            }
        ));
    }

    // SPEC §23 acceptance: parents waiting for children cannot exhaust
    // all execution slots and deadlock the scheduler.
    #[test]
    fn waiting_parents_cannot_deadlock_the_scheduler() {
        let mut state = state();
        let t0 = Instant::now();
        // Four parents fill the global cap, then each waits on a child.
        for tab in ["a", "b"] {
            for n in 0..2 {
                assert!(granted(state.request(
                    &req(
                        &format!("p-{tab}-{n}"),
                        &format!("run-{tab}"),
                        &format!("tab-{tab}")
                    ),
                    t0
                )));
            }
        }
        let children: Vec<DispatchRequest> = ["a", "b"]
            .iter()
            .flat_map(|tab| {
                (0..2).map(move |n| {
                    child(
                        &format!("c-{tab}-{n}"),
                        &format!("run-{tab}"),
                        &format!("tab-{tab}"),
                        &format!("p-{tab}-{n}"),
                        1,
                    )
                })
            })
            .collect();
        for kid in &children {
            assert!(matches!(state.request(kid, t0), Decision::Queued { .. }));
        }
        for (index, tab) in ["a", "b"].into_iter().enumerate() {
            for n in 0..2 {
                let pid = 700 + (index as u32) * 10 + n;
                wait_bound(&mut state, &format!("p-{tab}-{n}"), pid, t0);
            }
        }
        // Every child now runs: the waiters gave their seats back.
        for kid in &children {
            assert_eq!(
                state.request(kid, t0),
                Decision::Granted,
                "{}",
                kid.dispatch_id
            );
        }
        let status = state.status(t0);
        assert_eq!(status.waiting, 4);
        assert_eq!(status.active_by_class["model_work"], 4);
        // A parent resumes after its child finished: it takes its seat
        // back at once, and any overshoot is visible.
        state.release("c-a-0", t0);
        state.settle("c-a-0", Some(0), t0);
        assert_eq!(state.resume("p-a-0", t0), ResumeOutcome::Resumed);
        assert_eq!(state.status(t0).over_admitted, 0);
        assert_eq!(state.resume("p-a-1", t0), ResumeOutcome::Resumed);
        assert_eq!(
            state.status(t0).over_admitted,
            1,
            "overshoot is shown, not hidden"
        );
    }

    // C8: the overshoot a resumed parent takes is bounded. Past the
    // ceiling the resume is refused and the parent stays waiting, which
    // is what keeps a published cap a cap under deep nesting.
    #[test]
    fn resuming_past_the_over_admission_ceiling_is_refused() {
        let mut state = state();
        let t0 = Instant::now();
        state.set_max_over_admitted(Some(1));
        // Four parents fill the global cap (4) and all wait.
        for (index, tab) in ["a", "b"].into_iter().enumerate() {
            for n in 0..2 {
                let id = format!("p-{tab}-{n}");
                assert!(granted(state.request(
                    &req(&id, &format!("run-{tab}"), &format!("tab-{tab}")),
                    t0
                )));
                wait_bound(&mut state, &id, 800 + (index as u32) * 10 + n, t0);
            }
        }
        // Four children take the seats the parents gave back.
        for (index, tab) in ["a", "b"].into_iter().enumerate() {
            for n in 0..2 {
                let kid = child(
                    &format!("c-{tab}-{n}"),
                    &format!("run-{tab}"),
                    &format!("tab-{tab}"),
                    &format!("p-{tab}-{n}"),
                    1,
                );
                assert!(granted(state.request(&kid, t0)), "{index}");
            }
        }
        assert_eq!(state.status(t0).active_by_class["model_work"], 4);
        // The first parent back overshoots by one: allowed, and shown.
        assert_eq!(state.resume("p-a-0", t0), ResumeOutcome::Resumed);
        assert_eq!(state.status(t0).over_admitted, 1);
        // The second would make it two, past the ceiling of one.
        assert_eq!(
            state.resume("p-a-1", t0),
            ResumeOutcome::OverAdmitted { over: 2, max: 1 }
        );
        assert_eq!(state.status(t0).over_admitted, 1, "the cap held");
        assert_eq!(state.status(t0).waiting, 3, "it is still waiting");
        // A seat frees: the same poll now succeeds. Refusal is a delay,
        // never a lost parent.
        state.release("c-b-0", t0);
        state.settle("c-b-0", Some(0), t0);
        assert_eq!(state.resume("p-a-1", t0), ResumeOutcome::Resumed);
        assert_eq!(state.resume("nobody", t0), ResumeOutcome::UnknownDispatch);
    }

    // C8: a seat is given back on a claim the coordinator can at least
    // partly check — a live, bound process — and never on a bare word.
    #[test]
    fn waiting_needs_a_live_bound_process() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        assert_eq!(
            state.mark_waiting("d1", t0, &alive),
            WaitOutcome::NoLiveProcess,
            "nothing is bound: the claim is unverifiable"
        );
        assert_eq!(state.status(t0).waiting, 0);
        bind_live(&mut state, "d1", 4242, t0);
        assert_eq!(
            state.mark_waiting("d1", t0, &|_pid| false),
            WaitOutcome::NoLiveProcess,
            "the bound process is gone: it is not waiting, it is over"
        );
        assert!(state.mark_waiting("d1", t0, &alive).waiting());
        assert_eq!(state.status(t0).waiting, 1);
        assert_eq!(
            state.mark_waiting("nobody", t0, &alive),
            WaitOutcome::UnknownDispatch
        );
    }

    // C6: a PID off the wire is checked against the process table at
    // bind, which is the one moment the check means anything.
    #[test]
    fn a_bind_takes_only_a_live_pid() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        assert_eq!(
            state.bind("d1", Some("agent"), Some(4242), t0, &|_pid| false),
            BindOutcome::PidNotAlive,
            "a PID nothing is running is refused"
        );
        assert_eq!(
            state.bind("nobody", None, Some(4242), t0, &alive),
            BindOutcome::UnknownDispatch
        );
        // Refused: the lease still has no process, so reconcile treats it
        // as unbound rather than chasing a number.
        let stale = t0 + LEASE_GRACE + Duration::from_secs(1);
        assert_eq!(state.reconcile(stale, &alive).stale, vec!["d1".to_string()]);
        // An agent ID with no PID binds: identity and process are
        // separate facts and only the second is checkable.
        assert!(state
            .bind("d1", Some("agent"), None, t0, &|_pid| false)
            .bound());
        assert!(state.bind("d1", None, Some(4242), t0, &alive).bound());
    }

    // SPEC §23 acceptance: simultaneous dispatches cannot reserve the
    // same remaining budget twice; missing terminal usage stays uncertain.
    #[test]
    fn reservations_are_exclusive_and_unknown_usage_stays_uncertain() {
        let mut state = AdmissionState::new(limits());
        let t0 = Instant::now();
        state.register_run(
            &RunRegistration {
                run_id: "run-a".into(),
                session_id: "tab-a".into(),
                budget_micros: Some(100),
                max_agents: None,
                max_depth: None,
            },
            t0,
        );
        let mut first = req("d1", "run-a", "tab-a");
        first.reserve_micros = 60;
        let mut second = req("d2", "run-a", "tab-b");
        second.reserve_micros = 60;
        assert!(granted(state.request(&first, t0)));
        assert!(matches!(
            state.request(&second, t0),
            Decision::Refused {
                code: Refusal::BudgetExceeded,
                ..
            }
        ));
        // Releasing the seat without settling keeps the money reserved.
        state.release("d1", t0);
        assert!(matches!(
            state.request(&second, t0),
            Decision::Refused { .. }
        ));
        // Unknown usage: the reservation stands in, and the run is marked
        // uncertain — never zero.
        state.settle("d1", None, t0);
        let run = &state.status(t0).runs["run-a"];
        assert_eq!(run.settled_micros, 60);
        assert_eq!(run.uncertain_settlements, 1);
        assert_eq!(run.reserved_micros, 0);
        // 60 committed + 60 requested > 100: still refused; 40 fits.
        assert!(matches!(
            state.request(&second, t0),
            Decision::Refused { .. }
        ));
        second.reserve_micros = 40;
        assert!(granted(state.request(&second, t0)));
    }

    // SPEC §23 acceptance: cancelling one subtree leaves other runs and
    // tabs operational.
    #[test]
    fn cancelling_a_subtree_spares_siblings_other_runs_and_tabs() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("root-a", "run-a", "tab-a"), t0)));
        wait_bound(&mut state, "root-a", 4141, t0);
        assert!(granted(
            state.request(&child("kid-1", "run-a", "tab-a", "root-a", 1), t0)
        ));
        assert!(granted(
            state.request(&child("kid-2", "run-a", "tab-a", "root-a", 1), t0)
        ));
        bind_live(&mut state, "kid-1", 4242, t0);
        assert!(matches!(
            state.request(&child("grandkid", "run-a", "tab-a", "kid-1", 2), t0),
            Decision::Queued { .. }
        ));
        assert!(granted(state.request(&req("root-b", "run-b", "tab-b"), t0)));

        let to_signal = state.cancel_dispatch("kid-1", t0);
        assert_eq!(
            to_signal
                .iter()
                .map(|pending| (pending.dispatch_id.as_str(), pending.pid, pending.signal))
                .collect::<Vec<_>>(),
            vec![("kid-1", 4242, Signal::Terminate)]
        );
        assert!(state.heartbeat("kid-1", t0).cancelled);
        assert!(!state.heartbeat("kid-2", t0).cancelled, "the sibling lives");
        assert!(
            !state.heartbeat("root-b", t0).cancelled,
            "the other tab lives"
        );
        assert!(!state.heartbeat("root-a", t0).cancelled, "the parent lives");
        assert_eq!(
            state.status(t0).queued,
            0,
            "the cancelled subtree's queue entry is gone"
        );

        state.cancel_run("run-a", t0);
        assert!(state.heartbeat("root-a", t0).cancelled);
        assert!(!state.heartbeat("root-b", t0).cancelled);
        assert!(matches!(
            state.request(&req("late", "run-a", "tab-a"), t0),
            Decision::Refused {
                code: Refusal::RunCancelled,
                ..
            }
        ));
    }

    // SPEC §23 acceptance: coordinator restart, lost hooks and duplicate
    // lifecycle events reconcile without duplicate live workers.
    #[test]
    fn restart_adopts_live_dispatches_and_reconciles_leases_by_liveness() {
        let mut state = state();
        let t0 = Instant::now();
        // Adopt from the ledger's live set: no fresh admission, no
        // duplicate, unknown parent recorded as unknown.
        let orphan = child("orphan", "run-a", "tab-a", "gone-parent", 1);
        state.adopt(&orphan, Some(7777), Some("agent-7"), t0);
        state.adopt(&orphan, Some(7777), Some("agent-7"), t0);
        assert_eq!(state.status(t0).active_by_class["model_work"], 1);
        assert_eq!(
            state.parentage("orphan"),
            Some(Parentage::Unrecognised {
                parent: "gone-parent".into()
            })
        );
        assert_eq!(state.request(&orphan, t0), Decision::AlreadyAdmitted);

        // Duplicate lifecycle events are harmless.
        assert!(state
            .bind("orphan", Some("agent-7"), Some(7777), t0, &alive)
            .bound());
        assert!(state.heartbeat("orphan", t0).known);
        assert!(state.heartbeat("orphan", t0).known);

        // A stale lease with a LIVE process is kept; expiry alone never
        // proves death.
        let stale = t0 + LEASE_GRACE + Duration::from_secs(1);
        let report = state.reconcile(stale, &|_pid| true);
        assert!(report.dropped.is_empty());
        assert_eq!(state.status(stale).active_by_class["model_work"], 1);
        // A stale lease with NO process to check is kept and flagged.
        assert!(granted(
            state.request(&req("unbound", "run-b", "tab-b"), t0)
        ));
        let report = state.reconcile(stale, &|_pid| true);
        assert_eq!(report.stale, vec!["unbound".to_string()]);
        assert_eq!(state.status(stale).stale_leases, 1);
        // A stale lease with a DEAD process is dropped, usage uncertain.
        let report = state.reconcile(stale, &|pid| pid != 7777);
        assert_eq!(report.dropped, vec!["orphan".to_string()]);
        assert!(!state.heartbeat("orphan", stale).known);
        assert_eq!(state.status(stale).runs["run-a"].uncertain_settlements, 1);
        let _ = state.depth_of("unbound");
    }

    // C2: a lease that never got a process is not a worker. It used to
    // hold its seat for the life of the daemon — three killed runs took
    // half the seats, permanently.
    #[test]
    fn a_lease_that_can_never_be_bound_is_freed_and_reported() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(
            state.request(&req("unbound", "run-a", "tab-a"), t0)
        ));
        assert!(granted(state.request(&req("bound", "run-a", "tab-a"), t0)));
        bind_live(&mut state, "bound", 4242, t0);

        // Past grace once: kept and flagged. Expiry never proves death.
        let mut now = t0 + LEASE_GRACE + Duration::from_secs(1);
        let report = state.reconcile(now, &alive);
        assert_eq!(report.stale, vec!["unbound".to_string()]);
        assert!(report.unbindable.is_empty());
        assert_eq!(state.status(now).active_by_class["model_work"], 2);
        // A heartbeat restarts the count: a live worker that simply has
        // no PID to report is not unbindable.
        state.heartbeat("unbound", now);
        now += LEASE_GRACE + Duration::from_secs(1);

        // `UNBINDABLE_AFTER` (3) consecutive stale reconciles with
        // nothing bound: nothing can arrive to claim the seat, because a
        // bind only happens in the call that launched the process.
        assert_eq!(state.reconcile(now, &alive).stale, vec!["unbound"]);
        now += LEASE_GRACE;
        assert_eq!(state.reconcile(now, &alive).stale, vec!["unbound"]);
        assert_eq!(UNBINDABLE_AFTER, 3, "the count this test walks");
        now += LEASE_GRACE;
        let report = state.reconcile(now, &alive);
        assert_eq!(report.unbindable, vec!["unbound".to_string()]);
        assert!(report.stale.is_empty());
        assert_eq!(
            state.status(now).active_by_class["model_work"],
            1,
            "the seat came back"
        );
        assert_eq!(
            state.status(now).runs["run-a"].uncertain_settlements,
            1,
            "its usage is unknown, not zero"
        );
        // The bound one is untouched: it has a live process.
        assert!(state.heartbeat("bound", now).known);
    }

    // C5: a cancelled worker is asked once, told once, and then left
    // alone. The old reconcile re-sent the same signal every 15 s for as
    // long as the process lived, and never escalated.
    #[test]
    fn a_cancelled_dispatch_is_terminated_once_then_killed_once() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        // Cancelled before anything is bound: the caller has no PID to
        // signal, so reconcile owns the first signal too.
        state.cancel_dispatch("d1", t0);
        assert!(state.reconcile(t0, &alive).to_signal.is_empty());
        bind_live(&mut state, "d1", 4242, t0);

        let first = state.reconcile(t0 + Duration::from_secs(15), &alive);
        assert_eq!(
            signalled(&first),
            vec![("d1".to_string(), 4242, Signal::Terminate)]
        );
        // It is still running, but it has been asked: nothing is re-sent
        // while the grace period runs.
        for tick in 1..5 {
            let report = state.reconcile(t0 + Duration::from_secs(15 * tick), &alive);
            assert!(report.to_signal.is_empty(), "no re-sending at tick {tick}");
        }
        // A whole grace period ignored: once, harder.
        let escalation = t0 + CANCEL_ESCALATE_AFTER + Duration::from_secs(1);
        assert_eq!(
            signalled(&state.reconcile(escalation, &alive)),
            vec![("d1".to_string(), 4242, Signal::Kill)]
        );
        // And then never again, whatever the process does.
        for tick in 1..5 {
            let later = escalation + Duration::from_secs(15 * tick);
            assert!(
                state.reconcile(later, &alive).to_signal.is_empty(),
                "the ladder ends"
            );
        }
        // When the process does go, the lease is freed like any other.
        let dead = escalation + LEASE_GRACE + Duration::from_secs(1);
        assert_eq!(
            state.reconcile(dead, &|pid| pid != 4242).dropped,
            vec!["d1".to_string()]
        );
    }

    // C5: the worker that saw the cancellation and is stopping gives the
    // seat back now, instead of holding it until lease grace.
    #[test]
    fn acknowledging_a_cancellation_frees_the_seat_at_once() {
        let mut state = state();
        let t0 = Instant::now();
        let mut first = req("d1", "run-a", "tab-a");
        first.reserve_micros = 5;
        assert!(granted(state.request(&first, t0)));
        bind_live(&mut state, "d1", 4242, t0);
        state.cancel_dispatch("d1", t0);
        assert!(state.heartbeat("d1", t0).cancelled);

        assert!(state.acknowledge_cancel("d1", t0).applied());
        assert!(
            !state.heartbeat("d1", t0).known,
            "the lease is gone, not waiting out five minutes of grace"
        );
        let status = state.status(t0);
        assert_eq!(status.active_by_class.get("model_work"), None);
        assert_eq!(status.runs["run-a"].reserved_micros, 0);
        assert_eq!(
            status.runs["run-a"].uncertain_settlements, 1,
            "a cancelled worker's usage is unknown, never zero"
        );
        // Nothing is left to signal, and the ID cannot come back.
        assert!(state.reconcile(t0, &alive).to_signal.is_empty());
        assert_eq!(
            state.acknowledge_cancel("d1", t0),
            LifecycleOutcome::Unknown
        );
        assert!(matches!(
            state.request(&first, t0),
            Decision::Refused {
                code: Refusal::AlreadyFinished,
                ..
            }
        ));
    }

    // C8: idempotency used to lapse the moment a dispatch settled — the
    // entry was dropped, and the very same ID could be admitted again.
    #[test]
    fn a_settled_dispatch_id_is_refused_not_re_admitted() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        state.release("d1", t0);
        state.settle("d1", Some(7), t0);
        assert!(matches!(
            state.request(&req("d1", "run-a", "tab-a"), t0),
            Decision::Refused {
                code: Refusal::AlreadyFinished,
                ..
            }
        ));
        assert_eq!(
            state.status(t0).runs["run-a"].admitted_total,
            1,
            "the refusal admitted nothing"
        );
        // The memory is bounded and FIFO: past the cap the oldest IDs are
        // forgotten, which is what a coordinator restart does anyway.
        for n in 0..TERMINAL_MEMORY {
            state.remember_terminal(&format!("filler-{n}"));
        }
        assert_eq!(state.finished.len(), TERMINAL_MEMORY);
        assert!(
            !state.finished.contains("d1"),
            "the oldest fell out of the window"
        );
        assert!(state
            .finished
            .contains(&format!("filler-{}", TERMINAL_MEMORY - 1)));
    }

    // C9: parentage has no production caller yet, so the properties it
    // has to have are asserted at the level that does: admission.
    #[test]
    fn a_child_inherits_depth_and_budget_and_a_waiting_less_parent_cannot_deadlock() {
        let mut state = AdmissionState::new(limits());
        let t0 = Instant::now();
        state.register_run(
            &RunRegistration {
                run_id: "run-a".into(),
                session_id: "tab-a".into(),
                budget_micros: Some(1_000),
                max_agents: Some(10),
                max_depth: Some(3),
            },
            t0,
        );
        let mut root = req("root", "run-a", "tab-a");
        root.reserve_micros = 400;
        assert!(granted(state.request(&root, t0)));
        assert_eq!(state.depth_of("root"), Some(0));

        // The child names its parent and lies about its depth; what is
        // recorded is derived from the parent, and its reservation comes
        // out of the same root budget.
        let mut kid = child("kid", "run-a", "tab-a", "root", 0);
        kid.reserve_micros = 400;
        assert!(granted(state.request(&kid, t0)));
        assert_eq!(state.depth_of("kid"), Some(1), "derived from the parent");
        assert_eq!(
            state.parentage("kid"),
            Some(Parentage::Known {
                parent: "root".into()
            })
        );
        assert_eq!(
            state.status(t0).runs["run-a"].reserved_micros,
            800,
            "one budget, not one per generation"
        );
        let mut greedy = child("greedy", "run-a", "tab-a", "kid", 2);
        greedy.reserve_micros = 400;
        assert!(
            matches!(
                state.request(&greedy, t0),
                Decision::Refused {
                    code: Refusal::BudgetExceeded,
                    ..
                }
            ),
            "the third generation cannot spend what the first two hold"
        );

        // The parent never says it is waiting — it may not be able to.
        // Its children queue behind the per-session cap (2), the parent
        // keeps its seat, and the scheduler neither spins nor panics.
        let grandkids: Vec<DispatchRequest> = (0..4)
            .map(|n| child(&format!("g{n}"), "run-a", "tab-a", "kid", 2))
            .collect();
        for grandkid in &grandkids {
            let decision = state.request(grandkid, t0);
            assert!(matches!(decision, Decision::Queued { .. }), "{decision:?}");
        }
        let status = state.status(t0);
        assert_eq!(status.active_by_class["model_work"], 2, "the cap holds");
        assert_eq!(status.queued, 4);
        assert_eq!(status.waiting, 0, "nobody claimed to be waiting");
        // Polling the queue over and over changes nothing and costs
        // nothing: no progress is not a deadlock to panic over, it is
        // work waiting for a seat.
        for _ in 0..3 {
            for grandkid in &grandkids {
                assert!(matches!(
                    state.request(grandkid, t0),
                    Decision::Queued { .. }
                ));
            }
        }
        assert_eq!(state.status(t0).queued, 4);
        // The parent finishes, and the queue moves.
        state.release("kid", t0);
        state.settle("kid", Some(0), t0);
        assert_eq!(state.status(t0).queued, 3);
    }

    // C9 / SPEC §23: "concurrent writers need separate worktrees or
    // explicitly non-overlapping write leases", and root verification
    // waits for the relevant leases to be released. The runner does not
    // take these yet — it gives every writing attempt its own worktree —
    // so this is the coordinator-side half, tested on its own.
    #[test]
    fn a_worktree_has_one_writer_and_verification_can_see_it() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("w1", "run-a", "tab-a"), t0)));
        assert!(granted(state.request(&req("w2", "run-b", "tab-b"), t0)));

        assert_eq!(state.writers_active("/wt/alpha"), 0);
        assert!(state.acquire_write("/wt/alpha", "w1", t0).taken());
        assert!(
            state.acquire_write("/wt/alpha", "w1", t0).taken(),
            "the holder asking again is not a second writer"
        );
        assert_eq!(
            state.acquire_write("/wt/alpha", "w2", t0),
            WriteLeaseOutcome::HeldBy {
                holder: "w1".into()
            },
            "two writers in one worktree is the thing this prevents"
        );
        assert_eq!(state.writers_active("/wt/alpha"), 1);
        assert_eq!(
            state.write_lease_holder("/wt/alpha").map(|held| held.0),
            Some("w1")
        );
        assert_eq!(
            state.write_lease_holder("/wt/alpha").map(|held| held.1),
            Some(t0),
            "and since when"
        );
        // A different worktree is a different lease: writers in their own
        // worktrees do not contend at all.
        assert!(state.acquire_write("/wt/beta", "w2", t0).taken());
        assert_eq!(state.status(t0).write_leases.len(), 2);

        assert_eq!(
            state.release_write("/wt/alpha", "w2"),
            ReleaseWriteOutcome::NotTheHolder,
            "releasing somebody else's lease is how two writers happen"
        );
        assert_eq!(
            state.release_write("/wt/alpha", "w1"),
            ReleaseWriteOutcome::Released
        );
        assert_eq!(state.writers_active("/wt/alpha"), 0);
        assert_eq!(
            state.release_write("/wt/alpha", "w1"),
            ReleaseWriteOutcome::NotTheHolder
        );

        // A writer that ends without releasing does not hold a worktree
        // for ever: settlement drops its leases.
        assert!(state.acquire_write("/wt/gamma", "w2", t0).taken());
        state.release("w2", t0);
        state.settle("w2", Some(0), t0);
        assert_eq!(state.writers_active("/wt/gamma"), 0);
        assert_eq!(state.writers_active("/wt/beta"), 0);
        assert!(state.status(t0).write_leases.is_empty());
    }

    #[test]
    fn a_withdrawn_request_never_takes_a_seat_later() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("a1", "run-a", "tab-a"), t0)));
        assert!(granted(state.request(&req("a2", "run-a", "tab-a"), t0)));
        assert!(matches!(
            state.request(&req("a3", "run-a", "tab-a"), t0),
            Decision::Queued { .. }
        ));
        assert_eq!(state.withdraw("a3", t0), WithdrawOutcome::Withdrawn);
        assert_eq!(state.status(t0).queued, 0);
        // Drained-but-unclaimed is withdrawable too, and frees the seat.
        assert!(matches!(
            state.request(&req("a4", "run-a", "tab-a"), t0),
            Decision::Queued { .. }
        ));
        state.release("a1", t0);
        state.settle("a1", Some(0), t0);
        assert_eq!(
            state.status(t0).active_by_class["model_work"],
            2,
            "a4 drained in"
        );
        assert_eq!(state.withdraw("a4", t0), WithdrawOutcome::Withdrawn);
        assert_eq!(state.status(t0).active_by_class["model_work"], 1);
        // A claimed dispatch cannot be withdrawn: its launch may be live.
        assert_eq!(
            state.withdraw("a2", t0),
            WithdrawOutcome::Claimed,
            "a claimed dispatch cannot be withdrawn: its launch may be live"
        );
        assert_eq!(state.status(t0).runs["run-a"].settled_micros, 0);
    }

    // C3: depth, session and reservation are self-reported over a socket.
    #[test]
    fn a_child_cannot_reset_the_depth_cap_by_claiming_zero() {
        let mut state = state();
        let t0 = Instant::now();
        // limits(): max_agent_depth = 2. Parents wait so the session cap
        // does not queue their children.
        assert!(granted(state.request(&req("root", "run-a", "tab-a"), t0)));
        wait_bound(&mut state, "root", 501, t0);
        assert!(granted(
            state.request(&child("kid", "run-a", "tab-a", "root", 1), t0)
        ));
        wait_bound(&mut state, "kid", 502, t0);
        assert_eq!(state.depth_of("kid"), Some(1));
        assert!(granted(
            state.request(&child("deep", "run-a", "tab-a", "kid", 2), t0)
        ));
        assert_eq!(state.depth_of("deep"), Some(2));
        // A grandchild that lies about its depth is measured against its
        // parent's recorded depth, not its own claim.
        let liar = child("liar", "run-a", "tab-a", "deep", 0);
        assert!(matches!(
            state.request(&liar, t0),
            Decision::Refused {
                code: Refusal::DepthExceeded,
                ..
            }
        ));
        // Honest depth from a known parent is recorded as derived.
        let honest = child("honest", "run-a", "tab-a", "root", 9);
        assert!(granted(state.request(&honest, t0)));
        assert_eq!(state.depth_of("honest"), Some(1), "derived, not claimed");
    }

    #[test]
    fn a_hostile_reservation_cannot_wrap_or_credit_the_budget() {
        let mut state = AdmissionState::new(limits());
        let t0 = Instant::now();
        state.register_run(
            &RunRegistration {
                run_id: "run-a".into(),
                session_id: "tab-a".into(),
                budget_micros: Some(1_000),
                max_agents: Some(10),
                max_depth: Some(3),
            },
            t0,
        );
        let mut negative = req("negative", "run-a", "tab-a");
        negative.reserve_micros = -1_000_000;
        assert!(
            matches!(
                state.request(&negative, t0),
                Decision::Refused {
                    code: Refusal::BudgetExceeded,
                    ..
                }
            ),
            "a negative reservation would credit the run"
        );
        let mut huge = req("huge", "run-a", "tab-a");
        huge.reserve_micros = i64::MAX;
        assert!(matches!(
            state.request(&huge, t0),
            Decision::Refused {
                code: Refusal::BudgetExceeded,
                ..
            }
        ));
        // Neither attempt took a seat or moved the ledger.
        let status = state.status(t0);
        assert_eq!(status.runs["run-a"].admitted_total, 0);
        assert_eq!(status.runs["run-a"].reserved_micros, 0);
        // A saturated settlement never wraps into a run that spent nothing.
        let mut ok = req("ok", "run-a", "tab-a");
        ok.reserve_micros = 10;
        assert!(granted(state.request(&ok, t0)));
        state.release("ok", t0);
        state.settle("ok", Some(i64::MAX), t0);
        assert!(state.status(t0).runs["run-a"].settled_micros > 0);
    }

    // C1: the daemon used to unlink its socket under a run that was
    // merely between dispatches.
    #[test]
    fn a_run_between_dispatches_is_not_idle() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        state.release("d1", t0);
        state.settle("d1", Some(1), t0);
        assert!(
            state.dispatches.is_empty(),
            "the dispatch is settled and gone"
        );
        assert!(
            !state.is_idle(),
            "run-a is registered and unfinished: it is verifying, not absent"
        );
        // The root runner reports the run over; now the daemon may exit.
        assert!(state.finish_run("run-a").applied());
        assert!(state.finish_run("run-b").applied());
        assert!(state.finish_run("run-c").applied());
        assert_eq!(state.finish_run("run-unknown"), LifecycleOutcome::Unknown);
        assert!(state.is_idle());
        // Reconcile reaps the terminal rows; a cancelled run is terminal.
        state.reconcile(t0, &|_pid| true);
        assert!(state.status(t0).runs.is_empty());
        // A run that never reports finished still stops holding the
        // daemon open once its root runner has been silent long enough.
        state.register_run(
            &RunRegistration {
                run_id: "run-z".into(),
                session_id: "tab-z".into(),
                budget_micros: None,
                max_agents: None,
                max_depth: None,
            },
            t0,
        );
        assert!(!state.is_idle());
        state.reconcile(t0 + RUN_ABANDON_GRACE + Duration::from_secs(60), &|_pid| {
            true
        });
        assert!(state.is_idle(), "an abandoned run cannot pin the daemon");
    }

    #[test]
    fn resource_classes_are_scheduled_separately() {
        let mut state = state();
        let t0 = Instant::now();
        let mut heavy = req("h1", "run-a", "tab-a");
        heavy.resource = ResourceClass::HeavyLocal;
        assert!(granted(state.request(&heavy, t0)));
        let mut heavy2 = req("h2", "run-b", "tab-b");
        heavy2.resource = ResourceClass::HeavyLocal;
        assert!(matches!(
            state.request(&heavy2, t0),
            Decision::Queued { .. }
        ));
        // A model worker is not blocked by the heavy-command cap.
        assert!(granted(state.request(&req("m1", "run-b", "tab-b"), t0)));
        let mut training = req("t1", "run-c", "tab-c");
        training.resource = ResourceClass::Training;
        assert!(granted(state.request(&training, t0)));
        assert!(!state.is_idle());
    }

    // A3: `heavy_local` and `indexing` come out of ONE cap. Counting
    // them apart let each fill `max_heavy_commands` on its own, so the
    // machine ran twice the configured local load and `over_admitted`
    // never mentioned indexing at all.
    #[test]
    fn classes_that_share_a_cap_are_counted_against_it_together() {
        let mut state = AdmissionState::new(ConcurrencyLimits {
            max_heavy_commands: Some(2),
            ..limits()
        });
        let t0 = Instant::now();
        for (run, session) in [("run-a", "tab-a"), ("run-b", "tab-b")] {
            state.register_run(
                &RunRegistration {
                    run_id: run.into(),
                    session_id: session.into(),
                    budget_micros: None,
                    max_agents: None,
                    max_depth: None,
                },
                t0,
            );
        }
        let local = |id: &str, class: ResourceClass| DispatchRequest {
            resource: class,
            ..req(id, "run-a", "tab-a")
        };
        // Two heavy commands fill the cap of two.
        assert!(granted(
            state.request(&local("h1", ResourceClass::HeavyLocal), t0)
        ));
        assert!(granted(
            state.request(&local("h2", ResourceClass::HeavyLocal), t0)
        ));
        // Indexing shares that cap, so the next two queue — they do not
        // get a second cap's worth of the same machine.
        assert!(matches!(
            state.request(&local("i1", ResourceClass::Indexing), t0),
            Decision::Queued { .. }
        ));
        assert!(matches!(
            state.request(&local("i2", ResourceClass::Indexing), t0),
            Decision::Queued { .. }
        ));
        let status = state.status(t0);
        assert_eq!(status.active_by_class["heavy_local"], 2);
        assert_eq!(status.active_by_class.get("indexing"), None);
        assert_eq!(status.queued, 2);
        assert_eq!(status.over_admitted, 0);
        // One heavy command ends and an indexing job takes its place —
        // one in, one out, the group total unchanged.
        state.release("h1", t0);
        state.settle("h1", Some(0), t0);
        assert_eq!(
            state.request(&local("i1", ResourceClass::Indexing), t0),
            Decision::Granted
        );
        let status = state.status(t0);
        assert_eq!(status.active_by_class["heavy_local"], 1);
        assert_eq!(status.active_by_class["indexing"], 1);
        assert_eq!(status.over_admitted, 0);
        // And a cap that IS exceeded counts both classes towards the
        // overshoot, instead of showing half of it.
        state.set_max_over_admitted(None);
        bind_live(&mut state, "i1", 6001, t0);
        assert_eq!(
            state.mark_waiting("i1", t0, &alive),
            WaitOutcome::Waiting,
            "the seat comes back and the queued job takes it"
        );
        let status = state.status(t0);
        assert_eq!(status.active_by_class["heavy_local"], 1);
        assert_eq!(
            status.active_by_class["indexing"], 1,
            "the queued indexing job took the waiting one's seat"
        );
        assert_eq!(status.queued, 0);
        assert_eq!(state.resume("i1", t0), ResumeOutcome::Resumed);
        assert_eq!(
            state.status(t0).over_admitted,
            1,
            "the resumed indexing job overshoots the group's cap, and it shows"
        );
    }

    // A4: a dispatch drained from the queue holds a seat before anybody
    // claims it. The hard limits are not re-checked when the caller
    // finally polls, so cancellation has to be: a cancelled run used to
    // get `Granted` and launch a worker.
    #[test]
    fn a_dispatch_cancelled_while_it_waited_for_a_seat_is_not_granted() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("a1", "run-a", "tab-a"), t0)));
        assert!(granted(state.request(&req("a2", "run-a", "tab-a"), t0)));
        assert!(matches!(
            state.request(&req("a3", "run-a", "tab-a"), t0),
            Decision::Queued { .. }
        ));
        // A seat frees, so a3 is drained in — admitted, unclaimed.
        state.release("a1", t0);
        state.settle("a1", Some(0), t0);
        assert_eq!(state.status(t0).active_by_class["model_work"], 2);
        // The run is cancelled before its caller polls again.
        state.cancel_run("run-a", t0);
        assert!(matches!(
            state.request(&req("a3", "run-a", "tab-a"), t0),
            Decision::Refused {
                code: Refusal::RunCancelled,
                ..
            }
        ));
        // And the seat it was holding came back with it.
        assert_eq!(
            state.status(t0).active_by_class.get("model_work"),
            Some(&1),
            "the unclaimed seat is freed, not left for lease grace"
        );
    }

    // A14: a request that was never launched is not a dispatch that ran.
    // Withdrawing a drained-but-unclaimed one used to settle it, which
    // spent one of the run's aggregate agent slots for ever and
    // remembered the ID as finished so it could never be asked for again.
    #[test]
    fn withdrawing_an_unclaimed_dispatch_gives_back_its_agent_slot() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("a1", "run-a", "tab-a"), t0)));
        assert!(granted(state.request(&req("a2", "run-a", "tab-a"), t0)));
        assert!(matches!(
            state.request(&req("a3", "run-a", "tab-a"), t0),
            Decision::Queued { .. }
        ));
        state.release("a1", t0);
        state.settle("a1", Some(0), t0);
        let admitted = state.status(t0).runs["run-a"].admitted_total;
        assert_eq!(admitted, 3, "a3 drained in and counted");
        assert_eq!(state.withdraw("a3", t0), WithdrawOutcome::Withdrawn);
        let run = &state.status(t0).runs["run-a"];
        assert_eq!(
            run.admitted_total, 2,
            "a request nobody launched does not spend an agent slot"
        );
        assert_eq!(run.settled_micros, 0, "and settles nothing");
        // The ID is free again: it was never a dispatch that ran.
        assert_eq!(
            state.request(&req("a3", "run-a", "tab-a"), t0),
            Decision::Granted
        );
    }

    // A13: the moment a PID was checked alive is evidence with an age,
    // and both the snapshot and every signal handed over carry it.
    #[test]
    fn a_bound_process_is_visible_with_the_age_of_its_liveness_check() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        assert!(state.status(t0).bound_processes.is_empty());
        bind_live(&mut state, "d1", 4242, t0);
        let later = t0 + Duration::from_secs(30);
        assert_eq!(
            state.status(later).bound_processes["d1"],
            BoundProcess {
                pid: 4242,
                bound_for_secs: 30
            }
        );
        let signals = state.cancel_dispatch("d1", later);
        assert_eq!(signals[0].bound_at, t0, "the signal names its evidence");
    }

    // A7: the ladder only advances on a delivery the OS took. A step the
    // caller could not deliver comes back, so the dispatch is asked
    // again instead of being recorded as asked and never told.
    #[test]
    fn an_undelivered_signal_is_offered_again_next_reconcile() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(granted(state.request(&req("d1", "run-a", "tab-a"), t0)));
        bind_live(&mut state, "d1", 4242, t0);
        state.cancel_dispatch("d1", t0);
        // Nothing is re-sent while the grace period runs...
        assert!(state.reconcile(t0, &alive).to_signal.is_empty());
        // ...unless the terminate never actually went.
        state.signal_undelivered("d1", Signal::Terminate);
        assert_eq!(
            signalled(&state.reconcile(t0, &alive)),
            vec![("d1".to_string(), 4242, Signal::Terminate)]
        );
        // Giving back a step nobody is on changes nothing.
        state.signal_undelivered("d1", Signal::Kill);
        assert!(state.reconcile(t0, &alive).to_signal.is_empty());
    }

    #[test]
    fn local_gate_round_trips_through_the_trait() {
        let gate = LocalGate::new(limits());
        gate.register_run(&RunRegistration {
            run_id: "run-a".into(),
            session_id: "tab-a".into(),
            budget_micros: None,
            max_agents: None,
            max_depth: None,
        })
        .expect("register");
        assert_eq!(
            gate.admit(&req("d1", "run-a", "tab-a")).expect("admit"),
            Decision::Granted
        );
        // A real, live PID: the gate binds through the real process
        // table, and waiting requires one (C6/C8).
        gate.bind("d1", Some("agent"), Some(std::process::id()))
            .expect("bind");
        assert_eq!(
            gate.bind("d1", None, Some(u32::MAX - 7)).expect("bind"),
            BindOutcome::PidNotAlive,
            "a PID nothing is running is refused at the gate too"
        );
        assert!(gate.heartbeat("d1").expect("heartbeat").known);
        gate.mark_waiting("d1").expect("waiting");
        gate.resume("d1").expect("resume");
        gate.release("d1").expect("release");
        gate.settle("d1", Some(5)).expect("settle");
        assert_eq!(gate.status().runs["run-a"].settled_micros, 5);
        // A worker that saw its cancellation and is stopping: the seat
        // and the reservation go now, with usage unknown (C5).
        assert_eq!(
            gate.admit(&req("d2", "run-a", "tab-a")).expect("admit"),
            Decision::Granted
        );
        gate.acknowledge_cancel("d2").expect("ack");
        assert_eq!(gate.status().runs["run-a"].uncertain_settlements, 1);
        assert_eq!(gate.status().runs["run-a"].active, 0);
        gate.cancel_run("run-a");
        assert_eq!(gate.enforcement(), Enforcement::InProcess);
    }
}
