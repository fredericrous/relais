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
mod learning;
mod outcomes;
mod review;
mod router;
mod runs;
mod schema;
mod sessions;
mod successor;
mod test_support;
mod transitions;
mod trials;
mod trust;
mod usage;
pub use admission::GroupBindingRow;
pub use clock::{now_rfc3339, Clock, FixedClock, SystemClock};
pub use error::LedgerError;
use error::{parse_evidence_kind, parse_state, parse_trial_row, Result};
pub use evidence::{EvidenceKind, EvidenceOrigin, EvidenceRow};
pub use learning::{
    LearningAction, LearningCycleFinish, LearningCycleRow, LearningSource, LearningVerdict, LiveRun,
};
pub use outcomes::{RevertCandidate, RevertedReport, StoredOutcome};
pub use runs::{
    ChildOf, ChildRun, ModelObservation, ModelPair, RunWorkspace, TaskOrigin, TaskRow,
    WorkerAttempt, EFFORT_NOT_REQUESTED, EFFORT_NO_CONTROL, EFFORT_UNKNOWN,
};
use schema::{applied_count, migrate};
pub use schema::{LEDGER_SCHEMA_VERSION, LEGACY_ACTORS};
pub use sessions::OrchestrationUsageRow;
pub use successor::{
    Claim, IntentState, NewIntent, SuccessorIntent, SuccessorProcess, CLAIM_GRACE_SECONDS,
};
pub use transitions::{
    Actor, Answerer, DecisionAnswer, DecisionRecord, PersonAnswerCounts, RecordedVia, RunDriver,
    Transition, ADVISOR_EVENT_PREFIX,
};
use trials::TrialColumns;
pub use trials::{
    GroundTruth, GroundTruthVerdict, JudgeRefusal, Judged, NewRecipePromotion, NewReplayTrial,
    NewTrial, RecipePromotion, TrialCost, TrialCostError, TrialOutcome, TrialRow, TrialUsage,
    LIVE_WORKTREE,
};
pub use trust::{StoredDeclaration, TrustGrantRecord};
pub use usage::{Booking, DispatchBooking, LiveDispatch, PhaseCost, UsageEvent};
mod task_types;
pub use review::{MergeTotals, ReviewClassTotals, ReviewOutcomeRow};
pub use router::{
    ExplorationTask, RouterCheck, RouterClass, RouterClassificationRow, RouterDecisionRow,
    RouterExplorationStop, RouterProvenance, RouterReassessRow, RouterRecord, RouterTaskOutcomeRow,
    RouterTaskRow, RouterUsageRow, RouterWindow,
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
    use super::schema::{apply_step, migrations::MIGRATIONS};
    use super::test_support::{
        dispatch, driver_of, event, run, task, temp_dir, temp_ledger, v26_ledger_with,
    };
    use super::*;
    use crate::ids::{Pid, RunId, TaskId, TrialId};
    use crate::lifecycle::{Reason, State, UsagePhase};
    use crate::money::{CostCompleteness, MicroUsd};
    use crate::outcome::{Outcome, OutcomeDetail, OutcomeKind};
    use crate::policy::Tier;
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

    /// The v6 migration's backfill names a state by its quoted, stored
    /// spelling; `State::awaits_a_person` is the single definition of
    /// which states those are. This is the converse of `lifecycle`'s own
    /// test: that one asserts what the enum method answers, this one
    /// asserts the migration names no state the method disagrees with,
    /// so the two cannot drift apart silently.
    #[test]
    fn the_v6_migration_names_exactly_the_states_that_await_a_person() {
        let (_, sql) = MIGRATIONS
            .iter()
            .find(|(version, _)| *version == "v6")
            .expect("v6 exists");
        for state in State::ALL {
            let quoted = format!("'{}'", state.as_str());
            assert_eq!(
                sql.contains(&quoted),
                state.awaits_a_person(),
                "state {state} disagrees with the migration text"
            );
        }
    }

    #[test]
    fn v3_turns_the_zeros_that_stood_for_unknown_into_null() {
        let dir = temp_dir("v3");
        let path = dir.join("ledger.sqlite");
        // A v2 ledger, as 0.1.1 wrote it: NOT NULL cost, unknown stored as 0.
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..2] {
                conn.execute_batch(sql).expect("v1/v2");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 'then')",
                    [version],
                )
                .expect("mark");
            }
            conn.execute_batch(
                r#"INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                   VALUES ('run-old', '/r', 'accepted', 't', 't');
                   INSERT INTO usage_events (event_id, run_id, cost_micros, cost_kind, completeness, at)
                   VALUES ('e-known', 'run-old', 500, '"api_spend"', '"actual"', 't'),
                          ('e-unknown', 'run-old', 0, '"api_spend"', '"unknown"', 't');"#,
            )
            .expect("old rows");
        }
        let ledger = Ledger::open(&path).expect("migrates");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let unknown: Option<i64> = ledger
            .conn
            .query_row(
                "SELECT cost_micros FROM usage_events WHERE event_id = 'e-unknown'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(unknown, None);
        let known: Option<i64> = ledger
            .conn
            .query_row(
                "SELECT cost_micros FROM usage_events WHERE event_id = 'e-known'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(known, Some(500), "reported cost survives the rebuild");
        assert_eq!(
            ledger.run_cost(&run("run-old")).expect("cost"),
            MicroUsd::from_micros(500)
        );
        // The index came back with the table.
        let indexed: i64 = ledger
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_usage_run'",
                [],
                |row| row.get(0),
            )
            .expect("index");
        assert_eq!(indexed, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1: a migration step is one transaction over its DDL batch and
    /// the row that records it. A failure inside the batch leaves the
    /// ledger exactly as it was — not half a v3 that every later open
    /// trips over with "table already exists".
    #[test]
    fn a_failed_migration_step_leaves_nothing_behind() {
        let dir = temp_dir("partial");
        let path = dir.join("ledger.sqlite");
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version TEXT PRIMARY KEY, applied_at TEXT NOT NULL)",
            [],
        )
        .expect("migrations table");
        // A step that creates a table and then fails, the shape of a
        // crash in the middle of the real v3 rebuild.
        let broken = "CREATE TABLE half_built (id INTEGER);
                      INSERT INTO nowhere_at_all (id) VALUES (1);";
        assert!(
            apply_step(&conn, "v99", broken, "now").is_err(),
            "the step must fail"
        );
        let built: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'half_built'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(built, 0, "the table the step created was rolled back");
        assert_eq!(
            applied_count(&conn).expect("count"),
            0,
            "and the step is not recorded as applied"
        );
        // The same step, fixed, still applies afterwards.
        apply_step(&conn, "v99", "CREATE TABLE half_built (id INTEGER);", "now")
            .expect("a working step applies");
        assert_eq!(applied_count(&conn).expect("count"), 1);
        std::fs::remove_dir_all(&dir).ok();
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

    #[test]
    fn migrations_are_idempotent_and_additive() {
        let (ledger, dir) = temp_ledger();
        assert_eq!(
            ledger.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION
        );
        drop(ledger);
        let reopened = Ledger::open(&dir.join("ledger.sqlite")).expect("reopen");
        assert_eq!(
            reopened.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION,
            "no double-apply"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The v15 migration only adds a new table and its indexes — nothing
    /// it does touches an existing row, so an upgrade from a pre-v15
    /// ledger keeps every row exactly as it stood before.
    #[test]
    fn upgrading_to_v15_keeps_every_existing_row() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-pre-v15"), "/repo", None, &task("pre"), "rk")
            .expect("run");
        drop(ledger);
        let reopened = Ledger::open(&dir.join("ledger.sqlite")).expect("reopen applies v15");
        assert_eq!(
            reopened.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION
        );
        let (id, _, status, _) = reopened
            .runs_since("2000-01-01T00:00:00Z")
            .expect("runs")
            .into_iter()
            .find(|(id, ..)| *id == run("run-pre-v15"))
            .expect("the pre-v15 row is still there");
        assert_eq!(id, run("run-pre-v15"));
        assert_eq!(status, State::Prepared.as_str());
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
    fn a_ledger_from_a_newer_relais_is_refused() {
        let (ledger, dir) = temp_ledger();
        drop(ledger);
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("raw open");
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES ('v99', 'then')",
                [],
            )
            .expect("a migration this relais does not ship");
        }
        match Ledger::open(&path) {
            Err(LedgerError::SchemaAhead { found, known }) => {
                assert_eq!(
                    (found, known),
                    (LEDGER_SCHEMA_VERSION + 1, LEDGER_SCHEMA_VERSION)
                );
                let message = LedgerError::SchemaAhead { found, known }.to_string();
                assert!(message.contains(&found.to_string()), "{message}");
                assert!(message.contains(&known.to_string()), "{message}");
            }
            other => panic!(
                "a newer ledger must be refused, got {:?}",
                other.map(|_| ())
            ),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v1_ledger_upgrades_additively_and_keeps_its_rows() {
        let (ledger, dir) = temp_ledger();
        drop(ledger);
        let path = dir.join("ledger.sqlite");
        // Rewind to v1: drop the v2 columns' migration record and the
        // columns themselves, as a ledger written by an older relais
        // would look.
        {
            let conn = Connection::open(&path).expect("raw open");
            conn.execute_batch(
                "DROP INDEX idx_runs_parent;
                 ALTER TABLE runs DROP COLUMN parent_run;
                 ALTER TABLE runs DROP COLUMN package_id;
                 DELETE FROM schema_migrations WHERE version = 'v2';
                 INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('old-run', '/r', 'accepted', '2026-01-01T00:00:00+00:00',
                         '2026-01-01T00:00:00+00:00');",
            )
            .expect("rewind");
        }
        let upgraded = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            upgraded.schema_version().expect("count"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            upgraded.run_status(&run("old-run")).expect("status"),
            Some(State::Accepted),
            "existing rows survive the additive migration"
        );
        assert!(upgraded
            .child_runs(&run("old-run"))
            .expect("children")
            .is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A v3 ledger — the shape every relais installed before the task
    /// spine wrote — upgrades to v4 keeping every row, every run's cost,
    /// AND gets a `task-legacy-<root run id>` task backfilled onto its
    /// whole tree: the root and the package it decomposed into.
    /// A ledger already migrated to v4 — every machine that ran the
    /// task-spine release — gains the outcome columns from v5. The
    /// columns first landed as an EDIT to the applied v4 step, where
    /// `apply_step` skips them, so they existed only in tests, which
    /// always start from nothing (found in review of
    /// run-65c1620e17aec-1000062a1).
    /// A ledger already at v6 — every machine that ran `relais decide`
    /// before this step — has its open decisions repointed at the
    /// transition that RAISED them. v6 backfilled from the run's last
    /// transition, which for a retired run is the `worktree_retired`
    /// row: `approve` reads the transition `raised_at` names, so it
    /// would have found no gaps on a run that has them.
    #[test]
    fn a_v6_backfilled_decision_is_repointed_at_its_raising_transition() {
        let dir = crate::test_support::temp_dir("ledger-v6-to-v7");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v6 exactly as it shipped: every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            conn.execute_batch(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-w', '/repo', 'needs_decision', '2026-01-01T00:00:00+00:00',
                         '2026-01-01T00:02:00+00:00');",
            )
            .ok();
            // Pinned to v6, not `MIGRATIONS.len() - 1`: a step appended
            // after v7 must not silently move this fixture past the
            // v6-to-v7 boundary this test exists to prove.
            for (version, sql) in &MIGRATIONS[..6] {
                conn.execute_batch(sql).expect("step");
                if version == &"v1" {
                    // The run, then the wait, then the retirement that
                    // follows every real wait.
                    conn.execute_batch(
                        "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                         VALUES ('run-w', '/repo', 'needs_decision',
                                 '2026-01-01T00:00:00+00:00', '2026-01-01T00:02:00+00:00');
                         INSERT INTO transitions (run_id, from_state, to_state, reason, detail_json, at)
                         VALUES ('run-w', 'verifying', 'needs_decision', 'verification_gap',
                                 '{\"gaps\":[\"declared-check: status `inert`\"]}',
                                 '2026-01-01T00:01:00+00:00');
                         INSERT INTO transitions (run_id, from_state, to_state, reason, detail_json, at)
                         VALUES ('run-w', 'needs_decision', 'needs_decision', 'worktree_retired',
                                 '{\"reference\":\"refs/relais/candidates/run-w/final\"}',
                                 '2026-01-01T00:02:00+00:00');",
                    )
                    .expect("fixture rows");
                }
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-01-01T00:00:00+00:00"],
                )
                .expect("record");
            }
            let raised: String = conn
                .query_row(
                    "SELECT raised_reason FROM decisions WHERE run = 'run-w'",
                    [],
                    |row| row.get(0),
                )
                .expect("v6 backfilled a row");
            assert_eq!(
                raised, "worktree_retired",
                "v6 read the run's LAST transition — the defect this step corrects"
            );
        }

        let ledger = Ledger::open(&path).expect("upgrade to v7");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let decision = ledger
            .decision_of_run(&RunId::from_stored("run-w"))
            .expect("query")
            .expect("still open");
        assert_eq!(
            decision.raised_reason, "verification_gap",
            "the row now names the transition that RAISED the wait"
        );
        assert_eq!(decision.raised_at, "2026-01-01T00:01:00+00:00");
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v4_ledger_gains_the_outcome_columns() {
        let dir = crate::test_support::temp_dir("ledger-v4-to-v5");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v4 exactly as it shipped: apply every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Pinned to v4, not `MIGRATIONS.len() - 1`: a step appended
            // after v5 must not silently move this fixture past the
            // v4-to-v5 boundary this test exists to prove.
            for (version, sql) in &MIGRATIONS[..4] {
                conn.execute_batch(sql).expect("step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-09-22T00:00:00+00:00"],
                )
                .expect("record");
            }
        }
        let ledger = Ledger::open(&path).expect("upgrade to v5");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        // The proof: a write that needs the new columns succeeds on a
        // ledger that was already v4.
        let columns: Vec<String> = ledger
            .conn
            .prepare("SELECT name FROM pragma_table_info('outcomes')")
            .expect("pragma")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("rows");
        assert!(
            columns.iter().any(|c| c == "task_id"),
            "v5 adds task_id to an already-v4 ledger: {columns:?}"
        );
        assert!(
            columns.iter().any(|c| c == "candidate_sha"),
            "and candidate_sha: {columns:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger pinned at exactly v9 — every machine that ran the
    /// human-signoff release and no later one — upgrades to v10 keeping
    /// its outcome rows, and can then record one with no candidate: the
    /// case `relais feedback` gained for a person-approved run that
    /// never wrote a receipt.
    #[test]
    fn a_v9_ledger_upgrades_to_v10_and_accepts_an_outcome_with_no_candidate() {
        let dir = crate::test_support::temp_dir("ledger-v9-to-v10");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v9 exactly as it shipped: apply every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..9] {
                conn.execute_batch(sql).expect("step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-09-22T00:00:00+00:00"],
                )
                .expect("record");
            }
            conn.execute(
                "INSERT INTO outcomes (run_id, task_id, candidate_sha, kind, detail_json, at)
                 VALUES ('run-old', 'task-old', 'sha-old', 'accepted_unchanged', '{}', ?1)",
                params!["2026-09-22T00:00:00+00:00"],
            )
            .expect("a row written before v10");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v10");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        // The pre-existing row survived the rebuild.
        let survived: String = ledger
            .conn
            .query_row(
                "SELECT candidate_sha FROM outcomes WHERE run_id = 'run-old'",
                [],
                |row| row.get(0),
            )
            .expect("row read back");
        assert_eq!(survived, "sha-old");
        // The proof: a run accepted by a person, with no receipt, can
        // record an outcome that names no candidate.
        ledger
            .insert_run(
                &RunId::from_stored("run-a"),
                "/repo",
                None,
                &TaskId::from_stored("task-a"),
                "rk",
            )
            .expect("run");
        let outcome = Outcome::new(
            OutcomeKind::AcceptedUnchanged,
            OutcomeDetail {
                candidate_sha: None,
                strategy: crate::outcome::Strategy {
                    tier: Tier::Implementation,
                    models: vec!["sonnet".into()],
                    escalated: false,
                },
                correction_magnitude: None,
                evidence: vec![],
                actor: "a person".into(),
                note: None,
                source: None,
            },
        )
        .expect("valid outcome");
        ledger
            .record_outcome(
                &RunId::from_stored("run-a"),
                &TaskId::from_stored("task-a"),
                &outcome,
            )
            .expect("an outcome with no candidate records");
        let latest = ledger
            .latest_outcome(&TaskId::from_stored("task-a"))
            .expect("latest")
            .expect("recorded");
        assert_eq!(latest.outcome.detail.candidate_sha, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v10_ledger_upgrades_to_v11_keeping_every_evidence_row_readable() {
        let dir = crate::test_support::temp_dir("ledger-v10-to-v11");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        {
            // v10 exactly as it shipped: apply every step but the last.
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..10] {
                conn.execute_batch(sql).expect("step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "2026-09-22T00:00:00+00:00"],
                )
                .expect("record");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted',
                         '2026-09-22T00:00:00+00:00', '2026-09-22T00:00:00+00:00')",
                [],
            )
            .expect("a run written before v11");
            // v10's evidence row shape: no tool/external_id/subject/
            // criterion_id columns exist yet.
            conn.execute(
                "INSERT INTO evidence (run_id, attempt_id, kind, path, sha256, at)
                 VALUES ('run-old', NULL, 'check_log', '/tmp/old.log', 'deadbeef',
                         '2026-09-22T00:00:00+00:00')",
                [],
            )
            .expect("a row written before v11");
        }
        let ledger = Ledger::open(&path).expect("upgrade to v11");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let evidence = ledger.evidence(&run("run-old")).expect("evidence reads");
        assert_eq!(
            evidence,
            vec![EvidenceRow {
                kind: EvidenceKind::CheckLog,
                path: "/tmp/old.log".into(),
                sha256: Some("deadbeef".into()),
                tool: None,
                external_id: None,
                subject: None,
                criterion_id: None,
            }],
            "a pre-v11 row reads back under its typed kind, the new columns absent"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v3_ledger_upgrades_to_v4_keeping_every_row_and_cost() {
        let dir = temp_dir("v3-to-v4");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE schema_migrations (version TEXT PRIMARY KEY, applied_at TEXT NOT NULL);",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..3] {
                conn.execute_batch(sql).expect("v1/v2/v3");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 'then')",
                    [version],
                )
                .expect("mark");
            }
            conn.execute_batch(
                r#"INSERT INTO runs (id, repo_path, status, created_at, updated_at, parent_run, package_id)
                   VALUES ('root-old', '/r', 'accepted', 't1', 't1', NULL, NULL),
                          ('pkg-old', '/r', 'accepted', 't2', 't2', 'root-old', 'a');
                   INSERT INTO usage_events (event_id, run_id, cost_micros, cost_kind, completeness, at)
                   VALUES ('e-root', 'root-old', 300, '"api_spend"', '"actual"', 't1'),
                          ('e-pkg', 'pkg-old', 700, '"api_spend"', '"actual"', 't2');"#,
            )
            .expect("v3 rows");
        }
        let root_cost_before = MicroUsd::from_micros(1000);
        let ledger = Ledger::open(&path).expect("upgrades to v4");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger.run_status(&run("root-old")).expect("status"),
            Some(State::Accepted)
        );
        assert_eq!(
            ledger.run_cost(&run("root-old")).expect("cost"),
            root_cost_before,
            "every run's cost survives the upgrade"
        );
        let root_task = ledger
            .task_of_run(&run("root-old"))
            .expect("task")
            .expect("backfilled");
        assert_eq!(root_task.as_str(), "task-legacy-root-old");
        assert_eq!(
            ledger.task_of_run(&run("pkg-old")).expect("task"),
            Some(root_task.clone()),
            "the package inherits its root's backfilled task"
        );
        let mut runs = ledger.runs_of_task(&root_task).expect("runs of task");
        runs.sort();
        assert_eq!(runs, vec![run("pkg-old"), run("root-old")]);
        let tasks = ledger
            .tasks_since("2000-01-01T00:00:00+00:00")
            .expect("tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, root_task);
        assert_eq!(tasks[0].origin, TaskOrigin::Backfilled);
        assert_eq!(tasks[0].first_run, run("root-old"));
        // A migration cannot compute a repository key, so a backfilled
        // task says what it actually has. `legacy:` is unmistakably not
        // a key, so grouping by `repo_key` never merges a legacy task
        // into a repository's real bucket on the strength of a path.
        assert!(
            tasks[0].repo_key.starts_with("legacy:"),
            "a backfilled task names what it has: {}",
            tasks[0].repo_key
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

    /// A ledger frozen at exactly v12 upgrades to v13 without losing any
    /// existing row, and the new column is readable on it.
    #[test]
    fn v13_adds_routed_by_without_touching_existing_rows() {
        let dir = temp_dir("v12-to-v13");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Pinned to v12, not `MIGRATIONS.len() - 1`: a step appended
            // after this test is written must not silently change what
            // "at v12" means here.
            for (version, sql) in &MIGRATIONS[..12] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written before v13");
            conn.execute(
                "INSERT INTO dispatches
                    (dispatch_id, run_id, intent_json, state,
                     reserved_micros, created_at, updated_at)
                 VALUES ('pre-v13', 'run-old', '{}', 'intent', 0, 'now', 'now')",
                [],
            )
            .expect("pre-v13 row");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger
                .first_dispatch_routed_by(&run("run-old"))
                .expect("routed_by"),
            None,
            "the upgrade adds the column; it does not retroactively fill it in"
        );
        assert_eq!(
            ledger
                .first_dispatch_intent(&run("run-old"))
                .expect("intent")
                .expect("row survives the upgrade"),
            serde_json::json!({}),
            "the pre-existing row is preserved, not dropped or rewritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// v0.26.0 ledgers stop at main's v38 (`landing_searches`). The
    /// admission steps were written as v37/v38 on their branch and renamed
    /// v39/v40 when it was rebased; a released ledger must take them on
    /// top, keep what it had, and end at the current version.
    #[test]
    fn a_v38_ledger_takes_the_admission_steps_on_top() {
        let dir = temp_dir("v38-to-v40");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            assert_eq!(MIGRATIONS[37].0, "v38", "pinned to main's v38");
            for (version, sql) in &MIGRATIONS[..38] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written at v38");
            conn.execute(
                "INSERT INTO landing_searches (run_id, searched_to, searched_at)
                 VALUES ('run-old', 'abc', 'now')",
                [],
            )
            .expect("a v38 row");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        assert_eq!(ledger.schema_version().expect("version"), 40);
        assert_eq!(LEDGER_SCHEMA_VERSION, 40);
        let tables: Vec<String> = ledger
            .conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                 AND name IN ('group_bindings', 'run_epochs', 'check_disk_growth',
                              'landing_searches')
                 ORDER BY name",
            )
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<_, _>>()
            .expect("names");
        assert_eq!(
            tables,
            [
                "check_disk_growth",
                "group_bindings",
                "landing_searches",
                "run_epochs"
            ]
        );
        let kept: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM landing_searches", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(kept, 1, "the v38 row survives the upgrade");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at exactly v13 upgrades to v14 without losing any
    /// existing row: the `trials` table, `runs.purpose` and
    /// `dispatches.trial_id` all appear, and every pre-existing row
    /// reads back as ordinary work with no trial id — never guessed
    /// into either.
    #[test]
    fn v14_adds_trials_purpose_and_trial_id_without_touching_existing_rows() {
        let dir = temp_dir("v13-to-v14");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Pinned to v13, not `MIGRATIONS.len() - 1`: a step appended
            // after this test is written must not silently change what
            // "at v13" means here.
            for (version, sql) in &MIGRATIONS[..13] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/repo', 'accepted', 'now', 'now')",
                [],
            )
            .expect("a run written before v14");
            conn.execute(
                "INSERT INTO dispatches
                    (dispatch_id, run_id, intent_json, state,
                     reserved_micros, created_at, updated_at)
                 VALUES ('pre-v14', 'run-old', '{}', 'intent', 0, 'now', 'now')",
                [],
            )
            .expect("pre-v14 row");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger.trials_by_task(&task("nonexistent")).expect("trials"),
            vec![],
            "the trials table exists and is empty on a fresh upgrade"
        );
        assert_eq!(
            ledger.run_purpose(&run("run-old")).expect("purpose"),
            None,
            "the upgrade adds the column; it does not retroactively fill it in"
        );
        assert_eq!(
            ledger
                .first_dispatch_trial_id(&run("run-old"))
                .expect("trial_id"),
            None
        );
        assert_eq!(
            ledger
                .first_dispatch_intent(&run("run-old"))
                .expect("intent")
                .expect("row survives the upgrade"),
            serde_json::json!({}),
            "the pre-existing row is preserved, not dropped or rewritten"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at exactly v16 upgrades to v17 keeping every trial
    /// row: `arm_run_id` appears and reads `None` on the rows written
    /// before it, never filled in by a guess.
    #[test]
    fn v17_adds_arm_run_id_and_keeps_every_existing_trial() {
        let dir = temp_dir("v16-to-v17");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..16] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO trials
                    (trial_id, task_id, source_run_id, incumbent_recipe_id,
                     arm_recipe_id, arm_index, assignment_probability, seed,
                     base_sha, contract_hash, verification_profile_hash,
                     workspace_isolation, created_at)
                 VALUES ('trial-old', 'task-old', 'run-old', 'inc', 'arm', 0, 1.0, 0,
                         'sha', 'ch', 'ph', 'worktree', 'now')",
                [],
            )
            .expect("a trial written before v17");
        }
        let ledger = Ledger::open(&path).expect("upgrade to current schema");
        let rows = ledger.trials_by_task(&task("old")).expect("trials");
        assert_eq!(rows.len(), 1, "the pre-existing trial survives");
        assert_eq!(rows[0].source_run_id, run("run-old"));
        assert_eq!(rows[0].arm_run_id, None);
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

    /// A ledger frozen at v26 upgrades to v28: the three driver columns
    /// appear, NULL on the row written before them.
    #[test]
    fn v28_adds_the_driver_columns_to_an_older_ledger() {
        let dir = temp_dir("v26-to-v28");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            // Up to v26: v27 (the typed actor) and v28 run in `open`.
            for (version, sql) in &MIGRATIONS[..26] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/r', 'prepared', 'then', 'then')",
                [],
            )
            .expect("a run written before v28");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(driver_of(&ledger, "run-old"), (None, None, None));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// ADR-0007: the step that adds the repository grant's records first
    /// writes a copy of the ledger beside it (what a rollback restores),
    /// then applies; a run records the grant that admitted it, which
    /// `legacy_grant_runs_since` counts; a repository's declaration is an
    /// upsert.
    #[test]
    fn v30_copies_the_ledger_first_then_records_trust_status_and_declarations() {
        let dir = temp_dir("v29-to-v30");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..29] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO runs (id, repo_path, status, created_at, updated_at)
                 VALUES ('run-old', '/r', 'prepared', '2020-01-01T00:00:00Z', 'then')",
                [],
            )
            .expect("a run written before v30");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );

        let copy = format!("{}.bak-v30", path.display());
        let before = Connection::open(&copy).expect("the copy opens as a ledger");
        assert_eq!(
            applied_count(&before).expect("count"),
            29,
            "as it was before"
        );
        let old_runs: i64 = before
            .query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))
            .expect("the copy holds the run");
        assert_eq!(old_runs, 1);
        assert!(
            before.prepare("SELECT trust_status FROM runs").is_err(),
            "the copy is from before the step"
        );

        // The status a run records, and the count doctor reads.
        let old = RunId::from_stored("run-old".to_string());
        ledger
            .set_run_trust_status(&old, "legacy_exact")
            .expect("status");
        assert_eq!(
            ledger
                .legacy_grant_runs_since("2019-01-01T00:00:00Z")
                .expect("count"),
            1
        );
        assert_eq!(
            ledger
                .legacy_grant_runs_since("2021-01-01T00:00:00Z")
                .expect("count"),
            0,
            "a run older than the window is not counted"
        );
        ledger
            .set_run_trust_status(&old, "repository")
            .expect("status");
        assert_eq!(
            ledger
                .legacy_grant_runs_since("2019-01-01T00:00:00Z")
                .expect("count"),
            0
        );

        // A repository's declaration is upserted, one row per repository.
        assert_eq!(ledger.repo_declaration("rk").expect("read"), None);
        ledger
            .upsert_repo_declaration("rk", "h1", "{}")
            .expect("first");
        ledger
            .upsert_repo_declaration("rk", "h2", "{\"a\":1}")
            .expect("second");
        let stored = ledger.repo_declaration("rk").expect("read").expect("a row");
        assert_eq!(stored.declaration_hash, "h2");
        assert_eq!(stored.declaration_json, "{\"a\":1}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at v26 upgrades to v29: a task written before it
    /// has no type, never a guessed one, and a trial no design.
    #[test]
    fn v29_adds_task_types_and_the_trial_design_without_touching_old_rows() {
        let dir = temp_dir("v26-to-v29");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..26] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO tasks (task_id, repo_key, first_run, origin, created_at)
                 VALUES ('task-old', 'rk', 'run-old', 'created', 'then')",
                [],
            )
            .expect("a task written before v29");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        assert_eq!(
            ledger
                .task_type_of(&task("old"))
                .expect("reads the new columns"),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A ledger frozen at v31 upgrades to v32: a trial written before it
    /// keeps every column and reads back unjudged, and, like the other
    /// additive steps, the step leaves no backup copy.
    #[test]
    fn v32_adds_the_ground_truth_columns_and_keeps_every_trial() {
        let dir = temp_dir("v31-to-v32");
        let path = dir.join("ledger.sqlite");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version TEXT PRIMARY KEY,
                    applied_at TEXT NOT NULL
                );",
            )
            .expect("migrations table");
            for (version, sql) in &MIGRATIONS[..31] {
                conn.execute_batch(sql).expect("apply step");
                conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, "then"],
                )
                .expect("record step");
            }
            conn.execute(
                "INSERT INTO trials (trial_id, task_id, source_run_id, incumbent_recipe_id,
                    arm_recipe_id, arm_index, assignment_probability, seed, base_sha,
                    contract_hash, verification_profile_hash, workspace_isolation,
                    outcome, accepted_without_escalation, created_at)
                 VALUES ('trial-old', 'task-old', 'run-old', 'inc', 'arm', 0, 1.0, 1, 'b',
                    'c', 'p', 'w', 'rejected', 0, 'then')",
                [],
            )
            .expect("a trial written before v32");
        }
        let ledger = Ledger::open(&path).expect("upgrade");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        let old = ledger
            .trial(&TrialId::from_stored("trial-old"))
            .expect("reads")
            .expect("the old trial survives");
        assert_eq!(old.outcome, Some(TrialOutcome::Rejected));
        assert_eq!(old.arm_recipe_id, "arm");
        assert_eq!(old.ground_truth, None);
        assert!(
            !dir.join("ledger.sqlite.bak-v32").exists(),
            "an additive step takes no backup, like v29 and v31"
        );
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

    #[test]
    fn the_actor_step_maps_every_legacy_spelling_keeps_the_original_and_backs_up() {
        let dir = crate::test_support::temp_dir("ledger-v26-to-v27");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        let spellings: Vec<&str> = LEGACY_ACTORS.iter().map(|(old, _)| *old).collect();
        assert_eq!(spellings.len(), 7, "the seven spellings of the live ledger");
        v26_ledger_with(&path, &spellings);
        let ledger = Ledger::open(&path).expect("upgrade to v27");
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        for (i, (old, expected)) in LEGACY_ACTORS.iter().enumerate() {
            for (table, key) in [("decisions", "run"), ("human_signoffs", "run_id")] {
                let (actor, raw, via): (String, String, String) = ledger
                    .conn
                    .query_row(
                        &format!(
                            "SELECT actor, actor_raw, recorded_via FROM {table} WHERE {key} = ?1"
                        ),
                        [format!("run-{i}")],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .expect("row");
                assert_eq!(actor, expected.as_str(), "{table}: {old}");
                assert_eq!(raw, *old, "{table}: the original is kept");
                assert_eq!(via, "legacy", "{table}");
            }
        }
        let nulls: i64 = ledger
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM decisions WHERE recorded_via IS NULL)
                      + (SELECT COUNT(*) FROM human_signoffs WHERE recorded_via IS NULL)",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(nulls, 0, "every existing row is stamped legacy");
        let open_actor: Option<String> = ledger
            .conn
            .query_row(
                "SELECT actor FROM decisions WHERE run = 'run-open'",
                [],
                |row| row.get(0),
            )
            .expect("open row");
        assert_eq!(open_actor, None, "an open decision still has no actor");
        let backup = dir.join("ledger.sqlite.bak-v27");
        assert!(backup.is_file(), "the VACUUM INTO backup is written");
        let old = Connection::open(&backup).expect("backup opens");
        let kept: String = old
            .query_row(
                "SELECT actor FROM decisions WHERE run = 'run-0'",
                [],
                |row| row.get(0),
            )
            .expect("backup row");
        assert_eq!(
            kept, "claude",
            "the backup holds the pre-migration spelling"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_actor_step_refuses_an_unmapped_spelling_by_name_and_changes_nothing() {
        let dir = crate::test_support::temp_dir("ledger-v26-unmapped");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        v26_ledger_with(&path, &["claude", "a stranger"]);
        let refused = match Ledger::open(&path) {
            Ok(_) => panic!("an unmapped spelling must refuse"),
            Err(e) => e.to_string(),
        };
        assert!(refused.contains("a stranger"), "{refused}");
        assert!(
            !dir.join("ledger.sqlite.bak-v27").exists(),
            "no backup for a refusal"
        );
        let conn = Connection::open(&path).expect("open");
        let applied: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(applied, 26, "the ledger stays at v26");
        std::fs::remove_dir_all(&dir).ok();
    }
}
