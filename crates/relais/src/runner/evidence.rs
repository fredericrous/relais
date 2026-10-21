//! The evidence and receipt writes of a run.

use std::path::Path;
use std::time::Instant;

use super::{
    review_record_of, AttemptContext, Candidate, PendingEvidence, Progress, Reason, RunEngine,
    RunError, RunOutcome, State, Terminal, ADMISSION_POLL,
};
use crate::ledger::{EvidenceKind, LedgerError};
use crate::route::Recipe;
use crate::verify::{self, Receipt, VerificationReport};
use crate::workspace::{self};

impl<'a> RunEngine<'a> {
    /// Judge every dispatch the ledger still records as live for this
    /// run, with what the runner has: the process table, and the gate's
    /// word on whether a native agent's session still says hello. No
    /// coordinator view is consulted — it can only turn an unknown
    /// outcome into a held seat, and both keep the tree — so a dispatch
    /// with no PID and no session evidence stays unknown, never dead.
    pub(super) fn judge_live_dispatches(&self) -> Result<crate::resume::Reconciliation, RunError> {
        let live: Vec<_> = self
            .config
            .ledger
            .live_dispatches()?
            .into_iter()
            .filter(|dispatch| dispatch.run == self.run_id)
            .collect();
        let session_live = |session: &str| -> Option<bool> {
            let gate = self.config.gate?;
            match gate.native_hello_age(session) {
                Ok(Some(age)) => Some(age < crate::native::HELLO_FRESH),
                Ok(None) | Err(_) => None,
            }
        };
        Ok(crate::resume::reconcile_with_sessions(
            &live,
            None,
            &|pid| crate::coordinator::process_alive(pid.get()),
            &session_live,
        ))
    }

    /// Record every log a setup or a profile produced as evidence, under
    /// one kind. Called BEFORE the outcomes are judged: a failing setup's
    /// log, or the 127 that blocks a baseline, is the most useful
    /// evidence the run has, and an early return must not lose it.
    pub(crate) fn record_logs(
        &self,
        attempt_id: Option<i64>,
        kind: EvidenceKind,
        outcomes: &[verify::CheckOutcome],
    ) -> Result<(), LedgerError> {
        for outcome in outcomes {
            self.config.ledger.record_evidence(
                &self.run_id,
                attempt_id,
                kind,
                Path::new(&outcome.log_path),
                Some(&outcome.log_sha256),
            )?;
            self.record_junit_report(outcome)?;
        }
        Ok(())
    }

    /// Record the kept copy of a check's JUnit report, when one was read,
    /// under its own kind and with its sha256. A report that was missing
    /// or unparseable left no copy, so it has no row.
    pub(crate) fn record_junit_report(
        &self,
        outcome: &verify::CheckOutcome,
    ) -> Result<(), LedgerError> {
        if let Some(verify::JunitReport::Reported {
            artifact: Some(kept),
            ..
        }) = &outcome.junit
        {
            self.config.ledger.record_evidence(
                &self.run_id,
                None,
                EvidenceKind::JunitReport,
                Path::new(&kept.path),
                Some(&kept.sha256),
            )?;
        }
        Ok(())
    }

    /// Write one artifact next to the run and record its evidence row —
    /// one fallible operation, because an artifact the ledger does not
    /// point at is not evidence, and evidence pointing at a file that
    /// was never written is worse (X5).
    pub(crate) fn record_artifact(
        &self,
        attempt_id: Option<i64>,
        kind: EvidenceKind,
        path: &Path,
        body: &str,
    ) -> Result<(), RunError> {
        std::fs::write(path, body)?;
        self.config.ledger.record_evidence(
            &self.run_id,
            attempt_id,
            kind,
            path,
            Some(&workspace::sha256_file(path)?),
        )?;
        Ok(())
    }

    /// `record_artifact` for an artifact that is never overwritten: created
    /// new and synced, or already there with the same bytes.
    pub(crate) fn record_immutable_artifact(
        &self,
        kind: EvidenceKind,
        path: &Path,
        body: &str,
    ) -> Result<(), RunError> {
        super::review::write_immutable(path, body.as_bytes())?;
        self.config.ledger.record_evidence(
            &self.run_id,
            None,
            kind,
            path,
            Some(&workspace::sha256_file(path)?),
        )?;
        Ok(())
    }

