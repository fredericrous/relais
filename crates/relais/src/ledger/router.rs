//! The session router's tables (ledger step v20, SPEC §30): what `relais
//! native router-observe` writes and what `router-state` and `relais
//! report` read back. What a record means, and how one outcome ranks
//! against another, is `crate::router`'s; a task row arrives here carrying
//! its rank. The one rule this module owns is the rank-0 merge: between
//! `answered` and `unknown`, `unknown` wins in either arrival order, and a
//! task with files is never `answered`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{Ledger, Result};

/// The `outcome_source` migration v45 marks the rows it backfilled with.
const BACKFILL_V45_SOURCE: &str = "backfill_v45";
/// The `outcome_source` of an outcome the plugin observed itself: the
/// upsert's default, and the one source `router::explore` counts a
/// question's `answered` under.
pub const OWN_SOURCE: &str = "own";

/// The reassess event of a correction.
const CORRECTION_EVENT: &str = "correction";
/// The most a correction is charged back from its time: 24 hours.
const CORRECTION_LOOKBACK_HOURS: i64 = 24;

/// The most unmatched check commands a task keeps.
pub const UNMATCHED_CHECKS_MAX: usize = 5;

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
    /// The capability table's version the decision was made under; absent
    /// from a plugin that predates the field.
    pub table_version: Option<u32>,
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
    /// Hashed repo-relative paths (a `revert`'s files), sorted.
    pub files: Vec<String>,
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
    /// The outcome when the task ended: set once, never moved by a later
    /// record, so a later correction can be compared with it.
    pub completed_at_end: Option<String>,
    /// The outcome the task would have had without its corrections, as the
    /// plugin computed it at the end: set once from the first ended record,
    /// never moved by a later one. For gate 2, a task in
    /// [`corrections_charged_away`] has this as its outcome (unknown when
    /// `None`), and a subagent whose `outcome_source` is `parent` takes its
    /// parent's the same way.
    pub outcome_uncorrected: Option<String>,
    pub inferred: Vec<String>,
    /// Hashed repo-relative paths of the files the task edited, sorted.
    pub files: Vec<String>,
    pub escalations: u32,
    pub exhausted: bool,
    pub turns: u32,
    pub explicit_quote: Option<String>,
    /// Where `outcome` came from: `own` (this task's record) or `parent`
    /// (its parent's end, see [`RouterTaskOutcomeRow`]).
    pub outcome_source: Option<String>,
    /// The checks the task ran, with their kinds.
    pub checks: Vec<RouterCheck>,
    /// The task that spawned this one; none for a main task, an unparented
    /// spawn, or a row recorded before the plugin sent the link.
    pub parent_task_id: Option<String>,
    /// Whether the plugin that wrote the record captured the parent link:
    /// a missing parent on a `false` row is "not captured", not "no parent".
    pub parent_link_recorded: bool,
    /// Build or test commands the task ran that matched no check, as
    /// `<repository>: <argv prefix>`, at most [`UNMATCHED_CHECKS_MAX`].
    pub unmatched_checks: Vec<String>,
}

/// One check a task ran: a command or an amont gate stamp, with its kind
/// (`test`, `lint`, `build`, `gate`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouterCheck {
    pub name: String,
    pub kind: String,
    pub passed: bool,
}

/// A parent task's outcome sent for a subagent task that ended before it:
/// it replaces the subagent's stored outcome only when that is `unknown`.
#[derive(Debug, Clone, PartialEq)]
pub struct RouterTaskOutcomeRow {
    pub task_id: String,
    pub agent_id: Option<String>,
    pub outcome: String,
    pub outcome_rank: u8,
    pub at: String,
}

/// One classifier call, keyed `(session, call_id)`. It holds everything but
/// the call's text: `input_sha256` stands for it (the text is in a local
/// day file, `router::capture`).
#[derive(Debug, Clone, PartialEq)]
pub struct RouterClassificationRow {
    pub call_id: String,
    /// `prompt` or `spawn`.
    pub purpose: String,
    pub model: String,
    /// SHA-256 of the classifier's system prompt.
    pub prompt_version: String,
    pub input_sha256: String,
    /// The parsed answer, every field, as JSON; `None` when it did not parse.
    pub answer_json: Option<String>,
    pub parse_ok: bool,
    pub latency_ms: Option<u64>,
    pub at: String,
}

/// One record of a `router-observe` batch.
#[derive(Debug, Clone, PartialEq)]
pub enum RouterRecord {
    Classification(RouterClassificationRow),
    Decision(RouterDecisionRow),
    Usage(RouterUsageRow),
    Reassess(RouterReassessRow),
    Task(RouterTaskRow),
    TaskOutcome(RouterTaskOutcomeRow),
}

/// One `router_provenance` row: an envelope written or removed, with who
/// did it from where.
#[derive(Debug, Clone, PartialEq)]
pub enum RouterProvenance {
    Envelope {
        by: String,
        epsilon_max: f64,
        source: String,
        at: String,
    },
    EnvelopeRemoved {
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

/// One task the draw could apply to, as the exploration evidence reads it:
/// a main-session new task or a subagent spawn (`relation`), its class, how
/// it ended and what the draw did at its start.
#[derive(Debug, Clone, PartialEq)]
pub struct ExplorationTask {
    /// `new_task` (the person's session) or `subagent` (a spawn).
    pub relation: String,
    pub kind: String,
    pub difficulty: u8,
    pub scope: String,
    pub outcome: String,
    /// Where the outcome came from (`own`, `parent`, `backfill_v45`, ...).
    pub outcome_source: Option<String>,
    pub started_at: String,
    /// The draw lowered the tier (`explored`) and the switch was applied.
    pub drawn: bool,
    /// The draw ran and passed the task over: eligible (a propensity was
    /// recorded), not lowered, not held out.
    pub undrawn: bool,
}

/// One time the exploration stop rule fired, with the counts it saw
/// (drawn and undrawn tasks with strong outcomes in the strata the draws
/// fall in; `*_bad` are the corrected ones).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RouterExplorationStop {
    pub at: String,
    pub drawn: usize,
    pub drawn_bad: usize,
    pub undrawn: usize,
    pub undrawn_bad: usize,
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

/// `extra` added to `list` once each, sorted.
fn union(mut list: Vec<String>, extra: &[String]) -> Vec<String> {
    for item in extra {
        if !list.contains(item) {
            list.push(item.clone());
        }
    }
    list.sort();
    list
}

/// A stored outcome of rank 0, the only ones the merge rewrites. Parsed
/// once from the `outcome` column; a stronger outcome never reaches the
/// rank-0 rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RankZeroOutcome {
    Answered,
    Unknown,
}

impl RankZeroOutcome {
    /// The rank-0 outcome the merge rules cover for `text`, if any.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "answered" => Some(Self::Answered),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Unknown => "unknown",
        }
    }
}

