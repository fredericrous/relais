//! The heavy-command lease (SPEC §23, ADR-0010): every setup step and every
//! check, at the base and at the candidate, takes `weight` tokens of the
//! machine-wide `HeavyLocal` class from the coordinator before it starts,
//! holds them while its process group runs, and gives them back after.
//!
//! Admission has four parts, in this order:
//!
//! 1. the coordinator's tokens (`weight` of them, queued behind whoever
//!    holds them);
//! 2. `weight` host slots, all or none, through greengate's `greenharness-slots` crate
//!    (amont ADR-0014, decisions ADR-0049), so every heavy job on the
//!    machine shares one queue: greengate's gates, relais's checks, anything run
//!    through `amont slot run`. The directory is greengate's by default
//!    (`[resources] shared_host_slots`, `"off"` for the coordinator's queue
//!    alone); a directory the crate refuses is said once as
//!    `host_slots_off`. The runner holds the slots, never the check: their
//!    descriptors are close-on-exec, so a daemon a check leaves behind does
//!    not keep one. Where an ancestor already holds slots (relais's own
//!    `GREENHARNESS_HOST_SLOT` is a marker the slots crate's reader accepts:
//!    a live `held:<lease>`, or the bare `held` during the rollout) nothing
//!    is queued for, and the checks get that marker unchanged; under its
//!    own slots a check gets `held:<lease>`, its lease's id (amont
//!    host-slot-admission t15, phase 2);
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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::admission::{
    BindOutcome, Decision, DispatchRequest, DispatchSource, Gate, GateError, Refusal,
    ResourceClass, RunRegistration, ADMISSION_POLL,
};
use crate::disk::{DiskCheck, Shortfall};
use crate::policy::{Bound, Budgets, Exhausted, WaitKind};
use crate::procs::Ended;
use crate::protocol::{Event, Events};

/// How often a queued command says it is still queued.
pub const ANNOUNCE_EVERY: Duration = Duration::from_secs(30);

/// The environment variable a held host slot exports, so an amont started
/// inside the check does not queue again (amont ADR-0009).
pub const HOST_SLOT_ENV: &str = greenharness_slots::HELD_ENV;

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

/// relais's own held marker, judged once at the composition root (the CLI
/// reads it before `execute`) by the slots crate's one reader, with the log
/// that says a refusal once: every admission of one run, and of the child
/// runs it hosts, gets the same answer (amont host-slot-admission t15).
pub struct HostMarker {
    marker: greenharness_slots::Marker,
    /// The `GREENHARNESS_HOST_SLOT` text an accepted marker was judged from,
    /// passed to the checks unchanged; `None` unless the marker is held.
    inherited: Option<String>,
    log: greenharness_slots::MarkerLog,
}

/// What [`HostMarker::judge`] found for one admission.
#[derive(Debug, PartialEq, Eq)]
pub struct Judged {
    /// The marker an ancestor holds the slots under, for the checks; `None`
    /// when no ancestor holds them, and this run queues.
    pub ancestor_marker: Option<String>,
    /// The refusal line to say, the first time only.
    pub said: Option<String>,
}

impl HostMarker {
    /// This process's `GREENHARNESS_HOST_SLOT`, read once and judged by the
    /// slots crate's one reader against its own directory.
    pub fn read() -> HostMarker {
        let given = std::env::var_os(HOST_SLOT_ENV);
        HostMarker::judged(&greenharness_slots::dir(), given.as_deref())
    }

    /// `given` judged against the slot directory `dir`, with a fresh log:
    /// the one place the text and its verdict are paired.
    fn judged(dir: &Path, given: Option<&std::ffi::OsStr>) -> HostMarker {
        let marker = greenharness_slots::held_marker_in(dir, given);
        let inherited = marker
            .is_held()
            .then(|| given.map(|g| g.to_string_lossy().into_owned()))
            .flatten();
        HostMarker {
            marker,
            inherited,
            log: greenharness_slots::MarkerLog::new(),
        }
    }

    /// No marker: nothing held above this run.
    pub fn absent() -> HostMarker {
        HostMarker::judged(Path::new(""), None)
    }

    /// [`ancestor_holds_slots`] for this marker and its log, with the
    /// inherited marker the checks get unchanged (t15 phase 2).
    pub fn judge(&self, queues: Option<&Path>) -> Judged {
        let (held, said) = ancestor_holds_slots(&self.marker, queues, &self.log);
        debug_assert!(
            !held || self.inherited.is_some(),
            "a held marker keeps its text"
        );
        Judged {
            ancestor_marker: held.then(|| self.inherited.clone()).flatten(),
            said,
        }
    }
}

