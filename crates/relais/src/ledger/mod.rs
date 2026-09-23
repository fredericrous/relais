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

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

use crate::contract::TaskContract;
use crate::ids::{DispatchId, PackageId, Pid, RunId, TaskId};
use crate::lifecycle::{Reason, State, UsagePhase};
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::outcome::{Outcome, OutcomeDetail, OutcomeKind};
use crate::policy::Tier;

pub const LEDGER_SCHEMA_VERSION: u64 = 8;

#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// A stored row this binary cannot read: an unknown state string, a
    /// receipt that is not JSON. A ledger is external state — truncated,
    /// hand-edited or written by another version — so reading one is a
    /// fallible operation, never an assertion about what SQLite holds.
    Corrupt {
        what: String,
        detail: String,
    },
    /// The ledger carries more applied migrations than this binary knows:
    /// a newer relais wrote it, and writing it back could lose what that
    /// version recorded.
    SchemaAhead {
        found: u64,
        known: u64,
    },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Sqlite(e) => write!(f, "ledger: {e}"),
            LedgerError::Io(e) => write!(f, "ledger: {e}"),
            LedgerError::Corrupt { what, detail } => {
                write!(f, "ledger: {what} cannot be read: {detail}")
            }
            LedgerError::SchemaAhead { found, known } => write!(
                f,
                "ledger: {found} applied migration(s), this relais knows {known} — \
                 a newer relais wrote this ledger; upgrade relais or point \
                 RELAIS_STATE_DIR at another one"
            ),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<rusqlite::Error> for LedgerError {
    fn from(e: rusqlite::Error) -> Self {
        LedgerError::Sqlite(e)
    }
}

impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e)
    }
}

type Result<T> = std::result::Result<T, LedgerError>;

/// A state string as the ledger stored it. An unrecognised one is a
/// corrupt row — a value from a newer relais, a truncated write, a hand
/// edit — and the caller is told, rather than the state silently reading
/// as "no such run" or the process aborting mid-report.
fn parse_state(stored: &str) -> Result<State> {
    State::parse(stored).map_err(|unknown| LedgerError::Corrupt {
        what: unknown.what.into(),
        detail: unknown.to_string(),
    })
}

/// A tier string as an attempt row stored it.
fn parse_tier(stored: &str) -> Result<Tier> {
    Tier::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "attempt tier".into(),
        detail: format!("`{stored}` is not a tier this relais knows"),
    })
}

/// A typed outcome as `latest_outcome`/`outcomes_since` hand it back:
/// the row's identity plus the [`Outcome`] it parses to.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOutcome {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub outcome: Outcome,
    pub at: String,
}

/// Raw columns of one `outcomes` row: run_id, task_id, kind, detail_json, at.
type OutcomeRow = (String, Option<String>, String, Option<String>, String);

/// An `outcomes` row as columns, parsed into a [`StoredOutcome`] (P6): an
/// unknown kind, a missing task id or a detail that will not parse as
/// [`OutcomeDetail`] is a `Corrupt` row the caller reports, never a
/// silently dropped or defaulted outcome.
fn parse_outcome_row(
    run_id: String,
    task_id: Option<String>,
    kind: String,
    detail_json: Option<String>,
    at: String,
) -> Result<StoredOutcome> {
    let kind = OutcomeKind::parse(&kind).ok_or_else(|| LedgerError::Corrupt {
        what: format!("the outcome kind of run {run_id}"),
        detail: format!("`{kind}` is not an outcome kind this relais knows"),
    })?;
    let task_id = task_id.ok_or_else(|| LedgerError::Corrupt {
        what: format!("the outcome of run {run_id}"),
        detail: "no task_id was recorded".into(),
    })?;
    let detail_json = detail_json.ok_or_else(|| LedgerError::Corrupt {
        what: format!("the outcome of run {run_id}"),
        detail: "no detail was recorded".into(),
    })?;
    let detail: OutcomeDetail =
        serde_json::from_str(&detail_json).map_err(|e| LedgerError::Corrupt {
            what: format!("the outcome detail of run {run_id}"),
            detail: e.to_string(),
        })?;
    Ok(StoredOutcome {
        run_id: RunId::from_stored(run_id),
        task_id: TaskId::from_stored(task_id),
        outcome: Outcome { kind, detail },
        at,
    })
}

/// How a task's row came to exist: minted for a fresh task, or
/// reconstructed for a run that predates the task spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOrigin {
    /// Minted by [`crate::ids::derive_task_id`] when a run dispatched.
    Created,
    /// Reconstructed by the v4 migration for a run written before tasks
    /// existed: `task-legacy-<root run id>`, one per pre-existing tree.
    Backfilled,
}

impl TaskOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Backfilled => "backfilled",
        }
    }

    fn parse(stored: &str) -> Option<Self> {
        match stored {
            "created" => Some(Self::Created),
            "backfilled" => Some(Self::Backfilled),
            _ => None,
        }
    }
}

/// A task's row as the ledger holds it (SPEC's task spine): the stable
/// identity a run's cost is attributed to, across contract revisions and
/// re-runs. `origin` leaves the adapter typed (P6/P8): a value no version
/// of this binary wrote is a corrupt row, not a silently accepted string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub task_id: TaskId,
    pub repo_key: String,
    pub first_run: RunId,
    pub origin: TaskOrigin,
    pub created_at: String,
}

/// A task origin string as the ledger stored it.
fn parse_task_origin(stored: &str) -> Result<TaskOrigin> {
    TaskOrigin::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "task origin".into(),
        detail: format!("`{stored}` is not a task origin this relais knows"),
    })
}

