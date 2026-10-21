//! `relais run`.

use super::ctx::id_source;
use super::ctx::load_contract;
use super::ctx::load_machine;
use super::ctx::load_price_table;
use super::ctx::load_repo_policy;
use super::ctx::open_ledger;
use super::ctx::pricing_refusals;
use super::ctx::resolve_task_override;
use super::learn::learned_registry;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::plan::task_identity;
use super::protocol::refuse_run;
use relais::admission::Gate;
use relais::admission::GateError;
use relais::backend::Harness;
use relais::contract::TaskContract;
use relais::ids::RunId;
use relais::learn::predict::RegistryPredictor;
use relais::ledger::Ledger;
use relais::lifecycle::RunPurpose;
use relais::paths;
use relais::policy::MachineSettings;
use relais::policy::RepoPolicy;
use relais::report;
use relais::route;
use relais::runner::execute;
use relais::runner::live_trial;
use relais::runner::RunConfig;
use relais::runner::Terminal;
use relais::workspace;
use std::path::Path;

/// Why this `relais run` must not start, if it must not: it comes from the
/// relais plugin, whose session said hello recently, on a Claude Code the
/// plugin supports. `installed` is what `claude --version` printed;
/// `probe_failure` is why the probe could not say, when it could not. A
/// probe that merely timed out leaves the version unchecked: one warning
/// on stderr under `command`'s name, and the run goes on if the hello is
/// fresh.
pub(crate) fn run_refusal(
    command: &str,
    gate: &dyn Gate,
    session: &str,
    installed: Option<&str>,
    probe_failure: Option<&relais::adapter::claude::ProbeFailure>,
) -> Result<Option<String>, GateError> {
    use relais::adapter::claude::{claude_code_support, Support};
    let unchecked = match claude_code_support(installed, probe_failure) {
        Support::Unsupported(detail) => return Ok(Some(detail)),
        Support::Unchecked(reason) => Some(reason),
        Support::Supported => None,
    };
    let refusal = relais::native::hello_refusal(gate.native_hello_age(session)?);
    if refusal.is_none() {
        if let Some(reason) = unchecked {
            eprintln!(
                "{command}: warning: Claude Code version unchecked: {reason}; continuing \
                 (supported: {})",
                relais::native::SUPPORTED_CLAUDE_CODE
            );
        }
    }
    Ok(refusal)
}

/// A `relais run` starts only from Claude Code with the relais plugin.
/// Where this run works (ADR-0008): the flag, `RELAIS_WORKSPACE`, `CI`, the
/// machine setting and git's own answer about the checkout, through the one
/// pure decision. `Ok(Err(..))` is a refusal that blocks the run with its
/// remedy; an unknown value is a usage error.
pub(crate) fn run_workspace(
    flag: Option<&str>,
    root: &Path,
    machine: &MachineSettings,
    repo: &RepoPolicy,
) -> Result<Result<relais::workspace::reuse::Workspace, String>, CliError> {
    use relais::workspace::reuse::{resolve, ModeInputs, ModeRefusal, Requested};
    let flag = flag
        .map(Requested::parse)
        .transpose()
        .map_err(|refusal| CliError::Usage {
            detail: format!(
                "relais run: --workspace takes auto, fork, in-place or isolated ({})",
                match refusal {
                    ModeRefusal::Unknown(value) => format!("not `{value}`"),
                    _ => "unknown value".to_string(),
                }
            ),
        })?;
    let env_workspace = std::env::var("RELAIS_WORKSPACE").ok();
    let ci = std::env::var("CI").ok();
    let inputs = ModeInputs {
        flag,
        env_workspace: env_workspace.as_deref(),
        ci: ci.as_deref(),
        machine: machine.workspace.mode,
        repo_opts_out_of_in_place: repo.workspace.in_place == Some(false),
        linked_worktree: relais::repo::is_linked_worktree(root),
    };
    Ok(match resolve(inputs) {
        Ok(workspace) => Ok(workspace),
        Err(ModeRefusal::Unknown(value)) => {
            return Err(CliError::Usage {
                detail: format!(
                    "relais run: RELAIS_WORKSPACE takes auto, fork, in-place or isolated, not `{value}`"
                ),
            })
        }
        Err(ModeRefusal::WidenedPastTheEnvironment) => Err(
            "relais run: RELAIS_WORKSPACE=isolated or CI forces an isolated workspace, and --workspace cannot widen it; unset the variable or drop the flag".to_string(),
        ),
        Err(ModeRefusal::PrimaryCheckout) => Err(format!(
            "relais run: {} is the primary checkout; in-place needs a linked worktree (git worktree add …), or drop --workspace to fork",
            root.display()
        )),
        Err(ModeRefusal::InPlaceOptedOut) => Err(
            "relais run: this repository's relais.toml sets [workspace] in_place = false; drop --workspace to fork".to_string(),
        ),
    })
}

