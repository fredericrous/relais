//! The conformance suite every [`SharedState`] must pass: [`super::LocalState`]
//! and [`super::fake::FakeState`] here, the closed client in its own
//! repository through the `conformance` feature (an-t06, an-t13).
//!
//! [`run_all`] runs every check, each on a fresh implementation from
//! `make` and a fresh [`Fixture`] of node-local handles. The checks that
//! write locally go through [`super::node`], as relais does: the trait
//! sees plain values, the fixture's handles stay with the check. Each check is also
//! public, so a failing one can be run alone. The properties are the ones
//! both a single node and a primary must hold; what only a primary does
//! (expiry, a degraded mode) is tested on the fake itself.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use super::node::{self, NodeError};
use super::types::{DecisionGrant, Principal, PublishRow, Slice, DECISION_GRANT_TTL_SECS};
use super::{SharedState, SharedStateError, TicketClaim};
use crate::ids::{RunId, TaskId};
use crate::learn::fixtures::{artifact, evaluation};
use crate::learn::registry::Registry;
use crate::ledger::{
    Actor, Claim, DecisionAnswer, Ledger, NewIntent, RecordedVia, SuccessorProcess, Transition,
};
use crate::lifecycle::{Reason, State};
use crate::money::MicroUsd;
use crate::programme::claim::{ClaimKey, Contest, Store, Taken};
use crate::test_support::TempDir;

/// The node-local handles a check passes: a ledger, a claim store and a
/// registry under one scratch directory, and a name unique in the process
/// (resources a [`super::LocalState`] fences are process-wide).
pub struct Fixture {
    pub name: String,
    pub ledger: Ledger,
    pub store: Store,
    pub registry: Registry,
    _dir: TempDir,
}

impl Fixture {
    pub fn new(check: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "conf-{check}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let dir = crate::test_support::temp_dir(&name);
        let ledger = Ledger::open(&dir.join("ledger.sqlite")).expect("a scratch ledger opens");
        let store = Store::in_state_dir(&dir);
        let registry = Registry::open(&dir.join("registry")).expect("a scratch registry opens");
        Self {
            name,
            ledger,
            store,
            registry,
            _dir: dir,
        }
    }
}

/// One property, checked on one implementation with one fixture.
pub type Check = fn(&dyn SharedState, &Fixture);

/// Every check, each on a fresh implementation and fixture.
pub fn run_all<S: SharedState>(make: impl Fn() -> S) {
    let checks: [(&str, Check); 12] = [
        ("epoch_rises_on_every_acquire", epoch_rises_on_every_acquire),
        ("a_stale_epoch_is_lease_lost", a_stale_epoch_is_lease_lost),
        (
            "a_late_settle_is_never_refused",
            a_late_settle_is_never_refused,
        ),
        (
            "a_reserve_beyond_the_slice_is_refused",
            a_reserve_beyond_the_slice_is_refused,
        ),
        ("a_ticket_has_one_holder", a_ticket_has_one_holder),
        ("an_intent_is_claimed_once", an_intent_is_claimed_once),
        (
            "a_registry_swap_compares_first",
            a_registry_swap_compares_first,
        ),
        ("a_rollback_compares_first", a_rollback_compares_first),
        (
            "a_session_gives_back_its_work_and_keeps_its_handoff",
            a_session_gives_back_its_work_and_keeps_its_handoff,
        ),
        (
            "a_handoff_retried_by_its_holder_is_reserved_again",
            a_handoff_retried_by_its_holder_is_reserved_again,
        ),
        ("publish_replays_idempotently", publish_replays_idempotently),
        (
            "a_fresh_state_is_online_and_empty",
            a_fresh_state_is_online_and_empty,
        ),
    ];
    for (name, check) in checks {
        let state = make();
        let fixture = Fixture::new(name);
        check(&state, &fixture);
    }
}

