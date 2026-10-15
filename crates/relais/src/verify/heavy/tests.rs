use super::*;
use crate::admission::{
    Enforcement, HeartbeatStatus, LifecycleOutcome, LocalGate, ReleaseWriteOutcome, ResumeOutcome,
    RunRegistration, WaitOutcome, WithdrawOutcome, WriteLeaseOutcome,
};
use crate::policy::{BudgetClock, BudgetLimits, CommandSpec, ConcurrencyLimits};
use crate::verify::run_observed_within;
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
        ancestor_holds_slots: false,
        poll: Duration::from_millis(10),
        announce_every: ANNOUNCE_EVERY,
        heartbeat_every: Duration::from_millis(10),
        sleeper: Arc::new(ThreadSleeper),
        registration: None,
        tries: Tries::default(),
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
                run_observed_within(
                    dir,
                    &sh("sleep 0.02"),
                    &dir.join(format!("logs-{run}")),
                    "cmd0",
                    "x-cmd0",
                    &Events::silent(),
                    None,
                    &admission,
                )
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
    let outcome = run_observed_within(
        &dir,
        &sh(print),
        &dir.join("logs"),
        "plain",
        "x-cmd0",
        &Events::silent(),
        None,
        &admission,
    )
    .expect("run");
    let log = std::fs::read_to_string(&outcome.log_path).expect("log");
    assert_eq!(log.trim(), "-j4 4 4 4");

    // Weight 2 of cap 2: all eight; the check's own env replaces one.
    let mut heavy = sh(print);
    heavy.weight = Some(2);
    heavy.env.insert("CARGO_BUILD_JOBS".into(), "1".into());
    let outcome = run_observed_within(
        &dir,
        &heavy,
        &dir.join("logs"),
        "heavy",
        "x-cmd1",
        &Events::silent(),
        None,
        &admission,
    )
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
    let refused = run_observed_within(
        &dir,
        &too_heavy,
        &dir.join("logs"),
        "heavy",
        "x-cmd0",
        &Events::silent(),
        None,
        &admission,
    );
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
    let stopped = run_observed_within(
        &dir,
        &sh("echo ran > ran"),
        &dir.join("logs"),
        "cmd0",
        "x-cmd0",
        &Events::silent(),
        None,
        &queued,
    );
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
fn a_coordinator_that_cannot_be_reached_is_a_gate_error_not_a_refusal() {
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
    let mut down = admission(&Unreachable, "run-d", 1, 4);
    down.registration = Some(registration("run-d"));
    assert!(matches!(
        down.admit(&ask(&BTreeMap::new(), 1)),
        Err(LeaseStop::Gate(_))
    ));
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
    run_observed_within(
        &dir,
        &sh("sleep 0.04"),
        &dir.join("logs"),
        "cmd0",
        "x-cmd0",
        &with_events.events.clone(),
        None,
        &with_events,
    )
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
    #[cfg(unix)]
    assert!(lease.slot_fds().is_empty(), "no host slot was queued for");
    assert!(
        !dir.join("no-such-slots").exists(),
        "nothing was created either"
    );
    drop(lease);

    let outcome = run_observed_within(
        &dir,
        &sh("echo slot=$AMONT_HOST_SLOT"),
        &dir.join("logs"),
        "cmd0",
        "x-cmd0",
        &Events::silent(),
        None,
        &inside,
    )
    .expect("run");
    let log = std::fs::read_to_string(outcome.log_path).expect("log");
    assert_eq!(log.trim(), "slot=held", "the check sees the variable");
}

#[cfg(unix)]
mod host_slots {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    /// A slot directory as amont leaves it: private, with `files` slot files.
    fn slot_dir(tag: &str, files: u32) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("relais-heavy-slots-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .expect("slot dir");
        for index in 0..files {
            std::fs::write(dir.join(format!("slot-{index}")), b"").expect("slot file");
        }
        dir
    }

