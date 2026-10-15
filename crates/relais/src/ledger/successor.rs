//! Successor intents (docs/plans/2026-10-14-runs-survive-a-loaded-machine.md,
//! A5): the transactional outbox a `revise` answer writes.
//!
//! The decision answer and the intent row are one commit, keyed by the
//! decision, so a repeated answer finds the same intent. Only the plugin
//! starts runs: it claims an intent (`pending -> claimed(run)`, atomic) and
//! launches it. A second claim returns the run that holds the claim; a
//! claimed intent is handed to a new run only when the claiming run left no
//! row, or left a live-looking row whose driver process is gone, by
//! compare-and-set on the old run id. A claim whose run never wrote a driver
//! pid, or whose pid is now another process (its start time differs from the
//! one recorded), is abandoned once the grace passes; a launch a preflight
//! refuses hands the claim back (`release_claim`) with the refusal on the
//! intent.

use std::collections::BTreeSet;

use rusqlite::{params, OptionalExtension};

use crate::contract::ResolvedCandidate;
use crate::ids::{sha256_hex, RunId};
use crate::lifecycle::State;

use super::{parse_state, DecisionAnswer, Ledger, LedgerError, Result};

/// How long a claim may go without the run it launched appearing in the
/// ledger before the claim is called abandoned: a run inserts its row in
/// preflight, seconds after the claim.
pub const CLAIM_GRACE_SECONDS: i64 = 120;

/// What the host knows of a driver pid: gone, or alive with the start time
/// of the process that holds it now (`None` when it cannot be read).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuccessorProcess {
    Gone,
    Alive { started: Option<String> },
}

/// What a `revise` answer asks the plugin to launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewIntent {
    /// The run whose decision this answers.
    pub predecessor: RunId,
    pub contract_digest: String,
    pub contract_json: String,
    /// The paths a widened scope adds, exactly; empty for a contract the
    /// answer's author wrote.
    pub scope_delta: Vec<String>,
    /// The candidate the successor applies, resolved and pinned with the
    /// answer.
    pub candidate: Option<ResolvedCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Pending,
    Claimed,
}

impl IntentState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
        }
    }

    fn parse(stored: &str) -> Result<Self> {
        match stored {
            "pending" => Ok(Self::Pending),
            "claimed" => Ok(Self::Claimed),
            other => Err(LedgerError::Corrupt {
                what: "a successor intent state".into(),
                detail: format!("`{other}` is neither pending nor claimed"),
            }),
        }
    }
}

/// One successor intent as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessorIntent {
    pub intent_id: String,
    pub decision_id: String,
    pub predecessor: RunId,
    pub contract_digest: String,
    pub contract_json: String,
    pub scope_delta: Vec<String>,
    pub candidate: Option<ResolvedCandidate>,
    pub state: IntentState,
    pub run_id: Option<RunId>,
    /// Why the last launch was refused before it had a run, if one was.
    pub refusal: Option<String>,
    /// When that refusal was recorded.
    pub refused_at: Option<String>,
    /// How many launches have been refused; the backoff grows with it.
    pub refusal_count: u32,
}

/// The seconds a pending intent waits after its first refusal.
pub const REFUSAL_BACKOFF_SECONDS: i64 = 60;
/// The longest a refused intent waits before it is offered again.
pub const REFUSAL_BACKOFF_CEILING_SECONDS: i64 = 600;

/// The wait after the `count`th refusal: 60 s, doubling, capped at 10 min.
pub fn refusal_backoff_seconds(count: u32) -> i64 {
    let doublings = count.saturating_sub(1).min(16);
    (REFUSAL_BACKOFF_SECONDS << doublings).min(REFUSAL_BACKOFF_CEILING_SECONDS)
}

impl SuccessorIntent {
    /// When a refused intent may be launched again: its refusal time plus
    /// the backoff its refusal count earns. `None` for an intent never
    /// refused.
    pub fn retry_after(&self) -> Option<String> {
        let at = chrono::DateTime::parse_from_rfc3339(self.refused_at.as_deref()?).ok()?;
        let wait = chrono::Duration::seconds(refusal_backoff_seconds(self.refusal_count));
        Some((at + wait).to_rfc3339())
    }

