//! Learning cycles and the run counts they read.

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::ids::RunId;
use crate::lifecycle::{RunPurpose, State};

use super::transitions::PROJECTED_STATUS;
use super::{Ledger, LedgerError, Result};

/// A live root run with its first dispatch intent.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveRun {
    pub run: RunId,
    pub created_at: String,
    /// The run's first dispatch intent.
    pub intent: serde_json::Value,
}

/// What a learning cycle (or a person's rollback) came to (SPEC §17).
/// Closed, so every site that reads the table's `action` decides what each
/// case means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningAction {
    /// The cycle ran and changed nothing: no artifact worth promoting.
    None,
    /// The cycle promoted the artifact it trained.
    Promoted,
    /// The artifact passed its gates and waits for `relais promote`
    /// (there is no `[learning]` envelope).
    AwaitingPromotion,
    /// The active artifact was rolled back, by the cycle or by a person.
    RolledBack,
    /// The cycle never finished: a later cycle closed its row.
    Interrupted,
    /// The cycle stopped on an error, recorded beside it.
    Error,
}

impl LearningAction {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Promoted,
        Self::AwaitingPromotion,
        Self::RolledBack,
        Self::Interrupted,
        Self::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Promoted => "promoted",
            Self::AwaitingPromotion => "awaiting_promotion",
            Self::RolledBack => "rolled_back",
            Self::Interrupted => "interrupted",
            Self::Error => "error",
        }
    }

    pub fn parse(stored: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == stored)
    }
}

/// Who started a `learning_cycles` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningSource {
    /// `relais learn cycle`, by hand or from the plugin.
    Cycle,
    /// `relais rollback`: a person's own decision.
    Person,
}

impl LearningSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cycle => "cycle",
            Self::Person => "person",
        }
    }

    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "cycle" => Some(Self::Cycle),
            "person" => Some(Self::Person),
            _ => None,
        }
    }
}

/// The evaluation a cycle ended on: the verdict, and EVERY gate that did
/// not hold, in words.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LearningVerdict {
    pub passed: bool,
    pub failed_gates: Vec<String>,
}

/// One `learning_cycles` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningCycleRow {
    pub id: i64,
    pub started_at: String,
    /// `None` until the cycle finishes; a row that stays `None` was
    /// interrupted.
    pub ended_at: Option<String>,
    pub label_policy_version: Option<u32>,
    pub dataset_fingerprint: Option<String>,
    pub examples_by_class: std::collections::BTreeMap<String, u64>,
    pub examples_by_source: std::collections::BTreeMap<String, u64>,
    pub cohorts: Vec<String>,
    pub artifact_id: Option<String>,
    pub evaluate: Option<LearningVerdict>,
    pub action: LearningAction,
    pub source: LearningSource,
    pub error: Option<String>,
    pub note: Option<String>,
}

/// What closes a `learning_cycles` row (or is the whole of a finished
/// rollback row).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LearningCycleFinish {
    pub label_policy_version: Option<u32>,
    pub dataset_fingerprint: Option<String>,
    pub examples_by_class: std::collections::BTreeMap<String, u64>,
    pub examples_by_source: std::collections::BTreeMap<String, u64>,
    pub cohorts: Vec<String>,
    pub artifact_id: Option<String>,
    pub evaluate: Option<LearningVerdict>,
    pub action: Option<LearningAction>,
    pub error: Option<String>,
    pub note: Option<String>,
}

const LEARNING_COLUMNS: &str = "id, started_at, ended_at, label_policy_version, \
    dataset_fingerprint, examples_by_class_json, examples_by_source_json, cohorts_json, \
    artifact_id, evaluate_json, action, source, error, note";

fn learning_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LearningCycleRow> {
    // Parsed after the query: a corrupt column is the caller's error, not
    // a rusqlite one.
    let text = |index| row.get::<_, Option<String>>(index);
    Ok(LearningCycleRow {
        id: row.get(0)?,
        started_at: row.get(1)?,
        ended_at: row.get(2)?,
        label_policy_version: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
        dataset_fingerprint: row.get(4)?,
        examples_by_class: serde_json::from_str(&text(5)?.unwrap_or_default()).unwrap_or_default(),
        examples_by_source: serde_json::from_str(&text(6)?.unwrap_or_default()).unwrap_or_default(),
        cohorts: serde_json::from_str(&text(7)?.unwrap_or_default()).unwrap_or_default(),
        artifact_id: row.get(8)?,
        evaluate: text(9)?.and_then(|stored| serde_json::from_str(&stored).ok()),
        // A name this relais does not know reads as an error row rather
        // than failing the whole report: the row is real, its meaning is
        // another version's.
        action: LearningAction::parse(&row.get::<_, String>(10)?).unwrap_or(LearningAction::Error),
        source: LearningSource::parse(&row.get::<_, String>(11)?).unwrap_or(LearningSource::Cycle),
        error: row.get(12)?,
        note: row.get(13)?,
    })
}

