use super::*;
use crate::admission::{
    Enforcement, HeartbeatStatus, LifecycleOutcome, LocalGate, ReleaseWriteOutcome, ResumeOutcome,
    RunRegistration, WaitOutcome, WithdrawOutcome, WriteLeaseOutcome,
};
use crate::policy::{BudgetClock, BudgetLimits, CommandSpec, ConcurrencyLimits};
use crate::verify::{run_observed_within, ObservedRun};
use std::path::Path;
use std::sync::atomic::AtomicI64;

/// A clock a test moves by hand, and a sleeper that moves it: a queue that
/// "waits" advances the clock by the poll and returns at once.
struct ManualClock(AtomicI64);

impl ManualClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(1_000_000)))
    }
}

impl BudgetClock for ManualClock {
    fn now_millis(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Moves `clock` by what it is asked to sleep, remembers each sleep, and
/// runs `on_sleep` (with the number of this sleep) so a test can change the
/// world while a queue "waits".
struct Advancing {
    clock: Arc<ManualClock>,
    slept: Mutex<Vec<Duration>>,
    on_sleep: Box<dyn Fn(usize) + Send + Sync>,
}

impl Advancing {
    fn new(clock: &Arc<ManualClock>) -> Arc<Self> {
        Self::then(clock, |_| {})
    }

    fn then(
        clock: &Arc<ManualClock>,
        on_sleep: impl Fn(usize) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            clock: Arc::clone(clock),
            slept: Mutex::new(Vec::new()),
            on_sleep: Box::new(on_sleep),
        })
    }
}

impl Sleeper for Advancing {
    fn sleep(&self, duration: Duration) {
        let number = {
            let mut slept = self.slept.lock().expect("sleeps");
            slept.push(duration);
            slept.len()
        };
        self.clock.0.fetch_add(
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
            Ordering::SeqCst,
        );
        (self.on_sleep)(number);
    }
}

fn budgets_on(clock: &Arc<ManualClock>, admission_wait: u64) -> Budgets {
    Budgets::start(
        Arc::clone(clock) as Arc<dyn BudgetClock>,
        BudgetLimits::resolve(600, admission_wait, 60, Some(600), 0),
    )
}

fn temp_dir(tag: &str) -> crate::test_support::TempDir {
    crate::test_support::temp_dir(&format!("heavy-{tag}"))
}

fn sh(script: &str) -> CommandSpec {
    CommandSpec {
        name: None,
        argv: vec!["sh".into(), "-c".into(), script.into()],
        timeout_seconds: 30,
        junit: None,
        weight: None,
        retry: None,
        env: BTreeMap::new(),
        reuse: None,
    }
}

fn gate_with(heavy: u32) -> LocalGate {
    LocalGate::new(ConcurrencyLimits {
        max_heavy_commands: Some(heavy),
        ..ConcurrencyLimits::default()
    })
}

fn register(gate: &LocalGate, run: &str) {
    gate.register_run(&RunRegistration {
        run_id: run.into(),
        session_id: "tab".into(),
        budget_micros: None,
        max_agents: None,
        max_depth: None,
    })
    .expect("register");
}

/// A jitter source that always draws the whole backoff.
struct Full;

impl Jitter for Full {
    fn within(&self, max: Duration) -> Duration {
        max
    }
}

/// A jitter source that draws the given thousandths of each backoff, in
/// order, and remembers each backoff it was asked about.
struct Scripted {
    permille: Vec<u32>,
    asked: Mutex<Vec<Duration>>,
}

impl Scripted {
    fn new(permille: &[u32]) -> Arc<Self> {
        Arc::new(Self {
            permille: permille.to_vec(),
            asked: Mutex::new(Vec::new()),
        })
    }
}

impl Jitter for Scripted {
    fn within(&self, max: Duration) -> Duration {
        let mut asked = self.asked.lock().expect("asked");
        let share = self.permille[asked.len() % self.permille.len()];
        asked.push(max);
        max * share / 1000
    }
}

fn admission<'a>(
    gate: &'a (dyn Gate + Sync),
    run: &str,
    cap: u32,
    cores: u32,
) -> HeavyAdmission<'a> {
    HeavyAdmission {
        gate: Some(gate),
        run_id: run.into(),
        session_id: "tab".into(),
        budgets: Budgets::unbounded(),
        events: Events::silent(),
        cap,
        cores,
        slots: None,
        sizing: amont_slots::Sizing::Fixed(4),
        ancestor_holds_slots: false,
        poll: Duration::from_millis(10),
        announce_every: ANNOUNCE_EVERY,
        heartbeat_every: Duration::from_millis(10),
        sleeper: Arc::new(ThreadSleeper),
        jitter: Arc::new(Full),
        registration: None,
        tries: Tries::default(),
        exhausted: |_, _| false,
        disk: crate::disk::DiskCheck::off(),
    }
}

fn registration(run: &str) -> RunRegistration {
    RunRegistration {
        run_id: run.into(),
        session_id: "tab".into(),
        budget_micros: None,
        max_agents: None,
        max_depth: None,
    }
}

fn heavy_request(id: &str, run: &str, weight: u32) -> DispatchRequest {
    DispatchRequest {
        disk: None,
        dispatch_id: id.into(),
        run_id: run.into(),
        session_id: "tab".into(),
        parent_dispatch: None,
        depth: 0,
        resource: ResourceClass::HeavyLocal,
        reserve_micros: 0,
        source: DispatchSource::NativeRun,
        caller_agent_id: None,
        weight,
    }
}

fn ask<'a>(env: &'a BTreeMap<String, String>, weight: u32) -> Ask<'a> {
    Ask {
        phase: "base-cmd0",
        label: "cmd0",
        weight,
        env,
        cancel: None,
    }
}

/// A gate that remembers, in order, every grant and every release.
struct Recording {
    inner: LocalGate,
    events: Mutex<Vec<String>>,
    /// Answer `bind_group` with `UnknownDispatch`, as a coordinator that
    /// restarted would.
    refuse_bind: bool,
}

impl Recording {
    fn new(inner: LocalGate) -> Self {
        Self {
            inner,
            events: Mutex::new(Vec::new()),
            refuse_bind: false,
        }
    }

    fn refusing_bind(inner: LocalGate) -> Self {
        Self {
            refuse_bind: true,
            ..Self::new(inner)
        }
    }

    fn note(&self, what: String) {
        self.events.lock().expect("log").push(what);
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("log").clone()
    }
}

