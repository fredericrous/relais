// -- scripts/plugin-test.sh and the retry owner (plan C2) ---------------------
//
// One retry owner per execution context: under relais (`RELAIS_CHECK` set)
// the check's own `retry` owns the rerun, so the script runs the plugin suite
// at most once; outside it (CI) the script's #240 rerun of an all-timeout
// failure stays.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::test_support::temp_dir;

/// A `claude` that records each `plugin test` run and fails every one with
/// nothing but the runner's 5000 ms timeout.
fn timing_out_claude(dir: &Path) -> std::path::PathBuf {
    let bin = dir.join("claude");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\necho run >> '{}'\necho '(fail) slow test'\necho 'error: test timed out after 5000 ms'\nexit 1\n",
            dir.join("runs").display()
        ),
    )
    .expect("writes the fake");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    bin
}

fn runs_of_script(check_owner: bool) -> (Option<i32>, usize) {
    let dir = temp_dir("plugin-test-script");
    let bin = timing_out_claude(&dir);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/plugin-test.sh");
    let mut command = Command::new("sh");
    command
        .arg(script)
        .env("CLAUDE_BIN", bin)
        .env_remove("RELAIS_CHECK");
    if check_owner {
        command.env("RELAIS_CHECK", "1");
    }
    let status = command.output().expect("runs the script").status;
    let runs = std::fs::read_to_string(dir.join("runs"))
        .map(|text| text.lines().count())
        .unwrap_or(0);
    (status.code(), runs)
}

#[test]
fn under_relais_the_plugin_suite_runs_at_most_once() {
    assert_eq!(runs_of_script(true), (Some(1), 1));
}

#[test]
fn outside_relais_an_all_timeout_failure_is_rerun_once() {
    assert_eq!(runs_of_script(false), (Some(1), 2));
}
