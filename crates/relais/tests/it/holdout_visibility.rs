//! `relais doctor` and `relais plan` on holdouts the store declares
//! (SPEC §4): the listing with source and confirmation state, and the mark
//! on an attached store holdout nobody has confirmed. Through the real
//! binary, with a local git repository standing in for the store.

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use relais::test_support::short_temp_dir;
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

struct World {
    _scratch: relais::test_support::TempDir,
    _permit: common::WorldPermit,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl World {
    /// A repository with a policy, and a store holding `attached` (attaches
    /// to a `feature` touching `src/`) and `plain` (no attach rule).
    fn new(tag: &str) -> Self {
        let permit = common::WorldPermit::acquire(1);
        let scratch = short_temp_dir(&format!("hv-{tag}"));
        let root = scratch.to_path_buf();
        let world = Self {
            _scratch: scratch,
            _permit: permit,
            repo: root.join("repo"),
            state: root.join("state"),
            config: root.join("cfg"),
            root,
        };
        for dir in [&world.repo, &world.state, &world.config] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        let no_hooks = world.root.join("no-hooks");
        std::fs::create_dir_all(&no_hooks).expect("no-hooks directory");
        world.init_git(&world.repo, &no_hooks);
        std::fs::create_dir_all(world.repo.join("src")).expect("src");
        std::fs::write(world.repo.join("src/main.rs"), "fn main() {}\n").expect("write");
        std::fs::write(world.repo.join("relais.toml"), POLICY).expect("policy");
        git(&world.repo, &["add", "-A"]);
        git(&world.repo, &["commit", "-q", "-m", "base"]);
        let store = world.root.join("store");
        std::fs::create_dir_all(&store).expect("store");
        world.init_git(&store, &no_hooks);
        world.commit_store(&[
            (
                "repo/attached/holdout.toml",
                "attach = { task_types = [\"feature\"], paths = [\"src/**\"] }\n",
            ),
            ("repo/attached/check.sh", "exit 0\n"),
            ("repo/plain/check.sh", "exit 0\n"),
        ]);
        std::fs::write(
            world.config.join("machine.toml"),
            format!(
                "schema_version = 1\n\n[holdouts]\nstore = \"file://{}\"\nref = \"main\"\n",
                store.display()
            ),
        )
        .expect("machine");
        world
    }

    fn init_git(&self, dir: &Path, no_hooks: &Path) {
        git(dir, &["init", "-q", "-b", "main"]);
        git(
            dir,
            &["config", "core.hooksPath", &no_hooks.to_string_lossy()],
        );
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
    }

    fn commit_store(&self, files: &[(&str, &str)]) {
        let store = self.root.join("store");
        for (path, content) in files {
            let file = store.join(path);
            std::fs::create_dir_all(file.parent().expect("parent")).expect("dirs");
            std::fs::write(&file, content).expect("write");
        }
        git(&store, &["add", "-A"]);
        git(&store, &["commit", "-q", "-m", "store"]);
    }

    fn relais(&self, args: &[&str]) -> Output {
        common::relais(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", &self.config)
            .env("RELAIS_CLAUDE_BIN", self.root.join("no-such-claude"))
            .env("RELAIS_SESSION_ID", "tab-test")
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .expect("relais runs")
    }

    fn task(&self) -> PathBuf {
        let path = self.root.join("task.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "task_type": "feature",
                "objective": "Change the entry point",
                "base_ref": "HEAD",
                "write_scope": ["src/**"],
                "acceptance": ["src/main.rs changes"],
                "verification_profile": "default",
                "review": "off",
            })
            .to_string(),
        )
        .expect("task");
        path
    }

    /// The `holdouts` finding's lines, from `relais doctor`.
    fn doctor_holdouts(&self) -> String {
        let output = self.relais(&["doctor"]);
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let start = text
            .lines()
            .position(|line| line.contains("holdouts "))
            .unwrap_or_else(|| panic!("no holdouts finding in {text}"));
        let mut block = Vec::new();
        for (at, line) in text.lines().enumerate().skip(start) {
            if at > start && !line.starts_with("                ") {
                break;
            }
            block.push(line);
        }
        block.join("\n")
    }
}

const POLICY: &str = r#"schema_version = 1

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
"#;

fn confirm_all(world: &World) {
    let review = world.relais(&["trust", "holdouts", "--json"]);
    assert_eq!(
        review.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&review.stderr)
    );
    let reviews: Value = serde_json::from_slice(&review.stdout).expect("json");
    for holdout in reviews.as_array().expect("a list") {
        let accepted = world.relais(&[
            "trust",
            "holdouts",
            "--accept",
            holdout["name"].as_str().expect("name"),
            "--sha",
            holdout["store_sha"].as_str().expect("sha"),
            "--fingerprint",
            holdout["fingerprint"].as_str().expect("fingerprint"),
        ]);
        assert_eq!(
            accepted.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&accepted.stderr)
        );
    }
}

#[test]
fn doctor_lists_each_discovered_holdout_with_its_source_and_confirmation_state() {
    let world = World::new("doctor");
    let listed = world.doctor_holdouts();
    assert!(listed.contains("attached  source store  new"), "{listed}");
    assert!(listed.contains("plain  source store  new"), "{listed}");
    assert!(
        listed.contains("task types feature; paths src/**"),
        "{listed}"
    );

    confirm_all(&world);
    let listed = world.doctor_holdouts();
    assert!(
        listed.contains("attached  source store  confirmed"),
        "{listed}"
    );

    world.commit_store(&[("repo/plain/check.sh", "exit 1\n")]);
    let listed = world.doctor_holdouts();
    assert!(listed.contains("plain  source store  changed"), "{listed}");

    std::fs::write(
        world.config.join("machine.toml"),
        std::fs::read_to_string(world.config.join("machine.toml")).expect("machine")
            + "\n[holdouts.repo.plain]\npath = \"repo/plain/check.sh\"\noverlay = \"p.sh\"\n\
             command = { argv = [\"sh\", \"p.sh\"], junit = \"j.xml\" }\n",
    )
    .expect("machine");
    let listed = world.doctor_holdouts();
    assert!(
        listed.contains("plain  source machine  shadowed"),
        "{listed}"
    );
}

#[test]
fn doctor_shows_an_unreachable_store() {
    let world = World::new("unreachable");
    std::fs::remove_dir_all(world.root.join("store")).expect("remove the store");
    let listed = world.doctor_holdouts();
    assert!(listed.contains("holdout_store_unreachable"), "{listed}");
}

#[test]
fn plan_json_marks_an_attached_store_holdout_that_is_not_confirmed() {
    let world = World::new("plan");
    let task = world.task();
    let plan = |world: &World| -> Value {
        let output = world.relais(&["plan", "--json", "--task", &task.to_string_lossy()]);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "{e}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    };
    let before = plan(&world);
    let attached = &before["holdouts"]["attached"][0];
    assert_eq!(attached["name"], "attached", "{before}");
    assert_eq!(attached["unconfirmed"], true, "{before}");
    assert_eq!(attached["confirmation"], "new", "{before}");

    confirm_all(&world);
    let after = plan(&world);
    let attached = &after["holdouts"]["attached"][0];
    assert_eq!(attached["name"], "attached", "{after}");
    assert_eq!(attached["unconfirmed"], false, "{after}");

    let text = world.relais(&["plan", "--task", &task.to_string_lossy()]);
    assert!(
        String::from_utf8_lossy(&text.stdout).contains("holdout attached attached"),
        "{}",
        String::from_utf8_lossy(&text.stdout)
    );
}
