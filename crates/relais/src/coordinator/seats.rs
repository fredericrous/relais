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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::admission::{AdmissionState, Decision, Refusal};
use crate::money::MicroUsd;
use crate::shared_state::{
    LeaseToken, Principal, PrincipalKind, Reservation, ResourceId, Selection, SharedState,
    SharedStateError,
};

/// Where the book's lines go: the coordinator's stderr, or a test's buffer.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

/// The wall clock the book stamps waiting settles with, in unix seconds
/// (injected: `effects.no-ambient-access`).
pub type UnixClock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// How long a settle may wait for the primary. The counter is a UTC day, so
/// spend older than this belongs to another day.
pub const SETTLE_KEEP_SECS: i64 = 24 * 60 * 60;

/// How many waiting settles the book keeps (the newest); older ones are
/// dropped with a line.
pub const MAX_WAITING_SETTLES: usize = 500;

/// The file the waiting settles survive a restart in, beside the ledger.
pub const SETTLES_FILE: &str = "seat-settles.json";

fn system_clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Where a coordinator keeps its waiting settles: beside the ledger it serves.
pub fn settles_path(ledger: &Path) -> PathBuf {
    ledger.with_file_name(SETTLES_FILE)
}

/// Which sync is flushing the waiting settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pass {
    /// After a request: only settles that already have a reservation, so
    /// one request makes a bounded number of shared calls.
    Request,
    /// The reconcile tick (and shutdown): also settles with no reservation.
    Reconcile,
}

/// Why a `seat-settles.json` is not a settle list.
#[derive(Debug)]
enum SettleFileError {
    Json(serde_json::Error),
    HolderKind(String),
}

impl std::fmt::Display for SettleFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(e) => write!(f, "{e}"),
            Self::HolderKind(kind) => write!(f, "a settle names the unknown holder kind {kind:?}"),
        }
    }
}

/// What one dispatch spent that the primary has not booked yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingSettle {
    dispatch: String,
    /// `None`: the dispatch never got a reservation (its reserve failed
    /// after launch); one for zero is made when it is booked.
    reservation: Option<Reservation>,
    actual: MicroUsd,
    /// Unix seconds when it began to wait.
    since: i64,
    tries: u32,
}

/// The plain fields of a [`PendingSettle`] in `seat-settles.json`, so no
/// public type gains a derive.
#[derive(serde::Serialize, serde::Deserialize)]
struct SettleRow {
    dispatch: String,
    actual: i64,
    since: i64,
    tries: u32,
    reservation: Option<ReservationRow>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ReservationRow {
    id: String,
    amount: i64,
    node: String,
    seat_dispatch: String,
    holder_kind: String,
    holder_id: String,
    epoch: u64,
    expires_at: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SettleFile {
    settles: Vec<SettleRow>,
}

fn kind_name(kind: PrincipalKind) -> &'static str {
    match kind {
        PrincipalKind::Operator => "operator",
        PrincipalKind::Node => "node",
        PrincipalKind::Task => "task",
    }
}

fn kind_from(name: &str) -> Option<PrincipalKind> {
    match name {
        "operator" => Some(PrincipalKind::Operator),
        "node" => Some(PrincipalKind::Node),
        "task" => Some(PrincipalKind::Task),
        _ => None,
    }
}

impl SettleRow {
    /// `None` for a settle whose reservation is not under a seat (none is).
    fn of(pending: &PendingSettle) -> Option<Self> {
        let reservation = match &pending.reservation {
            None => None,
            Some(reservation) => {
                let ResourceId::Seat { node, dispatch } = &reservation.seat.resource else {
                    return None;
                };
                Some(ReservationRow {
                    id: reservation.id.clone(),
                    amount: reservation.amount.to_micros(),
                    node: node.clone(),
                    seat_dispatch: dispatch.clone(),
                    holder_kind: kind_name(reservation.seat.holder.kind).into(),
                    holder_id: reservation.seat.holder.id.clone(),
                    epoch: reservation.seat.epoch,
                    expires_at: reservation.seat.expires_at,
                })
            }
        };
        Some(Self {
            dispatch: pending.dispatch.clone(),
            actual: pending.actual.to_micros(),
            since: pending.since,
            tries: pending.tries,
            reservation,
        })
    }

