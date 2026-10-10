//! Release acceptance scenarios that need no worker (SPEC §14) —
//! through the real binary, on EVERY platform CI builds for.
//!
//! The sibling suite, `release_scenarios.rs`, drives a fake `claude`
//! that is a `sh` script, so the whole file is `#![cfg(unix)]`. Half of
//! what it proves has nothing to do with a worker: `init`, `install`,
//! `uninstall`, `doctor`, a `plan` that blocks on a missing trust
//! grant, `coordinator status` with no daemon, and the exit-code table.
//! Those ran on one platform out of two, and the shipped Windows build
//! had its PATH lookup broken for a release because of it (audit C2).
//! They live here, with no shell and no fake worker anywhere.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use relais::policy::RepoPolicy;
use relais::test_support::short_temp_dir;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

/// One isolated world: a git repository, a state directory and a config
/// directory, and nothing else. No fake harness: every scenario here is
/// one that must not launch one.
///
/// `_scratch` is the same `Drop` guard `test_support::short_temp_dir`
/// gives the library's own tests — this suite is a separate crate and
/// cannot reach a `pub(crate)` helper, which is why `test_support` is
/// `pub`. `root` stays a plain `PathBuf`, derived from the guard, since
/// the rest of this file joins paths off it throughout.
struct World {
    _scratch: relais::test_support::TempDir,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    config: PathBuf,
    /// The Claude Code this world's relais finds: one that does not exist,
    /// unless the scenario installs the plugin (`with_plugin_claude`).
    claude: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        // Short paths under /tmp where there is one: the coordinator
        // socket lives in the state directory and a Unix socket path is
        // capped near 100 bytes. Windows has neither /tmp nor the cap.
        let scratch = short_temp_dir(&format!("pt-{tag}"));
        let root = scratch.to_path_buf();
        let repo = root.join("repo");
        let state = root.join("state");
        let config = root.join("cfg");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::create_dir_all(&state).expect("state");
        std::fs::create_dir_all(&config).expect("config");
        let no_hooks = root.join("no-hooks");
        std::fs::create_dir_all(&no_hooks).expect("hooks");
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["config", "core.hooksPath", &no_hooks.to_string_lossy()],
        );
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(repo.join("src/main.rs"), "fn main() {}\n").expect("write");
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        Self {
            _scratch: scratch,
            claude: root.join("no-such-claude"),
            root,
            repo,
            state,
            config,
        }
    }

    /// A `claude` that says no plugin is installed and accepts every other
    /// call, for the scenarios whose subject is the files and the hook
    /// wiring `install --claude` writes beside the plugin. A `sh` script,
    /// so those scenarios run on Unix; the plugin's own steps are driven
    /// in `plugin_install.rs`.
    #[cfg(unix)]
    fn with_plugin_claude(mut self) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let claude = self.root.join("claude");
        let script =
            "#!/bin/sh\nif [ \"$1 $2 $3\" = \"plugin list --json\" ]; then echo '[]'; fi\nexit 0\n";
        std::fs::write(&claude, script).expect("fake claude");
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        self.claude = claude;
        self
    }

    /// The binary, in this world. `RELAIS_CLAUDE_BIN` names a path that
    /// does not exist, so "the harness is missing" is a fact of the
    /// scenario rather than a fact about the machine running it — and
    /// no scenario here can launch a worker by accident.
    fn relais(&self, args: &[&str]) -> Output {
        self.relais_in(&self.repo, args)
    }

    /// The binary, run from another directory of this world — a
    /// worktree of the repository, for the scenarios about what a
    /// worktree resolves to.
    fn relais_in(&self, cwd: &Path, args: &[&str]) -> Output {
        common::relais(BIN)
            .args(args)
            .current_dir(cwd)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", &self.config)
            .env("RELAIS_CLAUDE_BIN", &self.claude)
            .env("RELAIS_SESSION_ID", "tab-test")
            // This world's home, so nothing here reads — or acts on —
            // the home directory of whoever is running `make check`.
            // `doctor` falls back to `~/.claude/settings.json` when the
            // repository has none, and a developer who has installed the
            // hooks has a real relais command recorded there: without
            // this, the suite would spawn that command from inside the
            // test run and assert against whatever it said. A test whose
            // answer depends on the machine's configuration is not a test
            // of this code.
            .env("HOME", &self.root)
            // Windows resolves a home from `USERPROFILE` when `HOME` is
            // unset (`paths::resolve_home`), and the CI matrix runs
            // there, so both have to point into the world.
            .env("USERPROFILE", &self.root)
            .output()
            .expect("relais runs")
    }

    /// A policy whose verification profile names no command: nothing in
    /// this suite runs one, and a shell is not portable.
    fn write_policy(&self) -> String {
        let policy = r#"schema_version = 1

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
        std::fs::write(self.repo.join("relais.toml"), policy).expect("policy");
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", "policy"]);
        RepoPolicy::from_toml_str(policy)
            .expect("valid policy")
            .authority_hash()
    }

    /// A policy declaring one recipe family across two revisions —
    /// `docs-touchup` revision 0 (enabled) and revision 1 (disabled) —
    /// for the `recipe list`/`show`/`diff` scenarios, none of which run
    /// a verification command either.
    fn write_policy_with_recipes(&self) -> String {
        let policy = r#"schema_version = 1

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

[[recipes]]
name = "docs-touchup"
scope_within = ["docs/**"]
tier = "implementation"
revision = 0

[[recipes]]
name = "docs-touchup"
scope_within = ["docs/**"]
tier = "implementation"
revision = 1
enabled = false
"#;
        std::fs::write(self.repo.join("relais.toml"), policy).expect("policy");
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", "policy with recipes"]);
        policy.to_string()
    }

    /// Machine settings that exist and grant nothing: the state a fresh
    /// install is in, and the one a missing trust grant is about.
    fn write_machine_without_a_grant(&self) {
        std::fs::write(self.config.join("machine.toml"), "schema_version = 1\n").expect("machine");
    }

    fn write_task(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(
            &path,
            serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": "Remove the obsolete entry point",
                "base_ref": "HEAD",
                "write_scope": ["src/**"],
                "acceptance": ["src/main.rs no longer exists"],
                "verification_profile": "default",
                "review": "off",
            })
            .to_string(),
        )
        .expect("task");
        path
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // Nothing here starts a daemon, but `plan` and `status` may have
        // asked one to exist; stopping is harmless when none did. The
        // root itself is removed by `_scratch`'s own `Drop`, run after
        // this body, panic or not.
        let _ = self.relais(&["coordinator", "stop"]);
    }
}

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

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

#[test]
fn init_writes_a_valid_policy_once() {
    let world = World::new("init");
    let first = world.relais(&["init"]);
    assert_eq!(first.status.code(), Some(0));
    let policy = std::fs::read_to_string(world.repo.join("relais.toml")).expect("policy");
    RepoPolicy::from_toml_str(&policy).expect("the template is a valid policy");
    let second = world.relais(&["init"]);
    assert_eq!(second.status.code(), Some(2));
    assert!(text(&second.stderr).contains("never overwrites"));
    assert!(
        text(&first.stdout).contains("relais trust show; relais trust grant --key"),
        "{}",
        text(&first.stdout)
    );
}

/// `init --detect --write` writes the proposal it printed, once, and the
/// written policy parses; a second write is refused like plain init.
#[test]
fn init_detect_writes_the_proposal_once() {
    let world = World::new("detect");
    std::fs::write(world.repo.join("Makefile"), "check:\n\ttrue\n").expect("Makefile");
    let first = world.relais(&["init", "--detect", "--write"]);
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr));
    let stdout = text(&first.stdout);
    assert!(
        stdout.contains("  check  make check   Makefile: target check"),
        "{stdout}"
    );
    assert!(
        stdout.lines().all(|line| line.chars().count() <= 80),
        "{stdout}"
    );
    let policy = std::fs::read_to_string(world.repo.join("relais.toml")).expect("policy");
    let parsed = RepoPolicy::from_toml_str(&policy).expect("the proposal is a valid policy");
    assert_eq!(
        parsed.verification.profiles["default"].commands[0].argv,
        ["make", "check"]
    );
    let second = world.relais(&["init", "--detect", "--write"]);
    assert_eq!(second.status.code(), Some(2));
    assert!(text(&second.stderr).contains("never overwrites"));

    let json = world.relais(&["init", "--detect", "--json"]);
    assert_eq!(json.status.code(), Some(0));
    let document: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("--json prints one document");
    assert_eq!(
        document["proposal"]["commands"][0]["argv"],
        serde_json::json!(["make", "check"])
    );
}

