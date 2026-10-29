//! The heavy-command lease (SPEC §23, ADR-0010): every setup step and every
//! check, at the base and at the candidate, takes `weight` tokens of the
//! machine-wide `HeavyLocal` class from the coordinator before it starts,
//! holds them while its process group runs, and gives them back after.
//!
//! Admission has four parts, in this order:
//!
//! 1. the coordinator's tokens (`weight` of them, queued behind whoever
//!    holds them);
//! 2. `weight` host slots, all or none, through amont's `amont-slots` crate
//!    (amont ADR-0014, decisions ADR-0049), so every heavy job on the
//!    machine shares one queue: amont's gates, relais's checks, anything run
//!    through `amont slot run`. The directory is amont's by default
//!    (`[resources] shared_host_slots`, `"off"` for the coordinator's queue
//!    alone); a directory the crate refuses is said once as
//!    `host_slots_off`. The runner holds the slots, never the check: their
//!    descriptors are close-on-exec, so a daemon a check leaves behind does
//!    not keep one. Where an ancestor already holds slots
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
pub const HOST_SLOT_ENV: &str = amont_slots::HELD_ENV;

/// How often a lease the coordinator could not tie to a process group says
/// it is alive while its check runs (the coordinator frees a lease it has
/// not heard from for `LEASE_GRACE`, 300 s).
pub const LEASE_HEARTBEAT_EVERY: Duration = Duration::from_secs(30);

/// How long an unreachable coordinator is retried before the run stops.
/// Two minutes: it covers a reply slower than the 5 s request timeout and a
/// coordinator restart or re-election, with margin.
pub const UNREACHABLE_BOUND: Duration = Duration::from_secs(120);

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

/// Randomness, made injectable: the wait between tries of an unreachable
/// coordinator is drawn through this, so a test fixes the draw.
pub trait Jitter: Send + Sync {
    /// A duration in `[0, max]`.
    fn within(&self, max: Duration) -> Duration;
}

/// The real thing: a splitmix64 stream seeded once per process from values
/// the boundary hands in (run id, pid, time), so runs that start together
/// do not retry together.
pub struct SeededJitter {
    state: AtomicU64,
}

impl SeededJitter {
    /// A stream from the run id, the process id and a time in nanoseconds.
    pub fn new(run_id: &str, pid: u32, nanos: u128) -> Self {
        let mut seed = nanos as u64 ^ ((nanos >> 64) as u64) ^ (u64::from(pid) << 32);
        for byte in run_id.bytes() {
            seed = (seed ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self {
            state: AtomicU64::new(seed),
        }
    }

    /// The stream of this process: reads the pid and the clock, at the
    /// boundary where the admission is built.
    pub fn for_process(run_id: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        Self::new(run_id, std::process::id(), nanos)
    }

    fn next(&self) -> u64 {
        let step = self
            .state
            .fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed)
            .wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = step;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl Jitter for SeededJitter {
    fn within(&self, max: Duration) -> Duration {
        let millis = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
        match millis.checked_add(1) {
            Some(span) => Duration::from_millis(self.next() % span),
            None => Duration::from_millis(self.next()),
        }
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
    /// How many host slots there are (`[resources] host_slots`, else the
    /// automatic sizing).
    pub sizing: amont_slots::Sizing,
    /// An ancestor holds host slots for this tree: nothing is queued for
    /// here, and the fact is passed on to the check.
    pub ancestor_holds_slots: bool,
    pub poll: Duration,
    pub announce_every: Duration,
    /// How often a lease that is not tied to a group says it is alive.
    pub heartbeat_every: Duration,
    /// How a queued command waits.
    pub sleeper: Arc<dyn Sleeper>,
    /// Where the jitter of the retries of an unreachable coordinator comes
    /// from.
    pub jitter: Arc<dyn Jitter>,
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
            sizing: amont_slots::Sizing::Auto,
            ancestor_holds_slots: false,
            poll: ADMISSION_POLL,
            announce_every: ANNOUNCE_EVERY,
            heartbeat_every: LEASE_HEARTBEAT_EVERY,
            sleeper: Arc::new(ThreadSleeper),
            // No gate is asked here, so nothing is drawn: a fixed stream,
            // with no clock or pid read for it.
            jitter: Arc::new(SeededJitter::new("", 0, 0)),
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
    /// full-jitter backoff (a random wait in `[0, backoff]`, the backoff
    /// 1 s doubling to 30 s) for [`UNREACHABLE_BOUND`] of wall time,
    /// emitting a `waiting_for_slot` of class
    /// [`COORDINATOR_UNREACHABLE_CLASS`] on each retry. Once, when it has
    /// forgotten the run (a restart empties its registry), register the run
    /// again and ask again; an unreachable coordinator on that path is
    /// retried the same way. A second `UnknownRun` is the coordinator's
    /// answer.
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
                    if let Some(registration) = &self.registration {
                        match gate.register_run(registration) {
                            Ok(()) => *registered_again = true,
                            Err(e) if e.unavailable() => {
                                self.retry_unreachable(e, &mut retrying, cancel)?
                            }
                            Err(e) => return Err(LeaseStop::Gate(e)),
                        }
                    }
                }
                // The coordinator granted this id but its answer was lost
                // (the read timed out): asked again, it says the id is
                // already admitted. The lease is this request's own.
                Ok(Decision::AlreadyAdmitted) if retrying.first.is_some() => {
                    return Ok(Decision::Granted)
                }
                Ok(decision) => return Ok(decision),
                Err(e) if e.unavailable() => self.retry_unreachable(e, &mut retrying, cancel)?,
                Err(e) => return Err(LeaseStop::Gate(e)),
            }
        }
    }

