//! `relais native status` and its `current` key (SPEC §23), through the
//! real binary: what the ledger holds for a run now, read without
//! creating, migrating or waiting long on the ledger. The plugin checks
//! an outcome message against it just before the message goes out, so a
//! decision answered since the run ended is not announced as still open.

use crate::common;

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
    /// Released only after the scratch directory is gone, so the cap bounds
    /// live worlds, not finished ones.
    _permit: std::sync::Arc<common::WorldPermit>,
}

impl World {
    fn new(tag: &str) -> Self {
        Self::with_permit(tag, std::sync::Arc::new(common::WorldPermit::acquire(1)))
    }

    /// Builds a world under a permit the caller already holds, so a test
    /// with several worlds takes one `acquire(n)` for all of them.
    fn with_permit(tag: &str, permit: std::sync::Arc<common::WorldPermit>) -> Self {
        let scratch = short_temp_dir(&format!("ns-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        Self {
            _permit: permit,
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
                actor: relais::ledger::Actor::Person,
                person_quote: None,
                recorded_via: relais::ledger::RecordedVia::Cli,
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

/// A run still `running`, as a driver that never stamped itself leaves it.
fn running(ledger: &Ledger, run: &str, session: Option<&str>) {
    let id = RunId::from_stored(run);
    ledger
        .insert_run(&id, "/repo", session, &TaskId::from_stored(run), "rk")
        .expect("run");
    ledger
        .record_transition(&Transition {
            run_id: id,
            attempt_id: None,
            from_state: Some(State::Prepared),
            to_state: State::Running,
            reason: Reason::WorkerDispatched.as_str().into(),
            detail: None,
            at: ledger.now(),
        })
        .expect("running");
}

impl World {
    fn status_with(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = common::relais(BIN);
        cmd.args(["native", "status"])
            .args(args)
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root);
        for (key, value) in envs {
            cmd.env(key, value);
        }
        cmd.output().expect("relais runs")
    }

    fn json_with(&self, args: &[&str], envs: &[(&str, &str)]) -> serde_json::Value {
        let output = self.status_with(args, envs);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).expect("one JSON object")
    }

    fn events_path(&self, run: &str) -> PathBuf {
        self.state.join("runs").join(run).join("events.jsonl")
    }
}

#[test]
fn a_session_with_no_runs_prints_an_empty_list_and_exits_zero() {
    let world = World::new("sess0");
    // No ledger at all.
    assert_eq!(
        world.json_with(&["--session", "nobody"], &[]),
        serde_json::json!({ "runs": [] })
    );
    // A ledger that holds only another session's run.
    running(&world.ledger(), "run-other", Some("sess-other"));
    assert_eq!(
        world.json_with(&["--session", "nobody"], &[]),
        serde_json::json!({ "runs": [] })
    );
    let mine = world.json_with(&["--session", "sess-other"], &[]);
    assert_eq!(mine["runs"].as_array().expect("runs").len(), 1, "{mine}");
    assert_eq!(mine["runs"][0]["current"]["session"], "sess-other");
}

#[test]
fn a_run_silent_past_the_stall_threshold_is_stalled_and_a_recent_one_is_not() {
    let world = World::new("stall");
    running(&world.ledger(), "run-st", Some("s"));
    world.events("run-st", "none");
    let events = std::fs::File::options()
        .write(true)
        .open(world.events_path("run-st"))
        .expect("events");
    events
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(120))
        .expect("mtime");
    drop(events);
    let stalled = world.json_with(&["--run", "run-st"], &[("RELAIS_STALLED_AFTER_SECS", "60")]);
    assert_eq!(stalled["current"]["stalled"], true, "{stalled}");
    assert!(stalled["current"]["silent_seconds"].as_u64().expect("secs") >= 120);
    assert_eq!(
        stalled["current"]["state"], "running",
        "never reaped by stall"
    );
    let recent = world.json_with(
        &["--run", "run-st"],
        &[("RELAIS_STALLED_AFTER_SECS", "3600")],
    );
    assert!(recent["current"].get("stalled").is_none(), "{recent}");
}

#[cfg(unix)]
#[test]
fn status_reaps_a_run_whose_driver_is_gone_and_leaves_one_outcome() {
    let world = World::new("reap");
    let ledger = world.ledger();
    running(&ledger, "run-gone", Some("s"));
    // A pid that is certainly dead: a child that has been waited for.
    let mut child = std::process::Command::new("true").spawn().expect("child");
    let dead = child.id();
    child.wait().expect("wait");
    let long_ago = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let conn = rusqlite::Connection::open(world.ledger_path()).expect("conn");
    conn.execute(
        "UPDATE runs SET driver_pid = ?1, driver_started = 'gone', driver_since = ?2 WHERE id = 'run-gone'",
        rusqlite::params![dead, long_ago],
    )
    .expect("driver");
    drop(conn);
    drop(ledger);
    for _ in 0..2 {
        let json = world.json_with(&["--run", "run-gone"], &[]);
        assert_eq!(json["current"]["state"], "interrupted", "{json}");
        assert_eq!(json["outcome"]["state"], "interrupted", "{json}");
        assert!(
            json["current"].get("verdict").is_none(),
            "a verdict describes an open run, not a reaped one: {json}"
        );
    }
    let text = std::fs::read_to_string(world.events_path("run-gone")).expect("events");
    let outcomes = text
        .lines()
        .filter(|l| l.contains("\"kind\":\"outcome\""))
        .count();
    assert_eq!(outcomes, 1, "{text}");
}

/// A run whose driver is gone: a dead pid, stamped long ago. The reaping
/// tests below are Unix-only: a dead driver is a waited-for `true`, and a
/// read-only ledger is a mode bit.
#[cfg(unix)]
fn with_dead_driver(world: &World, run: &str) {
    let mut child = std::process::Command::new("true").spawn().expect("child");
    let dead = child.id();
    child.wait().expect("wait");
    let long_ago = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    let conn = rusqlite::Connection::open(world.ledger_path()).expect("conn");
    conn.execute(
        "UPDATE runs SET driver_pid = ?1, driver_started = 'gone', driver_since = ?2 WHERE id = ?3",
        rusqlite::params![dead, long_ago, run],
    )
    .expect("driver");
}

#[cfg(unix)]
fn ledger_state(world: &World, run: &str) -> Option<State> {
    let conn = rusqlite::Connection::open(world.ledger_path()).expect("conn");
    let state: Option<String> = conn
        .query_row(
            "SELECT to_state FROM transitions WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
            [run],
            |row| row.get(0),
        )
        .ok();
    state.map(|s| match s.as_str() {
        "running" => State::Running,
        other => panic!("unexpected state {other}"),
    })
}

#[cfg(unix)]
#[test]
fn a_non_terminal_run_carries_the_orphan_verdict() {
    let world = World::new("verdict");
    running(&world.ledger(), "run-v", Some("s"));
    world.events("run-v", "none");
    let alive = world.json_with(&["--run", "run-v"], &[]);
    assert_eq!(alive["current"]["verdict"], "alive", "{alive}");
    // A gone driver on a writable ledger is reaped at once: the state says
    // so, and no verdict contradicts it (the read-only case keeps "gone").
    with_dead_driver(&world, "run-v");
    let reaped = world.json_with(&["--run", "run-v"], &[]);
    assert_eq!(reaped["current"]["state"], "interrupted", "{reaped}");
    assert!(reaped["current"].get("verdict").is_none(), "{reaped}");
}

#[cfg(unix)]
#[test]
fn a_read_only_ledger_reports_the_orphan_and_writes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new("rolock");
    running(&world.ledger(), "run-rl", Some("s"));
    world.events("run-rl", "none");
    with_dead_driver(&world, "run-rl");
    let before = std::fs::read_to_string(world.events_path("run-rl")).expect("events");
    let path = world.ledger_path();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("chmod");
    let json = world.json_with(&["--run", "run-rl"], &[]);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(json["current"]["orphaned"], true, "{json}");
    assert_eq!(json["current"]["state"], "running", "{json}");
    assert_eq!(json["current"]["verdict"], "gone", "{json}");
    assert_eq!(ledger_state(&world, "run-rl"), Some(State::Running));
    let after = std::fs::read_to_string(world.events_path("run-rl")).expect("events");
    assert_eq!(before, after);
}

