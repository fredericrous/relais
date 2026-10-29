//! The baseline of a run: the checks on the unmodified base.

use super::setup_gate;
use super::{
    fresh_progress, Baseline, BlockCode, Ended, Event, EvidenceKind, Path, Phase, Preflight,
    RunEngine, RunError, VerificationProfile,
};
use crate::verify;

impl<'a> RunEngine<'a> {
    /// Baseline verification at the base SHA: pre-existing failures are
    /// visible from the start (SPEC §10); cached only when the profile
    /// opts in (SPEC §18).
    pub(super) fn baseline(&mut self, preflight: &Preflight) -> Result<Phase<Baseline>, RunError> {
        let authority = &preflight.authority;
        let logs_dir = self.artifacts.join("logs");
        let baseline_cache = verify::BaselineCache::new(
            &self
                .config
                .artifacts_dir
                .parent()
                .unwrap_or(&self.config.artifacts_dir)
                .join("baseline-cache"),
        );
        // The key names the toolchain the checks actually run on: a
        // `rustup update` changes the verdict, and the old key would
        // have reported a real regression as pre-existing (SPEC §18,
        // audit V2). A toolchain that cannot be identified refuses the
        // cache and says so in the receipt.
        let toolchain = verify::resolve_toolchain(&authority.verification_profile, &|program| {
            crate::tooling::program_version(program, None)
        });
        // The base's content, not its commit: the same files reached by a
        // different commit (an empty one between, an in-place start
        // snapshot) have the same verdict (ADR-0008). A tree git cannot
        // name falls back to the commit, which only narrows the cache.
        let base_tree = self
            .config
            .git
            .run(
                self.config.repo_dir,
                &["rev-parse", &format!("{}^{{tree}}", preflight.base_sha)],
            )
            .map(|tree| tree.trim().to_string())
            .unwrap_or_else(|_| preflight.base_sha.clone());
        let baseline_key = match &toolchain {
            Ok(toolchain) => Some(verify::baseline_key(
                &base_tree,
                &authority.verification_profile,
                &preflight.manifest.tool_versions,
                toolchain,
            )),
            Err(_) => None,
        };
        let cache_refused = toolchain.err();
        let cacheable = authority.verification_profile.cache_baseline;
        let cached = match (cacheable, &baseline_key) {
            (true, Some(key)) => baseline_cache.get(key, &logs_dir),
            _ => None,
        };
        let was_cached = cached.is_some();
        // The baseline's check outcomes are kept: a candidate identical to
        // the base has exactly these results, without re-running them.
        let (failures, checks) = match cached {
            // A hit that kept the outcomes has written their logs into this
            // run's `logs/`: they are this run's evidence like any check's.
            Some(hit) => {
                if let Some(checks) = &hit.checks {
                    self.record_logs(None, EvidenceKind::CheckLog, checks)?;
                }
                (hit.failures, hit.checks)
            }
            None => match verify::verification_worktree(
                self.config.git,
                self.config.repo_dir,
                &preflight.base_sha,
                &self.verify_dir.join("verify-base"),
            ) {
                Ok(mut baseline) => {
                    // The declared setup first, in the base's own copy:
                    // its logs are evidence whatever happens next, and a
                    // setup that did not succeed leaves nothing for the
                    // commands to run against — blocked, before any
                    // worker is bought (SPEC §10).
                    let setup = self.while_watching_cancel(|cancel| {
                        self.setup_tree(
                            baseline.path(),
                            Some(setup_gate::SeedSource {
                                path: self.config.repo_dir,
                                wait: setup_gate::SeedWait::QuietFor(setup_gate::Quiet::CALLER),
                            }),
                            &authority.verification_profile,
                            &logs_dir,
                            "base",
                            cancel,
                        )
                    });
                    let setup = match setup {
                        Ok(setup) => setup,
                        Err(verify::VerifyError::Lease(stop)) => {
                            if let Err(e) = baseline.release() {
                                eprintln!("relais: {e}");
                            }
                            let nominal = fresh_progress(preflight).budget;
                            return Ok(Phase::Ended(self.end_on_lease_stop(stop, &nominal)?));
                        }
                        Err(e) => return Err(e.into()),
                    };
                    self.record_logs(None, EvidenceKind::SetupLog, &setup)?;
                    if let Some(outcome) = self.end_if_cancelled()? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    if let Some(outcome) = self.end_on_baseline_exhaustion(&setup)? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    if let Some(exhausted) = self.budget_cut_in(&setup) {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        let nominal = fresh_progress(preflight).budget;
                        return Ok(Phase::Ended(self.end_on_exhausted(exhausted, &nominal)?));
                    }
                    if let Some(failed) = verify::setup_failure(&setup) {
                        let detail = setup_failure_detail(failed, "the base revision");
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(
                            self.fail_preflight(BlockCode::VerificationSetupFailed, detail)?,
                        ));
                    }
                    let checks = self.while_watching_cancel(|cancel| {
                        verify::run_profile_within(
                            baseline.path(),
                            &authority.verification_profile,
                            &logs_dir,
                            "base",
                            &self.events,
                            cancel,
                            &self.heavy(),
                        )
                    });
                    let checks = match checks {
                        Ok(checks) => checks,
                        Err(verify::VerifyError::Lease(stop)) => {
                            if let Err(e) = baseline.release() {
                                eprintln!("relais: {e}");
                            }
                            let nominal = fresh_progress(preflight).budget;
                            return Ok(Phase::Ended(self.end_on_lease_stop(stop, &nominal)?));
                        }
                        Err(e) => return Err(e.into()),
                    };
                    self.record_logs(None, EvidenceKind::CheckLog, &checks)?;
                    if let Some(outcome) = self.end_if_cancelled()? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    // Exhaustion is evidence, never a red base: a failed
                    // check that shows a full disk or no memory gave no
                    // verdict (SPEC §10).
                    if let Some(outcome) = self.end_on_baseline_exhaustion(&checks)? {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(outcome));
                    }
                    // A check a budget of the run cut says nothing about the base.
                    if let Some(exhausted) = self.budget_cut_in(&checks) {
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        let nominal = fresh_progress(preflight).budget;
                        return Ok(Phase::Ended(self.end_on_exhausted(exhausted, &nominal)?));
                    }
                    // A check that gave no verdict at the base — the
                    // program was not found (exit 127), or the wall clock
                    // cut it off before it reported a status — leaves
                    // nothing to compare a candidate against, for exactly
                    // the same reason a missing program does: nothing to
                    // cache, and no worker can fix code that never ran.
                    // Blocked, with the remedy named — which is not
                    // always "add a setup". A candidate that times out is
                    // still judged as a failure (verify::CheckOutcome::
                    // failed); this reclassification happens only here,
                    // where the base is judged.
                    let unrunnable: Vec<&verify::CheckOutcome> = checks
                        .iter()
                        .filter(|check| {
                            verify::unrunnable(check)
                                || matches!(
                                    check.ended.recorded(),
                                    Some(Ended::TimedOut | Ended::Cancelled | Ended::Signalled)
                                        | None
                                )
                        })
                        .collect();
                    if !unrunnable.is_empty() {
                        let detail = unrunnable_baseline_detail(
                            self.config.repo_dir,
                            &self.config.contract.verification_profile,
                            &authority.verification_profile,
                            &preflight.base_sha,
                            &unrunnable,
                        );
                        if let Err(e) = baseline.release() {
                            eprintln!("relais: {e}");
                        }
                        return Ok(Phase::Ended(
                            self.fail_preflight(BlockCode::BaselineUnrunnable, detail)?,
                        ));
                    }
                    let failures: Vec<String> = checks
                        .iter()
                        .filter(|outcome| outcome.failed())
                        .map(|outcome| outcome.label.clone())
                        .collect();
                    if let (true, Some(key)) = (cacheable, &baseline_key) {
                        let stored = baseline_cache.put(key, &failures, &checks);
                        let what = if stored == verify::Stored::NotCached {
                            "baseline_not_cached"
                        } else {
                            "baseline_cache_outcomes_skipped"
                        };
                        let skipped = match stored {
                            verify::Stored::WithOutcomes => None,
                            verify::Stored::NotCached => Some(
                                "the base has a failed check; a base is cached only when every check passed"
                                    .to_string(),
                            ),
                            verify::Stored::OverLimit { bytes } => Some(format!(
                                "the base's logs came to {} MiB; only its green verdict is cached, without its logs",
                                bytes / (1024 * 1024)
                            )),
                            verify::Stored::LogsUnreadable => Some(
                                "a base log could not be read back to match its hash; only its green verdict is cached, without its logs"
                                    .to_string(),
                            ),
                            verify::Stored::NotWritten => Some(
                                "the baseline cache could not be written; the next run of this base runs its checks again"
                                    .to_string(),
                            ),
                        };
                        if let Some(reason) = skipped {
                            self.events.emit(Event::Decision {
                                what: what.into(),
                                reason,
                            });
                        }
                    }
                    if let Err(e) = baseline.release() {
                        // Best effort: the verdict is already in hand and
                        // the throwaway tree holds nothing but a checkout
                        // of the base revision.
                        eprintln!("relais: {e}");
                    }
                    (failures, Some(checks))
                }
                Err(e) => {
                    return Ok(Phase::Ended(self.fail_preflight(
                        BlockCode::BaselineVerificationFailed,
                        e.to_string(),
                    )?));
                }
            },
        };
        Ok(Phase::Ready(Baseline {
            failures,
            checks,
            cached: was_cached,
            cache_refused,
            logs_dir,
        }))
    }
}

