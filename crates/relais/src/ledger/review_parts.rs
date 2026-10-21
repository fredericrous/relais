//! The parts manifest of a review in parts (plan review-over-budget,
//! Changes step 5; SPEC §10): one row per part of the candidate patch, keyed
//! by run, candidate hash and part index.
//!
//! A row names the part's immutable patch artifact and its hash, and, once
//! the part was reviewed, the dispatch, the result artifact and its hash.
//! Every part has an explicit status, so a failed or interrupted review
//! keeps what earlier parts produced and says which parts never ran.

use rusqlite::params;

use crate::ids::RunId;

use super::{Ledger, LedgerError, Result};

/// Where one part of a review stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartStatus {
    /// Planned and not dispatched (yet, or because an earlier part ended
    /// the review).
    NotRun,
    /// Handed to a reviewer; no answer recorded. A part still here after
    /// the run ended was interrupted.
    Dispatched,
    /// The reviewer answered `FINDINGS: none`.
    Passed,
    /// The reviewer reported findings.
    Findings,
    /// No usable answer: the reviewer could not be launched, or answered
    /// without a verdict.
    Unavailable,
}

impl PartStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::Dispatched => "dispatched",
            Self::Passed => "passed",
            Self::Findings => "findings",
            Self::Unavailable => "unavailable",
        }
    }

    fn parse(stored: &str) -> Result<Self> {
        match stored {
            "not_run" => Ok(Self::NotRun),
            "dispatched" => Ok(Self::Dispatched),
            "passed" => Ok(Self::Passed),
            "findings" => Ok(Self::Findings),
            "unavailable" => Ok(Self::Unavailable),
            other => Err(LedgerError::Corrupt {
                what: "a review part's status".into(),
                detail: format!("`{other}` is not a known part status"),
            }),
        }
    }
}

/// A part to plan: its immutable patch artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPart {
    pub index: u32,
    pub patch_path: String,
    pub patch_sha256: String,
}

/// What a part's review ended in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartResult {
    pub status: PartStatus,
    pub dispatch_id: Option<String>,
    pub result_path: Option<String>,
    pub result_sha256: Option<String>,
    pub detail: Option<String>,
}

/// One manifest row as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPartRow {
    pub candidate_sha: String,
    pub index: u32,
    pub count: u32,
    pub patch_path: String,
    pub patch_sha256: String,
    pub dispatch_id: Option<String>,
    pub result_path: Option<String>,
    pub result_sha256: Option<String>,
    pub status: PartStatus,
    pub detail: Option<String>,
}

