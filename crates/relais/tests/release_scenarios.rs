//! Release acceptance scenarios (SPEC §14, §21, §23) through the real
//! binary: a temporary repository, a fake `claude` that behaves by model
//! and prompt, an isolated state directory, and the CLI as the user
//! would call it. The library-level tests exercise the same paths with
//! mock backends; these prove the wiring — config files, trust grants,
//! the lazily started coordinator, exit codes and artifacts.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use relais::policy::RepoPolicy;

const BIN: &str = env!("CARGO_BIN_EXE_relais");

/// One isolated world: repo, state dir, config dir, fake claude. Short
/// paths under /tmp because the coordinator socket lives in the state
/// dir and Unix socket paths are limited to ~100 bytes.
struct World {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    config: PathBuf,
    claude: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from("/tmp").join(format!(
            "rl-it-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
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
        let claude = root.join("claude");
        std::fs::write(&claude, FAKE_CLAUDE).expect("fake claude");
        let mut mode = std::fs::metadata(&claude).expect("meta").permissions();
        use std::os::unix::fs::PermissionsExt;
        mode.set_mode(0o755);
        std::fs::set_permissions(&claude, mode).expect("chmod");
        Self {
            root,
            repo,
            state,
            config,
            claude,
        }
    }

    /// relais.toml with the integrations off (this world has no aval or
    /// amont on PATH) and a check that is green only once src/main.rs is
    /// gone — the fix-the-failing-state shape the fake worker knows.
    fn write_policy(&self, max_attempts: u32) -> String {
        let policy = format!(
            r#"schema_version = 1

[models.research]
id = "haiku"

[models.implementation]
id = "sonnet"

[models.escalation]
id = "fable"

[execution]
max_attempts = {max_attempts}
max_repairs_before_escalation = 1
max_wall_seconds = 120

[integrations]
aval = "off"
amont = "off"
amont_agent = "off"

[[verification.profiles.default.commands]]
argv = ["sh", "-c", "test ! -f src/main.rs"]
timeout_seconds = 30
"#
        );
        std::fs::write(self.repo.join("relais.toml"), &policy).expect("policy");
        git(&self.repo, &["add", "relais.toml"]);
        git(&self.repo, &["commit", "-q", "-m", "policy"]);
        RepoPolicy::from_toml_str(&policy)
            .expect("valid policy")
            .authority_hash()
    }

    fn write_machine(&self, authority_hash: &str, extra: &str) {
        std::fs::write(
            self.config.join("machine.toml"),
            format!(
                "schema_version = 1\n{extra}\n[trust.\"{authority_hash}\"]\ngranted_at = \"2026-09-18\"\n"
            ),
        )
        .expect("machine");
    }

    fn write_task(&self, name: &str, review: &str) -> PathBuf {
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
                "review": review,
            })
            .to_string(),
        )
        .expect("task");
        path
    }

    fn relais(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.repo)
            .env("RELAIS_STATE_DIR", &self.state)
            .env("RELAIS_CONFIG_DIR", &self.config)
            .env("RELAIS_CLAUDE_BIN", &self.claude)
            .env("RELAIS_SESSION_ID", "tab-test")
            .output()
            .expect("relais runs")
    }

    fn stop_coordinator(&self) {
        let _ = self.relais(&["coordinator", "stop"]);
    }

    fn run_id_of(stdout: &str) -> String {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("run:"))
            .map(|id| id.trim().to_string())
            .expect("run id line")
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.stop_coordinator();
        // Worktrees registered in the repo point into the state dir;
        // remove everything together.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// The fake harness: answers --version/--help like Claude Code, and on
/// `-p` reads the prompt from stdin and acts by model and prompt.
/// sonnet churns (a new file each time, never the fix); fable fixes;
/// the reviewer answers FINDINGS: none. A prompt mentioning
/// "blockage-please" makes the worker claim blockage. Usage is
/// reported as Claude Code does, so cost accounting is actual.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "fake-claude 9.9.9"; exit 0 ;;
  --help) echo "usage: claude -p --model --effort --max-turns --output-format --budget --disallowed-tools"; exit 0 ;;
