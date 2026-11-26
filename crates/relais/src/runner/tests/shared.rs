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
        Shared {
            state: &state,
            run_id: None,
            workspace: crate::workspace::reuse::Workspace::Isolated,
        },
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
        std::fs::write(spec.work_dir.join("outside.rs"), "// out of scope\n")
            .expect("write the out-of-scope file");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_shared(
        Shared {
            state,
            run_id: Some(run.clone()),
            workspace: crate::workspace::reuse::Workspace::Isolated,
        },
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
        crate::shared_state::DecisionGrant::HeldElsewhere,
        "the settle took the shared grant and kept it"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

/// The shared counter passes the cap while the worker runs: the review
/// check reads it and stops the run unreviewed, before any reviewer.
#[test]
fn the_review_check_reads_the_shared_counter_too() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let mut machine = fixture.machine_for(&repo);
    machine.spending.per_day_micros = Some(MicroUsd::from_micros(150));
    let state = FakeState::new();
    let raised = state.clone();
    let backend = MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            panic!("the day's cap is passed: no reviewer");
        }
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("fix");
        // Other nodes spend while this worker runs.
        raised.set_spent(MicroUsd::from_micros(200));
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_shared(
        Shared {
            state: &state,
            run_id: None,
            workspace: crate::workspace::reuse::Workspace::Isolated,
        },
        &fixture.contract(Review::Required),
        &repo,
        &machine,
        &backend,
    );
    let RunOutcome {
        terminal: Terminal::NeedsReview { detail },
        ..
    } = outcome
    else {
        panic!("expected needs_review, got {outcome:?}");
    };
    assert!(
        detail.contains("spend ceiling was reached before the review")
            && detail.contains("per day"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

/// A run with no coordinator feeds the shared counter with what it spent
/// (an-t24): the dispatch's whole cost, booked once.
#[test]
fn a_run_with_no_gate_books_its_cost_to_the_shared_counter() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let machine = fixture.machine_for(&repo);
    let state = FakeState::new();
    let backend = MockBackend::new(|spec| {
        if spec.prompt.contains("semantic reviewer") {
            return MockOutcome {
                result_text: Some("FINDINGS: none".into()),
                exit_code: Some(0),
                ..Default::default()
            };
        }
        std::fs::write(
            spec.work_dir.join("src/main.rs"),
            "fn main() { let _changed = 1; }\n",
        )
        .expect("write main.rs");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(120)),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_shared(
        Shared {
            state: &state,
            run_id: None,
            workspace: crate::workspace::reuse::Workspace::Isolated,
        },
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &backend,
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "the run is accepted, so its spend is a finished run's: {outcome:?}"
    );
    let ledger_spend = fixture
        .ledger
        .spend_since("1970-01-01T00:00:00+00:00")
        .expect("ledger spend");
    assert!(ledger_spend.to_micros() >= 120, "the ledger booked it");
    assert_eq!(
        state.spent(),
        ledger_spend,
        "the counter holds what the ledger holds, once each dispatch"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

/// With a coordinator the runner books nothing itself: the coordinator's
/// settle feeds the counter, so the run does not count its cost twice.
#[test]
fn a_run_under_a_gate_leaves_the_counter_to_the_coordinator() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let machine = fixture.machine_for(&repo);
    let state = FakeState::new();
    let gate = crate::admission::LocalGate::new(
        crate::policy::ConcurrencyLimits::default(),
        crate::ledger::now_rfc3339,
    );
    let backend = MockBackend::new(|spec| {
        if spec.prompt.contains("semantic reviewer") {
            return MockOutcome {
                result_text: Some("FINDINGS: none".into()),
                exit_code: Some(0),
                ..Default::default()
            };
        }
        std::fs::write(
            spec.work_dir.join("src/main.rs"),
            "fn main() { let _changed = 1; }\n",
        )
        .expect("write main.rs");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(120)),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_presented_with(
        &state,
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &backend,
        &gate,
        Presented {
            wire: Wire::process(),
        },
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "the run is accepted, so its spend is a finished run's: {outcome:?}"
    );
    let ledger_spend = fixture
        .ledger
        .spend_since("1970-01-01T00:00:00+00:00")
        .expect("ledger spend");
    assert!(ledger_spend.to_micros() >= 120, "the ledger booked it");
    assert_eq!(
        state.spent(),
        MicroUsd::from_micros(0),
        "the runner left the counter to the coordinator"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

/// an-t31: the line for a cost the counter did not book says how short the
/// counter is and where to read on, never a check no command can make.
#[test]
fn an_unbooked_cost_says_how_short_the_counter_is_and_where_to_look() {
    let line = crate::runner::launch::unbooked_line(
        "disp-1",
        &MicroUsd::from_micros(1_500_000),
        &"primary unreachable",
    );
    assert!(line.starts_with("relais: dispatch disp-1: "), "{line}");
    assert!(line.contains("primary unreachable"), "{line}");
    assert!(line.contains("the shared counter is short by"), "{line}");
    assert!(
        line.ends_with(crate::shared_state::PRIMARY_UNREAD),
        "{line}"
    );
    assert!(!line.contains("check the shared primary"), "{line}");
}
