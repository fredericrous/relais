//! The baseline of a run: the checks on the unmodified base.
//!
//! Up front only what is cheap: the baseline cache and amont's proof. A
//! base neither cached nor proven is deferred, and checked on demand when
//! a candidate goes red, on its red checks only (ADR-0013).

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::setup_gate;
use super::{
    AttemptContext, Baseline, BlockCode, Budget, Candidate, Ended, Event, EvidenceKind, Exhausted,
    KeptCandidate, Path, Phase, Preflight, Reason, RunEngine, RunError, RunOutcome, State,
    Terminal, VerificationProfile,
};
use crate::verify;

/// What the base checked on demand has established in this run
/// (ADR-0013). One per engine: the engine is borrowed mutably for the
/// whole run, so two red candidates never run the base at once, and
/// `running` refuses a re-entry. A decomposed run's packages each have
/// their own engine and their own `verify/<run>/verify-base`.
#[derive(Default)]
pub(crate) struct BaseMemo<'a> {
    /// The throwaway `verify-base` tree, kept for the run once made, and
    /// released when the run ends, however it ends.
    tree: Option<verify::VerificationWorktree<'a>>,
    /// The base's setup ran and succeeded in `tree`: it runs once per run.
    set_up: bool,
    /// Every red label already asked of the base, a label with no command
    /// on the base profile included: none is run twice.
    asked: BTreeSet<String>,
    /// The base's outcomes for the labels that ran, with their JUnit.
    checks: Vec<verify::CheckOutcome>,
    /// The labels among `checks` that failed at the base.
    failures: Vec<String>,
    /// A base run is under way.
    running: bool,
}

/// How an on-demand base run ended when it gave no verdict.
enum BaseEnding {
    /// A command was never admitted (a budget, a cancel, the coordinator).
    Lease(verify::LeaseStop),
    /// The run was cancelled between commands.
    Cancelled,
    /// A budget ran out under a base check (#329).
    Budget(Exhausted),
    /// A failed base check or setup step showed exhaustion.
    Exhaustion(
        String,
        crate::exhaustion::Exhaustion,
        crate::exhaustion::Measured,
    ),
    /// Blocked, with the block code and what it says.
    Blocked(BlockCode, String),
}

/// What running the base's commands gave.
enum BaseRan {
    Checks(Vec<verify::CheckOutcome>),
    Ended(BaseEnding),
}

