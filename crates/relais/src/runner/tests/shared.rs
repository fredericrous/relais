//! The runner's reads and settles go through the shared half (an-t16).

use super::*;
use crate::shared_state::fake::FakeState;
use crate::shared_state::SharedState;

/// The day's ceiling counts the global figure too: a fake counter over the
/// cap stops the run before any dispatch, though this node's ledger is empty.
#[test]
fn the_daily_ceiling_reads_the_shared_counter_before_dispatch() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let mut machine = fixture.machine_for(&repo);
    machine.spending.per_day_micros = Some(MicroUsd::from_micros(150));
    let state = FakeState::new();
    state.set_spent(MicroUsd::from_micros(151));
    let backend = MockBackend::new(|_| panic!("the shared counter is over the cap: no dispatch"));
    let outcome = fixture.execute_shared(
        &state,
        None,
        crate::workspace::reuse::Workspace::Isolated,
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &backend,
    );
    let RunOutcome {
        terminal: Terminal::BudgetExhausted { detail, .. },
        ..
    } = outcome
    else {
        panic!("expected budget_exhausted, got {outcome:?}");
    };
    assert!(detail.contains("per day"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

/// One small out-of-scope edit that relais answers itself; the shared half
/// says another node already answered that decision.
fn widened_run(state: &dyn SharedState, run: &RunId) -> (Fixture, RunOutcome) {
    let fixture = Fixture::new();
    let mut repo = fixture.repo_policy(vec![passing_check()], 3);
    repo.execution.widen_scope_small_max_files = 1;
    let machine = fixture.machine_for(&repo);
    let backend = MockBackend::new(|spec| {
        std::fs::write(spec.work_dir.join("outside.rs"), "// out of scope\n").ok();
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_shared(
        state,
        Some(run.clone()),
        crate::workspace::reuse::Workspace::Isolated,
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &backend,
    );
    (fixture, outcome)
}

#[test]
fn a_settle_answered_elsewhere_leaves_the_decision_and_writes_no_intent() {
    let run = RunId::from_stored("run-shared-widen");
    let state = FakeState::new();
    assert_eq!(
        state.answer_decision(&run).expect("granted"),
        crate::shared_state::DecisionGrant::Granted,
        "another node answers first"
    );
    let (fixture, outcome) = widened_run(&state, &run);
    assert!(
        matches!(
            outcome.terminal,
            Terminal::NeedsDecision {
                reason: Reason::ScopeExceeded,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(fixture
        .ledger
        .intent_of_decision(&run)
        .expect("read")
        .is_none());
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn the_same_settle_under_a_free_decision_is_written_through_the_shared_half() {
    let run = RunId::from_stored("run-shared-widen-free");
    let state = FakeState::new();
    let (fixture, outcome) = widened_run(&state, &run);
    assert!(
        matches!(outcome.terminal, Terminal::Cancelled { .. }),
        "{outcome:?}"
    );
    assert!(fixture
        .ledger
        .intent_of_decision(&run)
        .expect("read")
        .is_some());
    assert_eq!(
        state.answer_decision(&run).expect("held"),
        crate::shared_state::DecisionGrant::AlreadyAnswered,
        "the settle took the shared grant and kept it"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}
