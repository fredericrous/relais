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

use serde::{Deserialize, Serialize};

use crate::catalog::EffortCatalogs;
use crate::contract::scope::{scope_contained_in, write_scope_could_touch};
use crate::contract::{Kind, Review, Task, TaskContract};
use crate::money::MicroUsd;
use crate::policy::{
    BlockCode, Blocker, EffectiveAuthority, EffortId, MachineSettings, ModelProfile, RecipeSpec,
    RepoPolicy, RiskRule, Tier,
};

mod candidate;
mod rung;
pub mod trial;
pub use candidate::{
    default_tuning_bounds, validate_candidate, CandidateRecipe, CandidateRejection, TuningBounds,
};
pub use rung::{authority_ceiling, models_in_play, resolve_catalogs, EffortRequest, Rung};

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
    /// The resolved effort catalog of each model in play
    /// ([`models_in_play`]), computed by the caller from the harness probe
    /// and machine.toml so this function stays pure. A model with no
    /// catalog here has every fact unknown.
    pub catalogs: &'a EffortCatalogs,
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
    /// The first dispatch: `rung.tier` is `tier`. Repairs and escalations
    /// leave it (the ladder is P3); dispatch reads it for the initial tier.
    pub rung: Rung,
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
    /// Boxed: the estimates are the bulk of a route, and `Routed` holds a
    /// `Route` beside a much smaller `Blocked`.
    pub estimates: Option<Box<Estimates>>,
    /// The deterministic recipe that produced this route, when one fully
    /// covered the task; `None` when the learner or the conservative
    /// baseline decided instead. Named on the run's receipt and dispatch
    /// intent (SPEC §12, §17) via [`Recipe::covering_identity`].
    pub covering: Option<Recipe>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl RoutedBy {
    /// Every variant, for a caller that has to enumerate them — the
    /// round-trip test, and anything rendering a legend.
    ///
    /// The length is fixed, matching `Reason::ALL`, `State::ALL` and
    /// `UsagePhase::ALL`: a variant added to the enum and not added here
    /// fails to compile rather than quietly leaving the round-trip test
    /// walking a short list and reporting success over a variant it never
    /// saw. A `&[RoutedBy]` slice would have accepted the short list.
    pub const ALL: [Self; 4] = [
        RoutedBy::DeterministicRecipe,
        RoutedBy::RiskFloor,
        RoutedBy::LearnedArtifact,
        RoutedBy::ConservativeBaseline,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeterministicRecipe => "deterministic_recipe",
            Self::RiskFloor => "risk_floor",
            Self::LearnedArtifact => "learned_artifact",
            Self::ConservativeBaseline => "conservative_baseline",
        }
    }

    /// The inverse of [`RoutedBy::as_str`], for a value read back from the
    /// ledger. `None` is a name this relais does not know.
    pub fn parse(stored: &str) -> Option<Self> {
        Some(match stored {
            "deterministic_recipe" => Self::DeterministicRecipe,
            "risk_floor" => Self::RiskFloor,
            "learned_artifact" => Self::LearnedArtifact,
            "conservative_baseline" => Self::ConservativeBaseline,
            _ => return None,
        })
    }
}

impl std::fmt::Display for RoutedBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// The risk floor from declared scope and repository rules: the highest
/// minimum tier among rules whose paths the declared scope could touch.
///
/// A rule with no paths touches nothing. It used to fire on any contract
/// whose scope was `**`, through a `paths.first().unwrap_or(&String::new())`
/// disjunct that asked whether the scope could touch the empty pattern —
/// a rule nobody wrote, applied to the one scope that matches everything.
/// Policy validation rejects a pathless rule outright.
///
/// The effort floor rides alongside the tier. Which effort is above which
/// is the dispatched model's catalog order, unknown here, so every touched
/// rule's `minimum_effort` is returned and the rung takes the highest in
/// that order.
fn risk_floor(contract: &TaskContract, risk: &[RiskRule]) -> RiskFloor {
    let mut floor = RiskFloor::default();
    for (index, rule) in risk.iter().enumerate() {
        let touches = rule
            .paths
            .iter()
            .any(|pattern| write_scope_could_touch(contract, pattern));
        if touches {
            let effort_clause = rule
                .minimum_effort
                .as_ref()
                .map(|effort| format!(" and at least effort {effort}"))
                .unwrap_or_default();
            floor.reasons.push(RouteReason::new(
                format!("risk[{}]:{}", index, rule.minimum_tier.as_str()),
                format!(
                    "risk rule {index} ({}) requires at least the {} tier{effort_clause}",
                    rule.paths.join(", "),
                    rule.minimum_tier.as_str()
                ),
            ));
            floor.tier = Some(match floor.tier {
                Some(current) if current >= rule.minimum_tier => current,
                Some(_) | None => rule.minimum_tier,
            });
            floor.efforts.extend(rule.minimum_effort.clone());
        }
    }
    floor
}

/// What the risk rules a scope could touch demand.
#[derive(Debug, Default)]
struct RiskFloor {
    tier: Option<Tier>,
    /// Each touched rule's `minimum_effort`, in rule order.
    efforts: Vec<EffortId>,
    reasons: Vec<RouteReason>,
}

/// An explicitly configured deterministic recipe, used only when it FULLY
/// covers the task (SPEC §6, step 3). Recipes are never inferred from
/// prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// [`crate::policy::RecipeSpec::recipe_id`]: the recipe's
    /// content-derived identity, named on a run's receipt and dispatch
    /// intent alongside `name` and `revision` (SPEC §12, §17).
    pub recipe_id: String,
    pub name: String,
    pub kind: Option<Kind>,
    /// Contract scope must fall entirely within these patterns.
    pub scope_within: Vec<String>,
    pub tier: Tier,
    /// Which version of this recipe this is; when several recipes cover
    /// a task, the highest revision among the enabled ones wins.
    pub revision: u32,
    /// Whether this revision is eligible for selection at all. A
    /// disabled recipe is never selected, even when it is the highest
    /// revision among those covering the task.
    pub enabled: bool,
}

impl From<&crate::policy::RecipeSpec> for Recipe {
    fn from(spec: &crate::policy::RecipeSpec) -> Self {
        Recipe {
            recipe_id: spec.recipe_id(),
            name: spec.name.clone(),
            kind: spec.kind,
            scope_within: spec.scope_within.clone(),
            tier: spec.tier,
            revision: spec.revision,
            enabled: spec.enabled,
        }
    }
}

