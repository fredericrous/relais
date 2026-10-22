//! The heavy-command lease (SPEC §23, ADR-0010): every setup step and every
//! check, at the base and at the candidate, takes `weight` tokens of the
//! machine-wide `HeavyLocal` class from the coordinator before it starts,
//! holds them while its process group runs, and gives them back after.
//!
//! Admission has four parts, in this order:
//!
//! 1. the coordinator's tokens (`weight` of them, queued behind whoever
//!    holds them);
//! 2. when `[resources] shared_host_slots` names amont's directory, `weight`
//!    of its existing `slot-*` files, locked with `flock`, all or none, so
//!    amont's heavy checks and relais's share one host queue (amont
//!    ADR-0009). The directory and its files are amont's: relais creates
//!    neither, sizes the queue by counting the files, and falls back to the
//!    coordinator's queue alone (said once as `host_slots_off`) when there
//!    are none. Where an ancestor already holds slots
//!    (`AMONT_HOST_SLOT=held` in relais's own environment) nothing is
//!    queued for, and the variable is passed on;
//! 3. the child's environment: the parallelism bounds below, then the
//!    check's own `env`;
//! 4. once the process group exists, its id goes to the coordinator, which
//!    from then on keeps the tokens for as long as a member of the group
//!    runs, even if this process dies.
//!
//! Time spent in 1 and 2 is queue time: it draws on `admission_wait_seconds`
//! and `total_seconds`, never on work (`Budgets::wait`).
//!
//! The disk floor holds a lease apart from the queue (SPEC §23). The
//! coordinator grants only while the volumes the run writes stay above the
//! floor with the running leases' estimates set aside, and says
//! `WaitingForDisk` when the disk is what holds the lease; the runner then
//! checks the floor again once the lease is granted, because space can drop
//! between the grant and the use, and gives the lease back and waits when
//! it has. Waiting for the disk draws on `disk_wait_seconds` and the total,
//! never on `admission_wait_seconds` or work.
//!
//! The parallelism bounds limit how many jobs a child starts. They are not
//! a CPU or memory cap: a single job can still use every core and any
//! amount of memory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::admission::{
    BindOutcome, Decision, DispatchRequest, DispatchSource, Gate, GateError, Refusal,
    ResourceClass, RunRegistration, ADMISSION_POLL,
};
use crate::disk::{DiskCheck, Shortfall};
use crate::policy::{Budgets, Exhausted, WaitKind};
use crate::procs::Ended;
use crate::protocol::{Event, Events};

/// How often a queued command says it is still queued.
pub const ANNOUNCE_EVERY: Duration = Duration::from_secs(30);

/// The environment variable a held host slot exports, so an amont started
/// inside the check does not queue again (amont ADR-0009).
pub const HOST_SLOT_ENV: &str = "AMONT_HOST_SLOT";

/// How often a lease the coordinator could not tie to a process group says
/// it is alive while its check runs (the coordinator frees a lease it has
/// not heard from for `LEASE_GRACE`, 300 s).
pub const LEASE_HEARTBEAT_EVERY: Duration = Duration::from_secs(30);

/// How long an unreachable coordinator is retried before the run stops.
/// Ten minutes, because a machine-wide stall of 5 minutes was observed on
/// 2026-10-22: a bound under that would stop runs that a short wait saves.
pub const UNREACHABLE_BOUND: Duration = Duration::from_secs(600);

/// The first wait between tries of an unreachable coordinator; it doubles
/// up to [`UNREACHABLE_BACKOFF_MAX`].
pub const UNREACHABLE_BACKOFF_START: Duration = Duration::from_secs(1);

/// The longest wait between tries of an unreachable coordinator.
pub const UNREACHABLE_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The `class` of the `waiting_for_slot` event a run emits on each retry
/// while the coordinator is unreachable (position 0, `waited_s` the time
/// spent retrying), so the run stays visibly alive.
pub const COORDINATOR_UNREACHABLE_CLASS: &str = "coordinator_unreachable";

/// Whether relais's own environment says an ancestor already holds host
/// slots for this process tree (`AMONT_HOST_SLOT=held`, amont ADR-0009).
/// Read at the boundary (the runner), passed in as a fact.
pub fn ancestor_holds_slots(value: Option<&str>) -> bool {
    value == Some("held")
}

