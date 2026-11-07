// -- an admission wait that runs out around a kept candidate names it
// (docs/plans/2026-11-01-admission-queue-wastes-less.md, Phase 1) ---------

use std::sync::atomic::AtomicUsize;

use super::*;

/// When the scripted gate stops granting heavy commands.
#[derive(Clone, Copy)]
enum Heavy {
    /// Always granted.
    Free,
    /// From the first request: the baseline queues.
    Never,
    /// Once a worker or reviewer has been granted: verification queues.
    AfterModelWork,
    /// After this many heavy grants (the baseline's): the task tree's setup
    /// queues.
    AfterHeavyGrants(usize),
}

/// A gate that answers like a `LocalGate` until the scripted moment, then
/// leaves the request `Queued` for good.
struct QueueingGate {
    inner: crate::admission::LocalGate,
    heavy: Heavy,
    model_work_allowed: usize,
    model_work_granted: AtomicUsize,
    heavy_granted: AtomicUsize,
}

impl QueueingGate {
    fn new(heavy: Heavy, model_work_allowed: usize) -> Self {
        Self {
            inner: crate::admission::LocalGate::new(
                ConcurrencyLimits::default(),
                crate::ledger::now_rfc3339,
            ),
            heavy,
            model_work_allowed,
            model_work_granted: AtomicUsize::new(0),
            heavy_granted: AtomicUsize::new(0),
        }
    }
}

impl Gate for QueueingGate {
    fn register_run(&self, registration: &RunRegistration) -> Result<(), GateError> {
        self.inner.register_run(registration)
    }
    fn admit(&self, request: &DispatchRequest) -> Result<Decision, GateError> {
        let granted = self.model_work_granted.load(Ordering::SeqCst);
        match request.resource {
            ResourceClass::ModelWork => {
                if granted >= self.model_work_allowed {
                    return Ok(Decision::Queued { position: 1 });
                }
                self.model_work_granted.fetch_add(1, Ordering::SeqCst);
            }
            ResourceClass::HeavyLocal => {
                let queued = match self.heavy {
                    Heavy::Free => false,
                    Heavy::Never => true,
                    Heavy::AfterModelWork => granted > 0,
                    Heavy::AfterHeavyGrants(allowed) => {
                        self.heavy_granted.load(Ordering::SeqCst) >= allowed
                    }
                };
                if queued {
                    return Ok(Decision::Queued { position: 1 });
                }
                self.heavy_granted.fetch_add(1, Ordering::SeqCst);
            }
            ResourceClass::Indexing | ResourceClass::Training => {}
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

/// The detail of the transition that blocked the run on the admission
/// wait.
fn transition_detail(fixture: &Fixture, outcome: &RunOutcome) -> serde_json::Value {
    let transitions = fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("transitions");
    let blocking = transitions
        .iter()
        .find(|transition| transition.reason == "admission_wait_exhausted")
        .expect("the blocking transition");
    blocking.detail.clone().expect("a detail")
}

/// The run ended `blocked (admission_wait_exhausted)`; its finishing
/// transition's detail.
fn blocked_detail(fixture: &Fixture, outcome: &RunOutcome) -> serde_json::Value {
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
    transition_detail(fixture, outcome)
}

fn queued_machine(fixture: &Fixture, repo: &RepoPolicy) -> MachineSettings {
    let mut machine = fixture.machine_for(repo);
    machine.resources.admission_wait_seconds = Some(1);
    machine
}

fn attempt_rows(fixture: &Fixture, run: &RunId) -> Vec<(String, Option<String>)> {
    let raw = rusqlite::Connection::open(fixture.dir.join("ledger.sqlite")).expect("open");
    let mut statement = raw
        .prepare("SELECT state, candidate_sha FROM attempts WHERE run_id = ?1 ORDER BY id")
        .expect("prepare");
    statement
        .query_map([run.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

#[test]
fn an_expiry_during_an_attempts_verification_names_the_kept_candidate() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let machine = queued_machine(&fixture, &repo);
    let gate = QueueingGate::new(Heavy::AfterModelWork, usize::MAX);
    let outcome = fixture.execute_managed(
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &conditional_worker("relais task"),
        &gate,
    );
    let detail = blocked_detail(&fixture, &outcome);
    let rows = attempt_rows(&fixture, &outcome.run_id);
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (state, sha) = &rows[0];
    let sha = sha.as_deref().expect("the attempt keeps its candidate");
    assert_eq!(state, "blocked");
    assert_eq!(detail["candidate"]["run"], outcome.run_id.as_str());
    assert_eq!(detail["candidate"]["index"], 1);
    assert_eq!(detail["candidate"]["sha"], sha);
    assert!(
        detail["detail"]
            .as_str()
            .is_some_and(|text| text.contains("kind: verify")),
        "{detail}"
    );
}

#[test]
fn an_expiry_in_a_verify_runs_baseline_names_the_resolved_source_run() {
    let fixture = Fixture::new();
    let (source, _) = super::verify_kind::accepted_source(&fixture);
    std::fs::write(fixture.repo.join("other.txt"), "move the base on\n").expect("write");
    git(&fixture.repo, &["add", "-A"]);
    git(&fixture.repo, &["commit", "-q", "-m", "move the target on"]);
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let machine = queued_machine(&fixture, &repo);
    let gate = QueueingGate::new(Heavy::Never, usize::MAX);
    let mut contract = TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "verify",
            "objective": "Verify the candidate on the current base",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "candidate": { "run": source.as_str() },
            "acceptance": ["the profile's checks pass"],
            "verification_profile": "profile",
            "review": "off",
        })
        .to_string(),
    )
    .expect("a verify contract");
    contract.limits = Default::default();
    let outcome =
        fixture.execute_managed(&contract, &repo, &machine, &conditional_worker(""), &gate);
    let detail = blocked_detail(&fixture, &outcome);
    assert_ne!(outcome.run_id, source);
    assert_eq!(detail["candidate"]["run"], source.as_str());
    assert_eq!(detail["candidate"]["index"], 1);
}

#[test]
fn a_reviewers_admission_expiry_names_the_checked_candidate() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let machine = queued_machine(&fixture, &repo);
    // One model-work grant for the worker: the checks pass, and the
    // reviewer's request stays queued.
    let gate = QueueingGate::new(Heavy::Free, 1);
    let outcome = fixture.execute_managed(
        &fixture.contract(Review::Required),
        &repo,
        &machine,
        &conditional_worker("relais task"),
        &gate,
    );
    // A required review that cannot be dispatched ends `needs_review`
    // (review unavailable), and its text carries the recovery.
    match &outcome.terminal {
        Terminal::NeedsReview { detail } => assert!(
            detail.contains("the candidate is kept") && detail.contains("kind: verify"),
            "{detail}"
        ),
        other => panic!("expected needs_review, got {other:?}"),
    }
    let detail = transition_detail(&fixture, &outcome);
    let rows = attempt_rows(&fixture, &outcome.run_id);
    let sha = rows[0]
        .1
        .as_deref()
        .expect("the attempt keeps its candidate");
    assert_eq!(detail["candidate"]["run"], outcome.run_id.as_str());
    assert_eq!(detail["candidate"]["index"], 1);
    assert_eq!(detail["candidate"]["sha"], sha);
}

#[test]
fn an_expiry_with_no_candidate_under_verification_names_none() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let machine = queued_machine(&fixture, &repo);

