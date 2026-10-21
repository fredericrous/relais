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
        tier: Tier::Implementation,
        rung: RungIndex::INITIAL,
        escalation_tier: None,
    };
    engine
        .end_on_exhausted(exhausted, &budget)
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
