//! Task types in the ledger (step v29): the label a task carries, how it
//! got it (`declared` by its contract, or `classified` afterwards for
//! history), the taxonomy version it was given under, and the export and
//! import `relais dataset export-tasks` / `import-task-types` run on.
//! Mechanism only: relais links no model, so the classification itself is
//! the orchestrator's, and a label arrives here already chosen.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use super::{Ledger, LedgerError, Result};
use crate::contract::{TaskContract, TaskType, TASK_TYPE_TAXONOMY_VERSION};
use crate::ids::{RunId, TaskId};

/// How a task came by its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTypeSource {
    /// The contract named it when the task was created.
    Declared,
    /// A classifier labelled history after the fact.
    Classified,
}

impl TaskTypeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Classified => "classified",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "declared" => Some(Self::Declared),
            "classified" => Some(Self::Classified),
            _ => None,
        }
    }
}

/// A task's stored label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredTaskType {
    pub task_type: TaskType,
    pub source: TaskTypeSource,
    pub taxonomy_version: u32,
}

/// One recorded task as `relais dataset export-tasks` writes it: what a
/// classifier needs to place it, and no file contents. The contract
/// fields are absent for a task with no readable contract on record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskExportRow {
    pub task_id: String,
    pub repo: String,
    pub kind: Option<String>,
    pub objective: Option<String>,
    pub write_scope: Vec<String>,
    pub acceptance: Vec<String>,
}

/// What `export_tasks` read: a row per task, and how many tasks had a
/// stored contract that no longer parses (their row carries no contract
/// fields, like a task with none on record, so the count is the only way
/// a caller can tell the two apart).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedTasks {
    pub rows: Vec<TaskExportRow>,
    pub unreadable_contracts: u32,
}

/// What an import did, label by label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportedTaskTypes {
    /// Labels written, by type.
    pub applied: BTreeMap<TaskType, u32>,
    /// Labels already stored exactly so: a re-import writes nothing.
    pub unchanged: u32,
    /// Labels not written because the task's type was declared.
    pub kept_declared: u32,
}

fn corrupt(detail: String) -> LedgerError {
    LedgerError::Corrupt {
        what: "a task type".into(),
        detail,
    }
}

fn stored_type(
    row: (Option<String>, Option<String>, Option<i64>),
) -> Result<Option<StoredTaskType>> {
    let (task_type, source, version) = row;
    let (Some(task_type), Some(source)) = (task_type, source) else {
        return Ok(None);
    };
    Ok(Some(StoredTaskType {
        task_type: TaskType::parse_stored(&task_type).ok_or_else(|| {
            corrupt(format!(
                "`{task_type}` is not a task type this relais knows"
            ))
        })?,
        source: TaskTypeSource::parse(&source)
            .ok_or_else(|| corrupt(format!("`{source}` is not a task type source")))?,
        taxonomy_version: version
            .ok_or_else(|| corrupt(format!("`{task_type}` is stored with no taxonomy version")))?
            as u32,
    }))
}

