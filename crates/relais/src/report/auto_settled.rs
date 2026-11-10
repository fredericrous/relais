//! The settles relais answered itself (decisions ADR-0011,
//! `execution.settle-before-asking`), read from the ledger only.
//!
//! A scope settle is a decision answered by actor `relais`; its kind comes
//! from the successor intent written with the answer (joined on the decision
//! id), never from the decision's note: an empty `scope_delta` is a timed-out
//! verify, a delta of test paths alone is `widen_tests`, any other delta is
//! `widen_small`. A `review_repair` is a transition of the run itself, so its
//! outcome is the outcome of that same run. A `revise` answered by a person
//! or by the orchestrator is not a settle, whatever its delta.

use serde::Serialize;

use crate::ids::RunId;
use crate::ledger::{Ledger, LedgerError};
use crate::lifecycle::{Reason, State};
use crate::verify::test_tree_matcher;
use crate::workspace::is_protected_path;

/// The actor a settle relais answered itself carries.
const RELAIS_ACTOR: &str = "relais";

/// How many settles of one rule there were, and how their successors (for a
/// `review_repair`, the run itself) ended.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SettleOutcomes {
    pub count: usize,
    pub accepted: usize,
    pub failed: usize,
    pub needs_review: usize,
    /// Successors still in flight, never launched, or ended some other way.
    pub other: usize,
}

impl SettleOutcomes {
    fn add(&mut self, ended: Option<State>) {
        self.count += 1;
        match ended {
            Some(State::Accepted | State::AcceptedByPerson) => self.accepted += 1,
            Some(State::Failed | State::BudgetExhausted) => self.failed += 1,
            Some(State::NeedsReview) => self.needs_review += 1,
            _ => self.other += 1,
        }
    }
}

/// The `auto_settled` section.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AutoSettled {
    pub review_repair: SettleOutcomes,
    pub widen_tests: SettleOutcomes,
    pub widen_small: SettleOutcomes,
    pub timed_out_verify: SettleOutcomes,
}

enum Widening {
    TimedOutVerify,
    Tests,
    Small,
}

fn classify(scope_delta: &[String]) -> Result<Widening, LedgerError> {
    if scope_delta.is_empty() {
        return Ok(Widening::TimedOutVerify);
    }
    let tests = test_tree_matcher().map_err(|e| LedgerError::Corrupt {
        what: "the test-tree patterns".into(),
        detail: e.to_string(),
    })?;
    let all_tests = scope_delta
        .iter()
        .all(|path| !is_protected_path(path) && tests.is_match(path.as_str()));
    Ok(if all_tests {
        Widening::Tests
    } else {
        Widening::Small
    })
}

/// The section for the window's runs and the package runs beneath them.
pub fn auto_settled_report(
    ledger: &Ledger,
    window: &[(RunId, State)],
) -> Result<AutoSettled, LedgerError> {
    let mut section = AutoSettled::default();
    let mut pending: Vec<RunId> = window.iter().map(|(run, _)| run.clone()).collect();
    while let Some(run) = pending.pop() {
        let repaired = ledger
            .transitions(&run)?
            .iter()
            .any(|t| t.reason == Reason::ReviewRepair.as_str());
        if repaired {
            section.review_repair.add(ledger.run_status(&run)?);
        }
        let settled_by_relais = ledger.decision_of_run(&run)?.is_some_and(|d| {
            d.actor.as_deref() == Some(RELAIS_ACTOR)
                && d.resolution == Some(Reason::DecisionRevised)
        });
        if settled_by_relais {
            if let Some(intent) = ledger.intent_of_decision(&run)? {
                let ended = match &intent.run_id {
                    Some(successor) => ledger.run_status(successor)?,
                    None => None,
                };
                let tally = match classify(&intent.scope_delta)? {
                    Widening::TimedOutVerify => &mut section.timed_out_verify,
                    Widening::Tests => &mut section.widen_tests,
                    Widening::Small => &mut section.widen_small,
                };
                tally.add(ended);
            }
        }
        pending.extend(ledger.child_runs(&run)?.into_iter().map(|child| child.run));
    }
    Ok(section)
}

impl AutoSettled {
    /// Two lines within 80 columns: the count per rule, then how the
    /// successors ended.
    pub fn render(&self) -> String {
        let rules = [
            &self.review_repair,
            &self.widen_tests,
            &self.widen_small,
            &self.timed_out_verify,
        ];
        let sum =
            |field: fn(&SettleOutcomes) -> usize| rules.iter().map(|r| field(r)).sum::<usize>();
        format!(
            "auto settled: review_repair {}, widen_tests {}, widen_small {}, timed_out_verify {}\n  \
             outcomes: {} accepted, {} failed, {} needs_review, {} other\n",
            self.review_repair.count,
            self.widen_tests.count,
            self.widen_small.count,
            self.timed_out_verify.count,
            sum(|r| r.accepted),
            sum(|r| r.failed),
            sum(|r| r.needs_review),
            sum(|r| r.other),
        )
    }
}
