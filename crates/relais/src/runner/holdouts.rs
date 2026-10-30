//! Running holdouts at verification (ADR-0045, SPEC §10).
//!
//! Each resolved holdout is fetched at its recorded commit outside every
//! worktree, run in a second verification copy made after the profile's
//! commands finished, and settled by its command's exit status together
//! with its parsed JUnit report. Nothing here is recorded as a profile
//! check, and nothing it reads is shown to a worker.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::config::holdout_root;
use super::engine::{watch_cancel, CancelWatch};
use super::holdout_exposure::{scan_transcript, Markers};
use super::setup_gate;
use super::{AttemptLabel, HoldoutStop, RunEngine, RunError, VerifyFailure};
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

    /// The criteria that settle this run: the contract's own plus one per
    /// attached holdout, read from the ledger the run froze them in.
    pub(super) fn effective_acceptance(
        &self,
    ) -> Result<Vec<crate::acceptance::AcceptanceEntry>, crate::ledger::LedgerError> {
        self.config
            .ledger
            .effective_acceptance(&self.run_id, self.config.contract)
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
            let source = run.source.unwrap_or(holdout::HoldoutSource::Named);
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
                        source: source.as_str(),
                    },
                )
                .map_err(|e| format!("the ledger refused a holdout outcome row: {e}"))?;
        }
        Ok(())
    }

    /// What a worker is told of a failed holdout: the criterion's statement
    /// and the failing test ids, nothing else.
    pub(super) fn holdout_failures(&self, gaps: &[HoldoutGap]) -> Result<Vec<String>, RunError> {
        let acceptance = self.effective_acceptance()?;
        Ok(gaps
            .iter()
            .map(|HoldoutGap { name, gap }| {
                let statement = acceptance
                    .iter()
                    .find(|entry| entry.id() == gap.criterion_id)
                    .map_or("", |entry| entry.statement());
                if gap.criterion_id == holdout::attached_criterion_id(name) {
                    return attached_failure_line(statement, &gap.missing);
                }
                format!("{} (criterion: {statement})", gap.message())
            })
            .collect())
    }

    /// What a person is told when the attempts ran out on holdout gaps
    /// alone: each holdout with its commit, the failing test ids and the
    /// kept log and report. Only the person's `relais decide` reads it.
    pub(super) fn holdout_stop(
        &self,
        gaps: &[HoldoutGap],
        runs: &[HoldoutRun],
    ) -> Result<HoldoutStop, RunError> {
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
        Ok(HoldoutStop {
            detail: self.holdout_decision_detail(gaps, runs)?,
            holdouts: serde_json::Value::Array(records),
        })
    }

    fn holdout_decision_detail(
        &self,
        gaps: &[HoldoutGap],
        runs: &[HoldoutRun],
    ) -> Result<String, RunError> {
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
        let attached = self
            .config
            .ledger
            .attached_holdout_definitions(&self.run_id)?;
        let provenance = gaps
            .iter()
            .filter_map(|HoldoutGap { name, .. }| {
                let held = attached.iter().find(|held| held.name == *name)?;
                Some(format!(
                    " Holdout `{name}` was attached by machine.toml [holdouts.{}.{name}].attach; \
                     removing attach stops attachment on future root runs; successors retain \
                     frozen holdouts.",
                    held.repo
                ))
            })
            .collect::<String>();
        Ok(format!(
            "the attempts ran out and the only failures left are holdout gaps: {named}. Answer \
             with `relais decide {} --answer approve|revise|reject`; approve is refused, as for \
             any gap.{provenance}",
            self.run_id
        ))
    }

    /// Fetch every resolved holdout on its own, then run them concurrently.
    /// A store that cannot be fetched for a named holdout is the run's to end
    /// on, never a failed criterion; for an attached holdout it skips that
    /// holdout alone (`holdout_attach_skipped`) and records it as skipped, so
    /// its criterion leaves the run's effective acceptance.
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
        let attached = self
            .config
            .ledger
            .attached_holdout_definitions(&self.run_id)
            .map_err(|e| VerifyFailure::from(e.to_string()))?;
        let skipped = self
            .config
            .ledger
            .skipped_attached_holdouts(&self.run_id)
            .map_err(|e| VerifyFailure::from(e.to_string()))?;
        // A skipped attached holdout leaves no definition; the filter guards
        // the `resolved` list, which was captured before the definition was
        // dropped, so it is not asked for again.
        let live: Vec<&ResolvedHoldout> = resolved
            .iter()
            .filter(|holdout| !skipped.contains(&holdout.name))
            .collect();
        let fetched: Vec<Result<PathBuf, holdout::Unfetchable>> = live
            .iter()
            .map(|holdout| holdout::fetch(holdout, &root, &|| self.budgets.remaining()))
            .collect();
        let is_attached_name = |name: &str| attached.iter().any(|held| held.name == name);
        let is_attached = |holdout: &ResolvedHoldout| is_attached_name(&holdout.name);
        if let Some(e) = live
            .iter()
            .zip(&fetched)
            .find_map(|(holdout, fetch)| match fetch {
                Err(e) if !is_attached(holdout) => Some(e),
                _ => None,
            })
        {
            return Err(VerifyFailure::HoldoutUnfetchable(e.to_string()));
        }
        let running_together = fetched.iter().filter(|fetch| fetch.is_ok()).count();
        let mut sites = Vec::new();
        for (index, (holdout, fetch)) in live.iter().zip(&fetched).enumerate() {
            match fetch {
                Ok(source) => sites.push(HoldoutSite {
                    root: &root,
                    index,
                    running_together,
                    label,
                    holdout,
                    source,
                }),
                Err(e) => {
                    // A skipped holdout is counted per run and source, never as
                    // a catch: the profile's state is not what it was skipped on.
                    self.config
                        .ledger
                        .record_holdout_outcome(
                            &self.run_id,
                            &HoldoutOutcomeRow {
                                label_kind: label.kind(),
                                attempt_index: label.ordinal(),
                                name: &holdout.name,
                                outcome: holdout::SKIPPED_OUTCOME,
                                profile_green: false,
                                source: holdout::HoldoutSource::Attached.as_str(),
                            },
                        )
                        .map_err(|e| VerifyFailure::from(e.to_string()))?;
                    self.config
                        .ledger
                        .forget_attached_holdout_definition(&self.run_id, &holdout.name)
                        .map_err(|e| VerifyFailure::from(e.to_string()))?;
                    self.events.emit(Event::Decision {
                        what: "holdout_attach_skipped".to_string(),
                        reason: format!(
                            "holdout `{}`: its store could not be fetched ({e}); the run \
                             settles without it",
                            holdout.name
                        ),
                    });
                }
            }
        }
        // holds-until: a holdout's setup time stays small against its command
        // time (setup is a dependency fetch or a reused cache); setups run
        // concurrently once `setup_tree` stops borrowing the engine and a
        // run's holdouts spend more time in setup than in commands.
        // Setting a copy up needs the engine; the commands, which are the
        // long part, run side by side.
        let mut prepared: Vec<Prepared<'_, '_>> = sites
            .iter()
            .map(|site| self.prepare_holdout(site, candidate_sha, authority))
            .collect();
        let gate = self.config.gate;
        let every = self.config.heartbeat_every;
        let run_id = self.run_id.as_str();
        let asked: &AtomicBool = &self.cancel_asked;
        let artifacts = self.artifacts.as_path();
        let ran: Vec<Result<HoldoutRun, VerifyFailure>> = std::thread::scope(|scope| {
            let handles: Vec<_> = prepared
                .iter_mut()
                .map(|slot| {
                    let prepared_for = slot.prepared_for;
                    let state = &mut slot.state;
                    let admission = self.heavy();
                    scope.spawn(move || {
                        let started = std::time::Instant::now();
                        let watch = CancelWatch {
                            gate,
                            every,
                            run_id,
                            asked,
                        };
                        let run = match state {
                            Settled::Done(run) => {
                                run.take().expect("a settled holdout is read once")
                            }
                            Settled::Ready(job) => job.run_command(artifacts, &admission, &watch),
                        };
                        let wall = prepared_for + started.elapsed();
                        run.map(|run| HoldoutRun {
                            wall_ms: Some(u64::try_from(wall.as_millis()).unwrap_or(u64::MAX)),
                            ..run
                        })
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| match handle.join() {
                    Ok(run) => run,
                    Err(panic) => std::panic::resume_unwind(panic),
                })
                .collect()
        });
        for slot in &mut prepared {
            if let Some(copy) = slot.copy.as_mut() {
                if let Err(e) = copy.release() {
                    eprintln!("relais: {e}");
                }
            }
        }
        ran.into_iter()
            .map(|run| {
                run.map(|run| HoldoutRun {
                    source: Some(if is_attached_name(&run.name) {
                        holdout::HoldoutSource::Attached
                    } else {
                        holdout::HoldoutSource::Named
                    }),
                    ..run
                })
            })
            .collect()
    }

    /// Make the holdout's verification copy, set it up and place its
    /// overlay: everything before its command runs.
    fn prepare_holdout<'s>(
        &self,
        site: &HoldoutSite<'s>,
        candidate_sha: &str,
        authority: &EffectiveAuthority,
    ) -> Prepared<'s, 'a> {
        let started = std::time::Instant::now();
        let holdout = site.holdout;
        let did_not_build =
            || HoldoutRun::did_not_build(&holdout.name, &holdout.sha, &holdout.digest);
        let ready = |copy: Option<verify::VerificationWorktree<'a>>,
                     run: Result<HoldoutRun, VerifyFailure>| Prepared {
            copy,
            state: Settled::Done(Some(run)),
            prepared_for: started.elapsed(),
        };
        let decl = &holdout.decl;
        let source = match std::fs::canonicalize(site.source) {
            Ok(source) => source,
            Err(e) => return ready(None, Err(VerifyFailure::from(e.to_string()))),
        };
        let Some(argv) =
            holdout::substitute_config(&decl.command.argv, decl.config.as_deref(), &source)
        else {
            return ready(None, Ok(did_not_build()));
        };
        if !holdout::clean_relative(&decl.path) {
            return ready(None, Ok(did_not_build()));
        }
        let copy_path = site.root.join(format!(
            "copy-{}-{}",
            site.label.worktree_suffix(),
            site.index
        ));
        let copy = match verify::verification_worktree(
            self.config.git,
            self.config.repo_dir,
            candidate_sha,
            &copy_path,
        ) {
            Ok(copy) => copy,
            Err(e) => return ready(None, Err(e.into())),
        };
        let mut spec = CommandSpec {
            argv,
            ..decl.command.clone()
        };
        // Side by side with another holdout, its own build directory: no
        // other holdout's cargo takes the lock. A lone holdout builds as it
        // always did.
        if site.running_together > 1 {
            spec.env
                .entry("CARGO_TARGET_DIR".to_string())
                .or_insert_with(|| copy.path().join("target").to_string_lossy().into_owned());
        }
        let logs = site.root.join("logs");
        let prefix = format!("{}-holdout{}", site.label.log_prefix(), site.index);
        let setup = self.while_watching_cancel(|cancel| {
            self.setup_tree(
                copy.path(),
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
        });
        let setup = match setup {
            Ok(setup) => setup,
            Err(e) => return ready(Some(copy), Err(e.into())),
        };
        if verify::setup_failure(&setup).is_some() {
            return ready(Some(copy), Ok(did_not_build()));
        }
        if let Err(e) = holdout::copy_into(
            &source.join(&holdout.decl.path),
            &copy.path().join(&holdout.decl.overlay),
        ) {
            eprintln!(
                "relais: holdout `{}` could not be placed in the verification copy: {e}",
                holdout.name
            );
            return ready(Some(copy), Ok(did_not_build()));
        }
        let job = Job {
            holdout,
            dir: copy.path().to_path_buf(),
            spec,
            logs,
            prefix,
        };
        Prepared {
            copy: Some(copy),
            state: Settled::Ready(job),
            prepared_for: started.elapsed(),
        }
    }
}

/// A holdout after its copy was made: its copy, and either its settled
/// result or the job that still has to run its command.
struct Prepared<'h, 'g> {
    copy: Option<verify::VerificationWorktree<'g>>,
    state: Settled<'h>,
    prepared_for: std::time::Duration,
}

enum Settled<'h> {
    Done(Option<Result<HoldoutRun, VerifyFailure>>),
    Ready(Job<'h>),
}

/// What a holdout's command needs, none of it tied to the engine, so the
/// commands can run on separate threads.
struct Job<'h> {
    holdout: &'h ResolvedHoldout,
    dir: PathBuf,
    spec: CommandSpec,
    logs: PathBuf,
    prefix: String,
}

