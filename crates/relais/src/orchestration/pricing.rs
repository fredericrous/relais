//! Pricing an orchestration usage record from a machine-owned price
//! table (SPEC §11). Prices are never constants in this module: they
//! come from `[pricing]` in `~/.config/relais/machine.toml`, read and
//! parsed by the caller, so an Anthropic price change is a config edit,
//! not a code change.

use serde::{Deserialize, Serialize};

use super::transcript::{Speed, UsageRecord};
use crate::money::{CostCompleteness, MicroUsd};

/// One model's per-million-token rates, in integer micro-dollars.
/// `ids` names every model string that bills at this rate — a model is
/// commonly addressed both by its bare family name and by a dated
/// snapshot id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub ids: Vec<String>,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write_5m: i64,
    pub cache_write_1h: i64,
    #[serde(default)]
    pub fast_input: Option<i64>,
    #[serde(default)]
    pub fast_output: Option<i64>,
}

impl ModelPrice {
    fn matches(&self, model: &str) -> bool {
        self.ids.iter().any(|id| id == model)
    }
}

/// The `[pricing]` table: a version recorded on every priced row, and
/// the model rates. `version` is free text the machine owner controls —
/// it exists so a later reader can tell which price list a stored cost
/// came from, not to be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceTable {
    pub version: String,
    #[serde(default)]
    pub models: Vec<ModelPrice>,
}

impl PriceTable {
    /// An empty table: every record prices as `Unknown`, never zero.
    /// What a caller uses when machine.toml has no `[pricing]` block at
    /// all, rather than treating "no table configured" as an error.
    pub fn empty() -> Self {
        Self {
            version: String::new(),
            models: Vec::new(),
        }
    }

    fn rate_for(&self, model: &str) -> Option<&ModelPrice> {
        self.models.iter().find(|price| price.matches(model))
    }
}

/// The result of pricing one usage record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Priced {
    /// `None` = unknown, never zero (SPEC §11): a model absent from the
    /// table, or a fast-mode record whose table has no fast rate for
    /// this model.
    pub cost: Option<MicroUsd>,
    pub completeness: CostCompleteness,
    pub pricing_version: String,
}

