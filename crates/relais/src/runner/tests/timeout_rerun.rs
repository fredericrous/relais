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
            // Red on the candidate, so the deferred base is checked on
            // demand (ADR-0013); at the base it times out once, then passes.
            format!(
                "case \"$(pwd -P)\" in */verify-base) \
                 if [ -f {m} ]; then exit 0; else touch {m}; sleep 30; fi;; esac; exit 1",
                m = marker.display()
            ),
        ],
        timeout_seconds: 1,
        ..passing_check()
    };
    let repo = fixture.repo_policy(vec![check], 1);
    let outcome = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &changing_worker(Arc::new(std::sync::atomic::AtomicUsize::new(0))),
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
    // The base, checked on demand after a red candidate (ADR-0013),
    // spends more than the work budget on its timed-out run (10 s) and its
    // re-run. It draws on the total only, and its time is taken out of
    // work, so the run is not cut off by it: a base run longer than the
    // work left never ends the run `Work`. Six seconds of work leaves room
    // for the worker and the candidate's checks on a slow git.
    let fixture = Fixture::new();
    let marker = fixture.dir.join("base-ran-once");
    let check = CommandSpec {
        argv: vec![
            "sh".into(),
            "-c".into(),
            // Red on the candidate, so the deferred base is checked on
            // demand (ADR-0013); at the base it times out once, then passes.
            format!(
                "case \"$(pwd -P)\" in */verify-base) \
                 if [ -f {m} ]; then exit 0; else touch {m}; sleep 30; fi;; esac; exit 1",
                m = marker.display()
            ),
        ],
        timeout_seconds: 10,
        ..passing_check()
    };
    let repo = fixture.repo_policy(vec![check], 1);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.wall_seconds = Some(6);
    let outcome = fixture.execute(
        &contract,
        &repo,
        &changing_worker(Arc::new(std::sync::atomic::AtomicUsize::new(0))),
    );
    // One attempt, red: the attempt ceiling ends it, not the clock.
    assert!(
        outcome.terminal.detail().contains("attempt ceiling"),
        "{:?}",
        outcome.terminal
    );
    assert!(
        fixture
            .artifacts
            .join(outcome.run_id.as_str())
            .join("logs")
            .join("base-cmd0-retry1.log")
            .exists(),
        "the base ran, and ran again: {:?}",
        outcome.terminal
    );
    assert!(events_of(&fixture, &outcome).contains("timed_out_then_passed"));
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}
