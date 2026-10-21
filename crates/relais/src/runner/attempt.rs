//! The attempt loop: dispatch, snapshot, verify and judge each attempt.

use super::{
    build_prompt, conflict_block, effort_str, fresh_progress, spend_limit, split_profile_gaps,
    unpriced_native_model, verify, workspace, AgentKind, AttemptContext, AttemptKind, AttemptLabel,
    Attempted, BlockCode, Candidate, Ceiling, Dispatched, Duration, Ended, EvidenceKind, Instant,
    LaunchSpec, Limit, ManagedDispatch, Next, Observation, PathBuf, PendingEvidence, Phase,
    Progress, Reason, Review, RunEngine, RunError, RunOutcome, Snapshot, State, Step, Terminal,
    UsageEvent, Verified, VerifyFailure, WorkspaceError,
};

impl<'a> RunEngine<'a> {
    /// The bounded sequence of attempts the runner — not a model — owns
    /// (SPEC §9): ceilings first, then one dispatch, its candidate, and
    /// the machine's verdict on it.
    pub(super) fn attempt_loop(
        &mut self,
        ctx: &AttemptContext<'_>,
    ) -> Result<RunOutcome, RunError> {
        let mut progress = fresh_progress(ctx.preflight);

        loop {
            if let Some(outcome) = self.end_if_cancelled()? {
                return Ok(outcome);
            }
            if let Some(outcome) = self.ceiling_reached(&progress)? {
                return Ok(outcome);
            }
            let dispatched = match self.dispatch_attempt(ctx, &mut progress)? {
                Phase::Ended(outcome) => return Ok(outcome),
                Phase::Ready(dispatched) => dispatched,
            };
            let judged = match self.snapshot_attempt(ctx, &progress, dispatched)? {
                Snapshot::Ended(outcome) => return Ok(outcome),
                Snapshot::Ready(candidate) => {
                    self.judge_candidate(ctx, &mut progress, candidate)?
                }
                Snapshot::Unresolved { attempt_id, paths } => {
                    self.fail_unresolved(ctx, &mut progress, attempt_id, &paths)?
                }
            };
            match judged {
                Step::Ended(outcome) => return Ok(outcome),
                // The attempt is booked and judged; only a further dispatch
                // is replaced by the block.
                Step::Again => {
                    if let Some(model) = progress.unpriced_model.take() {
                        let machine_toml = crate::paths::machine_settings_path()
                            .unwrap_or_else(|_| PathBuf::from("machine.toml")); // detail only names the file
                        return self.block(
                            Reason::BlockedPreflight,
                            BlockCode::NativeUnpriced,
                            crate::native_pricing::backstop_detail(&model, &machine_toml),
                        );
                    }
                }
            }
        }
    }

    /// A `kind: verify` run after its tree is built: no worker, no repair
    /// and no escalation. The applied tree is snapshotted as the run's one
    /// attempt and goes through the pipeline a worker's candidate goes
    /// through: scope check, verification, review. A failed check ends the
    /// run `failed`, with the evidence.
    pub(super) fn verify_applied(
        &mut self,
        ctx: &AttemptContext<'_>,
    ) -> Result<RunOutcome, RunError> {
        if let Some(outcome) = self.end_if_cancelled()? {
            return Ok(outcome);
        }
        let mut progress = fresh_progress(ctx.preflight);
        progress.budget.attempts_used = 1;
        let tier = ctx.preflight.decision.tier;
        let attempt_id = self.config.ledger.insert_attempt(
            &self.run_id,
            ctx.preflight.revision_id,
            1,
            tier.as_str(),
            self.attempt_phase(AttemptKind::Initial),
        )?;
        let candidate = match self.snapshot_tree(
            ctx,
            &progress,
            Attempted {
                attempt_id,
                index: 1,
                tier,
                // No model wrote this candidate here: the reviewer, when
                // one is required, is independent of every worker.
                model: "relais-verify".to_string(),
            },
            None,
        )? {
            Snapshot::Ended(outcome) => return Ok(outcome),
            Snapshot::Ready(candidate) => candidate,
            // A verify is refused in preflight when the merge conflicts.
            Snapshot::Unresolved { .. } => return Err(RunError::Other(
                "a verify run has an unresolved conflict: it is refused before its tree is built"
                    .into(),
            )),
        };
        match self.judge_candidate(ctx, &mut progress, candidate)? {
            Step::Ended(outcome) => Ok(outcome),
            Step::Again => Err(RunError::Other(
                "a verify run was offered another attempt: it has no worker to repair with".into(),
            )),
        }
    }

