//! Reconciling an interrupted run (SPEC §12), as a pure decision.
//!
//! An absent terminal result never means nothing executed. Before a run
//! can be marked interrupted, every dispatch the ledger still records as
//! live has to be judged: a worker that is still running must not have
//! its run rewritten underneath it, and a worker that is provably gone
//! must be closed rather than left "launched" for ever.
//!
//! The policy lived in `main.rs` as eighty lines of loop and `println!`,
//! which made it untestable and put the one rule that matters — resume
//! never re-dispatches — out of reach of a unit test (C11). Here it is a
//! function of three inputs: what the ledger says is live, what the
//! coordinator says about the run, and whether a PID is alive. The ledger
//! write and the printing stay with the caller.
//!
//! `LiveDispatch::pid` is `None` for "no PID was ever recorded" and
//! nothing else: a column holding a number no process could have is a
//! corrupt row the ledger refuses, not a missing PID, and the two lead to
//! different verdicts here.

use std::time::Duration;

use crate::admission::RunStatus;
use crate::ids::{DispatchId, Pid};
use crate::ledger::LiveDispatch;

/// An attempt's wall time when neither the contract nor the policy names one.
pub const DEFAULT_WALL_SECONDS: u64 = 2700;
/// How long past the attempt wall time a run may stay silent before its
/// driver is presumed gone: the runner ends an attempt at its wall time and
/// records it, so no live driver is silent for longer.
pub const DRIVER_MARGIN: Duration = Duration::from_secs(15 * 60);

/// How long a non-terminal run may write nothing before status flags it
/// `stalled`. A hint for a reader, never a verdict: it reaps nothing.
pub const STALLED_AFTER: Duration = Duration::from_secs(10 * 60);

/// Whether a run silent for `silent` is stalled past `after` (injected, so a
/// test needs no ten-minute wait).
pub fn is_stalled(silent: Duration, after: Duration) -> bool {
    silent >= after
}

/// A non-terminal run whose driver has stopped writing for too long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub silent: Duration,
    pub threshold: Duration,
    /// Why the default wall time stood in, when it did.
    pub defaulted_because: Option<String>,
}

impl Orphan {
    /// The sentence recorded with the `driver_gone` transition.
    pub fn detail(&self) -> String {
        let basis = match &self.defaulted_because {
            Some(reason) => format!(
                ", the {DEFAULT_WALL_SECONDS}s default wall time having been used because {reason}"
            ),
            None => String::new(),
        };
        format!(
            "the driving `relais run` is gone: nothing was written for {}s, past the {}s threshold (attempt wall time plus a {}s margin{basis})",
            self.silent.as_secs(),
            self.threshold.as_secs(),
            DRIVER_MARGIN.as_secs()
        )
    }
}

/// An attempt's wall time, and why the default stood in for it if it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptWall {
    pub duration: Duration,
    pub defaulted_because: Option<String>,
}

/// The attempt wall time: the contract's, else the policy's, else the
/// default. `policy` is the reason the policy could not be read when it
/// could not, so a default is never silent.
pub fn attempt_wall(contract: Option<u64>, policy: Result<u64, String>) -> AttemptWall {
    match (contract, policy) {
        (Some(seconds), _) | (None, Ok(seconds)) => AttemptWall {
            duration: Duration::from_secs(seconds),
            defaulted_because: None,
        },
        (None, Err(reason)) => AttemptWall {
            duration: Duration::from_secs(DEFAULT_WALL_SECONDS),
            defaulted_because: Some(reason),
        },
    }
}

/// Whether a run silent for `silent` is orphaned, given its attempt wall time.
pub fn orphan_of(silent: Duration, wall: AttemptWall) -> Option<Orphan> {
    let threshold = wall.duration + DRIVER_MARGIN;
    (silent > threshold).then_some(Orphan {
        silent,
        threshold,
        defaulted_because: wall.defaulted_because,
    })
}

/// How long a driver that took a run is presumed alive whatever the probes
/// say: a resume of an old run stamps a new driver, and its first minute
/// must not be judged against a pid that has only just been recorded.
pub const DRIVER_GRACE: Duration = Duration::from_secs(60);

