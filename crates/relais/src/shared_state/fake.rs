//! An in-memory [`SharedState`] with an external primary's semantics
//! (an-t10, an-t11): leases that expire on its own clock, a bounded spend
//! slice, a degraded mode in which claims fail closed and publish rows
//! queue, and compare-and-set claims and registry pointer. It is not shaped
//! after [`super::LocalState`]: the conformance suite runs both, so a
//! property that holds for one only would show.
//!
//! It is the shared half only, as the trait is: the node-local writes (the
//! ledger's decision and intent rows, the claim files, the registry's
//! pointer files) are [`super::node`]'s, on the caller's handles.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use super::types::{
    DecisionGrant, IntentAnswer, LeaseToken, Mode, Pointer, Principal, PublishReceipt, PublishRow,
    Reservation, ResourceId, Settlement, Slice, Swap, TicketAnswer, TicketRequest,
    DECISION_GRANT_TTL_SECS,
};
use super::{Selection, SharedState, SharedStateError};
use crate::ids::RunId;
use crate::money::MicroUsd;
use crate::policy::MachineSettings;

/// A seat lease's time to live (an-t11's first value).
pub const SEAT_TTL_SECS: i64 = 90;
/// A ticket claim's time to live for a pod (an-t11).
pub const TICKET_TTL_SECS: i64 = 30 * 60;

#[derive(Debug, Clone)]
struct Row {
    holder: Option<Principal>,
    epoch: u64,
    expires_at: i64,
    claimed_at: i64,
    /// A `handoff` reservation, not a `work` hold.
    handoff: bool,
}

#[derive(Debug, Default)]
struct Inner {
    rows: HashMap<ResourceId, Row>,
    /// Remaining slice, micro-USD.
    slice: i64,
    /// The global counter: what every settle booked.
    spent: i64,
    settled: BTreeSet<String>,
    degraded: Option<String>,
    published: BTreeMap<(String, u64), PublishRow>,
    queue: Vec<PublishRow>,
    pointer: Option<String>,
    schema: BTreeMap<String, u64>,
    /// Successor intents: which run holds each.
    intents: HashMap<String, RunId>,
    /// Decision grants by run, with the time each was granted: a grant
    /// whose answer never landed lapses after [`DECISION_GRANT_TTL_SECS`].
    answered: HashMap<String, i64>,
    /// When set, every release is refused with this reason (a test knob).
    releases_fail: Option<String>,
    /// When set, the session-wide renew and release sleep this long first,
    /// as a primary that does not answer would (a test knob).
    slow: Option<std::time::Duration>,
}

