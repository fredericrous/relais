//! Judging a candidate: verification, settling and acceptance.

use std::collections::BTreeMap;
use std::path::Path;

use super::setup_gate;
use super::{
    review_record_of, AttemptContext, AttemptLabel, Budget, Candidate, Next, Observation,
    PendingEvidence, Progress, Reason, RepairFailure, ReviewOutcome, ReviewRequest, RunEngine,
    RunError, RunOutcome, State, Step, Terminal, Verified, VerifyFailure,
};
use crate::acceptance::Evidence;
use crate::contract::{Kind, Review};
use crate::ledger::{EvidenceKind, EvidenceOrigin, IntentReview, VerificationSnapshot};
use crate::policy::EffectiveAuthority;
use crate::route::Recipe;
use crate::verify::{self, amont_gaps, Receipt, VerificationReport};

/// What `candidate_receipt` builds a receipt from.
struct CandidateReceiptInput<'a> {
    attempt: &'a AttemptContext<'a>,
    progress: &'a Progress,
    candidate: &'a Candidate,
    verified: &'a Verified,
    verdict: Option<&'a verify::ReportVerdict>,
    outcome: State,
    review: verify::ReviewRecord,
}

impl<'a> RunEngine<'a> {
    /// Checks pass: semantic review where the risk asks for it, then a
    /// receipt bound to this candidate (SPEC §10, §12).
    pub(super) fn accept_candidate(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        candidate: &Candidate,
        verified: Verified,
        verdict: Option<&verify::ReportVerdict>,
    ) -> Result<Step, RunError> {
        let preflight = ctx.preflight;
        // An inspection has no patch to review: its report review has
        // already run (`judge_inspection`).
        // A `review-parts` answer asked for this review: the successor
        // reviews in parts whatever the contract's own review says.
        let in_parts = self
            .config
            .ledger
            .intent_of_run(&self.run_id)?
            .is_some_and(|intent| intent.review == Some(IntentReview::Parts));
        if (verified.review_required || in_parts) && verdict.is_none() {
            // A patch one reviewer cannot be given to read dispatches no
            // reviewer: the run asks a person (SPEC §10). A review in parts
            // is the answer to that question.
            if let Some(over) =
                super::review::over_budget(&candidate.latest_patch).filter(|_| !in_parts)
            {
                let review = verify::ReviewRecord::NotRun(verify::ReviewNotRun::OverBudget {
                    patch_bytes: over.patch_bytes,
                    budget_bytes: over.budget_bytes,
                });
                return self
                    .stop_for_review(
                        &progress.budget,
                        &|engine| {
                            engine.candidate_receipt(CandidateReceiptInput {
                                attempt: ctx,
                                progress,
                                candidate,
                                verified: &verified,
                                verdict,
                                outcome: State::NeedsDecision,
                                review: review.clone(),
                            })
                        },
                        Reason::ReviewOverBudget,
                        Observation::ReviewOverBudget(over),
                    )
                    .map(Step::Ended);
            }
            let request = ReviewRequest {
                manifest: &preflight.manifest,
                authority: &preflight.authority,
                candidate_sha: &candidate.sha,
                candidate_tier: candidate.tier,
                candidate_model: &candidate.model,
                verification_inputs_changed: &verified.verification_inputs_changed,
                patch_path: candidate.latest_patch.clone(),
                routed_by: preflight.decision.routed_by,
                covering: preflight.decision.covering.clone(),
                base_sha: &preflight.base_sha,
                holdouts: &verified.holdouts,
            };
            let review = if in_parts {
                self.review_candidate_in_parts(&request, &mut progress.spend)
            } else {
                self.review_candidate(&request, &mut progress.spend)
            };
            match review {
                ReviewOutcome::NoFindings => {}
                ReviewOutcome::Findings(detail) => {
                    // Only a change a worker wrote, reviewed whole, can be
                    // repaired: a `kind: verify` run and a review in parts
                    // have no worker to send findings back to, so their
                    // severities are withheld and the machine stops them.
                    // (Only a change reaches this review; a verify reports the
                    // `change` kind too, so `is_verify` is the test that matters.)
                    let repairable = !in_parts && !self.config.contract.is_verify();
                    let severities = repairable
                        .then(|| super::review::finding_severities(&detail))
                        .flatten();
                    let budget = super::spend::with_recorded_review_repairs(
                        progress.budget,
                        self.config.ledger,
                        &self.run_id,
                    )?;
                    let next = self.decide(
                        &budget,
                        Observation::ReviewFindings(detail.clone(), severities),
                    )?;
                    if matches!(next, Next::Attempt { .. }) {
                        progress.review_findings = Some(detail);
                    }
                    return self.proceed(ctx, progress, next);
                }
                ReviewOutcome::Unavailable(detail) => {
                    return self
                        .stop_for_review(
                            &progress.budget,
                            &|engine| {
                                engine.candidate_receipt(CandidateReceiptInput {
                                    attempt: ctx,
                                    progress,
                                    candidate,
                                    verified: &verified,
                                    verdict,
                                    outcome: State::NeedsDecision,
                                    review: engine.unavailable_review_record(&detail)?,
                                })
                            },
                            Reason::ReviewUnavailable,
                            Observation::ReviewUnavailable(detail.clone()),
                        )
                        .map(Step::Ended);
                }
            }
        }

        match self.decide(&progress.budget, Observation::ChecksAndReviewPassed)? {
            Next::Accept => {}
            other => {
                return Err(RunError::Other(format!(
                    "the machine answered {other:?} to a passing candidate"
                )))
            }
        }
        let review = review_record_of(self.config.ledger, &self.run_id)?;
        let receipt = self.candidate_receipt(CandidateReceiptInput {
            attempt: ctx,
            progress,
            candidate,
            verified: &verified,
            verdict,
            outcome: State::Accepted,
            review,
        })?;
        self.seal(
            &receipt,
            Some(candidate.attempt_id),
            Some(ctx.worktree_path),
            &candidate.sha,
        )?;
        Ok(Step::Ended(RunOutcome {
            run_id: self.run_id.clone(),
            terminal: Terminal::Accepted(Box::new(receipt)),
        }))
    }

