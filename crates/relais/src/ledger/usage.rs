//! Usage and cost accounting: usage events and their bookings, dispatch
//! intents, and the run, task and day cost queries over them.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::ids::{DispatchId, Pid, RunId, TaskId};
use crate::lifecycle::{RunPurpose, UsagePhase};
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::route::RoutedBy;

use super::{Ledger, LedgerError, Result, ADVISOR_EVENT_PREFIX};

/// The one `usage_events` insert: a duplicate event id is ignored.
fn insert_usage_event(conn: &Connection, event: &UsageEvent) -> Result<bool> {
    // A blank model is no model: store NULL, never "".
    let not_blank =
        |model: &Option<String>| model.as_ref().filter(|m| !m.trim().is_empty()).cloned();
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO usage_events
            (event_id, run_id, attempt_id, parent_event_id, model,
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
             cost_micros, cost_kind, completeness, inclusive, at,
             phase, duration_ms, requested_model, requested_effort, harness)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19)",
        params![
            event.event_id,
            event.run_id.as_str(),
            event.attempt_id,
            event.parent_event_id,
            not_blank(&event.model),
            event.input_tokens,
            event.output_tokens,
            event.cache_read_tokens,
            event.cache_write_tokens,
            event.cost.map(MicroUsd::to_micros),
            serde_json::to_string(&event.cost_kind).expect("cost kind serializes"),
            serde_json::to_string(&event.completeness).expect("completeness serializes"),
            event.inclusive,
            event.at,
            event.phase.map(UsagePhase::as_str),
            event.duration_ms,
            not_blank(&event.requested_model),
            event.requested_effort,
            event.harness,
        ],
    )?;
    Ok(inserted == 1)
}

/// Which booking of a dispatch's usage a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Booking {
    /// A turn while the dispatch runs (`native usage`): keyed
    /// `<dispatch>#<n>`, never by the bare id.
    Turn,
    /// The booking at the dispatch's end (`native stopped`, the runner):
    /// the one row keyed by the bare dispatch id, written even at a zero
    /// delta. Its presence is what says the dispatch reported
    /// (`run_dispatch_report_counts`), so a dispatch killed mid-flight
    /// with per-turn rows still reads as not reported.
    Final,
}

/// What a dispatch's usage rows already hold, summed.
struct BookedFigures {
    rows: i64,
    /// Whether the bare-id row, the final booking, is among them.
    has_final: bool,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    cost: i64,
}

/// What a dispatch's usage row needs besides the usage itself.
#[derive(Debug, Clone)]
pub struct DispatchBooking {
    pub run_id: RunId,
    pub attempt_id: Option<i64>,
    pub phase: Option<UsagePhase>,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub harness: Option<String>,
}

/// The part of a cumulative usage event that is new over `booked`: the
/// whole event when nothing is booked yet, otherwise each figure less what
/// is booked, clamped at zero, in a row named after the dispatch and the
/// rows before it. `None` when no figure grew.
fn delta_event(event: &UsageEvent, booked: &BookedFigures, event_id: String) -> Option<UsageEvent> {
    if booked.rows == 0 {
        return Some(UsageEvent {
            event_id,
            ..event.clone()
        });
    }
    let over = |total: Option<i64>, held: i64| total.map(|total| total.saturating_sub(held).max(0));
    let input = over(event.input_tokens, booked.input);
    let output = over(event.output_tokens, booked.output);
    let cache_read = over(event.cache_read_tokens, booked.cache_read);
    let cache_write = over(event.cache_write_tokens, booked.cache_write);
    let cost = over(event.cost.map(MicroUsd::to_micros), booked.cost);
    let grew = [input, output, cache_read, cache_write, cost]
        .iter()
        .any(|figure| figure.is_some_and(|figure| figure > 0));
    grew.then(|| UsageEvent {
        event_id,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        cost: cost.map(MicroUsd::from_micros),
        ..event.clone()
    })
}