/// What the ledger and the filesystem say about one non-terminal run's
/// driver, gathered by the caller so [`orphan_verdict`] stays pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanFacts {
    pub driver_pid: Option<u32>,
    /// The driver's process start time as recorded when it took the run.
    pub driver_started: Option<String>,
    /// When the driver took the run; `None` for a row no driver stamped.
    pub driver_since: Option<chrono::DateTime<chrono::Utc>>,
    /// The pids of the run's open dispatches (those that have one).
    pub dispatch_pids: Vec<u32>,
    /// How long the run has written nothing; `None` when unknown.
    pub silent_for: Option<Duration>,
    /// The silence past which a run whose driver cannot be judged by pid
    /// is presumed orphaned (attempt wall time plus [`DRIVER_MARGIN`]).
    pub silence_threshold: Duration,
}

/// What the evidence says about a run's driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanVerdict {
    /// The driver is provably gone: reap the run.
    Gone,
    /// The driver, or a dispatch, is alive, or nothing says otherwise: the
    /// run is never reaped, however silent it is.
    Alive,
    /// Liveness cannot be judged by pid, and the run has been silent past
    /// the threshold: the silence rule applies.
    Silent,
}

/// Judge one run's driver. `alive` is the liveness probe (EPERM counts as
/// alive: `procs::alive`), `start_time` reads a process start time.
///
/// Gone needs all of: the driver took the run more than [`DRIVER_GRACE`]
/// ago; its pid is dead or now has another start time (a reused pid); no
/// dispatch pid is alive. A live dispatch keeps the run in every case. A
/// live driver with the recorded start time is Alive however silent.
/// Silence decides only when pid evidence is missing: no recorded pid, or
/// a start time that cannot be compared.
pub fn orphan_verdict(
    facts: &OrphanFacts,
    alive: &dyn Fn(u32) -> bool,
    start_time: &dyn Fn(u32) -> Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> OrphanVerdict {
    if facts.dispatch_pids.iter().any(|pid| alive(*pid)) {
        return OrphanVerdict::Alive;
    }
    let past_grace = facts.driver_since.is_none_or(|since| {
        now.signed_duration_since(since)
            .to_std()
            .is_ok_and(|age| age > DRIVER_GRACE)
    });
    let by_silence = || match facts.silent_for {
        Some(silent) if silent > facts.silence_threshold => OrphanVerdict::Silent,
        Some(_) | None => OrphanVerdict::Alive,
    };
    let gone = || {
        if past_grace {
            OrphanVerdict::Gone
        } else {
            OrphanVerdict::Alive
        }
    };
    let Some(pid) = facts.driver_pid else {
        return by_silence();
    };
    if !alive(pid) {
        return gone();
    }
    match (start_time(pid), facts.driver_started.as_deref()) {
        (Some(now_started), Some(recorded)) if now_started == recorded => OrphanVerdict::Alive,
        (Some(_), Some(_)) => gone(),
        (None, _) | (_, None) => by_silence(),
    }
}

/// What reconciliation established about one dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchVerdict {
    /// A PID is on record and that process is still there. Resume
    /// refuses: re-dispatching now would run the task twice.
    StillAlive { dispatch: DispatchId, pid: Pid },
    /// A PID is on record and that process is gone. The dispatch can be
    /// closed; what it did or did not finish is still preserved.
    ProvablyDead { dispatch: DispatchId, pid: Pid },
    /// No PID was recorded, and the coordinator still counts this run as
    /// active or waiting: a worker may be between admission and launch.
    /// Refused for the same reason as `StillAlive`.
    CoordinatorHoldsASeat { dispatch: DispatchId },
    /// No PID was recorded and nothing else knows: the outcome is
    /// unknown. Unknown is never retried and never assumed dead.
    Unknown { dispatch: DispatchId },
    /// A native agent of a Claude Code session whose relais plugin is
    /// still saying hello: the session, and so maybe the agent, lives.
    /// Refused like `StillAlive`.
    SessionLive {
        dispatch: DispatchId,
        session: String,
    },
    /// A native agent of a session whose plugin stopped saying hello
    /// (`native::HELLO_FRESH`): a subagent does not outlive its session,
    /// so the agent is gone, as provably as a PID that is gone.
    SessionGone {
        dispatch: DispatchId,
        session: String,
    },
}

