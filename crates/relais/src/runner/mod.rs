//! Attempt lifecycle (SPEC §9, §19).
//!
//! States: prepared, running, verifying, repairing, escalating, accepted,
//! needs_review, needs_decision, blocked, failed, budget_exhausted,
//! cancelled, interrupted. Every transition carries a reason code,
//! timestamp and evidence references. Workers can propose completion or
//! blockage; only the runner assigns final state — a completion claim
//! never skips verification. Escalation uses a fresh context that labels
//! previous model claims unverified, never changes acceptance criteria,
//! and default ceilings are at most three attempts: initial, one repair,
//! one stronger attempt. A bounded planner may propose work packages
//! under explicit aggregate limits.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::adapter::{Backend, LaunchSpec};
use crate::context::{self, AvalVerdict, ContextError, ContextManifest};
use crate::contract::{Review, TaskContract};
use crate::ids::{DispatchId, RunId};
use crate::ledger::{now_rfc3339, Ledger, Transition, UsageEvent};
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::policy::{effective_authority, EffectiveAuthority, MachineSettings, RepoPolicy, Tier};
use crate::route::{route, RouteDecision, RouteInputs, RoutePredictor};
use crate::verify::{self, amont_gaps, Receipt, VerificationReport};
use crate::workspace::{self, TaskWorktree, WorkspaceError};
use serde::Serialize;

/// Reason codes recorded on every transition (SPEC §9's observation
/// table). Stable strings so `explain` and reports stay comparable
/// across versions.
pub mod reason {
    pub const CHECKS_AND_REVIEW_PASSED: &str = "checks_and_review_passed";
    pub const BEHAVIORAL_FAILURE: &str = "behavioral_failure";
    pub const REPAIR_EXHAUSTED: &str = "repair_exhausted";
    pub const AMBIGUOUS_DIAGNOSIS: &str = "ambiguous_diagnosis";
    pub const SAME_FAILURE_RECURRENCE: &str = "same_failure_recurrence";
    pub const ENV_MISSING: &str = "env_missing";
    pub const ARCHITECTURE_UNRESOLVED: &str = "architecture_unresolved";
    pub const SCOPE_EXCEEDED: &str = "scope_exceeded";
    pub const REVIEW_UNAVAILABLE: &str = "review_unavailable";
    pub const LIMIT_REACHED: &str = "limit_reached";
    pub const PROCESS_CRASH: &str = "process_crash";
    pub const CANCELLED_BY_USER: &str = "cancelled_by_user";
    pub const RECONCILED_INTERRUPTED: &str = "reconciled_interrupted";
    pub const BLOCKED_PREFLIGHT: &str = "blocked_preflight";
    pub const ESCALATION_NOT_AUTHORIZED: &str = "escalation_not_authorized";
    pub const MODEL_UNAVAILABLE: &str = "model_unavailable";
    pub const UNAPPROVED_SUBSTITUTION: &str = "unapproved_substitution";
    pub const ARCHITECTURE_CONTRADICTION: &str = "architecture_contradiction";
    pub const VERIFICATION_GAP: &str = "verification_gap";
    pub const BASELINE_FAILURE_NOT_WAIVED: &str = "baseline_failure_not_waived";
    pub const REVIEW_FINDINGS: &str = "review_findings";
}

/// The attempt lifecycle states (SPEC §9). Stored as their snake_case
/// names in the ledger; only the runner assigns terminal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Prepared,
    Running,
    Verifying,
    Repairing,
    Escalating,
    Accepted,
    NeedsReview,
    NeedsDecision,
    Blocked,
    Failed,
    BudgetExhausted,
    Cancelled,
    Interrupted,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Prepared => "prepared",
            State::Running => "running",
            State::Verifying => "verifying",
            State::Repairing => "repairing",
            State::Escalating => "escalating",
            State::Accepted => "accepted",
            State::NeedsReview => "needs_review",
            State::NeedsDecision => "needs_decision",
            State::Blocked => "blocked",
            State::Failed => "failed",
            State::BudgetExhausted => "budget_exhausted",
            State::Cancelled => "cancelled",
            State::Interrupted => "interrupted",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "prepared" => State::Prepared,
            "running" => State::Running,
            "verifying" => State::Verifying,
            "repairing" => State::Repairing,
            "escalating" => State::Escalating,
            "accepted" => State::Accepted,
            "needs_review" => State::NeedsReview,
            "needs_decision" => State::NeedsDecision,
            "blocked" => State::Blocked,
            "failed" => State::Failed,
            "budget_exhausted" => State::BudgetExhausted,
            "cancelled" => State::Cancelled,
            "interrupted" => State::Interrupted,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Accepted
                | State::NeedsReview
                | State::NeedsDecision
                | State::Blocked
                | State::Failed
                | State::BudgetExhausted
                | State::Cancelled
                | State::Interrupted
        )
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One executed run's terminal result, carrying the run identity so the
/// CLI can point at the ledger and artifacts.
#[derive(Debug, Clone, PartialEq)]
pub enum RunOutcome {
    Accepted {
        run_id: String,
        receipt: Box<Receipt>,
    },
    NeedsDecision {
        run_id: String,
        reason: String,
        detail: String,
    },
    NeedsReview {
        run_id: String,
        detail: String,
    },
    Blocked {
        run_id: String,
        code: String,
        detail: String,
    },
    Failed {
        run_id: String,
        detail: String,
    },
    BudgetExhausted {
        run_id: String,
        detail: String,
    },
    Interrupted {
        run_id: String,
        detail: String,
    },
}

impl RunOutcome {
    pub fn run_id(&self) -> &str {
        match self {
            RunOutcome::Accepted { run_id, .. }
            | RunOutcome::NeedsDecision { run_id, .. }
            | RunOutcome::NeedsReview { run_id, .. }
            | RunOutcome::Blocked { run_id, .. }
            | RunOutcome::Failed { run_id, .. }
            | RunOutcome::BudgetExhausted { run_id, .. }
            | RunOutcome::Interrupted { run_id, .. } => run_id,
        }
    }

    pub fn state(&self) -> State {
        match self {
            RunOutcome::Accepted { .. } => State::Accepted,
            RunOutcome::NeedsDecision { .. } => State::NeedsDecision,
            RunOutcome::NeedsReview { .. } => State::NeedsReview,
            RunOutcome::Blocked { .. } => State::Blocked,
            RunOutcome::Failed { .. } => State::Failed,
            RunOutcome::BudgetExhausted { .. } => State::BudgetExhausted,
            RunOutcome::Interrupted { .. } => State::Interrupted,
        }
    }
}

pub struct RunConfig<'a> {
    pub repo_dir: &'a Path,
    pub contract: &'a TaskContract,
    pub repo_policy: &'a RepoPolicy,
    pub machine: &'a MachineSettings,
    pub ledger: &'a Ledger,
    pub backend: &'a dyn Backend,
    pub artifacts_dir: PathBuf,
    /// aval resolution, injectable so runs are testable without the real
    /// corpus; production wiring calls `context::aval_resolve`.
    pub aval_resolver: &'a dyn Fn(&str, Option<&str>) -> AvalVerdict,
    pub predictor: Option<&'a dyn RoutePredictor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptKind {
    Initial,
    Repair,
    Escalation,
}

/// The supervised execution path (SPEC §3): preflight, route, then a
/// bounded sequence of attempts the runner — not a model — owns.
pub fn execute(config: &RunConfig<'_>) -> RunOutcome {
    let run = RunEngine::new(config);
    run.run()
}

