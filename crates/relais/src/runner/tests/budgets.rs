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
    let gate = crate::admission::LocalGate::new(ConcurrencyLimits {
        max_active_agents: Some(1),
        ..ConcurrencyLimits::default()
    });
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
    let gate = crate::admission::LocalGate::new(ConcurrencyLimits {
        max_heavy_commands: Some(1),
        ..ConcurrencyLimits::default()
    });
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

/// An engine for `repo`, not yet on record, handed to `act`.
fn with_engine_for<T>(
    fixture: &Fixture,
    repo: &RepoPolicy,
    act: impl FnOnce(&mut RunEngine<'_>) -> T,
) -> T {
    let machine = fixture.machine_for(repo);
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
        repo_policy: repo,
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
    act(&mut engine)
}

/// An engine for a run already on record and `running`, handed to `act`.
fn with_running_engine<T>(
    fixture: &Fixture,
    act: impl FnOnce(&mut RunEngine<'_>, &Budget) -> T,
) -> T {
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    with_engine_for(fixture, &repo, |engine| {
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
        act(engine, &budget)
    })
}

/// An engine for a run already on record and `running`, to end on a budget.
fn ended_on(fixture: &Fixture, exhausted: Exhausted) -> RunOutcome {
    with_running_engine(fixture, |engine, budget| {
        engine
            .end_on_exhausted(exhausted, budget)
            .expect("the run ends")
    })
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
        max_repairs_before_escalation: 1,
        allow_nested_agents: false,
        max_agent_depth: 1,
        max_agents_total: 24,
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

/// A check that runs until it is killed, with the limit it declares.
fn sleeping_check(timeout_seconds: u64) -> CommandSpec {
    CommandSpec {
        argv: vec!["sh".into(), "-c".into(), "sleep 30".into()],
        timeout_seconds,
        ..passing_check()
    }
}

fn blocked_code(outcome: &RunOutcome) -> BlockCode {
    match &outcome.terminal {
        Terminal::Blocked { code, .. } => *code,
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn a_baseline_check_cut_by_the_total_ends_on_the_total() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![sleeping_check(30)], 3);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.total_seconds = Some(1);
    let outcome = fixture.execute(&contract, &repo, &conditional_worker(""));
    assert!(
        matches!(&outcome.terminal, Terminal::BudgetExhausted { detail } if detail.contains("total")),
        "{outcome:?}"
    );

    // The base check was killed well before its declared 30 seconds: it
    // ran under the limit the total left it.
    let events = std::fs::read_to_string(
        fixture
            .artifacts
            .join(outcome.run_id.as_str())
            .join("events.jsonl"),
    )
    .expect("events");
    let base_check_ended = events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|line| line["event"].clone())
        .find(|event| event["kind"] == "check_ended")
        .unwrap_or_else(|| panic!("the base check ended: {events}"));
    assert!(base_check_ended["exit"].is_null(), "{base_check_ended}");
    assert!(
        base_check_ended["duration_ms"].as_u64().expect("duration") < 30_000,
        "{base_check_ended}"
    );

    // The same command under a total of a third of a second: the check is
    // killed by the clipped limit, and the outcome says what that was.
    let dir = std::env::temp_dir().join(format!("relais-budget-cut-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let admission = crate::verify::HeavyAdmission {
        budgets: Budgets::start(
            Arc::new(crate::policy::WallClock),
            BudgetLimits {
                work: Duration::from_secs(60),
                admission_wait: Duration::from_secs(60),
                disk_wait: Duration::from_secs(60),
                total: Duration::from_millis(300),
            },
        ),
        ..crate::verify::HeavyAdmission::unmanaged()
    };
    let check = crate::verify::run_observed_within(
        &dir,
        &sleeping_check(30),
        &dir.join("logs"),
        "c",
        "s",
        &Events::silent(),
        None,
        &admission,
    )
    .expect("runs");
    std::fs::remove_dir_all(&dir).expect("cleanup");
    assert_eq!(check.ended.recorded(), Some(crate::procs::Ended::TimedOut));
    let limit = check.limit.expect("the applied limit is recorded");
    assert!(limit.applied_ms < limit.declared_ms, "{limit:?}");
    assert_eq!(limit.declared_ms, 30_000);
}

#[test]
fn a_base_setup_cut_by_the_total_ends_on_the_total() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy_with_setup(vec![sleeping_check(30)], vec![passing_check()], 3);
    let mut contract = fixture.contract(Review::Off);
    contract.limits.total_seconds = Some(1);
    let outcome = fixture.execute(&contract, &repo, &conditional_worker(""));
    assert!(
        matches!(&outcome.terminal, Terminal::BudgetExhausted { detail } if detail.contains("total")),
        "{outcome:?}"
    );
}

#[test]
fn a_baseline_check_that_hits_its_own_limit_stays_unrunnable() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![sleeping_check(1)], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &conditional_worker(""),
    );
    assert_eq!(
        blocked_code(&outcome),
        BlockCode::BaselineUnrunnable,
        "{outcome:?}"
    );
}

