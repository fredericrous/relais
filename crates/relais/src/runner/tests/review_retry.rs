// -- a reviewer that ends without a result is asked once more ---------------
//
// docs/plans/2026-10-23-relais-shows-and-settles.md, Phase 4: exactly one
// retry for a reviewer that ended with no terminal result, none for a launch
// failure, a cancellation or an exhausted budget, and the dispatch end named
// on the `review_unavailable` transition.

use std::sync::atomic::{AtomicI64, AtomicUsize};

use crate::backend::{BackendError, Harness};

use super::*;

/// What the scripted reviewer does on its `n`th dispatch (counted from 0).
type Shape = Box<dyn Fn(usize, &mut LaunchResult) + Send + Sync>;

/// A mock whose reviewer dispatches are reshaped by `shape` after the mock
/// answered them, and counted. Workers pass through untouched.
struct Reviewers {
    inner: MockBackend,
    dispatched: Arc<AtomicUsize>,
    shape: Shape,
}

impl Harness for Reviewers {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn probe(&self) -> Option<crate::backend::Capabilities> {
        self.inner.probe()
    }
}

impl Backend for Reviewers {
    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let mut result = self.inner.launch(spec)?;
        if spec.agent == AgentKind::Reviewer {
            let nth = self.dispatched.fetch_add(1, Ordering::SeqCst);
            (self.shape)(nth, &mut result);
        }
        Ok(result)
    }
}

/// The reviewer that gave no result: it exited cleanly and said nothing.
fn silent(result: &mut LaunchResult) {
    result.ended = Ended::Exited(0);
    result.result_text = None;
}

