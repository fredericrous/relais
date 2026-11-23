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