impl Gate for Recording {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError> {
        self.inner.register_run(registration)
    }
    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError> {
        let decision = self.inner.admit(request)?;
        if decision == Decision::Granted {
            self.note(format!("granted {}", request.run_id));
        }
        Ok(decision)
    }
    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<BindOutcome, GateError> {
        self.inner.bind(dispatch_id, agent_id, pid)
    }
    fn bind_group(&self, id: &str, runner: u32, pgid: u32) -> Result<BindOutcome, GateError> {
        if self.refuse_bind {
            return Ok(BindOutcome::UnknownDispatch);
        }
        self.inner.bind_group(id, runner, pgid)
    }
    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError> {
        self.note(format!("heartbeat {dispatch_id}"));
        self.inner.heartbeat(dispatch_id)
    }
    fn mark_waiting(&self, dispatch_id: &str) -> Result<WaitOutcome, GateError> {
        self.inner.mark_waiting(dispatch_id)
    }
    fn resume(&self, dispatch_id: &str) -> Result<ResumeOutcome, GateError> {
        self.inner.resume(dispatch_id)
    }
    fn release(&self, dispatch_id: &str) -> Result<LifecycleOutcome, GateError> {
        // Noted before the release takes effect: the next grant can only
        // come after it, so the order of this log is the coordinator's.
        let run = dispatch_id.split("-heavy-").next().unwrap_or(dispatch_id);
        self.note(format!("released {run}"));
        self.inner.release(dispatch_id)
    }
    fn settle(
        &self,
        dispatch_id: &str,
        spent_micros: Option<i64>,
    ) -> Result<LifecycleOutcome, GateError> {
        self.inner.settle(dispatch_id, spent_micros)
    }
    fn withdraw(&self, dispatch_id: &str) -> Result<WithdrawOutcome, GateError> {
        self.inner.withdraw(dispatch_id)
    }
    fn acquire_write(
        &self,
        dispatch_id: &str,
        worktree: &str,
    ) -> Result<WriteLeaseOutcome, GateError> {
        self.inner.acquire_write(dispatch_id, worktree)
    }
    fn release_write(
        &self,
        dispatch_id: &str,
        worktree: &str,
    ) -> Result<ReleaseWriteOutcome, GateError> {
        self.inner.release_write(dispatch_id, worktree)
    }
    fn write_lease_holder(&self, worktree: &str) -> Result<Option<String>, GateError> {
        self.inner.write_lease_holder(worktree)
    }
    fn run_cancelled(&self, run_id: &str) -> Result<bool, GateError> {
        self.inner.run_cancelled(run_id)
    }
    fn enforcement(&self) -> Enforcement {
        self.inner.enforcement()
    }
}

#[test]
fn parallelism_is_the_weight_s_share_of_the_cores() {
    assert_eq!(parallelism(1, 16, 4), 4);
    assert_eq!(parallelism(2, 16, 4), 8);
    assert_eq!(parallelism(1, 2, 4), 1, "never below one");
    assert_eq!(heavy_cap(None, 16), 4);
    assert_eq!(
        heavy_cap(None, 2),
        1,
        "a quarter of the cores, at least one"
    );
    assert_eq!(heavy_cap(Some(3), 16), 3);
    let id = dispatch_id("run-1", "base-cmd0", "make@ab12:x", 0, 2);
    assert_eq!(id, "run-1-heavy-base-cmd0-make@ab12_x-2");
    let resumed = dispatch_id("run-1", "base-cmd0", "make@ab12:x", 1, 2);
    assert_eq!(resumed, "run-1-heavy-base-cmd0-make@ab12_x-e1-2");
}

#[test]
fn with_one_heavy_slot_two_runs_checks_never_overlap() {
    let dir = temp_dir("serial");
    let gate = Recording::new(gate_with(1));
    register(&gate.inner, "run-a");
    register(&gate.inner, "run-b");
    std::thread::scope(|scope| {
        for run in ["run-a", "run-b"] {
            let (gate, dir) = (&gate, &dir);
            scope.spawn(move || {
                let admission = admission(gate, run, 1, 4);
                run_observed_within(ObservedRun {
                    dir,
                    spec: &sh("sleep 0.02"),
                    logs_dir: &dir.join(format!("logs-{run}")),
                    label: "cmd0",
                    log_stem: "x-cmd0",
                    events: &Events::silent(),
                    cancel: None,
                    admission: &admission,
                })
                .expect("run");
            });
        }
    });
    // The coordinator's own order: a grant is always followed by its
    // release before the next grant. Overlap would show as two grants in
    // a row.
    let events = gate.events();
    assert_eq!(events.len(), 4, "{events:?}");
    for pair in events.chunks(2) {
        let run = pair[0].strip_prefix("granted ").expect("a grant first");
        assert_eq!(pair[1], format!("released {run}"), "{events:?}");
    }
}

#[test]
fn an_admitted_check_carries_parallelism_bounds_and_its_own_env_wins() {
    let dir = temp_dir("env");
    let gate = gate_with(2);
    register(&gate, "run-e");
    let admission = admission(&gate, "run-e", 2, 8);
    let print = "echo $MAKEFLAGS $CARGO_BUILD_JOBS $RUST_TEST_THREADS $NEXTEST_TEST_THREADS";

    // Weight 1 of cap 2 on 8 cores: four jobs.
    let outcome = run_observed_within(ObservedRun {
        dir: &dir,
        spec: &sh(print),
        logs_dir: &dir.join("logs"),
        label: "plain",
        log_stem: "x-cmd0",
        events: &Events::silent(),
        cancel: None,
        admission: &admission,
    })
    .expect("run");
    let log = std::fs::read_to_string(&outcome.log_path).expect("log");
    assert_eq!(log.trim(), "-j4 4 4 4");

    // Weight 2 of cap 2: all eight; the check's own env replaces one.
    let mut heavy = sh(print);
    heavy.weight = Some(2);
    heavy.env.insert("CARGO_BUILD_JOBS".into(), "1".into());
    let outcome = run_observed_within(ObservedRun {
        dir: &dir,
        spec: &heavy,
        logs_dir: &dir.join("logs"),
        label: "heavy",
        log_stem: "x-cmd1",
        events: &Events::silent(),
        cancel: None,
        admission: &admission,
    })
    .expect("run");
    let log = std::fs::read_to_string(&outcome.log_path).expect("log");
    assert_eq!(log.trim(), "-j8 1 8 8");
}

#[test]
fn a_weight_above_max_heavy_commands_is_refused_and_nothing_runs() {
    let dir = temp_dir("weight");
    let gate = gate_with(2);
    register(&gate, "run-w");
    let admission = admission(&gate, "run-w", 2, 8);
    let mut too_heavy = sh("echo ran > ran");
    too_heavy.weight = Some(3);
    let refused = run_observed_within(ObservedRun {
        dir: &dir,
        spec: &too_heavy,
        logs_dir: &dir.join("logs"),
        label: "heavy",
        log_stem: "x-cmd0",
        events: &Events::silent(),
        cancel: None,
        admission: &admission,
    });
    assert!(
        matches!(
            refused,
            Err(crate::verify::VerifyError::Lease(LeaseStop::Weight {
                weight: 3,
                cap: 2
            }))
        ),
        "{refused:?}"
    );
    assert!(!dir.join("ran").exists());
    // The coordinator refuses it too, whatever the runner thinks its cap is.
    let request = DispatchRequest {
        disk: None,
        dispatch_id: "w-heavy".into(),
        run_id: "run-w".into(),
        session_id: "tab".into(),
        parent_dispatch: None,
        depth: 0,
        resource: ResourceClass::HeavyLocal,
        reserve_micros: 0,
        source: DispatchSource::NativeRun,
        caller_agent_id: None,
        weight: 3,
    };
    assert!(matches!(
        gate.admit(&request),
        Ok(Decision::Refused {
            code: Refusal::WeightOutOfRange,
            ..
        })
    ));
}