/// Waiting, made injectable: a queue sleeps through this, so a test that
/// owns the clock moves it instead of waiting.
pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration);
}

/// The real thing: the thread sleeps.
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The tokens a machine hands out when nobody configured a number: a
/// quarter of its cores, at least one.
pub fn default_heavy_commands(cores: u32) -> u32 {
    (cores / 4).max(1)
}

/// `max_heavy_commands` in force: the configured number, else the default.
pub fn heavy_cap(configured: Option<u32>, cores: u32) -> u32 {
    configured
        .unwrap_or_else(|| default_heavy_commands(cores))
        .max(1)
}

/// The cores this machine has, never zero.
pub fn cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(u32::MAX))
        .unwrap_or(1)
}

/// The first command of a profile whose weight no machine setting can
/// grant (none, or above `cap`), as the sentence that refuses the profile.
/// Asked when the profile is loaded for a run, with or without a
/// coordinator, so a weight that could never be admitted is not found
/// halfway through the run.
pub fn overweight<'c>(
    commands: impl IntoIterator<Item = &'c crate::policy::CommandSpec>,
    cap: u32,
) -> Option<String> {
    commands
        .into_iter()
        .find(|command| command.heavy_weight() == 0 || command.heavy_weight() > cap)
        .map(|command| {
            format!(
                "`{}` asks for a weight of {}, outside 1..={cap} (`max_heavy_commands`); it could \
                 never be admitted",
                command.argv.join(" "),
                command.heavy_weight()
            )
        })
}

/// How many jobs a command of `weight` tokens may start:
/// `max(1, weight * cores / cap)`.
pub fn parallelism(weight: u32, cores: u32, cap: u32) -> u32 {
    let jobs = u64::from(weight) * u64::from(cores) / u64::from(cap.max(1));
    u32::try_from(jobs).unwrap_or(u32::MAX).max(1)
}

/// The environment of a command admitted with `jobs` jobs: the bounds
/// relais sets, with any key the check names itself replaced by the
/// check's value.
pub fn child_environment(jobs: u32, own: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::from([
        ("MAKEFLAGS".to_string(), format!("-j{jobs}")),
        ("CARGO_BUILD_JOBS".to_string(), jobs.to_string()),
        ("RUST_TEST_THREADS".to_string(), jobs.to_string()),
        ("NEXTEST_TEST_THREADS".to_string(), jobs.to_string()),
    ]);
    environment.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
    environment
}

/// The lease's dispatch id: unique per phase (base or candidate), command
/// and try, so the coordinator's refusal of an id that already settled
/// (C8) never refuses a rerun.
///
/// A run that was resumed counts its starts (`epoch`, from the ledger): the
/// id of a lease that settled before the resume is never asked for again.
pub fn dispatch_id(run: &str, phase: &str, label: &str, epoch: u32, attempt: u32) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let (phase, label) = (clean(phase), clean(label));
    match epoch {
        0 => format!("{run}-heavy-{phase}-{label}-{attempt}"),
        epoch => format!("{run}-heavy-{phase}-{label}-e{epoch}-{attempt}"),
    }
}

/// Why a command was not admitted.
#[derive(Debug)]
pub enum LeaseStop {
    /// A budget ran out while the command was queued.
    Exhausted(Exhausted),
    /// The run was cancelled while the command was queued.
    Cancelled,
    /// The coordinator could not be asked, or answered with an error.
    Gate(GateError),
    /// The coordinator decided against the request.
    Refused { code: Refusal, detail: String },
    /// This lease's id is already admitted elsewhere.
    Duplicate(String),
    /// The command asked for a weight no machine setting can grant.
    Weight { weight: u32, cap: u32 },
    /// The command asked for more host slots than the shared directory has
    /// files, so it could never get them.
    HostSlots {
        weight: u32,
        slots: usize,
        dir: String,
    },
}

