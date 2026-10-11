//! `relais native cancel` (SPEC §23), through the real binary: who may
//! cancel which run, and what each case prints and exits with.
// Dead drivers are waited-for `true` processes: Unix process semantics,
// as in release_scenarios.rs.
#![cfg(unix)]

mod common;

use std::path::PathBuf;
use std::process::Output;

use relais::ids::{RunId, TaskId};
use relais::ledger::{Ledger, Transition};
use relais::lifecycle::{Reason, State};
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("nc-{tag}"));
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

    fn cancel(&self, args: &[&str]) -> Output {
        common::relais(BIN)
            .args(["native", "cancel"])
            .args(args)
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .output()
            .expect("relais runs")
    }

    fn events_text(&self, run: &str) -> String {
        std::fs::read_to_string(self.state.join("runs").join(run).join("events.jsonl"))
            .unwrap_or_default()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn move_to(ledger: &Ledger, id: &RunId, from: State, to: State, reason: Reason) {
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

/// A run of `session` that is `running`.
fn running(ledger: &Ledger, run: &str, session: &str) {
    let id = RunId::from_stored(run);
    ledger
        .insert_run(&id, "/repo", Some(session), &TaskId::from_stored(run), "rk")
        .expect("run");
    move_to(
        ledger,
        &id,
        State::Prepared,
        State::Running,
        Reason::WorkerDispatched,
    );
}

/// A driver that is certainly gone: a dead pid, stamped long ago.
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

fn last_transition(world: &World, run: &str) -> (String, String) {
    let conn = rusqlite::Connection::open(world.ledger_path()).expect("conn");
    conn.query_row(
        "SELECT to_state, reason FROM transitions WHERE run_id = ?1 ORDER BY id DESC LIMIT 1",
        [run],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("a transition")
}

#[test]
fn without_a_session_it_refuses_and_names_the_flag() {
    let world = World::new("nosess");
    running(&world.ledger(), "run-a", "s");
    let output = world.cancel(&["--run", "run-a"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("--session"));
    assert_eq!(last_transition(&world, "run-a").0, "running");
}

#[test]
fn an_unknown_run_exits_ten() {
    let world = World::new("unknown");
    world.ledger();
    let output = world.cancel(&["--run", "run-nope", "--session", "s"]);
    assert_eq!(output.status.code(), Some(10), "{output:?}");
}

#[test]
fn an_ended_run_is_reported_and_left_alone() {
    let world = World::new("ended");
    let ledger = world.ledger();
    running(&ledger, "run-e", "s");
    move_to(
        &ledger,
        &RunId::from_stored("run-e"),
        State::Running,
        State::Failed,
        Reason::WorkerDispatched,
    );
    let output = world.cancel(&["--run", "run-e", "--session", "other"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(stdout(&output).trim(), "run run-e already ended: failed");
    assert_eq!(last_transition(&world, "run-e").0, "failed");
}

#[test]
fn a_run_with_a_dead_driver_is_cancelled_from_any_session_with_one_outcome() {
    let world = World::new("dead");
    running(&world.ledger(), "run-d", "owner");
    with_dead_driver(&world, "run-d");
    let output = world.cancel(&["--run", "run-d", "--session", "someone-else"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        stdout(&output).trim(),
        "run run-d had no live driver; marked cancelled"
    );
    assert_eq!(
        last_transition(&world, "run-d"),
        ("cancelled".into(), "cancelled_without_worker".into())
    );
    let again = world.cancel(&["--run", "run-d", "--session", "someone-else"]);
    assert_eq!(again.status.code(), Some(0), "{again:?}");
    assert_eq!(stdout(&again).trim(), "run run-d already ended: cancelled");
    let text = world.events_text("run-d");
    let outcomes = text
        .lines()
        .filter(|line| line.contains("\"kind\":\"outcome\""))
        .count();
    assert_eq!(outcomes, 1, "{text}");
}

#[test]
fn a_live_run_of_another_session_is_refused_with_eleven() {
    let world = World::new("foreign");
    running(&world.ledger(), "run-f", "owner");
    let output = world.cancel(&["--run", "run-f", "--session", "intruder"]);
    assert_eq!(output.status.code(), Some(11), "{output:?}");
    assert_eq!(
        stdout(&output).trim(),
        "run run-f belongs to live session owner; cancel it from that session"
    );
    assert_eq!(last_transition(&world, "run-f").0, "running");
}
