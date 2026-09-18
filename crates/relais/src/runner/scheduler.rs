//! Bounded decomposition (SPEC §19).
//!
//! A work plan — explicit in the contract, or proposed by a bounded
//! planner call and then validated deterministically — becomes a
//! dependency graph of work packages. Each package is a run of its own
//! (same lifecycle, same acceptance boundary, its own owned worktree at
//! its declared input revision) attributed to the root run. The
//! scheduler assembles completed candidates into one integration
//! worktree and verifies the integrated revision independently:
//! per-package receipts never constitute final acceptance. Aggregate
//! limits — wall clock, money, attempts, the agent cap — bound the whole
//! run; no package resets them, and integration failures spend from the
//! same budget with at most one integration repair.
//!
//! Packages run in topological order, one at a time. Waves of
//! independent packages are computed and recorded so a concurrent
//! executor can be dropped in behind the same validation and assembly,
//! but this executor is sequential: an integrated candidate is built by
//! fast-forward, and every package sees the packages before it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::adapter::LaunchSpec;
use crate::context::ContextManifest;
use crate::contract::{Decomposition, DecompositionMode, Kind, TaskContract, WorkPlan};
use crate::contract::{Review, WorkPackage};
use crate::ids::DispatchId;
use crate::ledger::{now_rfc3339, UsageEvent};
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::policy::{EffectiveAuthority, MachineSettings, Tier};
use crate::route::RouteDecision;
use crate::verify::{Receipt, VerificationReport};
use crate::workspace::{self, WorkspaceError};

use super::{
    execute_child, reason, worst_completeness, ReviewOutcome, RunConfig, RunEngine, RunOutcome,
    State,
};

/// Everything the root preflight established that the packages inherit.
pub(crate) struct RootContext<'a> {
    pub authority: &'a EffectiveAuthority,
    pub base_sha: &'a str,
    pub contract_hash: &'a str,
    pub manifest: &'a ContextManifest,
    pub decision: &'a RouteDecision,
    pub baseline_failures: &'a [String],
    pub logs_dir: &'a Path,
    pub deadline: Instant,
}

pub(crate) enum Decomposed {
    Outcome(RunOutcome),
    /// The planner answered "one worker is right"; the ordinary path
    /// continues.
    SingleWorker,
}

/// Coverage and limits against the effective authority: everything the
/// contract-level shape check cannot know. Every problem is reported,
/// not just the first, because the plan goes back to a human.
pub fn check_plan(
    plan: &WorkPlan,
    contract: &TaskContract,
    authority: &EffectiveAuthority,
) -> Vec<String> {
    let mut problems = Vec::new();
    let order = match plan.validate() {
        Ok(order) => order,
        Err(e) => return vec![e.to_string()],
    };
    let root_scope: Vec<&str> = contract
        .write_scope
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect();
    for package in &plan.packages {
        for pattern in &package.write_scope {
            if !root_scope.iter().any(|root| covers(root, pattern)) {
                problems.push(format!(
                    "package `{}` scope `{pattern}` is not within the contract scope {root_scope:?}",
                    package.id
                ));
            }
        }
    }
    // Independent packages (no dependency path either way) must not
    // overlap: overlapping writes are serialized by an explicit
    // dependency, never reconciled by guesswork.
    for (i, a) in plan.packages.iter().enumerate() {
        for b in plan.packages.iter().skip(i + 1) {
            if depends(plan, &a.id, &b.id) || depends(plan, &b.id, &a.id) {
                continue;
            }
            for pa in &a.write_scope {
                for pb in &b.write_scope {
                    if overlaps(pa, pb) {
                        problems.push(format!(
                            "independent packages `{}` and `{}` overlap on `{pa}` / `{pb}`; add a dependency to serialize them",
                            a.id, b.id
                        ));
                    }
                }
            }
        }
    }
    if plan.limits.attempts_per_package > authority.max_attempts {
        problems.push(format!(
            "attempts_per_package {} exceeds the run's attempt ceiling {}",
            plan.limits.attempts_per_package, authority.max_attempts
        ));
    }
    let worst_case = plan.packages.len() as u32 * plan.limits.attempts_per_package;
    if worst_case > authority.max_agents_total {
        problems.push(format!(
            "{} packages x {} attempts = {worst_case} dispatches exceeds the aggregate agent cap {}",
            plan.packages.len(),
            plan.limits.attempts_per_package,
            authority.max_agents_total
        ));
    }
    let _ = order;
    problems
}

