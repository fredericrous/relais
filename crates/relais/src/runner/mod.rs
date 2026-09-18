//! Attempt lifecycle (SPEC §9, §19).
//!
//! States: prepared, running, verifying, repairing, escalating, accepted,
//! needs_review, needs_decision, blocked, failed, budget_exhausted,
//! cancelled, interrupted. Every transition carries a reason code,
//! timestamp and evidence references. Workers can propose completion or
//! blockage; only the runner assigns final state. Escalation uses a fresh
//! context that labels previous model claims unverified, never changes
//! acceptance criteria, and default ceilings are at most three attempts:
//! initial, one repair, one stronger attempt. A bounded planner may
//! propose work packages under explicit aggregate limits.

use serde::{Deserialize, Serialize};

/// The attempt lifecycle states (SPEC §9). Stored as their snake_case
/// names in the ledger; only the runner assigns terminal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Prepared,
    Running,
    Verifying,
    Repairing,
    Escalating,
    Accepted,
    NeedsReview,
    NeedsDecision,
    Blocked,
    Failed,
    BudgetExhausted,
    Cancelled,
    Interrupted,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Prepared => "prepared",
            State::Running => "running",
            State::Verifying => "verifying",
            State::Repairing => "repairing",
            State::Escalating => "escalating",
            State::Accepted => "accepted",
            State::NeedsReview => "needs_review",
            State::NeedsDecision => "needs_decision",
            State::Blocked => "blocked",
            State::Failed => "failed",
            State::BudgetExhausted => "budget_exhausted",
            State::Cancelled => "cancelled",
            State::Interrupted => "interrupted",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "prepared" => State::Prepared,
            "running" => State::Running,
            "verifying" => State::Verifying,
            "repairing" => State::Repairing,
            "escalating" => State::Escalating,
            "accepted" => State::Accepted,
            "needs_review" => State::NeedsReview,
            "needs_decision" => State::NeedsDecision,
            "blocked" => State::Blocked,
            "failed" => State::Failed,
            "budget_exhausted" => State::BudgetExhausted,
            "cancelled" => State::Cancelled,
            "interrupted" => State::Interrupted,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            State::Accepted
                | State::NeedsReview
                | State::NeedsDecision
                | State::Blocked
                | State::Failed
                | State::BudgetExhausted
                | State::Cancelled
                | State::Interrupted
        )
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Reason codes recorded on every transition (SPEC §9's observation
/// table). Stable strings so `explain` and reports stay comparable
/// across versions.
pub mod reason {
    pub const CHECKS_AND_REVIEW_PASSED: &str = "checks_and_review_passed";
    pub const BEHAVIORAL_FAILURE: &str = "behavioral_failure";
    pub const REPAIR_EXHAUSTED: &str = "repair_exhausted";
    pub const AMBIGUOUS_DIAGNOSIS: &str = "ambiguous_diagnosis";
    pub const SAME_FAILURE_RECURRENCE: &str = "same_failure_recurrence";
    pub const ENV_MISSING: &str = "env_missing";
    pub const ARCHITECTURE_UNRESOLVED: &str = "architecture_unresolved";
    pub const SCOPE_EXCEEDED: &str = "scope_exceeded";
    pub const REVIEW_UNAVAILABLE: &str = "review_unavailable";
    pub const LIMIT_REACHED: &str = "limit_reached";
    pub const PROCESS_CRASH: &str = "process_crash";
    pub const CANCELLED_BY_USER: &str = "cancelled_by_user";
    pub const RECONCILED_INTERRUPTED: &str = "reconciled_interrupted";
    pub const BLOCKED_PREFLIGHT: &str = "blocked_preflight";
    pub const ESCALATION_NOT_AUTHORIZED: &str = "escalation_not_authorized";
    pub const MODEL_UNAVAILABLE: &str = "model_unavailable";
    pub const UNAPPROVED_SUBSTITUTION: &str = "unapproved_substitution";
    pub const ARCHITECTURE_CONTRADICTION: &str = "architecture_contradiction";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_round_trip_through_snake_case() {
        for state in [
            State::Prepared,
            State::Running,
            State::Verifying,
            State::Repairing,
            State::Escalating,
            State::Accepted,
            State::NeedsReview,
            State::NeedsDecision,
            State::Blocked,
            State::Failed,
            State::BudgetExhausted,
            State::Cancelled,
            State::Interrupted,
        ] {
            assert_eq!(State::parse(state.as_str()), Some(state));
        }
        assert_eq!(State::parse("nope"), None);
    }

    #[test]
    fn terminal_states() {
        assert!(State::Accepted.is_terminal());
        assert!(State::Interrupted.is_terminal());
        assert!(!State::Running.is_terminal());
        assert!(!State::Verifying.is_terminal());
    }
}