#[cfg(unix)]
#[test]
fn a_ledger_on_another_schema_is_not_reaped() {
    let world = World::new("noreap");
    running(&world.ledger(), "run-ro", Some("s"));
    world.events("run-ro", "none");
    with_dead_driver(&world, "run-ro");
    let events_before = std::fs::read_to_string(world.events_path("run-ro")).expect("events");
    let conn = rusqlite::Connection::open(world.ledger_path()).expect("conn");
    conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES ('v999', 'now')",
        [],
    )
    .expect("a newer step");
    drop(conn);
    let json = world.json_with(&["--run", "run-ro"], &[]);
    assert!(json.get("current").is_none(), "{json}");
    assert_eq!(ledger_state(&world, "run-ro"), Some(State::Running));
    let events_after = std::fs::read_to_string(world.events_path("run-ro")).expect("events");
    assert_eq!(events_before, events_after);
    assert!(!events_after.lines().any(|l| l.contains("interrupted")));
}

/// The plugin applies a status only if its revision is newer, and puts
/// the ledger's cost in place of the cost it summed.
#[test]
fn status_carries_the_last_seq_read_and_the_ledger_cost_total() {
    let world = World::new("revision");
    let ledger = world.ledger();
    ended(
        &ledger,
        "run-v",
        State::NeedsDecision,
        Reason::VerificationInputsChanged,
    );
    drop(ledger);
    let dir = world.state.join("runs").join("run-v");
    std::fs::create_dir_all(&dir).expect("run dir");
    let line = |seq: Option<u64>, kind: &str| {
        let mut entry = serde_json::json!({
            "at": "2026-10-09T11:32:24Z",
            "run": "run-v",
            "event": { "kind": kind, "booked": 5, "completeness": "actual" },
        });
        if let Some(seq) = seq {
            entry["seq"] = seq.into();
        }
        format!("{entry}\n")
    };
    let text = [
        line(Some(0), "cost"),
        line(Some(1), "cost"),
        line(None, "cost"),
    ]
    .concat();
    std::fs::write(dir.join("events.jsonl"), text).expect("events");
    let (json, _) = world.status_json("run-v");
    assert_eq!(json["revision"], 1, "{json}");
    assert_eq!(json["current"]["cost_total"]["micros"], 0, "{json}");
    assert!(
        json["current"]["cost_total"]["completeness"].is_string(),
        "{json}"
    );
}
