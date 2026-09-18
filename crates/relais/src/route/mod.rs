//! Routing (SPEC §6): a pure function over validated inputs.
//!
//! Eligibility, risk floors and acceptance are deterministic. Within the
//! eligible profiles, an owned, Relais-trained predictor estimates
//! acceptance and total cost; with insufficient trained evidence the
//! configured conservative baseline runs and outcomes are collected. No
//! silent fallback: unavailable models yield `blocked:model_unavailable`,
//! and unapproved provider substitution stops further dispatch. File
//! count alone never determines risk; complexity and consequence are
//! separate. Unclassified writes use the conservative configured route,
//! never the research tier.

use std::collections::BTreeMap;

use crate::contract::{Kind, Review, TaskContract};
use crate::money::MicroUsd;
use crate::policy::{Blocker, EffectiveAuthority, MachineSettings, RepoPolicy, Tier};

/// Estimates from an owned, Relais-trained artifact (SPEC §16). The
/// predictor abstains (returns `None`) when it has no supported coverage
/// for the eligible profiles — the router then uses the conservative
/// baseline and records why.
pub trait RoutePredictor {
    fn estimate(
        &self,
        contract: &TaskContract,
        authority: &EffectiveAuthority,
        eligible: &[Tier],
    ) -> Option<Estimates>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Estimates {
    pub artifact_id: String,
    /// Acceptance-without-escalation estimate per tier; complete-strategy
    /// cost per tier. Predictions are not guarantees; the deterministic
    /// runner still owns policy and acceptance.
    pub acceptance: BTreeMap<Tier, f64>,
    pub cost: BTreeMap<Tier, MicroUsd>,
}

pub struct RouteInputs<'a> {
    pub contract: &'a TaskContract,
    pub repo: &'a RepoPolicy,
    pub machine: &'a MachineSettings,
    pub authority: &'a EffectiveAuthority,
    pub predictor: Option<&'a dyn RoutePredictor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RouteDecision {
    /// None when routing is blocked before any dispatch.
    pub tier: Option<Tier>,
    pub reason_ids: Vec<String>,
    pub reasons: Vec<String>,
    pub review: Review,
    pub max_attempts: u32,
    pub max_repairs_before_escalation: u32,
    pub escalation_tier: Option<Tier>,
    pub blocked: Vec<Blocker>,
    /// Whether a learned artifact actually chose the tier (vs. baseline).
    pub routed_by: RoutedBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutedBy {
    /// Explicitly configured deterministic recipe covered the task.
    DeterministicRecipe,
    /// Risk floors alone decided (mandatory high-risk work).
    RiskFloor,
    /// A validated learned artifact decided among eligible profiles.
    LearnedArtifact,
    /// Conservative baseline; trained evidence absent or insufficient.
    ConservativeBaseline,
}

/// Conservative scope-vs-pattern overlap. Routing happens before any
/// diff exists, so floors are computed from the DECLARED scope patterns
/// (SPEC §6). The test is deliberately biased toward firing: over-applying
/// a floor costs an escalation tier, under-applying one silently routes a
/// sensitive write to a cheap model, which is the failure the spec
/// forbids.
pub(crate) fn scope_could_touch(scope: &str, pattern: &str) -> bool {
    let scope = scope.trim_start_matches("./");
    let pattern = pattern.trim_start_matches("./");
    if scope.starts_with("**") || pattern.starts_with("**") {
        return true;
    }
    let scope_segments: Vec<&str> = scope.split('/').collect();
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    for (s, p) in scope_segments.iter().zip(pattern_segments.iter()) {
        if *s == "**" || *p == "**" {
            return true;
        }
        if s == p {
            continue;
        }
        if s.contains('*') || p.contains('*') {
            return true;
        }
        return false;
    }
    true
}

pub fn write_scope_could_touch(contract: &TaskContract, pattern: &str) -> bool {
    contract
        .write_scope
        .as_deref()
        .is_some_and(|scopes| scopes.iter().any(|scope| scope_could_touch(scope, pattern)))
}

/// The risk floor from declared scope and repository rules: the highest
/// minimum tier among rules whose paths the declared scope could touch.
fn risk_floor(contract: &TaskContract, repo: &RepoPolicy) -> (Option<Tier>, Vec<String>) {
    let mut floor: Option<Tier> = None;
    let mut rule_ids = Vec::new();
    for (index, rule) in repo.risk.iter().enumerate() {
        let touches =
            write_scope_could_touch(contract, rule.paths.first().unwrap_or(&String::new()))
                || rule
                    .paths
                    .iter()
                    .any(|pattern| write_scope_could_touch(contract, pattern));
        if touches {
            rule_ids.push(format!("risk[{}]:{}", index, rule.minimum_tier.as_str()));
            floor = Some(match floor {
                Some(current) if current >= rule.minimum_tier => current,
                _ => rule.minimum_tier,
            });
        }
    }
    (floor, rule_ids)
}

/// An explicitly configured deterministic recipe, used only when it FULLY
/// covers the task (SPEC §6.3). Recipes are never inferred from prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub name: String,
    pub kind: Option<Kind>,
    /// Contract scope must fall entirely within these patterns.
    pub scope_within: Vec<String>,
    pub tier: Tier,
}

