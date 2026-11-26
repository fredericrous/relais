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
        .filter(|line| {
            line.contains("the shared state did not grant the seat")
                && line.ends_with(&format!(
                    "if it lasts, the primary's operator checks it ({PRIMARY_UNREAD})"
                ))
        })
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
                line.contains(
                    "booking the spend of $0.00001 was not accepted: refused: reservation unknown",
                )
            })
            .count()
    };
    assert_eq!(refused(&lines), 1, "dropped once, not retried: {lines:?}");
    assert!(
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.ends_with("(dropped_micros=10)")),
        "{lines:?}"
    );
    assert!(
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("the shared counter is short by")
                && line.contains(&format!(
                    "if it repeats, the primary's operator checks it ({PRIMARY_UNREAD})"
                ))
                && !line.contains("check the shared primary")),
        "{lines:?}"
    );
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
    fake.set_settles_conflict(Some("settle-slot"));
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

    fake.set_settles_conflict(None);
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 20, "both were kept and book now");
}

// ---- an-t24: every settle reaches the counter ----

/// A scripted wall clock the test moves.
fn scripted_clock(start: i64) -> (UnixClock, Arc<std::sync::atomic::AtomicI64>) {
    let now = Arc::new(std::sync::atomic::AtomicI64::new(start));
    let seen = Arc::clone(&now);
    (
        Arc::new(move || seen.load(std::sync::atomic::Ordering::SeqCst)),
        now,
    )
}

fn count_lines(lines: &Arc<Mutex<Vec<String>>>, needle: &str) -> usize {
    lines
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains(needle))
        .count()
}

/// A dispatch the book never saw, claimed and launched, whose reserve then
/// fails (its reservation is larger than the slice).
fn launched_without_reservation(state: &Mutex<AdmissionState>, id: &str) {
    let mut admission = super::super::lock_state(state);
    assert_eq!(
        admission.request(&agent(id, 10_000_000), Instant::now()),
        Decision::Granted
    );
    admission.bind(id, None, Some(4242), Instant::now(), &|_| true);
}

fn settle_and_free(state: &Mutex<AdmissionState>, id: &str, spent: i64) {
    let mut admission = super::super::lock_state(state);
    admission.settle(id, Some(spent), Instant::now());
    admission.release(id, Instant::now());
}

#[test]
fn a_reserve_that_failed_after_launch_still_books_its_spend_on_the_tick() {
    let fake = FakeState::new();
    let (book, lines) = book(Arc::new(fake.clone()));
    let state = admission(2);
    launched_without_reservation(&state, "d1");
    book.sync(&state);
    assert!(
        book.lease("d1").is_some(),
        "the running work keeps its seat"
    );
    assert_eq!(count_lines(&lines, "reserve failed"), 1, "{lines:?}");
    assert_eq!(
        count_lines(&lines, "this node only"),
        0,
        "the old wording is gone: {lines:?}"
    );

    settle_and_free(&state, "d1", 400);
    book.sync_request(&state, None);
    assert_eq!(
        fake.spent().to_micros(),
        0,
        "a request sync books no settle that has no reservation"
    );
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 400, "the tick books it");
    assert!(book.book().settles.is_empty());
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 400, "once");
}

#[test]
fn an_unreserved_settle_whose_first_settle_failed_books_once_on_the_same_reservation() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(2);
    launched_without_reservation(&state, "d1");
    book.sync(&state);
    settle_and_free(&state, "d1", 400);
    fake.set_settles_conflict(Some("slot"));
    book.sync(&state);
    let stored = book.book().settles[0]
        .reservation
        .clone()
        .expect("the reservation is stored before the settle is sent");
    assert_eq!(fake.spent().to_micros(), 0);

    // The primary applied that settle and its answer was lost: the retry
    // is on the same reservation, so the counter does not move again.
    fake.set_settles_conflict(None);
    fake.settle_spend(&stored, MicroUsd::from_micros(400))
        .expect("the primary applied it");
    assert_eq!(fake.spent().to_micros(), 400);
    book.sync(&state);
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 400, "booked once, not twice");
    assert!(stored.seat.resource.to_string().contains("d1"));
    assert!(book.book().settles.is_empty());
}

