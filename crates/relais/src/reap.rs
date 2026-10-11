//! Reaping an orphaned run: moving it out of a non-terminal state once its
//! driver is gone, from either the CLI or the coordinator daemon.
//!
//! The move is a compare-and-set ([`Ledger::transition_if`]): of several
//! reapers, or a reaper and a driver that ends at the same moment, exactly
//! one writes the transition. Only that one closes the open dispatches and
//! appends the run's end to `events.jsonl`, through the append-only writer
//! ([`protocol::append_event`]) because this process is not the driver. The
//! function returns its message; the caller prints or logs it.

use std::path::Path;

use crate::ids::{Pid, RunId};
use crate::ledger::{Ledger, LedgerError, Transition};
use crate::lifecycle::{Reason, State};
use crate::protocol::{self, Event};
use crate::resume::{self, DispatchVerdict};

/// How a reaped run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The driver died: the run is `interrupted` (`driver_gone`).
    Interrupted,
    /// A cancel found no worker to carry it out: the run is `cancelled`
    /// (`cancelled_without_worker`).
    Cancelled,
}

/// What a reap did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reaped {
    /// This call moved the run; the line to show.
    Moved(String),
    /// The run was no longer in the expected state: someone else moved it,
    /// and nothing was written.
    AlreadyMoved,
    /// A dispatch of the run is provably alive: nothing was changed.
    KeptAlive(String),
}

/// Why a reap did not complete cleanly.
#[derive(Debug)]
pub enum ReapError {
    /// The ledger could not be read or written; nothing was moved by us.
    Ledger(LedgerError),
    /// The run was moved in the ledger (and its dispatches closed), but its
    /// end could not be appended to `events.jsonl`: timeline readers will
    /// still see the run as running. `message` is what the reap did.
    EventsNotWritten {
        message: String,
        cause: std::io::Error,
    },
}

impl std::fmt::Display for ReapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(e) => write!(f, "{e}"),
            Self::EventsNotWritten { message, cause } => write!(
                f,
                "{message}, but its end could not be appended to events.jsonl: {cause}"
            ),
        }
    }
}

impl std::error::Error for ReapError {}

impl From<LedgerError> for ReapError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}

/// What to reap and why.
#[derive(Debug, Clone)]
pub struct Reap<'a> {
    pub run: &'a RunId,
    /// The state the run is expected to be in; the reap writes nothing if
    /// it has moved.
    pub from: State,
    pub ending: Ending,
    /// The evidence, as a sentence: shown in the returned message.
    pub detail: String,
    /// More evidence, recorded with the transition.
    pub extra: serde_json::Value,
    /// The run's artifacts directory, where `events.jsonl` lives.
    pub events_dir: &'a Path,
}

/// Move a run from its expected state to its ending.
pub fn reap(
    ledger: &Ledger,
    request: &Reap<'_>,
    alive: &dyn Fn(Pid) -> bool,
) -> Result<Reaped, ReapError> {
    let Reap {
        run,
        from,
        ending,
        detail,
        extra,
        events_dir,
    } = request;
    let (run, from, ending) = (*run, *from, *ending);
    let live: Vec<_> = ledger
        .live_dispatches()?
        .into_iter()
        .filter(|live| live.run == *run)
        .collect();
    let judged = resume::reconcile(&live, None, alive);
    let living: Vec<String> = judged
        .verdicts
        .iter()
        .filter(|verdict| matches!(verdict, DispatchVerdict::StillAlive { .. }))
        .map(DispatchVerdict::describe)
        .collect();
    if !living.is_empty() {
        return Ok(Reaped::KeptAlive(format!(
            "dispatch {} is alive",
            living.join(", ")
        )));
    }
    let (driver_pid, _, _) = ledger.run_driver(run)?;
    let (to, reason) = match ending {
        Ending::Interrupted => (State::Interrupted, Reason::DriverGone),
        Ending::Cancelled => (State::Cancelled, Reason::CancelledWithoutWorker),
    };
    let moved = ledger.transition_if(
        from,
        &Transition {
            run_id: run.clone(),
            attempt_id: None,
            from_state: Some(from),
            to_state: to,
            reason: reason.as_str().to_string(),
            detail: Some(serde_json::json!({ "detail": detail, "driver_pid": driver_pid, "evidence": extra })),
            at: crate::ledger::now_rfc3339(),
        },
    )?;
    if !moved {
        return Ok(Reaped::AlreadyMoved);
    }
    for verdict in &judged.verdicts {
        ledger.finish_dispatch(verdict.dispatch(), reason.as_str())?;
    }
    ledger.close_open_dispatches(run, reason.as_str())?;
    let sentence = sentence_of(run, ending, driver_pid);
    let name = run.as_str();
    let message = format!("{run}: {from} -> {to} ({detail})");
    // Both lines are attempted, so a failed first does not hide the end;
    // the ledger already records the reap, so a failure is reported, not
    // undone.
    let stderr = protocol::append_event(
        name,
        events_dir,
        &Event::Stderr {
            text: format!("{}: {sentence}", reason.as_str()),
        },
    );
    let outcome = protocol::append_event(
        name,
        events_dir,
        &Event::Outcome {
            state: to.as_str().to_string(),
            receipt: None,
            summary: None,
            reason: Some(sentence),
        },
    );
    match stderr.and(outcome) {
        Ok(()) => Ok(Reaped::Moved(message)),
        Err(cause) => Err(ReapError::EventsNotWritten { message, cause }),
    }
}

