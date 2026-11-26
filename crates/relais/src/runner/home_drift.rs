//! The home fingerprint around a run (SPEC §8): a snapshot of the watched
//! files before the run and another after it, and one evidence row per
//! file that differs. A warning only: nothing here changes a run's state.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use super::transcript_calls::{tool_uses, writes, ToolUse, WriteTarget, WriteWorld};
use super::RunEngine;
use crate::homeprint::{self, Drift, RepoConfig, Snapshot, Watch, WorkerCall};
use crate::ids::RunId;
use crate::ledger::{EvidenceKind, Ledger};

/// The watched files when the run started, and when that was: a worker call
/// older than the snapshot cannot have caused a change after it.
pub(super) struct HomeBefore {
    snapshot: Snapshot,
    taken_at: Option<DateTime<Utc>>,
}

/// The tool calls of the run's workers, read once per agent.
struct WorkerCalls {
    /// (agent id, its calls that ran at or after the first snapshot).
    by_agent: Vec<(String, Vec<ToolUse>)>,
    /// False when the run had no worker dispatch, or a transcript could not
    /// be read: "unexplained" is then weaker.
    all_read: bool,
}

impl<'a> RunEngine<'a> {
    /// The watched files now, for a root run on a machine that has the
    /// fingerprint enabled and a home directory; `None` otherwise.
    pub(super) fn home_snapshot(&self) -> Option<Snapshot> {
        let settings = &self.config.machine.fingerprint;
        if !settings.enabled || self.parent.is_some() {
            return None;
        }
        let Some(home) = self.config.home.home.as_deref() else {
            eprintln!("relais: no home directory could be resolved; home files are not watched");
            eprintln!("relais: set HOME, or set [fingerprint] enabled = false in machine.toml");
            return None;
        };
        let extra_paths = homeprint::expand_extra_paths(&settings.extra_paths, home);
        let watch = Watch {
            home,
            var: &*self.config.home.env,
            repo_config: homeprint::repo_config(self.config.repo_dir),
            extra_paths: &extra_paths,
        };
        Some(homeprint::snapshot(&watch, &|path| std::fs::read(path)))
    }

    /// The first snapshot of a root run, with its time.
    pub(super) fn home_before(&self) -> Option<HomeBefore> {
        let taken_at = DateTime::parse_from_rfc3339(&self.config.ledger.now())
            .ok()
            .map(|at| at.with_timezone(&Utc));
        self.home_snapshot()
            .map(|snapshot| HomeBefore { snapshot, taken_at })
    }

    /// The tool calls of every worker dispatch of the run tree, one
    /// transcript read per agent: a continued attempt reuses its agent and
    /// its transcript holds the whole history.
    fn worker_calls(&self, since: Option<DateTime<Utc>>) -> WorkerCalls {
        let dispatches = match self.config.ledger.agent_dispatches_of_tree(&self.run_id) {
            Ok(dispatches) => dispatches,
            Err(error) => {
                eprintln!(
                    "relais: the run's dispatches could not be read to explain home drift: {error}"
                );
                return WorkerCalls {
                    by_agent: Vec::new(),
                    all_read: false,
                };
            }
        };
        let mut seen = BTreeSet::new();
        let mut calls = WorkerCalls {
            by_agent: Vec::new(),
            all_read: !dispatches.is_empty(),
        };
        for (_dispatch, agent) in dispatches {
            if !seen.insert(agent.clone()) {
                continue;
            }
            match self.config.backend.transcript_of(&agent) {
                Some(text) => {
                    let kept = tool_uses(&text)
                        .into_iter()
                        .filter(|call| since.is_none_or(|since| call.at_or_after(since)))
                        .collect();
                    calls.by_agent.push((agent, kept));
                }
                None => calls.all_read = false,
            }
        }
        calls
    }

    /// Record one `HomeDrift` evidence row per file that changed since
    /// `before`, each with its cause. Best effort: a failure here is logged
    /// and the run goes on.
    pub(super) fn record_home_drift(&self, before: Option<&HomeBefore>) {
        let Some(before) = before else { return };
        let Some(after) = self.home_snapshot() else {
            return;
        };
        let drifts = homeprint::diff(&before.snapshot, &after);
        if drifts.is_empty() {
            return;
        }
        let calls = self.worker_calls(before.taken_at);
        let home = self.config.home.home.as_deref();
        let repo_config = match homeprint::repo_config(self.config.repo_dir) {
            RepoConfig::At(path) => Some(path),
            RepoConfig::Unresolved(_) => None,
        };
        let drifts = homeprint::attribute(
            drifts,
            &before.snapshot,
            &after,
            &|path| call_writing(&calls, path, home, repo_config.as_deref()),
            calls.all_read,
        );
        if let Err(error) = std::fs::create_dir_all(&self.artifacts) {
            eprintln!("relais: home drift could not be recorded: {error}");
            return;
        }
        for (index, drift) in drifts.iter().enumerate() {
            let path = self
                .artifacts
                .join(format!("home-drift-{}.json", index + 1));
            let body = match serde_json::to_string(drift) {
                Ok(body) => body,
                Err(error) => {
                    eprintln!("relais: home drift could not be recorded: {error}");
                    continue;
                }
            };
            if let Err(error) =
                self.record_immutable_artifact(EvidenceKind::HomeDrift, &path, &body)
            {
                eprintln!("relais: home drift could not be recorded: {error}");
            }
        }
    }
}

/// The first worker call that writes `path`.
fn call_writing(
    calls: &WorkerCalls,
    path: &Path,
    home: Option<&Path>,
    repo_config: Option<&Path>,
) -> Option<WorkerCall> {
    let is_repo_config = repo_config == Some(path);
    let target = WriteTarget::new(path, home, is_repo_config);
    let in_repo_cache: RefCell<HashMap<PathBuf, bool>> = RefCell::default();
    let in_run_repo = |dir: &Path| {
        *in_repo_cache
            .borrow_mut()
            .entry(dir.to_path_buf())
            .or_insert_with(|| {
                matches!(
                    homeprint::repo_config(dir),
                    RepoConfig::At(found) if Some(found.as_path()) == repo_config
                )
            })
    };
    let world = WriteWorld {
        home,
        in_run_repo: &in_run_repo,
    };
    calls.by_agent.iter().find_map(|(agent, uses)| {
        uses.iter()
            .find(|call| writes(call, &target, &world))
            .map(|call| WorkerCall {
                tool: call.name.clone(),
                agent: agent.clone(),
            })
    })
}

/// The drifts recorded for `run`, and one line for each record that could
/// not be read, so a lost record is visible instead of silent.
pub fn drifts_of_run(ledger: &Ledger, run: &RunId) -> (Vec<Drift>, Vec<String>) {
    let rows = match ledger.evidence(run) {
        Ok(rows) => rows,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("home drift records unreadable: {error}")],
            )
        }
    };
    let mut drifts = Vec::new();
    let mut problems = Vec::new();
    for row in rows
        .iter()
        .filter(|row| row.kind == EvidenceKind::HomeDrift)
    {
        let parsed = std::fs::read_to_string(&row.path)
            .map_err(|error| error.to_string())
            .and_then(|body| serde_json::from_str(&body).map_err(|error| error.to_string()));
        match parsed {
            Ok(drift) => drifts.push(drift),
            Err(error) => problems.push(format!(
                "home drift record {} unreadable: {error}",
                row.path
            )),
        }
    }
    (drifts, problems)
}
