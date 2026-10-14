//! The candidate a `kind: verify` run applies (docs/plans/2026-10-14-runs-
//! survive-a-loaded-machine.md, A1 and A2).
//!
//! The contract names a run (and optionally an attempt); relais resolves
//! it ONCE to a [`ResolvedCandidate`], persists that, and from then on the
//! run applies the commit object the ledger recorded for the attempt, not
//! the best-effort ref and not the derived `.patch` file. The patch file
//! is cross-checked against the digest recorded at resolution, so a file
//! changed afterwards ends the run `candidate_digest_mismatch` instead of
//! verifying something other than what was resolved.

use std::path::Path;

use crate::contract::{CandidateRef, ResolvedCandidate};
use crate::ids::{sha256_hex, RunId};
use crate::ledger::Ledger;
use crate::lifecycle::Reason;
use crate::workspace;

use super::RunError;

/// A verify run that cannot start, or cannot go on, because of the
/// candidate it names. It ends `blocked` under `reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) reason: Reason,
    pub(crate) detail: String,
}

impl Refusal {
    fn new(reason: Reason, detail: String) -> Self {
        Self { reason, detail }
    }
}

/// What resolving a candidate ended in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Resolution {
    Resolved(ResolvedCandidate),
    Refused(Refusal),
}

/// Where the run being verified keeps its artifacts.
pub(crate) struct Source<'a> {
    pub(crate) repo_dir: &'a Path,
    pub(crate) artifacts_dir: &'a Path,
}

fn patch_file(source: &Source<'_>, run: &str, name: &str) -> std::path::PathBuf {
    source.artifacts_dir.join(run).join(name)
}

/// Resolve `wanted` against the ledger and the object store, once, for a
/// run whose repository is `this_repo_key` and whose base is `target_base`.
pub(crate) fn resolve(
    ledger: &Ledger,
    source: &Source<'_>,
    this_repo_key: &str,
    target_base: &str,
    wanted: &CandidateRef,
) -> Result<Resolution, RunError> {
    let run = RunId::from_stored(wanted.run.as_str());
    let refuse = |reason, detail| Ok(Resolution::Refused(Refusal::new(reason, detail)));
    match ledger.run_status(&run)? {
        None => {
            return refuse(
                Reason::CandidateMissing,
                format!(
                    "run {} is not on record, so it has no candidate",
                    wanted.run
                ),
            )
        }
        Some(state) if !state.is_terminal() => {
            return refuse(
                Reason::CandidateSourceRunning,
                format!(
                    "run {} is still {state}: what it will leave is not known yet; verify it \
                     once it has ended",
                    wanted.run
                ),
            )
        }
        Some(_) => {}
    }
    match ledger.run_repo_key(&run)? {
        Some(key) if key == this_repo_key => {}
        Some(_) => {
            return refuse(
                Reason::CandidateForeignRepo,
                format!(
                    "run {} belongs to another repository; a candidate is only verified in the \
                     repository it was made in",
                    wanted.run
                ),
            )
        }
        None => {
            return refuse(
                Reason::CandidateMissing,
                format!("run {} has no repository on record", wanted.run),
            )
        }
    }
    let attempts = ledger.candidate_attempts(&run)?;
    let chosen = match wanted.index {
        Some(index) => attempts.iter().find(|(i, _)| *i == index),
        None => attempts.last(),
    };
    let Some((index, commit)) = chosen.cloned() else {
        return refuse(
            Reason::CandidateMissing,
            match wanted.index {
                Some(index) => format!("run {} has no candidate at attempt {index}", wanted.run),
                None => format!("run {} produced no candidate", wanted.run),
            },
        );
    };
    if !workspace::commit_exists(source.repo_dir, &commit) {
        return refuse(
            Reason::CandidateMissing,
            format!(
                "the commit {commit} that run {} recorded for attempt {index} is gone from the \
                 repository",
                wanted.run
            ),
        );
    }
    // The ref is best effort when a run writes it; resolution rewrites it
    // from the recorded commit, which keeps the object reachable.
    if let Err(e) = workspace::name_candidate(source.repo_dir, &wanted.run, index, &commit) {
        return refuse(
            Reason::CandidateMissing,
            format!(
                "the candidate ref of run {} attempt {index} could not be rewritten: {e}",
                wanted.run
            ),
        );
    }
    let source_base_sha = workspace::first_parent(source.repo_dir, &commit)?;
    let derived = workspace::derive_patch(source.repo_dir, &source_base_sha, &commit)?;
    let resolved = ResolvedCandidate {
        source_run: wanted.run.clone(),
        index,
        candidate_commit: commit,
        patch_sha256: sha256_hex(derived.as_bytes()),
        source_base_sha,
        target_base_sha: target_base.to_string(),
    };
    match cross_check(ledger, source, &resolved)? {
        Some(refusal) => Ok(Resolution::Refused(refusal)),
        None => Ok(Resolution::Resolved(resolved)),
    }
}

