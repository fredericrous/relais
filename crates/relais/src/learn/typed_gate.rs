//! The typed-research promotion gate (docs/plans/2026-10-12-task-types.md,
//! Phase 3): the one thing that can authorise a research-tier `change`
//! recipe. The generic comparison gates (sample support and comparability
//! in [`super::comparison`]) cannot: they count paired tasks, not whether
//! each trial ran this recipe, on a task of this type, in this arm, under
//! the research model.
//!
//! The count is recomputed from the ledger's trials on every call, from
//! QUALIFYING trials only. A qualifying trial
//!
//! - ran exactly the candidate's recipe id,
//! - on a task of one of the types the recipe names,
//! - in the recipe's own arm (a design was handed exactly when the recipe
//!   `requires_design`),
//! - with the research model dispatched first,
//! - and ended accepted without escalation (a success), or failed on
//!   reasoning or escalated (a failure).
//!
//! Anything else counts neither way: other recipes, other types, other
//! arms and other models are not this arm's evidence, and an invalid or
//! errored trial (an infrastructure stop) is not a verdict. A task counts
//! once, by its earliest qualifying trial. The bar is read at the first
//! 15 qualifying trials and never earlier, so a run of successes cannot be
//! stopped on while it is ahead; later trials do not move it.
//!
//! A trial's recorded outcome is taken as is ([`judge`]), except that a
//! review-rejected replay a person judged `pass` against the source work
//! (`relais dataset judge-trial`, plan D9) counts as a success.

use crate::contract::TaskType;
use crate::ids::{TaskId, TrialId};
use crate::ledger::{GroundTruthVerdict, Ledger, LedgerError, TrialOutcome, TrialRow};
use crate::policy::{RecipeSpec, RepoPolicy};
use crate::route::{
    is_typed_research_change, TypedGateCount, TypedGateEvidence, TYPED_GATE_TRIALS,
};

/// The gate's name, as the ledger records it beside the trials it counted.
pub const GATE_NAME: &str = "typed-research";

/// The model a recipe's first attempt must dispatch: the recipe's own
/// `models[tier]`, else the policy's (the profile the router's ladder
/// resolves for a covering recipe).
pub fn arm_model<'a>(policy: &'a RepoPolicy, spec: &'a RecipeSpec) -> Option<&'a str> {
    spec.models
        .as_ref()
        .and_then(|models| models.get(&spec.tier))
        .or_else(|| policy.models.get(&spec.tier))
        .map(|profile| profile.id.as_str())
}

/// What the ledger says about one trial beyond its own row.
#[derive(Debug, Clone, PartialEq)]
pub struct TrialFacts {
    pub trial: TrialRow,
    /// The type the trial's task carries, if any.
    pub task_type: Option<TaskType>,
    /// The model the arm run's first dispatch named, if any.
    pub first_model: Option<String>,
}

/// What one trial is worth to the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Judgement {
    /// Accepted without escalation.
    Success,
    /// Failed on reasoning or escalated.
    Failure,
    /// Not this arm's evidence, or not a verdict.
    NotCounted,
}

/// Judge one trial against the recipe, one of its types and the model its
/// first attempt must dispatch.
pub fn judge(
    facts: &TrialFacts,
    spec: &RecipeSpec,
    task_type: TaskType,
    research_model: Option<&str>,
) -> Judgement {
    let trial = &facts.trial;
    let in_arm = trial.arm_recipe_id == spec.recipe_id()
        && facts.task_type == Some(task_type)
        && spec.requires_design == trial.design_sha256.is_some();
    let research_first = match (research_model, facts.first_model.as_deref()) {
        (Some(expected), Some(dispatched)) => expected == dispatched,
        _ => false,
    };
    if !in_arm || !research_first {
        return Judgement::NotCounted;
    }
    // A rejected replay counts as a failure unless a person judged it pass.
    let judged_pass = trial
        .ground_truth
        .as_ref()
        .is_some_and(|truth| truth.verdict == GroundTruthVerdict::Pass);
    match (trial.outcome, trial.accepted_without_escalation) {
        (Some(TrialOutcome::Accepted), Some(true)) => Judgement::Success,
        (Some(TrialOutcome::Rejected), _) if judged_pass => Judgement::Success,
        (Some(TrialOutcome::Accepted), Some(false)) | (Some(TrialOutcome::Rejected), _) => {
            Judgement::Failure
        }
        (Some(TrialOutcome::Accepted), None)
        | (Some(TrialOutcome::Errored | TrialOutcome::Invalid), _)
        | (None, _) => Judgement::NotCounted,
    }
}

