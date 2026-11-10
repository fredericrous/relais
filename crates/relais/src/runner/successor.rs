//! Planning a successor intent (docs/plans/2026-10-14-runs-survive-a-loaded-
//! machine.md, A5): what `decide revise --widen-scope` and `decide revise
//! --successor-contract` hand to the ledger's outbox.
//!
//! Everything here reads: the decided run's recorded contract and
//! transitions, and the candidate it left. Writing the answer and the intent
//! row is one ledger transaction (`Ledger::resolve_decision_with_intent`).

use std::path::Path;

use crate::contract::{ContractError, Review, TaskContract};
use crate::ids::RunId;
use crate::ledger::{IntentReview, Ledger, LedgerError, NewIntent};
use crate::lifecycle::{Reason, State};
use crate::policy::VerificationProfile;
use crate::verify::classify_verification_inputs;
use crate::workspace::is_protected_path;

use super::candidate::{self, Refusal, Resolution, Source};
use super::RunError;

/// Why a successor intent could not be planned.
#[derive(Debug)]
pub enum PlanError {
    Ledger(LedgerError),
    Run(RunError),
    Contract(ContractError),
    /// The decided run is not one `--widen-scope` answers.
    NotWidenable(String),
    /// `review-parts` does not answer this run, or its patch cannot be
    /// reviewed in parts.
    NotReviewable(String),
    /// The successor's candidate cannot be resolved now.
    Candidate {
        reason: Reason,
        detail: String,
    },
    /// The ledger does not hold what planning reads.
    Missing(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ledger(e) => write!(f, "ledger: {e}"),
            Self::Run(e) => write!(f, "{e}"),
            Self::Contract(e) => write!(f, "the successor contract is invalid: {e}"),
            Self::NotWidenable(detail) | Self::NotReviewable(detail) | Self::Missing(detail) => {
                write!(f, "{detail}")
            }
            Self::Candidate { reason, detail } => write!(
                f,
                "the successor's candidate cannot be pinned ({reason}): {detail}"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

impl From<LedgerError> for PlanError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}

/// The transition that raised `run`'s open decision. Retiring the worktree
/// records a same-state row after it; the raising row is the last one that
/// is not that.
fn raising_transition(
    ledger: &Ledger,
    run: &RunId,
) -> Result<crate::ledger::Transition, PlanError> {
    ledger
        .transitions(run)?
        .into_iter()
        .rev()
        .find(|t| {
            t.to_state == State::NeedsDecision && t.reason != Reason::WorktreeRetired.as_str()
        })
        .ok_or_else(|| PlanError::NotWidenable(format!("run {run} is not waiting on a decision")))
}

/// The transition that ended `run` interrupted: the last row into
/// `interrupted` that is not a worktree retirement.
fn interrupted_transition(
    ledger: &Ledger,
    run: &RunId,
) -> Result<crate::ledger::Transition, PlanError> {
    ledger
        .transitions(run)?
        .into_iter()
        .rev()
        .find(|t| t.to_state == State::Interrupted && t.reason != Reason::WorktreeRetired.as_str())
        .ok_or_else(|| PlanError::NotWidenable(format!("run {run} did not end interrupted")))
}

/// The candidate sha a timed-out worker's `process_crash` transition
/// recorded in its JSON detail: `None` unless the run was interrupted by a
/// wall-time stop that left a candidate.
pub fn timed_out_candidate(ledger: &Ledger, run: &RunId) -> Result<Option<String>, PlanError> {
    let raised = interrupted_transition(ledger, run)?;
    if raised.reason != Reason::ProcessCrash.as_str() {
        return Ok(None);
    }
    let Some(detail) = raised.detail else {
        return Ok(None);
    };
    if detail.get("timed_out").and_then(|flag| flag.as_bool()) != Some(true) {
        return Ok(None);
    }
    Ok(detail
        .get("candidate")
        .and_then(|sha| sha.as_str())
        .filter(|sha| !sha.is_empty())
        .map(str::to_string))
}

/// The successor a timed-out run is answered with when relais settles it
/// itself: `Ok(None)` unless the run ended interrupted on a wall-time stop
/// that left a candidate, else the intent verifying that candidate with the
/// scope unchanged.
pub fn plan_timed_out_verify(
    ledger: &Ledger,
    run: &RunId,
    contract: &TaskContract,
    runs_dir: &Path,
) -> Result<Option<NewIntent>, PlanError> {
    if timed_out_candidate(ledger, run)?.is_none() {
        return Ok(None);
    }
    let verify = settled_contract(contract, run, &[]).map_err(PlanError::Contract)?;
    plan(ledger, run, &verify, Vec::new(), runs_dir).map(Some)
}

/// `--rerun-verify` answers a `resource_exhausted` decision only: a rerun of
/// the same candidate is the answer to a machine that was out of room, not
/// to a code failure or a scope question (plan B4).
pub fn require_resource_exhausted(ledger: &Ledger, run: &RunId) -> Result<(), PlanError> {
    let waiting = raising_transition(ledger, run)?;
    if waiting.reason != Reason::ResourceExhausted.as_str() {
        return Err(PlanError::NotWidenable(format!(
            "`--rerun-verify` answers a resource_exhausted decision, and run {run} waits on {}",
            waiting.reason
        )));
    }
    Ok(())
}

/// The out-of-scope paths a `scope_exceeded` decision of `run` lists,
/// exactly as its transition recorded them.
pub fn out_of_scope_paths(ledger: &Ledger, run: &RunId) -> Result<Vec<String>, PlanError> {
    let waiting = raising_transition(ledger, run)?;
    if waiting.reason != Reason::ScopeExceeded.as_str() {
        return Err(PlanError::NotWidenable(format!(
            "`--widen-scope` answers a scope_exceeded decision, and run {run} waits on {}",
            waiting.reason
        )));
    }
    let paths: Vec<String> = waiting
        .detail
        .as_ref()
        .and_then(|detail| detail.get("paths"))
        .and_then(|paths| paths.as_array())
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if paths.is_empty() {
        return Err(PlanError::Missing(format!(
            "run {run}'s scope_exceeded decision lists no paths, so there is nothing to widen to"
        )));
    }
    Ok(paths)
}

/// Whether `path` is a test-tree path (the globs of
/// [`crate::verify::TEST_TREE_INPUTS`]) that is neither one of the profile's
/// own verification inputs nor under a protected prefix. A pattern that
/// cannot be compiled makes no path a test.
fn is_test_path(profile: &VerificationProfile, path: &str) -> bool {
    let paths = [path.to_string()];
    if is_protected_path(path) {
        return false;
    }
    match classify_verification_inputs(profile, &paths) {
        Ok(touched) => touched.policy.is_empty() && touched.tests.len() == 1,
        Err(_) => false,
    }
}

/// How much one small non-test edit may change, from `[execution]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmallScopeLimits {
    pub max_files: u32,
    pub max_lines: u32,
}

/// One changed path of a scope overflow, as the raising transition
/// recorded it: `None` counts are a binary file or an unreadable count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeChange {
    pub path: String,
    pub added: Option<u64>,
    pub removed: Option<u64>,
}