    /// Is the intent still inside its backoff at `now`?
    pub fn backing_off(&self, now: &str) -> bool {
        self.state == IntentState::Pending
            && self
                .retry_after()
                .and_then(|retry| seconds_since(now, &retry))
                .is_some_and(|left| left > 0)
    }
}

/// What a claim ended in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// This run holds the claim now.
    Claimed(Box<SuccessorIntent>),
    /// Another run holds it: `intent_already_claimed`, with that run.
    AlreadyClaimed { run: RunId },
    /// No such intent.
    Unknown,
}

/// The intent id a decision's answer gets: a function of the decision, so
/// the same decision cannot name two.
pub fn intent_id_of(decision_id: &str) -> String {
    format!("intent-{}", &sha256_hex(decision_id.as_bytes())[..16])
}

const COLUMNS: &str = "intent_id, decision_id, predecessor, contract_digest, contract_json, \
                       scope_delta_json, candidate_json, state, run_id, \
                       refusal, refused_at, refusal_count";

type Row = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

fn row_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
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
        row.get(11)?,
    ))
}

fn corrupt(what: &str, e: impl std::fmt::Display) -> LedgerError {
    LedgerError::Corrupt {
        what: what.into(),
        detail: e.to_string(),
    }
}

fn intent_of_row(row: Row) -> Result<SuccessorIntent> {
    let (
        intent_id,
        decision_id,
        predecessor,
        contract_digest,
        contract_json,
        scope_delta_json,
        candidate_json,
        state,
        run_id,
        refusal,
        refused_at,
        refusal_count,
    ) = row;
    Ok(SuccessorIntent {
        intent_id,
        decision_id,
        predecessor: RunId::from_stored(predecessor),
        contract_digest,
        contract_json,
        scope_delta: serde_json::from_str(&scope_delta_json)
            .map_err(|e| corrupt("a successor intent's scope delta", e))?,
        candidate: candidate_json
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|e| corrupt("a successor intent's candidate", e))?,
        state: IntentState::parse(&state)?,
        run_id: run_id.map(RunId::from_stored),
        refusal,
        refused_at,
        refusal_count: u32::try_from(refusal_count).unwrap_or(u32::MAX),
    })
}

fn seconds_since(then: &str, now: &str) -> Option<i64> {
    let parse = |stamp: &str| chrono::DateTime::parse_from_rfc3339(stamp).ok();
    Some((parse(now)? - parse(then)?).num_seconds())
}

