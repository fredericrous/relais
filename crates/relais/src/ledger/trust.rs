//! Repository trust: grants, declarations and the status that admitted a run.

use rusqlite::{params, OptionalExtension};

use crate::ids::RunId;

use super::{Ledger, Result};

/// One row of `repo_declarations`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDeclaration {
    pub declaration_hash: String,
    pub declaration_json: String,
    pub seen_at: String,
}

/// One row of `trust_grants`: what [`Ledger::record_trust_grant`] writes.
#[derive(Debug, Clone, Copy)]
pub struct TrustGrantRecord<'a> {
    pub grant_key: &'a str,
    /// [`crate::policy::repo_key`] of the repository identity.
    pub repo_key: &'a str,
    /// The identity as a person reads it.
    pub repo: &'a str,
    pub reviewed_by: &'a str,
    /// `cli` or `plugin`: who asked the person.
    pub source: &'a str,
    pub steps_json: &'a str,
}

impl Ledger {
    /// Record a trust grant `relais trust grant` just wrote to
    /// machine.toml (SPEC §5). `steps_json` is the JSON list of the steps
    /// the grant authorized, as `trust show --json` printed them.
    pub fn record_trust_grant(&self, grant: &TrustGrantRecord<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO trust_grants
                (grant_key, repo_key, repo, reviewed_by, source, steps_json, granted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                grant.grant_key,
                grant.repo_key,
                grant.repo,
                grant.reviewed_by,
                grant.source,
                grant.steps_json,
                self.now()
            ],
        )?;
        Ok(())
    }

    /// The steps the latest grant for this repository authorized, as
    /// recorded — `None` when this repository was never granted from a
    /// command (a hand-pasted grant leaves no row).
    pub fn latest_trust_grant_steps(&self, repo_key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT steps_json FROM trust_grants WHERE repo_key = ?1 ORDER BY id DESC LIMIT 1",
                [repo_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// The command declaration `repo_key` last ran under (ADR-0007):
    /// `None` when none was ever recorded.
    pub fn repo_declaration(&self, repo_key: &str) -> Result<Option<StoredDeclaration>> {
        Ok(self
            .conn
            .query_row(
                "SELECT declaration_hash, declaration_json, seen_at
                 FROM repo_declarations WHERE repo_key = ?1",
                [repo_key],
                |row| {
                    Ok(StoredDeclaration {
                        declaration_hash: row.get(0)?,
                        declaration_json: row.get(1)?,
                        seen_at: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Record the declaration `repo_key` runs under now, replacing the
    /// last one. An upsert: two runs recording at once both land, the
    /// later one stands, and neither fails.
    pub fn upsert_repo_declaration(
        &self,
        repo_key: &str,
        declaration_hash: &str,
        declaration_json: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO repo_declarations
                (repo_key, declaration_hash, declaration_json, seen_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(repo_key) DO UPDATE SET
                declaration_hash = excluded.declaration_hash,
                declaration_json = excluded.declaration_json,
                seen_at = excluded.seen_at",
            params![repo_key, declaration_hash, declaration_json, self.now()],
        )?;
        Ok(())
    }

    /// Which grant admitted a run (`repository` or `legacy_exact`).
    pub fn set_run_trust_status(&self, id: &RunId, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET trust_status = ?2 WHERE id = ?1",
            params![id.as_str(), status],
        )?;
        Ok(())
    }

    /// How many runs created at or after `since` (RFC3339) a legacy
    /// `[trust]` grant admitted: what `relais doctor` reports before the
    /// legacy grants can be retired.
    pub fn legacy_grant_runs_since(&self, since: &str) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM runs
             WHERE trust_status = 'legacy_exact' AND created_at >= ?1",
            [since],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
}