/// Which rule let relais widen a scope overflow itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidenRule {
    /// Every path is a test path.
    Tests,
    /// Test paths plus at most the allowed number of small non-test edits.
    Small(Vec<ScopeChange>),
}

/// Codes a relais settle ends its run with.
pub const CODE_WIDENED_FOR_TESTS: &str = "scope_widened_for_tests";
pub const CODE_WIDENED_SMALL: &str = "scope_widened_small";
pub const CODE_TIMED_OUT_VERIFY_QUEUED: &str = "timed_out_verify_queued";

/// The note the answered decision of a timed-out run carries; it starts
/// with the code.
pub const TIMED_OUT_NOTE: &str = "timed_out_verify_queued: worker hit its wall time; relais \
     verifies its tree (execution.verify_timed_out)";

impl WidenRule {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Tests => CODE_WIDENED_FOR_TESTS,
            Self::Small(_) => CODE_WIDENED_SMALL,
        }
    }

    /// The note the answered decision carries; it starts with the code.
    pub fn note(&self) -> String {
        match self {
            Self::Tests => format!(
                "{CODE_WIDENED_FOR_TESTS}: scope widened for test paths \
                 (execution.widen_scope_for_tests)"
            ),
            Self::Small(_) => format!(
                "{CODE_WIDENED_SMALL}: scope widened by a small edit \
                 (execution.widen_scope_small_max_files)"
            ),
        }
    }
}

/// What a relais settle hands out: the code the run ends with, the sentence
/// that says why, and the successor the plugin or the CLI launches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub code: &'static str,
    pub sentence: String,
    pub successor: crate::protocol::SuccessorRef,
}

/// The code a relais answer's note starts with, if it names a known one.
pub fn settle_code_of_note(note: &str) -> Option<&'static str> {
    let (code, _) = note.split_once(": ")?;
    [
        CODE_WIDENED_FOR_TESTS,
        CODE_WIDENED_SMALL,
        CODE_TIMED_OUT_VERIFY_QUEUED,
    ]
    .into_iter()
    .find(|known| *known == code)
}

