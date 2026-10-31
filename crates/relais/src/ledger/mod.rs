//! The machine-local ledger (SPEC §12).
//!
//! SQLite for records (runs, contract revisions, attempts, transitions,
//! evidence, usage events, dispatch intents), an artifact directory per
//! run, explicit payload schema versions with additive migrations.
//! Dispatch intent is persisted before spawning a process; on restart
//! liveness, terminal output and artifacts are reconciled before another
//! attempt is scheduled. An absent terminal result never means nothing
//! executed. Every write is a short transaction; no transaction is ever
//! held across a model call — the API makes that structural by only
//! offering one-shot writes.

use std::path::Path;

use rusqlite::{Connection, Transaction, TransactionBehavior};

mod admission;
mod clock;
mod error;
mod evidence;
pub(crate) mod failpoint;
mod holdout_counts;
mod holdout_definitions;
mod learning;
mod outcomes;
mod review;
mod review_parts;
pub(crate) mod router;
mod runs;
mod schema;
mod sessions;
mod snapshot;
mod successor;
mod test_support;
mod transitions;
mod trials;
mod trust;
mod usage;
pub use admission::{GroupBindingRow, LedgerGrowth};
pub use clock::{now_rfc3339, Clock, FixedClock, SystemClock};
pub use error::LedgerError;
use error::{parse_state, Result};
pub use evidence::{EvidenceKind, EvidenceOrigin, EvidenceRow};
pub use holdout_counts::HoldoutOutcomeRow;
pub use learning::{
    LearningAction, LearningCycleFinish, LearningCycleRow, LearningSource, LearningVerdict, LiveRun,
};
pub use outcomes::{RevertCandidate, RevertedReport, StoredOutcome};
pub use runs::{
    ChildOf, ChildRun, ModelObservation, ModelPair, ReviewDispatch, RunWorkspace, TaskOrigin,
    TaskRow, WorkerAttempt, EFFORT_NOT_REQUESTED, EFFORT_NO_CONTROL, EFFORT_UNKNOWN,
};
pub use schema::LEDGER_SCHEMA_VERSION;
use schema::{applied_count, migrate};
pub use sessions::OrchestrationUsageRow;
pub use snapshot::{ReceiptResolution, VerificationSnapshot};
pub use successor::{
    Claim, IntentReview, IntentState, NewIntent, SuccessorIntent, SuccessorProcess,
    CLAIM_GRACE_SECONDS,
};
pub use transitions::{
    Actor, Answerer, DecisionAnswer, DecisionRecord, PersonAnswerCounts, RecordedVia, RunDriver,
    Transition, ADVISOR_EVENT_PREFIX, LEGACY_ACTORS,
};
pub use trials::{
    GroundTruth, GroundTruthVerdict, JudgeRefusal, Judged, NewRecipePromotion, NewReplayTrial,
    NewTrial, RecipePromotion, TrialCost, TrialCostError, TrialOutcome, TrialRow, TrialUsage,
    LIVE_WORKTREE,
};
pub use trust::{StoredDeclaration, TrustGrantRecord};
pub use usage::{Booking, DispatchBooking, LiveDispatch, PhaseCost, UsageEvent};
mod task_types;
pub use review::{MergeTotals, ReviewClassTotals, ReviewOutcomeRow};
pub use review_parts::{PartResult, PartStatus, PlannedPart, ReviewPartRow};
pub use router::{
    ExplorationTask, RouterCheck, RouterClass, RouterClassificationRow, RouterDecisionRow,
    RouterExplorationStop, RouterProvenance, RouterReassessRow, RouterRecord, RouterTaskOutcomeRow,
    RouterTaskRow, RouterUsageRow, RouterWindow, UNMATCHED_CHECKS_MAX,
};
pub use task_types::{
    ExportedTasks, ImportedTaskTypes, StoredTaskType, TaskExportRow, TaskTypeSource,
};

pub struct Ledger {
    conn: Connection,
    path: std::path::PathBuf,
    clock: std::sync::Arc<dyn Clock>,
}

/// How long any wait on the ledger's locks lasts before it is an error:
/// the busy handler's timeout, and the deadline for the WAL switch that
/// the busy handler does not cover.
const LEDGER_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a read-only open waits on a lock before it gives up. A
/// read-only caller sits on somebody's path (the plugin asks just before
/// a message goes out), and "cannot tell" is an answer it can use; five
/// seconds of waiting is not.
const READ_ONLY_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// The pause after the first busy refusal; it doubles up to the cap.
const FIRST_BACKOFF: std::time::Duration = std::time::Duration::from_millis(1);

/// The longest pause between two tries.
const BACKOFF_CAP: std::time::Duration = std::time::Duration::from_millis(25);

/// Whether SQLite refused because another connection holds a lock
/// (SQLITE_BUSY). SQLITE_LOCKED is not this: it is a conflict inside one
/// connection, a bug to report at once rather than wait out.
fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy
    )
}

/// Run `operation` until it stops being refused as busy, for at most
/// `deadline`. Between tries it sleeps 1 ms, doubling to a 25 ms cap, so
/// a waiting caller does not spin a core. Any other error returns at
/// once; when the deadline passes, the last busy refusal is returned.
fn retry_while_busy<T>(
    deadline: std::time::Duration,
    mut operation: impl FnMut() -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let started = std::time::Instant::now();
    let mut pause = FIRST_BACKOFF;
    loop {
        match operation() {
            Err(e) if is_busy(&e) => {
                let left = deadline.saturating_sub(started.elapsed());
                if left.is_zero() {
                    return Err(e);
                }
                std::thread::sleep(pause.min(left));
                pause = (pause * 2).min(BACKOFF_CAP);
            }
            other => return other,
        }
    }
}

