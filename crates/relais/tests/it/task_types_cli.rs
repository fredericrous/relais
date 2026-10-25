//! `relais dataset export-tasks` and `import-task-types` through the real
//! binary (docs/plans/2026-10-12-task-types.md, Phase 1): what the export
//! carries, how it ends on a closed pipe, and the all-or-nothing import.

use crate::common;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Output, Stdio};

use relais::contract::{TaskContract, TaskType};
use relais::ids::{RunId, TaskId};
use relais::ledger::{Ledger, TaskTypeSource};
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    state: PathBuf,
    /// Released only after the scratch directory is gone, so the cap bounds
    /// live worlds, not finished ones.
    _permit: std::sync::Arc<common::WorldPermit>,
}

impl World {
    fn new(tag: &str) -> Self {
        Self::with_permit(tag, std::sync::Arc::new(common::WorldPermit::acquire(1)))
    }

    /// Builds a world under a permit the caller already holds, so a test
    /// with several worlds takes one `acquire(n)` for all of them.
    fn with_permit(tag: &str, permit: std::sync::Arc<common::WorldPermit>) -> Self {
        let scratch = short_temp_dir(&format!("tt-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        Self {
            _permit: permit,
            _scratch: scratch,
            state,
            root,
        }
    }

    fn ledger(&self) -> Ledger {
        Ledger::open(&self.state.join("ledger.sqlite")).expect("ledger")
    }

    /// A task with one run and one contract revision.
    fn add_task(&self, ledger: &Ledger, name: &str, objective: &str, declared: Option<TaskType>) {
        let run = RunId::from_stored(format!("run-{name}"));
        ledger
            .insert_typed_run(
                &run,
                "/repo",
                None,
                &TaskId::from_stored(format!("task-{name}")),
                "rk",
                declared,
            )
            .expect("run");
        let contract = TaskContract::from_json_str(&format!(
            r#"{{"schema_version":1,"kind":"change","objective":{},
                "base_ref":"HEAD","write_scope":["crates/**"],"acceptance":["parses"],
                "verification_profile":"p"}}"#,
            serde_json::to_string(objective).expect("json")
        ))
        .expect("contract");
        let json = serde_json::to_string(&contract.canonical_value()).expect("json");
        ledger
            .insert_contract_revision(&run, &contract.hash(), &json, "HEAD", None)
            .expect("revision");
    }

    fn command(&self, args: &[&str]) -> std::process::Command {
        let mut command = common::relais(BIN);
        command
            .arg("dataset")
            .args(args)
            .current_dir(&self.root)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args)
            .stdin(Stdio::null())
            .output()
            .expect("relais runs")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[test]
