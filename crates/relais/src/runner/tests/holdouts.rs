//! Holdout criteria at preflight and at verification (ADR-0045 phases 2, 3a).

use super::*;
use crate::backend::{BackendError, Harness};
use crate::ledger::{Actor, DecisionAnswer, RecordedVia};
use crate::policy::{HoldoutDecl, HoldoutSettings};
use crate::runner::approval::{approve_from_snapshot, ApprovalError};
use crate::runner::prompt::RepairFailure;
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
        inputs: Vec::new(),
        attach: None,
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

/// A removal that finds nothing to remove is the expected case; any other
/// error is a broken fixture.
fn ignore_not_found(removal: std::io::Result<()>) {
    match removal {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("fixture removal failed: {e}"),
    }
}

fn counting_worker(launches: Arc<AtomicUsize>, prompts: Arc<Mutex<Vec<String>>>) -> MockBackend {
    MockBackend::new(move |spec| {
        launches.fetch_add(1, Ordering::SeqCst);
        prompts.lock().expect("lock").push(spec.prompt.clone());
        if !spec.prompt.contains("semantic reviewer") {
            // A repair finds the file already gone.
            ignore_not_found(std::fs::remove_file(spec.work_dir.join("src/main.rs")));
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
        inputs: Vec::new(),
        attach: None,
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
    assert_eq!(run.source, Some(crate::holdout::HoldoutSource::Named));
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
                ignore_not_found(std::fs::remove_file(spec.work_dir.join("src/main.rs")));
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
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
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

/// A run of the failing holdout script with `backend` as the worker.
fn run_failing_with(
    fixture: &Fixture,
    inputs: Vec<String>,
    mandatory: bool,
    review: Review,
    max_attempts: u32,
    backend: &MockBackend,
) -> (RunOutcome, String, String) {
    let (store, sha) = store_with(fixture, &[(STORE_PATH_IN_STORE, &writes(FAILING))]);
    let repo = fixture.repo_policy(vec![passing_check()], max_attempts);
    let mut declared = script_declaration(&["sh", OVERLAY], None);
    declared.inputs = inputs;
    let settings = settings_for(fixture, &store, declared);
    let mut contract = holdout_contract(fixture, mandatory);
    contract.review = review;
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, backend);
    (outcome, store, sha)
}

/// The detail of the transition that raised the run's open decision,
/// found the way `decide` finds the gaps that refuse an approval.
fn holdout_failed_detail(fixture: &Fixture, outcome: &RunOutcome) -> serde_json::Value {
    let decision = fixture
        .ledger
        .decision_of_run(&outcome.run_id)
        .expect("decision")
        .expect("an open decision");
    let raising = fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("transitions")
        .into_iter()
        .find(|transition| {
            transition.at == decision.raised_at && transition.to_state.awaits_a_person()
        })
        .expect("the transition that raised the decision");
    assert_eq!(raising.reason, "holdout_failed");
    raising.detail.expect("a detail")
}

fn reason_of_end(fixture: &Fixture, outcome: &RunOutcome) -> String {
    fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("transitions")
        .iter()
        .find(|transition| {
            transition.to_state.is_terminal() || transition.to_state.awaits_a_person()
        })
        .map(|transition| transition.reason.clone())
        .expect("a terminal transition")
}

/// Every file under the run's artifacts, with its path and content.
fn artifact_files(fixture: &Fixture, outcome: &RunOutcome) -> Vec<(std::path::PathBuf, String)> {
    let mut pending = vec![fixture.artifacts.join(outcome.run_id.as_str())];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).expect("artifacts").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(content) = std::fs::read_to_string(&path) {
                files.push((path, content));
            }
        }
    }
    files
}

