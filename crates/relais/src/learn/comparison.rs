//! Recipe comparison (SPEC §25): a paired interval over tasks, labelled
//! with the basis it rests on.
//!
//! `evaluate_candidate` reads the settled trials whose arm ran under one
//! of a candidate policy's own recipes, pairs each task's arm against the
//! incumbent result the task's source run already earned, and reports —
//! never promotes. A task replayed twice still contributes one paired
//! observation, because the unit under comparison is the task, not the
//! run: a task with more runs must not weigh more than one with a single
//! run.
//!
//! Promotion is gated: [`ComparisonReport::promotable`] hands back a
//! [`Promotable`] only when every gate in [`ComparisonReport::failures`]
//! holds, and that value has no public constructor and no field a caller
//! could set to claim promotability it did not earn. An
//! [`ComparisonBasis::Observational`] comparison never promotes, whatever
//! the numbers say — it is not a comparison. Off-policy acceptance
//! estimates abstain, rather than guess, when an arm has no support.
//!
//! Nothing in this module promotes a recipe, writes a policy, or issues a
//! trust grant. It reads settled trials and reports what they show.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ids::TaskId;
use crate::ledger::{Ledger, LedgerError, TrialCost, TrialOutcome, TrialRow, LIVE_WORKTREE};
use crate::lifecycle::State;
use crate::money::MicroUsd;
use crate::policy::{RecipeSpec, RepoPolicy};
use crate::rng::SplitMix64;

/// Below this many paired tasks, a comparison is a proof of mechanism,
/// not evidence about a recipe (SPEC §25).
pub const MIN_PAIRED_TASKS: usize = 20;

/// How many bootstrap resamples back the paired interval by default.
pub const DEFAULT_BOOTSTRAP_RESAMPLES: usize = 2_000;

/// What a comparison rests on. An observational basis can never promote,
/// whatever the numbers say — it is not a comparison (SPEC §17, §25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonBasis {
    /// Each task's arm was drawn by relais's own random assignment, its
    /// probability recorded at the moment of the draw. Not produced by
    /// anything in this crate yet.
    Randomized,
    /// Each task's arm is `relais dataset replay`'s one deliberate
    /// re-run: assigned with certainty, never sampled among several arms.
    Replay,
    /// Data that was never assigned at all — whatever produced it chose
    /// the arm on its own, with no probability to reason about.
    Observational,
}

impl std::fmt::Display for ComparisonBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Randomized => "randomized",
            Self::Replay => "replay",
            Self::Observational => "observational",
        })
    }
}

/// Why an off-policy estimate abstains rather than guessing (SPEC §17):
/// "do not infer performance for profiles with zero observation
/// probability".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstentionReason {
    /// Nothing settled for this arm at all.
    NoTrials,
    /// Every observation's assignment probability is zero or non-finite:
    /// nothing can be divided by it without inventing an estimate.
    AssignmentProbabilityUnusable,
}

impl std::fmt::Display for AbstentionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoTrials => "no trials support this arm",
            Self::AssignmentProbabilityUnusable => {
                "every trial's assignment probability cannot carry an estimate"
            }
        })
    }
}

/// An acceptance rate, or an explicit refusal to guess one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceEstimate {
    Estimated(f64),
    Abstained(AbstentionReason),
}

/// One arm's observation for one task: whether it was accepted, and what
/// it cost. `cost: None` means the ledger never settled a figure at all;
/// `Some(TrialCost)` may itself carry `Unknown` (SPEC §11: unknown stays
/// unknown, never zero — [`TrialCost`] is what keeps the figure and its
/// completeness from disagreeing).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArmObservation {
    pub accepted: bool,
    pub cost: Option<TrialCost>,
}

/// One task, observed under both arms — one paired observation, not two.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairedTask {
    pub task_id: TaskId,
    pub incumbent: ArmObservation,
    pub arm: ArmObservation,
}

/// Mean cost among the accepted observations of one arm. Unknown cost
/// stays unknown: it is counted, never folded into the mean as zero, and
/// the mean itself is `None` rather than a number when nothing accepted
/// carried a known figure (SPEC §11, §25).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostPerAcceptance {
    pub known_mean: Option<MicroUsd>,
    pub accepted_with_known_cost: usize,
    pub accepted_with_unknown_cost: usize,
}

/// One arm's summary: an off-policy acceptance estimate (or an
/// abstention) and its cost per accepted change.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArmSummary {
    pub acceptance: AcceptanceEstimate,
    pub cost_per_acceptance: CostPerAcceptance,
}

/// The paired bootstrap interval over the candidate-minus-incumbent
/// acceptance difference, one task, one observation. Seeded from a value
/// carried in the report, so the same trials give the same interval on
/// any machine (SPEC §25).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PairedInterval {
    pub point: f64,
    pub low: f64,
    pub high: f64,
    pub seed: u64,
    pub resamples: usize,
}

/// Which arm a gate failure is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhichArm {
    Incumbent,
    Arm,
}