/// [`run_all`] and the checks that need the implementation's clock. A
/// client whose test primary has an injectable clock passes `advance`,
/// which moves the clock of the state it is given forward by that many
/// seconds. Every primary (an implementation that holds decision grants
/// and ticket holds) must pass it: it adds the checks [`super::LocalState`]
/// does not hold to.
pub fn run_all_with_clock<S: SharedState>(make: impl Fn() -> S, advance: impl Fn(&S, i64)) {
    run_all(&make);
    let state = make();
    let fixture = Fixture::new("a_decision_grant_lapses");
    a_decision_grant_lapses(&state, &fixture, &|secs| advance(&state, secs));
    let state = make();
    let fixture = Fixture::new("a_primary_refuses_another_holders_reservation");
    a_primary_refuses_another_holders_reservation(&state, &fixture);
}

fn holder(fixture: &Fixture, who: &str) -> Principal {
    Principal::task(format!("{who}-{}", fixture.name))
}

/// Release then re-acquire gives a higher epoch.
pub fn epoch_rises_on_every_acquire(state: &dyn SharedState, fixture: &Fixture) {
    let me = holder(fixture, "a");
    let dispatch = format!("{}-d", fixture.name);
    let first = state.acquire_seat(&me, &dispatch).expect("first acquire");
    state.release_seat(&first).expect("release");
    let second = state.acquire_seat(&me, &dispatch).expect("second acquire");
    assert!(
        second.epoch > first.epoch,
        "epoch {} after {}: every acquire bumps it",
        second.epoch,
        first.epoch
    );
    assert_eq!(second.holder, me);
}

/// A fenced call with an old token is `lease_lost`; the current one passes.
pub fn a_stale_epoch_is_lease_lost(state: &dyn SharedState, fixture: &Fixture) {
    let me = holder(fixture, "a");
    let dispatch = format!("{}-d", fixture.name);
    let old = state.acquire_seat(&me, &dispatch).expect("acquire");
    state.release_seat(&old).expect("release");
    let current = state.acquire_seat(&me, &dispatch).expect("re-acquire");
    for (call, result) in [
        ("renew", state.renew_seat(&old).map(|_| ())),
        ("release", state.release_seat(&old)),
        (
            "reserve",
            state
                .reserve_spend(&old, MicroUsd::from_micros(1))
                .map(|_| ()),
        ),
    ] {
        assert!(
            matches!(result, Err(SharedStateError::LeaseLost { .. })),
            "{call} with a stale epoch must be lease_lost, got {result:?}"
        );
    }
    state
        .renew_seat(&current)
        .expect("the current epoch renews");
}

/// Settling a reservation after its seat moved on books it anyway.
pub fn a_late_settle_is_never_refused(state: &dyn SharedState, fixture: &Fixture) {
    let me = holder(fixture, "a");
    let dispatch = format!("{}-d", fixture.name);
    let seat = state.acquire_seat(&me, &dispatch).expect("acquire");
    let reservation = state
        .reserve_spend(&seat, MicroUsd::from_micros(1_000))
        .expect("reserve inside any slice");
    state.release_seat(&seat).expect("release");
    let _newer = state.acquire_seat(&me, &dispatch).expect("re-acquire");
    state
        .settle_spend(&reservation, MicroUsd::from_micros(900))
        .expect("a settle records spent money; it is never refused");
}

/// A bounded slice refuses what it cannot cover; an unbounded one does not
/// refuse.
pub fn a_reserve_beyond_the_slice_is_refused(state: &dyn SharedState, fixture: &Fixture) {
    let me = holder(fixture, "a");
    let seat = state
        .acquire_seat(&me, &format!("{}-d", fixture.name))
        .expect("acquire");
    match state.slice().expect("slice") {
        Slice::Granted { remaining, .. } => {
            let over = MicroUsd::from_micros(remaining.to_micros() + 1);
            assert!(matches!(
                state.reserve_spend(&seat, over),
                Err(SharedStateError::SliceExhausted { .. })
            ));
            state
                .reserve_spend(&seat, remaining)
                .expect("the whole slice is reservable");
        }
        Slice::Unbounded => {
            state
                .reserve_spend(&seat, MicroUsd::from_micros(1_000_000))
                .expect("no slice, no refusal");
        }
    }
}