#[test]
fn queued_time_is_charged_to_the_admission_wait_and_the_total_not_to_work() {
    let dir = temp_dir("queued");
    let gate = gate_with(1);
    register(&gate, "run-q");
    register(&gate, "run-other");
    // Another run holds the only token.
    assert_eq!(
        gate.admit(&heavy_request("other-heavy", "run-other", 1))
            .expect("admit"),
        Decision::Granted
    );

    let clock = ManualClock::new();
    let budgets = budgets_on(&clock, 1);
    budgets.begin_work();
    let mut queued = admission(&gate, "run-q", 1, 4);
    queued.budgets = budgets.clone();
    queued.sleeper = Advancing::new(&clock);
    queued.poll = Duration::from_millis(500);
    let stopped = run_observed_within(ObservedRun {
        dir: &dir,
        spec: &sh("echo ran > ran"),
        logs_dir: &dir.join("logs"),
        label: "cmd0",
        log_stem: "x-cmd0",
        events: &Events::silent(),
        cancel: None,
        admission: &queued,
    });
    assert!(
        matches!(
            stopped,
            Err(crate::verify::VerifyError::Lease(LeaseStop::Exhausted(
                Exhausted::AdmissionWait
            )))
        ),
        "{stopped:?}"
    );
    assert!(!dir.join("ran").exists(), "a queued command never ran");
    assert!(budgets.waited(WaitKind::Admission) >= Duration::from_secs(1));
    assert_eq!(
        budgets.worked(),
        Duration::ZERO,
        "the queue was taken out of work"
    );
    assert!(
        budgets.total_remaining() < Duration::from_secs(600),
        "the total kept running while queued"
    );
}

#[test]
fn a_queued_command_says_it_is_waiting_every_announce_period() {
    let dir = temp_dir("announce");
    let gate = gate_with(1);
    register(&gate, "run-n");
    register(&gate, "run-other");
    assert_eq!(
        gate.admit(&heavy_request("other-heavy", "run-other", 1))
            .expect("admit"),
        Decision::Granted
    );
    let clock = ManualClock::new();
    let mut queued = admission(&gate, "run-n", 1, 4);
    queued.events = Events::for_run_on("run-n", &dir, crate::protocol::Wire::to(std::io::sink()));
    queued.announce_every = Duration::from_millis(100);
    queued.poll = Duration::from_millis(50);
    queued.budgets = budgets_on(&clock, 1);
    queued.sleeper = Advancing::new(&clock);
    let _ = queued.admit(&ask(&BTreeMap::new(), 1));
    let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
    let announced = written
        .lines()
        .filter(|line| line.contains("\"waiting_for_slot\""))
        .count();
    assert!(announced >= 2, "{written}");
    assert!(written.contains("\"class\":\"heavy_local\""), "{written}");
}

#[test]
fn a_restarted_coordinator_is_told_about_the_run_once_and_the_lease_goes_through() {
    // The gate has never heard of the run: what a coordinator that
    // restarted answers.
    let gate = gate_with(1);
    let mut forgotten = admission(&gate, "run-r", 1, 4);
    let none = BTreeMap::new();
    let refused = forgotten.admit(&ask(&none, 1));
    assert!(
        matches!(
            refused,
            Err(LeaseStop::Refused {
                code: Refusal::UnknownRun,
                ..
            })
        ),
        "without a registration to repeat the refusal stands"
    );
    forgotten.registration = Some(registration("run-r"));
    let lease = forgotten
        .admit(&ask(&none, 1))
        .expect("registered once, then admitted");
    drop(lease);
}

#[test]
fn a_run_the_coordinator_still_refuses_after_registering_again_is_refused() {
    /// A coordinator that registers the run and still does not know it.
    struct Amnesiac;
    impl Gate for Amnesiac {
        fn register_run(&self, _: &RunRegistration) -> Result<(), GateError> {
            Ok(())
        }
        fn admit(&self, _: &DispatchRequest) -> Result<Decision, GateError> {
            Ok(Decision::Refused {
                code: Refusal::UnknownRun,
                detail: "unknown run".into(),
            })
        }
        fn bind(&self, _: &str, _: Option<&str>, _: Option<u32>) -> Result<BindOutcome, GateError> {
            Ok(BindOutcome::UnknownDispatch)
        }
        fn heartbeat(&self, _: &str) -> Result<HeartbeatStatus, GateError> {
            unreachable!()
        }
        fn mark_waiting(&self, _: &str) -> Result<WaitOutcome, GateError> {
            unreachable!()
        }
        fn resume(&self, _: &str) -> Result<ResumeOutcome, GateError> {
            unreachable!()
        }
        fn release(&self, _: &str) -> Result<LifecycleOutcome, GateError> {
            unreachable!()
        }
        fn settle(&self, _: &str, _: Option<i64>) -> Result<LifecycleOutcome, GateError> {
            unreachable!()
        }
        fn withdraw(&self, _: &str) -> Result<WithdrawOutcome, GateError> {
            unreachable!()
        }
        fn acquire_write(&self, _: &str, _: &str) -> Result<WriteLeaseOutcome, GateError> {
            unreachable!()
        }
        fn release_write(&self, _: &str, _: &str) -> Result<ReleaseWriteOutcome, GateError> {
            unreachable!()
        }
        fn write_lease_holder(&self, _: &str) -> Result<Option<String>, GateError> {
            unreachable!()
        }
        fn run_cancelled(&self, _: &str) -> Result<bool, GateError> {
            unreachable!()
        }
        fn enforcement(&self) -> Enforcement {
            unreachable!()
        }
    }
    let mut once = admission(&Amnesiac, "run-r", 1, 4);
    once.registration = Some(registration("run-r"));
    let refused = once.admit(&ask(&BTreeMap::new(), 1));
    assert!(
        matches!(
            refused,
            Err(LeaseStop::Refused {
                code: Refusal::UnknownRun,
                ..
            })
        ),
        "registered again once, not for ever"
    );
}

