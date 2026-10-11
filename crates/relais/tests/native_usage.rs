//! `relais native usage` (SPEC §23), through the real binary: a dispatch's
//! cumulative usage is booked as a delta, once, whatever the order and the
//! number of callers.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Output, Stdio};

use relais::ids::{DispatchId, RunId, TaskId};
use relais::ledger::Ledger;
use relais::route::RoutedBy;
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");
const DISPATCH: &str = "d-1";

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("nu-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        let world = Self {
            _scratch: scratch,
            state,
            root,
        };
        let ledger = world.ledger();
        let run = RunId::from_stored("run-u");
        ledger
            .insert_run(
                &run,
                "/repo",
                Some("s"),
                &TaskId::from_stored("run-u"),
                "rk",
            )
            .expect("run");
        ledger
            .record_dispatch_intent(
                &DispatchId::from_stored(DISPATCH),
                &run,
                None,
                &serde_json::json!({ "kind": "initial", "model": "m" }),
                0,
                RoutedBy::RiskFloor,
            )
            .expect("dispatch");
        drop(ledger);
        world
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.state.join("ledger.sqlite")).expect("ledger")
    }

    fn usage(&self, stdin: Option<&str>) -> Output {
        let mut child = common::relais(BIN)
            .args(["native", "usage", "--dispatch", DISPATCH])
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("relais runs");
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(text.as_bytes())
                .expect("write");
        }
        child.wait_with_output().expect("output")
    }

    fn booked_input(&self) -> i64 {
        let conn = rusqlite::Connection::open(self.state.join("ledger.sqlite")).expect("conn");
        conn.query_row(
            "SELECT COALESCE(SUM(input_tokens), 0) FROM usage_events WHERE run_id = 'run-u'",
            [],
            |row| row.get(0),
        )
        .expect("sum")
    }

    fn rows(&self) -> i64 {
        let conn = rusqlite::Connection::open(self.state.join("ledger.sqlite")).expect("conn");
        conn.query_row(
            "SELECT COUNT(*) FROM usage_events WHERE run_id = 'run-u'",
            [],
            |row| row.get(0),
        )
        .expect("count")
    }

    fn events(&self) -> String {
        std::fs::read_to_string(self.state.join("runs").join("run-u").join("events.jsonl"))
            .unwrap_or_default()
    }
}

fn report(input: u64, model: &str) -> String {
    serde_json::json!({
        "agent": "a-1",
        "turns": 1,
        "usage": { "input_tokens": input, "output_tokens": 1, "model": model },
        "message_ids": [format!("msg-{input}")],
    })
    .to_string()
}

#[test]
fn eight_concurrent_calls_with_shuffled_totals_book_exactly_the_final_total() {
    let world = World::new("conc");
    let totals = [30_u64, 80, 10, 100, 50, 20, 90, 60];
    std::thread::scope(|scope| {
        for total in totals {
            let world = &world;
            scope.spawn(move || {
                let output = world.usage(Some(&report(total, "m")));
                assert_eq!(output.status.code(), Some(0), "{output:?}");
            });
        }
    });
    assert_eq!(world.booked_input(), 100);
}

#[test]
fn a_lower_cumulative_books_nothing_and_the_same_one_twice_books_once() {
    let world = World::new("order");
    assert_eq!(world.usage(Some(&report(50, "m"))).status.code(), Some(0));
    assert_eq!(world.booked_input(), 50);
    assert_eq!(world.usage(Some(&report(20, "m"))).status.code(), Some(0));
    assert_eq!(world.booked_input(), 50);
    assert_eq!(world.usage(Some(&report(70, "m"))).status.code(), Some(0));
    assert_eq!(world.usage(Some(&report(70, "m"))).status.code(), Some(0));
    assert_eq!(world.booked_input(), 70);
    assert_eq!(world.rows(), 2);
}

#[test]
fn stopped_after_usage_books_only_the_remaining_delta() {
    let world = World::new("stopped");
    assert_eq!(world.usage(Some(&report(60, "m"))).status.code(), Some(0));
    let ledger = world.ledger();
    // What the runner books when `stopped` arrives (the runner's own path
    // is `runner::tests::stopped_after_per_turn_usage_books_only_the_rest_in_one_phase`):
    // the dispatch's whole usage, under the dispatch's own id.
    let template = relais::ledger::UsageEvent {
        event_id: DISPATCH.into(),
        run_id: RunId::from_stored("run-u"),
        attempt_id: None,
        parent_event_id: None,
        model: Some("m".into()),
        input_tokens: Some(100),
        output_tokens: Some(1),
        cache_read_tokens: None,
        cache_write_tokens: None,
        cost: None,
        cost_kind: relais::money::CostKind::EstimatedApiEquivalent,
        completeness: relais::money::CostCompleteness::Unknown,
        inclusive: false,
        at: ledger.now(),
        phase: None,
        duration_ms: None,
        requested_model: None,
        requested_effort: None,
        harness: None,
    };
    let delta = ledger
        .record_usage_delta(&template, &[], relais::ledger::Booking::Final)
        .expect("book")
        .expect("something new");
    assert_eq!(delta.input_tokens, Some(40));
    assert_eq!(
        delta.event_id, DISPATCH,
        "the final row carries the bare id"
    );
    assert_eq!(world.booked_input(), 100);
    assert!(ledger
        .record_usage_delta(&template, &[], relais::ledger::Booking::Final)
        .expect("book")
        .is_none());
}

#[test]
fn empty_stdin_exits_2_and_never_blocks() {
    let world = World::new("empty");
    let output = world.usage(Some(""));
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!output.stderr.is_empty());
    assert_eq!(world.rows(), 0);
}

#[test]
fn an_unpriced_model_is_named_in_the_cost_event_with_a_clean_short_name() {
    let world = World::new("unpriced");
    let model = format!("evil\u{1b}[31m{}\u{7}", "x".repeat(80));
    let output = world.usage(Some(&report(5, &model)));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let events = world.events();
    let line = events
        .lines()
        .find(|line| line.contains("\"kind\":\"cost\""))
        .expect("a cost event");
    let event: serde_json::Value = serde_json::from_str(line).expect("json");
    let reason = event["event"]["unpriced_reason"]
        .as_str()
        .expect("a reason");
    assert!(!reason.chars().any(char::is_control), "{reason:?}");
    let name = reason.split(':').next().expect("name");
    assert!(name.starts_with("evil"), "{name}");
    assert!(name.chars().count() <= 40, "{name}");
}
