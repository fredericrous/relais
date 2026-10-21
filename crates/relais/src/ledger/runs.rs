//! Runs and tasks: the run, attempt, task and contract-revision records
//! and the reads over them.

use std::path::PathBuf;

use rusqlite::{params, OptionalExtension, Transaction};

use crate::contract::TaskContract;
use crate::ids::{PackageId, RunId, TaskId, TrialId};
use crate::lifecycle::{RunPurpose, State, UsagePhase};
use crate::policy::Tier;
use crate::route::RoutedBy;

use super::error::{parse_routed_by, parse_run_purpose, parse_state, parse_tier};
use super::task_types;
use super::transitions::{stamp_driver, PROJECTED_STATUS};
use super::{Ledger, LedgerError, Result};

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
        // A new attempt makes a saved verification snapshot stale.
        self.delete_verification_snapshot(run_id)?;
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

impl TaskOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Backfilled => "backfilled",
        }
    }

    pub(super) fn parse(stored: &str) -> Option<Self> {
        match stored {
            "created" => Some(Self::Created),
            "backfilled" => Some(Self::Backfilled),
            _ => None,
        }
    }
}

/// A worker attempt as the `attempts` table holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerAttempt {
    pub id: i64,
    pub phase: UsagePhase,
}

/// The effort label of an attempt nothing recorded an effort for.
pub const EFFORT_UNKNOWN: &str = "unknown";
/// The effort label of a rung whose model has effort control and was asked
/// for none.
pub const EFFORT_NOT_REQUESTED: &str = "none requested";
/// The effort label of a rung whose model or CLI has no effort control.
pub const EFFORT_NO_CONTROL: &str = "no effort control";

/// The label a ladder rung's serialized effort request stands for:
/// `{"explicit": "<id>"}` is the id itself, and the two unit variants are
/// the labels above. A shape this binary does not know is `None`, so the
/// reader falls through to the next source instead of guessing.
fn effort_request_label(request: &serde_json::Value) -> Option<String> {
    if let Some(name) = request.as_str() {
        return if name == "not_requested" {
            Some(EFFORT_NOT_REQUESTED.to_string())
        } else if name == "control_unsupported" {
            Some(EFFORT_NO_CONTROL.to_string())
        } else {
            None
        };
    }
    request
        .get("explicit")
        .and_then(|id| id.as_str())
        .map(str::to_string)
}

/// The effort a dispatch intent requested: the ladder's rung at the
/// intent's `rung` when both are recorded, else its flat `effort`.
///
/// The runner writes the ladder on a run's FIRST worker dispatch only;
/// every later one (each repair, the escalation) records its `rung` and a
/// null `ladder`. `run_ladder` is that first dispatch's ladder, read when
/// the intent carries none — without it a repair on a rung that requested
/// no effort, whose flat `effort` is null, would read as unknown.
fn intent_effort(
    intent: &serde_json::Value,
    run_ladder: Option<&serde_json::Value>,
) -> Option<String> {
    let rung = intent.get("rung").and_then(|rung| rung.as_u64());
    let rungs = intent
        .get("ladder")
        .filter(|ladder| !ladder.is_null())
        .or(run_ladder)
        .and_then(|ladder| ladder.get("rungs"));
    let from_ladder = rungs
        .zip(rung)
        .and_then(|(rungs, rung)| rungs.get(usize::try_from(rung).ok()?))
        .and_then(|rung| rung.get("effort"))
        .and_then(effort_request_label);
    from_ladder.or_else(|| {
        intent
            .get("effort")
            .and_then(|effort| effort.as_str())
            .map(str::to_string)
    })
}

/// One requested alias and the model the harness reported running for
/// it, as one `usage_events` row carried them. Both are always present:
/// a row missing either says nothing about what an alias resolved to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModelPair {
    pub requested: String,
    pub effective: String,
}