/// Where a run's contract comes from.
#[derive(Clone, Copy)]
pub(crate) enum RunSource<'a> {
    /// A contract file.
    Task(&'a Path),
    /// A successor intent in the ledger (plan A5).
    Intent(&'a str),
}

/// The contract of a successor intent and the task its run belongs to: the
/// decided run's, so the successor's cost joins that task's total.
pub(crate) fn intent_contract(
    ledger: &Ledger,
    intent_id: &str,
) -> Result<(TaskContract, String), String> {
    let intent = ledger
        .successor_intent(intent_id)
        .map_err(|e| format!("ledger: {e}"))?
        .ok_or_else(|| format!("unknown successor intent {intent_id}"))?;
    let contract = TaskContract::from_json_str(&intent.contract_json)
        .map_err(|e| format!("the contract of intent {intent_id} is invalid: {e}"))?;
    if contract.hash() != intent.contract_digest {
        return Err(format!(
            "the contract of intent {intent_id} no longer has the digest recorded with it"
        ));
    }
    let task = ledger
        .task_of_run(&intent.predecessor)
        .map_err(|e| format!("ledger: {e}"))?
        .ok_or_else(|| format!("run {} has no task on record", intent.predecessor))?;
    Ok((contract, task.to_string()))
}

/// What the host says of a driver pid, with the start time of the process
/// that holds it now: the ledger compares it with the one recorded.
pub(crate) fn successor_process(pid: u32) -> relais::ledger::SuccessorProcess {
    if relais::coordinator::process_alive(pid) {
        relais::ledger::SuccessorProcess::Alive {
            started: relais::procs::start_time(pid),
        }
    } else {
        relais::ledger::SuccessorProcess::Gone
    }
}

/// A launch of a successor intent was refused before its run existed: hand
/// the claim back to pending with the refusal on the intent, so a later
/// launch can take it instead of finding a claim with no run row.
pub(crate) fn release_intent(
    ledger: &Ledger,
    source: RunSource<'_>,
    run_id: &RunId,
    refusal: &str,
) {
    if let RunSource::Intent(intent) = source {
        if let Err(e) = ledger.release_claim(intent, run_id, refusal) {
            eprintln!("relais run: intent {intent} could not be released: {e}");
        }
    }
}

/// A run that failed operationally hands its claim back only when it left
/// no row: a run that exists holds the claim, and recovery reads its state.
pub(crate) fn release_intent_without_run(
    ledger: &Ledger,
    source: RunSource<'_>,
    run_id: &RunId,
    refusal: &str,
) {
    match ledger.run_exists(run_id) {
        Ok(false) => release_intent(ledger, source, run_id, refusal),
        Ok(true) => {}
        Err(e) => eprintln!("relais run: could not tell whether run {run_id} exists: {e}"),
    }
}

pub(crate) fn run_command(
    source: RunSource<'_>,
    revise: Option<&str>,
    origin: &relais::native::RunOrigin<'_>,
    workspace_flag: Option<&str>,
) -> Result<CliOutcome, CliError> {
    if let Some(detail) = relais::native::origin_refusal(origin) {
        return Err(CliError::Usage { detail });
    }
    let session = relais::coordinator::resolve_session();
    let (root, repo) = load_repo_policy()?;
    let machine = load_machine()?;
    let workspace = match run_workspace(workspace_flag, &root, &machine, &repo)? {
        Ok(workspace) => workspace,
        Err(refusal) => {
            eprintln!("{refusal}");
            return Ok(CliOutcome::Blocked);
        }
    };
    let from_file = match source {
        RunSource::Task(task) => Some(load_contract(task)?),
        RunSource::Intent(_) => None,
    };
    let ledger = open_ledger()?;
    let (contract, revise) = match (source, from_file) {
        (_, Some(contract)) => (contract, revise.map(str::to_string)),
        (RunSource::Intent(intent), None) => match intent_contract(&ledger, intent) {
            Ok((contract, task)) => (contract, Some(task)),
            Err(detail) => {
                return Ok(refuse_run(
                    "intent_unavailable",
                    detail,
                    CliOutcome::Blocked,
                ))
            }
        },
        (RunSource::Task(_), None) => unreachable!("a task source loaded its contract above"),
    };
    let task_override = match resolve_task_override(&ledger, &contract, revise.as_deref()) {
        Ok(task_override) => task_override,
        Err(detail) => return Ok(refuse_run("task_refused", detail, CliOutcome::Blocked)),
    };
    let artifacts_dir = paths::runs_dir().map_err(CliError::Home)?;
    let backend = match relais::adapter::claude::ClaudeBackend::discover() {
        Ok(backend) => std::sync::Arc::from(backend),
        Err(e) => {
            return Ok(refuse_run(
                "backend_unavailable",
                e.to_string(),
                CliOutcome::Blocked,
            ))
        }
    };
    // The three tools a run talks to, each behind the port this crate
    // owns: git, the decision corpus and the check inventory.
    let git = relais::workspace::SystemGit;
    let aval_resolver = relais::context::AvalCli::new(root.clone());
    let hooks = relais::verify::AmontCli::new();
    let attest = relais::verify::AmontCli::new();
    // Managed dispatch is the only path `relais run` takes: a missing
    // coordinator blocks the run rather than launching unmanaged
    // (SPEC §23).
    let socket = relais::coordinator::socket_path().map_err(CliError::Home)?;
    if let Err(e) = relais::coordinator::ensure_running(&socket) {
        return Ok(refuse_run(
            "admission_unavailable",
            e.to_string(),
            CliOutcome::Blocked,
        ));
    }
    let gate = relais::coordinator::RemoteGate::new(socket);
    // Before anything is dispatched: the plugin of this session is alive,
    // and the Claude Code it runs on is one the plugin supports.
    let report = backend.probe_report();
    let installed = report
        .as_ref()
        .ok()
        .and_then(|capabilities| capabilities.version.as_deref());
    match run_refusal(
        "relais run",
        &gate,
        &session.id,
        installed,
        report.as_ref().err(),
    ) {
        Ok(None) => {}
        Ok(Some(detail)) => return Err(CliError::Usage { detail }),
        Err(e) => {
            return Ok(refuse_run(
                "admission_unavailable",
                e.to_string(),
                CliOutcome::Blocked,
            ))
        }
    }
    // Learned routing reads the registry's active artifact, pinned for
    // this run (SPEC §17); disabled routing leaves everything else intact.
    let harness = report.as_ref().ok().map(|capabilities| {
        format!(
            "{} {}",
            backend.name(),
            capabilities.version.as_deref().unwrap_or("?")
        )
    });
    let registry = learned_registry(&machine);
    let predictor = registry
        .as_ref()
        .map(|registry| RegistryPredictor::new(registry, &repo, harness.as_deref()));
    let ids = id_source();
    let run_id = match ids.run_id() {
        Ok(run_id) => run_id,
        Err(e) => {
            eprintln!("relais run: {e}");
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    // A successor intent is claimed by the run id minted for it: atomic,
    // so a second launch of the same intent finds the first run instead of
    // making another (plan A5).
    if let RunSource::Intent(intent) = source {
        match ledger.claim_intent(intent, &run_id, &successor_process) {
            Ok(relais::ledger::Claim::Claimed(_)) => {}
            Ok(relais::ledger::Claim::AlreadyClaimed { run }) => {
                return Ok(refuse_run(
                    "intent_already_claimed",
                    format!("intent {intent} is already claimed by run {run}"),
                    CliOutcome::Blocked,
                ))
            }
            Ok(relais::ledger::Claim::Unknown) => {
                return Ok(refuse_run(
                    "intent_unavailable",
                    format!("unknown successor intent {intent}"),
                    CliOutcome::Blocked,
                ))
            }
            Err(e) => {
                eprintln!("relais run: {e}");
                return Ok(CliOutcome::OperationalFailure);
            }
        }
    }
    // The worker attempts go to the relais plugin of the parent session;
    // everything else stays on the Claude backend.
    let prices = match load_price_table() {
        Ok(prices) => prices,
        Err(e) => {
            release_intent(
                &ledger,
                source,
                &run_id,
                &format!("prices unavailable: {e}"),
            );
            return Err(e);
        }
    };
    let refusals = match pricing_refusals(&repo, &prices, &ledger) {
        Ok(refusals) => refusals,
        Err(e) => {
            release_intent(
                &ledger,
                source,
                &run_id,
                &format!("pricing preflight failed: {e}"),
            );
            return Err(e);
        }
    };
    if !refusals.is_empty() {
        for refusal in &refusals {
            eprintln!("relais run: {refusal}");
        }
        release_intent(&ledger, source, &run_id, &refusals.join("; "));
        return Ok(CliOutcome::Blocked);
    }
    let native_backend = relais::adapter::native::NativeBackend::new(
        backend.as_ref(),
        &gate,
        relais::adapter::native::Link {
            run_id: run_id.as_str().to_string(),
            session_id: session.id.clone(),
            wire: relais::protocol::Wire::process(),
            // Without a home there is no transcript to read: the run books
            // its usage and says `rollback_ids_missing`.
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
    let run_backend: &dyn relais::backend::Backend = &native_backend;
    // Printed here, before `execute`, so it is visible on every path —
    // including the `Err` arm below that returns early — and only once:
    // a run can take minutes, and deferring this to the end read as
    // silence for the whole time it ran.
    eprintln!("session: {} ({})", session.id, session.source);
    if session.source.is_fallback() {
        eprintln!(
            "relais run: warning: no CLAUDE_CODE_SESSION_ID or RELAIS_SESSION_ID in the \
             environment; attributing this run to {}",
            session.source
        );
    }
    // Live trials (SPEC §28). With the envelope off `decide` answers
    // without reading a candidate or touching the ledger, `lines` is
    // empty and `record` writes nothing: the run below is the run it
    // always was.
    let repo_identity = relais::repo::identity(&root);
    let task_id = task_identity(task_override.clone(), &repo_identity, &contract);
    let trial = match operational(
        live_trial::decide(&live_trial::Environment {
            root: &root,
            repo: &repo,
            machine: &machine,
            identity: &repo_identity,
            contract: &contract,
            task_id: &task_id,
            ledger: Some(&ledger),
        }),
        "trial assignment",
    ) {
        Ok(trial) => trial,
        Err(e) => {
            release_intent(
                &ledger,
                source,
                &run_id,
                &format!("trial assignment failed: {e}"),
            );
            return Err(e);
        }
    };
    for line in trial.lines() {
        eprintln!("{line}");
    }
    // A row is written before the run, so a draw is on record even if the
    // run dies. Without a resolvable base there is nothing to record, and
    // then nothing is drawn: the incumbent runs and reports the base
    // itself. Never a candidate policy without its row.
    let trial_base = trial
        .draws()
        .then(|| workspace::resolve_base(&root, &contract.base_ref).ok())
        .flatten();
    let trial_id = match &trial_base {
        Some(base_sha) => {
            let contract_hash = contract.hash();
            let profile_hash = repo
                .verification
                .profiles
                .get(&contract.verification_profile)
                .map(|profile| profile.hash())
                .unwrap_or_default();
            match trial.record(
                &ledger,
                &ids,
                &live_trial::TrialFacts {
                    task_id: &task_id,
                    base_sha,
                    contract_hash: &contract_hash,
                    verification_profile_hash: &profile_hash,
                },
                &run_id,
            ) {
                Ok(trial_id) => trial_id,
                Err(e) => {
                    eprintln!("relais run: {e}");
                    release_intent(
                        &ledger,
                        source,
                        &run_id,
                        &format!("trial record failed: {e}"),
                    );
                    return Ok(CliOutcome::OperationalFailure);
                }
            }
        }
        None => None,
    };
    let run_policy: &RepoPolicy = match &trial_id {
        Some(_) => trial.candidate_policy().unwrap_or(&repo),
        None => &repo,
    };
    // The ballast is made at start when space allows, and made again on
    // the next start after a full disk freed it (SPEC §10).
    keep_ballast(&artifacts_dir);
    let outcome = match execute(&RunConfig {
        workspace,
        repo_dir: &root,
        contract: &contract,
        repo_policy: run_policy,
        machine: &machine,
        ledger: &ledger,
        ids: &ids,
        backend: run_backend,
        git: &git,
        hooks: &hooks,
        attest: &attest,
        artifacts_dir: artifacts_dir.clone(),
        aval_resolver: &aval_resolver,
        predictor: predictor
            .as_ref()
            .map(|predictor| predictor as &dyn route::RoutePredictor),
        gate: Some(&gate),
        session_id: session.id.clone(),
        heartbeat_every: std::time::Duration::from_secs(30),
        task_override: task_override.as_ref(),
        // A run a live trial row names is a trial arm on the run's own
        // record too — Control included — so `runs.purpose` and
        // `trials.source_run_id` never disagree about it (SPEC §28).
        purpose: trial_id.as_ref().map(|_| RunPurpose::TrialArm),
        run_id: Some(run_id.clone()),
        wire: relais::protocol::Wire::process(),
        design: relais::runner::DesignSource::Contract,
        // Only `run` consumes a pending change to the repository's
        // commands (ADR-0007).
        declarations: &relais::trust::Recorder(&ledger),
    }) {
        Ok(outcome) => outcome,
        Err(e) => {
            if let Some(trial_id) = &trial_id {
                if let Err(settle_error) = live_trial::settle_errored(&ledger, trial_id) {
                    eprintln!("relais run: could not settle trial {trial_id}: {settle_error}");
                }
            }
            if let relais::runner::RunError::StorageFull { volume } = &e {
                // One line, naming the volume; the run's last durable
                // ledger state is what `resume` reconciles from.
                eprintln!(
                    "relais run: storage full on {volume}; `relais resume {run_id}` reconciles the run"
                );
                release_intent_without_run(&ledger, source, &run_id, &format!("run failed: {e}"));
                return Ok(CliOutcome::StorageFull);
            }
            eprintln!("relais run: {e}");
            release_intent_without_run(&ledger, source, &run_id, &format!("run failed: {e}"));
            return Ok(CliOutcome::OperationalFailure);
        }
    };
    if let Some(trial_id) = &trial_id {
        if let Err(e) = live_trial::settle(&ledger, trial_id, &outcome) {
            eprintln!("relais run: could not settle trial {trial_id}: {e}");
        }
    }
    let run_dir = artifacts_dir.join(outcome.run_id());
    let result = match &outcome.terminal {
        Terminal::Accepted(receipt) => {
            println!("accepted: {}", receipt.candidate_sha);
            println!("receipt: {}/receipt.json", run_dir.display());
            println!(
                "patch:   {}/candidate-{}.patch",
                run_dir.display(),
                receipt.attempts
            );
            println!(
                "cost:    {}",
                report::cost_line(receipt.cost, receipt.cost_completeness)
            );
            println!("run:     {}", outcome.run_id());
            Ok(CliOutcome::Accepted)
        }
        Terminal::NeedsDecision { reason, detail } => {
            eprintln!("needs_decision ({reason}): {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            Ok(CliOutcome::NeedsDecision)
        }
        Terminal::NeedsReview { detail } => {
            eprintln!("needs_review: {detail}");
            eprintln!(
                "evidence and the preserved candidate are under {}",
                run_dir.display()
            );
            Ok(CliOutcome::NeedsReview)
        }
        Terminal::Blocked { code, detail } => {
            eprintln!("blocked ({code}): {detail}");
            Ok(CliOutcome::Blocked)
        }
        Terminal::BlockedBy { reason, detail } => {
            eprintln!("blocked ({reason}): {detail}");
            Ok(CliOutcome::Blocked)
        }
        Terminal::Failed { detail } => {
            eprintln!("failed: {detail}");
            eprintln!("the preserved candidate is under {}", run_dir.display());
            Ok(CliOutcome::Failed)
        }
        Terminal::BudgetExhausted { detail } => {
            eprintln!("budget_exhausted: {detail}");
            eprintln!(
                "the patch and evidence are preserved under {}",
                run_dir.display()
            );
            Ok(CliOutcome::BudgetExhausted)
        }
        Terminal::Interrupted { detail } => {
            eprintln!("interrupted: {detail}");
            eprintln!("run `relais resume {}` to reconcile", outcome.run_id());
            Ok(CliOutcome::Interrupted)
        }
        Terminal::Cancelled { detail } => {
            eprintln!("cancelled: {detail}");
            eprintln!(
                "the preserved candidate and evidence are under {}",
                run_dir.display()
            );
            Ok(CliOutcome::Cancelled)
        }
    };
    result
}

/// Make the state directory's ballast file when it is missing and the
/// volume has room for it. A ballast that cannot be made is said once and
/// never stops the run: it is a cushion, not a requirement.
pub(crate) fn keep_ballast(artifacts_dir: &Path) {
    use relais::storage_full::{ballast_path, ensure_ballast, BallastState, BALLAST_BYTES};
    let Some(state_dir) = artifacts_dir.parent() else {
        return;
    };
    let probe = relais::disk::StatvfsProbe;
    if let BallastState::Failed(why) =
        ensure_ballast(&ballast_path(state_dir), BALLAST_BYTES, &probe)
    {
        eprintln!("relais run: the 64 MiB ballast file could not be made: {why}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relais::ids::TaskId;
    use relais::ledger::{Ledger, Transition};

    /// A ledger holding one pending successor intent, and its id.
    fn ledger_with_intent(tag: &str) -> (Ledger, String, relais::test_support::TempDir) {
        use relais::ledger::{Actor, DecisionAnswer, NewIntent, RecordedVia};
        use relais::lifecycle::{Reason, State};
        let dir = relais::test_support::temp_dir(tag);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let decided = RunId::from_stored("run-a");
        ledger
            .insert_run(&decided, "/r", None, &TaskId::from_stored("task-a"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: decided.clone(),
                attempt_id: None,
                from_state: None,
                to_state: State::NeedsDecision,
                reason: "scope_exceeded".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("waiting");
        let intent = ledger
            .resolve_decision_with_intent(
                &decided,
                &DecisionAnswer {
                    resolution: Reason::DecisionRevised,
                    actor: Actor::Orchestrator,
                    person_quote: None,
                    recorded_via: RecordedVia::Cli,
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsDecision,
                    to_state: State::Failed,
                },
                &NewIntent {
                    predecessor: decided.clone(),
                    contract_digest: "digest".into(),
                    contract_json: "{}".into(),
                    scope_delta: Vec::new(),
                    candidate: None,
                    review: None,
                },
            )
            .expect("written")
            .expect("open decision");
        (ledger, intent.intent_id, dir)
    }

    #[test]
    fn a_failure_before_the_run_row_exists_leaves_the_intent_pending() {
        use relais::ledger::{Claim, IntentState};
        let (ledger, id, _dir) = ledger_with_intent("intent-release");
        let run_id = RunId::from_stored("run-new");
        assert!(matches!(
            ledger
                .claim_intent(&id, &run_id, &successor_process)
                .expect("claim"),
            Claim::Claimed(_)
        ));
        release_intent_without_run(&ledger, RunSource::Intent(&id), &run_id, "run failed");
        let after = ledger.successor_intent(&id).expect("read").expect("row");
        assert_eq!(after.state, IntentState::Pending);
        assert_eq!(after.run_id, None);
        assert_eq!(after.refusal.as_deref(), Some("run failed"));
    }

    #[test]
    fn a_failure_after_the_run_row_exists_keeps_the_claim() {
        use relais::ledger::IntentState;
        let (ledger, id, _dir) = ledger_with_intent("intent-keep");
        let run_id = RunId::from_stored("run-new");
        ledger
            .claim_intent(&id, &run_id, &successor_process)
            .expect("claim");
        ledger
            .insert_run(&run_id, "/r", None, &TaskId::from_stored("task-a"), "rk")
            .expect("the run row");
        release_intent_without_run(&ledger, RunSource::Intent(&id), &run_id, "run failed");
        let after = ledger.successor_intent(&id).expect("read").expect("row");
        assert_eq!(after.state, IntentState::Claimed);
        assert_eq!(after.run_id, Some(run_id));
    }

    #[test]
    fn a_run_is_refused_without_a_recent_hello_or_on_a_claude_code_outside_the_range() {
        use relais::adapter::claude::ProbeFailure;
        let gate = relais::admission::LocalGate::new(Default::default());
        let supported = Some("2.1.291 (Claude Code)");
        let silent = run_refusal("relais run", &gate, "s1", supported, None)
            .expect("answers")
            .expect("no hello is refused");
        assert!(silent.contains("hello"), "{silent}");
        gate.native_hello("s1").expect("hello");
        assert_eq!(
            run_refusal("relais run", &gate, "s1", supported, None).expect("answers"),
            None
        );
        assert!(run_refusal("relais run", &gate, "other", supported, None)
            .expect("answers")
            .is_some());
        for outside in ["2.0.0", "2.2.0", "not a version", "2.1.0 (Claude Code)"] {
            let refused = run_refusal("relais run", &gate, "s1", Some(outside), None)
                .expect("answers")
                .unwrap_or_else(|| panic!("{outside} is refused"));
            assert!(
                refused.contains(relais::native::SUPPORTED_CLAUDE_CODE),
                "{refused}"
            );
        }
        let empty = run_refusal("relais run", &gate, "s1", Some(""), None).expect("answers");
        assert!(empty.is_some(), "an empty answer is refused");
        let failed = run_refusal(
            "relais run",
            &gate,
            "s1",
            None,
            Some(&ProbeFailure::Refused {
                args: "--version".into(),
                ended: "exit 1".into(),
            }),
        )
        .expect("answers")
        .expect("a non-zero exit is refused");
        assert!(failed.contains("exit 1"), "{failed}");
        assert!(!failed.contains("printed nothing"), "{failed}");
    }

    #[test]
    fn a_timed_out_probe_continues_on_a_fresh_hello_and_is_refused_on_a_stale_one() {
        let gate = relais::admission::LocalGate::new(Default::default());
        let late = relais::adapter::claude::ProbeFailure::TimedOut {
            args: "--version".into(),
            after: std::time::Duration::from_secs(30),
        };
        let stale = run_refusal("relais run", &gate, "s1", None, Some(&late))
            .expect("answers")
            .expect("no hello is refused");
        assert!(stale.contains("hello"), "{stale}");
        gate.native_hello("s1").expect("hello");
        assert_eq!(
            run_refusal("relais run", &gate, "s1", None, Some(&late)).expect("answers"),
            None,
            "a late probe warns and continues"
        );
    }
}
