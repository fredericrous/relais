// -- passed checks carried to an unchanged candidate -----------------------
//
// docs/plans/2026-11-23-carry-passed-checks-unchanged-sha.md: a repair that
// returns the sha the previous attempt verified runs again the commands up
// to the last one that failed there, and carries the passes after it.
//
// Each command counts its own runs in a file outside the tree, and fails
// when it runs in the verification worktree of an attempt the test names
// (`verify-<n>`): the base run, which happens elsewhere, never fails.

use std::sync::atomic::AtomicUsize;

use super::holdouts::{
    holdout_contract, script_declaration, settings_for, store_with, writes, OVERLAY, PASSING,
    STORE_PATH_IN_STORE,
};
use super::*;

const LOW: &str = "FINDINGS:\n1. [low] src/lib.rs:1 stale comment. Fix.\n";

/// A command that counts its runs in `<dir>/counts/<name>` and fails in
/// the verification worktrees of the listed attempts.
fn counted(fixture: &Fixture, name: &str, failing_attempts: &[u32]) -> CommandSpec {
    let counts = fixture.dir.join("counts");
    std::fs::create_dir_all(&counts).expect("counts dir");
    let failing: String = failing_attempts
        .iter()
        .map(|attempt| format!("*verify-{attempt}) exit 1;; "))
        .collect();
    sh(&format!(
        "echo x >> '{}'; case \"$(pwd)\" in {failing}esac",
        counts.join(name).display()
    ))
}

/// A command that counts its runs in `<dir>/counts/<name>` and fails while
/// it has run at most `failing_runs` times: with `1`, only its first
/// invocation fails (the base run, on demand, is the second).
fn failing_first_runs(fixture: &Fixture, name: &str, failing_runs: usize) -> CommandSpec {
    let counts = fixture.dir.join("counts");
    std::fs::create_dir_all(&counts).expect("counts dir");
    let file = counts.join(name);
    sh(&format!(
        "echo x >> '{0}'; [ \"$(wc -l < '{0}')\" -gt {failing_runs} ]",
        file.display()
    ))
}

fn runs_of(fixture: &Fixture, name: &str) -> usize {
    std::fs::read_to_string(fixture.dir.join("counts").join(name))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// What a later attempt does to the tree its first attempt changed.
#[derive(Clone, Copy, PartialEq)]
enum Repair {
    /// Leaves it as it is: the candidate is unchanged.
    KeepsTree,
    /// Changes it again: another candidate.
    ChangesTree,
}

/// A worker whose first attempt writes `src/change.txt`; a later attempt
/// does what `repair` says. A reviewer answers `answers[n]` to its n-th
/// review, then `FINDINGS: none`.
fn worker(repair: Repair, answers: &'static [&'static str]) -> MockBackend {
    let reviews = AtomicUsize::new(0);
    MockBackend::new(move |spec| {
        if spec.prompt.contains("semantic reviewer") {
            let n = reviews.fetch_add(1, Ordering::SeqCst);
            return MockOutcome {
                result_text: Some(answers.get(n).copied().unwrap_or("FINDINGS: none").into()),
                exit_code: Some(0),
                usage: Some(usage(10)),
                ..Default::default()
            };
        }
        let change = spec.work_dir.join("src/change.txt");
        if repair == Repair::ChangesTree || !change.exists() {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&change)
                .expect("change file");
            writeln!(file, "change").expect("write");
        }
        MockOutcome {
            result_text: Some("DONE".into()),
            exit_code: Some(0),
            usage: Some(usage(100)),
            ..Default::default()
        }
    })
}

fn events_of(fixture: &Fixture, run: &RunId) -> Vec<serde_json::Value> {
    std::fs::read_to_string(fixture.artifacts.join(run.as_str()).join("events.jsonl"))
        .expect("events.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json")["event"].take())
        .collect()
}

fn kinds<'a>(events: &'a [serde_json::Value], kind: &str) -> Vec<&'a serde_json::Value> {
    events
        .iter()
        .filter(|event| event["kind"] == kind)
        .collect()
}

fn decisions<'a>(events: &'a [serde_json::Value], what: &str) -> Vec<&'a serde_json::Value> {
    events
        .iter()
        .filter(|event| event["kind"] == "decision" && event["what"] == what)
        .collect()
}

fn accepted(outcome: RunOutcome) -> (RunId, crate::verify::Receipt) {
    let RunOutcome {
        run_id,
        terminal: Terminal::Accepted(receipt),
    } = outcome
    else {
        panic!("expected acceptance, got {outcome:?}");
    };
    (run_id, *receipt)
}