#[test]
fn a_settle_with_no_reservation_while_its_seat_is_held_elsewhere_is_kept() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(2);
    // The dispatch loses its own lease, so the settle needs a seat of its
    // own, and another holder has taken it.
    launched_without_reservation(&state, "d1");
    book.sync(&state);
    fake.advance(10_000);
    book.renew();
    assert!(book.is_lost("d1"));
    fake.acquire_seat(&Principal::node("node-b"), "settle:d1")
        .expect("another holder");
    settle_and_free(&state, "d1", 400);
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 0, "kept, not booked");
    assert_eq!(book.book().settles.len(), 1);

    fake.advance(10_000);
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 400, "booked once the seat frees");
}

fn file_book(
    shared: Arc<dyn SharedState>,
    path: &std::path::Path,
    clock: UnixClock,
) -> (SeatBook, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let book = SeatBook::new(shared, Principal::node("node-a"))
        .with_log(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string())
        }))
        .with_clock(clock)
        .with_settle_file(path.to_path_buf());
    (book, lines)
}

#[test]
fn a_restarted_book_settles_what_the_last_one_left_waiting() {
    let dir = crate::test_support::temp_dir("seat-settles-restart");
    let path = dir.join(SETTLES_FILE);
    let fake = FakeState::new();
    let (clock, _) = scripted_clock(1_000);
    let state = admission(3);
    let (first, _) = file_book(Arc::new(fake.clone()), &path, Arc::clone(&clock));
    for id in ["d1", "d2"] {
        assert_eq!(admit(&first, &state, &agent(id, 100)), Decision::Granted);
    }
    launched_without_reservation(&state, "d3");
    first.sync(&state);
    fake.set_settles_conflict(Some("slot"));
    for (id, spent) in [("d1", 10), ("d2", 20), ("d3", 30)] {
        settle_and_free(&state, id, spent);
    }
    first.sync(&state);
    assert!(path.exists(), "the waiting settles are on disk");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the rows carry seat tokens");
    }
    assert_eq!(fake.spent().to_micros(), 0);
    drop(first);

    fake.set_settles_conflict(None);
    let (second, _) = file_book(Arc::new(fake.clone()), &path, clock);
    assert_eq!(second.book().settles.len(), 3);
    second.sync(&admission(1));
    assert_eq!(fake.spent().to_micros(), 60, "the sum, once");
    assert!(!path.exists(), "an empty list leaves no file");
}

#[test]
fn a_sync_that_changes_nothing_writes_nothing() {
    let dir = crate::test_support::temp_dir("seat-settles-idle");
    let path = dir.join(SETTLES_FILE);
    let fake = FakeState::new();
    let (clock, _) = scripted_clock(1_000);
    let state = admission(2);
    let (book, _) = file_book(Arc::new(fake.clone()), &path, clock);
    assert_eq!(admit(&book, &state, &agent("d1", 100)), Decision::Granted);
    fake.set_settles_conflict(Some("slot"));
    settle_and_free(&state, "d1", 10);
    book.sync(&state);
    assert!(path.exists(), "the change was written");
    std::fs::remove_file(&path).expect("remove");
    book.sync(&state);
    book.sync_request(&state, None);
    assert!(
        !path.exists(),
        "a failing retry is not a change: nothing is written"
    );
}

