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

use crate::ledger::{Ledger, TaskOrigin};
use crate::lifecycle::State;
use crate::money::{CostCompleteness, MicroUsd};

/// Bumped whenever a top-level `Report` key is added, renamed or removed
/// (this task added the task-spine fields), so a downstream parser can
/// tell an old shape from a new one instead of guessing from key presence.
pub const REPORT_SCHEMA_VERSION: u32 = 2;

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
    /// Accepted, and its task's latest recorded outcome (if any) has not
    /// withdrawn the change. Always `false` for a run that never
    /// accepted.
    pub standing: bool,
}

/// One task's contribution to the window (SPEC §20's task spine): every
/// run the task ever had, whatever run window they individually fall in,
/// because a revised task's cost is not split across its attempts.
#[derive(Debug, Clone, Serialize)]
pub struct TaskLine {
    pub task_id: String,
    pub runs: usize,
    pub cost: MicroUsd,
    pub cost_completeness: CostCompleteness,
    /// At least one of the task's runs reached `Accepted`.
    pub accepted: bool,
    /// Accepted, and the task's latest recorded outcome (if any) has not
    /// withdrawn it. Absence of feedback is never a negative label.
    pub standing: bool,
    /// Reconstructed by the v4 migration for a run written before the
    /// task spine existed, rather than minted at dispatch time.
    pub backfilled: bool,
    /// Accepted, with no outcome recorded yet: a change the dataset is
    /// still owed a label for.
    pub pending_feedback: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub since: String,
    pub runs: Vec<RunLine>,
    pub accepted: usize,
    /// Accepted runs whose task's latest recorded outcome has not
    /// withdrawn the change (SPEC §20): `accepted` minus the accepted
    /// runs whose task's `latest_outcome` is `reverted`. A task with no
    /// feedback recorded counts as standing — absence of feedback is
    /// never a negative label. The outcomes table's first reader: this
    /// is the only place `relais report` looks at it.
    pub standing: usize,
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
    /// One line per task whose first run falls in the window (SPEC §20):
    /// the unit the primary metric is actually denominated in, a run is
    /// only an attempt at it.
    pub tasks: Vec<TaskLine>,
    /// The completeness of the PER-TASK figures, folded from the tasks
    /// the numerator sums — not from the run window, which is a
    /// different population: a run created in the window whose task
    /// started before it is in `runs` and not in `tasks`, so the run
    /// window's label can call a fully-actual per-task figure unknown,
    /// or miss an unknown one.
    pub task_cost_completeness: CostCompleteness,
    pub accepted_tasks: usize,
    pub standing_tasks: usize,
    /// Total cost of every run of every in-window task, whatever its
    /// outcome, divided by `accepted_tasks`: the primary metric (SPEC
    /// §11), denominated in tasks rather than runs so a task retried to
    /// acceptance is not counted as multiple cheaper successes.
    pub cost_per_accepted_task: Option<MicroUsd>,
    /// Same numerator, divided by `standing_tasks`: what an accepted
    /// change actually cost once later-reverted tasks are excluded.
    pub cost_per_standing_change: Option<MicroUsd>,
    /// Accepted tasks with no recorded outcome yet.
    pub pending_feedback: usize,
    pub backfilled_tasks: usize,
}

