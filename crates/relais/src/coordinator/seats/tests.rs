use super::*;
use crate::admission::{
    DispatchRequest, DispatchSource, ResourceClass, RunRegistration, WithdrawOutcome,
};
use crate::coordinator::{answer_after_seats, Response};
use crate::policy::ConcurrencyLimits;
use crate::shared_state::fake::FakeState;
use crate::shared_state::{LocalState, Slice};

fn limits(agents: u32) -> ConcurrencyLimits {
    ConcurrencyLimits {
        max_active_agents: Some(agents),
        max_active_agents_per_session: Some(agents),
        max_agents_per_run: Some(24),
        max_agent_depth: Some(3),
        ..ConcurrencyLimits::default()
    }
}

/// An admission state as a coordinator builds it, with one run.
fn admission(agents: u32) -> Mutex<AdmissionState> {
    let mut state = AdmissionState::new(limits(agents));
    state.track_shared_spend();
    state.register_run(
        &RunRegistration {
            run_id: "run".into(),
            session_id: "tab".into(),
            budget_micros: None,
            max_agents: None,
            max_depth: None,
        },
        Instant::now(),
    );
    Mutex::new(state)
}

fn agent(dispatch: &str, reserve_micros: i64) -> DispatchRequest {
    DispatchRequest {
        disk: None,
        dispatch_id: dispatch.into(),
        run_id: "run".into(),
        session_id: "tab".into(),
        parent_dispatch: None,
        depth: 0,
        resource: ResourceClass::ModelWork,
        reserve_micros,
        source: DispatchSource::ManagedRun,
        caller_agent_id: None,
        weight: 1,
    }
}

/// A book over `shared` whose lines land in the returned buffer.
fn book(shared: Arc<dyn SharedState>) -> (SeatBook, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let book =
        SeatBook::new(shared, Principal::node("node-a")).with_log(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string())
        }));
    (book, lines)
}

/// What a handler answers an admission request: `handle`, then the seats.
fn admit(book: &SeatBook, state: &Mutex<AdmissionState>, request: &DispatchRequest) -> Decision {
    let decision = super::super::lock_state(state).request(request, Instant::now());
    book.sync_request(state, Some(&request.dispatch_id));
    match answer_after_seats(
        book,
        state,
        Some(&request.dispatch_id),
        Response::Decision { decision },
    ) {
        Response::Decision { decision } => decision,
        other => panic!("not a decision: {other:?}"),
    }
}

fn remaining(fake: &FakeState) -> i64 {
    match fake.slice().expect("slice") {
        Slice::Granted { remaining, .. } => remaining.to_micros(),
        Slice::Unbounded => panic!("the fake bounds its slice"),
    }
}

#[test]
fn a_grant_takes_a_seat_and_a_reservation_and_the_settle_books_the_spend() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(2);
    let before = remaining(&fake);

    assert_eq!(admit(&book, &state, &agent("d1", 1_000)), Decision::Granted);
    let lease = book.lease("d1").expect("a shared seat");
    assert_eq!(lease.epoch, 1);
    assert_eq!(lease.holder, Principal::node("node-a"));
    assert_eq!(
        remaining(&fake),
        before - 1_000,
        "the reservation is set aside"
    );

    {
        let mut admission = super::super::lock_state(&state);
        admission.settle("d1", Some(400), Instant::now());
        admission.release("d1", Instant::now());
    }
    book.sync(&state);
    assert!(book.lease("d1").is_none(), "the freed seat is released");
    assert_eq!(
        fake.spent().to_micros(),
        400,
        "the settle books what was spent"
    );
    assert_eq!(remaining(&fake), before - 400);
}

