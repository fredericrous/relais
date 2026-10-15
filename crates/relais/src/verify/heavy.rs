//! The heavy-command lease (SPEC §23, ADR-0010): every setup step and every
//! check, at the base and at the candidate, takes `weight` tokens of the
//! machine-wide `HeavyLocal` class from the coordinator before it starts,
//! holds them while its process group runs, and gives them back after.
//!
//! Admission has four parts, in this order:
//!
//! 1. the coordinator's tokens (`weight` of them, queued behind whoever
//!    holds them);
//! 2. when `[resources] shared_host_slots` names a directory, `weight`
//!    `flock` slot files in it, all or none, so amont's heavy checks and
//!    relais's share one host queue (amont ADR-0009);
//! 3. the child's environment: the parallelism bounds below, then the
//!    check's own `env`;
//! 4. once the process group exists, its id goes to the coordinator, which
//!    from then on keeps the tokens for as long as a member of the group
//!    runs, even if this process dies.
//!
//! Time spent in 1 and 2 is queue time: it draws on `admission_wait_seconds`
//! and `total_seconds`, never on work (`Budgets::wait`).
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
    ResourceClass, ADMISSION_POLL,
};
use crate::policy::{Budgets, Exhausted, WaitKind};
use crate::protocol::{Event, Events};

/// How often a queued command says it is still queued.
pub const ANNOUNCE_EVERY: Duration = Duration::from_secs(30);

/// The environment variable a held host slot exports, so an amont started
/// inside the check does not queue again (amont ADR-0009).
pub const HOST_SLOT_ENV: &str = "AMONT_HOST_SLOT";

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
pub fn dispatch_id(run: &str, phase: &str, label: &str, attempt: u32) -> String {
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
    format!("{run}-heavy-{}-{}-{attempt}", clean(phase), clean(label))
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

/// Every try of every command of one run, so a rerun gets a new id.
#[derive(Debug, Clone, Default)]
pub struct Tries(Arc<Mutex<BTreeMap<String, u32>>>);

impl Tries {
    fn next(&self, key: &str) -> u32 {
        let mut tries = self.0.lock().unwrap_or_else(|p| p.into_inner());
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
    pub poll: Duration,
    pub announce_every: Duration,
    pub tries: Tries,
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
            poll: ADMISSION_POLL,
            announce_every: ANNOUNCE_EVERY,
            tries: Tries::default(),
        }
    }

    /// Wait for admission, or say why not. With no gate the command is
    /// unadmitted: it runs, with only its own `env`.
    pub fn admit(&self, ask: &Ask<'_>) -> Result<Admitted<'a>, LeaseStop> {
        let Some(gate) = self.gate else {
            return Ok(Admitted::unadmitted(ask.env.clone()));
        };
        if ask.weight == 0 || ask.weight > self.cap {
            return Err(LeaseStop::Weight {
                weight: ask.weight,
                cap: self.cap,
            });
        }
        let attempt = self.tries.next(&format!("{}\u{1}{}", ask.phase, ask.label));
        let id = dispatch_id(&self.run_id, ask.phase, ask.label, attempt);
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
        };
        let mut queue = Queue::new(self);
        loop {
            match gate.admit(&request) {
                Err(e) => return Err(LeaseStop::Gate(e)),
                Ok(Decision::Granted) => break,
                Ok(Decision::AlreadyAdmitted) => return Err(LeaseStop::Duplicate(id)),
                Ok(Decision::Refused { code, detail }) => {
                    return Err(LeaseStop::Refused { code, detail })
                }
                Ok(Decision::Queued { position }) => {
                    if let Err(stop) = queue.wait("heavy_local", position, ask.cancel) {
                        // Best effort: nothing started, and a request the
                        // coordinator still holds expires unclaimed.
                        let _ = gate.withdraw(&id);
                        return Err(stop);
                    }
                }
            }
        }
        let mut lease = Admitted::leased(gate, id, ask.env.clone());
        lease.env = child_environment(parallelism(ask.weight, self.cores, self.cap), ask.env);
        if let Some(dir) = &self.slots {
            match slots::take(
                dir, ask.weight, self, &mut queue, ask.cancel, gate, &lease.id,
            ) {
                Ok(Some(held)) => {
                    lease.slots = held;
                    lease
                        .env
                        .insert(HOST_SLOT_ENV.to_string(), "held".to_string());
                }
                Ok(None) => {}
                // `lease` drops here and gives its tokens back.
                Err(stop) => return Err(stop),
            }
        }
        Ok(lease)
    }
}

/// Time spent queued, charged to `admission_wait_seconds` and the total,
/// announced every [`HeavyAdmission::announce_every`].
struct Queue<'q> {
    admission: &'q HeavyAdmission<'q>,
    wait: crate::policy::Wait,
    started: i64,
    last_announced: i64,
}

