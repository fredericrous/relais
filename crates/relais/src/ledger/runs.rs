//! Runs and tasks: the run, attempt, task and contract-revision records
//! and the reads over them.

use std::path::PathBuf;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::contract::TaskContract;
use crate::ids::{PackageId, RunId, TaskId};
use crate::lifecycle::{State, UsagePhase};
use crate::policy::Tier;

use super::error::{parse_state, parse_task_origin, parse_tier};
use super::transitions::{stamp_driver, PROJECTED_STATUS};
use super::{intent_effort, Ledger, LedgerError, Result, EFFORT_UNKNOWN};

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

/// Every event that ran `effective` under the requested `requested`
/// alias: when the first and the last of them happened, and how many
/// there were. The raw material of alias-drift detection (SPEC §11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelObservation {
    pub requested: String,
    pub effective: String,
    pub first_seen: String,
    pub last_seen: String,
    pub count: u64,
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

/// A work package run's place in its parent's tree: the run it belongs
/// under and the package it is. Grouped so `insert_child_run` carries one
/// argument for both instead of two more positional ones.
pub struct ChildOf<'a> {
    pub parent_run: &'a RunId,
    pub package_id: &'a PackageId,
}

impl Ledger {
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
        self.insert_typed_run(id, repo_path, root_session, task, repo_key, None)
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
        stamp_driver(&tx, id, &now)?;
        tx.commit()?;
        Ok(())
    }

    /// The tasks-row half of `insert_run`/`insert_child_run`: a no-op
    /// when the task already has a row (its first run created it), an
    /// insert with `origin = created` and `first_run = id` otherwise.
    pub(super) fn ensure_task(
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

    /// Persist the candidate a verify run resolved. Once: a second call
    /// for the same run keeps the first row, so what was resolved at
    /// validation is what is applied.
    pub fn record_candidate_input(
        &self,
        run: &RunId,
        candidate: &crate::contract::ResolvedCandidate,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO candidate_inputs
                (run_id, source_run, candidate_index, candidate_commit, patch_sha256,
                 source_base_sha, target_base_sha, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.as_str(),
                candidate.source_run,
                candidate.index,
                candidate.candidate_commit,
                candidate.patch_sha256,
                candidate.source_base_sha,
                candidate.target_base_sha,
                self.now()
            ],
        )?;
        Ok(())
    }

    /// The candidate a run resolved, as persisted.
    pub fn candidate_input(
        &self,
        run: &RunId,
    ) -> Result<Option<crate::contract::ResolvedCandidate>> {
        Ok(self
            .conn
            .query_row(
                "SELECT source_run, candidate_index, candidate_commit, patch_sha256,
                        source_base_sha, target_base_sha
                   FROM candidate_inputs WHERE run_id = ?1",
                [run.as_str()],
                |row| {
                    Ok(crate::contract::ResolvedCandidate {
                        source_run: row.get(0)?,
                        index: row.get::<_, i64>(1)? as u32,
                        candidate_commit: row.get(2)?,
                        patch_sha256: row.get(3)?,
                        source_base_sha: row.get(4)?,
                        target_base_sha: row.get(5)?,
                    })
                },
            )
            .optional()?)
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

    /// Every (requested alias, effective model) pair a usage event
    /// recorded, with its first and last `at` and its event count, oldest
    /// first within an alias. Rows missing either side are left out: they
    /// predate the `requested_model` column or carry no reported model,
    /// and neither says what an alias resolved to. A negative count is
    /// [`LedgerError::Corrupt`].
    pub fn model_observations(&self) -> Result<Vec<ModelObservation>> {
        let mut stmt = self.conn.prepare(
            "SELECT requested_model, model, MIN(at), MAX(at), COUNT(*) FROM usage_events
             WHERE requested_model IS NOT NULL AND model IS NOT NULL
               AND TRIM(requested_model) <> '' AND TRIM(model) <> ''
             GROUP BY requested_model, model
             ORDER BY requested_model, MIN(at), model",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let rows = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(requested, effective, first_seen, last_seen, count)| {
                let count = u64::try_from(count).map_err(|_| LedgerError::Corrupt {
                    what: "a model observation's event count".into(),
                    detail: format!("{requested} -> {effective} counted {count} events"),
                })?;
                Ok(ModelObservation {
                    requested,
                    effective,
                    first_seen,
                    last_seen,
                    count,
                })
            })
            .collect()
    }

    /// The effort a worker attempt REQUESTED, as a label — the one reader
    /// every effort figure goes through (`relais report --by effort`, the
    /// repair outcomes, a receipt's `efforts_used`), so they cannot
    /// disagree about what an attempt asked for.
    ///
    /// Read from the attempt's worker dispatch intent (the dispatch whose
    /// `attempt_id` is the attempt, `kind` not `review`), in this order:
    /// the resolved ladder's effort at the intent's rung, when the intent
    /// carries both; else the intent's flat `effort`; else the first
    /// non-review `usage_events.requested_effort` of the attempt; else
    /// [`EFFORT_UNKNOWN`]. An effort the ladder records as not requested or
    /// as having no control is labelled so ([`EFFORT_NOT_REQUESTED`],
    /// [`EFFORT_NO_CONTROL`]), never as an identifier. Nothing here names an
    /// effort: the label is whatever the ledger recorded.
    pub fn requested_effort(&self, attempt_id: i64) -> Result<String> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT intent_json, run_id FROM dispatches
                  WHERE attempt_id = ?1
                    AND json_extract(intent_json, '$.kind') IS NOT 'review'
                  ORDER BY created_at, dispatch_id LIMIT 1",
                [attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (intent, run_id) = row.unzip();
        // A corrupt intent is reported, not read as an absent one: falling
        // through would file the attempt under another step's effort.
        let from_intent = intent
            .map(|text| {
                serde_json::from_str::<serde_json::Value>(&text).map_err(|e| LedgerError::Corrupt {
                    what: format!("the dispatch intent of attempt {attempt_id}"),
                    detail: e.to_string(),
                })
            })
            .transpose()?;
        // A later dispatch carries only its rung; the ladder it indexes is
        // the one the run's first worker dispatch recorded.
        let run_ladder = match (&from_intent, run_id) {
            (Some(intent), Some(run_id))
                if intent.get("rung").is_some_and(|rung| !rung.is_null())
                    && intent.get("ladder").is_none_or(serde_json::Value::is_null) =>
            {
                self.first_dispatch_intent(&RunId::from_stored(run_id))?
                    .and_then(|first| first.get("ladder").cloned())
                    .filter(|ladder| !ladder.is_null())
            }
            (Some(_) | None, Some(_) | None) => None,
        };
        let from_intent =
            from_intent.and_then(|intent| intent_effort(&intent, run_ladder.as_ref()));
        if let Some(label) = from_intent {
            return Ok(label);
        }
        let from_usage: Option<String> = self
            .conn
            .query_row(
                "SELECT requested_effort FROM usage_events
                  WHERE attempt_id = ?1 AND requested_effort IS NOT NULL
                    AND phase IS NOT 'review'
                  ORDER BY id LIMIT 1",
                [attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(from_usage.unwrap_or_else(|| EFFORT_UNKNOWN.to_string()))
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
    use super::super::test_support::*;
    use super::super::ModelPair;
    use super::*;

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

    /// Observations group by (requested, effective) with the first and last
    /// `at` and a count; a row missing either side is not one, and a run's
    /// models and the models seen since a date read from the same rows.
    #[test]
    fn model_observations_group_the_pairs_and_leave_out_rows_without_both_sides() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-m"), "/r", None, &task("m"), "rk")
            .expect("run");
        let put = |id: &str, requested: Option<&str>, model: Option<&str>, at: &str| {
            let mut e = event(id, "run-m", 1);
            e.requested_model = requested.map(Into::into);
            e.model = model.map(Into::into);
            e.at = at.into();
            ledger.record_usage(&e).expect("usage");
        };
        put(
            "a",
            Some("sonnet"),
            Some("claude-sonnet-5"),
            "2026-09-23T00:39:00+00:00",
        );
        put(
            "b",
            Some("sonnet"),
            Some("claude-sonnet-5"),
            "2026-09-28T17:35:00+00:00",
        );
        put(
            "c",
            Some("sonnet"),
            Some("claude-sonnet-5-5"),
            "2026-09-28T20:08:00+00:00",
        );
        put("d", None, Some("claude-old"), "2026-09-01T00:00:00+00:00");
        put("e", Some("sonnet"), None, "2026-09-02T00:00:00+00:00");
        put("f", Some("sonnet"), Some(""), "2026-09-03T00:00:00+00:00");
        put("g", Some("sonnet"), Some("  "), "2026-09-04T00:00:00+00:00");
        let stored: Vec<Option<String>> = ledger
            .conn
            .prepare("SELECT model FROM usage_events WHERE event_id IN ('f','g')")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<std::result::Result<_, _>>()
            .expect("rows");
        assert_eq!(stored, vec![None, None]);
        ledger
            .conn
            .execute(
                "INSERT INTO usage_events (event_id, run_id, model, requested_model, at,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    cost_kind, completeness, inclusive)
                 SELECT 'h', run_id, '', 'sonnet', at, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens, cost_kind, completeness, inclusive
                 FROM usage_events WHERE event_id = 'a'",
                [],
            )
            .expect("older relais row");
        let observations = ledger.model_observations().expect("observations");
        assert_eq!(
            observations,
            vec![
                ModelObservation {
                    requested: "sonnet".into(),
                    effective: "claude-sonnet-5".into(),
                    first_seen: "2026-09-23T00:39:00+00:00".into(),
                    last_seen: "2026-09-28T17:35:00+00:00".into(),
                    count: 2,
                },
                ModelObservation {
                    requested: "sonnet".into(),
                    effective: "claude-sonnet-5-5".into(),
                    first_seen: "2026-09-28T20:08:00+00:00".into(),
                    last_seen: "2026-09-28T20:08:00+00:00".into(),
                    count: 1,
                },
            ]
        );
        let pair = |effective: &str| ModelPair {
            requested: "sonnet".into(),
            effective: effective.into(),
        };
        assert_eq!(
            ledger.run_models(&run("run-m")).expect("run models"),
            vec![pair("claude-sonnet-5"), pair("claude-sonnet-5-5")]
        );
        // Worker usage carries its provider-reported cost and is never
        // priced from `[pricing]`, so it names no model here; only
        // orchestration usage does.
        assert!(ledger
            .models_seen_since("2026-09-28T00:00:00+00:00")
            .expect("models since")
            .is_empty());
        ledger
            .record_orchestration_usage(&usage_row(
                "msg-orch",
                "sess-orch",
                "2026-09-28T21:00:00+00:00",
            ))
            .expect("orchestration usage");
        assert_eq!(
            ledger
                .models_seen_since("2026-09-28T00:00:00+00:00")
                .expect("models since"),
            vec!["claude-haiku-4-5".to_string()]
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
