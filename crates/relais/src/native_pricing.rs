//! Whether a `--native` run can price what it dispatches (SPEC §11, §23).
//!
//! A native attempt's cost is estimated from its transcript at the machine's
//! prices. A model with no price has an unknown cost, which settles as the
//! attempt's whole reservation — the run's budget — and the next attempt is
//! refused for it. So a native run is refused before anything is dispatched
//! when a model it can dispatch is known not to be priced, and ends blocked
//! rather than on a budget refusal when one turns out not to be.

use std::path::Path;

use crate::ledger::ModelObservation;
use crate::orchestration::PriceTable;

/// Where a model's current rates are published.
const RATES_URL: &str = "https://platform.claude.com/docs/en/about-claude/pricing";

/// What the price table and the ledger say about one configured model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// An entry matches the alias as written, or the model it last ran as.
    Priced,
    /// Never observed and not priced as written: the alias's resolution is
    /// not known yet, so only the runtime check can tell.
    Unobserved,
    /// The model the alias last ran as has no entry (or there is no table).
    Unpriced { effective: String },
}

/// The effective model the ledger last observed for `alias`: the pair with
/// the latest `last_seen`.
fn latest_effective<'a>(alias: &str, observations: &'a [ModelObservation]) -> Option<&'a str> {
    observations
        .iter()
        .filter(|observation| observation.requested == alias)
        .max_by(|a, b| a.last_seen.cmp(&b.last_seen))
        .map(|observation| observation.effective.as_str())
}

/// The verdict on one alias. Pure: the table and the observations are
/// arguments.
pub fn verdict(alias: &str, prices: &PriceTable, observations: &[ModelObservation]) -> Verdict {
    let latest = latest_effective(alias, observations);
    if prices.models.is_empty() {
        return Verdict::Unpriced {
            effective: latest.unwrap_or(alias).to_string(),
        };
    }
    if prices.prices(alias) {
        return Verdict::Priced;
    }
    match latest {
        Some(effective) if prices.prices(effective) => Verdict::Priced,
        Some(effective) => Verdict::Unpriced {
            effective: effective.to_string(),
        },
        None => Verdict::Unobserved,
    }
}

/// What the check on a run's models decided.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preflight {
    /// One `native_unpriced` message per unpriced model: the run is refused.
    pub refusals: Vec<String>,
    /// Aliases that go on under the runtime check alone.
    pub warnings: Vec<String>,
}

/// Judge every alias a native run can dispatch. `machine_toml` is named in
/// the remedy.
pub fn preflight(
    aliases: &[String],
    prices: &PriceTable,
    observations: &[ModelObservation],
    machine_toml: &Path,
) -> Preflight {
    let mut decided = Preflight::default();
    for alias in aliases {
        match verdict(alias, prices, observations) {
            Verdict::Priced => {}
            Verdict::Unobserved => decided.warnings.push(format!(
                "relais run: warning: {alias} has no observed resolution yet, so it may not be \
                 priced; the runtime check applies"
            )),
            Verdict::Unpriced { effective } => decided.refusals.push(format!(
                "native_unpriced: {alias} runs as {effective}, which has no [pricing.models] \
                 entry in {}; add one (rates: {RATES_URL})",
                machine_toml.display()
            )),
        }
    }
    decided
}

