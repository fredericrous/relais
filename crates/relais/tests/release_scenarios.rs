//! Release acceptance scenarios (SPEC §14, §21, §23) through the real
//! binary: a temporary repository, a fake `claude` that behaves by model
//! and prompt, an isolated state directory, and the CLI as the user
//! would call it. The library-level tests exercise the same paths with
//! mock backends; these prove the wiring — config files, trust grants,
//! the lazily started coordinator, exit codes and artifacts.
//!
//! Unix only: every scenario here drives the fake `claude`, which is a
//! `sh` script. The ones that need no worker — `init`, `install`,
//! `uninstall`, `doctor`, a blocked `plan`, `coordinator status` with
//! no daemon, and the exit-code table — are in `portable_scenarios.rs`
//! and run on Windows too, along with the library-level tests.
#![cfg(unix)]

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
        self.write_policy_with_wall(max_attempts, 120)
    }

    fn write_policy_with_wall(&self, max_attempts: u32, max_wall_seconds: u64) -> String {
        self.write_policy_verifying(
            max_attempts,
            max_wall_seconds,
            "[[verification.profiles.default.commands]]\n\
             argv = [\"sh\", \"-c\", \"test ! -f src/main.rs\"]\n\
             timeout_seconds = 30\n",
        )
    }

    /// A policy whose verification block the scenario writes itself.
    fn write_policy_verifying(
        &self,
        max_attempts: u32,
        max_wall_seconds: u64,
        verification: &str,
    ) -> String {
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
max_wall_seconds = {max_wall_seconds}

[integrations]
aval = "off"
amont = "off"
amont_agent = "off"

{verification}"#
        );
        std::fs::write(self.repo.join("relais.toml"), &policy).expect("policy");
        git(&self.repo, &["add", "relais.toml"]);
        git(&self.repo, &["commit", "-q", "-m", "policy"]);
        RepoPolicy::from_toml_str(&policy)
            .expect("valid policy")
            .authority_hash()
    }

    /// A trust grant is bound to the declaration AND to the repository
    /// it was reviewed for, so the key is the pair (P2), and every grant
    /// names its reviewer (P10).
    fn write_machine(&self, authority_hash: &str, extra: &str) {
        let key = relais::policy::grant_key(authority_hash, &relais::repo::identity(&self.repo));
        std::fs::write(
            self.config.join("machine.toml"),
            format!(
                "schema_version = 1\n{extra}\n[trust.\"{key}\"]\n                 granted_at = \"2026-09-18\"\nreviewed_by = \"the release suite\"\n"
            ),
        )
        .expect("machine");
    }

    fn write_task(&self, name: &str, review: &str) -> PathBuf {
        self.write_task_for(name, "Remove the obsolete entry point", review)
    }

    /// The objective is what carries a prompt marker to the fake worker.
    fn write_task_for(&self, name: &str, objective: &str, review: &str) -> PathBuf {
        self.write_task_scoped(name, objective, review, &["src/**"])
    }

    fn write_task_scoped(
        &self,
        name: &str,
        objective: &str,
        review: &str,
        write_scope: &[&str],
    ) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(
            &path,
            serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": objective,
                "base_ref": "HEAD",
                "write_scope": write_scope,
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

    /// How many times the fake harness was launched for a prompt. The
    /// probe calls (`--version`, `--help`) exit before counting.
    fn worker_launches(&self) -> usize {
        std::fs::read_to_string(self.root.join("invocations.log"))
            .map(|log| log.lines().count())
            .unwrap_or(0)
    }

    /// The run's own artifact directory.
    fn run_dir(&self, run_id: &str) -> PathBuf {
        self.state.join("runs").join(run_id)
    }

    /// The run's task worktree — a SIBLING of the artifact directory, so
    /// that nothing the run records is the worker's cwd parent.
    fn worktree(&self, run_id: &str) -> PathBuf {
        self.state.join("worktrees").join(run_id).join("task")
    }

    /// The single run this world has made. Outcomes other than
    /// `accepted` print no run-id line on stdout, and the directory is
    /// the same identity the ledger uses.
    fn only_run_id(&self) -> String {
        let mut ids: Vec<String> = std::fs::read_dir(self.state.join("runs"))
            .expect("runs directory")
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(ids.len(), 1, "exactly one run: {ids:?}");
        ids.pop().expect("one")
    }

    /// Poll a condition until it holds, up to `seconds`. Everything in
    /// this suite that has to wait for another process waits this way: a
    /// fixed sleep is a guess that is either too short on a loaded CI
    /// runner or wasted time on an idle workstation.
    fn poll_until(condition: impl Fn() -> bool, seconds: u64) -> bool {
        for _ in 0..(seconds * 20) {
            if condition() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    /// Wait for a path to appear, up to `seconds`. Used to prove a
    /// worker's background write really happened before asserting that
    /// the candidate does not contain it.
    fn wait_for(path: &Path, seconds: u64) -> bool {
        Self::poll_until(|| path.exists(), seconds)
    }

    /// Wait for a path to go away. The coordinator unlinks its endpoint
    /// on the way out, so this is how a test knows the daemon it asked to
    /// stop has actually stopped.
    fn wait_for_gone(path: &Path, seconds: u64) -> bool {
        Self::poll_until(|| !path.exists(), seconds)
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
/// the reviewer answers FINDINGS: none. Usage is reported as Claude Code
/// does, so cost accounting is actual.
///
/// Prompt markers select misbehaviour, so one script covers every
/// scenario: "blockage-please" claims blockage; "sleep-please" outlives
/// the run's wall clock and is killed; "policy-edit-please" edits the
/// repository's own `relais.toml`; "late-write-please" writes a file in
/// the background AFTER printing its terminal result.
///
/// Every `-p` launch appends a line to `invocations.log`, which is how a
/// scenario proves that no second worker was started.
const FAKE_CLAUDE: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "fake-claude 9.9.9"; exit 0 ;;
  --help) echo "usage: claude -p --model <model> --effort <level> --output-format <format> --max-budget-usd <amount> --disallowed-tools <tools...> --settings <file-or-json>"; exit 0 ;;
esac
here=$(dirname "$0")
printf '%s\n' "$@" > "$here/argv-last.log"
echo launch >> "$here/invocations.log"
prompt=$(cat)
model=""
while [ $# -gt 0 ]; do
  case "$1" in
    --model) model="$2"; shift ;;
  esac
  shift
done
if printf '%s' "$prompt" | grep -q "semantic reviewer"; then
  printf '{"result":"looked at it\\nFINDINGS: none","session_id":"rev-1","total_cost_usd":0.002,"modelUsage":{"%s":{"outputTokens":2}},"permission_denials":[],"usage":{"input_tokens":10,"output_tokens":2}}\n' "$model"
  exit 0
fi
if printf '%s' "$prompt" | grep -q "blockage-please"; then
  printf '{"result":"relais-blocked: the vendored crate is missing","session_id":"w-b","total_cost_usd":0.001,"modelUsage":{"%s":{"outputTokens":2}},"permission_denials":[],"usage":{"input_tokens":10,"output_tokens":2}}\n' "$model"
  exit 0
fi
if printf '%s' "$prompt" | grep -q "sleep-please"; then
  # Outlive the run's wall clock: the runner kills the process group and
  # the attempt has no terminal result.
  sleep 60
  exit 0
fi
if printf '%s' "$prompt" | grep -q "policy-edit-please"; then
  printf '\n[execution]\nmax_attempts = 99\n' >> relais.toml
  rm -f src/main.rs
  printf '{"result":"DONE","session_id":"w-p","total_cost_usd":0.01,"modelUsage":{"%s":{"outputTokens":10}},"permission_denials":[],"usage":{"input_tokens":100,"output_tokens":10}}\n' "$model"
  exit 0
fi
if printf '%s' "$prompt" | grep -q "late-write-please"; then
  # stdout is redirected away so the pipe closes when this script exits:
  # otherwise the runner would still be reading it when the write lands.
  # The marker is written OUTSIDE the worktree and after the attempted
  # write, so the scenario can prove the background write really ran
  # even when the accepted run has already released the worktree.
  ( sleep 2; echo late > src/late.txt; echo done > "$here/late-write.log" ) >/dev/null 2>&1 &
  rm -f src/main.rs
  printf '{"result":"DONE","session_id":"w-l","total_cost_usd":0.01,"modelUsage":{"%s":{"outputTokens":10}},"permission_denials":[],"usage":{"input_tokens":100,"output_tokens":10}}\n' "$model"
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
printf '{"result":"DONE","session_id":"w-1","total_cost_usd":0.01,"modelUsage":{"%s":{"outputTokens":10}},"permission_denials":[],"usage":{"input_tokens":100,"output_tokens":10}}\n' "$model"
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
    let feedback = world.relais(&[
        "feedback",
        &run_id,
        "--outcome",
        "accepted",
        "--actor",
        "the release suite",
    ]);
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

// SPEC §20 / the outcomes table's first reader: `relais report` counts
// an accepted task as "standing" until feedback says the change was
// taken back. Recording `reverted` must not touch the `accepted` count
// — that is the historical fact that a candidate was accepted — but it
// must drop the task out of `standing`, the count of accepted changes
// still in the tree.
#[test]
fn a_reverted_outcome_leaves_accepted_unchanged_and_drops_standing() {
    let world = World::new("revert");
    let hash = world.write_policy(1);
    world.write_machine(&hash, "");
    let task = world.write_task_for("task.json", "an easy one", "optional");

    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stdout = text(&run.stdout);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{stdout}\n{}",
        text(&run.stderr)
    );
    let run_id = World::run_id_of(&stdout);

    let report = world.relais(&["report", "--since", "2026-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    assert_eq!(report["accepted"], 1, "{report}");
    assert_eq!(report["standing"], 1, "{report}");

    let feedback = world.relais(&[
        "feedback",
        &run_id,
        "--outcome",
        "reverted",
        "--actor",
        "the release suite",
    ]);
    assert_eq!(
        feedback.status.code(),
        Some(0),
        "{}",
        text(&feedback.stderr)
    );

    let report = world.relais(&["report", "--since", "2026-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    assert_eq!(
        report["accepted"], 1,
        "a revert never un-accepts the historical fact: {report}"
    );
    assert_eq!(
        report["standing"], 0,
        "but the change is no longer standing: {report}"
    );
}

// `relais report --by model` groups the window's tasks by the model
// that ran, so a cost comparison is between like task classes rather
// than one number blending every model together (SPEC §11).
#[test]
fn report_by_model_groups_the_accepted_task_under_the_model_that_ran_it() {
    let world = World::new("cohort-model");
    let hash = world.write_policy(1);
    world.write_machine(&hash, "");
    let task = world.write_task_for("task.json", "an easy one", "optional");

    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stdout = text(&run.stdout);
    assert_eq!(
        run.status.code(),
        Some(0),
        "{stdout}\n{}",
        text(&run.stderr)
    );

    let report = world.relais(&["report", "--since", "2026-01-01", "--json", "--by", "model"]);
    assert_eq!(report.status.code(), Some(0), "{}", text(&report.stderr));
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    assert_eq!(
        report["accepted"], 1,
        "the default figures are unchanged: {report}"
    );
    assert_eq!(report["cohorts"]["dimension"], "model", "{report}");
    let cohorts = report["cohorts"]["cohorts"]
        .as_array()
        .expect("cohorts array");
    assert_eq!(cohorts.len(), 1, "one model ran: {report}");
    let cohort = &cohorts[0];
    assert_eq!(cohort["key"], "sonnet", "{report}");
    assert_eq!(cohort["tasks"], 1, "{report}");
    assert_eq!(cohort["accepted"], 1, "{report}");
    assert_eq!(cohort["standing"], 1, "{report}");
    assert_eq!(cohort["terminal"], 1, "{report}");
    assert_eq!(cohort["acceptance_rate"], 1.0, "{report}");
}

// Task-linking: a bare `relais run` derives a fresh task; a second run
// of the same contract launched with `--revise <task-id>` joins that
// task instead of starting a new one, so their cost is one denominator.
#[test]
fn a_revised_run_joins_its_declared_tasks_cost() {
    let world = World::new("revise");
    let hash = world.write_policy(1);
    world.write_machine(&hash, "");
    let task = world.write_task_for("task.json", "an easy one", "optional");

    let first = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let first_stdout = text(&first.stdout);
    assert_eq!(
        first.status.code(),
        Some(0),
        "{first_stdout}\n{}",
        text(&first.stderr)
    );
    let first_run = World::run_id_of(&first_stdout);

    let ledger =
        relais::ledger::Ledger::open(&world.state.join("ledger.sqlite")).expect("open ledger");
    let task_id = ledger
        .task_of_run(&relais::ids::RunId::from_stored(first_run.clone()))
        .expect("query task")
        .expect("first run is on record under a task");

    // A DIFFERENT contract: re-running the identical one derives the
    // identical task from (repo, contract hash) whatever the flag does,
    // so the same assertions would pass with `--revise` ignored. A
    // revision is a different contract for the same task, which is the
    // only shape that can tell the flag apart from the derivation.
    let revised = world.write_task_for("task-revised.json", "an easy one, restated", "optional");
    let second = world.relais(&[
        "run",
        "--task",
        revised.to_str().unwrap(),
        "--revise",
        task_id.as_str(),
    ]);
    let second_stdout = text(&second.stdout);
    assert_eq!(
        second.status.code(),
        Some(0),
        "{second_stdout}\n{}",
        text(&second.stderr)
    );
    let second_run = World::run_id_of(&second_stdout);
    assert_ne!(first_run, second_run, "two distinct runs");

    let second_task = ledger
        .task_of_run(&relais::ids::RunId::from_stored(second_run.clone()))
        .expect("query task")
        .expect("second run is on record under a task");
    assert_eq!(second_task, task_id, "a revised run joins the SAME task");

    let runs = ledger.runs_of_task(&task_id).expect("runs of task");
    assert_eq!(runs.len(), 2, "{runs:?}");

    // And the other half of the discrimination: the same revised
    // contract, run without the flag, is its own task. Without this the
    // test could not tell "the flag joined them" from "everything joins".
    let bare = world.relais(&["run", "--task", revised.to_str().unwrap()]);
    let bare_stdout = text(&bare.stdout);
    assert_eq!(
        bare.status.code(),
        Some(0),
        "{bare_stdout}\n{}",
        text(&bare.stderr)
    );
    let bare_task = ledger
        .task_of_run(&relais::ids::RunId::from_stored(World::run_id_of(
            &bare_stdout,
        )))
        .expect("query task")
        .expect("on record");
    assert_ne!(
        bare_task, task_id,
        "the same contract without --revise starts its own task"
    );

    let cost_first = ledger
        .run_cost(&relais::ids::RunId::from_stored(first_run))
        .expect("first run cost");
    let cost_second = ledger
        .run_cost(&relais::ids::RunId::from_stored(second_run))
        .expect("second run cost");
    let task_cost = ledger.task_cost(&task_id).expect("task cost");
    assert!(task_cost > relais::money::MicroUsd::ZERO);
    assert_eq!(
        task_cost,
        cost_first.saturating_add(cost_second),
        "one task's cost is the sum of both runs"
    );
}

/// A contract that declares a task disagreeing with `--revise`, and a
/// `--revise` naming a task nobody has ever run, are both refused before
/// any worker launches.
#[test]
fn revise_disagreement_and_unknown_task_are_refused_before_dispatch() {
    let world = World::new("revise-refused");
    let hash = world.write_policy(1);
    world.write_machine(&hash, "");
    let task = world.write_task_for("task.json", "an easy one", "optional");

    let unknown = world.relais(&[
        "run",
        "--task",
        task.to_str().unwrap(),
        "--revise",
        "task-0000000000000000",
    ]);
    assert_ne!(unknown.status.code(), Some(0));
    assert_eq!(world.worker_launches(), 0, "no worker for an unknown task");
    assert!(
        text(&unknown.stderr).contains("task-0000000000000000"),
        "{}",
        text(&unknown.stderr)
    );

    // Run once for real to get a task id on record, then declare a
    // DIFFERENT one on the contract than `--revise` names.
    let first = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let first_run = World::run_id_of(&text(&first.stdout));
    let ledger =
        relais::ledger::Ledger::open(&world.state.join("ledger.sqlite")).expect("open ledger");
    let real_task = ledger
        .task_of_run(&relais::ids::RunId::from_stored(first_run))
        .expect("query task")
        .expect("run is on record under a task");

    let declared_path = world.root.join("declared.json");
    let mut declared: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&task).expect("read")).expect("json");
    declared["task_id"] = serde_json::json!(real_task.as_str());
    std::fs::write(&declared_path, declared.to_string()).expect("write");

    let launches_before = world.worker_launches();
    let disagreeing = world.relais(&[
        "run",
        "--task",
        declared_path.to_str().unwrap(),
        "--revise",
        "task-1111111111111111",
    ]);
    assert_ne!(disagreeing.status.code(), Some(0));
    assert_eq!(
        world.worker_launches(),
        launches_before,
        "no worker for a disagreeing declaration"
    );
    let stderr = text(&disagreeing.stderr);
    assert!(stderr.contains(real_task.as_str()), "{stderr}");
    assert!(stderr.contains("task-1111111111111111"), "{stderr}");
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
    // SPEC §11 exhaustion has its own code now (main.rs's table): a
    // caller that retries on "failed" must not retry on a spent ceiling.
    assert_eq!(run.status.code(), Some(5), "{stderr}");
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

// SPEC §10: a baseline that cannot run, and a declared setup that does
// not succeed, are blocked before a worker is launched — with the
// remedy named, and with `relais plan` and `relais doctor` warning
// about the missing setup ahead of the run.
#[test]
fn an_unrunnable_baseline_and_a_failed_setup_block_before_any_launch() {
    let world = World::new("setup");
    std::fs::write(world.repo.join("package-lock.json"), "{}\n").expect("lockfile");
    git(&world.repo, &["add", "package-lock.json"]);
    git(&world.repo, &["commit", "-q", "-m", "lockfile"]);
    // The application-landscape shape: `npm run …` finds npm, and the
    // script inside cannot find its tool.
    let hash = world.write_policy_verifying(
        3,
        120,
        "[[verification.profiles.default.commands]]\n\
         argv = [\"sh\", \"-c\", \"relais-no-such-binary-4f3a --version\"]\n\
         timeout_seconds = 30\n",
    );
    world.write_machine(&hash, "");
    let task = world.write_task("task.json", "off");

    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    assert_eq!(plan.status.code(), Some(0), "{}", text(&plan.stderr));
    assert!(
        text(&plan.stderr).contains("package-lock.json present"),
        "plan warns about the missing setup: {}",
        text(&plan.stderr)
    );
    assert!(
        text(&plan.stderr).contains("argv = [\"npm\", \"ci\"]"),
        "{}",
        text(&plan.stderr)
    );
    let doctor = world.relais(&["doctor"]);
    assert!(
        text(&doctor.stdout).contains("! setup"),
        "doctor warns too: {}",
        text(&doctor.stdout)
    );

    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(3), "{}", text(&run.stderr));
    let stderr = text(&run.stderr);
    assert!(stderr.contains("baseline_unrunnable"), "{stderr}");
    assert!(stderr.contains("exited 127"), "{stderr}");
    assert!(
        stderr.contains("[[verification.profiles.default.setup]]"),
        "the block to declare is named: {stderr}"
    );
    assert_eq!(world.worker_launches(), 0, "no worker was launched");
    let run_id = world.only_run_id();
    assert!(
        world.run_dir(&run_id).join("logs/base-cmd0.log").exists(),
        "the base's log is kept as evidence"
    );
    assert!(
        !world.worktree(&run_id).exists(),
        "no task worktree was created"
    );

    // A declared setup that fails: blocked as well, still before a launch.
    let hash = world.write_policy_verifying(
        3,
        120,
        "[[verification.profiles.default.setup]]\n\
         argv = [\"sh\", \"-c\", \"echo install failed >&2; exit 1\"]\n\
         timeout_seconds = 30\n\n\
         [[verification.profiles.default.commands]]\n\
         argv = [\"sh\", \"-c\", \"test ! -f src/main.rs\"]\n\
         timeout_seconds = 30\n",
    );
    world.write_machine(&hash, "");
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    assert!(
        !text(&plan.stderr).contains("declares no setup"),
        "a declared setup silences the warning: {}",
        text(&plan.stderr)
    );
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(3), "{}", text(&run.stderr));
    let stderr = text(&run.stderr);
    assert!(stderr.contains("verification_setup_failed"), "{stderr}");
    assert!(stderr.contains("the base revision"), "{stderr}");
    assert_eq!(world.worker_launches(), 0, "still no worker");
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
    // needs_decision: a human has to decide, which is a different code
    // from "the contract you handed me is invalid" below (main.rs's
    // exit-code table).
    assert_eq!(run.status.code(), Some(8), "{}", text(&run.stderr));
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

// The decision spine (SPEC-new): a run that reaches `needs_decision` opens
// a decision record nobody has answered, `report` lists it, `relais
// decide` answers it by name, and `report` no longer lists it — the
// resolution and its actor are on record for `explain` to show.
#[test]
fn a_run_awaiting_a_person_is_answered_by_decide_and_drops_off_the_open_list() {
    let world = World::new("decide");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
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
    assert_eq!(run.status.code(), Some(8), "{}", text(&run.stderr));
    let run_id = world.only_run_id();

    let report = world.relais(&["report", "--since", "2000-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    let open: Vec<&str> = report["open_decisions"]
        .as_array()
        .expect("open_decisions is an array")
        .iter()
        .map(|decision| decision["run"].as_str().expect("run"))
        .collect();
    assert!(open.contains(&run_id.as_str()), "{report}");

    let decide = world.relais(&[
        "decide",
        &run_id,
        "--answer",
        "decided",
        "--actor",
        "the release suite",
        "--note",
        "the scope was intentional; the task will be revised",
    ]);
    assert_eq!(decide.status.code(), Some(0), "{}", text(&decide.stderr));

    let report = world.relais(&["report", "--since", "2000-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    assert!(
        report["open_decisions"]
            .as_array()
            .expect("array")
            .is_empty(),
        "{report}"
    );

    let explain = world.relais(&["explain", &run_id]);
    let explained = text(&explain.stdout);
    assert!(explained.contains("decision_recorded"), "{explained}");
    assert!(explained.contains("the release suite"), "{explained}");
    assert!(
        explained.contains("the scope was intentional"),
        "{explained}"
    );

    // The run is not waiting any more: `decided` ended it in `cancelled`
    // (a person's answer ends the run they answered), so a second answer
    // is refused by the state check itself, before it ever reaches the
    // decision row.
    let report = world.relais(&["report", "--since", "2000-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    assert_eq!(report["runs"][0]["status"], "cancelled", "{report}");

    let redecide = world.relais(&[
        "decide",
        &run_id,
        "--answer",
        "decided",
        "--actor",
        "someone else",
    ]);
    assert_eq!(
        redecide.status.code(),
        Some(2),
        "{}",
        text(&redecide.stderr)
    );
    assert!(
        text(&redecide.stderr).contains("not waiting on a person"),
        "{}",
        text(&redecide.stderr)
    );
}

/// `relais evidence attach` records one row against a run and, when it
/// names one, the acceptance criterion it answers — and decides nothing:
/// no criterion is marked met, no state changes, no gap clears. It
/// refuses a run it does not know, and refuses a criterion id the run's
/// contract does not declare, naming the ids it does.
#[test]
fn evidence_attach_refuses_unknowns_and_records_what_it_is_told() {
    let world = World::new("evidence-attach");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    let path = world.root.join("narrow.json");
    let statement = "src/main.rs no longer exists";
    std::fs::write(
        &path,
        serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Remove the entry point",
            "base_ref": "HEAD",
            "write_scope": ["docs/**"],
            "acceptance": [statement],
            "verification_profile": "default",
            "review": "off",
        })
        .to_string(),
    )
    .expect("task");
    // Scoped to docs/** while the criterion is about src/main.rs: the
    // cheapest way to a run that carries a real contract without waiting
    // on the fake worker to fix anything.
    let run = world.relais(&["run", "--task", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(8), "{}", text(&run.stderr));
    let run_id = world.only_run_id();
    let criterion_id: relais::acceptance::AcceptanceEntry = statement.into();
    let criterion_id = criterion_id.id();

    let evidence_path = world.root.join("coverage.json");
    std::fs::write(&evidence_path, "{\"lines\": 100}").expect("evidence file");

    let unknown_run = world.relais(&[
        "evidence",
        "attach",
        "not-a-real-run",
        "--path",
        evidence_path.to_str().unwrap(),
    ]);
    assert_eq!(
        unknown_run.status.code(),
        Some(10),
        "{}",
        text(&unknown_run.stderr)
    );
    assert!(
        text(&unknown_run.stderr).contains("unknown run"),
        "{}",
        text(&unknown_run.stderr)
    );

    let unknown_criterion = world.relais(&[
        "evidence",
        "attach",
        &run_id,
        "--path",
        evidence_path.to_str().unwrap(),
        "--criterion",
        "not-a-real-id",
    ]);
    assert_eq!(
        unknown_criterion.status.code(),
        Some(2),
        "{}",
        text(&unknown_criterion.stderr)
    );
    assert!(
        text(&unknown_criterion.stderr).contains(&criterion_id),
        "the refusal names the ids the contract does declare: {}",
        text(&unknown_criterion.stderr)
    );

    let attach = world.relais(&[
        "evidence",
        "attach",
        &run_id,
        "--path",
        evidence_path.to_str().unwrap(),
        "--tool",
        "coverage-bot",
        "--external-id",
        "cov-42",
        "--subject",
        "src/main.rs",
        "--criterion",
        &criterion_id,
    ]);
    assert_eq!(attach.status.code(), Some(0), "{}", text(&attach.stderr));

    // Recording never decides: the run is still exactly where it was,
    // waiting on a person, not settled by the evidence just attached.
    let report = world.relais(&["report", "--since", "2000-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    let open: Vec<&str> = report["open_decisions"]
        .as_array()
        .expect("open_decisions is an array")
        .iter()
        .map(|decision| decision["run"].as_str().expect("run"))
        .collect();
    assert!(open.contains(&run_id.as_str()), "{report}");

    let explain = world.relais(&["explain", &run_id]);
    let explained = text(&explain.stdout);
    assert!(explained.contains("external_attestation"), "{explained}");
    assert!(explained.contains("coverage-bot"), "{explained}");
    assert!(explained.contains(&criterion_id), "{explained}");

    // A RELATIVE `--path` is stored the way every other evidence row is
    // stored: absolute. The runner joins its artifacts directory and
    // `decide` joins the runs directory, and `explain` prints them all
    // together — a row saying `report.json` resolves from whatever
    // directory the person happened to be in, which is no directory at
    // all by the time anyone reads it back. `relais` runs with the repo
    // as its cwd here, so this is the path a person would actually type.
    std::fs::write(world.repo.join("report.json"), "{}").expect("relative evidence file");
    let relative = world.relais(&[
        "evidence",
        "attach",
        &run_id,
        "--path",
        "report.json",
        "--tool",
        "coverage-bot",
    ]);
    assert_eq!(
        relative.status.code(),
        Some(0),
        "{}",
        text(&relative.stderr)
    );

    let explain = world.relais(&["explain", &run_id]);
    let explained = text(&explain.stdout);
    let recorded = explained
        .lines()
        .find(|line| line.contains("report.json"))
        .expect("the attached row is printed: {explained}");
    assert!(
        recorded.contains(
            &world
                .repo
                .canonicalize()
                .expect("repo")
                .display()
                .to_string()
        ),
        "a relative path is recorded absolute, like every other evidence row: {recorded}"
    );
}

/// A person's answer ends the run they answered (SPEC §9): `approve`
/// assigns `accepted`, and every other answer — `reject`, `revise`,
/// `decided`, `abandon` — assigns `cancelled`. All five raise their
/// decision the same way here (a scope-exceeded run carries no
/// verification gaps, so `approve` is not refused) and each gets a run
/// of its own so one answer's transition cannot be read off another's.
#[test]
fn every_decide_answer_ends_the_run_they_answered_with_its_own_terminal_status() {
    let answers: [(&str, &str); 5] = [
        ("approve", "accepted"),
        ("reject", "cancelled"),
        ("revise", "cancelled"),
        ("decided", "cancelled"),
        ("abandon", "cancelled"),
    ];
    for (answer, expected_status) in answers {
        let world = World::new(&format!("decide-{answer}"));
        let hash = world.write_policy(3);
        world.write_machine(&hash, "");
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
        assert_eq!(
            run.status.code(),
            Some(8),
            "{answer}: {}",
            text(&run.stderr)
        );
        let run_id = world.only_run_id();

        let decide = world.relais(&[
            "decide",
            &run_id,
            "--answer",
            answer,
            "--actor",
            "the release suite",
        ]);
        assert_eq!(
            decide.status.code(),
            Some(0),
            "{answer}: {}",
            text(&decide.stderr)
        );

        let report = world.relais(&["report", "--since", "2000-01-01", "--json"]);
        let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
        assert_eq!(
            report["runs"][0]["status"], expected_status,
            "{answer}: {report}"
        );
    }
}

/// A mandatory criterion whose evidence is a human sign-off stops the
/// run `needs_decision` naming the gap, without a reviewer ever running
/// — even though policy requires one for every other candidate — and is
/// cleared by nothing but `relais decide --answer approve --criterion
/// <id>` (SPEC §10). That answer also assigns `accepted` (a person's
/// answer, not relais's own checks) and re-seals the receipt the runner
/// had already prepared, so `relais feedback` — which only ever accepts
/// an ACCEPTED run with a receipt — accepts it exactly like a
/// relais-accepted one. An unknown criterion id is refused first, naming
/// the ids the contract actually declares.
#[test]
fn a_human_sign_off_gap_is_cleared_only_by_decide_and_then_feedback_accepts_it() {
    let world = World::new("signoff");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    let statement = "a person signed off on the migration";
    let entry: relais::acceptance::AcceptanceEntry = serde_json::from_value(serde_json::json!({
        "statement": statement,
        "evidence": {"kind": "human_sign_off"},
    }))
    .expect("parses");
    let criterion_id = entry.id();
    // A second criterion the contract settles some OTHER way, to prove a
    // sign-off cannot answer it.
    let tested = "the entry point's absence is covered by a test";
    let tested_entry: relais::acceptance::AcceptanceEntry =
        serde_json::from_value(serde_json::json!({
            "statement": tested,
            "evidence": {"kind": "test", "authorship": "model_added"},
        }))
        .expect("parses");
    let path = world.root.join("signoff.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "schema_version": 1,
            "kind": "change",
            "objective": "Remove the entry point, an easy one",
            "base_ref": "HEAD",
            "write_scope": ["src/**"],
            "acceptance": [
                {"statement": statement, "evidence": {"kind": "human_sign_off"}},
                {"statement": tested, "evidence": {"kind": "test", "authorship": "model_added"}},
            ],
            "verification_profile": "default",
            "review": "required",
        })
        .to_string(),
    )
    .expect("task");
    let run = world.relais(&["run", "--task", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(8), "{}", text(&run.stderr));
    assert!(
        text(&run.stderr).contains("human sign-off"),
        "{}",
        text(&run.stderr)
    );
    let run_id = world.only_run_id();
    assert_eq!(
        world.worker_launches(),
        1,
        "the checks passed; nothing but the sign-off gap remained, so no reviewer ran"
    );

    let bad_criterion = world.relais(&[
        "decide",
        &run_id,
        "--answer",
        "approve",
        "--actor",
        "a person",
        "--criterion",
        "not-a-real-id",
    ]);
    assert_eq!(
        bad_criterion.status.code(),
        Some(2),
        "{}",
        text(&bad_criterion.stderr)
    );
    assert!(
        text(&bad_criterion.stderr).contains(&criterion_id),
        "the refusal names the ids the contract does declare: {}",
        text(&bad_criterion.stderr)
    );

    // A criterion the contract settles some OTHER way cannot be signed
    // off either: doing so would re-seal the receipt claiming a person
    // settled what a test was declared to settle — the second acceptance
    // path this mechanism exists to refuse. The id is real and the
    // contract does declare it, so only the evidence kind refuses it.
    let wrong_kind = world.relais(&[
        "decide",
        &run_id,
        "--answer",
        "approve",
        "--actor",
        "a person",
        "--criterion",
        &tested_entry.id(),
    ]);
    assert_eq!(
        wrong_kind.status.code(),
        Some(2),
        "{}",
        text(&wrong_kind.stderr)
    );
    assert!(
        text(&wrong_kind.stderr).contains("not settled by a human sign-off"),
        "{}",
        text(&wrong_kind.stderr)
    );

    let decide = world.relais(&[
        "decide",
        &run_id,
        "--answer",
        "approve",
        "--actor",
        "a person",
        "--note",
        "looks fine",
        "--criterion",
        &criterion_id,
    ]);
    assert_eq!(decide.status.code(), Some(0), "{}", text(&decide.stderr));

    let report = world.relais(&["report", "--since", "2000-01-01", "--json"]);
    let report: serde_json::Value = serde_json::from_str(&text(&report.stdout)).expect("json");
    assert_eq!(report["runs"][0]["status"], "accepted", "{report}");

    // The receipt the runner had already stored is RE-SEALED, not joined
    // by a second one: the criterion is met by the sign-off, the gap it
    // raised is gone, and the outcome now agrees with the run's own
    // state. A receipt still reading `needs_decision` beside an accepted
    // run would be two records of one fact disagreeing.
    let receipt: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(world.state.join("runs").join(&run_id).join("receipt.json"))
            .expect("receipt"),
    )
    .expect("json");
    assert_eq!(receipt["outcome"], "accepted", "{receipt}");
    assert_eq!(
        receipt["verification"]["gaps"],
        serde_json::json!([]),
        "the answered gap is no longer named: {receipt}"
    );
    let signed = receipt["criteria"]
        .as_array()
        .expect("criteria")
        .iter()
        .find(|criterion| criterion["id"] == criterion_id.as_str())
        .expect("the signed criterion is in the receipt");
    assert_eq!(signed["met"], true, "{signed}");
    assert_eq!(signed["evidence"]["kind"], "human_sign_off", "{signed}");

    let feedback = world.relais(&[
        "feedback",
        &run_id,
        "--outcome",
        "accepted",
        "--actor",
        "a person",
    ]);
    assert_eq!(
        feedback.status.code(),
        Some(0),
        "a person-approved run is accepted exactly like a relais-accepted one: {}",
        text(&feedback.stderr)
    );
}

/// A run whose acceptance criterion the contract's own scope excludes
/// (SPEC §9) reaches `needs_decision` before the runner ever writes a
/// receipt — there is nothing to seal yet. `relais decide --answer
/// approve` still accepts it (a person's answer, not relais's own
/// checks), and `relais feedback` about it has no candidate to verify —
/// this is the case a receipt-less accepted run exists for.
#[test]
fn relais_feedback_accepts_a_person_approved_run_with_no_receipt() {
    let world = World::new("feedback-no-receipt");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
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
    assert_eq!(run.status.code(), Some(8), "{}", text(&run.stderr));
    let run_id = world.only_run_id();

    let receipt_path = world.state.join("runs").join(&run_id).join("receipt.json");
    assert!(
        !receipt_path.exists(),
        "a scope-exceeded run has nothing to seal yet: {}",
        receipt_path.display()
    );

    let decide = world.relais(&[
        "decide",
        &run_id,
        "--answer",
        "approve",
        "--actor",
        "the release suite",
    ]);
    assert_eq!(decide.status.code(), Some(0), "{}", text(&decide.stderr));

    // A scope-exceeded run finished its attempt, so the LEDGER knows
    // what it built even though no receipt does. That, not the caller,
    // is what a `--candidate` is checked against: naming another sha is
    // refused exactly as it would be against a receipt.
    let wrong_candidate = world.relais(&[
        "feedback",
        &run_id,
        "--outcome",
        "accepted",
        "--actor",
        "a person",
        "--candidate",
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    ]);
    assert_eq!(
        wrong_candidate.status.code(),
        Some(2),
        "a candidate the run never produced is not recordable as the one it did: {}",
        text(&wrong_candidate.stderr)
    );

    let feedback = world.relais(&[
        "feedback",
        &run_id,
        "--outcome",
        "accepted",
        "--actor",
        "a person",
    ]);
    assert_eq!(
        feedback.status.code(),
        Some(0),
        "an accepted run with no receipt still accepts feedback: {}",
        text(&feedback.stderr)
    );
}

// SPEC §17, §21: execute → verify → label → build dataset → train →
// evaluate → promote → route. The loop closes: a promoted artifact is
// what `plan` and `run` consult, and it says so.
#[test]
fn the_learning_loop_closes_from_runs_to_a_learned_route() {
    let world = World::new("learn");
    let hash = world.write_policy(3);
    // Twelve runs is a scenario, not a corpus: the shipped promotion
    // gates want twenty held-out records supporting the selected route
    // before they call an artifact evidence-backed. This scenario proves
    // the LOOP closes, so it lowers that threshold explicitly — which is
    // also what makes the setting visible as the knob it is.
    world.write_machine(
        &hash,
        "[routing]\nmin_supported_test_records = 1\nmax_abstention_rate = 1.0\n",
    );
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
    world.write_machine(
        &hash,
        "[routing]\nlearned_enabled = false\nmin_supported_test_records = 1\n",
    );
    let plan = world.relais(&["plan", "--task", task.to_str().unwrap()]);
    assert!(
        text(&plan.stdout).contains("cold start"),
        "{}",
        text(&plan.stdout)
    );
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
    // The daemon unlinks its endpoint as it exits: wait for that rather
    // than for a fixed 200 ms, which was both a guess and a flake (C15).
    let socket = world.state.join("relais.sock");
    assert!(
        World::wait_for_gone(&socket, 10),
        "the coordinator asked to stop should have unlinked {}",
        socket.display()
    );
    let status = world.relais(&["coordinator", "status"]);
    assert!(
        text(&status.stdout).contains("no coordinator"),
        "{}",
        text(&status.stdout)
    );
    // Under --json the absent case is a document too, with the client's
    // own error carried in it — never an English sentence a parser would
    // choke on (C3).
    let json = world.relais(&["coordinator", "status", "--json"]);
    assert_eq!(json.status.code(), Some(0), "{}", text(&json.stderr));
    let document: serde_json::Value =
        serde_json::from_str(text(&json.stdout).trim()).expect("stdout is JSON in every state");
    assert_eq!(document["coordinator"], "absent", "{document}");
    assert!(
        document["cause"].as_str().is_some_and(|c| !c.is_empty()),
        "the reason nothing answered is carried: {document}"
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

// SPEC §8, §11: the machine's permission allowlist reaches the worker as an
// explicit settings document, the dollar ceiling as the CLI's own flag, and
// no permission-mode or bypass flag ever appears on the argv.
#[test]
fn launch_argv_carries_machine_permissions_and_the_budget_flag() {
    let world = World::new("argv");
    let hash = world.write_policy(1);
    world.write_machine(
        &hash,
        "[spending]\nper_run_micros = 2500000\n\n[permissions]\nallowed_tools = [\"Edit\", \"Bash(cargo test:*)\"]\n",
    );
    let task = world.write_task("task.json", "off");
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let argv = std::fs::read_to_string(world.root.join("argv-last.log")).expect("argv log");
    let args: Vec<&str> = argv.lines().collect();
    assert_eq!(args[0], "-p", "{argv}");
    let budget_at = args
        .iter()
        .position(|arg| *arg == "--max-budget-usd")
        .unwrap_or_else(|| panic!("budget flag missing: {argv}"));
    assert_eq!(args[budget_at + 1], "2.5");
    assert!(!args.contains(&"--budget"), "{argv}");
    let settings_at = args
        .iter()
        .position(|arg| *arg == "--settings")
        .unwrap_or_else(|| panic!("settings flag missing: {argv}"));
    let settings: serde_json::Value = serde_json::from_str(args[settings_at + 1]).expect("json");
    assert_eq!(
        settings["permissions"]["allow"],
        serde_json::json!(["Edit", "Bash(cargo test:*)"])
    );
    assert!(args.contains(&"--disallowed-tools"), "{argv}");
    assert!(
        !argv.contains("permission-mode") && !argv.contains("dangerously"),
        "{argv}"
    );
    // sonnet churns and never fixes; one attempt, then failed — the point
    // here is the argv, not the outcome.
    assert_ne!(run.status.code(), Some(0));
}

// SPEC §14: interrupted execution resumes without duplicate live workers
// or blind command replay. A worker that outlives the run's wall clock is
// killed with no terminal result: the run is `interrupted`, and `resume`
// reconciles it without starting a second worker.
#[test]
fn an_interrupted_worker_is_reconciled_by_resume_without_a_second_worker() {
    let world = World::new("intr");
    // Six seconds is long enough to reach the dispatch and far short of
    // the fake worker's sixty-second sleep.
    let hash = world.write_policy_with_wall(3, 6);
    world.write_machine(&hash, "");
    let task = world.write_task_for(
        "task.json",
        "Remove the obsolete entry point (sleep-please)",
        "off",
    );

    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stderr = text(&run.stderr);
    assert_eq!(run.status.code(), Some(6), "{stderr}");
    assert!(stderr.starts_with("interrupted:"), "{stderr}");
    let run_id = world.only_run_id();
    assert!(
        stderr.contains(&format!("relais resume {run_id}")),
        "the run says how to reconcile itself: {stderr}"
    );
    assert_eq!(
        world.worker_launches(),
        1,
        "one worker was launched and killed"
    );

    // Resume reconciles what is provably dead and reports the state; it
    // never re-dispatches, and it never replays a command whose side
    // effects are unknown.
    let resume = world.relais(&["resume", &run_id]);
    let resumed = text(&resume.stdout);
    assert!(
        resumed.contains("interrupted"),
        "resume reports the reconciled state: {resumed}"
    );
    assert_eq!(
        world.worker_launches(),
        1,
        "resume must not launch a second worker"
    );
    // The evidence is preserved, not cleaned up behind the failure.
    assert!(world.run_dir(&run_id).join("manifest.json").is_file());
}

// SPEC §8: a run that ends without acceptance keeps a NAMED candidate,
// not a directory — the runner retires the worktree at its terminal
// state — and a worktree left behind anyway (an older release, a
// retirement that failed) is retired by `resume --retire`: whatever it
// holds that no candidate does becomes `…/final` and a patch first.
#[test]
fn a_leftover_worktree_is_retired_by_resume_with_its_tree_named() {
    let world = World::new("retire");
    // One attempt, no allowance: sonnet churns once and the ceiling
    // ends the run without a candidate anyone accepted.
    let hash = world.write_policy(1);
    world.write_machine(&hash, "");
    let task = world.write_task("task.json", "off");
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stderr = text(&run.stderr);
    assert_eq!(run.status.code(), Some(5), "{stderr}");
    let run_id = world.only_run_id();
    assert!(
        !world.worktree(&run_id).exists(),
        "the runner retired the worktree at the run's end"
    );
    assert!(
        world.run_dir(&run_id).join("candidate-1.patch").is_file(),
        "the attempt's candidate is its patch"
    );
    let explained = text(&world.relais(&["explain", &run_id]).stdout);
    assert!(explained.contains("worktree_retired"), "{explained}");

    // A leftover, as an older release left them: the run's worktree
    // directory, holding an edit no candidate has.
    let leftover = world.worktree(&run_id);
    git(
        &world.repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &leftover.to_string_lossy(),
            "HEAD",
        ],
    );
    std::fs::write(leftover.join("src/leftover.rs"), "// never exported\n").expect("write");
    // `doctor` counts it and names the sweep; a leftover is never a
    // blocker, so the finding is a warning.
    let worktrees_finding = |world: &World| -> serde_json::Value {
        let report: serde_json::Value =
            serde_json::from_str(text(&world.relais(&["doctor", "--json"]).stdout).trim())
                .expect("doctor --json is a document");
        report["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .find(|f| f["component"] == "worktrees")
            .unwrap_or_else(|| panic!("no `worktrees` finding in {report}"))
            .clone()
    };
    let before = worktrees_finding(&world);
    assert_eq!(before["level"], "warn", "{before}");
    let detail = before["detail"].as_str().expect("detail");
    assert!(detail.starts_with("1 run worktree(s) retained"), "{detail}");
    assert!(detail.contains("relais resume --retire"), "{detail}");
    let nothing_to_do = world.relais(&["resume", "--retire", "--all"]);
    let swept = text(&nothing_to_do.stdout);
    assert_eq!(
        nothing_to_do.status.code(),
        Some(0),
        "{swept}\n{}",
        text(&nothing_to_do.stderr)
    );
    let final_ref = format!("refs/relais/candidates/{run_id}/final");
    assert!(
        swept.contains(&format!("{run_id}: retired")) && swept.contains(&final_ref),
        "the run, the ref and the bytes are reported: {swept}"
    );
    assert!(swept.contains("bytes reclaimed"), "{swept}");
    assert!(!leftover.exists(), "the leftover is gone");
    assert_eq!(
        git(
            &world.repo,
            &["show", &format!("{final_ref}:src/leftover.rs")]
        ),
        "// never exported\n",
        "and its tree survives as the run's final candidate"
    );
    assert!(
        world
            .run_dir(&run_id)
            .join("candidate-final.patch")
            .is_file(),
        "with the patch beside the run's evidence"
    );
    let listed = git(&world.repo, &["worktree", "list"]);
    assert!(!listed.contains(&run_id), "{listed}");
    let after = worktrees_finding(&world);
    assert_eq!(after["level"], "ok", "{after}");
    // Nothing left: the sweep says so and exits 0.
    let again = world.relais(&["resume", "--retire"]);
    assert_eq!(again.status.code(), Some(0));
    assert!(
        text(&again.stdout).contains("no run worktree is retained"),
        "{}",
        text(&again.stdout)
    );
    // The single-run form answers the same way for a run with nothing
    // retained, and keeps the existing semantics for the run itself.
    let one = world.relais(&["resume", &run_id, "--retire"]);
    let one_out = text(&one.stdout);
    assert_eq!(one.status.code(), Some(0), "{one_out}");
    assert!(one_out.contains("already terminal"), "{one_out}");
    assert!(one_out.contains("no worktree is retained"), "{one_out}");
}

// SPEC §14: a worker cannot obtain acceptance by modifying policy. The
// repository's own relais.toml is protected: a candidate that touches it
// is needs_decision, and the run is judged with the policy loaded from
// the original checkout, never the worker's edit.
#[test]
fn a_worker_editing_the_policy_cannot_obtain_acceptance() {
    let world = World::new("policy");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    // A scope broad enough to match the policy file on its own terms:
    // the point is that even an in-scope edit to protected repository
    // configuration is refused, not merely an out-of-scope one.
    let task = world.write_task_scoped(
        "task.json",
        "Remove the obsolete entry point (policy-edit-please)",
        "off",
        &["src/**", "*.toml"],
    );
    let run = world.relais(&["run", "--task", task.to_str().unwrap()]);
    let stderr = text(&run.stderr);
    assert_eq!(run.status.code(), Some(8), "{stderr}");
    assert!(stderr.contains("needs_decision"), "{stderr}");
    assert!(
        stderr.contains("relais.toml (protected repository configuration)"),
        "the edit is named as the protected file it is: {stderr}"
    );
    // The user's own checkout still holds the policy that was reviewed.
    let policy = std::fs::read_to_string(world.repo.join("relais.toml")).expect("policy");
    assert!(
        !policy.contains("max_attempts = 99"),
        "the worker's edit never reached the original checkout"
    );
    let run_id = world.only_run_id();
    assert!(
        !world.run_dir(&run_id).join("receipt.json").exists(),
        "a policy edit is not an acceptance"
    );
}

// SPEC §14: a worker cannot obtain acceptance by changing files after
// verification. The candidate is a snapshot taken when the worker's
// terminal result arrives; anything written afterwards is outside it,
// and the run is judged on the snapshot.
#[test]
fn files_written_after_the_result_are_not_in_the_candidate() {
    let world = World::new("late");
    let hash = world.write_policy(3);
    world.write_machine(&hash, "");
    let task = world.write_task_for(
        "task.json",
        "Remove the obsolete entry point (late-write-please)",
        "off",
    );
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
        &std::fs::read_to_string(world.run_dir(&run_id).join("receipt.json")).expect("receipt"),
    )
    .expect("json");
    let candidate = receipt["candidate_sha"].as_str().expect("candidate");

    // The background write never gets to happen: when the worker exits,
    // relais kills its whole process group before draining its pipes, so
    // the descendant that would have written after the terminal result
    // is gone (audit V1). The marker is outside the worktree, so its
    // absence is the descendant's death and not a released worktree.
    let marker = world.root.join("late-write.log");
    assert!(
        !World::wait_for(&marker, 5),
        "a descendant that outlived the worker wrote after the result ({})",
        marker.display()
    );
    let late = world.worktree(&run_id).join("src/late.txt");
    assert!(
        !late.exists(),
        "nothing was written into the worktree after the candidate was snapshotted"
    );

    let tree = git(&world.repo, &["ls-tree", "-r", "--name-only", candidate]);
    assert!(
        !tree.contains("src/late.txt"),
        "a file written after the terminal result is not in the candidate: {tree}"
    );
    assert!(
        !tree.contains("src/main.rs"),
        "the change the worker did make before finishing IS in the candidate: {tree}"
    );
    let patch =
        std::fs::read_to_string(world.run_dir(&run_id).join("candidate-1.patch")).expect("patch");
    assert!(!patch.contains("late.txt"), "{patch}");
    assert_eq!(
        receipt["verification"]["candidate_sha"], candidate,
        "the verdict is bound to the snapshot, not to the worktree as it is now"
    );
}
