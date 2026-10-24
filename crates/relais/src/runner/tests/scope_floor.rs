// A follow-up is scope-checked against its own changes (SPEC §8).

use super::*;

fn writing_worker(files: &'static [(&'static str, &'static str)]) -> MockBackend {
    MockBackend::new(move |spec| {
        for (path, content) in files {
            let target = spec.work_dir.join(path);
            std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
            std::fs::write(target, content).expect("write");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

fn change_contract(scope: &[&str], from_candidate: Option<&RunId>) -> TaskContract {
    let mut contract = serde_json::json!({
        "schema_version": 1,
        "kind": "change",
        "objective": "Add a module",
        "base_ref": "HEAD",
        "write_scope": scope,
        "acceptance": ["the module exists"],
        "verification_profile": "profile",
        "review": "off",
    });
    if let Some(source) = from_candidate {
        contract["from_candidate"] = serde_json::json!({ "run": source.as_str() });
    }
    TaskContract::from_json_str(&contract.to_string()).expect("a change contract")
}

/// An accepted parent that changed `src/parent.rs` and `docs/parent.md`.
fn accepted_parent(fixture: &Fixture, repo: &RepoPolicy) -> RunId {
    let worker = writing_worker(&[
        ("src/parent.rs", "pub fn parent() {}\n"),
        ("docs/parent.md", "p\n"),
    ]);
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(_),
    } = fixture.execute(
        &change_contract(&["src/**", "docs/**"], None),
        repo,
        &worker,
    )
    else {
        panic!("the parent is accepted");
    };
    run_id
}

fn commit_on_base(fixture: &Fixture, path: &str) {
    let target = fixture.repo.join(path);
    std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
    std::fs::write(target, "base\n").expect("write");
    git(&fixture.repo, &["add", "-A"]);
    git(&fixture.repo, &["commit", "-q", "-m", "move the target on"]);
}

#[test]
fn a_follow_up_changing_only_in_scope_files_is_not_charged_for_its_parents_files() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let parent = accepted_parent(&fixture, &repo);
    let outcome = fixture.execute(
        &change_contract(&["src/**"], Some(&parent)),
        &repo,
        &writing_worker(&[("src/child.rs", "pub fn child() {}\n")]),
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_follow_up_editing_a_parents_file_outside_its_scope_is_named() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let parent = accepted_parent(&fixture, &repo);
    let outcome = fixture.execute(
        &change_contract(&["src/**"], Some(&parent)),
        &repo,
        &writing_worker(&[("docs/parent.md", "edited\n")]),
    );
    let Terminal::NeedsDecision { reason, detail } = outcome.terminal else {
        panic!("an out-of-scope edit waits on a person: {outcome:?}");
    };
    assert_eq!(reason, Reason::ScopeExceeded);
    assert!(detail.contains("docs/parent.md"), "{detail}");
    assert!(!detail.contains("src/parent.rs"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn the_target_bases_own_changes_cause_no_scope_violation_after_it_advanced() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let parent = accepted_parent(&fixture, &repo);
    commit_on_base(&fixture, "notes/base.md");
    let outcome = fixture.execute(
        &change_contract(&["src/**"], Some(&parent)),
        &repo,
        &writing_worker(&[("src/child.rs", "pub fn child() {}\n")]),
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_protected_path_is_still_refused_for_a_follow_up() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let parent = accepted_parent(&fixture, &repo);
    let outcome = fixture.execute(
        &change_contract(&["**"], Some(&parent)),
        &repo,
        &writing_worker(&[(".github/workflows/ci.yml", "on: push\n")]),
    );
    let Terminal::NeedsDecision { reason, detail } = outcome.terminal else {
        panic!("a protected path waits on a person: {outcome:?}");
    };
    assert_eq!(reason, Reason::ScopeExceeded);
    assert!(detail.contains(".github/workflows/ci.yml"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_verify_is_still_measured_from_the_base_for_its_candidates_own_out_of_scope_change() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let RunOutcome {
        run_id: source,
        terminal: Terminal::NeedsDecision { .. },
    } = fixture.execute(
        &change_contract(&["src/**"], None),
        &repo,
        &writing_worker(&[("docs/extra.md", "one\n")]),
    )
    else {
        panic!("a write outside the scope waits on a person");
    };
    let verify = TaskContract::from_json_str(
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
    let outcome = fixture.execute(&verify, &repo, &writing_worker(&[]));
    let Terminal::NeedsDecision { reason, detail } = outcome.terminal else {
        panic!("the candidate's own out-of-scope change is reported: {outcome:?}");
    };
    assert_eq!(reason, Reason::ScopeExceeded);
    assert!(detail.contains("docs/extra.md"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_decomposed_from_candidate_contract_is_refused() {
    let error = TaskContract::from_json_str(
        &serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Add a module",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "from_candidate": { "run": "run-1" },
            "decomposition": "propose",
            "acceptance": ["the module exists"],
            "verification_profile": "profile",
            "review": "off",
        })
        .to_string(),
    )
    .expect_err("a from_candidate contract cannot be decomposed");
    assert!(
        error.to_string().contains("cannot be decomposed"),
        "{error}"
    );
}
