//! `relais dataset replay`.

use super::ctx::id_source;
use super::ctx::load_machine;
use super::ctx::load_price_table;
use super::ctx::load_repo_policy;
use super::ctx::open_ledger;
use super::ctx::pricing_preflight;
use super::ctx::tuning_bounds;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::run::run_refusal;
use relais::contract::TaskContract;
use relais::ids::TaskId;
use relais::learn::typed_gate;
use relais::lifecycle::RunPurpose;
use relais::paths;
use relais::policy::RecipeSpec;
use relais::policy::RepoPolicy;
use relais::route;
use relais::runner::execute;
use relais::runner::live_trial;
use relais::runner::worktree_root;
use relais::runner::RunConfig;
use relais::runner::State;
use relais::workspace;
use std::path::Path;
/// The `done` line of a replay, sent once its trial is recorded (or could
/// not be): the plugin's verdict names the trial, and never precedes it.
pub(crate) fn announce_replay_done(
    artifacts_dir: &Path,
    run: &str,
    outcome: &str,
    trial: Option<&str>,
) {
    let artifacts = artifacts_dir.join(run);
    let written = artifacts.join("receipt.json");
    let receipt = written
        .is_file()
        .then(|| written.to_string_lossy().into_owned());
    // Dropped on failure: the plugin that was reading is gone, and the
    // replay's result is in the ledger.
    let _ = relais::protocol::Wire::process().send(&relais::protocol::Request::Done {
        run: Some(run),
        outcome,
        receipt: receipt.as_deref(),
        summary: relais::protocol::Summary::of_candidate(&artifacts),
        trial,
        code: None,
        detail: None,
    });
}

