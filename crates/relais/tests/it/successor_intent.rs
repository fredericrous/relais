//! `relais decide --answer revise --widen-scope` through the real binary
//! (runs-survive-a-loaded-machine plan, A5): the answer and one successor
//! intent are one write, `--json` returns the intent, and the same decide
//! repeated returns the same intent. The claim and the launch are the
//! plugin's; their atomicity is the ledger's (`ledger::successor` tests).

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use relais::ids::{RunId, TaskId};
use relais::ledger::Ledger;
use relais::ledger::Transition;
use relais::lifecycle::{Reason, State, UsagePhase};
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

fn git(dir: &Path, args: &[&str]) -> String {
    // The machine's own commit hooks are not under test in a scratch repo.
    let output = std::process::Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
    /// Released only after the scratch directory is gone, so the cap bounds
    /// live worlds, not finished ones.
    _permit: std::sync::Arc<common::WorldPermit>,
}

impl World {
    /// `run-1`: a run in `needs_decision (scope_exceeded)` whose candidate
    /// changed `src/a.rs`, outside its `docs/**` scope.
    fn new(tag: &str) -> Self {
        Self::with_permit(tag, std::sync::Arc::new(common::WorldPermit::acquire(1)))
    }

    /// Builds a world under a permit the caller already holds, so a test
    /// with several worlds takes one `acquire(n)` for all of them.
    fn with_permit(tag: &str, permit: std::sync::Arc<common::WorldPermit>) -> Self {
        let scratch = short_temp_dir(&format!("si-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        let repo = root.join("repo");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::create_dir_all(repo.join("src")).expect("repo");
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("src/a.rs"), "a\n").expect("write");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("src/a.rs"), "a\nb\n").expect("write");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "candidate"]);
        let candidate = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["reset", "-q", "--hard", &base]);

        // The bytes relais itself exports for a candidate, so the digest
        // it derives at resolution matches the files.
        let patch =
            relais::workspace::derive_patch(&repo, &base, &candidate).expect("derive the patch");
        let run_dir = state.join("runs").join("run-1");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(run_dir.join("candidate-1.patch"), &patch).expect("patch");
        std::fs::write(run_dir.join("candidate-latest.patch"), &patch).expect("patch");

        let contract = serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Document the thing",
            "base_ref": "HEAD",
            "write_scope": ["docs/**"],
            "acceptance": ["it is documented"],
            "verification_profile": "profile",
        });
        let ledger = Ledger::open(&state.join("ledger.sqlite")).expect("ledger");
        let id = RunId::from_stored("run-1");
        ledger
            .insert_typed_run(
                &id,
                &repo.to_string_lossy(),
                None,
                &TaskId::from_stored("task-1"),
                "rk",
                None,
            )
            .expect("run");
        let revision = ledger
            .insert_contract_revision(
                &id,
                "contract-hash",
                &contract.to_string(),
                "HEAD",
                Some(&base),
            )
            .expect("revision");
        let attempt = ledger
            .insert_attempt(&id, revision, 1, "implementation", UsagePhase::Initial)
            .expect("attempt");
        ledger
            .finish_attempt(attempt, State::NeedsDecision, None, Some(&candidate))
            .expect("attempt ends");
        for (from, to, reason, detail) in [
            (
                State::Prepared,
                State::Running,
                Reason::WorkerDispatched,
                None,
            ),
            (
                State::Running,
                State::NeedsDecision,
                Reason::ScopeExceeded,
                Some(serde_json::json!({ "paths": ["src/a.rs"] })),
            ),
        ] {
            ledger
                .record_transition(&Transition {
                    run_id: id.clone(),
                    attempt_id: None,
                    from_state: Some(from),
                    to_state: to,
                    reason: reason.as_str().into(),
                    detail,
                    at: ledger.now(),
                })
                .expect("transition");
        }
        Self {
            _permit: permit,
            _scratch: scratch,
            root,
            state,
        }
    }

    fn relais(&self, args: &[&str]) -> Output {
        common::relais(BIN)
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

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.state.join("ledger.sqlite")).expect("ledger")
    }
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is JSON: {e}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn widen_scope_writes_the_answer_and_one_intent_and_a_repeat_returns_the_same() {
    let world = World::new("widen");
    let args = [
        "decide",
        "run-1",
        "--answer",
        "revise",
        "--widen-scope",
        "--actor",
        "orchestrator",
        "--json",
    ];
    let first = world.relais(&args);
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let answered = json(&first);
    let intent = &answered["successor_intent"];
    assert_eq!(answered["resolution"], "decision_revised");
    assert_eq!(intent["state"], "pending");
    assert_eq!(intent["decision_id"], "run-1");
    assert_eq!(intent["scope_delta"], serde_json::json!(["src/a.rs"]));
    assert!(
        intent["repo"]
            .as_str()
            .is_some_and(|repo| repo.ends_with("repo")),
        "the plugin is told where to launch it: {intent}"
    );
    let intent_id = intent["intent_id"].as_str().expect("an id").to_string();

    // One decision answer and one intent row.
    let ledger = world.ledger();
    let stored = ledger
        .successor_intent(&intent_id)
        .expect("read")
        .expect("the row exists");
    assert_eq!(stored.state, relais::ledger::IntentState::Pending);
    assert_eq!(
        ledger
            .decision_of_run(&RunId::from_stored("run-1"))
            .expect("read")
            .expect("row")
            .resolution,
        Some(Reason::DecisionRevised)
    );
    // The widened contract: a verify of this run's candidate, the old scope
    // plus exactly the out-of-scope path.
    let contract: serde_json::Value = serde_json::from_str(&stored.contract_json).expect("json");
    assert_eq!(contract["kind"], "verify");
    assert_eq!(contract["candidate"]["run"], "run-1");
    assert_eq!(
        contract["write_scope"],
        serde_json::json!(["docs/**", "src/a.rs"])
    );
    assert_eq!(
        stored
            .candidate
            .as_ref()
            .map(|c| (c.source_run.as_str(), c.index)),
        Some(("run-1", 1)),
        "the candidate is pinned with the answer"
    );
    assert!(ledger
        .pinned_candidates()
        .expect("pins")
        .contains(&("run-1".to_string(), 1)));

    // Repeating the decide returns the same intent and writes no second.
    let again = world.relais(&args);
    assert_eq!(again.status.code(), Some(0), "{again:?}");
    assert_eq!(json(&again)["successor_intent"]["intent_id"], intent_id);
    assert_eq!(rusqlite_count(&world), 1);

    // `native status --session` lists the pending intent for the plugin.
    let status = world.relais(&["native", "status", "--session", "any"]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    let listed = json(&status);
    assert_eq!(listed["intents"][0]["intent_id"], intent_id);
    assert_eq!(listed["intents"][0]["state"], "pending");
    // ... and so do `status` and `resume`, which only list.
    let text = String::from_utf8_lossy(&world.relais(&["status", "run-1"]).stdout).into_owned();
    assert!(text.contains(&intent_id), "{text}");
    let resumed = String::from_utf8_lossy(&world.relais(&["resume", "run-1"]).stdout).into_owned();
    assert!(resumed.contains(&intent_id), "{resumed}");
}

fn rusqlite_count(world: &World) -> u64 {
    world
        .ledger()
        .count_successor_intents()
        .expect("count the outbox rows")
}

#[test]
fn widen_scope_refuses_a_decision_that_is_not_a_scope_one_and_leaves_it_open() {
    let world = World::new("refuse");
    // Not `revise`.
    let wrong = world.relais(&[
        "decide",
        "run-1",
        "--answer",
        "approve",
        "--widen-scope",
        "--actor",
        "orchestrator",
    ]);
    assert_eq!(wrong.status.code(), Some(2), "{wrong:?}");
    assert_eq!(
        world
            .ledger()
            .run_status(&RunId::from_stored("run-1"))
            .expect("status"),
        Some(State::NeedsDecision)
    );
    assert_eq!(rusqlite_count(&world), 0);
}
