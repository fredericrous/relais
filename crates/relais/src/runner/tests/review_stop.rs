// -- a review that does not fit, or cannot be had, asks a person --------
//
// docs/plans/2026-10-20-review-over-budget.md, Changes step 4: the run
// ends `needs_decision (review_over_budget)` without dispatching a
// reviewer, saves a verification snapshot, and `approve` seals a receipt
// from it.

use std::sync::atomic::AtomicBool;

use crate::backend::{BackendError, Harness};
use crate::ledger::{Actor, DecisionAnswer, EvidenceKind, RecordedVia};
use crate::runner::approval::{approve_from_snapshot, ApprovalError};
use crate::verify::{ReviewNotRun, ReviewRecord};

use super::*;

/// A worker that removes `src/main.rs` and adds `files` text files of
/// `lines` lines each, a reviewer that records it was called.
fn big_patch_backend(files: usize, lines: usize, reviewed: Arc<AtomicBool>) -> MockBackend {
    MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            reviewed.store(true, Ordering::SeqCst);
            return MockOutcome {
                result_text: Some("FINDINGS: none".into()),
                exit_code: Some(0),
                usage: Some(usage(10)),
                ..Default::default()
            };
        }
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("fix");
        for file in 0..files {
            let body: String = (0..lines)
                .map(|line| format!("line {line:05} of file {file} {}\n", "x".repeat(50)))
                .collect();
            std::fs::write(spec.work_dir.join(format!("src/big{file}.txt")), body).expect("write");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

/// A run whose candidate patch is larger than the review budget: two files
/// of about 150 KB each.
fn over_budget_run(fixture: &Fixture, reviewed: Arc<AtomicBool>) -> (RunId, String) {
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let backend = big_patch_backend(2, 2300, reviewed);
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let RunOutcome {
        run_id,
        terminal: Terminal::NeedsDecision { reason, detail },
    } = outcome
    else {
        panic!("an over-budget patch is needs_decision, got {outcome:?}");
    };
    assert_eq!(reason, Reason::ReviewOverBudget);
    (run_id, detail)
}

/// A backend whose reviewer dispatches are refused by the coordinator.
struct Refusing(MockBackend);

impl Harness for Refusing {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn probe(&self) -> Option<crate::backend::Capabilities> {
        self.0.probe()
    }
}

impl Backend for Refusing {
    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let mut result = self.0.launch(spec)?;
        if spec.agent == AgentKind::Reviewer {
            result.ended = Ended::Exited(1);
            result.result_text = None;
            result.failure_detail = Some("the coordinator refused the dispatch".into());
        }
        Ok(result)
    }
}

/// A run whose reviewer could not be launched: `review_unavailable`.
fn unavailable_run(fixture: &Fixture) -> RunId {
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let backend = Refusing(big_patch_backend(0, 0, Arc::new(AtomicBool::new(false))));
    let RunOutcome {
        run_id,
        terminal: Terminal::NeedsReview { .. },
    } = fixture.execute(&fixture.contract(Review::Required), &repo, &backend)
    else {
        panic!("an unlaunched reviewer is needs_review");
    };
    run_id
}

fn answer(state: State) -> DecisionAnswer<'static> {
    DecisionAnswer {
        resolution: Reason::DecisionApproved,
        actor: Actor::Orchestrator,
        person_quote: None,
        recorded_via: RecordedVia::Cli,
        note: None,
        successor_run: None,
        from_state: state,
        to_state: State::Accepted,
    }
}

fn reopened(fixture: &Fixture) -> Ledger {
    Ledger::open(&fixture.dir.join("ledger.sqlite")).expect("reopen the ledger")
}

fn receipts_in_evidence(ledger: &Ledger, run: &RunId) -> usize {
    ledger
        .evidence(run)
        .expect("evidence")
        .iter()
        .filter(|row| row.kind == EvidenceKind::Receipt)
        .count()
}

