//! `shared_state` in machine.toml through the real binary: an external
//! layer this build does not have is refused by the commands that read the
//! machine settings (exit 3, one stderr line, nothing on stdout), `relais
//! hook` still answers, and `relais trust grant` keeps the key. Every run
//! uses scratch config and state directories.
#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use relais::test_support::{temp_dir, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_relais");

/// The repository this crate lives in: its own `relais.toml` is the policy
/// `trust` reads.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels under the repository")
        .to_path_buf()
}

struct Scratch {
    dir: TempDir,
}

impl Scratch {
    fn with_machine_toml(tag: &str, text: &str) -> Self {
        let dir = temp_dir(tag);
        for sub in ["home", "config", "state", "claude"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch dir");
        }
        std::fs::write(dir.join("config/machine.toml"), text).expect("machine.toml");
        Self { dir }
    }

    fn machine_toml(&self) -> PathBuf {
        self.dir.join("config/machine.toml")
    }

    fn relais(&self, args: &[&str], stdin: &str) -> Output {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(repo_root())
            .env("HOME", self.dir.join("home"))
            .env("RELAIS_CONFIG_DIR", self.dir.join("config"))
            .env("RELAIS_STATE_DIR", self.dir.join("state"))
            // `claude_projects_dir` prefers it over HOME: a session that
            // sets it must not lend its transcripts to a test.
            .env("CLAUDE_CONFIG_DIR", self.dir.join("claude"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("relais runs");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("stdin written");
        child.wait_with_output().expect("relais exits")
    }
}

#[test]
fn an_absent_external_layer_is_refused_with_exit_3_and_one_line() {
    let scratch = Scratch::with_machine_toml(
        "shared-state-external",
        "schema_version = 1\nshared_state = \"external\"\n",
    );
    let out = scratch.relais(&["trust", "show"], "");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(out.stdout.is_empty(), "nothing on stdout: {out:?}");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert_eq!(
        stderr.trim_end(),
        format!(
            "relais: machine.toml {} sets shared_state = \"external\", but this relais build \
             has no shared-state client; set shared_state = \"local\" or install the build that \
             has one",
            scratch.machine_toml().display()
        )
    );
}

#[test]
fn the_hook_still_answers_under_an_absent_external_layer() {
    let scratch = Scratch::with_machine_toml(
        "shared-state-hook",
        "schema_version = 1\nshared_state = \"external\"\n",
    );
    let out = scratch.relais(&["hook"], "{}");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
}

#[test]
fn a_session_end_under_an_absent_external_layer_updates_the_local_claims_and_says_so() {
    use relais::programme::claim::{ClaimKey, Contest, Store, LOCK_WAIT};
    let payload = r#"{"hook_event_name":"SessionEnd","session_id":"session-end-1"}"#;
    let mut stdouts = Vec::new();
    for (tag, text) in [
        ("shared-state-end-local", "schema_version = 1\n"),
        (
            "shared-state-end-external",
            "schema_version = 1\nshared_state = \"external\"\n",
        ),
    ] {
        let scratch = Scratch::with_machine_toml(tag, text);
        let store = Store::in_state_dir(&scratch.dir.join("state"));
        let key = ClaimKey {
            repo_key: "rk".into(),
            slug: "programme".into(),
        };
        let now = relais::programme::claim::now_unix();
        store
            .lock(&key, LOCK_WAIT, now)
            .expect("lock")
            .claim_work("t1", "session-end-1", "/r", Contest::Refuse, now)
            .expect("claimed");
        let out = scratch.relais(&["hook"], payload);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let warnings: Vec<&str> = stderr
            .lines()
            .filter(|line| line.contains("shared claims skipped"))
            .collect();
        if text.contains("external") {
            assert_eq!(warnings.len(), 1, "{stderr}");
            assert!(
                warnings[0].starts_with(
                    "relais: shared claims skipped, the local claims were updated: machine.toml "
                ),
                "{stderr}"
            );
            assert!(
                warnings[0]
                    .ends_with("set shared_state = \"local\" or install the build that has one"),
                "{stderr}"
            );
        } else {
            assert!(warnings.is_empty(), "{stderr}");
        }
        assert!(
            store.records(&key).expect("records").is_empty(),
            "{tag}: the work claim is gone"
        );
        stdouts.push(out.stdout);
    }
    assert_eq!(stdouts[0], stdouts[1], "the hook's answer is the same");
}

/// an-t20: every settings writer asks the shared state, so under an absent
/// external layer each exits 3 with the one sentence and the file is the
/// bytes it was.
#[test]
fn every_settings_writer_is_refused_under_an_absent_external_layer() {
    let before = "# mine\nschema_version = 1\nshared_state = \"external\"\n";
    let commands: [&[&str]; 5] = [
        // Pins exit 3, not the write path: `trust grant` already stops at
        // its read of the settings. The rows below reach the write.
        &["trust", "grant", "--reviewed-by", "test"],
        &["native", "learning-envelope", "--by", "me"],
        &["native", "learning-envelope", "--off", "--by", "me"],
        &[
            "native",
            "router-envelope",
            "--by",
            "me",
            "--epsilon-max",
            "0.1",
        ],
        &["native", "router-envelope", "--off", "--by", "me"],
    ];
    for args in commands {
        let scratch = Scratch::with_machine_toml("shared-state-writers-external", before);
        let out = scratch.relais(args, "");
        assert_eq!(out.status.code(), Some(3), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        let stderr = String::from_utf8(out.stderr).expect("utf-8");
        assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
        assert!(
            stderr.contains("sets shared_state = \"external\""),
            "{args:?}: {stderr}"
        );
        assert_eq!(
            std::fs::read_to_string(scratch.machine_toml()).expect("read"),
            before,
            "{args:?}: the file is untouched"
        );
    }
}

/// With no home directory there is no settings path: the envelope commands
/// stay exit 3 (`CliError::Home`), as before the write went through the trait.
#[test]
fn a_settings_writer_with_no_home_directory_is_exit_3() {
    let scratch = Scratch::with_machine_toml("shared-state-no-home", "schema_version = 1\n");
    let out = Command::new(BIN)
        .args(["native", "router-envelope", "--off", "--by", "me"])
        .current_dir(repo_root())
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env_remove("RELAIS_CONFIG_DIR")
        .env("RELAIS_STATE_DIR", scratch.dir.join("state"))
        .output()
        .expect("relais runs");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
}

#[test]
fn a_trust_grant_keeps_the_key_and_adds_none() {
    let with_key = Scratch::with_machine_toml(
        "shared-state-grant-key",
        "schema_version = 1\nshared_state = \"local\"\n",
    );
    let out = with_key.relais(&["trust", "grant", "--reviewed-by", "test"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = std::fs::read_to_string(with_key.machine_toml()).expect("read");
    assert!(text.contains("shared_state = \"local\""), "{text}");

    let without = Scratch::with_machine_toml("shared-state-grant-none", "schema_version = 1\n");
    let out = without.relais(&["trust", "grant", "--reviewed-by", "test"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = std::fs::read_to_string(without.machine_toml()).expect("read");
    assert!(!text.contains("shared_state"), "{text}");
}

/// an-t31: `trust grant --json` says where the grant was recorded; on a
/// single node that is the settings file, still named in `machine_settings`.
#[test]
fn a_json_trust_grant_says_where_it_was_recorded() {
    let scratch = Scratch::with_machine_toml("shared-state-grant-json", "schema_version = 1\n");
    let out = scratch.relais(&["trust", "grant", "--reviewed-by", "test", "--json"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let object: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(object["code"], "granted", "{object}");
    assert_eq!(object["recorded_on"], "machine_settings", "{object}");
    assert_eq!(
        object["machine_settings"],
        scratch.machine_toml().display().to_string(),
        "{object}"
    );
}

/// an-t26: a salvage of a terminal run with no recorded workspace is refused
/// by the absent external layer before anything is written: one stderr line,
/// no "unknown containment" claim, and the run is left as it was.
#[test]
fn a_salvage_under_an_absent_external_layer_is_refused_and_records_nothing() {
    use relais::ids::{RunId, TaskId};
    use relais::ledger::{Ledger, Transition};
    use relais::runner::{Reason, State};
    for json in [false, true] {
        let scratch = Scratch::with_machine_toml(
            "shared-state-salvage-external",
            "schema_version = 1\nshared_state = \"external\"\n",
        );
        let ledger = Ledger::open(&scratch.dir.join("state/ledger.sqlite")).expect("ledger");
        let run = RunId::from_stored("run-ended");
        ledger
            .insert_run(&run, "/repo", None, &TaskId::from_stored("task"), "rk")
            .expect("run");
        for (from, to, why) in [
            (State::Prepared, State::Running, Reason::WorkerDispatched),
            (State::Running, State::Failed, Reason::WorkerLooped),
        ] {
            ledger
                .record_transition(&Transition {
                    run_id: run.clone(),
                    attempt_id: None,
                    from_state: Some(from),
                    to_state: to,
                    reason: why.as_str().into(),
                    detail: None,
                    at: ledger.now(),
                })
                .expect("transition");
        }
        let mut args = vec![
            "decide",
            "run-ended",
            "--answer",
            "salvaged",
            "--candidate",
            "abc123",
            "--actor",
            "orchestrator",
        ];
        if json {
            args.push("--json");
        }
        let out = scratch.relais(&args, "");
        assert_eq!(out.status.code(), Some(3), "json={json}: {out:?}");
        let stderr = String::from_utf8(out.stderr.clone()).expect("utf-8");
        let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
        assert!(!stderr.contains("recorded it as unknown"), "{stderr}");
        assert!(!stderr.contains("recording it as unknown"), "{stderr}");
        // The absent layer is refused before the answer, with its own
        // sentence: on stderr, and under `--json` also as the stdout object.
        assert!(
            stderr.contains("sets shared_state = \"external\""),
            "json={json}: {stderr}"
        );
        // An error, not a refusal: main's one line, `relais: <error>`, on
        // both paths (an-t31: the prefix differs from a refusal's on purpose).
        assert_eq!(stderr.lines().count(), 1, "json={json}: {stderr}");
        assert!(stderr.starts_with("relais: "), "json={json}: {stderr}");
        if json {
            assert_eq!(stdout.lines().count(), 1, "{stdout}");
            assert!(stdout.contains("\"exit\":3"), "{stdout}");
        } else {
            assert!(stdout.is_empty(), "{stdout}");
        }
        assert!(
            ledger.decision_of_run(&run).expect("read").is_none(),
            "no decision row was written"
        );
    }
}

/// an-t29: `relais report` reads one price table, so builds one state, for
/// the import and the routing section, before the import. Under an absent
/// external layer both forms exit 3 with the one sentence, once, and print
/// nothing. This pins the refusal and its single report, not the build count
/// (`LocalState` holds no state; `every_command_builds_its_shared_state_once`
/// counts the builds), and passes on main too.
#[test]
fn a_report_under_an_absent_external_layer_is_refused_before_the_import() {
    for args in [&["report"][..], &["report", "--no-import"][..]] {
        let scratch = Scratch::with_machine_toml(
            "shared-state-report-external",
            "schema_version = 1\nshared_state = \"external\"\n",
        );
        let out = scratch.relais(args, "");
        assert_eq!(out.status.code(), Some(3), "{args:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{args:?}: {out:?}");
        let stderr = String::from_utf8(out.stderr).expect("utf-8");
        assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
        assert!(
            stderr.contains("sets shared_state = \"external\""),
            "{args:?}: {stderr}"
        );
    }
}

/// an-t29: `relais learn cycle` builds its one state where it reads the
/// machine settings, before it starts a coordinator, so an absent external
/// layer exits 3 with the one sentence and leaves no coordinator socket
/// (`socket_path()` is `RELAIS_STATE_DIR/relais.sock`).
#[test]
fn a_learn_cycle_under_an_absent_external_layer_is_refused_before_the_coordinator() {
    let scratch = Scratch::with_machine_toml(
        "shared-state-cycle-external",
        "schema_version = 1\nshared_state = \"external\"\n",
    );
    let out = scratch.relais(&["learn", "cycle"], "");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.contains("sets shared_state = \"external\""),
        "{stderr}"
    );
    assert!(
        !scratch.dir.join("state/relais.sock").exists(),
        "no coordinator was started"
    );
}

/// an-t31: a refusal under `--json` is the `{"error","exit"}` object on
/// stdout and one `relais <command>:` line on stderr, the exit code agreeing
/// with both (`cli::decision_record::refusal_streams`); plain, it is the stderr line
/// alone. Both commands refuse here before any shared state is built.
#[test]
fn a_refusal_under_json_is_one_stdout_object_and_one_stderr_line() {
    let scratch =
        Scratch::with_machine_toml("shared-state-refusal-streams", "schema_version = 1\n");
    let decide = [
        "decide",
        "run-x",
        "--answer",
        "approve",
        "--actor",
        "person-via-orchestrator",
    ];
    let feedback = [
        "feedback",
        "run-x",
        "--outcome",
        "accepted",
        "--actor",
        "nobody",
    ];
    for (prefix, args) in [
        ("relais decide: ", &decide[..]),
        ("relais feedback: ", &feedback[..]),
    ] {
        for json in [false, true] {
            let mut args = args.to_vec();
            if json {
                args.push("--json");
            }
            let out = scratch.relais(&args, "");
            let code = out.status.code().expect("an exit code");
            assert_ne!(code, 0, "{args:?} is refused: {out:?}");
            let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
            let stderr = String::from_utf8(out.stderr.clone()).expect("utf-8");
            assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
            assert!(stderr.starts_with(prefix), "{args:?}: {stderr}");
            if json {
                assert_eq!(stdout.lines().count(), 1, "{args:?}: {stdout}");
                let object: serde_json::Value = serde_json::from_str(&stdout).expect("json");
                assert_eq!(object["exit"], code, "{args:?}: the exit agrees");
                assert_eq!(
                    format!("{prefix}{}", object["error"].as_str().expect("error")),
                    stderr.trim_end(),
                    "{args:?}: the same refusal on both streams"
                );
            } else {
                assert!(stdout.is_empty(), "{args:?}: {stdout}");
            }
        }
    }
}

/// an-t31: an approval that wrote no receipt says so on stderr under
/// `--json` too, while stdout stays the one object carrying
/// `receipt_skipped`.
#[test]
fn a_json_approval_with_no_receipt_warns_on_stderr_and_keeps_one_object() {
    use relais::ids::{RunId, TaskId};
    use relais::ledger::{Ledger, Transition};
    use relais::runner::{Reason, State};
    let scratch =
        Scratch::with_machine_toml("shared-state-receipt-skipped", "schema_version = 1\n");
    let ledger = Ledger::open(&scratch.dir.join("state/ledger.sqlite")).expect("ledger");
    let run = RunId::from_stored("run-old");
    ledger
        .insert_run(&run, "/repo", None, &TaskId::from_stored("task"), "rk")
        .expect("run");
    let revision = ledger
        .insert_contract_revision(&run, "h", "{}", "HEAD", None)
        .expect("revision");
    let attempt = ledger
        .insert_attempt(
            &run,
            revision,
            1,
            "implementation",
            relais::lifecycle::UsagePhase::Initial,
        )
        .expect("attempt");
    ledger
        .finish_attempt(attempt, State::Running, None, Some("cand"))
        .expect("candidate");
    for (from, to, why) in [
        (State::Prepared, State::Running, Reason::WorkerDispatched),
        (
            State::Running,
            State::NeedsReview,
            Reason::ReviewUnavailable,
        ),
    ] {
        ledger
            .record_transition(&Transition {
                run_id: run.clone(),
                attempt_id: None,
                from_state: Some(from),
                to_state: to,
                reason: why.as_str().into(),
                detail: None,
                at: ledger.now(),
            })
            .expect("transition");
    }
    let out = scratch.relais(
        &[
            "decide",
            "run-old",
            "--answer",
            "approve",
            "--actor",
            "orchestrator",
            "--json",
        ],
        "",
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
    let stderr = String::from_utf8(out.stderr.clone()).expect("utf-8");
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let object: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(object["receipt_skipped"], "review_unavailable", "{object}");
    assert_eq!(
        stderr.trim_end(),
        "relais decide: no receipt written: no verification snapshot for this \
         review_unavailable stop",
        "{stderr}"
    );
}
