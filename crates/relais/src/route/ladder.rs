//! The effort ladder: the rung of every attempt a run's budget can reach
//! (SPEC §6).
//!
//! Resolved once, at route time, and executed as resolved: the state machine
//! only chooses the KIND of the next attempt, and the runner reads tier,
//! model and effort from the ladder at the index the machine names. Candidate
//! admission resolves the very same ladder ([`resolve_ladder`]) under the
//! incumbent's budget, so a candidate is admitted exactly when routing would
//! not block it.
//!
//! * The initial rung is [`super::rung::resolve_rung`]'s.
//! * A repair keeps its predecessor's tier and model. Under `raise` its
//!   effort is the catalog's `next()` of the previous rung's, and AT THE
//!   CEILING it stays there: no next step is not "no effort". Under `same`,
//!   or when the model's effort order is unknown, it is unchanged.
//! * An escalation is the escalation tier resolved like the initial rung,
//!   floor included. One whose model the machine's `allowed_models` removed
//!   is not a rung: the ladder ends before it, and no other model stands in.
//!
//! Every rung is validated here; any failure blocks the route before the
//! first attempt.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{Admissible, EffortCatalog, EffortCatalogs};
use crate::policy::{Blocker, EffortId, ModelProfile, RecipeSpec, RepairEffort, Tier};

use super::rung::{
    authority_ceiling, resolve_rung, validate_rung, EffortRequest, ResolvedRung, RungRequest,
};
use super::{RouteReason, Rung};

/// Where a rung sits in a [`Ladder`]: 0 is the initial attempt, and each
/// later attempt is the next index, so a machine that knows the attempt it
/// is on names the next rung without knowing what is in any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RungIndex(usize);

impl RungIndex {
    /// The first attempt's rung.
    pub const INITIAL: Self = Self(0);

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

/// What one rung of a ladder is for. Derived from where the rung sits, never
/// stored beside it, so the two cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RungRole {
    Initial,
    Repair,
    Escalation,
}

impl RungRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Repair => "repair",
            Self::Escalation => "escalation",
        }
    }
}

/// The ordered rungs for every attempt the budget can reach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ladder {
    rungs: Vec<Rung>,
    /// Where the escalation rung sits, when the ladder has one.
    escalation: Option<RungIndex>,
    /// Models whose repair effort is held at the previous attempt's because
    /// their effort order is unknown, `raise` notwithstanding.
    held: Vec<String>,
}

impl Ladder {
    /// A ladder of one rung: the attempt that always runs.
    pub fn single(rung: Rung) -> Self {
        Self {
            rungs: vec![rung],
            escalation: None,
            held: Vec::new(),
        }
    }

    pub fn rung(&self, at: RungIndex) -> Option<&Rung> {
        self.rungs.get(at.get())
    }

    pub fn initial(&self) -> &Rung {
        &self.rungs[0]
    }

    pub fn rungs(&self) -> &[Rung] {
        &self.rungs
    }

    /// The escalation rung, when the budget reaches one and its model is
    /// allowed.
    pub fn escalation(&self) -> Option<&Rung> {
        self.escalation.and_then(|at| self.rung(at))
    }

    /// What the rung at `at` is for.
    pub fn role(&self, at: RungIndex) -> RungRole {
        if at == RungIndex::INITIAL {
            RungRole::Initial
        } else if self.escalation == Some(at) {
            RungRole::Escalation
        } else {
            RungRole::Repair
        }
    }

    /// The models whose repair effort is held, in first-seen order.
    pub fn held(&self) -> &[String] {
        &self.held
    }

    /// Every rung after the initial one, with its role, in dispatch order.
    pub fn after_initial(&self) -> impl Iterator<Item = (RungRole, &Rung)> {
        self.rungs
            .iter()
            .enumerate()
            .skip(1)
            .map(|(at, rung)| (self.role(RungIndex(at)), rung))
    }
}

/// The attempts a budget allows: what decides which rungs are reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LadderBudget {
    pub max_attempts: u32,
    pub max_repairs_before_escalation: u32,
}

impl LadderBudget {
    fn reachable_repairs(self) -> u32 {
        self.max_repairs_before_escalation
            .min(self.max_attempts.saturating_sub(1))
    }

    /// An escalation is reachable when the attempts outlast the repairs:
    /// the initial attempt, every repair, then the escalated one.
    fn escalation_reachable(self) -> bool {
        self.max_attempts > self.max_repairs_before_escalation.saturating_add(1)
    }
}

