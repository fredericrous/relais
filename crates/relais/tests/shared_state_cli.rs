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
        for sub in ["home", "config", "state"] {
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