struct RunEngine<'a> {
    config: &'a RunConfig<'a>,
    run_id: String,
    artifacts: PathBuf,
    state: State,
}

impl<'a> RunEngine<'a> {
    fn new(config: &'a RunConfig<'a>) -> Self {
        let run_id = RunId::generate();
        let artifacts = config.artifacts_dir.join(run_id.as_str());
        Self {
            config,
            run_id: run_id.to_string(),
            artifacts,
            state: State::Prepared,
        }
    }

    fn transition(&mut self, to: State, reason_code: &str, detail: serde_json::Value) {
        let from = self.state;
        self.config
            .ledger
            .record_transition(&Transition {
                run_id: self.run_id.clone(),
                attempt_id: None,
                from_state: Some(from),
                to_state: to,
                reason: reason_code.to_string(),
                detail: Some(detail),
                at: now_rfc3339(),
            })
            .expect("ledger records transitions");
        self.state = to;
    }

    fn outcome_for(&self, outcome: RunOutcome) -> RunOutcome {
        outcome
    }

    fn fail_preflight(&mut self, code: &str, detail: String) -> RunOutcome {
        let outcome = RunOutcome::Blocked {
            run_id: self.run_id.clone(),
            code: code.to_string(),
            detail: detail.clone(),
        };
        self.transition(
            State::Blocked,
            reason::BLOCKED_PREFLIGHT,
            serde_json::json!({ "code": code, "detail": detail }),
        );
        self.outcome_for(outcome)
    }

    fn run(mut self) -> RunOutcome {
        std::fs::create_dir_all(&self.artifacts).expect("artifact directory");
        let ledger = self.config.ledger;
        ledger
            .insert_run(&self.run_id, &self.config.repo_dir.to_string_lossy(), None)
            .expect("ledger records the run");

        // Preflight: dirty base is explicit, never copied (SPEC §8).
        if let Ok(dirty) = workspace::dirty_paths(self.config.repo_dir) {
            if !dirty.is_empty() {
                return self.fail_preflight(
                    "dirty_base",
                    format!(
                        "working tree has uncommitted changes ({}); commit or stash first",
                        dirty.join(", ")
                    ),
                );
            }
        }

        // Effective authority is the intersection; blockers stop dispatch
        // (SPEC §5, §6).
        let authority = effective_authority(
            self.config.repo_policy,
            self.config.machine,
            self.config.contract,
        );
        if !authority.blockers.is_empty() {
            let first = &authority.blockers[0];
            return self.fail_preflight(&first.code, first.detail.clone());
        }

        // The base resolves once (SPEC §4).
        let base_sha =
            match workspace::resolve_base(self.config.repo_dir, &self.config.contract.base_ref) {
                Ok(sha) => sha,
                Err(e) => return self.fail_preflight("base_unresolvable", e.to_string()),
            };

        let contract_hash = self.config.contract.hash();
        let revision_id = ledger
            .insert_contract_revision(
                &self.run_id,
                &contract_hash,
                &serde_json::to_string(&self.config.contract.canonical_value())
                    .expect("serializes"),
                &self.config.contract.base_ref,
                Some(&base_sha),
            )
            .expect("ledger records the contract revision");

        // Context: verdicts and tool failures are distinct, contradictions
        // block, missing answers gate dependents only (SPEC §7).
        let resolver = |key: &str, scope: Option<&str>| (self.config.aval_resolver)(key, scope);
        let manifest = match context::assemble(context::ContextInputs {
            contract: self.config.contract,
            repo: self.config.repo_policy,
            contract_hash: &contract_hash,
            base_sha: &base_sha,
            policy_hash: &authority.authority_hash,
            fingerprints: Vec::new(),
            tool_versions: context::ToolVersions {
                relais: crate::version().to_string(),
                aval: None,
                amont: None,
                claude_code: None,
            },
            resolver: &resolver,
        }) {
            Ok(manifest) => manifest,
            Err(ContextError::ContradictionBlocked { key, heads }) => {
                let detail = format!("aval contradiction on `{key}` ({heads} heads)");
                self.transition(
                    State::Blocked,
                    reason::ARCHITECTURE_CONTRADICTION,
                    serde_json::json!({ "key": key, "heads": heads }),
                );
                return self.outcome_for(RunOutcome::Blocked {
                    run_id: self.run_id.clone(),
                    code: "architecture_contradiction".into(),
                    detail,
                });
            }
            Err(ContextError::NeedsDecision { key, verdict }) => {
                let detail = format!("the task depends on `{key}` but aval answers {verdict:?}");
                self.transition(
                    State::NeedsDecision,
                    reason::ARCHITECTURE_UNRESOLVED,
                    serde_json::json!({ "key": key, "verdict": format!("{verdict:?}") }),
                );
                return self.outcome_for(RunOutcome::NeedsDecision {
                    run_id: self.run_id.clone(),
                    reason: reason::ARCHITECTURE_UNRESOLVED.into(),
                    detail,
                });
            }
            Err(ContextError::ToolFailure { key, detail }) => {
                return self.fail_preflight("aval_tool_failure", format!("`{key}`: {detail}"));
            }
            Err(ContextError::SizingProblem { .. }) => {
                return self.fail_preflight(
                    "context_sizing",
                    "required context exceeds the budget".to_string(),
                );
            }
        };

        // Route (SPEC §6).
        let decision = route(RouteInputs {
            contract: self.config.contract,
            repo: self.config.repo_policy,
            machine: self.config.machine,
            authority: &authority,
            predictor: self.config.predictor,
        });
        if decision.tier.is_none() {
            let first = &decision.blocked[0];
            return self.fail_preflight(&first.code, first.detail.clone());
        }
        std::fs::write(
            self.artifacts.join("route.txt"),
            decision.explain(
                decision
                    .tier
                    .and_then(|tier| authority.models.get(&tier))
                    .map(|profile| profile.id.as_str()),
            ),
        )
        .expect("route artifact");
        let initial_tier = decision.tier.expect("checked");

        // Baseline verification at the base SHA: pre-existing failures are
        // visible from the start (SPEC §10).
        let logs_dir = self.artifacts.join("logs");
        let baseline_failures = match verify::verification_worktree(
            self.config.repo_dir,
            &base_sha,
            &self.artifacts.join("verify-base"),
        ) {
            Ok(baseline) => verify::run_profile(
                baseline.path(),
                &authority.verification_profile,
                &logs_dir,
                "base",
            )
            .expect("baseline verification runs")
            .into_iter()
            .filter(|outcome| outcome.failed())
            .map(|outcome| outcome.label)
            .collect::<Vec<String>>(),
            Err(e) => {
                return self.fail_preflight("baseline_verification_failed", e.to_string());
            }
        };

        // One owned worktree for the whole run: repairs continue from a
        // candidate whose scope and integrity passed; a scope violation
        // stops everything (SPEC §8, §9).
        let worktree_path = self.artifacts.join("worktree");
        let worktree =
            match workspace::create_worktree(self.config.repo_dir, &base_sha, &worktree_path) {
                Ok(worktree) => worktree,
                Err(e) => return self.fail_preflight("worktree_unavailable", e.to_string()),
            };

        let deadline = Instant::now() + Duration::from_secs(authority.max_wall_seconds);
        let mut attempt_index: u32 = 0;
        let mut tier = initial_tier;
        let mut kind = AttemptKind::Initial;
        let mut repairs_used: u32 = 0;
        let mut last_failures: Option<Vec<String>> = None;
        let mut last_candidate: Option<String> = None;
        let mut total_cost = MicroUsd::ZERO;
        let mut cost_completeness = CostCompleteness::Actual;
        let mut models_used: Vec<String> = Vec::new();

        loop {
            // Budget ceilings are checked BEFORE admitting more work
            // (SPEC §9, §11).
            if attempt_index >= authority.max_attempts {
                let detail = format!(
                    "attempt ceiling reached ({}) with failures: {:?}",
                    authority.max_attempts,
                    last_failures.unwrap_or_default()
                );
                self.transition(
                    State::BudgetExhausted,
                    reason::LIMIT_REACHED,
                    serde_json::json!({ "detail": detail }),
                );
                return self.outcome_for(RunOutcome::BudgetExhausted {
                    run_id: self.run_id.clone(),
                    detail,
                });
            }
            if Instant::now() >= deadline {
                let detail = "wall clock for the run is exhausted".to_string();
                self.transition(
                    State::BudgetExhausted,
                    reason::LIMIT_REACHED,
                    serde_json::json!({}),
                );
                return self.outcome_for(RunOutcome::BudgetExhausted {
                    run_id: self.run_id.clone(),
                    detail,
                });
            }
            if let Some(ceiling) = self.config.machine.spending.per_run_micros {
                if total_cost.to_micros() >= ceiling {
                    let detail = format!(
                        "per-run spend ceiling reached ({} of {})",
                        total_cost,
                        MicroUsd::from_micros(ceiling)
                    );
                    self.transition(
                        State::BudgetExhausted,
                        reason::LIMIT_REACHED,
                        serde_json::json!({}),
                    );
                    return self.outcome_for(RunOutcome::BudgetExhausted {
                        run_id: self.run_id.clone(),
                        detail,
                    });
                }
            }

            attempt_index += 1;
            let attempt_id = ledger
                .insert_attempt(
                    &self.run_id,
                    revision_id,
                    attempt_index as i64,
                    tier.as_str(),
                    match kind {
                        AttemptKind::Initial => "initial",
                        AttemptKind::Repair => "repair",
                        AttemptKind::Escalation => "escalation",
                    },
                )
                .expect("attempt recorded");

            let model_profile = &authority.models[&tier];
            let prompt = build_prompt(
                self.config.contract,
                &manifest,
                &decision,
                last_failures.as_deref(),
                kind,
            );

            // Dispatch intent is persisted BEFORE the process exists
            // (SPEC §12), keyed so retries cannot duplicate agents.
            let dispatch_id = DispatchId::generate();
            ledger
                .record_dispatch_intent(
                    dispatch_id.as_str(),
                    &self.run_id,
                    Some(attempt_id),
                    &serde_json::json!({
                        "model": model_profile.id,
                        "effort": model_profile.effort,
                        "tier": tier.as_str(),
                        "kind": format!("{kind:?}"),
                        "prompt_bytes": prompt.len(),
                    }),
                    0,
                )
                .expect("intent recorded");
            ledger
                .record_features(
                    dispatch_id.as_str(),
                    &serde_json::json!({
                        "attempt_index": attempt_index,
                        "tier": tier.as_str(),
                        "model": model_profile.id,
                        "kind": format!("{kind:?}"),
                    }),
                )
                .expect("features recorded");

            let remaining_wall = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_secs(1));
            let spec = LaunchSpec {
                dispatch_id: dispatch_id.as_str().to_string(),
                prompt,
                model: model_profile.id.clone(),
                effort: model_profile.effort,
                max_turns: None,
                budget_micros: self.config.machine.spending.per_run_micros,
                disallowed_tools: authority.disallowed_tools.clone(),
                work_dir: worktree_path.clone(),
                wall_timeout: remaining_wall,
            };

            let result = match self.config.backend.launch(&spec) {
                Ok(result) => result,
                Err(e) => {
                    ledger
                        .finish_dispatch(dispatch_id.as_str(), "launch_failed")
                        .expect("finish");
                    return self.fail_preflight("backend_unavailable", e.to_string());
                }
            };
            ledger
                .attach_dispatch_process(dispatch_id.as_str(), None, result.session_id.as_deref())
                .expect("attach");
            ledger
                .finish_dispatch(dispatch_id.as_str(), "completed")
                .expect("finish");

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
                cost: usage.cost.unwrap_or(MicroUsd::ZERO),
                cost_kind: CostKind::ApiSpend,
                completeness: usage.cost_completeness,
                inclusive: usage.inclusive,
                at: now_rfc3339(),
            };
            ledger.record_usage(&event).expect("usage recorded");
            total_cost += event.cost;
            cost_completeness = match (cost_completeness, usage.cost_completeness) {
                (_, CostCompleteness::Unknown) | (CostCompleteness::Unknown, _) => {
                    CostCompleteness::Unknown
                }
                (CostCompleteness::IncompleteLowerBound, _)
                | (_, CostCompleteness::IncompleteLowerBound) => {
                    CostCompleteness::IncompleteLowerBound
                }
                (CostCompleteness::Estimated, _) | (_, CostCompleteness::Estimated) => {
                    CostCompleteness::Estimated
                }
                (CostCompleteness::Actual, CostCompleteness::Actual) => CostCompleteness::Actual,
            };
            if let Some(model) = &result.effective_model {
                if !models_used.contains(model) {
                    models_used.push(model.clone());
                }
            }

