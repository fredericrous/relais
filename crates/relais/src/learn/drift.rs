//! Model-alias drift (SPEC §11): a requested alias such as `sonnet`
//! silently starting to run a different effective model.
//!
//! Pure: it decides from observations the ledger read and value pairs the
//! caller collected, and touches no file, clock or process. Claude Code
//! 2.1.284 moved `sonnet` from `claude-sonnet-5` to `claude-sonnet-5-5`
//! and nothing on this machine said so; every figure straddling the move
//! mixed two models.
//!
//! What is NOT drift matters as much. On the measured ledger `sonnet`
//! also ran `claude-opus-5-5` three times while `claude-sonnet-5` was
//! still running: a worker spawning an opus subagent, an unapproved
//! substitution, not an alias moving. A model whose events overlap the
//! current one's span is therefore an anomaly and produces no switch.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ledger::{ModelObservation, ModelPair};

/// Fewest events a model needs before it can supersede another. One event
/// is a stray, not a new resolution of the alias.
const MIN_EVENTS_TO_SUPERSEDE: u64 = 2;

/// An alias that stopped running `from` and now runs `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasSwitch {
    pub alias: String,
    pub from: String,
    pub to: String,
    /// The last event that ran `from`.
    pub last_from: String,
    /// The first event that ran `to`.
    pub first_to: String,
}

/// Every switch the observations show, grouped by alias (alphabetical)
/// and in time order within one.
///
/// Model B SUPERSEDES the alias's current model A when B's first event is
/// after A's LAST event and B has at least [`MIN_EVENTS_TO_SUPERSEDE`]
/// events. A model that overlaps A's span, or has a single event, is an
/// anomaly and yields nothing. Successive switches chain: A to B to C is
/// two entries, never a third A to C. Timestamps compare as strings, which
/// is exact for the ledger's RFC3339 values in one offset.
///
/// FALSIFY: drop the "first after last" condition (the comparison of
/// `first_seen` with `last_seen`) and
/// `an_overlapping_model_is_an_anomaly_not_a_switch` fails, the opus-5-5 events becoming a switch away from `claude-sonnet-5`
/// that the measured ledger says never happened. Confirmed, then restored.
pub fn alias_switches(observations: &[ModelObservation]) -> Vec<AliasSwitch> {
    let mut by_alias: BTreeMap<&str, Vec<&ModelObservation>> = BTreeMap::new();
    for observation in observations {
        by_alias
            .entry(observation.requested.as_str())
            .or_default()
            .push(observation);
    }
    let mut switches = Vec::new();
    for (alias, mut models) in by_alias {
        models.sort_by(|a, b| (&a.first_seen, &a.effective).cmp(&(&b.first_seen, &b.effective)));
        let mut models = models.into_iter();
        let Some(mut current) = models.next() else {
            continue;
        };
        for candidate in models {
            let supersedes = candidate.count >= MIN_EVENTS_TO_SUPERSEDE
                && candidate.first_seen > current.last_seen;
            if supersedes {
                switches.push(AliasSwitch {
                    alias: alias.to_string(),
                    from: current.effective.clone(),
                    to: candidate.effective.clone(),
                    last_from: current.last_seen.clone(),
                    first_to: candidate.first_seen.clone(),
                });
                current = candidate;
            }
        }
    }
    switches
}

/// One alias that ran different effective models in the two sides of a
/// comparison. A caveat on the evidence, never a gate on promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCaveat {
    pub alias: String,
    pub incumbent: BTreeSet<String>,
    pub candidate: BTreeSet<String>,
}

impl std::fmt::Display for ModelCaveat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let join = |models: &BTreeSet<String>| {
            models
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        };
        write!(
            f,
            "alias {} ran {} in incumbent runs and {} in candidate runs",
            self.alias,
            join(&self.incumbent),
            join(&self.candidate)
        )
    }
}

/// The aliases both sides used whose sets of effective models differ,
/// alphabetically. An alias only one side used cannot be compared, so it
/// is not a caveat.
pub fn model_caveats(incumbent: &[ModelPair], candidate: &[ModelPair]) -> Vec<ModelCaveat> {
    let incumbent = models_by_alias(incumbent);
    let candidate = models_by_alias(candidate);
    incumbent
        .into_iter()
        .filter_map(|(alias, incumbent)| {
            let candidate = candidate.get(&alias)?;
            (&incumbent != candidate).then(|| ModelCaveat {
                alias,
                incumbent,
                candidate: candidate.clone(),
            })
        })
        .collect()
}

