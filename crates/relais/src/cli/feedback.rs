//! `relais feedback`.

use super::decide::*;
use super::*;

/// The run a `relais feedback` invocation is about: the one named
/// directly, or the task's accepted run when `--task` is given instead.
/// Exactly one of `run_id`/`task` is `Some` — clap's `conflicts_with` and
/// `required_unless_present` on the `Feedback` variant guarantee it.
pub(crate) fn feedback_run(
    ledger: &Ledger,
    run_id: Option<&str>,
    task: Option<&str>,
) -> Result<std::result::Result<RunId, Refusal>, CliError> {
    if let Some(run_id) = run_id {
        return Ok(Ok(RunId::from_stored(run_id)));
    }
    let task_id = TaskId::from_stored(task.expect("clap requires run_id or --task"));
    let runs = operational(ledger.runs_of_task(&task_id), "feedback")?;
    for candidate in runs.into_iter().rev() {
        if operational(ledger.run_status(&candidate), "feedback")?.is_some_and(State::is_accepted) {
            return Ok(Ok(candidate));
        }
    }
    Ok(refusal(
        CliOutcome::UnknownRun,
        format!("task {task_id} has no accepted run on record"),
    ))
}

/// One `relais feedback` invocation's arguments, bundled so the handler
/// takes one value rather than seven positional ones.
pub(crate) struct FeedbackRequest {
    pub(crate) run_id: Option<String>,
    pub(crate) task: Option<String>,
    pub(crate) outcome: FeedbackOutcome,
    pub(crate) candidate: Option<String>,
    pub(crate) magnitude: Option<f64>,
    pub(crate) evidence: Vec<String>,
    pub(crate) actor: String,
    pub(crate) note: Option<String>,
    pub(crate) json: bool,
    pub(crate) proof_stdin: bool,
}

/// What a recorded outcome reports, for both the sentence and `--json`.
pub(crate) struct Recorded {
    pub(crate) run: RunId,
    pub(crate) task: TaskId,
    pub(crate) outcome: &'static str,
    pub(crate) actor: relais::ledger::Actor,
    pub(crate) source: &'static str,
}

pub(crate) fn feedback_command(request: FeedbackRequest) -> Result<CliOutcome, CliError> {
    let json = request.json;
    match feedback_inner(request) {
        Ok(Ok(done)) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "run": done.run.as_str(),
                        "task": done.task.as_str(),
                        "outcome": done.outcome,
                        "actor": done.actor.as_str(),
                        "source": done.source,
                    })
                );
            } else {
                println!("recorded {} for {}", done.outcome, done.run);
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
                eprintln!("relais feedback: {}", refused.message);
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

/// The `source` an outcome is stored with: `orchestrator` only when the
/// plugin's nonce verified, `cli` for every other call.
pub(crate) fn feedback_source(via: relais::ledger::RecordedVia) -> &'static str {
    match via {
        relais::ledger::RecordedVia::Plugin => relais::outcome::SOURCE_ORCHESTRATOR,
        relais::ledger::RecordedVia::Cli => relais::outcome::SOURCE_CLI,
    }
}