/// Why a run ends after an attempt booked records relais could not price:
/// each reason as `orchestration::unpriced_reason` said it (no entry, or no
/// fast-mode rate), never inferred from the attempt's total.
pub fn backstop_detail(reasons: &[String], machine_toml: &Path) -> String {
    format!(
        "native_unpriced: {}; add the missing rates to {} (rates: {RATES_URL}). The attempt's \
         cost is unknown, which settles as its whole reservation, so no further attempt is \
         dispatched",
        reasons.join("; "),
        machine_toml.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::ModelPrice;
    use std::path::PathBuf;

    fn table(ids: &[&str]) -> PriceTable {
        PriceTable {
            version: "v".into(),
            models: vec![ModelPrice {
                ids: ids.iter().map(|id| id.to_string()).collect(),
                input: 1,
                output: 1,
                cache_read: 1,
                cache_write_5m: 1,
                cache_write_1h: 1,
                fast_input: None,
                fast_output: None,
            }],
        }
    }

    fn seen(requested: &str, effective: &str, last_seen: &str) -> ModelObservation {
        ModelObservation {
            requested: requested.into(),
            effective: effective.into(),
            first_seen: "2026-01-01T00:00:00Z".into(),
            last_seen: last_seen.into(),
            count: 1,
        }
    }

    fn machine() -> PathBuf {
        PathBuf::from("/home/me/machine.toml")
    }

    #[test]
    fn an_alias_priced_as_written_passes() {
        assert_eq!(verdict("sonnet", &table(&["sonnet"]), &[]), Verdict::Priced);
    }

    #[test]
    fn an_alias_whose_latest_observation_is_priced_passes() {
        let observations = [seen("sonnet", "claude-sonnet-5", "2026-10-02T00:00:00Z")];
        assert_eq!(
            verdict("sonnet", &table(&["claude-sonnet-5"]), &observations),
            Verdict::Priced
        );
    }

    #[test]
    fn an_unpriced_latest_observation_refuses_even_after_a_priced_older_one() {
        let observations = [
            seen("sonnet", "claude-sonnet-5", "2026-10-01T00:00:00Z"),
            seen("sonnet", "claude-sonnet-5-5", "2026-10-02T00:00:00Z"),
        ];
        let table = table(&["claude-sonnet-5"]);
        assert_eq!(
            verdict("sonnet", &table, &observations),
            Verdict::Unpriced {
                effective: "claude-sonnet-5-5".into()
            }
        );
        let decided = preflight(&["sonnet".into()], &table, &observations, &machine());
        assert_eq!(
            decided.refusals,
            [
                "native_unpriced: sonnet runs as claude-sonnet-5-5, which has no \
                 [pricing.models] entry in /home/me/machine.toml; add one \
                 (rates: https://platform.claude.com/docs/en/about-claude/pricing)"
            ]
        );
        assert!(decided.warnings.is_empty());
    }

    #[test]
    fn the_order_the_observations_come_in_does_not_pick_the_latest() {
        let observations = [
            seen("sonnet", "claude-sonnet-5-5", "2026-10-02T00:00:00Z"),
            seen("sonnet", "claude-sonnet-5", "2026-10-01T00:00:00Z"),
        ];
        assert_eq!(
            verdict("sonnet", &table(&["claude-sonnet-5"]), &observations),
            Verdict::Unpriced {
                effective: "claude-sonnet-5-5".into()
            }
        );
    }

    #[test]
    fn an_alias_never_observed_and_unpriced_as_written_passes_with_a_warning() {
        let decided = preflight(&["opus".into()], &table(&["haiku"]), &[], &machine());
        assert!(decided.refusals.is_empty());
        assert_eq!(decided.warnings.len(), 1);
        assert!(decided.warnings[0].contains("opus has no observed resolution yet"));
    }

    #[test]
    fn no_price_table_refuses_every_alias() {
        let observations = [seen("sonnet", "claude-sonnet-5", "2026-10-01T00:00:00Z")];
        let decided = preflight(
            &["sonnet".into(), "opus".into()],
            &PriceTable::empty(),
            &observations,
            &machine(),
        );
        assert_eq!(decided.refusals.len(), 2);
        assert!(decided.refusals[0].starts_with("native_unpriced: sonnet runs as claude-sonnet-5,"));
        assert!(decided.refusals[1].starts_with("native_unpriced: opus runs as opus,"));
        assert!(decided.warnings.is_empty());
    }

    #[test]
    fn every_unpriced_alias_is_named() {
        let observations = [
            seen("sonnet", "claude-sonnet-5-5", "2026-10-01T00:00:00Z"),
            seen("opus", "claude-opus-5-5", "2026-10-01T00:00:00Z"),
            seen("haiku", "claude-haiku-4-5", "2026-10-01T00:00:00Z"),
        ];
        let aliases: Vec<String> = ["haiku", "sonnet", "opus"].map(String::from).to_vec();
        let decided = preflight(
            &aliases,
            &table(&["claude-haiku-4-5"]),
            &observations,
            &machine(),
        );
        assert_eq!(decided.refusals.len(), 2);
        assert!(decided.refusals[0].contains("sonnet runs as claude-sonnet-5-5"));
        assert!(decided.refusals[1].contains("opus runs as claude-opus-5-5"));
    }

    #[test]
    fn the_backstop_names_the_model_and_the_remedy() {
        let detail = backstop_detail(
            &["claude-sonnet-5-5 has no [pricing.models] entry".to_string()],
            &machine(),
        );
        assert!(detail.starts_with("native_unpriced: claude-sonnet-5-5 has no [pricing.models]"));
        assert!(detail.contains("/home/me/machine.toml"));
        assert!(detail.contains("platform.claude.com/docs/en/about-claude/pricing"));
    }
}