#[test]
fn an_unreachable_primary_turns_the_grant_into_a_wait_until_it_grants() {
    let fake = FakeState::new();
    let (book, lines) = book(Arc::new(fake.clone()));
    let state = admission(2);
    fake.set_degraded(Some("network down"));

    assert_eq!(
        admit(&book, &state, &agent("d1", 0)),
        Decision::Queued { position: 1 }
    );
    assert_eq!(
        admit(&book, &state, &agent("d1", 0)),
        Decision::Queued { position: 1 },
        "the next poll asks again and waits again"
    );
    assert!(book.lease("d1").is_none());
    let downgrades = lines
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains("the shared state did not grant the seat"))
        .count();
    assert_eq!(
        downgrades, 1,
        "one line per cause, not one per poll: {lines:?}"
    );

    fake.set_degraded(None);
    assert_eq!(admit(&book, &state, &agent("d1", 0)), Decision::Granted);
    assert!(book.lease("d1").is_some());
    assert!(lines
        .lock()
        .unwrap()
        .iter()
        .any(|line| line == "seat d1: shared seat granted"));
}

#[test]
fn an_exhausted_slice_refuses_the_grant_as_a_budget_and_the_id_may_ask_again() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(2);
    let slice = remaining(&fake);

    match admit(&book, &state, &agent("big", slice + 1)) {
        Decision::Refused { code, detail } => {
            assert_eq!(code, Refusal::BudgetExceeded);
            assert_eq!(
                detail,
                format!(
                    "this node's share of the shared budget is used up: this run needs {}, {} is \
                     left; raise this node's share on the shared primary",
                    crate::money::MicroUsd::from_micros(slice + 1),
                    crate::money::MicroUsd::from_micros(slice)
                )
            );
        }
        other => panic!("expected a budget refusal, got {other:?}"),
    }
    assert!(
        book.lease("big").is_none(),
        "the seat went back with the grant"
    );
    assert!(!super::super::lock_state(&state).has_dispatch("big"));
    assert_eq!(
        admit(&book, &state, &agent("big", 1)),
        Decision::Granted,
        "a revoked grant is not a finished id"
    );
}

#[test]
fn a_lease_found_lost_on_renew_is_not_taken_again() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(2);
    assert_eq!(admit(&book, &state, &agent("d1", 0)), Decision::Granted);

    fake.advance(crate::shared_state::fake::SEAT_TTL_SECS + 1);
    book.renew();
    assert!(book.is_lost("d1"));
    book.sync(&state);
    assert!(
        book.lease("d1").is_none(),
        "a re-acquire would defeat the fence"
    );
    assert!(book.is_lost("d1"));
}

#[test]
fn a_withdrawn_unclaimed_dispatch_settles_its_reservation_at_zero() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(1);
    let before = remaining(&fake);
    assert_eq!(admit(&book, &state, &agent("first", 10)), Decision::Granted);
    assert_eq!(
        admit(&book, &state, &agent("second", 20)),
        Decision::Queued { position: 1 }
    );
    {
        let mut admission = super::super::lock_state(&state);
        admission.settle("first", Some(10), Instant::now());
        admission.release("first", Instant::now());
    }
    // `second` drains in, unclaimed, and the sync takes its seat.
    book.sync(&state);
    assert!(book.lease("second").is_some());
    assert_eq!(remaining(&fake), before - 10 - 20);

    assert_eq!(
        super::super::lock_state(&state).withdraw("second", Instant::now()),
        WithdrawOutcome::Withdrawn
    );
    book.sync(&state);
    assert!(book.lease("second").is_none());
    assert_eq!(
        fake.spent().to_micros(),
        10,
        "nothing was spent for `second`"
    );
    assert_eq!(remaining(&fake), before - 10);
}

#[test]
fn a_restarted_coordinator_takes_its_adopted_seat_again_at_a_higher_epoch() {
    let fake = FakeState::new();
    let state = admission(2);
    let (first, _) = book(Arc::new(fake.clone()));
    assert_eq!(admit(&first, &state, &agent("d1", 0)), Decision::Granted);
    assert_eq!(first.lease("d1").expect("held").epoch, 1);

    // The daemon dies without releasing; the next one on this host syncs
    // the same seated dispatch under the same holder.
    let (second, _) = book(Arc::new(fake.clone()));
    second.sync(&state);
    assert_eq!(second.lease("d1").expect("taken again").epoch, 2);
    // The dead daemon's token is fenced.
    assert!(matches!(
        fake.renew_seat(&first.lease("d1").expect("old token")),
        Err(SharedStateError::LeaseLost { .. })
    ));
}

