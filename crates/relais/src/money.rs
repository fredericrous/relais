//! Integer money (SPEC §11): recorded cost never accumulates in floats.
//!
//! Provider-reported dollar figures arrive as floats and are converted to
//! micro-USD exactly once, at the boundary; every sum, average and persisted
//! value is integer arithmetic. Missing usage is `Unknown`, never zero, and
//! cost kinds stay distinct because API spend, usage credits, estimated
//! API-equivalents and subscription consumption are not interchangeable.

use serde::{Deserialize, Serialize};

/// Micro-dollars in a signed 64-bit integer (~9.2 trillion dollars headroom).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MicroUsd(i64);

impl MicroUsd {
    pub const ZERO: Self = Self(0);

    pub fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Boundary conversion from a provider-reported dollar float. The float
    /// exists only here, rounded once, never accumulated.
    pub fn from_dollars(dollars: f64) -> Self {
        if !dollars.is_finite() {
            return Self::ZERO;
        }
        Self((dollars * 1_000_000.0).round() as i64)
    }

    pub fn to_micros(self) -> i64 {
        self.0
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

impl std::ops::Add for MicroUsd {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl std::ops::AddAssign for MicroUsd {
    fn add_assign(&mut self, rhs: Self) {
        self.0 = self.0.saturating_add(rhs.0);
    }
}

impl std::ops::Sub for MicroUsd {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

/// `$12.345678`, trailing zeros trimmed so a whole-dollar figure reads
/// `$12`, a cent figure `$0.12`.
impl std::fmt::Display for MicroUsd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let micros = self.0.unsigned_abs();
        let dollars = micros / 1_000_000;
        let frac = micros % 1_000_000;
        if frac == 0 {
            write!(f, "{sign}${dollars}")
        } else {
            let mut frac = format!("{frac:06}");
            while frac.ends_with('0') {
                frac.pop();
            }
            write!(f, "{sign}${dollars}.{frac}")
        }
    }
}

/// What a recorded cost figure is (SPEC §11: distinguish API spend,
/// usage-credit spend, estimated API-equivalent cost, subscription
/// consumption).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CostKind {
    ApiSpend,
    UsageCreditSpend,
    EstimatedApiEquivalent,
    SubscriptionConsumption,
}

/// How complete a cost figure is. An interrupted run's cost is an
/// incomplete lower bound, not an authoritative total (SPEC §11). The
/// default is `Unknown`: absent usage is unknown, never zero.
///
/// A total order from most to least complete, so the completeness of a
/// sum is `max` over its parts: one unknown makes the total unknown, one
/// lower bound makes it a lower bound. `fold(Actual, max)` replaces the
/// three hand-written worst-of matches this used to have.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum CostCompleteness {
    Actual,
    Estimated,
    IncompleteLowerBound,
    #[default]
    Unknown,
}

impl CostCompleteness {
    /// The completeness of a figure made of these parts.
    pub fn worst<I: IntoIterator<Item = CostCompleteness>>(parts: I) -> Self {
        parts.into_iter().fold(Self::Actual, Self::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_trims_trailing_zeros() {
        assert_eq!(MicroUsd::from_micros(12_000_000).to_string(), "$12");
        assert_eq!(MicroUsd::from_micros(120_000).to_string(), "$0.12");
        assert_eq!(MicroUsd::from_micros(1_234_567).to_string(), "$1.234567");
        assert_eq!(MicroUsd::from_micros(-120_000).to_string(), "-$0.12");
    }

    #[test]
    fn float_conversion_rounds_once_at_the_boundary() {
        assert_eq!(
            MicroUsd::from_dollars(0.012345),
            MicroUsd::from_micros(12_345)
        );
        assert_eq!(MicroUsd::from_dollars(f64::NAN), MicroUsd::ZERO);
        assert_eq!(MicroUsd::from_dollars(f64::INFINITY), MicroUsd::ZERO);
    }

    #[test]
    fn completeness_is_ordered_from_actual_to_unknown() {
        use CostCompleteness::*;
        assert!(
            Actual < Estimated
                && Estimated < IncompleteLowerBound
                && IncompleteLowerBound < Unknown
        );
        assert_eq!(CostCompleteness::worst([Actual, Actual]), Actual);
        assert_eq!(
            CostCompleteness::worst([Actual, Estimated, Actual]),
            Estimated
        );
        assert_eq!(
            CostCompleteness::worst([IncompleteLowerBound, Unknown]),
            Unknown
        );
        assert_eq!(
            CostCompleteness::worst(std::iter::empty()),
            Actual,
            "nothing is complete"
        );
    }

    #[test]
    fn sums_saturate_instead_of_overflowing() {
        let max = MicroUsd::from_micros(i64::MAX);
        assert_eq!(max + MicroUsd::from_micros(1), max);
    }
}