impl std::fmt::Display for LeaseStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted(which) => write!(f, "a budget ran out while queued ({which:?})"),
            Self::Cancelled => write!(f, "the run was cancelled while the command was queued"),
            Self::Gate(e) => write!(f, "{e}"),
            Self::Refused { detail, .. } => write!(f, "{detail}"),
            Self::Duplicate(id) => write!(f, "lease {id} was already admitted elsewhere"),
            Self::Weight { weight, cap } => write!(
                f,
                "a weight of {weight} is outside 1..={cap} (`max_heavy_commands`); the command \
                 could never be admitted"
            ),
            Self::HostSlots { weight, slots, dir } => write!(
                f,
                "a weight of {weight} is more than the {slots} slot file(s) in {dir} \
                 (`shared_host_slots`); the command could never be admitted"
            ),
        }
    }
}

/// What a command asks to be admitted for.
pub struct Ask<'a> {
    /// `base` or the candidate's attempt prefix.
    pub phase: &'a str,
    pub label: &'a str,
    pub weight: u32,
    /// The check's own `env`, which wins over the bounds.
    pub env: &'a BTreeMap<String, String>,
    pub cancel: Option<&'a AtomicBool>,
}

/// What every command of one run shares: the tries of each command, so a
/// rerun gets a new id; the epoch of this start of the run, so a resumed
/// run never reuses an id that settled before; and whether the shared host
/// slots were already said to be off.
#[derive(Debug, Clone, Default)]
pub struct Tries {
    epoch: u32,
    counts: Arc<Mutex<BTreeMap<String, u32>>>,
    slots_off_said: Arc<AtomicBool>,
}

impl Tries {
    /// The tries of a run's `epoch`-th start (0 the first; the ledger
    /// counts them, `Ledger::next_run_epoch`).
    pub fn for_epoch(epoch: u32) -> Self {
        Self {
            epoch,
            ..Self::default()
        }
    }

    fn next(&self, key: &str) -> u32 {
        let mut tries = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        let slot = tries.entry(key.to_string()).or_insert(0);
        *slot += 1;
        *slot
    }
}

/// Admits the commands of one run.
pub struct HeavyAdmission<'a> {
    /// `None`: no coordinator, nothing is admitted (as for model work).
    pub gate: Option<&'a (dyn Gate + Sync)>,
    pub run_id: String,
    pub session_id: String,
    pub budgets: Budgets,
    pub events: Events,
    /// `max_heavy_commands` in force.
    pub cap: u32,
    pub cores: u32,
    pub slots: Option<PathBuf>,
    /// An ancestor holds host slots for this tree: nothing is queued for
    /// here, and the fact is passed on to the check.
    pub ancestor_holds_slots: bool,
    pub poll: Duration,
    pub announce_every: Duration,
    /// How often a lease that is not tied to a group says it is alive.
    pub heartbeat_every: Duration,
    /// How a queued command waits.
    pub sleeper: Arc<dyn Sleeper>,
    /// What the run registered with the coordinator, to register again
    /// once if a restarted coordinator does not know it.
    pub registration: Option<RunRegistration>,
    pub tries: Tries,
    /// The disk floor and the means to measure it.
    pub disk: DiskCheck<'a>,
    /// Does a failed check's end and output show resource exhaustion? Such
    /// a failure is evidence for a decision and is never retried. Passed in
    /// because the classifier reads `verify`'s outcomes.
    pub exhausted: fn(Ended, &str) -> bool,
}

/// The default `exhausted`: nothing shows exhaustion.
fn never_exhausted(_: Ended, _: &str) -> bool {
    false
}

