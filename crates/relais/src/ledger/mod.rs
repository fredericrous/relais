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

pub const LEDGER_SCHEMA_VERSION: u64 = 1;

#[derive(Debug)]
pub enum LedgerError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Sqlite(e) => write!(f, "ledger: {e}"),
            LedgerError::Io(e) => write!(f, "ledger: {e}"),
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

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Additive migrations, in order. Existing steps are never edited; a new
/// step appends. `schema_migrations` records what applied.
const MIGRATIONS: &[(&str, &str)] = &[(
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
)];

pub struct Ledger {
    conn: Connection,
    path: std::path::PathBuf,
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
    pub cost: MicroUsd,
    pub cost_kind: CostKind,
    pub completeness: CostCompleteness,
    /// True when this event's total already includes its descendants:
    /// an inclusive parent is never added to its children (SPEC §11).
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
        };
        ledger.migrate()?;
        Ok(ledger)
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
                    params![version, now_rfc3339()],
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
        let now = now_rfc3339();
        self.conn.execute(
            "INSERT INTO runs (id, repo_path, status, root_session, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![id, repo_path, State::Prepared.as_str(), root_session, now],
        )?;
        Ok(())
    }

    pub fn set_run_status(&self, id: &str, status: State) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, status.as_str(), now_rfc3339()],
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
        Ok(status.and_then(|s| State::parse(&s)))
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
            params![
                run_id,
                hash,
                contract_json,
                base_ref,
                base_sha,
                now_rfc3339()
            ],
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
                now_rfc3339()
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
                now_rfc3339(),
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
        if transition.to_state.is_terminal() {
            self.set_run_status(&transition.run_id, transition.to_state)?;
        }
        Ok(())
    }

    pub fn transitions(&self, run_id: &str) -> Result<Vec<Transition>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, attempt_id, from_state, to_state, reason, detail_json, at
             FROM transitions WHERE run_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([run_id], |row| {
            let from_state: Option<String> = row.get(2)?;
            let detail: Option<String> = row.get(5)?;
            Ok(Transition {
                run_id: row.get(0)?,
                attempt_id: row.get(1)?,
                from_state: from_state.and_then(|s| State::parse(&s)),
                to_state: State::parse(&row.get::<_, String>(3)?)
                    .expect("ledger only stores valid states"),
                reason: row.get(4)?,
                detail: detail.and_then(|d| serde_json::from_str(&d).ok()),
                at: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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
                event.cost.to_micros(),
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
        let now = now_rfc3339();
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
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn finish_dispatch(&self, dispatch_id: &str, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE dispatches SET state = ?2, updated_at = ?3 WHERE dispatch_id = ?1",
            params![dispatch_id, state, now_rfc3339()],
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

    /// Aggregate cost for a run. Inclusive parent totals are alternative
    /// aggregation sources (SPEC §11): an inclusive event already
    /// contains its descendants, so a child attributed to an inclusive
    /// parent is excluded and the parent is counted once. An inclusive
    /// event with no children simply counts itself.
    pub fn run_cost(&self, run_id: &str) -> Result<MicroUsd> {
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
        let values: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT completeness FROM usage_events WHERE run_id = ?1")?;
            let rows = stmt.query_map([run_id], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut completeness = CostCompleteness::Actual;
        for value in values {
            let parsed: CostCompleteness =
                serde_json::from_str(&value).unwrap_or(CostCompleteness::Unknown);
            completeness = match (completeness, parsed) {
                (_, CostCompleteness::Unknown) | (CostCompleteness::Unknown, _) => {
                    CostCompleteness::Unknown
                }
                (_, CostCompleteness::IncompleteLowerBound)
                | (CostCompleteness::IncompleteLowerBound, _) => {
                    CostCompleteness::IncompleteLowerBound
                }
                (_, CostCompleteness::Estimated) | (CostCompleteness::Estimated, _) => {
                    CostCompleteness::Estimated
                }
                (CostCompleteness::Actual, CostCompleteness::Actual) => CostCompleteness::Actual,
            };
        }
        Ok(completeness)
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
                now_rfc3339()
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
        Ok(row.map(|(json, hash)| {
            (
                serde_json::from_str(&json).expect("ledger stores valid receipts"),
                hash,
            )
        }))
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
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn record_features(&self, dispatch_id: &str, features: &serde_json::Value) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO features (dispatch_id, feature_json, at) VALUES (?1, ?2, ?3)",
            params![
                dispatch_id,
                serde_json::to_string(features).expect("features serialize"),
                now_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// All runs created at or after `since` (RFC3339), newest first.
    pub fn runs_since(&self, since: &str) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_path, status, created_at FROM runs
             WHERE created_at >= ?1 ORDER BY created_at DESC",
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
            cost: MicroUsd::from_micros(micros),
            cost_kind: CostKind::ApiSpend,
            completeness: CostCompleteness::Actual,
            inclusive: false,
            at: now_rfc3339(),
        }
    }

    #[test]
    fn migrations_are_idempotent_and_additive() {
        let (ledger, dir) = temp_ledger();
        assert_eq!(ledger.schema_version().expect("count"), 1);
        drop(ledger);
        let reopened = Ledger::open(&dir.join("ledger.sqlite")).expect("reopen");
        assert_eq!(
            reopened.schema_version().expect("count"),
            1,
            "no double-apply"
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
                reason: crate::runner::reason::BLOCKED_PREFLIGHT.into(),
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
