//! The rung: the tier, model and effort a route dispatches first (SPEC §6).
//!
//! Resolved here from the authority policy, the covering recipe's
//! `models[tier]`, the risk rules' effort floors and the effort catalogs the
//! caller resolved. Pure: nothing is probed, nothing is read from disk.
//!
//! * The model comes from the recipe when it names one for the tier, else
//!   from the authority policy, and must be in the machine's
//!   `allowed_models` — never a fallback to another model.
//! * A floor binds the initial rung: the effort is the higher of the start
//!   effort and the floor in the catalog's order, and a model that cannot
//!   satisfy it blocks the route. The escalation rung is resolved the same
//!   way (`ladder`), so a floor binds every rung the budget can reach.
//! * A ceiling is never a clamp: an effort above the smaller of the
//!   authority ceiling and the top of the dispatched model's admissible set
//!   blocks the route.
//! * An unknown catalog lets exactly the authority's configured effort run,
//!   as before; any effort a recipe or a floor changed needs a known one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{Admissible, EffortCatalog, EffortCatalogs, EffortSet, Fact};
use crate::policy::{
    BlockCode, Blocker, EffectiveAuthority, EffortId, MachineSettings, ModelProfile, RecipeSpec,
    RepoPolicy, Tier,
};

use super::RouteReason;

/// What a dispatch asks of the harness about effort. Three states that a
/// caller must tell apart, so never an `Option`: `None` would say nothing
/// about whether the model has no control or nobody asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortRequest {
    /// `--effort <id>` is passed.
    Explicit(EffortId),
    /// The model has effort control and none is requested.
    NotRequested,
    /// The model or the CLI has no effort control; none is passed.
    ControlUnsupported,
}

impl EffortRequest {
    /// The effort to pass on the command line, when there is one.
    pub fn id(&self) -> Option<&EffortId> {
        match self {
            Self::Explicit(effort) => Some(effort),
            Self::NotRequested | Self::ControlUnsupported => None,
        }
    }
}

/// One dispatch of a route: the tier, model and effort an attempt runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rung {
    pub tier: Tier,
    pub model: String,
    pub effort: EffortRequest,
}

impl Rung {
    /// The profile a dispatch of this rung is launched with.
    pub fn profile(&self) -> ModelProfile {
        ModelProfile {
            id: self.model.clone(),
            effort: self.effort.id().cloned(),
            max_effort: None,
        }
    }
}

/// The reason ids `Route::explain` reads back to say who set the effort.
pub(super) const RECIPE_RUNG: &str = "recipe_rung";
pub(super) const EFFORT_FLOOR: &str = "effort_floor";

/// The AUTHORITY policy's ceiling for `tier`: its `max_effort` when set,
/// else the top of the authority model's admissible set. Resolved from the
/// policy's own `models` alone — never from a recipe or a candidate — so
/// changing a start effort cannot move it. `None` when the policy names no
/// ceiling and the set's top is not decidable.
pub fn authority_ceiling(
    models: &BTreeMap<Tier, ModelProfile>,
    catalogs: &EffortCatalogs,
    tier: Tier,
) -> Option<EffortId> {
    let profile = models.get(&tier)?;
    profile
        .max_effort
        .clone()
        .or_else(|| catalogs.get(&profile.id).and_then(EffortCatalog::top))
}

/// Every model the router may dispatch for `repo` under `authority`: each
/// tier's, and each recipe's own. The catalogs the caller resolves cover
/// exactly these.
pub fn models_in_play<'a>(repo: &'a RepoPolicy, authority: &'a EffectiveAuthority) -> Vec<&'a str> {
    let recipe_models = repo
        .recipes
        .iter()
        .filter_map(|recipe| recipe.models.as_ref())
        .flat_map(|models| models.values());
    authority
        .models
        .values()
        .chain(recipe_models)
        .map(|profile| profile.id.as_str())
        .collect()
}

/// The catalogs [`super::RouteInputs::catalogs`] carries, for every model
/// in play: what the harness accepts (`cli`, `None` when it could not be
/// probed — every fact of it unknown) and machine.toml's `[efforts]` and
/// `[routing] max_effort`. The one place the callers (`relais plan`, the
/// runner preflight) resolve them, so they cannot disagree.
pub fn resolve_catalogs(
    repo: &RepoPolicy,
    authority: &EffectiveAuthority,
    machine: &MachineSettings,
    cli: Option<&EffortSet>,
) -> EffortCatalogs {
    let cli = cli.cloned().unwrap_or(Fact::Unknown);
    EffortCatalogs::resolve_all(
        &cli,
        &machine.efforts,
        &machine.routing.max_effort,
        models_in_play(repo, authority),
    )
}

pub(super) struct RungRequest<'a> {
    pub tier: Tier,
    /// The authority policy's model table, already narrowed by the machine.
    pub models: &'a BTreeMap<Tier, ModelProfile>,
    /// machine.toml's `allowed_models` (`None`: any).
    pub allowed_models: Option<&'a [String]>,
    /// The recipe that covers the task, when one does.
    pub recipe: Option<&'a RecipeSpec>,
    /// The `minimum_effort` of every risk rule the scope could touch.
    pub floors: &'a [EffortId],
    pub catalogs: &'a EffortCatalogs,
}

