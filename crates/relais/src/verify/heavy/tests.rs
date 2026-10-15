use super::*;
use crate::admission::{
    Enforcement, HeartbeatStatus, LifecycleOutcome, LocalGate, ReleaseWriteOutcome, ResumeOutcome,
    RunRegistration, WaitOutcome, WithdrawOutcome, WriteLeaseOutcome,
};
use crate::policy::{BudgetLimits, CommandSpec, ConcurrencyLimits, WallClock};
use crate::verify::run_observed_within;
use std::path::Path;

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
        poll: Duration::from_millis(10),
        announce_every: ANNOUNCE_EVERY,
        tries: Tries::default(),
    }
}

/// A gate that remembers, in order, every grant and every release.
struct Recording {
    inner: LocalGate,
    events: Mutex<Vec<String>>,
}

impl Recording {
    fn new(inner: LocalGate) -> Self {
        Self {
            inner,
            events: Mutex::new(Vec::new()),
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
        self.inner.bind_group(id, runner, pgid)
    }
    fn heartbeat(&self, dispatch_id: &str) -> Result<HeartbeatStatus, GateError> {
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
    let id = dispatch_id("run-1", "base-cmd0", "make@ab12:x", 2);
    assert_eq!(id, "run-1-heavy-base-cmd0-make@ab12_x-2");
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
                    &sh("sleep 0.3"),
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
    let holder = DispatchRequest {
        dispatch_id: "other-heavy".into(),
        run_id: "run-other".into(),
        session_id: "tab".into(),
        parent_dispatch: None,
        depth: 0,
        resource: ResourceClass::HeavyLocal,
        reserve_micros: 0,
        source: DispatchSource::NativeRun,
        caller_agent_id: None,
        weight: 1,
    };
    assert_eq!(gate.admit(&holder).expect("admit"), Decision::Granted);

    let budgets = Budgets::start(
        Arc::new(WallClock),
        BudgetLimits::resolve(600, 1, 60, Some(600), 0),
    );
    budgets.begin_work();
    let mut queued = admission(&gate, "run-q", 1, 4);
    queued.budgets = budgets.clone();
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
    assert!(
        budgets.worked() < Duration::from_millis(500),
        "the queue was taken out of work: {:?}",
        budgets.worked()
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
    let holder = DispatchRequest {
        dispatch_id: "other-heavy".into(),
        run_id: "run-other".into(),
        session_id: "tab".into(),
        parent_dispatch: None,
        depth: 0,
        resource: ResourceClass::HeavyLocal,
        reserve_micros: 0,
        source: DispatchSource::NativeRun,
        caller_agent_id: None,
        weight: 1,
    };
    assert_eq!(gate.admit(&holder).expect("admit"), Decision::Granted);
    let events = Events::for_run_on("run-n", &dir, crate::protocol::Wire::to(std::io::sink()));
    let mut queued = admission(&gate, "run-n", 1, 4);
    queued.events = events;
    queued.announce_every = Duration::from_millis(100);
    queued.budgets = Budgets::start(
        Arc::new(WallClock),
        BudgetLimits::resolve(600, 1, 60, Some(600), 0),
    );
    let _ = queued.admit(&Ask {
        phase: "base-cmd0",
        label: "cmd0",
        weight: 1,
        env: &BTreeMap::new(),
        cancel: None,
    });
    let written = std::fs::read_to_string(dir.join("events.jsonl")).expect("events");
    let announced = written
        .lines()
        .filter(|line| line.contains("\"waiting_for_slot\""))
        .count();
    assert!(announced >= 2, "{written}");
    assert!(written.contains("\"class\":\"heavy_local\""), "{written}");
}

#[cfg(unix)]
mod host_slots {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    fn slot_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("relais-heavy-slots-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .expect("slot dir");
        dir
    }

    fn lock(path: &Path) -> std::fs::File {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .expect("slot file");
        let locked = crate::procs::try_flock(&file).expect("flock");
        assert!(locked, "the slot was free");
        file
    }

    #[test]
    fn a_partial_set_is_released_and_nothing_stays_held() {
        let dir = slot_dir("partial");
        let other = lock(&dir.join("slot-1"));
        assert!(
            slots::try_take(&dir, 2, 2).expect("tried").is_none(),
            "slot-1 is taken elsewhere, so a weight of two gets nothing"
        );
        // slot-0 was released on the back-off: a taker of one gets it.
        let one = slots::try_take(&dir, 2, 1)
            .expect("tried")
            .expect("slot-0 is free");
        assert_eq!(one.len(), 1);
        drop(other);
        drop(one);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_check_waits_for_a_held_host_slot_and_runs_holding_it() {
        let dir = temp_dir("slots");
        let slots_dir = slot_dir("wait");
        let held = lock(&slots_dir.join("slot-0"));
        let gate = gate_with(1);
        register(&gate, "run-s");
        let mut with_slots = admission(&gate, "run-s", 1, 4);
        with_slots.slots = Some(slots_dir.clone());

        let finished = AtomicBool::new(false);
        let log = std::thread::scope(|scope| {
            let runner = scope.spawn(|| {
                let outcome = run_observed_within(
                    &dir,
                    &sh("echo slot=$AMONT_HOST_SLOT"),
                    &dir.join("logs"),
                    "cmd0",
                    "x-cmd0",
                    &Events::silent(),
                    None,
                    &with_slots,
                )
                .expect("run");
                finished.store(true, Ordering::SeqCst);
                std::fs::read_to_string(outcome.log_path).expect("log")
            });
            std::thread::sleep(Duration::from_millis(600));
            assert!(
                !finished.load(Ordering::SeqCst),
                "the check ran while another process held the slot"
            );
            drop(held);
            runner.join().expect("the runner thread")
        });
        assert_eq!(log.trim(), "slot=held");
        std::fs::remove_dir_all(&slots_dir).ok();
    }
}