pub(crate) fn replay_command(
    task: &str,
    recipe: &Path,
    dry_run: bool,
    design: Option<&Path>,
    origin: &relais::native::RunOrigin<'_>,
) -> Result<CliOutcome, CliError> {
    // The design file first, before anything else is read or run: a
    // missing or oversized one refuses the replay outright.
    let given_design = match design.map(relais::context::read_design_file).transpose() {
        Ok(given) => given,
        Err(e) => {
            eprintln!("relais dataset replay: refused (design_unreadable): {e}");
            return Ok(CliOutcome::InvalidInput);
        }
    };
    // A dry run spends nothing and is allowed anywhere; a spending replay
    // starts only the way a run does.
    if !dry_run {
        if let Some(detail) = relais::native::origin_refusal(origin) {
            return Err(CliError::Usage { detail });
        }
    }
    let (root, incumbent) = load_repo_policy()?;
    let candidate_text = std::fs::read_to_string(recipe).map_err(|cause| CliError::Read {
        what: "the candidate recipe",
        path: recipe.to_path_buf(),
        cause,
    })?;
    let candidate_policy =
        RepoPolicy::from_toml_str(&candidate_text).map_err(|cause| CliError::Invalid {
            what: "the candidate recipe",
            path: recipe.to_path_buf(),
            cause: Box::new(cause),
        })?;
    // Admitted through the same door a learner's proposal is — never a
    // second, looser one for replay.
    let bounds = tuning_bounds(&incumbent);
    let candidate = match route::validate_candidate(
        &incumbent,
        &candidate_policy,
        &bounds,
        route::CandidateAdmission::Replay,
    ) {
        Ok(candidate) => candidate,
        Err(rejection) => {
            eprintln!("relais dataset replay: candidate refused: {rejection}");
            return Ok(CliOutcome::Blocked);
        }
    };

    let ledger = open_ledger()?;
    let task_id = TaskId::from_stored(task);
    let runs = operational(ledger.runs_of_task(&task_id), "dataset replay")?;
    // `runs_of_task` comes back oldest-first, so `.rev()` walks newest
    // first — and needs the `break` it was missing: without one the
    // assignment kept overwriting and the EARLIEST accepted run won,
    // the opposite of what reversing the list was for. An older receipt
    // is also likelier to carry an empty verification profile hash, so
    // the bug made the comparability refusal fire more often too.
    //
    // A replay is never its own source. Replay runs execute under the
    // task's own id, so an accepted replay would otherwise be eligible as
    // the "accepted run" of the next replay, and an arm would be compared
    // against another arm instead of against the work that actually
    // shipped.
    let mut source_run = None;
    for run in runs.into_iter().rev() {
        // Exactly `Accepted` (SPEC §24) — not `AcceptedByPerson`: a
        // person's salvage never went through the runner's own
        // verification, so there is no receipt with a verification
        // profile hash to check a replay's comparability against.
        if operational(ledger.run_status(&run), "dataset replay")? != Some(State::Accepted) {
            continue;
        }
        if operational(ledger.run_purpose(&run), "dataset replay")?.is_some() {
            continue;
        }
        source_run = Some(run);
        break;
    }
    let Some(source_run) = source_run else {
        eprintln!("relais dataset replay: task {task_id} has no accepted run on record");
        return Ok(CliOutcome::UnknownRun);
    };

    let Some((source_contract, _tier)) =
        operational(ledger.run_contract_and_tier(&source_run), "dataset replay")?
    else {
        eprintln!("relais dataset replay: run {source_run} carries no recorded contract");
        return Ok(CliOutcome::OperationalFailure);
    };
    let Some((receipt, _hash)) = operational(ledger.receipt(&source_run), "dataset replay")? else {
        eprintln!("relais dataset replay: run {source_run} has no receipt to replay from");
        return Ok(CliOutcome::OperationalFailure);
    };
    let (Some(base_sha), Some(source_profile_hash), Some(contract_hash)) = (
        receipt["base_sha"].as_str(),
        receipt["verification_profile_hash"].as_str(),
        receipt["contract_hash"].as_str(),
    ) else {
        eprintln!("relais dataset replay: run {source_run}'s receipt is missing a base SHA, a contract hash, or a verification profile hash");
        return Ok(CliOutcome::OperationalFailure);
    };
    let base_sha = base_sha.to_string();
    let source_profile_hash = source_profile_hash.to_string();
    let contract_hash = contract_hash.to_string();
    // The design the arm is handed: `--design`, else whatever the source
    // contract named, read here at its base so routing, the package and
    // the trial row all see the one design.
    let design_input = match (given_design, &source_contract.design) {
        (Some(given), _) => Some(given),
        (None, None) => None,
        (None, Some(path)) => {
            match relais::context::read_design(
                &relais::workspace::SystemGit,
                &root,
                &base_sha,
                path,
            ) {
                Ok(read) => Some(read),
                Err(e) => {
                    eprintln!("relais dataset replay: blocked (design_unreadable): {e}");
                    return Ok(CliOutcome::Blocked);
                }
            }
        }
    };
    let design_sha256 = design_input
        .as_ref()
        .map(|design| relais::ids::sha256_hex(design.text.as_bytes()));

    // The profile hash that would judge THIS replay: the candidate's own
    // verification is byte-identical to the incumbent's
    // (`fixed_fields_match` never lets it move), so this is really asking
    // whether the REPOSITORY's verification has moved since the source run
    // — a comparison judged by different checks is not a comparison.
    let profile_name = &source_contract.verification_profile;
    let Some(profile) = candidate.policy().verification.profiles.get(profile_name) else {
        eprintln!(
            "relais dataset replay: verification profile `{profile_name}` is not declared in \
             the candidate policy"
        );
        return Ok(CliOutcome::InvalidInput);
    };
    let replay_profile_hash = profile.hash();
    if replay_profile_hash != source_profile_hash {
        eprintln!(
            "relais dataset replay: verification profile hash mismatch — the source run {} was \
             judged by `{source_profile_hash}`, but a replay would be judged by \
             `{replay_profile_hash}`; a comparison judged by different checks is not a \
             comparison",
            source_run
        );
        return Ok(CliOutcome::Blocked);
    }

    // The recorded contract carries no type; the ledger's (declared or
    // classified) one comes in through the facts.
    let task_type = operational(
        ledger.routing_type(&task_id, source_contract.task_type),
        "dataset replay",
    )?;
    // The incumbent is what routed the source run, with its own design;
    // the arm is routed with the design it is handed.
    let incumbent_facts = route::TaskFacts {
        task_type,
        has_design: source_contract.design.is_some(),
        admission: route::Admission::Live,
    };
    let replay_facts = route::TaskFacts {
        has_design: design_input.is_some(),
        admission: route::Admission::Replay,
        ..incumbent_facts
    };
    let incumbent_eligible = route::eligible_tiers(
        &source_contract,
        &incumbent,
        &incumbent.models,
        incumbent_facts,
    );
    let incumbent_recipe_id = route::covering_recipe_id(
        &source_contract,
        &incumbent,
        &incumbent_eligible.tiers,
        incumbent_facts,
    )
    .unwrap_or_default();
    let candidate_eligible = route::eligible_tiers(
        &source_contract,
        candidate.policy(),
        &candidate.policy().models,
        replay_facts,
    );
    let arm_recipe = route::covering_recipe_id(
        &source_contract,
        candidate.policy(),
        &candidate_eligible.tiers,
        replay_facts,
    );

    println!(
        "this replay is one arm's result; it does not compare, score, rank or promote anything"
    );
    let arm = arm_recipe
        .as_deref()
        .and_then(|id| ReplayArm::of(candidate.policy(), id));
    if dry_run {
        println!("dry run: nothing was dispatched, no trial was recorded, no usage was spent");
        println!("would replay source run: {source_run} (task {task_id})");
        println!(
            "task type: {}",
            task_type.map_or("none", relais::contract::TaskType::as_str)
        );
        println!(
            "candidate recipe: {}",
            arm_recipe
                .as_deref()
                .unwrap_or("(no recipe covers this task under the candidate)")
        );
        if let Some(arm) = &arm {
            println!(
                "arm: {} revision {}, tier {}, model {}",
                arm.spec.name,
                arm.spec.revision,
                arm.spec.tier.as_str(),
                arm.model.unwrap_or("(none configured)")
            );
        }
        println!("design: {}", design_sha256.as_deref().unwrap_or("none"));
        println!("verification profile hash checked: {replay_profile_hash}");
        return Ok(CliOutcome::Accepted);
    }
    let (Some(arm_recipe), Some(arm)) = (arm_recipe, arm) else {
        eprintln!("relais dataset replay: no recipe in the candidate policy covers task {task_id}");
        return Ok(CliOutcome::Blocked);
    };

    let machine = load_machine()?;
    let artifacts_dir = paths::runs_dir().map_err(CliError::Home)?;
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => std::sync::Arc::from(backend),
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::Blocked);
        }
    };
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    if let Err(e) = relais::coordinator::ensure_running(&socket) {
        eprintln!("relais dataset replay: blocked (admission_unavailable): {e}");
        return Ok(CliOutcome::Blocked);
    }
    let gate = relais::coordinator::RemoteGate::new(socket);
    let session = relais::coordinator::resolve_session();
    // Before the checkout and before anything is dispatched: the plugin of
    // this session is alive, on a Claude Code it supports.
    let report = backend.probe_report();
    let installed = report
        .as_ref()
        .ok()
        .and_then(|capabilities| capabilities.version.as_deref());
    match run_refusal(
        "relais dataset replay",
        &gate,
        &session.id,
        installed,
        report.as_ref().err(),
    ) {
        Ok(None) => {}
        Ok(Some(detail)) => return Err(CliError::Usage { detail }),
        Err(e) => {
            eprintln!("relais dataset replay: blocked (admission_unavailable): {e}");
            return Ok(CliOutcome::Blocked);
        }
    }
    let ids = id_source();
    let run_id = match ids.run_id() {
        Ok(run_id) => run_id,
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    let replay_id = match ids.mint_replay_trial() {
        Ok(id) => id,
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };

    // The isolated workspace (SPEC §24): a fresh checkout at the source
    // run's base SHA, never a worktree of the live repository — so the
    // accepted answer this task already earned is not merely hidden from
    // the tree, it was never fetched into this repository's object store.
    let checkout_dir = worktree_root(&artifacts_dir)
        .join("replay")
        .join(replay_id.as_str());
    if let Err(e) = workspace::create_replay_checkout(&root, &base_sha, &checkout_dir) {
        eprintln!("relais dataset replay: {e}");
        return Ok(CliOutcome::OperationalFailure);
    }
    let replay_contract = TaskContract {
        base_ref: base_sha.clone(),
        ..source_contract.clone()
    };

    let git = relais::workspace::SystemGit;
    let aval_resolver = relais::context::AvalCli::new(checkout_dir.clone());
    let hooks = relais::verify::AmontCli::new();
    let attest = relais::verify::AmontCli::new();
    let prices = load_price_table()?;
    if let Some(blocked) = pricing_preflight(
        "relais dataset replay",
        candidate.policy(),
        &prices,
        &ledger,
    )? {
        return Ok(blocked);
    }
    let native_backend = relais::adapter::native::NativeBackend::new(
        backend.as_ref(),
        &gate,
        relais::adapter::native::Link {
            run_id: run_id.as_str().to_string(),
            session_id: session.id.clone(),
            wire: relais::protocol::Wire::process(),
            projects_dir: paths::claude_projects_dir().ok(),
        },
        relais::adapter::native::Waits::DEFAULT,
        Some(prices),
    );
    // The advisor calls an agent's transcript holds are booked into the
    // ledger beside the plugin's figures, which never contain them.
    let native_backend = match paths::ledger_path() {
        Ok(path) => native_backend.with_advisor_ledger(path),
        Err(e) => {
            eprintln!("advisor usage will not be booked: the ledger path is unknown: {e}");
            native_backend
        }
    };
    // Same placement as `relais run`: before `execute`, so it is visible
    // on every path rather than deferred to the end of a run that spends
    // real money and can take minutes.
    eprintln!("session: {} ({})", session.id, session.source);
    if session.source.is_fallback() {
        eprintln!(
            "relais dataset replay: warning: no CLAUDE_CODE_SESSION_ID or RELAIS_SESSION_ID in \
             the environment; attributing this replay to {}",
            session.source
        );
    }
    let log = ReplayLog {
        task_type,
        task: &task_id,
        arm_model: arm.model.unwrap_or("none"),
        load: relais::procs::load_average(),
        started: std::time::Instant::now(),
    };
    eprintln!("{}", log.start_line());
    let outcome = execute(&RunConfig {
        workspace: relais::workspace::reuse::Workspace::Isolated,
        repo_dir: &checkout_dir,
        contract: &replay_contract,
        repo_policy: candidate.policy(),
        machine: &machine,
        ledger: &ledger,
        ids: &ids,
        backend: &native_backend,
        git: &git,
        hooks: &hooks,
        attest: &attest,
        artifacts_dir: artifacts_dir.clone(),
        aval_resolver: &aval_resolver,
        predictor: None,
        gate: Some(&gate),
        session_id: session.id.clone(),
        heartbeat_every: std::time::Duration::from_secs(30),
        task_override: Some(&task_id),
        purpose: Some(RunPurpose::Replay),
        run_id: Some(run_id.clone()),
        wire: relais::protocol::Wire::process(),
        design: design_input
            .as_ref()
            .map_or(relais::runner::DesignSource::Contract, |design| {
                relais::runner::DesignSource::Given(design)
            }),
        // A replay neither says nor records a pending command change: the
        // runner skips it for a replay, and `plan`, `doctor` and `trust
        // show` report it.
        declarations: &relais::trust::NoHistory,
    });
    // Bring the replay's own refs into the live repository BEFORE the
    // checkout goes. The runner names its candidate and snapshot refs in
    // `repo_dir`, which here is the scratch checkout, and the receipt's
    // `candidate_sha` and the run's `repo_path` point into it — so
    // deleting it first left the ledger describing objects that no longer
    // existed, and an arm that "earned its result" had nothing to show.
    let fetched = relais::workspace::fetch_candidate_refs(&root, &checkout_dir);
    if let Err(e) = &fetched {
        eprintln!(
            "relais dataset replay: could not bring the replay's refs into {}: {e}. Leaving the \
             checkout at {} so the result is not lost.",
            root.display(),
            checkout_dir.display()
        );
    }
    // Only once the refs are safe. A failure to remove is logged, never
    // fatal; a checkout kept because its refs could not be saved is the
    // better outcome of the two.
    if fetched.is_ok() {
        if let Err(e) = std::fs::remove_dir_all(&checkout_dir) {
            eprintln!("relais dataset replay: could not remove the replay checkout: {e}");
        }
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!("relais dataset replay: {e}");
            eprintln!("{}", log.end_line(&ReplayEnd::unrecorded("interrupted")));
            announce_replay_done(&artifacts_dir, run_id.as_str(), "interrupted", None);
            return Ok(CliOutcome::OperationalFailure);
        }
    };

    // The purpose was written with the run row (see `RunConfig::purpose`),
    // so there is nothing to stamp here.
    let recorded = record_replay_trial(
        &ledger,
        &outcome,
        &relais::ledger::NewReplayTrial {
            trial_id: &replay_id,
            task_id: &task_id,
            source_run_id: &source_run,
            incumbent_recipe_id: &incumbent_recipe_id,
            arm_recipe_id: &arm_recipe,
            base_sha: &base_sha,
            contract_hash: &contract_hash,
            verification_profile_hash: &replay_profile_hash,
            workspace_isolation: "fresh_checkout_no_accepted_answer",
            arm_run_id: &outcome.run_id,
            design_sha256: design_sha256.as_deref(),
        },
        &arm,
    );
    let trial_outcome = match recorded {
        Ok(settled) => {
            if let Some(mismatch) = &settled.verdict.mismatch {
                eprintln!("relais dataset replay: recorded invalid: {mismatch}");
            }
            eprintln!("{}", log.end_line(&settled.end()));
            announce_replay_done(
                &artifacts_dir,
                outcome.run_id(),
                outcome.state().as_str(),
                Some(replay_id.as_str()),
            );
            settled.verdict.outcome
        }
        Err(e) => {
            eprintln!(
                "{}",
                log.end_line(&ReplayEnd::unrecorded(outcome.state().as_str()))
            );
            announce_replay_done(
                &artifacts_dir,
                outcome.run_id(),
                outcome.state().as_str(),
                None,
            );
            return Err(e);
        }
    };

    println!("replay run: {}", outcome.run_id());
    println!("replay record: {replay_id}");
    println!("outcome:       {trial_outcome}");
    println!(
        "this replay is one arm's result; it does not compare, score, rank or promote anything"
    );
    Ok(CliOutcome::Accepted)
}

