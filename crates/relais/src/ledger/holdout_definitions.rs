//! The holdout definitions a run resolved at preflight (ADR-0045 phase 3c,
//! SPEC §10): one row per holdout name, keyed by run, holding the resolved
//! definition and its digest. A successor intent reads its predecessor's
//! rows, so the evidence that judges a piece of work does not change with
//! machine.toml or the store's ref. The rows live in the ledger only.

use rusqlite::params;

use crate::acceptance::AcceptanceEntry;
use crate::holdout::{HoldoutSource, ResolvedHoldout};
use crate::ids::RunId;

use super::{Ledger, LedgerError, Result};

impl Ledger {
    /// Freeze `holdouts` as the run's definitions. Recording the same name
    /// again for the run replaces the row, so a re-entered preflight is safe.
    pub fn record_holdout_definitions(
        &self,
        run_id: &RunId,
        holdouts: &[(ResolvedHoldout, HoldoutSource)],
    ) -> Result<()> {
        let transaction = self.write_tx()?;
        for (holdout, source) in holdouts {
            let definition = serde_json::to_string(holdout).map_err(|e| LedgerError::Corrupt {
                what: "holdout definition".into(),
                detail: e.to_string(),
            })?;
            transaction.execute(
                "INSERT OR REPLACE INTO holdout_definitions
                    (run_id, name, definition_json, digest, source)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    run_id.as_str(),
                    holdout.name,
                    definition,
                    holdout.digest,
                    source.as_str()
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// The definitions a run froze, ordered by name; empty for a run that
    /// resolved no holdout.
    pub fn holdout_definitions(&self, run_id: &RunId) -> Result<Vec<ResolvedHoldout>> {
        self.definitions_where(run_id, None)
    }

    /// The definitions a run froze with source `attached`, ordered by name.
    pub fn attached_holdout_definitions(&self, run_id: &RunId) -> Result<Vec<ResolvedHoldout>> {
        self.definitions_where(run_id, Some(HoldoutSource::Attached))
    }

    /// The criteria that settle the run: the contract's own, then one
    /// mandatory criterion per attached holdout the ledger holds for the run.
    /// Every read that settles a criterion goes through here, so what the
    /// machine declaration says now never changes what judges a run.
    pub fn effective_acceptance(
        &self,
        run_id: &RunId,
        contract: &crate::contract::TaskContract,
    ) -> Result<Vec<AcceptanceEntry>> {
        let mut entries = contract.acceptance.clone();
        entries.extend(
            self.attached_holdout_definitions(run_id)?
                .iter()
                .map(|holdout| crate::holdout::attached_criterion(&holdout.name)),
        );
        Ok(entries)
    }

    fn definitions_where(
        &self,
        run_id: &RunId,
        source: Option<HoldoutSource>,
    ) -> Result<Vec<ResolvedHoldout>> {
        let mut statement = self.conn.prepare(
            "SELECT definition_json FROM holdout_definitions
              WHERE run_id = ?1 AND (?2 IS NULL OR source = ?2) ORDER BY name",
        )?;
        let stored = statement
            .query_map(
                params![run_id.as_str(), source.map(HoldoutSource::as_str)],
                |row| row.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        stored
            .iter()
            .map(|json| {
                let holdout: ResolvedHoldout =
                    serde_json::from_str(json).map_err(|e| LedgerError::Corrupt {
                        what: "holdout definition".into(),
                        detail: e.to_string(),
                    })?;
                if let Some(attach) = &holdout.decl.attach {
                    attach.check().map_err(|detail| LedgerError::Corrupt {
                        what: format!("holdout definition `{}`", holdout.name),
                        detail,
                    })?;
                }
                Ok(holdout)
            })
            .collect()
    }
}
