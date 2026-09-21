//! The lifecycle vocabulary (SPEC §9): the states a run passes through
//! and the reasons it moves between them.
//!
//! A leaf module on purpose. `State` and `Reason` are the names the
//! ledger stores, the report prints and the learning dataset labels
//! with, so every one of those modules needs them — and none of them
//! has any business depending on the runner that drives the lifecycle.
//! They live here, depend on nothing, and `runner` re-exports them so
//! its own callers read as before.

use serde::{Deserialize, Serialize};

/// Why a transition happened. Stored by name in the ledger, so the
/// spelling is the wire format and `parse` is its inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    ChecksAndReviewPassed,
    BehavioralFailure,
    RepairExhausted,
    AmbiguousDiagnosis,
    SameFailureRecurrence,
    EnvMissing,
    ArchitectureUnresolved,
    ScopeExceeded,
    ReviewUnavailable,
    LimitReached,
    ProcessCrash,
    CancelledByUser,
    ReconciledInterrupted,
    BlockedPreflight,
    EscalationNotAuthorized,
    ModelUnavailable,
    UnapprovedSubstitution,
    ArchitectureContradiction,
    VerificationGap,
    BaselineFailureNotWaived,
    ReviewFindings,
    VerificationInputsChanged,
    AdmissionUnavailable,
    PlanAccepted,
    PlanRejected,
    PackageStarted,
    PackageFinished,
    IntegrationConflict,
    IntegrationFailed,
    AdmissionRefused,
    DuplicateDispatch,
    /// The harness refused the worker a tool it needed (SPEC §8).
    PermissionDenied,
    /// The candidate carries the base tree: verification is the
    /// baseline's, not re-run.
    CandidateIdenticalToBase,
    /// Policy configures no tier but the candidate's own, so the review
    /// was not an independent opinion (SPEC §10).
    ReviewerSameTier,
    /// An accepted run's task worktree could not be released; its
    /// content is in the patch and the candidate ref regardless.
    WorktreeNotReleased,
    /// Verification had to wait for a worktree's writer to relinquish
    /// its lease before snapshotting (SPEC §23).
    WriteLeaseWait,
    /// A writer still held the worktree when the run's clock ran out:
    /// nothing was snapshotted, the tree is preserved.
    WriteLeaseHeld,
    /// The run ended because the runner itself could not go on — a
    /// ledger or filesystem failure — not because of anything a worker
    /// did (SPEC §12: uncertain state is interrupted, never retried).
    RunnerFailure,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChecksAndReviewPassed => "checks_and_review_passed",
            Self::BehavioralFailure => "behavioral_failure",
            Self::RepairExhausted => "repair_exhausted",
            Self::AmbiguousDiagnosis => "ambiguous_diagnosis",
            Self::SameFailureRecurrence => "same_failure_recurrence",
            Self::EnvMissing => "env_missing",
            Self::ArchitectureUnresolved => "architecture_unresolved",
            Self::ScopeExceeded => "scope_exceeded",
            Self::ReviewUnavailable => "review_unavailable",
            Self::LimitReached => "limit_reached",
            Self::ProcessCrash => "process_crash",
            Self::CancelledByUser => "cancelled_by_user",
            Self::ReconciledInterrupted => "reconciled_interrupted",
            Self::BlockedPreflight => "blocked_preflight",
            Self::EscalationNotAuthorized => "escalation_not_authorized",
            Self::ModelUnavailable => "model_unavailable",
            Self::UnapprovedSubstitution => "unapproved_substitution",
            Self::ArchitectureContradiction => "architecture_contradiction",
            Self::VerificationGap => "verification_gap",
            Self::BaselineFailureNotWaived => "baseline_failure_not_waived",
            Self::ReviewFindings => "review_findings",
            Self::VerificationInputsChanged => "verification_inputs_changed",
            Self::AdmissionUnavailable => "admission_unavailable",
            Self::PlanAccepted => "plan_accepted",
            Self::PlanRejected => "plan_rejected",
            Self::PackageStarted => "package_started",
            Self::PackageFinished => "package_finished",
            Self::IntegrationConflict => "integration_conflict",
            Self::IntegrationFailed => "integration_failed",
            Self::AdmissionRefused => "admission_refused",
            Self::DuplicateDispatch => "duplicate_dispatch",
            Self::PermissionDenied => "permission_denied",
            Self::CandidateIdenticalToBase => "candidate_identical_to_base",
            Self::ReviewerSameTier => "reviewer_same_tier",
            Self::WorktreeNotReleased => "worktree_not_released",
            Self::WriteLeaseWait => "write_lease_wait",
            Self::WriteLeaseHeld => "write_lease_held",
            Self::RunnerFailure => "runner_failure",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(text.to_string())).ok()
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The lifecycle states (SPEC §9). Stored as their snake_case names in
/// the ledger; only the runner assigns terminal states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
            Self::Prepared => "prepared",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Repairing => "repairing",
            Self::Escalating => "escalating",
            Self::Accepted => "accepted",
            Self::NeedsReview => "needs_review",
            Self::NeedsDecision => "needs_decision",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::BudgetExhausted => "budget_exhausted",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(text.to_string())).ok()
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted
                | Self::NeedsReview
                | Self::NeedsDecision
                | Self::Blocked
                | Self::Failed
                | Self::BudgetExhausted
                | Self::Cancelled
                | Self::Interrupted
        )
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_and_states_round_trip_through_their_names() {
        for reason in [
            Reason::ChecksAndReviewPassed,
            Reason::SameFailureRecurrence,
            Reason::VerificationInputsChanged,
            Reason::RunnerFailure,
        ] {
            assert_eq!(Reason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(Reason::parse("nope"), None);
        for state in [State::Prepared, State::NeedsDecision, State::Interrupted] {
            assert_eq!(State::parse(state.as_str()), Some(state));
        }
        assert!(State::Accepted.is_terminal() && !State::Verifying.is_terminal());
    }
}