/// Nothing to propose: exit 19, `{"proposal": null}`, and no file — the
/// caller asks a person for the command instead.
#[test]
fn init_detect_on_an_empty_repository_exits_19_and_writes_nothing() {
    let world = World::new("detect-none");
    let none = world.relais(&["init", "--detect", "--write", "--json"]);
    assert_eq!(none.status.code(), Some(19), "{}", text(&none.stderr));
    let document: serde_json::Value = serde_json::from_slice(&none.stdout).expect("json");
    assert_eq!(document, serde_json::json!({ "proposal": null }));
    assert!(!world.repo.join("relais.toml").exists());

    let refused = world.relais(&["init", "--detect", "--command", "make test | tee x"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        text(&refused.stderr).contains('|'),
        "{}",
        text(&refused.stderr)
    );

    let typed = world.relais(&["init", "--detect", "--write", "--command", "make test"]);
    assert_eq!(typed.status.code(), Some(0), "{}", text(&typed.stderr));
    let policy = std::fs::read_to_string(world.repo.join("relais.toml")).expect("policy");
    let parsed = RepoPolicy::from_toml_str(&policy).expect("valid");
    assert_eq!(
        parsed.verification.profiles["default"].commands[0].argv,
        ["make", "test"]
    );
}

// SPEC §14: installation and uninstall preserve unrelated configuration
// and modified owned files.
#[test]
#[cfg(unix)]
fn install_is_preview_first_and_uninstall_keeps_foreign_and_modified_files() {
    let world = World::new("install").with_plugin_claude();
    let preview = world.relais(&["install", "--claude"]);
    assert_eq!(preview.status.code(), Some(0));
    assert!(text(&preview.stdout).contains("preview only"));
    assert!(!world
        .repo
        .join(".claude/skills/relais-verified-push/SKILL.md")
        .exists());
    std::fs::create_dir_all(world.repo.join(".claude/agents")).expect("mkdir");
    std::fs::write(world.repo.join(".claude/agents/custom.md"), "# mine\n").expect("foreign");
    let write = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(write.status.code(), Some(0), "{}", text(&write.stderr));
    let skill = world
        .repo
        .join(".claude/skills/relais-verified-push/SKILL.md");
    assert!(skill.exists());
    assert!(world
        .repo
        .join(".claude/agents/relais-research.md")
        .exists());
    std::fs::write(&skill, "# edited by the user\n").expect("modify");
    let uninstall = world.relais(&["uninstall", "--claude", "--write"]);
    assert_eq!(
        uninstall.status.code(),
        Some(0),
        "{}",
        text(&uninstall.stderr)
    );
    assert!(skill.exists(), "a modified owned file is kept");
    assert!(!world
        .repo
        .join(".claude/agents/relais-research.md")
        .exists());
    assert!(
        world.repo.join(".claude/agents/custom.md").exists(),
        "foreign files are kept"
    );
}

// The objective this suite exists for: `relais install --claude` alone
// never touches settings.json, `--hooks` wires all seven targets in and
// a re-run says nothing is left to do, and `relais doctor` exercises the
// recorded command — spawning it for real against a scratch environment
// — rather than merely reading the file back.
#[test]
#[cfg(unix)]
fn install_claude_alone_never_touches_settings_json() {
    let world = World::new("install-no-hooks").with_plugin_claude();
    std::fs::create_dir_all(world.repo.join(".claude")).expect("mkdir");
    let settings = world.repo.join(".claude/settings.json");
    let original = "{\n  \"hooks\": {}\n}\n";
    std::fs::write(&settings, original).expect("write settings");
    let write = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(write.status.code(), Some(0), "{}", text(&write.stderr));
    assert_eq!(
        std::fs::read_to_string(&settings).expect("read"),
        original,
        "settings.json is untouched by a plain --claude install"
    );
}

#[test]
#[cfg(unix)]
fn install_hooks_wires_settings_json_and_a_rerun_is_current() {
    let world = World::new("install-hooks").with_plugin_claude();
    let write = world.relais(&["install", "--claude", "--hooks", "--write"]);
    assert_eq!(write.status.code(), Some(0), "{}", text(&write.stderr));
    let settings_path = world.repo.join(".claude/settings.json");
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).expect("settings"))
            .expect("valid json");
    for event in [
        "PreToolUse",
        "PostToolUse",
        "PostToolUseFailure",
        "SubagentStart",
        "SubagentStop",
        "SessionStart",
        "SessionEnd",
    ] {
        assert!(
            settings["hooks"][event]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "{event} must carry a relais handler: {settings}"
        );
    }
    assert!(
        settings["hooks"].get("WorktreeCreate").is_none(),
        "relais answers no WorktreeCreate: {settings}"
    );
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["matcher"], "Agent|Task",
        "{settings}"
    );
    assert!(settings["hooks"]["SessionStart"][0]
        .get("matcher")
        .is_none());

    let rerun = world.relais(&["install", "--claude", "--hooks", "--write"]);
    assert_eq!(rerun.status.code(), Some(0), "{}", text(&rerun.stderr));
    assert!(
        text(&rerun.stdout).contains("already current"),
        "{}",
        text(&rerun.stdout)
    );

    // Uninstall removes relais's own commands and leaves anything foreign.
    let uninstall = world.relais(&["uninstall", "--claude", "--hooks", "--write"]);
    assert_eq!(
        uninstall.status.code(),
        Some(0),
        "{}",
        text(&uninstall.stderr)
    );
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).expect("settings"))
            .expect("valid json");
    assert!(
        after["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{after}"
    );
}

// A settings file the hook-side native path (#171) wrote — the SendMessage
// matcher and relais's WorktreeCreate entry, beside a foreign WorktreeCreate
// hook — is flagged by `relais doctor`, migrated by the next install with the
// foreign entry kept, and no longer flagged; uninstall removes either form.
#[test]
#[cfg(unix)]
fn install_hooks_migrates_the_native_hook_wiring_in_a_temp_home() {
    let world = World::new("install-migrate").with_plugin_claude();
    std::fs::create_dir_all(world.repo.join(".claude")).expect("mkdir");
    let settings_path = world.repo.join(".claude/settings.json");
    let command = format!("{BIN} hook");
    let relais = serde_json::json!([{"type": "command", "command": command, "timeout": 10}]);
    let foreign = serde_json::json!({"hooks": [
        {"type": "command", "command": "/usr/bin/their-worktree-tool"}
    ]});
    let old = serde_json::json!({"hooks": {
        "PreToolUse": [{"matcher": "Agent|Task|SendMessage", "hooks": relais}],
        "PostToolUse": [{"matcher": "Agent|Task|SendMessage", "hooks": relais}],
        "PostToolUseFailure": [{"matcher": "Agent|Task|SendMessage", "hooks": relais}],
        "SubagentStart": [{"hooks": relais}],
        "SubagentStop": [{"hooks": relais}],
        "SessionStart": [{"hooks": relais}],
        "SessionEnd": [{"hooks": relais}],
        "WorktreeCreate": [{"hooks": [{"type": "command", "command": command, "timeout": 60}]}, foreign]
    }});
    let write_old = || {
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&old).expect("render") + "\n",
        )
        .expect("write settings");
    };
    let wiring_finding = |world: &World| -> Option<serde_json::Value> {
        let report = world.relais(&["doctor", "--json"]);
        let report: serde_json::Value =
            serde_json::from_str(text(&report.stdout).trim()).expect("doctor --json");
        report["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .find(|f| f["component"] == "hook-wiring")
            .cloned()
    };

    write_old();
    let flagged = wiring_finding(&world).expect("doctor flags the old wiring");
    assert!(
        flagged["detail"]
            .as_str()
            .unwrap()
            .contains("relais install --claude"),
        "{flagged}"
    );

    let write = world.relais(&["install", "--claude", "--hooks", "--write"]);
    assert_eq!(write.status.code(), Some(0), "{}", text(&write.stderr));
    let migrated: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).expect("settings"))
            .expect("valid json");
    assert_eq!(migrated["hooks"]["PreToolUse"][0]["matcher"], "Agent|Task");
    assert_eq!(
        migrated["hooks"]["WorktreeCreate"],
        serde_json::json!([foreign]),
        "relais's entry goes, the foreign one stays"
    );
    assert!(wiring_finding(&world).is_none(), "{migrated}");

    // Uninstall removes either form.
    write_old();
    let uninstall = world.relais(&["uninstall", "--claude", "--hooks", "--write"]);
    assert_eq!(
        uninstall.status.code(),
        Some(0),
        "{}",
        text(&uninstall.stderr)
    );
    let after = std::fs::read_to_string(&settings_path).expect("settings");
    assert!(!after.contains(&command), "{after}");
    assert!(after.contains("/usr/bin/their-worktree-tool"), "{after}");
}

