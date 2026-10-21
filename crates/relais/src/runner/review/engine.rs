//! The engine side of review: dispatching reviewers and booking them.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::backend::{LaunchResult, LaunchSpec};
use crate::context::GuidanceGap;
use crate::ledger::{EvidenceKind, Ledger, UsageEvent};
use crate::money::MicroUsd;
use crate::protocol::AgentKind;
use crate::route::RungIndex;
use crate::verify;
use crate::workspace;

use super::{
    read_patch, report_review_prompt, review_verdict, reviewer_tier, ReviewOutcome, ReviewPurpose,
    ReviewRequest, ReviewSeat, REVIEW_MIN_WALL, REVIEW_PATCH_BUDGET_BYTES,
};
use crate::runner::{
    data_block, data_list_block, effort_str, reviewer_guidance, utc_day_start, Budget, Candidate,
    Ceiling, ManagedDispatch, Reason, RunEngine, RunError, RunSpend, State,
};

impl<'a> RunEngine<'a> {
    /// An inspection's verification: one read-only dispatch that checks
    /// the worker's report against the files in the candidate worktree,
    /// on the worker's own tier and model. `Err` is the reason it could
    /// not be had, which ends the run `needs_review`, never accepted.
    pub(in crate::runner) fn review_report(
        &mut self,
        request: &ReviewRequest<'_>,
        candidate: &Candidate,
        worktree: &Path,
        tree: Option<&workspace::TrackedTree>,
        spend: &mut RunSpend,
    ) -> Result<verify::ReportVerdict, String> {
        let Some(profile) = request
            .authority
            .models
            .get(&request.candidate_tier)
            .cloned()
        else {
            return Err(format!(
                "no model configured at the {} tier to review the report",
                request.candidate_tier.as_str()
            ));
        };
        if let Some(exhausted) = self.review_spend_blocked(spend.total) {
            return Err(exhausted);
        }
        let statements = self.config.contract.acceptance_statements();
        let report = candidate.result_text.as_deref().unwrap_or_default();
        let prompt =
            report_review_prompt(&self.config.contract.objective, &statements, report, tree);
        let seat = ReviewSeat {
            profile: &profile,
            tier: request.candidate_tier,
            dir: worktree.to_path_buf(),
            purpose: ReviewPurpose::Report,
        };
        let result = match self.dispatch_reviewer(request, seat, prompt, spend) {
            Ok(result) => result,
            Err(ReviewOutcome::Unavailable(detail)) => return Err(detail),
            Err(ReviewOutcome::NoFindings | ReviewOutcome::Findings(_)) => {
                return Err("the report review dispatch answered with a verdict".into());
            }
        };
        let text = result.result_text.unwrap_or_default();
        self.record_artifact(
            Some(candidate.attempt_id),
            EvidenceKind::ReviewResult,
            &self
                .artifacts
                .join(format!("report-review-{}.txt", candidate.index)),
            &text,
        )
        .map_err(|e| format!("the report review could not be recorded as evidence: {e}"))?;
        Ok(verify::ReportVerdict::parse(&text, statements.len()))
    }

    /// Is a spending ceiling already reached, so the review must not be
    /// dispatched? The message names the ceiling; the caller turns it
    /// into `needs_review`, never an acceptance.
    pub(crate) fn review_spend_blocked(&self, total_cost: MicroUsd) -> Option<String> {
        let spending = &self.config.machine.spending;
        if let Some(ceiling) = spending.per_run_micros {
            if total_cost >= ceiling {
                let (spent, ceiling) = Ceiling::PerRun.render(total_cost, ceiling);
                return Some(format!(
                    "the spend ceiling was reached before the review could be dispatched \
                     ({spent} of {ceiling}); the candidate is unreviewed"
                ));
            }
        }
        if let Some(ceiling) = spending.per_day_micros {
            let today = match self.spent_today(self.config.ledger) {
                Ok(today) => today,
                Err(e) => return Some(format!("the day's spend could not be read: {e}")),
            };
            if today >= ceiling {
                let (spent, ceiling) = Ceiling::PerDay.render(today, ceiling);
                return Some(format!(
                    "the spend ceiling was reached before the review could be dispatched \
                     ({spent} of {ceiling}); the candidate is unreviewed"
                ));
            }
        }
        None
    }

