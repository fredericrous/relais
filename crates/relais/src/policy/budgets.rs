//! The four budgets a run consumes (relais ADR-0010, SPEC §9, §10, §11, §23).
//!
//! * `work` — attempts, candidate verification and review after the
//!   baseline (the repository's `max_wall_seconds`, narrowed by
//!   `limits.wall_seconds`);
//! * `admission_wait` — cumulative time queued for a lease;
//! * `disk_wait` — cumulative time waiting for the disk floor;
//! * `total` — end to end from the start of the run.
//!
//! A wait consumes its own budget and `total`, never `work`: the time a
//! queue held a run is not time its worker had. Every command starts with
//! the smaller of its own timeout and what is left of each budget that
//! applies to it.
//!
//! [`Budgets`] holds no clock of its own. It reads the one it is given
//! ([`BudgetClock`]); the runner hands it the ledger's, so a test that
//! scripts the ledger's clock scripts the budgets too.

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where the budgets read the time: milliseconds on any one fixed scale.
pub trait BudgetClock: Send + Sync {
    fn now_millis(&self) -> i64;
}

/// The wall clock.
pub struct WallClock;

impl BudgetClock for WallClock {
    fn now_millis(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// Which budget ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    /// `work_seconds`: ends `budget_exhausted (wall_clock)`.
    Work,
    /// `admission_wait_seconds`: ends `blocked (admission_wait_exhausted)`.
    AdmissionWait,
    /// `disk_wait_seconds`: ends `blocked (disk_low)`.
    DiskWait,
    /// `total_seconds`: ends `budget_exhausted (total)`.
    Total,
}

/// Which wait is being charged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    Admission,
    Disk,
}

/// The size of each budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLimits {
    pub work: Duration,
    pub admission_wait: Duration,
    pub disk_wait: Duration,
    pub total: Duration,
}

/// `[resources] admission_wait_seconds` when machine.toml does not say.
pub const DEFAULT_ADMISSION_WAIT_SECONDS: u64 = 1800;
/// `[resources] disk_wait_seconds` when machine.toml does not say.
pub const DEFAULT_DISK_WAIT_SECONDS: u64 = 600;

impl BudgetLimits {
    /// The limits of one run. `total` is the contract's
    /// `limits.total_seconds`; absent, it is everything the run may
    /// legitimately spend: the baseline's own command timeouts, the work
    /// budget and both wait budgets.
    pub fn resolve(
        work_seconds: u64,
        admission_wait_seconds: u64,
        disk_wait_seconds: u64,
        total_seconds: Option<u64>,
        baseline_command_seconds: u64,
    ) -> Self {
        let total = total_seconds.unwrap_or_else(|| {
            baseline_command_seconds
                .saturating_add(work_seconds)
                .saturating_add(admission_wait_seconds)
                .saturating_add(disk_wait_seconds)
        });
        Self {
            work: Duration::from_secs(work_seconds),
            admission_wait: Duration::from_secs(admission_wait_seconds),
            disk_wait: Duration::from_secs(disk_wait_seconds),
            total: Duration::from_secs(total),
        }
    }

    /// No budget binds: for a caller with no run around it.
    pub fn unbounded() -> Self {
        let forever = Duration::from_secs(u64::MAX / 4);
        Self {
            work: forever,
            admission_wait: forever,
            disk_wait: forever,
            total: forever,
        }
    }
}

/// What has been spent. Milliseconds on the clock's scale.
#[derive(Debug)]
struct Spent {
    run_started: i64,
    work_started: Option<i64>,
    admission_waited: i64,
    disk_waited: i64,
    /// Waits charged since `work_started`: excluded from the work budget.
    waited_in_work: i64,
}

/// One run's budgets, shared by everything that spends them. Cloning
/// shares the account.
#[derive(Clone)]
pub struct Budgets {
    clock: Arc<dyn BudgetClock>,
    limits: BudgetLimits,
    spent: Arc<Mutex<Spent>>,
}

impl std::fmt::Debug for Budgets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Budgets")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

fn from_millis(millis: i64) -> Duration {
    Duration::from_millis(u64::try_from(millis.max(0)).unwrap_or(0))
}

impl Budgets {
    /// Budgets for a run that began at `run_started` on `clock`'s scale.
    pub fn started_at(clock: Arc<dyn BudgetClock>, limits: BudgetLimits, run_started: i64) -> Self {
        Self {
            clock,
            limits,
            spent: Arc::new(Mutex::new(Spent {
                run_started,
                work_started: None,
                admission_waited: 0,
                disk_waited: 0,
                waited_in_work: 0,
            })),
        }
    }

    /// Budgets that begin now.
    pub fn start(clock: Arc<dyn BudgetClock>, limits: BudgetLimits) -> Self {
        let now = clock.now_millis();
        Self::started_at(clock, limits, now)
    }

