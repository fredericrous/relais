//! Holdout criteria at preflight and at verification (ADR-0045 phase 2).

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
    let store = fixture.dir.join("holdout-store");
    std::fs::create_dir_all(&store).expect("store dir");
    git(&store, &["init", "-q", "-b", "main"]);
    std::fs::write(store.join("README"), "holdouts\n").expect("write");
    git(&store, &["add", "README"]);
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
