//! `relais decide`.

use super::ctx::load_contract;
use super::ctx::refusal;
use super::ctx::Refusal;
use super::decision_record::answer_error;
use super::decision_record::print_error_object;
use super::decision_record::print_refusal;
use super::decision_record::read_decide_proof;
use super::decision_record::Decided;
use super::decision_record::Decision;
use super::decision_record::NextStep;
use super::evidence::authorship_label;
use super::evidence::reseal_receipt_with_signoff;
use super::feedback::open_verification_gaps;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use super::programme::Format;
use super::salvage::salvage_command;
use super::salvage::SalvageAnswer;
use super::status::intent_json;
use super::DecideAnswer;
use relais::acceptance::Evidence;
use relais::ids::RunId;
use relais::ledger::Ledger;
use relais::paths;
use relais::runner::approval;
use relais::runner::approval::seals_on_approve;
use relais::runner::Reason;
use relais::runner::State;
use relais::shared_state::node;
use relais::shared_state::SharedState;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

pub(crate) fn decide_command(request: DecideRequest<'_>) -> Result<CliOutcome, CliError> {
    let json = request.json;
    match decide_inner(&request) {
        Ok(Ok(done)) => {
            if json {
                println!("{}", decided_json(&done));
            } else {
                println!("{}", done.sentence);
            }
            // A warning reaches stderr under `--json` too (`print_refusal`'s
            // rule); the object carries it as `receipt_skipped` as well.
            if let Some(line) = done.receipt_skipped_line() {
                eprintln!("{line}");
            }
            Ok(CliOutcome::Accepted)
        }
        Ok(Err(refused)) => {
            print_refusal(
                "relais decide",
                Format::from_json_flag(json),
                &refused.outcome,
                &refused.message,
            );
            Ok(refused.outcome)
        }
        Err(e) => {
            print_error_object(Format::from_json_flag(json), &e.outcome(), &e.to_string());
            Err(e)
        }
    }
}

/// The one stdout object `relais decide --json` prints for an answer it
/// recorded. An approval that wrote no receipt adds `"receipt": null` and
/// `"receipt_skipped"`, the stop reason it answered.
fn decided_json(done: &Decided) -> serde_json::Value {
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
    if let Some(reason) = &done.receipt_skipped {
        printed["receipt"] = serde_json::Value::Null;
        printed["receipt_skipped"] = serde_json::json!(reason);
    }
    printed
}

/// Why a `person-via-orchestrator` sign-off without a live plugin nonce is
/// refused: only the plugin's proved call counts as the person's
/// (ADR-0004).
pub(crate) const UNPROVED_SIGNOFF: &str =
    "`--criterion` is refused for `--actor person-via-orchestrator` \
     without the plugin's proof: only an answer recorded through the decide tool counts as the \
     person's sign-off";

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
    decide_in(
        request,
        &locations_from_environment,
        &super::ctx::shared_state,
    )
}

