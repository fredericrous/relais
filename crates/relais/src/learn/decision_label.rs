//! The label of an answered decision (orchestrator-decides plan, 4c).
//!
//! The question a decision model answers is "should this candidate be
//! approved as it is?". The label is the OUTCOME where one is known and the
//! answer only where none is, whoever answered: an outcome is true whatever
//! the actor said. [`decision_label_of`] is that rule as one pure function
//! beside [`super::dataset::labelling_of`], versioned apart from it so the
//! decision rule changes without re-fingerprinting the run dataset.
//! [`label_decisions`] is the thin reader that feeds it from the ledger.

use crate::ids::{RunId, TaskId};
use crate::ledger::{Ledger, LedgerError};
use crate::lifecycle::{Reason, State};
use crate::outcome::{OutcomeKind, SOURCE_ORCHESTRATOR};

use super::dataset::outcome_stands;
use super::revert;
use super::salvage::{salvage_evidence_of, SalvageEvidence};

/// The version of the rule in [`decision_label_of`]. Bump it whenever a
/// decision's label can change for the same facts.
pub const DECISION_LABEL_POLICY_VERSION: u32 = 1;

/// Where a decision's label comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DecisionLabelSource {
    /// A recorded outcome of the task (or the verified candidate landing
    /// intact), whoever reported it.
    Outcome,
    /// A revert relais itself detected in git.
    Detected,
    /// A revert the orchestrator reported through the plugin.
    Orchestrator,
    /// The landed commit rewrote the reviewed candidate: a test-grade no.
    Rewritten,
    /// The answer itself, because no outcome is known.
    Answer,
}

impl DecisionLabelSource {
    /// Every source, for a caller that prints one line per source.
    pub const ALL: [Self; 5] = [
        Self::Outcome,
        Self::Detected,
        Self::Orchestrator,
        Self::Rewritten,
        Self::Answer,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outcome => "outcome",
            Self::Detected => "detected",
            Self::Orchestrator => "orchestrator",
            Self::Rewritten => "rewritten",
            Self::Answer => "answer",
        }
    }

    /// Whether the label rests on what happened rather than on what was
    /// said: the grade calibration and test rows are drawn from.
    pub const fn rests_on_an_outcome(self) -> bool {
        match self {
            Self::Outcome | Self::Detected | Self::Orchestrator | Self::Rewritten => true,
            Self::Answer => false,
        }
    }
}

/// The label of one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionLabel {
    Yes(DecisionLabelSource),
    No(DecisionLabelSource),
    /// Not a label, with the reason it is left out.
    Excluded(&'static str),
}

/// Who answered, as far as the rule cares: the advisor never trains on its
/// own unlabelled guesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answerer {
    Advisor,
    Anyone,
}

impl Answerer {
    /// Read the stored actor text; `advisor` is the fourth actor a later
    /// release records, which no `--actor` accepts.
    pub fn of_stored(actor: Option<&str>) -> Self {
        match actor {
            Some("advisor") => Self::Advisor,
            Some(_) | None => Self::Anyone,
        }
    }
}

/// Who recorded a task's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reporter {
    Git,
    Orchestrator,
    Other,
}

impl Reporter {
    /// Read `OutcomeDetail::source`; a command line without proof (`cli`)
    /// and a row stored before the field existed are `Other`.
    pub fn of_stored(source: Option<&str>) -> Self {
        match source {
            Some(revert::SOURCE) => Self::Git,
            Some(SOURCE_ORCHESTRATOR) => Self::Orchestrator,
            Some(_) | None => Self::Other,
        }
    }

    const fn label_source(self) -> DecisionLabelSource {
        match self {
            Self::Git => DecisionLabelSource::Detected,
            Self::Orchestrator => DecisionLabelSource::Orchestrator,
            Self::Other => DecisionLabelSource::Outcome,
        }
    }
}

/// A task's latest recorded outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskOutcome {
    pub kind: OutcomeKind,
    pub reporter: Reporter,
}