/// Is there a dependency path from `from` to `to`?
fn depends(plan: &WorkPlan, from: &str, to: &str) -> bool {
    let mut stack = vec![from.to_string()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(current) = stack.pop() {
        if !seen.insert(current.clone()) {
            continue;
        }
        if let Some(package) = plan.packages.iter().find(|p| p.id == current) {
            for dependency in &package.depends_on {
                if dependency == to {
                    return true;
                }
                stack.push(dependency.clone());
            }
        }
    }
    false
}

/// Does the root pattern cover the package pattern? Exact, or a `/**`
/// root whose prefix the package pattern starts with. Deliberately
/// conservative: the actual diff is still checked against both scopes.
pub fn covers(root: &str, pattern: &str) -> bool {
    if root == pattern {
        return true;
    }
    match root.strip_suffix("/**").or_else(|| root.strip_suffix("**")) {
        Some(prefix) => {
            let prefix = prefix.trim_end_matches('/');
            prefix.is_empty() || pattern == prefix || pattern.starts_with(&format!("{prefix}/"))
        }
        None => false,
    }
}

/// Could two patterns match the same path? Judged on their literal
/// prefixes before the first glob character; a shared prefix (either
/// way) is an overlap. Conservative by design.
pub fn overlaps(a: &str, b: &str) -> bool {
    let literal = |pattern: &str| {
        pattern
            .split(['*', '?', '['])
            .next()
            .unwrap_or("")
            .to_string()
    };
    let (la, lb) = (literal(a), literal(b));
    la.starts_with(&lb) || lb.starts_with(&la)
}

pub(crate) fn run_decomposed(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    decomposition: &Decomposition,
) -> Decomposed {
    let contract = engine.config.contract;
    if contract.kind != Kind::Change {
        return Decomposed::Outcome(engine.fail_preflight(
            "decomposition_kind",
            "only kind=change can be decomposed".into(),
        ));
    }
    let plan = match decomposition {
        Decomposition::Plan(plan) => plan.clone(),
        Decomposition::Mode(DecompositionMode::Propose) => match propose_plan(engine, root) {
            Proposal::Plan(plan) => plan,
            Proposal::SingleWorker => return Decomposed::SingleWorker,
            Proposal::Rejected(detail) => {
                engine.transition(
                    State::NeedsDecision,
                    reason::PLAN_REJECTED,
                    serde_json::json!({ "detail": detail }),
                );
                return Decomposed::Outcome(engine.outcome_for(RunOutcome::NeedsDecision {
                    run_id: engine.run_id.clone(),
                    reason: reason::PLAN_REJECTED.into(),
                    detail,
                }));
            }
            Proposal::Failed(outcome) => return Decomposed::Outcome(outcome),
        },
    };
    let problems = check_plan(&plan, contract, root.authority);
    if !problems.is_empty() {
        let detail = format!("the work plan was rejected: {}", problems.join("; "));
        engine.transition(
            State::NeedsDecision,
            reason::PLAN_REJECTED,
            serde_json::json!({ "problems": problems }),
        );
        return Decomposed::Outcome(engine.outcome_for(RunOutcome::NeedsDecision {
            run_id: engine.run_id.clone(),
            reason: reason::PLAN_REJECTED.into(),
            detail,
        }));
    }
    let order = plan.validate().expect("checked above");
    let _ = std::fs::write(
        engine.artifacts.join("plan.json"),
        serde_json::to_string_pretty(&plan).expect("serializes"),
    );
    engine.transition(
        State::Running,
        reason::PLAN_ACCEPTED,
        serde_json::json!({
            "packages": order.iter().map(|(index, wave)| {
                serde_json::json!({ "id": plan.packages[*index].id, "wave": wave })
            }).collect::<Vec<_>>(),
        }),
    );

    // The integration worktree: every accepted package fast-forwards it.
    let integration_path = engine.artifacts.join("integration");
    let integration = match workspace::create_worktree(
        engine.config.repo_dir,
        root.base_sha,
        &integration_path,
    ) {
        Ok(worktree) => worktree,
        Err(e) => {
            return Decomposed::Outcome(
                engine.fail_preflight("worktree_unavailable", e.to_string()),
            )
        }
    };
    let mut head = root.base_sha.to_string();
    let mut attempts_total: u32 = 0;
    let mut models_used: Vec<String> = Vec::new();

    for (index, wave) in &order {
        let package = &plan.packages[*index];
        let outcome = run_package(
            engine,
            root,
            &plan,
            package,
            *wave,
            &head,
            &mut attempts_total,
            &mut models_used,
        );
        match outcome {
            PackageEnd::Accepted(candidate) => match fast_forward(&integration_path, &candidate) {
                Ok(new_head) => head = new_head,
                Err(detail) => {
                    engine.transition(
                        State::NeedsDecision,
                        reason::INTEGRATION_CONFLICT,
                        serde_json::json!({ "package": package.id, "detail": detail }),
                    );
                    return Decomposed::Outcome(engine.outcome_for(RunOutcome::NeedsDecision {
                        run_id: engine.run_id.clone(),
                        reason: reason::INTEGRATION_CONFLICT.into(),
                        detail: format!(
                            "package `{}` cannot be integrated: {detail}; its candidate is preserved",
                            package.id
                        ),
                    }));
                }
            },
            PackageEnd::Stop(outcome) => return Decomposed::Outcome(outcome),
        }
    }

    // Independent final verification of the assembled candidate (SPEC
    // §19), against the ROOT contract's scope and profile.
    let mut integration_repairs: u32 = 0;
    loop {
        match workspace::check_scope(&integration, contract) {
            Ok(_) => {}
            Err(WorkspaceError::ScopeViolation(paths)) => {
                let detail = format!(
                    "the integrated diff leaves the contract scope: {}",
                    paths.join(", ")
                );
                engine.transition(
                    State::NeedsDecision,
                    reason::SCOPE_EXCEEDED,
                    serde_json::json!({ "paths": paths }),
                );
                return Decomposed::Outcome(engine.outcome_for(RunOutcome::NeedsDecision {
                    run_id: engine.run_id.clone(),
                    reason: reason::SCOPE_EXCEEDED.into(),
                    detail,
                }));
            }
            Err(e) => {
                return Decomposed::Outcome(
                    engine.fail_preflight("scope_check_failed", e.to_string()),
                )
            }
        }
        engine.state = State::Verifying;
        let label = 900 + integration_repairs;
        let (checks, gaps) = match engine.verify_candidate(
            &integration,
            &head,
            root.authority,
            root.logs_dir,
            label,
        ) {
            Ok(result) => result,
            Err(e) => {
                return Decomposed::Outcome(
                    engine.fail_preflight("verification_unavailable", e.to_string()),
                )
            }
        };
        if !gaps.is_empty() {
            let detail = format!("required checks are gaps, not passes: {}", gaps.join("; "));
            engine.transition(
                State::NeedsDecision,
                reason::VERIFICATION_GAP,
                serde_json::json!({ "gaps": gaps }),
            );
            return Decomposed::Outcome(engine.outcome_for(RunOutcome::NeedsDecision {
                run_id: engine.run_id.clone(),
                reason: reason::VERIFICATION_GAP.into(),
                detail,
            }));
        }
        let failures: Vec<String> = checks
            .iter()
            .filter(|check| check.failed())
            .map(|check| check.label.clone())
            .collect();
        if failures.is_empty() {
            let _ = integration
                .export_patch(&head, &engine.artifacts.join("candidate-integrated.patch"));
            return Decomposed::Outcome(accept_integrated(
                engine,
                root,
                &head,
                checks,
                attempts_total,
                models_used,
            ));
        }
        // One bounded integration repair from the assembled head, within
        // the same aggregate budget (SPEC §19). It is a package with the
        // whole contract scope and the integration acceptance.
        if integration_repairs >= 1 {
            let detail = format!(
                "the integrated candidate fails verification after one repair: {}",
                failures.join(", ")
            );
            engine.transition(
                State::Failed,
                reason::INTEGRATION_FAILED,
                serde_json::json!({ "failures": failures }),
            );
            return Decomposed::Outcome(engine.outcome_for(RunOutcome::Failed {
                run_id: engine.run_id.clone(),
                detail,
            }));
        }
        integration_repairs += 1;
        let repair = WorkPackage {
            id: "integration-repair".into(),
            objective: format!(
                "Integration of the work packages fails verification ({}); make the assembled \
                 change pass without changing what the packages delivered",
                failures.join(", ")
            ),
            write_scope: contract.write_scope.clone().unwrap_or_default(),
            depends_on: plan.packages.iter().map(|p| p.id.clone()).collect(),
            acceptance: plan.integration_acceptance.clone(),
        };
        engine.transition(
            State::Repairing,
            reason::BEHAVIORAL_FAILURE,
            serde_json::json!({ "failures": failures, "package": repair.id }),
        );
        match run_package(
            engine,
            root,
            &plan,
            &repair,
            order.iter().map(|(_, wave)| *wave).max().unwrap_or(0) + 1,
            &head,
            &mut attempts_total,
            &mut models_used,
        ) {
            PackageEnd::Accepted(candidate) => match fast_forward(&integration_path, &candidate) {
                Ok(new_head) => head = new_head,
                Err(detail) => {
                    engine.transition(
                        State::NeedsDecision,
                        reason::INTEGRATION_CONFLICT,
                        serde_json::json!({ "package": repair.id, "detail": detail }),
                    );
                    return Decomposed::Outcome(engine.outcome_for(RunOutcome::NeedsDecision {
                        run_id: engine.run_id.clone(),
                        reason: reason::INTEGRATION_CONFLICT.into(),
                        detail,
                    }));
                }
            },
            PackageEnd::Stop(outcome) => return Decomposed::Outcome(outcome),
        }
    }
}

enum PackageEnd {
    Accepted(String),
    Stop(RunOutcome),
}

/// Run one package as a child run from `input_sha`, under what remains
/// of the aggregate limits. The child's own lifecycle decides its
/// state; the root mirrors anything but acceptance.
#[allow(clippy::too_many_arguments)]
fn run_package(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    plan: &WorkPlan,
    package: &WorkPackage,
    wave: u32,
    input_sha: &str,
    attempts_total: &mut u32,
    models_used: &mut Vec<String>,
) -> PackageEnd {
    let ledger = engine.config.ledger;
    let contract = engine.config.contract;

    // Aggregate limits, checked before every package (SPEC §19).
    let remaining_wall = root.deadline.saturating_duration_since(Instant::now());
    if remaining_wall < Duration::from_secs(1) {
        return PackageEnd::Stop(engine.budget_exhausted(format!(
            "wall clock for the run is exhausted before package `{}`",
            package.id
        )));
    }
    let spent = ledger.run_cost(&engine.run_id).expect("cost");
    let remaining_budget = engine
        .config
        .machine
        .spending
        .per_run_micros
        .map(|ceiling| ceiling - spent.to_micros());
    if remaining_budget.is_some_and(|remaining| remaining <= 0) {
        return PackageEnd::Stop(engine.budget_exhausted(format!(
            "per-run spend ceiling reached ({spent}) before package `{}`",
            package.id
        )));
    }
    let dispatched: u32 = std::iter::once(engine.run_id.clone())
        .chain(
            ledger
                .child_runs(&engine.run_id)
                .expect("children")
                .into_iter()
                .map(|(run_id, _, _)| run_id),
        )
        .map(|run_id| ledger.dispatch_count(&run_id).expect("dispatches"))
        .sum();
    let remaining_agents = root.authority.max_agents_total.saturating_sub(dispatched);
    if remaining_agents == 0 {
        return PackageEnd::Stop(engine.budget_exhausted(format!(
            "aggregate agent cap {} reached before package `{}`",
            root.authority.max_agents_total, package.id
        )));
    }

    // The package contract: the package's objective, scope and
    // acceptance, the root's everything else, from the input revision.
    let child_contract = TaskContract {
        schema_version: contract.schema_version,
        kind: Kind::Change,
        objective: format!(
            "{}\n\n(work package `{}` of: {})",
            package.objective, package.id, contract.objective
        ),
        base_ref: input_sha.to_string(),
        write_scope: Some(package.write_scope.clone()),
        read_hints: contract.read_hints.clone(),
        acceptance: package.acceptance.clone(),
        verification_profile: contract.verification_profile.clone(),
        architecture: contract.architecture.clone(),
        risk_hints: contract.risk_hints.clone(),
        limits: crate::contract::Limits {
            attempts: plan.limits.attempts_per_package,
            wall_seconds: remaining_wall.as_secs().max(1),
        },
        review: contract.review.max(Review::Off),
        decomposition: None,
    };
    let mut child_machine: MachineSettings = engine.config.machine.clone();
    child_machine.spending.per_run_micros = remaining_budget;
    child_machine.concurrency.max_agents_per_run = Some(
        child_machine
            .concurrency
            .max_agents_per_run
            .map_or(remaining_agents, |cap| cap.min(remaining_agents)),
    );
    let child_config = RunConfig {
        repo_dir: engine.config.repo_dir,
        contract: &child_contract,
        repo_policy: engine.config.repo_policy,
        machine: &child_machine,
        ledger,
        backend: engine.config.backend,
        artifacts_dir: engine.artifacts.join("packages").join(&package.id),
        aval_resolver: engine.config.aval_resolver,
        predictor: engine.config.predictor,
        gate: engine.config.gate,
        session_id: engine.config.session_id.clone(),
        heartbeat_every: engine.config.heartbeat_every,
    };
    engine.transition(
        State::Running,
        reason::PACKAGE_STARTED,
        serde_json::json!({ "package": package.id, "wave": wave, "input": input_sha }),
    );
    let outcome = execute_child(&child_config, &engine.run_id, &package.id);
    let child_run = outcome.run_id().to_string();
    *attempts_total += ledger.attempt_count(&child_run).expect("attempts") as u32;
    for model in ledger.models_used(&child_run).expect("models") {
        if !models_used.contains(&model) {
            models_used.push(model);
        }
    }
    engine.transition(
        State::Running,
        reason::PACKAGE_FINISHED,
        serde_json::json!({
            "package": package.id,
            "child_run": child_run,
            "state": outcome.state().as_str(),
        }),
    );
    match outcome {
        RunOutcome::Accepted { receipt, .. } => PackageEnd::Accepted(receipt.candidate_sha),
        other => {
            let prefix = format!(
                "package `{}` ({child_run}) ended {}: ",
                package.id,
                other.state()
            );
            PackageEnd::Stop(mirror(engine, other, &prefix))
        }
    }
}

/// The root takes the child's terminal state, with the package named.
fn mirror(engine: &mut RunEngine<'_>, outcome: RunOutcome, prefix: &str) -> RunOutcome {
    let run_id = engine.run_id.clone();
    let mirrored = match outcome {
        RunOutcome::Accepted { .. } => unreachable!("acceptance is not mirrored"),
        RunOutcome::NeedsDecision { reason, detail, .. } => RunOutcome::NeedsDecision {
            run_id,
            reason,
            detail: format!("{prefix}{detail}"),
        },
        RunOutcome::NeedsReview { detail, .. } => RunOutcome::NeedsReview {
            run_id,
            detail: format!("{prefix}{detail}"),
        },
        RunOutcome::Blocked { code, detail, .. } => RunOutcome::Blocked {
            run_id,
            code,
            detail: format!("{prefix}{detail}"),
        },
        RunOutcome::Failed { detail, .. } => RunOutcome::Failed {
            run_id,
            detail: format!("{prefix}{detail}"),
        },
        RunOutcome::BudgetExhausted { detail, .. } => RunOutcome::BudgetExhausted {
            run_id,
            detail: format!("{prefix}{detail}"),
        },
        RunOutcome::Interrupted { detail, .. } => RunOutcome::Interrupted {
            run_id,
            detail: format!("{prefix}{detail}"),
        },
        RunOutcome::Cancelled { detail, .. } => RunOutcome::Cancelled {
            run_id,
            detail: format!("{prefix}{detail}"),
        },
    };
    let reason_code = match &mirrored {
        RunOutcome::NeedsDecision { reason, .. } => reason.clone(),
        RunOutcome::Blocked { code, .. } => code.clone(),
        RunOutcome::Cancelled { .. } => reason::CANCELLED_BY_USER.to_string(),
        RunOutcome::Interrupted { .. } => reason::PROCESS_CRASH.to_string(),
        RunOutcome::BudgetExhausted { .. } => reason::LIMIT_REACHED.to_string(),
        _ => reason::PACKAGE_FINISHED.to_string(),
    };
    engine.transition(
        mirrored.state(),
        &reason_code,
        serde_json::json!({ "detail": prefix.trim_end_matches(": ") }),
    );
    engine.outcome_for(mirrored)
}

/// Advance the integration worktree to a candidate that descends from
/// its head. A non-fast-forward means the package was built on a stale
/// input: a decision, not a silent merge.
fn fast_forward(integration_path: &Path, candidate_sha: &str) -> Result<String, String> {
    let merge = Command::new("git")
        .args(["merge", "--ff-only", candidate_sha])
        .current_dir(integration_path)
        .output()
        .map_err(|e| format!("git merge: {e}"))?;
    if !merge.status.success() {
        return Err(format!(
            "not a fast-forward of the integration head: {}",
            String::from_utf8_lossy(&merge.stderr).trim()
        ));
    }
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(integration_path)
        .output()
        .map_err(|e| format!("git rev-parse: {e}"))?;
    Ok(String::from_utf8_lossy(&head.stdout).trim().to_string())
}

fn accept_integrated(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    head: &str,
    checks: Vec<crate::verify::CheckOutcome>,
    attempts_total: u32,
    mut models_used: Vec<String>,
) -> RunOutcome {
    let ledger = engine.config.ledger;
    let mut total_cost = ledger.run_cost(&engine.run_id).expect("cost");
    let mut completeness = ledger
        .run_cost_completeness(&engine.run_id)
        .expect("completeness");
    if root.decision.review >= Review::Required {
        let mut review_cost = MicroUsd::ZERO;
        let mut review_completeness = CostCompleteness::Actual;
        let review = engine.review_candidate(
            root.manifest,
            root.authority,
            head,
            &mut review_cost,
            &mut review_completeness,
            root.deadline,
        );
        total_cost += review_cost;
        completeness = worst_completeness(completeness, review_completeness);
        for model in ledger.models_used(&engine.run_id).expect("models") {
            if !models_used.contains(&model) {
                models_used.push(model);
            }
        }
        match review {
            ReviewOutcome::NoFindings => {}
            ReviewOutcome::Findings(detail) => {
                engine.transition(
                    State::NeedsReview,
                    reason::REVIEW_FINDINGS,
                    serde_json::json!({ "detail": detail }),
                );
                return engine.outcome_for(RunOutcome::NeedsReview {
                    run_id: engine.run_id.clone(),
                    detail,
                });
            }
            ReviewOutcome::Unavailable(detail) => {
                engine.transition(
                    State::NeedsReview,
                    reason::REVIEW_UNAVAILABLE,
                    serde_json::json!({ "detail": detail }),
                );
                return engine.outcome_for(RunOutcome::NeedsReview {
                    run_id: engine.run_id.clone(),
                    detail,
                });
            }
        }
    }
    let report = VerificationReport {
        candidate_sha: head.to_string(),
        base_sha: root.base_sha.to_string(),
        contract_hash: root.contract_hash.to_string(),
        policy_hash: root.authority.authority_hash.clone(),
        checks,
        gaps: Vec::new(),
        baseline_failures: root.baseline_failures.to_vec(),
        amont_bypasses: Vec::new(),
        amont_downgrades: Vec::new(),
    };
    let receipt = Receipt {
        run_id: engine.run_id.clone(),
        candidate_sha: head.to_string(),
        base_sha: root.base_sha.to_string(),
        contract_hash: root.contract_hash.to_string(),
        policy_hash: root.authority.authority_hash.clone(),
        outcome: State::Accepted.as_str().to_string(),
        verification: report,
        models_used,
        attempts: attempts_total,
        cost_completeness: completeness,
        cost: total_cost,
    };
    let receipt_hash = receipt.hash();
    ledger
        .store_receipt(
            &engine.run_id,
            &serde_json::to_value(&receipt).expect("serializes"),
            &receipt_hash,
        )
        .expect("receipt stored");
    std::fs::write(
        engine.artifacts.join("receipt.json"),
        serde_json::to_string_pretty(&receipt).expect("serializes"),
    )
    .expect("receipt artifact");
    engine.transition(
        State::Accepted,
        reason::CHECKS_AND_REVIEW_PASSED,
        serde_json::json!({
            "candidate": head,
            "receipt_hash": receipt_hash,
            "attempts": attempts_total,
            "integrated": true,
        }),
    );
    engine.outcome_for(RunOutcome::Accepted {
        run_id: engine.run_id.clone(),
        receipt: Box::new(receipt),
    })
}

enum Proposal {
    Plan(WorkPlan),
    SingleWorker,
    Rejected(String),
    Failed(RunOutcome),
}

/// One bounded planner call at the research tier (SPEC §19). Its output
/// is data: parsed, then validated like an explicit plan. An empty
/// package list means "one worker"; anything unparseable goes back to a
/// human with the raw proposal preserved.
fn propose_plan(engine: &mut RunEngine<'_>, root: &RootContext<'_>) -> Proposal {
    let contract = engine.config.contract;
    let profile = root
        .authority
        .models
        .get(&Tier::Research)
        .or_else(|| root.authority.models.get(&Tier::Implementation))
        .cloned();
    let Some(profile) = profile else {
        return Proposal::Rejected("no research or implementation model to plan with".into());
    };
    let mut prompt = String::from(
        "You are a bounded planner. Decide whether the task below separates into independent work \
         packages. Output ONLY one JSON object, no prose, in this shape:\n\
         {\"packages\":[{\"id\":\"short-id\",\"objective\":\"...\",\"write_scope\":[\"glob\"],\
         \"depends_on\":[],\"acceptance\":[\"...\"]}],\"integration_acceptance\":[\"...\"]}\n\
         Rules: every package scope must lie within the contract write_scope; packages with no \
         dependency path between them must not overlap in scope; each package needs its own \
         acceptance; at most 4 packages. If one worker is the right shape, answer \
         {\"packages\":[],\"integration_acceptance\":[]}.\n\n",
    );
    prompt.push_str(&format!("objective: {}\n", contract.objective));
    prompt.push_str(&format!(
        "write_scope: {:?}\n",
        contract.write_scope.as_deref().unwrap_or_default()
    ));
    prompt.push_str("acceptance:\n");
    for criterion in &contract.acceptance {
        prompt.push_str(&format!("  - {criterion}\n"));
    }
    if !root.manifest.constraints.is_empty() {
        prompt.push_str("constraints:\n");
        for constraint in &root.manifest.constraints {
            prompt.push_str(&format!("  - {constraint}\n"));
        }
    }
    let dispatch_id = DispatchId::generate();
    let remaining_budget = engine
        .config
        .machine
        .spending
        .per_run_micros
        .map(|ceiling| (ceiling - ledger_cost(engine)).max(0));
    engine
        .config
        .ledger
        .record_dispatch_intent(
            dispatch_id.as_str(),
            &engine.run_id,
            None,
            &serde_json::json!({ "model": profile.id, "kind": "Plan" }),
            remaining_budget.unwrap_or(0),
        )
        .expect("intent recorded");
    let spec = LaunchSpec {
        dispatch_id: dispatch_id.as_str().to_string(),
        prompt,
        model: profile.id.clone(),
        effort: profile.effort,
        max_turns: Some(8),
        budget_micros: remaining_budget,
        disallowed_tools: {
            let mut tools = root.authority.disallowed_tools.clone();
            tools.extend(["Edit".to_string(), "Write".to_string()]);
            tools
        },
        work_dir: engine.config.repo_dir.to_path_buf(),
        wall_timeout: root
            .deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_secs(1)),
        cancel: None,
        pid_slot: None,
    };
    let result =
        match engine.managed_launch(spec, 0, None, remaining_budget.unwrap_or(0), root.deadline) {
            Ok(result) => result,
            Err(outcome) => {
                let _ = engine
                    .config
                    .ledger
                    .finish_dispatch(dispatch_id.as_str(), "launch_failed");
                return Proposal::Failed(outcome);
            }
        };
    engine
        .config
        .ledger
        .finish_dispatch(dispatch_id.as_str(), "completed")
        .expect("finish");
    // Planning overhead is the run's cost (SPEC §19).
    engine
        .config
        .ledger
        .record_usage(&UsageEvent {
            event_id: dispatch_id.as_str().to_string(),
            run_id: engine.run_id.clone(),
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
        })
        .expect("planner usage recorded");
    if result.cancelled {
        return Proposal::Failed(engine.cancelled("cancelled while planning".into()));
    }
    if result.terminal_result_missing() {
        return Proposal::Rejected("the planner ended without a terminal result".into());
    }
    let text = result.result_text.unwrap_or_default();
    let _ = std::fs::write(engine.artifacts.join("plan-proposal.txt"), &text);
    let Some(json) = extract_json_object(&text) else {
        return Proposal::Rejected(
            "the planner produced no JSON object; the proposal is preserved in plan-proposal.txt"
                .into(),
        );
    };
    match serde_json::from_str::<WorkPlan>(json) {
        Ok(plan) if plan.packages.is_empty() => Proposal::SingleWorker,
        Ok(plan) => Proposal::Plan(plan),
        Err(e) => Proposal::Rejected(format!(
            "the planner's proposal does not parse as a work plan: {e}; preserved in plan-proposal.txt"
        )),
    }
}

