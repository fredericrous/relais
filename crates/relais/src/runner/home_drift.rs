//! The home fingerprint around a run (SPEC §8): a snapshot of the watched
//! files before the run and another after it, and one evidence row per
//! file that differs. A warning only: nothing here changes a run's state.

use super::RunEngine;
use crate::homeprint::{self, Drift, Snapshot, Watch};
use crate::ids::RunId;
use crate::ledger::{EvidenceKind, Ledger};

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

    /// Record one `HomeDrift` evidence row per file that changed since
    /// `before`. Best effort: a failure here is logged and the run goes on.
    pub(super) fn record_home_drift(&self, before: Option<&Snapshot>) {
        let Some(before) = before else { return };
        let Some(after) = self.home_snapshot() else {
            return;
        };
        let drifts = homeprint::diff(before, &after);
        if drifts.is_empty() {
            return;
        }
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
