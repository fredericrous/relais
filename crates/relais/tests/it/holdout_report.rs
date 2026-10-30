//! `relais report --json` through the real binary: the `holdouts` section
//! counts a catch and an exposure recorded in a fixture ledger (ADR-0045
//! phase 4).

use crate::common;

use relais::ids::{RunId, TaskId};
use relais::ledger::{HoldoutOutcomeRow, Ledger};
use relais::test_support::short_temp_dir;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

#[test]
fn the_report_json_counts_one_catch_and_one_exposure_under_holdouts() {
    let _permit = common::WorldPermit::acquire(1);
    let scratch = short_temp_dir("ho-report");
    let root = scratch.to_path_buf();
    let state = root.join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    let ledger = Ledger::open(&state.join("ledger.sqlite")).expect("ledger");
    let caught = RunId::from_stored("run-caught");
    let exposed = RunId::from_stored("run-exposed");
    for run in [&caught, &exposed] {
        ledger
            .insert_run(run, "/repo", None, &TaskId::from_stored(run.as_str()), "rk")
            .expect("run");
    }
    ledger
        .record_holdout_outcome(
            &caught,
            &HoldoutOutcomeRow {
                label_kind: "attempt",
                attempt_index: 1,
                name: "escaping",
                outcome: "failed",
                profile_green: true,
                source: "named",
            },
        )
        .expect("outcome");
    ledger
        .record_holdout_outcome(
            &exposed,
            &HoldoutOutcomeRow {
                label_kind: "attempt",
                attempt_index: 1,
                name: "escaping",
                outcome: "passed",
                profile_green: true,
                source: "named",
            },
        )
        .expect("outcome");
    ledger
        .record_holdout_exposure(&exposed, "dispatch-1", "Read", "machine_toml")
        .expect("exposure");
    drop(ledger);

    let output = common::relais(BIN)
        .args(["report", "--no-import", "--json"])
        .current_dir(&root)
        .env("RELAIS_STATE_DIR", &state)
        .env("RELAIS_CONFIG_DIR", root.join("cfg"))
        .env("HOME", &root)
        .env("USERPROFILE", &root)
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .expect("relais runs");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("json");
    let holdouts = &report["holdouts"];
    assert_eq!(holdouts["holdout_runs"], 2, "{report}");
    assert_eq!(holdouts["holdout_catches"], 1, "{report}");
    assert_eq!(holdouts["holdout_exposures"]["runs"], 1, "{report}");
    assert_eq!(
        holdouts["holdout_exposures"]["by_marker"]["machine_toml"], 1,
        "{report}"
    );
}

#[test]
fn the_report_json_splits_holdouts_by_source_and_counts_skipped_attached() {
    let _permit = common::WorldPermit::acquire(1);
    let scratch = short_temp_dir("ho-source");
    let root = scratch.to_path_buf();
    let state = root.join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    let ledger = Ledger::open(&state.join("ledger.sqlite")).expect("ledger");
    let rows = [
        ("run-named", "passed", "named"),
        ("run-attached", "failed", "attached"),
        ("run-skipped", "skipped", "attached"),
    ];
    for (run, outcome, source) in rows {
        let run = RunId::from_stored(run);
        ledger
            .insert_run(
                &run,
                "/repo",
                None,
                &TaskId::from_stored(run.as_str()),
                "rk",
            )
            .expect("run");
        ledger
            .record_holdout_outcome(
                &run,
                &HoldoutOutcomeRow {
                    label_kind: "attempt",
                    attempt_index: 1,
                    name: "h",
                    outcome,
                    profile_green: true,
                    source,
                },
            )
            .expect("outcome");
    }
    drop(ledger);

    let output = common::relais(BIN)
        .args(["report", "--no-import", "--json"])
        .current_dir(&root)
        .env("RELAIS_STATE_DIR", &state)
        .env("RELAIS_CONFIG_DIR", root.join("cfg"))
        .env("HOME", &root)
        .env("USERPROFILE", &root)
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .expect("relais runs");
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).expect("json");
    assert_eq!(report["schema_version"], 16, "{report}");
    let holdouts = &report["holdouts"];
    assert_eq!(holdouts["holdout_runs"], 2, "{report}");
    assert_eq!(holdouts["holdout_catches"], 1, "{report}");
    assert_eq!(holdouts["by_source"]["named"]["runs"], 1, "{report}");
    assert_eq!(holdouts["by_source"]["named"]["catches"], 0, "{report}");
    assert_eq!(holdouts["by_source"]["attached"]["runs"], 1, "{report}");
    assert_eq!(holdouts["by_source"]["attached"]["catches"], 1, "{report}");
    assert_eq!(holdouts["by_source"]["attached"]["skipped"], 1, "{report}");
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run `relais plan` in a scratch repository whose machine.toml is
/// `holdouts_toml` and whose task carries `acceptance`.
fn plan_in_scratch(holdouts_toml: &str, acceptance: Value, extra: &[&str]) -> std::process::Output {
    let scratch = short_temp_dir("ho-plan");
    let root = scratch.to_path_buf();
    let repo = root.join("repo");
    let config = root.join("cfg");
    let state = root.join("state");
    for dir in [&repo, &config, &state, &root.join("no-hooks")] {
        std::fs::create_dir_all(dir).expect("dir");
    }
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "config",
            "core.hooksPath",
            &root.join("no-hooks").to_string_lossy(),
        ],
    );
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").expect("write");
    std::fs::write(
        repo.join("relais.toml"),
        r#"schema_version = 1

[models.research]
id = "haiku"

[models.implementation]
id = "sonnet"

[models.escalation]
id = "fable"

[execution]
max_attempts = 3
max_repairs_before_escalation = 1
max_wall_seconds = 120

[integrations]
aval = "off"
amont = "off"
amont_agent = "off"

[verification.profiles.default]
commands = []
"#,
    )
    .expect("policy");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    std::fs::write(config.join("machine.toml"), holdouts_toml).expect("machine");
    let task = root.join("task.json");
    std::fs::write(
        &task,
        serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "task_type": "feature",
            "objective": "Remove the obsolete entry point",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "acceptance": acceptance,
            "verification_profile": "default",
            "review": "off",
        })
        .to_string(),
    )
    .expect("task");
    let mut args = vec!["plan", "--task", task.to_str().unwrap()];
    args.extend_from_slice(extra);
    let output = common::relais(BIN)
        .args(args)
        .current_dir(&repo)
        .env("RELAIS_STATE_DIR", &state)
        .env("RELAIS_CONFIG_DIR", &config)
        .env("RELAIS_CLAUDE_BIN", root.join("no-such-claude"))
        .env("RELAIS_SESSION_ID", "tab-test")
        .env("HOME", &root)
        .env("USERPROFILE", &root)
        .env_remove("CLAUDE_CONFIG_DIR")
        .output()
        .expect("relais runs");
    let _ = common::relais(BIN)
        .args(["coordinator", "stop"])
        .env("RELAIS_STATE_DIR", &state)
        .env("RELAIS_CONFIG_DIR", &config)
        .env("HOME", &root)
        .output(); // best-effort: plan may have started a coordinator, stopping is harmless
    output
}