fn ledger_cost(engine: &RunEngine<'_>) -> i64 {
    engine
        .config
        .ledger
        .run_cost(&engine.run_id)
        .map(MicroUsd::to_micros)
        .unwrap_or(0)
}

/// The outermost `{ ... }` of a text, or nothing.
pub fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// Where a package's preserved worktree lives, for reports.
pub fn package_dir(root_artifacts: &Path, package_id: &str) -> PathBuf {
    root_artifacts.join("packages").join(package_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_is_prefix_based_and_conservative() {
        assert!(covers("crates/**", "crates/amont/**"));
        assert!(covers("crates/**", "crates/amont/src/lib.rs"));
        assert!(covers("**", "anything/at/all"));
        assert!(covers("src/main.rs", "src/main.rs"));
        assert!(!covers("crates/**", "docs/**"));
        assert!(!covers("crates/a/**", "crates/ab/**"));
        assert!(!covers("src/main.rs", "src/**"));
    }

    #[test]
    fn overlap_is_judged_on_literal_prefixes() {
        assert!(overlaps("crates/a/**", "crates/a/src/**"));
        assert!(overlaps("**", "docs/**"));
        assert!(!overlaps("crates/a/**", "crates/b/**"));
        assert!(!overlaps("docs/**", "src/**"));
    }

    #[test]
    fn json_object_extraction_takes_the_outermost_braces() {
        assert_eq!(
            extract_json_object("sure:\n{\"packages\":[{\"id\":\"a\"}]}\nthanks"),
            Some("{\"packages\":[{\"id\":\"a\"}]}")
        );
        assert_eq!(extract_json_object("no json here"), None);
        assert_eq!(extract_json_object("}{"), None);
    }
}