            // An unapproved substitution stops further dispatch and
            // invalidates any claim that the requested route was tested
            // (SPEC §6).
            if result
                .effective_model
                .as_deref()
                .is_some_and(|model| model != model_profile.id)
            {
                let detail = format!(
                    "provider substituted {} for the requested {}; no further dispatch",
                    result.effective_model.as_deref().unwrap_or("?"),
                    model_profile.id
                );
                self.transition(
                    State::Failed,
                    reason::UNAPPROVED_SUBSTITUTION,
                    serde_json::json!({ "requested": model_profile.id, "effective": result.effective_model }),
                );
                return self.outcome_for(RunOutcome::Failed {
                    run_id: self.run_id.clone(),
                    detail,
                });
            }

            // Missing terminal result = interrupted, not failed (SPEC §9).
            if result.terminal_result_missing() {
                ledger
                    .finish_attempt(
                        attempt_id,
                        State::Interrupted,
                        Some(worktree_path.to_string_lossy().as_ref()),
                        None,
                    )
                    .expect("attempt");
                let detail =
                    "the worker process ended without a terminal result; state is interrupted and the worktree is preserved"
                        .to_string();
                self.transition(
                    State::Interrupted,
                    reason::PROCESS_CRASH,
                    serde_json::json!({ "timed_out": result.timed_out }),
                );
                return self.outcome_for(RunOutcome::Interrupted {
                    run_id: self.run_id.clone(),
                    detail,
                });
            }

            // A worker blockage proposal is recorded as evidence and the
            // runner assigns blocked — the environment is never escalated
            // to a stronger model (SPEC §9).
            if result.worker_claims_blockage {
                ledger
                    .finish_attempt(attempt_id, State::Blocked, None, None)
                    .expect("attempt");
                let claim = result.result_text.unwrap_or_default();
                let detail = format!(
                    "worker blockage: {}",
                    claim.lines().last().unwrap_or(&claim)
                );
                self.transition(
                    State::Blocked,
                    reason::ENV_MISSING,
                    serde_json::json!({ "claim": claim }),
                );
                return self.outcome_for(RunOutcome::Blocked {
                    run_id: self.run_id.clone(),
                    code: "env_missing".into(),
                    detail,
                });
            }

