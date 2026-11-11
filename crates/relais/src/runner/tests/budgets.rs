// -- the four budgets (ADR-0010, SPEC §9-§11, §23) ---------------------------
//
// work, admission wait, disk wait and total; each ends the run under its own
// name, and a wait never spends work. The clock arithmetic is tested on
// `Budgets` itself with a clock the test moves (`policy::budgets`); these
// tests drive whole runs.

use super::engine::budget_limits;
use super::*;

/// A gate whose only agent seat another tab holds, so this run queues.
fn gate_with_the_seat_taken() -> crate::admission::LocalGate {
    let gate = crate::admission::LocalGate::new(
        ConcurrencyLimits {
            max_active_agents: Some(1),
            ..ConcurrencyLimits::default()
        },
        crate::ledger::now_rfc3339,
    );
    gate.register_run(&RunRegistration {
        run_id: "other-run".into(),
        session_id: "other-tab".into(),
        budget_micros: None,
        max_agents: None,
        max_depth: None,
    })
    .expect("register");
    assert_eq!(
        gate.admit(&DispatchRequest {
            disk: None,
            dispatch_id: "other-dispatch".into(),
            run_id: "other-run".into(),
            session_id: "other-tab".into(),
            parent_dispatch: None,
            depth: 0,
            resource: ResourceClass::ModelWork,
            reserve_micros: 0,
            source: DispatchSource::ManagedRun,
            caller_agent_id: None,
            weight: 1,
        })
        .expect("admit"),
        Decision::Granted
    );
    gate
}

/// A worker that edits nothing and takes `millis` doing it: its check
/// fails, so the machine wants a repair, and the work budget is what
/// stops it.
fn slow_idle_worker(millis: u64) -> MockBackend {
    MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            return MockOutcome {
                result_text: Some("FINDINGS: none".into()),
                exit_code: Some(0),
                ..Default::default()
            };
        }
        std::thread::sleep(Duration::from_millis(millis));
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