#[test]
fn a_patch_over_the_budget_dispatches_no_reviewer_and_asks() {
    let fixture = Fixture::new();
    let reviewed = Arc::new(AtomicBool::new(false));
    let (run, detail) = over_budget_run(&fixture, reviewed.clone());
    assert!(
        !reviewed.load(Ordering::SeqCst),
        "no reviewer was dispatched"
    );
    assert!(detail.contains("bytes against the 196608-byte"), "{detail}");
    assert!(
        detail.contains("a review in parts would take 2"),
        "{detail}"
    );
    assert_eq!(
        fixture.ledger.run_status(&run).expect("status"),
        Some(State::NeedsDecision)
    );
    let snapshot = fixture
        .ledger
        .verification_snapshot(&run)
        .expect("read")
        .expect("the snapshot was saved before the run stopped");
    assert_eq!(snapshot.stop_reason, "review_over_budget");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_single_file_over_the_budget_says_so() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let backend = big_patch_backend(1, 4000, Arc::new(AtomicBool::new(false)));
    let RunOutcome {
        terminal: Terminal::NeedsDecision { reason, detail },
        ..
    } = fixture.execute(&fixture.contract(Review::Required), &repo, &backend)
    else {
        panic!("needs_decision");
    };
    assert_eq!(reason, Reason::ReviewOverBudget);
    assert!(
        detail.contains("a single file exceeds the budget"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn parts_are_cut_at_file_boundaries_and_each_fits() {
    use crate::runner::review::review_part_count;
    let file =
        |name: &str, bytes: usize| format!("diff --git a/{name} b/{name}\n{}\n", "y".repeat(bytes));
    let patch = [file("a", 60), file("b", 60), file("c", 60)].concat();
    let one = file("a", 60).len();
    assert_eq!(review_part_count(&patch, one * 3), Some(1));
    assert_eq!(review_part_count(&patch, one * 2), Some(2));
    assert_eq!(review_part_count(&patch, one), Some(3));
    assert_eq!(
        review_part_count(&patch, one - 1),
        None,
        "a file larger than a part"
    );
}

#[test]
fn approving_each_review_stop_seals_a_receipt_from_its_snapshot() {
    let fixture = Fixture::new();
    let (over, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    let unavailable = unavailable_run(&fixture);
    for (run, state) in [
        (&over, State::NeedsDecision),
        (&unavailable, State::NeedsReview),
    ] {
        let sealed = approve_from_snapshot(
            &fixture.ledger,
            &fixture.artifacts,
            run,
            &answer(state),
            &|| Ok(()),
        )
        .expect("approved");
        assert!(sealed.written);
        let body = std::fs::read(&sealed.artifact).expect("the artifact");
        // Reopening the ledger reads the sealed receipt back.
        let ledger = reopened(&fixture);
        let (row, hash) = ledger.receipt(run).expect("read").expect("a receipt row");
        assert_eq!(row["outcome"], "accepted");
        let receipt: crate::verify::Receipt = serde_json::from_value(row).expect("parses");
        assert_eq!(hash, receipt.hash(), "the row stores Receipt::hash()");
        assert_eq!(hash, sealed.receipt_hash);
        // The content address stays the artifact's sha256 on the evidence row.
        assert!(ledger
            .evidence(run)
            .expect("evidence")
            .iter()
            .any(|row| row.kind == EvidenceKind::Receipt
                && row.sha256.as_deref() == Some(crate::ids::sha256_hex(&body).as_str())));
        // Beside the run's other artifacts, with the same bytes.
        assert_eq!(
            std::fs::read(fixture.artifacts.join(run.as_str()).join("receipt.json"))
                .expect("receipt.json"),
            body
        );
        assert_eq!(receipt.run_id, run.as_str());
        if run == &over {
            assert!(
                matches!(
                    receipt.review,
                    ReviewRecord::NotRun(ReviewNotRun::OverBudget { patch_bytes, budget_bytes })
                        if patch_bytes > budget_bytes && budget_bytes == 196_608
                ),
                "{:?}",
                receipt.review
            );
        } else {
            assert!(
                matches!(
                    &receipt.review,
                    ReviewRecord::Unavailable { detail }
                        if detail.contains("the coordinator refused the dispatch")
                ),
                "{:?}",
                receipt.review
            );
        }
        assert_eq!(receipts_in_evidence(&ledger, run), 1);
        assert!(ledger
            .decision_of_run(run)
            .expect("decision")
            .expect("a decision")
            .resolved_at
            .is_some());
    }
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_failure_after_the_artifact_is_recovered_by_the_retry() {
    let fixture = Fixture::new();
    let (run, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    let state = State::NeedsDecision;
    let failed = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run,
        &answer(state),
        &|| Err("injected".into()),
    );
    assert!(
        matches!(failed, Err(ApprovalError::Interrupted(_))),
        "{failed:?}"
    );
    // The artifact stands, no row points at it, the decision is open.
    let ledger = reopened(&fixture);
    assert_eq!(ledger.receipt(&run).expect("read"), None);
    assert_eq!(receipts_in_evidence(&ledger, &run), 0);
    assert!(ledger
        .decision_of_run(&run)
        .expect("decision")
        .expect("a decision")
        .resolved_at
        .is_none());
    let dir = fixture.artifacts.join(run.as_str()).join("receipts");
    assert_eq!(std::fs::read_dir(&dir).expect("receipts dir").count(), 1);

    let sealed = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run,
        &answer(state),
        &|| Ok(()),
    )
    .expect("the retry seals");
    assert!(sealed.written);
    assert_eq!(std::fs::read_dir(&dir).expect("receipts dir").count(), 1);

    // A second approve is a no-op: nothing is written.
    let again = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run,
        &answer(state),
        &|| panic!("a resolved decision writes nothing"),
    )
    .expect("a repeat succeeds");
    assert!(!again.written);
    assert_eq!(again.receipt_hash, sealed.receipt_hash);
    let ledger = reopened(&fixture);
    assert_eq!(receipts_in_evidence(&ledger, &run), 1);
    let approvals = ledger
        .transitions(&run)
        .expect("transitions")
        .iter()
        .filter(|t| t.reason == Reason::DecisionApproved.as_str())
        .count();
    assert_eq!(approvals, 1, "exactly one resolution");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn missing_evidence_and_gaps_are_named_errors_and_seal_nothing() {
    let fixture = Fixture::new();
    let (run, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    // A gap on the snapshot still refuses.
    let mut snapshot = fixture
        .ledger
        .verification_snapshot(&run)
        .expect("read")
        .expect("saved");
    snapshot.receipt["verification"]["gaps"] =
        serde_json::json!(["declared-check: status `inert`"]);
    fixture
        .ledger
        .save_verification_snapshot(&run, &snapshot)
        .expect("save");
    let refused = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run,
        &answer(State::NeedsDecision),
        &|| Ok(()),
    );
    assert!(
        matches!(refused, Err(ApprovalError::Gaps { .. })),
        "{refused:?}"
    );
    // A snapshot not bound to its receipt is refused.
    snapshot.receipt["verification"]["gaps"] = serde_json::json!([]);
    snapshot.base_sha = "another-base".into();
    fixture
        .ledger
        .save_verification_snapshot(&run, &snapshot)
        .expect("save");
    let refused = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run,
        &answer(State::NeedsDecision),
        &|| Ok(()),
    );
    assert!(
        matches!(refused, Err(ApprovalError::Unbound { .. })),
        "{refused:?}"
    );
    // No snapshot at all.
    let task = crate::ids::TaskId::from_stored("t");
    let bare = RunId::from_stored("run-bare");
    fixture
        .ledger
        .insert_run(&bare, "/repo", None, &task, "rk")
        .expect("run");
    let refused = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &bare,
        &answer(State::NeedsDecision),
        &|| Ok(()),
    );
    assert!(
        matches!(refused, Err(ApprovalError::NoSnapshot { .. })),
        "{refused:?}"
    );
    assert_eq!(fixture.ledger.receipt(&run).expect("read"), None);
    assert_eq!(fixture.ledger.receipt(&bare).expect("read"), None);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn report_counts_runs_over_budget() {
    let fixture = Fixture::new();
    let (_run, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    let report =
        crate::report::runs_report(&fixture.ledger, "1970-01-01T00:00:00Z", None).expect("report");
    assert_eq!(report.review_over_budget, 1);
    assert!(report.render().contains("review over budget: 1 run(s)"));
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn an_inspection_whose_report_review_is_unavailable_is_approvable() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let (inner, _) = inspecting_backend("DONE: the report", "[1] met: read it");
    let backend = Refusing(inner);
    let RunOutcome {
        run_id,
        terminal: Terminal::NeedsReview { .. },
    } = fixture.execute(&inspect_contract(), &repo, &backend)
    else {
        panic!("an unavailable report review is needs_review");
    };
    let snapshot = fixture
        .ledger
        .verification_snapshot(&run_id)
        .expect("read")
        .expect("the report-review stop saved a snapshot");
    assert_eq!(snapshot.stop_reason, "review_unavailable");
    let sealed = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run_id,
        &answer(State::NeedsReview),
        &|| Ok(()),
    )
    .expect("approved");
    assert!(sealed.written);
    let (row, _) = fixture
        .ledger
        .receipt(&run_id)
        .expect("read")
        .expect("a receipt row");
    assert_eq!(row["outcome"], "accepted");
    let receipt: crate::verify::Receipt = serde_json::from_value(row).expect("parses");
    assert!(
        matches!(receipt.review, ReviewRecord::Unavailable { .. }),
        "{:?}",
        receipt.review
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn approval_refuses_a_snapshot_for_another_candidate_or_stop() {
    let fixture = Fixture::new();
    let (run, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    let good = fixture
        .ledger
        .verification_snapshot(&run)
        .expect("read")
        .expect("saved");
    let attempt = |snapshot: &crate::ledger::VerificationSnapshot| {
        fixture
            .ledger
            .save_verification_snapshot(&run, snapshot)
            .expect("save");
        let refused = approve_from_snapshot(
            &fixture.ledger,
            &fixture.artifacts,
            &run,
            &answer(State::NeedsDecision),
            &|| Ok(()),
        );
        assert!(
            matches!(refused, Err(ApprovalError::Stale { .. })),
            "{refused:?}"
        );
    };
    let mut other_candidate = good.clone();
    other_candidate.candidate_sha = "0".repeat(40);
    other_candidate.receipt["candidate_sha"] = serde_json::json!("0".repeat(40));
    attempt(&other_candidate);
    let mut other_stop = good.clone();
    other_stop.stop_reason = "review_unavailable".into();
    attempt(&other_stop);
    assert_eq!(fixture.ledger.receipt(&run).expect("read"), None);
    assert!(!fixture
        .artifacts
        .join(run.as_str())
        .join("receipts")
        .exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn the_over_budget_detail_reads_as_one_sentence() {
    use crate::runner::machine::OverBudget;
    let over = OverBudget {
        patch_bytes: 300_000,
        budget_bytes: 196_608,
        files: 2,
        parts: Some(2),
    };
    assert_eq!(
        over.describe(),
        "the candidate patch is 300000 bytes against the 196608-byte review budget, across 2 \
         files; a review in parts would take 2; no reviewer was dispatched"
    );
    let single = OverBudget {
        parts: None,
        ..over
    };
    assert_eq!(
        single.describe(),
        "the candidate patch is 300000 bytes against the 196608-byte review budget, across 2 \
         files; a single file exceeds the budget, so it cannot be reviewed in parts; no \
         reviewer was dispatched"
    );
}

#[test]
fn a_new_attempt_drops_the_runs_snapshot() {
    let fixture = Fixture::new();
    let (run, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    assert!(fixture
        .ledger
        .verification_snapshot(&run)
        .expect("read")
        .is_some());
    let revision = fixture
        .ledger
        .insert_contract_revision(&run, "h2", "{}", "HEAD", None)
        .expect("revision");
    fixture
        .ledger
        .insert_attempt(&run, revision, 9, "implementation", UsagePhase::Repair)
        .expect("attempt");
    assert!(fixture
        .ledger
        .verification_snapshot(&run)
        .expect("read")
        .is_none());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// A decomposed run whose assembled candidate's review is unavailable.
fn unavailable_decomposed_run(fixture: &Fixture) -> RunId {
    let repo = repo_policy_reviewing_only_the_root(fixture);
    let launches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend = Refusing(package_worker(launches));
    let contract = decomposed_contract(fixture, plan_json(false));
    let RunOutcome {
        run_id,
        terminal: Terminal::NeedsReview { .. },
    } = fixture.execute(&contract, &repo, &backend)
    else {
        panic!("an unavailable review of the assembled candidate is needs_review");
    };
    run_id
}

#[test]
fn a_decomposed_run_stopped_at_review_unavailable_is_approved_with_the_integration_head() {
    let fixture = Fixture::new();
    let run = unavailable_decomposed_run(&fixture);
    let snapshot = fixture
        .ledger
        .verification_snapshot(&run)
        .expect("read")
        .expect("the stop saved a snapshot");
    assert_eq!(
        fixture.ledger.attempt_count(&run).expect("attempts"),
        0,
        "no attempt row records the integration head"
    );
    let sealed = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run,
        &answer(State::NeedsReview),
        &|| Ok(()),
    )
    .expect("approved");
    assert!(sealed.written);
    let (row, _) = fixture
        .ledger
        .receipt(&run)
        .expect("read")
        .expect("a receipt row");
    assert_eq!(row["outcome"], "accepted");
    assert_eq!(row["candidate_sha"], snapshot.candidate_sha.as_str());
    assert!(fixture
        .artifacts
        .join(run.as_str())
        .join("receipt.json")
        .exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_seal_that_finds_the_decision_not_open_writes_no_receipt_json() {
    let fixture = Fixture::new();
    let (run, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    let state = State::NeedsDecision;
    // The decision is rejected between the artifact and the ledger commit.
    let refused = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &run,
        &answer(state),
        &|| {
            let rejection = DecisionAnswer {
                resolution: Reason::DecisionRejected,
                to_state: State::Failed,
                ..answer(state)
            };
            fixture
                .ledger
                .resolve_decision(&run, &rejection)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
    );
    assert!(
        matches!(refused, Err(ApprovalError::NotOpen { .. })),
        "{refused:?}"
    );
    assert!(!fixture
        .artifacts
        .join(run.as_str())
        .join("receipt.json")
        .exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_repeat_approve_writes_nothing_and_heals_a_missing_receipt_json() {
    let fixture = Fixture::new();
    let (run, _) = over_budget_run(&fixture, Arc::new(AtomicBool::new(false)));
    let state = State::NeedsDecision;
    let approve = || {
        approve_from_snapshot(
            &fixture.ledger,
            &fixture.artifacts,
            &run,
            &answer(state),
            &|| Ok(()),
        )
        .expect("approves")
    };
    let first = approve();
    assert!(first.written);
    let file = fixture.artifacts.join(run.as_str()).join("receipt.json");
    let bytes = std::fs::read(&file).expect("receipt.json");
    let modified = std::fs::metadata(&file)
        .expect("meta")
        .modified()
        .expect("mtime");
    let repeat = approve();
    assert!(!repeat.written);
    assert_eq!(repeat.receipt_hash, first.receipt_hash);
    assert_eq!(
        std::fs::metadata(&file)
            .expect("meta")
            .modified()
            .expect("mtime"),
        modified,
        "an intact receipt.json is left alone"
    );
    std::fs::remove_file(&file).expect("delete");
    let healed = approve();
    assert!(!healed.written);
    assert_eq!(std::fs::read(&file).expect("rewritten"), bytes);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_failure_building_the_stops_receipt_still_ends_the_run_at_its_stop() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let ledger_file = fixture.dir.join("ledger.sqlite");
    let inner = big_patch_backend(0, 0, Arc::new(AtomicBool::new(false)));
    let backend = Refusing(MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            // Settling the receipt reads the sign-offs; take them away.
            rusqlite::Connection::open(&ledger_file)
                .expect("open")
                .execute_batch("DROP TABLE human_signoffs")
                .expect("drop");
        }
        (inner.behavior)(spec)
    }));
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let RunOutcome {
        run_id,
        terminal: Terminal::NeedsReview { .. },
    } = outcome
    else {
        panic!("the run still ends at its stop, got {outcome:?}");
    };
    assert_eq!(
        fixture.ledger.run_status(&run_id).expect("status"),
        Some(State::NeedsReview)
    );
    let events =
        std::fs::read_to_string(fixture.artifacts.join(run_id.as_str()).join("events.jsonl"))
            .expect("events.jsonl");
    assert!(events.contains("verification_snapshot_failed"), "{events}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}