#[test]
fn after_a_spent_admission_wait_a_check_on_its_own_limit_is_not_a_budget_cut() {
    let fixture = Fixture::new();
    let own_limit = |declared_ms: u64| crate::verify::CheckOutcome {
        label: "c".into(),
        argv: vec!["sleep".into(), "30".into()],
        ended: crate::verify::CheckEnd::new(crate::procs::Ended::TimedOut),
        log_path: String::new(),
        log_sha256: String::new(),
        junit: None,
        attempts: Vec::new(),
        flaky: false,
        limit: Some(crate::verify::AppliedLimit {
            applied_ms: declared_ms,
            declared_ms,
        }),
    };
    let spent_admission_wait = || {
        let budgets = Budgets::start(
            Arc::new(crate::policy::WallClock),
            BudgetLimits {
                work: Duration::from_secs(600),
                admission_wait: Duration::from_millis(1),
                disk_wait: Duration::from_secs(600),
                total: Duration::from_secs(600),
            },
        );
        let started = budgets.now();
        budgets.charge_wait(crate::policy::WaitKind::Admission, started - 5_000);
        assert_eq!(budgets.exhausted(), Some(Exhausted::AdmissionWait));
        budgets
    };

    // Baseline: the base check ran its own one second limit while the
    // admission wait was spent; the base is unrunnable, not a budget end.
    let repo = fixture.repo_policy(vec![sleeping_check(1)], 3);
    let outcome = with_engine_for(&fixture, &repo, |engine| {
        let Phase::Ready(preflight) = engine.preflight().expect("preflight") else {
            panic!("preflight ended the run");
        };
        engine.budgets = spent_admission_wait();
        match engine.baseline(&preflight).expect("baseline") {
            Phase::Ended(outcome) => outcome,
            Phase::Ready(_) => panic!("the baseline did not end the run"),
        }
    });
    assert_eq!(
        blocked_code(&outcome),
        BlockCode::BaselineUnrunnable,
        "{outcome:?}"
    );

    // Candidate: the same helper leaves the timeout to the existing judgement.
    with_running_engine(&fixture, |engine, _| {
        engine.budgets = spent_admission_wait();
        // Baseline and candidate share the helper: a check that ran its own
        // full limit is not a budget cut, whatever budget is spent.
        assert_eq!(engine.budget_cut(&own_limit(1_000)), None);
        let mut clipped = own_limit(1_000);
        clipped.limit = Some(crate::verify::AppliedLimit {
            applied_ms: 10,
            declared_ms: 1_000,
        });
        assert_eq!(engine.budget_cut(&clipped), Some(Exhausted::AdmissionWait));
        let mut unknown = own_limit(1_000);
        unknown.limit = None;
        assert_eq!(engine.budget_cut(&unknown), None);
    });
}
