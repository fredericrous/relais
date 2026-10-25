// -- a review in parts -------------------------------------------------
//
// docs/plans/2026-10-20-review-over-budget.md, Changes step 5: answering
// `review-parts` records a verify successor of the same candidate that
// reviews its patch in parts cut at file boundaries, one reviewer dispatch
// per part, in order, every part passing.

use std::sync::Mutex;

use crate::backend::{BackendError, Harness};
use crate::ledger::{
    Actor, DecisionAnswer, IntentReview, PartResult, PartStatus, PlannedPart, RecordedVia,
};
use crate::runner::approval::approve_from_snapshot;
use crate::runner::review::REVIEW_PATCH_BUDGET_BYTES;
use crate::runner::successor;
use crate::verify::{ReviewPartRecord, ReviewRecord};

use super::*;

/// What a reviewer saw of one part.
#[derive(Debug, Clone)]
pub(super) struct Seen {
    label: String,
    patch: Vec<u8>,
}

/// A backend whose worker adds `files` files of `lines` lines and removes
/// `src/main.rs`, and whose reviewer records the part it is asked about,
/// answers `FINDINGS: none` except at `findings_at`, and is refused at
/// `refused_at`. Both are 1-based part numbers.
pub(super) struct Parts {
    pub(super) inner: MockBackend,
    refused_at: Option<u32>,
}

impl Harness for Parts {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn probe(&self) -> Option<crate::backend::Capabilities> {
        self.inner.probe()
    }
}

impl Backend for Parts {
    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let mut result = self.inner.launch(spec)?;
        if let Some(part) = self.refused_at {
            if spec.prompt.contains(&format!("this is part {part} of")) {
                result.ended = Ended::Exited(1);
                result.result_text = None;
                result.failure_detail = Some("the coordinator refused the dispatch".into());
            }
        }
        Ok(result)
    }
}

pub(super) fn parts_backend(
    files: usize,
    lines: usize,
    findings_at: Option<u32>,
    refused_at: Option<u32>,
) -> (Parts, Arc<Mutex<Vec<Seen>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let inner = MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            let label = spec
                .prompt
                .split("this is ")
                .nth(1)
                .and_then(|rest| rest.split(':').next())
                .unwrap_or("the whole patch")
                .to_string();
            // The path the prompt names, read while the copy still exists.
            let path = spec
                .prompt
                .split("the candidate patch is the file `")
                .nth(1)
                .and_then(|rest| rest.split('`').next())
                .expect("the prompt names the patch");
            let patch = std::fs::read(path).expect("the reviewer can read its patch");
            let finding = findings_at.is_some_and(|at| label.starts_with(&format!("part {at} of")));
            log.lock().expect("log").push(Seen { label, patch });
            return MockOutcome {
                result_text: Some(if finding {
                    "FINDINGS:\n- src/big1.txt:1 high - the part-two finding\n".into()
                } else {
                    "FINDINGS: none".into()
                }),
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
    });
    (Parts { inner, refused_at }, seen)
}

/// A first run that stops at the review: over budget when its patch is
/// large, unavailable when its reviewer is refused.
pub(super) fn stopped_run(
    fixture: &Fixture,
    files: usize,
    lines: usize,
    refuse_review: bool,
) -> RunId {
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (backend, _) = parts_backend(files, lines, None, None);
    if refuse_review {
        let refusing = RefuseAll(backend);
        let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &refusing);
        let RunOutcome {
            run_id,
            terminal: Terminal::NeedsReview { .. },
        } = outcome
        else {
            panic!("an unlaunched reviewer is needs_review, got {outcome:?}");
        };
        return run_id;
    }
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let RunOutcome {
        run_id,
        terminal: Terminal::NeedsDecision { reason, .. },
    } = outcome
    else {
        panic!("an over-budget patch is needs_decision, got {outcome:?}");
    };
    assert_eq!(reason, Reason::ReviewOverBudget);
    run_id
}

/// Every reviewer dispatch is refused.
struct RefuseAll(Parts);

impl Harness for RefuseAll {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn probe(&self) -> Option<crate::backend::Capabilities> {
        self.0.probe()
    }
}