#[test]
fn a_repair_on_the_same_sha_runs_the_failed_command_and_carries_the_pass_after_it() {
    let fixture = Fixture::new();
    let first = failing_first_runs(&fixture, "first", 1);
    let second = counted(&fixture, "second", &[]);
    let repo = fixture.repo_policy(vec![first, second], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Optional),
        &repo,
        &worker(Repair::KeepsTree, &[]),
    );
    let (run_id, receipt) = accepted(outcome);
    assert_eq!(receipt.attempts, 2);
    assert_eq!(runs_of(&fixture, "second"), 1, "the pass was not run again");
    let checks = &receipt.verification.checks;
    assert_eq!(checks.len(), 2, "{checks:?}");
    assert_eq!(checks[0].carried_from, None);
    assert_eq!(checks[1].carried_from.as_deref(), Some("attempt1"));
    assert!(checks[1].timing.as_ref().expect("timing").cached);
    let rows = fixture
        .ledger
        .check_timing_carries(&run_id)
        .expect("timing rows");
    let carried_row = rows
        .iter()
        .find(|(phase, label, _)| phase == "attempt2" && *label == checks[1].label)
        .expect("a row under the carrying attempt");
    assert_eq!(carried_row.2.as_deref(), Some("attempt1"));
    let own_row = rows
        .iter()
        .find(|(phase, label, _)| phase == "attempt2" && *label == checks[0].label)
        .expect("the re-run command's row");
    assert_eq!(own_row.2, None);
    let events = events_of(&fixture, &run_id);
    let carried = kinds(&events, "check_carried");
    assert_eq!(carried.len(), 1, "{carried:?}");
    assert_eq!(carried[0]["label"], checks[1].label.as_str());
    assert_eq!(carried[0]["from"], "attempt1");
    assert_eq!(decisions(&events, "checks_carried").len(), 1);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn a_repair_that_changes_the_sha_runs_every_command_and_carries_nothing() {
    let fixture = Fixture::new();
    let first = counted(&fixture, "first", &[1]);
    let second = counted(&fixture, "second", &[]);
    let repo = fixture.repo_policy(vec![first, second], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Optional),
        &repo,
        &worker(Repair::ChangesTree, &[]),
    );
    let (run_id, receipt) = accepted(outcome);
    assert_eq!(runs_of(&fixture, "second"), 2);
    assert!(receipt
        .verification
        .checks
        .iter()
        .all(|check| check.carried_from.is_none()));
    let events = events_of(&fixture, &run_id);
    assert!(kinds(&events, "check_carried").is_empty());
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn a_later_failure_on_the_same_sha_runs_both_commands_again() {
    let fixture = Fixture::new();
    let make = counted(&fixture, "make", &[]);
    let test = counted(&fixture, "test", &[1]);
    let repo = fixture.repo_policy(vec![make, test], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Optional),
        &repo,
        &worker(Repair::KeepsTree, &[]),
    );
    let (run_id, receipt) = accepted(outcome);
    assert_eq!(runs_of(&fixture, "make"), 2, "the earlier pass runs again");
    assert!(receipt
        .verification
        .checks
        .iter()
        .all(|check| check.carried_from.is_none()));
    let events = events_of(&fixture, &run_id);
    assert!(kinds(&events, "check_carried").is_empty());
    assert!(decisions(&events, "checks_carried").is_empty());
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn an_in_place_run_runs_every_command_and_says_why() {
    use crate::workspace::reuse::Workspace;
    let fixture = fixture_in_a_linked_worktree();
    let first = counted(&fixture, "first", &[1]);
    let second = counted(&fixture, "second", &[]);
    let repo = fixture.repo_policy_with_setup(Vec::new(), vec![first, second], 3);
    let machine = fixture.machine_for(&repo);
    let outcome = fixture.execute_in(
        Workspace::InPlace,
        &fixture.contract(Review::Optional),
        &repo,
        &machine,
        &worker(Repair::KeepsTree, &[]),
    );
    let events = events_of(&fixture, &outcome.run_id);
    assert_eq!(runs_of(&fixture, "second"), 2, "{outcome:?}");
    assert!(kinds(&events, "check_carried").is_empty());
    let refused = decisions(&events, "checks_not_carried");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(
        refused[0]["reason"]
            .as_str()
            .expect("reason")
            .contains("in-place"),
        "{refused:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn in_a_chain_on_one_sha_a_carried_pass_names_the_attempt_that_ran_it() {
    let fixture = Fixture::new();
    let lint = counted(&fixture, "lint", &[2]);
    let make = counted(&fixture, "make", &[1]);
    let test = counted(&fixture, "test", &[]);
    let repo = fixture.repo_policy(vec![lint, make, test], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Optional),
        &repo,
        &worker(Repair::KeepsTree, &[]),
    );
    let (_, receipt) = accepted(outcome);
    assert_eq!(receipt.attempts, 3);
    let checks = &receipt.verification.checks;
    assert_eq!(checks[0].carried_from, None);
    assert_eq!(checks[1].carried_from.as_deref(), Some("attempt2"));
    assert_eq!(checks[2].carried_from.as_deref(), Some("attempt1"));
    assert_eq!(runs_of(&fixture, "test"), 1);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn the_same_failure_on_an_unchanged_sha_still_ends_the_run() {
    let fixture = Fixture::new();
    let first = counted(&fixture, "first", &[1, 2]);
    let second = counted(&fixture, "second", &[]);
    let repo = fixture.repo_policy(vec![first, second], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Optional),
        &repo,
        &worker(Repair::KeepsTree, &[]),
    );
    let transitions = fixture
        .ledger
        .transitions(&outcome.run_id)
        .expect("history");
    assert_eq!(
        ending(&transitions).reason,
        Reason::SameFailureRecurrence.as_str(),
        "{outcome:?}"
    );
    assert_eq!(runs_of(&fixture, "second"), 1);
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn a_review_sending_back_an_all_green_sha_runs_no_command_but_makes_the_tree_and_runs_the_holdouts()
{
    let fixture = Fixture::new();
    let setup = counted(&fixture, "setup", &[]);
    let make = counted(&fixture, "make", &[]);
    let test = counted(&fixture, "test", &[]);
    let repo = fixture.repo_policy_with_setup(vec![setup], vec![make, test], 3);
    let (store, _) = store_with(&fixture, &[(STORE_PATH_IN_STORE, &writes(PASSING))]);
    let holdouts = settings_for(&fixture, &store, script_declaration(&["sh", OVERLAY], None));
    let mut contract = holdout_contract(&fixture, true);
    contract.review = Review::Required;
    let outcome = fixture.execute_with_holdouts(
        holdouts,
        &contract,
        &repo,
        &worker(Repair::KeepsTree, &[LOW]),
    );
    let (run_id, receipt) = accepted(outcome);
    assert_eq!(receipt.attempts, 2);
    assert_eq!(
        (runs_of(&fixture, "make"), runs_of(&fixture, "test")),
        (1, 1),
        "no command runs again"
    );
    assert!(
        receipt
            .verification
            .checks
            .iter()
            .all(|check| check.carried_from.as_deref() == Some("attempt1")),
        "{:?}",
        receipt.verification.checks
    );
    let events = events_of(&fixture, &run_id);
    assert_eq!(kinds(&events, "check_carried").len(), 2);
    let phases = fixture.ledger.check_timing_phases(&run_id).expect("rows");
    assert!(
        phases.iter().any(|(phase, _)| phase == "attempt2-setup"),
        "the setup still ran in attempt 2's own tree: {phases:?}"
    );
    let holdout_rows = fixture
        .ledger
        .holdout_outcome_sources(&run_id)
        .expect("holdout rows");
    assert_eq!(
        holdout_rows.len(),
        2,
        "the holdout ran on both attempts: {holdout_rows:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}

#[test]
fn in_a_carry_the_rerun_command_records_its_junit_report_and_the_carried_one_does_not() {
    let fixture = Fixture::new();
    let report =
        "printf '<testsuite name=\"s\" tests=\"1\" failures=\"0\"><testcase classname=\"c\" \
                  name=\"t\"/></testsuite>' > report.xml; ";
    let with_report = |command: CommandSpec| {
        let script = format!("{report}{}", command.argv[2]);
        CommandSpec {
            junit: Some("report.xml".into()),
            ..sh(&script)
        }
    };
    let first = with_report(counted(&fixture, "first", &[1]));
    let second = with_report(counted(&fixture, "second", &[]));
    let repo = fixture.repo_policy(vec![first, second], 3);
    let outcome = fixture.execute(
        &fixture.contract(Review::Optional),
        &repo,
        &worker(Repair::KeepsTree, &[]),
    );
    let (run_id, _) = accepted(outcome);
    let reports: Vec<String> = fixture
        .ledger
        .evidence(&run_id)
        .expect("evidence")
        .into_iter()
        .filter(|row| row.kind == EvidenceKind::JunitReport)
        .map(|row| row.path)
        .collect();
    assert_eq!(
        reports.len(),
        4,
        "attempt 1 twice, the base, attempt 2 once: {reports:?}"
    );
    assert!(
        reports.iter().any(|path| path.contains("attempt2-cmd0")),
        "{reports:?}"
    );
    assert!(
        !reports.iter().any(|path| path.contains("attempt2-cmd1")),
        "{reports:?}"
    );
    std::fs::remove_dir_all(&fixture.dir).ok(); // best-effort temp cleanup
}