/// Put the ledger in WAL mode (SPEC §23: several processes share it).
///
/// `journal_mode` is persistent in the database FILE, so it is set once
/// and every later connection reads it back as `wal`. Changing it takes
/// a database-wide lock that SQLite refuses WITHOUT consulting the busy
/// handler, so `busy_timeout` does not cover this one statement: two
/// processes opening the same ledger in the same instant had one of them
/// die at the door with "database is locked" before it read a row. A
/// refusal here is not a broken ledger, it is somebody else setting the
/// same mode, so it is retried with a short backoff until
/// `LEDGER_BUSY_TIMEOUT` — the same time any other lock is waited for, so
/// a hang is bounded by that deadline, not by a count of tries. Any other
/// error returns at once. `PRAGMA journal_mode = WAL` answers with the
/// mode in force, which is the check and the change in one statement.
fn use_wal(conn: &Connection) -> Result<()> {
    let mode = retry_while_busy(LEDGER_BUSY_TIMEOUT, || {
        conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        })
    })?;
    if mode.eq_ignore_ascii_case("wal") {
        Ok(())
    } else {
        Err(LedgerError::Corrupt {
            what: "the ledger's journal mode".into(),
            detail: format!("SQLite kept `{mode}` when asked for WAL"),
        })
    }
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_clock(path, Box::new(SystemClock))
    }

    /// The same ledger on an injected clock.
    pub fn open_with_clock(path: &Path, clock: Box<dyn Clock>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        // FIRST, before any other statement. Several processes and
        // threads share one ledger in short transactions (SPEC §23), and
        // a writer in progress is a wait, not an error — including for
        // the `journal_mode` pragma below, which takes an exclusive lock
        // of its own. Set after it, as it used to be, two processes
        // opening the same ledger at the same moment raced and one died
        // with "database is locked" before it had opened anything.
        conn.busy_timeout(LEDGER_BUSY_TIMEOUT)?;
        use_wal(&conn)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&conn, clock.as_ref())?;
        Ok(Ledger {
            conn,
            path: path.to_path_buf(),
            clock: std::sync::Arc::from(clock),
        })
    }

    /// Fold the write-ahead log into the database file, truncate it, and
    /// close the connection. A ledger closed here leaves no `-wal` sidecar
    /// behind, so the file can be linked or copied on its own. An error
    /// (a reader holding the log, a failed close) is returned, never
    /// swallowed: the caller must not treat the file as self-contained.
    pub fn close(self) -> Result<()> {
        let still_busy: i64 =
            self.conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if still_busy != 0 {
            return Err(LedgerError::Corrupt {
                what: "the write-ahead log".into(),
                detail: "a checkpoint could not complete because another connection holds the log"
                    .into(),
            });
        }
        self.conn.close().map_err(|(_, error)| error.into())
    }

    /// Whether SQLite opened this connection read-only. [`Self::open`] on a
    /// file it cannot write (another owner, a read-only mount, an ACL)
    /// falls back to a read-only connection without an error; this is the
    /// one answer that covers every such cause.
    pub fn is_read_only(&self) -> Result<bool> {
        Ok(self.conn.is_readonly(rusqlite::DatabaseName::Main)?)
    }

    /// The ledger for reading only, when there is one this binary can read.
    ///
    /// Unlike [`Self::open`], this creates no directory and no file,
    /// writes no pragma and runs no migration: a missing ledger is an
    /// error (`SQLITE_CANTOPEN`), and so is one on any other schema than
    /// this binary's ([`LedgerError::SchemaMismatch`]). Its busy wait is
    /// [`READ_ONLY_BUSY_TIMEOUT`], not the writers' five seconds. Every
    /// write through it fails, which is the point: nothing that only
    /// reads can change the ledger.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(READ_ONLY_BUSY_TIMEOUT)?;
        let applied = applied_count(&conn)?;
        if applied != LEDGER_SCHEMA_VERSION {
            return Err(LedgerError::SchemaMismatch {
                found: applied,
                known: LEDGER_SCHEMA_VERSION,
            });
        }
        Ok(Ledger {
            conn,
            path: path.to_path_buf(),
            clock: std::sync::Arc::new(SystemClock),
        })
    }

    /// A short write transaction, taking the write lock up front
    /// (`BEGIN IMMEDIATE`). Two processes then serialise here instead of
    /// racing to a commit one of them loses.
    ///
    /// Unchecked because the ledger owns its connection and no method
    /// below opens a transaction inside another: every writer here is a
    /// leaf. Nothing does I/O inside one (SPEC §12: no transaction is
    /// ever held across a model call).
    fn write_tx(&self) -> Result<Transaction<'_>> {
        // The failpoint a test arms to meet a full disk at a ledger write
        // (SPEC §10); nothing in a release build.
        crate::storage_full::at(crate::storage_full::Site::Ledger)?;
        Ok(Transaction::new_unchecked(
            &self.conn,
            TransactionBehavior::Immediate,
        )?)
    }

    /// The ledger's idea of now — the one clock every record it writes
    /// is stamped by, and the one the runner stamps its events with.
    pub fn now(&self) -> String {
        self.clock.now_rfc3339()
    }

    /// The same clock, shared: what the run's budgets read, so a test
    /// that scripts the ledger's clock scripts them too.
    pub fn clock(&self) -> std::sync::Arc<dyn Clock> {
        std::sync::Arc::clone(&self.clock)
    }

    /// Where this ledger lives, so a side thread can open its own
    /// connection (a connection is not shareable across threads).
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<u64> {
        applied_count(&self.conn)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{dispatch, event, run, task, temp_dir, temp_ledger};
    use super::*;
    use crate::ids::Pid;
    use crate::lifecycle::{Reason, State, UsagePhase};
    use crate::money::{CostCompleteness, MicroUsd};
    use crate::route::RoutedBy;
    use rusqlite::params;

    /// A read-only open shut out by an exclusive lock gives up after
    /// `READ_ONLY_BUSY_TIMEOUT`, not the writers' five seconds: it sits
    /// on the path to a session's next turn. Timed in-process, so the
    /// bound measures the wait and not the load on the machine.
    #[test]
    fn a_read_only_open_gives_up_after_its_own_short_wait() {
        let (ledger, dir) = temp_ledger();
        let path = ledger.path().to_path_buf();
        drop(ledger);
        let holder = Connection::open(&path).expect("holder");
        holder
            .execute_batch(
                "PRAGMA locking_mode = EXCLUSIVE; \
                 BEGIN IMMEDIATE; UPDATE schema_migrations SET applied_at = applied_at; COMMIT;",
            )
            .expect("take the exclusive lock");
        let started = std::time::Instant::now();
        let error = match Ledger::open_read_only(&path) {
            Ok(_) => panic!("a reader got past an exclusive lock"),
            Err(error) => error,
        };
        let waited = started.elapsed();
        assert!(
            matches!(&error, LedgerError::Sqlite(e) if is_busy(e)),
            "{error}"
        );
        assert!(waited >= READ_ONLY_BUSY_TIMEOUT, "gave up after {waited:?}");
        assert!(waited < LEDGER_BUSY_TIMEOUT, "waited {waited:?}");
        drop(holder);
        drop(dir);
    }

    /// Reading never creates: a missing ledger is an error, and no file,
    /// directory or journal appears.
    #[test]
    fn a_read_only_open_creates_nothing() {
        let dir = temp_dir("read-only-missing");
        let path = dir.join("state").join("ledger.sqlite");
        assert!(Ledger::open_read_only(&path).is_err());
        assert!(!dir.join("state").exists());
    }

    /// P1: two processes opening the same fresh ledger at once used to
    /// race between "is this step applied?" and applying it. They now
    /// serialise on the write lock and the loser re-checks inside its
    /// own transaction.
    #[test]
    fn two_openers_of_a_fresh_ledger_both_succeed() {
        let dir = temp_dir("concurrent");
        let path = dir.join("ledger.sqlite");
        let start = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let path = &path;
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        Ledger::open(path).map(|ledger| ledger.schema_version())
                    })
                })
                .collect();
            for handle in handles {
                let version = handle
                    .join()
                    .expect("no panic")
                    .expect("both openers succeed")
                    .expect("schema readable");
                assert_eq!(version, LEDGER_SCHEMA_VERSION);
            }
        });
        // Exactly one row per step, not two.
        let ledger = Ledger::open(&path).expect("reopen");
        assert_eq!(
            ledger.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn busy() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        )
    }

    /// A refusal that outlasts any count of yields is waited out — by
    /// sleeping, not spinning: a backoff of 1, 2, 4, 8, 16, 25 ms covers
    /// 60 ms in about seven calls, where a loop of thread yields either
    /// gives up within microseconds or burns every try.
    #[test]
    fn a_busy_refusal_lasting_60ms_is_waited_out() {
        let started = std::time::Instant::now();
        let mut calls = 0;
        let result = retry_while_busy(std::time::Duration::from_secs(5), || {
            calls += 1;
            if started.elapsed() < std::time::Duration::from_millis(60) {
                Err(busy())
            } else {
                Ok(7)
            }
        });
        assert_eq!(result.expect("the refusal ends before the deadline"), 7);
        assert!(
            calls <= 12,
            "a waiting opener sleeps between tries instead of spinning: {calls} calls"
        );
    }

    #[test]
    fn an_endless_busy_refusal_gives_up_at_the_deadline() {
        let started = std::time::Instant::now();
        let mut calls = 0;
        let result: rusqlite::Result<()> =
            retry_while_busy(std::time::Duration::from_millis(50), || {
                calls += 1;
                Err(busy())
            });
        let error = result.expect_err("never succeeds");
        assert!(is_busy(&error), "the busy error is returned: {error:?}");
        assert!(started.elapsed() >= std::time::Duration::from_millis(50));
        assert!(calls > 1, "it retried within the deadline: {calls} calls");
    }

    #[test]
    fn a_non_busy_error_is_returned_after_one_call() {
        let mut calls = 0;
        let result: rusqlite::Result<()> =
            retry_while_busy(std::time::Duration::from_secs(5), || {
                calls += 1;
                Err(rusqlite::Error::QueryReturnedNoRows)
            });
        assert!(matches!(result, Err(rusqlite::Error::QueryReturnedNoRows)));
        assert_eq!(calls, 1);
    }

    /// Many threads switching each fresh file to WAL at the same moment.
    /// On macOS the old 32-yield loop also passed this; it guards the
    /// slower Windows runner, where that loop ran out (#147).
    #[test]
    fn many_openers_of_fresh_ledgers_all_succeed() {
        const THREADS: usize = 6;
        let dir = temp_dir("stampede");
        for round in 0..30 {
            let path = dir.join(format!("ledger-{round}.sqlite"));
            let start = std::sync::Barrier::new(THREADS);
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..THREADS)
                    .map(|_| {
                        let path = &path;
                        let start = &start;
                        scope.spawn(move || {
                            start.wait();
                            Ledger::open(path).map(|_| ())
                        })
                    })
                    .collect();
                for handle in handles {
                    handle
                        .join()
                        .expect("no panic")
                        .expect("every opener succeeds");
                }
            });
        }
        // Best-effort temp cleanup: a leftover directory fails nothing.
        std::fs::remove_dir_all(&dir).ok();
    }

    // SPEC §12: a ledger is external state. A row this binary cannot read
    // is an error the CLI can print, never an abort in the middle of
    // `status`, `explain` or `report`.
    #[test]
    fn an_unknown_stored_state_is_an_error_not_a_panic() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-future"), "/repo", None, &task("run-future"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-future"),
                attempt_id: None,
                from_state: None,
                to_state: State::Running,
                reason: "dispatched".into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        // A state only a newer relais knows, in both places one is read.
        ledger
            .conn
            .execute_batch(
                "UPDATE runs SET status = 'hibernating' WHERE id = 'run-future';
                 UPDATE transitions SET to_state = 'hibernating' WHERE run_id = 'run-future';",
            )
            .expect("write the future");

        match ledger.run_status(&run("run-future")) {
            Err(LedgerError::Corrupt { what, detail }) => {
                assert_eq!(what, "run state");
                assert!(detail.contains("hibernating"), "{detail}");
            }
            other => panic!("an unknown state must be an error, got {other:?}"),
        }
        assert!(
            matches!(
                ledger.transitions(&run("run-future")),
                Err(LedgerError::Corrupt { .. })
            ),
            "reading the history of that run is an error too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_injected_clock_stamps_every_record() {
        let dir = temp_dir("clock");
        let ledger = Ledger::open_with_clock(
            &dir.join("ledger.sqlite"),
            Box::new(FixedClock::new([
                "2026-09-18T10:00:00+00:00",
                "2026-09-18T10:00:01+00:00",
            ])),
        )
        .expect("ledger");
        // Migrations already consumed the first tick or two; the
        // sequence then repeats its last value.
        ledger
            .insert_run(&run("run-1"), "/r", None, &task("run-1"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-1"),
                attempt_id: None,
                from_state: None,
                to_state: State::Running,
                reason: "plan_accepted".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
        let at = ledger.transitions(&run("run-1")).expect("transitions")[0]
            .at
            .clone();
        assert_eq!(at, "2026-09-18T10:00:01+00:00");
        assert_eq!(
            ledger.now(),
            "2026-09-18T10:00:01+00:00",
            "repeats its last value"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Empty usage means three different things depending on the run's
    /// own record of having started: a run never dispatched has a true
    /// zero cost; a run that dispatched an attempt and reported nothing
    /// back has an unknown cost, not a zero one; a run that reported one
    /// attempt and started a second that never reported has a real,
    /// incomplete lower bound. Pinned together so the three boundaries
    /// stay distinguishable from one another, not just individually true.
    #[test]
    fn a_killed_run_reports_unknown_or_incomplete_not_a_false_zero() {
        let (ledger, dir) = temp_ledger();

        ledger
            .insert_run(&run("run-never"), "/repo", None, &task("run-never"), "rk")
            .expect("run");
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-never"))
                .expect("completeness"),
            CostCompleteness::Actual,
            "a run that never dispatched an attempt truly spent nothing"
        );

        ledger
            .insert_run(&run("run-silent"), "/repo", None, &task("run-silent"), "rk")
            .expect("run");
        let revision = ledger
            .insert_contract_revision(&run("run-silent"), "h", "{}", "HEAD", None)
            .expect("revision");
        let attempt = ledger
            .insert_attempt(
                &run("run-silent"),
                revision,
                1,
                "implementation",
                UsagePhase::Initial,
            )
            .expect("attempt");
        ledger
            .record_dispatch_intent(
                &dispatch("disp-silent"),
                &run("run-silent"),
                Some(attempt),
                &serde_json::json!({}),
                0,
                RoutedBy::ConservativeBaseline,
            )
            .expect("intent");
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-silent"))
                .expect("completeness"),
            CostCompleteness::Unknown,
            "a dispatched attempt that reported no usage at all died before it could report"
        );

        ledger
            .insert_run(
                &run("run-partial"),
                "/repo",
                None,
                &task("run-partial"),
                "rk",
            )
            .expect("run");
        let revision = ledger
            .insert_contract_revision(&run("run-partial"), "h", "{}", "HEAD", None)
            .expect("revision");
        let first = ledger
            .insert_attempt(
                &run("run-partial"),
                revision,
                1,
                "implementation",
                UsagePhase::Initial,
            )
            .expect("first attempt");
        ledger
            .record_dispatch_intent(
                &dispatch("disp-partial-1"),
                &run("run-partial"),
                Some(first),
                &serde_json::json!({}),
                0,
                RoutedBy::ConservativeBaseline,
            )
            .expect("first intent");
        // Keyed by the DISPATCH id, which is what every producer does —
        // worker, reviewer and planner all set `event_id: dispatch_id`,
        // and all 47 usage events in a live ledger are keyed that way,
        // none otherwise. A fixture that invented its own id would be
        // asserting against a shape relais never writes.
        let mut reported = event("disp-partial-1", "run-partial", 500);
        reported.attempt_id = Some(first);
        ledger.record_usage(&reported).expect("reported");
        let second = ledger
            .insert_attempt(
                &run("run-partial"),
                revision,
                2,
                "implementation",
                UsagePhase::Repair,
            )
            .expect("second attempt");
        ledger
            .record_dispatch_intent(
                &dispatch("disp-partial-2"),
                &run("run-partial"),
                Some(second),
                &serde_json::json!({}),
                0,
                RoutedBy::ConservativeBaseline,
            )
            .expect("second intent");
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-partial"))
                .expect("completeness"),
            CostCompleteness::IncompleteLowerBound,
            "the first attempt's usage is real; the silent second attempt \
             makes the total a lower bound, not an unknown"
        );
        assert_eq!(
            ledger.run_cost(&run("run-partial")).expect("cost"),
            MicroUsd::from_micros(500),
            "the reported attempt's cost is still the true lower bound"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_intent_persists_before_the_process_exists() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-d"), "/repo", None, &task("run-d"), "rk")
            .expect("run");
        let intent = serde_json::json!({"model": "sonnet", "effort": "medium"});
        assert!(ledger
            .record_dispatch_intent(
                &dispatch("disp-1"),
                &run("run-d"),
                None,
                &intent,
                0,
                RoutedBy::ConservativeBaseline
            )
            .expect("intent"));
        assert!(
            !ledger
                .record_dispatch_intent(
                    &dispatch("disp-1"),
                    &run("run-d"),
                    None,
                    &intent,
                    0,
                    RoutedBy::ConservativeBaseline
                )
                .expect("retry"),
            "same dispatch ID cannot create a duplicate"
        );
        ledger
            .attach_dispatch_process(&dispatch("disp-1"), Some(Pid::new(4242)), Some("sess-1"))
            .expect("attach");
        let live = ledger.live_dispatches().expect("live");
        assert_eq!(
            live,
            vec![LiveDispatch {
                dispatch: dispatch("disp-1"),
                run: run("run-d"),
                pid: Some(Pid::new(4242)),
                session_id: Some("sess-1".into()),
                reserve_micros: 0,
                source: Some("managed_run".into()),
                agent_id: None,
                parent_dispatch: None,
                depth: Some(0),
            }]
        );
        ledger
            .finish_dispatch(&dispatch("disp-1"), "completed")
            .expect("finish");
        assert!(ledger.live_dispatches().expect("live").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // A row a pre-v12 binary wrote and never revisited: `source`,
    // `agent_id`, `parent_dispatch` and `depth` were never columns it
    // could have filled in, so they read back NULL forever. `live_dispatches`
    // must report that as absent, not silently reuse the post-migration
    // default of "root, depth 0, managed_run".
    #[test]
    fn a_pre_migration_row_reports_its_new_columns_as_unrecorded() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-legacy"), "/repo", None, &task("run-legacy"), "rk")
            .expect("run");
        // What a pre-v12 `record_dispatch_intent` + `attach_dispatch_process`
        // pair would have left behind: session and reservation set, the
        // four new columns never touched.
        ledger
            .conn
            .execute(
                "INSERT INTO dispatches
                    (dispatch_id, run_id, intent_json, state, pid, session_id,
                     reserved_micros, created_at, updated_at)
                 VALUES ('legacy-1', 'run-legacy', '{}', 'launched', 4242, 'sess-old',
                         500, 'now', 'now')",
                [],
            )
            .expect("legacy row");
        let live = ledger.live_dispatches().expect("live");
        assert_eq!(
            live,
            vec![LiveDispatch {
                dispatch: dispatch("legacy-1"),
                run: run("run-legacy"),
                pid: Some(Pid::new(4242)),
                session_id: Some("sess-old".into()),
                reserve_micros: 500,
                source: None,
                agent_id: None,
                parent_dispatch: None,
                depth: None,
            }],
            "session and reservation were already columns and read back; \
             source, parent and depth did not exist yet and read back absent"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // A row a pre-v13 binary wrote: `routed_by` was never a column it
    // could have filled in, so it reads back NULL forever — `None`, the
    // fact "nobody recorded how this was routed", never coerced into
    // `Some(RoutedBy::ConservativeBaseline)`, which would claim the
    // router ran and abstained when no such decision was ever captured.
    #[test]
    fn a_pre_migration_row_reports_its_routed_by_as_unrecorded() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-legacy"), "/repo", None, &task("run-legacy"), "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO dispatches
                    (dispatch_id, run_id, intent_json, state,
                     reserved_micros, created_at, updated_at)
                 VALUES ('legacy-1', 'run-legacy', '{}', 'intent', 0, 'now', 'now')",
                [],
            )
            .expect("legacy row");
        assert_eq!(
            ledger
                .first_dispatch_routed_by(&run("run-legacy"))
                .expect("routed_by"),
            None,
            "a pre-v13 row never wrote routed_by; it reads back absent, not baseline"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unparseable_routed_by_is_a_corrupt_row_not_a_default() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-bad"), "/repo", None, &task("run-bad"), "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO dispatches
                    (dispatch_id, run_id, intent_json, state,
                     reserved_micros, created_at, updated_at, routed_by)
                 VALUES ('bad-1', 'run-bad', '{}', 'intent', 0, 'now', 'now', 'not_a_route')",
                [],
            )
            .expect("bad row");
        let err = ledger
            .first_dispatch_routed_by(&run("run-bad"))
            .expect_err("unparseable routed_by must not read as a default");
        assert!(matches!(err, LedgerError::Corrupt { .. }), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The boundary that actually keeps a model out of a comparison: no
    /// CLI surface and no contract field names a trial's arm or the
    /// probability it was drawn with. A worker cannot call a Rust
    /// function at any visibility, so this — not `pub(crate)` — is the
    /// thing worth asserting, and it must still hold when assignment is
    /// written.
    #[test]
    fn nothing_a_model_can_reach_names_a_trials_arm() {
        // The same function the negative controls below run on fixture
        // trees, so what they prove holds for this check too.
        if let Err(e) = check_arm_boundary(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))) {
            panic!("{e}");
        }
    }

    /// The needle for the declaration, split so this file's own text does
    /// not hold it: the ledger tree is scanned for exactly one match.
    const REPLAY_TRIAL_DECL: &str = concat!("pub struct NewReplayTrial", "<'a> {");

    /// The CLI roots under `src`: `main.rs` and `cli/`. Both are required
    /// (`read_rs_tree` fails on a missing root), so a renamed CLI tree
    /// cannot pass unscanned.
    fn cli_roots(src: &std::path::Path) -> Vec<std::path::PathBuf> {
        vec![src.join("main.rs"), src.join("cli")]
    }

    /// Every `.rs` file at or under each root, as (path, text) pairs in
    /// path order. A missing root or a tree with no `.rs` file is an
    /// error naming `what`.
    fn read_rs_tree(
        src: &std::path::Path,
        roots: &[std::path::PathBuf],
        what: &str,
    ) -> std::result::Result<Vec<(String, String)>, String> {
        fn walk(p: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
            if p.is_dir() {
                for entry in std::fs::read_dir(p)? {
                    walk(&entry?.path(), out)?;
                }
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p.to_path_buf());
            }
            Ok(())
        }
        let mut files = Vec::new();
        for root in roots {
            if !root.exists() {
                return Err(format!(
                    "{what} is missing: {} does not exist",
                    root.display()
                ));
            }
            walk(root, &mut files).map_err(|e| format!("{what}: {}: {e}", root.display()))?;
        }
        if files.is_empty() {
            return Err(format!("{what} has no .rs file under {}", src.display()));
        }
        files.sort();
        files
            .into_iter()
            .map(|f| {
                let text = std::fs::read_to_string(&f)
                    .map_err(|e| format!("{what}: cannot read {}: {e}", f.display()))?;
                Ok((f.display().to_string(), text))
            })
            .collect()
    }

    /// No CLI file and no contract file names an identifier a caller must
    /// never choose.
    fn check_unreachable_identifiers(
        cli: &[(String, String)],
        contract: &[(String, String)],
    ) -> std::result::Result<(), String> {
        // The three a caller must never choose. An arm index and a draw
        // probability describe a SAMPLED comparison; a seed reproduces
        // one. A caller able to name any of them could describe a
        // comparison that never happened, which is the whole value of a
        // trial gone.
        //
        // `trial_id` and `arm_recipe_id` were on this list and are not
        // any more. That was over-broad, and only passed while nothing
        // recorded a trial from the CLI: `relais dataset replay` now
        // does, and both are DERIVED there — the id from
        // `IdSource::mint_replay_trial`, the recipe from the candidate
        // `validate_candidate` already admitted. Forbidding the
        // identifiers would have forced the legitimate path to obscure
        // itself. What replaces them is stricter, not looser: the
        // structural check below.
        for (what, files) in [("the CLI", cli), ("the task contract", contract)] {
            // `seed` is NOT scanned for: `main.rs` names the solver's
            // training seed (`settings.solver.seed`), which has nothing
            // to do with a trial, and a substring scan cannot tell the
            // two apart. The structural check below is what actually
            // keeps a caller from naming a trial's seed; these two
            // identifiers are scanned because they have no innocent
            // meaning anywhere in this crate.
            for (path, source) in files {
                for forbidden in ["arm_index", "assignment_probability"] {
                    if source.contains(forbidden) {
                        return Err(format!(
                            "{what} must not name `{forbidden}` (found in {path}): a trial's \
                             arm and the probability it was drawn with come from relais's own \
                             assignment, never from a flag a person types or a field a model \
                             writes"
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Structural, and the part the identifier scan could not make:
    /// the only shape a caller outside this module can hand to
    /// `insert_replay_trial` has no field for any of the three, so a
    /// replay cannot describe a draw even by accident. `insert_trial`
    /// takes `NewTrial`, which does carry them — and is reachable only
    /// from inside this module until assignment is written.
    fn check_replay_trial_declaration(
        ledger: &[(String, String)],
    ) -> std::result::Result<(), String> {
        let holders: Vec<&(String, String)> = ledger
            .iter()
            .filter(|(_, text)| text.contains(REPLAY_TRIAL_DECL))
            .collect();
        let [(path, text)] = holders.as_slice() else {
            return Err(format!(
                "NewReplayTrial must be declared in exactly one file under src/ledger/, found {}",
                holders.len()
            ));
        };
        let decl_start = text
            .find(REPLAY_TRIAL_DECL)
            .expect("it holds the declaration");
        let decl_end = decl_start
            + text[decl_start..]
                .find("\n}")
                .ok_or_else(|| format!("its declaration in {path} does not end"))?;
        let decl = &text[decl_start..decl_end];
        for absent in ["arm_index", "assignment_probability", "seed"] {
            if decl.contains(absent) {
                return Err(format!(
                    "NewReplayTrial must have no `{absent}` field, so no replay caller can name \
                     one. Declaration in {path}: {decl}"
                ));
            }
        }
        Ok(())
    }

    /// Writes `files` (relative path, text) under a fresh `src/` in a
    /// temp directory and returns it.
    fn fixture_src(label: &str, files: &[(&str, String)]) -> crate::test_support::TempDir {
        let dir = temp_dir(label);
        for (rel, text) in files {
            let p = dir.join("src").join(rel);
            std::fs::create_dir_all(p.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&p, text).expect("write");
        }
        dir
    }

    /// Both checks over the crate (or fixture) rooted at `dir`.
    fn check_arm_boundary(dir: &std::path::Path) -> std::result::Result<(), String> {
        let src = dir.join("src");
        let cli = read_rs_tree(&src, &cli_roots(&src), "the binary CLI tree")?;
        let contract = read_rs_tree(&src, &[src.join("contract")], "the contract tree")?;
        let ledger = read_rs_tree(&src, &[src.join("ledger")], "the ledger tree")?;
        check_unreachable_identifiers(&cli, &contract)?;
        check_replay_trial_declaration(&ledger)
    }

    fn clean_layout() -> Vec<(&'static str, String)> {
        vec![
            ("main.rs", "mod cli;\nfn main() {}\n".to_string()),
            ("cli/run.rs", "pub fn run() {}\n".to_string()),
            ("contract/mod.rs", "pub struct C;\n".to_string()),
            ("ledger/mod.rs", "mod trials;\n".to_string()),
            (
                "ledger/trials.rs",
                format!("{REPLAY_TRIAL_DECL}\n    pub id: u32,\n}}\n"),
            ),
        ]
    }

    fn with_file(
        mut layout: Vec<(&'static str, String)>,
        rel: &'static str,
        text: String,
    ) -> Vec<(&'static str, String)> {
        layout.retain(|(r, _)| *r != rel);
        layout.push((rel, text));
        layout
    }

    #[test]
    fn a_clean_extracted_layout_passes_the_arm_boundary_checks() {
        let dir = fixture_src("arm-clean", &clean_layout());
        assert_eq!(check_arm_boundary(&dir), Ok(()));
    }

    #[test]
    fn a_forbidden_identifier_in_an_extracted_cli_child_is_reported() {
        let layout = with_file(
            clean_layout(),
            "cli/run.rs",
            "let arm_index = 1;\n".to_string(),
        );
        let dir = fixture_src("arm-cli-child", &layout);
        let err = check_arm_boundary(&dir).expect_err("reported");
        assert!(err.contains("arm_index") && err.contains("run.rs"), "{err}");
    }

    #[test]
    fn a_forbidden_identifier_in_a_nested_cli_file_is_reported() {
        let layout = with_file(
            clean_layout(),
            "cli/learn/train.rs",
            "let arm_index = 1;\n".to_string(),
        );
        let dir = fixture_src("arm-cli-nested", &layout);
        let err = check_arm_boundary(&dir).expect_err("reported");
        assert!(
            err.contains("arm_index") && err.contains("train.rs"),
            "{err}"
        );
    }

    #[test]
    fn a_forbidden_identifier_in_a_contract_child_is_reported() {
        let layout = with_file(
            clean_layout(),
            "contract/child.rs",
            "pub assignment_probability: f64,\n".to_string(),
        );
        let dir = fixture_src("arm-contract-child", &layout);
        let err = check_arm_boundary(&dir).expect_err("reported");
        assert!(
            err.contains("assignment_probability") && err.contains("child.rs"),
            "{err}"
        );
    }

    #[test]
    fn a_seed_field_in_a_ledger_child_new_replay_trial_is_reported() {
        let layout = with_file(
            clean_layout(),
            "ledger/trials.rs",
            format!("{REPLAY_TRIAL_DECL}\n    pub seed: u64,\n}}\n"),
        );
        let dir = fixture_src("arm-seed", &layout);
        let err = check_arm_boundary(&dir).expect_err("reported");
        assert!(err.contains("`seed`") && err.contains("trials.rs"), "{err}");
    }

    #[test]
    fn a_ledger_tree_without_the_declaration_is_an_error() {
        let layout = with_file(
            clean_layout(),
            "ledger/trials.rs",
            "pub struct Other;\n".to_string(),
        );
        let dir = fixture_src("arm-missing-decl", &layout);
        let err = check_arm_boundary(&dir).expect_err("an error");
        assert!(err.contains("NewReplayTrial"), "{err}");
    }

    #[test]
    fn a_missing_cli_tree_is_an_error() {
        let layout: Vec<_> = clean_layout()
            .into_iter()
            .filter(|(r, _)| !r.starts_with("cli/"))
            .collect();
        let dir = fixture_src("arm-no-cli", &layout);
        let err = check_arm_boundary(&dir).expect_err("an error");
        assert!(err.contains("CLI tree"), "{err}");
    }

    #[test]
    fn a_missing_contract_tree_is_an_error() {
        let layout: Vec<_> = clean_layout()
            .into_iter()
            .filter(|(r, _)| !r.starts_with("contract/"))
            .collect();
        let dir = fixture_src("arm-no-contract", &layout);
        let err = check_arm_boundary(&dir).expect_err("an error");
        assert!(err.contains("contract tree"), "{err}");
    }

    /// An unparseable outcome, cost completeness or run purpose is a
    /// corrupt row the caller reports, never silently normalised into a
    /// default.
    #[test]
    fn unrecognised_outcome_completeness_and_purpose_are_corrupt_rows() {
        let (ledger, dir) = temp_ledger();
        let source_task = task("t-bad");
        let source_run = run("run-bad-trial");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO trials
                    (trial_id, task_id, source_run_id, incumbent_recipe_id,
                     arm_recipe_id, arm_index, assignment_probability, seed,
                     base_sha, contract_hash, verification_profile_hash,
                     workspace_isolation, outcome, cost_completeness, created_at)
                 VALUES ('trial-bad', ?1, ?2, 'inc', 'arm', 0, 0.5, 1,
                         'sha', 'ch', 'ph', 'worktree', 'not_an_outcome',
                         '\"not_a_completeness\"', 'now')",
                params![source_task.as_str(), source_run.as_str()],
            )
            .expect("bad row");
        let err = ledger
            .trials_by_task(&source_task)
            .expect_err("unparseable outcome must not read as a default");
        assert!(matches!(err, LedgerError::Corrupt { .. }), "{err}");

        ledger
            .conn
            .execute(
                "UPDATE trials SET outcome = NULL WHERE trial_id = 'trial-bad'",
                [],
            )
            .expect("clear outcome");
        let err = ledger
            .trials_by_task(&source_task)
            .expect_err("unparseable completeness must not read as a default");
        assert!(matches!(err, LedgerError::Corrupt { .. }), "{err}");

        ledger
            .conn
            .execute(
                "UPDATE runs SET purpose = 'not_a_purpose' WHERE id = ?1",
                [source_run.as_str()],
            )
            .expect("bad purpose");
        let err = ledger
            .run_purpose(&source_run)
            .expect_err("unparseable purpose must not read as ordinary work");
        assert!(matches!(err, LedgerError::Corrupt { .. }), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn receipts_are_stored_by_the_runner_only() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-r"), "/repo", None, &task("run-r"), "rk")
            .expect("run");
        assert!(ledger.receipt(&run("run-r")).expect("receipt").is_none());
        let receipt = serde_json::json!({"run_id": "run-r", "outcome": "accepted"});
        ledger
            .store_receipt(&run("run-r"), &receipt, "hash123")
            .expect("store");
        let (stored, hash) = ledger
            .receipt(&run("run-r"))
            .expect("receipt")
            .expect("present");
        assert_eq!(stored["outcome"], "accepted");
        assert_eq!(hash, "hash123");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A run the OS sandbox once handled leaves rows nothing writes any
    /// more: a `shape_refused` transition naming a retired block code, and
    /// a `sandbox_denials` evidence row. They still read back through the
    /// ledger API `status` and `explain` use, by their stored spellings
    /// (the manifest such a run stored is `context`'s test).
    #[test]
    fn a_run_with_sandbox_era_rows_still_reads_back() {
        use crate::policy::BlockCode;
        let (ledger, dir) = temp_ledger();
        let id = run("run-sandbox-era");
        ledger
            .insert_run(&id, "/repo", None, &task("run-sandbox-era"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: id.clone(),
                attempt_id: None,
                from_state: Some(State::Running),
                to_state: State::Repairing,
                reason: Reason::ShapeRefused.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        for code in [
            BlockCode::SandboxUnavailable,
            BlockCode::SandboxWeakened,
            BlockCode::SandboxUnverified,
        ] {
            ledger
                .record_transition(&Transition {
                    run_id: id.clone(),
                    attempt_id: None,
                    from_state: Some(State::Repairing),
                    to_state: State::Blocked,
                    reason: Reason::PermissionDenied.as_str().into(),
                    detail: Some(serde_json::json!({ "code": code.as_str() })),
                    at: now_rfc3339(),
                })
                .expect("transition");
            let stored = serde_json::to_string(&code).expect("a code serializes");
            let back: BlockCode = serde_json::from_str(&stored).expect("a code parses");
            assert_eq!(back, code, "{stored}");
        }
        ledger
            .record_evidence(
                &id,
                None,
                EvidenceKind::SandboxDenials,
                Path::new("/runs/r/attempts/sandbox-denials-1.json"),
                None,
            )
            .expect("evidence");
        assert_eq!(
            ledger.run_status(&id).expect("status"),
            Some(State::Blocked)
        );
        let transitions = ledger.transitions(&id).expect("transitions");
        assert!(transitions
            .iter()
            .any(|t| t.reason == "shape_refused" && t.to_state == State::Repairing));
        assert!(transitions.iter().any(|t| t
            .detail
            .as_ref()
            .is_some_and(|d| d["code"] == "sandbox_unverified")));
        assert_eq!(
            Reason::parse("shape_refused").expect("a stored reason"),
            Reason::ShapeRefused
        );
        let evidence = ledger.evidence(&id).expect("evidence");
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].kind, EvidenceKind::SandboxDenials);
        assert_eq!(evidence[0].kind.to_string(), "sandbox_denials");
        std::fs::remove_dir_all(&dir).ok();
    }
}
