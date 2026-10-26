//! Running holdouts at verification (ADR-0045, SPEC §10).
//!
//! Each resolved holdout is fetched at its recorded commit outside every
//! worktree, run in a second verification copy made after the profile's
//! commands finished, and settled by its command's exit status together
//! with its parsed JUnit report. Nothing here is recorded as a profile
//! check, and nothing it reads is shown to a worker.

use std::path::{Path, PathBuf};

use super::config::holdout_root;
use super::{AttemptLabel, RunEngine, VerifyFailure};
use crate::holdout::{self, ResolvedHoldout};
use crate::policy::{CommandSpec, EffectiveAuthority};
use crate::protocol::Events;
use crate::verify::{self, HoldoutRun, JunitReport};

impl<'a> RunEngine<'a> {
    /// The run's holdout directory: the store is fetched into it once and
    /// reused by every verification of the run; [`Self::remove_holdout_dir`]
    /// removes it at the run's terminal state.
    fn holdout_dir(&self) -> PathBuf {
        holdout_root(&self.config.artifacts_dir).join(self.run_id.as_str())
    }

    /// Remove this run's holdout directory; a root run's start also removes
    /// those of every run the ledger records as ended.
    pub(super) fn remove_holdout_dir(&self) {
        match std::fs::remove_dir_all(self.holdout_dir()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!(
                "relais: the holdout directory {} could not be removed: {e}",
                self.holdout_dir().display()
            ),
        }
    }

    pub(super) fn sweep_ended_holdouts(&self) {
        sweep_ended_holdouts(
            &holdout_root(&self.config.artifacts_dir),
            self.config.ledger,
        );
    }

    /// Fetch and run every resolved holdout, in contract order. A store
    /// that cannot be fetched is the run's to end on, never a failed
    /// criterion.
    pub(super) fn run_holdouts(
        &self,
        candidate_sha: &str,
        authority: &EffectiveAuthority,
        label: AttemptLabel,
        resolved: &[ResolvedHoldout],
    ) -> Result<Vec<HoldoutRun>, VerifyFailure> {
        if resolved.is_empty() {
            return Ok(Vec::new());
        }
        let root = self.holdout_dir();
        let sources = resolved
            .iter()
            .map(|holdout| {
                holdout::fetch(holdout, &root, &|| self.budgets.remaining())
                    .map_err(|e| VerifyFailure::HoldoutUnfetchable(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        resolved
            .iter()
            .zip(&sources)
            .enumerate()
            .map(|(index, (holdout, source))| {
                let site = HoldoutSite {
                    root: &root,
                    index,
                    label,
                    holdout,
                    source,
                };
                self.run_holdout(&site, candidate_sha, authority)
            })
            .collect()
    }

    fn run_holdout(
        &self,
        site: &HoldoutSite<'_>,
        candidate_sha: &str,
        authority: &EffectiveAuthority,
    ) -> Result<HoldoutRun, VerifyFailure> {
        let holdout = site.holdout;
        let did_not_build =
            || HoldoutRun::did_not_build(&holdout.name, &holdout.sha, &holdout.digest);
        let decl = &holdout.decl;
        let source = std::fs::canonicalize(site.source).map_err(|e| e.to_string())?;
        let Some(argv) =
            holdout::substitute_config(&decl.command.argv, decl.config.as_deref(), &source)
        else {
            return Ok(did_not_build());
        };
        if !holdout::clean_relative(&decl.path) {
            return Ok(did_not_build());
        }
        let copy_path = site.root.join(format!(
            "copy-{}-{}",
            site.label.worktree_suffix(),
            site.index
        ));
        let mut copy = verify::verification_worktree(
            self.config.git,
            self.config.repo_dir,
            candidate_sha,
            &copy_path,
        )?;
        let spec = CommandSpec {
            argv,
            ..decl.command.clone()
        };
        let settled = self.run_in_copy(site, &source, copy.path(), &spec, authority);
        if let Err(e) = copy.release() {
            eprintln!("relais: {e}");
        }
        settled
    }

    fn run_in_copy(
        &self,
        site: &HoldoutSite<'_>,
        source: &Path,
        copy: &Path,
        spec: &CommandSpec,
        authority: &EffectiveAuthority,
    ) -> Result<HoldoutRun, VerifyFailure> {
        let holdout = site.holdout;
        let did_not_build =
            || HoldoutRun::did_not_build(&holdout.name, &holdout.sha, &holdout.digest);
        let logs = site.root.join("logs");
        let prefix = format!("{}-holdout{}", site.label.log_prefix(), site.index);
        let setup = self
            .while_watching_cancel(|cancel| {
                self.setup_tree(
                    copy,
                    self.task_tree().as_deref(),
                    &authority.verification_profile,
                    &logs,
                    &prefix,
                    cancel,
                )
            })
            .map_err(VerifyFailure::from)?;
        if verify::setup_failure(&setup).is_some() {
            return Ok(did_not_build());
        }
        if let Err(e) = holdout::copy_into(
            &source.join(&holdout.decl.path),
            &copy.join(&holdout.decl.overlay),
        ) {
            eprintln!(
                "relais: holdout `{}` could not be placed in the verification copy: {e}",
                holdout.name
            );
            return Ok(did_not_build());
        }
        // Silent events: the holdout's output and arguments stay off
        // every channel but the kept log.
        let silent = Events::silent();
        let check = self
            .while_watching_cancel(|cancel| {
                verify::run_observed_within(
                    copy,
                    spec,
                    &logs,
                    &verify::check_label(spec),
                    &format!("{prefix}-run"),
                    &silent,
                    cancel,
                    &self.heavy(),
                )
            })
            .map_err(VerifyFailure::from)?;
        let mut run = HoldoutRun::settled(&holdout.name, &holdout.sha, &holdout.digest, &check);
        let kept = self
            .artifacts
            .join("holdouts")
            .join(file_safe(&holdout.name));
        std::fs::create_dir_all(&kept).map_err(|e| e.to_string())?;
        let log = kept.join("log.txt");
        std::fs::copy(&check.log_path, &log).map_err(|e| e.to_string())?;
        run.log = Some(log.to_string_lossy().into_owned());
        if let Some(JunitReport::Reported {
            artifact: Some(report),
            ..
        }) = &check.junit
        {
            let kept_report = kept.join("report.xml");
            std::fs::copy(&report.path, &kept_report).map_err(|e| e.to_string())?;
            run.report = Some(kept_report.to_string_lossy().into_owned());
        }
        Ok(run)
    }
}

/// Remove the holdout directory of every run the ledger records as ended.
pub fn sweep_ended_holdouts(root: &Path, ledger: &crate::ledger::Ledger) {
    holdout::sweep_ended(root, &|name| {
        let run = crate::ids::RunId::from_stored(name);
        match ledger.run_status(&run) {
            Ok(status) => Ok(matches!(status, Some(state) if state.is_terminal())),
            Err(e) => Err(e.to_string()),
        }
    });
}

/// Where one holdout runs: the run's holdout directory, its position, the
/// verification it belongs to and its fetched store.
struct HoldoutSite<'h> {
    root: &'h Path,
    index: usize,
    label: AttemptLabel,
    holdout: &'h ResolvedHoldout,
    source: &'h Path,
}

/// A holdout name as one path segment.
fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::file_safe;

    #[test]
    fn a_holdout_name_becomes_one_plain_path_segment() {
        assert_eq!(file_safe("json-escaping_2"), "json-escaping_2");
        assert_eq!(file_safe("../a/b"), "_a_b");
        assert_eq!(file_safe(".hidden"), "hidden");
    }
}
