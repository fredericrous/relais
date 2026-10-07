//! `relais install --claude`, `uninstall --claude` and `doctor` against
//! the relais Claude Code plugin, through the real binary: a fake `claude`
//! on PATH that records its argv and answers `plugin list --json` from a
//! fixture, a temp HOME, and a temp state directory. Nothing here touches
//! the real `~/.claude`, and the real Claude Code is never run.
//!
//! Unix only: the fake `claude` is a `sh` script.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use relais::ids::sha256_hex;
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Records every call, answers `plugin list --json` with `list.json`, and
/// fails the one call whose arguments are the first line of `fail`,
/// printing the rest of that file on stderr.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_CLAUDE_DIR/argv.log"
if [ "$1 $2 $3" = "plugin list --json" ]; then
    cat "$FAKE_CLAUDE_DIR/list.json"
    exit 0
fi
if [ -f "$FAKE_CLAUDE_DIR/fail" ] && [ "$*" = "$(head -n 1 "$FAKE_CLAUDE_DIR/fail")" ]; then
    tail -n +2 "$FAKE_CLAUDE_DIR/fail" >&2
    exit 1
fi
exit 0
"#;

struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    /// The fake's directory: its `claude`, `argv.log`, `list.json`, `fail`.
    fake: PathBuf,
    project: PathBuf,
    state: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let scratch = short_temp_dir(&format!("pi-{tag}"));
        let root = scratch.to_path_buf();
        let fake = root.join("fake");
        let project = root.join("project");
        let state = root.join("state");
        for dir in [&fake, &project, &state, &root.join("cfg")] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        let claude = fake.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).expect("fake claude");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let world = Self {
            _scratch: scratch,
            root,
            fake,
            project,
            state,
        };
        world.list("[]");
        world
    }

    /// What `claude plugin list --json` answers.
    fn list(&self, json: &str) {
        std::fs::write(self.fake.join("list.json"), json).expect("list.json");
    }

    fn list_with(&self, version: &str, enabled: bool) {
        self.list(&format!(
            r#"[{{"id": "relais@relais-local", "version": "{version}", "scope": "user",
                 "enabled": {enabled}, "installPath": "/cache", "readFromFolder": true}}]"#
        ));
    }

    /// Make the call `args` fail with `stderr`.
    fn fail(&self, args: &str, stderr: &str) {
        std::fs::write(self.fake.join("fail"), format!("{args}\n{stderr}\n")).expect("fail");
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.fake.join("argv.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn marketplace(&self) -> PathBuf {
        self.state.join("claude-marketplace")
    }

    fn relais(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.project)
            .env_remove("RELAIS_CLAUDE_BIN")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.fake.to_string_lossy()),
            )
            .env("FAKE_CLAUDE_DIR", &self.fake)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", self.root.join("cfg"))
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .output()
            .expect("relais runs")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[test]
fn a_first_install_writes_the_marketplace_then_adds_it_and_installs_the_plugin() {
    let world = World::new("first");
    let out = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let marketplace = world.marketplace();
    assert_eq!(
        world.calls(),
        vec![
            "plugin list --json".to_string(),
            format!("plugin marketplace add {}", marketplace.display()),
            "plugin install relais@relais-local".to_string(),
        ]
    );
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(marketplace.join(".claude-plugin/marketplace.json"))
            .expect("marketplace.json"),
    )
    .expect("json");
    assert_eq!(manifest["name"], "relais-local");
    assert_eq!(manifest["plugins"][0]["source"], "./plugins/relais");
    let plugin = marketplace.join("plugins/relais");
    let plugin_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).expect("plugin.json"),
    )
    .expect("json");
    assert_eq!(plugin_json["version"], VERSION);
    assert!(plugin.join("skills/relais/SKILL.md").is_file());
    assert!(plugin.join("hooks/register.ts").is_file());
    assert!(
        !plugin.join("tests").exists(),
        "development files are not shipped"
    );
    // The skill and the workers are the plugin's now, not the project's.
    assert!(!world
        .project
        .join(".claude/skills/relais/SKILL.md")
        .exists());
    assert!(world
        .project
        .join(".claude/skills/relais-verified-push/SKILL.md")
        .is_file());
    assert!(!world
        .project
        .join(".claude/agents/relais-worker-sonnet-medium.md")
        .exists());
}