/// The outcome after a merge at rank 0: `unknown` wins over `answered` in
/// either arrival order, and a task with files is never `answered`: the
/// plugin's rule is monotone within a task, so once a write is seen the
/// task reports `unknown`.
fn rank_zero_outcome(
    picked: RankZeroOutcome,
    stored: RankZeroOutcome,
    incoming: RankZeroOutcome,
    files: &[String],
) -> RankZeroOutcome {
    let either_unknown = stored == RankZeroOutcome::Unknown || incoming == RankZeroOutcome::Unknown;
    let answered_with_files = picked == RankZeroOutcome::Answered && !files.is_empty();
    if either_unknown || answered_with_files {
        RankZeroOutcome::Unknown
    } else {
        picked
    }
}

/// The outcome name after a merge: `picked` run through `rank_zero_outcome`
/// when all three names are rank-0 outcomes, else (a stronger outcome, or
/// a name this binary does not know) left as it is.
fn merged_outcome(picked: &str, stored: &str, incoming: &str, files: &[String]) -> String {
    match (
        RankZeroOutcome::parse(picked),
        RankZeroOutcome::parse(stored),
        RankZeroOutcome::parse(incoming),
    ) {
        (Some(picked), Some(stored), Some(incoming)) => {
            rank_zero_outcome(picked, stored, incoming, files)
                .as_str()
                .to_string()
        }
        _ => picked.to_string(),
    }
}

/// The first record of a task, stored as it came but for the rank-0 rule:
/// an `answered` record that carries files is stored `unknown`, end
/// outcome included, as a merge would store it.
fn first_task(incoming: &RouterTaskRow) -> RouterTaskRow {
    let outcome = merged_outcome(
        &incoming.outcome,
        &incoming.outcome,
        &incoming.outcome,
        &incoming.files,
    );
    let outcome_uncorrected = incoming
        .ended_at
        .as_ref()
        .and(incoming.outcome_uncorrected.clone());
    if outcome == incoming.outcome {
        return RouterTaskRow {
            outcome_uncorrected,
            ..incoming.clone()
        };
    }
    let completed_at_end = match incoming
        .completed_at_end
        .as_deref()
        .map(RankZeroOutcome::parse)
    {
        Some(Some(RankZeroOutcome::Answered)) => Some(outcome.clone()),
        Some(Some(RankZeroOutcome::Unknown)) | Some(None) | None => {
            incoming.completed_at_end.clone()
        }
    };
    RouterTaskRow {
        outcome,
        completed_at_end,
        outcome_uncorrected,
        ..incoming.clone()
    }
}

