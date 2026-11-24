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
pub(crate) enum SeatRefusal {
    /// The seat or its reservation was refused, or the primary is
    /// unreachable: the dispatch waits and asks again.
    Wait,
    /// This node's spend slice cannot cover the reservation.
    SliceExhausted {
        reserve: MicroUsd,
        remaining: MicroUsd,
    },
}

/// The shared operation a failure line is about, so each is written once
/// per change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SeatOp {
    Seat,
    Reserve,
    Settle,
    Renew,
    Release,
}

/// Which new seats one sync acquires.
#[derive(Debug, Clone, Copy)]
enum Acquire<'a> {
    /// Every new seat (the reconcile tick).
    Every,
    /// Only this dispatch's (the request that asks a seat for it).
    Only(&'a str),
    /// None (a request that asks no seat): new seats wait for the tick.
    NoNew,
}

impl Acquire<'_> {
    fn takes(self, dispatch_id: &str) -> bool {
        match self {
            Self::Every => true,
            Self::Only(only) => only == dispatch_id,
            Self::NoNew => false,
        }
    }
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
    // holds-until: settles are persisted (follow-up to an-t17); until then
    // a restart loses the ones still waiting, and a settle the primary
    // refuses is dropped; the ledger floor in `node::daily_spent` keeps the
    // cap from counting less than this node spent.
    /// Settles the primary has not taken yet, oldest first.
    settles: Vec<(String, Reservation, MicroUsd)>,
    /// The failure line last written per dispatch and operation, so each
    /// failure is written once per change, not once per sync.
    failing: BTreeMap<(String, SeatOp), String>,
}

impl Book {
    /// Write `line` for `(dispatch, op)` unless it is the line already
    /// written for it.
    fn fail(&mut self, log: &LogSink, dispatch: &str, op: SeatOp, line: String) {
        let key = (dispatch.to_string(), op);
        if self.failing.get(&key) != Some(&line) {
            log(&line);
            self.failing.insert(key, line);
        }
    }

    /// `(dispatch, op)` works again: say so once, if it was failing.
    fn recover(&mut self, log: &LogSink, dispatch: &str, op: SeatOp, line: &str) {
        if self.failing.remove(&(dispatch.to_string(), op)).is_some() {
            log(line);
        }
    }

    /// Hand the waiting settles to the primary, oldest first; what it does
    /// not take waits for the next sync. A settle is never refused for a
    /// stale seat (an-t11). An outage stops the pass: the rest are not
    /// tried until the next sync. A settle the primary refuses (`Refused`)
    /// is dropped; any other failure is kept and the pass goes on. Returns
    /// `false` when the primary was found unreachable.
    fn flush_settles(&mut self, shared: &dyn SharedState, log: &LogSink) -> bool {
        let mut waiting = std::mem::take(&mut self.settles).into_iter();
        let mut reachable = true;
        for (id, reservation, actual) in waiting.by_ref() {
            match shared.settle_spend(&reservation, actual) {
                Ok(_) => self.recover(
                    log,
                    &id,
                    SeatOp::Settle,
                    &format!("seat {id}: settle recovered"),
                ),
                Err(settle_error @ SharedStateError::Unreachable { .. }) => {
                    // An outage: kept, and the pass stops here.
                    self.fail(
                        log,
                        &id,
                        SeatOp::Settle,
                        format!(
                            "seat {id}: booking the spend failed: {settle_error}; it is retried \
                             at the next sync"
                        ),
                    );
                    self.settles.push((id, reservation, actual));
                    reachable = false;
                    break;
                }
                Err(settle_error @ SharedStateError::Refused { .. }) => {
                    // The primary answered and refused it: retrying would
                    // repeat the refusal forever, so it is dropped. The
                    // shared counter never books it; this node's ledger does,
                    // and the floor in `node::daily_spent` counts it here.
                    // The error already reads `refused: <reason>`.
                    self.failing.remove(&(id.clone(), SeatOp::Settle));
                    log(&format!(
                        "seat {id}: booking the spend was not accepted: {settle_error}; dropped, \
                         the shared budget does not count it, this node's ledger does; if it \
                         repeats, check the shared primary"
                    ));
                }
                Err(
                    settle_error @ (SharedStateError::LeaseLost { .. }
                    | SharedStateError::ClaimLost { .. }
                    | SharedStateError::CasConflict { .. }
                    | SharedStateError::SliceExhausted { .. }
                    | SharedStateError::ExternalAbsent { .. }
                    | SharedStateError::Home(_)
                    | SharedStateError::Read { .. }
                    | SharedStateError::Invalid { .. }),
                ) => {
                    // Every other failure is kept and retried at the next
                    // sync; a new variant must be placed here or above.
                    self.fail(
                        log,
                        &id,
                        SeatOp::Settle,
                        format!(
                            "seat {id}: booking the spend failed: {settle_error}; it is retried \
                             at the next sync"
                        ),
                    );
                    self.settles.push((id, reservation, actual));
                }
            }
        }
        self.settles.extend(waiting);
        reachable
    }

