//! The reviewer dispatch's value types.

use super::super::{
    ContextManifest, Duration, EffectiveAuthority, PathBuf, Recipe, RoutedBy, Tier, UsagePhase,
};

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

/// What is told a dispatch id: `Err` is the reason it could not be kept.
pub(in crate::runner) type OnDispatch<'a> = dyn Fn(&str) -> Result<(), String> + 'a;

/// Who a reviewer dispatch is seated as: the model, its tier, where it
/// reads, and what it is for.
pub(in crate::runner) struct ReviewSeat<'p> {
    pub(in crate::runner) profile: &'p crate::policy::ModelProfile,
    pub(in crate::runner) tier: Tier,
    pub(in crate::runner) dir: PathBuf,
    pub(in crate::runner) purpose: ReviewPurpose,
    /// The wall clock this dispatch gets; `None` is what the run has left,
    /// at least [`super::REVIEW_MIN_WALL`]. A part of a review in parts has
    /// its own, [`super::REVIEW_PART_TIMEOUT`].
    pub(in crate::runner) wall: Option<Duration>,
    /// Told the dispatch id once the dispatch is on record and before the
    /// reviewer runs; an `Err` ends the review `Unavailable`.
    pub(in crate::runner) on_dispatch: Option<&'p OnDispatch<'p>>,
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
    /// The run's base: adr-lint's candidates are the lines between it and
    /// `candidate_sha`.
    pub(crate) base_sha: &'r str,
}

impl ReviewRequest<'_> {
    /// The same request about another patch file: one part of the patch.
    pub(in crate::runner) fn for_patch(&self, patch_path: PathBuf) -> ReviewRequest<'_> {
        ReviewRequest {
            manifest: self.manifest,
            authority: self.authority,
            candidate_sha: self.candidate_sha,
            candidate_tier: self.candidate_tier,
            candidate_model: self.candidate_model,
            verification_inputs_changed: self.verification_inputs_changed,
            patch_path,
            routed_by: self.routed_by,
            covering: self.covering.clone(),
            base_sha: self.base_sha,
        }
    }
}

pub(crate) enum ReviewOutcome {
    NoFindings,
    Findings(String),
    Unavailable(String),
}

/// Why a review ended `Unavailable`, chosen from how the reviewer dispatch
/// ended. The retry decision reads this, never the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::runner) enum UnavailableCause {
    /// The reviewer ran and ended with no terminal result to read.
    TerminalMissing,
    /// The reviewer could not be launched, or the harness reported why it
    /// could not run.
    LaunchFailed,
    /// A cancellation request ended the reviewer.
    Cancelled,
    /// The reviewer ran out of wall clock, or a budget ended the review.
    ResourceExhausted,
    /// No model is configured to review.
    NoReviewerTier,
    /// The runner could not stage, record or read the review.
    NotRead,
}

impl UnavailableCause {
    pub(in crate::runner) fn as_str(self) -> &'static str {
        match self {
            Self::TerminalMissing => "terminal_missing",
            Self::LaunchFailed => "launch_failed",
            Self::Cancelled => "cancelled",
            Self::ResourceExhausted => "resource_exhausted",
            Self::NoReviewerTier => "no_reviewer_tier",
            Self::NotRead => "not_read",
        }
    }
}
