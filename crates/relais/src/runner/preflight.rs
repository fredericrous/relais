//! Preflight: everything before the first worker launches.

use crate::admission::RunRegistration;
use crate::context::{self, ContextError, ContextManifest, GuidanceGap};
use crate::ids::derive_task_id;
use crate::ledger::EvidenceKind;
use crate::money::MicroUsd;
use crate::policy::{effective_authority, repo_key, BlockCode, EffectiveAuthority};
use crate::protocol::Event;
use crate::route::{route, RouteInputs, Routed};
use crate::verify;
use crate::workspace;

use super::{
    admission_block, candidate, effort_str, harness_reading, CandidateConflicts, DesignSource,
    Phase, Preflight, PreparedCandidate, Reason, RunEngine, RunError, RunOutcome, Terminal,
};

impl<'a> RunEngine<'a> {
    /// Everything that must hold before a single worker is launched
    /// (SPEC §4–§7): the run is on record, the base is clean and
    /// resolved, authority and integrations allow the work, the
    /// coordinator knows the run's budget, the context package is
    /// assembled and the route is decided.
    pub(in crate::runner) fn preflight(&mut self) -> Result<Phase<Preflight>, RunError> {
        std::fs::create_dir_all(&self.artifacts)?;
        let ledger = self.config.ledger;
        // The repository half of this run's task identity (P2's
        // `RepoIdentity`, hashed the way a trust grant is): resolved
        // here, ahead of `insert_run`/`insert_child_run`, because both
        // need it to place the run under its task.
        let repo_identity = crate::repo::identity(self.config.repo_dir);
        let this_repo_key = repo_key(&repo_identity);
        match &self.parent {
            None => {
                // A fresh task identity is DERIVED, not minted: the same
                // repository and the same first contract always land on
                // the same task, so a re-run of this task reuses the row
                // its first run created. `task_override` is the CLI's
                // already-confirmed answer to `--revise` or a declared
                // `task_id` — set, it names the task directly rather
                // than deriving one.
                let task = match self.config.task_override {
                    Some(task) => task.clone(),
                    None => derive_task_id(&this_repo_key, &self.config.contract.hash()),
                };
                let inserted = ledger.insert_typed_run(
                    &self.run_id,
                    &self.config.repo_dir.to_string_lossy(),
                    Some(&self.config.session_id),
                    &task,
                    &this_repo_key,
                    self.config.contract.task_type,
                );
                // Before anything can be dispatched. A purpose written
                // after the run finishes is absent for every path that
                // does not reach the end — an error, a kill, a ceiling —
                // and those are exactly the runs whose spend most needs
                // accounting for.
                if let Some(purpose) = self.config.purpose {
                    ledger.set_run_purpose(&self.run_id, purpose)?;
                }
                inserted?;
                // The run is launched (its row exists) and nothing has
                // told the caller its id yet.
                #[cfg(test)]
                crate::ledger::failpoint::hit(crate::ledger::failpoint::AFTER_LAUNCH);
            }
            Some((parent_run, package_id, _role)) => {
                // A work package is not a task of its own (SPEC §19): it
                // inherits the root's, which the root's own preflight
                // already put on record before any package is scheduled.
                let task = ledger.task_of_run(parent_run)?.ok_or_else(|| {
                    RunError::Other(format!(
                        "run {parent_run} has no task on record; its own preflight must \
                         complete before a work package can inherit it"
                    ))
                })?;
                ledger.insert_child_run(
                    &self.run_id,
                    &self.config.repo_dir.to_string_lossy(),
                    Some(&self.config.session_id),
                    crate::ledger::ChildOf {
                        parent_run,
                        package_id,
                    },
                    &task,
                    &this_repo_key,
                )?
            }
        }

        // Preflight: dirty base is explicit, never copied (SPEC §8). A
        // status check that cannot RUN is not a clean tree: it is no
        // answer at all, and proceeding would be the silent copy §8
        // forbids, performed on an unknown tree (audit B11). The refusal
        // is the same one a dirty tree gets, with the reason the check
        // gave.
        // An in-place run starts from the tree as it is: uncommitted work
        // is part of the start snapshot, not a refusal (ADR-0008).
        // A verify applies a merge result into a tree of its own; it never
        // rewrites the person's worktree.
        let in_place = self.config.workspace == crate::workspace::reuse::Workspace::InPlace
            && self.parent.is_none()
            && !self.config.contract.is_verify();
        match workspace::dirty_paths(self.config.repo_dir).map(|dirty| {
            if in_place {
                Vec::new()
            } else {
                dirty
            }
        }) {
            Ok(dirty) if dirty.is_empty() => {}
            Ok(dirty) => {
                return Ok(Phase::Ended(self.fail_preflight(
                    BlockCode::DirtyBase,
                    format!(
                        "working tree has uncommitted changes ({}); commit or stash first",
                        dirty.join(", ")
                    ),
                )?));
            }
            Err(e) => {
                return Ok(Phase::Ended(self.fail_preflight(
                    BlockCode::DirtyBase,
                    format!(
                        "the working tree's status could not be read ({e}); relais cannot show \
                         that the base is clean and will not run against a tree it cannot see"
                    ),
                )?));
            }
        }

        // Effective authority is the intersection; blockers stop dispatch
        // (SPEC §5, §6). `repo_identity` is the same value the task
        // identity above was keyed on, resolved once at the top of this
        // function because this is where the run meets the disk; `policy`
        // decides from the value and looks at nothing.
        let authority = effective_authority(
            self.config.repo_policy,
            self.config.machine,
            self.config.contract,
            &repo_identity,
        );
        if let Some(first) = authority.blockers.first() {
            let (code, detail) = (first.code, first.detail.clone());
            return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
        }
        // Which grant admitted this run, kept on the run: `relais doctor`
        // counts the runs a legacy grant still admits (ADR-0007).
        ledger.set_run_trust_status(&self.run_id, authority.trust_status.as_str())?;
        // A pending command change is `relais run`'s to say: a replay
        // neither says nor records it, so a batch of replays does not
        // repeat the warning once per run (`plan`, `doctor` and `trust
        // show` report it).
        if self.parent.is_none()
            && self.config.purpose != Some(crate::lifecycle::RunPurpose::Replay)
        {
            self.say_command_changes(&this_repo_key);
        }
        if self.config.workspace != crate::workspace::reuse::Workspace::Isolated {
            self.events.emit(Event::Decision {
                what: "workspace_resolved".into(),
                reason: format!(
                    "{} in {}",
                    self.config.workspace.as_str(),
                    self.config.repo_dir.display()
                ),
            });
        }
        // Integrations (SPEC §5): a missing required one blocks execution;
        // optional gaps travel in the receipt and are never passes.
        if let Some(blocker) = crate::tooling::probe_integrations(self.config.repo_policy).first() {
            let (code, detail) = (blocker.code, blocker.detail.clone());
            return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
        }
        let integration_gaps = verify::integration_gaps(
            &self.config.repo_policy.integrations,
            &crate::tooling::binary_available,
        );

        // A weight no machine setting can grant is a profile that cannot
        // run here: refused now, whether or not a coordinator is up.
        let heavy_cap = verify::heavy_cap(
            self.config.machine.concurrency.max_heavy_commands,
            verify::cores(),
        );
        let profile = &authority.verification_profile;
        if let Some(detail) =
            verify::overweight(profile.setup.iter().chain(&profile.commands), heavy_cap)
        {
            return Ok(Phase::Ended(
                self.fail_preflight(BlockCode::AdmissionRefused, detail)?,
            ));
        }

        // Managed runs register their root budget and agent-tree limits
        // before any dispatch; children can only narrow them (SPEC §23).
        if let Some(gate) = self.config.gate {
            let registration = RunRegistration {
                run_id: self.run_id.as_str().to_string(),
                session_id: self.config.session_id.clone(),
                budget_micros: self
                    .config
                    .machine
                    .spending
                    .per_run_micros
                    .map(MicroUsd::to_micros),
                max_agents: Some(authority.max_agents_total),
                max_depth: Some(authority.max_agent_depth),
            };
            if let Err(e) = gate.register_run(&registration) {
                let (reason, code) = admission_block(&e);
                return Ok(Phase::Ended(self.block(
                    reason,
                    code,
                    format!("{e}; nothing was launched"),
                )?));
            }
            self.registration = Some(registration);
        }

        // The base resolves once (SPEC §4).
        let base_sha =
            match workspace::resolve_base(self.config.repo_dir, &self.config.contract.base_ref) {
                Ok(sha) => sha,
                Err(e) => {
                    return Ok(Phase::Ended(
                        self.fail_preflight(BlockCode::BaseUnresolvable, e.to_string())?,
                    ))
                }
            };

        let base_sha = if in_place {
            match self.enter_in_place(base_sha) {
                Ok(sha) => sha,
                Err((code, detail)) => {
                    return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
                }
            }
        } else {
            base_sha
        };

        let contract_hash = self.config.contract.hash();
        let revision_id = ledger.insert_contract_revision(
            &self.run_id,
            &contract_hash,
            &serde_json::to_string(&self.config.contract.canonical_value())
                .expect("a validated contract serializes"),
            &self.config.contract.base_ref,
            Some(&base_sha),
        )?;

        // A candidate is resolved once and checked for conflicts here, in
        // the object store, before anything runs: a conflicting candidate
        // costs no baseline (plan A1, A2).
        let candidate = match self.config.contract.source_candidate() {
            None => None,
            Some(wanted) => match self.prepare_candidate(wanted, &this_repo_key, &base_sha)? {
                Phase::Ended(outcome) => return Ok(Phase::Ended(outcome)),
                Phase::Ready(prepared) => Some(prepared),
            },
        };

        let predecessor =
            ledger
                .intent_of_run(&self.run_id)?
                .map(|intent| crate::contract::Predecessor {
                    run: intent.predecessor.as_str().to_string(),
                    intent_id: intent.intent_id,
                    scope_delta: intent.scope_delta,
                });

        // One probe: the version, the turn-ceiling capability and the
        // efforts the CLI accepts are the same answer about the same
        // installed harness.
        let capabilities = self.config.backend.probe();
        // A `from_candidate` worker sees the merged candidate tree, so its
        // hints resolve there; every other run reads hints at the base.
        let hint_tree = match (&candidate, self.config.contract.from_candidate()) {
            (Some(prepared), Some(_)) => prepared.tree.clone(),
            _ => base_sha.clone(),
        };
        let (manifest, holdouts) = match self.assemble_context(
            &authority,
            &contract_hash,
            &base_sha,
            &hint_tree,
            capabilities.as_ref(),
        )? {
            Phase::Ended(outcome) => return Ok(Phase::Ended(outcome)),
            Phase::Ready(assembled) => assembled,
        };

        // Route (SPEC §6). The catalogs are resolved here, from the probe
        // and machine.toml, so `route` stays pure.
        let catalogs = crate::route::resolve_catalogs(
            self.config.repo_policy,
            &authority,
            self.config.machine,
            capabilities.as_ref().map(|c| &c.accepted_efforts),
        );
        // The declared type, else the ledger's classified one. Only a run
        // the replay command started (`RunPurpose::Replay`, set nowhere
        // else) is admitted below the implementation tier for a change.
        let task = ledger.task_of_run(&self.run_id)?;
        let task_type = match &task {
            Some(task) => ledger.routing_type(task, self.config.contract.task_type)?,
            None => self.config.contract.task_type,
        };
        let decision = match route(RouteInputs {
            draw_key: task.as_ref().map(|task| crate::route::DrawKey {
                task: task.as_str(),
                run: self.run_id.as_str(),
            }),
            task_type,
            has_design: self.config.design.is_present(self.config.contract),
            admission: if self.config.purpose == Some(crate::lifecycle::RunPurpose::Replay) {
                crate::route::Admission::Replay
            } else {
                crate::route::Admission::Live
            },
            contract: self.config.contract,
            repo: self.config.repo_policy,
            machine: self.config.machine,
            authority: &authority,
            predictor: self.config.predictor,
            catalogs: &catalogs,
        }) {
            Routed::Route(route) => route,
            Routed::Blocked(blocked) => {
                let first = blocked.first();
                let (code, detail) = (first.code, first.detail.clone());
                return Ok(Phase::Ended(self.fail_preflight(code, detail)?));
            }
        };
        // A verify dispatches no worker: there is nothing to predict.
        let estimates = decision
            .estimates
            .as_ref()
            .filter(|_| !self.config.contract.is_verify());
        if let Some(estimates) = estimates {
            ledger.record_prediction(
                &self.run_id,
                &estimates.artifact_id,
                &estimates.input_hash,
                &estimates.raw,
            )?;
        }
        std::fs::write(self.artifacts.join("route.txt"), decision.explain())?;
        let rung = &decision.rung;
        let effort = effort_str(rung.effort.id())
            .map(|effort| format!("@{effort}"))
            .unwrap_or_default();
        self.events.emit(Event::Step {
            name: "preflight".to_string(),
            detail: format!("route {} · {}{effort}", decision.tier.as_str(), rung.model),
            max_attempts: Some(decision.max_attempts),
        });

        Ok(Phase::Ready(Preflight {
            authority,
            integration_gaps,
            base_sha,
            contract_hash,
            revision_id,
            manifest,
            holdouts,
            decision,
            candidate,
            predecessor,
        }))
    }

