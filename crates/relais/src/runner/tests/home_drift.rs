// -- a watched home file that changes during a run is a warning (SPEC §8) -----
//
// The fingerprint never changes how a run ends: it adds evidence rows.

use super::*;
use crate::homeprint::{Change, Drift};
use crate::ledger::EvidenceKind;
use crate::policy::MachineSettings;

/// A machine whose home directory is a scratch one, with `.gitconfig` in it.
fn machine_with_home(fixture: &Fixture, repo: &RepoPolicy) -> (MachineSettings, PathBuf) {
    let home = fixture.dir.join("home");
    std::fs::create_dir_all(&home).expect("mkdir");
    std::fs::write(home.join(".gitconfig"), "[user]\nname = before\n").expect("write");
    (fixture.machine_for(repo), home)
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
    let (drifts, problems) = crate::runner::drifts_of_run(&fixture.ledger, &outcome.run_id);
    assert_eq!(problems, Vec::<String>::new());
    drifts
}

fn modified_gitconfig() -> Drift {
    Drift {
        path: "~/.gitconfig".into(),
        change: Change::Modified,
    }
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

#[test]
fn a_baseline_check_writing_a_watched_home_file_leaves_one_drift_row() {
    let fixture = Fixture::new();
    // The baseline is checked only when the candidate is red, so the
    // check always fails; it writes the home file only while src/main.rs
    // exists, which is the base revision, not the candidate.
    let home = fixture.dir.join("home");
    let writes_at_the_base = sh(&format!(
        "if [ -f src/main.rs ]; then printf changed > '{}'; fi; exit 1",
        home.join(".gitconfig").display()
    ));
    let repo = fixture.repo_policy(vec![writes_at_the_base], 1);
    let (machine, _home) = machine_with_home(&fixture, &repo);
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(None),
    );
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "{outcome:?}"
    );
    if cfg!(unix) {
        assert_eq!(drifts_of(&fixture, &outcome), vec![modified_gitconfig()]);
    } else {
        assert_eq!(drifts_of(&fixture, &outcome).len(), 1);
    }
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn a_superseded_run_still_records_the_home_drift() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (machine, home) = machine_with_home(&fixture, &repo);
    let ledger_path = fixture.dir.join("ledger.sqlite");
    let leak = home.join(".gitconfig");
    // The worker leaks into the home, then another process reaps the run.
    let backend = MockBackend::new(move |spec| {
        std::fs::write(&leak, "[user]\nname = after\n").expect("leak");
        let connection = rusqlite::Connection::open(&ledger_path).expect("open ledger");
        let run: String = connection
            .query_row("SELECT id FROM runs", [], |row| row.get(0))
            .expect("the run exists");
        let reaper = Ledger::open(&ledger_path).expect("second connection");
        reaper
            .record_transition(&crate::ledger::Transition {
                run_id: crate::ids::RunId::from_stored(&run),
                attempt_id: None,
                from_state: Some(State::Running),
                to_state: State::Interrupted,
                reason: Reason::DriverGone.as_str().to_string(),
                detail: None,
                at: reaper.now(),
            })
            .expect("reap");
        std::fs::remove_file(spec.work_dir.join("src/main.rs")).expect("worker completes");
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    });
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &backend,
    );
    assert!(
        !matches!(outcome.terminal, Terminal::Accepted(_)),
        "the reaped run is not accepted: {outcome:?}"
    );
    if cfg!(unix) {
        assert_eq!(drifts_of(&fixture, &outcome), vec![modified_gitconfig()]);
    } else {
        assert_eq!(drifts_of(&fixture, &outcome).len(), 1);
    }
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn the_status_report_ends_with_the_follow_up() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (machine, home) = machine_with_home(&fixture, &repo);
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(Some(home.join(".gitconfig"))),
    );
    let (drifts, problems) = crate::runner::drifts_of_run(&fixture.ledger, &outcome.run_id);
    assert_eq!(problems, Vec::<String>::new());
    let heading = crate::homeprint::STATUS_HEADING;
    let status = crate::homeprint::report(&drifts, heading);
    assert_eq!(status.first().map(String::as_str), Some(heading));
    assert_eq!(
        status.last().map(String::as_str),
        Some(crate::homeprint::FOLLOW_UP[1])
    );
    // The fixed lines fit 80 columns; a path line is as long as its path
    // (absolute on Windows).
    assert!(
        std::iter::once(heading)
            .chain(crate::homeprint::FOLLOW_UP)
            .all(|line| line.chars().count() <= 80),
        "{status:?}"
    );
    assert_eq!(status.len(), 4, "{status:?}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn an_unreadable_home_record_is_named_not_skipped() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (machine, home) = machine_with_home(&fixture, &repo);
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(Some(home.join(".gitconfig"))),
    );
    let rows = fixture.ledger.evidence(&outcome.run_id).expect("evidence");
    let row = rows
        .iter()
        .find(|row| row.kind == EvidenceKind::HomeDrift)
        .expect("a drift row");
    std::fs::remove_file(&row.path).expect("remove the record");
    let (drifts, problems) = crate::runner::drifts_of_run(&fixture.ledger, &outcome.run_id);
    assert_eq!(drifts, Vec::new());
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!("home drift record {} unreadable: ", row.path)),
        "{problems:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn an_extra_path_under_tilde_is_watched_under_the_machines_home() {
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (mut machine, home) = machine_with_home(&fixture, &repo);
    std::fs::create_dir_all(home.join("tool")).expect("mkdir");
    std::fs::write(home.join("tool").join("a.toml"), "before").expect("write");
    machine.fingerprint.extra_paths = vec![PathBuf::from("~/tool")];
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(Some(home.join("tool").join("a.toml"))),
    );
    let drifts = drifts_of(&fixture, &outcome);
    assert_eq!(drifts.len(), 1, "{drifts:?}");
    assert_eq!(drifts[0].change, Change::Modified);
    assert!(
        Path::new(&drifts[0].path).ends_with(Path::new("tool").join("a.toml")),
        "{drifts:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn a_relative_extra_path_is_refused_when_machine_toml_loads() {
    let error = MachineSettings::from_toml_str(
        "schema_version = 1\n[fingerprint]\nextra_paths = [\"tool/config\"]\n",
    )
    .expect_err("a relative extra path is refused");
    let text = error.to_string();
    assert!(text.contains("[fingerprint] extra_paths"), "{text}");
    assert!(text.contains("tool/config"), "{text}");
    let absolute = if cfg!(windows) { "C:/abs" } else { "/abs" };
    let accepted = MachineSettings::from_toml_str(&format!(
        "schema_version = 1\n[fingerprint]\nextra_paths = [\"~/tool\", \"{absolute}\"]\n"
    ));
    assert!(accepted.is_ok(), "{accepted:?}");
}

#[cfg(unix)]
#[test]
fn a_path_unreadable_before_and_after_prints_the_not_watched_line() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let repo = fixture.repo_policy(vec![main_gone_check()], 3);
    let (mut machine, home) = machine_with_home(&fixture, &repo);
    let locked = home.join("locked");
    std::fs::create_dir_all(&locked).expect("mkdir");
    machine.fingerprint.extra_paths = vec![locked.clone()];
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read_dir(&locked).is_ok() {
        // A user that ignores modes (root): the directory is readable.
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).ok(); // cleanup only
        return;
    }
    let outcome = fixture.execute_with_machine(
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(None),
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).ok(); // so the cleanup below can remove it
    let drifts = drifts_of(&fixture, &outcome);
    let lines = crate::homeprint::report(&drifts, "heading");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("not watched (unreadable): "),
        "{lines:?}"
    );
    assert!(lines[0].ends_with("locked"), "{lines:?}");
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}
