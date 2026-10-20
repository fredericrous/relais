//! Evidence bound to a run, its transitions and its receipt.

use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::ids::RunId;

use super::{parse_state, Ledger, LedgerError, Result, Transition};

/// What an evidence row is: every kind this ledger has ever stored, plus
/// the two this package introduces. A stored name this binary does not
/// know is a corrupt row (`parse_evidence_kind`), never silently dropped
/// or coerced into one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// A named check's log, from the verification profile.
    CheckLog,
    /// The context manifest assembled for a dispatch.
    ContextManifest,
    /// A worker's own result text.
    WorkerResult,
    /// A candidate's exported patch.
    CandidatePatch,
    /// A reviewer's result text.
    ReviewResult,
    /// A stored receipt.
    Receipt,
    /// A declared setup's log.
    SetupLog,
    /// Evidence another tool produced, recorded by `relais evidence
    /// attach` and decided about by nothing here.
    ExternalAttestation,
    /// A person's sign-off, recorded as evidence like any other kind.
    HumanSignOff,
    /// The JUnit report a command wrote, kept as its check log is.
    JunitReport,
    /// Nothing produces this any more (the OS sandbox's denial report); the
    /// ledger may still hold it.
    SandboxDenials,
    /// What relais did differently for a native worker than it asked
    /// (SPEC §23): a continuation keeps its agent's own effort.
    NativeNote,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckLog => "check_log",
            Self::ContextManifest => "context_manifest",
            Self::WorkerResult => "worker_result",
            Self::CandidatePatch => "candidate_patch",
            Self::ReviewResult => "review_result",
            Self::Receipt => "receipt",
            Self::SetupLog => "setup_log",
            Self::ExternalAttestation => "external_attestation",
            Self::HumanSignOff => "human_sign_off",
            Self::JunitReport => "junit_report",
            Self::SandboxDenials => "sandbox_denials",
            Self::NativeNote => "native_note",
        }
    }

    pub(super) fn parse(stored: &str) -> Option<Self> {
        match stored {
            "check_log" => Some(Self::CheckLog),
            "context_manifest" => Some(Self::ContextManifest),
            "worker_result" => Some(Self::WorkerResult),
            "candidate_patch" => Some(Self::CandidatePatch),
            "review_result" => Some(Self::ReviewResult),
            "receipt" => Some(Self::Receipt),
            "setup_log" => Some(Self::SetupLog),
            "external_attestation" => Some(Self::ExternalAttestation),
            "human_sign_off" => Some(Self::HumanSignOff),
            "junit_report" => Some(Self::JunitReport),
            "sandbox_denials" => Some(Self::SandboxDenials),
            "native_note" => Some(Self::NativeNote),
            _ => None,
        }
    }
}

impl std::fmt::Display for EvidenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Where a piece of evidence came from and what it is about — every
/// field optional, so a row `record_evidence` writes (nothing here) and
/// a row `attach_evidence` writes (whatever the caller names) share one
/// column set without one implying the other.
#[derive(Debug, Clone, Copy, Default)]
pub struct EvidenceOrigin<'a> {
    /// The tool that produced this evidence.
    pub tool: Option<&'a str>,
    /// That tool's own identifier for this evidence.
    pub external_id: Option<&'a str>,
    /// The subject this evidence attests to.
    pub subject: Option<&'a str>,
    /// The acceptance criterion this evidence answers, when it answers
    /// one.
    pub criterion_id: Option<&'a str>,
}

/// One piece of evidence bound to a run, read back typed: what kind it
/// is, where it is, and — when recorded — what it is about. Typed so a
/// caller cannot read the kind where it meant to read the path, the way
/// the positional tuple this replaced allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    pub kind: EvidenceKind,
    pub path: String,
    pub sha256: Option<String>,
    pub tool: Option<String>,
    pub external_id: Option<String>,
    pub subject: Option<String>,
    pub criterion_id: Option<String>,
}

impl Ledger {
    /// One piece of evidence bound to a run: the context manifest, a
    /// candidate patch, a check log, the receipt — by path and content
    /// hash (SPEC §12: "every transition has … evidence references").
    pub fn record_evidence(
        &self,
        run_id: &RunId,
        attempt_id: Option<i64>,
        kind: EvidenceKind,
        path: &Path,
        sha256: Option<&str>,
    ) -> Result<()> {
        self.insert_evidence_row(
            run_id,
            attempt_id,
            kind,
            path,
            sha256,
            EvidenceOrigin::default(),
        )
    }

    /// Evidence produced by another tool: `relais evidence attach`
    /// records one row and decides nothing about it — no criterion is
    /// marked met, no run's state changes, no gap clears.
    pub fn attach_evidence(
        &self,
        run_id: &RunId,
        path: &Path,
        sha256: Option<&str>,
        origin: EvidenceOrigin,
    ) -> Result<()> {
        self.insert_evidence_row(
            run_id,
            None,
            EvidenceKind::ExternalAttestation,
            path,
            sha256,
            origin,
        )
    }