    /// Resolve the candidate a verify names, persist it, and merge it into
    /// the target base in the object store. A refusal or a conflict ends
    /// the run `blocked` before the baseline.
    fn prepare_candidate(
        &mut self,
        wanted: &crate::contract::CandidateRef,
        repo_key: &str,
        base_sha: &str,
    ) -> Result<Phase<PreparedCandidate>, RunError> {
        let ledger = self.config.ledger;
        if let Err(detail) = workspace::merge_tree_supported() {
            return Ok(Phase::Ended(
                self.fail_preflight(BlockCode::VerificationUnavailable, detail)?,
            ));
        }
        let source = candidate::Source {
            repo_dir: self.config.repo_dir,
            artifacts_dir: &self.config.artifacts_dir,
        };
        let resolved = match ledger.candidate_input(&self.run_id)? {
            // Resolved once: a run that already has its row applies it.
            Some(stored) => {
                if stored.target_base_sha != base_sha {
                    let detail = format!(
                        "the candidate was resolved against base {} but this run's base is {base_sha}; \
                         detection and application must use one base",
                        stored.target_base_sha
                    );
                    return Ok(Phase::Ended(
                        self.fail_preflight(BlockCode::BaseUnresolvable, detail)?,
                    ));
                }
                stored
            }
            None => match candidate::resolve(ledger, &source, repo_key, base_sha, wanted)? {
                candidate::Resolution::Refused(refusal) => {
                    return Ok(Phase::Ended(self.refuse_candidate(refusal)?));
                }
                candidate::Resolution::Resolved(resolved) => {
                    ledger.record_candidate_input(&self.run_id, &resolved)?;
                    resolved
                }
            },
        };
        self.events.emit(Event::Decision {
            what: "candidate_resolved".into(),
            reason: format!(
                "{}/{} at {}",
                resolved.source_run, resolved.index, resolved.candidate_commit
            ),
        });
        let merged = workspace::merge_candidate(
            self.config.repo_dir,
            &resolved.source_base_sha,
            &resolved.target_base_sha,
            &resolved.candidate_commit,
        );
        match merged {
            Ok(workspace::Merged::Clean { tree }) => Ok(Phase::Ready(PreparedCandidate {
                resolved,
                tree,
                conflicts: None,
            })),
            // A verify judges the candidate as it is: it cannot be asked to
            // resolve a merge. A change that continues from one is handed
            // the merge instead (plan A4).
            Ok(workspace::Merged::Conflicted {
                tree,
                files,
                index_info,
            }) => {
                if self.config.contract.is_verify() {
                    let detail = candidate::conflict_detail(&resolved, &files);
                    return Ok(Phase::Ended(self.refuse_candidate(candidate::Refusal {
                        reason: Reason::CandidateDoesNotApply,
                        detail,
                    })?));
                }
                self.events.emit(Event::Decision {
                    what: "candidate_conflicts".into(),
                    reason: format!(
                        "{}/{} conflicts with the base in {}; the worker resolves them first",
                        resolved.source_run,
                        resolved.index,
                        files.join(", ")
                    ),
                });
                Ok(Phase::Ready(PreparedCandidate {
                    resolved,
                    tree,
                    conflicts: Some(CandidateConflicts { files, index_info }),
                }))
            }
            Err(e) => Ok(Phase::Ended(
                self.fail_preflight(BlockCode::SnapshotFailed, e.to_string())?,
            )),
        }
    }