impl Ledger {
    /// Plan the parts of a run's review of `candidate_sha`: every part gets
    /// a row, `not_run`, in one transaction. A part already on record keeps
    /// its row.
    pub fn plan_review_parts(
        &self,
        run_id: &RunId,
        candidate_sha: &str,
        parts: &[PlannedPart],
    ) -> Result<()> {
        let tx = self.write_tx()?;
        let count = i64::try_from(parts.len()).unwrap_or(i64::MAX);
        for part in parts {
            tx.execute(
                "INSERT INTO review_parts
                    (run_id, candidate_sha, part_index, part_count, patch_path,
                     patch_sha256, status, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'not_run', ?7)
                 ON CONFLICT(run_id, candidate_sha, part_index) DO NOTHING",
                params![
                    run_id.as_str(),
                    candidate_sha,
                    i64::from(part.index),
                    count,
                    part.patch_path,
                    part.patch_sha256,
                    self.now(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record that a part was handed to the reviewer dispatch `dispatch_id`,
    /// before the reviewer runs, so an interrupted part still names it. A part
    /// that was interrupted (`unavailable`) may be handed out again.
    pub fn mark_review_part_dispatched(
        &self,
        run_id: &RunId,
        candidate_sha: &str,
        index: u32,
        dispatch_id: &str,
    ) -> Result<()> {
        self.settle_review_part(
            run_id,
            candidate_sha,
            index,
            &PartResult {
                status: PartStatus::Dispatched,
                dispatch_id: Some(dispatch_id.to_string()),
                result_path: None,
                result_sha256: None,
                detail: None,
            },
            &["not_run", "unavailable"],
        )
    }

    /// A review that is entered again for the same run and candidate: every
    /// part still `dispatched` was cut off by the end of the earlier attempt,
    /// so it becomes `unavailable (interrupted)`, keeping its dispatch id.
    pub fn interrupt_review_parts(&self, run_id: &RunId, candidate_sha: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE review_parts
                SET status = 'unavailable', detail = 'interrupted', updated_at = ?3
              WHERE run_id = ?1 AND candidate_sha = ?2 AND status = 'dispatched'",
            params![run_id.as_str(), candidate_sha, self.now()],
        )?;
        Ok(())
    }

    /// Record how a part's review ended. A part that passed or found
    /// something is never rewritten: its evidence is immutable. One that was
    /// `unavailable` has none, and a review entered again may settle it.
    pub fn finish_review_part(
        &self,
        run_id: &RunId,
        candidate_sha: &str,
        index: u32,
        result: &PartResult,
    ) -> Result<()> {
        self.settle_review_part(
            run_id,
            candidate_sha,
            index,
            result,
            &["not_run", "dispatched", "unavailable"],
        )
    }

    fn settle_review_part(
        &self,
        run_id: &RunId,
        candidate_sha: &str,
        index: u32,
        result: &PartResult,
        from: &[&str],
    ) -> Result<()> {
        let allowed = from
            .iter()
            .map(|status| format!("'{status}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let changed = self.conn.execute(
            &format!(
                "UPDATE review_parts
                    SET status = ?4, dispatch_id = COALESCE(?5, dispatch_id), result_path = ?6,
                        result_sha256 = ?7, detail = ?8, updated_at = ?9
                  WHERE run_id = ?1 AND candidate_sha = ?2 AND part_index = ?3
                    AND status IN ({allowed})"
            ),
            params![
                run_id.as_str(),
                candidate_sha,
                i64::from(index),
                result.status.as_str(),
                result.dispatch_id,
                result.result_path,
                result.result_sha256,
                result.detail,
                self.now(),
            ],
        )?;
        if changed == 1 {
            return Ok(());
        }
        Err(LedgerError::Corrupt {
            what: "a review part".into(),
            detail: format!(
                "part {index} of run {run_id} is not planned, or has already ended and its \
                 evidence is immutable"
            ),
        })
    }

    /// The manifest rows of one candidate of a run's review in parts, by part
    /// index.
    pub fn review_parts_of(
        &self,
        run_id: &RunId,
        candidate_sha: &str,
    ) -> Result<Vec<ReviewPartRow>> {
        Ok(self
            .review_parts(run_id)?
            .into_iter()
            .filter(|row| row.candidate_sha == candidate_sha)
            .collect())
    }

    /// The manifest of a run's review in parts, ordered by candidate and
    /// part index; empty for a run that reviewed in one piece.
    pub fn review_parts(&self, run_id: &RunId) -> Result<Vec<ReviewPartRow>> {
        let mut statement = self.conn.prepare(
            "SELECT candidate_sha, part_index, part_count, patch_path, patch_sha256,
                    dispatch_id, result_path, result_sha256, status, detail
               FROM review_parts WHERE run_id = ?1
              ORDER BY candidate_sha, part_index",
        )?;
        let rows = statement
            .query_map([run_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(
                    candidate_sha,
                    index,
                    count,
                    patch_path,
                    patch_sha256,
                    dispatch_id,
                    result_path,
                    result_sha256,
                    status,
                    detail,
                )| {
                    Ok(ReviewPartRow {
                        candidate_sha,
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                        count: u32::try_from(count).unwrap_or(u32::MAX),
                        patch_path,
                        patch_sha256,
                        dispatch_id,
                        result_path,
                        result_sha256,
                        status: PartStatus::parse(&status)?,
                        detail,
                    })
                },
            )
            .collect()
    }
}