    /// One backoff after an unreachable coordinator: stop when cancelled or
    /// when the bound (wall time since the first failure) is spent, naming
    /// how long it retried, else say the run is alive and sleep.
    fn retry_unreachable(
        &self,
        error: GateError,
        state: &mut Unreachable,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), LeaseStop> {
        let cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::SeqCst));
        if cancelled() {
            return Err(LeaseStop::Cancelled);
        }
        let now = self.budgets.now();
        let first = *state.first.get_or_insert(now);
        let spent = Duration::from_millis(u64::try_from((now - first).max(0)).unwrap_or(0));
        if spent >= UNREACHABLE_BOUND {
            return Err(LeaseStop::Gate(match error {
                GateError::Unavailable {
                    operation,
                    socket,
                    cause,
                } => GateError::Unavailable {
                    operation,
                    socket,
                    cause: format!("{cause}; retried for {} s", spent.as_secs()).into(),
                },
                other => other,
            }));
        }
        self.events.emit(Event::WaitingForSlot {
            class: COORDINATOR_UNREACHABLE_CLASS.to_string(),
            position: 0,
            waited_s: spent.as_secs(),
            behind: None,
        });
        let backoff = state.next.max(UNREACHABLE_BACKOFF_START);
        state.next = (backoff * 2).min(UNREACHABLE_BACKOFF_MAX);
        let pause = self
            .jitter
            .within(backoff.min(UNREACHABLE_BACKOFF_MAX))
            .min(UNREACHABLE_BOUND - spent);
        let slice = self.poll.max(Duration::from_millis(1));
        let mut left = pause;
        while !left.is_zero() {
            let step = left.min(slice);
            self.sleeper.sleep(step);
            left -= step;
            if cancelled() {
                return Err(LeaseStop::Cancelled);
            }
        }
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
            (Some(dir), false) => slots::usable(dir, ask.weight, self)?,
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
                let answer = self.ask(gate, &request, &mut registered_again, ask.cancel);
                if matches!(answer, Err(LeaseStop::Cancelled)) {
                    // Best effort, as in the queue: nothing started.
                    let _ = gate.withdraw(&id);
                }
                match answer? {
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
                                behind: None,
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
            if let Some(dir) = &shared {
                // `lease` drops on an error and gives its tokens back.
                if let Some(held) = slots::take(dir, ask, self, &mut queue, gate, &lease.id)? {
                    lease.slots = Some(held);
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

/// The tries made so far at an unreachable coordinator: the clock's reading
/// at the first failure and the next wait.
#[derive(Default)]
struct Unreachable {
    first: Option<i64>,
    next: Duration,
}

/// What holds a command: its place in a queue, or the disk floor.
enum Holding<'h> {
    /// `behind`: who holds the host slots, when they said so.
    Slot {
        class: &'h str,
        position: usize,
        behind: Option<String>,
    },
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
                Holding::Slot {
                    class, position, ..
                } => format!("still queued for {class} at position {position}"),
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
                Holding::Slot {
                    class,
                    position,
                    behind,
                } => Event::WaitingForSlot {
                    class: class.to_string(),
                    position,
                    waited_s,
                    behind,
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
    /// The host slots held, by this runner and never by the check: released
    /// when the lease drops, after the check ended.
    slots: Option<amont_slots::Lease>,
}

impl<'a> Admitted<'a> {
    fn unadmitted(env: BTreeMap<String, String>) -> Self {
        Self {
            gate: None,
            id: String::new(),
            env,
            slots: None,
        }
    }

    fn leased(gate: &'a (dyn Gate + Sync), id: String, env: BTreeMap<String, String>) -> Self {
        Self {
            gate: Some(gate),
            id,
            env,
            slots: None,
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

    /// How many host slots this lease holds.
    pub fn host_slots(&self) -> usize {
        self.slots.as_ref().map_or(0, amont_slots::Lease::weight)
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

mod slots {
    //! The host slots, through amont's `amont-slots` crate (amont ADR-0014):
    //! a waiter keeps its place across polls, and relais drives the polling
    //! from its own queue, so the coordinator lease keeps its heartbeat and
    //! the wait is charged like any other.

    use std::path::{Path, PathBuf};

    use super::{Ask, Gate, HeavyAdmission, Holding, LeaseStop, Queue};

    /// Whether the slot directory can be used, before anything is queued
    /// for. `Ok(None)`: it cannot, which is said once and not fatal, the
    /// host queue being a courtesy to the machine. A weight above the cap
    /// is refused: capping it would let a command that asked for more than
    /// the host has run as if it had not.
    pub(super) fn usable(
        dir: &Path,
        weight: u32,
        admission: &HeavyAdmission<'_>,
    ) -> Result<Option<PathBuf>, LeaseStop> {
        if !amont_slots::SUPPORTED {
            admission.host_slots_off("host slots need flock, which this platform does not have");
            return Ok(None);
        }
        if let Err(why) = amont_slots::safe_dir(dir) {
            admission.host_slots_off(&why);
            return Ok(None);
        }
        let cap = admission.sizing.cap();
        if weight > cap {
            return Err(LeaseStop::HostSlots {
                weight,
                slots: cap as usize,
                dir: dir.display().to_string(),
            });
        }
        Ok(Some(dir.to_path_buf()))
    }

    /// Wait for `ask.weight` host slots. `Ok(None)`: the slots became
    /// unusable, which is said once and not fatal.
    pub(super) fn take(
        dir: &Path,
        ask: &Ask<'_>,
        admission: &HeavyAdmission<'_>,
        queue: &mut Queue<'_>,
        gate: &(dyn Gate + Sync),
        lease: &str,
    ) -> Result<Option<amont_slots::Lease>, LeaseStop> {
        let mut holder = amont_slots::Holder::here("relais", ask.label);
        holder.session = admission.run_id.clone();
        let started = std::time::Instant::now();
        let mut waiter =
            match amont_slots::Waiter::new_in(dir, admission.sizing, ask.weight, holder) {
                Ok(waiter) => waiter,
                Err(e) => {
                    admission.host_slots_off(&format!("a host slot could not be queued for: {e}"));
                    return Ok(None);
                }
            };
        loop {
            let queued = match waiter.poll() {
                Ok(amont_slots::Poll::Granted(held)) => {
                    amont_slots::record_wait(
                        dir,
                        "relais",
                        ask.weight,
                        started.elapsed().as_millis(),
                        "admitted",
                    );
                    return Ok(Some(held));
                }
                Ok(amont_slots::Poll::Waiting(queued)) => queued,
                Err(e) => {
                    admission.host_slots_off(&format!("a host slot could not be locked: {e}"));
                    return Ok(None);
                }
            };
            // The tokens are held while the slots are awaited: say so, so
            // the coordinator does not take the lease for abandoned.
            let _ = gate.heartbeat(lease); // best-effort: the next poll beats again
            let waited = queue.wait(
                Holding::Slot {
                    class: "host_slot",
                    position: queued.position,
                    behind: behind(&queued),
                },
                ask.cancel,
                admission.sleeper.as_ref(),
            );
            if let Err(stop) = waited {
                // Ended by the budget or a cancel: a sample too, or the
                // day-14 wait percentiles would see only the admissions.
                amont_slots::record_wait(
                    dir,
                    "relais",
                    ask.weight,
                    started.elapsed().as_millis(),
                    "deferred",
                );
                return Err(stop);
            }
        }
    }

    /// `relais: cargo test (amont-wt-x, 4m), 1 unknown (old client)`, or
    /// nothing when no holder said who it is.
    fn behind(queued: &amont_slots::Queue) -> Option<String> {
        let now = amont_slots::now_ms();
        let mut who: Vec<String> = queued.holders.iter().map(|h| h.describe(now)).collect();
        if queued.unknown > 0 {
            who.push(format!("{} unknown (old client)", queued.unknown));
        }
        (!who.is_empty()).then(|| who.join(", "))
    }
}

#[cfg(test)]
mod tests;
