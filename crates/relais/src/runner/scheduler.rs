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
use crate::ledger::UsageEvent;
use crate::money::{CostKind, MicroUsd};
use crate::policy::{BlockCode, EffectiveAuthority, MachineSettings, Tier};
use crate::procs::Ended;
use crate::route::Route;
use crate::verify::{self, Receipt, VerificationReport};
use crate::workspace::{self, WorkspaceError};

use super::{
    data_block, data_list_block, execute_child, AttemptLabel, Budget, Limit, ManagedDispatch, Next,
    Observation, Reason, ReviewOutcome, RunConfig, RunEngine, RunError, RunOutcome, State,
    Terminal,
};

/// The patch an accepted decomposed run exports its assembled revision
/// to, and the file its reviewer is told to read.
const INTEGRATED_PATCH: &str = "candidate-integrated.patch";

/// The attempt number the root run's own candidate ref is filed under.
/// Attempts count from 1, so 0 is free and names the one candidate a
/// decomposed root produces itself: the assembled revision.
const INTEGRATION_ATTEMPT: u32 = 0;

/// Everything the root preflight established that the packages inherit.
pub(crate) struct RootContext<'a> {
    pub authority: &'a EffectiveAuthority,
    pub base_sha: &'a str,
    pub contract_hash: &'a str,
    pub manifest: &'a ContextManifest,
    pub decision: &'a Route,
    pub baseline_failures: &'a [String],
    pub integration_gaps: &'a [String],
    pub baseline_cached: bool,
    /// Why the baseline could not be cached, when it could not (V2).
    pub baseline_cache_refused: &'a Option<crate::verify::CacheRefused>,
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
    // The shape check first: everything below reads `plan.packages` and
    // `plan.limits`, which `validate` is what bounds.
    if let Err(e) = plan.validate() {
        return vec![e.to_string()];
    }
    let root_scope: Vec<&str> = contract
        .scope_patterns()
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
    // Saturating: both factors are contract-supplied, and a product
    // that wrapped would report a plan of millions of dispatches as
    // fitting comfortably inside the cap (R11). `WorkPlan::validate`
    // bounds each factor as well, so this saturates only in theory.
    let worst_case = (plan.packages.len() as u32).saturating_mul(plan.limits.attempts_per_package);
    if worst_case > authority.max_agents_total {
        problems.push(format!(
            "{} packages x {} attempts = {worst_case} dispatches exceeds the aggregate agent cap {}",
            plan.packages.len(),
            plan.limits.attempts_per_package,
            authority.max_agents_total
        ));
    }
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

/// The decomposed path (SPEC §19): a validated plan, one integration
/// worktree, the packages in topological order, then the assembled
/// candidate verified independently.
pub(crate) fn run_decomposed(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    decomposition: &Decomposition,
) -> Result<Decomposed, RunError> {
    let plan = match plan_for(engine, root, decomposition)? {
        Planned::Ready(plan) => plan,
        Planned::SingleWorker => return Ok(Decomposed::SingleWorker),
        Planned::Ended(outcome) => return Ok(Decomposed::Outcome(outcome)),
    };

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

    let mut assembly = Assembly {
        head: root.base_sha.to_string(),
        attempts_total: 0,
        models_used: Vec::new(),
    };
    let outcome = execute_waves(engine, root, &plan, &integration, &mut assembly)?;
    // One exit for the integration worktree. An accepted run released
    // it already, against the patch and the ref it just wrote; what is
    // left here is a run that ended some other way, and the tree is
    // released only while it still holds nothing but the base — after
    // that, the assembled revision exists nowhere else and §8's "never
    // force-cleans a worktree containing unexported changes" applies
    // (R8).
    if !matches!(outcome.terminal, Terminal::Accepted(_)) && assembly.head == root.base_sha {
        if let Err(e) = workspace::release_worktree(
            engine.config.repo_dir,
            &integration_path,
            workspace::Disposition::MustBeClean,
        ) {
            engine.transition(
                engine.state,
                Reason::WorktreeNotReleased,
                serde_json::json!({
                    "worktree": integration_path.to_string_lossy(),
                    "error": e.to_string(),
                    "detail": "nothing was integrated, but the tree could not be removed",
                }),
            )?;
        }
    }
    Ok(Decomposed::Outcome(outcome))
}

