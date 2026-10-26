//! Holdout criteria at preflight and at verification (ADR-0045 phases 2, 3a).

use super::*;
use crate::policy::{HoldoutDecl, HoldoutSettings};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const STATEMENT: &str = "the escaping holdout passes";
const STORE_PATH_IN_STORE: &str = "secret-store-path/escaping.rs";
const OVERLAY: &str = "src/holdout_escaping.rs";
const RUNNER_CONFIG: &str = "secret-store-path/runner.toml";

fn declaration(junit: Option<&str>, overlay: &str) -> HoldoutDecl {
    HoldoutDecl {
        path: STORE_PATH_IN_STORE.into(),
        overlay: overlay.into(),
        command: serde_json::from_value(
            serde_json::json!({ "argv": ["sh", "-c", "true"], "junit": junit }),
        )
        .expect("a command"),
        config: Some(RUNNER_CONFIG.into()),
    }
}

fn settings_for(fixture: &Fixture, store: &str, declared: HoldoutDecl) -> HoldoutSettings {
    let repo_name = crate::repo::main_checkout_name(&fixture.repo).expect("repo name");
    let mut settings = HoldoutSettings {
        store: Some(store.into()),
        ..HoldoutSettings::default()
    };
    settings
        .repos
        .entry(repo_name)
        .or_default()
        .insert("escaping".into(), declared);
    settings
}

fn holdout_contract(fixture: &Fixture, mandatory: bool) -> TaskContract {
    let mut contract = fixture.contract(Review::Off);
    contract.acceptance = vec![crate::acceptance::AcceptanceEntry::Declared(
        crate::acceptance::DeclaredCriterion {
            statement: STATEMENT.into(),
            id: None,
            mandatory,
            evidence: crate::acceptance::Evidence::Holdout {
                name: "escaping".into(),
            },
        },
    )];
    contract
}

/// A local git repository with a `main` branch, standing in for the store.
fn local_store(fixture: &Fixture) -> (String, String) {
    store_with(fixture, &[("README", "holdouts\n")])
}

/// The same, holding `files` (path, content) in its one commit.
fn store_with(fixture: &Fixture, files: &[(&str, &str)]) -> (String, String) {
    let store = fixture.dir.join("holdout-store");
    std::fs::create_dir_all(&store).expect("store dir");
    git(&store, &["init", "-q", "-b", "main"]);
    for (path, content) in files {
        let file = store.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
        std::fs::write(&file, content).expect("write");
        git(&store, &["add", path]);
    }
    git(
        &store,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "test: store",
        ],
    );
    let sha = git(&store, &["rev-parse", "main"]).trim().to_string();
    (format!("file://{}", store.display()), sha)
}