            // The candidate snapshot is recorded outside model control,
            // added files included (SPEC §8).
            let candidate_sha = match worktree
                .snapshot_candidate(&format!("run {} attempt {attempt_index}", self.run_id))
            {
                Ok(sha) => sha,
                Err(e) => {
                    ledger
                        .finish_attempt(attempt_id, State::Interrupted, None, None)
                        .expect("attempt");
                    return self.fail_preflight("snapshot_failed", e.to_string());
                }
            };
            ledger
                .finish_attempt(
                    attempt_id,
                    State::Verifying,
                    Some(worktree_path.to_string_lossy().as_ref()),
                    Some(&candidate_sha),
                )
                .expect("attempt");
            let _ = std::fs::write(
                self.artifacts
                    .join(format!("candidate-{attempt_index}.sha")),
                &candidate_sha,
            );
            worktree
                .export_patch(
                    &candidate_sha,
                    &self
                        .artifacts
                        .join(format!("candidate-{attempt_index}.patch")),
                )
                .expect("patch export");

            // Write scope is checked on the actual diff; a violation can
            // never be accepted (SPEC §8, §9).
            match workspace::check_scope(&worktree, self.config.contract) {
                Ok(_) => {}
                Err(WorkspaceError::ScopeViolation(paths)) => {
                    ledger
                        .finish_attempt(
                            attempt_id,
                            State::NeedsDecision,
                            None,
                            Some(&candidate_sha),
                        )
                        .expect("attempt");
                    let detail =
                        format!("the diff leaves the contract scope: {}", paths.join(", "));
                    self.transition(
                        State::NeedsDecision,
                        reason::SCOPE_EXCEEDED,
                        serde_json::json!({ "paths": paths }),
                    );
                    return self.outcome_for(RunOutcome::NeedsDecision {
                        run_id: self.run_id.clone(),
                        reason: reason::SCOPE_EXCEEDED.into(),
                        detail,
                    });
                }
                Err(e) => {
                    return self.fail_preflight("scope_check_failed", e.to_string());
                }
            }

            // Verification against an immutable copy of the candidate
            // (SPEC §10).
            self.state = State::Verifying;
            let (checks, gaps) = match self.verify_candidate(
                &worktree,
                &candidate_sha,
                &authority,
                &logs_dir,
                attempt_index,
            ) {
                Ok(result) => result,
                Err(e) => {
                    return self.fail_preflight("verification_unavailable", e.to_string());
                }
            };
            let failures: Vec<String> = checks
                .iter()
                .filter(|check| check.failed())
                .map(|check| check.label.clone())
                .collect();

            if !gaps.is_empty() {
                let detail = format!("required checks are gaps, not passes: {}", gaps.join("; "));
                self.transition(
                    State::NeedsDecision,
                    reason::VERIFICATION_GAP,
                    serde_json::json!({ "gaps": gaps }),
                );
                return self.outcome_for(RunOutcome::NeedsDecision {
                    run_id: self.run_id.clone(),
                    reason: reason::VERIFICATION_GAP.into(),
                    detail,
                });
            }

            if !failures.is_empty() {
                // Same failure on an unchanged candidate: escalate or fail
                // immediately, do not buy another no-op (SPEC §9).
                let unchanged = last_candidate.as_deref() == Some(candidate_sha.as_str());
                last_candidate = Some(candidate_sha.clone());
                if unchanged && last_failures.as_deref() == Some(failures.as_slice()) {
                    let detail = format!(
                        "same failure on an unchanged candidate: {}; failing immediately",
                        failures.join(", ")
                    );
                    self.transition(
                        State::Failed,
                        reason::SAME_FAILURE_RECURRENCE,
                        serde_json::json!({ "failures": failures }),
                    );
                    return self.outcome_for(RunOutcome::Failed {
                        run_id: self.run_id.clone(),
                        detail,
                    });
                }
                last_failures = Some(failures.clone());

                // A repair with a localized correction, when allowance
                // remains (SPEC §9). Pre-existing failures are chased too:
                // a task may exist precisely to fix them, and acceptance
                // still requires green checks or an explicit waiver.
                if repairs_used < authority.max_repairs_before_escalation {
                    repairs_used += 1;
                    kind = AttemptKind::Repair;
                    self.transition(
                        State::Repairing,
                        reason::BEHAVIORAL_FAILURE,
                        serde_json::json!({ "failures": failures, "attempt": attempt_index }),
                    );
                    continue;
                }
                // Then a stronger profile, when authorized and budget
                // permits (SPEC §9).
                if let Some(escalation_tier) = decision.escalation_tier {
                    if escalation_tier > tier {
                        kind = AttemptKind::Escalation;
                        tier = escalation_tier;
                        self.transition(
                            State::Escalating,
                            reason::REPAIR_EXHAUSTED,
                            serde_json::json!({ "failures": failures, "to_tier": tier.as_str() }),
                        );
                        continue;
                    }
                }
                // Terminal: existing failures are not automatically
                // waived — the waiver must already be in policy or become
                // an explicit contract revision (SPEC §10).
                let all_preexisting = failures
                    .iter()
                    .all(|label| baseline_failures.contains(label));
                if all_preexisting {
                    let detail = format!(
                        "these checks also fail at the base and are not waived: {}",
                        failures.join(", ")
                    );
                    self.transition(
                        State::NeedsDecision,
                        reason::BASELINE_FAILURE_NOT_WAIVED,
                        serde_json::json!({ "failures": failures }),
                    );
                    return self.outcome_for(RunOutcome::NeedsDecision {
                        run_id: self.run_id.clone(),
                        reason: reason::BASELINE_FAILURE_NOT_WAIVED.into(),
                        detail,
                    });
                }
                let detail = format!(
                    "verification failed and no repair or escalation remains: {}",
                    failures.join(", ")
                );
                self.transition(
                    State::Failed,
                    reason::REPAIR_EXHAUSTED,
                    serde_json::json!({ "failures": failures }),
                );
                return self.outcome_for(RunOutcome::Failed {
                    run_id: self.run_id.clone(),
                    detail,
                });
            }

            // Checks pass. Semantic review is risk-dependent (SPEC §10).
            if decision.review >= Review::Required {
                match self.review_candidate(&manifest, &decision, &authority, &candidate_sha) {
                    ReviewOutcome::Findings(detail) => {
                        self.transition(
                            State::NeedsReview,
                            reason::REVIEW_FINDINGS,
                            serde_json::json!({ "detail": detail }),
                        );
                        return self.outcome_for(RunOutcome::NeedsReview {
                            run_id: self.run_id.clone(),
                            detail,
                        });
                    }
                    ReviewOutcome::NoFindings => {}
                    ReviewOutcome::Unavailable(detail) => {
                        self.transition(
                            State::NeedsReview,
                            reason::REVIEW_UNAVAILABLE,
                            serde_json::json!({ "detail": detail }),
                        );
                        return self.outcome_for(RunOutcome::NeedsReview {
                            run_id: self.run_id.clone(),
                            detail,
                        });
                    }
                }
            }