#[test]
fn another_nodes_holder_cannot_take_a_held_seat() {
    let fake = FakeState::new();
    let state = admission(2);
    let (mine, _) = book(Arc::new(fake.clone()));
    assert_eq!(admit(&mine, &state, &agent("d1", 0)), Decision::Granted);
    let theirs = SeatBook::new(Arc::new(fake.clone()), Principal::node("node-b"))
        .with_log(Arc::new(|_: &str| {}));
    theirs.sync(&state);
    assert!(theirs.lease("d1").is_none(), "a live holder keeps its seat");
    assert_eq!(mine.lease("d1").expect("still mine").epoch, 1);
}

#[test]
fn concurrent_syncs_acquire_each_seat_once() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(4);
    for id in ["a", "b", "c", "d"] {
        assert_eq!(
            super::super::lock_state(&state).request(&agent(id, 1), Instant::now()),
            Decision::Granted
        );
    }
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| book.sync(&state));
        }
    });
    for id in ["a", "b", "c", "d"] {
        assert_eq!(book.lease(id).expect("held").epoch, 1, "{id} acquired once");
    }
}

#[test]
fn local_state_grants_as_before() {
    let (book, lines) = book(Arc::new(LocalState::new()));
    let state = admission(1);
    assert_eq!(admit(&book, &state, &agent("d1", 5)), Decision::Granted);
    assert_eq!(
        admit(&book, &state, &agent("d2", 5)),
        Decision::Queued { position: 1 }
    );
    {
        let mut admission = super::super::lock_state(&state);
        admission.settle("d1", None, Instant::now());
        admission.release("d1", Instant::now());
    }
    book.sync(&state);
    assert_eq!(admit(&book, &state, &agent("d2", 5)), Decision::Granted);
    book.renew();
    book.release_all();
    assert!(lines.lock().unwrap().is_empty(), "{lines:?}");
}

/// Through the socket: a runner's admission against a coordinator whose
/// primary is unreachable reads `Queued`, then `Granted` once it is back.
#[test]
fn the_coordinator_answers_a_refused_shared_seat_as_queued() {
    use crate::admission::Gate;
    let dir = crate::test_support::short_temp_dir("seat-sock");
    let socket = dir.join("relais.sock");
    let fake = FakeState::new();
    fake.set_degraded(Some("network down"));
    let (coordinator, listener) = super::super::Coordinator::start(
        &socket,
        limits(2),
        None,
        crate::admission::DEFAULT_AGENT_LEASE_TTL,
        Arc::new(fake.clone()),
        Principal::node("node-a"),
    )
    .expect("start");
    let server = std::thread::spawn(move || coordinator.serve(listener));
    let gate = super::super::RemoteGate::new(socket.clone());
    gate.register_run(&RunRegistration {
        run_id: "run".into(),
        session_id: "tab".into(),
        budget_micros: None,
        max_agents: None,
        max_depth: None,
    })
    .expect("register");
    assert_eq!(
        gate.admit(&agent("d1", 0)).expect("answered"),
        Decision::Queued { position: 1 }
    );
    fake.set_degraded(None);
    assert_eq!(
        gate.admit(&agent("d1", 0)).expect("answered"),
        Decision::Granted
    );
    super::super::Client::new(socket)
        .request(&super::super::Request::Shutdown)
        .expect("shutdown");
    server.join().expect("joined").expect("served");
    assert!(
        matches!(
            fake.renew_seat(&LeaseToken {
                resource: crate::shared_state::ResourceId::Seat {
                    node: "fake".into(),
                    dispatch: "d1".into(),
                },
                holder: Principal::node("node-a"),
                epoch: 1,
                expires_at: None,
            }),
            Err(crate::shared_state::SharedStateError::LeaseLost { .. })
        ),
        "the seat went back when the daemon stopped"
    );
}