/// Everything the rule reads about one decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionFacts {
    /// The state the run was raised from.
    pub raised_state: State,
    /// How the decision was answered; `None` while it is open.
    pub resolution: Option<Reason>,
    pub answerer: Answerer,
    pub salvage: SalvageEvidence,
    pub outcome: Option<TaskOutcome>,
}

/// What a task's outcome says about the change an answer let through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judgement {
    NoOutcome,
    Stands,
    Withdrawn(DecisionLabelSource),
    /// An outcome that says nothing about quality (`never_shipped`).
    Silent,
}

fn judgement_of(outcome: Option<TaskOutcome>) -> Judgement {
    let Some(outcome) = outcome else {
        return Judgement::NoOutcome;
    };
    match outcome_stands(outcome.kind) {
        Some(true) => Judgement::Stands,
        // A detected or orchestrator-reported revert outranks containment;
        // any other withdrawal is a plain outcome.
        Some(false) => Judgement::Withdrawn(outcome.reporter.label_source()),
        None => Judgement::Silent,
    }
}

const NOT_AN_ANSWER: &str = "the decision is open, or was not answered with a decision answer";

/// The label of one decision, or why it has none. Pure.
///
/// - an approval: its task's outcome decides (stands → yes, withdrawn →
///   no with the reporter's source); with no outcome the answer does (yes),
///   unless the advisor gave it;
/// - a salvage raised for review findings: the verified candidate landing
///   intact is yes, the landed commit rewriting it is no (`rewritten`), a
///   withdrawn outcome beats containment; unknown containment and any
///   other raising reason are excluded;
/// - revise, reject and abandon: no from the answer, unless the advisor
///   gave it;
/// - `decided`/`recorded`, a run raised from any other state, and a
///   `never_shipped` task: excluded.
pub fn decision_label_of(facts: &DecisionFacts) -> DecisionLabel {
    if !matches!(
        facts.raised_state,
        State::NeedsReview | State::NeedsDecision
    ) {
        return DecisionLabel::Excluded("not raised from needs_review or needs_decision");
    }
    let Some(resolution) = facts.resolution else {
        return DecisionLabel::Excluded(NOT_AN_ANSWER);
    };
    match resolution {
        Reason::DecisionApproved => approval_label(facts),
        Reason::DecisionSalvaged => salvage_label(facts),
        Reason::DecisionRevised | Reason::DecisionRejected | Reason::DecisionAbandoned => {
            match facts.answerer {
                Answerer::Advisor => DecisionLabel::Excluded("the advisor's own unlabelled answer"),
                Answerer::Anyone => DecisionLabel::No(DecisionLabelSource::Answer),
            }
        }
        Reason::DecisionRecorded => {
            DecisionLabel::Excluded("decided or recorded: neither a yes nor a no")
        }
        // `Reason` is the whole lifecycle's vocabulary; every other reason
        // is not a decision answer.
        _ => DecisionLabel::Excluded(NOT_AN_ANSWER),
    }
}

fn approval_label(facts: &DecisionFacts) -> DecisionLabel {
    match (judgement_of(facts.outcome), facts.answerer) {
        (Judgement::Stands, _) => DecisionLabel::Yes(DecisionLabelSource::Outcome),
        (Judgement::Withdrawn(source), _) => DecisionLabel::No(source),
        (Judgement::Silent, _) => DecisionLabel::Excluded("never shipped: no quality label"),
        (Judgement::NoOutcome, Answerer::Advisor) => {
            DecisionLabel::Excluded("the advisor's own unlabelled answer")
        }
        (Judgement::NoOutcome, Answerer::Anyone) => DecisionLabel::Yes(DecisionLabelSource::Answer),
    }
}