#[test]
fn a_queue_that_outlasts_the_admission_wait_blocks_with_its_own_name() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let mut machine = fixture.machine_for(&repo);
    machine.resources.admission_wait_seconds = Some(1);
    let gate = gate_with_the_seat_taken();
    let outcome = fixture.execute_managed(
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &conditional_worker(""),
        &gate,
    );
    assert!(
        matches!(
            &outcome.terminal,
            Terminal::BlockedBy {
                reason: Reason::AdmissionWaitExhausted,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(outcome.state(), State::Blocked);
}

/// A gate whose only heavy-command token another run holds, so this run's
/// first check queues.
fn gate_with_the_heavy_token_taken() -> crate::admission::LocalGate {
    let gate = crate::admission::LocalGate::new(
        ConcurrencyLimits {
            max_heavy_commands: Some(1),
            ..ConcurrencyLimits::default()
        },
        crate::ledger::now_rfc3339,
    );
    gate.register_run(&RunRegistration {
        run_id: "other-run".into(),
        session_id: "other-tab".into(),
        budget_micros: None,
        max_agents: None,
        max_depth: None,
    })
    .expect("register");
    assert_eq!(
        gate.admit(&DispatchRequest {
            disk: None,
            dispatch_id: "other-heavy".into(),
            run_id: "other-run".into(),
            session_id: "other-tab".into(),
            parent_dispatch: None,
            depth: 0,
            resource: ResourceClass::HeavyLocal,
            reserve_micros: 0,
            source: DispatchSource::NativeRun,
            caller_agent_id: None,
            weight: 1,
        })
        .expect("admit"),
        Decision::Granted
    );
    gate
}

#[test]
fn a_check_queued_for_a_heavy_token_blocks_with_the_admission_wait_name() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let mut machine = fixture.machine_for(&repo);
    machine.resources.admission_wait_seconds = Some(1);
    let gate = gate_with_the_heavy_token_taken();
    let outcome = fixture.execute_managed(
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &conditional_worker(""),
        &gate,
    );
    assert!(
        matches!(
            &outcome.terminal,
            Terminal::BlockedBy {
                reason: Reason::AdmissionWaitExhausted,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(outcome.state(), State::Blocked);
}

#[test]
fn a_queue_does_not_spend_the_work_budget() {
    // The work budget (one second) is shorter than the wait, yet the run
    // ends on the wait budget: the queue was never charged to work.
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let mut machine = fixture.machine_for(&repo);
    machine.resources.admission_wait_seconds = Some(2);
    let gate = gate_with_the_seat_taken();
    let mut contract = fixture.contract(Review::Off);
    contract.limits.wall_seconds = Some(1);
    let outcome =
        fixture.execute_managed(&contract, &repo, &machine, &conditional_worker(""), &gate);
    assert!(
        matches!(
            &outcome.terminal,
            Terminal::BlockedBy {
                reason: Reason::AdmissionWaitExhausted,
                ..
            }
        ),
        "a one second work budget outlived by a two second queue: {outcome:?}"
    );
}

#[test]
fn the_total_ends_a_queued_run_before_its_wait_budget_does() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let mut machine = fixture.machine_for(&repo);
    machine.resources.admission_wait_seconds = Some(600);
    let gate = gate_with_the_seat_taken();
    let mut contract = fixture.contract(Review::Off);
    contract.limits.total_seconds = Some(1);
    let outcome =
        fixture.execute_managed(&contract, &repo, &machine, &conditional_worker(""), &gate);
    assert!(
        matches!(&outcome.terminal, Terminal::BudgetExhausted { detail } if detail.contains("total")),
        "{outcome:?}"
    );
}

#[test]
fn wall_seconds_caps_the_work_and_nothing_else() {
    // A worker that takes longer than `wall_seconds` and leaves its check
    // red: the repair is refused because the work budget is spent, while
    // the total (the default) has hours left.
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.wall_seconds = Some(1);
    // The repair floor is what would extend it; off, the refusal stays.
    let mut repo = repo;
    repo.execution.min_repair_seconds = 0;
    let outcome = fixture.execute(&contract, &repo, &slow_idle_worker(1_200));
    assert!(
        matches!(&outcome.terminal, Terminal::BudgetExhausted { detail } if detail.contains("work budget")),
        "{outcome:?}"
    );
    let events = std::fs::read_to_string(
        fixture
            .artifacts
            .join(outcome.run_id.as_str())
            .join("events.jsonl"),
    )
    .unwrap_or_default();
    assert!(!events.contains("\"limit\":\"total\""), "{events}");
}

// The wall-time floor of a repair or an escalation.
//
// These runs move a scripted ledger clock instead of sleeping: the budgets
// read the ledger's clock (`LedgerBudgetClock`), so a worker that "takes 80
// seconds" only adds 80 to the skew and the test spends no real time.

/// A ledger clock the test moves: by `skew` seconds, and by a further number
/// of seconds once a marker file exists (a check creates it, so time passes
/// while the check runs, which no in-process call can do).
struct SkewClock {
    skew: Arc<std::sync::atomic::AtomicI64>,
    after_marker: Option<(PathBuf, i64)>,
}

impl crate::ledger::Clock for SkewClock {
    fn now_rfc3339(&self) -> String {
        let base = chrono::DateTime::parse_from_rfc3339("2026-09-20T12:00:00+00:00")
            .expect("a fixed instant");
        let marked = self
            .after_marker
            .as_ref()
            .filter(|(marker, _)| marker.exists())
            .map_or(0, |(_, seconds)| *seconds);
        (base + chrono::Duration::seconds(self.skew.load(Ordering::SeqCst) + marked)).to_rfc3339()
    }
}

/// A fixture on a skewable clock.
fn skewed_fixture() -> (Fixture, Arc<std::sync::atomic::AtomicI64>) {
    let skew = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let fixture = Fixture::with_clock(Box::new(SkewClock {
        skew: Arc::clone(&skew),
        after_marker: None,
    }));
    (fixture, skew)
}

/// A worker that moves the clock by `steps[n]` seconds on its nth launch,
/// records the wall time it was launched with, and fixes the tree on launch
/// number `fixes_on` (counted from 1).
fn skewing_worker(
    skew: Arc<std::sync::atomic::AtomicI64>,
    steps: Vec<i64>,
    fixes_on: usize,
) -> (MockBackend, Arc<std::sync::Mutex<Vec<Duration>>>) {
    let walls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&walls);
    let launches = std::sync::atomic::AtomicUsize::new(0);
    let backend = MockBackend::new(move |spec| {
        let nth = launches.fetch_add(1, Ordering::SeqCst);
        record
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(spec.wall_timeout);
        skew.fetch_add(steps.get(nth).copied().unwrap_or(0), Ordering::SeqCst);
        if nth + 1 == fixes_on {
            std::fs::remove_file(spec.work_dir.join("src/main.rs"))
                .expect("the worker removes the file its check fails on");
        } else {
            // A different change each time, or the machine ends the run on
            // the same failure over an unchanged candidate.
            std::fs::write(spec.work_dir.join(format!("src/try-{nth}.txt")), "try\n")
                .expect("the worker writes its attempt's change");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    (backend, walls)
}

/// The `worker_dispatched` details of a run, in order.
fn dispatched_details(fixture: &Fixture, outcome: &RunOutcome) -> Vec<serde_json::Value> {
    fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("transitions")
        .into_iter()
        .filter(|t| t.reason == Reason::WorkerDispatched.as_str())
        .map(|t| t.detail.unwrap_or_default())
        .collect()
}

fn extension_of(detail: &serde_json::Value) -> Option<u64> {
    detail.get("work_extended_s").and_then(|v| v.as_u64())
}

/// A repo policy with a 100 s work budget, floors of 60 s (repair) and 70 s
/// (escalation).
fn floored_repo(fixture: &Fixture) -> RepoPolicy {
    floored_repo_checking(fixture, main_gone_check())
}

fn floored_repo_checking(fixture: &Fixture, check: CommandSpec) -> RepoPolicy {
    let mut repo = fixture.repo_policy(vec![check], 3);
    repo.execution.max_wall_seconds = 100;
    repo.execution.min_repair_seconds = 60;
    repo.execution.min_escalation_seconds = 70;
    repo
}

#[test]
fn a_repair_after_a_slow_first_attempt_is_extended_to_its_floor() {
    let (fixture, skew) = skewed_fixture();
    let repo = floored_repo(&fixture);
    let (backend, walls) = skewing_worker(skew, vec![80, 0], 2);
    let outcome = fixture.execute(&fixture.contract(Review::Off), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let walls = walls.lock().unwrap().clone();
    assert_eq!(walls.len(), 2);
    assert!(
        walls[1] >= Duration::from_secs(60),
        "20 s of work were left, the floor is 60 s: {:?}",
        walls[1]
    );
    let details = dispatched_details(&fixture, &outcome);
    assert_eq!(
        extension_of(&details[0]),
        None,
        "the initial attempt is never extended"
    );
    assert_eq!(extension_of(&details[1]), Some(40));
    let sentence = details[1]["detail"].as_str().expect("a detail sentence");
    assert!(sentence.contains("min_repair_seconds"), "{sentence}");
}

#[test]
fn a_repair_whose_floor_exceeds_the_total_is_not_launched() {
    let (fixture, skew) = skewed_fixture();
    let repo = floored_repo(&fixture);
    let (backend, walls) = skewing_worker(skew, vec![80, 0], 99);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.total_seconds = Some(90);
    let outcome = fixture.execute(&contract, &repo, &backend);
    let Terminal::BudgetExhausted { detail } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert!(detail.contains("limits.total_seconds"), "{detail}");
    assert!(detail.contains("min_repair_seconds"), "{detail}");
    assert_eq!(walls.lock().unwrap().len(), 1, "no repair worker ran");
    assert_eq!(dispatched_details(&fixture, &outcome).len(), 1);
    assert_eq!(
        fixture
            .ledger
            .dispatch_count(&outcome.run_id)
            .expect("count"),
        1
    );
    assert_eq!(
        fixture
            .ledger
            .attempt_count(&outcome.run_id)
            .expect("count"),
        1,
        "the refused repair left no attempt row"
    );
    let refusal = fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("transitions")
        .into_iter()
        .find(|t| t.reason == Reason::LimitReached.as_str())
        .and_then(|t| t.detail)
        .expect("the refusal is recorded");
    assert_eq!(refusal["limit"], "total");
    assert_eq!(refusal["cause"], "repair_floor_exceeds_total");
}

#[test]
fn a_floor_of_zero_leaves_the_repair_the_remainder() {
    let (fixture, skew) = skewed_fixture();
    let mut repo = floored_repo(&fixture);
    repo.execution.min_repair_seconds = 0;
    let (backend, walls) = skewing_worker(skew, vec![80, 0], 2);
    let outcome = fixture.execute(&fixture.contract(Review::Off), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(walls.lock().unwrap()[1], Duration::from_secs(20));
    assert!(dispatched_details(&fixture, &outcome)
        .iter()
        .all(|detail| extension_of(detail).is_none()));
}

#[test]
fn an_initial_attempt_is_never_extended() {
    let (fixture, skew) = skewed_fixture();
    let mut repo = floored_repo(&fixture);
    repo.execution.min_repair_seconds = 100;
    repo.execution.min_escalation_seconds = 100;
    let (backend, walls) = skewing_worker(skew, vec![0], 1);
    let outcome = fixture.execute(&fixture.contract(Review::Off), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(
        walls.lock().unwrap().clone(),
        vec![Duration::from_secs(100)]
    );
    let details = dispatched_details(&fixture, &outcome);
    assert_eq!(details.len(), 1);
    assert_eq!(extension_of(&details[0]), None);
}

#[test]
fn an_escalation_is_extended_to_its_own_floor() {
    let (fixture, skew) = skewed_fixture();
    let mut repo = floored_repo(&fixture);
    repo.execution.min_repair_seconds = 10;
    let (backend, walls) = skewing_worker(skew, vec![50, 30, 0], 3);
    let outcome = fixture.execute(&fixture.contract(Review::Off), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let details = dispatched_details(&fixture, &outcome);
    assert_eq!(details.len(), 3);
    assert_eq!(details[2]["kind"], "escalation");
    assert_eq!(
        extension_of(&details[1]),
        None,
        "50 s left cover the 10 s repair floor"
    );
    assert_eq!(extension_of(&details[2]), Some(50));
    let sentence = details[2]["detail"].as_str().expect("a detail sentence");
    assert!(sentence.contains("min_escalation_seconds"), "{sentence}");
    assert!(walls.lock().unwrap()[2] >= Duration::from_secs(70));
}

/// A gate that leaves the first model-work request queued once, then grants
/// it after the ledger clock has moved `jump_seconds`: the queue wait is
/// charged only when the grant lands, so it spends the admission budget
/// without the queue loop ending the run first.
struct LateGrantGate {
    inner: crate::admission::LocalGate,
    skew: Arc<std::sync::atomic::AtomicI64>,
    jump_seconds: i64,
    model_work_requests: std::sync::atomic::AtomicUsize,
}

impl Gate for LateGrantGate {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError> {
        self.inner.register_run(registration)
    }
    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError> {
        if request.resource == ResourceClass::ModelWork {
            match self.model_work_requests.fetch_add(1, Ordering::SeqCst) {
                0 => return Ok(Decision::Queued { position: 1 }),
                1 => {
                    self.skew.fetch_add(self.jump_seconds, Ordering::SeqCst);
                }
                _ => {}
            }
        }
        self.inner.admit(request)
    }
    fn bind(
        &self,
        dispatch_id: &str,
        agent_id: Option<&str>,
        pid: Option<u32>,
    ) -> Result<crate::admission::BindOutcome, GateError> {
        self.inner.bind(dispatch_id, agent_id, pid)
    }
    fn bind_group(
        &self,
        dispatch_id: &str,
        runner: u32,
        pgid: u32,
    ) -> Result<crate::admission::BindOutcome, GateError> {
        self.inner.bind_group(dispatch_id, runner, pgid)
    }
    fn heartbeat(&self, dispatch_id: &str) -> Result<crate::admission::HeartbeatStatus, GateError> {
        self.inner.heartbeat(dispatch_id)
    }
    fn mark_waiting(&self, dispatch_id: &str) -> Result<crate::admission::WaitOutcome, GateError> {
        self.inner.mark_waiting(dispatch_id)
    }
    fn resume(&self, dispatch_id: &str) -> Result<crate::admission::ResumeOutcome, GateError> {
        self.inner.resume(dispatch_id)
    }
    fn release(&self, dispatch_id: &str) -> Result<crate::admission::LifecycleOutcome, GateError> {
        self.inner.release(dispatch_id)
    }
    fn settle(
        &self,
        dispatch_id: &str,
        spent_micros: Option<i64>,
    ) -> Result<crate::admission::LifecycleOutcome, GateError> {
        self.inner.settle(dispatch_id, spent_micros)
    }
    fn withdraw(&self, dispatch_id: &str) -> Result<crate::admission::WithdrawOutcome, GateError> {
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
    fn enforcement(&self) -> crate::admission::Enforcement {
        self.inner.enforcement()
    }
}

#[test]
fn a_queued_worker_is_launched_with_no_more_than_the_total_has_left() {
    let (fixture, skew) = skewed_fixture();
    let repo = floored_repo(&fixture);
    let machine = fixture.machine_for(&repo);
    let gate = LateGrantGate {
        inner: crate::admission::LocalGate::new(
            ConcurrencyLimits::default(),
            crate::ledger::now_rfc3339,
        ),
        skew: Arc::clone(&skew),
        jump_seconds: 80,
        model_work_requests: std::sync::atomic::AtomicUsize::new(0),
    };
    let (backend, walls) = skewing_worker(skew, vec![0], 1);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.total_seconds = Some(150);
    let outcome = fixture.execute_managed(&contract, &repo, &machine, &backend, &gate);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let walls = walls.lock().unwrap().clone();
    assert_eq!(
        walls,
        vec![Duration::from_secs(70)],
        "the 80 s queue left 70 s of the 150 s total; the work budget alone says 100 s"
    );
}

/// The queue cut applies to workers only: a reviewer launched through the
/// gate with less of the total left than `REVIEW_MIN_WALL` still gets that
/// floor (review/engine.rs), as it did before the cut existed.
#[test]
fn a_gated_reviewer_keeps_its_own_floor_when_the_total_is_low() {
    let (fixture, skew) = skewed_fixture();
    let repo = floored_repo(&fixture);
    let machine = fixture.machine_for(&repo);
    let gate =
        crate::admission::LocalGate::new(ConcurrencyLimits::default(), crate::ledger::now_rfc3339);
    let review_walls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = Arc::clone(&review_walls);
    let backend = MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            record
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(spec.wall_timeout);
            return MockOutcome {
                result_text: Some("FINDINGS: none".into()),
                exit_code: Some(0),
                usage: Some(usage(100)),
                ..Default::default()
            };
        }
        // The worker fixes the tree and takes 90 s of the 150 s total.
        skew.fetch_add(90, Ordering::SeqCst);
        std::fs::remove_file(spec.work_dir.join("src/main.rs"))
            .expect("the worker removes the file its check fails on");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let mut contract = fixture.contract(Review::Required);
    contract.limits.total_seconds = Some(150);
    let outcome = fixture.execute_managed(&contract, &repo, &machine, &backend, &gate);
    let walls = review_walls.lock().unwrap().clone();
    assert!(!walls.is_empty(), "a reviewer was launched: {outcome:?}");
    assert!(
        walls
            .iter()
            .all(|wall| *wall >= super::super::review::REVIEW_MIN_WALL),
        "the reviewer keeps its floor although the total had under 60 s left: {walls:?}"
    );
}

#[test]
fn a_spent_admission_wait_blocks_the_repair_even_though_the_total_covers_its_floor() {
    // The first attempt queued 100 s (budget 60 s) and then worked 120 s
    // (budget 100 s): work and the admission wait are both spent, the total
    // (baseline + work + waits, thousands of seconds) covers the 60 s floor.
    // The floor must not turn the spent wait budget into a launch.
    // Work is spent while the first check runs (the marker file moves the
    // clock): a worker that spent it would leave the check no time, and a
    // timed-out check ends the run on the budget before any repair is asked.
    // Real time: only the queue loop's own poll sleep.
    let skew = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let marker = crate::test_support::temp_dir("floor-marker").join("check-ran");
    let fixture = Fixture::with_clock(Box::new(SkewClock {
        skew: Arc::clone(&skew),
        after_marker: Some((marker.clone(), 120)),
    }));
    let repo = floored_repo_checking(
        &fixture,
        CommandSpec {
            argv: vec![
                "sh".into(),
                "-c".into(),
                format!("touch '{}'; test ! -f src/main.rs", marker.display()),
            ],
            ..main_gone_check()
        },
    );
    let mut machine = fixture.machine_for(&repo);
    machine.resources.admission_wait_seconds = Some(60);
    let gate = LateGrantGate {
        inner: crate::admission::LocalGate::new(
            ConcurrencyLimits::default(),
            crate::ledger::now_rfc3339,
        ),
        skew: Arc::clone(&skew),
        jump_seconds: 100,
        model_work_requests: std::sync::atomic::AtomicUsize::new(0),
    };
    let (backend, walls) = skewing_worker(skew, vec![0], 99);
    let outcome = fixture.execute_managed(
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &backend,
        &gate,
    );
    assert!(
        matches!(
            &outcome.terminal,
            Terminal::BlockedBy {
                reason: Reason::AdmissionWaitExhausted,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(walls.lock().unwrap().len(), 1, "no repair worker ran");
    assert_eq!(dispatched_details(&fixture, &outcome).len(), 1);
}

#[test]
fn the_verification_of_an_extended_repair_is_clipped_to_what_the_budgets_have_left() {
    // Work 100 s: the first attempt takes 80, the repair is extended by 40
    // (work 140 s) and takes 50, so 10 s are left for its verification, which
    // the 30 s check is clipped to.
    let (fixture, skew) = skewed_fixture();
    let repo = floored_repo(&fixture);
    let (backend, _walls) = skewing_worker(skew, vec![80, 50], 2);
    let outcome = fixture.execute(&fixture.contract(Review::Off), &repo, &backend);
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    let receipt = serde_json::to_value(receipt).expect("receipt json");
    let check = &receipt["verification"]["checks"][0];
    assert_eq!(check["applied_limit_ms"], 10_000, "{check}");
    assert_eq!(check["clipped"], true, "{check}");
}

#[test]
fn a_short_wall_still_extends_a_repair_to_its_floor() {
    // The sibling of `wall_seconds_caps_the_work_and_nothing_else`: with the
    // floor on (default), `limits.wall_seconds = 2` caps the floor at 2 s; 1 s
    // is left after the first attempt, so the repair is extended by 1 s
    // instead of being refused.
    let (fixture, skew) = skewed_fixture();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.wall_seconds = Some(2);
    let (backend, walls) = skewing_worker(skew, vec![1, 0], 2);
    let outcome = fixture.execute(&contract, &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let details = dispatched_details(&fixture, &outcome);
    assert_eq!(extension_of(&details[1]), Some(1), "{details:?}");
    assert!(walls.lock().unwrap()[1] >= Duration::from_secs(1));
}

#[test]
fn total_seconds_caps_the_run_end_to_end() {
    // The same worker with a generous work budget and a one second total:
    // the run ends on the total, not on work.
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.total_seconds = Some(1);
    let outcome = fixture.execute(&contract, &repo, &slow_idle_worker(1_200));
    assert!(
        matches!(&outcome.terminal, Terminal::BudgetExhausted { detail } if detail.contains("total")),
        "{outcome:?}"
    );
}

#[test]
fn a_base_check_cut_off_by_the_total_ends_on_the_total() {
    // The check's own limit is far away; the one second total clips it.
    // The total ended it, so the run ends on the total, not as a base
    // that gave no verdict (#329). A check cut off by its own limit stays
    // `baseline_unrunnable`
    // (`a_baseline_check_the_wall_clock_kills_is_unrunnable_not_failed`).
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(
        vec![CommandSpec {
            argv: vec!["sh".into(), "-c".into(), "sleep 5".into()],
            ..passing_check()
        }],
        3,
    );
    let mut contract = fixture.contract(Review::Off);
    contract.limits.total_seconds = Some(1);
    let outcome = fixture.execute(&contract, &repo, &conditional_worker(""));
    assert!(
        matches!(&outcome.terminal, Terminal::BudgetExhausted { detail } if detail.contains("total")),
        "{outcome:?}"
    );
}

/// An engine for a run already on record and `running`, to end on a budget.
fn ended_on(fixture: &Fixture, exhausted: Exhausted) -> RunOutcome {
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let machine = fixture.machine_for(&repo);
    let contract = fixture.contract(Review::Off);
    let backend = conditional_worker("");
    let resolver = |_: &str, _: Option<&str>| AvalVerdict::Active {
        adr: "ADR-0001".into(),
        choice: None,
        reason: None,
    };
    let config = RunConfig {
        workspace: crate::workspace::reuse::Workspace::Isolated,
        repo_dir: &fixture.repo,
        contract: &contract,
        repo_policy: &repo,
        machine: &machine,
        ledger: &fixture.ledger,
        ids: &fixture.ids,
        backend: &backend,
        git: &crate::workspace::SystemGit,
        hooks: &crate::verify::FixedInventory(None),
        attest: &crate::verify::FixedAttest::default(),
        artifacts_dir: fixture.artifacts.clone(),
        aval_resolver: &resolver,
        predictor: None,
        gate: None,
        session_id: "test-session".into(),
        heartbeat_every: Duration::from_millis(50),
        task_override: None,
        purpose: None,
        run_id: None,
        wire: Wire::process(),
        design: DesignSource::Contract,
        declarations: &crate::trust::NoHistory,
    };
    let mut engine = RunEngine::new(&config, None).expect("engine");
    fixture
        .ledger
        .insert_run(
            &engine.run_id,
            "/repo",
            None,
            &crate::ids::TaskId::from_stored("t"),
            "rk",
        )
        .expect("insert");
    engine
        .transition(
            State::Running,
            Reason::WorkerDispatched,
            serde_json::json!({}),
        )
        .expect("running");
    let budget = Budget {
        attempts_used: 1,
        max_attempts: 3,
        repairs_used: 0,
        max_repairs: 1,
        review_repairs_used: 0,
        tier: Tier::Implementation,
        rung: RungIndex::INITIAL,
        escalation_tier: None,
    };
    engine
        .end_on_exhausted(exhausted, &budget, None)
        .expect("the run ends")
}

#[test]
fn each_exhausted_budget_ends_the_run_under_its_own_name() {
    let fixture = Fixture::new();

    let work = ended_on(&fixture, Exhausted::Work);
    assert_eq!(work.state(), State::BudgetExhausted);
    assert!(
        matches!(&work.terminal, Terminal::BudgetExhausted { detail } if detail.contains("work budget")),
        "{work:?}"
    );

    let total = ended_on(&fixture, Exhausted::Total);
    assert_eq!(total.state(), State::BudgetExhausted);
    assert!(
        matches!(&total.terminal, Terminal::BudgetExhausted { detail } if detail.contains("total")),
        "{total:?}"
    );

    let admission = ended_on(&fixture, Exhausted::AdmissionWait);
    assert_eq!(admission.state(), State::Blocked);
    assert!(
        matches!(
            &admission.terminal,
            Terminal::BlockedBy {
                reason: Reason::AdmissionWaitExhausted,
                ..
            }
        ),
        "{admission:?}"
    );
    assert_eq!(
        admission.terminal.named_reason().0.as_deref(),
        Some("admission_wait_exhausted")
    );

    let disk = ended_on(&fixture, Exhausted::DiskWait);
    assert_eq!(disk.state(), State::Blocked);
    assert!(
        matches!(
            &disk.terminal,
            Terminal::BlockedBy {
                reason: Reason::DiskLow,
                ..
            }
        ),
        "{disk:?}"
    );
    assert_eq!(disk.terminal.named_reason().0.as_deref(), Some("disk_low"));
}

#[test]
fn the_limits_of_a_run_come_from_the_repository_the_machine_and_the_contract() {
    let mut contract = TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "o",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "acceptance": ["a"],
            "verification_profile": "p",
        })
        .to_string(),
    )
    .expect("contract");
    let resources = crate::policy::ResourceSettings::default();
    let mut authority = crate::policy::EffectiveAuthority {
        models: BTreeMap::new(),
        max_attempts: 3,
        max_wall_seconds: 2700,
        min_repair_seconds: 600,
        min_escalation_seconds: 900,
        max_repairs_before_escalation: 1,
        allow_nested_agents: false,
        max_agent_depth: 1,
        max_agents_total: 24,
        loop_guard: crate::policy::LoopGuardMode::Shadow,
        verification_profile: VerificationProfile::default(),
        review_floor: Review::Off,
        disallowed_tools: Vec::new(),
        authority_hash: "hash".into(),
        grant_key: "key".into(),
        trust_status: crate::policy::TrustStatus::Repository,
        blockers: Vec::new(),
    };
    authority.verification_profile.commands = vec![main_gone_check()];
    let limits = budget_limits(&authority, &contract.limits, &resources);
    assert_eq!(limits.work, Duration::from_secs(2700));
    assert_eq!(limits.admission_wait, Duration::from_secs(1800));
    assert_eq!(limits.disk_wait, Duration::from_secs(600));
    assert_eq!(
        limits.total,
        Duration::from_secs(30 + 2700 + 1800 + 600),
        "the baseline's command timeouts, work and both waits"
    );
    contract.limits.total_seconds = Some(90);
    assert_eq!(
        budget_limits(&authority, &contract.limits, &resources).total,
        Duration::from_secs(90)
    );
}
