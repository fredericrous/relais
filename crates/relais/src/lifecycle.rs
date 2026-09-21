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

/// A stored name that is not one of this binary's variants: a value a
/// newer relais wrote, a truncated write, a hand edit. Parsing a stored
/// name is fallible, so callers say what they do about it — the ledger
/// turns it into `LedgerError::Corrupt` rather than reading it as "no
/// such run".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownVariant {
    /// What was being read: `run state`, `transition reason`.
    pub what: &'static str,
    /// The name as it was stored.
    pub found: String,
}

impl std::fmt::Display for UnknownVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` is not a {} this relais knows",
            self.found, self.what
        )
    }
}

impl std::error::Error for UnknownVariant {}

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
    /// A dispatch of this run could not give back the write lease it
    /// took: the coordinator refused or was unreachable twice. The lease
    /// is this run's own, so verification does not wait on it.
    WriteLeaseNotReleased,
    /// Verification of a candidate started: the tree is snapshotted, in
    /// scope, and the profile's checks are about to run (SPEC §10).
    VerificationStarted,
    /// The coordinator stopped answering heartbeats while a worker ran.
    /// Cancellation travels on the heartbeat, so a run that cannot hear
    /// it is no longer supervised (SPEC §23).
    CoordinatorUnreachable,
    /// A writer still held the worktree when the run's clock ran out:
    /// nothing was snapshotted, the tree is preserved.
    WriteLeaseHeld,
    /// The run ended because the runner itself could not go on — a
    /// ledger or filesystem failure — not because of anything a worker
    /// did (SPEC §12: uncertain state is interrupted, never retried).
    RunnerFailure,
}

impl Reason {
    /// Every variant, for a caller that has to enumerate them — the
    /// round-trip property test, and anything rendering a legend.
    ///
    /// The length is fixed, so a variant added to the enum without being
    /// added here does not compile the `match` that walks it.
    pub const ALL: [Self; 41] = [
        Self::ChecksAndReviewPassed,
        Self::BehavioralFailure,
        Self::RepairExhausted,
        Self::AmbiguousDiagnosis,
        Self::SameFailureRecurrence,
        Self::EnvMissing,
        Self::ArchitectureUnresolved,
        Self::ScopeExceeded,
        Self::ReviewUnavailable,
        Self::LimitReached,
        Self::ProcessCrash,
        Self::CancelledByUser,
        Self::ReconciledInterrupted,
        Self::BlockedPreflight,
        Self::EscalationNotAuthorized,
        Self::ModelUnavailable,
        Self::UnapprovedSubstitution,
        Self::ArchitectureContradiction,
        Self::VerificationGap,
        Self::BaselineFailureNotWaived,
        Self::ReviewFindings,
        Self::VerificationInputsChanged,
        Self::AdmissionUnavailable,
        Self::PlanAccepted,
        Self::PlanRejected,
        Self::PackageStarted,
        Self::PackageFinished,
        Self::IntegrationConflict,
        Self::IntegrationFailed,
        Self::AdmissionRefused,
        Self::DuplicateDispatch,
        Self::PermissionDenied,
        Self::CandidateIdenticalToBase,
        Self::ReviewerSameTier,
        Self::WorktreeNotReleased,
        Self::WriteLeaseWait,
        Self::WriteLeaseNotReleased,
        Self::VerificationStarted,
        Self::CoordinatorUnreachable,
        Self::WriteLeaseHeld,
        Self::RunnerFailure,
    ];

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
            Self::WriteLeaseNotReleased => "write_lease_not_released",
            Self::VerificationStarted => "verification_started",
            Self::CoordinatorUnreachable => "coordinator_unreachable",
            Self::WriteLeaseHeld => "write_lease_held",
            Self::RunnerFailure => "runner_failure",
        }
    }

    /// The inverse of [`Reason::as_str`]. A name this binary does not
    /// know is an error the caller must decide about, never a silently
    /// dropped reason.
    pub fn parse(text: &str) -> Result<Self, UnknownVariant> {
        serde_json::from_value(serde_json::Value::String(text.to_string())).map_err(|_| {
            UnknownVariant {
                what: "transition reason",
                found: text.to_string(),
            }
        })
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
    /// Every variant, for a caller that has to enumerate them — the
    /// round-trip property test, and anything rendering a legend.
    ///
    /// The length is fixed, so a variant added to the enum without being
    /// added here does not compile the `match` that walks it.
    pub const ALL: [Self; 13] = [
        Self::Prepared,
        Self::Running,
        Self::Verifying,
        Self::Repairing,
        Self::Escalating,
        Self::Accepted,
        Self::NeedsReview,
        Self::NeedsDecision,
        Self::Blocked,
        Self::Failed,
        Self::BudgetExhausted,
        Self::Cancelled,
        Self::Interrupted,
    ];

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

    /// The inverse of [`State::as_str`]. A stored name this binary does
    /// not know is a corrupt row, and the ledger reports it as one.
    pub fn parse(text: &str) -> Result<Self, UnknownVariant> {
        serde_json::from_value(serde_json::Value::String(text.to_string())).map_err(|_| {
            UnknownVariant {
                what: "run state",
                found: text.to_string(),
            }
        })
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
            assert_eq!(Reason::parse(reason.as_str()), Ok(reason));
        }
        for state in [State::Prepared, State::NeedsDecision, State::Interrupted] {
            assert_eq!(State::parse(state.as_str()), Ok(state));
        }
        assert!(State::Accepted.is_terminal() && !State::Verifying.is_terminal());
    }

    /// X5: a name this binary does not know is an error that names what
    /// was read and what was found — not a `None` a caller can mistake
    /// for "nothing recorded".
    #[test]
    fn an_unknown_name_is_a_typed_error_naming_it() {
        let error = State::parse("hibernating").expect_err("not a known state");
        assert_eq!(error.what, "run state");
        assert_eq!(error.found, "hibernating");
        assert!(error.to_string().contains("hibernating"), "{error}");
        let error = Reason::parse("nope").expect_err("not a known reason");
        assert_eq!(error.what, "transition reason");
    }

    proptest::proptest! {
        /// Every state and every reason survives the round trip through
        /// the ledger: `as_str` writes the row, `parse` reads it back,
        /// and a variant that lost its spelling would come back as a
        /// corrupt row on a ledger this binary wrote itself.
        #[test]
        fn every_state_round_trips_through_its_stored_spelling(index in 0usize..State::ALL.len()) {
            use proptest::prelude::*;
            let state = State::ALL[index];
            prop_assert_eq!(State::parse(state.as_str()).expect("its own spelling"), state);
        }

        #[test]
        fn every_reason_round_trips_through_its_stored_spelling(index in 0usize..Reason::ALL.len()) {
            use proptest::prelude::*;
            let reason = Reason::ALL[index];
            prop_assert_eq!(Reason::parse(reason.as_str()).expect("its own spelling"), reason);
        }

        /// …and a spelling no variant has is refused, never guessed into
        /// the nearest one.
        #[test]
        fn an_unknown_spelling_is_refused(text in "[a-z_]{0,24}") {
            use proptest::prelude::*;
            let known_state = State::ALL.iter().any(|state| state.as_str() == text);
            prop_assert_eq!(State::parse(&text).is_ok(), known_state, "{}", text);
            let known_reason = Reason::ALL.iter().any(|reason| reason.as_str() == text);
            prop_assert_eq!(Reason::parse(&text).is_ok(), known_reason, "{}", text);
        }
    }
}
