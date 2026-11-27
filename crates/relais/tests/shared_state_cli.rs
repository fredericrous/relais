//! `shared_state` in machine.toml through the real binary: an external
//! layer this build does not have is refused by the commands that read the
//! machine settings (exit 3, one stderr line, nothing on stdout), `relais
//! hook` still answers, and `relais trust grant` keeps the key. Every run
//! uses scratch config and state directories.
#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use relais::test_support::{short_temp_dir, temp_dir, TempDir};

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

    /// A scratch whose path is short enough for a Unix socket under it.
    fn short(tag: &str) -> Self {
        let dir = short_temp_dir(tag);
        for sub in ["home", "config", "state", "claude"] {
            std::fs::create_dir_all(dir.join(sub)).expect("scratch dir");
        }
        std::fs::write(dir.join("config/machine.toml"), "schema_version = 1\n")
            .expect("machine.toml");
        Self { dir }
    }

    /// `program` with this scratch's environment, run from `cwd`.
    fn command_in(&self, program: &str, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(cwd)
            .env("HOME", self.dir.join("home"))
            .env("RELAIS_CONFIG_DIR", self.dir.join("config"))
            .env("RELAIS_STATE_DIR", self.dir.join("state"))
            .env("CLAUDE_CONFIG_DIR", self.dir.join("claude"))
            .stdin(Stdio::null());
        command
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

/// an-t32: a grant that fails after its checks (an empty `--reviewed-by`) is
/// one `relais trust grant: ` stderr line and, under `--json`, the
/// `{"error","exit"}` object; plain, stdout is empty.
#[test]
fn a_failed_grant_follows_the_json_error_contract() {
    for json in [false, true] {
        let scratch =
            Scratch::with_machine_toml("shared-state-grant-error", "schema_version = 1\n");
        let mut args = vec!["trust", "grant", "--reviewed-by", ""];
        if json {
            args.push("--json");
        }
        let out = scratch.relais(&args, "");
        assert_eq!(out.status.code(), Some(2), "{args:?}: {out:?}");
        let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
        let stderr = String::from_utf8(out.stderr.clone()).expect("utf-8");
        assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
        assert!(
            stderr.starts_with("relais trust grant: "),
            "{args:?}: {stderr}"
        );
        if json {
            let object: serde_json::Value = serde_json::from_str(&stdout).expect("json");
            assert_eq!(object["exit"], 2, "{args:?}: {stdout}");
            assert_eq!(
                format!(
                    "relais trust grant: {}",
                    object["error"].as_str().expect("error")
                ),
                stderr.trim_end(),
                "{args:?}"
            );
        } else {
            assert!(stdout.is_empty(), "{args:?}: {stdout}");
        }
    }
}

/// an-t32: a stale `--key` keeps its `code` and `current_key` on the one
/// error object (they were a second document before), exit 18.
#[test]
fn a_stale_grant_key_is_one_error_object_with_its_code() {
    let scratch = Scratch::with_machine_toml("shared-state-grant-stale", "schema_version = 1\n");
    let out = scratch.relais(
        &[
            "trust",
            "grant",
            "--key",
            "stale",
            "--reviewed-by",
            "test",
            "--json",
        ],
        "",
    );
    assert_eq!(out.status.code(), Some(18), "{out:?}");
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let object: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(object["exit"], 18, "{object}");
    assert_eq!(object["code"], "stale_grant_key", "{object}");
    assert!(object["current_key"].is_string(), "{object}");
    assert!(object["error"].is_string(), "{object}");
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.starts_with("relais trust grant: "), "{stderr}");
}