impl<'a> HeavyAdmission<'a> {
    /// A command outside any run: nothing is queued for and nothing held.
    pub fn unmanaged() -> HeavyAdmission<'static> {
        HeavyAdmission {
            gate: None,
            run_id: String::new(),
            session_id: String::new(),
            budgets: Budgets::unbounded(),
            events: Events::silent(),
            cap: 1,
            cores: 1,
            slots: None,
            ancestor_holds_slots: false,
            poll: ADMISSION_POLL,
            announce_every: ANNOUNCE_EVERY,
            heartbeat_every: LEASE_HEARTBEAT_EVERY,
            sleeper: Arc::new(ThreadSleeper),
            registration: None,
            tries: Tries::default(),
            disk: DiskCheck::off(),
            exhausted: never_exhausted,
        }
    }

    /// Whether this admission belongs to a run. A command outside any run
    /// has no run id: it is bounded by nothing and queued for nothing.
    pub fn tracks_growth(&self) -> bool {
        self.manages()
    }

    fn manages(&self) -> bool {
        !self.run_id.is_empty()
    }

    /// The environment of a command of `weight` tokens: the parallelism
    /// bounds, the host-slot fact when an ancestor holds the slots, and
    /// then the check's own `env`, which wins.
    fn environment(&self, weight: u32, own: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        if !self.manages() {
            return own.clone();
        }
        let mut environment =
            child_environment(parallelism(weight, self.cores, self.cap), &BTreeMap::new());
        if self.ancestor_holds_slots {
            environment.insert(HOST_SLOT_ENV.to_string(), "held".to_string());
        }
        environment.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
        environment
    }

    /// Ask the coordinator. An unreachable coordinator is retried with
    /// backoff (1 s doubling to 30 s) for [`UNREACHABLE_BOUND`], emitting a
    /// `waiting_for_slot` of class [`COORDINATOR_UNREACHABLE_CLASS`] on each
    /// retry. Once, when it has forgotten the run (a
    /// restart empties its registry), register the run again and ask
    /// again. A second `UnknownRun` is the coordinator's answer.
    fn ask(
        &self,
        gate: &(dyn Gate + Sync),
        request: &DispatchRequest,
        registered_again: &mut bool,
        cancel: Option<&AtomicBool>,
    ) -> Result<Decision, LeaseStop> {
        let mut retrying = Unreachable::default();
        loop {
            match gate.admit(request) {
                Ok(Decision::Refused {
                    code: Refusal::UnknownRun,
                    ..
                }) if !*registered_again && self.registration.is_some() => {
                    *registered_again = true;
                    if let Some(registration) = &self.registration {
                        gate.register_run(registration).map_err(LeaseStop::Gate)?;
                    }
                }
                // The coordinator granted this id but its answer was lost
                // (the read timed out): asked again, it says the id is
                // already admitted. The lease is this request's own.
                Ok(Decision::AlreadyAdmitted) if retrying.spent > Duration::ZERO => {
                    return Ok(Decision::Granted)
                }
                Ok(decision) => return Ok(decision),
                Err(e) if e.unavailable() => self.retry_unreachable(e, &mut retrying, cancel)?,
                Err(e) => return Err(LeaseStop::Gate(e)),
            }
        }
    }

    /// One backoff after an unreachable coordinator: stop when cancelled or
    /// when the bound is spent (naming how long it retried), else say the
    /// run is alive and sleep.
    fn retry_unreachable(
        &self,
        error: GateError,
        state: &mut Unreachable,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), LeaseStop> {
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(LeaseStop::Cancelled);
        }
        if state.spent >= UNREACHABLE_BOUND {
            return Err(LeaseStop::Gate(match error {
                GateError::Unavailable {
                    operation,
                    socket,
                    cause,
                } => GateError::Unavailable {
                    operation,
                    socket,
                    cause: format!("{cause}; retried for {} s", state.spent.as_secs()).into(),
                },
                other => other,
            }));
        }
        self.events.emit(Event::WaitingForSlot {
            class: COORDINATOR_UNREACHABLE_CLASS.to_string(),
            position: 0,
            waited_s: state.spent.as_secs(),
        });
        let pause = state
            .next
            .max(UNREACHABLE_BACKOFF_START)
            .min(UNREACHABLE_BOUND - state.spent);
        self.sleeper.sleep(pause);
        state.spent += pause;
        state.next = (pause * 2).min(UNREACHABLE_BACKOFF_MAX);
        Ok(())
    }

    /// Wait for admission, or say why not. With no gate the command is
    /// unadmitted: it runs with the same environment bounds, but queues for
    /// nothing. A weight no machine setting can grant is refused either way.
    pub fn admit(&self, ask: &Ask<'_>) -> Result<Admitted<'a>, LeaseStop> {
        if self.manages() && (ask.weight == 0 || ask.weight > self.cap) {
            return Err(LeaseStop::Weight {
                weight: ask.weight,
                cap: self.cap,
            });
        }
        let environment = self.environment(ask.weight, ask.env);
        let Some(gate) = self.gate else {
            if self.manages() {
                self.wait_for_disk(self.disk.estimate(ask.label), ask.cancel)?;
            }
            return Ok(Admitted::unadmitted(environment));
        };
        // The shared queue is amont's: its files are counted before any
        // token is queued for, so a weight it can never grant is refused
        // without waiting.
        let shared = match (&self.slots, self.ancestor_holds_slots) {
            (Some(dir), false) => slots::inventory(dir, ask.weight, self)?,
            _ => None,
        };
        let estimate = self.disk.estimate(ask.label);
        let mut queue = Queue::new(self);
        let mut registered_again = false;
        loop {
            // Each try has its own id: a lease given back settles, and a
            // settled id is never admitted again.
            let attempt = self.tries.next(&format!("{}\u{1}{}", ask.phase, ask.label));
            let id = dispatch_id(
                &self.run_id,
                ask.phase,
                ask.label,
                self.tries.epoch,
                attempt,
            );
            let request = DispatchRequest {
                dispatch_id: id.clone(),
                run_id: self.run_id.clone(),
                session_id: self.session_id.clone(),
                parent_dispatch: None,
                depth: 0,
                resource: ResourceClass::HeavyLocal,
                reserve_micros: 0,
                source: DispatchSource::NativeRun,
                caller_agent_id: None,
                weight: ask.weight,
                disk: self.disk.ask(estimate),
            };
            loop {
                match self.ask(gate, &request, &mut registered_again, ask.cancel)? {
                    Decision::Granted => break,
                    Decision::AlreadyAdmitted => return Err(LeaseStop::Duplicate(id)),
                    Decision::Refused { code, detail } => {
                        return Err(LeaseStop::Refused { code, detail })
                    }
                    decision => {
                        let holding = match &decision {
                            Decision::WaitingForDisk {
                                volume,
                                available,
                                floor,
                                reserved,
                            } => Holding::Disk(Shortfall {
                                volume: volume.clone(),
                                available: *available,
                                floor: *floor,
                                reserved: *reserved,
                            }),
                            Decision::Queued { position } => Holding::Slot {
                                class: "heavy_local",
                                position: *position,
                            },
                            _ => unreachable!("granted, duplicate and refused return above"),
                        };
                        if let Err(stop) = queue.wait(holding, ask.cancel, self.sleeper.as_ref()) {
                            // Best effort: nothing started, and a request the
                            // coordinator still holds expires unclaimed.
                            let _ = gate.withdraw(&id);
                            return Err(stop);
                        }
                    }
                }
            }
            let mut lease = Admitted::leased(gate, id, environment.clone());
            if let Some(files) = &shared {
                // `lease` drops on an error and gives its tokens back.
                if let Some(held) = slots::take(
                    files, ask.weight, self, &mut queue, ask.cancel, gate, &lease.id,
                )? {
                    lease.slots = held;
                    lease
                        .env
                        .insert(HOST_SLOT_ENV.to_string(), "held".to_string());
                }
            }
            // The window between the grant and the use: space may have gone
            // since the coordinator looked. The lease is given back and the
            // command queues for the disk instead of running.
            match self.disk.short(estimate) {
                None => return Ok(lease),
                Some(short) => {
                    drop(lease);
                    queue.wait(Holding::Disk(short), ask.cancel, self.sleeper.as_ref())?;
                }
            }
        }
    }

    /// With no coordinator nothing reserves the disk, but a command still
    /// does not start below the floor: wait for it.
    fn wait_for_disk(&self, estimate: u64, cancel: Option<&AtomicBool>) -> Result<(), LeaseStop> {
        let mut queue = Queue::new(self);
        while let Some(short) = self.disk.short(estimate) {
            queue.wait(Holding::Disk(short), cancel, self.sleeper.as_ref())?;
        }
        Ok(())
    }

    /// Say once, on stderr and as an event, that the shared host slots are
    /// not in use.
    fn host_slots_off(&self, why: &str) {
        if self.tries.slots_off_said.swap(true, Ordering::SeqCst) {
            return;
        }
        let detail = format!("shared host slots are off for this run: {why}");
        eprintln!("relais: host_slots_off: {detail}");
        self.events.emit(Event::EnvironmentWarning {
            what: "host_slots_off".into(),
            detail,
        });
    }
}

