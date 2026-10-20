//! `relais decide`.

use super::evidence::*;
use super::feedback::*;
use super::salvage::*;
use crate::*;

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
            Actor::Orchestrator => {
                return refusal(
                    CliOutcome::InvalidInput,
                    "`--criterion` signs off a human criterion and an orchestrator never does: \
                     the person signs it off from their own terminal (`--actor person`)",
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

pub(crate) fn decide_inner(request: &DecideRequest<'_>) -> Result<Decision, CliError> {
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
    let ledger = open_ledger()?;
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
    if request.widen_scope || request.rerun_verify || request.successor_contract.is_some() {
        if answer != DecideAnswer::Revise {
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
            relais::ledger::Answerer {
                actor,
                person_quote,
                recorded_via,
            },
        );
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

/// `relais decide --answer revise --widen-scope | --successor-contract
/// <file>`: the answer and the successor intent that carries it out are one
/// ledger transaction (plan A5). Repeated on an answered decision it returns
/// the intent the first answer wrote.
pub(crate) fn successor_decide(
    ledger: &Ledger,
    run: &RunId,
    state: State,
    request: &DecideRequest<'_>,
    answerer: relais::ledger::Answerer<'_>,
) -> Result<Decision, CliError> {
    use relais::runner::successor;
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
        let runs_dir =
            paths::runs_dir().map_err(|e| successor::PlanError::Missing(e.to_string()))?;
        successor::plan(ledger, run, &contract, scope_delta, &runs_dir)
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