    fn insert_evidence_row(
        &self,
        run_id: &RunId,
        attempt_id: Option<i64>,
        kind: EvidenceKind,
        path: &Path,
        sha256: Option<&str>,
        origin: EvidenceOrigin,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO evidence
                (run_id, attempt_id, kind, path, sha256, at,
                 tool, external_id, subject, criterion_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                run_id.as_str(),
                attempt_id,
                kind.as_str(),
                path.to_string_lossy(),
                sha256,
                self.now(),
                origin.tool,
                origin.external_id,
                origin.subject,
                origin.criterion_id,
            ],
        )?;
        Ok(())
    }

    pub fn evidence(&self, run_id: &RunId) -> Result<Vec<EvidenceRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, path, sha256, tool, external_id, subject, criterion_id
               FROM evidence WHERE run_id = ?1 ORDER BY id",
        )?;
        type Row = (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(kind, path, sha256, tool, external_id, subject, criterion_id)| {
                    Ok(EvidenceRow {
                        kind: parse_evidence_kind(&kind)?,
                        path,
                        sha256,
                        tool,
                        external_id,
                        subject,
                        criterion_id,
                    })
                },
            )
            .collect()
    }

    pub fn transitions(&self, run_id: &RunId) -> Result<Vec<Transition>> {
        let mut stmt = self.conn.prepare(
            "SELECT run_id, attempt_id, from_state, to_state, reason, detail_json, at
             FROM transitions WHERE run_id = ?1 ORDER BY id",
        )?;
        // The rows come back raw and are interpreted here: a state this
        // binary does not know is a LedgerError, not a panic in the
        // middle of `explain`.
        type Row = (
            String,
            Option<i64>,
            Option<String>,
            String,
            String,
            Option<String>,
            String,
        );
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(run_id, attempt_id, from_state, to_state, reason, detail, at)| {
                    Ok(Transition {
                        run_id: RunId::from_stored(run_id),
                        attempt_id,
                        from_state: from_state.as_deref().map(parse_state).transpose()?,
                        to_state: parse_state(&to_state)?,
                        reason,
                        // Free-form diagnostic JSON a transition carried.
                        // Nothing reads it back as a value — it is
                        // printed — so a row that will not parse is
                        // reported as "no detail", not as a read failure
                        // that would hide the transition itself.
                        detail: detail.and_then(|d| serde_json::from_str(&d).ok()),
                        at,
                    })
                },
            )
            .collect()
    }

    pub fn store_receipt(
        &self,
        run_id: &RunId,
        receipt_json: &serde_json::Value,
        hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO receipts (run_id, receipt_json, hash, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                run_id.as_str(),
                serde_json::to_string(receipt_json).expect("receipt serializes"),
                hash,
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn receipt(&self, run_id: &RunId) -> Result<Option<(serde_json::Value, String)>> {
        let row: Option<(String, String)> = self
            .conn
            .query_row(
                "SELECT receipt_json, hash FROM receipts WHERE run_id = ?1",
                [run_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        // A receipt is the run's evidence; a stored one that will not
        // parse is reported, so `explain` prints the rest of what it
        // knows instead of aborting.
        row.map(|(json, hash)| {
            let value = serde_json::from_str(&json).map_err(|e| LedgerError::Corrupt {
                what: format!("the receipt of run {run_id}"),
                detail: e.to_string(),
            })?;
            Ok((value, hash))
        })
        .transpose()
    }
}

/// An evidence kind string as an evidence row stored it.
pub(super) fn parse_evidence_kind(stored: &str) -> Result<EvidenceKind> {
    EvidenceKind::parse(stored).ok_or_else(|| LedgerError::Corrupt {
        what: "evidence kind".into(),
        detail: format!("`{stored}` is not an evidence kind this relais knows"),
    })
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{run, task, temp_ledger};
    use super::super::*;

    #[test]
    fn an_unparseable_receipt_is_an_error_not_a_panic() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-bad"), "/repo", None, &task("run-bad"), "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO receipts (run_id, receipt_json, hash, at)
                 VALUES ('run-bad', '{truncated', 'h', 'then')",
                [],
            )
            .expect("truncated receipt");
        assert!(matches!(
            ledger.receipt(&run("run-bad")),
            Err(LedgerError::Corrupt { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unknown_evidence_kind_is_an_error_not_a_panic() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-bad"), "/repo", None, &task("run-bad"), "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "INSERT INTO evidence (run_id, attempt_id, kind, path, sha256, at)
                 VALUES ('run-bad', NULL, 'a_kind_nobody_wrote', '/tmp/x', NULL, 'then')",
                [],
            )
            .expect("a row this binary does not know how to read");
        match ledger.evidence(&run("run-bad")) {
            Err(LedgerError::Corrupt { what, detail }) => {
                assert_eq!(what, "evidence kind");
                assert!(detail.contains("a_kind_nobody_wrote"), "{detail}");
            }
            other => panic!("an unknown evidence kind must be an error, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