    /// Budget ceilings are checked BEFORE admitting more work (SPEC §9,
    /// §11). `Some(outcome)` means one was reached and the run ended.
    fn ceiling_reached(&mut self, progress: &Progress) -> Result<Option<RunOutcome>, RunError> {
        if progress.budget.attempts_used >= progress.budget.max_attempts {
            return Ok(Some(self.stop(
                &progress.budget,
                Observation::LimitReached(Limit::Attempts {
                    max: progress.budget.max_attempts,
                    last_failures: progress.last_failures.clone().unwrap_or_default(),
                }),
            )?));
        }
        if let Some(exhausted) = self.budgets.exhausted() {
            return Ok(Some(self.end_on_exhausted(exhausted, &progress.budget)?));
        }
        if let Some(ceiling) = self.config.machine.spending.per_run_micros {
            if progress.spend.total >= ceiling {
                return Ok(Some(self.stop(
                    &progress.budget,
                    Observation::LimitReached(spend_limit(
                        progress.spend.total,
                        ceiling,
                        Ceiling::PerRun,
                    )),
                )?));
            }
        }
        // The machine's day, not this run's (SPEC §11: stop admitting
        // work once the dollar control is exhausted). Every settled
        // attempt of this run is already a usage row, so the day's
        // recorded spend includes it — adding the run's own total
        // again would count it twice.
        if let Some(ceiling) = self.config.machine.spending.per_day_micros {
            let today = self.spent_today(self.config.ledger)?;
            if today >= ceiling {
                return Ok(Some(self.stop(
                    &progress.budget,
                    Observation::LimitReached(spend_limit(today, ceiling, Ceiling::PerDay)),
                )?));
            }
        }
        Ok(None)
    }

