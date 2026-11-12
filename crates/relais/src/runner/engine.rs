//! The run engine: its state, constructor, core and run driver.

use super::setup_gate;
use super::workspace_ops::SetupWithin;
use super::{
    admission_block, baseline_summary, decide, exhaustion, fresh_progress, kept_sentence,
    scheduler, setup_failure_detail, state_sibling, sweep_ended_verify_dirs, verify, verify_root,
    workspace, worktree_root, Arc, AtomicBool, AttemptContext, AttemptKind, BaseMemo, BlockCode,
    Budget, BudgetClock, BudgetLimits, Budgets, Candidate, CostKind, Duration, EffectiveAuthority,
    Event, Events, EvidenceKind, Exhausted, Finish, Gate, Instant, KeptCandidate, Limit, Next,
    Observation, Ordering, PackageId, PackageRole, PathBuf, Phase, Reason, Refusal, Request,
    RunConfig, RunEngine, RunError, RunId, RunOutcome, State, Summary, Terminal, Transition,
    UsagePhase, ADMISSION_POLL,
};

impl<'a> RunEngine<'a> {
    /// The phase an attempt of THIS run records: a root run and an
    /// ordinary package say which attempt it is; every attempt of an
    /// integration repair is integration, because that is what the
    /// money bought.
    pub(super) fn attempt_phase(&self, kind: AttemptKind) -> UsagePhase {
        self.parent
            .as_ref()
            .map_or(UsagePhase::from(kind), |(_, _, role)| role.phase(kind))
    }

    /// The candidate a `kind: verify` run is verifying, named by the run
    /// that wrote it; `None` for every other run, whose baseline has no
    /// candidate under verification.
    pub(super) fn verify_source(&self, preflight: &super::Preflight) -> Option<KeptCandidate> {
        if !self.config.contract.is_verify() {
            return None;
        }
        preflight.candidate.as_ref().map(|prepared| KeptCandidate {
            run: prepared.resolved.source_run.clone(),
            index: prepared.resolved.index,
            sha: prepared.resolved.candidate_commit.clone(),
        })
    }

    /// The candidate an admission wait that runs out around attempt
    /// `index`'s verification or review keeps: this run's own attempt, or
    /// for a `kind: verify` run the source it resolved.
    pub(super) fn kept_candidate(
        &self,
        preflight: &super::Preflight,
        index: u32,
        sha: &str,
    ) -> KeptCandidate {
        self.verify_source(preflight)
            .unwrap_or_else(|| KeptCandidate {
                run: self.run_id.as_str().to_string(),
                index,
                sha: sha.to_string(),
            })
    }

    pub(super) fn new(
        config: &'a RunConfig<'a>,
        parent: Option<(RunId, PackageId, PackageRole)>,
    ) -> Result<Self, RunError> {
        let run_id = match &config.run_id {
            Some(run_id) if parent.is_none() => run_id.clone(),
            Some(_) | None => config.ids.run_id()?,
        };
        // How often this run's runner has started: a resumed run counts on
        // from there, so its heavy leases never reuse a settled id.
        let epoch = config.ledger.next_run_epoch(run_id.as_str())?;
        let artifacts = config.artifacts_dir.join(run_id.as_str());
        let worktrees = worktree_root(&config.artifacts_dir).join(run_id.as_str());
        let verify_dir = verify_root(&config.artifacts_dir).join(run_id.as_str());
        let events = Events::for_run_on(run_id.as_str(), &artifacts, config.wire.clone());
        if parent.is_none() {
            // The process's own stderr belongs to the root run.
            events.mirror_stderr();
        }
        Ok(Self {
            config,
            run_id,
            artifacts,
            worktrees,
            verify_dir,
            state: State::Prepared,
            parent,
            harness: None,
            own_write_leases: std::collections::BTreeSet::new(),
            inplace: None,
            last_snapshot: None,
            timed_out_settled: None,
            base_memo: BaseMemo::default(),
            reviewer_gap: None,
            candidates_run: None,
            worker_code_rules_gap: None,
            cancel_asked: Arc::new(AtomicBool::new(false)),
            budgets: Budgets::unbounded(),
            heavy_tries: verify::Tries::for_epoch(epoch),
            growth: crate::ledger::LedgerGrowth::of(config.ledger),
            registration: None,
            events,
        })
    }

    /// Record a transition. The ledger is the record; a transition it
    /// cannot write is a runner failure, not something to carry on past.
    pub(crate) fn transition(
        &mut self,
        to: State,
        reason: Reason,
        detail: serde_json::Value,
    ) -> Result<(), RunError> {
        let from = self.state;
        let wrote = self.config.ledger.transition_if(
            from,
            &Transition {
                run_id: self.run_id.clone(),
                attempt_id: None,
                from_state: Some(from),
                to_state: to,
                reason: reason.as_str().to_string(),
                detail: Some(detail.clone()),
                at: self.config.ledger.now(),
            },
        )?;
        if !wrote {
            return Err(RunError::Superseded(self.run_id.clone()));
        }
        self.state = to;
        self.events.emit(Event::Phase {
            state: to.as_str().to_string(),
            reason: reason.as_str().to_string(),
            detail,
        });
        Ok(())
    }

    /// Run `work` while a watcher asks the coordinator, every
    /// `heartbeat_every`, whether this run is cancelled: a cancel reaches
    /// a dispatch on its heartbeat, but a verification check has none. The
    /// flag handed to `work` turns true on a cancel and stays true.
    pub(super) fn while_watching_cancel<T>(
        &self,
        work: impl FnOnce(Option<&AtomicBool>) -> T,
    ) -> T {
        watch_cancel(
            &CancelWatch {
                gate: self.config.gate,
                every: self.config.heartbeat_every,
                run_id: self.run_id.as_str(),
                asked: &self.cancel_asked,
            },
            work,
        )
    }

    /// Whether the coordinator has cancelled this run: the watcher saw it,
    /// or the gate says so now.
    pub(super) fn cancel_requested(&self) -> bool {
        self.cancel_asked.load(Ordering::SeqCst)
            || self
                .config
                .gate
                .is_some_and(|gate| gate.run_cancelled(self.run_id.as_str()).unwrap_or(false))
    }