// `relais doctor` exercises the hook it finds recorded in settings.json
// by spawning it for real: a fixture spawn payload on stdin, its
// directories redirected to a scratch environment that refuses when the
// coordinator is unreachable (there is none reachable from a scratch
// state directory). The recorded command IS this test binary, so this
// proves the wiring end to end, not just the planning.
#[test]
#[cfg(unix)]
fn doctor_exercises_the_recorded_hook_and_reports_a_refusal() {
    let world = World::new("doctor-hook-live").with_plugin_claude();
    let before = world.relais(&["doctor", "--json"]);
    let report: serde_json::Value =
        serde_json::from_str(text(&before.stdout).trim()).expect("doctor --json is a document");
    let finding = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["component"] == "hook-live")
        .expect("a hook-live finding");
    assert_eq!(finding["level"], "warn", "{finding}");
    assert!(
        finding["detail"]
            .as_str()
            .unwrap()
            .contains("no relais command is recorded"),
        "{finding}"
    );

    let write = world.relais(&["install", "--claude", "--hooks", "--write"]);
    assert_eq!(write.status.code(), Some(0), "{}", text(&write.stderr));

    let after = world.relais(&["doctor", "--json"]);
    let report: serde_json::Value =
        serde_json::from_str(text(&after.stdout).trim()).expect("doctor --json is a document");
    let finding = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["component"] == "hook-live")
        .expect("a hook-live finding");
    assert_eq!(finding["level"], "ok", "{finding}");
    let detail = finding["detail"].as_str().expect("detail");
    assert!(detail.contains("refused"), "{finding}");
    // The command it exercised is the one THIS world wrote, not one from
    // the home directory of whoever is running the suite: `doctor` falls
    // back to `~/.claude/settings.json`, and this world's home is inside
    // it (see `World::relais_in`), so the path it names proves which file
    // it read.
    //
    // Matched on this world's unique directory name rather than on the
    // rendered path: the two disagree about separators on Windows (the
    // detail carries `…\repo\.claude\settings.json`, a `join` here
    // produces `…\repo\.claude/settings.json`) and the name is what
    // actually carries the proof — it holds the pid and a counter, so no
    // settings.json outside this world can contain it.
    let world_name = world
        .root
        .file_name()
        .expect("the world root has a name")
        .to_string_lossy()
        .into_owned();
    assert!(detail.contains(&world_name), "{finding}");
    assert!(detail.contains("settings.json"), "{finding}");
}

// Issue #94, measured on Claude Code 2.1.282: `.claude/settings.json` and
// `.claude/settings.local.json` are merged, and a relais handler recorded
// in both fires twice on every spawn. `install --claude --hooks` must not
// produce that duplicate itself — if one is already wired in the OTHER
// file the harness merges, it refuses to write the one it owns rather
// than adding a second handler behind a person's back.
#[test]
#[cfg(unix)]
fn install_hooks_refuses_when_the_local_settings_file_already_carries_one() {
    let world = World::new("install-hooks-dup").with_plugin_claude();
    std::fs::create_dir_all(world.repo.join(".claude")).expect("mkdir");
    std::fs::write(
        world.repo.join(".claude/settings.local.json"),
        serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Agent|Task", "hooks": [
                        {"type": "command", "command": "/opt/relais/bin/relais hook"}
                    ]}
                ]
            }
        })
        .to_string(),
    )
    .expect("write settings.local.json");

    let write = world.relais(&["install", "--claude", "--hooks", "--write"]);
    assert_eq!(write.status.code(), Some(13), "{}", text(&write.stdout));
    let stderr = text(&write.stderr);
    assert!(stderr.contains("refused"), "{stderr}");
    assert!(stderr.contains("settings.local.json"), "{stderr}");
    assert!(
        !world.repo.join(".claude/settings.json").exists(),
        "refused: settings.json must not have been written"
    );
}

// The same measured merge, from doctor's side: two settings files each
// naming a relais command is a finding of its own, not a silent pass on
// whichever one doctor happens to read first.
#[test]
fn doctor_fails_hook_live_when_two_settings_files_record_a_hook() {
    let world = World::new("doctor-hook-dup");
    std::fs::create_dir_all(world.repo.join(".claude")).expect("mkdir");
    let entry = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {"matcher": "Agent|Task", "hooks": [
                    {"type": "command", "command": "/opt/relais/bin/relais hook"}
                ]}
            ]
        }
    })
    .to_string();
    std::fs::write(world.repo.join(".claude/settings.json"), &entry).expect("write settings.json");
    std::fs::write(world.repo.join(".claude/settings.local.json"), &entry)
        .expect("write settings.local.json");

    let doctor = world.relais(&["doctor", "--json"]);
    let report: serde_json::Value =
        serde_json::from_str(text(&doctor.stdout).trim()).expect("doctor --json is a document");
    let finding = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["component"] == "hook-live")
        .expect("a hook-live finding");
    assert_eq!(finding["level"], "fail", "{finding}");
    let detail = finding["detail"].as_str().expect("detail");
    assert!(detail.contains("settings.json"), "{finding}");
    assert!(detail.contains("settings.local.json"), "{finding}");
}

// SPEC §3: doctor names what is missing rather than dying on it, and the
// process exits on a blocker. C2: the PATH lookup that finds `git` has to
// work on the platform the build ships for — this scenario is the one
// that would have caught the extensionless-only lookup on Windows.
#[test]
fn doctor_names_what_is_missing_and_exits_on_a_blocker() {
    let world = World::new("doctor");
    let first = world.relais(&["doctor", "--json"]);
    let report: serde_json::Value =
        serde_json::from_str(text(&first.stdout).trim()).expect("doctor --json is a document");
    let finding = |component: &str| -> serde_json::Value {
        report["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .find(|f| f["component"] == component)
            .unwrap_or_else(|| panic!("no `{component}` finding in {report}"))
            .clone()
    };
    assert_eq!(
        finding("git")["level"],
        "ok",
        "git is found on this platform: {report}"
    );
    assert_eq!(
        finding("claude-code")["level"],
        "fail",
        "the named binary does not exist: {report}"
    );
    assert_eq!(
        finding("relais.toml")["level"],
        "fail",
        "there is no policy here yet: {report}"
    );
    assert_eq!(
        first.status.code(),
        Some(3),
        "a blocker exits 3: {}",
        text(&first.stderr)
    );
    // With a policy in place, that finding turns; the harness one does
    // not, because nothing about it changed.
    world.write_policy();
    let second = world.relais(&["doctor", "--json"]);
    let report: serde_json::Value =
        serde_json::from_str(text(&second.stdout).trim()).expect("still a document");
    let policy_level = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["component"] == "relais.toml")
        .map(|f| f["level"].clone())
        .expect("relais.toml finding");
    assert_eq!(policy_level, "ok", "{report}");
}

#[test]
fn doctor_probe_hooks_is_a_usage_error() {
    let world = World::new("doctor-probe-hooks");
    let output = world.relais(&["doctor", "--probe-hooks"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr));
    assert!(
        text(&output.stderr).contains("--probe-hooks"),
        "{}",
        text(&output.stderr)
    );
}

#[test]
fn hook_probe_record_writes_the_payload_verbatim_and_stays_silent() {
    let world = World::new("hook-probe");
    let dir = world.root.join("recordings");
    std::fs::create_dir_all(&dir).expect("recordings dir");
    let payload = br#"{"hook_event_name":"PreToolUse","tool_name":"Agent"}"#;
    let output = common::relais(BIN)
        .args(["hook", "--probe", "--record", &dir.to_string_lossy()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin piped")
                .write_all(payload)?;
            child.wait_with_output()
        })
        .expect("hook --probe --record runs");
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(
        output.stdout.is_empty(),
        "the handler must never write to stdout: {}",
        text(&output.stdout)
    );
    let written = std::fs::read_dir(&dir)
        .expect("recordings dir readable")
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    assert_eq!(written.len(), 1, "exactly one payload was recorded");
    let recorded = std::fs::read(written[0].path()).expect("payload readable");
    assert_eq!(recorded, payload, "the payload is recorded byte for byte");
}

