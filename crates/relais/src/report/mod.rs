//! Cost and outcome reporting (SPEC §11, §20).
//!
//! Requested and effective model, token categories, provider-reported
//! cost, duration, attempt, phase and final outcome are recorded per
//! dispatch by the runner and ledger; this module turns that into
//! readable output. Missing usage is marked unknown, never zero; an
//! inclusive parent total is never added to its children (the ledger's
//! aggregation owns that rule); the primary metric is total recorded
//! cost — failed runs included — divided by accepted tasks in the same
//! cohort. Savings are reported as measured comparisons only; nothing
//! here converts token savings into subscription fee savings.

use serde::Serialize;

use crate::ledger::Ledger;
use crate::lifecycle::State;
use crate::money::{CostCompleteness, MicroUsd};

#[derive(Debug, Clone, Serialize)]
pub struct RunLine {
    pub run_id: String,
    /// The run's lifecycle state, parsed out of the ledger row: a state
    /// this binary does not know is a `LedgerError`, not a string nobody
    /// matches. Counting states by string literal is how `accepted` and
    /// `needs_review` drifted apart from the states the runner writes.
    pub status: State,
    pub attempts: usize,
    pub cost: MicroUsd,
    pub cost_completeness: CostCompleteness,
    pub models: Vec<String>,
    pub final_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub since: String,
    pub runs: Vec<RunLine>,
    pub accepted: usize,
    pub total_cost: MicroUsd,
    pub cost_per_accepted: Option<MicroUsd>,
    pub acceptance_rate: Option<f64>,
    /// Runs whose outcome is owed to a person rather than settled by the
    /// runner: `needs_review` and `needs_decision` (the quality bar held
    /// and a human is owed a look) plus `interrupted` (the run's state is
    /// uncertain and is never retried on its own, SPEC §12). Accepted,
    /// failed, blocked, cancelled and budget-exhausted runs are settled;
    /// a run still in flight is not counted either.
    pub pending_decisions: usize,
    pub cost_completeness: CostCompleteness,
}

pub fn runs_report(ledger: &Ledger, since: &str) -> Result<Report, crate::ledger::LedgerError> {
    let mut runs = Vec::new();
    for (run_id, _repo, status, _created) in ledger.runs_since(since)? {
        let status = State::parse(&status).ok_or_else(|| crate::ledger::LedgerError::Corrupt {
            what: format!("status of run {run_id}"),
            detail: format!("`{status}` is not a lifecycle state this relais knows"),
        })?;
        let transitions = ledger.transitions(&run_id)?;
        let attempts = ledger.attempt_count(&run_id)?;
        let cost = ledger.run_cost(&run_id)?;
        let completeness = ledger.run_cost_completeness(&run_id)?;
        let models = ledger.models_used(&run_id)?;
        let final_detail = transitions
            .last()
            .and_then(|transition| {
                transition
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("detail"))
                    .and_then(|value| value.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                transitions
                    .last()
                    .map(|transition| transition.reason.clone())
            });
        runs.push(RunLine {
            run_id,
            status,
            attempts,
            cost,
            cost_completeness: completeness,
            models,
            final_detail,
        });
    }
    let accepted = runs
        .iter()
        .filter(|run| run.status == State::Accepted)
        .count();
    let total_cost = runs.iter().fold(MicroUsd::ZERO, |acc, run| acc + run.cost);
    let pending_decisions = runs
        .iter()
        .filter(|run| awaits_a_person(run.status))
        .count();
    let cost_completeness = CostCompleteness::worst(runs.iter().map(|run| run.cost_completeness));
    let cost_per_accepted = if accepted > 0 {
        Some(MicroUsd::from_micros(
            (total_cost.to_micros() as f64 / accepted as f64).round() as i64,
        ))
    } else {
        None
    };
    let acceptance_rate = if runs.is_empty() {
        None
    } else {
        Some(accepted as f64 / runs.len() as f64)
    };
    Ok(Report {
        since: since.to_string(),
        runs,
        accepted,
        total_cost,
        cost_per_accepted,
        acceptance_rate,
        pending_decisions,
        cost_completeness,
    })
}