/// The tries made so far at an unreachable coordinator: the time slept and
/// the next wait.
#[derive(Default)]
struct Unreachable {
    spent: Duration,
    next: Duration,
}

/// What holds a command: its place in a queue, or the disk floor.
enum Holding<'h> {
    Slot { class: &'h str, position: usize },
    Disk(Shortfall),
}

impl Holding<'_> {
    fn kind(&self) -> WaitKind {
        match self {
            Self::Slot { .. } => WaitKind::Admission,
            Self::Disk(_) => WaitKind::Disk,
        }
    }
}

/// Time spent held, announced every [`HeavyAdmission::announce_every`] and
/// charged to the budget of what holds the command: `admission_wait_seconds`
/// for the queue, `disk_wait_seconds` for the disk floor, and the total for
/// both.
struct Queue<'q> {
    admission: &'q HeavyAdmission<'q>,
    wait: Option<(WaitKind, crate::policy::Wait)>,
    started: i64,
    last_announced: i64,
}

impl<'q> Queue<'q> {
    fn new(admission: &'q HeavyAdmission<'q>) -> Self {
        let started = admission.budgets.now();
        Self {
            admission,
            wait: None,
            started,
            last_announced: started,
        }
    }

    /// The wait that charges `kind`, started now if another was running
    /// (dropping that one charges it up to now).
    fn charging(&mut self, kind: WaitKind) -> &mut crate::policy::Wait {
        if self.wait.as_ref().is_none_or(|(held, _)| *held != kind) {
            self.wait = None;
            let now = self.admission.budgets.now();
            self.started = now;
            self.last_announced = now;
            self.wait = Some((kind, self.admission.budgets.wait(kind)));
        }
        match &mut self.wait {
            Some((_, wait)) => wait,
            None => unreachable!("the wait was set above"),
        }
    }

