//! Transitions and decisions: the transition chain, the decision spine and
//! the answers recorded on it. Together because they share transactions.

use std::path::PathBuf;

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::ids::{RunId, TaskId};
use crate::lifecycle::{Reason, State};

use super::error::parse_state;
use super::{Ledger, LedgerError, Result};

/// Who decided (orchestrator-decides plan, Phase 1). Exactly three
/// values; there is no free-text name because the machine has one person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Actor {
    /// The root Claude Code session decided on its own authority.
    #[serde(rename = "orchestrator")]
    Orchestrator,
    /// The person decided, typing the command in their own terminal.
    #[serde(rename = "person")]
    Person,
    /// The person said it, the orchestrator typed it; a quote of the
    /// person's words is recorded with the answer.
    #[serde(rename = "person-via-orchestrator")]
    PersonViaOrchestrator,
}

/// How an answer reached relais (ADR-0004): through the plugin's decide
/// tool, proved by the session's nonce, or from any other command line.
/// Rows written before the column existed read `legacy`, which nothing
/// writes now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordedVia {
    /// The call carried the plugin session's nonce
    /// ([`Ledger::verify_decide_nonce`]).
    Plugin,
    /// Anything else: a terminal, a script, or a model shelling out.
    Cli,
}

/// Who answered a salvage, and how the answer reached relais: the three
/// facts [`Ledger::record_salvage_contained`] stores beside the answer.
#[derive(Debug, Clone, Copy)]
pub struct Answerer<'a> {
    pub actor: Actor,
    /// The person's words, when `actor` is [`Actor::PersonViaOrchestrator`].
    pub person_quote: Option<&'a str>,
    pub recorded_via: RecordedVia,
}

/// What [`Ledger::person_answer_counts`] counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PersonAnswerCounts {
    /// Every answer that counts as the person's, ever.
    pub total: usize,
    /// `person` answers resolved since the window's start.
    pub recent_person: usize,
    /// Proved `person-via-orchestrator` answers resolved since then.
    pub recent_proxied: usize,
    /// `person-via-orchestrator` answers recorded without the plugin's
    /// proof: zero unless something went around the decide tool.
    pub unproved_proxied: usize,
}

/// How long a minted decide nonce is honoured when its session never said
/// it ended (a crash, a kill): past this, minting any nonce sweeps it.
const DECIDE_NONCE_TTL_DAYS: i64 = 7;

/// A run's recorded driver: pid, process start time, since when.
pub type RunDriver = (Option<i64>, Option<String>, Option<String>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub run_id: RunId,
    pub attempt_id: Option<i64>,
    pub from_state: Option<State>,
    pub to_state: State,
    pub reason: String,
    pub detail: Option<serde_json::Value>,
    pub at: String,
}

/// Append one transition row and bring `runs.status` with it, inside a
/// transaction the caller owns.
///
/// Both writers of the transition chain go through here — the runner's
/// [`Ledger::record_transition`] and a person's answer in
/// [`Ledger::resolve_decision`], which cannot call the former because
/// that one also OPENS a decision row for a waiting state and would
/// reopen the very row the answer is closing. Two hand-written copies of
/// this pair is how a column added to one silently stops being written
/// by the other, so there is one copy.
///
/// `runs.status` is updated on every transition, not only terminal ones:
/// `relais status` used to say `prepared` for a run ten minutes into
/// verifying.
fn append_transition(tx: &rusqlite::Transaction<'_>, transition: &Transition) -> Result<()> {
    let detail = transition
        .detail
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|e| LedgerError::Corrupt {
            what: "a transition detail".into(),
            detail: e.to_string(),
        })?;
    tx.execute(
        "INSERT INTO transitions
            (run_id, attempt_id, from_state, to_state, reason, detail_json, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            transition.run_id.as_str(),
            transition.attempt_id,
            transition.from_state.map(|state| state.as_str()),
            transition.to_state.as_str(),
            transition.reason,
            detail,
            transition.at,
        ],
    )?;
    tx.execute(
        "UPDATE runs SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![
            transition.run_id.as_str(),
            transition.to_state.as_str(),
            transition.at
        ],
    )?;
    Ok(())
}

/// Record this process as the run's driver: its pid, its start time (the
/// token that tells a reused pid from the driver) and when it took the run.
/// Runs inside the caller's transaction, so the driver is on record in the
/// same commit as the transition that makes it the driver.
pub(super) fn stamp_driver(tx: &rusqlite::Transaction<'_>, run: &RunId, now: &str) -> Result<()> {
    let pid = std::process::id();
    tx.execute(
        "UPDATE runs SET driver_pid = ?2, driver_started = ?3, driver_since = ?4 WHERE id = ?1",
        params![
            run.as_str(),
            i64::from(pid),
            crate::procs::start_time(pid),
            now
        ],
    )?;
    Ok(())
}

/// A run's status as a projection of its transition history (P3): the
/// last transition's `to_state`, falling back to the `runs.status` column
/// only for a row with no transition at all — an old ledger's run, written
/// before every state change recorded a row. Every status reader selects
/// this expression, so no reader can disagree with a chain that exists,
/// and `runs.status` survives only as that legacy fallback.
pub(super) const PROJECTED_STATUS: &str = "COALESCE(\
    (SELECT t.to_state FROM transitions t WHERE t.run_id = runs.id ORDER BY t.id DESC LIMIT 1), \
    runs.status) AS status";

/// One run's decision row (SPEC's decision spine): when a person was
/// first owed a look, what state and reason raised it, and — once
/// answered — who decided what, when, and any successor run their
/// decision names. `resolved_at.is_none()` is the open half `report`
/// lists and `relais decide` closes; `waited_seconds` is derived from the
/// recorded timestamps, never stored, so it is always current for an open
/// row and exact for a resolved one.
/// A person's answer to an open decision: what they decided, who they
/// are, any note and successor run they named, and the two states their
/// answer moves the run between. One argument rather than six, because
/// they are one answer and every caller has all of them at once.
pub struct DecisionAnswer<'a> {
    pub resolution: Reason,
    pub actor: Actor,
    /// The person's words, when `actor` is [`Actor::PersonViaOrchestrator`].
    pub person_quote: Option<&'a str>,
    /// How the answer reached relais.
    pub recorded_via: RecordedVia,
    pub note: Option<&'a str>,
    pub successor_run: Option<&'a RunId>,
    /// The state the run was waiting in.
    pub from_state: State,
    /// The terminal state this answer assigns. Never
    /// [`State::awaits_a_person`]: an answer ends a wait, never starts one.
    pub to_state: State,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionRecord {
    pub run: RunId,
    pub task: TaskId,
    pub raised_at: String,
    pub raised_state: State,
    pub raised_reason: String,
    pub resolved_at: Option<String>,
    pub resolution: Option<Reason>,
    pub actor: Option<String>,
    pub note: Option<String>,
    pub successor_run: Option<RunId>,
    /// For a salvage: whether the run's verified candidate patch survives
    /// intact in the salvaged commit. `None` is unknown, never `false`.
    pub candidate_contained: Option<bool>,
    pub waited_seconds: i64,
}

/// The seconds between two RFC3339 timestamps this ledger itself wrote —
/// a stamp that will not parse is a corrupt row, not a wait of zero.
pub(super) fn seconds_between(start: &str, end: &str) -> Result<i64> {
    let parse = |what: &'static str, stamp: &str| {
        chrono::DateTime::parse_from_rfc3339(stamp).map_err(|e| LedgerError::Corrupt {
            what: what.into(),
            detail: format!("`{stamp}` is not an RFC3339 timestamp: {e}"),
        })
    };
    let start = parse("a decision's raised_at", start)?;
    let end = parse("a decision's resolved_at", end)?;
    Ok((end - start).num_seconds())
}