/// One recipe's standing for one of its task types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeStanding {
    pub task_type: TaskType,
    pub count: TypedGateCount,
    /// The first [`TYPED_GATE_TRIALS`] qualifying trials, in order: what
    /// the bar was read from, and what authorises a promotion that passes.
    pub counted: Vec<TrialId>,
}

/// The standing of `spec` for `task_type` among `trials` (oldest first).
pub fn standing(
    trials: &[TrialFacts],
    spec: &RecipeSpec,
    task_type: TaskType,
    research_model: Option<&str>,
) -> TypeStanding {
    let mut seen: Vec<&TaskId> = Vec::new();
    let mut qualifying: Vec<(&TrialId, bool)> = Vec::new();
    for facts in trials {
        let success = match judge(facts, spec, task_type, research_model) {
            Judgement::NotCounted => continue,
            Judgement::Success => true,
            Judgement::Failure => false,
        };
        if seen.contains(&&facts.trial.task_id) {
            continue;
        }
        seen.push(&facts.trial.task_id);
        qualifying.push((&facts.trial.trial_id, success));
    }
    let read = &qualifying[..qualifying.len().min(TYPED_GATE_TRIALS)];
    TypeStanding {
        task_type,
        count: TypedGateCount {
            qualifying: qualifying.len(),
            accepted: read.iter().filter(|(_, success)| *success).count(),
        },
        counted: read.iter().map(|(id, _)| (*id).clone()).collect(),
    }
}

/// The trials that authorised one recipe revision for one type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorisation {
    pub recipe_id: String,
    pub name: String,
    pub revision: u32,
    pub task_type: TaskType,
    pub trial_ids: Vec<TrialId>,
}

/// The gate's reading of a candidate: the counts for admission and, for
/// each recipe and type that passes, the trials to record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assessment {
    pub evidence: TypedGateEvidence,
    pub authorisations: Vec<Authorisation>,
}

/// The gate's reading of the recipes `candidate` adds beyond `incumbent`
/// whose shape is a typed research change. A recipe naming several types
/// must clear the bar for each; its count is the first type that does not,
/// else the last.
pub fn assess(
    ledger: &Ledger,
    incumbent: &RepoPolicy,
    candidate: &RepoPolicy,
) -> Result<Assessment, LedgerError> {
    let mut assessment = Assessment::default();
    let added = candidate.recipes.iter().skip(incumbent.recipes.len());
    for spec in added.filter(|spec| is_typed_research_change(spec)) {
        let recipe_id = spec.recipe_id();
        let trials = facts_of(ledger, &recipe_id)?;
        let research_model = arm_model(candidate, spec);
        let standings: Vec<TypeStanding> = spec
            .task_type
            .iter()
            .map(|task_type| standing(&trials, spec, *task_type, research_model))
            .collect();
        let representative = standings
            .iter()
            .find(|standing| !standing.count.passes())
            .or_else(|| standings.last());
        if let Some(representative) = representative {
            assessment
                .evidence
                .insert(&spec.name, spec.revision, representative.count);
        }
        if standings.iter().all(|standing| standing.count.passes()) {
            assessment
                .authorisations
                .extend(standings.into_iter().map(|standing| Authorisation {
                    recipe_id: recipe_id.clone(),
                    name: spec.name.clone(),
                    revision: spec.revision,
                    task_type: standing.task_type,
                    trial_ids: standing.counted,
                }));
        }
    }
    Ok(assessment)
}

/// Every settled trial of `recipe_id`, oldest first, with its task's type
/// and its arm run's first dispatched model.
fn facts_of(ledger: &Ledger, recipe_id: &str) -> Result<Vec<TrialFacts>, LedgerError> {
    let ids = std::iter::once(recipe_id.to_string()).collect();
    ledger
        .settled_trials_for_recipes(&ids)?
        .into_iter()
        .map(|trial| {
            let task_type = ledger
                .task_type_of(&trial.task_id)?
                .map(|stored| stored.task_type);
            let first_model = match &trial.arm_run_id {
                Some(run) => ledger
                    .first_dispatch_intent(run)?
                    .and_then(|intent| intent.get("model")?.as_str().map(str::to_string)),
                None => None,
            };
            Ok(TrialFacts {
                trial,
                task_type,
                first_model,
            })
        })
        .collect()
}