fn counting_worker(launches: Arc<AtomicUsize>, prompts: Arc<Mutex<Vec<String>>>) -> MockBackend {
    MockBackend::new(move |spec| {
        launches.fetch_add(1, Ordering::SeqCst);
        prompts.lock().expect("lock").push(spec.prompt.clone());
        if !spec.prompt.contains("semantic reviewer") {
            std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("worker completes");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

fn blocked_code(outcome: RunOutcome) -> BlockCode {
    let RunOutcome {
        terminal: Terminal::Blocked { code, .. },
        ..
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    code
}

#[test]
fn each_unusable_holdout_is_refused_by_its_own_code_before_any_worker() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let contract = holdout_contract(&fixture, true);
    let (store, _) = local_store(&fixture);
    let missing_store = format!("file://{}/no-such-store", fixture.dir.display());
    let cases = [
        (HoldoutSettings::default(), BlockCode::HoldoutUnknown),
        (
            settings_for(&fixture, &store, declaration(None, OVERLAY)),
            BlockCode::HoldoutNoJunit,
        ),
        (
            settings_for(&fixture, &store, declaration(Some("j.xml"), "../x.rs")),
            BlockCode::HoldoutBadOverlay,
        ),
        (
            settings_for(&fixture, &store, declaration(Some("j.xml"), "src/main.rs")),
            BlockCode::HoldoutOverlayExists,
        ),
        (
            settings_for(
                &fixture,
                &missing_store,
                declaration(Some("j.xml"), OVERLAY),
            ),
            BlockCode::HoldoutUnreadable,
        ),
    ];
    for (settings, expected) in cases {
        let launches = Arc::new(AtomicUsize::new(0));
        let backend = counting_worker(launches.clone(), Arc::default());
        let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
        assert_eq!(blocked_code(outcome), expected);
        assert_eq!(launches.load(Ordering::SeqCst), 0, "{expected}");
    }
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_resolved_holdout_is_recorded_without_its_location_and_stays_a_gap() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let (store, store_sha) = local_store(&fixture);
    // The overlay lies inside the write scope (`src/**`): accepted.
    let settings = settings_for(&fixture, &store, declaration(Some("j.xml"), OVERLAY));
    let launches = Arc::new(AtomicUsize::new(0));
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let backend = counting_worker(launches.clone(), prompts.clone());
    let outcome = fixture.execute_with_holdouts(
        settings.clone(),
        &holdout_contract(&fixture, true),
        &repo,
        &backend,
    );
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "a mandatory holdout is an unsettled gap: {outcome:?}"
    );
    assert!(launches.load(Ordering::SeqCst) > 0, "preflight passed");

    let manifest_text = std::fs::read_to_string(
        fixture
            .artifacts
            .join(outcome.run_id.as_str())
            .join("manifest.json"),
    )
    .expect("manifest");
    let manifest: serde_json::Value = serde_json::from_str(&manifest_text).expect("json");
    let recorded = manifest["holdouts"].as_array().expect("holdouts");
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["name"], "escaping");
    assert_eq!(recorded[0]["sha"], store_sha.as_str());
    assert_eq!(recorded[0]["digest"].as_str().map(str::len), Some(64));
    for hidden in [&store, STORE_PATH_IN_STORE, OVERLAY, RUNNER_CONFIG] {
        assert!(!manifest_text.contains(hidden), "manifest leaks {hidden}");
    }

    let prompts = prompts.lock().expect("lock");
    assert!(
        prompts.iter().any(|prompt| prompt.contains(STATEMENT)),
        "the worker is told the criterion"
    );
    for hidden in [&store, STORE_PATH_IN_STORE, OVERLAY, RUNNER_CONFIG] {
        assert!(
            prompts.iter().all(|prompt| !prompt.contains(hidden)),
            "the worker's prompt leaks {hidden}"
        );
    }

    // The same holdout, optional: nothing blocks acceptance.
    let backend = counting_worker(Arc::default(), Arc::default());
    let optional = fixture.execute_with_holdouts(
        settings,
        &holdout_contract(&fixture, false),
        &repo,
        &backend,
    );
    assert!(
        matches!(optional.terminal, Terminal::Accepted(_)),
        "{optional:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

const SCRIPT_MARKER: &str = "SECRET-SCRIPT-LINE";
const PASSING: &str =
    r#"<testsuite tests="1" failures="0"><testcase classname="h" name="escapes"/></testsuite>"#;
const FAILING: &str = r#"<testsuite tests="1" failures="1"><testcase classname="h" name="escapes"><failure message="m"/></testcase></testsuite>"#;
const SKIPPED: &str = r#"<testsuite tests="1" failures="0"><testcase classname="h" name="escapes"><skipped/></testcase></testsuite>"#;

/// A holdout script that writes `report` where the declaration expects it.
fn writes(report: &str) -> String {
    format!("#!/bin/sh\n# {SCRIPT_MARKER}\ncat > j.xml <<'EOF'\n{report}\nEOF\n")
}

fn script_declaration(argv: &[&str], config: Option<&str>) -> HoldoutDecl {
    HoldoutDecl {
        path: STORE_PATH_IN_STORE.into(),
        overlay: OVERLAY.into(),
        command: serde_json::from_value(serde_json::json!({ "argv": argv, "junit": "j.xml" }))
            .expect("a command"),
        config: config.map(Into::into),
    }
}

fn run_script(fixture: &Fixture, script: &str, config: Option<(&str, &str)>) -> RunOutcome {
    let mut files = vec![(STORE_PATH_IN_STORE, script)];
    let argv: &[&str] = match config {
        Some((path, content)) => {
            files.push((path, content));
            &["sh", OVERLAY, "{config}"]
        }
        None => &["sh", OVERLAY],
    };
    let (store, _) = store_with(fixture, &files);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_for(
        fixture,
        &store,
        script_declaration(argv, config.map(|(path, _)| path)),
    );
    let backend = counting_worker(Arc::default(), Arc::default());
    fixture.execute_with_holdouts(settings, &holdout_contract(fixture, true), &repo, &backend)
}

fn holdout_dir_of(fixture: &Fixture, outcome: &RunOutcome) -> std::path::PathBuf {
    crate::runner::config::holdout_root(&fixture.artifacts).join(outcome.run_id.as_str())
}

fn assert_gap_leaks_nothing(detail: &str, store: &str) {
    for script_line in writes(FAILING).lines().filter(|line| !line.is_empty()) {
        assert!(
            !detail.contains(script_line),
            "the gap leaks the script line {script_line}: {detail}"
        );
    }
    for hidden in [
        SCRIPT_MARKER,
        store,
        STORE_PATH_IN_STORE,
        OVERLAY,
        RUNNER_CONFIG,
    ] {
        assert!(!detail.contains(hidden), "the gap leaks {hidden}: {detail}");
    }
}

#[test]
fn a_passing_holdout_settles_its_criterion_and_is_listed_on_the_receipt() {
    let fixture = Fixture::new();
    let outcome = run_script(&fixture, &writes(PASSING), None);
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    let [run] = receipt.holdouts.as_slice() else {
        panic!("{:?}", receipt.holdouts);
    };
    assert_eq!(run.name, "escaping");
    assert_eq!(run.outcome, crate::verify::HoldoutVerdict::Passed);
    assert_eq!(run.tests.len(), 1);
    assert_eq!(run.tests[0].id, "h::escapes");
    assert!(receipt.criteria.iter().all(|criterion| criterion.met));
    let kept_log = run.log.as_deref().expect("the log is kept for the person");
    assert!(std::path::Path::new(kept_log).is_file(), "{kept_log}");

    assert!(
        !holdout_dir_of(&fixture, &outcome).exists(),
        "the run's holdout directory is removed"
    );
    let holdout_root = crate::runner::config::holdout_root(&fixture.artifacts);
    let check_logs = evidence_of(&fixture, &outcome.run_id, EvidenceKind::CheckLog);
    assert!(
        !check_logs.is_empty(),
        "the profile's own check is recorded"
    );
    for (path, _) in check_logs {
        assert!(
            !path.contains("holdout") && !path.starts_with(&*holdout_root.to_string_lossy()),
            "a holdout log is not a check log: {path}"
        );
    }
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_failing_holdout_test_leaves_the_criterion_unmet_without_leaking_the_holdout() {
    let fixture = Fixture::new();
    let outcome = run_script(&fixture, &writes(FAILING), None);
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let detail = outcome.detail();
    assert!(
        detail.contains("holdout `escaping` failed: h::escapes"),
        "{detail}"
    );
    let criterion_id = holdout_contract(&fixture, true).acceptance[0].id();
    assert!(
        detail.contains(&format!("acceptance criterion `{criterion_id}`")),
        "{detail}"
    );
    assert_gap_leaks_nothing(detail, &format!("file://{}", fixture.dir.display()));
    assert!(!holdout_dir_of(&fixture, &outcome).exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_skipped_holdout_test_is_not_a_pass_even_on_exit_zero() {
    let fixture = Fixture::new();
    let outcome = run_script(&fixture, &writes(SKIPPED), None);
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert!(
        outcome
            .detail()
            .contains("holdout `escaping` failed: h::escapes"),
        "{}",
        outcome.detail()
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_holdout_that_writes_no_report_did_not_build() {
    let fixture = Fixture::new();
    let outcome = run_script(
        &fixture,
        &format!("#!/bin/sh\n# {SCRIPT_MARKER}\nexit 0\n"),
        None,
    );
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let detail = outcome.detail();
    assert!(
        detail.contains("holdout `escaping` did not build or wrote no report"),
        "{detail}"
    );
    assert_gap_leaks_nothing(detail, &format!("file://{}", fixture.dir.display()));
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn the_config_placeholder_reaches_the_holdout_command_as_the_stores_config_path() {
    let fixture = Fixture::new();
    // The script copies the config file it is handed: it passes only when
    // `{config}` was replaced by a readable absolute path inside the store.
    let script = "#!/bin/sh\ncat \"$1\" > j.xml\n";
    let outcome = run_script(&fixture, script, Some((RUNNER_CONFIG, PASSING)));
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn a_store_deleted_after_preflight_blocks_the_run_and_keeps_the_candidate() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let store_dir = fixture.dir.join("holdout-store");
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let backend = MockBackend::new(move |spec| {
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("worker completes");
        if store_dir.exists() {
            std::fs::remove_dir_all(&store_dir).expect("the store goes");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome =
        fixture.execute_with_holdouts(settings, &holdout_contract(&fixture, true), &repo, &backend);
    let run_id = outcome.run_id.clone();
    assert_eq!(blocked_code(outcome.clone()), BlockCode::HoldoutUnfetchable);
    assert!(
        outcome.detail().contains("holdout store"),
        "{}",
        outcome.detail()
    );
    git(
        &fixture.repo,
        &[
            "rev-parse",
            "--verify",
            &crate::workspace::candidate_ref(run_id.as_str(), 1),
        ],
    );
    assert!(!holdout_dir_of(&fixture, &outcome).exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn the_store_is_fetched_once_and_reused_across_attempts() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let store_dir = fixture.dir.join("holdout-store");
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let launches = Arc::new(AtomicUsize::new(0));
    let counted = launches.clone();
    let backend = MockBackend::new(move |spec| {
        // The first verification has fetched by the time the second
        // attempt launches: without the reuse it would find no store.
        let launch = counted.fetch_add(1, Ordering::SeqCst);
        if launch >= 1 && store_dir.exists() {
            std::fs::remove_dir_all(&store_dir).expect("the store goes");
        }
        // The first attempt leaves the check failing, so a repair follows.
        if !spec.prompt.contains("semantic reviewer") {
            if launch == 0 {
                std::fs::write(spec.work_dir.join("src/other.rs"), "// first\n").expect("write");
            } else {
                std::fs::remove_file(spec.work_dir.join("src/main.rs")).ok();
            }
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome =
        fixture.execute_with_holdouts(settings, &holdout_contract(&fixture, true), &repo, &backend);
    assert!(launches.load(Ordering::SeqCst) >= 2, "{outcome:?}");
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "the second attempt fetched again: {outcome:?}"
    );
    assert!(!holdout_dir_of(&fixture, &outcome).exists());
    std::fs::remove_dir_all(&fixture.dir).ok();
}

#[test]
fn ended_run_holdout_dirs_are_swept_at_run_start_and_live_or_unknown_ones_kept() {
    let fixture = Fixture::new();
    let ended = run_script(&fixture, &writes(PASSING), None);
    assert!(
        fixture
            .ledger
            .run_status(&ended.run_id)
            .expect("reads")
            .expect("recorded")
            .is_terminal(),
        "{ended:?}"
    );
    let task = fixture
        .ledger
        .task_of_run(&ended.run_id)
        .expect("reads")
        .expect("the run's task");
    let live = RunId::from_stored("run-still-live");
    fixture
        .ledger
        .insert_run(&live, "/repo", None, &task, "rk")
        .expect("a live run");
    assert!(
        !fixture
            .ledger
            .run_status(&live)
            .expect("reads")
            .expect("recorded")
            .is_terminal(),
        "the inserted run is live"
    );
    let root = crate::runner::config::holdout_root(&fixture.artifacts);
    let dir_of = |name: &str| {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("x"), "fetched").expect("file");
        dir
    };
    let ended_dir = dir_of(ended.run_id.as_str());
    let live_dir = dir_of(live.as_str());
    let unknown_dir = dir_of("run-unknown-to-the-ledger");

    // The start of another root run sweeps. Its script differs so that
    // the shared store gains a commit.
    let _next = run_script(&fixture, &format!("{}\n# again\n", writes(PASSING)), None);
    assert!(!ended_dir.exists(), "an ended run's holdout dir is swept");
    assert!(
        live_dir.join("x").is_file(),
        "a live run's holdout dir is kept"
    );
    assert!(
        unknown_dir.join("x").is_file(),
        "an unknown run's holdout dir is kept"
    );

    // The `relais resume --retire` path calls the same sweep.
    let ended_again = dir_of(ended.run_id.as_str());
    crate::runner::sweep_ended_holdouts(&root, &fixture.ledger);
    assert!(!ended_again.exists(), "the retire sweep removes it too");
    assert!(live_dir.join("x").is_file());
    assert!(unknown_dir.join("x").is_file());
    std::fs::remove_dir_all(&fixture.dir).ok();
}