pub(super) struct ResolvedRung {
    pub rung: Rung,
    pub reasons: Vec<RouteReason>,
}

fn blocked(code: BlockCode, detail: String) -> Blocker {
    Blocker { code, detail }
}

/// The machine.toml keys that turn an unknown catalog into a known one.
const CATALOG_KEYS: &str = "[efforts] order, [[efforts.models]] supported";

fn set_text(set: &[EffortId]) -> String {
    if set.is_empty() {
        "none".to_string()
    } else {
        set.iter()
            .map(EffortId::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn undetermined_text(catalog: Option<&EffortCatalog>) -> String {
    match catalog.map(EffortCatalog::admissible) {
        Some(Admissible::Undetermined(unknown)) => unknown
            .iter()
            .map(|fact| fact.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        Some(Admissible::Set(_)) | None => "cli-accepted, model support, order".to_string(),
    }
}

/// Resolve the initial rung for `request.tier`, or the blocker that stops
/// it.
pub(super) fn resolve_rung(request: RungRequest<'_>) -> Result<ResolvedRung, Blocker> {
    let RungRequest {
        tier,
        models,
        allowed_models,
        recipe,
        floors,
        catalogs,
    } = request;
    let Some(authority_profile) = models.get(&tier) else {
        return Err(blocked(
            BlockCode::ModelUnavailable,
            format!("no model configured for the {} tier", tier.as_str()),
        ));
    };
    let recipe_profile = recipe
        .and_then(|spec| spec.models.as_ref())
        .and_then(|models| models.get(&tier));
    let profile = recipe_profile.unwrap_or(authority_profile);
    let mut reasons = Vec::new();

    if let Some(allowed) = allowed_models {
        if !allowed.contains(&profile.id) {
            return Err(blocked(
                BlockCode::ModelNotAllowed,
                format!(
                    "the {} tier's model `{}`{} is not in the machine's allowed_models; no \
                     other model is substituted",
                    tier.as_str(),
                    profile.id,
                    if recipe_profile.is_some() {
                        " (named by the covering recipe)"
                    } else {
                        ""
                    }
                ),
            ));
        }
    }
    if let (Some(spec), Some(_)) = (recipe, recipe_profile) {
        reasons.push(RouteReason::new(
            RECIPE_RUNG,
            format!(
                "recipe `{}` sets the {} tier to {}{}",
                spec.name,
                tier.as_str(),
                profile.id,
                profile
                    .effort
                    .as_ref()
                    .map(|effort| format!("@{effort}"))
                    .unwrap_or_default()
            ),
        ));
    }

    let catalog = catalogs.get(&profile.id);
    // The effort the authority policy itself configures for this tier and
    // this model: the only one an unknown catalog lets through.
    let configured = if authority_profile.id == profile.id {
        authority_profile.effort.as_ref()
    } else {
        None
    };

    let start = profile.effort.as_ref();
    let effort = if floors.is_empty() {
        start.cloned()
    } else {
        let raised = apply_floor(catalog, &profile.id, start, floors)?;
        if Some(&raised) != start {
            reasons.push(RouteReason::new(
                EFFORT_FLOOR,
                format!("a risk rule requires effort of at least {raised}"),
            ));
        }
        Some(raised)
    };

    let request = match effort {
        Some(effort) => {
            check_effort(
                &effort,
                catalog,
                configured,
                authority_ceiling(models, catalogs, tier).as_ref(),
                &profile.id,
            )?;
            EffortRequest::Explicit(effort)
        }
        None => match catalog {
            Some(catalog) if !catalog.unsupported().is_empty() => EffortRequest::ControlUnsupported,
            Some(_) | None => EffortRequest::NotRequested,
        },
    };
    Ok(ResolvedRung {
        rung: Rung {
            tier,
            model: profile.id.clone(),
            effort: request,
        },
        reasons,
    })
}

/// max(start, highest floor) in the catalog's order, snapped up to the
/// lowest admissible effort at or above it. Every way the model cannot
/// satisfy a floor is a blocker naming the floor, the model and the
/// admissible set.
pub(super) fn apply_floor(
    catalog: Option<&EffortCatalog>,
    model: &str,
    start: Option<&EffortId>,
    floors: &[EffortId],
) -> Result<EffortId, Blocker> {
    let floor_names = floor_names(floors);
    let unmet = |why: String, admissible: &str| {
        blocked(
            BlockCode::EffortUnsupported,
            format!(
                "risk floor `{floor_names}` cannot be met by `{model}`: {why} (admissible \
                 efforts: {admissible})"
            ),
        )
    };
    let Some(catalog) = catalog else {
        return Err(unmet(
            format!("its catalog is unknown; set {CATALOG_KEYS} in machine.toml"),
            "unknown",
        ));
    };
    let set = match catalog.admissible() {
        Admissible::Set(set) => set,
        Admissible::Undetermined(_) => {
            return Err(unmet(
                format!(
                    "its catalog is unknown ({}); set {CATALOG_KEYS} in machine.toml",
                    undetermined_text(Some(catalog))
                ),
                "unknown",
            ))
        }
    };
    if set.is_empty() {
        return Err(unmet(
            "it has no effort control or no admissible effort".to_string(),
            "none",
        ));
    }
    let admissible = set_text(&set);
    let floor = match catalog.highest(floors) {
        Ok(Some(floor)) => floor,
        Ok(None) => return Err(unmet("no floor to rank".to_string(), &admissible)),
        Err(unplaced) => {
            return Err(unmet(
                format!("the effort order does not name `{unplaced}`"),
                &admissible,
            ))
        }
    };
    let floor_at = catalog
        .position(&floor)
        .ok_or_else(|| unmet("the floor is not in the order".to_string(), &admissible))?;
    let target_at = match start {
        Some(start) => {
            let start_at = catalog.position(start).ok_or_else(|| {
                unmet(
                    format!("the effort order does not name the start effort `{start}`"),
                    &admissible,
                )
            })?;
            start_at.max(floor_at)
        }
        None => floor_at,
    };
    set.iter()
        .find(|effort| catalog.position(effort).is_some_and(|at| at >= target_at))
        .cloned()
        .ok_or_else(|| unmet("none is at or above the floor".to_string(), &admissible))
}

fn floor_names(floors: &[EffortId]) -> String {
    floors
        .iter()
        .map(EffortId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A rung the ladder derived (a repair) checked as the initial one is: its
/// effort must pass [`check_effort`] against the tier's configured effort,
/// catalog and authority ceiling. A rung with no explicit effort asks
/// nothing of the model and passes.
pub(super) fn validate_rung(
    rung: &Rung,
    models: &BTreeMap<Tier, ModelProfile>,
    catalogs: &EffortCatalogs,
) -> Result<(), Blocker> {
    let EffortRequest::Explicit(effort) = &rung.effort else {
        return Ok(());
    };
    let configured = models
        .get(&rung.tier)
        .filter(|profile| profile.id == rung.model)
        .and_then(|profile| profile.effort.as_ref());
    check_effort(
        effort,
        catalogs.get(&rung.model),
        configured,
        authority_ceiling(models, catalogs, rung.tier).as_ref(),
        &rung.model,
    )
}

/// The ceiling and carve-out rules for one effort about to be dispatched.
fn check_effort(
    effort: &EffortId,
    catalog: Option<&EffortCatalog>,
    configured: Option<&EffortId>,
    authority_ceiling: Option<&EffortId>,
    model: &str,
) -> Result<(), Blocker> {
    let known = catalog.and_then(|catalog| match catalog.admissible() {
        Admissible::Set(set) => Some((catalog, set)),
        Admissible::Undetermined(_) => None,
    });
    let Some((catalog, set)) = known else {
        // CARVE-OUT: an unknown catalog admits exactly the authority's
        // configured effort, as before, and nothing changed away from it.
        if configured == Some(effort) {
            return Ok(());
        }
        return Err(blocked(
            BlockCode::EffortUnsupported,
            format!(
                "effort `{effort}` for `{model}` differs from the configured effort and its \
                 catalog is unknown ({}); set {CATALOG_KEYS} in machine.toml",
                undetermined_text(catalog)
            ),
        ));
    };
    if set.is_empty() {
        return Err(blocked(
            BlockCode::EffortUnsupported,
            format!(
                "effort `{effort}` was requested of `{model}`, which admits no effort \
                 (admissible efforts: none)"
            ),
        ));
    }
    let admissible = set_text(&set);
    let above_cap = |cap: &str| {
        blocked(
            BlockCode::EffortAboveCap,
            format!(
                "effort `{effort}` for `{model}` is above its ceiling `{cap}` (admissible \
                 efforts: {admissible}); it is refused, never lowered"
            ),
        )
    };
    let placed = catalog.position(effort);
    if let (Some(at), Some(top)) = (placed, set.last()) {
        if catalog.position(top).is_some_and(|top_at| at > top_at) {
            return Err(above_cap(top.as_str()));
        }
    }
    if let Some(ceiling) = authority_ceiling {
        match (placed, catalog.position(ceiling)) {
            (Some(at), Some(ceiling_at)) if at > ceiling_at => {
                return Err(above_cap(ceiling.as_str()))
            }
            (_, None) => {
                return Err(blocked(
                    BlockCode::EffortAboveCap,
                    format!(
                        "the authority ceiling `{ceiling}` is not in `{model}`'s effort order, \
                         so effort `{effort}` cannot be checked against it"
                    ),
                ))
            }
            (Some(_) | None, Some(_)) => {}
        }
    }
    if set.contains(effort) {
        Ok(())
    } else {
        Err(blocked(
            BlockCode::EffortUnsupported,
            format!(
                "effort `{effort}` is not admissible for `{model}` (admissible efforts: \
                 {admissible})"
            ),
        ))
    }
}