/// The recipe a replay's arm runs, and the model its first attempt must
/// dispatch: the recipe's own `models[tier]`, else the policy's (the
/// profile the router's ladder resolves for a covering recipe).
pub(crate) struct ReplayArm<'a> {
    spec: &'a RecipeSpec,
    model: Option<&'a str>,
}

impl<'a> ReplayArm<'a> {
    fn of(policy: &'a RepoPolicy, recipe_id: &str) -> Option<Self> {
        let spec = policy
            .recipes
            .iter()
            .find(|spec| spec.recipe_id() == recipe_id)?;
        let model = typed_gate::arm_model(policy, spec);
        Some(Self { spec, model })
    }
}

/// A replay's trial as written: the self-check's verdict and the figures
/// it settled with.
pub(crate) struct SettledReplay {
    verdict: live_trial::ReplayVerdict,
    dispatched_model: Option<String>,
    cost: relais::ledger::TrialCost,
}

impl SettledReplay {
    pub(crate) fn end(&self) -> ReplayEnd<'_> {
        ReplayEnd {
            outcome: self.verdict.outcome.as_str(),
            dispatched_model: self.dispatched_model.as_deref(),
            cost: Some(self.cost),
        }
    }
}

/// Writes the replay's trial and settles it from the arm run's outcome,
/// after the replay's self-check: a first attempt that ran another model
/// than the arm's, or a design that disagrees with the arm's recipe, is
/// recorded `invalid`, never as the arm's result.
pub(crate) fn record_replay_trial(
    ledger: &relais::ledger::Ledger,
    outcome: &relais::runner::RunOutcome,
    trial: &relais::ledger::NewReplayTrial<'_>,
    arm: &ReplayArm<'_>,
) -> Result<SettledReplay, CliError> {
    let dispatched_model = operational(
        ledger.first_dispatch_intent(&outcome.run_id),
        "dataset replay",
    )?
    .and_then(|intent| {
        intent
            .get("model")
            .and_then(|m| m.as_str())
            .map(str::to_string)
    });
    let verdict = live_trial::ReplayCheck {
        arm_model: arm.model.unwrap_or_default(),
        dispatched_model: dispatched_model.as_deref(),
        requires_design: arm.spec.requires_design,
        design_sha256: trial.design_sha256,
    }
    .settle(live_trial::trial_outcome_of(&outcome.terminal));
    let (cost, duration_ms) = operational(
        live_trial::settled_figures(ledger, &outcome.run_id),
        "dataset replay",
    )?;
    operational(ledger.insert_replay_trial(trial), "dataset replay")?;
    operational(
        ledger.settle_trial(
            trial.trial_id,
            verdict.outcome,
            verdict.accepted_without_escalation,
            cost,
            duration_ms,
        ),
        "dataset replay",
    )?;
    Ok(SettledReplay {
        verdict,
        dispatched_model,
        cost,
    })
}