#[test]
fn a_second_install_updates_the_marketplace_and_the_plugin() {
    let world = World::new("second");
    world.list_with("0.8.0", true);
    let out = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        world.calls(),
        vec![
            "plugin list --json".to_string(),
            "plugin marketplace update relais-local".to_string(),
            "plugin update relais@relais-local".to_string(),
        ]
    );
    assert!(world
        .marketplace()
        .join("plugins/relais/skills/relais/SKILL.md")
        .is_file());
}

#[test]
fn a_preview_names_the_steps_and_writes_and_runs_nothing() {
    let world = World::new("preview");
    let out = world.relais(&["install", "--claude"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let shown = text(&out.stdout);
    assert!(shown.contains("plugin relais@relais-local"), "{shown}");
    assert!(shown.contains("claude plugin marketplace add"), "{shown}");
    assert!(
        shown.contains("claude plugin install relais@relais-local"),
        "{shown}"
    );
    assert!(shown.contains("preview only"), "{shown}");
    assert_eq!(world.calls(), vec!["plugin list --json".to_string()]);
    assert!(!world.marketplace().exists());
}

#[test]
fn a_claude_that_fails_fails_the_install_with_its_stderr() {
    let world = World::new("fails");
    world.fail(
        "plugin install relais@relais-local",
        "marketplace is broken",
    );
    let out = world.relais(&["install", "--claude", "--write"]);
    assert_ne!(out.status.code(), Some(0));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("marketplace is broken"), "{stderr}");
    assert!(
        stderr.contains("claude plugin install relais@relais-local"),
        "{stderr}"
    );

    let world = World::new("fails-add");
    world.fail(
        &format!("plugin marketplace add {}", world.marketplace().display()),
        "cannot add",
    );
    let out = world.relais(&["install", "--claude", "--write"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(text(&out.stderr).contains("cannot add"));
    assert!(
        !world
            .calls()
            .contains(&"plugin install relais@relais-local".to_string()),
        "nothing runs after a failed step: {:?}",
        world.calls()
    );
}

#[test]
fn an_install_without_claude_fails_and_says_so() {
    let world = World::new("no-claude");
    std::fs::remove_file(world.fake.join("claude")).expect("remove");
    let out = world.relais(&["install", "--claude", "--write"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(
        text(&out.stderr).contains("no Claude Code to run"),
        "{}",
        text(&out.stderr)
    );
}

#[test]
fn uninstall_removes_the_plugin_the_marketplace_and_the_directory() {
    let world = World::new("uninstall");
    let out = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    std::fs::remove_file(world.fake.join("argv.log")).expect("reset the log");

    let out = world.relais(&["uninstall", "--claude", "--write"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        world.calls(),
        vec![
            "plugin uninstall relais@relais-local".to_string(),
            "plugin marketplace remove relais-local".to_string(),
        ]
    );
    assert!(!world.marketplace().exists());
    assert!(
        !world
            .project
            .join(".claude/agents/relais-research.md")
            .exists(),
        "the owned files go too"
    );
}

#[test]
fn uninstall_accepts_what_is_already_absent_and_reports_any_other_failure() {
    let world = World::new("uninstall-absent");
    world.fail(
        "plugin uninstall relais@relais-local",
        "Plugin relais@relais-local not found",
    );
    let out = world.relais(&["uninstall", "--claude", "--write"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(
        text(&out.stdout).contains("already gone"),
        "{}",
        text(&out.stdout)
    );
    assert_eq!(world.calls().len(), 2, "{:?}", world.calls());

    let world = World::new("uninstall-fails");
    std::fs::create_dir_all(world.marketplace()).expect("mkdir");
    world.fail("plugin marketplace remove relais-local", "disk on fire");
    let out = world.relais(&["uninstall", "--claude", "--write"]);
    assert_ne!(out.status.code(), Some(0));
    assert!(text(&out.stderr).contains("disk on fire"));
    assert!(
        world.marketplace().exists(),
        "the directory stays while Claude Code may still read it"
    );
}

/// What an earlier relais wrote: the file, marked with the sha of its body.
fn write_owned(path: &Path, name: &str, body: &str) {
    let content = format!("name: {name}\n---\n\n{body}");
    let sha = sha256_hex(content.trim_end_matches('\n').as_bytes());
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        path,
        format!("---\n# relais:begin {sha}\n{content}\n<!-- relais:end -->\n"),
    )
    .expect("write");
}

#[test]
fn an_install_removes_the_old_skill_and_workers_unless_they_were_edited() {
    let world = World::new("retire");
    let claude = world.project.join(".claude");
    let skill = claude.join("skills/relais/SKILL.md");
    let worker = claude.join("agents/relais-worker-sonnet-medium.md");
    let edited = claude.join("agents/relais-worker-opus-high.md");
    write_owned(&skill, "relais", "the old skill\n");
    write_owned(&worker, "relais-worker-sonnet-medium", "a worker\n");
    write_owned(&edited, "relais-worker-opus-high", "a worker\n");
    std::fs::write(
        &edited,
        std::fs::read_to_string(&edited)
            .expect("read")
            .replace("a worker", "MY worker"),
    )
    .expect("edit");

    let out = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(!skill.exists(), "an unedited old skill is removed");
    assert!(!worker.exists(), "an unedited old worker is removed");
    assert!(
        std::fs::read_to_string(&edited)
            .expect("kept")
            .contains("MY worker"),
        "an edited one is kept"
    );
    let shown = text(&out.stdout);
    assert!(shown.contains("remove  skills/relais/SKILL.md"), "{shown}");
    assert!(
        shown.contains("keep    agents/relais-worker-opus-high.md"),
        "{shown}"
    );
}

fn plugin_finding(world: &World, json: bool) -> (String, String) {
    let args: &[&str] = match json {
        true => &["doctor", "--json"],
        false => &["doctor"],
    };
    let out = world.relais(args);
    let shown = text(&out.stdout);
    match json {
        true => {
            let report: serde_json::Value = serde_json::from_str(shown.trim()).expect("json");
            let finding = report["findings"]
                .as_array()
                .expect("findings")
                .iter()
                .find(|finding| finding["component"] == "plugin")
                .expect("a plugin finding")
                .clone();
            (
                finding["level"].as_str().expect("level").to_string(),
                finding["detail"].as_str().expect("detail").to_string(),
            )
        }
        false => {
            let line = shown
                .lines()
                .find(|line| line.contains(" plugin "))
                .unwrap_or_else(|| panic!("a plugin line:\n{shown}"));
            let level = match line.trim_start().chars().next() {
                Some('✓') => "ok",
                Some('!') => "warn",
                _ => "fail",
            };
            (level.to_string(), line.to_string())
        }
    }
}

#[test]
fn doctor_reports_the_plugin_ok_when_enabled_at_this_version() {
    let world = World::new("doctor-ok");
    world.list_with(VERSION, true);
    for json in [true, false] {
        let (level, detail) = plugin_finding(&world, json);
        assert_eq!(level, "ok", "{detail}");
        assert!(detail.contains("relais@relais-local"), "{detail}");
    }
}

#[test]
fn doctor_warns_with_the_fix_when_the_plugin_is_absent_disabled_or_stale() {
    let world = World::new("doctor-warn");
    for (setup, says) in [
        (None, "not installed"),
        (Some((VERSION, false)), "disabled"),
        (Some(("0.0.1", true)), "is at 0.0.1"),
    ] {
        match setup {
            None => world.list("[]"),
            Some((version, enabled)) => world.list_with(version, enabled),
        }
        for json in [true, false] {
            let (level, detail) = plugin_finding(&world, json);
            assert_eq!(level, "warn", "{says}: {detail}");
            assert!(detail.contains(says), "{says}: {detail}");
            assert!(
                detail.contains("relais install --claude"),
                "{says}: {detail}"
            );
        }
    }
}