/// Where the ledger's timestamps come from. The wall clock in
/// production; a fixed or scripted clock in tests, so a transition's
/// `at`, a dispatch's `created_at` and the dataset's temporal splits are
/// assertable values rather than "whenever the test ran".
pub trait Clock: Send + Sync {
    fn now_rfc3339(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_rfc3339(&self) -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

/// A clock that answers a scripted sequence and then repeats its last
/// value: `FixedClock::new(["2026-09-18T10:00:00+00:00", …])`.
pub struct FixedClock {
    times: std::sync::Mutex<(Vec<String>, usize)>,
}

impl FixedClock {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(times: I) -> Self {
        let times: Vec<String> = times.into_iter().map(Into::into).collect();
        assert!(!times.is_empty(), "a fixed clock needs at least one time");
        Self {
            times: std::sync::Mutex::new((times, 0)),
        }
    }
}

impl Clock for FixedClock {
    fn now_rfc3339(&self) -> String {
        // A poisoned clock is still a list of times: the panic that
        // poisoned it is the caller's to report, not this clock's to
        // re-raise on every later read (the coordinator's `lock_state`
        // recovers its admission lock the same way).
        let mut guard = self
            .times
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (times, index) = &mut *guard;
        let now = times[(*index).min(times.len() - 1)].clone();
        *index += 1;
        now
    }
}

/// The wall clock, for callers that stamp outside the ledger (the CLI's
/// `report --since` default, dataset build time).
pub fn now_rfc3339() -> String {
    SystemClock.now_rfc3339()
}

/// Additive migrations, in order. Existing steps are never edited; a new
/// step appends. `schema_migrations` records what applied.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "v1",
        r#"
    CREATE TABLE runs (
        id TEXT PRIMARY KEY,
        repo_path TEXT NOT NULL,
        status TEXT NOT NULL,
        root_session TEXT,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE contract_revisions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        hash TEXT NOT NULL,
        contract_json TEXT NOT NULL,
        base_ref TEXT NOT NULL,
        base_sha TEXT,
        created_at TEXT NOT NULL,
        superseded_by INTEGER
    );
    CREATE TABLE attempts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        revision_id INTEGER NOT NULL,
        attempt_index INTEGER NOT NULL,
        tier TEXT NOT NULL,
        phase TEXT NOT NULL,
        state TEXT NOT NULL,
        started_at TEXT NOT NULL,
        ended_at TEXT,
        worktree TEXT,
        candidate_sha TEXT
    );
    CREATE TABLE transitions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        from_state TEXT,
        to_state TEXT NOT NULL,
        reason TEXT NOT NULL,
        detail_json TEXT,
        at TEXT NOT NULL
    );
    CREATE TABLE evidence (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        kind TEXT NOT NULL,
        path TEXT NOT NULL,
        sha256 TEXT,
        at TEXT NOT NULL
    );
    CREATE TABLE usage_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL UNIQUE,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        parent_event_id TEXT,
        model TEXT,
        input_tokens INTEGER,
        output_tokens INTEGER,
        cache_read_tokens INTEGER,
        cache_write_tokens INTEGER,
        cost_micros INTEGER NOT NULL,
        cost_kind TEXT NOT NULL,
        completeness TEXT NOT NULL,
        inclusive INTEGER NOT NULL DEFAULT 0,
        at TEXT NOT NULL
    );
    CREATE INDEX idx_usage_run ON usage_events(run_id);
    CREATE TABLE dispatches (
        dispatch_id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        intent_json TEXT NOT NULL,
        pid INTEGER,
        session_id TEXT,
        state TEXT NOT NULL,
        reserved_micros INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE TABLE outcomes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        detail_json TEXT,
        at TEXT NOT NULL
    );
    CREATE TABLE receipts (
        run_id TEXT PRIMARY KEY,
        receipt_json TEXT NOT NULL,
        hash TEXT NOT NULL,
        at TEXT NOT NULL
    );
    CREATE TABLE features (
        dispatch_id TEXT PRIMARY KEY,
        feature_json TEXT NOT NULL,
        at TEXT NOT NULL
    );
    CREATE TABLE predictions (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        run_id TEXT NOT NULL,
        artifact_id TEXT,
        input_hash TEXT,
        result_json TEXT,
        at TEXT NOT NULL
    );
    "#,
    ),
    (
        // Work packages (SPEC §19) are runs of their own, linked to the root
        // run so cost, attempts and identity aggregate over the tree.
        "v2",
        r#"
    ALTER TABLE runs ADD COLUMN parent_run TEXT;
    ALTER TABLE runs ADD COLUMN package_id TEXT;
    CREATE INDEX idx_runs_parent ON runs(parent_run);
    "#,
    ),
    (
        // Unknown usage is unknown, never zero (SPEC §11). `cost_micros`
        // was NOT NULL, so a dispatch whose harness reported no cost was
        // stored as 0 and summed into a figure that read as money. SQLite
        // cannot drop a NOT NULL constraint in place: the table is rebuilt
        // with the column nullable, rows preserved, and every row whose
        // completeness already said `unknown` gets the NULL its zero stood
        // for. `SUM` skips NULL, so a run's cost becomes the lower bound
        // its completeness label always claimed it was.
        "v3",
        r#"
    CREATE TABLE usage_events_v3 (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        event_id TEXT NOT NULL UNIQUE,
        run_id TEXT NOT NULL,
        attempt_id INTEGER,
        parent_event_id TEXT,
        model TEXT,
        input_tokens INTEGER,
        output_tokens INTEGER,
        cache_read_tokens INTEGER,
        cache_write_tokens INTEGER,
        cost_micros INTEGER,
        cost_kind TEXT NOT NULL,
        completeness TEXT NOT NULL,
        inclusive INTEGER NOT NULL DEFAULT 0,
        at TEXT NOT NULL
    );
    INSERT INTO usage_events_v3
        (id, event_id, run_id, attempt_id, parent_event_id, model,
         input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
         cost_micros, cost_kind, completeness, inclusive, at)
    SELECT id, event_id, run_id, attempt_id, parent_event_id, model,
           input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
           CASE WHEN completeness = '"unknown"' THEN NULL ELSE cost_micros END,
           cost_kind, completeness, inclusive, at
    FROM usage_events;
    DROP TABLE usage_events;
    ALTER TABLE usage_events_v3 RENAME TO usage_events;
    CREATE INDEX idx_usage_run ON usage_events(run_id);
    "#,
    ),
    (
        // A task is the identity a run's cost is attributed to (SPEC's
        // task spine): stable across a contract revision or a re-run, so
        // cost sums per task rather than per run. Every existing run
        // predates the spine, so each root run's tree is backfilled one
        // `task-legacy-<root run id>` task, `origin = 'backfilled'`,
        // propagated to every descendant by a recursive walk of
        // `parent_run` — the same tree `run_cost` already walks.
        "v4",
        r#"
    CREATE TABLE tasks (
        task_id TEXT PRIMARY KEY,
        repo_key TEXT NOT NULL,
        first_run TEXT NOT NULL,
        origin TEXT NOT NULL,
        created_at TEXT NOT NULL
    );
    ALTER TABLE runs ADD COLUMN task_id TEXT;
    CREATE INDEX idx_runs_task ON runs(task_id);
    -- `repo_key` is a hash of the run's repository identity everywhere a
    -- run writes one. A migration cannot compute it: the identity comes
    -- from git, and a legacy row carries only the path the run was
    -- launched in, which may have been a worktree that no longer exists.
    -- So a backfilled task says so — `legacy:<path>` is unmistakably not
    -- a key, and grouping by `repo_key` puts each legacy task in its own
    -- bucket instead of silently merging it with a repository's real one.
    INSERT INTO tasks (task_id, repo_key, first_run, origin, created_at)
        SELECT 'task-legacy-' || id, 'legacy:' || repo_path, id, 'backfilled', created_at
          FROM runs WHERE parent_run IS NULL;
    WITH RECURSIVE tree(id, root) AS (
        SELECT id, id FROM runs WHERE parent_run IS NULL
        UNION ALL
        SELECT runs.id, tree.root FROM runs JOIN tree ON runs.parent_run = tree.id
    )
    UPDATE runs SET task_id = 'task-legacy-' || (SELECT root FROM tree WHERE tree.id = runs.id)
     WHERE id IN (SELECT id FROM tree);
    "#,
    ),
    (
        "v5",
        r#"
    -- Typed outcomes (SPEC §20) are read per task and per candidate, not
    -- only per run: `outcomes.task_id` lets `latest_outcome` answer "is
    -- this task's accepted change still standing" without a join through
    -- `runs`, and `outcomes.candidate_sha` names the exact candidate a
    -- row is about even if the run later grows more attempts.
    --
    -- Its own step, not an edit to v4: v4 shipped in 2f37f80 and every
    -- ledger opened since has it recorded, so `apply_step` would skip
    -- the additions and the columns would exist only where the ledger
    -- had never been opened — which is every test and no real machine.
    ALTER TABLE outcomes ADD COLUMN task_id TEXT;
    ALTER TABLE outcomes ADD COLUMN candidate_sha TEXT;
    CREATE INDEX idx_outcomes_task ON outcomes(task_id);
    "#,
    ),
    (
        // A run that reaches `needs_review`, `needs_decision` or
        // `interrupted` is waiting for a person to answer it (SPEC's
        // decision spine: `State::awaits_a_person`). Before this, that
        // wait had no record of its own — only the run's transition
        // history, which never says WHO answered it or WHEN. One open
        // row per such run, closed by `relais decide`; `record_transition`
        // opens the row for every run from here on, in the same
        // transaction as the transition that raises it, so a run can
        // never be waiting without one.
        //
        // The three state names below are `State::awaits_a_person`'s
        // definition, spelled out in SQL because a migration cannot call
        // into this binary's Rust — `lifecycle`'s own test and this
        // migration's ledger-side test each assert they still agree.
        "v6",
        r#"
    CREATE TABLE decisions (
        run TEXT PRIMARY KEY,
        task TEXT NOT NULL,
        raised_at TEXT NOT NULL,
        raised_state TEXT NOT NULL,
        raised_reason TEXT NOT NULL,
        resolved_at TEXT,
        resolution TEXT,
        actor TEXT,
        note TEXT,
        successor_run TEXT
    );
    CREATE INDEX idx_decisions_unresolved ON decisions(resolved_at);
    INSERT INTO decisions (run, task, raised_at, raised_state, raised_reason)
    SELECT runs.id,
           COALESCE(runs.task_id, 'task-legacy-' || runs.id),
           COALESCE(
               (SELECT t.at FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               runs.updated_at
           ),
           COALESCE(
               (SELECT t.to_state FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               runs.status
           ),
           COALESCE(
               (SELECT t.reason FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               'worker_dispatched'
           )
      FROM runs
     WHERE COALESCE(
               (SELECT t.to_state FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1),
               runs.status
           ) IN ('needs_review', 'needs_decision', 'interrupted');
    "#,
    ),
    (
        "v7",
        r#"
    -- v6 backfilled each open decision from the run's LAST transition.
    -- For a run that started waiting and then had its worktree retired,
    -- that is the retirement row: a same-state transition whose detail
    -- carries a reference and a patch and no gaps. So the row pointed at
    -- the wrong moment, and `decide --answer approve` — which reads the
    -- transition `raised_at` names — would find no gaps on a run that
    -- has them. The raising transition is the FIRST one landing on a
    -- state that awaits a person, which for a backfilled row is the only
    -- wait it has ever had.
    --
    -- Its own step, not a correction of v6: v6 is recorded on every
    -- ledger opened since it merged, and `apply_step` skips a version
    -- already applied.
    UPDATE decisions
       SET raised_at = COALESCE(
               (SELECT t.at FROM transitions t
                 WHERE t.run_id = decisions.run
                   AND t.to_state IN ('needs_review', 'needs_decision', 'interrupted')
                 ORDER BY t.id ASC LIMIT 1),
               decisions.raised_at
           ),
           raised_state = COALESCE(
               (SELECT t.to_state FROM transitions t
                 WHERE t.run_id = decisions.run
                   AND t.to_state IN ('needs_review', 'needs_decision', 'interrupted')
                 ORDER BY t.id ASC LIMIT 1),
               decisions.raised_state
           ),
           raised_reason = COALESCE(
               (SELECT t.reason FROM transitions t
                 WHERE t.run_id = decisions.run
                   AND t.to_state IN ('needs_review', 'needs_decision', 'interrupted')
                 ORDER BY t.id ASC LIMIT 1),
               decisions.raised_reason
           )
     WHERE resolved_at IS NULL;
    "#,
    ),
    (
        // A usage event said only what it cost, never what produced it.
        // Five columns say so: `phase` names which of the run's phases
        // (worker attempt, review, planning, integration) spent the
        // money — the same typed `UsagePhase` an attempt's own `phase`
        // column already carries, so the two cannot spell the same thing
        // differently; `duration_ms` is the dispatch's own elapsed wall
        // time, not derived from `attempts.started_at`/`ended_at`, which
        // are the ATTEMPT's span and do not exist at all for a reviewer
        // or planner dispatch; `requested_model`/`requested_effort`
        // record what the route asked for, beside `model`, which already
        // records what the harness reported running, so a substitution
        // is visible in the row rather than only at the moment it
        // happened; `harness` names the identity the run probed.
        //
        // All nullable: a row written before this step reads them as
        // absent, never as a default that would misreport an attempt
        // this binary never measured.
        "v8",
        r#"
    ALTER TABLE usage_events ADD COLUMN phase TEXT;
    ALTER TABLE usage_events ADD COLUMN duration_ms INTEGER;
    ALTER TABLE usage_events ADD COLUMN requested_model TEXT;
    ALTER TABLE usage_events ADD COLUMN requested_effort TEXT;
    ALTER TABLE usage_events ADD COLUMN harness TEXT;
    "#,
    ),
];

pub struct Ledger {
    conn: Connection,
    path: std::path::PathBuf,
    clock: Box<dyn Clock>,
}

/// Every usage event that an inclusive ancestor already accounts for,
/// at any depth (P11).
///
/// An inclusive event's total already contains its descendants (SPEC
/// §11), so adding those descendants would count the same money twice.
/// The dedup this replaced looked one generation up, and the comment
/// above `UsageEvent::inclusive` said it was "ready for nesting" — it
/// was not: a grandchild of an inclusive event was counted. The walk is
/// `UNION`, not `UNION ALL`, so a parent chain that somehow loops
/// terminates instead of running forever. `event_id` is UNIQUE across
/// the table, so the walk needs no run scoping to be unambiguous.
const COVERED_BY_AN_INCLUSIVE_PARENT: &str = "WITH RECURSIVE covered(event_id) AS (
        SELECT child.event_id
          FROM usage_events child
          JOIN usage_events parent ON parent.event_id = child.parent_event_id
         WHERE parent.inclusive = 1
         UNION
        SELECT child.event_id
          FROM usage_events child
          JOIN covered ON covered.event_id = child.parent_event_id
     )";

/// How many times to ask for WAL before giving up. Each refusal means
/// another connection is setting the same mode right now — one pragma
/// on one connection, microseconds long — so a handful of attempts is
/// generous and a hang is impossible.
const WAL_ATTEMPTS: u32 = 32;

/// Put the ledger in WAL mode (SPEC §23: several processes share it).
///
/// `journal_mode` is persistent in the database FILE, so it is set once
/// and every later connection reads it back as `wal`. Changing it takes
/// a database-wide lock that SQLite refuses WITHOUT consulting the busy
/// handler, so `busy_timeout` does not cover this one statement: two
/// processes opening the same ledger in the same instant had one of them
/// die at the door with "database is locked" before it read a row. A
/// refusal here is not a broken ledger, it is somebody else setting the
/// same mode, so it is retried — and `PRAGMA journal_mode = WAL` answers
/// with the mode in force, which is the check and the change in one
/// statement.
fn use_wal(conn: &Connection) -> Result<()> {
    let mut refusal = None;
    for _ in 0..WAL_ATTEMPTS {
        match conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(mode) if mode.eq_ignore_ascii_case("wal") => return Ok(()),
            Ok(mode) => {
                return Err(LedgerError::Corrupt {
                    what: "the ledger's journal mode".into(),
                    detail: format!("SQLite kept `{mode}` when asked for WAL"),
                })
            }
            Err(e) => {
                refusal = Some(e);
                // Not a wait for time to pass: the other connection is
                // runnable now, and this hands it the core to finish on.
                std::thread::yield_now();
            }
        }
    }
    Err(refusal.map_or_else(
        || LedgerError::Corrupt {
            what: "the ledger's journal mode".into(),
            detail: "WAL was never asked for".into(),
        },
        LedgerError::from,
    ))
}

/// How many migration steps this ledger has applied.
fn applied_count(conn: &Connection) -> Result<u64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get(0)
    })?;
    Ok(count as u64)
}