/// Whether an ancestor already holds host slots for this process tree, and
/// the line to say when it does not: `marker` is relais's own
/// `GREENHARNESS_HOST_SLOT` as the slots crate's one reader judged it (amont
/// ADR-0022, amended for t15; the bare `held` is accepted during the
/// rollout), and `log` the run's ([`HostMarker`]); the runner prints the
/// line. `queues` is the slot directory this run would
/// queue on, when the host queue is on there: a refused marker is then
/// journalled there once per log, and its line returned once. A held marker
/// never returns a line: the crate has none for an accepted marker.
// holds-until: no machine sets `shared_host_slots`. The row goes to the
// run's queue directory while the lease is judged against the slots crate's
// own directory; the two differ only under that setting, and the phase-3
// count reads the default one (amont plan, phase 3 gate).
pub fn ancestor_holds_slots(
    marker: &greenharness_slots::Marker,
    queues: Option<&Path>,
    log: &greenharness_slots::MarkerLog,
) -> (bool, Option<String>) {
    // `record` appends the journal row: an action, so a match, not a filter.
    let said = match queues {
        Some(dir) if log.record(dir, "relais", marker) => {
            greenharness_slots::refused_line("relais", marker)
        }
        _ => None,
    };
    (marker.is_held(), said)
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

/// The logical cores this machine has, never zero. Only the coordinator's
/// own cap ([`default_heavy_commands`]) reads this (relais ADR-0010: a
/// quarter of them); a command's width counts the cores the host slots
/// count ([`HostPlan::machine`]).
pub fn logical_cores() -> u32 {
    greenharness_slots::logical_cores()
}

/// What relais sizes the host queue from: greengate's two host keys,
/// resolved once by the slots crate for every client, and relais's own
/// `[resources] host_slots`, which is deprecated but still wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPlan {
    /// Whether the host slots are on, and how they are sized, or which
    /// setting turned them off.
    pub slots: HostSlots,
    /// The cores the slots count: `greengate.hostCores`, else the physical
    /// cores.
    pub machine: greenharness_slots::Machine,
    /// What the resolution found wrong, each with its fix, for the caller
    /// to say once.
    pub warnings: Vec<String>,
}

/// The host slots of a plan: on with their sizing, or off with the setting
/// that turned them off, as a person would undo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSlots {
    On(greenharness_slots::Sizing),
    Off(HostSlotsOff),
}

/// The setting that turned the host slots off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSlotsOff {
    /// `GREENGATE_HOST_SLOTS=0`.
    Env,
    /// `greengate.hostSlots 0` in `--global` git config.
    GitGlobal,
    /// `greengate.hostSlots 0` in `--system` git config.
    GitSystem,
    /// `[resources] host_slots = 0` in machine.toml.
    MachineToml,
}

impl HostSlotsOff {
    /// The setting, as a person would read it: `GREENGATE_HOST_SLOTS=0`.
    pub fn setting(self) -> String {
        use greenharness_slots::host::{SLOTS_ENV, SLOTS_KEY};
        match self {
            HostSlotsOff::Env => format!("{SLOTS_ENV}=0"),
            HostSlotsOff::GitGlobal => format!("{SLOTS_KEY} 0 (git config --global)"),
            HostSlotsOff::GitSystem => format!("{SLOTS_KEY} 0 (git config --system)"),
            HostSlotsOff::MachineToml => "[resources] host_slots = 0".to_string(),
        }
    }

    /// How to turn the host slots back on.
    pub fn fix(self) -> String {
        use greenharness_slots::host::{SLOTS_ENV, SLOTS_KEY};
        match self {
            HostSlotsOff::Env => format!("unset {SLOTS_ENV}"),
            HostSlotsOff::GitGlobal => format!("git config --global --unset {SLOTS_KEY}"),
            HostSlotsOff::GitSystem => format!("git config --system --unset {SLOTS_KEY}"),
            HostSlotsOff::MachineToml => "remove host_slots from machine.toml".to_string(),
        }
    }
}

impl HostSlots {
    /// The sizing when the host slots are on.
    pub fn sizing(&self) -> Option<greenharness_slots::Sizing> {
        match self {
            HostSlots::On(sizing) => Some(*sizing),
            HostSlots::Off(_) => None,
        }
    }
}

