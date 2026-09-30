//! Inference from loaded artifacts (SPEC §16, §21).
//!
//! Training and inference share the same feature extraction code; the
//! artifact carries its own standardization so the transform is the same
//! bytes at train and inference. An inference result carries the artifact
//! ID, schema version, input hash, estimates per eligible arm (a tier and
//! an effort), the
//! supported cohorts and an abstention reason when the artifact cannot
//! honestly estimate. Predictions are not guarantees; the deterministic
//! runner still owns policy and acceptance. Neither predictor invents
//! coverage for unseen profiles.

use serde::{Deserialize, Serialize};

use super::features::{
    expand, feature_dim, FeatureSchema, RecipeIdentity, SparseVec, Standardization, TaskFeatures,
};
use super::registry::Registry;
use crate::contract::TaskContract;
use crate::money::MicroUsd;
use crate::policy::{EffectiveAuthority, ModelProfile, RepoPolicy, Tier};
use crate::route::{Arm, Estimates, RoutePredictor};

/// What the artifact said about the arms it was offered. Every arm is named
/// by its label (`implementation@high`, see [`Arm::label`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InferenceResult {
    pub artifact_id: String,
    pub feature_schema_version: u32,
    pub input_hash: String,
    pub eligible: Vec<String>,
    pub acceptance: Vec<(String, f64)>,
    pub cost_micros: Vec<(String, i64)>,
    pub supported_cohorts: Vec<String>,
    /// Eligible arms the artifact declined to estimate for, and why —
    /// recorded as evidence even when other arms were estimated, so a
    /// route can be read back against the profile that was in force.
    pub declined: Vec<(String, String)>,
    pub abstention_reason: Option<String>,
}