    /// One attempt: its ledger row, its prompt, its managed dispatch and
    /// the accounting that follows — usage, cost and the model the
    /// harness actually ran (SPEC §6, §11, §12).
    fn dispatch_attempt(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
    ) -> Result<Phase<Dispatched>, RunError> {
        let ledger = self.config.ledger;
        let authority = &ctx.preflight.authority;
        progress.budget.attempts_used += 1;
        let index = progress.budget.attempts_used;
        let rung_index = progress.budget.rung;
        let rung = ctx
            .preflight
            .decision
            .ladder
            .rung(rung_index)
            .ok_or_else(|| {
                RunError::Other(format!(
                    "internal error: the ladder has no rung {} for attempt {index}",
                    rung_index.get()
                ))
            })?;
        let tier = rung.tier;
        let kind = progress.kind;
        let attempt_id = ledger.insert_attempt(
            &self.run_id,
            ctx.preflight.revision_id,
            index as i64,
            tier.as_str(),
            self.attempt_phase(kind),
        )?;

        // The ladder's rung at the index the machine named, exactly as
        // `route()` resolved and validated it: tier, model and effort.
        let model_profile = rung.profile();
        let mut prompt = build_prompt(
            self.config.contract,
            &ctx.preflight.manifest,
            authority.verification_profile.commands.len(),
            progress.last_failures.as_deref(),
            kind,
        );
        // A continued candidate that conflicted: what is still open is the
        // worker's first job (plan A4). A path it already resolved is not
        // listed again.
        let conflicted = ctx.preflight.conflicted_paths();
        let open = ctx
            .worktree
            .unresolved_conflicts(conflicted)
            .unwrap_or_else(|_| conflicted.to_vec());
        prompt.push_str(&conflict_block(&open));

        // Dispatch intent is persisted BEFORE the process exists
        // (SPEC §12), keyed so retries cannot duplicate agents.
        let dispatch_id = self.config.ids.dispatch_id()?;
        // The whole resolved ladder rides on the run's first worker
        // dispatch, so the intent alone says what every later rung was.
        let ladder = (index == 1).then_some(&ctx.preflight.decision.ladder);
        ledger.record_dispatch_intent(
            &dispatch_id,
            &self.run_id,
            Some(attempt_id),
            &serde_json::json!({
                "model": model_profile.id,
                "effort": model_profile.effort,
                "harness": self.harness,
                "tier": tier.as_str(),
                "rung": rung_index,
                "ladder": ladder,
                "kind": kind.as_str(),
                "prompt_bytes": prompt.len(),
                "recipe_id": ctx.preflight.decision.covering.as_ref().map(|r| &r.recipe_id),
                "recipe_name": ctx.preflight.decision.covering.as_ref().map(|r| &r.name),
                "recipe_revision": ctx.preflight.decision.covering.as_ref().map(|r| r.revision),
                "task_type_draw": ctx.preflight.decision.type_draw,
            }),
            0,
            ctx.preflight.decision.routed_by,
        )?;
        // The row says what the coordinator is told (`NativeRun`) from the
        // moment it exists, so the two never disagree during the wait, and
        // a launch that fails before any agent binds keeps the right kind.
        self.record_native_row(&dispatch_id, None)?;
        ledger.record_features(
            &dispatch_id,
            &serde_json::json!({
                "attempt_index": index,
                "tier": tier.as_str(),
                "model": model_profile.id,
                "kind": kind.as_str(),
            }),
        )?;

        // Every worker dispatch is a transition into `running`, whatever
        // kind of attempt it is (SPEC §9): the chain used to skip
        // straight from `prepared` to `verifying`, so a run ten minutes
        // into its first attempt still read as not yet started.
        self.transition(
            State::Running,
            Reason::WorkerDispatched,
            serde_json::json!({
                "dispatch_id": dispatch_id.as_str(),
                "attempt_index": index,
                "tier": tier.as_str(),
                "kind": kind.as_str(),
            }),
        )?;
        // Re-taken after `transition(&mut self)`: the shared reference the
        // function opened with cannot outlive that mutable call.
        let ledger = self.config.ledger;

        let remaining_wall = self.budgets.remaining().max(Duration::from_secs(1));
        // What this attempt may still spend, not the whole ceiling
        // again: the budget is the run's, not the attempt's.
        // Saturating: a wrapped remainder would read as a budget
        // nothing bounds (P7).
        let remaining_budget = self
            .config
            .machine
            .spending
            .per_run_micros
            .map(|ceiling| ceiling.remaining_after(progress.spend.total).to_micros());
        let spec = LaunchSpec {
            dispatch_id: dispatch_id.as_str().to_string(),
            prompt,
            model: model_profile.id.clone(),
            effort: model_profile.effort.clone(),
            max_turns: None,
            budget_micros: remaining_budget,
            disallowed_tools: authority.disallowed_tools.clone(),
            work_dir: ctx.worktree_path.to_path_buf(),
            wall_timeout: remaining_wall,
            cancel: None,
            agent: AgentKind::Worker,
        };

        let requested_model = model_profile.id.clone();
        let requested_effort = effort_str(model_profile.effort.as_ref());
        // Around the launch, monotonic: the dispatch's own elapsed time,
        // never derived from the `at` timestamps `record_usage` stamps.
        let dispatch_start = Instant::now();
        let result = match self.managed_launch(ManagedDispatch {
            spec,
            attempt: Some(index),
            depth: 0,
            parent: None,
            reserve_micros: remaining_budget.unwrap_or(0),
            budget: &progress.budget,
            write_lease: Some(ctx.worktree_path),
        })? {
            Ok(result) => result,
            Err(outcome) => {
                ledger.finish_dispatch(&dispatch_id, "launch_failed")?;
                ledger.finish_attempt(attempt_id, outcome.state(), None, None)?;
                return Ok(Phase::Ended(outcome));
            }
        };
        let duration_ms = dispatch_start.elapsed().as_millis() as i64;
        ledger.finish_dispatch(&dispatch_id, "completed")?;
        ledger.record_native_dispatch(&dispatch_id, result.session_id.as_deref())?;
        // What the backend says it did differently from what was asked is
        // evidence of the attempt.
        if !result.stderr.is_empty() {
            let note_path = self
                .artifacts
                .join(format!("attempt-{index}-native-note.txt"));
            self.record_artifact(
                Some(attempt_id),
                EvidenceKind::NativeNote,
                &note_path,
                &result.stderr,
            )?;
        }

        // Usage is recorded even when the attempt went nowhere: all
        // recorded cost, failed runs included (SPEC §11).
        let usage = &result.usage;
        let event = UsageEvent {
            event_id: dispatch_id.as_str().to_string(),
            run_id: self.run_id.clone(),
            attempt_id: Some(attempt_id),
            parent_event_id: None,
            model: result.effective_model.clone(),
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cost: usage.cost.micros(),
            // An attempt's cost is always tokens times the price table,
            // known or not: an unpriced one is still an estimate that could
            // not be made, never API spend somebody reported.
            cost_kind: self.usage_cost_kind(),
            completeness: usage.cost.completeness(),
            inclusive: usage.cost.inclusive(),
            at: self.config.ledger.now(),
            phase: Some(self.attempt_phase(kind)),
            duration_ms: Some(duration_ms),
            requested_model: Some(requested_model.clone()),
            requested_effort,
            harness: self.harness.clone(),
        };
        self.book_usage(&event, &result)?;
        progress.spend.fold(event.cost, usage.cost.completeness());
        progress.unpriced_model = unpriced_native_model(&result);

        if let Some(model) = &result.effective_model {
            if !progress.models_used.contains(model) {
                progress.models_used.push(model.clone());
            }
        }

        // An unapproved substitution stops further dispatch and
        // invalidates any claim that the requested route was tested
        // (SPEC §6). A harness that named NO model leaves the
        // question open, which is a recorded gap and stops dispatch
        // just the same: the run cannot say the route was tested
        // (audit V3). A substitution `machine.toml` approved in advance
        // is neither: dispatch carries on exactly as a match would — the
        // usage event just recorded above already carries both the
        // requested model and the effective one that ran, so the money
        // is attributed to the model that actually ran and the route's
        // own request is not lost.
        match crate::backend::verify_model(
            &requested_model,
            result.effective_model.as_deref(),
            &self.config.machine.routing.approved_substitutions,
        ) {
            crate::backend::ModelVerification::Matches => {}
            // Recorded, not waved through. The usage event above already
            // carries both identities, so the money is attributed
            // correctly either way — but that pair is two columns a
            // reader would have to compare deliberately, and nothing in
            // the run's own story would say a substitution happened.
            // This is a same-state transition, the idiom this runner
            // already uses for a fact that changes no state.
            crate::backend::ModelVerification::Approved {
                requested,
                effective,
            } => {
                let state = self.state;
                self.transition(
                    state,
                    Reason::ModelSubstitutionApproved,
                    serde_json::json!({
                        "requested": requested,
                        "effective": effective,
                        "dispatch": dispatch_id.as_str(),
                    }),
                )?;
            }
            crate::backend::ModelVerification::Substituted {
                requested,
                effective,
            } => {
                return Ok(Phase::Ended(self.stop(
                    &progress.budget,
                    Observation::UnapprovedSubstitution {
                        requested,
                        effective,
                    },
                )?));
            }
            // A dispatch that produced no terminal result — killed by
            // the wall clock, cancelled, crashed — could not have
            // reported a model either. The interruption below is the
            // outcome; calling it an unverified model would hide it.
            crate::backend::ModelVerification::Unverified { .. }
                if result.terminal_result_missing() => {}
            crate::backend::ModelVerification::Unverified { requested } => {
                return Ok(Phase::Ended(self.block(
                    Reason::UnapprovedSubstitution,
                    BlockCode::ModelUnverified,
                    format!(
                        "the harness reported no effective model, so nothing establishes that \
                         `{requested}` ran; dispatch stops rather than assume it"
                    ),
                )?));
            }
        }
        Ok(Phase::Ready(Dispatched {
            attempt_id,
            index,
            tier,
            model: model_profile.id.clone(),
            result,
        }))
    }