/// The full sentence a reaped run's outcome carries.
fn sentence_of(run: &RunId, ending: Ending, driver_pid: Option<i64>) -> String {
    let driver = match driver_pid {
        Some(pid) => format!("its driver (pid {pid}) is gone"),
        None => "its driver is gone (no pid was recorded)".to_string(),
    };
    let verb = match ending {
        Ending::Interrupted => "interrupted",
        Ending::Cancelled => "cancelled",
    };
    format!(
        "run {run} {verb}: {driver}; the worktree is kept, `relais resume --retire {run}` removes it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;
    use crate::test_support::short_temp_dir;
    use std::sync::Arc;

    fn world(tag: &str) -> (crate::test_support::TempDir, std::path::PathBuf, RunId) {
        let dir = short_temp_dir(tag);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = RunId::from_stored("run-reap");
        ledger
            .insert_run(&run, "/repo", None, &TaskId::from_stored("task-reap"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Running,
                reason: "start".into(),
                detail: None,
                at: crate::ledger::now_rfc3339(),
            })
            .expect("running");
        let path = dir.join("ledger.sqlite");
        (dir, path, run)
    }

    fn request<'a>(run: &'a RunId, ending: Ending, events_dir: &'a Path) -> Reap<'a> {
        Reap {
            run,
            from: State::Running,
            ending,
            detail: "test".into(),
            extra: serde_json::Value::Null,
            events_dir,
        }
    }

    fn reap_once(path: &Path, events: &Path, run: &RunId) -> Reaped {
        let ledger = Ledger::open(path).expect("ledger");
        reap(&ledger, &request(run, Ending::Interrupted, events), &|_| {
            false
        })
        .expect("reap")
    }

    #[test]
    fn two_threads_reaping_one_run_leave_one_transition_and_one_outcome() {
        let (dir, path, run) = world("reap2");
        let events = Arc::new(dir.join("events-dir"));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let (path, events, run) = (path.clone(), events.clone(), run.clone());
                std::thread::spawn(move || reap_once(&path, &events, &run))
            })
            .collect();
        let results: Vec<Reaped> = handles
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect();
        let moved = results
            .iter()
            .filter(|r| matches!(r, Reaped::Moved(_)))
            .count();
        assert_eq!(moved, 1, "{results:?}");
        let ledger = Ledger::open(&path).expect("ledger");
        let interrupted = ledger
            .transitions(&run)
            .expect("transitions")
            .into_iter()
            .filter(|t| t.to_state == State::Interrupted)
            .count();
        assert_eq!(interrupted, 1);
        let text = std::fs::read_to_string(events.join("events.jsonl")).expect("events");
        let outcomes = text
            .lines()
            .filter(|l| l.contains("\"kind\":\"outcome\""))
            .count();
        assert_eq!(outcomes, 1, "{text}");
        let timeline = protocol::timeline("run-reap", &text);
        assert_eq!(timeline["outcome"]["state"], "interrupted");
        let reason = timeline["outcome"]["reason"].as_str().expect("reason");
        assert!(
            reason.contains("relais resume --retire run-reap"),
            "{reason}"
        );
        assert!(text.lines().all(|l| !l.contains("\"seq\"")), "{text}");
    }

    #[test]
    fn an_events_append_failure_is_reported_not_discarded() {
        let (dir, path, run) = world("reapf");
        let ledger = Ledger::open(&path).expect("ledger");
        // A file where the events directory should be: every append fails.
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "x").expect("file");
        let err = reap(
            &ledger,
            &request(&run, Ending::Interrupted, &blocker),
            &|_| false,
        )
        .expect_err("the append failure is returned");
        assert!(matches!(err, ReapError::EventsNotWritten { .. }), "{err}");
        assert!(err.to_string().contains("events.jsonl"), "{err}");
        // The ledger move stands.
        assert_eq!(
            ledger.run_status(&run).expect("status"),
            Some(State::Interrupted)
        );
    }

    #[test]
    fn the_stderr_prefix_names_the_ending() {
        for (ending, prefix) in [
            (Ending::Interrupted, "driver_gone:"),
            (Ending::Cancelled, "cancelled_without_worker:"),
        ] {
            let (dir, path, run) = world("reapp");
            let ledger = Ledger::open(&path).expect("ledger");
            let events = dir.join("ev");
            reap(&ledger, &request(&run, ending, &events), &|_| false).expect("reap");
            let text = std::fs::read_to_string(events.join("events.jsonl")).expect("events");
            assert!(text.contains(&format!("\"text\":\"{prefix} ")), "{text}");
        }
    }

    #[test]
    fn a_reaped_run_is_not_reaped_again() {
        let (dir, path, run) = world("reap1");
        let events = dir.join("ev");
        assert!(matches!(reap_once(&path, &events, &run), Reaped::Moved(_)));
        assert_eq!(reap_once(&path, &events, &run), Reaped::AlreadyMoved);
    }

    #[test]
    fn a_cancel_without_a_worker_ends_the_run_cancelled() {
        let (dir, path, run) = world("reapc");
        let ledger = Ledger::open(&path).expect("ledger");
        let events = dir.join("ev");
        let reaped = reap(&ledger, &request(&run, Ending::Cancelled, &events), &|_| {
            false
        })
        .expect("reap");
        assert!(matches!(reaped, Reaped::Moved(_)), "{reaped:?}");
        assert_eq!(
            ledger.run_status(&run).expect("status"),
            Some(State::Cancelled)
        );
    }
}