/// Load the registry's active artifact and estimate. Abstains when no
/// artifact is active, when its schema is unreadable, or when an
/// eligible arm has no trained coverage — abstention routes to the
/// conservative baseline, never to a guess.
pub fn estimate_from_registry(
    registry: &Registry,
    contract: &TaskContract,
    repo_policy: &RepoPolicy,
    authority: &EffectiveAuthority,
    arms: &[Arm],
    harness: Option<&str>,
) -> InferenceResult {
    let schema = FeatureSchema::standard();
    let task = TaskFeatures::extract(contract, repo_policy);
    // The recipe that would cover this task among the eligible tiers, the
    // same pure rule the router uses to decide whether a recipe wins over
    // the learner (SPEC §6, step 3) — recorded here so evidence is keyed
    // on the recipe that produced a run, not only the model beneath it.
    let mut tiers: Vec<Tier> = Vec::new();
    for arm in arms {
        if !tiers.contains(&arm.tier) {
            tiers.push(arm.tier);
        }
    }
    let recipe_id = crate::route::covering_recipe_id(contract, repo_policy, &tiers);
    // The identity an arm would dispatch WITH — the same tokens the
    // dataset builder reads back from the dispatch intent and the recipe
    // that would cover it, at the arm's own effort.
    let identity_of = |arm: &Arm| -> RecipeIdentity {
        authority
            .models
            .get(&arm.tier)
            .map(|profile| RecipeIdentity {
                recipe_id: recipe_id.clone(),
                ..arm_identity(profile, arm, harness)
            })
            .unwrap_or_default()
    };
    let inputs: Vec<(&Arm, SparseVec)> = arms
        .iter()
        .map(|arm| {
            (
                arm,
                expand(
                    &task,
                    arm.tier,
                    &contract.objective,
                    &identity_of(arm),
                    &schema,
                ),
            )
        })
        .collect();
    let input_hash = crate::ids::sha256_hex(
        serde_json::to_string(&inputs.iter().map(|(_, sparse)| sparse).collect::<Vec<_>>())
            .expect("a vector of sparse f64 features serializes: no map keys, no NaN")
            .as_bytes(),
    );

    let abstain = |reason: String| InferenceResult {
        artifact_id: "none".into(),
        feature_schema_version: schema.version,
        input_hash: input_hash.clone(),
        eligible: arms.iter().map(Arm::label).collect(),
        acceptance: Vec::new(),
        cost_micros: Vec::new(),
        supported_cohorts: Vec::new(),
        declined: Vec::new(),
        abstention_reason: Some(reason),
    };

    // An unreadable active pointer is not an absent one: reporting a
    // schema-incompatible or unreadable artifact as "none promoted" hid
    // every reason a promoted artifact had stopped being used.
    let artifact = match registry.active() {
        Ok(Some(artifact)) => artifact,
        Ok(None) => return abstain("no active learned artifact".into()),
        Err(e) => return abstain(format!("active artifact unusable: {e}")),
    };
    if artifact.feature_schema != schema {
        return abstain("feature schema mismatch".into());
    }
    let mut acceptance = Vec::new();
    let mut costs = Vec::new();
    let mut declined: Vec<(String, String)> = Vec::new();
    // The cohort is this contract's KIND — the same token the dataset
    // recorded when it was trained. It used to be the literal "change"
    // for every task, so an `inspect` contract that fell back to the
    // empirical baseline was priced with the cost of changing code.
    let cohort = super::features::cohort_of_kind(contract.kind());
    for (arm, features) in &inputs {
        if !artifact.tiers_supported.contains(&arm.tier) {
            declined.push((arm.label(), "no trained coverage for this tier".to_string()));
            continue;
        }
        // Evidence belongs to the profile that produced it (SPEC §17). An
        // arm whose model, effort or harness differs from what training
        // observed is a profile the artifact never saw: it gets no
        // estimate, rather than the previous model's acceptance record.
        let identity = identity_of(arm);
        if !observed_identity(&artifact, arm.tier, &identity) {
            declined.push((
                arm.label(),
                format!(
                    "profile identity {} was not observed in training; a model swap inherits no \
                     evidence",
                    describe(&identity)
                ),
            ));
            continue;
        }
        let standardized = artifact.standardization.apply(features);
        let probability = artifact.acceptance.predict_proba(&standardized);
        acceptance.push((arm.label(), probability));
        // An arm the cost model cannot price is left out of `cost_micros`
        // rather than priced at zero: the router skips an arm with no cost
        // estimate instead of treating it as the cheapest one.
        if let Some(cost) = artifact.cost.predict(&standardized, Some(cohort)) {
            costs.push((arm.label(), cost.max(0.0) as i64));
        }
    }
    if acceptance.is_empty() {
        let why = declined
            .iter()
            .map(|(arm, reason)| format!("{arm}: {reason}"))
            .collect::<Vec<_>>()
            .join("; ");
        // The per-arm reasons ride along with the abstention: "no
        // trained coverage" and "that profile was never observed" are
        // different facts about the artifact, and the route records both.
        return InferenceResult {
            declined,
            ..abstain(if why.is_empty() {
                format!(
                    "no trained coverage for {:?}",
                    arms.iter().map(Arm::label).collect::<Vec<_>>()
                )
            } else {
                why
            })
        };
    }
    InferenceResult {
        artifact_id: artifact.artifact_id,
        feature_schema_version: artifact.feature_schema.version,
        input_hash,
        eligible: arms.iter().map(Arm::label).collect(),
        acceptance,
        cost_micros: costs,
        supported_cohorts: artifact.cohorts.clone(),
        declined,
        abstention_reason: None,
    }
}

/// Whether the artifact observed this exact profile identity at this tier
/// while training. An artifact that lists no identity for a tier observed
/// none: it cannot vouch for any profile there.
fn observed_identity(
    artifact: &super::registry::Artifact,
    tier: Tier,
    identity: &RecipeIdentity,
) -> bool {
    artifact
        .observed_identities
        .iter()
        .find(|(observed_tier, _)| *observed_tier == tier)
        .is_some_and(|(_, identities)| identities.contains(identity))
}

