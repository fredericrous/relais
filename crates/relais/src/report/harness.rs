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
//! Each model row also carries what its step's own dispatches cost, beside
//! what they caught. Cost is counted per dispatch or attempt, never through
//! `Ledger::run_cost_by_phase`, which folds a child run into its parent and
//! would count the child twice: this walk visits child runs itself.
//!
//! * review: every review dispatch of every run, a dispatch that ended with
//!   no verdict included, credited to that dispatch's own model.
//! * repair: each counted repair's own cost; a `review_repair` attempt is
//!   not a counted repair.
//!
//! `unknown` is the model of a step no usage row named a model for.

use std::collections::BTreeMap;

use serde::Serialize;

use super::repairs::window_repairs;
use crate::ids::RunId;
use crate::ledger::{Ledger, LedgerError, UnitUsage};
use crate::lifecycle::{Reason, State, UsagePhase};
use crate::money::CostCompleteness;

/// The model label of a step no usage row named a model for.
const UNKNOWN_MODEL: &str = "unknown";

/// One step run by one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StepModel {
    pub model: String,
    /// Times the step ran to a verdict: one review per run, one repair per
    /// attempt.
    pub ran: usize,
    /// Times it caught what it guards against: a review whose findings led
    /// to `revise`, a repair that turned a failed check green.
    pub caught: usize,
    /// Reviews and repairs that ended with no verdict (a reviewer
    /// unavailable or not launched, a repair crashed or cancelled, a run
    /// still in flight); they are in neither `ran` nor `caught`.
    pub no_verdict: usize,
    /// The sum of the reported costs of the step's dispatches or attempts,
    /// in micro-dollars. Read with `cost_completeness`: a figure that is not
    /// `actual` is a lower bound, and an all-silent set is never a true zero.
    /// `None` when no usage row of the step reported a cost: unknown, never
    /// `$0.00`.
    pub cost_micros: Option<i64>,
    /// Input, output and cache tokens of the same dispatches or attempts.
    pub tokens: u64,
    /// The worst completeness over the counted dispatches or attempts; one
    /// that reported no usage at all makes it not `actual`.
    pub cost_completeness: CostCompleteness,
    /// `cost_micros` per catch, rounded; `None` when nothing was caught.
    pub cost_per_caught_micros: Option<i64>,
    /// `tokens` per catch, rounded; `None` when nothing was caught.
    pub tokens_per_caught: Option<u64>,
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

/// What one model's rows of a step add up to while the window is walked.
#[derive(Default)]
struct ModelTally {
    ran: usize,
    caught: usize,
    no_verdict: usize,
    cost_micros: Option<i64>,
    tokens: u64,
    completeness: Vec<CostCompleteness>,
}

impl ModelTally {
    fn add_usage(&mut self, usage: UnitUsage) {
        if let Some(cost) = usage.cost {
            self.cost_micros = Some(
                self.cost_micros
                    .unwrap_or(0)
                    .saturating_add(cost.to_micros()),
            );
        }
        self.tokens = self.tokens.saturating_add(usage.tokens);
        self.completeness.push(usage.completeness);
    }
}

type Tally = BTreeMap<String, ModelTally>;

fn per_catch(total: u64, caught: usize) -> Option<u64> {
    (caught > 0).then(|| (total as f64 / caught as f64).round() as u64)
}

fn step(step: Step, tally: Tally) -> HarnessStep {
    HarnessStep {
        step,
        models: tally
            .into_iter()
            .map(|(model, row)| StepModel {
                model,
                ran: row.ran,
                caught: row.caught,
                no_verdict: row.no_verdict,
                cost_micros: row.cost_micros,
                tokens: row.tokens,
                // Nothing counted is nothing known, not a true zero.
                cost_completeness: if row.completeness.is_empty() {
                    CostCompleteness::Unknown
                } else {
                    CostCompleteness::worst(row.completeness)
                },
                cost_per_caught_micros: row.cost_micros.and_then(|micros| {
                    per_catch(micros.max(0) as u64, row.caught).map(|per| per as i64)
                }),
                tokens_per_caught: per_catch(row.tokens, row.caught),
            })
            .collect(),
    }
}