    /// Store a receipt in the ledger and next to the run, with its
    /// evidence row; close the accepting attempt.
    pub(crate) fn seal(
        &mut self,
        receipt: &Receipt,
        attempt_id: Option<i64>,
        worktree_path: Option<&Path>,
        candidate_sha: &str,
    ) -> Result<(), RunError> {
        let ledger = self.config.ledger;
        self.persist_receipt(receipt, attempt_id)?;
        if let Some(attempt_id) = attempt_id {
            ledger.finish_attempt(
                attempt_id,
                State::Accepted,
                worktree_path
                    .map(|path| path.to_string_lossy().into_owned())
                    .as_deref(),
                Some(candidate_sha),
            )?;
        }
        Ok(())
    }

    /// Write one receipt everywhere a receipt lives: the ledger row, the
    /// `receipt.json` beside the run's other artifacts, and an evidence
    /// row pointing at that file with its hash.
    ///
    /// All three, always. A receipt in the ledger and not on disk is one
    /// `relais explain` and a person reading the artifacts directory
    /// cannot see, and a run that reached a receipt without leaving one
    /// there looks, to anyone but a ledger query, like a run that never
    /// produced one.
    pub(super) fn persist_receipt(
        &mut self,
        receipt: &Receipt,
        attempt_id: Option<i64>,
    ) -> Result<(), RunError> {
        let ledger = self.config.ledger;
        let receipt_hash = receipt.hash();
        ledger.store_receipt(
            &self.run_id,
            &serde_json::to_value(receipt).expect("a receipt serializes"),
            &receipt_hash,
        )?;
        let receipt_path = self.artifacts.join("receipt.json");
        std::fs::write(
            &receipt_path,
            serde_json::to_string_pretty(receipt).expect("a receipt serializes"),
        )?;
        self.step("receipt", receipt_path.display().to_string());
        ledger.record_evidence(
            &self.run_id,
            attempt_id,
            EvidenceKind::Receipt,
            &receipt_path,
            Some(&receipt_hash),
        )?;
        Ok(())
    }

    /// Store the receipt a candidate stopped only by unmet mandatory
    /// human-sign-off criteria would have earned, `outcome` named as
    /// `needs_decision` rather than `accepted` — unlike [`Self::seal`],
    /// this closes no attempt and assigns no state, because the run is
    /// NOT accepted yet. `relais decide --answer approve --criterion
    /// <id>` is the only thing that later re-seals this same row: one
    /// receipt, written once here and possibly overwritten there, never
    /// a second one.
    pub(super) fn store_pending_receipt(
        &mut self,
        ctx: &AttemptContext<'_>,
        progress: &mut Progress,
        candidate: &Candidate,
        pending: PendingEvidence,
    ) -> Result<(), RunError> {
        let PendingEvidence {
            checks,
            gaps,
            gaps_not_judged,
            notes,
            gate_coverage,
            verdict,
        } = pending;
        let preflight = ctx.preflight;
        let report = VerificationReport {
            candidate_sha: candidate.sha.clone(),
            base_sha: preflight.base_sha.clone(),
            contract_hash: preflight.contract_hash.clone(),
            policy_hash: preflight.authority.authority_hash.clone(),
            checks,
            gaps,
            gaps_not_judged,
            notes,
            baseline_failures: ctx.baseline.failures.clone(),
            amont_bypasses: Vec::new(),
            amont_downgrades: Vec::new(),
            verification_inputs_changed: Vec::new(),
            integration_gaps: preflight.integration_gaps.clone(),
            baseline_cached: ctx.baseline.cached,
            baseline_cache_refused: ctx.baseline.cache_refused.clone(),
        };
        let (criteria, mandatory_evidence_independence) =
            self.settle(ctx, &report, &gate_coverage, verdict.as_ref())?;
        let receipt = Receipt {
            run_id: self.run_id.as_str().to_string(),
            candidate_sha: candidate.sha.clone(),
            base_sha: preflight.base_sha.clone(),
            contract_hash: preflight.contract_hash.clone(),
            policy_hash: preflight.authority.authority_hash.clone(),
            outcome: State::NeedsDecision.as_str().to_string(),
            verification: report,
            kind: self.config.contract.kind(),
            candidate: preflight
                .candidate
                .as_ref()
                .map(|prepared| prepared.resolved.clone()),
            predecessor: preflight.predecessor.clone(),
            models_used: progress.models_used.clone(),
            attempts: candidate.index,
            cost_completeness: progress.spend.completeness,
            cost: progress.spend.total,
            criteria,
            mandatory_evidence_independence,
            recipe: crate::policy::RecipeRecord::of(
                preflight
                    .decision
                    .covering
                    .as_ref()
                    .map(Recipe::covering_identity),
            ),
            verification_profile_hash: preflight.authority.verification_profile.hash(),
            review: review_record_of(self.config.ledger, &self.run_id)?,
            ladder: verify::LadderRecord::Single(preflight.decision.ladder.rungs().to_vec()),
            efforts_used: self.config.ledger.efforts_used(&self.run_id)?,
        };
        // No attempt id: this receipt belongs to the run, and the
        // attempt that earned it is not finished — the run is waiting on
        // a person, not accepted.
        self.persist_receipt(&receipt, None)
    }

