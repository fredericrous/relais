//! `relais report runs --json` (time-to-done B1), through the real binary:
//! the runs of a window with their start, end and hours, read without
//! writing the ledger. greenpiste's time-to-done measure joins on it.

use crate::common;

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
    _permit: std::sync::Arc<common::WorldPermit>,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("rr-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        Self {
            _permit: std::sync::Arc::new(common::WorldPermit::acquire(1)),
            _scratch: scratch,
            state,
            root,
        }
    }

    fn ledger_path(&self) -> PathBuf {
        self.state.join("ledger.sqlite")
    }

    fn relais(&self, args: &[&str]) -> Output {
        common::relais(BIN)
            .args(args)
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .output()
            .expect("relais runs")
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.relais(args);
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
        serde_json::from_slice(&output.stdout).expect("one JSON document")
    }
}

/// A run that went through `Running`, and landed on `to` when it is given.
fn run(ledger: &Ledger, id: &str, session: Option<&str>, to: Option<State>) {
    let run_id = RunId::from_stored(id);
    ledger
        .insert_run(&run_id, "/repo/a", session, &TaskId::from_stored(id), "rk")
        .expect("run");
    let mut steps = vec![(State::Prepared, State::Running, Reason::WorkerDispatched)];
    if let Some(to) = to {
        steps.push((State::Running, to, Reason::ChecksAndReviewPassed));
    }
    for (from, to, reason) in steps {
        ledger
            .record_transition(&Transition {
                run_id: run_id.clone(),
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

/// Pin the stored times, so the hours are values, not "whenever the test ran".
fn stamp(world: &World, id: &str, created: &str, updated: &str, attempts: &[(&str, Option<&str>)]) {
    let conn = rusqlite::Connection::open(world.ledger_path()).expect("ledger file");
    conn.execute(
        "UPDATE runs SET created_at = ?2, updated_at = ?3 WHERE id = ?1",
        rusqlite::params![id, created, updated],
    )
    .expect("stamp run");
    for (index, (start, end)) in attempts.iter().enumerate() {
        conn.execute(
            "INSERT INTO attempts
                (run_id, revision_id, attempt_index, tier, phase, state, started_at, ended_at)
             VALUES (?1, 1, ?2, 't', 'worker', 'running', ?3, ?4)",
            rusqlite::params![id, index as i64, start, end],
        )
        .expect("attempt");
    }
}

fn seeded(tag: &str) -> World {
    let world = World::new(tag);
    {
        let ledger = Ledger::open(&world.ledger_path()).expect("ledger");
        run(&ledger, "run-old", Some("s0"), Some(State::Accepted));
        run(&ledger, "run-done", Some("s1"), Some(State::Accepted));
        run(&ledger, "run-live", None, None);
    }
    stamp(
        &world,
        "run-old",
        "2026-10-01T09:00:00+00:00",
        "2026-10-01T10:00:00+00:00",
        &[],
    );
    stamp(
        &world,
        "run-done",
        "2026-11-10T10:00:00+00:00",
        "2026-11-10T11:30:00+00:00",
        &[
            (
                "2026-11-10T10:05:00+00:00",
                Some("2026-11-10T10:35:00+00:00"),
            ),
            (
                "2026-11-10T10:40:00+00:00",
                Some("2026-11-10T11:20:00+00:00"),
            ),
        ],
    );
    stamp(
        &world,
        "run-live",
        "2026-11-14T08:00:00+00:00",
        "2026-11-14T08:00:00+00:00",
        &[
            (
                "2026-11-14T08:10:00+00:00",
                Some("2026-11-14T08:20:00+00:00"),
            ),
            ("2026-11-14T08:30:00+00:00", None),
        ],
    );
    world
}

fn usage_events(world: &World) -> i64 {
    rusqlite::Connection::open(world.ledger_path())
        .expect("ledger file")
        .query_row("SELECT COUNT(*) FROM usage_events", [], |row| row.get(0))
        .expect("count")
}

#[test]
fn the_window_lists_each_run_with_its_start_end_and_hours() {
    let world = seeded("window");
    let doc = world.json(&["report", "runs", "--json", "--since", "2026-11-01"]);
    assert_eq!(doc["schema"], 1, "{doc}");
    assert_eq!(doc["since"], "2026-11-01", "{doc}");
    let runs = doc["runs"].as_array().expect("runs");
    assert_eq!(runs.len(), 2, "run-old is before the window: {doc}");

    let done = &runs[0];
    assert_eq!(done["id"], "run-done", "oldest first: {doc}");
    assert_eq!(done["repo"], "/repo/a");
    assert_eq!(done["root_session"], "s1");
    assert_eq!(done["status"], "accepted");
    assert_eq!(done["started_at"], "2026-11-10T10:05:00+00:00");
    assert_eq!(done["ended_at"], "2026-11-10T11:20:00+00:00");
    assert_eq!(done["hours"], 1.25, "10:05 to 11:20: {doc}");

    let live = &runs[1];
    assert_eq!(live["id"], "run-live");
    assert!(live["root_session"].is_null(), "{doc}");
    assert_eq!(live["status"], "running");
    assert_eq!(live["started_at"], "2026-11-14T08:10:00+00:00");
    assert!(
        live["ended_at"].is_null(),
        "a finished first attempt does not end a run that still goes: {doc}"
    );
    assert!(live["hours"].as_f64().expect("hours") > 0.0, "{doc}");
}

#[test]
fn an_empty_window_is_an_empty_list_with_exit_zero() {
    let world = seeded("empty");
    let doc = world.json(&["report", "runs", "--json", "--since", "2027-01-01"]);
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["runs"], serde_json::json!([]), "{doc}");
}

#[test]
fn a_machine_with_no_ledger_has_no_runs_and_gets_none_made() {
    let world = World::new("noledger");
    let doc = world.json(&["report", "runs", "--json", "--since", "2026-11-01"]);
    assert_eq!(doc["runs"], serde_json::json!([]), "{doc}");
    assert!(
        !world.ledger_path().exists(),
        "a read must not create the ledger"
    );
}

#[test]
fn the_default_window_is_the_start_of_the_current_month() {
    let world = seeded("default");
    let doc = world.json(&["report", "runs", "--json"]);
    let since = doc["since"].as_str().expect("since");
    assert!(since.ends_with("-01T00:00:00+00:00"), "{doc}");
}

#[test]
fn json_is_required() {
    let world = seeded("nojson");
    let output = world.relais(&["report", "runs"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn report_flags_are_not_part_of_the_subcommand() {
    let world = seeded("flags");
    for extra in [["--by", "task-type"], ["--no-import", ""]] {
        let mut args = vec!["report", "runs", "--json"];
        args.extend(extra.iter().filter(|a| !a.is_empty()));
        let output = world.relais(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
    }
    // The parent's own flags may not be mixed with the subcommand.
    let output = world.relais(&["report", "--since", "2026-11-01", "runs", "--json"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn the_query_writes_nothing() {
    let world = seeded("readonly");
    // Fold the write-ahead log, so the main file is the whole ledger.
    rusqlite::Connection::open(world.ledger_path())
        .expect("ledger file")
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("checkpoint");
    let before = std::fs::read(world.ledger_path()).expect("ledger bytes");
    let events = usage_events(&world);
    world.json(&["report", "runs", "--json", "--since", "2026-11-01"]);
    world.json(&["report", "runs", "--json", "--since", "2026-11-01"]);
    assert_eq!(usage_events(&world), events, "no usage was imported");
    rusqlite::Connection::open(world.ledger_path())
        .expect("ledger file")
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("checkpoint");
    assert_eq!(
        std::fs::read(world.ledger_path()).expect("ledger bytes"),
        before,
        "the ledger file is byte for byte what it was"
    );
}
