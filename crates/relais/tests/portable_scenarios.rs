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

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use relais::policy::RepoPolicy;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

/// One isolated world: a git repository, a state directory and a config
/// directory, and nothing else. No fake harness: every scenario here is
/// one that must not launch one.
struct World {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Short paths under /tmp where there is one: the coordinator
        // socket lives in the state directory and a Unix socket path is
        // capped near 100 bytes. Windows has neither /tmp nor the cap.
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let root = base.join(format!(
            "rl-pt-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // Pre-cleaned: a directory left by a killed run of this suite
        // with the same pid would hand the scenario somebody's ledger.
        let _ = std::fs::remove_dir_all(&root);
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
            root,
            repo,
            state,
            config,
        }
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
        Command::new(BIN)
            .args(args)
            .current_dir(cwd)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", &self.config)
            .env("RELAIS_CLAUDE_BIN", self.root.join("no-such-claude"))
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
        // asked one to exist; stopping is harmless when none did.
        let _ = self.relais(&["coordinator", "stop"]);
        let _ = std::fs::remove_dir_all(&self.root);
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
}

// SPEC §14: installation and uninstall preserve unrelated configuration
// and modified owned files.
#[test]
fn install_is_preview_first_and_uninstall_keeps_foreign_and_modified_files() {
    let world = World::new("install");
    let preview = world.relais(&["install", "--claude"]);
    assert_eq!(preview.status.code(), Some(0));
    assert!(text(&preview.stdout).contains("preview only"));
    assert!(!world.repo.join(".claude/skills/relais/SKILL.md").exists());
    std::fs::create_dir_all(world.repo.join(".claude/agents")).expect("mkdir");
    std::fs::write(world.repo.join(".claude/agents/custom.md"), "# mine\n").expect("foreign");
    let write = world.relais(&["install", "--claude", "--write"]);
    assert_eq!(write.status.code(), Some(0), "{}", text(&write.stderr));
    let skill = world.repo.join(".claude/skills/relais/SKILL.md");
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
fn install_claude_alone_never_touches_settings_json() {
    let world = World::new("install-no-hooks");
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
fn install_hooks_wires_settings_json_and_a_rerun_is_current() {
    let world = World::new("install-hooks");
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

// `relais doctor` exercises the hook it finds recorded in settings.json
// by spawning it for real: a fixture spawn payload on stdin, its
// directories redirected to a scratch environment that refuses when the
// coordinator is unreachable (there is none reachable from a scratch
// state directory). The recorded command IS this test binary, so this
// proves the wiring end to end, not just the planning.
#[test]
fn doctor_exercises_the_recorded_hook_and_reports_a_refusal() {
    let world = World::new("doctor-hook-live");
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
    assert!(
        detail.contains(
            &world
                .repo
                .join(".claude/settings.json")
                .display()
                .to_string()
        ),
        "{finding}"
    );
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
fn doctor_reports_no_hook_compat_record_until_one_is_written() {
    let world = World::new("doctor-hook-compat");
    let first = world.relais(&["doctor", "--json"]);
    let report: serde_json::Value =
        serde_json::from_str(text(&first.stdout).trim()).expect("doctor --json is a document");
    let finding = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["component"] == "hook-compat")
        .unwrap_or_else(|| panic!("no `hook-compat` finding in {report}"))
        .clone();
    assert_eq!(finding["level"], "warn", "{report}");
    assert!(
        finding["detail"]
            .as_str()
            .unwrap()
            .contains("no hook compatibility record"),
        "{finding}"
    );
}

#[test]
fn hook_probe_record_writes_the_payload_verbatim_and_stays_silent() {
    let world = World::new("hook-probe");
    let dir = world.root.join("recordings");
    std::fs::create_dir_all(&dir).expect("recordings dir");
    let payload = br#"{"hook_event_name":"PreToolUse","tool_name":"Agent"}"#;
    let output = Command::new(BIN)
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