impl From<&crate::policy::RecipeSpec> for Recipe {
    fn from(spec: &crate::policy::RecipeSpec) -> Self {
        Recipe {
            name: spec.name.clone(),
            kind: spec.kind,
            scope_within: spec.scope_within.clone(),
            tier: spec.tier,
        }
    }
}

fn recipe_covers(contract: &TaskContract, recipe: &Recipe) -> bool {
    if let Some(kind) = recipe.kind {
        if contract.kind != kind {
            return false;
        }
    }
    if recipe.scope_within.is_empty() {
        return true;
    }
    contract.write_scope.as_deref().is_some_and(|scopes| {
        scopes.iter().all(|scope| {
            recipe
                .scope_within
                .iter()
                .any(|cover| scope_could_touch(scope, cover))
        })
    })
}

pub fn route(inputs: RouteInputs<'_>) -> RouteDecision {
    let RouteInputs {
        contract,
        repo,
        machine,
        authority,
        predictor,
    } = inputs;

    let mut reasons = Vec::new();
    let mut reason_ids = Vec::new();

    if !authority.blockers.is_empty() {
        return RouteDecision {
            tier: None,
            reason_ids: vec!["preflight_blockers".into()],
            reasons: vec![format!(
                "blocked before dispatch by {} authority blocker(s)",
                authority.blockers.len()
            )],
            review: authority.review_floor,
            max_attempts: 0,
            max_repairs_before_escalation: 0,
            escalation_tier: None,
            blocked: authority.blockers.clone(),
            routed_by: RoutedBy::ConservativeBaseline,
        };
    }

    // Complexity and consequence are separate (SPEC §6): a change starts
    // at the implementation tier even when the diff will be one line.
    let mut floor = match contract.kind {
        Kind::Change => Tier::Implementation,
        Kind::Inspect => Tier::Research,
    };
    match contract.kind {
        Kind::Change => {
            reasons.push("bounded change; conservative route floor is implementation".into());
            reason_ids.push("kind_change_floor".into());
        }
        Kind::Inspect => {
            reasons.push("inspection task; research tier eligible".into());
            reason_ids.push("kind_inspect_floor".into());
        }
    }

    let (rule_floor, rule_ids) = risk_floor(contract, repo);
    reason_ids.extend(rule_ids);
    if let Some(rule_floor) = rule_floor {
        if rule_floor > floor {
            reasons.push(format!(
                "risk floor: scope touches configured high-risk paths ({} required)",
                rule_floor.as_str()
            ));
            floor = rule_floor;
        }
    }

    // Worker-supplied risk hints increase caution, never lower floors
    // (SPEC §4). They cannot lift a tier — only the evidence of a
    // validated artifact can — but they do force review.
    let mut review = authority.review_floor;
    if !contract.risk_hints.is_empty() {
        reason_ids.push("risk_hints".into());
        reasons.push(format!(
            "risk hints supplied: {}",
            contract.risk_hints.join(", ")
        ));
        if review < Review::Required {
            review = Review::Required;
            reasons.push("risk hints raise review to required".into());
        }
    }

    // Eligible tiers: floor and everything above it that is configured.
    let eligible: Vec<Tier> = [Tier::Research, Tier::Implementation, Tier::Escalation]
        .into_iter()
        .filter(|tier| *tier >= floor && authority.models.contains_key(tier))
        .collect();

    if eligible.is_empty() {
        return RouteDecision {
            tier: None,
            reason_ids: vec!["model_unavailable".into()],
            reasons: vec![format!(
                "no model configured at or above the {} floor; no silent fallback is allowed",
                floor.as_str()
            )],
            review,
            max_attempts: 0,
            max_repairs_before_escalation: 0,
            escalation_tier: None,
            blocked: vec![Blocker {
                code: "model_unavailable".into(),
                detail: format!(
                    "no model configured at or above the {} floor",
                    floor.as_str()
                ),
            }],
            routed_by: RoutedBy::ConservativeBaseline,
        };
    }

    // Deterministic recipes win when they fully cover the task (SPEC §6.3).
    let recipe_recipes: Vec<Recipe> = repo.recipes.iter().map(Recipe::from).collect();
    let selected: (Tier, RoutedBy) = if let Some(recipe) = recipe_recipes
        .iter()
        .find(|recipe| eligible.contains(&recipe.tier) && recipe_covers(contract, recipe))
    {
        reasons.push(format!(
            "deterministic recipe `{}` fully covers the task",
            recipe.name
        ));
        (recipe.tier, RoutedBy::DeterministicRecipe)
    } else if let (true, Some(predictor)) = (machine.routing.learned_enabled, predictor) {
        match predictor.estimate(contract, authority, &eligible) {
            Some(estimates) => {
                let quality_floor = machine.routing.quality_floor.unwrap_or(0.75);
                match select_learned(&estimates, &eligible, quality_floor) {
                    Some(tier) => {
                        reasons.push(format!(
                            "learned artifact {} estimated acceptance/cost and selected {}",
                            estimates.artifact_id,
                            tier.as_str()
                        ));
                        (tier, RoutedBy::LearnedArtifact)
                    }
                    None => {
                        reasons.push(
                                "learned estimates did not clear the quality floor; conservative baseline".into(),
                            );
                        (eligible[0], RoutedBy::ConservativeBaseline)
                    }
                }
            }
            None => {
                reasons.push(
                        "no supported trained coverage; conservative baseline while outcomes are collected".into(),
                    );
                (eligible[0], RoutedBy::ConservativeBaseline)
            }
        }
    } else {
        reasons.push("cold start: conservative configured baseline".into());
        (eligible[0], RoutedBy::ConservativeBaseline)
    };

    let escalation_tier = eligible
        .iter()
        .rev()
        .find(|tier| **tier > selected.0)
        .copied();

    let max_attempts = authority.max_attempts;

    RouteDecision {
        tier: Some(selected.0),
        reason_ids,
        reasons,
        review,
        max_attempts,
        max_repairs_before_escalation: authority.max_repairs_before_escalation,
        escalation_tier,
        blocked: Vec::new(),
        routed_by: selected.1,
    }
}