    /// One poll's worth of waiting: charge it, stop if a budget ran out or
    /// the run was cancelled, announce when it is time, then sleep.
    fn wait(
        &mut self,
        holding: Holding<'_>,
        cancel: Option<&AtomicBool>,
        sleeper: &dyn Sleeper,
    ) -> Result<(), LeaseStop> {
        if let Some(exhausted) = self.charging(holding.kind()).tick() {
            let reason = match &holding {
                Holding::Slot { class, position } => {
                    format!("still queued for {class} at position {position}")
                }
                Holding::Disk(short) => format!(
                    "still waiting for {} to rise above the floor ({} available, floor {})",
                    short.volume,
                    crate::disk::gib(short.available),
                    crate::disk::gib(short.floor)
                ),
            };
            self.admission.events.emit(Event::Decision {
                what: "admission_queue_ended".into(),
                reason,
            });
            return Err(LeaseStop::Exhausted(exhausted));
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            return Err(LeaseStop::Cancelled);
        }
        let now = self.admission.budgets.now();
        let every = i64::try_from(self.admission.announce_every.as_millis()).unwrap_or(i64::MAX);
        if now - self.last_announced >= every {
            self.last_announced = now;
            let waited_s = u64::try_from((now - self.started).max(0) / 1000).unwrap_or(0);
            self.admission.events.emit(match holding {
                Holding::Slot { class, position } => Event::WaitingForSlot {
                    class: class.to_string(),
                    position,
                    waited_s,
                },
                Holding::Disk(short) => Event::WaitingForDisk {
                    volume: short.volume,
                    available: short.available,
                    floor: short.floor,
                    reserved: short.reserved,
                },
            });
        }
        sleeper.sleep(self.admission.poll);
        Ok(())
    }
}