    /// What the snapshot saved at a review that could not be had says of the
    /// review. A run that reviewed in parts lists every part with its
    /// verdict, the failing one included, so approving it seals a receipt
    /// that shows what was and was not reviewed; any other review is just
    /// unavailable, with its detail.
    fn unavailable_review_record(&self, detail: &str) -> Result<verify::ReviewRecord, RunError> {
        Ok(match review_record_of(self.config.ledger, &self.run_id)? {
            parts @ verify::ReviewRecord::Parts { .. } => parts,
            _ => verify::ReviewRecord::Unavailable {
                detail: detail.to_string(),
            },
        })
    }

    /// The receipt a candidate that passed its checks earns, `outcome` named
    /// by the caller: `accepted` for the run's own acceptance,
    /// `needs_decision` for the snapshot a run keeps when it stops for a
    /// review it could not have.
    fn candidate_receipt(&self, input: CandidateReceiptInput<'_>) -> Result<Receipt, RunError> {
        let CandidateReceiptInput {
            attempt,
            progress,
            candidate,
            verified,
            verdict,
            outcome,
            review,
        } = input;
        let preflight = attempt.preflight;
        let mut notes = verified.notes.clone();
        notes.extend(self.guidance_note(&preflight.manifest));
        let report = VerificationReport {
            notes,
            ..verification_report(attempt, candidate, verified)
        };
        let patch_adds_test = self.patch_adds_test(attempt, &candidate.sha)?;
        let (criteria, mandatory_evidence_independence) = self.settle(
            attempt,
            &report,
            &verified.gate_coverage,
            &verified.holdouts,
            patch_adds_test,
            verdict,
        )?;
        Ok(Receipt {
            run_id: self.run_id.as_str().to_string(),
            candidate_sha: candidate.sha.clone(),
            base_sha: preflight.base_sha.clone(),
            contract_hash: preflight.contract_hash.clone(),
            policy_hash: preflight.authority.authority_hash.clone(),
            outcome: outcome.as_str().to_string(),
            verification: report,
            kind: self.config.contract.kind(),
            candidate: preflight
                .candidate
                .as_ref()
                .map(|prepared| prepared.resolved.clone()),
            predecessor: preflight.predecessor.clone(),
            models_used: progress.models_used.clone(),
            attempts: candidate.index,
            cost_completeness: progress.spend.completeness,
            cost: progress.spend.total,
            criteria,
            mandatory_evidence_independence,
            recipe: crate::policy::RecipeRecord::of(
                preflight
                    .decision
                    .covering
                    .as_ref()
                    .map(Recipe::covering_identity),
            ),
            verification_profile_hash: preflight.authority.verification_profile.hash(),
            review,
            ladder: verify::LadderRecord::Single(preflight.decision.ladder.rungs().to_vec()),
            efforts_used: self.config.ledger.efforts_used(&self.run_id)?,
            candidates: self.candidates_record(&candidate.sha),
            holdouts: verified.holdouts.clone(),
        })
    }