fn select_learned(estimates: &Estimates, eligible: &[Tier], quality_floor: f64) -> Option<Tier> {
    eligible
        .iter()
        .filter(|tier| {
            estimates
                .acceptance
                .get(*tier)
                .is_some_and(|acceptance| *acceptance >= quality_floor)
        })
        .min_by_key(|tier| estimates.cost.get(*tier).map(|cost| cost.to_micros()))
        .copied()
}

impl RouteDecision {
    /// The explanation block (SPEC §6 example shape). Reasons are joined;
    /// blocked routes list their codes instead of a route line.
    pub fn explain(&self, model: Option<&str>) -> String {
        let mut out = String::new();
        match (self.tier, model) {
            (Some(tier), Some(model)) => {
                out.push_str(&format!("route: {} / {}\n", tier.as_str(), model));
                out.push_str(&format!("reason: {}\n", self.reasons.join("; ")));
                match self.review {
                    Review::Required => out.push_str("review: required\n"),
                    Review::Optional => out.push_str("review: optional\n"),
                    Review::Off => out.push_str("review: off\n"),
                }
                let repairs = self.max_repairs_before_escalation;
                match self.escalation_tier {
                    Some(escalation) => out.push_str(&format!(
                        "on failure: {repairs} repair(s), then escalation to {}\n",
                        escalation.as_str()
                    )),
                    None => out.push_str(&format!(
                        "on failure: {repairs} repair(s), then fail (no stronger tier authorized)\n"
                    )),
                }
                out.push_str(&format!("max attempts: {}\n", self.max_attempts));
            }
            _ => {
                out.push_str("route: blocked\n");
                for blocker in &self.blocked {
                    out.push_str(&format!("blocked: {} — {}\n", blocker.code, blocker.detail));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::ContractError;
    use crate::policy::{
        effective_authority, CommandSpec, ConcurrencyLimits, Dependency, DependencyMode,
        ExecutionPolicy, ModelProfile, RiskRule, VerificationPolicy, VerificationProfile,
    };

    fn repo_policy() -> RepoPolicy {
        RepoPolicy {
            schema_version: 1,
            models: BTreeMap::from([
                (
                    Tier::Research,
                    ModelProfile {
                        id: "haiku".into(),
                        effort: None,
                    },
                ),
                (
                    Tier::Implementation,
                    ModelProfile {
                        id: "sonnet".into(),
                        effort: Some(crate::policy::Effort::Medium),
                    },
                ),
                (
                    Tier::Escalation,
                    ModelProfile {
                        id: "fable".into(),
                        effort: Some(crate::policy::Effort::Medium),
                    },
                ),
            ]),
            execution: ExecutionPolicy {
                max_attempts: 3,
                max_repairs_before_escalation: 1,
                max_wall_seconds: 1200,
                allow_nested_agents: true,
                max_agent_depth: 3,
                max_agents_total: 24,
            },
            integrations: Default::default(),
            verification: VerificationPolicy {
                profiles: BTreeMap::from([(
                    "rust-change".into(),
                    VerificationProfile {
                        commands: vec![CommandSpec {
                            argv: vec!["make".into(), "check".into()],
                            timeout_seconds: 300,
                        }],
                        amont_checks: Vec::new(),
                    },
                )]),
            },
            risk: Vec::new(),
            architecture: Default::default(),
            recipes: Vec::new(),
        }
    }

    fn machine_for(repo: &RepoPolicy) -> MachineSettings {
        let mut machine = MachineSettings {
            schema_version: 1,
            allowed_models: None,
            spending: Default::default(),
            trust: BTreeMap::new(),
            permissions: Default::default(),
            concurrency: ConcurrencyLimits::default(),
            trials: Default::default(),
            routing: Default::default(),
        };
        machine.trust.insert(
            repo.authority_hash(),
            crate::policy::TrustGrant {
                granted_at: "2026-09-18".into(),
                reviewed_by: None,
                note: None,
            },
        );
        machine
    }

    fn change_contract(scope: &[&str]) -> TaskContract {
        let scope: Vec<String> = scope.iter().map(|s| s.to_string()).collect();
        TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": "Fix JSON escaping",
                "base_ref": "HEAD",
                "write_scope": scope,
                "acceptance": ["output parses"],
                "verification_profile": "rust-change",
            })
            .to_string(),
        )
        .expect("contract parses")
    }

    fn inspect_contract() -> TaskContract {
        TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1,
                "kind": "inspect",
                "objective": "Investigate why a check is inactive",
                "base_ref": "HEAD",
                "acceptance": ["evidence of why the check is inert"],
                "verification_profile": "rust-change",
            })
            .to_string(),
        )
        .expect("contract parses")
    }

    fn route_with(
        contract: &TaskContract,
        repo: &RepoPolicy,
        machine: &MachineSettings,
    ) -> RouteDecision {
        let authority = effective_authority(repo, machine, contract);
        route(RouteInputs {
            contract,
            repo,
            machine,
            authority: &authority,
            predictor: None,
        })
    }

    #[test]
    fn inspect_contracts_parse_without_write_scope() {
        let c = inspect_contract();
        assert_eq!(c.kind, Kind::Inspect);
        let _ = ContractError::EmptyObjective;
    }

    #[test]
    fn change_routes_to_the_conservative_floor() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        assert_eq!(d.tier, Some(Tier::Implementation));
        assert!(d.blocked.is_empty());
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
        assert_eq!(d.max_attempts, 3);
        assert_eq!(d.escalation_tier, Some(Tier::Escalation));
    }

    #[test]
    fn inspect_routes_to_research() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let d = route_with(&inspect_contract(), &repo, &machine);
        assert_eq!(d.tier, Some(Tier::Research));
    }

    #[test]
    fn risk_rule_raises_the_floor_to_escalation() {
        let mut repo = repo_policy();
        repo.risk.push(RiskRule {
            paths: vec!["**/trust/**".into()],
            minimum_tier: Tier::Escalation,
            review: Some(Review::Required),
        });
        let machine = machine_for(&repo);
        let d = route_with(
            &change_contract(&["crates/amont/trust/**"]),
            &repo,
            &machine,
        );
        assert_eq!(d.tier, Some(Tier::Escalation));
        assert!(d
            .reason_ids
            .iter()
            .any(|id| id.starts_with("risk[0]:escalation")));
        assert_eq!(d.review, Review::Required);
    }

    #[test]
    fn broader_scope_than_a_rule_still_fires_the_floor() {
        let mut repo = repo_policy();
        repo.risk.push(RiskRule {
            paths: vec!["**/trust/**".into()],
            minimum_tier: Tier::Escalation,
            review: None,
        });
        let machine = machine_for(&repo);
        // `crates/**` could touch `**/trust/**`: the overlap test must fire.
        let d = route_with(&change_contract(&["crates/**"]), &repo, &machine);
        assert_eq!(
            d.tier,
            Some(Tier::Escalation),
            "declared scope that COULD touch a rule area must take the floor"
        );
    }

    #[test]
    fn disjoint_scope_leaves_the_floor_alone() {
        let mut repo = repo_policy();
        repo.risk.push(RiskRule {
            paths: vec!["crates/other/**".into()],
            minimum_tier: Tier::Escalation,
            review: None,
        });
        let machine = machine_for(&repo);
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        assert_eq!(d.tier, Some(Tier::Implementation));
    }

    #[test]
    fn risk_hints_raise_review_but_never_lower_floors() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let mut c = change_contract(&["crates/amont/**"]);
        c.risk_hints = vec!["public-output-contract".into()];
        let d = route_with(&c, &repo, &machine);
        assert_eq!(
            d.tier,
            Some(Tier::Implementation),
            "hints cannot buy escalation"
        );
        assert_eq!(d.review, Review::Required);
    }

    #[test]
    fn missing_model_blocks_without_silent_fallback() {
        let mut repo = repo_policy();
        repo.models.remove(&Tier::Implementation);
        repo.models.remove(&Tier::Escalation);
        // Trust grant must match the changed authority.
        let machine = machine_for(&repo);
        let mut machine = machine;
        machine.allowed_models = None;
        machine.trust.insert(
            repo.authority_hash(),
            crate::policy::TrustGrant {
                granted_at: "2026-09-18".into(),
                reviewed_by: None,
                note: None,
            },
        );
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        assert!(d.blocked.iter().any(|b| b.code == "model_unavailable"));
        assert_eq!(d.tier, None);
    }

    #[test]
    fn authority_blockers_stop_routing_before_dispatch() {
        let repo = repo_policy();
        let machine = MachineSettings {
            schema_version: 1,
            allowed_models: None,
            spending: Default::default(),
            trust: BTreeMap::new(),
            permissions: Default::default(),
            concurrency: ConcurrencyLimits::default(),
            trials: Default::default(),
            routing: Default::default(),
        };
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        assert!(d.blocked.iter().any(|b| b.code == "missing_trust_grant"));
        assert_eq!(d.tier, None);
    }

    struct FixedPredictor(Vec<(Tier, f64, i64)>);

    impl RoutePredictor for FixedPredictor {
        fn estimate(
            &self,
            _contract: &TaskContract,
            _authority: &EffectiveAuthority,
            eligible: &[Tier],
        ) -> Option<Estimates> {
            let mut acceptance = BTreeMap::new();
            let mut cost = BTreeMap::new();
            for tier in eligible {
                if let Some((_, acceptance_value, cost_micros)) = self
                    .0
                    .iter()
                    .find(|(predictor_tier, _, _)| predictor_tier == tier)
                {
                    acceptance.insert(*tier, *acceptance_value);
                    cost.insert(*tier, MicroUsd::from_micros(*cost_micros));
                }
            }
            if acceptance.is_empty() {
                return None;
            }
            Some(Estimates {
                artifact_id: "artifact-test-1".into(),
                acceptance,
                cost,
            })
        }
    }

    #[test]
    fn learned_artifact_selects_the_cheapest_tier_that_clears_quality() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let authority =
            effective_authority(&repo, &machine, &change_contract(&["crates/amont/**"]));
        // Research clears the floor and is cheaper; implementation clears it
        // too but costs more. The change-task floor is implementation, so
        // research is NOT eligible despite the estimate.
        let predictor = FixedPredictor(vec![
            (Tier::Research, 0.9, 100),
            (Tier::Implementation, 0.9, 400),
            (Tier::Escalation, 0.95, 4000),
        ]);
        let d = route(RouteInputs {
            contract: &change_contract(&["crates/amont/**"]),
            repo: &repo,
            machine: &machine,
            authority: &authority,
            predictor: Some(&predictor),
        });
        assert_eq!(d.tier, Some(Tier::Implementation));
        assert_eq!(d.routed_by, RoutedBy::LearnedArtifact);
    }

    #[test]
    fn learned_artifact_below_quality_floor_falls_back_to_baseline() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let authority =
            effective_authority(&repo, &machine, &change_contract(&["crates/amont/**"]));
        let predictor = FixedPredictor(vec![
            (Tier::Implementation, 0.40, 300),
            (Tier::Escalation, 0.40, 900),
        ]);
        let d = route(RouteInputs {
            contract: &change_contract(&["crates/amont/**"]),
            repo: &repo,
            machine: &machine,
            authority: &authority,
            predictor: Some(&predictor),
        });
        assert_eq!(d.tier, Some(Tier::Implementation));
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
    }

    #[test]
    fn abstaining_predictor_uses_the_conservative_baseline() {
        struct Abstainer;
        impl RoutePredictor for Abstainer {
            fn estimate(
                &self,
                _contract: &TaskContract,
                _authority: &EffectiveAuthority,
                _eligible: &[Tier],
            ) -> Option<Estimates> {
                None
            }
        }
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let authority =
            effective_authority(&repo, &machine, &change_contract(&["crates/amont/**"]));
        let d = route(RouteInputs {
            contract: &change_contract(&["crates/amont/**"]),
            repo: &repo,
            machine: &machine,
            authority: &authority,
            predictor: Some(&Abstainer),
        });
        assert_eq!(d.tier, Some(Tier::Implementation));
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
    }

    #[test]
    fn explanation_matches_the_spec_shape() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        let text = d.explain(Some("sonnet"));
        assert!(
            text.starts_with("route: implementation / sonnet\n"),
            "{text}"
        );
        assert!(text.contains("review: optional"));
        assert!(text.contains("on failure: 1 repair(s), then escalation to escalation"));
    }

    #[test]
    fn blocked_explanation_lists_codes() {
        let repo = repo_policy();
        let machine = MachineSettings {
            schema_version: 1,
            allowed_models: None,
            spending: Default::default(),
            trust: BTreeMap::new(),
            permissions: Default::default(),
            concurrency: ConcurrencyLimits::default(),
            trials: Default::default(),
            routing: Default::default(),
        };
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        let text = d.explain(None);
        assert!(text.starts_with("route: blocked\n"), "{text}");
        assert!(text.contains("blocked: missing_trust_grant"));
    }

    #[test]
    fn dependency_enum_round_trip() {
        assert_eq!(
            Dependency::Mode(DependencyMode::Optional).mode(),
            DependencyMode::Optional
        );
    }

    #[test]
    fn deterministic_recipe_preferred_when_it_fully_covers() {
        let mut repo = repo_policy();
        repo.recipes.push(crate::policy::RecipeSpec {
            name: "amont-runtime-docs".into(),
            kind: Some(Kind::Change),
            scope_within: vec!["docs/**".into()],
            tier: Tier::Implementation,
        });
        let machine = machine_for(&repo);
        let docs = change_contract(&["docs/guide.md"]);
        let d = route_with(&docs, &repo, &machine);
        assert_eq!(d.routed_by, RoutedBy::DeterministicRecipe);
        assert_eq!(d.tier, Some(Tier::Implementation));

        // Scope reaching outside the recipe does not count as coverage.
        let straddling = change_contract(&["docs/**", "crates/**"]);
        let d = route_with(&straddling, &repo, &machine);
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
    }
}
