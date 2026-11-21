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
    is_typed_research_change, validity_floor_rule, FloorRule, TypedGateCount, TypedGateEvidence,
    ValidityFloor, TYPED_GATE_TRIALS,
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
    /// The arm's earliest unsettled trial, which ends the closed prefix the
    /// gate reads: its id and when it was created. `None` when no pending
    /// trial bars the read, or the floor does not apply.
    pub waiting: Option<Waiting>,
}

/// A pending trial the gate waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub trial: TrialId,
    pub since: String,
}

impl Waiting {
    /// `waiting on trial <id> (pending since <date>)`.
    pub fn line(&self) -> String {
        let date = self.since.get(..10).unwrap_or(&self.since);
        format!("waiting on trial {} (pending since {date})", self.trial)
    }
}

/// Is `facts` in the recipe's arm for `task_type`: its recipe, type and
/// design, whatever model ran it and whatever its outcome?
fn in_arm(facts: &TrialFacts, spec: &RecipeSpec, task_type: TaskType) -> bool {
    facts.trial.arm_recipe_id == spec.recipe_id()
        && facts.task_type == Some(task_type)
        && spec.requires_design == facts.trial.design_sha256.is_some()
}

/// The standing of `spec` for `task_type` among `trials` (oldest first).
///
/// Under [`FloorRule::Applies`] (ADR-0006, a revision first trialled from
/// [`crate::route::TYPED_GATE_FLOOR_FROM`]), the gate reads only the closed
/// prefix that ends before the arm's earliest unsettled trial, and the
/// validity floor is read at the 15th qualifying trial: `settled` is the
/// qualifying trials plus every in-arm errored trial up to it. Below the
/// floor, it is read again at each later trial until a prefix reaches it,
/// and a prefix that reaches it is final. Grandfathered, pending trials
/// are skipped and the floor is not read: the bar the revision started under.
pub fn standing(
    trials: &[TrialFacts],
    spec: &RecipeSpec,
    task_type: TaskType,
    research_model: Option<&str>,
    floor: FloorRule,
) -> TypeStanding {
    let applies = floor == FloorRule::Applies;
    let mut seen: Vec<&TaskId> = Vec::new();
    let mut qualifying: Vec<(&TrialId, bool)> = Vec::new();
    let mut errored = 0usize;
    let mut validity = ValidityFloor::NotApplied;
    let mut waiting = None;
    for facts in trials {
        if facts.trial.outcome.is_none() {
            if applies && in_arm(facts, spec, task_type) {
                waiting = Some(Waiting {
                    trial: facts.trial.trial_id.clone(),
                    since: facts.trial.created_at.clone(),
                });
                break;
            }
            continue;
        }
        match judge(facts, spec, task_type, research_model) {
            Judgement::NotCounted => {
                if facts.trial.outcome == Some(TrialOutcome::Errored)
                    && in_arm(facts, spec, task_type)
                {
                    errored += 1;
                }
            }
            judged => {
                // A valid repeat on a task already counted enters neither
                // count.
                if !seen.contains(&&facts.trial.task_id) {
                    seen.push(&facts.trial.task_id);
                    qualifying.push((&facts.trial.trial_id, judged == Judgement::Success));
                }
            }
        }
        if applies && qualifying.len() >= TYPED_GATE_TRIALS && validity != ValidityFloor::Met {
            let (valid, settled) = (qualifying.len(), qualifying.len() + errored);
            validity = if ValidityFloor::met_by(valid, settled) {
                ValidityFloor::Met
            } else {
                ValidityFloor::Held { valid, settled }
            };
        }
    }
    let read = &qualifying[..qualifying.len().min(TYPED_GATE_TRIALS)];
    TypeStanding {
        task_type,
        count: TypedGateCount {
            qualifying: qualifying.len(),
            accepted: read.iter().filter(|(_, success)| *success).count(),
            floor: validity,
        },
        counted: read.iter().map(|(id, _)| (*id).clone()).collect(),
        waiting,
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
    /// For a recipe the gate refuses while a pending trial bars its read,
    /// keyed by recipe name and revision: the trial it waits on.
    pub waiting: std::collections::BTreeMap<(String, u32), Waiting>,
}

impl Assessment {
    /// The pending trial that bars the read of `name` at `revision`.
    pub fn waiting_on(&self, name: &str, revision: u32) -> Option<&Waiting> {
        self.waiting.get(&(name.to_string(), revision))
    }
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
        // The revision's first trial, settled or not, decides whether it is
        // held to the validity floor; one with none yet always is.
        let floor = trials.first().map_or(FloorRule::Applies, |first| {
            validity_floor_rule(&first.trial.created_at)
        });
        let research_model = arm_model(candidate, spec);
        let standings: Vec<TypeStanding> = spec
            .task_type
            .iter()
            .map(|task_type| standing(&trials, spec, *task_type, research_model, floor))
            .collect();
        let representative = standings
            .iter()
            .find(|standing| !standing.count.passes())
            .or_else(|| standings.last());
        if let Some(representative) = representative {
            assessment
                .evidence
                .insert(&spec.name, spec.revision, representative.count);
            if !representative.count.passes() {
                if let Some(waiting) = &representative.waiting {
                    assessment
                        .waiting
                        .insert((spec.name.clone(), spec.revision), waiting.clone());
                }
            }
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

/// Every trial of `recipe_id`, settled or pending, oldest first, with its
/// task's type and its arm run's first dispatched model.
fn facts_of(ledger: &Ledger, recipe_id: &str) -> Result<Vec<TrialFacts>, LedgerError> {
    let ids = std::iter::once(recipe_id.to_string()).collect();
    ledger
        .trials_for_recipes(&ids)?
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

    /// A clock one second further on at every read, from `start`: trials
    /// are created in the order a test writes them, on either side of the
    /// floor's cutoff.
    struct Ticking {
        start: chrono::DateTime<chrono::FixedOffset>,
        reads: std::sync::atomic::AtomicI64,
    }

    impl crate::ledger::Clock for Ticking {
        fn now_rfc3339(&self) -> String {
            let n = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (self.start + chrono::Duration::seconds(n)).to_rfc3339()
        }
    }

    /// A world whose trials are first created after the floor's cutoff.
    fn world() -> World {
        world_from("2026-12-01T00:00:00+00:00")
    }

    fn world_from(start: &str) -> World {
        let dir = crate::test_support::short_temp_dir("typed-gate");
        let clock = Ticking {
            start: chrono::DateTime::parse_from_rfc3339(start).expect("a start"),
            reads: std::sync::atomic::AtomicI64::new(0),
        };
        let ledger = Ledger::open_with_clock(&dir.join("ledger.sqlite"), Box::new(clock))
            .expect("ledger opens");
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
                accepted: 14,
                floor: ValidityFloor::Met,
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
                accepted: 13,
                floor: ValidityFloor::Met,
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
                accepted: 15,
                floor: ValidityFloor::Met,
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
                accepted: 14,
                floor: ValidityFloor::Met,
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
                accepted: 13,
                floor: ValidityFloor::Met,
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
                accepted: 14,
                floor: ValidityFloor::Met,
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
                accepted: 15,
                floor: ValidityFloor::Met,
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

    impl World {
        /// `n` in-arm errored trials on tasks `from..from + n`.
        fn errored(&self, from: usize, n: usize) {
            for k in 0..n {
                self.trial(from + k, |spec| spec.outcome = TrialOutcome::Errored);
            }
        }

        /// A trial of the plain candidate on `task-<n>` that has not settled.
        fn pending(&self, n: usize, id: &str) {
            let task = TaskId::from_stored(format!("task-{n}"));
            let source = RunId::from_stored(format!("source-{n}"));
            self.ledger
                .insert_typed_run(&source, "/r", None, &task, "rk", Some(TaskType::Feature))
                .expect("task");
            self.ledger
                .insert_replay_trial(&NewReplayTrial {
                    trial_id: &TrialId::from_stored(format!("trial-{id}")),
                    task_id: &task,
                    source_run_id: &source,
                    incumbent_recipe_id: "incumbent",
                    arm_recipe_id: &self.recipe_id(3),
                    base_sha: "base",
                    contract_hash: "contract",
                    verification_profile_hash: "profile",
                    workspace_isolation: "fresh_checkout_no_accepted_answer",
                    arm_run_id: &RunId::from_stored(format!("arm-{id}")),
                    design_sha256: None,
                })
                .expect("trial");
        }

        fn floor(&self) -> ValidityFloor {
            self.count().floor
        }
    }

    #[test]
    fn the_floor_is_whole_numbers_and_pins_its_boundaries() {
        assert_eq!(ValidityFloor::needs(11, 19), 21, "an f64 0.8 x 19 gives 22");
        assert!(ValidityFloor::met_by(16, 20));
        assert!(ValidityFloor::met_by(20, 25));
        assert!(!ValidityFloor::met_by(15, 19));
        assert_eq!(ValidityFloor::needs(15, 20), 5);
        assert_eq!(ValidityFloor::needs(16, 20), 0);
    }

    #[test]
    fn fifteen_of_eighteen_passes_the_floor() {
        let world = world();
        world.errored(100, 3);
        world.qualifying(0, 15, 0);
        assert_eq!(world.floor(), ValidityFloor::Met);
        world.promote().expect("15/18 passes");
    }

    #[test]
    fn fifteen_of_nineteen_is_held_and_sixteen_of_twenty_clears_it() {
        let world = world();
        world.errored(100, 4);
        world.qualifying(0, 15, 0);
        assert_eq!(
            world.floor(),
            ValidityFloor::Held {
                valid: 15,
                settled: 19
            }
        );
        assert!(matches!(
            world.promote(),
            Err(CandidateRejection::TypedResearchGate { .. })
        ));
        world.qualifying(15, 1, 0);
        assert_eq!(world.floor(), ValidityFloor::Met);
        world.promote().expect("16/20 clears the floor");
    }

    #[test]
    fn fifteen_of_twenty_is_held_with_the_pinned_line() {
        let world = world();
        world.errored(100, 5);
        world.qualifying(0, 15, 0);
        let refusal = world.promote().unwrap_err().to_string();
        assert_eq!(
            refusal,
            "held: 15/20 valid (75%), below the 80% floor; needs 5 more valid trials"
        );
        assert!(refusal.chars().count() <= 80);
        let assessment = assess(&world.ledger, &world.incumbent, &world.candidate).expect("reads");
        assert!(
            assessment.authorisations.is_empty(),
            "held authorises nothing"
        );
        assert_eq!(
            assessment
                .evidence
                .count_of("change-feature", 2)
                .held_by_floor(),
            Some(5)
        );
    }

    #[test]
    fn a_pass_is_final_and_later_errored_trials_never_move_it() {
        let world = world();
        world.errored(100, 3);
        world.qualifying(0, 15, 0);
        world.errored(200, 6);
        assert_eq!(world.floor(), ValidityFloor::Met);
        world.promote().expect("still passes");
    }

    #[test]
    fn a_pending_trial_is_a_barrier_never_a_denominator_entry() {
        let world = world();
        world.pending(300, "000-pending");
        world.errored(100, 3);
        world.qualifying(0, 15, 0);
        let assessment = assess(&world.ledger, &world.incumbent, &world.candidate).expect("reads");
        let count = assessment.evidence.count_of("change-feature", 2);
        assert_eq!(
            count.qualifying, 0,
            "nothing after the pending trial is read"
        );
        assert!(!count.passes());
        let waiting = assessment.waiting_on("change-feature", 2).expect("waits");
        assert_eq!(waiting.trial.as_str(), "trial-000-pending");
        assert_eq!(
            waiting.line(),
            "waiting on trial trial-000-pending (pending since 2026-12-01)"
        );
        // The oldest trial settles errored: 15 of 19, held.
        world
            .ledger
            .settle_trial(
                &TrialId::from_stored("trial-000-pending"),
                TrialOutcome::Errored,
                false,
                TrialCost::UNKNOWN,
                1,
            )
            .expect("settle");
        assert_eq!(
            world.floor(),
            ValidityFloor::Held {
                valid: 15,
                settled: 19
            }
        );
        let assessment = assess(&world.ledger, &world.incumbent, &world.candidate).expect("reads");
        assert!(assessment.waiting.is_empty());
    }

    #[test]
    fn a_held_arm_passes_at_the_earliest_prefix_that_reaches_the_floor() {
        let world = world();
        world.errored(100, 5);
        world.qualifying(0, 15, 0);
        assert!(world.promote().is_err(), "15/20 is held");
        world.qualifying(15, 5, 0);
        assert_eq!(world.floor(), ValidityFloor::Met, "20/25 reaches it");
        world.errored(200, 5);
        assert_eq!(
            world.floor(),
            ValidityFloor::Met,
            "and later errors never move it"
        );
        world.promote().expect("still passes");
    }

    #[test]
    fn a_valid_repeat_enters_neither_count() {
        let world = world();
        world.qualifying(0, 7, 0);
        // A valid repeat of task-0 between the 1st and the 15th qualifying.
        world.retrial(0, "500", |_| {});
        world.qualifying(7, 8, 0);
        let count = world.count();
        assert_eq!(count.qualifying, 15);
        assert_eq!(count.floor, ValidityFloor::Met, "15/15, not 15/16");
        world.promote().expect("passes");
    }

    #[test]
    fn errored_retries_on_one_task_each_enter_the_denominator() {
        let world = world();
        world.trial(100, |spec| spec.outcome = TrialOutcome::Errored);
        for id in ["100b", "100c", "100d"] {
            world.retrial(100, id, |spec| spec.outcome = TrialOutcome::Errored);
        }
        world.qualifying(0, 15, 0);
        assert_eq!(
            world.floor(),
            ValidityFloor::Held {
                valid: 15,
                settled: 19
            }
        );
    }

    /// A revision first trialled before the release keeps the bar it
    /// started under: no floor, and a pending trial bars nothing.
    #[test]
    fn a_revision_first_trialled_before_the_release_is_not_held() {
        let world = world_from("2026-11-10T00:00:00+00:00");
        world.pending(300, "000-pending");
        world.errored(100, 8);
        world.qualifying(0, 15, 1);
        let assessment = assess(&world.ledger, &world.incumbent, &world.candidate).expect("reads");
        let count = assessment.evidence.count_of("change-feature", 2);
        assert_eq!(count.floor, ValidityFloor::NotApplied);
        assert_eq!((count.qualifying, count.accepted), (15, 14));
        assert!(assessment.waiting.is_empty());
        world.promote().expect("the old bar passes 14 of 15");
        // The cutoff's boundary is the cutoff itself.
        assert_eq!(
            validity_floor_rule(crate::route::TYPED_GATE_FLOOR_FROM),
            FloorRule::Applies
        );
        assert_eq!(
            validity_floor_rule("2026-11-20T23:59:59+00:00"),
            FloorRule::Grandfathered
        );
        assert_eq!(validity_floor_rule("not a stamp"), FloorRule::Applies);
    }
}
