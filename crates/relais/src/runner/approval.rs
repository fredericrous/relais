//! Approving a run that stopped for a review it could not have (SPEC §10).
//!
//! `relais decide <run> approve` on a run that ended `review_over_budget`
//! or `review_unavailable` seals a receipt from the verification snapshot
//! the run saved before it stopped. Publication order: the receipt artifact
//! is written to a content-addressed path, then the receipt row, its
//! evidence row and the resolution are one ledger transaction. A retry that
//! finds the decision resolved with the same receipt hash writes nothing;
//! an artifact with no row is rewritten with the same bytes; missing
//! evidence is a named error, never a fabricated receipt.

use std::path::{Path, PathBuf};

use crate::contract::Kind;
use crate::ids::{sha256_hex, RunId};
use crate::ledger::{DecisionAnswer, Ledger, LedgerError, ReceiptResolution};
use crate::lifecycle::{Reason, State};
use crate::verify::{Receipt, SettledVia};

/// The reasons whose approval is sealed from a snapshot.
pub fn seals_from_snapshot(raised_reason: &str) -> bool {
    [Reason::ReviewOverBudget, Reason::ReviewUnavailable]
        .iter()
        .any(|reason| reason.as_str() == raised_reason)
}

/// Why an approval could not be sealed.
#[derive(Debug)]
pub enum ApprovalError {
    /// The run saved no snapshot: there is no evidence to seal a receipt
    /// from, and none is invented.
    NoSnapshot {
        run: String,
    },
    /// The snapshot's receipt is not the one its bound hashes name.
    Unbound {
        run: String,
        what: String,
    },
    /// The snapshot's verification carries gaps.
    Gaps {
        run: String,
        gaps: Vec<String>,
    },
    /// The snapshot's checks do not accept the candidate.
    NotAccepted {
        run: String,
    },
    /// The run has no open decision, and none resolved with this receipt.
    NotOpen {
        run: String,
    },
    /// The snapshot is for another stop or another candidate than the
    /// decision and the run: it is stale.
    Stale {
        run: String,
        what: String,
    },
    /// The snapshot's receipt will not parse.
    Corrupt {
        run: String,
        detail: String,
    },
    Io(std::io::Error),
    Ledger(LedgerError),
    /// A failure injected between the artifact and the ledger commit.
    Interrupted(String),
}

impl std::fmt::Display for ApprovalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSnapshot { run } => write!(
                f,
                "run {run} saved no verification snapshot, so there is no evidence to seal a \
                 receipt from; it can be revised or rejected"
            ),
            Self::Unbound { run, what } => write!(
                f,
                "the verification snapshot of run {run} is not bound to its receipt: {what}"
            ),
            Self::Gaps { run, gaps } => write!(
                f,
                "run {run} still carries verification gaps ({}) — a waiver is a policy or \
                 contract change, never a CLI flag",
                gaps.join("; ")
            ),
            Self::Stale { run, what } => {
                write!(f, "the verification snapshot of run {run} is stale: {what}")
            }
            Self::NotAccepted { run } => write!(
                f,
                "the verification snapshot of run {run} does not accept its candidate"
            ),
            Self::NotOpen { run } => write!(f, "run {run} has no open decision on record"),
            Self::Corrupt { run, detail } => write!(
                f,
                "the verification snapshot of run {run} will not parse: {detail}"
            ),
            Self::Io(e) => write!(f, "the receipt artifact could not be written: {e}"),
            Self::Ledger(e) => write!(f, "{e}"),
            Self::Interrupted(detail) => write!(f, "receipt publication was interrupted: {detail}"),
        }
    }
}

impl std::error::Error for ApprovalError {}

impl From<LedgerError> for ApprovalError {
    fn from(e: LedgerError) -> Self {
        Self::Ledger(e)
    }
}

impl From<std::io::Error> for ApprovalError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// What a sealed approval wrote, or found written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub receipt_hash: String,
    pub artifact: PathBuf,
    /// `false` when the decision was already resolved with this receipt and
    /// nothing was written.
    pub written: bool,
}

/// The receipt an approval seals, from the run's snapshot: the saved
/// receipt with its outcome `accepted`. Checked against the snapshot's
/// bound hashes and the run's verification gaps first.
pub fn sealed_receipt(
    ledger: &Ledger,
    run: &RunId,
    raised_reason: &str,
) -> Result<Receipt, ApprovalError> {
    let name = run.as_str().to_string();
    let snapshot = ledger
        .verification_snapshot(run)?
        .ok_or_else(|| ApprovalError::NoSnapshot { run: name.clone() })?;
    if snapshot.stop_reason != raised_reason {
        return Err(ApprovalError::Stale {
            run: name,
            what: format!(
                "it was saved for a stop for {} and the decision was raised for {raised_reason}",
                snapshot.stop_reason
            ),
        });
    }
    let latest = ledger.latest_attempt_candidate(run)?;
    // A run that assembled its packages' work records no attempt of its own:
    // its candidate is the integration head, which the snapshot names, and
    // any attempt it later makes deletes the snapshot.
    let integrated = latest.is_none() && ledger.attempt_count(run)? == 0;
    if !integrated && latest.as_deref() != Some(snapshot.candidate_sha.as_str()) {
        return Err(ApprovalError::Stale {
            run: name,
            what: format!(
                "it is for candidate {} and the run's latest candidate is {}",
                snapshot.candidate_sha,
                latest.as_deref().unwrap_or("none")
            ),
        });
    }
    let mut receipt: Receipt =
        serde_json::from_value(snapshot.receipt).map_err(|e| ApprovalError::Corrupt {
            run: name.clone(),
            detail: e.to_string(),
        })?;
    let bound = [
        ("candidate", &snapshot.candidate_sha, &receipt.candidate_sha),
        ("base", &snapshot.base_sha, &receipt.base_sha),
        ("contract", &snapshot.contract_hash, &receipt.contract_hash),
        ("policy", &snapshot.policy_hash, &receipt.policy_hash),
    ];
    if let Some((what, saved, in_receipt)) = bound.iter().find(|(_, saved, held)| saved != held) {
        return Err(ApprovalError::Unbound {
            run: name,
            what: format!(
                "the {what} hash is {saved} on the snapshot and {in_receipt} in the receipt"
            ),
        });
    }
    if receipt.run_id != name {
        return Err(ApprovalError::Unbound {
            run: name,
            what: format!("the receipt names run {}", receipt.run_id),
        });
    }
    if !receipt.verification.gaps.is_empty() {
        return Err(ApprovalError::Gaps {
            run: name,
            gaps: receipt.verification.gaps,
        });
    }
    if !accepted_without_the_review(&receipt) {
        return Err(ApprovalError::NotAccepted { run: name });
    }
    receipt.outcome = State::Accepted.as_str().to_string();
    Ok(receipt)
}

