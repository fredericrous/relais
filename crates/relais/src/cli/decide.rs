//! `relais decide`.

use super::ctx::load_contract;
use super::ctx::refusal;
use super::ctx::Refusal;
use super::evidence::authorship_label;
use super::evidence::reseal_receipt_with_signoff;
use super::feedback::open_verification_gaps;
use super::outcome::exit_code;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::salvage::salvage_command;
use super::salvage::SalvageAnswer;
use super::status::intent_json;
use super::DecideAnswer;
use relais::acceptance::Evidence;
use relais::ids::RunId;
use relais::ledger::Ledger;
use relais::paths;
use relais::runner::approval;
use relais::runner::approval::seals_from_snapshot;
use relais::runner::Reason;
use relais::runner::State;
use std::path::{Path, PathBuf};
/// One `relais decide` invocation's arguments, bundled because they
/// answer one decision together.
pub(crate) struct DecideRequest<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) answer: DecideAnswer,
    pub(crate) actor: &'a str,
    pub(crate) person_quote: Option<&'a str>,
    pub(crate) note: Option<&'a str>,
    pub(crate) successor: Option<&'a str>,
    pub(crate) criterion: Option<&'a str>,
    pub(crate) candidate: Option<&'a str>,
    pub(crate) widen_scope: bool,
    pub(crate) rerun_verify: bool,
    pub(crate) successor_contract: Option<&'a Path>,
    pub(crate) json: bool,
    pub(crate) proof_stdin: bool,
}

/// What a recorded decision reports, for both the sentence and `--json`.
pub(crate) struct Decided {
    pub(crate) run: String,
    pub(crate) resolution: Reason,
    pub(crate) actor: relais::ledger::Actor,
    pub(crate) from_state: State,
    pub(crate) to_state: State,
    pub(crate) advice: serde_json::Value,
    pub(crate) sentence: String,
    /// The successor intent a `revise --widen-scope` or `--successor-
    /// contract` answer wrote (or, repeated, found).
    pub(crate) successor_intent: Option<serde_json::Value>,
}

pub(crate) type Decision = Result<Decided, Refusal>;

pub(crate) fn decide_command(request: DecideRequest<'_>) -> Result<CliOutcome, CliError> {
    let json = request.json;
    match decide_inner(&request) {
        Ok(Ok(done)) => {
            if json {
                let mut printed = serde_json::json!({
                    "run": done.run,
                    "resolution": done.resolution.as_str(),
                    "actor": done.actor.as_str(),
                    "from_state": done.from_state.as_str(),
                    "to_state": done.to_state.as_str(),
                    "advice": done.advice,
                });
                if let Some(intent) = &done.successor_intent {
                    printed["successor_intent"] = intent.clone();
                }
                println!("{printed}");
            } else {
                println!("{}", done.sentence);
            }
            Ok(CliOutcome::Accepted)
        }
        Ok(Err(refused)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": refused.message,
                        "exit": exit_code(&refused.outcome),
                    })
                );
            } else {
                eprintln!("relais decide: {}", refused.message);
            }
            Ok(refused.outcome)
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "error": e.to_string(),
                        "exit": exit_code(&e.outcome()),
                    })
                );
            }
            Err(e)
        }
    }
}

/// Why a `person-via-orchestrator` sign-off without a live plugin nonce is
/// refused: only the plugin's proved call counts as the person's
/// (ADR-0004).
pub(crate) const UNPROVED_SIGNOFF: &str =
    "`--criterion` is refused for `--actor person-via-orchestrator` \
     without the plugin's proof: only an answer recorded through the decide tool counts as the \
     person's sign-off";

/// The plugin's proof on stdin (`--proof-stdin`).
#[derive(serde::Deserialize)]
pub(crate) struct DecideProof {
    pub(crate) session: String,
    pub(crate) nonce: String,
}

/// Read the `--proof-stdin` frame. A malformed frame is a refusal, never a
/// silent `cli`: the plugin wrote it, so a bad one is a bug to see.
pub(crate) fn read_decide_proof() -> Result<DecideProof, Refusal> {
    use std::io::Read;
    let mut text = String::new();
    if let Err(e) = std::io::stdin().take(64 * 1024).read_to_string(&mut text) {
        return refusal(
            CliOutcome::InvalidInput,
            format!("`--proof-stdin`: could not read stdin: {e}"),
        );
    }
    serde_json::from_str(text.trim()).or_else(|e| {
        refusal(
            CliOutcome::InvalidInput,
            format!("`--proof-stdin` expects {{\"session\", \"nonce\"}} on stdin: {e}"),
        )
    })
}

