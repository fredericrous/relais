//! The data a run passes between its phases, and the small helpers that
//! only build them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{AttemptKind, Budget, Launched, RunOutcome};
use crate::backend::{LaunchResult, LaunchSpec};
use crate::context::ContextManifest;
use crate::ledger::UsageEvent;
use crate::lifecycle::UsagePhase;
use crate::money::MicroUsd;
use crate::policy::{EffectiveAuthority, Tier};
use crate::protocol::{CostFigure, Event, TokenUsage};
use crate::route::Route;
use crate::verify::{self};
use crate::workspace::{self, TaskWorktree, WorkspaceError};

/// What a child run IS to its parent: an ordinary work package, or the
/// repair of an assembled candidate that failed verification. The
/// difference is not cosmetic — an integration repair's spend is
/// integration spend, and typing it `initial` (which is what its
/// attempts are, considered alone) hides it inside the packages' own
/// first attempts and leaves the `integration` bucket always empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageRole {
    Work,
    IntegrationRepair,
}

impl PackageRole {
    /// The phase an attempt of this child records. A work package's
    /// attempts say which attempt they are; every attempt of an
    /// integration repair is integration.
    pub(super) fn phase(self, kind: AttemptKind) -> UsagePhase {
        match self {
            Self::Work => UsagePhase::from(kind),
            Self::IntegrationRepair => UsagePhase::Integration,
        }
    }
}

/// One dispatch as admission and the launch need it: the spec, its place
/// in the agent tree, what it may spend and how long it has.
pub(crate) struct ManagedDispatch<'d> {
    pub(crate) spec: LaunchSpec,
    /// The attempt this dispatch is, for a worker; reviews and plans have
    /// none.
    pub(crate) attempt: Option<u32>,
    /// Depth in the agent tree; a root dispatch is 0 (SPEC §23).
    pub(crate) depth: u32,
    /// The dispatch that asked for this one, when one did.
    pub(crate) parent: Option<&'d str>,
    /// What the coordinator reserves for it.
    pub(crate) reserve_micros: i64,
    /// The budget an admission refusal is judged against.
    pub(crate) budget: &'d Budget,
    /// The worktree this dispatch writes, when it writes one. Readers
    /// pass `None` and take no lease.
    pub(crate) write_lease: Option<&'d Path>,
}

/// An in-place run's hold on the caller's worktree.
pub(super) struct InPlaceRun {
    /// Taken at the end, when the tree is finalized.
    pub(super) lease: Option<workspace::inplace::Lease>,
    pub(super) start: workspace::inplace::Start,
}

/// What one phase of a run hands the next — or the end it reached
/// instead. Every phase can finish the run (a blocked preflight, an
/// unusable baseline, a cancelled attempt), and saying so in the type
/// is what lets each phase be a function of its own.
pub(crate) enum Phase<T> {
    Ready(T),
    Ended(RunOutcome),
}

/// What snapshotting an attempt's tree produced.
pub(super) enum Snapshot {
    /// The candidate, recorded and scope-checked.
    Ready(Candidate),
    /// The run ended.
    Ended(RunOutcome),
    /// A path the candidate's merge conflicted in is still unresolved: the
    /// attempt failed (plan A4), and the machine decides what follows.
    Unresolved { attempt_id: i64, paths: Vec<String> },
}

/// What the attempt loop does next.
pub(super) enum Step {
    /// Dispatch another attempt.
    Again,
    /// The run ended.
    Ended(RunOutcome),
}

/// What preflight established: fixed for the whole run, and the same for
/// every attempt of it.
pub(super) struct Preflight {
    pub(super) authority: EffectiveAuthority,
    /// Optional integrations that are not installed; they ride in the
    /// receipt and are never passes (SPEC §5).
    pub(super) integration_gaps: Vec<String>,
    pub(super) base_sha: String,
    pub(super) contract_hash: String,
    /// The contract revision row every attempt of this run belongs to.
    pub(super) revision_id: i64,
    pub(super) manifest: ContextManifest,
    pub(super) decision: Route,
    /// The candidate a `kind: verify` run applies, or a `kind: change` run
    /// continues from, resolved and merge-checked in preflight; `None`
    /// for every other run.
    pub(super) candidate: Option<PreparedCandidate>,
    /// The intent this run was launched from (plan A5), when it was: what
    /// the receipt names as its predecessor.
    pub(super) predecessor: Option<crate::contract::Predecessor>,
}

