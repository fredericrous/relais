//! `relais decide --answer salvaged`.

use super::ctx::refusal;
use super::decision_record::Decided;
use super::decision_record::Decision;
use super::outcome::operational;
use super::outcome::CliError;
use super::outcome::CliOutcome;
use relais::ids::RunId;
use relais::ledger::Ledger;
use relais::runner::Reason;
use relais::runner::State;
use relais::shared_state::node;
use relais::shared_state::SharedState;
use std::sync::Arc;
/// The CLI flags `--answer salvaged` reads, grouped because they answer
/// one decision together (mirrors [`relais::ledger::DecisionAnswer`]).
pub(crate) struct SalvageAnswer<'a> {
    pub(crate) actor: relais::ledger::Actor,
    pub(crate) person_quote: Option<&'a str>,
    pub(crate) recorded_via: relais::ledger::RecordedVia,
    pub(crate) note: Option<&'a str>,
    pub(crate) criterion: Option<&'a str>,
    pub(crate) successor: Option<&'a str>,
    pub(crate) candidate: Option<&'a str>,
}

/// `relais decide --answer salvaged --candidate <sha>`: a run relais
/// itself never accepted, whose candidate a person finished and merged
/// anyway (SPEC's decision spine) — terminal, or still waiting on a
/// person. Distinct from every other answer in `decide_inner`: a run
/// that never awaited a person opened no decision row for
/// `resolve_decision` to close, and even a run that IS waiting is
/// answered here rather than there, because this is about what happened
/// to the candidate, not about resolving the question the wait raised.
/// Either way the run's own reason for stopping is kept, not overwritten,
/// by [`Ledger::record_salvage`].
pub(crate) fn salvage_command(
    shared: &dyn Fn() -> Result<Arc<dyn SharedState>, CliError>,
    ledger: &Ledger,
    run: &RunId,
    state: State,
    answer: &SalvageAnswer<'_>,
) -> Result<Decision, CliError> {
    let Some(candidate_sha) = answer.candidate else {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            "`--answer salvaged` requires `--candidate <sha>`",
        ));
    };
    if answer.criterion.is_some() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            "`--criterion` does not apply to `--answer salvaged`",
        ));
    }
    if answer.successor.is_some() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            "`--successor` does not apply to `--answer salvaged`",
        ));
    }
    // Salvage answers a run that has stopped running, without relais
    // itself producing an accepted candidate: never one still in flight,
    // never one already accepted the ordinary way or already salvaged
    // once.
    if !state.is_terminal() || state.is_accepted() {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!(
                "run {run} is {state} — `salvaged` answers a run that has already \
                 stopped without an accepted candidate, not one still in flight or already accepted"
            ),
        ));
    }
    // Whether the verified candidate patch survives intact in the commit a
    // person landed is computed here, once, from the run's own evidence;
    // when the repository or the patch is gone it is recorded as unknown
    // (NULL) and said so, never guessed.
    let (contained, unknown) = salvage_containment(ledger, run, candidate_sha)?;
    // The decision grant comes after the read-only containment check (which
    // can be slow) and before the write, so a salvage held elsewhere writes
    // nothing and says nothing about recording.
    let recorded = match node::answer_under_grant(
        &*shared()?,
        run,
        || {
            ledger.record_salvage_contained(
                run,
                candidate_sha,
                relais::ledger::Answerer {
                    actor: answer.actor,
                    person_quote: answer.person_quote,
                    recorded_via: answer.recorded_via,
                },
                answer.note,
                contained,
            )
        },
        salvage_closed,
    ) {
        Ok(recorded) => recorded,
        Err(e) => {
            return super::decision_record::answer_error(
                e,
                run,
                None,
                super::decision_record::SALVAGE_AGAIN,
            )
        }
    };
    let Some(original_reason) = operational(recorded, "decide")? else {
        return Ok(refusal(
            CliOutcome::InvalidInput,
            format!("run {run} already carries a decision on record"),
        ));
    };
    if let Some(reason) = unknown {
        eprintln!(
            "relais decide: could not tell whether run {run}'s verified candidate landed intact \
             in {candidate_sha} ({reason}); recorded it as unknown"
        );
    }
    // The decision is saved: a landing that cannot be recorded is logged and
    // never fails the command; detection finds the landing itself later.
    if contained == Some(true) {
        let recorded = ledger.run_workspace(run).and_then(|workspace| {
            workspace
                .map(|w| {
                    relais::learn::salvage::record_salvage_landing(
                        ledger,
                        run,
                        &w.repo_path,
                        candidate_sha,
                    )
                })
                .transpose()
        });
        if let Err(e) = recorded {
            eprintln!("relais: could not record the landing of {run}: {e}");
        }
    }
    Ok(Ok(Decided {
        run: run.to_string(),
        resolution: Reason::DecisionSalvaged,
        actor: answer.actor,
        from_state: state,
        to_state: State::AcceptedByPerson,
        advice: operational(ledger.decision_advice(run), "decide")?,
        successor_intent: None,
        receipt_skipped: None,
        sentence: format!(
            "recorded {} for {run} by {} (originally ended: {}; candidate contained: {})",
            Reason::DecisionSalvaged.as_str(),
            answer.actor,
            original_reason.as_str(),
            match contained {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            }
        ),
    }))
}

/// Whether the write of a salvage closed the run's decision: the grant stays
/// only then. `Ok(None)` is a decision already on record and `Err` a failed
/// write, so the grant goes back in both.
fn salvage_closed<E>(written: &Result<Option<Reason>, E>) -> bool {
    matches!(written, Ok(Some(_)))
}

/// Whether `run`'s latest verified candidate patch reverse-applies on
/// `candidate_sha`'s tree, and, when the repository or the patch is
/// unavailable, the reason: the caller says it on stderr once the answer is
/// recorded, never before (a salvage refused by the grant records nothing).
pub(crate) fn salvage_containment(
    ledger: &Ledger,
    run: &RunId,
    candidate_sha: &str,
) -> Result<(Option<bool>, Option<String>), CliError> {
    let workspace = operational(ledger.run_workspace(run), "decide")?;
    use relais::learn::salvage::Containment;
    let containment = match workspace {
        Some(workspace) => operational(
            relais::learn::salvage::containment_of_run(
                ledger,
                run,
                &workspace.repo_path,
                candidate_sha,
            ),
            "decide",
        )?,
        None => Containment::Unreachable("the run has no recorded workspace".to_string()),
    };
    let unknown = match &containment {
        Containment::Unreachable(reason) => Some(reason.clone()),
        _ => None,
    };
    Ok((containment.answer(), unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_recorded_salvage_keeps_the_grant() {
        assert!(salvage_closed::<()>(&Ok(Some(Reason::ScopeExceeded))));
        assert!(!salvage_closed::<()>(&Ok(None)));
        assert!(!salvage_closed(&Err("write failed")));
    }
}