    /// End the run `blocked` under the reason a candidate refusal names.
    fn refuse_candidate(&mut self, refusal: candidate::Refusal) -> Result<RunOutcome, RunError> {
        let candidate::Refusal { reason, detail } = refusal;
        self.finish(
            reason,
            serde_json::json!({ "detail": detail }),
            Terminal::BlockedBy { reason, detail },
        )
    }

    /// Before a verify's tree is built: the identity resolved in preflight
    /// still holds (the object, the derived digest, the patch files). A
    /// file changed between validation and now ends the run
    /// `candidate_digest_mismatch`.
    pub(in crate::runner) fn recheck_candidate(
        &mut self,
        prepared: &PreparedCandidate,
    ) -> Result<Option<RunOutcome>, RunError> {
        let source = candidate::Source {
            repo_dir: self.config.repo_dir,
            artifacts_dir: &self.config.artifacts_dir,
        };
        match candidate::cross_check(self.config.ledger, &source, &prepared.resolved)? {
            Some(refusal) => Ok(Some(self.refuse_candidate(refusal)?)),
            None => Ok(None),
        }
    }

    /// Context: verdicts and tool failures are distinct, contradictions
    /// block, missing answers gate dependents only (SPEC §7). The
    /// assembled package is evidence, written next to the run and
    /// referenced by hash (SPEC §7, §12).
    fn assemble_context(
        &mut self,
        authority: &EffectiveAuthority,
        contract_hash: &str,
        base_sha: &str,
        hint_tree: &str,
        capabilities: Option<&crate::backend::Capabilities>,
    ) -> Result<Phase<(ContextManifest, Vec<crate::holdout::ResolvedHoldout>)>, RunError> {
        // Read hints are fingerprinted from `hint_tree`; one that
        // resolves to nothing would point the worker at a path this
        // revision does not have, which is a preflight problem (V15).
        let fingerprints = match context::fingerprint_hints(
            self.config.git,
            self.config.repo_dir,
            hint_tree,
            &self.config.contract.read_hints,
        ) {
            Ok(fingerprints) => fingerprints,
            Err(unresolvable) => {
                return Ok(Phase::Ended(self.fail_preflight(
                    BlockCode::ReadHintUnresolvable,
                    unresolvable.to_string(),
                )?))
            }
        };
        // The design is read from the base revision, like the hints, and an
        // unreadable one refuses the contract before any worker is paid for.
        let design = match (self.config.design, &self.config.contract.design) {
            (DesignSource::Given(design), _) => Some(design.clone()),
            (DesignSource::Contract, None) => None,
            (DesignSource::Contract, Some(path)) => {
                match context::read_design(self.config.git, self.config.repo_dir, base_sha, path) {
                    Ok(design) => Some(design),
                    Err(e) => {
                        return Ok(Phase::Ended(
                            self.fail_preflight(BlockCode::DesignUnreadable, e.to_string())?,
                        ))
                    }
                }
            }
        };
        // Holdout criteria are resolved here too: an undeclared, unusable
        // or unreachable holdout refuses the contract before any worker is
        // paid for. The manifest keeps only name, commit and digest.
        let holdouts = match crate::holdout::resolve_all(
            &self.config.contract.acceptance,
            &self.config.machine.holdouts,
            &crate::repo::main_checkout_name(self.config.repo_dir).unwrap_or_default(),
            self.config.git,
            self.config.repo_dir,
            base_sha,
            &crate::holdout::machine_toml_label(),
        ) {
            Ok(resolved) => resolved,
            Err(block) => return Ok(Phase::Ended(self.fail_preflight(block.code, block.detail)?)),
        };
        let (turn_ceiling, warning) = harness_reading(
            capabilities,
            capabilities
                .is_none()
                .then(|| self.config.backend.probe_timed_out())
                .flatten(),
        );
        if let Some(warning) = warning {
            self.events.emit(warning);
        }
        let assembled = context::assemble(context::ContextInputs {
            contract: self.config.contract,
            repo: self.config.repo_policy,
            contract_hash,
            base_sha,
            policy_hash: &authority.authority_hash,
            fingerprints,
            tool_versions: context::ToolVersions {
                relais: crate::version().to_string(),
                aval: crate::tooling::integration_version("aval"),
                amont: crate::tooling::integration_version("amont"),
                claude_code: capabilities.and_then(|capabilities| capabilities.version.clone()),
            },
            // Recorded so a receipt says whether a turn ceiling was
            // enforceable at all on this harness (SPEC §11).
            turn_ceiling,
            resolver: self.config.aval_resolver,
            design,
            repo_name: crate::repo::main_checkout_name(self.config.repo_dir),
            holdouts: holdouts.iter().map(|holdout| holdout.record()).collect(),
        });
        match assembled {
            Ok(manifest) => {
                for gap in &manifest.guidance.gaps {
                    self.emit_guidance_gap(gap);
                }
                let manifest_path = self.artifacts.join("manifest.json");
                std::fs::write(
                    &manifest_path,
                    serde_json::to_string_pretty(&manifest).expect("a manifest serializes"),
                )?;
                self.config.ledger.record_evidence(
                    &self.run_id,
                    None,
                    EvidenceKind::ContextManifest,
                    &manifest_path,
                    Some(&context::manifest_hash(&manifest)),
                )?;
                Ok(Phase::Ready((manifest, holdouts)))
            }
            Err(ContextError::ContradictionBlocked { key, heads }) => {
                Ok(Phase::Ended(self.block(
                    Reason::ArchitectureContradiction,
                    BlockCode::ArchitectureContradiction,
                    format!("aval contradiction on `{key}` ({heads} heads)"),
                )?))
            }
            Err(ContextError::NeedsDecision { key, verdict }) => {
                let detail = format!("the task depends on `{key}` but aval answers {verdict:?}");
                Ok(Phase::Ended(self.finish(
                    Reason::ArchitectureUnresolved,
                    serde_json::json!({ "key": key, "verdict": format!("{verdict:?}") }),
                    Terminal::NeedsDecision {
                        reason: Reason::ArchitectureUnresolved,
                        detail,
                    },
                )?))
            }
            Err(ContextError::ToolFailure { key, detail }) => Ok(Phase::Ended(
                self.fail_preflight(BlockCode::AvalToolFailure, format!("`{key}`: {detail}"))?,
            )),
            Err(ContextError::SizingProblem { .. }) => Ok(Phase::Ended(self.fail_preflight(
                BlockCode::ContextSizing,
                "required context exceeds the budget".to_string(),
            )?)),
        }
    }

    /// A guidance gap is an event of the run, never a failure of it.
    pub(in crate::runner) fn emit_guidance_gap(&self, gap: &GuidanceGap) {
        self.events.emit(Event::Decision {
            what: "guidance".to_string(),
            reason: gap.describe(),
        });
    }

    /// The receipt's guidance note: one line, and only when guidance had
    /// a gap, the reviewer's included.
    pub(in crate::runner) fn guidance_note(&self, manifest: &ContextManifest) -> Option<String> {
        let mut gaps = manifest.guidance.gaps.clone();
        gaps.extend(self.worker_code_rules_gap.clone());
        gaps.extend(self.reviewer_gap.clone());
        context::guidance::gap_line(&gaps)
    }
}