fn salvage_label(facts: &DecisionFacts) -> DecisionLabel {
    match facts.salvage {
        SalvageEvidence::ReviewedAndRewritten => DecisionLabel::No(DecisionLabelSource::Rewritten),
        SalvageEvidence::ReviewedAndContained => match judgement_of(facts.outcome) {
            Judgement::NoOutcome | Judgement::Stands => {
                DecisionLabel::Yes(DecisionLabelSource::Outcome)
            }
            Judgement::Withdrawn(source) => DecisionLabel::No(source),
            Judgement::Silent => DecisionLabel::Excluded("never shipped: no quality label"),
        },
        SalvageEvidence::ReviewedContainmentUnknown => {
            DecisionLabel::Excluded("whether the verified candidate landed intact is unknown")
        }
        SalvageEvidence::RaisedElsewhere => DecisionLabel::Excluded(
            "salvaged, but not raised from needs_review with review_findings: nothing was verified",
        ),
        SalvageEvidence::NotSalvaged => DecisionLabel::Excluded("not salvaged"),
    }
}

/// One answered decision with its label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelledDecision {
    pub run: RunId,
    pub task: TaskId,
    pub raised_at: String,
    pub label: DecisionLabel,
}

/// Label every answered decision in the ledger, oldest raised first. The
/// effectful reading around [`decision_label_of`]: it asks the ledger for
/// each decision's task outcome and hands the facts to the pure rule.
pub fn label_decisions(ledger: &Ledger) -> Result<Vec<LabelledDecision>, LedgerError> {
    ledger
        .answered_decisions()?
        .into_iter()
        .map(|decision| {
            let outcome = ledger
                .latest_outcome(&decision.task)?
                .map(|stored| TaskOutcome {
                    kind: stored.outcome.kind,
                    reporter: Reporter::of_stored(stored.outcome.detail.source.as_deref()),
                });
            let facts = DecisionFacts {
                raised_state: decision.raised_state,
                resolution: decision.resolution,
                answerer: Answerer::of_stored(decision.actor.as_deref()),
                salvage: salvage_evidence_of(Some(&decision)),
                outcome,
            };
            Ok(LabelledDecision {
                run: decision.run,
                task: decision.task,
                raised_at: decision.raised_at,
                label: decision_label_of(&facts),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use DecisionLabel::{Excluded, No, Yes};
    use DecisionLabelSource as S;

    fn facts(resolution: Reason) -> DecisionFacts {
        DecisionFacts {
            raised_state: State::NeedsReview,
            resolution: Some(resolution),
            answerer: Answerer::Anyone,
            salvage: SalvageEvidence::NotSalvaged,
            outcome: None,
        }
    }

    fn salvaged(evidence: SalvageEvidence) -> DecisionFacts {
        DecisionFacts {
            salvage: evidence,
            ..facts(Reason::DecisionSalvaged)
        }
    }

    fn outcome(kind: OutcomeKind, reporter: Reporter) -> Option<TaskOutcome> {
        Some(TaskOutcome { kind, reporter })
    }

    #[test]
    fn a_reviewed_contained_salvage_is_a_yes_from_the_outcome() {
        let f = salvaged(SalvageEvidence::ReviewedAndContained);
        assert_eq!(decision_label_of(&f), Yes(S::Outcome));
    }

    #[test]
    fn a_reviewed_rewritten_salvage_is_a_no_from_the_rewrite() {
        let f = salvaged(SalvageEvidence::ReviewedAndRewritten);
        assert_eq!(decision_label_of(&f), No(S::Rewritten));
    }

    #[test]
    fn a_rewrite_raised_elsewhere_and_unknown_containment_are_excluded() {
        for evidence in [
            SalvageEvidence::RaisedElsewhere,
            SalvageEvidence::ReviewedContainmentUnknown,
        ] {
            assert!(
                matches!(decision_label_of(&salvaged(evidence)), Excluded(_)),
                "{evidence:?}"
            );
        }
    }

    #[test]
    fn an_approval_whose_task_outcome_stands_is_a_yes_from_the_outcome() {
        for kind in [OutcomeKind::AcceptedUnchanged, OutcomeKind::Corrected] {
            let f = DecisionFacts {
                outcome: outcome(kind, Reporter::Other),
                ..facts(Reason::DecisionApproved)
            };
            assert_eq!(decision_label_of(&f), Yes(S::Outcome), "{kind:?}");
        }
    }

    #[test]
    fn an_approval_later_reverted_is_a_no_with_the_reporters_source() {
        for (reporter, source) in [
            (Reporter::Git, S::Detected),
            (Reporter::Orchestrator, S::Orchestrator),
            (Reporter::Other, S::Outcome),
        ] {
            for kind in [OutcomeKind::Reverted, OutcomeKind::ConfirmedRegression] {
                let f = DecisionFacts {
                    outcome: outcome(kind, reporter),
                    ..facts(Reason::DecisionApproved)
                };
                assert_eq!(decision_label_of(&f), No(source), "{kind:?} {reporter:?}");
            }
        }
    }

    #[test]
    fn a_detected_or_reported_revert_outranks_containment() {
        for (reporter, source) in [
            (Reporter::Git, S::Detected),
            (Reporter::Orchestrator, S::Orchestrator),
        ] {
            let f = DecisionFacts {
                outcome: outcome(OutcomeKind::Reverted, reporter),
                ..salvaged(SalvageEvidence::ReviewedAndContained)
            };
            assert_eq!(decision_label_of(&f), No(source));
        }
    }

    #[test]
    fn an_approval_with_no_outcome_is_a_yes_from_the_answer() {
        assert_eq!(
            decision_label_of(&facts(Reason::DecisionApproved)),
            Yes(S::Answer)
        );
    }

    #[test]
    fn revise_reject_and_abandon_with_no_outcome_are_a_no_from_the_answer() {
        for reason in [
            Reason::DecisionRevised,
            Reason::DecisionRejected,
            Reason::DecisionAbandoned,
        ] {
            assert_eq!(
                decision_label_of(&facts(reason)),
                No(S::Answer),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn decided_never_shipped_and_other_raising_states_are_excluded() {
        assert!(matches!(
            decision_label_of(&facts(Reason::DecisionRecorded)),
            Excluded(_)
        ));
        let never_shipped = DecisionFacts {
            outcome: outcome(OutcomeKind::NeverShipped, Reporter::Other),
            ..facts(Reason::DecisionApproved)
        };
        assert!(matches!(decision_label_of(&never_shipped), Excluded(_)));
        let interrupted = DecisionFacts {
            raised_state: State::Interrupted,
            ..facts(Reason::DecisionApproved)
        };
        assert!(matches!(decision_label_of(&interrupted), Excluded(_)));
    }

    #[test]
    fn an_open_decision_is_excluded() {
        let open = DecisionFacts {
            resolution: None,
            ..facts(Reason::DecisionApproved)
        };
        assert!(matches!(decision_label_of(&open), Excluded(_)));
    }

    #[test]
    fn an_advisor_answer_with_no_outcome_is_excluded_and_an_outcome_labels_it() {
        for reason in [Reason::DecisionApproved, Reason::DecisionRevised] {
            let f = DecisionFacts {
                answerer: Answerer::Advisor,
                ..facts(reason)
            };
            assert!(matches!(decision_label_of(&f), Excluded(_)), "{reason:?}");
        }
        let judged = DecisionFacts {
            answerer: Answerer::Advisor,
            outcome: outcome(OutcomeKind::Reverted, Reporter::Git),
            ..facts(Reason::DecisionApproved)
        };
        assert_eq!(decision_label_of(&judged), No(S::Detected));
    }

    #[test]
    fn the_stored_spellings_read_back() {
        assert_eq!(Answerer::of_stored(Some("advisor")), Answerer::Advisor);
        assert_eq!(Answerer::of_stored(Some("orchestrator")), Answerer::Anyone);
        assert_eq!(Answerer::of_stored(None), Answerer::Anyone);
        assert_eq!(Reporter::of_stored(Some("detected")), Reporter::Git);
        assert_eq!(
            Reporter::of_stored(Some("orchestrator")),
            Reporter::Orchestrator
        );
        assert_eq!(Reporter::of_stored(Some("cli")), Reporter::Other);
        assert_eq!(Reporter::of_stored(None), Reporter::Other);
        assert!(S::Rewritten.rests_on_an_outcome());
        assert!(!S::Answer.rests_on_an_outcome());
    }
}
