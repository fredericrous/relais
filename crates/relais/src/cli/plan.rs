//! `relais plan`.

use super::ctx::load_contract;
use super::ctx::load_machine;
use super::ctx::load_repo_policy;
use super::ctx::open_ledger;
use super::ctx::resolve_task_override;
use super::learn::harness_identity;
use super::learn::learned_registry;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::recipe::missing_grant_text;
use relais::contract::TaskContract;
use relais::ids::TaskId;
use relais::learn::predict::RegistryPredictor;
use relais::ledger::Ledger;
use relais::paths;
use relais::policy::effective_authority;
use relais::policy::EffectiveAuthority;
use relais::policy::MachineSettings;
use relais::policy::RepoIdentity;
use relais::policy::RepoPolicy;
use relais::route;
use relais::runner::live_trial;
use relais::workspace;
use std::path::Path;
/// A change to the repository's commands that no `relais run` has said
/// yet, read without consuming it (`plan`, `trust show`): from the ledger
/// when there is one, nothing otherwise. Best effort — a ledger that
/// cannot be read costs the report, never the command, and is said on
/// stderr under `command`'s name.
pub(crate) fn pending_command_change(
    command: &str,
    repo_key: &str,
    policy: &RepoPolicy,
) -> Option<relais::trust::CommandsChanged> {
    let ledger = match paths::ledger_path()
        .ok()
        .filter(|path| path.exists())
        .map(|path| Ledger::open(&path))
        .transpose()
    {
        Ok(ledger) => ledger,
        Err(e) => {
            eprintln!(
                "relais {command}: warning: the ledger could not be opened ({e}); a pending \
                 change to the commands is not reported"
            );
            return None;
        }
    };
    let seen = match &ledger {
        Some(ledger) => relais::trust::observe(&relais::trust::Reader(ledger), repo_key, policy),
        None => relais::trust::observe(&relais::trust::NoHistory, repo_key, policy),
    };
    match seen {
        Ok(change) => change,
        Err(e) => {
            eprintln!(
                "relais {command}: warning: the commands this repository last ran under could \
                 not be read ({e}); a pending change to them is not reported"
            );
            None
        }
    }
}

/// How `plan` prints its decision.
#[derive(Clone, Copy)]
pub(crate) enum PlanFormat {
    Text,
    Json,
}

/// `plan --json`: the decision as one JSON object — the route with its
/// rung, or every blocker with its code.
pub(crate) fn plan_json(
    decision: &route::Routed,
    authority: &EffectiveAuthority,
    contract: &TaskContract,
) -> Result<CliOutcome, CliError> {
    let (value, outcome) = match decision {
        route::Routed::Route(routed) => (
            serde_json::json!({
                "contract_hash": contract.hash(),
                "policy_hash": authority.authority_hash,
                "route": {
                    "tier": routed.tier.as_str(),
                    "rung": routed.rung,
                    "ladder": routed.ladder,
                    "routed_by": routed.routed_by.as_str(),
                    "escalation_tier": routed.escalation_tier.map(|tier| tier.as_str()),
                    "reasons": routed.reasons.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
                },
            }),
            CliOutcome::Accepted,
        ),
        route::Routed::Blocked(blocked) => (
            serde_json::json!({
                "contract_hash": contract.hash(),
                "policy_hash": authority.authority_hash,
                "blocked": blocked
                    .blockers()
                    .iter()
                    .map(|b| serde_json::json!({"code": b.code.as_str(), "detail": b.detail}))
                    .collect::<Vec<_>>(),
            }),
            CliOutcome::Blocked,
        ),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&value).expect("a plan decision serializes")
    );
    Ok(outcome)
}

