//! The holdout definitions a run resolved at preflight (ADR-0045 phase 3c,
//! SPEC §10): one row per holdout name, keyed by run, holding the resolved
//! definition and its digest. A successor intent reads its predecessor's
//! rows, so the evidence that judges a piece of work does not change with
//! machine.toml or the store's ref. The rows live in the ledger only.

use rusqlite::params;

use crate::holdout::ResolvedHoldout;
use crate::ids::RunId;

use super::{Ledger, LedgerError, Result};

impl Ledger {
    /// Freeze `holdouts` as the run's definitions. Recording the same name
    /// again for the run replaces the row, so a re-entered preflight is safe.
    pub fn record_holdout_definitions(
        &self,
        run_id: &RunId,
        holdouts: &[ResolvedHoldout],
    ) -> Result<()> {
        let transaction = self.write_tx()?;
        for holdout in holdouts {
            let definition = serde_json::to_string(holdout).map_err(|e| LedgerError::Corrupt {
                what: "holdout definition".into(),
                detail: e.to_string(),
            })?;
            transaction.execute(
                "INSERT OR REPLACE INTO holdout_definitions
                    (run_id, name, definition_json, digest)
                 VALUES (?1, ?2, ?3, ?4)",
                params![run_id.as_str(), holdout.name, definition, holdout.digest],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// The definitions a run froze, ordered by name; empty for a run that
    /// resolved no holdout.
    pub fn holdout_definitions(&self, run_id: &RunId) -> Result<Vec<ResolvedHoldout>> {
        let mut statement = self.conn.prepare(
            "SELECT definition_json FROM holdout_definitions
              WHERE run_id = ?1 ORDER BY name",
        )?;
        let stored = statement
            .query_map([run_id.as_str()], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        stored
            .iter()
            .map(|json| {
                serde_json::from_str(json).map_err(|e| LedgerError::Corrupt {
                    what: "holdout definition".into(),
                    detail: e.to_string(),
                })
            })
            .collect()
    }
}
