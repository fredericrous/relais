//! The home fingerprint around a run (SPEC §8): a snapshot of the watched
//! files before the run and another after it, and one evidence row per
//! file that differs. A warning only: nothing here changes a run's state.

use std::ffi::OsString;

use super::RunEngine;
use crate::homeprint::{self, Snapshot, Watch};
use crate::ledger::EvidenceKind;

/// A variable lookup, by name.
type EnvLookup<'a> = Box<dyn Fn(&str) -> Option<OsString> + 'a>;

impl<'a> RunEngine<'a> {
    /// The watched files now, for a root run on a machine that has the
    /// fingerprint enabled and a home directory; `None` otherwise.
    pub(super) fn home_snapshot(&self) -> Option<Snapshot> {
        let settings = &self.config.machine.fingerprint;
        if !settings.enabled || self.parent.is_some() {
            return None;
        }
        let (home, var) = self.home_and_env()?;
        let watch = Watch {
            home: &home,
            var: &var,
            repo_config: homeprint::repo_config(self.config.repo_dir),
            extra_paths: &settings.extra_paths,
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
        let Some((home, _)) = self.home_and_env() else {
            return;
        };
        let drifts = homeprint::diff(before, &after, &home);
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
            if let Err(error) = self.record_artifact(None, EvidenceKind::HomeDrift, &path, &body) {
                eprintln!("relais: home drift could not be recorded: {error}");
            }
        }
    }

    /// The home directory and the variable lookup that goes with it: the
    /// machine's override alone (nothing else is read from the process),
    /// or the process environment.
    fn home_and_env(&self) -> Option<(std::path::PathBuf, EnvLookup<'_>)> {
        if let Some(home) = &self.config.machine.fingerprint.home_override {
            let owned = home.clone();
            let var = move |name: &str| (name == "HOME").then(|| owned.clone().into_os_string());
            return Some((home.clone(), Box::new(var)));
        }
        let home = crate::paths::resolve_home(|name| std::env::var_os(name)).ok()?;
        Some((home, Box::new(|name| std::env::var_os(name))))
    }
}
