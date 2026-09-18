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
//! Nothing here touches a socket, a clock or a process table: every method
//! takes `now`, and liveness is a callback. That is what makes the §23
//! concurrency scenarios testable without three real Claude Code tabs.

use std::collections::BTreeMap;
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
    Refused { code: String, detail: String },
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
    /// waiting: bounded by the number of waiters, and visible.
    pub over_admitted: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReconcileReport {
    /// Leases freed because their process is provably gone.
    pub dropped: Vec<String>,
    /// Past grace with no process to check; kept, flagged.
    pub stale: Vec<String>,
    /// Bound processes of cancelled dispatches that the caller should
    /// signal; the state machine never signals anything itself.
    pub to_signal: Vec<(String, u32)>,
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
    /// Seat given back while awaiting children (SPEC §23).
    waiting: bool,
    /// A caller has received `Granted` for this ID.
    claimed: bool,
    /// The seat is released; the entry lingers only until settlement.
    released: bool,
    /// Usage has been settled (actual or unknown); the entry lingers
    /// only until release.
    settled: bool,
    pid: Option<u32>,
    agent_id: Option<String>,
    last_heartbeat: Instant,
    cancelled: bool,
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
}

impl AdmissionState {
    pub fn new(limits: ConcurrencyLimits) -> Self {
        Self {
            limits,
            sessions: BTreeMap::new(),
            runs: BTreeMap::new(),
            dispatches: BTreeMap::new(),
            queue: Vec::new(),
        }
    }

    pub fn limits(&self) -> &ConcurrencyLimits {
        &self.limits
    }

    /// Idempotent (SPEC §23: joins/resumes register idempotently).
    pub fn register_session(&mut self, session_id: &str) {
        self.sessions
            .entry(session_id.to_string())
            .or_insert(Session { last_served: None });
    }