#[test]
fn a_settle_past_the_age_cap_is_dropped_with_one_line() {
    let fake = FakeState::new();
    let (clock, now) = scripted_clock(1_000);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let book = SeatBook::new(Arc::new(fake.clone()), Principal::node("node-a"))
        .with_log(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string())
        }))
        .with_clock(clock);
    let state = admission(2);
    assert_eq!(admit(&book, &state, &agent("d1", 100)), Decision::Granted);
    fake.set_settles_conflict(Some("slot"));
    settle_and_free(&state, "d1", 1_500_000);
    book.sync(&state);
    assert_eq!(book.book().settles.len(), 1);

    now.store(
        1_000 + SETTLE_KEEP_SECS + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    book.sync(&state);
    assert!(book.book().settles.is_empty());
    assert_eq!(
        count_lines(&lines, "seat settles: dropped 1 waiting"),
        1,
        "{lines:?}"
    );
    assert_eq!(count_lines(&lines, "$1.5"), 1, "{lines:?}");
    assert_eq!(
        count_lines(&lines, "(dropped_micros=1500000)"),
        1,
        "{lines:?}"
    );
    assert_eq!(count_lines(&lines, "oldest dispatch: d1"), 1, "{lines:?}");
    // an-t31: the line says how short the counter is and where to read on,
    // never a check no command can make.
    assert_eq!(
        count_lines(&lines, "the shared counter is short by $1.5"),
        1,
        "{lines:?}"
    );
    assert_eq!(
        count_lines(&lines, &format!("({PRIMARY_UNREAD}) (dropped_micros=")),
        1,
        "{lines:?}"
    );
    assert_eq!(
        count_lines(&lines, "check the shared primary"),
        0,
        "{lines:?}"
    );
}

#[test]
fn more_than_the_count_cap_drops_the_oldest_in_one_line() {
    let fake = FakeState::new();
    let (clock, _) = scripted_clock(1_000);
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let book = SeatBook::new(Arc::new(fake.clone()), Principal::node("node-a"))
        .with_log(Arc::new(move |line: &str| {
            sink.lock().unwrap().push(line.to_string())
        }))
        .with_clock(clock);
    let state = admission(2);
    fake.set_degraded(Some("network down"));
    {
        let mut inner = book.book();
        for n in 0..(MAX_WAITING_SETTLES + 3) {
            inner.push_settle(
                format!("x{n}"),
                None,
                MicroUsd::from_micros(1_000_000),
                1_000,
            );
        }
    }
    book.sync(&state);
    assert_eq!(book.book().settles.len(), MAX_WAITING_SETTLES);
    assert_eq!(book.book().settles[0].dispatch, "x3", "the oldest went");
    assert_eq!(
        count_lines(&lines, "seat settles: dropped 3"),
        1,
        "{lines:?}"
    );
    assert_eq!(count_lines(&lines, "$3"), 1, "{lines:?}");
    assert_eq!(
        count_lines(&lines, "(dropped_micros=3000000)"),
        1,
        "{lines:?}"
    );
}

#[test]
fn an_unreadable_settle_file_is_kept_aside_and_the_book_starts_empty() {
    let dir = crate::test_support::temp_dir("seat-settles-bad");
    let path = dir.join(SETTLES_FILE);
    std::fs::write(&path, "not json").expect("write");
    let (clock, _) = scripted_clock(777);
    let (book, lines) = file_book(Arc::new(FakeState::new()), &path, clock);
    assert!(book.book().settles.is_empty());
    let aside = dir.join(format!("{SETTLES_FILE}.unreadable-777"));
    assert_eq!(std::fs::read_to_string(&aside).expect("kept"), "not json");
    assert_eq!(count_lines(&lines, "unreadable-777"), 1, "{lines:?}");
    assert_eq!(
        count_lines(&lines, "not booked to the shared counter"),
        1,
        "{lines:?}"
    );
}

#[test]
fn a_single_node_keeps_no_file_and_queues_no_unreserved_settle() {
    let dir = crate::test_support::temp_dir("seat-settles-local");
    let path = dir.join(SETTLES_FILE);
    let (clock, _) = scripted_clock(1_000);
    let (book, _) = file_book(Arc::new(LocalState::new()), &path, clock);
    let state = admission(2);
    assert_eq!(admit(&book, &state, &agent("d1", 100)), Decision::Granted);
    launched_without_reservation(&state, "d2");
    book.sync(&state);
    settle_and_free(&state, "d1", 10);
    settle_and_free(&state, "d2", 20);
    book.sync(&state);
    assert!(book.book().settles.is_empty());
    assert!(!path.exists());
}