/// Raw columns of one `decisions` row, in `SELECT` order.
pub(super) type DecisionRow = (
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<bool>,
);

const DECISION_COLUMNS: &str =
    "run, task, raised_at, raised_state, raised_reason, resolved_at, resolution, actor, note, successor_run, candidate_contained";

fn decision_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<DecisionRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

impl Ledger {
    /// Record a transition and the run status it implies.
    ///
    /// One transaction, because they are one fact (P3). As two
    /// autocommits, a kill between them left a run whose history said
    /// `accepted` and whose status still said `verifying`, and `resume`
    /// then overwrote an accepted run as `interrupted`.
    ///
    /// The only other method that writes a record plus derived state used
    /// to be `insert_contract_revision`; this one now also opens a
    /// [`DecisionRecord`] in the same transaction whenever the transition
    /// lands on a state [`State::awaits_a_person`] — a run can never be
    /// waiting on a person without a row recording that it started to.
    /// Nothing here closes one: only `resolve_decision` does that.
    pub fn record_transition(&self, transition: &Transition) -> Result<()> {
        self.record_transition_inner(transition, false)
    }

    /// [`Self::record_transition`] by a process that re-enters a run (a
    /// resume): the same transaction also records that process as the
    /// run's driver.
    pub fn record_transition_as_driver(&self, transition: &Transition) -> Result<()> {
        self.record_transition_inner(transition, true)
    }

