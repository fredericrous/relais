// -- kind: verify (docs/plans/2026-10-14-runs-survive-a-loaded-machine.md
// A1, A2, A3) -----------------------------------------------------------
//
// A source run produces a candidate; a second run, `kind: verify`, applies
// that candidate to the current base without a worker.

use std::sync::atomic::AtomicUsize;

use super::*;

/// The number of worker launches a backend saw.
fn launches() -> Arc<AtomicUsize> {
    Arc::new(AtomicUsize::new(0))
}

/// A source run that removes `src/main.rs`, accepted: its candidate is what
/// the tests verify. Returns its id and its receipt.
fn accepted_source(fixture: &Fixture) -> (RunId, Receipt) {
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let backend = conditional_worker("relais task");
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(receipt),
    } = fixture.execute(&fixture.contract(Review::Off), &repo, &backend)
    else {
        panic!("the source run is accepted");
    };
    (run_id, *receipt)
}

/// Move the repository's `HEAD` on: the verify run's target base differs
/// from the base the candidate was made against.
fn commit_file(fixture: &Fixture, path: &str, content: &str) {
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(fixture.repo.join(parent)).expect("mkdir");
    }
    std::fs::write(fixture.repo.join(path), content).expect("write");
    git(&fixture.repo, &["add", "-A"]);
    git(&fixture.repo, &["commit", "-q", "-m", "move the target on"]);
}

fn verify_contract(source: &RunId, index: Option<u32>) -> TaskContract {
    let mut candidate = serde_json::json!({ "run": source.as_str() });
    if let Some(index) = index {
        candidate["index"] = serde_json::json!(index);
    }
    TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "verify",
            "objective": "Verify the candidate on the current base",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "candidate": candidate,
            "acceptance": ["the profile's checks pass"],
            "verification_profile": "profile",
            "review": "off",
        })
        .to_string(),
    )
    .expect("a verify contract")
}

fn repo_key_of(fixture: &Fixture) -> String {
    repo_key(&crate::repo::identity(&fixture.repo))
}

/// A run row of the fixture's repository (or another's), left in the state
/// the test chooses.
fn recorded_run(fixture: &Fixture, repo_key: &str, end: Option<State>) -> RunId {
    let run = fixture.ids.run_id().expect("a run id");
    let task = derive_task_id(repo_key, run.as_str());
    fixture
        .ledger
        .insert_typed_run(
            &run,
            &fixture.repo.to_string_lossy(),
            None,
            &task,
            repo_key,
            None,
        )
        .expect("run row");
    if let Some(end) = end {
        let moved = fixture
            .ledger
            .transition_if(
                State::Prepared,
                &Transition {
                    run_id: run.clone(),
                    attempt_id: None,
                    from_state: Some(State::Prepared),
                    to_state: end,
                    reason: Reason::BlockedPreflight.as_str().into(),
                    detail: None,
                    at: fixture.ledger.now(),
                },
            )
            .expect("transition");
        assert!(moved);
    }
    run
}

fn blocked_by(outcome: RunOutcome) -> (RunId, Reason, String) {
    match outcome {
        RunOutcome {
            run_id,
            terminal: Terminal::BlockedBy { reason, detail },
        } => (run_id, reason, detail),
        other => panic!("expected a candidate block, got {other:?}"),
    }
}

