//! Verification snapshots: what a receipt needs, saved before a run stops
//! for a review that did not fit or could not be had (SPEC §10).
//!
//! The snapshot is the receipt the candidate would have earned, bound to
//! the candidate, base, contract and policy hashes it was verified under.
//! `relais decide <run> approve` seals the person's receipt from it, so
//! nothing in the receipt is recomputed or invented at approval time.

use rusqlite::{params, OptionalExtension};

use crate::ids::RunId;

use super::{DecisionAnswer, EvidenceKind, Ledger, LedgerError, Result};

/// One saved snapshot, as the ledger stores it.
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationSnapshot {
    pub candidate_sha: String,
    pub base_sha: String,
    pub contract_hash: String,
    pub policy_hash: String,
    /// The `Reason` the run stopped for (`review_over_budget`,
    /// `review_unavailable`).
    pub stop_reason: String,
    /// The receipt the run would have sealed, outcome `needs_decision`.
    pub receipt: serde_json::Value,
}

/// How an approval's publication ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptResolution {
    /// The receipt row, its evidence row and the decision were written.
    Sealed,
    /// The decision was already resolved with this very receipt: nothing
    /// was written.
    AlreadySealed,
    /// The run has no open decision, and none resolved with this receipt.
    NotOpen,
}

impl Ledger {
    /// Keep the snapshot of a run about to stop for review. A run has one;
    /// a later save for the same run (a resumed run) replaces it.
    pub fn save_verification_snapshot(
        &self,
        run_id: &RunId,
        snapshot: &VerificationSnapshot,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO verification_snapshots
                (run_id, candidate_sha, base_sha, contract_hash, policy_hash,
                 stop_reason, receipt_json, saved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(run_id) DO UPDATE SET
                 candidate_sha = excluded.candidate_sha,
                 base_sha = excluded.base_sha,
                 contract_hash = excluded.contract_hash,
                 policy_hash = excluded.policy_hash,
                 stop_reason = excluded.stop_reason,
                 receipt_json = excluded.receipt_json,
                 saved_at = excluded.saved_at",
            params![
                run_id.as_str(),
                snapshot.candidate_sha,
                snapshot.base_sha,
                snapshot.contract_hash,
                snapshot.policy_hash,
                snapshot.stop_reason,
                serde_json::to_string(&snapshot.receipt).expect("a receipt serializes"),
                self.now(),
            ],
        )?;
        Ok(())
    }

    /// Drop the run's snapshot: a new attempt makes it stale.
    pub fn delete_verification_snapshot(&self, run_id: &RunId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM verification_snapshots WHERE run_id = ?1",
            [run_id.as_str()],
        )?;
        Ok(())
    }

    /// The run's saved snapshot, if it saved one.
    pub fn verification_snapshot(&self, run_id: &RunId) -> Result<Option<VerificationSnapshot>> {
        let row: Option<(String, String, String, String, String, String)> = self
            .conn
            .query_row(
                "SELECT candidate_sha, base_sha, contract_hash, policy_hash, stop_reason,
                        receipt_json
                   FROM verification_snapshots WHERE run_id = ?1",
                [run_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        row.map(
            |(candidate_sha, base_sha, contract_hash, policy_hash, stop_reason, json)| {
                let receipt = serde_json::from_str(&json).map_err(|e| LedgerError::Corrupt {
                    what: format!("the verification snapshot of run {run_id}"),
                    detail: e.to_string(),
                })?;
                Ok(VerificationSnapshot {
                    candidate_sha,
                    base_sha,
                    contract_hash,
                    policy_hash,
                    stop_reason,
                    receipt,
                })
            },
        )
        .transpose()
    }

    /// Answer an open decision AND record the receipt that answer seals:
    /// the receipt row, its evidence row (the artifact's path and
    /// sha256) and the resolution are one transaction. The artifact was
    /// written first, to a content-addressed path, so a failure before
    /// this commit leaves a file no row points at, and the retry rewrites
    /// the same bytes.
    ///
    /// `hash` is `Receipt::hash()`, what every receipt row stores;
    /// `artifact_sha256` is the artifact's content address, stored on the
    /// evidence row.
    ///
    /// A retry that finds the decision already resolved, and the run's
    /// receipt row carrying `hash`, writes nothing.
    pub fn resolve_decision_with_receipt(
        &self,
        run_id: &RunId,
        answer: &DecisionAnswer<'_>,
        receipt_json: &serde_json::Value,
        hash: &str,
        artifact: &std::path::Path,
        artifact_sha256: &str,
    ) -> Result<ReceiptResolution> {
        let tx = self.write_tx()?;
        if !self.resolve_decision_in(&tx, run_id, answer)? {
            let sealed: Option<String> = tx
                .query_row(
                    "SELECT hash FROM receipts WHERE run_id = ?1",
                    [run_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            return Ok(if sealed.as_deref() == Some(hash) {
                ReceiptResolution::AlreadySealed
            } else {
                ReceiptResolution::NotOpen
            });
        }
        tx.execute(
            "INSERT OR REPLACE INTO receipts (run_id, receipt_json, hash, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                run_id.as_str(),
                serde_json::to_string(receipt_json).expect("receipt serializes"),
                hash,
                self.now()
            ],
        )?;
        tx.execute(
            "INSERT INTO evidence
                (run_id, attempt_id, kind, path, sha256, at,
                 tool, external_id, subject, criterion_id)
             VALUES (?1, NULL, ?2, ?3, ?4, ?5, NULL, NULL, NULL, NULL)",
            params![
                run_id.as_str(),
                EvidenceKind::Receipt.as_str(),
                artifact.to_string_lossy(),
                artifact_sha256,
                self.now(),
            ],
        )?;
        tx.commit()?;
        Ok(ReceiptResolution::Sealed)
    }
}
