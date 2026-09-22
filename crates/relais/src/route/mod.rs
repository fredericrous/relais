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

use crate::contract::scope::{scope_contained_in, write_scope_could_touch};
use crate::contract::{Kind, Review, TaskContract};
use crate::money::MicroUsd;
use crate::policy::{
    BlockCode, Blocker, EffectiveAuthority, MachineSettings, ModelProfile, RepoPolicy, Tier,
};

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
    /// Hash of the exact inputs the artifact saw, so the ledger's
    /// prediction row can be matched to a later outcome.
    pub input_hash: String,
    /// Acceptance-without-escalation estimate per tier; complete-strategy
    /// cost per tier. Predictions are not guarantees; the deterministic
    /// runner still owns policy and acceptance.
    pub acceptance: BTreeMap<Tier, f64>,
    pub cost: BTreeMap<Tier, MicroUsd>,
    /// The full inference result, recorded as evidence.
    pub raw: serde_json::Value,
}

pub struct RouteInputs<'a> {
    pub contract: &'a TaskContract,
    pub repo: &'a RepoPolicy,
    pub machine: &'a MachineSettings,
    pub authority: &'a EffectiveAuthority,
    pub predictor: Option<&'a dyn RoutePredictor>,
}

/// One reason the router gives for what it did: a stable id the ledger
/// and the tests match on, and the sentence a person reads. They were
/// two parallel `Vec<String>`s pushed in different places, and they
/// drifted — the risk floor pushed ids with no text, a recipe pushed
/// text with no id — so neither list could be read as an explanation of
/// the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteReason {
    pub id: String,
    pub text: String,
}

impl RouteReason {
    /// One reason: the id the ledger and the tests match on, and the
    /// sentence `explain` prints for it. Both, always — that is the
    /// point of the type.
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

/// What the router decided: a route, or a refusal to route. There is no
/// third state and no combination of the two — a decision with no tier
/// used to carry a `blocked` list the caller indexed at `[0]` and hoped
/// was there.
#[derive(Debug, Clone, PartialEq)]
pub enum Routed {
    Route(Route),
    Blocked(Blocked),
}

/// A task that will be dispatched, and on what terms (SPEC §6).
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub tier: Tier,
    /// The stronger tier a failure may escalate to, when policy
    /// authorizes one.
    pub escalation_tier: Option<Tier>,
    pub review: Review,
    pub max_attempts: u32,
    pub max_repairs_before_escalation: u32,
    /// Whether a learned artifact actually chose the tier (vs. baseline).
    pub routed_by: RoutedBy,
    /// What the artifact estimated, when one was consulted — recorded by
    /// the runner as a prediction row whatever it decided.
    pub estimates: Option<Estimates>,
    pub reasons: Vec<RouteReason>,
}

/// A task that will not be dispatched at all, and why. At least one
/// blocker, by construction: a blocked route with nothing blocking it
/// is not a state this crate can build.
#[derive(Debug, Clone, PartialEq)]
pub struct Blocked {
    blockers: Vec<Blocker>,
    pub review: Review,
    pub reasons: Vec<RouteReason>,
}

impl Blocked {
    /// The first blocker is the one the run reports; `rest` is whatever
    /// else preflight found. Taking the first one by value is what makes
    /// the list non-empty.
    pub fn new(
        first: Blocker,
        rest: Vec<Blocker>,
        review: Review,
        reasons: Vec<RouteReason>,
    ) -> Self {
        let mut blockers = Vec::with_capacity(1 + rest.len());
        blockers.push(first);
        blockers.extend(rest);
        Self {
            blockers,
            review,
            reasons,
        }
    }

    /// Every blocker preflight found, first one first.
    pub fn blockers(&self) -> &[Blocker] {
        &self.blockers
    }

