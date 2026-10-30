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
