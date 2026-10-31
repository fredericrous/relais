// -- a check that times out on its own limit runs once more (SPEC §29) --------
//
// One extra run, by the check alone and with no worker, before the timeout
// counts: a pass on it is a flaky pass, and a timeout the machine's load
// caused no longer buys a repair or blocks a base.

use super::*;

/// A check that is green while `src/main.rs` exists (the base), and once the
/// worker deletes it hangs the first time it runs and passes the second.
/// The marker lives outside the worktrees, so each run sees the same one.
fn hangs_once_after_the_change(fixture: &Fixture) -> CommandSpec {
    let marker = fixture.dir.join("ran-once");
    CommandSpec {
        argv: vec![
            "sh".into(),
            "-c".into(),
            format!(
                "if [ -f src/main.rs ] || [ -f {m} ]; then exit 0; else touch {m}; sleep 30; fi",
                m = marker.display()
            ),
        ],
        timeout_seconds: 1,
        ..passing_check()
    }
}

/// A worker that deletes `src/main.rs` and counts its launches.
fn deleting_worker(launched: &Arc<std::sync::atomic::AtomicUsize>) -> MockBackend {
    let launched = Arc::clone(launched);
    MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            return MockOutcome {
                result_text: Some("review ok\nFINDINGS: none".into()),
                exit_code: Some(0),
                ..Default::default()
            };
        }
        launched.fetch_add(1, Ordering::SeqCst);
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("worker completes");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

fn events_of(fixture: &Fixture, outcome: &RunOutcome) -> String {
    std::fs::read_to_string(
        fixture
            .artifacts
            .join(outcome.run_id.as_str())
            .join("events.jsonl"),
    )
    .unwrap_or_default()
}

fn reasons_of(fixture: &Fixture, outcome: &RunOutcome) -> Vec<String> {
    fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("transitions")
        .into_iter()
        .map(|transition| transition.reason)
        .collect()
}

#[test]
fn a_change_whose_only_failure_is_a_first_run_timeout_is_accepted_without_a_repair() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![hangs_once_after_the_change(&fixture)], 3);
    let launched = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let outcome = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &deleting_worker(&launched),
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{:?}",
        outcome.terminal
    );
    assert_eq!(launched.load(Ordering::SeqCst), 1, "no repair worker");
    let reasons = reasons_of(&fixture, &outcome);
    assert!(
        !reasons.iter().any(|reason| reason == "behavioral_failure"),
        "{reasons:?}"
    );
    assert!(events_of(&fixture, &outcome).contains("timed_out_then_passed"));
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_baseline_check_that_times_out_once_then_passes_is_not_unrunnable() {
    let fixture = Fixture::new();
    let marker = fixture.dir.join("base-ran-once");
    let check = CommandSpec {
        argv: vec![
            "sh".into(),
            "-c".into(),
            format!(
                "if [ -f {m} ]; then exit 0; else touch {m}; sleep 30; fi",
                m = marker.display()
            ),
        ],
        timeout_seconds: 1,
        ..passing_check()
    };
    let repo = fixture.repo_policy(vec![check], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &conditional_worker(""),
    );
    assert!(
        !matches!(
            &outcome.terminal,
            Terminal::Blocked {
                code: BlockCode::BaselineUnrunnable,
                ..
            }
        ),
        "{:?}",
        outcome.terminal
    );
    assert!(events_of(&fixture, &outcome).contains("timed_out_then_passed"));
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_baseline_retry_draws_on_the_total_not_on_work() {
    // The work budget is one second; the baseline spends more than that on
    // its timed-out run and its re-run. Work has not begun then, so the run
    // is not cut off by it.
    let fixture = Fixture::new();
    let marker = fixture.dir.join("base-ran-once");
    let check = CommandSpec {
        argv: vec![
            "sh".into(),
            "-c".into(),
            format!(
                "if [ -f {m} ]; then exit 0; else touch {m}; sleep 30; fi",
                m = marker.display()
            ),
        ],
        timeout_seconds: 2,
        ..passing_check()
    };
    let repo = fixture.repo_policy(vec![check], 3);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.wall_seconds = Some(1);
    let outcome = fixture.execute(&contract, &repo, &conditional_worker(""));
    assert!(
        !matches!(&outcome.terminal, Terminal::BudgetExhausted { .. }),
        "{:?}",
        outcome.terminal
    );
    assert!(events_of(&fixture, &outcome).contains("timed_out_then_passed"));
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}