/// A second holder is refused; the first refreshes at a higher epoch.
pub fn a_ticket_has_one_holder(state: &dyn SharedState, fixture: &Fixture) {
    let key = ClaimKey {
        repo_key: fixture.name.clone(),
        slug: "programme".into(),
    };
    let closed = BTreeSet::new();
    let me = holder(fixture, "a");
    let other = holder(fixture, "b");
    fn claim<'a>(
        key: &'a ClaimKey,
        closed: &'a BTreeSet<String>,
        who: &'a Principal,
        now: i64,
    ) -> TicketClaim<'a> {
        TicketClaim {
            key,
            closed,
            id: "t1",
            holder: who,
            repo_display: "/r",
            contest: Contest::Refuse,
            now,
        }
    }
    let now = crate::programme::claim::now_unix();
    let first = node::claim_ticket(state, &fixture.store, &claim(&key, &closed, &me, now))
        .expect("first claim");
    assert!(matches!(first.taken, Taken::Claimed { .. }));
    let first_lease = first.lease.expect("a claim carries its lease");
    let refused = node::claim_ticket(
        state,
        &fixture.store,
        &claim(&key, &closed, &other, now + 1),
    )
    .expect("answered");
    assert!(
        matches!(refused.taken, Taken::Refused { .. }),
        "another holder is refused, got {:?}",
        refused.taken
    );
    assert!(refused.lease.is_none());
    state
        .release_ticket(&first_lease)
        .expect("a refused claim leaves the holder's lease current");
    let again = node::claim_ticket(state, &fixture.store, &claim(&key, &closed, &me, now + 2))
        .expect("refresh");
    assert!(matches!(again.taken, Taken::Claimed { .. }));
    assert!(again.lease.expect("lease").epoch > first_lease.epoch);
}

/// A decision's answer writes one intent; one run claims it, a second is
/// told who holds it, and the claim is given back before its run starts.
pub fn an_intent_is_claimed_once(state: &dyn SharedState, fixture: &Fixture) {
    let ledger = &fixture.ledger;
    let decided = RunId::from_stored("run-decided");
    ledger
        .insert_run(&decided, "/r", None, &TaskId::from_stored("task-a"), "rk")
        .expect("run row");
    ledger
        .record_transition(&Transition {
            run_id: decided.clone(),
            attempt_id: None,
            from_state: None,
            to_state: State::NeedsDecision,
            reason: "scope_exceeded".into(),
            detail: None,
            at: ledger.now(),
        })
        .expect("waiting");
    let answer = DecisionAnswer {
        resolution: Reason::DecisionRevised,
        actor: Actor::Orchestrator,
        person_quote: None,
        recorded_via: RecordedVia::Cli,
        note: None,
        successor_run: None,
        from_state: State::NeedsDecision,
        to_state: State::Failed,
    };
    let intent = node::resolve_decision_with_intent(
        state,
        ledger,
        &decided,
        &answer,
        &NewIntent {
            predecessor: decided.clone(),
            contract_digest: "digest".into(),
            contract_json: "{}".into(),
            scope_delta: Vec::new(),
            candidate: None,
            review: None,
        },
    )
    .expect("written")
    .expect("the decision was open");
    let gone = |_: u32| SuccessorProcess::Gone;
    let first = RunId::from_stored("run-first");
    assert!(matches!(
        node::claim_intent(state, ledger, &intent.intent_id, &first, &gone).expect("claim"),
        Claim::Claimed(_)
    ));
    match node::claim_intent(
        state,
        ledger,
        &intent.intent_id,
        &RunId::from_stored("run-second"),
        &gone,
    )
    .expect("answered")
    {
        Claim::AlreadyClaimed { run } => assert_eq!(run, first),
        other => panic!("a second run must be told the holder, got {other:?}"),
    }
    assert!(
        node::release_claim(state, ledger, &intent.intent_id, &first, "never started")
            .expect("release")
    );
}

