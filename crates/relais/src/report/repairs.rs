//! Repairs by the effort they requested (SPEC §11).
//!
//! A repair is an attempt with phase `repair`. Two facts about it are kept
//! apart on purpose: how ITS OWN verification ended (read off the
//! transition that closes it) and how the RUN it belongs to eventually
//! ended. A repair that passed its checks inside a run that later failed a
//! review is a repair that worked; conflating the two would score it as a
//! failure.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::ids::RunId;
use crate::ledger::{Ledger, LedgerError, Transition};
use crate::lifecycle::{State, UsagePhase};
use crate::money::MicroUsd;

/// Why a repair's own verification has no verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NoVerdict {
    Blocked,
    Crash,
    Cancelled,
    /// The run stopped asking a person; named by the transition's reason.
    Decision(String),
    /// The run stopped before the repair's checks ran (a limit reached
    /// while dispatching, an unapproved substitution); named by the
    /// transition's reason.
    Stopped(String),
    /// A transition no repair can end in, or a repair no transition opened.
    Unrecorded,
    InFlight,
}

impl NoVerdict {
    fn label(&self) -> String {
        match self {
            Self::Blocked => "blocked".to_string(),
            Self::Crash => "crash".to_string(),
            Self::Cancelled => "cancelled".to_string(),
            Self::Decision(reason) | Self::Stopped(reason) => reason.clone(),
            Self::Unrecorded => "unrecorded".to_string(),
            Self::InFlight => "in flight".to_string(),
        }
    }
}

/// How one repair's own verification ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Passed,
    Failed,
    NoVerdict(NoVerdict),
}

/// What a transition from `from` into `to_state` says about the repair it
/// closes, or `None` when the repair is still running or being verified.
/// Exhaustive over [`State`]: a state added later is a decision made here.
fn closing_verdict(from: State, to_state: State, reason: &str) -> Option<Verdict> {
    match to_state {
        State::Running | State::Verifying => None,
        // Another repair, an escalation, or a terminal failure, reached
        // from `verifying`: the checks ran and did not pass. Reached from
        // `running`, the checks never ran — the machine stops a dispatch
        // there on a limit (`budget_exhausted`, runner/machine.rs) or an
        // unapproved substitution (`failed`) — so it is no verdict.
        State::Repairing | State::Escalating | State::Failed | State::BudgetExhausted => {
            Some(if from == State::Verifying {
                Verdict::Failed
            } else {
                Verdict::NoVerdict(NoVerdict::Stopped(reason.to_string()))
            })
        }
        // `verifying -> accepted | needs_review` is emitted only after the
        // attempt's checks passed with no gaps: `accept_candidate` in
        // runner/mod.rs reaches `Observation::ChecksAndReviewPassed`,
        // `ReviewFindings` and `ReviewUnavailable` (the two `needs_review`
        // reasons) once `failures` and `gaps` are empty, and
        // `accept_integrated` in runner/scheduler.rs does the same for an
        // assembled candidate. Review is not verification, so a review
        // that found something or was unavailable still leaves the checks
        // passed. A person's `accepted_by_person` follows a terminal run.
        State::Accepted | State::NeedsReview | State::AcceptedByPerson => Some(Verdict::Passed),
        State::Blocked => Some(Verdict::NoVerdict(NoVerdict::Blocked)),
        State::Interrupted => Some(Verdict::NoVerdict(NoVerdict::Crash)),
        State::Cancelled => Some(Verdict::NoVerdict(NoVerdict::Cancelled)),
        State::NeedsDecision => Some(Verdict::NoVerdict(NoVerdict::Decision(reason.to_string()))),
        // Nothing moves a run back to `prepared`.
        State::Prepared => Some(Verdict::NoVerdict(NoVerdict::Unrecorded)),
    }
}

/// One repair attempt of a run: its requested effort, how its own
/// verification ended, and what it cost when any usage was reported.
struct Repair {
    effort: String,
    verdict: Verdict,
    cost: Option<MicroUsd>,
}