/// The plan for `host` and relais's own `override_slots`. Pure: the caller
/// resolves the host with [`greenharness_slots::host::from_host`].
/// Precedence: `GREENGATE_HOST_SLOTS` (an environment variable outranks a
/// file), then the deprecated `override_slots` from machine.toml, then
/// `greengate.hostSlots` in git config, then the automatic sizing. An
/// `override_slots` of 0 turns the host slots off, as `greengate.hostSlots
/// 0` does.
pub fn host_plan(
    host: &greenharness_slots::host::HostSizing,
    override_slots: Option<u32>,
) -> HostPlan {
    use greenharness_slots::host::Source;
    let slots = match (host.slots.source, override_slots, host.slots.value) {
        (Source::Env, _, Some(sizing)) | (_, None, Some(sizing)) => HostSlots::On(sizing),
        (Source::Env, _, None) => HostSlots::Off(HostSlotsOff::Env),
        (_, Some(0), _) => HostSlots::Off(HostSlotsOff::MachineToml),
        (_, Some(n), _) => HostSlots::On(greenharness_slots::Sizing::Fixed(n)),
        (Source::System, None, None) => HostSlots::Off(HostSlotsOff::GitSystem),
        // `None` comes only from a `0` the crate read: global, or detected
        // never (it sizes automatically).
        (_, None, None) => HostSlots::Off(HostSlotsOff::GitGlobal),
    };
    HostPlan {
        slots,
        machine: greenharness_slots::Machine {
            cores: host.cores.value.max(1),
        },
        warnings: host.warnings.clone(),
    }
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

/// How many jobs a command of `weight` tokens may start,
/// `max(1, weight * cores / cap)`: greengate's own formula, from the crate
/// both tools link, so a weight means the same width whoever holds it.
pub use greenharness_slots::parallelism;

/// Where a command runs, for its width: under a host slot (its own or an
/// ancestor's), or queued only on the coordinator's tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    UnderHostSlot,
    CoordinatorOnly,
}

impl Placement {
    /// [`Placement::UnderHostSlot`] when `held`, else the coordinator's.
    pub fn from_held(held: bool) -> Self {
        if held {
            Placement::UnderHostSlot
        } else {
            Placement::CoordinatorOnly
        }
    }
}

/// The cap a command's width divides the cores by, and the placement it
/// was counted for: the host slots' cap for a command under a host slot
/// when the slots are on, else the coordinator's `cap`. Pure: the one rule
/// the admission and `relais doctor` both apply.
pub fn host_share(
    machine: greenharness_slots::Machine,
    sizing: Option<greenharness_slots::Sizing>,
    placement: Placement,
    coordinator_cap: u32,
) -> (u32, Placement) {
    match (sizing, placement) {
        (Some(sizing), Placement::UnderHostSlot) => {
            (sizing.cap_for(machine.cores), Placement::UnderHostSlot)
        }
        (Some(_) | None, Placement::UnderHostSlot | Placement::CoordinatorOnly) => {
            (coordinator_cap, Placement::CoordinatorOnly)
        }
    }
}

/// The environment of a command admitted with `jobs` jobs: the bounds
/// relais sets, with any key the check names itself replaced by the
/// check's value.
///
/// The keys are greengate's own list ([`greenharness_slots::WIDTH_KEYS`]),
/// so a key greengate learns to bound is bounded here on the next bump.
/// Every key is set whatever relais's own environment holds: the check's
/// own `env` is the only override.
pub fn child_environment(jobs: u32, own: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut environment: BTreeMap<String, String> = greenharness_slots::width_env(jobs, |_| None)
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
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
    /// The host-slot line ended the wait without a spent budget (ADR-0015).
    SlotLine(SlotLineEnd),
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

/// Why a host-slot wait ended with no slot and no spent budget (ADR-0015).
#[derive(Debug)]
pub enum SlotLineEnd {
    /// No admission and no change of place for the window.
    Stalled { window_s: u64, position: usize },
    /// The queue was joined, then its lock failed `failures` polls in a row.
    Unreadable { failures: u32, cause: String },
    /// The line's hard ceiling ran out after `waited_s`: never under relais's
    /// `Duration::MAX`, kept so the crate's every end has a name here.
    Ceiling { waited_s: u64 },
}

impl std::fmt::Display for SlotLineEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stalled { window_s, position } => write!(
                f,
                "the host slot line made no progress for {window_s} s at position {position}"
            ),
            Self::Unreadable { failures, cause } => write!(
                f,
                "the host slot line could not be read {failures} times in a row: {cause}"
            ),
            Self::Ceiling { waited_s } => {
                write!(
                    f,
                    "the host slot line reached its ceiling after {waited_s} s"
                )
            }
        }
    }
}

