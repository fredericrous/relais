//! The ledger's clocks: where its timestamps come from.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

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
        let anchor = PROCESS_CLOCKS.anchor.get_or_init(std::time::Instant::now);
        i64::try_from(anchor.elapsed().as_millis()).unwrap_or(i64::MAX)
    }

    /// One source for the life of the process: the sleep-counting clock
    /// when the first read succeeds, else the awake clock (no suspend is
    /// ever seen). See [`continuous_reading`].
    fn continuous_millis(&self) -> i64 {
        let awake = self.awake_millis();
        let available = *PROCESS_CLOCKS.sleep_counting.get_or_init(|| {
            crate::procs::sleep_counting_millis().is_some_and(|continuous| {
                PROCESS_CLOCKS
                    .last_gap
                    .store(continuous.saturating_sub(awake), Ordering::Relaxed);
                true
            })
        });
        let reading = if available {
            SleepCounting::Read(crate::procs::sleep_counting_millis())
        } else {
            SleepCounting::Unavailable
        };
        let last_gap = PROCESS_CLOCKS.last_gap.load(Ordering::Relaxed);
        let (continuous, gap) = continuous_reading(reading, awake, last_gap);
        PROCESS_CLOCKS.last_gap.store(gap, Ordering::Relaxed);
        continuous
    }
}

/// The per-process clock state `SystemClock` reads. Process-wide on purpose,
/// an exemption from `dependencies.explicit`: `Instant`'s anchor and whether
/// the sleep-counting clock reads are facts of the process, and every
/// `SystemClock` value must read them on one scale, or two budgets would
/// subtract readings with unrelated origins. Nothing reaches this state but
/// `SystemClock`, and callers reach `SystemClock` only through the injected
/// [`Clock`] trait, so a test scripts its clock instead.
struct ProcessClocks {
    anchor: OnceLock<std::time::Instant>,
    sleep_counting: OnceLock<bool>,
    last_gap: AtomicI64,
}

static PROCESS_CLOCKS: ProcessClocks = ProcessClocks {
    anchor: OnceLock::new(),
    sleep_counting: OnceLock::new(),
    last_gap: AtomicI64::new(0),
};

/// What the sleep-counting clock answered: unavailable for the life of the
/// process, or this read, which may fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SleepCounting {
    Unavailable,
    Read(Option<i64>),
}

/// The continuous reading from what the clocks answered, and the gap to
/// keep for the next one. With no sleep-counting clock it is the awake
/// reading: no suspend is ever seen. A successful read is the reading, and
/// its gap to the awake one is kept. A failed later read keeps `last_gap`,
/// so it reports no new sleep instead of mixing two scales with unrelated
/// origins.
fn continuous_reading(reading: SleepCounting, awake: i64, last_gap: i64) -> (i64, i64) {
    match reading {
        SleepCounting::Unavailable => (awake, last_gap),
        SleepCounting::Read(Some(continuous)) => (continuous, continuous.saturating_sub(awake)),
        SleepCounting::Read(None) => (awake.saturating_add(last_gap), last_gap),
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
    use super::{continuous_reading, Clock, SleepCounting, SystemClock};

    #[test]
    fn with_no_sleep_counting_clock_the_reading_is_the_awake_one() {
        assert_eq!(
            continuous_reading(SleepCounting::Unavailable, 1_000, 7_000),
            (1_000, 7_000)
        );
    }

    #[test]
    fn a_read_records_its_gap_and_a_later_failed_read_keeps_it() {
        let (reading, gap) = continuous_reading(SleepCounting::Read(Some(5_000)), 1_000, 0);
        assert_eq!((reading, gap), (5_000, 4_000));
        // The host stayed awake for 2 s, and the read then failed: the
        // reading moves with the awake clock, reporting no new sleep.
        assert_eq!(
            continuous_reading(SleepCounting::Read(None), 3_000, gap),
            (7_000, 4_000)
        );
    }

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
        // Loose enough for a preempted thread on a loaded host; a real
        // sleep, or a unit slip, moves it by seconds.
        assert!(
            (gap_after - gap_before).abs() < 500,
            "the gap moved {} ms with the host awake",
            gap_after - gap_before
        );
    }
}