/// A run's repairs, paired with the transitions that opened and closed
/// them. `transitions.attempt_id` is NULL in practice, so it cannot be
/// joined: the k-th transition into `repairing` opens the run's k-th repair
/// attempt (by `attempt_index`), and that repair's verification ends at the
/// first later transition that leaves `running`/`verifying`.
fn repairs_of_run(ledger: &Ledger, run_id: &RunId) -> Result<Vec<Repair>, LedgerError> {
    let transitions = ledger.transitions(run_id)?;
    let openings: Vec<usize> = transitions
        .iter()
        .enumerate()
        .filter(|(_, transition)| transition.to_state == State::Repairing)
        .map(|(position, _)| position)
        .collect();
    let repair_attempts = ledger
        .worker_attempts(run_id)?
        .into_iter()
        .filter(|attempt| attempt.phase == UsagePhase::Repair);
    let mut repairs = Vec::new();
    for (k, attempt) in repair_attempts.enumerate() {
        let verdict = match openings.get(k) {
            Some(&opened) => closing_of(&transitions[opened..]),
            None => Verdict::NoVerdict(NoVerdict::Unrecorded),
        };
        repairs.push(Repair {
            effort: ledger.requested_effort(attempt.id)?,
            verdict,
            cost: ledger.attempt_cost(attempt.id)?,
        });
    }
    Ok(repairs)
}

/// The verdict of the first transition after `from_opening[0]` (the one
/// that opened the repair) that closes it; a repair with none yet is in
/// flight. Each transition is judged with the state it left: its recorded
/// `from_state`, else the state the previous transition entered.
fn closing_of(from_opening: &[Transition]) -> Verdict {
    from_opening
        .windows(2)
        .find_map(|pair| {
            let from = pair[1].from_state.unwrap_or(pair[0].to_state);
            closing_verdict(from, pair[1].to_state, &pair[1].reason)
        })
        .unwrap_or(Verdict::NoVerdict(NoVerdict::InFlight))
}

/// What a requested effort's repairs cost. An attempt with no reported
/// usage is counted in `unknown` and left out of `mean`, `min` and `max`:
/// it cost something, and `$0.00` would say it did not.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepairCost {
    pub with_usage: usize,
    pub unknown: usize,
    pub mean: Option<MicroUsd>,
    pub min: Option<MicroUsd>,
    pub max: Option<MicroUsd>,
}

/// The repairs that requested one effort.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EffortRepairs {
    pub effort: String,
    pub repairs: usize,
    /// The repairs' own checks passed (review is not verification).
    pub verification_passed: usize,
    pub verification_failed: usize,
    pub no_verdict: usize,
    /// `no_verdict` by kind: blocked, crash, cancelled, a decision's
    /// reason, in flight, unrecorded.
    pub no_verdict_kinds: BTreeMap<String, usize>,
    pub cost: RepairCost,
    /// The eventual status of the runs these repairs belong to, each run
    /// counted once however many of its repairs requested this effort.
    pub run_statuses: BTreeMap<State, usize>,
}

/// What one effort's repairs add up to while the window is walked.
#[derive(Default)]
struct Tally {
    repairs: usize,
    passed: usize,
    failed: usize,
    no_verdict_kinds: BTreeMap<String, usize>,
    costs: Vec<MicroUsd>,
    unknown_cost: usize,
    runs: BTreeMap<String, State>,
}

impl Tally {
    fn add(&mut self, repair: &Repair, run: &RunId, status: State) {
        self.repairs += 1;
        match &repair.verdict {
            Verdict::Passed => self.passed += 1,
            Verdict::Failed => self.failed += 1,
            Verdict::NoVerdict(kind) => {
                *self.no_verdict_kinds.entry(kind.label()).or_default() += 1;
            }
        }
        match repair.cost {
            Some(cost) => self.costs.push(cost),
            None => self.unknown_cost += 1,
        }
        self.runs.insert(run.as_str().to_string(), status);
    }

