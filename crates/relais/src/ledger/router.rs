//! The session router's tables (ledger step v20, SPEC §30): what `relais
//! native router-observe` writes and what `router-state` and `relais
//! report` read back. Mechanism only: what a record means, and how one
//! outcome ranks against another, is `crate::router`'s; a task row arrives
//! here carrying its rank.

use rusqlite::{params, OptionalExtension};

use super::{Ledger, Result};

/// The classifier's reading of a task, as the plugin sent it.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterClass {
    pub kind: String,
    pub difficulty: u8,
    pub scope: String,
    pub uncertainty: String,
    pub verifiable: bool,
    pub confidence: f64,
}

/// One routing decision, keyed `(session, task_id, turn_id, agent_id)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterDecisionRow {
    pub task_id: String,
    pub turn_id: Option<String>,
    pub agent_id: Option<String>,
    pub relation: String,
    pub class: Option<RouterClass>,
    pub tier: String,
    pub model: String,
    pub effort: Option<String>,
    pub reason: String,
    pub mode_effective: String,
    pub holdout: bool,
    pub applied: bool,
    pub explored: bool,
    pub propensity: Option<f64>,
    pub draw: Option<f64>,
    pub would_pass_gate: bool,
    pub at: String,
}

/// One request's or one classifier call's tokens, keyed `(session,
/// turn_id, step, agent_id, source)`. Never a cost.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterUsageRow {
    pub task_id: String,
    pub turn_id: Option<String>,
    pub step: u32,
    pub agent_id: Option<String>,
    /// `step` or `classifier`.
    pub source: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub at: String,
}

/// One reassessment, keyed `(session, task_id, agent_id, event, at)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterReassessRow {
    pub task_id: String,
    pub agent_id: Option<String>,
    pub event: String,
    pub tier_from: String,
    pub tier_to: String,
    pub effort_to: Option<String>,
    pub escalating: bool,
    pub at: String,
}

/// One task, keyed `(session, task_id, agent_id)`. `outcome_rank` is the
/// caller's (`router::outcome::rank`): a later record replaces the stored
/// outcome only when its rank is strictly higher.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterTaskRow {
    pub task_id: String,
    pub agent_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub class: Option<RouterClass>,
    pub outcome: String,
    pub outcome_rank: u8,
    pub inferred: Vec<String>,
    pub escalations: u32,
    pub exhausted: bool,
    pub turns: u32,
    pub explicit_quote: Option<String>,
}

/// One record of a `router-observe` batch.
#[derive(Debug, Clone, PartialEq)]
pub enum RouterRecord {
    Decision(RouterDecisionRow),
    Usage(RouterUsageRow),
    Reassess(RouterReassessRow),
    Task(RouterTaskRow),
}

/// One `router_provenance` row: an envelope written, or an R3 result
/// recorded, with who wrote it from where.
#[derive(Debug, Clone, PartialEq)]
pub enum RouterProvenance {
    Envelope {
        by: String,
        epsilon_max: f64,
        source: String,
        at: String,
    },
    R3 {
        id: String,
        passed: bool,
        source: String,
        at: String,
    },
}

/// What `relais report` reads for its session-routing section: every
/// session with a task started in the window, with all of those sessions'
/// tasks, usage and decision flags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RouterWindow {
    pub tasks: Vec<(String, RouterTaskRow)>,
    pub usage: Vec<(String, RouterUsageRow)>,
    /// `(session, holdout, applied)` per decision.
    pub decisions: Vec<(String, bool, bool)>,
}

/// An absent id, as a key column stores it.
fn key(id: &Option<String>) -> &str {
    id.as_deref().unwrap_or("")
}

/// A key column read back: '' is absent.
fn unkey(id: String) -> Option<String> {
    (!id.is_empty()).then_some(id)
}

/// A class as its six columns, NULL when there is no class.
type ClassColumns<'a> = (
    Option<&'a str>,
    Option<i64>,
    Option<&'a str>,
    Option<&'a str>,
    Option<bool>,
    Option<f64>,
);