/// The checks that need only the arguments, run before the ledger is
/// opened so a refused invocation touches nothing.
pub(crate) fn check_decide_arguments(
    request: &DecideRequest<'_>,
    stdin_is_terminal: bool,
) -> Result<relais::ledger::Actor, Refusal> {
    use relais::ledger::Actor;
    let actor = match Actor::parse(request.actor) {
        Ok(actor) => actor,
        Err(message) => return refusal(CliOutcome::InvalidInput, message),
    };
    let quote = request.person_quote.map(str::trim);
    match (actor, quote) {
        (Actor::PersonViaOrchestrator, None | Some("")) => {
            return refusal(
                CliOutcome::InvalidInput,
                "`--actor person-via-orchestrator` requires `--person-quote <text>`: the \
                 person's own words",
            );
        }
        (Actor::PersonViaOrchestrator, Some(_)) => {}
        (_, Some(_)) => {
            return refusal(
                CliOutcome::InvalidInput,
                "`--person-quote` only applies to `--actor person-via-orchestrator`",
            );
        }
        (_, None) => {}
    }
    if actor == Actor::Person && !stdin_is_terminal {
        return refusal(
            CliOutcome::InvalidInput,
            "`--actor person` is for the person at their own terminal, and stdin is not one — \
             run it from a terminal, or record it with `--actor person-via-orchestrator \
             --person-quote <their words>`",
        );
    }
    if request.criterion.is_some() {
        match actor {
            Actor::Orchestrator | Actor::Relais => {
                return refusal(
                    CliOutcome::InvalidInput,
                    "`--criterion` signs off a human criterion and neither an orchestrator nor \
                     relais ever does: the person signs it off from their own terminal \
                     (`--actor person`)",
                );
            }
            Actor::PersonViaOrchestrator if !request.proof_stdin => {
                return refusal(CliOutcome::InvalidInput, UNPROVED_SIGNOFF);
            }
            Actor::PersonViaOrchestrator => {}
            Actor::Person => {}
        }
    }
    Ok(actor)
}

/// Where a decision reads and writes: the ledger file and the runs directory.
pub(crate) struct StateLocations {
    pub(crate) ledger: PathBuf,
    pub(crate) runs: PathBuf,
}

fn locations_from_environment() -> Result<StateLocations, CliError> {
    Ok(StateLocations {
        ledger: paths::ledger_path().map_err(CliError::Home)?,
        runs: paths::runs_dir().map_err(CliError::Home)?,
    })
}

pub(crate) fn decide_inner(request: &DecideRequest<'_>) -> Result<Decision, CliError> {
    decide_in(request, &locations_from_environment)
}

/// [`decide_inner`] with the state locations passed in, so a test names its
/// own directory instead of setting process-wide environment.
pub(crate) fn decide_in(
    request: &DecideRequest<'_>,
    locate: &dyn Fn() -> Result<StateLocations, CliError>,
) -> Result<Decision, CliError> {
    let mut decision = record_answer(request, locate)?;
    if let Ok(done) = &mut decision {
        let frees_in_place = matches!(
            request.answer,
            DecideAnswer::Salvaged | DecideAnswer::Abandon | DecideAnswer::Decided
        );
        if frees_in_place && done.from_state == State::Interrupted {
            // The answer is already recorded: whatever goes wrong freeing
            // the worktree is reported inside the answer, never as an error.
            let report = match free_after_answer(locate, done) {
                Ok(report) => report,
                Err(e) => Some(format!(
                    "the in-place worktree could not be freed ({e}); finish it with \
                     `relais resume --retire {}`",
                    done.run
                )),
            };
            if let Some(report) = report {
                done.sentence = format!("{}; {report}", done.sentence);
            }
        }
    }
    Ok(decision)
}

fn free_after_answer(
    locate: &dyn Fn() -> Result<StateLocations, CliError>,
    done: &Decided,
) -> Result<Option<String>, CliError> {
    let locations = locate()?;
    let ledger = operational(Ledger::open(&locations.ledger), "the ledger is unavailable")?;
    let run = RunId::from_stored(&done.run);
    free_kept_in_place(&ledger, &run, done.to_state, &locations.runs)
}

/// What answering an interrupted in-place run does to its worktree marker.
/// Only a moved HEAD means the tree and branch are the person's, so only
/// then is the marker retired (the code `relais resume --retire` runs);
/// otherwise it stays and the command that retires it is named. Only a
/// missing marker means there is nothing to free; every other failure is
/// named in the returned report.
fn free_kept_in_place(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    runs_dir: &Path,
) -> Result<Option<String>, CliError> {
    use relais::workspace::inplace;
    let Some(workspace) = operational(ledger.run_workspace(run), "decide")? else {
        return Ok(None);
    };
    // The ledger records the run's directory, not whether it ran in place.
    // A directory that is not on disk holds nothing this can read, so there
    // is nothing to free here (a deleted, unpruned worktree's marker under
    // the common git dir is left to `relais resume --retire`). A directory
    // that exists but whose git dir cannot be read is a failure, named.
    if !workspace.repo_path.is_dir() {
        return Ok(None);
    }
    let git_dir = match inplace::git_dir(&workspace.repo_path) {
        Ok(git_dir) => git_dir,
        Err(e) => {
            return Ok(Some(format!(
                "the git directory of {} could not be read ({e}); finish it with \
                 `relais resume --retire {run}`",
                workspace.repo_path.display()
            )))
        }
    };
    let marker = match inplace::read_marker(&git_dir, run.as_str()) {
        Ok(marker) => marker,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Ok(Some(format!(
                "the in-place marker for it in {} could not be read ({e}); finish it with \
                 `relais resume --retire {run}`",
                workspace.repo_path.display()
            )))
        }
    };
    let head_moved = match inplace::head(&workspace.repo_path) {
        Ok((_, now)) => now != marker.head_sha,
        Err(e) => {
            return Ok(Some(format!(
                "the in-place worktree {} still holds a marker for it; HEAD could not be \
                 read ({e}), so finish it with `relais resume --retire {run}`",
                workspace.repo_path.display()
            )))
        }
    };
    if !head_moved {
        return Ok(Some(format!(
            "the in-place worktree {} still holds a marker for it; HEAD did not move, so \
             finish it with `relais resume --retire {run}`",
            workspace.repo_path.display()
        )));
    }
    let reconciliation = super::resume::reconcile_run(ledger, run)?;
    let mut lines = Vec::new();
    super::resume::retire_in_place_saying(
        ledger,
        run,
        state,
        &reconciliation,
        runs_dir,
        &mut |line| {
            lines.push(line);
        },
    )?;
    Ok(Some(lines.join("; ")))
}