/// The pointer swaps only from the value the caller expected.
pub fn a_registry_swap_compares_first(state: &dyn SharedState, fixture: &Fixture) {
    let registry = &fixture.registry;
    for id in ["art-a", "art-b"] {
        registry
            .store(&artifact(id, Some(evaluation(id, true))))
            .expect("store");
    }
    let a = registry.evaluated("art-a").expect("evidence holds");
    let b = registry.evaluated("art-b").expect("evidence holds");
    let start = node::registry_pointer(state, registry).expect("read");
    node::swap_registry_pointer(state, registry, start.as_deref(), &a)
        .expect("swap from what was read");
    assert_eq!(
        node::registry_pointer(state, registry)
            .expect("read")
            .as_deref(),
        Some("art-a")
    );
    match node::swap_registry_pointer(state, registry, start.as_deref(), &b) {
        Err(NodeError::Shared(SharedStateError::CasConflict { found, .. })) => {
            assert_eq!(found.as_deref(), Some("art-a"))
        }
        other => panic!("a stale expectation must be cas_conflict, got {other:?}"),
    }
    node::swap_registry_pointer(state, registry, Some("art-a"), &b)
        .expect("swap from the current value");
}

/// A rollback lands only from the artifact the caller judged, and then
/// the shared and the node's pointers both name the previous artifact.
pub fn a_rollback_compares_first(state: &dyn SharedState, fixture: &Fixture) {
    let registry = &fixture.registry;
    for id in ["art-a", "art-b"] {
        registry
            .store(&artifact(id, Some(evaluation(id, true))))
            .expect("store");
    }
    let start = node::registry_pointer(state, registry).expect("read");
    for (expected, id) in [(start.as_deref(), "art-a"), (Some("art-a"), "art-b")] {
        let evaluated = registry.evaluated(id).expect("evidence holds");
        node::swap_registry_pointer(state, registry, expected, &evaluated).expect("promote");
    }
    match node::roll_back_registry(state, registry, "art-a") {
        Err(NodeError::Shared(SharedStateError::CasConflict { found, .. })) => {
            assert_eq!(found.as_deref(), Some("art-b"))
        }
        other => panic!("a rollback judged against art-a must be cas_conflict, got {other:?}"),
    }
    assert_eq!(
        node::roll_back_registry(state, registry, "art-b")
            .expect("rollback from the current value")
            .as_deref(),
        Some("art-a")
    );
    assert_eq!(
        node::registry_pointer(state, registry)
            .expect("read")
            .as_deref(),
        Some("art-a")
    );
    assert_eq!(
        registry.active_id().expect("read").as_deref(),
        Some("art-a")
    );
    assert_eq!(
        node::roll_back_registry(state, registry, "art-a").expect("nothing to return to"),
        None,
        "the previous artifact is the active one now"
    );
}

fn ticket(fixture: &Fixture, id: &str, who: &Principal) -> super::TicketRequest {
    super::TicketRequest {
        repo_key: fixture.name.clone(),
        slug: "programme".into(),
        id: id.into(),
        holder: who.clone(),
        contest: Contest::Refuse,
    }
}

