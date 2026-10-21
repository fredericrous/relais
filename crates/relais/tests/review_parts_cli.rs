//! `relais decide --answer review-parts` through the real binary (plan
//! review-over-budget, Changes step 5): a patch that can be cut into parts
//! records one successor intent `review: parts`; a patch that cannot (a
//! single file over the review budget, or more than 8 parts) is refused by
//! name before any intent is written.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use relais::ids::{RunId, TaskId};
use relais::ledger::{IntentReview, Ledger, Transition};
use relais::lifecycle::{Reason, State, UsagePhase};
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

fn git(dir: &Path, args: &[&str]) -> String {
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
}

impl World {
    /// `run-1`: a run in `needs_decision (review_over_budget)` whose
    /// candidate adds `files` (name, bytes) under `src/`.
    fn new(tag: &str, files: &[(&str, usize)]) -> Self {
        let scratch = short_temp_dir(&format!("rp-{tag}"));
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
        for (name, bytes) in files {
            let body: String = (0..bytes / 20).map(|n| format!("line {n:013}\n")).collect();
            std::fs::write(repo.join("src").join(name), body).expect("write");
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "candidate"]);
        let candidate = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["reset", "-q", "--hard", &base]);

        let patch =
            relais::workspace::derive_patch(&repo, &base, &candidate).expect("derive the patch");
        let run_dir = state.join("runs").join("run-1");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        std::fs::write(run_dir.join("candidate-1.patch"), &patch).expect("patch");
        std::fs::write(run_dir.join("candidate-latest.patch"), &patch).expect("patch");

        let contract = serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Add the files",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "acceptance": ["they exist"],
            "verification_profile": "profile",
            "review": "required",
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
        for (from, to, reason) in [
            (State::Prepared, State::Running, Reason::WorkerDispatched),
            (
                State::Running,
                State::NeedsDecision,
                Reason::ReviewOverBudget,
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
        Self {
            _scratch: scratch,
            root,
            state,
        }
    }

    fn decide(&self, answer: &str) -> Output {
        common::relais(BIN)
            .args([
                "decide",
                "run-1",
                "--answer",
                answer,
                "--actor",
                "orchestrator",
                "--json",
            ])
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

/// Refused by name: exit 2, no intent, the decision still open.
fn assert_refused(world: &World, why: &str) {
    let refused = world.decide("review-parts");
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    let error = json(&refused)["error"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(error.contains(why), "{error}");
    assert!(error.contains("cannot be reviewed in parts"), "{error}");
    let ledger = world.ledger();
    assert_eq!(ledger.count_successor_intents().expect("count"), 0);
    let decision = ledger
        .decision_of_run(&RunId::from_stored("run-1"))
        .expect("read")
        .expect("a decision");
    assert!(decision.resolved_at.is_none(), "the decision stays open");
}

#[test]
fn a_patch_that_cuts_into_parts_records_one_intent_with_review_parts() {
    // Two files of 150 KB: over the 192 KiB budget, two parts.
    let world = World::new("cut", &[("big0.txt", 150_000), ("big1.txt", 150_000)]);
    let first = world.decide("review-parts");
    assert_eq!(first.status.code(), Some(0), "{first:?}");
    let answered = json(&first);
    assert_eq!(answered["resolution"], "decision_revised");
    let intent = &answered["successor_intent"];
    assert_eq!(intent["state"], "pending");
    assert_eq!(intent["review"], "parts");
    let intent_id = intent["intent_id"].as_str().expect("an id").to_string();

    let stored = world
        .ledger()
        .successor_intent(&intent_id)
        .expect("read")
        .expect("the row exists");
    assert_eq!(stored.review, Some(IntentReview::Parts));
    let contract: serde_json::Value = serde_json::from_str(&stored.contract_json).expect("json");
    assert_eq!(contract["kind"], "verify");
    assert_eq!(contract["candidate"]["run"], "run-1");
    assert!(stored.scope_delta.is_empty());

    // The answer judged nothing, so the learning labels leave it out.
    let labelled = relais::learn::decision_label::label_decisions(&world.ledger()).expect("labels");
    let label = &labelled
        .iter()
        .find(|decision| decision.run.as_str() == "run-1")
        .expect("the decision is labelled")
        .label;
    assert!(
        matches!(
            label,
            relais::learn::decision_label::DecisionLabel::Excluded(_)
        ),
        "{label:?}"
    );

    // A repeat returns the same intent and writes no second.
    let again = world.decide("review-parts");
    assert_eq!(again.status.code(), Some(0), "{again:?}");
    assert_eq!(json(&again)["successor_intent"]["intent_id"], intent_id);
    assert_eq!(world.ledger().count_successor_intents().expect("count"), 1);
}

#[test]
fn a_patch_within_the_budget_is_one_part_a_retry_of_the_review() {
    let world = World::new("one", &[("small.txt", 2_000)]);
    let answered = json(&world.decide("review-parts"));
    assert_eq!(answered["successor_intent"]["review"], "parts");
}

#[test]
fn a_single_file_over_the_budget_is_refused_before_any_intent() {
    let world = World::new("file", &[("huge.txt", 250_000)]);
    assert_refused(&world, "src/huge.txt");
}

#[test]
fn more_than_eight_parts_is_refused_before_any_intent() {
    // Nine files of 120 KB: no two fit one part, so nine parts.
    let names: Vec<String> = (0..9).map(|n| format!("f{n}.txt")).collect();
    let files: Vec<(&str, usize)> = names.iter().map(|n| (n.as_str(), 120_000)).collect();
    let world = World::new("many", &files);
    assert_refused(&world, "9 parts");
}
