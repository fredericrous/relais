//! A routed task's outcome (plan §5) and how one outranks another when a
//! task record is sent again. Pure.

use serde::{Deserialize, Serialize};

/// How a task ended, from evidence only. Inferred signals (an abort, no
/// complaint, a repeated delegation) are never an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    /// The last verification in the task passed with no failure after
    /// it, or a relais run in it ended accepted.
    CompletedVerified,
    /// The person said so ("looks good", "ship it").
    CompletedAccepted,
    /// The person corrected the work, or flagged it.
    Corrected,
    /// Nothing was edited and nothing could have been: a question, a
    /// review. Never a learning label.
    Answered,
    Unknown,
}

impl TaskOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CompletedVerified => "completed_verified",
            Self::CompletedAccepted => "completed_accepted",
            Self::Corrected => "corrected",
            Self::Answered => "answered",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "completed_verified" => Self::CompletedVerified,
            "completed_accepted" => Self::CompletedAccepted,
            "corrected" => Self::Corrected,
            "answered" => Self::Answered,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// The wire contract's rank: `corrected` > `completed_verified` =
    /// `completed_accepted` > `unknown`. A resent task record replaces
    /// the stored outcome only when its rank is strictly higher, so a
    /// correction is never downgraded and `unknown` never overwrites
    /// anything.
    pub fn rank(self) -> u8 {
        match self {
            Self::Unknown | Self::Answered => 0,
            Self::CompletedVerified | Self::CompletedAccepted => 1,
            Self::Corrected => 2,
        }
    }

    /// The denominator of "cost per completed task".
    pub fn is_completed(self) -> bool {
        matches!(self, Self::CompletedVerified | Self::CompletedAccepted)
    }

    /// An outcome learning may count (plan §5): every
    /// one but `unknown` and `answered`.
    pub fn is_strong(self) -> bool {
        !matches!(self, Self::Unknown | Self::Answered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [TaskOutcome; 5] = [
        TaskOutcome::CompletedVerified,
        TaskOutcome::CompletedAccepted,
        TaskOutcome::Corrected,
        TaskOutcome::Answered,
        TaskOutcome::Unknown,
    ];

    #[test]
    fn corrected_outranks_everything_and_unknown_nothing() {
        for outcome in ALL {
            assert!(TaskOutcome::Corrected.rank() >= outcome.rank());
            assert!(TaskOutcome::Unknown.rank() <= outcome.rank());
        }
        assert_eq!(
            TaskOutcome::CompletedVerified.rank(),
            TaskOutcome::CompletedAccepted.rank()
        );
        assert!(TaskOutcome::Corrected.rank() > TaskOutcome::CompletedVerified.rank());
        assert!(TaskOutcome::CompletedAccepted.rank() > TaskOutcome::Unknown.rank());
    }

    #[test]
    fn answered_is_neither_completed_nor_strong() {
        assert_eq!(TaskOutcome::Answered.rank(), 0);
        assert!(!TaskOutcome::Answered.is_completed());
        assert!(!TaskOutcome::Answered.is_strong());
        assert!(!TaskOutcome::Unknown.is_strong());
        assert!(TaskOutcome::Corrected.is_strong());
        assert_eq!(ALL.len(), 5);
    }

    #[test]
    fn every_rank_zero_outcome_is_covered_by_the_ledgers_rank_zero_merge() {
        for outcome in ALL {
            assert_eq!(
                crate::ledger::router::RankZeroOutcome::parse(outcome.as_str()).is_some(),
                outcome.rank() == 0,
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn names_round_trip() {
        for outcome in ALL {
            assert_eq!(TaskOutcome::parse(outcome.as_str()), Some(outcome));
        }
        assert_eq!(TaskOutcome::parse("aborted"), None);
    }
}