// Task-linking: `--revise` names an existing task by id; one that
// matches no run on record is refused by name, before route or dispatch.
#[test]
fn plan_revise_of_an_unknown_task_is_refused_by_name() {
    let world = World::new("revise-unknown");
    world.write_policy();
    world.write_machine_without_a_grant();
    let task = world.write_task("task.json");
    let plan = world.relais(&[
        "plan",
        "--task",
        task.to_str().unwrap(),
        "--revise",
        "task-does-not-exist0",
    ]);
    assert_ne!(plan.status.code(), Some(0));
    assert!(
        text(&plan.stderr).contains("task-does-not-exist0"),
        "{}",
        text(&plan.stderr)
    );
}

// SPEC §5: execution needs a trust grant bound to this repository AND
// this declaration. Without one, `plan` says so and prints the block to
// paste into machine.toml; nothing is launched and nothing is written.
#[test]
fn plan_without_a_trust_grant_is_blocked_and_prints_the_grant() {
    let world = World::new("trust");
    let hash = world.write_policy();
    world.write_machine_without_a_grant();
    let task = world.write_task("task.json");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    let stdout = text(&plan.stdout);
    assert_eq!(
        plan.status.code(),
        Some(3),
        "policy refuses and no model ran: {stdout}\n{}",
        text(&plan.stderr)
    );
    assert!(
        stdout.contains(&format!("policy hash: {hash}")),
        "the declaration being granted is named: {stdout}"
    );
    assert!(
        stdout.contains("trust grant: MISSING"),
        "the plan names trust as what is missing: {stdout}"
    );
    // The block is printed ready to paste, with the reviewer and the
    // repository label in it: a grant nobody reviewed is not a grant.
    assert!(stdout.contains("[trust.\""), "{stdout}");
    assert!(stdout.contains("reviewed_by = \"<your name>\""), "{stdout}");
    assert!(stdout.contains("granted_at = "), "{stdout}");
    // The plan says which identity the grant is keyed on. This world's
    // repository has no origin, so it is the common git directory — the
    // REPOSITORY, not this checkout — and a worktree of it plans the
    // same key, which is the whole point of a repository-bound grant.
    let common_dir =
        std::fs::canonicalize(world.repo.join(".git")).expect("the repository's git dir");
    let identity_line = format!("repository: {} (no origin)", common_dir.display());
    assert!(stdout.contains(&identity_line), "{identity_line}\n{stdout}");
    let key_line = |stdout: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with("[trust.\""))
            .map(str::to_string)
            .unwrap_or_else(|| panic!("no grant block in {stdout}"))
    };
    let worktree = world.root.join("wt");
    git(
        &world.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &worktree.to_string_lossy(),
            "HEAD",
        ],
    );
    let from_worktree = world.relais_in(&worktree, &["plan", "--task", task.to_str().unwrap()]);
    let worktree_stdout = text(&from_worktree.stdout);
    assert!(
        worktree_stdout.contains(&identity_line),
        "a worktree names the same repository: {worktree_stdout}"
    );
    assert_eq!(
        key_line(&worktree_stdout),
        key_line(&stdout),
        "one repository, one grant key, from every worktree"
    );
    // The blocker's code appears exactly once: `explain` lists it, and
    // `plan` used to repeat the whole list on stderr as well.
    assert_eq!(
        stdout.matches("missing_trust_grant").count(),
        1,
        "the refusal is named once, with its code: {stdout}"
    );
    assert!(
        !text(&plan.stderr).contains("missing_trust_grant"),
        "and not a second time on stderr: {}",
        text(&plan.stderr)
    );
    // Nothing was executed and no run exists.
    assert!(
        !world.state.join("runs").exists()
            || std::fs::read_dir(world.state.join("runs"))
                .expect("runs")
                .next()
                .is_none(),
        "plan launches nothing"
    );
}

#[test]
fn a_fresh_machine_is_told_the_grant_is_missing_and_trust_grant_unblocks_plan() {
    let world = World::new("trust-grant");
    world.write_policy();
    // No machine.toml at all: the state of a machine that never ran
    // relais. Absent is empty settings, so `plan` reaches the trust check
    // and says what is missing instead of failing to read the file.
    let machine = world.config.join("machine.toml");
    assert!(!machine.exists());
    let task = world.write_task("task.json");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    let stdout = text(&plan.stdout);
    assert_eq!(
        plan.status.code(),
        Some(3),
        "{stdout}\n{}",
        text(&plan.stderr)
    );
    assert!(stdout.contains("trust grant: MISSING"), "{stdout}");
    assert!(stdout.contains("relais trust grant --key "), "{stdout}");

    let show = world.relais(&["trust", "show", "--json"]);
    assert_eq!(show.status.code(), Some(0), "{}", text(&show.stderr));
    let shown: serde_json::Value = serde_json::from_slice(&show.stdout).expect("show json");
    let key = shown["grant_key"].as_str().expect("grant_key").to_string();
    assert_eq!(shown["granted"], serde_json::json!(false));

    // A key that is not this policy's is refused by name, and writes
    // nothing.
    let stale = world.relais(&[
        "trust",
        "grant",
        "--key",
        "0000",
        "--reviewed-by",
        "the suite",
    ]);
    assert_eq!(stale.status.code(), Some(18), "{}", text(&stale.stderr));
    assert!(text(&stale.stderr).contains("stale_grant_key"));
    assert!(
        text(&stale.stderr).contains(&key),
        "the current key is named"
    );
    assert!(!machine.exists(), "a refused grant writes nothing");

    let grant = world.relais(&[
        "trust",
        "grant",
        "--key",
        &key,
        "--reviewed-by",
        "the suite",
    ]);
    assert_eq!(grant.status.code(), Some(0), "{}", text(&grant.stderr));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&machine)
            .expect("machine.toml")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "machine.toml is private");
    }
    let again = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    let again_stdout = text(&again.stdout);
    assert!(
        again_stdout.contains(&format!("trust grant: {key} (in machine.toml)")),
        "{again_stdout}\n{}",
        text(&again.stderr)
    );
    assert!(
        !again_stdout.contains("missing_trust_grant"),
        "{again_stdout}"
    );
}

// `plan`'s stdout is a stable contract other tooling parses (the
// preceding test asserts `missing_trust_grant` appears exactly once
// there). Session attribution is not part of that contract and belongs
// on stderr, alongside the fallback warning.
#[test]
fn plan_prints_the_session_line_to_stderr_not_stdout() {
    let world = World::new("session-stderr");
    world.write_policy();
    world.write_machine_without_a_grant();
    let task = world.write_task("task.json");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    let stdout = text(&plan.stdout);
    let stderr = text(&plan.stderr);
    assert!(
        !stdout.contains("session:"),
        "session attribution must not be on stdout: {stdout}"
    );
    assert!(
        stderr.contains("session: tab-test"),
        "and must be on stderr: {stderr}"
    );
}

// C3: `coordinator status --json` is a document in every state,
// including the one where nothing answers — never an English sentence a
// parser would choke on, and never a silent zero.
#[test]
fn coordinator_status_is_a_document_when_nothing_answers() {
    let world = World::new("coordstatus");
    let human = world.relais(&["coordinator", "status"]);
    assert!(
        text(&human.stdout).contains("no coordinator"),
        "{}",
        text(&human.stdout)
    );
    let json = world.relais(&["coordinator", "status", "--json"]);
    assert_eq!(json.status.code(), Some(0), "{}", text(&json.stderr));
    let document: serde_json::Value =
        serde_json::from_str(text(&json.stdout).trim()).expect("stdout is JSON in every state");
    assert_eq!(document["coordinator"], "absent", "{document}");
    assert!(
        document["cause"].as_str().is_some_and(|c| !c.is_empty()),
        "the reason nothing answered is carried: {document}"
    );
}