fn the_export_carries_the_contract_and_no_file_contents() {
    let world = World::new("export");
    let ledger = world.ledger();
    world.add_task(&ledger, "one", "Fix the escaping", None);
    // A task whose contract names a file, in a repository that holds it:
    // only the path may be exported, never the file's text.
    let repo = world.root.join("repo");
    std::fs::create_dir_all(repo.join("crates")).expect("repo");
    std::fs::write(repo.join("crates/lib.rs"), "FILE-CONTENT-MARKER").expect("write");
    let run = RunId::from_stored("run-hinted".to_string());
    ledger
        .insert_typed_run(
            &run,
            &repo.to_string_lossy(),
            None,
            &TaskId::from_stored("task-hinted".to_string()),
            "rk",
            None,
        )
        .expect("run");
    let contract = TaskContract::from_json_str(
        r#"{"schema_version":1,"kind":"change","objective":"Tidy lib.rs",
            "base_ref":"HEAD","write_scope":["crates/lib.rs"],"read_hints":["crates/lib.rs"],
            "acceptance":["parses"],"verification_profile":"p"}"#,
    )
    .expect("contract");
    let json = serde_json::to_string(&contract.canonical_value()).expect("json");
    ledger
        .insert_contract_revision(&run, &contract.hash(), &json, "HEAD", None)
        .expect("revision");
    drop(ledger);

    let out = world.run(&["export-tasks", "--out", "-"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(!stdout.contains("FILE-CONTENT-MARKER"));
    assert!(
        stdout.contains(r#""write_scope":["crates/lib.rs"]"#),
        "{stdout}"
    );
    let row: serde_json::Value =
        serde_json::from_str(stdout.lines().next().expect("one row")).expect("json");
    assert_eq!(row["task_id"], "task-one");
    assert_eq!(row["repo"], "/repo");
    assert_eq!(row["kind"], "change");
    assert_eq!(row["objective"], "Fix the escaping");
    assert_eq!(row["write_scope"][0], "crates/**");
    assert_eq!(row["acceptance"][0], "parses");
    let mut keys: Vec<&str> = row
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "acceptance",
            "kind",
            "objective",
            "repo",
            "task_id",
            "write_scope"
        ]
    );
    assert!(
        text(&out.stderr).contains("2 task(s)"),
        "{}",
        text(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn an_export_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new("mode");
    let ledger = world.ledger();
    world.add_task(&ledger, "one", "o", None);
    drop(ledger);
    let file = world.root.join("tasks.jsonl");
    let out = world.run(&["export-tasks", "--out", &file.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let mode = std::fs::metadata(&file)
        .expect("written")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        std::fs::read_to_string(&file)
            .expect("read")
            .lines()
            .count(),
        1
    );
}

#[test]
fn a_closed_pipe_ends_the_export_quietly_with_exit_0() {
    let world = World::new("pipe");
    let ledger = world.ledger();
    // Well over a pipe buffer, so the write meets the closed reader.
    let objective = "x".repeat(600);
    for index in 0..400 {
        world.add_task(&ledger, &format!("t{index}"), &objective, None);
    }
    drop(ledger);
    let mut child = world
        .command(&["export-tasks", "--out", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("relais runs");
    let mut first = String::new();
    {
        let mut reader = BufReader::new(child.stdout.take().expect("stdout"));
        reader.read_line(&mut first).expect("one line");
        // `head -1` is done: the reader goes away here.
    }
    let out = child.wait_with_output().expect("relais ends");
    assert!(first.contains("\"task_id\""), "{first}");
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text(&out.stderr),
        "",
        "a closed pipe is not worth a message"
    );
}

fn labels_file(world: &World, rows: &[(&str, &str)]) -> PathBuf {
    let body: String = rows
        .iter()
        .map(|(task, task_type)| {
            format!("{{\"task_id\":\"{task}\",\"task_type\":\"{task_type}\",\"reason\":\"r\"}}\n")
        })
        .collect();
    let file = world.root.join("labels.jsonl");
    std::fs::write(&file, body).expect("write");
    file
}

#[test]
fn one_unknown_id_refuses_the_whole_import_naming_the_line() {
    let world = World::new("bad");
    let ledger = world.ledger();
    world.add_task(&ledger, "a", "o", None);
    world.add_task(&ledger, "b", "o", None);
    drop(ledger);
    let file = labels_file(
        &world,
        &[
            ("task-a", "feature"),
            ("task-nowhere", "feature"),
            ("task-b", "ui"),
        ],
    );
    let out = world.run(&["import-task-types", &file.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains(&format!("{}:2:", file.display())),
        "names <file>:<line>: {stderr}"
    );
    assert!(stderr.contains("task-nowhere"), "{stderr}");
    assert_eq!(
        world.ledger().typed_task_count().expect("count"),
        0,
        "nothing was written"
    );
}

#[test]
fn an_unknown_type_is_refused_naming_the_eight() {
    let world = World::new("chore");
    let ledger = world.ledger();
    world.add_task(&ledger, "a", "o", None);
    drop(ledger);
    let file = labels_file(&world, &[("task-a", "chore")]);
    let out = world.run(&["import-task-types", &file.to_string_lossy()]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = text(&out.stderr);
    for declared in TaskType::DECLARABLE {
        assert!(stderr.contains(declared.as_str()), "{stderr}");
    }
}

#[test]
fn an_import_labels_history_once_and_never_overwrites_a_declared_type() {
    let world = World::new("good");
    let ledger = world.ledger();
    world.add_task(&ledger, "a", "o", None);
    world.add_task(&ledger, "b", "o", Some(TaskType::Ui));
    drop(ledger);
    let file = labels_file(&world, &[("task-a", "feature"), ("task-b", "infra")]);
    let path = file.to_string_lossy().to_string();
    let args = ["import-task-types", path.as_str()];
    let first = world.run(&args);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    assert!(
        text(&first.stderr).contains("feature 1"),
        "{}",
        text(&first.stderr)
    );

    let ledger = world.ledger();
    let a = ledger
        .task_type_of(&TaskId::from_stored("task-a"))
        .expect("read")
        .expect("typed");
    assert_eq!(
        (a.task_type, a.source),
        (TaskType::Feature, TaskTypeSource::Classified)
    );
    let b = ledger
        .task_type_of(&TaskId::from_stored("task-b"))
        .expect("read")
        .expect("typed");
    assert_eq!(
        (b.task_type, b.source),
        (TaskType::Ui, TaskTypeSource::Declared),
        "a declared type is never overwritten by a classified one"
    );
    drop(ledger);

    let again = world.run(&args);
    assert_eq!(again.status.code(), Some(0));
    assert!(
        text(&again.stderr).contains("1 unchanged"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(world.ledger().typed_task_count().expect("count"), 2);
}

/// A settled replay trial `trial-<name>` on a fresh task, ended `outcome`.
fn settled_replay(
    world: &World,
    name: &str,
    outcome: relais::ledger::TrialOutcome,
) -> relais::ids::TrialId {
    use relais::ids::TrialId;
    let ledger = world.ledger();
    world.add_task(&ledger, name, "o", None);
    let trial_id = TrialId::from_stored(format!("trial-{name}"));
    let arm_run = RunId::from_stored(format!("arm-{name}"));
    ledger
        .insert_replay_trial(&relais::ledger::NewReplayTrial {
            trial_id: &trial_id,
            task_id: &TaskId::from_stored(format!("task-{name}")),
            source_run_id: &RunId::from_stored(format!("run-{name}")),
            incumbent_recipe_id: "incumbent",
            arm_recipe_id: "arm",
            base_sha: "base",
            contract_hash: "contract",
            verification_profile_hash: "profile",
            workspace_isolation: "fresh_checkout_no_accepted_answer",
            arm_run_id: &arm_run,
            design_sha256: None,
        })
        .expect("trial");
    ledger
        .settle_trial(
            &trial_id,
            outcome,
            false,
            relais::ledger::TrialCost::UNKNOWN,
            1,
        )
        .expect("settle");
    trial_id
}

fn stored_truth(
    world: &World,
    trial: &relais::ids::TrialId,
) -> Option<relais::ledger::GroundTruth> {
    world
        .ledger()
        .trial(trial)
        .expect("reads")
        .expect("row")
        .ground_truth
}

#[test]
fn judge_trial_records_a_verdict_and_the_same_verdict_again_is_a_no_op() {
    let world = World::new("judge");
    let trial = settled_replay(&world, "r", relais::ledger::TrialOutcome::Rejected);
    let out = world.run(&[
        "judge-trial",
        "trial-r",
        "--verdict",
        "pass",
        "--note",
        "commit abc has the same defect",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(json["trial_id"], "trial-r");
    assert_eq!(json["verdict"], "pass");
    assert_eq!(json["note"], "commit abc has the same defect");
    let at = json["at"].as_str().expect("timestamp").to_string();
    assert!(!at.is_empty());
    let stored = stored_truth(&world, &trial).expect("stored");
    assert_eq!(stored.at, at);

    let again = world.run(&["judge-trial", "trial-r", "--verdict", "pass", "--note", "x"]);
    assert_eq!(again.status.code(), Some(0));
    assert!(text(&again.stdout).contains("already judged pass"));
    assert_eq!(stored_truth(&world, &trial), Some(stored));
}

#[test]
fn judge_trial_refusals_exit_2_naming_the_trial_and_write_nothing() {
    use relais::ledger::TrialOutcome;
    let world = World::new("judge-no");
    let rejected = settled_replay(&world, "r", TrialOutcome::Rejected);
    let accepted = settled_replay(&world, "a", TrialOutcome::Accepted);
    let errored = settled_replay(&world, "e", TrialOutcome::Errored);
    let refuse = |args: &[&str]| {
        let out = world.run(args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
        text(&out.stderr)
    };
    assert!(refuse(&[
        "judge-trial",
        "trial-ghost",
        "--verdict",
        "pass",
        "--note",
        "n"
    ])
    .contains("trial-ghost"));
    assert!(
        refuse(&["judge-trial", "trial-a", "--verdict", "pass", "--note", "n"]).contains("trial-a")
    );
    assert!(
        refuse(&["judge-trial", "trial-e", "--verdict", "fail", "--note", "n"]).contains("trial-e")
    );
    assert!(
        refuse(&["judge-trial", "trial-r", "--verdict", "pass", "--note", " "]).contains("trial-r")
    );
    for trial in [&rejected, &accepted, &errored] {
        assert_eq!(stored_truth(&world, trial), None);
    }
    let ok = world.run(&["judge-trial", "trial-r", "--verdict", "fail", "--note", "n"]);
    assert_eq!(ok.status.code(), Some(0));
    let before = stored_truth(&world, &rejected);
    assert!(
        refuse(&["judge-trial", "trial-r", "--verdict", "pass", "--note", "m"]).contains("trial-r")
    );
    assert_eq!(stored_truth(&world, &rejected), before);
}
