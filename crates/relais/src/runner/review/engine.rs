//! The engine side of review: dispatching reviewers and booking them.

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::backend::{LaunchResult, LaunchSpec};
use crate::context::candidates::parse_rulings;
use crate::context::GuidanceGap;
use crate::ledger::{EvidenceKind, Ledger, UsageEvent};
use crate::money::MicroUsd;
use crate::procs::Ended;
use crate::protocol::{AgentKind, Event};
use crate::route::RungIndex;
use crate::verify;
use crate::workspace;

use super::candidates::{
    append_rulings, candidates_block, candidates_unavailable_line, utc_seconds, CandidatesRun,
    PartCandidates, RulingContext,
};
use super::{
    merge_reviewer_gaps, patch_section, report_review_prompt, review_verdict, reviewer_decisions,
    reviewer_tier, stage_patch, ReviewOutcome, ReviewPurpose, ReviewRequest, ReviewSeat,
    StagedPatch, UnavailableCause, REVIEW_CONSTRAINTS_BUDGET_BYTES, REVIEW_MIN_WALL,
    REVIEW_PART_TIMEOUT,
};
use crate::runner::prompt::DECISION_PRECEDENCE;
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
            wall: None,
            on_dispatch: None,
        };
        let result = match self.dispatch_reviewer(request, seat, prompt, spend) {
            Ok((result, _)) => result,
            Err(failure) => return Err(failure.detail),
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
        // One file per dispatch, named for the candidate it judges, so a
        // second review never overwrites the first's evidence.
        let artifact = self
            .artifacts
            .join(format!("review-{}.txt", request.candidate_sha));
        let first = self.review_unit(request, spend, None, &artifact, None);
        let ReviewOutcome::Unavailable(detail) = first.outcome else {
            return first.outcome;
        };
        // The retry reads the cause, never the sentence: only a reviewer
        // that ended with no result is worth asking again. A refusal, a
        // cancellation or an exhausted resource would end the same way.
        if first.cause != Some(UnavailableCause::TerminalMissing) {
            return ReviewOutcome::Unavailable(detail);
        }
        let wall = match self.review_retry_wall(spend.total) {
            Ok(wall) => wall,
            Err(why) => {
                return ReviewOutcome::Unavailable(format!("{detail}; not retried: {why}"));
            }
        };
        self.events.emit(Event::EnvironmentWarning {
            what: "review_retry".to_string(),
            detail: format!(
                "the reviewer ended with cause {}; retrying once: {detail}",
                UnavailableCause::TerminalMissing.as_str()
            ),
        });
        match self
            .review_unit(request, spend, None, &artifact, Some(wall))
            .outcome
        {
            ReviewOutcome::Unavailable(second) => ReviewOutcome::Unavailable(format!(
                "{second}; the first dispatch had also failed: {detail}"
            )),
            decided => decided,
        }
    }

    /// The wall clock a second reviewer dispatch may have, or why it may
    /// not be had: a spend ceiling, or a work or total budget with nothing
    /// left. The floor of a first review is not renewed.
    fn review_retry_wall(&self, total_cost: MicroUsd) -> Result<std::time::Duration, String> {
        if let Some(blocked) = self.review_spend_blocked(total_cost) {
            return Err(blocked);
        }
        let work = self.budgets.work_remaining();
        if work.is_zero() {
            return Err("the work budget is exhausted".to_string());
        }
        let total = self.budgets.total_remaining();
        if total.is_zero() {
            return Err("the total budget is exhausted".to_string());
        }
        Ok(work.min(total))
    }

    /// One reviewer dispatch about the patch `request` names: the whole
    /// candidate (`part` is `None`) or one part of it. The answer is kept at
    /// `artifact`.
    pub(super) fn review_unit(
        &mut self,
        request: &ReviewRequest<'_>,
        spend: &mut RunSpend,
        part: Option<PartRun<'_>>,
        artifact: &Path,
        wall_limit: Option<std::time::Duration>,
    ) -> Reviewed {
        let on_dispatch = part.as_ref().map(|part| part.on_dispatch);
        let part = part.map(|part| part.label);
        // A reviewer gap belongs to the review that made it: a later
        // review that ends before it builds its prompt carries none.
        self.reviewer_gap = None;
        let Some((reviewer_tier, same_model)) = reviewer_tier(
            request.authority,
            request.candidate_tier,
            request.candidate_model,
        ) else {
            return Reviewed::unavailable(
                UnavailableCause::NoReviewerTier,
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
            return Reviewed::unavailable(UnavailableCause::ResourceExhausted, exhausted);
        }
        self.step("review", format!("{} tier", reviewer_tier.as_str()));
        let Some(profile) = request.authority.models.get(&reviewer_tier).cloned() else {
            return Reviewed::unavailable(
                UnavailableCause::NoReviewerTier,
                format!(
                    "no reviewer model configured at the {} tier",
                    reviewer_tier.as_str()
                ),
            );
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
                return Reviewed::unavailable(
                    UnavailableCause::NotRead,
                    format!("the ledger refused the reviewer-tier record: {e}"),
                );
            }
        }
        let review_dir = self.review_dir();
        // The reviewer reads the patch from a copy in its worktree's
        // ignored relais area; the copy goes when the review ends. A copy
        // that cannot be written leaves nothing to review: no reviewer is
        // dispatched for it.
        let staged = match stage_patch(&review_dir, &self.run_id, &request.patch_path) {
            Ok(staged) => staged,
            Err(e) => {
                return Reviewed::unavailable(
                    UnavailableCause::NotRead,
                    format!("the candidate patch could not be staged for the reviewer: {e}"),
                )
            }
        };
        let part_candidates = self.part_candidates(request, &staged.files);
        let (prompt, reviewer_gap) =
            self.review_prompt(request, &review_dir, &staged, part, &part_candidates);
        if let Some(gap) = &reviewer_gap {
            self.emit_guidance_gap(gap);
        }
        self.reviewer_gap = reviewer_gap;
        let seat = ReviewSeat {
            profile: &profile,
            tier: reviewer_tier,
            dir: review_dir,
            purpose: ReviewPurpose::Patch,
            wall: part.map(|_| REVIEW_PART_TIMEOUT).or(wall_limit),
            on_dispatch,
        };
        let dispatched = self.dispatch_reviewer(request, seat, prompt, spend);
        // The copy's removal is reported, never allowed to replace the
        // reviewer's answer or the launch failure that came first.
        if let Err(e) = std::fs::remove_dir_all(&staged.dir) {
            self.events.emit(Event::EnvironmentWarning {
                what: "review_patch_copy_not_removed".to_string(),
                detail: format!(
                    "the patch copy {} could not be removed: {e}",
                    staged.dir.display()
                ),
            });
        }
        let (result, dispatch_id) = match dispatched {
            Ok(dispatched) => dispatched,
            Err(failure) => return Reviewed::failed(failure),
        };
        let text = result.result_text.unwrap_or_default();
        // The artifact and its evidence row are one operation: a review
        // the run cannot record is a review it cannot show, and a run
        // that cannot show its review does not accept (X5).
        // Every answer, whole or a part's, lives at a path of its own
        // dispatch, so an earlier review's answer and its evidence row stay
        // as they were.
        let own = artifact.with_extension(format!("{dispatch_id}.txt"));
        let recorded = self.record_immutable_artifact(EvidenceKind::ReviewResult, &own, &text);
        let kept_at = part.is_some().then_some(own);
        if let Err(e) = recorded {
            return Reviewed {
                dispatch_id: Some(dispatch_id),
                ..Reviewed::unavailable(
                    UnavailableCause::NotRead,
                    format!("the review could not be recorded as evidence: {e}"),
                )
            };
        }
        if let Ok(listed) = &part_candidates {
            self.record_rulings(request, listed, &text, &dispatch_id);
        }
        let outcome = review_verdict(&text);
        let cause =
            matches!(outcome, ReviewOutcome::Unavailable(_)).then_some(UnavailableCause::NotRead);
        Reviewed {
            outcome,
            cause,
            dispatch_id: Some(dispatch_id),
            result_sha256: Some(crate::ids::sha256_hex(text.as_bytes())),
            result_path: kept_at,
        }
    }

    /// The candidates this dispatch lists: adr-lint is asked once per
    /// candidate commit, and each dispatch gets those on the files of the
    /// patch it reads. A gap is the answer when adr-lint gave none.
    fn part_candidates(
        &mut self,
        request: &ReviewRequest<'_>,
        files: &[String],
    ) -> Result<PartCandidates, GuidanceGap> {
        let sha = request.candidate_sha;
        let mut run = match self.candidates_run.take() {
            Some(run) if run.is_for(sha) => run,
            _ => CandidatesRun::new(
                sha,
                self.config
                    .aval_resolver
                    .candidates_for(request.base_sha, sha),
            ),
        };
        let part = run
            .fetched()
            .as_ref()
            .map(|list| PartCandidates::of(list, files))
            .map_err(Clone::clone);
        if let Ok(part) = &part {
            run.list(&part.items);
        }
        self.candidates_run = Some(run);
        part
    }

    /// Read the reviewer's rulings out of its answer, count them, and append
    /// one `adr-lint.ruling/1` line for each. A file that cannot be written
    /// is an environment warning, never a failed run.
    fn record_rulings(
        &mut self,
        request: &ReviewRequest<'_>,
        listed: &PartCandidates,
        answer: &str,
        dispatch_id: &str,
    ) {
        if listed.items.is_empty() {
            return;
        }
        let ids: Vec<&str> = listed.items.iter().map(|item| item.id.as_str()).collect();
        let rulings = parse_rulings(answer, &ids);
        if let Some(run) = self.candidates_run.as_mut() {
            run.rule(&rulings);
        }
        if rulings.is_empty() {
            return;
        }
        let written = std::fs::read(&request.patch_path).and_then(|patch| {
            let at = utc_seconds(&self.config.ledger.now())
                .ok_or_else(|| std::io::Error::other("the ledger clock is not RFC 3339"))?;
            let repo = self
                .config
                .repo_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            append_rulings(
                &self.artifacts,
                &RulingContext {
                    review: dispatch_id,
                    repo: &repo,
                    patch_bytes: &patch,
                    at: &at,
                },
                &listed.items,
                &rulings,
            )
        });
        if let Err(e) = written {
            self.events.emit(Event::EnvironmentWarning {
                what: "rulings_not_written".to_string(),
                detail: format!("the candidate rulings could not be written: {e}"),
            });
        }
    }

    /// What the reviewer is asked. Every piece of project text — the
    /// objective, the criteria, the constraints, the changed paths — is
    /// quoted as data, never as instructions (SPEC §16).
    fn review_prompt(
        &self,
        request: &ReviewRequest<'_>,
        review_dir: &Path,
        staged: &StagedPatch,
        part: Option<PartLabel>,
        candidates: &Result<PartCandidates, GuidanceGap>,
    ) -> (String, Option<GuidanceGap>) {
        let mut prompt = String::from(
            "You are a semantic reviewer. You cannot edit or waive checks; you report findings only.\n\
             Report each finding with file/range, the violated acceptance criterion or code rule id, evidence, and suggested verification.\n\
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
             a state is usually not the event that caused it.\n\
             A violation of a code rule that THIS diff introduces, deletions included, is a \
             finding citing the rule id and `file:line` (the old-side location for a deletion). \
             It blocks like an unmet acceptance criterion. Violations that were already there \
             and are unchanged are not reported.\n",
        );
        prompt.push_str(DECISION_PRECEDENCE);
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
        // The patch is not in the prompt, so the constraints have a
        // budget of their own.
        let (guidance_text, reviewer_gap) =
            reviewer_guidance(&request.manifest.guidance, REVIEW_CONSTRAINTS_BUDGET_BYTES);
        prompt.push_str(&guidance_text);
        let (decisions_text, decisions_gap) = reviewer_decisions(
            self.config.aval_resolver,
            &staged.files,
            REVIEW_CONSTRAINTS_BUDGET_BYTES,
            guidance_text.len(),
        );
        prompt.push_str(&decisions_text);
        match candidates {
            Ok(listed) => prompt.push_str(&candidates_block(listed)),
            Err(gap) => prompt.push_str(&candidates_unavailable_line(gap)),
        }
        let reviewer_gap = merge_reviewer_gaps(
            reviewer_gap,
            decisions_gap,
            candidates.as_ref().err().cloned(),
        );
        match self.config.ledger.intent_of_run(&self.run_id) {
            Ok(Some(intent)) => prompt.push_str(&super::widened_scope_block(&intent.scope_delta)),
            Ok(None) => {}
            // Not fatal, and said: the reviewer is told what it was not.
            Err(e) => prompt.push_str(&format!(
                "the paths this run's scope was widened to could not be read ({e}); check \
                 every out-of-scope edit in the patch closely.\n"
            )),
        }
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
        if let Some(part) = part {
            prompt.push_str(&format!(
                "this is {part}: the candidate patch was too large for one review and was cut \
                 at file boundaries; the patch below is that part only. Review the files in it, \
                 and report findings about them.\n"
            ));
        }
        prompt.push_str(&patch_section(staged));
        prompt.push_str(&format!("source to inspect: {}\n", review_dir.display()));
        prompt.push('\n');
        prompt.push_str(super::report_severity_instruction());
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
    ) -> Result<(LaunchResult, String), ReviewFailure> {
        let ReviewSeat {
            profile,
            tier: reviewer_tier,
            dir: review_dir,
            purpose,
            wall,
            on_dispatch,
        } = seat;
        // A reviewer the runner cannot even name is a reviewer it
        // cannot dispatch: `Unavailable`, never a silent acceptance.
        let dispatch_id = match self.config.ids.dispatch_id() {
            Ok(id) => id,
            Err(e) => return Err(ReviewFailure::new(UnavailableCause::NotRead, e.to_string())),
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
            wall_timeout: wall.unwrap_or_else(|| self.budgets.remaining().max(REVIEW_MIN_WALL)),
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
            return Err(ReviewFailure::new(
                UnavailableCause::NotRead,
                format!("the ledger refused the review intent: {e}"),
            ));
        }
        if let Some(on_dispatch) = on_dispatch {
            if let Err(e) = on_dispatch(dispatch_id.as_str()) {
                return Err(ReviewFailure::new(UnavailableCause::NotRead, e));
            }
        }
        // A review dispatch ends the run only through `Unavailable`, so
        // the budget it observes with is nominal.
        let budget = Budget {
            attempts_used: 0,
            max_attempts: request.authority.max_attempts,
            repairs_used: 0,
            max_repairs: 0,
            review_repairs_used: 0,
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
                return Err(ReviewFailure::new(
                    UnavailableCause::LaunchFailed,
                    format!(
                        "reviewer dispatch ended {}: {}{detail}",
                        outcome.state(),
                        outcome.detail()
                    ),
                ));
            }
            Err(e) => {
                return Err(ReviewFailure::new(
                    UnavailableCause::LaunchFailed,
                    format!("reviewer dispatch failed: {e}"),
                ));
            }
        };
        if let Err(e) = self
            .config
            .ledger
            .finish_dispatch(&dispatch_id, "completed")
            .and_then(|()| self.record_native_row(&dispatch_id, result.session_id.as_deref()))
        {
            return Err(ReviewFailure::new(
                UnavailableCause::NotRead,
                format!("the ledger refused the review record: {e}"),
            ));
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
            return Err(ReviewFailure::new(
                UnavailableCause::NotRead,
                format!("the ledger refused the review usage: {e}"),
            ));
        }
        spend.fold(event.cost, result.usage.cost.completeness());
        if result.terminal_result_missing() {
            return Err(unanswered(&result));
        }
        Ok((result, dispatch_id.as_str().to_string()))
    }
}