// C10: the exit-code table is the CLI's contract with its callers, and
// the codes README.md publishes are the ones the binary really uses.
#[test]
fn exit_codes_follow_the_documented_table() {
    let world = World::new("codes");
    world.write_policy();

    // 2 — the invocation, or a file it named, is invalid.
    let missing = world.relais(&["plan", "--task", "no-such-task.json"]);
    assert_eq!(
        missing.status.code(),
        Some(2),
        "a contract file that is not there: {}",
        text(&missing.stderr)
    );
    let bad = world.root.join("bad.json");
    std::fs::write(&bad, r#"{"schema_version":1,"kind":"change","nope":1}"#).expect("write");
    assert_eq!(
        world
            .relais(&["plan", "--task", bad.to_str().unwrap()])
            .status
            .code(),
        Some(2),
        "an unknown contract field is rejected, not ignored"
    );
    assert_eq!(
        world.relais(&["coordinator", "cancel"]).status.code(),
        Some(2),
        "naming none of --run/--dispatch/--session is a usage error"
    );

    // 10 — the run or artifact named is not on record. Distinct from 2:
    // the invocation was well formed.
    assert_eq!(
        world
            .relais(&["status", "run-that-never-was"])
            .status
            .code(),
        Some(10)
    );
    assert_eq!(
        world
            .relais(&["resume", "run-that-never-was"])
            .status
            .code(),
        Some(10)
    );

    // 14 — nothing to train on yet, which is not a failure of relais.
    assert_eq!(world.relais(&["train"]).status.code(), Some(14));

    // 0 — and the ordinary path still exits 0.
    assert_eq!(world.relais(&["doctor", "--json"]).status.code(), Some(3));
    assert_eq!(world.relais(&["report", "--json"]).status.code(), Some(0));
}

/// The case a trailing `remove_dir_all` always missed: the test body
/// never reaches its last line. `World`'s `_scratch` field is a `Drop`
/// guard, so a panic mid-body still loses the directory.
#[test]
fn a_panicking_test_still_loses_its_world() {
    let world = World::new("panic-cleanup");
    let root = world.root.clone();
    assert!(root.exists());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _world = world;
        panic!("the test body never gets here on purpose");
    }));
    assert!(outcome.is_err());
    assert!(!root.exists());
}

/// The assertion that would have caught the present hole: this suite's
/// own `World` builds a directory under the same `SCRATCH_PREFIX`
/// `doctor::count_strays` reads, not a hand-rolled `rl-pt-` prefix the
/// scan never knew about.
#[test]
fn a_world_is_countable_by_doctors_scan() {
    let world = World::new("scan-proof");
    let parent = world.root.parent().expect("world's parent").to_path_buf();
    let (count, _) = relais::doctor::count_strays(&parent).expect("scan");
    assert!(
        count >= 1,
        "the world's own root must be counted among {parent:?}'s strays"
    );
}

