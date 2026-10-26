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

static PERMITS: std::sync::Mutex<Permits> = std::sync::Mutex::new(Permits {
    free: MAX_LIVE_WORLDS,
    next: 0,
    serving: 0,
    abandoned: std::collections::BTreeSet::new(),
});
static PERMITS_RETURNED: std::sync::Condvar = std::sync::Condvar::new();

/// `count` places in the cap, returned together when this drops. A test
/// holding several worlds takes one permit for all of them and shares it
/// (`Arc`), so it never waits on itself while holding part of the cap.
pub struct WorldPermit {
    count: usize,
}

impl WorldPermit {
    /// Waits for its turn and for `count` free places, then takes them. A `count` above
    /// `MAX_LIVE_WORLDS` is refused before any wait. A wait past
    /// `PERMIT_WAIT` panics, naming what it asked for and what was free: the
    /// slowest scenario takes well under a minute, so a wait that long is a
    /// test holding part of the cap while it waits for more, or scenarios
    /// slowed by a loaded machine.
    pub fn acquire(count: usize) -> Self {
        assert!(
            (1..=MAX_LIVE_WORLDS).contains(&count),
            "a permit of {count} worlds can never be granted"
        );
        // Poisoning is tolerated deliberately: a panicking scenario must
        // not wedge every scenario after it. The count is a plain usize
        // and a panic cannot leave it in a shape the next waiter cannot
        // read.
        let mut permits = PERMITS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ticket = permits.next;
        permits.next += 1;
        let deadline = std::time::Instant::now() + PERMIT_WAIT;
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
                PERMITS_RETURNED.notify_all();
                panic!(
                    "waited {PERMIT_WAIT:?} for {count} of {MAX_LIVE_WORLDS} world permits, \
                     {free} free: a test holding permits while it waits for more, \
                     or scenarios slowed by machine load"
                );
            }
            permits = PERMITS_RETURNED
                .wait_timeout(permits, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        permits.free -= count;
        permits.advance();
        drop(permits);
        // The next ticket may be able to go now.
        PERMITS_RETURNED.notify_all();
        Self { count }
    }
}

impl Drop for WorldPermit {
    fn drop(&mut self) {
        let mut permits = PERMITS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        permits.free += self.count;
        drop(permits);
        // Every waiter, not one: only the ticket being served may go, and a
        // single wakeup could land on another.
        PERMITS_RETURNED.notify_all();
    }
}