/// A recipe identity as a reason line reads it.
fn describe(identity: &RecipeIdentity) -> String {
    let mut text = identity.model.clone();
    if let Some(effort) = &identity.effort {
        text.push_str(&format!("/{effort}"));
    }
    if let Some(harness) = &identity.harness {
        text.push_str(&format!(" on {harness}"));
    }
    if let Some(recipe_id) = &identity.recipe_id {
        text.push_str(&format!(" via recipe {recipe_id}"));
    }
    text
}

/// One profile's model/effort/harness identity tokens, as the runner
/// records them in the dispatch intent and the dataset reads them back:
/// one function, so the train and inference sides cannot drift. Carries no
/// `recipe_id` — the caller folds in the recipe that covers the task, since
/// that is a property of the task and policy, not of the profile.
pub fn profile_identity(
    profile: &crate::policy::ModelProfile,
    harness: Option<&str>,
) -> RecipeIdentity {
    RecipeIdentity {
        model: profile.id.clone(),
        effort: profile
            .effort
            .as_ref()
            .map(|effort| effort.as_str().to_string()),
        harness: harness.map(str::to_string),
        recipe_id: None,
    }
}

/// The identity `arm` dispatches with: its tier's authority `profile` with
/// the effort the arm asks for in place of the configured one. Carries no
/// `recipe_id`, as [`profile_identity`] does not.
pub fn arm_identity(profile: &ModelProfile, arm: &Arm, harness: Option<&str>) -> RecipeIdentity {
    profile_identity(
        &ModelProfile {
            effort: arm.effort.id().cloned(),
            ..profile.clone()
        },
        harness,
    )
}

/// The bridge the router consumes: a RoutePredictor backed by the local
/// registry's active artifact. Runs pin what they read; a newly trained
/// artifact affects only new runs.
pub struct RegistryPredictor<'a> {
    registry: &'a Registry,
    repo_policy: &'a RepoPolicy,
    harness: Option<String>,
}

impl<'a> RegistryPredictor<'a> {
    pub fn new(registry: &'a Registry, repo_policy: &'a RepoPolicy, harness: Option<&str>) -> Self {
        Self {
            registry,
            repo_policy,
            harness: harness.map(str::to_string),
        }
    }
}

impl RoutePredictor for RegistryPredictor<'_> {
    fn estimate(
        &self,
        contract: &TaskContract,
        authority: &EffectiveAuthority,
        arms: &[Arm],
    ) -> Option<Estimates> {
        let result = estimate_from_registry(
            self.registry,
            contract,
            self.repo_policy,
            authority,
            arms,
            self.harness.as_deref(),
        );
        if result.abstention_reason.is_some() {
            return None;
        }
        let input_hash = result.input_hash.clone();
        let raw = serde_json::to_value(&result)
            .expect("an InferenceResult serializes: owned strings, integers and finite f64");
        // The result names arms by label; the labels are the ones `arms`
        // produced, so each reads back to the arm it was written for.
        let arm_named = |label: &str| arms.iter().find(|arm| arm.label() == label).cloned();
        let acceptance: Vec<(Arm, f64)> = result
            .acceptance
            .into_iter()
            .filter_map(|(label, probability)| Some((arm_named(&label)?, probability)))
            .collect();
        let cost: Vec<(Arm, MicroUsd)> = result
            .cost_micros
            .into_iter()
            .filter_map(|(label, cost_micros)| {
                Some((arm_named(&label)?, MicroUsd::from_micros(cost_micros)))
            })
            .collect();
        if acceptance.is_empty() {
            return None;
        }
        Some(Estimates {
            artifact_id: result.artifact_id,
            input_hash,
            acceptance,
            cost,
            raw,
        })
    }
}