#[test]
fn a_coordinator_that_cannot_be_reached_is_a_gate_error_after_the_bounded_retry_not_a_refusal() {
    struct Unreachable;
    impl Gate for Unreachable {
        fn register_run(&self, _: &RunRegistration) -> Result<(), GateError> {
            Err(GateError::Unavailable {
                operation: "admit",
                socket: "none".into(),
                cause: "down".into(),
            })
        }
        fn admit(&self, _: &DispatchRequest) -> Result<Decision, GateError> {
            Err(GateError::Unavailable {
                operation: "admit",
                socket: "none".into(),
                cause: "down".into(),
            })
        }
        fn bind(&self, _: &str, _: Option<&str>, _: Option<u32>) -> Result<BindOutcome, GateError> {
            unreachable!()
        }
        fn heartbeat(&self, _: &str) -> Result<HeartbeatStatus, GateError> {
            unreachable!()
        }
        fn mark_waiting(&self, _: &str) -> Result<WaitOutcome, GateError> {
            unreachable!()
        }
        fn resume(&self, _: &str) -> Result<ResumeOutcome, GateError> {
            unreachable!()
        }
        fn release(&self, _: &str) -> Result<LifecycleOutcome, GateError> {
            unreachable!()
        }
        fn settle(&self, _: &str, _: Option<i64>) -> Result<LifecycleOutcome, GateError> {
            unreachable!()
        }
        fn withdraw(&self, _: &str) -> Result<WithdrawOutcome, GateError> {
            unreachable!()
        }
        fn acquire_write(&self, _: &str, _: &str) -> Result<WriteLeaseOutcome, GateError> {
            unreachable!()
        }
        fn release_write(&self, _: &str, _: &str) -> Result<ReleaseWriteOutcome, GateError> {
            unreachable!()
        }
        fn write_lease_holder(&self, _: &str) -> Result<Option<String>, GateError> {
            unreachable!()
        }
        fn run_cancelled(&self, _: &str) -> Result<bool, GateError> {
            unreachable!()
        }
        fn enforcement(&self) -> Enforcement {
            unreachable!()
        }
    }
    let clock = ManualClock::new();
    let mut down = admission(&Unreachable, "run-d", 1, 4);
    down.budgets = budgets_on(&clock, 1800);
    down.poll = Duration::from_secs(60);
    down.sleeper = Advancing::new(&clock);
    down.registration = Some(registration("run-d"));
    let Err(LeaseStop::Gate(error)) = down.admit(&ask(&BTreeMap::new(), 1)) else {
        panic!("expected a gate error, not a refusal");
    };
    assert!(error.unavailable());
    assert!(error.to_string().contains("retried for 120 s"), "{error}");
    assert_eq!(
        clock.now_millis(),
        1_000_000 + 120_000,
        "bounded, then stopped"
    );
}

#[test]
fn a_resumed_runs_heavy_ids_do_not_collide_with_ids_settled_before_the_resume() {
    let gate = gate_with(1);
    register(&gate, "run-x");
    let none = BTreeMap::new();
    // The first start: a lease is taken, given back and settled, so the
    // coordinator now remembers its id as finished (C8).
    let first = admission(&gate, "run-x", 1, 4);
    drop(first.admit(&ask(&none, 1)).expect("first start"));
    // The same try, asked again by the same start, is a rerun and gets a
    // new id; but a resume restarts the counter, which is what used to
    // reuse the settled id.
    let mut resumed = admission(&gate, "run-x", 1, 4);
    resumed.tries = Tries::for_epoch(0);
    assert!(
        matches!(
            resumed.admit(&ask(&none, 1)),
            Err(LeaseStop::Refused {
                code: Refusal::AlreadyFinished,
                ..
            })
        ),
        "a counter that starts over reuses a settled id"
    );
    resumed.tries = Tries::for_epoch(1);
    drop(
        resumed
            .admit(&ask(&none, 1))
            .expect("the resumed epoch has its own ids"),
    );
}

#[test]
fn with_no_gate_an_overweight_command_is_refused_and_the_bounds_still_apply() {
    let unused = gate_with(1);
    let mut ungated = admission(&unused, "run-n", 2, 8);
    ungated.gate = None;
    let none = BTreeMap::new();
    assert!(matches!(
        ungated.admit(&ask(&none, 3)),
        Err(LeaseStop::Weight { weight: 3, cap: 2 })
    ));
    let lease = ungated.admit(&ask(&none, 1)).expect("unadmitted");
    assert_eq!(
        lease.env().get("MAKEFLAGS").map(String::as_str),
        Some("-j4")
    );
    assert_eq!(lease.id(), "", "nothing was leased");

    // The profile is refused when it is loaded, whatever the gate.
    let heavy = {
        let mut spec = sh("true");
        spec.weight = Some(5);
        spec
    };
    let light = sh("true");
    assert!(overweight([&light], 2).is_none());
    let refusal = overweight([&light, &heavy], 2).expect("refused");
    assert!(refusal.contains("weight of 5"), "{refusal}");
}

#[cfg(unix)]
#[test]
fn a_lease_the_coordinator_could_not_tie_to_the_group_is_kept_alive_while_the_check_runs() {
    let dir = temp_dir("untied");
    let gate = Recording::refusing_bind(gate_with(1));
    register(&gate.inner, "run-u");
    let admission = admission(&gate, "run-u", 1, 4);
    let mut with_events = admission;
    with_events.events =
        Events::for_run_on("run-u", &dir, crate::protocol::Wire::to(std::io::sink()));
    run_observed_within(ObservedRun {
        dir: &dir,
        spec: &sh("sleep 0.04"),
        logs_dir: &dir.join("logs"),
        label: "cmd0",
        log_stem: "x-cmd0",
        events: &with_events.events.clone(),
        cancel: None,
        admission: &with_events,
    })
    .expect("run");
    let events = gate.events();
    assert!(
        events.iter().any(|e| e.starts_with("heartbeat ")),
        "the untied lease was never heartbeated: {events:?}"
    );
    let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
    assert!(
        written.contains("heavy_lease_not_tied_to_group"),
        "{written}"
    );
    assert!(written.contains("kept alive by heartbeats"), "{written}");
}

#[test]
fn an_ancestor_that_holds_host_slots_is_not_queued_behind_and_the_fact_is_passed_on() {
    assert!(ancestor_holds_slots(Some("held")));
    assert!(!ancestor_holds_slots(Some("other")));
    assert!(!ancestor_holds_slots(None));

    let dir = temp_dir("ancestor");
    let gate = gate_with(1);
    register(&gate, "run-a");
    let mut inside = admission(&gate, "run-a", 1, 4);
    // Slots that could never be taken: a directory that does not exist.
    inside.slots = Some(dir.join("no-such-slots"));
    inside.ancestor_holds_slots = true;
    let lease = inside.admit(&ask(&BTreeMap::new(), 1)).expect("admitted");
    assert_eq!(
        lease.env().get(HOST_SLOT_ENV).map(String::as_str),
        Some("held")
    );
    assert_eq!(lease.host_slots(), 0, "no host slot was queued for");
    assert!(
        !dir.join("no-such-slots").exists(),
        "nothing was created either"
    );
    drop(lease);

    let outcome = run_observed_within(ObservedRun {
        dir: &dir,
        spec: &sh("echo slot=$AMONT_HOST_SLOT"),
        logs_dir: &dir.join("logs"),
        label: "cmd0",
        log_stem: "x-cmd0",
        events: &Events::silent(),
        cancel: None,
        admission: &inside,
    })
    .expect("run");
    let log = std::fs::read_to_string(outcome.log_path).expect("log");
    assert_eq!(log.trim(), "slot=held", "the check sees the variable");
}

#[cfg(unix)]
mod host_slots {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    /// A private slot directory, as `amont-slots` requires (0700, ours).
    fn slot_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("relais-heavy-slots-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // best-effort: a fresh scratch dir
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .expect("slot dir");
        dir
    }

    /// Another holder of one slot in `dir`, through the same crate.
    fn other_holder(dir: &Path, command: &str) -> amont_slots::Lease {
        let mut waiter = amont_slots::Waiter::new_in(
            dir,
            amont_slots::Sizing::Fixed(1),
            1,
            amont_slots::Holder::here("other", command),
        )
        .expect("waiter");
        match waiter.poll().expect("poll") {
            amont_slots::Poll::Granted(lease) => lease,
            amont_slots::Poll::Waiting(q) => panic!("the slot was free: {q:?}"),
        }
    }

