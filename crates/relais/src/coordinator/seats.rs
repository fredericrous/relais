//! The coordinator's seats in the shared state (programme agents-many-nodes,
//! an-t17).
//!
//! `AdmissionState` still decides who gets a seat on this node: the queue,
//! the caps, the hard limits and the disk. The [`SeatBook`] mirrors what it
//! decided into the [`SharedState`]: every agent dispatch that holds a seat
//! (`model_work`, see [`AdmissionState::shared_seats`]) holds a shared seat
//! lease with an-t11's epoch and a spend reservation under it, and both go
//! when the dispatch frees its seat or settles. It works by diffing the
//! admission state against the book after each request and on each
//! reconcile tick, so every path a seat changes on is covered without
//! touching each path.
//!
//! Local first, then shared, the reverse of `shared_state::node`: asking
//! the primary before the queue decides would hold cluster seats for
//! requests that then queue here, and the undo (`unclaim`, `revoke_grant`)
//! is in memory and cannot fail.
//!
//! Locks: the book, then admission, briefly, never across a shared call;
//! no thread takes the book while it holds admission. One sync runs at a
//! time (the book is held across its acquire, reserve, settle and release
//! calls); the renew pass is the one exception and holds the book only to
//! copy the tokens and to apply each result. The trait has no timeout: a
//! closed client bounds each call.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::admission::{AdmissionState, Decision, Refusal};
use crate::money::MicroUsd;
use crate::shared_state::{LeaseToken, Principal, Reservation, SharedState, SharedStateError};

/// Where the book's lines go: the coordinator's stderr, or a test's buffer.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Why the shared half did not give a dispatch its seat, as the last
/// attempt found it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatRefusal {
    /// The seat or its reservation was refused, or the primary is
    /// unreachable: the dispatch waits and asks again.
    Wait { cause: String },
    /// This node's spend slice cannot cover the reservation.
    SliceExhausted {
        reserve_micros: i64,
        remaining_micros: i64,
    },
}

#[derive(Default)]
struct Book {
    /// The shared seat each dispatch holds.
    leases: BTreeMap<String, LeaseToken>,
    /// The reservation each dispatch holds, until it settles. It outlives
    /// the seat: a dispatch may free its seat before its usage settles.
    reservations: BTreeMap<String, Reservation>,
    /// Dispatches whose lease was found lost on renew: never re-acquired,
    /// since a re-acquire bumps the epoch and defeats the fence (an-t11).
    lost: BTreeSet<String>,
    /// The last acquire or reserve that failed, per dispatch; read by the
    /// handler that granted the dispatch.
    refused: BTreeMap<String, SeatRefusal>,
    /// Settles the primary has not taken yet, oldest first.
    settles: Vec<(String, Reservation, MicroUsd)>,
    /// The failure line last written per dispatch and operation, so each
    /// failure is written once per change, not once per sync.
    failing: BTreeMap<(String, &'static str), String>,
}

/// The shared seats and reservations one coordinator holds.
pub struct SeatBook {
    shared: Arc<dyn SharedState>,
    holder: Principal,
    book: Mutex<Book>,
    log: LogSink,
}

impl SeatBook {
    /// A book holding nothing yet, writing to the coordinator's stderr.
    pub fn new(shared: Arc<dyn SharedState>, holder: Principal) -> Self {
        Self {
            shared,
            holder,
            book: Mutex::new(Book::default()),
            log: Arc::new(|line: &str| super::log_line(line)),
        }
    }

    /// Send the book's lines somewhere else (a test's buffer).
    pub fn with_log(mut self, log: LogSink) -> Self {
        self.log = log;
        self
    }

    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Write `line` for `(dispatch, op)` unless it is the line already
    /// written for it.
    fn fail(&self, book: &mut Book, dispatch: &str, op: &'static str, line: String) {
        let key = (dispatch.to_string(), op);
        if book.failing.get(&key) != Some(&line) {
            (self.log)(&line);
            book.failing.insert(key, line);
        }
    }

    /// `(dispatch, op)` works again: say so once, if it was failing.
    fn recover(&self, book: &mut Book, dispatch: &str, op: &'static str, line: &str) {
        if book.failing.remove(&(dispatch.to_string(), op)).is_some() {
            (self.log)(line);
        }
    }

    /// Mirror the admission state's seated agent dispatches into the
    /// shared state: settle what settled, release the seats that were
    /// freed, acquire and reserve the new ones. The reconcile tick's sync.
    pub fn sync(&self, state: &Mutex<AdmissionState>) {
        self.sync_acquiring(state, None);
    }

    /// The sync after one request: settles and releases as [`Self::sync`],
    /// but only `admitting` (the dispatch this request asks a seat for, if
    /// any) is acquired; every other new seat waits for the tick, so one
    /// request makes a bounded number of shared calls while the primary
    /// fails.
    pub fn sync_request(&self, state: &Mutex<AdmissionState>, admitting: Option<&str>) {
        self.sync_acquiring(state, Some(admitting));
    }

