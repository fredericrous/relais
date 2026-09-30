//! The learner's arms: the (tier, effort) pairs it chooses among (SPEC §6).
//!
//! An arm is not a new rule about effort. Whether a tier can start at an
//! effort is asked of [`resolve_rung`], the resolver every route uses, so the
//! floor, the authoritative ceiling, `allowed_models` and the unknown-catalog
//! carve-out are the resolver's and are never restated here.

use std::collections::BTreeMap;
use std::fmt;

use crate::catalog::{Admissible, EffortCatalog, EffortCatalogs};
use crate::policy::{EffortId, ModelProfile, Tier};

use super::rung::{resolve_rung, EffortRequest, RungRequest};

/// One thing the learner can choose: a tier, dispatched with an effort
/// request. Distinct efforts of one tier are distinct arms; so are the two
/// states that pass no effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    pub tier: Tier,
    pub effort: EffortRequest,
}

impl Arm {
    /// The stable name the ledger, the inference result and the route
    /// reasons use: `implementation@high`, `implementation:not_requested`,
    /// `implementation:control_unsupported`.
    pub fn label(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Arm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tier = self.tier.as_str();
        match &self.effort {
            EffortRequest::Explicit(effort) => write!(f, "{tier}@{effort}"),
            EffortRequest::NotRequested => write!(f, "{tier}:not_requested"),
            EffortRequest::ControlUnsupported => write!(f, "{tier}:control_unsupported"),
        }
    }
}

/// The arms of every `eligible` tier, in tier order, each tier's in the
/// catalog's order.
///
/// With no recipe covering the task (the learner only runs then), a tier's
/// arms are the efforts of its model's admissible set whose initial rung
/// RESOLVES to exactly that effort when it is the start. When the set is
/// undetermined or empty, the tier contributes the one arm the resolver
/// gives with no override: the configured effort, `ControlUnsupported` or
/// `NotRequested`. A tier whose rung is blocked contributes none.
pub fn learner_arms(
    eligible: &[Tier],
    models: &BTreeMap<Tier, ModelProfile>,
    allowed_models: Option<&[String]>,
    floors: &[EffortId],
    catalogs: &EffortCatalogs,
) -> Vec<Arm> {
    let resolves_to = |tier: Tier, start_effort: Option<&EffortId>| {
        resolve_rung(RungRequest {
            tier,
            models,
            allowed_models,
            recipe: None,
            floors,
            catalogs,
            start_effort,
        })
        .map(|resolved| resolved.rung.effort)
    };
    eligible
        .iter()
        .flat_map(|&tier| {
            let candidates = models
                .get(&tier)
                .and_then(|profile| catalogs.get(&profile.id))
                .map(EffortCatalog::admissible)
                .and_then(|admissible| match admissible {
                    Admissible::Set(set) if !set.is_empty() => Some(set),
                    Admissible::Set(_) | Admissible::Undetermined(_) => None,
                });
            let efforts: Vec<EffortRequest> = match candidates {
                Some(set) => set
                    .iter()
                    .filter_map(|candidate| {
                        // A start the resolver refuses (floor, ceiling,
                        // allowed_models) is no arm, so its blocker is dropped.
                        let resolved = resolves_to(tier, Some(candidate)).ok()?;
                        (resolved == EffortRequest::Explicit(candidate.clone())).then_some(resolved)
                    })
                    .collect(),
                // A blocked rung has no arm; the blocker is the route's to
                // report when the baseline runs into it.
                None => resolves_to(tier, None).ok().into_iter().collect(),
            };
            efforts.into_iter().map(move |effort| Arm { tier, effort })
        })
        .collect()
}