impl Recipe {
    /// This recipe's identity as recorded on a receipt or dispatch
    /// intent: id, name and revision, and nothing else — `kind`,
    /// `scope_within`, `tier` and `enabled` decided whether it covered
    /// the task, not what a reader needs to know afterward.
    pub fn covering_identity(&self) -> crate::policy::CoveringRecipe {
        crate::policy::CoveringRecipe {
            id: self.recipe_id.clone(),
            name: self.name.clone(),
            revision: self.revision,
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

/// The tier floor a RECIPE's own declared kind and scope demand, using the
/// exact [`kind_floor`] and [`risk_floor`] a real task's floor is computed
/// with — never a second copy of either rule. `kind: None` matches both
/// kinds (SPEC §6, [`recipe_covers`]), so it is judged as `Kind::Change`,
/// the stricter of the two floors, rather than letting an unconstrained
/// recipe hide behind the lower inspect floor. An empty `scope_within`
/// covers any path (same as `recipe_covers`), so it is judged against `**`
/// — every risk rule it could touch, not none of them. A scope this relais
/// cannot even compile is judged at the top of the ladder: an unreadable
/// declaration is not evidence of safety.
fn recipe_tier_floor(kind: Option<Kind>, scope_within: &[String], risk: &[RiskRule]) -> Tier {
    let effective_kind = kind.unwrap_or(Kind::Change);
    let floor = kind_floor(effective_kind);
    let patterns = if scope_within.is_empty() {
        vec!["**".to_string()]
    } else {
        scope_within.to_vec()
    };
    // `risk_floor` runs for EVERY kind, as `eligible_tiers` does. An
    // earlier version returned `kind_floor` immediately for
    // `Kind::Inspect`, which gives the same answer today — `Task::Inspect`
    // carries no write scope, so no risk rule can match it — but only
    // because of that. It baked the assumption in, and a change to
    // inspect contracts or to how rules match scope would have made the
    // validator's floor and routing's floor disagree silently. Letting
    // the same function decide costs nothing and cannot drift.
    let task = match effective_kind {
        Kind::Inspect => Task::Inspect,
        Kind::Change => match Task::change(patterns) {
            Ok(task) => task,
            Err(_) => return Tier::Escalation,
        },
    };
    match risk_floor(&floor_contract(task), risk).tier {
        Some(rule_floor) if rule_floor > floor => rule_floor,
        _ => floor,
    }
}

/// The `minimum_effort` of every risk rule a RECIPE's own kind and scope
/// could touch, judged exactly as [`recipe_tier_floor`] judges the tier
/// floor: the same scope reading, the same [`risk_floor`]. The caller ranks
/// them in the model's catalog order.
fn recipe_effort_floors(
    kind: Option<Kind>,
    scope_within: &[String],
    risk: &[RiskRule],
) -> Vec<EffortId> {
    let patterns = if scope_within.is_empty() {
        vec!["**".to_string()]
    } else {
        scope_within.to_vec()
    };
    let task = match kind.unwrap_or(Kind::Change) {
        Kind::Inspect => Task::Inspect,
        Kind::Change => match Task::change(patterns) {
            Ok(task) => task,
            // An unreadable scope is judged against every rule, as the
            // tier floor judges it at the top of the ladder.
            Err(_) => {
                return risk
                    .iter()
                    .filter_map(|rule| rule.minimum_effort.clone())
                    .collect()
            }
        },
    };
    risk_floor(&floor_contract(task), risk).efforts
}

fn floor_contract(task: Task) -> TaskContract {
    TaskContract {
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
    let RiskFloor {
        tier: rule_floor,
        efforts: _ranked_by_the_rung,
        reasons: fired_rules,
    } = risk_floor(contract, &repo.risk);
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

/// The `RecipeSpec` that would win among the eligible tiers for this
/// contract — highest enabled revision, config order breaking ties — the
/// same rule `route` uses to decide whether a recipe wins over the
/// learner (SPEC §6, step 3). The one place that rule is stated; `route`
/// and [`covering_recipe_id`] both call it rather than restating it.
fn covering_recipe_spec<'a>(
    contract: &TaskContract,
    repo: &'a RepoPolicy,
    eligible: &[Tier],
) -> Option<&'a RecipeSpec> {
    crate::policy::select_highest_enabled_revision(&repo.recipes, |spec| {
        let recipe = Recipe::from(spec);
        eligible.contains(&recipe.tier) && recipe_covers(contract, &recipe)
    })
}

/// The [`RecipeSpec::recipe_id`] of the recipe that produced this task's
/// route, or `None` when no configured recipe covers it. Pure: no clock,
/// no environment, no filesystem — only the recipes policy declares and
/// the contract's own kind and scope, read in the fixed order `repo.recipes`
/// stores them. Used to key learned evidence on the recipe that actually
/// produced a run (SPEC §17), not only the model/effort/harness beneath it:
/// two revisions of one recipe can behave differently on the same profile.
pub fn covering_recipe_id(
    contract: &TaskContract,
    repo: &RepoPolicy,
    eligible: &[Tier],
) -> Option<String> {
    covering_recipe_spec(contract, repo, eligible).map(RecipeSpec::recipe_id)
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
        catalogs,
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
    // (SPEC §6, step 3). The rule for WHICH one — highest enabled
    // revision, config order breaking ties — is
    // `policy::recipe::select_highest_enabled_revision` and is called,
    // never restated here. Route held its own `filter`+`fold` saying the
    // same thing; the two agreed, including the tie-break, and nothing
    // kept them agreeing, which is the shape of defect this codebase has
    // paid for repeatedly. This is the only caller, so the rule's own
    // unit tests now cover production rather than a parallel copy.
    let mut estimates_seen: Option<Box<Estimates>> = None;
    let covering_spec = covering_recipe_spec(contract, repo, &eligible);
    let covering = covering_spec.map(Recipe::from);
    let selected: (Tier, RoutedBy) = if let Some(recipe) = &covering {
        reasons.push(RouteReason::new(
            "deterministic_recipe",
            format!(
                "deterministic recipe `{}` (revision {}) fully covers the task",
                recipe.name, recipe.revision
            ),
        ));
        (recipe.tier, RoutedBy::DeterministicRecipe)
    } else if let (true, Some(predictor)) = (machine.routing.learned_enabled, predictor) {
        match predictor.estimate(contract, authority, &eligible) {
            Some(estimates) => {
                let quality_floor = machine.routing.quality_floor.unwrap_or(0.75);
                let selection = select_learned(&estimates, &eligible, quality_floor);
                estimates_seen = Some(Box::new(estimates.clone()));
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

    // A covering recipe chose the tier, so its `models[tier]` is the
    // profile; with none, the authority policy's is.
    let floors = risk_floor(contract, &repo.risk).efforts;
    let resolved = match rung::resolve_rung(rung::RungRequest {
        tier: selected.0,
        authority,
        machine,
        recipe: covering_spec,
        floors: &floors,
        catalogs,
    }) {
        Ok(resolved) => resolved,
        Err(blocker) => {
            reasons.push(RouteReason::new(
                "rung_blocked",
                format!("the initial rung cannot be dispatched: {}", blocker.detail),
            ));
            return Routed::Blocked(Blocked::new(blocker, Vec::new(), review, reasons));
        }
    };
    reasons.extend(resolved.reasons);

    // An escalation is reachable when the attempts outlast the repairs: the
    // initial attempt, every repair, then the escalated one.
    let reachable_escalation = escalation_tier.filter(|_| {
        authority.max_attempts > authority.max_repairs_before_escalation.saturating_add(1)
    });
    // holds-until: the P3 ladder resolves and validates every reachable rung.
    // Until then candidate admission (`validate_candidate`) does not mirror
    // this check: a candidate that moves its covering tier below the
    // escalation tier under a floor can be admitted and still blocked here.
    // P3 shares one ladder validation between routing and admission.
    if let Some(escalation) = reachable_escalation {
        if let Err(blocker) = rung::check_escalation_floor(escalation, authority, catalogs, &floors)
        {
            reasons.push(RouteReason::new(
                "escalation_blocked",
                format!(
                    "the escalation rung cannot honour the risk floor: {}",
                    blocker.detail
                ),
            ));
            return Routed::Blocked(Blocked::new(blocker, Vec::new(), review, reasons));
        }
    }

    Routed::Route(Route {
        tier: selected.0,
        rung: resolved.rung,
        escalation_tier,
        review,
        max_attempts: authority.max_attempts,
        max_repairs_before_escalation: authority.max_repairs_before_escalation,
        routed_by: selected.1,
        estimates: estimates_seen,
        covering,
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
    pub fn explain(&self) -> String {
        match self {
            Self::Route(route) => route.explain(),
            Self::Blocked(blocked) => blocked.explain(),
        }
    }
}

impl Route {
    /// The profile a worker at `tier` is dispatched with: this route's
    /// rung for its own tier, and the authority policy's profile for any
    /// other (a repair keeps the rung's tier; an escalation leaves it, and
    /// the ladder is P3). `None` when neither names a model.
    pub fn dispatch_profile(
        &self,
        tier: Tier,
        authority: &EffectiveAuthority,
    ) -> Option<ModelProfile> {
        if tier == self.rung.tier {
            Some(self.rung.profile())
        } else {
            authority.models.get(&tier).cloned()
        }
    }

    /// `effort: <model>@<effort>` (or the two states without one), with
    /// who set the effort when a recipe or a risk floor did. Within 80
    /// columns: a longer line puts the reason on a line of its own.
    fn effort_line(&self) -> String {
        let model = &self.rung.model;
        let head = match &self.rung.effort {
            EffortRequest::Explicit(effort) => format!("effort: {model}@{effort}"),
            EffortRequest::ControlUnsupported => format!("effort: {model}, no effort control"),
            EffortRequest::NotRequested => format!("effort: {model}, none requested"),
        };
        let by_recipe = self.reasons.iter().any(|r| r.id == rung::RECIPE_RUNG);
        let by_floor = self.reasons.iter().any(|r| r.id == rung::EFFORT_FLOOR);
        let why = match (by_recipe, by_floor) {
            (true, true) => "set by recipe, raised by risk floor",
            (true, false) => "set by recipe",
            (false, true) => "raised by risk floor",
            (false, false) => return format!("{head}\n"),
        };
        if head.chars().count() + why.len() + 3 <= 80 {
            format!("{head} ({why})\n")
        } else {
            format!("{head}\n  ({why})\n")
        }
    }

    /// The route's terms, in the shape SPEC §6 gives as an example. The
    /// model is the RUNG's — the one dispatched, which a covering recipe may
    /// have swapped for the policy's — so the `route:` and `effort:` lines
    /// cannot name different models.
    pub fn explain(&self) -> String {
        let mut out = format!("route: {} / {}\n", self.tier.as_str(), self.rung.model);
        out.push_str(&self.effort_line());
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

    #[test]
    fn routed_by_round_trips_through_its_string_form() {
        for variant in RoutedBy::ALL {
            let stored = variant.as_str();
            assert_eq!(
                RoutedBy::parse(stored),
                Some(variant),
                "{variant:?} must parse back from its own as_str()"
            );
            let json = serde_json::to_string(&variant).expect("serializes");
            assert_eq!(
                serde_json::from_str::<RoutedBy>(&json).expect("deserializes"),
                variant,
                "{variant:?} must round-trip through serde"
            );
        }
    }

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
                        max_effort: None,
                    },
                ),
                (
                    Tier::Implementation,
                    ModelProfile {
                        id: "sonnet".into(),
                        effort: crate::policy::EffortId::parse("medium").ok(),
                        max_effort: None,
                    },
                ),
                (
                    Tier::Escalation,
                    ModelProfile {
                        id: "fable".into(),
                        effort: crate::policy::EffortId::parse("medium").ok(),
                        max_effort: None,
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
                            name: None,
                            argv: vec!["make".into(), "check".into()],
                            timeout_seconds: 300,
                            junit: None,
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
            admission: Default::default(),
            pricing: None,
            efforts: Default::default(),
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
        decide_with(contract, repo, machine, &EffortCatalogs::default())
    }

    fn decide_with(
        contract: &TaskContract,
        repo: &RepoPolicy,
        machine: &MachineSettings,
        catalogs: &EffortCatalogs,
    ) -> Routed {
        let authority = effective_authority(repo, machine, contract, &identity());
        route(RouteInputs {
            contract,
            repo,
            machine,
            authority: &authority,
            predictor: None,
            catalogs,
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

    /// A tier configured with an effort id no code names still routes, and
    /// the routed tier's profile carries it: effort is data to the router.
    #[test]
    fn a_tier_with_an_unfamiliar_effort_routes_and_carries_it() {
        use crate::catalog::{self, Admissible, Fact};
        use crate::policy::{EffortId, EffortModelEntry, EffortSettings};
        let ultra = EffortId::parse("ultra").expect("valid");
        let mut repo = repo_policy();
        repo.models
            .get_mut(&Tier::Implementation)
            .expect("the implementation tier")
            .effort = Some(ultra.clone());
        let mut machine = machine_for(&repo);
        machine.efforts = EffortSettings {
            order: Some(vec![EffortId::parse("low").expect("valid"), ultra.clone()]),
            models: vec![EffortModelEntry {
                ids: vec!["sonnet".into()],
                supported: Some(vec![ultra.clone()]),
                order: None,
            }],
        };
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        assert_eq!(d.tier, Tier::Implementation);
        // P1 routing does not choose effort, so the route decision carries
        // none to assert on; that `ultra` is actually DISPATCHED is proved
        // by the runner test that records every launch's `spec.effort`.
        let cli = Fact::Known(vec![ultra.clone()]);
        let catalog = catalog::resolve(&cli, &machine.efforts, &ultra, "sonnet");
        assert_eq!(catalog.admissible(), Admissible::Set(vec![ultra]));
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
            minimum_effort: None,
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
            minimum_effort: None,
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
            minimum_effort: None,
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
            minimum_effort: None,
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
            minimum_effort: None,
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
            minimum_effort: None,
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
            admission: Default::default(),
            pricing: None,
            efforts: Default::default(),
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
            catalogs: &EffortCatalogs::default(),
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
            catalogs: &EffortCatalogs::default(),
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
            catalogs: &EffortCatalogs::default(),
        }));
        assert_eq!(d.tier, Tier::Implementation);
        assert_eq!(d.routed_by, RoutedBy::ConservativeBaseline);
    }

    #[test]
    fn explanation_matches_the_spec_shape() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let d = route_with(&change_contract(&["crates/amont/**"]), &repo, &machine);
        let text = d.explain();
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
            admission: Default::default(),
            pricing: None,
            efforts: Default::default(),
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
            revision: 0,
            enabled: true,
            models: None,
            execution: None,
            context: None,
            review: None,
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
            revision: 0,
            enabled: true,
            models: None,
            execution: None,
            context: None,
            review: None,
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

    fn docs_recipe(revision: u32, enabled: bool) -> crate::policy::RecipeSpec {
        crate::policy::RecipeSpec {
            kind: Some(Kind::Change),
            scope_within: vec!["docs/**".into()],
            revision,
            enabled,
            ..crate::policy::RecipeSpec::covering("docs-touchup", Tier::Implementation)
        }
    }

    #[test]
    fn selection_picks_the_highest_enabled_revision_among_covering_recipes() {
        let mut repo = repo_policy();
        repo.recipes.push(docs_recipe(0, true));
        repo.recipes.push(docs_recipe(2, true));
        repo.recipes.push(docs_recipe(1, true));
        let machine = machine_for(&repo);
        let docs = change_contract(&["docs/guide.md"]);
        let d = route_with(&docs, &repo, &machine);
        assert_eq!(d.routed_by, RoutedBy::DeterministicRecipe);
        assert!(
            d.reasons
                .iter()
                .any(|r| r.id == "deterministic_recipe" && r.text.contains("revision 2")),
            "the highest enabled revision (2) is the one selected: {:?}",
            d.reasons
        );
    }

    #[test]
    fn a_disabled_recipe_is_never_selected_even_at_the_highest_revision() {
        let mut repo = repo_policy();
        repo.recipes.push(docs_recipe(0, true));
        repo.recipes.push(docs_recipe(2, false));
        let machine = machine_for(&repo);
        let docs = change_contract(&["docs/guide.md"]);
        let d = route_with(&docs, &repo, &machine);
        assert_eq!(d.routed_by, RoutedBy::DeterministicRecipe);
        assert!(
            d.reasons
                .iter()
                .any(|r| r.id == "deterministic_recipe" && r.text.contains("revision 0")),
            "the disabled revision 2 is never selected, whatever its revision: {:?}",
            d.reasons
        );
        let mut repo_all_disabled = repo_policy();
        repo_all_disabled.recipes.push(docs_recipe(2, false));
        let machine = machine_for(&repo_all_disabled);
        let d = route_with(&docs, &repo_all_disabled, &machine);
        assert_eq!(
            d.routed_by,
            RoutedBy::ConservativeBaseline,
            "the only recipe is disabled, so nothing routes by recipe"
        );
    }

    /// The recipe identity a run gets keyed on: a pure function of the
    /// contract and policy, agreeing with `route`'s own choice of recipe —
    /// two revisions of one recipe are two different ids, and a
    /// byte-identical recipe (whatever it is named) is the same id.
    #[test]
    fn covering_recipe_id_differs_by_revision_and_matches_when_byte_identical() {
        let docs = change_contract(&["docs/guide.md"]);
        let eligible = [Tier::Implementation];

        let mut repo_rev0 = repo_policy();
        repo_rev0.recipes.push(docs_recipe(0, true));
        let id_rev0 = covering_recipe_id(&docs, &repo_rev0, &eligible);
        assert!(id_rev0.is_some());

        let mut repo_rev1 = repo_policy();
        repo_rev1.recipes.push(docs_recipe(1, true));
        let id_rev1 = covering_recipe_id(&docs, &repo_rev1, &eligible);
        assert_ne!(
            id_rev0, id_rev1,
            "two runs differing only in the recipe revision that produced them are different \
             identities"
        );

        let mut repo_rev0_again = repo_policy();
        repo_rev0_again.recipes.push(docs_recipe(0, true));
        let id_rev0_again = covering_recipe_id(&docs, &repo_rev0_again, &eligible);
        assert_eq!(
            id_rev0, id_rev0_again,
            "two runs under a byte-identical recipe are the same identity"
        );

        let no_recipe = repo_policy();
        assert_eq!(
            covering_recipe_id(&docs, &no_recipe, &eligible),
            None,
            "no configured recipe covers this task"
        );
    }

    fn eid(text: &str) -> EffortId {
        EffortId::parse(text).expect("a valid effort identifier")
    }

    /// The catalog of `model`: the harness accepts `cli`, the order is
    /// `order`, the model supports `supported` (`&[]` is no effort
    /// control), authorized up to `max`.
    fn catalog_of(
        model: &str,
        cli: &[&str],
        order: &[&str],
        supported: &[&str],
        max: &str,
    ) -> EffortCatalogs {
        use crate::catalog::Fact;
        use crate::policy::{EffortModelEntry, EffortSettings};
        let ids = |list: &[&str]| -> Vec<EffortId> { list.iter().copied().map(eid).collect() };
        let settings = EffortSettings {
            order: Some(ids(order)),
            models: vec![EffortModelEntry {
                ids: vec![model.into()],
                supported: Some(ids(supported)),
                order: None,
            }],
        };
        EffortCatalogs::resolve_all(&Fact::Known(ids(cli)), &settings, &eid(max), [model])
    }

    const LEVELS: [&str; 4] = ["low", "medium", "high", "max"];

    fn sonnet_catalog() -> EffortCatalogs {
        catalog_of("sonnet", &LEVELS, &LEVELS, &LEVELS, "max")
    }

    /// A recipe covering every task that sets the implementation tier to
    /// `model` at `effort`.
    fn recipe_setting(model: &str, effort: Option<&str>) -> RecipeSpec {
        RecipeSpec {
            models: Some(BTreeMap::from([(
                Tier::Implementation,
                ModelProfile {
                    id: model.into(),
                    effort: effort.map(eid),
                    max_effort: None,
                },
            )])),
            ..RecipeSpec::covering("fast", Tier::Implementation)
        }
    }

    fn floor_rule(effort: &str) -> RiskRule {
        RiskRule {
            paths: vec!["crates/amont/**".into()],
            minimum_tier: Tier::Implementation,
            review: None,
            minimum_effort: Some(eid(effort)),
        }
    }

    fn blocker_of(decision: Routed) -> Blocker {
        expect_blocked(decision).first().clone()
    }

    fn decide_amont(
        repo: &RepoPolicy,
        machine: &MachineSettings,
        catalogs: &EffortCatalogs,
    ) -> Routed {
        decide_with(
            &change_contract(&["crates/amont/**"]),
            repo,
            machine,
            catalogs,
        )
    }

    /// (a) A covering recipe's `models.implementation` is READ: the rung
    /// is its model at its effort, the catalog admitting it, and the
    /// rung's tier is the route's.
    #[test]
    fn a_covering_recipe_sets_the_rung_model_and_effort() {
        let mut repo = repo_policy();
        repo.recipes.push(recipe_setting("sonnet", Some("high")));
        let machine = machine_for(&repo);
        let d = expect_route(decide_amont(&repo, &machine, &sonnet_catalog()));
        assert_eq!(d.routed_by, RoutedBy::DeterministicRecipe);
        assert_eq!(
            d.rung,
            Rung {
                tier: Tier::Implementation,
                model: "sonnet".into(),
                effort: EffortRequest::Explicit(eid("high")),
            }
        );
        assert_eq!(d.rung.tier, d.tier);
        assert!(d
            .reasons
            .iter()
            .any(|reason| reason.id == rung::RECIPE_RUNG));
    }

    /// (b) A recipe naming a model the machine does not allow is blocked
    /// `model_not_allowed` — never routed to another model.
    #[test]
    fn a_recipe_model_outside_allowed_models_blocks_the_route() {
        let mut repo = repo_policy();
        repo.recipes.push(recipe_setting("opus", None));
        let mut machine = machine_for(&repo);
        machine.allowed_models = Some(vec!["haiku".into(), "sonnet".into(), "fable".into()]);
        let blocker = blocker_of(decide_amont(&repo, &machine, &EffortCatalogs::default()));
        assert_eq!(blocker.code, BlockCode::ModelNotAllowed);
        assert!(blocker.detail.contains("opus"), "{}", blocker.detail);
    }

    /// (c) A floor `high` on a model with `supported = []` blocks, naming
    /// the floor and the model.
    ///
    /// FALSIFIED: with the empty-set guards in both `apply_floor` and
    /// `check_effort` opened (a `ControlUnsupported` model passing the
    /// floor), this test failed at `expect_blocked` — the route came back
    /// as a route; the guards were restored. Opening only `apply_floor`'s
    /// still blocked, through `check_effort`'s guard, which is why the
    /// assertion names the floor's own wording (`risk floor `high``).
    #[test]
    fn a_floor_on_a_model_with_no_effort_control_blocks_naming_both() {
        let mut repo = repo_policy();
        repo.risk.push(floor_rule("high"));
        let machine = machine_for(&repo);
        let none = catalog_of("sonnet", &LEVELS, &LEVELS, &[], "max");
        let blocker = blocker_of(decide_amont(&repo, &machine, &none));
        assert_eq!(blocker.code, BlockCode::EffortUnsupported);
        assert!(
            blocker.detail.contains("risk floor `high`") && blocker.detail.contains("sonnet"),
            "the floor and the model are named: {}",
            blocker.detail
        );
        assert!(blocker.detail.contains("admissible"), "{}", blocker.detail);
    }

    /// (d) A floor above the configured effort raises the rung to it, and
    /// says so.
    #[test]
    fn a_floor_raises_the_configured_effort_in_catalog_order() {
        let mut repo = repo_policy();
        repo.risk.push(floor_rule("high"));
        // The initial rung alone: no escalation is reachable.
        repo.execution.max_attempts = 1;
        let machine = machine_for(&repo);
        let d = expect_route(decide_amont(&repo, &machine, &sonnet_catalog()));
        assert_eq!(d.rung.effort, EffortRequest::Explicit(eid("high")));
        assert!(d
            .reasons
            .iter()
            .any(|reason| reason.id == rung::EFFORT_FLOOR));
        assert!(d.explain().contains("raised by risk floor"));
    }

    /// Every model of `models` at all four levels, the harness accepting
    /// them all, authorized up to `max`.
    fn levels_catalog(models: &[&str]) -> EffortCatalogs {
        let supporting: Vec<(&str, &[&str])> =
            models.iter().map(|model| (*model, &LEVELS[..])).collect();
        catalogs_supporting(&supporting)
    }

    /// [`levels_catalog`] where each model supports its own set (`&[]` is
    /// no effort control); a model not listed has no catalog at all.
    fn catalogs_supporting(models: &[(&str, &[&str])]) -> EffortCatalogs {
        use crate::catalog::Fact;
        use crate::policy::{EffortModelEntry, EffortSettings};
        let ids = |list: &[&str]| -> Vec<EffortId> { list.iter().copied().map(eid).collect() };
        let settings = EffortSettings {
            order: Some(ids(&LEVELS)),
            models: models
                .iter()
                .map(|(model, supported)| EffortModelEntry {
                    ids: vec![(*model).into()],
                    supported: Some(ids(supported)),
                    order: None,
                })
                .collect(),
        };
        EffortCatalogs::resolve_all(
            &Fact::Known(ids(&LEVELS)),
            &settings,
            &eid("max"),
            models.iter().map(|(model, _)| *model),
        )
    }

    /// A repo with a `high` floor over the amont scope, the escalation tier
    /// configured at `escalation_effort` and `max_attempts` attempts.
    fn floored_repo(escalation_effort: &str, max_attempts: u32) -> RepoPolicy {
        let mut repo = repo_policy();
        repo.risk.push(floor_rule("high"));
        repo.execution.max_attempts = max_attempts;
        repo.models
            .get_mut(&Tier::Escalation)
            .expect("the escalation tier")
            .effort = Some(eid(escalation_effort));
        repo
    }

    /// A floor binds the initial rung only; an escalation dispatches its
    /// tier's configured profile. While that is so, a route whose reachable
    /// escalation would run below the floor is blocked, naming the floor,
    /// the escalation tier and its configured effort.
    ///
    /// FALSIFIED: with the `check_escalation_floor` call removed from
    /// `route()`, the first assertion failed at `expect_blocked` — the
    /// route came back as a route that escalates to `fable@medium` under a
    /// `high` floor; the call was restored.
    #[test]
    fn a_reachable_escalation_below_the_floor_blocks_the_route() {
        let repo = floored_repo("medium", 3);
        let machine = machine_for(&repo);
        let catalogs = levels_catalog(&["haiku", "sonnet", "fable"]);
        let blocker = blocker_of(decide_amont(&repo, &machine, &catalogs));
        assert_eq!(blocker.code, BlockCode::EffortUnsupported);
        for named in ["high", "escalation", "fable", "medium"] {
            assert!(
                blocker.detail.contains(named),
                "`{named}` is named: {}",
                blocker.detail
            );
        }
    }

    /// The same policy, where the attempt budget cannot reach the
    /// escalation, routes: the initial rung is raised to the floor.
    #[test]
    fn an_escalation_the_budget_cannot_reach_does_not_block() {
        let repo = floored_repo("medium", 1);
        let machine = machine_for(&repo);
        let catalogs = levels_catalog(&["haiku", "sonnet", "fable"]);
        let d = expect_route(decide_amont(&repo, &machine, &catalogs));
        assert_eq!(d.rung.effort, EffortRequest::Explicit(eid("high")));
        assert_eq!(d.escalation_tier, Some(Tier::Escalation));
    }

    /// Repairs come first: with one repair before escalation, two attempts
    /// end on the repair and the escalation needs a third.
    #[test]
    fn an_escalation_behind_the_repairs_is_reachable_only_with_room_for_it() {
        let catalogs = levels_catalog(&["haiku", "sonnet", "fable"]);
        let repo = floored_repo("medium", 2);
        let machine = machine_for(&repo);
        expect_route(decide_amont(&repo, &machine, &catalogs));
        let repo = floored_repo("medium", 3);
        let machine = machine_for(&repo);
        expect_blocked(decide_amont(&repo, &machine, &catalogs));
    }

    /// An escalation configured at the floor, in a catalog that knows it,
    /// is fine — and one whose catalog is unknown cannot carry a floor.
    #[test]
    fn an_escalation_at_the_floor_routes_and_an_unknown_one_blocks() {
        let repo = floored_repo("high", 3);
        let machine = machine_for(&repo);
        let d = expect_route(decide_amont(
            &repo,
            &machine,
            &levels_catalog(&["haiku", "sonnet", "fable"]),
        ));
        assert_eq!(d.escalation_tier, Some(Tier::Escalation));

        let blocker = blocker_of(decide_amont(&repo, &machine, &sonnet_catalog()));
        assert_eq!(blocker.code, BlockCode::EffortUnsupported);
        assert!(
            blocker.detail.contains("fable") && blocker.detail.contains("escalation"),
            "{}",
            blocker.detail
        );
    }

    /// `route:` names the model the rung dispatches, as `effort:` does, so
    /// a recipe swapping the model changes both; with no recipe setting a
    /// model it is the policy's, as it always was.
    #[test]
    fn the_route_line_names_the_rungs_model() {
        let mut swapped = repo_policy();
        swapped.recipes.push(recipe_setting("haiku", Some("low")));
        let machine = machine_for(&swapped);
        let catalogs = levels_catalog(&["haiku", "sonnet", "fable"]);
        let text = expect_route(decide_amont(&swapped, &machine, &catalogs)).explain();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("route: implementation / haiku"));
        assert!(
            lines
                .next()
                .is_some_and(|line| line.starts_with("effort: haiku@low")),
            "{text}"
        );

        let plain = repo_policy();
        let machine = machine_for(&plain);
        let text = expect_route(decide_amont(&plain, &machine, &catalogs)).explain();
        assert!(
            text.starts_with("route: implementation / sonnet\neffort: sonnet@medium\n"),
            "{text}"
        );
    }

    /// A floor below the configured effort leaves it alone: max(start,
    /// floor) in catalog order, not the floor.
    #[test]
    fn a_floor_below_the_start_effort_keeps_the_start() {
        let mut repo = repo_policy();
        repo.risk.push(floor_rule("low"));
        // The initial rung alone: no escalation is reachable.
        repo.execution.max_attempts = 1;
        let machine = machine_for(&repo);
        let d = expect_route(decide_amont(&repo, &machine, &sonnet_catalog()));
        assert_eq!(d.rung.effort, EffortRequest::Explicit(eid("medium")));
    }

    /// The highest of several touched rules' floors wins, in the catalog's
    /// order — `max` is not "above" `high` by its name.
    #[test]
    fn the_highest_floor_is_ranked_by_the_catalog_not_the_name() {
        let mut repo = repo_policy();
        repo.risk.push(floor_rule("high"));
        repo.risk.push(floor_rule("max"));
        // The initial rung alone: no escalation is reachable.
        repo.execution.max_attempts = 1;
        let machine = machine_for(&repo);
        let order = ["low", "medium", "max", "high"];
        let catalogs = catalog_of("sonnet", &order, &order, &order, "high");
        let d = expect_route(decide_amont(&repo, &machine, &catalogs));
        assert_eq!(
            d.rung.effort,
            EffortRequest::Explicit(eid("high")),
            "in this catalog `high` is the top"
        );
    }

    /// (e) An unknown catalog lets exactly the configured effort through,
    /// as before.
    ///
    /// FALSIFIED (the second half, in the test below): with `check_effort`
    /// allowing any effort under an unknown catalog, the changed-effort
    /// test failed at `expect_blocked` (this test and the floor test kept
    /// passing); the guard was restored.
    #[test]
    fn an_unknown_catalog_allows_the_configured_effort() {
        let repo = repo_policy();
        let machine = machine_for(&repo);
        let d = expect_route(decide_amont(&repo, &machine, &EffortCatalogs::default()));
        assert_eq!(d.rung.effort, EffortRequest::Explicit(eid("medium")));
    }

    /// (e) Under an unknown catalog an effort a recipe CHANGED needs a
    /// known catalog: blocked, naming the machine.toml keys.
    #[test]
    fn an_unknown_catalog_blocks_an_effort_a_recipe_changed() {
        let mut repo = repo_policy();
        repo.recipes.push(recipe_setting("sonnet", Some("high")));
        let machine = machine_for(&repo);
        let blocker = blocker_of(decide_amont(&repo, &machine, &EffortCatalogs::default()));
        assert_eq!(blocker.code, BlockCode::EffortUnsupported);
        assert!(
            blocker.detail.contains("[efforts]") && blocker.detail.contains("machine.toml"),
            "the keys to set are named: {}",
            blocker.detail
        );
    }

    /// Under an unknown catalog `validate_candidate` admits exactly the
    /// recipes whose rung `route()` resolves: the carve-out is the
    /// configured model at its configured effort, in both.
    #[test]
    fn candidate_validation_admits_what_an_unknown_catalog_route_resolves() {
        let mut incumbent = repo_policy();
        incumbent
            .recipes
            .push(recipe_setting("sonnet", Some("medium")));
        let none = EffortCatalogs::default();
        let bounds = default_tuning_bounds(&incumbent, None, &none);
        let cases = [
            ("sonnet", "medium", true),
            ("haiku", "medium", false),
            ("fable", "medium", false),
            ("sonnet", "high", false),
        ];
        for (model, effort, admitted) in cases {
            let mut tuned = recipe_setting(model, Some(effort));
            tuned.revision = 1;
            let mut candidate = incumbent.clone();
            candidate.recipes.push(tuned.clone());
            let validated = validate_candidate(&incumbent, &candidate, &bounds).is_ok();

            let mut routed = incumbent.clone();
            routed.recipes = vec![tuned];
            let machine = machine_for(&routed);
            let resolves = matches!(
                decide_amont(&routed, &machine, &none),
                Routed::Route(route) if route.rung.model == model
            );
            assert_eq!(validated, admitted, "validate_candidate: {model}@{effort}");
            assert_eq!(resolves, admitted, "route: {model}@{effort}");
        }
    }

    /// Whether `validate_candidate` admits the recipe `tuned` (a revision
    /// of the incumbent's) and whether `route()` resolves a rung from it,
    /// under `machine` and `catalogs`.
    fn admitted_and_routed(
        incumbent: &RepoPolicy,
        tuned: RecipeSpec,
        machine_allowed: Option<Vec<String>>,
        catalogs: &EffortCatalogs,
    ) -> (bool, bool) {
        let bounds = default_tuning_bounds(incumbent, machine_allowed.as_deref(), catalogs);
        let mut candidate = incumbent.clone();
        candidate.recipes.push(tuned.clone());
        let admitted = validate_candidate(incumbent, &candidate, &bounds).is_ok();

        let mut routed = incumbent.clone();
        routed.recipes = vec![tuned];
        let mut machine = machine_for(&routed);
        machine.allowed_models = machine_allowed;
        let resolves = matches!(decide_amont(&routed, &machine, catalogs), Routed::Route(_));
        (admitted, resolves)
    }

    /// A candidate whose profile sets no effort, in a scope a floor applies
    /// to, is admitted exactly when routing resolves it: a model with
    /// `supported = []` or an unknown catalog cannot meet the floor, so it
    /// is refused as routing would block it.
    ///
    /// FALSIFIED: with `check_floor_without_effort` returning `Ok(())`
    /// for an absent effort, the `supported = []` and unknown-catalog cases
    /// were admitted while routing blocked them, and this failed; restored.
    #[test]
    fn candidate_validation_admits_what_a_floored_route_resolves() {
        let mut incumbent = repo_policy();
        incumbent.risk.push(floor_rule("high"));
        // One attempt: no escalation is reachable, so the P2 escalation-
        // floor block does not apply. Admission does not mirror that block
        // yet (see the holds-until at it); P3's shared ladder validation
        // closes the gap and this pin goes.
        incumbent.execution.max_attempts = 1;
        incumbent
            .recipes
            .push(recipe_setting("sonnet", Some("high")));
        let catalogs = catalogs_supporting(&[
            ("haiku", &[]),
            ("sonnet", &LEVELS),
            ("fable", &["low", "medium"]),
        ]);
        let cases = [
            ("sonnet", true),
            ("haiku", false),
            // Its set tops out under the floor.
            ("fable", false),
            // No catalog at all.
            ("opus", false),
        ];
        for (model, expected) in cases {
            let mut tuned = recipe_setting(model, None);
            tuned.revision = 1;
            let mut repo = incumbent.clone();
            repo.models.insert(
                Tier::Research,
                ModelProfile {
                    id: model.into(),
                    effort: None,
                    max_effort: None,
                },
            );
            // Only a model the repository configures can be admitted.
            let (admitted, resolves) = admitted_and_routed(&repo, tuned, None, &catalogs);
            assert_eq!(admitted, expected, "validate_candidate: {model}");
            assert_eq!(resolves, expected, "route: {model}");
        }
    }

    /// The models a candidate may name are the machine's `allowed_models`
    /// intersected with the repository's — what routing dispatches from.
    #[test]
    fn candidate_validation_admits_what_the_machines_allowed_models_route_resolves() {
        let incumbent = {
            let mut repo = repo_policy();
            repo.recipes.push(recipe_setting("sonnet", Some("medium")));
            repo
        };
        let catalogs = levels_catalog(&["haiku", "sonnet", "fable"]);
        let allowed = || Some(vec!["haiku".to_string(), "sonnet".to_string()]);
        let cases = [
            ("sonnet", true),
            ("haiku", true),
            // Configured by the repository, excluded by the machine.
            ("fable", false),
        ];
        for (model, expected) in cases {
            let mut tuned = recipe_setting(model, Some("medium"));
            tuned.revision = 1;
            let (admitted, resolves) = admitted_and_routed(&incumbent, tuned, allowed(), &catalogs);
            assert_eq!(admitted, expected, "validate_candidate: {model}");
            assert_eq!(resolves, expected, "route: {model}");
        }
    }

    /// An unknown catalog cannot satisfy a floor either: a floor raising
    /// the effort is a change.
    #[test]
    fn an_unknown_catalog_blocks_a_floor() {
        let mut repo = repo_policy();
        repo.risk.push(floor_rule("high"));
        let machine = machine_for(&repo);
        let blocker = blocker_of(decide_amont(&repo, &machine, &EffortCatalogs::default()));
        assert_eq!(blocker.code, BlockCode::EffortUnsupported);
        assert!(blocker.detail.contains("high"), "{}", blocker.detail);
    }

    /// (g) An identifier no code names, in the catalog and set by a
    /// recipe, routes as itself.
    #[test]
    fn an_unfamiliar_recipe_effort_routes_as_itself() {
        let mut repo = repo_policy();
        repo.recipes.push(recipe_setting("sonnet", Some("ultra")));
        let machine = machine_for(&repo);
        let levels = ["low", "high", "ultra"];
        let catalogs = catalog_of("sonnet", &levels, &levels, &levels, "ultra");
        let d = expect_route(decide_amont(&repo, &machine, &catalogs));
        assert_eq!(d.rung.effort, EffortRequest::Explicit(eid("ultra")));
    }

    /// An effort above the authority's per-tier ceiling is blocked, never
    /// clamped; lowering it is fine; and neither moves the ceiling.
    #[test]
    fn an_effort_above_the_authority_ceiling_is_blocked_and_never_clamped() {
        let mut authority_repo = repo_policy();
        authority_repo
            .models
            .get_mut(&Tier::Implementation)
            .expect("the implementation tier")
            .max_effort = Some(eid("medium"));

        let mut raised = authority_repo.clone();
        raised.recipes.push(recipe_setting("sonnet", Some("high")));
        let machine = machine_for(&raised);
        let blocker = blocker_of(decide_amont(&raised, &machine, &sonnet_catalog()));
        assert_eq!(blocker.code, BlockCode::EffortAboveCap);
        assert!(blocker.detail.contains("medium"), "{}", blocker.detail);

        let mut lowered = authority_repo.clone();
        lowered.recipes.push(recipe_setting("sonnet", Some("low")));
        let machine = machine_for(&lowered);
        let d = expect_route(decide_amont(&lowered, &machine, &sonnet_catalog()));
        assert_eq!(d.rung.effort, EffortRequest::Explicit(eid("low")));

        let ceiling = |repo: &RepoPolicy| {
            let machine = machine_for(repo);
            let authority = effective_authority(
                repo,
                &machine,
                &change_contract(&["crates/amont/**"]),
                &identity(),
            );
            authority_ceiling(&authority.models, &sonnet_catalog(), Tier::Implementation)
        };
        assert_eq!(ceiling(&raised), Some(eid("medium")));
        assert_eq!(
            ceiling(&raised),
            ceiling(&lowered),
            "a recipe's start effort never moves a ceiling"
        );
    }

    /// Without a policy ceiling the ceiling is the top of the authority
    /// model's admissible set, so an effort above the dispatched model's
    /// own top is blocked too.
    #[test]
    fn an_effort_above_the_admissible_top_is_blocked() {
        let mut repo = repo_policy();
        repo.recipes.push(recipe_setting("sonnet", Some("max")));
        let machine = machine_for(&repo);
        let capped = catalog_of("sonnet", &LEVELS, &LEVELS, &LEVELS, "high");
        let blocker = blocker_of(decide_amont(&repo, &machine, &capped));
        assert_eq!(blocker.code, BlockCode::EffortAboveCap);
    }

    /// The three effort states stay apart: a model with no control is
    /// `ControlUnsupported`, one with control and no request is
    /// `NotRequested`, and a configured effort on a model with no control
    /// is blocked at route time.
    #[test]
    fn the_three_effort_states_are_distinct() {
        let mut repo = repo_policy();
        repo.models
            .get_mut(&Tier::Implementation)
            .expect("the implementation tier")
            .effort = None;
        let machine = machine_for(&repo);
        let none = catalog_of("sonnet", &LEVELS, &LEVELS, &[], "max");
        let d = expect_route(decide_amont(&repo, &machine, &none));
        assert_eq!(d.rung.effort, EffortRequest::ControlUnsupported);
        assert!(d.explain().contains("sonnet, no effort control"));

        let d = expect_route(decide_amont(&repo, &machine, &sonnet_catalog()));
        assert_eq!(d.rung.effort, EffortRequest::NotRequested);
        assert!(d.explain().contains("sonnet, none requested"));

        let configured = repo_policy();
        let machine = machine_for(&configured);
        let blocker = blocker_of(decide_amont(&configured, &machine, &none));
        assert_eq!(blocker.code, BlockCode::EffortUnsupported);
    }

    /// `route:` is byte-identical, the effort line follows it directly,
    /// and every line fits in 80 columns.
    #[test]
    fn the_effort_line_follows_the_route_line_within_80_columns() {
        let mut repo = repo_policy();
        repo.recipes.push(recipe_setting("sonnet", Some("high")));
        repo.risk.push(floor_rule("max"));
        // The initial rung alone: no escalation is reachable.
        repo.execution.max_attempts = 1;
        let machine = machine_for(&repo);
        let d = expect_route(decide_amont(&repo, &machine, &sonnet_catalog()));
        let text = d.explain();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("route: implementation / sonnet"));
        assert_eq!(
            lines.next(),
            Some("effort: sonnet@max (set by recipe, raised by risk floor)")
        );
        assert!(
            text.lines()
                .filter(|line| !line.starts_with("reason:"))
                .all(|line| line.chars().count() <= 80),
            "{text}"
        );
    }

    #[test]
    fn a_recipe_that_does_not_cover_the_task_is_never_selected() {
        let mut repo = repo_policy();
        // Highest revision, enabled — but scoped to docs/**, which does
        // not cover this contract's write scope.
        repo.recipes.push(docs_recipe(5, true));
        let machine = machine_for(&repo);
        let outside_scope = change_contract(&["crates/**"]);
        let d = route_with(&outside_scope, &repo, &machine);
        assert_eq!(
            d.routed_by,
            RoutedBy::ConservativeBaseline,
            "a recipe whose scope_within does not cover the contract is never selected"
        );
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
