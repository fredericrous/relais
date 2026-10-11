//! The reviewer outcomes table (ledger step v26): what `relais native
//! review-outcome` writes and `relais report` reads back. Mechanism only:
//! how a review earns its label is `crate::review_outcome`'s; a row
//! arrives here already labelled.

use std::collections::BTreeMap;

use rusqlite::{params, OptionalExtension};

use super::{Ledger, Result};

/// One labelled review, keyed by the reviewer's agent id.
#[derive(Debug, Clone, PartialEq)]
pub struct ReviewOutcomeRow {
    pub agent_id: String,
    pub plan_path: String,
    pub role: String,
    /// The stratum the review counts in; the role, for now.
    pub class: String,
    pub round: u32,
    pub body_sha: String,
    pub model: Option<String>,
    pub tokens: Option<u64>,
    pub seconds: Option<f64>,
    pub verdict: Option<String>,
    /// `clean`, `miss` or `unknown`.
    pub label: String,
    /// Why a label is `unknown`.
    pub label_reason: Option<String>,
    /// The router's task for this agent, when it has one.
    pub router_task_id: Option<String>,
    /// Unix seconds, as the ledger file stamped it.
    pub recorded_at: i64,
}

/// Router tasks settled by a fallback subagent's merge row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeTotals {
    pub by_subagent: u64,
    pub verified: u64,
}

/// One class's labelled reviews and the tokens they spent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReviewClassTotals {
    pub class: String,
    pub clean: u64,
    pub miss: u64,
    pub unknown: u64,
    /// Tokens by model; a review recorded without a model counts under
    /// `unknown`.
    pub tokens_by_model: BTreeMap<String, u64>,
}