pub(crate) fn feedback_inner(
    request: FeedbackRequest,
) -> Result<Result<Recorded, Refusal>, CliError> {
    let FeedbackRequest {
        run_id,
        task,
        outcome,
        candidate,
        magnitude,
        evidence,
        actor,
        note,
        json: _,
        proof_stdin,
    } = request;
    let actor = match relais::ledger::Actor::parse(&actor) {
        Ok(actor) => actor,
        Err(message) => return Ok(refusal(CliOutcome::InvalidInput, message)),
    };
    let proof = if proof_stdin {
        match read_decide_proof() {
            Ok(proof) => Some(proof),
            Err(refused) => return Ok(Err(refused)),
        }
    } else {
        None
    };
    let ledger = open_ledger()?;
    let via = match &proof {
        Some(proof) => operational(
            ledger.verify_decide_nonce(&proof.session, &proof.nonce),
            "feedback",
        )?,
        None => relais::ledger::RecordedVia::Cli,
    };
    let run = match feedback_run(&ledger, run_id.as_deref(), task.as_deref())? {
        Ok(run) => run,
        Err(refused) => return Ok(Err(refused)),
    };
    let state = operational(ledger.run_status(&run), "feedback")?;
    let Some(state) = state else {
        return Ok(refusal(
            CliOutcome::UnknownRun,
            format!("unknown run {run}"),
        ));
    };
    // SPEC §20: feedback is attributed to the candidate; absence of
    // feedback is never a positive label. Only a run that reached an
    // accepted candidate — the runner's own verification, or a person's
    // salvage of a terminal run's work — has one whose later life is
    // worth recording.
    if !state.is_accepted() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!(
                "run {run} is {state}, not accepted — final outcome feedback \
                 records what happened to an ACCEPTED change"
            ),
        ));
    }
    let Some(task_id) = operational(ledger.task_of_run(&run), "feedback")? else {
        return Ok(refusal(
            CliOutcome::OperationalFailure,
            format!("run {run} carries no task identity"),
        ));
    };
    // A salvaged run's candidate is never relais's own: it is whatever a
    // person named with `--candidate` on `relais decide --answer
    // salvaged`, read back from the same transition that recorded it —
    // never the receipt (a salvaged run never wrote one) and never the
    // latest attempt's own candidate (that is the work relais discarded,
    // not what the person actually merged).
    let recorded_candidate = if state == State::AcceptedByPerson {
        operational(ledger.salvaged_candidate(&run), "feedback")?
    } else {
        // A run accepted through a person's approval (`relais decide
        // --answer approve`) on a contract interrupted before
        // verification never wrote a receipt — SPEC's decision spine
        // reaches `accepted` without one. Feedback about it is still
        // worth recording; whenever a receipt DOES exist, it is still
        // the source of truth a `--candidate` is checked against (SPEC
        // §20: feedback is attributed to the candidate).
        match operational(ledger.receipt(&run), "feedback")? {
            Some((receipt, _hash)) => {
                let Some(sha) = receipt["candidate_sha"].as_str() else {
                    return Ok(refusal(
                        CliOutcome::OperationalFailure,
                        format!("run {run}'s receipt names no candidate"),
                    ));
                };
                Some(sha.to_string())
            }
            // No receipt, but the run may still have finished an attempt
            // and named what it built. That is the ledger's own answer,
            // and it stands in for the receipt here exactly as the
            // receipt would.
            None => operational(ledger.latest_attempt_candidate(&run), "feedback")?,
        }
    };
    match (&recorded_candidate, &candidate) {
        (Some(recorded), Some(candidate)) if candidate != recorded => {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "--candidate {candidate} does not match run {run}'s accepted \
                     candidate {recorded}"
                ),
            ));
        }
        // Nothing in the ledger says what this run built, so nothing can
        // check the caller's claim. Storing it anyway would put an
        // unverifiable sha in the outcomes table as the candidate this
        // outcome is about (SPEC §20: the receipt, not the caller, is the
        // source of truth). The outcome itself is still recordable —
        // without a candidate it cannot vouch for.
        (None, Some(candidate)) => {
            return Ok(refusal(
                CliOutcome::InvalidInput,
                format!(
                    "run {run} has no recorded candidate to check --candidate \
                     {candidate} against — record the outcome without it, or name the run that \
                     produced the candidate"
                ),
            ));
        }
        (Some(_), Some(_)) | (Some(_), None) | (None, None) => {}
    }
    let candidate_sha = recorded_candidate;
    // The strategy actually used is read off the ledger, never asked of
    // the caller (SPEC §20).
    let Some((_contract, tier)) = operational(ledger.run_contract_and_tier(&run), "feedback")?
    else {
        return Ok(refusal(
            CliOutcome::OperationalFailure,
            format!("run {run} carries no recorded attempt"),
        ));
    };
    let escalated = operational(ledger.escalation_attempted(&run), "feedback")?;
    let models = operational(ledger.models_used(&run), "feedback")?;
    let magnitude = match magnitude {
        Some(value) => match relais::outcome::CorrectionMagnitude::new(value) {
            Ok(magnitude) => Some(magnitude),
            Err(e) => return Ok(refusal(CliOutcome::InvalidInput, e.to_string())),
        },
        None => None,
    };
    let kind = outcome.into();
    let source = feedback_source(via);
    let detail = relais::outcome::OutcomeDetail {
        candidate_sha,
        strategy: relais::outcome::Strategy {
            tier,
            models,
            escalated,
        },
        correction_magnitude: magnitude,
        evidence,
        actor: actor.as_str().to_string(),
        note,
        source: Some(source.to_string()),
    };
    let recorded = match relais::outcome::Outcome::new(kind, detail) {
        Ok(outcome) => outcome,
        Err(e) => return Ok(refusal(CliOutcome::InvalidInput, e.to_string())),
    };
    operational(ledger.record_outcome(&run, &task_id, &recorded), "feedback")?;
    Ok(Ok(Recorded {
        run,
        task: task_id,
        outcome: kind.as_str(),
        actor,
        source,
    }))
}

