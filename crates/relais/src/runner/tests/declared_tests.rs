// -- declared test evidence means the patch adds a test (SPEC §10) --------

use super::verify_kind::accepted_source;
use super::*;

const TEST_LINE: &str = "#[test]\nfn it_works() {}\n";

fn declared_test_contract(extra: serde_json::Value) -> TaskContract {
    let mut contract = serde_json::json!({
        "schema_version": 1,
        "kind": "change",
        "objective": "Remove the obsolete entry point",
        "base_ref": "HEAD",
        "write_scope": ["src/**"],
        "acceptance": [{
            "statement": "the removal is covered by a new test",
            "evidence": {"kind": "test", "authorship": "model_added"},
        }],
        "verification_profile": "profile",
        "review": "off",
    });
    for (key, value) in extra.as_object().expect("an object") {
        contract[key] = value.clone();
    }
    TaskContract::from_json_str(&contract.to_string()).expect("a declared-test contract")
}

/// A worker that removes `src/main.rs` and, when `test_file` is given,
/// writes that content to `src/tests.rs`.
fn removing_worker(
    launches: Arc<std::sync::atomic::AtomicUsize>,
    test_file: &'static str,
) -> MockBackend {
    MockBackend::new(move |spec| {
        launches.fetch_add(1, Ordering::SeqCst);
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).ok();
        std::fs::write(spec.work_dir.join("src/tests.rs"), test_file).expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

fn launch_counter() -> Arc<std::sync::atomic::AtomicUsize> {
    Arc::default()
}

#[test]
fn a_declared_test_criterion_is_met_when_the_patch_adds_a_test() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let backend = removing_worker(launch_counter(), TEST_LINE);
    let outcome = fixture.execute(
        &declared_test_contract(serde_json::json!({})),
        &repo,
        &backend,
    );
    let Terminal::Accepted(receipt) = outcome.terminal else {
        panic!("a patch that adds a test is accepted: {outcome:?}");
    };
    assert!(receipt.criteria.iter().all(|criterion| criterion.met));
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_declared_test_criterion_without_an_added_test_takes_one_repair_then_fails() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let launches = launch_counter();
    let backend = removing_worker(Arc::clone(&launches), "// no test here\n");
    let outcome = fixture.execute(
        &declared_test_contract(serde_json::json!({})),
        &repo,
        &backend,
    );
    let Terminal::Failed { detail, .. } = &outcome.terminal else {
        panic!("passing checks and no added test must not accept: {outcome:?}");
    };
    assert!(
        detail.contains("declared test evidence, but the patch adds no test"),
        "{detail}"
    );
    assert_eq!(
        launches.load(Ordering::SeqCst),
        2,
        "the first attempt and exactly one repair"
    );
    assert!(
        !fixture
            .artifacts
            .join(outcome.run_id.as_str())
            .join("receipt.json")
            .exists(),
        "no accepted receipt"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// A check that deletes the base commit's loose object, so the patch the
/// declared-test rule must read can no longer be derived.
fn base_destroying_check(fixture: &Fixture) -> CommandSpec {
    let head = git(&fixture.repo, &["rev-parse", "HEAD"]);
    let base = head.trim();
    let object = fixture
        .repo
        .join(".git/objects")
        .join(&base[..2])
        .join(&base[2..]);
    CommandSpec {
        argv: vec![
            "sh".into(),
            "-c".into(),
            format!(
                "test -f src/main.rs || {{ chmod u+w '{0}' 2>/dev/null; rm -f '{0}'; }}",
                object.display()
            ),
        ],
        ..passing_check()
    }
}

#[test]
fn an_unreadable_diff_on_a_worker_backed_change_is_an_error_never_met() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![base_destroying_check(&fixture)], 3);
    let backend = removing_worker(launch_counter(), TEST_LINE);
    let outcome = fixture.execute(
        &declared_test_contract(serde_json::json!({})),
        &repo,
        &backend,
    );
    let Terminal::Interrupted { detail } = &outcome.terminal else {
        panic!("an unreadable diff ends the run in an error: {outcome:?}");
    };
    assert!(
        detail.contains("the runner could not continue")
            && detail.contains("git failed: git -c diff.noprefix=false")
            && detail.contains("bad object"),
        "{detail}"
    );
    assert_eq!(
        fixture.ledger.receipt(&outcome.run_id).expect("reads"),
        None,
        "no receipt row: the criterion was never met"
    );
    assert!(
        !fixture
            .artifacts
            .join(outcome.run_id.as_str())
            .join("receipt.json")
            .exists(),
        "no accepted receipt"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// A declared-test run whose first attempt adds no test and whose one
/// repair does, settled as the arm of a trial through `live_trial::settle`.
#[test]
fn a_run_accepted_after_a_same_tier_repair_settles_accepted_without_escalation() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let launches = launch_counter();
    let seen = Arc::clone(&launches);
    let backend = MockBackend::new(move |spec| {
        let earlier = seen.fetch_add(1, Ordering::SeqCst);
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).ok();
        let content = if earlier == 0 {
            "// no test\n"
        } else {
            TEST_LINE
        };
        std::fs::write(spec.work_dir.join("src/tests.rs"), content).expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome = fixture.execute(
        &declared_test_contract(serde_json::json!({})),
        &repo,
        &backend,
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(launches.load(Ordering::SeqCst), 2, "one repair");
    assert!(!fixture
        .ledger
        .escalation_attempted(&outcome.run_id)
        .expect("reads"));

    let task = fixture
        .ledger
        .task_of_run(&outcome.run_id)
        .expect("reads")
        .expect("the run's task");
    let source = RunId::from_stored("run-source");
    fixture
        .ledger
        .insert_run(&source, "/repo", None, &task, "rk")
        .expect("source run");
    let trial_id = crate::ids::TrialId::from_stored("trial-repaired");
    fixture
        .ledger
        .insert_replay_trial(&crate::ledger::NewReplayTrial {
            trial_id: &trial_id,
            task_id: &task,
            source_run_id: &source,
            incumbent_recipe_id: "incumbent",
            arm_recipe_id: "arm",
            base_sha: "base",
            contract_hash: "contract",
            verification_profile_hash: "profile",
            workspace_isolation: "fresh_checkout_no_accepted_answer",
            arm_run_id: &outcome.run_id,
            design_sha256: None,
        })
        .expect("trial");
    crate::runner::live_trial::settle(&fixture.ledger, &trial_id, &outcome).expect("settles");
    let settled = fixture
        .ledger
        .trial(&trial_id)
        .expect("reads")
        .expect("row");
    assert_eq!(settled.accepted_without_escalation, Some(true));
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_verify_only_contract_with_declared_test_evidence_is_settled_as_before() {
    let fixture = Fixture::new();
    let (source, _) = accepted_source(&fixture);
    let launches = launch_counter();
    let backend = removing_worker(Arc::clone(&launches), "");
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let contract = TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "verify",
            "objective": "Verify the candidate on the current base",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "candidate": { "run": source.as_str() },
            "acceptance": [{
                "statement": "the candidate is tested",
                "evidence": {"kind": "test", "authorship": "model_added"},
            }],
            "verification_profile": "profile",
            "review": "off",
        })
        .to_string(),
    )
    .expect("a verify contract");
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(receipt),
    } = fixture.execute(&contract, &repo, &backend)
    else {
        panic!("a verify has no worker patch to hold to the rule");
    };
    assert!(receipt.criteria.iter().all(|criterion| criterion.met));
    assert_eq!(launches.load(Ordering::SeqCst), 0, "no worker");
    assert_eq!(fixture.ledger.dispatch_count(&run_id).expect("count"), 0);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// A parent run whose accepted candidate adds `src/parent_tests.rs` with a
/// test, then a `from_candidate` child that adds `src/child.rs`.
fn child_of_a_testing_parent(
    fixture: &Fixture,
    child_file: &'static str,
    after_parent: &dyn Fn(&Fixture),
) -> (RunOutcome, Arc<std::sync::atomic::AtomicUsize>) {
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let parent = MockBackend::new(|spec| {
        std::fs::write(spec.work_dir.join("src/parent_tests.rs"), TEST_LINE).expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let RunOutcome {
        run_id: source,
        terminal: Terminal::Accepted(_),
    } = fixture.execute(&fixture.contract(Review::Off), &repo, &parent)
    else {
        panic!("the parent is accepted");
    };
    after_parent(fixture);
    let launches = launch_counter();
    let seen = Arc::clone(&launches);
    let child = MockBackend::new(move |spec| {
        seen.fetch_add(1, Ordering::SeqCst);
        std::fs::write(spec.work_dir.join("src/child.rs"), child_file).expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let contract = declared_test_contract(serde_json::json!({
        "from_candidate": { "run": source.as_str() },
    }));
    (fixture.execute(&contract, &repo, &child), launches)
}

#[test]
fn a_follow_up_gets_no_credit_for_the_tests_its_parent_added() {
    let fixture = Fixture::new();
    let (outcome, _) = child_of_a_testing_parent(&fixture, "pub fn child() {}\n", &|_| {});
    assert!(
        matches!(outcome.terminal, Terminal::Failed { .. }),
        "the parent's tests are the lower bound, not the child's: {outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_follow_up_gets_no_credit_for_tests_the_target_base_gained_after_the_parent() {
    let fixture = Fixture::new();
    let (outcome, _) = child_of_a_testing_parent(&fixture, "pub fn child() {}\n", &|fixture| {
        super::scope_floor::commit_on_base(fixture, "src/base_tests.rs");
        std::fs::write(fixture.repo.join("src/base_tests.rs"), TEST_LINE).expect("write");
        git(&fixture.repo, &["add", "-A"]);
        git(
            &fixture.repo,
            &["commit", "-q", "-m", "the base gains a test"],
        );
    });
    let Terminal::Failed { detail } = &outcome.terminal else {
        panic!("inherited tests are not new tests: {outcome:?}");
    };
    assert!(
        detail.contains("declared test evidence, but the patch adds no test"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_follow_up_that_adds_its_own_test_is_accepted() {
    let fixture = Fixture::new();
    let (outcome, _) = child_of_a_testing_parent(&fixture, TEST_LINE, &|_| {});
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

fn decomposed_declared_test_contract(fixture: &Fixture) -> TaskContract {
    let _ = fixture;
    TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Add the a and b modules",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "acceptance": [{
                "statement": "the modules are covered by a new test",
                "evidence": {"kind": "test", "authorship": "model_added"},
            }],
            "verification_profile": "profile",
            "review": "off",
            "decomposition": plan_json(false),
        })
        .to_string(),
    )
    .expect("a decomposed contract")
}

/// The packages write modules; the integration repair writes
/// `repair_file` into `src/repair_tests.rs`.
fn decomposed_worker(
    launches: Arc<std::sync::atomic::AtomicUsize>,
    repair_file: &'static str,
) -> MockBackend {
    MockBackend::new(move |spec| {
        launches.fetch_add(1, Ordering::SeqCst);
        if spec.prompt.contains("bounded planner") {
            return MockOutcome {
                result_text: Some(
                    "here you go:\n{\"packages\":[],\"integration_acceptance\":[]}".into(),
                ),
                exit_code: Some(0),
                usage: Some(usage(7)),
                ..Default::default()
            };
        }
        let (dir, file) = if spec.prompt.contains("work package `a`") {
            ("src/a", "lib.rs")
        } else if spec.prompt.contains("work package `b`") {
            ("src/b", "lib.rs")
        } else {
            ("src", "repair_tests.rs")
        };
        std::fs::create_dir_all(spec.work_dir.join(dir)).expect("mkdir");
        let content = if file == "repair_tests.rs" {
            repair_file
        } else {
            "pub fn module() {}\n"
        };
        std::fs::write(spec.work_dir.join(dir).join(file), content).expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

#[test]
fn a_decomposed_run_with_no_added_test_stops_after_its_one_integration_repair() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let launches = launch_counter();
    let backend = decomposed_worker(Arc::clone(&launches), "// still no test\n");
    let outcome = fixture.execute(
        &decomposed_declared_test_contract(&fixture),
        &repo,
        &backend,
    );
    let Terminal::Failed { detail, .. } = &outcome.terminal else {
        panic!(
            "passing checks with no added test cannot seal an accepted root receipt: {outcome:?}"
        );
    };
    assert!(
        detail.contains("declared test evidence, but the patch adds no test"),
        "{detail}"
    );
    assert_eq!(
        launches.load(Ordering::SeqCst),
        3,
        "two packages and one integration repair"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_decomposed_integration_repair_that_adds_a_test_can_accept() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let backend = decomposed_worker(launch_counter(), TEST_LINE);
    let outcome = fixture.execute(
        &decomposed_declared_test_contract(&fixture),
        &repo,
        &backend,
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_declared_test_gap_after_a_spent_check_repair_fails_without_a_second_repair() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let launches = launch_counter();
    let counted = Arc::clone(&launches);
    let backend = MockBackend::new(move |spec| {
        let attempt = counted.fetch_add(1, Ordering::SeqCst);
        if attempt > 0 {
            std::fs::remove_file(spec.work_dir.join("src/main.rs")).ok();
        }
        std::fs::write(spec.work_dir.join("src/notes.rs"), "// no test here\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome = fixture.execute(
        &declared_test_contract(serde_json::json!({})),
        &repo,
        &backend,
    );
    let Terminal::Failed { detail, .. } = &outcome.terminal else {
        panic!("a spent repair budget leaves the missing test a failure: {outcome:?}");
    };
    assert!(
        detail.contains("declared test evidence, but the patch adds no test"),
        "{detail}"
    );
    assert_eq!(
        launches.load(Ordering::SeqCst),
        2,
        "the first attempt and the check repair, no second repair"
    );
    assert_eq!(
        fixture
            .ledger
            .dispatch_count(&outcome.run_id)
            .expect("count"),
        2
    );
    let transitions = fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("history");
    let first_repair = transitions
        .iter()
        .find(|transition| transition.to_state == State::Repairing)
        .expect("the failed check takes the one repair");
    assert_eq!(first_repair.reason, Reason::BehavioralFailure.as_str());
    let detail = first_repair.detail.as_ref().expect("repair detail");
    assert!(
        detail.get("failures").is_some() && detail.get("criteria").is_none(),
        "the repair answers the failed check, not the criteria: {detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}