    /// Give a freed seat back; a failure is one line and the token goes
    /// either way: the primary's TTL ends the hold.
    fn release(
        &mut self,
        shared: &dyn SharedState,
        log: &LogSink,
        id: &str,
        token: &LeaseToken,
    ) -> bool {
        match shared.release_seat(token) {
            Ok(()) => {
                self.failing.remove(&(id.to_string(), SeatOp::Release));
                true
            }
            Err(release_error) => {
                let reachable = !matches!(release_error, SharedStateError::Unreachable { .. });
                self.fail(
                    log,
                    id,
                    SeatOp::Release,
                    format!(
                        "seat {id}: giving the shared seat back failed: {release_error}; it ends by \
                         itself after its time limit"
                    ),
                );
                reachable
            }
        }
    }
}

/// The shared seats and reservations one coordinator holds.
pub struct SeatBook {
    shared: Arc<dyn SharedState>,
    holder: Principal,
    book: Mutex<Book>,
    log: LogSink,
}

/// The line for a dispatch that waits on its shared seat.
fn wait_line(id: &str, cause: &SharedStateError) -> String {
    format!(
        "seat {id}: the shared state did not grant the seat ({cause}); this run waits and asks \
         again; if it lasts, check the shared primary"
    )
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
        self.book
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mirror the admission state's seated agent dispatches into the
    /// shared state: settle what settled, release the seats that were
    /// freed, acquire and reserve the new ones. The reconcile tick's sync.
    pub fn sync(&self, state: &Mutex<AdmissionState>) {
        self.sync_acquiring(state, Acquire::Every);
    }

    /// The sync after one request: settles and releases as [`Self::sync`],
    /// but only `admitting` (the dispatch this request asks a seat for, if
    /// any) is acquired; every other new seat waits for the tick. Once the
    /// primary is found unreachable the pass makes no more shared calls,
    /// so one request makes a bounded number of them while it is down.
    pub fn sync_request(&self, state: &Mutex<AdmissionState>, admitting: Option<&str>) {
        self.sync_acquiring(state, admitting.map_or(Acquire::NoNew, Acquire::Only));
    }

    fn sync_acquiring(&self, state: &Mutex<AdmissionState>, acquire: Acquire<'_>) {
        let shared = self.shared.as_ref();
        let log = &self.log;
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
        let mut reachable = book.flush_settles(shared, log);

        // Seats freed: released. Once the primary is found unreachable the
        // freed seats not yet tried keep their tokens, and the next sync
        // releases them; the one whose release failed ends at its TTL.
        let seated_ids: BTreeSet<&str> = seated
            .iter()
            .map(|seat| seat.dispatch_id.as_str())
            .collect();
        let freed: Vec<String> = book
            .leases
            .keys()
            .filter(|id| !seated_ids.contains(id.as_str()))
            .cloned()
            .collect();
        for id in freed {
            if !reachable {
                break;
            }
            let Some(token) = book.leases.remove(&id) else {
                continue;
            };
            reachable = book.release(shared, log, &id, &token);
        }
        book.lost.retain(|id| seated_ids.contains(id.as_str()));
        book.refused
            .retain(|id, _| seated_ids.contains(id.as_str()));
        let settling: BTreeSet<String> = book.settles.iter().map(|(id, ..)| id.clone()).collect();
        let releasing: BTreeSet<String> = book.leases.keys().cloned().collect();
        book.failing.retain(|(id, _), _| {
            seated_ids.contains(id.as_str()) || settling.contains(id) || releasing.contains(id)
        });

        // New seats: acquired, then reserved (once per dispatch: a
        // resumed parent keeps the reservation it had).
        for seat in &seated {
            let id = seat.dispatch_id.as_str();
            if book.leases.contains_key(id) || book.lost.contains(id) || !acquire.takes(id) {
                continue;
            }
            if !reachable {
                // Not asked again in this pass: the dispatch waits, and
                // its next poll or the tick asks the primary.
                book.refused.insert(id.to_string(), SeatRefusal::Wait);
                continue;
            }
            let token = match shared.acquire_seat(&self.holder, id) {
                Ok(token) => token,
                Err(acquire_error) => {
                    reachable = !matches!(acquire_error, SharedStateError::Unreachable { .. });
                    book.fail(log, id, SeatOp::Seat, wait_line(id, &acquire_error));
                    book.refused.insert(id.to_string(), SeatRefusal::Wait);
                    continue;
                }
            };
            if book.reservations.contains_key(id) {
                book.leases.insert(id.to_string(), token);
                book.refused.remove(id);
                book.recover(
                    log,
                    id,
                    SeatOp::Seat,
                    &format!("seat {id}: shared seat granted"),
                );
                continue;
            }
            let amount = MicroUsd::from_micros(seat.reserve_micros);
            match shared.reserve_spend(&token, amount) {
                Ok(reservation) => {
                    book.reservations.insert(id.to_string(), reservation);
                    book.leases.insert(id.to_string(), token);
                    book.refused.remove(id);
                    book.recover(
                        log,
                        id,
                        SeatOp::Seat,
                        &format!("seat {id}: shared seat granted"),
                    );
                    book.recover(
                        log,
                        id,
                        SeatOp::Reserve,
                        &format!("seat {id}: reserve recovered"),
                    );
                }
                Err(reserve_error) if seat.launched => {
                    // Its work may be running: the seat stays, and the
                    // node's ledger still books what it spends (the floor
                    // in `node::daily_spent` keeps the cap from missing it).
                    reachable = !matches!(reserve_error, SharedStateError::Unreachable { .. });
                    book.leases.insert(id.to_string(), token);
                    book.refused.remove(id);
                    book.fail(
                        log,
                        id,
                        SeatOp::Reserve,
                        format!(
                            "seat {id}: reserve failed: {reserve_error}; the spend is booked on \
                             this node only"
                        ),
                    );
                }
                Err(reserve_error) => {
                    reachable = !matches!(reserve_error, SharedStateError::Unreachable { .. });
                    if reachable {
                        reachable = book.release(shared, log, id, &token);
                    }
                    let refusal = if let SharedStateError::SliceExhausted {
                        requested,
                        remaining,
                    } = &reserve_error
                    {
                        SeatRefusal::SliceExhausted {
                            reserve: *requested,
                            remaining: *remaining,
                        }
                    } else {
                        SeatRefusal::Wait
                    };
                    book.fail(log, id, SeatOp::Seat, wait_line(id, &reserve_error));
                    book.refused.insert(id.to_string(), refusal);
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
            SeatRefusal::Wait => admission
                .unclaim(dispatch_id, now)
                .then_some(Decision::Queued { position: 1 }),
            SeatRefusal::SliceExhausted { reserve, remaining } => admission
                .revoke_grant(dispatch_id, now)
                .then(|| Decision::Refused {
                    code: Refusal::BudgetExceeded,
                    detail: format!(
                        "this node's share of the shared budget is used up: this run needs \
                         {reserve}, {remaining} is left; raise this node's share on the shared \
                         primary"
                    ),
                }),
        }
    }

    /// Extend every held seat (an-t11: every 15 s). The book is held only
    /// to copy the tokens and to apply each answer, so a slow primary never
    /// stalls the handlers; an answer is applied only while the book still
    /// holds the token it renewed. The first unreachable answer stops the
    /// pass; the rest are renewed at the next tick.
    pub fn renew(&self) {
        let log = &self.log;
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
                    book.recover(
                        log,
                        &id,
                        SeatOp::Renew,
                        &format!("seat {id}: renew recovered"),
                    );
                }
                Err(SharedStateError::LeaseLost { .. }) => {
                    book.leases.remove(&id);
                    book.lost.insert(id.clone());
                    book.failing.remove(&(id.clone(), SeatOp::Renew));
                    log(&format!(
                        "seat {id}: the shared seat was taken away (lease lost) on renew; the work \
                         in flight finishes and the seat is not taken again"
                    ));
                }
                Err(renew_error) => {
                    let unreachable = matches!(renew_error, SharedStateError::Unreachable { .. });
                    book.fail(
                        log,
                        &id,
                        SeatOp::Renew,
                        format!("seat {id}: renew failed: {renew_error}; retried at the next tick"),
                    );
                    if unreachable {
                        break;
                    }
                }
            }
        }
    }

    /// Give back every seat still held, best effort (the daemon is going
    /// down). A killed daemon releases nothing: its seats end at their TTL.
    pub fn release_all(&self) {
        let shared = self.shared.as_ref();
        let log = &self.log;
        let mut book = self.book();
        let book = &mut *book;
        if !book.flush_settles(shared, log) {
            return;
        }
        for (id, token) in std::mem::take(&mut book.leases) {
            if !book.release(shared, log, &id, &token) {
                return;
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