    /// What this machine has spent since the start of the current UTC
    /// day, by the ledger's own clock — the figure a `per_day_micros`
    /// ceiling bounds. Every run on this machine counts, because the
    /// ceiling is the machine's.
    pub(crate) fn spent_today(&self, ledger: &Ledger) -> Result<MicroUsd, RunError> {
        let now = ledger.now();
        // The clock is the ledger's own, so an unparseable stamp is a
        // defect, not input: refusing to run beats running with a
        // ceiling that silently cannot be applied.
        let day_start = utc_day_start(&now).ok_or_else(|| {
            RunError::Other(format!(
                "the ledger clock answered {now:?}, which is not an RFC3339 timestamp; \
                 the daily spend ceiling has no day to measure"
            ))
        })?;
        Ok(ledger.spend_since(&day_start)?)
    }

    /// Where a reviewer reads the candidate from: the run's own task
    /// worktree, the assembled integration worktree when the run was
    /// decomposed, and the repository itself when neither exists (a
    /// reviewer with a working directory that is not there cannot even
    /// be launched).
    pub(crate) fn review_dir(&self) -> PathBuf {
        for candidate in [
            self.worktrees.join("task"),
            self.worktrees.join("integration"),
        ] {
            if candidate.is_dir() {
                return candidate;
            }
        }
        self.config.repo_dir.to_path_buf()
    }

    /// One separate review call per candidate requiring it (SPEC §9, §10).
    /// The reviewer cannot edit or waive anything; findings are triaged,
    /// and "no findings" is recorded as evidence, not proof. A reviewer
    /// the runner cannot dispatch or record is `Unavailable`: the run
    /// ends needs_review, not accepted.
    ///
    /// Four steps, each its own function: who reviews and what they are
    /// asked (`review_prompt`), the dispatch, the accounting and the
    /// evidence, and the verdict read out of the answer.
    pub(crate) fn review_candidate(
        &mut self,
        request: &ReviewRequest<'_>,
        spend: &mut RunSpend,
    ) -> ReviewOutcome {
        // A reviewer gap belongs to the review that made it: a later
        // review that ends before it builds its prompt carries none.
        self.reviewer_gap = None;
        let Some((reviewer_tier, same_model)) = reviewer_tier(
            request.authority,
            request.candidate_tier,
            request.candidate_model,
        ) else {
            return ReviewOutcome::Unavailable(
                "no reviewer model is configured at any tier".into(),
            );
        };
        // The review is a dispatch and costs money like any other. It is
        // reached after the loop-top check, with the attempt that
        // produced this candidate already settled, so the run can be
        // exhausted here even though it was not when the loop last
        // looked: without this the reviewer launched with a budget of
        // zero and the run was accepted on a review nobody paid for
        // (SPEC §11: stop admitting work once the ceiling is reached).
        if let Some(exhausted) = self.review_spend_blocked(spend.total) {
            return ReviewOutcome::Unavailable(exhausted);
        }
        self.step("review", format!("{} tier", reviewer_tier.as_str()));
        let Some(profile) = request.authority.models.get(&reviewer_tier).cloned() else {
            return ReviewOutcome::Unavailable(format!(
                "no reviewer model configured at the {} tier",
                reviewer_tier.as_str()
            ));
        };
        // "A separate reviewer" (SPEC §10) is separate in fact, not just
        // in dispatch: when the model that wrote the candidate is the one
        // reviewing it — an escalated run, or a recipe that put the
        // escalation model on the worker's tier — asking it for findings
        // asks it about its own work. The review still happens — a second
        // opinion from the same model is worth more than none — but the
        // run records that it was not independent (audit B14).
        if same_model {
            if let Err(e) = self.transition(
                State::Verifying,
                Reason::ReviewerSameModel,
                serde_json::json!({
                    "reviewer_same_model": true,
                    "model": profile.id,
                    "tier": reviewer_tier.as_str(),
                    "candidate": request.candidate_sha,
                }),
            ) {
                return ReviewOutcome::Unavailable(format!(
                    "the ledger refused the reviewer-tier record: {e}"
                ));
            }
        }
        let review_dir = self.review_dir();
        let (prompt, reviewer_gap) = self.review_prompt(request, &review_dir);
        if let Some(gap) = &reviewer_gap {
            self.emit_guidance_gap(gap);
        }
        self.reviewer_gap = reviewer_gap;
        let seat = ReviewSeat {
            profile: &profile,
            tier: reviewer_tier,
            dir: review_dir,
            purpose: ReviewPurpose::Patch,
        };
        let result = match self.dispatch_reviewer(request, seat, prompt, spend) {
            Ok(result) => result,
            Err(unavailable) => return unavailable,
        };
        let text = result.result_text.unwrap_or_default();
        // The artifact and its evidence row are one operation: a review
        // the run cannot record is a review it cannot show, and a run
        // that cannot show its review does not accept (X5).
        if let Err(e) = self.record_artifact(
            None,
            EvidenceKind::ReviewResult,
            &self.artifacts.join("review.txt"),
            &text,
        ) {
            return ReviewOutcome::Unavailable(format!(
                "the review could not be recorded as evidence: {e}"
            ));
        }
        review_verdict(&text)
    }

