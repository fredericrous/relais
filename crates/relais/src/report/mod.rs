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

use crate::ledger::{DecisionRecord, Ledger, PhaseCost, TaskOrigin};
use crate::lifecycle::{Reason, State};
use crate::money::{CostCompleteness, MicroUsd};
use crate::outcome::OutcomeKind;

/// Whether the transition that landed this run in `Accepted` carries a
/// person's own answer (`relais decide --answer approve`) rather than
/// relais's own checks and review (`Reason::ChecksAndReviewPassed`) —
/// the fact the report's accepted-by-relais/accepted-by-person split is
/// read off, since nothing else on the run records who judged it.
/// `false` for a run that never reached `Accepted` at all.
///
/// The transition wanted is the one that ENTERED `Accepted`, which is
/// why `from_state` is part of the test. A run keeps recording
/// transitions after it becomes terminal — retiring its worktree writes
/// an `accepted -> accepted` row — so the LAST row landing on `Accepted`
/// is usually that retirement, and reading its reason reports every
/// person-approved run as relais-approved. The same same-state row
/// defeated `decide --answer approve`'s gap check (#43) and the v6
/// decision backfill (#44); this is the third reader to meet it.
fn accepted_by_person(
    ledger: &Ledger,
    run_id: &crate::ids::RunId,
) -> Result<bool, crate::ledger::LedgerError> {
    Ok(ledger
        .transitions(run_id)?
        .iter()
        .find(|transition| {
            transition.to_state == State::Accepted && transition.from_state != Some(State::Accepted)
        })
        .is_some_and(|transition| transition.reason == Reason::DecisionApproved.as_str()))
}

/// Bumped whenever a top-level `Report` key is added, renamed or removed
/// (this task added `enforcement`, the same summary `relais coordinator
/// status` prints, so its JSON agrees with the sentence a person sees),
/// so a downstream parser can tell an old shape from a new one instead of
/// guessing from key presence.
pub const REPORT_SCHEMA_VERSION: u32 = 6;

/// A dimension `relais report --by` groups the window's tasks over (SPEC
/// §11: "compare like task classes and policy versions"). Total over the
/// values a task can be grouped by, so a new dimension is a decision made
/// here rather than a silently unhandled CLI value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Dimension {
    TaskClass,
    Tier,
    Model,
    Policy,
    Repository,
}

impl Dimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TaskClass => "task_class",
            Self::Tier => "tier",
            Self::Model => "model",
            Self::Policy => "policy",
            Self::Repository => "repository",
        }
    }
}