/// Bring a ledger up to this binary's schema.
///
/// Each step is ONE transaction covering its DDL batch and the
/// `schema_migrations` row that records it (P1). Two consequences, both
/// load-bearing: a crash in the middle of a step rolls the whole step
/// back, so the next open does not meet half a table and fail forever
/// with "table already exists"; and `BEGIN IMMEDIATE` makes two
/// processes opening a fresh ledger serialise on the write lock instead
/// of racing between the check and the apply — the second one re-checks
/// INSIDE its transaction and finds the step already applied.
fn migrate(conn: &Connection, clock: &dyn Clock) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version TEXT PRIMARY KEY,
            applied_at TEXT NOT NULL
        )",
        [],
    )?;
    // Migrations are additive and never edited, so more applied steps
    // than this binary ships means a NEWER relais owns this ledger.
    // Refuse before touching it: its extra tables and columns are not
    // ours to write through.
    let applied = applied_count(conn)?;
    if applied > LEDGER_SCHEMA_VERSION {
        return Err(LedgerError::SchemaAhead {
            found: applied,
            known: LEDGER_SCHEMA_VERSION,
        });
    }
    for (version, sql) in MIGRATIONS {
        apply_step(conn, version, sql, &clock.now_rfc3339())?;
    }
    Ok(())
}

/// One migration step, all or nothing.
fn apply_step(conn: &Connection, version: &str, sql: &str, now: &str) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let already: Option<String> = tx
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = ?1",
            [version],
            |row| row.get(0),
        )
        .optional()?;
    if already.is_none() {
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub event_id: String,
    pub run_id: RunId,
    pub attempt_id: Option<i64>,
    pub parent_event_id: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    /// `None` = the harness reported no cost. Unknown, never zero (SPEC
    /// §11): it is stored as NULL, left out of every sum, and makes the
    /// run's completeness `unknown`.
    pub cost: Option<MicroUsd>,
    pub cost_kind: CostKind,
    pub completeness: CostCompleteness,
    /// True when this event's total already includes its descendants:
    /// an inclusive parent is never added to its children (SPEC §11).
    /// Set by the adapter when the harness reports subagents in the
    /// session; no producer records the descendants as rows of their
    /// own today (native subagents are observed, not dispatched), so
    /// `parent_event_id` stays `None` until managed nested dispatch
    /// exists. The dedup in `run_cost` walks the whole ancestor chain,
    /// so it is ready for one at any depth.
    pub inclusive: bool,
    pub at: String,
    /// Which phase of the run spent this money — a worker attempt at one
    /// of its three kinds, the reviewer, the planner, or a package's
    /// integration. `None` for a row written before this column existed,
    /// never a guess (SPEC §11: absent is absent).
    pub phase: Option<UsagePhase>,
    /// The dispatch's own elapsed wall time, measured with a monotonic
    /// clock around the launch that produced this event — not derived
    /// from any stored timestamp, and not the run's own elapsed time.
    pub duration_ms: Option<i64>,
    /// The model the route asked for, beside `model` (what the harness
    /// reported running): a substitution is visible in the row, not only
    /// at the moment it happened.
    pub requested_model: Option<String>,
    /// The effort the route asked for.
    pub requested_effort: Option<String>,
    /// The harness identity the run probed.
    pub harness: Option<String>,
}

/// One phase's contribution to a run's cost (SPEC §11): what
/// [`Ledger::run_cost_by_phase`] returns, one entry per distinct phase
/// seen plus the `None` unattributed bucket when any row predates the
/// `phase` column.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseCost {
    pub phase: Option<UsagePhase>,
    pub cost: MicroUsd,
    pub completeness: CostCompleteness,
}

/// Add one usage row's cost and completeness into a phase's running
/// total, or open a new entry for a phase not yet seen. Unknown cost
/// (`cost_micros: None`) contributes nothing to the sum but still folds
/// its completeness in, the same null-skip `SUM` gives `run_own_cost`.
fn fold_phase_cost(
    totals: &mut Vec<(Option<UsagePhase>, MicroUsd, Vec<CostCompleteness>)>,
    phase: Option<UsagePhase>,
    cost_micros: Option<i64>,
    completeness: CostCompleteness,
) {
    let cost = cost_micros.map_or(MicroUsd::ZERO, MicroUsd::from_micros);
    match totals.iter_mut().find(|(p, ..)| *p == phase) {
        Some((_, total, completenesses)) => {
            if cost_micros.is_some() {
                *total = total.saturating_add(cost);
            }
            completenesses.push(completeness);
        }
        None => totals.push((phase, cost, vec![completeness])),
    }
}

/// A work package's run as the ledger holds it (SPEC §19). Rows leave
/// the adapter typed: the status is a `State`, not a string a caller
/// re-parses or compares to a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRun {
    pub run: RunId,
    pub package: PackageId,
    pub status: State,
}

/// The repository a run ran in and the base it resolved, as the ledger
/// recorded them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunWorkspace {
    pub repo_path: PathBuf,
    pub base_sha: Option<String>,
}

/// A dispatch that claimed to launch and never recorded a terminal
/// state: what reconciliation looks at after a crash or restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDispatch {
    pub dispatch: DispatchId,
    pub run: RunId,
    /// The bound process, when one was recorded. `None` is "no pid on
    /// record" and nothing else — a value no process could have is a
    /// corrupt row and reported as one.
    pub pid: Option<Pid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub run_id: RunId,
    pub attempt_id: Option<i64>,
    pub from_state: Option<State>,
    pub to_state: State,
    pub reason: String,
    pub detail: Option<serde_json::Value>,
    pub at: String,
}

/// A run's status as a projection of its transition history (P3): the
/// last transition's `to_state`, falling back to the `runs.status` column
/// only for a row with no transition at all — an old ledger's run, written
/// before every state change recorded a row. Every status reader selects
/// this expression, so no reader can disagree with a chain that exists,
/// and `runs.status` survives only as that legacy fallback.
const PROJECTED_STATUS: &str = "COALESCE(\
    (SELECT t.to_state FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1), \
    runs.status) AS status";

/// A work package run's place in its parent's tree: the run it belongs
/// under and the package it is. Grouped so `insert_child_run` carries one
/// argument for both instead of two more positional ones.
pub struct ChildOf<'a> {
    pub parent_run: &'a RunId,
    pub package_id: &'a PackageId,
}

/// One run's decision row (SPEC's decision spine): when a person was
/// first owed a look, what state and reason raised it, and — once
/// answered — who decided what, when, and any successor run their
/// decision names. `resolved_at.is_none()` is the open half `report`
/// lists and `relais decide` closes; `waited_seconds` is derived from the
/// recorded timestamps, never stored, so it is always current for an open
/// row and exact for a resolved one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionRecord {
    pub run: RunId,
    pub task: TaskId,
    pub raised_at: String,
    pub raised_state: State,
    pub raised_reason: String,
    pub resolved_at: Option<String>,
    pub resolution: Option<Reason>,
    pub actor: Option<String>,
    pub note: Option<String>,
    pub successor_run: Option<RunId>,
    pub waited_seconds: i64,
}

/// The seconds between two RFC3339 timestamps this ledger itself wrote —
/// a stamp that will not parse is a corrupt row, not a wait of zero.
fn seconds_between(start: &str, end: &str) -> Result<i64> {
    let parse = |what: &'static str, stamp: &str| {
        chrono::DateTime::parse_from_rfc3339(stamp).map_err(|e| LedgerError::Corrupt {
            what: what.into(),
            detail: format!("`{stamp}` is not an RFC3339 timestamp: {e}"),
        })
    };
    let start = parse("a decision's raised_at", start)?;
    let end = parse("a decision's resolved_at", end)?;
    Ok((end - start).num_seconds())
}

/// Raw columns of one `decisions` row, in `SELECT` order.
type DecisionRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A `decisions` row as columns, parsed into a [`DecisionRecord`] (P6): an
/// unknown state or resolution is a `Corrupt` row the caller reports,
/// never a silently dropped or mismatched decision. `now` is the wait
/// clock for a row still open; a resolved row uses its own `resolved_at`
/// instead, so re-reading it later never changes what it says.
fn parse_decision_row(row: DecisionRow, now: &str) -> Result<DecisionRecord> {
    let (
        run,
        task,
        raised_at,
        raised_state,
        raised_reason,
        resolved_at,
        resolution,
        actor,
        note,
        successor_run,
    ) = row;
    let resolution = resolution
        .map(|stored| {
            Reason::parse(&stored).map_err(|unknown| LedgerError::Corrupt {
                what: unknown.what.into(),
                detail: unknown.to_string(),
            })
        })
        .transpose()?;
    let waited_seconds = seconds_between(&raised_at, resolved_at.as_deref().unwrap_or(now))?;
    Ok(DecisionRecord {
        run: RunId::from_stored(run),
        task: TaskId::from_stored(task),
        raised_at,
        raised_state: parse_state(&raised_state)?,
        raised_reason,
        resolved_at,
        resolution,
        actor,
        note,
        successor_run: successor_run.map(RunId::from_stored),
        waited_seconds,
    })
}

const DECISION_COLUMNS: &str =
    "run, task, raised_at, raised_state, raised_reason, resolved_at, resolution, actor, note, successor_run";