impl DispatchVerdict {
    pub fn dispatch(&self) -> &DispatchId {
        match self {
            DispatchVerdict::StillAlive { dispatch, .. }
            | DispatchVerdict::ProvablyDead { dispatch, .. }
            | DispatchVerdict::CoordinatorHoldsASeat { dispatch }
            | DispatchVerdict::Unknown { dispatch }
            | DispatchVerdict::SessionLive { dispatch, .. }
            | DispatchVerdict::SessionGone { dispatch, .. } => dispatch,
        }
    }

    /// How this verdict reads in the line the CLI prints.
    pub fn describe(&self) -> String {
        match self {
            DispatchVerdict::StillAlive { dispatch, pid } => format!("{dispatch} (pid {pid})"),
            DispatchVerdict::ProvablyDead { dispatch, pid } => {
                format!("{dispatch} (pid {pid} is gone)")
            }
            DispatchVerdict::CoordinatorHoldsASeat { dispatch } => {
                format!("{dispatch} (no pid recorded; the coordinator holds a seat)")
            }
            DispatchVerdict::Unknown { dispatch } => {
                format!("{dispatch} (no pid recorded; nothing knows its outcome)")
            }
            DispatchVerdict::SessionLive { dispatch, session } => {
                format!("{dispatch} (a native agent of session {session}, whose relais plugin is still live)")
            }
            DispatchVerdict::SessionGone { dispatch, session } => {
                format!("{dispatch} (a native agent of session {session}, which has ended)")
            }
        }
    }
}

/// The verdict on every live dispatch of one run, in the order the ledger
/// reported them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub verdicts: Vec<DispatchVerdict>,
}

impl Reconciliation {
    /// The dispatches that forbid resuming: a live process, or a seat the
    /// coordinator still holds.
    pub fn refusing(&self) -> Vec<&DispatchVerdict> {
        self.verdicts
            .iter()
            .filter(|verdict| {
                matches!(
                    verdict,
                    DispatchVerdict::StillAlive { .. }
                        | DispatchVerdict::CoordinatorHoldsASeat { .. }
                        | DispatchVerdict::SessionLive { .. }
                )
            })
            .collect()
    }

    /// Whether reconciliation may proceed at all.
    pub fn may_reconcile(&self) -> bool {
        self.refusing().is_empty()
    }

    /// Whether every dispatch that was live is PROVABLY gone — the
    /// condition under which the run's worktree may be retired: an
    /// `Unknown` dispatch may still be writing it, so a reconciliation
    /// that may proceed is not yet one that may remove the tree. No
    /// live dispatch at all is trivially proven.
    pub fn all_provably_dead(&self) -> bool {
        self.verdicts.iter().all(|verdict| match verdict {
            DispatchVerdict::ProvablyDead { .. } | DispatchVerdict::SessionGone { .. } => true,
            DispatchVerdict::StillAlive { .. }
            | DispatchVerdict::CoordinatorHoldsASeat { .. }
            | DispatchVerdict::Unknown { .. }
            | DispatchVerdict::SessionLive { .. } => false,
        })
    }

    /// The dispatches to close as `reconciled_dead`, which the caller
    /// writes to the ledger.
    pub fn provably_dead(&self) -> Vec<&DispatchId> {
        self.verdicts
            .iter()
            .filter_map(|verdict| match verdict {
                DispatchVerdict::ProvablyDead { dispatch, .. }
                | DispatchVerdict::SessionGone { dispatch, .. } => Some(dispatch),
                DispatchVerdict::StillAlive { .. }
                | DispatchVerdict::CoordinatorHoldsASeat { .. }
                | DispatchVerdict::Unknown { .. }
                | DispatchVerdict::SessionLive { .. } => None,
            })
            .collect()
    }

    /// The sentence recorded with the interrupted transition: what was
    /// proved dead and what stayed unknown, never folded together.
    pub fn detail(&self) -> String {
        if self.verdicts.is_empty() {
            return "no dispatch was live; the run stopped before or between launches".to_string();
        }
        let dead: Vec<String> = self
            .verdicts
            .iter()
            .filter(|verdict| {
                matches!(
                    verdict,
                    DispatchVerdict::ProvablyDead { .. } | DispatchVerdict::SessionGone { .. }
                )
            })
            .map(DispatchVerdict::describe)
            .collect();
        let unknown: Vec<String> = self
            .verdicts
            .iter()
            .filter(|verdict| matches!(verdict, DispatchVerdict::Unknown { .. }))
            .map(|verdict| verdict.dispatch().to_string())
            .collect();
        format!(
            "provably dead: [{}]; unknown outcome (uncertain, not retried): [{}]",
            dead.join(", "),
            unknown.join(", ")
        )
    }

