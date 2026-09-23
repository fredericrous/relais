//! Owned dataset construction (SPEC §17, §21).
//!
//! A task is the unit of sampling. Pre-dispatch features come from the
//! ledger's dispatch records — reconstructed without future information;
//! labels are evidence-backed and attempt-level outcomes stay distinct
//! from complete-strategy outcomes: a cheap worker rescued by a stronger
//! worker did NOT succeed without escalation. Infrastructure failures
//! (blocked, interrupted) and unresolved runs are excluded with reasons,
//! never silently converted into reasoning failures. Task families group
//! duplicates and replays; the dataset records its exclusions, class
//! distribution and profile coverage.

use serde::{Deserialize, Serialize};

use super::features::{
    expand, AcceptanceRoute, FeatureSchema, ProfileIdentity, SparseVec, TaskFeatures,
    TrainingExample,
};
use crate::ids::{sha256_hex, TaskId};
use crate::ledger::{Ledger, LedgerError};
use crate::lifecycle::{Reason, State};
use crate::outcome::OutcomeKind;
use crate::policy::{RepoPolicy, Tier};

/// The stored contract and first-attempt tier for a run, as dataset
/// construction consumes them.
pub type ContractMaterial = (crate::contract::TaskContract, String, String);

/// How dataset construction reads a run's contract revision: `Ok(None)` is
/// a run with no revision recorded (an exclusion), `Err` a ledger that
/// could not be read (an error). The two were one `Option` and a failing
/// read was indistinguishable from a run that never recorded a contract.
pub type ContractLookup<'a> = dyn Fn(&str) -> Result<Option<ContractMaterial>, LedgerError> + 'a;

/// Version 2 records, per example, the routing FLOOR its contract had at
/// dispatch time (SPEC §6): the evaluator's baseline is the route the
/// router would have taken for that task, which a version-1 dataset does
/// not carry. Version 3 samples by TASK rather than by run, reads a
/// task's later recorded outcome into its label, and records which route
/// (verification or a person's approval) accepted it — none of which a
/// version-2 dataset carries. Rebuild with `relais dataset build` to
/// train again.
pub const DATASET_VERSION: u32 = 3;

/// The version of the rule in [`labelling_of`] and the acceptance
/// decision at [`build`]'s `route == AcceptanceRoute::Verified` check —
/// together, "what counts as a positive label". Recorded on every
/// [`Dataset`] and carried onto the [`super::registry::Artifact`] it
/// trains, so a dataset or artifact built under an earlier rule is
/// distinguishable from one built under this one rather than silently
/// comparable. Bump this whenever either decision changes, independently
/// of `DATASET_VERSION` — the record layout and the labelling rule are
/// two different things that happen to change together today.
pub const LABEL_POLICY_VERSION: u32 = 1;

/// The window dataset construction reads: every run the ledger holds.
/// Far enough in the past that no relais ledger predates it.
const EPOCH: &str = "2000-01-01T00:00:00+00:00";

/// Dataset construction failed against the ledger. A failed read is never
/// an empty dataset and never an exclusion: silently training on nothing
/// after `SQLITE_BUSY` looked exactly like "collect outcomes first".
#[derive(Debug)]
pub enum DatasetError {
    /// The run list itself could not be read.
    RunList { since: String, cause: LedgerError },
    /// A per-run read failed. `query` names what was being read.
    LedgerRead {
        run: String,
        query: &'static str,
        cause: LedgerError,
    },
}

impl std::fmt::Display for DatasetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RunList { since, cause } => {
                write!(f, "dataset: cannot list runs since {since}: {cause}")
            }
            Self::LedgerRead { run, query, cause } => {
                write!(f, "dataset: cannot read the {query} of run {run}: {cause}")
            }
        }
    }
}

impl std::error::Error for DatasetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RunList { cause, .. } | Self::LedgerRead { cause, .. } => Some(cause),
        }
    }
}

/// A ledger read for one run: a failure names the run and the query, and
/// stops dataset construction. Absence is the caller's business.
fn read_of<T>(
    run: &crate::ids::RunId,
    query: &'static str,
    result: Result<T, LedgerError>,
) -> Result<T, DatasetError> {
    result.map_err(|cause| DatasetError::LedgerRead {
        run: run.as_str().to_string(),
        query,
        cause,
    })
}

/// What a run's recorded state means for the dataset: an evidence-backed
/// reasoning outcome, or an exclusion and the reason for it (SPEC §17).
///
/// Only a run that reached a reasoning verdict is labelled. Everything
/// else — a run still in flight, one the environment blocked, one whose
/// budget ran out before the reasoning was tested, one waiting on a person
/// — is excluded and counted in `exclusions`. They used to fall through a
/// `_ => {}` arm into the NEGATIVE class: every running task in the ledger
/// taught the learner that its tier fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Labelling {
    /// The run was accepted: positive unless escalation was attempted.
    Accepted,
    /// The run failed after the ladder ran out: a negative label.
    Failed,
    /// Not a reasoning outcome, with the reason it is excluded.
    Excluded(&'static str),
}

