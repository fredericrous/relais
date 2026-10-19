// -- exhaustion is evidence, not a verdict (plan B4, SPEC §9, §10) ------------
//
// A failed check whose output or end shows a full disk or no memory ends the
// run for a person, never in a repair; on the base it blocks the run instead
// of calling the base red. relais's own writes meeting a full disk persist, in
// order, what they can. The disk is never really filled: a failpoint makes a
// write site fail with `StorageFull`, and a stub probe says what space reads.

use std::sync::atomic::AtomicUsize;

use super::*;
use crate::storage_full::failpoint::{arm, Armed, BallastProbe, Fire, FixedProbe, Script};
use crate::storage_full::Site;

const GIB: u64 = 1024 * 1024 * 1024;

/// Green while `src/main.rs` exists, so the base passes; once the worker
/// deletes it the check runs `failure`.
fn check_that_fails_with(failure: &str) -> CommandSpec {
    CommandSpec {
        argv: vec![
            "sh".into(),
            "-c".into(),
            format!("if [ -f src/main.rs ]; then exit 0; else {failure}; fi"),
        ],
        ..passing_check()
    }
}

/// A worker that deletes `src/main.rs` and counts its launches.
fn counting_worker(launched: &Arc<AtomicUsize>) -> MockBackend {
    let launched = Arc::clone(launched);
    MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            return MockOutcome {
                result_text: Some("review ok\nFINDINGS: none".into()),
                exit_code: Some(0),
                ..Default::default()
            };
        }
        launched.fetch_add(1, Ordering::SeqCst);
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("worker completes");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

/// Space reads as plenty, whatever the machine running the suite has.
fn plenty() -> Armed {
    arm(Script {
        probe: Some(Arc::new(FixedProbe(50 * GIB))),
        ..Script::default()
    })
}

struct Ran {
    fixture: Fixture,
    outcome: RunOutcome,
    launched: usize,
}

impl Ran {
    fn reasons(&self) -> Vec<String> {
        self.fixture
            .ledger
            .transitions(&self.outcome.run_id)
            .expect("transitions")
            .into_iter()
            .map(|t| t.reason)
            .collect()
    }

    fn candidate_ref_exists(&self) -> bool {
        let reference = workspace::candidate_ref(self.outcome.run_id.as_str(), 1);
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", &reference])
            .current_dir(&self.fixture.repo)
            .status()
            .expect("git")
            .success()
    }
}

fn run_with(check: CommandSpec, max_attempts: u32) -> Ran {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![check], max_attempts);
    let launched = Arc::new(AtomicUsize::new(0));
    let outcome = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &counting_worker(&launched),
    );
    let launched = launched.load(Ordering::SeqCst);
    Ran {
        fixture,
        outcome,
        launched,
    }
}

fn assert_resource_exhausted(ran: &Ran, resource: &str) {
    assert!(
        matches!(
            &ran.outcome.terminal,
            Terminal::NeedsDecision {
                reason: Reason::ResourceExhausted,
                ..
            }
        ),
        "{:?}",
        ran.outcome.terminal
    );
    assert_eq!(ran.launched, 1, "no repair worker was launched");
    let reasons = ran.reasons();
    assert!(
        !reasons
            .iter()
            .any(|r| r == "behavioral_failure" || r == "repair_exhausted"),
        "no repair was spent: {reasons:?}"
    );
    assert!(ran.candidate_ref_exists(), "the candidate is preserved");
    let transitions = ran
        .fixture
        .ledger
        .transitions(&ran.outcome.run_id)
        .expect("transitions");
    let ended = transitions
        .iter()
        .rev()
        .find(|t| t.reason == "resource_exhausted")
        .expect("the resource_exhausted transition");
    let detail = ended.detail.as_ref().expect("detail");
    assert_eq!(detail["resource"], resource, "{detail}");
    assert!(detail.get("floor").is_some() && detail.get("peak_rss").is_some());
}

#[test]
fn each_disk_signature_in_a_failed_candidate_check_ends_resource_exhausted() {
    let _space = plenty();
    for signature in [
        "No space left on device",
        "os error 28",
        "database or disk is full",
    ] {
        let ran = run_with(
            check_that_fails_with(&format!("echo '{signature}'; exit 1")),
            3,
        );
        assert_resource_exhausted(&ran, "disk");
        let detail = ran.outcome.terminal.detail().to_string();
        assert!(detail.contains(signature), "{detail}");
    }
}

#[test]
fn each_memory_signature_and_an_unsent_sigkill_end_resource_exhausted() {
    let _space = plenty();
    let mut failures = vec![
        "echo 'Cannot allocate memory'; exit 1",
        "echo 'thread panicked: os error 12'; exit 101",
    ];
    // Windows has no SIGKILL: Git Bash's `kill -9 $$` ends the shell with
    // neither a signal nor 137. `classify` is tested on 137 everywhere.
    if cfg!(unix) {
        failures.push("kill -9 $$");
    }
    for failure in failures {
        let ran = run_with(check_that_fails_with(failure), 3);
        assert_resource_exhausted(&ran, "memory");
    }
}