    fn into_pending(self) -> Result<PendingSettle, SettleFileError> {
        let reservation = match self.reservation {
            None => None,
            Some(row) => Some(Reservation {
                id: row.id,
                amount: MicroUsd::from_micros(row.amount),
                seat: LeaseToken {
                    resource: ResourceId::Seat {
                        node: row.node,
                        dispatch: row.seat_dispatch,
                    },
                    holder: Principal {
                        kind: kind_from(&row.holder_kind)
                            .ok_or_else(|| SettleFileError::HolderKind(row.holder_kind.clone()))?,
                        id: row.holder_id,
                    },
                    epoch: row.epoch,
                    expires_at: row.expires_at,
                },
            }),
        };
        Ok(PendingSettle {
            dispatch: self.dispatch,
            reservation,
            actual: MicroUsd::from_micros(self.actual),
            since: self.since,
            tries: self.tries,
        })
    }
}

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
    /// Settles the primary has not taken yet, oldest first. They survive a
    /// restart in [`SETTLES_FILE`] when the book has one.
    settles: Vec<PendingSettle>,
    /// Where the waiting settles are kept, when they are (an external
    /// layer and a ledger); `None` writes nothing.
    store: Option<PathBuf>,
    /// The waiting settles changed since the file was last written.
    dirty: bool,
    /// The last write of the file failed (said once per change).
    store_failing: bool,
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

    /// Write the waiting settles when they changed: owner-only (the rows
    /// carry seat tokens), directory fsynced. An empty list removes the
    /// file. A failure is one line per change of state.
    fn persist(&mut self, log: &LogSink) {
        let Some(path) = self.store.clone() else {
            self.dirty = false;
            return;
        };
        if !self.dirty {
            return;
        }
        let written = if self.settles.is_empty() {
            match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        } else {
            let rows: Vec<SettleRow> = self.settles.iter().filter_map(SettleRow::of).collect();
            if rows.len() < self.settles.len() {
                log(&format!(
                    "seat settles: {} waiting settle(s) have a reservation that is not under a \
                     seat and are not written to {}",
                    self.settles.len() - rows.len(),
                    path.display()
                ));
            }
            let file = SettleFile { settles: rows };
            serde_json::to_string(&file)
                .map_err(std::io::Error::other)
                .and_then(|json| crate::fsutil::write_atomic_with_mode(&path, &json, Some(0o600)))
        };
        match written {
            Ok(()) => {
                self.dirty = false;
                if std::mem::take(&mut self.store_failing) {
                    log(&format!(
                        "seat settles: {} is written again",
                        path.display()
                    ));
                }
            }
            Err(e) => {
                if !self.store_failing {
                    log(&format!(
                        "seat settles: could not write {}: {e}; waiting settles are lost if the \
                         coordinator restarts before the primary takes them",
                        path.display()
                    ));
                }
                self.store_failing = true;
            }
        }
    }

    /// Queue a settle, newest last.
    fn push_settle(
        &mut self,
        dispatch: String,
        reservation: Option<Reservation>,
        actual: MicroUsd,
        now: i64,
    ) {
        self.settles.push(PendingSettle {
            dispatch,
            reservation,
            actual,
            since: now,
            tries: 0,
        });
        self.dirty = true;
    }