impl std::fmt::Display for WhichArm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Incumbent => "incumbent",
            Self::Arm => "candidate",
        })
    }
}

/// One promotion gate that does not hold. Named in words, not spelled as
/// a boolean a reader can misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateFailure {
    /// The comparison does not rest on an assignment at all.
    NonComparableBasis(ComparisonBasis),
    InsufficientPairedTasks {
        observed: usize,
        minimum: usize,
    },
    NoSupport {
        arm: WhichArm,
        reason: AbstentionReason,
    },
}

impl std::fmt::Display for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonComparableBasis(basis) => write!(
                f,
                "the comparison basis is {basis}: observational data was never assigned, so it is \
                 not a comparison and cannot promote whatever the numbers say"
            ),
            Self::InsufficientPairedTasks { observed, minimum } => write!(
                f,
                "{observed} paired task(s) is below the {minimum} required for promotion"
            ),
            Self::NoSupport { arm, reason } => write!(f, "the {arm} arm has no support: {reason}"),
        }
    }
}

/// A comparison's report: what it rests on, what it paired, and what
/// each arm shows. `failures` is always RECOMPUTED from these fields —
/// nothing here stores a verdict a caller could edit out of step with
/// the numbers beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonReport {
    pub candidate_recipe_ids: BTreeSet<String>,
    pub basis: ComparisonBasis,
    pub paired_tasks: usize,
    pub min_paired_tasks: usize,
    /// Settled trials for this candidate whose outcome was
    /// [`TrialOutcome::Errored`] — "not comparable evidence for or against
    /// the arm" by that variant's own doc, so they never enter a pair. Set
    /// aside rather than dropped: this count is how they stay visible.
    pub errored_trials_set_aside: usize,
    /// Settled live (`live_worktree`) trials for this candidate, set aside
    /// whole: this estimator pairs each trial with a replay SOURCE run,
    /// which a live trial does not have, so pairing one would compare the
    /// arm with nothing. They stay counted, not dropped, until an unpaired
    /// estimator exists. Additive: a report written before this field
    /// existed reads back as zero.
    #[serde(default)]
    pub live_trials_set_aside: usize,
    pub incumbent: ArmSummary,
    pub arm: ArmSummary,
    pub interval: PairedInterval,
}

impl ComparisonReport {
    /// Every gate that does not hold, recomputed from the numbers beside
    /// it. Empty means promotable.
    pub fn failures(&self) -> Vec<GateFailure> {
        let mut failures = Vec::new();
        if self.basis == ComparisonBasis::Observational {
            failures.push(GateFailure::NonComparableBasis(self.basis));
        }
        if self.paired_tasks < self.min_paired_tasks {
            failures.push(GateFailure::InsufficientPairedTasks {
                observed: self.paired_tasks,
                minimum: self.min_paired_tasks,
            });
        }
        if let AcceptanceEstimate::Abstained(reason) = self.incumbent.acceptance {
            failures.push(GateFailure::NoSupport {
                arm: WhichArm::Incumbent,
                reason,
            });
        }
        if let AcceptanceEstimate::Abstained(reason) = self.arm.acceptance {
            failures.push(GateFailure::NoSupport {
                arm: WhichArm::Arm,
                reason,
            });
        }
        failures
    }

    /// The only way to obtain a [`Promotable`]: `Some` exactly when
    /// [`Self::failures`] is empty.
    pub fn promotable(&self) -> Option<Promotable> {
        self.failures().is_empty().then(sealed::Promotable::mint)
    }

    /// What the comparison rests on and what each arm showed — basis, n,
    /// per-arm acceptance and cost, the paired interval — with no verdict
    /// and no statement about what the caller will do with it.
    pub fn render_evidence(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("basis: {}, n={}\n", self.basis, self.paired_tasks));
        out.push_str(&format!(
            "errored trials set aside (not comparable evidence for or against the arm): {}\n",
            self.errored_trials_set_aside
        ));
        out.push_str(&format!(
            "randomized trials set aside: {} (unpaired estimator not implemented yet)\n",
            self.live_trials_set_aside
        ));
        out.push_str(&format!("incumbent: {}\n", render_arm(&self.incumbent)));
        out.push_str(&format!("candidate arm: {}\n", render_arm(&self.arm)));
        out.push_str(&format!(
            "paired acceptance difference (candidate - incumbent) over {} task(s): {:.3} \
             [{:.3}, {:.3}] ({} bootstrap resample(s), seed {})\n",
            self.paired_tasks,
            self.interval.point,
            self.interval.low,
            self.interval.high,
            self.interval.resamples,
            self.interval.seed,
        ));
        out
    }

    pub fn render(&self) -> String {
        let mut out = String::from("relais recipe evaluate\n");
        out.push_str(
            "this evaluation reads and reports; it promotes nothing, writes no policy, and \
             issues no grant\n",
        );
        out.push_str(&self.render_evidence());
        let failures = self.failures();
        if failures.is_empty() {
            out.push_str("gates: PASSED\n");
        } else {
            out.push_str("gates: FAILED — promotion is refused\n");
            for failure in failures {
                out.push_str(&format!("  gate: {failure}\n"));
            }
        }
        out
    }
}