    /// Between phases: a run cancelled at the coordinator ends cancelled
    /// here, through the same transition as any other end.
    pub(super) fn end_if_cancelled(&mut self) -> Result<Option<RunOutcome>, RunError> {
        if !self.cancel_requested() {
            return Ok(None);
        }
        self.cancel_asked.store(true, Ordering::SeqCst);
        let detail =
            "the run was cancelled through the coordinator; its evidence is kept".to_string();
        Ok(Some(self.finish(
            Reason::CancelledByUser,
            serde_json::json!({ "detail": detail }),
            Terminal::Cancelled { detail },
        )?))
    }

    /// Record a terminal transition and end the run with it.
    pub(crate) fn finish(
        &mut self,
        reason: Reason,
        detail: serde_json::Value,
        terminal: Terminal,
    ) -> Result<RunOutcome, RunError> {
        self.transition(terminal.state(), reason, detail)?;
        Ok(RunOutcome {
            run_id: self.run_id.clone(),
            terminal,
        })
    }

    /// Ask the machine, record what it decided, hand back the step.
    pub(crate) fn decide(
        &mut self,
        budget: &Budget,
        observation: Observation,
    ) -> Result<Next, RunError> {
        let decision = decide(budget, observation);
        self.record_decision(decision)
    }

    /// Record a decision the machine made and hand back its step.
    fn record_decision(&mut self, decision: super::machine::Decision) -> Result<Next, RunError> {
        let what = match &decision.next {
            Next::Attempt { kind, .. } => kind.as_str(),
            Next::Accept => "accept",
            Next::Stop(_) => "stop",
        };
        self.events.emit(Event::Decision {
            what: what.to_string(),
            reason: decision.reason.as_str().to_string(),
        });
        self.transition(decision.state, decision.reason, decision.detail)?;
        Ok(decision.next)
    }

    pub(crate) fn fail_preflight(
        &mut self,
        code: BlockCode,
        detail: String,
    ) -> Result<RunOutcome, RunError> {
        self.block(Reason::BlockedPreflight, code, detail)
    }

    pub(crate) fn block(
        &mut self,
        reason: Reason,
        code: BlockCode,
        detail: String,
    ) -> Result<RunOutcome, RunError> {
        self.finish(
            reason,
            serde_json::json!({ "code": code, "detail": detail }),
            Terminal::Blocked { code, detail },
        )
    }

    /// What a dispatch's usage is: its cost is always tokens times the
    /// price table, known or not, never API spend somebody reported.
    pub(crate) fn usage_cost_kind(&self) -> CostKind {
        CostKind::EstimatedApiEquivalent
    }

    /// Observe something terminal, record the machine's decision and end
    /// the run. For observations the table can only ever stop on.
    pub(crate) fn stop(
        &mut self,
        budget: &Budget,
        observation: Observation,
    ) -> Result<RunOutcome, RunError> {
        let next = self.decide(budget, observation)?;
        self.outcome_of_stop(next)
    }

    /// The run's outcome for a recorded `Next` that must be a stop.
    fn outcome_of_stop(&self, next: Next) -> Result<RunOutcome, RunError> {
        match next {
            Next::Stop(terminal) => Ok(RunOutcome {
                run_id: self.run_id.clone(),
                terminal,
            }),
            other => Err(RunError::Other(format!(
                "the machine answered {other:?} to a terminal observation"
            ))),
        }
    }

    /// A step of the run the pane shows (`protocol::Event::Step`).
    pub(crate) fn step(&self, name: &str, detail: String) {
        self.events.emit(Event::Step {
            name: name.to_string(),
            detail,
            max_attempts: None,
        });
    }

    pub(super) fn run(mut self) -> Result<RunOutcome, RunError> {
        if self.parent.is_none() {
            self.sweep_ended_holdouts();
            sweep_ended_verify_dirs(&verify_root(&self.config.artifacts_dir), self.config.ledger);
        }
        let finish = self.run_to_outcome();
        self.count_holdout_exposures();
        // A package (child) run has its own run id and so its own holdout
        // directory; it removes it here like a root run does.
        self.remove_holdout_dir();
        // The base checked on demand kept its `verify-base` tree for the
        // run: it goes now, whatever the ending (ADR-0013).
        self.release_base_tree();
        // Each verification worktree removed itself; the run's own
        // `verify/<run>/` directory is left empty, and goes too. One that
        // still holds a file (a release that failed) stays visible.
        if let Err(e) = crate::prune::remove_empty_tree(&self.verify_dir) {
            eprintln!(
                "relais: the verify directory {} could not be removed: {e}",
                self.verify_dir.display()
            );
        }
        let outcome = match finish {
            Finish::Own(outcome) => outcome,
            Finish::Exit(error) => return Err(error),
            // Another process ended the run (a reap, a workerless cancel)
            // and wrote its one outcome event. This driver writes none: a
            // second would contradict it. The plugin reading this stream
            // is still told the run is over, with the state the ledger
            // holds now.
            Finish::Superseded(outcome) => {
                if self.config.purpose != Some(crate::lifecycle::RunPurpose::Replay)
                    && self.parent.is_none()
                {
                    let (code, detail) = outcome.terminal.named_reason();
                    let _ = self.config.wire.send(&Request::Done {
                        run: Some(self.run_id.as_str()),
                        outcome: outcome.state().as_str(),
                        receipt: None,
                        summary: None,
                        trial: None,
                        code: code.as_deref(),
                        detail,
                        successor_intent: None,
                    });
                }
                return Ok(outcome);
            }
        };
        // Whatever `persist_receipt` wrote, whichever way the run ended: an
        // accepted run's receipt, and the pending one a run stopped only by
        // an unmet sign-off stores (`store_pending_receipt`). The artifacts
        // directory is this run's own, so the file is this run's receipt.
        let written = self.artifacts.join("receipt.json");
        let receipt = written
            .is_file()
            .then(|| written.to_string_lossy().into_owned());
        let summary = Summary::of_candidate(&self.artifacts);
        let widened = self
            .settle_scope_overflow(&outcome)
            .or_else(|| self.timed_out_settled.take());
        // The state the ledger now holds: a widened run was answered
        // (`revise`) and cancelled, whatever its terminal said.
        let resolved = if widened.is_some() {
            State::Cancelled
        } else {
            outcome.state()
        };
        self.events.emit(Event::Outcome {
            state: resolved.as_str().to_string(),
            receipt: receipt.clone(),
            summary,
            reason: widened.as_ref().map(|settled| settled.sentence.clone()),
            successor_intent: widened.as_ref().map(|settled| settled.successor.clone()),
        });
        // A replay's `done` waits for its trial to be recorded: its caller
        // sends it (`relais dataset replay`).
        let announces = match self.config.purpose {
            Some(crate::lifecycle::RunPurpose::Replay) => false,
            Some(crate::lifecycle::RunPurpose::TrialArm) | None => self.parent.is_none(),
        };
        if announces {
            let (code, detail) = match &widened {
                Some(settled) => (
                    Some(settled.code.to_string()),
                    Some(settled.sentence.as_str()),
                ),
                None => outcome.terminal.named_reason(),
            };
            let done = Request::Done {
                run: Some(self.run_id.as_str()),
                outcome: resolved.as_str(),
                receipt: receipt.as_deref(),
                summary,
                trial: None,
                code: code.as_deref(),
                detail,
                successor_intent: widened.as_ref().map(|settled| &settled.successor),
            };
            // Dropped on failure: the plugin that was reading is gone, the
            // run's outcome is in the ledger and in `events.jsonl`.
            let _ = self.config.wire.send(&done);
        }
        // The caller (CLI exit status, stderr, trial settlement) reads the
        // resolved state, not the terminal the decision was opened from.
        match widened {
            Some(settled) => Ok(RunOutcome {
                run_id: outcome.run_id,
                terminal: Terminal::Cancelled {
                    detail: settled.sentence,
                },
            }),
            None => Ok(outcome),
        }
    }

