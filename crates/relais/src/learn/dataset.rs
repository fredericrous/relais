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
    expand, FeatureSchema, ProfileIdentity, SparseVec, TaskFeatures, TrainingExample,
};
use crate::ids::sha256_hex;
use crate::ledger::Ledger;
use crate::lifecycle::State;
use crate::policy::{RepoPolicy, Tier};

/// The stored contract and first-attempt tier for a run, as dataset
/// construction consumes them.
pub type ContractMaterial = (crate::contract::TaskContract, String, String);

pub const DATASET_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dataset {
    pub version: u32,
    pub records: Vec<TrainingExample>,
    pub exclusions: Vec<String>,
    pub fingerprint: String,
    pub built_at: String,
}

impl Dataset {
    pub fn acceptance_labels(&self) -> (Vec<&TrainingExample>, usize, usize) {
        let positives = self
            .records
            .iter()
            .filter(|record| record.accepted_without_escalation)
            .count();
        (
            self.records.iter().collect(),
            positives,
            self.records.len() - positives,
        )
    }
}

/// The features recorded at dispatch time for a run's first attempt,
/// reconstructed from what the runner persisted BEFORE the worker ran.
/// The objective text is read back from the stored contract, and the
/// task-level features plus tier/model identity are the ONLY inputs —
/// outcomes, patch sizes and later failures never enter (SPEC §21).
pub fn build(
    ledger: &Ledger,
    contract_of: &dyn Fn(&str) -> Option<ContractMaterial>,
    repo_policy: &RepoPolicy,
) -> Dataset {
    let schema = FeatureSchema::standard();
    let mut records = Vec::new();
    let mut exclusions = Vec::new();
    let runs = ledger
        .runs_since("2000-01-01T00:00:00+00:00")
        .unwrap_or_default();
    for (run_id, _repo, _status, _created) in runs {
        let Some((contract, objective, tier_name)) = contract_of(&run_id) else {
            exclusions.push(format!("{run_id}: no contract revision recorded"));
            continue;
        };
        let Some(state) = ledger.run_status(&run_id).unwrap_or(None) else {
            exclusions.push(format!("{run_id}: no state recorded"));
            continue;
        };
        // Infrastructure and unresolved outcomes are excluded, not
        // relabelled (SPEC §17).
        match state {
            State::Blocked => {
                exclusions.push(format!(
                    "{run_id}: blocked environment, not a reasoning label"
                ));
                continue;
            }
            State::Interrupted => {
                exclusions.push(format!("{run_id}: interrupted, outcome unknown"));
                continue;
            }
            State::NeedsDecision | State::NeedsReview | State::Cancelled => {
                exclusions.push(format!("{run_id}: {state}, no evidence-backed label yet"));
                continue;
            }
            _ => {}
        }
        let Some(tier) = Tier::from_name(&tier_name) else {
            exclusions.push(format!("{run_id}: unknown tier `{tier_name}`"));
            continue;
        };
        // A cheap worker rescued by a stronger worker did not succeed
        // without escalation. The attempt table's phase says whether the
        // ladder moved; the set of models seen does NOT — a reviewer at
        // the escalation tier is not an escalation, and counting it as
        // one labelled every reviewed run a failure of its worker.
        let escalated = ledger.escalation_attempted(&run_id).unwrap_or(true);
        let accepted_without_escalation = state == State::Accepted && !escalated;
        let cost = ledger
            .run_cost(&run_id)
            .unwrap_or(crate::money::MicroUsd::ZERO);
        let completeness = ledger
            .run_cost_completeness(&run_id)
            .unwrap_or(crate::money::CostCompleteness::Unknown);
        let cost_complete = completeness == crate::money::CostCompleteness::Actual;
        let task = TaskFeatures::extract(&contract, repo_policy);
        let identity = ledger
            .first_dispatch_intent(&run_id)
            .ok()
            .flatten()
            .map(|intent| ProfileIdentity {
                model: intent["model"].as_str().unwrap_or("unknown").to_string(),
                effort: intent["effort"].as_str().map(str::to_string),
                harness: intent["harness"].as_str().map(str::to_string),
            })
            .unwrap_or_default();
        let sparse: SparseVec = expand(&task, tier, &objective, &identity, &schema);
        let dispatched_at = ledger
            .transitions(&run_id)
            .unwrap_or_default()
            .first()
            .map(|transition| transition.at.clone())
            .unwrap_or_default();
        records.push(TrainingExample {
            family: task_family(&contract),
            tier,
            task,
            objective: objective.clone(),
            identity,
            sparse,
            accepted_without_escalation,
            complete_cost: cost,
            cost_complete,
            dispatched_at,
        });
    }
    let fingerprint_source = serde_json::to_string(&records).unwrap_or_default();
    let fingerprint = sha256_hex(fingerprint_source.as_bytes());
    Dataset {
        version: DATASET_VERSION,
        records,
        exclusions,
        fingerprint,
        built_at: crate::ledger::now_rfc3339(),
    }
}