fn render_arm(summary: &ArmSummary) -> String {
    let acceptance = match summary.acceptance {
        AcceptanceEstimate::Estimated(rate) => format!("acceptance {rate:.2}"),
        AcceptanceEstimate::Abstained(reason) => format!("acceptance abstained ({reason})"),
    };
    let cost = match summary.cost_per_acceptance.known_mean {
        Some(mean) => format!(
            "cost per accepted change {mean} ({} known, {} unknown)",
            summary.cost_per_acceptance.accepted_with_known_cost,
            summary.cost_per_acceptance.accepted_with_unknown_cost
        ),
        None => format!(
            "cost per accepted change unknown ({} known, {} unknown)",
            summary.cost_per_acceptance.accepted_with_known_cost,
            summary.cost_per_acceptance.accepted_with_unknown_cost
        ),
    };
    format!("{acceptance}; {cost}")
}

/// A value that says a candidate has cleared every promotion gate.
///
/// Private field, no public constructor: [`ComparisonReport::promotable`]
/// is the ONLY way to obtain one, and only when its own
/// [`ComparisonReport::failures`] is empty. No `Default`, no
/// `Deserialize`, no field a caller could set to claim promotability a
/// report did not earn. Nothing reads this value to promote, write a
/// policy or issue a grant — it exists so the type system, not a
/// boolean, is what a caller has to satisfy before claiming a candidate
/// cleared every gate.
mod sealed {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Promotable {
        _private: (),
    }

    impl Promotable {
        pub(super) fn mint() -> Self {
            Self { _private: () }
        }
    }
}
pub use sealed::Promotable;

/// Read the settled trials for `candidate` and build its comparison
/// report (SPEC §25). "The trials for this candidate" means every
/// settled arm whose `arm_recipe_id` names one of the candidate's own
/// recipes — a candidate is a whole policy, and different tasks may
/// cover under different recipes within it.
pub fn evaluate_candidate(
    ledger: &Ledger,
    candidate: &RepoPolicy,
    min_paired_tasks: usize,
    resamples: usize,
) -> Result<ComparisonReport, LedgerError> {
    let recipe_ids: BTreeSet<String> = candidate
        .recipes
        .iter()
        .map(RecipeSpec::recipe_id)
        .collect();
    let (trials, live_trials_set_aside) =
        set_aside_live_trials(ledger.settled_trials_for_recipes(&recipe_ids)?);
    let (by_task, errored_trials_set_aside) = earliest_non_errored_per_task(trials);

    let basis = comparison_basis(by_task.values());

    let mut pairs = Vec::with_capacity(by_task.len());
    let mut arm_observations: Vec<(f64, ArmObservation)> = Vec::with_capacity(by_task.len());
    let mut incumbent_observations: Vec<(f64, ArmObservation)> = Vec::with_capacity(by_task.len());
    for (task_id, trial) in &by_task {
        let arm_outcome = trial
            .outcome
            .expect("settled_trials_for_recipes only returns settled trials");
        let arm = ArmObservation {
            accepted: arm_outcome == TrialOutcome::Accepted,
            cost: trial.cost,
        };
        let incumbent_accepted = ledger.run_status(&trial.source_run_id)? == Some(State::Accepted);
        let incumbent = ArmObservation {
            accepted: incumbent_accepted,
            cost: Some(trial_cost_from_run(ledger, &trial.source_run_id)?),
        };
        arm_observations.push((trial.assignment_probability, arm));
        // The incumbent is what already shipped for this task, not a
        // draw: its assignment probability is 1.0 by construction.
        incumbent_observations.push((1.0, incumbent));
        pairs.push(PairedTask {
            task_id: task_id.clone(),
            incumbent,
            arm,
        });
    }

    let incumbent_summary = arm_summary(&incumbent_observations);
    let arm_summary_value = arm_summary(&arm_observations);

    let seed = derive_seed(
        &pairs
            .iter()
            .map(|pair| pair.task_id.clone())
            .collect::<Vec<_>>(),
    );
    let interval = bootstrap_paired_interval(&pairs, seed, resamples);

    Ok(ComparisonReport {
        candidate_recipe_ids: recipe_ids,
        basis,
        paired_tasks: pairs.len(),
        min_paired_tasks,
        errored_trials_set_aside,
        live_trials_set_aside,
        incumbent: incumbent_summary,
        arm: arm_summary_value,
        interval,
    })
}