impl Ledger {
    /// Answer an open decision AND write its successor intent, one
    /// transaction. `Ok(None)` when the run has no open decision: neither
    /// the answer nor the intent was written.
    pub fn resolve_decision_with_intent(
        &self,
        run_id: &RunId,
        answer: &DecisionAnswer<'_>,
        intent: &NewIntent,
    ) -> Result<Option<SuccessorIntent>> {
        let tx = self.write_tx()?;
        if !self.resolve_decision_in(&tx, run_id, answer)? {
            return Ok(None);
        }
        let decision_id = run_id.as_str();
        let intent_id = intent_id_of(decision_id);
        let candidate_json = intent
            .candidate
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| corrupt("a successor intent's candidate", e))?;
        tx.execute(
            "INSERT INTO successor_intents
                (intent_id, decision_id, predecessor, contract_digest, contract_json,
                 scope_delta_json, candidate_json, pin_run, pin_index, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10)",
            params![
                intent_id,
                decision_id,
                intent.predecessor.as_str(),
                intent.contract_digest,
                intent.contract_json,
                serde_json::to_string(&intent.scope_delta)
                    .map_err(|e| corrupt("a successor intent's scope delta", e))?,
                candidate_json,
                intent.candidate.as_ref().map(|c| c.source_run.clone()),
                intent.candidate.as_ref().map(|c| i64::from(c.index)),
                self.now(),
            ],
        )?;
        let stored = read_intent(&tx, &intent_id)?;
        tx.commit()?;
        #[cfg(test)]
        super::failpoint::hit(super::failpoint::AFTER_DECIDE_COMMIT);
        Ok(stored)
    }

    /// The intent a decision's answer wrote, if it wrote one.
    pub fn intent_of_decision(&self, decision_id: &RunId) -> Result<Option<SuccessorIntent>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM successor_intents WHERE decision_id = ?1"),
                [decision_id.as_str()],
                row_of,
            )
            .optional()?
            .map(intent_of_row)
            .transpose()
    }

    /// How many successor intents the outbox holds.
    pub fn count_successor_intents(&self) -> Result<u64> {
        let count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM successor_intents", [], |row| {
                    row.get(0)
                })?;
        Ok(count as u64)
    }

    pub fn successor_intent(&self, intent_id: &str) -> Result<Option<SuccessorIntent>> {
        read_intent(&self.conn, intent_id)
    }

    /// The intent a run was launched from, for its receipt.
    pub fn intent_of_run(&self, run: &RunId) -> Result<Option<SuccessorIntent>> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM successor_intents WHERE run_id = ?1"),
                [run.as_str()],
                row_of,
            )
            .optional()?
            .map(intent_of_row)
            .transpose()
    }

    /// Claim an intent for `new_run`. Pending: the claim is taken. Claimed:
    /// the existing run is returned, unless that run left no row (after the
    /// grace a launch needs) or is not terminal and its driver is gone, in
    /// which case the claim moves to `new_run` by compare-and-set on the
    /// old run id.
    pub fn claim_intent(
        &self,
        intent_id: &str,
        new_run: &RunId,
        alive: &dyn Fn(u32) -> SuccessorProcess,
    ) -> Result<Claim> {
        let tx = self.write_tx()?;
        let Some(current) = read_intent(&tx, intent_id)? else {
            return Ok(Claim::Unknown);
        };
        let now = self.now();
        let took = match (&current.state, &current.run_id) {
            (IntentState::Pending, _) => tx.execute(
                "UPDATE successor_intents SET state = 'claimed', run_id = ?2, claimed_at = ?3
                  WHERE intent_id = ?1 AND state = 'pending'",
                params![intent_id, new_run.as_str(), now],
            )?,
            (IntentState::Claimed, Some(old)) if self.abandoned(&tx, intent_id, old, alive)? => tx
                .execute(
                    "UPDATE successor_intents SET run_id = ?2, claimed_at = ?3
                      WHERE intent_id = ?1 AND state = 'claimed' AND run_id = ?4",
                    params![intent_id, new_run.as_str(), now, old.as_str()],
                )?,
            (IntentState::Claimed, _) => 0,
        };
        let after = read_intent(&tx, intent_id)?;
        tx.commit()?;
        #[cfg(test)]
        if took == 1 {
            super::failpoint::hit(super::failpoint::AFTER_CLAIM);
        }
        match after {
            Some(intent) if took == 1 => Ok(Claim::Claimed(Box::new(intent))),
            Some(SuccessorIntent {
                run_id: Some(run), ..
            }) => Ok(Claim::AlreadyClaimed { run }),
            _ => Ok(Claim::Unknown),
        }
    }

    /// Has the run that holds a claim left nothing a live launch would
    /// have: no row after the grace, a row whose driver never wrote a pid
    /// after the grace, a row whose pid is now another process (a different
    /// start time) after the grace, or a row not yet terminal whose driver
    /// process is gone?
    fn abandoned(
        &self,
        conn: &rusqlite::Connection,
        intent_id: &str,
        run: &RunId,
        process: &dyn Fn(u32) -> SuccessorProcess,
    ) -> Result<bool> {
        type Driver = (Option<i64>, Option<String>, Option<String>);
        let row: Option<Driver> = conn
            .query_row(
                &format!(
                    "SELECT driver_pid, driver_started,
                            (SELECT {} FROM runs r2 WHERE r2.id = runs.id)
                       FROM runs WHERE id = ?1",
                    super::PROJECTED_STATUS
                ),
                [run.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let past_grace = || -> Result<bool> {
            let claimed_at: Option<String> = conn
                .query_row(
                    "SELECT claimed_at FROM successor_intents WHERE intent_id = ?1",
                    [intent_id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            Ok(claimed_at
                .and_then(|at| seconds_since(&at, &self.now()))
                .is_some_and(|waited| waited >= CLAIM_GRACE_SECONDS))
        };
        let Some((pid, started, status)) = row else {
            return past_grace();
        };
        let terminal = status
            .as_deref()
            .is_some_and(|status| parse_state(status).is_ok_and(State::is_terminal));
        if terminal {
            return Ok(false);
        }
        let Some(pid) = pid.and_then(|pid| u32::try_from(pid).ok()) else {
            return past_grace();
        };
        match process(pid) {
            SuccessorProcess::Gone => Ok(true),
            SuccessorProcess::Alive { started: now } => match (now, started) {
                (Some(now), Some(recorded)) if now != recorded => past_grace(),
                _ => Ok(false),
            },
        }
    }

    /// Hand a claim back after the launch it was for was refused before the
    /// run existed: compare-and-set on the claiming run, back to pending,
    /// with the refusal on the intent. `false` when the claim has moved.
    pub fn release_claim(&self, intent_id: &str, run: &RunId, refusal: &str) -> Result<bool> {
        let released = self.conn.execute(
            "UPDATE successor_intents
                SET state = 'pending', run_id = NULL, claimed_at = NULL,
                    refusal = ?3, refused_at = ?4, refusal_count = refusal_count + 1
              WHERE intent_id = ?1 AND state = 'claimed' AND run_id = ?2",
            params![intent_id, run.as_str(), refusal, self.now()],
        )?;
        Ok(released == 1)
    }

    /// Whether a run row exists (a launched run inserts its row in
    /// preflight).
    pub fn run_exists(&self, run: &RunId) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM runs WHERE id = ?1", [run.as_str()], |_| {
                Ok(())
            })
            .optional()?
            .is_some())
    }

    /// The reason the last launch of an intent was refused, if one was.
    pub fn intent_refusal(&self, intent_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT refusal FROM successor_intents WHERE intent_id = ?1",
                [intent_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Intents the plugin should launch now: those `listed_intents` names
    /// that are not inside the backoff a refusal earned.
    pub fn launchable_intents(
        &self,
        alive: &dyn Fn(u32) -> SuccessorProcess,
    ) -> Result<Vec<SuccessorIntent>> {
        let now = self.now();
        Ok(self
            .listed_intents(alive)?
            .into_iter()
            .filter(|intent| !intent.backing_off(&now))
            .collect())
    }

    /// Intents a status shows: every pending one (a refused one carries its
    /// refusal and retry time), and every claimed one whose claim was
    /// abandoned (what a claim would take).
    pub fn listed_intents(
        &self,
        alive: &dyn Fn(u32) -> SuccessorProcess,
    ) -> Result<Vec<SuccessorIntent>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM successor_intents ORDER BY created_at, intent_id"
        ))?;
        let rows = statement
            .query_map([], row_of)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut launchable = Vec::new();
        for row in rows {
            let intent = intent_of_row(row)?;
            let abandoned = match (&intent.state, &intent.run_id) {
                (IntentState::Pending, _) => true,
                (IntentState::Claimed, Some(run)) => {
                    self.abandoned(&self.conn, &intent.intent_id, run, alive)?
                }
                (IntentState::Claimed, None) => false,
            };
            if abandoned {
                launchable.push(intent);
            }
        }
        Ok(launchable)
    }

    /// `(source run, attempt index)` of every candidate a successor intent
    /// still needs: pending intents, and claimed ones whose run has not
    /// ended.
    pub(super) fn pinned_by_intents(&self) -> Result<BTreeSet<(String, u32)>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT pin_run, pin_index, state,
                    (SELECT {} FROM runs WHERE runs.id = successor_intents.run_id)
               FROM successor_intents WHERE pin_run IS NOT NULL AND pin_index IS NOT NULL",
            super::PROJECTED_STATUS
        ))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|(_, _, state, status)| {
                state == "pending"
                    || !status
                        .as_deref()
                        .is_some_and(|status| parse_state(status).is_ok_and(State::is_terminal))
            })
            .map(|(run, index, _, _)| (run, index as u32))
            .collect())
    }
}

