//! Candidate recipe validation: the boundary a learner may not cross.
//!
//! A learner may propose a retuned `relais.toml`. Nothing here writes one,
//! grants one, or enables anything it names — [`validate_candidate`] only
//! decides whether a proposed policy is ADMISSIBLE, and hands back a value
//! that says so. No policy file is written, no trust grant is issued, no
//! recipe is enabled, and no learner calls this yet.
//!
//! The boundary is DEFAULT-FIXED, not default-tunable: every `RepoPolicy`
//! field except `recipes` must be byte-identical to the incumbent's, and
//! within `recipes` only a bounded set of per-recipe knobs may move,
//! bounded further by a tier floor this module re-derives with the exact
//! [`kind_floor`] and [`risk_floor`] a real task's floor is computed with —
//! never a second copy of either rule. Lives beside them, in `route`
//! rather than `policy`, because `policy` must stay a leaf `route` depends
//! on (`scripts/check-module-cycles.py`): a `policy` module that reached
//! back into `route` would be exactly the cycle that script exists to
//! catch.

use std::collections::BTreeSet;

use crate::contract::Review;
use crate::policy::{
    validate_recipes, ContextPolicy, Effort, ExecutionPolicy, ModelProfile, RecipeError,
    RecipeSpec, RepoPolicy, RiskRule, Tier,
};

use super::recipe_tier_floor;

/// Caps a learner's proposed knobs must respect. Never sourced from the
/// candidate itself — a self-reported cap is not a cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuningBounds {
    /// Model ids a recipe's own `models` table may name, per tier.
    pub allowed_models: BTreeSet<String>,
    pub max_attempts: u32,
    pub max_repairs_before_escalation: u32,
    pub max_wall_seconds: u64,
    pub max_agent_depth: u32,
    pub max_agents_total: u32,
    pub max_context_budget_bytes: usize,
    /// Efforts a recipe's own `models` table may name. `effort` was
    /// unbounded, so a candidate could raise every tier's effort — spend
    /// a learner must not choose for itself.
    pub allowed_efforts: Vec<Effort>,
    /// Whether a candidate may grant nested agent spawning. A CAPABILITY,
    /// not a number to tune: `false` here means a candidate cannot turn
    /// it on however it retunes the caps around it.
    pub allow_nested_agents: bool,
}

/// The seal. `CandidateRecipe`'s field is private to THIS module, and
/// this module holds the only function that can build one — so "nothing
/// but the validator mints a candidate" is enforced by the compiler
/// rather than by a source scan.
///
/// A scan was tried first and was not good enough: it exempted the whole
/// file, so a `fn forged(policy) -> CandidateRecipe { CandidateRecipe {
/// policy } }` added a few lines away passed it (verified). Tightening it
/// ran into a grep being unable to tell a construction from a
/// `matches!(.., Ok(CandidateRecipe { .. }))` pattern or a mention in a
/// doc comment. Privacy can tell. Anything outside this module that tries
/// to build one fails to compile, which is the only version of this
/// guarantee that cannot rot.
mod sealed {
    use super::{fixed_fields_match, validate_recipe_history, CandidateRejection, TuningBounds};
    use crate::policy::RepoPolicy;

    /// A candidate `RepoPolicy` [`validate_candidate`] accepted: the
    /// executable authority is identical to the incumbent's except for
    /// tunable recipe knobs, every recipe's tier still clears the floor its
    /// own risk rules and kind demand, and nothing protective moved.
    ///
    /// Private field, no other constructor: [`validate_candidate`] is the
    /// ONLY way to obtain one. No `Default`, no `Deserialize`, no `From`, no
    /// `pub(crate)` field or method that would hand out the inner value
    /// without going through the check.
    #[derive(Debug, Clone, PartialEq)]
    pub struct CandidateRecipe {
        policy: RepoPolicy,
    }

    impl CandidateRecipe {
        /// The accepted policy. Read-only: nothing here writes it, grants it,
        /// or enables anything it names (see the module doc).
        pub fn policy(&self) -> &RepoPolicy {
            &self.policy
        }
    }

    /// The ONLY way to obtain a [`CandidateRecipe`]: decides whether
    /// `candidate` differs from `incumbent` in recipe knobs alone, and hands
    /// back a value that says so. Never writes a policy file, issues a trust
    /// grant, or enables a recipe — see the module doc.
    pub fn validate_candidate(
        incumbent: &RepoPolicy,
        candidate: &RepoPolicy,
        bounds: &TuningBounds,
    ) -> Result<CandidateRecipe, CandidateRejection> {
        fixed_fields_match(incumbent, candidate)?;
        validate_recipe_history(
            &incumbent.recipes,
            &candidate.recipes,
            &candidate.risk,
            bounds,
        )?;
        Ok(CandidateRecipe {
            policy: candidate.clone(),
        })
    }
}

pub use sealed::{validate_candidate, CandidateRecipe};