    /// Drop the settles that waited past [`SETTLE_KEEP_SECS`] and all but
    /// the newest [`MAX_WAITING_SETTLES`]: one line for everything dropped.
    fn cap_settles(&mut self, shared: &dyn SharedState, log: &LogSink, now: i64) {
        let mut reachable = true;
        let mut dropped: Vec<PendingSettle> = Vec::new();
        let (keep, old): (Vec<_>, Vec<_>) = std::mem::take(&mut self.settles)
            .into_iter()
            .partition(|pending| now.saturating_sub(pending.since) <= SETTLE_KEEP_SECS);
        dropped.extend(old);
        self.settles = keep;
        if self.settles.len() > MAX_WAITING_SETTLES {
            let excess = self.settles.len() - MAX_WAITING_SETTLES;
            dropped.extend(self.settles.drain(..excess));
        }
        if dropped.is_empty() {
            return;
        }
        self.dirty = true;
        let total = dropped
            .iter()
            .fold(MicroUsd::from_micros(0), |sum, pending| {
                sum.saturating_add(pending.actual)
            });
        let oldest = dropped
            .iter()
            .min_by_key(|pending| pending.since)
            .map_or("", |pending| pending.dispatch.as_str());
        for pending in &dropped {
            self.failing
                .remove(&(pending.dispatch.clone(), SeatOp::Settle));
            // Seats of their own go back until the primary is found
            // unreachable; the rest end at their time limit.
            if reachable {
                reachable = self.release_settle_seat(shared, log, pending.reservation.as_ref());
            }
        }
        log(&format!(
            "seat settles: dropped {} waiting settle(s) totalling {total} (oldest dispatch: \
             {oldest}); not booked to the shared counter; check the shared primary",
            dropped.len()
        ));
    }

    /// Give back the seat a settle made for itself (`settle:<dispatch>`),
    /// when its reservation is under one; never the dispatch's own seat. A
    /// failure is the book's one line per change, and the seat ends at its
    /// time limit. Answers `false` when the primary was unreachable.
    fn release_settle_seat(
        &mut self,
        shared: &dyn SharedState,
        log: &LogSink,
        reservation: Option<&Reservation>,
    ) -> bool {
        let Some(reservation) = reservation else {
            return true;
        };
        match &reservation.seat.resource {
            ResourceId::Seat { dispatch, .. } if dispatch.starts_with("settle:") => {
                self.release(shared, log, dispatch, &reservation.seat)
            }
            _ => true,
        }
    }

    /// The reservation a settle with none is booked under: for zero, on the
    /// dispatch's own lease when the book holds one (never re-acquiring that
    /// seat, which would bump its epoch), else on a seat of its own,
    /// `settle:<dispatch>`. A reservation for zero cannot exhaust the slice,
    /// and the settle that follows is a true-up.
    fn reserve_for_settle(
        &mut self,
        shared: &dyn SharedState,
        log: &LogSink,
        holder: &Principal,
        dispatch: &str,
    ) -> Result<Reservation, SharedStateError> {
        let zero = MicroUsd::from_micros(0);
        if let Some(lease) = self.leases.get(dispatch).cloned() {
            match shared.reserve_spend(&lease, zero) {
                Err(SharedStateError::LeaseLost { .. }) => {
                    // The work is over: the fence protects nothing now.
                    self.leases.remove(dispatch);
                }
                other => return other,
            }
        }
        let seat = shared.acquire_seat(holder, &format!("settle:{dispatch}"))?;
        match shared.reserve_spend(&seat, zero) {
            Ok(reservation) => Ok(reservation),
            Err(e) => {
                self.release(shared, log, &format!("settle:{dispatch}"), &seat);
                Err(e)
            }
        }
    }