#[test]
fn a_clean_verify_dispatches_no_worker_and_persists_the_resolved_candidate() {
    let fixture = Fixture::new();
    let (source, source_receipt) = accepted_source(&fixture);
    let source_base = source_receipt.base_sha.clone();
    commit_file(&fixture, "README.md", "unrelated\n");
    let target_base = git(&fixture.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    assert_ne!(source_base, target_base);

    let seen = launches();
    let backend = counting_worker(Arc::clone(&seen));
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(receipt),
    } = fixture.execute(&verify_contract(&source, None), &repo, &backend)
    else {
        panic!("a clean candidate whose checks pass is accepted");
    };
    assert_eq!(seen.load(Ordering::SeqCst), 0, "no worker was dispatched");
    assert_eq!(fixture.ledger.dispatch_count(&run_id).expect("count"), 0);
    assert_eq!(receipt.attempts, 1);

    // The six fields, resolved once.
    let patch = std::fs::read(
        fixture
            .artifacts
            .join(source.as_str())
            .join("candidate-1.patch"),
    )
    .expect("the source's patch");
    let resolved = receipt.candidate.clone().expect("the receipt carries it");
    assert_eq!(resolved.source_run, source.as_str());
    assert_eq!(resolved.index, 1);
    assert_eq!(resolved.candidate_commit, source_receipt.candidate_sha);
    assert_eq!(resolved.patch_sha256, crate::ids::sha256_hex(&patch));
    assert_eq!(resolved.source_base_sha, source_base);
    assert_eq!(resolved.target_base_sha, target_base);
    // ... persisted in `candidate_inputs` ...
    assert_eq!(
        fixture.ledger.candidate_input(&run_id).expect("read"),
        Some(resolved.clone())
    );
    // ... and on the receipt file.
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.artifacts.join(run_id.as_str()).join("receipt.json"))
            .expect("receipt.json"),
    )
    .expect("json");
    assert_eq!(written["candidate"]["source_run"], source.as_str());
    assert_eq!(written["candidate"]["patch_sha256"], resolved.patch_sha256);

    // The conflict test's event-log assertion is not vacuous: a run that
    // reaches the baseline does write this step.
    let events =
        std::fs::read_to_string(fixture.artifacts.join(run_id.as_str()).join("events.jsonl"))
            .expect("events");
    assert!(events.contains("\"name\":\"baseline\""), "{events}");

    // The candidate really is applied: the merge result has the source's
    // deletion and the target's new file.
    let tree = git(
        &fixture.repo,
        &["ls-tree", "-r", "--name-only", &receipt.candidate_sha],
    );
    assert!(tree.contains("README.md"), "{tree}");
    assert!(!tree.contains("src/main.rs"), "{tree}");

    // `report` counts it as its own kind.
    let report = crate::report::runs_report(&fixture.ledger, "1970-01-01", None).expect("report");
    assert_eq!(report.runs.iter().filter(|run| run.verify).count(), 1);
    assert!(report.render().contains("verify runs: 1 of 2"));
    assert!(report
        .runs
        .iter()
        .any(|run| run.run_id == run_id.as_str() && run.verify));
    assert!(report
        .runs
        .iter()
        .any(|run| run.run_id == source.as_str() && !run.verify));
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_verify_whose_check_fails_ends_failed_without_a_repair() {
    let fixture = Fixture::new();
    let (source, _) = accepted_source(&fixture);
    commit_file(&fixture, "README.md", "unrelated\n");

    let seen = launches();
    let backend = counting_worker(Arc::clone(&seen));
    // The candidate removes src/main.rs; this check wants it.
    let repo = fixture.repo_policy(vec![sh("test -f src/main.rs")], 3);
    let outcome = fixture.execute(&verify_contract(&source, Some(1)), &repo, &backend);
    let RunOutcome {
        run_id,
        terminal: Terminal::Failed { detail },
    } = outcome
    else {
        panic!("a failed check ends the run failed, got {outcome:?}");
    };
    assert!(detail.contains("no worker was dispatched"), "{detail}");
    assert_eq!(seen.load(Ordering::SeqCst), 0, "no repair, no escalation");
    assert_eq!(
        fixture.ledger.attempt_count(&run_id).expect("attempts"),
        1,
        "one verified attempt and nothing after it"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_conflicting_candidate_is_blocked_before_the_baseline() {
    let fixture = Fixture::new();
    let (source, _) = accepted_source(&fixture);
    // The target changed the file the candidate deleted.
    commit_file(
        &fixture,
        "src/main.rs",
        "fn main() { println!(\"moved\"); }\n",
    );

    let marker = fixture.dir.join("baseline-ran");
    let seen = launches();
    let backend = counting_worker(Arc::clone(&seen));
    let repo = fixture.repo_policy(vec![sh(&format!("touch {}", marker.display()))], 3);
    let (run_id, reason, detail) =
        blocked_by(fixture.execute(&verify_contract(&source, None), &repo, &backend));
    assert_eq!(reason, Reason::CandidateDoesNotApply);
    assert!(detail.contains("src/main.rs"), "names the files: {detail}");
    assert!(!marker.exists(), "no baseline command ran");
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    let events =
        std::fs::read_to_string(fixture.artifacts.join(run_id.as_str()).join("events.jsonl"))
            .expect("events");
    assert!(
        !events.contains("\"name\":\"baseline\""),
        "the run never reached the baseline: {events}"
    );
    assert_eq!(
        fixture.ledger.run_status(&run_id).expect("status"),
        Some(State::Blocked)
    );
    // The resolved candidate is on record even though the run ended.
    assert!(fixture
        .ledger
        .candidate_input(&run_id)
        .expect("read")
        .is_some());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_patch_changed_after_validation_ends_candidate_digest_mismatch() {
    let fixture = Fixture::new();
    let (source, _) = accepted_source(&fixture);
    commit_file(&fixture, "README.md", "unrelated\n");
    let latest = fixture
        .artifacts
        .join(source.as_str())
        .join("candidate-latest.patch");
    // The baseline runs after validation and before the candidate is
    // applied: it is where the file changes.
    let repo = fixture.repo_policy(
        vec![sh(&format!("echo tampered >> {}", latest.display()))],
        3,
    );
    let seen = launches();
    let backend = counting_worker(Arc::clone(&seen));
    let (run_id, reason, detail) =
        blocked_by(fixture.execute(&verify_contract(&source, None), &repo, &backend));
    assert_eq!(reason, Reason::CandidateDigestMismatch, "{detail}");
    assert!(detail.contains("candidate-latest.patch"), "{detail}");
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.ledger.run_status(&run_id).expect("status"),
        Some(State::Blocked)
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_source_that_has_not_ended_is_refused() {
    let fixture = Fixture::new();
    let running = recorded_run(&fixture, &repo_key_of(&fixture), None);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let backend = counting_worker(launches());
    let (_, reason, detail) =
        blocked_by(fixture.execute(&verify_contract(&running, None), &repo, &backend));
    assert_eq!(reason, Reason::CandidateSourceRunning, "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_source_without_a_candidate_or_without_its_commit_is_missing() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let backend = counting_worker(launches());

    // A run that ended before it produced anything.
    let empty = recorded_run(&fixture, &repo_key_of(&fixture), Some(State::Blocked));
    let (_, reason, detail) =
        blocked_by(fixture.execute(&verify_contract(&empty, None), &repo, &backend));
    assert_eq!(reason, Reason::CandidateMissing, "{detail}");
    assert!(detail.contains("no candidate"), "{detail}");

    // A run nobody recorded.
    let (_, reason, _) = blocked_by(fixture.execute(
        &verify_contract(&RunId::from_stored("run-never-recorded"), None),
        &repo,
        &backend,
    ));
    assert_eq!(reason, Reason::CandidateMissing);

    // A run whose commit object is gone from the repository.
    let (source, receipt) = accepted_source(&fixture);
    git(
        &fixture.repo,
        &[
            "update-ref",
            "-d",
            &workspace::candidate_ref(source.as_str(), 1),
        ],
    );
    git(
        &fixture.repo,
        &["reflog", "expire", "--expire=now", "--all"],
    );
    git(&fixture.repo, &["gc", "--prune=now", "-q"]);
    assert!(!workspace::commit_exists(
        &fixture.repo,
        &receipt.candidate_sha
    ));
    let (_, reason, detail) =
        blocked_by(fixture.execute(&verify_contract(&source, None), &repo, &backend));
    assert_eq!(reason, Reason::CandidateMissing, "{detail}");
    assert!(detail.contains(&receipt.candidate_sha), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_source_from_another_repository_is_refused() {
    let fixture = Fixture::new();
    let foreign = recorded_run(&fixture, "another-repository-key", Some(State::Accepted));
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let backend = counting_worker(launches());
    let (_, reason, detail) =
        blocked_by(fixture.execute(&verify_contract(&foreign, None), &repo, &backend));
    assert_eq!(reason, Reason::CandidateForeignRepo, "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// Resolution rewrites a ref that was lost, from the commit the ledger
/// recorded: the ref is a convenience, the ledger is the identity.
#[test]
fn resolution_applies_the_recorded_commit_and_rewrites_a_lost_ref() {
    let fixture = Fixture::new();
    let (source, receipt) = accepted_source(&fixture);
    let name = workspace::candidate_ref(source.as_str(), 1);
    git(&fixture.repo, &["update-ref", "-d", &name]);
    commit_file(&fixture, "README.md", "unrelated\n");
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let backend = counting_worker(launches());
    let outcome = fixture.execute(&verify_contract(&source, None), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(
        git(&fixture.repo, &["rev-parse", &name]).trim(),
        receipt.candidate_sha,
        "the ref names the recorded commit again"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// While a run that has not ended references a candidate, a reap leaves its
/// ref and patch file alone; once that run ends they are released.
#[test]
fn a_pinned_candidate_survives_release_while_its_consumer_is_running() {
    let fixture = Fixture::new();
    let (source, receipt) = accepted_source(&fixture);
    let consumer = recorded_run(&fixture, &repo_key_of(&fixture), None);
    fixture
        .ledger
        .record_candidate_input(
            &consumer,
            &crate::contract::ResolvedCandidate {
                source_run: source.as_str().to_string(),
                index: 1,
                candidate_commit: receipt.candidate_sha.clone(),
                patch_sha256: "0".repeat(64),
                source_base_sha: receipt.base_sha.clone(),
                target_base_sha: receipt.base_sha.clone(),
            },
        )
        .expect("pin");
    let source_dir = fixture.artifacts.join(source.as_str());
    let name = workspace::candidate_ref(source.as_str(), 1);

    let kept =
        crate::reap::release_candidates(&fixture.ledger, &fixture.repo, &source, &source_dir)
            .expect("release");
    assert!(
        kept.refs_removed.is_empty() && kept.patches_removed.is_empty(),
        "{kept:?}"
    );
    assert!(kept.kept.contains(&name), "{kept:?}");
    assert!(git(&fixture.repo, &["rev-parse", "--verify", &name]).trim() == receipt.candidate_sha);
    assert!(source_dir.join("candidate-1.patch").is_file());
    assert!(source_dir.join("candidate-latest.patch").is_file());

    // The consumer ends: nothing holds the candidate any more.
    let moved = fixture
        .ledger
        .transition_if(
            State::Prepared,
            &Transition {
                run_id: consumer.clone(),
                attempt_id: None,
                from_state: Some(State::Prepared),
                to_state: State::Failed,
                reason: Reason::BehavioralFailure.as_str().into(),
                detail: None,
                at: fixture.ledger.now(),
            },
        )
        .expect("end the consumer");
    assert!(moved);
    let released =
        crate::reap::release_candidates(&fixture.ledger, &fixture.repo, &source, &source_dir)
            .expect("release");
    assert_eq!(released.refs_removed, vec![name.clone()]);
    assert!(!source_dir.join("candidate-1.patch").exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}