/// The tier a failure at `selected` may escalate to: the strongest ELIGIBLE
/// tier above it. The one selection, shared by `route()` and candidate
/// admission, so the two cannot name different escalations.
pub(super) fn escalation_target(eligible: &[Tier], selected: Tier) -> Option<Tier> {
    eligible
        .iter()
        .rev()
        .find(|tier| **tier > selected)
        .copied()
}

pub(super) struct LadderRequest<'a> {
    pub tier: Tier,
    /// The stronger tier a failure may escalate to, when policy authorizes
    /// one.
    pub escalation_tier: Option<Tier>,
    /// The authority policy's model table, already narrowed by the machine.
    pub models: &'a BTreeMap<Tier, ModelProfile>,
    /// machine.toml's `allowed_models` (`None`: any).
    pub allowed_models: Option<&'a [String]>,
    pub recipe: Option<&'a RecipeSpec>,
    pub floors: &'a [EffortId],
    pub catalogs: &'a EffortCatalogs,
    pub budget: LadderBudget,
    pub repair_effort: RepairEffort,
}

pub(super) struct ResolvedLadder {
    pub ladder: Ladder,
    /// The escalation tier the ladder keeps: the request's, unless the
    /// machine's `allowed_models` removed its model.
    pub escalation_tier: Option<Tier>,
    pub reasons: Vec<RouteReason>,
}

/// The ladder for `request`, or the blocker of the first rung that cannot
/// be dispatched.
pub(super) fn resolve_ladder(request: LadderRequest<'_>) -> Result<ResolvedLadder, Blocker> {
    let LadderRequest {
        tier,
        escalation_tier,
        models,
        allowed_models,
        recipe,
        floors,
        catalogs,
        budget,
        repair_effort,
    } = request;
    let ResolvedRung {
        rung: initial,
        mut reasons,
    } = resolve_rung(RungRequest {
        tier,
        models,
        allowed_models,
        recipe,
        floors,
        catalogs,
    })?;

    let mut held = Vec::new();
    let mut rungs = vec![initial];
    for _ in 0..budget.reachable_repairs() {
        let previous = rungs
            .last()
            .expect("the ladder starts with its initial rung");
        let repair = repair_of(previous, models, catalogs, repair_effort, &mut held);
        validate_rung(&repair, models, catalogs)?;
        rungs.push(repair);
    }

    let mut escalation = None;
    let mut kept_tier = escalation_tier;
    if let Some(target) = escalation_tier {
        let request = RungRequest {
            tier: target,
            models,
            allowed_models,
            recipe,
            floors,
            catalogs,
        };
        if removed_by_allowed_models(&request) {
            kept_tier = None;
            reasons.push(RouteReason::new(
                "escalation_model_not_allowed",
                format!(
                    "the {} tier's model is not in the machine's allowed_models; the ladder \
                     ends before it and no other model is substituted",
                    target.as_str()
                ),
            ));
        } else if budget.escalation_reachable() {
            let escalated = resolve_rung(request).map_err(|blocker| Blocker {
                code: blocker.code,
                detail: format!(
                    "the escalation rung, the {} tier, is reachable within the attempt budget \
                     and cannot be dispatched: {}",
                    target.as_str(),
                    blocker.detail
                ),
            })?;
            escalation = Some(RungIndex(rungs.len()));
            rungs.push(escalated.rung);
        }
    }

    Ok(ResolvedLadder {
        ladder: Ladder {
            rungs,
            escalation,
            held,
        },
        escalation_tier: kept_tier,
        reasons,
    })
}

/// Whether the machine's `allowed_models` removes the model an escalation
/// to `request.tier` would dispatch (the covering recipe's, else the
/// policy's).
fn removed_by_allowed_models(request: &RungRequest<'_>) -> bool {
    let profile = request
        .recipe
        .and_then(|spec| spec.models.as_ref())
        .and_then(|models| models.get(&request.tier))
        .or_else(|| request.models.get(&request.tier));
    match (profile, request.allowed_models) {
        (Some(profile), Some(allowed)) => !allowed.contains(&profile.id),
        (None, _) | (Some(_), None) => false,
    }
}

