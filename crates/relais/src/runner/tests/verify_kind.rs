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

// -- from_candidate on kind: change (A4) ---------------------------------

/// The contract of a change that continues from `source`'s candidate.
fn continue_contract(source: &RunId) -> TaskContract {
    TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Carry on from the earlier candidate",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "from_candidate": { "run": source.as_str() },
            "acceptance": ["the profile's checks pass"],
            "verification_profile": "profile",
            "review": "off",
        })
        .to_string(),
    )
    .expect("a from_candidate contract")
}

/// A source run whose candidate rewrites `src/main.rs`, accepted.
fn rewriting_source(fixture: &Fixture) -> RunId {
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let backend = MockBackend::new(|spec| {
        std::fs::write(
            spec.work_dir.join("src/main.rs"),
            "fn main() { /* candidate */ }\n",
        )
        .expect("the worker rewrites the file");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(_),
    } = fixture.execute(&fixture.contract(Review::Off), &repo, &backend)
    else {
        panic!("the source run is accepted");
    };
    run_id
}

#[test]
fn a_conflicting_from_candidate_hands_the_merge_to_the_worker_and_refuses_the_snapshot_until_resolved(
) {
    let fixture = Fixture::new();
    let source = rewriting_source(&fixture);
    commit_file(
        &fixture,
        "src/main.rs",
        "fn main() { println!(\"target\"); }\n",
    );
    let target_base = git(&fixture.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    // What each launch saw in its tree and its prompt.
    let seen: Arc<std::sync::Mutex<Vec<(String, String, String)>>> = Arc::default();
    let record = Arc::clone(&seen);
    let backend = MockBackend::new(move |spec| {
        let unmerged = git(&spec.work_dir, &["ls-files", "-u", "--", "src/main.rs"]);
        let file = std::fs::read_to_string(spec.work_dir.join("src/main.rs")).unwrap_or_default();
        let mut record = record.lock().expect("lock");
        record.push((spec.prompt.clone(), unmerged, file));
        // The first launch leaves the conflict alone; the second resolves.
        if record.len() == 2 {
            std::fs::write(
                spec.work_dir.join("src/main.rs"),
                "fn main() { /* both */ }\n",
            )
            .expect("resolve");
            git(&spec.work_dir, &["add", "src/main.rs"]);
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(receipt),
    } = fixture.execute(&continue_contract(&source), &repo, &backend)
    else {
        panic!("the resolved candidate is accepted");
    };

    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 2, "one attempt refused, one repair");
    let (prompt, unmerged, file) = &seen[0];
    assert!(
        unmerged.contains("src/main.rs") && unmerged.lines().count() >= 2,
        "unmerged stages in the index: {unmerged}"
    );
    assert!(file.contains("<<<<<<<"), "markers in the file: {file}");
    assert!(prompt.contains("conflicts: [{"), "{prompt}");
    assert!(prompt.contains("\"path\":\"src/main.rs\""), "{prompt}");
    assert!(prompt.contains("\"ours\":\"target_base\""), "{prompt}");
    assert!(prompt.contains("\"theirs\":\"candidate\""), "{prompt}");
    assert!(prompt.contains("resolve these first"), "{prompt}");
    assert!(
        seen[1].0.contains("unresolved_conflict: src/main.rs"),
        "the repair is told why the first attempt was refused: {}",
        seen[1].0
    );
    assert!(seen[1].0.contains("resolve these first"));

    // The refused attempt is a failed attempt on record.
    let transitions = fixture.ledger.transitions(&run_id).expect("transitions");
    assert!(
        transitions.iter().any(|t| t
            .detail
            .as_ref()
            .is_some_and(|d| d.to_string().contains("unresolved_conflict: src/main.rs"))),
        "{transitions:?}"
    );
    // Patch and scope stay relative to the target base.
    assert_eq!(receipt.base_sha, target_base);
    let content = git(
        &fixture.repo,
        &["show", &format!("{}:src/main.rs", receipt.candidate_sha)],
    );
    assert_eq!(content, "fn main() { /* both */ }\n");
    assert_eq!(
        receipt.candidate.as_ref().map(|c| c.source_run.as_str()),
        Some(source.as_str())
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_clean_from_candidate_starts_with_the_candidate_applied_and_no_conflict_text() {
    let fixture = Fixture::new();
    let source = rewriting_source(&fixture);
    commit_file(&fixture, "README.md", "unrelated\n");
    let seen: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::default();
    let record = Arc::clone(&seen);
    let backend = MockBackend::new(move |spec| {
        let file = std::fs::read_to_string(spec.work_dir.join("src/main.rs")).unwrap_or_default();
        record
            .lock()
            .expect("lock")
            .push((spec.prompt.clone(), file));
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let outcome = fixture.execute(&continue_contract(&source), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let seen = seen.lock().expect("lock");
    assert_eq!(seen[0].1, "fn main() { /* candidate */ }\n");
    assert!(!seen[0].0.contains("resolve these first"), "{}", seen[0].0);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

// -- successor intent: widen scope (A5) -----------------------------------

#[test]
fn a_widened_scope_verify_names_the_source_run_and_its_receipt_carries_the_delta() {
    use crate::ledger::DecisionAnswer;
    use crate::runner::successor;

    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    // The worker leaves the scope the contract declares (`src/**`).
    let backend = MockBackend::new(|spec| {
        std::fs::create_dir_all(spec.work_dir.join("docs")).expect("mkdir");
        std::fs::write(spec.work_dir.join("docs/extra.md"), "one\ntwo\n").expect("write");
        std::fs::write(
            spec.work_dir.join("src/main.rs"),
            "fn main() {}\n// edited\n",
        )
        .expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let RunOutcome {
        run_id: decided,
        terminal: Terminal::NeedsDecision { reason, detail },
    } = fixture.execute(&fixture.contract(Review::Off), &repo, &backend)
    else {
        panic!("a write outside the scope waits on a person");
    };
    assert_eq!(reason, Reason::ScopeExceeded);
    // The decision text lists the out-of-scope path with +/- counts.
    assert!(detail.contains("docs/extra.md (+2/-0)"), "{detail}");

    // The recorded paths are the bare out-of-scope ones.
    let paths = successor::out_of_scope_paths(&fixture.ledger, &decided).expect("paths");
    assert_eq!(paths, vec!["docs/extra.md".to_string()]);

    // The widened contract: same contract, kind verify, this run's
    // candidate, scope plus exactly those paths.
    let (original, _) = fixture
        .ledger
        .run_contract_and_tier(&decided)
        .expect("read")
        .expect("recorded");
    let widened = successor::widened_contract(&original, &decided, &paths).expect("widened");
    assert!(widened.is_verify());
    assert_eq!(
        widened.candidate().map(|c| (c.run.as_str(), c.index)),
        Some((decided.as_str(), None))
    );
    assert_eq!(
        widened.scope_patterns(),
        vec!["src/**".to_string(), "docs/extra.md".to_string()]
    );
    assert_eq!(widened.objective, original.objective);
    assert_eq!(widened.acceptance, original.acceptance);

    // Answering writes the decision and the intent together.
    let intent = successor::plan(
        &fixture.ledger,
        &decided,
        &widened,
        paths.clone(),
        &fixture.artifacts,
    )
    .expect("planned");
    assert_eq!(
        intent.candidate.as_ref().map(|c| c.source_run.as_str()),
        Some(decided.as_str()),
        "the candidate is resolved and pinned with the answer"
    );
    let stored = fixture
        .ledger
        .resolve_decision_with_intent(
            &decided,
            &DecisionAnswer {
                resolution: Reason::DecisionRevised,
                actor: crate::ledger::Actor::Orchestrator,
                person_quote: None,
                recorded_via: crate::ledger::RecordedVia::Cli,
                note: None,
                successor_run: None,
                from_state: State::NeedsDecision,
                to_state: State::Failed,
            },
            &intent,
        )
        .expect("written")
        .expect("open decision");

    // The plugin's side: claim for a run id, run the intent's contract.
    let new_run = fixture.ids.run_id().expect("a run id");
    assert!(matches!(
        fixture
            .ledger
            .claim_intent(&stored.intent_id, &new_run, &|_| {
                crate::ledger::SuccessorProcess::Alive { started: None }
            })
            .expect("claim"),
        crate::ledger::Claim::Claimed(_)
    ));
    let contract = TaskContract::from_json_str(&stored.contract_json).expect("the stored contract");
    let seen = launches();
    let counting = counting_worker(Arc::clone(&seen));
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(receipt),
    } = fixture.execute_as(Some(new_run.clone()), &contract, &repo, &counting)
    else {
        panic!("the widened verify is accepted");
    };
    assert_eq!(run_id, new_run);
    assert_eq!(
        seen.load(Ordering::SeqCst),
        0,
        "a verify dispatches no worker"
    );
    let predecessor = receipt
        .predecessor
        .expect("the receipt names its predecessor");
    assert_eq!(predecessor.run, decided.as_str());
    assert_eq!(predecessor.intent_id, stored.intent_id);
    assert_eq!(predecessor.scope_delta, vec!["docs/extra.md".to_string()]);
    assert_eq!(
        receipt.candidate.map(|c| c.source_run),
        Some(decided.as_str().to_string())
    );
    let written: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture.artifacts.join(run_id.as_str()).join("receipt.json"))
            .expect("receipt.json"),
    )
    .expect("json");
    assert_eq!(written["predecessor"]["scope_delta"][0], "docs/extra.md");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_stored_row_resolved_against_another_base_is_refused_in_preflight() {
    let fixture = Fixture::new();
    let (source, _) = accepted_source(&fixture);
    commit_file(&fixture, "README.md", "unrelated\n");
    let base = git(&fixture.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let run = fixture.ids.run_id().expect("a run id");
    let stale = "0".repeat(40);
    fixture
        .ledger
        .record_candidate_input(
            &run,
            &crate::contract::ResolvedCandidate {
                source_run: source.as_str().to_string(),
                index: 1,
                candidate_commit: "1".repeat(40),
                patch_sha256: "2".repeat(64),
                source_base_sha: "3".repeat(40),
                target_base_sha: stale.clone(),
            },
        )
        .expect("record");
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let seen = launches();
    let backend = counting_worker(Arc::clone(&seen));
    let outcome = fixture.execute_as(
        Some(run.clone()),
        &verify_contract(&source, None),
        &repo,
        &backend,
    );
    let detail = format!("{outcome:?}");
    assert!(
        detail.contains(&stale) && detail.contains(&base),
        "{detail}"
    );
    assert_eq!(
        fixture.ledger.run_status(&run).expect("status"),
        Some(State::Blocked)
    );
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_user_diff_config_does_not_change_the_candidate_digest() {
    let fixture = Fixture::new();
    let (source, _) = accepted_source(&fixture);
    commit_file(&fixture, "README.md", "unrelated\n");
    git(&fixture.repo, &["config", "diff.noprefix", "true"]);
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let backend = counting_worker(launches());
    let outcome = fixture.execute(&verify_contract(&source, None), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// A predictor that always estimates, and counts how often it was asked:
/// a route that consulted it carries estimates.
struct CountingPredictor(AtomicUsize);

impl crate::route::RoutePredictor for CountingPredictor {
    fn estimate(
        &self,
        _contract: &TaskContract,
        _authority: &crate::policy::EffectiveAuthority,
        arms: &[crate::route::Arm],
    ) -> Option<crate::route::Estimates> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(crate::route::Estimates {
            artifact_id: "artifact-verify-test".into(),
            input_hash: "in".into(),
            acceptance: arms.iter().map(|arm| (arm.clone(), 0.9)).collect(),
            cost: arms
                .iter()
                .map(|arm| (arm.clone(), crate::money::MicroUsd::from_micros(10)))
                .collect(),
            raw: serde_json::Value::Null,
        })
    }
}

/// `Fixture::execute` with a learned predictor in the route.
fn execute_predicted(
    fixture: &Fixture,
    contract: &TaskContract,
    repo: &RepoPolicy,
    backend: &dyn Backend,
    predictor: &dyn crate::route::RoutePredictor,
) -> RunOutcome {
    let mut machine = fixture.machine_for(repo);
    machine.routing.learned_enabled = true;
    let resolver = |_: &str, _: Option<&str>| AvalVerdict::Active {
        adr: "ADR-0001".into(),
        choice: None,
        reason: None,
    };
    execute(&RunConfig {
        workspace: crate::workspace::reuse::Workspace::Isolated,
        repo_dir: &fixture.repo,
        contract,
        repo_policy: repo,
        machine: &machine,
        ledger: &fixture.ledger,
        ids: &fixture.ids,
        backend,
        git: &crate::workspace::SystemGit,
        hooks: &crate::verify::FixedInventory(None),
        attest: &crate::verify::FixedAttest::default(),
        artifacts_dir: fixture.artifacts.clone(),
        aval_resolver: &resolver,
        predictor: Some(predictor),
        gate: None,
        session_id: "test-session".into(),
        heartbeat_every: Duration::from_millis(50),
        task_override: None,
        purpose: None,
        run_id: None,
        wire: Wire::process(),
        design: DesignSource::Contract,
        declarations: &crate::trust::NoHistory,
    })
    .expect("the fixture's id source mints identifiers")
}

#[test]
fn a_verify_run_records_no_routing_prediction() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let predictor = CountingPredictor(AtomicUsize::new(0));

    // A change routed through the predictor records its prediction.
    let change = execute_predicted(
        &fixture,
        &fixture.contract(Review::Off),
        &repo,
        &conditional_worker("relais task"),
        &predictor,
    );
    assert!(
        matches!(change.terminal, Terminal::Accepted(_)),
        "{change:?}"
    );
    assert_eq!(
        fixture
            .ledger
            .predictions(&change.run_id)
            .expect("predictions")
            .len(),
        1
    );

    // A verify of that change's candidate dispatches no worker and
    // records none, though the predictor is in its route as well.
    let asked_before = predictor.0.load(Ordering::SeqCst);
    let verify = execute_predicted(
        &fixture,
        &verify_contract(&change.run_id, None),
        &repo,
        &counting_worker(launches()),
        &predictor,
    );
    assert!(
        matches!(verify.terminal, Terminal::Accepted(_)),
        "{verify:?}"
    );
    assert!(predictor.0.load(Ordering::SeqCst) > asked_before);
    assert!(fixture
        .ledger
        .predictions(&verify.run_id)
        .expect("predictions")
        .is_empty());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

/// Which of the runs a test launched for an intent have a row, whatever the
/// intent's claim names now: a duplicate shows here even when the claim
/// points elsewhere.
fn runs_with_a_row<'a>(ledger: &Ledger, launched: &[&'a RunId]) -> Vec<&'a RunId> {
    launched
        .iter()
        .copied()
        .filter(|run| ledger.run_exists(run).expect("read"))
        .collect()
}

#[test]
fn a_launch_that_dies_before_its_id_is_returned_leaves_exactly_one_successor_run() {
    use crate::ledger::{failpoint, Claim, DecisionAnswer, SuccessorProcess};
    use crate::runner::successor;

    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let backend = MockBackend::new(|spec| {
        std::fs::create_dir_all(spec.work_dir.join("docs")).expect("mkdir");
        std::fs::write(spec.work_dir.join("docs/extra.md"), "one\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let RunOutcome {
        run_id: decided,
        terminal: Terminal::NeedsDecision { .. },
    } = fixture.execute(&fixture.contract(Review::Off), &repo, &backend)
    else {
        panic!("a write outside the scope waits on a person");
    };
    let paths = successor::out_of_scope_paths(&fixture.ledger, &decided).expect("paths");
    let (original, _) = fixture
        .ledger
        .run_contract_and_tier(&decided)
        .expect("read")
        .expect("recorded");
    let widened = successor::widened_contract(&original, &decided, &paths).expect("widened");
    let intent = successor::plan(
        &fixture.ledger,
        &decided,
        &widened,
        paths,
        &fixture.artifacts,
    )
    .expect("planned");
    let stored = fixture
        .ledger
        .resolve_decision_with_intent(
            &decided,
            &DecisionAnswer {
                resolution: Reason::DecisionRevised,
                actor: crate::ledger::Actor::Orchestrator,
                person_quote: None,
                recorded_via: crate::ledger::RecordedVia::Cli,
                note: None,
                successor_run: None,
                from_state: State::NeedsDecision,
                to_state: State::Failed,
            },
            &intent,
        )
        .expect("written")
        .expect("open decision");
    let alive = |_: u32| SuccessorProcess::Alive { started: None };
    let first = fixture.ids.run_id().expect("a run id");
    assert!(matches!(
        fixture
            .ledger
            .claim_intent(&stored.intent_id, &first, &alive)
            .expect("claim"),
        Claim::Claimed(_)
    ));
    let contract = TaskContract::from_json_str(&stored.contract_json).expect("the stored contract");

    // The launch inserts the run row, then dies before the caller learns
    // the run id.
    failpoint::arm(failpoint::AFTER_LAUNCH);
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fixture.execute_as(Some(first.clone()), &contract, &repo, &backend)
    }));
    failpoint::disarm();
    assert!(crashed.is_err(), "the failpoint fired");

    // Recovery: the plugin polls again and claims again. The run that was
    // launched holds the claim; no second run is made.
    let retry = fixture.ids.run_id().expect("a run id");
    assert_eq!(
        fixture
            .ledger
            .claim_intent(&stored.intent_id, &retry, &alive)
            .expect("claim"),
        Claim::AlreadyClaimed { run: first.clone() }
    );
    assert_eq!(
        runs_with_a_row(&fixture.ledger, &[&first, &retry]),
        vec![&first],
        "the crashed launch made one run; the retry made none"
    );

    // The launched run is presented as gone. A run that has a row and a
    // recorded pid whose process is gone is abandoned at once: the grace
    // period applies only to a claim whose run has no usable pid. The claim
    // moves, once, to the retry.
    let gone = |_: u32| SuccessorProcess::Gone;
    assert!(matches!(
        fixture
            .ledger
            .claim_intent(&stored.intent_id, &retry, &gone)
            .expect("claim"),
        Claim::Claimed(intent) if intent.run_id == Some(retry.clone())
    ));
    let third = fixture.ids.run_id().expect("a run id");
    assert!(
        matches!(
            fixture
                .ledger
                .claim_intent(&stored.intent_id, &third, &alive)
                .expect("claim"),
            Claim::AlreadyClaimed { run } if run == retry
        ),
        "the claim moved once"
    );
    // The second run launches; the intent now names it alone.
    fixture.execute_as(Some(retry.clone()), &contract, &repo, &backend);
    // Total rows tied to this intent: the crashed first run and the retry,
    // never a third.
    assert_eq!(
        runs_with_a_row(&fixture.ledger, &[&first, &retry, &third]),
        vec![&first, &retry],
        "two launches in total, one abandoned, and none for the refused third claim"
    );
    assert_eq!(
        fixture
            .ledger
            .successor_intent(&stored.intent_id)
            .expect("read")
            .expect("the intent")
            .run_id,
        Some(retry.clone())
    );
    assert_eq!(fixture.ledger.count_successor_intents().expect("count"), 1);
    std::fs::remove_dir_all(&fixture.dir).ok();
}