    /// What the reviewer is asked. Every piece of project text — the
    /// objective, the criteria, the constraints, the changed paths — is
    /// quoted as data, never as instructions (SPEC §16).
    fn review_prompt(
        &self,
        request: &ReviewRequest<'_>,
        review_dir: &Path,
    ) -> (String, Option<GuidanceGap>) {
        let mut prompt = String::from(
            "You are a semantic reviewer. You cannot edit or waive checks; you report findings only.\n\
             Report each finding with file/range, the violated acceptance criterion, evidence, and suggested verification.\n\
             You may only run read-only commands (reading files, git log/diff/show, searching). \
             Do not build, test or run checks: the runner has run the profile's checks already, \
             and a command you could not run is not a finding.\n\
             For every fact this change RECORDS, ask where else that same fact is already \
             written — a ledger row and a file on disk, a run's state and its receipt, a \
             transition and a column derived from it — and whether the change keeps them in \
             step. A test that exercises only the in-memory value passes while the stored \
             copies disagree, so the passing checks above are not evidence about this.\n\
             Ask the same of what it READS: a record appended after a run is already terminal \
             (a retired worktree writes a same-state transition) means the NEWEST row matching \
             a state is usually not the event that caused it.\n",
        );
        prompt.push('\n');
        prompt.push_str(&data_block("objective", &self.config.contract.objective));
        prompt.push_str(&data_list_block(
            "acceptance criteria",
            &self.config.contract.acceptance_statements(),
        ));
        if !request.manifest.constraints.is_empty() {
            prompt.push_str(&data_list_block(
                "architectural constraints",
                &request.manifest.constraints,
            ));
        }
        // The reviewer judges the change against the same design the
        // worker was handed.
        if let Some(design) = &request.manifest.design {
            prompt.push_str(&data_block("program design", design));
        }
        // The patch is read first: the constraint statements share its
        // budget, and the patch is what the reviewer cannot do without.
        let patch = read_patch(&request.patch_path, REVIEW_PATCH_BUDGET_BYTES);
        let patch_bytes = patch.as_ref().map_or(0, String::len);
        let (guidance_text, reviewer_gap) = reviewer_guidance(
            &request.manifest.guidance,
            REVIEW_PATCH_BUDGET_BYTES.saturating_sub(patch_bytes),
        );
        prompt.push_str(&guidance_text);
        if !request.verification_inputs_changed.is_empty() {
            prompt.push_str(
                "this candidate CHANGES THE TEST TREE (tests or fixtures the checks execute). \
                 Adding regression tests is expected work; judge whether each change weakens \
                 what the acceptance criteria verify — a deleted, skipped or loosened test is a \
                 finding.\n",
            );
            // Paths come out of the candidate's diff: worker-chosen text,
            // quoted like every other piece the runner did not write.
            prompt.push_str(&data_list_block(
                "changed verification inputs",
                request.verification_inputs_changed,
            ));
        }
        prompt.push_str(&format!("\ncandidate commit: {}\n", request.candidate_sha));
        // The patch travels IN the prompt. A reviewer's agent definition
        // gives it Read, Grep and Glob only — it reports, it does not act —
        // and a path it cannot open is a review that cannot happen: the first time this
        // prompt told a reviewer to read a file, it spent its whole wall
        // clock being refused and answered nothing at all.
        match patch {
            Ok(patch) => prompt.push_str(&data_block("candidate patch", &patch)),
            Err(e) => {
                // Say so in the prompt rather than pretending there is no
                // diff: a reviewer that cannot see the change must report
                // that, and the runner reads the missing verdict as
                // `Unavailable` rather than as approval.
                prompt.push_str(&format!(
                    "\nthe candidate patch could not be read ({e}); review what the \
                     objective and the criteria demand, and report that the diff was \
                     unavailable to you\n"
                ));
            }
        }
        prompt.push_str(&format!("source to inspect: {}\n", review_dir.display()));
        prompt.push_str(
            "\nFinish your answer with exactly one of these, and nothing after it: the line \
             `FINDINGS: none` when there are no findings, or the line `FINDINGS:` followed by \
             the list of findings.\n",
        );
        (prompt, reviewer_gap)
    }

