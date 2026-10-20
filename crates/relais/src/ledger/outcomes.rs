//! Typed final outcomes and the reverts found for them.

use std::path::PathBuf;

use rusqlite::{params, OptionalExtension};

use crate::ids::{RunId, TaskId};
use crate::lifecycle::{Reason, RunPurpose, State};
use crate::outcome::{Outcome, OutcomeDetail, OutcomeKind};

use super::{Ledger, LedgerError, Result};

/// A typed outcome as `latest_outcome`/`outcomes_since` hand it back:
/// the row's identity plus the [`Outcome`] it parses to.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredOutcome {
    pub run_id: RunId,
    pub task_id: TaskId,
    pub outcome: Outcome,
    pub at: String,
}

/// A run whose accepted change may have landed and been undone since.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertCandidate {
    pub run: RunId,
    pub task: TaskId,
    /// Where the run ran; may be a removed worktree.
    pub repo_path: PathBuf,
    /// When the run reached its accepted state (RFC3339).
    pub ended_at: String,
}

/// Raw columns of one `outcomes` row: run_id, task_id, kind, detail_json, at.
type OutcomeRow = (String, Option<String>, String, Option<String>, String);

/// An `outcomes` row as columns, parsed into a [`StoredOutcome`] (P6): an
/// unknown kind, a missing task id or a detail that will not parse as
/// [`OutcomeDetail`] is a `Corrupt` row the caller reports, never a
/// silently dropped or defaulted outcome.
pub(super) fn parse_outcome_row(
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

impl Ledger {
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
                outcome.detail.candidate_sha.as_deref(),
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

    /// The most recently recorded outcome for a run, if any — `relais
    /// explain <run>`'s source: a per-run query (`WHERE run_id = ?`), not
    /// [`Ledger::latest_outcome`]'s task-scoped one. A later run of the
    /// same task recording its own feedback must not shadow an earlier
    /// run's outcome when THAT run is explained.
    pub fn outcome_of_run(&self, run_id: &RunId) -> Result<Option<StoredOutcome>> {
        let row: Option<OutcomeRow> = self
            .conn
            .query_row(
                "SELECT run_id, task_id, kind, detail_json, at FROM outcomes
                 WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
                [run_id.as_str()],
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

    /// Runs whose accepted change a git revert could undo, ended at or
    /// after `since`: runs the runner accepted (verified, or approved by a
    /// person), and salvaged runs whose verified candidate was contained in
    /// the landed commit. `ended_at` is when the run reached that state.
    /// Replays are not changes anyone landed.
    pub fn revert_candidates(&self, since: &str) -> Result<Vec<RevertCandidate>> {
        let mut stmt = self.conn.prepare(
            "SELECT runs.id, runs.task_id, runs.repo_path, t.at
               FROM runs
               JOIN transitions t ON t.id = (
                    SELECT MAX(id) FROM transitions
                     WHERE run_id = runs.id AND to_state IN (?1, ?2))
               LEFT JOIN decisions ON decisions.run = runs.id
              WHERE runs.task_id IS NOT NULL
                AND COALESCE(runs.purpose, '') <> ?3
                AND t.at >= ?4
                AND (runs.status = ?1
                     OR (runs.status = ?2
                         AND decisions.resolution = ?5
                         AND decisions.candidate_contained = 1
                         AND decisions.raised_state = ?6
                         AND decisions.raised_reason = ?7))
              ORDER BY runs.id",
        )?;
        let rows = stmt.query_map(
            params![
                State::Accepted.as_str(),
                State::AcceptedByPerson.as_str(),
                RunPurpose::Replay.as_str(),
                since,
                Reason::DecisionSalvaged.as_str(),
                State::NeedsReview.as_str(),
                Reason::ReviewFindings.as_str(),
            ],
            |row| {
                Ok(RevertCandidate {
                    run: RunId::from_stored(row.get::<_, String>(0)?),
                    task: TaskId::from_stored(row.get::<_, String>(1)?),
                    repo_path: PathBuf::from(row.get::<_, String>(2)?),
                    ended_at: row.get(3)?,
                })
            },
        )?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Record a revert relais found in git (`learn::revert`), unless the
    /// task already has it or has since been vouched for. Checked and
    /// inserted in one `BEGIN IMMEDIATE` transaction, so `dataset build`
    /// and a learn cycle running together insert one row between them.
    ///
    /// Not recorded (`Ok(false)`): a `source = detected` row of this task
    /// already names `undo_sha`; or the task's latest outcome was recorded
    /// after the undo commit (`undo_at`) and says the change stands
    /// (`accepted_unchanged`, `corrected`). An earlier outcome, or a
    /// `reverted` or `confirmed_regression` row from another source, does
    /// not block it. The two shas ride in the row's detail as
    /// `landing_sha` and `undo_sha`.
    pub fn record_detected_revert(
        &self,
        run_id: &RunId,
        task_id: &TaskId,
        outcome: &Outcome,
        landing_sha: &str,
        undo_sha: &str,
        undo_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        let tx = self.write_tx()?;
        let already: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM outcomes
                             WHERE task_id = ?1
                               AND json_extract(detail_json, '$.source') = 'detected'
                               AND json_extract(detail_json, '$.undo_sha') = ?2)",
            params![task_id.as_str(), undo_sha],
            |row| row.get(0),
        )?;
        if already {
            return Ok(false);
        }
        let latest: Option<(String, String)> = tx
            .query_row(
                "SELECT kind, at FROM outcomes WHERE task_id = ?1 ORDER BY id DESC LIMIT 1",
                [task_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((kind, at)) = latest {
            let stands = kind == OutcomeKind::AcceptedUnchanged.as_str()
                || kind == OutcomeKind::Corrected.as_str();
            let recorded =
                chrono::DateTime::parse_from_rfc3339(&at).map_err(|e| LedgerError::Corrupt {
                    what: format!("the time of an outcome of task {task_id}"),
                    detail: format!("`{at}` is not an RFC3339 timestamp: {e}"),
                })?;
            if stands && recorded > undo_at {
                return Ok(false);
            }
        }
        let mut detail = serde_json::to_value(&outcome.detail).expect("outcome detail serializes");
        if let Some(object) = detail.as_object_mut() {
            object.insert("landing_sha".into(), landing_sha.into());
            object.insert("undo_sha".into(), undo_sha.into());
            object.insert("undo_at".into(), undo_at.to_rfc3339().into());
        }
        tx.execute(
            "INSERT INTO outcomes (run_id, task_id, candidate_sha, kind, detail_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id.as_str(),
                task_id.as_str(),
                outcome.detail.candidate_sha.as_deref(),
                outcome.kind.as_str(),
                detail.to_string(),
                self.now()
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{run, sample_outcome, task, temp_ledger};
    use super::super::*;

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
}