/// Why [`validate_candidate`] refused a proposed policy — one variant per
/// rule, each naming what it refused and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateRejection {
    /// A `RepoPolicy` field other than `recipes` differs from the
    /// incumbent's. The boundary is default-fixed: only `recipes` may
    /// move, and only in the ways the other variants below describe.
    FixedFieldChanged { field: &'static str },
    /// The candidate's `recipes` do not start with every recipe the
    /// incumbent already declared, in the same order. History is
    /// append-only: a candidate tunes by adding a new revision, never by
    /// rewriting or dropping one already on record.
    RecipeHistoryRewritten { index: usize },
    /// A new recipe entry's `name` names no recipe the incumbent already
    /// declared. A candidate may add a new REVISION of an existing
    /// recipe; it may not mint a recipe family that never existed.
    UnknownRecipeTemplate { name: String },
    /// A new recipe entry changed a field this boundary does not let a
    /// recipe tune: `kind` or `scope_within`. What a recipe covers is
    /// fixed; how it executes is what a candidate may retune.
    FixedRecipeFieldChanged { name: String, field: &'static str },
    /// A new recipe entry's `revision` did not strictly advance past the
    /// highest revision the incumbent already declared for that name.
    RevisionNotAdvanced {
        name: String,
        revision: u32,
        base_revision: u32,
    },
    /// A new recipe entry's `tier` is below the floor its own risk rules
    /// and kind demand, re-derived at validation time — never read from a
    /// field the candidate supplies.
    TierBelowFloor {
        name: String,
        tier: Tier,
        floor: Tier,
    },
    /// A new recipe entry's `review` is less cautious than the recipe it
    /// tunes. A candidate may raise a recipe's review floor; it may never
    /// lower one.
    ReviewLowered {
        name: String,
        from: Review,
        to: Review,
    },
    /// A new recipe entry's `models` table names an effort outside the
    /// allowed set. Effort is spend; a learner does not raise its own.
    EffortNotAllowed {
        name: String,
        tier: Tier,
        effort: Effort,
    },
    /// A new recipe entry would grant a capability the bounds withhold.
    /// Distinct from a knob above its cap: no number makes this
    /// admissible, so it is not a matter of degree.
    CapabilityNotGranted {
        name: String,
        capability: &'static str,
    },
    /// A new recipe entry's `models` table names a model outside the
    /// allowed set the caller's [`TuningBounds`] declares.
    ModelNotAllowed {
        name: String,
        tier: Tier,
        model: String,
    },
    /// A new recipe entry's `execution` or `context` names a value above
    /// its cap in [`TuningBounds`].
    KnobAboveCap {
        name: String,
        knob: &'static str,
        cap: u64,
        value: u64,
    },
    /// The candidate's `recipes` table itself is invalid — a duplicate
    /// `(name, revision)` pair, or two recipes declaring the same content
    /// under different names ([`RecipeError`]).
    InvalidRecipe(RecipeError),
}

impl std::fmt::Display for CandidateRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FixedFieldChanged { field } => write!(
                f,
                "policy field `{field}` differs from the incumbent; only `recipes` may change"
            ),
            Self::RecipeHistoryRewritten { index } => write!(
                f,
                "candidate recipes[{index}] does not match the incumbent's declared history; \
                 a candidate may only append new revisions"
            ),
            Self::UnknownRecipeTemplate { name } => write!(
                f,
                "recipe `{name}` names no recipe the incumbent already declared; a candidate \
                 may add a new revision of an existing recipe, not a new one"
            ),
            Self::FixedRecipeFieldChanged { name, field } => write!(
                f,
                "recipe `{name}` changed `{field}`, which a candidate may not tune"
            ),
            Self::RevisionNotAdvanced {
                name,
                revision,
                base_revision,
            } => write!(
                f,
                "recipe `{name}` revision {revision} does not advance past the incumbent's \
                 {base_revision}"
            ),
            Self::TierBelowFloor { name, tier, floor } => write!(
                f,
                "recipe `{name}` tier {} is below the floor {} its own risk rules and kind \
                 demand",
                tier.as_str(),
                floor.as_str()
            ),
            Self::ReviewLowered { name, from, to } => write!(
                f,
                "recipe `{name}` review moved from {from:?} to {to:?}; a candidate may raise a \
                 recipe's review floor, never lower it"
            ),
            Self::EffortNotAllowed { name, tier, effort } => write!(
                f,
                "recipe `{name}` names effort `{effort:?}` for the {tier:?} tier, which the \
                 tuning bounds do not allow: effort is spend, and a learner does not raise its own"
            ),
            Self::CapabilityNotGranted { name, capability } => write!(
                f,
                "recipe `{name}` would grant `{capability}`, which the tuning bounds withhold: \
                 a capability is not a knob, so no value of the caps around it makes this \
                 admissible"
            ),
            Self::ModelNotAllowed { name, tier, model } => write!(
                f,
                "recipe `{name}` names model `{model}` at tier {} outside the allowed set",
                tier.as_str()
            ),
            Self::KnobAboveCap {
                name,
                knob,
                cap,
                value,
            } => write!(
                f,
                "recipe `{name}` sets `{knob}` to {value}, above its cap of {cap}"
            ),
            Self::InvalidRecipe(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CandidateRejection {}

/// Every `RepoPolicy` field but `recipes` must be identical. Destructured
/// field-by-field, without a `..` catch-all, so a field added to
/// `RepoPolicy` later fails this function to COMPILE rather than silently
/// becoming tunable — the property the acceptance criteria asks for is
/// enforced by the compiler, not by a reviewer remembering to update a
/// list.
fn fixed_fields_match(
    incumbent: &RepoPolicy,
    candidate: &RepoPolicy,
) -> Result<(), CandidateRejection> {
    let RepoPolicy {
        schema_version: i_schema_version,
        models: i_models,
        execution: i_execution,
        context: i_context,
        integrations: i_integrations,
        verification: i_verification,
        risk: i_risk,
        architecture: i_architecture,
        recipes: _incumbent_recipes,
    } = incumbent;
    let RepoPolicy {
        schema_version: c_schema_version,
        models: c_models,
        execution: c_execution,
        context: c_context,
        integrations: c_integrations,
        verification: c_verification,
        risk: c_risk,
        architecture: c_architecture,
        recipes: _candidate_recipes,
    } = candidate;

    if i_schema_version != c_schema_version {
        return Err(CandidateRejection::FixedFieldChanged {
            field: "schema_version",
        });
    }
    if i_models != c_models {
        return Err(CandidateRejection::FixedFieldChanged { field: "models" });
    }
    if i_execution != c_execution {
        return Err(CandidateRejection::FixedFieldChanged { field: "execution" });
    }
    if i_context != c_context {
        return Err(CandidateRejection::FixedFieldChanged { field: "context" });
    }
    if i_integrations != c_integrations {
        return Err(CandidateRejection::FixedFieldChanged {
            field: "integrations",
        });
    }
    if i_verification != c_verification {
        return Err(CandidateRejection::FixedFieldChanged {
            field: "verification",
        });
    }
    if i_risk != c_risk {
        return Err(CandidateRejection::FixedFieldChanged { field: "risk" });
    }
    if i_architecture != c_architecture {
        return Err(CandidateRejection::FixedFieldChanged {
            field: "architecture",
        });
    }
    Ok(())
}

/// `candidate_recipes` must start with every `incumbent_recipes` entry, in
/// order (history is append-only), and every entry appended past that
/// point must be a validly tuned new revision of a recipe the incumbent
/// already declared.
fn validate_recipe_history(
    incumbent_recipes: &[RecipeSpec],
    candidate_recipes: &[RecipeSpec],
    candidate_risk: &[RiskRule],
    bounds: &TuningBounds,
) -> Result<(), CandidateRejection> {
    if candidate_recipes.len() < incumbent_recipes.len() {
        return Err(CandidateRejection::RecipeHistoryRewritten {
            index: candidate_recipes.len(),
        });
    }
    for (index, (inc, cand)) in incumbent_recipes
        .iter()
        .zip(candidate_recipes.iter())
        .enumerate()
    {
        if inc != cand {
            return Err(CandidateRejection::RecipeHistoryRewritten { index });
        }
    }
    for new_recipe in &candidate_recipes[incumbent_recipes.len()..] {
        validate_new_recipe(incumbent_recipes, new_recipe, candidate_risk, bounds)?;
    }
    validate_recipes(candidate_recipes).map_err(CandidateRejection::InvalidRecipe)?;
    Ok(())
}

fn validate_new_recipe(
    incumbent_recipes: &[RecipeSpec],
    new: &RecipeSpec,
    candidate_risk: &[RiskRule],
    bounds: &TuningBounds,
) -> Result<(), CandidateRejection> {
    let base = incumbent_recipes
        .iter()
        .filter(|recipe| recipe.name == new.name)
        .max_by_key(|recipe| recipe.revision)
        .ok_or_else(|| CandidateRejection::UnknownRecipeTemplate {
            name: new.name.clone(),
        })?;

    // EXHAUSTIVE DESTRUCTURE, deliberately. Naming only the fields to
    // check made the recipe boundary tunable by DEFAULT — the inverse of
    // what `fixed_fields_match` does for `RepoPolicy`. Measured before
    // this landed: a candidate revision setting
    // `execution.allow_nested_agents = true` was ACCEPTED, so a learner
    // could have turned on nested agent spawning. `effort` was unbounded
    // the same way. Binding every field means a field added to
    // `RecipeSpec` later fails to compile here until someone decides
    // whether it is a knob or fixed.
    let RecipeSpec {
        name: new_name,
        kind: new_kind,
        scope_within: new_scope,
        tier: new_tier,
        revision: new_revision,
        enabled: _enabled_is_a_knob,
        models: new_models,
        execution: new_execution,
        context: new_context,
        review: new_review_field,
    } = new;

    if new_kind != &base.kind {
        return Err(CandidateRejection::FixedRecipeFieldChanged {
            name: new_name.clone(),
            field: "kind",
        });
    }
    if new_scope != &base.scope_within {
        return Err(CandidateRejection::FixedRecipeFieldChanged {
            name: new_name.clone(),
            field: "scope_within",
        });
    }
    if *new_revision <= base.revision {
        return Err(CandidateRejection::RevisionNotAdvanced {
            name: new_name.clone(),
            revision: *new_revision,
            base_revision: base.revision,
        });
    }

    let floor = recipe_tier_floor(*new_kind, new_scope, candidate_risk);
    if *new_tier < floor {
        return Err(CandidateRejection::TierBelowFloor {
            name: new_name.clone(),
            tier: *new_tier,
            floor,
        });
    }

    // `Review::default()` is `Optional`, NOT `Off`. Reading an absent
    // review as `Off` let `None -> Some(Off)` through as "no change",
    // and `RecipeSpec::review` is not read yet — so that accepted
    // candidate would have become a lowering the moment a later release
    // resolves `None` through the type's own default.
    let base_review = base.review.unwrap_or_default();
    let new_review = new_review_field.unwrap_or_default();
    if new_review < base_review {
        return Err(CandidateRejection::ReviewLowered {
            name: new_name.clone(),
            from: base_review,
            to: new_review,
        });
    }

    if let Some(models) = new_models {
        for (tier, profile) in models {
            let ModelProfile { id, effort } = profile;
            if !bounds.allowed_models.contains(id) {
                return Err(CandidateRejection::ModelNotAllowed {
                    name: new_name.clone(),
                    tier: *tier,
                    model: id.clone(),
                });
            }
            // `effort` was unbounded: a candidate could raise every
            // tier's effort, which is spend a learner must not choose
            // for itself.
            if let Some(effort) = effort {
                if !bounds.allowed_efforts.contains(effort) {
                    return Err(CandidateRejection::EffortNotAllowed {
                        name: new_name.clone(),
                        tier: *tier,
                        effort: *effort,
                    });
                }
            }
        }
    }

    if let Some(execution) = new_execution {
        let ExecutionPolicy {
            max_attempts,
            max_repairs_before_escalation,
            max_wall_seconds,
            allow_nested_agents,
            max_agent_depth,
            max_agents_total,
        } = execution;
        check_cap(
            new_name,
            "execution.max_attempts",
            u64::from(*max_attempts),
            u64::from(bounds.max_attempts),
        )?;
        check_cap(
            new_name,
            "execution.max_repairs_before_escalation",
            u64::from(*max_repairs_before_escalation),
            u64::from(bounds.max_repairs_before_escalation),
        )?;
        check_cap(
            new_name,
            "execution.max_wall_seconds",
            *max_wall_seconds,
            bounds.max_wall_seconds,
        )?;
        check_cap(
            new_name,
            "execution.max_agent_depth",
            u64::from(*max_agent_depth),
            u64::from(bounds.max_agent_depth),
        )?;
        check_cap(
            new_name,
            "execution.max_agents_total",
            u64::from(*max_agents_total),
            u64::from(bounds.max_agents_total),
        )?;
        // NOT a knob. Nested agents are a capability, not a number to
        // tune: a candidate may not grant one the incumbent withheld.
        if *allow_nested_agents && !bounds.allow_nested_agents {
            return Err(CandidateRejection::CapabilityNotGranted {
                name: new_name.clone(),
                capability: "execution.allow_nested_agents",
            });
        }
    }

    if let Some(context) = new_context {
        let ContextPolicy { budget_bytes } = context;
        check_cap(
            new_name,
            "context.budget_bytes",
            *budget_bytes as u64,
            bounds.max_context_budget_bytes as u64,
        )?;
    }

    Ok(())
}

fn check_cap(
    name: &str,
    knob: &'static str,
    value: u64,
    cap: u64,
) -> Result<(), CandidateRejection> {
    if value > cap {
        return Err(CandidateRejection::KnobAboveCap {
            name: name.to_string(),
            knob,
            cap,
            value,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    /// Nested agent spawning is a capability, not a knob, and a candidate
    /// may not grant itself one the bounds withhold.
    ///
    /// This was ACCEPTED before the exhaustive destructure landed —
    /// measured, not supposed: `validate_new_recipe` checked only the
    /// fields it named, so a revision setting
    /// `execution.allow_nested_agents = true` came back
    /// `Ok(CandidateRecipe { .. })` and a learner could have turned on
    /// nested spawning. That is what "tunable by default" cost, and why
    /// every sub-struct is now destructured field by field.
    #[test]
    fn a_candidate_cannot_grant_itself_nested_agents() {
        let incumbent = policy_with_recipes(vec![base_recipe()]);
        let mut candidate = incumbent.clone();
        let mut tuned = base_recipe();
        tuned.revision = 1;
        let mut execution = incumbent.execution.clone();
        assert!(
            !execution.allow_nested_agents,
            "the incumbent must withhold it for this to mean anything"
        );
        execution.allow_nested_agents = true;
        tuned.execution = Some(execution);
        candidate.recipes.push(tuned);
        assert!(
            matches!(
                validate_candidate(&incumbent, &candidate, &bounds()),
                Err(CandidateRejection::CapabilityNotGranted {
                    capability: "execution.allow_nested_agents",
                    ..
                })
            ),
            "a withheld capability is refused by name, not by a cap on some number near it"
        );
    }

    /// Effort is spend. A candidate may not raise its own beyond the
    /// bounds, which `effort` being unbound previously allowed.
    #[test]
    fn a_candidate_cannot_raise_its_own_effort_past_the_bounds() {
        let incumbent = policy_with_recipes(vec![base_recipe()]);
        let mut candidate = incumbent.clone();
        let mut tuned = base_recipe();
        tuned.revision = 1;
        tuned.models = Some(BTreeMap::from([(
            Tier::Implementation,
            ModelProfile {
                id: "sonnet".into(),
                effort: Some(Effort::High),
            },
        )]));
        candidate.recipes.push(tuned);
        assert!(
            matches!(
                validate_candidate(&incumbent, &candidate, &bounds()),
                Err(CandidateRejection::EffortNotAllowed {
                    effort: Effort::High,
                    ..
                })
            ),
            "an effort outside the allowed set is refused"
        );
    }

    /// `Review::default()` is `Optional`, not `Off`. Reading an absent
    /// review as `Off` let `None -> Some(Off)` pass as "no change", and
    /// `RecipeSpec::review` is not read yet — so the accepted candidate
    /// would have become a lowering the moment a release resolves `None`
    /// through the type's own default.
    #[test]
    fn an_absent_review_is_optional_not_off_so_setting_off_lowers_it() {
        let mut base = base_recipe();
        base.review = None;
        let incumbent = policy_with_recipes(vec![base]);
        let mut candidate = incumbent.clone();
        let mut tuned = base_recipe();
        tuned.revision = 1;
        tuned.review = Some(Review::Off);
        candidate.recipes.push(tuned);
        assert!(
            matches!(
                validate_candidate(&incumbent, &candidate, &bounds()),
                Err(CandidateRejection::ReviewLowered {
                    from: Review::Optional,
                    to: Review::Off,
                    ..
                })
            ),
            "None resolves through Review::default() = Optional, so Off is a lowering"
        );
    }
    use std::collections::BTreeMap;

    use proptest::prelude::*;
    use proptest::strategy::ValueTree;

    use crate::contract::Kind;
    use crate::policy::{
        ArchitectureConfig, ContextPolicy, Dependency, DependencyMode, Effort, ExecutionPolicy,
        Integrations, ModelProfile, RiskRule, VerificationPolicy,
    };

    use super::*;

    fn bounds() -> TuningBounds {
        TuningBounds {
            allowed_models: ["haiku", "sonnet", "fable"]
                .into_iter()
                .map(String::from)
                .collect(),
            max_attempts: 5,
            max_repairs_before_escalation: 2,
            max_wall_seconds: 3600,
            max_agent_depth: 4,
            max_agents_total: 32,
            max_context_budget_bytes: 128 * 1024,
            allowed_efforts: vec![Effort::Low, Effort::Medium],
            // Withheld, which is the interesting default for a test: a
            // candidate must not be able to grant itself nested agents.
            allow_nested_agents: false,
        }
    }

    fn base_recipe() -> RecipeSpec {
        RecipeSpec {
            kind: Some(Kind::Change),
            scope_within: vec!["docs/**".into()],
            ..RecipeSpec::covering("docs-touchup", Tier::Implementation)
        }
    }

    fn policy_with_recipes(recipes: Vec<RecipeSpec>) -> RepoPolicy {
        RepoPolicy {
            schema_version: crate::policy::POLICY_SCHEMA_VERSION,
            models: BTreeMap::new(),
            execution: ExecutionPolicy::default(),
            context: ContextPolicy::default(),
            integrations: Integrations::default(),
            verification: VerificationPolicy::default(),
            risk: Vec::new(),
            architecture: ArchitectureConfig::default(),
            recipes,
        }
    }

    #[test]
    fn identical_policies_are_accepted() {
        let policy = policy_with_recipes(vec![base_recipe()]);
        let accepted = validate_candidate(&policy, &policy, &bounds()).expect("no change admits");
        assert_eq!(accepted.policy(), &policy);
    }

    #[test]
    fn a_fixed_field_touched_is_refused_and_named() {
        let incumbent = policy_with_recipes(vec![base_recipe()]);
        let mut candidate = incumbent.clone();
        candidate.execution.max_attempts += 1;
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::FixedFieldChanged { field: "execution" }
        );
    }

    #[test]
    fn every_repo_policy_field_but_recipes_is_fixed() {
        // One case per `RepoPolicy` field. Destructuring `RepoPolicy` in
        // `fixed_fields_match` without a `..` is what makes a field added
        // there and not here fail to COMPILE rather than passing silently.
        let incumbent = policy_with_recipes(vec![base_recipe()]);

        let mut schema = incumbent.clone();
        schema.schema_version += 1;
        assert_eq!(
            validate_candidate(&incumbent, &schema, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged {
                field: "schema_version"
            }
        );

        let mut models = incumbent.clone();
        models.models.insert(
            Tier::Research,
            ModelProfile {
                id: "haiku".into(),
                effort: None,
            },
        );
        assert_eq!(
            validate_candidate(&incumbent, &models, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged { field: "models" }
        );

        let mut execution = incumbent.clone();
        execution.execution.max_wall_seconds += 1;
        assert_eq!(
            validate_candidate(&incumbent, &execution, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged { field: "execution" }
        );

        let mut context = incumbent.clone();
        context.context.budget_bytes += 1;
        assert_eq!(
            validate_candidate(&incumbent, &context, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged { field: "context" }
        );

        let mut integrations = incumbent.clone();
        integrations.integrations.aval = Some(Dependency::Mode(DependencyMode::Required));
        assert_eq!(
            validate_candidate(&incumbent, &integrations, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged {
                field: "integrations"
            }
        );

        let mut verification = incumbent.clone();
        verification
            .verification
            .profiles
            .insert("default".into(), Default::default());
        assert_eq!(
            validate_candidate(&incumbent, &verification, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged {
                field: "verification"
            }
        );

        let mut risk = incumbent.clone();
        risk.risk.push(RiskRule {
            paths: vec!["**/trust/**".into()],
            minimum_tier: Tier::Escalation,
            review: None,
        });
        assert_eq!(
            validate_candidate(&incumbent, &risk, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged { field: "risk" }
        );

        let mut architecture = incumbent.clone();
        architecture
            .architecture
            .mapping
            .push(crate::policy::ArchitectureMapping {
                paths: vec!["docs/**".into()],
                keys: vec!["decisions:ADR-0001".into()],
                scope: None,
            });
        assert_eq!(
            validate_candidate(&incumbent, &architecture, &bounds()).unwrap_err(),
            CandidateRejection::FixedFieldChanged {
                field: "architecture"
            }
        );
    }

    #[test]
    fn a_new_recipe_with_no_incumbent_family_is_refused() {
        let incumbent = policy_with_recipes(vec![]);
        let candidate = policy_with_recipes(vec![base_recipe()]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::UnknownRecipeTemplate {
                name: "docs-touchup".into()
            }
        );
    }

    #[test]
    fn a_tuned_recipe_must_advance_its_revision() {
        let base = base_recipe();
        let incumbent = policy_with_recipes(vec![base.clone()]);
        let mut tuned = base.clone();
        tuned.tier = Tier::Escalation;
        // revision left at 0: does not advance past the incumbent's 0.
        let candidate = policy_with_recipes(vec![base, tuned]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::RevisionNotAdvanced {
                name: "docs-touchup".into(),
                revision: 0,
                base_revision: 0,
            }
        );
    }

    #[test]
    fn a_tuned_recipe_cannot_change_kind_or_scope() {
        let base = base_recipe();
        let incumbent = policy_with_recipes(vec![base.clone()]);

        let mut kind_changed = base.clone();
        kind_changed.revision = 1;
        kind_changed.kind = Some(Kind::Inspect);
        let candidate = policy_with_recipes(vec![base.clone(), kind_changed]);
        assert_eq!(
            validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err(),
            CandidateRejection::FixedRecipeFieldChanged {
                name: "docs-touchup".into(),
                field: "kind",
            }
        );

        let mut scope_changed = base.clone();
        scope_changed.revision = 1;
        scope_changed.scope_within = vec!["**".into()];
        let candidate = policy_with_recipes(vec![base, scope_changed]);
        assert_eq!(
            validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err(),
            CandidateRejection::FixedRecipeFieldChanged {
                name: "docs-touchup".into(),
                field: "scope_within",
            }
        );
    }

    #[test]
    fn a_tuned_recipe_cannot_lower_its_tier_below_its_own_floor() {
        let base = base_recipe(); // Kind::Change, docs/** -> floor implementation
        let incumbent = policy_with_recipes(vec![base.clone()]);
        let mut tuned = base.clone();
        tuned.revision = 1;
        tuned.tier = Tier::Research; // below the change floor
        let candidate = policy_with_recipes(vec![base, tuned]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::TierBelowFloor {
                name: "docs-touchup".into(),
                tier: Tier::Research,
                floor: Tier::Implementation,
            }
        );
    }

    #[test]
    fn a_tuned_recipe_cannot_lower_its_tier_below_a_risk_rule_it_touches() {
        let base = base_recipe();
        let mut incumbent = policy_with_recipes(vec![base.clone()]);
        incumbent.risk.push(RiskRule {
            paths: vec!["docs/**".into()],
            minimum_tier: Tier::Escalation,
            review: None,
        });
        let mut tuned = base.clone();
        tuned.revision = 1;
        tuned.tier = Tier::Implementation; // below the risk-raised floor
        let mut candidate = incumbent.clone();
        candidate.recipes.push(tuned);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::TierBelowFloor {
                name: "docs-touchup".into(),
                tier: Tier::Implementation,
                floor: Tier::Escalation,
            }
        );
    }

    #[test]
    fn a_tuned_recipe_cannot_lower_its_review() {
        let mut base = base_recipe();
        base.review = Some(Review::Required);
        let incumbent = policy_with_recipes(vec![base.clone()]);
        let mut tuned = base.clone();
        tuned.revision = 1;
        tuned.tier = Tier::Escalation;
        tuned.review = Some(Review::Optional);
        let candidate = policy_with_recipes(vec![base, tuned]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::ReviewLowered {
                name: "docs-touchup".into(),
                from: Review::Required,
                to: Review::Optional,
            }
        );
    }

    #[test]
    fn a_tuned_recipe_cannot_name_a_model_outside_the_allowed_set() {
        let base = base_recipe();
        let incumbent = policy_with_recipes(vec![base.clone()]);
        let mut tuned = base.clone();
        tuned.revision = 1;
        tuned.models = Some(BTreeMap::from([(
            Tier::Implementation,
            ModelProfile {
                id: "not-allowed-model".into(),
                effort: Some(Effort::Medium),
            },
        )]));
        let candidate = policy_with_recipes(vec![base, tuned]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::ModelNotAllowed {
                name: "docs-touchup".into(),
                tier: Tier::Implementation,
                model: "not-allowed-model".into(),
            }
        );
    }

    #[test]
    fn a_tuned_recipe_cannot_set_a_knob_above_its_cap() {
        let base = base_recipe();
        let incumbent = policy_with_recipes(vec![base.clone()]);
        let mut tuned = base.clone();
        tuned.revision = 1;
        tuned.execution = Some(ExecutionPolicy {
            max_wall_seconds: bounds().max_wall_seconds + 1,
            ..ExecutionPolicy::default()
        });
        let candidate = policy_with_recipes(vec![base, tuned]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(
            err,
            CandidateRejection::KnobAboveCap {
                name: "docs-touchup".into(),
                knob: "execution.max_wall_seconds",
                cap: bounds().max_wall_seconds,
                value: bounds().max_wall_seconds + 1,
            }
        );
    }

    #[test]
    fn a_duplicate_recipe_id_is_refused() {
        // Two independent recipe families, tuned to identical content: the
        // resulting entries share a `recipe_id` (which excludes only
        // `name`) despite belonging to different families, each of which
        // legitimately passes the "known template" and "revision advanced"
        // checks on its own.
        let mut family_a = base_recipe();
        family_a.name = "docs-touchup-a".into();
        let mut family_b = base_recipe();
        family_b.name = "docs-touchup-b".into();
        let incumbent = policy_with_recipes(vec![family_a.clone(), family_b.clone()]);

        let mut tuned_a = family_a.clone();
        tuned_a.revision = 1;
        tuned_a.tier = Tier::Escalation;
        let mut tuned_b = family_b.clone();
        tuned_b.revision = 1;
        tuned_b.tier = Tier::Escalation;

        let candidate = policy_with_recipes(vec![family_a, family_b, tuned_a, tuned_b]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert!(
            matches!(err, CandidateRejection::InvalidRecipe(_)),
            "{err:?}"
        );
    }

    #[test]
    fn history_cannot_be_rewritten() {
        let base = base_recipe();
        let incumbent = policy_with_recipes(vec![base.clone()]);
        let mut rewritten = base.clone();
        rewritten.tier = Tier::Escalation; // same (name, revision), different content
        let candidate = policy_with_recipes(vec![rewritten]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(err, CandidateRejection::RecipeHistoryRewritten { index: 0 });
    }

    #[test]
    fn history_cannot_be_dropped() {
        let base = base_recipe();
        let incumbent = policy_with_recipes(vec![base]);
        let candidate = policy_with_recipes(vec![]);
        let err = validate_candidate(&incumbent, &candidate, &bounds()).unwrap_err();
        assert_eq!(err, CandidateRejection::RecipeHistoryRewritten { index: 0 });
    }

    /// `CandidateRecipe` cannot be minted outside its validator, and that
    /// is the COMPILER's job, not a source scan's.
    ///
    /// The field is private to `mod sealed`, which holds only
    /// `validate_candidate`. Adding
    /// `fn forged(p) -> CandidateRecipe { CandidateRecipe { policy: p } }`
    /// anywhere else in this file fails with `error[E0451]: field `policy`
    /// of struct `CandidateRecipe` is private` — verified by doing it.
    ///
    /// The scan this replaces exempted the whole file, so that exact
    /// forgery passed it (also verified). Tightening the scan ran into a
    /// grep being unable to tell a construction from a
    /// `matches!(.., Ok(CandidateRecipe { .. }))` pattern or a mention in
    /// a doc comment — it counted twelve "literals" in this file, one of
    /// which was real.
    ///
    /// What a test still has to cover is the part privacy does not: a
    /// derive or impl handing one out around the check. Rust cannot assert
    /// the ABSENCE of an impl from inside a test, so this reads the
    /// declaration, where such a thing would have to appear.
    #[test]
    fn candidate_recipe_derives_nothing_that_could_mint_one() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("route")
                .join("candidate.rs"),
        )
        .expect("this file is readable");

        let decl = text
            .find("pub struct CandidateRecipe")
            .expect("the type is declared here");
        let derive_start = text[..decl]
            .rfind("#[derive(")
            .expect("it derives something");
        let derives = &text[derive_start..decl];
        for forbidden in ["Default", "Deserialize"] {
            assert!(
                !derives.contains(forbidden),
                "CandidateRecipe must not derive {forbidden}: it would mint one without the \
                 validator. Derives found: {derives}"
            );
        }
        // The needles are ASSEMBLED, never written out. A source-scanning
        // test that spells its own forbidden strings finds ITSELF: the
        // first version of this failed on the literal in its own list.
        let ty = "CandidateRecipe";
        for trait_name in ["Default", "Deserialize<'de>", "Deserialize"] {
            let needle = format!("impl {trait_name} for {ty}");
            assert!(
                !text.contains(&needle),
                "{ty} must not have `{needle}`: it would hand one out around the validator"
            );
        }
        let generic_de = format!("impl<'de> Deserialize<'de> for {ty}");
        assert!(
            !text.contains(&generic_de),
            "{ty} must not have `{generic_de}`"
        );
        let from_impl = format!("for {ty} {{\n    fn from");
        assert!(!text.contains(&from_impl), "{ty} must not have a From impl");
    }

    fn arb_review() -> impl Strategy<Value = Review> {
        prop_oneof![
            Just(Review::Off),
            Just(Review::Optional),
            Just(Review::Required),
        ]
    }

    fn arb_tier() -> impl Strategy<Value = Tier> {
        prop_oneof![
            Just(Tier::Research),
            Just(Tier::Implementation),
            Just(Tier::Escalation),
        ]
    }

    fn arb_scope() -> impl Strategy<Value = Vec<String>> {
        prop_oneof![
            Just(vec!["docs/**".to_string()]),
            Just(vec!["crates/relais/**".to_string()]),
            Just(vec!["**".to_string()]),
        ]
    }

    /// The floor PRODUCTION would derive for a recipe's kind and scope,
    /// built here from `kind_floor` and `risk_floor` directly.
    ///
    /// Deliberately not a call to `recipe_tier_floor`: a property that
    /// recomputes its expectation with the function it is testing cannot
    /// fail when that function is wrong. This mirrors `eligible_tiers`,
    /// which applies `risk_floor` on top of `kind_floor` for EVERY kind —
    /// including `Inspect`, which `recipe_tier_floor` used to return early
    /// for, skipping risk entirely.
    fn expected_floor_via_production(
        kind: Option<Kind>,
        scope_within: &[String],
        risk: &[RiskRule],
    ) -> Tier {
        let effective_kind = kind.unwrap_or(Kind::Change);
        let floor = crate::route::kind_floor(effective_kind);
        let patterns = if scope_within.is_empty() {
            vec!["**".to_string()]
        } else {
            scope_within.to_vec()
        };
        let task = match effective_kind {
            Kind::Inspect => crate::contract::Task::Inspect,
            Kind::Change => match crate::contract::Task::change(patterns) {
                Ok(task) => task,
                Err(_) => return Tier::Escalation,
            },
        };
        let contract = crate::contract::TaskContract {
            schema_version: crate::contract::SCHEMA_VERSION,
            task,
            objective: String::new(),
            base_ref: String::new(),
            read_hints: Vec::new(),
            acceptance: Vec::new(),
            verification_profile: String::new(),
            architecture: Default::default(),
            risk_hints: Vec::new(),
            limits: Default::default(),
            review: Default::default(),
            decomposition: None,
            task_id: None,
        };
        let (rule_floor, _) = crate::route::risk_floor(&contract, risk);
        match rule_floor {
            Some(rule_floor) if rule_floor > floor => rule_floor,
            _ => floor,
        }
    }

    /// Risk rules for the incumbent. Non-empty cases are the point: with
    /// `risk: Vec::new()` the risk-derived floor — the whole reason
    /// `recipe_tier_floor` exists beyond `kind_floor` — is never reached,
    /// and both properties below passed with `risk_floor` deleted
    /// outright (verified by deleting it).
    fn arb_risk(scope: Vec<String>) -> impl Strategy<Value = Vec<RiskRule>> {
        prop_oneof![
            Just(Vec::new()),
            Just(vec![RiskRule {
                paths: scope.clone(),
                minimum_tier: Tier::Escalation,
                review: Some(Review::Required),
            }]),
            Just(vec![RiskRule {
                paths: scope,
                minimum_tier: Tier::Implementation,
                review: None,
            }]),
        ]
    }

    /// How a candidate might tamper with something fixed. Without this the
    /// byte-identity assertions compared a candidate that was
    /// `incumbent.clone()` against the incumbent — a property that cannot
    /// fail and therefore says nothing.
    #[derive(Debug, Clone, Copy)]
    enum Tamper {
        Nothing,
        AddRiskRule,
        DropRiskRules,
        WeakenVerification,
        EditArchitecture,
        EditIntegrations,
    }

    fn arb_tamper() -> impl Strategy<Value = Tamper> {
        prop_oneof![
            Just(Tamper::Nothing),
            Just(Tamper::AddRiskRule),
            Just(Tamper::DropRiskRules),
            Just(Tamper::WeakenVerification),
            Just(Tamper::EditArchitecture),
            Just(Tamper::EditIntegrations),
        ]
    }

    fn apply_tamper(policy: &mut RepoPolicy, tamper: Tamper) {
        match tamper {
            Tamper::Nothing => {}
            Tamper::AddRiskRule => policy.risk.push(RiskRule {
                paths: vec!["**".to_string()],
                minimum_tier: Tier::Research,
                review: None,
            }),
            Tamper::DropRiskRules => policy.risk.clear(),
            Tamper::WeakenVerification => {
                policy.verification = crate::policy::VerificationPolicy::default();
            }
            Tamper::EditArchitecture => {
                policy.architecture = crate::policy::ArchitectureConfig {
                    mapping: vec![crate::policy::ArchitectureMapping {
                        paths: vec!["**".to_string()],
                        keys: vec!["tampered".to_string()],
                        scope: None,
                    }],
                };
            }
            Tamper::EditIntegrations => {
                policy.integrations = crate::policy::Integrations {
                    aval: None,
                    amont: None,
                    amont_agent: None,
                };
            }
        }
    }

    /// A tuned recipe entry, built from `base` by moving only the knobs
    /// this boundary calls tunable, each within `bounds`. `respect_floor`
    /// clamps the tier at or above the floor this module itself computes
    /// with the SAME `recipe_tier_floor` `validate_candidate` calls — the
    /// property below asks what happens astride that floor from both
    /// sides, not two independent guesses at where it is.
    fn arb_tuned(
        base: RecipeSpec,
        risk: Vec<RiskRule>,
        respect_floor: bool,
    ) -> impl Strategy<Value = RecipeSpec> {
        let floor = recipe_tier_floor(base.kind, &base.scope_within, &risk);
        (arb_tier(), arb_review(), 1u64..8, 0u64..8000, any::<bool>()).prop_map(
            move |(tier, review, wall_extra, wall_bytes_extra, lower_tier)| {
                let tier = if respect_floor {
                    tier.max(floor)
                } else if lower_tier {
                    // Deliberately produce a tier below the floor, when
                    // one exists below it, to give the adversarial
                    // property something to refuse.
                    match floor {
                        Tier::Research => Tier::Research,
                        Tier::Implementation => Tier::Research,
                        Tier::Escalation => Tier::Implementation,
                    }
                } else {
                    tier
                };
                let mut tuned = base.clone();
                tuned.revision = base.revision + 1;
                tuned.tier = tier;
                tuned.review = Some(review.max(base.review.unwrap_or_default()));
                // Inside the cap ON PURPOSE. This was `1200 + 1..8 +
                // 0..8000`, above the 3600 cap about 70% of the time, so
                // most cases were refused by `KnobAboveCap` and the
                // adversarial property's `is_err()` passed without the
                // floor check ever being consulted. A cap rejection must
                // not stand in for a floor rejection.
                tuned.execution = Some(ExecutionPolicy {
                    max_wall_seconds: 1200 + (wall_extra + wall_bytes_extra) % 2_000,
                    ..ExecutionPolicy::default()
                });
                tuned
            },
        )
    }

    proptest! {
        /// For every `Ok` result, the candidate's `risk`, `verification`,
        /// `architecture` and `integrations` serialize byte-identically
        /// to the incumbent's, and no recipe's tier is below its floor.
        /// Over the validator's OUTPUT, not over hand-picked inputs: the
        /// strategy below generates recipes whose tier is free to fall
        /// below the floor, and the property still holds because a
        /// candidate that does so is never `Ok`.
        #[test]
        fn accepted_candidates_never_loosen_what_protects_the_run(
            scope in arb_scope(),
            kind_is_change in any::<bool>(),
            risk in arb_scope().prop_flat_map(arb_risk),
            tamper in arb_tamper(),
        ) {
            let kind = if kind_is_change { Kind::Change } else { Kind::Inspect };
            let mut base = base_recipe();
            base.kind = Some(kind);
            base.scope_within = scope;
            let mut incumbent = policy_with_recipes(vec![base.clone()]);
            // Generated, not always empty. With `risk: Vec::new()` the
            // risk-derived floor was never reached and this property
            // passed with `risk_floor` deleted.
            incumbent.risk = risk;

            let tuned_strategy = arb_tuned(base.clone(), incumbent.risk.clone(), false);
            let mut runner = proptest::test_runner::TestRunner::default();
            for _ in 0..64 {
                let tuned = tuned_strategy.new_tree(&mut runner).unwrap().current();
                let mut candidate = incumbent.clone();
                candidate.recipes.push(tuned.clone());
                // A candidate that may also have touched something fixed,
                // so the byte-identity assertions below have something
                // they could catch. Before this they compared
                // `incumbent.clone()` with the incumbent.
                apply_tamper(&mut candidate, tamper);

                if let Ok(accepted) = validate_candidate(&incumbent, &candidate, &bounds()) {
                    let accepted_policy = accepted.policy();
                    prop_assert_eq!(
                        serde_json::to_value(&accepted_policy.risk).unwrap(),
                        serde_json::to_value(&incumbent.risk).unwrap(),
                    );
                    prop_assert_eq!(
                        serde_json::to_value(&accepted_policy.verification).unwrap(),
                        serde_json::to_value(&incumbent.verification).unwrap(),
                    );
                    prop_assert_eq!(
                        serde_json::to_value(&accepted_policy.architecture).unwrap(),
                        serde_json::to_value(&incumbent.architecture).unwrap(),
                    );
                    prop_assert_eq!(
                        serde_json::to_value(&accepted_policy.integrations).unwrap(),
                        serde_json::to_value(&incumbent.integrations).unwrap(),
                    );
                    // Only the entries the CANDIDATE added. Scoping this
                    // to every recipe in the accepted policy was wrong,
                    // and the property said so on its first real run:
                    // with a risk rule of `** -> Escalation` the
                    // incumbent's OWN pre-existing recipe sits below the
                    // floor, because risk rules can be tightened after a
                    // recipe is written. `validate_candidate` asks whether
                    // the candidate loosens anything, not whether the
                    // incumbent was already compliant — refusing every
                    // candidate on a policy that needs fixing would make
                    // the boundary unusable exactly when it is needed.
                    // Routing applies `risk_floor` again at dispatch, so
                    // an under-floor incumbent recipe is refused there.
                    for recipe in &accepted_policy.recipes {
                        let is_new = !incumbent
                            .recipes
                            .iter()
                            .any(|old| old.name == recipe.name && old.revision == recipe.revision);
                        if !is_new {
                            continue;
                        }
                        // The expectation is computed INDEPENDENTLY, from
                        // `kind_floor` and `risk_floor` directly — never by
                        // calling `recipe_tier_floor`, the function under
                        // test. Calling it made the property
                        // self-referential: it compared the validator's
                        // floor against itself, so deleting `risk_floor`
                        // from `recipe_tier_floor` outright left BOTH
                        // proptests green (verified twice by deleting it).
                        // Deriving the floor here the way production does
                        // is what turns this into a cross-check, and it is
                        // what catches `recipe_tier_floor` drifting from
                        // the rule routing actually applies.
                        let floor = expected_floor_via_production(
                            recipe.kind,
                            &recipe.scope_within,
                            &accepted_policy.risk,
                        );
                        prop_assert!(
                            recipe.tier >= floor,
                            "accepted a NEW recipe at {:?} below the floor production would \
                             derive, {:?}",
                            recipe.tier,
                            floor
                        );
                    }
                }
            }
        }

        /// The adversarial case stated directly: no generated candidate
        /// that lowers a tier below its floor, weakens verification, or
        /// edits risk rules can yield `Ok`. A scorer optimising freely
        /// against `validate_candidate` must not be able to reach a
        /// policy that loosens what protects the run.
        #[test]
        fn no_candidate_that_loosens_protection_is_ever_accepted(
            scope in arb_scope(),
            kind_is_change in any::<bool>(),
            weaken_verification in any::<bool>(),
            edit_risk in any::<bool>(),
        ) {
            let kind = if kind_is_change { Kind::Change } else { Kind::Inspect };
            let mut base = base_recipe();
            base.kind = Some(kind);
            base.scope_within = scope;
            let incumbent = policy_with_recipes(vec![base.clone()]);
            let risk = incumbent.risk.clone();

            let tuned_strategy = arb_tuned(base, risk, false);
            let mut runner = proptest::test_runner::TestRunner::default();
            let tuned = tuned_strategy.new_tree(&mut runner).unwrap().current();
            let mut candidate = incumbent.clone();
            candidate.recipes.push(tuned.clone());

            let below_floor = {
                let floor = recipe_tier_floor(tuned.kind, &tuned.scope_within, &candidate.risk);
                tuned.tier < floor
            };

            if weaken_verification {
                candidate
                    .verification
                    .profiles
                    .insert("weakened".into(), Default::default());
            }
            if edit_risk {
                candidate.risk.push(RiskRule {
                    paths: vec!["**".into()],
                    minimum_tier: Tier::Research,
                    review: None,
                });
            }

            let result = validate_candidate(&incumbent, &candidate, &bounds());
            if below_floor || weaken_verification || edit_risk {
                prop_assert!(result.is_err());
            }
        }
    }
}
