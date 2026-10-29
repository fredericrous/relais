//! Running holdouts at verification (ADR-0045, SPEC §10).
//!
//! Each resolved holdout is fetched at its recorded commit outside every
//! worktree, run in a second verification copy made after the profile's
//! commands finished, and settled by its command's exit status together
//! with its parsed JUnit report. Nothing here is recorded as a profile
//! check, and nothing it reads is shown to a worker.

use std::path::{Path, PathBuf};

use super::config::holdout_root;
use super::holdout_exposure::{scan_transcript, Markers};
use super::setup_gate;
use super::{AttemptLabel, HoldoutStop, RunEngine, VerifyFailure};
use crate::holdout::{self, ResolvedHoldout};
use crate::ledger::HoldoutOutcomeRow;
use crate::policy::{CommandSpec, EffectiveAuthority};
use crate::protocol::Event;
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

    /// Count the holdout locations this run's workers named in their own
    /// tool calls: the transcript of each dispatch that ran as an agent is
    /// scanned for the markers of the run's frozen holdouts, and every
    /// (dispatch, tool, marker) hit is one ledger row holding the marker's
    /// name only. A transcript that cannot be read is no exposure and is
    /// said as `holdout_exposure_unread`. Counting never changes the run's
    /// outcome: a ledger failure is reported and the run ends as it did.
    pub(super) fn count_holdout_exposures(&self) {
        if self.parent.is_some() {
            return;
        }
        let ledger = self.config.ledger;
        let held = match ledger.holdout_definitions(&self.run_id) {
            Ok(held) => held,
            Err(e) => {
                eprintln!("relais: the run's holdouts could not be read to count exposures: {e}");
                return;
            }
        };
        if held.is_empty() {
            return;
        }
        let dispatches = match ledger.agent_dispatches_of_tree(&self.run_id) {
            Ok(dispatches) => dispatches,
            Err(e) => {
                eprintln!("relais: the run's dispatches could not be read to count exposures: {e}");
                return;
            }
        };
        let markers = Markers::of(
            &held,
            &holdout_root(&self.config.artifacts_dir).to_string_lossy(),
            &holdout::machine_toml_label(),
        );
        for (dispatch, agent) in dispatches {
            let Some(transcript) = self.config.backend.transcript_of(&agent) else {
                self.events.emit(Event::Decision {
                    what: "holdout_exposure_unread".to_string(),
                    reason: dispatch,
                });
                continue;
            };
            for (tool, marker) in scan_transcript(&transcript, &markers) {
                if let Err(e) =
                    ledger.record_holdout_exposure(&self.run_id, &dispatch, &tool, marker.as_str())
                {
                    eprintln!("relais: a holdout exposure could not be recorded: {e}");
                }
            }
        }
    }

    /// Record one holdout outcome row per holdout a verification ran, with
    /// whether the profile's checks were green on that candidate.
    pub(super) fn record_holdout_outcomes(
        &self,
        label: AttemptLabel,
        runs: &[HoldoutRun],
        profile_green: bool,
    ) -> Result<(), String> {
        for run in runs {
            self.config
                .ledger
                .record_holdout_outcome(
                    &self.run_id,
                    &HoldoutOutcomeRow {
                        label_kind: label.kind(),
                        attempt_index: label.ordinal(),
                        name: &run.name,
                        outcome: run.outcome.as_str(),
                        profile_green,
                    },
                )
                .map_err(|e| format!("the ledger refused a holdout outcome row: {e}"))?;
        }
        Ok(())
    }

    /// What a worker is told of a failed holdout: the criterion's statement
    /// and the failing test ids, nothing else.
    pub(super) fn holdout_failures(&self, gaps: &[HoldoutGap]) -> Vec<String> {
        gaps.iter()
            .map(|HoldoutGap { gap, .. }| {
                let statement = self
                    .config
                    .contract
                    .acceptance
                    .iter()
                    .find(|entry| entry.id() == gap.criterion_id)
                    .map_or("", |entry| entry.statement());
                format!("{} (criterion: {statement})", gap.message())
            })
            .collect()
    }

    /// What a person is told when the attempts ran out on holdout gaps
    /// alone: each holdout with its commit, the failing test ids and the
    /// kept log and report. Only the person's `relais decide` reads it.
    pub(super) fn holdout_stop(&self, gaps: &[HoldoutGap], runs: &[HoldoutRun]) -> HoldoutStop {
        let records = gaps
            .iter()
            .map(|HoldoutGap { name, .. }| {
                let kept = runs.iter().find(|run| run.name == *name);
                serde_json::json!({
                    "name": name,
                    "sha": kept.map(|run| run.sha.as_str()),
                    "failing": kept.map(HoldoutRun::failing).unwrap_or_default(),
                    "log": kept.and_then(|run| run.log.as_ref()),
                    "report": kept.and_then(|run| run.report.as_ref()),
                })
            })
            .collect::<Vec<_>>();
        HoldoutStop {
            detail: self.holdout_decision_detail(gaps, runs),
            holdouts: serde_json::Value::Array(records),
        }
    }

    fn holdout_decision_detail(&self, gaps: &[HoldoutGap], runs: &[HoldoutRun]) -> String {
        let named = gaps
            .iter()
            .map(|HoldoutGap { name, gap }| {
                let kept = runs.iter().find(|run| run.name == *name);
                let place = |label: &str, path: Option<&String>| {
                    path.map(|path| format!("; {label} {path}"))
                        .unwrap_or_default()
                };
                format!(
                    "{} (holdout `{name}` at {}, failing tests: {}{}{})",
                    gap.message(),
                    kept.map_or("an unknown commit", |run| run.sha.as_str()),
                    kept.map(HoldoutRun::failing)
                        .filter(|ids| !ids.is_empty())
                        .map_or_else(|| "none reported".to_string(), |ids| ids.join(", ")),
                    place("log", kept.and_then(|run| run.log.as_ref())),
                    place("report", kept.and_then(|run| run.report.as_ref())),
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "the attempts ran out and the only failures left are holdout gaps: {named}. Answer \
             with `relais decide {} --answer approve|revise|reject`; approve is refused, as for \
             any gap",
            self.run_id
        )
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
                    self.task_tree()
                        .as_deref()
                        .map(|path| setup_gate::SeedSource {
                            path,
                            wait: setup_gate::SeedWait::WritersEnded,
                        }),
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
        if run.outcome == verify::HoldoutVerdict::TimedOut {
            use std::io::Write;
            let mut kept_log = std::fs::OpenOptions::new()
                .append(true)
                .open(&log)
                .map_err(|e| e.to_string())?;
            writeln!(
                kept_log,
                "\nholdout timed out after {} s",
                spec.timeout_seconds
            )
            .map_err(|e| e.to_string())?;
        }
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

/// A holdout gap a repair can answer, with the holdout it names.
pub(super) struct HoldoutGap {
    pub(super) name: String,
    pub(super) gap: verify::AcceptanceGap,
}

/// `gaps` and `acceptance_gaps` split into the holdout gaps a repair can
/// answer (a failed holdout, one that did not build) and the rest. A
/// holdout that was not run stays a gap.
pub(super) struct HoldoutSplit {
    pub(super) holdout: Vec<HoldoutGap>,
    pub(super) gaps: Vec<String>,
    pub(super) acceptance_gaps: Vec<verify::AcceptanceGap>,
}

pub(super) fn split_holdout_gaps(
    mut gaps: Vec<String>,
    acceptance_gaps: Vec<verify::AcceptanceGap>,
) -> HoldoutSplit {
    let mut holdout = Vec::new();
    let mut kept = Vec::new();
    for gap in acceptance_gaps {
        match &gap.missing {
            verify::MissingEvidence::HoldoutFailed { name, .. }
            | verify::MissingEvidence::HoldoutDidNotBuild { name }
            | verify::MissingEvidence::HoldoutTimedOut { name } => {
                let message = gap.message();
                if let Some(at) = gaps.iter().position(|sentence| *sentence == message) {
                    gaps.remove(at);
                }
                holdout.push(HoldoutGap {
                    name: name.clone(),
                    gap,
                });
            }
            verify::MissingEvidence::CheckProducedNothing { .. }
            | verify::MissingEvidence::CheckUndefined { .. }
            | verify::MissingEvidence::SignOffUnrecorded
            | verify::MissingEvidence::GateNotCovered { .. }
            | verify::MissingEvidence::GateUnavailable { .. }
            | verify::MissingEvidence::HoldoutNotRun { .. }
            | verify::MissingEvidence::NoPerTestResults { .. }
            | verify::MissingEvidence::TestNotReported { .. }
            | verify::MissingEvidence::TestNotPassed { .. } => kept.push(gap),
        }
    }
    HoldoutSplit {
        holdout,
        gaps,
        acceptance_gaps: kept,
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
