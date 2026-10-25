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

static FREE_PERMITS: std::sync::Mutex<usize> = std::sync::Mutex::new(MAX_LIVE_WORLDS);
static PERMITS_RETURNED: std::sync::Condvar = std::sync::Condvar::new();

/// `count` places in the cap, returned together when this drops. A test
/// holding several worlds takes one permit for all of them and shares it
/// (`Arc`), so it never waits on itself while holding part of the cap.
pub struct WorldPermit {
    count: usize,
}

impl WorldPermit {
    /// Waits until `count` places are free, then takes them. `count` must
    /// not exceed `MAX_LIVE_WORLDS`, or the wait never ends. A wait past
    /// `PERMIT_WAIT` panics, naming what it asked for and what was free:
    /// the slowest scenario takes well under a minute, so a wait that long
    /// is a test holding part of the cap while it waits for more.
    pub fn acquire(count: usize) -> Self {
        assert!(
            (1..=MAX_LIVE_WORLDS).contains(&count),
            "a permit of {count} worlds can never be granted"
        );
        // Poisoning is tolerated deliberately: a panicking scenario must
        // not wedge every scenario after it. The count is a plain usize
        // and a panic cannot leave it in a shape the next waiter cannot
        // read.
        let mut free = FREE_PERMITS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline = std::time::Instant::now() + PERMIT_WAIT;
        while *free < count {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(
                !left.is_zero(),
                "waited {PERMIT_WAIT:?} for {count} of {MAX_LIVE_WORLDS} world permits, \
                 {} free: a test is holding permits while it waits for more",
                *free
            );
            free = PERMITS_RETURNED
                .wait_timeout(free, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        *free -= count;
        Self { count }
    }
}

impl Drop for WorldPermit {
    fn drop(&mut self) {
        let mut free = FREE_PERMITS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *free += self.count;
        // Every waiter, not one: waiters ask for different counts, so the
        // one woken might still be unable to proceed while another could.
        PERMITS_RETURNED.notify_all();
    }
}