fn record_answer(
    request: &DecideRequest<'_>,
    locate: &dyn Fn() -> Result<StateLocations, CliError>,
) -> Result<Decision, CliError> {
    use std::io::IsTerminal;
    let actor = match check_decide_arguments(request, std::io::stdin().is_terminal()) {
        Ok(actor) => actor,
        Err(refused) => return Ok(Err(refused)),
    };
    let run_id = request.run_id;
    let (answer, note, successor, criterion, candidate) = (
        request.answer,
        request.note,
        request.successor,
        request.criterion,
        request.candidate,
    );
    let person_quote = request.person_quote.map(str::trim);
    let proof = if request.proof_stdin {
        match read_decide_proof() {
            Ok(proof) => Some(proof),
            Err(refused) => return Ok(Err(refused)),
        }
    } else {
        None
    };
    let locations = locate()?;
    let ledger = operational(Ledger::open(&locations.ledger), "the ledger is unavailable")?;
    let recorded_via = match &proof {
        Some(proof) => operational(
            ledger.verify_decide_nonce(&proof.session, &proof.nonce),
            "decide",
        )?,
        None => relais::ledger::RecordedVia::Cli,
    };
    if criterion.is_some()
        && actor == relais::ledger::Actor::PersonViaOrchestrator
        && recorded_via != relais::ledger::RecordedVia::Plugin
    {
        return Ok(refusal(CliOutcome::InvalidInput, UNPROVED_SIGNOFF));
    }
    let run = RunId::from_stored(run_id);
    let Some(state) = operational(ledger.run_status(&run), "decide")? else {
        return Ok(refusal(
            CliOutcome::UnknownRun,
            format!("unknown run {run_id}"),
        ));
    };
    // `salvaged` answers a run no other answer can reach: one whose
    // candidate a person finished and merged themselves, whether the run
    // ended on its own (never awaiting a person, so it opened no decision
    // row for `resolve_decision` to close) or is still waiting on one (an
    // OPEN row of its own). It takes its own path below rather than
    // falling into the `awaits_a_person` gate every other answer shares.
    if answer == DecideAnswer::Salvaged {
        return salvage_command(
            &ledger,
            &run,
            state,
            &SalvageAnswer {
                actor,
                person_quote,
                recorded_via,
                note,
                criterion,
                successor,
                candidate,
            },
        );
    }
    let revision_flags =
        request.widen_scope || request.rerun_verify || request.successor_contract.is_some();
    if revision_flags || answer == DecideAnswer::ReviewParts {
        if revision_flags && answer != DecideAnswer::Revise {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                "`--widen-scope`, `--rerun-verify` and `--successor-contract` only apply to \
                 `--answer revise`",
            ));
        }
        if criterion.is_some() {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                "`--criterion` only applies to `--answer approve`",
            ));
        }
        return successor_decide(
            &ledger,
            &run,
            state,
            request,
            &locations.runs,
            relais::ledger::Answerer {
                actor,
                person_quote,
                recorded_via,
            },
        );
    }
    if answer == DecideAnswer::Approve && criterion.is_none() {
        // An answered decision falls through to the refusal below, before any
        // artifact is written. A stop with no snapshot that is not a change's
        // over-budget stop (one from before schema 41, or whose snapshot could
        // not be saved) is resolved as it always was.
        let open_review_stop = match operational(ledger.decision_of_run(&run), "decide")? {
            Some(decision)
                if decision.resolved_at.is_none()
                    && seals_from_snapshot(&decision.raised_reason) =>
            {
                let has_snapshot =
                    operational(ledger.verification_snapshot(&run), "decide")?.is_some();
                has_snapshot || decision.raised_reason == Reason::ReviewOverBudget.as_str()
            }
            // Resolved with a receipt row: a repeat. `approve_from_snapshot`
            // finds it matches the snapshot's receipt (exit 0, heals a missing
            // `receipt.json`) or refuses it by name.
            Some(decision)
                if decision.resolved_at.is_some()
                    && seals_from_snapshot(&decision.raised_reason) =>
            {
                operational(ledger.receipt(&run), "decide")?.is_some()
                    && operational(ledger.verification_snapshot(&run), "decide")?.is_some()
            }
            _ => false,
        };
        if open_review_stop {
            return approve_stopped_review(
                &ledger,
                &locations.runs,
                &run,
                state,
                relais::ledger::Answerer {
                    actor,
                    person_quote,
                    recorded_via,
                },
                note,
            );
        }
    }
    if !state.awaits_a_person() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run_id} is {state}, not waiting on a person — nothing to decide"),
        ));
    }
    if let Some(criterion_id) = criterion {
        if answer != DecideAnswer::Approve {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                "`--criterion` only applies to `--answer approve`",
            ));
        }
        let Some((contract, _tier)) = operational(ledger.run_contract_and_tier(&run), "decide")?
        else {
            return Ok(refusal(
                CliOutcome::OperationalFailure,
                format!("run {run_id} carries no recorded contract"),
            ));
        };
        let declared: Vec<String> = contract.acceptance.iter().map(|entry| entry.id()).collect();
        let Some(entry) = contract
            .acceptance
            .iter()
            .find(|entry| entry.id() == criterion_id)
        else {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run_id} names no acceptance criterion `{criterion_id}` — \
                     this contract declares: {}",
                    declared.join(", ")
                ),
            ));
        };
        // Signing off a criterion the contract settles some OTHER way is
        // the second acceptance path this whole mechanism exists to
        // refuse: it would re-seal the receipt claiming a person settled
        // what a check was declared to settle. The gap itself would
        // survive — `acceptance_gaps` consults the sign-off set only for
        // `HumanSignOff` evidence — but the receipt would say something
        // untrue about how the criterion was met, and a receipt nobody
        // can trust is worse than a run that stays blocked.
        if !matches!(entry.evidence(), Some(Evidence::HumanSignOff)) {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "acceptance criterion `{criterion_id}` is not settled by a human \
                     sign-off but by {} — a sign-off answers only a criterion that asked for one",
                    match entry.evidence() {
                        Some(Evidence::Check { name }) => format!("check `{name}`"),
                        Some(Evidence::Test { authorship, .. }) =>
                            format!("a test ({})", authorship_label(*authorship)),
                        Some(Evidence::LlmReview) => "an LLM review".to_string(),
                        Some(Evidence::AmontGate { gate }) => format!("amont gate `{gate}`"),
                        Some(Evidence::Holdout { name }) => format!("holdout `{name}`"),
                        Some(Evidence::HumanSignOff) => unreachable!("guarded above"),
                        None => "the verification profile as a whole".to_string(),
                    }
                ),
            ));
        }
        operational(
            ledger.record_human_signoff(&run, criterion_id, actor, recorded_via, note),
            "decide",
        )?;
        reseal_receipt_with_signoff(&ledger, &run, criterion_id)?;
    }
    if answer == DecideAnswer::Approve {
        let gaps = open_verification_gaps(&ledger, &run)?;
        if !gaps.is_empty() {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run_id} still carries verification gaps ({}) — a waiver is \
                     a policy or contract change, never a CLI flag",
                    gaps.join("; ")
                ),
            ));
        }
    }
    let successor_run = successor.map(RunId::from_stored);
    let resolution = answer.resolution();
    let to_state = answer.terminal_state();
    let resolved = operational(
        ledger.resolve_decision(
            &run,
            &relais::ledger::DecisionAnswer {
                resolution,
                actor,
                person_quote,
                recorded_via,
                note,
                successor_run: successor_run.as_ref(),
                from_state: state,
                to_state,
            },
        ),
        "decide",
    )?;
    if !resolved {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run_id} has no open decision on record"),
        ));
    }
    Ok(Ok(Decided {
        run: run_id.to_string(),
        resolution,
        actor,
        from_state: state,
        to_state,
        advice: operational(ledger.decision_advice(&run), "decide")?,
        sentence: format!("recorded {} for {run_id} by {actor}", resolution.as_str()),
        successor_intent: None,
    }))
}