    /// The blocker the run ends on.
    pub fn first(&self) -> &Blocker {
        self.blockers
            .first()
            .expect("Blocked::new always stores the first blocker")
    }
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

/// The risk floor from declared scope and repository rules: the highest
/// minimum tier among rules whose paths the declared scope could touch.
///
/// A rule with no paths touches nothing. It used to fire on any contract
/// whose scope was `**`, through a `paths.first().unwrap_or(&String::new())`
/// disjunct that asked whether the scope could touch the empty pattern —
/// a rule nobody wrote, applied to the one scope that matches everything.
/// Policy validation rejects a pathless rule outright.
fn risk_floor(contract: &TaskContract, repo: &RepoPolicy) -> (Option<Tier>, Vec<RouteReason>) {
    let mut floor: Option<Tier> = None;
    let mut fired = Vec::new();
    for (index, rule) in repo.risk.iter().enumerate() {
        let touches = rule
            .paths
            .iter()
            .any(|pattern| write_scope_could_touch(contract, pattern));
        if touches {
            fired.push(RouteReason::new(
                format!("risk[{}]:{}", index, rule.minimum_tier.as_str()),
                format!(
                    "risk rule {index} ({}) requires at least the {} tier",
                    rule.paths.join(", "),
                    rule.minimum_tier.as_str()
                ),
            ));
            floor = Some(match floor {
                Some(current) if current >= rule.minimum_tier => current,
                Some(_) | None => rule.minimum_tier,
            });
        }
    }
    (floor, fired)
}

/// An explicitly configured deterministic recipe, used only when it FULLY
/// covers the task (SPEC §6, step 3). Recipes are never inferred from
/// prose.
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

/// The tiers a task may be dispatched to, and the floor they start at.
///
/// One answer, used by the router when it dispatches and by the evaluator
/// when it asks what the router WOULD have done: an evaluator with its own
/// notion of eligibility measures a policy nothing will ever run.
#[derive(Debug, Clone, PartialEq)]
pub struct Eligibility {
    /// The lowest tier the task may run at: its kind's conservative floor,
    /// raised by any risk rule the declared scope could touch.
    pub floor: Tier,
    /// Every configured tier at or above the floor, cheapest first. Empty
    /// when policy configures no model there — the router blocks rather
    /// than falling back silently.
    pub tiers: Vec<Tier>,
    /// Why the floor is where it is, in the router's own words.
    pub reasons: Vec<RouteReason>,
}

/// The conservative floor a task's KIND implies (SPEC §6): complexity and
/// consequence are separate, so a change starts at the implementation tier
/// even when the diff will be one line.
pub fn kind_floor(kind: Kind) -> Tier {
    match kind {
        Kind::Change => Tier::Implementation,
        Kind::Inspect => Tier::Research,
    }
}

/// Every configured tier at or above `floor`, cheapest first. The ladder
/// is fixed (research < implementation < escalation); `configured` says
/// which rungs policy names a model for.
pub fn tiers_at_or_above(floor: Tier, configured: &[Tier]) -> Vec<Tier> {
    [Tier::Research, Tier::Implementation, Tier::Escalation]
        .into_iter()
        .filter(|tier| *tier >= floor && configured.contains(tier))
        .collect()
}

/// Eligibility for a contract under a repository's rules: the kind floor,
/// raised by the risk rules its declared scope could touch, intersected
/// with the tiers `configured` names a model for. Pure: the caller passes
/// the model table it means — the router the effective authority's,
/// dataset construction the repository policy's.
pub fn eligible_tiers(
    contract: &TaskContract,
    repo: &RepoPolicy,
    configured: &std::collections::BTreeMap<Tier, ModelProfile>,
) -> Eligibility {
    let mut reasons = Vec::new();
    let mut floor = kind_floor(contract.kind());
    match contract.kind() {
        Kind::Change => reasons.push(RouteReason::new(
            "kind_change_floor",
            "bounded change; conservative route floor is implementation",
        )),
        Kind::Inspect => reasons.push(RouteReason::new(
            "kind_inspect_floor",
            "inspection task; research tier eligible",
        )),
    }
    let (rule_floor, fired_rules) = risk_floor(contract, repo);
    reasons.extend(fired_rules);
    if let Some(rule_floor) = rule_floor {
        if rule_floor > floor {
            reasons.push(RouteReason::new(
                "risk_floor",
                format!(
                    "risk floor: scope touches configured high-risk paths ({} required)",
                    rule_floor.as_str()
                ),
            ));
            floor = rule_floor;
        }
    }
    let keys: Vec<Tier> = configured.keys().copied().collect();
    Eligibility {
        floor,
        tiers: tiers_at_or_above(floor, &keys),
        reasons,
    }
}

fn recipe_covers(contract: &TaskContract, recipe: &Recipe) -> bool {
    if let Some(kind) = recipe.kind {
        if contract.kind() != kind {
            return false;
        }
    }
    if recipe.scope_within.is_empty() {
        return true;
    }
    // FULLY covers (SPEC §6, step 3): every pattern the contract may write must
    // be contained in some recipe pattern.
    let scopes = contract.scope_patterns();
    !scopes.is_empty()
        && scopes.iter().all(|scope| {
            recipe
                .scope_within
                .iter()
                .any(|cover| scope_contained_in(scope, cover))
        })
}

pub fn route(inputs: RouteInputs<'_>) -> Routed {
    let RouteInputs {
        contract,
        repo,
        machine,
        authority,
        predictor,
    } = inputs;

    let mut reasons = Vec::new();

    if let Some((first, rest)) = authority.blockers.split_first() {
        return Routed::Blocked(Blocked::new(
            first.clone(),
            rest.to_vec(),
            authority.review_floor,
            vec![RouteReason::new(
                "preflight_blockers",
                format!(
                    "blocked before dispatch by {} authority blocker(s)",
                    authority.blockers.len()
                ),
            )],
        ));
    }

    // Eligibility — the kind's floor, raised by risk rules, intersected
    // with the configured models — is computed by the same function the
    // evaluator uses, so a promoted artifact is measured over the tiers
    // the router would actually have offered it.
    let Eligibility {
        floor,
        tiers: eligible,
        reasons: eligibility_reasons,
    } = eligible_tiers(contract, repo, &authority.models);
    reasons.extend(eligibility_reasons);

    // Worker-supplied risk hints increase caution, never lower floors
    // (SPEC §4). They cannot lift a tier — only the evidence of a
    // validated artifact can — but they do force review.
    let mut review = authority.review_floor;
    if !contract.risk_hints.is_empty() {
        reasons.push(RouteReason::new(
            "risk_hints",
            format!("risk hints supplied: {}", contract.risk_hints.join(", ")),
        ));
        if review < Review::Required {
            review = Review::Required;
            reasons.push(RouteReason::new(
                "risk_hints_review",
                "risk hints raise review to required",
            ));
        }
    }

    let Some(&cheapest_eligible) = eligible.first() else {
        reasons.push(RouteReason::new(
            "model_unavailable",
            format!(
                "no model configured at or above the {} floor; no silent fallback is allowed",
                floor.as_str()
            ),
        ));
        return Routed::Blocked(Blocked::new(
            Blocker {
                code: BlockCode::ModelUnavailable,
                detail: format!(
                    "no model configured at or above the {} floor",
                    floor.as_str()
                ),
            },
            Vec::new(),
            review,
            reasons,
        ));
    };

    // Deterministic recipes win when they fully cover the task
    // (SPEC §6, step 3).
    let mut estimates_seen: Option<Estimates> = None;
    let recipe_recipes: Vec<Recipe> = repo.recipes.iter().map(Recipe::from).collect();
    let selected: (Tier, RoutedBy) = if let Some(recipe) = recipe_recipes
        .iter()
        .find(|recipe| eligible.contains(&recipe.tier) && recipe_covers(contract, recipe))
    {
        reasons.push(RouteReason::new(
            "deterministic_recipe",
            format!(
                "deterministic recipe `{}` fully covers the task",
                recipe.name
            ),
        ));
        (recipe.tier, RoutedBy::DeterministicRecipe)
    } else if let (true, Some(predictor)) = (machine.routing.learned_enabled, predictor) {
        match predictor.estimate(contract, authority, &eligible) {
            Some(estimates) => {
                let quality_floor = machine.routing.quality_floor.unwrap_or(0.75);
                let selection = select_learned(&estimates, &eligible, quality_floor);
                estimates_seen = Some(estimates.clone());
                match selection {
                    Some(tier) => {
                        reasons.push(RouteReason::new(
                            "learned_artifact",
                            format!(
                                "learned artifact {} estimated acceptance/cost and selected {}",
                                estimates.artifact_id,
                                tier.as_str()
                            ),
                        ));
                        (tier, RoutedBy::LearnedArtifact)
                    }
                    None => {
                        reasons.push(RouteReason::new(
                            "below_quality_floor",
                            "learned estimates did not clear the quality floor; conservative \
                             baseline",
                        ));
                        (cheapest_eligible, RoutedBy::ConservativeBaseline)
                    }
                }
            }
            None => {
                reasons.push(RouteReason::new(
                    "no_trained_coverage",
                    "no supported trained coverage; conservative baseline while outcomes are \
                     collected",
                ));
                (cheapest_eligible, RoutedBy::ConservativeBaseline)
            }
        }
    } else {
        reasons.push(RouteReason::new(
            "cold_start",
            "cold start: conservative configured baseline",
        ));
        (cheapest_eligible, RoutedBy::ConservativeBaseline)
    };

    let escalation_tier = eligible
        .iter()
        .rev()
        .find(|tier| **tier > selected.0)
        .copied();

    Routed::Route(Route {
        tier: selected.0,
        escalation_tier,
        review,
        max_attempts: authority.max_attempts,
        max_repairs_before_escalation: authority.max_repairs_before_escalation,
        routed_by: selected.1,
        estimates: estimates_seen,
        reasons,
    })
}

/// The tier a learned artifact selects: the cheapest ELIGIBLE tier whose
/// estimated acceptance clears the quality floor and that the artifact can
/// price. `None` abstains to the conservative baseline. The evaluator calls
/// this too, so what it measures is what the router will do.
pub fn select_learned(
    estimates: &Estimates,
    eligible: &[Tier],
    quality_floor: f64,
) -> Option<Tier> {
    eligible
        .iter()
        .filter(|tier| {
            estimates
                .acceptance
                .get(*tier)
                .is_some_and(|acceptance| *acceptance >= quality_floor)
        })
        .filter_map(|tier| {
            estimates
                .cost
                .get(tier)
                .map(|cost| (tier, cost.to_micros()))
        })
        .min_by_key(|(_, cost_micros)| *cost_micros)
        .map(|(tier, _)| *tier)
}

impl Routed {
    /// The explanation block (SPEC §6 example shape), whichever this is.
    pub fn explain(&self, model: Option<&str>) -> String {
        match self {
            Self::Route(route) => route.explain(model),
            Self::Blocked(blocked) => blocked.explain(),
        }
    }
}

impl Route {
    /// The route's terms, in the shape SPEC §6 gives as an example. The
    /// model is the one policy names for the selected tier; when nothing
    /// names one the tier still prints, because the tuple match this
    /// replaces fell through to the blocked arm and reported a perfectly
    /// good route as blocked.
    pub fn explain(&self, model: Option<&str>) -> String {
        let mut out = match model {
            Some(model) => format!("route: {} / {model}\n", self.tier.as_str()),
            None => format!("route: {}\n", self.tier.as_str()),
        };
        out.push_str(&format!("reason: {}\n", joined(&self.reasons)));
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
        out
    }
}

impl Blocked {
    /// Why nothing will be dispatched: the reasons first, then every
    /// blocker. The reasons used to be dropped here, so a blocked task
    /// printed codes and no explanation of them.
    pub fn explain(&self) -> String {
        let mut out = String::from("route: blocked\n");
        out.push_str(&format!("reason: {}\n", joined(&self.reasons)));
        for blocker in self.blockers() {
            out.push_str(&format!("blocked: {} — {}\n", blocker.code, blocker.detail));
        }
        out
    }
}

fn joined(reasons: &[RouteReason]) -> String {
    reasons
        .iter()
        .map(|reason| reason.text.as_str())
        .collect::<Vec<_>>()
        .join("; ")
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
            context: Default::default(),
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
                        setup: Vec::new(),
                        commands: vec![CommandSpec {
                            argv: vec!["make".into(), "check".into()],
                            timeout_seconds: 300,
                        }],
                        amont_checks: Vec::new(),
                        amont_waivers: Vec::new(),
                        inputs: Vec::new(),
                        cache_baseline: false,
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
            crate::policy::grant_key(&repo.authority_hash(), &identity()),
            crate::policy::TrustGrant {
                granted_at: "2026-09-18".into(),
                reviewed_by: "a reviewer".into(),
                note: None,
                repo: None,
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

    fn identity() -> crate::policy::RepoIdentity {
        crate::policy::RepoIdentity::common_dir(std::path::Path::new("/repos/relais/.git"))
    }

    fn decide(contract: &TaskContract, repo: &RepoPolicy, machine: &MachineSettings) -> Routed {
        let authority = effective_authority(repo, machine, contract, &identity());
        route(RouteInputs {
            contract,
            repo,
            machine,
            authority: &authority,
            predictor: None,
        })
    }

    /// The route these inputs produce, or a failure naming what blocked.
    fn route_with(contract: &TaskContract, repo: &RepoPolicy, machine: &MachineSettings) -> Route {
        expect_route(decide(contract, repo, machine))
    }

    fn expect_route(decision: Routed) -> Route {
        match decision {
            Routed::Route(route) => route,
            Routed::Blocked(blocked) => {
                panic!("expected a route, blocked by {:?}", blocked.blockers())
            }
        }
    }

    fn expect_blocked(decision: Routed) -> Blocked {
        match decision {
            Routed::Blocked(blocked) => blocked,
            Routed::Route(route) => panic!("expected blockers, routed to {:?}", route.tier),
        }
    }

    #[test]
    fn inspect_contracts_parse_without_write_scope() {
        let c = inspect_contract();
        assert_eq!(c.kind(), Kind::Inspect);
        let _ = ContractError::EmptyObjective;
    }

    #[test]
    fn change_routes_to_the_conservative_floor() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        assert_eq!(d.tier, Tier::Implementation);
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
        assert_eq!(d.max_attempts, 3);
        assert_eq!(d.escalation_tier, Some(Tier::Escalation));
    }

    #[test]
    fn inspect_routes_to_research() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let d = route_with(&inspect_contract(), &repo, &machine);
        assert_eq!(d.tier, Tier::Research);
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
        assert_eq!(d.tier, Tier::Escalation);
        assert!(d
            .reasons
            .iter()
            .any(|reason| reason.id.starts_with("risk[0]:escalation")));
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
            Tier::Escalation,
            "declared scope that COULD touch a rule area must take the floor"
        );
    }

    #[test]
    fn a_risk_rules_review_floor_is_scoped_to_its_paths() {
        let mut repo = repo_policy();
        repo.risk.push(RiskRule {
            paths: vec!["**/trust/**".into()],
            minimum_tier: Tier::Escalation,
            review: Some(Review::Required),
        });
        let machine = machine_for(&repo);
        let d = route_with(&change_contract(&["docs/README.md"]), &repo, &machine);
        assert_eq!(d.tier, Tier::Implementation);
        assert_eq!(
            d.review,
            Review::Optional,
            "a rule the scope cannot touch does not force review"
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
        assert_eq!(d.tier, Tier::Implementation);
    }

    /// L15: a rule with no paths used to fire on every `**` contract,
    /// through a disjunct that tested the declared scope against the empty
    /// pattern. A rule that names no path governs no path. Policy
    /// validation refuses such a rule (`PolicyError::EmptyRiskPaths`);
    /// this is the second line of that defence, for a policy built in
    /// memory rather than parsed.
    #[test]
    fn a_risk_rule_with_no_paths_never_fires() {
        assert!(
            matches!(
                RepoPolicy::from_toml_str(
                    "schema_version = 1\n[[risk]]\npaths = []\nminimum_tier = \"escalation\"\n"
                ),
                Err(crate::policy::PolicyError::EmptyRiskPaths)
            ),
            "a pathless risk rule is refused at the policy boundary"
        );
        let mut repo = repo_policy();
        repo.risk.push(RiskRule {
            paths: Vec::new(),
            minimum_tier: Tier::Escalation,
            review: None,
        });
        let machine = machine_for(&repo);
        let everything = route_with(&change_contract(&["**"]), &repo, &machine);
        assert_eq!(
            everything.tier,
            Tier::Implementation,
            "a pathless rule may not raise the floor of a whole-repository scope"
        );
        assert!(
            !everything
                .reasons
                .iter()
                .any(|reason| reason.id.starts_with("risk[")),
            "no rule fired: {:?}",
            everything.reasons
        );
    }

    /// The evaluator and the router must agree on what is eligible; both
    /// go through `eligible_tiers`.
    #[test]
    fn eligibility_is_the_floor_and_every_configured_tier_above_it() {
        let mut repo = repo_policy();
        repo.risk.push(RiskRule {
            paths: vec!["**/trust/**".into()],
            minimum_tier: Tier::Escalation,
            review: None,
        });
        let machine = machine_for(&repo);
        let contract = change_contract(&["crates/amont/trust/**"]);
        let authority = effective_authority(&repo, &machine, &contract, &identity());
        let eligibility = eligible_tiers(&contract, &repo, &authority.models);
        assert_eq!(eligibility.floor, Tier::Escalation);
        assert_eq!(eligibility.tiers, vec![Tier::Escalation]);

        let inspect = inspect_contract();
        let authority = effective_authority(&repo, &machine, &inspect, &identity());
        let eligibility = eligible_tiers(&inspect, &repo, &authority.models);
        assert_eq!(eligibility.floor, Tier::Research);
        assert_eq!(
            eligibility.tiers,
            vec![Tier::Research, Tier::Implementation, Tier::Escalation]
        );
        assert_eq!(
            tiers_at_or_above(Tier::Implementation, &[Tier::Research, Tier::Escalation]),
            vec![Tier::Escalation],
            "an unconfigured tier is not eligible, and nothing below the floor is"
        );
    }

    #[test]
    fn risk_hints_raise_review_but_never_lower_floors() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let mut c = change_contract(&["crates/amont/**"]);
        c.risk_hints = vec!["public-output-contract".into()];
        let d = route_with(&c, &repo, &machine);
        assert_eq!(d.tier, Tier::Implementation, "hints cannot buy escalation");
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
            crate::policy::grant_key(&repo.authority_hash(), &identity()),
            crate::policy::TrustGrant {
                granted_at: "2026-09-18".into(),
                reviewed_by: "a reviewer".into(),
                note: None,
                repo: None,
            },
        );
        let d = expect_blocked(decide(
            &change_contract(&["crates/amont/**"]),
            &repo,
            &machine,
        ));
        assert!(d
            .blockers()
            .iter()
            .any(|b| b.code == BlockCode::ModelUnavailable));
        assert_eq!(d.first().code, BlockCode::ModelUnavailable);
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
        let d = expect_blocked(decide(
            &change_contract(&["crates/amont/**"]),
            &repo,
            &machine,
        ));
        assert!(d
            .blockers()
            .iter()
            .any(|b| b.code == BlockCode::MissingTrustGrant));
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
                input_hash: "in".into(),
                acceptance,
                cost,
                raw: serde_json::Value::Null,
            })
        }
    }

    #[test]
    fn learned_artifact_selects_the_cheapest_tier_that_clears_quality() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let authority = effective_authority(
            &repo,
            &machine,
            &change_contract(&["crates/amont/**"]),
            &identity(),
        );
        // Research clears the floor and is cheaper; implementation clears it
        // too but costs more. The change-task floor is implementation, so
        // research is NOT eligible despite the estimate.
        let predictor = FixedPredictor(vec![
            (Tier::Research, 0.9, 100),
            (Tier::Implementation, 0.9, 400),
            (Tier::Escalation, 0.95, 4000),
        ]);
        let d = expect_route(route(RouteInputs {
            contract: &change_contract(&["crates/amont/**"]),
            repo: &repo,
            machine: &machine,
            authority: &authority,
            predictor: Some(&predictor),
        }));
        assert_eq!(d.tier, Tier::Implementation);
        assert_eq!(d.routed_by, RoutedBy::LearnedArtifact);
    }

    #[test]
    fn learned_artifact_below_quality_floor_falls_back_to_baseline() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let authority = effective_authority(
            &repo,
            &machine,
            &change_contract(&["crates/amont/**"]),
            &identity(),
        );
        let predictor = FixedPredictor(vec![
            (Tier::Implementation, 0.40, 300),
            (Tier::Escalation, 0.40, 900),
        ]);
        let d = expect_route(route(RouteInputs {
            contract: &change_contract(&["crates/amont/**"]),
            repo: &repo,
            machine: &machine,
            authority: &authority,
            predictor: Some(&predictor),
        }));
        assert_eq!(d.tier, Tier::Implementation);
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
    }

    #[test]
    fn select_learned_skips_a_tier_with_no_cost_estimate() {
        let mut acceptance = BTreeMap::new();
        acceptance.insert(Tier::Research, 0.9);
        acceptance.insert(Tier::Implementation, 0.9);
        let mut cost = BTreeMap::new();
        // Research clears the floor and has no cost estimate; if `None`
        // costs sorted first it would win despite being unpriced.
        cost.insert(Tier::Implementation, MicroUsd::from_micros(400));
        let estimates = Estimates {
            artifact_id: "artifact-test-1".into(),
            input_hash: "in".into(),
            acceptance,
            cost,
            raw: serde_json::Value::Null,
        };
        let eligible = [Tier::Research, Tier::Implementation];
        let selected = select_learned(&estimates, &eligible, 0.5);
        assert_eq!(selected, Some(Tier::Implementation));
    }

    #[test]
    fn select_learned_abstains_when_no_eligible_tier_has_a_cost_estimate() {
        let mut acceptance = BTreeMap::new();
        acceptance.insert(Tier::Research, 0.9);
        acceptance.insert(Tier::Implementation, 0.9);
        let estimates = Estimates {
            artifact_id: "artifact-test-1".into(),
            input_hash: "in".into(),
            acceptance,
            cost: BTreeMap::new(),
            raw: serde_json::Value::Null,
        };
        let eligible = [Tier::Research, Tier::Implementation];
        let selected = select_learned(&estimates, &eligible, 0.5);
        assert_eq!(selected, None);
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
        let authority = effective_authority(
            &repo,
            &machine,
            &change_contract(&["crates/amont/**"]),
            &identity(),
        );
        let d = expect_route(route(RouteInputs {
            contract: &change_contract(&["crates/amont/**"]),
            repo: &repo,
            machine: &machine,
            authority: &authority,
            predictor: Some(&Abstainer),
        }));
        assert_eq!(d.tier, Tier::Implementation);
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
        let d = expect_blocked(decide(
            &change_contract(&["crates/amont/**"]),
            &repo,
            &machine,
        ));
        let text = d.explain();
        assert!(text.starts_with("route: blocked\n"), "{text}");
        assert!(
            text.contains("reason: blocked before dispatch by"),
            "a blocked route says why, not only which code: {text}"
        );
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
        assert_eq!(d.tier, Tier::Implementation);

        // Scope reaching outside the recipe does not count as coverage.
        let straddling = change_contract(&["docs/**", "crates/**"]);
        let d = route_with(&straddling, &repo, &machine);
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
    }

    #[test]
    fn a_whole_repository_scope_is_not_covered_by_a_docs_recipe() {
        let mut repo = repo_policy();
        repo.recipes.push(crate::policy::RecipeSpec {
            name: "docs-only".into(),
            kind: Some(Kind::Change),
            scope_within: vec!["docs/**".into()],
            // A tier that IS eligible for a change, so nothing but the
            // coverage test can keep the recipe from being chosen.
            tier: Tier::Escalation,
        });
        let machine = machine_for(&repo);
        let everything = change_contract(&["**"]);
        let d = route_with(&everything, &repo, &machine);
        assert_eq!(
            d.routed_by,
            RoutedBy::ConservativeBaseline,
            "a `**` contract is not covered by a docs recipe"
        );
        assert_eq!(
            d.tier,
            Tier::Implementation,
            "it takes the conservative floor for a change, not the recipe's tier"
        );
        // The same recipe still covers what it really covers.
        let docs = change_contract(&["docs/api/**"]);
        let d = route_with(&docs, &repo, &machine);
        assert_eq!(d.routed_by, RoutedBy::DeterministicRecipe);
    }

    proptest::proptest! {
        /// Selection is bounded by eligibility: whatever the estimates
        /// say, the router may only pick a tier the floor and the
        /// configured models left on the table — and only one that clears
        /// the quality floor and carries a price. The evaluator measures
        /// through this same function, so the bound holds for what a
        /// promotion claims as well as for what a run does.
        #[test]
        fn selection_never_leaves_eligibility(
            estimates in proptest::collection::vec(
                (0usize..3, 0.0f64..1.0, proptest::option::of(0i64..1_000_000)),
                0..6,
            ),
            eligible_mask in 0u8..8,
            quality_floor in 0.0f64..1.0,
        ) {
            use proptest::prelude::*;
            let ladder = [Tier::Research, Tier::Implementation, Tier::Escalation];
            let eligible: Vec<Tier> = ladder
                .iter()
                .enumerate()
                .filter(|(index, _)| eligible_mask & (1 << index) != 0)
                .map(|(_, tier)| *tier)
                .collect();
            let mut acceptance = BTreeMap::new();
            let mut cost = BTreeMap::new();
            for (index, probability, price) in estimates {
                acceptance.insert(ladder[index], probability);
                if let Some(price) = price {
                    cost.insert(ladder[index], MicroUsd::from_micros(price));
                }
            }
            let estimates = Estimates {
                artifact_id: "artifact-property".into(),
                input_hash: "in".into(),
                acceptance,
                cost,
                raw: serde_json::Value::Null,
            };
            let selected = select_learned(&estimates, &eligible, quality_floor);
            if let Some(tier) = selected {
                prop_assert!(eligible.contains(&tier), "{tier:?} is not eligible");
                prop_assert!(estimates.acceptance[&tier] >= quality_floor);
                prop_assert!(estimates.cost.contains_key(&tier), "an unpriced tier won");
                // And it is the cheapest such tier: nothing eligible that
                // clears the floor is priced below it.
                let chosen = estimates.cost[&tier];
                for other in &eligible {
                    let clears = estimates
                        .acceptance
                        .get(other)
                        .is_some_and(|value| *value >= quality_floor);
                    if clears {
                        if let Some(price) = estimates.cost.get(other) {
                            prop_assert!(*price >= chosen);
                        }
                    }
                }
            } else {
                // Abstention means nothing eligible was both good enough
                // and priced.
                for tier in &eligible {
                    let clears = estimates
                        .acceptance
                        .get(tier)
                        .is_some_and(|value| *value >= quality_floor);
                    prop_assert!(!(clears && estimates.cost.contains_key(tier)));
                }
            }
        }

        /// `eligible_tiers` never offers a tier below the floor its
        /// KIND implies, whatever the scope is and whatever risk rules
        /// fire: a risk rule may only raise the floor. An inspection
        /// that could be routed at research must never become a change,
        /// and a change must never fall back to research (SPEC §6).
        #[test]
        fn eligibility_never_falls_below_the_kind_floor(
            scope in proptest::collection::vec(
                proptest::sample::select(vec![
                    "src/**".to_string(),
                    "docs/**".to_string(),
                    "crates/relais/src/policy/**".to_string(),
                    "**".to_string(),
                    "Cargo.toml".to_string(),
                ]),
                1..4,
            ),
            inspect in proptest::bool::ANY,
            configured_mask in 0u8..8,
        ) {
            use proptest::prelude::*;
            let repo = repo_policy();
            let ladder = [Tier::Research, Tier::Implementation, Tier::Escalation];
            let configured: BTreeMap<Tier, ModelProfile> = ladder
                .iter()
                .enumerate()
                .filter(|(index, _)| configured_mask & (1 << index) != 0)
                .map(|(_, tier)| (*tier, repo.models[tier].clone()))
                .collect();
            let contract = if inspect {
                inspect_contract()
            } else {
                let borrowed: Vec<&str> = scope.iter().map(String::as_str).collect();
                change_contract(&borrowed)
            };
            let eligibility = eligible_tiers(&contract, &repo, &configured);
            let kind_floor = kind_floor(contract.kind());
            prop_assert!(
                eligibility.floor >= kind_floor,
                "a risk rule may only RAISE the floor: {:?} < {:?}",
                eligibility.floor,
                kind_floor
            );
            for tier in &eligibility.tiers {
                prop_assert!(*tier >= eligibility.floor, "{:?}", eligibility);
                prop_assert!(*tier >= kind_floor, "{:?}", eligibility);
                prop_assert!(configured.contains_key(tier), "{:?}", eligibility);
            }
            prop_assert!(
                eligibility.tiers.windows(2).all(|pair| pair[0] < pair[1]),
                "cheapest first, no repeats: {:?}",
                eligibility.tiers
            );
        }

        /// Eligibility itself is bounded: every tier it offers is
        /// configured and at or above the floor it reports.
        #[test]
        fn eligibility_offers_only_configured_tiers_above_its_floor(
            configured_mask in 0u8..8,
            floor_index in 0usize..3,
        ) {
            use proptest::prelude::*;
            let ladder = [Tier::Research, Tier::Implementation, Tier::Escalation];
            let configured: Vec<Tier> = ladder
                .iter()
                .enumerate()
                .filter(|(index, _)| configured_mask & (1 << index) != 0)
                .map(|(_, tier)| *tier)
                .collect();
            let floor = ladder[floor_index];
            let tiers = tiers_at_or_above(floor, &configured);
            for tier in &tiers {
                prop_assert!(*tier >= floor);
                prop_assert!(configured.contains(tier));
            }
            prop_assert!(tiers.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }
}