/// A resolved candidate and the tree its merge into the target base
/// produced: the task tree is checked out from exactly this tree.
pub(super) struct PreparedCandidate {
    pub(super) resolved: crate::contract::ResolvedCandidate,
    pub(super) tree: String,
    /// Set when the merge conflicted and the run continues from it (plan
    /// A4): the tree holds the 3-way result for the worker to resolve.
    pub(super) conflicts: Option<CandidateConflicts>,
}

/// A `from_candidate` merge that conflicted.
pub(super) struct CandidateConflicts {
    pub(super) files: Vec<String>,
    /// Input for `git update-index -z --index-info`.
    pub(super) index_info: Vec<u8>,
}

impl PreparedCandidate {
    /// Make `worktree` this candidate's tree: the clean merge result, or
    /// the 3-way result with its unmerged index entries.
    pub(super) fn check_out(&self, worktree: &TaskWorktree) -> Result<(), WorkspaceError> {
        match &self.conflicts {
            Some(conflicts) => worktree.check_out_conflicted(&self.tree, &conflicts.index_info),
            None => worktree.check_out_tree(&self.tree),
        }
    }
}

impl Preflight {
    /// The paths a `from_candidate` merge conflicted in; empty otherwise.
    pub(super) fn conflicted_paths(&self) -> &[String] {
        self.candidate
            .as_ref()
            .and_then(|prepared| prepared.conflicts.as_ref())
            .map_or(&[], |conflicts| conflicts.files.as_slice())
    }
}

/// What the base revision's own verification established (SPEC §10).
pub(super) struct Baseline {
    /// Check labels that already fail at the base.
    pub(super) failures: Vec<String>,
    /// The base's outcomes when the commands actually ran: a candidate
    /// carrying the base tree has exactly these, without spending them
    /// again. `None` when the verdict came from the cache.
    pub(super) checks: Option<Vec<verify::CheckOutcome>>,
    /// The verdict came from the baseline cache (SPEC §18).
    pub(super) cached: bool,
    /// Why the baseline could not be cached, when it could not (V2).
    pub(super) cache_refused: Option<verify::CacheRefused>,
    /// Where this run's check logs are written.
    pub(super) logs_dir: PathBuf,
}

/// Everything the attempt loop reads and never changes.
pub(super) struct AttemptContext<'c> {
    pub(super) preflight: &'c Preflight,
    pub(super) baseline: &'c Baseline,
    /// The run's one owned worktree, at the base revision.
    pub(super) worktree: &'c TaskWorktree,
    pub(super) worktree_path: &'c Path,
}

/// The attempt row a candidate is snapshotted for.
pub(super) struct Attempted {
    pub(super) attempt_id: i64,
    pub(super) index: u32,
    pub(super) tier: Tier,
    pub(super) model: String,
}

/// One attempt that reached a worker and came back.
pub(super) struct Dispatched {
    pub(super) attempt_id: i64,
    pub(super) index: u32,
    pub(super) tier: Tier,
    /// The model the attempt was dispatched with: the rung's, which a
    /// recipe may have set to any allowed model.
    pub(super) model: String,
    pub(super) result: LaunchResult,
}

/// Split a candidate's gaps into the acceptance-evidence ones, which
/// keep stopping the run, and those about the profile's own checks.
pub(super) fn split_profile_gaps(
    gaps: Vec<String>,
    acceptance_gaps: &[verify::AcceptanceGap],
) -> (Vec<String>, Vec<String>) {
    let evidence: Vec<String> = acceptance_gaps
        .iter()
        .map(verify::AcceptanceGap::message)
        .collect();
    gaps.into_iter().partition(|gap| evidence.contains(gap))
}

/// What a candidate stopped only by sign-offs has established, for the
/// pending receipt that keeps it.
pub(super) struct PendingEvidence {
    pub(super) checks: Vec<verify::CheckOutcome>,
    pub(super) gaps: Vec<String>,
    pub(super) gaps_not_judged: Vec<String>,
    pub(super) notes: Vec<String>,
    pub(super) gate_coverage: BTreeMap<String, Result<bool, verify::AttestError>>,
    /// An inspection's report review, which the pending receipt keeps.
    pub(super) verdict: Option<verify::ReportVerdict>,
}

/// An attempt's candidate: snapshotted, named, exported and in scope.
pub(super) struct Candidate {
    pub(super) attempt_id: i64,
    pub(super) index: u32,
    /// The tier that wrote it.
    pub(super) tier: Tier,
    /// The model that wrote it, as dispatched — which the reviewer must
    /// not be.
    pub(super) model: String,
    pub(super) sha: String,
    /// The copy a reviewer's prompt names.
    pub(super) latest_patch: PathBuf,
    /// What the worker answered: an inspection's deliverable.
    pub(super) result_text: Option<String>,
}