impl Serialize for Dimension {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A task, already mapped by the ledger adapter, ready for the rate
/// functions below — none of which touch the ledger again. Keeping the
/// I/O and the arithmetic apart is what makes a rate testable with a
/// hand-built `Vec` instead of a database.
#[derive(Debug, Clone)]
struct CohortTaskRow {
    accepted: bool,
    standing: bool,
    /// Every run of the task has reached a terminal state (SPEC's
    /// decision spine): a task still in flight has not failed, so
    /// counting it against acceptance would misreport work that simply
    /// is not done yet.
    terminal: bool,
    escalated: bool,
    /// A reviewer produced at least one usage event for some run of the
    /// task — the review-correction rate's denominator.
    reviewed: bool,
    corrected: bool,
    regressed: bool,
    pending_feedback: bool,
    /// Summed wall-clock span of the task's completed runs; `None` when
    /// none of them has finished an attempt yet.
    duration_seconds: Option<f64>,
    cost: MicroUsd,
    cost_completeness: CostCompleteness,
}

/// Total cost of the cohort's tasks, divided by its accepted count — the
/// primary metric (SPEC §11), denominated per cohort instead of over the
/// whole window, so a comparison is between like task classes rather than
/// one number blending them.
fn cost_per_accepted_change(rows: &[CohortTaskRow]) -> Option<MicroUsd> {
    let accepted = rows.iter().filter(|row| row.accepted).count();
    if accepted == 0 {
        return None;
    }
    let total = rows.iter().fold(MicroUsd::ZERO, |acc, row| acc + row.cost);
    Some(MicroUsd::from_micros(
        (total.to_micros() as f64 / accepted as f64).round() as i64,
    ))
}

/// Accepted terminal tasks divided by terminal tasks: a task still in
/// flight is excluded from the denominator rather than counted as a
/// failure it has not had the chance to be.
fn acceptance_rate_over_terminal(rows: &[CohortTaskRow]) -> Option<f64> {
    let terminal: Vec<&CohortTaskRow> = rows.iter().filter(|row| row.terminal).collect();
    if terminal.is_empty() {
        return None;
    }
    Some(terminal.iter().filter(|row| row.accepted).count() as f64 / terminal.len() as f64)
}

/// Escalated tasks divided by every task in the cohort.
fn escalation_rate(rows: &[CohortTaskRow]) -> Option<f64> {
    if rows.is_empty() {
        return None;
    }
    Some(rows.iter().filter(|row| row.escalated).count() as f64 / rows.len() as f64)
}

/// Corrections divided by tasks whose reviewer actually ran — the count
/// reviewed, and the rate over it, since a rate alone hides whether the
/// denominator was one task or a hundred.
fn review_correction_rate(rows: &[CohortTaskRow]) -> Option<(usize, f64)> {
    let reviewed: Vec<&CohortTaskRow> = rows.iter().filter(|row| row.reviewed).collect();
    if reviewed.is_empty() {
        return None;
    }
    let corrected = reviewed.iter().filter(|row| row.corrected).count();
    Some((reviewed.len(), corrected as f64 / reviewed.len() as f64))
}

/// The middle value of the cohort's known task durations; tasks with no
/// completed run contribute nothing to it rather than a zero that would
/// pull the median toward work that has not finished.
fn median_duration_seconds(rows: &[CohortTaskRow]) -> Option<f64> {
    let mut durations: Vec<f64> = rows.iter().filter_map(|row| row.duration_seconds).collect();
    if durations.is_empty() {
        return None;
    }
    durations.sort_by(|a, b| a.partial_cmp(b).expect("a duration in seconds is finite"));
    let mid = durations.len() / 2;
    Some(if durations.len().is_multiple_of(2) {
        (durations[mid - 1] + durations[mid]) / 2.0
    } else {
        durations[mid]
    })
}

fn regressions_count(rows: &[CohortTaskRow]) -> usize {
    rows.iter().filter(|row| row.regressed).count()
}

fn pending_feedback_count(rows: &[CohortTaskRow]) -> usize {
    rows.iter().filter(|row| row.pending_feedback).count()
}

/// One dimension's value, and what the window's tasks with that value
/// cost, accepted, escalated and were corrected on — like compared with
/// like (SPEC §11), rather than one number blending every task class.
#[derive(Debug, Clone, Serialize)]
pub struct Cohort {
    pub key: String,
    pub tasks: usize,
    pub accepted: usize,
    pub standing: usize,
    pub terminal: usize,
    /// Tasks with no terminal run yet — reported separately from
    /// acceptance rather than folded into it as failures.
    pub in_flight: usize,
    pub cost: MicroUsd,
    pub cost_completeness: CostCompleteness,
    pub cost_per_accepted: Option<MicroUsd>,
    pub acceptance_rate: Option<f64>,
    pub escalation_rate: Option<f64>,
    /// Denominator of `review_correction_rate`: tasks whose reviewer
    /// actually ran.
    pub reviewed: usize,
    pub review_correction_rate: Option<f64>,
    pub median_duration_seconds: Option<f64>,
    pub regressions: usize,
    pub pending_feedback: usize,
}

fn build_cohort(key: String, rows: Vec<CohortTaskRow>) -> Cohort {
    let accepted = rows.iter().filter(|row| row.accepted).count();
    let standing = rows.iter().filter(|row| row.standing).count();
    let terminal = rows.iter().filter(|row| row.terminal).count();
    let cost = rows.iter().fold(MicroUsd::ZERO, |acc, row| acc + row.cost);
    let cost_completeness = CostCompleteness::worst(rows.iter().map(|row| row.cost_completeness));
    let cost_per_accepted = cost_per_accepted_change(&rows);
    let acceptance_rate = acceptance_rate_over_terminal(&rows);
    let escalation_rate = escalation_rate(&rows);
    let (reviewed, review_correction_rate) = match review_correction_rate(&rows) {
        Some((reviewed, rate)) => (reviewed, Some(rate)),
        None => (0, None),
    };
    let median_duration_seconds = median_duration_seconds(&rows);
    let regressions = regressions_count(&rows);
    let pending_feedback = pending_feedback_count(&rows);
    Cohort {
        key,
        tasks: rows.len(),
        accepted,
        standing,
        terminal,
        in_flight: rows.len() - terminal,
        cost,
        cost_completeness,
        cost_per_accepted,
        acceptance_rate,
        escalation_rate,
        reviewed,
        review_correction_rate,
        median_duration_seconds,
        regressions,
        pending_feedback,
    }
}

/// The window's tasks grouped over one dimension (SPEC §11).
#[derive(Debug, Clone, Serialize)]
pub struct CohortReport {
    pub dimension: Dimension,
    pub cohorts: Vec<Cohort>,
}

/// A task's bucket for one dimension. A task whose value is unknown —
/// never dispatched under a recorded contract, no receipt yet, no usage
/// recorded — lands in a named `"unknown"` bucket rather than being
/// dropped: a report that silently omits work is worse than one that
/// says it cannot classify it.
fn cohort_key(
    ledger: &Ledger,
    dimension: Dimension,
    task_row: &crate::ledger::TaskRow,
) -> Result<String, crate::ledger::LedgerError> {
    const UNKNOWN: &str = "unknown";
    Ok(match dimension {
        Dimension::Repository => task_row.repo_key.clone(),
        Dimension::Tier => ledger
            .run_contract_and_tier(&task_row.first_run)?
            .map(|(_, tier)| tier.as_str().to_string())
            .unwrap_or_else(|| UNKNOWN.to_string()),
        Dimension::TaskClass => ledger
            .run_contract_and_tier(&task_row.first_run)?
            .map(|(contract, _)| contract.verification_profile)
            .unwrap_or_else(|| UNKNOWN.to_string()),
        Dimension::Model => ledger
            .task_first_model(&task_row.task_id)?
            .unwrap_or_else(|| UNKNOWN.to_string()),
        Dimension::Policy => ledger
            .task_policy_hash(&task_row.task_id)?
            .map(|hash| hash.chars().take(12).collect())
            .unwrap_or_else(|| UNKNOWN.to_string()),
    })
}

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
    /// The run's own spend grouped by phase (SPEC §11): which phase —
    /// worker attempt, review, planning, integration — spent the money.
    /// `phase: None` is the unattributed bucket: usage recorded before
    /// this breakdown existed.
    pub phase_costs: Vec<PhaseCost>,
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
    /// The task's accepted run — when it has one — was judged by a
    /// person's `relais decide --answer approve`, not by relais's own
    /// checks and review. Meaningless when `accepted` is `false`.
    pub accepted_by_person: bool,
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
    /// runner and has not yet been answered: a run whose decision row is
    /// still open. A run that `relais decide` has answered is settled,
    /// whatever state it is terminal in — `revise` and `abandon` leave
    /// it where it was on purpose — so counting states here would report
    /// answered runs as waiting and contradict the open-decision list
    /// this report prints.
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
    /// `accepted_tasks` judged by relais's own checks and review
    /// (`Reason::ChecksAndReviewPassed`) — the part of `accepted_tasks`
    /// that was never a person's own answer. `accepted_tasks_by_relais +
    /// accepted_tasks_by_person == accepted_tasks` always; printed
    /// separately because a count that mixed the two would say nothing
    /// about how much of the primary metric's denominator relais itself
    /// vouched for.
    pub accepted_tasks_by_relais: usize,
    /// `accepted_tasks` judged by a person's `relais decide --answer
    /// approve` instead — most often a mandatory human-sign-off
    /// criterion's own answer (SPEC §10).
    pub accepted_tasks_by_person: usize,
    pub standing_tasks: usize,
    /// Total cost of every run of every in-window task, whatever its
    /// outcome, divided by `accepted_tasks` — both relais's own and a
    /// person's — the primary metric (SPEC §11), denominated in tasks
    /// rather than runs so a task retried to acceptance is not counted
    /// as multiple cheaper successes.
    pub cost_per_accepted_task: Option<MicroUsd>,
    /// Same numerator, divided by `standing_tasks`: what an accepted
    /// change actually cost once later-reverted tasks are excluded.
    pub cost_per_standing_change: Option<MicroUsd>,
    /// Accepted tasks with no recorded outcome yet.
    pub pending_feedback: usize,
    pub backfilled_tasks: usize,
    /// Every run still waiting on a person to answer it — a `needs_review`
    /// or `needs_decision` run nobody has decided, or an `interrupted` one
    /// nobody has resolved (SPEC's decision spine): not scoped to `since`,
    /// because a decision opened before the window is exactly as owed to
    /// a person today as one opened inside it.
    pub open_decisions: Vec<DecisionRecord>,
    /// Present only when `--by <dimension>` was given; the default report
    /// is unchanged when it is absent.
    pub cohorts: Option<CohortReport>,
    /// The same enforcement summary `relais coordinator status` prints,
    /// carried as structured data (SPEC §23) so a later tool reading this
    /// report's JSON is never told something the coordinator disagrees
    /// with. `runs_report` itself never touches a coordinator socket —
    /// it stays a pure function of the ledger — so this defaults to
    /// [`EnforcementReport::observed`]; `relais report`'s CLI layer
    /// overwrites it with a live snapshot when one is reachable.
    pub enforcement: EnforcementReport,
}