impl std::fmt::Display for LeaseStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted(which) => write!(f, "a budget ran out while queued ({which:?})"),
            Self::SlotLine(end) => write!(f, "{end}"),
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
    sizing_said: Arc<AtomicBool>,
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
    /// `max_heavy_commands` in force: the coordinator's cap, read from the
    /// logical cores.
    pub cap: u32,
    /// The cores the host slots count, so a command's width reads the same
    /// machine greengate does.
    pub machine: greenharness_slots::Machine,
    pub slots: Option<PathBuf>,
    /// How many host slots there are, from the run's plan; `None` when the
    /// host slots are off, so no width is ever read from a placeholder.
    pub sizing: Option<greenharness_slots::Sizing>,
    /// An ancestor holds host slots for this tree: its accepted marker, the
    /// inherited `GREENHARNESS_HOST_SLOT`. Nothing is queued for here, and the
    /// marker passes on to the check unchanged (amont host-slot-admission t15).
    pub ancestor_marker: Option<String>,
    pub poll: Duration,
    /// A host-slot line that made no progress for this long ends the wait
    /// (ADR-0015): read from `GREENHARNESS_SLOT_STALL_S` at the edge.
    pub stall_window: Duration,
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
    /// The host slot a setup keeps across its steps; closed outside one.
    pub setup_slot: SetupSlot,
}

/// The host slot a setup keeps across its steps (amont host-slot-admission
/// t07): the first step that queues for host slots takes them, each later
/// step relabels the same lease, and the setup's end gives it back. Without
/// it each step joined the tail of the line again and starved under load.
/// For the sequential steps of one setup: the admission that opens it is
/// not shared with commands running beside them.
#[derive(Debug, Default)]
pub struct SetupSlot(Mutex<Kept>);

/// Whether a setup is open, and what it keeps: one state under one lock,
/// so a lease is never kept by a closed setup.
#[derive(Debug, Default)]
enum Kept {
    #[default]
    Closed,
    Open {
        lease: Option<greenharness_slots::Lease>,
        /// A failed relabel was said; said once a setup.
        rename_said: bool,
    },
}

