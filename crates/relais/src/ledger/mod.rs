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

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::runner::State;

pub const LEDGER_SCHEMA_VERSION: u64 = 3;

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
    State::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "run state".into(),
        detail: format!("`{stored}` is not a state this relais knows"),
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
        let mut guard = self.times.lock().expect("clock lock");
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
];

pub struct Ledger {
    conn: Connection,
    path: std::path::PathBuf,
    clock: Box<dyn Clock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub event_id: String,
    pub run_id: String,
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
    /// exists. The dedup in `run_cost` is ready for it.
    pub inclusive: bool,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub run_id: String,
    pub attempt_id: Option<i64>,
    pub from_state: Option<State>,
    pub to_state: State,
    pub reason: String,
    pub detail: Option<serde_json::Value>,
    pub at: String,
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
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // Several processes and threads share one ledger in short
        // transactions (SPEC §23); a writer in progress is a wait, not
        // an error.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let ledger = Ledger {
            conn,
            path: path.to_path_buf(),
            clock,
        };
        ledger.migrate()?;
        Ok(ledger)
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

    fn migrate(&self) -> Result<()> {
        self.conn.execute(
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
        let applied_count = self.schema_version()?;
        if applied_count > LEDGER_SCHEMA_VERSION {
            return Err(LedgerError::SchemaAhead {
                found: applied_count,
                known: LEDGER_SCHEMA_VERSION,
            });
        }
        for (version, sql) in MIGRATIONS {
            let applied: Option<String> = self
                .conn
                .query_row(
                    "SELECT version FROM schema_migrations WHERE version = ?1",
                    [version],
                    |row| row.get(0),
                )
                .optional()?;
            if applied.is_none() {
                self.conn.execute_batch(sql)?;
                self.conn.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                    params![version, self.now()],
                )?;
            }
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<u64> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                    row.get(0)
                })?;
        Ok(count as u64)
    }

    pub fn insert_run(&self, id: &str, repo_path: &str, root_session: Option<&str>) -> Result<()> {
        let now = self.now();
        self.conn.execute(
            "INSERT INTO runs (id, repo_path, status, root_session, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, repo_path, State::Prepared.as_str(), root_session, now],
        )?;
        Ok(())
    }

    /// A work package's run: its own lifecycle, attributed to the root.
    pub fn insert_child_run(
        &self,
        id: &str,
        repo_path: &str,
        root_session: Option<&str>,
        parent_run: &str,
        package_id: &str,
    ) -> Result<()> {
        let now = self.now();
        self.conn.execute(
            "INSERT INTO runs (id, repo_path, status, root_session, created_at, updated_at,
                               parent_run, package_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
            params![
                id,
                repo_path,
                State::Prepared.as_str(),
                root_session,
                now,
                parent_run,
                package_id
            ],
        )?;
        Ok(())
    }

    /// (run_id, package_id, status) of a root run's packages, in
    /// creation order.
    pub fn child_runs(&self, parent_run: &str) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, package_id, status FROM runs WHERE parent_run = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([parent_run], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Every dispatch ever recorded for a run: the aggregate agent count
    /// no work package resets (SPEC §19).
    pub fn dispatch_count(&self, run_id: &str) -> Result<u32> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM dispatches WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(count as u32)
    }

    pub fn set_run_status(&self, id: &str, status: State) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), self.now()],
        )?;
        Ok(())
    }

    pub fn run_status(&self, id: &str) -> Result<Option<State>> {
        let status: Option<String> = self
            .conn
            .query_row("SELECT status FROM runs WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        status.map(|s| parse_state(&s)).transpose()
    }

    pub fn insert_contract_revision(
        &self,
        run_id: &str,
        hash: &str,
        contract_json: &str,
        base_ref: &str,
        base_sha: Option<&str>,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO contract_revisions
                (run_id, hash, contract_json, base_ref, base_sha, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![run_id, hash, contract_json, base_ref, base_sha, self.now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_attempt(
        &self,
        run_id: &str,
        revision_id: i64,
        attempt_index: i64,
        tier: &str,
        phase: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO attempts
                (run_id, revision_id, attempt_index, tier, phase, state, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run_id,
                revision_id,
                attempt_index,
                tier,
                phase,
                State::Running.as_str(),
                self.now()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Distinct models that actually ran for a run, from usage events.
    pub fn models_used(&self, run_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT model FROM usage_events
             WHERE run_id = ?1 AND model IS NOT NULL ORDER BY model",
        )?;
        let rows = stmt.query_map([run_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The stored contract JSON and the first attempt's tier for a run —
    /// what dataset construction needs to reconstruct dispatch-time
    /// features without future information (SPEC §21).
    pub fn run_contract_and_tier(&self, run_id: &str) -> Result<Option<(String, String, String)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT revisions.contract_json, attempts.tier
                 FROM contract_revisions revisions
                 JOIN attempts ON attempts.run_id = revisions.run_id
                 WHERE revisions.run_id = ?1
                 ORDER BY revisions.id, attempts.id
                 LIMIT 1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(contract_json, tier)| {
            let value: serde_json::Value =
                serde_json::from_str(&contract_json).unwrap_or(serde_json::Value::Null);
            let objective = value
                .get("objective")
                .and_then(|objective| objective.as_str())
                .unwrap_or_default()
                .to_string();
            (contract_json, objective, tier)
        }))
    }

    /// How many worker attempts a run consumed — the attempts table is
    /// the source of truth for the ladder.
    /// Did the ladder move? True when any attempt of the run ran at the
    /// `escalation` phase — the label source for "accepted WITHOUT
    /// escalation" (SPEC §17). Not derived from the models seen: the
    /// reviewer and the planner run on their own models without any
    /// escalation having happened.
    pub fn escalation_attempted(&self, run_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE run_id = ?1 AND phase = 'escalation'",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn attempt_count(&self, run_id: &str) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
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

    pub fn record_transition(&self, transition: &Transition) -> Result<()> {
        self.conn.execute(
            "INSERT INTO transitions
                (run_id, attempt_id, from_state, to_state, reason, detail_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                transition.run_id,
                transition.attempt_id,
                transition.from_state.map(|s| s.as_str()),
                transition.to_state.as_str(),
                transition.reason,
                transition
                    .detail
                    .as_ref()
                    .map(|d| serde_json::to_string(d).unwrap_or_default()),
                transition.at,
            ],
        )?;
        // Every transition, not only the terminal ones: `relais status`
        // used to say `prepared` for a run ten minutes into verifying.
        self.set_run_status(&transition.run_id, transition.to_state)?;
        Ok(())
    }

    /// One piece of evidence bound to a run: the context manifest, a
    /// candidate patch, a check log, the receipt — by path and content
    /// hash (SPEC §12: "every transition has … evidence references").
    pub fn record_evidence(
        &self,
        run_id: &str,
        attempt_id: Option<i64>,
        kind: &str,
        path: &Path,
        sha256: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO evidence (run_id, attempt_id, kind, path, sha256, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id,
                attempt_id,
                kind,
                path.to_string_lossy(),
                sha256,
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn evidence(&self, run_id: &str) -> Result<Vec<(String, String, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT kind, path, sha256 FROM evidence WHERE run_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn transitions(&self, run_id: &str) -> Result<Vec<Transition>> {
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
        let rows = stmt.query_map([run_id], |row| {
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
                        run_id,
                        attempt_id,
                        from_state: from_state.as_deref().map(parse_state).transpose()?,
                        to_state: parse_state(&to_state)?,
                        reason,
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
                 cost_micros, cost_kind, completeness, inclusive, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                event.event_id,
                event.run_id,
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
            ],
        )?;
        Ok(inserted == 1)
    }

    /// Dispatch intent is persisted BEFORE the process exists (SPEC §12).
    /// A retry with the same dispatch ID is a no-op, not a duplicate.
    pub fn record_dispatch_intent(
        &self,
        dispatch_id: &str,
        run_id: &str,
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
                dispatch_id,
                run_id,
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
        dispatch_id: &str,
        pid: Option<u32>,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE dispatches SET pid = ?2, session_id = ?3, state = ?4, updated_at = ?5
             WHERE dispatch_id = ?1",
            params![
                dispatch_id,
                pid.map(|pid| pid as i64),
                session_id,
                "launched",
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn finish_dispatch(&self, dispatch_id: &str, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE dispatches SET state = ?2, updated_at = ?3 WHERE dispatch_id = ?1",
            params![dispatch_id, state, self.now()],
        )?;
        Ok(())
    }

    /// Dispatches that claimed to launch but never recorded a terminal
    /// state — the reconciliation set after a crash or restart. An absent
    /// terminal result never means nothing executed (SPEC §12).
    pub fn live_dispatches(&self) -> Result<Vec<(String, String, Option<i64>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT dispatch_id, run_id, pid FROM dispatches WHERE state = 'launched'")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Aggregate cost for a run: the sum of what was REPORTED. Unknown
    /// usage is NULL and `SUM` skips it, so the figure is a lower bound
    /// whenever `run_cost_completeness` says `unknown` — read the two
    /// together (`report::cost_line` does). Inclusive parent totals are
    /// alternative aggregation sources (SPEC §11): an inclusive event
    /// already contains its descendants, so a child attributed to an
    /// inclusive parent is excluded and the parent is counted once. An
    /// inclusive event with no children simply counts itself.
    pub fn run_cost(&self, run_id: &str) -> Result<MicroUsd> {
        let mut total = self.run_own_cost(run_id)?;
        // A root run's cost is its tree's: packages are separate runs
        // attributed to it (SPEC §19, §23), each counted once.
        for (child, _, _) in self.child_runs(run_id)? {
            total = total.saturating_add(self.run_cost(&child)?);
        }
        Ok(total)
    }

    fn run_own_cost(&self, run_id: &str) -> Result<MicroUsd> {
        let micros: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost_micros), 0) FROM usage_events
             WHERE run_id = ?1
               AND NOT EXISTS (
                     SELECT 1 FROM usage_events parent
                     WHERE parent.run_id = usage_events.run_id
                       AND parent.event_id = usage_events.parent_event_id
                       AND parent.inclusive = 1)",
            [run_id],
            |row| row.get(0),
        )?;
        Ok(MicroUsd::from_micros(micros))
    }

    /// Worst-case completeness for the run's recorded usage: an unknown
    /// anywhere makes the run's cost unknown; an incomplete anywhere makes
    /// it an incomplete lower bound.
    pub fn run_cost_completeness(&self, run_id: &str) -> Result<CostCompleteness> {
        let mut values: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT completeness FROM usage_events WHERE run_id = ?1")?;
            let rows = stmt.query_map([run_id], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (child, _, _) in self.child_runs(run_id)? {
            values.push(
                serde_json::to_string(&self.run_cost_completeness(&child)?).expect("serializes"),
            );
        }
        Ok(CostCompleteness::worst(values.iter().map(|value| {
            serde_json::from_str(value).unwrap_or(CostCompleteness::Unknown)
        })))
    }

    pub fn store_receipt(
        &self,
        run_id: &str,
        receipt_json: &serde_json::Value,
        hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO receipts (run_id, receipt_json, hash, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                run_id,
                serde_json::to_string(receipt_json).expect("receipt serializes"),
                hash,
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn receipt(&self, run_id: &str) -> Result<Option<(serde_json::Value, String)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT receipt_json, hash FROM receipts WHERE run_id = ?1",
                [run_id],
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

    pub fn record_outcome(
        &self,
        run_id: &str,
        kind: &str,
        detail: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO outcomes (run_id, kind, detail_json, at) VALUES (?1, ?2, ?3, ?4)",
            params![
                run_id,
                kind,
                detail.map(|d| serde_json::to_string(d).expect("detail serializes")),
                self.now()
            ],
        )?;
        Ok(())
    }

    /// The intent recorded for a run's first dispatch: model, effort and
    /// harness identity as they were at dispatch time (SPEC §21: features
    /// are reconstructed without future information).
    pub fn first_dispatch_intent(&self, run_id: &str) -> Result<Option<serde_json::Value>> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT intent_json FROM dispatches WHERE run_id = ?1
                 ORDER BY created_at, dispatch_id LIMIT 1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(text.and_then(|text| serde_json::from_str(&text).ok()))
    }

    /// What a learned artifact estimated for a run at routing time, so a
    /// report can compare the estimate with what happened (SPEC §16).
    pub fn record_prediction(
        &self,
        run_id: &str,
        artifact_id: &str,
        input_hash: &str,
        result: &serde_json::Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO predictions (run_id, artifact_id, input_hash, result_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run_id,
                artifact_id,
                input_hash,
                serde_json::to_string(result).expect("prediction serializes"),
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn predictions(&self, run_id: &str) -> Result<Vec<(String, String, serde_json::Value)>> {
        let mut stmt = self.conn.prepare(
            "SELECT artifact_id, input_hash, result_json FROM predictions WHERE run_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            let text: String = row.get(2)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn record_features(&self, dispatch_id: &str, features: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO features (dispatch_id, feature_json, at) VALUES (?1, ?2, ?3)",
            params![
                dispatch_id,
                serde_json::to_string(features).expect("features serialize"),
                self.now()
            ],
        )?;
        Ok(())
    }

    /// All root runs created at or after `since` (RFC3339), newest first.
    /// Package runs are folded into their root's cost and are not listed
    /// twice.
    pub fn runs_since(&self, since: &str) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_path, status, created_at FROM runs
             WHERE created_at >= ?1 AND parent_run IS NULL ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([since], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_ledger() -> (Ledger, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "relais-ledger-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("ledger.sqlite");
        let ledger = Ledger::open(&path).expect("ledger opens");
        (ledger, dir)
    }

    fn event(event_id: &str, run_id: &str, micros: i64) -> UsageEvent {
        UsageEvent {
            event_id: event_id.into(),
            run_id: run_id.into(),
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
        }
    }

    #[test]
    fn unknown_usage_is_null_left_out_of_the_sum_and_poisons_completeness() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-u", "/r", None).expect("run");
        ledger
            .record_usage(&event("known", "run-u", 700))
            .expect("known");
        let mut unknown = event("unknown", "run-u", 0);
        unknown.cost = None;
        unknown.completeness = CostCompleteness::Unknown;
        ledger.record_usage(&unknown).expect("unknown");
        assert_eq!(
            ledger.run_cost("run-u").expect("cost"),
            MicroUsd::from_micros(700),
            "the reported part, a lower bound"
        );
        assert_eq!(
            ledger.run_cost_completeness("run-u").expect("completeness"),
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
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v3_turns_the_zeros_that_stood_for_unknown_into_null() {
        let dir = std::env::temp_dir().join(format!("relais-ledger-v3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
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
        assert_eq!(ledger.schema_version().expect("version"), 3);
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
            ledger.run_cost("run-old").expect("cost"),
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
        ledger.insert_run("run-future", "/repo", None).expect("run");
        ledger
            .record_transition(&Transition {
                run_id: "run-future".into(),
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

        match ledger.run_status("run-future") {
            Err(LedgerError::Corrupt { what, detail }) => {
                assert_eq!(what, "run state");
                assert!(detail.contains("hibernating"), "{detail}");
            }
            other => panic!("an unknown state must be an error, got {other:?}"),
        }
        assert!(
            matches!(
                ledger.transitions("run-future"),
                Err(LedgerError::Corrupt { .. })
            ),
            "reading the history of that run is an error too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unparseable_receipt_is_an_error_not_a_panic() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-bad", "/repo", None).expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO receipts (run_id, receipt_json, hash, at)
                 VALUES ('run-bad', '{truncated', 'h', 'then')",
                [],
            )
            .expect("truncated receipt");
        assert!(matches!(
            ledger.receipt("run-bad"),
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
            upgraded.run_status("old-run").expect("status"),
            Some(State::Accepted),
            "existing rows survive the additive migration"
        );
        assert!(upgraded.child_runs("old-run").expect("children").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_injected_clock_stamps_every_record() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "relais-ledger-clock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
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
        ledger.insert_run("run-1", "/r", None).expect("run");
        ledger
            .record_transition(&Transition {
                run_id: "run-1".into(),
                attempt_id: None,
                from_state: None,
                to_state: State::Running,
                reason: "plan_accepted".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
        let at = ledger.transitions("run-1").expect("transitions")[0]
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
        ledger.insert_run("root", "/r", None).expect("root");
        ledger
            .insert_child_run("pkg-a", "/r", None, "root", "a")
            .expect("child");
        ledger
            .insert_child_run("pkg-b", "/r", None, "root", "b")
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
        assert_eq!(ledger.run_cost("root").expect("cost").to_micros(), 1110);
        assert_eq!(ledger.run_cost("pkg-a").expect("cost").to_micros(), 100);
        let listed = ledger
            .runs_since("2000-01-01T00:00:00+00:00")
            .expect("runs");
        assert_eq!(listed.len(), 1, "packages are not listed as separate runs");
        assert_eq!(
            ledger
                .child_runs("root")
                .expect("children")
                .iter()
                .map(|(_, package, _)| package.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_usage_events_are_deduped_not_duplicated() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-x", "/repo", None).expect("run");
        let e = event("req-1", "run-x", 500);
        assert!(ledger.record_usage(&e).expect("first"), "first insert wins");
        assert!(
            !ledger.record_usage(&e).expect("second"),
            "duplicate event_id is a no-op"
        );
        assert_eq!(
            ledger.run_cost("run-x").expect("cost"),
            MicroUsd::from_micros(500)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inclusive_parent_never_double_counts_children() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-y", "/repo", None).expect("run");
        let mut parent = event("parent", "run-y", 900);
        parent.inclusive = true;
        let mut child = event("child", "run-y", 400);
        child.parent_event_id = Some("parent".into());
        assert!(ledger.record_usage(&parent).expect("parent"));
        assert!(ledger.record_usage(&child).expect("child"));
        assert_eq!(
            ledger.run_cost("run-y").expect("cost"),
            MicroUsd::from_micros(900),
            "inclusive parent stands in for its children"
        );
        let mut standalone = event("solo", "run-y", 100);
        standalone.inclusive = true;
        assert!(ledger.record_usage(&standalone).expect("solo"));
        assert_eq!(
            ledger.run_cost("run-y").expect("cost"),
            MicroUsd::from_micros(1000)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completeness_degrades_to_the_worst_observed() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-c", "/repo", None).expect("run");
        assert!(ledger.record_usage(&event("a", "run-c", 10)).expect("a"));
        assert_eq!(
            ledger.run_cost_completeness("run-c").expect("completeness"),
            CostCompleteness::Actual
        );
        let mut unknown = event("b", "run-c", 0);
        unknown.completeness = CostCompleteness::Unknown;
        assert!(ledger.record_usage(&unknown).expect("b"));
        assert_eq!(
            ledger.run_cost_completeness("run-c").expect("completeness"),
            CostCompleteness::Unknown,
            "unknown usage is never zero and poisons the total"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dispatch_intent_persists_before_the_process_exists() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-d", "/repo", None).expect("run");
        let intent = serde_json::json!({"model": "sonnet", "effort": "medium"});
        assert!(ledger
            .record_dispatch_intent("disp-1", "run-d", None, &intent, 0)
            .expect("intent"));
        assert!(
            !ledger
                .record_dispatch_intent("disp-1", "run-d", None, &intent, 0)
                .expect("retry"),
            "same dispatch ID cannot create a duplicate"
        );
        ledger
            .attach_dispatch_process("disp-1", Some(4242), Some("sess-1"))
            .expect("attach");
        let live = ledger.live_dispatches().expect("live");
        assert_eq!(live, vec![("disp-1".into(), "run-d".into(), Some(4242))]);
        ledger
            .finish_dispatch("disp-1", "completed")
            .expect("finish");
        assert!(ledger.live_dispatches().expect("live").is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transitions_set_terminal_run_status() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-t", "/repo", None).expect("run");
        assert_eq!(
            ledger.run_status("run-t").expect("status"),
            Some(State::Prepared)
        );
        ledger
            .record_transition(&Transition {
                run_id: "run-t".into(),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Blocked,
                reason: crate::runner::Reason::BlockedPreflight.as_str().into(),
                detail: Some(serde_json::json!({"code": "missing_trust_grant"})),
                at: now_rfc3339(),
            })
            .expect("transition");
        assert_eq!(
            ledger.run_status("run-t").expect("status"),
            Some(State::Blocked)
        );
        let history = ledger.transitions("run-t").expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].to_state, State::Blocked);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn receipts_are_stored_by_the_runner_only() {
        let (ledger, dir) = temp_ledger();
        ledger.insert_run("run-r", "/repo", None).expect("run");
        assert!(ledger.receipt("run-r").expect("receipt").is_none());
        let receipt = serde_json::json!({"run_id": "run-r", "outcome": "accepted"});
        ledger
            .store_receipt("run-r", &receipt, "hash123")
            .expect("store");
        let (stored, hash) = ledger.receipt("run-r").expect("receipt").expect("present");
        assert_eq!(stored["outcome"], "accepted");
        assert_eq!(hash, "hash123");
        std::fs::remove_dir_all(&dir).ok();
    }
}