impl<'q> Queue<'q> {
    fn new(admission: &'q HeavyAdmission<'q>) -> Self {
        let started = admission.budgets.now();
        Self {
            admission,
            wait: admission.budgets.wait(WaitKind::Admission),
            started,
            last_announced: started,
        }
    }

    /// One poll's worth of waiting: charge it, stop if a budget ran out or
    /// the run was cancelled, announce when it is time, then sleep.
    fn wait(
        &mut self,
        class: &str,
        position: usize,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), LeaseStop> {
        if let Some(exhausted) = self.wait.tick() {
            self.admission.events.emit(Event::Decision {
                what: "admission_queue_ended".into(),
                reason: format!("still queued for {class} at position {position}"),
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
            self.admission.events.emit(Event::WaitingForSlot {
                class: class.to_string(),
                position,
                waited_s: u64::try_from((now - self.started).max(0) / 1000).unwrap_or(0),
            });
        }
        std::thread::sleep(self.admission.poll);
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
    /// A coordinator that cannot take it is reported, not hidden.
    pub fn bound_to_group(&self, pgid: u32, events: &Events) {
        let Some(gate) = self.gate else { return };
        let problem = match gate.bind_group(&self.id, std::process::id(), pgid) {
            Ok(BindOutcome::Bound) => return,
            Ok(other) => format!("{other:?}"),
            Err(e) => e.to_string(),
        };
        events.emit(Event::EnvironmentWarning {
            what: "heavy_lease_not_tied_to_group".into(),
            detail: format!(
                "lease {} was not tied to process group {pgid} ({problem}); its tokens return \
                 when this run ends, not when the group does",
                self.id
            ),
        });
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
    //! amont's host-slot protocol (ADR-0009): `slot-<i>` files in one
    //! private directory, locked with `flock(LOCK_EX | LOCK_NB)`.

    use std::fs::File;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use super::{Gate, HeavyAdmission, LeaseStop, Queue};

    /// Take `want` slot files of `total`, all or none: on a partial set
    /// every file taken is released and the attempt backs off.
    pub(super) fn try_take(
        dir: &Path,
        total: u32,
        want: u32,
    ) -> std::io::Result<Option<Vec<File>>> {
        let mut held = Vec::new();
        for index in 0..total {
            if held.len() as u32 == want {
                break;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join(format!("slot-{index}")))?;
            if crate::procs::try_flock(&file)? {
                held.push(file);
            }
        }
        // Dropping `held` on a partial set unlocks it.
        Ok((held.len() as u32 == want).then_some(held))
    }

    /// Make the directory if needed, then accept it only when it is a real
    /// directory this user owns with mode 0700: on a shared `/tmp`, another
    /// user could create it first and hold every slot.
    pub(super) fn private(dir: &Path) -> Result<(), String> {
        if dir.symlink_metadata().is_err() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
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

    /// Wait for `weight` slot files, charging the wait like the queue for
    /// tokens. `Ok(None)`: the directory is unusable, which is said and
    /// not fatal — the host queue is amont's courtesy, not relais's rule.
    pub(super) fn take(
        dir: &Path,
        weight: u32,
        admission: &HeavyAdmission<'_>,
        queue: &mut Queue<'_>,
        cancel: Option<&AtomicBool>,
        gate: &(dyn Gate + Sync),
        lease: &str,
    ) -> Result<Option<Vec<File>>, LeaseStop> {
        if let Err(why) = private(dir) {
            admission
                .events
                .emit(crate::protocol::Event::EnvironmentWarning {
                    what: "host_slots_off".into(),
                    detail: format!("shared host slots are off for this command: {why}"),
                });
            return Ok(None);
        }
        let total = super::default_heavy_commands(admission.cores);
        let want = weight.min(total);
        loop {
            match try_take(dir, total, want) {
                Ok(Some(held)) => return Ok(Some(held)),
                Ok(None) => {}
                Err(e) => {
                    admission
                        .events
                        .emit(crate::protocol::Event::EnvironmentWarning {
                            what: "host_slots_off".into(),
                            detail: format!(
                                "shared host slots are off for this command: {}: {e}",
                                dir.display()
                            ),
                        });
                    return Ok(None);
                }
            }
            // The tokens are held while the slots are awaited: say so, so
            // the coordinator does not take the lease for abandoned.
            let _ = gate.heartbeat(lease);
            queue.wait("host_slot", 0, cancel)?;
        }
    }
}

#[cfg(not(unix))]
mod slots {
    //! `flock` is unix-only: on other platforms the coordinator's token
    //! queue is the whole queue.

    use std::fs::File;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use super::{Gate, HeavyAdmission, LeaseStop, Queue};

    pub(super) fn take(
        _dir: &Path,
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