/// How a run's review ended: with a verdict (its findings raised a decision,
/// or the run has a `checks_and_review_passed` transition, whatever it ended as), and whether those findings were
/// answered with `revise`; or with no verdict (the reviewer was unavailable,
/// failed to launch, or the run is still in flight).
enum ReviewEnd {
    Verdict { revised: bool },
    NoVerdict,
}

fn review_end(ledger: &Ledger, run: &RunId) -> Result<ReviewEnd, LedgerError> {
    let decision = ledger.decision_of_run(run)?;
    let transitions = ledger.transitions(run)?;
    // Findings the worker was sent back to repair were caught by review
    // as much as findings a person revised.
    let repaired = transitions
        .iter()
        .any(|t| t.reason == Reason::ReviewRepair.as_str());
    let raised_by_findings = decision
        .as_ref()
        .is_some_and(|d| d.raised_reason == Reason::ReviewFindings.as_str());
    if raised_by_findings {
        let revised = decision.is_some_and(|d| d.resolution == Some(Reason::DecisionRevised));
        return Ok(ReviewEnd::Verdict {
            revised: revised || repaired,
        });
    }
    let passed_clean = transitions
        .iter()
        .any(|t| t.reason == Reason::ChecksAndReviewPassed.as_str());
    Ok(if repaired {
        ReviewEnd::Verdict { revised: true }
    } else if passed_clean {
        ReviewEnd::Verdict { revised: false }
    } else {
        ReviewEnd::NoVerdict
    })
}

/// The section for the window's runs and the package runs beneath them.
pub fn harness_report(
    ledger: &Ledger,
    window: &[(RunId, State)],
) -> Result<HarnessReport, LedgerError> {
    let mut reviews: Tally = BTreeMap::new();
    let mut pending: Vec<RunId> = window.iter().map(|(run, _)| run.clone()).collect();
    while let Some(run) = pending.pop() {
        // One review per run, however many parts it was dispatched in,
        // credited to the model of its last dispatch.
        let dispatches = ledger.review_dispatches(&run)?;
        // Cost is every dispatch's own, whether or not it ended in a verdict.
        for dispatch in &dispatches {
            let usage = ledger.dispatch_usage(&dispatch.dispatch_id, UsagePhase::Review)?;
            reviews
                .entry(model_label(dispatch.model.clone()))
                .or_default()
                .add_usage(usage);
        }
        if let Some(last) = dispatches.last() {
            let entry = reviews.entry(model_label(last.model.clone())).or_default();
            match review_end(ledger, &run)? {
                ReviewEnd::Verdict { revised } => {
                    entry.ran += 1;
                    if revised {
                        entry.caught += 1;
                    }
                }
                ReviewEnd::NoVerdict => entry.no_verdict += 1,
            }
        }
        pending.extend(ledger.child_runs(&run)?.into_iter().map(|child| child.run));
    }
    let mut repairs: Tally = BTreeMap::new();
    for (repair, _, _) in window_repairs(ledger, window)? {
        let entry = repairs
            .entry(model_label(repair.model.clone()))
            .or_default();
        entry.add_usage(repair.usage);
        if repair.ended_without_verdict() {
            entry.no_verdict += 1;
            continue;
        }
        entry.ran += 1;
        if repair.turned_green() {
            entry.caught += 1;
        }
    }
    Ok(HarnessReport {
        steps: vec![step(Step::Review, reviews), step(Step::Repair, repairs)],
    })
}

