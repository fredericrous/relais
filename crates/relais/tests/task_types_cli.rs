//! `relais dataset export-tasks` and `import-task-types` through the real
//! binary (docs/plans/2026-10-12-task-types.md, Phase 1): what the export
//! carries, how it ends on a closed pipe, and the all-or-nothing import.

mod common;

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
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("tt-{tag}"));
        let root = scratch.to_path_buf();
        let state = root.join("state");
        std::fs::create_dir_all(&state).expect("state");
        Self {
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