fn decision_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<DecisionRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
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
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        use_wal(&conn)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&conn, clock.as_ref())?;
        Ok(Ledger {
            conn,
            path: path.to_path_buf(),
            clock,
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

    /// Where this ledger lives, so a side thread can open its own
    /// connection (a connection is not shareable across threads).
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> Result<u64> {
        applied_count(&self.conn)
    }

    /// Record a task's row, if none exists yet, and the run row that
    /// belongs to it — one transaction (P1): a run whose task row lost a
    /// race would have its cost silently excluded from every
    /// `task_cost` sum. `INSERT OR IGNORE` because a task outlives one
    /// run: the same task dispatched again, or revised, derives the same
    /// [`TaskId`] and reuses the row its first run created.
    pub fn insert_run(
        &self,
        id: &RunId,
        repo_path: &str,
        root_session: Option<&str>,
        task: &TaskId,
        repo_key: &str,
    ) -> Result<()> {
        let now = self.now();
        let tx = self.write_tx()?;
        Self::ensure_task(&tx, task, repo_key, id, &now)?;
        tx.execute(
            "INSERT INTO runs (id, repo_path, status, root_session, created_at, updated_at, task_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
            params![
                id.as_str(),
                repo_path,
                State::Prepared.as_str(),
                root_session,
                now,
                task.as_str()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A work package's run: its own lifecycle, attributed to the root —
    /// and to the root's task, inherited by the caller (SPEC's task
    /// spine: a package run is not a task of its own).
    pub fn insert_child_run(
        &self,
        id: &RunId,
        repo_path: &str,
        root_session: Option<&str>,
        child: ChildOf<'_>,
        task: &TaskId,
        repo_key: &str,
    ) -> Result<()> {
        let now = self.now();
        let tx = self.write_tx()?;
        Self::ensure_task(&tx, task, repo_key, id, &now)?;
        tx.execute(
            "INSERT INTO runs (id, repo_path, status, root_session, created_at, updated_at,
                               parent_run, package_id, task_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8)",
            params![
                id.as_str(),
                repo_path,
                State::Prepared.as_str(),
                root_session,
                now,
                child.parent_run.as_str(),
                child.package_id.as_str(),
                task.as_str()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The tasks-row half of `insert_run`/`insert_child_run`: a no-op
    /// when the task already has a row (its first run created it), an
    /// insert with `origin = created` and `first_run = id` otherwise.
    fn ensure_task(
        tx: &Transaction<'_>,
        task: &TaskId,
        repo_key: &str,
        first_run: &RunId,
        now: &str,
    ) -> Result<()> {
        tx.execute(
            "INSERT OR IGNORE INTO tasks (task_id, repo_key, first_run, origin, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                task.as_str(),
                repo_key,
                first_run.as_str(),
                TaskOrigin::Created.as_str(),
                now
            ],
        )?;
        Ok(())
    }

    /// A root run's work packages, in creation order, with their states
    /// already parsed (P8): a status this binary cannot read is a
    /// corrupt row here, not a string a caller compares to a literal.
    pub fn child_runs(&self, parent_run: &RunId) -> Result<Vec<ChildRun>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, package_id, {PROJECTED_STATUS} FROM runs WHERE parent_run = ?1 ORDER BY created_at, id"
        ))?;
        type Row = (String, String, String);
        let rows = stmt.query_map([parent_run.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(run, package, status)| {
                Ok(ChildRun {
                    run: RunId::from_stored(run),
                    package: PackageId::from_stored(package),
                    status: parse_state(&status)?,
                })
            })
            .collect()
    }

    /// Every dispatch ever recorded for a run: the aggregate agent count
    /// no work package resets (SPEC §19).
    pub fn dispatch_count(&self, run_id: &RunId) -> Result<u32> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM dispatches WHERE run_id = ?1",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    /// A run's status, projected from its transition history rather than
    /// read from a column a writer could drift from it (P3): the last
    /// transition's `to_state`, when the run has one. A run with none
    /// yet — an old ledger's row, written before transitions recorded
    /// every state change — falls back to the `runs.status` an upgrade
    /// preserved, so status can never disagree with a chain that exists,
    /// and a run that predates the chain still answers.
    pub fn run_status(&self, id: &RunId) -> Result<Option<State>> {
        let status: Option<String> = self
            .conn
            .query_row(
                &format!("SELECT {PROJECTED_STATUS} FROM runs WHERE id = ?1"),
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        status.map(|s| parse_state(&s)).transpose()
    }

    /// Where a run's worktree came from: the repository it ran in and
    /// the base its latest contract revision resolved — what
    /// `workspace::retire` needs to name a leftover tree against the
    /// right base. `None` when the run is not on record; a run that
    /// never resolved a base (blocked before preflight finished) has
    /// `base_sha: None`.
    pub fn run_workspace(&self, id: &RunId) -> Result<Option<RunWorkspace>> {
        self.conn
            .query_row(
                "SELECT runs.repo_path,
                        (SELECT base_sha FROM contract_revisions
                          WHERE run_id = runs.id AND base_sha IS NOT NULL
                          ORDER BY id DESC LIMIT 1)
                 FROM runs WHERE id = ?1",
                [id.as_str()],
                |row| {
                    Ok(RunWorkspace {
                        repo_path: PathBuf::from(row.get::<_, String>(0)?),
                        base_sha: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(LedgerError::from)
    }

    /// Record a contract revision for a run and return its id.
    ///
    /// The insert and the `superseded_by` back-link on the run's
    /// previous revisions are one transaction: a revision chain with a
    /// gap in it would misreport which contract an attempt ran under
    /// (P13 — the column existed and nothing ever wrote it).
    pub fn insert_contract_revision(
        &self,
        run_id: &RunId,
        hash: &str,
        contract_json: &str,
        base_ref: &str,
        base_sha: Option<&str>,
    ) -> Result<i64> {
        let tx = self.write_tx()?;
        tx.execute(
            "INSERT INTO contract_revisions
                (run_id, hash, contract_json, base_ref, base_sha, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id.as_str(),
                hash,
                contract_json,
                base_ref,
                base_sha,
                self.now()
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE contract_revisions SET superseded_by = ?2
             WHERE run_id = ?1 AND id != ?2 AND superseded_by IS NULL",
            params![run_id.as_str(), id],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// The revision a run's contract revision was replaced by, if any —
    /// the read side of `superseded_by`.
    pub fn superseded_by(&self, revision_id: i64) -> Result<Option<i64>> {
        let found: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT superseded_by FROM contract_revisions WHERE id = ?1",
                [revision_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.flatten())
    }

    pub fn insert_attempt(
        &self,
        run_id: &RunId,
        revision_id: i64,
        attempt_index: i64,
        tier: &str,
        phase: UsagePhase,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO attempts
                (run_id, revision_id, attempt_index, tier, phase, state, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id.as_str(),
                revision_id,
                attempt_index,
                tier,
                phase.as_str(),
                State::Running.as_str(),
                self.now()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Distinct models that actually ran for a run, from usage events.
    pub fn models_used(&self, run_id: &RunId) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT model FROM usage_events
             WHERE run_id = ?1 AND model IS NOT NULL ORDER BY model",
        )?;
        let rows = stmt.query_map([run_id.as_str()], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The contract a run was dispatched under and the tier its first
    /// attempt ran at — what dataset construction needs to reconstruct
    /// dispatch-time features without future information (SPEC §21).
    ///
    /// Parsed here (P6): a stored contract that will not parse, or a
    /// tier name this binary does not know, is a `Corrupt` row the
    /// caller reports as a skipped run. It used to leave the adapter as
    /// raw JSON text with `unwrap_or(Null)`, and two `.ok().flatten()`
    /// layers downstream turned a busy database into runs that silently
    /// vanished from the dataset.
    pub fn run_contract_and_tier(&self, run_id: &RunId) -> Result<Option<(TaskContract, Tier)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT revisions.contract_json, attempts.tier
                 FROM contract_revisions revisions
                 JOIN attempts ON attempts.run_id = revisions.run_id
                 WHERE revisions.run_id = ?1
                 ORDER BY revisions.id, attempts.id
                 LIMIT 1",
                [run_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        row.map(|(contract_json, tier)| {
            let contract =
                TaskContract::from_json_str(&contract_json).map_err(|e| LedgerError::Corrupt {
                    what: format!("the contract of run {run_id}"),
                    detail: e.to_string(),
                })?;
            Ok((contract, parse_tier(&tier)?))
        })
        .transpose()
    }

    /// How many worker attempts a run consumed — the attempts table is
    /// the source of truth for the ladder.
    /// Did the ladder move? True when any attempt of the run ran at the
    /// `escalation` phase — the label source for "accepted WITHOUT
    /// escalation" (SPEC §17). Not derived from the models seen: the
    /// reviewer and the planner run on their own models without any
    /// escalation having happened.
    pub fn escalation_attempted(&self, run_id: &RunId) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE run_id = ?1 AND phase = 'escalation'",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn attempt_count(&self, run_id: &RunId) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE run_id = ?1",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Whether a run's reviewer actually ran, from usage events tagged
    /// with the review phase. The review-correction rate's denominator
    /// counts only tasks a reviewer looked at (SPEC §11): a rate over
    /// never-reviewed tasks says nothing about review quality.
    pub fn review_attempted(&self, run_id: &RunId) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM usage_events WHERE run_id = ?1 AND phase = 'review'",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// A run's wall-clock span, from its first attempt's start to its
    /// last attempt's end. `None` while ANY attempt of the run is still
    /// open: a run in flight has no duration to report, not a partial
    /// one. `MAX` skips NULLs, so asking for it alone would answer with
    /// the span of the attempts that happen to have finished and call a
    /// run that is still working "done in eight minutes".
    pub fn run_duration_seconds(&self, run_id: &RunId) -> Result<Option<f64>> {
        let span: (Option<String>, Option<String>, i64) = self.conn.query_row(
            "SELECT MIN(started_at), MAX(ended_at),
                    SUM(CASE WHEN ended_at IS NULL THEN 1 ELSE 0 END)
               FROM attempts WHERE run_id = ?1",
            [run_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2).unwrap_or(0))),
        )?;
        if span.2 > 0 {
            return Ok(None);
        }
        let span = (span.0, span.1);
        let (start, end) = match span {
            (Some(start), Some(end)) => (start, end),
            _ => return Ok(None),
        };
        let parse = |what: &'static str, stamp: &str| {
            chrono::DateTime::parse_from_rfc3339(stamp).map_err(|e| LedgerError::Corrupt {
                what: format!("{what} of run {run_id}"),
                detail: format!("`{stamp}` is not an RFC3339 timestamp: {e}"),
            })
        };
        let start = parse("an attempt's started_at", &start)?;
        let end = parse("an attempt's ended_at", &end)?;
        Ok(Some((end - start).num_seconds() as f64))
    }

    /// The earliest model dispatched for any of a task's runs — the
    /// cohort key for `--by model`. A task revised across models still
    /// needs one deterministic bucket, and the model it started under is
    /// the one an author actually chose.
    pub fn task_first_model(&self, task_id: &TaskId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT usage_events.model FROM usage_events
                 JOIN runs ON runs.id = usage_events.run_id
                 WHERE runs.task_id = ?1 AND usage_events.model IS NOT NULL
                 ORDER BY usage_events.at ASC, usage_events.id ASC
                 LIMIT 1",
                [task_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// The policy hash on the most recent receipt among a task's runs —
    /// the cohort key for `--by policy`. A task with no receipt yet (no
    /// run of it reached verification) has no policy to report.
    pub fn task_policy_hash(&self, task_id: &TaskId) -> Result<Option<String>> {
        let receipt_json: Option<String> = self
            .conn
            .query_row(
                "SELECT receipts.receipt_json FROM receipts
                 JOIN runs ON runs.id = receipts.run_id
                 WHERE runs.task_id = ?1
                 ORDER BY receipts.at DESC
                 LIMIT 1",
                [task_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        receipt_json
            .map(|json| {
                serde_json::from_str::<crate::verify::Receipt>(&json)
                    .map(|receipt| receipt.policy_hash)
                    .map_err(|e| LedgerError::Corrupt {
                        what: format!("the receipt of task {task_id}"),
                        detail: e.to_string(),
                    })
            })
            .transpose()
    }

    pub fn finish_attempt(
        &self,
        attempt_id: i64,
        state: State,
        worktree: Option<&str>,
        candidate_sha: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE attempts
             SET state = ?2, ended_at = ?3, worktree = COALESCE(?4, worktree),
                 candidate_sha = COALESCE(?5, candidate_sha)
             WHERE id = ?1",
            params![
                attempt_id,
                state.as_str(),
                self.now(),
                worktree,
                candidate_sha
            ],
        )?;
        Ok(())
    }

    /// Record a transition and the run status it implies.
    ///
    /// One transaction, because they are one fact (P3). As two
    /// autocommits, a kill between them left a run whose history said
    /// `accepted` and whose status still said `verifying`, and `resume`
    /// then overwrote an accepted run as `interrupted`.
    ///
    /// The only other method that writes a record plus derived state used
    /// to be `insert_contract_revision`; this one now also opens a
    /// [`DecisionRecord`] in the same transaction whenever the transition
    /// lands on a state [`State::awaits_a_person`] — a run can never be
    /// waiting on a person without a row recording that it started to.
    /// Nothing here closes one: only `resolve_decision` does that.
    pub fn record_transition(&self, transition: &Transition) -> Result<()> {
        let detail = transition
            .detail
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| LedgerError::Corrupt {
                what: "a transition detail".into(),
                detail: e.to_string(),
            })?;
        let tx = self.write_tx()?;
        tx.execute(
            "INSERT INTO transitions
                (run_id, attempt_id, from_state, to_state, reason, detail_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                transition.run_id.as_str(),
                transition.attempt_id,
                transition.from_state.map(|s| s.as_str()),
                transition.to_state.as_str(),
                transition.reason,
                detail,
                transition.at,
            ],
        )?;
        // Every transition, not only the terminal ones: `relais status`
        // used to say `prepared` for a run ten minutes into verifying.
        tx.execute(
            "UPDATE runs SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![
                transition.run_id.as_str(),
                transition.to_state.as_str(),
                self.now()
            ],
        )?;
        if transition.to_state.awaits_a_person() {
            let task_id: Option<String> = tx
                .query_row(
                    "SELECT task_id FROM runs WHERE id = ?1",
                    [transition.run_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let task_id =
                task_id.unwrap_or_else(|| format!("task-legacy-{}", transition.run_id.as_str()));
            // `OR IGNORE`: a run already terminal in an awaiting state can
            // still record a same-state transition of its own (worktree
            // retirement transitions `self.state` to itself once the run
            // has already stopped) — the decision was already opened the
            // first time this run reached the state, and `run` is its
            // primary key.
            tx.execute(
                "INSERT OR IGNORE INTO decisions (run, task, raised_at, raised_state, raised_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    transition.run_id.as_str(),
                    task_id,
                    transition.at,
                    transition.to_state.as_str(),
                    transition.reason,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every run still waiting for a person to answer it, oldest raised
    /// first — the queue `relais report` lists and `relais decide`
    /// closes one row at a time from.
    pub fn open_decisions(&self) -> Result<Vec<DecisionRecord>> {
        let now = self.now();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE resolved_at IS NULL ORDER BY raised_at, run"
        ))?;
        let rows = stmt.query_map([], decision_row_from)?;
        rows.collect::<std::result::Result<Vec<DecisionRow>, _>>()?
            .into_iter()
            .map(|row| parse_decision_row(row, &now))
            .collect()
    }

    /// One run's own decision row, open or resolved — what `relais
    /// explain` reads back. `None` for a run that never raised one.
    pub fn decision_of_run(&self, run_id: &RunId) -> Result<Option<DecisionRecord>> {
        let now = self.now();
        let row: Option<DecisionRow> = self
            .conn
            .query_row(
                &format!("SELECT {DECISION_COLUMNS} FROM decisions WHERE run = ?1"),
                [run_id.as_str()],
                decision_row_from,
            )
            .optional()?;
        row.map(|row| parse_decision_row(row, &now)).transpose()
    }

    /// Answer an open decision: who decided what, when, with an optional
    /// note and successor run. `Ok(false)` when the run has no OPEN
    /// decision — never raised one, or one already answered — so the CLI
    /// can refuse "deciding a run that is not waiting" by name rather
    /// than silently overwriting a prior answer.
    pub fn resolve_decision(
        &self,
        run_id: &RunId,
        resolution: Reason,
        actor: &str,
        note: Option<&str>,
        successor_run: Option<&RunId>,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE decisions SET resolved_at = ?2, resolution = ?3, actor = ?4, note = ?5, successor_run = ?6
             WHERE run = ?1 AND resolved_at IS NULL",
            params![
                run_id.as_str(),
                self.now(),
                resolution.as_str(),
                actor,
                note,
                successor_run.map(RunId::as_str),
            ],
        )?;
        Ok(changed > 0)
    }

    /// One piece of evidence bound to a run: the context manifest, a
    /// candidate patch, a check log, the receipt — by path and content
    /// hash (SPEC §12: "every transition has … evidence references").
    pub fn record_evidence(
        &self,
        run_id: &RunId,
        attempt_id: Option<i64>,
        kind: &str,
        path: &Path,
        sha256: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO evidence (run_id, attempt_id, kind, path, sha256, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id.as_str(),
                attempt_id,
                kind,
                path.to_string_lossy(),
                sha256,
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn evidence(&self, run_id: &RunId) -> Result<Vec<(String, String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, path, sha256 FROM evidence WHERE run_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn transitions(&self, run_id: &RunId) -> Result<Vec<Transition>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, attempt_id, from_state, to_state, reason, detail_json, at
             FROM transitions WHERE run_id = ?1 ORDER BY id",
        )?;
        // The rows come back raw and are interpreted here: a state this
        // binary does not know is a LedgerError, not a panic in the
        // middle of `explain`.
        type Row = (
            String,
            Option<i64>,
            Option<String>,
            String,
            String,
            Option<String>,
            String,
        );
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(run_id, attempt_id, from_state, to_state, reason, detail, at)| {
                    Ok(Transition {
                        run_id: RunId::from_stored(run_id),
                        attempt_id,
                        from_state: from_state.as_deref().map(parse_state).transpose()?,
                        to_state: parse_state(&to_state)?,
                        reason,
                        // Free-form diagnostic JSON a transition carried.
                        // Nothing reads it back as a value — it is
                        // printed — so a row that will not parse is
                        // reported as "no detail", not as a read failure
                        // that would hide the transition itself.
                        detail: detail.and_then(|d| serde_json::from_str(&d).ok()),
                        at,
                    })
                },
            )
            .collect()
    }

    pub fn record_usage(&self, event: &UsageEvent) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO usage_events
                (event_id, run_id, attempt_id, parent_event_id, model,
                 input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                 cost_micros, cost_kind, completeness, inclusive, at,
                 phase, duration_ms, requested_model, requested_effort, harness)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?17, ?18, ?19)",
            params![
                event.event_id,
                event.run_id.as_str(),
                event.attempt_id,
                event.parent_event_id,
                event.model,
                event.input_tokens,
                event.output_tokens,
                event.cache_read_tokens,
                event.cache_write_tokens,
                event.cost.map(MicroUsd::to_micros),
                serde_json::to_string(&event.cost_kind).expect("cost kind serializes"),
                serde_json::to_string(&event.completeness).expect("completeness serializes"),
                event.inclusive,
                event.at,
                event.phase.map(UsagePhase::as_str),
                event.duration_ms,
                event.requested_model,
                event.requested_effort,
                event.harness,
            ],
        )?;
        Ok(inserted == 1)
    }

    /// Dispatch intent is persisted BEFORE the process exists (SPEC §12).
    /// A retry with the same dispatch ID is a no-op, not a duplicate.
    pub fn record_dispatch_intent(
        &self,
        dispatch_id: &DispatchId,
        run_id: &RunId,
        attempt_id: Option<i64>,
        intent: &serde_json::Value,
        reserved_micros: i64,
    ) -> Result<bool> {
        let now = self.now();
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO dispatches
                (dispatch_id, run_id, attempt_id, intent_json, state,
                 reserved_micros, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                dispatch_id.as_str(),
                run_id.as_str(),
                attempt_id,
                serde_json::to_string(intent).expect("intent serializes"),
                "intent",
                reserved_micros,
                now
            ],
        )?;
        Ok(inserted == 1)
    }

    pub fn attach_dispatch_process(
        &self,
        dispatch_id: &DispatchId,
        pid: Option<Pid>,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE dispatches SET pid = ?2, session_id = ?3, state = ?4, updated_at = ?5
             WHERE dispatch_id = ?1",
            params![
                dispatch_id.as_str(),
                pid.map(|pid| i64::from(pid.get())),
                session_id,
                "launched",
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn finish_dispatch(&self, dispatch_id: &DispatchId, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE dispatches SET state = ?2, updated_at = ?3 WHERE dispatch_id = ?1",
            params![dispatch_id.as_str(), state, self.now()],
        )?;
        Ok(())
    }

    /// Dispatches that claimed to launch but never recorded a terminal
    /// state — the reconciliation set after a crash or restart. An absent
    /// terminal result never means nothing executed (SPEC §12).
    pub fn live_dispatches(&self) -> Result<Vec<LiveDispatch>> {
        let mut stmt = self
            .conn
            .prepare("SELECT dispatch_id, run_id, pid FROM dispatches WHERE state = 'launched'")?;
        type Row = (String, String, Option<i64>);
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(dispatch, run, pid)| {
                let pid = match pid {
                    None => None,
                    Some(raw) => Some(Pid::stored(raw).ok_or_else(|| LedgerError::Corrupt {
                        what: format!("the pid of dispatch {dispatch}"),
                        detail: format!("`{raw}` is not a process id"),
                    })?),
                };
                Ok(LiveDispatch {
                    dispatch: DispatchId::from_stored(dispatch),
                    run: RunId::from_stored(run),
                    pid,
                })
            })
            .collect()
    }

    /// Aggregate cost for a run: the sum of what was REPORTED. Unknown
    /// usage is NULL and `SUM` skips it, so the figure is a lower bound
    /// whenever `run_cost_completeness` says `unknown` — read the two
    /// together (`report::cost_line` does). Inclusive parent totals are
    /// alternative aggregation sources (SPEC §11): an inclusive event
    /// already contains its descendants, so everything below an
    /// inclusive parent is excluded and the parent is counted once. An
    /// inclusive event with no children simply counts itself.
    pub fn run_cost(&self, run_id: &RunId) -> Result<MicroUsd> {
        let mut total = self.run_own_cost(run_id)?;
        // A root run's cost is its tree's: packages are separate runs
        // attributed to it (SPEC §19, §23), each counted once.
        for child in self.child_runs(run_id)? {
            total = total.saturating_add(self.run_cost(&child.run)?);
        }
        Ok(total)
    }

    fn run_own_cost(&self, run_id: &RunId) -> Result<MicroUsd> {
        let micros: i64 = self.conn.query_row(
            &format!(
                "{COVERED_BY_AN_INCLUSIVE_PARENT}
                 SELECT COALESCE(SUM(cost_micros), 0) FROM usage_events
                  WHERE run_id = ?1
                    AND event_id NOT IN (SELECT event_id FROM covered)"
            ),
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(MicroUsd::from_micros(micros))
    }

    /// Worst-case completeness for the run's recorded usage: an unknown
    /// anywhere makes the run's cost unknown; an incomplete anywhere makes
    /// it an incomplete lower bound.
    pub fn run_cost_completeness(&self, run_id: &RunId) -> Result<CostCompleteness> {
        let mut values: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT completeness FROM usage_events WHERE run_id = ?1")?;
            let rows = stmt.query_map([run_id.as_str()], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for child in self.child_runs(run_id)? {
            values.push(
                serde_json::to_string(&self.run_cost_completeness(&child.run)?)
                    .expect("CostCompleteness serializes: a fieldless enum"),
            );
        }
        Ok(CostCompleteness::worst(values.iter().map(|value| {
            // A completeness this binary cannot read IS unknown: the fold
            // is the value's own meaning, not a swallowed error, and it
            // can only widen the answer (`Unknown` is the worst case).
            serde_json::from_str(value).unwrap_or(CostCompleteness::Unknown)
        })))
    }

    /// The run's spend grouped by phase (SPEC §11): which of the run's
    /// phases — a worker attempt, the reviewer, the planner, a package's
    /// integration — spent the money, and how complete that figure is.
    /// `phase: None` is the unattributed bucket: a row written before
    /// the `phase` column existed, or a child run's own unattributed
    /// rows. Same dedup as [`Ledger::run_cost`]: an inclusive parent's
    /// descendants are excluded so nothing is counted twice, and a
    /// child run's (a work package's) breakdown is merged into its
    /// parent's by phase.
    pub fn run_cost_by_phase(&self, run_id: &RunId) -> Result<Vec<PhaseCost>> {
        type Row = (Option<String>, Option<i64>, String);
        let mut stmt = self.conn.prepare(&format!(
            "{COVERED_BY_AN_INCLUSIVE_PARENT}
             SELECT phase, cost_micros, completeness FROM usage_events
              WHERE run_id = ?1
                AND event_id NOT IN (SELECT event_id FROM covered)"
        ))?;
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        let mut totals: Vec<(Option<UsagePhase>, MicroUsd, Vec<CostCompleteness>)> = Vec::new();
        for (phase_raw, cost_micros, completeness_raw) in rows {
            let phase = phase_raw
                .as_deref()
                .map(UsagePhase::parse)
                .transpose()
                .map_err(|unknown| LedgerError::Corrupt {
                    what: format!("the phase of a usage event of run {run_id}"),
                    detail: unknown.to_string(),
                })?;
            // A completeness this binary cannot read IS unknown, same
            // fold as `run_cost_completeness`.
            let completeness: CostCompleteness =
                serde_json::from_str(&completeness_raw).unwrap_or(CostCompleteness::Unknown);
            fold_phase_cost(&mut totals, phase, cost_micros, completeness);
        }
        for child in self.child_runs(run_id)? {
            for entry in self.run_cost_by_phase(&child.run)? {
                fold_phase_cost(
                    &mut totals,
                    entry.phase,
                    Some(entry.cost.to_micros()),
                    entry.completeness,
                );
            }
        }
        totals.sort_by_key(|(phase, ..)| (phase.is_none(), *phase));
        Ok(totals
            .into_iter()
            .map(|(phase, cost, completenesses)| PhaseCost {
                phase,
                cost,
                completeness: CostCompleteness::worst(completenesses),
            })
            .collect())
    }

    pub fn store_receipt(
        &self,
        run_id: &RunId,
        receipt_json: &serde_json::Value,
        hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO receipts (run_id, receipt_json, hash, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                run_id.as_str(),
                serde_json::to_string(receipt_json).expect("receipt serializes"),
                hash,
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn receipt(&self, run_id: &RunId) -> Result<Option<(serde_json::Value, String)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT receipt_json, hash FROM receipts WHERE run_id = ?1",
                [run_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        // A receipt is the run's evidence; a stored one that will not
        // parse is reported, so `explain` prints the rest of what it
        // knows instead of aborting.
        row.map(|(json, hash)| {
            let value = serde_json::from_str(&json).map_err(|e| LedgerError::Corrupt {
                what: format!("the receipt of run {run_id}"),
                detail: e.to_string(),
            })?;
            Ok((value, hash))
        })
        .transpose()
    }

    /// Record a typed final-outcome (SPEC §20), attributed to the task
    /// and the exact candidate the detail names.
    pub fn record_outcome(
        &self,
        run_id: &RunId,
        task_id: &TaskId,
        outcome: &Outcome,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO outcomes (run_id, task_id, candidate_sha, kind, detail_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id.as_str(),
                task_id.as_str(),
                outcome.detail.candidate_sha,
                outcome.kind.as_str(),
                serde_json::to_string(&outcome.detail).expect("outcome detail serializes"),
                self.now()
            ],
        )?;
        Ok(())
    }

    /// The most recently recorded outcome for a task, if any.
    pub fn latest_outcome(&self, task_id: &TaskId) -> Result<Option<StoredOutcome>> {
        let row: Option<OutcomeRow> = self
            .conn
            .query_row(
                "SELECT run_id, task_id, kind, detail_json, at FROM outcomes
                 WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(run_id, task_id, kind, detail_json, at)| {
            parse_outcome_row(run_id, task_id, kind, detail_json, at)
        })
        .transpose()
    }

    /// Every outcome recorded at or after `since`, oldest first.
    pub fn outcomes_since(&self, since: &str) -> Result<Vec<StoredOutcome>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, task_id, kind, detail_json, at FROM outcomes
             WHERE at >= ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (run_id, task_id, kind, detail_json, at) = row?;
            parse_outcome_row(run_id, task_id, kind, detail_json, at)
        })
        .collect()
    }

    /// The intent recorded for a run's first dispatch: model, effort and
    /// harness identity as they were at dispatch time (SPEC §21: features
    /// are reconstructed without future information).
    pub fn first_dispatch_intent(&self, run_id: &RunId) -> Result<Option<serde_json::Value>> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT intent_json FROM dispatches WHERE run_id = ?1
                 ORDER BY created_at, dispatch_id LIMIT 1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        // An intent column that is not JSON is a corrupt row, not an
        // absent intent: read as `None` the dataset would exclude the
        // run for "no dispatch intent" and never name the real fault.
        text.map(|text| {
            serde_json::from_str(&text).map_err(|e| LedgerError::Corrupt {
                what: format!("the dispatch intent of run {run_id}"),
                detail: e.to_string(),
            })
        })
        .transpose()
    }

    /// What a learned artifact estimated for a run at routing time, so a
    /// report can compare the estimate with what happened (SPEC §16).
    pub fn record_prediction(
        &self,
        run_id: &RunId,
        artifact_id: &str,
        input_hash: &str,
        result: &serde_json::Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO predictions (run_id, artifact_id, input_hash, result_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run_id.as_str(),
                artifact_id,
                input_hash,
                serde_json::to_string(result).expect("prediction serializes"),
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn predictions(&self, run_id: &RunId) -> Result<Vec<(String, String, serde_json::Value)>> {
        let mut stmt = self.conn.prepare(
            "SELECT artifact_id, input_hash, result_json FROM predictions WHERE run_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([run_id.as_str()], |row| {
            let text: String = row.get(2)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn record_features(
        &self,
        dispatch_id: &DispatchId,
        features: &serde_json::Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO features (dispatch_id, feature_json, at) VALUES (?1, ?2, ?3)",
            params![
                dispatch_id.as_str(),
                serde_json::to_string(features).expect("features serialize"),
                self.now()
            ],
        )?;
        Ok(())
    }

    /// All root runs created at or after `since` (RFC3339), newest first.
    /// Package runs are folded into their root's cost and are not listed
    /// twice.
    pub fn runs_since(&self, since: &str) -> Result<Vec<(RunId, String, String, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, repo_path, {PROJECTED_STATUS}, created_at FROM runs
                 WHERE created_at >= ?1 AND parent_run IS NULL ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([since], |row| {
            Ok((
                RunId::from_stored(row.get::<_, String>(0)?),
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Everything this machine has spent since `since` (RFC3339), across
    /// every run — what a per-day ceiling is a ceiling on. Unknown usage
    /// is NULL and `SUM` skips it, so this is a lower bound: a daily
    /// ceiling is best effort in exactly the way SPEC §11 says a dollar
    /// control is. Children attributed to an inclusive parent are
    /// excluded on the same rule as `run_own_cost`, so a provider total
    /// that already contains its subagents is counted once.
    pub fn spend_since(&self, since: &str) -> Result<MicroUsd> {
        let micros: i64 = self.conn.query_row(
            &format!(
                "{COVERED_BY_AN_INCLUSIVE_PARENT}
                 SELECT COALESCE(SUM(cost_micros), 0) FROM usage_events
                  WHERE at >= ?1
                    AND event_id NOT IN (SELECT event_id FROM covered)"
            ),
            [since],
            |row| row.get(0),
        )?;
        Ok(MicroUsd::from_micros(micros))
    }

    /// The task a run belongs to, when one is on record. `None` only for
    /// a row written by a binary older than schema v4 that has not yet
    /// been migrated into this ledger — every run the migration or
    /// `insert_run`/`insert_child_run` touched has one.
    pub fn task_of_run(&self, run_id: &RunId) -> Result<Option<TaskId>> {
        let task: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT task_id FROM runs WHERE id = ?1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(task.flatten().map(TaskId::from_stored))
    }

    /// Every run recorded under a task, in creation order — the root and
    /// every package inherited from it, each counted once.
    pub fn runs_of_task(&self, task_id: &TaskId) -> Result<Vec<RunId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM runs WHERE task_id = ?1 ORDER BY created_at, id")?;
        let rows = stmt.query_map([task_id.as_str()], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(RunId::from_stored)
            .collect())
    }

    /// A task's total cost: each of its runs' OWN usage, summed once.
    /// Unlike [`Ledger::run_cost`] this does not also walk `child_runs` —
    /// a decomposed run's packages already carry the same `task_id` as
    /// their root (inherited at `insert_child_run`), so a run-by-run own
    /// cost over every run of the task already counts the whole tree
    /// exactly once; recursing as `run_cost` does would double it.
    pub fn task_cost(&self, task_id: &TaskId) -> Result<MicroUsd> {
        let mut total = MicroUsd::ZERO;
        for run in self.runs_of_task(task_id)? {
            total = total.saturating_add(self.run_own_cost(&run)?);
        }
        Ok(total)
    }

    /// Worst-case completeness across a task's runs, the same rule
    /// [`Ledger::run_cost_completeness`] applies to one run's tree.
    pub fn task_cost_completeness(&self, task_id: &TaskId) -> Result<CostCompleteness> {
        let mut values = Vec::new();
        for run in self.runs_of_task(task_id)? {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT completeness FROM usage_events WHERE run_id = ?1")?;
            let rows = stmt.query_map([run.as_str()], |row| row.get::<_, String>(0))?;
            for value in rows {
                values.push(value?);
            }
        }
        Ok(CostCompleteness::worst(values.iter().map(|value| {
            // A completeness this binary cannot read IS unknown: the fold
            // is the value's own meaning, not a swallowed error, and it
            // can only widen the answer (`Unknown` is the worst case).
            serde_json::from_str(value).unwrap_or(CostCompleteness::Unknown)
        })))
    }

    /// Every task created at or after `since` (RFC3339), newest first —
    /// the read side of the tasks table, typed (P6/P8): a row whose
    /// `origin` no version of this binary wrote is a `Corrupt` result,
    /// never a silently accepted string.
    pub fn tasks_since(&self, since: &str) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT task_id, repo_key, first_run, origin, created_at FROM tasks
             WHERE created_at >= ?1 ORDER BY created_at DESC",
        )?;
        type Row = (String, String, String, String, String);
        let rows = stmt.query_map([since], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(task_id, repo_key, first_run, origin, created_at)| {
                Ok(TaskRow {
                    task_id: TaskId::from_stored(task_id),
                    repo_key,
                    first_run: RunId::from_stored(first_run),
                    origin: parse_task_origin(&origin)?,
                    created_at,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str) -> RunId {
        RunId::from_stored(id)
    }

    fn dispatch(id: &str) -> DispatchId {
        DispatchId::from_stored(id)
    }

    fn package(id: &str) -> PackageId {
        PackageId::from_stored(id)
    }

    fn task(id: &str) -> TaskId {
        TaskId::from_stored(format!("task-{id}"))
    }

    /// Pid AND an in-process counter, pre-cleaned: the test binary runs
    /// these in parallel threads of one process, so a pid-only name is
    /// one directory two tests share (P12).
    fn temp_dir(label: &str) -> std::path::PathBuf {
        crate::test_support::temp_dir(&format!("ledger-{label}"))
    }

    fn temp_ledger() -> (Ledger, std::path::PathBuf) {
        let dir = temp_dir("open");
        let path = dir.join("ledger.sqlite");
        let ledger = Ledger::open(&path).expect("ledger opens");
        (ledger, dir)
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

    /// `record_transition` opens a decision row, in the same transaction,
    /// the moment a run lands on a state a person is owed a look at — and
    /// closes nothing itself.
    #[test]
    fn record_transition_opens_a_decision_for_a_state_awaiting_a_person() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-nr"), "/repo", None, &task("run-nr"), "rk")
            .expect("run");
        assert!(ledger
            .decision_of_run(&run("run-nr"))
            .expect("read")
            .is_none());
        ledger
            .record_transition(&Transition {
                run_id: run("run-nr"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsReview,
                reason: Reason::ReviewFindings.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        let decision = ledger
            .decision_of_run(&run("run-nr"))
            .expect("read")
            .expect("a decision was opened");
        assert_eq!(decision.raised_state, State::NeedsReview);
        assert_eq!(decision.raised_reason, Reason::ReviewFindings.as_str());
        assert!(decision.resolved_at.is_none());
        assert!(decision.resolution.is_none());
        assert!(decision.waited_seconds >= 0);
        let open = ledger.open_decisions().expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].run, run("run-nr"));
        // A transition into a state nobody is owed a look at opens nothing.
        ledger
            .insert_run(&run("run-ok"), "/repo", None, &task("run-ok"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-ok"),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Running,
                reason: Reason::WorkerDispatched.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert!(ledger
            .decision_of_run(&run("run-ok"))
            .expect("read")
            .is_none());
        assert_eq!(ledger.open_decisions().expect("open").len(), 1);
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `resolve_decision` closes the open row and is refused, by a plain
    /// `false` the caller reports by name, for a run with no open
    /// decision — never raised, or already answered.
    #[test]
    fn resolve_decision_closes_once_and_refuses_a_run_not_waiting() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-d"), "/repo", None, &task("run-d"), "rk")
            .expect("run");
        assert!(!ledger
            .resolve_decision(
                &run("run-d"),
                Reason::DecisionRecorded,
                "a person",
                None,
                None
            )
            .expect("resolve"));
        ledger
            .record_transition(&Transition {
                run_id: run("run-d"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::ScopeExceeded.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert!(ledger
            .resolve_decision(
                &run("run-d"),
                Reason::DecisionApproved,
                "a person",
                Some("looks fine"),
                Some(&run("run-successor")),
            )
            .expect("resolve"));
        let decision = ledger
            .decision_of_run(&run("run-d"))
            .expect("read")
            .expect("still on record");
        assert_eq!(decision.resolution, Some(Reason::DecisionApproved));
        assert_eq!(decision.actor.as_deref(), Some("a person"));
        assert_eq!(decision.note.as_deref(), Some("looks fine"));
        assert_eq!(decision.successor_run, Some(run("run-successor")));
        assert!(decision.resolved_at.is_some());
        assert!(ledger.open_decisions().expect("open").is_empty());
        // A second answer to the same run is refused: it is not waiting
        // any more.
        assert!(!ledger
            .resolve_decision(
                &run("run-d"),
                Reason::DecisionRejected,
                "someone else",
                None,
                None
            )
            .expect("resolve"));
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    fn event(event_id: &str, run_id: &str, micros: i64) -> UsageEvent {
        UsageEvent {
            event_id: event_id.into(),
            run_id: run(run_id),
            attempt_id: None,
            parent_event_id: None,
            model: Some("sonnet".into()),
            input_tokens: Some(100),
            output_tokens: Some(10),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost: Some(MicroUsd::from_micros(micros)),
            cost_kind: CostKind::ApiSpend,
            completeness: CostCompleteness::Actual,
            inclusive: false,
            at: now_rfc3339(),
            phase: None,
            duration_ms: None,
            requested_model: None,
            requested_effort: None,
            harness: None,
        }
    }

    #[test]
    fn unknown_usage_is_null_left_out_of_the_sum_and_poisons_completeness() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-u"), "/r", None, &task("run-u"), "rk")
            .expect("run");
        ledger
            .record_usage(&event("known", "run-u", 700))
            .expect("known");
        let mut unknown = event("unknown", "run-u", 0);
        unknown.cost = None;
        unknown.completeness = CostCompleteness::Unknown;
        ledger.record_usage(&unknown).expect("unknown");
        assert_eq!(
            ledger.run_cost(&run("run-u")).expect("cost"),
            MicroUsd::from_micros(700),
            "the reported part, a lower bound"
        );
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-u"))
                .expect("completeness"),
            CostCompleteness::Unknown
        );
        let stored: Option<i64> = ledger
            .conn
            .query_row(
                "SELECT cost_micros FROM usage_events WHERE event_id = 'unknown'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(stored, None, "NULL, not 0");
        // Best effort: the fixture is a temp dir and a leftover costs
        // nothing but disk, which the next run's pre-clean takes.
        std::fs::remove_dir_all(&dir).ok();
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
    fn an_unparseable_receipt_is_an_error_not_a_panic() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-bad"), "/repo", None, &task("run-bad"), "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO receipts (run_id, receipt_json, hash, at)
                 VALUES ('run-bad', '{truncated', 'h', 'then')",
                [],
            )
            .expect("truncated receipt");
        assert!(matches!(
            ledger.receipt(&run("run-bad")),
            Err(LedgerError::Corrupt { .. })
        ));
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

    /// SPEC's task spine: a package run is not a task of its own, it
    /// inherits its root's — exercised through the public `insert_run`/
    /// `insert_child_run` API rather than a raw migration this time.
    #[test]
    fn a_package_run_inherits_its_roots_task() {
        let (ledger, dir) = temp_ledger();
        let root_task = task("shared");
        ledger
            .insert_run(&run("root-2"), "/r", None, &root_task, "rk")
            .expect("root");
        ledger
            .insert_child_run(
                &run("pkg-2"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root-2"),
                    package_id: &package("a"),
                },
                &root_task,
                "rk",
            )
            .expect("child");
        assert_eq!(
            ledger.task_of_run(&run("pkg-2")).expect("task"),
            Some(root_task.clone())
        );
        assert_eq!(
            ledger.task_of_run(&run("root-2")).expect("task"),
            Some(root_task)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `task_cost` sums a decomposed run's tree exactly once: it must
    /// not double what `run_cost`'s own recursive walk already counts,
    /// because every run in the tree shares the same task.
    #[test]
    fn task_cost_counts_a_decomposed_runs_tree_exactly_once() {
        let (ledger, dir) = temp_ledger();
        let the_task = task("dedup");
        ledger
            .insert_run(&run("root-3"), "/r", None, &the_task, "rk")
            .expect("root");
        ledger
            .insert_child_run(
                &run("pkg-3a"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root-3"),
                    package_id: &package("a"),
                },
                &the_task,
                "rk",
            )
            .expect("child a");
        ledger
            .insert_child_run(
                &run("pkg-3b"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root-3"),
                    package_id: &package("b"),
                },
                &the_task,
                "rk",
            )
            .expect("child b");
        ledger
            .record_usage(&event("e-root3", "root-3", 10))
            .expect("usage");
        ledger
            .record_usage(&event("e-pkg3a", "pkg-3a", 100))
            .expect("usage");
        ledger
            .record_usage(&event("e-pkg3b", "pkg-3b", 1000))
            .expect("usage");
        assert_eq!(
            ledger.task_cost(&the_task).expect("task cost").to_micros(),
            1110,
            "once per run, not once per (run_cost's own tree walk) x (task's runs)"
        );
        assert_eq!(
            ledger
                .run_cost(&run("root-3"))
                .expect("run cost")
                .to_micros(),
            1110,
            "run_cost's own tree walk agrees"
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

    #[test]
    fn a_root_run_costs_its_whole_tree_once() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("root"), "/r", None, &task("root"), "rk")
            .expect("root");
        ledger
            .insert_child_run(
                &run("pkg-a"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root"),
                    package_id: &package("a"),
                },
                &task("root"),
                "rk",
            )
            .expect("child");
        ledger
            .insert_child_run(
                &run("pkg-b"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root"),
                    package_id: &package("b"),
                },
                &task("root"),
                "rk",
            )
            .expect("child");
        ledger
            .record_usage(&event("e-root", "root", 10))
            .expect("usage");
        ledger
            .record_usage(&event("e-a", "pkg-a", 100))
            .expect("usage");
        ledger
            .record_usage(&event("e-b", "pkg-b", 1000))
            .expect("usage");
        assert_eq!(
            ledger.run_cost(&run("root")).expect("cost").to_micros(),
            1110
        );
        assert_eq!(
            ledger.run_cost(&run("pkg-a")).expect("cost").to_micros(),
            100
        );
        let listed = ledger
            .runs_since("2000-01-01T00:00:00+00:00")
            .expect("runs");
        assert_eq!(listed.len(), 1, "packages are not listed as separate runs");
        assert_eq!(
            ledger
                .child_runs(&run("root"))
                .expect("children")
                .iter()
                .map(|child| child.package.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_usage_events_are_deduped_not_duplicated() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-x"), "/repo", None, &task("run-x"), "rk")
            .expect("run");
        let e = event("req-1", "run-x", 500);
        assert!(ledger.record_usage(&e).expect("first"), "first insert wins");
        assert!(
            !ledger.record_usage(&e).expect("second"),
            "duplicate event_id is a no-op"
        );
        assert_eq!(
            ledger.run_cost(&run("run-x")).expect("cost"),
            MicroUsd::from_micros(500)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inclusive_parent_never_double_counts_children() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-y"), "/repo", None, &task("run-y"), "rk")
            .expect("run");
        let mut parent = event("parent", "run-y", 900);
        parent.inclusive = true;
        let mut child = event("child", "run-y", 400);
        child.parent_event_id = Some("parent".into());
        assert!(ledger.record_usage(&parent).expect("parent"));
        assert!(ledger.record_usage(&child).expect("child"));
        assert_eq!(
            ledger.run_cost(&run("run-y")).expect("cost"),
            MicroUsd::from_micros(900),
            "inclusive parent stands in for its children"
        );
        let mut standalone = event("solo", "run-y", 100);
        standalone.inclusive = true;
        assert!(ledger.record_usage(&standalone).expect("solo"));
        assert_eq!(
            ledger.run_cost(&run("run-y")).expect("cost"),
            MicroUsd::from_micros(1000)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P11: the dedup used to look one generation up, so a grandchild
    /// of an inclusive event was added to a total that already contained
    /// it.
    #[test]
    fn an_inclusive_ancestor_covers_its_whole_subtree() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-deep"), "/repo", None, &task("run-deep"), "rk")
            .expect("run");
        let mut root = event("top", "run-deep", 900);
        root.inclusive = true;
        let mut child = event("mid", "run-deep", 400);
        child.parent_event_id = Some("top".into());
        let mut grandchild = event("leaf", "run-deep", 200);
        grandchild.parent_event_id = Some("mid".into());
        for e in [&root, &child, &grandchild] {
            assert!(ledger.record_usage(e).expect("usage"));
        }
        assert_eq!(
            ledger.run_cost(&run("run-deep")).expect("cost"),
            MicroUsd::from_micros(900),
            "the inclusive total stands in for every generation below it"
        );
        assert_eq!(
            ledger
                .spend_since("2000-01-01T00:00:00+00:00")
                .expect("spend"),
            MicroUsd::from_micros(900),
            "the day's figure counts it once too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P13: `superseded_by` was a column nothing ever wrote. A new
    /// revision now closes the ones it replaces, in the same transaction
    /// that records it.
    #[test]
    fn a_new_contract_revision_supersedes_the_ones_before_it() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-rev"), "/repo", None, &task("run-rev"), "rk")
            .expect("run");
        let first = ledger
            .insert_contract_revision(&run("run-rev"), "h1", "{}", "HEAD", None)
            .expect("first");
        assert_eq!(ledger.superseded_by(first).expect("read"), None);
        let second = ledger
            .insert_contract_revision(&run("run-rev"), "h2", "{}", "HEAD", None)
            .expect("second");
        assert_eq!(ledger.superseded_by(first).expect("read"), Some(second));
        assert_eq!(ledger.superseded_by(second).expect("read"), None);
        // Another run's revisions are not touched.
        ledger
            .insert_run(&run("run-other"), "/repo", None, &task("run-other"), "rk")
            .expect("run");
        let other = ledger
            .insert_contract_revision(&run("run-other"), "h3", "{}", "HEAD", None)
            .expect("other");
        let third = ledger
            .insert_contract_revision(&run("run-rev"), "h4", "{}", "HEAD", None)
            .expect("third");
        assert_eq!(ledger.superseded_by(other).expect("read"), None);
        assert_eq!(ledger.superseded_by(second).expect("read"), Some(third));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P6: the contract and tier leave the adapter as values. A stored
    /// contract this binary cannot read is a corrupt row, not an empty
    /// objective silently fed to the learner.
    #[test]
    fn a_run_contract_comes_back_typed_or_corrupt() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-c1"), "/repo", None, &task("run-c1"), "rk")
            .expect("run");
        assert_eq!(
            ledger.run_contract_and_tier(&run("run-c1")).expect("read"),
            None,
            "a run with no revision has no contract"
        );
        let contract = r#"{"schema_version":1,"kind":"change","objective":"o",
            "base_ref":"HEAD","write_scope":["src/**"],"acceptance":["a"],
            "verification_profile":"p"}"#;
        let revision = ledger
            .insert_contract_revision(&run("run-c1"), "h", contract, "HEAD", None)
            .expect("revision");
        ledger
            .insert_attempt(
                &run("run-c1"),
                revision,
                1,
                "implementation",
                UsagePhase::Initial,
            )
            .expect("attempt");
        let (parsed, tier) = ledger
            .run_contract_and_tier(&run("run-c1"))
            .expect("read")
            .expect("present");
        assert_eq!(parsed.objective, "o");
        assert_eq!(parsed.scope_patterns(), ["src/**"]);
        assert_eq!(tier, Tier::Implementation);

        ledger
            .conn
            .execute_batch("UPDATE contract_revisions SET contract_json = '{trunc' ")
            .expect("truncate the stored contract");
        assert!(matches!(
            ledger.run_contract_and_tier(&run("run-c1")),
            Err(LedgerError::Corrupt { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completeness_degrades_to_the_worst_observed() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-c"), "/repo", None, &task("run-c"), "rk")
            .expect("run");
        assert!(ledger.record_usage(&event("a", "run-c", 10)).expect("a"));
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-c"))
                .expect("completeness"),
            CostCompleteness::Actual
        );
        let mut unknown = event("b", "run-c", 0);
        unknown.completeness = CostCompleteness::Unknown;
        assert!(ledger.record_usage(&unknown).expect("b"));
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-c"))
                .expect("completeness"),
            CostCompleteness::Unknown,
            "unknown usage is never zero and poisons the total"
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
            .record_dispatch_intent(&dispatch("disp-1"), &run("run-d"), None, &intent, 0)
            .expect("intent"));
        assert!(
            !ledger
                .record_dispatch_intent(&dispatch("disp-1"), &run("run-d"), None, &intent, 0)
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
            }]
        );
        ledger
            .finish_dispatch(&dispatch("disp-1"), "completed")
            .expect("finish");
        assert!(ledger.live_dispatches().expect("live").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transitions_set_terminal_run_status() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-t"), "/repo", None, &task("run-t"), "rk")
            .expect("run");
        assert_eq!(
            ledger.run_status(&run("run-t")).expect("status"),
            Some(State::Prepared)
        );
        ledger
            .record_transition(&Transition {
                run_id: run("run-t"),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Blocked,
                reason: crate::lifecycle::Reason::BlockedPreflight.as_str().into(),
                detail: Some(serde_json::json!({"code": "missing_trust_grant"})),
                at: now_rfc3339(),
            })
            .expect("transition");
        assert_eq!(
            ledger.run_status(&run("run-t")).expect("status"),
            Some(State::Blocked)
        );
        let history = ledger.transitions(&run("run-t")).expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].to_state, State::Blocked);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P3: `run_status` is a projection of the `transitions` table, not a
    /// column a writer could leave behind it. A run's status is the last
    /// transition's `to_state` even when an earlier write left the `runs`
    /// row's own `status` column stale — there is no code path that can
    /// do that through the public API, and this proves it: the two can
    /// never disagree because one is derived from the other.
    #[test]
    fn run_status_can_never_disagree_with_the_transition_chain() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-p"), "/repo", None, &task("run-p"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-p"),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Running,
                reason: crate::lifecycle::Reason::WorkerDispatched.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert_eq!(
            ledger.run_status(&run("run-p")).expect("status"),
            Some(State::Running),
            "status follows the last transition, not the first"
        );
        // Simulate the only way the column and the chain could ever
        // differ: something wrote the `runs` row directly, bypassing
        // `record_transition`. The chain still has the truth.
        ledger
            .conn
            .execute("UPDATE runs SET status = 'prepared' WHERE id = 'run-p'", [])
            .expect("stale column write");
        assert_eq!(
            ledger.run_status(&run("run-p")).expect("status"),
            Some(State::Running),
            "a stale `runs.status` column cannot outvote the transition chain"
        );
        ledger
            .record_transition(&Transition {
                run_id: run("run-p"),
                attempt_id: None,
                from_state: Some(State::Running),
                to_state: State::Verifying,
                reason: crate::lifecycle::Reason::VerificationStarted
                    .as_str()
                    .into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert_eq!(
            ledger.run_status(&run("run-p")).expect("status"),
            Some(State::Verifying),
            "status moves with every new transition, never lagging behind one"
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

    #[test]
    fn spend_since_sums_the_day_across_runs_and_skips_unknown() {
        let (ledger, dir) = temp_ledger();
        for run in ["run-a", "run-b"] {
            ledger
                .insert_run(
                    &super::RunId::from_stored(run),
                    "/r",
                    None,
                    &task(run),
                    "rk",
                )
                .expect("run");
        }
        let mut yesterday = event("old", "run-a", 5_000);
        yesterday.at = "2026-09-19T23:59:59+00:00".into();
        ledger.record_usage(&yesterday).expect("old");
        let mut early = event("early", "run-a", 700);
        early.at = "2026-09-20T00:00:00+00:00".into();
        ledger.record_usage(&early).expect("early");
        // Another run on the same machine, same day: a daily ceiling is
        // the machine's, not one run's.
        let mut other = event("other", "run-b", 300);
        other.at = "2026-09-20T09:00:00+00:00".into();
        ledger.record_usage(&other).expect("other");
        // Usage the provider never reported is NULL, not zero: the sum
        // skips it and the day's figure is a lower bound.
        let mut unknown = event("unknown", "run-b", 0);
        unknown.at = "2026-09-20T10:00:00+00:00".into();
        unknown.cost = None;
        unknown.completeness = CostCompleteness::Unknown;
        ledger.record_usage(&unknown).expect("unknown");

        assert_eq!(
            ledger
                .spend_since("2026-09-20T00:00:00+00:00")
                .expect("spend"),
            MicroUsd::from_micros(1_000),
            "today only, both runs, unknown left out"
        );
        assert_eq!(
            ledger
                .spend_since("2026-09-21T00:00:00+00:00")
                .expect("spend"),
            MicroUsd::ZERO,
            "a fresh day starts at zero"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    fn sample_outcome(kind: OutcomeKind) -> Outcome {
        Outcome::new(
            kind,
            OutcomeDetail {
                candidate_sha: "cand123".into(),
                strategy: crate::outcome::Strategy {
                    tier: Tier::Implementation,
                    models: vec!["sonnet".into()],
                    escalated: false,
                },
                correction_magnitude: None,
                evidence: vec!["run-a/review.txt".into()],
                actor: "a reviewer".into(),
            },
        )
        .expect("a valid outcome")
    }

    #[test]
    fn recorded_outcomes_are_read_back_typed_by_task_and_since() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-a"), "/repo", None, &task("a"), "rk")
            .expect("run");
        let outcome = sample_outcome(OutcomeKind::AcceptedUnchanged);
        ledger
            .record_outcome(&run("run-a"), &task("a"), &outcome)
            .expect("record");

        let latest = ledger
            .latest_outcome(&task("a"))
            .expect("latest")
            .expect("one outcome recorded");
        assert_eq!(latest.run_id, run("run-a"));
        assert_eq!(latest.task_id, task("a"));
        assert_eq!(latest.outcome, outcome);

        assert!(ledger
            .latest_outcome(&task("nothing-recorded"))
            .expect("latest")
            .is_none());

        let since = ledger
            .outcomes_since("2000-01-01T00:00:00+00:00")
            .expect("since");
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].outcome, outcome);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The most recently recorded outcome wins, so a later `reverted`
    /// eclipses an earlier `accepted_unchanged` for the same task.
    #[test]
    fn latest_outcome_is_the_most_recent_one_recorded() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-a"), "/repo", None, &task("a"), "rk")
            .expect("run");
        ledger
            .record_outcome(
                &run("run-a"),
                &task("a"),
                &sample_outcome(OutcomeKind::AcceptedUnchanged),
            )
            .expect("record accepted");
        ledger
            .record_outcome(
                &run("run-a"),
                &task("a"),
                &sample_outcome(OutcomeKind::Reverted),
            )
            .expect("record reverted");
        let latest = ledger
            .latest_outcome(&task("a"))
            .expect("latest")
            .expect("recorded");
        assert_eq!(latest.outcome.kind, OutcomeKind::Reverted);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P6: a stored outcome row this binary cannot read is reported, not
    /// silently dropped or defaulted.
    #[test]
    fn a_stored_outcome_this_relais_cannot_read_is_corrupt() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-a"), "/repo", None, &task("a"), "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO outcomes (run_id, task_id, candidate_sha, kind, detail_json, at)
                 VALUES ('run-a', 'task-a', 'sha', 'not-a-kind-this-relais-knows', '{}', ?1)",
                params![now_rfc3339()],
            )
            .expect("hand-inserted row");
        let err = ledger
            .latest_outcome(&task("a"))
            .expect_err("an unknown kind is corrupt, not silently absent");
        assert!(matches!(err, LedgerError::Corrupt { .. }), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
    /// A run whose second attempt is still open has no duration yet.
    /// `MAX(ended_at)` skips NULLs, so asking for it alone answers with
    /// the span of the attempts that finished — which would put a run
    /// that is still working into a median of completed ones.
    #[test]
    fn a_run_with_an_open_attempt_has_no_duration_yet() {
        let (ledger, dir) = temp_ledger();
        let run = RunId::from_stored("run-open");
        let task = TaskId::from_stored("task-open");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        let revision = ledger
            .insert_contract_revision(&run, "hash", "{}", "HEAD", Some("sha"))
            .expect("revision");
        let first = ledger
            .insert_attempt(&run, revision, 1, "implementation", UsagePhase::Initial)
            .expect("first attempt");
        ledger
            .finish_attempt(first, State::Verifying, None, None)
            .expect("finished");
        assert!(
            ledger.run_duration_seconds(&run).expect("query").is_some(),
            "one finished attempt and nothing open: the run has a span"
        );

        ledger
            .insert_attempt(&run, revision, 2, "implementation", UsagePhase::Repair)
            .expect("second attempt");
        assert_eq!(
            ledger.run_duration_seconds(&run).expect("query"),
            None,
            "the repair is still open, so the run is not done"
        );
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }
}