impl Ledger {
    /// [`Ledger::insert_run`] for a contract that declares a task type:
    /// the type is written with the task, in the same transaction. A
    /// declared type replaces a classified one and an earlier declared one
    /// (the contract of the newest run is the orchestrator's latest word);
    /// `None` leaves whatever the task has.
    pub fn insert_typed_run(
        &self,
        id: &RunId,
        repo_path: &str,
        root_session: Option<&str>,
        task: &TaskId,
        repo_key: &str,
        declared: Option<crate::contract::TaskType>,
    ) -> Result<()> {
        let now = self.now();
        let tx = self.write_tx()?;
        Self::ensure_task(&tx, task, repo_key, id, &now)?;
        if let Some(task_type) = declared {
            tx.execute(
                "UPDATE tasks SET task_type = ?2, task_type_source = ?3, taxonomy_version = ?4
                  WHERE task_id = ?1",
                params![
                    task.as_str(),
                    task_type.as_str(),
                    task_types::TaskTypeSource::Declared.as_str(),
                    crate::contract::TASK_TYPE_TAXONOMY_VERSION
                ],
            )?;
        }
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
        stamp_driver(&tx, id, &now)?;
        tx.commit()?;
        Ok(())
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

    /// Every run that has not reached a terminal state, with its state. A
    /// row whose status this binary cannot read is skipped, not fatal: one
    /// bad row must not stop the reap sweep for every other run.
    pub fn non_terminal_runs(&self) -> Result<Vec<(RunId, State)>> {
        let mut statement = self
            .conn
            .prepare(&format!("SELECT id, {PROJECTED_STATUS} FROM runs"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut open = Vec::new();
        for (id, status) in rows {
            match parse_state(&status) {
                Ok(state) if !state.is_terminal() => open.push((RunId::from_stored(&id), state)),
                Ok(_) => {}
                Err(e) => eprintln!("relais: run {id} has an unreadable status, not judged: {e}"),
            }
        }
        Ok(open)
    }

    /// When a run's row was last written (`runs.updated_at`, RFC 3339):
    /// every transition rewrites it. `None` when the run is not on record.
    pub fn run_updated_at(&self, id: &RunId) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT updated_at FROM runs WHERE id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(LedgerError::from)
    }

    /// The repository a run was started in (`runs.repo_path`); `None` when
    /// the run is not on record.
    pub fn run_repo_path(&self, id: &RunId) -> Result<Option<PathBuf>> {
        let path: Option<String> = self
            .conn
            .query_row(
                "SELECT repo_path FROM runs WHERE id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(path.map(PathBuf::from))
    }

    /// The repository identity a run was started under
    /// ([`crate::policy::repo_key`], kept on its task); `None` when the
    /// run or its task is not on record.
    pub fn run_repo_key(&self, id: &RunId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT tasks.repo_key FROM runs JOIN tasks ON tasks.task_id = runs.task_id
                  WHERE runs.id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The attempts of a run that produced a candidate commit, by attempt
    /// index: the commit object each one recorded (SPEC §8).
    pub fn candidate_attempts(&self, id: &RunId) -> Result<Vec<(u32, String)>> {
        let mut statement = self.conn.prepare(
            "SELECT attempt_index, candidate_sha FROM attempts
              WHERE run_id = ?1 AND candidate_sha IS NOT NULL
              ORDER BY attempt_index",
        )?;
        let rows = statement
            .query_map([id.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .map(|(index, sha)| (index as u32, sha))
            .collect())
    }

    /// The base a run's contract resolved to (SPEC §4), from its newest
    /// contract revision.
    pub fn run_base_sha(&self, id: &RunId) -> Result<Option<String>> {
        let sha: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT base_sha FROM contract_revisions WHERE run_id = ?1
                  ORDER BY id DESC LIMIT 1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(sha.flatten())
    }

    /// The `(source run, attempt index)` pairs a run that has not ended
    /// still needs: their candidate refs and patch files are pinned
    /// against retirement and reaping. A consumer whose status this binary
    /// cannot read keeps its pin: a pin is only ever released by a state
    /// that is known to be terminal.
    ///
    /// A pending successor intent pins its candidate too, and so does a
    /// claimed one while the run it launched has not ended (plan A5).
    // holds-until: the decision advisor reads candidates (plan A6 follow-up):
    // an open decision does not pin one yet; its run is terminal and the
    // candidate is kept by the intent a revise writes.
    pub fn pinned_candidates(&self) -> Result<std::collections::BTreeSet<(String, u32)>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT candidate_inputs.source_run, candidate_inputs.candidate_index,
                    (SELECT {PROJECTED_STATUS} FROM runs WHERE runs.id = candidate_inputs.run_id)
               FROM candidate_inputs"
        ))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut pinned: std::collections::BTreeSet<(String, u32)> = rows
            .into_iter()
            .filter(|(_, _, status)| {
                status
                    .as_deref()
                    .is_none_or(|status| !parse_state(status).is_ok_and(State::is_terminal))
            })
            .map(|(run, index, _)| (run, index as u32))
            .collect();
        pinned.extend(self.pinned_by_intents()?);
        Ok(pinned)
    }

    /// Why a run exists, when it is not ordinary work (SPEC §17). `None`
    /// covers both a run written before migration v14 and an ordinary
    /// run written since — the column is nullable and nothing in this
    /// crate ever writes it, so every run reads back this way.
    pub fn run_purpose(&self, id: &RunId) -> Result<Option<RunPurpose>> {
        let purpose: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT purpose FROM runs WHERE id = ?1",
                [id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        purpose.flatten().map(|p| parse_run_purpose(&p)).transpose()
    }

    /// Record why a run exists, when it is not ordinary work (SPEC §17,
    /// §24). Written once, by the one caller minting that kind of run —
    /// `relais dataset replay` for [`RunPurpose::Replay`] — never by
    /// anything a contract or a worker can reach: no `TaskContract` field
    /// and no CLI flag on `relais plan`/`relais run` accepts a purpose.
    /// `changed != 1` is a run id nothing on record recognizes, reported
    /// rather than silently accepted as a no-op.
    pub fn set_run_purpose(&self, id: &RunId, purpose: RunPurpose) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE runs SET purpose = ?2 WHERE id = ?1",
            params![id.as_str(), purpose.as_str()],
        )?;
        if changed != 1 {
            return Err(LedgerError::Corrupt {
                what: "runs.purpose".into(),
                detail: format!("setting the purpose of {id} changed {changed} rows, not 1"),
            });
        }
        Ok(())
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

    /// The requested alias and effective model of each distinct pair the
    /// usage events of a run's WHOLE tree recorded — the root and every
    /// child run, the set [`Ledger::run_cost`] sums over.
    pub fn run_models(&self, run_id: &RunId) -> Result<Vec<ModelPair>> {
        let mut pairs = std::collections::BTreeSet::new();
        for run in self.run_tree(run_id)? {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT requested_model, model FROM usage_events
                 WHERE run_id = ?1 AND requested_model IS NOT NULL AND model IS NOT NULL
                   AND TRIM(requested_model) <> '' AND TRIM(model) <> ''",
            )?;
            let rows = stmt.query_map([run.as_str()], |row| {
                Ok(ModelPair {
                    requested: row.get(0)?,
                    effective: row.get(1)?,
                })
            })?;
            for pair in rows {
                pairs.insert(pair?);
            }
        }
        Ok(pairs.into_iter().collect())
    }

    /// A run and every child run beneath it, recursively, the root first:
    /// the one walk `run_cost` and `run_models` share, so "the run's tree"
    /// cannot mean two sets.
    pub(super) fn run_tree(&self, root: &RunId) -> Result<Vec<RunId>> {
        let mut tree = vec![root.clone()];
        let mut next = 0;
        while let Some(run) = tree.get(next).cloned() {
            tree.extend(self.child_runs(&run)?.into_iter().map(|child| child.run));
            next += 1;
        }
        Ok(tree)
    }

    /// Distinct models that actually ran for a run, from usage events.
    pub fn models_used(&self, run_id: &RunId) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT model FROM usage_events
             WHERE run_id = ?1 AND model IS NOT NULL AND event_id NOT LIKE 'advisor:%'
             ORDER BY model",
        )?;
        let rows = stmt.query_map([run_id.as_str()], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// A run's worker attempts, first dispatched first. The reviewer and the
    /// planner are dispatches with no attempt row, so they are not here.
    pub fn worker_attempts(&self, run_id: &RunId) -> Result<Vec<WorkerAttempt>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, phase FROM attempts WHERE run_id = ?1
             ORDER BY attempt_index, id",
        )?;
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let rows: Vec<(i64, String)> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, phase)| {
                let phase = UsagePhase::parse(&phase).map_err(|unknown| LedgerError::Corrupt {
                    what: format!("the phase of attempt {id} of run {run_id}"),
                    detail: unknown.to_string(),
                })?;
                Ok(WorkerAttempt { id, phase })
            })
            .collect()
    }

    /// The distinct efforts a run's worker attempts requested, in first-use
    /// order, each through [`Ledger::requested_effort`].
    pub fn efforts_used(&self, run_id: &RunId) -> Result<Vec<String>> {
        let mut used: Vec<String> = Vec::new();
        for attempt in self.worker_attempts(run_id)? {
            let effort = self.requested_effort(attempt.id)?;
            if !used.contains(&effort) {
                used.push(effort);
            }
        }
        Ok(used)
    }

    /// The candidate the run's last finished attempt produced, if any.
    ///
    /// The ledger's own answer to "what did this run actually build",
    /// independent of whether verification ever sealed a receipt. It is
    /// what a person-accepted run has instead of one: an interrupted run
    /// can still have finished an attempt and named its candidate, and
    /// attributing that run's outcome to a sha the caller typed rather
    /// than to the one the run recorded would put an unverifiable claim
    /// in the outcomes table as fact (SPEC §20).
    pub fn latest_attempt_candidate(&self, run_id: &RunId) -> Result<Option<String>> {
        let sha: Option<String> = self
            .conn
            .query_row(
                "SELECT candidate_sha FROM attempts
                 WHERE run_id = ?1 AND candidate_sha IS NOT NULL
                 ORDER BY attempt_index DESC, id DESC LIMIT 1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(sha)
    }

    /// Is this run a `kind: verify` run, by its newest recorded contract
    /// revision? A run with no revision, or one this binary cannot read,
    /// is not: the answer only ever says "verify" on evidence.
    pub fn run_is_verify(&self, run_id: &RunId) -> Result<bool> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT contract_json FROM contract_revisions WHERE run_id = ?1
                  ORDER BY id DESC LIMIT 1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(json
            .and_then(|json| TaskContract::from_json_str(&json).ok())
            .is_some_and(|contract| contract.is_verify()))
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
                   AND usage_events.event_id NOT LIKE 'advisor:%'
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

    /// How a run's first dispatch was routed (SPEC §6), as recorded at
    /// dispatch time. `None` is a dispatch that predates migration v13 —
    /// no routing decision was ever captured for it — not the same fact
    /// as [`RoutedBy::ConservativeBaseline`], which says the router ran
    /// and abstained to the baseline.
    pub fn first_dispatch_routed_by(&self, run_id: &RunId) -> Result<Option<RoutedBy>> {
        // `routed_by` is nullable, so the column read itself is
        // `Option<String>`; wrapping that in `.optional()` (for "no
        // dispatch row at all") gives `Option<Option<String>>` — no
        // dispatch and a dispatch with `routed_by` NULL collapse into
        // the same `None` via `.flatten()`, which is the right answer:
        // both mean "no routing decision to read".
        let stored: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT routed_by FROM dispatches WHERE run_id = ?1
                 ORDER BY created_at, dispatch_id LIMIT 1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        stored
            .flatten()
            .map(|stored| parse_routed_by(&stored))
            .transpose()
    }

    /// The trial a run's first dispatch belongs to, if any (SPEC §17).
    /// `None` covers a dispatch with no trial id and a run with no
    /// dispatch at all — nothing in this crate ever writes the column,
    /// so every dispatch reads back this way. An id is stored as given,
    /// not validated against the `trials` table: reading whether it
    /// names a real trial is [`Ledger::trials_by_run`]'s job, not this
    /// one's.
    pub fn first_dispatch_trial_id(&self, run_id: &RunId) -> Result<Option<TrialId>> {
        let stored: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT trial_id FROM dispatches WHERE run_id = ?1
                 ORDER BY created_at, dispatch_id LIMIT 1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored.flatten().map(TrialId::from_stored))
    }

    /// The session a run was started from; `None` when the run is unknown
    /// or has none.
    pub fn run_root_session(&self, run: &RunId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT root_session FROM runs WHERE id = ?1",
                [run.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
}

/// A task origin string as the ledger stored it.
pub(super) fn parse_task_origin(stored: &str) -> Result<TaskOrigin> {
    TaskOrigin::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "task origin".into(),
        detail: format!("`{stored}` is not a task origin this relais knows"),
    })
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

    /// A run's recorded purpose is distinguishable from ordinary work
    /// and from the other purpose — read back typed, exactly as
    /// written, by a reader nothing in this package calls.
    #[test]
    fn a_recorded_run_purpose_reads_back_distinct_from_ordinary_work() {
        let (ledger, dir) = temp_ledger();
        let source_task = task("t-purpose");
        let source_run = run("run-purpose");
        ledger
            .insert_run(&source_run, "/repo", None, &source_task, "rk")
            .expect("run");
        assert_eq!(
            ledger.run_purpose(&source_run).expect("purpose"),
            None,
            "nothing in this package ever assigns a purpose"
        );
        ledger
            .conn
            .execute(
                "UPDATE runs SET purpose = ?1 WHERE id = ?2",
                params![RunPurpose::Replay.as_str(), source_run.as_str()],
            )
            .expect("set purpose");
        assert_eq!(
            ledger.run_purpose(&source_run).expect("purpose"),
            Some(RunPurpose::Replay)
        );
        ledger
            .conn
            .execute(
                "UPDATE runs SET purpose = ?1 WHERE id = ?2",
                params![RunPurpose::TrialArm.as_str(), source_run.as_str()],
            )
            .expect("set purpose");
        assert_eq!(
            ledger.run_purpose(&source_run).expect("purpose"),
            Some(RunPurpose::TrialArm)
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
