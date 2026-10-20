//! The reviewer dispatch's value types.

use super::super::*;

/// What a reviewer dispatch is for. A report review reads the worktree and
/// nothing else; it is not the patch review, and the ledger does not count
/// it as one: its intent kind is `report_review`, so `review_dispatched`
/// is false, and its usage is tagged `UsagePhase::ReportReview`, so
/// `review_attempted` is false too.
#[derive(Debug, Clone, Copy)]
pub(in crate::runner) enum ReviewPurpose {
    Patch,
    Report,
}

/// Who a reviewer dispatch is seated as: the model, its tier, where it
/// reads, and what it is for.
pub(in crate::runner) struct ReviewSeat<'p> {
    pub(in crate::runner) profile: &'p crate::policy::ModelProfile,
    pub(in crate::runner) tier: Tier,
    pub(in crate::runner) dir: PathBuf,
    pub(in crate::runner) purpose: ReviewPurpose,
}

impl ReviewPurpose {
    pub(in crate::runner) fn usage_phase(self) -> UsagePhase {
        match self {
            Self::Patch => UsagePhase::Review,
            Self::Report => UsagePhase::ReportReview,
        }
    }

    pub(in crate::runner) fn intent_kind(self) -> &'static str {
        match self {
            Self::Patch => "review",
            Self::Report => "report_review",
        }
    }
}

/// What a reviewer is asked about one candidate.
pub(crate) struct ReviewRequest<'r> {
    pub(crate) manifest: &'r ContextManifest,
    pub(crate) authority: &'r EffectiveAuthority,
    pub(crate) candidate_sha: &'r str,
    /// The tier that WROTE the candidate: the reviewer is a different
    /// one wherever policy configures one (SPEC §10).
    pub(crate) candidate_tier: Tier,
    /// The model the candidate was dispatched with. Independence is
    /// decided by it, not by the tier: a recipe can put any allowed
    /// model on the worker's tier.
    pub(crate) candidate_model: &'r str,
    /// Test-tree paths the candidate changes, which the reviewer is
    /// asked to judge rather than waive.
    pub(crate) verification_inputs_changed: &'r [String],
    /// The patch the prompt tells the reviewer to read. The caller owns
    /// it, because the single-worker and the decomposed path export
    /// different files and the reviewer used to be sent to the one the
    /// decomposed path never writes (R4).
    pub(crate) patch_path: PathBuf,
    /// How the candidate's own dispatch was routed (SPEC §6) — the
    /// review is accounted to the same run, so it carries the same
    /// routing fact rather than a value the reviewer's own dispatch
    /// never actually decided.
    pub(crate) routed_by: RoutedBy,
    /// The recipe that covered the candidate's own dispatch, if any —
    /// carried the same way as `routed_by`, for the same reason.
    pub(crate) covering: Option<Recipe>,
}

pub(crate) enum ReviewOutcome {
    NoFindings,
    Findings(String),
    Unavailable(String),
}
