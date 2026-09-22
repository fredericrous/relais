//! Typed final-outcome feedback (SPEC §20).
//!
//! After acceptance, a candidate's later life — merged unchanged, needed
//! a correction, backed out, or a later confirmed regression — is
//! attributed to the candidate AND the strategy that actually produced
//! it, never floated as an unscoped opinion. Absence of feedback is not
//! a positive label; nothing here records that.

use serde::{Deserialize, Serialize};

use crate::policy::Tier;

/// What actually happened to an accepted candidate later (SPEC §20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    /// Merged and unchanged.
    AcceptedUnchanged,
    /// Needed a correction; carries a [`CorrectionMagnitude`].
    Corrected,
    /// Backed out.
    Reverted,
    /// A later user-reported regression was confirmed.
    ConfirmedRegression,
}

impl OutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AcceptedUnchanged => "accepted_unchanged",
            Self::Corrected => "corrected",
            Self::Reverted => "reverted",
            Self::ConfirmedRegression => "confirmed_regression",
        }
    }

    /// The inverse of [`OutcomeKind::as_str`]. `None` is a name this
    /// relais does not know — a newer relais's kind, or a hand-edited row.
    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "accepted_unchanged" => Some(Self::AcceptedUnchanged),
            "corrected" => Some(Self::Corrected),
            "reverted" => Some(Self::Reverted),
            "confirmed_regression" => Some(Self::ConfirmedRegression),
            _ => None,
        }
    }

    /// Whether an accepted change is still standing under this outcome —
    /// the primary "cost per accepted change" denominator is only
    /// meaningful for changes that were not later taken back. Only
    /// `reverted` withdraws the change itself; a correction or a
    /// confirmed regression are later facts about a change that is still
    /// in the tree.
    pub fn withdraws_acceptance(self) -> bool {
        match self {
            Self::Reverted => true,
            Self::AcceptedUnchanged | Self::Corrected | Self::ConfirmedRegression => false,
        }
    }
}

/// A correction's size, in the fraction of the accepted candidate's
/// content the correction touched — `0.0` excluded, since "corrected"
/// with nothing changed is "accepted unchanged" mislabeled.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
// Deserialized THROUGH the constructor: a derived impl would let a
// stored `0.0` or a negative back in as a valid typed value, so a row
// nobody could have written through `new` would read as one that was.
#[serde(try_from = "f64")]
pub struct CorrectionMagnitude(f64);

impl TryFrom<f64> for CorrectionMagnitude {
    type Error = MagnitudeError;

    fn try_from(value: f64) -> std::result::Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Why a magnitude value could not be recorded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MagnitudeError {
    /// Not a finite, positive number.
    NotPositive(f64),
}

impl std::fmt::Display for MagnitudeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPositive(value) => {
                write!(
                    f,
                    "a correction magnitude must be a positive number, got {value}"
                )
            }
        }
    }
}

impl std::error::Error for MagnitudeError {}

impl CorrectionMagnitude {
    pub fn new(value: f64) -> Result<Self, MagnitudeError> {
        if value.is_finite() && value > 0.0 {
            Ok(Self(value))
        } else {
            Err(MagnitudeError::NotPositive(value))
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

/// The strategy that actually produced a candidate — read off the
/// ledger's attempt and usage records, never asked of the user (SPEC
/// §20: feedback is attributed to the strategy, and only the ledger
/// knows what actually ran).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Strategy {
    /// The tier the run's first attempt was dispatched at.
    pub tier: Tier,
    /// Distinct models that actually ran, from usage events.
    pub models: Vec<String>,
    /// Whether the ladder moved to an escalation attempt.
    pub escalated: bool,
}

/// Everything an outcome carries beyond its kind: the exact candidate,
/// the strategy that produced it, an optional correction magnitude,
/// evidence references and who is recording it (SPEC §20).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutcomeDetail {
    pub candidate_sha: String,
    pub strategy: Strategy,
    pub correction_magnitude: Option<CorrectionMagnitude>,
    pub evidence: Vec<String>,
    pub actor: String,
}

/// A typed outcome: a kind together with the detail its kind requires.
/// Constructed only through [`Outcome::new`], so a stored row can never
/// carry a magnitude a `reverted` outcome refuses, or lack one a
/// `corrected` outcome requires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub kind: OutcomeKind,
    pub detail: OutcomeDetail,
}

/// Why an outcome could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeError {
    /// This kind requires a correction magnitude and none was given.
    MagnitudeRequired(OutcomeKind),
    /// This kind refuses a correction magnitude and one was given.
    MagnitudeRefused(OutcomeKind),
}

