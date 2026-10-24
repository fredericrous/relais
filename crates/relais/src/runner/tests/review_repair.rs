// -- findings below `high` get one repair ---------------------------------
//
// docs/plans/2026-10-23-relais-shows-and-settles.md, Phase 2: a review
// whose findings are all medium or low sends a `kind: change` run back to
// its worker once; everything else stops `needs_review`.

use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

use super::*;

const LOW: &str = "FINDINGS:\n1. [low] src/lib.rs:1 stale comment. Fix.\n";
const HIGH: &str = "FINDINGS:\n1. [high] src/lib.rs:1 wrong result. Fix.\n";

/// What a backend saw: worker prompts in order, and the number of reviews.
#[derive(Default)]
struct Seen {
    workers: Mutex<Vec<String>>,
    reviews: AtomicUsize,
}

/// A worker that removes `src/main.rs` and writes a file per attempt, and a
/// reviewer that answers `answers[n]` to its n-th review, then
/// `FINDINGS: none`.
fn backend(seen: &Arc<Seen>, answers: &'static [&'static str]) -> MockBackend {
    let seen = Arc::clone(seen);
    MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            let n = seen.reviews.fetch_add(1, Ordering::SeqCst);
            let text = answers.get(n).copied().unwrap_or("FINDINGS: none");
            return MockOutcome {
                result_text: Some(text.into()),
                exit_code: Some(0),
                usage: Some(usage(10)),
                ..Default::default()
            };
        }
        let mut workers = seen.workers.lock().expect("lock");
        workers.push(spec.prompt.clone());
        let attempt = workers.len();
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).ok();
        std::fs::write(spec.work_dir.join(format!("src/note-{attempt}.txt")), "x\n")
            .expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

fn reasons(fixture: &Fixture, run: &RunId) -> Vec<String> {
    fixture
        .ledger
        .transitions(run)
        .expect("rows")
        .into_iter()
        .map(|t| t.reason)
        .collect()
}

#[test]
fn low_findings_are_repaired_once_and_the_run_is_accepted() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let seen = Arc::new(Seen::default());
    let backend = backend(&seen, &[LOW]);
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let workers = seen.workers.lock().expect("lock");
    assert_eq!(workers.len(), 2, "one attempt and exactly one repair");
    assert_eq!(seen.reviews.load(Ordering::SeqCst), 2);
    let repair = &workers[1];
    assert!(repair.contains("[review repair addendum]"), "{repair}");
    assert!(repair.contains("stale comment"), "{repair}");
    assert!(!repair.contains("failed verification on these checks"));
    let reasons = reasons(&fixture, &outcome.run_id);
    assert_eq!(
        reasons
            .iter()
            .filter(|r| *r == Reason::ReviewRepair.as_str())
            .count(),
        1,
        "{reasons:?}"
    );
    // Each review is its own file, still holding the bytes it was hashed at.
    let rows: Vec<_> = fixture
        .ledger
        .evidence(&outcome.run_id)
        .expect("evidence")
        .into_iter()
        .filter(|row| row.kind == EvidenceKind::ReviewResult)
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_ne!(rows[0].path, rows[1].path);
    for row in &rows {
        let bytes = std::fs::read(&row.path).expect("the review file");
        assert_eq!(Some(crate::ids::sha256_hex(&bytes)), row.sha256, "{row:?}");
    }
    assert_eq!(
        std::fs::read_to_string(&rows[0].path).expect("first review"),
        LOW
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_high_finding_stops_without_a_repair() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let seen = Arc::new(Seen::default());
    let backend = backend(&seen, &[HIGH]);
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::NeedsReview { .. }),
        "{outcome:?}"
    );
    assert_eq!(seen.workers.lock().expect("lock").len(), 1);
    assert!(
        !reasons(&fixture, &outcome.run_id).contains(&Reason::ReviewRepair.as_str().to_string())
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn low_findings_twice_stop_after_one_repair() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let seen = Arc::new(Seen::default());
    let backend = backend(&seen, &[LOW, LOW]);
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::NeedsReview { .. }),
        "{outcome:?}"
    );
    assert_eq!(seen.workers.lock().expect("lock").len(), 2);
    assert_eq!(seen.reviews.load(Ordering::SeqCst), 2);
    let reasons = reasons(&fixture, &outcome.run_id);
    let repairs = reasons
        .iter()
        .filter(|r| *r == Reason::ReviewRepair.as_str())
        .count();
    assert_eq!(repairs, 1, "{reasons:?}");
    let settled: Vec<&String> = reasons
        .iter()
        .filter(|r| *r != Reason::WorktreeRetired.as_str())
        .collect();
    assert_eq!(
        settled.last().map(|r| r.as_str()),
        Some(Reason::ReviewFindings.as_str()),
        "{reasons:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_verify_run_with_low_findings_stops_and_dispatches_no_worker() {
    let fixture = Fixture::new();
    let (source, _) = super::verify_kind::accepted_source(&fixture);
    let seen = Arc::new(Seen::default());
    let backend = backend(&seen, &[LOW]);
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let json = serde_json::json!({
        "schema_version": 1,
        "kind": "verify",
        "objective": "Verify the candidate",
        "base_ref": "HEAD",
        "write_scope": ["src/**"],
        "candidate": { "run": source.as_str() },
        "acceptance": ["the profile's checks pass"],
        "verification_profile": "profile",
        "review": "required",
    });
    let contract = TaskContract::from_json_str(&json.to_string()).expect("contract");
    let outcome = fixture.execute(&contract, &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::NeedsReview { .. }),
        "{outcome:?}"
    );
    assert!(seen.workers.lock().expect("lock").is_empty());
    assert_eq!(seen.reviews.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn the_review_repair_prompt_quotes_the_findings_as_data() {
    let fixture = Fixture::new();
    let contract = fixture.contract(Review::Required);
    let manifest = manifest_with(Vec::new());
    let prompt = build_review_repair_prompt(&contract, &manifest, 1, LOW);
    assert!(prompt.contains("a reviewer found these"), "{prompt}");
    assert!(prompt.contains("quoted data"), "{prompt}");
    assert!(prompt.contains("stale comment"), "{prompt}");
    assert!(prompt.contains("change nothing else"), "{prompt}");
    assert!(!prompt.contains("failed verification on these checks"));
}
