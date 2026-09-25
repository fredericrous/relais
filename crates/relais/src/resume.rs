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

use crate::admission::RunStatus;
use crate::ids::{DispatchId, Pid};
use crate::ledger::LiveDispatch;

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
}

impl DispatchVerdict {
    pub fn dispatch(&self) -> &DispatchId {
        match self {
            DispatchVerdict::StillAlive { dispatch, .. }
            | DispatchVerdict::ProvablyDead { dispatch, .. }
            | DispatchVerdict::CoordinatorHoldsASeat { dispatch }
            | DispatchVerdict::Unknown { dispatch } => dispatch,
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
            DispatchVerdict::ProvablyDead { .. } => true,
            DispatchVerdict::StillAlive { .. }
            | DispatchVerdict::CoordinatorHoldsASeat { .. }
            | DispatchVerdict::Unknown { .. } => false,
        })
    }

    /// The dispatches to close as `reconciled_dead`, which the caller
    /// writes to the ledger.
    pub fn provably_dead(&self) -> Vec<&DispatchId> {
        self.verdicts
            .iter()
            .filter_map(|verdict| match verdict {
                DispatchVerdict::ProvablyDead { dispatch, .. } => Some(dispatch),
                DispatchVerdict::StillAlive { .. }
                | DispatchVerdict::CoordinatorHoldsASeat { .. }
                | DispatchVerdict::Unknown { .. } => None,
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
            .filter(|verdict| matches!(verdict, DispatchVerdict::ProvablyDead { .. }))
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
    let coordinator_busy = coordinator_view
        .is_some_and(|run: &RunStatus| run.active > 0 || run.waiting > 0 || run.queued > 0);
    let verdicts = dispatches
        .iter()
        .map(|live| {
            let dispatch = live.dispatch.clone();
            match live.pid {
                Some(pid) if alive(pid) => DispatchVerdict::StillAlive { dispatch, pid },
                Some(pid) => DispatchVerdict::ProvablyDead { dispatch, pid },
                None if coordinator_busy => DispatchVerdict::CoordinatorHoldsASeat { dispatch },
                None => DispatchVerdict::Unknown { dispatch },
            }
        })
        .collect();
    Reconciliation { verdicts }
}

#[cfg(test)]
mod tests {
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

    const NOTHING_IS_ALIVE: &dyn Fn(Pid) -> bool = &|_| false;
    const EVERYTHING_IS_ALIVE: &dyn Fn(Pid) -> bool = &|_| true;

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
}
