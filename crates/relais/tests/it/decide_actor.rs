//! `relais decide --actor` through the real binary (orchestrator-decides
//! plan, Phase 1): a typed actor, the refusals that keep a model from
//! writing the person's name, and the `--json` shapes.

use crate::common;

use std::path::PathBuf;
use std::process::{Output, Stdio};

use relais::ids::{RunId, TaskId};
use relais::ledger::Ledger;
use relais::ledger::Transition;
use relais::lifecycle::{Reason, State};
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
    /// Released only after the scratch directory is gone, so the cap bounds
    /// live worlds, not finished ones.
    _permit: std::sync::Arc<common::WorldPermit>,
}

impl World {
    /// A world with one run, `run-1`, waiting in `needs_decision` and
    /// carrying a sealed receipt.
    fn new(tag: &str) -> Self {
        Self::with_permit(tag, std::sync::Arc::new(common::WorldPermit::acquire(1)))
    }

    /// Builds a world under a permit the caller already holds, so a test
    /// with several worlds takes one `acquire(n)` for all of them.
    fn with_permit(tag: &str, permit: std::sync::Arc<common::WorldPermit>) -> Self {
        let scratch = short_temp_dir(&format!("da-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        let world = Self {
            _permit: permit,
            _scratch: scratch,
            state,
            root,
        };
        let ledger = Ledger::open(&world.state.join("ledger.sqlite")).expect("ledger");
        let id = RunId::from_stored("run-1");
        ledger
            .insert_run(&id, "/repo", None, &TaskId::from_stored("run-1"), "rk")
            .expect("run");
        for (from, to, reason) in [
            (State::Prepared, State::Running, Reason::WorkerDispatched),
            (State::Running, State::NeedsDecision, Reason::ScopeExceeded),
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
        ledger
            .store_receipt(&id, &serde_json::json!({"sealed": true}), "hash-1")
            .expect("receipt");
        world
    }

    fn decide(&self, args: &[&str]) -> Output {
        common::relais(BIN)
            .arg("decide")
            .arg("run-1")
            .args(args)
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .stdin(Stdio::null())
            .output()
            .expect("relais runs")
    }

    fn receipt(&self) -> Option<(serde_json::Value, String)> {
        Ledger::open(&self.state.join("ledger.sqlite"))
            .expect("ledger")
            .receipt(&RunId::from_stored("run-1"))
            .expect("receipt")
    }

    fn state(&self) -> Option<State> {
        Ledger::open(&self.state.join("ledger.sqlite"))
            .expect("ledger")
            .run_status(&RunId::from_stored("run-1"))
            .expect("status")
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn an_orchestrator_never_signs_off_a_criterion_and_the_receipt_is_untouched() {
    let world = World::new("crit");
    let before = world.receipt();
    let output = world.decide(&[
        "--answer",
        "approve",
        "--criterion",
        "X",
        "--actor",
        "orchestrator",
    ]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("orchestrator"),
        "{}",
        stderr(&output)
    );
    assert_eq!(world.receipt(), before);
    assert_eq!(world.state(), Some(State::NeedsDecision));
}

#[test]
fn an_old_spelling_is_refused_naming_its_replacement() {
    let world = World::new("old");
    let output = world.decide(&["--answer", "revise", "--actor", "claude"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("`claude` is now `orchestrator`"),
        "{}",
        stderr(&output)
    );
    assert_eq!(world.state(), Some(State::NeedsDecision));
}

#[test]
fn an_unknown_actor_lists_the_three_values() {
    let world = World::new("unknown");
    let output = world.decide(&["--answer", "revise", "--actor", "bob"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let message = stderr(&output);
    for value in ["orchestrator", "person", "person-via-orchestrator"] {
        assert!(message.contains(value), "{message}");
    }
}

#[test]
fn the_relais_actor_cannot_be_claimed_from_the_command_line() {
    let world = World::new("relais-actor");
    let output = world.decide(&["--answer", "revise", "--actor", "relais"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("unknown actor `relais`"),
        "{}",
        stderr(&output)
    );
    assert_eq!(world.state(), Some(State::NeedsDecision));
}

#[test]
fn the_person_needs_a_terminal_and_a_proxy_needs_a_quote() {
    let world = World::new("tty");
    let person = world.decide(&["--answer", "revise", "--actor", "person"]);
    assert_eq!(person.status.code(), Some(2), "{person:?}");
    assert!(stderr(&person).contains("terminal"), "{}", stderr(&person));
    let proxy = world.decide(&["--answer", "revise", "--actor", "person-via-orchestrator"]);
    assert_eq!(proxy.status.code(), Some(2), "{proxy:?}");
    assert!(
        stderr(&proxy).contains("--person-quote"),
        "{}",
        stderr(&proxy)
    );
    let blank = world.decide(&[
        "--answer",
        "revise",
        "--actor",
        "person-via-orchestrator",
        "--person-quote",
        "  ",
    ]);
    assert_eq!(blank.status.code(), Some(2), "{blank:?}");
    assert_eq!(world.state(), Some(State::NeedsDecision));
}

#[test]
fn a_proxy_answer_without_a_plugin_proof_cannot_sign_off() {
    let world = World::new("proxy-crit");
    let output = world.decide(&[
        "--answer",
        "approve",
        "--criterion",
        "X",
        "--actor",
        "person-via-orchestrator",
        "--person-quote",
        "yes",
    ]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(world.state(), Some(State::NeedsDecision));
}

#[test]
fn every_actor_but_the_terminal_one_round_trips_and_json_has_the_documented_shape() {
    let permit = std::sync::Arc::new(common::WorldPermit::acquire(2));
    let world = World::with_permit("json", permit.clone());
    let output = world.decide(&[
        "--answer",
        "revise",
        "--actor",
        "person-via-orchestrator",
        "--person-quote",
        "go ahead",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("one JSON object");
    assert_eq!(json["run"], "run-1");
    assert_eq!(json["resolution"], "decision_revised");
    assert_eq!(json["actor"], "person-via-orchestrator");
    assert_eq!(json["from_state"], "needs_decision");
    assert_eq!(json["to_state"], "cancelled");
    assert!(
        json.get("advice").is_some() && json["advice"].is_null(),
        "{json}"
    );
    let ledger = Ledger::open(&world.state.join("ledger.sqlite")).expect("ledger");
    let row = ledger
        .decision_of_run(&RunId::from_stored("run-1"))
        .expect("read")
        .expect("row");
    assert_eq!(row.actor.as_deref(), Some("person-via-orchestrator"));
    // Orchestrator round-trips on a second run.
    drop(ledger);
    let second = World::with_permit("json2", permit.clone());
    let output = second.decide(&["--answer", "abandon", "--actor", "orchestrator", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(json["actor"], "orchestrator");
}

#[test]
fn a_refusal_under_json_prints_error_and_the_matching_exit() {
    let world = World::new("json-refuse");
    let output = world.decide(&["--answer", "revise", "--actor", "claude", "--json"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("one JSON object");
    assert_eq!(json["exit"], 2);
    assert!(
        json["error"]
            .as_str()
            .is_some_and(|e| e.contains("orchestrator")),
        "{json}"
    );
    let unknown = common::relais(BIN)
        .args([
            "decide",
            "run-nope",
            "--answer",
            "revise",
            "--actor",
            "orchestrator",
            "--json",
        ])
        .current_dir(&world.root)
        .env("RELAIS_STATE_DIR", &world.state)
        .env("RELAIS_CONFIG_DIR", world.root.join("cfg"))
        .env("HOME", &world.root)
        .env("USERPROFILE", &world.root)
        .stdin(Stdio::null())
        .output()
        .expect("runs");
    assert_eq!(unknown.status.code(), Some(10), "{unknown:?}");
    let json: serde_json::Value = serde_json::from_slice(&unknown.stdout).expect("json");
    assert_eq!(json["exit"], 10);
}

impl World {
    /// `decide` with `--proof-stdin` and `proof` written on its stdin.
    fn decide_proved(&self, proof: &str, args: &[&str]) -> Output {
        use std::io::Write;
        let mut child = common::relais(BIN)
            .arg("decide")
            .arg("run-1")
            .args(args)
            .arg("--proof-stdin")
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
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(proof.as_bytes())
            .expect("write proof");
        child.wait_with_output().expect("relais ends")
    }

    fn mint(&self, session: &str) -> String {
        Ledger::open(&self.state.join("ledger.sqlite"))
            .expect("ledger")
            .mint_decide_nonce(session)
            .expect("mint")
    }

    fn recorded_via(&self) -> String {
        rusqlite::Connection::open(self.state.join("ledger.sqlite"))
            .expect("open")
            .query_row(
                "SELECT recorded_via FROM decisions WHERE run = 'run-1'",
                [],
                |row| row.get(0),
            )
            .expect("row")
    }
}

#[test]
fn a_live_plugin_nonce_records_plugin_and_the_ledger_never_holds_it() {
    let world = World::new("nonce-ok");
    let nonce = world.mint("s-1");
    let proof = serde_json::json!({"session": "s-1", "nonce": nonce}).to_string();
    let output = world.decide_proved(
        &proof,
        &[
            "--answer",
            "revise",
            "--actor",
            "person-via-orchestrator",
            "--person-quote",
            "revise it, the scope is too wide",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(world.recorded_via(), "plugin");
    let bytes = std::fs::read(world.state.join("ledger.sqlite")).expect("ledger bytes");
    let wal = std::fs::read(world.state.join("ledger.sqlite-wal")).unwrap_or_default();
    for file in [&bytes, &wal] {
        assert!(
            !file.windows(nonce.len()).any(|w| w == nonce.as_bytes()),
            "the plaintext nonce is never stored"
        );
    }
}

#[test]
fn a_wrong_or_ended_nonce_records_cli() {
    for (tag, end) in [("nonce-wrong", false), ("nonce-ended", true)] {
        let world = World::new(tag);
        let nonce = world.mint("s-1");
        let sent = if end {
            assert!(Ledger::open(&world.state.join("ledger.sqlite"))
                .expect("ledger")
                .end_decide_nonce("s-1")
                .expect("end"));
            nonce
        } else {
            "0".repeat(64)
        };
        let proof = serde_json::json!({"session": "s-1", "nonce": sent}).to_string();
        let output =
            world.decide_proved(&proof, &["--answer", "revise", "--actor", "orchestrator"]);
        assert_eq!(output.status.code(), Some(0), "{tag}: {}", stderr(&output));
        assert_eq!(world.recorded_via(), "cli", "{tag}");
    }
}

#[test]
fn a_stale_nonce_cannot_sign_off_and_the_receipt_is_untouched() {
    let world = World::new("nonce-crit");
    world.mint("s-1");
    let before = world.receipt();
    let proof = serde_json::json!({"session": "s-1", "nonce": "f".repeat(64)}).to_string();
    let output = world.decide_proved(
        &proof,
        &[
            "--answer",
            "approve",
            "--criterion",
            "X",
            "--actor",
            "person-via-orchestrator",
            "--person-quote",
            "yes, approve it",
        ],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("plugin's proof"),
        "{}",
        stderr(&output)
    );
    assert_eq!(world.receipt(), before);
    assert_eq!(world.state(), Some(State::NeedsDecision));
}

#[test]
fn a_malformed_proof_is_refused_not_read_as_cli() {
    let world = World::new("nonce-bad");
    let output = world.decide_proved(
        "not json",
        &["--answer", "revise", "--actor", "orchestrator"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert_eq!(world.state(), Some(State::NeedsDecision));
}