/// The labelling a lifecycle state earns on its own. Total over `State`
/// on purpose: a new state is a compile error here, not a silent negative
/// label.
fn state_labelling(state: State) -> Labelling {
    match state {
        State::Accepted => Labelling::Accepted,
        State::Failed => Labelling::Failed,
        State::Prepared
        | State::Running
        | State::Verifying
        | State::Repairing
        | State::Escalating => Labelling::Excluded("still in flight; the outcome is not known yet"),
        State::BudgetExhausted => {
            Labelling::Excluded("budget exhausted before the reasoning was tested")
        }
        State::Blocked => Labelling::Excluded("blocked environment, not a reasoning label"),
        State::Interrupted => Labelling::Excluded("interrupted, outcome unknown"),
        State::NeedsReview => Labelling::Excluded("needs_review, no evidence-backed label yet"),
        State::NeedsDecision => Labelling::Excluded("needs_decision, no evidence-backed label yet"),
        State::Cancelled => Labelling::Excluded("cancelled, no evidence-backed label yet"),
    }
}

/// Whether a task's most recently recorded final outcome (SPEC §20) says
/// the accepted change is still standing. Exhaustive over `OutcomeKind`
/// with no wildcard arm: a new outcome kind does not compile here until
/// it says which way it falls.
///
/// The ONE place an outcome kind is read as standing-or-not.
/// [`labelling_of`] calls it rather than matching the kinds again, and
/// `TrainingExample::outcome_stood` records what it returned, so an
/// example cannot say a change stood while its own label says it did
/// not — two exhaustive matches over one enum stay correct only until
/// someone adds a variant and answers them differently.
pub(crate) fn outcome_stands(kind: OutcomeKind) -> bool {
    match kind {
        OutcomeKind::AcceptedUnchanged => true,
        OutcomeKind::Corrected => true,
        OutcomeKind::Reverted => false,
        OutcomeKind::ConfirmedRegression => false,
    }
}