    /// Budgets no run can exhaust, for a caller outside a run.
    pub fn unbounded() -> Self {
        Self::start(Arc::new(WallClock), BudgetLimits::unbounded())
    }

    pub fn limits(&self) -> BudgetLimits {
        self.limits
    }

    /// The clock's reading, for a caller timing a wait.
    pub fn now(&self) -> i64 {
        self.clock.now_millis()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Spent> {
        // A poisoned account is still an account: the panic that
        // poisoned it is its thrower's to report.
        self.spent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The baseline is over: work begins now. Time spent so far counts
    /// against `total` only.
    pub fn begin_work(&self) {
        let now = self.now();
        let mut spent = self.lock();
        if spent.work_started.is_none() {
            spent.work_started = Some(now);
            spent.waited_in_work = 0;
        }
    }

    /// Charge `from..now` to a wait budget, and return the new mark.
    /// A wait consumes its own budget and `total` (which reads the clock
    /// and so needs no charge), and is taken out of `work`.
    pub fn charge_wait(&self, kind: WaitKind, from: i64) -> i64 {
        let now = self.now();
        let waited = (now - from).max(0);
        let mut spent = self.lock();
        match kind {
            WaitKind::Admission => spent.admission_waited += waited,
            WaitKind::Disk => spent.disk_waited += waited,
        }
        if spent.work_started.is_some() {
            spent.waited_in_work += waited;
        }
        now
    }

    /// Start timing a wait. [`Wait::tick`] charges what has passed and
    /// says whether a budget ran out; dropping the guard charges the rest.
    pub fn wait(&self, kind: WaitKind) -> Wait {
        Wait {
            budgets: self.clone(),
            kind,
            mark: self.now(),
        }
    }

    /// What remains of the total.
    pub fn total_remaining(&self) -> Duration {
        let now = self.now();
        let spent = self.lock();
        self.limits
            .total
            .saturating_sub(from_millis(now - spent.run_started))
    }

    /// What remains of the work budget; all of it before work begins.
    pub fn work_remaining(&self) -> Duration {
        let now = self.now();
        let spent = self.lock();
        match spent.work_started {
            None => self.limits.work,
            Some(started) => {
                let worked = (now - started - spent.waited_in_work).max(0);
                self.limits.work.saturating_sub(from_millis(worked))
            }
        }
    }

    /// What remains for a command: base-tree setup and baseline commands
    /// draw on `total`; once work has begun, on `work` and `total`.
    pub fn remaining(&self) -> Duration {
        let total = self.total_remaining();
        if self.lock().work_started.is_some() {
            total.min(self.work_remaining())
        } else {
            total
        }
    }

    /// A command's timeout: its own, or what is left of the budgets that
    /// apply to it, whichever is smaller.
    pub fn clip(&self, own: Duration) -> Duration {
        own.min(self.remaining())
    }

    /// The first budget found spent, `total` before the others.
    pub fn exhausted(&self) -> Option<Exhausted> {
        if self.total_remaining().is_zero() {
            return Some(Exhausted::Total);
        }
        let started = self.lock().work_started.is_some();
        if started && self.work_remaining().is_zero() {
            return Some(Exhausted::Work);
        }
        let spent = self.lock();
        if from_millis(spent.admission_waited) >= self.limits.admission_wait
            && !self.limits.admission_wait.is_zero()
        {
            return Some(Exhausted::AdmissionWait);
        }
        if from_millis(spent.disk_waited) >= self.limits.disk_wait
            && !self.limits.disk_wait.is_zero()
        {
            return Some(Exhausted::DiskWait);
        }
        None
    }

    /// Whether a wait of this kind has a budget left to wait with.
    pub fn wait_exhausted(&self, kind: WaitKind) -> bool {
        let spent = self.lock();
        match kind {
            WaitKind::Admission => {
                from_millis(spent.admission_waited) >= self.limits.admission_wait
            }
            WaitKind::Disk => from_millis(spent.disk_waited) >= self.limits.disk_wait,
        }
    }

    /// Time charged to a wait budget so far.
    pub fn waited(&self, kind: WaitKind) -> Duration {
        let spent = self.lock();
        from_millis(match kind {
            WaitKind::Admission => spent.admission_waited,
            WaitKind::Disk => spent.disk_waited,
        })
    }

    /// Time spent working (waits excluded); zero before work begins.
    pub fn worked(&self) -> Duration {
        let now = self.now();
        let spent = self.lock();
        spent.work_started.map_or(Duration::ZERO, |started| {
            from_millis(now - started - spent.waited_in_work)
        })
    }
}

/// A wait being timed. Created by [`Budgets::wait`].
pub struct Wait {
    budgets: Budgets,
    kind: WaitKind,
    mark: i64,
}

impl Wait {
    /// Charge the time since the last tick, and report the first budget
    /// that is spent, the wait's own included.
    pub fn tick(&mut self) -> Option<Exhausted> {
        self.mark = self.budgets.charge_wait(self.kind, self.mark);
        self.budgets.exhausted()
    }
}

impl Drop for Wait {
    fn drop(&mut self) {
        self.budgets.charge_wait(self.kind, self.mark);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock a test moves by hand.
    struct ManualClock(std::sync::atomic::AtomicI64);

    impl ManualClock {
        fn new() -> Arc<Self> {
            Arc::new(Self(std::sync::atomic::AtomicI64::new(1_000_000)))
        }

        fn advance(&self, seconds: u64) {
            self.0
                .fetch_add(seconds as i64 * 1000, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl BudgetClock for ManualClock {
        fn now_millis(&self) -> i64 {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
    fn limits(work: u64, admission: u64, disk: u64, total: u64) -> BudgetLimits {
        BudgetLimits::resolve(work, admission, disk, Some(total), 0)
    }

    #[test]
    fn the_default_total_is_the_baseline_plus_work_plus_both_waits() {
        let l = BudgetLimits::resolve(2700, 1800, 600, None, 900);
        assert_eq!(l.total, Duration::from_secs(900 + 2700 + 1800 + 600));
        let l = BudgetLimits::resolve(2700, 1800, 600, Some(100), 900);
        assert_eq!(l.total, Duration::from_secs(100), "a written total wins");
    }

    #[test]
    fn an_admission_wait_consumes_its_budget_and_the_total_but_not_work() {
        let clock = ManualClock::new();
        let budgets = Budgets::start(clock.clone(), limits(100, 50, 50, 300));
        budgets.begin_work();
        {
            let mut wait = budgets.wait(WaitKind::Admission);
            clock.advance(30);
            assert_eq!(wait.tick(), None);
        }
        assert_eq!(budgets.waited(WaitKind::Admission), Duration::from_secs(30));
        assert_eq!(budgets.work_remaining(), Duration::from_secs(100));
        assert_eq!(budgets.total_remaining(), Duration::from_secs(270));
        clock.advance(10);
        assert_eq!(budgets.work_remaining(), Duration::from_secs(90));
        assert_eq!(budgets.total_remaining(), Duration::from_secs(260));
    }

    #[test]
    fn each_budget_names_itself_when_spent() {
        let clock = ManualClock::new();
        let budgets = Budgets::start(clock.clone(), limits(100, 50, 40, 1000));
        budgets.begin_work();
        let mut wait = budgets.wait(WaitKind::Admission);
        clock.advance(50);
        assert_eq!(wait.tick(), Some(Exhausted::AdmissionWait));
        drop(wait);

        let clock = ManualClock::new();
        let budgets = Budgets::start(clock.clone(), limits(100, 50, 40, 1000));
        let mut wait = budgets.wait(WaitKind::Disk);
        clock.advance(40);
        assert_eq!(wait.tick(), Some(Exhausted::DiskWait));
        drop(wait);

        let clock = ManualClock::new();
        let budgets = Budgets::start(clock.clone(), limits(100, 50, 40, 1000));
        budgets.begin_work();
        clock.advance(100);
        assert_eq!(budgets.exhausted(), Some(Exhausted::Work));

        let clock = ManualClock::new();
        let budgets = Budgets::start(clock.clone(), limits(100, 50, 40, 60));
        budgets.begin_work();
        clock.advance(60);
        assert_eq!(budgets.exhausted(), Some(Exhausted::Total));
    }

    #[test]
    fn a_command_timeout_is_clipped_to_what_is_left() {
        let clock = ManualClock::new();
        let budgets = Budgets::start(clock.clone(), limits(100, 50, 50, 500));
        // The baseline draws on total only: work is not started.
        assert_eq!(
            budgets.clip(Duration::from_secs(1000)),
            Duration::from_secs(500)
        );
        clock.advance(20);
        budgets.begin_work();
        clock.advance(70);
        assert_eq!(
            budgets.clip(Duration::from_secs(1000)),
            Duration::from_secs(30),
            "work has 30 s left, total 410"
        );
        assert_eq!(
            budgets.clip(Duration::from_secs(10)),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn the_total_clips_when_it_is_the_smaller() {
        let clock = ManualClock::new();
        let budgets = Budgets::start(clock.clone(), limits(100, 50, 50, 120));
        clock.advance(60);
        budgets.begin_work();
        assert_eq!(
            budgets.clip(Duration::from_secs(1000)),
            Duration::from_secs(60)
        );
    }
}