/// `relais decide --answer approve` on a run that stopped for a review it
/// could not have (`review_over_budget`, `review_unavailable`): the existing
/// verification-gap guards, then a receipt sealed from the run's saved
/// snapshot, published with the resolution (see [`approval`]). Repeated on
/// the answered run it finds the same receipt and writes nothing.
fn approve_stopped_review(
    ledger: &Ledger,
    runs_dir: &Path,
    run: &RunId,
    state: State,
    answerer: relais::ledger::Answerer<'_>,
    note: Option<&str>,
) -> Result<Decision, CliError> {
    if state.awaits_a_person() {
        let gaps = open_verification_gaps(ledger, run)?;
        if !gaps.is_empty() {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run} still carries verification gaps ({}) — a waiver is \
                     a policy or contract change, never a CLI flag",
                    gaps.join("; ")
                ),
            ));
        }
    }
    let answer = relais::ledger::DecisionAnswer {
        resolution: Reason::DecisionApproved,
        actor: answerer.actor,
        person_quote: answerer.person_quote,
        recorded_via: answerer.recorded_via,
        note,
        successor_run: None,
        from_state: state,
        to_state: DecideAnswer::Approve.terminal_state(),
    };
    let sealed = match approval::approve_from_snapshot(ledger, runs_dir, run, &answer, &|| Ok(())) {
        Ok(sealed) => sealed,
        Err(e @ (approval::ApprovalError::Io(_) | approval::ApprovalError::Ledger(_))) => {
            return Err(CliError::Operational {
                operation: "decide",
                cause: Box::new(e),
            })
        }
        Err(refused) => return Ok(refusal(CliOutcome::InvalidInput, refused.to_string())),
    };
    let sentence = if sealed.written {
        format!(
            "recorded {} for {run} by {}; receipt {} sealed at {}",
            Reason::DecisionApproved.as_str(),
            answerer.actor,
            sealed.receipt_hash,
            sealed.artifact.display()
        )
    } else {
        format!(
            "run {run} was already approved; receipt {} is sealed, nothing written",
            sealed.receipt_hash
        )
    };
    Ok(Ok(Decided {
        run: run.to_string(),
        resolution: Reason::DecisionApproved,
        actor: answerer.actor,
        from_state: state,
        to_state: DecideAnswer::Approve.terminal_state(),
        advice: operational(ledger.decision_advice(run), "decide")?,
        sentence,
        successor_intent: None,
    }))
}