/// The cost the ledger settled for a run, paired with its completeness —
/// `None` only when the run's own completeness is `Unknown` (SPEC §11):
/// a figure of zero recorded against an unknown completeness would read
/// as free.
fn trial_cost_from_run(
    ledger: &Ledger,
    run_id: &crate::ids::RunId,
) -> Result<TrialCost, LedgerError> {
    let completeness = ledger.run_cost_completeness(run_id)?;
    let cost = match completeness {
        crate::money::CostCompleteness::Unknown => None,
        _ => Some(ledger.run_cost(run_id)?),
    };
    Ok(TrialCost::new(cost, completeness)
        .expect("a cost/completeness pair read back from the ledger's own settled values is always a consistent one"))
}

/// The replay trials, and how many live ones were set aside. A live trial
/// (SPEC §28) ran in the repository's own worktree and has no replay
/// source run to be paired with; until an unpaired estimator exists it is
/// excluded from pairing and from [`comparison_basis`] alike, so a ledger
/// holding both evaluates exactly as it would holding the replays alone.
fn set_aside_live_trials(trials: Vec<TrialRow>) -> (Vec<TrialRow>, usize) {
    let total = trials.len();
    let replays: Vec<TrialRow> = trials
        .into_iter()
        .filter(|trial| trial.workspace_isolation != LIVE_WORKTREE)
        .collect();
    let set_aside = total - replays.len();
    (replays, set_aside)
}

/// One trial per task, the earliest settled NON-errored trial, plus how
/// many errored trials were set aside along the way. An errored trial
/// ended on an infrastructure fault, not a verification verdict —
/// [`TrialOutcome::Errored`]'s own doc says it is "not comparable evidence
/// for or against the arm" — so it never becomes a task's paired
/// observation: a task whose earliest settled trial errored pairs on its
/// next, non-errored one instead, and a task with nothing else contributes
/// no pair at all. Set aside rather than dropped: the count this returns
/// is how they stay visible in the rendered report.
fn earliest_non_errored_per_task(trials: Vec<TrialRow>) -> (BTreeMap<TaskId, TrialRow>, usize) {
    let mut by_task: BTreeMap<TaskId, TrialRow> = BTreeMap::new();
    let mut errored_set_aside = 0usize;
    for trial in trials {
        let outcome = trial
            .outcome
            .expect("settled_trials_for_recipes only returns settled trials");
        if outcome == TrialOutcome::Errored {
            errored_set_aside += 1;
            continue;
        }
        by_task.entry(trial.task_id.clone()).or_insert(trial);
    }
    (by_task, errored_set_aside)
}

/// What the matched trials rest on. Every trial `dataset replay` writes
/// is assigned with certainty under a fresh, no-accepted-answer
/// workspace; anything else — a probability that was actually drawn, or
/// no trials at all — is not that shape. With no trials matched, the
/// only producer this crate ships is replay, so the default names that
/// rather than a basis nothing here ever produces; [`ComparisonReport::failures`]
/// refuses on paired-task count in that case regardless.
fn comparison_basis<'a>(trials: impl Iterator<Item = &'a TrialRow>) -> ComparisonBasis {
    let mut saw_any = false;
    let mut all_replay = true;
    for trial in trials {
        saw_any = true;
        if trial.workspace_isolation != "fresh_checkout_no_accepted_answer"
            || trial.assignment_probability != 1.0
        {
            all_replay = false;
        }
    }
    if saw_any && !all_replay {
        ComparisonBasis::Randomized
    } else {
        ComparisonBasis::Replay
    }
}

/// Inverse-propensity-weighted acceptance over observations carrying
/// their own assignment probability. Abstains rather than guessing when
/// there is nothing to weight, or nothing an assignment probability can
/// carry an estimate over (SPEC §17).
fn ipw_acceptance(observations: &[(f64, ArmObservation)]) -> AcceptanceEstimate {
    if observations.is_empty() {
        return AcceptanceEstimate::Abstained(AbstentionReason::NoTrials);
    }
    if observations
        .iter()
        .any(|(probability, _)| !probability.is_finite() || *probability <= 0.0)
    {
        return AcceptanceEstimate::Abstained(AbstentionReason::AssignmentProbabilityUnusable);
    }
    let mut weight_sum = 0.0;
    let mut accepted_weight = 0.0;
    for (probability, observation) in observations {
        let weight = 1.0 / probability;
        weight_sum += weight;
        if observation.accepted {
            accepted_weight += weight;
        }
    }
    AcceptanceEstimate::Estimated(accepted_weight / weight_sum)
}

fn cost_per_acceptance(observations: &[(f64, ArmObservation)]) -> CostPerAcceptance {
    let mut known: Vec<i64> = Vec::new();
    let mut unknown_count = 0usize;
    for (_, observation) in observations.iter().filter(|(_, obs)| obs.accepted) {
        match observation.cost.and_then(TrialCost::cost) {
            Some(cost) => known.push(cost.to_micros()),
            None => unknown_count += 1,
        }
    }
    let known_mean = (!known.is_empty())
        .then(|| MicroUsd::from_micros(known.iter().sum::<i64>() / known.len() as i64));
    CostPerAcceptance {
        known_mean,
        accepted_with_known_cost: known.len(),
        accepted_with_unknown_cost: unknown_count,
    }
}