impl Report {
    /// Human-readable output. Every cost line carries its completeness
    /// label: actual, estimated, incomplete lower bound, or unknown
    /// (SPEC §11: reports distinguish them; unknown is never zero).
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("relais report since {}\n", self.since));
        out.push('\n');
        if self.runs.is_empty() {
            out.push_str("no runs recorded in this window\n");
            return out;
        }
        for run in &self.runs {
            out.push_str(&format!(
                "{}  {:<16} attempts {:<3} {}  [{}]\n",
                run.run_id,
                run.status.as_str(),
                run.attempts,
                cost_line(run.cost, run.cost_completeness),
                if run.models.is_empty() {
                    "-".to_string()
                } else {
                    run.models.join(",")
                },
            ));
            if let Some(detail) = &run.final_detail {
                out.push_str(&format!("    {}\n", truncate_chars(detail, 120)));
            }
        }
        out.push('\n');
        out.push_str(&format!(
            "accepted: {} of {} runs ({})\n",
            self.accepted,
            self.runs.len(),
            match self.acceptance_rate {
                Some(rate) => format!("{:.0}%", rate * 100.0),
                None => "n/a".into(),
            }
        ));
        out.push_str(&format!(
            "total recorded cost: {} — failed runs included\n",
            cost_line(self.total_cost, self.cost_completeness)
        ));
        match self.cost_per_accepted {
            Some(cost) => out.push_str(&format!(
                "cost per accepted task: {} (primary metric)\n",
                cost
            )),
            None => out.push_str("cost per accepted task: no accepted tasks in window\n"),
        }
        if self.pending_decisions > 0 {
            out.push_str(&format!(
                "awaiting a human: {} run(s) in needs_review/needs_decision/interrupted\n",
                self.pending_decisions
            ));
        }
        out
    }
}

/// Whether a run's final state leaves a person something to do. Total
/// over `State`, so a new state is a decision made here rather than a
/// silent omission from the count.
fn awaits_a_person(state: State) -> bool {
    match state {
        State::NeedsReview | State::NeedsDecision | State::Interrupted => true,
        State::Prepared
        | State::Running
        | State::Verifying
        | State::Repairing
        | State::Escalating
        | State::Accepted
        | State::Blocked
        | State::Failed
        | State::BudgetExhausted
        | State::Cancelled => false,
    }
}

/// Clip a line to `max` CHARACTERS, never bytes: the text is model prose
/// and a byte slice at a fixed offset panics whenever the boundary falls
/// inside a multi-byte character (an em dash, an accented word, an emoji).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head}…")
}

/// A cost and its completeness as one phrase, because the number alone
/// lies whenever usage went unreported: `$0 (unknown)` reads as free.
/// The reported sum is a lower bound the label qualifies; when nothing
/// at all was reported, there is no number to print (SPEC §11: never
/// replace missing usage with zero).
pub fn cost_line(cost: MicroUsd, completeness: CostCompleteness) -> String {
    match completeness {
        CostCompleteness::Unknown if cost == MicroUsd::ZERO => {
            "unknown (no usage was reported)".to_string()
        }
        CostCompleteness::Unknown => {
            format!("at least {cost} (unknown: some usage was not reported)")
        }
        CostCompleteness::IncompleteLowerBound => format!("at least {cost} (incomplete)"),
        other => format!("{cost} ({})", completeness_label(other)),
    }
}