fn class_columns(class: &Option<RouterClass>) -> ClassColumns<'_> {
    match class {
        Some(c) => (
            Some(c.kind.as_str()),
            Some(i64::from(c.difficulty)),
            Some(c.scope.as_str()),
            Some(c.uncertainty.as_str()),
            Some(c.verifiable),
            Some(c.confidence),
        ),
        None => (None, None, None, None, None, None),
    }
}

/// The later of two optional timestamps (RFC 3339 in UTC compares as
/// text).
fn later(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if b > a { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// `incoming` folded onto `stored`: the outcome moves only up the rank,
/// counters only up, `exhausted` and the inferred signals only gain.
/// Order-independent, so a resend arriving late changes nothing.
fn merge_task(stored: RouterTaskRow, incoming: &RouterTaskRow) -> RouterTaskRow {
    let promote = incoming.outcome_rank > stored.outcome_rank;
    let mut inferred = stored.inferred;
    for signal in &incoming.inferred {
        if !inferred.contains(signal) {
            inferred.push(signal.clone());
        }
    }
    inferred.sort();
    RouterTaskRow {
        task_id: stored.task_id,
        agent_id: stored.agent_id,
        started_at: if incoming.started_at < stored.started_at {
            incoming.started_at.clone()
        } else {
            stored.started_at
        },
        ended_at: later(stored.ended_at, incoming.ended_at.clone()),
        class: incoming.class.clone().or(stored.class),
        outcome: if promote {
            incoming.outcome.clone()
        } else {
            stored.outcome
        },
        outcome_rank: stored.outcome_rank.max(incoming.outcome_rank),
        inferred,
        escalations: stored.escalations.max(incoming.escalations),
        exhausted: stored.exhausted || incoming.exhausted,
        turns: stored.turns.max(incoming.turns),
        explicit_quote: if promote {
            incoming.explicit_quote.clone().or(stored.explicit_quote)
        } else {
            stored.explicit_quote.or(incoming.explicit_quote.clone())
        },
    }
}

fn read_class(row: &rusqlite::Row<'_>, first: usize) -> rusqlite::Result<Option<RouterClass>> {
    let kind: Option<String> = row.get(first)?;
    Ok(match kind {
        None => None,
        Some(kind) => Some(RouterClass {
            kind,
            difficulty: row.get::<_, i64>(first + 1)?.clamp(0, 255) as u8,
            scope: row.get(first + 2)?,
            uncertainty: row.get(first + 3)?,
            verifiable: row.get(first + 4)?,
            confidence: row.get(first + 5)?,
        }),
    })
}

const TASK_COLUMNS: &str =
    "task_id, agent_id, started_at, ended_at, class_kind, difficulty, scope, \
     uncertainty, verifiable, confidence, outcome, outcome_rank, inferred_json, escalations, \
     exhausted, turns, explicit_quote";

fn read_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<RouterTaskRow> {
    read_task_at(row, 0)
}

impl Ledger {
    /// Every record of one `router-observe` call, in ONE transaction: all
    /// land or none do. Each is an upsert on the contract's key, so a
    /// batch sent twice records nothing new.
    pub fn record_router_batch(&self, session: &str, records: &[RouterRecord]) -> Result<()> {
        let now = self.now();
        let tx = self.write_tx()?;
        for record in records {
            match record {
                RouterRecord::Decision(d) => {
                    let (kind, difficulty, scope, uncertainty, verifiable, confidence) =
                        class_columns(&d.class);
                    tx.execute(
                        "INSERT INTO router_decisions
                            (session, task_id, turn_id, agent_id, relation, class_kind, difficulty,
                             scope, uncertainty, verifiable, confidence, tier, model, effort, reason,
                             mode_effective, holdout, applied, explored, propensity, draw,
                             would_pass_gate, at, recorded_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                                 ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
                         ON CONFLICT (session, task_id, turn_id, agent_id) DO UPDATE SET
                            relation = excluded.relation, class_kind = excluded.class_kind,
                            difficulty = excluded.difficulty, scope = excluded.scope,
                            uncertainty = excluded.uncertainty, verifiable = excluded.verifiable,
                            confidence = excluded.confidence, tier = excluded.tier,
                            model = excluded.model, effort = excluded.effort,
                            reason = excluded.reason, mode_effective = excluded.mode_effective,
                            holdout = excluded.holdout, applied = excluded.applied,
                            explored = excluded.explored, propensity = excluded.propensity,
                            draw = excluded.draw, would_pass_gate = excluded.would_pass_gate,
                            at = excluded.at",
                        params![
                            session,
                            d.task_id,
                            key(&d.turn_id),
                            key(&d.agent_id),
                            d.relation,
                            kind,
                            difficulty,
                            scope,
                            uncertainty,
                            verifiable,
                            confidence,
                            d.tier,
                            d.model,
                            d.effort,
                            d.reason,
                            d.mode_effective,
                            d.holdout,
                            d.applied,
                            d.explored,
                            d.propensity,
                            d.draw,
                            d.would_pass_gate,
                            d.at,
                            now,
                        ],
                    )?;
                }
                RouterRecord::Usage(u) => {
                    tx.execute(
                        "INSERT INTO router_usage
                            (session, task_id, turn_id, step, agent_id, source, model,
                             input_tokens, output_tokens, cache_read_input_tokens,
                             cache_creation_input_tokens, at, recorded_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                         ON CONFLICT (session, turn_id, step, agent_id, source) DO UPDATE SET
                            task_id = excluded.task_id, model = excluded.model,
                            input_tokens = excluded.input_tokens,
                            output_tokens = excluded.output_tokens,
                            cache_read_input_tokens = excluded.cache_read_input_tokens,
                            cache_creation_input_tokens = excluded.cache_creation_input_tokens,
                            at = excluded.at",
                        params![
                            session,
                            u.task_id,
                            key(&u.turn_id),
                            u.step,
                            key(&u.agent_id),
                            u.source,
                            u.model,
                            saturating_i64(u.input_tokens),
                            saturating_i64(u.output_tokens),
                            saturating_i64(u.cache_read_input_tokens),
                            saturating_i64(u.cache_creation_input_tokens),
                            u.at,
                            now,
                        ],
                    )?;
                }
                RouterRecord::Reassess(r) => {
                    tx.execute(
                        "INSERT INTO router_reassess
                            (session, task_id, agent_id, event, tier_from, tier_to, effort_to,
                             escalating, at, recorded_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                         ON CONFLICT (session, task_id, agent_id, event, at) DO NOTHING",
                        params![
                            session,
                            r.task_id,
                            key(&r.agent_id),
                            r.event,
                            r.tier_from,
                            r.tier_to,
                            r.effort_to,
                            r.escalating,
                            r.at,
                            now,
                        ],
                    )?;
                }
                RouterRecord::Task(t) => {
                    let stored = tx
                        .query_row(
                            &format!(
                                "SELECT {TASK_COLUMNS} FROM router_tasks
                                  WHERE session = ?1 AND task_id = ?2 AND agent_id = ?3"
                            ),
                            params![session, t.task_id, key(&t.agent_id)],
                            read_task,
                        )
                        .optional()?;
                    let merged = match stored {
                        Some(stored) => merge_task(stored, t),
                        None => t.clone(),
                    };
                    let (kind, difficulty, scope, uncertainty, verifiable, confidence) =
                        class_columns(&merged.class);
                    let inferred = serde_json::to_string(&merged.inferred)
                        .expect("a list of strings serializes");
                    tx.execute(
                        "INSERT INTO router_tasks
                            (session, task_id, agent_id, started_at, ended_at, class_kind,
                             difficulty, scope, uncertainty, verifiable, confidence, outcome,
                             outcome_rank, inferred_json, escalations, exhausted, turns,
                             explicit_quote, recorded_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                                 ?15, ?16, ?17, ?18, ?19, ?19)
                         ON CONFLICT (session, task_id, agent_id) DO UPDATE SET
                            started_at = excluded.started_at, ended_at = excluded.ended_at,
                            class_kind = excluded.class_kind, difficulty = excluded.difficulty,
                            scope = excluded.scope, uncertainty = excluded.uncertainty,
                            verifiable = excluded.verifiable, confidence = excluded.confidence,
                            outcome = excluded.outcome, outcome_rank = excluded.outcome_rank,
                            inferred_json = excluded.inferred_json,
                            escalations = excluded.escalations, exhausted = excluded.exhausted,
                            turns = excluded.turns, explicit_quote = excluded.explicit_quote,
                            updated_at = excluded.updated_at",
                        params![
                            session,
                            merged.task_id,
                            key(&merged.agent_id),
                            merged.started_at,
                            merged.ended_at,
                            kind,
                            difficulty,
                            scope,
                            uncertainty,
                            verifiable,
                            confidence,
                            merged.outcome,
                            merged.outcome_rank,
                            inferred,
                            merged.escalations,
                            merged.exhausted,
                            merged.turns,
                            merged.explicit_quote,
                            now,
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// One task as stored, for tests and diagnostics.
    pub fn router_task(
        &self,
        session: &str,
        task_id: &str,
        agent_id: Option<&str>,
    ) -> Result<Option<RouterTaskRow>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {TASK_COLUMNS} FROM router_tasks
                      WHERE session = ?1 AND task_id = ?2 AND agent_id = ?3"
                ),
                params![session, task_id, agent_id.unwrap_or("")],
                read_task,
            )
            .optional()?)
    }

    /// How many rows each router table holds, in table order: decisions,
    /// usage, reassess, tasks.
    pub fn router_row_counts(&self) -> Result<[u64; 4]> {
        let mut counts = [0u64; 4];
        for (slot, table) in counts.iter_mut().zip([
            "router_decisions",
            "router_usage",
            "router_reassess",
            "router_tasks",
        ]) {
            let n: i64 =
                self.conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
            *slot = n.max(0) as u64;
        }
        Ok(counts)
    }

    /// `(kind, difficulty, total tokens)` of every task whose outcome is
    /// one of `outcomes` and that has a class: what the per-class median
    /// priors are computed from.
    pub fn router_task_token_totals(&self, outcomes: &[&str]) -> Result<Vec<(String, u8, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.class_kind, t.difficulty, t.outcome, COALESCE(k.total_tokens, 0)
               FROM router_tasks t
               LEFT JOIN router_task_tokens k
                 ON k.session = t.session AND k.task_id = t.task_id AND k.agent_id = t.agent_id
              WHERE t.class_kind IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (kind, difficulty, outcome, total) = row?;
            if outcomes.contains(&outcome.as_str()) {
                out.push((kind, difficulty.clamp(0, 255) as u8, total.max(0) as u64));
            }
        }
        Ok(out)
    }

    /// Record an envelope write or an R3 result.
    pub fn record_router_provenance(&self, row: &RouterProvenance) -> Result<()> {
        match row {
            RouterProvenance::Envelope {
                by,
                epsilon_max,
                source,
                at,
            } => self.conn.execute(
                "INSERT INTO router_provenance (kind, granted_by, epsilon_max, source, at)
                 VALUES ('envelope', ?1, ?2, ?3, ?4)",
                params![by, epsilon_max, source, at],
            )?,
            RouterProvenance::R3 {
                id,
                passed,
                source,
                at,
            } => self.conn.execute(
                "INSERT INTO router_provenance (kind, r3_id, passed, source, at)
                 VALUES ('r3', ?1, ?2, ?3, ?4)",
                params![id, passed, source, at],
            )?,
        };
        Ok(())
    }

    /// The latest R3 row, which alone decides whether R3 has passed: a
    /// failure recorded after a pass withdraws it.
    pub fn latest_router_r3(&self) -> Result<Option<RouterProvenance>> {
        Ok(self
            .conn
            .query_row(
                "SELECT r3_id, passed, source, at FROM router_provenance
                  WHERE kind = 'r3' ORDER BY id DESC LIMIT 1",
                [],
                |row| {
                    Ok(RouterProvenance::R3 {
                        id: row.get(0)?,
                        passed: row.get(1)?,
                        source: row.get(2)?,
                        at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Every provenance row, oldest first.
    pub fn router_provenance(&self) -> Result<Vec<RouterProvenance>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, r3_id, passed, granted_by, epsilon_max, source, at
               FROM router_provenance ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            let kind: String = row.get(0)?;
            Ok(if kind == "r3" {
                RouterProvenance::R3 {
                    id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    passed: row.get::<_, Option<bool>>(2)?.unwrap_or(false),
                    source: row.get(5)?,
                    at: row.get(6)?,
                }
            } else {
                RouterProvenance::Envelope {
                    by: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    epsilon_max: row.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    source: row.get(5)?,
                    at: row.get(6)?,
                }
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The sessions with a task started at or after `since`, with all of
    /// their tasks, usage and decision flags.
    pub fn router_window(&self, since: &str) -> Result<RouterWindow> {
        let in_window =
            "session IN (SELECT DISTINCT session FROM router_tasks WHERE started_at >= ?1)";
        let mut window = RouterWindow::default();
        {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT session, {TASK_COLUMNS} FROM router_tasks WHERE {in_window}
                  ORDER BY session, started_at, task_id, agent_id"
            ))?;
            let rows = stmt.query_map([since], |row| {
                Ok((row.get::<_, String>(0)?, read_task_at(row, 1)?))
            })?;
            for row in rows {
                window.tasks.push(row?);
            }
        }
        {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT session, task_id, turn_id, step, agent_id, source, model, input_tokens,
                        output_tokens, cache_read_input_tokens, cache_creation_input_tokens, at
                   FROM router_usage WHERE {in_window}
                  ORDER BY session, agent_id, at, turn_id, step"
            ))?;
            let rows = stmt.query_map([since], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    RouterUsageRow {
                        task_id: row.get(1)?,
                        turn_id: unkey(row.get(2)?),
                        step: row.get::<_, i64>(3)?.max(0) as u32,
                        agent_id: unkey(row.get(4)?),
                        source: row.get(5)?,
                        model: row.get(6)?,
                        input_tokens: row.get::<_, i64>(7)?.max(0) as u64,
                        output_tokens: row.get::<_, i64>(8)?.max(0) as u64,
                        cache_read_input_tokens: row.get::<_, i64>(9)?.max(0) as u64,
                        cache_creation_input_tokens: row.get::<_, i64>(10)?.max(0) as u64,
                        at: row.get(11)?,
                    },
                ))
            })?;
            for row in rows {
                window.usage.push(row?);
            }
        }
        {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT session, holdout, applied FROM router_decisions WHERE {in_window}
                  ORDER BY session"
            ))?;
            let rows = stmt.query_map([since], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?;
            for row in rows {
                window.decisions.push(row?);
            }
        }
        Ok(window)
    }
}

/// [`read_task`] for a row whose task columns start at `offset`.
fn read_task_at(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<RouterTaskRow> {
    let inferred: String = row.get(offset + 12)?;
    Ok(RouterTaskRow {
        task_id: row.get(offset)?,
        agent_id: unkey(row.get(offset + 1)?),
        started_at: row.get(offset + 2)?,
        ended_at: row.get(offset + 3)?,
        class: read_class(row, offset + 4)?,
        outcome: row.get(offset + 10)?,
        outcome_rank: row.get::<_, i64>(offset + 11)?.clamp(0, 255) as u8,
        // A row this binary wrote always parses; one that does not is a
        // corrupt row, reported as such rather than read as "no signals".
        inferred: serde_json::from_str(&inferred).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + 12,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })?,
        escalations: row.get::<_, i64>(offset + 13)?.max(0) as u32,
        exhausted: row.get(offset + 14)?,
        turns: row.get::<_, i64>(offset + 15)?.max(0) as u32,
        explicit_quote: row.get(offset + 16)?,
    })
}

/// Token counts are `u64` on the wire and INTEGER (i64) in SQLite.
fn saturating_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> (Ledger, crate::test_support::TempDir) {
        let dir = crate::test_support::temp_dir("router-ledger");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        (ledger, dir)
    }

    fn task(outcome: &str, rank: u8) -> RouterTaskRow {
        RouterTaskRow {
            task_id: "t1".into(),
            agent_id: None,
            started_at: "2026-10-07T10:00:00Z".into(),
            ended_at: None,
            class: None,
            outcome: outcome.into(),
            outcome_rank: rank,
            inferred: vec![],
            escalations: 0,
            exhausted: false,
            turns: 1,
            explicit_quote: None,
        }
    }

    fn usage(step: u32) -> RouterUsageRow {
        RouterUsageRow {
            task_id: "t1".into(),
            turn_id: None,
            step,
            agent_id: None,
            source: "step".into(),
            model: "claude-haiku-5-5".into(),
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            at: "2026-10-07T10:00:01Z".into(),
        }
    }

    #[test]
    fn a_batch_sent_twice_records_nothing_new_even_with_null_key_parts() {
        let (ledger, _dir) = ledger();
        let batch = vec![
            RouterRecord::Usage(usage(0)),
            RouterRecord::Usage(usage(1)),
            RouterRecord::Task(task("unknown", 0)),
        ];
        ledger.record_router_batch("s1", &batch).unwrap();
        ledger.record_router_batch("s1", &batch).unwrap();
        assert_eq!(ledger.router_row_counts().unwrap(), [0, 2, 0, 1]);
    }

    #[test]
    fn a_task_outcome_only_moves_up_the_rank() {
        let (ledger, _dir) = ledger();
        let write = |outcome: &str, rank: u8| {
            ledger
                .record_router_batch("s", &[RouterRecord::Task(task(outcome, rank))])
                .unwrap();
            ledger
                .router_task("s", "t1", None)
                .unwrap()
                .unwrap()
                .outcome
        };
        assert_eq!(write("completed_verified", 1), "completed_verified");
        assert_eq!(write("unknown", 0), "completed_verified");
        assert_eq!(write("completed_accepted", 1), "completed_verified");
        assert_eq!(write("corrected", 2), "corrected");
        assert_eq!(write("completed_verified", 1), "corrected");
        assert_eq!(write("unknown", 0), "corrected");
    }

    #[test]
    fn task_counters_and_signals_only_gain() {
        let stored = RouterTaskRow {
            escalations: 2,
            turns: 5,
            exhausted: true,
            inferred: vec!["aborted".into()],
            ended_at: Some("2026-10-07T11:00:00Z".into()),
            ..task("unknown", 0)
        };
        let incoming = RouterTaskRow {
            inferred: vec!["respawned".into()],
            ..task("unknown", 0)
        };
        let merged = merge_task(stored, &incoming);
        assert_eq!(merged.escalations, 2);
        assert_eq!(merged.turns, 5);
        assert!(merged.exhausted);
        assert_eq!(merged.inferred, vec!["aborted", "respawned"]);
        assert_eq!(merged.ended_at.as_deref(), Some("2026-10-07T11:00:00Z"));
    }

    #[test]
    fn the_latest_r3_row_decides() {
        let (ledger, _dir) = ledger();
        assert_eq!(ledger.latest_router_r3().unwrap(), None);
        for passed in [true, false] {
            ledger
                .record_router_provenance(&RouterProvenance::R3 {
                    id: "r3-a".into(),
                    passed,
                    source: "cli (unattributed)".into(),
                    at: "2026-10-07T00:00:00Z".into(),
                })
                .unwrap();
        }
        assert!(matches!(
            ledger.latest_router_r3().unwrap(),
            Some(RouterProvenance::R3 { passed: false, .. })
        ));
    }
}
