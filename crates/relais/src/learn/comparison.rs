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

/// The unpaired bootstrap interval over the candidate-minus-control
/// acceptance difference of a randomized comparison: tasks are resampled
/// with replacement WITHIN each arm, independently, each resample weighted
/// by inverse assignment probability. Seeded from a value carried in the
/// report, so the same trials give the same interval on any machine
/// (SPEC §25).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RandomizedInterval {
    pub point: f64,
    pub low: f64,
    pub high: f64,
    pub seed: u64,
    pub resamples: usize,
}

/// The randomized section of a comparison (SPEC §25, §28): live trials in
/// which the candidate was among the arms at draw time, control included,
/// estimated without pairing. `n` counts tasks, one earliest settled
/// non-errored observation per task per arm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RandomizedComparison {
    pub control_tasks: usize,
    pub candidate_tasks: usize,
    /// Settled live trials of this comparison whose outcome was
    /// [`TrialOutcome::Errored`], set aside exactly as for replays.
    pub errored_trials_set_aside: usize,
    /// The smallest recorded assignment probability among the observations
    /// (a non-finite one counts as zero); `None` with no observation.
    pub min_assignment_probability: Option<f64>,
    pub control: ArmSummary,
    pub candidate: ArmSummary,
    pub interval: RandomizedInterval,
}

impl RandomizedComparison {
    /// Every randomized gate that does not hold, recomputed from the
    /// stored figures: each arm has `minimum` observations, neither arm
    /// abstains, and the minimum recorded assignment probability is > 0.
    pub fn failures(&self, minimum: usize) -> Vec<GateFailure> {
        let mut failures = Vec::new();
        for (arm, observed, summary) in [
            (WhichArm::Incumbent, self.control_tasks, &self.control),
            (WhichArm::Arm, self.candidate_tasks, &self.candidate),
        ] {
            if observed < minimum {
                failures.push(GateFailure::RandomizedInsufficientTasks {
                    arm,
                    observed,
                    minimum,
                });
            }
            if let AcceptanceEstimate::Abstained(reason) = summary.acceptance {
                failures.push(GateFailure::RandomizedNoSupport { arm, reason });
            }
        }
        if !self
            .min_assignment_probability
            .is_some_and(|p| p.is_finite() && p > 0.0)
        {
            failures.push(GateFailure::RandomizedProbabilityUnusable);
        }
        failures
    }
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
    /// A randomized arm has too few observations (`Incumbent` is control).
    RandomizedInsufficientTasks {
        arm: WhichArm,
        observed: usize,
        minimum: usize,
    },
    /// A randomized arm abstains (`Incumbent` is control).
    RandomizedNoSupport {
        arm: WhichArm,
        reason: AbstentionReason,
    },
    /// The minimum recorded assignment probability is not above zero.
    RandomizedProbabilityUnusable,
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
            Self::RandomizedInsufficientTasks {
                arm,
                observed,
                minimum,
            } => write!(
                f,
                "randomized: the {} arm has {observed} observation(s), below the {minimum} \
                 required for promotion",
                randomized_arm_name(*arm)
            ),
            Self::RandomizedNoSupport { arm, reason } => write!(
                f,
                "randomized: the {} arm has no support: {reason}",
                randomized_arm_name(*arm)
            ),
            Self::RandomizedProbabilityUnusable => f.write_str(
                "randomized: the minimum recorded assignment probability is not above zero",
            ),
        }
    }
}

/// The incumbent is the control of a randomized comparison.
fn randomized_arm_name(arm: WhichArm) -> &'static str {
    match arm {
        WhichArm::Incumbent => "control",
        WhichArm::Arm => "candidate",
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
    pub incumbent: ArmSummary,
    pub arm: ArmSummary,
    pub interval: PairedInterval,
    /// The unpaired estimate from live (randomized) trials, when any
    /// settled trial drew the candidate among its arms. Additive: a report
    /// written before this field existed reads back as `None`.
    #[serde(default)]
    pub randomized: Option<RandomizedComparison>,
}