esac
prompt=$(cat)
model=""
while [ $# -gt 0 ]; do
  case "$1" in
    --model) model="$2"; shift ;;
  esac
  shift
done
if printf '%s' "$prompt" | grep -q "semantic reviewer"; then
  printf '{"result":"looked at it\\nFINDINGS: none","session_id":"rev-1","total_cost_usd":0.002,"model":"%s","usage":{"input_tokens":10,"output_tokens":2}}\n' "$model"
  exit 0
fi
if printf '%s' "$prompt" | grep -q "blockage-please"; then
  printf '{"result":"relais-blocked: the vendored crate is missing","session_id":"w-b","total_cost_usd":0.001,"model":"%s","usage":{"input_tokens":10,"output_tokens":2}}\n' "$model"
  exit 0
fi
if printf '%s' "$prompt" | grep -q "an easy one"; then
  rm -f src/main.rs
else
  case "$model" in
    fable) rm -f src/main.rs ;;
    *) echo churn > "src/tick-$$-$(date +%s%N).txt" ;;
  esac
fi
printf '{"result":"DONE","session_id":"w-1","total_cost_usd":0.01,"model":"%s","usage":{"input_tokens":100,"output_tokens":10}}\n' "$model"
"#;

// SPEC §14: a bounded change routes to the configured model, passes
// required verification and produces a receipt bound to the candidate.
// SPEC §23: the coordinator starts lazily and the run is managed.
#[test]
fn bounded_change_end_to_end_with_receipt_status_explain_report_and_feedback() {
    let world = World::new("e2e");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    // Escalation is what fixes it here; make the route start there so a
    // single attempt suffices: risk floor on src/**.
    let policy_extra = r#"
[[risk]]
paths = ["src/**"]
minimum_tier = "escalation"
"#;
    let mut policy = std::fs::read_to_string(world.repo.join("relais.toml")).expect("read");
    policy.push_str(policy_extra);
    std::fs::write(world.repo.join("relais.toml"), &policy).expect("write");
    git(&world.repo, &["commit", "-q", "-am", "risk"]);
    let hash = RepoPolicy::from_toml_str(&policy)
        .expect("policy")
        .authority_hash();
    world.write_machine(&hash, "");
    let task = world.write_task("task.json", "required");

    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    assert_eq!(plan.status.code(), Some(0), "{}", text(&plan.stderr));
    assert!(
        text(&plan.stdout).contains("route: escalation / fable"),
        "{}",
        text(&plan.stdout)
    );

    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stdout = text(&run.stdout);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{stdout}\n{}",
        text(&run.stderr)
    );
    assert!(stdout.starts_with("accepted: "), "{stdout}");
    let run_id = World::run_id_of(&stdout);
    let run_dir = world.state.join("runs").join(&run_id);
    let receipt: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("receipt.json")).expect("receipt"),
    )
    .expect("json");
    assert_eq!(receipt["run_id"], run_id);
    assert_eq!(receipt["attempts"], 1);
    assert_eq!(receipt["models_used"], serde_json::json!(["fable"]));
    assert_eq!(receipt["cost_completeness"], "actual");
    assert!(run_dir.join("candidate-1.patch").exists());
    assert!(run_dir.join("review.txt").exists());
    // The candidate the receipt names is a real commit in the repo.
    let candidate = receipt["candidate_sha"].as_str().unwrap();
    git(&world.repo, &["cat-file", "-e", candidate]);
    // The user's checkout is untouched (SPEC §8).
    assert!(world.repo.join("src/main.rs").exists());

    let status = world.relais(&["status", &run_id]);
    assert!(
        text(&status.stdout).contains("accepted"),
        "{}",
        text(&status.stdout)
    );
    let explain = world.relais(&["explain", &run_id]);
    let explained = text(&explain.stdout);
    assert!(
        explained.contains("checks_and_review_passed"),
        "{explained}"
    );
    assert!(explained.contains("cost: $0.012 (actual)"), "{explained}");
    let report = world.relais(&["report", "--since", "2026-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    assert_eq!(report["accepted"], 1, "{report}");
    let feedback = world.relais(&["feedback", &run_id, "--outcome", "accepted"]);
    assert_eq!(
        feedback.status.code(),
        Some(0),
        "{}",
        text(&feedback.stderr)
    );
    let resume = world.relais(&["resume", &run_id]);
    assert!(text(&resume.stdout).contains("already terminal"));

    // The coordinator was started lazily and saw this session.
    let coord = world.relais(&["coordinator", "status"]);
    let coord_out = text(&coord.stdout);
    assert!(coord_out.contains("sessions: tab-test"), "{coord_out}");
    assert!(coord_out.contains(&run_id), "{coord_out}");
    let doctor = world.relais(&["doctor"]);
    assert!(
        text(&doctor.stdout).contains("answers on"),
        "{}",
        text(&doctor.stdout)
    );
}

// SPEC §14: a failed implementation gets at most the configured repair
// and escalation attempts; all costs stay attributed to one task.
#[test]
fn repair_then_escalation_all_attributed_to_one_run() {
    let world = World::new("esc");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    let task = world.write_task("task.json", "off");
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stdout = text(&run.stdout);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{stdout}\n{}",
        text(&run.stderr)
    );
    let run_id = World::run_id_of(&stdout);
    let receipt: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(world.state.join("runs").join(&run_id).join("receipt.json"))
            .expect("receipt"),
    )
    .expect("json");
    assert_eq!(
        receipt["attempts"], 3,
        "initial, one repair, one escalation"
    );
    assert_eq!(
        receipt["models_used"],
        serde_json::json!(["sonnet", "fable"])
    );
    assert_eq!(receipt["cost"], 30_000, "micro-USD, an integer");
    let explained = text(&world.relais(&["explain", &run_id]).stdout);
    assert!(explained.contains("repairing"), "{explained}");
    assert!(explained.contains("escalating"), "{explained}");
}