pub(crate) fn plan_command(
    task: &Path,
    revise: Option<&str>,
    format: PlanFormat,
) -> Result<CliOutcome, CliError> {
    let (root, repo) = load_repo_policy()?;
    let machine = load_machine()?;
    let contract = load_contract(task)?;
    // Only a contract that NAMES a task needs the ledger, and `plan`
    // writes nothing: opening it unconditionally would create the state
    // directory, the database and its migrations as a side effect of a
    // preflight whose whole promise is that nothing happens yet.
    let mut task_override = None;
    if contract.task_id.is_some() || revise.is_some() {
        let ledger = open_ledger()?;
        match resolve_task_override(&ledger, &contract, revise) {
            Ok(resolved) => task_override = resolved,
            Err(detail) => {
                eprintln!("relais plan: {detail}");
                return Ok(CliOutcome::Blocked);
            }
        }
    }

    // Matched, not `if let Ok`: a `git status` that FAILS is not a clean
    // tree, and `run` has blocked on exactly this since v0.1.3 (C6).
    match workspace::dirty_paths(&root) {
        Ok(dirty) if dirty.is_empty() => {}
        Ok(dirty) => {
            eprintln!(
                "relais plan: working tree is dirty ({}); commit or stash first",
                dirty.join(", ")
            );
            return Ok(CliOutcome::Blocked);
        }
        Err(e) => {
            eprintln!(
                "relais plan: blocked (dirty_base): the working tree of {} could not be read: {e}",
                root.display()
            );
            return Ok(CliOutcome::Blocked);
        }
    }
    let base_sha = match workspace::resolve_base(&root, &contract.base_ref) {
        Ok(sha) => sha,
        Err(e) => {
            eprintln!(
                "relais plan: cannot resolve base_ref {}: {e}",
                contract.base_ref
            );
            return Ok(CliOutcome::Blocked);
        }
    };
    // The same check `run` performs in `assemble_context`: a read hint
    // that does not resolve at the base revision points the worker at a
    // path this tree does not have, and `plan` should refuse it exactly
    // as `run` does rather than route a candidate `run` would then block.
    if let Err(unresolvable) = relais::context::fingerprint_hints(
        &relais::workspace::SystemGit,
        &root,
        &base_sha,
        &contract.read_hints,
    ) {
        eprintln!(
            "relais plan: blocked (read_hint_unresolvable): {unresolvable} (base {base_sha})"
        );
        return Ok(CliOutcome::Blocked);
    }
    // And the design: `run` refuses a contract whose design is missing,
    // oversized or not text at the base revision, so `plan` does too.
    if let Some(design) = &contract.design {
        if let Err(e) =
            relais::context::read_design(&relais::workspace::SystemGit, &root, &base_sha, design)
        {
            eprintln!("relais plan: blocked (design_unreadable): {e} (base {base_sha})");
            return Ok(CliOutcome::Blocked);
        }
    }
    // And the holdouts: `run` refuses a holdout criterion it cannot
    // resolve, with the same codes.
    if let Err(block) = relais::holdout::resolve_all(
        &contract.acceptance,
        &machine.holdouts,
        &relais::repo::main_checkout_name(&root).unwrap_or_default(),
        &relais::workspace::SystemGit,
        &root,
        &base_sha,
        &relais::holdout::machine_toml_label(),
    ) {
        eprintln!(
            "relais plan: blocked ({}): {} (base {base_sha})",
            block.code, block.detail
        );
        return Ok(CliOutcome::Blocked);
    }
    let repo_identity = relais::repo::identity(&root);
    let authority = effective_authority(&repo, &machine, &contract, &repo_identity);
    let probe = harness_identity();
    let harness = probe.identity;
    let registry = learned_registry(&machine);
    let predictor = registry
        .as_ref()
        .map(|registry| RegistryPredictor::new(registry, &repo, harness.as_deref()));
    let catalogs = route::resolve_catalogs(&repo, &authority, &machine, probe.efforts.as_ref());
    let task_type = match (&task_override, contract.task_type) {
        (_, Some(declared)) => Some(declared),
        (Some(task), None) => operational(open_ledger()?.routing_type(task, None), "plan")?,
        (None, None) => None,
    };
    let decision = route::route(route::RouteInputs {
        draw_key: None,
        task_type,
        has_design: contract.design.is_some(),
        admission: route::Admission::Live,
        contract: &contract,
        repo: &repo,
        machine: &machine,
        authority: &authority,
        predictor: predictor
            .as_ref()
            .map(|predictor| predictor as &dyn route::RoutePredictor),
        catalogs: &catalogs,
    });
    if let PlanFormat::Json = format {
        return plan_json(&decision, &authority, &contract);
    }
    let session = relais::coordinator::resolve_session();
    println!("contract hash: {}", contract.hash());
    println!("policy hash: {}", authority.authority_hash);
    println!("repository: {repo_identity}");
    println!("base: {} ({})", base_sha, contract.base_ref);
    // Stderr, not stdout: `plan`'s stdout stays what it was (see below),
    // and session attribution is not part of that contract.
    eprintln!("session: {} ({})", session.id, session.source);
    if session.source.is_fallback() {
        eprintln!(
            "relais plan: warning: no CLAUDE_CODE_SESSION_ID or RELAIS_SESSION_ID in the \
             environment; attributing this call to {}",
            session.source
        );
    }
    // The same line `relais doctor` prints, on stderr so the plan's
    // stdout stays what it was: a lockfile with no setup declared is
    // worth knowing before a run spends a baseline on exit 127.
    if let Some(finding) = relais::doctor::setup_finding(&repo, &relais::repo::lockfiles(&root)) {
        if finding.level == relais::doctor::Level::Warn {
            eprintln!("relais plan: warning: {}", finding.detail);
        }
    }
    match authority.trust_status {
        relais::policy::TrustStatus::Repository => {
            println!(
                "trust grant: repository {} (in machine.toml)",
                relais::policy::repo_key(&repo_identity)
            );
        }
        relais::policy::TrustStatus::LegacyExact => {
            println!("trust grant: {} (in machine.toml)", authority.grant_key);
        }
        relais::policy::TrustStatus::Missing => {
            println!(
                "{}",
                missing_grant_text(&authority.grant_key, &repo_identity)?
            );
        }
    }
    // A pending change to the commands is reported here and consumed only
    // by `run` (ADR-0007). On stderr: `plan`'s stdout stays what it was.
    if let Some(change) =
        pending_command_change("plan", &relais::policy::repo_key(&repo_identity), &repo)
    {
        eprintln!("{}", change.text());
    }
    match &decision {
        route::Routed::Route(routed) => {
            print!("{}", routed.explain());
            // Stderr, like the other advisories: the route line and the
            // ladder on stdout are unchanged by holding the effort.
            for model in routed.ladder.held() {
                eprintln!("{}", route::held_text(model));
            }
            for line in plan_trial_lines(
                &root,
                &repo,
                &machine,
                &repo_identity,
                &contract,
                task_override,
            )? {
                println!("{line}");
            }
            Ok(CliOutcome::Accepted)
        }
        route::Routed::Blocked(blocked) => {
            // `explain` already lists every blocker with its code; this
            // used to print them a second time on stderr, so a plan
            // blocked by one thing reported it twice.
            print!("{}", blocked.explain());
            Ok(CliOutcome::Blocked)
        }
    }
}

