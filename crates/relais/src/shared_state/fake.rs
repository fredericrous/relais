//! An in-memory [`SharedState`] with an external primary's semantics
//! (an-t10, an-t11): leases that expire on its own clock, a bounded spend
//! slice, a degraded mode in which claims fail closed and publish rows
//! queue, and compare-and-set claims and registry pointer. It is not shaped
//! after [`super::LocalState`]: the conformance suite runs both, so a
//! property that holds for one only would show.
//!
//! Node-local writes (the ledger's decision and intent rows, the registry's
//! artifact files) still go to the handles the caller passes, as a real
//! client's would; the fake keeps the shared half in memory.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use super::types::{
    LeaseToken, Mode, Principal, PublishReceipt, PublishRow, Reservation, ResourceId, Settlement,
    Slice,
};
use super::{Selection, SharedState, SharedStateError, TicketClaim, TicketGrant};
use crate::ids::RunId;
use crate::learn::registry::{EvaluatedArtifact, Registry};
use crate::ledger::{Claim, DecisionAnswer, Ledger, NewIntent, SuccessorIntent, SuccessorProcess};
use crate::money::MicroUsd;
use crate::policy::MachineSettings;
use crate::programme::claim::{Acquired, Holder, Kind, Store, Taken};

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

    /// Rows the sink holds.
    pub fn published_count(&self) -> usize {
        self.lock().published.len()
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
        self.online()?;
        let now = self.now();
        let mut inner = self.lock();
        let row = inner.rows.entry(resource.clone()).or_insert(Row {
            holder: None,
            epoch: 0,
            expires_at: 0,
            claimed_at: now,
        });
        if let Some(current) = &row.holder {
            if current != holder && row.expires_at > now {
                return Err(SharedStateError::ClaimLost { resource });
            }
        }
        if row.holder.as_ref() != Some(holder) || row.expires_at <= now {
            row.claimed_at = now;
        }
        row.epoch += 1;
        row.holder = Some(holder.clone());
        row.expires_at = now + ttl;
        Ok(LeaseToken {
            resource,
            holder: holder.clone(),
            epoch: row.epoch,
            expires_at: Some(row.expires_at),
        })
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

    fn daily_spent(
        &self,
        _ledger: &Ledger,
        _day_start: &str,
    ) -> Result<MicroUsd, SharedStateError> {
        Ok(self.spent())
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
                requested,
                remaining: inner.slice,
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

    fn claim_ticket(
        &self,
        _store: &Store,
        claim: &TicketClaim<'_>,
    ) -> Result<TicketGrant, SharedStateError> {
        self.online()?;
        let resource = ResourceId::Ticket {
            repo_key: claim.key.repo_key.clone(),
            slug: claim.key.slug.clone(),
            id: claim.id.to_string(),
        };
        let now = self.now();
        let before = self.lock().rows.get(&resource).cloned();
        let refreshed = matches!(
            &before,
            Some(row) if row.holder.as_ref() == Some(claim.holder) && row.expires_at > now
        );
        match self.acquire(resource, claim.holder, TICKET_TTL_SECS) {
            Ok(lease) => Ok(TicketGrant {
                taken: Taken::Claimed {
                    how: if refreshed {
                        Acquired::Refreshed
                    } else {
                        Acquired::Created
                    },
                    from: None,
                },
                lease: Some(lease),
            }),
            Err(SharedStateError::ClaimLost { .. }) => {
                let row = before.expect("a refused claim has a holder");
                let holder = row.holder.expect("a refused claim has a holder");
                Ok(TicketGrant {
                    taken: Taken::Refused {
                        holder: Holder::Session {
                            id: holder.id,
                            kind: Kind::Work,
                        },
                        age_secs: (now - row.claimed_at).max(0) as u64,
                    },
                    lease: None,
                })
            }
            Err(other) => Err(other),
        }
    }

    fn claim_intent(
        &self,
        ledger: &Ledger,
        intent_id: &str,
        run: &RunId,
        alive: &dyn Fn(u32) -> SuccessorProcess,
    ) -> Result<Claim, SharedStateError> {
        // Fails closed offline: two nodes must not both launch it (an-t10).
        self.online()?;
        Ok(ledger.claim_intent(intent_id, run, alive)?)
    }

    fn release_claim(
        &self,
        ledger: &Ledger,
        intent_id: &str,
        run: &RunId,
        refusal: &str,
    ) -> Result<bool, SharedStateError> {
        self.online()?;
        Ok(ledger.release_claim(intent_id, run, refusal)?)
    }

    fn resolve_decision_with_intent(
        &self,
        ledger: &Ledger,
        run: &RunId,
        answer: &DecisionAnswer<'_>,
        intent: &NewIntent,
    ) -> Result<Option<SuccessorIntent>, SharedStateError> {
        self.online()?;
        Ok(ledger.resolve_decision_with_intent(run, answer, intent)?)
    }

    fn registry_pointer(&self, _registry: &Registry) -> Result<Option<String>, SharedStateError> {
        // A cached read is served offline (an-t10).
        Ok(self.lock().pointer.clone())
    }

    fn swap_registry_pointer(
        &self,
        _registry: &Registry,
        expected: Option<&str>,
        evaluated: &EvaluatedArtifact,
    ) -> Result<(), SharedStateError> {
        self.online()?;
        let mut inner = self.lock();
        if inner.pointer.as_deref() != expected {
            return Err(SharedStateError::CasConflict {
                expected: expected.map(str::to_string),
                found: inner.pointer.clone(),
            });
        }
        inner.pointer = Some(evaluated.artifact_id().to_string());
        Ok(())
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
}