    fn lock(path: &Path) -> std::fs::File {
        let file = std::fs::File::open(path).expect("slot file");
        let locked = crate::procs::try_flock(&file).expect("flock");
        assert!(locked, "the slot was free");
        file
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        slots::slot_files(dir).expect("slot files")
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_partial_set_is_released_and_nothing_stays_held() {
        let dir = slot_dir("partial", 2);
        let all = files(&dir);
        let other = lock(&dir.join("slot-1"));
        assert!(
            matches!(
                slots::try_take(&all, 2).expect("tried"),
                slots::Taken::Short { missing: 1 }
            ),
            "slot-1 is taken elsewhere, so a weight of two gets nothing"
        );
        // slot-0 was released on the back-off: a taker of one gets it.
        // (A process forked by another test in this binary can hold a copy
        // of the descriptor for the instant before its exec, so a few
        // short retries are allowed.)
        let one = (0..200)
            .find_map(|_| match slots::try_take(&all, 1).expect("tried") {
                slots::Taken::All(held) => Some(held),
                slots::Taken::Short { .. } => {
                    std::thread::sleep(Duration::from_millis(5));
                    None
                }
            })
            .expect("slot-0 is free");
        assert_eq!(one.len(), 1);
        drop(other);
        drop(one);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_directory_or_one_with_no_slot_files_warns_once_and_creates_nothing() {
        let scratch = temp_dir("slots-off");
        let missing = scratch.join("never-made");
        let empty = slot_dir("empty", 0);
        std::fs::write(empty.join("not-a-slot"), b"").expect("other file");
        let before = entries(&empty);
        let gate = gate_with(1);
        register(&gate, "run-o");
        let mut off = admission(&gate, "run-o", 1, 4);
        off.events = Events::for_run_on(
            "run-o",
            &scratch,
            crate::protocol::Wire::to(std::io::sink()),
        );
        for dir in [&missing, &empty, &missing, &empty] {
            off.slots = Some(dir.clone());
            drop(
                off.admit(&ask(&BTreeMap::new(), 1))
                    .expect("falls back to the coordinator queue"),
            );
        }
        assert!(!missing.exists(), "the directory is amont's to make");
        assert_eq!(entries(&empty), before, "no slot file was created");
        let written = std::fs::read_to_string(scratch.join("events.jsonl")).expect("events");
        assert_eq!(
            written.matches("host_slots_off").count(),
            1,
            "said once, however often it is asked: {written}"
        );
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    fn a_weight_above_the_number_of_slot_files_is_refused_not_capped() {
        // Four cores' worth of tokens, but amont made one slot file.
        let dir = slot_dir("short", 1);
        let gate = gate_with(2);
        register(&gate, "run-w");
        let mut shared = admission(&gate, "run-w", 2, 16);
        shared.slots = Some(dir.clone());
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

    #[test]
    fn a_check_waits_for_a_held_host_slot_and_runs_holding_it() {
        let dir = temp_dir("slots");
        let slots_dir = slot_dir("wait", 1);
        let held = Mutex::new(Some(lock(&slots_dir.join("slot-0"))));
        let gate = Recording::new(gate_with(1));
        register(&gate.inner, "run-s");
        let clock = ManualClock::new();
        let mut with_slots = admission(&gate, "run-s", 1, 4);
        with_slots.slots = Some(slots_dir.clone());
        with_slots.budgets = budgets_on(&clock, 60);
        with_slots.events =
            Events::for_run_on("run-s", &dir, crate::protocol::Wire::to(std::io::sink()));
        with_slots.announce_every = Duration::from_millis(1);
        with_slots.poll = Duration::from_millis(5);
        // The other holder lets go during the third wait; the order of
        // what the gate saw proves the check did not run before that.
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let noted = Arc::clone(&order);
        let held = Arc::new(held);
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
        let outcome = run_observed_within(
            &dir,
            &sh("echo slot=$AMONT_HOST_SLOT"),
            &dir.join("logs"),
            "cmd0",
            "x-cmd0",
            &with_slots.events.clone(),
            None,
            &with_slots,
        )
        .expect("run");
        order.lock().expect("order").push("check finished".into());
        let log = std::fs::read_to_string(outcome.log_path).expect("log");
        assert_eq!(log.trim(), "slot=held");
        // A process forked elsewhere in this test binary can hold the
        // descriptor for an instant, so the waits after the release are
        // not counted: the order of events is what is asserted.
        let order = order.lock().expect("order");
        assert_eq!(
            order[..4],
            ["waited 1", "waited 2", "waited 3", "other holder let go"],
            "{order:?}"
        );
        assert_eq!(order.last().map(String::as_str), Some("check finished"));
        // While it waited for the slot it held its token and said so.
        assert!(gate.events().iter().any(|e| e.starts_with("heartbeat ")));
        // The queue position is the real shortfall: one slot to be freed.
        let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
        assert!(written.contains("\"class\":\"host_slot\""), "{written}");
        assert!(written.contains("\"position\":1"), "{written}");
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
    let stopped = run_observed_within(
        &dir,
        &sh("echo ran > ran"),
        &dir.join("logs"),
        "cmd0",
        "x-cmd0",
        &Events::silent(),
        None,
        &stuck,
    );
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
    let outcome = run_observed_within(
        &dir,
        &sh("head -c 1048576 /dev/zero > big"),
        &dir.join("logs"),
        "cmd0",
        "x-cmd0",
        &Events::silent(),
        None,
        &measured,
    )
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