/// What a run's packages assembled into, and what they spent doing it.
struct Assembly {
    /// The integration worktree's head: the base until a package is
    /// fast-forwarded into it.
    head: String,
    attempts_total: u32,
    models_used: Vec<String>,
}

/// The plan this run executes, or the end it reached instead.
enum Planned {
    Ready(WorkPlan),
    /// The planner answered "one worker is right".
    SingleWorker,
    Ended(RunOutcome),
}

/// The plan, validated against the contract and the run's authority
/// before anything is dispatched: an explicit plan as written, a
/// proposed one parsed from a bounded planner call and then checked
/// exactly the same way (SPEC §19).
fn plan_for(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    decomposition: &Decomposition,
) -> Result<Planned, RunError> {
    let contract = engine.config.contract;
    if contract.kind() != Kind::Change {
        return Ok(Planned::Ended(engine.fail_preflight(
            BlockCode::DecompositionKind,
            "only kind=change can be decomposed".into(),
        )?));
    }
    let plan = match decomposition {
        Decomposition::Plan(plan) => plan.clone(),
        Decomposition::Mode(DecompositionMode::Propose) => match propose_plan(engine, root)? {
            Proposal::Plan(plan) => plan,
            Proposal::SingleWorker => return Ok(Planned::SingleWorker),
            Proposal::Rejected(detail) => {
                return Ok(Planned::Ended(engine.finish(
                    Reason::PlanRejected,
                    serde_json::json!({ "detail": detail }),
                    Terminal::NeedsDecision {
                        reason: Reason::PlanRejected,
                        detail,
                    },
                )?));
            }
            Proposal::Failed(outcome) => return Ok(Planned::Ended(outcome)),
        },
    };
    let problems = check_plan(&plan, contract, root.authority);
    if !problems.is_empty() {
        let detail = format!("the work plan was rejected: {}", problems.join("; "));
        return Ok(Planned::Ended(engine.finish(
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
    Ok(Planned::Ready(plan))
}

/// Run the packages in topological order, then verify what they
/// assembled — with at most one bounded integration repair, from the
/// same aggregate budget (SPEC §19).
fn execute_waves(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    plan: &WorkPlan,
    integration: &workspace::TaskWorktree,
    assembly: &mut Assembly,
) -> Result<RunOutcome, RunError> {
    let contract = engine.config.contract;
    let order = plan
        .validate()
        .map_err(|e| RunError::Other(format!("a checked plan failed to validate: {e}")))?;

    for (index, wave) in &order {
        let package = &plan.packages[*index];
        match run_package(
            engine,
            &PackageRun {
                root,
                plan,
                package,
                wave: *wave,
                input_sha: &assembly.head.clone(),
            },
            assembly,
        )? {
            PackageEnd::Accepted(candidate) => match fast_forward(&integration.path, &candidate) {
                Ok(new_head) => assembly.head = new_head,
                Err(e) => {
                    let detail = e.to_string();
                    return engine.finish(
                            Reason::IntegrationConflict,
                            serde_json::json!({ "package": package.id, "detail": detail }),
                            Terminal::NeedsDecision {
                                reason: Reason::IntegrationConflict,
                                detail: format!(
                                    "package `{}` cannot be integrated: {detail}; its candidate is preserved",
                                    package.id
                                ),
                            },
                        );
                }
            },
            PackageEnd::Stop(outcome) => return Ok(outcome),
        }
    }

    // Independent final verification of the assembled candidate (SPEC
    // §19), against the ROOT contract's scope and profile.
    let mut integration_repairs: u32 = 0;
    loop {
        let budget = Budget {
            attempts_used: assembly.attempts_total,
            max_attempts: root.authority.max_attempts,
            repairs_used: 0,
            max_repairs: 0,
            tier: root.decision.tier,
            escalation_tier: None,
        };
        match workspace::check_scope(integration, &assembly.head, contract) {
            Ok(_) => {}
            Err(WorkspaceError::ScopeViolation(paths)) => {
                return engine.stop(&budget, Observation::ScopeViolation(paths));
            }
            Err(e) => {
                return engine.fail_preflight(BlockCode::ScopeCheckFailed, e.to_string());
            }
        }
        // Entering `verifying` is a transition like every other, so the
        // ledger shows the state it then records as the next row's
        // origin (R7).
        engine.transition(
            State::Verifying,
            Reason::VerificationStarted,
            serde_json::json!({
                "candidate": assembly.head,
                "integration_repairs": integration_repairs,
            }),
        )?;
        // Root verification waits for every relevant write lease: the
        // integration tree's and each package worker's (SPEC §23). The
        // packages ran to completion and gave their leases back with
        // their processes; a holder here is a straggler, and the
        // integrated candidate is not verified over it.
        let mut relevant = vec![integration.path.clone()];
        for child in engine.config.ledger.child_runs(&engine.run_id)? {
            relevant.push(
                crate::runner::worktree_root(
                    &engine
                        .artifacts
                        .join("packages")
                        .join(child.package.as_str()),
                )
                .join(child.run.as_str())
                .join("task"),
            );
        }
        for path in &relevant {
            if let Some(holder) = engine.wait_for_writers(path, root.deadline)? {
                return engine.stop_on_held_lease(path, &holder);
            }
        }
        let verify::Verified {
            checks,
            gaps,
            amont_bypasses,
            amont_downgrades,
        } = match engine.verify_candidate(
            &assembly.head,
            root.authority,
            root.logs_dir,
            AttemptLabel::Integration(integration_repairs),
            None,
        ) {
            Ok(result) => result,
            Err(e) => return engine.fail_preflight(BlockCode::VerificationUnavailable, e),
        };
        if !gaps.is_empty() {
            return engine.stop(&budget, Observation::VerificationGap(gaps));
        }
        let failures: Vec<String> = checks
            .iter()
            .filter(|check| check.failed())
            .map(|check| check.label.clone())
            .collect();
        if failures.is_empty() {
            let patch_path = engine.artifacts.join(INTEGRATED_PATCH);
            integration.export_patch(&assembly.head, &patch_path)?;
            let touched_inputs = verify::classify_verification_inputs(
                &root.authority.verification_profile,
                &integration.changed_paths_in(&assembly.head)?,
            )?;
            return accept_integrated(
                engine,
                root,
                integration,
                Assembled {
                    checks,
                    amont_bypasses,
                    amont_downgrades,
                    touched_inputs,
                    patch_path,
                },
                assembly,
            );
        }
        // One bounded integration repair from the assembled head, within
        // the same aggregate budget (SPEC §19). It is a package with the
        // whole contract scope and the integration acceptance.
        if integration_repairs >= 1 {
            let detail = format!(
                "the integrated candidate fails verification after one repair: {}",
                failures.join(", ")
            );
            return engine.finish(
                Reason::IntegrationFailed,
                serde_json::json!({ "failures": failures }),
                Terminal::Failed { detail },
            );
        }
        integration_repairs += 1;
        let repair = WorkPackage {
            id: "integration-repair".into(),
            objective: format!(
                "Integration of the work packages fails verification ({}); make the assembled \
                 change pass without changing what the packages delivered",
                failures.join(", ")
            ),
            write_scope: contract.scope_patterns().to_vec(),
            depends_on: plan.packages.iter().map(|p| p.id.clone()).collect(),
            acceptance: plan.integration_acceptance.clone(),
        };
        engine.transition(
            State::Repairing,
            Reason::BehavioralFailure,
            serde_json::json!({ "failures": failures, "package": repair.id }),
        )?;
        let wave = order.iter().map(|(_, wave)| *wave).max().unwrap_or(0) + 1;
        match run_package(
            engine,
            &PackageRun {
                root,
                plan,
                package: &repair,
                wave,
                input_sha: &assembly.head.clone(),
            },
            assembly,
        )? {
            PackageEnd::Accepted(candidate) => match fast_forward(&integration.path, &candidate) {
                Ok(new_head) => assembly.head = new_head,
                Err(e) => {
                    let detail = e.to_string();
                    return engine.finish(
                        Reason::IntegrationConflict,
                        serde_json::json!({ "package": repair.id, "detail": detail }),
                        Terminal::NeedsDecision {
                            reason: Reason::IntegrationConflict,
                            detail,
                        },
                    );
                }
            },
            PackageEnd::Stop(outcome) => return Ok(outcome),
        }
    }
}

enum PackageEnd {
    Accepted(String),
    Stop(RunOutcome),
}

/// One package's place in the run: which package, under which plan,
/// from which revision, in which wave.
struct PackageRun<'p> {
    root: &'p RootContext<'p>,
    plan: &'p WorkPlan,
    package: &'p WorkPackage,
    /// 0 = no dependencies; recorded so a concurrent executor can be
    /// dropped in behind the same validation and assembly.
    wave: u32,
    /// The revision this package's worktree starts from: the assembled
    /// head, so every package sees the packages before it.
    input_sha: &'p str,
}

/// Run one package as a child run from its input revision, under what
/// remains of the aggregate limits. The child's own lifecycle decides
/// its state; the root mirrors anything but acceptance.
fn run_package(
    engine: &mut RunEngine<'_>,
    run: &PackageRun<'_>,
    assembly: &mut Assembly,
) -> Result<PackageEnd, RunError> {
    let PackageRun {
        root,
        plan,
        package,
        wave,
        input_sha,
    } = *run;
    let ledger = engine.config.ledger;
    let contract = engine.config.contract;
    let budget = Budget {
        attempts_used: assembly.attempts_total,
        max_attempts: root.authority.max_attempts,
        repairs_used: 0,
        max_repairs: 0,
        tier: root.decision.tier,
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
    // Saturating money arithmetic, not `ceiling - spent` on raw
    // integers: a remainder that wrapped would read as a budget nothing
    // bounds (P7).
    let remaining_budget = engine
        .config
        .machine
        .spending
        .per_run_micros
        .map(|ceiling| ceiling.remaining_after(spent));
    if let Some(ceiling) = engine.config.machine.spending.per_run_micros {
        if remaining_budget.is_some_and(|remaining| remaining == MicroUsd::ZERO) {
            return Ok(PackageEnd::Stop(engine.stop(
                &budget,
                Observation::LimitReached(Limit::Spend {
                    spent: spent.to_string(),
                    ceiling: ceiling.to_string(),
                }),
            )?));
        }
    }
    let mut dispatched: u32 = ledger.dispatch_count(&engine.run_id)?;
    for child in ledger.child_runs(&engine.run_id)? {
        dispatched += ledger.dispatch_count(&child.run)?;
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
        // Through the constructor: the package scope is compiled and
        // judged here, before a worker is launched against it (P5).
        task: crate::contract::Task::change(package.write_scope.clone()).map_err(|e| {
            RunError::Other(format!(
                "work package `{}` declares a scope that cannot bound anything: {e}",
                package.id
            ))
        })?,
        objective: format!(
            "{}\n\n(work package `{}` of: {})",
            package.objective, package.id, contract.objective
        ),
        base_ref: input_sha.to_string(),
        read_hints: contract.read_hints.clone(),
        acceptance: package.acceptance.clone(),
        verification_profile: contract.verification_profile.clone(),
        architecture: contract.architecture.clone(),
        risk_hints: contract.risk_hints.clone(),
        limits: crate::contract::Limits {
            attempts: plan.limits.attempts_per_package,
            wall_seconds: remaining_wall.as_secs().max(1),
        },
        // The root's review setting, as written: a package is reviewed
        // exactly as the contract asks the run to be.
        review: contract.review,
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
        ids: engine.config.ids,
        repo_dir: engine.config.repo_dir,
        contract: &child_contract,
        repo_policy: engine.config.repo_policy,
        machine: &child_machine,
        ledger,
        backend: engine.config.backend,
        git: engine.config.git,
        hooks: engine.config.hooks,
        worker_env: engine.config.worker_env.clone(),
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
    let outcome = execute_child(
        &child_config,
        &engine.run_id,
        &crate::ids::PackageId::from_stored(package.id.clone()),
    )?;
    let child_run = outcome.run_id.clone();
    assembly.attempts_total += ledger.attempt_count(&child_run)? as u32;
    for model in ledger.models_used(&child_run)? {
        if !assembly.models_used.contains(&model) {
            assembly.models_used.push(model);
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
fn fast_forward(integration_path: &Path, candidate_sha: &str) -> Result<String, WorkspaceError> {
    // `workspace::git_command` and not `Command::new("git")`: an
    // inherited GIT_DIR or GIT_INDEX_FILE would merge into a different
    // repository than the one this path names (audit B15).
    let merge = workspace::git_command(integration_path)
        .args(["merge", "--ff-only", candidate_sha])
        .output()
        .map_err(|e| WorkspaceError::Git(format!("merge --ff-only: {e}")))?;
    if !merge.status.success() {
        return Err(WorkspaceError::Git(format!(
            "not a fast-forward of the integration head: {}",
            String::from_utf8_lossy(&merge.stderr).trim()
        )));
    }
    // The new head is the answer this function exists to give: a
    // `rev-parse` whose status went unread returned `Ok("")`, and the
    // empty string then travelled on as the integrated revision (R5).
    let head = workspace::git_command(integration_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| WorkspaceError::Git(format!("rev-parse HEAD: {e}")))?;
    if !head.status.success() {
        return Err(WorkspaceError::Git(format!(
            "the integration head could not be read after the merge: {}",
            String::from_utf8_lossy(&head.stderr).trim()
        )));
    }
    let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(WorkspaceError::Git(
            "`rev-parse HEAD` named no revision in the integration worktree".to_string(),
        ));
    }
    Ok(sha)
}

/// What the integrated candidate's verification established.
struct Assembled {
    checks: Vec<crate::verify::CheckOutcome>,
    amont_bypasses: Vec<String>,
    amont_downgrades: Vec<String>,
    touched_inputs: verify::TouchedInputs,
    /// The exported patch of the assembled revision — the file a
    /// reviewer is told to read, and one of the two places an accepted
    /// run's content lives.
    patch_path: PathBuf,
}

/// The assembled candidate passed the profile's checks: review it where
/// the risk asks for it, then seal the root's receipt (SPEC §19).
fn accept_integrated(
    engine: &mut RunEngine<'_>,
    root: &RootContext<'_>,
    integration: &workspace::TaskWorktree,
    assembled: Assembled,
    assembly: &mut Assembly,
) -> Result<RunOutcome, RunError> {
    let ledger = engine.config.ledger;
    let head = assembly.head.clone();
    let budget = Budget {
        attempts_used: assembly.attempts_total,
        max_attempts: root.authority.max_attempts,
        repairs_used: 0,
        max_repairs: 0,
        tier: root.decision.tier,
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
    // Every model the packages ran is billed by this receipt, so every
    // model the packages ran is named by it — including the planner's,
    // which is a dispatch of the ROOT run and used to be folded in only
    // when a review happened to be required (R9).
    for model in ledger.models_used(&engine.run_id)? {
        if !assembly.models_used.contains(&model) {
            assembly.models_used.push(model);
        }
    }
    // The run's real spend, read from the ledger: the reviewer is a
    // dispatch like any other and the ceiling that stopped the packages
    // stops it too. Handing `review_candidate` a fresh zero let a run
    // spend its whole ceiling again on the review (R1).
    let mut spend = crate::runner::RunSpend {
        total: ledger.run_cost(&engine.run_id)?,
        completeness: ledger.run_cost_completeness(&engine.run_id)?,
    };
    let verification_inputs_changed = assembled.touched_inputs.tests;
    let review_required =
        root.decision.review >= Review::Required || !verification_inputs_changed.is_empty();
    if review_required {
        let review = engine.review_candidate(
            &crate::runner::ReviewRequest {
                manifest: root.manifest,
                authority: root.authority,
                candidate_sha: &head,
                candidate_tier: root.decision.tier,
                verification_inputs_changed: &verification_inputs_changed,
                // The file this path names is the one the assembled
                // candidate was exported to a moment ago; the reviewer
                // used to be sent to the single-worker path's
                // `candidate-latest.patch`, which a decomposed run never
                // writes (R4).
                patch_path: assembled.patch_path.clone(),
                deadline: root.deadline,
            },
            &mut spend,
        );
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
        candidate_sha: head.clone(),
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
        baseline_cache_refused: root.baseline_cache_refused.clone(),
    };
    let receipt = Receipt {
        run_id: engine.run_id.as_str().to_string(),
        candidate_sha: head.clone(),
        base_sha: root.base_sha.to_string(),
        contract_hash: root.contract_hash.to_string(),
        policy_hash: root.authority.authority_hash.clone(),
        outcome: State::Accepted.as_str().to_string(),
        verification: report,
        models_used: std::mem::take(&mut assembly.models_used),
        attempts: assembly.attempts_total,
        cost_completeness: spend.completeness,
        cost: spend.total,
    };
    engine.seal(&receipt, None, None, &head)?;
    // The integration worktree goes the way an accepted single-worker
    // run's does: the assembled revision is in the patch and under a
    // ref, so the directory adds nothing (R8). A ref that could not be
    // written keeps the worktree, because the commit would then have
    // nothing else holding it.
    let reference = workspace::name_candidate(
        engine.config.repo_dir,
        engine.run_id.as_str(),
        INTEGRATION_ATTEMPT,
        &head,
    )
    .ok();
    engine.release_exported_worktree(
        integration,
        crate::runner::TreeIdentity::CheckedOut,
        &head,
        &assembled.patch_path,
        reference.as_deref(),
    )?;
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
    prompt.push_str(&format!("write_scope: {:?}\n", contract.scope_patterns()));
    prompt.push_str(&data_list_block("acceptance", &contract.acceptance));
    if !root.manifest.constraints.is_empty() {
        prompt.push_str(&data_list_block("constraints", &root.manifest.constraints));
    }
    let dispatch_id = engine.config.ids.dispatch_id()?;
    let spent = engine.config.ledger.run_cost(&engine.run_id)?;
    let remaining_budget = engine
        .config
        .machine
        .spending
        .per_run_micros
        .map(|ceiling| ceiling.remaining_after(spent).to_micros());
    engine.config.ledger.record_dispatch_intent(
        &dispatch_id,
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
        env: engine.config.worker_env.clone(),
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
    let result = match engine.managed_launch(ManagedDispatch {
        spec,
        depth: 0,
        parent: None,
        reserve_micros: remaining_budget.unwrap_or(0),
        deadline: root.deadline,
        budget: &budget,
        // The planner reads; it writes no worktree and takes no lease.
        write_lease: None,
    })? {
        Ok(result) => result,
        Err(outcome) => {
            engine
                .config
                .ledger
                .finish_dispatch(&dispatch_id, "launch_failed")?;
            return Ok(Proposal::Failed(outcome));
        }
    };
    engine
        .config
        .ledger
        .finish_dispatch(&dispatch_id, "completed")?;
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

    /// R5: the integration head is the answer `fast_forward` exists to
    /// give. Both git calls it makes are checked, and a failure is a
    /// typed error carrying what git said — never an `Ok("")` that
    /// travels on as a revision.
    #[test]
    fn a_failing_fast_forward_is_a_typed_git_error() {
        let dir = crate::test_support::temp_dir("ff");
        let err = fast_forward(&dir, "0000000000000000000000000000000000000000")
            .expect_err("a directory that is not a repository");
        let WorkspaceError::Git(detail) = &err else {
            panic!("a git failure is a git error, got {err:?}");
        };
        assert!(!detail.is_empty(), "git's own message is carried");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// R11: `packages x attempts_per_package` used to be an unchecked
    /// multiplication of two contract-supplied numbers.
    #[test]
    fn the_worst_case_dispatch_count_cannot_wrap() {
        let package = |id: &str| WorkPackage {
            id: id.into(),
            objective: "do the thing".into(),
            write_scope: vec!["src/**".into()],
            depends_on: Vec::new(),
            acceptance: vec!["it builds".into()],
        };
        let plan = WorkPlan {
            packages: vec![package("a"), package("b")],
            integration_acceptance: vec!["it all builds".into()],
            limits: crate::contract::PlanLimits {
                max_packages: 4,
                attempts_per_package: u32::MAX,
            },
        };
        let contract = TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": "o",
                "base_ref": "HEAD",
                "write_scope": ["src/**"],
                "acceptance": ["a"],
                "verification_profile": "p",
            })
            .to_string(),
        )
        .expect("contract");
        let authority = EffectiveAuthority {
            models: std::collections::BTreeMap::new(),
            max_attempts: 3,
            max_wall_seconds: 60,
            max_repairs_before_escalation: 1,
            allow_nested_agents: false,
            max_agent_depth: 1,
            max_agents_total: 24,
            verification_profile: crate::policy::VerificationProfile::default(),
            review_floor: Review::Off,
            disallowed_tools: Vec::new(),
            allowed_tools: Vec::new(),
            authority_hash: "hash".into(),
            grant_key: "key".into(),
            trust_granted: true,
            blockers: Vec::new(),
        };
        // The shape check refuses the plan before the multiplication,
        // and the message names the limit rather than reporting a
        // wrapped product as comfortably within the cap.
        let problems = check_plan(&plan, &contract, &authority);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("attempts_per_package"), "{problems:?}");
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