    /// `only`: `None` acquires every new seat, `Some(x)` only `x`'s.
    fn sync_acquiring(&self, state: &Mutex<AdmissionState>, only: Option<Option<&str>>) {
        let mut book = self.book();
        let (seated, journal, gone) = {
            let mut admission = super::lock_state(state);
            let seated = admission.shared_seats();
            let journal = admission.take_spend_journal();
            let gone: Vec<String> = book
                .reservations
                .keys()
                .filter(|id| !admission.has_dispatch(id))
                .cloned()
                .collect();
            (seated, journal, gone)
        };
        let book = &mut *book;

        // What settled, at what it booked; then what left without a
        // settle (withdrawn, discarded unclaimed), at zero.
        for booked in journal {
            if let Some(reservation) = book.reservations.remove(&booked.dispatch_id) {
                book.settles.push((
                    booked.dispatch_id,
                    reservation,
                    MicroUsd::from_micros(booked.booked_micros),
                ));
            }
        }
        for id in gone {
            if let Some(reservation) = book.reservations.remove(&id) {
                book.settles
                    .push((id, reservation, MicroUsd::from_micros(0)));
            }
        }
        self.flush_settles(book);

        // Seats freed: released. A failure is one line; the token goes
        // either way and the primary's TTL ends the hold.
        let seated_ids: BTreeSet<&str> = seated.iter().map(|s| s.dispatch_id.as_str()).collect();
        let freed: Vec<String> = book
            .leases
            .keys()
            .filter(|id| !seated_ids.contains(id.as_str()))
            .cloned()
            .collect();
        for id in freed {
            let Some(token) = book.leases.remove(&id) else {
                continue;
            };
            if let Err(e) = self.shared.release_seat(&token) {
                (self.log)(&format!(
                    "seat {id}: release failed: {e}; the shared seat ends at its TTL"
                ));
            }
        }
        book.lost.retain(|id| seated_ids.contains(id.as_str()));
        book.refused
            .retain(|id, _| seated_ids.contains(id.as_str()));
        let settling: BTreeSet<String> = book.settles.iter().map(|(id, ..)| id.clone()).collect();
        book.failing
            .retain(|(id, _), _| seated_ids.contains(id.as_str()) || settling.contains(id));

        // New seats: acquired, then reserved (once per dispatch: a
        // resumed parent keeps the reservation it had).
        for seat in &seated {
            let id = seat.dispatch_id.as_str();
            if book.leases.contains_key(id) || book.lost.contains(id) {
                continue;
            }
            if only.is_some_and(|only| only != Some(id)) {
                continue;
            }
            let token = match self.shared.acquire_seat(&self.holder, id) {
                Ok(token) => token,
                Err(e) => {
                    let cause = e.to_string();
                    self.fail(
                        book,
                        id,
                        "seat",
                        format!(
                            "seat {id}: shared seat not granted ({cause}); the dispatch waits \
                             until the shared state grants it"
                        ),
                    );
                    book.refused
                        .insert(id.to_string(), SeatRefusal::Wait { cause });
                    continue;
                }
            };
            if book.reservations.contains_key(id) {
                book.leases.insert(id.to_string(), token);
                book.refused.remove(id);
                self.recover(book, id, "seat", &format!("seat {id}: shared seat granted"));
                continue;
            }
            let amount = MicroUsd::from_micros(seat.reserve_micros);
            match self.shared.reserve_spend(&token, amount) {
                Ok(reservation) => {
                    book.reservations.insert(id.to_string(), reservation);
                    book.leases.insert(id.to_string(), token);
                    book.refused.remove(id);
                    self.recover(book, id, "seat", &format!("seat {id}: shared seat granted"));
                    self.recover(
                        book,
                        id,
                        "reserve",
                        &format!("seat {id}: reserve recovered"),
                    );
                }
                Err(e) if seat.launched => {
                    // Its work may be running: the seat stays, and the
                    // node's ledger still books what it spends.
                    book.leases.insert(id.to_string(), token);
                    book.refused.remove(id);
                    self.fail(
                        book,
                        id,
                        "reserve",
                        format!(
                            "seat {id}: reserve failed: {e}; the spend is booked on this node only"
                        ),
                    );
                }
                Err(e) => {
                    if let Err(release) = self.shared.release_seat(&token) {
                        (self.log)(&format!(
                            "seat {id}: release failed: {release}; the shared seat ends at its TTL"
                        ));
                    }
                    let refusal = match &e {
                        SharedStateError::SliceExhausted {
                            requested,
                            remaining,
                        } => SeatRefusal::SliceExhausted {
                            reserve_micros: requested.to_micros(),
                            remaining_micros: remaining.to_micros(),
                        },
                        other => SeatRefusal::Wait {
                            cause: other.to_string(),
                        },
                    };
                    self.fail(
                        book,
                        id,
                        "seat",
                        format!(
                            "seat {id}: shared seat not granted ({e}); the dispatch waits \
                             until the shared state grants it"
                        ),
                    );
                    book.refused.insert(id.to_string(), refusal);
                }
            }
        }
    }