/// Gap 5: the runner's `gate.settle` failed, so the dispatch was released
/// and never settled. The reconcile pass settles it unknown, which the
/// journal books at its reservation, once.
#[test]
fn a_dispatch_released_but_never_settled_books_its_reservation_once() {
    let fake = FakeState::new();
    let (book, _) = book(Arc::new(fake.clone()));
    let state = admission(2);
    assert_eq!(admit(&book, &state, &agent("d1", 1_000)), Decision::Granted);
    {
        let mut admission = super::super::lock_state(&state);
        admission.bind("d1", Some("agent"), None, Instant::now(), &|_| true);
        admission.release("d1", Instant::now());
    }
    let start = Instant::now();
    for hour in 1..=12 {
        let at = start + std::time::Duration::from_secs(3_600 * hour);
        super::super::lock_state(&state).reconcile(at, &|_| false);
        book.sync(&state);
    }
    assert_eq!(
        fake.spent().to_micros(),
        1_000,
        "booked at the reservation, once"
    );
}

/// A refused settle that was made on a seat of its own gives that seat back
/// at once, not at its time limit.
#[test]
fn a_refused_unreserved_settle_frees_its_own_seat() {
    let fake = FakeState::new();
    let (book, lines) = book(Arc::new(fake.clone()));
    let state = admission(2);
    launched_without_reservation(&state, "d1");
    book.sync(&state);
    fake.advance(10_000);
    book.renew();
    assert!(
        book.is_lost("d1"),
        "the dispatch has no seat of its own left"
    );
    settle_and_free(&state, "d1", 400);
    fake.set_settles_refused(Some("unknown reservation"));
    book.sync(&state);
    assert!(book.book().settles.is_empty(), "dropped");
    // The gap is named with its amount, so the 5% check can add it back.
    assert!(
        lines.lock().unwrap().iter().any(|line| line
            .contains("seat d1: booking the spend of $0.0004 was not accepted")
            && line.ends_with("(dropped_micros=400)")),
        "{lines:?}"
    );
    assert_eq!(
        fake.spent().to_micros(),
        0,
        "the refused settle is not booked"
    );
    fake.acquire_seat(&Principal::node("node-b"), "settle:d1")
        .expect("the settle's seat is free again");
}

/// The lost answer across a restart: the reservation was written before the
/// settle was sent, so a second book on the same file retries it on the same
/// reservation, and the counter holds the spend once.
#[test]
fn a_lost_settle_answer_is_retried_on_the_stored_reservation_after_a_restart() {
    let dir = crate::test_support::temp_dir("seat-settles-lost");
    let path = dir.join(SETTLES_FILE);
    let fake = FakeState::new();
    let (clock, _) = scripted_clock(1_000);
    let state = admission(2);
    let (first, _) = file_book(Arc::new(fake.clone()), &path, Arc::clone(&clock));
    launched_without_reservation(&state, "d1");
    first.sync(&state);
    settle_and_free(&state, "d1", 400);
    fake.set_settles_conflict(Some("slot"));
    first.sync(&state);
    let stored = first.book().settles[0]
        .reservation
        .clone()
        .expect("stored before the settle was sent");
    drop(first);

    // The primary applied the settle; its answer never reached the book.
    fake.set_settles_conflict(None);
    fake.settle_spend(&stored, MicroUsd::from_micros(400))
        .expect("applied");
    let (second, _) = file_book(Arc::new(fake.clone()), &path, clock);
    assert_eq!(
        second.book().settles[0]
            .reservation
            .as_ref()
            .map(|r| r.id.clone()),
        Some(stored.id.clone()),
        "the file carries the reservation"
    );
    second.sync(&admission(1));
    assert_eq!(fake.spent().to_micros(), 400, "once, not twice");
}

/// A file that cannot be read for a reason other than being absent is set
/// aside too, and never overwritten.
#[test]
fn a_settle_file_that_is_a_directory_is_set_aside_and_not_overwritten() {
    let dir = crate::test_support::temp_dir("seat-settles-dir");
    let path = dir.join(SETTLES_FILE);
    std::fs::create_dir(&path).expect("dir");
    let (clock, _) = scripted_clock(5);
    let (book, lines) = file_book(Arc::new(FakeState::new()), &path, clock);
    assert!(book.book().settles.is_empty());
    assert!(dir.join(format!("{SETTLES_FILE}.unreadable-5")).exists());
    assert_eq!(count_lines(&lines, "kept aside"), 1, "{lines:?}");
}