    /// Hand the waiting settles to the primary, oldest first; what it does
    /// not take waits for the next sync. A settle is never refused for a
    /// stale seat (an-t11). An outage stops the pass: the rest are not
    /// tried until the next sync. A settle the primary refuses (`Refused`)
    /// is dropped, deliberately: a retry repeats the refusal. Any other
    /// failure is kept and the pass goes on. A settle with no reservation is
    /// booked only on a [`Pass::Reconcile`], so one request keeps a bounded
    /// number of shared calls; its new reservation is stored, and the file
    /// written, before the settle is sent (and not sent when the write
    /// failed), so a settle whose answer was lost retries on the same
    /// reservation and is never booked twice. Returns `false` when the
    /// primary was found unreachable.
    fn flush_settles(
        &mut self,
        shared: &dyn SharedState,
        holder: &Principal,
        log: &LogSink,
        now: i64,
        pass: Pass,
    ) -> bool {
        self.cap_settles(shared, log, now);
        let mut reachable = true;
        let mut at = 0;
        while at < self.settles.len() {
            let id = self.settles[at].dispatch.clone();
            let reservation = match self.settles[at].reservation.clone() {
                Some(reservation) => reservation,
                None if pass == Pass::Request => {
                    at += 1;
                    continue;
                }
                None => match self.reserve_for_settle(shared, log, holder, &id) {
                    Ok(reservation) => {
                        self.settles[at].reservation = Some(reservation.clone());
                        self.dirty = true;
                        self.persist(log);
                        if self.dirty {
                            // The file could not be written: a settle sent
                            // now could book twice after a restart. The
                            // reservation was never sent, so it is dropped
                            // and the next pass makes and writes a new one.
                            self.settles[at].reservation = None;
                            self.release_settle_seat(shared, log, Some(&reservation));
                            at += 1;
                            continue;
                        }
                        reservation
                    }
                    Err(reserve_error) => {
                        let outage = matches!(reserve_error, SharedStateError::Unreachable { .. });
                        self.settles[at].tries += 1;
                        self.fail(
                            log,
                            &id,
                            SeatOp::Settle,
                            format!(
                                "seat {id}: booking the spend failed: {reserve_error}; it is \
                                 retried at the next sync"
                            ),
                        );
                        if outage {
                            reachable = false;
                            break;
                        }
                        at += 1;
                        continue;
                    }
                },
            };
            let actual = self.settles[at].actual;
            match shared.settle_spend(&reservation, actual) {
                Ok(_) => {
                    self.recover(
                        log,
                        &id,
                        SeatOp::Settle,
                        &format!("seat {id}: settle recovered"),
                    );
                    self.settles.remove(at);
                    self.dirty = true;
                    // A seat made for this settle alone goes back at once.
                    self.release_settle_seat(shared, log, Some(&reservation));
                }
                Err(settle_error @ SharedStateError::Unreachable { .. }) => {
                    // An outage: kept, and the pass stops here.
                    self.settles[at].tries += 1;
                    self.fail(
                        log,
                        &id,
                        SeatOp::Settle,
                        format!(
                            "seat {id}: booking the spend failed: {settle_error}; it is retried \
                             at the next sync"
                        ),
                    );
                    reachable = false;
                    break;
                }
                Err(settle_error @ SharedStateError::Refused { .. }) => {
                    // The primary answered and refused it: retrying would
                    // repeat the refusal forever, so it is dropped
                    // (deliberate, decided-by: agent, ADR-0080). The shared
                    // counter never books it. The error already reads
                    // `refused: <reason>`.
                    self.failing.remove(&(id.clone(), SeatOp::Settle));
                    let dropped = self.settles.remove(at);
                    self.release_settle_seat(shared, log, dropped.reservation.as_ref());
                    self.dirty = true;
                    log(&format!(
                        "seat {id}: booking the spend was not accepted: {settle_error}; dropped, \
                         the shared counter does not count it; if it repeats, check the shared \
                         primary"
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
                    self.settles[at].tries += 1;
                    self.fail(
                        log,
                        &id,
                        SeatOp::Settle,
                        format!(
                            "seat {id}: booking the spend failed: {settle_error}; it is retried \
                             at the next sync"
                        ),
                    );
                    at += 1;
                }
            }
        }
        self.persist(log);
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
    clock: UnixClock,
    /// Whether the shared state keeps a counter these settles feed: a
    /// single node (`LocalState`) has none, so a settle with no
    /// reservation is not queued and no file is written.
    feeds_counter: bool,
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
        let feeds_counter = shared.selection() == Selection::External;
        Self {
            shared,
            holder,
            book: Mutex::new(Book::default()),
            log: Arc::new(|line: &str| super::log_line(line)),
            clock: Arc::new(system_clock),
            feeds_counter,
        }
    }

    /// Stamp waiting settles with another clock (a test's scripted one).
    pub fn with_clock(mut self, clock: UnixClock) -> Self {
        self.clock = clock;
        self
    }

    /// Keep the waiting settles in `path` (see [`settles_path`]) and take
    /// back those a coordinator that ended left there. Nothing is kept when
    /// the shared state has no counter. A file that cannot be read is
    /// renamed aside (`<name>.unreadable-<unix-secs>`) and named in one
    /// line; the book starts empty and the coordinator still starts. Call
    /// it after [`Self::with_log`] and [`Self::with_clock`].
    pub fn with_settle_file(self, path: PathBuf) -> Self {
        if !self.feeds_counter {
            return self;
        }
        let log = Arc::clone(&self.log);
        let now = (self.clock)();
        let mut book = self.book();
        book.store = Some(path.clone());
        let read: Result<Option<Vec<PendingSettle>>, String> = match std::fs::read_to_string(&path)
        {
            Ok(text) => serde_json::from_str::<SettleFile>(&text)
                .map_err(|e| SettleFileError::Json(e).to_string())
                .and_then(|file| {
                    file.settles
                        .into_iter()
                        .map(|row| row.into_pending().map_err(|e| e.to_string()))
                        .collect::<Result<Vec<_>, _>>()
                })
                .map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        };
        match read {
            Ok(Some(rows)) => book.settles = rows,
            Ok(None) => {}
            Err(cause) => {
                let mut name = path.clone().into_os_string();
                name.push(format!(".unreadable-{now}"));
                let aside = PathBuf::from(name);
                match std::fs::rename(&path, &aside) {
                    Ok(()) => log(&format!(
                        "seat settles: {} could not be read ({cause}); kept aside as {}; its \
                         settles are not booked to the shared counter; the book starts empty",
                        path.display(),
                        aside.display()
                    )),
                    Err(e) => {
                        // Nothing may overwrite a file the daemon could not read.
                        book.store = None;
                        log(&format!(
                            "seat settles: {} could not be read ({cause}) and could not be moved \
                             aside ({e}); the book starts empty and keeps nothing on disk until \
                             the file is fixed and the coordinator restarted",
                            path.display()
                        ));
                    }
                }
            }
        }
        drop(book);
        self
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
        let now = (self.clock)();
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
            let actual = MicroUsd::from_micros(booked.booked_micros);
            match book.reservations.remove(&booked.dispatch_id) {
                Some(reservation) => {
                    book.push_settle(booked.dispatch_id, Some(reservation), actual, now);
                }
                // No reservation: its reserve failed after launch. The
                // spend is queued all the same (booked at the next tick),
                // unless it was nothing or there is no counter to feed.
                None if self.feeds_counter && booked.booked_micros > 0 => {
                    book.push_settle(booked.dispatch_id, None, actual, now);
                }
                None => {}
            }
        }
        for id in gone {
            if let Some(reservation) = book.reservations.remove(&id) {
                book.push_settle(id, Some(reservation), MicroUsd::from_micros(0), now);
            }
        }
        book.persist(log);
        let pass = if matches!(acquire, Acquire::Every) {
            Pass::Reconcile
        } else {
            Pass::Request
        };
        let mut reachable = book.flush_settles(shared, &self.holder, log, now, pass);

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
        let settling: BTreeSet<String> = book
            .settles
            .iter()
            .map(|pending| pending.dispatch.clone())
            .collect();
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
                    // Its work may be running: the seat stays, and its
                    // spend is booked to the counter when it settles
                    // (queued without a reservation, see `flush_settles`).
                    reachable = !matches!(reserve_error, SharedStateError::Unreachable { .. });
                    book.leases.insert(id.to_string(), token);
                    book.refused.remove(id);
                    book.fail(
                        log,
                        id,
                        SeatOp::Reserve,
                        format!(
                            "seat {id}: reserve failed: {reserve_error}; the spend is booked to \
                             the shared counter when it settles"
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
        let now = (self.clock)();
        let mut book = self.book();
        let book = &mut *book;
        if !book.flush_settles(shared, &self.holder, log, now, Pass::Reconcile) {
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