    /// A scope overflow that is only tests, or tests plus one small edit
    /// ([`super::successor::widenable`]), is answered here, by relais:
    /// `revise` with the scope widened to those paths, as a successor
    /// intent (SPEC §8). Returns what the outcome carries; `None` when the
    /// run did not stop that way, policy says no, the run is itself the
    /// successor of a relais settle, or the planning or the write failed,
    /// in which case the decision stays open for a person, as it always did.
    // holds-until: no overlay filter here; the holdout overlay collision
    // (runner/attempt.rs, runner/scheduler.rs) ends a run before any scope can
    // be widened. Ends if phase 5 of the holdout plan counts a
    // holdout_exposures that bypassed the collision (ADR-0045).
    fn settle_scope_overflow(&self, outcome: &RunOutcome) -> Option<super::successor::Settled> {
        let config = self.config;
        let overflowed = matches!(
            outcome.terminal,
            Terminal::NeedsDecision {
                reason: Reason::ScopeExceeded,
                ..
            }
        );
        let ordinary =
            self.parent.is_none() && config.purpose != Some(crate::lifecycle::RunPurpose::Replay);
        let execution = &config.repo_policy.execution;
        if !overflowed || !ordinary || !execution.widen_scope_for_tests {
            return None;
        }
        match super::successor::is_relais_successor(config.ledger, &self.run_id) {
            Ok(false) => {}
            Ok(true) => return None,
            Err(e) => {
                self.step("widen_scope", format!("not widened: {e}"));
                return None;
            }
        }
        let profile = config
            .repo_policy
            .verification
            .profiles
            .get(&config.contract.verification_profile)?;
        let planned = super::successor::plan_scope_widening(
            config.ledger,
            &self.run_id,
            config.contract,
            profile,
            super::successor::SmallScopeLimits {
                max_files: execution.widen_scope_small_max_files,
                max_lines: execution.widen_scope_small_max_lines,
            },
            &config.artifacts_dir,
        );
        let (intent, paths, rule) = match planned {
            Ok(Some(planned)) => planned,
            Ok(None) => return None,
            Err(e) => {
                self.step("widen_scope", format!("not widened: {e}"));
                return None;
            }
        };
        let note = rule.note();
        let written = config.ledger.resolve_decision_with_intent(
            &self.run_id,
            &crate::ledger::DecisionAnswer {
                resolution: Reason::DecisionRevised,
                actor: crate::ledger::Actor::Relais,
                person_quote: None,
                recorded_via: crate::ledger::RecordedVia::Cli,
                note: Some(&note),
                successor_run: None,
                from_state: State::NeedsDecision,
                to_state: State::Cancelled,
            },
            &intent,
        );
        let stored = match written {
            Ok(Some(stored)) => stored,
            Ok(None) => return None,
            Err(e) => {
                self.step("widen_scope", format!("not widened: {e}"));
                return None;
            }
        };
        let what = match &rule {
            super::successor::WidenRule::Tests => {
                "the diff left the scope only by test paths".to_string()
            }
            super::successor::WidenRule::Small(edits) => format!(
                "the diff left the scope by one small edit: {}",
                edits
                    .iter()
                    .map(|edit| match (edit.added, edit.removed) {
                        (Some(added), Some(removed)) => {
                            format!("{} (+{added}/-{removed})", edit.path)
                        }
                        _ => edit.path.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let sentence = format!(
            "{what}; relais widened the scope to the changed paths and recorded successor \
             intent {} to verify and review the candidate again: {}",
            stored.intent_id,
            paths.join(", ")
        );
        Some(super::successor::Settled {
            code: rule.code(),
            sentence,
            successor: crate::protocol::SuccessorRef {
                intent_id: stored.intent_id,
                repo: config.repo_dir.to_string_lossy().into_owned(),
                widened_paths: paths,
                code: rule.code().to_string(),
                small_edit: match &rule {
                    super::successor::WidenRule::Tests => None,
                    super::successor::WidenRule::Small(edits) => {
                        edits.first().map(|edit| crate::protocol::SmallEdit {
                            path: edit.path.clone(),
                            added: edit.added,
                            removed: edit.removed,
                        })
                    }
                },
            },
        })
    }

    fn run_to_outcome(&mut self) -> Finish {
        match self.run_inner() {
            Ok(outcome) => Finish::Own(outcome),
            Err(RunError::Superseded(_)) => Finish::Superseded(self.superseded_outcome()),
            Err(error) if error.is_storage_full() => self.on_storage_full(&error),
            Err(error) => {
                // The runner itself failed. Say so on the record if the
                // record will still take it; the outcome says so either
                // way, and nothing is retried (SPEC §12).
                let detail = format!(
                    "the runner could not continue: {error}; the run is interrupted and \
                     whatever was written is preserved"
                );
                match self.transition(
                    State::Interrupted,
                    Reason::RunnerFailure,
                    serde_json::json!({ "error": error.to_string() }),
                ) {
                    Err(RunError::Superseded(_)) => Finish::Superseded(self.superseded_outcome()),
                    // Not recorded (the ledger refused it): the outcome
                    // still says the run is interrupted, and why.
                    Ok(()) | Err(_) => Finish::Own(RunOutcome {
                        run_id: self.run_id.clone(),
                        terminal: Terminal::Interrupted { detail },
                    }),
                }
            }
        }
    }

    /// relais's own write met a full disk (SPEC §10). The error has
    /// already unwound out of whatever was running, so no new launch
    /// starts and the running check's process group is gone. What is left
    /// is to persist, in order, the little that can still be saved: the
    /// ballast is deleted first and must have raised the available space,
    /// else nothing is written (a local snapshot can keep its blocks) and
    /// the run exits on its own code; then the candidate's ref, the
    /// `blocked (relais_storage_full)` transition and the follow-up note.
    /// A step that fails ends the same way, and `relais resume` reconciles
    /// from the last durable ledger state.
    fn on_storage_full(&mut self, error: &RunError) -> Finish {
        let probe = crate::storage_full::probe();
        let volume = self.tightest_volume(probe.as_ref());
        let exit = || {
            Finish::Exit(RunError::StorageFull {
                volume: volume.clone(),
            })
        };
        let ballast = state_sibling(
            &self.config.artifacts_dir,
            crate::storage_full::BALLAST_FILE,
        );
        if crate::storage_full::free_ballast(&ballast, probe.as_ref())
            != crate::storage_full::Freed::Rose
        {
            return exit();
        }
        // 1. The candidate's ref, when a snapshot exists. When the ref
        // cannot be written the outcome says so; the tree stays for resume.
        let mut notes = Vec::new();
        if let Some((index, sha)) = self.last_snapshot.clone() {
            if let Err(e) =
                workspace::name_candidate(self.config.repo_dir, self.run_id.as_str(), index, &sha)
            {
                notes.push(format!(
                    "the candidate {sha} could not be given its ref ({e}); its worktree is kept \
                     for `relais resume`"
                ));
            }
        }
        // 2. The ledger transition.
        let detail = format!(
            "relais's own storage on {volume} is full ({error}); new launches stopped and \
             running checks were killed. {}Free space on {volume}, then `relais resume {}` \
             reconciles the run",
            notes.iter().map(|n| format!("{n}. ")).collect::<String>(),
            self.run_id
        );
        let recorded = self.transition(
            State::Blocked,
            Reason::RelaisStorageFull,
            serde_json::json!({
                "volume": volume,
                "error": error.to_string(),
                "candidate": self.last_snapshot.as_ref().map(|(_, sha)| sha),
            }),
        );
        match recorded {
            Ok(()) => {}
            Err(RunError::Superseded(_)) => return Finish::Superseded(self.superseded_outcome()),
            Err(_) => return exit(),
        }
        // 3. The follow-up intent.
        let note = serde_json::json!({
            "intent": "resume",
            "run": self.run_id.as_str(),
            "command": format!("relais resume {}", self.run_id),
            "volume": volume,
        });
        let follow_up = self.artifacts.join("follow-up.json");
        if crate::fsutil::write_atomic(&follow_up, &note.to_string()).is_err() {
            return exit();
        }
        Finish::Own(RunOutcome {
            run_id: self.run_id.clone(),
            terminal: Terminal::BlockedBy {
                reason: Reason::RelaisStorageFull,
                detail,
            },
        })
    }

    /// The volume with the least space among those this run writes: the
    /// likeliest one to be full.
    fn tightest_volume(&self, probe: &dyn crate::disk::DiskProbe) -> String {
        crate::disk::volumes(probe, &self.disk_paths())
            .into_iter()
            .min_by_key(|volume| volume.available)
            .map_or_else(|| "unknown volume".to_string(), |volume| volume.mount)
    }

    /// The outcome of a run another process ended: what the ledger holds
    /// now, `cancelled` or else `interrupted`.
    fn superseded_outcome(&self) -> RunOutcome {
        let detail = format!(
            "run {} was ended by another process (reaped or cancelled)",
            self.run_id.as_str()
        );
        let terminal = match self.config.ledger.run_status(&self.run_id) {
            Ok(Some(State::Cancelled)) => Terminal::Cancelled { detail },
            Ok(_) | Err(_) => Terminal::Interrupted { detail },
        };
        RunOutcome {
            run_id: self.run_id.clone(),
            terminal,
        }
    }

    /// The run, by phase: preflight, baseline, then either the
    /// decomposed path or one owned worktree and a bounded attempt loop
    /// (SPEC §3, §9). Each phase either hands the next one what it
    /// established, or ends the run.
    fn run_inner(&mut self) -> Result<RunOutcome, RunError> {
        // The run's clock starts here, on the ledger's own: preflight
        // counts against `total_seconds`.
        let clock: Arc<dyn BudgetClock> = Arc::new(LedgerBudgetClock(self.config.ledger.clock()));
        let run_started = clock.now_millis();
        self.step("preflight", String::new());
        let preflight = match self.preflight()? {
            Phase::Ended(outcome) => return Ok(outcome),
            Phase::Ready(preflight) => preflight,
        };
        self.budgets = Budgets::started_at(
            clock,
            budget_limits(
                &preflight.authority,
                &self.config.contract.limits,
                &self.config.machine.resources,
            ),
            run_started,
        );
        self.step("baseline", String::new());
        let baseline = match self.baseline(&preflight)? {
            Phase::Ended(outcome) => return Ok(outcome),
            Phase::Ready(baseline) => baseline,
        };
        self.step("baseline", baseline_summary(&baseline));

        // The baseline is over: what follows draws on the work budget.
        self.budgets.begin_work();
        if let Some(exhausted) = self.budgets.exhausted() {
            let nominal = fresh_progress(&preflight).budget;
            let kept = self.verify_source(&preflight);
            return self.end_on_exhausted(exhausted, &nominal, kept.as_ref());
        }
        // The harness identity every dispatch of this run records, probed
        // once (SPEC §16: model/effort/harness identity are features).
        self.harness = self.config.backend.probe().map(|capabilities| {
            format!(
                "{} {}",
                self.config.backend.name(),
                capabilities.version.as_deref().unwrap_or("?")
            )
        });

        // Bounded decomposition (SPEC §19): work packages as runs of their
        // own, an assembled candidate verified independently. A planner
        // may answer "single worker", in which case the ordinary path
        // continues below.
        if let Some(decomposition) = &self.config.contract.decomposition {
            let flags = baseline.flags();
            let root = scheduler::RootContext {
                authority: &preflight.authority,
                base_sha: &preflight.base_sha,
                contract_hash: &preflight.contract_hash,
                manifest: &preflight.manifest,
                decision: &preflight.decision,
                baseline_failures: baseline.failures(),
                integration_gaps: &preflight.integration_gaps,
                baseline_cached: flags.cached,
                baseline_cache_refused: &baseline.cache_refused,
                baseline_proven: flags.proven,
                baseline_deferred: flags.deferred,
                logs_dir: &baseline.logs_dir,
                holdouts: &preflight.holdouts,
            };
            match scheduler::run_decomposed(self, &root, decomposition)? {
                scheduler::Decomposed::Outcome(outcome) => return Ok(outcome),
                scheduler::Decomposed::SingleWorker => {}
            }
        }

        // One owned worktree for the whole run: repairs continue from a
        // candidate whose scope and integrity passed; a scope violation
        // stops everything (SPEC §8, §9).
        let worktree_path = self.worktrees.join("task");
        self.step("worktree · setup", String::new());
        let worktree = match self.take_in_place_worktree() {
            Some(Ok(worktree)) => worktree,
            Some(Err(e)) => return self.fail_preflight(BlockCode::WorktreeUnavailable, e),
            None => match workspace::create_worktree(
                self.config.repo_dir,
                &preflight.base_sha,
                &worktree_path,
            ) {
                Ok(worktree) => worktree,
                Err(e) => {
                    return self.fail_preflight(BlockCode::WorktreeUnavailable, e.to_string())
                }
            },
        };
        let worktree_path = worktree.path.clone();
        // A verify's tree is the merge result computed in preflight, not a
        // second merge: what conflict detection judged is what is checked.
        if let Some(prepared) = &preflight.candidate {
            let ended = match self.recheck_candidate(prepared)? {
                Some(outcome) => Some(outcome),
                None => match prepared.check_out(&worktree) {
                    Ok(()) => None,
                    Err(e) => Some(self.fail_preflight(
                        BlockCode::WorktreeUnavailable,
                        format!("the candidate's tree could not be checked out: {e}"),
                    )?),
                },
            };
            if let Some(outcome) = ended {
                self.retire_worktree(&worktree, &outcome)?;
                return Ok(outcome);
            }
        }
        // The same setup in the worker's own tree, so the worker can run
        // the checks it is told to pass. This is also the one place the
        // setup is proven when the baseline came from the cache: a
        // cached verdict skipped the base's copy, not the machine.
        let setup = self.while_watching_cancel(|cancel| {
            self.setup_tree(
                &worktree_path,
                Some(setup_gate::SeedSource {
                    path: self.config.repo_dir,
                    wait: setup_gate::SeedWait::QuietFor(setup_gate::Quiet::CALLER),
                }),
                &preflight.authority.verification_profile,
                &baseline.logs_dir,
                "task",
                SetupWithin {
                    cancel,
                    budgets: &self.budgets,
                },
            )
        });
        let setup = match setup {
            Ok(setup) => setup,
            Err(verify::VerifyError::Lease(stop)) => {
                let nominal = fresh_progress(&preflight).budget;
                let kept = self.verify_source(&preflight);
                let outcome = self.end_on_lease_stop(stop, &nominal, kept.as_ref())?;
                self.retire_worktree(&worktree, &outcome)?;
                return Ok(outcome);
            }
            Err(e) => return Err(e.into()),
        };
        self.record_logs(None, EvidenceKind::SetupLog, &setup)?;
        if let Some(outcome) = self.end_if_cancelled()? {
            self.retire_worktree(&worktree, &outcome)?;
            return Ok(outcome);
        }
        let outcome = match verify::setup_failure(&setup) {
            // The worktree exists and no worker has touched it; whatever
            // the setup left there — an installed `node_modules` the
            // tree ignores, or a generated file it does not — is
            // retired like any run's tree: kept if in no patch, then gone.
            Some(failed) => self.fail_preflight(
                BlockCode::VerificationSetupFailed,
                setup_failure_detail(failed, "the task worktree"),
            )?,
            None => {
                let ctx = AttemptContext {
                    preflight: &preflight,
                    baseline: &baseline,
                    worktree: &worktree,
                    worktree_path: &worktree_path,
                };
                if self.config.contract.is_verify() {
                    self.verify_applied(&ctx)?
                } else {
                    self.attempt_loop(&ctx)?
                }
            }
        };
        if let Some(settled) = self.settle_timed_out(&outcome) {
            // The decision is answered and the run is cancelled: the tree
            // goes like a cancelled run's, now the successor's intent holds
            // its candidate.
            self.state = State::Cancelled;
            let cancelled = RunOutcome {
                run_id: outcome.run_id.clone(),
                terminal: Terminal::Cancelled {
                    detail: settled.sentence.clone(),
                },
            };
            self.timed_out_settled = Some(settled);
            self.retire_worktree(&worktree, &cancelled)?;
            return Ok(outcome);
        }
        self.retire_worktree(&worktree, &outcome)?;
        Ok(outcome)
    }

    /// A change run that ended interrupted on its worker's wall time, with
    /// the stop confirmed and an edit published as its candidate, is
    /// answered here, by relais: `revise` with a `kind: verify` successor
    /// of that candidate, scope unchanged (SPEC §8). `None` when the run
    /// ended another way, policy says no, the run is a verify, a child, a
    /// replay or the successor of a relais settle, or planning or the write
    /// failed, in which case the decision stays open for a person.
    fn settle_timed_out(&self, outcome: &RunOutcome) -> Option<super::successor::Settled> {
        let config = self.config;
        let ordinary = self.parent.is_none()
            && config.purpose != Some(crate::lifecycle::RunPurpose::Replay)
            && config.contract.kind() == crate::contract::Kind::Change
            && !config.contract.is_verify();
        if !matches!(outcome.terminal, Terminal::Interrupted { .. })
            || !ordinary
            || !config.repo_policy.execution.verify_timed_out
        {
            return None;
        }
        match super::successor::is_relais_successor(config.ledger, &self.run_id) {
            Ok(false) => {}
            Ok(true) => return None,
            Err(e) => {
                self.step("verify_timed_out", format!("not settled: {e}"));
                return None;
            }
        }
        let intent = match super::successor::plan_timed_out_verify(
            config.ledger,
            &self.run_id,
            config.contract,
            &config.artifacts_dir,
        ) {
            Ok(Some(intent)) => intent,
            Ok(None) => return None,
            Err(e) => {
                self.step("verify_timed_out", format!("not settled: {e}"));
                return None;
            }
        };
        let written = config.ledger.resolve_decision_with_intent(
            &self.run_id,
            &crate::ledger::DecisionAnswer {
                resolution: Reason::DecisionRevised,
                actor: crate::ledger::Actor::Relais,
                person_quote: None,
                recorded_via: crate::ledger::RecordedVia::Cli,
                note: Some(super::successor::TIMED_OUT_NOTE),
                successor_run: None,
                from_state: State::Interrupted,
                to_state: State::Cancelled,
            },
            &intent,
        );
        let stored = match written {
            Ok(Some(stored)) => stored,
            Ok(None) => return None,
            Err(e) => {
                self.step("verify_timed_out", format!("not settled: {e}"));
                return None;
            }
        };
        let sentence = format!(
            "the worker hit its wall time; relais recorded successor intent {} to verify and \
             review its tree",
            stored.intent_id
        );
        Some(super::successor::Settled {
            code: super::successor::CODE_TIMED_OUT_VERIFY_QUEUED,
            sentence,
            successor: crate::protocol::SuccessorRef {
                intent_id: stored.intent_id,
                repo: config.repo_dir.to_string_lossy().into_owned(),
                widened_paths: Vec::new(),
                code: super::successor::CODE_TIMED_OUT_VERIFY_QUEUED.to_string(),
                small_edit: None,
            },
        })
    }

    /// A failed check among `checks` that shows exhaustion, with what the
    /// machine had at failure time (SPEC §10). Read from the failed check's
    /// output only.
    pub(super) fn exhaustion_in(
        &self,
        checks: &[verify::CheckOutcome],
    ) -> Option<(String, exhaustion::Exhaustion, exhaustion::Measured)> {
        let (label, found) = exhaustion::first_exhaustion(checks)?;
        let probe = crate::storage_full::probe();
        let measured = exhaustion::measure(
            found.resource,
            probe.as_ref(),
            &self.disk_paths(),
            self.config.machine.resources.min_free_bytes(),
        );
        Some((label, found, measured))
    }

    /// The candidate's failed check showed exhaustion: `needs_decision
    /// (resource_exhausted)` with the candidate preserved and no repair
    /// (SPEC §10). Exhaustion is observed, not proof the code is innocent.
    pub(super) fn end_on_candidate_exhaustion(
        &mut self,
        ctx: &AttemptContext<'_>,
        candidate: &Candidate,
        exhausted: (String, exhaustion::Exhaustion, exhaustion::Measured),
    ) -> Result<RunOutcome, RunError> {
        let (label, found, measured) = exhausted;
        self.config.ledger.finish_attempt(
            candidate.attempt_id,
            State::NeedsDecision,
            Some(&ctx.worktree_path.to_string_lossy()),
            Some(&candidate.sha),
        )?;
        let detail = exhaustion::candidate_text(&label, &found, &measured, self.run_id.as_str());
        self.finish(
            Reason::ResourceExhausted,
            exhaustion::detail_json(&label, &found, &measured, Some(&candidate.sha)),
            Terminal::NeedsDecision {
                reason: Reason::ResourceExhausted,
                detail,
            },
        )
    }

    /// End the run on the budget that ran out (ADR-0010, SPEC §11): the
    /// work and total budgets are `budget_exhausted`; a wait budget is a
    /// block, because the machine, not the work, was what ran short.
    /// The heavy-command admission of this run (SPEC §23, ADR-0010).
    pub(crate) fn heavy(&self) -> verify::HeavyAdmission<'_> {
        self.heavy_with(&self.budgets)
    }

    /// [`RunEngine::heavy`] drawing on `budgets`: an on-demand base run's
    /// commands draw on the total only (ADR-0013).
    pub(crate) fn heavy_with(&self, budgets: &Budgets) -> verify::HeavyAdmission<'_> {
        // The coordinator's cap reads the logical cores (relais ADR-0010);
        // the host queue reads the sizing greengate reads: the physical
        // cores and its two host keys, resolved once by the slots crate.
        let plan = verify::host_plan(
            &greenharness_slots::host::from_host(greenharness_slots::detect_cores()),
            self.config.machine.resources.host_slots,
        );
        let (queue_dir, queue_sizing) = host_queue(
            &plan,
            self.config.machine.resources.shared_host_slots.as_deref(),
        );
        let admission = verify::HeavyAdmission {
            gate: self.config.gate,
            run_id: self.run_id.as_str().to_string(),
            session_id: self.config.session_id.clone(),
            budgets: budgets.clone(),
            events: self.events.clone(),
            cap: verify::heavy_cap(
                self.config.machine.concurrency.max_heavy_commands,
                verify::logical_cores(),
            ),
            machine: plan.machine,
            slots: queue_dir,
            sizing: queue_sizing,
            ancestor_holds_slots: verify::ancestor_holds_slots(
                std::env::var(verify::HOST_SLOT_ENV).ok().as_deref(),
            ),
            poll: ADMISSION_POLL,
            announce_every: verify::ANNOUNCE_EVERY,
            heartbeat_every: verify::LEASE_HEARTBEAT_EVERY,
            sleeper: Arc::new(verify::ThreadSleeper),
            jitter: Arc::new(verify::SeededJitter::for_process(self.run_id.as_str())),
            registration: self.registration.clone(),
            tries: self.heavy_tries.clone(),
            exhausted: |ended, tail| crate::exhaustion::classify(ended, tail).is_some(),
            disk: crate::disk::DiskCheck {
                floor: self.config.machine.resources.min_free_bytes(),
                probe: Arc::new(crate::disk::StatvfsProbe),
                paths: self.disk_paths(),
                growth: &self.growth,
            },
        };
        admission.note_host_sizing(&plan);
        admission
    }

    /// The places this run writes (SPEC §23): the state directory, the
    /// repository, the worktrees and `TMPDIR`.
    fn disk_paths(&self) -> Vec<PathBuf> {
        vec![
            self.config.artifacts_dir.clone(),
            self.config.repo_dir.to_path_buf(),
            worktree_root(&self.config.artifacts_dir),
            verify_root(&self.config.artifacts_dir),
            std::env::temp_dir(),
        ]
    }

    /// End the run on a command that was never admitted: a budget that
    /// ran out in the queue ends on its own name, a cancellation as a
    /// cancellation, and a coordinator that is down or refuses as
    /// admission does for model work.
    pub(crate) fn end_on_lease_stop(
        &mut self,
        stop: verify::LeaseStop,
        budget: &Budget,
        candidate: Option<&KeptCandidate>,
    ) -> Result<RunOutcome, RunError> {
        use verify::LeaseStop;
        let text = stop.to_string();
        match stop {
            LeaseStop::Exhausted(exhausted) => self.end_on_exhausted(exhausted, budget, candidate),
            LeaseStop::Cancelled
            | LeaseStop::Refused {
                code: Refusal::RunCancelled,
                ..
            } => self.stop(
                budget,
                Observation::Cancelled("the run was cancelled while a command was queued".into()),
            ),
            LeaseStop::Gate(e) => {
                let (reason, code) = admission_block(&e);
                self.block(reason, code, format!("{e}; the command was not started"))
            }
            LeaseStop::Refused { detail, .. } => self.block(
                Reason::AdmissionRefused,
                BlockCode::AdmissionRefused,
                detail,
            ),
            LeaseStop::Weight { .. } | LeaseStop::HostSlots { .. } => {
                self.block(Reason::AdmissionRefused, BlockCode::AdmissionRefused, text)
            }
            LeaseStop::Duplicate(id) => self.finish(
                Reason::DuplicateDispatch,
                serde_json::json!({ "dispatch_id": id }),
                Terminal::Interrupted {
                    detail: format!("lease {id} was already admitted elsewhere"),
                },
            ),
        }
    }

    /// End the run on a repair or escalation whose wall-time floor the
    /// total budget cannot cover: nothing was launched for it. A candidate
    /// an earlier attempt made is named as kept.
    pub(crate) fn end_on_repair_floor(
        &mut self,
        limit: Limit,
        budget: &Budget,
        candidate: Option<&KeptCandidate>,
    ) -> Result<RunOutcome, RunError> {
        let mut decision = decide(budget, Observation::LimitReached(limit));
        if let (Some(kept), Next::Stop(Terminal::BudgetExhausted { detail })) =
            (candidate, &mut decision.next)
        {
            detail.push_str(&format!("; {}", kept_sentence(kept)));
            decision.detail["detail"] = serde_json::Value::String(detail.clone());
            decision.detail["candidate"] =
                serde_json::json!({ "run": kept.run, "index": kept.index, "sha": kept.sha });
        }
        let next = self.record_decision(decision)?;
        self.outcome_of_stop(next)
    }

    pub(crate) fn end_on_exhausted(
        &mut self,
        exhausted: Exhausted,
        budget: &Budget,
        candidate: Option<&KeptCandidate>,
    ) -> Result<RunOutcome, RunError> {
        let limits = self.budgets.limits();
        match exhausted {
            Exhausted::Work => self.stop(budget, Observation::LimitReached(Limit::WallClock)),
            Exhausted::Total => self.stop(budget, Observation::LimitReached(Limit::Total)),
            Exhausted::AdmissionWait => {
                let mut detail = format!(
                    "queued for a lease for {}s, the whole admission wait budget \
                     (`[resources] admission_wait_seconds`)",
                    limits.admission_wait.as_secs()
                );
                // "Nothing was launched" holds for the lease, not the run: a
                // worker may already have made the candidate.
                if candidate.is_none() {
                    detail.push_str("; nothing was launched");
                }
                let mut detail_json = serde_json::json!({ "detail": detail });
                if let Some(kept) = candidate {
                    detail.push_str(&format!("; {}", kept_sentence(kept)));
                    detail_json = serde_json::json!({
                        "detail": detail,
                        "candidate": { "run": kept.run, "index": kept.index, "sha": kept.sha },
                    });
                }
                self.finish(
                    Reason::AdmissionWaitExhausted,
                    detail_json,
                    Terminal::BlockedBy {
                        reason: Reason::AdmissionWaitExhausted,
                        detail,
                    },
                )
            }
            Exhausted::DiskWait => {
                let detail = format!(
                    "waited {}s for free disk space, the whole disk wait budget \
                     (`[resources] disk_wait_seconds`)",
                    limits.disk_wait.as_secs()
                );
                self.finish(
                    Reason::DiskLow,
                    serde_json::json!({ "detail": detail }),
                    Terminal::BlockedBy {
                        reason: Reason::DiskLow,
                        detail,
                    },
                )
            }
        }
    }
}

/// A route's requested effort, spelled the way a usage event stores it.
/// `None` for a model without effort control — omitted, never guessed.
/// What the pane says of a baseline: how many checks ran and how many
/// were already red on the base.
/// The ledger's clock, read as the budgets' (ADR-0010).
struct LedgerBudgetClock(Arc<dyn crate::ledger::Clock>);

impl BudgetClock for LedgerBudgetClock {
    fn now_millis(&self) -> i64 {
        chrono::DateTime::parse_from_rfc3339(&self.0.now_rfc3339()).map_or_else(
            |_| chrono::Utc::now().timestamp_millis(),
            |at| at.timestamp_millis(),
        )
    }
}

/// The size of each budget for one run: the work budget is the effective
/// `max_wall_seconds` (the repository's, narrowed by `limits.wall_seconds`);
/// the waits come from machine settings; the total is the contract's
/// `limits.total_seconds` or the sum of everything the run may spend
/// (ADR-0010).
pub(super) fn budget_limits(
    authority: &EffectiveAuthority,
    limits: &crate::contract::Limits,
    resources: &crate::policy::ResourceSettings,
) -> BudgetLimits {
    let profile = &authority.verification_profile;
    let baseline_seconds = profile
        .setup
        .iter()
        .chain(&profile.commands)
        .fold(0u64, |sum, command| {
            sum.saturating_add(command.timeout_seconds)
        });
    BudgetLimits::resolve(
        authority.max_wall_seconds,
        resources.admission_wait_seconds(),
        resources.disk_wait_seconds(),
        limits.total_seconds,
        baseline_seconds,
    )
}

/// The host slot directory a run queues on: greengate's own unless
/// `[resources] shared_host_slots` names another, and none for `"off"`
/// (amont ADR-0014: relais joins the host queue with nothing configured).
pub(crate) fn host_slot_dir(setting: Option<&str>) -> Option<PathBuf> {
    match setting {
        None => Some(greenharness_slots::dir()),
        Some("off") => None,
        Some(dir) => Some(PathBuf::from(dir)),
    }
}

/// The host queue a run's commands join, from its `plan` and the
/// `shared_host_slots` setting: the directory and its sizing, or no
/// directory when the host slots are off (the setting says `"off"`, or the
/// plan's sizing is `None`). Pure, so the mapping is tested without a
/// machine; with no directory the sizing is never read.
pub(crate) fn host_queue(
    plan: &verify::HostPlan,
    shared_setting: Option<&str>,
) -> (Option<PathBuf>, greenharness_slots::Sizing) {
    match plan.sizing {
        Some(sizing) => (host_slot_dir(shared_setting), sizing),
        None => (None, greenharness_slots::Sizing::Auto),
    }
}

/// What a cancel watcher needs of a run, all of it shareable across threads.
pub(super) struct CancelWatch<'a> {
    pub(super) gate: Option<&'a (dyn Gate + Sync)>,
    pub(super) every: Duration,
    pub(super) run_id: &'a str,
    pub(super) asked: &'a AtomicBool,
}

/// [`RunEngine::while_watching_cancel`] without the engine, so work on
/// another thread can be watched too.
pub(super) fn watch_cancel<T>(
    watch: &CancelWatch<'_>,
    work: impl FnOnce(Option<&AtomicBool>) -> T,
) -> T {
    let CancelWatch {
        gate,
        every,
        run_id,
        asked,
    } = *watch;
    let Some(gate) = gate else {
        return work(None);
    };
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let done = &done;
        scope.spawn(move || {
            let mut last: Option<Instant> = None;
            while !done.load(Ordering::SeqCst) {
                if last.is_none_or(|last| last.elapsed() >= every) {
                    last = Some(Instant::now());
                    if gate.run_cancelled(run_id).unwrap_or(false) {
                        asked.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let result = work(Some(asked));
        done.store(true, Ordering::SeqCst);
        result
    })
}

#[cfg(test)]
mod host_slot_dir_tests {
    use super::{host_queue, host_slot_dir};
    use std::path::PathBuf;

    fn plan(sizing: Option<greenharness_slots::Sizing>) -> crate::verify::HostPlan {
        crate::verify::HostPlan {
            sizing,
            machine: greenharness_slots::Machine { cores: 6 },
            warnings: Vec::new(),
            off_by: None,
        }
    }

    /// The run joins the host queue only when the plan sizes it: off
    /// (`greengate.hostSlots 0`) means no directory, whatever the setting.
    #[test]
    fn the_host_queue_follows_the_plan_and_the_shared_setting() {
        let fixed = Some(greenharness_slots::Sizing::Fixed(2));
        assert_eq!(
            host_queue(&plan(fixed), None),
            (
                Some(greenharness_slots::dir()),
                greenharness_slots::Sizing::Fixed(2)
            )
        );
        assert_eq!(
            host_queue(&plan(fixed), Some("/run/slots")),
            (
                Some(PathBuf::from("/run/slots")),
                greenharness_slots::Sizing::Fixed(2)
            )
        );
        assert_eq!(host_queue(&plan(fixed), Some("off")).0, None);
        assert_eq!(host_queue(&plan(None), None).0, None);
        assert_eq!(host_queue(&plan(None), Some("/run/slots")).0, None);
    }

    /// With nothing configured relais queues in greengate's directory; `"off"`
    /// is the coordinator's queue alone; a path is used as given.
    #[test]
    fn a_run_with_no_setting_queues_in_greengates_directory() {
        assert_eq!(host_slot_dir(None), Some(greenharness_slots::dir()));
        // And it is the one 3.x's `amont-slots` is not: the crate's name, not
        // only the crate's own answer, says which queue this joins.
        let name = host_slot_dir(None).unwrap();
        let name = name.file_name().unwrap().to_string_lossy();
        assert!(name.starts_with("greenharness-slots"), "{name}");
        assert_eq!(host_slot_dir(Some("off")), None);
        assert_eq!(
            host_slot_dir(Some("/run/slots")),
            Some(PathBuf::from("/run/slots"))
        );
    }
}