impl SetupSlot {
    fn state(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// An open [`SetupSlot`]; dropping it, on every way out of the setup,
/// gives the kept slot back, which writes its one `released` row.
///
/// deliberate: relais installs no SIGINT/SIGTERM handler, so a killed runner
/// never drops this guard: the kernel frees the flock, but no `released` row
/// is written and the holder record keeps the last step's label, for the
/// whole setup. holds-until: relais handles a terminating signal by
/// unwinding its run.
#[must_use = "dropping the guard gives the kept slot back at once"]
pub struct KeptSlot<'k> {
    slot: &'k SetupSlot,
}

impl Drop for KeptSlot<'_> {
    fn drop(&mut self) {
        let kept = std::mem::take(&mut *self.slot.state());
        drop(kept);
    }
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
            machine: greenharness_slots::Machine { cores: 1 },
            slots: None,
            sizing: None,
            ancestor_marker: None,
            poll: ADMISSION_POLL,
            stall_window: greenharness_slots::STALL,
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
            setup_slot: SetupSlot::default(),
        }
    }

    /// Keep the host slot the next command takes until the guard drops:
    /// the commands admitted meanwhile are one setup's steps, run one after
    /// another (amont host-slot-admission t07). Setups do not nest.
    #[must_use = "dropping the guard gives the kept slot back at once"]
    pub fn keep_slot(&self) -> KeptSlot<'_> {
        let mut state = self.setup_slot.state();
        debug_assert!(matches!(*state, Kept::Closed), "setups do not nest");
        *state = Kept::Open {
            lease: None,
            rename_said: false,
        };
        KeptSlot {
            slot: &self.setup_slot,
        }
    }

    /// Give the kept slot back before the setup ends, and say why. Nothing
    /// happens when none is kept.
    fn give_back_kept(&self, why: &str) {
        let kept = match &mut *self.setup_slot.state() {
            Kept::Open { lease, .. } => lease.take(),
            Kept::Closed => None,
        };
        let Some(kept) = kept else {
            return;
        };
        drop(kept);
        self.events.emit(Event::Decision {
            what: "kept_slot_given_back".into(),
            reason: format!("the host slot kept for this setup was given back: {why}"),
        });
    }

    /// The kept slot, relabelled for `ask`, when one is kept and fits its
    /// weight: the weight it holds and its marker. A slot of another weight is
    /// given back, and the step takes its own.
    fn reuse_kept(&self, ask: &Ask<'_>) -> Option<(usize, String)> {
        let asked = usize::try_from(ask.weight).unwrap_or(usize::MAX);
        // Under the lock: read, relabel, or take out the lease of another
        // weight. Everything said is said after the lock is let go.
        let (weight, kept_marker, renamed, other) = {
            let mut state = self.setup_slot.state();
            let Kept::Open {
                lease: Some(lease),
                rename_said,
            } = &mut *state
            else {
                return None;
            };
            let weight = lease.weight();
            if weight == asked {
                let renamed = match lease.rename(&slots::holder(self, ask.label)) {
                    Err(e) if !*rename_said => {
                        *rename_said = true;
                        Err(e)
                    }
                    _ => Ok(()),
                };
                (weight, Some(lease.marker()), renamed, None)
            } else {
                let Kept::Open { lease, .. } = &mut *state else {
                    unreachable!("matched as open above");
                };
                (weight, None, Ok(()), lease.take())
            }
        };
        if let Some(other) = other {
            drop(other);
            self.events.emit(Event::Decision {
                what: "kept_slot_given_back".into(),
                reason: format!(
                    "the host slot kept for this setup was given back: the next step asks \
                     a weight of {}, not {weight}",
                    ask.weight
                ),
            });
            return None;
        }
        if let Err(e) = renamed {
            // The record keeps the previous step's name; said once a setup.
            let detail = format!("the kept host slot still names the previous step: {e}");
            eprintln!("relais: host slot rename: {detail}");
            self.events.emit(Event::EnvironmentWarning {
                what: "host_slot_rename_failed".into(),
                detail,
            });
        }
        self.events.emit(Event::Decision {
            what: "host_slot_kept".into(),
            reason: format!(
                "`{}` runs under the host slot this setup already holds",
                ask.label
            ),
        });
        kept_marker.map(|marker| (weight, marker))
    }

    /// Keep `held` for the setup when one is open: the weight it holds.
    /// Otherwise it is handed back, for the command's own lease.
    fn keep(&self, held: greenharness_slots::Lease) -> Result<usize, greenharness_slots::Lease> {
        match &mut *self.setup_slot.state() {
            Kept::Open { lease, .. } => {
                // `reuse_kept` ran first and emptied it on every path to a take.
                debug_assert!(lease.is_none(), "a kept slot is never replaced");
                let weight = held.weight();
                *lease = Some(held);
                Ok(weight)
            }
            Kept::Closed => Err(held),
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

    /// How many jobs a command of `weight` tokens may start: under a host
    /// slot (its own or an ancestor's) the weight's share of the slots'
    /// cores, as a greengate holder of the same weight gets; otherwise the
    /// coordinator's cap shares the same cores ([`host_share`]).
    pub(crate) fn width(&self, weight: u32, placement: Placement) -> u32 {
        let (shares, _) = host_share(self.machine, self.sizing, placement, self.cap);
        parallelism(weight, self.machine.cores, shares)
    }

    /// The environment of a command of `weight` tokens: the parallelism
    /// bounds for whether it runs under a host slot, the host-slot fact
    /// when an ancestor holds the slots, and then the check's own `env`,
    /// which wins.
    fn environment(
        &self,
        weight: u32,
        own: &BTreeMap<String, String>,
        placement: Placement,
    ) -> BTreeMap<String, String> {
        if !self.manages() {
            return own.clone();
        }
        let mut environment = child_environment(self.width(weight, placement), &BTreeMap::new());
        if let Some(marker) = &self.ancestor_marker {
            environment.insert(HOST_SLOT_ENV.to_string(), marker.clone());
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
                    // Never wait on the coordinator while holding a slot:
                    // its holders wait for slots while holding its tokens.
                    self.give_back_kept("the coordinator had forgotten the run");
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
                Err(e) if e.unavailable() => {
                    self.give_back_kept("the coordinator could not be reached");
                    self.retry_unreachable(e, &mut retrying, cancel)?
                }
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
        // Before a host slot is held, the width is the coordinator's share
        // (or the slots' when an ancestor holds them): a command that ends
        // up queued nowhere must not run narrower than the coordinator gives.
        let environment = self.environment(
            ask.weight,
            ask.env,
            Placement::from_held(self.ancestor_marker.is_some()),
        );
        let Some(gate) = self.gate else {
            if self.manages() {
                self.wait_for_disk(self.disk.estimate(ask.label), ask.cancel)?;
            }
            return Ok(Admitted::unadmitted(environment));
        };
        // The shared queue is greengate's: its files are counted before any
        // token is queued for, so a weight it can never grant is refused
        // without waiting.
        let shared = match (&self.slots, self.ancestor_marker.is_some()) {
            (Some(dir), false) => slots::usable(dir, ask.weight, self)?,
            _ => None,
        };
        if shared.is_none() {
            self.give_back_kept("the host slots are no longer usable");
        }
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
                                bound: Bound::Budget,
                            },
                            _ => unreachable!("granted, duplicate and refused return above"),
                        };
                        // Never wait on the coordinator while holding a
                        // slot: its holders wait for slots while holding
                        // its tokens. The step takes a slot afresh.
                        self.give_back_kept(match &holding {
                            Holding::Disk(_) => "the coordinator held this step for the disk",
                            Holding::Slot { .. } => "the coordinator queued this step",
                        });
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
                // A setup's later step runs under the slot its first step
                // took; any other command (or a first step) queues for one.
                // `lease` drops on an error and gives its tokens back.
                // The marker its command gets: `held:<lease>` of the slot it
                // runs under, the kept one's for a setup's later step.
                let marker = match self.reuse_kept(ask) {
                    Some((weight, marker)) => {
                        lease.slots = HostHold::Kept { weight };
                        Some(marker)
                    }
                    None => match slots::take(dir, ask, self, &mut queue, gate, &lease.id)? {
                        Some(held) => {
                            // Read before `keep` moves the lease.
                            let marker = held.marker();
                            match self.keep(held) {
                                Ok(weight) => lease.slots = HostHold::Kept { weight },
                                Err(held) => lease.slots = HostHold::Own(held),
                            }
                            Some(marker)
                        }
                        None => None,
                    },
                };
                if let Some(marker) = marker {
                    // Under its own host slot: the slots' share, as greengate gives.
                    lease.env = self.environment(ask.weight, ask.env, Placement::UnderHostSlot);
                    lease.env.insert(HOST_SLOT_ENV.to_string(), marker);
                }
            }
            // The window between the grant and the use: space may have gone
            // since the coordinator looked. The lease is given back and the
            // command queues for the disk instead of running.
            match self.disk.short(estimate) {
                None => return Ok(lease),
                Some(short) => {
                    drop(lease);
                    self.give_back_kept("the disk fell short after the grant");
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

    /// Say once per run what was wrong with the host sizing keys, and that
    /// `greengate.hostSlots 0` has the host slots off, on stderr and as an
    /// event.
    pub fn note_host_sizing(&self, plan: &HostPlan) {
        if self.tries.sizing_said.swap(true, Ordering::SeqCst) {
            return;
        }
        for warning in &plan.warnings {
            eprintln!("relais: host sizing: {warning}");
            self.events.emit(Event::EnvironmentWarning {
                what: "host_sizing".into(),
                detail: warning.clone(),
            });
        }
        if let HostSlots::Off(by) = plan.slots {
            self.host_slots_off(&format!(
                "{} turns them off. Fix: {}",
                by.setting(),
                by.fix()
            ));
        }
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
        /// What ends this wait: the spent budget, or a stalled line (ADR-0015).
        bound: Bound,
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

    fn bound(&self) -> Bound {
        match self {
            Self::Slot { bound, .. } => *bound,
            Self::Disk(_) => Bound::Budget,
        }
    }
}

/// Time spent held, announced every [`HeavyAdmission::announce_every`] and
/// charged to the budget of what holds the command: `admission_wait_seconds`
/// for the queue, `disk_wait_seconds` for the disk floor, and the total for
/// both.
struct Queue<'q> {
    admission: &'q HeavyAdmission<'q>,
    wait: Option<(WaitKind, Bound, crate::policy::Wait)>,
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
    fn charging(&mut self, kind: WaitKind, bound: Bound) -> &mut crate::policy::Wait {
        if self
            .wait
            .as_ref()
            .is_none_or(|(held, held_bound, _)| *held != kind || *held_bound != bound)
        {
            self.wait = None;
            let now = self.admission.budgets.now();
            self.started = now;
            self.last_announced = now;
            let wait = match bound {
                Bound::Stall => self.admission.budgets.moving_wait(),
                Bound::Budget => self.admission.budgets.wait(kind),
            };
            self.wait = Some((kind, bound, wait));
        }
        match &mut self.wait {
            Some((_, _, wait)) => wait,
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
        if let Some(exhausted) = self.charging(holding.kind(), holding.bound()).tick() {
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
                    ..
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
    /// The host slots this command runs under.
    slots: HostHold,
}

/// The host slots a command runs under, held by this runner and never by
/// the check.
enum HostHold {
    None,
    /// Its own: released when the lease drops, after the check ended.
    Own(greenharness_slots::Lease),
    /// Its setup's ([`SetupSlot`]): not released when this lease drops.
    Kept {
        weight: usize,
    },
}

impl<'a> Admitted<'a> {
    fn unadmitted(env: BTreeMap<String, String>) -> Self {
        Self {
            gate: None,
            id: String::new(),
            env,
            slots: HostHold::None,
        }
    }

    fn leased(gate: &'a (dyn Gate + Sync), id: String, env: BTreeMap<String, String>) -> Self {
        Self {
            gate: Some(gate),
            id,
            env,
            slots: HostHold::None,
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

    /// How many host slots this command runs under: its own, or those its
    /// setup keeps.
    pub fn host_slots(&self) -> usize {
        match &self.slots {
            HostHold::None => 0,
            HostHold::Own(lease) => lease.weight(),
            HostHold::Kept { weight } => *weight,
        }
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
    //! The host slots, through greengate's `greenharness-slots` crate (amont ADR-0014):
    //! a waiter keeps its place across polls in the crate's one wait loop
    //! (`wait_in_line_paced`, amont ADR-0022), and relais owns the pause
    //! between polls through its own queue, so the coordinator lease keeps its
    //! heartbeat and the wait is charged like any other.

    use std::ops::ControlFlow;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use greenharness_slots::{Ended, LineEnd, Pause, WaitBounds, WaitEnd};

    use super::{Ask, Bound, Event, Gate, HeavyAdmission, Holding, LeaseStop, Queue, SlotLineEnd};

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
        if !greenharness_slots::SUPPORTED {
            admission.host_slots_off("host slots need flock, which this platform does not have");
            return Ok(None);
        }
        if let Err(why) = greenharness_slots::safe_dir(dir) {
            admission.host_slots_off(&why);
            return Ok(None);
        }
        let Some(sizing) = admission.sizing else {
            // A directory with the host slots off: queued nowhere.
            return Ok(None);
        };
        let cap = sizing.cap_for(admission.machine.cores);
        if weight > cap {
            return Err(LeaseStop::HostSlots {
                weight,
                slots: cap as usize,
                dir: dir.display().to_string(),
            });
        }
        Ok(Some(dir.to_path_buf()))
    }

    /// The holder record of a relais command: its label, under the session
    /// that dispatched the run, so that session may move it ahead with
    /// `greengate slot promote` (amont host-slot-admission t06, t24). With
    /// no session, or only the `unattributed-` stand-in, the run id names
    /// it: no caller is either, and the run id tells such rows apart where
    /// an 8-character `unattrib` would not.
    pub(super) fn holder(
        admission: &HeavyAdmission<'_>,
        label: &str,
    ) -> greenharness_slots::Holder {
        let mut holder = greenharness_slots::Holder::here("relais", label);
        let session = &admission.session_id;
        holder.session = if session.is_empty() || session.starts_with("unattributed-") {
            admission.run_id.clone()
        } else {
            session.clone()
        };
        holder
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
    ) -> Result<Option<greenharness_slots::Lease>, LeaseStop> {
        let Some(sizing) = admission.sizing else {
            return Ok(None);
        };
        let holder = holder(admission, ask.label);
        let started = std::time::Instant::now();
        let slot_cap = sizing.cap_for(admission.machine.cores);
        let width = admission.width(ask.weight, super::Placement::UnderHostSlot);
        // Every wait line names the sizing the command was queued under.
        let record = |waited_ms: u128, outcome: &str, end: Option<greenharness_slots::WaitEnd>| {
            greenharness_slots::record_wait_named(
                dir,
                "relais",
                greenharness_slots::WaitContext {
                    cap: Some(slot_cap),
                    cores: Some(admission.machine.cores),
                    end,
                    ..greenharness_slots::WaitContext::default()
                },
                ask.weight,
                Some(width),
                waited_ms,
                outcome,
            );
        };
        let waiter = match greenharness_slots::Waiter::new_in_on(
            dir,
            sizing,
            ask.weight,
            holder,
            admission.machine,
        ) {
            Ok(waiter) => waiter,
            Err(e) => {
                admission.host_slots_off(&format!("a host slot could not be queued for: {e}"));
                return Ok(None);
            }
        };
        // A line that moved inside the window keeps the wait going past
        // `admission_wait_seconds`; one that did not ends it (ADR-0015). The
        // crate's one loop (amont ADR-0022) owns the stall rule and the
        // failure count, relais the pause; its budgets end a wait, not a ceiling.
        let bounds = WaitBounds {
            window: Some(admission.stall_window),
            ceiling: Duration::MAX,
        };
        let mut joined_at: Option<usize> = None;
        let ended = greenharness_slots::wait_in_line_paced(
            waiter,
            bounds,
            |_| {},
            |step| {
                let (position, behind) = match (step, joined_at) {
                    (Pause::Waiting(queued), _) => {
                        joined_at = Some(queued.position);
                        (queued.position, behind(queued))
                    }
                    (Pause::Retrying { .. }, Some(position)) => (position, None),
                    // Not joined yet: the slots cannot be used, said once.
                    (Pause::Retrying { error, .. }, None) => {
                        admission
                            .host_slots_off(&format!("a host slot could not be locked: {error}"));
                        return ControlFlow::Break(Halt::Unusable);
                    }
                };
                // The tokens are held while the slots are awaited: say so, so
                // the coordinator does not take the lease for abandoned.
                let _ = gate.heartbeat(lease); // best-effort: the next poll beats again
                let slot = Holding::Slot {
                    class: "host_slot",
                    position,
                    behind,
                    bound: Bound::Stall,
                };
                match queue.wait(slot, ask.cancel, admission.sleeper.as_ref()) {
                    Ok(()) => ControlFlow::Continue(()),
                    Err(stop) => ControlFlow::Break(Halt::Stop(stop)),
                }
            },
        );
        let waited_ms = started.elapsed().as_millis();
        let (what, end, journal) = match ended {
            Ok(held) => {
                record(waited_ms, "admitted", None);
                let over = admission.budgets.overrun();
                if !over.is_zero() {
                    admission.events.emit(Event::Decision {
                        what: "admission_overrun".into(),
                        reason: format!(
                            "queued {} s past `admission_wait_seconds` while the host slot \
                             line moved; charged to the total only",
                            over.as_secs()
                        ),
                    });
                }
                return Ok(Some(held));
            }
            Err(Ended::Caller(Halt::Unusable)) => return Ok(None),
            // Ended by the budget or a cancel: a sample too, or the day-14
            // wait percentiles would see only the admissions.
            Err(Ended::Caller(Halt::Stop(stop))) => {
                record(waited_ms, "deferred", end_of(&stop));
                return Err(stop);
            }
            Err(Ended::Line(LineEnd::PollFailed { error })) => (
                "admission_queue_unreadable",
                SlotLineEnd::Unreadable {
                    failures: greenharness_slots::POLL_FAILURES,
                    cause: error.to_string(),
                },
                WaitEnd::IoError,
            ),
            Err(Ended::Line(LineEnd::Stalled { window, position })) => (
                "admission_queue_ended",
                SlotLineEnd::Stalled {
                    window_s: window.as_secs(),
                    position,
                },
                WaitEnd::Stalled,
            ),
            // Never reached under `Duration::MAX`; named, never a panic.
            Err(Ended::Line(LineEnd::Ceiling { waited })) => (
                "admission_queue_ended",
                SlotLineEnd::Ceiling {
                    waited_s: waited.as_secs(),
                },
                WaitEnd::Ceiling,
            ),
        };
        record(waited_ms, "deferred", Some(journal));
        admission.events.emit(Event::Decision {
            what: what.into(),
            reason: end.to_string(),
        });
        Err(LeaseStop::SlotLine(end))
    }

    /// What ends a host-slot wait from relais's side of the pause: slots that
    /// could not be used before the line was joined (said once, the command
    /// runs unqueued), or a stop of relais's own queue (a spent budget, a
    /// cancel).
    enum Halt {
        Unusable,
        Stop(LeaseStop),
    }

    /// The journal's `end` for a wait the queue stopped: a spent budget is a
    /// ceiling, and a cancel ends no line.
    fn end_of(stop: &LeaseStop) -> Option<greenharness_slots::WaitEnd> {
        matches!(stop, LeaseStop::Exhausted(_)).then_some(greenharness_slots::WaitEnd::Ceiling)
    }

    /// `relais: cargo test (amont-wt-x, 4m), 1 unknown (old client)`, or
    /// nothing when no holder said who it is.
    fn behind(queued: &greenharness_slots::Queue) -> Option<String> {
        let now = greenharness_slots::now_ms();
        let mut who: Vec<String> = queued.holders.iter().map(|h| h.describe(now)).collect();
        if queued.unknown > 0 {
            who.push(format!("{} unknown (old client)", queued.unknown));
        }
        (!who.is_empty()).then(|| who.join(", "))
    }
}

#[cfg(test)]
mod tests;