// SPEC §14: budget exhaustion preserves the patch and evidence and
// prevents further dispatch; reports distinguish the outcome.
#[test]
fn budget_exhaustion_preserves_evidence_and_stops_dispatch() {
    let world = World::new("budget");
    let hash = world.write_policy(3);
    // Two sonnet attempts at $0.01 each reach a $0.02 ceiling before the
    // escalation could be bought.
    world.write_machine(&hash, "[spending]\nper_run_micros = 20000\n");
    let task = world.write_task("task.json", "off");
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stderr = text(&run.stderr);
    assert_eq!(run.status.code(), Some(4), "{stderr}");
    assert!(stderr.starts_with("budget_exhausted:"), "{stderr}");
    let runs = std::fs::read_dir(world.state.join("runs"))
        .expect("runs")
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0].join("candidate-2.patch").exists(),
        "the patch is preserved"
    );
    assert!(
        !runs[0].join("candidate-3.patch").exists(),
        "no third dispatch"
    );
    let report: serde_json::Value = serde_json::from_str(&text(
        &world
            .relais(&["report", "--since", "2026-01-01", "--json"])
            .stdout,
    ))
    .expect("json");
    assert_eq!(report["accepted"], 0);
    assert_eq!(report["runs"].as_array().map(Vec::len), Some(1));
    assert_eq!(report["runs"][0]["status"], "budget_exhausted");
    assert_eq!(report["runs"][0]["cost_completeness"], "actual");
}