fn arm_summary(observations: &[(f64, ArmObservation)]) -> ArmSummary {
    ArmSummary {
        acceptance: ipw_acceptance(observations),
        cost_per_acceptance: cost_per_acceptance(observations),
    }
}

/// A deterministic seed from the paired tasks alone: the same set of
/// tasks, in any order, hands back the same seed on any machine — never
/// the platform's own randomness, and never iteration order.
fn derive_seed(task_ids: &[TaskId]) -> u64 {
    let mut ids: Vec<&str> = task_ids.iter().map(TaskId::as_str).collect();
    ids.sort_unstable();
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for id in ids {
        for byte in id.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        // A separator between ids so `["ab", "c"]` and `["a", "bc"]`
        // never collide.
        hash ^= 0xff;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

/// The paired bootstrap over the candidate-minus-incumbent acceptance
/// difference, one resample redrawing whole tasks with replacement — a
/// task's two arms are resampled together, never split, because the pair
/// is the unit of evidence.
fn bootstrap_paired_interval(pairs: &[PairedTask], seed: u64, resamples: usize) -> PairedInterval {
    let diffs: Vec<f64> = pairs
        .iter()
        .map(|pair| (pair.arm.accepted as u8 as f64) - (pair.incumbent.accepted as u8 as f64))
        .collect();
    let point = mean(&diffs);
    if diffs.is_empty() || resamples == 0 {
        return PairedInterval {
            point,
            low: point,
            high: point,
            seed,
            resamples,
        };
    }
    let mut rng = SplitMix64::new(seed);
    let mut means: Vec<f64> = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let mut sum = 0.0;
        for _ in 0..diffs.len() {
            let index = rng.below(diffs.len() as u64) as usize;
            sum += diffs[index];
        }
        means.push(sum / diffs.len() as f64);
    }
    means.sort_by(|a, b| {
        a.partial_cmp(b)
            .expect("bootstrap means of finite diffs are finite")
    });
    let low_index = (means.len() as f64 * 0.025) as usize;
    let high_index = ((means.len() as f64 * 0.975) as usize).min(means.len() - 1);
    PairedInterval {
        point,
        low: means[low_index],
        high: means[high_index],
        seed,
        resamples,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{RunId, TrialId};

    /// A settled trial row for one task, with only the fields the pairing
    /// logic reads left variable.
    fn settled_trial(id: &str, task_id: &str, created_at: &str, outcome: TrialOutcome) -> TrialRow {
        TrialRow {
            trial_id: TrialId::from_stored(id),
            task_id: TaskId::from_stored(task_id),
            source_run_id: RunId::from_stored(format!("run-{id}")),
            incumbent_recipe_id: "recipe-incumbent".into(),
            arm_recipe_id: "recipe-arm".into(),
            arm_index: 0,
            assignment_probability: 1.0,
            seed: 0,
            base_sha: "base-sha".into(),
            contract_hash: "contract-hash".into(),
            verification_profile_hash: "profile-hash".into(),
            workspace_isolation: "fresh_checkout_no_accepted_answer".into(),
            outcome: Some(outcome),
            accepted_without_escalation: Some(false),
            cost: Some(TrialCost::UNKNOWN),
            duration_ms: Some(1),
            created_at: created_at.into(),
        }
    }

    /// L (a): a task whose earliest settled trial errored, followed by a
    /// real one, pairs on the real one — never on the errored trial, and
    /// never as a task with no pair at all.
    ///
    /// FALSIFY: pick the earliest settled trial of any outcome instead of
    /// the earliest non-errored one, and this test fails — the errored
    /// trial, being earliest, would win the task's slot and its `Errored`
    /// outcome would carry no pairable acceptance. Confirmed, then
    /// restored.
    #[test]
    fn a_task_with_an_errored_trial_then_an_accepted_one_pairs_on_the_accepted_trial() {
        let trials = vec![
            settled_trial(
                "t1",
                "task-a",
                "2020-01-01T00:00:00Z",
                TrialOutcome::Errored,
            ),
            settled_trial(
                "t2",
                "task-a",
                "2020-01-02T00:00:00Z",
                TrialOutcome::Accepted,
            ),
        ];
        let (by_task, errored_set_aside) = earliest_non_errored_per_task(trials);
        assert_eq!(errored_set_aside, 1);
        let trial = by_task
            .get(&TaskId::from_stored("task-a"))
            .expect("the task has a pair");
        assert_eq!(trial.trial_id, TrialId::from_stored("t2"));
        assert_eq!(trial.outcome, Some(TrialOutcome::Accepted));
    }

    /// L (b): a task with only errored trials contributes no pair at all,
    /// and every one of its errored trials is still counted as set aside.
    ///
    /// FALSIFY: fall back to the earliest trial of any outcome when none
    /// is non-errored, and this test fails — `task-b` would wrongly gain a
    /// pair carrying an `Errored` outcome. Confirmed, then restored.
    #[test]
    fn a_task_with_only_errored_trials_contributes_no_pair() {
        let trials = vec![
            settled_trial(
                "t1",
                "task-b",
                "2020-01-01T00:00:00Z",
                TrialOutcome::Errored,
            ),
            settled_trial(
                "t2",
                "task-b",
                "2020-01-02T00:00:00Z",
                TrialOutcome::Errored,
            ),
        ];
        let (by_task, errored_set_aside) = earliest_non_errored_per_task(trials);
        assert_eq!(errored_set_aside, 2);
        assert!(!by_task.contains_key(&TaskId::from_stored("task-b")));
    }

    /// L (c): the set-aside count appears in the rendered report,
    /// additively — the errored trials never vanish from view.
    #[test]
    fn the_rendered_report_names_the_errored_trials_set_aside() {
        let report = ComparisonReport {
            errored_trials_set_aside: 3,
            live_trials_set_aside: 0,
            ..passing_report()
        };
        let rendered = report.render();
        // The exact line, count included: a bare `contains('3')` also
        // matched digits elsewhere in the report. FALSIFIED: rendering a
        // constant 0 in place of the count turned this red; restored.
        assert!(
            rendered.contains(
                "errored trials set aside (not comparable evidence for or against the arm): 3\n"
            ),
            "{rendered}"
        );
    }

    fn passing_report() -> ComparisonReport {
        ComparisonReport {
            candidate_recipe_ids: BTreeSet::new(),
            basis: ComparisonBasis::Replay,
            paired_tasks: 25,
            min_paired_tasks: MIN_PAIRED_TASKS,
            errored_trials_set_aside: 0,
            live_trials_set_aside: 0,
            incumbent: ArmSummary {
                acceptance: AcceptanceEstimate::Estimated(0.8),
                cost_per_acceptance: CostPerAcceptance {
                    known_mean: Some(MicroUsd::from_micros(1_000_000)),
                    accepted_with_known_cost: 20,
                    accepted_with_unknown_cost: 0,
                },
            },
            arm: ArmSummary {
                acceptance: AcceptanceEstimate::Estimated(0.9),
                cost_per_acceptance: CostPerAcceptance {
                    known_mean: Some(MicroUsd::from_micros(800_000)),
                    accepted_with_known_cost: 22,
                    accepted_with_unknown_cost: 0,
                },
            },
            interval: PairedInterval {
                point: 0.1,
                low: 0.02,
                high: 0.18,
                seed: 42,
                resamples: 2_000,
            },
        }
    }

    /// L: `failures` recomputes from the stored figures rather than
    /// trusting a stored verdict — editing a figure after the fact
    /// changes the recomputed verdict.
    #[test]
    fn failures_are_recomputed_from_stored_figures_not_a_stored_verdict() {
        let report = passing_report();
        assert!(report.failures().is_empty(), "{:?}", report.failures());
        assert!(report.promotable().is_some());

        let edited = ComparisonReport {
            paired_tasks: 3,
            ..report
        };
        assert!(
            edited
                .failures()
                .iter()
                .any(|failure| matches!(failure, GateFailure::InsufficientPairedTasks { .. })),
            "{:?}",
            edited.failures()
        );
        assert!(edited.promotable().is_none());
    }

    /// An observational basis never promotes, whatever the numbers say —
    /// even the otherwise-passing fixture above.
    #[test]
    fn an_observational_basis_never_promotes() {
        let report = ComparisonReport {
            basis: ComparisonBasis::Observational,
            ..passing_report()
        };
        assert!(matches!(
            report.failures().as_slice(),
            [GateFailure::NonComparableBasis(
                ComparisonBasis::Observational
            )]
        ));
        assert!(report.promotable().is_none());
    }

    /// The realistic first use: three replayed tasks. This must refuse to
    /// promote, and the refusal must be unmistakable — no sentence in the
    /// rendered output could be quoted as approval.
    #[test]
    fn a_three_task_evaluation_refuses_to_promote() {
        let mut pairs = Vec::new();
        for n in 0..3u32 {
            pairs.push(PairedTask {
                task_id: TaskId::from_stored(format!("task-{n}")),
                incumbent: ArmObservation {
                    accepted: true,
                    cost: Some(
                        TrialCost::new(
                            Some(MicroUsd::from_micros(1_000_000)),
                            crate::money::CostCompleteness::Actual,
                        )
                        .unwrap(),
                    ),
                },
                arm: ArmObservation {
                    accepted: true,
                    cost: Some(
                        TrialCost::new(
                            Some(MicroUsd::from_micros(900_000)),
                            crate::money::CostCompleteness::Actual,
                        )
                        .unwrap(),
                    ),
                },
            });
        }
        let observations: Vec<(f64, ArmObservation)> =
            pairs.iter().map(|pair| (1.0, pair.arm)).collect();
        let incumbent_observations: Vec<(f64, ArmObservation)> =
            pairs.iter().map(|pair| (1.0, pair.incumbent)).collect();
        let seed = derive_seed(
            &pairs
                .iter()
                .map(|pair| pair.task_id.clone())
                .collect::<Vec<_>>(),
        );
        let report = ComparisonReport {
            candidate_recipe_ids: BTreeSet::new(),
            basis: ComparisonBasis::Replay,
            paired_tasks: pairs.len(),
            min_paired_tasks: MIN_PAIRED_TASKS,
            errored_trials_set_aside: 0,
            live_trials_set_aside: 0,
            incumbent: arm_summary(&incumbent_observations),
            arm: arm_summary(&observations),
            interval: bootstrap_paired_interval(&pairs, seed, DEFAULT_BOOTSTRAP_RESAMPLES),
        };
        assert!(report.promotable().is_none());
        assert!(report
            .failures()
            .iter()
            .any(|failure| matches!(failure, GateFailure::InsufficientPairedTasks { .. })));
        let rendered = report.render();
        assert!(rendered.contains("basis: replay, n=3"), "{rendered}");
        assert!(rendered.contains("gates: FAILED"), "{rendered}");
        assert!(
            !rendered.contains("PASSED"),
            "no sentence here can read as approval: {rendered}"
        );
    }

    /// Off-policy estimation abstains, rather than guessing, when an arm
    /// has no support at all.
    #[test]
    fn no_trials_at_all_abstains_rather_than_guessing() {
        let estimate = ipw_acceptance(&[]);
        assert!(matches!(
            estimate,
            AcceptanceEstimate::Abstained(AbstentionReason::NoTrials)
        ));
    }

    /// An unusable assignment probability abstains too, rather than
    /// dividing by zero or a non-finite weight into a number that looks
    /// like an estimate.
    #[test]
    fn an_unusable_assignment_probability_abstains() {
        let observation = ArmObservation {
            accepted: true,
            cost: None,
        };
        let estimate = ipw_acceptance(&[(0.0, observation)]);
        assert!(matches!(
            estimate,
            AcceptanceEstimate::Abstained(AbstentionReason::AssignmentProbabilityUnusable)
        ));
    }

    /// A cost that was never reported must not enter the mean as zero,
    /// and the report says how many were unknown rather than dropping
    /// them silently.
    #[test]
    fn unknown_cost_never_enters_the_mean_as_zero() {
        let known = ArmObservation {
            accepted: true,
            cost: Some(
                TrialCost::new(
                    Some(MicroUsd::from_micros(1_000_000)),
                    crate::money::CostCompleteness::Actual,
                )
                .unwrap(),
            ),
        };
        let unknown = ArmObservation {
            accepted: true,
            cost: Some(TrialCost::UNKNOWN),
        };
        let summary = cost_per_acceptance(&[(1.0, known), (1.0, unknown)]);
        assert_eq!(summary.known_mean, Some(MicroUsd::from_micros(1_000_000)));
        assert_eq!(summary.accepted_with_known_cost, 1);
        assert_eq!(summary.accepted_with_unknown_cost, 1);
    }

    /// The bootstrap seed is a pure function of the task set, independent
    /// of the order the trials happened to be read back in.
    #[test]
    fn the_seed_does_not_depend_on_task_order() {
        let a = vec![TaskId::from_stored("task-a"), TaskId::from_stored("task-b")];
        let b = vec![TaskId::from_stored("task-b"), TaskId::from_stored("task-a")];
        assert_eq!(derive_seed(&a), derive_seed(&b));
    }

    /// Same seed, same trials: the same interval on any machine.
    #[test]
    fn the_bootstrap_is_reproducible_under_the_same_seed() {
        let pairs = vec![
            PairedTask {
                task_id: TaskId::from_stored("task-a"),
                incumbent: ArmObservation {
                    accepted: true,
                    cost: None,
                },
                arm: ArmObservation {
                    accepted: false,
                    cost: None,
                },
            },
            PairedTask {
                task_id: TaskId::from_stored("task-b"),
                incumbent: ArmObservation {
                    accepted: false,
                    cost: None,
                },
                arm: ArmObservation {
                    accepted: true,
                    cost: None,
                },
            },
        ];
        let first = bootstrap_paired_interval(&pairs, 7, 500);
        let second = bootstrap_paired_interval(&pairs, 7, 500);
        assert_eq!(first, second);
    }

    /// A candidate policy with one recipe, and that recipe's id: the id a
    /// trial names to count as this candidate's arm.
    fn candidate_with_one_recipe() -> (RepoPolicy, String) {
        let policy = RepoPolicy::from_toml_str(
            "schema_version = 1\n[[recipes]]\nname = \"docs\"\nscope_within = [\"docs/**\"]\n\
             tier = \"implementation\"\nrevision = 1\n",
        )
        .expect("policy parses");
        let arm = policy.recipes[0].recipe_id();
        (policy, arm)
    }

    /// Settled, accepted replay trials for `arm`, one per task, in a fresh
    /// ledger; plus, when asked, one settled live trial drawn at p = 0.5.
    fn ledger_with_trials(arm: &str, replays: usize, live: usize) -> (Ledger, std::path::PathBuf) {
        use crate::ledger::{NewReplayTrial, NewTrial};
        let dir = crate::test_support::short_temp_dir("cmp-live").to_path_buf();
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        for n in 0..replays {
            let trial_id = TrialId::from_stored(format!("replay-{n}"));
            let task_id = TaskId::from_stored(format!("task-{n}"));
            let run_id = RunId::from_stored(format!("run-{n}"));
            ledger
                .insert_replay_trial(&NewReplayTrial {
                    trial_id: &trial_id,
                    task_id: &task_id,
                    source_run_id: &run_id,
                    incumbent_recipe_id: "incumbent",
                    arm_recipe_id: arm,
                    base_sha: "base",
                    contract_hash: "contract",
                    verification_profile_hash: "profile",
                    workspace_isolation: "fresh_checkout_no_accepted_answer",
                })
                .expect("insert replay");
            ledger
                .settle_trial(
                    &trial_id,
                    TrialOutcome::Accepted,
                    true,
                    TrialCost::UNKNOWN,
                    1,
                )
                .expect("settle replay");
        }
        for n in 0..live {
            let trial_id = TrialId::from_stored(format!("live-{n}"));
            let task_id = TaskId::from_stored(format!("live-task-{n}"));
            // A live row names the arm's OWN run, as `relais run` records
            // it — here an accepted one, the case in which pairing it as
            // if it were a replay source would count it as the incumbent's
            // accepted result and move the figures.
            let run_id = RunId::from_stored(format!("live-run-{n}"));
            ledger
                .insert_run(&run_id, "/repo", None, &task_id, "rk")
                .expect("live arm's run");
            ledger
                .record_transition(&crate::ledger::Transition {
                    run_id: run_id.clone(),
                    attempt_id: None,
                    from_state: Some(State::Prepared),
                    to_state: State::Accepted,
                    reason: "verification_passed".into(),
                    detail: None,
                    at: "2026-09-29T00:00:00+00:00".into(),
                })
                .expect("live arm's run accepted");
            assert_eq!(
                ledger.run_status(&run_id).expect("status"),
                Some(State::Accepted)
            );
            ledger
                .insert_trial(&NewTrial {
                    trial_id: &trial_id,
                    task_id: &task_id,
                    source_run_id: &run_id,
                    incumbent_recipe_id: "incumbent",
                    arm_recipe_id: arm,
                    arm_index: 1,
                    assignment_probability: 0.5,
                    seed: 7,
                    base_sha: "base",
                    contract_hash: "contract",
                    verification_profile_hash: "profile",
                    workspace_isolation: LIVE_WORKTREE,
                    arms_json: None,
                })
                .expect("insert live");
            ledger
                .settle_trial(
                    &trial_id,
                    TrialOutcome::Accepted,
                    true,
                    TrialCost::UNKNOWN,
                    1,
                )
                .expect("settle live");
        }
        (ledger, dir)
    }

    /// A ledger holding replay AND live trials evaluates exactly as one
    /// holding the replays alone, plus the count of live trials set aside:
    /// the basis stays `Replay`, no pair is made with a live trial, and
    /// the rendered evidence names the count.
    ///
    /// FALSIFY: `set_aside_live_trials` was made to keep live trials (its
    /// filter matched nothing), and this test failed on the count
    /// (`left: 0, right: 2`): the live trials, drawn at p = 0.5, stayed in
    /// the pairing and would flip the basis to `Randomized`. Then the
    /// filter was restored. The live rows name real, ACCEPTED runs (as
    /// `relais run` records them), so a mis-pairing would also count them
    /// as the incumbent's accepted results.
    #[test]
    fn live_trials_are_set_aside_and_never_change_what_replays_show() {
        let (candidate, arm) = candidate_with_one_recipe();
        let (replays_only, dir_a) = ledger_with_trials(&arm, 3, 0);
        let (mixed, dir_b) = ledger_with_trials(&arm, 3, 2);
        let alone = evaluate_candidate(&replays_only, &candidate, 20, 200).expect("evaluates");
        let both = evaluate_candidate(&mixed, &candidate, 20, 200).expect("evaluates");
        assert_eq!(alone.live_trials_set_aside, 0);
        assert_eq!(both.live_trials_set_aside, 2);
        assert_eq!(
            both,
            ComparisonReport {
                live_trials_set_aside: 2,
                ..alone
            }
        );
        assert_eq!(both.basis, ComparisonBasis::Replay);
        assert!(
            both.render_evidence().contains(
                "randomized trials set aside: 2 (unpaired estimator not implemented yet)\n"
            ),
            "{}",
            both.render_evidence()
        );
        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }
}