    /// Save the snapshot a receipt would be sealed from, then stop the run
    /// on `observation`: the snapshot is durable before the run waits on a
    /// person, so an approval never has to recompute verification.
    pub(super) fn stop_for_review(
        &mut self,
        budget: &Budget,
        build: &dyn Fn(&Self) -> Result<Receipt, RunError>,
        reason: Reason,
        observation: Observation,
    ) -> Result<RunOutcome, RunError> {
        // Best effort: the stop is the run's outcome, the snapshot only
        // lets an approval seal a receipt. Building the receipt is inside the
        // same block, so a failure there is a step, never the run's end.
        // Without a snapshot, approve is refused by name (a change) or
        // resolves only (an older run), never invented.
        let saved = build(self).map_err(|e| e.to_string()).and_then(|receipt| {
            self.config
                .ledger
                .save_verification_snapshot(
                    &self.run_id,
                    &VerificationSnapshot {
                        candidate_sha: receipt.candidate_sha.clone(),
                        base_sha: receipt.base_sha.clone(),
                        contract_hash: receipt.contract_hash.clone(),
                        policy_hash: receipt.policy_hash.clone(),
                        stop_reason: reason.as_str().to_string(),
                        receipt: serde_json::to_value(&receipt).expect("a receipt serializes"),
                    },
                )
                .map_err(|e| e.to_string())
        });
        if let Err(e) = saved {
            self.step(
                "verification_snapshot_failed",
                format!("the verification snapshot could not be saved: {e}"),
            );
        }
        self.stop(budget, observation)
    }

    /// Settle the contract's criteria against a report, reading the
    /// sign-offs the ledger holds. `verdict` is an inspection's report
    /// review; a change has none.
    pub(super) fn settle(
        &self,
        ctx: &AttemptContext<'_>,
        report: &VerificationReport,
        gate_coverage: &BTreeMap<String, Result<bool, verify::AttestError>>,
        holdouts: &[verify::HoldoutRun],
        patch_adds_test: verify::PatchAddsTest,
        verdict: Option<&verify::ReportVerdict>,
    ) -> Result<
        (
            Vec<verify::CriterionOutcome>,
            Option<verify::IndependenceSummary>,
        ),
        RunError,
    > {
        let signoffs = self
            .config
            .ledger
            .human_signoffs(&self.run_id)?
            .into_iter()
            .map(|(criterion_id, _actor)| criterion_id)
            .collect();
        Ok(verify::settle_acceptance(
            &self.effective_acceptance()?,
            &ctx.preflight.authority.verification_profile,
            report,
            verify::Attestations {
                signoffs: &signoffs,
                gate_coverage,
                holdouts,
            },
            self.config.contract.kind(),
            patch_adds_test,
            verdict,
        ))
    }