    /// The reason resume refused, for the operator.
    pub fn refusal(&self) -> String {
        self.refusing()
            .into_iter()
            .map(DispatchVerdict::describe)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Judge every dispatch the ledger still records as live for one run.
///
/// `dispatches` is already narrowed to the run. `coordinator_view` is what
/// the coordinator says about that run, or `None` when no coordinator
/// answered — which is not evidence of anything, so it can only leave a
/// dispatch `Unknown`, never mark one dead. `alive` is the liveness probe
/// (`procs::alive` in production), injected so the table below is a unit
/// test rather than a process-spawning one.
pub fn reconcile(
    dispatches: &[LiveDispatch],
    coordinator_view: Option<&RunStatus>,
    alive: &dyn Fn(Pid) -> bool,
) -> Reconciliation {
    reconcile_with_sessions(dispatches, coordinator_view, alive, &|_| None)
}

/// [`reconcile`], with what is known of the Claude Code sessions native
/// agents belong to: `session_live(session)` is `Some(true)` while that
/// session's relais plugin says hello, `Some(false)` once it has stopped,
/// and `None` when nothing answered (no evidence, so the dispatch stays
/// judged as before). A native agent has no PID relais owns; its session
/// is what it lives and dies with, and that outranks a coordinator seat
/// the run that died left behind.
pub fn reconcile_with_sessions(
    dispatches: &[LiveDispatch],
    coordinator_view: Option<&RunStatus>,
    alive: &dyn Fn(Pid) -> bool,
    session_live: &dyn Fn(&str) -> Option<bool>,
) -> Reconciliation {
    let coordinator_busy = coordinator_view
        .is_some_and(|run: &RunStatus| run.active > 0 || run.waiting > 0 || run.queued > 0);
    let verdicts = dispatches
        .iter()
        .map(|live| {
            let dispatch = live.dispatch.clone();
            let native_session = (live.source.as_deref() == Some("native_run"))
                .then_some(live.session_id.as_deref())
                .flatten();
            let session = native_session.and_then(|session| {
                session_live(session).map(|is_live| (session.to_string(), is_live))
            });
            match (live.pid, session) {
                (Some(pid), _) if alive(pid) => DispatchVerdict::StillAlive { dispatch, pid },
                (Some(pid), _) => DispatchVerdict::ProvablyDead { dispatch, pid },
                (None, Some((session, true))) => DispatchVerdict::SessionLive { dispatch, session },
                (None, Some((session, false))) => {
                    DispatchVerdict::SessionGone { dispatch, session }
                }
                (None, None) if coordinator_busy => {
                    DispatchVerdict::CoordinatorHoldsASeat { dispatch }
                }
                (None, None) => DispatchVerdict::Unknown { dispatch },
            }
        })
        .collect();
    Reconciliation { verdicts }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_run_is_stalled_only_past_the_injected_threshold() {
        let after = Duration::from_secs(60);
        assert!(is_stalled(Duration::from_secs(60), after));
        assert!(is_stalled(Duration::from_secs(120), after));
        assert!(!is_stalled(Duration::from_secs(5), after));
        assert_eq!(STALLED_AFTER, Duration::from_secs(600));
    }

    use super::*;
    use crate::ids::RunId;

    fn live(dispatch: &str, pid: Option<u32>) -> LiveDispatch {
        LiveDispatch {
            dispatch: DispatchId::from_stored(dispatch.to_string()),
            run: RunId::from_stored("run-1"),
            pid: pid.map(Pid::new),
            session_id: Some("tab".into()),
            reserve_micros: 0,
            source: Some("managed_run".into()),
            agent_id: None,
            parent_dispatch: None,
            depth: Some(0),
        }
    }

    fn busy_run(active: u32, waiting: u32, queued: u32) -> RunStatus {
        RunStatus {
            session_id: "tab".into(),
            budget_micros: None,
            reserved_micros: 0,
            settled_micros: 0,
            uncertain_settlements: 0,
            admitted_total: 1,
            active,
            waiting,
            queued,
            cancelled: false,
        }
    }

    fn facts() -> OrphanFacts {
        OrphanFacts {
            driver_pid: Some(100),
            driver_started: Some("t0".into()),
            driver_since: Some(at(-3600)),
            dispatch_pids: vec![],
            silent_for: Some(Duration::from_secs(5)),
            silence_threshold: Duration::from_secs(3600),
        }
    }

    fn at(offset_seconds: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now() + chrono::Duration::seconds(offset_seconds)
    }

    fn verdict(facts: &OrphanFacts, alive: bool, started: Option<&str>) -> OrphanVerdict {
        let started = started.map(str::to_string);
        orphan_verdict(facts, &|_| alive, &|_| started.clone(), chrono::Utc::now())
    }

    #[test]
    fn a_dead_driver_pid_is_gone() {
        assert_eq!(verdict(&facts(), false, None), OrphanVerdict::Gone);
    }

    #[test]
    fn a_live_driver_with_its_start_time_is_never_reaped_however_silent() {
        let mut f = facts();
        f.silent_for = Some(Duration::from_secs(5 * 3600));
        assert_eq!(verdict(&f, true, Some("t0")), OrphanVerdict::Alive);
    }

    #[test]
    fn a_reused_pid_with_another_start_time_is_gone() {
        assert_eq!(verdict(&facts(), true, Some("t9")), OrphanVerdict::Gone);
    }

    #[test]
    fn no_pid_and_silent_past_the_threshold_is_silent() {
        let mut f = facts();
        f.driver_pid = None;
        f.silent_for = Some(Duration::from_secs(7200));
        assert_eq!(verdict(&f, true, None), OrphanVerdict::Silent);
    }

    #[test]
    fn no_pid_and_recent_is_alive() {
        let mut f = facts();
        f.driver_pid = None;
        assert_eq!(verdict(&f, false, None), OrphanVerdict::Alive);
    }

    #[test]
    fn an_unreadable_start_time_falls_back_to_silence() {
        let mut f = facts();
        f.silent_for = Some(Duration::from_secs(7200));
        assert_eq!(verdict(&f, true, None), OrphanVerdict::Silent);
        f.silent_for = Some(Duration::from_secs(5));
        assert_eq!(verdict(&f, true, None), OrphanVerdict::Alive);
    }

    #[test]
    fn a_live_dispatch_pid_keeps_the_run_in_every_case() {
        let probe_alive = |pid: u32| pid == 555;
        let no_start = |_: u32| None;
        let now = chrono::Utc::now();
        let mut cases = vec![facts()];
        let mut silent = facts();
        silent.driver_pid = None;
        silent.silent_for = Some(Duration::from_secs(7200));
        cases.push(silent);
        let mut reused = facts();
        reused.driver_started = Some("other".into());
        cases.push(reused);
        for mut f in cases {
            f.dispatch_pids = vec![554, 555];
            assert_eq!(
                orphan_verdict(&f, &probe_alive, &no_start, now),
                OrphanVerdict::Alive,
                "{f:?}"
            );
        }
    }

    #[test]
    fn a_driver_that_took_the_run_under_a_minute_ago_is_not_gone() {
        let mut f = facts();
        f.driver_since = Some(at(-30));
        assert_eq!(verdict(&f, false, None), OrphanVerdict::Alive);
        assert_eq!(verdict(&f, true, Some("t9")), OrphanVerdict::Alive);
        f.driver_since = Some(at(-61));
        assert_eq!(verdict(&f, false, None), OrphanVerdict::Gone);
    }

    const NOTHING_IS_ALIVE: &dyn Fn(Pid) -> bool = &|_| false;
    const EVERYTHING_IS_ALIVE: &dyn Fn(Pid) -> bool = &|_| true;

    #[test]
    fn a_run_is_orphaned_only_past_wall_time_plus_margin() {
        let wall = attempt_wall(Some(600), Ok(1200));
        assert_eq!(wall.duration, Duration::from_secs(600));
        assert_eq!(
            attempt_wall(None, Ok(1200)).duration,
            Duration::from_secs(1200)
        );
        assert_eq!(
            attempt_wall(None, Err("x".into())).duration,
            Duration::from_secs(2700)
        );
        assert_eq!(orphan_of(Duration::from_secs(1500), wall.clone()), None);
        let orphan = orphan_of(Duration::from_secs(1501), wall).expect("orphaned");
        assert_eq!(orphan.threshold, Duration::from_secs(1500));
        assert_eq!(
            orphan.detail(),
            "the driving `relais run` is gone: nothing was written for 1501s, past the 1500s threshold (attempt wall time plus a 900s margin)"
        );
    }

    #[test]
    fn a_defaulted_wall_time_says_why() {
        let wall = attempt_wall(None, Err("no relais.toml was readable".into()));
        let orphan = orphan_of(Duration::from_secs(9000), wall).expect("orphaned");
        assert_eq!(
            orphan.detail(),
            "the driving `relais run` is gone: nothing was written for 9000s, past the 3600s threshold (attempt wall time plus a 900s margin, the 2700s default wall time having been used because no relais.toml was readable)"
        );
    }

    #[test]
    fn a_live_worker_refuses_the_resume() {
        let outcome = reconcile(&[live("d1", Some(4242))], None, EVERYTHING_IS_ALIVE);
        assert_eq!(
            outcome.verdicts,
            vec![DispatchVerdict::StillAlive {
                dispatch: DispatchId::from_stored("d1"),
                pid: Pid::new(4242),
            }]
        );
        assert!(!outcome.may_reconcile(), "resume never re-dispatches");
        assert!(outcome.provably_dead().is_empty());
        assert!(
            outcome.refusal().contains("pid 4242"),
            "{}",
            outcome.refusal()
        );
    }

    #[test]
    fn a_recorded_pid_that_is_gone_is_provably_dead() {
        let outcome = reconcile(&[live("d1", Some(7))], None, NOTHING_IS_ALIVE);
        assert!(outcome.may_reconcile());
        assert!(outcome.all_provably_dead(), "the worktree may be retired");
        assert_eq!(
            outcome.provably_dead(),
            vec![&DispatchId::from_stored("d1")]
        );
        let detail = outcome.detail();
        assert!(detail.contains("pid 7 is gone"), "{detail}");
        assert!(detail.contains("unknown outcome"), "{detail}");
    }

    /// "No PID recorded" is its own row: it is not a dead worker, and it
    /// is not a number that failed to parse. A PID column holding a value
    /// no process could have is a corrupt row the ledger refuses
    /// (`Pid::stored`), so it can never arrive here as `None` (C11).
    #[test]
    fn no_pid_recorded_is_not_a_pid_out_of_range() {
        assert_eq!(Pid::stored(-1), None, "a negative pid is not a pid");
        assert_eq!(Pid::stored(1 << 40), None, "nor is one wider than a pid");
        assert_eq!(Pid::stored(31), Some(Pid::new(31)));

        // With nothing else to go on, a dispatch with no PID is unknown:
        // never assumed dead, never retried.
        let outcome = reconcile(&[live("d1", None)], None, NOTHING_IS_ALIVE);
        assert_eq!(
            outcome.verdicts,
            vec![DispatchVerdict::Unknown {
                dispatch: DispatchId::from_stored("d1")
            }]
        );
        assert!(outcome.may_reconcile());
        assert!(
            outcome.provably_dead().is_empty(),
            "an unknown outcome is not a dead one"
        );
        assert!(
            !outcome.all_provably_dead(),
            "and a worktree an unknown dispatch may still write is not retired"
        );
        assert!(outcome.detail().contains("uncertain, not retried"));
    }

    #[test]
    fn no_pid_but_a_seat_the_coordinator_still_holds_refuses() {
        for (active, waiting, queued) in [(1, 0, 0), (0, 1, 0), (0, 0, 1)] {
            let view = busy_run(active, waiting, queued);
            let outcome = reconcile(&[live("d1", None)], Some(&view), NOTHING_IS_ALIVE);
            assert_eq!(
                outcome.verdicts,
                vec![DispatchVerdict::CoordinatorHoldsASeat {
                    dispatch: DispatchId::from_stored("d1")
                }],
                "active {active} waiting {waiting} queued {queued}"
            );
            assert!(!outcome.may_reconcile());
        }
        // A coordinator that knows the run and counts nothing for it is
        // evidence the other way: nothing holds a seat.
        let idle = busy_run(0, 0, 0);
        let outcome = reconcile(&[live("d1", None)], Some(&idle), NOTHING_IS_ALIVE);
        assert!(matches!(
            outcome.verdicts.as_slice(),
            [DispatchVerdict::Unknown { .. }]
        ));
        assert!(outcome.may_reconcile());
    }

    #[test]
    fn a_run_with_nothing_live_reconciles_and_says_so() {
        let outcome = reconcile(&[], None, NOTHING_IS_ALIVE);
        assert!(outcome.may_reconcile());
        assert!(outcome.all_provably_dead());
        assert!(outcome.verdicts.is_empty());
        assert_eq!(
            outcome.detail(),
            "no dispatch was live; the run stopped before or between launches"
        );
    }

    /// Every row at once: one live worker is enough to refuse, and the
    /// dead one is still named — the operator needs both halves.
    #[test]
    fn the_verdicts_keep_every_dispatch_apart() {
        let dispatches = [
            live("alive", Some(11)),
            live("dead", Some(12)),
            live("nopid", None),
        ];
        let only_11_is_alive: &dyn Fn(Pid) -> bool = &|pid| pid == Pid::new(11);
        let outcome = reconcile(&dispatches, None, only_11_is_alive);
        assert_eq!(outcome.verdicts.len(), 3);
        assert!(!outcome.may_reconcile());
        assert_eq!(outcome.refusing().len(), 1);
        assert_eq!(
            outcome.provably_dead(),
            vec![&DispatchId::from_stored("dead")]
        );
        let detail = outcome.detail();
        assert!(detail.contains("dead (pid 12 is gone)"), "{detail}");
        assert!(detail.contains("[nopid]"), "{detail}");
    }

    fn native(dispatch: &str, session: &str) -> LiveDispatch {
        LiveDispatch {
            source: Some("native_run".into()),
            session_id: Some(session.into()),
            ..live(dispatch, None)
        }
    }

    /// A native agent whose session ended (its plugin stopped saying
    /// hello) is gone: the run reconciles, even with the seat the dead
    /// run left in the coordinator, and its tree may be retired.
    #[test]
    fn a_native_agent_of_an_ended_session_is_gone_whatever_the_seat() {
        let view = busy_run(1, 0, 0);
        let outcome = reconcile_with_sessions(
            &[native("d1", "tab-gone")],
            Some(&view),
            NOTHING_IS_ALIVE,
            &|session| Some(session != "tab-gone"),
        );
        assert!(outcome.may_reconcile(), "{}", outcome.refusal());
        assert!(outcome.all_provably_dead());
        assert_eq!(outcome.provably_dead(), [&DispatchId::from_stored("d1")]);
        assert!(
            outcome
                .detail()
                .contains("session tab-gone, which has ended"),
            "{}",
            outcome.detail()
        );
    }

    /// A native agent whose session still says hello may be running:
    /// resume refuses, as for a live PID.
    #[test]
    fn a_native_agent_of_a_live_session_refuses_the_resume() {
        let outcome =
            reconcile_with_sessions(&[native("d1", "tab")], None, NOTHING_IS_ALIVE, &|_| {
                Some(true)
            });
        assert!(!outcome.may_reconcile());
        assert!(
            outcome.refusal().contains("still live"),
            "{}",
            outcome.refusal()
        );
    }

    /// With nothing known of the session, a native dispatch is judged as
    /// before: a held seat refuses, no seat leaves it unknown.
    #[test]
    fn a_native_agent_with_no_session_evidence_is_judged_as_before() {
        let view = busy_run(1, 0, 0);
        let held = reconcile_with_sessions(
            &[native("d1", "tab")],
            Some(&view),
            NOTHING_IS_ALIVE,
            &|_| None,
        );
        assert!(!held.may_reconcile());
        let free =
            reconcile_with_sessions(&[native("d1", "tab")], None, NOTHING_IS_ALIVE, &|_| None);
        assert!(free.may_reconcile());
        assert!(!free.all_provably_dead(), "unknown is never assumed dead");
    }
}