    /// Wait for a worktree's write lease to be gone before it is
    /// snapshotted or verified (SPEC §23: "root verification waits for
    /// all relevant write leases to be released"; §10: "after all
    /// candidate-writing descendants have stopped or relinquished their
    /// write leases"). `Ok(None)` = nobody writes it; `Ok(Some(holder))`
    /// = the clock ran out with the holder still there. A wait that
    /// actually happened is a transition on the run. Unmanaged execution
    /// has no leases and waits for nothing.
    ///
    /// A lease this run's OWN dispatch still holds is treated as gone:
    /// that process has ended — the lease outlived it only because the
    /// coordinator refused the release (R3) — so the tree is still, and
    /// waiting would be the run waiting on itself to its own deadline.
    pub(crate) fn wait_for_writers(&mut self, worktree: &Path) -> Result<Option<String>, RunError> {
        let Some(gate) = self.config.gate else {
            return Ok(None);
        };
        let key = worktree.to_string_lossy().into_owned();
        let started = Instant::now();
        let mut waited = false;
        loop {
            match gate.write_lease_holder(&key) {
                Ok(None) => {
                    if waited {
                        self.transition(
                            State::Verifying,
                            Reason::WriteLeaseWait,
                            serde_json::json!({
                                "worktree": key,
                                "waited_ms": started.elapsed().as_millis() as u64,
                            }),
                        )?;
                    }
                    return Ok(None);
                }
                Ok(Some(holder)) if self.own_write_leases.contains(&holder) => {
                    self.transition(
                        State::Verifying,
                        Reason::WriteLeaseNotReleased,
                        serde_json::json!({
                            "worktree": key,
                            "holder": holder,
                            "detail": "the lease is this run's own and its process has ended; \
                                       verification does not wait on itself",
                        }),
                    )?;
                    return Ok(None);
                }
                Ok(Some(holder)) => {
                    if self.budgets.remaining().is_zero() {
                        return Ok(Some(holder));
                    }
                    waited = true;
                    std::thread::sleep(ADMISSION_POLL);
                }
                Err(e) => {
                    return Err(RunError::Other(format!(
                        "the write lease on {key} could not be queried: {e}"
                    )));
                }
            }
        }
    }

    /// The run ends interrupted because a worktree is still being
    /// written: nothing is snapshotted from a tree in motion, and the
    /// tree is preserved for whoever holds it (SPEC §23).
    pub(crate) fn stop_on_held_lease(
        &mut self,
        worktree: &Path,
        holder: &str,
    ) -> Result<RunOutcome, RunError> {
        let key = worktree.to_string_lossy().into_owned();
        self.finish(
            Reason::WriteLeaseHeld,
            serde_json::json!({ "worktree": key, "holder": holder }),
            Terminal::Interrupted {
                detail: format!(
                    "worktree {key} is still being written by {holder} at the run's deadline; \
                     no candidate was snapshotted and the worktree is preserved"
                ),
            },
        )
    }
}