/// A reviewer dispatch that gave no answer to record: what happened, and
/// the cause the retry decision reads.
pub(in crate::runner) struct ReviewFailure {
    pub(in crate::runner) cause: UnavailableCause,
    pub(in crate::runner) detail: String,
}

impl ReviewFailure {
    fn new(cause: UnavailableCause, detail: String) -> Self {
        Self { cause, detail }
    }
}

/// The cause a reviewer's end names, from how the process ended and
/// whether the harness said why it could not run.
fn unanswered_cause(result: &LaunchResult) -> UnavailableCause {
    match result.ended {
        Ended::Cancelled => UnavailableCause::Cancelled,
        Ended::TimedOut => UnavailableCause::ResourceExhausted,
        Ended::Exited(_) | Ended::Signalled => {
            if result
                .failure_detail
                .as_deref()
                .is_some_and(|d| !d.is_empty())
            {
                UnavailableCause::LaunchFailed
            } else {
                UnavailableCause::TerminalMissing
            }
        }
    }
}

/// A reviewer that ended without a result, with its dispatch end named so
/// the next count can tell why.
fn unanswered(result: &LaunchResult) -> ReviewFailure {
    let cause = unanswered_cause(result);
    let sentence = match &result.failure_detail {
        // A launch that failed says why; the generic sentence is for an
        // end with nothing to report.
        Some(detail) if !detail.is_empty() => {
            format!("the reviewer could not be launched: {detail}")
        }
        _ => "the reviewer ended without a terminal result".to_string(),
    };
    ReviewFailure::new(cause, format!("{sentence} ({})", result.dispatch_end()))
}