impl Ledger {
    /// Start a cycle: the row exists, with `ended_at` NULL, before any
    /// work is done, so a cycle that is killed leaves a trace.
    pub fn begin_learning_cycle(&self) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO learning_cycles (started_at, action, source) VALUES (?1, 'none', 'cycle')",
            [self.now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Close every cycle row left unfinished by a process that died, except
    /// `keep` (the cycle now running). Only the holder of the Training
    /// lease calls it, so an unfinished row is never a live cycle's.
    pub fn close_unfinished_learning_cycles(&self, keep: i64) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE learning_cycles SET ended_at = ?1, action = 'interrupted'
             WHERE ended_at IS NULL AND id != ?2",
            params![self.now(), keep],
        )?)
    }

    /// Finish a cycle row. A row already closed is left alone.
    pub fn finish_learning_cycle(&self, id: i64, finish: &LearningCycleFinish) -> Result<()> {
        self.conn.execute(
            "UPDATE learning_cycles SET ended_at = ?2, label_policy_version = ?3,
                    dataset_fingerprint = ?4, examples_by_class_json = ?5,
                    examples_by_source_json = ?6, cohorts_json = ?7, artifact_id = ?8,
                    evaluate_json = ?9, action = ?10, error = ?11, note = ?12
             WHERE id = ?1 AND ended_at IS NULL",
            params![
                id,
                self.now(),
                finish.label_policy_version.map(i64::from),
                finish.dataset_fingerprint,
                serde_json::to_string(&finish.examples_by_class).expect("counts serialize"),
                serde_json::to_string(&finish.examples_by_source).expect("counts serialize"),
                serde_json::to_string(&finish.cohorts).expect("cohorts serialize"),
                finish.artifact_id,
                finish
                    .evaluate
                    .as_ref()
                    .map(|v| serde_json::to_string(v).expect("verdict serializes")),
                finish.action.unwrap_or(LearningAction::None).as_str(),
                finish.error,
                finish.note,
            ],
        )?;
        Ok(())
    }

    /// A finished row written in one step: a rollback, by the cycle or by
    /// a person.
    pub fn record_learning_rollback(
        &self,
        source: LearningSource,
        finish: &LearningCycleFinish,
    ) -> Result<i64> {
        let now = self.now();
        self.conn.execute(
            "INSERT INTO learning_cycles
                (started_at, ended_at, label_policy_version, dataset_fingerprint,
                 examples_by_class_json, examples_by_source_json, cohorts_json,
                 artifact_id, evaluate_json, action, source, error, note)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'rolled_back', ?9, ?10, ?11)",
            params![
                now,
                finish.label_policy_version.map(i64::from),
                finish.dataset_fingerprint,
                serde_json::to_string(&finish.examples_by_class).expect("counts serialize"),
                serde_json::to_string(&finish.examples_by_source).expect("counts serialize"),
                serde_json::to_string(&finish.cohorts).expect("cohorts serialize"),
                finish.artifact_id,
                finish
                    .evaluate
                    .as_ref()
                    .map(|v| serde_json::to_string(v).expect("verdict serializes")),
                source.as_str(),
                finish.error,
                finish.note,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn learning_rows(&self, filter: &str) -> Result<Vec<LearningCycleRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {LEARNING_COLUMNS} FROM learning_cycles {filter}"
        ))?;
        let rows = stmt.query_map([], learning_row)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The most recent `learning_cycles` row of any kind.
    pub fn last_learning_cycle(&self) -> Result<Option<LearningCycleRow>> {
        Ok(self
            .learning_rows("ORDER BY id DESC LIMIT 1")?
            .into_iter()
            .next())
    }

    /// The most recent row a cycle (not a person) started, of any state:
    /// finished, unfinished or closed as `interrupted`. What the report
    /// shows.
    pub fn last_cycle_started(&self) -> Result<Option<LearningCycleRow>> {
        Ok(self
            .learning_rows("WHERE source = 'cycle' ORDER BY id DESC LIMIT 1")?
            .into_iter()
            .next())
    }

    /// The most recent cycle row that finished, a rollback included: what
    /// `--if-due` measures its 20 hours from. A row that never finished (a
    /// killed cycle) or that the next cycle closed as `interrupted` does not
    /// count: it must never make `--if-due` answer "not due".
    pub fn last_cycle_for_due_check(&self) -> Result<Option<LearningCycleRow>> {
        Ok(self
            .learning_rows(
                "WHERE source = 'cycle' AND ended_at IS NOT NULL AND action != 'interrupted'
                 ORDER BY id DESC LIMIT 1",
            )?
            .into_iter()
            .next())
    }

    /// Every rollback row, newest first.
    pub fn learning_rollbacks(&self) -> Result<Vec<LearningCycleRow>> {
        self.learning_rows("WHERE action = 'rolled_back' ORDER BY id DESC")
    }

    /// Every row, newest first, for tests and the report.
    pub fn learning_cycles(&self) -> Result<Vec<LearningCycleRow>> {
        self.learning_rows("ORDER BY id DESC")
    }

    /// The last row that finished with a trained dataset: what the report
    /// counts labelled examples from.
    pub fn last_learning_dataset_row(&self) -> Result<Option<LearningCycleRow>> {
        Ok(self
            .learning_rows(
                "WHERE dataset_fingerprint IS NOT NULL AND source = 'cycle' ORDER BY id DESC LIMIT 1",
            )?
            .into_iter()
            .next())
    }

    /// Root runs that reached a terminal state after `since` (RFC3339): the
    /// evidence a new cycle has something to learn from, and the count the
    /// anti-flap rule waits on.
    pub fn terminal_root_runs_since(&self, since: &str) -> Result<usize> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {PROJECTED_STATUS},
                    COALESCE((SELECT MAX(t.at) FROM transitions t WHERE t.run_id = runs.id),
                             runs.updated_at) AS last_at
               FROM runs WHERE parent_run IS NULL"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut count = 0;
        for row in rows {
            let (status, last_at) = row?;
            let state = State::parse(&status).map_err(|e| LedgerError::Corrupt {
                what: "run state".into(),
                detail: e.to_string(),
            })?;
            if state.is_terminal() && last_at.as_str() > since {
                count += 1;
            }
        }
        Ok(count)
    }

    /// Runs whose routing prediction came from artifact `artifact_id` and
    /// whose first dispatch was `routed_by = learned_artifact`.
    pub fn runs_routed_by_artifact(&self, artifact_id: &str) -> Result<Vec<RunId>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT p.run_id FROM predictions p
               JOIN dispatches d ON d.run_id = p.run_id
              WHERE p.artifact_id = ?1 AND d.routed_by = 'learned_artifact'
              ORDER BY p.run_id",
        )?;
        let rows = stmt.query_map([artifact_id], |row| row.get::<_, String>(0))?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(RunId::from_stored)
            .collect())
    }

    /// Every root run created at or after `since` that is not a replay and
    /// has a dispatch, oldest first, with its first dispatch intent: the
    /// live runs a recipe routed.
    // holds-until: the runs table stays in the tens of thousands; this
    // reads every live run since `since` and filters in Rust. A selective
    // index on the intent's recipe id ends it.
    pub fn live_runs_with_first_intent(&self, since: &str) -> Result<Vec<LiveRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.created_at,
                    (SELECT d.intent_json FROM dispatches d WHERE d.run_id = r.id
                      ORDER BY d.created_at, d.dispatch_id LIMIT 1)
               FROM runs r
              WHERE r.parent_run IS NULL AND COALESCE(r.purpose, '') <> ?2
                AND r.created_at >= ?1
              ORDER BY r.created_at, r.id",
        )?;
        let rows = stmt.query_map(params![since, RunPurpose::Replay.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let rows: Vec<_> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let mut out = Vec::new();
        for (id, created_at, text) in rows {
            let Some(text) = text else { continue };
            let intent = serde_json::from_str(&text).map_err(|e| LedgerError::Corrupt {
                what: format!("the dispatch intent of run {id}"),
                detail: e.to_string(),
            })?;
            out.push(LiveRun {
                run: RunId::from_stored(id),
                created_at,
                intent,
            });
        }
        Ok(out)
    }

    /// `(runs with a recorded route, of which routed by a learned
    /// artifact)` among runs dispatched at or after `since`.
    pub fn learned_routing_share_since(&self, since: &str) -> Result<(u64, u64)> {
        let (total, learned): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(DISTINCT run_id),
                    COUNT(DISTINCT CASE WHEN routed_by = 'learned_artifact' THEN run_id END)
               FROM dispatches WHERE routed_by IS NOT NULL AND created_at >= ?1",
            [since],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((total as u64, learned as u64))
    }
}