/// `relais decide --answer revise --widen-scope | --successor-contract
/// <file>`: the answer and the successor intent that carries it out are one
/// ledger transaction (plan A5). Repeated on an answered decision it returns
/// the intent the first answer wrote.
pub(crate) fn successor_decide(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    request: &DecideRequest<'_>,
    runs_dir: &Path,
    answerer: relais::ledger::Answerer<'_>,
) -> Result<Decision, CliError> {
    use relais::runner::successor;
    let in_parts = request.answer == DecideAnswer::ReviewParts;
    let supplied = match request.successor_contract {
        Some(path) => Some(load_contract(path)?),
        None => None,
    };
    let decided =
        |intent: relais::ledger::SuccessorIntent, sentence: String| -> Result<Decision, CliError> {
            Ok(Ok(Decided {
                run: run.to_string(),
                resolution: Reason::DecisionRevised,
                actor: answerer.actor,
                from_state: state,
                to_state: DecideAnswer::Revise.terminal_state(),
                advice: operational(ledger.decision_advice(run), "decide")?,
                sentence,
                successor_intent: Some(intent_json(ledger, &intent)),
            }))
        };
    // Repeated: the decision is answered and its intent exists.
    if let Some(existing) = operational(ledger.intent_of_decision(run), "decide")? {
        if let Some(contract) = &supplied {
            if contract.hash() != existing.contract_digest {
                return Ok(refusal(
                    CliOutcome::InvalidInput,
                    format!(
                        "run {run} was already answered with successor intent {} for another \
                         contract ({}); a decision has one successor",
                        existing.intent_id, existing.contract_digest
                    ),
                ));
            }
        }
        if in_parts != (existing.review == Some(relais::ledger::IntentReview::Parts)) {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run} was already answered with successor intent {} ({}); a decision \
                     has one successor",
                    existing.intent_id,
                    existing
                        .review
                        .map_or("a revision", |_| "a review in parts")
                ),
            ));
        }
        let sentence = format!(
            "run {run} was already answered; successor intent {} is {}",
            existing.intent_id,
            existing.state.as_str()
        );
        return decided(existing, sentence);
    }
    if !state.awaits_a_person() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run} is {state}, not waiting on a person — nothing to decide"),
        ));
    }
    let planned = (|| -> Result<_, successor::PlanError> {
        if in_parts {
            return successor::plan_review_parts(ledger, run, runs_dir);
        }
        let (contract, scope_delta) = match supplied {
            Some(contract) => (contract, Vec::new()),
            None if request.rerun_verify => {
                successor::require_resource_exhausted(ledger, run)?;
                let (original, _tier) = ledger.run_contract_and_tier(run)?.ok_or_else(|| {
                    successor::PlanError::Missing(format!("run {run} carries no recorded contract"))
                })?;
                let rerun = successor::widened_contract(&original, run, &[])
                    .map_err(successor::PlanError::Contract)?;
                (rerun, Vec::new())
            }
            None => {
                let paths = successor::out_of_scope_paths(ledger, run)?;
                let (original, _tier) = ledger.run_contract_and_tier(run)?.ok_or_else(|| {
                    successor::PlanError::Missing(format!("run {run} carries no recorded contract"))
                })?;
                let widened = successor::widened_contract(&original, run, &paths)
                    .map_err(successor::PlanError::Contract)?;
                (widened, paths)
            }
        };
        successor::plan(ledger, run, &contract, scope_delta, runs_dir)
    })();
    let intent = match planned {
        Ok(intent) => intent,
        Err(e) => return Ok(refusal(CliOutcome::InvalidInput, e.to_string())),
    };
    let written = operational(
        ledger.resolve_decision_with_intent(
            run,
            &relais::ledger::DecisionAnswer {
                resolution: Reason::DecisionRevised,
                actor: answerer.actor,
                person_quote: answerer.person_quote,
                recorded_via: answerer.recorded_via,
                note: request.note,
                successor_run: None,
                from_state: state,
                to_state: DecideAnswer::Revise.terminal_state(),
            },
            &intent,
        ),
        "decide",
    )?;
    let Some(stored) = written else {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run} has no open decision on record"),
        ));
    };
    let sentence = format!(
        "recorded {} for {run} by {}; successor intent {} is pending",
        Reason::DecisionRevised.as_str(),
        answerer.actor,
        stored.intent_id
    );
    decided(stored, sentence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use relais::ids::TaskId;
    use relais::ledger::{Transition, VerificationSnapshot};
    use relais::money::{CostCompleteness, MicroUsd};
    use relais::verify::{LadderRecord, Receipt, ReviewNotRun, ReviewRecord, VerificationReport};

    fn receipt(run: &str) -> Receipt {
        Receipt {
            run_id: run.into(),
            candidate_sha: "cand".into(),
            base_sha: "base".into(),
            contract_hash: "ch".into(),
            policy_hash: "ph".into(),
            outcome: State::NeedsDecision.as_str().into(),
            verification: VerificationReport {
                candidate_sha: "cand".into(),
                base_sha: "base".into(),
                contract_hash: "ch".into(),
                policy_hash: "ph".into(),
                checks: Vec::new(),
                gaps: Vec::new(),
                gaps_not_judged: Vec::new(),
                notes: Vec::new(),
                baseline_failures: Vec::new(),
                amont_bypasses: Vec::new(),
                amont_downgrades: Vec::new(),
                verification_inputs_changed: Vec::new(),
                integration_gaps: Vec::new(),
                baseline_cached: false,
                baseline_cache_refused: None,
            },
            kind: relais::contract::Kind::Change,
            candidate: None,
            predecessor: None,
            models_used: Vec::new(),
            attempts: 1,
            cost_completeness: CostCompleteness::Actual,
            cost: MicroUsd::from_micros(1),
            criteria: Vec::new(),
            mandatory_evidence_independence: None,
            recipe: relais::policy::RecipeRecord::NotRecorded,
            verification_profile_hash: "vph".into(),
            review: ReviewRecord::NotRun(ReviewNotRun::OverBudget {
                patch_bytes: 300_000,
                budget_bytes: 196_608,
            }),
            ladder: LadderRecord::NotRecorded,
            efforts_used: Vec::new(),
            candidates: relais::verify::CandidatesRecord::NotRecorded,
        }
    }

    /// A run waiting on a person since `reason`, with a snapshot for
    /// `snapshot_candidate` when given.
    fn waiting_run(
        ledger: &Ledger,
        name: &str,
        repo: &str,
        state: State,
        reason: Reason,
        detail: Option<serde_json::Value>,
        snapshot_candidate: Option<&str>,
    ) {
        let run = RunId::from_stored(name);
        ledger
            .insert_run(&run, repo, None, &TaskId::from_stored(name), "rk")
            .expect("run");
        let revision = ledger
            .insert_contract_revision(&run, "h", "{}", "HEAD", None)
            .expect("revision");
        let attempt = ledger
            .insert_attempt(
                &run,
                revision,
                1,
                "implementation",
                relais::lifecycle::UsagePhase::Initial,
            )
            .expect("attempt");
        ledger
            .finish_attempt(attempt, State::Running, None, Some("cand"))
            .expect("candidate");
        for (from, to, why, detail) in [
            (
                State::Prepared,
                State::Running,
                Reason::WorkerDispatched,
                None,
            ),
            (State::Running, state, reason, detail),
        ] {
            ledger
                .record_transition(&Transition {
                    run_id: run.clone(),
                    attempt_id: None,
                    from_state: Some(from),
                    to_state: to,
                    reason: why.as_str().into(),
                    detail,
                    at: ledger.now(),
                })
                .expect("transition");
        }
        if let Some(candidate) = snapshot_candidate {
            let mut saved = receipt(name);
            saved.candidate_sha = candidate.into();
            saved.verification.candidate_sha = candidate.into();
            ledger
                .save_verification_snapshot(
                    &run,
                    &VerificationSnapshot {
                        candidate_sha: candidate.into(),
                        base_sha: "base".into(),
                        contract_hash: "ch".into(),
                        policy_hash: "ph".into(),
                        stop_reason: reason.as_str().into(),
                        receipt: serde_json::to_value(&saved).expect("receipt"),
                    },
                )
                .expect("snapshot");
        }
    }

    fn approve(dir: &Path, run_id: &str) -> Decision {
        let locations = || {
            Ok(StateLocations {
                ledger: dir.join("ledger.sqlite"),
                runs: dir.join("runs"),
            })
        };
        decide_in(
            &DecideRequest {
                run_id,
                answer: DecideAnswer::Approve,
                actor: "orchestrator",
                person_quote: None,
                note: None,
                successor: None,
                criterion: None,
                candidate: None,
                widen_scope: false,
                rerun_verify: false,
                successor_contract: None,
                json: false,
                proof_stdin: false,
            },
            &locations,
        )
        .expect("decide runs")
    }

    fn exit_of(decision: &Decision) -> i32 {
        match decision {
            Ok(_) => exit_code(&CliOutcome::Accepted),
            Err(refused) => exit_code(&refused.outcome),
        }
    }

    #[test]
    fn approving_a_review_stop_seals_once_and_refuses_what_it_must() {
        let dir = relais::test_support::short_temp_dir("decide-review");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let gaps = serde_json::json!({"gaps": ["declared-check: status `inert`"]});
        waiting_run(
            &ledger,
            "run-ok",
            "/repo",
            State::NeedsDecision,
            Reason::ReviewOverBudget,
            None,
            Some("cand"),
        );
        waiting_run(
            &ledger,
            "run-gaps",
            "/repo",
            State::NeedsDecision,
            Reason::ReviewOverBudget,
            Some(gaps),
            Some("cand"),
        );
        waiting_run(
            &ledger,
            "run-old",
            "/repo",
            State::NeedsReview,
            Reason::ReviewUnavailable,
            None,
            None,
        );
        waiting_run(
            &ledger,
            "run-stale",
            "/repo",
            State::NeedsDecision,
            Reason::ReviewOverBudget,
            None,
            Some("other"),
        );

        // First approve: sealed, exit 0.
        let first = approve(&dir, "run-ok");
        assert_eq!(exit_of(&first), 0);
        let decided = first.expect("approved");
        assert!(
            decided.sentence.contains("sealed at"),
            "{}",
            decided.sentence
        );
        let runs = dir.join("runs");
        let mut expected = receipt("run-ok");
        expected.outcome = State::Accepted.as_str().into();
        let (_, row_hash) = ledger
            .receipt(&RunId::from_stored("run-ok"))
            .expect("read")
            .expect("a receipt row");
        assert_eq!(row_hash, expected.hash());
        let receipts = std::fs::read_dir(runs.join("run-ok").join("receipts"))
            .expect("receipts dir")
            .count();
        assert_eq!(receipts, 1);
        assert_eq!(
            std::fs::read_to_string(runs.join("run-ok").join("receipt.json"))
                .expect("receipt.json"),
            serde_json::to_string_pretty(&expected).expect("serializes"),
        );

        // Repeat approve: the receipt row matches, so it exits 0 and writes
        // nothing; with `receipt.json` deleted it rewrites the same bytes.
        let before = std::fs::read(runs.join("run-ok").join("receipt.json")).expect("bytes");
        let again = approve(&dir, "run-ok");
        assert_eq!(exit_of(&again), 0);
        assert!(again.expect("repeat").sentence.contains("nothing written"));
        std::fs::remove_file(runs.join("run-ok").join("receipt.json")).expect("delete");
        assert_eq!(exit_of(&approve(&dir, "run-ok")), 0);
        assert_eq!(
            std::fs::read(runs.join("run-ok").join("receipt.json")).expect("healed"),
            before
        );
        assert_eq!(
            std::fs::read_dir(runs.join("run-ok").join("receipts"))
                .expect("receipts dir")
                .count(),
            1
        );

        // The verification-gap guard refuses, and seals nothing.
        let gapped = approve(&dir, "run-gaps");
        assert_eq!(exit_of(&gapped), 2);
        assert!(gapped
            .err()
            .expect("refused")
            .message
            .contains("verification gaps"));
        assert!(!runs.join("run-gaps").exists());

        // A snapshot of another candidate is refused.
        let stale = approve(&dir, "run-stale");
        assert_eq!(exit_of(&stale), 2);
        assert!(stale.err().expect("refused").message.contains("stale"));
        assert!(!runs.join("run-stale").exists());

        // A stop with no snapshot that is not over budget resolves as before.
        let old = approve(&dir, "run-old");
        assert_eq!(exit_of(&old), 0);
        assert_eq!(
            ledger
                .receipt(&RunId::from_stored("run-old"))
                .expect("read"),
            None
        );
        assert!(!runs.join("run-old").exists());
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = relais::workspace::git_command(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A linked worktree whose run was interrupted: its marker is on disk,
    /// the lease released, and the ledger holds the run waiting as
    /// `interrupted` with the worktree as its workspace.
    fn interrupted_in_place(dir: &Path, name: &str) -> (Ledger, PathBuf) {
        use relais::workspace::inplace::{Lease, Marker, Stage};
        let live = dir.join("live");
        std::fs::create_dir_all(&live).expect("mkdir");
        git(&live, &["init", "-q", "-b", "main"]);
        git(&live, &["config", "user.email", "t@t"]);
        git(&live, &["config", "user.name", "t"]);
        git(&live, &["config", "commit.gpgsign", "false"]);
        let no_hooks = dir.join("no-hooks");
        std::fs::create_dir_all(&no_hooks).expect("mkdir");
        git(
            &live,
            &["config", "core.hooksPath", &no_hooks.to_string_lossy()],
        );
        std::fs::write(live.join("a.txt"), "a\n").expect("a");
        git(&live, &["add", "-A"]);
        git(&live, &["commit", "-q", "-m", "base"]);
        let worktree = dir.join("wt");
        git(
            &live,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "task",
                &worktree.to_string_lossy(),
            ],
        );
        let lease = Lease::acquire(&worktree, name).expect("lease");
        let start = relais::workspace::inplace::start_snapshot(&lease).expect("start");
        lease
            .write_marker(&Marker {
                run: name.to_string(),
                stage: Stage::Running,
                head_symbolic: start.head_symbolic.clone(),
                head_sha: start.head_sha.clone(),
                start: start.sha.clone(),
                index_listing: start.index_listing.clone(),
                target: None,
                exported: None,
            })
            .expect("marker");
        drop(lease);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            name,
            &worktree.to_string_lossy(),
            State::Interrupted,
            Reason::ReconciledInterrupted,
            None,
            None,
        );
        (ledger, worktree)
    }

    fn answer_with(dir: &Path, run_id: &str, answer: DecideAnswer) -> Decided {
        let locations = || {
            Ok(StateLocations {
                ledger: dir.join("ledger.sqlite"),
                runs: dir.join("runs"),
            })
        };
        decide_in(
            &DecideRequest {
                run_id,
                answer,
                actor: "orchestrator",
                person_quote: None,
                note: None,
                successor: None,
                criterion: None,
                candidate: (answer == DecideAnswer::Salvaged).then_some("cand"),
                widen_scope: false,
                rerun_verify: false,
                successor_contract: None,
                json: false,
                proof_stdin: false,
            },
            &locations,
        )
        .expect("decide runs")
        .expect("answered")
    }

    fn marker_exists(worktree: &Path, run: &str) -> bool {
        let git_dir = relais::workspace::inplace::git_dir(worktree).expect("git dir");
        git_dir
            .join("relais-inplace")
            .join(format!("{run}.json"))
            .exists()
    }

    fn move_head(worktree: &Path) {
        std::fs::write(worktree.join("a.txt"), "moved\n").expect("edit");
        git(worktree, &["commit", "-q", "-am", "worker"]);
    }

    #[test]
    fn salvaging_an_interrupted_run_whose_head_moved_frees_its_worktree() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-salvage");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-moved");
        move_head(&worktree);
        let decided = answer_with(&dir, "run-moved", DecideAnswer::Salvaged);
        assert!(decided.sentence.contains("kept"), "{}", decided.sentence);
        assert!(!marker_exists(&worktree, "run-moved"));
        relais::workspace::inplace::Lease::acquire(&worktree, "run-next")
            .unwrap_or_else(|_| panic!("a new run is not refused"));
    }

    #[test]
    fn abandoning_an_interrupted_run_whose_head_moved_frees_its_worktree() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-abandon");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-moved");
        move_head(&worktree);
        answer_with(&dir, "run-moved", DecideAnswer::Abandon);
        assert!(!marker_exists(&worktree, "run-moved"));
        relais::workspace::inplace::Lease::acquire(&worktree, "run-next")
            .unwrap_or_else(|_| panic!("a new run is not refused"));
    }

    #[test]
    fn answering_an_interrupted_run_whose_head_did_not_move_keeps_the_marker_and_names_the_retire()
    {
        let dir = relais::test_support::short_temp_dir("decide-inplace-unmoved");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-still");
        let decided = answer_with(&dir, "run-still", DecideAnswer::Decided);
        assert!(marker_exists(&worktree, "run-still"));
        assert!(
            decided
                .sentence
                .contains("relais resume --retire run-still"),
            "{}",
            decided.sentence
        );
    }

    #[test]
    fn a_corrupt_marker_is_reported_and_the_answer_still_recorded() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-corrupt");
        let (ledger, worktree) = interrupted_in_place(&dir, "run-corrupt");
        let git_dir = relais::workspace::inplace::git_dir(&worktree).expect("git dir");
        std::fs::write(
            git_dir.join("relais-inplace").join("run-corrupt.json"),
            "not json",
        )
        .expect("corrupt");
        let decided = answer_with(&dir, "run-corrupt", DecideAnswer::Abandon);
        assert!(
            decided.sentence.contains("marker") && decided.sentence.contains("could not be read"),
            "{}",
            decided.sentence
        );
        assert!(decided
            .sentence
            .contains("relais resume --retire run-corrupt"));
        assert_eq!(
            ledger
                .run_status(&RunId::from_stored("run-corrupt"))
                .expect("status"),
            Some(DecideAnswer::Abandon.terminal_state())
        );
    }

    #[test]
    fn an_unreadable_head_is_reported_instead_of_claiming_it_did_not_move() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-head");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-head");
        let git_dir = relais::workspace::inplace::git_dir(&worktree).expect("git dir");
        let branch = git(&worktree, &["symbolic-ref", "HEAD"]);
        let reference = git_dir
            .parent()
            .and_then(Path::parent)
            .expect("common dir")
            .join(branch);
        std::fs::write(&reference, "garbage\n").expect("break the branch");
        let decided = answer_with(&dir, "run-head", DecideAnswer::Decided);
        assert!(
            decided.sentence.contains("HEAD could not be read"),
            "{}",
            decided.sentence
        );
        assert!(
            !decided.sentence.contains("HEAD did not move"),
            "{}",
            decided.sentence
        );
    }

    #[test]
    fn answering_a_run_that_is_not_in_place_says_nothing_about_a_worktree() {
        let dir = relais::test_support::short_temp_dir("decide-not-in-place");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-plain",
            "/repo",
            State::Interrupted,
            Reason::ReconciledInterrupted,
            None,
            None,
        );
        let decided = answer_with(&dir, "run-plain", DecideAnswer::Abandon);
        assert_eq!(
            decided.sentence,
            format!(
                "recorded {} for run-plain by orchestrator",
                Reason::DecisionAbandoned.as_str()
            )
        );
    }
}
