//! Orchestrating sessions: predictions, features, per-session run lists and
//! orchestration usage.

use rusqlite::params;

use crate::ids::{DispatchId, RunId};
use crate::money::{CostCompleteness, MicroUsd};
use crate::orchestration::{Speed, TranscriptSource};

use super::{Ledger, Result, PROJECTED_STATUS};

/// One orchestrating-session API message, ready to persist: the pure
/// [`crate::orchestration::UsageRecord`] plus its price and the identity
/// the transcript itself does not carry (which session, which file).
#[derive(Debug, Clone, PartialEq)]
pub struct OrchestrationUsageRow {
    pub message_id: String,
    pub session_id: String,
    pub transcript: TranscriptSource,
    pub model: String,
    pub speed: Speed,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_5m_tokens: u64,
    pub cache_write_1h_tokens: u64,
    pub cost: Option<MicroUsd>,
    pub pricing_version: Option<String>,
    pub completeness: CostCompleteness,
    pub at: String,
}

impl Ledger {
    /// What a learned artifact estimated for a run at routing time, so a
    /// report can compare the estimate with what happened (SPEC §16).
    pub fn record_prediction(
        &self,
        run_id: &RunId,
        artifact_id: &str,
        input_hash: &str,
        result: &serde_json::Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO predictions (run_id, artifact_id, input_hash, result_json, at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run_id.as_str(),
                artifact_id,
                input_hash,
                serde_json::to_string(result).expect("prediction serializes"),
                self.now()
            ],
        )?;
        Ok(())
    }

    pub fn record_features(
        &self,
        dispatch_id: &DispatchId,
        features: &serde_json::Value,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO features (dispatch_id, feature_json, at) VALUES (?1, ?2, ?3)",
            params![
                dispatch_id.as_str(),
                serde_json::to_string(features).expect("features serialize"),
                self.now()
            ],
        )?;
        Ok(())
    }

    /// All root runs created at or after `since` (RFC3339), newest first.
    /// Package runs are folded into their root's cost and are not listed
    /// twice.
    pub fn runs_since(&self, since: &str) -> Result<Vec<(RunId, String, String, String)>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, repo_path, {PROJECTED_STATUS}, created_at FROM runs
                 WHERE created_at >= ?1 AND parent_run IS NULL ORDER BY created_at DESC"
        ))?;
        let rows = stmt.query_map([since], |row| {
            Ok((
                RunId::from_stored(row.get::<_, String>(0)?),
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// The runs started from one Claude Code session (`runs.root_session`),
    /// newest first, at most `limit`.
    pub fn runs_of_session(&self, session: &str, limit: usize) -> Result<Vec<RunId>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM runs WHERE root_session = ?1
                 ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![session, limit], |row| row.get::<_, String>(0))?;
        let ids = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ids
            .iter()
            .map(String::as_str)
            .map(RunId::from_stored)
            .collect())
    }

    /// Every session id that is the `root_session` of at least one run
    /// (SPEC §11): the population `relais usage import` and `relais
    /// report` import orchestration spend for. A worker's own session is
    /// never in here as a worker — only as whatever it happens to have
    /// been the root of, if anything — so a relais worker's spend, which
    /// is already paid for as a usage event of its run, is never
    /// double-counted as orchestration too.
    pub fn distinct_root_sessions(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT root_session FROM runs WHERE root_session IS NOT NULL")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// [`Self::distinct_root_sessions`], narrowed to sessions that are the
    /// `root_session` of a run created inside the window (SPEC §11): the
    /// same `created_at >= ?1` a report's own run population is windowed
    /// by ([`Self::runs_since`]). Without this, a session from long
    /// before the window — one already reported `unattributable` or
    /// `transcript missing` on every past `relais report` — would keep
    /// showing up in that count forever, on every window, rather than
    /// only the ones a person is actually looking at right now.
    pub fn distinct_root_sessions_since(&self, since: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT root_session FROM runs
                 WHERE root_session IS NOT NULL AND created_at >= ?1",
        )?;
        let rows = stmt.query_map([since], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// One orchestration API message, priced (or not) and attributed to
    /// a session and the transcript file it came from. Returns whether a
    /// row was inserted or grew. A message imported while it was still
    /// streaming carries a partial `output_tokens`; a later import of the
    /// finished message raises it (and its cost), and nothing else ever
    /// changes an existing row — re-importing the same content is a no-op.
    /// A message a relais run already booked (`native_usage_messages`) is
    /// never inserted, and the check is part of the insert itself: an
    /// import that read the message as unbooked, racing a native run that
    /// books it and deletes any imported copy in one transaction, cannot
    /// slip the row in after that delete. One statement is atomic, so the
    /// message is counted once in either order.
    pub fn record_orchestration_usage(&self, row: &OrchestrationUsageRow) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT INTO orchestration_usage
                (message_id, session_id, transcript, model, speed,
                 input_tokens, output_tokens, cache_read_tokens,
                 cache_write_5m_tokens, cache_write_1h_tokens,
                 cost_micros, pricing_version, completeness, at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
             WHERE NOT EXISTS
                 (SELECT 1 FROM native_usage_messages WHERE message_id = ?1)
             ON CONFLICT(message_id) DO UPDATE SET
                 output_tokens = excluded.output_tokens,
                 cost_micros = excluded.cost_micros,
                 pricing_version = excluded.pricing_version,
                 completeness = excluded.completeness
             WHERE excluded.output_tokens > orchestration_usage.output_tokens",
            params![
                row.message_id,
                row.session_id,
                row.transcript.label(),
                row.model,
                row.speed.as_str(),
                row.input_tokens as i64,
                row.output_tokens as i64,
                row.cache_read_tokens as i64,
                row.cache_write_5m_tokens as i64,
                row.cache_write_1h_tokens as i64,
                row.cost.map(MicroUsd::to_micros),
                row.pricing_version,
                serde_json::to_string(&row.completeness).expect("completeness serializes"),
                row.at,
            ],
        )?;
        Ok(inserted == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{run, task, temp_ledger};

    #[test]
    fn runs_of_session_returns_only_that_session_newest_first_capped() {
        let (ledger, dir) = temp_ledger();
        for i in 0..25 {
            ledger
                .insert_run(
                    &run(&format!("run-a{i:02}")),
                    "/repo",
                    Some("sess-a"),
                    &task("a"),
                    "rk",
                )
                .expect("run a");
        }
        ledger
            .insert_run(&run("run-b"), "/repo", Some("sess-b"), &task("b"), "rk")
            .expect("run b");
        ledger
            .insert_run(&run("run-n"), "/repo", None, &task("n"), "rk")
            .expect("run n");
        let runs = ledger.runs_of_session("sess-a", 20).expect("runs");
        assert_eq!(runs.len(), 20);
        assert_eq!(runs[0].as_str(), "run-a24");
        assert_eq!(runs[19].as_str(), "run-a05");
        assert!(runs.iter().all(|r| r.as_str().starts_with("run-a")));
        let b = ledger.runs_of_session("sess-b", 20).expect("runs");
        assert_eq!(b, vec![run("run-b")]);
        assert!(ledger
            .runs_of_session("nobody", 20)
            .expect("runs")
            .is_empty());
        assert_eq!(
            ledger.run_root_session(&run("run-b")).expect("session"),
            Some("sess-b".to_string())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A relais worker's own session is never in `distinct_root_sessions`
    /// as a worker — only ever as whatever it was the root of, if
    /// anything — so its spend, already paid for as a usage event of its
    /// run, is never imported and double-counted as orchestration too.
    #[test]
    fn distinct_root_sessions_only_names_sessions_that_started_a_run() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-a"), "/repo", Some("sess-root"), &task("a"), "rk")
            .expect("run with a root session");
        ledger
            .insert_run(&run("run-b"), "/repo", None, &task("b"), "rk")
            .expect("run with no root session");
        let sessions = ledger.distinct_root_sessions().expect("sessions");
        assert_eq!(sessions, vec!["sess-root".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A session from long before the report window must not keep
    /// showing up as `unattributable`/`transcript missing` on every
    /// later `relais report` (SPEC §11): `distinct_root_sessions_since`
    /// narrows those COUNTS to runs created inside the window. It never
    /// narrows what is imported — see `import_for_report` in main.rs.
    #[test]
    fn distinct_root_sessions_since_excludes_runs_created_before_the_window() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(
                &run("run-old"),
                "/repo",
                Some("sess-old"),
                &task("old"),
                "rk",
            )
            .expect("old run");
        ledger
            .insert_run(
                &run("run-new"),
                "/repo",
                Some("sess-new"),
                &task("new"),
                "rk",
            )
            .expect("new run");
        ledger
            .conn
            .execute(
                "UPDATE runs SET created_at = '2020-01-01T00:00:00+00:00' WHERE id = 'run-old'",
                [],
            )
            .expect("backdate the old run");
        let sessions = ledger
            .distinct_root_sessions_since("2025-01-01T00:00:00+00:00")
            .expect("sessions");
        assert_eq!(sessions, vec!["sess-new".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