fn models_by_alias(pairs: &[ModelPair]) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_alias: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pair in pairs {
        by_alias
            .entry(pair.requested.clone())
            .or_default()
            .insert(pair.effective.clone());
    }
    by_alias
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(alias: &str, model: &str, first: &str, last: &str, count: u64) -> ModelObservation {
        ModelObservation {
            requested: alias.into(),
            effective: model.into(),
            first_seen: first.into(),
            last_seen: last.into(),
            count,
        }
    }

    fn pair(alias: &str, model: &str) -> ModelPair {
        ModelPair {
            requested: alias.into(),
            effective: model.into(),
        }
    }

    fn sonnet_5() -> ModelObservation {
        seen(
            "sonnet",
            "claude-sonnet-5",
            "2026-09-23T00:39:00+00:00",
            "2026-09-28T17:35:00+00:00",
            58,
        )
    }

    fn sonnet_5_5() -> ModelObservation {
        seen(
            "sonnet",
            "claude-sonnet-5-5",
            "2026-09-28T20:08:00+00:00",
            "2026-09-29T09:00:00+00:00",
            10,
        )
    }

    fn stray_opus() -> ModelObservation {
        seen(
            "sonnet",
            "claude-opus-5-5",
            "2026-09-24T10:00:00+00:00",
            "2026-09-27T10:00:00+00:00",
            3,
        )
    }

    #[test]
    fn a_model_that_starts_after_the_last_ran_and_has_events_is_a_switch() {
        assert_eq!(
            alias_switches(&[sonnet_5_5(), sonnet_5()]),
            vec![AliasSwitch {
                alias: "sonnet".into(),
                from: "claude-sonnet-5".into(),
                to: "claude-sonnet-5-5".into(),
                last_from: "2026-09-28T17:35:00+00:00".into(),
                first_to: "2026-09-28T20:08:00+00:00".into(),
            }]
        );
    }

    /// The measured ledger: `sonnet` ran opus-5-5 three times inside the
    /// sonnet-5 span. Reported as a switch it would tell the person the
    /// alias moved to opus, which never happened.
    #[test]
    fn an_overlapping_model_is_an_anomaly_not_a_switch() {
        let switches = alias_switches(&[sonnet_5(), stray_opus(), sonnet_5_5()]);
        assert_eq!(switches.len(), 1, "only the real move: {switches:?}");
        assert_eq!(switches[0].to, "claude-sonnet-5-5");
        assert!(alias_switches(&[sonnet_5(), stray_opus()]).is_empty());
    }

    #[test]
    fn a_single_stray_event_after_the_last_is_not_a_switch() {
        let stray = seen(
            "sonnet",
            "claude-opus-5-5",
            "2026-09-29T00:00:00+00:00",
            "2026-09-29T00:00:00+00:00",
            1,
        );
        assert!(alias_switches(&[sonnet_5(), stray]).is_empty());
    }

    #[test]
    fn successive_switches_chain_in_order_without_skipping() {
        let c = seen(
            "sonnet",
            "claude-sonnet-6",
            "2026-10-05T00:00:00+00:00",
            "2026-10-06T00:00:00+00:00",
            4,
        );
        let switches = alias_switches(&[c, sonnet_5_5(), sonnet_5()]);
        let moves: Vec<(&str, &str)> = switches
            .iter()
            .map(|s| (s.from.as_str(), s.to.as_str()))
            .collect();
        assert_eq!(
            moves,
            vec![
                ("claude-sonnet-5", "claude-sonnet-5-5"),
                ("claude-sonnet-5-5", "claude-sonnet-6"),
            ]
        );
    }

    #[test]
    fn aliases_are_judged_apart() {
        let haiku = seen(
            "haiku",
            "claude-haiku-4-5",
            "2026-09-01T00:00:00+00:00",
            "2026-09-29T00:00:00+00:00",
            9,
        );
        let switches = alias_switches(&[haiku, sonnet_5(), sonnet_5_5()]);
        assert_eq!(switches.len(), 1);
        assert_eq!(switches[0].alias, "sonnet");
    }

    #[test]
    fn a_caveat_names_the_alias_and_each_side_s_models() {
        let caveats = model_caveats(
            &[pair("sonnet", "claude-sonnet-5")],
            &[pair("sonnet", "claude-sonnet-5-5")],
        );
        assert_eq!(caveats.len(), 1);
        assert_eq!(
            caveats[0].to_string(),
            "alias sonnet ran claude-sonnet-5 in incumbent runs and claude-sonnet-5-5 in \
             candidate runs"
        );
    }

    #[test]
    fn the_same_models_or_an_alias_on_one_side_only_are_no_caveat() {
        assert!(model_caveats(
            &[pair("sonnet", "claude-sonnet-5"), pair("haiku", "h")],
            &[pair("sonnet", "claude-sonnet-5"), pair("opus", "o")],
        )
        .is_empty());
    }
}