    /// Hand the waiting settles to the primary; what it does not take
    /// waits for the next sync. A settle is never refused for a stale seat
    /// (an-t11), so only an outage keeps one waiting.
    fn flush_settles(&self, book: &mut Book) {
        let waiting = std::mem::take(&mut book.settles);
        for (id, reservation, actual) in waiting {
            match self.shared.settle_spend(&reservation, actual) {
                Ok(_) => self.recover(book, &id, "settle", &format!("seat {id}: settle recovered")),
                Err(e) => {
                    self.fail(
                        book,
                        &id,
                        "settle",
                        format!("seat {id}: settle failed: {e}; it is retried at the next sync"),
                    );
                    book.settles.push((id, reservation, actual));
                }
            }
        }
    }

    /// The answer to a request `handle` granted, once the book has synced:
    /// `None` keeps `Granted`. When the shared half refused the dispatch's
    /// seat, the grant is taken back if the dispatch is still the claimed,
    /// unlaunched one this reply granted: a wait becomes `Queued`, an
    /// exhausted slice a budget refusal.
    pub fn answer_grant(
        &self,
        state: &Mutex<AdmissionState>,
        dispatch_id: &str,
        now: Instant,
    ) -> Option<Decision> {
        let refusal = {
            let book = self.book();
            if book.leases.contains_key(dispatch_id) {
                return None;
            }
            book.refused.get(dispatch_id).cloned()?
        };
        let mut admission = super::lock_state(state);
        match refusal {
            SeatRefusal::Wait { .. } => admission
                .unclaim(dispatch_id, now)
                .then_some(Decision::Queued { position: 1 }),
            SeatRefusal::SliceExhausted {
                reserve_micros,
                remaining_micros,
            } => admission
                .revoke_grant(dispatch_id, now)
                .then(|| Decision::Refused {
                    code: Refusal::BudgetExceeded,
                    detail: format!(
                        "shared spend slice exhausted: reservation {reserve_micros} micros, \
                         slice left {remaining_micros} micros"
                    ),
                }),
        }
    }

    /// Extend every held seat (an-t11: every 15 s). The book is held only
    /// to copy the tokens and to apply each answer, so a slow primary never
    /// stalls the handlers; an answer is applied only while the book still
    /// holds the token it renewed.
    pub fn renew(&self) {
        let held: Vec<(String, LeaseToken)> = self
            .book()
            .leases
            .iter()
            .map(|(id, token)| (id.clone(), token.clone()))
            .collect();
        for (id, token) in held {
            let answer = self.shared.renew_seat(&token);
            let mut book = self.book();
            let book = &mut *book;
            if book.leases.get(&id) != Some(&token) {
                continue;
            }
            match answer {
                Ok(renewed) => {
                    book.leases.insert(id.clone(), renewed);
                    self.recover(book, &id, "renew", &format!("seat {id}: renew recovered"));
                }
                Err(SharedStateError::LeaseLost { .. }) => {
                    book.leases.remove(&id);
                    book.lost.insert(id.clone());
                    book.failing.remove(&(id.clone(), "renew"));
                    (self.log)(&format!(
                        "seat {id}: lease_lost on renew; the work in flight finishes and the \
                         seat is not taken again"
                    ));
                }
                Err(e) => self.fail(
                    book,
                    &id,
                    "renew",
                    format!("seat {id}: renew failed: {e}; retried at the next tick"),
                ),
            }
        }
    }

    /// Give back every seat still held, best effort (the daemon is going
    /// down). A killed daemon releases nothing: its seats end at their TTL.
    pub fn release_all(&self) {
        let mut book = self.book();
        let book = &mut *book;
        self.flush_settles(book);
        for (id, token) in std::mem::take(&mut book.leases) {
            if let Err(e) = self.shared.release_seat(&token) {
                (self.log)(&format!(
                    "seat {id}: release failed: {e}; the shared seat ends at its TTL"
                ));
            }
        }
    }

    /// The seat `dispatch_id` holds, if any.
    #[cfg(test)]
    pub(crate) fn lease(&self, dispatch_id: &str) -> Option<LeaseToken> {
        self.book().leases.get(dispatch_id).cloned()
    }

    /// Whether `dispatch_id`'s lease was found lost.
    #[cfg(test)]
    pub(crate) fn is_lost(&self, dispatch_id: &str) -> bool {
        self.book().lost.contains(dispatch_id)
    }
}

#[cfg(test)]
mod tests;