#[test]
fn a_worker_that_writes_the_overlay_path_ends_the_run_failed_with_no_repair() {
    let fixture = Fixture::new();
    let launches = Arc::new(AtomicUsize::new(0));
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let (counted, seen) = (launches.clone(), prompts.clone());
    let backend = MockBackend::new(move |spec| {
        counted.fetch_add(1, Ordering::SeqCst);
        seen.lock().expect("lock").push(spec.prompt.clone());
        std::fs::write(spec.work_dir.join(OVERLAY), "// planted\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let (outcome, _store, _sha) =
        run_failing_with(&fixture, Vec::new(), true, Review::Off, 3, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        reason_of_end(&fixture, &outcome),
        "holdout_overlay_collision"
    );
    assert!(outcome.detail().contains(OVERLAY), "{}", outcome.detail());
    assert_eq!(
        launches.load(Ordering::SeqCst),
        1,
        "no repair, no escalation"
    );
    for prompt in prompts.lock().expect("lock").iter() {
        assert!(!prompt.contains(OVERLAY), "a prompt carries the overlay");
    }
    let files = artifact_files(&fixture, &outcome);
    assert!(!files.is_empty(), "the run keeps artifacts");
    // The candidate's patch is the worker's own text and the event log
    // carries the detail only the person reads; every other artifact (the
    // manifest a successor's context is built from, receipts) must not
    // name the overlay.
    for (path, content) in files.into_iter().filter(|(path, _)| {
        path.extension().is_none_or(|ext| ext != "patch")
            && path.file_name().is_none_or(|name| name != "events.jsonl")
    }) {
        assert!(
            !content.contains(OVERLAY),
            "{} carries the overlay",
            path.display()
        );
    }
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_worker_that_changes_a_declared_holdout_input_stops_for_a_person() {
    let fixture = Fixture::new();
    let backend = MockBackend::new(move |spec| {
        std::fs::create_dir_all(spec.work_dir.join("src/guarded")).expect("dir");
        std::fs::write(spec.work_dir.join("src/guarded/runner.toml"), "x = 1\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let (outcome, _store, _sha) = run_failing_with(
        &fixture,
        vec!["src/guarded/**".into()],
        true,
        Review::Off,
        3,
        &backend,
    );
    let Terminal::NeedsDecision { reason, detail } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert_eq!(*reason, Reason::VerificationInputsChanged);
    assert!(detail.contains("src/guarded/runner.toml"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

/// A worker whose every attempt writes a different file, so no attempt
/// repeats the one before and the run goes on to escalate.
fn changing_worker(prompts: Arc<Mutex<Vec<String>>>) -> MockBackend {
    let launches = Arc::new(AtomicUsize::new(0));
    MockBackend::new(move |spec| {
        let launch = launches.fetch_add(1, Ordering::SeqCst);
        prompts.lock().expect("lock").push(spec.prompt.clone());
        if !spec.prompt.contains("semantic reviewer") {
            std::fs::write(
                spec.work_dir.join(format!("src/attempt_{launch}.rs")),
                "//\n",
            )
            .expect("write");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

#[test]
fn every_attempt_failing_on_the_holdout_ends_for_a_person_with_what_a_worker_never_sees() {
    let fixture = Fixture::new();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let backend = changing_worker(prompts.clone());
    let (outcome, store, sha) =
        run_failing_with(&fixture, Vec::new(), true, Review::Off, 3, &backend);
    let Terminal::NeedsDecision { reason, detail } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert_eq!(*reason, Reason::HoldoutFailed);
    let recorded = holdout_failed_detail(&fixture, &outcome);
    assert_eq!(recorded["holdouts"][0]["name"], "escaping");
    assert_eq!(recorded["holdouts"][0]["sha"], sha.as_str());
    assert_eq!(recorded["holdouts"][0]["failing"][0], "h::escapes");
    assert!(
        recorded["holdouts"][0]["log"].as_str().is_some_and(|path| {
            Path::new(path).ends_with(Path::new("holdouts").join("escaping").join("log.txt"))
        }),
        "{recorded}"
    );
    assert!(
        recorded["holdouts"][0]["report"]
            .as_str()
            .is_some_and(|path| {
                Path::new(path).ends_with(Path::new("holdouts").join("escaping").join("report.xml"))
            }),
        "{recorded}"
    );
    assert!(detail.contains(&sha), "{detail}");
    assert_eq!(reason_of_end(&fixture, &outcome), "holdout_failed");
    assert!(detail.contains("escaping"), "{detail}");
    assert!(detail.contains("h::escapes"), "{detail}");
    let recorded_log = recorded["holdouts"][0]["log"].as_str().expect("a log path");
    assert!(detail.contains(recorded_log), "{detail}");
    assert!(
        detail.contains("--answer approve|revise|reject"),
        "{detail}"
    );

    let prompts = prompts.lock().expect("lock");
    assert!(prompts.len() >= 2, "a repair was dispatched");
    for prompt in prompts.iter() {
        for hidden in [
            store.as_str(),
            STORE_PATH_IN_STORE,
            OVERLAY,
            RUNNER_CONFIG,
            SCRIPT_MARKER,
            "log.txt",
            "report.xml",
        ] {
            assert!(!prompt.contains(hidden), "a prompt leaks {hidden}");
        }
    }
    let repair = prompts
        .iter()
        .find(|prompt| prompt.contains("[repair addendum]"))
        .expect("a repair prompt");
    assert!(
        repair.contains(STATEMENT) && repair.contains("h::escapes"),
        "{repair}"
    );
    assert!(!repair.contains("(log under logs/)"), "{repair}");
    let escalation = prompts
        .iter()
        .find(|prompt| prompt.contains("[escalation addendum]"))
        .expect("an escalation prompt");
    assert!(
        escalation.contains(STATEMENT) && escalation.contains("h::escapes"),
        "{escalation}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn running_out_of_attempts_on_a_holdout_alone_ends_holdout_failed_not_budget_exhausted() {
    let fixture = Fixture::new();
    let backend = changing_worker(Arc::default());
    let (outcome, _store, _sha) =
        run_failing_with(&fixture, Vec::new(), true, Review::Off, 2, &backend);
    let Terminal::NeedsDecision { reason, .. } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert_eq!(*reason, Reason::HoldoutFailed);
    let recorded = holdout_failed_detail(&fixture, &outcome);
    assert!(
        recorded["gaps"]
            .as_array()
            .is_some_and(|gaps| !gaps.is_empty()),
        "the raising transition names the gaps that make `decide` refuse approve: {recorded}"
    );
    // The text `decide`'s refusal quotes (cli/decide.rs's holdout_failed test
    // uses this same format): it must keep naming the holdout and its test.
    assert!(
        recorded["gaps"][0]
            .as_str()
            .is_some_and(|gap| gap.contains("holdout `escaping` failed: h::escapes")),
        "{recorded}"
    );
    let states: Vec<State> = fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("transitions")
        .iter()
        .map(|transition| transition.to_state)
        .collect();
    assert!(states.contains(&State::Escalating), "{states:?}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_holdout_failed_stop_saves_no_verification_snapshot() {
    let fixture = Fixture::new();
    let backend = changing_worker(Arc::default());
    let (outcome, _store, _sha) =
        run_failing_with(&fixture, Vec::new(), true, Review::Off, 2, &backend);
    let Terminal::NeedsDecision { reason, .. } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert_eq!(*reason, Reason::HoldoutFailed);
    let refused = approve_from_snapshot(
        &fixture.ledger,
        &fixture.artifacts,
        &outcome.run_id,
        &DecisionAnswer {
            resolution: Reason::DecisionApproved,
            actor: Actor::Orchestrator,
            person_quote: None,
            recorded_via: RecordedVia::Cli,
            note: None,
            successor_run: None,
            from_state: State::NeedsDecision,
            to_state: State::Accepted,
        },
        &|| Ok(()),
    );
    // A holdout stop saves no snapshot, so this library call can only say
    // NoSnapshot. The refusal `relais decide --answer approve` really gives
    // (the holdout gap) is covered by the decide test
    // `approving_a_holdout_failed_stop_is_refused_naming_the_holdout_gap`.
    assert!(
        matches!(refused, Err(ApprovalError::NoSnapshot { .. })),
        "{refused:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_check_label_that_mentions_a_criterion_still_gets_the_log_hint() {
    let fixture = Fixture::new();
    let contract = holdout_contract(&fixture, true);
    let failures = vec![RepairFailure::Check(
        "lint (criterion: looks like a holdout)".to_string(),
    )];
    let (prompt, _) = build_prompt(
        &contract,
        &manifest_with(Vec::new()),
        1,
        Some(&failures),
        AttemptKind::Repair,
    );
    assert!(
        prompt.contains("  - lint (criterion: looks like a holdout) (log under logs/)\n"),
        "{prompt}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_holdout_failure_beside_a_failing_check_still_ends_failed() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(FAILING))]);
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let backend = MockBackend::new(move |spec| {
        std::fs::write(spec.work_dir.join("src/other.rs"), "// again\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome =
        fixture.execute_with_holdouts(settings, &holdout_contract(&fixture, true), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Failed { .. }),
        "{outcome:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn the_reviewer_is_given_each_holdout_as_name_outcome_and_test_ids_only() {
    let fixture = Fixture::new();
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let backend = counting_worker(Arc::default(), prompts.clone());
    let (outcome, store, _) =
        run_failing_with(&fixture, Vec::new(), false, Review::Required, 3, &backend);
    let prompts = prompts.lock().expect("lock");
    let reviewer = prompts
        .iter()
        .find(|prompt| prompt.contains("semantic reviewer"))
        .expect("a reviewer prompt");
    assert!(
        reviewer.contains("escaping: failed (h::escapes)"),
        "{reviewer}"
    );
    assert!(reviewer.contains(STATEMENT));
    let report_path = artifact_files(&fixture, &outcome)
        .iter()
        .map(|(path, _)| path.to_string_lossy().into_owned())
        .find(|path| {
            Path::new(path).ends_with(Path::new("holdouts").join("escaping").join("report.xml"))
        })
        .expect("the kept report");
    for hidden in [
        store.as_str(),
        report_path.as_str(),
        STORE_PATH_IN_STORE,
        OVERLAY,
        RUNNER_CONFIG,
        SCRIPT_MARKER,
        "log.txt",
    ] {
        assert!(
            !reviewer.contains(hidden),
            "the reviewer prompt leaks {hidden}"
        );
    }
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_failed_holdout_beside_another_gap_is_named_in_the_gap_stop() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(FAILING))]);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let mut contract = holdout_contract(&fixture, true);
    contract
        .acceptance
        .push(crate::acceptance::AcceptanceEntry::Declared(
            crate::acceptance::DeclaredCriterion {
                statement: "a person signs off".into(),
                id: None,
                mandatory: true,
                evidence: crate::acceptance::Evidence::HumanSignOff,
            },
        ));
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    let Terminal::NeedsDecision { reason, detail } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert_eq!(*reason, Reason::VerificationGap);
    assert!(
        detail.contains("holdout `escaping` failed: h::escapes"),
        "{detail}"
    );
    assert!(detail.contains("human sign-off"), "{detail}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_holdouts_output_is_never_quoted_by_the_exhaustion_detail() {
    let fixture = Fixture::new();
    let script = format!(
        "#!/bin/sh\necho 'No space left on device'\necho 'Cannot allocate memory'\n{}",
        writes(FAILING)
    );
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &script)]);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome =
        fixture.execute_with_holdouts(settings, &holdout_contract(&fixture, true), &repo, &backend);
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_ne!(reason_of_end(&fixture, &outcome), "resource_exhausted");
    let detail = outcome.detail();
    assert!(
        !detail.contains("No space left") && !detail.contains("Cannot allocate"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_holdout_failure_line_omits_the_log_hint_and_a_check_line_keeps_it() {
    let fixture = Fixture::new();
    let contract = holdout_contract(&fixture, true);
    let failures = vec![
        RepairFailure::Holdout(format!("a gap (criterion: {STATEMENT})")),
        RepairFailure::Check("make@1".to_string()),
        RepairFailure::Check("lint (criterion: looks like a holdout)".to_string()),
    ];
    let (prompt, _) = build_prompt(
        &contract,
        &manifest_with(Vec::new()),
        1,
        Some(&failures),
        AttemptKind::Repair,
    );
    assert!(
        prompt.contains(&format!("  - {}\n", failures[0].text())),
        "{prompt}"
    );
    assert!(
        prompt.contains("  - lint (criterion: looks like a holdout) (log under logs/)\n"),
        "{prompt}"
    );
    assert!(
        prompt.contains("  - make@1 (log under logs/)\n"),
        "{prompt}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

/// Commit `content` as the store's script and leave `main` on the new commit.
fn advance_store(fixture: &Fixture, content: &str) -> String {
    let store = fixture.dir.join("holdout-store");
    std::fs::write(store.join(STORE_PATH_IN_STORE), content).expect("write");
    git(&store, &["add", STORE_PATH_IN_STORE]);
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
            "test: store moves on",
        ],
    );
    git(&store, &["rev-parse", "main"]).trim().to_string()
}

/// The events of a run, as the file the pane reads holds them.
fn events_text(fixture: &Fixture, run: &RunId) -> String {
    std::fs::read_to_string(fixture.artifacts.join(run.as_str()).join("events.jsonl"))
        .expect("events")
}

/// A widening successor of a run waiting for a decision, claimed under a
/// run id, with its contract (one more holdout criterion when `added` names one).
fn claimed_successor(
    fixture: &Fixture,
    decided: &RunId,
    added: Option<&str>,
) -> (RunId, TaskContract) {
    use crate::ledger::DecisionAnswer;
    use crate::runner::successor;

    let (original, _) = fixture
        .ledger
        .run_contract_and_tier(decided)
        .expect("read")
        .expect("recorded");
    let paths = successor::out_of_scope_paths(&fixture.ledger, decided).expect("paths");
    let widened = successor::widened_contract(&original, decided, &paths).expect("widened");
    let mut widened_json = widened.canonical_value();
    if let Some(name) = added {
        widened_json["acceptance"]
            .as_array_mut()
            .expect("acceptance")
            .push(serde_json::json!({
                "statement": "the added holdout passes",
                "evidence": { "kind": "holdout", "name": name },
            }));
    }
    let widened = TaskContract::from_json_str(&widened_json.to_string()).expect("a contract");
    let intent = successor::plan(
        &fixture.ledger,
        decided,
        &widened,
        paths,
        &fixture.artifacts,
    )
    .expect("planned");
    let stored = fixture
        .ledger
        .resolve_decision_with_intent(
            decided,
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
    let contract = TaskContract::from_json_str(&stored.contract_json).expect("the contract");
    (new_run, contract)
}

#[test]
fn a_successor_is_judged_by_its_predecessors_frozen_holdout_and_resolves_a_new_name_fresh() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let (store, sha_a) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let contract = holdout_contract(&fixture, true);
    // The worker leaves its scope, so the run waits for a person.
    let leaving = MockBackend::new(|spec| {
        std::fs::create_dir_all(spec.work_dir.join("docs")).expect("mkdir");
        std::fs::write(spec.work_dir.join("docs/extra.md"), "one\n").expect("write");
        std::fs::write(spec.work_dir.join("src/main.rs"), "fn main() {}\n// e\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let decided = fixture
        .execute_with_holdouts(settings.clone(), &contract, &repo, &leaving)
        .run_id;
    let frozen = fixture
        .ledger
        .holdout_definitions(&decided)
        .expect("definitions");
    let [frozen] = frozen.as_slice() else {
        panic!("one frozen definition: {frozen:?}");
    };
    assert_eq!(frozen.sha, sha_a);

    // The person edits machine.toml and the store's ref moves on.
    let sha_b = advance_store(&fixture, &format!("{}# moved\n", writes(PASSING)));
    assert_ne!(sha_a, sha_b);
    let mut edited = settings_for(
        &fixture,
        &store,
        script_declaration(&["sh", "src/other_overlay.rs"], None),
    );
    let repo_name = crate::repo::main_checkout_name(&fixture.repo).expect("repo name");
    let mut changed = script_declaration(&["sh", "src/other_overlay.rs"], None);
    changed.overlay = "src/other_overlay.rs".into();
    let edited_declaration = changed.clone();
    let mut fresh = script_declaration(&["sh", "src/fresh_overlay.rs"], None);
    fresh.overlay = "src/fresh_overlay.rs".into();
    let declared = edited.repos.get_mut(&repo_name).expect("repo");
    declared.insert("escaping".into(), changed);
    declared.insert("fresh".into(), fresh);

    let (new_run, contract) = claimed_successor(&fixture, &decided, Some("fresh"));
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome =
        fixture.execute_with_holdouts_as(Some(new_run.clone()), edited, &contract, &repo, &backend);
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    let judged = |name: &str| {
        receipt
            .holdouts
            .iter()
            .find(|run| run.name == name)
            .unwrap_or_else(|| panic!("no {name} in {:?}", receipt.holdouts))
    };
    assert_eq!(judged("escaping").sha, sha_a, "the predecessor's commit");
    assert_eq!(judged("escaping").digest, frozen.digest);
    assert_eq!(judged("fresh").sha, sha_b, "a new name resolves fresh");

    let events = events_text(&fixture, &new_run);
    assert!(events.contains("holdout_definition_changed"), "{events}");
    let current_digest =
        crate::holdout::digest_of(&repo_name, "escaping", &store, &sha_a, &edited_declaration);
    assert_ne!(current_digest, frozen.digest);
    assert!(
        events.contains(&format!(
            "frozen digest {}, current digest {current_digest}",
            frozen.digest
        )),
        "{events}"
    );
    let inherited = fixture
        .ledger
        .holdout_definitions(&new_run)
        .expect("definitions");
    assert_eq!(
        inherited
            .iter()
            .find(|held| held.name == "escaping")
            .map(|held| held.digest.as_str()),
        Some(frozen.digest.as_str())
    );
    assert!(
        !events_text(&fixture, &decided).contains("holdout_definition_changed"),
        "the predecessor froze its own definition"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_successor_is_judged_by_the_frozen_definition_of_a_name_machine_toml_no_longer_declares() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let (store, sha_a) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let contract = holdout_contract(&fixture, true);
    let leaving = MockBackend::new(|spec| {
        std::fs::create_dir_all(spec.work_dir.join("docs")).expect("mkdir");
        std::fs::write(spec.work_dir.join("docs/extra.md"), "one\n").expect("write");
        std::fs::write(spec.work_dir.join("src/main.rs"), "fn main() {}\n// e\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let decided = fixture
        .execute_with_holdouts(settings, &contract, &repo, &leaving)
        .run_id;
    let frozen = fixture
        .ledger
        .holdout_definitions(&decided)
        .expect("definitions");
    let [frozen] = frozen.as_slice() else {
        panic!("one frozen definition: {frozen:?}");
    };

    // The person edits machine.toml so that it no longer declares `escaping`.
    let edited = HoldoutSettings {
        store: Some(store.clone()),
        ..HoldoutSettings::default()
    };
    let (new_run, contract) = claimed_successor(&fixture, &decided, None);
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome =
        fixture.execute_with_holdouts_as(Some(new_run.clone()), edited, &contract, &repo, &backend);
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    let judged = receipt
        .holdouts
        .iter()
        .find(|run| run.name == "escaping")
        .unwrap_or_else(|| panic!("no escaping in {:?}", receipt.holdouts));
    assert_eq!(judged.sha, sha_a, "the predecessor's commit");
    assert_eq!(judged.digest, frozen.digest);
    let events = events_text(&fixture, &new_run);
    assert!(
        events.contains(&format!(
            "frozen digest {}, current digest undeclared",
            frozen.digest
        )),
        "{events}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_successor_does_not_ask_the_store_again_for_a_frozen_holdout() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let (store, _sha) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let contract = holdout_contract(&fixture, true);
    // The worker leaves its scope, so the run waits for a person.
    let leaving = MockBackend::new(|spec| {
        std::fs::create_dir_all(spec.work_dir.join("docs")).expect("mkdir");
        std::fs::write(spec.work_dir.join("docs/extra.md"), "one\n").expect("write");
        std::fs::write(spec.work_dir.join("src/main.rs"), "fn main() {}\n// e\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let decided = fixture
        .execute_with_holdouts(settings.clone(), &contract, &repo, &leaving)
        .run_id;

    // The store disappears before the successor starts. Preflight must not
    // ask it again (that would refuse `holdout_unreadable`); the frozen
    // definition carries the run to the fetch, which is where a gone store
    // is reported.
    std::fs::remove_dir_all(fixture.dir.join("holdout-store")).expect("the store is removed");
    let (new_run, contract) = claimed_successor(&fixture, &decided, None);
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome =
        fixture.execute_with_holdouts_as(Some(new_run), settings, &contract, &repo, &backend);
    assert_eq!(blocked_code(outcome), BlockCode::HoldoutUnfetchable);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_verify_whose_candidate_wrote_the_overlay_ends_failed_with_the_collision_code() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    // The source run has no holdout, so its candidate may write the path.
    let writing = MockBackend::new(|spec| {
        std::fs::write(spec.work_dir.join(OVERLAY), "// planted\n").expect("write");
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
    } = fixture.execute(&fixture.contract(Review::Off), &repo, &writing)
    else {
        panic!("the source run is accepted");
    };
    let (store, _) = local_store(&fixture);
    let settings = settings_for(&fixture, &store, declaration(Some("j.xml"), OVERLAY));
    let verify_json = serde_json::json!({
        "schema_version": 1,
        "kind": "verify",
        "objective": "Verify the candidate on the current base",
        "base_ref": "HEAD",
        "write_scope": ["src/**"],
        "candidate": { "run": source.as_str() },
        "acceptance": [{
            "statement": STATEMENT,
            "evidence": { "kind": "holdout", "name": "escaping" },
        }],
        "verification_profile": "profile",
        "review": "off",
    });
    let contract =
        TaskContract::from_json_str(&verify_json.to_string()).expect("a verify contract");
    let launches = Arc::new(AtomicUsize::new(0));
    let backend = counting_worker(launches.clone(), Arc::default());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        reason_of_end(&fixture, &outcome),
        "holdout_overlay_collision"
    );
    assert_eq!(
        launches.load(Ordering::SeqCst),
        0,
        "a verify runs no worker"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_decomposed_run_whose_assembled_candidate_wrote_the_overlay_leaves_no_attempt_open() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let (store, _) = local_store(&fixture);
    // The overlay lies inside package b's scope, so only the assembled
    // candidate is checked against it.
    let overlay = "src/b/holdout_escaping.rs";
    let settings = settings_for(&fixture, &store, declaration(Some("j.xml"), overlay));
    let mut decomposed_json = holdout_contract(&fixture, true).canonical_value();
    decomposed_json["decomposition"] = plan_json(false);
    let contract =
        TaskContract::from_json_str(&decomposed_json.to_string()).expect("a decomposed contract");
    let backend = MockBackend::new(|spec| {
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
            ("src", "unexpected.rs")
        };
        std::fs::create_dir_all(spec.work_dir.join(dir)).expect("mkdir");
        std::fs::write(spec.work_dir.join(dir).join(file), "pub fn module() {}\n").expect("write");
        if spec.prompt.contains("work package `b`") {
            std::fs::write(
                spec.work_dir.join("src/b/holdout_escaping.rs"),
                "// planted\n",
            )
            .expect("write");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        reason_of_end(&fixture, &outcome),
        "holdout_overlay_collision"
    );
    let Terminal::Failed { detail } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert!(
        detail.contains("the assembled candidate wrote `src/b/holdout_escaping.rs`"),
        "{detail}"
    );
    let ledger = rusqlite::Connection::open(fixture.dir.join("ledger.sqlite")).expect("ledger");
    let unfinished: Vec<(String, i64, String)> = ledger
        .prepare("SELECT run_id, attempt_index, state FROM attempts WHERE ended_at IS NULL")
        .expect("prepare")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert!(unfinished.is_empty(), "{unfinished:?}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

/// A backend that runs as an agent with a transcript: each dispatch is
/// recorded as a native agent in the ledger, and `transcript_of` answers
/// with the given text.
struct TranscriptBackend {
    inner: MockBackend,
    ledger_path: std::path::PathBuf,
    transcript: Option<String>,
}

impl Harness for TranscriptBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn probe(&self) -> Option<crate::backend::Capabilities> {
        self.inner.probe()
    }
}

impl Backend for TranscriptBackend {
    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let launched = self.inner.launch(spec)?;
        let ledger = crate::ledger::Ledger::open(&self.ledger_path).expect("ledger");
        ledger
            .record_native_dispatch(
                &crate::ids::DispatchId::from_stored(spec.dispatch_id.clone()),
                Some("agent-1"),
            )
            .expect("the dispatch is recorded as an agent");
        Ok(launched)
    }

    fn transcript_of(&self, agent_id: &str) -> Option<String> {
        assert_eq!(agent_id, "agent-1");
        self.transcript.clone()
    }
}

fn holdout_counts(fixture: &Fixture) -> crate::report::HoldoutsReport {
    crate::report::runs_report(&fixture.ledger, "1970-01-01T00:00:00Z", None)
        .expect("a report")
        .holdouts
}

#[test]
fn a_failing_holdout_on_green_checks_is_one_catch_and_a_passing_one_is_none() {
    let failing = Fixture::new();
    let outcome = run_script(&failing, &writes(FAILING), None);
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let counts = holdout_counts(&failing);
    assert_eq!(counts.holdout_runs, 1);
    assert_eq!(counts.holdout_catches, 1);
    assert_eq!(counts.holdout_exposures.runs, 0);
    std::fs::remove_dir_all(&failing.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing

    let passing = Fixture::new();
    let outcome = run_script(&passing, &writes(PASSING), None);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let counts = holdout_counts(&passing);
    assert_eq!(counts.holdout_runs, 1);
    assert_eq!(counts.holdout_catches, 0);
    std::fs::remove_dir_all(&passing.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_holdout_that_did_not_build_on_green_checks_is_not_a_catch() {
    // A broken store or runner config is not a catch: it says nothing about
    // the candidate, and the pilot's keep bar must not count it.
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
    let counts = holdout_counts(&fixture);
    assert_eq!(counts.holdout_runs, 1);
    assert_eq!(counts.holdout_catches, 0);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_holdout_failing_beside_a_red_check_is_not_a_catch() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(FAILING))]);
    let repo = fixture.repo_policy(vec![main_gone_check()], 1);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let backend = MockBackend::new(|spec| {
        std::fs::write(spec.work_dir.join("src/other.rs"), "// again\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome =
        fixture.execute_with_holdouts(settings, &holdout_contract(&fixture, true), &repo, &backend);
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let counts = holdout_counts(&fixture);
    assert_eq!(counts.holdout_runs, 1);
    assert_eq!(counts.holdout_catches, 0, "the checks were not green");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_worker_transcript_that_reads_the_settings_file_is_one_exposure_by_marker() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let settings_path = crate::holdout::machine_toml_label();
    let transcript = serde_json::json!({
        "type": "assistant",
        "message": { "content": [{
            "type": "tool_use",
            "id": "toolu-1",
            "name": "Read",
            "input": { "file_path": settings_path },
        }] },
    })
    .to_string();
    let backend = TranscriptBackend {
        inner: counting_worker(Arc::default(), Arc::default()),
        ledger_path: fixture.dir.join("ledger.sqlite"),
        transcript: Some(transcript),
    };
    let outcome =
        fixture.execute_with_holdouts(settings, &holdout_contract(&fixture, true), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    let counts = holdout_counts(&fixture);
    assert_eq!(counts.holdout_exposures.runs, 1);
    assert_eq!(
        counts.holdout_exposures.by_marker.get("machine_toml"),
        Some(&1)
    );
    let stored: Vec<(String, String)> =
        rusqlite::Connection::open(fixture.dir.join("ledger.sqlite"))
            .expect("ledger")
            .prepare("SELECT tool, marker FROM holdout_exposures")
            .expect("prepare")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
    assert_eq!(stored, [("Read".to_string(), "machine_toml".to_string())]);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_package_workers_transcript_counts_as_an_exposure_on_the_root_run() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let mut decomposed_json = holdout_contract(&fixture, true).canonical_value();
    decomposed_json["decomposition"] = plan_json(false);
    let contract =
        TaskContract::from_json_str(&decomposed_json.to_string()).expect("a decomposed contract");
    let settings_path = crate::holdout::machine_toml_label();
    let transcript = serde_json::json!({
        "type": "assistant",
        "message": { "content": [{
            "type": "tool_use",
            "id": "toolu-1",
            "name": "Read",
            "input": { "file_path": settings_path },
        }] },
    })
    .to_string();
    let package_worker = MockBackend::new(|spec| {
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
        let dir = if spec.prompt.contains("work package `a`") {
            "src/a"
        } else {
            "src/b"
        };
        std::fs::create_dir_all(spec.work_dir.join(dir)).expect("mkdir");
        std::fs::write(
            spec.work_dir.join(dir).join("lib.rs"),
            "pub fn module() {}\n",
        )
        .expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let backend = TranscriptBackend {
        inner: package_worker,
        ledger_path: fixture.dir.join("ledger.sqlite"),
        transcript: Some(transcript),
    };
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    let from_package_runs: i64 = rusqlite::Connection::open(fixture.dir.join("ledger.sqlite"))
        .expect("ledger")
        .query_row(
            "SELECT COUNT(*) FROM holdout_exposures e
               JOIN dispatches d ON d.dispatch_id = e.dispatch_id
               JOIN runs package_run ON package_run.id = d.run_id
              WHERE e.run_id = ?1 AND package_run.parent_run = ?1",
            [outcome.run_id.as_str()],
            |row| row.get(0),
        )
        .expect("count");
    assert!(from_package_runs >= 1, "{outcome:?}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn an_unreadable_worker_transcript_is_no_exposure_and_says_so() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let backend = TranscriptBackend {
        inner: counting_worker(Arc::default(), Arc::default()),
        ledger_path: fixture.dir.join("ledger.sqlite"),
        transcript: None,
    };
    let outcome =
        fixture.execute_with_holdouts(settings, &holdout_contract(&fixture, true), &repo, &backend);
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(holdout_counts(&fixture).holdout_exposures.runs, 0);
    assert!(
        events_text(&fixture, &outcome.run_id).contains("holdout_exposure_unread"),
        "the unread transcript is a run event"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn the_report_json_carries_the_holdout_counts_under_holdouts() {
    let fixture = Fixture::new();
    let outcome = run_script(&fixture, &writes(FAILING), None);
    assert!(!matches!(outcome.terminal, Terminal::Accepted(_)));
    let report = crate::report::runs_report(&fixture.ledger, "1970-01-01T00:00:00Z", None)
        .expect("a report");
    let json = serde_json::to_value(&report).expect("json");
    assert_eq!(json["schema_version"], 16);
    assert_eq!(crate::report::REPORT_SCHEMA_VERSION, 16);
    assert_eq!(json["holdouts"]["holdout_runs"], 1);
    assert_eq!(json["holdouts"]["holdout_catches"], 1);
    assert_eq!(json["holdouts"]["holdout_exposures"]["runs"], 0);
    assert!(json["holdouts"]["holdout_exposures"]["by_marker"]
        .as_object()
        .expect("by marker")
        .is_empty());
    assert!(report.render().contains("holdout_catches: 1"));
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_verify_of_a_candidate_with_green_checks_and_a_failing_holdout_is_not_accepted() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let RunOutcome {
        run_id: source,
        terminal: Terminal::Accepted(_),
    } = fixture.execute(
        &fixture.contract(Review::Off),
        &repo,
        &counting_worker(Arc::default(), Arc::default()),
    )
    else {
        panic!("the source run is accepted");
    };
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(FAILING))]);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let verify_json = serde_json::json!({
        "schema_version": 1,
        "kind": "verify",
        "objective": "Verify the candidate on the current base",
        "base_ref": "HEAD",
        "write_scope": ["src/**"],
        "candidate": { "run": source.as_str() },
        "acceptance": [{
            "statement": STATEMENT,
            "evidence": { "kind": "holdout", "name": "escaping" },
        }],
        "verification_profile": "profile",
        "review": "off",
    });
    let contract =
        TaskContract::from_json_str(&verify_json.to_string()).expect("a verify contract");
    let launches = Arc::new(AtomicUsize::new(0));
    let backend = counting_worker(launches.clone(), Arc::default());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(
        launches.load(Ordering::SeqCst),
        0,
        "a verify runs no worker"
    );
    let counts = holdout_counts(&fixture);
    assert_eq!(counts.holdout_runs, 1);
    assert_eq!(counts.holdout_catches, 1);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

/// A worker that leaves its scope, so the run waits for a person.
fn scope_leaving_worker() -> MockBackend {
    MockBackend::new(|spec| {
        std::fs::create_dir_all(spec.work_dir.join("docs")).expect("mkdir");
        std::fs::write(spec.work_dir.join("docs/extra.md"), "one\n").expect("write");
        std::fs::write(spec.work_dir.join("src/main.rs"), "fn main() {}\n// e\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

/// Answer `decided` with a successor intent carrying `contract` (and
/// `review`), and claim it under a new run id.
fn claimed_intent(
    fixture: &Fixture,
    decided: &RunId,
    contract: &TaskContract,
    review: Option<crate::ledger::IntentReview>,
) -> (RunId, TaskContract) {
    use crate::runner::successor;

    let mut intent = successor::plan(
        &fixture.ledger,
        decided,
        contract,
        Vec::new(),
        &fixture.artifacts,
    )
    .expect("planned");
    intent.review = review;
    let stored = fixture
        .ledger
        .resolve_decision_with_intent(
            decided,
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
    let claimed = fixture
        .ledger
        .intent_of_run(&new_run)
        .expect("read")
        .expect("the claimed run reaches its intent");
    assert_eq!(claimed.review, review);
    (
        new_run,
        TaskContract::from_json_str(&stored.contract_json).expect("the contract"),
    )
}

/// Settings declaring `escaping` differently from the frozen definition.
fn edited_settings(fixture: &Fixture, store: &str) -> HoldoutSettings {
    let mut edited = settings_for(
        fixture,
        store,
        script_declaration(&["sh", "src/other_overlay.rs"], None),
    );
    let repo_name = crate::repo::main_checkout_name(&fixture.repo).expect("repo name");
    let declared = edited.repos.get_mut(&repo_name).expect("repo");
    let mut changed = script_declaration(&["sh", "src/other_overlay.rs"], None);
    changed.overlay = "src/other_overlay.rs".into();
    declared.insert("escaping".into(), changed);
    edited
}

#[test]
fn a_successor_of_each_kind_reaches_its_intent_and_is_judged_by_the_frozen_definition() {
    use crate::ledger::IntentReview;
    use crate::runner::successor;

    // rerun-verify and review-parts answer with the decided run's own
    // contract as a verify; successor-contract supplies one. The preconditions
    // the CLI checks for the first and last (resource exhaustion, a review
    // stop) concern the decided run's state, not what the successor is
    // judged by, so the intents are written directly.
    for kind in ["rerun-verify", "successor-contract", "review-parts"] {
        let fixture = Fixture::new();
        let repo = fixture.repo_policy(vec![passing_check()], 3);
        let (store, sha_a) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
        let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
        let decided = fixture
            .execute_with_holdouts(
                settings,
                &holdout_contract(&fixture, true),
                &repo,
                &scope_leaving_worker(),
            )
            .run_id;
        let frozen = fixture
            .ledger
            .holdout_definitions(&decided)
            .expect("definitions");
        let [frozen] = frozen.as_slice() else {
            panic!("{kind}: one frozen definition: {frozen:?}");
        };
        advance_store(&fixture, &format!("{}# moved\n", writes(PASSING)));

        let (original, _) = fixture
            .ledger
            .run_contract_and_tier(&decided)
            .expect("read")
            .expect("recorded");
        let rerun = successor::widened_contract(&original, &decided, &[]).expect("a verify");
        let (contract, review) = match kind {
            "successor-contract" => {
                let mut supplied = rerun.canonical_value();
                supplied["objective"] = "Verify the candidate again, by a supplied contract".into();
                (
                    TaskContract::from_json_str(&supplied.to_string()).expect("a contract"),
                    None,
                )
            }
            "review-parts" => (rerun, Some(IntentReview::Parts)),
            _ => (rerun, None),
        };
        let (new_run, contract) = claimed_intent(&fixture, &decided, &contract, review);
        let backend = counting_worker(Arc::default(), Arc::default());
        fixture.execute_with_holdouts_as(
            Some(new_run.clone()),
            edited_settings(&fixture, &store),
            &contract,
            &repo,
            &backend,
        );
        let inherited = fixture
            .ledger
            .holdout_definitions(&new_run)
            .expect("definitions");
        let [inherited] = inherited.as_slice() else {
            panic!("{kind}: one inherited definition: {inherited:?}");
        };
        assert_eq!(inherited.digest, frozen.digest, "{kind}");
        assert_eq!(inherited.sha, sha_a, "{kind}");
        assert!(
            events_text(&fixture, &new_run).contains("holdout_definition_changed"),
            "{kind}: the edited declaration is noticed and the frozen one judges"
        );
        std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
    }
}

#[test]
fn a_successor_of_a_successor_is_judged_by_the_first_runs_frozen_definition() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let (store, sha_a) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let settings = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let contract = holdout_contract(&fixture, true);
    let first = fixture
        .execute_with_holdouts(settings, &contract, &repo, &scope_leaving_worker())
        .run_id;
    let frozen = fixture
        .ledger
        .holdout_definitions(&first)
        .expect("definitions");
    let [frozen] = frozen.as_slice() else {
        panic!("one frozen definition: {frozen:?}");
    };
    advance_store(&fixture, &format!("{}# moved\n", writes(PASSING)));

    // The second run is a change again, supplied by contract, and leaves
    // its scope again, so it too waits for a person.
    let (second, second_contract) = claimed_intent(&fixture, &first, &contract, None);
    fixture.execute_with_holdouts_as(
        Some(second.clone()),
        edited_settings(&fixture, &store),
        &second_contract,
        &repo,
        &scope_leaving_worker(),
    );
    assert_eq!(
        fixture
            .ledger
            .holdout_definitions(&second)
            .expect("definitions")[0]
            .digest,
        frozen.digest
    );

    let (third, third_contract) = claimed_successor(&fixture, &second, None);
    let outcome = fixture.execute_with_holdouts_as(
        Some(third.clone()),
        edited_settings(&fixture, &store),
        &third_contract,
        &repo,
        &counting_worker(Arc::default(), Arc::default()),
    );
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    let [judged] = receipt.holdouts.as_slice() else {
        panic!("{:?}", receipt.holdouts);
    };
    assert_eq!(judged.sha, sha_a, "the first run's commit");
    assert_eq!(judged.digest, frozen.digest);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

fn attaching(declared: HoldoutDecl) -> HoldoutDecl {
    HoldoutDecl {
        attach: Some(crate::policy::HoldoutAttach {
            task_types: vec![crate::contract::TaskType::Feature],
            paths: vec!["src/**".into()],
        }),
        ..declared
    }
}

/// A feature contract scoped to `src/**` whose criteria are `acceptance`.
fn feature_contract(
    fixture: &Fixture,
    acceptance: Vec<crate::acceptance::AcceptanceEntry>,
) -> TaskContract {
    let mut contract = fixture.contract(Review::Off);
    contract.task_type = Some(crate::contract::TaskType::Feature);
    contract.acceptance = acceptance;
    contract
}

fn bare_acceptance() -> Vec<crate::acceptance::AcceptanceEntry> {
    vec!["src/main.rs no longer exists".into()]
}

fn attach_settings(fixture: &Fixture, script: &str) -> HoldoutSettings {
    let (store, _) = store_with(fixture, &[(STORE_PATH_IN_STORE, script)]);
    settings_for(
        fixture,
        &store,
        attaching(script_declaration(&["sh", OVERLAY], None)),
    )
}

#[test]
fn an_attached_holdout_runs_records_source_attached_and_settles_its_criterion() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = attach_settings(&fixture, &writes(PASSING));
    let contract = feature_contract(&fixture, bare_acceptance());
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    let [run] = receipt.holdouts.as_slice() else {
        panic!("{:?}", receipt.holdouts);
    };
    assert_eq!(run.name, "escaping");
    assert_eq!(run.source, Some(crate::holdout::HoldoutSource::Attached));
    let attached = receipt
        .criteria
        .iter()
        .find(|criterion| criterion.id == "attached:escaping")
        .expect("the attached criterion is on the receipt");
    assert!(attached.met);
    assert_eq!(
        attached.statement,
        "holdout escaping passes (attached by the machine declaration)"
    );
    assert_eq!(
        fixture
            .ledger
            .holdout_outcome_sources(&outcome.run_id)
            .expect("sources"),
        vec![("escaping".to_string(), "attached".to_string())]
    );
    let events = events_text(&fixture, &outcome.run_id);
    assert!(events.contains("holdout_attached"), "{events}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn plan_and_preflight_attach_the_same_holdout_names() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = attach_settings(&fixture, &writes(PASSING));
    let contract = feature_contract(&fixture, bare_acceptance());
    let planned: Vec<String> = crate::holdout::attachments(
        &settings,
        &crate::repo::main_checkout_name(&fixture.repo).unwrap_or_default(),
        &contract,
        contract.task_type,
    )
    .into_iter()
    .map(|attachment| attachment.name)
    .collect();
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    let preflight: Vec<String> = fixture
        .ledger
        .attached_holdout_definitions(&outcome.run_id)
        .expect("definitions")
        .into_iter()
        .map(|holdout| holdout.name)
        .collect();
    assert_eq!(planned, vec!["escaping".to_string()]);
    assert_eq!(planned, preflight);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn an_optional_named_criterion_still_attaches_and_a_failed_holdout_prevents_acceptance() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 1);
    let settings = attach_settings(&fixture, &writes(FAILING));
    let mut acceptance = bare_acceptance();
    acceptance.extend(holdout_contract(&fixture, false).acceptance);
    let contract = feature_contract(&fixture, acceptance);
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(
        fixture
            .ledger
            .holdout_outcome_sources(&outcome.run_id)
            .expect("sources"),
        vec![("escaping".to_string(), "attached".to_string())],
        "the holdout ran once"
    );
    assert!(
        outcome.detail().contains("attached:escaping"),
        "{}",
        outcome.detail()
    );

    let passing_settings = attach_settings(&fixture, &writes(PASSING));
    let passed = fixture.execute_with_holdouts(passing_settings, &contract, &repo, &backend);
    let Terminal::Accepted(receipt) = &passed.terminal else {
        panic!("{passed:?}");
    };
    let [run] = receipt.holdouts.as_slice() else {
        panic!("the holdout ran more than once: {:?}", receipt.holdouts);
    };
    assert_eq!(run.name, "escaping");
    assert_eq!(run.source, Some(crate::holdout::HoldoutSource::Attached));
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_successor_of_an_attached_run_runs_the_frozen_holdout_once_as_attached() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = attach_settings(&fixture, &writes(PASSING));
    let contract = feature_contract(&fixture, bare_acceptance());
    let leaving = MockBackend::new(|spec| {
        std::fs::create_dir_all(spec.work_dir.join("docs")).expect("mkdir");
        std::fs::write(spec.work_dir.join("docs/extra.md"), "one\n").expect("write");
        std::fs::write(spec.work_dir.join("src/main.rs"), "fn main() {}\n// e\n").expect("write");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let decided = fixture
        .execute_with_holdouts(settings.clone(), &contract, &repo, &leaving)
        .run_id;
    assert_eq!(
        fixture
            .ledger
            .attached_holdout_definitions(&decided)
            .expect("definitions")
            .len(),
        1
    );

    // The person removed `attach`: the successor still keeps the frozen holdout.
    let mut without_attach = settings.clone();
    let repo_name = crate::repo::main_checkout_name(&fixture.repo).expect("repo name");
    without_attach
        .repos
        .get_mut(&repo_name)
        .and_then(|declared| declared.get_mut("escaping"))
        .expect("the declaration")
        .attach = None;
    let (new_run, successor_contract) = claimed_successor(&fixture, &decided, None);
    let backend = counting_worker(Arc::default(), Arc::default());
    let outcome = fixture.execute_with_holdouts_as(
        Some(new_run.clone()),
        without_attach,
        &successor_contract,
        &repo,
        &backend,
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(
        fixture
            .ledger
            .holdout_outcome_sources(&new_run)
            .expect("sources"),
        vec![("escaping".to_string(), "attached".to_string())]
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_contract_criterion_with_the_attached_id_is_refused_before_any_worker() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = attach_settings(&fixture, &writes(PASSING));
    let mut acceptance = bare_acceptance();
    acceptance.push(crate::acceptance::AcceptanceEntry::Declared(
        crate::acceptance::DeclaredCriterion {
            statement: "the contract's own criterion".into(),
            id: Some("attached:escaping".into()),
            mandatory: true,
            evidence: crate::acceptance::Evidence::LlmReview,
        },
    ));
    let contract = feature_contract(&fixture, acceptance);
    let launches = Arc::new(AtomicUsize::new(0));
    let backend = counting_worker(launches.clone(), Arc::default());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    let detail = outcome.detail().to_string();
    assert_eq!(blocked_code(outcome), BlockCode::HoldoutCriterionCollision);
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert!(detail.contains("the contract's own criterion"), "{detail}");
    assert!(
        detail.contains("holdout escaping passes (attached by the machine declaration)"),
        "{detail}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_retry_prompt_for_an_attached_holdout_hides_machine_toml_and_decide_names_it() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = attach_settings(&fixture, &writes(FAILING));
    let contract = feature_contract(&fixture, bare_acceptance());
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let backend = counting_worker(Arc::default(), prompts.clone());
    let outcome = fixture.execute_with_holdouts(settings, &contract, &repo, &backend);
    let Terminal::NeedsDecision { detail, .. } = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert!(detail.contains("attached:escaping"), "{detail}");
    assert!(detail.contains("machine.toml"), "{detail}");
    let prompts = prompts.lock().expect("lock");
    let repair = prompts
        .iter()
        .find(|prompt| prompt.contains("[repair addendum]"))
        .expect("a repair prompt");
    assert!(repair.contains("h::escapes"), "{repair}");
    assert!(!repair.contains("attached:"), "{repair}");
    for prompt in prompts.iter() {
        assert!(!prompt.contains("machine.toml"), "{prompt}");
        assert!(!prompt.contains(".attach"), "{prompt}");
    }
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

fn named_declared_at(path: &str, overlay: &str) -> HoldoutDecl {
    HoldoutDecl {
        path: path.into(),
        overlay: overlay.into(),
        ..script_declaration(&["sh", overlay], None)
    }
}

fn attached_declared_at(path: &str, overlay: &str) -> HoldoutDecl {
    attaching(named_declared_at(path, overlay))
}

fn settings_with(
    fixture: &Fixture,
    store: &str,
    declared: Vec<(&str, HoldoutDecl)>,
) -> HoldoutSettings {
    let repo_name = crate::repo::main_checkout_name(&fixture.repo).expect("repo name");
    let mut settings = HoldoutSettings {
        store: Some(store.into()),
        ..HoldoutSettings::default()
    };
    for (name, decl) in declared {
        settings
            .repos
            .entry(repo_name.clone())
            .or_default()
            .insert(name.into(), decl);
    }
    settings
}

fn named_criterion(name: &str) -> crate::acceptance::AcceptanceEntry {
    crate::acceptance::AcceptanceEntry::Declared(crate::acceptance::DeclaredCriterion {
        statement: format!("the {name} holdout passes"),
        id: None,
        mandatory: true,
        evidence: crate::acceptance::Evidence::Holdout { name: name.into() },
    })
}

fn store_vanishing_worker(store_dir: std::path::PathBuf) -> MockBackend {
    MockBackend::new(move |spec| {
        ignore_not_found(std::fs::remove_file(spec.work_dir.join("src/main.rs")));
        ignore_not_found(std::fs::remove_dir_all(&store_dir));
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

#[test]
fn an_unreachable_store_skips_an_attached_holdout_and_blocks_for_a_named_one() {
    let fixture = Fixture::new();
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let store_dir = fixture.dir.join("holdout-store");
    let repo = fixture.repo_policy(vec![passing_check()], 3);

    let both = settings_with(
        &fixture,
        &store,
        vec![
            (
                "escaping",
                attached_declared_at(STORE_PATH_IN_STORE, OVERLAY),
            ),
            (
                "named",
                named_declared_at(STORE_PATH_IN_STORE, "src/holdout_named.rs"),
            ),
        ],
    );
    let mut acceptance = bare_acceptance();
    acceptance.push(named_criterion("named"));
    let outcome = fixture.execute_with_holdouts(
        both,
        &feature_contract(&fixture, acceptance),
        &repo,
        &store_vanishing_worker(store_dir.clone()),
    );
    assert_eq!(blocked_code(outcome), BlockCode::HoldoutUnfetchable);

    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let attached_only = settings_with(
        &fixture,
        &store,
        vec![(
            "escaping",
            attached_declared_at(STORE_PATH_IN_STORE, OVERLAY),
        )],
    );
    let outcome = fixture.execute_with_holdouts(
        attached_only,
        &feature_contract(&fixture, bare_acceptance()),
        &repo,
        &store_vanishing_worker(store_dir),
    );
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert!(receipt.holdouts.is_empty(), "{:?}", receipt.holdouts);
    assert!(receipt
        .criteria
        .iter()
        .all(|criterion| criterion.id != "attached:escaping"));
    let events = events_text(&fixture, &outcome.run_id);
    assert!(events.contains("holdout_attach_skipped"), "{events}");
    assert!(events.contains("escaping"), "{events}");
    assert_eq!(
        fixture
            .ledger
            .attached_holdout_definitions(&outcome.run_id)
            .expect("definitions")
            .len(),
        0,
        "a skipped attached holdout leaves no definition"
    );
    assert_eq!(
        fixture
            .ledger
            .skipped_attached_holdouts(&outcome.run_id)
            .expect("skipped"),
        vec!["escaping".to_string()]
    );
    assert_eq!(
        fixture
            .ledger
            .holdout_outcome_sources(&outcome.run_id)
            .expect("sources"),
        vec![("escaping".to_string(), "attached".to_string())]
    );
    assert!(!fixture
        .ledger
        .run_holdout_caught(&outcome.run_id)
        .expect("caught"));
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn a_store_unreachable_at_preflight_skips_an_attached_holdout_and_refuses_a_named_one() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let missing_store = format!("file://{}/no-such-store", fixture.dir.display());

    let mut acceptance = bare_acceptance();
    acceptance.push(named_criterion("named"));
    let both = settings_with(
        &fixture,
        &missing_store,
        vec![
            (
                "escaping",
                attached_declared_at(STORE_PATH_IN_STORE, OVERLAY),
            ),
            (
                "named",
                named_declared_at(STORE_PATH_IN_STORE, "src/holdout_named.rs"),
            ),
        ],
    );
    let outcome = fixture.execute_with_holdouts(
        both,
        &feature_contract(&fixture, acceptance),
        &repo,
        &MockBackend::new(|_| MockOutcome::default()),
    );
    assert_eq!(blocked_code(outcome), BlockCode::HoldoutUnreadable);

    let named_only = settings_with(
        &fixture,
        &missing_store,
        vec![(
            "named",
            named_declared_at(STORE_PATH_IN_STORE, "src/holdout_named.rs"),
        )],
    );
    let mut named_acceptance = bare_acceptance();
    named_acceptance.push(named_criterion("named"));
    let outcome = fixture.execute_with_holdouts(
        named_only,
        &feature_contract(&fixture, named_acceptance),
        &repo,
        &MockBackend::new(|_| MockOutcome::default()),
    );
    assert_eq!(blocked_code(outcome), BlockCode::HoldoutUnreadable);

    let attached_only = settings_with(
        &fixture,
        &missing_store,
        vec![(
            "escaping",
            attached_declared_at(STORE_PATH_IN_STORE, OVERLAY),
        )],
    );
    let outcome = fixture.execute_with_holdouts(
        attached_only,
        &feature_contract(&fixture, bare_acceptance()),
        &repo,
        &counting_worker(Arc::default(), Arc::default()),
    );
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert!(receipt
        .criteria
        .iter()
        .all(|criterion| criterion.id != "attached:escaping"));
    let events = events_text(&fixture, &outcome.run_id);
    assert!(events.contains("holdout_attach_skipped"), "{events}");
    assert!(
        fixture
            .ledger
            .holdout_definitions(&outcome.run_id)
            .expect("definitions")
            .is_empty(),
        "a skipped holdout leaves no definition"
    );
    assert_eq!(
        fixture
            .ledger
            .skipped_attached_holdouts(&outcome.run_id)
            .expect("skipped"),
        vec!["escaping".to_string()]
    );
    assert_eq!(
        fixture
            .ledger
            .holdout_outcome_sources(&outcome.run_id)
            .expect("sources"),
        vec![("escaping".to_string(), "attached".to_string())]
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

fn report_named(test: &str) -> String {
    format!(
        r#"<testsuite tests="1" failures="0"><testcase classname="{test}" name="t"/></testsuite>"#
    )
}

fn script_writing(report: &str, before: &str) -> String {
    format!("#!/bin/sh\n# {SCRIPT_MARKER}\n{before}\ncat > j.xml <<'EOF'\n{report}\nEOF\n")
}

fn two_named(
    fixture: &Fixture,
    first_script: &str,
    second_script: &str,
    second_overlay: &str,
) -> RunOutcome {
    let (store, _) = store_with(fixture, &[("a.sh", first_script), ("b.sh", second_script)]);
    let repo = fixture.repo_policy(vec![passing_check()], 3);
    let settings = settings_with(
        fixture,
        &store,
        vec![
            ("first", named_declared_at("a.sh", OVERLAY)),
            ("second", named_declared_at("b.sh", second_overlay)),
        ],
    );
    let mut contract = fixture.contract(Review::Off);
    contract.acceptance = vec![named_criterion("first"), named_criterion("second")];
    fixture.execute_with_holdouts(
        settings,
        &contract,
        &repo,
        &counting_worker(Arc::default(), Arc::default()),
    )
}

#[test]
fn holdouts_run_side_by_side_and_the_receipt_carries_each_wall_duration() {
    let fixture = Fixture::new();
    // Each script waits (at most 20 s) for the other's marker, which only
    // appears if both are running at the same time.
    let meets = |mine: &str, theirs: &str, test: &str| {
        let dir = fixture.dir.display();
        script_writing(
            &report_named(test),
            &format!(
                "touch '{dir}/{mine}.up'\nfor _ in $(seq 200); do [ -e '{dir}/{theirs}.up' ] && break; sleep 0.1; done\n[ -e '{dir}/{theirs}.up' ] || exit 1"
            ),
        )
    };
    let outcome = two_named(
        &fixture,
        &meets("a", "b", "a"),
        &meets("b", "a", "b"),
        "src/holdout_second.rs",
    );
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    assert_eq!(receipt.holdouts.len(), 2);
    for run in &receipt.holdouts {
        assert!(run.wall_ms.is_some(), "{} has no wall duration", run.name);
    }
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}

#[test]
fn two_holdouts_overlaying_one_path_each_see_only_their_own_files() {
    let fixture = Fixture::new();
    let outcome = two_named(
        &fixture,
        &script_writing(
            &report_named("a"),
            "grep -q 'writes a' \"$0\" || exit 1\n# writes a",
        ),
        &script_writing(
            &report_named("b"),
            "grep -q 'writes b' \"$0\" || exit 1\n# writes b",
        ),
        OVERLAY,
    );
    let Terminal::Accepted(receipt) = &outcome.terminal else {
        panic!("{outcome:?}");
    };
    let tests_of = |name: &str| -> Vec<String> {
        receipt
            .holdouts
            .iter()
            .find(|run| run.name == name)
            .map(|run| run.tests.iter().map(|test| test.id.clone()).collect())
            .unwrap_or_default()
    };
    assert_eq!(tests_of("first"), vec!["a::t".to_string()]);
    assert_eq!(tests_of("second"), vec!["b::t".to_string()]);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup; a leftover dir fails nothing
}