const ATTACHED_ESCAPING: &str = r#"schema_version = 1

[holdouts]
store = "unused"

[holdouts.repo.escaping]
path = "repo/escaping.sh"
overlay = "tests/escaping.sh"
command = { argv = ["true"], junit = "junit.xml" }

[holdouts.repo.escaping.attach]
task_types = ["feature"]
paths = ["src/**"]
"#;

const NAMED_ONLY: &str = r#"schema_version = 1

[holdouts]
store = "unused"

[holdouts.repo.named]
path = "repo/named.sh"
overlay = "tests/named.sh"
command = { argv = ["true"], junit = "junit.xml" }
"#;

#[test]
fn plan_skips_an_attached_holdout_on_an_unreachable_store_in_json_and_human_output() {
    let _permit = common::WorldPermit::acquire(1);
    let acceptance = serde_json::json!(["src/main.rs no longer exists"]);
    let json = plan_in_scratch(ATTACHED_ESCAPING, acceptance.clone(), &["--json"]);
    let stderr = String::from_utf8_lossy(&json.stderr);
    assert!(stderr.contains("holdout_attach_skipped"), "{stderr}");
    let report: Value = serde_json::from_slice(&json.stdout).expect("plan json");
    assert_eq!(
        report["holdouts"]["skipped"],
        serde_json::json!([{"name": "escaping"}]),
        "{report}"
    );
    assert_eq!(
        report["holdouts"]["attached"],
        serde_json::json!([]),
        "{report}"
    );
    let human = plan_in_scratch(ATTACHED_ESCAPING, acceptance, &[]);
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(
        stdout.contains("holdout escaping skipped: store unreachable"),
        "{stdout}"
    );
    assert!(!stdout.contains("escaping attached"), "{stdout}");
}

#[test]
fn plan_lists_an_attached_holdout_on_a_reachable_store_in_json_and_human_output() {
    let _permit = common::WorldPermit::acquire(1);
    let store = short_temp_dir("ho-store");
    let store_dir = store.to_path_buf();
    git(&store_dir, &["init", "-q"]);
    std::fs::write(store_dir.join("escaping.txt"), "holdout\n").expect("holdout file");
    git(&store_dir, &["add", "-A"]);
    git(
        &store_dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "test: add a holdout",
        ],
    );
    let machine = ATTACHED_ESCAPING.replace(
        "store = \"unused\"",
        // A TOML literal string: a Windows path's backslashes are not escapes.
        &format!("store = '{}'\nref = \"HEAD\"", store_dir.display()),
    );
    assert_ne!(machine, ATTACHED_ESCAPING);
    let acceptance = serde_json::json!(["src/main.rs no longer exists"]);
    let json = plan_in_scratch(&machine, acceptance.clone(), &["--json"]);
    let report: Value = serde_json::from_slice(&json.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&json.stdout),
            String::from_utf8_lossy(&json.stderr)
        )
    });
    assert_eq!(
        report["holdouts"]["attached"],
        serde_json::json!([
            {"name": "escaping", "task_type": "feature", "path": "src/**"}
        ]),
        "{report}"
    );
    assert_eq!(
        report["holdouts"]["skipped"],
        serde_json::json!([]),
        "{report}"
    );
    let human = plan_in_scratch(&machine, acceptance, &[]);
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(
        stdout.contains("holdout escaping attached: task type feature, path src/**"),
        "{stdout}"
    );
}

#[test]
fn plan_refuses_a_named_holdout_on_an_unreachable_store() {
    let _permit = common::WorldPermit::acquire(1);
    let named = plan_in_scratch(
        NAMED_ONLY,
        serde_json::json!([
            {"statement": "the named holdout passes", "evidence": {"kind": "holdout", "name": "named"}}
        ]),
        &[],
    );
    assert_eq!(named.status.code(), Some(3), "{named:?}");
    let stderr = String::from_utf8_lossy(&named.stderr);
    assert!(stderr.contains("blocked (holdout_unreadable)"), "{stderr}");
}