/// [`decide_inner`] with the state locations passed in, so a test names its
/// own directory instead of setting process-wide environment.
pub(crate) fn decide_in(
    request: &DecideRequest<'_>,
    locate: &dyn Fn() -> Result<StateLocations, CliError>,
    shared: &dyn Fn() -> Result<Arc<dyn SharedState>, CliError>,
) -> Result<Decision, CliError> {
    if request.answer == DecideAnswer::Abandon {
        if let Some(fence) = standing_worker_fence(locate, request.run_id)? {
            return Ok(refusal(CliOutcome::ResumeRefusedLive, fence));
        }
    }
    let mut decision = record_answer(request, locate, shared)?;
    if let Ok(done) = &mut decision {
        let frees_in_place = matches!(
            request.answer,
            DecideAnswer::Salvaged | DecideAnswer::Abandon | DecideAnswer::Decided
        );
        if frees_in_place && done.from_state == State::Interrupted {
            // The answer is already recorded: whatever goes wrong freeing
            // the worktree is reported inside the answer, never as an error.
            let report = match free_after_answer(locate, done, request.answer) {
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

/// The refusal text when `run` is an in-place run that lost a worker nothing
/// proves stopped: abandoning it would hand the tree to the person while the
/// worker may still write it. `None` when the run holds no such fence, or
/// when its marker cannot be read (that is reported when the tree is freed).
fn standing_worker_fence(
    locate: &dyn Fn() -> Result<StateLocations, CliError>,
    run_id: &str,
) -> Result<Option<String>, CliError> {
    use relais::workspace::inplace;
    let locations = locate()?;
    let ledger = operational(Ledger::open(&locations.ledger), "the ledger is unavailable")?;
    let run = RunId::from_stored(run_id);
    let Some(workspace) = operational(ledger.run_workspace(&run), "decide")? else {
        return Ok(None);
    };
    let marker = inplace::git_dir(&workspace.repo_path)
        .and_then(|git_dir| inplace::read_marker(&git_dir, run.as_str()));
    Ok(marker
        .ok()
        .and_then(|marker| marker.orphan)
        .filter(|orphan| orphan.standing())
        .map(|orphan| orphan.fence_text(run.as_str(), &relais::ledger::now_rfc3339())))
}

fn free_after_answer(
    locate: &dyn Fn() -> Result<StateLocations, CliError>,
    done: &Decided,
    answer: DecideAnswer,
) -> Result<Option<String>, CliError> {
    let locations = locate()?;
    let ledger = operational(Ledger::open(&locations.ledger), "the ledger is unavailable")?;
    let run = RunId::from_stored(&done.run);
    free_kept_in_place(&ledger, &run, answer, done.to_state, &locations.runs)
}

/// What answering an interrupted in-place run does to its worktree marker.
/// A moved HEAD means the tree and branch are the person's, so the marker
/// is retired (the code `relais resume --retire` runs); so it is for an
/// abandon whose HEAD did not move and whose working tree equals the start
/// snapshot, since nothing is kept to lose. Otherwise it stays and the command that retires it is named. Only a
/// missing marker means there is nothing to free; every other failure is
/// named in the returned report.
fn free_kept_in_place(
    ledger: &Ledger,
    run: &RunId,
    answer: DecideAnswer,
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
    let nothing_kept = !head_moved
        && answer == DecideAnswer::Abandon
        && tree_equals_start(&workspace.repo_path, &git_dir, run, &marker.start);
    if !head_moved && !nothing_kept {
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

/// Whether the working tree (untracked non-ignored files included) is the
/// start snapshot's tree. Any failure to tell answers no: the marker stays.
fn tree_equals_start(root: &Path, git_dir: &Path, run: &RunId, start: &str) -> bool {
    use relais::workspace::inplace;
    let index = git_dir.join(format!("relais-decide-{run}.index"));
    let now = inplace::working_tree(root, &index);
    let _ = std::fs::remove_file(&index); // scratch index; `working_tree` deletes a stale one before it rebuilds
    let Ok(now) = now else { return false };
    let start_tree = relais::workspace::git_command(root)
        .args(["rev-parse", &format!("{start}^{{tree}}")])
        .output();
    match start_tree {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == now,
        Ok(_) | Err(_) => false,
    }
}

fn record_answer(
    request: &DecideRequest<'_>,
    locate: &dyn Fn() -> Result<StateLocations, CliError>,
    shared: &dyn Fn() -> Result<Arc<dyn SharedState>, CliError>,
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
            shared,
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
            shared,
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
                if decision.resolved_at.is_none() && seals_on_approve(&decision.raised_reason) =>
            {
                let has_snapshot =
                    operational(ledger.verification_snapshot(&run), "decide")?.is_some();
                has_snapshot || decision.raised_reason == Reason::ReviewOverBudget.as_str()
            }
            // Resolved with a receipt row: a repeat. `approve_from_snapshot`
            // finds it matches the snapshot's receipt (exit 0, heals a missing
            // `receipt.json`) or refuses it by name.
            Some(decision)
                if decision.resolved_at.is_some() && seals_on_approve(&decision.raised_reason) =>
            {
                operational(ledger.receipt(&run), "decide")?.is_some()
                    && operational(ledger.verification_snapshot(&run), "decide")?.is_some()
            }
            _ => false,
        };
        if open_review_stop {
            return approve_stopped_review(
                &*shared()?,
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
        let acceptance = operational(ledger.effective_acceptance(&run, &contract), "decide")?;
        let declared: Vec<String> = acceptance.iter().map(|entry| entry.id()).collect();
        let Some(entry) = acceptance.iter().find(|entry| entry.id() == criterion_id) else {
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
    // The stop an approval answers, read before it resolves: an approval
    // that leaves the run with no receipt names it (`receipt_skipped`).
    let raised_reason = if answer == DecideAnswer::Approve {
        operational(ledger.decision_of_run(&run), "decide")?
            .filter(|decision| decision.resolved_at.is_none())
            .map(|decision| decision.raised_reason)
    } else {
        None
    };
    let resolved = match node::resolve_decision(
        &*shared()?,
        &ledger,
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
    ) {
        Ok(resolved) => resolved,
        Err(e) => return answer_error(e, &run, criterion, NextStep::AnswerAgain),
    };
    if !resolved {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run_id} has no open decision on record"),
        ));
    }
    let receipt_skipped = match raised_reason {
        Some(reason) if operational(ledger.receipt(&run), "decide")?.is_none() => Some(reason),
        _ => None,
    };
    Ok(Ok(Decided {
        run: run_id.to_string(),
        resolution,
        actor,
        from_state: state,
        to_state,
        advice: operational(ledger.decision_advice(&run), "decide")?,
        sentence: format!("recorded {} for {run_id} by {actor}", resolution.as_str()),
        successor_intent: None,
        receipt_skipped,
    }))
}

/// `relais decide --answer approve` on a run that stopped for a review it
/// could not have (`review_over_budget`, `review_unavailable`): the existing
/// verification-gap guards, then a receipt sealed from the run's saved
/// snapshot, published with the resolution (see [`approval`]). Repeated on
/// the answered run it finds the same receipt and writes nothing.
fn approve_stopped_review(
    shared: &dyn SharedState,
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
    let approve = || approval::approve_from_snapshot(ledger, runs_dir, run, &answer, &|| Ok(()));
    // A repeat on an answered decision writes nothing and asks no grant: it
    // is the idempotent exit 0 that heals `receipt.json`.
    let answered = operational(ledger.decision_of_run(run), "decide")?
        .is_some_and(|decision| decision.resolved_at.is_some());
    let sealed = if answered {
        approve()
    } else {
        let closed = |sealed: &Result<approval::Sealed, approval::ApprovalError>| match sealed {
            // `Ok` is a sealed receipt, or the same one already sealed by a
            // racing answer: the decision is closed either way.
            Ok(_) => true,
            // A failure after the ledger commit (the `receipt.json` write)
            // left the decision closed: its grant stays. A read that fails
            // keeps it too; it lapses within its TTL.
            Err(_) => match ledger.decision_of_run(run) {
                Ok(decision) => decision.is_some_and(|d| d.resolved_at.is_some()),
                Err(e) => {
                    eprintln!(
                        "relais decide: the decision of run {run} could not be re-read ({e}); \
                         the approval may be recorded: check `relais status {run}` before \
                         answering again"
                    );
                    true
                }
            },
        };
        match node::answer_under_grant(shared, run, approve, closed) {
            Ok(sealed) => sealed,
            Err(e) => return answer_error(e, run, None, NextStep::AnswerAgain),
        }
    };
    let sealed = match sealed {
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
        receipt_skipped: None,
    }))
}

/// `relais decide --answer revise --widen-scope | --successor-contract
/// <file>`: the answer and the successor intent that carries it out are one
/// ledger transaction (plan A5). Repeated on an answered decision it returns
/// the intent the first answer wrote.
pub(crate) fn successor_decide(
    shared: &dyn Fn() -> Result<Arc<dyn SharedState>, CliError>,
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
                receipt_skipped: None,
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
    let written = match node::resolve_decision_with_intent(
        &*shared()?,
        ledger,
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
    ) {
        Ok(written) => written,
        Err(e) => return answer_error(e, run, None, NextStep::AnswerAgain),
    };
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
    use crate::cli::outcome::exit_code;
    use relais::ids::TaskId;
    use relais::ledger::{Transition, VerificationSnapshot};
    use relais::money::{CostCompleteness, MicroUsd};
    use relais::shared_state::node::NodeError;
    use relais::shared_state::SharedStateError;
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
                baseline_proven: false,
                baseline_deferred: false,
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
            review_level: None,
            review: ReviewRecord::NotRun(ReviewNotRun::OverBudget {
                patch_bytes: 300_000,
                budget_bytes: 196_608,
            }),
            ladder: LadderRecord::NotRecorded,
            efforts_used: Vec::new(),
            candidates: relais::verify::CandidatesRecord::NotRecorded,
            holdouts: Vec::new(),
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

    /// The state every pre-existing test runs under: `LocalState`, which
    /// always grants.
    fn local_shared() -> Result<Arc<dyn SharedState>, CliError> {
        Ok(Arc::new(relais::shared_state::LocalState::new()))
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
            &local_shared,
        )
        .expect("decide runs")
    }

    /// `LocalState` in every answer but one: the decision grant is held by
    /// another answerer (`held`), and each ask is counted. The binary's
    /// tests cannot see `FakeState` (it is behind the `conformance`
    /// feature), so this stands in for a shared half that refuses.
    struct Elsewhere {
        local: relais::shared_state::LocalState,
        held: bool,
        asks: std::sync::atomic::AtomicUsize,
        releases: std::sync::atomic::AtomicUsize,
    }

    impl Elsewhere {
        fn holding() -> Self {
            Self::with(true)
        }

        fn granting() -> Self {
            Self::with(false)
        }

        fn with(held: bool) -> Self {
            Self {
                local: relais::shared_state::LocalState::new(),
                held,
                asks: std::sync::atomic::AtomicUsize::new(0),
                releases: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn asks(&self) -> usize {
            self.asks.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn releases(&self) -> usize {
            self.releases.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl SharedState for Elsewhere {
        fn selection(&self) -> relais::shared_state::Selection {
            self.local.selection()
        }
        fn machine_settings(&self) -> Result<relais::policy::MachineSettings, SharedStateError> {
            self.local.machine_settings()
        }
        fn write_settings(
            &self,
            writer: &relais::shared_state::Principal,
            edit: &relais::shared_state::SettingsEdit,
        ) -> Result<relais::shared_state::SettingsWrite, SharedStateError> {
            self.local.write_settings(writer, edit)
        }
        fn daily_spent(
            &self,
            day_start: &str,
        ) -> Result<Option<relais::money::MicroUsd>, SharedStateError> {
            self.local.daily_spent(day_start)
        }
        fn slice(&self) -> Result<relais::shared_state::Slice, SharedStateError> {
            self.local.slice()
        }
        fn reserve_spend(
            &self,
            seat: &relais::shared_state::LeaseToken,
            amount: relais::money::MicroUsd,
        ) -> Result<relais::shared_state::Reservation, SharedStateError> {
            self.local.reserve_spend(seat, amount)
        }
        fn settle_spend(
            &self,
            reservation: &relais::shared_state::Reservation,
            actual: relais::money::MicroUsd,
        ) -> Result<relais::shared_state::Settlement, SharedStateError> {
            self.local.settle_spend(reservation, actual)
        }
        fn acquire_seat(
            &self,
            holder: &relais::shared_state::Principal,
            dispatch: &str,
        ) -> Result<relais::shared_state::LeaseToken, SharedStateError> {
            self.local.acquire_seat(holder, dispatch)
        }
        fn renew_seat(
            &self,
            lease: &relais::shared_state::LeaseToken,
        ) -> Result<relais::shared_state::LeaseToken, SharedStateError> {
            self.local.renew_seat(lease)
        }
        fn release_seat(
            &self,
            lease: &relais::shared_state::LeaseToken,
        ) -> Result<(), SharedStateError> {
            self.local.release_seat(lease)
        }
        fn claim_ticket(
            &self,
            request: &relais::shared_state::TicketRequest,
        ) -> Result<relais::shared_state::TicketAnswer, SharedStateError> {
            self.local.claim_ticket(request)
        }
        fn grant_ticket(
            &self,
            request: &relais::shared_state::TicketRequest,
        ) -> Result<relais::shared_state::LeaseToken, SharedStateError> {
            self.local.grant_ticket(request)
        }
        fn release_ticket(
            &self,
            lease: &relais::shared_state::LeaseToken,
        ) -> Result<(), SharedStateError> {
            self.local.release_ticket(lease)
        }
        fn reserve_handoff(
            &self,
            request: &relais::shared_state::TicketRequest,
        ) -> Result<relais::shared_state::TicketAnswer, SharedStateError> {
            self.local.reserve_handoff(request)
        }
        fn renew_tickets(
            &self,
            holder: &relais::shared_state::Principal,
        ) -> Result<(), SharedStateError> {
            self.local.renew_tickets(holder)
        }
        fn release_tickets(
            &self,
            holder: &relais::shared_state::Principal,
        ) -> Result<(), SharedStateError> {
            self.local.release_tickets(holder)
        }
        fn claim_intent(
            &self,
            intent_id: &str,
            run: &RunId,
            replacing: Option<&RunId>,
        ) -> Result<relais::shared_state::IntentAnswer, SharedStateError> {
            self.local.claim_intent(intent_id, run, replacing)
        }
        fn release_intent(&self, intent_id: &str, run: &RunId) -> Result<(), SharedStateError> {
            self.local.release_intent(intent_id, run)
        }
        fn answer_decision(
            &self,
            run: &RunId,
        ) -> Result<relais::shared_state::DecisionGrant, SharedStateError> {
            self.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.held {
                Ok(relais::shared_state::DecisionGrant::HeldElsewhere)
            } else {
                self.local.answer_decision(run)
            }
        }
        fn release_decision(&self, run: &RunId) -> Result<(), SharedStateError> {
            self.releases
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.local.release_decision(run)
        }
        fn registry_pointer(&self) -> Result<relais::shared_state::Pointer, SharedStateError> {
            self.local.registry_pointer()
        }
        fn swap_registry_pointer(
            &self,
            expected: Option<&str>,
            to: Option<&str>,
        ) -> Result<relais::shared_state::Swap, SharedStateError> {
            self.local.swap_registry_pointer(expected, to)
        }
        fn announce_schema(&self, node: &str, version: u64) -> Result<(), SharedStateError> {
            self.local.announce_schema(node, version)
        }
        fn publish(
            &self,
            rows: &[relais::shared_state::PublishRow],
        ) -> Result<relais::shared_state::PublishReceipt, SharedStateError> {
            self.local.publish(rows)
        }
        fn queued(&self) -> Result<u64, SharedStateError> {
            self.local.queued()
        }
        fn mode(&self) -> relais::shared_state::Mode {
            self.local.mode()
        }
    }

    /// `answer` for `run_id` through `shared`.
    fn answer_through(
        dir: &Path,
        run_id: &str,
        answer: DecideAnswer,
        shared: &Arc<Elsewhere>,
    ) -> Decision {
        try_answer_through(dir, run_id, answer, None, shared).expect("decide runs")
    }

    /// [`answer_through`] without unwrapping an operational failure.
    fn try_answer_through(
        dir: &Path,
        run_id: &str,
        answer: DecideAnswer,
        candidate: Option<&str>,
        shared: &Arc<Elsewhere>,
    ) -> Result<Decision, CliError> {
        let shared: Arc<dyn SharedState> = shared.clone();
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
                candidate,
                widen_scope: false,
                rerun_verify: false,
                successor_contract: None,
                json: false,
                proof_stdin: false,
            },
            &locations,
            &|| Ok(shared.clone()),
        )
    }

    fn still_open(ledger: &Ledger, run: &str) -> bool {
        ledger
            .decision_of_run(&RunId::from_stored(run))
            .expect("read")
            .is_some_and(|decision| decision.resolved_at.is_none())
    }

    #[test]
    fn an_answer_held_elsewhere_is_refused_with_exit_3_and_leaves_the_decision_open() {
        let dir = relais::test_support::short_temp_dir("decide-held");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        for name in ["run-approve", "run-reject"] {
            waiting_run(
                &ledger,
                name,
                "/repo",
                State::NeedsDecision,
                Reason::ScopeExceeded,
                None,
                None,
            );
        }
        let held = Arc::new(Elsewhere::holding());
        for (name, answer) in [
            ("run-approve", DecideAnswer::Approve),
            ("run-reject", DecideAnswer::Reject),
        ] {
            let refused = answer_through(&dir, name, answer, &held)
                .err()
                .expect("held elsewhere");
            assert_eq!(exit_code(&refused.outcome), 3, "{name}");
            assert!(
                refused.message.starts_with(&format!(
                    "another answer holds run {name}'s decision (it may already be recorded): \
                     check"
                )),
                "{}",
                refused.message
            );
            assert!(
                refused
                    .message
                    .contains(&format!("check `relais status {name}`")),
                "{}",
                refused.message
            );
            assert!(
                refused
                    .message
                    .contains("after the hold (up to 10 min), answer again if it is still open"),
                "the wait comes before the next step: {}",
                refused.message
            );
            assert!(still_open(&ledger, name), "{name} stays open");
        }
        assert_eq!(held.asks(), 2);
        // The same answers through a state that grants are written.
        let granting = Arc::new(Elsewhere::granting());
        let approved = answer_through(&dir, "run-approve", DecideAnswer::Approve, &granting);
        assert_eq!(exit_of(&approved), 0);
        assert!(!still_open(&ledger, "run-approve"));
    }

    #[test]
    fn a_salvage_held_elsewhere_is_refused_with_exit_3_and_writes_nothing() {
        let dir = relais::test_support::short_temp_dir("decide-salvage-held");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-wait",
            "/repo",
            State::NeedsDecision,
            Reason::ScopeExceeded,
            None,
            None,
        );
        let held = Arc::new(Elsewhere::holding());
        let refused = try_answer_through(
            &dir,
            "run-wait",
            DecideAnswer::Salvaged,
            Some("cand"),
            &held,
        )
        .expect("decide runs")
        .err()
        .expect("held elsewhere");
        assert_eq!(exit_code(&refused.outcome), 3);
        assert_eq!(
            refused.message,
            "another answer holds run run-wait's decision (it may already be recorded): check \
             `relais status run-wait`; after the hold (up to 10 min), run `relais decide \
             run-wait --answer salvaged` again (status cannot show another node's salvage)"
        );
        assert!(still_open(&ledger, "run-wait"), "the decision stays open");
        assert_eq!((held.asks(), held.releases()), (1, 0));
    }

    #[test]
    fn a_granted_salvage_closes_the_open_decision_and_keeps_the_grant() {
        let dir = relais::test_support::short_temp_dir("decide-salvage-granted");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-wait",
            "/repo",
            State::NeedsDecision,
            Reason::ScopeExceeded,
            None,
            None,
        );
        let granting = Arc::new(Elsewhere::granting());
        let salvaged = try_answer_through(
            &dir,
            "run-wait",
            DecideAnswer::Salvaged,
            Some("cand"),
            &granting,
        )
        .expect("decide runs");
        assert!(salvaged.is_ok(), "the salvage is recorded");
        assert!(!still_open(&ledger, "run-wait"));
        assert_eq!((granting.asks(), granting.releases()), (1, 0));
        // A second salvage meets an already accepted run: refused before any grant.
        let again = Arc::new(Elsewhere::granting());
        let refused = try_answer_through(
            &dir,
            "run-wait",
            DecideAnswer::Salvaged,
            Some("cand"),
            &again,
        )
        .expect("decide runs")
        .err()
        .expect("refused");
        assert!(matches!(refused.outcome, CliOutcome::InvalidInput));
        assert_eq!(again.asks(), 0);
    }

    #[test]
    fn a_salvage_of_an_already_answered_decision_gives_its_grant_back() {
        let dir = relais::test_support::short_temp_dir("decide-salvage-answered");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-wait",
            "/repo",
            State::NeedsDecision,
            Reason::ScopeExceeded,
            None,
            None,
        );
        let granting = Arc::new(Elsewhere::granting());
        let rejected = answer_through(&dir, "run-wait", DecideAnswer::Reject, &granting);
        assert_eq!(exit_of(&rejected), 0);
        // The rejected run is terminal and not accepted, so a salvage reaches
        // the grant; the ledger finds the decision already on record.
        let salvage = Arc::new(Elsewhere::granting());
        let refused = try_answer_through(
            &dir,
            "run-wait",
            DecideAnswer::Salvaged,
            Some("cand"),
            &salvage,
        )
        .expect("decide runs")
        .err()
        .expect("refused");
        assert!(matches!(refused.outcome, CliOutcome::InvalidInput));
        assert_eq!((salvage.asks(), salvage.releases()), (1, 1));
    }

    #[test]
    fn a_salvage_of_an_ended_run_held_elsewhere_is_refused_and_writes_nothing() {
        let dir = relais::test_support::short_temp_dir("decide-salvage-ended-held");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-ended",
            "/repo",
            State::Failed,
            Reason::WorkerLooped,
            None,
            None,
        );
        let held = Arc::new(Elsewhere::holding());
        let refused = try_answer_through(
            &dir,
            "run-ended",
            DecideAnswer::Salvaged,
            Some("cand"),
            &held,
        )
        .expect("decide runs")
        .err()
        .expect("held elsewhere");
        assert_eq!(exit_code(&refused.outcome), 3);
        assert!(
            ledger
                .decision_of_run(&RunId::from_stored("run-ended"))
                .expect("read")
                .is_none_or(|decision| decision.resolved_at.is_none()),
            "no decision was resolved"
        );
        assert_eq!((held.asks(), held.releases()), (1, 0));
    }

    #[test]
    fn a_salvage_refused_for_its_arguments_asks_no_grant_and_builds_no_state() {
        let dir = relais::test_support::short_temp_dir("decide-salvage-args");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-wait",
            "/repo",
            State::NeedsDecision,
            Reason::ScopeExceeded,
            None,
            None,
        );
        let locations = || {
            Ok(StateLocations {
                ledger: dir.join("ledger.sqlite"),
                runs: dir.join("runs"),
            })
        };
        let built = std::cell::Cell::new(0);
        let shared = || {
            built.set(built.get() + 1);
            local_shared()
        };
        let refused = decide_in(
            &DecideRequest {
                run_id: "run-wait",
                answer: DecideAnswer::Salvaged,
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
            &shared,
        )
        .expect("decide runs")
        .err()
        .expect("a missing candidate is refused");
        assert!(matches!(refused.outcome, CliOutcome::InvalidInput));
        assert_eq!(built.get(), 0, "no shared state was built");
        assert!(still_open(&ledger, "run-wait"));
    }

    #[test]
    fn a_salvage_under_a_failing_shared_state_is_refused_before_any_write() {
        let dir = relais::test_support::short_temp_dir("decide-salvage-external");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-wait",
            "/repo",
            State::NeedsDecision,
            Reason::ScopeExceeded,
            None,
            None,
        );
        let locations = || {
            Ok(StateLocations {
                ledger: dir.join("ledger.sqlite"),
                runs: dir.join("runs"),
            })
        };
        let failing = || -> Result<Arc<dyn SharedState>, CliError> {
            Err(CliError::Operational {
                operation: "shared state",
                cause: "no shared-state client".into(),
            })
        };
        let decided = decide_in(
            &DecideRequest {
                run_id: "run-wait",
                answer: DecideAnswer::Salvaged,
                actor: "orchestrator",
                person_quote: None,
                note: None,
                successor: None,
                criterion: None,
                candidate: Some("cand"),
                widen_scope: false,
                rerun_verify: false,
                successor_contract: None,
                json: false,
                proof_stdin: false,
            },
            &locations,
            &failing,
        );
        assert!(decided.is_err(), "the builder's error comes through");
        assert!(still_open(&ledger, "run-wait"));
    }

    #[test]
    fn a_stopped_review_approval_held_elsewhere_writes_no_artifact_and_a_repeat_asks_nothing() {
        let dir = relais::test_support::short_temp_dir("decide-held-review");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-ok",
            "/repo",
            State::NeedsDecision,
            Reason::ReviewOverBudget,
            None,
            Some("cand"),
        );
        let held = Arc::new(Elsewhere::holding());
        let refused = answer_through(&dir, "run-ok", DecideAnswer::Approve, &held)
            .err()
            .expect("held elsewhere");
        assert_eq!(exit_code(&refused.outcome), 3);
        assert!(still_open(&ledger, "run-ok"));
        assert!(
            !dir.join("runs").join("run-ok").join("receipts").exists(),
            "the grant is asked before any artifact is written"
        );
        // Granted: sealed. A repeat on the answered decision is the local
        // idempotent exit 0 and asks no grant, even where one is held.
        let granting = Arc::new(Elsewhere::granting());
        let sealed = answer_through(&dir, "run-ok", DecideAnswer::Approve, &granting);
        assert_eq!(exit_of(&sealed), 0);
        let repeat = Arc::new(Elsewhere::holding());
        let again = answer_through(&dir, "run-ok", DecideAnswer::Approve, &repeat);
        assert_eq!(exit_of(&again), 0);
        assert_eq!(repeat.asks(), 0, "a repeat asks no grant");
    }

    #[test]
    fn a_successor_revise_held_elsewhere_is_refused_with_exit_3_and_writes_no_intent() {
        let dir = relais::test_support::short_temp_dir("decide-held-revise");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-revise",
            "/repo",
            State::NeedsDecision,
            Reason::ScopeExceeded,
            None,
            None,
        );
        let contract = dir.join("successor.json");
        std::fs::write(
            &contract,
            serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": "o",
                "base_ref": "HEAD",
                "acceptance": ["bare"],
                "verification_profile": "profile",
                "write_scope": ["src/**"],
            })
            .to_string(),
        )
        .expect("contract");
        let revise = |shared: &Arc<Elsewhere>| {
            let shared: Arc<dyn SharedState> = shared.clone();
            decide_in(
                &DecideRequest {
                    run_id: "run-revise",
                    answer: DecideAnswer::Revise,
                    actor: "orchestrator",
                    person_quote: None,
                    note: None,
                    successor: None,
                    criterion: None,
                    candidate: None,
                    widen_scope: false,
                    rerun_verify: false,
                    successor_contract: Some(&contract),
                    json: false,
                    proof_stdin: false,
                },
                &|| {
                    Ok(StateLocations {
                        ledger: dir.join("ledger.sqlite"),
                        runs: dir.join("runs"),
                    })
                },
                &|| Ok(shared.clone()),
            )
            .expect("decide runs")
        };
        let held = Arc::new(Elsewhere::holding());
        let refused = revise(&held).err().expect("held elsewhere");
        assert_eq!(exit_code(&refused.outcome), 3);
        assert!(
            refused
                .message
                .starts_with("another answer holds run run-revise's decision ("),
            "{}",
            refused.message
        );
        assert!(still_open(&ledger, "run-revise"));
        let run = RunId::from_stored("run-revise");
        assert!(
            ledger.intent_of_decision(&run).expect("read").is_none(),
            "no successor intent is written"
        );
        assert_eq!(held.asks(), 1);
        // Granted, the same answer writes its intent.
        let granting = Arc::new(Elsewhere::granting());
        assert_eq!(exit_of(&revise(&granting)), 0);
        assert!(ledger.intent_of_decision(&run).expect("read").is_some());
    }

    #[test]
    fn a_failure_after_the_ledger_commit_keeps_the_grant() {
        let dir = relais::test_support::short_temp_dir("decide-held-after-commit");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        waiting_run(
            &ledger,
            "run-ok",
            "/repo",
            State::NeedsDecision,
            Reason::ReviewOverBudget,
            None,
            Some("cand"),
        );
        // A directory where `receipt.json` goes: the write after the commit fails.
        std::fs::create_dir_all(dir.join("runs").join("run-ok").join("receipt.json"))
            .expect("blocker");
        let granting = Arc::new(Elsewhere::granting());
        let failed = try_answer_through(&dir, "run-ok", DecideAnswer::Approve, None, &granting);
        assert!(failed.is_err(), "the receipt.json write fails");
        assert!(!still_open(&ledger, "run-ok"), "the ledger committed");
        assert_eq!(granting.asks(), 1);
        assert_eq!(granting.releases(), 0, "a closed decision keeps its grant");
    }

    #[test]
    fn only_the_held_elsewhere_refusal_is_exit_3_and_names_a_recorded_sign_off() {
        let run = RunId::from_stored("run-x");
        // The wire text, written out on purpose: a change to it is a change
        // to what the person reads, and should fail here.
        let held = || {
            NodeError::Shared(SharedStateError::Refused {
                reason: format!("the decision of run {run} is being answered elsewhere"),
            })
        };
        let plain = answer_error(held(), &run, None, NextStep::AnswerAgain)
            .expect("a refusal, not an error")
            .err()
            .expect("refused");
        assert_eq!(exit_code(&plain.outcome), 3);
        assert!(!plain.message.contains("sign-off"));
        let signed = answer_error(held(), &run, Some("c1"), NextStep::AnswerAgain)
            .expect("a refusal, not an error")
            .err()
            .expect("refused");
        assert!(
            signed.message.ends_with(
                "answer again if it is still open; the sign-off of criterion c1 is recorded"
            ),
            "{}",
            signed.message
        );
        // Any other shared refusal stays an operational failure.
        let other = NodeError::Shared(SharedStateError::Refused {
            reason: "not authorised".into(),
        });
        assert!(answer_error(other, &run, None, NextStep::AnswerAgain).is_err());
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
        let old = old.expect("resolved");
        assert_eq!(old.receipt_skipped.as_deref(), Some("review_unavailable"));
    }

    #[test]
    fn approving_a_review_findings_stop_seals_its_findings_or_says_none_was_written() {
        let dir = relais::test_support::short_temp_dir("decide-findings");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let findings = "FINDINGS:\n1. [high] src/lib.rs:1 wrong result. Fix.";
        waiting_run(
            &ledger,
            "run-found",
            "/repo",
            State::NeedsReview,
            Reason::ReviewFindings,
            None,
            None,
        );
        // The snapshot a review-findings stop saves: its review is the
        // findings, around the review that found them.
        let mut saved = receipt("run-found");
        saved.review = ReviewRecord::Findings {
            detail: findings.into(),
            review: Box::new(ReviewRecord::SameModel),
        };
        ledger
            .save_verification_snapshot(
                &RunId::from_stored("run-found"),
                &VerificationSnapshot {
                    candidate_sha: "cand".into(),
                    base_sha: "base".into(),
                    contract_hash: "ch".into(),
                    policy_hash: "ph".into(),
                    stop_reason: Reason::ReviewFindings.as_str().into(),
                    receipt: serde_json::to_value(&saved).expect("receipt"),
                },
            )
            .expect("snapshot");
        waiting_run(
            &ledger,
            "run-bare",
            "/repo",
            State::NeedsReview,
            Reason::ReviewFindings,
            None,
            None,
        );
        waiting_run(
            &ledger,
            "run-over",
            "/repo",
            State::NeedsDecision,
            Reason::ReviewOverBudget,
            None,
            None,
        );

        // With a snapshot: sealed, accepted, the findings kept.
        let sealed = approve(&dir, "run-found").expect("approved");
        assert!(sealed.sentence.contains("sealed at"), "{}", sealed.sentence);
        assert_eq!(sealed.receipt_skipped, None);
        assert!(!decided_json(&sealed)
            .as_object()
            .expect("object")
            .contains_key("receipt_skipped"));
        let (row, _) = ledger
            .receipt(&RunId::from_stored("run-found"))
            .expect("read")
            .expect("a receipt row");
        let kept: Receipt = serde_json::from_value(row).expect("parses");
        assert_eq!(kept.outcome, State::Accepted.as_str());
        assert!(
            matches!(
                &kept.review,
                ReviewRecord::Findings { detail, review }
                    if detail == findings && **review == ReviewRecord::SameModel
            ),
            "{:?}",
            kept.review
        );

        // With none: it resolves, writes no receipt, and says so.
        let bare = approve(&dir, "run-bare");
        assert_eq!(exit_of(&bare), 0);
        let bare = bare.expect("resolved");
        assert_eq!(
            ledger
                .receipt(&RunId::from_stored("run-bare"))
                .expect("read"),
            None
        );
        assert_eq!(
            bare.receipt_skipped_line().as_deref(),
            Some("relais decide: no receipt written: no verification snapshot for this review_findings stop")
        );
        let printed = decided_json(&bare);
        assert_eq!(printed["receipt"], serde_json::Value::Null);
        assert_eq!(printed["receipt_skipped"], "review_findings");
        assert!(printed.as_object().expect("object").contains_key("receipt"));

        // An over-budget stop with no snapshot stays refused.
        let over = approve(&dir, "run-over");
        assert_eq!(exit_of(&over), 2);
        assert_eq!(
            ledger
                .receipt(&RunId::from_stored("run-over"))
                .expect("read"),
            None
        );
    }

    #[test]
    fn approving_a_holdout_failed_stop_is_refused_naming_the_holdout_gap() {
        let dir = relais::test_support::short_temp_dir("decide-holdout");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        // The runner's real gap format (verify::MissingEvidence::HoldoutFailed),
        // pinned by the runner's holdout_failed test.
        let failure = "acceptance criterion `c1`: holdout `escaping` failed: h::escapes";
        // The shape `HoldoutStop::recorded` writes: failures, the same as
        // gaps, and the holdouts' records. No verification snapshot. The runner
        // test `running_out_of_attempts_on_a_holdout_alone_ends_holdout_failed_not_budget_exhausted`
        // checks that the runner really records `gaps`; this hand-copied
        // detail must stay in step with it.
        let detail = serde_json::json!({
            "failures": [failure],
            "gaps": [failure],
            "holdouts": [],
        });
        waiting_run(
            &ledger,
            "run-holdout",
            "/repo",
            State::NeedsDecision,
            Reason::HoldoutFailed,
            Some(detail),
            None,
        );
        let refused = approve(&dir, "run-holdout");
        assert_eq!(exit_of(&refused), 2);
        let message = refused.err().expect("refused").message;
        assert!(message.contains("verification gaps"), "{message}");
        assert!(message.contains("holdout `escaping` failed"), "{message}");
        assert_eq!(
            ledger
                .run_status(&RunId::from_stored("run-holdout"))
                .expect("status"),
            Some(State::NeedsDecision)
        );
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
                orphan: None,
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
            &local_shared,
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

    /// The marker of `run` in `worktree` says its worker was lost on 2026-01-01,
    /// well past the escalation point, and nobody has confirmed it stopped.
    fn lose_worker(worktree: &Path, run: &str) {
        use relais::workspace::inplace::{self, OrphanedWorker};
        let git_dir = inplace::git_dir(worktree).expect("git dir");
        let mut marker = inplace::read_marker(&git_dir, run).expect("marker");
        marker.orphan = OrphanedWorker::lost_at(
            "dispatch-7",
            "sess",
            Some("agent-7"),
            "2026-01-01T10:00:00+00:00",
        );
        std::fs::write(
            inplace::marker_file(&git_dir, run),
            serde_json::to_string_pretty(&marker).expect("json"),
        )
        .expect("marker");
    }

    fn try_answer(dir: &Path, run_id: &str, answer: DecideAnswer) -> Decision {
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
                candidate: None,
                widen_scope: false,
                rerun_verify: false,
                successor_contract: None,
                json: false,
                proof_stdin: false,
            },
            &locations,
            &local_shared,
        )
        .expect("decide runs")
    }

    #[test]
    fn abandoning_a_run_whose_lost_worker_may_still_write_is_refused() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-orphan-abandon");
        let (ledger, worktree) = interrupted_in_place(&dir, "run-lost");
        lose_worker(&worktree, "run-lost");
        move_head(&worktree);
        let Err(refused) = try_answer(&dir, "run-lost", DecideAnswer::Abandon) else {
            panic!("the abandon is refused");
        };
        assert!(matches!(refused.outcome, CliOutcome::ResumeRefusedLive));
        assert!(
            refused
                .message
                .starts_with(relais::workspace::inplace::WORKER_MAY_RUN),
            "{}",
            refused.message
        );
        assert!(
            refused.message.contains("dispatch-7"),
            "{}",
            refused.message
        );
        assert!(
            refused.message.contains("--worker-stopped"),
            "{}",
            refused.message
        );
        assert!(
            refused.message.contains("escalation point has passed"),
            "a stale fence says a stop or --worker-stopped is still required: {}",
            refused.message
        );
        assert!(marker_exists(&worktree, "run-lost"));
        assert_eq!(
            ledger
                .run_status(&RunId::from_stored("run-lost"))
                .expect("status"),
            Some(State::Interrupted),
            "nothing was recorded"
        );
    }

    #[test]
    fn salvaging_a_run_whose_lost_worker_may_still_write_keeps_the_marker_and_says_why() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-orphan-salvage");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-lost");
        lose_worker(&worktree, "run-lost");
        move_head(&worktree);
        let decided = answer_with(&dir, "run-lost", DecideAnswer::Salvaged);
        assert!(
            decided.sentence.contains("inplace_worker_may_run"),
            "{}",
            decided.sentence
        );
        assert!(marker_exists(&worktree, "run-lost"));
    }

    #[test]
    fn the_persons_word_frees_a_lost_worker_and_the_abandon_goes_through() {
        use relais::workspace::inplace::{self, StoppedBy};
        let dir = relais::test_support::short_temp_dir("decide-inplace-orphan-word");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-lost");
        lose_worker(&worktree, "run-lost");
        move_head(&worktree);
        let git_dir = inplace::git_dir(&worktree).expect("git dir");
        assert!(inplace::confirm_stopped(
            &inplace::marker_file(&git_dir, "run-lost"),
            "dispatch-7",
            StoppedBy::Person,
            "2026-11-07T11:00:00+00:00",
        )
        .expect("confirm"));
        answer_with(&dir, "run-lost", DecideAnswer::Abandon);
        assert!(!marker_exists(&worktree, "run-lost"));
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
    fn abandoning_an_unmoved_clean_run_retires_the_marker() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-clean");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-clean");
        answer_with(&dir, "run-clean", DecideAnswer::Abandon);
        assert!(!marker_exists(&worktree, "run-clean"));
    }

    #[test]
    fn abandoning_an_unmoved_run_with_a_tracked_edit_keeps_the_marker() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-edit");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-edit");
        std::fs::write(worktree.join("a.txt"), "edited\n").expect("edit");
        let decided = answer_with(&dir, "run-edit", DecideAnswer::Abandon);
        assert!(marker_exists(&worktree, "run-edit"));
        assert!(
            decided.sentence.contains("relais resume --retire run-edit"),
            "{}",
            decided.sentence
        );
    }

    #[test]
    fn abandoning_an_unmoved_run_with_an_untracked_file_keeps_the_marker() {
        let dir = relais::test_support::short_temp_dir("decide-inplace-untracked");
        let (_ledger, worktree) = interrupted_in_place(&dir, "run-new");
        std::fs::write(worktree.join("new.txt"), "x\n").expect("new");
        let decided = answer_with(&dir, "run-new", DecideAnswer::Abandon);
        assert!(marker_exists(&worktree, "run-new"));
        assert!(
            decided.sentence.contains("relais resume --retire run-new"),
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