            // Accepted: a receipt bound to this candidate (SPEC §10, §12).
            let report = VerificationReport {
                candidate_sha: candidate_sha.clone(),
                base_sha: base_sha.clone(),
                contract_hash: contract_hash.clone(),
                policy_hash: authority.authority_hash.clone(),
                checks,
                gaps,
                baseline_failures,
                amont_bypasses: Vec::new(),
                amont_downgrades: Vec::new(),
            };
            let receipt = Receipt {
                run_id: self.run_id.clone(),
                candidate_sha: candidate_sha.clone(),
                base_sha,
                contract_hash,
                policy_hash: authority.authority_hash,
                outcome: State::Accepted.as_str().to_string(),
                verification: report,
                models_used,
                attempts: attempt_index,
                cost_completeness,
                cost: total_cost,
            };
            let receipt_hash = receipt.hash();
            ledger
                .store_receipt(
                    &self.run_id,
                    &serde_json::to_value(&receipt).expect("serializes"),
                    &receipt_hash,
                )
                .expect("receipt stored");
            std::fs::write(
                self.artifacts.join("receipt.json"),
                serde_json::to_string_pretty(&receipt).expect("serializes"),
            )
            .expect("receipt artifact");
            ledger
                .finish_attempt(
                    attempt_id,
                    State::Accepted,
                    Some(worktree_path.to_string_lossy().as_ref()),
                    Some(&candidate_sha),
                )
                .expect("attempt");
            self.transition(
                State::Accepted,
                reason::CHECKS_AND_REVIEW_PASSED,
                serde_json::json!({
                    "candidate": candidate_sha,
                    "receipt_hash": receipt_hash,
                    "attempts": attempt_index,
                }),
            );
            return self.outcome_for(RunOutcome::Accepted {
                run_id: self.run_id.clone(),
                receipt: Box::new(receipt),
            });
        }
    }

    /// Verify the immutable candidate copy: a throwaway worktree at the
    /// candidate SHA, profile commands with logged, hashed evidence, and
    /// amont-derived gaps when the profile names required checks.
    fn verify_candidate(
        &self,
        _worktree: &TaskWorktree,
        candidate_sha: &str,
        authority: &EffectiveAuthority,
        logs_dir: &Path,
        attempt_index: u32,
    ) -> Result<(Vec<verify::CheckOutcome>, Vec<String>), String> {
        let verify_path = self.artifacts.join(format!("verify-{attempt_index}"));
        let holder =
            verify::verification_worktree(self.config.repo_dir, candidate_sha, &verify_path)
                .map_err(|e| e.to_string())?;
        let checks = verify::run_profile(
            holder.path(),
            &authority.verification_profile,
            logs_dir,
            &format!("attempt{attempt_index}"),
        )
        .map_err(|e| e.to_string())?;
        let required = &authority.verification_profile.amont_checks;
        let gaps = if required.is_empty() {
            Vec::new()
        } else {
            let inventory = verify::amont_list(self.config.repo_dir, None, false);
            amont_gaps(inventory.as_ref(), required)
        };
        Ok((checks, gaps))
    }

    /// One separate review call per candidate requiring it (SPEC §9, §10).
    /// The reviewer cannot edit or waive anything; findings are triaged,
    /// and "no findings" is recorded as evidence, not proof.
    fn review_candidate(
        &self,
        manifest: &ContextManifest,
        decision: &RouteDecision,
        authority: &EffectiveAuthority,
        candidate_sha: &str,
    ) -> ReviewOutcome {
        let reviewer_tier = Tier::Escalation;
        let Some(profile) = authority.models.get(&reviewer_tier) else {
            return ReviewOutcome::Unavailable(
                "no reviewer model configured at the escalation tier".into(),
            );
        };
        let _ = decision;
        let patch_path = self.artifacts.join("candidate-latest.patch");
        let mut prompt = String::from(
            "You are a semantic reviewer. You cannot edit or waive checks; you report findings only.\n\
             Report each finding with file/range, the violated acceptance criterion, evidence, and suggested verification.\n\
             If there are no findings, end with the exact line: FINDINGS: none\n",
        );
        prompt.push_str(&format!(
            "\nobjective: {}\n",
            self.config.contract.objective
        ));
        prompt.push_str("acceptance criteria:\n");
        for criterion in &self.config.contract.acceptance {
            prompt.push_str(&format!("  - {criterion}\n"));
        }
        if !manifest.constraints.is_empty() {
            prompt.push_str("architectural constraints:\n");
            for constraint in &manifest.constraints {
                prompt.push_str(&format!("  - {constraint}\n"));
            }
        }
        prompt.push_str(&format!("\ncandidate commit: {candidate_sha}\n"));
        prompt.push_str(&format!(
            "candidate patch (read it): {}\n",
            patch_path.display()
        ));
        prompt.push_str(&format!(
            "source to inspect: {}\n",
            self.artifacts.join("worktree").display()
        ));

        let dispatch_id = DispatchId::generate();
        let spec = LaunchSpec {
            dispatch_id: dispatch_id.as_str().to_string(),
            prompt,
            model: profile.id.clone(),
            effort: profile.effort,
            max_turns: None,
            budget_micros: None,
            disallowed_tools: authority.disallowed_tools.clone(),
            work_dir: self.artifacts.join("worktree"),
            wall_timeout: Duration::from_secs(authority.max_wall_seconds),
        };
        let result = match self.config.backend.launch(&spec) {
            Ok(result) => result,
            Err(e) => return ReviewOutcome::Unavailable(e.to_string()),
        };
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
            cost: result.usage.cost.unwrap_or(MicroUsd::ZERO),
            cost_kind: CostKind::ApiSpend,
            completeness: result.usage.cost_completeness,
            inclusive: result.usage.inclusive,
            at: now_rfc3339(),
        };
        self.config
            .ledger
            .record_usage(&event)
            .expect("review usage recorded");
        if result.terminal_result_missing() {
            return ReviewOutcome::Unavailable(
                "the reviewer ended without a terminal result".into(),
            );
        }
        let text = result.result_text.unwrap_or_default();
        let _ = std::fs::write(self.artifacts.join("review.txt"), &text);
        if text
            .lines()
            .rev()
            .take(5)
            .any(|line| line.trim().eq_ignore_ascii_case("findings: none"))
        {
            ReviewOutcome::NoFindings
        } else if text.contains("FINDINGS") {
            ReviewOutcome::Findings(text)
        } else {
            ReviewOutcome::Findings(format!("reviewer output without a clear verdict:\n{text}"))
        }
    }
}

enum ReviewOutcome {
    NoFindings,
    Findings(String),
    Unavailable(String),
}