    /// Idempotent; a repeat can only narrow limits and budget.
    pub fn register_run(&mut self, registration: &RunRegistration) {
        self.register_session(&registration.session_id);
        match self.runs.get_mut(&registration.run_id) {
            Some(run) => {
                run.budget = min_opt_i64(run.budget, registration.budget_micros);
                run.max_agents = min_opt(run.max_agents, registration.max_agents);
                run.max_depth = min_opt(run.max_depth, registration.max_depth);
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
        if let Some(existing) = self.dispatches.get_mut(&request.dispatch_id) {
            if existing.claimed {
                return Decision::AlreadyAdmitted;
            }
            existing.claimed = true;
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
            self.admit(request.clone(), now, true);
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
                code: "unknown_run".into(),
                detail: format!(
                    "run {} is not registered; the root runner registers before dispatch",
                    request.run_id
                ),
            });
        };
        if run.cancelled {
            return Some(Decision::Refused {
                code: "run_cancelled".into(),
                detail: format!("run {} was cancelled", request.run_id),
            });
        }
        let max_depth = min_opt(run.max_depth, self.limits.max_agent_depth);
        if max_depth.is_some_and(|max| request.depth > max) {
            return Some(Decision::Refused {
                code: "depth_exceeded".into(),
                detail: format!(
                    "depth {} exceeds the effective maximum {}",
                    request.depth,
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
                code: "run_agent_cap".into(),
                detail: format!(
                    "run {} has used its aggregate agent cap ({}); no work package resets it",
                    request.run_id,
                    max_agents.unwrap_or(0)
                ),
            });
        }
        if let Some(budget) = run.budget {
            let committed = run.settled + self.outstanding_reservations(&request.run_id);
            let reserve = request.reserve_micros.max(0);
            if committed + reserve > budget {
                return Some(Decision::Refused {
                    code: "budget_exceeded".into(),
                    detail: format!(
                        "reserving {reserve} on top of {committed} committed exceeds the run budget {budget}"
                    ),
                });
            }
        }
        None
    }

    fn outstanding_reservations(&self, run_id: &str) -> i64 {
        self.dispatches
            .values()
            .filter(|dispatch| dispatch.run_id == run_id)
            .map(|dispatch| dispatch.reserved)
            .sum()
    }

    fn active_count(&self, class: ResourceClass, session: Option<&str>) -> u32 {
        self.dispatches
            .values()
            .filter(|dispatch| {
                dispatch.class == class
                    && !dispatch.waiting
                    && !dispatch.released
                    && session.is_none_or(|session| dispatch.session_id == session)
            })
            .count() as u32
    }

    fn class_cap(&self, class: ResourceClass) -> Option<u32> {
        match class {
            ResourceClass::ModelWork => self.limits.max_active_agents,
            ResourceClass::HeavyLocal | ResourceClass::Indexing => self.limits.max_heavy_commands,
            ResourceClass::Training => self.limits.max_training_jobs,
        }
    }

    /// Capacity caps only; hard limits were checked before queueing.
    fn admissible(&self, request: &DispatchRequest) -> bool {
        if self
            .class_cap(request.resource)
            .is_some_and(|cap| self.active_count(request.resource, None) >= cap)
        {
            return false;
        }
        if request.resource == ResourceClass::ModelWork {
            if let Some(per_session) = self.limits.max_active_agents_per_session {
                if self.active_count(ResourceClass::ModelWork, Some(&request.session_id))
                    >= per_session
                {
                    return false;
                }
            }
        }
        true
    }

    fn admit(&mut self, request: DispatchRequest, now: Instant, claimed: bool) {
        let parent_known = request
            .parent_dispatch
            .as_ref()
            .is_none_or(|parent| self.dispatches.contains_key(parent));
        if let Some(run) = self.runs.get_mut(&request.run_id) {
            run.admitted_total += 1;
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
                depth: request.depth,
                class: request.resource,
                reserved: request.reserve_micros.max(0),
                waiting: false,
                claimed,
                released: false,
                settled: false,
                pid: None,
                agent_id: None,
                last_heartbeat: now,
                cancelled: false,
            },
        );
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
            self.admit(queued.request, now, false);
        }
    }

    /// Bind the harness agent/session identity and process to the
    /// reservation once the launch happened (SPEC §23).
    pub fn bind(&mut self, dispatch_id: &str, agent_id: Option<&str>, pid: Option<u32>) -> bool {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return false;
        };
        if agent_id.is_some() {
            dispatch.agent_id = agent_id.map(str::to_string);
        }
        if pid.is_some() {
            dispatch.pid = pid;
        }
        true
    }

    pub fn heartbeat(&mut self, dispatch_id: &str, now: Instant) -> HeartbeatStatus {
        match self.dispatches.get_mut(dispatch_id) {
            Some(dispatch) => {
                dispatch.last_heartbeat = now;
                let run_cancelled = self
                    .runs
                    .get(&dispatch.run_id)
                    .is_some_and(|run| run.cancelled);
                HeartbeatStatus {
                    known: true,
                    cancelled: dispatch.cancelled || run_cancelled,
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
    pub fn mark_waiting(&mut self, dispatch_id: &str, now: Instant) -> bool {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return false;
        };
        dispatch.waiting = true;
        dispatch.last_heartbeat = now;
        self.drain(now);
        true
    }

    /// The waiting parent's children finished: it takes its seat back
    /// immediately. A resumed parent may overshoot a cap; the overshoot
    /// is bounded by the number of waiters and shown in status, which is
    /// the deliberate trade against re-queueing an admitted parent.
    pub fn resume(&mut self, dispatch_id: &str, now: Instant) -> bool {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return false;
        };
        dispatch.waiting = false;
        dispatch.last_heartbeat = now;
        true
    }

    /// Free the seat. The reservation stays until settled, so a release
    /// before settlement cannot let a sibling spend the same money.
    pub fn release(&mut self, dispatch_id: &str, now: Instant) -> bool {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return false;
        };
        dispatch.released = true;
        self.forget_if_settled(dispatch_id);
        self.drain(now);
        true
    }

    /// Settle a reservation with actual spend. `None` = unknown usage:
    /// the reservation stands in as a lower bound and the run is marked
    /// uncertain; never zero (SPEC §23).
    pub fn settle(&mut self, dispatch_id: &str, spent_micros: Option<i64>, now: Instant) -> bool {
        let Some(dispatch) = self.dispatches.get_mut(dispatch_id) else {
            return false;
        };
        if dispatch.settled {
            // Duplicate lifecycle events are harmless (SPEC §23).
            return true;
        }
        dispatch.settled = true;
        let reserved = std::mem::take(&mut dispatch.reserved);
        let run_id = dispatch.run_id.clone();
        if let Some(run) = self.runs.get_mut(&run_id) {
            match spent_micros {
                Some(spent) => run.settled += spent.max(0),
                None => {
                    run.settled += reserved;
                    run.uncertain += 1;
                }
            }
        }
        self.forget_if_settled(dispatch_id);
        self.drain(now);
        true
    }

    /// The caller gave up on a request it never launched: drop it from
    /// the queue, or, if it was drained meanwhile but never claimed,
    /// free the seat with nothing spent. A claimed dispatch is not
    /// withdrawable — its launch may be in flight.
    pub fn withdraw(&mut self, dispatch_id: &str, now: Instant) -> bool {
        let before = self.queue.len();
        self.queue
            .retain(|queued| queued.request.dispatch_id != dispatch_id);
        if self.queue.len() != before {
            return true;
        }
        let unclaimed = self
            .dispatches
            .get(dispatch_id)
            .is_some_and(|dispatch| !dispatch.claimed);
        if unclaimed {
            self.settle(dispatch_id, Some(0), now);
            self.release(dispatch_id, now);
            return true;
        }
        false
    }

    fn forget_if_settled(&mut self, dispatch_id: &str) {
        let gone = self
            .dispatches
            .get(dispatch_id)
            .is_some_and(|dispatch| dispatch.released && dispatch.settled);
        if gone {
            self.dispatches.remove(dispatch_id);
        }
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
    pub fn cancel_dispatch(&mut self, dispatch_id: &str, now: Instant) -> Vec<(String, u32)> {
        let subtree = self.descendants(dispatch_id);
        let mut to_signal = Vec::new();
        for id in &subtree {
            if let Some(dispatch) = self.dispatches.get_mut(id) {
                dispatch.cancelled = true;
                if let Some(pid) = dispatch.pid {
                    to_signal.push((id.clone(), pid));
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
    pub fn cancel_run(&mut self, run_id: &str, now: Instant) -> Vec<(String, u32)> {
        let mut to_signal = Vec::new();
        if let Some(run) = self.runs.get_mut(run_id) {
            run.cancelled = true;
        }
        for (id, dispatch) in self.dispatches.iter_mut() {
            if dispatch.run_id == run_id {
                dispatch.cancelled = true;
                if let Some(pid) = dispatch.pid {
                    to_signal.push((id.clone(), pid));
                }
            }
        }
        self.queue.retain(|queued| queued.request.run_id != run_id);
        self.drain(now);
        to_signal
    }

    /// Cancel one session's runs, never another tab's.
    pub fn cancel_session(&mut self, session_id: &str, now: Instant) -> Vec<(String, u32)> {
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

    /// A cancelled dispatch whose caller acknowledged the cancellation:
    /// freed with unknown usage unless settled.
    pub fn acknowledge_cancel(&mut self, dispatch_id: &str, now: Instant) -> bool {
        if !self.dispatches.contains_key(dispatch_id) {
            return false;
        }
        self.settle(dispatch_id, None, now);
        self.release(dispatch_id, now)
    }

    /// Lease reconciliation. `alive(pid)` is the process table. A stale
    /// lease with a dead bound process is dropped (its reservation
    /// settles as unknown); a stale lease with no process to check is
    /// kept and flagged; cancelled dispatches with bound processes are
    /// handed back to be signalled.
    pub fn reconcile(&mut self, now: Instant, alive: &dyn Fn(u32) -> bool) -> ReconcileReport {
        let mut report = ReconcileReport::default();
        let ids: Vec<String> = self.dispatches.keys().cloned().collect();
        for id in ids {
            let (stale, pid, cancelled) = {
                let dispatch = &self.dispatches[&id];
                (
                    now.saturating_duration_since(dispatch.last_heartbeat) >= LEASE_GRACE,
                    dispatch.pid,
                    dispatch.cancelled,
                )
            };
            if cancelled {
                if let Some(pid) = pid {
                    if alive(pid) {
                        report.to_signal.push((id.clone(), pid));
                    }
                }
            }
            if !stale {
                continue;
            }
            match pid {
                Some(pid) if !alive(pid) => {
                    self.settle(&id, None, now);
                    self.release(&id, now);
                    report.dropped.push(id);
                }
                Some(_) => {}
                None => report.stale.push(id),
            }
        }
        report
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
            self.register_run(&RunRegistration {
                run_id: request.run_id.clone(),
                session_id: request.session_id.clone(),
                budget_micros: None,
                max_agents: None,
                max_depth: None,
            });
        }
        self.admit(request.clone(), now, true);
        self.bind(&request.dispatch_id, agent_id, pid);
    }

    pub fn status(&self, now: Instant) -> StatusSnapshot {
        let mut active_by_class: BTreeMap<String, u32> = BTreeMap::new();
        let mut waiting = 0;
        let mut stale_leases = 0;
        for dispatch in self.dispatches.values() {
            if dispatch.released {
                continue;
            }
            if dispatch.waiting {
                waiting += 1;
                continue;
            }
            *active_by_class
                .entry(dispatch.class.as_str().to_string())
                .or_default() += 1;
            if now.saturating_duration_since(dispatch.last_heartbeat) >= LEASE_GRACE
                && dispatch.pid.is_none()
            {
                stale_leases += 1;
            }
        }
        let over_admitted = [
            ResourceClass::ModelWork,
            ResourceClass::HeavyLocal,
            ResourceClass::Training,
        ]
        .into_iter()
        .map(|class| {
            let active = self.active_count(class, None);
            self.class_cap(class)
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
                            .filter(|d| !d.waiting && !d.released)
                            .count() as u32,
                        waiting: dispatches
                            .iter()
                            .filter(|d| d.waiting && !d.released)
                            .count() as u32,
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
        }
    }

    /// Parent evidence for a dispatch: `None` = unknown dispatch,
    /// `Some(None)` = root, `Some(Some((parent, known)))` otherwise.
    pub fn parentage(&self, dispatch_id: &str) -> Option<Option<(String, bool)>> {
        self.dispatches.get(dispatch_id).map(|dispatch| {
            dispatch
                .parent
                .clone()
                .map(|parent| (parent, dispatch.parent_known))
        })
    }

    pub fn depth_of(&self, dispatch_id: &str) -> Option<u32> {
        self.dispatches
            .get(dispatch_id)
            .map(|dispatch| dispatch.depth)
    }

    /// True when nothing is registered as active, waiting or queued — the
    /// coordinator's idle-exit condition.
    pub fn is_idle(&self) -> bool {
        self.dispatches.is_empty() && self.queue.is_empty()
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
    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<(), GateError>;
    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError>;
    fn mark_waiting(&self, dispatch_id: &str) -> Result<(), GateError>;
    fn resume(&self, dispatch_id: &str) -> Result<(), GateError>;
    fn release(&self, dispatch_id: &str) -> Result<(), GateError>;
    fn settle(&self, dispatch_id: &str, spent_micros: Option<i64>) -> Result<(), GateError>;
    /// Abandon a request that was never launched.
    fn withdraw(&self, dispatch_id: &str) -> Result<(), GateError>;
    /// What this gate can enforce, for reports (SPEC §23: observed-only
    /// paths are labelled, never claimed as guarantees).
    fn enforcement(&self) -> &'static str;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateError(pub String);

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "admission unavailable: {}", self.0)
    }
}

impl std::error::Error for GateError {}

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

    pub fn status(&self) -> StatusSnapshot {
        self.state
            .lock()
            .expect("admission lock")
            .status(Instant::now())
    }

    pub fn cancel_run(&self, run_id: &str) {
        self.state
            .lock()
            .expect("admission lock")
            .cancel_run(run_id, Instant::now());
    }
}

impl Gate for LocalGate {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError> {
        self.state
            .lock()
            .expect("admission lock")
            .register_run(registration);
        Ok(())
    }

    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError> {
        Ok(self
            .state
            .lock()
            .expect("admission lock")
            .request(request, Instant::now()))
    }

    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<(), GateError> {
        self.state
            .lock()
            .expect("admission lock")
            .bind(dispatch_id, agent_id, pid);
        Ok(())
    }

    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError> {
        Ok(self
            .state
            .lock()
            .expect("admission lock")
            .heartbeat(dispatch_id, Instant::now()))
    }

    fn mark_waiting(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.state
            .lock()
            .expect("admission lock")
            .mark_waiting(dispatch_id, Instant::now());
        Ok(())
    }

    fn resume(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.state
            .lock()
            .expect("admission lock")
            .resume(dispatch_id, Instant::now());
        Ok(())
    }

    fn release(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.state
            .lock()
            .expect("admission lock")
            .release(dispatch_id, Instant::now());
        Ok(())
    }

    fn settle(&self, dispatch_id: &str, spent_micros: Option<i64>) -> Result<(), GateError> {
        self.state.lock().expect("admission lock").settle(
            dispatch_id,
            spent_micros,
            Instant::now(),
        );
        Ok(())
    }

    fn withdraw(&self, dispatch_id: &str) -> Result<(), GateError> {
        self.state
            .lock()
            .expect("admission lock")
            .withdraw(dispatch_id, Instant::now());
        Ok(())
    }

    fn enforcement(&self) -> &'static str {
        "managed (in-process)"
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
            state.register_run(&RunRegistration {
                run_id: run.into(),
                session_id: session.into(),
                budget_micros: None,
                max_agents: None,
                max_depth: None,
            });
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
        state.register_run(&RunRegistration {
            run_id: "run-a".into(),
            session_id: "tab-a".into(),
            budget_micros: Some(1_000),
            max_agents: Some(10),
            max_depth: Some(3),
        });
        let mut root = req("root", "run-a", "tab-a");
        root.reserve_micros = 600;
        assert!(granted(state.request(&root, t0)));
        state.mark_waiting("root", t0);
        let mut kid = child("kid", "run-a", "tab-a", "root", 1);
        kid.reserve_micros = 300;
        assert!(granted(state.request(&kid, t0)));
        assert_eq!(state.parentage("kid"), Some(Some(("root".into(), true))));
        // A grandchild that would overspend the ROOT budget is refused:
        // children cannot independently spend the same allowance.
        let mut grandkid = child("grandkid", "run-a", "tab-a", "kid", 2);
        grandkid.reserve_micros = 200;
        assert!(matches!(
            state.request(&grandkid, t0),
            Decision::Refused { ref code, .. } if code == "budget_exceeded"
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
        state.register_run(&RunRegistration {
            run_id: "run-a".into(),
            session_id: "tab-a".into(),
            budget_micros: Some(1_000_000),
            max_agents: Some(1_000),
            max_depth: Some(99),
        });
        assert_eq!(state.status(t0).runs["run-a"].budget_micros, Some(1_000));
    }

    #[test]
    fn depth_and_aggregate_agent_caps_are_hard_limits() {
        let mut state = state();
        let t0 = Instant::now();
        assert!(matches!(
            state.request(&child("deep", "run-a", "tab-a", "nobody", 3), t0),
            Decision::Refused { ref code, .. } if code == "depth_exceeded"
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
            Decision::Refused { ref code, .. } if code == "run_agent_cap"
        ));
        assert!(matches!(
            state.request(&req("x", "run-unknown", "tab-a"), t0),
            Decision::Refused { ref code, .. } if code == "unknown_run"
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
        for tab in ["a", "b"] {
            for n in 0..2 {
                state.mark_waiting(&format!("p-{tab}-{n}"), t0);
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
        state.resume("p-a-0", t0);
        assert_eq!(state.status(t0).over_admitted, 0);
        state.resume("p-a-1", t0);
        assert_eq!(
            state.status(t0).over_admitted,
            1,
            "overshoot is shown, not hidden"
        );
    }

    // SPEC §23 acceptance: simultaneous dispatches cannot reserve the
    // same remaining budget twice; missing terminal usage stays uncertain.
    #[test]
    fn reservations_are_exclusive_and_unknown_usage_stays_uncertain() {
        let mut state = AdmissionState::new(limits());
        let t0 = Instant::now();
        state.register_run(&RunRegistration {
            run_id: "run-a".into(),
            session_id: "tab-a".into(),
            budget_micros: Some(100),
            max_agents: None,
            max_depth: None,
        });
        let mut first = req("d1", "run-a", "tab-a");
        first.reserve_micros = 60;
        let mut second = req("d2", "run-a", "tab-b");
        second.reserve_micros = 60;
        assert!(granted(state.request(&first, t0)));
        assert!(matches!(
            state.request(&second, t0),
            Decision::Refused { ref code, .. } if code == "budget_exceeded"
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
        state.mark_waiting("root-a", t0);
        assert!(granted(
            state.request(&child("kid-1", "run-a", "tab-a", "root-a", 1), t0)
        ));
        assert!(granted(
            state.request(&child("kid-2", "run-a", "tab-a", "root-a", 1), t0)
        ));
        state.bind("kid-1", Some("agent-1"), Some(4242));
        assert!(matches!(
            state.request(&child("grandkid", "run-a", "tab-a", "kid-1", 2), t0),
            Decision::Queued { .. }
        ));
        assert!(granted(state.request(&req("root-b", "run-b", "tab-b"), t0)));

        let to_signal = state.cancel_dispatch("kid-1", t0);
        assert_eq!(to_signal, vec![("kid-1".to_string(), 4242)]);
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
            Decision::Refused { ref code, .. } if code == "run_cancelled"
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
            Some(Some(("gone-parent".into(), false)))
        );
        assert_eq!(state.request(&orphan, t0), Decision::AlreadyAdmitted);

        // Duplicate lifecycle events are harmless.
        assert!(state.bind("orphan", Some("agent-7"), Some(7777)));
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
        assert!(state.withdraw("a3", t0));
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
        assert!(state.withdraw("a4", t0));
        assert_eq!(state.status(t0).active_by_class["model_work"], 1);
        // A claimed dispatch cannot be withdrawn: its launch may be live.
        assert!(!state.withdraw("a2", t0));
        assert_eq!(state.status(t0).runs["run-a"].settled_micros, 0);
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
        gate.bind("d1", Some("agent"), None).expect("bind");
        assert!(gate.heartbeat("d1").expect("heartbeat").known);
        gate.mark_waiting("d1").expect("waiting");
        gate.resume("d1").expect("resume");
        gate.release("d1").expect("release");
        gate.settle("d1", Some(5)).expect("settle");
        assert_eq!(gate.status().runs["run-a"].settled_micros, 5);
        gate.cancel_run("run-a");
        assert_eq!(gate.enforcement(), "managed (in-process)");
    }
}