    /// Compare-and-set: record `transition` only when the run's projected
    /// state is `expected_from`, all inside one `BEGIN IMMEDIATE`. Returns
    /// whether it wrote. A driver whose run was reaped meanwhile (the state
    /// moved on) writes nothing, so it can never overwrite the reap.
    pub fn transition_if(&self, expected_from: State, transition: &Transition) -> Result<bool> {
        let tx = self.write_tx()?;
        let current: Option<String> = tx
            .query_row(
                &format!("SELECT {PROJECTED_STATUS} FROM runs WHERE id = ?1"),
                [transition.run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if current.as_deref() != Some(expected_from.as_str()) {
            return Ok(false);
        }
        self.write_transition(&tx, transition)?;
        tx.commit()?;
        Ok(true)
    }

    /// The driver a run records: its pid, the process start time and when
    /// it took the run. Each is `None` for a run no driver has stamped.
    pub fn run_driver(&self, run: &RunId) -> Result<RunDriver> {
        Ok(self.conn.query_row(
            "SELECT driver_pid, driver_started, driver_since FROM runs WHERE id = ?1",
            [run.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?)
    }

    fn record_transition_inner(&self, transition: &Transition, as_driver: bool) -> Result<()> {
        let tx = self.write_tx()?;
        self.write_transition(&tx, transition)?;
        if as_driver {
            stamp_driver(&tx, &transition.run_id, &transition.at)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn write_transition(&self, tx: &Transaction<'_>, transition: &Transition) -> Result<()> {
        append_transition(tx, transition)?;
        if transition.to_state.awaits_a_person() {
            let task_id: Option<String> = tx
                .query_row(
                    "SELECT task_id FROM runs WHERE id = ?1",
                    [transition.run_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let task_id =
                task_id.unwrap_or_else(|| format!("task-legacy-{}", transition.run_id.as_str()));
            // `OR IGNORE`: a run already terminal in an awaiting state can
            // still record a same-state transition of its own (worktree
            // retirement transitions `self.state` to itself once the run
            // has already stopped) — the decision was already opened the
            // first time this run reached the state, and `run` is its
            // primary key.
            tx.execute(
                "INSERT OR IGNORE INTO decisions (run, task, raised_at, raised_state, raised_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    transition.run_id.as_str(),
                    task_id,
                    transition.at,
                    transition.to_state.as_str(),
                    transition.reason,
                ],
            )?;
        }
        Ok(())
    }

    /// Every run still waiting for a person to answer it, oldest raised
    /// first — the queue `relais report` lists and `relais decide`
    /// closes one row at a time from.
    /// How many answers count as the person's judgment (ADR-0004), for the
    /// decision advisor's data gate: a `person` answer, or a
    /// `person-via-orchestrator` one the plugin proved, on a
    /// `needs_decision` or `needs_review` run. `legacy` rows never count.
    /// `since` bounds the recent counts; `total` is every counted answer.
    pub fn person_answer_counts(&self, since: &str) -> Result<PersonAnswerCounts> {
        let counted = "raised_state IN ('needs_decision', 'needs_review')
             AND ((actor = 'person' AND recorded_via != 'legacy')
               OR (actor = 'person-via-orchestrator' AND recorded_via = 'plugin'))";
        let count = |sql: String, since: Option<&str>| -> Result<usize> {
            let n: i64 = match since {
                Some(since) => self.conn.query_row(&sql, [since], |row| row.get(0))?,
                None => self.conn.query_row(&sql, [], |row| row.get(0))?,
            };
            Ok(n.max(0) as usize)
        };
        Ok(PersonAnswerCounts {
            total: count(
                format!("SELECT COUNT(*) FROM decisions WHERE {counted}"),
                None,
            )?,
            recent_person: count(
                format!(
                    "SELECT COUNT(*) FROM decisions WHERE {counted} AND actor = 'person'
                     AND resolved_at >= ?1"
                ),
                Some(since),
            )?,
            recent_proxied: count(
                format!(
                    "SELECT COUNT(*) FROM decisions WHERE {counted}
                     AND actor = 'person-via-orchestrator' AND resolved_at >= ?1"
                ),
                Some(since),
            )?,
            unproved_proxied: count(
                "SELECT COUNT(*) FROM decisions
                  WHERE actor = 'person-via-orchestrator' AND recorded_via = 'cli'"
                    .to_string(),
                None,
            )?,
        })
    }

    pub fn open_decisions(&self) -> Result<Vec<DecisionRecord>> {
        let now = self.now();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE resolved_at IS NULL ORDER BY raised_at, run"
        ))?;
        let rows = stmt.query_map([], decision_row_from)?;
        rows.collect::<std::result::Result<Vec<DecisionRow>, _>>()?
            .into_iter()
            .map(|row| parse_decision_row(row, &now))
            .collect()
    }

    /// Every decision somebody has answered, oldest raised first: what the
    /// decision label rule (`learn::decision_label`) reads.
    pub fn answered_decisions(&self) -> Result<Vec<DecisionRecord>> {
        let now = self.now();
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {DECISION_COLUMNS} FROM decisions WHERE resolved_at IS NOT NULL
             ORDER BY raised_at, run"
        ))?;
        let rows = stmt.query_map([], decision_row_from)?;
        rows.collect::<std::result::Result<Vec<DecisionRow>, _>>()?
            .into_iter()
            .map(|row| parse_decision_row(row, &now))
            .collect()
    }

    /// One run's own decision row, open or resolved — what `relais
    /// explain` reads back. `None` for a run that never raised one.
    pub fn decision_of_run(&self, run_id: &RunId) -> Result<Option<DecisionRecord>> {
        let now = self.now();
        let row: Option<DecisionRow> = self
            .conn
            .query_row(
                &format!("SELECT {DECISION_COLUMNS} FROM decisions WHERE run = ?1"),
                [run_id.as_str()],
                decision_row_from,
            )
            .optional()?;
        row.map(|row| parse_decision_row(row, &now)).transpose()
    }

    /// Answer an open decision — who decided what, when, with an optional
    /// note and successor run — AND record the terminal state that
    /// answer assigns, one transaction because they are one fact (P3):
    /// two autocommits here used to let a kill leave a run whose
    /// decision said `approved` and whose projected status still said
    /// `needs_decision`, disagreeing with the very answer sitting next
    /// to it. `to_state` is never [`State::awaits_a_person`], so this
    /// never opens a new decision row the way [`Self::record_transition`]
    /// does — it only ever closes the one already open. `Ok(false)` when
    /// the run has no OPEN decision — never raised one, or one already
    /// answered — so the CLI can refuse "deciding a run that is not
    /// waiting" by name rather than silently overwriting a prior answer;
    /// in that case neither the decision nor the run's state changed.
    pub fn resolve_decision(&self, run_id: &RunId, answer: &DecisionAnswer<'_>) -> Result<bool> {
        let tx = self.write_tx()?;
        let changed = self.resolve_decision_in(&tx, run_id, answer)?;
        tx.commit()?;
        Ok(changed)
    }

    /// [`Self::resolve_decision`] inside a transaction the caller owns, so
    /// the answer and a row that must stand or fall with it (a successor
    /// intent) are one commit.
    pub(super) fn resolve_decision_in(
        &self,
        tx: &Transaction<'_>,
        run_id: &RunId,
        answer: &DecisionAnswer<'_>,
    ) -> Result<bool> {
        let now = self.now();
        let changed = tx.execute(
            "UPDATE decisions SET resolved_at = ?2, resolution = ?3, actor = ?4, actor_raw = ?4,
                    note = ?5, successor_run = ?6, person_quote = ?7, recorded_via = ?8
             WHERE run = ?1 AND resolved_at IS NULL",
            params![
                run_id.as_str(),
                now,
                answer.resolution.as_str(),
                answer.actor.as_str(),
                answer.note,
                answer.successor_run.map(RunId::as_str),
                answer.person_quote,
                answer.recorded_via.as_str(),
            ],
        )?;
        if changed > 0 {
            append_transition(
                tx,
                &Transition {
                    run_id: run_id.clone(),
                    attempt_id: None,
                    from_state: Some(answer.from_state),
                    to_state: answer.to_state,
                    reason: answer.resolution.as_str().to_string(),
                    detail: None,
                    at: now.clone(),
                },
            )?;
            stamp_driver(tx, run_id, &now)?;
        }
        Ok(changed > 0)
    }

    /// Record that a person finished and merged the candidate a run left
    /// behind — `relais decide --answer salvaged --candidate <sha>` —
    /// whether that run ended on its own (terminal, never awaiting anyone)
    /// or is still waiting on a person (an OPEN decision row). Either way
    /// the run's own reason for stopping is kept exactly as its last
    /// state-changing transition recorded it, not replaced by the
    /// salvage's own resolution, and a new transition to
    /// [`State::AcceptedByPerson`] is recorded whose detail names the
    /// candidate a person actually merged.
    ///
    /// A run with no decision row never opened one:
    /// [`Self::record_transition`] only opens one for a state
    /// [`State::awaits_a_person`], and a salvaged terminal run's state
    /// (`failed`, `blocked`, `budget_exhausted`, `cancelled`) is never one
    /// of those — so this both raises AND resolves the row in the same
    /// write. A run with an OPEN row already has one raised: this resolves
    /// THAT row rather than raising a second one, the same way
    /// [`Self::resolve_decision`] would, except that the answer is
    /// `salvaged` rather than a person's own approve/reject. Either path
    /// reads the raising transition, not merely the last row: a run keeps
    /// recording transitions after it stops changing state — retiring its
    /// worktree writes a same-state row — and reading THAT reason would
    /// report every salvaged run's own ending as `worktree_retired` (the
    /// same trap `report::accepted_by_person` guards against for
    /// `Accepted`).
    ///
    /// `Ok(None)` when the run has no transition to salvage (an unknown
    /// run, or one still in flight) or its decision row is already
    /// RESOLVED — a person already answered it, or it was already
    /// salvaged — so a second answer is refused by name rather than
    /// silently overwriting the first. `Ok(Some(reason))` on success,
    /// naming the reason the run originally stopped for, for the caller to
    /// report back.
    pub fn record_salvage(
        &self,
        run_id: &RunId,
        candidate_sha: &str,
        actor: Actor,
        note: Option<&str>,
    ) -> Result<Option<Reason>> {
        self.record_salvage_contained(
            run_id,
            candidate_sha,
            Answerer {
                actor,
                person_quote: None,
                recorded_via: RecordedVia::Cli,
            },
            note,
            None,
        )
    }

    /// Record where a run's accepted change landed. The first landing found
    /// is kept; a later one for the same run is ignored.
    pub fn record_landing(&self, run_id: &RunId, sha: &str, landed_at: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO landings (run_id, landing_sha, landed_at, found_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![run_id.as_str(), sha, landed_at, self.now()],
        )?;
        Ok(())
    }

    /// The recorded landing of a run: its commit and when it landed.
    pub fn landing_of(&self, run_id: &RunId) -> Result<Option<(String, String)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT landing_sha, landed_at FROM landings WHERE run_id = ?1",
                [run_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Keep where a run's landing search got to: the last commit checked
    /// that did not reverse-apply. A later search replaces it.
    pub fn record_landing_search(&self, run_id: &RunId, searched_to: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO landing_searches (run_id, searched_to, searched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(run_id) DO UPDATE SET
                 searched_to = excluded.searched_to,
                 searched_at = excluded.searched_at",
            params![run_id.as_str(), searched_to, self.now()],
        )?;
        Ok(())
    }

    /// The commit a run's landing search got to, if one is kept.
    pub fn landing_search_of(&self, run_id: &RunId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT searched_to FROM landing_searches WHERE run_id = ?1",
                [run_id.as_str()],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Drop a run's kept search cursor.
    pub fn clear_landing_search(&self, run_id: &RunId) -> Result<()> {
        self.conn.execute(
            "DELETE FROM landing_searches WHERE run_id = ?1",
            [run_id.as_str()],
        )?;
        Ok(())
    }

    /// [`Self::record_salvage`], also recording whether the run's verified
    /// candidate patch survives intact in the salvaged commit
    /// (`candidate_contained`): `Some(true)` contained, `Some(false)`
    /// rewritten, `None` could not be computed.
    pub fn record_salvage_contained(
        &self,
        run_id: &RunId,
        candidate_sha: &str,
        answerer: Answerer<'_>,
        note: Option<&str>,
        candidate_contained: Option<bool>,
    ) -> Result<Option<Reason>> {
        let Answerer {
            actor,
            person_quote,
            recorded_via,
        } = answerer;
        let now = self.now();
        let tx = self.write_tx()?;
        let existing: Option<(Option<String>, String, String)> = tx
            .query_row(
                "SELECT resolved_at, raised_state, raised_reason FROM decisions WHERE run = ?1",
                [run_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (from_state, original_reason) = match existing {
            // Already answered — a person's own approve/reject, or an
            // earlier salvage — refuses a second answer.
            Some((Some(_resolved_at), ..)) => return Ok(None),
            // An OPEN row: raised when the run first reached its current
            // waiting state, and immune to any same-state retirement row
            // recorded since (`record_transition`'s `INSERT OR IGNORE`
            // only fires the first time). Resolve that row in place.
            Some((None, raised_state, raised_reason)) => {
                let original_reason =
                    Reason::parse(&raised_reason).map_err(|unknown| LedgerError::Corrupt {
                        what: unknown.what.into(),
                        detail: unknown.to_string(),
                    })?;
                let changed = tx.execute(
                    "UPDATE decisions SET resolved_at = ?2, resolution = ?3, actor = ?4, actor_raw = ?4,
                            note = ?5, candidate_contained = ?6, person_quote = ?7,
                            recorded_via = ?8
                     WHERE run = ?1 AND resolved_at IS NULL",
                    params![
                        run_id.as_str(),
                        now,
                        Reason::DecisionSalvaged.as_str(),
                        actor.as_str(),
                        note,
                        candidate_contained,
                        person_quote,
                        recorded_via.as_str(),
                    ],
                )?;
                debug_assert_eq!(changed, 1, "read as open under the same transaction above");
                (parse_state(&raised_state)?, original_reason)
            }
            // No decision row at all: this run never awaited a person, so
            // nothing opened one for it. Raise and resolve it here, in the
            // same write.
            None => {
                let last: Option<(String, String)> = tx
                    .query_row(
                        "SELECT to_state, reason FROM transitions
                         WHERE run_id = ?1 AND (from_state IS NULL OR from_state != to_state)
                         ORDER BY id DESC LIMIT 1",
                        [run_id.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((last_state, last_reason)) = last else {
                    return Ok(None);
                };
                let original_reason =
                    Reason::parse(&last_reason).map_err(|unknown| LedgerError::Corrupt {
                        what: unknown.what.into(),
                        detail: unknown.to_string(),
                    })?;
                let task_id: Option<String> = tx
                    .query_row(
                        "SELECT task_id FROM runs WHERE id = ?1",
                        [run_id.as_str()],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                let task_id = task_id.unwrap_or_else(|| format!("task-legacy-{}", run_id.as_str()));
                tx.execute(
                    "INSERT INTO decisions
                        (run, task, raised_at, raised_state, raised_reason, resolved_at, resolution, actor, note,
                         candidate_contained, actor_raw, person_quote, recorded_via)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?8, ?11, ?12)",
                    params![
                        run_id.as_str(),
                        task_id,
                        now,
                        last_state,
                        last_reason,
                        now,
                        Reason::DecisionSalvaged.as_str(),
                        actor.as_str(),
                        note,
                        candidate_contained,
                        person_quote,
                        recorded_via.as_str(),
                    ],
                )?;
                (parse_state(&last_state)?, original_reason)
            }
        };
        append_transition(
            &tx,
            &Transition {
                run_id: run_id.clone(),
                attempt_id: None,
                from_state: Some(from_state),
                to_state: State::AcceptedByPerson,
                reason: Reason::DecisionSalvaged.as_str().to_string(),
                detail: Some(serde_json::json!({ "candidate_sha": candidate_sha })),
                at: now.clone(),
            },
        )?;
        stamp_driver(&tx, run_id, &now)?;
        tx.commit()?;
        Ok(Some(original_reason))
    }

    /// Record that a person signed off on one acceptance criterion of
    /// one run — written only by `relais decide --answer approve
    /// --criterion <id>`. `INSERT OR REPLACE`: answering the same
    /// criterion twice updates who last signed it and when, rather than
    /// refusing or silently duplicating a row a later reader would have
    /// to pick one of.
    pub fn record_human_signoff(
        &self,
        run_id: &RunId,
        criterion_id: &str,
        actor: Actor,
        recorded_via: RecordedVia,
        note: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO human_signoffs
                (run_id, criterion_id, actor, actor_raw, note, recorded_at, recorded_via)
             VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
            params![
                run_id.as_str(),
                criterion_id,
                actor.as_str(),
                note,
                self.now(),
                recorded_via.as_str()
            ],
        )?;
        Ok(())
    }

    /// Mint the nonce that proves a plugin session's decide calls
    /// (ADR-0004): 32 bytes from the OS, returned once as hex; only its
    /// SHA-256 is stored, so reading the ledger never yields it. Minting
    /// again for a session replaces its nonce, and every nonce older than
    /// [`DECIDE_NONCE_TTL_DAYS`] is swept, whichever session it was for.
    pub fn mint_decide_nonce(&self, session: &str) -> Result<String> {
        let mut bytes = [0u8; 32];
        crate::procs::os_random(&mut bytes).map_err(|e| LedgerError::Corrupt {
            what: "the OS random source".into(),
            detail: e.to_string(),
        })?;
        let nonce: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let now = self.now();
        let cutoff = chrono::DateTime::parse_from_rfc3339(&now)
            .map(|t| (t - chrono::Duration::days(DECIDE_NONCE_TTL_DAYS)).to_rfc3339())
            .unwrap_or_else(|_| now.clone());
        let tx = self.write_tx()?;
        tx.execute("DELETE FROM decide_nonces WHERE created_at < ?1", [&cutoff])?;
        tx.execute(
            "INSERT OR REPLACE INTO decide_nonces (session, nonce_hash, created_at)
             VALUES (?1, ?2, ?3)",
            params![session, crate::ids::sha256_hex(nonce.as_bytes()), now],
        )?;
        tx.commit()?;
        Ok(nonce)
    }

    /// Forget a session's nonce: its session ended. `Ok(false)` when there
    /// was none.
    pub fn end_decide_nonce(&self, session: &str) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM decide_nonces WHERE session = ?1", [session])?;
        Ok(changed > 0)
    }

    /// Whether `nonce` is the live nonce of `session`:
    /// [`RecordedVia::Plugin`] when it is, [`RecordedVia::Cli`] for a
    /// wrong, ended or swept one.
    pub fn verify_decide_nonce(&self, session: &str, nonce: &str) -> Result<RecordedVia> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT nonce_hash FROM decide_nonces WHERE session = ?1",
                [session],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match stored {
            Some(hash) if hash == crate::ids::sha256_hex(nonce.as_bytes()) => RecordedVia::Plugin,
            _ => RecordedVia::Cli,
        })
    }
}

/// `advice_p_approve, advice_model, advice_revision, advice_null_reason`.
type StoredAdvice = (Option<f64>, Option<String>, Option<String>, Option<String>);

impl Actor {
    pub const ALL: [Actor; 3] = [
        Actor::Orchestrator,
        Actor::Person,
        Actor::PersonViaOrchestrator,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Actor::Orchestrator => "orchestrator",
            Actor::Person => "person",
            Actor::PersonViaOrchestrator => "person-via-orchestrator",
        }
    }

    /// Parse what the command line names. An old spelling is refused with
    /// the actor that replaces it; any other string with the three values.
    pub fn parse(text: &str) -> std::result::Result<Actor, String> {
        if let Some(actor) = Actor::ALL.into_iter().find(|a| a.as_str() == text) {
            return Ok(actor);
        }
        if let Some((_, actor)) = LEGACY_ACTORS.iter().find(|(old, _)| *old == text) {
            return Err(format!("`{text}` is now `{}`", actor.as_str()));
        }
        Err(format!(
            "unknown actor `{text}` — one of: {}",
            Actor::ALL.map(Actor::as_str).join(", ")
        ))
    }

    /// Read a stored actor leniently: a canonical value, or a legacy
    /// spelling mapped to its replacement.
    pub fn from_stored(text: &str) -> Option<Actor> {
        Actor::ALL
            .into_iter()
            .find(|a| a.as_str() == text)
            .or_else(|| {
                LEGACY_ACTORS
                    .iter()
                    .find(|(old, _)| *old == text)
                    .map(|(_, actor)| *actor)
            })
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl RecordedVia {
    pub const fn as_str(self) -> &'static str {
        match self {
            RecordedVia::Plugin => "plugin",
            RecordedVia::Cli => "cli",
        }
    }
}

/// The `event_id` prefix of the usage row of a Claude Code advisor call:
/// `advisor:<message id>#advisor<n>`. Such a row carries the advisor's own
/// model, never a requested one, and is no model a run or task used.
pub const ADVISOR_EVENT_PREFIX: &str = "advisor:";

impl Ledger {
    /// Salvaged runs whose `candidate_contained` was never computed, with
    /// the repository each ran in: what `dataset build` backfills once.
    pub fn salvages_without_containment(&self) -> Result<Vec<(RunId, PathBuf)>> {
        let mut stmt = self.conn.prepare(
            "SELECT decisions.run, runs.repo_path
               FROM decisions JOIN runs ON runs.id = decisions.run
              WHERE decisions.resolution = ?1 AND decisions.candidate_contained IS NULL
              ORDER BY decisions.run",
        )?;
        let rows = stmt.query_map([Reason::DecisionSalvaged.as_str()], |row| {
            Ok((
                RunId::from_stored(row.get::<_, String>(0)?),
                PathBuf::from(row.get::<_, String>(1)?),
            ))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Record the containment of a salvage whose answer is still NULL.
    /// Never overwrites a recorded answer: `Ok(false)` when there was
    /// nothing to fill.
    pub fn set_candidate_contained(&self, run_id: &RunId, contained: bool) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE decisions SET candidate_contained = ?2
              WHERE run = ?1 AND resolution = ?3 AND candidate_contained IS NULL",
            params![
                run_id.as_str(),
                contained,
                Reason::DecisionSalvaged.as_str()
            ],
        )?;
        Ok(changed > 0)
    }

    /// The candidate a person recorded merging when they salvaged a
    /// terminal run (SPEC's decision spine): the `candidate_sha`
    /// [`Self::record_salvage`] wrote into the [`State::AcceptedByPerson`]
    /// transition's detail. `None` for a run that was never salvaged —
    /// including one accepted the ordinary way, whose candidate `receipt`
    /// names instead.
    pub fn salvaged_candidate(&self, run_id: &RunId) -> Result<Option<String>> {
        let detail: Option<String> = self
            .conn
            .query_row(
                "SELECT detail_json FROM transitions
                 WHERE run_id = ?1 AND to_state = ?2
                 ORDER BY id DESC LIMIT 1",
                params![run_id.as_str(), State::AcceptedByPerson.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(detail) = detail else {
            return Ok(None);
        };
        let value: serde_json::Value =
            serde_json::from_str(&detail).map_err(|e| LedgerError::Corrupt {
                what: format!("the salvage detail of run {run_id}"),
                detail: e.to_string(),
            })?;
        Ok(value
            .get("candidate_sha")
            .and_then(|v| v.as_str())
            .map(str::to_string))
    }

    /// The advisor's stored advice for one run's decision, as JSON, or
    /// `null` when none was stored (no advisor yet: the columns exist, and
    /// nothing writes them).
    pub fn decision_advice(&self, run_id: &RunId) -> Result<serde_json::Value> {
        let row: Option<StoredAdvice> = self
            .conn
            .query_row(
                "SELECT advice_p_approve, advice_model, advice_revision, advice_null_reason
                   FROM decisions WHERE run = ?1",
                [run_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        Ok(match row {
            Some((p_approve, model, revision, null_reason))
                if p_approve.is_some() || null_reason.is_some() =>
            {
                serde_json::json!({
                    "p_approve": p_approve,
                    "model": model,
                    "revision": revision,
                    "null_reason": null_reason,
                })
            }
            _ => serde_json::Value::Null,
        })
    }

    /// Every criterion a person has signed off for one run, with who
    /// signed each — what `verify::acceptance_gaps`/`settle_acceptance`
    /// read back to stop treating an answered human-sign-off criterion
    /// as unmet.
    pub fn human_signoffs(&self, run_id: &RunId) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT criterion_id, actor FROM human_signoffs WHERE run_id = ?1 ORDER BY criterion_id",
        )?;
        let rows = stmt.query_map([run_id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

/// A `decisions` row as columns, parsed into a [`DecisionRecord`] (P6): an
/// unknown state or resolution is a `Corrupt` row the caller reports,
/// never a silently dropped or mismatched decision. `now` is the wait
/// clock for a row still open; a resolved row uses its own `resolved_at`
/// instead, so re-reading it later never changes what it says.
pub(super) fn parse_decision_row(row: DecisionRow, now: &str) -> Result<DecisionRecord> {
    let (
        run,
        task,
        raised_at,
        raised_state,
        raised_reason,
        resolved_at,
        resolution,
        actor,
        note,
        successor_run,
        candidate_contained,
    ) = row;
    let resolution = resolution
        .map(|stored| {
            Reason::parse(&stored).map_err(|unknown| LedgerError::Corrupt {
                what: unknown.what.into(),
                detail: unknown.to_string(),
            })
        })
        .transpose()?;
    let waited_seconds = seconds_between(&raised_at, resolved_at.as_deref().unwrap_or(now))?;
    Ok(DecisionRecord {
        run: RunId::from_stored(run),
        task: TaskId::from_stored(task),
        raised_at,
        raised_state: parse_state(&raised_state)?,
        raised_reason,
        resolved_at,
        resolution,
        actor,
        note,
        successor_run: successor_run.map(RunId::from_stored),
        candidate_contained,
        waited_seconds,
    })
}

/// Every actor spelling a ledger written before the typed [`Actor`] may
/// hold, and the actor it becomes. The v27 SQL `CASE` repeats this table;
/// a test runs each row through the migration so the two cannot drift.
pub const LEGACY_ACTORS: &[(&str, Actor)] = &[
    ("claude", Actor::Orchestrator),
    ("claude-orchestrator", Actor::Orchestrator),
    ("claude-session", Actor::Orchestrator),
    ("claude (for the person)", Actor::PersonViaOrchestrator),
    (
        "Frédéric Rousseau (via Claude Code)",
        Actor::PersonViaOrchestrator,
    ),
    ("fredericrous", Actor::Person),
    ("person", Actor::Person),
];

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::super::{now_rfc3339, ChildOf, LEDGER_SCHEMA_VERSION};
    use super::*;
    use crate::ids::PackageId;

    /// `record_transition` opens a decision row, in the same transaction,
    /// the moment a run lands on a state a person is owed a look at — and
    /// closes nothing itself.
    #[test]
    fn record_transition_opens_a_decision_for_a_state_awaiting_a_person() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-nr"), "/repo", None, &task("run-nr"), "rk")
            .expect("run");
        assert!(ledger
            .decision_of_run(&run("run-nr"))
            .expect("read")
            .is_none());
        ledger
            .record_transition(&Transition {
                run_id: run("run-nr"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsReview,
                reason: Reason::ReviewFindings.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        let decision = ledger
            .decision_of_run(&run("run-nr"))
            .expect("read")
            .expect("a decision was opened");
        assert_eq!(decision.raised_state, State::NeedsReview);
        assert_eq!(decision.raised_reason, Reason::ReviewFindings.as_str());
        assert!(decision.resolved_at.is_none());
        assert!(decision.resolution.is_none());
        assert!(decision.waited_seconds >= 0);
        let open = ledger.open_decisions().expect("open");
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].run, run("run-nr"));
        // A transition into a state nobody is owed a look at opens nothing.
        ledger
            .insert_run(&run("run-ok"), "/repo", None, &task("run-ok"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-ok"),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Running,
                reason: Reason::WorkerDispatched.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert!(ledger
            .decision_of_run(&run("run-ok"))
            .expect("read")
            .is_none());
        assert_eq!(ledger.open_decisions().expect("open").len(), 1);
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `resolve_decision` closes the open row, assigns the terminal
    /// state that answer carries, and is refused, by a plain `false` the
    /// caller reports by name, for a run with no open decision — never
    /// raised, already answered, or already terminal.
    #[test]
    fn resolve_decision_closes_once_and_refuses_a_run_not_waiting() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-d"), "/repo", None, &task("run-d"), "rk")
            .expect("run");
        assert!(!ledger
            .resolve_decision(
                &run("run-d"),
                &DecisionAnswer {
                    resolution: Reason::DecisionRecorded,
                    actor: Actor::Person,
                    person_quote: None,
                    recorded_via: crate::ledger::RecordedVia::Cli,
                    note: None,
                    successor_run: None,
                    from_state: State::Prepared,
                    to_state: State::Cancelled,
                },
            )
            .expect("resolve"));
        ledger
            .record_transition(&Transition {
                run_id: run("run-d"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::ScopeExceeded.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert!(ledger
            .resolve_decision(
                &run("run-d"),
                &DecisionAnswer {
                    resolution: Reason::DecisionApproved,
                    actor: Actor::Person,
                    person_quote: None,
                    recorded_via: crate::ledger::RecordedVia::Cli,
                    note: Some("looks fine"),
                    successor_run: Some(&run("run-successor")),
                    from_state: State::NeedsDecision,
                    to_state: State::Accepted,
                },
            )
            .expect("resolve"));
        let decision = ledger
            .decision_of_run(&run("run-d"))
            .expect("read")
            .expect("still on record");
        assert_eq!(decision.resolution, Some(Reason::DecisionApproved));
        assert_eq!(decision.actor.as_deref(), Some("person"));
        assert_eq!(decision.note.as_deref(), Some("looks fine"));
        assert_eq!(decision.successor_run, Some(run("run-successor")));
        assert!(decision.resolved_at.is_some());
        assert!(ledger.open_decisions().expect("open").is_empty());
        assert_eq!(
            ledger.run_status(&run("run-d")).expect("status"),
            Some(State::Accepted),
            "a person's approval assigns the state, not only the decision row"
        );
        // A second answer to the same run is refused: it is not waiting
        // any more.
        assert!(!ledger
            .resolve_decision(
                &run("run-d"),
                &DecisionAnswer {
                    resolution: Reason::DecisionRejected,
                    actor: Actor::Person,
                    person_quote: None,
                    recorded_via: crate::ledger::RecordedVia::Cli,
                    note: None,
                    successor_run: None,
                    from_state: State::Accepted,
                    to_state: State::Cancelled,
                },
            )
            .expect("resolve"));
        assert_eq!(
            ledger.run_status(&run("run-d")).expect("status"),
            Some(State::Accepted),
            "a refused second answer leaves the run's state exactly as the first left it"
        );
        // Best effort: the fixture is a temp dir; a leftover costs
        // nothing but disk, and the next run pre-cleans it.
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_first_recorded_landing_of_a_run_is_kept() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-l"), "/repo", None, &task("run-l"), "rk")
            .expect("run");
        assert_eq!(ledger.landing_of(&run("run-l")).expect("read"), None);
        ledger
            .record_landing(&run("run-l"), "aaa", "2026-09-05T12:00:00+00:00")
            .expect("first");
        ledger
            .record_landing(&run("run-l"), "bbb", "2026-09-06T12:00:00+00:00")
            .expect("second");
        assert_eq!(
            ledger.landing_of(&run("run-l")).expect("read"),
            Some(("aaa".to_string(), "2026-09-05T12:00:00+00:00".to_string()))
        );
        assert_eq!(
            ledger.schema_version().expect("version"),
            LEDGER_SCHEMA_VERSION
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_landing_search_cursor_is_kept_replaced_and_deleted() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-s"), "/repo", None, &task("run-s"), "rk")
            .expect("run");
        assert_eq!(ledger.landing_search_of(&run("run-s")).expect("read"), None);
        ledger
            .record_landing_search(&run("run-s"), "aaa")
            .expect("first");
        ledger
            .record_landing_search(&run("run-s"), "bbb")
            .expect("second");
        assert_eq!(
            ledger.landing_search_of(&run("run-s")).expect("read"),
            Some("bbb".to_string())
        );
        ledger.clear_landing_search(&run("run-s")).expect("delete");
        assert_eq!(ledger.landing_search_of(&run("run-s")).expect("read"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Containment is recorded with the salvage, stays unknown (NULL) when
    /// it could not be computed, and a backfill fills only a NULL.
    #[test]
    fn a_salvage_records_candidate_containment_and_backfill_only_fills_null() {
        let (ledger, dir) = temp_ledger();
        for name in ["run-yes", "run-unknown"] {
            ledger
                .insert_run(&run(name), "/repo", None, &task(name), "rk")
                .expect("run");
            ledger
                .record_transition(&Transition {
                    run_id: run(name),
                    attempt_id: None,
                    from_state: Some(State::Verifying),
                    to_state: State::NeedsReview,
                    reason: Reason::ReviewFindings.as_str().into(),
                    detail: None,
                    at: now_rfc3339(),
                })
                .expect("transition");
        }
        ledger
            .record_salvage_contained(
                &run("run-yes"),
                "abc",
                Answerer {
                    actor: Actor::Person,
                    person_quote: None,
                    recorded_via: RecordedVia::Cli,
                },
                None,
                Some(true),
            )
            .expect("salvage");
        ledger
            .record_salvage(&run("run-unknown"), "abc", Actor::Person, None)
            .expect("salvage");
        let contained = |name: &str| {
            ledger
                .decision_of_run(&run(name))
                .expect("read")
                .expect("row")
                .candidate_contained
        };
        assert_eq!(contained("run-yes"), Some(true));
        assert_eq!(contained("run-unknown"), None);
        let pending: Vec<_> = ledger
            .salvages_without_containment()
            .expect("pending")
            .into_iter()
            .map(|(run, _)| run)
            .collect();
        assert_eq!(pending, vec![run("run-unknown")]);
        assert!(!ledger
            .set_candidate_contained(&run("run-yes"), false)
            .expect("set"));
        assert!(ledger
            .set_candidate_contained(&run("run-unknown"), false)
            .expect("set"));
        assert_eq!(contained("run-yes"), Some(true));
        assert_eq!(contained("run-unknown"), Some(false));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A terminal run that never awaited a person — `record_transition`
    /// opens a decision row only for [`State::awaits_a_person`], and
    /// `failed` is not one — has no open decision for `resolve_decision`
    /// to close. `record_salvage` is the answer that reaches it anyway,
    /// keeping the run's own reason for ending rather than losing it
    /// under the salvage's own resolution (#49), and a second salvage of
    /// the same run is refused exactly like a second `resolve_decision`.
    #[test]
    fn record_salvage_keeps_the_original_reason_and_refuses_a_second_answer() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-s"), "/repo", None, &task("run-s"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-s"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::Failed,
                reason: Reason::BehavioralFailure.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        // A `failed` run opens no decision row: nothing for
        // `resolve_decision` to close.
        assert!(ledger
            .decision_of_run(&run("run-s"))
            .expect("read")
            .is_none());
        let original_reason = ledger
            .record_salvage(
                &run("run-s"),
                "abc123",
                Actor::Person,
                Some("merged by hand"),
            )
            .expect("salvage")
            .expect("a terminal run with no prior decision salvages");
        assert_eq!(original_reason, Reason::BehavioralFailure);
        assert_eq!(
            ledger.run_status(&run("run-s")).expect("status"),
            Some(State::AcceptedByPerson)
        );
        assert_eq!(
            ledger.salvaged_candidate(&run("run-s")).expect("candidate"),
            Some("abc123".to_string())
        );
        let decision = ledger
            .decision_of_run(&run("run-s"))
            .expect("read")
            .expect("the salvage recorded a decision row");
        assert_eq!(decision.raised_state, State::Failed);
        assert_eq!(
            decision.raised_reason,
            Reason::BehavioralFailure.as_str(),
            "the run's own reason for ending survives, not overwritten by the salvage"
        );
        assert_eq!(decision.resolution, Some(Reason::DecisionSalvaged));
        assert_eq!(decision.actor.as_deref(), Some("person"));
        assert!(decision.resolved_at.is_some());
        assert!(
            ledger.open_decisions().expect("open").is_empty(),
            "raised and resolved in the same write; never left open"
        );
        // A second salvage of the same run is refused: it already
        // carries a decision.
        assert!(ledger
            .record_salvage(&run("run-s"), "def456", Actor::Orchestrator, None)
            .expect("salvage")
            .is_none());
        assert_eq!(
            ledger.salvaged_candidate(&run("run-s")).expect("candidate"),
            Some("abc123".to_string()),
            "the refused second answer leaves the first candidate on record"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// #86: a run WAITING on a person (its decision row still OPEN) can
    /// be salvaged too — `record_salvage` used to refuse it outright,
    /// conflating "a person already answered" with "a row exists". This
    /// drives both halves of the split so the distinction cannot quietly
    /// collapse back into one `existing.is_some()`: an OPEN row resolves,
    /// a RESOLVED row still refuses exactly like a second answer.
    #[test]
    fn record_salvage_resolves_an_open_decision_but_refuses_a_resolved_one() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-w"), "/repo", None, &task("run-w"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-w"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::ScopeExceeded.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        // The run keeps recording transitions after it starts waiting —
        // the runner retiring its worktree writes a same-state row — and
        // that must not be mistaken for the reason the run stopped for.
        ledger
            .record_transition(&Transition {
                run_id: run("run-w"),
                attempt_id: None,
                from_state: Some(State::NeedsDecision),
                to_state: State::NeedsDecision,
                reason: "worktree_retired".into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert!(
            ledger
                .decision_of_run(&run("run-w"))
                .expect("read")
                .expect("needs_decision opened a row")
                .resolved_at
                .is_none(),
            "nobody has answered yet"
        );
        let original_reason = ledger
            .record_salvage(
                &run("run-w"),
                "abc123",
                Actor::Person,
                Some("merged by hand"),
            )
            .expect("salvage")
            .expect("an open decision salvages");
        assert_eq!(original_reason, Reason::ScopeExceeded);
        assert_eq!(
            ledger.run_status(&run("run-w")).expect("status"),
            Some(State::AcceptedByPerson)
        );
        assert_eq!(
            ledger.salvaged_candidate(&run("run-w")).expect("candidate"),
            Some("abc123".to_string())
        );
        let decision = ledger
            .decision_of_run(&run("run-w"))
            .expect("read")
            .expect("the row raised when the run started waiting");
        assert_eq!(decision.raised_state, State::NeedsDecision);
        assert_eq!(
            decision.raised_reason,
            Reason::ScopeExceeded.as_str(),
            "the reason the run started waiting for survives, not the worktree retirement"
        );
        assert_eq!(decision.resolution, Some(Reason::DecisionSalvaged));
        assert_eq!(decision.actor.as_deref(), Some("person"));
        assert!(decision.resolved_at.is_some());
        assert!(
            ledger.open_decisions().expect("open").is_empty(),
            "the row already open was resolved, not left open beside a second one"
        );

        // A decision already answered by a person — the ordinary way,
        // through `resolve_decision` — still refuses a salvage: this is
        // the second-answer guard the existing test above also covers,
        // and it must keep working once the open-row case no longer
        // shares its one `existing.is_some()` check.
        ledger
            .insert_run(&run("run-r"), "/repo", None, &task("run-r"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-r"),
                attempt_id: None,
                from_state: Some(State::Verifying),
                to_state: State::NeedsDecision,
                reason: Reason::ScopeExceeded.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        ledger
            .resolve_decision(
                &run("run-r"),
                &DecisionAnswer {
                    resolution: Reason::DecisionApproved,
                    actor: Actor::Person,
                    person_quote: None,
                    recorded_via: crate::ledger::RecordedVia::Cli,
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsDecision,
                    to_state: State::Accepted,
                },
            )
            .expect("resolve");
        assert!(
            ledger
                .record_salvage(&run("run-r"), "def456", Actor::Orchestrator, None)
                .expect("salvage")
                .is_none(),
            "a decision a person already answered still refuses a salvage"
        );
        assert_eq!(
            ledger.run_status(&run("run-r")).expect("status"),
            Some(State::Accepted),
            "the refused salvage leaves the person's own answer exactly as it left it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transitions_set_terminal_run_status() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-t"), "/repo", None, &task("run-t"), "rk")
            .expect("run");
        assert_eq!(
            ledger.run_status(&run("run-t")).expect("status"),
            Some(State::Prepared)
        );
        ledger
            .record_transition(&Transition {
                run_id: run("run-t"),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Blocked,
                reason: crate::lifecycle::Reason::BlockedPreflight.as_str().into(),
                detail: Some(serde_json::json!({"code": "missing_trust_grant"})),
                at: now_rfc3339(),
            })
            .expect("transition");
        assert_eq!(
            ledger.run_status(&run("run-t")).expect("status"),
            Some(State::Blocked)
        );
        let history = ledger.transitions(&run("run-t")).expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].to_state, State::Blocked);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn assert_driver_recorded(ledger: &Ledger, id: &str) {
        let (pid, started, since) = driver_of(ledger, id);
        assert_eq!(pid, Some(i64::from(std::process::id())), "{id}: pid");
        // What this platform can read: a token on Unix, `None` on Windows,
        // where no start-time reader exists.
        assert_eq!(
            started,
            crate::procs::start_time(std::process::id()),
            "{id}: start time"
        );
        assert!(since.is_some(), "{id}: since");
    }

    fn step(run_id: &str, from: State, to: State) -> Transition {
        Transition {
            run_id: run(run_id),
            attempt_id: None,
            from_state: Some(from),
            to_state: to,
            reason: crate::lifecycle::Reason::BlockedPreflight.as_str().into(),
            detail: None,
            at: now_rfc3339(),
        }
    }

    /// Every way a process takes a run records it as the driver.
    #[test]
    fn every_driver_entry_point_records_the_driver() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-root"), "/r", None, &task("run-root"), "rk")
            .expect("insert_run");
        assert_driver_recorded(&ledger, "run-root");

        ledger
            .insert_child_run(
                &run("run-child"),
                "/r",
                None,
                ChildOf {
                    parent_run: &run("run-root"),
                    package_id: &PackageId::from_stored("pkg-1"),
                },
                &task("run-root"),
                "rk",
            )
            .expect("insert_child_run");
        assert_driver_recorded(&ledger, "run-child");

        // Resume: a transition recorded by the re-entering process.
        ledger
            .insert_run(&run("run-resume"), "/r", None, &task("run-resume"), "rk")
            .expect("run");
        ledger
            .conn
            .execute(
                "UPDATE runs SET driver_pid = NULL, driver_started = NULL, driver_since = NULL",
                [],
            )
            .expect("clear");
        ledger
            .record_transition_as_driver(&step("run-resume", State::Prepared, State::Interrupted))
            .expect("resume");
        assert_driver_recorded(&ledger, "run-resume");

        // Decide re-entry: a person's answer.
        ledger
            .insert_run(&run("run-decide"), "/r", None, &task("run-decide"), "rk")
            .expect("run");
        ledger
            .record_transition(&step("run-decide", State::Prepared, State::NeedsDecision))
            .expect("raise");
        ledger
            .conn
            .execute(
                "UPDATE runs SET driver_pid = NULL, driver_started = NULL, driver_since = NULL",
                [],
            )
            .expect("clear");
        let answered = ledger
            .resolve_decision(
                &run("run-decide"),
                &DecisionAnswer {
                    resolution: Reason::DecisionApproved,
                    actor: Actor::Person,
                    person_quote: None,
                    recorded_via: crate::ledger::RecordedVia::Cli,
                    note: None,
                    successor_run: None,
                    from_state: State::NeedsDecision,
                    to_state: State::Accepted,
                },
            )
            .expect("decide");
        assert!(answered);
        assert_driver_recorded(&ledger, "run-decide");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transition_if_writes_nothing_when_the_state_differs() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-c"), "/r", None, &task("run-c"), "rk")
            .expect("run");
        let wrote = ledger
            .transition_if(
                State::Verifying,
                &step("run-c", State::Verifying, State::Blocked),
            )
            .expect("cas");
        assert!(!wrote);
        assert!(ledger
            .transitions(&run("run-c"))
            .expect("history")
            .is_empty());
        assert_eq!(
            ledger.run_status(&run("run-c")).expect("status"),
            Some(State::Prepared)
        );
        let wrote = ledger
            .transition_if(
                State::Prepared,
                &step("run-c", State::Prepared, State::Blocked),
            )
            .expect("cas");
        assert!(wrote);
        assert_eq!(
            ledger.run_status(&run("run-c")).expect("status"),
            Some(State::Blocked)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_concurrent_transition_ifs_leave_one_row() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-r"), "/r", None, &task("run-r"), "rk")
            .expect("run");
        let path = ledger.path().to_path_buf();
        let handles: Vec<_> = [State::Blocked, State::Interrupted]
            .into_iter()
            .map(|to| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let ledger = Ledger::open(&path).expect("open");
                    ledger
                        .transition_if(State::Prepared, &step("run-r", State::Prepared, to))
                        .expect("cas")
                })
            })
            .collect();
        let wrote: Vec<bool> = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect();
        assert_eq!(wrote.iter().filter(|w| **w).count(), 1, "{wrote:?}");
        assert_eq!(ledger.transitions(&run("run-r")).expect("history").len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A driver whose run was reaped to interrupted from outside writes
    /// no row when it later tries to move the run on.
    #[test]
    fn a_driver_transition_after_a_reap_writes_no_row() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-g"), "/r", None, &task("run-g"), "rk")
            .expect("run");
        ledger
            .transition_if(
                State::Prepared,
                &step("run-g", State::Prepared, State::Verifying),
            )
            .expect("driver moves");
        // The reaper.
        assert!(ledger
            .transition_if(
                State::Verifying,
                &step("run-g", State::Verifying, State::Interrupted)
            )
            .expect("reap"));
        // The driver's own next and final transitions.
        for to in [State::Accepted, State::Failed] {
            assert!(!ledger
                .transition_if(State::Verifying, &step("run-g", State::Verifying, to))
                .expect("late"));
        }
        let history = ledger.transitions(&run("run-g")).expect("history");
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].to_state, State::Interrupted);
        assert_eq!(
            ledger.run_status(&run("run-g")).expect("status"),
            Some(State::Interrupted)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P3: `run_status` is a projection of the `transitions` table, not a
    /// column a writer could leave behind it. A run's status is the last
    /// transition's `to_state` even when an earlier write left the `runs`
    /// row's own `status` column stale — there is no code path that can
    /// do that through the public API, and this proves it: the two can
    /// never disagree because one is derived from the other.
    #[test]
    fn run_status_can_never_disagree_with_the_transition_chain() {
        let (ledger, dir) = temp_ledger();
        ledger
            .insert_run(&run("run-p"), "/repo", None, &task("run-p"), "rk")
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-p"),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Running,
                reason: crate::lifecycle::Reason::WorkerDispatched.as_str().into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert_eq!(
            ledger.run_status(&run("run-p")).expect("status"),
            Some(State::Running),
            "status follows the last transition, not the first"
        );
        // Simulate the only way the column and the chain could ever
        // differ: something wrote the `runs` row directly, bypassing
        // `record_transition`. The chain still has the truth.
        ledger
            .conn
            .execute("UPDATE runs SET status = 'prepared' WHERE id = 'run-p'", [])
            .expect("stale column write");
        assert_eq!(
            ledger.run_status(&run("run-p")).expect("status"),
            Some(State::Running),
            "a stale `runs.status` column cannot outvote the transition chain"
        );
        ledger
            .record_transition(&Transition {
                run_id: run("run-p"),
                attempt_id: None,
                from_state: Some(State::Running),
                to_state: State::Verifying,
                reason: crate::lifecycle::Reason::VerificationStarted
                    .as_str()
                    .into(),
                detail: None,
                at: now_rfc3339(),
            })
            .expect("transition");
        assert_eq!(
            ledger.run_status(&run("run-p")).expect("status"),
            Some(State::Verifying),
            "status moves with every new transition, never lagging behind one"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only a `person` answer or a plugin-proved proxy on a
    /// needs_decision/needs_review run counts; legacy and cli proxies do not.
    #[test]
    fn person_answers_count_only_what_the_person_said() {
        let dir = crate::test_support::temp_dir("ledger-person-answers");
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("ledger");
        let row = |run: &str, state: &str, actor: &str, via: &str, at: &str| {
            ledger
                .conn
                .execute(
                    "INSERT INTO decisions (run, task, raised_at, raised_state, raised_reason,
                         resolved_at, resolution, actor, actor_raw, recorded_via)
                     VALUES (?1, 't', ?5, ?2, 'x', ?5, 'decision_revised', ?3, ?3, ?4)",
                    params![run, state, actor, via, at],
                )
                .expect("row");
        };
        row(
            "r1",
            "needs_decision",
            "person",
            "cli",
            "2026-10-10T00:00:00+00:00",
        );
        row(
            "r2",
            "needs_review",
            "person-via-orchestrator",
            "plugin",
            "2026-10-10T00:00:00+00:00",
        );
        row(
            "r3",
            "needs_review",
            "person-via-orchestrator",
            "cli",
            "2026-10-10T00:00:00+00:00",
        );
        row(
            "r4",
            "needs_review",
            "person",
            "legacy",
            "2026-10-10T00:00:00+00:00",
        );
        row(
            "r5",
            "interrupted",
            "person",
            "cli",
            "2026-10-10T00:00:00+00:00",
        );
        row(
            "r6",
            "needs_review",
            "orchestrator",
            "plugin",
            "2026-10-10T00:00:00+00:00",
        );
        row(
            "r7",
            "needs_decision",
            "person",
            "cli",
            "2026-01-01T00:00:00+00:00",
        );
        let counts = ledger
            .person_answer_counts("2026-09-01T00:00:00+00:00")
            .expect("counts");
        assert_eq!(
            counts,
            PersonAnswerCounts {
                total: 3,
                recent_person: 1,
                recent_proxied: 1,
                unproved_proxied: 1,
            }
        );
    }

    #[test]
    fn actor_parse_names_the_replacement_and_lists_the_three_values() {
        for actor in Actor::ALL {
            assert_eq!(Actor::parse(actor.as_str()), Ok(actor));
        }
        assert!(Actor::parse("claude")
            .unwrap_err()
            .contains("`orchestrator`"));
        let unknown = Actor::parse("bob").unwrap_err();
        for actor in Actor::ALL {
            assert!(unknown.contains(actor.as_str()), "{unknown}");
        }
    }
}