impl Job<'_> {
    fn run_command(
        &self,
        artifacts: &Path,
        admission: &verify::HeavyAdmission<'_>,
        watch: &CancelWatch<'_>,
    ) -> Result<HoldoutRun, VerifyFailure> {
        let holdout = self.holdout;
        let spec = &self.spec;
        // Silent events: the holdout's output and arguments stay off
        // every channel but the kept log.
        let silent = Events::silent();
        let check = watch_cancel(watch, |cancel| {
            verify::run_observed_within(verify::ObservedRun {
                dir: &self.dir,
                spec,
                logs_dir: &self.logs,
                label: &verify::check_label(spec),
                log_stem: &format!("{}-run", self.prefix),
                events: &silent,
                cancel,
                admission,
            })
        })
        .map_err(VerifyFailure::from)?;
        let mut run = HoldoutRun::settled(&holdout.name, &holdout.sha, &holdout.digest, &check);
        let kept = artifacts.join("holdouts").join(file_safe(&holdout.name));
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

/// What a worker is told of an attached holdout's failure: the criterion's
/// statement, the verdict and the failing test names, and nothing of where
/// the holdout was declared.
fn attached_failure_line(statement: &str, missing: &verify::MissingEvidence) -> String {
    let verdict = match missing {
        verify::MissingEvidence::HoldoutFailed { failing, .. } if !failing.is_empty() => {
            format!("failed: {}", failing.join(", "))
        }
        verify::MissingEvidence::HoldoutFailed { .. } => "failed".to_string(),
        verify::MissingEvidence::HoldoutDidNotBuild { .. } => {
            "did not build or wrote no report".to_string()
        }
        verify::MissingEvidence::HoldoutTimedOut { .. } => "timed out".to_string(),
        verify::MissingEvidence::HoldoutNotRun { .. }
        | verify::MissingEvidence::CheckProducedNothing { .. }
        | verify::MissingEvidence::CheckUndefined { .. }
        | verify::MissingEvidence::SignOffUnrecorded
        | verify::MissingEvidence::GateNotCovered { .. }
        | verify::MissingEvidence::GateUnavailable { .. }
        | verify::MissingEvidence::NoPerTestResults { .. }
        | verify::MissingEvidence::TestNotReported { .. }
        | verify::MissingEvidence::TestNotPassed { .. } => "unmet".to_string(),
    };
    format!("{statement}: {verdict}")
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
    /// How many holdouts of this verification run side by side.
    running_together: usize,
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