    /// The review as a managed dispatch with its own seat, and the
    /// accounting that follows it: its cost is the run's (SPEC §9, §11).
    /// `Err` carries the `Unavailable` the caller returns — every way a
    /// reviewer fails to produce an answer the runner can record.
    fn dispatch_reviewer(
        &mut self,
        request: &ReviewRequest<'_>,
        seat: ReviewSeat<'_>,
        prompt: String,
        spend: &mut RunSpend,
    ) -> Result<LaunchResult, ReviewOutcome> {
        let ReviewSeat {
            profile,
            tier: reviewer_tier,
            dir: review_dir,
            purpose,
        } = seat;
        // A reviewer the runner cannot even name is a reviewer it
        // cannot dispatch: `Unavailable`, never a silent acceptance.
        let dispatch_id = match self.config.ids.dispatch_id() {
            Ok(id) => id,
            Err(e) => return Err(ReviewOutcome::Unavailable(e.to_string())),
        };
        let remaining_budget = self
            .config
            .machine
            .spending
            .per_run_micros
            .map(|ceiling| ceiling.remaining_after(spend.total).to_micros());
        let spec = LaunchSpec {
            dispatch_id: dispatch_id.as_str().to_string(),
            prompt,
            model: profile.id.clone(),
            effort: profile.effort.clone(),
            max_turns: None,
            budget_micros: remaining_budget,
            disallowed_tools: request.authority.disallowed_tools.clone(),
            work_dir: review_dir,
            // The review is part of acceptance, so it gets a floor of its
            // own rather than whatever the worker left of the run's wall
            // clock: a reviewer handed the one-second remainder is killed
            // before it can answer, and the run ends `needs_review` with
            // the whole attempt's spend already paid.
            wall_timeout: self.budgets.remaining().max(REVIEW_MIN_WALL),
            cancel: None,
            agent: AgentKind::Reviewer,
        };
        let recorded = self.config.ledger.record_dispatch_intent(
            &dispatch_id,
            &self.run_id,
            None,
            &serde_json::json!({
                "model": profile.id,
                "effort": profile.effort,
                "harness": self.harness,
                "tier": reviewer_tier.as_str(),
                "kind": purpose.intent_kind(),
                "recipe_id": request.covering.as_ref().map(|r| &r.recipe_id),
                "recipe_name": request.covering.as_ref().map(|r| &r.name),
                "recipe_revision": request.covering.as_ref().map(|r| r.revision),
            }),
            remaining_budget.unwrap_or(0),
            request.routed_by,
        );
        let recorded = recorded.and_then(|_| self.record_native_row(&dispatch_id, None));
        if let Err(e) = recorded {
            return Err(ReviewOutcome::Unavailable(format!(
                "the ledger refused the review intent: {e}"
            )));
        }
        // A review dispatch ends the run only through `Unavailable`, so
        // the budget it observes with is nominal.
        let budget = Budget {
            attempts_used: 0,
            max_attempts: request.authority.max_attempts,
            repairs_used: 0,
            max_repairs: 0,
            tier: reviewer_tier,
            rung: RungIndex::INITIAL,
            escalation_tier: None,
        };
        // Around the launch, monotonic: the dispatch's own elapsed time.
        let dispatch_start = Instant::now();
        let result = match self.managed_launch(ManagedDispatch {
            spec,
            attempt: None,
            depth: 0,
            parent: None,
            reserve_micros: remaining_budget.unwrap_or(0),
            budget: &budget,
            // The reviewer reads; it writes no worktree and takes no
            // lease.
            write_lease: None,
        }) {
            Ok(Ok(result)) => result,
            Ok(Err(outcome)) => {
                let closed = self
                    .config
                    .ledger
                    .finish_dispatch(&dispatch_id, "launch_failed");
                let detail = match closed {
                    Ok(()) => String::new(),
                    Err(e) => format!("; the ledger also refused the dispatch record: {e}"),
                };
                return Err(ReviewOutcome::Unavailable(format!(
                    "reviewer dispatch ended {}: {}{detail}",
                    outcome.state(),
                    outcome.detail()
                )));
            }
            Err(e) => {
                return Err(ReviewOutcome::Unavailable(format!(
                    "reviewer dispatch failed: {e}"
                )));
            }
        };
        if let Err(e) = self
            .config
            .ledger
            .finish_dispatch(&dispatch_id, "completed")
            .and_then(|()| self.record_native_row(&dispatch_id, result.session_id.as_deref()))
        {
            return Err(ReviewOutcome::Unavailable(format!(
                "the ledger refused the review record: {e}"
            )));
        }
        let event = UsageEvent {
            event_id: dispatch_id.as_str().to_string(),
            run_id: self.run_id.clone(),
            attempt_id: None,
            parent_event_id: None,
            model: result.effective_model.clone(),
            input_tokens: result.usage.input_tokens,
            output_tokens: result.usage.output_tokens,
            cache_read_tokens: result.usage.cache_read_tokens,
            cache_write_tokens: result.usage.cache_write_tokens,
            cost: result.usage.cost.micros(),
            cost_kind: self.usage_cost_kind(),
            completeness: result.usage.cost.completeness(),
            inclusive: result.usage.cost.inclusive(),
            at: self.config.ledger.now(),
            phase: Some(purpose.usage_phase()),
            duration_ms: Some(dispatch_start.elapsed().as_millis() as i64),
            requested_model: Some(profile.id.clone()),
            requested_effort: effort_str(profile.effort.as_ref()),
            harness: self.harness.clone(),
        };
        if let Err(e) = self.book_usage(&event, &result) {
            return Err(ReviewOutcome::Unavailable(format!(
                "the ledger refused the review usage: {e}"
            )));
        }
        spend.fold(event.cost, result.usage.cost.completeness());
        if result.terminal_result_missing() {
            return Err(ReviewOutcome::Unavailable(
                "the reviewer ended without a terminal result".into(),
            ));
        }
        Ok(result)
    }
}