impl Backend for RefuseAll {
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

/// Answer `review-parts` to the stopped run as the CLI does (plan, then one
/// transaction), claim the intent and return the successor's contract and id.
pub(super) fn answer_review_parts(
    fixture: &Fixture,
    run: &RunId,
    from_state: State,
) -> (TaskContract, RunId) {
    let intent =
        successor::plan_review_parts(&fixture.ledger, run, &fixture.artifacts).expect("planned");
    assert_eq!(intent.review, Some(IntentReview::Parts));
    let stored = fixture
        .ledger
        .resolve_decision_with_intent(
            run,
            &DecisionAnswer {
                resolution: Reason::DecisionRevised,
                actor: Actor::Orchestrator,
                person_quote: None,
                recorded_via: RecordedVia::Cli,
                note: None,
                successor_run: None,
                from_state,
                to_state: State::Cancelled,
            },
            &intent,
        )
        .expect("written")
        .expect("an open decision");
    assert_eq!(stored.review, Some(IntentReview::Parts));
    let new_run = fixture.ids.run_id().expect("a run id");
    assert!(matches!(
        fixture
            .ledger
            .claim_intent(&stored.intent_id, &new_run, &|_| {
                crate::ledger::SuccessorProcess::Alive { started: None }
            })
            .expect("claim"),
        crate::ledger::Claim::Claimed(_)
    ));
    (
        TaskContract::from_json_str(&stored.contract_json).expect("the stored contract"),
        new_run,
    )
}

fn sha_of_file(path: &str) -> String {
    crate::ids::sha256_hex(&std::fs::read(path).expect("the artifact exists"))
}

/// Reopen the ledger and check every recorded artifact against its hash.
fn manifest(fixture: &Fixture, run: &RunId) -> Vec<crate::ledger::ReviewPartRow> {
    let rows = Ledger::open(&fixture.dir.join("ledger.sqlite"))
        .expect("reopen")
        .review_parts(run)
        .expect("manifest");
    for row in &rows {
        assert_eq!(sha_of_file(&row.patch_path), row.patch_sha256, "{row:?}");
        if let (Some(path), Some(sha)) = (&row.result_path, &row.result_sha256) {
            assert_eq!(&sha_of_file(path), sha, "{row:?}");
        }
    }
    rows
}

#[test]
fn an_over_budget_patch_is_reviewed_in_parts_cut_at_file_boundaries_and_all_must_pass() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = stopped_run(&fixture, 2, 2300, false);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    let (backend, seen) = parts_backend(2, 2300, None, None);