    /// Whether `candidate_sha` adds a test, measured from the run's lower
    /// bound: the merged parent tree under `from_candidate`, else the
    /// base. A verify-only contract and an inspection have no worker patch
    /// to judge. A diff that cannot be read is an error, never a pass.
    pub(super) fn patch_adds_test(
        &self,
        ctx: &AttemptContext<'_>,
        candidate_sha: &str,
    ) -> Result<verify::PatchAddsTest, RunError> {
        if self.config.contract.is_verify() || self.config.contract.kind() == Kind::Inspect {
            return Ok(verify::PatchAddsTest::NotJudged);
        }
        let lower_bound = ctx
            .preflight
            .candidate
            .as_ref()
            .map_or(ctx.preflight.base_sha.as_str(), |prepared| {
                prepared.tree.as_str()
            });
        let patch = crate::workspace::derive_patch(ctx.worktree_path, lower_bound, candidate_sha)?;
        Ok(verify::PatchAddsTest::of_patch(&patch))
    }

    /// The gaps a change's declared-test criteria carry because its patch
    /// adds no test; empty when it adds one or none is declared.
    pub(super) fn missing_test_gaps(
        &self,
        ctx: &AttemptContext<'_>,
        candidate_sha: &str,
    ) -> Result<Vec<String>, RunError> {
        let adds = self.patch_adds_test(ctx, candidate_sha)?;
        Ok(verify::missing_test_gaps(
            &self.effective_acceptance()?,
            adds,
        ))
    }

    /// An inspection is judged by its report (SPEC §10): the checks and
    /// their gaps were already read as information, and one read-only
    /// review of the report settles the criteria no check names. In
    /// order, the first that holds decides: a red check criterion fails
    /// the run, an unmet report criterion is repaired once, a pending
    /// sign-off is a person's, and anything else is accepted.
    pub(super) fn judge_inspection(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        candidate: &Candidate,
        mut verified: Verified,
    ) -> Result<Step, RunError> {
        let preflight = ctx.preflight;
        if self.config.contract.review == Review::Off {
            verified.notes.push(
                "the contract says `review: off`, which governs the patch review only: an \
                 inspection has no patch, and its report review ran"
                    .into(),
            );
        }
        let request = ReviewRequest {
            manifest: &preflight.manifest,
            authority: &preflight.authority,
            candidate_sha: &candidate.sha,
            candidate_tier: candidate.tier,
            candidate_model: &candidate.model,
            verification_inputs_changed: &[],
            patch_path: candidate.latest_patch.clone(),
            routed_by: preflight.decision.routed_by,
            covering: preflight.decision.covering.clone(),
            base_sha: &preflight.base_sha,
            holdouts: &verified.holdouts,
        };
        // Not fatal: without the listing the reviewer works as it did before,
        // and the prompt says so.
        let tree = ctx.worktree.tracked_tree_in(&candidate.sha).ok();
        let verdict = match self.review_report(
            &request,
            candidate,
            ctx.worktree_path,
            tree.as_ref(),
            &mut progress.spend,
        ) {
            Ok(verdict) => verdict,
            Err(detail) => {
                return Ok(Step::Ended(self.stop_for_review(
                    &progress.budget,
                    &|engine| {
                        engine.candidate_receipt(CandidateReceiptInput {
                            attempt: ctx,
                            progress,
                            candidate,
                            verified: &verified,
                            verdict: None,
                            outcome: State::NeedsDecision,
                            review: verify::ReviewRecord::Unavailable {
                                detail: detail.clone(),
                            },
                        })
                    },
                    Reason::ReviewUnavailable,
                    Observation::ReviewUnavailable(detail.clone()),
                )?));
            }
        };
        let report = verification_report(ctx, candidate, &verified);
        let (criteria, _) = self.settle(
            ctx,
            &report,
            &verified.gate_coverage,
            &verified.holdouts,
            verify::PatchAddsTest::NotJudged,
            Some(&verdict),
        )?;
        let unmet: Vec<(usize, &verify::CriterionOutcome)> = criteria
            .iter()
            .enumerate()
            .filter(|(_, criterion)| criterion.mandatory && !criterion.met)
            .collect();
        let by_checks: Vec<String> = unmet
            .iter()
            .filter(|(_, criterion)| match criterion.settled_via {
                Some(verify::SettledVia::ProfileChecks | verify::SettledVia::DeclaredEvidence) => {
                    true
                }
                Some(verify::SettledVia::ReportReview | verify::SettledVia::HumanSignOff)
                | None => false,
            })
            .map(|(_, criterion)| {
                format!(
                    "criterion `{}` is not met: {}",
                    criterion.id,
                    unmet_evidence(criterion.evidence.as_ref())
                )
            })
            .collect();
        if !by_checks.is_empty() {
            return self.follow(ctx, progress, Observation::CheckCriteriaUnmet(by_checks));
        }
        let by_report: Vec<String> = unmet
            .iter()
            .filter(|(_, criterion)| {
                criterion.settled_via == Some(verify::SettledVia::ReportReview)
            })
            .map(|(at, criterion)| {
                format!(
                    "[{}] {}: {}",
                    at + 1,
                    criterion.statement,
                    verdict.of(*at).reason()
                )
            })
            .collect();
        if !by_report.is_empty() {
            progress.last_candidate = Some(candidate.sha.clone());
            progress.last_failures = Some(
                by_report
                    .iter()
                    .cloned()
                    .map(RepairFailure::Check)
                    .collect(),
            );
            return self.follow(ctx, progress, Observation::CriteriaUnmet(by_report));
        }
        if !unmet.is_empty() {
            // What is left is a sign-off no one has recorded: the gaps
            // listed are those alone, never the profile's.
            self.store_pending_receipt(
                ctx,
                progress,
                candidate,
                PendingEvidence {
                    checks: verified.checks,
                    gaps: verified.gaps.clone(),
                    gaps_not_judged: verified.gaps_not_judged,
                    notes: verified.notes,
                    gate_coverage: verified.gate_coverage,
                    holdouts: verified.holdouts,
                    verdict: Some(verdict),
                },
            )?;
            return Ok(Step::Ended(self.stop(
                &progress.budget,
                Observation::VerificationGap(verified.gaps),
            )?));
        }
        self.accept_candidate(ctx, progress, candidate, verified, Some(&verdict))
    }