/// Which part of a patch a reviewer is asked about: `part 2 of 5`, counted
/// from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) struct PartLabel {
    pub(in crate::runner) index: u32,
    pub(in crate::runner) of: u32,
}

impl std::fmt::Display for PartLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "part {} of {}", self.index, self.of)
    }
}

/// A part about to be reviewed: which one, and what to tell the parts
/// manifest when its dispatch exists.
pub(in crate::runner) struct PartRun<'a> {
    pub(in crate::runner) label: PartLabel,
    pub(in crate::runner) on_dispatch: &'a super::types::OnDispatch<'a>,
}

/// One reviewer dispatch's result: the verdict, the dispatch that gave it
/// and the hash of the answer kept as evidence.
pub(in crate::runner) struct Reviewed {
    pub(in crate::runner) outcome: ReviewOutcome,
    /// Why the outcome is `Unavailable`; `None` for any other outcome.
    pub(in crate::runner) cause: Option<UnavailableCause>,
    pub(in crate::runner) dispatch_id: Option<String>,
    pub(in crate::runner) result_sha256: Option<String>,
    /// Where a part's answer is kept: a path naming the attempt.
    pub(in crate::runner) result_path: Option<PathBuf>,
}

impl Reviewed {
    pub(in crate::runner) fn failed(failure: ReviewFailure) -> Self {
        Self {
            outcome: ReviewOutcome::Unavailable(failure.detail),
            cause: Some(failure.cause),
            dispatch_id: None,
            result_sha256: None,
            result_path: None,
        }
    }

    pub(in crate::runner) fn unavailable(cause: UnavailableCause, detail: String) -> Self {
        Self::failed(ReviewFailure::new(cause, detail))
    }
}