/// What the profile's checks established about a candidate that passed
/// them, and whether a semantic review is still owed.
pub(super) struct Verified {
    pub(super) checks: Vec<verify::CheckOutcome>,
    pub(super) gaps: Vec<String>,
    pub(super) gaps_not_judged: Vec<String>,
    /// How the run was judged, when that is worth saying; not a gap.
    pub(super) notes: Vec<String>,
    pub(super) amont_bypasses: Vec<String>,
    pub(super) amont_downgrades: Vec<String>,
    pub(super) verification_inputs_changed: Vec<String>,
    pub(super) review_required: bool,
    pub(super) gate_coverage: BTreeMap<String, Result<bool, verify::AttestError>>,
}

/// How one verification's artifacts are named: its throwaway worktree
/// and the stem of every check log it writes. A single-worker run
/// labels by attempt; a decomposed run labels the assembled candidate,
/// which used to be `900 + repairs` — a number with no meaning at the
/// place it was read (audit style note).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptLabel {
    /// The nth attempt of a single-worker run.
    Attempt(u32),
    /// The assembled candidate of a decomposed run, after n integration
    /// repairs.
    Integration(u32),
}

impl AttemptLabel {
    /// The suffix of the throwaway verification worktree.
    pub(super) fn worktree_suffix(self) -> String {
        match self {
            Self::Attempt(index) => index.to_string(),
            Self::Integration(repairs) => format!("integration-{repairs}"),
        }
    }

    /// The stem every check log of this verification is named after.
    pub(super) fn log_prefix(self) -> String {
        match self {
            Self::Attempt(index) => format!("attempt{index}"),
            Self::Integration(repairs) => format!("integration{repairs}"),
        }
    }
}

/// How a managed dispatch ended, as the run's event: what the agent
/// reported when it ran, or the state the run ended in when the dispatch
/// never got that far.
pub(super) fn dispatch_ended(dispatch: String, launched: &Launched) -> Event {
    match launched {
        Ok(result) => Event::DispatchEnded {
            dispatch,
            agent: result.session_id.clone(),
            outcome: result.ended.describe(),
            usage: Some(TokenUsage {
                input_tokens: result.usage.input_tokens,
                output_tokens: result.usage.output_tokens,
                cache_read_tokens: result.usage.cache_read_tokens,
                cache_write_tokens: result.usage.cache_write_tokens,
            }),
            cost: Some(CostFigure {
                booked: result.usage.cost.micros().map(MicroUsd::to_micros),
                completeness: result.usage.cost.completeness(),
            }),
        },
        Err(outcome) => Event::DispatchEnded {
            dispatch,
            agent: None,
            outcome: outcome.state().as_str().to_string(),
            usage: None,
            cost: None,
        },
    }
}

/// What a usage event just recorded booked, for the run's events.
pub(super) fn cost_booked(event: &UsageEvent, unpriced: &[String]) -> Event {
    Event::Cost {
        booked: event.cost.map(MicroUsd::to_micros),
        completeness: event.completeness,
        unpriced_reason: (!unpriced.is_empty()).then(|| unpriced.join("; ")),
    }
}

/// The models a native attempt's booked usage could not price, if any.
/// Named record by record where they were priced, never inferred from the
/// total: a fast-mode record of a priced model, or an earlier record of
/// another model, is said as what it is.
pub(super) fn unpriced_native_model(result: &LaunchResult) -> Option<Vec<String>> {
    (!result.unpriced.is_empty()).then(|| result.unpriced.clone())
}

/// What a run records about its harness from the probe: the turn ceiling
/// for the manifest, and the warning to emit when the probe timed out
/// (`timed_out`), leaving the version unchecked rather than unsupported.
pub(super) fn harness_reading(
    capabilities: Option<&crate::backend::Capabilities>,
    timed_out: Option<String>,
) -> (crate::backend::TurnCeiling, Option<Event>) {
    match (capabilities, timed_out) {
        (Some(capabilities), _) => (capabilities.turn_ceiling(), None),
        (None, Some(detail)) => (
            crate::backend::TurnCeiling::UnknownTimedOut,
            Some(Event::EnvironmentWarning {
                what: "claude_code_version_unchecked".to_string(),
                detail,
            }),
        ),
        (None, None) => (crate::backend::TurnCeiling::default(), None),
    }
}