fn read_intent(conn: &rusqlite::Connection, intent_id: &str) -> Result<Option<SuccessorIntent>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM successor_intents WHERE intent_id = ?1"),
        [intent_id],
        row_of,
    )
    .optional()?
    .map(intent_of_row)
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::failpoint;
    use crate::ledger::{Actor, Clock, RecordedVia};
    use crate::lifecycle::Reason;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    struct Tick(Arc<AtomicI64>);

    impl Clock for Tick {
        fn now_rfc3339(&self) -> String {
            chrono::DateTime::from_timestamp(1_800_000_000 + self.0.load(Ordering::SeqCst), 0)
                .expect("a time")
                .to_rfc3339()
        }
    }

    fn up(_: u32) -> SuccessorProcess {
        SuccessorProcess::Alive { started: None }
    }

    fn down(_: u32) -> SuccessorProcess {
        SuccessorProcess::Gone
    }

    fn only_4242(pid: u32) -> SuccessorProcess {
        if pid == 4242 {
            up(pid)
        } else {
            down(pid)
        }
    }

    fn run(id: &str) -> RunId {
        RunId::from_stored(id)
    }

    fn intent_for(predecessor: &str) -> NewIntent {
        NewIntent {
            predecessor: run(predecessor),
            contract_digest: "digest".into(),
            contract_json: "{}".into(),
            scope_delta: vec!["a.rs".into()],
            candidate: Some(ResolvedCandidate {
                source_run: predecessor.into(),
                index: 2,
                candidate_commit: "c".repeat(40),
                patch_sha256: "p".into(),
                source_base_sha: "b".repeat(40),
                target_base_sha: "b".repeat(40),
            }),
        }
    }

    fn answer<'a>() -> DecisionAnswer<'a> {
        DecisionAnswer {
            resolution: Reason::DecisionRevised,
            actor: Actor::Orchestrator,
            person_quote: None,
            recorded_via: RecordedVia::Cli,
            note: None,
            successor_run: None,
            from_state: State::NeedsDecision,
            to_state: State::Failed,
        }
    }

    /// A ledger on a clock the test moves, and a run waiting on a decision.
    fn waiting(label: &str) -> (Ledger, Arc<AtomicI64>, crate::test_support::TempDir) {
        let dir = crate::test_support::temp_dir(&format!("ledger-successor-{label}"));
        let seconds = Arc::new(AtomicI64::new(0));
        let ledger = Ledger::open_with_clock(
            &dir.join("ledger.sqlite"),
            Box::new(Tick(Arc::clone(&seconds))),
        )
        .expect("ledger");
        ledger
            .insert_run(
                &run("run-a"),
                "/r",
                None,
                &crate::ids::TaskId::from_stored("task-a"),
                "rk",
            )
            .expect("run");
        ledger
            .record_transition(&crate::ledger::Transition {
                run_id: run("run-a"),
                attempt_id: None,
                from_state: None,
                to_state: State::NeedsDecision,
                reason: "scope_exceeded".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("waiting");
        (ledger, seconds, dir)
    }

    #[test]
    fn the_answer_and_one_intent_are_written_together_and_repeat_to_the_same_intent() {
        let (ledger, _, _dir) = waiting("together");
        let first = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("an open decision");
        assert_eq!(first.state, IntentState::Pending);
        assert_eq!(first.scope_delta, vec!["a.rs".to_string()]);
        let decision = ledger
            .decision_of_run(&run("run-a"))
            .expect("read")
            .expect("row");
        assert_eq!(decision.resolution, Some(Reason::DecisionRevised));
        assert_eq!(
            ledger.intent_of_decision(&run("run-a")).expect("read"),
            Some(first.clone())
        );
        // The decision is answered now: a second write finds no open
        // decision and writes no second intent.
        assert_eq!(
            ledger
                .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
                .expect("second"),
            None
        );
        let count: i64 = ledger
            .conn
            .query_row("SELECT COUNT(*) FROM successor_intents", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(count, 1);
    }

    #[test]
    fn a_failed_intent_write_leaves_the_decision_open() {
        let (ledger, _, _dir) = waiting("atomic");
        ledger
            .conn
            .execute("DROP TABLE successor_intents", [])
            .expect("break the outbox");
        assert!(ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .is_err());
        let decision = ledger
            .decision_of_run(&run("run-a"))
            .expect("read")
            .expect("row");
        assert_eq!(decision.resolution, None, "the answer rolled back with it");
    }

    #[test]
    fn two_concurrent_claims_yield_one_run() {
        let (ledger, _, dir) = waiting("concurrent");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        let path = ledger.path().to_path_buf();
        let outcomes: Vec<Claim> = (0..2)
            .map(|n| {
                let path = path.clone();
                let id = intent.intent_id.clone();
                std::thread::spawn(move || {
                    let ledger = Ledger::open(&path).expect("own connection");
                    ledger
                        .claim_intent(&id, &run(&format!("run-new-{n}")), &up)
                        .expect("claim")
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();
        let won: Vec<&Claim> = outcomes
            .iter()
            .filter(|c| matches!(c, Claim::Claimed(_)))
            .collect();
        assert_eq!(won.len(), 1, "{outcomes:?}");
        let Claim::Claimed(winner) = won[0] else {
            unreachable!()
        };
        let existing = winner.run_id.clone().expect("a run id");
        assert!(
            outcomes.iter().any(|c| matches!(
                c,
                Claim::AlreadyClaimed { run } if *run == existing
            )),
            "the other claim returns the existing run: {outcomes:?}"
        );
        drop(dir);
    }

    #[test]
    fn a_claim_whose_run_row_is_absent_is_taken_over_once_after_the_grace() {
        let (ledger, seconds, _dir) = waiting("absent");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        let id = intent.intent_id;
        assert!(matches!(
            ledger.claim_intent(&id, &run("run-1"), &up).expect("claim"),
            Claim::Claimed(_)
        ));
        // Inside the grace the first launch may still be starting.
        assert_eq!(
            ledger.claim_intent(&id, &run("run-2"), &up).expect("claim"),
            Claim::AlreadyClaimed { run: run("run-1") }
        );
        seconds.store(CLAIM_GRACE_SECONDS + 1, Ordering::SeqCst);
        assert_eq!(ledger.launchable_intents(&up).expect("list").len(), 1);
        assert!(matches!(
            ledger.claim_intent(&id, &run("run-2"), &up).expect("claim"),
            Claim::Claimed(intent) if intent.run_id == Some(run("run-2"))
        ));
        // Compare-and-set: the claim moved once; the next caller finds
        // run-2 and, run-2 having no row yet, waits out the grace again.
        assert_eq!(
            ledger.claim_intent(&id, &run("run-3"), &up).expect("claim"),
            Claim::AlreadyClaimed { run: run("run-2") }
        );
    }

    #[test]
    fn a_claim_whose_driver_is_alive_is_not_taken_over_and_a_dead_one_is() {
        let (ledger, seconds, _dir) = waiting("pid");
        // The claiming run exists, running, driven by pid 4242.
        ledger
            .insert_run(
                &run("run-1"),
                "/r",
                None,
                &crate::ids::TaskId::from_stored("task-a"),
                "rk",
            )
            .expect("run");
        ledger
            .record_transition_as_driver(&crate::ledger::Transition {
                run_id: run("run-1"),
                attempt_id: None,
                from_state: None,
                to_state: State::Running,
                reason: "worker_dispatched".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("running");
        ledger
            .conn
            .execute("UPDATE runs SET driver_pid = 4242 WHERE id = 'run-1'", [])
            .expect("bind a pid");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        let id = intent.intent_id;
        assert!(matches!(
            ledger.claim_intent(&id, &run("run-1"), &up).expect("claim"),
            Claim::Claimed(_)
        ));
        seconds.store(CLAIM_GRACE_SECONDS * 10, Ordering::SeqCst);
        assert_eq!(
            ledger
                .claim_intent(&id, &run("run-2"), &only_4242)
                .expect("claim"),
            Claim::AlreadyClaimed { run: run("run-1") },
            "a live driver keeps its claim, however old"
        );
        assert!(ledger
            .launchable_intents(&only_4242)
            .expect("list")
            .is_empty());
        assert!(matches!(
            ledger
                .claim_intent(&id, &run("run-2"), &down)
                .expect("claim"),
            Claim::Claimed(_)
        ));
    }

    #[test]
    fn a_pending_intent_pins_its_candidate_and_a_finished_run_releases_it() {
        let (ledger, _, _dir) = waiting("pin");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        assert!(ledger
            .pinned_candidates()
            .expect("pins")
            .contains(&("run-a".to_string(), 2)));
        ledger
            .claim_intent(&intent.intent_id, &run("run-a"), &up)
            .expect("claim");
        // The claiming run (here run-a itself, already terminal) has ended.
        assert!(!ledger
            .pinned_candidates()
            .expect("pins")
            .contains(&("run-a".to_string(), 2)));
    }

    /// A run `run-1` that holds the claim, running, with the given driver.
    fn claimed_by_running(
        label: &str,
        pid: Option<i64>,
        started: Option<&str>,
    ) -> (Ledger, Arc<AtomicI64>, crate::test_support::TempDir, String) {
        let (ledger, seconds, dir) = waiting(label);
        ledger
            .insert_run(
                &run("run-1"),
                "/r",
                None,
                &crate::ids::TaskId::from_stored("task-a"),
                "rk",
            )
            .expect("run");
        ledger
            .record_transition_as_driver(&crate::ledger::Transition {
                run_id: run("run-1"),
                attempt_id: None,
                from_state: None,
                to_state: State::Running,
                reason: "worker_dispatched".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("running");
        ledger
            .conn
            .execute(
                "UPDATE runs SET driver_pid = ?1, driver_started = ?2 WHERE id = 'run-1'",
                params![pid, started],
            )
            .expect("bind a driver");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        assert!(matches!(
            ledger
                .claim_intent(&intent.intent_id, &run("run-1"), &up)
                .expect("claim"),
            Claim::Claimed(_)
        ));
        (ledger, seconds, dir, intent.intent_id)
    }

    #[test]
    fn a_claim_whose_run_has_no_driver_pid_is_abandoned_after_the_grace() {
        let (ledger, seconds, _dir, id) = claimed_by_running("nullpid", None, None);
        assert!(ledger.launchable_intents(&up).expect("list").is_empty());
        seconds.store(CLAIM_GRACE_SECONDS + 1, Ordering::SeqCst);
        assert_eq!(ledger.launchable_intents(&up).expect("list").len(), 1);
        assert!(matches!(
            ledger.claim_intent(&id, &run("run-2"), &up).expect("claim"),
            Claim::Claimed(intent) if intent.run_id == Some(run("run-2"))
        ));
    }

    #[test]
    fn a_claim_whose_pid_is_another_process_is_abandoned_after_the_grace() {
        let (ledger, seconds, _dir, id) = claimed_by_running("reused", Some(4242), Some("t0"));
        let reused = |_: u32| SuccessorProcess::Alive {
            started: Some("t1".into()),
        };
        let same = |_: u32| SuccessorProcess::Alive {
            started: Some("t0".into()),
        };
        assert!(ledger.launchable_intents(&reused).expect("list").is_empty());
        seconds.store(CLAIM_GRACE_SECONDS + 1, Ordering::SeqCst);
        assert!(
            ledger.launchable_intents(&same).expect("list").is_empty(),
            "the very process that was recorded keeps its claim"
        );
        assert_eq!(ledger.launchable_intents(&reused).expect("list").len(), 1);
        assert!(matches!(
            ledger
                .claim_intent(&id, &run("run-2"), &reused)
                .expect("claim"),
            Claim::Claimed(_)
        ));
    }

    #[test]
    fn a_refused_launch_hands_the_claim_back_with_the_refusal_recorded() {
        let (ledger, _, _dir) = waiting("release");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        let id = intent.intent_id;
        ledger.claim_intent(&id, &run("run-1"), &up).expect("claim");
        assert!(ledger
            .release_claim(&id, &run("run-1"), "pricing: no price for model x")
            .expect("release"));
        let after = ledger.successor_intent(&id).expect("read").expect("row");
        assert_eq!(after.state, IntentState::Pending);
        assert_eq!(after.run_id, None);
        assert_eq!(
            ledger.intent_refusal(&id).expect("read").as_deref(),
            Some("pricing: no price for model x")
        );
        // Listed at once; launchable once the refusal's backoff has passed.
        assert_eq!(ledger.listed_intents(&up).expect("list").len(), 1);
        assert!(ledger.launchable_intents(&up).expect("list").is_empty());
        // A release by a run that no longer holds the claim changes nothing.
        assert!(!ledger
            .release_claim(&id, &run("run-9"), "stale")
            .expect("release"));
    }

    #[test]
    fn a_refused_intent_is_not_launchable_inside_its_backoff_and_is_after_it() {
        let (ledger, seconds, _dir) = waiting("backoff");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        let id = intent.intent_id;
        ledger.claim_intent(&id, &run("run-1"), &up).expect("claim");
        ledger
            .release_claim(&id, &run("run-1"), "pricing: no price")
            .expect("release");
        let refused = ledger.successor_intent(&id).expect("read").expect("row");
        assert_eq!(refused.refusal_count, 1);
        assert!(refused.retry_after().is_some());
        // Inside the backoff it is listed (with its refusal) but not launchable.
        seconds.store(REFUSAL_BACKOFF_SECONDS - 1, Ordering::SeqCst);
        assert!(ledger.launchable_intents(&up).expect("list").is_empty());
        assert_eq!(ledger.listed_intents(&up).expect("list").len(), 1);
        seconds.store(REFUSAL_BACKOFF_SECONDS, Ordering::SeqCst);
        assert_eq!(ledger.launchable_intents(&up).expect("list").len(), 1);
        // A second refusal doubles the wait.
        ledger.claim_intent(&id, &run("run-2"), &up).expect("claim");
        ledger
            .release_claim(&id, &run("run-2"), "pricing: no price")
            .expect("release");
        // Refused at 60 s, a second refusal waits 120 s: until 180 s.
        seconds.store(179, Ordering::SeqCst);
        assert!(ledger.launchable_intents(&up).expect("list").is_empty());
        seconds.store(180, Ordering::SeqCst);
        assert_eq!(ledger.launchable_intents(&up).expect("list").len(), 1);
    }

    #[test]
    fn the_backoff_grows_to_ten_minutes_and_stays() {
        assert_eq!(refusal_backoff_seconds(1), 60);
        assert_eq!(refusal_backoff_seconds(2), 120);
        assert_eq!(refusal_backoff_seconds(5), 600);
        assert_eq!(refusal_backoff_seconds(500), 600);
    }

    /// The runs a recovering plugin has launched for an intent: each poll
    /// lists what is launchable, claims it for a new run and, when the claim
    /// is taken, inserts that run's row (a launch).
    fn recover(ledger: &Ledger, label: &str, alive: &dyn Fn(u32) -> SuccessorProcess) {
        for intent in ledger.launchable_intents(alive).expect("list") {
            let new_run = run(&format!("succ-{label}"));
            if matches!(
                ledger
                    .claim_intent(&intent.intent_id, &new_run, alive)
                    .expect("claim"),
                Claim::Claimed(_)
            ) {
                ledger
                    .insert_run(
                        &new_run,
                        "/r",
                        None,
                        &crate::ids::TaskId::from_stored("task-a"),
                        "rk",
                    )
                    .expect("launch");
            }
        }
    }

    fn successor_runs(ledger: &Ledger) -> i64 {
        ledger
            .conn
            .query_row(
                "SELECT COUNT(*) FROM runs WHERE id LIKE 'succ-%'",
                [],
                |r| r.get(0),
            )
            .expect("count")
    }

    #[test]
    fn a_crash_after_the_decide_commit_still_yields_one_successor_run() {
        let (ledger, _, _dir) = waiting("fp-decide");
        failpoint::arm(failpoint::AFTER_DECIDE_COMMIT);
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ledger.resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
        }));
        failpoint::disarm();
        assert!(crashed.is_err());
        // A repeated decide finds the answered decision and writes no
        // second intent; the committed one is launched once.
        assert_eq!(
            ledger
                .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
                .expect("repeat"),
            None
        );
        assert_eq!(ledger.count_successor_intents().expect("count"), 1);
        recover(&ledger, "1", &up);
        recover(&ledger, "2", &up);
        assert_eq!(successor_runs(&ledger), 1);
    }

    #[test]
    fn a_crash_after_the_claim_still_yields_one_successor_run() {
        let (ledger, seconds, _dir) = waiting("fp-claim");
        let intent = ledger
            .resolve_decision_with_intent(&run("run-a"), &answer(), &intent_for("run-a"))
            .expect("written")
            .expect("open");
        failpoint::arm(failpoint::AFTER_CLAIM);
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ledger.claim_intent(&intent.intent_id, &run("lost"), &up)
        }));
        failpoint::disarm();
        assert!(crashed.is_err());
        // Inside the grace the claim holds; once it passes with no run row
        // the next poll takes the claim over, once.
        recover(&ledger, "1", &up);
        assert_eq!(successor_runs(&ledger), 0);
        seconds.store(CLAIM_GRACE_SECONDS + 1, Ordering::SeqCst);
        recover(&ledger, "2", &up);
        recover(&ledger, "3", &up);
        assert_eq!(successor_runs(&ledger), 1);
    }
}