impl std::fmt::Display for OutcomeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MagnitudeRequired(kind) => write!(
                f,
                "outcome `{}` requires a correction magnitude",
                kind.as_str()
            ),
            Self::MagnitudeRefused(kind) => write!(
                f,
                "outcome `{}` refuses a correction magnitude",
                kind.as_str()
            ),
        }
    }
}

impl std::error::Error for OutcomeError {}

impl Outcome {
    pub fn new(kind: OutcomeKind, detail: OutcomeDetail) -> Result<Self, OutcomeError> {
        let has_magnitude = detail.correction_magnitude.is_some();
        match kind {
            OutcomeKind::Corrected if !has_magnitude => Err(OutcomeError::MagnitudeRequired(kind)),
            OutcomeKind::Corrected => Ok(Self { kind, detail }),
            OutcomeKind::AcceptedUnchanged
            | OutcomeKind::Reverted
            | OutcomeKind::ConfirmedRegression
                if has_magnitude =>
            {
                Err(OutcomeError::MagnitudeRefused(kind))
            }
            OutcomeKind::AcceptedUnchanged
            | OutcomeKind::Reverted
            | OutcomeKind::ConfirmedRegression => Ok(Self { kind, detail }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(magnitude: Option<CorrectionMagnitude>) -> OutcomeDetail {
        OutcomeDetail {
            candidate_sha: "abc123".into(),
            strategy: Strategy {
                tier: Tier::Implementation,
                models: vec!["sonnet".into()],
                escalated: false,
            },
            correction_magnitude: magnitude,
            evidence: vec![],
            actor: "a reviewer".into(),
        }
    }

    #[test]
    fn corrected_requires_a_magnitude() {
        assert_eq!(
            Outcome::new(OutcomeKind::Corrected, detail(None)),
            Err(OutcomeError::MagnitudeRequired(OutcomeKind::Corrected))
        );
        let magnitude = CorrectionMagnitude::new(0.2).expect("magnitude");
        assert!(Outcome::new(OutcomeKind::Corrected, detail(Some(magnitude))).is_ok());
    }

    #[test]
    fn accepted_unchanged_refuses_a_magnitude() {
        let magnitude = CorrectionMagnitude::new(0.2).expect("magnitude");
        assert_eq!(
            Outcome::new(OutcomeKind::AcceptedUnchanged, detail(Some(magnitude))),
            Err(OutcomeError::MagnitudeRefused(
                OutcomeKind::AcceptedUnchanged
            ))
        );
        assert!(Outcome::new(OutcomeKind::AcceptedUnchanged, detail(None)).is_ok());
    }

    #[test]
    fn reverted_and_confirmed_regression_also_refuse_a_magnitude() {
        let magnitude = CorrectionMagnitude::new(0.2).expect("magnitude");
        assert_eq!(
            Outcome::new(OutcomeKind::Reverted, detail(Some(magnitude))),
            Err(OutcomeError::MagnitudeRefused(OutcomeKind::Reverted))
        );
        assert_eq!(
            Outcome::new(OutcomeKind::ConfirmedRegression, detail(Some(magnitude))),
            Err(OutcomeError::MagnitudeRefused(
                OutcomeKind::ConfirmedRegression
            ))
        );
    }

    #[test]
    fn a_magnitude_must_be_positive_and_finite() {
        assert!(CorrectionMagnitude::new(0.0).is_err());
        assert!(CorrectionMagnitude::new(-1.0).is_err());
        assert!(CorrectionMagnitude::new(f64::NAN).is_err());
        assert!(CorrectionMagnitude::new(f64::INFINITY).is_err());
        assert!(CorrectionMagnitude::new(0.5).is_ok());
    }

    #[test]
    fn kind_round_trips_through_its_stored_string() {
        for kind in [
            OutcomeKind::AcceptedUnchanged,
            OutcomeKind::Corrected,
            OutcomeKind::Reverted,
            OutcomeKind::ConfirmedRegression,
        ] {
            assert_eq!(OutcomeKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(OutcomeKind::parse("not-a-kind"), None);
    }

    #[test]
    fn only_reverted_withdraws_acceptance() {
        assert!(OutcomeKind::Reverted.withdraws_acceptance());
        assert!(!OutcomeKind::AcceptedUnchanged.withdraws_acceptance());
        assert!(!OutcomeKind::Corrected.withdraws_acceptance());
        assert!(!OutcomeKind::ConfirmedRegression.withdraws_acceptance());
    }
}