impl<'a> RunEngine<'a> {
    /// The baseline's cheap half (SPEC §10, ADR-0013): a cache hit is
    /// served, a base amont proved green is not run, and any other base is
    /// deferred to the first red candidate. The cache is read, never
    /// written: an on-demand run checks red commands only, never the full
    /// green base the cache stores (SPEC §18).
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
        let toolchain_identified = toolchain.is_ok();
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
        // A hit that kept the outcomes has written their logs into this
        // run's `logs/`: they are this run's evidence like any check's.
        if let Some(hit) = cached {
            let checks = hit.checks.map(|checks| {
                checks
                    .into_iter()
                    .map(verify::CheckOutcome::reused)
                    .collect::<Vec<_>>()
            });
            if let Some(checks) = &checks {
                self.record_logs(None, EvidenceKind::CheckLog, checks)?;
                self.record_check_timings("base", checks)?;
            }
            return Ok(Phase::Ready(Baseline {
                failures: hit.failures,
                checks,
                cached: true,
                cache_refused,
                proven: false,
                deferred: false,
                logs_dir,
            }));
        }
        // A base amont already proved green on this platform — every gate
        // the profile runs through `amont run --stamp` is stamped or
        // attested on the base tree — is not run (decisions programme
        // "one verdict per tree", t04). A proof writes nothing to the
        // cache.
        if self.amont_on() {
            let gates = verify::stamps::profile_gates(&authority.verification_profile);
            if !gates.is_empty() {
                let answer =
                    self.config
                        .hooks
                        .stamps(self.config.repo_dir, Some(&base_tree), &gates);
                // An identified toolchain, as the cache requires: a stamp
                // carries none (see `base_is_proven`).
                if toolchain_identified && verify::stamps::base_is_proven(&gates, &answer) {
                    self.events.emit(Event::Decision {
                        what: "baseline_proven".into(),
                        reason: format!(
                            "amont reports {} stamped or attested on base tree {base_tree}; the base is green by that proof and was not run",
                            gates.join(", ")
                        ),
                    });
                    return Ok(Phase::Ready(Baseline {
                        failures: Vec::new(),
                        checks: None,
                        cached: false,
                        cache_refused,
                        proven: true,
                        deferred: false,
                        logs_dir,
                    }));
                }
            }
        }
        // Proof, else lazy (ADR-0013): the worker starts at once, and the
        // base is checked only if a candidate goes red, on its red checks.
        self.events.emit(Event::Decision {
            what: "baseline_deferred".into(),
            reason: format!(
                "base tree {base_tree} is neither cached nor proven by amont; it is checked only \
                 when a candidate fails, and only on the checks that failed"
            ),
        });
        Ok(Phase::Ready(Baseline {
            failures: Vec::new(),
            checks: None,
            cached: false,
            cache_refused,
            proven: false,
            deferred: true,
            logs_dir,
        }))
    }

    /// The labels that fail at the base: the baseline's, and what the
    /// base checked on demand found so far. The one view every consumer
    /// reads (ADR-0013).
    pub(super) fn base_failures(&self, baseline: &Baseline) -> Vec<String> {
        let mut all = baseline.failures.clone();
        for label in &self.base_memo.failures {
            if !all.contains(label) {
                all.push(label.clone());
            }
        }
        all
    }

    /// The base's outcomes, with their JUnit: the baseline's when its
    /// commands ran or a hit kept them, else those checked on demand.
    pub(super) fn base_checks<'s>(
        &'s self,
        baseline: &'s Baseline,
    ) -> Option<&'s [verify::CheckOutcome]> {
        match &baseline.checks {
            Some(checks) => Some(checks),
            None if !self.base_memo.checks.is_empty() => Some(&self.base_memo.checks),
            None => None,
        }
    }

    /// Remove the `verify-base` tree an on-demand run made: at the run's
    /// end, whatever the ending.
    pub(super) fn release_base_tree(&mut self) {
        if let Some(mut tree) = self.base_memo.tree.take() {
            if let Err(e) = tree.release() {
                eprintln!("relais: {e}");
            }
        }
    }

    /// A candidate whose tree is the base's has the base's results by
    /// identity: its own checks with a verdict are the base's, so nothing
    /// runs on the base for them (ADR-0013). A check with no verdict (a
    /// missing program, a timeout) says nothing about the base and stays
    /// a failure of the candidate.
    pub(super) fn base_is_candidate(&mut self, checks: &[verify::CheckOutcome]) {
        for check in checks {
            if !self.base_memo.asked.insert(check.label.clone()) {
                continue;
            }
            let verdict = matches!(check.ended.recorded(), Some(Ended::Exited(_)))
                && !verify::unrunnable(check);
            if !verdict {
                continue;
            }
            if check.failed() {
                self.base_memo.failures.push(check.label.clone());
            }
            self.base_memo.checks.push(check.clone());
        }
    }

    /// Check the base on demand for a red candidate's red labels
    /// (ADR-0013): the base's setup once per run, then only the profile's
    /// commands with those labels not asked before, each logged under its
    /// profile index. Its commands draw on the total budget only, and
    /// their time is taken out of work (ADR-0010). `None` when the base
    /// gave its verdict, which the merged view then holds; otherwise the
    /// run's end, with the candidate kept.
    pub(super) fn base_on_demand(
        &mut self,
        ctx: &AttemptContext<'_>,
        candidate: &Candidate,
        red: &[String],
        budget: &Budget,
    ) -> Result<Option<RunOutcome>, RunError> {
        let wanted: Vec<String> = red
            .iter()
            .filter(|label| !self.base_memo.asked.contains(*label))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if wanted.is_empty() {
            return Ok(None);
        }
        if self.base_memo.running {
            return Err(RunError::Other(
                "the base was asked to run on demand while it was already running".into(),
            ));
        }
        let profile = &ctx.preflight.authority.verification_profile;
        // A red label with no command on the base profile is new: nothing
        // at the base can have failed it.
        let selected = verify::commands_labelled(profile, &wanted);
        self.base_memo.asked.extend(wanted);
        if selected.is_empty() {
            return Ok(None);
        }
        let count = selected.len();
        self.step(
            "baseline",
            format!(
                "on demand · {count} check{}",
                if count == 1 { "" } else { "s" }
            ),
        );
        self.base_memo.running = true;
        let normal = self.budgets.clone();
        let mark = normal.base_mark();
        self.budgets = normal.for_base();
        let ran = self.run_base(ctx.preflight, profile, &selected, &ctx.baseline.logs_dir);
        self.budgets = normal;
        self.budgets.charge_base(mark);
        self.base_memo.running = false;
        let checks = match ran? {
            BaseRan::Checks(checks) => checks,
            BaseRan::Ended(ending) => {
                return self
                    .end_base_on_demand(ctx, candidate, budget, ending)
                    .map(Some);
            }
        };
        // A base check's timeout is clipped to what is left of the total:
        // when the total ran out, it was the run's own budget that ended
        // it, not the check's limit, and the run ends on that budget
        // (#329). Read on the run's own budgets, now the base's time is
        // out of work.
        let timed_out = checks
            .iter()
            .any(|check| check.ended.recorded() == Some(Ended::TimedOut));
        if let Some(exhausted) = timed_out.then(|| self.budgets.exhausted()).flatten() {
            return self
                .end_base_on_demand(ctx, candidate, budget, BaseEnding::Budget(exhausted))
                .map(Some);
        }
        // A check that gave no verdict at the base leaves nothing to tell
        // the candidate's failure new from pre-existing (SPEC §10).
        let unrunnable: Vec<&verify::CheckOutcome> = checks
            .iter()
            .filter(|check| {
                verify::unrunnable(check)
                    || matches!(
                        check.ended.recorded(),
                        Some(Ended::TimedOut | Ended::Cancelled | Ended::Signalled) | None
                    )
            })
            .collect();
        if !unrunnable.is_empty() {
            let detail = unrunnable_baseline_detail(
                self.config.repo_dir,
                &self.config.contract.verification_profile,
                profile,
                &ctx.preflight.base_sha,
                &unrunnable,
            );
            return self
                .end_base_on_demand(
                    ctx,
                    candidate,
                    budget,
                    BaseEnding::Blocked(BlockCode::BaselineUnrunnable, detail),
                )
                .map(Some);
        }
        let red_on_base = checks.iter().filter(|check| check.failed()).count();
        for check in &checks {
            if check.failed() && !self.base_memo.failures.contains(&check.label) {
                self.base_memo.failures.push(check.label.clone());
            }
        }
        self.base_memo.checks.extend(checks);
        self.step("baseline", checks_summary(Some(count), red_on_base));
        Ok(None)
    }

    /// The base's tree, its setup once, then the chosen commands.
    fn run_base(
        &mut self,
        preflight: &Preflight,
        profile: &VerificationProfile,
        selected: &[(usize, &crate::policy::CommandSpec)],
        logs_dir: &Path,
    ) -> Result<BaseRan, RunError> {
        if self.base_memo.tree.is_none() {
            match verify::verification_worktree(
                self.config.git,
                self.config.repo_dir,
                &preflight.base_sha,
                &self.verify_dir.join("verify-base"),
            ) {
                Ok(tree) => self.base_memo.tree = Some(tree),
                Err(e) => {
                    return Ok(BaseRan::Ended(BaseEnding::Blocked(
                        BlockCode::BaselineVerificationFailed,
                        e.to_string(),
                    )))
                }
            }
        }
        let path: PathBuf = match &self.base_memo.tree {
            Some(tree) => tree.path().to_path_buf(),
            None => return Err(RunError::Other("the base tree vanished".into())),
        };
        if !self.base_memo.set_up {
            // The declared setup first, in the base's own copy: its logs
            // are evidence whatever happens next, and a setup that did not
            // succeed leaves nothing for the commands to run against.
            let setup = self.while_watching_cancel(|cancel| {
                self.setup_tree(
                    &path,
                    Some(setup_gate::SeedSource {
                        path: self.config.repo_dir,
                        wait: setup_gate::SeedWait::QuietFor(setup_gate::Quiet::CALLER),
                    }),
                    profile,
                    logs_dir,
                    "base",
                    cancel,
                )
            });
            let setup = match setup {
                Ok(setup) => setup,
                Err(verify::VerifyError::Lease(stop)) => {
                    return Ok(BaseRan::Ended(BaseEnding::Lease(stop)))
                }
                Err(e) => return Err(e.into()),
            };
            self.record_logs(None, EvidenceKind::SetupLog, &setup)?;
            self.record_check_timings("base-setup", &setup)?;
            if self.cancel_requested() {
                return Ok(BaseRan::Ended(BaseEnding::Cancelled));
            }
            if let Some((label, found, measured)) = self.exhaustion_in(&setup) {
                return Ok(BaseRan::Ended(BaseEnding::Exhaustion(
                    label, found, measured,
                )));
            }
            if let Some(failed) = verify::setup_failure(&setup) {
                return Ok(BaseRan::Ended(BaseEnding::Blocked(
                    BlockCode::VerificationSetupFailed,
                    setup_failure_detail(failed, "the base revision"),
                )));
            }
            self.base_memo.set_up = true;
        }
        let checks = self.while_watching_cancel(|cancel| {
            verify::run_indexed_within(
                &path,
                selected.iter().copied(),
                logs_dir,
                "base",
                &self.events,
                cancel,
                &self.heavy(),
            )
        });
        let checks = match checks {
            Ok(checks) => checks,
            Err(verify::VerifyError::Lease(stop)) => {
                return Ok(BaseRan::Ended(BaseEnding::Lease(stop)))
            }
            Err(e) => return Err(e.into()),
        };
        self.record_logs(None, EvidenceKind::CheckLog, &checks)?;
        self.record_check_timings("base", &checks)?;
        if self.cancel_requested() {
            return Ok(BaseRan::Ended(BaseEnding::Cancelled));
        }
        // Exhaustion is evidence, never a red base: a failed check that
        // shows a full disk or no memory gave no verdict (SPEC §10).
        if let Some((label, found, measured)) = self.exhaustion_in(&checks) {
            return Ok(BaseRan::Ended(BaseEnding::Exhaustion(
                label, found, measured,
            )));
        }
        Ok(BaseRan::Checks(checks))
    }

    /// End the run on an on-demand base run that gave no verdict. A worker
    /// was paid by now: the attempt is closed with its candidate, which the
    /// message says is kept and how to verify it again.
    fn end_base_on_demand(
        &mut self,
        ctx: &AttemptContext<'_>,
        candidate: &Candidate,
        budget: &Budget,
        ending: BaseEnding,
    ) -> Result<RunOutcome, RunError> {
        self.release_base_tree();
        let cancelled = matches!(
            ending,
            BaseEnding::Cancelled | BaseEnding::Lease(verify::LeaseStop::Cancelled)
        );
        self.config.ledger.finish_attempt(
            candidate.attempt_id,
            if cancelled {
                State::Cancelled
            } else {
                State::Blocked
            },
            Some(&ctx.worktree_path.to_string_lossy()),
            Some(&candidate.sha),
        )?;
        let kept = self.kept_candidate(ctx.preflight, candidate.index, &candidate.sha);
        let after = format!(
            "; the base was checked on demand after attempt {} went red, so a worker was paid: {}",
            candidate.index,
            kept_sentence(&kept)
        );
        let kept_json =
            serde_json::json!({ "run": kept.run, "index": kept.index, "sha": kept.sha });
        match ending {
            BaseEnding::Lease(stop) => self.end_on_lease_stop(stop, budget, Some(&kept)),
            BaseEnding::Budget(exhausted) => self.end_on_exhausted(exhausted, budget, Some(&kept)),
            BaseEnding::Cancelled => match self.end_if_cancelled()? {
                Some(outcome) => Ok(outcome),
                None => Err(RunError::Other(
                    "the base run stopped for a cancel the coordinator no longer reports".into(),
                )),
            },
            BaseEnding::Exhaustion(label, found, measured) => {
                let detail = format!(
                    "{}{after}",
                    crate::exhaustion::baseline_on_demand_text(&label, &found, &measured)
                );
                let mut json =
                    crate::exhaustion::detail_json(&label, &found, &measured, Some(&candidate.sha));
                json["kept"] = kept_json;
                self.finish(
                    Reason::BaselineResourceExhausted,
                    json,
                    Terminal::BlockedBy {
                        reason: Reason::BaselineResourceExhausted,
                        detail,
                    },
                )
            }
            BaseEnding::Blocked(code, detail) => {
                let detail = format!("{detail}{after}");
                self.finish(
                    Reason::BlockedPreflight,
                    serde_json::json!({ "code": code, "detail": detail, "candidate": kept_json }),
                    Terminal::Blocked { code, detail },
                )
            }
        }
    }

    /// Whether this run asks amont at all: the integration is on in policy
    /// and the binary is on this machine.
    pub(super) fn amont_on(&self) -> bool {
        self.config
            .repo_policy
            .integrations
            .amont
            .as_ref()
            .is_some_and(|dependency| dependency.mode() != crate::policy::DependencyMode::Off)
            && crate::tooling::integration_available("amont")
    }
}

pub(super) fn baseline_summary(baseline: &Baseline) -> String {
    if baseline.deferred {
        return "deferred".to_string();
    }
    let checks = baseline.checks.as_ref().map(Vec::len);
    let summary = checks_summary(checks, baseline.failures.len());
    if baseline.cached {
        let red = checks_summary(None, baseline.failures.len());
        return format!("cached · {red}");
    }
    summary
}

/// `N checks · M red on base`, or the red part alone with no count.
fn checks_summary(checks: Option<usize>, red: usize) -> String {
    let red = match red {
        0 => "green on base".to_string(),
        n => format!("{n} red on base"),
    };
    match checks {
        Some(1) => format!("1 check · {red}"),
        Some(n) => format!("{n} checks · {red}"),
        None => red,
    }
}

/// What a kept candidate's ending says of it: it is kept, and how to
/// verify it again without a worker.
pub(super) fn kept_sentence(kept: &KeptCandidate) -> String {
    format!(
        "the candidate is kept: verify it again without a worker with a `kind: verify` run, \
         `candidate {{run: {}, index: {}}}`",
        kept.run, kept.index
    )
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