    /// What the worker left behind, turned into a candidate: the
    /// immutable snapshot, its name, its patch and its scope (SPEC §8).
    /// The ways an attempt ends without one — cancelled, interrupted,
    /// blocked, a tree still being written, a scope violation — end the
    /// run here.
    fn snapshot_attempt(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &Progress,
        dispatched: Dispatched,
    ) -> Result<Snapshot, RunError> {
        let ledger = self.config.ledger;
        let Dispatched {
            attempt_id,
            index,
            tier,
            model,
            result,
        } = dispatched;
        let worktree_path = ctx.worktree_path;
        let held_worktree = worktree_path.to_string_lossy().into_owned();

        // Cancelled through the coordinator: the worktree and
        // evidence stay; nothing else is dispatched (SPEC §23).
        if result.ended == Ended::Cancelled {
            ledger.finish_attempt(attempt_id, State::Cancelled, Some(&held_worktree), None)?;
            return Ok(Snapshot::Ended(self.stop(
                &progress.budget,
                Observation::Cancelled(
                    "the dispatch was cancelled through the coordinator; what the worker wrote \
                     is kept as a named candidate"
                        .into(),
                ),
            )?));
        }

        // Missing terminal result = interrupted, not failed (SPEC §9).
        if result.terminal_result_missing() {
            ledger.finish_attempt(attempt_id, State::Interrupted, Some(&held_worktree), None)?;
            return Ok(Snapshot::Ended(self.stop(
                &progress.budget,
                Observation::TerminalResultMissing {
                    timed_out: result.ended == Ended::TimedOut,
                    detail: result.failure_detail.clone().unwrap_or_default(),
                },
            )?));
        }

        // The worker's answer is evidence — for an inspect task it is
        // the whole deliverable (SPEC §4) — recorded before anything
        // is judged about it.
        if let Some(text) = result.result_text.as_deref() {
            let result_path = self.artifacts.join(format!("attempt-{index}-result.txt"));
            self.record_artifact(
                Some(attempt_id),
                EvidenceKind::WorkerResult,
                &result_path,
                text,
            )?;
        }

        // A worker blockage proposal is recorded as evidence and the
        // runner assigns blocked — the environment is never escalated
        // to a stronger model (SPEC §9).
        let claimed_blockage = result.worker_claims_blockage;
        if claimed_blockage {
            ledger.finish_attempt(attempt_id, State::Blocked, None, None)?;
            return Ok(Snapshot::Ended(self.stop(
                &progress.budget,
                Observation::WorkerBlockage(result.result_text.unwrap_or_default()),
            )?));
        }

        // Nothing is snapshotted from a tree still being written: the
        // worker's own lease went back when its process ended, so
        // any holder here is a straggler another tab or run owns
        // (SPEC §23).
        if let Some(holder) = self.wait_for_writers(worktree_path)? {
            ledger.finish_attempt(attempt_id, State::Interrupted, Some(&held_worktree), None)?;
            return Ok(Snapshot::Ended(
                self.stop_on_held_lease(worktree_path, &holder)?,
            ));
        }

        self.snapshot_tree(
            ctx,
            progress,
            Attempted {
                attempt_id,
                index,
                tier,
                model,
            },
            result.result_text,
        )
    }