/// Whether a scope overflow is one relais may widen itself, and by which
/// rule: no path protected, none a policy verification input, and every
/// path a test path except at most `limits.max_files` non-test paths whose
/// added plus removed lines sum to at most `limits.max_lines`. A change with
/// an unknown count is not small.
pub fn widenable(
    profile: &VerificationProfile,
    changes: &[ScopeChange],
    limits: SmallScopeLimits,
) -> Option<WidenRule> {
    if changes.is_empty() || changes.iter().any(|change| is_protected_path(&change.path)) {
        return None;
    }
    let paths: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();
    match classify_verification_inputs(profile, &paths) {
        Ok(touched) if touched.policy.is_empty() => {}
        _ => return None,
    }
    let sources: Vec<&ScopeChange> = changes
        .iter()
        .filter(|change| !is_test_path(profile, &change.path))
        .collect();
    if sources.is_empty() {
        return Some(WidenRule::Tests);
    }
    if sources.len() as u64 > u64::from(limits.max_files) {
        return None;
    }
    let mut lines: u64 = 0;
    for change in &sources {
        lines = lines.checked_add(change.added?.checked_add(change.removed?)?)?;
    }
    (lines <= u64::from(limits.max_lines))
        .then(|| WidenRule::Small(sources.into_iter().cloned().collect()))
}

/// The changed paths a `scope_exceeded` decision of `run` lists, with the
/// counts its transition recorded beside `paths`. A transition from before
/// the counts were recorded yields paths with unknown counts.
pub fn out_of_scope_changes(ledger: &Ledger, run: &RunId) -> Result<Vec<ScopeChange>, PlanError> {
    let paths = out_of_scope_paths(ledger, run)?;
    let waiting = raising_transition(ledger, run)?;
    let recorded = waiting
        .detail
        .as_ref()
        .and_then(|detail| detail.get("changes"))
        .and_then(|changes| changes.as_array());
    Ok(paths
        .into_iter()
        .map(|path| {
            let entry = recorded.and_then(|changes| {
                changes
                    .iter()
                    .find(|change| change.get("path").and_then(|p| p.as_str()) == Some(&path))
            });
            let count = |key: &str| entry.and_then(|e| e.get(key)).and_then(|n| n.as_u64());
            ScopeChange {
                added: count("added"),
                removed: count("removed"),
                path,
            }
        })
        .collect())
}

/// The successor `scope_exceeded` is answered with when relais settles it
/// itself: `Ok(None)` unless [`widenable`] says yes, else the intent
/// widening the scope by exactly the out-of-scope paths, the paths, and the
/// rule that fired.
pub fn plan_scope_widening(
    ledger: &Ledger,
    run: &RunId,
    contract: &TaskContract,
    profile: &VerificationProfile,
    limits: SmallScopeLimits,
    runs_dir: &Path,
) -> Result<Option<(NewIntent, Vec<String>, WidenRule)>, PlanError> {
    let changes = out_of_scope_changes(ledger, run)?;
    let Some(rule) = widenable(profile, &changes, limits) else {
        return Ok(None);
    };
    let paths: Vec<String> = changes.into_iter().map(|change| change.path).collect();
    let widened = settled_contract(contract, run, &paths).map_err(PlanError::Contract)?;
    let intent = plan(ledger, run, &widened, paths.clone(), runs_dir)?;
    Ok(Some((intent, paths, rule)))
}

/// The contract every automatic successor runs: [`widened_contract`] with
/// its review raised to at least `Required`, keeping a stronger one, so a
/// source edit relais let through is never accepted unreviewed.
pub fn settled_contract(
    contract: &TaskContract,
    run: &RunId,
    paths: &[String],
) -> Result<TaskContract, ContractError> {
    let widened = widened_contract(contract, run, paths)?;
    if widened.review >= Review::Required {
        return Ok(widened);
    }
    let mut value = widened.canonical_value();
    if let Some(fields) = value.as_object_mut() {
        fields.insert("review".into(), serde_json::json!(Review::Required));
    }
    TaskContract::from_json_str(&value.to_string())
}

/// Whether `run` is itself the successor of a decision relais answered: a
/// settle is never settled again.
pub fn is_relais_successor(ledger: &Ledger, run: &RunId) -> Result<bool, PlanError> {
    let Some(intent) = ledger.intent_of_run(run)? else {
        return Ok(false);
    };
    Ok(ledger
        .decision_of_run(&intent.predecessor)?
        .and_then(|decision| decision.actor)
        .is_some_and(|actor| actor == crate::ledger::Actor::Relais.as_str()))
}