// SPEC §14: missing required integrations, unavailable models and
// missing permissions produce explicit blocked outcomes; dirty source
// worktrees are handled explicitly.
#[test]
fn blocked_outcomes_are_explicit_and_launch_nothing() {
    let world = World::new("blocked");
    let hash = world.write_policy(3);
    let task = world.write_task("task.json", "off");

    // No trust grant: blocked before any dispatch.
    std::fs::write(world.config.join("machine.toml"), "schema_version = 1\n").expect("machine");
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(3), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("missing_trust_grant"),
        "{}",
        text(&run.stderr)
    );

    // Model not allowed by the machine.
    world.write_machine(&hash, "allowed_models = [\"haiku\"]\n");
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(3), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("model_unavailable"),
        "{}",
        text(&run.stderr)
    );

    // Harness binary missing: blocked, never a fallback.
    world.write_machine(&hash, "");
    let missing = Command::new(BIN)
        .args(["run", "--task", task.to_str().unwrap()])
        .current_dir(&world.repo)
        .env("RELAIS_STATE_DIR", &world.state)
        .env("RELAIS_CONFIG_DIR", &world.config)
        .env("RELAIS_CLAUDE_BIN", world.root.join("no-such-claude"))
        .output()
        .expect("relais");
    assert_eq!(missing.status.code(), Some(3), "{}", text(&missing.stderr));

    // Dirty tree: explicit, never copied.
    std::fs::write(world.repo.join("src/wip.rs"), "// uncommitted\n").expect("dirty");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    assert_eq!(plan.status.code(), Some(3));
    assert!(
        text(&plan.stderr).contains("dirty"),
        "{}",
        text(&plan.stderr)
    );
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(3));
    assert!(
        text(&run.stderr).contains("dirty_base"),
        "{}",
        text(&run.stderr)
    );
    std::fs::remove_file(world.repo.join("src/wip.rs")).expect("clean");

    // Worker-claimed blockage: blocked, not escalated to a stronger model.
    let path = world.root.join("blocked.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Remove the entry point (blockage-please)",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "acceptance": ["src/main.rs no longer exists"],
            "verification_profile": "default",
            "review": "off",
        })
        .to_string(),
    )
    .expect("task");
    let run = world.relais(&["run", "--task", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(3), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("env_missing"),
        "{}",
        text(&run.stderr)
    );
    // Every blocked run is recorded, and none of them produced a
    // candidate: nothing was accepted, nothing was snapshotted.
    let candidates = std::fs::read_dir(world.state.join("runs"))
        .expect("runs")
        .flatten()
        .filter(|entry| entry.path().join("candidate-1.patch").exists())
        .count();
    assert_eq!(candidates, 0, "no blocked run reached a candidate");
}