/// The train-side transform, shared byte-for-byte with inference: fitted
/// standardization applied to the same expansion.
pub fn standardized_inputs(
    task: &TaskFeatures,
    objective: &str,
    identity: &RecipeIdentity,
    tiers: &[Tier],
    schema: &FeatureSchema,
    standardization: &Standardization,
) -> Vec<(Tier, SparseVec)> {
    tiers
        .iter()
        .map(|tier| {
            let expanded = expand(task, *tier, objective, identity, schema);
            (*tier, standardization.apply(&expanded))
        })
        .collect()
}

pub fn dim_of(schema: &FeatureSchema) -> usize {
    feature_dim(schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::registry::Artifact;
    use crate::policy::Tier;
    use crate::route::EffortRequest;
    use std::collections::BTreeMap;

    fn repo_policy() -> RepoPolicy {
        RepoPolicy::from_toml_str(crate::policy::INIT_TEMPLATE).expect("policy")
    }

    /// A test directory nobody else can collide with, pre-cleaned so a
    /// crashed earlier run cannot decide this one: the process owns the
    /// pid, the counter orders the directories within it. A thread id is
    /// reused the moment a thread ends.
    fn temp_dir(name: &str) -> crate::test_support::TempDir {
        crate::test_support::temp_dir(&format!("predict-{name}"))
    }

    /// The identity the implementation tier dispatches with under the
    /// template policy and no harness — what the fixture artifact
    /// observed in training.
    fn identity_under_test() -> RecipeIdentity {
        let repo = repo_policy();
        let profile = repo
            .models
            .get(&Tier::Implementation)
            .expect("the template configures an implementation model");
        profile_identity(profile, None)
    }

    /// The arm the template's implementation tier dispatches: its
    /// configured effort, which is what the fixture artifact observed.
    fn implementation_arm() -> Arm {
        let profile = &repo_policy().models[&Tier::Implementation];
        Arm {
            tier: Tier::Implementation,
            effort: match &profile.effort {
                Some(effort) => EffortRequest::Explicit(effort.clone()),
                None => EffortRequest::NotRequested,
            },
        }
    }

    fn contract() -> TaskContract {
        TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1, "kind": "change",
                "objective": "Remove the obsolete entry point",
                "base_ref": "HEAD", "write_scope": ["src/**"],
                "acceptance": ["src/main.rs is gone"],
                "verification_profile": "default",
            })
            .to_string(),
        )
        .expect("contract")
    }

    /// An artifact whose arrays cover the WHOLE feature space, as a
    /// trained one's do. The registry used to accept an eight-weight
    /// artifact here and predict over three hundred dimensions with it
    /// (D1); `validate` now refuses that, so the fixture is honest.
    fn artifact_over_the_whole_feature_space() -> Artifact {
        let schema = FeatureSchema::standard();
        let dim = feature_dim(&schema);
        Artifact {
            schema_version: super::super::registry::ARTIFACT_SCHEMA_VERSION,
            artifact_id: "art-test".into(),
            feature_schema: schema,
            standardization: Standardization::fit(
                &[crate::learn::features::SparseVec(vec![(0, 1.0)])],
                dim,
            ),
            acceptance: crate::learn::learner::LogisticModel {
                dim,
                weights: vec![0.01; dim],
                bias: 0.0,
            },
            cost: crate::learn::learner::CostModel {
                dim,
                weights: vec![0.01; dim],
                bias: 3.0,
                cohort_means: vec![("change".into(), 500.0), ("inspect".into(), 70.0)],
                observed_log_min: 1.0,
                observed_log_max: 8.0,
            },
            tiers_supported: vec![Tier::Implementation],
            observed_identities: vec![(Tier::Implementation, vec![identity_under_test()])],
            cohorts: vec!["change".into(), "inspect".into()],
            label_policy_version: crate::learn::dataset::LABEL_POLICY_VERSION,
            dataset_fingerprint: "f".into(),
            solver: crate::learn::learner::SolverSettings::default(),
            trained_at: "now".into(),
            relais_version: crate::version().into(),
            evaluation: None,
        }
    }

    fn authority() -> EffectiveAuthority {
        let repo = repo_policy();
        let machine =
            crate::policy::MachineSettings::from_toml_str("schema_version = 1").expect("machine");
        crate::policy::effective_authority(
            &repo,
            &machine,
            &contract(),
            &crate::policy::RepoIdentity::common_dir(std::path::Path::new("/repos/relais/.git")),
        )
    }

    #[test]
    fn abstains_without_an_active_artifact() {
        let dir = temp_dir("1");
        let registry = Registry::open(&dir).expect("registry");
        let repo = repo_policy();
        let task = contract();
        let auth = authority();
        let result = estimate_from_registry(
            &registry,
            &task,
            &repo,
            &auth,
            &[implementation_arm()],
            None,
        );
        assert_eq!(
            result.abstention_reason.as_deref(),
            Some("no active learned artifact")
        );
        let repo = repo_policy();
        let predictor = RegistryPredictor::new(&registry, &repo, None);
        assert!(predictor
            .estimate(&task, &auth, &[implementation_arm()])
            .is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn estimates_with_coverage_and_abstains_without_it() {
        let dir = temp_dir("2");
        let registry = Registry::open(&dir).expect("registry");
        let artifact = artifact_over_the_whole_feature_space();
        registry.store(&artifact).expect("store");
        // Promotion needs the evaluator's report; write the active pointer
        // the way the registry does after a real promotion.
        std::fs::write(
            dir.join("active.json"),
            serde_json::json!({ "artifact_id": "art-test" }).to_string(),
        )
        .expect("activate");

        let repo = repo_policy();
        let task = contract();
        let auth = authority();
        let predictor = RegistryPredictor::new(&registry, &repo, None);
        let estimates = predictor
            .estimate(&task, &auth, &[implementation_arm()])
            .expect("implementation is covered");
        let (arm, probability) = &estimates.acceptance[0];
        assert_eq!(*arm, implementation_arm());
        assert!(*probability > 0.0);
        assert!(*probability < 1.0);

        // An eligible-but-unseen tier gets no invented coverage.
        let escalation = Arm {
            tier: Tier::Escalation,
            effort: EffortRequest::NotRequested,
        };
        assert!(predictor.estimate(&task, &auth, &[escalation]).is_none());
        let _ = BTreeMap::<Tier, f64>::new();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// L8: coverage is keyed by the PROFILE identity, not the tier. The
    /// same model under a different harness is a profile the artifact
    /// never observed, and it inherits none of its evidence (SPEC §17).
    #[test]
    fn a_profile_identity_training_never_saw_gets_no_estimate() {
        let dir = temp_dir("identity");
        let registry = Registry::open(&dir).expect("registry");
        registry
            .store(&artifact_over_the_whole_feature_space())
            .expect("store");
        std::fs::write(
            dir.join("active.json"),
            serde_json::json!({ "artifact_id": "art-test" }).to_string(),
        )
        .expect("activate");

        let repo = repo_policy();
        let auth = authority();
        let observed = estimate_from_registry(
            &registry,
            &contract(),
            &repo,
            &auth,
            &[implementation_arm()],
            None,
        );
        assert_eq!(
            observed.abstention_reason, None,
            "the observed profile estimates"
        );

        let swapped = estimate_from_registry(
            &registry,
            &contract(),
            &repo,
            &auth,
            &[implementation_arm()],
            Some("another-harness 9.9"),
        );
        assert!(
            swapped
                .abstention_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("was not observed in training")),
            "{:?}",
            swapped.abstention_reason
        );
        assert!(swapped.acceptance.is_empty());
        assert_eq!(
            swapped.declined.len(),
            1,
            "the declined tier is recorded as evidence: {:?}",
            swapped.declined
        );
        // The router sees an abstention, not a guess.
        let predictor = RegistryPredictor::new(&registry, &repo, Some("another-harness 9.9"));
        assert!(predictor
            .estimate(&contract(), &auth, &[implementation_arm()])
            .is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Coverage is keyed by the ARM's own identity: the same model at an
    /// effort training never dispatched it with is an unobserved profile.
    #[test]
    fn an_effort_training_never_saw_gets_no_estimate() {
        let dir = temp_dir("arm-effort");
        let registry = Registry::open(&dir).expect("registry");
        registry
            .store(&artifact_over_the_whole_feature_space())
            .expect("store");
        std::fs::write(
            dir.join("active.json"),
            serde_json::json!({ "artifact_id": "art-test" }).to_string(),
        )
        .expect("activate");

        let unseen = Arm {
            tier: Tier::Implementation,
            effort: EffortRequest::Explicit(
                crate::policy::EffortId::parse("unseen").expect("an effort id"),
            ),
        };
        let result = estimate_from_registry(
            &registry,
            &contract(),
            &repo_policy(),
            &authority(),
            &[implementation_arm(), unseen.clone()],
            None,
        );
        assert_eq!(result.abstention_reason, None, "the observed arm estimates");
        assert_eq!(
            result
                .acceptance
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            vec![implementation_arm().label()]
        );
        assert_eq!(
            result.eligible,
            vec![implementation_arm().label(), unseen.label()]
        );
        assert!(
            result
                .declined
                .iter()
                .any(|(label, reason)| *label == unseen.label()
                    && reason.contains("was not observed in training")),
            "{:?}",
            result.declined
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Coverage is keyed by the RECIPE identity too, not only model/effort/
    /// harness: a task now covered by a different revision of the recipe
    /// that produced training is a `RecipeIdentity` the artifact never
    /// observed, and gets no invented evidence (SPEC §17). The artifact HAS
    /// observations here — for the no-recipe identity — so this exercises
    /// the abstention path rather than one an empty artifact would pass
    /// trivially.
    #[test]
    fn a_recipe_revision_training_never_saw_gets_no_estimate() {
        let dir = temp_dir("recipe-identity");
        let registry = Registry::open(&dir).expect("registry");
        registry
            .store(&artifact_over_the_whole_feature_space())
            .expect("store");
        std::fs::write(
            dir.join("active.json"),
            serde_json::json!({ "artifact_id": "art-test" }).to_string(),
        )
        .expect("activate");

        let repo_no_recipe = repo_policy();
        let auth = authority();
        let observed = estimate_from_registry(
            &registry,
            &contract(),
            &repo_no_recipe,
            &auth,
            &[implementation_arm()],
            None,
        );
        assert_eq!(
            observed.abstention_reason, None,
            "the identity training observed (no covering recipe) still estimates"
        );

        let mut repo_with_recipe = repo_policy();
        repo_with_recipe.recipes.push(crate::policy::RecipeSpec {
            scope_within: vec!["src/**".into()],
            ..crate::policy::RecipeSpec::covering("src-recipe", Tier::Implementation)
        });
        let under_recipe = estimate_from_registry(
            &registry,
            &contract(),
            &repo_with_recipe,
            &auth,
            &[implementation_arm()],
            None,
        );
        assert!(
            under_recipe
                .abstention_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("was not observed in training")),
            "{:?}",
            under_recipe.abstention_reason
        );
        assert!(under_recipe.acceptance.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// L7: an artifact that exists but cannot be read is not "none
    /// promoted"; the reason reaches the abstention.
    #[test]
    fn an_unreadable_active_artifact_says_so() {
        let dir = temp_dir("unreadable");
        let registry = Registry::open(&dir).expect("registry");
        std::fs::write(
            dir.join("active.json"),
            serde_json::json!({ "artifact_id": "art-missing" }).to_string(),
        )
        .expect("activate");
        let repo = repo_policy();
        let auth = authority();
        let result = estimate_from_registry(
            &registry,
            &contract(),
            &repo,
            &auth,
            &[implementation_arm()],
            None,
        );
        assert!(
            result
                .abstention_reason
                .as_deref()
                .is_some_and(|reason| reason.starts_with("active artifact unusable:")),
            "{:?}",
            result.abstention_reason
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// D4: the cohort handed to the cost model was the literal `"change"`
    /// for every contract, so an `inspect` task that fell back to the
    /// empirical baseline was priced as a code change.
    #[test]
    fn the_cost_cohort_is_the_contracts_own_kind() {
        let dir = temp_dir("3");
        let registry = Registry::open(&dir).expect("registry");
        let mut artifact = artifact_over_the_whole_feature_space();
        // A cost model that can only extrapolate: every prediction lands
        // far outside the range it was fitted on, so every answer is the
        // cohort's empirical mean and the cohort is observable.
        artifact.cost.weights = vec![0.0; artifact.cost.dim];
        artifact.cost.bias = 20.0;
        artifact.cost.observed_log_min = 0.0;
        artifact.cost.observed_log_max = 0.0;
        registry.store(&artifact).expect("store");
        std::fs::write(
            dir.join("active.json"),
            serde_json::json!({ "artifact_id": artifact.artifact_id }).to_string(),
        )
        .expect("activate");

        let repo = repo_policy();
        let auth = authority();
        let inspect = TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1, "kind": "inspect",
                "objective": "Explain how routing picks a tier",
                "base_ref": "HEAD",
                "acceptance": ["names the deciding function"],
                "verification_profile": "default",
            })
            .to_string(),
        )
        .expect("contract");

        let change_cost = estimate_from_registry(
            &registry,
            &contract(),
            &repo,
            &auth,
            &[implementation_arm()],
            None,
        )
        .cost_micros;
        let inspect_cost = estimate_from_registry(
            &registry,
            &inspect,
            &repo,
            &auth,
            &[implementation_arm()],
            None,
        )
        .cost_micros;
        let label = implementation_arm().label();
        assert_eq!(change_cost, vec![(label.clone(), 500)]);
        assert_eq!(
            inspect_cost,
            vec![(label, 70)],
            "an inspection is priced from the inspect cohort, not the change cohort"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tier the cost model cannot price at all is reported with no cost
    /// entry — never with a zero, which would make it the cheapest tier.
    #[test]
    fn an_unpriceable_tier_carries_no_cost_estimate() {
        let dir = temp_dir("4");
        let registry = Registry::open(&dir).expect("registry");
        let mut artifact = artifact_over_the_whole_feature_space();
        artifact.cost.weights = vec![0.0; artifact.cost.dim];
        artifact.cost.bias = 20.0;
        artifact.cost.observed_log_min = 0.0;
        artifact.cost.observed_log_max = 0.0;
        artifact.cost.cohort_means = vec![("inspect".into(), 70.0)];
        registry.store(&artifact).expect("store");
        std::fs::write(
            dir.join("active.json"),
            serde_json::json!({ "artifact_id": artifact.artifact_id }).to_string(),
        )
        .expect("activate");

        let repo = repo_policy();
        let auth = authority();
        let result = estimate_from_registry(
            &registry,
            &contract(),
            &repo,
            &auth,
            &[implementation_arm()],
            None,
        );
        assert_eq!(result.acceptance.len(), 1, "acceptance is still estimated");
        assert!(
            result.cost_micros.is_empty(),
            "no observed support for the change cohort: no price, not a free tier"
        );
        // The router consumes it through the predictor and, with nothing
        // priced, selects nothing (D5).
        let predictor = RegistryPredictor::new(&registry, &repo, None);
        let estimates = predictor
            .estimate(&contract(), &auth, &[implementation_arm()])
            .expect("acceptance is available");
        assert!(estimates.cost.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
