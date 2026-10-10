//! `relais native status` and its `current` key (SPEC §23), through the
//! real binary: what the ledger holds for a run now, read without
//! creating, migrating or waiting long on the ledger. The plugin checks
//! an outcome message against it just before the message goes out, so a
//! decision answered since the run ended is not announced as still open.

mod common;

use std::path::PathBuf;
use std::process::Output;

use relais::ids::{RunId, TaskId};
use relais::ledger::{DecisionAnswer, Ledger, Transition};
use relais::lifecycle::{Reason, State};
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

/// A run's state and its own events file, in a world of its own.
struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("ns-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        Self {
            _scratch: scratch,
            state,
            root,
        }
    }

    fn ledger_path(&self) -> PathBuf {
        self.state.join("ledger.sqlite")
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.ledger_path()).expect("ledger")
    }

    /// The `events.jsonl` a run that ended in `outcome` leaves behind.
    fn events(&self, run: &str, outcome: &str) {
        let dir = self.state.join("runs").join(run);
        std::fs::create_dir_all(&dir).expect("run dir");
        let line = serde_json::json!({
            "at": "2026-10-09T11:32:24Z",
            "run": run,
            "event": { "kind": "outcome", "state": outcome, "receipt": null, "summary": null },
        });
        std::fs::write(dir.join("events.jsonl"), format!("{line}\n")).expect("events");
    }

    fn status(&self, run: &str) -> Output {
        common::relais(BIN)
            .args(["native", "status", "--run", run])
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .output()
            .expect("relais runs")
    }

    fn status_json(&self, run: &str) -> (serde_json::Value, String) {
        let output = self.status(run);
        assert!(output.status.success(), "{output:?}");
        let json = serde_json::from_slice(&output.stdout).expect("one JSON object");
        (json, String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

/// A run that went through `Running` and landed on `to`.
fn ended(ledger: &Ledger, run: &str, to: State, reason: Reason) {
    let id = RunId::from_stored(run);
    ledger
        .insert_run(&id, "/repo", None, &TaskId::from_stored(run), "rk")
        .expect("run");
    for (from, to, reason) in [
        (State::Prepared, State::Running, Reason::WorkerDispatched),
        (State::Running, to, reason),
    ] {
        ledger
            .record_transition(&Transition {
                run_id: id.clone(),
                attempt_id: None,
                from_state: Some(from),
                to_state: to,
                reason: reason.as_str().into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
    }
}

fn approve(ledger: &Ledger, run: &str) {
    let answered = ledger
        .resolve_decision(
            &RunId::from_stored(run),
            &DecisionAnswer {
                resolution: Reason::DecisionApproved,
                actor: "person",
                note: None,
                successor_run: None,
                from_state: State::NeedsDecision,
                to_state: State::Accepted,
            },
        )
        .expect("decide");
    assert!(answered);
}

/// The incident: the run ended `needs_decision` and was approved since.
/// The timeline still says how it ended; `current` says what holds now.
#[test]
fn a_resolved_decision_reads_back_as_the_current_state() {
    let world = World::new("resolved");
    let ledger = world.ledger();
    ended(
        &ledger,
        "run-r",
        State::NeedsDecision,
        Reason::VerificationInputsChanged,
    );
    approve(&ledger, "run-r");
    drop(ledger);
    world.events("run-r", "needs_decision");
    let (json, stderr) = world.status_json("run-r");
    assert_eq!(json["outcome"]["state"], "needs_decision", "{json}");
    let current = &json["current"];
    assert_eq!(current["state"], "accepted", "{json}");
    assert_eq!(current["decision"]["raised_state"], "needs_decision");
    assert_eq!(
        current["decision"]["raised_reason"],
        "verification_inputs_changed"
    );
    assert_eq!(current["decision"]["resolution"], "decision_approved");
    assert_eq!(current["decision"]["actor"], "person");
    assert!(current["decision"]["resolved_at"].is_string(), "{json}");
    assert_eq!(stderr, "");
}

#[test]
fn an_open_decision_has_no_resolution() {
    let world = World::new("open");
    ended(
        &world.ledger(),
        "run-o",
        State::NeedsDecision,
        Reason::VerificationInputsChanged,
    );
    world.events("run-o", "needs_decision");
    let (json, _) = world.status_json("run-o");
    assert_eq!(json["current"]["state"], "needs_decision", "{json}");
    assert!(
        json["current"]["decision"]["resolution"].is_null(),
        "{json}"
    );
    assert!(
        json["current"]["decision"]["resolved_at"].is_null(),
        "{json}"
    );
}

/// The plugin compares `current.state` with the `done` line's `outcome`,
/// which is `State::as_str`; the two spellings must not drift apart.
#[test]
fn every_state_awaiting_a_person_reads_back_as_the_done_line_spells_it() {
    let world = World::new("spell");
    let ledger = world.ledger();
    for (run, state, reason) in [
        (
            "run-d",
            State::NeedsDecision,
            Reason::VerificationInputsChanged,
        ),
        ("run-v", State::NeedsReview, Reason::ReviewFindings),
        ("run-i", State::Interrupted, Reason::ReconciledInterrupted),
    ] {
        assert!(state.awaits_a_person());
        ended(&ledger, run, state, reason);
        world.events(run, state.as_str());
        let (json, _) = world.status_json(run);
        assert_eq!(json["current"]["state"], state.as_str(), "{json}");
        assert_eq!(json["current"]["decision"]["raised_state"], state.as_str());
    }
}

/// A run refused before it started has events and no ledger row: no
/// `current`, and nothing on stderr, because nothing is wrong.
#[test]
fn a_run_the_ledger_does_not_know_has_no_current_and_no_warning() {
    let world = World::new("unknown");
    drop(world.ledger());
    world.events("run-u", "blocked");
    let (json, stderr) = world.status_json("run-u");
    assert!(json.get("current").is_none(), "{json}");
    assert_eq!(stderr, "");
}

/// Reading never creates: no ledger file, no `current`, one stderr line,
/// exit 0 and the timeline as before.
#[test]
fn an_empty_state_directory_stays_empty() {
    let world = World::new("empty");
    world.events("run-e", "needs_decision");
    let (json, stderr) = world.status_json("run-e");
    assert_eq!(json["outcome"]["state"], "needs_decision", "{json}");
    assert!(json.get("current").is_none(), "{json}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(!world.ledger_path().exists(), "a read created the ledger");
    assert!(!world.state.join("ledger.sqlite-wal").exists());
}

/// Under WAL a writer does not block readers: a transaction holding the
/// write lock leaves the read, and its answer, intact.
#[test]
fn a_writer_in_progress_does_not_hide_the_current_state() {
    let world = World::new("writer");
    ended(
        &world.ledger(),
        "run-w",
        State::NeedsDecision,
        Reason::VerificationInputsChanged,
    );
    world.events("run-w", "needs_decision");
    let writer = rusqlite::Connection::open(world.ledger_path()).expect("writer");
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE runs SET updated_at = updated_at;")
        .expect("hold the write lock");
    let output = world.status("run-w");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(json["current"]["state"], "needs_decision", "{json}");
    writer.execute_batch("ROLLBACK").expect("release");
}

/// A connection in exclusive locking mode shuts every reader out: the
/// read gives up, prints the timeline without `current`, and exits 0.
/// How long it waits first is the ledger's own test
/// (`a_read_only_open_gives_up_after_its_own_short_wait`): timed around a
/// process start, the bound measured the machine's load instead.
#[test]
fn an_exclusive_lock_costs_the_current_state_not_the_timeline() {
    let world = World::new("exclusive");
    ended(
        &world.ledger(),
        "run-x",
        State::NeedsDecision,
        Reason::VerificationInputsChanged,
    );
    world.events("run-x", "needs_decision");
    let holder = rusqlite::Connection::open(world.ledger_path()).expect("holder");
    holder
        .execute_batch(
            "PRAGMA locking_mode = EXCLUSIVE; \
             BEGIN IMMEDIATE; UPDATE runs SET updated_at = updated_at; COMMIT;",
        )
        .expect("take the exclusive lock");
    let output = world.status("run-x");
    assert!(output.status.success(), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(json["outcome"]["state"], "needs_decision", "{json}");
    assert!(json.get("current").is_none(), "{json}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains("locked"), "{stderr}");
    drop(holder);
}

/// The normal state between runs: a WAL ledger with every writer closed.
/// The read-only open reads it, and leaves the database file unchanged
/// (SQLite may still create `-wal`/`-shm` beside it for a reader).
#[test]
fn a_closed_wal_ledger_is_read_and_left_unchanged() {
    let world = World::new("closed");
    let ledger = world.ledger();
    ended(&ledger, "run-c", State::NeedsReview, Reason::ReviewFindings);
    drop(ledger);
    world.events("run-c", "needs_review");
    let before = std::fs::read(world.ledger_path()).expect("ledger bytes");
    let (json, stderr) = world.status_json("run-c");
    assert_eq!(json["current"]["state"], "needs_review", "{json}");
    assert_eq!(stderr, "");
    let after = std::fs::read(world.ledger_path()).expect("ledger bytes");
    assert!(before == after, "a read changed the database file");
}

/// A ledger on another schema is not read: the reader cannot migrate it.
#[test]
fn a_ledger_on_another_schema_is_not_read() {
    let world = World::new("schema");
    ended(
        &world.ledger(),
        "run-s",
        State::NeedsDecision,
        Reason::VerificationInputsChanged,
    );
    world.events("run-s", "needs_decision");
    let conn = rusqlite::Connection::open(world.ledger_path()).expect("conn");
    conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES ('v999', 'now')",
        [],
    )
    .expect("a newer step");
    drop(conn);
    let (json, stderr) = world.status_json("run-s");
    assert!(json.get("current").is_none(), "{json}");
    assert!(stderr.contains("migration"), "{stderr}");
}