    /// A missing directory is made, private, and the command holds a slot
    /// in it: relais joins the host queue with nothing configured.
    #[test]
    fn a_missing_directory_is_made_private_and_a_slot_is_held() {
        let scratch = temp_dir("slots-new");
        let missing = scratch.join("made-here");
        let gate = gate_with(1);
        register(&gate, "run-n");
        let mut shared = admission(&gate, "run-n", 1, 4);
        shared.slots = Some(missing.clone());
        shared.sizing = amont_slots::Sizing::Fixed(1);
        let lease = shared.admit(&ask(&BTreeMap::new(), 1)).expect("admitted");
        assert_eq!(lease.host_slots(), 1);
        assert_eq!(
            lease.env().get(HOST_SLOT_ENV).map(String::as_str),
            Some("held")
        );
        let mode = std::fs::metadata(&missing)
            .expect("made")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        drop(lease);
        std::fs::remove_dir_all(&scratch).ok();
    }

    /// A directory another user could have made (0755) is refused by the
    /// crate: said once, and the coordinator's queue alone is used.
    #[test]
    fn an_unsafe_directory_warns_once_and_falls_back() {
        let scratch = temp_dir("slots-off");
        let open = slot_dir("open");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let gate = gate_with(1);
        register(&gate, "run-o");
        let mut off = admission(&gate, "run-o", 1, 4);
        off.events = Events::for_run_on(
            "run-o",
            &scratch,
            crate::protocol::Wire::to(std::io::sink()),
        );
        off.slots = Some(open.clone());
        for _ in 0..3 {
            let lease = off
                .admit(&ask(&BTreeMap::new(), 1))
                .expect("falls back to the coordinator queue");
            assert_eq!(lease.host_slots(), 0);
        }
        let written = std::fs::read_to_string(scratch.join("events.jsonl")).expect("events");
        assert_eq!(
            written.matches("host_slots_off").count(),
            1,
            "said once, however often it is asked: {written}"
        );
        std::fs::remove_dir_all(&open).ok();
    }