impl Ledger {
    /// Every row, in ONE transaction, each an upsert on the agent id: a
    /// ledger ingested twice records nothing new. `labelled_at` moves only
    /// when the label or its reason does.
    pub fn record_review_outcomes(&self, rows: &[ReviewOutcomeRow]) -> Result<()> {
        let now = self.now();
        let tx = self.write_tx()?;
        for row in rows {
            tx.execute(
                "INSERT INTO review_outcomes
                    (agent_id, plan_path, role, class, round, body_sha, model, tokens, seconds,
                     verdict, label, label_reason, router_task_id, recorded_at, labelled_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                 ON CONFLICT(agent_id) DO UPDATE SET
                    plan_path = excluded.plan_path,
                    role = excluded.role,
                    class = excluded.class,
                    round = excluded.round,
                    body_sha = excluded.body_sha,
                    model = excluded.model,
                    tokens = excluded.tokens,
                    seconds = excluded.seconds,
                    verdict = excluded.verdict,
                    label = excluded.label,
                    label_reason = excluded.label_reason,
                    router_task_id = excluded.router_task_id,
                    recorded_at = excluded.recorded_at,
                    labelled_at = CASE
                        WHEN review_outcomes.label IS excluded.label
                         AND review_outcomes.label_reason IS excluded.label_reason
                        THEN review_outcomes.labelled_at
                        ELSE excluded.labelled_at
                    END",
                params![
                    row.agent_id,
                    row.plan_path,
                    row.role,
                    row.class,
                    row.round,
                    row.body_sha,
                    row.model,
                    row.tokens.map(super::router::saturating_i64),
                    row.seconds,
                    row.verdict,
                    row.label,
                    row.label_reason,
                    row.router_task_id,
                    row.recorded_at,
                    now,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The router task of a subagent, by the agent id both sides carry;
    /// the latest when several sessions reused it.
    pub fn router_task_for_agent(&self, agent_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT task_id FROM router_tasks WHERE agent_id = ?1
                  ORDER BY started_at DESC LIMIT 1",
                params![agent_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// A fallback subagent's merge outcome, over its router task: written
    /// only where the stored outcome is still `unknown` and the new rank is
    /// higher, with `outcome_source = 'merge'`, so a second ingest changes
    /// nothing. The `router-observe` statement is not reused: it keys on
    /// the session, which the amont-agent ledger does not carry, and names
    /// the parent as its source. `outcome` and `rank` are the wire
    /// contract's (`router::outcome::TaskOutcome`), resolved by the caller:
    /// the ledger does not depend on the router. Returns whether a row
    /// changed.
    pub fn record_merge_outcome(
        &self,
        task_id: &str,
        agent_id: &str,
        outcome: &str,
        rank: u8,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE router_tasks
                SET outcome = ?1, outcome_rank = ?2, outcome_source = 'merge',
                    updated_at = ?3
              WHERE task_id = ?4 AND agent_id = ?5
                AND outcome = 'unknown' AND outcome_rank < ?2",
            params![outcome, i64::from(rank), self.now(), task_id, agent_id,],
        )?;
        Ok(changed > 0)
    }

    /// How many router tasks a fallback subagent's merge row settled, and
    /// how many of those were verified.
    pub fn merge_outcome_totals(&self) -> Result<MergeTotals> {
        let (by_subagent, verified) = self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN outcome = 'completed_verified' THEN 1 ELSE 0 END), 0)
               FROM router_tasks WHERE outcome_source = 'merge'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        Ok(MergeTotals {
            by_subagent: by_subagent.max(0) as u64,
            verified: verified.max(0) as u64,
        })
    }

    /// Every class's label counts and token totals, by class name.
    pub fn review_class_totals(&self) -> Result<Vec<ReviewClassTotals>> {
        let mut by_class: BTreeMap<String, ReviewClassTotals> = BTreeMap::new();
        let mut counts = self
            .conn
            .prepare("SELECT class, label, COUNT(*) FROM review_outcomes GROUP BY class, label")?;
        let rows = counts.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        })?;
        for row in rows {
            let (class, label, n) = row?;
            let totals = by_class
                .entry(class.clone())
                .or_insert_with(|| ReviewClassTotals {
                    class,
                    ..Default::default()
                });
            match label.as_str() {
                "clean" => totals.clean += n,
                "miss" => totals.miss += n,
                _ => totals.unknown += n,
            }
        }
        let mut tokens = self.conn.prepare(
            "SELECT class, COALESCE(model, 'unknown'), COALESCE(SUM(tokens), 0)
               FROM review_outcomes GROUP BY class, COALESCE(model, 'unknown')",
        )?;
        let rows = tokens.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        })?;
        for row in rows {
            let (class, model, total) = row?;
            if let Some(totals) = by_class.get_mut(&class) {
                totals.tokens_by_model.insert(model, total);
            }
        }
        Ok(by_class.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: &str, label: &str, model: Option<&str>, tokens: u64) -> ReviewOutcomeRow {
        ReviewOutcomeRow {
            agent_id: agent.into(),
            plan_path: "/p.md".into(),
            role: "backend".into(),
            class: "backend".into(),
            round: 1,
            body_sha: "ab".into(),
            model: model.map(str::to_string),
            tokens: Some(tokens),
            seconds: Some(1.5),
            verdict: None,
            label: label.into(),
            label_reason: (label == "unknown").then(|| "not_pushed".to_string()),
            router_task_id: None,
            recorded_at: 1,
        }
    }

    fn ledger() -> (Ledger, crate::test_support::TempDir) {
        let dir = crate::test_support::temp_dir("review-ledger");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        (ledger, dir)
    }

    #[test]
    fn rows_upsert_by_agent_id_and_totals_count_by_class() {
        let (ledger, _dir) = ledger();
        let rows = vec![
            row("a1", "clean", Some("m1"), 100),
            row("a2", "miss", Some("m1"), 50),
            row("a3", "unknown", None, 7),
        ];
        ledger.record_review_outcomes(&rows).expect("first");
        ledger.record_review_outcomes(&rows).expect("second");
        let totals = ledger.review_class_totals().expect("totals");
        assert_eq!(totals.len(), 1);
        let backend = &totals[0];
        assert_eq!(
            (backend.clean, backend.miss, backend.unknown),
            (1, 1, 1),
            "{backend:?}"
        );
        assert_eq!(backend.tokens_by_model["m1"], 150);
        assert_eq!(backend.tokens_by_model["unknown"], 7);
    }

    #[test]
    fn a_relabelled_review_replaces_its_row() {
        let (ledger, _dir) = ledger();
        ledger
            .record_review_outcomes(&[row("a1", "unknown", None, 1)])
            .expect("first");
        ledger
            .record_review_outcomes(&[row("a1", "miss", None, 1)])
            .expect("second");
        let totals = ledger.review_class_totals().expect("totals");
        assert_eq!((totals[0].miss, totals[0].unknown), (1, 0));
    }

    #[test]
    fn the_router_task_of_an_agent_is_absent_without_a_row() {
        let (ledger, _dir) = ledger();
        assert_eq!(ledger.router_task_for_agent("nobody").expect("read"), None);
    }
}