pub(super) fn baseline_summary(baseline: &Baseline) -> String {
    let red = match baseline.failures.len() {
        0 => "green on base".to_string(),
        n => format!("{n} red on base"),
    };
    match &baseline.checks {
        _ if baseline.cached => format!("cached · {red}"),
        Some(checks) if checks.len() == 1 => format!("1 check · {red}"),
        Some(checks) => format!("{} checks · {red}", checks.len()),
        None => red,
    }
}

/// What a failed setup command says on the run's record: which command,
/// how it ended, where its log is, and where it was tried.
pub(super) fn setup_failure_detail(failed: &verify::CheckOutcome, where_: &str) -> String {
    format!(
        "the declared setup `{}` did not succeed in {where_} ({}); the profile's commands \
         did not run — see {}",
        failed.argv.join(" "),
        failed.ended.describe(),
        failed.log_path
    )
}

/// The wall limit a profile declared for the command behind this
/// label, if the label still names one of the profile's commands.
pub(super) fn command_timeout_seconds(profile: &VerificationProfile, label: &str) -> Option<u64> {
    profile
        .commands
        .iter()
        .find(|command| verify::check_label(command) == label)
        .map(|command| command.timeout_seconds)
}

/// What an unrunnable baseline says, and what to do about it. A check
/// that never reported a status splits into two different remedies: a
/// missing program (exit 127) depends on what was declared — with no
/// setup and a lockfile at the root, the missing program is most likely
/// the tree's own dependencies and the block that installs them is
/// spelled out; with a setup that succeeded and a program still missing,
/// adding another install step is the wrong move. A check the wall clock
/// or a signal cut off names the limit it ran against instead, so a
/// reader is pointed at contention or at a limit that is too low, not at
/// a base that is broken.
pub(super) fn unrunnable_baseline_detail(
    repo_dir: &Path,
    profile_name: &str,
    profile: &VerificationProfile,
    base_sha: &str,
    checks: &[&verify::CheckOutcome],
) -> String {
    let short = base_sha.get(..8).unwrap_or(base_sha);
    let mut not_found_labels: Vec<&str> = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    // Told apart because the remedy differs: a wall limit can be raised,
    // a kill cannot.
    let mut timed_out = false;
    let mut cut_off = false;
    for check in checks {
        match check.ended.recorded() {
            Some(Ended::Exited(_)) => {
                not_found_labels.push(&check.label);
                lines.push(format!(
                    "{} exited {} (command not found)",
                    check.label,
                    verify::COMMAND_NOT_FOUND
                ));
            }
            // The wall limit belongs to the ending that hit it. A
            // check the OOM killer took, or one that crashed, did not
            // run out of time, and quoting a limit it never reached
            // sends a reader to raise a number that was not the cause.
            Some(Ended::TimedOut) => {
                timed_out = true;
                let limit = command_timeout_seconds(profile, &check.label)
                    .map(|seconds| format!("{seconds}s wall limit"))
                    .unwrap_or_else(|| "no known wall limit".to_string());
                lines.push(format!("{} timed out ({limit})", check.label));
            }
            Some(Ended::Signalled | Ended::Cancelled) | None => {
                cut_off = true;
                lines.push(format!("{} {}", check.label, check.ended.describe()));
            }
        }
    }
    let mut detail = format!(
        "{} at the base revision {short}: the base has no verdict to compare a candidate \
         against. A verification worktree holds the repository's files and nothing else.",
        lines.join("; ")
    );
    if !not_found_labels.is_empty() {
        detail.push_str(
            " A missing program is not something a worker can put there; the exited-127 \
             check(s) above name it.",
        );
    }
    if timed_out {
        detail.push_str(
            " A check that did not finish before its wall limit is not a failure in the \
             code: either this machine was contended when it ran, or the limit is too low \
             for this check. A timeout that reproduces consistently against this exact base \
             revision, run alone, points at the limit; one that does not points at \
             contention.",
        );
    }
    if cut_off {
        detail.push_str(
            " A check that died without reporting a status did not run out of time: it was \
             killed or it crashed. The out-of-memory killer is the common cause on a \
             contended machine, and the check's own log is where the evidence is.",
        );
    }
    if not_found_labels.is_empty() {
        return detail;
    }
    if profile.setup.is_empty() {
        let found = crate::repo::lockfiles(repo_dir);
        if found.is_empty() {
            detail.push_str(
                " The profile declares no setup; if the commands need dependencies installed \
                 in the tree, declare the install step under \
                 [[verification.profiles.<name>.setup]] in relais.toml (this changes the policy \
                 hash: review it and re-grant trust from `relais plan`).",
            );
        } else {
            let suggestions = found
                .iter()
                .map(|ecosystem| {
                    format!(
                        "{} is present, suggesting:\n{}",
                        ecosystem.lockfile,
                        ecosystem.setup_block(profile_name)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            detail.push_str(&format!(
                " The profile declares no setup and {suggestions}\nA setup step is executable \
                 authority: adding it changes the policy hash, so review it and re-grant trust \
                 from `relais plan`."
            ));
        }
    } else {
        detail.push_str(&format!(
            " The profile's declared setup ({}) succeeded and the program is still not found: \
             the setup does not provide it, or it is not on PATH in a verification worktree. \
             Check what the setup installs rather than adding another install step.",
            profile
                .setup
                .iter()
                .map(|command| command.argv.join(" "))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    detail
}