pub fn completeness_label(completeness: CostCompleteness) -> &'static str {
    match completeness {
        CostCompleteness::Actual => "actual",
        CostCompleteness::Estimated => "estimated",
        CostCompleteness::IncompleteLowerBound => "incomplete lower bound",
        CostCompleteness::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_cost_line_never_reads_unreported_usage_as_free() {
        use super::*;
        assert_eq!(
            cost_line(MicroUsd::ZERO, CostCompleteness::Unknown),
            "unknown (no usage was reported)"
        );
        assert_eq!(
            cost_line(MicroUsd::from_micros(1_500_000), CostCompleteness::Unknown),
            "at least $1.5 (unknown: some usage was not reported)"
        );
        assert_eq!(
            cost_line(
                MicroUsd::from_micros(20),
                CostCompleteness::IncompleteLowerBound
            ),
            "at least $0.00002 (incomplete)"
        );
        assert_eq!(
            cost_line(MicroUsd::from_micros(1_000_000), CostCompleteness::Actual),
            "$1 (actual)"
        );
        assert_eq!(
            cost_line(MicroUsd::ZERO, CostCompleteness::Actual),
            "$0 (actual)",
            "a reported zero is a zero"
        );
    }

    use super::*;
    use crate::ledger::{now_rfc3339, Transition};

    /// A test directory nobody else can collide with, pre-cleaned so a
    /// crashed earlier run cannot decide this one. The pid alone is not
    /// enough: two tests in one process shared it, and the second opened
    /// the first's ledger.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "relais-report-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // Best effort: usually absent, and `create_dir_all` below reports
        // anything that keeps it from being made.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// The detail line is reviewer prose: a fixed BYTE slice at 120 in a
    /// sentence whose 121st byte lands inside an em dash panics.
    #[test]
    fn render_truncates_on_char_boundaries_not_bytes() {
        // 119 ASCII characters, then an em dash straddling byte 120.
        let detail = format!("{}—{}", "a".repeat(119), "b".repeat(50));
        assert!(!detail.is_char_boundary(120), "the fixture must straddle");
        let report = Report {
            since: "2026-09-01".into(),
            runs: vec![RunLine {
                run_id: "run-a".into(),
                status: State::Accepted,
                attempts: 1,
                cost: MicroUsd::from_micros(10),
                cost_completeness: CostCompleteness::Actual,
                models: vec!["haiku".into()],
                final_detail: Some(detail),
            }],
            accepted: 1,
            total_cost: MicroUsd::from_micros(10),
            cost_per_accepted: Some(MicroUsd::from_micros(10)),
            acceptance_rate: Some(1.0),
            pending_decisions: 0,
            cost_completeness: CostCompleteness::Actual,
        };
        let rendered = report.render();
        assert!(
            rendered.contains(&format!("    {}—…\n", "a".repeat(119))),
            "{rendered}"
        );
    }

    #[test]
    fn truncate_chars_keeps_short_text_and_clips_by_character() {
        assert_eq!(truncate_chars("héllo", 120), "héllo");
        assert_eq!(truncate_chars("héllo", 2), "hé…");
        assert_eq!(truncate_chars("—————", 3), "———…");
    }

    #[test]
    fn cost_per_accepted_counts_failed_runs_in_the_numerator() {
        let dir = temp_dir("aggr");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        // Two runs: one accepted at 100, one failed at 50.
        for (run_id, status, cost) in [
            ("run-a", State::Accepted, 100i64),
            ("run-b", State::Failed, 50),
        ] {
            ledger.insert_run(run_id, "/repo", None).expect("run");
            ledger
                .record_usage(&crate::ledger::UsageEvent {
                    event_id: format!("e-{run_id}"),
                    run_id: run_id.into(),
                    attempt_id: None,
                    parent_event_id: None,
                    model: Some("sonnet".into()),
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cost: Some(MicroUsd::from_micros(cost)),
                    cost_kind: crate::money::CostKind::ApiSpend,
                    completeness: CostCompleteness::Actual,
                    inclusive: false,
                    at: now_rfc3339(),
                })
                .expect("usage");
            ledger
                .record_transition(&Transition {
                    run_id: run_id.into(),
                    attempt_id: None,
                    from_state: Some(State::Prepared),
                    to_state: status,
                    reason: "t".into(),
                    detail: None,
                    at: now_rfc3339(),
                })
                .expect("transition");
        }
        let early = "2000-01-01T00:00:00+00:00";
        let report = runs_report(&ledger, early).expect("report");
        assert_eq!(report.accepted, 1);
        assert_eq!(report.total_cost, MicroUsd::from_micros(150));
        assert_eq!(report.cost_per_accepted, Some(MicroUsd::from_micros(150)));
        let text = report.render();
        assert!(text.contains("cost per accepted task: $0.00015"), "{text}");
        assert!(text.contains("failed runs included"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pending_decisions_and_labels() {
        let dir = temp_dir("pending");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        ledger.insert_run("run-1", "/repo", None).expect("run");
        ledger
            .record_transition(&Transition {
                run_id: "run-1".into(),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::NeedsDecision,
                reason: "test".into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        let report = runs_report(&ledger, "2000-01-01T00:00:00+00:00").expect("report");
        assert_eq!(report.runs.len(), 1);
        assert_eq!(report.pending_decisions, 1);
        assert_eq!(completeness_label(CostCompleteness::Unknown), "unknown");
        drop(ledger);
        std::fs::remove_dir_all(&dir).ok();
    }
}