/// Every usage event that an inclusive ancestor already accounts for,
/// at any depth (P11).
///
/// An inclusive event's total already contains its descendants (SPEC
/// §11), so adding those descendants would count the same money twice.
/// The dedup this replaced looked one generation up, and the comment
/// above `UsageEvent::inclusive` said it was "ready for nesting" — it
/// was not: a grandchild of an inclusive event was counted. The walk is
/// `UNION`, not `UNION ALL`, so a parent chain that somehow loops
/// terminates instead of running forever. `event_id` is UNIQUE across
/// the table, so the walk needs no run scoping to be unambiguous.
const COVERED_BY_AN_INCLUSIVE_PARENT: &str = "WITH RECURSIVE covered(event_id) AS (
        SELECT child.event_id
          FROM usage_events child
          JOIN usage_events parent ON parent.event_id = child.parent_event_id
         WHERE parent.inclusive = 1
         UNION
        SELECT child.event_id
          FROM usage_events child
          JOIN covered ON covered.event_id = child.parent_event_id
     )";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub event_id: String,
    pub run_id: RunId,
    pub attempt_id: Option<i64>,
    pub parent_event_id: Option<String>,
    pub model: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    /// `None` = the harness reported no cost. Unknown, never zero (SPEC
    /// §11): it is stored as NULL, left out of every sum, and makes the
    /// run's completeness `unknown`.
    pub cost: Option<MicroUsd>,
    pub cost_kind: CostKind,
    pub completeness: CostCompleteness,
    /// True when this event's total already includes its descendants:
    /// an inclusive parent is never added to its children (SPEC §11).
    /// Set by the adapter when the harness reports subagents in the
    /// session; no producer records the descendants as rows of their
    /// own today (native subagents are observed, not dispatched), so
    /// `parent_event_id` stays `None` until managed nested dispatch
    /// exists. The dedup in `run_cost` walks the whole ancestor chain,
    /// so it is ready for one at any depth.
    pub inclusive: bool,
    pub at: String,
    /// Which phase of the run spent this money — a worker attempt at one
    /// of its three kinds, the reviewer, the planner, or a package's
    /// integration. `None` for a row written before this column existed,
    /// never a guess (SPEC §11: absent is absent).
    pub phase: Option<UsagePhase>,
    /// The dispatch's own elapsed wall time, measured with a monotonic
    /// clock around the launch that produced this event — not derived
    /// from any stored timestamp, and not the run's own elapsed time.
    pub duration_ms: Option<i64>,
    /// The model the route asked for, beside `model` (what the harness
    /// reported running): a substitution is visible in the row, not only
    /// at the moment it happened.
    pub requested_model: Option<String>,
    /// The effort the route asked for.
    pub requested_effort: Option<String>,
    /// The harness identity the run probed.
    pub harness: Option<String>,
}

/// One phase's contribution to a run's cost (SPEC §11): what
/// [`Ledger::run_cost_by_phase`] returns, one entry per distinct phase
/// seen plus the `None` unattributed bucket when any row predates the
/// `phase` column.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseCost {
    pub phase: Option<UsagePhase>,
    pub cost: MicroUsd,
    pub completeness: CostCompleteness,
}

/// Add one usage row's cost and completeness into a phase's running
/// total, or open a new entry for a phase not yet seen. Unknown cost
/// (`cost_micros: None`) contributes nothing to the sum but still folds
/// its completeness in, the same null-skip `SUM` gives `run_own_cost`.
fn fold_phase_cost(
    totals: &mut Vec<(Option<UsagePhase>, MicroUsd, Vec<CostCompleteness>)>,
    phase: Option<UsagePhase>,
    cost_micros: Option<i64>,
    completeness: CostCompleteness,
) {
    let cost = cost_micros.map_or(MicroUsd::ZERO, MicroUsd::from_micros);
    match totals.iter_mut().find(|(p, ..)| *p == phase) {
        Some((_, total, completenesses)) => {
            if cost_micros.is_some() {
                *total = total.saturating_add(cost);
            }
            completenesses.push(completeness);
        }
        None => totals.push((phase, cost, vec![completeness])),
    }
}

/// A dispatch that claimed to launch and never recorded a terminal
/// state: what reconciliation looks at after a crash or restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDispatch {
    pub dispatch: DispatchId,
    pub run: RunId,
    /// The bound process, when one was recorded. `None` is "no pid on
    /// record" and nothing else — a value no process could have is a
    /// corrupt row and reported as one.
    pub pid: Option<Pid>,
    /// The session this dispatch was launched for, set the moment the
    /// process was bound (`attach_dispatch_process`) — the same fact a
    /// fresh admission already carries.
    pub session_id: Option<String>,
    /// What was reserved from the run's budget before launch (SPEC §12).
    pub reserve_micros: i64,
    /// `Some("managed_run")` on every row this crate ever wrote; `None`
    /// only for a row from before the migration that added this column —
    /// the signal `attribution` in `coordinator` reads to tell a fully
    /// recorded row from one it cannot.
    pub source: Option<String>,
    /// The agent bound to this dispatch, when the binding reached the
    /// ledger before this row stopped being live. Genuinely optional on
    /// every row, migration or not: nothing currently writes it while a
    /// dispatch is still `launched`.
    pub agent_id: Option<String>,
    /// The dispatch this one descends from. `None` on a post-migration
    /// row is a fact — this crate only ever writes root dispatches — not
    /// an unrecorded parent; a pre-migration row (`source` is `None`)
    /// carries no such fact either way.
    pub parent_dispatch: Option<String>,
    /// `Some(0)` on every post-migration row (this crate only ever
    /// writes root dispatches); `None` only on a pre-migration one.
    pub depth: Option<i64>,
}