fn build_prompt(
    contract: &TaskContract,
    manifest: &ContextManifest,
    decision: &RouteDecision,
    previous_failures: Option<&[String]>,
    kind: AttemptKind,
) -> String {
    let mut prompt = String::from("[relais task]\n");
    prompt.push_str(&format!("objective: {}\n", contract.objective));
    prompt.push_str("acceptance criteria (verification decides, not you):\n");
    for criterion in &contract.acceptance {
        prompt.push_str(&format!("  - {criterion}\n"));
    }
    if !manifest.constraints.is_empty() {
        prompt.push_str("architectural constraints (in force):\n");
        for constraint in &manifest.constraints {
            prompt.push_str(&format!("  - {constraint}\n"));
        }
    }
    if let Some(scope) = contract.write_scope.as_deref() {
        prompt.push_str(&format!(
            "write scope (stay within): {}\n",
            scope.join(", ")
        ));
    }
    if !contract.read_hints.is_empty() {
        prompt.push_str(&format!(
            "entry points: {}\n",
            contract.read_hints.join(", ")
        ));
    }
    prompt.push_str(&format!(
        "verification profile: {} ({} command(s) judge the result)\n",
        contract.verification_profile, decision.max_attempts
    ));
    prompt.push_str(
        "\nrules: you cannot commit, merge, push or publish; do not modify policy,\n\
         verification commands or fixtures; work only in this directory.\n\
         finish with a line starting DONE when you believe the criteria are met,\n\
         or relais-blocked: <reason> when something outside the task blocks you.\n",
    );
    if let Some(failures) = previous_failures {
        match kind {
            AttemptKind::Repair => {
                prompt.push_str("\n[repair addendum]\n");
                prompt.push_str("the previous candidate failed verification on these checks:\n");
                for failure in failures {
                    prompt.push_str(&format!("  - {failure} (log under logs/)\n"));
                }
                prompt.push_str("fix only the failures; do not rewrite unrelated work.\n");
            }
            AttemptKind::Escalation => {
                prompt.push_str("\n[escalation addendum]\n");
                prompt.push_str(
                    "a previous attempt exists. Its claims are UNVERIFIED evidence, not facts.\n\
                     the same checks failed:\n",
                );
                for failure in failures {
                    prompt.push_str(&format!("  - {failure}\n"));
                }
                prompt.push_str(
                    "judge from the contract and the evidence; acceptance criteria are unchanged.\n",
                );
            }
            AttemptKind::Initial => {}
        }
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{MockBackend, MockOutcome};
    use crate::policy::{
        CommandSpec, ConcurrencyLimits, Dependency, DependencyMode, ExecutionPolicy, ModelProfile,
        RiskRule, TrialEnvelope, VerificationPolicy, VerificationProfile,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    // -- fixtures ---------------------------------------------------------

    struct Fixture {
        dir: PathBuf,
        repo: PathBuf,
        artifacts: PathBuf,
        ledger: Ledger,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "relais-run-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let repo = dir.join("repo");
            std::fs::create_dir_all(&repo).expect("mkdir");
            let no_hooks = dir.join("no-hooks");
            std::fs::create_dir_all(&no_hooks).expect("mkdir");
            git(&repo, &["init", "-q"]);
            git(
                &repo,
                &["config", "core.hooksPath", &no_hooks.to_string_lossy()],
            );
            git(&repo, &["config", "user.email", "t@t"]);
            git(&repo, &["config", "user.name", "t"]);
            std::fs::create_dir_all(repo.join("src")).expect("mkdir");
            std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").expect("write");
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", "base"]);
            let artifacts = dir.join("runs");
            std::fs::create_dir_all(&artifacts).expect("mkdir");
            let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
            Self {
                dir,
                repo,
                artifacts,
                ledger,
            }
        }

        fn contract(&self, review: Review) -> TaskContract {
            TaskContract::from_json_str(
                &serde_json::json!({
                    "schema_version": 1,
                    "kind": "change",
                    "objective": "Remove the obsolete entry point",
                    "base_ref": "HEAD",
                    "write_scope": ["src/**"],
                    "acceptance": ["src/main.rs no longer exists"],
                    "verification_profile": "profile",
                    "review": review,
                })
                .to_string(),
            )
            .expect("contract")
        }

        fn contract_with_scope(&self, scope: &[&str]) -> TaskContract {
            TaskContract::from_json_str(
                &serde_json::json!({
                    "schema_version": 1,
                    "kind": "change",
                    "objective": "Touch the trust area",
                    "base_ref": "HEAD",
                    "write_scope": scope,
                    "acceptance": ["the check passes"],
                    "verification_profile": "profile",
                })
                .to_string(),
            )
            .expect("contract")
        }

        fn repo_policy(&self, commands: Vec<CommandSpec>, max_attempts: u32) -> RepoPolicy {
            RepoPolicy {
                schema_version: 1,
                models: BTreeMap::from([
                    (
                        Tier::Research,
                        ModelProfile {
                            id: "haiku".into(),
                            effort: None,
                        },
                    ),
                    (
                        Tier::Implementation,
                        ModelProfile {
                            id: "sonnet".into(),
                            effort: None,
                        },
                    ),
                    (
                        Tier::Escalation,
                        ModelProfile {
                            id: "fable".into(),
                            effort: None,
                        },
                    ),
                ]),
                execution: ExecutionPolicy {
                    max_attempts,
                    max_repairs_before_escalation: 1,
                    max_wall_seconds: 1200,
                    allow_nested_agents: false,
                    max_agent_depth: 1,
                    max_agents_total: 24,
                },
                integrations: Default::default(),
                verification: VerificationPolicy {
                    profiles: BTreeMap::from([(
                        "profile".into(),
                        VerificationProfile {
                            commands,
                            amont_checks: Vec::new(),
                        },
                    )]),
                },
                risk: Vec::new(),
                architecture: Default::default(),
                recipes: Vec::new(),
            }
        }

        fn machine_for(&self, repo: &RepoPolicy) -> MachineSettings {
            let mut trust = BTreeMap::new();
            trust.insert(
                repo.authority_hash(),
                crate::policy::TrustGrant {
                    granted_at: "2026-09-18".into(),
                    reviewed_by: None,
                    note: None,
                },
            );
            MachineSettings {
                schema_version: 1,
                allowed_models: None,
                spending: Default::default(),
                trust,
                permissions: Default::default(),
                concurrency: ConcurrencyLimits::default(),
                trials: TrialEnvelope::default(),
                routing: Default::default(),
            }
        }

        fn execute(
            &self,
            contract: &TaskContract,
            repo: &RepoPolicy,
            backend: &dyn Backend,
        ) -> RunOutcome {
            let machine = self.machine_for(repo);
            let resolver = |_: &str, _: Option<&str>| AvalVerdict::Active {
                adr: "ADR-0001".into(),
            };
            execute(&RunConfig {
                repo_dir: &self.repo,
                contract,
                repo_policy: repo,
                machine: &machine,
                ledger: &self.ledger,
                backend,
                artifacts_dir: self.artifacts.clone(),
                aval_resolver: &resolver,
                predictor: None,
            })
        }

        fn execute_with_machine(
            &self,
            contract: &TaskContract,
            repo: &RepoPolicy,
            machine: &MachineSettings,
            backend: &dyn Backend,
        ) -> RunOutcome {
            let resolver = |_: &str, _: Option<&str>| AvalVerdict::Active {
                adr: "ADR-0001".into(),
            };
            execute(&RunConfig {
                repo_dir: &self.repo,
                contract,
                repo_policy: repo,
                machine,
                ledger: &self.ledger,
                backend,
                artifacts_dir: self.artifacts.clone(),
                aval_resolver: &resolver,
                predictor: None,
            })
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn count_tick_files(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter(|entry| {
                        entry
                            .as_ref()
                            .map(|entry| entry.file_name().to_string_lossy().starts_with("tick-"))
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// The profile command that decides acceptance: green only once
    /// src/main.rs is gone. It fails at the base on purpose — this is a
    /// fix-the-failing-state task (SPEC §10: baseline failures are
    /// visible from preflight and the candidate must make them green).
    fn main_gone_check() -> CommandSpec {
        CommandSpec {
            argv: vec!["sh".into(), "-c".into(), "test ! -f src/main.rs".into()],
            timeout_seconds: 30,
        }
    }

    fn passing_check() -> CommandSpec {
        CommandSpec {
            argv: vec!["sh".into(), "-c".into(), "true".into()],
            timeout_seconds: 30,
        }
    }

    fn usage(cost_micros: i64) -> crate::adapter::UsageReport {
        crate::adapter::UsageReport {
            input_tokens: Some(100),
            output_tokens: Some(10),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost: Some(MicroUsd::from_micros(cost_micros)),
            cost_completeness: CostCompleteness::Actual,
            inclusive: false,
        }
    }

    /// A worker that completes (deletes src/main.rs) when — and only when
    /// — its prompt contains `signal`. Review prompts answer FINDINGS:
    /// none.
    fn conditional_worker(signal: &str) -> MockBackend {
        let signal = signal.to_string();
        MockBackend::new(move |spec| {
            if spec.prompt.contains("semantic reviewer") {
                return MockOutcome {
                    result_text: Some("review ok\nFINDINGS: none".into()),
                    exit_code: Some(0),
                    ..Default::default()
                };
            }
            let complete = !signal.is_empty() && spec.prompt.contains(&signal);
            if complete {
                std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("worker completes");
            }
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                usage: Some(usage(100)),
                ..Default::default()
            }
        })
    }

    /// A worker that changes the worktree every attempt (so candidates
    /// differ) without ever completing.
    fn churning_worker(extra_check: &'static str) -> MockBackend {
        MockBackend::new(move |spec| {
            if spec.prompt.contains("semantic reviewer") {
                return MockOutcome {
                    result_text: Some("FINDINGS: none".into()),
                    exit_code: Some(0),
                    ..Default::default()
                };
            }
            let src = spec.work_dir.join("src");
            let existing = count_tick_files(&src);
            std::fs::write(src.join(format!("tick-{existing}.txt")), "churn\n").expect("write");
            let _ = extra_check;
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                usage: Some(usage(100)),
                ..Default::default()
            }
        })
    }

    // -- release scenarios (SPEC §14) -------------------------------------

    #[test]
    fn bounded_change_routes_verifies_and_receives_a_bound_receipt() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let backend = conditional_worker("relais task");
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::Accepted { run_id, receipt } = outcome else {
            panic!("expected acceptance, got {outcome:?}");
        };
        assert_eq!(receipt.attempts, 1);
        assert_eq!(receipt.models_used, vec!["sonnet".to_string()]);
        assert_eq!(receipt.cost, MicroUsd::from_micros(100));
        assert_eq!(
            fixture.ledger.run_cost(&run_id).expect("cost"),
            MicroUsd::from_micros(100),
            "all recorded cost, one task"
        );
        let (stored, _) = fixture
            .ledger
            .receipt(&run_id)
            .expect("receipt")
            .expect("present");
        assert_eq!(stored["outcome"], "accepted");
        let worktree = fixture.artifacts.join(&run_id).join("worktree");
        assert!(
            !worktree.join("src/main.rs").exists(),
            "the candidate is the accepted state"
        );
        assert!(worktree.is_dir(), "the worktree is retained");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn failed_implementation_gets_repair_then_escalation_all_attributed() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        // No-op on the initial attempt, changes something on repair
        // (candidates must differ), completes on escalation.
        let backend = MockBackend::new(|spec| {
            if spec.prompt.contains("semantic reviewer") {
                return MockOutcome {
                    result_text: Some("FINDINGS: none".into()),
                    exit_code: Some(0),
                    ..Default::default()
                };
            }
            if spec.prompt.contains("escalation addendum") {
                std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("remove");
            } else if spec.prompt.contains("repair addendum") {
                std::fs::write(spec.work_dir.join("src/notes.txt"), "investigation\n")
                    .expect("write");
            }
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                usage: Some(usage(100)),
                ..Default::default()
            }
        });
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::Accepted { run_id, receipt } = outcome else {
            panic!("expected acceptance, got {outcome:?}");
        };
        assert_eq!(
            receipt.attempts, 3,
            "initial, one repair, one stronger attempt"
        );
        assert_eq!(
            receipt.models_used,
            vec!["sonnet".to_string(), "fable".to_string()],
            "the stronger profile is recorded"
        );
        assert_eq!(
            fixture.ledger.run_cost(&run_id).expect("cost"),
            MicroUsd::from_micros(300),
            "every attempt's cost is attributed to the one task"
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn repair_recovers_a_failed_attempt() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let backend = conditional_worker("repair addendum");
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::Accepted { receipt, .. } = outcome else {
            panic!("expected acceptance, got {outcome:?}");
        };
        assert_eq!(receipt.attempts, 2);
        assert_eq!(receipt.models_used, vec!["sonnet".to_string()]);
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn same_failure_on_unchanged_candidate_fails_immediately() {
        let fixture = Fixture::new();
        // A check green at the base, red once the worker's change lands —
        // and the worker never makes a second change.
        let no_evil = CommandSpec {
            argv: vec!["sh".into(), "-c".into(), "test ! -f src/evil.txt".into()],
            timeout_seconds: 30,
        };
        let repo = fixture.repo_policy(vec![no_evil], 3);
        let backend = MockBackend::new(|spec| {
            if spec.prompt.contains("semantic reviewer") {
                return MockOutcome {
                    result_text: Some("FINDINGS: none".into()),
                    exit_code: Some(0),
                    ..Default::default()
                };
            }
            let evil = spec.work_dir.join("src/evil.txt");
            if !evil.exists() {
                std::fs::write(&evil, "once\n").expect("write once");
            }
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                usage: Some(usage(100)),
                ..Default::default()
            }
        });
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::Failed { detail, .. } = &outcome else {
            panic!("expected failure, got {outcome:?}");
        };
        assert!(detail.contains("unchanged candidate"), "{detail}");
        let transitions = fixture
            .ledger
            .transitions(outcome.run_id())
            .expect("history");
        assert_eq!(transitions.last().unwrap().to_state, State::Failed);
        assert_eq!(
            transitions.last().unwrap().reason,
            reason::SAME_FAILURE_RECURRENCE
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn worker_blockage_is_blocked_not_escalated() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let backend = MockBackend::new(|_| MockOutcome {
            result_text: Some("relais-blocked: required dependency missing".into()),
            exit_code: Some(0),
            ..Default::default()
        });
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        assert!(matches!(outcome, RunOutcome::Blocked { .. }), "{outcome:?}");
        let transitions = fixture
            .ledger
            .transitions(outcome.run_id())
            .expect("history");
        assert_eq!(
            transitions.len(),
            1,
            "no escalation is bought for the environment"
        );
        assert_eq!(transitions[0].to_state, State::Blocked);
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn missing_terminal_result_is_interrupted_with_worktree_preserved() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let backend = MockBackend::new(|_| MockOutcome {
            result_text: None,
            timed_out: true,
            ..Default::default()
        });
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::Interrupted { run_id, .. } = outcome else {
            panic!("expected interrupted, got {outcome:?}");
        };
        let worktree = fixture.artifacts.join(&run_id).join("worktree");
        assert!(
            worktree.is_dir(),
            "changes are preserved for reconciliation"
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn unapproved_substitution_stops_dispatch() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let backend = MockBackend::new(|_| MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            effective_model: Some("mystery-model".into()),
            ..Default::default()
        });
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::Failed { detail, .. } = outcome else {
            panic!("expected failed, got {outcome:?}");
        };
        assert!(detail.contains("substituted"), "{detail}");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn scope_violation_cannot_be_accepted() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![passing_check()], 3);
        let backend = MockBackend::new(|spec| {
            std::fs::write(spec.work_dir.join("outside.rs"), "// out of scope\n").ok();
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                ..Default::default()
            }
        });
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::NeedsDecision {
            reason: why,
            detail,
            ..
        } = outcome
        else {
            panic!("expected needs_decision, got {outcome:?}");
        };
        assert_eq!(why, reason::SCOPE_EXCEEDED);
        assert!(detail.contains("outside.rs"), "{detail}");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn dirty_base_blocks_before_any_dispatch() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        std::fs::write(fixture.repo.join("src/main.rs"), "dirty\n").expect("dirty");
        let backend = MockBackend::new(|_| panic!("no dispatch may happen"));
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::Blocked { code, detail, .. } = outcome else {
            panic!("expected blocked, got {outcome:?}");
        };
        assert_eq!(code, "dirty_base");
        assert!(detail.contains("commit or stash"), "{detail}");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn missing_trust_grant_blocks_preflight() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let mut machine = fixture.machine_for(&repo);
        machine.trust.clear();
        let backend = MockBackend::new(|_| panic!("no dispatch may happen"));
        let outcome = fixture.execute_with_machine(
            &fixture.contract(Review::Optional),
            &repo,
            &machine,
            &backend,
        );
        let RunOutcome::Blocked { code, .. } = outcome else {
            panic!("expected blocked, got {outcome:?}");
        };
        assert_eq!(code, "missing_trust_grant");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn required_review_accepts_only_with_findings_none() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let backend = conditional_worker("relais task");
        let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
        assert!(
            matches!(outcome, RunOutcome::Accepted { .. }),
            "FINDINGS: none accepts: {outcome:?}"
        );
        let review = fixture.artifacts.join(outcome.run_id()).join("review.txt");
        assert!(review.is_file(), "review evidence is retained");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn review_findings_return_needs_review() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![main_gone_check()], 3);
        let backend = MockBackend::new(|spec| {
            if spec.prompt.contains("semantic reviewer") {
                return MockOutcome {
                    result_text: Some(
                        "FINDINGS:\n- src/main.rs: the criterion demands removal, but the deletion leaves the entry config stale\n"
                            .into(),
                    ),
                    exit_code: Some(0),
                    ..Default::default()
                };
            }
            std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("remove");
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                ..Default::default()
            }
        });
        let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
        let RunOutcome::NeedsReview { detail, .. } = outcome else {
            panic!("expected needs_review, got {outcome:?}");
        };
        assert!(detail.contains("criterion"), "{detail}");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn spend_ceiling_exhausts_the_budget_with_evidence_preserved() {
        let fixture = Fixture::new();
        // Green at the base, red forever once the worker churns.
        let no_tick0 = CommandSpec {
            argv: vec!["sh".into(), "-c".into(), "test ! -f src/tick-0.txt".into()],
            timeout_seconds: 30,
        };
        let repo = fixture.repo_policy(vec![no_tick0], 5);
        let mut machine = fixture.machine_for(&repo);
        // 150 sits between one attempt (100) and two (200): after the
        // repair attempt the run must refuse to admit more work.
        machine.spending.per_run_micros = Some(150);
        // Each attempt costs 100 micros and churns a new file: attempt 3
        // takes the run past 250 and no further dispatch is admitted.
        let backend = MockBackend::new(|spec| {
            if spec.prompt.contains("semantic reviewer") {
                return MockOutcome {
                    result_text: Some("FINDINGS: none".into()),
                    exit_code: Some(0),
                    ..Default::default()
                };
            }
            let src = spec.work_dir.join("src");
            let existing = count_tick_files(&src);
            std::fs::write(src.join(format!("tick-{existing}.txt")), "churn\n").expect("write");
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                usage: Some(usage(100)),
                ..Default::default()
            }
        });
        let contract = fixture.contract(Review::Optional);
        let outcome = fixture.execute_with_machine(&contract, &repo, &machine, &backend);
        let RunOutcome::BudgetExhausted { run_id, detail } = outcome else {
            panic!("expected budget_exhausted, got {outcome:?}");
        };
        assert!(detail.contains("ceiling"), "{detail}");
        assert_eq!(
            fixture.ledger.run_cost(&run_id).expect("cost").to_micros(),
            200,
            "two attempts settled, then admission stopped"
        );
        let run_dir = fixture.artifacts.join(run_id);
        assert!(
            run_dir.join("candidate-1.patch").is_file(),
            "patch preserved"
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn attempts_ceiling_without_allowance_fails_the_task() {
        let fixture = Fixture::new();
        let no_tick0 = CommandSpec {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "test ! -f src/trust/tick-0.txt".into(),
            ],
            timeout_seconds: 30,
        };
        let mut repo = fixture.repo_policy(vec![no_tick0], 2);
        // The risk floor puts this at the escalation tier already, so no
        // stronger tier exists beyond it: repairs exhaust, then fail.
        repo.risk.push(RiskRule {
            paths: vec!["src/trust/**".into()],
            minimum_tier: Tier::Escalation,
            review: None,
        });
        let backend = MockBackend::new(|spec| {
            if spec.prompt.contains("semantic reviewer") {
                return MockOutcome {
                    result_text: Some("FINDINGS: none".into()),
                    exit_code: Some(0),
                    ..Default::default()
                };
            }
            let trust = spec.work_dir.join("src/trust");
            std::fs::create_dir_all(&trust).expect("mkdir");
            let existing = count_tick_files(&trust);
            std::fs::write(trust.join(format!("tick-{existing}.txt")), "churn\n").expect("write");
            MockOutcome {
                result_text: Some("DONE".into()),
                exit_code: Some(0),
                usage: Some(usage(100)),
                ..Default::default()
            }
        });
        let outcome = fixture.execute(
            &fixture.contract_with_scope(&["src/trust/**"]),
            &repo,
            &backend,
        );
        let RunOutcome::Failed { detail, .. } = outcome else {
            panic!("expected failed, got {outcome:?}");
        };
        assert!(detail.contains("no repair or escalation"), "{detail}");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn baseline_failure_is_visible_and_not_waived() {
        let fixture = Fixture::new();
        // marker.txt never exists: the check fails at the base and at every
        // candidate. Churning candidates avoid the unchanged-candidate
        // shortcut so the full repair-escalation ladder runs out first.
        let marker = CommandSpec {
            argv: vec!["sh".into(), "-c".into(), "test -f src/marker.txt".into()],
            timeout_seconds: 30,
        };
        let repo = fixture.repo_policy(vec![marker], 3);
        let backend = churning_worker("");
        let outcome = fixture.execute(&fixture.contract(Review::Optional), &repo, &backend);
        let RunOutcome::NeedsDecision {
            reason: why,
            detail,
            ..
        } = outcome
        else {
            panic!("expected needs_decision, got {outcome:?}");
        };
        assert_eq!(why, reason::BASELINE_FAILURE_NOT_WAIVED, "{detail}");
        assert!(detail.contains("not waived"), "{detail}");
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn inspect_contract_runs_the_research_tier() {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![passing_check()], 3);
        let backend = MockBackend::new(|_| {
            MockOutcome {
            result_text: Some(
                "DONE: the check is inert because its declaration is untrusted. Evidence: amont trust --show"
                    .into(),
            ),
            exit_code: Some(0),
            usage: Some(usage(50)),
            ..Default::default()
        }
        });
        let contract = TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1,
                "kind": "inspect",
                "objective": "Investigate why a check is inactive",
                "base_ref": "HEAD",
                "acceptance": ["evidence of why the check is inert"],
                "verification_profile": "profile",
            })
            .to_string(),
        )
        .expect("contract");
        let outcome = fixture.execute(&contract, &repo, &backend);
        let RunOutcome::Accepted { receipt, .. } = outcome else {
            panic!("inspect tasks accept on evidence, got {outcome:?}");
        };
        assert_eq!(
            receipt.models_used,
            vec!["haiku".to_string()],
            "research tier"
        );
        std::fs::remove_dir_all(&fixture.dir).ok();
    }

    #[test]
    fn dependency_enum_used() {
        assert_eq!(
            Dependency::Mode(DependencyMode::Optional).mode(),
            DependencyMode::Optional
        );
    }
}