/// The in-memory primary and its client in one.
#[derive(Debug, Clone)]
pub struct FakeState {
    inner: Arc<Mutex<Inner>>,
    clock: Arc<AtomicI64>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeState {
    /// A fresh primary at a fixed time, with a 5 USD slice.
    pub fn new() -> Self {
        let inner = Inner {
            slice: 5_000_000,
            ..Inner::default()
        };
        Self {
            inner: Arc::new(Mutex::new(inner)),
            clock: Arc::new(AtomicI64::new(1_800_000_000)),
        }
    }

    /// Move the primary's clock forward.
    pub fn advance(&self, secs: i64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    /// Lose (`Some(reason)`) or regain (`None`) the primary. Regaining it
    /// replays the queue, oldest first.
    pub fn set_degraded(&self, reason: Option<&str>) {
        let mut inner = self.lock();
        inner.degraded = reason.map(str::to_string);
        if inner.degraded.is_none() {
            let queue = std::mem::take(&mut inner.queue);
            for row in queue {
                inner
                    .published
                    .entry((row.node.clone(), row.seq))
                    .or_insert(row);
            }
        }
    }

    /// Refuse (`Some(reason)`) or allow (`None`) every release from now on,
    /// so a test sees what a release that fails after a local refusal does.
    pub fn set_releases_fail(&self, reason: Option<&str>) {
        self.lock().releases_fail = reason.map(str::to_string);
    }

    /// Make the session-wide renew and release take `delay` (`None`: answer
    /// at once), so a test sees what a caller with a deadline does.
    pub fn set_slow(&self, delay: Option<std::time::Duration>) {
        self.lock().slow = delay;
    }

    fn wait_if_slow(&self) {
        let delay = self.lock().slow;
        if let Some(delay) = delay {
            std::thread::sleep(delay);
        }
    }

    /// The run holding `intent_id`, if any.
    pub fn intent_holder(&self, intent_id: &str) -> Option<RunId> {
        self.lock().intents.get(intent_id).cloned()
    }

    /// Rows the sink holds.
    pub fn published_count(&self) -> usize {
        self.lock().published.len()
    }

    /// Set the global counter, as if other nodes had settled that much
    /// (a test knob: nothing in the runner feeds it until an-t17).
    pub fn set_spent(&self, amount: MicroUsd) {
        self.lock().spent = amount.to_micros();
    }

    /// What every settle booked against the global counter.
    pub fn spent(&self) -> MicroUsd {
        MicroUsd::from_micros(self.lock().spent)
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Online, and releases are not refused by the test knob.
    fn may_release(&self) -> Result<(), SharedStateError> {
        self.online()?;
        match &self.lock().releases_fail {
            Some(reason) => Err(SharedStateError::Refused {
                reason: reason.clone(),
            }),
            None => Ok(()),
        }
    }

    fn online(&self) -> Result<(), SharedStateError> {
        match &self.lock().degraded {
            Some(reason) => Err(SharedStateError::Unreachable {
                detail: reason.clone(),
            }),
            None => Ok(()),
        }
    }

    /// Take `resource` for `holder`: refused while another holder's row is
    /// live; else the epoch bumps.
    fn acquire(
        &self,
        resource: ResourceId,
        holder: &Principal,
        ttl: i64,
    ) -> Result<LeaseToken, SharedStateError> {
        match self.try_acquire(resource.clone(), holder, ttl)? {
            Ok(lease) => Ok(lease),
            Err(_held) => Err(SharedStateError::ClaimLost { resource }),
        }
    }

    /// [`Self::acquire`], answering the live holder and its age when
    /// refused, read under the same lock as the refusal.
    fn try_acquire(
        &self,
        resource: ResourceId,
        holder: &Principal,
        ttl: i64,
    ) -> Result<Result<LeaseToken, (Principal, u64)>, SharedStateError> {
        self.online()?;
        let now = self.now();
        let mut inner = self.lock();
        let row = inner.rows.entry(resource.clone()).or_insert(Row {
            holder: None,
            epoch: 0,
            expires_at: 0,
            claimed_at: now,
            handoff: false,
        });
        if let Some(current) = &row.holder {
            if current != holder && row.expires_at > now {
                return Ok(Err((current.clone(), (now - row.claimed_at).max(0) as u64)));
            }
        }
        if row.holder.as_ref() != Some(holder) || row.expires_at <= now {
            row.claimed_at = now;
        }
        row.epoch += 1;
        row.holder = Some(holder.clone());
        row.expires_at = now + ttl;
        Ok(Ok(LeaseToken {
            resource,
            holder: holder.clone(),
            epoch: row.epoch,
            expires_at: Some(row.expires_at),
        }))
    }

    fn fence(&self, lease: &LeaseToken) -> Result<(), SharedStateError> {
        let now = self.now();
        let inner = self.lock();
        match inner.rows.get(&lease.resource) {
            Some(row)
                if row.epoch == lease.epoch
                    && row.holder.as_ref() == Some(&lease.holder)
                    && row.expires_at > now =>
            {
                Ok(())
            }
            _ => Err(SharedStateError::LeaseLost {
                resource: lease.resource.clone(),
                epoch: lease.epoch,
            }),
        }
    }
}

impl SharedState for FakeState {
    fn selection(&self) -> Selection {
        Selection::External
    }

    fn machine_settings(&self) -> Result<MachineSettings, SharedStateError> {
        MachineSettings::from_toml_str(&format!(
            "schema_version = {}\n",
            crate::policy::MACHINE_SCHEMA_VERSION
        ))
        .map_err(|cause| SharedStateError::Refused {
            reason: cause.to_string(),
        })
    }

    fn daily_spent(&self, _day_start: &str) -> Result<Option<MicroUsd>, SharedStateError> {
        Ok(Some(self.spent()))
    }

    fn slice(&self) -> Result<Slice, SharedStateError> {
        let now = self.now();
        Ok(Slice::Granted {
            remaining: MicroUsd::from_micros(self.lock().slice),
            valid_until: now - now.rem_euclid(86_400) + 86_400,
        })
    }

    fn reserve_spend(
        &self,
        seat: &LeaseToken,
        amount: MicroUsd,
    ) -> Result<Reservation, SharedStateError> {
        self.fence(seat)?;
        let mut inner = self.lock();
        let requested = amount.to_micros();
        if requested > inner.slice {
            return Err(SharedStateError::SliceExhausted {
                requested: amount,
                remaining: MicroUsd::from_micros(inner.slice),
            });
        }
        inner.slice -= requested;
        Ok(Reservation {
            id: format!("{}#{}", seat.resource, seat.epoch),
            seat: seat.clone(),
            amount,
        })
    }

    fn settle_spend(
        &self,
        reservation: &Reservation,
        actual: MicroUsd,
    ) -> Result<Settlement, SharedStateError> {
        let current = self.fence(&reservation.seat).is_ok();
        let mut inner = self.lock();
        // Idempotent by reservation: a replayed settle books nothing twice.
        if inner.settled.insert(reservation.id.clone()) {
            inner.spent += actual.to_micros();
            inner.slice += reservation.amount.to_micros() - actual.to_micros();
        }
        Ok(if current {
            Settlement::Booked
        } else {
            Settlement::TrueUp
        })
    }

    fn acquire_seat(
        &self,
        holder: &Principal,
        dispatch: &str,
    ) -> Result<LeaseToken, SharedStateError> {
        self.acquire(
            ResourceId::Seat {
                node: "fake".into(),
                dispatch: dispatch.into(),
            },
            holder,
            SEAT_TTL_SECS,
        )
    }

    fn renew_seat(&self, lease: &LeaseToken) -> Result<LeaseToken, SharedStateError> {
        self.online()?;
        self.fence(lease)?;
        let now = self.now();
        let mut inner = self.lock();
        let row = inner
            .rows
            .get_mut(&lease.resource)
            .expect("a fenced lease has a row");
        row.expires_at = now + SEAT_TTL_SECS;
        Ok(LeaseToken {
            expires_at: Some(row.expires_at),
            ..lease.clone()
        })
    }

    fn release_seat(&self, lease: &LeaseToken) -> Result<(), SharedStateError> {
        self.online()?;
        self.fence(lease)?;
        if let Some(row) = self.lock().rows.get_mut(&lease.resource) {
            row.holder = None;
            row.expires_at = 0;
        }
        Ok(())
    }

    fn claim_ticket(&self, request: &TicketRequest) -> Result<TicketAnswer, SharedStateError> {
        let resource = request.resource();
        // A handoff reservation is taken over by the next session's work
        // claim, whoever reserved it.
        if let Some(row) = self.lock().rows.get_mut(&resource) {
            if row.handoff {
                row.holder = None;
                row.expires_at = 0;
            }
        }
        Ok(
            match self.try_acquire(resource.clone(), &request.holder, TICKET_TTL_SECS)? {
                Ok(lease) => {
                    if let Some(row) = self.lock().rows.get_mut(&resource) {
                        row.handoff = false;
                    }
                    TicketAnswer::Granted(lease)
                }
                Err((holder, age_secs)) => TicketAnswer::Held { holder, age_secs },
            },
        )
    }

    fn reserve_handoff(&self, request: &TicketRequest) -> Result<TicketAnswer, SharedStateError> {
        self.online()?;
        let now = self.now();
        let resource = request.resource();
        // A held ticket is never reserved, except again by the holder of
        // its own reservation (a retried `next --claim` whose claim file
        // was never written): the claim file refuses the rest the same.
        if let Some(row) = self.lock().rows.get(&resource) {
            if let Some(holder) = &row.holder {
                let own_reservation = row.handoff && *holder == request.holder;
                if row.expires_at > now && !own_reservation {
                    return Ok(TicketAnswer::Held {
                        holder: holder.clone(),
                        age_secs: (now - row.claimed_at).max(0) as u64,
                    });
                }
            }
        }
        let ttl = crate::programme::claim::HANDOFF_HOLD_SECS;
        Ok(
            match self.try_acquire(resource.clone(), &request.holder, ttl)? {
                Ok(lease) => {
                    if let Some(row) = self.lock().rows.get_mut(&resource) {
                        row.handoff = true;
                    }
                    TicketAnswer::Granted(lease)
                }
                Err((holder, age_secs)) => TicketAnswer::Held { holder, age_secs },
            },
        )
    }

    fn renew_tickets(&self, holder: &Principal) -> Result<(), SharedStateError> {
        self.wait_if_slow();
        self.online()?;
        let now = self.now();
        for (resource, row) in self.lock().rows.iter_mut() {
            if matches!(resource, ResourceId::Ticket { .. })
                && !row.handoff
                && row.holder.as_ref() == Some(holder)
                && row.expires_at > now
            {
                row.expires_at = now + TICKET_TTL_SECS;
            }
        }
        Ok(())
    }

    fn release_tickets(&self, holder: &Principal) -> Result<(), SharedStateError> {
        self.wait_if_slow();
        self.may_release()?;
        for (resource, row) in self.lock().rows.iter_mut() {
            if matches!(resource, ResourceId::Ticket { .. })
                && !row.handoff
                && row.holder.as_ref() == Some(holder)
            {
                row.holder = None;
                row.expires_at = 0;
            }
        }
        Ok(())
    }

    fn grant_ticket(&self, request: &TicketRequest) -> Result<LeaseToken, SharedStateError> {
        // A primary never answers `NodeDecides`, so nothing calls this; it
        // grants as a claim would.
        self.acquire(request.resource(), &request.holder, TICKET_TTL_SECS)
    }

    fn release_ticket(&self, lease: &LeaseToken) -> Result<(), SharedStateError> {
        self.may_release()?;
        self.fence(lease)?;
        if let Some(row) = self.lock().rows.get_mut(&lease.resource) {
            row.holder = None;
            row.expires_at = 0;
        }
        Ok(())
    }

    fn claim_intent(
        &self,
        intent_id: &str,
        run: &RunId,
        replacing: Option<&RunId>,
    ) -> Result<IntentAnswer, SharedStateError> {
        // Fails closed offline: two nodes must not both launch it (an-t10).
        self.online()?;
        let mut inner = self.lock();
        match inner.intents.get(intent_id) {
            Some(holder) if holder != run && Some(holder) != replacing => {
                Ok(IntentAnswer::HeldBy {
                    run: holder.clone(),
                })
            }
            _ => {
                inner.intents.insert(intent_id.to_string(), run.clone());
                Ok(IntentAnswer::Granted)
            }
        }
    }

    fn release_intent(&self, intent_id: &str, run: &RunId) -> Result<(), SharedStateError> {
        self.may_release()?;
        let mut inner = self.lock();
        if inner.intents.get(intent_id) == Some(run) {
            inner.intents.remove(intent_id);
        }
        Ok(())
    }

    fn answer_decision(&self, run: &RunId) -> Result<DecisionGrant, SharedStateError> {
        self.online()?;
        let now = self.now();
        let mut inner = self.lock();
        match inner.answered.get(run.as_str()) {
            Some(at) if now < at + DECISION_GRANT_TTL_SECS => Ok(DecisionGrant::HeldElsewhere),
            _ => {
                inner.answered.insert(run.to_string(), now);
                Ok(DecisionGrant::Granted)
            }
        }
    }

    fn release_decision(&self, run: &RunId) -> Result<(), SharedStateError> {
        self.may_release()?;
        self.lock().answered.remove(run.as_str());
        Ok(())
    }

    fn registry_pointer(&self) -> Result<Pointer, SharedStateError> {
        // A cached read is served offline (an-t10).
        Ok(Pointer::Shared(self.lock().pointer.clone()))
    }

    fn swap_registry_pointer(
        &self,
        expected: Option<&str>,
        to: Option<&str>,
    ) -> Result<Swap, SharedStateError> {
        self.online()?;
        let mut inner = self.lock();
        if inner.pointer.as_deref() != expected {
            return Err(SharedStateError::CasConflict {
                expected: expected.map(str::to_string),
                found: inner.pointer.clone(),
            });
        }
        inner.pointer = to.map(str::to_string);
        Ok(Swap::Swapped)
    }

    fn announce_schema(&self, node: &str, version: u64) -> Result<(), SharedStateError> {
        self.online()?;
        self.lock().schema.insert(node.to_string(), version);
        Ok(())
    }

    fn publish(&self, rows: &[PublishRow]) -> Result<PublishReceipt, SharedStateError> {
        let mut inner = self.lock();
        let mut receipt = PublishReceipt::default();
        for row in rows {
            let key = (row.node.clone(), row.seq);
            let known = inner.published.contains_key(&key)
                || inner
                    .queue
                    .iter()
                    .any(|queued| (queued.node.as_str(), queued.seq) == (key.0.as_str(), key.1));
            if known {
                receipt.duplicates += 1;
            } else if inner.degraded.is_some() {
                inner.queue.push(row.clone());
                receipt.queued += 1;
            } else {
                inner.published.insert(key, row.clone());
                receipt.accepted += 1;
            }
        }
        receipt.through_seq = rows.iter().map(|row| row.seq).max();
        Ok(receipt)
    }

    fn queued(&self) -> Result<u64, SharedStateError> {
        Ok(self.lock().queue.len() as u64)
    }

    fn mode(&self) -> Mode {
        match &self.lock().degraded {
            Some(reason) => Mode::Degraded {
                reason: reason.clone(),
            },
            None => Mode::Online,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seat(state: &FakeState, dispatch: &str) -> LeaseToken {
        state
            .acquire_seat(&Principal::task("pod-a"), dispatch)
            .expect("seat")
    }

    #[test]
    fn a_seat_past_its_ttl_is_lost_and_its_late_settle_is_a_true_up() {
        let state = FakeState::new();
        let lease = seat(&state, "d1");
        let reservation = state
            .reserve_spend(&lease, MicroUsd::from_micros(1_000))
            .expect("reserve");
        state.advance(SEAT_TTL_SECS + 1);
        assert!(matches!(
            state.renew_seat(&lease),
            Err(SharedStateError::LeaseLost { .. })
        ));
        assert_eq!(
            state
                .settle_spend(&reservation, MicroUsd::from_micros(800))
                .expect("never refused"),
            Settlement::TrueUp
        );
        assert_eq!(state.spent(), MicroUsd::from_micros(800));
        // Replayed: booked once.
        state
            .settle_spend(&reservation, MicroUsd::from_micros(800))
            .expect("replay");
        assert_eq!(state.spent(), MicroUsd::from_micros(800));
    }

    #[test]
    fn another_holder_waits_for_expiry_then_takes_the_seat_at_a_new_epoch() {
        let state = FakeState::new();
        let first = seat(&state, "d2");
        let other = Principal::task("pod-b");
        assert!(matches!(
            state.acquire_seat(&other, "d2"),
            Err(SharedStateError::ClaimLost { .. })
        ));
        state.advance(SEAT_TTL_SECS + 1);
        let second = state.acquire_seat(&other, "d2").expect("free after ttl");
        assert!(second.epoch > first.epoch);
        assert!(matches!(
            state.release_seat(&first),
            Err(SharedStateError::LeaseLost { .. })
        ));
    }

    #[test]
    fn degraded_claims_fail_closed_and_publish_rows_queue_then_replay_once() {
        let state = FakeState::new();
        state.set_degraded(Some("primary down"));
        assert!(matches!(
            state.acquire_seat(&Principal::task("pod-a"), "d3"),
            Err(SharedStateError::Unreachable { .. })
        ));
        let rows: Vec<PublishRow> = (1..=2)
            .map(|seq| PublishRow {
                node: "n1".into(),
                seq,
                table: "transitions".into(),
                payload: "{}".into(),
            })
            .collect();
        let receipt = state.publish(&rows).expect("queued");
        assert_eq!(receipt.queued, 2);
        assert_eq!(state.queued().expect("depth"), 2);
        assert!(matches!(state.mode(), Mode::Degraded { .. }));
        state.set_degraded(None);
        assert_eq!(state.queued().expect("depth"), 0);
        assert_eq!(state.published_count(), 2);
        let again = state.publish(&rows).expect("replay");
        assert_eq!(again.duplicates, 2);
        assert_eq!(state.published_count(), 2);
    }

    // The node half against the fake: shared first, then local, then one
    // release when the local step refuses.

    use super::super::node::{self, NodeError};
    use crate::ledger::{
        Actor, Claim, Clock, DecisionAnswer, Ledger, NewIntent, RecordedVia, SuccessorProcess,
        Transition, CLAIM_GRACE_SECONDS,
    };
    use crate::lifecycle::{Reason, State};
    use std::sync::atomic::AtomicI64;

    /// A ledger clock a test moves by hand.
    struct Tick(Arc<AtomicI64>);

    impl Clock for Tick {
        fn now_rfc3339(&self) -> String {
            chrono::DateTime::from_timestamp(1_800_000_000 + self.0.load(Ordering::SeqCst), 0)
                .expect("a time")
                .to_rfc3339()
        }
    }

    fn run(id: &str) -> RunId {
        RunId::from_stored(id)
    }

    /// A ledger on a hand clock with `run-a` waiting on a decision.
    fn waiting(label: &str) -> (Ledger, Arc<AtomicI64>, crate::test_support::TempDir) {
        let dir = crate::test_support::temp_dir(&format!("fake-node-{label}"));
        let seconds = Arc::new(AtomicI64::new(0));
        let ledger = Ledger::open_with_clock(
            &dir.join("ledger.sqlite"),
            Box::new(Tick(Arc::clone(&seconds))),
        )
        .expect("ledger");
        ledger
            .insert_run(
                &run("run-a"),
                "/r",
                None,
                &crate::ids::TaskId::from_stored("task-a"),
                "rk",
            )
            .expect("run");
        ledger
            .record_transition(&Transition {
                run_id: run("run-a"),
                attempt_id: None,
                from_state: None,
                to_state: State::NeedsDecision,
                reason: "scope_exceeded".into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("waiting");
        (ledger, seconds, dir)
    }

    fn answer() -> DecisionAnswer<'static> {
        DecisionAnswer {
            resolution: Reason::DecisionRevised,
            actor: Actor::Orchestrator,
            person_quote: None,
            recorded_via: RecordedVia::Cli,
            note: None,
            successor_run: None,
            from_state: State::NeedsDecision,
            to_state: State::Failed,
        }
    }

    fn intent() -> NewIntent {
        NewIntent {
            predecessor: run("run-a"),
            contract_digest: "digest".into(),
            contract_json: "{}".into(),
            scope_delta: Vec::new(),
            candidate: None,
            review: None,
        }
    }

    fn up(_: u32) -> SuccessorProcess {
        SuccessorProcess::Alive { started: None }
    }

    /// The decision answered on `ledger` through the fake: its intent id.
    fn answered(state: &FakeState, ledger: &Ledger) -> String {
        node::resolve_decision_with_intent(state, ledger, &run("run-a"), &answer(), &intent())
            .expect("written")
            .expect("open")
            .intent_id
    }

    #[test]
    fn a_local_refusal_after_a_shared_grant_leaves_the_intent_free() {
        let (ledger, _seconds, _dir) = waiting("unknown");
        let state = FakeState::new();
        assert_eq!(
            node::claim_intent(&state, &ledger, "intent-nowhere", &run("run-1"), &up)
                .expect("answered"),
            Claim::Unknown
        );
        assert_eq!(state.intent_holder("intent-nowhere"), None);
    }

    #[test]
    fn a_claim_held_by_a_gone_run_moves_to_the_new_run() {
        let (ledger, seconds, _dir) = waiting("takeover");
        let state = FakeState::new();
        let id = answered(&state, &ledger);
        assert!(matches!(
            node::claim_intent(&state, &ledger, &id, &run("run-1"), &up).expect("claim"),
            Claim::Claimed(_)
        ));
        // run-1 never got a run row: past the grace, it is abandoned.
        seconds.store(CLAIM_GRACE_SECONDS + 1, Ordering::SeqCst);
        assert!(matches!(
            node::claim_intent(&state, &ledger, &id, &run("run-2"), &up).expect("claim"),
            Claim::Claimed(intent) if intent.run_id == Some(run("run-2"))
        ));
        assert_eq!(state.intent_holder(&id), Some(run("run-2")));
    }

    #[test]
    fn a_shared_holder_this_ledger_never_saw_stays_the_holder() {
        let (ledger, seconds, _dir) = waiting("elsewhere");
        let state = FakeState::new();
        let id = answered(&state, &ledger);
        // Another node's run holds it; this ledger's row is still pending.
        state
            .claim_intent(&id, &run("run-elsewhere"), None)
            .expect("claimed elsewhere");
        seconds.store(CLAIM_GRACE_SECONDS + 1, Ordering::SeqCst);
        assert_eq!(
            node::claim_intent(&state, &ledger, &id, &run("run-2"), &up).expect("answered"),
            Claim::AlreadyClaimed {
                run: run("run-elsewhere")
            }
        );
        assert_eq!(state.intent_holder(&id), Some(run("run-elsewhere")));
        let local = ledger
            .successor_intent(&id)
            .expect("read")
            .expect("the intent");
        assert_eq!(local.run_id, None, "no local write");
    }

    #[test]
    fn a_decision_held_elsewhere_is_refused_until_its_grant_lapses() {
        let (ledger, _seconds, _dir) = waiting("decision");
        let state = FakeState::new();
        // Another answerer took the decision and never wrote its answer.
        assert_eq!(
            state.answer_decision(&run("run-a")).expect("granted"),
            DecisionGrant::Granted
        );
        assert!(
            matches!(
                node::resolve_decision_with_intent(
                    &state,
                    &ledger,
                    &run("run-a"),
                    &answer(),
                    &intent()
                ),
                Err(NodeError::Shared(SharedStateError::Refused { .. }))
            ),
            "held elsewhere, said as such"
        );
        // Its grant lapses; this node's answer is then written.
        state.advance(DECISION_GRANT_TTL_SECS + 1);
        assert!(
            node::resolve_decision_with_intent(
                &state,
                &ledger,
                &run("run-a"),
                &answer(),
                &intent()
            )
            .expect("written")
            .is_some(),
            "the decision was still open on this node"
        );
        // A run with no open decision here gives the grant back.
        assert_eq!(
            node::resolve_decision_with_intent(
                &state,
                &ledger,
                &run("run-z"),
                &answer(),
                &intent()
            )
            .expect("answered"),
            None
        );
        assert_eq!(
            state.answer_decision(&run("run-z")).expect("free again"),
            DecisionGrant::Granted
        );
    }

    #[test]
    fn a_failed_release_after_a_local_refusal_returns_the_refusal() {
        let (ledger, _seconds, _dir) = waiting("release-fails");
        let state = FakeState::new();
        state.set_releases_fail(Some("primary refused the release"));
        assert_eq!(
            node::claim_intent(&state, &ledger, "intent-nowhere", &run("run-1"), &up)
                .expect("the refusal, not the release failure"),
            Claim::Unknown
        );
        // The hold the release could not give back is still there.
        assert_eq!(state.intent_holder("intent-nowhere"), Some(run("run-1")));
    }

    #[test]
    fn a_claim_file_refusal_after_a_shared_grant_gives_the_ticket_back() {
        use crate::programme::claim::{ClaimKey, Contest, Store, Taken, LOCK_WAIT};
        let dir = crate::test_support::temp_dir("fake-node-ticket");
        let store = Store::in_state_dir(&dir);
        let key = ClaimKey {
            repo_key: "rk".into(),
            slug: "programme".into(),
        };
        let now = crate::programme::claim::now_unix();
        // Another session on this node holds the claim file; the shared
        // half has never heard of it.
        store
            .lock(&key, LOCK_WAIT, now)
            .expect("lock")
            .claim_work("t1", "session-local", "/r", Contest::Refuse, now)
            .expect("claimed locally");
        let state = FakeState::new();
        let me = Principal::operator("session-me");
        let closed = BTreeSet::new();
        let grant = node::claim_ticket(
            &state,
            &store,
            &super::super::TicketClaim {
                key: &key,
                closed: &closed,
                id: "t1",
                holder: &me,
                repo_display: "/r",
                contest: Contest::Refuse,
                now: now + 1,
            },
        )
        .expect("answered");
        assert!(matches!(grant.taken, Taken::Refused { .. }), "{grant:?}");
        assert!(grant.lease.is_none());
        // The shared hold was given back: another holder gets it at once.
        let other = TicketRequest {
            repo_key: "rk".into(),
            slug: "programme".into(),
            id: "t1".into(),
            holder: Principal::operator("session-other"),
            contest: Contest::Refuse,
        };
        assert!(matches!(
            state.claim_ticket(&other).expect("answered"),
            TicketAnswer::Granted(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_local_promote_puts_the_shared_pointer_back() {
        use crate::learn::fixtures::{artifact, evaluation};
        use crate::learn::registry::Registry;
        let dir = crate::test_support::temp_dir("fake-node-promote");
        let registry_dir = dir.join("registry");
        let registry = Registry::open(&registry_dir).expect("registry");
        registry
            .store(&artifact("art-a", Some(evaluation("art-a", true))))
            .expect("store");
        let evaluated = registry.evaluated("art-a").expect("evidence");
        let state = FakeState::new();
        // The pointer files cannot be written: the local promote fails.
        let read_only = |mode: u32| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&registry_dir, std::fs::Permissions::from_mode(mode))
                .expect("chmod");
        };
        // The lock file exists before the directory turns read-only: only
        // the pointer writes fail, after the shared swap.
        drop(
            crate::procs::LockFile::try_acquire(&registry_dir.join(".pointer.lock")).expect("lock"),
        );
        read_only(0o555);
        let swap = node::swap_registry_pointer(&state, &registry, None, &evaluated);
        read_only(0o755);
        assert!(swap.is_err(), "the promote failed: {swap:?}");
        assert_eq!(
            state.registry_pointer().expect("read"),
            Pointer::Shared(None),
            "the shared pointer is back where it was"
        );
    }

    use crate::learn::registry::Registry;

    /// A registry with `art-z` promoted then `art-a` (so `art-a` is active
    /// and `art-z` previous) and `art-b` stored, all with passing evidence.
    fn three_artifacts(label: &str) -> (Registry, crate::test_support::TempDir) {
        use crate::learn::fixtures::{artifact, evaluation};
        let dir = crate::test_support::temp_dir(label);
        let registry = Registry::open(&dir.join("registry")).expect("registry");
        for id in ["art-z", "art-a", "art-b"] {
            registry
                .store(&artifact(id, Some(evaluation(id, true))))
                .expect("store");
        }
        for id in ["art-z", "art-a"] {
            registry
                .promote(&registry.evaluated(id).expect("evidence"))
                .expect("promote");
        }
        (registry, dir)
    }

    fn is_conflict(attempt: &Result<impl std::fmt::Debug, node::NodeError>) -> bool {
        matches!(
            attempt,
            Err(node::NodeError::Shared(
                SharedStateError::CasConflict { .. }
            ))
        )
    }

    /// A promote of `art-b` and a rollback, both judged against `art-a`:
    /// whichever lands first, the other is a compare-and-set conflict.
    fn race(state: &dyn SharedState, label: &str) {
        for promote_first in [true, false] {
            let (registry, _dir) = three_artifacts(&format!("{label}-{promote_first}"));
            let b = registry.evaluated("art-b").expect("evidence");
            let promote = || node::swap_registry_pointer(state, &registry, Some("art-a"), &b);
            let rollback = || node::roll_back_registry(state, &registry, "art-a");
            let (promoted, rolled) = if promote_first {
                let p = promote();
                (p, rollback())
            } else {
                let r = rollback();
                (promote(), r)
            };
            let wins = [promoted.is_ok(), rolled.is_ok()];
            assert_eq!(
                wins.iter().filter(|w| **w).count(),
                1,
                "exactly one wins: {promoted:?} {rolled:?}"
            );
            assert!(is_conflict(&promoted) || is_conflict(&rolled));
            let winner = if promote_first { "art-b" } else { "art-z" };
            assert_eq!(registry.active_id().expect("read").as_deref(), Some(winner));
            // Put the shared pointer back for the next order.
            if let Pointer::Shared(current) = state.registry_pointer().expect("read") {
                state
                    .swap_registry_pointer(current.as_deref(), Some("art-a"))
                    .expect("reset");
            }
        }
    }

    #[test]
    fn a_promote_and_a_rollback_judged_against_one_artifact_cannot_both_win() {
        race(&super::super::LocalState::new(), "race-local");
        let fake = FakeState::new();
        fake.swap_registry_pointer(None, Some("art-a"))
            .expect("seed");
        race(&fake, "race-fake");
    }

    #[test]
    fn a_held_pointer_lock_refuses_both_paths_and_moves_nothing() {
        let (registry, dir) = three_artifacts("pointer-lock");
        let _held =
            crate::procs::LockFile::try_acquire(&dir.join("registry").join(".pointer.lock"))
                .expect("lock")
                .expect("free");
        let b = registry.evaluated("art-b").expect("evidence");
        let fake = FakeState::new();
        fake.swap_registry_pointer(None, Some("art-a"))
            .expect("seed");
        let local = super::super::LocalState::new();
        for state in [&fake as &dyn SharedState, &local] {
            let no_wait = std::time::Duration::ZERO;
            for refused in [
                node::swap_within(state, &registry, Some("art-a"), &b, no_wait).map(|_| None),
                node::roll_back_within(state, &registry, "art-a", no_wait),
            ] {
                assert!(
                    matches!(
                        refused,
                        Err(node::NodeError::Shared(SharedStateError::Refused { .. }))
                    ),
                    "{refused:?}"
                );
            }
        }
        assert_eq!(
            fake.registry_pointer().expect("read"),
            Pointer::Shared(Some("art-a".into()))
        );
        assert_eq!(
            registry.active_id().expect("read").as_deref(),
            Some("art-a")
        );
    }

    #[test]
    fn a_shared_pointer_ahead_is_healed_before_the_next_swap() {
        let (registry, _dir) = three_artifacts("heal");
        let fake = FakeState::new();
        // A promote of art-b won the shared swap and died before its local
        // half: the shared pointer is ahead of this node's.
        fake.swap_registry_pointer(None, Some("art-b"))
            .expect("ahead");
        let restored = node::roll_back_registry(&fake, &registry, "art-b").expect("rolled back");
        assert_eq!(
            restored.as_deref(),
            Some("art-a"),
            "healed to art-b, then back"
        );
        assert_eq!(
            registry.active_id().expect("read").as_deref(),
            Some("art-a")
        );
        assert_eq!(
            fake.registry_pointer().expect("read"),
            Pointer::Shared(Some("art-a".into()))
        );
    }

    #[test]
    fn a_shared_pointer_naming_an_artifact_this_node_lacks_fails_closed() {
        let (registry, _dir) = three_artifacts("heal-missing");
        let fake = FakeState::new();
        fake.swap_registry_pointer(None, Some("art-elsewhere"))
            .expect("ahead");
        let b = registry.evaluated("art-b").expect("evidence");
        let swap = node::swap_registry_pointer(&fake, &registry, Some("art-elsewhere"), &b);
        assert!(
            matches!(
                &swap,
                Err(node::NodeError::Shared(SharedStateError::Refused { reason }))
                    if reason.starts_with(
                        "the shared registry pointer names art-elsewhere, which this node \
                         cannot make active (no artifact art-elsewhere in this registry"
                    )
            ),
            "{swap:?}"
        );
        assert_eq!(
            registry.active_id().expect("read").as_deref(),
            Some("art-a")
        );
    }

    fn ticket(id: &str, who: &Principal) -> TicketRequest {
        TicketRequest {
            repo_key: "rk".into(),
            slug: "programme".into(),
            id: id.into(),
            holder: who.clone(),
            contest: crate::programme::claim::Contest::Refuse,
        }
    }

    #[test]
    fn a_work_hold_renewed_stays_and_one_left_alone_lapses() {
        let state = FakeState::new();
        let me = Principal::operator("session-me");
        let other = Principal::operator("session-other");
        assert!(matches!(
            state.claim_ticket(&ticket("t1", &me)).expect("claim"),
            TicketAnswer::Granted(_)
        ));
        state.advance(TICKET_TTL_SECS - 10);
        state.renew_tickets(&me).expect("renew");
        state.advance(20);
        assert!(matches!(
            state.claim_ticket(&ticket("t1", &other)).expect("answered"),
            TicketAnswer::Held { .. }
        ));
        state.advance(TICKET_TTL_SECS + 1);
        assert!(matches!(
            state.claim_ticket(&ticket("t1", &other)).expect("answered"),
            TicketAnswer::Granted(_)
        ));
    }

    #[test]
    fn a_retried_reservation_by_its_own_holder_is_granted_again() {
        let state = FakeState::new();
        let me = Principal::operator("session-me");
        for attempt in ["first", "retry"] {
            assert!(
                matches!(
                    state.reserve_handoff(&ticket("t1", &me)).expect("answered"),
                    TicketAnswer::Granted(_)
                ),
                "{attempt}"
            );
        }
        assert!(matches!(
            state
                .reserve_handoff(&ticket("t1", &Principal::operator("session-other")))
                .expect("answered"),
            TicketAnswer::Held { .. }
        ));
    }

    #[test]
    fn a_session_end_gives_back_the_shared_work_and_keeps_the_handoff() {
        use crate::programme::claim::{ClaimKey, Contest, Store, LOCK_WAIT};
        let dir = crate::test_support::temp_dir("fake-node-session");
        let store = Store::in_state_dir(&dir);
        let key = ClaimKey {
            repo_key: "rk".into(),
            slug: "programme".into(),
        };
        let now = crate::programme::claim::now_unix();
        let me = Principal::operator("session-me");
        let state: Arc<dyn SharedState> = Arc::new(FakeState::new());
        let closed = BTreeSet::new();
        node::claim_ticket(
            &*state,
            &store,
            &super::super::TicketClaim {
                key: &key,
                closed: &closed,
                id: "t1",
                holder: &me,
                repo_display: "/r",
                contest: Contest::Refuse,
                now,
            },
        )
        .expect("claimed");
        let lock = store.lock(&key, LOCK_WAIT, now).expect("lock");
        assert!(
            node::reserve_handoff(&*state, &lock, &key, "t2", &me, "/r", now).expect("reserve")
        );
        drop(lock);
        let pass = node::end_session(&state, &store, "session-me", now + 1, None);
        pass.shared.expect("given back");
        pass.local.expect("claim files");
        let other = Principal::operator("session-other");
        assert!(matches!(
            state.claim_ticket(&ticket("t1", &other)).expect("answered"),
            TicketAnswer::Granted(_)
        ));
        assert!(
            matches!(
                state
                    .reserve_handoff(&ticket("t2", &other))
                    .expect("answered"),
                TicketAnswer::Held { .. }
            ),
            "the reservation stays"
        );
        let records = store.records(&key).expect("records");
        assert!(
            !records.contains_key("t1") && records.contains_key("t2"),
            "{records:?}"
        );
    }

    #[test]
    fn a_shared_handoff_hold_skips_the_ticket_and_the_claim_file_is_untouched() {
        use crate::programme::claim::{ClaimKey, Store, LOCK_WAIT};
        let dir = crate::test_support::temp_dir("fake-node-handoff");
        let store = Store::in_state_dir(&dir);
        let key = ClaimKey {
            repo_key: "rk".into(),
            slug: "programme".into(),
        };
        let state = FakeState::new();
        // Another node holds t1.
        state
            .claim_ticket(&ticket("t1", &Principal::operator("elsewhere")))
            .expect("held");
        let now = crate::programme::claim::now_unix();
        let lock = store.lock(&key, LOCK_WAIT, now).expect("lock");
        let me = Principal::operator("session-me");
        assert!(
            !node::reserve_handoff(&state, &lock, &key, "t1", &me, "/r", now).expect("answered")
        );
        drop(lock);
        assert!(store.records(&key).expect("records").is_empty());
    }
}