// SPEC §14: a worker cannot obtain acceptance by omitting a failed
// check or changing files outside scope; unknown contract fields are
// rejected.
#[test]
fn out_of_scope_edits_and_misspelled_controls_are_refused() {
    let world = World::new("scope");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    // The fake sonnet worker writes src/tick-*.txt; a scope that excludes
    // src/ makes every candidate a scope violation.
    let path = world.root.join("narrow.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Remove the entry point",
            "base_ref": "HEAD",
            "write_scope": ["docs/**"],
            "acceptance": ["src/main.rs no longer exists"],
            "verification_profile": "default",
            "review": "off",
        })
        .to_string(),
    )
    .expect("task");
    let run = world.relais(&["run", "--task", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(2), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("scope_exceeded"),
        "{}",
        text(&run.stderr)
    );

    let typo = world.root.join("typo.json");
    std::fs::write(
        &typo,
        r#"{"schema_version":1,"kind":"change","objective":"x","base_ref":"HEAD","write_scope":["src/**"],"acceptance":["y"],"verification_profile":"default","reveiw":"off"}"#,
    )
    .expect("task");
    let plan = world.relais(&["plan", "--task", typo.to_str().unwrap()]);
    assert_eq!(plan.status.code(), Some(2));
    assert!(
        text(&plan.stderr).contains("unknown field `reveiw`"),
        "{}",
        text(&plan.stderr)
    );
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

// SPEC §17, §21: execute → verify → label → build dataset → train →
// evaluate → promote → route. The loop closes: a promoted artifact is
// what `plan` and `run` consult, and it says so.
#[test]
fn the_learning_loop_closes_from_runs_to_a_learned_route() {
    let world = World::new("learn");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    // Twelve distinct easy tasks the cheap tier solves outright: twelve
    // families, all accepted without escalation.
    for n in 0..12 {
        let path = world.root.join(format!("easy-{n}.json"));
        std::fs::write(
            &path,
            serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": format!("Remove the entry point, an easy one, variant {n} {}", ["alpha","beta","gamma","delta","eps","zeta","eta","theta","iota","kappa","lambda","mu"][n]),
                "base_ref": "HEAD",
                "write_scope": ["src/**"],
                "acceptance": ["src/main.rs no longer exists"],
                "verification_profile": "default",
                "review": "off",
            })
            .to_string(),
        )
        .expect("task");
        let run = world.relais(&["run", "--task", path.to_str().unwrap()]);
        assert_eq!(run.status.code(), Some(0), "{}", text(&run.stderr));
        let receipt_attempts = World::run_id_of(&text(&run.stdout));
        let _ = receipt_attempts;
    }
    let built = world.relais(&["dataset", "build"]);
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr));
    assert!(
        text(&built.stdout).contains("12 accepted-without-escalation"),
        "{}",
        text(&built.stdout)
    );
    let trained = world.relais(&["train"]);
    let trained_out = text(&trained.stdout);
    assert_eq!(
        trained.status.code(),
        Some(0),
        "{trained_out}
{}",
        text(&trained.stderr)
    );
    assert!(trained_out.contains("gates: PASSED"), "{trained_out}");
    let artifact = trained_out
        .lines()
        .find_map(|line| line.strip_prefix("candidate artifact: "))
        .expect("artifact id")
        .trim()
        .to_string();
    let evaluated = world.relais(&["evaluate", "--artifact", &artifact]);
    assert_eq!(
        evaluated.status.code(),
        Some(0),
        "{}",
        text(&evaluated.stderr)
    );
    let promoted = world.relais(&["promote", &artifact]);
    assert_eq!(
        promoted.status.code(),
        Some(0),
        "promotion reads the evaluator's own report: {}",
        text(&promoted.stderr)
    );
    // The next plan is routed by the artifact, and says which one.
    let task = world.write_task("next.json", "off");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    let planned = text(&plan.stdout);
    assert_eq!(
        plan.status.code(),
        Some(0),
        "{planned}
{}",
        text(&plan.stderr)
    );
    assert!(
        planned.contains(&format!("learned artifact {artifact}")),
        "{planned}"
    );
    // Learned routing can be switched off without touching anything else.
    world.write_machine(&hash, "[routing]\nlearned_enabled = false\n");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    assert!(
        text(&plan.stdout).contains("cold start"),
        "{}",
        text(&plan.stdout)
    );
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

// SPEC §23: cancelling one run through the coordinator leaves the
// machine usable, and a stopped coordinator does not take the ledger
// with it.
#[test]
fn coordinator_cancel_and_stop_leave_state_consistent() {
    let world = World::new("coord");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    let task = world.write_task("task.json", "off");
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(0), "{}", text(&run.stderr));
    let cancel = world.relais(&["coordinator", "cancel", "--run", "not-a-run"]);
    assert_eq!(cancel.status.code(), Some(0), "{}", text(&cancel.stderr));
    let stop = world.relais(&["coordinator", "stop"]);
    assert_eq!(stop.status.code(), Some(0));
    std::thread::sleep(std::time::Duration::from_millis(200));
    let status = world.relais(&["coordinator", "status"]);
    assert!(
        text(&status.stdout).contains("no coordinator"),
        "{}",
        text(&status.stdout)
    );
    let listing = world.relais(&["status"]);
    assert!(
        text(&listing.stdout).contains("accepted"),
        "{}",
        text(&listing.stdout)
    );
    // The next run starts a fresh coordinator by itself.
    let again = world.relais(&["run", "--task", task.to_str().unwrap()]);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
}