/// The same enforcement summary `relais coordinator status` prints (SPEC
/// §23), carried as structured data: the counts by
/// [`crate::admission::DispatchSource`] the sentence is computed from,
/// alongside the sentence itself, so a later reader never has to
/// re-derive it and risk disagreeing with what a person saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnforcementReport {
    pub enforcement: crate::admission::Enforcement,
    pub active_by_source: std::collections::BTreeMap<String, u32>,
    pub admitted_by_source: std::collections::BTreeMap<String, u32>,
    pub expired_hook_leases: u32,
    pub sentence: String,
}

impl EnforcementReport {
    /// No coordinator was reachable (or none was asked): the honest
    /// default is that nothing is capping anything, not a guess dressed
    /// up as one of the other two.
    pub fn observed() -> Self {
        Self::from_snapshot(
            crate::admission::Enforcement::Observed,
            &crate::admission::StatusSnapshot::default(),
        )
    }

    pub fn from_snapshot(
        enforcement: crate::admission::Enforcement,
        snapshot: &crate::admission::StatusSnapshot,
    ) -> Self {
        Self {
            enforcement,
            active_by_source: snapshot.active_by_source.clone(),
            admitted_by_source: snapshot.admitted_by_source.clone(),
            expired_hook_leases: snapshot.expired_hook_leases,
            sentence: crate::admission::enforcement_line(enforcement, snapshot),
        }
    }
}