/// A reservation made for a settle is dropped when the file cannot be
/// written: the settle is not sent, and the next pass makes a new one.
#[test]
fn a_settle_is_not_sent_while_its_reservation_cannot_be_written() {
    let dir = crate::test_support::temp_dir("seat-settles-nowrite");
    let parent = dir.join("missing");
    let path = parent.join(SETTLES_FILE);
    let fake = FakeState::new();
    let (clock, _) = scripted_clock(1_000);
    let state = admission(2);
    let (book, _) = file_book(Arc::new(fake.clone()), &path, clock);
    launched_without_reservation(&state, "d1");
    book.sync(&state);
    settle_and_free(&state, "d1", 400);
    book.sync(&state);
    book.sync(&state);
    assert_eq!(
        fake.spent().to_micros(),
        0,
        "nothing sent without a written reservation"
    );
    assert!(book.book().settles[0].reservation.is_none());
    let probe = fake
        .acquire_seat(&Principal::node("node-b"), "settle:d1")
        .expect("the dropped reservation's seat went back");
    assert_eq!(
        probe.epoch, 1,
        "the book acquired no settle seat while it could not write"
    );
    fake.release_seat(&probe).expect("probe released");

    std::fs::create_dir(&parent).expect("dir");
    book.sync(&state);
    assert_eq!(
        fake.spent().to_micros(),
        400,
        "booked once the file can be written"
    );
}

/// The drop of an unsent reservation: the first write works (the settle is
/// stored with no reservation), then the directory goes, and the pass that
/// would reserve finds the write failing.
#[test]
fn a_reservation_made_just_before_the_file_fails_is_dropped_unsent() {
    let dir = crate::test_support::temp_dir("seat-settles-vanish");
    let parent = dir.join("state");
    std::fs::create_dir(&parent).expect("dir");
    let path = parent.join(SETTLES_FILE);
    let fake = FakeState::new();
    let (clock, _) = scripted_clock(1_000);
    let state = admission(2);
    let (book, _) = file_book(Arc::new(fake.clone()), &path, clock);
    launched_without_reservation(&state, "d1");
    book.sync(&state);
    settle_and_free(&state, "d1", 400);
    book.sync_request(&state, None);
    assert!(path.exists(), "stored with no reservation");
    assert!(book.book().settles[0].reservation.is_none());

    std::fs::remove_dir_all(&parent).expect("the directory goes");
    book.sync(&state);
    assert_eq!(fake.spent().to_micros(), 0, "nothing sent");
    assert!(book.book().settles[0].reservation.is_none());
    let probe = fake
        .acquire_seat(&Principal::node("node-b"), "settle:d1")
        .expect("the dropped reservation's seat is free");
    assert_eq!(probe.epoch, 2, "it was acquired and given back once");
}

/// A file that cannot be read and cannot be moved aside is left alone and
/// the book keeps nothing on disk.
#[test]
fn a_settle_file_that_cannot_be_moved_aside_is_never_overwritten() {
    let dir = crate::test_support::temp_dir("seat-settles-stuck");
    let path = dir.join(SETTLES_FILE);
    std::fs::create_dir(&path).expect("unreadable as a file");
    let aside = dir.join(format!("{SETTLES_FILE}.unreadable-9"));
    std::fs::create_dir(&aside).expect("the aside name is taken");
    std::fs::write(aside.join("keep"), "x").expect("make it non-empty");
    let (clock, _) = scripted_clock(9);
    let state = admission(2);
    let fake = FakeState::new();
    let (book, lines) = file_book(Arc::new(fake.clone()), &path, clock);
    assert_eq!(count_lines(&lines, "keeps nothing on disk"), 1, "{lines:?}");
    assert_eq!(admit(&book, &state, &agent("d1", 100)), Decision::Granted);
    fake.set_settles_conflict(Some("slot"));
    settle_and_free(&state, "d1", 10);
    book.sync(&state);
    assert!(path.is_dir(), "the unreadable file was not replaced");
}