/// The two lines every replay prints to stderr, one as its arm starts and
/// one as it ends; the plan's record is copied from them
/// (docs/plans/2026-10-12-task-types.md).
pub(crate) struct ReplayLog<'a> {
    task_type: Option<relais::contract::TaskType>,
    task: &'a TaskId,
    /// The model the arm's first attempt is meant to dispatch.
    arm_model: &'a str,
    /// The one-minute load average when the replay started.
    load: Option<f64>,
    started: std::time::Instant,
}

/// How a replay ended, for its end line.
pub(crate) struct ReplayEnd<'a> {
    outcome: &'a str,
    /// What the first attempt dispatched; `None` when nothing was.
    dispatched_model: Option<&'a str>,
    /// `None` when no trial was settled to carry a figure.
    cost: Option<relais::ledger::TrialCost>,
}

impl ReplayEnd<'_> {
    /// A replay whose trial could not be written: its run's state, and no
    /// figures.
    pub(crate) fn unrecorded(outcome: &str) -> ReplayEnd<'_> {
        ReplayEnd {
            outcome,
            dispatched_model: None,
            cost: None,
        }
    }
}

impl ReplayLog<'_> {
    pub(crate) fn head(&self, event: &str) -> String {
        format!(
            "relais dataset replay: {event} type={} task={}",
            self.task_type
                .map_or("none", relais::contract::TaskType::as_str),
            self.task
        )
    }

    pub(crate) fn load_text(&self) -> String {
        self.load
            .map_or_else(|| "unknown".to_string(), |load| format!("{load:.2}"))
    }

    pub(crate) fn start_line(&self) -> String {
        format!(
            "{} model={} load={}",
            self.head("start"),
            self.arm_model,
            self.load_text()
        )
    }

    pub(crate) fn end_line(&self, end: &ReplayEnd<'_>) -> String {
        self.end_line_after(end, self.started.elapsed())
    }

    pub(crate) fn end_line_after(&self, end: &ReplayEnd<'_>, wall: std::time::Duration) -> String {
        let cost = end
            .cost
            .and_then(|cost| cost.cost())
            .map_or_else(|| "unknown".to_string(), |cost| cost.to_string());
        format!(
            "{} outcome={} model={} cost={cost} wall={:.1}s load={}",
            self.head("end"),
            end.outcome,
            end.dispatched_model.unwrap_or("none"),
            wall.as_secs_f64(),
            self.load_text()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start and end lines carry the type, the task, the outcome, the
    /// dispatched model, the cost, the wall time and the load at start.
    #[test]
    fn a_replay_logs_one_start_and_one_end_line() {
        let task = TaskId::from_stored("task-abc");
        let log = ReplayLog {
            task_type: Some(relais::contract::TaskType::Feature),
            task: &task,
            arm_model: "haiku",
            load: Some(3.5),
            started: std::time::Instant::now(),
        };
        assert_eq!(
            log.start_line(),
            "relais dataset replay: start type=feature task=task-abc model=haiku load=3.50"
        );
        let cost = relais::ledger::TrialCost::new(
            Some(relais::money::MicroUsd::from_micros(300_000)),
            relais::money::CostCompleteness::Actual,
        )
        .expect("a coherent cost");
        let end = ReplayEnd {
            outcome: "invalid",
            dispatched_model: Some("sonnet"),
            cost: Some(cost),
        };
        assert_eq!(
            log.end_line_after(&end, std::time::Duration::from_millis(12_340)),
            "relais dataset replay: end type=feature task=task-abc outcome=invalid \
             model=sonnet cost=$0.3 wall=12.3s load=3.50"
        );
        let untyped = ReplayLog {
            task_type: None,
            load: None,
            ..log
        };
        assert_eq!(
            untyped.end_line_after(
                &ReplayEnd::unrecorded("interrupted"),
                std::time::Duration::ZERO
            ),
            "relais dataset replay: end type=none task=task-abc outcome=interrupted \
             model=none cost=unknown wall=0.0s load=unknown"
        );
    }
}