    let baseline = fixture.execute_managed(
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &conditional_worker("relais task"),
        &QueueingGate::new(Heavy::Never, usize::MAX),
    );
    assert!(blocked_detail(&fixture, &baseline)
        .get("candidate")
        .is_none());

    let worker_queue = fixture.execute_managed(
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &conditional_worker("relais task"),
        &QueueingGate::new(Heavy::AfterModelWork, 0),
    );
    assert!(blocked_detail(&fixture, &worker_queue)
        .get("candidate")
        .is_none());
}

/// The heavy grants a baseline takes with one setup step: the step, then the checks.
const HEAVY_GRANTS_OF_A_BASELINE: usize = 2;

#[test]
fn an_expiry_while_the_task_trees_setup_queues_ends_blocked() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy_with_setup(vec![sh("true")], vec![main_gone_check()], 3);
    let machine = queued_machine(&fixture, &repo);
    let gate = QueueingGate::new(
        Heavy::AfterHeavyGrants(HEAVY_GRANTS_OF_A_BASELINE),
        usize::MAX,
    );
    let outcome = fixture.execute_managed(
        &fixture.contract(Review::Off),
        &repo,
        &machine,
        &conditional_worker("relais task"),
        &gate,
    );
    // `blocked_detail` pins `Terminal::BlockedBy`; `cli/run.rs` maps every
    // `BlockedBy` to `CliOutcome::Blocked`, which `cli/outcome.rs::exit_code`
    // maps to 3. The binary-only `cli` module is out of reach of this test.
    let detail = blocked_detail(&fixture, &outcome);
    // The baseline finished first: its check ran and left its log. A stop
    // during the baseline (`baseline.rs` handles that path already) ends
    // with the same reason and no attempt, and would leave no such log.
    let base_log = fixture
        .artifacts
        .join(outcome.run_id.as_str())
        .join("logs")
        .join("base-cmd0.log");
    assert!(
        base_log.exists(),
        "the baseline's check ran before the stop: {}",
        base_log.display()
    );
    // The stop came while the setup queued: no worker was dispatched, so no
    // attempt exists, and the detail names no candidate and says nothing was
    // launched. A stop during candidate verification would leave an attempt
    // row with a candidate and a detail naming it.
    let rows = attempt_rows(&fixture, &outcome.run_id);
    assert!(rows.is_empty(), "{rows:?}");
    assert!(detail.get("candidate").is_none(), "{detail}");
    assert!(
        detail["detail"]
            .as_str()
            .is_some_and(|text| text.ends_with("; nothing was launched")),
        "{detail}"
    );
}