/// The contract `--widen-scope` builds: the decided run's own, as a
/// `kind: verify` of that run's candidate, whose write scope is the old one
/// plus exactly `paths`. A verify has no worker and no decomposition, so
/// those fields do not carry over.
// holds-until: no overlay filter here; the holdout overlay collision
// (runner/attempt.rs, runner/scheduler.rs) ends a run before any scope can
// be widened. Ends if phase 5 of the holdout plan counts a
// holdout_exposures that bypassed the collision (ADR-0045).
pub fn widened_contract(
    contract: &TaskContract,
    run: &RunId,
    paths: &[String],
) -> Result<TaskContract, ContractError> {
    let mut value = contract.canonical_value();
    let Some(fields) = value.as_object_mut() else {
        return Err(ContractError::MalformedJson(
            "a contract is a JSON object".into(),
        ));
    };
    let mut scope: Vec<String> = fields
        .get("write_scope")
        .and_then(|scope| scope.as_array())
        .map(|scope| {
            scope
                .iter()
                .filter_map(|pattern| pattern.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for path in paths {
        if !scope.contains(path) {
            scope.push(path.clone());
        }
    }
    fields.insert("kind".into(), "verify".into());
    fields.insert("write_scope".into(), serde_json::json!(scope));
    fields.insert(
        "candidate".into(),
        serde_json::json!({ "run": run.as_str() }),
    );
    fields.remove("from_candidate");
    fields.remove("decomposition");
    TaskContract::from_json_str(&value.to_string())
}

/// The outbox row for `contract` as the successor of `run`'s decision: its
/// digest, its JSON, the scope it added, and its candidate resolved and
/// pinned against the decided run's repository and base.
pub fn plan(
    ledger: &Ledger,
    run: &RunId,
    contract: &TaskContract,
    scope_delta: Vec<String>,
    runs_dir: &Path,
) -> Result<NewIntent, PlanError> {
    let candidate = match contract.source_candidate() {
        None => None,
        Some(wanted) => {
            let workspace = ledger.run_workspace(run)?.ok_or_else(|| {
                PlanError::Missing(format!("run {run} has no workspace on record"))
            })?;
            let base = workspace.base_sha.ok_or_else(|| {
                PlanError::Missing(format!("run {run} recorded no base revision"))
            })?;
            let repo_key = ledger
                .run_repo_key(run)?
                .ok_or_else(|| PlanError::Missing(format!("run {run} has no repository")))?;
            let source = Source {
                repo_dir: &workspace.repo_path,
                artifacts_dir: runs_dir,
            };
            match candidate::resolve(ledger, &source, &repo_key, &base, wanted)
                .map_err(PlanError::Run)?
            {
                Resolution::Resolved(resolved) => Some(resolved),
                Resolution::Refused(Refusal { reason, detail }) => {
                    return Err(PlanError::Candidate { reason, detail })
                }
            }
        }
    };
    Ok(NewIntent {
        predecessor: run.clone(),
        contract_digest: contract.hash(),
        contract_json: serde_json::to_string(&contract.canonical_value())
            .expect("a validated contract serializes"),
        scope_delta,
        candidate,
        review: None,
    })
}

/// The outbox row for a `review-parts` answer to `run`'s decision: a
/// `kind: verify` of the run's own candidate, `review: parts`. Refused, with
/// nothing planned, when the run did not stop for a review it could not have
/// or when its patch cannot be cut into parts that each fit the budget (a
/// single file over it, or more than the most parts a review may take).
pub fn plan_review_parts(
    ledger: &Ledger,
    run: &RunId,
    runs_dir: &Path,
) -> Result<NewIntent, PlanError> {
    let raised = ledger
        .decision_of_run(run)?
        .map(|decision| decision.raised_reason)
        .unwrap_or_default();
    if !super::approval::seals_from_snapshot(&raised) {
        return Err(PlanError::NotReviewable(format!(
            "`review-parts` answers a review_over_budget or review_unavailable stop, and run \
             {run} waits on {}",
            if raised.is_empty() {
                "nothing"
            } else {
                &raised
            }
        )));
    }
    let (original, _tier) = ledger
        .run_contract_and_tier(run)?
        .ok_or_else(|| PlanError::Missing(format!("run {run} carries no recorded contract")))?;
    let contract = widened_contract(&original, run, &[]).map_err(PlanError::Contract)?;
    let mut intent = plan(ledger, run, &contract, Vec::new(), runs_dir)?;
    let resolved = intent.candidate.as_ref().ok_or_else(|| {
        PlanError::Missing(format!("run {run}'s candidate could not be resolved"))
    })?;
    let patch_path = runs_dir
        .join(&resolved.source_run)
        .join(format!("candidate-{}.patch", resolved.index));
    let patch = std::fs::read(&patch_path).map_err(|e| {
        PlanError::Missing(format!(
            "the candidate patch {} could not be read: {e}",
            patch_path.display()
        ))
    })?;
    super::review::split_into_parts(&patch, super::review::REVIEW_PATCH_BUDGET_BYTES)
        .map_err(|refusal| PlanError::NotReviewable(refusal.to_string()))?;
    intent.review = Some(IntentReview::Parts);
    Ok(intent)
}