/// Before the candidate is applied: the commit object is still there, the
/// ref names it again, the digest of the patch it derives is the one that
/// was resolved, and the patch files on disk still carry that digest.
/// `None` means the identity holds.
pub(crate) fn cross_check(
    ledger: &Ledger,
    source: &Source<'_>,
    resolved: &ResolvedCandidate,
) -> Result<Option<Refusal>, RunError> {
    let mismatch = |detail: String| Ok(Some(Refusal::new(Reason::CandidateDigestMismatch, detail)));
    let run = RunId::from_stored(resolved.source_run.as_str());
    if !workspace::commit_exists(source.repo_dir, &resolved.candidate_commit) {
        return Ok(Some(Refusal::new(
            Reason::CandidateMissing,
            format!(
                "the commit {} of run {} attempt {} is gone from the repository",
                resolved.candidate_commit, resolved.source_run, resolved.index
            ),
        )));
    }
    let derived = workspace::derive_patch(
        source.repo_dir,
        &resolved.source_base_sha,
        &resolved.candidate_commit,
    )?;
    if sha256_hex(derived.as_bytes()) != resolved.patch_sha256 {
        return mismatch(format!(
            "the patch derived from commit {} no longer has the digest {} recorded at resolution",
            resolved.candidate_commit, resolved.patch_sha256
        ));
    }
    // The files a reader or a reviewer is pointed at: the attempt's own
    // patch, and `candidate-latest.patch` while this is the latest attempt.
    let latest = ledger
        .candidate_attempts(&run)?
        .last()
        .is_some_and(|(index, _)| *index == resolved.index);
    let mut names = vec![format!("candidate-{}.patch", resolved.index)];
    if latest {
        names.push("candidate-latest.patch".to_string());
    }
    for name in names {
        let path = patch_file(source, &resolved.source_run, &name);
        match std::fs::read(&path) {
            Ok(bytes) if sha256_hex(&bytes) == resolved.patch_sha256 => {}
            Ok(_) => {
                return mismatch(format!(
                    "{} was changed after the candidate was resolved: its digest is not the \
                     recorded {}",
                    path.display(),
                    resolved.patch_sha256
                ))
            }
            Err(e) => {
                return mismatch(format!(
                    "{} cannot be read ({e}), so its digest cannot be checked against the \
                     recorded {}",
                    path.display(),
                    resolved.patch_sha256
                ))
            }
        }
    }
    Ok(None)
}

/// The sentence a conflicting candidate ends with.
pub(crate) fn conflict_detail(resolved: &ResolvedCandidate, files: &[String]) -> String {
    format!(
        "candidate {}/{} does not apply to {}: conflicts in {}; nothing was run",
        resolved.source_run,
        resolved.index,
        &resolved.target_base_sha[..resolved.target_base_sha.len().min(12)],
        files.join(", ")
    )
}