/// The verification gaps named by the transition that raised a run's
/// still-open wait, if any: the detail `Observation::VerificationGap`
/// records (SPEC's decision spine — `decide --answer approve` is refused
/// while these are non-empty, because a waiver is a policy or contract
/// change, never a CLI flag).
pub(crate) fn open_verification_gaps(
    ledger: &Ledger,
    run: &RunId,
) -> Result<Vec<String>, CliError> {
    // Read the transition that RAISED the open decision, found by the
    // row's own `raised_at`, not the last transition landing on an
    // awaiting state. A run keeps transitioning after it starts waiting:
    // the runner retires its worktree and records that as a same-state
    // row whose detail carries a reference and a patch and no gaps. The
    // newest awaiting transition is therefore usually that retirement,
    // and reading it would find no gaps and wave through exactly the run
    // this refusal exists for.
    let Some(decision) = operational(ledger.decision_of_run(run), "decide")? else {
        return Ok(Vec::new());
    };
    let transitions = operational(ledger.transitions(run), "decide")?;
    let raised: Vec<String> = transitions
        .iter()
        .find(|transition| {
            transition.at == decision.raised_at && transition.to_state.awaits_a_person()
        })
        .and_then(|transition| transition.detail.as_ref())
        .and_then(|detail| detail.get("gaps"))
        .and_then(|gaps| gaps.as_array())
        .map(|gaps| {
            gaps.iter()
                .filter_map(|gap| gap.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // A human-sign-off gap this run's own transition still names is not
    // open any more once `relais decide --answer approve --criterion
    // <id>` has recorded that id's sign-off — the frozen detail never
    // changes, so what clears the gap is read fresh, here, rather than
    // by rewriting history. The id is taken out of the stored sentence
    // by `verify::sign_off_gap_criterion`, the inverse of the one
    // function that wrote it; matching the id anywhere in the prose
    // would also clear a CHECK gap that happened to name it.
    let signed_off: Vec<String> = operational(ledger.human_signoffs(run), "decide")?
        .into_iter()
        .map(|(criterion_id, _actor)| criterion_id)
        .collect();
    Ok(raised
        .into_iter()
        .filter(|gap| match relais::verify::sign_off_gap_criterion(gap) {
            Some(criterion_id) => !signed_off.iter().any(|signed| signed == criterion_id),
            None => true,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tests::temp_ledger;
    use relais::ledger::Transition;

    /// `decide --answer approve` reads the gaps a `VerificationGap`
    /// transition recorded, not a fresh verification run: the run is
    /// already terminal, and the detail its OWN transition into
    /// `needs_decision` carried is the only record of what the gaps were.
    #[test]
    fn open_verification_gaps_reads_the_transition_that_raised_the_decision() {
        let (ledger, dir) = temp_ledger("gaps");
        let run = RunId::from_stored("run-gaps");
        let task = relais::ids::TaskId::from_stored("task-gaps");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::VerificationGap.as_str().into(),
                detail: Some(serde_json::json!({ "gaps": ["declared-check: status `inert`"] })),
                at: relais::ledger::now_rfc3339(),
            })
            .expect("transition");
        // What every real run does next: the runner retires the worktree
        // and records it as a SAME-STATE transition whose detail carries
        // a reference and a patch and no gaps. Reading the newest
        // awaiting transition would find that row and wave the run
        // through — the bug this test exists to keep out.
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::NeedsDecision),
                to_state: State::NeedsDecision,
                reason: Reason::WorktreeRetired.as_str().into(),
                detail: Some(serde_json::json!({
                    "worktree": "/state/worktrees/run-gaps/task",
                    "reference": "refs/relais/candidates/run-gaps/final",
                })),
                at: relais::ledger::now_rfc3339(),
            })
            .expect("retirement transition");
        let gaps = open_verification_gaps(&ledger, &run).expect("gaps read");
        assert_eq!(
            gaps,
            vec!["declared-check: status `inert`".to_string()],
            "the raising transition's gaps survive a later same-state row"
        );
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A decision raised for a reason that carries no `gaps` detail —
    /// `scope_exceeded`, say — names none: `approve` is only refused when
    /// the run's OWN wait actually carries verification gaps.
    #[test]
    fn open_verification_gaps_is_empty_when_the_raising_transition_names_none() {
        let (ledger, dir) = temp_ledger("nogaps");
        let run = RunId::from_stored("run-nogaps");
        let task = relais::ids::TaskId::from_stored("task-nogaps");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::ScopeExceeded.as_str().into(),
                detail: Some(serde_json::json!({ "paths": ["docs/x.md"] })),
                at: relais::ledger::now_rfc3339(),
            })
            .expect("transition");
        let gaps = open_verification_gaps(&ledger, &run).expect("gaps read");
        assert!(gaps.is_empty(), "{gaps:?}");
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// #118: `feedback --task` used to look for a run in `State::Accepted`
    /// only, so a task whose sole acceptance was a person's salvage
    /// (`State::AcceptedByPerson`) refused with "no accepted run on
    /// record" — even though `relais feedback <run-id>` on that very run
    /// accepts it. The task here carries two runs: the first reaches
    /// `needs_review` and is answered `revise` (cancelled, no
    /// acceptance), the second ends `failed` and is then salvaged. The
    /// `--task` lookup must land on the salvaged run.
    ///
    /// FALSIFIED: restricting `feedback_run`'s loop back to
    /// `Some(State::Accepted)` (the pre-fix condition) makes this test
    /// fail with `Err(CliOutcome::UnknownRun)`; restored to
    /// `State::is_accepted`.
    #[test]
    fn feedback_task_finds_a_run_the_only_acceptance_was_a_persons_salvage() {
        let (ledger, dir) = temp_ledger("salvage-task");
        let task = relais::ids::TaskId::from_stored("task-salvaged");

        let revised = RunId::from_stored("run-revised");
        ledger
            .insert_run(&revised, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: revised.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsReview,
                reason: Reason::VerificationGap.as_str().into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
        assert!(ledger
            .resolve_decision(
                &revised,
                &relais::ledger::DecisionAnswer {
                    resolution: Reason::DecisionRevised,
                    actor: relais::ledger::Actor::Person,
                    person_quote: None,
                    recorded_via: relais::ledger::RecordedVia::Cli,
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsReview,
                    to_state: State::Cancelled,
                },
            )
            .expect("resolve"));

        let salvaged = RunId::from_stored("run-salvaged");
        ledger
            .insert_run(&salvaged, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: salvaged.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::Failed,
                reason: Reason::BehavioralFailure.as_str().into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
        ledger
            .record_salvage(&salvaged, "deadbeef", relais::ledger::Actor::Person, None)
            .expect("salvage")
            .expect("a terminal run with no prior decision salvages");

        let found = feedback_run(&ledger, None, Some(task.as_str()))
            .expect("feedback_run")
            .expect("a salvaged run counts as accepted");
        assert_eq!(found, salvaged);

        std::fs::remove_dir_all(&dir).ok();
    }
}
