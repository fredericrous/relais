//! The conformance suite every [`SharedState`] must pass: [`super::LocalState`]
//! and [`super::fake::FakeState`] here, the closed client in its own
//! repository through the `conformance` feature (an-t06, an-t13).
//!
//! [`run_all`] runs every check, each on a fresh implementation from
//! `make` and a fresh [`Fixture`] of node-local handles. Each check is also
//! public, so a failing one can be run alone. The properties are the ones
//! both a single node and a primary must hold; what only a primary does
//! (expiry, a degraded mode) is tested on the fake itself.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use super::types::{Principal, PublishRow, Slice};
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
    let checks: [(&str, Check); 9] = [
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
    let first = state
        .claim_ticket(&fixture.store, &claim(&key, &closed, &me, now))
        .expect("first claim");
    assert!(matches!(first.taken, Taken::Claimed { .. }));
    let first_lease = first.lease.expect("a claim carries its lease");
    let refused = state
        .claim_ticket(&fixture.store, &claim(&key, &closed, &other, now + 1))
        .expect("answered");
    assert!(
        matches!(refused.taken, Taken::Refused { .. }),
        "another holder is refused, got {:?}",
        refused.taken
    );
    assert!(refused.lease.is_none());
    let again = state
        .claim_ticket(&fixture.store, &claim(&key, &closed, &me, now + 2))
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
    let intent = state
        .resolve_decision_with_intent(
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
        state
            .claim_intent(ledger, &intent.intent_id, &first, &gone)
            .expect("claim"),
        Claim::Claimed(_)
    ));
    match state
        .claim_intent(
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
    assert!(state
        .release_claim(ledger, &intent.intent_id, &first, "never started")
        .expect("release"));
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
    let start = state.registry_pointer(registry).expect("read");
    state
        .swap_registry_pointer(registry, start.as_deref(), &a)
        .expect("swap from what was read");
    assert_eq!(
        state.registry_pointer(registry).expect("read").as_deref(),
        Some("art-a")
    );
    match state.swap_registry_pointer(registry, start.as_deref(), &b) {
        Err(SharedStateError::CasConflict { found, .. }) => {
            assert_eq!(found.as_deref(), Some("art-a"))
        }
        other => panic!("a stale expectation must be cas_conflict, got {other:?}"),
    }
    state
        .swap_registry_pointer(registry, Some("art-a"), &b)
        .expect("swap from the current value");
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
        state
            .daily_spent(&fixture.ledger, "2000-01-01T00:00:00+00:00")
            .expect("spent"),
        MicroUsd::ZERO
    );
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
                    super::super::LocalState::new()
                        .swap_registry_pointer(&registry, None, &evaluated)
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
            .filter(|r| matches!(r, Err(SharedStateError::CasConflict { .. })))
            .count();
        assert_eq!((landed, conflicts), (1, 1), "{results:?}");
    }
}