/// A session's end gives back its `work` holds and keeps its `handoff`
/// reservations; a reservation is not made over a held ticket, and the
/// next session's `work` claim takes a reservation over.
pub fn a_session_gives_back_its_work_and_keeps_its_handoff(
    state: &dyn SharedState,
    fixture: &Fixture,
) {
    use super::TicketAnswer;
    let me = holder(fixture, "a");
    let other = holder(fixture, "b");
    let held = |answer: &TicketAnswer| matches!(answer, TicketAnswer::Held { .. });
    // `work` on t1, a reservation on t2.
    let work = state
        .claim_ticket(&ticket(fixture, "t1", &me))
        .expect("claim");
    if let TicketAnswer::NodeDecides = work {
        state
            .grant_ticket(&ticket(fixture, "t1", &me))
            .expect("fence");
    }
    let reserved = state
        .reserve_handoff(&ticket(fixture, "t2", &me))
        .expect("reserve");
    assert!(
        !held(&reserved),
        "a free ticket is reserved, got {reserved:?}"
    );
    let over_work = state
        .reserve_handoff(&ticket(fixture, "t1", &other))
        .expect("answered");
    assert!(
        !matches!(over_work, TicketAnswer::Granted(_)),
        "a held ticket is never reserved, got {over_work:?}"
    );
    state.renew_tickets(&me).expect("renew");
    state.release_tickets(&me).expect("release");
    let after = state
        .claim_ticket(&ticket(fixture, "t1", &other))
        .expect("answered");
    assert!(
        !held(&after),
        "an ended session's work is free, got {after:?}"
    );
    let again = state
        .reserve_handoff(&ticket(fixture, "t2", &other))
        .expect("answered");
    assert!(
        !matches!(again, TicketAnswer::Granted(_)),
        "an ended session's reservation stays, got {again:?}"
    );
    let taken = state
        .claim_ticket(&ticket(fixture, "t2", &other))
        .expect("answered");
    assert!(
        !held(&taken),
        "the next session's work claim takes a reservation over, got {taken:?}"
    );
    state
        .release_tickets(&holder(fixture, "nobody"))
        .expect("nothing to give back");
}

/// A `handoff` reservation retried by its own holder is answered again,
/// and the node completes it; another holder's reservation of it is
/// refused while it lasts. The retry modelled is `next --claim` after a
/// crash between the shared grant and the claim file: the shared half
/// granted, no claim file was written. The case never retries the holder
/// at the node once the file exists (the ticket is held then, and
/// `next --claim` skips it).
pub fn a_handoff_retried_by_its_holder_is_reserved_again(
    state: &dyn SharedState,
    fixture: &Fixture,
) {
    use super::TicketAnswer;
    use crate::programme::claim::{now_unix, Holder, Kind, LOCK_WAIT};
    const CHECK: &str = "a_handoff_retried_by_its_holder_is_reserved_again";
    let me = holder(fixture, "a");
    let other = holder(fixture, "b");
    let first = state
        .reserve_handoff(&ticket(fixture, "t1", &me))
        .expect("reserve");
    assert!(
        !matches!(first, TicketAnswer::Held { .. }),
        "{CHECK} step 1: a free ticket is reserved, got {first:?}"
    );
    let retry = state
        .reserve_handoff(&ticket(fixture, "t1", &me))
        .expect("retry");
    assert!(
        !matches!(retry, TicketAnswer::Held { .. }),
        "{CHECK} step 2: a holder's own retry is never Held, got {retry:?}"
    );
    if let (TicketAnswer::Granted(first), TicketAnswer::Granted(retry)) = (&first, &retry) {
        assert!(
            retry.epoch > first.epoch,
            "{CHECK} step 2: a retry is an acquire and bumps the epoch, got {} after {}",
            retry.epoch,
            first.epoch
        );
    }
    let key = ClaimKey {
        repo_key: fixture.name.clone(),
        slug: "programme".into(),
    };
    let now = now_unix();
    {
        let lock = fixture.store.lock(&key, LOCK_WAIT, now).expect("lock");
        assert!(
            node::reserve_handoff(state, &lock, &key, "t1", &me, "/r", now).expect("answered"),
            "{CHECK} step 3: the node completes its holder's retry"
        );
        let holds = fixture.store.holds(&key, now).expect("holds");
        assert_eq!(
            holds.get("t1").map(|hold| &hold.holder),
            Some(&Holder::Session {
                id: me.id.clone(),
                kind: Kind::Handoff
            }),
            "{CHECK} step 3: the claim file names the holder's handoff"
        );
        assert!(
            !node::reserve_handoff(state, &lock, &key, "t1", &other, "/r", now).expect("answered"),
            "{CHECK} step 4: another holder's reservation is refused"
        );
    }
    let over = state
        .reserve_handoff(&ticket(fixture, "t1", &other))
        .expect("answered");
    match &over {
        TicketAnswer::Granted(_) => {
            panic!("{CHECK} step 5: another holder is never granted, got {over:?}")
        }
        TicketAnswer::Held { holder, .. } => {
            assert_eq!(holder, &me, "{CHECK} step 5: a refusal names the reserver")
        }
        TicketAnswer::NodeDecides => {}
    }
}