#[test]
fn the_decision_says_exhaustion_is_not_innocence_and_offers_a_verify_rerun() {
    let _space = plenty();
    let ran = run_with(
        check_that_fails_with("echo 'No space left on device'; exit 1"),
        3,
    );
    let detail = ran.outcome.terminal.detail();
    assert!(
        detail.contains("not proof that the code is innocent"),
        "{detail}"
    );
    assert!(detail.contains("kind: verify"), "{detail}");
    assert!(detail.contains("--rerun-verify"), "{detail}");

    // The offer is the existing successor path (plan A5): answering it
    // plans a `kind: verify` of this same candidate with the same scope.
    let run = &ran.outcome.run_id;
    crate::runner::successor::require_resource_exhausted(&ran.fixture.ledger, run)
        .expect("this decision is the one --rerun-verify answers");
    let (original, _tier) = ran
        .fixture
        .ledger
        .run_contract_and_tier(run)
        .expect("contract")
        .expect("recorded");
    let rerun = crate::runner::successor::widened_contract(&original, run, &[]).expect("rerun");
    assert!(rerun.is_verify());
    assert_eq!(
        rerun.source_candidate().map(|c| c.run.as_str().to_string()),
        Some(run.as_str().to_string())
    );
}

#[test]
fn a_rerun_is_refused_for_a_decision_that_is_not_resource_exhausted() {
    let _space = plenty();
    let ran = run_with(
        CommandSpec {
            argv: vec!["sh".into(), "-c".into(), "exit 1".into()],
            ..passing_check()
        },
        1,
    );
    assert!(crate::runner::successor::require_resource_exhausted(
        &ran.fixture.ledger,
        &ran.outcome.run_id
    )
    .is_err());
}

#[test]
fn a_timeout_relais_sent_is_not_memory() {
    let _space = plenty();
    let mut check = check_that_fails_with("echo 'Cannot allocate memory'; sleep 30");
    check.timeout_seconds = 1;
    let ran = run_with(check, 1);
    assert!(
        !matches!(
            &ran.outcome.terminal,
            Terminal::NeedsDecision {
                reason: Reason::ResourceExhausted,
                ..
            }
        ),
        "a check relais killed on its clock is a failure, not memory: {:?}",
        ran.outcome.terminal
    );
    assert!(!ran.reasons().iter().any(|r| r == "resource_exhausted"));
}

#[test]
fn a_passing_check_that_prints_a_signature_is_still_a_pass() {
    let _space = plenty();
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(
        vec![CommandSpec {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "echo 'No space left on device (handled)'; exit 0".into(),
            ],
            ..passing_check()
        }],
        3,
    );
    let outcome = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &conditional_worker("relais task"),
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{:?}",
        outcome.terminal
    );
}

#[test]
fn a_classified_failure_on_the_base_blocks_the_run_and_never_reddens_the_base() {
    let _space = plenty();
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(
        vec![CommandSpec {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "echo 'No space left on device'; exit 1".into(),
            ],
            ..passing_check()
        }],
        3,
    );
    let launched = Arc::new(AtomicUsize::new(0));
    let outcome = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &counting_worker(&launched),
    );
    assert_eq!(outcome.state(), State::Blocked);
    assert!(
        matches!(
            &outcome.terminal,
            Terminal::BlockedBy {
                reason: Reason::BaselineResourceExhausted,
                ..
            }
        ),
        "{:?}",
        outcome.terminal
    );
    assert_eq!(
        outcome.terminal.named_reason().0.as_deref(),
        Some("baseline_resource_exhausted")
    );
    assert!(outcome.terminal.detail().contains("not red"));
    assert_eq!(launched.load(Ordering::SeqCst), 0, "no worker was launched");
}

// -- relais's own storage (SPEC §10) -------------------------------------------

/// A completed run's inputs plus a ballast file, with the failpoint armed.
struct Fill {
    fixture: Fixture,
    ballast: PathBuf,
    _armed: Option<Armed>,
}

fn filling(script: impl FnOnce(&PathBuf) -> Script, with_ballast: bool) -> Fill {
    let fixture = Fixture::new();
    let ballast = fixture.dir.join(crate::storage_full::BALLAST_FILE);
    if with_ballast {
        std::fs::write(&ballast, vec![0u8; 4096]).expect("ballast");
    }
    let armed = arm(script(&ballast));
    Fill {
        fixture,
        ballast,
        _armed: Some(armed),
    }
}

fn script_failing(site: Site, after: Option<Site>, ballast: &Path) -> Script {
    Script {
        fires: vec![Fire { site, after }],
        probe: Some(Arc::new(BallastProbe {
            ballast: ballast.to_path_buf(),
            low: GIB,
            high: 50 * GIB,
            keeps_blocks: false,
        })),
        ..Script::default()
    }
}

