// -- a watched home file that changes during a run is a warning (SPEC §8) -----
//
// The fingerprint never changes how a run ends: it adds evidence rows.

use super::*;
use crate::homeprint::{Change, Drift};
use crate::ledger::EvidenceKind;

/// A machine whose home directory is a scratch one, with `.gitconfig` in it.
fn machine_with_home(fixture: &Fixture, repo: &RepoPolicy) -> (MachineSettings, PathBuf) {
    let home = fixture.dir.join("home");
    std::fs::create_dir_all(&home).expect("mkdir");
    std::fs::write(home.join(".gitconfig"), "[user]\nname = before\n").expect("write");
    let mut machine = fixture.machine_for(repo);
    machine.fingerprint.home_override = Some(home.clone());
    (machine, home)
}

/// A worker that deletes `src/main.rs`, as the contract asks, and writes
/// `.gitconfig` in the fake home when `leak` is set.
fn worker(leak: Option<PathBuf>) -> MockBackend {
    MockBackend::new(move |spec| {
        if let Some(target) = &leak {
            std::fs::write(target, "[user]\nname = after\n").expect("leak");
        }
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("worker completes");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

fn drifts_of(fixture: &Fixture, outcome: &RunOutcome) -> Vec<Drift> {
    fixture
        .ledger
        .evidence(&outcome.run_id)
        .expect("evidence")
        .into_iter()
        .filter(|row| row.kind == EvidenceKind::HomeDrift)
        .map(|row| {
            let body = std::fs::read_to_string(&row.path).expect("the evidence file exists");
            serde_json::from_str(&body).expect("a drift record")
        })
        .collect()
}

#[test]
fn a_worker_writing_a_watched_home_file_leaves_a_drift_row_and_the_run_is_accepted() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (machine, home) = machine_with_home(&fixture, &repo);
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(Some(home.join(".gitconfig"))),
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "drift never changes the outcome: {outcome:?}"
    );
    if cfg!(unix) {
        assert_eq!(
            drifts_of(&fixture, &outcome),
            vec![Drift {
                path: "~/.gitconfig".into(),
                change: Change::Modified,
            }]
        );
    } else {
        assert_eq!(drifts_of(&fixture, &outcome).len(), 1);
    }
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn a_run_that_writes_nothing_in_the_home_leaves_no_drift_row() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (machine, _home) = machine_with_home(&fixture, &repo);
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(None),
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(drifts_of(&fixture, &outcome), Vec::new());
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn a_disabled_fingerprint_records_nothing() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (mut machine, home) = machine_with_home(&fixture, &repo);
    machine.fingerprint.enabled = false;
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(Some(home.join(".gitconfig"))),
    );
    assert!(
        matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    assert_eq!(drifts_of(&fixture, &outcome), Vec::new());
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}