    /// Verify the immutable candidate copy: a throwaway worktree at the
    /// candidate SHA, profile commands with logged, hashed evidence, and
    /// amont's inventory when the integration is on.
    pub(crate) fn verify_candidate(
        &self,
        candidate_sha: &str,
        authority: &EffectiveAuthority,
        logs_dir: &Path,
        label: AttemptLabel,
        reuse: Option<&[verify::CheckOutcome]>,
        resolved_holdouts: &[crate::holdout::ResolvedHoldout],
    ) -> Result<verify::Verified, VerifyFailure> {
        // amont's effective inventory (SPEC §10): consulted whenever the
        // integration is on, for the bypasses and downgrades it declares
        // and for the gaps among the checks this profile requires.
        let amont_on = self
            .config
            .repo_policy
            .integrations
            .amont
            .as_ref()
            .is_some_and(|dependency| dependency.mode() != crate::policy::DependencyMode::Off)
            && crate::tooling::integration_available("amont");
        // Every distinct gate a declared criterion names as its
        // evidence: asked through `amont attest covered`, on the
        // candidate's own tree, whenever there is at least one (SPEC
        // §10, §18).
        let acceptance = self.effective_acceptance().map_err(|e| e.to_string())?;
        let gate_names = verify::amont_gate_names(&acceptance);
        // The inventory is read INSIDE the immutable copy the receipt
        // binds to, never in the user's checkout — a hook the candidate
        // adds or removes must be seen as the candidate has it, and the
        // checkout can change under the run (audit V11). Gate coverage is
        // asked the same way, against the same tree, for the same
        // reason. So the throwaway worktree is created whenever the
        // checks, the inventory or a named gate needs it.
        let mut holder = match (reuse.is_none(), amont_on || !gate_names.is_empty()) {
            (false, false) => None,
            _ => {
                let verify_path = self
                    .verify_dir
                    .join(format!("verify-{}", label.worktree_suffix()));
                Some(
                    verify::verification_worktree(
                        self.config.git,
                        self.config.repo_dir,
                        candidate_sha,
                        &verify_path,
                    )
                    .map_err(VerifyFailure::from)?,
                )
            }
        };
        let mut setup_gap: Option<String> = None;
        let checks = match (reuse, holder.as_ref()) {
            // The candidate is the base tree: the baseline's outcomes are
            // its outcomes, by identity — and the base's setup already
            // proved the setup, so it is not spent again either.
            (Some(outcomes), _) => outcomes.to_vec(),
            (None, Some(holder)) => {
                let setup = self
                    .while_watching_cancel(|cancel| {
                        self.setup_tree(
                            holder.path(),
                            self.task_tree()
                                .as_deref()
                                .map(|path| setup_gate::SeedSource {
                                    path,
                                    wait: setup_gate::SeedWait::WritersEnded,
                                }),
                            &authority.verification_profile,
                            logs_dir,
                            &label.log_prefix(),
                            cancel,
                        )
                    })
                    .map_err(VerifyFailure::from)?;
                self.record_logs(None, EvidenceKind::SetupLog, &setup)
                    .map_err(|e| {
                        VerifyFailure::Unavailable(format!(
                            "the ledger refused a setup-log evidence row: {e}"
                        ))
                    })?;
                match verify::setup_failure(&setup) {
                    // A setup that did not complete at the candidate is a
                    // gap, not a failure: no check ran, so nothing was
                    // tested, and no stronger model installs
                    // dependencies (SPEC §10). If the candidate changed
                    // the lockfile, that is reported on its own.
                    Some(failed) => {
                        self.events.emit(crate::protocol::Event::Decision {
                            what: "verification_setup_gap".to_string(),
                            reason: format!(
                                "setup step {} did not complete ({})",
                                failed.label,
                                failed.ended.describe()
                            ),
                        });
                        setup_gap = Some(format!(
                            "verification setup did not complete: {} ({}); the profile's \
                             commands did not run — see {}",
                            failed.label,
                            failed.ended.describe(),
                            failed.log_path
                        ));
                        Vec::new()
                    }
                    None => self
                        .while_watching_cancel(|cancel| {
                            verify::run_profile_within(
                                holder.path(),
                                &authority.verification_profile,
                                logs_dir,
                                &label.log_prefix(),
                                &self.events,
                                cancel,
                                &self.heavy(),
                            )
                        })
                        .map_err(VerifyFailure::from)?,
                }
            }
            (None, None) => {
                return Err(VerifyFailure::Unavailable(
                    "a candidate to verify but no worktree to verify it in".to_string(),
                ))
            }
        };
        if let Some(gap) = setup_gap {
            if let Some(holder) = holder.as_mut() {
                if let Err(e) = holder.release() {
                    eprintln!("relais: {e}");
                }
            }
            return Ok(verify::Verified {
                gaps: vec![gap],
                ..Default::default()
            });
        }
        // A check log the ledger cannot point at is a check nobody can
        // audit, and acceptance rests on these (X5).
        for check in &checks {
            self.config
                .ledger
                .record_evidence(
                    &self.run_id,
                    None,
                    EvidenceKind::CheckLog,
                    Path::new(&check.log_path),
                    Some(&check.log_sha256),
                )
                .map_err(|e| format!("the ledger refused a check-log evidence row: {e}"))?;
            // A reused baseline outcome's report was already recorded when
            // the baseline ran; recording it again would add an identical
            // row the ledger has no constraint against.
            if reuse.is_none() {
                self.record_junit_report(check)
                    .map_err(|e| format!("the ledger refused a junit-report evidence row: {e}"))?;
            }
        }
        let inventory = match (amont_on, holder.as_ref()) {
            (true, Some(holder)) => self.config.hooks.list(holder.path(), verify::Stage::Local),
            _ => Err(verify::InventoryError::NotAsked),
        };
        // A profile that names no checks is not a profile that depends
        // on none: what it depends on is whatever amont is enforcing,
        // so the in-force blocking checks are the default required set
        // (audit B10).
        let required = if authority.verification_profile.amont_checks.is_empty() {
            // No inventory means no default required set to derive; the
            // failure itself is reported by `amont_waiver_gaps` below
            // and by the profile's own named checks when it has any.
            inventory
                .as_ref()
                .map(verify::default_required_checks)
                .unwrap_or_default()
        } else {
            authority.verification_profile.amont_checks.clone()
        };
        let mut gaps = if required.is_empty() {
            Vec::new()
        } else {
            amont_gaps(inventory.as_ref(), &required)
        };
        // A bypass or a downgrade is amont saying a check cannot fail.
        // That is a gap on this candidate unless policy waived it
        // ahead of the run — acceptance must not depend on a check that
        // was not enforcing (SPEC §10, audit B10).
        gaps.extend(verify::amont_waiver_gaps(
            inventory.as_ref(),
            &authority.verification_profile.amont_waivers,
        ));
        // amont was asked and would not answer, and the profile names no
        // check for `amont_gaps` to hang the reason on. Reported anyway:
        // a run must never read "no inventory" as "nothing to enforce"
        // (SPEC §10).
        if let Err(e) = &inventory {
            if required.is_empty() && !matches!(e, verify::InventoryError::NotAsked) {
                gaps.push(format!("amont inventory: {e}"));
            }
        }
        // A mandatory criterion whose named check produced no evidence
        // is a gap here too — the same mechanism that already refuses a
        // gap, not a second acceptance path (SPEC §10). A human sign-off
        // this run already carries (recorded by an earlier `relais
        // decide --answer approve --criterion <id>`) is not a gap either.
        let signoffs = self
            .config
            .ledger
            .human_signoffs(&self.run_id)
            .map_err(|e| format!("the ledger refused to read this run's sign-offs: {e}"))?
            .into_iter()
            .map(|(criterion_id, _actor)| criterion_id)
            .collect();
        // amont's answer about every gate a declared criterion names,
        // asked once per gate against the candidate's own tree. `None`
        // holder with gates named means the checks were reused from the
        // baseline and no worktree was made for this candidate: relais
        // still asked nothing about THIS tree, so that is a gap naming
        // why, never a silent pass.
        let gate_coverage: BTreeMap<String, Result<bool, verify::AttestError>> =
            if gate_names.is_empty() {
                BTreeMap::new()
            } else {
                match holder.as_ref() {
                    Some(holder) => {
                        verify::amont_gate_coverage(&gate_names, self.config.attest, holder.path())
                    }
                    None => gate_names
                        .iter()
                        .cloned()
                        .map(|gate| {
                            (
                                gate,
                                Err(verify::AttestError::NotRun {
                                    detail: "no verification worktree was available to ask \
                                             amont about this candidate's tree"
                                        .to_string(),
                                }),
                            )
                        })
                        .collect(),
                }
            };
        // A gate amont reports as covered is amont's own verdict about
        // this candidate: recorded as external attestation evidence,
        // against the run and the criterion it answers, so the receipt
        // and `relais explain` show what settled it and where it came
        // from (SPEC §10, §12).
        for entry in &acceptance {
            if let Some(crate::acceptance::Evidence::AmontGate { gate }) = entry.evidence() {
                if matches!(gate_coverage.get(gate), Some(Ok(true))) {
                    let criterion_id = entry.id();
                    self.config
                        .ledger
                        .attach_evidence(
                            &self.run_id,
                            Path::new(&format!("amont:attest:{gate}")),
                            None,
                            EvidenceOrigin {
                                tool: Some("amont"),
                                external_id: Some(gate),
                                subject: Some(candidate_sha),
                                criterion_id: Some(&criterion_id),
                            },
                        )
                        .map_err(|e| {
                            format!("the ledger refused an external-attestation evidence row: {e}")
                        })?;
                }
            }
        }
        // Holdouts run in a second copy made now that the profile's
        // commands have finished (ADR-0045). Their logs are not check logs.
        let holdouts = self.run_holdouts(candidate_sha, authority, label, resolved_holdouts)?;
        for entry in &acceptance {
            if let Some(crate::acceptance::Evidence::Holdout { name }) = entry.evidence() {
                let passed = holdouts
                    .iter()
                    .find(|run| run.name == *name && run.outcome == verify::HoldoutVerdict::Passed);
                if let Some(run) = passed {
                    let criterion_id = entry.id();
                    let external_id = format!("{}@{}", run.name, run.sha);
                    self.config
                        .ledger
                        .attach_evidence(
                            &self.run_id,
                            Path::new(&format!("relais-holdout:{external_id}")),
                            None,
                            EvidenceOrigin {
                                tool: Some("relais-holdout"),
                                external_id: Some(&external_id),
                                subject: Some(candidate_sha),
                                criterion_id: Some(&criterion_id),
                            },
                        )
                        .map_err(|e| format!("the ledger refused a holdout evidence row: {e}"))?;
                }
            }
        }
        // The profile is green when no check failed and no profile gap
        // stands (the acceptance gaps are added below, apart).
        let profile_green = gaps.is_empty() && checks.iter().all(|check| !check.failed());
        self.record_holdout_outcomes(label, &holdouts, profile_green)?;
        let acceptance_gaps = verify::acceptance_gaps(
            &acceptance,
            &authority.verification_profile,
            &checks,
            &signoffs,
            &gate_coverage,
            &holdouts,
        );
        gaps.extend(acceptance_gaps.iter().map(verify::AcceptanceGap::message));
        // The throwaway worktree has done its work. Releasing it here —
        // rather than leaving it to `Drop` — is what gives the failure
        // somewhere to be reported.
        if let Some(holder) = holder.as_mut() {
            if let Err(e) = holder.release() {
                eprintln!("relais: {e}");
            }
        }
        Ok(verify::Verified {
            checks,
            gaps,
            acceptance_gaps,
            amont_bypasses: inventory
                .as_ref()
                .map(|inventory| inventory.bypasses.clone())
                .unwrap_or_default(),
            amont_downgrades: inventory
                .as_ref()
                .map(|inventory| inventory.downgrades.clone())
                .unwrap_or_default(),
            gate_coverage,
            holdouts,
        })
    }
}