fn run_to_the_end(fill: &Fill, run_id: Option<RunId>) -> Result<RunOutcome, RunError> {
    let fixture = &fill.fixture;
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let machine = fixture.machine_for(&repo);
    let contract = fixture.contract(Review::Off);
    let backend = conditional_worker("relais task");
    let resolver = |_: &str, _: Option<&str>| AvalVerdict::Active {
        adr: "ADR-0001".into(),
        choice: None,
        reason: None,
    };
    execute(&RunConfig {
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
        run_id,
        wire: Wire::process(),
        design: DesignSource::Contract,
        declarations: &crate::trust::NoHistory,
    })
}

fn candidate_ref_exists(fixture: &Fixture, run: &RunId) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &workspace::candidate_ref(run.as_str(), 1),
        ])
        .current_dir(&fixture.repo)
        .status()
        .expect("git")
        .success()
}

#[test]
fn storage_full_at_each_write_site_frees_the_ballast_and_records_it() {
    // Each site fails once, placed after the candidate's snapshot where the
    // site comes after it, so the candidate ref has something to name.
    for (site, after) in [
        (Site::Log, Some(Site::Ref)),
        (Site::Snapshot, None),
        (Site::Ref, None),
        (Site::Patch, None),
        (Site::Ledger, Some(Site::Ref)),
    ] {
        let fill = filling(|ballast| script_failing(site, after, ballast), true);
        let run = fill.fixture.ids.run_id().expect("run id");
        let outcome = run_to_the_end(&fill, Some(run.clone())).expect("persisted, so an outcome");
        assert!(
            matches!(
                &outcome.terminal,
                Terminal::BlockedBy {
                    reason: Reason::RelaisStorageFull,
                    ..
                }
            ),
            "{site:?}: {:?}",
            outcome.terminal
        );
        assert_eq!(outcome.state(), State::Blocked, "{site:?}");
        assert!(!fill.ballast.exists(), "{site:?}: the ballast was freed");
        let transitions = fill.fixture.ledger.transitions(&run).expect("transitions");
        assert!(
            transitions
                .iter()
                .any(|t| t.reason == "relais_storage_full"),
            "{site:?}: the ledger records it"
        );
        assert!(
            candidate_ref_exists(&fill.fixture, &run),
            "{site:?}: the candidate ref is present"
        );
        assert!(
            fill.fixture
                .artifacts
                .join(run.as_str())
                .join("follow-up.json")
                .is_file(),
            "{site:?}: the follow-up is written"
        );
    }
}

#[test]
fn storage_full_at_the_ledger_with_the_ballast_gone_exits_distinctly_and_resume_reconciles() {
    let fill = filling(
        |ballast| Script {
            ledger_stays_full: true,
            ..script_failing(Site::Ledger, Some(Site::Ref), ballast)
        },
        false,
    );
    let run = fill.fixture.ids.run_id().expect("run id");
    let result = run_to_the_end(&fill, Some(run.clone()));
    let Err(RunError::StorageFull { volume }) = result else {
        panic!("the run exits on its own error: {result:?}");
    };
    assert_eq!(volume, "/stub");
    drop(fill._armed);

    // The last durable ledger state is non-terminal, and `resume` reconciles
    // from it exactly as `relais resume` does: nothing live, so the run is
    // marked interrupted.
    let ledger = &fill.fixture.ledger;
    let state = ledger.run_status(&run).expect("status").expect("on record");
    assert!(!state.is_terminal(), "last durable state: {state}");
    let live: Vec<_> = ledger
        .live_dispatches()
        .expect("live")
        .into_iter()
        .filter(|live| live.run == run)
        .collect();
    assert!(crate::resume::reconcile(&live, None, &|_| false).may_reconcile());
    ledger
        .record_transition_as_driver(&Transition {
            run_id: run.clone(),
            attempt_id: None,
            from_state: Some(state),
            to_state: State::Interrupted,
            reason: Reason::ReconciledInterrupted.as_str().to_string(),
            detail: Some(serde_json::json!({ "detail": "resume" })),
            at: crate::ledger::now_rfc3339(),
        })
        .expect("resume writes once space is back");
    assert_eq!(
        ledger.run_status(&run).expect("status"),
        Some(State::Interrupted)
    );
    assert!(candidate_ref_exists(&fill.fixture, &run));
}

#[test]
fn a_ballast_that_frees_nothing_goes_straight_to_the_distinct_exit() {
    let fill = filling(
        |ballast| Script {
            probe: Some(Arc::new(BallastProbe {
                ballast: ballast.clone(),
                low: GIB,
                high: 50 * GIB,
                keeps_blocks: true,
            })),
            ..script_failing(Site::Patch, None, ballast)
        },
        true,
    );
    let run = fill.fixture.ids.run_id().expect("run id");
    let result = run_to_the_end(&fill, Some(run.clone()));
    assert!(
        matches!(result, Err(RunError::StorageFull { .. })),
        "{result:?}"
    );
    let transitions = fill.fixture.ledger.transitions(&run).expect("transitions");
    assert!(
        !transitions
            .iter()
            .any(|t| t.reason == "relais_storage_full"),
        "nothing was persisted past a ballast that freed no space"
    );
}