    /// Snapshot the worktree as attempt `index`'s candidate, name it,
    /// export its patch and judge its scope. Shared by a worker's attempt
    /// and a verify run's applied tree: both go through the same
    /// pipeline.
    fn snapshot_tree(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &Progress,
        attempted: Attempted,
        result_text: Option<String>,
    ) -> Result<Snapshot, RunError> {
        let ledger = self.config.ledger;
        let Attempted {
            attempt_id,
            index,
            tier,
            model,
        } = attempted;
        let worktree_path = ctx.worktree_path;
        let held_worktree = worktree_path.to_string_lossy().into_owned();
        // The candidate snapshot is recorded outside model control,
        // added files included (SPEC §8).
        let sha = match ctx.worktree.snapshot_resolved(
            &format!("run {} attempt {index}", self.run_id),
            ctx.preflight.conflicted_paths(),
        ) {
            Ok(sha) => sha,
            Err(WorkspaceError::UnresolvedConflict(paths)) => {
                return Ok(Snapshot::Unresolved { attempt_id, paths });
            }
            Err(e) => {
                ledger.finish_attempt(attempt_id, State::Interrupted, None, None)?;
                return Ok(Snapshot::Ended(
                    self.fail_preflight(BlockCode::SnapshotFailed, e.to_string())?,
                ));
            }
        };
        self.last_snapshot = Some((index, sha.clone()));
        // `commit-tree` leaves a dangling object: the candidate the
        // receipt names, the patch's other side and the worktree's
        // history would all go in the next `git gc`. A ref under
        // `refs/relais/candidates/` keeps it reachable without
        // putting a branch in the user's namespace (audit B15).
        // Best effort: a ref that could not be written is not fatal —
        // the object exists and this run verifies it from the object —
        // and the retirement at the run's end names the tree `…/final`
        // itself when no ref holds it, so nothing is lost either way.
        // A full disk is the one failure of it that is not shrugged off:
        // relais's own write failing stops the run (SPEC §10).
        let named = crate::storage_full::at(crate::storage_full::Site::Ref)
            .map_err(WorkspaceError::Io)
            .and_then(|()| {
                workspace::name_candidate(self.config.repo_dir, self.run_id.as_str(), index, &sha)
            });
        if let Err(e) = named {
            if e.is_storage_full() {
                return Err(e.into());
            }
        }
        ledger.finish_attempt(
            attempt_id,
            State::Verifying,
            Some(&held_worktree),
            Some(&sha),
        )?;
        crate::storage_full::at(crate::storage_full::Site::Snapshot)?;
        std::fs::write(self.artifacts.join(format!("candidate-{index}.sha")), &sha)?;
        let patch_path = self.artifacts.join(format!("candidate-{index}.patch"));
        ctx.worktree.export_patch(&sha, &patch_path)?;
        // The reviewer's prompt names `candidate-latest.patch`, so the
        // copy is part of producing the candidate, not a convenience:
        // a failure here stops the run instead of handing a reviewer a
        // path that is not there (R4).
        let latest_patch = self.artifacts.join("candidate-latest.patch");
        std::fs::copy(&patch_path, &latest_patch)?;
        ledger.record_evidence(
            &self.run_id,
            Some(attempt_id),
            EvidenceKind::CandidatePatch,
            &patch_path,
            Some(&workspace::sha256_file(&patch_path)?),
        )?;

        // Write scope is checked on the actual diff; a violation can
        // never be accepted (SPEC §8, §9).
        match workspace::check_scope(ctx.worktree, &sha, self.config.contract) {
            Ok(_) => {}
            Err(WorkspaceError::ScopeViolation(paths)) => {
                ledger.finish_attempt(attempt_id, State::NeedsDecision, None, Some(&sha))?;
                return Ok(Snapshot::Ended(self.stop(
                    &progress.budget,
                    Observation::ScopeViolation(workspace::out_of_scope(
                        self.config.repo_dir,
                        &ctx.preflight.base_sha,
                        &sha,
                        paths,
                    )),
                )?));
            }
            Err(e) => {
                return Ok(Snapshot::Ended(
                    self.fail_preflight(BlockCode::ScopeCheckFailed, e.to_string())?,
                ));
            }
        }

        Ok(Snapshot::Ready(Candidate {
            attempt_id,
            index,
            tier,
            model,
            sha,
            latest_patch,
            result_text,
        }))
    }