    fn finish(self, effort: String) -> EffortRepairs {
        let total = self
            .costs
            .iter()
            .fold(MicroUsd::ZERO, |sum, cost| sum.saturating_add(*cost));
        let mean = (!self.costs.is_empty()).then(|| {
            MicroUsd::from_micros(
                (total.to_micros() as f64 / self.costs.len() as f64).round() as i64
            )
        });
        let mut run_statuses: BTreeMap<State, usize> = BTreeMap::new();
        for status in self.runs.values() {
            *run_statuses.entry(*status).or_default() += 1;
        }
        EffortRepairs {
            effort,
            repairs: self.repairs,
            verification_passed: self.passed,
            verification_failed: self.failed,
            no_verdict: self.no_verdict_kinds.values().sum(),
            no_verdict_kinds: self.no_verdict_kinds,
            cost: RepairCost {
                with_usage: self.costs.len(),
                unknown: self.unknown_cost,
                mean,
                min: self.costs.iter().min().copied(),
                max: self.costs.iter().max().copied(),
            },
            run_statuses,
        }
    }
}

/// Every repair of the window's runs and of the package runs beneath
/// them, grouped by the effort it requested (read through the ledger's one
/// reader), ordered by effort label.
///
/// The window lists root runs only, but a decomposed run repairs inside
/// its package runs: each is walked, and each repair's run is the package
/// run it happened in, counted with that run's own status.
pub fn repair_outcomes(
    ledger: &Ledger,
    window: &[(RunId, State)],
) -> Result<Vec<EffortRepairs>, LedgerError> {
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut pending: Vec<(RunId, State)> = window.to_vec();
    while let Some((run_id, status)) = pending.pop() {
        for repair in repairs_of_run(ledger, &run_id)? {
            tallies
                .entry(repair.effort.clone())
                .or_default()
                .add(&repair, &run_id, status);
        }
        for child in ledger.child_runs(&run_id)? {
            pending.push((child.run, child.status));
        }
    }
    Ok(tallies
        .into_iter()
        .map(|(effort, tally)| tally.finish(effort))
        .collect())
}

/// A state as a person reads it in a count: `needs review`, `accepted by a
/// person`.
fn state_words(state: State) -> String {
    if state == State::AcceptedByPerson {
        "accepted by a person".to_string()
    } else {
        state.as_str().replace('_', " ")
    }
}

impl EffortRepairs {
    /// The effort's lines: the repairs' own verification, what they cost,
    /// and — on a line of its own — how their runs ended.
    pub fn render(&self) -> String {
        let mut out = format!(
            "{}: {} repair{} — verification passed {}, failed {}, no verdict {}",
            self.effort,
            self.repairs,
            if self.repairs == 1 { "" } else { "s" },
            self.verification_passed,
            self.verification_failed,
            self.no_verdict,
        );
        if !self.no_verdict_kinds.is_empty() {
            let kinds: Vec<String> = self
                .no_verdict_kinds
                .iter()
                .map(|(kind, count)| format!("{kind} {count}"))
                .collect();
            out.push_str(&format!(" ({})", kinds.join(", ")));
        }
        out.push('\n');
        let cost = &self.cost;
        match (cost.mean, cost.min, cost.max) {
            (Some(mean), Some(min), Some(max)) => out.push_str(&format!(
                "  cost mean {mean}, range {min}–{max} ({} with usage, {} unknown)\n",
                cost.with_usage, cost.unknown
            )),
            _ => out.push_str(&format!(
                "  cost unknown (no usage was reported for {} repair{})\n",
                cost.unknown,
                if cost.unknown == 1 { "" } else { "s" }
            )),
        }
        let statuses: Vec<String> = self
            .run_statuses
            .iter()
            .map(|(state, count)| format!("{} {count}", state_words(*state)))
            .collect();
        out.push_str(&format!("  their runs: {}\n", statuses.join(", ")));
        out
    }
}