impl Ledger {
    /// The type a task carries, if it has one.
    pub fn task_type_of(&self, task: &TaskId) -> Result<Option<StoredTaskType>> {
        let row = self
            .conn
            .query_row(
                "SELECT task_type, task_type_source, taxonomy_version FROM tasks WHERE task_id = ?1",
                [task.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        row.map(stored_type).transpose().map(Option::flatten)
    }

    /// The type routing uses for a task: the contract's declared one, else
    /// the ledger's (classified) one. `unclassified` routes as no type.
    pub fn routing_type(
        &self,
        task: &TaskId,
        declared: Option<TaskType>,
    ) -> Result<Option<TaskType>> {
        if declared.is_some() {
            return Ok(declared);
        }
        Ok(self
            .task_type_of(task)?
            .map(|stored| stored.task_type)
            .filter(|task_type| *task_type != TaskType::Unclassified))
    }

    /// How many tasks carry a type at all.
    pub fn typed_task_count(&self) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_type IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }

    /// Every task id on record.
    pub fn task_id_set(&self) -> Result<BTreeSet<String>> {
        let mut stmt = self.conn.prepare("SELECT task_id FROM tasks")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<BTreeSet<_>, _>>()?;
        Ok(ids)
    }

    /// One row per recorded task, oldest first: the repository its first
    /// run ran in, and the objective, scope and acceptance of the task's
    /// newest contract revision. No file contents.
    pub fn export_tasks(&self) -> Result<ExportedTasks> {
        let mut stmt = self.conn.prepare(
            "SELECT t.task_id,
                    COALESCE((SELECT r.repo_path FROM runs r WHERE r.id = t.first_run), t.repo_key),
                    (SELECT c.contract_json FROM contract_revisions c
                       JOIN runs r ON r.id = c.run_id
                      WHERE r.task_id = t.task_id
                      ORDER BY c.id DESC LIMIT 1)
               FROM tasks t ORDER BY t.created_at, t.task_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut unreadable_contracts = 0;
        let rows = rows
            .into_iter()
            .map(|(task_id, repo, contract_json)| {
                let contract = contract_json.as_deref().and_then(|json| {
                    let parsed = TaskContract::from_json_str(json).ok();
                    if parsed.is_none() {
                        unreadable_contracts += 1;
                    }
                    parsed
                });
                TaskExportRow {
                    task_id,
                    repo,
                    kind: contract.as_ref().map(|c| {
                        match c.kind() {
                            crate::contract::Kind::Change => "change",
                            crate::contract::Kind::Inspect => "inspect",
                        }
                        .to_string()
                    }),
                    objective: contract.as_ref().map(|c| c.objective.clone()),
                    write_scope: contract
                        .as_ref()
                        .map(|c| c.scope_patterns().to_vec())
                        .unwrap_or_default(),
                    acceptance: contract
                        .as_ref()
                        .map(TaskContract::acceptance_statements)
                        .unwrap_or_default(),
                }
            })
            .collect();
        Ok(ExportedTasks {
            rows,
            unreadable_contracts,
        })
    }

    /// Write classified labels, all in one transaction. A label never
    /// replaces a declared type, and one already stored exactly so (same
    /// type, `classified`, same taxonomy version) writes nothing, so a
    /// re-import is a no-op. A task id the ledger does not know rolls the
    /// whole import back.
    pub fn import_task_types(&self, labels: &[(String, TaskType)]) -> Result<ImportedTaskTypes> {
        let tx = self.write_tx()?;
        let mut imported = ImportedTaskTypes::default();
        for (task_id, task_type) in labels {
            let row: Option<(Option<String>, Option<String>, Option<i64>)> = tx
                .query_row(
                    "SELECT task_type, task_type_source, taxonomy_version FROM tasks WHERE task_id = ?1",
                    [task_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some(row) = row else {
                return Err(corrupt(format!("task `{task_id}` is not on record")));
            };
            match stored_type(row)? {
                Some(stored) if stored.source == TaskTypeSource::Declared => {
                    imported.kept_declared += 1;
                }
                Some(stored)
                    if stored.task_type == *task_type
                        && stored.taxonomy_version == TASK_TYPE_TAXONOMY_VERSION =>
                {
                    imported.unchanged += 1;
                }
                _ => {
                    tx.execute(
                        "UPDATE tasks SET task_type = ?2, task_type_source = ?3, taxonomy_version = ?4
                          WHERE task_id = ?1",
                        params![
                            task_id,
                            task_type.as_str(),
                            TaskTypeSource::Classified.as_str(),
                            TASK_TYPE_TAXONOMY_VERSION
                        ],
                    )?;
                    *imported.applied.entry(*task_type).or_default() += 1;
                }
            }
        }
        tx.commit()?;
        Ok(imported)
    }

    /// Whether the trial arm that ran as `run` was handed a design.
    pub fn run_had_design(&self, run: &RunId) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM trials WHERE arm_run_id = ?1 AND design_sha256 IS NOT NULL",
            [run.as_str()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger(tag: &str) -> (crate::test_support::TempDir, Ledger) {
        let dir = crate::test_support::temp_dir(&format!("task-types-{tag}"));
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        (dir, ledger)
    }

    fn task(ledger: &Ledger, name: &str, declared: Option<TaskType>) -> TaskId {
        let task = TaskId::from_stored(name);
        ledger
            .insert_typed_run(
                &RunId::from_stored(format!("run-{name}")),
                "/repo",
                None,
                &task,
                "rk",
                declared,
            )
            .expect("run");
        task
    }

    #[test]
    fn a_declared_type_is_written_with_the_task() {
        let (_dir, ledger) = ledger("declared");
        let typed = task(&ledger, "typed", Some(TaskType::Mechanical));
        let plain = task(&ledger, "plain", None);
        assert_eq!(
            ledger.task_type_of(&typed).expect("read"),
            Some(StoredTaskType {
                task_type: TaskType::Mechanical,
                source: TaskTypeSource::Declared,
                taxonomy_version: 1,
            })
        );
        assert_eq!(ledger.task_type_of(&plain).expect("read"), None);
    }

    #[test]
    fn an_import_never_overwrites_a_declared_type_and_a_reimport_is_a_no_op() {
        let (_dir, ledger) = ledger("import");
        let declared = task(&ledger, "declared", Some(TaskType::Ui));
        let history = task(&ledger, "history", None);
        let labels = vec![
            (declared.as_str().to_string(), TaskType::Infra),
            (history.as_str().to_string(), TaskType::Feature),
        ];
        let first = ledger.import_task_types(&labels).expect("imports");
        assert_eq!(first.applied.get(&TaskType::Feature), Some(&1));
        assert_eq!(first.kept_declared, 1);
        assert_eq!(first.unchanged, 0);
        assert_eq!(
            ledger
                .task_type_of(&declared)
                .expect("read")
                .map(|t| t.task_type),
            Some(TaskType::Ui),
            "a declared type survives an import"
        );
        let stored = ledger.task_type_of(&history).expect("read").expect("typed");
        assert_eq!(stored.source, TaskTypeSource::Classified);

        let again = ledger.import_task_types(&labels).expect("imports");
        assert!(again.applied.is_empty(), "{again:?}");
        assert_eq!(again.unchanged, 1);
        assert_eq!(ledger.task_type_of(&history).expect("read"), Some(stored));
    }

    #[test]
    fn an_unknown_task_id_rolls_the_whole_import_back() {
        let (_dir, ledger) = ledger("rollback");
        let known = task(&ledger, "known", None);
        let labels = vec![
            (known.as_str().to_string(), TaskType::Report),
            ("task-missing".to_string(), TaskType::Report),
        ];
        assert!(ledger.import_task_types(&labels).is_err());
        assert_eq!(ledger.typed_task_count().expect("count"), 0);
    }

    #[test]
    fn an_unreadable_contract_is_exported_without_one_and_counted() {
        let (_dir, ledger) = ledger("unreadable");
        task(&ledger, "none", None);
        task(&ledger, "broken", None);
        let contract = TaskContract::from_json_str(
            r#"{"schema_version":1,"kind":"change","objective":"o","base_ref":"HEAD",
                "write_scope":["a"],"acceptance":["b"],"verification_profile":"p"}"#,
        )
        .expect("contract");
        ledger
            .insert_contract_revision(
                &RunId::from_stored("run-broken"),
                &contract.hash(),
                "{not json",
                "HEAD",
                None,
            )
            .expect("revision");
        let exported = ledger.export_tasks().expect("export");
        assert_eq!(exported.rows.len(), 2);
        assert!(exported.rows.iter().all(|row| row.objective.is_none()));
        assert_eq!(exported.unreadable_contracts, 1);
    }

    #[test]
    fn a_type_stored_with_no_taxonomy_version_is_corrupt() {
        let (_dir, ledger) = ledger("no-version");
        let task = task(&ledger, "typed", Some(TaskType::Feature));
        ledger
            .conn
            .execute(
                "UPDATE tasks SET taxonomy_version = NULL WHERE task_id = ?1",
                [task.as_str()],
            )
            .expect("update");
        assert!(ledger.task_type_of(&task).is_err());
    }
}