    #[test]
    fn a_weight_above_the_cap_is_refused_not_capped() {
        let dir = slot_dir("short");
        let gate = gate_with(2);
        register(&gate, "run-w");
        let mut shared = admission(&gate, "run-w", 2, 16);
        shared.slots = Some(dir.clone());
        shared.sizing = amont_slots::Sizing::Fixed(1);
        let refused = shared.admit(&ask(&BTreeMap::new(), 2)).err();
        assert!(
            matches!(
                refused,
                Some(LeaseStop::HostSlots {
                    weight: 2,
                    slots: 1,
                    ..
                })
            ),
            "{refused:?}"
        );
        // Nothing was queued for: the token is free for the next command.
        shared.slots = None;
        drop(
            shared
                .admit(&ask(&BTreeMap::new(), 2))
                .expect("no token was kept"),
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An older relais counted the `slot-*` files of the directory to size
    /// the queue. The holder records live under `holders/`, so that count
    /// never grows past the real slots while one is held.
    #[test]
    fn an_old_relais_counting_slot_files_never_counts_a_holder_record() {
        let dir = slot_dir("old-reader");
        let held = other_holder(&dir, "cargo test");
        assert!(dir.join("holders/slot-0.who").exists());
        // The old `slot_files`, as it read the directory.
        let counted = std::fs::read_dir(&dir)
            .expect("dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("slot-"))
            .filter(|e| e.file_type().is_ok_and(|kind| kind.is_file()))
            .count();
        assert_eq!(counted, 1, "one slot, one file");
        drop(held);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_check_waits_for_a_held_host_slot_and_runs_holding_it() {
        let dir = temp_dir("slots");
        let slots_dir = slot_dir("wait");
        let held = Arc::new(Mutex::new(Some(other_holder(&slots_dir, "xcodebuild"))));
        let gate = Recording::new(gate_with(1));
        register(&gate.inner, "run-s");
        let clock = ManualClock::new();
        let mut with_slots = admission(&gate, "run-s", 1, 4);
        with_slots.slots = Some(slots_dir.clone());
        with_slots.sizing = amont_slots::Sizing::Fixed(1);
        with_slots.budgets = budgets_on(&clock, 60);
        with_slots.events =
            Events::for_run_on("run-s", &dir, crate::protocol::Wire::to(std::io::sink()));
        with_slots.announce_every = Duration::from_millis(1);
        with_slots.poll = Duration::from_millis(5);
        // The other holder lets go during the third wait; the order of
        // what the gate saw proves the check did not run before that.
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let noted = Arc::clone(&order);
        let release = Arc::clone(&held);
        with_slots.sleeper = Advancing::then(&clock, move |number| {
            noted
                .lock()
                .expect("order")
                .push(format!("waited {number}"));
            if number == 3 {
                noted
                    .lock()
                    .expect("order")
                    .push("other holder let go".into());
                release.lock().expect("held").take();
            }
        });
        let outcome = run_observed_within(ObservedRun {
            dir: &dir,
            spec: &sh("echo slot=$AMONT_HOST_SLOT"),
            logs_dir: &dir.join("logs"),
            label: "cmd0",
            log_stem: "x-cmd0",
            events: &with_slots.events.clone(),
            cancel: None,
            admission: &with_slots,
        })
        .expect("run");
        order.lock().expect("order").push("check finished".into());
        let log = std::fs::read_to_string(outcome.log_path).expect("log");
        assert_eq!(log.trim(), "slot=held");
        let order = order.lock().expect("order");
        assert_eq!(
            order[..4],
            ["waited 1", "waited 2", "waited 3", "other holder let go"],
            "{order:?}"
        );
        assert_eq!(order.last().map(String::as_str), Some("check finished"));
        // While it waited for the slot it held its token and said so.
        assert!(gate.events().iter().any(|e| e.starts_with("heartbeat ")));
        // The wait names whom it waited behind.
        let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
        assert!(written.contains("\"class\":\"host_slot\""), "{written}");
        assert!(
            written.contains("\"behind\":\"other: xcodebuild"),
            "{written}"
        );
        std::fs::remove_dir_all(&slots_dir).ok();
    }
}

// ---- the disk floor and its reservation (SPEC §23) ----

const GIB: u64 = 1024 * 1024 * 1024;

/// A disk whose free space a test sets by hand: one volume.
#[derive(Debug)]
struct StubDisk {
    available: std::sync::atomic::AtomicU64,
}

impl StubDisk {
    fn with(bytes: u64) -> Arc<Self> {
        Arc::new(Self {
            available: std::sync::atomic::AtomicU64::new(bytes),
        })
    }

    fn set(&self, bytes: u64) {
        self.available.store(bytes, Ordering::SeqCst);
    }
}

impl crate::disk::DiskProbe for StubDisk {
    fn volume_of(&self, _path: &Path) -> Option<crate::disk::Volume> {
        Some(crate::disk::Volume {
            id: 7,
            mount: "/stub".into(),
            available: self.available.load(Ordering::SeqCst),
        })
    }
}

fn disk_gate(heavy: u32, floor: u64, disk: &Arc<StubDisk>) -> LocalGate {
    gate_with(heavy).with_disk(floor, Arc::clone(disk) as Arc<dyn crate::disk::DiskProbe>)
}

fn checking<'a>(
    mut admission: HeavyAdmission<'a>,
    floor: u64,
    disk: &Arc<StubDisk>,
) -> HeavyAdmission<'a> {
    admission.disk = crate::disk::DiskCheck {
        floor,
        probe: Arc::clone(disk) as Arc<dyn crate::disk::DiskProbe>,
        paths: vec![PathBuf::from("/stub/work")],
        growth: &crate::disk::NoGrowth,
    };
    admission
}

fn disk_request(id: &str, run: &str, estimate: u64) -> DispatchRequest {
    DispatchRequest {
        disk: Some(crate::disk::DiskAsk {
            estimate_bytes: estimate,
            paths: vec!["/stub/work".into()],
        }),
        ..heavy_request(id, run, 1)
    }
}

#[test]
fn a_disk_below_the_floor_is_waited_out_under_the_disk_budget_and_ends_disk_low() {
    let dir = temp_dir("disk-low");
    let disk = StubDisk::with(5 * GIB);
    let gate = disk_gate(2, 8 * GIB, &disk);
    register(&gate, "run-d");
    let clock = ManualClock::new();
    // 60 s of disk wait, a large admission wait.
    let budgets = budgets_on(&clock, 1800);
    budgets.begin_work();
    let mut held = checking(admission(&gate, "run-d", 2, 4), 8 * GIB, &disk);
    held.budgets = budgets.clone();
    held.sleeper = Advancing::new(&clock);
    held.poll = Duration::from_secs(10);
    held.events = Events::for_run_on("run-d", &dir, crate::protocol::Wire::to(std::io::sink()));
    let stopped = held.admit(&ask(&BTreeMap::new(), 1));
    assert!(
        matches!(stopped, Err(LeaseStop::Exhausted(Exhausted::DiskWait))),
        "{:?}",
        stopped.as_ref().err()
    );
    let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
    assert!(written.contains("\"waiting_for_disk\""), "{written}");
    assert!(written.contains("\"volume\":\"/stub\""), "{written}");
    assert!(written.contains("\"floor\":8589934592"), "{written}");
    assert!(budgets.waited(WaitKind::Disk) >= Duration::from_secs(60));
    assert_eq!(budgets.waited(WaitKind::Admission), Duration::ZERO);
    assert_eq!(budgets.worked(), Duration::ZERO, "the wait was not work");
    assert!(budgets.total_remaining() < Duration::from_secs(600));
}

#[test]
fn a_second_run_that_does_not_fit_above_the_floor_queues_until_the_first_lease_is_released() {
    let disk = StubDisk::with(11 * GIB);
    let gate = disk_gate(2, 8 * GIB, &disk);
    register(&gate, "run-1");
    register(&gate, "run-2");
    // 11 GiB available, floor 8, estimate 2: one fits (10), two do not.
    assert_eq!(
        gate.admit(&disk_request("first", "run-1", 2 * GIB))
            .expect("admit"),
        Decision::Granted
    );
    assert_eq!(
        gate.admit(&disk_request("second", "run-2", 2 * GIB))
            .expect("admit"),
        Decision::WaitingForDisk {
            volume: "/stub".into(),
            available: 11 * GIB,
            floor: 8 * GIB,
            reserved: 2 * GIB,
        },
        "a token is free; the disk is what holds the second lease"
    );
    gate.release("first").expect("release");
    gate.settle("first", Some(0)).expect("settle");
    assert_eq!(
        gate.admit(&disk_request("second", "run-2", 2 * GIB))
            .expect("admit"),
        Decision::Granted
    );
}

#[test]
fn the_floor_is_checked_again_after_the_grant_and_the_command_queues_instead_of_running() {
    let dir = temp_dir("disk-window");
    // The coordinator sees plenty; the runner, a moment later, sees little.
    let coordinator_disk = StubDisk::with(50 * GIB);
    let runner_disk = StubDisk::with(5 * GIB);
    let gate = disk_gate(1, 8 * GIB, &coordinator_disk);
    register(&gate, "run-w");
    let clock = ManualClock::new();
    let relief = Arc::clone(&runner_disk);
    let mut racing = checking(admission(&gate, "run-w", 1, 4), 8 * GIB, &runner_disk);
    racing.budgets = budgets_on(&clock, 1800);
    let sleeper = Advancing::then(&clock, move |_| relief.set(50 * GIB));
    racing.sleeper = sleeper.clone();
    racing.poll = Duration::from_secs(10);
    let lease = racing
        .admit(&ask(&BTreeMap::new(), 1))
        .expect("admitted once the space came back");
    assert!(
        lease.id().ends_with("-2"),
        "the first lease was given back and a second one taken: {}",
        lease.id()
    );
    assert_eq!(
        sleeper.slept.lock().expect("sleeps").len(),
        1,
        "the command waited once instead of running"
    );
    drop(lease);
    // A disk that stays low never lets the command run, and leaves no
    // lease behind.
    runner_disk.set(5 * GIB);
    let mut stuck = checking(admission(&gate, "run-w", 1, 4), 8 * GIB, &runner_disk);
    let clock = ManualClock::new();
    stuck.budgets = budgets_on(&clock, 1800);
    stuck.sleeper = Advancing::new(&clock);
    stuck.poll = Duration::from_secs(10);
    stuck.tries = Tries::for_epoch(1);
    let stopped = run_observed_within(ObservedRun {
        dir: &dir,
        spec: &sh("echo ran > ran"),
        logs_dir: &dir.join("logs"),
        label: "cmd0",
        log_stem: "x-cmd0",
        events: &Events::silent(),
        cancel: None,
        admission: &stuck,
    });
    assert!(
        matches!(
            stopped,
            Err(crate::verify::VerifyError::Lease(LeaseStop::Exhausted(
                Exhausted::DiskWait
            )))
        ),
        "{stopped:?}"
    );
    assert!(!dir.join("ran").exists(), "the command never ran");
    assert_eq!(
        gate.admit(&heavy_request("after", "run-w", 1))
            .expect("admit"),
        Decision::Granted,
        "the token came back"
    );
}

/// A check's measured growth, kept in memory.
#[derive(Default)]
struct Remembered(Mutex<BTreeMap<String, u64>>);

impl crate::disk::Growth for Remembered {
    fn last(&self, label: &str) -> Option<u64> {
        self.0.lock().expect("growth").get(label).copied()
    }
    fn record(&self, label: &str, bytes: u64) {
        self.0.lock().expect("growth").insert(label.into(), bytes);
    }
}

#[test]
fn a_checks_growth_is_recorded_and_is_its_next_estimate_with_two_gib_before() {
    let dir = temp_dir("disk-growth");
    let gate = gate_with(1);
    register(&gate, "run-g");
    let growth = Remembered::default();
    let mut measured = admission(&gate, "run-g", 1, 4);
    measured.disk = crate::disk::DiskCheck {
        floor: 0,
        probe: Arc::new(crate::disk::StatvfsProbe),
        paths: Vec::new(),
        growth: &growth,
    };
    assert_eq!(
        measured.disk.estimate("cmd0"),
        2 * GIB,
        "2 GiB before any measurement"
    );
    let outcome = run_observed_within(ObservedRun {
        dir: &dir,
        spec: &sh("head -c 1048576 /dev/zero > big"),
        logs_dir: &dir.join("logs"),
        label: "cmd0",
        log_stem: "x-cmd0",
        events: &Events::silent(),
        cancel: None,
        admission: &measured,
    })
    .expect("run");
    assert!(outcome.ended.succeeded());
    assert_eq!(
        crate::disk::Growth::last(&growth, "cmd0"),
        Some(1_048_576),
        "the tree grew by the file the check wrote"
    );
    assert_eq!(measured.disk.estimate("cmd0"), 1_048_576);
    assert_eq!(
        measured.disk.estimate("other"),
        2 * GIB,
        "another label is still unmeasured"
    );
}

/// A gate whose `admit` is unreachable `down` times, then answers `then`.
struct Flaky {
    down: usize,
    calls: std::sync::atomic::AtomicUsize,
    /// `register_run` is unreachable this many times, then succeeds.
    register_down: usize,
    /// After the unreachable answers, this many `UnknownRun` refusals.
    unknown: usize,
    registers: std::sync::atomic::AtomicUsize,
    withdrawn: std::sync::atomic::AtomicUsize,
    then: Decision,
}

impl Flaky {
    fn new(down: usize) -> Self {
        Self {
            down,
            calls: std::sync::atomic::AtomicUsize::new(0),
            register_down: 0,
            unknown: 0,
            registers: std::sync::atomic::AtomicUsize::new(0),
            withdrawn: std::sync::atomic::AtomicUsize::new(0),
            then: Decision::Granted,
        }
    }
}

impl Gate for Flaky {
    fn register_run(&self, _: &RunRegistration) -> Result<(), GateError> {
        if self.registers.fetch_add(1, Ordering::SeqCst) < self.register_down {
            return Err(GateError::Unavailable {
                operation: "register_run",
                socket: "none".into(),
                cause: "Resource temporarily unavailable".into(),
            });
        }
        Ok(())
    }
    fn admit(&self, _: &DispatchRequest) -> Result<Decision, GateError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.down {
            Err(GateError::Unavailable {
                operation: "admit",
                socket: "none".into(),
                cause: "Resource temporarily unavailable".into(),
            })
        } else if n < self.down + self.unknown {
            Ok(Decision::Refused {
                code: Refusal::UnknownRun,
                detail: "forgotten".into(),
            })
        } else {
            Ok(self.then.clone())
        }
    }
    fn bind(&self, _: &str, _: Option<&str>, _: Option<u32>) -> Result<BindOutcome, GateError> {
        unreachable!()
    }
    fn heartbeat(&self, _: &str) -> Result<HeartbeatStatus, GateError> {
        unreachable!()
    }
    fn mark_waiting(&self, _: &str) -> Result<WaitOutcome, GateError> {
        unreachable!()
    }
    fn resume(&self, _: &str) -> Result<ResumeOutcome, GateError> {
        unreachable!()
    }
    fn release(&self, _: &str) -> Result<LifecycleOutcome, GateError> {
        unreachable!()
    }
    fn settle(&self, _: &str, _: Option<i64>) -> Result<LifecycleOutcome, GateError> {
        unreachable!()
    }
    fn withdraw(&self, _: &str) -> Result<WithdrawOutcome, GateError> {
        self.withdrawn.fetch_add(1, Ordering::SeqCst);
        Ok(WithdrawOutcome::Withdrawn)
    }
    fn acquire_write(&self, _: &str, _: &str) -> Result<WriteLeaseOutcome, GateError> {
        unreachable!()
    }
    fn release_write(&self, _: &str, _: &str) -> Result<ReleaseWriteOutcome, GateError> {
        unreachable!()
    }
    fn write_lease_holder(&self, _: &str) -> Result<Option<String>, GateError> {
        unreachable!()
    }
    fn run_cancelled(&self, _: &str) -> Result<bool, GateError> {
        unreachable!()
    }
    fn enforcement(&self) -> Enforcement {
        unreachable!()
    }
}

/// The grant reached the coordinator but its answer did not: asked again
/// with the same id, it answers `AlreadyAdmitted`, and the lease is ours.
#[test]
fn a_grant_whose_answer_was_lost_is_the_lease_on_retry() {
    let clock = ManualClock::new();
    let gate = Flaky {
        then: Decision::AlreadyAdmitted,
        ..Flaky::new(1)
    };
    let mut heavy = admission(&gate, "run-l", 1, 4);
    heavy.budgets = budgets_on(&clock, 1800);
    heavy.poll = Duration::from_secs(60);
    heavy.sleeper = Advancing::new(&clock);
    let env = BTreeMap::new();
    let lease = heavy.admit(&ask(&env, 1)).map_err(|e| e.to_string());
    let lease = lease.expect("the lost grant is this request's lease");
    std::mem::forget(lease);
    assert_eq!(gate.calls.load(Ordering::SeqCst), 2);
}

#[test]
fn an_unreachable_coordinator_is_retried_with_backoff_and_the_lease_is_granted() {
    let clock = ManualClock::new();
    let sleeper = Advancing::new(&clock);
    let jitter = Scripted::new(&[750]);
    let gate = Flaky::new(6);
    let dir = temp_dir("unreachable-events");
    let mut heavy = admission(&gate, "run-r", 1, 4);
    heavy.budgets = budgets_on(&clock, 1800);
    heavy.poll = Duration::from_secs(60);
    heavy.events = Events::for_run_on("run-r", &dir, crate::protocol::Wire::to(std::io::sink()));
    heavy.sleeper = sleeper.clone();
    heavy.jitter = jitter.clone();
    let env = BTreeMap::new();
    let lease = heavy.admit(&ask(&env, 1)).map_err(|e| e.to_string());
    let lease = lease.expect("granted after the coordinator came back");
    // `release` and `settle` are not wired in the fake gate.
    std::mem::forget(lease);
    let slept = sleeper.slept.lock().expect("sleeps").clone();
    let backoffs = jitter.asked.lock().expect("asked").clone();
    let secs = |n: &[u64]| {
        n.iter()
            .map(|s| Duration::from_secs(*s))
            .collect::<Vec<_>>()
    };
    assert_eq!(backoffs, secs(&[1, 2, 4, 8, 16, 30]), "doubles, capped");
    assert_eq!(slept.len(), backoffs.len());
    for (wait, backoff) in slept.iter().zip(&backoffs) {
        assert!(wait <= backoff, "{wait:?} within [0, {backoff:?}]");
    }
    assert_eq!(gate.calls.load(Ordering::SeqCst), 7);
    let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
    let waits: Vec<&str> = written
        .lines()
        .filter(|line| line.contains("\"coordinator_unreachable\""))
        .collect();
    assert_eq!(waits.len(), 6, "{written}");
    assert!(waits[0].contains("\"waited_s\":0"), "{}", waits[0]);
    assert!(waits[1].contains("\"waited_s\":0"), "{}", waits[1]);
    assert!(waits[5].contains("\"waited_s\":"), "{}", waits[5]);
    assert!(waits[5].contains("\"position\":0"), "{}", waits[5]);
}

#[test]
fn a_fixed_jitter_source_gives_an_exact_jittered_sequence() {
    let clock = ManualClock::new();
    let sleeper = Advancing::new(&clock);
    let gate = Flaky::new(6);
    let mut heavy = admission(&gate, "run-j", 1, 4);
    heavy.budgets = budgets_on(&clock, 1800);
    heavy.poll = Duration::from_secs(60);
    heavy.sleeper = sleeper.clone();
    heavy.jitter = Scripted::new(&[500, 250, 750, 1000, 0, 100]);
    let env = BTreeMap::new();
    std::mem::forget(
        heavy
            .admit(&ask(&env, 1))
            .map_err(|e| e.to_string())
            .expect("granted"),
    );
    let slept: Vec<u128> = sleeper
        .slept
        .lock()
        .expect("sleeps")
        .iter()
        .map(Duration::as_millis)
        .collect();
    // 1 s*0.5, 2 s*0.25, 4 s*0.75, 8 s*1, 16 s*0 (no sleep), 30 s*0.1.
    assert_eq!(slept, vec![500, 500, 3000, 8000, 3000]);
}

#[test]
fn the_seeded_jitter_stays_within_zero_and_the_backoff_and_differs_by_seed() {
    let a = SeededJitter::new("run-a", 1, 42);
    let b = SeededJitter::new("run-b", 1, 42);
    let max = Duration::from_secs(30);
    let draws_a: Vec<Duration> = (0..200).map(|_| a.within(max)).collect();
    let draws_b: Vec<Duration> = (0..200).map(|_| b.within(max)).collect();
    assert!(draws_a.iter().all(|d| *d <= max));
    assert!(draws_a.iter().any(|d| !d.is_zero()));
    assert_ne!(draws_a, draws_b);
    assert_eq!(
        SeededJitter::new("r", 1, 1).within(Duration::ZERO),
        Duration::ZERO
    );
}

#[test]
fn an_unreachable_coordinator_is_given_up_on_after_the_bound() {
    let clock = ManualClock::new();
    let sleeper = Advancing::new(&clock);
    let gate = Flaky::new(usize::MAX);
    let mut heavy = admission(&gate, "run-g", 1, 4);
    heavy.budgets = budgets_on(&clock, 1800);
    heavy.poll = Duration::from_secs(60);
    heavy.sleeper = sleeper.clone();
    let env = BTreeMap::new();
    let Err(LeaseStop::Gate(error)) = heavy.admit(&ask(&env, 1)) else {
        panic!("expected a gate stop");
    };
    assert!(error.unavailable());
    assert!(error.to_string().contains("retried for 120 s"), "{error}");
    let total: Duration = sleeper.slept.lock().expect("sleeps").iter().sum();
    assert_eq!(total, UNREACHABLE_BOUND);
    assert_eq!(clock.now_millis(), 1_000_000 + 120_000);
    // Whole backoffs 1+2+4+8+16 (31 s), then 30 + 30, then 29 s clamped
    // to the bound: eight sleeps, nine asks.
    assert_eq!(gate.calls.load(Ordering::SeqCst), 9);
}

#[test]
fn a_cancel_set_while_the_coordinator_is_unreachable_stops_the_retries() {
    let clock = ManualClock::new();
    let flag = Arc::new(AtomicBool::new(false));
    let sleeper = {
        let flag = Arc::clone(&flag);
        Advancing::then(&clock, move |n| {
            if n == 3 {
                flag.store(true, Ordering::SeqCst);
            }
        })
    };
    let gate = Flaky::new(usize::MAX);
    let mut heavy = admission(&gate, "run-c", 1, 4);
    heavy.budgets = budgets_on(&clock, 1800);
    heavy.poll = Duration::from_secs(60);
    heavy.sleeper = sleeper.clone();
    let env = BTreeMap::new();
    let mut request = ask(&env, 1);
    request.cancel = Some(&flag);
    assert!(matches!(heavy.admit(&request), Err(LeaseStop::Cancelled)));
    assert_eq!(sleeper.slept.lock().expect("sleeps").len(), 3);
    assert_eq!(
        gate.withdrawn.load(Ordering::SeqCst),
        1,
        "withdrawn on cancel"
    );
}

#[test]
fn a_duplicate_with_no_prior_retry_still_stops_as_duplicate() {
    let gate = Flaky {
        then: Decision::AlreadyAdmitted,
        ..Flaky::new(0)
    };
    let heavy = admission(&gate, "run-dup", 1, 4);
    let env = BTreeMap::new();
    assert!(matches!(
        heavy.admit(&ask(&env, 1)),
        Err(LeaseStop::Duplicate(_))
    ));
}

#[test]
fn a_registration_that_is_unreachable_once_is_retried_and_the_lease_is_granted() {
    let clock = ManualClock::new();
    let sleeper = Advancing::new(&clock);
    let gate = Flaky {
        register_down: 1,
        unknown: 2,
        ..Flaky::new(0)
    };
    let mut heavy = admission(&gate, "run-reg", 1, 4);
    heavy.budgets = budgets_on(&clock, 1800);
    heavy.poll = Duration::from_secs(60);
    heavy.sleeper = sleeper.clone();
    heavy.registration = Some(registration("run-reg"));
    let env = BTreeMap::new();
    let lease = heavy.admit(&ask(&env, 1)).map_err(|e| e.to_string());
    std::mem::forget(lease.expect("granted"));
    assert_eq!(
        gate.registers.load(Ordering::SeqCst),
        2,
        "one failed, one made"
    );
    assert_eq!(sleeper.slept.lock().expect("sleeps").len(), 1);
}

#[test]
fn a_cancel_during_a_long_backoff_is_seen_within_one_poll() {
    let clock = ManualClock::new();
    let flag = Arc::new(AtomicBool::new(false));
    let sleeper = {
        let flag = Arc::clone(&flag);
        Advancing::then(&clock, move |n| {
            // Each backoff is sliced by the 100 ms poll: the 1, 2, 4, 8
            // and 16 s backoffs take slices 1-310, so slice 320 is ten
            // slices into the 30 s one. Cancel there.
            if n == 320 {
                flag.store(true, Ordering::SeqCst);
            }
        })
    };
    let gate = Flaky::new(usize::MAX);
    let mut heavy = admission(&gate, "run-p", 1, 4);
    heavy.budgets = budgets_on(&clock, 1800);
    heavy.poll = Duration::from_millis(100);
    heavy.sleeper = sleeper.clone();
    let env = BTreeMap::new();
    let mut request = ask(&env, 1);
    request.cancel = Some(&flag);
    assert!(matches!(heavy.admit(&request), Err(LeaseStop::Cancelled)));
    let slept = sleeper.slept.lock().expect("sleeps");
    assert_eq!(slept.len(), 320, "stopped on the slice that set the flag");
    assert!(slept.iter().all(|d| *d <= Duration::from_millis(100)));
}