pub fn runs_report(ledger: &Ledger, since: &str) -> Result<Report, crate::ledger::LedgerError> {
    let mut runs = Vec::new();
    // One pass over the window's outcomes instead of a `latest_outcome`
    // per accepted run: the rows are ordered oldest first, so the last
    // one written for a task is the one that stands. A task with no row
    // is absent from the map and counts as standing — absence of
    // feedback is never a negative label (SPEC §20).
    let mut withdrawn_tasks: std::collections::BTreeMap<crate::ids::TaskId, bool> =
        std::collections::BTreeMap::new();
    for stored in ledger.outcomes_since(since)? {
        withdrawn_tasks.insert(stored.task_id, stored.outcome.kind.withdraws_acceptance());
    }
    for (run_id, _repo, status, _created) in ledger.runs_since(since)? {
        let status =
            State::parse(&status).map_err(|unknown| crate::ledger::LedgerError::Corrupt {
                what: format!("status of run {run_id}"),
                detail: unknown.to_string(),
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
        let mut standing = status == State::Accepted;
        if standing {
            if let Some(task_id) = ledger.task_of_run(&run_id)? {
                if let Some(withdrawn) = withdrawn_tasks.get(&task_id) {
                    standing = !withdrawn;
                }
            }
        }
        runs.push(RunLine {
            run_id: run_id.to_string(),
            status,
            attempts,
            cost,
            cost_completeness: completeness,
            models,
            final_detail,
            standing,
        });
    }
    let accepted = runs
        .iter()
        .filter(|run| run.status == State::Accepted)
        .count();
    let standing = runs.iter().filter(|run| run.standing).count();
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

    // The task spine: a task in the window contributes every run it has,
    // whatever run's own timestamp is, because a revised task's third,
    // accepted run is not a separate cohort member from its first two,
    // failed ones.
    let mut tasks = Vec::new();
    for task_row in ledger.tasks_since(since)? {
        let runs_of_task = ledger.runs_of_task(&task_row.task_id)?;
        let mut accepted_task = false;
        for run_id in &runs_of_task {
            if ledger.run_status(run_id)? == Some(State::Accepted) {
                accepted_task = true;
                break;
            }
        }
        let cost = ledger.task_cost(&task_row.task_id)?;
        let cost_completeness = ledger.task_cost_completeness(&task_row.task_id)?;
        let latest_outcome = ledger.latest_outcome(&task_row.task_id)?;
        let standing_task = accepted_task
            && !latest_outcome
                .as_ref()
                .is_some_and(|stored| stored.outcome.kind.withdraws_acceptance());
        let pending_feedback = accepted_task && latest_outcome.is_none();
        tasks.push(TaskLine {
            task_id: task_row.task_id.as_str().to_string(),
            runs: runs_of_task.len(),
            cost,
            cost_completeness,
            accepted: accepted_task,
            standing: standing_task,
            backfilled: task_row.origin == TaskOrigin::Backfilled,
            pending_feedback,
        });
    }
    let accepted_tasks = tasks.iter().filter(|task| task.accepted).count();
    let standing_tasks = tasks.iter().filter(|task| task.standing).count();
    let pending_feedback = tasks.iter().filter(|task| task.pending_feedback).count();
    let backfilled_tasks = tasks.iter().filter(|task| task.backfilled).count();
    let total_task_cost = tasks
        .iter()
        .fold(MicroUsd::ZERO, |acc, task| acc + task.cost);
    let cost_per_accepted_task = if accepted_tasks > 0 {
        Some(MicroUsd::from_micros(
            (total_task_cost.to_micros() as f64 / accepted_tasks as f64).round() as i64,
        ))
    } else {
        None
    };
    let cost_per_standing_change = if standing_tasks > 0 {
        Some(MicroUsd::from_micros(
            (total_task_cost.to_micros() as f64 / standing_tasks as f64).round() as i64,
        ))
    } else {
        None
    };

    Ok(Report {
        schema_version: REPORT_SCHEMA_VERSION,
        since: since.to_string(),
        runs,
        accepted,
        standing,
        total_cost,
        cost_per_accepted,
        acceptance_rate,
        pending_decisions,
        cost_completeness,
        task_cost_completeness: CostCompleteness::worst(
            tasks.iter().map(|task| task.cost_completeness),
        ),
        tasks,
        accepted_tasks,
        standing_tasks,
        cost_per_accepted_task,
        cost_per_standing_change,
        pending_feedback,
        backfilled_tasks,
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
            Some(cost) => out.push_str(&format!("cost per accepted run: {}\n", cost)),
            None => out.push_str("cost per accepted run: no accepted tasks in window\n"),
        }
        out.push_str(&format!(
            "still standing: {} of {} accepted ({} later reverted)\n",
            self.standing,
            self.accepted,
            self.accepted.saturating_sub(self.standing)
        ));
        if self.pending_decisions > 0 {
            out.push_str(&format!(
                "awaiting a human: {} run(s) in needs_review/needs_decision/interrupted\n",
                self.pending_decisions
            ));
        }
        out.push('\n');
        out.push_str(&format!(
            "tasks in window: {} ({} backfilled)\n",
            self.tasks.len(),
            self.backfilled_tasks
        ));
        out.push_str(&format!(
            "accepted tasks: {} of {} ({} standing, {} later reverted)\n",
            self.accepted_tasks,
            self.tasks.len(),
            self.standing_tasks,
            self.accepted_tasks.saturating_sub(self.standing_tasks)
        ));
        let feedback_coverage = if self.accepted_tasks > 0 {
            format!(
                "feedback {}/{} accepted tasks",
                self.accepted_tasks - self.pending_feedback,
                self.accepted_tasks
            )
        } else {
            "no accepted tasks".to_string()
        };
        // Through `cost_line`, like every other figure: an unknown
        // completeness with nothing recorded must not render as `$0`,
        // which reads as free — least of all on the line this report
        // exists to print.
        match self.cost_per_accepted_task {
            Some(cost) => out.push_str(&format!(
                "cost per accepted task: {} ({}) (primary metric)\n",
                cost_line(cost, self.task_cost_completeness),
                feedback_coverage
            )),
            None => out.push_str("cost per accepted task: no accepted tasks in window\n"),
        }
        match self.cost_per_standing_change {
            Some(cost) => out.push_str(&format!(
                "cost per standing change: {}\n",
                cost_line(cost, self.task_cost_completeness)
            )),
            None => out.push_str("cost per standing change: no standing changes in window\n"),
        }
        if self.pending_feedback > 0 {
            out.push_str(&format!(
                "pending feedback: {} accepted task(s) awaiting an outcome\n",
                self.pending_feedback
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
        crate::test_support::temp_dir(&format!("report-{name}"))
    }

    /// The detail line is reviewer prose: a fixed BYTE slice at 120 in a
    /// sentence whose 121st byte lands inside an em dash panics.
    #[test]
    fn render_truncates_on_char_boundaries_not_bytes() {
        // 119 ASCII characters, then an em dash straddling byte 120.
        let detail = format!("{}—{}", "a".repeat(119), "b".repeat(50));
        assert!(!detail.is_char_boundary(120), "the fixture must straddle");
        let report = Report {
            schema_version: REPORT_SCHEMA_VERSION,
            since: "2026-09-01".into(),
            runs: vec![RunLine {
                run_id: "run-a".into(),
                status: State::Accepted,
                attempts: 1,
                cost: MicroUsd::from_micros(10),
                cost_completeness: CostCompleteness::Actual,
                models: vec!["haiku".into()],
                final_detail: Some(detail),
                standing: true,
            }],
            accepted: 1,
            standing: 1,
            total_cost: MicroUsd::from_micros(10),
            cost_per_accepted: Some(MicroUsd::from_micros(10)),
            acceptance_rate: Some(1.0),
            pending_decisions: 0,
            cost_completeness: CostCompleteness::Actual,
            tasks: vec![TaskLine {
                task_id: "task-a".into(),
                runs: 1,
                cost: MicroUsd::from_micros(10),
                cost_completeness: CostCompleteness::Actual,
                accepted: true,
                standing: true,
                backfilled: false,
                pending_feedback: true,
            }],
            accepted_tasks: 1,
            standing_tasks: 1,
            task_cost_completeness: CostCompleteness::Actual,
            cost_per_accepted_task: Some(MicroUsd::from_micros(10)),
            cost_per_standing_change: Some(MicroUsd::from_micros(10)),
            pending_feedback: 1,
            backfilled_tasks: 0,
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
            let run = crate::ids::RunId::from_stored(run_id);
            let task = crate::ids::TaskId::from_stored(format!("task-{run_id}"));
            ledger
                .insert_run(&run, "/repo", None, &task, "rk")
                .expect("run");
            ledger
                .record_usage(&crate::ledger::UsageEvent {
                    event_id: format!("e-{run_id}"),
                    run_id: run.clone(),
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
                    run_id: run.clone(),
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
        assert!(text.contains("cost per accepted run: $0.00015"), "{text}");
        assert!(text.contains("failed runs included"), "{text}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pending_decisions_and_labels() {
        let dir = temp_dir("pending");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = crate::ids::RunId::from_stored("run-1");
        ledger
            .insert_run(
                &run,
                "/repo",
                None,
                &crate::ids::TaskId::from_stored("task-run-1"),
                "rk",
            )
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
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

    /// SPEC's task spine: a task revised to acceptance is one cohort
    /// member, not three, but its cost is not thrown away either — the
    /// failed attempts' spend is still real money the primary metric must
    /// count.
    #[test]
    fn a_task_revised_to_acceptance_counts_its_runs_cost_once_as_one_accepted_task() {
        let dir = temp_dir("revised-to-acceptance");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let task = crate::ids::TaskId::from_stored("task-revised");
        for (run_id, status, cost) in [
            ("run-1", State::Failed, 10i64),
            ("run-2", State::Failed, 20),
            ("run-3", State::Accepted, 30),
        ] {
            let run = crate::ids::RunId::from_stored(run_id);
            ledger
                .insert_run(&run, "/repo", None, &task, "rk")
                .expect("run");
            ledger
                .record_usage(&crate::ledger::UsageEvent {
                    event_id: format!("e-{run_id}"),
                    run_id: run.clone(),
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
                    run_id: run.clone(),
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
        assert_eq!(report.tasks.len(), 1, "one task, three runs");
        assert_eq!(report.tasks[0].runs, 3);
        assert_eq!(report.tasks[0].cost, MicroUsd::from_micros(60));
        assert_eq!(report.accepted_tasks, 1);
        assert_eq!(report.standing_tasks, 1);
        assert_eq!(
            report.cost_per_accepted_task,
            Some(MicroUsd::from_micros(60))
        );
        // Best effort: a leftover temp dir costs nothing but disk.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A downstream parser keys off exact top-level field names; a rename
    /// here should fail this test, not the parser in production.
    #[test]
    fn the_reports_json_shape_has_exactly_the_documented_top_level_keys() {
        let dir = temp_dir("json-shape");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let report = runs_report(&ledger, "2000-01-01T00:00:00+00:00").expect("report");
        let value = serde_json::to_value(&report).expect("report serializes");
        let object = value.as_object().expect("report is a JSON object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "acceptance_rate",
                "accepted",
                "accepted_tasks",
                "backfilled_tasks",
                "cost_completeness",
                "cost_per_accepted",
                "cost_per_accepted_task",
                "cost_per_standing_change",
                "pending_decisions",
                "pending_feedback",
                "runs",
                "schema_version",
                "since",
                "standing",
                "standing_tasks",
                "task_cost_completeness",
                "tasks",
                "total_cost",
            ]
        );
        // Best effort: a leftover temp dir costs nothing but disk.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The per-task figure wears the TASK cohort's completeness, not the
    /// run window's. The two populations differ: a run created inside the
    /// window whose task started before it is in `runs` and not in
    /// `tasks`, so borrowing the run window's label can call a
    /// fully-actual per-task figure unknown (found in review of
    /// run-65c1693e90caa-9a72).
    #[test]
    fn the_per_task_figure_wears_the_task_cohorts_completeness() {
        let dir = temp_dir("task-cohort-completeness");
        let path = dir.join("ledger.sqlite");
        // One handle per day, each with a clock that answers one time
        // forever: the migrations at open consume clock reads too, so a
        // list of times would have to know how many steps there are.
        let day = |at: &'static str| {
            Ledger::open_with_clock(&path, Box::new(crate::ledger::FixedClock::new([at])))
                .expect("ledger")
        };

        for (at, run_id, task_id, cost, completeness) in [
            (
                "2026-01-01T00:00:00+00:00",
                "run-old",
                "task-old",
                None,
                CostCompleteness::Unknown,
            ),
            (
                "2026-01-04T00:00:00+00:00",
                "run-new",
                "task-new",
                Some(MicroUsd::from_micros(500)),
                CostCompleteness::Actual,
            ),
        ] {
            let ledger = day(at);
            let run = crate::ids::RunId::from_stored(run_id);
            let task = crate::ids::TaskId::from_stored(task_id);
            ledger
                .insert_run(&run, "/repo", None, &task, "rk")
                .expect("run");
            ledger
                .record_usage(&crate::ledger::UsageEvent {
                    event_id: format!("e-{run_id}"),
                    run_id: run.clone(),
                    attempt_id: None,
                    parent_event_id: None,
                    model: Some("sonnet".into()),
                    input_tokens: Some(1),
                    output_tokens: Some(1),
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cost,
                    cost_kind: crate::money::CostKind::ApiSpend,
                    completeness,
                    inclusive: false,
                    at: now_rfc3339(),
                })
                .expect("usage");
            ledger
                .record_transition(&Transition {
                    run_id: run.clone(),
                    attempt_id: None,
                    from_state: Some(State::Prepared),
                    to_state: State::Accepted,
                    reason: "t".into(),
                    detail: None,
                    at: now_rfc3339(),
                })
                .expect("transition");
        }

        let ledger = day("2026-01-05T00:00:00+00:00");
        let report = runs_report(&ledger, "2026-01-02T00:00:00+00:00").expect("report");
        assert_eq!(
            report.tasks.len(),
            1,
            "only the in-window task is in the cohort: {:?}",
            report.tasks
        );
        assert_eq!(
            report.task_cost_completeness,
            CostCompleteness::Actual,
            "the per-task figure wears ITS completeness, not the older task's"
        );
        // Best effort: a leftover temp dir costs nothing but disk.
        std::fs::remove_dir_all(&dir).ok();
    }
}