/// Micro-dollars as `$X.XX`.
fn dollars(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1_000_000.0)
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
                // The figure's own kind, so a priced-from-tokens total is not
                // called partial: every real row is estimated.
                let partial = match row.cost_completeness {
                    CostCompleteness::Actual => "",
                    CostCompleteness::Estimated => " (est.)",
                    CostCompleteness::IncompleteLowerBound => " (lower bound)",
                    CostCompleteness::Unknown => " (partial)",
                };
                let per_catch = row
                    .cost_per_caught_micros
                    .map_or_else(|| "-".to_string(), dollars);
                match row.cost_micros {
                    Some(micros) => out.push_str(&format!(
                        ", {} total{partial}, {per_catch}/catch",
                        dollars(micros)
                    )),
                    None => out.push_str(", cost unknown"),
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
            out.push_str("  unknown: no usage row named the model that ran the step\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::PackageId;
    use crate::ids::{DispatchId, TaskId};
    use crate::ledger::{now_rfc3339, ChildOf, Transition, UsageEvent};
    use crate::money::{CostKind, MicroUsd};
    use crate::route::RoutedBy;

    use serde_json::json;

    fn open(label: &str) -> (Ledger, crate::test_support::TempDir) {
        let dir = crate::test_support::temp_dir(label);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        (ledger, dir)
    }

    fn new_run(ledger: &Ledger, id: &str) -> RunId {
        let run = RunId::from_stored(id);
        ledger
            .insert_run(&run, "/repo", None, &TaskId::from_stored("task-x"), "rk")
            .expect("run");
        run
    }

    fn go(ledger: &Ledger, run: &RunId, to_state: State, reason: &str) {
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: None,
                to_state,
                reason: reason.into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
    }

    fn review_dispatch(ledger: &Ledger, run: &RunId, id: &str) {
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored(id),
                run,
                None,
                &json!({ "kind": "review" }),
                0,
                RoutedBy::ConservativeBaseline,
            )
            .expect("dispatch");
    }

    /// A review that ended clean: nothing caught.
    fn finish_review_clean(ledger: &Ledger, run: &RunId) {
        go(ledger, run, State::Verifying, "verification_started");
        go(ledger, run, State::Accepted, "checks_and_review_passed");
    }

    /// A review whose findings sent the worker back to a review repair
    /// before the run passed (caught by review).
    fn finish_review_caught(ledger: &Ledger, run: &RunId) {
        go(ledger, run, State::Verifying, "verification_started");
        go(ledger, run, State::Repairing, "review_repair");
        go(ledger, run, State::Verifying, "verification_started");
        go(ledger, run, State::Accepted, "checks_and_review_passed");
    }

    struct Row {
        id: &'static str,
        attempt: Option<i64>,
        parent: Option<&'static str>,
        inclusive: bool,
        model: Option<&'static str>,
        phase: UsagePhase,
        cost: Option<i64>,
        tokens: i64,
        completeness: CostCompleteness,
    }

    impl Row {
        fn new(id: &'static str, phase: UsagePhase) -> Self {
            Self {
                id,
                attempt: None,
                parent: None,
                inclusive: false,
                model: None,
                phase,
                cost: None,
                tokens: 0,
                completeness: CostCompleteness::Actual,
            }
        }
    }

    fn record(ledger: &Ledger, run: &RunId, row: Row) {
        ledger
            .record_usage(&UsageEvent {
                event_id: row.id.into(),
                run_id: run.clone(),
                attempt_id: row.attempt,
                parent_event_id: row.parent.map(str::to_string),
                model: row.model.map(str::to_string),
                input_tokens: Some(row.tokens),
                output_tokens: Some(0),
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost: row.cost.map(MicroUsd::from_micros),
                cost_kind: CostKind::ApiSpend,
                completeness: row.completeness,
                inclusive: row.inclusive,
                at: now_rfc3339(),
                phase: Some(row.phase),
                duration_ms: None,
                requested_model: None,
                requested_effort: None,
                harness: None,
            })
            .expect("usage");
    }

    fn models_of(report: &HarnessReport, wanted: Step) -> serde_json::Value {
        let json = serde_json::to_value(report).expect("json");
        json["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .find(|s| s["step"] == serde_json::to_value(wanted).expect("step"))
            .expect("step present")["models"]
            .clone()
    }

    /// A parent run and one child run, each with a review dispatch whose
    /// usage rows are keyed by the dispatch id and its `#` range. The
    /// parent also holds an inclusive row with a covered child row: only the
    /// inclusive total counts.
    #[test]
    fn a_parent_and_its_child_run_are_summed_once_with_the_hash_range_matched() {
        let (ledger, _dir) = open("harness-cost-tree");
        let parent = new_run(&ledger, "run-p");
        review_dispatch(&ledger, &parent, "d-p");
        let rows = [
            ("d-p", 100_000, 10),
            ("d-p#0", 50_000, 5),
            ("d-p#1", 1_000_000, 7),
        ];
        for (id, cost, tokens) in rows {
            let mut row = Row::new(id, UsagePhase::Review);
            row.model = Some("opus");
            row.cost = Some(cost);
            row.tokens = tokens;
            row.inclusive = id == "d-p#1";
            record(&ledger, &parent, row);
        }
        let mut covered = Row::new("d-p#1#sub", UsagePhase::Review);
        covered.model = Some("opus");
        covered.cost = Some(9_999_000);
        covered.tokens = 999;
        covered.parent = Some("d-p#1");
        record(&ledger, &parent, covered);
        finish_review_caught(&ledger, &parent);

        let child = RunId::from_stored("run-c");
        ledger
            .insert_child_run(
                &child,
                "/repo",
                None,
                ChildOf {
                    parent_run: &parent,
                    package_id: &PackageId::from_stored("pkg"),
                },
                &TaskId::from_stored("task-x"),
                "rk",
            )
            .expect("child");
        review_dispatch(&ledger, &child, "d-c");
        let mut row = Row::new("d-c#0", UsagePhase::Review);
        row.model = Some("opus");
        row.cost = Some(400_000);
        row.tokens = 20;
        record(&ledger, &child, row);

        let report = harness_report(&ledger, &[(parent, State::Accepted)]).expect("report");
        assert_eq!(
            models_of(&report, Step::Review),
            json!([{
                "model": "opus", "ran": 1, "caught": 1, "no_verdict": 1,
                "cost_micros": 1_550_000, "tokens": 42, "cost_completeness": "actual",
                "cost_per_caught_micros": 1_550_000, "tokens_per_caught": 42,
            }])
        );
        let text = report.render();
        let line = "review [opus]: 1 of 1 reviews led to revise, 1 with no verdict, $1.55 total, $1.55/catch\n";
        assert!(text.contains(line), "{line} missing from {text}");
    }

    /// A check repair and a `review_repair` attempt in one run: only the
    /// check repair's cost lands in the repair step.
    #[test]
    fn only_the_check_repair_cost_lands_in_the_repair_step() {
        let (ledger, _dir) = open("harness-cost-repair");
        let run = new_run(&ledger, "run-r");
        let mut attempts = Vec::new();
        for (index, phase) in [
            (1, UsagePhase::Initial),
            (2, UsagePhase::Repair),
            (3, UsagePhase::Repair),
        ] {
            attempts.push(
                ledger
                    .insert_attempt(&run, 1, index, "implementation", phase)
                    .expect("attempt"),
            );
        }
        for (id, attempt, cost, tokens, model) in [
            ("e-init", attempts[0], 7_000_000, 70, "opus"),
            ("e-check", attempts[1], 2_000_000, 30, "sonnet"),
            ("e-review", attempts[2], 900_000_000, 900, "sonnet"),
        ] {
            let mut row = Row::new(id, UsagePhase::Repair);
            row.attempt = Some(attempt);
            row.model = Some(model);
            row.cost = Some(cost);
            row.tokens = tokens;
            record(&ledger, &run, row);
        }
        for (state, reason) in [
            (State::Running, "worker_dispatched"),
            (State::Verifying, "verification_started"),
            (State::Repairing, "behavioral_failure"),
            (State::Running, "worker_dispatched"),
            (State::Verifying, "verification_started"),
            (State::Repairing, "review_repair"),
            (State::Running, "worker_dispatched"),
            (State::Verifying, "verification_started"),
            (State::Accepted, "checks_and_review_passed"),
        ] {
            go(&ledger, &run, state, reason);
        }
        let report = harness_report(&ledger, &[(run, State::Accepted)]).expect("report");
        assert_eq!(
            models_of(&report, Step::Repair),
            json!([{
                "model": "sonnet", "ran": 1, "caught": 1, "no_verdict": 0,
                "cost_micros": 2_000_000, "tokens": 30, "cost_completeness": "actual",
                "cost_per_caught_micros": 2_000_000, "tokens_per_caught": 30,
            }])
        );
    }

    /// Mixed completeness inside a dispatch, a silent dispatch among
    /// reported ones, and an all-silent set: none is complete or an actual
    /// zero.
    #[test]
    fn mixed_and_silent_usage_is_partial_or_unknown_never_complete_or_zero() {
        let (ledger, _dir) = open("harness-cost-mixed");
        let haiku = new_run(&ledger, "run-haiku");
        review_dispatch(&ledger, &haiku, "d-haiku");
        for (id, cost, completeness) in [
            ("d-haiku#0", 1_000_000, CostCompleteness::Actual),
            ("d-haiku#1", 500_000, CostCompleteness::IncompleteLowerBound),
        ] {
            let mut row = Row::new(id, UsagePhase::Review);
            row.model = Some("haiku");
            row.cost = Some(cost);
            row.tokens = 5;
            row.completeness = completeness;
            record(&ledger, &haiku, row);
        }
        finish_review_caught(&ledger, &haiku);
        let opus = new_run(&ledger, "run-opus");
        review_dispatch(&ledger, &opus, "d-opus-1");
        review_dispatch(&ledger, &opus, "d-opus-2");
        let mut reported = Row::new("d-opus-1", UsagePhase::Review);
        reported.model = Some("opus");
        reported.cost = Some(2_000_000);
        reported.tokens = 8;
        record(&ledger, &opus, reported);
        let mut silent = Row::new("d-opus-2", UsagePhase::Review);
        silent.model = Some("opus");
        silent.tokens = 3;
        silent.completeness = CostCompleteness::Unknown;
        record(&ledger, &opus, silent);
        finish_review_clean(&ledger, &opus);
        let sonnet = new_run(&ledger, "run-sonnet");
        review_dispatch(&ledger, &sonnet, "d-sonnet");
        let mut row = Row::new("d-sonnet", UsagePhase::Review);
        row.model = Some("sonnet");
        row.tokens = 2;
        row.completeness = CostCompleteness::Unknown;
        record(&ledger, &sonnet, row);
        finish_review_clean(&ledger, &sonnet);
        let none = new_run(&ledger, "run-none");
        review_dispatch(&ledger, &none, "d-none");
        finish_review_clean(&ledger, &none);

        let window = [
            (haiku, State::Accepted),
            (opus, State::Accepted),
            (sonnet, State::Accepted),
            (none, State::Accepted),
        ];
        let report = harness_report(&ledger, &window).expect("report");
        assert_eq!(
            models_of(&report, Step::Review),
            json!([
                { "model": "haiku", "ran": 1, "caught": 1, "no_verdict": 0,
                  "cost_micros": 1_500_000, "tokens": 10,
                  "cost_completeness": "incomplete_lower_bound",
                  "cost_per_caught_micros": 1_500_000, "tokens_per_caught": 10 },
                { "model": "opus", "ran": 1, "caught": 0, "no_verdict": 0,
                  "cost_micros": 2_000_000, "tokens": 11, "cost_completeness": "unknown",
                  "cost_per_caught_micros": null, "tokens_per_caught": null },
                { "model": "sonnet", "ran": 1, "caught": 0, "no_verdict": 0,
                  "cost_micros": null, "tokens": 2, "cost_completeness": "unknown",
                  "cost_per_caught_micros": null, "tokens_per_caught": null },
                { "model": "unknown", "ran": 1, "caught": 0, "no_verdict": 0,
                  "cost_micros": null, "tokens": 0, "cost_completeness": "unknown",
                  "cost_per_caught_micros": null, "tokens_per_caught": null },
            ])
        );
        let text = report.render();
        for line in [
            "review [haiku]: 1 of 1 reviews led to revise, $1.50 total (lower bound), $1.50/catch\n",
            "review [opus]: 0 of 1 reviews led to revise, $2.00 total (partial), -/catch\n",
            "review [sonnet]: 0 of 1 reviews led to revise, cost unknown\n",
            "review [unknown]: 0 of 1 reviews led to revise, cost unknown\n",
        ] {
            assert!(text.contains(line), "{line} missing from {text}");
        }
    }

    /// A run's first review dispatch ran on opus and its last on sonnet:
    /// each dispatch's cost goes to its own model, the verdict to the last.
    /// Every real row is priced from tokens, so it is estimated: the text
    /// says so instead of calling it partial.
    #[test]
    fn an_estimated_cost_is_marked_estimated_not_partial() {
        let (ledger, _dir) = open("harness-cost-estimated");
        let run = new_run(&ledger, "run-est");
        review_dispatch(&ledger, &run, "d-est");
        let mut row = Row::new("d-est", UsagePhase::Review);
        row.model = Some("opus");
        row.cost = Some(1_000_000);
        row.tokens = 4;
        row.completeness = CostCompleteness::Estimated;
        record(&ledger, &run, row);
        finish_review_caught(&ledger, &run);
        let report = harness_report(&ledger, &[(run, State::Accepted)]).expect("report");
        let text = report.render();
        let line = "review [opus]: 1 of 1 reviews led to revise, $1.00 total (est.), $1.00/catch\n";
        assert!(text.contains(line), "{line} missing from {text}");
    }

    #[test]
    fn a_review_dispatch_cost_goes_to_its_own_model_not_the_last_ones() {
        let (ledger, _dir) = open("harness-cost-own-model");
        let run = new_run(&ledger, "run-m");
        review_dispatch(&ledger, &run, "d-1");
        review_dispatch(&ledger, &run, "d-2");
        for (id, model, cost, tokens) in
            [("d-1", "opus", 200_000, 4), ("d-2", "sonnet", 800_000, 6)]
        {
            let mut row = Row::new(id, UsagePhase::Review);
            row.model = Some(model);
            row.cost = Some(cost);
            row.tokens = tokens;
            record(&ledger, &run, row);
        }
        finish_review_clean(&ledger, &run);
        let report = harness_report(&ledger, &[(run, State::Accepted)]).expect("report");
        assert_eq!(
            models_of(&report, Step::Review),
            json!([
                { "model": "opus", "ran": 0, "caught": 0, "no_verdict": 0,
                  "cost_micros": 200_000, "tokens": 4, "cost_completeness": "actual",
                  "cost_per_caught_micros": null, "tokens_per_caught": null },
                { "model": "sonnet", "ran": 1, "caught": 0, "no_verdict": 0,
                  "cost_micros": 800_000, "tokens": 6, "cost_completeness": "actual",
                  "cost_per_caught_micros": null, "tokens_per_caught": null },
            ])
        );
    }

    /// A run that was sent back to its worker on a review's findings and
    /// then passed clean was caught by review.
    #[test]
    fn a_review_repair_run_that_passes_counts_as_caught_by_review() {
        let dir = crate::test_support::temp_dir("harness-review-repair");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = RunId::from_stored("run-h");
        ledger
            .insert_run(&run, "/repo", None, &TaskId::from_stored("task-h"), "rk")
            .expect("run");
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored("d-review-h"),
                &run,
                None,
                &serde_json::json!({ "kind": "review" }),
                0,
                RoutedBy::ConservativeBaseline,
            )
            .expect("dispatch");
        for (state, reason) in [
            (State::Verifying, "verification_started"),
            (State::Repairing, "review_repair"),
            (State::Verifying, "verification_started"),
            (State::Accepted, "checks_and_review_passed"),
        ] {
            ledger
                .record_transition(&Transition {
                    run_id: run.clone(),
                    attempt_id: None,
                    from_state: None,
                    to_state: state,
                    reason: reason.into(),
                    detail: None,
                    at: now_rfc3339(),
                })
                .expect("transition");
        }
        let report = harness_report(&ledger, &[(run, State::Accepted)]).expect("report");
        let review = report
            .steps
            .iter()
            .find(|s| s.step == Step::Review)
            .expect("review step");
        assert_eq!(review.models.len(), 1, "{review:?}");
        assert_eq!((review.models[0].ran, review.models[0].caught), (1, 1));
    }
}
