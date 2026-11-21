//! The ledger's clocks: where its timestamps come from.

/// Where the ledger's timestamps come from. The wall clock in
/// production; a fixed or scripted clock in tests, so a transition's
/// `at`, a dispatch's `created_at` and the dataset's temporal splits are
/// assertable values rather than "whenever the test ran".
pub trait Clock: Send + Sync {
    fn now_rfc3339(&self) -> String;

    /// Milliseconds on a monotonic scale that does NOT advance while the
    /// host sleeps. The default is a constant that reads no timestamp, so a
    /// scripted clock reports no suspend unless a test overrides both
    /// readings.
    fn awake_millis(&self) -> i64 {
        0
    }

    /// Milliseconds on a monotonic scale that keeps counting through a
    /// host sleep and never jumps. Only the difference to
    /// [`Clock::awake_millis`] over a window means anything: the scales
    /// have unrelated origins.
    fn continuous_millis(&self) -> i64 {
        0
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_rfc3339(&self) -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn awake_millis(&self) -> i64 {
        static ANCHOR: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let anchor = ANCHOR.get_or_init(std::time::Instant::now);
        i64::try_from(anchor.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    /// One source for the life of the process: the sleep-counting clock
    /// when the first read succeeds, else the awake clock (no suspend is
    /// ever seen). A later failed read keeps the last gap, so it reports no
    /// new sleep instead of mixing two scales with unrelated origins.
    fn continuous_millis(&self) -> i64 {
        use std::sync::atomic::{AtomicI64, Ordering};
        static LAST_GAP: AtomicI64 = AtomicI64::new(0);
        static SLEEP_COUNTING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let awake = self.awake_millis();
        let available = *SLEEP_COUNTING.get_or_init(|| {
            crate::procs::sleep_counting_millis().is_some_and(|continuous| {
                LAST_GAP.store(continuous - awake, Ordering::Relaxed);
                true
            })
        });
        if !available {
            return awake;
        }
        match crate::procs::sleep_counting_millis() {
            Some(continuous) => {
                LAST_GAP.store(continuous - awake, Ordering::Relaxed);
                continuous
            }
            None => awake + LAST_GAP.load(Ordering::Relaxed),
        }
    }
}

/// A clock that answers a scripted sequence and then repeats its last
/// value: `FixedClock::new(["2026-09-18T10:00:00+00:00", …])`.
pub struct FixedClock {
    times: std::sync::Mutex<(Vec<String>, usize)>,
}

impl FixedClock {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(times: I) -> Self {
        let times: Vec<String> = times.into_iter().map(Into::into).collect();
        assert!(!times.is_empty(), "a fixed clock needs at least one time");
        Self {
            times: std::sync::Mutex::new((times, 0)),
        }
    }
}

impl Clock for FixedClock {
    fn now_rfc3339(&self) -> String {
        // A poisoned clock is still a list of times: the panic that
        // poisoned it is the caller's to report, not this clock's to
        // re-raise on every later read (the coordinator's `lock_state`
        // recovers its admission lock the same way).
        let mut guard = self
            .times
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (times, index) = &mut *guard;
        let now = times[(*index).min(times.len() - 1)].clone();
        *index += 1;
        now
    }
}

/// The wall clock, for callers that stamp outside the ledger (the CLI's
/// `report --since` default, dataset build time).
pub fn now_rfc3339() -> String {
    SystemClock.now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::{Clock, SystemClock};

    /// The real clocks, not a script: on darwin and Linux the
    /// sleep-counting clock reads, and with no sleep between two readings
    /// the gap between it and the awake clock barely moves.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_real_clocks_read_and_report_no_sleep_when_the_host_is_awake() {
        assert!(
            crate::procs::sleep_counting_millis().is_some(),
            "the sleep-counting clock reads on this platform"
        );
        let clock = SystemClock;
        let gap_before = clock.continuous_millis() - clock.awake_millis();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let gap_after = clock.continuous_millis() - clock.awake_millis();
        assert!(
            (gap_after - gap_before).abs() < 50,
            "the gap moved {} ms with the host awake",
            gap_after - gap_before
        );
    }
}
