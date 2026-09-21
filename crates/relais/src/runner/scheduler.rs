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
use std::time::{Duration, Instant};

use crate::backend::LaunchSpec;
use crate::context::ContextManifest;
use crate::contract::{Decomposition, DecompositionMode, Kind, TaskContract, WorkPlan};
use crate::contract::{Review, WorkPackage};
use crate::ids::DispatchId;
use crate::ledger::UsageEvent;
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::policy::{BlockCode, EffectiveAuthority, MachineSettings, Tier};
use crate::procs::Ended;
use crate::route::RouteDecision;
use crate::verify::{self, Receipt, VerificationReport};
use crate::workspace::{self, WorkspaceError};

use super::{
    data_block, data_list_block, execute_child, Budget, Limit, Next, Observation, Reason,
    ReviewOutcome, RunConfig, RunEngine, RunError, RunOutcome, State, Terminal,
};

/// Everything the root preflight established that the packages inherit.
pub(crate) struct RootContext<'a> {
    pub authority: &'a EffectiveAuthority,
    pub base_sha: &'a str,
    pub contract_hash: &'a str,
    pub manifest: &'a ContextManifest,
    pub decision: &'a RouteDecision,
    pub baseline_failures: &'a [String],
    pub integration_gaps: &'a [String],
    pub baseline_cached: bool,
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
) -> Result<Decomposed, RunError> {
    let contract = engine.config.contract;
    if contract.kind != Kind::Change {
        return Ok(Decomposed::Outcome(engine.fail_preflight(
            BlockCode::DecompositionKind,
            "only kind=change can be decomposed".into(),
        )?));
    }
    let plan = match decomposition {
        Decomposition::Plan(plan) => plan.clone(),
        Decomposition::Mode(DecompositionMode::Propose) => match propose_plan(engine, root)? {
            Proposal::Plan(plan) => plan,
            Proposal::SingleWorker => return Ok(Decomposed::SingleWorker),
            Proposal::Rejected(detail) => {
                return Ok(Decomposed::Outcome(engine.finish(
                    Reason::PlanRejected,
                    serde_json::json!({ "detail": detail }),
                    Terminal::NeedsDecision {
                        reason: Reason::PlanRejected,
                        detail,
                    },
                )?));
            }
            Proposal::Failed(outcome) => return Ok(Decomposed::Outcome(outcome)),
        },
    };
    let problems = check_plan(&plan, contract, root.authority);
    if !problems.is_empty() {
        let detail = format!("the work plan was rejected: {}", problems.join("; "));
        return Ok(Decomposed::Outcome(engine.finish(
            Reason::PlanRejected,
            serde_json::json!({ "problems": problems }),
            Terminal::NeedsDecision {
                reason: Reason::PlanRejected,
                detail,
            },
        )?));
    }
    let order = plan
        .validate()
        .map_err(|e| RunError::Other(format!("a checked plan failed to validate: {e}")))?;
    std::fs::write(
        engine.artifacts.join("plan.json"),
        serde_json::to_string_pretty(&plan).expect("a plan serializes"),
    )?;
    engine.transition(
        State::Running,
        Reason::PlanAccepted,
        serde_json::json!({
            "packages": order.iter().map(|(index, wave)| {
                serde_json::json!({ "id": plan.packages[*index].id, "wave": wave })
            }).collect::<Vec<_>>(),
        }),
    )?;

    // The integration worktree: every accepted package fast-forwards
    // it. Outside the artifact directory like every other worktree
    // (audit B6) — a package's worker cannot reach the assembled
    // candidate, or the root's receipt, through a relative path.
    let integration_path = engine.worktrees.join("integration");
    let integration = match workspace::create_worktree(
        engine.config.repo_dir,
        root.base_sha,
        &integration_path,
    ) {
        Ok(worktree) => worktree,
        Err(e) => {
            return Ok(Decomposed::Outcome(
                engine.fail_preflight(BlockCode::WorktreeUnavailable, e.to_string())?,
            ))
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
        )?;
        match outcome {
            PackageEnd::Accepted(candidate) => match fast_forward(&integration_path, &candidate) {
                Ok(new_head) => head = new_head,
                Err(detail) => {
                    return Ok(Decomposed::Outcome(engine.finish(
                        Reason::IntegrationConflict,
                        serde_json::json!({ "package": package.id, "detail": detail }),
                        Terminal::NeedsDecision {
                            reason: Reason::IntegrationConflict,
                            detail: format!(
                                "package `{}` cannot be integrated: {detail}; its candidate is preserved",
                                package.id
                            ),
                        },
                    )?));
                }
            },
            PackageEnd::Stop(outcome) => return Ok(Decomposed::Outcome(outcome)),
        }
    }

    // Independent final verification of the assembled candidate (SPEC
    // §19), against the ROOT contract's scope and profile.
    let budget = Budget {
        attempts_used: attempts_total,
        max_attempts: root.authority.max_attempts,
        repairs_used: 0,
        max_repairs: 0,
        tier: root.decision.tier.unwrap_or(Tier::Implementation),
        escalation_tier: None,
    };
    let mut integration_repairs: u32 = 0;
    loop {
        match workspace::check_scope(&integration, &head, contract) {
            Ok(_) => {}
            Err(WorkspaceError::ScopeViolation(paths)) => {
                return Ok(Decomposed::Outcome(
                    engine.stop(&budget, Observation::ScopeViolation(paths))?,
                ));
            }
            Err(e) => {
                return Ok(Decomposed::Outcome(
                    engine.fail_preflight(BlockCode::ScopeCheckFailed, e.to_string())?,
                ))
            }
        }
        engine.state = State::Verifying;
        // Root verification waits for every relevant write lease: the
        // integration tree's and each package worker's (SPEC §23). The
        // packages ran to completion and gave their leases back with
        // their processes; a holder here is a straggler, and the
        // integrated candidate is not verified over it.
        let mut relevant = vec![integration.path.clone()];
        for (child_run, package_id, _) in engine.config.ledger.child_runs(&engine.run_id)? {
            relevant.push(
                crate::runner::worktree_root(&engine.artifacts.join("packages").join(&package_id))
                    .join(&child_run)
                    .join("task"),
            );
        }
        for path in &relevant {
            if let Some(holder) = engine.wait_for_writers(path, root.deadline)? {
                return Ok(Decomposed::Outcome(
                    engine.stop_on_held_lease(path, &holder)?,
                ));
            }
        }
        let label = 900 + integration_repairs;
        let verify::Verified {
            checks,
            gaps,
            amont_bypasses,
            amont_downgrades,
        } = match engine.verify_candidate(
            &integration,
            &head,
            root.authority,
            root.logs_dir,
            label,
            None,
        ) {
            Ok(result) => result,
            Err(e) => {
                return Ok(Decomposed::Outcome(
                    engine.fail_preflight(BlockCode::VerificationUnavailable, e)?,
                ))
            }
        };
        if !gaps.is_empty() {
            return Ok(Decomposed::Outcome(
                engine.stop(&budget, Observation::VerificationGap(gaps))?,
            ));
        }
        let failures: Vec<String> = checks
            .iter()
            .filter(|check| check.failed())
            .map(|check| check.label.clone())
            .collect();
        if failures.is_empty() {
            integration
                .export_patch(&head, &engine.artifacts.join("candidate-integrated.patch"))?;
            let touched_inputs = verify::classify_verification_inputs(
                &root.authority.verification_profile,
                &integration.changed_paths_in(&head)?,
            );
            return Ok(Decomposed::Outcome(accept_integrated(
                engine,
                root,
                &head,
                Assembled {
                    checks,
                    amont_bypasses,
                    amont_downgrades,
                    touched_inputs,
                },
                attempts_total,
                models_used,
            )?));
        }
        // One bounded integration repair from the assembled head, within
        // the same aggregate budget (SPEC §19). It is a package with the
        // whole contract scope and the integration acceptance.
        if integration_repairs >= 1 {
            let detail = format!(
                "the integrated candidate fails verification after one repair: {}",
                failures.join(", ")
            );
            return Ok(Decomposed::Outcome(engine.finish(
                Reason::IntegrationFailed,
                serde_json::json!({ "failures": failures }),
                Terminal::Failed { detail },
            )?));
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
            Reason::BehavioralFailure,
            serde_json::json!({ "failures": failures, "package": repair.id }),
        )?;
        match run_package(
            engine,
            root,
            &plan,
            &repair,
            order.iter().map(|(_, wave)| *wave).max().unwrap_or(0) + 1,
            &head,
            &mut attempts_total,
            &mut models_used,
        )? {
            PackageEnd::Accepted(candidate) => match fast_forward(&integration_path, &candidate) {
                Ok(new_head) => head = new_head,
                Err(detail) => {
                    return Ok(Decomposed::Outcome(engine.finish(
                        Reason::IntegrationConflict,
                        serde_json::json!({ "package": repair.id, "detail": detail }),
                        Terminal::NeedsDecision {
                            reason: Reason::IntegrationConflict,
                            detail,
                        },
                    )?));
                }
            },
            PackageEnd::Stop(outcome) => return Ok(Decomposed::Outcome(outcome)),
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
) -> Result<PackageEnd, RunError> {
    let ledger = engine.config.ledger;
    let contract = engine.config.contract;
    let budget = Budget {
        attempts_used: *attempts_total,
        max_attempts: root.authority.max_attempts,
        repairs_used: 0,
        max_repairs: 0,
        tier: root.decision.tier.unwrap_or(Tier::Implementation),
        escalation_tier: None,
    };

    // Aggregate limits, checked before every package (SPEC §19).
    let remaining_wall = root.deadline.saturating_duration_since(Instant::now());
    if remaining_wall < Duration::from_secs(1) {
        return Ok(PackageEnd::Stop(
            engine.stop(&budget, Observation::LimitReached(Limit::WallClock))?,
        ));
    }
    let spent = ledger.run_cost(&engine.run_id)?;
    let remaining_budget = engine
        .config
        .machine
        .spending
        .per_run_micros
        .map(|ceiling| ceiling - spent.to_micros());
    if let Some(ceiling) = engine.config.machine.spending.per_run_micros {
        if remaining_budget.is_some_and(|remaining| remaining <= 0) {
            return Ok(PackageEnd::Stop(engine.stop(
                &budget,
                Observation::LimitReached(Limit::Spend {
                    spent: spent.to_string(),
                    ceiling: MicroUsd::from_micros(ceiling).to_string(),
                }),
            )?));
        }
    }
    let mut dispatched: u32 = ledger.dispatch_count(&engine.run_id)?;
    for (run_id, _, _) in ledger.child_runs(&engine.run_id)? {
        dispatched += ledger.dispatch_count(&run_id)?;
    }
    let remaining_agents = root.authority.max_agents_total.saturating_sub(dispatched);
    if remaining_agents == 0 {
        return Ok(PackageEnd::Stop(engine.stop(
            &budget,
            Observation::LimitReached(Limit::Admission {
                code: "run_agent_cap".into(),
                detail: format!(
                    "aggregate agent cap {} reached before package `{}`",
                    root.authority.max_agents_total, package.id
                ),
            }),
        )?));
    }

    // The package contract: the package's objective, scope and
    // acceptance, the root's everything else, from the input revision.
    // Under `"decomposition": "propose"` the package objective is model
    // output; `WorkPlan::validate` has already refused a plan whose
    // objective is not a single line of at most
    // `MAX_PACKAGE_OBJECTIVE_CHARS`, so splicing it here cannot add lines
    // to the child's objective, and `build_prompt` quotes it as data.
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
        // The package's artifacts hang off the root run's; its
        // worktrees hang off THEIR parent, so a package worker's tree
        // sits under `packages/worktrees/<child-run>/` and no package's
        // record — its own or a sibling's — is its cwd's parent (B6).
        artifacts_dir: engine.artifacts.join("packages").join(&package.id),
        aval_resolver: engine.config.aval_resolver,
        predictor: engine.config.predictor,
        gate: engine.config.gate,
        session_id: engine.config.session_id.clone(),
        heartbeat_every: engine.config.heartbeat_every,
    };
    engine.transition(
        State::Running,
        Reason::PackageStarted,
        serde_json::json!({ "package": package.id, "wave": wave, "input": input_sha }),
    )?;
    let outcome = execute_child(&child_config, &engine.run_id, &package.id);
    let child_run = outcome.run_id().to_string();
    *attempts_total += ledger.attempt_count(&child_run)? as u32;
    for model in ledger.models_used(&child_run)? {
        if !models_used.contains(&model) {
            models_used.push(model);
        }
    }
    engine.transition(
        State::Running,
        Reason::PackageFinished,
        serde_json::json!({
            "package": package.id,
            "child_run": child_run,
            "state": outcome.state().as_str(),
        }),
    )?;
    match outcome.terminal {
        Terminal::Accepted(receipt) => Ok(PackageEnd::Accepted(receipt.candidate_sha)),
        terminal => {
            let prefix = format!(
                "package `{}` ({child_run}) ended {}: ",
                package.id,
                terminal.state()
            );
            Ok(PackageEnd::Stop(mirror(engine, terminal, &prefix)?))
        }
    }
}

/// The root takes the child's terminal state, with the package named.
fn mirror(
    engine: &mut RunEngine<'_>,
    terminal: Terminal,
    prefix: &str,
) -> Result<RunOutcome, RunError> {
    let with_prefix = |detail: String| format!("{prefix}{detail}");
    let (reason, mirrored) = match terminal {
        Terminal::Accepted(_) => return Err(RunError::Other("acceptance is not mirrored".into())),
        Terminal::NeedsDecision { reason, detail } => (
            reason,
            Terminal::NeedsDecision {
                reason,
                detail: with_prefix(detail),
            },
        ),
        Terminal::NeedsReview { detail } => (
            Reason::PackageFinished,
            Terminal::NeedsReview {
                detail: with_prefix(detail),
            },
        ),
        Terminal::Blocked { code, detail } => (
            Reason::BlockedPreflight,
            Terminal::Blocked {
                code,
                detail: with_prefix(detail),
            },
        ),
        Terminal::Failed { detail } => (
            Reason::PackageFinished,
            Terminal::Failed {
                detail: with_prefix(detail),
            },
        ),
        Terminal::BudgetExhausted { detail } => (
            Reason::LimitReached,
            Terminal::BudgetExhausted {
                detail: with_prefix(detail),
            },
        ),
        Terminal::Interrupted { detail } => (
            Reason::ProcessCrash,
            Terminal::Interrupted {
                detail: with_prefix(detail),
            },
        ),
        Terminal::Cancelled { detail } => (
            Reason::CancelledByUser,
            Terminal::Cancelled {
                detail: with_prefix(detail),
            },
        ),
    };
    engine.finish(
        reason,
        serde_json::json!({ "detail": prefix.trim_end_matches(": ") }),
        mirrored,
    )
}

/// Advance the integration worktree to a candidate that descends from
/// its head. A non-fast-forward means the package was built on a stale
/// input: a decision, not a silent merge.
fn fast_forward(integration_path: &Path, candidate_sha: &str) -> Result<String, String> {
    // `workspace::git_command` and not `Command::new("git")`: an
    // inherited GIT_DIR or GIT_INDEX_FILE would merge into a different
    // repository than the one this path names (audit B15).
    let merge = workspace::git_command(integration_path)
        .args(["merge", "--ff-only", candidate_sha])
        .output()
        .map_err(|e| format!("git merge: {e}"))?;
    if !merge.status.success() {
        return Err(format!(
            "not a fast-forward of the integration head: {}",
            String::from_utf8_lossy(&merge.stderr).trim()
        ));
    }
    let head = workspace::git_command(integration_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("git rev-parse: {e}"))?;
    Ok(String::from_utf8_lossy(&head.stdout).trim().to_string())
}

/// What the integrated candidate's verification established.
struct Assembled {
    checks: Vec<crate::verify::CheckOutcome>,
    amont_bypasses: Vec<String>,
    amont_downgrades: Vec<String>,
    touched_inputs: verify::TouchedInputs,
}

fn accept_integrated(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    head: &str,
    assembled: Assembled,
    attempts_total: u32,
    mut models_used: Vec<String>,
) -> Result<RunOutcome, RunError> {
    let ledger = engine.config.ledger;
    let mut total_cost = ledger.run_cost(&engine.run_id)?;
    let mut completeness = ledger.run_cost_completeness(&engine.run_id)?;
    let budget = Budget {
        attempts_used: attempts_total,
        max_attempts: root.authority.max_attempts,
        repairs_used: 0,
        max_repairs: 0,
        tier: root.decision.tier.unwrap_or(Tier::Implementation),
        escalation_tier: None,
    };
    // The assembled candidate is judged like any other (SPEC §19):
    // policy-class verification inputs are the user's decision, the test
    // tree is a reviewer's (audit B8).
    if !assembled.touched_inputs.policy.is_empty() {
        let detail = format!(
            "the assembled candidate changes what verification is: {}; the checks it passed are \
             no longer the checks the policy declares. The integrated candidate is preserved",
            assembled.touched_inputs.policy.join(", ")
        );
        return engine.finish(
            Reason::VerificationInputsChanged,
            serde_json::json!({ "paths": assembled.touched_inputs.policy, "candidate": head }),
            Terminal::NeedsDecision {
                reason: Reason::VerificationInputsChanged,
                detail,
            },
        );
    }
    let verification_inputs_changed = assembled.touched_inputs.tests;
    let review_required =
        root.decision.review >= Review::Required || !verification_inputs_changed.is_empty();
    if review_required {
        let mut review_cost = MicroUsd::ZERO;
        let mut review_completeness = CostCompleteness::Actual;
        let review = engine.review_candidate(
            root.manifest,
            root.authority,
            head,
            root.decision.tier.unwrap_or(Tier::Implementation),
            &verification_inputs_changed,
            &mut review_cost,
            &mut review_completeness,
            root.deadline,
        );
        total_cost += review_cost;
        completeness = completeness.max(review_completeness);
        for model in ledger.models_used(&engine.run_id)? {
            if !models_used.contains(&model) {
                models_used.push(model);
            }
        }
        match review {
            ReviewOutcome::NoFindings => {}
            ReviewOutcome::Findings(detail) => {
                return engine.stop(&budget, Observation::ReviewFindings(detail));
            }
            ReviewOutcome::Unavailable(detail) => {
                return engine.stop(&budget, Observation::ReviewUnavailable(detail));
            }
        }
    }
    match engine.decide(&budget, Observation::ChecksAndReviewPassed)? {
        Next::Accept => {}
        other => {
            return Err(RunError::Other(format!(
                "the machine answered {other:?} to a passing integrated candidate"
            )))
        }
    }
    let report = VerificationReport {
        candidate_sha: head.to_string(),
        base_sha: root.base_sha.to_string(),
        contract_hash: root.contract_hash.to_string(),
        policy_hash: root.authority.authority_hash.clone(),
        checks: assembled.checks,
        gaps: Vec::new(),
        baseline_failures: root.baseline_failures.to_vec(),
        amont_bypasses: assembled.amont_bypasses,
        amont_downgrades: assembled.amont_downgrades,
        verification_inputs_changed,
        integration_gaps: root.integration_gaps.to_vec(),
        baseline_cached: root.baseline_cached,
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
    engine.seal(&receipt, None, None, head)?;
    Ok(RunOutcome {
        run_id: engine.run_id.clone(),
        terminal: Terminal::Accepted(Box::new(receipt)),
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
fn propose_plan(engine: &mut RunEngine<'_>, root: &RootContext<'_>) -> Result<Proposal, RunError> {
    let contract = engine.config.contract;
    let profile = root
        .authority
        .models
        .get(&Tier::Research)
        .or_else(|| root.authority.models.get(&Tier::Implementation))
        .cloned();
    let Some(profile) = profile else {
        return Ok(Proposal::Rejected(
            "no research or implementation model to plan with".into(),
        ));
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
    // The objective, the acceptance criteria and the constraints are
    // project text, not instructions to the planner (SPEC §16): each one
    // is quoted inside its own labelled fence.
    prompt.push_str(&data_block("objective", &contract.objective));
    prompt.push_str(&format!(
        "write_scope: {:?}\n",
        contract.write_scope.as_deref().unwrap_or_default()
    ));
    prompt.push_str(&data_list_block("acceptance", &contract.acceptance));
    if !root.manifest.constraints.is_empty() {
        prompt.push_str(&data_list_block("constraints", &root.manifest.constraints));
    }
    let dispatch_id = DispatchId::generate();
    let spent = engine.config.ledger.run_cost(&engine.run_id)?.to_micros();
    let remaining_budget = engine
        .config
        .machine
        .spending
        .per_run_micros
        .map(|ceiling| (ceiling - spent).max(0));
    engine.config.ledger.record_dispatch_intent(
        dispatch_id.as_str(),
        &engine.run_id,
        None,
        &serde_json::json!({
            "model": profile.id,
            "effort": profile.effort,
            "harness": engine.harness,
            "kind": "plan",
        }),
        remaining_budget.unwrap_or(0),
    )?;
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
        // The planner reads; it gets no allowlist.
        allowed_tools: Vec::new(),
        work_dir: engine.config.repo_dir.to_path_buf(),
        wall_timeout: root
            .deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_secs(1)),
        cancel: None,
        pid_slot: None,
    };
    let budget = Budget {
        attempts_used: 0,
        max_attempts: root.authority.max_attempts,
        repairs_used: 0,
        max_repairs: 0,
        tier: Tier::Research,
        escalation_tier: None,
    };
    let result = match engine.managed_launch(
        spec,
        0,
        None,
        remaining_budget.unwrap_or(0),
        root.deadline,
        &budget,
        // The planner reads; it writes no worktree and takes no lease.
        None,
    )? {
        Ok(result) => result,
        Err(outcome) => {
            engine
                .config
                .ledger
                .finish_dispatch(dispatch_id.as_str(), "launch_failed")?;
            return Ok(Proposal::Failed(outcome));
        }
    };
    engine
        .config
        .ledger
        .finish_dispatch(dispatch_id.as_str(), "completed")?;
    // Planning overhead is the run's cost (SPEC §19).
    engine.config.ledger.record_usage(&UsageEvent {
        event_id: dispatch_id.as_str().to_string(),
        run_id: engine.run_id.clone(),
        attempt_id: None,
        parent_event_id: None,
        model: result.effective_model.clone(),
        input_tokens: result.usage.input_tokens,
        output_tokens: result.usage.output_tokens,
        cache_read_tokens: result.usage.cache_read_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        cost: result.usage.cost.micros(),
        cost_kind: CostKind::ApiSpend,
        completeness: result.usage.cost.completeness(),
        inclusive: result.usage.cost.inclusive(),
        at: engine.config.ledger.now(),
    })?;
    if result.ended == Ended::Cancelled {
        return Ok(Proposal::Failed(engine.stop(
            &budget,
            Observation::Cancelled("cancelled while planning".into()),
        )?));
    }
    if result.terminal_result_missing() {
        return Ok(Proposal::Rejected(
            "the planner ended without a terminal result".into(),
        ));
    }
    let text = result.result_text.unwrap_or_default();
    std::fs::write(engine.artifacts.join("plan-proposal.txt"), &text)?;
    let Some(json) = extract_json_object(&text) else {
        return Ok(Proposal::Rejected(
            "the planner produced no JSON object; the proposal is preserved in plan-proposal.txt"
                .into(),
        ));
    };
    Ok(match serde_json::from_str::<WorkPlan>(json) {
        Ok(plan) if plan.packages.is_empty() => Proposal::SingleWorker,
        Ok(plan) => Proposal::Plan(plan),
        Err(e) => Proposal::Rejected(format!(
            "the planner's proposal does not parse as a work plan: {e}; preserved in plan-proposal.txt"
        )),
    })
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
