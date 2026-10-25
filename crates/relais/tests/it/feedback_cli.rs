//! `relais feedback` through the real binary (orchestrator-decides plan,
//! 4c): a typed `--actor`, the `--json` shapes, and the plugin's proof on
//! stdin deciding whether an outcome's source is `orchestrator` or `cli`.

use crate::common;

use std::path::PathBuf;
use std::process::{Output, Stdio};

use relais::contract::TaskContract;
use relais::ids::{RunId, TaskId};
use relais::ledger::{Ledger, Transition};
use relais::lifecycle::{Reason, State, UsagePhase};
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
    /// A world with one accepted run, `run-1`, of task `task-1`.
    fn new(tag: &str) -> Self {
        Self::with_permit(tag, std::sync::Arc::new(common::WorldPermit::acquire(1)))
    }

    /// Builds a world under a permit the caller already holds, so a test
    /// with several worlds takes one `acquire(n)` for all of them.
    fn with_permit(tag: &str, permit: std::sync::Arc<common::WorldPermit>) -> Self {
        let scratch = short_temp_dir(&format!("fb-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        let world = Self {
            _permit: permit,
            _scratch: scratch,
            state,
            root,
        };
        let ledger = world.ledger();
        let id = RunId::from_stored("run-1");
        ledger
            .insert_run(&id, "/repo", None, &TaskId::from_stored("task-1"), "rk")
            .expect("run");
        let contract = TaskContract::from_json_str(
            r#"{"schema_version":1,"kind":"change","objective":"Fix the escaping",
                "base_ref":"HEAD","write_scope":["crates/**"],"acceptance":["parses"],
                "verification_profile":"p"}"#,
        )
        .expect("contract");
        let json = serde_json::to_string(&contract.canonical_value()).expect("json");
        let revision = ledger
            .insert_contract_revision(&id, &contract.hash(), &json, "HEAD", None)
            .expect("revision");
        ledger
            .insert_attempt(&id, revision, 1, "implementation", UsagePhase::Initial)
            .expect("attempt");
        for (from, to, reason) in [
            (State::Prepared, State::Running, Reason::WorkerDispatched),
            (
                State::Verifying,
                State::Accepted,
                Reason::ChecksAndReviewPassed,
            ),
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
        drop(ledger);
        world
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.state.join("ledger.sqlite")).expect("ledger")
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut command = common::relais(BIN);
        command
            .arg("feedback")
            .args(args)
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root);
        command
    }

    fn feedback(&self, args: &[&str]) -> Output {
        self.command(args)
            .stdin(Stdio::null())
            .output()
            .expect("relais runs")
    }

    /// `feedback` with `--proof-stdin` and `proof` written on its stdin.
    fn feedback_proved(&self, proof: &str, args: &[&str]) -> Output {
        use std::io::Write;
        let mut child = self
            .command(args)
            .arg("--proof-stdin")
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

    /// The `source` and `actor` the newest outcome row was stored with.
    fn stored(&self) -> (Option<String>, String) {
        let stored = self
            .ledger()
            .latest_outcome(&TaskId::from_stored("task-1"))
            .expect("read")
            .expect("an outcome");
        (stored.outcome.detail.source, stored.outcome.detail.actor)
    }
}

fn json_of(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("one JSON object on stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn json_prints_the_success_shape() {
    let world = World::new("json");
    let output = world.feedback(&[
        "run-1",
        "--outcome",
        "reverted",
        "--actor",
        "orchestrator",
        "--evidence",
        "abc123",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json = json_of(&output);
    assert_eq!(json["run"], "run-1");
    assert_eq!(json["task"], "task-1");
    assert_eq!(json["outcome"], "reverted");
    assert_eq!(json["actor"], "orchestrator");
    assert_eq!(json["source"], "cli", "{json}");
    assert_eq!(world.stored(), (Some("cli".into()), "orchestrator".into()));
}

#[test]
fn json_prints_error_and_the_matching_exit_on_a_refusal() {
    let world = World::new("json-refuse");
    let unknown = world.feedback(&[
        "run-nope",
        "--outcome",
        "reverted",
        "--actor",
        "orchestrator",
        "--json",
    ]);
    assert_eq!(unknown.status.code(), Some(10), "{unknown:?}");
    let json = json_of(&unknown);
    assert_eq!(json["exit"], 10, "{json}");
    assert!(json["error"].as_str().is_some(), "{json}");

    let missing_magnitude = world.feedback(&[
        "run-1",
        "--outcome",
        "corrected",
        "--actor",
        "orchestrator",
        "--json",
    ]);
    assert_eq!(missing_magnitude.status.code(), Some(2));
    let json = json_of(&missing_magnitude);
    assert_eq!(json["exit"], 2, "{json}");
    assert!(
        json["error"]
            .as_str()
            .is_some_and(|e| e.contains("magnitude")),
        "{json}"
    );
    assert!(world
        .ledger()
        .latest_outcome(&TaskId::from_stored("task-1"))
        .expect("read")
        .is_none());
}

#[test]
fn an_old_actor_spelling_exits_2_naming_the_replacement() {
    let world = World::new("old-actor");
    let output = world.feedback(&["run-1", "--outcome", "reverted", "--actor", "claude"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        stderr(&output).contains("`claude` is now `orchestrator`"),
        "{}",
        stderr(&output)
    );
    let json = json_of(&world.feedback(&[
        "run-1",
        "--outcome",
        "reverted",
        "--actor",
        "a reviewer",
        "--json",
    ]));
    assert_eq!(json["exit"], 2, "{json}");
    for value in ["orchestrator", "person", "person-via-orchestrator"] {
        assert!(json["error"].as_str().unwrap().contains(value), "{json}");
    }
}

#[test]
fn a_live_plugin_nonce_stores_orchestrator_and_without_one_cli() {
    let world = World::new("nonce");
    let nonce = world.ledger().mint_decide_nonce("s-1").expect("mint");
    let proof = serde_json::json!({"session": "s-1", "nonce": nonce}).to_string();
    let proved = world.feedback_proved(
        &proof,
        &[
            "run-1",
            "--outcome",
            "reverted",
            "--actor",
            "orchestrator",
            "--json",
        ],
    );
    assert_eq!(proved.status.code(), Some(0), "{}", stderr(&proved));
    assert_eq!(json_of(&proved)["source"], "orchestrator");
    assert_eq!(
        world.stored(),
        (Some("orchestrator".into()), "orchestrator".into())
    );

    let wrong = serde_json::json!({"session": "s-1", "nonce": "0".repeat(64)}).to_string();
    let unproved = world.feedback_proved(
        &wrong,
        &[
            "run-1",
            "--outcome",
            "reverted",
            "--actor",
            "orchestrator",
            "--json",
        ],
    );
    assert_eq!(unproved.status.code(), Some(0), "{}", stderr(&unproved));
    assert_eq!(json_of(&unproved)["source"], "cli");
    assert_eq!(world.stored().0.as_deref(), Some("cli"));

    let bare = world.feedback(&["run-1", "--outcome", "reverted", "--actor", "orchestrator"]);
    assert_eq!(bare.status.code(), Some(0), "{}", stderr(&bare));
    assert_eq!(world.stored().0.as_deref(), Some("cli"));
}

#[test]
fn a_malformed_proof_is_refused_not_read_as_cli() {
    let world = World::new("bad-proof");
    let output = world.feedback_proved(
        "not json",
        &["run-1", "--outcome", "reverted", "--actor", "orchestrator"],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(world
        .ledger()
        .latest_outcome(&TaskId::from_stored("task-1"))
        .expect("read")
        .is_none());
}