/// A worker that removes `src/main.rs`; a reviewer that answers
/// `FINDINGS: none` before `shape` reshapes it.
fn scripted(
    shape: impl Fn(usize, &mut LaunchResult) + Send + Sync + 'static,
) -> (Reviewers, Arc<AtomicUsize>) {
    let dispatched = Arc::new(AtomicUsize::new(0));
    let inner = MockBackend::new(|spec| {
        if spec.agent == AgentKind::Reviewer {
            return MockOutcome {
                result_text: Some("FINDINGS: none".into()),
                exit_code: Some(0),
                usage: Some(usage(10)),
                ..Default::default()
            };
        }
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("fix");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let backend = Reviewers {
        inner,
        dispatched: Arc::clone(&dispatched),
        shape: Box::new(shape),
    };
    (backend, dispatched)
}

/// The `what` and `detail` of every environment warning the run emitted.
fn warnings(fixture: &Fixture, run: &RunId) -> Vec<(String, String)> {
    let events = std::fs::read_to_string(fixture.artifacts.join(run.as_str()).join("events.jsonl"))
        .expect("events");
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|line| line["event"].clone())
        .filter(|event| event["kind"] == "environment_warning")
        .map(|event| {
            (
                event["what"].as_str().unwrap_or_default().to_string(),
                event["detail"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn retries(fixture: &Fixture, run: &RunId) -> Vec<String> {
    warnings(fixture, run)
        .into_iter()
        .filter(|(what, _)| what == "review_retry")
        .map(|(_, detail)| detail)
        .collect()
}

/// The `detail` the run's ending transition recorded.
fn ending_detail(fixture: &Fixture, run: &RunId) -> String {
    let transitions = fixture.ledger.transitions(run).expect("history");
    ending(&transitions).detail.as_ref().expect("a detail")["detail"]
        .as_str()
        .expect("a sentence")
        .to_string()
}

fn needs_review(outcome: RunOutcome, fixture: &Fixture) -> (RunId, String) {
    let RunOutcome {
        run_id,
        terminal: Terminal::NeedsReview { detail },
    } = outcome
    else {
        panic!("expected needs_review, got {outcome:?}");
    };
    assert_eq!(
        ended_reason(fixture, &run_id),
        Reason::ReviewUnavailable.as_str()
    );
    (run_id, detail)
}

#[test]
fn a_reviewer_without_a_result_is_retried_once_and_the_second_answer_accepts() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (backend, dispatched) = scripted(|nth, result| {
        if nth == 0 {
            silent(result);
        }
    });
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted { .. }),
        "{outcome:?}"
    );
    // Removing the retry fails both assertions: the run ends needs_review
    // after 1 dispatch. An unconditional retry still passes here; the
    // tests below for launch failure, cancellation and budgets catch it.
    assert_eq!(dispatched.load(Ordering::SeqCst), 2);
    let retried = retries(&fixture, &outcome.run_id);
    assert_eq!(retried.len(), 1, "{retried:?}");
    assert!(retried[0].contains("terminal_missing"), "{retried:?}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_reviewer_without_a_result_twice_is_unavailable_after_exactly_two_dispatches() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (backend, dispatched) = scripted(|_, result| silent(result));
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let (run, detail) = needs_review(outcome, &fixture);
    // No retry leaves 1 dispatch; a retry loop leaves more than 2.
    assert_eq!(dispatched.load(Ordering::SeqCst), 2);
    assert_eq!(retries(&fixture, &run).len(), 1);
    assert!(
        detail.contains("the first dispatch had also failed"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn the_unavailable_transition_names_how_the_reviewer_ended() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (backend, dispatched) = scripted(|_, result| {
        silent(result);
        result.stderr = "starting\nmax turns reached\n".into();
        result.stdout = "{\"type\":\"assistant\"}\n{\"type\":\"assistant\"}\n".into();
    });
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let (run, _) = needs_review(outcome, &fixture);
    // Removing the retry fails the first two; a transition detail without
    // the dispatch end fails the last three.
    assert_eq!(dispatched.load(Ordering::SeqCst), 2);
    let detail = ending_detail(&fixture, &run);
    assert!(
        detail.contains("the first dispatch had also failed"),
        "{detail}"
    );
    assert!(detail.contains("dispatch end: exit 0"), "{detail}");
    assert!(detail.contains("2 turns"), "{detail}");
    assert!(
        detail.contains("last stderr: max turns reached"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_reviewer_that_could_not_be_launched_is_not_retried() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (backend, dispatched) = scripted(|_, result| {
        result.ended = Ended::Exited(1);
        result.result_text = None;
        result.failure_detail = Some("the coordinator refused the dispatch".into());
    });
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let (run, detail) = needs_review(outcome, &fixture);
    // An unconditional retry makes this 2 dispatches and one warning.
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert!(retries(&fixture, &run).is_empty());
    assert!(detail.contains("could not be launched"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_cancelled_reviewer_is_not_retried() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (backend, dispatched) = scripted(|_, result| {
        result.ended = Ended::Cancelled;
        result.result_text = None;
    });
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let run = outcome.run_id.clone();
    // An unconditional retry makes this 2 dispatches and one warning.
    assert_eq!(dispatched.load(Ordering::SeqCst), 1, "{outcome:?}");
    assert!(retries(&fixture, &run).is_empty());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// A ledger clock a test moves: a fixed instant plus the seconds the
/// reviewer's first dispatch added. No test waits for real time.
struct Movable {
    skew: Arc<AtomicI64>,
}

impl crate::ledger::Clock for Movable {
    fn now_rfc3339(&self) -> String {
        let base = chrono::DateTime::parse_from_rfc3339("2026-09-20T12:00:00+00:00")
            .expect("a fixed instant");
        (base + chrono::Duration::seconds(self.skew.load(Ordering::SeqCst))).to_rfc3339()
    }
}

/// A fixture on a movable clock, and a reviewer whose first dispatch moves
/// that clock forward by `seconds` and then ends without a result.
fn skewing_first_dispatch(seconds: i64) -> (Fixture, Reviewers, Arc<AtomicUsize>) {
    let skew = Arc::new(AtomicI64::new(0));
    let fixture = Fixture::with_clock(Box::new(Movable {
        skew: Arc::clone(&skew),
    }));
    let (backend, dispatched) = scripted(move |nth, result| {
        if nth == 0 {
            skew.fetch_add(seconds, Ordering::SeqCst);
            silent(result);
        }
    });
    (fixture, backend, dispatched)
}

#[test]
fn a_work_budget_spent_by_the_first_failure_allows_no_retry() {
    let (fixture, backend, dispatched) = skewing_first_dispatch(120);
    let mut repo = fixture.repo_policy(vec![main_gone_check()], 3);
    repo.execution.max_wall_seconds = 60;
    let outcome = fixture.execute(&fixture.contract(Review::Required), &repo, &backend);
    let (run, detail) = needs_review(outcome, &fixture);
    // An unconditional retry makes this 2 dispatches.
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert!(retries(&fixture, &run).is_empty());
    assert!(
        detail.contains("not retried: the work budget is exhausted"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_total_budget_spent_by_the_first_failure_allows_no_retry() {
    let (fixture, backend, dispatched) = skewing_first_dispatch(120);
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let mut contract = fixture.contract(Review::Required);
    contract.limits.total_seconds = Some(60);
    let outcome = fixture.execute(&contract, &repo, &backend);
    let (run, detail) = needs_review(outcome, &fixture);
    // An unconditional retry makes this 2 dispatches.
    assert_eq!(dispatched.load(Ordering::SeqCst), 1);
    assert!(retries(&fixture, &run).is_empty());
    assert!(
        detail.contains("not retried: the total budget is exhausted"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}
