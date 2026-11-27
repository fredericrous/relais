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
    LocalGate::new(
        ConcurrencyLimits {
            max_heavy_commands: Some(heavy),
            ..ConcurrencyLimits::default()
        },
        fixed_clock,
    )
}

/// These tests confirm no orphan, so the gate's clock is never read; a
/// fixed one keeps `verify` from importing `ledger` (module-cycles).
fn fixed_clock() -> String {
    "2026-11-07T00:00:00+00:00".to_string()
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
        machine: greenharness_slots::Machine { cores },
        slots: None,
        sizing: Some(greenharness_slots::Sizing::Fixed(4)),
        ancestor_holds_slots: false,
        poll: Duration::from_millis(10),
        stall_window: greenharness_slots::STALL,
        announce_every: ANNOUNCE_EVERY,
        heartbeat_every: Duration::from_millis(10),
        sleeper: Arc::new(ThreadSleeper),
        jitter: Arc::new(Full),
        registration: None,
        tries: Tries::default(),
        exhausted: |_, _| false,
        disk: crate::disk::DiskCheck::off(),
        setup_slot: SetupSlot::default(),
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
    /// Answer this many `admit` calls as unreachable first.
    down: std::sync::atomic::AtomicUsize,
}

impl Recording {
    fn new(inner: LocalGate) -> Self {
        Self {
            inner,
            events: Mutex::new(Vec::new()),
            refuse_bind: false,
            down: std::sync::atomic::AtomicUsize::new(0),
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
        let down = self
            .down
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        if down.is_ok() {
            return Err(GateError::Unavailable {
                operation: "admit",
                socket: "none".into(),
                cause: "Resource temporarily unavailable".into(),
            });
        }
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

/// The host sizing greengate would resolve, from no keys and a detected
/// core count: hermetic, never this machine's git config.
fn detected_host(cores: u32) -> greenharness_slots::host::HostSizing {
    greenharness_slots::host::resolve(
        |_| None,
        &[],
        greenharness_slots::host::Resolved {
            value: cores,
            source: greenharness_slots::host::Source::Detected,
        },
    )
}

#[test]
fn the_host_plan_counts_the_detected_cores_and_sizes_the_slots_automatically() {
    let plan = host_plan(&detected_host(6), None);
    assert_eq!(plan.machine.cores, 6);
    assert_eq!(plan.slots.sizing(), Some(greenharness_slots::Sizing::Auto));
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
}

#[test]
fn relais_own_host_slots_still_wins_over_the_greengate_keys() {
    let rows = [(
        "global".to_string(),
        "greengate.hostslots".to_string(),
        "5".to_string(),
    )];
    let host = greenharness_slots::host::resolve(
        |_| None,
        &rows,
        greenharness_slots::host::Resolved {
            value: 6,
            source: greenharness_slots::host::Source::Detected,
        },
    );
    assert_eq!(
        host_plan(&host, None).slots.sizing(),
        Some(greenharness_slots::Sizing::Fixed(5))
    );
    assert_eq!(
        host_plan(&host, Some(2)).slots.sizing(),
        Some(greenharness_slots::Sizing::Fixed(2)),
        "the deprecated setting wins"
    );
}

#[test]
fn greengate_host_slots_zero_turns_the_host_slots_off_for_relais_too() {
    let rows = [(
        "global".to_string(),
        "greengate.hostslots".to_string(),
        "0".to_string(),
    )];
    let host = greenharness_slots::host::resolve(
        |_| None,
        &rows,
        greenharness_slots::host::Resolved {
            value: 6,
            source: greenharness_slots::host::Source::Detected,
        },
    );
    assert_eq!(host_plan(&host, None).slots.sizing(), None);
}

#[test]
fn greengate_host_slots_from_the_environment_wins_over_host_slots() {
    let host = greenharness_slots::host::resolve(
        |name| (name == greenharness_slots::host::SLOTS_ENV).then(|| "5".to_string()),
        &[],
        greenharness_slots::host::Resolved {
            value: 6,
            source: greenharness_slots::host::Source::Detected,
        },
    );
    assert_eq!(
        host_plan(&host, Some(2)).slots.sizing(),
        Some(greenharness_slots::Sizing::Fixed(5)),
        "an environment variable outranks a file"
    );
}

#[test]
fn the_plan_names_the_setting_that_turned_the_host_slots_off() {
    let resolved = |env_slots: Option<&str>, rows: &[(String, String, String)]| {
        let env_slots = env_slots.map(str::to_string);
        greenharness_slots::host::resolve(
            move |name| {
                (name == greenharness_slots::host::SLOTS_ENV)
                    .then(|| env_slots.clone())
                    .flatten()
            },
            rows,
            greenharness_slots::host::Resolved {
                value: 6,
                source: greenharness_slots::host::Source::Detected,
            },
        )
    };
    let git = [(
        "global".to_string(),
        "greengate.hostslots".to_string(),
        "0".to_string(),
    )];
    let by_git = host_plan(&resolved(None, &git), None);
    assert_eq!(by_git.slots.sizing(), None);
    assert_eq!(by_git.slots, HostSlots::Off(HostSlotsOff::GitGlobal));
    let by_env = host_plan(&resolved(Some("0"), &[]), Some(3));
    assert_eq!(by_env.slots.sizing(), None);
    assert_eq!(by_env.slots, HostSlots::Off(HostSlotsOff::Env));
    let by_file = host_plan(&resolved(None, &[]), Some(0));
    assert_eq!(by_file.slots.sizing(), None);
    assert_eq!(by_file.slots, HostSlots::Off(HostSlotsOff::MachineToml));
    assert_eq!(
        host_plan(&resolved(None, &[]), None).slots,
        HostSlots::On(greenharness_slots::Sizing::Auto)
    );
}

#[test]
fn an_off_setting_names_itself_and_its_fix() {
    assert_eq!(HostSlotsOff::Env.setting(), "GREENGATE_HOST_SLOTS=0");
    assert_eq!(HostSlotsOff::Env.fix(), "unset GREENGATE_HOST_SLOTS");
    assert_eq!(
        HostSlotsOff::GitGlobal.setting(),
        "greengate.hostSlots 0 (git config --global)"
    );
    assert_eq!(
        HostSlotsOff::GitGlobal.fix(),
        "git config --global --unset greengate.hostSlots"
    );
    assert_eq!(
        HostSlotsOff::GitSystem.fix(),
        "git config --system --unset greengate.hostSlots"
    );
    assert_eq!(
        HostSlotsOff::MachineToml.setting(),
        "[resources] host_slots = 0"
    );
    assert_eq!(
        HostSlotsOff::MachineToml.fix(),
        "remove host_slots from machine.toml"
    );
}

/// With the host slots off, an ancestor's held slot gives no slots' share
/// from a placeholder sizing: the coordinator's share stands.
#[test]
fn an_ancestor_slot_with_the_host_slots_off_keeps_the_coordinators_width() {
    let gate = gate_with(2);
    register(&gate, "run-a0");
    let mut nested = admission(&gate, "run-a0", 2, 8);
    nested.sizing = None;
    nested.ancestor_holds_slots = true;
    let lease = nested.admit(&ask(&BTreeMap::new(), 1)).expect("admitted");
    assert_eq!(
        lease.env().get("CARGO_BUILD_JOBS").map(String::as_str),
        Some("4"),
        "8 cores / the coordinator's 2"
    );
}

#[test]
fn the_width_rule_uses_the_slots_cap_only_under_a_host_slot() {
    let six = greenharness_slots::Machine { cores: 6 };
    let auto = Some(greenharness_slots::Sizing::Auto);
    assert_eq!(
        host_share(six, auto, Placement::UnderHostSlot, 1),
        (3, Placement::UnderHostSlot)
    );
    assert_eq!(
        host_share(six, auto, Placement::CoordinatorOnly, 1),
        (1, Placement::CoordinatorOnly)
    );
    assert_eq!(
        host_share(six, None, Placement::UnderHostSlot, 2),
        (2, Placement::CoordinatorOnly)
    );
}

#[test]
fn the_host_plan_carries_the_core_override_and_the_warnings() {
    let host = greenharness_slots::host::resolve(
        |name| (name == greenharness_slots::host::CORES_ENV).then(|| "4".to_string()),
        &[],
        greenharness_slots::host::Resolved {
            value: 6,
            source: greenharness_slots::host::Source::Detected,
        },
    );
    assert_eq!(host_plan(&host, None).machine.cores, 4);
    let bad = greenharness_slots::host::resolve(
        |name| (name == greenharness_slots::host::SLOTS_ENV).then(|| "abc".to_string()),
        &[],
        greenharness_slots::host::Resolved {
            value: 6,
            source: greenharness_slots::host::Source::Detected,
        },
    );
    let plan = host_plan(&bad, None);
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(plan.slots.sizing(), Some(greenharness_slots::Sizing::Auto));
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
fn the_lease_bounds_exactly_greengates_width_keys() {
    let environment = child_environment(3, &BTreeMap::new());
    let keys: Vec<&str> = environment.keys().map(String::as_str).collect();
    let mut shared = greenharness_slots::WIDTH_KEYS.to_vec();
    shared.sort_unstable();
    assert_eq!(keys, shared);
    assert_eq!(environment["MAKEFLAGS"], "-j3");
    assert_eq!(environment["CARGO_BUILD_JOBS"], "3");
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
        spec: &sh("echo slot=$GREENHARNESS_HOST_SLOT"),
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

/// A held marker is read by the slots crate's one reader (amont t15): a
/// refused one, where the run would queue, is said once and journalled once,
/// and the run queues; with no queue, nothing is said or written.
#[test]
fn a_refused_marker_is_journalled_once_where_the_run_queues() {
    use greenharness_slots::{held_marker_in, Marker, MarkerLog};
    let dir = temp_dir("marker");
    let refused = held_marker_in(&dir, Some(std::ffi::OsStr::new("1")));
    assert!(matches!(refused, Marker::Refused { .. }), "{refused:?}");
    let log = MarkerLog::new();
    let (held, said) = ancestor_holds_slots(&refused, Some(&*dir), &log);
    assert!(!held);
    let said = said.expect("the first refusal is said");
    assert!(said.starts_with("relais: "), "{said}");
    assert_eq!(
        ancestor_holds_slots(&refused, Some(&*dir), &log),
        (false, None),
        "said once"
    );
    let journal = std::fs::read_to_string(dir.join("waits.jsonl")).expect("journal");
    assert_eq!(journal.lines().count(), 1, "{journal}");
    assert!(journal.contains("\"tool\":\"relais\""), "{journal}");
    assert!(
        journal.contains("\"outcome\":\"marker_refused\""),
        "{journal}"
    );

    let quiet = temp_dir("marker-quiet");
    assert_eq!(
        ancestor_holds_slots(&refused, None, &MarkerLog::new()),
        (false, None)
    );
    assert!(!quiet.join("waits.jsonl").exists());
    let bare = held_marker_in(&quiet, Some(std::ffi::OsStr::new("held")));
    assert_eq!(
        ancestor_holds_slots(&bare, Some(&*quiet), &MarkerLog::new()),
        (true, None)
    );
    assert!(
        !quiet.join("waits.jsonl").exists(),
        "an accepted marker writes nothing"
    );
    for empty in [None, Some(std::ffi::OsStr::new(""))] {
        let absent = held_marker_in(&quiet, empty);
        assert_eq!(
            ancestor_holds_slots(&absent, Some(&*quiet), &MarkerLog::new()),
            (false, None),
            "no marker queues, silently: {absent:?}"
        );
    }
    assert!(
        !quiet.join("waits.jsonl").exists(),
        "an absent marker writes nothing"
    );
}

/// `.cargo/config.toml` reaches the tests: without it, every test that
/// builds a runner with default settings would queue on the real host slots.
/// Slots are off, and the marker is forced empty: a bare `held` is refused
/// once amont's held-marker rollout ends.
#[test]
fn cargo_pins_the_host_slot_for_tests() {
    assert_eq!(std::env::var(HOST_SLOT_ENV).as_deref(), Ok(""));
    assert_eq!(
        std::env::var(greenharness_slots::host::SLOTS_ENV).as_deref(),
        Ok("0")
    );
}

#[cfg(unix)]
mod host_slots {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    /// A private slot directory, as `greenharness-slots` requires (0700, ours).
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
    fn other_holder(dir: &Path, command: &str) -> greenharness_slots::Lease {
        let mut waiter = greenharness_slots::Waiter::new_in(
            dir,
            greenharness_slots::Sizing::Fixed(1),
            1,
            greenharness_slots::Holder::here("other", command),
        )
        .expect("waiter");
        match waiter.poll().expect("poll") {
            greenharness_slots::Poll::Granted(lease) => lease,
            greenharness_slots::Poll::Waiting(q) => panic!("the slot was free: {q:?}"),
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
        shared.sizing = Some(greenharness_slots::Sizing::Fixed(1));
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
        std::fs::remove_dir_all(&scratch).ok(); // best-effort: a scratch dir
    }

    /// Queued on the host slots, a command gets its weight's share of the
    /// slots' cores, as a greengate holder does (the coordinator's cap alone
    /// would give it twice that), and its wait line names the sizing.
    #[test]
    fn a_queued_command_gets_the_slots_share_and_its_wait_line_names_it() {
        let scratch = temp_dir("slots-width");
        let slots = scratch.join("queue");
        let gate = gate_with(2);
        register(&gate, "run-w2");
        // 8 cores, 4 fixed slots, a coordinator cap of 2.
        let mut shared = admission(&gate, "run-w2", 2, 8);
        shared.slots = Some(slots.clone());
        shared.sizing = Some(greenharness_slots::Sizing::Fixed(4));
        let lease = shared.admit(&ask(&BTreeMap::new(), 1)).expect("admitted");
        assert_eq!(
            lease.env().get("CARGO_BUILD_JOBS").map(String::as_str),
            Some("2"),
            "8 cores / 4 slots, not 8 / the coordinator's 2"
        );
        let line = std::fs::read_to_string(slots.join("waits.jsonl")).expect("waits.jsonl");
        for part in [
            "\"tool\":\"relais\"",
            "\"weight\":1",
            "\"jobs\":2",
            "\"cap\":4",
            "\"cores\":8",
            "\"outcome\":\"admitted\"",
            // The line counts its own lease (greenharness-slots 4.13).
            "\"holders\":1,",
            "\"holder_jobs\":2,",
        ] {
            assert!(line.contains(part), "{part} in {line}");
        }
        drop(lease);
        // The dropped lease appends one release line, with its hold time.
        let journal = std::fs::read_to_string(slots.join("waits.jsonl")).expect("waits.jsonl");
        let released: Vec<&str> = journal
            .lines()
            .filter(|l| l.contains("\"outcome\":\"released\""))
            .collect();
        assert_eq!(released.len(), 1, "{journal}");
        assert!(released[0].contains("\"tool\":\"relais\""), "{journal}");
        assert!(released[0].contains("\"held_ms\":"), "{journal}");
        // The run's id is the holder's session, so the release names the run.
        for part in ["\"session\":\"run-w2\"", "\"weight\":1,", "\"jobs\":2,"] {
            assert!(released[0].contains(part), "{part} in {journal}");
        }
        std::fs::remove_dir_all(&scratch).ok(); // best-effort: a scratch dir
    }

    /// With no host queue, the coordinator's cap shares the cores, as it
    /// always did: the width does not shrink for a run that queues nowhere.
    #[test]
    fn with_no_host_queue_the_coordinators_cap_shares_the_cores() {
        let gate = gate_with(2);
        register(&gate, "run-w3");
        let alone = admission(&gate, "run-w3", 2, 8);
        assert!(alone.slots.is_none());
        let lease = alone.admit(&ask(&BTreeMap::new(), 1)).expect("admitted");
        assert_eq!(
            lease.env().get("CARGO_BUILD_JOBS").map(String::as_str),
            Some("4"),
            "8 cores / the coordinator's 2"
        );
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
        std::fs::remove_dir_all(&open).ok(); // best-effort: a scratch dir
    }

    /// A refused directory leaves the command outside any host queue: it
    /// gets the coordinator's share of the cores, never the narrower slots'
    /// share it would have had under a host slot.
    #[test]
    fn a_refused_directory_runs_at_the_coordinators_width() {
        let open = slot_dir("open-width");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let gate = gate_with(2);
        register(&gate, "run-r");
        // 8 cores, 4 fixed slots, a coordinator cap of 2.
        let mut refused = admission(&gate, "run-r", 2, 8);
        refused.slots = Some(open.clone());
        refused.sizing = Some(greenharness_slots::Sizing::Fixed(4));
        let lease = refused
            .admit(&ask(&BTreeMap::new(), 1))
            .expect("falls back");
        assert_eq!(lease.host_slots(), 0);
        assert_eq!(
            lease.env().get("CARGO_BUILD_JOBS").map(String::as_str),
            Some("4"),
            "8 cores / the coordinator's 2, not 8 / 4 slots"
        );
        drop(lease);
        std::fs::remove_dir_all(&open).ok(); // best-effort: a scratch dir
    }

    #[test]
    fn a_weight_above_the_cap_is_refused_not_capped() {
        let dir = slot_dir("short");
        let gate = gate_with(2);
        register(&gate, "run-w");
        let mut shared = admission(&gate, "run-w", 2, 16);
        shared.slots = Some(dir.clone());
        shared.sizing = Some(greenharness_slots::Sizing::Fixed(1));
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
        std::fs::remove_dir_all(&dir).ok(); // best-effort: a scratch dir
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
        std::fs::remove_dir_all(&dir).ok(); // best-effort: a scratch dir
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
        with_slots.sizing = Some(greenharness_slots::Sizing::Fixed(1));
        with_slots.budgets = budgets_on(&clock, 60);
        with_slots.events =
            Events::for_run_on("run-s", &dir, crate::protocol::Wire::to(std::io::sink()));
        with_slots.announce_every = Duration::from_millis(1);
        with_slots.poll = Duration::from_millis(5);
        // The other holder lets go once a wait has named it (or after 200
        // waits): a process another test forks holds copies of every
        // descriptor until its exec, so a few polls can find the admission
        // lock busy, which answers with no holder. The order of what the
        // gate saw proves the check did not run before the release.
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let noted = Arc::clone(&order);
        let release = Arc::clone(&held);
        let events_file = dir.join("events.jsonl");
        with_slots.sleeper = Advancing::then(&clock, move |number| {
            noted
                .lock()
                .expect("order")
                .push(format!("waited {number}"));
            let named = std::fs::read_to_string(&events_file)
                .is_ok_and(|text| text.contains("other: xcodebuild"));
            let mut held = release.lock().expect("held");
            if held.is_some() && (named || number >= 200) {
                noted
                    .lock()
                    .expect("order")
                    .push("other holder let go".into());
                held.take();
            }
        });
        let outcome = run_observed_within(ObservedRun {
            dir: &dir,
            spec: &sh("echo slot=$GREENHARNESS_HOST_SLOT"),
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
        let let_go = order
            .iter()
            .position(|e| e == "other holder let go")
            .expect("the other holder let go");
        assert!(let_go >= 1, "at least one wait came first: {order:?}");
        assert!(
            order[..let_go].iter().all(|e| e.starts_with("waited ")),
            "nothing ran before the release: {order:?}"
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
        std::fs::remove_dir_all(&slots_dir).ok(); // best-effort: a scratch dir
    }

    /// ADR-0015: a host-slot line that is still moving outlives
    /// `admission_wait_seconds`; the run is granted and not blocked after.
    #[test]
    fn a_moving_line_outlives_the_admission_budget_and_the_grant_is_not_blocked() {
        let slots_dir = slot_dir("moving");
        let held = Arc::new(Mutex::new(Some(other_holder(&slots_dir, "xcodebuild"))));
        let gate = Recording::new(gate_with(1));
        register(&gate.inner, "run-m");
        let clock = ManualClock::new();
        let mut with_slots = admission(&gate, "run-m", 1, 4);
        with_slots.slots = Some(slots_dir.clone());
        with_slots.sizing = Some(greenharness_slots::Sizing::Fixed(1));
        // 5 s of admission budget, a 1 s poll, and the holder lets go after
        // 20 polls: the wait runs 4x past the budget.
        with_slots.budgets = budgets_on(&clock, 5);
        with_slots.poll = Duration::from_secs(1);
        let release = Arc::clone(&held);
        with_slots.sleeper = Advancing::then(&clock, move |number| {
            if number >= 20 {
                release.lock().expect("held").take();
            }
        });
        let lease = with_slots
            .admit(&ask(&BTreeMap::new(), 1))
            .expect("a moving line is granted past the budget");
        assert!(with_slots.budgets.overrun() >= Duration::from_secs(10));
        assert!(
            with_slots
                .budgets
                .waited(crate::policy::WaitKind::Admission)
                < Duration::from_secs(5)
        );
        assert_eq!(
            with_slots.budgets.exhausted(),
            None,
            "the run is not blocked after the grant"
        );
        drop(lease);
        std::fs::remove_dir_all(&slots_dir).ok(); // best-effort: a scratch dir
    }

    /// ADR-0015: a line with no progress for the window ends the wait as the
    /// spent budget did: `admission_wait_exhausted`, a `deferred` row with
    /// `"end":"stalled"`, and the reason names the window and the place.
    #[test]
    fn a_stalled_line_ends_the_wait_and_the_row_says_so() {
        let dir = temp_dir("slots-stalled");
        let slots_dir = slot_dir("stalled");
        let _held = other_holder(&slots_dir, "xcodebuild");
        let gate = Recording::new(gate_with(1));
        register(&gate.inner, "run-st");
        let clock = ManualClock::new();
        let mut with_slots = admission(&gate, "run-st", 1, 4);
        with_slots.slots = Some(slots_dir.clone());
        with_slots.sizing = Some(greenharness_slots::Sizing::Fixed(1));
        with_slots.budgets = budgets_on(&clock, 600);
        with_slots.poll = Duration::from_millis(5);
        with_slots.stall_window = Duration::from_secs(1);
        with_slots.events =
            Events::for_run_on("run-st", &dir, crate::protocol::Wire::to(std::io::sink()));
        // deliberate: the crate's `Stall` reads the real clock (5.0.0 has no
        // seam for its admitted-at term), so the 1 s window passes in real time.
        // holds-until: greenharness-slots exposes admitted_at; then drive
        // Stall::observe_at from the ManualClock and drop this sleep.
        with_slots.sleeper = Advancing::then(&clock, |_| {
            std::thread::sleep(Duration::from_millis(20));
        });
        let stop = with_slots
            .admit(&ask(&BTreeMap::new(), 1))
            .err()
            .expect("a line that does not move ends the wait");
        assert!(
            matches!(
                stop,
                LeaseStop::SlotLine(super::super::SlotLineEnd::Stalled { window_s: 1, .. })
            ),
            "{stop:?}"
        );
        let journal = std::fs::read_to_string(slots_dir.join("waits.jsonl")).expect("waits.jsonl");
        assert!(journal.contains("\"outcome\":\"deferred\""), "{journal}");
        assert!(journal.contains("\"end\":\"stalled\""), "{journal}");
        let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
        assert!(
            written.contains("the host slot line made no progress for 1 s at position"),
            "{written}"
        );
        std::fs::remove_dir_all(&slots_dir).ok(); // best-effort: a scratch dir
    }

    /// ADR-0015: once the line is joined, a lock that fails three polls in a
    /// row ends the wait with its own cause. It is never a spent budget, and
    /// never runs the command unqueued.
    #[test]
    fn an_unreadable_line_after_joining_ends_the_wait_with_its_own_cause() {
        let dir = temp_dir("slots-unreadable");
        let slots_dir = slot_dir("unreadable");
        let _held = other_holder(&slots_dir, "xcodebuild");
        let gate = Recording::new(gate_with(1));
        register(&gate.inner, "run-u");
        let clock = ManualClock::new();
        let mut with_slots = admission(&gate, "run-u", 1, 4);
        with_slots.slots = Some(slots_dir.clone());
        with_slots.sizing = Some(greenharness_slots::Sizing::Fixed(1));
        with_slots.budgets = budgets_on(&clock, 600);
        with_slots.poll = Duration::from_millis(5);
        with_slots.events =
            Events::for_run_on("run-u", &dir, crate::protocol::Wire::to(std::io::sink()));
        let gone = slots_dir.clone();
        with_slots.sleeper = Advancing::then(&clock, move |number| {
            if number == 1 {
                std::fs::remove_dir_all(&gone).ok(); // best-effort: breaks the queue
            }
        });
        let stop = with_slots
            .admit(&ask(&BTreeMap::new(), 1))
            .err()
            .expect("an unreadable line ends the wait");
        assert!(
            matches!(
                stop,
                LeaseStop::SlotLine(super::super::SlotLineEnd::Unreadable { failures: 3, .. })
            ),
            "{stop:?}"
        );
        let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
        assert!(written.contains("admission_queue_unreadable"), "{written}");
    }

    /// A cancel during a host-slot wait ends it from relais's side of the
    /// pause: `Cancelled`, one `deferred` row with no `end`, and the waiter
    /// has left the queue (no ticket of it is left in the directory).
    #[test]
    fn a_cancel_while_queued_for_a_host_slot_ends_the_wait_and_leaves_the_line() {
        let slots_dir = slot_dir("cancel");
        let _held = other_holder(&slots_dir, "xcodebuild");
        let gate = Recording::new(gate_with(1));
        register(&gate.inner, "run-c");
        let clock = ManualClock::new();
        let mut with_slots = admission(&gate, "run-c", 1, 4);
        with_slots.slots = Some(slots_dir.clone());
        with_slots.sizing = Some(greenharness_slots::Sizing::Fixed(1));
        with_slots.budgets = budgets_on(&clock, 600);
        with_slots.poll = Duration::from_millis(5);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        with_slots.sleeper = Advancing::then(&clock, move |number| {
            if number == 2 {
                flag.store(true, Ordering::SeqCst);
            }
        });
        let env = BTreeMap::new();
        let mut asked = ask(&env, 1);
        asked.cancel = Some(&cancel);
        let stop = with_slots
            .admit(&asked)
            .err()
            .expect("a cancel ends the wait");
        assert!(matches!(stop, LeaseStop::Cancelled), "{stop:?}");
        // Every pause beat the lease before it slept: the coordinator never
        // saw the queued lease go quiet. The cancel is read on the third.
        let beats = gate
            .events()
            .iter()
            .filter(|e| e.starts_with("heartbeat "))
            .count();
        assert_eq!(beats, 3, "one heartbeat per pause: {:?}", gate.events());
        let journal = std::fs::read_to_string(slots_dir.join("waits.jsonl")).expect("waits.jsonl");
        let deferred: Vec<&str> = journal
            .lines()
            .filter(|l| l.contains("\"tool\":\"relais\"") && l.contains("\"outcome\":\"deferred\""))
            .collect();
        assert_eq!(deferred.len(), 1, "{journal}");
        assert!(!deferred[0].contains("\"end\""), "{journal}");
        let queue = greenharness_slots::status_in(&slots_dir, greenharness_slots::Sizing::Fixed(1))
            .expect("status");
        assert_eq!(
            queue.waiting, 0,
            "the cancelled waiter left the line: {queue:?}"
        );
        std::fs::remove_dir_all(&slots_dir).ok(); // best-effort: a scratch dir
    }

    /// A lock that fails before the line was joined turns the host slots off
    /// for the run, said once with its cause, and the command runs unqueued
    /// (`Ok(None)` from the wait: no host slot held), as it did before the
    /// crate's loop.
    #[test]
    fn a_lock_that_fails_before_joining_turns_the_host_slots_off_once() {
        let dir = temp_dir("slots-prejoin");
        let slots_dir = slot_dir("prejoin");
        // `admission` as a directory: the ticket is made, the lock cannot be.
        // Relies on the crate's lock file name (greenharness-slots `poll_with`).
        std::fs::create_dir(slots_dir.join("admission")).expect("admission dir");
        let gate = Recording::new(gate_with(1));
        register(&gate.inner, "run-pj");
        let clock = ManualClock::new();
        let mut with_slots = admission(&gate, "run-pj", 1, 4);
        with_slots.slots = Some(slots_dir.clone());
        with_slots.sizing = Some(greenharness_slots::Sizing::Fixed(1));
        with_slots.budgets = budgets_on(&clock, 600);
        with_slots.poll = Duration::from_millis(5);
        with_slots.sleeper = Advancing::new(&clock);
        with_slots.events =
            Events::for_run_on("run-pj", &dir, crate::protocol::Wire::to(std::io::sink()));
        let lease = with_slots
            .admit(&ask(&BTreeMap::new(), 1))
            .expect("the command runs without a host slot");
        assert_eq!(lease.host_slots(), 0);
        let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
        assert_eq!(written.matches("host_slots_off").count(), 1, "{written}");
        assert!(
            written.contains("a host slot could not be locked"),
            "{written}"
        );
        drop(lease);
        std::fs::remove_dir_all(&slots_dir).ok(); // best-effort: a scratch dir
    }

    /// ADR-0015: the coordinator's queue and the host-slot queue charge the
    /// same budget, and the overrun of one never lets the other off: a
    /// coordinator wait, a host-slot wait past the budget, then a coordinator
    /// wait that exhausts it, all through `Queue::charging`.
    #[test]
    fn the_coordinator_still_exhausts_after_a_host_slot_overrun() {
        let clock = ManualClock::new();
        let gate = Recording::new(gate_with(1));
        let mut shared = admission(&gate, "run-q", 1, 4);
        shared.budgets = budgets_on(&clock, 50);
        shared.poll = Duration::from_secs(1);
        let sleeper = Advancing::new(&clock);
        let slot = |bound| Holding::Slot {
            class: "x",
            position: 1,
            behind: None,
            bound,
        };
        let mut queue = Queue::new(&shared);
        for _ in 0..10 {
            queue
                .wait(slot(Bound::Budget), None, sleeper.as_ref())
                .expect("10 s of coordinator queue is inside the budget");
        }
        for _ in 0..60 {
            queue
                .wait(slot(Bound::Stall), None, sleeper.as_ref())
                .expect("a host-slot line that moves is not ended by the budget");
        }
        assert!(shared.budgets.overrun() >= Duration::from_secs(15));
        // The first coordinator tick charges nothing yet; the next one finds
        // the millisecond the host-slot wait left unspent gone.
        queue
            .wait(slot(Bound::Budget), None, sleeper.as_ref())
            .expect("a wait is charged from its second poll");
        let ended = queue.wait(slot(Bound::Budget), None, sleeper.as_ref());
        assert!(
            matches!(
                ended,
                Err(LeaseStop::Exhausted(
                    crate::policy::Exhausted::AdmissionWait
                ))
            ),
            "{ended:?}"
        );
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

/// amont host-slot-admission t07/t12: a setup takes one host slot and keeps
/// it across its steps, relabelled per step, instead of joining the tail of
/// the line again between them.
#[cfg(unix)]
mod setup_keeps_one_slot {
    use super::*;
    use crate::policy::VerificationProfile;
    use crate::verify::{run_setup_gated_within, SetupGate, SetupRun};
    use std::os::unix::fs::DirBuilderExt;

    /// A private slot directory, as `greenharness-slots` requires (0700, ours).
    fn slot_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("relais-kept-slots-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // best-effort: a fresh scratch dir
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .expect("slot dir");
        dir
    }

    /// Another holder of one slot in `dir`, through the same crate.
    fn other_holder(dir: &Path) -> greenharness_slots::Lease {
        let mut waiter = other_waiter(dir);
        match waiter.poll().expect("poll") {
            greenharness_slots::Poll::Granted(lease) => lease,
            greenharness_slots::Poll::Waiting(q) => panic!("the slot was free: {q:?}"),
        }
    }

    fn other_waiter(dir: &Path) -> greenharness_slots::Waiter {
        greenharness_slots::Waiter::new_in(
            dir,
            greenharness_slots::Sizing::Fixed(1),
            1,
            greenharness_slots::Holder::here("other", "cargo test"),
        )
        .expect("waiter")
    }

    /// relais's rows of `outcome` in the slot journal.
    fn rows(slots: &Path, outcome: &str) -> usize {
        let journal = std::fs::read_to_string(slots.join("waits.jsonl")).unwrap_or_default();
        journal
            .lines()
            .filter(|l| l.contains("\"tool\":\"relais\""))
            .filter(|l| l.contains(&format!("\"outcome\":\"{outcome}\"")))
            .count()
    }

    fn profile(setup: Vec<CommandSpec>) -> VerificationProfile {
        VerificationProfile {
            setup,
            commands: Vec::new(),
            amont_checks: Vec::new(),
            amont_waivers: Vec::new(),
            inputs: Vec::new(),
            cache_baseline: false,
        }
    }

    /// Step `n` writes the held marker it sees and the holder records.
    fn step(n: usize, slots: &Path) -> CommandSpec {
        let mut spec = sh(&format!(
            "printf %s \"$GREENHARNESS_HOST_SLOT\" > marker{n}; \
             cat \"$SLOTS\"/holders/*.who > holder{n}"
        ));
        spec.env.insert("SLOTS".into(), slots.display().to_string());
        spec
    }

    fn admission_on<'a>(
        gate: &'a Recording,
        run: &str,
        slots: &Path,
        events_dir: &Path,
    ) -> HeavyAdmission<'a> {
        register(&gate.inner, run);
        let mut kept = admission(gate, run, 1, 4);
        kept.slots = Some(slots.to_path_buf());
        kept.sizing = Some(greenharness_slots::Sizing::Fixed(1));
        kept.events =
            Events::for_run_on(run, events_dir, crate::protocol::Wire::to(std::io::sink()));
        kept
    }

    fn events(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default()
    }

    /// Joins the host-slot line after step 0 and polls once after each step,
    /// noting how many queue sleeps there were by then.
    struct JoinsBehind {
        slots: PathBuf,
        sleeper: Arc<Advancing>,
        other: Option<greenharness_slots::Waiter>,
        waiting: Vec<bool>,
        slept: Vec<usize>,
    }

    impl SetupGate for JoinsBehind {
        fn skip(&mut self, _: usize, _: &CommandSpec) -> bool {
            false
        }

        fn ran(&mut self, index: usize, _: &CommandSpec, _: bool) {
            self.slept
                .push(self.sleeper.slept.lock().expect("sleeps").len());
            if index == 0 {
                self.other = Some(other_waiter(&self.slots));
            }
            if let Some(other) = &mut self.other {
                let polled = other.poll().expect("poll");
                self.waiting
                    .push(matches!(polled, greenharness_slots::Poll::Waiting(_)));
            }
        }
    }

    /// The starvation in miniature, fixed: 3 steps, 1 slot held by another
    /// tool for the first 3 s. The first step waits once; a waiter that joins
    /// after it never gets the slot between steps; one `slots::take` (one
    /// `admitted` row), one `released` row, one admission-budget spend.
    #[test]
    fn a_setup_takes_one_host_slot_for_all_its_steps() {
        let work = temp_dir("kept-setup");
        let slots = slot_dir("setup");
        let held = Arc::new(Mutex::new(Some(other_holder(&slots))));
        let gate = Recording::new(gate_with(1));
        let clock = ManualClock::new();
        let mut kept = admission_on(&gate, "run-k", &slots, &work);
        kept.budgets = budgets_on(&clock, 50);
        kept.poll = Duration::from_secs(1);
        let release = Arc::clone(&held);
        let sleeper = Advancing::then(&clock, move |number| {
            if number >= 3 {
                release.lock().expect("held").take();
            }
        });
        kept.sleeper = sleeper.clone();
        let steps = profile((0..3).map(|n| step(n, &slots)).collect());
        let mut joins = JoinsBehind {
            slots: slots.clone(),
            sleeper: Arc::clone(&sleeper),
            other: None,
            waiting: Vec::new(),
            slept: Vec::new(),
        };
        let outcomes = run_setup_gated_within(SetupRun {
            dir: &work,
            profile: &steps,
            logs_dir: &work.join("logs"),
            prefix: "base",
            events: &kept.events,
            cancel: None,
            gate: &mut joins,
            admission: &kept,
        })
        .expect("setup");
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|o| !o.failed()), "{outcomes:?}");
        // One wait, in the first step, charged once: every queue sleep
        // happened before step 0 ended (at least the 3 the holder needed;
        // a poll can also find the admission lock busy), and the admission
        // budget holds exactly those 1 s sleeps.
        let journal = std::fs::read_to_string(slots.join("waits.jsonl")).unwrap_or_default();
        let slept = sleeper.slept.lock().expect("sleeps").len();
        assert!(slept >= 3, "{slept} sleeps: {journal}");
        assert_eq!(
            joins.slept,
            vec![slept; 3],
            "no step after the first slept: {journal}"
        );
        assert_eq!(
            kept.budgets.waited(crate::policy::WaitKind::Admission),
            Duration::from_secs(u64::try_from(slept).expect("small"))
        );
        assert_eq!(kept.budgets.exhausted(), None);
        // The waiter that joined after step 0 waited through every step.
        assert_eq!(joins.waiting, vec![true, true, true]);
        // Every `slots::take` ends in one row with `waited_ms` (admitted or
        // deferred): exactly one take for the 3 steps.
        let journal = std::fs::read_to_string(slots.join("waits.jsonl")).expect("journal");
        let takes = journal
            .lines()
            .filter(|l| l.contains("\"tool\":\"relais\"") && l.contains("\"waited_ms\""))
            .count();
        assert_eq!(takes, 1, "one slots::take for 3 steps: {journal}");
        assert_eq!(rows(&slots, "admitted"), 1);
        assert_eq!(rows(&slots, "released"), 1);
        // ...and gets the slot once the setup is over.
        let mut other = joins.other.take().expect("joined");
        assert!(matches!(
            other.poll().expect("poll"),
            greenharness_slots::Poll::Granted(_)
        ));
        for (n, outcome) in outcomes.iter().enumerate() {
            let marker = std::fs::read_to_string(work.join(format!("marker{n}"))).expect("marker");
            assert_eq!(marker, HELD_MARKER, "step {n} runs under the held marker");
            let holder = std::fs::read_to_string(work.join(format!("holder{n}"))).expect("holder");
            assert!(
                holder.contains(&format!("\"command\":\"{}\"", outcome.label)),
                "step {n} is the named holder: {holder}"
            );
        }
        let written = events(&work);
        assert_eq!(written.matches("host_slot_kept").count(), 2, "{written}");
        let first_kept = written.find("host_slot_kept").expect("kept");
        assert!(
            written
                .rfind("waiting_for_slot")
                .is_none_or(|at| at < first_kept),
            "no step after the first waits: {written}"
        );
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }

    /// Cancels the setup after step 0.
    struct CancelsAfterFirst<'c>(&'c AtomicBool);

    impl SetupGate for CancelsAfterFirst<'_> {
        fn skip(&mut self, _: usize, _: &CommandSpec) -> bool {
            false
        }

        fn ran(&mut self, _: usize, _: &CommandSpec, _: bool) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// A setup that fails at its first step, and one cancelled after it,
    /// each give the kept slot back exactly once.
    #[test]
    fn a_failed_or_cancelled_setup_gives_its_slot_back_once() {
        let work = temp_dir("kept-fail");
        let slots = slot_dir("fail");
        let gate = Recording::new(gate_with(1));
        let failing = admission_on(&gate, "run-f", &slots, &work);
        let steps = profile(vec![sh("exit 1"), step(1, &slots)]);
        let outcomes = run_setup_gated_within(SetupRun {
            dir: &work,
            profile: &steps,
            logs_dir: &work.join("logs"),
            prefix: "base",
            events: &failing.events,
            cancel: None,
            gate: &mut crate::verify::RunEveryStep,
            admission: &failing,
        })
        .expect("setup");
        assert_eq!(outcomes.len(), 1, "stops at the failure");
        assert_eq!(rows(&slots, "admitted"), 1);
        assert_eq!(rows(&slots, "released"), 1);

        let cancel = AtomicBool::new(false);
        let cancelled = admission_on(&gate, "run-c", &slots, &work);
        let steps = profile(vec![step(0, &slots), step(1, &slots)]);
        run_setup_gated_within(SetupRun {
            dir: &work,
            profile: &steps,
            logs_dir: &work.join("logs"),
            prefix: "cand",
            events: &cancelled.events,
            cancel: Some(&cancel),
            gate: &mut CancelsAfterFirst(&cancel),
            admission: &cancelled,
        })
        .expect("a cancel ends the setup, not the call");
        assert_eq!(rows(&slots, "admitted"), 2);
        assert_eq!(rows(&slots, "released"), 2);
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }

    fn labelled<'a>(label: &'a str, env: &'a BTreeMap<String, String>, weight: u32) -> Ask<'a> {
        Ask {
            phase: "base-setup",
            label,
            weight,
            env,
            cancel: None,
        }
    }

    /// Under an open kept slot, the second command runs under the first's
    /// slot with the marker, and the holder record names it.
    #[test]
    fn a_second_command_under_a_kept_slot_is_relabelled_not_queued() {
        let work = temp_dir("kept-reuse");
        let slots = slot_dir("reuse");
        let gate = Recording::new(gate_with(1));
        let kept = admission_on(&gate, "run-r", &slots, &work);
        let none = BTreeMap::new();
        let open = kept.keep_slot();
        drop(kept.admit(&labelled("step-1", &none, 1)).expect("first"));
        let second = kept.admit(&labelled("step-2", &none, 1)).expect("second");
        assert_eq!(second.host_slots(), 1);
        assert_eq!(
            second.env().get(HOST_SLOT_ENV).map(String::as_str),
            Some(HELD_MARKER)
        );
        let record =
            std::fs::read_to_string(slots.join("holders/slot-0.who")).expect("holder record");
        assert!(record.contains("\"command\":\"step-2\""), "{record}");
        drop(second);
        assert_eq!(rows(&slots, "admitted"), 1);
        assert_eq!(rows(&slots, "released"), 0, "kept past the command");
        drop(open);
        assert_eq!(rows(&slots, "released"), 1);
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }

    /// Takes the only coordinator token for another run after step 0.
    struct TakesTheToken<'g>(&'g LocalGate);

    impl SetupGate for TakesTheToken<'_> {
        fn skip(&mut self, _: usize, _: &CommandSpec) -> bool {
            false
        }

        fn ran(&mut self, index: usize, _: &CommandSpec, _: bool) {
            if index == 0 {
                register(self.0, "run-other");
                assert_eq!(
                    self.0
                        .admit(&heavy_request("other", "run-other", 1))
                        .expect("answered"),
                    Decision::Granted
                );
            }
        }
    }

    /// The coordinator queues step 1 of a setup: the kept slot is given back
    /// before that wait (never a slot held while waiting for a token), and
    /// the step takes a slot afresh.
    #[test]
    fn a_coordinator_wait_gives_the_kept_slot_back_first() {
        let work = temp_dir("kept-coord");
        let slots = slot_dir("coord");
        let gate = Arc::new(Recording::new(gate_with(1)));
        let clock = ManualClock::new();
        let mut kept = admission_on(&gate, "run-q", &slots, &work);
        kept.budgets = budgets_on(&clock, 50);
        kept.poll = Duration::from_secs(1);
        let other = Arc::clone(&gate);
        kept.sleeper = Advancing::then(&clock, move |number| {
            if number == 1 {
                // The other run's command ends: its token comes back.
                other.inner.release("other").expect("release");
                other.inner.settle("other", Some(0)).expect("settle");
            }
        });
        let steps = profile(vec![step(0, &slots), step(1, &slots)]);
        let outcomes = run_setup_gated_within(SetupRun {
            dir: &work,
            profile: &steps,
            logs_dir: &work.join("logs"),
            prefix: "base",
            events: &kept.events,
            cancel: None,
            gate: &mut TakesTheToken(&gate.inner),
            admission: &kept,
        })
        .expect("setup");
        assert!(outcomes.iter().all(|o| !o.failed()), "{outcomes:?}");
        let written = events(&work);
        assert!(
            written.contains("kept_slot_given_back")
                && written.contains("the coordinator queued this step"),
            "{written}"
        );
        assert_eq!(rows(&slots, "admitted"), 2, "taken afresh");
        assert_eq!(rows(&slots, "released"), 2);
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }

    /// The slot directory becomes unusable between two commands: the kept
    /// slot is given back, and the next command runs queued nowhere.
    #[test]
    fn an_unusable_slot_dir_gives_the_kept_slot_back() {
        use std::os::unix::fs::PermissionsExt;
        let work = temp_dir("kept-unusable");
        let slots = slot_dir("unusable");
        let gate = Recording::new(gate_with(1));
        let kept = admission_on(&gate, "run-x", &slots, &work);
        let none = BTreeMap::new();
        let open = kept.keep_slot();
        drop(kept.admit(&labelled("step-1", &none, 1)).expect("first"));
        std::fs::set_permissions(&slots, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let second = kept.admit(&labelled("step-2", &none, 1)).expect("second");
        assert_eq!(second.host_slots(), 0, "queued nowhere");
        assert!(
            events(&work).contains("the host slots are no longer usable"),
            "{}",
            events(&work)
        );
        assert_eq!(rows(&slots, "released"), 1, "given back before step 2 ran");
        drop(second);
        drop(open);
        assert_eq!(rows(&slots, "released"), 1);
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }

    /// An unreachable coordinator on the second command: the kept slot is
    /// given back before the retry sleep.
    #[test]
    fn an_unreachable_coordinator_gives_the_kept_slot_back_first() {
        let work = temp_dir("kept-down");
        let slots = slot_dir("down");
        let gate = Recording::new(gate_with(1));
        let mut kept = admission_on(&gate, "run-u2", &slots, &work);
        let clock = ManualClock::new();
        kept.budgets = budgets_on(&clock, 50);
        kept.sleeper = Advancing::new(&clock);
        let none = BTreeMap::new();
        let open = kept.keep_slot();
        drop(kept.admit(&labelled("step-1", &none, 1)).expect("first"));
        gate.down.store(1, Ordering::SeqCst);
        let second = kept.admit(&labelled("step-2", &none, 1)).expect("second");
        let written = events(&work);
        assert!(
            written.contains("the coordinator could not be reached"),
            "{written}"
        );
        assert_eq!(rows(&slots, "admitted"), 2);
        drop(second);
        drop(open);
        assert_eq!(rows(&slots, "released"), 2);
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }

    /// A step of another weight gives the kept slot back and takes its own.
    #[test]
    fn a_step_of_another_weight_takes_its_own_slot() {
        let work = temp_dir("kept-weight");
        let slots = slot_dir("weight");
        let gate = Recording::new(gate_with(2));
        register(&gate.inner, "run-w");
        let mut kept = admission(&gate, "run-w", 2, 4);
        kept.slots = Some(slots.clone());
        kept.sizing = Some(greenharness_slots::Sizing::Fixed(2));
        kept.events =
            Events::for_run_on("run-w", &work, crate::protocol::Wire::to(std::io::sink()));
        let none = BTreeMap::new();
        let open = kept.keep_slot();
        drop(kept.admit(&labelled("step-1", &none, 1)).expect("first"));
        let second = kept.admit(&labelled("step-2", &none, 2)).expect("second");
        assert_eq!(second.host_slots(), 2);
        assert!(events(&work).contains("asks a weight of 2, not 1"));
        drop(second);
        drop(open);
        assert_eq!(rows(&slots, "admitted"), 2);
        assert_eq!(rows(&slots, "released"), 2);
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }

    /// The disk falls short after the coordinator's grant: the kept slot is
    /// given back before the disk wait, as the step's own slot was before.
    #[test]
    fn a_disk_short_after_the_grant_gives_the_kept_slot_back() {
        let work = temp_dir("kept-disk");
        let slots = slot_dir("disk");
        let disk = StubDisk::with(10 * GIB);
        let gate = Recording::new(gate_with(1));
        let clock = ManualClock::new();
        let mut kept = checking(admission_on(&gate, "run-d2", &slots, &work), 8 * GIB, &disk);
        kept.budgets = budgets_on(&clock, 50);
        let refill = Arc::clone(&disk);
        kept.sleeper = Advancing::then(&clock, move |_| refill.set(10 * GIB));
        let none = BTreeMap::new();
        let open = kept.keep_slot();
        drop(kept.admit(&labelled("step-1", &none, 1)).expect("first"));
        disk.set(5 * GIB);
        let second = kept.admit(&labelled("step-2", &none, 1)).expect("second");
        assert!(
            events(&work).contains("the disk fell short after the grant"),
            "{}",
            events(&work)
        );
        assert_eq!(rows(&slots, "admitted"), 2);
        drop(second);
        drop(open);
        assert_eq!(rows(&slots, "released"), 2);
        std::fs::remove_dir_all(&slots).ok(); // best-effort: a scratch dir
    }
}
