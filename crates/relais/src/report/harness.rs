//! The harness section (decisions ADR-0042, `harness.part-names-its-failure`):
//! per pipeline step and per model, how often a step ran and how often it
//! caught what it exists to catch, read from the ledger only. This is
//! measurement; nothing here lowers or switches off a step (SPEC §28).
//!
//! * review: the patch-review dispatches that ran, and those whose findings
//!   led to a `revise` answer (the share is the review's catch rate). The
//!   decision is per run, so it is credited to the run's last review.
//! * repair: the repair attempts that ran to a verdict, and those that turned
//!   a failed check green (the repair's own verification passed). An attempt
//!   with no verdict (crashed, cancelled) is counted apart as `no_verdict`.
//!
//! `unknown` is the model of a step no usage row named a model for.

use std::collections::BTreeMap;

use serde::Serialize;

use super::repairs::window_repairs;
use crate::ids::RunId;
use crate::ledger::{Ledger, LedgerError};
use crate::lifecycle::{Reason, State};

/// The model label of a step no usage row named a model for.
const UNKNOWN_MODEL: &str = "unknown";

/// One step run by one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepModel {
    pub model: String,
    /// Times the step ran to a verdict.
    pub ran: usize,
    /// Times it caught what it guards against: a review whose findings led
    /// to `revise`, a repair that turned a failed check green.
    pub caught: usize,
    /// Repair attempts that ended with no verdict (crashed, cancelled, cut
    /// off before the checks ran); they are in neither `ran` nor `caught`.
    pub no_verdict: usize,
}

/// A pipeline step the section reports on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Review,
    Repair,
}

/// One pipeline step's rows, ordered by model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessStep {
    pub step: Step,
    pub models: Vec<StepModel>,
}

/// The `harness` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessReport {
    pub steps: Vec<HarnessStep>,
}

fn model_label(model: Option<String>) -> String {
    model.unwrap_or_else(|| UNKNOWN_MODEL.to_string())
}

/// Per model: (ran, caught, no_verdict).
type Tally = BTreeMap<String, (usize, usize, usize)>;

fn step(step: Step, tally: Tally) -> HarnessStep {
    HarnessStep {
        step,
        models: tally
            .into_iter()
            .map(|(model, (ran, caught, no_verdict))| StepModel {
                model,
                ran,
                caught,
                no_verdict,
            })
            .collect(),
    }
}

/// Whether the run's review findings were answered with `revise`.
fn revised_after_review(ledger: &Ledger, run: &RunId) -> Result<bool, LedgerError> {
    Ok(ledger.decision_of_run(run)?.is_some_and(|decision| {
        decision.raised_reason == Reason::ReviewFindings.as_str()
            && decision.resolution == Some(Reason::DecisionRevised)
    }))
}

/// The section for the window's runs and the package runs beneath them.
pub fn harness_report(
    ledger: &Ledger,
    window: &[(RunId, State)],
) -> Result<HarnessReport, LedgerError> {
    let mut reviews: Tally = BTreeMap::new();
    let mut pending: Vec<RunId> = window.iter().map(|(run, _)| run.clone()).collect();
    while let Some(run) = pending.pop() {
        let dispatches = ledger.review_dispatches(&run)?;
        let revised = !dispatches.is_empty() && revised_after_review(ledger, &run)?;
        let last = dispatches.len().saturating_sub(1);
        for (position, dispatch) in dispatches.into_iter().enumerate() {
            let entry = reviews.entry(model_label(dispatch.model)).or_default();
            entry.0 += 1;
            if revised && position == last {
                entry.1 += 1;
            }
        }
        pending.extend(ledger.child_runs(&run)?.into_iter().map(|child| child.run));
    }
    let mut repairs: Tally = BTreeMap::new();
    for (repair, _, _) in window_repairs(ledger, window)? {
        let entry = repairs
            .entry(model_label(repair.model.clone()))
            .or_default();
        if repair.ended_without_verdict() {
            entry.2 += 1;
            continue;
        }
        entry.0 += 1;
        if repair.turned_green() {
            entry.1 += 1;
        }
    }
    Ok(HarnessReport {
        steps: vec![step(Step::Review, reviews), step(Step::Repair, repairs)],
    })
}

impl HarnessReport {
    /// The section's lines: one per step and model.
    pub fn render(&self) -> String {
        let mut out = String::from("harness (per step and model):\n");
        for step in &self.steps {
            let (name, noun, caught) = match step.step {
                Step::Review => ("review", "reviews", "led to revise"),
                Step::Repair => ("repair", "repairs", "turned a failed check green"),
            };
            if step.models.is_empty() {
                out.push_str(&format!("  {name}: none in this window\n"));
            }
            for row in &step.models {
                out.push_str(&format!(
                    "  {name} [{}]: {} of {} {noun} {caught}",
                    row.model, row.caught, row.ran
                ));
                if row.no_verdict > 0 {
                    out.push_str(&format!(", {} with no verdict", row.no_verdict));
                }
                out.push('\n');
            }
        }
        let any_unknown = self
            .steps
            .iter()
            .flat_map(|s| &s.models)
            .any(|row| row.model == UNKNOWN_MODEL);
        if any_unknown {
            out.push_str("  unknown: no usage row named the dispatch's model\n");
        }
        out
    }
}