/// While the primary is down, one sync stops at its first unreachable
/// answer: three waiting settles cost one shared call and one line, not
/// three, and every one is booked once the primary is back.
#[test]
fn an_unreachable_primary_stops_the_sync_at_its_first_call() {
    let fake = FakeState::new();
    let (book, lines) = book(Arc::new(fake.clone()));
    let state = admission(3);
    for id in ["s1", "s2", "s3"] {
        assert_eq!(admit(&book, &state, &agent(id, 100)), Decision::Granted);
    }
    fake.set_degraded(Some("network down"));
    {
        let mut admission = super::super::lock_state(&state);
        for id in ["s1", "s2", "s3"] {
            admission.settle(id, Some(10), Instant::now());
            admission.release(id, Instant::now());
        }
    }
    book.sync(&state);
    let failed = |lines: &Arc<Mutex<Vec<String>>>| {
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| {
                line.contains("booking the spend failed")
                    || line.contains("giving the shared seat back failed")
            })
            .count()
    };
    assert_eq!(
        failed(&lines),
        1,
        "the pass stopped at the first call: {lines:?}"
    );
    assert!(
        book.lease("s3").is_some(),
        "a freed seat keeps its token while the primary is down"
    );

    fake.set_degraded(None);
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 30, "every settle is booked once");
    for id in ["s1", "s2", "s3"] {
        assert!(
            book.lease(id).is_none(),
            "{id} is released once the primary is back"
        );
    }
}

/// A settle the primary answers and refuses is dropped once with one line,
/// not retried at every sync; the next settle after it is still booked.
#[test]
fn a_settle_the_primary_refuses_is_dropped_once_and_the_next_one_books() {
    let fake = FakeState::new();
    let (book, lines) = book(Arc::new(fake.clone()));
    let state = admission(2);
    for id in ["r1", "r2"] {
        assert_eq!(admit(&book, &state, &agent(id, 100)), Decision::Granted);
    }
    fake.set_settles_refused(Some("reservation unknown to the primary"));
    {
        let mut admission = super::super::lock_state(&state);
        admission.settle("r1", Some(10), Instant::now());
        admission.release("r1", Instant::now());
    }
    book.sync(&state);
    book.sync(&state);
    let refused = |lines: &Arc<Mutex<Vec<String>>>| {
        lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| {
                line.contains("booking the spend was not accepted: refused: reservation unknown")
            })
            .count()
    };
    assert_eq!(refused(&lines), 1, "dropped once, not retried: {lines:?}");
    assert_eq!(
        fake.spent().to_micros(),
        0,
        "the refused settle is not booked"
    );

    fake.set_settles_refused(None);
    {
        let mut admission = super::super::lock_state(&state);
        admission.settle("r2", Some(20), Instant::now());
        admission.release("r2", Instant::now());
    }
    book.sync(&state);
    assert_eq!(refused(&lines), 1, "{lines:?}");
    assert_eq!(fake.spent().to_micros(), 20, "the next settle books");
}

/// A settle that fails in a way that is neither an outage nor a refusal is
/// kept, written once, and retried without stopping the pass.
#[test]
fn a_settle_with_another_failure_is_kept_and_retried_without_stopping_the_pass() {
    let fake = FakeState::new();
    let (book, lines) = book(Arc::new(fake.clone()));
    let state = admission(2);
    for id in ["c1", "c2"] {
        assert_eq!(admit(&book, &state, &agent(id, 100)), Decision::Granted);
    }
    fake.set_settles_conflict(true);
    {
        let mut admission = super::super::lock_state(&state);
        for id in ["c1", "c2"] {
            admission.settle(id, Some(10), Instant::now());
            admission.release(id, Instant::now());
        }
    }
    book.sync(&state);
    book.sync(&state);
    let failed = lines
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains("booking the spend failed"))
        .count();
    assert_eq!(
        failed, 2,
        "both settles were tried (the pass went on), each said once: {lines:?}"
    );
    assert_eq!(fake.spent().to_micros(), 0);

    fake.set_settles_conflict(false);
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 20, "both were kept and book now");
}