/// `relais dataset replay` writes NOTHING when it cannot proceed.
///
/// Scoped to what this can actually establish, and named for it. The
/// first version of this test claimed to cover `--dry-run` itself and did
/// not: with a task id that has no accepted run, the command returns at
/// that refusal BEFORE reaching the dry-run branch, so injecting a write
/// into that branch left the test green (verified). A test whose name
/// promises more than it reaches is worse than no test, because the gap
/// then looks covered.
///
/// What it does prove is still worth having: the refusal paths — an
/// unknown task, no accepted run — touch the ledger not at all, byte for
/// byte. Compared as FILE BYTES rather than row counts, since a row
/// written and rolled back, or a table no reader happens to query, passes
/// a count and is still a write.
///
/// The dry-run branch proper needs a world holding an accepted run with a
/// receipt, which no portable scenario can build without dispatching a
/// worker. It is covered by the unit tests around `replay_command`'s
/// pieces instead, and the branch itself returns before `load_machine`,
/// before a trial id is minted, and before `execute` — the writes are all
/// downstream of the return.
#[test]
fn a_replay_that_cannot_proceed_leaves_the_ledger_byte_identical() {
    let world = World::new("replay-dry-run");

    // Give the world a ledger to be unchanged: any command that opens it
    // creates and migrates it, and a migration is a legitimate write.
    // The comparison has to start after that.
    world.relais(&["report", "--json"]);
    let ledger = world.state.join("ledger.sqlite");
    assert!(
        ledger.is_file(),
        "the world needs a ledger before this can mean anything"
    );
    let before = std::fs::read(&ledger).expect("read the ledger");

    // A candidate recipe file that parses. The task id need not exist:
    // whichever refusal the dry run reaches, it must reach it without
    // writing — an unknown task, a candidate no recipe covers and a
    // clean dry run are all cases where nothing should be recorded.
    let candidate = world.root.join("candidate.toml");
    std::fs::write(
        &candidate,
        std::fs::read(world.repo.join("relais.toml")).unwrap_or_default(),
    )
    .expect("write the candidate");

    let out = world.relais(&[
        "dataset",
        "replay",
        "--task",
        "task-0000000000000000",
        "--recipe",
        candidate.to_string_lossy().as_ref(),
        "--dry-run",
    ]);

    let after = std::fs::read(&ledger).expect("read the ledger again");
    assert_eq!(
        before,
        after,
        "a replay that cannot proceed must leave the ledger byte-identical; stdout was \
         {:?}, stderr {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// SPEC §26: `relais recipe list`/`show`/`diff` are read-only, and a
// policy that declares no recipes says so in words.

#[test]
fn recipe_list_says_so_in_words_when_there_are_none() {
    let world = World::new("recipe-list-empty");
    world.write_policy();
    let out = world.relais(&["recipe", "list"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains("declares no recipes"),
        "empty output would be indistinguishable from a failure to read: {stdout:?}"
    );
}

#[test]
fn recipe_list_reports_every_recipe_in_declaration_order() {
    let world = World::new("recipe-list");
    world.write_policy_with_recipes();
    let out = world.relais(&["recipe", "list"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = text(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "one line per recipe: {stdout:?}");
    for (line, revision, enabled) in [
        (lines[0], "revision=0", "enabled=true"),
        (lines[1], "revision=1", "enabled=false"),
    ] {
        assert!(line.contains("docs-touchup"), "{line}");
        assert!(line.contains(revision), "{line}");
        assert!(line.contains(enabled), "{line}");
        assert!(line.contains("tier=implementation"), "{line}");
        assert!(line.contains("kind=unset"), "{line}");
        assert!(line.contains("scope=docs/**"), "{line}");
        assert!(line.contains("recipe_id="), "{line}");
    }
}

#[test]
fn recipe_show_prints_every_revision_and_refuses_an_unknown_name() {
    let world = World::new("recipe-show");
    world.write_policy_with_recipes();

    let ok = world.relais(&["recipe", "show", "docs-touchup"]);
    assert_eq!(ok.status.code(), Some(0));
    let stdout = text(&ok.stdout);
    assert!(stdout.contains("revision 0"), "{stdout}");
    assert!(stdout.contains("revision 1"), "{stdout}");
    assert!(
        stdout.contains("revision 0") && stdout.find("revision 0") < stdout.find("revision 1"),
        "revisions print oldest first: {stdout}"
    );

    let unknown = world.relais(&["recipe", "show", "no-such-recipe"]);
    assert_eq!(
        unknown.status.code(),
        Some(2),
        "an unknown name is a refusal, never exit 0: {:?}",
        text(&unknown.stderr)
    );
    let stderr = text(&unknown.stderr);
    assert!(stderr.contains("no-such-recipe"), "{stderr}");
    assert!(
        stderr.contains("docs-touchup"),
        "the refusal names the recipes that do exist: {stderr}"
    );
}

#[test]
fn recipe_show_flags_declared_but_unread_blocks() {
    let world = World::new("recipe-show-blocks");
    let policy = r#"schema_version = 1

[models.implementation]
id = "sonnet"

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

[[recipes]]
name = "tuned"
scope_within = ["docs/**"]
tier = "implementation"
review = "required"

[recipes.models.implementation]
id = "sonnet"
"#;
    std::fs::write(world.repo.join("relais.toml"), policy).expect("policy");
    git(&world.repo, &["add", "-A"]);
    git(&world.repo, &["commit", "-q", "-m", "recipe with models"]);

    let out = world.relais(&["recipe", "show", "tuned"]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = text(&out.stdout);
    assert!(stdout.contains("models:"), "{stdout}");
    assert!(stdout.contains("review:"), "{stdout}");
    let line_of = |label: &str| {
        stdout
            .lines()
            .find(|line| line.trim_start().starts_with(label))
            .map(str::to_string)
    };
    // `review` is still declared and hashed only; `models` is read by
    // routing and no longer carries the caveat.
    assert!(
        line_of("review:").is_some_and(|line| line.contains("DECLARED AND HASHED, NOT YET READ")),
        "review: must carry the same caveat its field doc does: {stdout}"
    );
    assert!(
        line_of("models:").is_some_and(|line| !line.contains("NOT YET READ")),
        "models: is read by routing and carries no caveat: {stdout}"
    );
}

#[test]
fn recipe_diff_reports_field_changes_and_the_admission_verdict() {
    let world = World::new("recipe-diff");
    let base = world.write_policy_with_recipes();

    // Rewrites revision 1's history in place: `enabled` flips true and
    // `tier` rises to escalation. History being append-only, this is a
    // real, field-level change that `validate_candidate` must refuse.
    let rewritten = base.replace(
        "tier = \"implementation\"\nrevision = 1\nenabled = false",
        "tier = \"escalation\"\nrevision = 1",
    );
    assert_ne!(rewritten, base, "the replacement must actually match");
    let rewritten_candidate = world.root.join("rewritten.toml");
    std::fs::write(&rewritten_candidate, &rewritten).expect("write candidate");
    RepoPolicy::from_toml_str(&rewritten).expect("the candidate itself parses");

    let diff = world.relais(&[
        "recipe",
        "diff",
        rewritten_candidate.to_string_lossy().as_ref(),
    ]);
    assert_eq!(diff.status.code(), Some(0), "{:?}", text(&diff.stderr));
    let stdout = text(&diff.stdout);
    assert!(stdout.contains("docs-touchup revision 1:"), "{stdout}");
    assert!(stdout.contains("enabled"), "{stdout}");
    assert!(stdout.contains("tier"), "{stdout}");
    assert!(
        stdout.contains("not admissible"),
        "a rewritten history entry must not be admissible: {stdout}"
    );
    assert!(
        stdout.contains("admissible is not approved"),
        "the caveat must be stated whichever way the verdict falls: {stdout}"
    );

    // A candidate that only APPENDS a validly tuned new revision is
    // admissible.
    let appended = format!(
        "{base}\n[[recipes]]\nname = \"docs-touchup\"\nscope_within = [\"docs/**\"]\ntier = \"implementation\"\nrevision = 2\n"
    );
    let appended_candidate = world.root.join("appended.toml");
    std::fs::write(&appended_candidate, &appended).expect("write candidate");
    RepoPolicy::from_toml_str(&appended).expect("the candidate itself parses");

    let diff2 = world.relais(&[
        "recipe",
        "diff",
        appended_candidate.to_string_lossy().as_ref(),
    ]);
    assert_eq!(diff2.status.code(), Some(0), "{:?}", text(&diff2.stderr));
    let stdout2 = text(&diff2.stdout);
    assert!(stdout2.contains("only in the candidate"), "{stdout2}");
    assert!(
        stdout2.contains("admissible: route::validate_candidate would ADMIT"),
        "{stdout2}"
    );
}

/// ALL THREE recipe subcommands are read-only. Falsified separately for
/// each path so an early failure can never pass the byte-identical ledger
/// check by accident:
/// - `recipe_list_command`: a temporary probe appending bytes to the
///   ledger file was added, and this test turned red with exactly the
///   ledger-bytes assertion failing, before the probe was removed.
/// - `recipe_show_command`: same probe, injected into the loop over
///   revisions, same red result, before it was removed.
/// - `recipe_diff_command`: same probe, injected right before the
///   admission verdict is printed, same red result, before it was
///   removed.
///
/// Each subcommand's own exit code and stdout are asserted too, so a
/// silent early failure inside one of them cannot slip past the ledger
/// comparison by short-circuiting before the probe would have fired.
#[test]
fn recipe_subcommands_never_touch_the_ledger() {
    let world = World::new("recipe-read-only");
    world.write_policy_with_recipes();

    // Give the world a ledger to be unchanged: any command that opens it
    // creates and migrates it, and a migration is a legitimate write.
    // The comparison has to start after that.
    world.relais(&["report", "--json"]);
    let ledger = world.state.join("ledger.sqlite");
    assert!(
        ledger.is_file(),
        "the world needs a ledger before this can mean anything"
    );
    let before = std::fs::read(&ledger).expect("read the ledger");

    let candidate = world.root.join("candidate.toml");
    std::fs::write(
        &candidate,
        std::fs::read(world.repo.join("relais.toml")).unwrap_or_default(),
    )
    .expect("write the candidate");

    let list = world.relais(&["recipe", "list"]);
    assert_eq!(list.status.code(), Some(0), "{:?}", text(&list.stderr));
    assert!(
        text(&list.stdout).contains("docs-touchup"),
        "{:?}",
        text(&list.stdout)
    );

    let show = world.relais(&["recipe", "show", "docs-touchup"]);
    assert_eq!(show.status.code(), Some(0), "{:?}", text(&show.stderr));
    assert!(
        text(&show.stdout).contains("revision 0"),
        "{:?}",
        text(&show.stdout)
    );

    let show_unknown = world.relais(&["recipe", "show", "no-such-recipe"]);
    assert_eq!(
        show_unknown.status.code(),
        Some(2),
        "{:?}",
        text(&show_unknown.stderr)
    );

    let diff = world.relais(&["recipe", "diff", candidate.to_string_lossy().as_ref()]);
    assert_eq!(diff.status.code(), Some(0), "{:?}", text(&diff.stderr));
    assert!(
        text(&diff.stdout).contains("no differences"),
        "{:?}",
        text(&diff.stdout)
    );

    let after = std::fs::read(&ledger).expect("read the ledger again");
    assert_eq!(
        before, after,
        "recipe list/show/diff must leave the ledger byte-identical"
    );
}

// SPEC §27: `recipe promote` and `recipe rollback`. The ledger is filled
// through the library's own `insert_replay_trial`/`settle_trial`, so the
// comparison the binary recomputes is built from settled trials, never a
// hand-made report.

/// A candidate that only APPENDS revision 2 of `docs-touchup` to `base`,
/// and the `recipe_id` trials must name to count as its arm.
fn write_appending_candidate(world: &World, base: &str) -> (PathBuf, String) {
    let candidate = format!(
        "{base}\n[[recipes]]\nname = \"docs-touchup\"\nscope_within = [\"docs/**\"]\ntier = \"implementation\"\nrevision = 2\n"
    );
    let path = world.root.join("promote-candidate.toml");
    std::fs::write(&path, &candidate).expect("write candidate");
    let parsed = RepoPolicy::from_toml_str(&candidate).expect("the candidate parses");
    let arm_id = parsed.recipes.last().expect("a new revision").recipe_id();
    (path, arm_id)
}

/// `count` settled, accepted replay trials for `arm_recipe_id`, one per
/// task.
fn settle_accepted_replays(world: &World, arm_recipe_id: &str, count: usize) {
    use relais::ids::{RunId, TaskId, TrialId};
    use relais::ledger::{Ledger, NewReplayTrial, TrialCost, TrialOutcome};
    let ledger = Ledger::open(&world.state.join("ledger.sqlite")).expect("ledger opens");
    for n in 0..count {
        let trial_id = TrialId::from_stored(format!("trial-{n}"));
        let task_id = TaskId::from_stored(format!("task-{n}"));
        let run_id = RunId::from_stored(format!("run-{n}"));
        ledger
            .insert_replay_trial(&NewReplayTrial {
                trial_id: &trial_id,
                task_id: &task_id,
                source_run_id: &run_id,
                incumbent_recipe_id: "incumbent",
                arm_recipe_id,
                base_sha: "base",
                contract_hash: "contract",
                verification_profile_hash: "profile",
                workspace_isolation: "fresh_checkout_no_accepted_answer",
                arm_run_id: &run_id,
            })
            .expect("insert trial");
        ledger
            .settle_trial(
                &trial_id,
                TrialOutcome::Accepted,
                true,
                TrialCost::UNKNOWN,
                1,
            )
            .expect("settle trial");
    }
}

fn policy_text(world: &World) -> String {
    std::fs::read_to_string(world.repo.join("relais.toml")).expect("read policy")
}

/// (a) An n=3 replay comparison — the only real one measured on the
/// machine this was written on — makes `promote` refuse with its own exit
/// code, name the below-20 gate, and leave relais.toml byte-identical,
/// with and without `--write`.
///
/// FALSIFY: `ComparisonReport::promotable` was made to mint a `Promotable`
/// whatever `failures` said (the moral equivalent of `promote` skipping
/// the check); this test failed on the exit code and on the file being
/// written to, then the check was restored.
#[test]
fn promote_refuses_a_three_task_comparison_and_writes_nothing() {
    let world = World::new("promote-refuse");
    let base = world.write_policy_with_recipes();
    let (candidate, arm_id) = write_appending_candidate(&world, &base);
    settle_accepted_replays(&world, &arm_id, 3);
    let before = policy_text(&world);

    for extra in [&[][..], &["--write"][..]] {
        let mut args = vec!["recipe", "promote", candidate.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = world.relais(&args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(17), "{stderr}");
        assert!(stderr.contains("basis: replay, n=3"), "{stderr}");
        assert!(
            stderr.contains("3 paired task(s) is below the 20 required"),
            "{stderr}"
        );
        assert_eq!(
            policy_text(&world),
            before,
            "a refused promotion leaves relais.toml untouched"
        );
    }
}

/// `per_arm` accepted control rows and `per_arm` accepted candidate rows,
/// every one a live draw at p = 0.5 between the incumbent and the arm.
fn settle_accepted_live_draws(world: &World, arm_recipe_id: &str, per_arm: usize) {
    use relais::ids::{RunId, TaskId, TrialId};
    use relais::ledger::{Ledger, NewTrial, TrialCost, TrialOutcome, LIVE_WORKTREE};
    let ledger = Ledger::open(&world.state.join("ledger.sqlite")).expect("ledger opens");
    let arms_json = serde_json::to_string(&["incumbent", arm_recipe_id]).expect("arms");
    for n in 0..per_arm {
        for (side, arm_index, recipe) in [("c", 0, "incumbent"), ("a", 1, arm_recipe_id)] {
            let trial_id = TrialId::from_stored(format!("live-{side}{n}"));
            let task_id = TaskId::from_stored(format!("task-{side}{n}"));
            let run_id = RunId::from_stored(format!("run-{side}{n}"));
            ledger
                .insert_run(&run_id, "/repo", None, &task_id, "rk")
                .expect("live run");
            ledger
                .insert_trial(&NewTrial {
                    trial_id: &trial_id,
                    task_id: &task_id,
                    source_run_id: &run_id,
                    incumbent_recipe_id: "incumbent",
                    arm_recipe_id: recipe,
                    arm_index,
                    assignment_probability: 0.5,
                    seed: 7,
                    base_sha: "base",
                    contract_hash: "contract",
                    verification_profile_hash: "profile",
                    workspace_isolation: LIVE_WORKTREE,
                    arms_json: Some(&arms_json),
                    arm_run_id: &run_id,
                })
                .expect("insert live trial");
            ledger
                .settle_trial(
                    &trial_id,
                    TrialOutcome::Accepted,
                    true,
                    TrialCost::UNKNOWN,
                    1,
                )
                .expect("settle live trial");
        }
    }
}

/// 19 randomized observations per arm is one short of the gate: `promote`
/// refuses naming it, and relais.toml stays byte-identical, with and
/// without `--write`.
#[test]
fn promote_refuses_nineteen_live_draws_per_arm_and_writes_nothing() {
    let world = World::new("promote-live-refuse");
    let base = world.write_policy_with_recipes();
    let (candidate, arm_id) = write_appending_candidate(&world, &base);
    settle_accepted_live_draws(&world, &arm_id, 19);
    let before = policy_text(&world);

    for extra in [&[][..], &["--write"][..]] {
        let mut args = vec!["recipe", "promote", candidate.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = world.relais(&args);
        let stderr = text(&out.stderr);
        assert_eq!(out.status.code(), Some(17), "{stderr}");
        assert!(
            stderr.contains("basis: randomized, control n=19, candidate n=19"),
            "{stderr}"
        );
        assert!(
            stderr.contains("randomized: the control arm has 19 observation(s)"),
            "{stderr}"
        );
        assert_eq!(policy_text(&world), before);
    }
}

/// (b) A promotable comparison, with `--write`, appends exactly the new
/// fragment and issues no grant: `plan` then blocks on
/// `missing_trust_grant`, on the very key `promote` printed.
#[test]
fn promote_appends_the_fragment_and_plan_then_wants_a_grant() {
    let world = World::new("promote-write");
    let base = world.write_policy_with_recipes();
    let (candidate, arm_id) = write_appending_candidate(&world, &base);
    settle_accepted_replays(&world, &arm_id, 20);
    world.write_machine_without_a_grant();
    let before = policy_text(&world);

    let shown = world.relais(&["recipe", "promote", candidate.to_str().unwrap()]);
    assert_eq!(shown.status.code(), Some(0), "{}", text(&shown.stderr));
    assert_eq!(
        policy_text(&world),
        before,
        "without --write nothing is written"
    );
    let shown_stdout = text(&shown.stdout);
    assert!(
        shown_stdout.contains("basis: replay, n=20"),
        "{shown_stdout}"
    );
    assert!(shown_stdout.contains("[[recipes]]"), "{shown_stdout}");

    let written = world.relais(&["recipe", "promote", candidate.to_str().unwrap(), "--write"]);
    assert_eq!(written.status.code(), Some(0), "{}", text(&written.stderr));
    let after = policy_text(&world);
    assert!(
        after.starts_with(&before),
        "every existing byte is kept, as a prefix"
    );
    let appended = &after[before.len()..];
    assert_eq!(
        appended.matches("[[recipes]]").count(),
        1,
        "exactly the one new revision: {appended}"
    );
    assert!(appended.contains("revision = 2"), "{appended}");
    let policy = RepoPolicy::from_toml_str(&after).expect("the result parses");
    assert_eq!(policy.recipes.len(), 3);
    assert_eq!(policy.recipes[2].recipe_id(), arm_id);
    assert_eq!(
        std::fs::read_to_string(world.config.join("machine.toml")).expect("machine"),
        "schema_version = 1\n",
        "machine.toml is never touched"
    );

    git(&world.repo, &["add", "-A"]);
    git(&world.repo, &["commit", "-q", "-m", "promote"]);
    let task = world.write_task("task.json");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    let plan_stdout = text(&plan.stdout);
    assert!(plan_stdout.contains("missing_trust_grant"), "{plan_stdout}");
    let key_line = |stdout: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with("[trust.\""))
            .map(str::to_string)
            .unwrap_or_else(|| panic!("no grant block in {stdout}"))
    };
    assert_eq!(
        key_line(&text(&written.stdout)),
        key_line(&plan_stdout),
        "promote prints the block the next plan asks for"
    );
}

/// (c) Rollback appends revision N+1 whose fields equal N-1's, and that
/// becomes the effective recipe; history is only ever appended to.
#[test]
fn rollback_appends_a_revision_equal_to_the_one_below_the_effective() {
    let world = World::new("rollback");
    let base = world.write_policy_with_recipes().replace(
        "tier = \"implementation\"\nrevision = 1\nenabled = false",
        "tier = \"escalation\"\nrevision = 1",
    );
    std::fs::write(world.repo.join("relais.toml"), &base).expect("policy");
    let before = policy_text(&world);

    let shown = world.relais(&["recipe", "rollback", "docs-touchup"]);
    assert_eq!(shown.status.code(), Some(0), "{}", text(&shown.stderr));
    assert_eq!(
        policy_text(&world),
        before,
        "without --write nothing is written"
    );
    assert!(text(&shown.stdout).contains("trust grant: MISSING"));

    let written = world.relais(&["recipe", "rollback", "docs-touchup", "--write"]);
    assert_eq!(written.status.code(), Some(0), "{}", text(&written.stderr));
    let after = policy_text(&world);
    assert!(after.starts_with(&before));
    let policy = RepoPolicy::from_toml_str(&after).expect("the result parses");
    assert_eq!(policy.recipes.len(), 3);
    let effective = relais::policy::select_highest_enabled_revision(&policy.recipes, |recipe| {
        recipe.name == "docs-touchup"
    })
    .expect("an effective recipe");
    let restored = relais::policy::RecipeSpec {
        revision: 2,
        ..policy.recipes[0].clone()
    };
    assert_eq!(effective, &restored, "revision 2 repeats revision 0");
    let history = RepoPolicy::from_toml_str(&before).expect("before parses");
    assert_eq!(policy.recipes[..2], history.recipes[..]);
}

/// (d) A recipe with a single revision, or no such recipe, is refused
/// naming what exists, and nothing is written.
#[test]
fn rollback_of_a_single_revision_recipe_is_refused() {
    let world = World::new("rollback-single");
    let base = world.write_policy_with_recipes();
    let cut = base.find("\n[[recipes]]").expect("first revision");
    let second = cut
        + base[cut + 1..]
            .find("\n[[recipes]]")
            .expect("second revision")
        + 1;
    let single = &base[..second];
    std::fs::write(world.repo.join("relais.toml"), single).expect("policy");
    RepoPolicy::from_toml_str(single).expect("one revision parses");

    let out = world.relais(&["recipe", "rollback", "docs-touchup", "--write"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("`docs-touchup` has 1 revision(s) (0)"),
        "{stderr}"
    );
    assert_eq!(policy_text(&world), single);

    let unknown = world.relais(&["recipe", "rollback", "nope"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        text(&unknown.stderr).contains("declared recipes: docs-touchup"),
        "{}",
        text(&unknown.stderr)
    );
}

/// The plugin's run tool hands the model's contract to `relais native
/// contract` on stdin: a contract is saved under `.relais/tasks/` (kept out
/// of `git status`), and prose is refused by its schema error.
#[test]
fn native_contract_saves_a_contract_and_refuses_prose() {
    use std::io::Write;
    let world = World::new("native-contract");
    world.write_policy();
    let pipe = |stdin: &str| {
        let mut child = common::relais(BIN)
            .args(["native", "contract"])
            .current_dir(&world.repo)
            .env("RELAIS_STATE_DIR", &world.state)
            .env("RELAIS_CONFIG_DIR", &world.config)
            .env("HOME", &world.root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("relais runs");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stdin.as_bytes())
            .expect("written");
        child.wait_with_output().expect("relais ends")
    };
    let contract = std::fs::read_to_string(world.write_task("task.json")).expect("task");
    let saved = pipe(&contract);
    assert_eq!(saved.status.code(), Some(0), "{}", text(&saved.stderr));
    let path = PathBuf::from(text(&saved.stdout).trim());
    // Both sides canonical: macOS reaches the temp dir through a symlink,
    // and Windows prints a short (8.3) name where canonicalize gives `\\?\`.
    let tasks = std::fs::canonicalize(world.repo.join(".relais").join("tasks")).expect("tasks");
    let saved_at = std::fs::canonicalize(&path).expect("the printed path exists");
    assert!(saved_at.starts_with(&tasks), "{}", path.display());
    assert_eq!(std::fs::read_to_string(&path).expect("saved"), contract);
    assert_eq!(
        git(&world.repo, &["status", "--porcelain"]).trim(),
        "",
        "nothing shows in git status"
    );

    let prose = pipe("fix the JSON escaping in amont list");
    assert_eq!(prose.status.code(), Some(2));
    assert!(
        text(&prose.stderr).contains("not a task contract"),
        "{}",
        text(&prose.stderr)
    );
}

/// The `done` lines a `relais run --protocol` wrote on stdout.
#[cfg(unix)]
fn done_lines(stdout: &[u8]) -> Vec<serde_json::Value> {
    text(stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|line| line["relais"] == "done")
        .collect()
}

/// A run refused before it exists still sends exactly one `done` line,
/// with `run: null` and the refusal by name, so the plugin can tell the
/// model what to do instead of a bare exit code (SPEC §29).
#[cfg(unix)]
#[test]
fn a_run_refused_before_it_starts_still_sends_one_done_line_naming_why() {
    let world = World::new("proto-unstarted");
    let task = world.write_task("task.json");
    let plugin_run = |world: &World| {
        common::relais(BIN)
            .args(["run", "--task", task.to_str().unwrap(), "--protocol"])
            .current_dir(&world.repo)
            .env("RELAIS_STATE_DIR", &world.state)
            .env("RELAIS_CONFIG_DIR", &world.config)
            .env("RELAIS_CLAUDE_BIN", &world.claude)
            .env("RELAIS_SESSION_ID", "tab-test")
            .env("RELAIS_HOST", "claude-code-mod")
            .env("HOME", &world.root)
            .output()
            .expect("relais runs")
    };
    // No relais.toml in this repository yet.
    let _ = std::fs::remove_file(world.repo.join("relais.toml"));
    let run = plugin_run(&world);
    let done = done_lines(&run.stdout);
    assert_eq!(
        done.len(),
        1,
        "{}\n{}",
        text(&run.stdout),
        text(&run.stderr)
    );
    assert!(done[0]["run"].is_null(), "{}", done[0]);
    assert_eq!(done[0]["outcome"], "blocked");
    assert_eq!(done[0]["code"], "no_policy", "{}", done[0]);
    assert!(
        done[0]["detail"]
            .as_str()
            .unwrap_or("")
            .contains("relais.toml"),
        "{}",
        done[0]
    );

    // A policy that does not parse is named too.
    std::fs::write(world.repo.join("relais.toml"), "schema_version = 99\n").expect("policy");
    let invalid = plugin_run(&world);
    let done = done_lines(&invalid.stdout);
    assert_eq!(done.len(), 1, "{}", text(&invalid.stderr));
    assert_eq!(done[0]["code"], "invalid_policy", "{}", done[0]);
}

// SPEC §29: `--protocol` needs the descriptors of a Unix process; where it
// cannot be honoured it is refused before anything runs, not ignored.
#[cfg(windows)]
#[test]
fn run_protocol_is_refused_on_windows_before_anything_runs() {
    let world = World::new("proto-win");
    let run = world.relais(&["run", "--task", "no-such-task.json", "--protocol"]);
    assert_eq!(run.status.code(), Some(2), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("not supported on Windows yet"),
        "{}",
        text(&run.stderr)
    );
    assert!(text(&run.stdout).is_empty(), "{}", text(&run.stdout));
}

/// A ledger a headless relais wrote — a `managed_run` dispatch, `ApiSpend`
/// usage and a `permission_denied` block — still reads: `status`,
/// `explain` and `report` work on it.
#[test]
fn a_ledger_holding_headless_era_rows_still_reads() {
    use relais::ids::{DispatchId, RunId, TaskId};
    use relais::ledger::{Ledger, Transition, UsageEvent};
    use relais::lifecycle::State;
    use relais::money::{CostCompleteness, CostKind, MicroUsd};
    use relais::route::RoutedBy;

    let world = World::new("headless-ledger");
    let ledger = Ledger::open(&world.state.join("ledger.sqlite")).expect("ledger opens");
    let run = RunId::from_stored("run-headless");
    ledger
        .insert_run(
            &run,
            &world.repo.to_string_lossy(),
            Some("tab-old"),
            &TaskId::from_stored("task-headless"),
            "repo-key",
        )
        .expect("run");
    ledger
        .record_dispatch_intent(
            &DispatchId::from_stored("disp-headless"),
            &run,
            None,
            &serde_json::json!({}),
            0,
            RoutedBy::ConservativeBaseline,
        )
        .expect("a managed_run dispatch");
    ledger
        .record_usage(&UsageEvent {
            event_id: "usage-headless".into(),
            run_id: run.clone(),
            attempt_id: None,
            parent_event_id: None,
            model: Some("sonnet".into()),
            input_tokens: Some(100),
            output_tokens: Some(10),
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost: Some(MicroUsd::from_micros(1_234)),
            cost_kind: CostKind::ApiSpend,
            completeness: CostCompleteness::Actual,
            inclusive: false,
            at: "2026-09-01T00:00:00+00:00".into(),
            phase: None,
            duration_ms: None,
            requested_model: None,
            requested_effort: None,
            harness: None,
        })
        .expect("api spend usage");
    ledger
        .record_transition(&Transition {
            run_id: run.clone(),
            attempt_id: None,
            from_state: Some(State::Prepared),
            to_state: State::Blocked,
            reason: "permission_denied".into(),
            detail: Some(serde_json::json!({ "tools": ["Edit"] })),
            at: "2026-09-01T00:00:01+00:00".into(),
        })
        .expect("a permission_denied block");
    drop(ledger);

    let status = world.relais(&["status"]);
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr));
    let explain = world.relais(&["explain", "run-headless"]);
    assert_eq!(explain.status.code(), Some(0), "{}", text(&explain.stderr));
    assert!(
        text(&explain.stdout).contains("permission_denied"),
        "{}",
        text(&explain.stdout)
    );
    let report = world.relais(&["report"]);
    assert_eq!(report.status.code(), Some(0), "{}", text(&report.stderr));
}