/// Does the receipt accept its candidate, the review that could not be had
/// aside? An inspection's report review settles the criteria no check names;
/// with that review unavailable, the approval is the judgment that stands in
/// for it, so those criteria are not held against the receipt.
fn accepted_without_the_review(receipt: &Receipt) -> bool {
    match receipt.kind {
        Kind::Inspect => receipt.criteria.iter().all(|criterion| {
            !criterion.mandatory
                || criterion.met
                || criterion.settled_via == Some(SettledVia::ReportReview)
        }),
        Kind::Change => receipt.accepted_by_settlement(),
    }
}

/// Seal `run`'s approval. `after_artifact` runs between the artifact write
/// and the ledger commit: a seam for the test that proves a failure there is
/// recovered by the retry; production passes a no-op.
pub fn approve_from_snapshot(
    ledger: &Ledger,
    runs_dir: &Path,
    run: &RunId,
    answer: &DecisionAnswer<'_>,
    after_artifact: &dyn Fn() -> Result<(), String>,
) -> Result<Sealed, ApprovalError> {
    let decision = ledger.decision_of_run(run)?;
    let raised = decision.as_ref().map_or("", |d| d.raised_reason.as_str());
    let receipt = sealed_receipt(ledger, run, raised)?;
    let decision = decision.ok_or_else(|| ApprovalError::NotOpen {
        run: run.as_str().to_string(),
    })?;
    let body = serde_json::to_string_pretty(&receipt).expect("a receipt serializes");
    // The row stores the canonical-JSON hash every receipt row stores; the
    // content address names the artifact and is its sha256 on the evidence row.
    let receipt_hash = receipt.hash();
    let content_address = sha256_hex(body.as_bytes());
    let artifact = runs_dir
        .join(run.as_str())
        .join("receipts")
        .join(format!("{content_address}.json"));
    // Resolved with this very receipt already: a retry, nothing to decide.
    if decision.resolved_at.is_some() {
        let held = ledger.receipt(run)?.map(|(_, hash)| hash);
        return if held.as_deref() == Some(receipt_hash.as_str()) {
            // Heal a missing `<run>/receipt.json` with the same bytes; an
            // intact one is left alone.
            write_artifact(&runs_dir.join(run.as_str()).join("receipt.json"), &body)?;
            Ok(Sealed {
                receipt_hash,
                artifact,
                written: false,
            })
        } else {
            Err(ApprovalError::NotOpen {
                run: run.as_str().to_string(),
            })
        };
    }
    write_artifact(&artifact, &body)?;
    after_artifact().map_err(ApprovalError::Interrupted)?;
    let value = serde_json::to_value(&receipt).expect("a receipt serializes");
    let resolution = ledger.resolve_decision_with_receipt(
        run,
        answer,
        &value,
        &receipt_hash,
        &artifact,
        &content_address,
    )?;
    match resolution {
        ReceiptResolution::Sealed | ReceiptResolution::AlreadySealed => {
            // Every receipt is also `<run>/receipt.json`, where resume, run
            // and reseal look for it. Written after the commit, with the same
            // bytes, and only for a receipt the ledger holds.
            write_artifact(&runs_dir.join(run.as_str()).join("receipt.json"), &body)?;
            Ok(Sealed {
                receipt_hash,
                artifact,
                written: resolution == ReceiptResolution::Sealed,
            })
        }
        ReceiptResolution::NotOpen => Err(ApprovalError::NotOpen {
            run: run.as_str().to_string(),
        }),
    }
}

/// Write `body` at the content-addressed `path` unless the same bytes are
/// already there. Through a temporary sibling and a rename, so a reader
/// never sees half an artifact.
fn write_artifact(path: &Path, body: &str) -> std::io::Result<()> {
    if std::fs::read(path).is_ok_and(|held| held == body.as_bytes()) {
        return Ok(());
    }
    let dir = path.parent().expect("an artifact path has a directory");
    std::fs::create_dir_all(dir)?;
    let temporary = path.with_extension("json.partial");
    let mut file = std::fs::File::create(&temporary)?;
    std::io::Write::write_all(&mut file, body.as_bytes())?;
    // Durable before the rename names it.
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary, path)
}
