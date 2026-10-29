//! What holdouts catch and leak, counted (ADR-0042, ADR-0045 phase 4,
//! SPEC §10): one `holdout_outcomes` row per holdout a verification ran,
//! with whether the profile's checks were green on that candidate, and one
//! `holdout_exposures` row per (dispatch, tool, marker) where a worker's
//! tool call named a holdout location. A row stores the marker's name,
//! never the text that matched.

use rusqlite::params;

use crate::ids::RunId;

use super::{Ledger, Result};

/// One holdout a verification ran, as the ledger keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldoutOutcomeRow<'a> {
    /// `attempt` or `integration`: the ordinal alone repeats across them.
    pub label_kind: &'a str,
    pub attempt_index: u32,
    pub name: &'a str,
    /// `passed`, `failed`, `did_not_build` or `timed_out`.
    pub outcome: &'a str,
    /// Every profile check passed on that candidate (no failed check, no
    /// profile gap).
    pub profile_green: bool,
}

impl Ledger {
    /// Record one holdout outcome; the same (run, label kind, attempt, name) again
    /// replaces the row, so a re-entered verification is safe.
    pub fn record_holdout_outcome(
        &self,
        run_id: &RunId,
        row: &HoldoutOutcomeRow<'_>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO holdout_outcomes
                (run_id, label_kind, attempt_index, name, outcome, profile_green)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run_id.as_str(),
                row.label_kind,
                i64::from(row.attempt_index),
                row.name,
                row.outcome,
                i64::from(row.profile_green)
            ],
        )?;
        Ok(())
    }

    /// Record that a dispatch's `tool` named the holdout location `marker`.
    /// Recording the same triple again changes nothing.
    pub fn record_holdout_exposure(
        &self,
        run_id: &RunId,
        dispatch_id: &str,
        tool: &str,
        marker: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO holdout_exposures (run_id, dispatch_id, tool, marker)
             VALUES (?1, ?2, ?3, ?4)",
            params![run_id.as_str(), dispatch_id, tool, marker],
        )?;
        Ok(())
    }

    /// The dispatches of the run and of its package runs that name a native
    /// agent, as (dispatch id, agent id), oldest first: the ones that have a
    /// transcript to read.
    // holds-until: decomposition is one level deep (a package run does not
    // decompose again), so `parent_run = ?1` reaches every worker; becomes a
    // recursive CTE if packages ever decompose.
    pub fn agent_dispatches_of_tree(&self, run_id: &RunId) -> Result<Vec<(String, String)>> {
        let mut statement = self.conn.prepare(
            "SELECT dispatch_id, agent_id FROM dispatches
              WHERE (run_id = ?1 OR run_id IN (SELECT id FROM runs WHERE parent_run = ?1))
                AND agent_id IS NOT NULL
              ORDER BY created_at, dispatch_id",
        )?;
        let rows = statement
            .query_map([run_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Whether the run named at least one holdout (it has an outcome row).
    pub fn run_named_a_holdout(&self, run_id: &RunId) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM holdout_outcomes WHERE run_id = ?1",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Whether the run has an outcome row that failed (a holdout that did
    /// not build is no catch) while the profile's checks were green on that candidate: the
    /// acceptance on evidence the worker controlled that a holdout stopped.
    pub fn run_holdout_caught(&self, run_id: &RunId) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM holdout_outcomes
              WHERE run_id = ?1 AND profile_green = 1 AND outcome = 'failed'",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// The distinct markers a run's workers named, ordered.
    pub fn run_exposure_markers(&self, run_id: &RunId) -> Result<Vec<String>> {
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT marker FROM holdout_exposures WHERE run_id = ?1 ORDER BY marker",
        )?;
        let markers = statement
            .query_map([run_id.as_str()], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(markers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TaskId;

    #[test]
    fn a_timed_out_holdout_on_green_checks_is_not_a_catch() {
        let dir = crate::test_support::short_temp_dir("ho-timeout");
        let ledger = Ledger::open(&dir.to_path_buf().join("ledger.sqlite")).expect("ledger");
        let run = RunId::from_stored("run-timeout");
        ledger
            .insert_run(
                &run,
                "/repo",
                None,
                &TaskId::from_stored("run-timeout"),
                "rk",
            )
            .expect("run");
        ledger
            .record_holdout_outcome(
                &run,
                &HoldoutOutcomeRow {
                    label_kind: "attempt",
                    attempt_index: 1,
                    name: "slow",
                    outcome: "timed_out",
                    profile_green: true,
                },
            )
            .expect("outcome");
        assert!(ledger.run_named_a_holdout(&run).expect("named"));
        assert!(!ledger.run_holdout_caught(&run).expect("caught"));
    }
}