    let outcome = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &backend);
    let RunOutcome {
        terminal: Terminal::Accepted(receipt),
        ..
    } = outcome
    else {
        panic!("every part passed, so the successor is accepted: {outcome:?}");
    };

    // Dispatched in order, each part within the budget.
    let seen = seen.lock().expect("log").clone();
    let labels: Vec<&str> = seen.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["part 1 of 2", "part 2 of 2"]);
    assert!(seen
        .iter()
        .all(|s| s.patch.len() <= REVIEW_PATCH_BUDGET_BYTES));
    // Cut at file boundaries: every part begins with a file header, and the
    // parts concatenated are the successor's whole patch.
    assert!(seen.iter().all(|s| s.patch.starts_with(b"diff --git ")));
    let whole = std::fs::read(
        fixture
            .artifacts
            .join(new_run.as_str())
            .join("candidate-latest.patch"),
    )
    .expect("the successor's patch");
    assert!(whole.len() > REVIEW_PATCH_BUDGET_BYTES);
    assert_eq!(
        seen.iter()
            .flat_map(|s| s.patch.clone())
            .collect::<Vec<u8>>(),
        whole
    );

    // The manifest and the artifacts agree, and the receipt lists each part.
    let rows = manifest(&fixture, &new_run);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.status == PartStatus::Passed));
    assert!(rows.iter().all(|row| row.dispatch_id.is_some()));
    let ReviewRecord::Parts { parts, .. } = &receipt.review else {
        panic!("the receipt lists the parts: {:?}", receipt.review);
    };
    assert_eq!(
        parts,
        &rows
            .iter()
            .map(|row| ReviewPartRecord {
                index: row.index,
                of: 2,
                patch_sha256: row.patch_sha256.clone(),
                result_sha256: row.result_sha256.clone(),
                verdict: "passed".into(),
            })
            .collect::<Vec<_>>()
    );
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            fixture
                .artifacts
                .join(new_run.as_str())
                .join("receipt.json"),
        )
        .expect("receipt.json"),
    )
    .expect("json");
    assert_eq!(
        written["review"]["parts"]["parts"].as_array().map(Vec::len),
        Some(2)
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_failing_part_fails_the_review_with_its_findings_and_earlier_evidence_stands() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = stopped_run(&fixture, 3, 2300, false);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    let (backend, seen) = parts_backend(3, 2300, Some(2), None);

    let outcome = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &backend);
    let RunOutcome {
        terminal: Terminal::NeedsReview { detail, .. },
        ..
    } = outcome
    else {
        panic!("a part with findings is needs_review: {outcome:?}");
    };
    assert!(detail.contains("part 2 of 3"), "{detail}");
    assert!(detail.contains("the part-two finding"), "{detail}");
    assert_eq!(
        seen.lock().expect("log").len(),
        2,
        "the third part was never dispatched"
    );
    let rows = manifest(&fixture, &new_run);
    let statuses: Vec<PartStatus> = rows.iter().map(|row| row.status).collect();
    assert_eq!(
        statuses,
        [PartStatus::Passed, PartStatus::Findings, PartStatus::NotRun]
    );
    assert!(rows[2].result_path.is_none());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_later_dispatch_failure_keeps_the_earlier_evidence_and_says_which_parts_never_ran() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = stopped_run(&fixture, 3, 2300, false);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    let (backend, seen) = parts_backend(3, 2300, None, Some(2));

    let outcome = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &backend);
    let RunOutcome {
        terminal: Terminal::NeedsReview { detail, .. },
        ..
    } = outcome
    else {
        panic!("an unlaunched part is needs_review: {outcome:?}");
    };
    assert!(detail.contains("part 2 of 3"), "{detail}");
    assert!(
        detail.contains("the coordinator refused the dispatch"),
        "{detail}"
    );
    assert_eq!(seen.lock().expect("log").len(), 2);
    let rows = manifest(&fixture, &new_run);
    let statuses: Vec<PartStatus> = rows.iter().map(|row| row.status).collect();
    assert_eq!(
        statuses,
        [
            PartStatus::Passed,
            PartStatus::Unavailable,
            PartStatus::NotRun
        ]
    );
    // Part one's answer is on disk and in the manifest; part two has none.
    assert!(rows[0].result_sha256.is_some());
    assert!(rows[1].result_sha256.is_none());
    assert!(rows[1]
        .detail
        .as_deref()
        .is_some_and(|d| d.contains("the coordinator refused the dispatch")));
    // The dispatch id was recorded when the part was marked dispatched, so
    // a part that never answered still names it.
    assert!(rows[0].dispatch_id.is_some());
    assert!(rows[1].dispatch_id.is_some());
    assert!(rows[2].dispatch_id.is_none());
    // The decision of the new run is a review stop, answerable again.
    assert_eq!(
        fixture.ledger.run_status(&new_run).expect("status"),
        Some(State::NeedsReview)
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn review_parts_on_an_unavailable_review_within_the_budget_reviews_it_as_one_part() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = stopped_run(&fixture, 0, 0, true);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsReview);
    let (backend, seen) = parts_backend(0, 0, None, None);

    let outcome = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &backend);
    let RunOutcome {
        terminal: Terminal::Accepted(receipt),
        ..
    } = outcome
    else {
        panic!("the retried review passes: {outcome:?}");
    };
    let labels: Vec<String> = seen
        .lock()
        .expect("log")
        .iter()
        .map(|s| s.label.clone())
        .collect();
    assert_eq!(labels, ["part 1 of 1"]);
    let ReviewRecord::Parts { parts, .. } = &receipt.review else {
        panic!("{:?}", receipt.review);
    };
    assert_eq!(parts.len(), 1);
    assert_eq!(manifest(&fixture, &new_run).len(), 1);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_single_file_over_the_budget_is_not_planned_and_dispatches_nothing() {
    let fixture = Fixture::new();
    let run = stopped_run(&fixture, 1, 4000, false);
    let refused = successor::plan_review_parts(&fixture.ledger, &run, &fixture.artifacts);
    let Err(successor::PlanError::NotReviewable(why)) = refused else {
        panic!("a file over the budget is refused: {refused:?}");
    };
    assert!(why.contains("src/big0.txt"), "{why}");
    assert!(why.contains("cannot be reviewed in parts"), "{why}");
    assert_eq!(
        fixture.ledger.count_successor_intents().expect("count"),
        0,
        "no intent was written"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn the_split_is_whole_ordered_and_bounded() {
    use crate::runner::review::{split_into_parts, PartsRefusal};
    let file =
        |name: &str, bytes: usize| format!("diff --git a/{name} b/{name}\n{}\n", "y".repeat(bytes));
    let patch = [file("a", 60), file("b", 60), file("c", 60)].concat();
    let one = file("a", 60).len();
    let parts = split_into_parts(patch.as_bytes(), one * 2).expect("two parts");
    assert_eq!(parts.len(), 2);
    assert_eq!(parts.concat(), patch.as_bytes());
    assert!(parts.iter().all(|part| part.len() <= one * 2));
    assert!(matches!(
        split_into_parts(patch.as_bytes(), one - 1),
        Err(PartsRefusal::FileOverBudget { ref file, .. }) if file == "a"
    ));
    // Nine files, one per part: more than a review may take.
    let many: String = (0..9).map(|n| file(&format!("f{n}"), 60)).collect();
    let each = file("f0", 60).len();
    assert!(matches!(
        split_into_parts(many.as_bytes(), each),
        Err(PartsRefusal::TooManyParts { needed: 9 })
    ));
    // A patch within the budget is one part.
    assert_eq!(
        split_into_parts(patch.as_bytes(), one * 3)
            .expect("one part")
            .len(),
        1
    );
    // The decision detail does not offer what cannot be had.
    let over = crate::runner::machine::OverBudget {
        patch_bytes: 1_000_000,
        budget_bytes: 196_608,
        files: 9,
        parts: Some(9),
    };
    assert!(
        over.describe()
            .contains("more than the 8 a review may take"),
        "{}",
        over.describe()
    );
}

/// The parts a first, interrupted attempt of `new_run` left on record: part 1
/// passed, part 2 handed to a reviewer and never answered, the rest not run.
fn seed_interrupted_review(fixture: &Fixture, run: &RunId, new_run: &RunId) {
    let sha = fixture
        .ledger
        .verification_snapshot(run)
        .expect("snapshot")
        .expect("a snapshot")
        .candidate_sha;
    let patch = std::fs::read(
        fixture
            .artifacts
            .join(run.as_str())
            .join("candidate-latest.patch"),
    )
    .expect("the first run's patch");
    let dir = fixture
        .artifacts
        .join(new_run.as_str())
        .join("review-parts");
    std::fs::create_dir_all(&dir).expect("dir");
    let planned: Vec<PlannedPart> =
        crate::runner::review::split_into_parts(&patch, REVIEW_PATCH_BUDGET_BYTES)
            .expect("parts")
            .iter()
            .enumerate()
            .map(|(at, bytes)| {
                let index = u32::try_from(at + 1).expect("index");
                let path = dir.join(format!("{index}.patch"));
                crate::runner::review::write_immutable(&path, bytes).expect("part file");
                PlannedPart {
                    index,
                    patch_path: path.to_string_lossy().into_owned(),
                    patch_sha256: crate::ids::sha256_hex(bytes),
                }
            })
            .collect();
    assert_eq!(planned.len(), 3);
    let ledger = &fixture.ledger;
    // The manifest names a run that must exist while it is written; the
    // run row goes again afterwards, because the run starts as its own.
    let task = crate::ids::TaskId::from_stored("task-seed".to_string());
    ledger
        .insert_run(new_run, "/repo", None, &task, "rk")
        .expect("a run row");
    ledger
        .plan_review_parts(new_run, &sha, &planned)
        .expect("plan");
    ledger
        .mark_review_part_dispatched(new_run, &sha, 1, "old-dispatch-1")
        .expect("mark 1");
    ledger
        .finish_review_part(
            new_run,
            &sha,
            1,
            &PartResult {
                status: PartStatus::Passed,
                dispatch_id: None,
                result_path: None,
                result_sha256: None,
                detail: None,
            },
        )
        .expect("finish 1");
    ledger
        .mark_review_part_dispatched(new_run, &sha, 2, "old-dispatch-2")
        .expect("mark 2");
    // The reviewer answered and the run stopped before it settled the part:
    // the answer file and its evidence row exist, the part is `dispatched`.
    let old_answer = dir.join("2.old-dispatch-2.txt");
    crate::runner::review::write_immutable(&old_answer, b"an earlier attempt's answer")
        .expect("old answer");
    ledger
        .record_evidence(
            new_run,
            None,
            EvidenceKind::ReviewResult,
            &old_answer,
            Some(&crate::ids::sha256_hex(b"an earlier attempt's answer")),
        )
        .expect("old evidence row");
    let raw = rusqlite::Connection::open(fixture.dir.join("ledger.sqlite")).expect("open");
    raw.execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("pragma");
    raw.execute("DELETE FROM runs WHERE id = ?1", [new_run.as_str()])
        .expect("the run row goes");
}

/// A retried part keeps the answer an earlier attempt left: the new answer
/// goes to a path of its own, so every evidence row still names the bytes on
/// disk, and the manifest names the attempt that counts.
#[test]
fn a_retried_part_keeps_the_answer_an_earlier_attempt_left() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = stopped_run(&fixture, 3, 2300, false);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    seed_interrupted_review(&fixture, &run, &new_run);
    let (backend, _seen) = parts_backend(3, 2300, None, None);

    let outcome = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "the retry is not blocked by the earlier answer: {outcome:?}"
    );
    let results: Vec<_> = fixture
        .ledger
        .evidence(&new_run)
        .expect("evidence")
        .into_iter()
        .filter(|row| row.kind == EvidenceKind::ReviewResult)
        .collect();
    assert!(
        results
            .iter()
            .any(|row| row.path.ends_with("2.old-dispatch-2.txt")),
        "the earlier attempt's row stays: {results:?}"
    );
    for row in &results {
        let on_disk = crate::ids::sha256_hex(&std::fs::read(&row.path).expect("answer on disk"));
        assert_eq!(
            row.sha256.as_deref(),
            Some(on_disk.as_str()),
            "{}",
            row.path
        );
    }
    let sha = fixture
        .ledger
        .verification_snapshot(&run)
        .expect("snapshot")
        .expect("a snapshot")
        .candidate_sha;
    let part_two = fixture
        .ledger
        .review_parts_of(&new_run, &sha)
        .expect("manifest")
        .into_iter()
        .find(|row| row.index == 2)
        .expect("part 2");
    let counted = part_two.result_path.expect("a result path");
    assert!(!counted.ends_with("old-dispatch-2.txt"), "{counted}");
    assert!(
        results.iter().any(|row| row.path == counted),
        "the manifest's path has an evidence row: {counted}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn an_interrupted_review_in_parts_resumes_skipping_the_passed_and_dispatching_the_rest() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = stopped_run(&fixture, 3, 2300, false);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    seed_interrupted_review(&fixture, &run, &new_run);
    let (backend, seen) = parts_backend(3, 2300, None, None);

    let outcome = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &backend);
    let RunOutcome {
        terminal: Terminal::Accepted(receipt),
        ..
    } = outcome
    else {
        panic!("the resumed review passes: {outcome:?}");
    };

    let labels: Vec<String> = seen
        .lock()
        .expect("log")
        .iter()
        .map(|s| s.label.clone())
        .collect();
    assert_eq!(labels, ["part 2 of 3", "part 3 of 3"], "part 1 passed");
    let rows = manifest(&fixture, &new_run);
    assert!(rows.iter().all(|row| row.status == PartStatus::Passed));
    assert_eq!(rows[0].dispatch_id.as_deref(), Some("old-dispatch-1"));
    for row in &rows[1..] {
        let id = row.dispatch_id.as_deref().expect("a dispatch id");
        assert_ne!(id, "old-dispatch-2");
        assert!(row.result_sha256.is_some());
    }
    let ReviewRecord::Parts { parts, .. } = &receipt.review else {
        panic!("{:?}", receipt.review);
    };
    assert_eq!(parts.len(), 3);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_part_left_dispatched_becomes_unavailable_interrupted_and_keeps_its_dispatch() {
    let fixture = Fixture::new();
    let run = stopped_run(&fixture, 3, 2300, false);
    let (_, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    seed_interrupted_review(&fixture, &run, &new_run);
    let sha = fixture
        .ledger
        .verification_snapshot(&run)
        .expect("snapshot")
        .expect("a snapshot")
        .candidate_sha;

    fixture
        .ledger
        .interrupt_review_parts(&new_run, &sha)
        .expect("interrupt");
    let rows = fixture
        .ledger
        .review_parts_of(&new_run, &sha)
        .expect("manifest");
    let seen: Vec<(PartStatus, Option<&str>)> = rows
        .iter()
        .map(|row| (row.status, row.detail.as_deref()))
        .collect();
    assert_eq!(
        seen,
        [
            (PartStatus::Passed, None),
            (PartStatus::Unavailable, Some("interrupted")),
            (PartStatus::NotRun, None)
        ]
    );
    assert_eq!(rows[1].dispatch_id.as_deref(), Some("old-dispatch-2"));
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn approving_a_failed_parts_successor_seals_a_receipt_listing_every_part() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let run = stopped_run(&fixture, 3, 2300, false);
    let (contract, new_run) = answer_review_parts(&fixture, &run, State::NeedsDecision);
    let (backend, _) = parts_backend(3, 2300, None, Some(2));
    let outcome = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::NeedsReview { .. }),
        "{outcome:?}"
    );

    let sealed = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &new_run,
        &DecisionAnswer {
            resolution: Reason::DecisionApproved,
            actor: Actor::Orchestrator,
            person_quote: None,
            recorded_via: RecordedVia::Cli,
            note: None,
            successor_run: None,
            from_state: State::NeedsReview,
            to_state: State::Accepted,
        },
        &|| Ok(()),
    )
    .expect("approved");
    let body = std::fs::read_to_string(&sealed.artifact).expect("the artifact");
    let receipt: crate::verify::Receipt = serde_json::from_str(&body).expect("a receipt");
    assert_eq!(receipt.outcome, "accepted");
    let ReviewRecord::Parts { parts, .. } = &receipt.review else {
        panic!("the sealed receipt lists the parts: {:?}", receipt.review);
    };
    let rows = manifest(&fixture, &new_run);
    assert_eq!(
        parts
            .iter()
            .map(|part| (part.index, part.of, part.verdict.as_str()))
            .collect::<Vec<_>>(),
        [(1, 3, "passed"), (2, 3, "unavailable"), (3, 3, "not_run")]
    );
    assert_eq!(
        parts
            .iter()
            .map(|part| part.patch_sha256.clone())
            .collect::<Vec<_>>(),
        rows.iter()
            .map(|row| row.patch_sha256.clone())
            .collect::<Vec<_>>()
    );
    assert!(parts[0].result_sha256.is_some());
    assert!(parts[1].result_sha256.is_none());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_part_artifact_is_created_once_synced_and_never_overwritten_with_other_bytes() {
    use crate::runner::review::write_immutable;
    let dir = crate::test_support::temp_dir("review-parts-immutable");
    let path = dir.join("1.txt");

    write_immutable(&path, b"FINDINGS: none").expect("created");
    assert_eq!(std::fs::read(&path).expect("read"), b"FINDINGS: none");
    // The same bytes again are accepted and change nothing.
    write_immutable(&path, b"FINDINGS: none").expect("same bytes");
    // Other bytes are refused and the file is untouched.
    let refused = write_immutable(&path, b"FINDINGS:\n- a thing");
    assert!(refused.is_err());
    assert!(refused
        .expect_err("refused")
        .to_string()
        .contains("immutable"));
    assert_eq!(std::fs::read(&path).expect("read"), b"FINDINGS: none");
    // A shorter write over a longer file is also refused, not truncated into.
    assert!(write_immutable(&path, b"FIND").is_err());
    assert_eq!(std::fs::read(&path).expect("read"), b"FINDINGS: none");
}