/// an-t33: `trust show --json` carries the same `recorded_on` as the grant.
#[test]
fn a_json_trust_show_says_where_settings_live() {
    let scratch = Scratch::with_machine_toml("shared-state-show-json", "schema_version = 1\n");
    let out = scratch.relais(&["trust", "show", "--json"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let object: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(object["recorded_on"], "machine_settings", "{object}");
    let text = scratch.relais(&["trust", "show"], "");
    assert_eq!(text.status.code(), Some(0), "{text:?}");
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("machine.toml: "), "{stdout}");
    assert!(!stdout.contains("the shared primary"), "{stdout}");
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
        // This is also an-t32's pin of `decide`'s error arm under `--json`.
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

/// The `--json` contract for an error that is not a refusal (an-t32):
/// stderr is the one `relais: ` line, exit 3, and stdout is empty without
/// `--json` and exactly the `{"error","exit"}` object with it, the object
/// naming the same error the line does.
fn assert_error_streams(args: &[&str], out: &Output) {
    let json = args.contains(&"--json");
    assert_eq!(out.status.code(), Some(3), "{args:?}: {out:?}");
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
    let stderr = String::from_utf8(out.stderr.clone()).expect("utf-8");
    assert_eq!(stderr.lines().count(), 1, "{args:?}: {stderr}");
    assert!(stderr.starts_with("relais: "), "{args:?}: {stderr}");
    assert!(
        stderr.contains("sets shared_state = \"external\""),
        "{args:?}: {stderr}"
    );
    if json {
        assert_eq!(stdout.lines().count(), 1, "{args:?}: {stdout}");
        let object: serde_json::Value = serde_json::from_str(&stdout).expect("json");
        assert_eq!(object["exit"], 3, "{args:?}: {stdout}");
        assert_eq!(
            format!("relais: {}", object["error"].as_str().expect("error")),
            stderr.trim_end(),
            "{args:?}: the same error on both streams"
        );
    } else {
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
    }
}

/// an-t29: `relais report` reads one price table, so builds one state, for
/// the import and the routing section, before the import. Under an absent
/// external layer both forms exit 3 with the one sentence, once, and print
/// nothing else (`LocalState` holds no state;
/// `every_command_builds_its_shared_state_once` counts the builds). an-t32:
/// under `--json` stdout is the error object.
#[test]
fn a_report_under_an_absent_external_layer_is_refused_before_the_import() {
    for args in [
        &["report"][..],
        &["report", "--no-import"][..],
        &["report", "--json"][..],
        &["report", "--no-import", "--json"][..],
    ] {
        let scratch = Scratch::with_machine_toml(
            "shared-state-report-external",
            "schema_version = 1\nshared_state = \"external\"\n",
        );
        let out = scratch.relais(args, "");
        assert_error_streams(args, &out);
    }
}

/// an-t29: `relais learn cycle` builds its one state where it reads the
/// machine settings, before it starts a coordinator, so an absent external
/// layer exits 3 with the one sentence and leaves no coordinator socket
/// (`socket_path()` is `RELAIS_STATE_DIR/relais.sock`). an-t32: `--json`
/// adds the error object on stdout.
#[test]
fn a_learn_cycle_under_an_absent_external_layer_is_refused_before_the_coordinator() {
    for args in [&["learn", "cycle"][..], &["learn", "cycle", "--json"][..]] {
        let scratch = Scratch::with_machine_toml(
            "shared-state-cycle-external",
            "schema_version = 1\nshared_state = \"external\"\n",
        );
        let out = scratch.relais(args, "");
        assert_error_streams(args, &out);
        assert!(
            !scratch.dir.join("state/relais.sock").exists(),
            "{args:?}: no coordinator was started"
        );
    }
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

/// an-t35: `relais counter` under an absent external layer follows the
/// an-t32 contract: exit 3, one `relais: ` line, and with `--json` the
/// error object on stdout.
#[test]
fn a_counter_under_an_absent_external_layer_is_refused_on_both_streams() {
    for args in [&["counter"][..], &["counter", "--json"][..]] {
        let scratch = Scratch::with_machine_toml(
            "shared-state-counter-external",
            "schema_version = 1\nshared_state = \"external\"\n",
        );
        let out = scratch.relais(args, "");
        assert_error_streams(args, &out);
    }
}

/// an-t35: a local node has no counter: one object, `no_counter` on every
/// day, today included, the dropped sum read from this node's own log.
#[test]
fn a_local_counter_is_one_object_with_no_counter_verdicts() {
    let scratch = Scratch::with_machine_toml("shared-state-counter-local", "schema_version = 1\n");
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    std::fs::write(
        scratch.dir.join("state/coordinator.log"),
        format!("{stamp} relais coordinator: seat settles: dropped 1 waiting settle(s) (dropped_micros=42)\n"),
    )
    .expect("log");
    let out = scratch.relais(&["counter", "--days", "3", "--json"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    let object: serde_json::Value = serde_json::from_slice(&out.stdout).expect("one object");
    assert_eq!(object["selection"], "local");
    let days = object["days"].as_array().expect("days");
    assert_eq!(days.len(), 3);
    assert!(
        days.iter().all(|day| day["check"] == "no_counter"),
        "{days:?}"
    );
    assert_eq!(days[0]["dropped_micros"], 42, "newest first, the log day");
    assert_eq!(object["log"]["unparsed"], 0);
    assert_eq!(object["waiting"]["count"], 0);
}

/// an-t35: the text form fits 80 columns, has no escape byte and keeps
/// stderr empty; a bad `--days` is exit 2 with one line, and the object
/// under `--json`.
#[test]
fn the_counter_text_fits_and_a_bad_day_count_is_exit_2() {
    let scratch = Scratch::with_machine_toml("shared-state-counter-text", "schema_version = 1\n");
    let out = scratch.relais(&["counter"], "");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    let text = String::from_utf8(out.stdout).expect("utf-8");
    assert!(
        text.lines().all(|line| line.chars().count() <= 80),
        "{text}"
    );
    assert!(!text.contains('\u{1b}'), "{text}");
    assert!(text.contains("no shared counter"), "{text}");

    let out = scratch.relais(&["counter", "--days", "0"], "");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stderr)
            .expect("utf-8")
            .lines()
            .count(),
        1
    );
    let out = scratch.relais(&["counter", "--days", "40", "--json"], "");
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let object: serde_json::Value = serde_json::from_slice(&out.stdout).expect("object");
    assert_eq!(object["exit"], 2);
}

/// an-t35: a reader that closes stdout before the first write is not a
/// failure: the binary's own exit is 0 and stderr is empty. The output is
/// under the pipe buffer, so a shell `head` could not raise EPIPE; the
/// read end is dropped before the child writes.
#[test]
fn a_counter_whose_stdout_is_closed_still_exits_0() {
    let scratch = Scratch::with_machine_toml("shared-state-counter-pipe", "schema_version = 1\n");
    for args in [&["counter"][..], &["counter", "--json"][..]] {
        let mut child = Command::new(BIN)
            .args(args)
            .current_dir(repo_root())
            .env("HOME", scratch.dir.join("home"))
            .env("RELAIS_CONFIG_DIR", scratch.dir.join("config"))
            .env("RELAIS_STATE_DIR", scratch.dir.join("state"))
            .env("CLAUDE_CONFIG_DIR", scratch.dir.join("claude"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("relais runs");
        drop(child.stdout.take());
        let out = child.wait_with_output().expect("exits");
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert!(out.stderr.is_empty(), "{args:?}: {out:?}");
    }
}

/// A coordinator that answers `ping` (at `protocol`), `acquire_training` (with
/// `decision`) and `end_training`, and counts the last. Its replies are
/// encoded from the crate's own `Response`. It is bound before the binary
/// runs: any other ping failure makes `ensure_running` start a real
/// detached daemon.
struct FakeCoordinator {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ended: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeCoordinator {
    fn start(socket: &Path, protocol: u32, decision: relais::admission::TrainingDecision) -> Self {
        use relais::coordinator::{Request, Response};
        use std::io::{BufRead, BufReader};
        use std::sync::atomic::Ordering;
        let listener = std::os::unix::net::UnixListener::bind(socket).expect("fake socket");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (stop_in, ended_in) = (stop.clone(), ended.clone());
        let thread = std::thread::spawn(move || {
            while !stop_in.load(Ordering::SeqCst) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).expect("blocking");
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let response = match serde_json::from_str::<Request>(line.trim()) {
                    Ok(Request::Ping) => Response::Pong {
                        pid: std::process::id(),
                        version: "fake".into(),
                        protocol,
                    },
                    Ok(Request::AcquireTraining { .. }) => Response::Training {
                        decision: decision.clone(),
                    },
                    Ok(Request::EndTraining { .. }) => {
                        ended_in.fetch_add(1, Ordering::SeqCst);
                        Response::Ack
                    }
                    _ => Response::Error {
                        detail: "the fake serves ping and training only".into(),
                    },
                };
                let mut stream = stream;
                let payload = serde_json::to_string(&response).expect("response");
                let _ = writeln!(stream, "{payload}");
            }
        });
        Self {
            stop,
            ended,
            thread: Some(thread),
        }
    }

    fn ended(&self) -> usize {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for FakeCoordinator {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The failure contract of a command that says `relais <command>: <message>`:
/// plain, one stderr line (or the message's own lines) and empty stdout;
/// under `--json` the one `{"error","exit"}` object whose message is the
/// stderr text minus its prefix.
fn assert_failure_streams(prefix: &str, args: &[&str], out: &Output, exit: i32) -> String {
    assert_eq!(out.status.code(), Some(exit), "{args:?}: {out:?}");
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
    let stderr = String::from_utf8(out.stderr.clone()).expect("utf-8");
    assert!(stderr.starts_with(prefix), "{args:?}: {stderr}");
    let message = stderr
        .trim_end()
        .strip_prefix(prefix)
        .expect("prefix")
        .to_string();
    if args.contains(&"--json") {
        assert_eq!(stdout.lines().count(), 1, "{args:?}: {stdout}");
        let object: serde_json::Value = serde_json::from_str(&stdout).expect("json");
        assert_eq!(object["exit"], exit, "{args:?}: {stdout}");
        assert_eq!(
            object["error"].as_str().expect("error").trim_end(),
            message,
            "{args:?}"
        );
    } else {
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
    }
    message
}

/// an-t37: the coordinator's refusal of the Training lease (a hard limit) is
/// exit 1, one `relais learn cycle:` line naming the code and detail, and
/// under `--json` the error object. Nothing is held, so nothing is given back.
#[test]
fn a_refused_training_lease_follows_the_json_error_contract() {
    for json in [false, true] {
        let scratch = Scratch::short("shared-state-refused");
        let fake = FakeCoordinator::start(
            &scratch.dir.join("state/relais.sock"),
            relais::coordinator::PROTOCOL_VERSION,
            relais::admission::TrainingDecision::Refused {
                code: relais::admission::Refusal::DepthExceeded,
                detail: "depth 9 is over 8".into(),
            },
        );
        let mut args = vec!["learn", "cycle"];
        if json {
            args.push("--json");
        }
        let out = scratch.relais(&args, "");
        let message = assert_failure_streams("relais learn cycle: ", &args, &out, 1);
        assert!(
            message.contains("the coordinator refused the Training lease (depth_exceeded)")
                && message.contains("depth 9 is over 8"),
            "{message}"
        );
        assert_eq!(fake.ended(), 0, "no lease, nothing to give back");
    }
}

/// an-t37: a Training lease held by a process the coordinator did not record
/// is exit 20 and says so, without a holder to name.
#[test]
fn a_busy_lease_with_no_recorded_holder_is_exit_20_and_the_error_object() {
    let scratch = Scratch::short("shared-state-busy");
    let _fake = FakeCoordinator::start(
        &scratch.dir.join("state/relais.sock"),
        relais::coordinator::PROTOCOL_VERSION,
        relais::admission::TrainingDecision::Busy { holder: None },
    );
    let args = ["learn", "cycle", "--json"];
    let out = scratch.relais(&args, "");
    let message = assert_failure_streams("relais learn cycle: ", &args, &out, 20);
    assert!(message.contains("its holder is not recorded"), "{message}");
}

/// an-t37: a daemon that speaks another protocol is named, exit 1, and no
/// second coordinator is started over it.
#[test]
fn a_coordinator_of_another_protocol_follows_the_json_error_contract() {
    for json in [false, true] {
        let scratch = Scratch::short("shared-state-skew");
        let _fake = FakeCoordinator::start(
            &scratch.dir.join("state/relais.sock"),
            relais::coordinator::PROTOCOL_VERSION + 1,
            relais::admission::TrainingDecision::Granted,
        );
        let mut args = vec!["learn", "cycle"];
        if json {
            args.push("--json");
        }
        let out = scratch.relais(&args, "");
        let message = assert_failure_streams("relais learn cycle: ", &args, &out, 1);
        assert!(!message.is_empty());
    }
}

/// an-t37: a policy that does not parse, after the lease was granted, is
/// exit 1 and the error object, and the lease is given back exactly once.
#[test]
fn a_policy_failure_gives_the_training_lease_back_once() {
    for json in [false, true] {
        let scratch = Scratch::short("shared-state-policy");
        let repo = scratch.dir.join("repo");
        std::fs::create_dir_all(&repo).expect("repo");
        let git = scratch
            .command_in("git", &repo)
            .args(["init", "-q"])
            .output()
            .expect("git init");
        assert!(git.status.success());
        std::fs::write(repo.join("relais.toml"), "this is = = not toml\n").expect("policy");
        let fake = FakeCoordinator::start(
            &scratch.dir.join("state/relais.sock"),
            relais::coordinator::PROTOCOL_VERSION,
            relais::admission::TrainingDecision::Granted,
        );
        let mut args = vec!["learn", "cycle"];
        if json {
            args.push("--json");
        }
        let out = scratch
            .command_in(BIN, &repo)
            .args(&args)
            .output()
            .expect("relais runs");
        let message = assert_failure_streams("relais learn cycle: ", &args, &out, 1);
        assert!(message.contains("relais.toml"), "{message}");
        assert_eq!(fake.ended(), 1, "the lease was given back once");
    }
}

/// an-t37: `doctor --json` whose command fails before it can report (here the
/// working directory was removed under it) is the error object on stdout and
/// the same message on stderr, exit 3; plain, stdout is empty.
#[test]
fn a_failed_doctor_follows_the_json_error_contract() {
    for json in [false, true] {
        let scratch = Scratch::with_machine_toml("shared-state-doctor", "schema_version = 1\n");
        let gone = scratch.dir.join("gone");
        std::fs::create_dir_all(&gone).expect("dir");
        let mut args = vec!["doctor"];
        if json {
            args.push("--json");
        }
        let out = scratch
            .command_in("sh", &scratch.dir)
            .arg("-c")
            .arg(r#"cd "$1" && rmdir "$1" && shift && exec "$@""#)
            .arg("sh")
            .arg(&gone)
            .arg(BIN)
            .args(&args)
            .output()
            .expect("sh runs");
        let message = assert_failure_streams("relais: ", &args, &out, 3);
        assert!(message.contains("working directory"), "{message}");
    }
}

/// an-t37: a stale holdout review keeps its `code` and `detail` on the one
/// error object, exit 3 (reached with no `[holdouts]` store: `accept`
/// refuses with its own text, the same arm that prints the fields).
#[test]
fn a_stale_holdout_review_is_one_error_object_with_its_code_and_detail() {
    let scratch = Scratch::with_machine_toml("shared-state-holdout", "schema_version = 1\n");
    let args = [
        "trust",
        "holdouts",
        "--accept",
        "h",
        "--sha",
        "abc",
        "--fingerprint",
        "def",
        "--json",
    ];
    let out = scratch.relais(&args, "");
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let stdout = String::from_utf8(out.stdout.clone()).expect("utf-8");
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    let object: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(object["exit"], 3, "{object}");
    assert_eq!(object["code"], "stale_holdout_review", "{object}");
    let detail = object["detail"].as_str().expect("detail");
    assert_eq!(
        object["error"].as_str().expect("error"),
        format!("refused: {detail}")
    );
    let stderr = String::from_utf8(out.stderr).expect("utf-8");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert_eq!(
        stderr.trim_end(),
        format!(
            "relais trust holdouts: {}",
            object["error"].as_str().unwrap()
        )
    );
}