/// `tokens * rate_per_million / 1_000_000`, saturating at the `i64`
/// extremes instead of overflowing — an `i128` intermediate makes the
/// multiplication itself exact for any realistic token count and rate,
/// so only the final narrowing needs to saturate.
fn micros_for(tokens: u64, rate_per_million_micros: i64) -> MicroUsd {
    let product = (tokens as i128) * (rate_per_million_micros as i128) / 1_000_000;
    MicroUsd::from_micros(product.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
}

/// Price one usage record. A model absent from the table, or a
/// non-standard speed the table has no fast rate for, prices as
/// `cost: None` with `completeness: Unknown` — a missing price is never
/// zero (SPEC §11).
pub fn price(record: &UsageRecord, table: &PriceTable) -> Priced {
    let pricing_version = table.version.clone();
    let unknown = || Priced {
        cost: None,
        completeness: CostCompleteness::Unknown,
        pricing_version: pricing_version.clone(),
    };
    let Some(rate) = table.rate_for(&record.model) else {
        return unknown();
    };
    let (input_rate, output_rate) = match &record.speed {
        Speed::Standard => (Some(rate.input), Some(rate.output)),
        Speed::Other(_) => match (rate.fast_input, rate.fast_output) {
            (Some(input), Some(output)) => (Some(input), Some(output)),
            _ => (None, None),
        },
    };
    let (Some(input_rate), Some(output_rate)) = (input_rate, output_rate) else {
        return unknown();
    };
    let cost = MicroUsd::ZERO
        .saturating_add(micros_for(record.input_tokens, input_rate))
        .saturating_add(micros_for(record.output_tokens, output_rate))
        .saturating_add(micros_for(record.cache_read_input_tokens, rate.cache_read))
        .saturating_add(micros_for(
            record.cache_writes.ephemeral_5m_input_tokens,
            rate.cache_write_5m,
        ))
        .saturating_add(micros_for(
            record.cache_writes.ephemeral_1h_input_tokens,
            rate.cache_write_1h,
        ));
    // Estimated, never Actual: this is tokens times a hand-maintained
    // price table, not a figure any bill reported. On a subscription it
    // is what the same usage would cost at API rates, not what was paid.
    Priced {
        cost: Some(cost),
        completeness: CostCompleteness::Estimated,
        pricing_version,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::transcript::CacheWrites;

    fn record(input: u64, output: u64, cache_read: u64, cache: CacheWrites) -> UsageRecord {
        UsageRecord {
            message_id: "msg_1".into(),
            model: "claude-haiku-4-5".into(),
            speed: Speed::Standard,
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_writes: cache,
            timestamp: "2026-09-28T00:00:00Z".into(),
        }
    }

    fn table() -> PriceTable {
        PriceTable {
            version: "2026-09-01".into(),
            models: vec![ModelPrice {
                ids: vec![
                    "claude-haiku-4-5".into(),
                    "claude-haiku-4-5-20251001".into(),
                ],
                input: 1_000_000,
                output: 5_000_000,
                cache_read: 100_000,
                cache_write_5m: 1_250_000,
                cache_write_1h: 2_000_000,
                fast_input: None,
                fast_output: None,
            }],
        }
    }

    #[test]
    fn a_model_absent_from_the_table_is_unknown_never_zero() {
        let mut rec = record(100, 0, 0, CacheWrites::default());
        rec.model = "claude-opus-5-5".into();
        let priced = price(&rec, &table());
        assert_eq!(priced.cost, None);
        assert_eq!(priced.completeness, CostCompleteness::Unknown);
    }

    #[test]
    fn a_second_model_id_maps_to_the_same_price() {
        let mut rec = record(1_000_000, 0, 0, CacheWrites::default());
        rec.model = "claude-haiku-4-5-20251001".into();
        let priced = price(&rec, &table());
        assert_eq!(priced.cost, Some(MicroUsd::from_micros(1_000_000)));
    }

    #[test]
    fn input_and_output_are_priced_at_their_own_rates() {
        let priced = price(
            &record(1_000_000, 1_000_000, 0, CacheWrites::default()),
            &table(),
        );
        assert_eq!(
            priced.cost,
            Some(MicroUsd::from_micros(1_000_000 + 5_000_000))
        );
        assert_eq!(priced.completeness, CostCompleteness::Estimated);
        assert_eq!(priced.pricing_version, "2026-09-01");
    }

    /// A 1-hour write must be priced at `cache_write_1h`, not
    /// `cache_write_5m` — the two differ (2x base input vs 1.25x), so
    /// swapping them silently would misprice every session, which is
    /// exactly what this session's own transcripts look like (44.7M
    /// `ephemeral_1h`, zero `ephemeral_5m`). Swap the rates used here to
    /// see this assertion fail, then restore it: that is the
    /// falsification the acceptance criteria calls for.
    #[test]
    fn a_1h_cache_write_is_priced_at_the_1h_rate_not_the_5m_rate() {
        let cache = CacheWrites {
            ephemeral_5m_input_tokens: 0,
            ephemeral_1h_input_tokens: 1_000_000,
        };
        let priced = price(&record(0, 0, 0, cache), &table());
        assert_eq!(priced.cost, Some(MicroUsd::from_micros(2_000_000)));
        assert_ne!(priced.cost, Some(MicroUsd::from_micros(1_250_000)));
    }

    #[test]
    fn a_5h_cache_write_is_priced_at_the_5m_rate() {
        let cache = CacheWrites {
            ephemeral_5m_input_tokens: 1_000_000,
            ephemeral_1h_input_tokens: 0,
        };
        let priced = price(&record(0, 0, 0, cache), &table());
        assert_eq!(priced.cost, Some(MicroUsd::from_micros(1_250_000)));
    }

    #[test]
    fn cache_read_is_priced_at_its_own_rate() {
        let priced = price(&record(0, 0, 1_000_000, CacheWrites::default()), &table());
        assert_eq!(priced.cost, Some(MicroUsd::from_micros(100_000)));
    }

    #[test]
    fn fast_speed_without_a_fast_rate_is_unknown() {
        let mut rec = record(1_000_000, 0, 0, CacheWrites::default());
        rec.speed = Speed::Other("priority".into());
        let priced = price(&rec, &table());
        assert_eq!(priced.cost, None);
        assert_eq!(priced.completeness, CostCompleteness::Unknown);
    }

    #[test]
    fn fast_speed_with_a_fast_rate_uses_it() {
        let mut t = table();
        t.models[0].fast_input = Some(2_000_000);
        t.models[0].fast_output = Some(10_000_000);
        let mut rec = record(1_000_000, 1_000_000, 0, CacheWrites::default());
        rec.speed = Speed::Other("priority".into());
        let priced = price(&rec, &t);
        assert_eq!(
            priced.cost,
            Some(MicroUsd::from_micros(2_000_000 + 10_000_000))
        );
    }

    #[test]
    fn arithmetic_saturates_instead_of_overflowing() {
        let mut t = table();
        t.models[0].input = i64::MAX;
        let priced = price(&record(u64::MAX, 0, 0, CacheWrites::default()), &t);
        assert_eq!(priced.cost, Some(MicroUsd::from_micros(i64::MAX)));
    }

    #[test]
    fn an_empty_table_prices_everything_unknown() {
        let priced = price(
            &record(1, 1, 1, CacheWrites::default()),
            &PriceTable::empty(),
        );
        assert_eq!(priced.cost, None);
        assert_eq!(priced.completeness, CostCompleteness::Unknown);
    }
}