/// The task identity a run of this contract will have: the one already
/// confirmed on record, or the one derived from the repository and the
/// contract, exactly as the runner derives it at preflight.
pub(crate) fn task_identity(
    task_override: Option<TaskId>,
    repo_identity: &RepoIdentity,
    contract: &TaskContract,
) -> TaskId {
    task_override.unwrap_or_else(|| {
        relais::ids::derive_task_id(&relais::policy::repo_key(repo_identity), &contract.hash())
    })
}

/// What `relais plan` prints about live trials (SPEC §28): the assignment
/// it WOULD make, and every candidate dropped and why. Nothing when the
/// envelope is off, so plan's output is unchanged. Writes nothing: with no
/// ledger on disk yet it does not create one to learn that today's usage
/// is zero.
pub(crate) fn plan_trial_lines(
    root: &Path,
    repo: &RepoPolicy,
    machine: &MachineSettings,
    repo_identity: &RepoIdentity,
    contract: &TaskContract,
    task_override: Option<TaskId>,
) -> Result<Vec<String>, CliError> {
    if !machine.trials.enabled {
        return Ok(Vec::new());
    }
    let ledger_path = paths::ledger_path().map_err(CliError::Home)?;
    let ledger = if ledger_path.exists() {
        Some(open_ledger()?)
    } else {
        None
    };
    let task_id = task_identity(task_override, repo_identity, contract);
    let decision = operational(
        live_trial::decide(&live_trial::Environment {
            root,
            repo,
            machine,
            identity: repo_identity,
            contract,
            task_id: &task_id,
            ledger: ledger.as_ref(),
        }),
        "plan trial assignment",
    )?;
    Ok(decision.lines())
}