/// `incoming` folded onto `stored`: the outcome moves only up the rank,
/// counters only up, `exhausted`, the inferred signals and the files only
/// gain, and the outcome at the end is kept from the first record that
/// had one. Order-independent for everything but that first end outcome.
fn merge_task(stored: RouterTaskRow, incoming: &RouterTaskRow) -> RouterTaskRow {
    let promote = incoming.outcome_rank > stored.outcome_rank;
    let inferred = union(stored.inferred, &incoming.inferred);
    let files = union(stored.files, &incoming.files);
    let outcome_rank = stored.outcome_rank.max(incoming.outcome_rank);
    let picked = if promote {
        &incoming.outcome
    } else {
        &stored.outcome
    };
    let outcome = merged_outcome(picked, &stored.outcome, &incoming.outcome, &files);
    let forced_unknown =
        RankZeroOutcome::parse(&outcome) == Some(RankZeroOutcome::Unknown) && !promote;
    let completed_at_end = match stored
        .completed_at_end
        .or(incoming.completed_at_end.clone())
    {
        Some(end)
            if forced_unknown
                && RankZeroOutcome::parse(&end) == Some(RankZeroOutcome::Answered) =>
        {
            Some(RankZeroOutcome::Unknown.as_str().to_string())
        }
        end => end,
    };
    let backfilled = stored.outcome_source.as_deref() == Some(BACKFILL_V45_SOURCE);
    let outcome_source_kept = if promote || (forced_unknown && backfilled) {
        incoming.outcome_source.clone().or(stored.outcome_source)
    } else {
        stored.outcome_source.or(incoming.outcome_source.clone())
    };
    let outcome_uncorrected = stored.outcome_uncorrected.or_else(|| {
        incoming
            .ended_at
            .as_ref()
            .and(incoming.outcome_uncorrected.clone())
    });
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
        outcome,
        outcome_rank,
        completed_at_end,
        outcome_uncorrected,
        inferred,
        files,
        escalations: stored.escalations.max(incoming.escalations),
        exhausted: stored.exhausted || incoming.exhausted,
        turns: stored.turns.max(incoming.turns),
        explicit_quote: if promote {
            incoming.explicit_quote.clone().or(stored.explicit_quote)
        } else {
            stored.explicit_quote.or(incoming.explicit_quote.clone())
        },
        outcome_source: outcome_source_kept,
        checks: if incoming.checks.is_empty() {
            stored.checks
        } else {
            incoming.checks.clone()
        },
        parent_task_id: stored.parent_task_id.or(incoming.parent_task_id.clone()),
        parent_link_recorded: stored.parent_link_recorded || incoming.parent_link_recorded,
        unmatched_checks: {
            let mut unmatched = union(stored.unmatched_checks, &incoming.unmatched_checks);
            unmatched.truncate(UNMATCHED_CHECKS_MAX);
            unmatched
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
     exhausted, turns, explicit_quote, completed_at_end, files_json, outcome_source, checks_json, \
     parent_task_id, parent_link_recorded, unmatched_checks_json, outcome_uncorrected";

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
                             would_pass_gate, at, recorded_at, table_version)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                                 ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)
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
                            at = excluded.at, table_version = excluded.table_version",
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
                            d.table_version,
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
                    let files = if r.event == CORRECTION_EVENT {
                        let stored: Option<String> = tx
                            .query_row(
                                "SELECT files_json FROM router_reassess
                                  WHERE session = ?1 AND task_id = ?2 AND agent_id = ?3
                                    AND event = ?4 AND at = ?5",
                                params![session, r.task_id, key(&r.agent_id), r.event, r.at],
                                |row| row.get(0),
                            )
                            .optional()?;
                        let stored_files: Vec<String> = match stored {
                            Some(text) => serde_json::from_str(&text).unwrap_or_default(),
                            None => Vec::new(),
                        };
                        union(stored_files, &r.files)
                    } else {
                        r.files.clone()
                    };
                    let files =
                        serde_json::to_string(&files).expect("a list of strings serializes");
                    tx.execute(
                        "INSERT INTO router_reassess
                            (session, task_id, agent_id, event, tier_from, tier_to, effort_to,
                             escalating, files_json, at, recorded_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                         ON CONFLICT (session, task_id, agent_id, event, at) DO UPDATE SET
                            files_json = CASE WHEN excluded.event = 'correction'
                                              THEN excluded.files_json
                                              ELSE router_reassess.files_json END",
                        params![
                            session,
                            r.task_id,
                            key(&r.agent_id),
                            r.event,
                            r.tier_from,
                            r.tier_to,
                            r.effort_to,
                            r.escalating,
                            files,
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
                        None => first_task(t),
                    };
                    let (kind, difficulty, scope, uncertainty, verifiable, confidence) =
                        class_columns(&merged.class);
                    let inferred = serde_json::to_string(&merged.inferred)
                        .expect("a list of strings serializes");
                    let files =
                        serde_json::to_string(&merged.files).expect("a list of strings serializes");
                    let checks =
                        serde_json::to_string(&merged.checks).expect("a list of checks serializes");
                    let unmatched = serde_json::to_string(&merged.unmatched_checks)
                        .expect("a list of strings serializes");
                    tx.execute(
                        "INSERT INTO router_tasks
                            (session, task_id, agent_id, started_at, ended_at, class_kind,
                             difficulty, scope, uncertainty, verifiable, confidence, outcome,
                             outcome_rank, inferred_json, escalations, exhausted, turns,
                             explicit_quote, completed_at_end, files_json, recorded_at,
                             updated_at, outcome_source, checks_json, parent_task_id,
                             parent_link_recorded, unmatched_checks_json, outcome_uncorrected)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21, ?22, ?23, ?24, ?25, ?26,
                                 ?27)
                         ON CONFLICT (session, task_id, agent_id) DO UPDATE SET
                            started_at = excluded.started_at, ended_at = excluded.ended_at,
                            class_kind = excluded.class_kind, difficulty = excluded.difficulty,
                            scope = excluded.scope, uncertainty = excluded.uncertainty,
                            verifiable = excluded.verifiable, confidence = excluded.confidence,
                            outcome = excluded.outcome, outcome_rank = excluded.outcome_rank,
                            inferred_json = excluded.inferred_json,
                            escalations = excluded.escalations, exhausted = excluded.exhausted,
                            turns = excluded.turns, explicit_quote = excluded.explicit_quote,
                            completed_at_end = excluded.completed_at_end,
                            files_json = excluded.files_json,
                            updated_at = excluded.updated_at,
                            outcome_source = excluded.outcome_source,
                            checks_json = excluded.checks_json,
                            parent_task_id = excluded.parent_task_id,
                            parent_link_recorded = excluded.parent_link_recorded,
                            unmatched_checks_json = excluded.unmatched_checks_json,
                            outcome_uncorrected = excluded.outcome_uncorrected",
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
                            merged.completed_at_end,
                            files,
                            now,
                            merged.outcome_source.as_deref().unwrap_or(OWN_SOURCE),
                            checks,
                            merged.parent_task_id,
                            merged.parent_link_recorded,
                            unmatched,
                            merged.outcome_uncorrected,
                        ],
                    )?;
                }
                RouterRecord::Classification(c) => {
                    tx.execute(
                        "INSERT INTO router_classifications
                            (session, call_id, purpose, model, prompt_version, input_sha256,
                             answer_json, parse_ok, latency_ms, at, recorded_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                         ON CONFLICT (session, call_id) DO UPDATE SET
                            purpose = excluded.purpose, model = excluded.model,
                            prompt_version = excluded.prompt_version,
                            input_sha256 = excluded.input_sha256,
                            answer_json = excluded.answer_json, parse_ok = excluded.parse_ok,
                            latency_ms = excluded.latency_ms, at = excluded.at",
                        params![
                            session,
                            c.call_id,
                            c.purpose,
                            c.model,
                            c.prompt_version,
                            c.input_sha256,
                            c.answer_json,
                            c.parse_ok,
                            c.latency_ms.map(saturating_i64),
                            c.at,
                            now,
                        ],
                    )?;
                }
                RouterRecord::TaskOutcome(o) => {
                    // Only over a stored `unknown`, and only with a real
                    // outcome; a task that has no row yet is left alone.
                    tx.execute(
                        "UPDATE router_tasks
                            SET outcome = ?1, outcome_rank = ?2, outcome_source = 'parent',
                                updated_at = ?3
                          WHERE session = ?4 AND task_id = ?5 AND agent_id = ?6
                            AND outcome IN ('unknown', 'answered') AND outcome_rank < ?2",
                        params![
                            o.outcome,
                            o.outcome_rank,
                            now,
                            session,
                            o.task_id,
                            key(&o.agent_id),
                        ],
                    )?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// One row per main-session correction reassess row, with the earlier
    /// tasks it is charged to (see [`CorrectionCharge`]). A task A is
    /// charged when it is a main task other than B, completed verified or
    /// accepted at its end, ended within 24 hours before the row's `at`
    /// (any session, compared as UTC instants; a row whose time does not
    /// parse is skipped), and shares a file hash with the row's files
    /// outside `stop`. `stop` is filtered here, not bound to SQL.
    // holds-until: ~10k main tasks in the ledger — every correction row scans every candidate; index by end time after
    pub fn router_correction_charges(&self, stop: &[String]) -> Result<Vec<CorrectionCharge>> {
        let stop: BTreeSet<&str> = stop.iter().map(String::as_str).collect();
        let mut candidates = Vec::new();
        let mut statement = self.conn.prepare(
            "SELECT session, task_id, ended_at, files_json FROM router_tasks
              WHERE agent_id = '' AND ended_at IS NOT NULL
                AND completed_at_end IN ('completed_verified', 'completed_accepted')",
        )?;
        let task_rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for task_row in task_rows {
            let (session, task_id, ended_at, files_json) = task_row?;
            let Some(ended_at) = parse_utc(&ended_at) else {
                continue;
            };
            let files: Vec<String> = serde_json::from_str(&files_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            candidates.push(ChargeCandidate {
                key: TaskKey { session, task_id },
                ended_at,
                files: files.into_iter().collect(),
            });
        }
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT session, task_id FROM router_reassess
              WHERE agent_id = '' AND event = 'flag'",
        )?;
        let flagged: BTreeSet<TaskKey> = statement
            .query_map([], |row| {
                Ok(TaskKey {
                    session: row.get(0)?,
                    task_id: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut statement = self.conn.prepare(
            "SELECT session, task_id, at, files_json FROM router_reassess
              WHERE agent_id = '' AND event = 'correction'
              ORDER BY session, task_id, at",
        )?;
        let correction_rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut charges = Vec::new();
        for correction_row in correction_rows {
            let (session, task_id, at, files_json) = correction_row?;
            let Some(at_instant) = parse_utc(&at) else {
                continue;
            };
            let files: Vec<String> = serde_json::from_str(&files_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            let legacy = files.is_empty();
            let fixed: Vec<&String> = files
                .iter()
                .filter(|hash| !stop.contains(hash.as_str()))
                .collect();
            let window_start = at_instant - Duration::hours(CORRECTION_LOOKBACK_HOURS);
            let mut charged: Vec<TaskKey> = candidates
                .iter()
                .filter(|a| {
                    !(a.key.session == session && a.key.task_id == task_id)
                        && a.ended_at >= window_start
                        && a.ended_at < at_instant
                        && fixed.iter().any(|hash| a.files.contains(*hash))
                })
                .map(|a| a.key.clone())
                .collect();
            charged.sort();
            let task = TaskKey {
                session: session.clone(),
                task_id: task_id.clone(),
            };
            charges.push(CorrectionCharge {
                flagged: flagged.contains(&task),
                session,
                task_id,
                at,
                charged,
                legacy,
            });
        }
        Ok(charges)
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

    /// How many classifier calls are kept, and how many of them parsed:
    /// the count the first Laya run waits on.
    pub fn router_classification_counts(&self) -> Result<(u64, u64)> {
        let (all, parsed): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(parse_ok), 0) FROM router_classifications",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((all.max(0) as u64, parsed.max(0) as u64))
    }

    /// Every kept classifier call that parsed, `at` or later, oldest
    /// first. The text is not here (see [`RouterClassificationRow`]).
    pub fn router_classifications_parsed(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<(String, RouterClassificationRow)>> {
        let mut stmt = self.conn.prepare(
            "SELECT session, call_id, purpose, model, prompt_version, input_sha256,
                    answer_json, parse_ok, latency_ms, at
               FROM router_classifications
              WHERE parse_ok = 1 AND answer_json IS NOT NULL AND at >= ?1
              ORDER BY at, session, call_id",
        )?;
        let rows = stmt.query_map(params![since.unwrap_or("")], |row| {
            Ok((
                row.get::<_, String>(0)?,
                RouterClassificationRow {
                    call_id: row.get(1)?,
                    purpose: row.get(2)?,
                    model: row.get(3)?,
                    prompt_version: row.get(4)?,
                    input_sha256: row.get(5)?,
                    answer_json: row.get(6)?,
                    parse_ok: row.get(7)?,
                    latency_ms: row.get::<_, Option<i64>>(8)?.map(|n| n.max(0) as u64),
                    at: row.get(9)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

    /// Every main-session task that has a class and a `new_task` decision,
    /// with what the exploration draw did at its start: drawn (lowered and
    /// applied) or undrawn (the draw ran, passed it over, not held out).
    ///
    /// Two arms: the main session's `new_task` decisions, and subagent
    /// spawns. A spawn is joined on `(session, task_id)` alone: its decision
    /// row is written before the spawn is keyed (`agent_id` empty) while its
    /// task row may carry the real agent id, and a spawn that never keys
    /// keeps the empty id on both, so any `agent_id` condition would drop
    /// drawn spawns from the stop rule. Spawn task ids are unique per session.
    pub fn router_exploration_tasks(&self) -> Result<Vec<ExplorationTask>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.relation, t.class_kind, t.difficulty, t.scope, t.outcome, t.started_at,
                    d.explored, d.applied, d.propensity, d.holdout, t.outcome_source
               FROM router_tasks t
               JOIN router_decisions d
                 ON d.session = t.session AND d.task_id = t.task_id
                AND d.agent_id = '' AND d.relation = 'new_task'
              WHERE t.agent_id = '' AND t.class_kind IS NOT NULL AND t.scope IS NOT NULL
                AND t.difficulty IS NOT NULL
             UNION ALL
             SELECT d.relation, t.class_kind, t.difficulty, t.scope, t.outcome, t.started_at,
                    d.explored, d.applied, d.propensity, d.holdout, t.outcome_source
               FROM router_tasks t
               JOIN router_decisions d
                 ON d.session = t.session AND d.task_id = t.task_id
                AND d.relation = 'subagent'
              WHERE t.class_kind IS NOT NULL AND t.scope IS NOT NULL
                AND t.difficulty IS NOT NULL
              ORDER BY 6, 1",
        )?;
        let rows = stmt.query_map([], |row| {
            let explored: bool = row.get(6)?;
            let applied: bool = row.get(7)?;
            let propensity: Option<f64> = row.get(8)?;
            let holdout: bool = row.get(9)?;
            Ok(ExplorationTask {
                relation: row.get(0)?,
                kind: row.get(1)?,
                difficulty: row.get::<_, i64>(2)?.clamp(0, 255) as u8,
                scope: row.get(3)?,
                outcome: row.get(4)?,
                started_at: row.get(5)?,
                outcome_source: row.get(10)?,
                drawn: explored && applied && !holdout,
                undrawn: !explored && propensity.is_some() && !holdout,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record that the stop rule fired, unless the latest stop row already
    /// holds these counts (the rule is re-read at every session start).
    /// Whether a row was written.
    pub fn record_router_exploration_stop(&self, stop: &RouterExplorationStop) -> Result<bool> {
        if let Some(latest) = self.router_exploration_stops()?.pop() {
            if (
                latest.drawn,
                latest.drawn_bad,
                latest.undrawn,
                latest.undrawn_bad,
            ) == (stop.drawn, stop.drawn_bad, stop.undrawn, stop.undrawn_bad)
            {
                return Ok(false);
            }
        }
        self.conn.execute(
            "INSERT INTO router_exploration_stops (at, drawn, drawn_bad, undrawn, undrawn_bad)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                stop.at,
                stop.drawn as i64,
                stop.drawn_bad as i64,
                stop.undrawn as i64,
                stop.undrawn_bad as i64
            ],
        )?;
        Ok(true)
    }

    /// Every stop row, oldest first.
    pub fn router_exploration_stops(&self) -> Result<Vec<RouterExplorationStop>> {
        let mut stmt = self.conn.prepare(
            "SELECT at, drawn, drawn_bad, undrawn, undrawn_bad
               FROM router_exploration_stops ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RouterExplorationStop {
                at: row.get(0)?,
                drawn: row.get::<_, i64>(1)?.max(0) as usize,
                drawn_bad: row.get::<_, i64>(2)?.max(0) as usize,
                undrawn: row.get::<_, i64>(3)?.max(0) as usize,
                undrawn_bad: row.get::<_, i64>(4)?.max(0) as usize,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Record an envelope write or removal.
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
            RouterProvenance::EnvelopeRemoved { source, at } => self.conn.execute(
                "INSERT INTO router_provenance (kind, source, at)
                 VALUES ('envelope_off', ?1, ?2)",
                params![source, at],
            )?,
        };
        Ok(())
    }

    /// Every provenance row, oldest first.
    pub fn router_provenance(&self) -> Result<Vec<RouterProvenance>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, granted_by, epsilon_max, source, at
               FROM router_provenance ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            let kind: String = row.get(0)?;
            match kind.as_str() {
                "envelope_off" => Ok(RouterProvenance::EnvelopeRemoved {
                    source: row.get(3)?,
                    at: row.get(4)?,
                }),
                "envelope" => Ok(RouterProvenance::Envelope {
                    by: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    epsilon_max: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                    source: row.get(3)?,
                    at: row.get(4)?,
                }),
                // A kind this binary never writes is a ledger it does not
                // understand, not an envelope.
                other => Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("unknown router provenance kind `{other}`").into(),
                )),
            }
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
    // A row this binary wrote always parses; one that does not is a
    // corrupt row, reported as such rather than read as "no signals".
    let json_list = |index: usize| -> rusqlite::Result<Vec<String>> {
        let text: String = row.get(offset + index)?;
        serde_json::from_str(&text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                offset + index,
                rusqlite::types::Type::Text,
                Box::new(e),
            )
        })
    };
    Ok(RouterTaskRow {
        task_id: row.get(offset)?,
        agent_id: unkey(row.get(offset + 1)?),
        started_at: row.get(offset + 2)?,
        ended_at: row.get(offset + 3)?,
        class: read_class(row, offset + 4)?,
        outcome: row.get(offset + 10)?,
        outcome_rank: row.get::<_, i64>(offset + 11)?.clamp(0, 255) as u8,
        completed_at_end: row.get(offset + 17)?,
        outcome_uncorrected: row.get(offset + 24)?,
        inferred: json_list(12)?,
        files: json_list(18)?,
        escalations: row.get::<_, i64>(offset + 13)?.max(0) as u32,
        exhausted: row.get(offset + 14)?,
        turns: row.get::<_, i64>(offset + 15)?.max(0) as u32,
        explicit_quote: row.get(offset + 16)?,
        outcome_source: row.get(offset + 19)?,
        checks: match row.get::<_, Option<String>>(offset + 20)? {
            None => Vec::new(),
            Some(text) => serde_json::from_str(&text).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    offset + 20,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        },
        parent_task_id: row.get(offset + 21)?,
        parent_link_recorded: row.get(offset + 22)?,
        unmatched_checks: match row.get::<_, Option<String>>(offset + 23)? {
            None => Vec::new(),
            Some(text) => serde_json::from_str(&text).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    offset + 23,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        },
    })
}

/// A task by its session and id: the key of a main task, whose `agent_id`
/// is empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskKey {
    pub session: String,
    pub task_id: String,
}

/// One main-session correction row and the earlier tasks it is charged to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionCharge {
    /// The task B the correction was made in.
    pub session: String,
    pub task_id: String,
    pub at: String,
    /// The earlier tasks A whose files the correction fixes.
    pub charged: Vec<TaskKey>,
    /// Task B holds a main-session `flag` reassess row.
    pub flagged: bool,
    /// The row's files are empty (written before the plugin sent them), so
    /// an empty `charged` means unknown, not no overlap.
    pub legacy: bool,
}

/// The tasks B every one of whose correction rows is charged to an earlier
/// task and that were never flagged: a flag is never charged away.
///
/// Gate 2 rule: such a B's outcome is its `outcome_uncorrected` (unknown
/// when null), and a subagent whose `outcome_source` is `parent` takes its
/// parent's the same way.
pub fn corrections_charged_away(charges: &[CorrectionCharge]) -> BTreeSet<TaskKey> {
    let mut away: BTreeMap<TaskKey, bool> = BTreeMap::new();
    for charge in charges {
        let task = TaskKey {
            session: charge.session.clone(),
            task_id: charge.task_id.clone(),
        };
        let all_so_far = away.entry(task).or_insert(true);
        *all_so_far = *all_so_far && !charge.charged.is_empty() && !charge.flagged;
    }
    away.into_iter()
        .filter_map(|(task, charged_away)| charged_away.then_some(task))
        .collect()
}

fn parse_utc(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// A task that could be charged: a main task that ended verified or
/// accepted, with its end time parsed and its files.
struct ChargeCandidate {
    key: TaskKey,
    ended_at: DateTime<Utc>,
    files: BTreeSet<String>,
}

/// Token counts are `u64` on the wire and INTEGER (i64) in SQLite.
pub(super) fn saturating_i64(value: u64) -> i64 {
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
            completed_at_end: None,
            outcome_uncorrected: None,
            inferred: vec![],
            files: vec![],
            escalations: 0,
            exhausted: false,
            turns: 1,
            explicit_quote: None,
            outcome_source: Some("own".into()),
            checks: vec![],
            parent_task_id: None,
            parent_link_recorded: false,
            unmatched_checks: vec![],
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
    fn exploration_tasks_carry_their_outcome_source() {
        let (ledger, _dir) = ledger();
        let class = Some(RouterClass {
            kind: "question".into(),
            difficulty: 2,
            scope: "local".into(),
            uncertainty: "low".into(),
            verifiable: false,
            confidence: 0.9,
        });
        let decision = |relation: &str, agent: Option<&str>| RouterDecisionRow {
            task_id: "t1".into(),
            turn_id: None,
            agent_id: agent.map(String::from),
            relation: relation.into(),
            class: class.clone(),
            tier: "research".into(),
            model: "claude-haiku-5-5".into(),
            effort: None,
            reason: "table".into(),
            mode_effective: "shadow".into(),
            holdout: false,
            applied: true,
            explored: true,
            propensity: Some(0.1),
            draw: Some(0.05),
            would_pass_gate: true,
            table_version: Some(3),
            at: "2026-10-07T10:00:00Z".into(),
        };
        let answered = |source: &str, agent: Option<&str>| RouterTaskRow {
            agent_id: agent.map(String::from),
            class: class.clone(),
            outcome_source: Some(source.into()),
            ..task("answered", 1)
        };
        ledger
            .record_router_batch(
                "main",
                &[
                    RouterRecord::Decision(decision("new_task", None)),
                    RouterRecord::Task(answered("backfill_v45", None)),
                ],
            )
            .unwrap();
        ledger
            .record_router_batch(
                "spawn",
                &[
                    RouterRecord::Decision(decision("subagent", Some("a1"))),
                    RouterRecord::Task(answered("own", Some("a1"))),
                ],
            )
            .unwrap();
        let tasks = ledger.router_exploration_tasks().unwrap();
        let sources: Vec<(&str, Option<&str>)> = tasks
            .iter()
            .map(|t| (t.relation.as_str(), t.outcome_source.as_deref()))
            .collect();
        assert_eq!(
            sources,
            [
                ("new_task", Some("backfill_v45")),
                ("subagent", Some("own"))
            ]
        );
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
    fn at_rank_zero_unknown_beats_answered_in_either_order() {
        let answered = || task("answered", 0);
        let unknown = || task("unknown", 0);
        assert_eq!(merge_task(answered(), &unknown()).outcome, "unknown");
        assert_eq!(merge_task(unknown(), &answered()).outcome, "unknown");
        assert_eq!(merge_task(answered(), &answered()).outcome, "answered");
    }

    #[test]
    fn a_merge_keeps_the_stored_parent_ors_the_marker_and_caps_unmatched_checks() {
        let stored = RouterTaskRow {
            parent_task_id: Some("first".into()),
            parent_link_recorded: false,
            unmatched_checks: vec!["r: make a".into(), "r: make b".into(), "r: make c".into()],
            ..task("unknown", 0)
        };
        let incoming = RouterTaskRow {
            parent_task_id: Some("second".into()),
            parent_link_recorded: true,
            unmatched_checks: vec![
                "r: make b".into(),
                "r: make d".into(),
                "r: make e".into(),
                "r: make f".into(),
            ],
            ..task("unknown", 0)
        };
        let merged = merge_task(stored, &incoming);
        assert_eq!(merged.parent_task_id.as_deref(), Some("first"));
        assert!(merged.parent_link_recorded);
        assert_eq!(
            merged.unmatched_checks,
            [
                "r: make a",
                "r: make b",
                "r: make c",
                "r: make d",
                "r: make e"
            ]
        );

        let adopted = merge_task(task("unknown", 0), &incoming);
        assert_eq!(adopted.parent_task_id.as_deref(), Some("second"));
        let kept = merge_task(
            RouterTaskRow {
                parent_link_recorded: true,
                ..task("unknown", 0)
            },
            &task("unknown", 0),
        );
        assert!(kept.parent_link_recorded);
    }

    #[test]
    fn the_parent_link_and_unmatched_checks_round_trip_through_the_ledger() {
        let (ledger, _dir) = ledger();
        let written = RouterTaskRow {
            parent_task_id: Some("parent-1".into()),
            parent_link_recorded: true,
            unmatched_checks: vec!["repo: cargo nextest".into(), "repo: make check".into()],
            ..task("unknown", 0)
        };
        ledger
            .record_router_batch("s", &[RouterRecord::Task(written)])
            .unwrap();
        let read = ledger.router_task("s", "t1", None).unwrap().unwrap();
        assert_eq!(read.parent_task_id.as_deref(), Some("parent-1"));
        assert!(read.parent_link_recorded);
        assert_eq!(
            read.unmatched_checks,
            ["repo: cargo nextest", "repo: make check"]
        );
    }

    #[test]
    fn answered_with_files_is_forced_unknown() {
        let with_files = RouterTaskRow {
            files: vec!["abc".into()],
            ..task("answered", 0)
        };
        assert_eq!(
            merge_task(task("answered", 0), &with_files).outcome,
            "unknown"
        );
        assert_eq!(
            merge_task(with_files.clone(), &task("answered", 0)).outcome,
            "unknown"
        );
    }

    #[test]
    fn a_forced_unknown_keeps_the_end_outcome_and_the_source_consistent() {
        let backfilled = RouterTaskRow {
            completed_at_end: Some("answered".into()),
            outcome_source: Some("backfill_v45".into()),
            ..task("answered", 0)
        };
        let incoming = RouterTaskRow {
            outcome_source: Some("own".into()),
            ..task("unknown", 0)
        };
        let merged = merge_task(backfilled, &incoming);
        assert_eq!(merged.outcome, "unknown");
        assert_eq!(merged.completed_at_end.as_deref(), Some("unknown"));
        assert_eq!(merged.outcome_source.as_deref(), Some("own"));
        let kept = merge_task(
            RouterTaskRow {
                completed_at_end: Some("answered".into()),
                ..task("answered", 0)
            },
            &task("answered", 0),
        );
        assert_eq!(kept.completed_at_end.as_deref(), Some("answered"));
    }

    #[test]
    fn a_first_answered_record_with_files_is_stored_unknown() {
        let with_files = RouterTaskRow {
            files: vec!["abc".into()],
            completed_at_end: Some("answered".into()),
            ..task("answered", 0)
        };
        let stored = first_task(&with_files);
        assert_eq!(stored.outcome, "unknown");
        assert_eq!(stored.completed_at_end.as_deref(), Some("unknown"));
        assert_eq!(stored.files, vec!["abc".to_string()]);
        let plain = task("answered", 0);
        assert_eq!(first_task(&plain), plain);
        let verified = RouterTaskRow {
            files: vec!["abc".into()],
            ..task("completed_verified", 1)
        };
        assert_eq!(first_task(&verified), verified);
    }

    #[test]
    fn a_strong_outcome_replaces_answered_and_corrected_is_never_replaced() {
        let verified = merge_task(task("answered", 0), &task("completed_verified", 1));
        assert_eq!(verified.outcome, "completed_verified");
        let with_files = RouterTaskRow {
            files: vec!["abc".into()],
            ..task("completed_verified", 1)
        };
        assert_eq!(
            merge_task(task("answered", 0), &with_files).outcome,
            "completed_verified"
        );
        let corrected = merge_task(task("corrected", 2), &task("answered", 0));
        assert_eq!(corrected.outcome, "corrected");
        let still = merge_task(corrected, &task("completed_verified", 1));
        assert_eq!(still.outcome, "corrected");
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
    fn a_later_correction_moves_the_outcome_but_not_the_end_outcome_and_files_gain() {
        let (ledger, _dir) = ledger();
        let ended = RouterTaskRow {
            completed_at_end: Some("completed_verified".into()),
            files: vec!["aaaaaaaaaaaaaaaa".into()],
            ..task("completed_verified", 1)
        };
        let corrected = RouterTaskRow {
            completed_at_end: Some("corrected".into()),
            files: vec!["bbbbbbbbbbbbbbbb".into()],
            ..task("corrected", 2)
        };
        for row in [ended, corrected] {
            ledger
                .record_router_batch("s", &[RouterRecord::Task(row)])
                .unwrap();
        }
        let stored = ledger.router_task("s", "t1", None).unwrap().unwrap();
        assert_eq!(stored.outcome, "corrected");
        assert_eq!(
            stored.completed_at_end.as_deref(),
            Some("completed_verified")
        );
        assert_eq!(stored.files, vec!["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"]);
    }

    fn parent_outcome(task_id: &str, agent: Option<&str>, outcome: &str, rank: u8) -> RouterRecord {
        RouterRecord::TaskOutcome(RouterTaskOutcomeRow {
            task_id: task_id.into(),
            agent_id: agent.map(String::from),
            outcome: outcome.into(),
            outcome_rank: rank,
            at: "2026-10-07T10:05:00Z".into(),
        })
    }

    fn subagent_task(id: &str, outcome: &str, rank: u8) -> RouterRecord {
        RouterRecord::Task(RouterTaskRow {
            task_id: id.into(),
            agent_id: Some("a1".into()),
            ..task(outcome, rank)
        })
    }

    #[test]
    fn a_subagent_that_ended_before_its_parent_gets_the_parents_outcome() {
        let (ledger, _dir) = ledger();
        ledger
            .record_router_batch("s", &[subagent_task("sub", "unknown", 0)])
            .unwrap();
        ledger
            .record_router_batch(
                "s",
                &[parent_outcome("sub", Some("a1"), "completed_verified", 1)],
            )
            .unwrap();
        let stored = ledger.router_task("s", "sub", Some("a1")).unwrap().unwrap();
        assert_eq!(stored.outcome, "completed_verified");
        assert_eq!(stored.outcome_rank, 1);
        assert_eq!(stored.outcome_source.as_deref(), Some("parent"));
    }

    #[test]
    fn a_subagent_with_its_own_outcome_keeps_it_and_its_source() {
        let (ledger, _dir) = ledger();
        ledger
            .record_router_batch("s", &[subagent_task("sub", "completed_accepted", 1)])
            .unwrap();
        ledger
            .record_router_batch("s", &[parent_outcome("sub", Some("a1"), "corrected", 2)])
            .unwrap();
        let stored = ledger.router_task("s", "sub", Some("a1")).unwrap().unwrap();
        assert_eq!(stored.outcome, "completed_accepted");
        assert_eq!(stored.outcome_source.as_deref(), Some("own"));
    }

    #[test]
    fn a_later_own_record_outranks_a_parent_outcome() {
        let (ledger, _dir) = ledger();
        ledger
            .record_router_batch(
                "s",
                &[
                    subagent_task("sub", "unknown", 0),
                    parent_outcome("sub", Some("a1"), "completed_verified", 1),
                    // A parent outcome for a task never recorded changes nothing.
                    parent_outcome("ghost", None, "corrected", 2),
                    subagent_task("sub", "corrected", 2),
                ],
            )
            .unwrap();
        let stored = ledger.router_task("s", "sub", Some("a1")).unwrap().unwrap();
        assert_eq!(stored.outcome, "corrected");
        assert_eq!(stored.outcome_source.as_deref(), Some("own"));
        assert!(ledger.router_task("s", "ghost", None).unwrap().is_none());
    }

    #[test]
    fn checks_and_the_tables_version_are_stored() {
        let (ledger, _dir) = ledger();
        let checks = vec![
            RouterCheck {
                name: "go vet ./...".into(),
                kind: "lint".into(),
                passed: true,
            },
            RouterCheck {
                name: "greengate-gate@abc1234".into(),
                kind: "gate".into(),
                passed: false,
            },
        ];
        let decision = RouterDecisionRow {
            task_id: "t1".into(),
            turn_id: None,
            agent_id: None,
            relation: "new_task".into(),
            class: None,
            tier: "research".into(),
            model: "claude-haiku-5-5".into(),
            effort: None,
            reason: "table".into(),
            mode_effective: "shadow".into(),
            holdout: false,
            applied: false,
            explored: false,
            propensity: None,
            draw: None,
            would_pass_gate: true,
            table_version: Some(3),
            at: "2026-10-07T10:00:00Z".into(),
        };
        ledger
            .record_router_batch(
                "s",
                &[
                    RouterRecord::Decision(decision),
                    RouterRecord::Task(RouterTaskRow {
                        checks: checks.clone(),
                        ..task("unknown", 0)
                    }),
                ],
            )
            .unwrap();
        let stored = ledger.router_task("s", "t1", None).unwrap().unwrap();
        assert_eq!(stored.checks, checks);
        let version: Option<i64> = ledger
            .conn
            .query_row("SELECT table_version FROM router_decisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, Some(3));
    }

    #[test]
    fn provenance_rows_read_back_in_order() {
        let (ledger, _dir) = ledger();
        let written = vec![
            RouterProvenance::Envelope {
                by: "relais install".into(),
                epsilon_max: 0.1,
                source: "install".into(),
                at: "2026-10-07T00:00:00Z".into(),
            },
            RouterProvenance::EnvelopeRemoved {
                source: "plugin-ask".into(),
                at: "2026-10-07T01:00:00Z".into(),
            },
        ];
        for row in &written {
            ledger.record_router_provenance(row).unwrap();
        }
        assert_eq!(ledger.router_provenance().unwrap(), written);
    }

    fn reassess(task_id: &str, event: &str, files: &[&str], at: &str) -> RouterRecord {
        RouterRecord::Reassess(RouterReassessRow {
            task_id: task_id.into(),
            agent_id: None,
            event: event.into(),
            tier_from: "sonnet".into(),
            tier_to: "opus".into(),
            effort_to: None,
            escalating: true,
            files: files.iter().map(|f| (*f).to_string()).collect(),
            at: at.into(),
        })
    }

    fn stored_reassess_files(ledger: &Ledger, event: &str) -> String {
        ledger
            .conn
            .query_row(
                "SELECT files_json FROM router_reassess WHERE event = ?1",
                [event],
                |row| row.get(0),
            )
            .unwrap()
    }

    const CORRECTION_AT: &str = "2026-10-07T12:00:00.000Z";

    fn ended_task(
        task_id: &str,
        ended_at: &str,
        end: &str,
        files: &[&str],
        agent: Option<&str>,
    ) -> RouterRecord {
        RouterRecord::Task(RouterTaskRow {
            task_id: task_id.into(),
            agent_id: agent.map(str::to_string),
            ended_at: Some(ended_at.into()),
            completed_at_end: Some(end.into()),
            files: files.iter().map(|f| (*f).to_string()).collect(),
            ..task("completed_verified", 4)
        })
    }

    fn correction_charges(
        records: &[RouterRecord],
        stop: &[String],
    ) -> (Vec<CorrectionCharge>, BTreeSet<TaskKey>) {
        let (ledger, _dir) = ledger();
        ledger.record_router_batch("s1", records).unwrap();
        let charges = ledger.router_correction_charges(stop).unwrap();
        let away = corrections_charged_away(&charges);
        (charges, away)
    }

    fn key_of(session: &str, task_id: &str) -> TaskKey {
        TaskKey {
            session: session.into(),
            task_id: task_id.into(),
        }
    }

    /// Task A ended `ended_at` with files `files`, B ("b") corrected at
    /// `CORRECTION_AT` with `row_files`; B itself is an unended task.
    fn charge_case(ended_at: &str, end: &str, files: &[&str], row_files: &[&str]) -> bool {
        let (ledger, _dir) = ledger();
        ledger
            .record_router_batch("other", &[ended_task("a", ended_at, end, files, None)])
            .unwrap();
        ledger
            .record_router_batch(
                "s1",
                &[reassess("b", "correction", row_files, CORRECTION_AT)],
            )
            .unwrap();
        let charges = ledger.router_correction_charges(&[]).unwrap();
        corrections_charged_away(&charges).contains(&key_of("s1", "b"))
    }

    #[test]
    fn a_correction_row_resent_unions_its_files() {
        let (ledger, _dir) = ledger();
        for files in [&["x"][..], &["z"], &["z"]] {
            ledger
                .record_router_batch("s1", &[reassess("b", "correction", files, "t")])
                .unwrap();
        }
        assert_eq!(
            stored_reassess_files(&ledger, "correction"),
            "[\"x\",\"z\"]"
        );
    }

    #[test]
    fn the_end_row_before_the_early_row_keeps_the_files() {
        let (ledger, _dir) = ledger();
        for files in [&["x", "z"][..], &[]] {
            ledger
                .record_router_batch("s1", &[reassess("b", "correction", files, "t")])
                .unwrap();
        }
        assert_eq!(
            stored_reassess_files(&ledger, "correction"),
            "[\"x\",\"z\"]"
        );
    }

    #[test]
    fn a_revert_row_sent_twice_keeps_its_first_files() {
        let (ledger, _dir) = ledger();
        for files in [&["x"][..], &["z"]] {
            ledger
                .record_router_batch("s1", &[reassess("b", "revert", files, "t")])
                .unwrap();
        }
        assert_eq!(stored_reassess_files(&ledger, "revert"), "[\"x\"]");
    }

    #[test]
    fn a_correction_is_charged_to_an_earlier_verified_task() {
        let (ledger, _dir) = ledger();
        ledger
            .record_router_batch(
                "other",
                &[ended_task(
                    "a",
                    "2026-10-07T10:00:00.000Z",
                    "completed_verified",
                    &["x"],
                    None,
                )],
            )
            .unwrap();
        ledger
            .record_router_batch("s1", &[reassess("b", "correction", &["x"], CORRECTION_AT)])
            .unwrap();
        let charges = ledger.router_correction_charges(&[]).unwrap();
        assert_eq!(
            charges,
            vec![CorrectionCharge {
                session: "s1".into(),
                task_id: "b".into(),
                at: CORRECTION_AT.into(),
                charged: vec![key_of("other", "a")],
                flagged: false,
                legacy: false,
            }]
        );
        assert!(corrections_charged_away(&charges).contains(&key_of("s1", "b")));
    }

    #[test]
    fn a_correction_stays_with_its_task_in_the_kept_cases() {
        let ok = "2026-10-07T10:00:00.000Z";
        let verified = "completed_verified";
        assert!(charge_case(ok, verified, &["x"], &["x"]));
        assert!(charge_case(ok, "completed_accepted", &["x"], &["x"]));
        assert!(!charge_case(ok, verified, &["y"], &["x"]));
        assert!(!charge_case(ok, "corrected", &["x"], &["x"]));
        assert!(!charge_case(ok, verified, &["x"], &[]));
        assert!(!charge_case(
            "2026-10-06T11:59:00.000Z",
            verified,
            &["x"],
            &["x"]
        ));
        assert!(charge_case(
            "2026-10-06T12:01:00.000Z",
            verified,
            &["x"],
            &["x"]
        ));
        assert!(!charge_case(CORRECTION_AT, verified, &["x"], &["x"]));
    }

    #[test]
    fn a_task_is_not_charged_to_itself_or_a_subagent() {
        let a = "2026-10-07T10:00:00.000Z";
        let verified = "completed_verified";
        let (_, away) = correction_charges(
            &[
                ended_task("b", a, verified, &["x"], None),
                reassess("b", "correction", &["x"], CORRECTION_AT),
            ],
            &[],
        );
        assert!(away.is_empty());
        let (_, away) = correction_charges(
            &[
                ended_task("a", a, verified, &["x"], Some("agent")),
                reassess("b", "correction", &["x"], CORRECTION_AT),
            ],
            &[],
        );
        assert!(away.is_empty());
    }

    #[test]
    fn two_corrections_one_uncharged_keep_the_task_corrected() {
        let (charges, away) = correction_charges(
            &[
                ended_task(
                    "a",
                    "2026-10-07T10:00:00.000Z",
                    "completed_verified",
                    &["x"],
                    None,
                ),
                reassess("b", "correction", &["x"], CORRECTION_AT),
                reassess("b", "correction", &["q"], "2026-10-07T12:30:00.000Z"),
            ],
            &[],
        );
        assert_eq!(charges.len(), 2);
        assert!(away.is_empty());
    }

    #[test]
    fn a_legacy_row_is_marked_and_charges_no_one() {
        let (charges, away) = correction_charges(
            &[
                ended_task(
                    "a",
                    "2026-10-07T10:00:00.000Z",
                    "completed_verified",
                    &["x"],
                    None,
                ),
                reassess("b", "correction", &[], CORRECTION_AT),
            ],
            &[],
        );
        assert!(charges[0].legacy && charges[0].charged.is_empty());
        assert!(away.is_empty());
    }

    #[test]
    fn an_overlap_only_through_a_stop_hash_is_not_charged() {
        let records = [
            ended_task(
                "a",
                "2026-10-07T10:00:00.000Z",
                "completed_verified",
                &["x"],
                None,
            ),
            reassess("b", "correction", &["x"], CORRECTION_AT),
        ];
        let (_, away) = correction_charges(&records, &["x".to_string()]);
        assert!(away.is_empty());
        let stop: Vec<String> = (0..1500).map(|n| format!("h{n}")).collect();
        let (_, away) = correction_charges(&records, &stop);
        assert!(away.contains(&key_of("s1", "b")));
    }

    #[test]
    fn a_flagged_task_is_never_charged_away() {
        let (charges, away) = correction_charges(
            &[
                ended_task(
                    "a",
                    "2026-10-07T10:00:00.000Z",
                    "completed_verified",
                    &["x"],
                    None,
                ),
                reassess("b", "correction", &["x"], CORRECTION_AT),
                reassess("b", "flag", &[], "2026-10-07T12:10:00.000Z"),
            ],
            &[],
        );
        assert!(charges[0].flagged);
        assert!(away.is_empty());
    }

    #[test]
    fn a_row_with_an_unparseable_time_is_skipped() {
        let (charges, _) = correction_charges(&[reassess("b", "correction", &["x"], "soon")], &[]);
        assert!(charges.is_empty());
    }

    fn uncorrected(ended_at: Option<&str>, value: Option<&str>) -> RouterRecord {
        RouterRecord::Task(RouterTaskRow {
            ended_at: ended_at.map(str::to_string),
            outcome_uncorrected: value.map(str::to_string),
            ..task("corrected", 5)
        })
    }

    fn stored_uncorrected(ledger: &Ledger) -> Option<String> {
        ledger
            .router_task("s1", "t1", None)
            .unwrap()
            .unwrap()
            .outcome_uncorrected
    }

    #[test]
    fn outcome_uncorrected_comes_from_the_first_ended_record() {
        let end = "2026-10-07T11:00:00Z";
        let (ledger, _dir) = ledger();
        ledger
            .record_router_batch("s1", &[uncorrected(None, Some("completed_verified"))])
            .unwrap();
        assert_eq!(stored_uncorrected(&ledger), None);
        ledger
            .record_router_batch("s1", &[uncorrected(Some(end), Some("completed_verified"))])
            .unwrap();
        assert_eq!(
            stored_uncorrected(&ledger).as_deref(),
            Some("completed_verified")
        );
        ledger
            .record_router_batch("s1", &[uncorrected(Some(end), Some("completed_accepted"))])
            .unwrap();
        assert_eq!(
            stored_uncorrected(&ledger).as_deref(),
            Some("completed_verified")
        );
        ledger
            .record_router_batch("s1", &[uncorrected(None, Some("completed_accepted"))])
            .unwrap();
        assert_eq!(
            stored_uncorrected(&ledger).as_deref(),
            Some("completed_verified")
        );
    }

    #[test]
    fn a_record_without_outcome_uncorrected_stores_null() {
        let (ledger, _dir) = ledger();
        ledger
            .record_router_batch("s1", &[uncorrected(Some("2026-10-07T11:00:00Z"), None)])
            .unwrap();
        assert_eq!(stored_uncorrected(&ledger), None);
    }
}