/// The report a candidate's checks, gaps and amont findings add up to.
pub(super) fn verification_report(
    ctx: &AttemptContext<'_>,
    candidate: &Candidate,
    verified: &Verified,
) -> VerificationReport {
    let preflight = ctx.preflight;
    VerificationReport {
        candidate_sha: candidate.sha.clone(),
        base_sha: preflight.base_sha.clone(),
        contract_hash: preflight.contract_hash.clone(),
        policy_hash: preflight.authority.authority_hash.clone(),
        checks: verified.checks.clone(),
        gaps: verified.gaps.clone(),
        gaps_not_judged: verified.gaps_not_judged.clone(),
        notes: verified.notes.clone(),
        baseline_failures: ctx.baseline.failures.clone(),
        amont_bypasses: verified.amont_bypasses.clone(),
        amont_downgrades: verified.amont_downgrades.clone(),
        verification_inputs_changed: verified.verification_inputs_changed.clone(),
        integration_gaps: preflight.integration_gaps.clone(),
        baseline_cached: ctx.baseline.cached,
        baseline_cache_refused: ctx.baseline.cache_refused.clone(),
    }
}

/// What an unmet criterion that no report can settle names as its
/// evidence, for the run's failure detail.
pub(super) fn unmet_evidence(evidence: Option<&Evidence>) -> String {
    match evidence {
        Some(Evidence::Check { name }) => format!("check `{name}` is not green"),
        Some(Evidence::Test {
            name: Some(test), ..
        }) => format!("test `{test}` did not pass"),
        Some(Evidence::AmontGate { gate }) => format!("amont gate `{gate}` is not covered"),
        Some(Evidence::Holdout { name }) => {
            format!("holdout `{name}` did not pass")
        }
        Some(Evidence::Test { name: None, .. } | Evidence::LlmReview | Evidence::HumanSignOff)
        | None => "its evidence is not met".into(),
    }
}