/// A primary's half of [`a_handoff_retried_by_its_holder_is_reserved_again`]:
/// the shared half grants the reservation and refuses another holder with
/// `Held`, naming the reserver, since the claim file of one node is not
/// seen by another. [`super::LocalState`] answers `NodeDecides` and is not
/// run through it.
pub fn a_primary_refuses_another_holders_reservation(state: &dyn SharedState, fixture: &Fixture) {
    use super::TicketAnswer;
    const CHECK: &str = "a_primary_refuses_another_holders_reservation";
    let me = holder(fixture, "a");
    let other = holder(fixture, "b");
    let first = state
        .reserve_handoff(&ticket(fixture, "t1", &me))
        .expect("reserve");
    assert!(
        matches!(first, TicketAnswer::Granted(_)),
        "{CHECK} step 1: a primary grants a free ticket, got {first:?}"
    );
    let retry = state
        .reserve_handoff(&ticket(fixture, "t1", &me))
        .expect("retry");
    assert!(
        matches!(retry, TicketAnswer::Granted(_)),
        "{CHECK} step 2: a primary grants its holder's retry, got {retry:?}"
    );
    let over = state
        .reserve_handoff(&ticket(fixture, "t1", &other))
        .expect("answered");
    match &over {
        TicketAnswer::Held { holder, .. } => {
            assert_eq!(holder, &me, "{CHECK} step 5: a refusal names the reserver")
        }
        _ => panic!("{CHECK} step 5: a primary refuses another holder with Held, got {over:?}"),
    }
}

/// Publishing the same rows twice names the same high-water mark, and
/// every row is accounted for once each time.
pub fn publish_replays_idempotently(state: &dyn SharedState, fixture: &Fixture) {
    let rows: Vec<PublishRow> = (1..=3)
        .map(|seq| PublishRow {
            node: fixture.name.clone(),
            seq,
            table: "transitions".into(),
            payload: format!("{{\"n\":{seq}}}"),
        })
        .collect();
    let first = state.publish(&rows).expect("publish");
    let again = state.publish(&rows).expect("replay");
    for receipt in [first, again] {
        assert_eq!(receipt.through_seq, Some(3));
        assert_eq!(
            receipt.accepted + receipt.duplicates + receipt.queued,
            3,
            "every row is accounted for once: {receipt:?}"
        );
    }
    state
        .announce_schema(&fixture.name, crate::ledger::LEDGER_SCHEMA_VERSION)
        .expect("announce");
}

/// A fresh state talks to its primary, queues nothing and has spent
/// nothing today on a fresh ledger.
pub fn a_fresh_state_is_online_and_empty(state: &dyn SharedState, fixture: &Fixture) {
    assert_eq!(state.mode(), super::Mode::Online);
    assert_eq!(state.queued().expect("depth"), 0);
    assert_eq!(
        node::daily_spent(state, &fixture.ledger, "2000-01-01T00:00:00+00:00").expect("spent"),
        MicroUsd::ZERO
    );
}