pub fn runs_report(
    ledger: &Ledger,
    since: &str,
    by: Option<Dimension>,
) -> Result<Report, crate::ledger::LedgerError> {
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
        let phase_costs = ledger.run_cost_by_phase(&run_id)?;
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
            phase_costs,
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
    // An OPEN decision, not a state: once `relais decide` answers a run
    // the person is no longer owed anything, and `revise`/`abandon`
    // deliberately leave the run terminal in the state it reached. A
    // count by state would keep reporting answered runs as waiting, and
    // disagree with the list of open decisions printed below it.
    let open_decisions = ledger.open_decisions()?;
    let pending_decisions = open_decisions
        .iter()
        .filter(|decision| runs.iter().any(|run| run.run_id == decision.run.as_str()))
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
    let mut cohort_rows: std::collections::BTreeMap<String, Vec<CohortTaskRow>> =
        std::collections::BTreeMap::new();
    for task_row in ledger.tasks_since(since)? {
        let runs_of_task = ledger.runs_of_task(&task_row.task_id)?;
        let mut accepted_task = false;
        let mut accepted_task_by_person = false;
        let mut terminal = true;
        let mut escalated = false;
        let mut reviewed = false;
        let mut duration_seconds: Option<f64> = None;
        for run_id in &runs_of_task {
            let status = ledger.run_status(run_id)?;
            if status == Some(State::Accepted) {
                accepted_task = true;
                if accepted_by_person(ledger, run_id)? {
                    accepted_task_by_person = true;
                }
            }
            if by.is_some() {
                if !status.is_some_and(State::is_terminal) {
                    terminal = false;
                }
                if ledger.escalation_attempted(run_id)? {
                    escalated = true;
                }
                if ledger.review_attempted(run_id)? {
                    reviewed = true;
                }
                if let Some(run_duration) = ledger.run_duration_seconds(run_id)? {
                    duration_seconds = Some(duration_seconds.unwrap_or(0.0) + run_duration);
                }
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
        if let Some(dimension) = by {
            let key = cohort_key(ledger, dimension, &task_row)?;
            cohort_rows.entry(key).or_default().push(CohortTaskRow {
                accepted: accepted_task,
                standing: standing_task,
                terminal,
                escalated,
                reviewed,
                corrected: latest_outcome
                    .as_ref()
                    .is_some_and(|stored| stored.outcome.kind == OutcomeKind::Corrected),
                regressed: latest_outcome
                    .as_ref()
                    .is_some_and(|stored| stored.outcome.kind == OutcomeKind::ConfirmedRegression),
                pending_feedback,
                duration_seconds,
                cost,
                cost_completeness,
            });
        }
        tasks.push(TaskLine {
            task_id: task_row.task_id.as_str().to_string(),
            runs: runs_of_task.len(),
            cost,
            cost_completeness,
            accepted: accepted_task,
            standing: standing_task,
            backfilled: task_row.origin == TaskOrigin::Backfilled,
            pending_feedback,
            accepted_by_person: accepted_task_by_person,
        });
    }
    let cohorts = by.map(|dimension| CohortReport {
        dimension,
        cohorts: cohort_rows
            .into_iter()
            .map(|(key, rows)| build_cohort(key, rows))
            .collect(),
    });
    let accepted_tasks = tasks.iter().filter(|task| task.accepted).count();
    let accepted_tasks_by_person = tasks
        .iter()
        .filter(|task| task.accepted && task.accepted_by_person)
        .count();
    let accepted_tasks_by_relais = accepted_tasks - accepted_tasks_by_person;
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
        accepted_tasks_by_relais,
        accepted_tasks_by_person,
        standing_tasks,
        cost_per_accepted_task,
        cost_per_standing_change,
        pending_feedback,
        backfilled_tasks,
        open_decisions,
        cohorts,
        enforcement: EnforcementReport::observed(),
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
            for phase_cost in &run.phase_costs {
                out.push_str(&format!(
                    "    {:<12} {}\n",
                    phase_cost
                        .phase
                        .map(|phase| phase.as_str())
                        .unwrap_or("unattributed"),
                    cost_line(phase_cost.cost, phase_cost.completeness),
                ));
            }
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
                "awaiting a human: {} run(s) with an unanswered decision\n",
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
        out.push_str(&format!(
            "  judged by relais: {}, judged by a person: {}\n",
            self.accepted_tasks_by_relais, self.accepted_tasks_by_person
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
                "cost per accepted task (N={}, relais {} + person {}): {} ({}) (primary metric)\n",
                self.accepted_tasks,
                self.accepted_tasks_by_relais,
                self.accepted_tasks_by_person,
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
        if !self.open_decisions.is_empty() {
            out.push('\n');
            out.push_str("open decisions (a person has not yet answered):\n");
            for decision in &self.open_decisions {
                out.push_str(&format!(
                    "  {}  {} ({}) waiting {}s\n",
                    decision.run,
                    decision.raised_state,
                    decision.raised_reason,
                    decision.waited_seconds
                ));
            }
        }
        if let Some(cohort_report) = &self.cohorts {
            out.push('\n');
            out.push_str(&format!("by {}:\n", cohort_report.dimension.as_str()));
            for cohort in &cohort_report.cohorts {
                out.push_str(&format!(
                    "  {}  {} tasks, {} accepted, {} standing, {} in flight\n",
                    cohort.key, cohort.tasks, cohort.accepted, cohort.standing, cohort.in_flight
                ));
                match cohort.cost_per_accepted {
                    Some(cost) => out.push_str(&format!(
                        "      cost per accepted change: {}\n",
                        cost_line(cost, cohort.cost_completeness)
                    )),
                    None => out
                        .push_str("      cost per accepted change: no accepted tasks in cohort\n"),
                }
                out.push_str(&format!(
                    "      acceptance rate (of {} terminal): {}\n",
                    cohort.terminal,
                    match cohort.acceptance_rate {
                        Some(rate) => format!("{:.0}%", rate * 100.0),
                        None => "no terminal tasks".to_string(),
                    }
                ));
                out.push_str(&format!(
                    "      escalation rate: {}\n",
                    match cohort.escalation_rate {
                        Some(rate) => format!("{:.0}%", rate * 100.0),
                        None => "no tasks".to_string(),
                    }
                ));
                out.push_str(&format!(
                    "      review-correction rate (of {} reviewed): {}\n",
                    cohort.reviewed,
                    match cohort.review_correction_rate {
                        Some(rate) => format!("{:.0}%", rate * 100.0),
                        None => "no reviewed tasks".to_string(),
                    }
                ));
                out.push_str(&format!(
                    "      median duration: {}\n",
                    match cohort.median_duration_seconds {
                        Some(seconds) => format!("{seconds:.0}s"),
                        None => "no completed tasks".to_string(),
                    }
                ));
                out.push_str(&format!(
                    "      regressions: {}, pending feedback: {}\n",
                    cohort.regressions, cohort.pending_feedback
                ));
            }
        }
        out.push('\n');
        out.push_str(&self.enforcement.sentence);
        out.push('\n');
        out
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

    /// The default every `relais report` starts from, and the one that
    /// shipped contradicting itself: with no coordinator reachable the
    /// summary says nothing is capped, so its sentence must not go on to
    /// describe the caps. This is the path a report takes whenever no
    /// daemon is running, not an edge case.
    #[test]
    fn the_observed_summary_does_not_describe_caps_it_just_denied() {
        let observed = EnforcementReport::observed();
        assert_eq!(
            observed.enforcement,
            crate::admission::Enforcement::Observed
        );
        let lower = observed.sentence.to_lowercase();
        assert!(lower.contains("nothing is capped"), "{}", observed.sentence);
        assert!(
            !lower.contains("are capped on agent count"),
            "{}",
            observed.sentence
        );
    }
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

    /// Every flag false, no duration, the given cost: a base row a test
    /// overrides only the fields its scenario cares about, rather than
    /// naming all eleven positionally.
    fn cohort_row(cost: MicroUsd) -> CohortTaskRow {
        CohortTaskRow {
            accepted: false,
            standing: false,
            terminal: false,
            escalated: false,
            reviewed: false,
            corrected: false,
            regressed: false,
            pending_feedback: false,
            duration_seconds: None,
            cost,
            cost_completeness: CostCompleteness::Actual,
        }
    }

    #[test]
    fn cost_per_accepted_change_divides_total_cost_by_accepted_tasks() {
        let rows = vec![
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                ..cohort_row(MicroUsd::from_micros(100))
            },
            CohortTaskRow {
                terminal: true,
                ..cohort_row(MicroUsd::from_micros(50))
            },
        ];
        assert_eq!(
            cost_per_accepted_change(&rows),
            Some(MicroUsd::from_micros(150)),
            "divides all recorded cost by the accepted count, failed tasks included"
        );
    }

    #[test]
    fn cost_per_accepted_change_has_none_with_no_accepted_tasks() {
        let rows = vec![CohortTaskRow {
            terminal: true,
            ..cohort_row(MicroUsd::from_micros(50))
        }];
        assert_eq!(
            cost_per_accepted_change(&rows),
            None,
            "a zero denominator prints that it has none, not a zero"
        );
    }

    #[test]
    fn acceptance_rate_divides_accepted_by_terminal_and_excludes_in_flight() {
        let rows = vec![
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                ..cohort_row(MicroUsd::ZERO)
            },
            CohortTaskRow {
                terminal: true,
                ..cohort_row(MicroUsd::ZERO)
            },
            // In flight: not terminal, excluded from both sides of the rate.
            cohort_row(MicroUsd::ZERO),
        ];
        assert_eq!(
            acceptance_rate_over_terminal(&rows),
            Some(0.5),
            "divides accepted TERMINAL tasks by terminal tasks, not by every task"
        );
    }

    #[test]
    fn acceptance_rate_is_none_with_no_terminal_tasks() {
        let rows = vec![cohort_row(MicroUsd::ZERO)];
        assert_eq!(acceptance_rate_over_terminal(&rows), None);
    }

    #[test]
    fn escalation_rate_divides_escalated_by_every_task() {
        let rows = vec![
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                escalated: true,
                ..cohort_row(MicroUsd::ZERO)
            },
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                ..cohort_row(MicroUsd::ZERO)
            },
        ];
        assert_eq!(escalation_rate(&rows), Some(0.5));
    }

    #[test]
    fn review_correction_rate_divides_corrections_by_reviewed_tasks_only() {
        let rows = vec![
            // Reviewed and corrected.
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                reviewed: true,
                corrected: true,
                ..cohort_row(MicroUsd::ZERO)
            },
            // Reviewed, not corrected.
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                reviewed: true,
                ..cohort_row(MicroUsd::ZERO)
            },
            // Never reviewed: excluded from the denominator entirely, even
            // though this one WAS corrected — a rate over never-reviewed
            // tasks says nothing about review quality.
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                corrected: true,
                ..cohort_row(MicroUsd::ZERO)
            },
        ];
        assert_eq!(
            review_correction_rate(&rows),
            Some((2, 0.5)),
            "the denominator is tasks whose reviewer actually ran, not every task"
        );
    }

    #[test]
    fn review_correction_rate_is_none_with_no_reviewed_tasks() {
        let rows = vec![CohortTaskRow {
            accepted: true,
            standing: true,
            terminal: true,
            ..cohort_row(MicroUsd::ZERO)
        }];
        assert_eq!(review_correction_rate(&rows), None);
    }

    #[test]
    fn median_duration_seconds_ignores_tasks_with_no_completed_run() {
        let rows = vec![
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                duration_seconds: Some(10.0),
                ..cohort_row(MicroUsd::ZERO)
            },
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                duration_seconds: Some(30.0),
                ..cohort_row(MicroUsd::ZERO)
            },
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                duration_seconds: Some(20.0),
                ..cohort_row(MicroUsd::ZERO)
            },
            // Still in flight: no duration yet, must not pull the median
            // toward zero.
            cohort_row(MicroUsd::ZERO),
        ];
        assert_eq!(median_duration_seconds(&rows), Some(20.0));
    }

    #[test]
    fn median_duration_seconds_is_none_with_no_completed_tasks() {
        let rows = vec![cohort_row(MicroUsd::ZERO)];
        assert_eq!(median_duration_seconds(&rows), None);
    }

    #[test]
    fn regressions_and_pending_feedback_count_their_own_flag_only() {
        let rows = vec![
            CohortTaskRow {
                accepted: true,
                terminal: true,
                regressed: true,
                ..cohort_row(MicroUsd::ZERO)
            },
            CohortTaskRow {
                accepted: true,
                standing: true,
                terminal: true,
                pending_feedback: true,
                ..cohort_row(MicroUsd::ZERO)
            },
        ];
        assert_eq!(regressions_count(&rows), 1);
        assert_eq!(pending_feedback_count(&rows), 1);
    }

    #[test]
    fn a_task_unknown_in_a_dimension_lands_in_the_named_unknown_bucket() {
        let dir = temp_dir("cohort-unknown-bucket");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = crate::ids::RunId::from_stored("run-bare");
        let task = crate::ids::TaskId::from_stored("task-bare");
        // A run with no contract revision, no receipt, no usage: every
        // dimension must still classify it rather than drop it.
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        let task_row = ledger
            .tasks_since("2000-01-01T00:00:00+00:00")
            .expect("tasks")
            .into_iter()
            .find(|row| row.task_id == task)
            .expect("the bare task is in the window");
        for dimension in [Dimension::TaskClass, Dimension::Tier, Dimension::Model] {
            assert_eq!(
                cohort_key(&ledger, dimension, &task_row).expect("cohort key"),
                "unknown",
                "{dimension:?} of an undispatched task"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

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
                phase_costs: vec![],
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
                accepted_by_person: false,
            }],
            accepted_tasks: 1,
            accepted_tasks_by_relais: 1,
            accepted_tasks_by_person: 0,
            standing_tasks: 1,
            task_cost_completeness: CostCompleteness::Actual,
            cost_per_accepted_task: Some(MicroUsd::from_micros(10)),
            cost_per_standing_change: Some(MicroUsd::from_micros(10)),
            pending_feedback: 1,
            backfilled_tasks: 0,
            open_decisions: vec![],
            cohorts: None,
            enforcement: EnforcementReport::observed(),
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
                    phase: None,
                    duration_ms: None,
                    requested_model: None,
                    requested_effort: None,
                    harness: None,
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
        let report = runs_report(&ledger, early, None).expect("report");
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
        let report = runs_report(&ledger, "2000-01-01T00:00:00+00:00", None).expect("report");
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
                    phase: None,
                    duration_ms: None,
                    requested_model: None,
                    requested_effort: None,
                    harness: None,
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
        let report = runs_report(&ledger, early, None).expect("report");
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
        let report = runs_report(&ledger, "2000-01-01T00:00:00+00:00", None).expect("report");
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
                "accepted_tasks_by_person",
                "accepted_tasks_by_relais",
                "backfilled_tasks",
                "cohorts",
                "cost_completeness",
                "cost_per_accepted",
                "cost_per_accepted_task",
                "cost_per_standing_change",
                "enforcement",
                "open_decisions",
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
                    phase: None,
                    duration_ms: None,
                    requested_model: None,
                    requested_effort: None,
                    harness: None,
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
        let report = runs_report(&ledger, "2026-01-02T00:00:00+00:00", None).expect("report");
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
    /// The failure this test exists for: `accepted_by_person` read the
    /// LAST transition landing on `Accepted`, and retiring a run's
    /// worktree writes an `accepted -> accepted` row whose reason is
    /// `worktree_retired`. Every person-approved run whose tree had been
    /// retired — which is every one of them, since retirement follows
    /// acceptance — was therefore reported as accepted by relais.
    /// Measured live: two runs approved by hand, the report said
    /// "judged by relais: 5, judged by a person: 1".
    #[test]
    fn a_retired_worktree_does_not_disguise_a_person_approved_run() {
        let dir = temp_dir("retired-person-approved");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = crate::ids::RunId::from_stored("run-approved");
        let task = crate::ids::TaskId::from_stored("task-approved");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Running),
                to_state: State::NeedsDecision,
                reason: "scope_exceeded".into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        ledger
            .resolve_decision(
                &run,
                &crate::ledger::DecisionAnswer {
                    resolution: Reason::DecisionApproved,
                    actor: "a person",
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsDecision,
                    to_state: State::Accepted,
                },
            )
            .expect("approved");

        let early = "2000-01-01T00:00:00+00:00";
        let before = runs_report(&ledger, early, None).expect("report");
        assert_eq!(before.accepted_tasks_by_person, 1, "{before:?}");

        // Retirement: the same-state row that used to hide the approval.
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Accepted),
                to_state: State::Accepted,
                reason: "worktree_retired".into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("retirement");

        let after = runs_report(&ledger, early, None).expect("report");
        assert_eq!(
            after.accepted_tasks_by_person, 1,
            "retiring the worktree does not change who approved the run: {after:?}"
        );
        assert_eq!(after.accepted_tasks_by_relais, 0, "{after:?}");
        assert_eq!(
            after.accepted_tasks,
            after.accepted_tasks_by_relais + after.accepted_tasks_by_person,
            "the two routes always add up to the whole: {after:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An answered run is settled, whatever state it is terminal in.
    /// `relais decide --answer revise` moves the run to `cancelled` — a
    /// person's answer ends the run they answered — so a count by STATE
    /// agrees with the open-decision list right below it, which —
    /// correctly — shows nothing once the run is answered.
    #[test]
    fn an_answered_run_is_not_still_awaiting_a_person() {
        let dir = temp_dir("answered-not-awaiting");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = crate::ids::RunId::from_stored("run-waited");
        let task = crate::ids::TaskId::from_stored("task-waited");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsReview,
                reason: "review_findings".into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");

        let early = "2000-01-01T00:00:00+00:00";
        let waiting = runs_report(&ledger, early, None).expect("report");
        assert_eq!(waiting.pending_decisions, 1, "it is owed an answer");
        assert_eq!(waiting.open_decisions.len(), 1);

        ledger
            .resolve_decision(
                &run,
                &crate::ledger::DecisionAnswer {
                    resolution: crate::lifecycle::Reason::DecisionRevised,
                    actor: "fredericrous",
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsReview,
                    to_state: State::Cancelled,
                },
            )
            .expect("answered");

        let answered = runs_report(&ledger, early, None).expect("report");
        assert_eq!(
            answered.pending_decisions, 0,
            "answered, and cancelled, so it is no longer owed a look"
        );
        assert!(answered.open_decisions.is_empty());
        assert_eq!(
            answered.runs[0].status,
            State::Cancelled,
            "`revise` ends the run a person answered, same as reject/decided/abandon"
        );
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A usage event now says which phase spent the money: `relais
    /// report` prints the breakdown beneath the run's own cost line, and
    /// a run whose reviewer cost more than its worker says so — the
    /// review is not folded silently into an undifferentiated total.
    #[test]
    fn a_run_whose_reviewer_cost_more_than_its_worker_says_so() {
        use crate::lifecycle::UsagePhase;

        let dir = temp_dir("phase-breakdown");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = crate::ids::RunId::from_stored("run-phases");
        let task = crate::ids::TaskId::from_stored("task-phases");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        let revision = ledger
            .insert_contract_revision(&run, "h", "{}", "HEAD", None)
            .expect("revision");
        let attempt = ledger
            .insert_attempt(&run, revision, 1, "implementation", UsagePhase::Initial)
            .expect("attempt");
        ledger
            .record_usage(&crate::ledger::UsageEvent {
                event_id: "worker".into(),
                run_id: run.clone(),
                attempt_id: Some(attempt),
                parent_event_id: None,
                model: Some("sonnet".into()),
                input_tokens: Some(1),
                output_tokens: Some(1),
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost: Some(MicroUsd::from_micros(100)),
                cost_kind: crate::money::CostKind::ApiSpend,
                completeness: CostCompleteness::Actual,
                inclusive: false,
                at: now_rfc3339(),
                phase: Some(UsagePhase::Initial),
                duration_ms: Some(500),
                requested_model: Some("sonnet".into()),
                requested_effort: None,
                harness: Some("claude 1.0".into()),
            })
            .expect("worker usage");
        ledger
            .record_usage(&crate::ledger::UsageEvent {
                event_id: "reviewer".into(),
                run_id: run.clone(),
                attempt_id: None,
                parent_event_id: None,
                model: Some("opus".into()),
                input_tokens: Some(1),
                output_tokens: Some(1),
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost: Some(MicroUsd::from_micros(900)),
                cost_kind: crate::money::CostKind::ApiSpend,
                completeness: CostCompleteness::Actual,
                inclusive: false,
                at: now_rfc3339(),
                phase: Some(UsagePhase::Review),
                duration_ms: Some(300),
                requested_model: Some("opus".into()),
                requested_effort: None,
                harness: Some("claude 1.0".into()),
            })
            .expect("reviewer usage");
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

        let breakdown = ledger.run_cost_by_phase(&run).expect("phase costs");
        let worker = breakdown
            .iter()
            .find(|entry| entry.phase == Some(UsagePhase::Initial))
            .expect("worker bucket");
        let reviewer = breakdown
            .iter()
            .find(|entry| entry.phase == Some(UsagePhase::Review))
            .expect("reviewer bucket");
        assert!(
            reviewer.cost > worker.cost,
            "the reviewer spent more than the worker: {reviewer:?} vs {worker:?}"
        );

        let report = runs_report(&ledger, "2000-01-01T00:00:00+00:00", None).expect("report");
        let text = report.render();
        // The FIGURES, and where they sit: a rendering that swapped the
        // two phases, or printed them somewhere other than under the
        // run they belong to, would pass a `contains` on the names.
        let lines: Vec<&str> = text.lines().collect();
        let run_line = lines
            .iter()
            .position(|line| line.starts_with(run.as_str()))
            .expect("the run has a line");
        let review_line = lines
            .iter()
            .position(|line| line.trim_start().starts_with("review"))
            .expect("a review phase line");
        let initial_line = lines
            .iter()
            .position(|line| line.trim_start().starts_with("initial"))
            .expect("an initial phase line");
        assert!(
            review_line > run_line && initial_line > run_line,
            "the breakdown sits beneath its run:\n{text}"
        );
        assert!(
            lines[review_line].contains("$0.0009"),
            "the reviewer's own figure: {}",
            lines[review_line]
        );
        assert!(
            lines[initial_line].contains("$0.0001"),
            "the worker's own figure: {}",
            lines[initial_line]
        );
        assert!(
            lines[review_line].contains("actual") && lines[initial_line].contains("actual"),
            "each figure carries its completeness:\n{text}"
        );
        // Best effort: a leftover temp dir costs nothing but disk.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A run whose attempt started and died before reporting any usage
    /// is the run the `unknown` label exists for. Before this, an empty
    /// fold over its usage answered `Actual`, so the window printed a
    /// clean `$0 (actual)` total for a run the ledger cannot vouch for.
    #[test]
    fn a_window_with_a_killed_run_says_unknown_not_a_clean_total() {
        use crate::lifecycle::UsagePhase;

        let dir = temp_dir("killed-run");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = crate::ids::RunId::from_stored("run-killed");
        let task = crate::ids::TaskId::from_stored("task-killed");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        let revision = ledger
            .insert_contract_revision(&run, "h", "{}", "HEAD", None)
            .expect("revision");
        let attempt = ledger
            .insert_attempt(&run, revision, 1, "implementation", UsagePhase::Initial)
            .expect("attempt");
        ledger
            .record_dispatch_intent(
                &crate::ids::DispatchId::from_stored("disp-killed"),
                &run,
                Some(attempt),
                &serde_json::json!({}),
                0,
            )
            .expect("dispatch intent");
        // The session ends mid-dispatch: no usage event ever arrives.

        let report = runs_report(&ledger, "2000-01-01T00:00:00+00:00", None).expect("report");
        assert_eq!(report.cost_completeness, CostCompleteness::Unknown);
        let text = report.render();
        assert!(
            text.contains("unknown (no usage was reported)"),
            "a killed run must not read as a clean zero:\n{text}"
        );
        // The line the objective is actually about. The per-run line
        // above satisfied the substring assertion while THIS one printed
        // `(actual)` a few lines below it, because the task-level
        // completeness restated the rule instead of calling it.
        assert_eq!(
            report.task_cost_completeness,
            CostCompleteness::Unknown,
            "the primary metric's own label:\n{text}"
        );
        let metric = text
            .lines()
            .find(|line| line.contains("cost per accepted task"))
            .expect("the primary metric line is printed");
        assert!(
            !metric.contains("(actual)"),
            "the primary metric must not call a killed run's total measured: {metric}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A run whose WORKER reported and whose reviewer was then killed is
    /// not complete either. The reviewer and the planner are dispatched
    /// with `attempt_id: None`, so a rule counting attempts reads this
    /// run as fully reported; the rule counts dispatches, which is what
    /// the usage event is keyed by.
    #[test]
    fn a_reviewer_killed_after_the_worker_reported_is_not_a_complete_total() {
        use crate::lifecycle::UsagePhase;
        use crate::money::CostKind;

        let dir = temp_dir("killed-reviewer");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let run = crate::ids::RunId::from_stored("run-rev");
        let task = crate::ids::TaskId::from_stored("task-rev");
        ledger
            .insert_run(&run, "/repo", None, &task, "rk")
            .expect("run");
        let revision = ledger
            .insert_contract_revision(&run, "h", "{}", "HEAD", None)
            .expect("revision");
        let attempt = ledger
            .insert_attempt(&run, revision, 1, "implementation", UsagePhase::Initial)
            .expect("attempt");
        let worker = crate::ids::DispatchId::from_stored("disp-worker");
        ledger
            .record_dispatch_intent(&worker, &run, Some(attempt), &serde_json::json!({}), 0)
            .expect("worker dispatch");
        ledger
            .record_usage(&crate::ledger::UsageEvent {
                event_id: worker.as_str().to_string(),
                run_id: run.clone(),
                attempt_id: Some(attempt),
                parent_event_id: None,
                model: Some("sonnet".into()),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost: Some(MicroUsd::from_micros(1_000)),
                cost_kind: CostKind::ApiSpend,
                completeness: CostCompleteness::Actual,
                inclusive: false,
                phase: Some(UsagePhase::Initial),
                duration_ms: None,
                requested_model: None,
                requested_effort: None,
                harness: None,
                at: crate::ledger::now_rfc3339(),
            })
            .expect("worker usage");
        // The reviewer goes out with no attempt of its own, and the
        // session dies before its usage arrives.
        ledger
            .record_dispatch_intent(
                &crate::ids::DispatchId::from_stored("disp-reviewer"),
                &run,
                None,
                &serde_json::json!({"kind": "review"}),
                0,
            )
            .expect("reviewer dispatch");

        assert_eq!(
            ledger.run_cost_completeness(&run).expect("completeness"),
            CostCompleteness::IncompleteLowerBound,
            "what the worker reported is real; the reviewer's spend is missing"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
