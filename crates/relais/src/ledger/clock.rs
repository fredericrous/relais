//! The ledger's clocks: where its timestamps come from.

/// Where the ledger's timestamps come from. The wall clock in
/// production; a fixed or scripted clock in tests, so a transition's
/// `at`, a dispatch's `created_at` and the dataset's temporal splits are
/// assertable values rather than "whenever the test ran".
pub trait Clock: Send + Sync {
    fn now_rfc3339(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_rfc3339(&self) -> String {
        chrono::Utc::now().to_rfc3339()
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