/// A decision grant that is held is exclusive until [`DECISION_GRANT_TTL_SECS`]
/// have passed, and lapses then when its answer never lands. This is for an
/// implementation that holds grants (a primary): [`super::LocalState`]
/// holds none and is not run through it. `advance` moves the
/// implementation's clock forward by that many seconds. The primary is
/// fresh for each check (`run_all_with_clock` calls `make`); a check that
/// panics leaves its grant held.
pub fn a_decision_grant_lapses(state: &dyn SharedState, fixture: &Fixture, advance: &dyn Fn(i64)) {
    let run = RunId::from_stored(format!("run-grant-{}", fixture.name));
    assert_eq!(
        state.answer_decision(&run).expect("first ask"),
        DecisionGrant::Granted
    );
    assert_eq!(
        state.answer_decision(&run).expect("second ask"),
        DecisionGrant::HeldElsewhere,
        "a held grant is exclusive until it is released or lapses"
    );
    advance(DECISION_GRANT_TTL_SECS - 1);
    assert_eq!(
        state
            .answer_decision(&run)
            .expect("ask just before the lapse"),
        DecisionGrant::HeldElsewhere,
        "a grant is held until {DECISION_GRANT_TTL_SECS} s have passed"
    );
    advance(2);
    assert_eq!(
        state.answer_decision(&run).expect("ask after the lapse"),
        DecisionGrant::Granted,
        "a grant whose answer never landed lapses after {DECISION_GRANT_TTL_SECS} s"
    );
    state.release_decision(&run).expect("release");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_state_conforms() {
        run_all(super::super::LocalState::new);
    }

    #[test]
    fn the_fake_conforms() {
        run_all(super::super::fake::FakeState::new);
    }

    #[test]
    fn the_fake_conforms_with_its_clock() {
        run_all_with_clock(
            super::super::fake::FakeState::new,
            |state: &super::super::fake::FakeState, secs| state.advance(secs),
        );
    }

    /// The check is not satisfied by holding nothing: `LocalState` grants
    /// every ask, so the second ask is not `HeldElsewhere`.
    #[test]
    #[should_panic(expected = "a held grant is exclusive")]
    fn the_lapse_check_fails_an_implementation_that_never_holds() {
        let fixture = Fixture::new("never-holds");
        a_decision_grant_lapses(&super::super::LocalState::new(), &fixture, &|_| {});
    }

    /// Nor by lapsing early: a clock that runs twice as fast ends the grant
    /// before its TTL.
    #[test]
    #[should_panic(expected = "held until")]
    fn the_lapse_check_fails_a_grant_that_lapses_early() {
        let fixture = Fixture::new("lapses-early");
        let fake = super::super::fake::FakeState::new();
        a_decision_grant_lapses(&fake, &fixture, &|secs| fake.advance(secs * 2));
    }

    /// Nor by holding forever: a clock that does not move never lapses.
    #[test]
    #[should_panic(expected = "lapses after")]
    fn the_lapse_check_fails_a_grant_that_never_lapses() {
        let fixture = Fixture::new("never-lapses");
        let fake = super::super::fake::FakeState::new();
        a_decision_grant_lapses(&fake, &fixture, &|_| {});
    }

    /// Two swaps racing from the same expectation: exactly one lands. Each
    /// thread opens its own lock file description, as two processes would.
    #[test]
    fn concurrent_local_swaps_land_exactly_once() {
        let dir = crate::test_support::temp_dir("conf-race");
        let registry_dir = dir.join("registry");
        let registry = Registry::open(&registry_dir).expect("registry");
        for id in ["art-x", "art-y"] {
            registry
                .store(&artifact(id, Some(evaluation(id, true))))
                .expect("store");
        }
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["art-x", "art-y"]
            .into_iter()
            .map(|id| {
                let registry_dir = registry_dir.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    // Its own registry handle and its own lock file
                    // description, as a second process would have.
                    let registry = Registry::open(&registry_dir).expect("registry");
                    let evaluated = registry.evaluated(id).expect("evidence");
                    barrier.wait();
                    node::swap_registry_pointer(
                        &super::super::LocalState::new(),
                        &registry,
                        None,
                        &evaluated,
                    )
                })
            })
            .collect();
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();
        let landed = results.iter().filter(|r| r.is_ok()).count();
        let conflicts = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(NodeError::Shared(SharedStateError::CasConflict { .. }))
                )
            })
            .count();
        assert_eq!((landed, conflicts), (1, 1), "{results:?}");
    }
}