/// The labelling a task earns from its run's terminal state AND its most
/// recently recorded final outcome (SPEC §20), so a task later reverted
/// or a later confirmed regression is never a positive label whatever its
/// run's state said. Non-accepted labellings are untouched by outcome —
/// a failed or excluded run has no accepted change for an outcome to
/// speak about. The `Accepted` branch below matches `Option<OutcomeKind>`
/// exhaustively with no wildcard, so a new outcome kind does not compile
/// here until it says which way it falls: they used to be read only from
/// `state`, so a reverted or later-regressed task kept the positive label
/// its run's terminal state gave it forever.
pub fn labelling_of(state: State, outcome: Option<OutcomeKind>) -> Labelling {
    let base = state_labelling(state);
    if base != Labelling::Accepted {
        return base;
    }
    match outcome {
        // No outcome recorded yet is not evidence of anything: the run
        // was accepted, and absence of feedback is never a positive
        // label in its own right, only the accepted label already earned.
        None => Labelling::Accepted,
        // Not a wildcard: `kind` is bound and handed to the one
        // exhaustive match over `OutcomeKind` there is, so a new variant
        // still fails to compile — in `outcome_stands`, once, instead of
        // here and there.
        Some(kind) => {
            if outcome_stands(kind) {
                Labelling::Accepted
            } else {
                Labelling::Failed
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dataset {
    pub version: u32,
    /// The version of the labelling rule that produced `records` (see
    /// [`LABEL_POLICY_VERSION`]).
    pub label_policy_version: u32,
    pub records: Vec<TrainingExample>,
    pub exclusions: Vec<String>,
    pub fingerprint: String,
    pub built_at: String,
}

/// How the labels fall: the numbers a person needs to see before trusting
/// anything fitted on them. Two bare `usize`s in a tuple were read in the
/// wrong order once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassDistribution {
    pub records: usize,
    /// Records labelled "the first-choice tier succeeded on its own".
    pub accepted_without_escalation: usize,
    /// Records labelled otherwise: failed, or rescued by escalation.
    pub other: usize,
}

impl Dataset {
    /// The class distribution of the acceptance label.
    pub fn acceptance_labels(&self) -> ClassDistribution {
        let accepted_without_escalation = self
            .records
            .iter()
            .filter(|record| record.accepted_without_escalation)
            .count();
        ClassDistribution {
            records: self.records.len(),
            accepted_without_escalation,
            other: self.records.len() - accepted_without_escalation,
        }
    }
}

/// The features recorded at dispatch time for a run's first attempt,
/// reconstructed from what the runner persisted BEFORE the worker ran.
/// The objective text is read back from the stored contract, and the
/// task-level features plus tier/model identity are the ONLY inputs —
/// outcomes, patch sizes and later failures never enter (SPEC §21).
///
/// Every ledger read is fallible and every failure stops the build: an
/// unreadable ledger produces an error a person sees, never a smaller
/// dataset that looks like a quiet week.
pub fn build(
    ledger: &Ledger,
    contract_of: &ContractLookup<'_>,
    repo_policy: &RepoPolicy,
) -> Result<Dataset, DatasetError> {
    let schema = FeatureSchema::standard();
    let mut records = Vec::new();
    let mut exclusions = Vec::new();
    let runs = ledger
        .runs_since(EPOCH)
        .map_err(|cause| DatasetError::RunList {
            since: EPOCH.to_string(),
            cause,
        })?;
    // The sampling unit is the task, not the run (SPEC §17): a task
    // retried to acceptance contributes one example, not one per attempt.
    // `runs_since` reads newest-first, so the first run seen for a task
    // is its most recent attempt — the one worth representing it, since a
    // retry exists only because an earlier attempt did not stand.
    let mut seen_tasks: std::collections::HashSet<TaskId> = std::collections::HashSet::new();
    for (run_id, _repo, _status, _created) in runs {
        let Some(task_id) = read_of(&run_id, "task identity", ledger.task_of_run(&run_id))? else {
            exclusions.push(format!("{run_id}: no task identity recorded"));
            continue;
        };
        if !seen_tasks.insert(task_id.clone()) {
            exclusions.push(format!(
                "{run_id}: superseded by a later attempt at task {task_id}"
            ));
            continue;
        }
        let material = read_of(&run_id, "contract revision", contract_of(run_id.as_str()))?;
        let Some((contract, objective, tier_name)) = material else {
            exclusions.push(format!("{run_id}: no contract revision recorded"));
            continue;
        };
        let Some(state) = read_of(&run_id, "state", ledger.run_status(&run_id))? else {
            exclusions.push(format!("{run_id}: no state recorded"));
            continue;
        };
        // A task's later recorded outcome (SPEC §20) can withdraw its
        // positive label even when its state alone said accepted — a
        // reverted change or a later confirmed regression is never a
        // positive label.
        let latest_outcome = read_of(&run_id, "latest outcome", ledger.latest_outcome(&task_id))?;
        let outcome_kind = latest_outcome.as_ref().map(|stored| stored.outcome.kind);
        // Infrastructure and unresolved outcomes are excluded, not
        // relabelled (SPEC §17).
        let accepted = match labelling_of(state, outcome_kind) {
            Labelling::Accepted => true,
            Labelling::Failed => false,
            Labelling::Excluded(why) => {
                exclusions.push(format!("{run_id}: {state}: {why}"));
                continue;
            }
        };
        let Some(tier) = Tier::parse(&tier_name) else {
            exclusions.push(format!("{run_id}: unknown tier `{tier_name}`"));
            continue;
        };
        // A cheap worker rescued by a stronger worker did not succeed
        // without escalation. The attempt table's phase says whether the
        // ladder moved; the set of models seen does NOT — a reviewer at
        // the escalation tier is not an escalation, and counting it as
        // one labelled every reviewed run a failure of its worker.
        let escalated = read_of(
            &run_id,
            "escalation attempts",
            ledger.escalation_attempted(&run_id),
        )?;
        // A person's approval (`relais decide --answer approve`) accepts
        // the candidate, but it is not the VERIFIED acceptance SPEC §17
        // reserves a positive label for: the acceptance predictor is
        // fitted to estimate verification success, and a human override
        // is not independent evidence of that. The example still enters
        // the dataset — `route` names how it was actually accepted, so
        // the dataset reads honestly — it just never contributes a
        // positive label.
        let decision = read_of(&run_id, "decision", ledger.decision_of_run(&run_id))?;
        let route =
            if decision.and_then(|record| record.resolution) == Some(Reason::DecisionApproved) {
                AcceptanceRoute::PersonApproved
            } else {
                AcceptanceRoute::Verified
            };
        let accepted_without_escalation =
            accepted && !escalated && route == AcceptanceRoute::Verified;
        let outcome_stood = outcome_kind.map(outcome_stands);
        let correction_magnitude = latest_outcome
            .as_ref()
            .and_then(|stored| stored.outcome.detail.correction_magnitude)
            .map(|magnitude| magnitude.get());
        let cost = read_of(&run_id, "cost", ledger.run_cost(&run_id))?;
        let completeness = read_of(
            &run_id,
            "cost completeness",
            ledger.run_cost_completeness(&run_id),
        )?;
        let cost_complete = completeness == crate::money::CostCompleteness::Actual;
        let task = TaskFeatures::extract(&contract, repo_policy);
        // The identity that actually ran. Without a dispatch intent there
        // is no model to attribute the outcome to, and a default identity
        // would credit the empty model with this run's evidence.
        let intent = read_of(
            &run_id,
            "first dispatch intent",
            ledger.first_dispatch_intent(&run_id),
        )?;
        let Some(identity) = intent.as_ref().and_then(identity_of_intent) else {
            exclusions.push(format!(
                "{run_id}: no dispatch intent naming a model; the outcome cannot be attributed"
            ));
            continue;
        };
        let sparse: SparseVec = expand(&task, tier, &objective, &identity, &schema);
        let transitions = read_of(&run_id, "transitions", ledger.transitions(&run_id))?;
        let Some(dispatched_at) = transitions.first().map(|transition| transition.at.clone())
        else {
            // An empty timestamp sorts before every real one, so such a
            // record would land in the training split whatever its date.
            exclusions.push(format!("{run_id}: no transition recorded; undatable"));
            continue;
        };
        records.push(TrainingExample {
            family: task_family(&contract),
            tier,
            floor: crate::route::eligible_tiers(&contract, repo_policy, &repo_policy.models).floor,
            task,
            objective: objective.clone(),
            identity,
            sparse,
            accepted_without_escalation,
            route,
            outcome_stood,
            correction_magnitude,
            complete_cost: cost,
            cost_complete,
            dispatched_at,
        });
    }
    // The records are owned structs of numbers, strings and vectors:
    // serialization has no failure mode, and a fingerprint over
    // `unwrap_or_default()`'s empty string would have been the SAME hash
    // for every dataset that failed to serialize.
    let fingerprint_source =
        serde_json::to_string(&records).expect("training examples are plain owned data");
    let fingerprint = sha256_hex(fingerprint_source.as_bytes());
    Ok(Dataset {
        version: DATASET_VERSION,
        label_policy_version: LABEL_POLICY_VERSION,
        records,
        exclusions,
        fingerprint,
        built_at: crate::ledger::now_rfc3339(),
    })
}

/// The profile identity a dispatch intent records, or `None` when it names
/// no model. Reading with `intent["model"]` returned JSON null for a
/// malformed intent, which became the model literally called "unknown".
fn identity_of_intent(intent: &serde_json::Value) -> Option<ProfileIdentity> {
    let text = |key: &str| {
        intent
            .get(key)
            .and_then(|value| value.as_str())
            .map(str::to_string)
    };
    Some(ProfileIdentity {
        model: text("model")?,
        effort: text("effort"),
        harness: text("harness"),
    })
}

/// The family a contract belongs to, for split grouping (SPEC §17:
/// "avoid counting many retries or near-identical task variants as
/// independent examples"). Kind, scope and the objective's token SET
/// — not its exact text, so a re-worded retry of the same task stays with
/// the original, while a different task on the same scope does not.
pub fn task_family(contract: &crate::contract::TaskContract) -> String {
    // The feature tokenizer, not a second spelling of it: a family must
    // group the tasks whose features the learner cannot tell apart.
    let mut tokens: Vec<String> = super::features::tokenize(&contract.objective);
    tokens.sort();
    tokens.dedup();
    let mut scope = contract.scope_patterns().to_vec();
    scope.sort();
    sha256_hex(
        serde_json::json!({
            "kind": contract.kind(),
            "scope": scope,
            "tokens": tokens,
        })
        .to_string()
        .as_bytes(),
    )
}

/// Splits are TEMPORAL and family-aware (SPEC §17): records are ordered by
/// dispatch time, a family lands entirely in the LATEST split any of its
/// records falls into, and the splits are train / calibration / test.
/// Duplicated replays of one task never straddle a split boundary.
///
/// The pin used to be the family's FIRST record, which put future data in
/// the training split: a family whose first replay landed in train
/// absorbed every later record of that family, including ones dispatched
/// after the test cutoff, and the learner was fitted on outcomes from
/// after the period it is evaluated over (SPEC §21: "dataset splits
/// prevent related-task leakage").
///
/// Pinning to the last record fixes it in the only direction that
/// matters. Leakage is asymmetric: an early record sitting in the test
/// split tells the learner nothing, because it was never trained on,
/// while a late record sitting in train is information from the future.
/// Dropping straddling families would also be leak-free, but it discards
/// evidence — and it discards exactly the families with the most
/// observations, which are the ones replays were collected for.
#[derive(Debug, Clone, PartialEq)]
pub struct TemporalSplits<'a> {
    pub train: Vec<&'a TrainingExample>,
    pub calibration: Vec<&'a TrainingExample>,
    pub test: Vec<&'a TrainingExample>,
}

pub fn temporal_splits(
    records: &[TrainingExample],
    calibration_fraction: f64,
    test_fraction: f64,
) -> TemporalSplits<'_> {
    let mut sorted: Vec<&TrainingExample> = records.iter().collect();
    sorted.sort_by(|a, b| a.dispatched_at.cmp(&b.dispatched_at));
    let n = sorted.len();
    let calibration_start = ((n as f64) * (1.0 - test_fraction - calibration_fraction)) as usize;
    let test_start = ((n as f64) * (1.0 - test_fraction)) as usize;
    let split_of = |index: usize| -> u8 {
        if index < calibration_start {
            0
        } else if index < test_start {
            1
        } else {
            2
        }
    };
    // Two passes: the latest split any record of a family falls into, then
    // every record of that family placed there. A family that straddles a
    // boundary moves forward in time, never back into training.
    let mut latest_split_of_family: std::collections::BTreeMap<&str, u8> =
        std::collections::BTreeMap::new();
    for (index, record) in sorted.iter().enumerate() {
        let split = split_of(index);
        latest_split_of_family
            .entry(record.family.as_str())
            .and_modify(|pinned| *pinned = (*pinned).max(split))
            .or_insert(split);
    }
    let mut train = Vec::new();
    let mut calibration = Vec::new();
    let mut test = Vec::new();
    for record in sorted {
        match latest_split_of_family
            .get(record.family.as_str())
            .copied()
            .unwrap_or(2)
        {
            0 => train.push(record),
            1 => calibration.push(record),
            _ => test.push(record),
        }
    }
    TemporalSplits {
        train,
        calibration,
        test,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::UsagePhase;

    fn run_id(id: &str) -> crate::ids::RunId {
        crate::ids::RunId::from_stored(id)
    }

    fn task_id(id: &str) -> crate::ids::TaskId {
        crate::ids::TaskId::from_stored(format!("task-{id}"))
    }

    /// A test directory nobody else can collide with, pre-cleaned so a
    /// crashed earlier run cannot make this one pass or fail: the process
    /// owns the pid, and the counter orders the directories within it.
    /// A thread id is reused the moment a thread ends, which made two
    /// tests in one run share a ledger.
    fn temp_dir(name: &str) -> std::path::PathBuf {
        crate::test_support::temp_dir(name)
    }

    fn example(family: &str, at: &str, accepted: bool) -> TrainingExample {
        TrainingExample {
            family: family.into(),
            tier: Tier::Implementation,
            floor: Tier::Implementation,
            task: TaskFeatures::extract(&contract(), &repo_policy()),
            objective: "objective".into(),
            identity: ProfileIdentity::default(),
            sparse: SparseVec(vec![(0, 1.0)]),
            accepted_without_escalation: accepted,
            route: AcceptanceRoute::Verified,
            outcome_stood: None,
            correction_magnitude: None,
            complete_cost: crate::money::MicroUsd::from_micros(100),
            cost_complete: true,
            dispatched_at: at.into(),
        }
    }

    #[test]
    fn temporal_splits_respect_time_and_family() {
        let mut records = Vec::new();
        for day in 1..=9 {
            records.push(example(
                &format!("family-{day}"),
                &format!("2026-09-{day:02}T00:00:00+00:00"),
                true,
            ));
        }
        let splits = temporal_splits(&records, 0.2, 0.2);
        assert_eq!(splits.train.len(), 5);
        assert_eq!(splits.calibration.len(), 2);
        assert_eq!(splits.test.len(), 2);
        let train_last = splits.train.last().unwrap().dispatched_at.clone();
        let cal_first = splits.calibration.first().unwrap().dispatched_at.clone();
        assert!(train_last < cal_first, "temporal order holds");
    }

    /// D2: a family whose first record landed in train used to absorb
    /// every later record — including replays dispatched after the test
    /// cutoff. The training split then held data from the future.
    #[test]
    fn a_family_straddling_the_cutoff_leaves_the_training_split() {
        let mut records = Vec::new();
        for day in 1..=9 {
            records.push(example(
                &format!("family-{day}"),
                &format!("2026-09-{day:02}T00:00:00+00:00"),
                true,
            ));
        }
        // `family-1`'s first record is the earliest of all (train under
        // any rule); its replay is the latest of all (test).
        records.push(example("family-1", "2026-10-01T00:00:00+00:00", false));
        let splits = temporal_splits(&records, 0.2, 0.2);

        let in_train: Vec<&str> = splits
            .train
            .iter()
            .map(|record| record.dispatched_at.as_str())
            .collect();
        assert!(
            splits
                .train
                .iter()
                .all(|record| record.family != "family-1"),
            "a straddling family may not train the learner: {in_train:?}"
        );
        assert_eq!(
            splits
                .test
                .iter()
                .filter(|record| record.family == "family-1")
                .count(),
            2,
            "both of the family's records land together, in its latest split"
        );
        // The training split now ends strictly before the test split
        // begins — the property the pin exists for.
        let train_end = in_train.iter().max().copied().unwrap_or("");
        let post_cutoff = splits
            .test
            .iter()
            .map(|record| record.dispatched_at.as_str())
            .max()
            .unwrap_or("");
        assert!(
            train_end < post_cutoff,
            "train {train_end} must precede the last test record {post_cutoff}"
        );
        assert!(
            !in_train.iter().any(|at| at.starts_with("2026-10")),
            "no post-cutoff record reaches the training split: {in_train:?}"
        );
        // Nothing is dropped: every record is somewhere.
        assert_eq!(
            splits.train.len() + splits.calibration.len() + splits.test.len(),
            records.len(),
            "a straddling family is moved, not discarded"
        );
    }

    fn contract() -> crate::contract::TaskContract {
        crate::contract::TaskContract::from_json_str(
            r#"{"schema_version":1,"kind":"change","objective":"Fix the escaping",
                "base_ref":"HEAD","write_scope":["crates/**"],"acceptance":["parses"],
                "verification_profile":"p"}"#,
        )
        .expect("contract")
    }

    fn repo_policy() -> RepoPolicy {
        RepoPolicy::from_toml_str(
            r#"schema_version = 1
[models.implementation]
id = "sonnet"
[[verification.profiles.p.commands]]
argv = ["true"]
"#,
        )
        .expect("policy")
    }

    /// Everything a dataset-worthy run records: a contract revision, an
    /// attempt, a dispatch intent naming the model, and usage.
    struct LedgerFixture {
        ledger: crate::ledger::Ledger,
        contract: crate::contract::TaskContract,
        contract_json: String,
        dir: std::path::PathBuf,
    }

    impl LedgerFixture {
        fn open(name: &str) -> Self {
            let dir = temp_dir(name);
            let ledger = crate::ledger::Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
            let contract = contract();
            let contract_json = serde_json::to_string(&contract.canonical_value()).expect("json");
            Self {
                ledger,
                contract,
                contract_json,
                dir,
            }
        }

        /// Like [`Self::open`], but with a scripted clock — for a test
        /// that needs several runs at KNOWN, distinct timestamps rather
        /// than whatever the wall clock hands out.
        fn open_with_clock<I: IntoIterator<Item = S>, S: Into<String>>(
            name: &str,
            times: I,
        ) -> Self {
            let dir = temp_dir(name);
            let ledger = crate::ledger::Ledger::open_with_clock(
                &dir.join("ledger.sqlite"),
                Box::new(crate::ledger::FixedClock::new(times)),
            )
            .expect("ledger");
            let contract = contract();
            let contract_json = serde_json::to_string(&contract.canonical_value()).expect("json");
            Self {
                ledger,
                contract,
                contract_json,
                dir,
            }
        }

        fn dispatched(&self, run: &str, tier: &str, phase: &str) -> i64 {
            self.dispatched_for_task(run, task_id(run).as_str(), tier, phase)
        }

        /// Like [`Self::dispatched`], but for a caller that needs several
        /// runs to share one task — a retry, or a person's approval
        /// after an interruption.
        fn dispatched_for_task(&self, run: &str, task: &str, tier: &str, phase: &str) -> i64 {
            self.ledger
                .insert_run(&run_id(run), "/r", None, &TaskId::from_stored(task), "rk")
                .expect("run");
            let revision = self
                .ledger
                .insert_contract_revision(
                    &run_id(run),
                    &self.contract.hash(),
                    &self.contract_json,
                    "HEAD",
                    None,
                )
                .expect("revision");
            let attempt = self
                .ledger
                .insert_attempt(
                    &run_id(run),
                    revision,
                    1,
                    tier,
                    UsagePhase::parse(phase).expect("known phase"),
                )
                .expect("attempt");
            self.ledger
                .record_dispatch_intent(
                    &crate::ids::DispatchId::from_stored(format!("{run}-d1")),
                    &run_id(run),
                    Some(attempt),
                    &serde_json::json!({"model": "sonnet", "effort": "medium"}),
                    0,
                )
                .expect("intent");
            revision
        }

        fn usage(&self, event: &str, run: &str, model: &str) {
            use crate::money::{CostCompleteness, CostKind, MicroUsd};
            self.ledger
                .record_usage(&crate::ledger::UsageEvent {
                    event_id: event.into(),
                    run_id: run_id(run),
                    attempt_id: None,
                    parent_event_id: None,
                    model: Some(model.into()),
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    cost: Some(MicroUsd::from_micros(10)),
                    cost_kind: CostKind::ApiSpend,
                    completeness: CostCompleteness::Actual,
                    inclusive: false,
                    at: crate::ledger::now_rfc3339(),
                    phase: None,
                    duration_ms: None,
                    requested_model: None,
                    requested_effort: None,
                    harness: None,
                })
                .expect("usage");
        }

        fn settle(&self, run: &str, state: State) {
            self.ledger
                .record_transition(&crate::ledger::Transition {
                    run_id: run_id(run),
                    attempt_id: None,
                    from_state: Some(State::Verifying),
                    to_state: state,
                    reason: "test".into(),
                    detail: None,
                    at: crate::ledger::now_rfc3339(),
                })
                .expect("transition");
        }

        fn build(&self) -> Dataset {
            let contract_of = |run: &str| {
                let Some((contract, tier)) = self.ledger.run_contract_and_tier(&run_id(run))?
                else {
                    return Ok(None);
                };
                let objective = contract.objective.clone();
                Ok(Some((contract, objective, tier.as_str().to_string())))
            };
            build(&self.ledger, &contract_of, &repo_policy()).expect("dataset builds")
        }
    }

    impl Drop for LedgerFixture {
        fn drop(&mut self) {
            // Best effort: a leftover temp directory costs nothing, and
            // the next run of this test pre-cleans its own.
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    /// The sampling unit is the task, not the run (SPEC §17): a task
    /// retried to acceptance contributes ONE record, built from its most
    /// recent attempt, not one per attempt.
    #[test]
    fn a_task_retried_to_acceptance_contributes_one_record() {
        let times: Vec<String> = (0..30)
            .map(|minute| format!("2026-01-01T00:{minute:02}:00+00:00"))
            .collect();
        let fixture = LedgerFixture::open_with_clock("dataset-retry", times);
        fixture.dispatched_for_task("run-old", "task-shared", "implementation", "initial");
        fixture.settle("run-old", State::Failed);
        fixture.dispatched_for_task("run-new", "task-shared", "escalation", "escalation");
        fixture.usage("new-worker", "run-new", "fable");
        fixture.settle("run-new", State::Accepted);
        let dataset = fixture.build();
        assert_eq!(
            dataset.records.len(),
            1,
            "one task, however many times it was retried: {:?}",
            dataset.records
        );
        assert_eq!(
            dataset.records[0].tier,
            Tier::Escalation,
            "the later attempt represents it"
        );
        assert!(
            dataset
                .exclusions
                .iter()
                .any(|exclusion| exclusion.contains("run-old") && exclusion.contains("superseded")),
            "the superseded attempt is still named, not silently dropped: {:?}",
            dataset.exclusions
        );
    }

    /// A person's approval (`relais decide --answer approve`) accepts a
    /// candidate too, but it is not the verified acceptance a positive
    /// label requires — the example still enters the dataset, naming its
    /// route, but never contributes a positive label.
    #[test]
    fn a_person_approved_task_is_recorded_but_not_a_positive_label() {
        let fixture = LedgerFixture::open("dataset-person-approved");
        fixture.dispatched("run-a", "implementation", "initial");
        fixture
            .ledger
            .record_transition(&crate::ledger::Transition {
                run_id: run_id("run-a"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: "worker_dispatched".into(),
                detail: None,
                at: crate::ledger::now_rfc3339(),
            })
            .expect("transition into needs_decision opens a decision");
        fixture
            .ledger
            .resolve_decision(
                &run_id("run-a"),
                &crate::ledger::DecisionAnswer {
                    resolution: crate::lifecycle::Reason::DecisionApproved,
                    actor: "a person",
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsDecision,
                    to_state: State::Accepted,
                },
            )
            .expect("resolve");
        let dataset = fixture.build();
        assert_eq!(dataset.records.len(), 1);
        assert_eq!(dataset.records[0].route, AcceptanceRoute::PersonApproved);
        assert!(
            !dataset.records[0].accepted_without_escalation,
            "a person's approval is not the verified acceptance a positive label requires"
        );
    }

    #[test]
    fn a_reviewed_run_is_not_labelled_as_escalated() {
        let fixture = LedgerFixture::open("dataset");
        // Run A: one sonnet attempt, reviewed by fable. Two models, no
        // escalation.
        fixture.dispatched("run-a", "implementation", "initial");
        fixture.usage("a-worker", "run-a", "sonnet");
        fixture.usage("a-review", "run-a", "fable");
        fixture.settle("run-a", State::Accepted);
        assert_eq!(
            fixture.build().acceptance_labels(),
            ClassDistribution {
                records: 1,
                accepted_without_escalation: 1,
                other: 0
            },
            "a reviewer on another model is not an escalation"
        );
        // Run B: sonnet failed, fable rescued it. Escalated.
        let revision = fixture.dispatched("run-b", "implementation", "initial");
        fixture
            .ledger
            .insert_attempt(
                &run_id("run-b"),
                revision,
                2,
                "escalation",
                UsagePhase::Escalation,
            )
            .expect("attempt");
        fixture.usage("b-worker", "run-b", "sonnet");
        fixture.usage("b-fable", "run-b", "fable");
        fixture.settle("run-b", State::Accepted);
        assert_eq!(
            fixture.build().acceptance_labels(),
            ClassDistribution {
                records: 2,
                accepted_without_escalation: 1,
                other: 1
            },
            "a rescue by the stronger tier is"
        );
    }

    /// L1: every non-terminal state, and every terminal one that is not a
    /// reasoning verdict, is EXCLUDED with a reason. They used to fall
    /// through a `_ => {}` arm into the negative class, so a run that was
    /// merely still running taught the learner that its tier fails.
    #[test]
    fn only_a_reasoning_verdict_is_labelled() {
        for state in [
            State::Prepared,
            State::Running,
            State::Verifying,
            State::Repairing,
            State::Escalating,
            State::BudgetExhausted,
            State::Blocked,
            State::Interrupted,
            State::NeedsReview,
            State::NeedsDecision,
            State::Cancelled,
        ] {
            assert!(
                matches!(labelling_of(state, None), Labelling::Excluded(_)),
                "{state} is not an evidence-backed reasoning label"
            );
            let fixture = LedgerFixture::open("dataset-state");
            fixture.dispatched("run-x", "implementation", "initial");
            fixture.settle("run-x", state);
            let dataset = fixture.build();
            assert!(
                dataset.records.is_empty(),
                "{state} produced a training record"
            );
            assert_eq!(dataset.exclusions.len(), 1, "{state} is excluded silently");
            assert!(
                dataset.exclusions[0].contains(&state.to_string()),
                "the exclusion names the state: {}",
                dataset.exclusions[0]
            );
        }
        assert_eq!(labelling_of(State::Accepted, None), Labelling::Accepted);
        assert_eq!(labelling_of(State::Failed, None), Labelling::Failed);
    }

    /// D1: a task whose latest recorded outcome is `reverted` or a
    /// confirmed regression is never a positive label, whatever its run's
    /// terminal state said.
    #[test]
    fn a_reverted_or_regressed_outcome_withdraws_an_accepted_label() {
        assert_eq!(
            labelling_of(State::Accepted, Some(OutcomeKind::Reverted)),
            Labelling::Failed
        );
        assert_eq!(
            labelling_of(State::Accepted, Some(OutcomeKind::ConfirmedRegression)),
            Labelling::Failed
        );
        assert_eq!(
            labelling_of(State::Accepted, Some(OutcomeKind::AcceptedUnchanged)),
            Labelling::Accepted
        );
        assert_eq!(
            labelling_of(State::Accepted, Some(OutcomeKind::Corrected)),
            Labelling::Accepted
        );
        // Absence of feedback is never a positive label BY ITSELF, but it
        // also never withdraws one already earned by verified acceptance.
        assert_eq!(labelling_of(State::Accepted, None), Labelling::Accepted);
        // A non-accepted state is untouched by outcome: there is no
        // accepted change for an outcome to speak about.
        assert_eq!(
            labelling_of(State::Failed, Some(OutcomeKind::Reverted)),
            Labelling::Failed
        );

        // The label and the `outcome_stood` an example records must agree
        // for EVERY kind, not only the four spelled out above: they are
        // one judgement read twice, and a kind added later could be
        // classified one way in the label and the other in the example.
        for kind in OutcomeKind::ALL {
            assert_eq!(
                labelling_of(State::Accepted, Some(kind)) == Labelling::Accepted,
                outcome_stands(kind),
                "the label and `outcome_stands` disagree about {kind:?}"
            );
        }
    }

    /// L4: a failing ledger read is an error, never an empty dataset. The
    /// closure stands in for the ledger going away mid-build.
    #[test]
    fn a_failed_contract_read_stops_the_build() {
        let fixture = LedgerFixture::open("dataset-err");
        fixture.dispatched("run-a", "implementation", "initial");
        fixture.settle("run-a", State::Accepted);
        let failing = |run_id: &str| -> Result<Option<ContractMaterial>, LedgerError> {
            Err(LedgerError::Corrupt {
                what: format!("contract of {run_id}"),
                detail: "disk gave up".into(),
            })
        };
        let error = build(&fixture.ledger, &failing, &repo_policy()).expect_err("refuses");
        assert!(
            matches!(
                &error,
                DatasetError::LedgerRead {
                    run,
                    query: "contract revision",
                    ..
                } if run == "run-a"
            ),
            "{error}"
        );
        assert!(error.to_string().contains("disk gave up"), "{error}");
    }

    /// L4/L8: a run with no dispatch intent has no model to attribute its
    /// outcome to. It is excluded, not credited to the default identity.
    #[test]
    fn a_run_without_a_dispatch_intent_is_excluded() {
        let fixture = LedgerFixture::open("dataset-nointent");
        fixture
            .ledger
            .insert_run(&run_id("run-a"), "/r", None, &task_id("run-a"), "rk")
            .expect("run");
        let revision = fixture
            .ledger
            .insert_contract_revision(
                &run_id("run-a"),
                &fixture.contract.hash(),
                &fixture.contract_json,
                "HEAD",
                None,
            )
            .expect("revision");
        fixture
            .ledger
            .insert_attempt(
                &run_id("run-a"),
                revision,
                1,
                "implementation",
                UsagePhase::Initial,
            )
            .expect("attempt");
        fixture.settle("run-a", State::Accepted);
        let dataset = fixture.build();
        assert!(dataset.records.is_empty());
        assert!(
            dataset.exclusions[0].contains("no dispatch intent"),
            "{:?}",
            dataset.exclusions
        );
    }

    /// The floor recorded per record is the route the conservative
    /// baseline would have taken for that contract (SPEC §6).
    #[test]
    fn each_record_carries_its_own_routing_floor() {
        let fixture = LedgerFixture::open("dataset-floor");
        fixture.dispatched("run-a", "implementation", "initial");
        fixture.usage("a-worker", "run-a", "sonnet");
        fixture.settle("run-a", State::Accepted);
        let dataset = fixture.build();
        assert_eq!(dataset.version, DATASET_VERSION);
        assert_eq!(dataset.records.len(), 1);
        assert_eq!(dataset.records[0].floor, Tier::Implementation);
        assert_eq!(
            dataset.records[0].identity,
            ProfileIdentity {
                model: "sonnet".into(),
                effort: Some("medium".into()),
                harness: None,
            }
        );
    }

    #[test]
    fn families_group_reworded_retries_but_not_different_tasks() {
        let base = contract();
        let mut reworded = base.clone();
        reworded.objective = "the escaping: fix".into();
        assert_eq!(task_family(&base), task_family(&reworded));
        let mut other = base.clone();
        other.objective = "Add a --json flag".into();
        assert_ne!(task_family(&base), task_family(&other));
        let mut elsewhere = base.clone();
        elsewhere.task = crate::contract::Task::change(vec!["docs/**".into()]).expect("compiles");
        assert_ne!(task_family(&base), task_family(&elsewhere));
    }

    #[test]
    fn tier_names_round_trip() {
        assert_eq!(Tier::parse("escalation"), Some(Tier::Escalation));
        assert_eq!(Tier::parse("nonsense"), None);
    }

    #[test]
    fn class_distribution_is_reported() {
        let dataset = Dataset {
            version: DATASET_VERSION,
            label_policy_version: LABEL_POLICY_VERSION,
            records: vec![
                example("a", "2026-09-01", true),
                example("b", "2026-09-02", false),
            ],
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        assert_eq!(
            dataset.acceptance_labels(),
            ClassDistribution {
                records: 2,
                accepted_without_escalation: 1,
                other: 1
            }
        );
    }
}