impl ComparisonReport {
    /// Every gate that does not hold, recomputed from the numbers beside
    /// it. Empty means promotable: the replay section has no failure, or a
    /// randomized section exists and has none.
    pub fn failures(&self) -> Vec<GateFailure> {
        let mut failures = self.replay_failures();
        if failures.is_empty() {
            return failures;
        }
        if let Some(randomized) = &self.randomized {
            let randomized_failures = randomized.failures(self.min_paired_tasks);
            if randomized_failures.is_empty() {
                return Vec::new();
            }
            failures.extend(randomized_failures);
        }
        failures
    }

    fn replay_failures(&self) -> Vec<GateFailure> {
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
        if let Some(randomized) = &self.randomized {
            out.push_str(&render_randomized(randomized));
        }
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
            if self.replay_failures().is_empty() {
                out.push_str("gates: PASSED\n");
            } else {
                out.push_str("gates: PASSED (basis: randomized)\n");
            }
        } else {
            out.push_str("gates: FAILED — promotion is refused\n");
            for failure in failures {
                out.push_str(&format!("  gate: {failure}\n"));
            }
        }
        out
    }
}

fn render_randomized(randomized: &RandomizedComparison) -> String {
    let mut out = format!(
        "basis: randomized, control n={}, candidate n={}\n",
        randomized.control_tasks, randomized.candidate_tasks
    );
    out.push_str(&format!(
        "  errored trials set aside (not comparable evidence for or against the arm): {}\n",
        randomized.errored_trials_set_aside
    ));
    match randomized.min_assignment_probability {
        Some(p) => out.push_str(&format!("  minimum assignment probability: {p:.3}\n")),
        None => out.push_str("  minimum assignment probability: none recorded\n"),
    }
    out.push_str(&format!("  control: {}\n", render_arm(&randomized.control)));
    out.push_str(&format!(
        "  candidate arm: {}\n",
        render_arm(&randomized.candidate)
    ));
    out.push_str(&format!(
        "  unpaired acceptance difference (candidate - control): {:.3} [{:.3}, {:.3}] \
         ({} bootstrap resample(s), seed {})\n",
        randomized.interval.point,
        randomized.interval.low,
        randomized.interval.high,
        randomized.interval.resamples,
        randomized.interval.seed,
    ));
    out
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
///
/// A candidate is the base policy plus appended revisions, so its recipe
/// ids include the `incumbent`'s own (a recipe id is a content hash). The
/// live section therefore keys on the candidate's ARM ids: its recipe ids
/// minus the incumbent's, so a control row counts only when a recipe new in
/// the candidate was among its non-control arms.
pub fn evaluate_candidate(
    ledger: &Ledger,
    incumbent: &RepoPolicy,
    candidate: &RepoPolicy,
    min_paired_tasks: usize,
    resamples: usize,
) -> Result<ComparisonReport, LedgerError> {
    let recipe_ids: BTreeSet<String> = candidate
        .recipes
        .iter()
        .map(RecipeSpec::recipe_id)
        .collect();
    let trials = replay_trials(ledger.settled_trials_for_recipes(&recipe_ids)?);
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

    let incumbent_ids: BTreeSet<String> = incumbent
        .recipes
        .iter()
        .map(RecipeSpec::recipe_id)
        .collect();
    let arm_ids: BTreeSet<String> = recipe_ids.difference(&incumbent_ids).cloned().collect();
    let randomized = randomized_comparison(
        ledger.settled_live_trials_for_arms(&arm_ids)?,
        &arm_ids,
        resamples,
    );

    Ok(ComparisonReport {
        candidate_recipe_ids: recipe_ids,
        basis,
        paired_tasks: pairs.len(),
        min_paired_tasks,
        errored_trials_set_aside,
        incumbent: incumbent_summary,
        arm: arm_summary_value,
        interval,
        randomized,
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

/// The replay trials: a live trial (SPEC §28) ran in the repository's own
/// worktree and has no replay source run to be paired with, so it takes
/// the unpaired randomized path instead and never enters pairing or
/// [`comparison_basis`].
fn replay_trials(trials: Vec<TrialRow>) -> Vec<TrialRow> {
    trials
        .into_iter()
        .filter(|trial| trial.workspace_isolation != LIVE_WORKTREE)
        .collect()
}

/// The unpaired, inverse-propensity comparison over live trials, or `None`
/// when no settled live trial drew the candidate among its arms. Control
/// rows (`arm_index` 0) form the control arm; rows whose own arm is one of
/// the candidate's recipes form the candidate's. Each arm keeps one
/// observation per task, as replays do, and sets errored trials aside.
fn randomized_comparison(
    live: Vec<TrialRow>,
    arm_ids: &BTreeSet<String>,
    resamples: usize,
) -> Option<RandomizedComparison> {
    if live.is_empty() {
        return None;
    }
    let (control_rows, rest): (Vec<TrialRow>, Vec<TrialRow>) =
        live.into_iter().partition(|trial| trial.arm_index == 0);
    let candidate_rows: Vec<TrialRow> = rest
        .into_iter()
        .filter(|trial| arm_ids.contains(&trial.arm_recipe_id))
        .collect();
    let (control_by_task, control_errored) = earliest_non_errored_per_task(control_rows);
    let (candidate_by_task, candidate_errored) = earliest_non_errored_per_task(candidate_rows);

    let observe = |by_task: &BTreeMap<TaskId, TrialRow>| -> Vec<(f64, ArmObservation)> {
        by_task
            .values()
            .map(|trial| {
                (
                    trial.assignment_probability,
                    ArmObservation {
                        accepted: trial.outcome == Some(TrialOutcome::Accepted),
                        cost: trial.cost,
                    },
                )
            })
            .collect()
    };
    let control = observe(&control_by_task);
    let candidate = observe(&candidate_by_task);

    let min_assignment_probability = control
        .iter()
        .chain(candidate.iter())
        .map(|(p, _)| if p.is_finite() { *p } else { 0.0 })
        .fold(None, |min: Option<f64>, p| {
            Some(min.map_or(p, |m| m.min(p)))
        });
    let trial_ids: Vec<&str> = control_by_task
        .values()
        .chain(candidate_by_task.values())
        .map(|trial| trial.trial_id.as_str())
        .collect();
    let seed = seed_from_ids(trial_ids);

    Some(RandomizedComparison {
        control_tasks: control.len(),
        candidate_tasks: candidate.len(),
        errored_trials_set_aside: control_errored + candidate_errored,
        min_assignment_probability,
        control: hajek_summary(&control),
        candidate: hajek_summary(&candidate),
        interval: bootstrap_unpaired_interval(&control, &candidate, seed, resamples),
    })
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
    seed_from_ids(task_ids.iter().map(TaskId::as_str).collect())
}

/// The same hash over any set of ids, sorted first so read order never
/// matters.
fn seed_from_ids(mut ids: Vec<&str>) -> u64 {
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

/// The inverse-probability weight of one draw, or `None` when the recorded
/// probability cannot carry one (zero, negative, non-finite).
fn inverse_weight(probability: f64) -> Option<f64> {
    let weight = 1.0 / probability;
    (probability.is_finite() && probability > 0.0 && weight.is_finite()).then_some(weight)
}

/// Hajek estimate Σ(w·y)/Σ(w) over `(weight, value)` pairs; `None` when
/// there is no weight to divide by.
fn hajek_mean(weighted: &[(f64, f64)]) -> Option<f64> {
    let weight_sum: f64 = weighted.iter().map(|(w, _)| w).sum();
    (weight_sum > 0.0).then(|| weighted.iter().map(|(w, y)| w * y).sum::<f64>() / weight_sum)
}

/// `(weight, accepted as 0/1)` for every row whose probability can carry a
/// weight.
fn weighted_acceptance(observations: &[(f64, ArmObservation)]) -> Vec<(f64, f64)> {
    observations
        .iter()
        .filter_map(|(p, o)| inverse_weight(*p).map(|w| (w, f64::from(u8::from(o.accepted)))))
        .collect()
}

/// One randomized arm: Hajek acceptance and Hajek cost per accepted change.
/// An arm with no rows, or none whose weight is usable, abstains.
fn hajek_summary(observations: &[(f64, ArmObservation)]) -> ArmSummary {
    let acceptance = if observations.is_empty() {
        AcceptanceEstimate::Abstained(AbstentionReason::NoTrials)
    } else {
        match hajek_mean(&weighted_acceptance(observations)) {
            Some(rate) => AcceptanceEstimate::Estimated(rate),
            None => AcceptanceEstimate::Abstained(AbstentionReason::AssignmentProbabilityUnusable),
        }
    };
    let mut known: Vec<(f64, f64)> = Vec::new();
    let mut unknown_count = 0usize;
    let mut known_count = 0usize;
    for (probability, observation) in observations.iter().filter(|(_, o)| o.accepted) {
        match observation.cost.and_then(TrialCost::cost) {
            Some(cost) => {
                known_count += 1;
                if let Some(weight) = inverse_weight(*probability) {
                    known.push((weight, cost.to_micros() as f64));
                }
            }
            None => unknown_count += 1,
        }
    }
    ArmSummary {
        acceptance,
        cost_per_acceptance: CostPerAcceptance {
            known_mean: hajek_mean(&known)
                .map(|micros| MicroUsd::from_micros(micros.round() as i64)),
            accepted_with_known_cost: known_count,
            accepted_with_unknown_cost: unknown_count,
        },
    }
}

/// The unpaired bootstrap over the candidate-minus-control acceptance
/// difference: each resample redraws tasks with replacement WITHIN each arm,
/// independently — a row is never paired with one of the other arm — and
/// re-estimates each arm's Hajek acceptance from the redrawn, weighted rows.
/// An arm without a usable estimate leaves the interval at a point of zero.
fn bootstrap_unpaired_interval(
    control: &[(f64, ArmObservation)],
    candidate: &[(f64, ArmObservation)],
    seed: u64,
    resamples: usize,
) -> RandomizedInterval {
    let control = weighted_acceptance(control);
    let candidate = weighted_acceptance(candidate);
    let point = match (hajek_mean(&candidate), hajek_mean(&control)) {
        (Some(c), Some(k)) => c - k,
        _ => 0.0,
    };
    if control.is_empty() || candidate.is_empty() || resamples == 0 {
        return RandomizedInterval {
            point,
            low: point,
            high: point,
            seed,
            resamples,
        };
    }
    let mut rng = SplitMix64::new(seed);
    let mut redraw = |arm: &[(f64, f64)]| -> f64 {
        let drawn: Vec<(f64, f64)> = (0..arm.len())
            .map(|_| arm[rng.below(arm.len() as u64) as usize])
            .collect();
        hajek_mean(&drawn).expect("a redraw of positive weights has a positive sum")
    };
    let mut diffs: Vec<f64> = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let control_rate = redraw(&control);
        let candidate_rate = redraw(&candidate);
        diffs.push(candidate_rate - control_rate);
    }
    diffs.sort_by(|a, b| a.partial_cmp(b).expect("bootstrap differences are finite"));
    let low_index = (diffs.len() as f64 * 0.025) as usize;
    let high_index = ((diffs.len() as f64 * 0.975) as usize).min(diffs.len() - 1);
    RandomizedInterval {
        point,
        low: diffs[low_index],
        high: diffs[high_index],
        seed,
        resamples,
    }
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
            randomized: None,
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
            randomized: None,
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

    const BASE_RECIPE: &str = "schema_version = 1\n[[recipes]]\nname = \"docs\"\n\
         scope_within = [\"docs/**\"]\ntier = \"implementation\"\nrevision = 1\n";

    /// The repository's current policy: the incumbent.
    fn base_policy() -> RepoPolicy {
        RepoPolicy::from_toml_str(BASE_RECIPE).expect("policy parses")
    }

    /// The id of the incumbent's own recipe: what a control row names.
    fn base_recipe_id() -> String {
        base_policy().recipes[0].recipe_id()
    }

    /// A candidate built the way real ones are: the base policy plus one
    /// appended revision. Returns it with the appended recipe's id, the id
    /// a trial names to count as this candidate's arm.
    fn candidate_with_one_recipe() -> (RepoPolicy, String) {
        let appended = "[[recipes]]\nname = \"docs\"\nscope_within = [\"docs/**\"]\n\
             tier = \"implementation\"\nrevision = 2\nreview = \"required\"\n";
        let policy =
            RepoPolicy::from_toml_str(&[BASE_RECIPE, appended].concat()).expect("policy parses");
        let arm = policy.recipes[1].recipe_id();
        (policy, arm)
    }

    /// A real recipe id that belongs to neither the base nor the candidate.
    fn unrelated_recipe_id() -> String {
        RepoPolicy::from_toml_str(
            "schema_version = 1\n[[recipes]]\nname = \"src\"\nscope_within = [\"src/**\"]\n\
             tier = \"implementation\"\nrevision = 1\n",
        )
        .expect("policy parses")
        .recipes[0]
            .recipe_id()
    }

    /// Settled, accepted replay trials for `arm`, one per task, in a fresh
    /// ledger.
    fn ledger_with_replays(arm: &str, replays: usize) -> (Ledger, std::path::PathBuf) {
        use crate::ledger::NewReplayTrial;
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
        (ledger, dir)
    }

    /// One settled live trial row: which arm ran (`arm_index` 0 is
    /// control), at what probability, among which `arms`, and how it ended.
    struct LiveRow<'a> {
        name: &'a str,
        arm_index: u32,
        arm_recipe_id: &'a str,
        probability: f64,
        arms: &'a [&'a str],
        outcome: TrialOutcome,
    }

    fn insert_live(ledger: &Ledger, row: &LiveRow<'_>) {
        use crate::ledger::NewTrial;
        let trial_id = TrialId::from_stored(format!("live-{}", row.name));
        let task_id = TaskId::from_stored(format!("task-{}", row.name));
        let run_id = RunId::from_stored(format!("run-{}", row.name));
        ledger
            .insert_run(&run_id, "/repo", None, &task_id, "rk")
            .expect("live arm's run");
        // The run's own state agrees with the trial's outcome, as `relais
        // run` leaves it: an accepted arm names an ACCEPTED run, the case
        // in which leaking a live row into replay pairing would count it
        // as the incumbent's accepted result and move the figures.
        if row.outcome == TrialOutcome::Accepted {
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
        }
        let arms_json = serde_json::to_string(row.arms).expect("arms serialize");
        ledger
            .insert_trial(&NewTrial {
                trial_id: &trial_id,
                task_id: &task_id,
                source_run_id: &run_id,
                incumbent_recipe_id: "incumbent",
                arm_recipe_id: row.arm_recipe_id,
                arm_index: row.arm_index,
                assignment_probability: row.probability,
                seed: 7,
                base_sha: "base",
                contract_hash: "contract",
                verification_profile_hash: "profile",
                workspace_isolation: LIVE_WORKTREE,
                arms_json: Some(&arms_json),
            })
            .expect("insert live");
        ledger
            .settle_trial(&trial_id, row.outcome, true, TrialCost::UNKNOWN, 1)
            .expect("settle live");
    }

    /// `per_arm` accepted control rows and `per_arm` accepted candidate rows,
    /// every one drawn at p = 0.5 among `["incumbent", arm]`.
    fn ledger_with_draws(arm: &str, per_arm: usize) -> (Ledger, std::path::PathBuf) {
        let dir = crate::test_support::short_temp_dir("cmp-rand").to_path_buf();
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        for n in 0..per_arm {
            for (side, arm_index, recipe) in [("c", 0, "incumbent"), ("a", 1, arm)] {
                insert_live(
                    &ledger,
                    &LiveRow {
                        name: &format!("{side}{n}"),
                        arm_index,
                        arm_recipe_id: recipe,
                        probability: 0.5,
                        arms: &["incumbent", arm],
                        outcome: TrialOutcome::Accepted,
                    },
                );
            }
        }
        (ledger, dir)
    }

    /// A replay-only ledger evaluates with no randomized section, and its
    /// rendering carries no trace of one.
    #[test]
    fn a_replay_only_ledger_has_no_randomized_section() {
        let (candidate, arm) = candidate_with_one_recipe();
        let (ledger, dir) = ledger_with_replays(&arm, 3);
        let report =
            evaluate_candidate(&ledger, &base_policy(), &candidate, 20, 200).expect("evaluates");
        assert_eq!(report.randomized, None);
        assert_eq!(report.basis, ComparisonBasis::Replay);
        assert!(
            !report.render().contains("randomized"),
            "{}",
            report.render()
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// Live trials never enter replay pairing: a ledger holding replays AND
    /// live trials (the live rows naming real, ACCEPTED runs) yields the same replay
    /// section as the replays alone. Only the randomized section differs.
    ///
    /// FALSIFY: `replay_trials` was made to keep live rows, and this test
    /// failed (the basis read `Randomized`, not `Replay`). The
    /// `LIVE_WORKTREE` filter was restored.
    #[test]
    fn live_trials_leave_the_replay_section_of_a_mixed_ledger_unchanged() {
        let (candidate, arm) = candidate_with_one_recipe();
        let base = base_recipe_id();
        let (alone_ledger, alone_dir) = ledger_with_replays(&arm, 3);
        let alone = evaluate_candidate(&alone_ledger, &base_policy(), &candidate, 20, 200)
            .expect("evaluates");
        let (mixed_ledger, mixed_dir) = ledger_with_replays(&arm, 3);
        for (name, arm_index, recipe) in [("m-c", 0, base.as_str()), ("m-a", 1, arm.as_str())] {
            insert_live(
                &mixed_ledger,
                &LiveRow {
                    name,
                    arm_index,
                    arm_recipe_id: recipe,
                    probability: 0.5,
                    arms: &[&base, &arm],
                    outcome: TrialOutcome::Accepted,
                },
            );
        }
        let mixed = evaluate_candidate(&mixed_ledger, &base_policy(), &candidate, 20, 200)
            .expect("evaluates");
        assert!(mixed.randomized.is_some(), "the live rows form a section");
        assert_eq!(mixed.basis, ComparisonBasis::Replay);
        assert_eq!(alone.paired_tasks, 3);
        assert_eq!(
            ComparisonReport {
                randomized: None,
                ..mixed
            },
            alone
        );
        std::fs::remove_dir_all(alone_dir).ok();
        std::fs::remove_dir_all(mixed_dir).ok();
    }

    /// A control row counts only when the candidate was among the arms at
    /// draw time. Rows drawn between `incumbent` and some OTHER recipe say
    /// nothing about this candidate, control or not.
    ///
    /// The candidate's arm ids are its recipe ids MINUS the incumbent's,
    /// because the candidate carries the base recipe too: a control row
    /// drawn among `[base_id, other_id]` names the base id, which would
    /// match by membership against every candidate id.
    ///
    /// FALSIFY: membership was matched against all of `arms_json` (and the
    /// candidate's full recipe ids) again, and this test failed
    /// (`control_tasks` read 3, not 1): the control row drawn without the
    /// candidate was counted. The arm-id set and `arms[1..]` were restored.
    #[test]
    fn a_control_row_drawn_without_the_candidate_is_not_counted() {
        let (candidate, arm) = candidate_with_one_recipe();
        let base = base_recipe_id();
        let other = unrelated_recipe_id();
        let dir = crate::test_support::short_temp_dir("cmp-rand-a").to_path_buf();
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        let outcome = TrialOutcome::Accepted;
        insert_live(
            &ledger,
            &LiveRow {
                name: "in-control",
                arm_index: 0,
                arm_recipe_id: &base,
                probability: 0.5,
                arms: &[&base, &arm],
                outcome,
            },
        );
        insert_live(
            &ledger,
            &LiveRow {
                name: "in-arm",
                arm_index: 1,
                arm_recipe_id: &arm,
                probability: 0.5,
                arms: &[&base, &arm],
                outcome,
            },
        );
        for name in ["out-control-1", "out-control-2"] {
            insert_live(
                &ledger,
                &LiveRow {
                    name,
                    arm_index: 0,
                    arm_recipe_id: &base,
                    probability: 0.5,
                    arms: &[&base, &other],
                    outcome,
                },
            );
        }
        let report =
            evaluate_candidate(&ledger, &base_policy(), &candidate, 20, 200).expect("evaluates");
        let randomized = report.randomized.expect("a randomized section");
        assert_eq!(randomized.control_tasks, 1);
        assert_eq!(randomized.candidate_tasks, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    /// The Hajek estimate weights each row by 1/p. Control: accepted at
    /// p = 0.5 (w 2), rejected at p = 0.25 (w 4) is 2/6 = 1/3, where an
    /// unweighted mean says 1/2. Candidate: accepted at p = 0.25 (w 4) and
    /// rejected at p = 0.5 (w 2) is 4/6 = 2/3.
    ///
    /// FALSIFY: every row was weighted 1.0 in `weighted_acceptance`, and this
    /// test failed (control read 0.5, not 1/3); the weights were restored.
    #[test]
    fn the_hajek_estimate_weights_rows_by_inverse_probability() {
        let (candidate, arm) = candidate_with_one_recipe();
        let dir = crate::test_support::short_temp_dir("cmp-rand-b").to_path_buf();
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        let arms: &[&str] = &["incumbent", &arm];
        for (name, arm_index, recipe, probability, outcome) in [
            ("c1", 0, "incumbent", 0.5, TrialOutcome::Accepted),
            ("c2", 0, "incumbent", 0.25, TrialOutcome::Rejected),
            ("a1", 1, arm.as_str(), 0.25, TrialOutcome::Accepted),
            ("a2", 1, arm.as_str(), 0.5, TrialOutcome::Rejected),
        ] {
            insert_live(
                &ledger,
                &LiveRow {
                    name,
                    arm_index,
                    arm_recipe_id: recipe,
                    probability,
                    arms,
                    outcome,
                },
            );
        }
        let randomized = evaluate_candidate(&ledger, &base_policy(), &candidate, 20, 200)
            .expect("evaluates")
            .randomized
            .expect("a randomized section");
        let AcceptanceEstimate::Estimated(control) = randomized.control.acceptance else {
            panic!("control abstained: {:?}", randomized.control);
        };
        let AcceptanceEstimate::Estimated(arm_rate) = randomized.candidate.acceptance else {
            panic!("candidate abstained: {:?}", randomized.candidate);
        };
        assert!((control - 1.0 / 3.0).abs() < 1e-12, "{control}");
        assert!((arm_rate - 2.0 / 3.0).abs() < 1e-12, "{arm_rate}");
        assert!((randomized.interval.point - 1.0 / 3.0).abs() < 1e-12);
        std::fs::remove_dir_all(dir).ok();
    }

    /// 20 observations per arm clear every randomized gate, so the report
    /// promotes on that basis alone and says so; 19 per arm refuses and
    /// names the gate.
    #[test]
    fn twenty_per_arm_promotes_on_randomized_evidence_and_nineteen_refuses() {
        let (candidate, arm) = candidate_with_one_recipe();
        let (enough, dir_a) = ledger_with_draws(&arm, 20);
        let report = evaluate_candidate(&enough, &base_policy(), &candidate, MIN_PAIRED_TASKS, 200)
            .expect("evaluates");
        assert!(report.promotable().is_some(), "{:?}", report.failures());
        let rendered = report.render();
        assert!(
            rendered.contains("basis: randomized, control n=20, candidate n=20"),
            "{rendered}"
        );
        assert!(
            rendered.contains("gates: PASSED (basis: randomized)"),
            "{rendered}"
        );

        let (short, dir_b) = ledger_with_draws(&arm, 19);
        let refused = evaluate_candidate(&short, &base_policy(), &candidate, MIN_PAIRED_TASKS, 200)
            .expect("evaluates");
        assert!(refused.promotable().is_none());
        assert!(
            refused
                .failures()
                .contains(&GateFailure::RandomizedInsufficientTasks {
                    arm: WhichArm::Incumbent,
                    observed: 19,
                    minimum: MIN_PAIRED_TASKS,
                }),
            "{:?}",
            refused.failures()
        );
        let rendered = refused.render();
        assert!(
            rendered.contains("randomized: the control arm has 19 observation(s)"),
            "{rendered}"
        );
        assert!(!rendered.contains("PASSED"), "{rendered}");
        std::fs::remove_dir_all(dir_a).ok();
        std::fs::remove_dir_all(dir_b).ok();
    }

    /// An arm whose recorded probabilities are all unusable abstains, and a
    /// zero minimum probability fails its own gate.
    #[test]
    fn an_arm_with_no_usable_weight_abstains_and_fails_the_probability_gate() {
        let (candidate, arm) = candidate_with_one_recipe();
        let dir = crate::test_support::short_temp_dir("cmp-rand-abstain").to_path_buf();
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        let arms: &[&str] = &["incumbent", &arm];
        for (name, arm_index, recipe, probability) in
            [("c1", 0, "incumbent", 0.5), ("a1", 1, arm.as_str(), 0.0)]
        {
            insert_live(
                &ledger,
                &LiveRow {
                    name,
                    arm_index,
                    arm_recipe_id: recipe,
                    probability,
                    arms,
                    outcome: TrialOutcome::Accepted,
                },
            );
        }
        let report =
            evaluate_candidate(&ledger, &base_policy(), &candidate, 1, 200).expect("evaluates");
        let randomized = report.randomized.as_ref().expect("a randomized section");
        assert_eq!(
            randomized.candidate.acceptance,
            AcceptanceEstimate::Abstained(AbstentionReason::AssignmentProbabilityUnusable)
        );
        let failures = randomized.failures(1);
        assert!(failures.contains(&GateFailure::RandomizedProbabilityUnusable));
        assert!(report.promotable().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    /// Same rows, same interval — and the same seed, from the sorted trial
    /// ids.
    #[test]
    fn the_unpaired_bootstrap_is_deterministic_for_the_same_rows() {
        let (candidate, arm) = candidate_with_one_recipe();
        let (ledger, dir) = ledger_with_draws(&arm, 25);
        let first = evaluate_candidate(&ledger, &base_policy(), &candidate, MIN_PAIRED_TASKS, 500)
            .expect("evaluates");
        let second = evaluate_candidate(&ledger, &base_policy(), &candidate, MIN_PAIRED_TASKS, 500)
            .expect("evaluates");
        assert_eq!(first, second);
        assert_eq!(
            first.randomized.expect("randomized").interval.resamples,
            500
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// Errored live trials are set aside and counted, never observed.
    #[test]
    fn errored_live_trials_are_set_aside_and_counted() {
        let (candidate, arm) = candidate_with_one_recipe();
        let dir = crate::test_support::short_temp_dir("cmp-rand-err").to_path_buf();
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        insert_live(
            &ledger,
            &LiveRow {
                name: "c1",
                arm_index: 0,
                arm_recipe_id: "incumbent",
                probability: 0.5,
                arms: &["incumbent", &arm],
                outcome: TrialOutcome::Errored,
            },
        );
        let randomized = evaluate_candidate(&ledger, &base_policy(), &candidate, 20, 200)
            .expect("evaluates")
            .randomized
            .expect("a randomized section");
        assert_eq!(randomized.errored_trials_set_aside, 1);
        assert_eq!(randomized.control_tasks, 0);
        assert_eq!(
            randomized.control.acceptance,
            AcceptanceEstimate::Abstained(AbstentionReason::NoTrials)
        );
        std::fs::remove_dir_all(dir).ok();
    }
}