    /// The attempt left a conflicted path unresolved: it produced no
    /// candidate, so it fails like a red check does (SPEC §9): a repair,
    /// then an escalation, then the attempt ceiling.
    fn fail_unresolved(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        attempt_id: i64,
        paths: &[String],
    ) -> Result<Step, RunError> {
        self.config
            .ledger
            .finish_attempt(attempt_id, State::Failed, None, None)?;
        let failures = vec![format!("unresolved_conflict: {}", paths.join(", "))];
        let same_failures = progress.last_failures.as_deref() == Some(failures.as_slice());
        progress.last_failures = Some(failures.clone());
        self.follow(
            ctx,
            progress,
            Observation::VerificationFailed {
                failures,
                unchanged_candidate: false,
                same_failures,
                all_preexisting: false,
            },
        )
    }

    /// Judge one candidate (SPEC §9, §10): what it changes about
    /// verification, whether the worker could act at all, the profile's
    /// checks, and what the machine says about the failures.
    fn judge_candidate(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        candidate: Candidate,
    ) -> Result<Step, RunError> {
        let ledger = self.config.ledger;
        let authority = &ctx.preflight.authority;
        let worktree_path = ctx.worktree_path;
        let held_worktree = worktree_path.to_string_lossy().into_owned();

        // A candidate that changes what verification IS gets neither
        // a silent pass nor one uniform answer (SPEC §9, §10).
        // Editing the profile's own inputs — build manifests,
        // lockfiles, the commands' programs — is "changes protected
        // verification": §9's table sends that to the user as
        // `needs_decision`, because no reviewer can decide on the
        // user's behalf that a loosened build is what was wanted.
        // Editing the TEST TREE is work §10 explicitly invites, so
        // it stays what it was: explicit review, whatever the route
        // said (audit B8).
        let touched_inputs = verify::classify_verification_inputs(
            &authority.verification_profile,
            &ctx.worktree.changed_paths_in(&candidate.sha)?,
        )?;
        if !touched_inputs.policy.is_empty() {
            let detail = format!(
                "the candidate changes what verification is: {}; these are the profile's own \
                 inputs, so passing its checks would not mean what the policy says it means. \
                 It is not a reviewer's to decide — the candidate and its patch are \
                 preserved",
                touched_inputs.policy.join(", ")
            );
            ledger.finish_attempt(
                candidate.attempt_id,
                State::NeedsDecision,
                Some(&held_worktree),
                Some(&candidate.sha),
            )?;
            return Ok(Step::Ended(self.finish(
                Reason::VerificationInputsChanged,
                serde_json::json!({
                    "paths": touched_inputs.policy,
                    "candidate": candidate.sha,
                }),
                Terminal::NeedsDecision {
                    reason: Reason::VerificationInputsChanged,
                    detail,
                },
            )?));
        }
        let verification_inputs_changed = touched_inputs.tests;
        let review_required = ctx.preflight.decision.review >= Review::Required
            || !verification_inputs_changed.is_empty();

        // A candidate that carries the base tree has the baseline's
        // results by identity: the commands are not spent again, and
        // the receipt says so.
        let identical = match ctx.worktree.same_tree_as_base(&candidate.sha) {
            Ok(same) => same,
            Err(e) => {
                return Ok(Step::Ended(
                    self.fail_preflight(BlockCode::SnapshotFailed, e.to_string())?,
                ));
            }
        };
        // An unchanged candidate is measured against the attempt before it
        // once there is one: from attempt 2 the previous attempt's work is
        // already in the tree, so comparing to the BASE says every repair
        // worker produced something (R2).
        //
        // An inspection's tree is always unchanged: what it produces is
        // its final message, and only an empty one is nothing.
        let inspecting = self.config.contract.kind() == crate::contract::Kind::Inspect;
        let unchanged = match progress.last_candidate.as_deref() {
            Some(previous) => previous == candidate.sha,
            None => identical,
        };
        let empty_report = inspecting
            && candidate
                .result_text
                .as_deref()
                .is_none_or(|text| text.trim().is_empty());
        // Entered only once the candidate is to be verified.
        if !verification_inputs_changed.is_empty()
            && ctx.preflight.decision.review < Review::Required
        {
            self.transition(
                State::Verifying,
                Reason::VerificationInputsChanged,
                serde_json::json!({ "paths": verification_inputs_changed }),
            )?;
        }

        // Verification against an immutable copy of the candidate
        // (SPEC §10). Entering `verifying` is a transition like every
        // other: assigned straight to the field, the ledger never
        // showed the state it then recorded as the next row's origin
        // (R7).
        self.transition(
            State::Verifying,
            Reason::VerificationStarted,
            serde_json::json!({ "candidate": candidate.sha, "attempt": candidate.index }),
        )?;
        let reuse = if identical {
            self.transition(
                State::Verifying,
                Reason::CandidateIdenticalToBase,
                serde_json::json!({ "candidate": candidate.sha }),
            )?;
            ctx.baseline.checks.as_deref()
        } else {
            None
        };
        let verify::Verified {
            checks,
            mut gaps,
            acceptance_gaps,
            amont_bypasses,
            amont_downgrades,
            gate_coverage,
        } = match self.verify_candidate(
            &candidate.sha,
            authority,
            &ctx.baseline.logs_dir,
            AttemptLabel::Attempt(candidate.index),
            reuse,
        ) {
            Ok(result) => result,
            Err(VerifyFailure::Lease(stop)) => {
                return Ok(Step::Ended(self.end_on_lease_stop(stop, &progress.budget)?));
            }
            Err(VerifyFailure::Unavailable(e)) => {
                return Ok(Step::Ended(
                    self.fail_preflight(BlockCode::VerificationUnavailable, e)?,
                ));
            }
            Err(VerifyFailure::Full(e)) => return Err(e),
        };
        if let Some(outcome) = self.end_if_cancelled()? {
            return Ok(Step::Ended(outcome));
        }
        // A check that timed out because a budget ran out under it says
        // nothing about the code: the run ends on the budget (ADR-0010),
        // with the candidate kept, rather than repairing a clock.
        if checks
            .iter()
            .any(|check| matches!(check.ended.recorded(), Some(Ended::TimedOut)))
        {
            if let Some(exhausted) = self.budgets.exhausted() {
                return Ok(Step::Ended(
                    self.end_on_exhausted(exhausted, &progress.budget)?,
                ));
            }
        }
        // A failed check that shows a full disk or no memory is evidence
        // for a person: no repair, no escalation, the candidate kept.
        if let Some(exhausted) = self.exhaustion_in(&checks) {
            return Ok(Step::Ended(
                self.end_on_candidate_exhaustion(ctx, &candidate, exhausted)?,
            ));
        }
        // An inspection judges its report: a gap about the profile's own
        // checks is recorded, not a stop. The acceptance-evidence gaps
        // keep their handling below.
        let mut gaps_not_judged = Vec::new();
        if inspecting {
            (gaps, gaps_not_judged) = split_profile_gaps(gaps, &acceptance_gaps);
        }
        let sign_off_only = !acceptance_gaps.is_empty()
            && acceptance_gaps.len() == gaps.len()
            && acceptance_gaps
                .iter()
                .all(|gap| gap.missing == verify::MissingEvidence::SignOffUnrecorded);
        // An inspection with only sign-offs open is judged first: its
        // report may still be unmet, which a person's answer cannot fix.
        let judged_first = inspecting && sign_off_only;
        if !gaps.is_empty() && !judged_first {
            // Every other gap here is either unfixable from `relais
            // decide` (a missing/undefined check) or already a second
            // problem alongside one; a human-sign-off gap is the one
            // kind a person's OWN later answer can clear (SPEC §10). When
            // that is the only kind present, the checks, tests and review
            // this attempt already ran are worth keeping: store the
            // receipt they would have produced now, `needs_decision`,
            // rather than losing that work and making the eventual
            // approval re-derive it from nothing.
            if sign_off_only {
                self.store_pending_receipt(
                    ctx,
                    progress,
                    &candidate,
                    PendingEvidence {
                        checks: checks.clone(),
                        gaps: gaps.clone(),
                        gaps_not_judged: gaps_not_judged.clone(),
                        notes: Vec::new(),
                        gate_coverage: gate_coverage.clone(),
                        verdict: None,
                    },
                )?;
            }
            return Ok(Step::Ended(
                self.stop(&progress.budget, Observation::VerificationGap(gaps))?,
            ));
        }
        // An inspection changes nothing, so a check already failing at
        // the base is not its to repair (SPEC §10).
        let mut failures: Vec<String> = if inspecting {
            verify::new_failure_labels(&checks, &ctx.baseline.failures)
        } else {
            checks
                .iter()
                .filter(|check| check.failed())
                .map(|check| check.label.clone())
                .collect()
        };
        // An inspection's deliverable is its report: an empty one is a
        // failure the worker may repair once, as an empty candidate is
        // for a change.
        if empty_report {
            failures.push("empty_report".into());
        }
        // A change task whose candidate changes nothing has not met
        // its objective, whatever the baseline says: a behavioural
        // failure the worker can repair, never an acceptance.
        if identical
            && self.config.contract.kind() == crate::contract::Kind::Change
            && !self.config.contract.is_verify()
        {
            failures.push("empty_candidate".into());
        }

        if !failures.is_empty() && self.config.contract.is_verify() {
            // No repair and no escalation: the candidate was verified and
            // it failed.
            let detail = format!(
                "the verified candidate fails: {}; no worker was dispatched and nothing was \
                 repaired",
                failures.join(", ")
            );
            return Ok(Step::Ended(self.finish(
                Reason::BehavioralFailure,
                serde_json::json!({ "failures": failures, "candidate": candidate.sha }),
                Terminal::Failed { detail },
            )?));
        }
        if !failures.is_empty() {
            let same_failures = progress.last_failures.as_deref() == Some(failures.as_slice());
            progress.last_candidate = Some(candidate.sha.clone());
            let all_preexisting = failures
                .iter()
                .all(|label| ctx.baseline.failures.contains(label));
            progress.last_failures = Some(failures.clone());
            // Plan C1: complete JUnit on both sides says every failing
            // test already fails at the base and nothing targets it. A
            // cached base keeps no failing checks, so it never applies.
            if let Some(tests) = ctx.baseline.checks.as_deref().and_then(|base| {
                verify::guarded_preexisting(
                    &failures,
                    base,
                    &checks,
                    &ctx.preflight.authority.verification_profile,
                    &self.config.contract.acceptance,
                )
            }) {
                return self.follow(
                    ctx,
                    progress,
                    Observation::PreexistingTestFailures { failures, tests },
                );
            }
            return self.follow(
                ctx,
                progress,
                Observation::VerificationFailed {
                    failures,
                    unchanged_candidate: unchanged,
                    same_failures,
                    all_preexisting,
                },
            );
        }

        let verified = Verified {
            checks,
            gaps,
            gaps_not_judged,
            notes: Vec::new(),
            amont_bypasses,
            amont_downgrades,
            verification_inputs_changed,
            review_required,
            gate_coverage,
        };
        if inspecting {
            return self.judge_inspection(ctx, progress, &candidate, verified);
        }
        self.accept_candidate(ctx, progress, &candidate, verified, None)
            .map(Step::Ended)
    }

    /// Record the machine's decision on an observation and follow it:
    /// another attempt, or the end of the run.
    pub(super) fn follow(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        observation: Observation,
    ) -> Result<Step, RunError> {
        match self.decide(&progress.budget, observation)? {
            Next::Attempt { kind, rung } => {
                if kind == AttemptKind::Repair {
                    progress.budget.repairs_used += 1;
                }
                progress.kind = kind;
                progress.budget.rung = rung;
                // The ladder is read again at dispatch; the tier the
                // budget carries is only the rung's, kept for the
                // limits that name it.
                // A rung the ladder lacks is never substituted: the
                // attempt ceiling ends the run before dispatch, and
                // dispatch refuses an index out of the ladder.
                if let Some(at) = ctx.preflight.decision.ladder.rung(rung) {
                    progress.budget.tier = at.tier;
                }
                Ok(Step::Again)
            }
            Next::Accept => Err(RunError::Other(
                "the machine accepted a failing candidate".into(),
            )),
            Next::Stop(terminal) => Ok(Step::Ended(RunOutcome {
                run_id: self.run_id.clone(),
                terminal,
            })),
        }
    }
}