/// The family a contract belongs to, for split grouping (SPEC §17:
/// "avoid counting many retries or near-identical task variants as
/// independent examples"). Kind, scope and the objective's token SET
/// — not its exact text, so a re-worded retry of the same task stays with
/// the original, while a different task on the same scope does not.
pub fn task_family(contract: &crate::contract::TaskContract) -> String {
    let mut tokens: Vec<String> = contract
        .objective
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    tokens.sort();
    tokens.dedup();
    let mut scope = contract.write_scope.clone().unwrap_or_default();
    scope.sort();
    sha256_hex(
        serde_json::json!({
            "kind": contract.kind,
            "scope": scope,
            "tokens": tokens,
        })
        .to_string()
        .as_bytes(),
    )
}

impl Tier {
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "research" => Tier::Research,
            "implementation" => Tier::Implementation,
            "escalation" => Tier::Escalation,
            _ => return None,
        })
    }
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

    fn example(family: &str, at: &str, accepted: bool) -> TrainingExample {
        TrainingExample {
            family: family.into(),
            tier: Tier::Implementation,
            task: TaskFeatures::extract(&contract(), &repo_policy()),
            objective: "objective".into(),
            identity: ProfileIdentity::default(),
            sparse: SparseVec(vec![(0, 1.0)]),
            accepted_without_escalation: accepted,
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

    #[test]
    fn a_reviewed_run_is_not_labelled_as_escalated() {
        use crate::ledger::{Ledger, Transition, UsageEvent};
        use crate::money::{CostCompleteness, CostKind, MicroUsd};
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "relais-dataset-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let contract = contract();
        let contract_json = serde_json::to_string(&contract.canonical_value()).expect("json");
        let usage = |event: &str, run: &str, model: &str| UsageEvent {
            event_id: event.into(),
            run_id: run.into(),
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
        };
        let accept = |run: &str| {
            ledger
                .record_transition(&Transition {
                    run_id: run.into(),
                    attempt_id: None,
                    from_state: Some(State::Verifying),
                    to_state: State::Accepted,
                    reason: "checks_and_review_passed".into(),
                    detail: None,
                    at: crate::ledger::now_rfc3339(),
                })
                .expect("transition");
        };
        let contract_of = |run_id: &str| {
            ledger
                .run_contract_and_tier(run_id)
                .ok()
                .flatten()
                .and_then(|(json, objective, tier)| {
                    crate::contract::TaskContract::from_json_str(&json)
                        .ok()
                        .map(|c| (c, objective, tier))
                })
        };
        // Run A: one sonnet attempt, reviewed by fable. Two models, no
        // escalation.
        ledger.insert_run("run-a", "/r", None).expect("run");
        let revision = ledger
            .insert_contract_revision("run-a", &contract.hash(), &contract_json, "HEAD", None)
            .expect("revision");
        ledger
            .insert_attempt("run-a", revision, 1, "implementation", "initial")
            .expect("attempt");
        ledger
            .record_usage(&usage("a-worker", "run-a", "sonnet"))
            .expect("usage");
        ledger
            .record_usage(&usage("a-review", "run-a", "fable"))
            .expect("usage");
        accept("run-a");
        let (_, positives, negatives) =
            build(&ledger, &contract_of, &repo_policy()).acceptance_labels();
        assert_eq!(
            (positives, negatives),
            (1, 0),
            "a reviewer on another model is not an escalation"
        );
        // Run B: sonnet failed, fable rescued it. Escalated.
        ledger.insert_run("run-b", "/r", None).expect("run");
        let revision = ledger
            .insert_contract_revision("run-b", &contract.hash(), &contract_json, "HEAD", None)
            .expect("revision");
        ledger
            .insert_attempt("run-b", revision, 1, "implementation", "initial")
            .expect("attempt");
        ledger
            .insert_attempt("run-b", revision, 2, "escalation", "escalation")
            .expect("attempt");
        ledger
            .record_usage(&usage("b-worker", "run-b", "sonnet"))
            .expect("usage");
        ledger
            .record_usage(&usage("b-fable", "run-b", "fable"))
            .expect("usage");
        accept("run-b");
        let (_, positives, negatives) =
            build(&ledger, &contract_of, &repo_policy()).acceptance_labels();
        assert_eq!(
            (positives, negatives),
            (1, 1),
            "a rescue by the stronger tier is"
        );
        std::fs::remove_dir_all(&dir).ok();
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
        elsewhere.write_scope = Some(vec!["docs/**".into()]);
        assert_ne!(task_family(&base), task_family(&elsewhere));
    }

    #[test]
    fn tier_names_round_trip() {
        assert_eq!(Tier::from_name("escalation"), Some(Tier::Escalation));
        assert_eq!(Tier::from_name("nonsense"), None);
    }

    #[test]
    fn class_distribution_is_reported() {
        let dataset = Dataset {
            version: DATASET_VERSION,
            records: vec![
                example("a", "2026-09-01", true),
                example("b", "2026-09-02", false),
            ],
            exclusions: vec![],
            fingerprint: "f".into(),
            built_at: "now".into(),
        };
        let (_, positives, negatives) = dataset.acceptance_labels();
        assert_eq!((positives, negatives), (1, 1));
    }
}
