//! What every integration test shares: the `relais` command, started clean.

use std::process::Command;

/// A command for the `relais` binary at `bin`, with every `RELAIS_*`
/// variable of the test's own environment removed.
///
/// The suite runs inside relais runs too (`make check` as a verification
/// command), whose environment carries `RELAIS_HOST`, `RELAIS_SESSION_ID`
/// and the rest. A test that means "no host" must not inherit one; each
/// test sets what it means on the returned command.
pub fn relais(bin: &str) -> Command {
    let mut command = Command::new(bin);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("RELAIS_") {
            command.env_remove(&key);
        }
    }
    command
}

/// How many worlds may be alive at once, across every module of this test
/// binary. A saturated machine turns a passing candidate into a
/// `behavioral_failure` when relais verifies it with this project's own
/// `make check`, which buys a repair attempt and can exhaust a run's wall
/// clock (relais#115). The bound is on worlds rather than on
/// `--test-threads` because a harness flag cannot be set per suite, and
/// scenarios that build no world have no reason to queue.
pub const MAX_LIVE_WORLDS: usize = 4;

/// How long `WorldPermit::acquire` waits before it calls the wait a hang.
const PERMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// The cap's state. Waiters are served in ticket order: a waiter for two
/// places used to be overtaken by every one-place waiter that arrived while
/// it waited, and under load it starved past `PERMIT_WAIT` (three scenarios
/// on 2026-10-26, each waiting for 2 of 4 with 0 free).
struct Permits {
    free: usize,
    /// The ticket the next waiter takes.
    next: u64,
    /// The only ticket allowed to take places now.
    serving: u64,
    /// Tickets whose waiter gave up; the queue moves past them.
    abandoned: std::collections::BTreeSet<u64>,
}

impl Permits {
    /// Pass the turn to the next ticket still waiting.
    fn advance(&mut self) {
        self.serving += 1;
        while self.abandoned.remove(&self.serving) {
            self.serving += 1;
        }
    }
}

/// A cap of places served in ticket order. One process-wide instance bounds
/// the worlds ([`WorldPermit`]); a test builds its own with a short `wait`.
pub struct Gate {
    cap: usize,
    wait: std::time::Duration,
    permits: std::sync::Mutex<Permits>,
    returned: std::sync::Condvar,
}

impl Gate {
    pub const fn new(cap: usize, wait: std::time::Duration) -> Self {
        Self {
            cap,
            wait,
            permits: std::sync::Mutex::new(Permits {
                free: cap,
                next: 0,
                serving: 0,
                abandoned: std::collections::BTreeSet::new(),
            }),
            returned: std::sync::Condvar::new(),
        }
    }

    /// Waits for its turn and for `count` free places, then takes them. A
    /// `count` outside `1..=cap` is refused before any wait. A wait past
    /// `wait` panics, naming what it asked for and what was free.
    pub fn take(&self, count: usize) {
        assert!(
            (1..=self.cap).contains(&count),
            "a permit of {count} worlds can never be granted"
        );
        // A poisoned lock is reused: no critical section below can panic
        // (the explicit panic runs after the guard is dropped), so a
        // panicking scenario elsewhere leaves the queue readable.
        let mut permits = self
            .permits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ticket = permits.next;
        permits.next += 1;
        let deadline = std::time::Instant::now() + self.wait;
        while permits.serving != ticket || permits.free < count {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                let free = permits.free;
                // Give the turn up before panicking, so the waiters behind
                // this one are not stuck behind a ticket nobody will take.
                if permits.serving == ticket {
                    permits.advance();
                } else {
                    permits.abandoned.insert(ticket);
                }
                drop(permits);
                self.returned.notify_all();
                panic!(
                    "waited {:?} for {count} of {} world permits, {free} free: a test \
                     holding permits while it waits for more, or scenarios slowed by \
                     machine load",
                    self.wait, self.cap
                );
            }
            permits = self
                .returned
                .wait_timeout(permits, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        permits.free -= count;
        permits.advance();
        drop(permits);
        // The next ticket may be able to go now.
        self.returned.notify_all();
    }

    pub fn give(&self, count: usize) {
        let mut permits = self
            .permits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        permits.free += count;
        drop(permits);
        // Every waiter, not one: only the ticket being served may go, and a
        // single wakeup could land on another.
        self.returned.notify_all();
    }
}

/// The process-wide cap on live worlds. The slowest scenario takes well
/// under a minute, so a wait of `PERMIT_WAIT` is a hang.
static WORLDS: Gate = Gate::new(MAX_LIVE_WORLDS, PERMIT_WAIT);

/// `count` places in the cap, returned together when this drops. A test
/// holding several worlds takes one permit for all of them and shares it
/// (`Arc`), so it never waits on itself while holding part of the cap.
pub struct WorldPermit {
    count: usize,
}

impl WorldPermit {
    /// See [`Gate::take`].
    pub fn acquire(count: usize) -> Self {
        WORLDS.take(count);
        Self { count }
    }
}

impl Drop for WorldPermit {
    fn drop(&mut self) {
        WORLDS.give(self.count);
    }
}

#[cfg(test)]
mod gate_tests {
    use super::Gate;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Waits until `gate` has handed out exactly `next` tickets, so threads
    /// queue in a known order.
    fn queued(gate: &Gate, next: u64) {
        while gate.permits.lock().unwrap().next < next {
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_two_place_waiter_is_served_before_later_one_place_waiters() {
        let gate = Arc::new(Gate::new(4, Duration::from_secs(30)));
        gate.take(1);
        gate.take(1);
        gate.take(1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut threads = Vec::new();
        for (name, count) in [("two", 2), ("one-a", 1), ("one-b", 1)] {
            let (waiter, order) = (gate.clone(), order.clone());
            let next = gate.permits.lock().unwrap().next;
            threads.push(std::thread::spawn(move || {
                waiter.take(count);
                order.lock().unwrap().push(name);
            }));
            queued(&gate, next + 1);
        }
        // One place is free, but the head of the queue wants two: the
        // one-place waiters behind it must not overtake it.
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            order.lock().unwrap().is_empty(),
            "{:?}",
            order.lock().unwrap()
        );
        // Two places free: only the head can be served, so "two" is
        // checked alone. Giving back all three at once would let a
        // one-place waiter be served next and push its name before "two"
        // pushes its own (#338).
        gate.give(1);
        let mut threads = threads.into_iter();
        threads
            .next()
            .expect("the two-place waiter")
            .join()
            .unwrap();
        assert_eq!(*order.lock().unwrap(), ["two"]);
        gate.give(2);
        for thread in threads {
            thread.join().unwrap();
        }
    }

    #[test]
    fn a_waiter_that_gives_up_does_not_block_the_queue() {
        let gate = Arc::new(Gate::new(2, Duration::from_millis(200)));
        gate.take(2);
        // The head times out, and so does the waiter behind it; both pass
        // their turn on, so a later waiter is served once places return.
        let head = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.take(2))
        };
        queued(&gate, 2);
        let behind = {
            let gate = gate.clone();
            std::thread::spawn(move || gate.take(1))
        };
        assert!(head.join().is_err(), "the head gives up after its wait");
        assert!(behind.join().is_err(), "so does the waiter behind it");
        gate.give(2);
        gate.take(1);
        let state = gate.permits.lock().unwrap_or_else(|p| p.into_inner());
        // Tickets 0 (the first take), 1 and 2 (the two that gave up) and 3
        // (the last take) were all served or passed: nothing is pending.
        assert_eq!((state.free, state.serving, state.next), (1, 4, 4));
        assert!(state.abandoned.is_empty(), "{:?}", state.abandoned);
    }
}