/// Record the trials that authorised a promotion, once it is written.
pub fn record(ledger: &Ledger, assessment: &Assessment) -> Result<(), LedgerError> {
    for authorisation in &assessment.authorisations {
        ledger.record_recipe_promotion(&crate::ledger::NewRecipePromotion {
            recipe_id: &authorisation.recipe_id,
            recipe_name: &authorisation.name,
            revision: authorisation.revision,
            task_type: authorisation.task_type.as_str(),
            gate: GATE_NAME,
            trial_ids: &authorisation.trial_ids,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RunId;
    use crate::ledger::{NewReplayTrial, TrialCost};
    use crate::route::{
        validate_candidate, CandidateAdmission, CandidateRejection, TypedGateCount,
        TYPED_GATE_ACCEPTED,
    };
    use crate::router::stats::{wilson, Z95};

    const INCUMBENT: &str = r#"schema_version = 1
[models.research]
id = "haiku"
[models.implementation]
id = "sonnet"
[[recipes]]
name = "change-feature"
revision = 1
kind = "change"
scope_within = ["**"]
task_type = ["feature"]
tier = "implementation"
[[recipes]]
name = "change-feature-designed"
revision = 1
kind = "change"
scope_within = ["**"]
task_type = ["feature"]
requires_design = true
tier = "implementation"
[[recipes]]
name = "change-ui"
revision = 1
kind = "change"
scope_within = ["**"]
task_type = ["ui"]
tier = "implementation"
"#;

    const CANDIDATES: &str = r#"
[[recipes]]
name = "change-feature"
revision = 2
kind = "change"
scope_within = ["**"]
task_type = ["feature"]
tier = "research"
[[recipes]]
name = "change-feature-designed"
revision = 2
kind = "change"
scope_within = ["**"]
task_type = ["feature"]
requires_design = true
tier = "research"
"#;

    struct World {
        ledger: Ledger,
        incumbent: RepoPolicy,
        candidate: RepoPolicy,
        _dir: crate::test_support::TempDir,
    }

    fn world() -> World {
        let dir = crate::test_support::short_temp_dir("typed-gate");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger opens");
        World {
            ledger,
            incumbent: RepoPolicy::from_toml_str(INCUMBENT).expect("incumbent parses"),
            candidate: RepoPolicy::from_toml_str(&format!("{INCUMBENT}{CANDIDATES}"))
                .expect("candidate parses"),
            _dir: dir,
        }
    }

    /// What a trial is, apart from its id: the knobs a disqualifier moves.
    struct Spec {
        recipe: usize,
        task_type: TaskType,
        design: bool,
        model: &'static str,
        outcome: TrialOutcome,
        without_escalation: bool,
    }

    impl Spec {
        /// The plain candidate `change-feature` rev 2: a qualifying
        /// success unless `tweak` moves it.
        fn tweaked(tweak: impl FnOnce(&mut Spec)) -> Spec {
            let mut spec = Spec {
                recipe: 3,
                task_type: TaskType::Feature,
                design: false,
                model: "haiku",
                outcome: TrialOutcome::Accepted,
                without_escalation: true,
            };
            tweak(&mut spec);
            spec
        }
    }

    impl World {
        fn recipe_id(&self, index: usize) -> String {
            self.candidate.recipes[index].recipe_id()
        }

        /// The plain candidate `change-feature` rev 2 and its settled
        /// trial on `task-<n>`: a qualifying success unless `tweak` moves it.
        fn trial(&self, n: usize, tweak: impl FnOnce(&mut Spec)) {
            let spec = Spec::tweaked(tweak);
            let task = TaskId::from_stored(format!("task-{n}"));
            let source = RunId::from_stored(format!("source-{n}"));
            self.ledger
                .insert_typed_run(&source, "/r", None, &task, "rk", Some(spec.task_type))
                .expect("task");
            self.settled(n, &format!("{n:03}"), &spec);
        }

        /// Another settled trial `trial-<id>` of the task `trial(n, ..)`
        /// already made, recorded after every trial before it.
        fn retrial(&self, n: usize, id: &str, tweak: impl FnOnce(&mut Spec)) {
            self.settled(n, id, &Spec::tweaked(tweak));
        }

        fn settled(&self, n: usize, id: &str, spec: &Spec) {
            let task = TaskId::from_stored(format!("task-{n}"));
            let source = RunId::from_stored(format!("source-{n}"));
            let trial_id = TrialId::from_stored(format!("trial-{id}"));
            let arm_run = RunId::from_stored(format!("arm-{id}"));
            let arm = self.recipe_id(spec.recipe);
            self.ledger
                .insert_replay_trial(&NewReplayTrial {
                    trial_id: &trial_id,
                    task_id: &task,
                    source_run_id: &source,
                    incumbent_recipe_id: "incumbent",
                    arm_recipe_id: &arm,
                    base_sha: "base",
                    contract_hash: "contract",
                    verification_profile_hash: "profile",
                    workspace_isolation: "fresh_checkout_no_accepted_answer",
                    arm_run_id: &arm_run,
                    design_sha256: spec.design.then_some("design-sha"),
                })
                .expect("trial");
            self.ledger
                .record_dispatch_intent(
                    &crate::ids::DispatchId::from_stored(format!("d-{id}")),
                    &arm_run,
                    None,
                    &serde_json::json!({ "model": spec.model }),
                    0,
                    crate::route::RoutedBy::DeterministicRecipe,
                )
                .expect("dispatch");
            self.ledger
                .settle_trial(
                    &trial_id,
                    spec.outcome,
                    spec.without_escalation,
                    TrialCost::UNKNOWN,
                    1,
                )
                .expect("settle");
        }

        /// `n` qualifying trials on tasks `from..from + n`, the first
        /// `failures` of them failing on reasoning.
        fn qualifying(&self, from: usize, n: usize, failures: usize) {
            for k in 0..n {
                self.trial(from + k, |spec| {
                    if k < failures {
                        spec.outcome = TrialOutcome::Rejected;
                        spec.without_escalation = false;
                    }
                });
            }
        }

        fn count(&self) -> TypedGateCount {
            let assessment = assess(&self.ledger, &self.incumbent, &self.candidate).expect("reads");
            assessment.evidence.count_of("change-feature", 2)
        }

        /// The candidate's admission through the promotion door with the
        /// gate's recomputed evidence: `recipe promote`'s own first step.
        fn promote(&self) -> Result<(), CandidateRejection> {
            let assessment = assess(&self.ledger, &self.incumbent, &self.candidate).expect("reads");
            let mut candidate = self.incumbent.clone();
            candidate.recipes.push(self.candidate.recipes[3].clone());
            validate_candidate(
                &self.incumbent,
                &candidate,
                &crate::route::default_tuning_bounds(
                    &self.incumbent,
                    None,
                    &crate::catalog::EffortCatalogs::default(),
                ),
                CandidateAdmission::PromoteGated(&assessment.evidence),
            )
            .map(|_| ())
        }
    }

    #[test]
    fn fourteen_of_fifteen_is_the_wilson_bar() {
        assert_eq!((TYPED_GATE_TRIALS, TYPED_GATE_ACCEPTED), (15, 14));
        assert!(wilson(14, 15, Z95).0 >= 0.7017);
        assert!(wilson(13, 15, Z95).0 < 0.7017);
    }

    #[test]
    fn fourteen_of_fifteen_qualifying_successes_authorise_promotion() {
        let w = world();
        w.qualifying(0, 15, 1);
        assert_eq!(
            w.count(),
            TypedGateCount {
                qualifying: 15,
                accepted: 14
            }
        );
        w.promote().expect("14/15 promotes");
    }

    #[test]
    fn thirteen_of_fifteen_and_fourteen_of_fourteen_do_not() {
        let w = world();
        w.qualifying(0, 15, 2);
        assert!(matches!(
            w.promote(),
            Err(CandidateRejection::TypedResearchGate { .. })
        ));
        let w = world();
        w.qualifying(0, 14, 0);
        let refusal = w.promote().unwrap_err().to_string();
        assert_eq!(
            refusal,
            "typed-research gate: change-feature@2 has 14 qualifying trials, needs 15 with at \
             least 14 accepted without escalation"
        );
    }

    #[test]
    fn the_bar_is_read_at_the_first_fifteen_and_later_trials_never_move_it() {
        let w = world();
        w.qualifying(0, 15, 2);
        w.qualifying(15, 10, 0);
        assert_eq!(
            w.count(),
            TypedGateCount {
                qualifying: 25,
                accepted: 13
            }
        );
        assert!(w.promote().is_err(), "ten later successes do not rescue it");
    }

    #[test]
    fn a_task_counts_once_by_its_earliest_qualifying_trial() {
        let w = world();
        w.qualifying(0, 15, 0);
        // A later qualifying trial of task-0 that failed: the earlier
        // success decides the task, and the failure is not counted.
        w.retrial(0, "900", |spec| {
            spec.outcome = TrialOutcome::Rejected;
            spec.without_escalation = false;
        });
        assert_eq!(
            w.ledger
                .trials_by_task(&TaskId::from_stored("task-0"))
                .expect("trials")
                .len(),
            2
        );
        assert_eq!(
            w.count(),
            TypedGateCount {
                qualifying: 15,
                accepted: 15
            }
        );
        let assessment = assess(&w.ledger, &w.incumbent, &w.candidate).expect("reads");
        let counted = &assessment.authorisations[0].trial_ids;
        assert!(counted.contains(&TrialId::from_stored("trial-000")));
        assert!(!counted.contains(&TrialId::from_stored("trial-900")));
    }

    /// Fourteen real successes plus one trial that must not be the
    /// fifteenth: each disqualifier leaves the arm at 14 qualifying trials.
    #[test]
    fn trials_that_are_not_this_arms_evidence_cannot_authorise() {
        type Tweak = fn(&mut Spec);
        let cases: [(&str, Tweak); 8] = [
            ("mismatched model", |s| s.model = "sonnet"),
            ("other type", |s| s.task_type = TaskType::Ui),
            ("other arm (design handed)", |s| s.design = true),
            ("other recipe id", |s| s.recipe = 4),
            ("errored", |s| s.outcome = TrialOutcome::Errored),
            ("invalid", |s| s.outcome = TrialOutcome::Invalid),
            ("an unrelated recipe", |s| s.recipe = 2),
            ("the incumbent", |s| s.recipe = 0),
        ];
        for (name, tweak) in cases {
            let w = world();
            w.qualifying(0, 14, 0);
            w.trial(99, tweak);
            assert_eq!(w.count().qualifying, 14, "{name}");
            assert!(w.promote().is_err(), "{name} must not authorise");
        }
    }

    #[test]
    fn failed_and_escalated_trials_count_as_failures_in_the_denominator() {
        let w = world();
        w.qualifying(0, 14, 0);
        w.trial(50, |s| {
            s.outcome = TrialOutcome::Rejected;
            s.without_escalation = false;
        });
        assert_eq!(
            w.count(),
            TypedGateCount {
                qualifying: 15,
                accepted: 14
            }
        );
        let w = world();
        w.qualifying(0, 14, 0);
        // Accepted, but only after escalating: a failure.
        w.trial(50, |s| s.without_escalation = false);
        assert_eq!(w.count().accepted, 14);
        assert_eq!(w.count().qualifying, 15);
        // 13 successes and two failures: 13/15.
        let w = world();
        w.qualifying(0, 13, 0);
        w.trial(50, |s| s.without_escalation = false);
        w.trial(51, |s| s.outcome = TrialOutcome::Rejected);
        assert_eq!(
            w.count(),
            TypedGateCount {
                qualifying: 15,
                accepted: 13
            }
        );
        assert!(w.promote().is_err());
    }

    /// Judge the rejected trial of task `n` with `verdict`.
    fn judge_rejected(w: &World, n: usize, verdict: GroundTruthVerdict) {
        w.ledger
            .judge_trial(
                &TrialId::from_stored(format!("trial-{n:03}")),
                verdict,
                "evidence",
            )
            .expect("reads")
            .expect("recorded");
    }

    #[test]
    fn a_rejected_replay_judged_pass_counts_and_judged_fail_or_unjudged_does_not() {
        // 13 accepted + 1 rejected judged pass + 1 rejected unjudged.
        let w = world();
        w.qualifying(0, 15, 2);
        judge_rejected(&w, 0, GroundTruthVerdict::Pass);
        assert_eq!(
            w.count(),
            TypedGateCount {
                qualifying: 15,
                accepted: 14
            }
        );
        w.promote().expect("14/15 authorises");
        // The same with the rejected one judged fail.
        let w = world();
        w.qualifying(0, 15, 2);
        judge_rejected(&w, 0, GroundTruthVerdict::Fail);
        assert_eq!(w.count().accepted, 13);
        assert!(w.promote().is_err());
        // Neither judged pass: 13/15.
        let w = world();
        w.qualifying(0, 15, 2);
        assert_eq!(w.count().accepted, 13);
    }

    #[test]
    fn judge_ignores_a_ground_truth_on_any_other_outcome() {
        let truth = crate::ledger::GroundTruth {
            verdict: GroundTruthVerdict::Pass,
            note: "n".into(),
            at: "then".into(),
        };
        let cases = [
            (TrialOutcome::Accepted, false, Judgement::Failure),
            (TrialOutcome::Accepted, true, Judgement::Success),
            (TrialOutcome::Errored, false, Judgement::NotCounted),
            (TrialOutcome::Invalid, false, Judgement::NotCounted),
        ];
        for (outcome, without_escalation, expected) in cases {
            let w = world();
            w.trial(0, |s| {
                s.outcome = outcome;
                s.without_escalation = without_escalation;
            });
            let mut trial = w
                .ledger
                .trials_by_task(&TaskId::from_stored("task-0"))
                .expect("trials")[0]
                .clone();
            trial.ground_truth = Some(truth.clone());
            let facts = TrialFacts {
                trial,
                task_type: Some(TaskType::Feature),
                first_model: Some("haiku".into()),
            };
            assert_eq!(
                judge(
                    &facts,
                    &w.candidate.recipes[3],
                    TaskType::Feature,
                    Some("haiku")
                ),
                expected,
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn a_designed_arm_counts_only_trials_that_were_handed_a_design() {
        let w = world();
        let designed = |s: &mut Spec| {
            s.recipe = 4;
            s.design = true;
        };
        for n in 0..15 {
            w.trial(n, designed);
        }
        let assessment = assess(&w.ledger, &w.incumbent, &w.candidate).expect("reads");
        assert_eq!(
            assessment.evidence.count_of("change-feature-designed", 2),
            TypedGateCount {
                qualifying: 15,
                accepted: 15
            }
        );
        assert_eq!(
            assessment.evidence.count_of("change-feature", 2).qualifying,
            0
        );
    }

    #[test]
    fn a_passing_promotion_records_its_trial_ids_and_keeps_the_recipe_id() {
        let w = world();
        w.qualifying(0, 15, 1);
        let assessment = assess(&w.ledger, &w.incumbent, &w.candidate).expect("reads");
        let [authorisation] = assessment.authorisations.as_slice() else {
            panic!("one authorisation: {:?}", assessment.authorisations)
        };
        assert_eq!(authorisation.trial_ids.len(), 15);
        // The recipe the amendment would append is the measured one: its
        // id is the trials' arm id, and provenance lives outside it.
        let appended = w.candidate.recipes[3].clone();
        assert_eq!(appended.recipe_id(), w.recipe_id(3));
        assert_eq!(authorisation.recipe_id, w.recipe_id(3));
        record(&w.ledger, &assessment).expect("records");
        let recorded = w
            .ledger
            .recipe_promotions(&w.recipe_id(3))
            .expect("reads back");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].trial_ids, authorisation.trial_ids);
        assert_eq!(recorded[0].gate, GATE_NAME);
        assert_eq!(recorded[0].task_type, "feature");
        let trial_ids: Vec<String> = (0..15).map(|n| format!("trial-{n:03}")).collect();
        let stored: Vec<&str> = recorded[0].trial_ids.iter().map(TrialId::as_str).collect();
        assert_eq!(stored, trial_ids);
    }

    #[test]
    fn a_failing_arm_authorises_and_records_nothing() {
        let w = world();
        w.qualifying(0, 15, 2);
        let assessment = assess(&w.ledger, &w.incumbent, &w.candidate).expect("reads");
        assert!(assessment.authorisations.is_empty());
    }

    #[test]
    fn an_untyped_research_change_is_never_assessed() {
        let w = world();
        let mut candidate = w.incumbent.clone();
        let mut untyped = w.candidate.recipes[3].clone();
        untyped.task_type.clear();
        candidate.recipes.push(untyped);
        let assessment = assess(&w.ledger, &w.incumbent, &candidate).expect("reads");
        assert_eq!(assessment, Assessment::default());
    }
}