/// A command that was admitted. Dropping it gives the tokens and the host
/// slots back.
pub struct Admitted<'a> {
    gate: Option<&'a (dyn Gate + Sync)>,
    id: String,
    /// The environment the command starts with.
    env: BTreeMap<String, String>,
    /// The host slot files held. Their descriptors are passed to the
    /// command (`procs::inherit_fds`), so a slot lasts as long as any
    /// descendant does.
    slots: Vec<std::fs::File>,
}

impl<'a> Admitted<'a> {
    fn unadmitted(env: BTreeMap<String, String>) -> Self {
        Self {
            gate: None,
            id: String::new(),
            env,
            slots: Vec::new(),
        }
    }

    fn leased(gate: &'a (dyn Gate + Sync), id: String, env: BTreeMap<String, String>) -> Self {
        Self {
            gate: Some(gate),
            id,
            env,
            slots: Vec::new(),
        }
    }

    /// The lease's dispatch id; empty when unadmitted.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The environment the command starts with.
    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    /// The raw descriptors of the host slots held.
    #[cfg(unix)]
    pub fn slot_fds(&self) -> Vec<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.slots.iter().map(AsRawFd::as_raw_fd).collect()
    }

    /// Tell the coordinator which process group runs the command, so the
    /// lease outlives this process for exactly as long as the group does.
    /// `true` when it is tied. A coordinator that cannot take it is
    /// reported, not hidden, and the lease is then kept alive by
    /// heartbeats for as long as the check runs ([`Self::keep_alive`]).
    pub fn bound_to_group(&self, pgid: u32, events: &Events) -> bool {
        let Some(gate) = self.gate else { return true };
        let problem = match gate.bind_group(&self.id, std::process::id(), pgid) {
            Ok(BindOutcome::Bound) => return true,
            Ok(other) => format!("{other:?}"),
            Err(e) => e.to_string(),
        };
        events.emit(Event::EnvironmentWarning {
            what: "heavy_lease_not_tied_to_group".into(),
            detail: format!(
                "lease {} was not tied to process group {pgid} ({problem}); it is kept alive by \
                 heartbeats while the check runs, its tokens return when the check ends, and \
                 they are not held for a descendant that outlives this run",
                self.id
            ),
        });
        false
    }

    /// Heartbeat the lease every `every` until `stop`: what keeps the
    /// coordinator from taking an untied lease for abandoned while its
    /// check runs. The first beat is at once.
    pub fn keep_alive(&self, stop: &AtomicBool, every: Duration) {
        let Some(gate) = self.gate else { return };
        const SLICE: Duration = Duration::from_millis(20);
        while !stop.load(Ordering::SeqCst) {
            // A beat that fails is tried again next time round; a
            // coordinator that stays unreachable is the check's end to see.
            let _ = gate.heartbeat(&self.id);
            let mut waited = Duration::ZERO;
            while waited < every && !stop.load(Ordering::SeqCst) {
                std::thread::sleep(SLICE);
                waited += SLICE;
            }
        }
    }
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        let Some(gate) = self.gate else { return };
        let given_back = gate
            .release(&self.id)
            .and_then(|_| gate.settle(&self.id, Some(0)));
        if let Err(e) = given_back {
            // Nowhere to return this from a drop; the coordinator frees a
            // lease whose runner is gone (SPEC §23).
            eprintln!("relais: heavy lease {} was not given back: {e}", self.id);
        }
    }
}

#[cfg(unix)]
mod slots {
    //! amont's host-slot protocol (ADR-0009): `slot-<i>` files in amont's
    //! private directory, locked with `flock(LOCK_EX | LOCK_NB)`. The
    //! directory and the files are amont's: nothing here creates them.

    use std::fs::File;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    use super::{Gate, HeavyAdmission, Holding, LeaseStop, Queue};

    /// What a try at the slots found.
    pub(super) enum Taken {
        All(Vec<File>),
        /// Not enough are free: this many more must be released.
        Short {
            missing: u32,
        },
    }