impl Ledger {
    /// What one attempt cost, from the usage rows that name it and whose
    /// cost was reported. `None` is unknown — no such row — and never
    /// `$0.00`: an attempt that died before it could report cost something.
    /// An inclusive parent's covered children are not added again.
    pub fn attempt_cost(&self, attempt_id: i64) -> Result<Option<MicroUsd>> {
        let (reported, micros): (i64, i64) = self.conn.query_row(
            &format!(
                "{COVERED_BY_AN_INCLUSIVE_PARENT}
                 SELECT COUNT(cost_micros), COALESCE(SUM(cost_micros), 0) FROM usage_events
                  WHERE attempt_id = ?1
                    AND event_id NOT IN (SELECT event_id FROM covered)"
            ),
            [attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((reported > 0).then(|| MicroUsd::from_micros(micros)))
    }

    pub fn record_usage(&self, event: &UsageEvent) -> Result<bool> {
        insert_usage_event(&self.conn, event)
    }

    /// An advisor call's usage row, with the dispatch that booked it kept
    /// in `native_usage_messages` (keyed by the call's message id, the
    /// row's `event_id` without [`ADVISOR_EVENT_PREFIX`]). The row is no
    /// part of the dispatch's own `<id>#…` range, so its figures stay out
    /// of [`Self::record_usage_delta`]'s sum; the owner is what lets the
    /// dispatch's final booking repair the row's phase.
    pub fn record_advisor_usage(&self, event: &UsageEvent, dispatch_id: &str) -> Result<bool> {
        let tx = self.write_tx()?;
        let inserted = insert_usage_event(&tx, event)?;
        let message_id = event
            .event_id
            .strip_prefix(ADVISOR_EVENT_PREFIX)
            .unwrap_or(&event.event_id);
        tx.execute(
            "INSERT OR IGNORE INTO native_usage_messages
                (message_id, dispatch_id, run_id, recorded_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![message_id, dispatch_id, event.run_id.as_str(), event.at],
        )?;
        tx.execute(
            "DELETE FROM orchestration_usage WHERE message_id = ?1",
            params![message_id],
        )?;
        tx.commit()?;
        Ok(inserted)
    }

    /// A native attempt's usage event and the transcript messages it was
    /// booked for, in one transaction. Those messages belong to the run:
    /// an `orchestration_usage` row an earlier import made of any of them
    /// is deleted, so every message is counted once (SPEC §11).
    pub fn record_native_usage(&self, event: &UsageEvent, message_ids: &[String]) -> Result<bool> {
        let tx = self.write_tx()?;
        let inserted = insert_usage_event(&tx, event)?;
        for message_id in message_ids {
            tx.execute(
                "INSERT OR IGNORE INTO native_usage_messages
                    (message_id, dispatch_id, run_id, recorded_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![message_id, event.event_id, event.run_id.as_str(), event.at],
            )?;
            tx.execute(
                "DELETE FROM orchestration_usage WHERE message_id = ?1",
                params![message_id],
            )?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Books a dispatch's cumulative usage, `event`, as only what is new:
    /// the figure over what the dispatch's earlier usage rows already hold,
    /// each figure clamped at zero, so a lower or equal cumulative books
    /// nothing and a retry books once. The message ids go through the same
    /// dedup as [`Self::record_native_usage`]. All in one `BEGIN IMMEDIATE`
    /// transaction, so concurrent callers see each other's rows. Returns the
    /// event that was booked, `None` when nothing was new.
    pub fn record_usage_delta(
        &self,
        event: &UsageEvent,
        message_ids: &[String],
        booking: Booking,
    ) -> Result<Option<UsageEvent>> {
        let tx = self.write_tx()?;
        // The dispatch's rows: the bare id, and every `<id>#…` — a range
        // on the `event_id` key ('#' is 0x23, '$' the next byte), so the
        // lookup stays on the index however large the table grows.
        let booked = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(event_id = ?1), 0) > 0,
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(cost_micros), 0)
             FROM usage_events
             WHERE event_id = ?1 OR (event_id >= ?1 || '#' AND event_id < ?1 || '$')",
            params![event.event_id],
            |row| {
                Ok(BookedFigures {
                    rows: row.get(0)?,
                    has_final: row.get(1)?,
                    input: row.get(2)?,
                    output: row.get(3)?,
                    cache_read: row.get(4)?,
                    cache_write: row.get(5)?,
                    cost: row.get(6)?,
                })
            },
        )?;
        let numbered = format!("{}#{}", event.event_id, booked.rows);
        let delta = match booking {
            // The final row is written once, even at zero: it is the
            // dispatch's "reported" marker.
            Booking::Final if !booked.has_final => Some(
                delta_event(event, &booked, event.event_id.clone()).unwrap_or_else(|| UsageEvent {
                    event_id: event.event_id.clone(),
                    input_tokens: event.input_tokens.map(|_| 0),
                    output_tokens: event.output_tokens.map(|_| 0),
                    cache_read_tokens: event.cache_read_tokens.map(|_| 0),
                    cache_write_tokens: event.cache_write_tokens.map(|_| 0),
                    cost: event.cost.map(|_| MicroUsd::from_micros(0)),
                    ..event.clone()
                }),
            ),
            Booking::Final | Booking::Turn => delta_event(event, &booked, numbered),
        };
        if let Some(delta) = &delta {
            insert_usage_event(&tx, delta)?;
        }
        // One dispatch, one phase: the final booking knows the phase the
        // runner books it under (a planner's `planning`, a package child's
        // role), which the per-turn rows could only guess from the intent.
        if booking == Booking::Final {
            tx.execute(
                "UPDATE usage_events SET phase = ?2
                 WHERE event_id >= ?1 || '#' AND event_id < ?1 || '$'",
                params![event.event_id, event.phase.map(UsagePhase::as_str)],
            )?;
            // The advisor calls this dispatch booked are its rows too. The
            // keys come from the owner table, so `usage_events` is reached
            // through its `event_id` index rather than scanned.
            // holds-until: `native_usage_messages` has no index on
            // `dispatch_id`, so this subquery scans that table (one row per
            // booked message); add the index once a ledger holds enough
            // messages for a Final booking to show it.
            tx.execute(
                "UPDATE usage_events SET phase = ?2
                 WHERE event_id IN
                       (SELECT ?3 || message_id FROM native_usage_messages WHERE dispatch_id = ?1)",
                params![
                    event.event_id,
                    event.phase.map(UsagePhase::as_str),
                    ADVISOR_EVENT_PREFIX
                ],
            )?;
        }
        for message_id in message_ids {
            tx.execute(
                "INSERT OR IGNORE INTO native_usage_messages
                    (message_id, dispatch_id, run_id, recorded_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![message_id, event.event_id, event.run_id.as_str(), event.at],
            )?;
            tx.execute(
                "DELETE FROM orchestration_usage WHERE message_id = ?1",
                params![message_id],
            )?;
        }
        tx.commit()?;
        Ok(delta)
    }

    /// What a native dispatch's usage rows need to book a figure for it:
    /// its run, attempt, phase and requested model, from its intent.
    pub fn dispatch_booking(&self, dispatch_id: &str) -> Result<Option<DispatchBooking>> {
        let row = self
            .conn
            .query_row(
                "SELECT run_id, attempt_id, intent_json FROM dispatches WHERE dispatch_id = ?1",
                params![dispatch_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((run_id, attempt_id, intent)) = row else {
            return Ok(None);
        };
        let intent: serde_json::Value =
            serde_json::from_str(&intent).map_err(|e| LedgerError::Corrupt {
                what: format!("dispatch {dispatch_id}'s intent"),
                detail: e.to_string(),
            })?;
        let text = |key: &str| intent[key].as_str().map(str::to_string);
        Ok(Some(DispatchBooking {
            run_id: RunId::from_stored(&run_id),
            attempt_id,
            // A kind that is not a usage phase (the planner's `plan`)
            // books no phase here; the final booking sets the phase
            // the runner books it under on every per-turn row.
            phase: text("kind").and_then(|kind| UsagePhase::parse(&kind).ok()),
            requested_model: text("model"),
            requested_effort: text("effort"),
            harness: text("harness"),
        }))
    }

    /// Dispatch intent is persisted BEFORE the process exists (SPEC §12).
    /// A retry with the same dispatch ID is a no-op, not a duplicate.
    ///
    /// `parent_dispatch` is always NULL and `depth` always 0: every
    /// dispatch this function ever writes is a root managed worker a
    /// runner launches directly (SPEC §23) — a deeper, hook-admitted
    /// spawn never calls this, and never has a ledger row to adopt at
    /// all. `source` is written `managed_run`, the managed path's kind; a
    /// native worker's row is relabelled `native_run` by
    /// [`Self::record_native_dispatch`] right after, before its launch.
    /// Recording these as constants here is stating what is true at the
    /// point of writing, not a guess `live_dispatches` would otherwise
    /// have to make later.
    pub fn record_dispatch_intent(
        &self,
        dispatch_id: &DispatchId,
        run_id: &RunId,
        attempt_id: Option<i64>,
        intent: &serde_json::Value,
        reserved_micros: i64,
        routed_by: RoutedBy,
    ) -> Result<bool> {
        let now = self.now();
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO dispatches
                (dispatch_id, run_id, attempt_id, intent_json, state,
                 reserved_micros, created_at, updated_at,
                 source, agent_id, parent_dispatch, depth, routed_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, NULL, NULL, ?9, ?10)",
            params![
                dispatch_id.as_str(),
                run_id.as_str(),
                attempt_id,
                serde_json::to_string(intent).expect("intent serializes"),
                "intent",
                reserved_micros,
                now,
                "managed_run",
                0i64,
                routed_by.as_str(),
            ],
        )?;
        Ok(inserted == 1)
    }

    /// Dispatches that claimed to launch but never recorded a terminal
    /// state — the reconciliation set after a crash or restart. An absent
    /// terminal result never means nothing executed (SPEC §12).
    pub fn live_dispatches(&self) -> Result<Vec<LiveDispatch>> {
        /// One raw row of the adoption query, before the pid is checked
        /// for corruption — named so the query and the conversion below
        /// are not stitched together through a tuple wide enough to
        /// misalign a field by position.
        struct Row {
            dispatch: String,
            run: String,
            pid: Option<i64>,
            session_id: Option<String>,
            reserve_micros: i64,
            source: Option<String>,
            agent_id: Option<String>,
            parent_dispatch: Option<String>,
            depth: Option<i64>,
        }
        let mut stmt = self.conn.prepare(
            "SELECT dispatch_id, run_id, pid, session_id, reserved_micros,
                    source, agent_id, parent_dispatch, depth
             FROM dispatches WHERE state = 'launched'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Row {
                dispatch: row.get(0)?,
                run: row.get(1)?,
                pid: row.get(2)?,
                session_id: row.get(3)?,
                reserve_micros: row.get(4)?,
                source: row.get(5)?,
                agent_id: row.get(6)?,
                parent_dispatch: row.get(7)?,
                depth: row.get(8)?,
            })
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| {
                let pid = match row.pid {
                    None => None,
                    Some(raw) => Some(Pid::stored(raw).ok_or_else(|| LedgerError::Corrupt {
                        what: format!("the pid of dispatch {}", row.dispatch),
                        detail: format!("`{raw}` is not a process id"),
                    })?),
                };
                Ok(LiveDispatch {
                    dispatch: DispatchId::from_stored(row.dispatch),
                    run: RunId::from_stored(row.run),
                    pid,
                    session_id: row.session_id,
                    reserve_micros: row.reserve_micros,
                    source: row.source,
                    agent_id: row.agent_id,
                    parent_dispatch: row.parent_dispatch,
                    depth: row.depth,
                })
            })
            .collect()
    }

    /// Aggregate cost for a run: the sum of what was REPORTED. Unknown
    /// usage is NULL and `SUM` skips it, so the figure is a lower bound
    /// whenever `run_cost_completeness` says `unknown` — read the two
    /// together (`report::cost_line` does). Inclusive parent totals are
    /// alternative aggregation sources (SPEC §11): an inclusive event
    /// already contains its descendants, so everything below an
    /// inclusive parent is excluded and the parent is counted once. An
    /// inclusive event with no children simply counts itself.
    pub fn run_cost(&self, run_id: &RunId) -> Result<MicroUsd> {
        // A root run's cost is its tree's: packages are separate runs
        // attributed to it (SPEC §19, §23), each counted once.
        let mut total = MicroUsd::from_micros(0);
        for run in self.run_tree(run_id)? {
            total = total.saturating_add(self.run_own_cost(&run)?);
        }
        Ok(total)
    }

    fn run_own_cost(&self, run_id: &RunId) -> Result<MicroUsd> {
        let micros: i64 = self.conn.query_row(
            &format!(
                "{COVERED_BY_AN_INCLUSIVE_PARENT}
                 SELECT COALESCE(SUM(cost_micros), 0) FROM usage_events
                  WHERE run_id = ?1
                    AND event_id NOT IN (SELECT event_id FROM covered)"
            ),
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok(MicroUsd::from_micros(micros))
    }

    /// Worst-case completeness for the run's recorded usage: an unknown
    /// anywhere makes the run's cost unknown; an incomplete anywhere makes
    /// it an incomplete lower bound.
    ///
    /// Recorded usage alone cannot tell a run that never ran apart from
    /// one that ran and reported nothing back — both fold to `Actual`
    /// over zero rows, and an empty fold is not evidence of a true zero.
    /// The run's own attempts settle it: a run with no dispatched attempt
    /// never spent anything, so an empty recording IS the true zero; a
    /// run with a dispatched attempt that reported no usage at all died
    /// before it could report, so its cost is unknown, not zero; and a
    /// run where only some of its attempts reported is a real lower
    /// bound, not unknown — what was recorded still happened.
    pub fn run_cost_completeness(&self, run_id: &RunId) -> Result<CostCompleteness> {
        let mut values: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT completeness FROM usage_events WHERE run_id = ?1")?;
            let rows = stmt.query_map([run_id.as_str()], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for child in self.child_runs(run_id)? {
            values.push(
                serde_json::to_string(&self.run_cost_completeness(&child.run)?)
                    .expect("CostCompleteness serializes: a fieldless enum"),
            );
        }
        let recorded = CostCompleteness::worst(values.iter().map(|value| {
            // A completeness this binary cannot read IS unknown: the fold
            // is the value's own meaning, not a swallowed error, and it
            // can only widen the answer (`Unknown` is the worst case).
            serde_json::from_str(value).unwrap_or(CostCompleteness::Unknown)
        }));

        let (dispatched, reported) = self.run_dispatch_report_counts(run_id)?;
        // Nothing was ever sent out, so nothing could have reported: the
        // recorded fold is the whole truth, and for a run blocked at
        // preflight that is a real $0.
        if dispatched == 0 {
            return Ok(recorded);
        }
        Ok(if reported == 0 {
            CostCompleteness::Unknown
        } else if reported < dispatched {
            recorded.max(CostCompleteness::IncompleteLowerBound)
        } else {
            recorded
        })
    }

    /// How many dispatches this run sent out, and how many of them came
    /// back with their usage — what tells a fully-silent run apart from
    /// one that reported some dispatches and died before the rest.
    ///
    /// Dispatches, not attempts. A worker attempt has both, but the
    /// reviewer and the planner are dispatched with `attempt_id: None`
    /// (`runner/mod.rs` review, `runner/scheduler.rs` plan), so counting
    /// attempts reads a run whose worker reported and whose REVIEWER was
    /// killed mid-flight as complete. The dispatch is the thing that
    /// spends money, and its own id is what the usage event carries:
    /// measured on a live ledger, all 47 `completed` dispatches have a
    /// usage event keyed by their `dispatch_id` and the two that never
    /// finished have none.
    fn run_dispatch_report_counts(&self, run_id: &RunId) -> Result<(i64, i64)> {
        let dispatched: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM dispatches WHERE run_id = ?1",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        let reported: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM dispatches d WHERE d.run_id = ?1
               AND EXISTS (SELECT 1 FROM usage_events u WHERE u.event_id = d.dispatch_id)",
            [run_id.as_str()],
            |row| row.get(0),
        )?;
        Ok((dispatched, reported))
    }

    /// The run's spend grouped by phase (SPEC §11): which of the run's
    /// phases — a worker attempt, the reviewer, the planner, a package's
    /// integration — spent the money, and how complete that figure is.
    /// `phase: None` is the unattributed bucket: a row written before
    /// the `phase` column existed, or a child run's own unattributed
    /// rows. Same dedup as [`Ledger::run_cost`]: an inclusive parent's
    /// descendants are excluded so nothing is counted twice, and a
    /// child run's (a work package's) breakdown is merged into its
    /// parent's by phase.
    pub fn run_cost_by_phase(&self, run_id: &RunId) -> Result<Vec<PhaseCost>> {
        type Row = (Option<String>, Option<i64>, String);
        let mut stmt = self.conn.prepare(&format!(
            "{COVERED_BY_AN_INCLUSIVE_PARENT}
             SELECT phase, cost_micros, completeness FROM usage_events
              WHERE run_id = ?1
                AND event_id NOT IN (SELECT event_id FROM covered)"
        ))?;
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let rows: Vec<Row> = rows.collect::<std::result::Result<Vec<_>, _>>()?;

        let mut totals: Vec<(Option<UsagePhase>, MicroUsd, Vec<CostCompleteness>)> = Vec::new();
        for (phase_raw, cost_micros, completeness_raw) in rows {
            let phase = phase_raw
                .as_deref()
                .map(UsagePhase::parse)
                .transpose()
                .map_err(|unknown| LedgerError::Corrupt {
                    what: format!("the phase of a usage event of run {run_id}"),
                    detail: unknown.to_string(),
                })?;
            // A completeness this binary cannot read IS unknown, same
            // fold as `run_cost_completeness`.
            let completeness: CostCompleteness =
                serde_json::from_str(&completeness_raw).unwrap_or(CostCompleteness::Unknown);
            fold_phase_cost(&mut totals, phase, cost_micros, completeness);
        }
        for child in self.child_runs(run_id)? {
            for entry in self.run_cost_by_phase(&child.run)? {
                fold_phase_cost(
                    &mut totals,
                    entry.phase,
                    Some(entry.cost.to_micros()),
                    entry.completeness,
                );
            }
        }
        totals.sort_by_key(|(phase, ..)| (phase.is_none(), *phase));
        Ok(totals
            .into_iter()
            .map(|(phase, cost, completenesses)| PhaseCost {
                phase,
                cost,
                completeness: CostCompleteness::worst(completenesses),
            })
            .collect())
    }

    /// Everything this machine has spent since `since` (RFC3339), across
    /// every run — what a per-day ceiling is a ceiling on. Unknown usage
    /// is NULL and `SUM` skips it, so this is a lower bound: a daily
    /// ceiling is best effort in exactly the way SPEC §11 says a dollar
    /// control is. Children attributed to an inclusive parent are
    /// excluded on the same rule as `run_own_cost`, so a provider total
    /// that already contains its subagents is counted once.
    pub fn spend_since(&self, since: &str) -> Result<MicroUsd> {
        let micros: i64 = self.conn.query_row(
            &format!(
                "{COVERED_BY_AN_INCLUSIVE_PARENT}
                 SELECT COALESCE(SUM(cost_micros), 0) FROM usage_events
                  WHERE at >= ?1
                    AND event_id NOT IN (SELECT event_id FROM covered)"
            ),
            [since],
            |row| row.get(0),
        )?;
        Ok(MicroUsd::from_micros(micros))
    }

    /// A task's total cost: each of its runs' OWN usage, summed once.
    /// Unlike [`Ledger::run_cost`] this does not also walk `child_runs` —
    /// a decomposed run's packages already carry the same `task_id` as
    /// their root (inherited at `insert_child_run`), so a run-by-run own
    /// cost over every run of the task already counts the whole tree
    /// exactly once; recursing as `run_cost` does would double it.
    pub fn task_cost(&self, task_id: &TaskId) -> Result<MicroUsd> {
        let mut total = MicroUsd::ZERO;
        for run in self.runs_of_task(task_id)? {
            total = total.saturating_add(self.run_own_cost(&run)?);
        }
        Ok(total)
    }

    /// Worst-case completeness across a task's runs, BY CALLING
    /// [`Ledger::run_cost_completeness`] per run rather than restating
    /// what it does.
    ///
    /// This used to fold the `usage_events` rows itself, which was the
    /// same answer right up until `run_cost_completeness` learned that a
    /// dispatch which never reported its usage makes a run's cost
    /// unknown. Then a task holding a killed run still folded to
    /// `Actual` here — and this is the function the PRIMARY METRIC line
    /// is labelled from, so the one number the change existed to correct
    /// was the one still printing `(actual)`. One rule, one place.
    pub fn task_cost_completeness(&self, task_id: &TaskId) -> Result<CostCompleteness> {
        let mut values = Vec::new();
        for run in self.runs_of_task(task_id)? {
            values.push(self.run_cost_completeness(&run)?);
        }
        Ok(CostCompleteness::worst(values))
    }

    /// The own cost of exactly these runs, each counted once — a task's
    /// cost over a chosen subset of its runs (see [`Ledger::task_cost`]
    /// for why the sum is of own costs, not of trees).
    pub fn runs_own_cost(&self, runs: &[RunId]) -> Result<MicroUsd> {
        let mut total = MicroUsd::ZERO;
        for run in runs {
            total = total.saturating_add(self.run_own_cost(run)?);
        }
        Ok(total)
    }

    /// Worst-case completeness across exactly these runs.
    pub fn runs_cost_completeness(&self, runs: &[RunId]) -> Result<CostCompleteness> {
        let mut values = Vec::new();
        for run in runs {
            values.push(self.run_cost_completeness(run)?);
        }
        Ok(CostCompleteness::worst(values))
    }

    /// What runs of one recorded purpose spent, for runs created at or
    /// after `since` (SPEC §11): own cost of each run, summed once, its
    /// completeness, and how many runs it is over. Kept apart from every
    /// cohort's figures because a replay or a trial arm is an experiment's
    /// expense, not ordinary work.
    pub fn purpose_spend_since(
        &self,
        since: &str,
        purpose: RunPurpose,
    ) -> Result<(MicroUsd, CostCompleteness, usize)> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM runs WHERE purpose = ?1 AND created_at >= ?2 ORDER BY id")?;
        let rows = stmt.query_map(params![purpose.as_str(), since], |row| {
            row.get::<_, String>(0)
        })?;
        let runs: Vec<RunId> = rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .map(RunId::from_stored)
            .collect();
        Ok((
            self.runs_own_cost(&runs)?,
            self.runs_cost_completeness(&runs)?,
            runs.len(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{event, package, run, task, temp_ledger};
    use super::super::ChildOf;
    use super::*;

    #[test]
    fn unknown_usage_is_null_left_out_of_the_sum_and_poisons_completeness() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-u"), "/r", None, &task("run-u"), "rk")
            .expect("run");
        ledger
            .record_usage(&event("known", "run-u", 700))
            .expect("known");
        let mut unknown = event("unknown", "run-u", 0);
        unknown.cost = None;
        unknown.completeness = CostCompleteness::Unknown;
        ledger.record_usage(&unknown).expect("unknown");
        assert_eq!(
            ledger.run_cost(&run("run-u")).expect("cost"),
            MicroUsd::from_micros(700),
            "the reported part, a lower bound"
        );
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-u"))
                .expect("completeness"),
            CostCompleteness::Unknown
        );
        let stored: Option<i64> = ledger
            .conn
            .query_row(
                "SELECT cost_micros FROM usage_events WHERE event_id = 'unknown'",
                [],
                |row| row.get(0),
            )
            .expect("row");
        assert_eq!(stored, None, "NULL, not 0");
        // Best effort: the fixture is a temp dir and a leftover costs
        // nothing but disk, which the next run's pre-clean takes.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_usage_events_are_deduped_not_duplicated() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-x"), "/repo", None, &task("run-x"), "rk")
            .expect("run");
        let e = event("req-1", "run-x", 500);
        assert!(ledger.record_usage(&e).expect("first"), "first insert wins");
        assert!(
            !ledger.record_usage(&e).expect("second"),
            "duplicate event_id is a no-op"
        );
        assert_eq!(
            ledger.run_cost(&run("run-x")).expect("cost"),
            MicroUsd::from_micros(500)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn inclusive_parent_never_double_counts_children() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-y"), "/repo", None, &task("run-y"), "rk")
            .expect("run");
        let mut parent = event("parent", "run-y", 900);
        parent.inclusive = true;
        let mut child = event("child", "run-y", 400);
        child.parent_event_id = Some("parent".into());
        assert!(ledger.record_usage(&parent).expect("parent"));
        assert!(ledger.record_usage(&child).expect("child"));
        assert_eq!(
            ledger.run_cost(&run("run-y")).expect("cost"),
            MicroUsd::from_micros(900),
            "inclusive parent stands in for its children"
        );
        let mut standalone = event("solo", "run-y", 100);
        standalone.inclusive = true;
        assert!(ledger.record_usage(&standalone).expect("solo"));
        assert_eq!(
            ledger.run_cost(&run("run-y")).expect("cost"),
            MicroUsd::from_micros(1000)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P11: the dedup used to look one generation up, so a grandchild
    /// of an inclusive event was added to a total that already contained
    /// it.
    #[test]
    fn an_inclusive_ancestor_covers_its_whole_subtree() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-deep"), "/repo", None, &task("run-deep"), "rk")
            .expect("run");
        let mut root = event("top", "run-deep", 900);
        root.inclusive = true;
        let mut child = event("mid", "run-deep", 400);
        child.parent_event_id = Some("top".into());
        let mut grandchild = event("leaf", "run-deep", 200);
        grandchild.parent_event_id = Some("mid".into());
        for e in [&root, &child, &grandchild] {
            assert!(ledger.record_usage(e).expect("usage"));
        }
        assert_eq!(
            ledger.run_cost(&run("run-deep")).expect("cost"),
            MicroUsd::from_micros(900),
            "the inclusive total stands in for every generation below it"
        );
        assert_eq!(
            ledger
                .spend_since("2000-01-01T00:00:00+00:00")
                .expect("spend"),
            MicroUsd::from_micros(900),
            "the day's figure counts it once too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completeness_degrades_to_the_worst_observed() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-c"), "/repo", None, &task("run-c"), "rk")
            .expect("run");
        assert!(ledger.record_usage(&event("a", "run-c", 10)).expect("a"));
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-c"))
                .expect("completeness"),
            CostCompleteness::Actual
        );
        let mut unknown = event("b", "run-c", 0);
        unknown.completeness = CostCompleteness::Unknown;
        assert!(ledger.record_usage(&unknown).expect("b"));
        assert_eq!(
            ledger
                .run_cost_completeness(&run("run-c"))
                .expect("completeness"),
            CostCompleteness::Unknown,
            "unknown usage is never zero and poisons the total"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn spend_since_sums_the_day_across_runs_and_skips_unknown() {
        let (ledger, dir) = temp_ledger();
        for run in ["run-a", "run-b"] {
            ledger
                .insert_run(
                    &super::RunId::from_stored(run),
                    "/r",
                    None,
                    &task(run),
                    "rk",
                )
                .expect("run");
        }
        let mut yesterday = event("old", "run-a", 5_000);
        yesterday.at = "2026-09-19T23:59:59+00:00".into();
        ledger.record_usage(&yesterday).expect("old");
        let mut early = event("early", "run-a", 700);
        early.at = "2026-09-20T00:00:00+00:00".into();
        ledger.record_usage(&early).expect("early");
        // Another run on the same machine, same day: a daily ceiling is
        // the machine's, not one run's.
        let mut other = event("other", "run-b", 300);
        other.at = "2026-09-20T09:00:00+00:00".into();
        ledger.record_usage(&other).expect("other");
        // Usage the provider never reported is NULL, not zero: the sum
        // skips it and the day's figure is a lower bound.
        let mut unknown = event("unknown", "run-b", 0);
        unknown.at = "2026-09-20T10:00:00+00:00".into();
        unknown.cost = None;
        unknown.completeness = CostCompleteness::Unknown;
        ledger.record_usage(&unknown).expect("unknown");

        assert_eq!(
            ledger
                .spend_since("2026-09-20T00:00:00+00:00")
                .expect("spend"),
            MicroUsd::from_micros(1_000),
            "today only, both runs, unknown left out"
        );
        assert_eq!(
            ledger
                .spend_since("2026-09-21T00:00:00+00:00")
                .expect("spend"),
            MicroUsd::ZERO,
            "a fresh day starts at zero"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_root_run_costs_its_whole_tree_once() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("root"), "/r", None, &task("root"), "rk")
            .expect("root");
        ledger
            .insert_child_run(
                &run("pkg-a"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root"),
                    package_id: &package("a"),
                },
                &task("root"),
                "rk",
            )
            .expect("child");
        ledger
            .insert_child_run(
                &run("pkg-b"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root"),
                    package_id: &package("b"),
                },
                &task("root"),
                "rk",
            )
            .expect("child");
        ledger
            .record_usage(&event("e-root", "root", 10))
            .expect("usage");
        ledger
            .record_usage(&event("e-a", "pkg-a", 100))
            .expect("usage");
        ledger
            .record_usage(&event("e-b", "pkg-b", 1000))
            .expect("usage");
        assert_eq!(
            ledger.run_cost(&run("root")).expect("cost").to_micros(),
            1110
        );
        assert_eq!(
            ledger.run_cost(&run("pkg-a")).expect("cost").to_micros(),
            100
        );
        let listed = ledger
            .runs_since("2000-01-01T00:00:00+00:00")
            .expect("runs");
        assert_eq!(listed.len(), 1, "packages are not listed as separate runs");
        assert_eq!(
            ledger
                .child_runs(&run("root"))
                .expect("children")
                .iter()
                .map(|child| child.package.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `task_cost` sums a decomposed run's tree exactly once: it must
    /// not double what `run_cost`'s own recursive walk already counts,
    /// because every run in the tree shares the same task.
    #[test]
    fn task_cost_counts_a_decomposed_runs_tree_exactly_once() {
        let (ledger, dir) = temp_ledger();
        let the_task = task("dedup");
        ledger
            .insert_run(&run("root-3"), "/r", None, &the_task, "rk")
            .expect("root");
        ledger
            .insert_child_run(
                &run("pkg-3a"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root-3"),
                    package_id: &package("a"),
                },
                &the_task,
                "rk",
            )
            .expect("child a");
        ledger
            .insert_child_run(
                &run("pkg-3b"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("root-3"),
                    package_id: &package("b"),
                },
                &the_task,
                "rk",
            )
            .expect("child b");
        ledger
            .record_usage(&event("e-root3", "root-3", 10))
            .expect("usage");
        ledger
            .record_usage(&event("e-pkg3a", "pkg-3a", 100))
            .expect("usage");
        ledger
            .record_usage(&event("e-pkg3b", "pkg-3b", 1000))
            .expect("usage");
        assert_eq!(
            ledger.task_cost(&the_task).expect("task cost").to_micros(),
            1110,
            "once per run, not once per (run_cost's own tree walk) x (task's runs)"
        );
        assert_eq!(
            ledger
                .run_cost(&run("root-3"))
                .expect("run cost")
                .to_micros(),
            1110,
            "run_cost's own tree walk agrees"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