/// The repair of the attempt that ran `previous`: same tier and model, and
/// the effort `repair_effort` says. A model whose effort order is unknown
/// is held at `previous`'s effort and named in `held`.
fn repair_of(
    previous: &Rung,
    models: &BTreeMap<Tier, ModelProfile>,
    catalogs: &EffortCatalogs,
    repair_effort: RepairEffort,
    held: &mut Vec<String>,
) -> Rung {
    let effort = match (&previous.effort, repair_effort) {
        (EffortRequest::Explicit(effort), RepairEffort::Raise) => {
            if repair_effort_held(&previous.model, &previous.effort, catalogs, repair_effort) {
                if !held.contains(&previous.model) {
                    held.push(previous.model.clone());
                }
                previous.effort.clone()
            } else {
                // At the ceiling there is no next step, and the repair
                // stays where the attempt was: never "no effort".
                raised(
                    effort,
                    catalogs.get(&previous.model),
                    models,
                    catalogs,
                    previous,
                )
                .map_or_else(|| previous.effort.clone(), EffortRequest::Explicit)
            }
        }
        (EffortRequest::Explicit(_), RepairEffort::Same)
        | (EffortRequest::NotRequested, RepairEffort::Raise | RepairEffort::Same)
        | (EffortRequest::ControlUnsupported, RepairEffort::Raise | RepairEffort::Same) => {
            previous.effort.clone()
        }
    };
    Rung {
        tier: previous.tier,
        model: previous.model.clone(),
        effort,
    }
}

/// The next admissible effort after `effort`, unless that passes the tier's
/// ceiling. `None` at the top: the caller keeps the effort it has.
fn raised(
    effort: &EffortId,
    catalog: Option<&EffortCatalog>,
    models: &BTreeMap<Tier, ModelProfile>,
    catalogs: &EffortCatalogs,
    previous: &Rung,
) -> Option<EffortId> {
    let catalog = catalog?;
    let ceiling = authority_ceiling(models, catalogs, previous.tier);
    catalog.next(effort).filter(|next| {
        ceiling.as_ref().is_none_or(|ceiling| {
            catalog
                .position(next)
                .zip(catalog.position(ceiling))
                .is_some_and(|(at, cap)| at <= cap)
        })
    })
}

/// Whether the catalog decides which efforts exist and in what order.
fn order_known(catalog: Option<&EffortCatalog>) -> bool {
    catalog.is_some_and(|catalog| match catalog.admissible() {
        Admissible::Set(_) => true,
        Admissible::Undetermined(_) => false,
    })
}

/// Whether a repair of an attempt that ran `model` at `effort` has its effort
/// held: `raise` was asked, an effort was configured, and the model's effort
/// order is unknown. The one predicate behind [`Ladder::held`] and
/// [`held_repair_models`].
fn repair_effort_held(
    model: &str,
    effort: &EffortRequest,
    catalogs: &EffortCatalogs,
    repair_effort: RepairEffort,
) -> bool {
    matches!(effort, EffortRequest::Explicit(_))
        && repair_effort == RepairEffort::Raise
        && !order_known(catalogs.get(model))
}

/// The models a repair COULD run at with its effort held: every configured
/// tier's model with a configured effort and an unknown order. What `relais
/// doctor` reports. Doctor has no task and no budget, and a repair runs at
/// the initial rung's tier — which a risk floor can make any configured
/// tier — so this is a superset of one run's [`Ladder::held`], the models
/// `relais plan` names for that run's own ladder.
pub fn held_repair_models(
    models: &BTreeMap<Tier, ModelProfile>,
    catalogs: &EffortCatalogs,
    repair_effort: RepairEffort,
) -> Vec<String> {
    let mut held: Vec<String> = Vec::new();
    for profile in models.values() {
        let Some(effort) = profile.effort.clone() else {
            continue;
        };
        if repair_effort_held(
            &profile.id,
            &EffortRequest::Explicit(effort),
            catalogs,
            repair_effort,
        ) && !held.contains(&profile.id)
        {
            held.push(profile.id.clone());
        }
    }
    held
}

/// The line `relais plan` prints on stderr for each model THIS run's ladder
/// holds ([`Ladder::held`]).
pub fn held_text(model: &str) -> String {
    format!("repair effort held: effort order unknown for {model}")
}

/// The line `relais doctor` adds to its effort finding: conditional, because
/// doctor has no task or budget and names the models a repair COULD run at
/// held ([`held_repair_models`]), not the ones a run's ladder holds.
pub fn held_would_text(models: &[String]) -> String {
    format!(
        "repair effort would be held (effort order unknown) for: {}",
        models.join(", ")
    )
}