    /// Take `want` of the slot files, all or none: on a partial set every
    /// file taken is released and the attempt backs off. The files are
    /// opened, never created.
    pub(super) fn try_take(files: &[PathBuf], want: u32) -> std::io::Result<Taken> {
        let mut held = Vec::new();
        for path in files {
            if held.len() as u32 == want {
                break;
            }
            let file = File::open(path)?;
            if crate::procs::try_flock(&file)? {
                held.push(file);
            }
        }
        // Dropping `held` on a partial set unlocks it.
        let got = held.len() as u32;
        Ok(if got == want {
            Taken::All(held)
        } else {
            Taken::Short {
                missing: want - got,
            }
        })
    }

    /// Accept the directory only when it is a real directory this user
    /// owns with mode 0700: on a shared `/tmp`, another user could create
    /// it first and hold every slot. Nothing is created.
    fn private(dir: &Path) -> Result<(), String> {
        let meta = dir
            .symlink_metadata()
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(format!("{} is not a plain directory", dir.display()));
        }
        if meta.uid() != crate::procs::uid() {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(format!("{} has mode {mode:o}, not 700", dir.display()));
        }
        Ok(())
    }

    /// The existing `slot-*` files of the directory, in name order.
    pub(super) fn slot_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
        private(dir)?;
        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("slot-"))
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.path())
            .collect();
        files.sort();
        if files.is_empty() {
            return Err(format!("{} has no slot-* files", dir.display()));
        }
        Ok(files)
    }

    /// Count the slot files before anything is queued for. `Ok(None)`: the
    /// shared queue is not usable, which is said once and not fatal, the
    /// host queue being amont's courtesy and not relais's rule. A weight
    /// above the number of files is refused: capping it would let a
    /// command that asked for more than the host has run as if it had not.
    pub(super) fn inventory(
        dir: &Path,
        weight: u32,
        admission: &HeavyAdmission<'_>,
    ) -> Result<Option<Vec<PathBuf>>, LeaseStop> {
        match slot_files(dir) {
            Err(why) => {
                admission.host_slots_off(&why);
                Ok(None)
            }
            Ok(files) if weight as usize > files.len() => Err(LeaseStop::HostSlots {
                weight,
                slots: files.len(),
                dir: dir.display().to_string(),
            }),
            Ok(files) => Ok(Some(files)),
        }
    }

    /// Wait for `weight` of the slot files, charging the wait like the
    /// queue for tokens and reporting how many more must be released as the
    /// queue position. `Ok(None)`: the files became unusable, which is
    /// said once and not fatal.
    pub(super) fn take(
        files: &[PathBuf],
        weight: u32,
        admission: &HeavyAdmission<'_>,
        queue: &mut Queue<'_>,
        cancel: Option<&AtomicBool>,
        gate: &(dyn Gate + Sync),
        lease: &str,
    ) -> Result<Option<Vec<File>>, LeaseStop> {
        loop {
            let missing = match try_take(files, weight) {
                Ok(Taken::All(held)) => return Ok(Some(held)),
                Ok(Taken::Short { missing }) => missing,
                Err(e) => {
                    admission.host_slots_off(&format!("a slot file could not be locked: {e}"));
                    return Ok(None);
                }
            };
            // The tokens are held while the slots are awaited: say so, so
            // the coordinator does not take the lease for abandoned.
            let _ = gate.heartbeat(lease);
            queue.wait(
                Holding::Slot {
                    class: "host_slot",
                    position: missing as usize,
                },
                cancel,
                admission.sleeper.as_ref(),
            )?;
        }
    }
}

#[cfg(not(unix))]
mod slots {
    //! `flock` is unix-only: on other platforms the coordinator's token
    //! queue is the whole queue.

    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicBool;

    use super::{Gate, HeavyAdmission, LeaseStop, Queue};

    pub(super) fn inventory(
        _dir: &Path,
        _weight: u32,
        admission: &HeavyAdmission<'_>,
    ) -> Result<Option<Vec<PathBuf>>, LeaseStop> {
        admission.host_slots_off("host slots need flock, which this platform does not have");
        Ok(None)
    }

    pub(super) fn take(
        _files: &[PathBuf],
        _weight: u32,
        _admission: &HeavyAdmission<'_>,
        _queue: &mut Queue<'_>,
        _cancel: Option<&AtomicBool>,
        _gate: &(dyn Gate + Sync),
        _lease: &str,
    ) -> Result<Option<Vec<File>>, LeaseStop> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
