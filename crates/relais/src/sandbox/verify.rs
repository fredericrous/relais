//! `relais doctor --verify-sandbox`: two real probe sessions, and the record
//! of a pass.
//!
//! [`verify_sandbox`] prepares a relais-style worktree under the state directory,
//! runs the sandbox probe (a worker's launch, one fixture richer) and the
//! allowlist probe (the launch relais uses with `[sandbox]` off), judges
//! both transcripts with [`evaluate`] and, only when both pass, records the
//! configuration under [`dispatch_key`]. What launches a session, and git,
//! arrive as arguments, so every branch is testable with canned
//! stream-json; the command wires the real ones.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::{
    dispatch_key, evaluate, probe_plan, probe_plan_allowlist, probe_prompt, short_tmp_link,
    store_path, worker_launch, DispatchKeyInputs, InitExpect, LaunchInputs, ProbePlanInputs,
    ProbeReport, ProbeStep, StoreError, TmpLink, VerificationKey, VerificationRecord,
    VerificationStore,
};
use crate::backend::{
    worker_launch_env, BackendError, Capabilities, LaunchEnv, LaunchSpec, ProbeLauncher,
    SandboxLaunch,
};
use crate::money::MicroUsd;
use crate::policy::SandboxSettings;
use crate::workspace::{self, Git};

const PROBE_MODEL: &str = "haiku";
/// The most one probe session may spend.
const PROBE_BUDGET_MICROS: i64 = 500_000;
const PROBE_WALL: Duration = Duration::from_secs(300);
/// The synthetic credential both probes put in the launch environment; the
/// sandbox must keep it from the worker's commands, the allowlist scrub
/// from its subprocesses.
const SYNTHETIC_ENV: &str = "AWS_SECRET_ACCESS_KEY";
/// The tools a sandboxed worker's `init` record lists.
const SANDBOX_TOOLS: [&str; 6] = ["Bash", "Read", "Edit", "Write", "Grep", "Glob"];
/// What the allowlist probe's session may run without asking: the two
/// programs its steps use.
const ALLOWLIST_PROBE_TOOLS: [&str; 2] = ["Bash(printenv:*)", "Bash(echo:*)"];
/// Planted in the probe worktree: a project setting that would take
/// `python3` out of the sandbox if the harness read it. A worker launched
/// `--restricted` must not.
const PLANTED_SETTINGS: &str = r#"{"sandbox":{"excludedCommands":["python3"]}}"#;

/// What one verification is run from. `settings` is the machine's
/// `[sandbox]`, `base_env` the environment a worker starts from, `managed`
/// the bytes of the managed files the preflight judged.
pub struct VerifyInputs<'a> {
    pub settings: &'a SandboxSettings,
    pub state_dir: &'a Path,
    /// Random hex, unique to this attempt.
    pub nonce: &'a str,
    pub home: &'a Path,
    /// The directory the probe's short temp-dir link is made under.
    pub tmp_link_root: &'a Path,
    pub config_dir: &'a Path,
    pub ledger_path: &'a Path,
    /// Relais's own environment, as the credential floor reads it.
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub base_env: &'a LaunchEnv,
    pub capabilities: &'a Capabilities,
    pub platform: &'a str,
    pub managed: &'a [(PathBuf, Vec<u8>)],
    pub verified_at: &'a str,
}

/// Why a verification could not be carried out. A probe that ran and failed
/// is an outcome, not an error.
#[derive(Debug)]
pub enum VerifyError {
    /// The throwaway repository or worktree could not be prepared.
    Prepare(String),
    /// The probe steps could not be built for these paths.
    Plan(String),
    /// The harness version is unknown, so there is nothing to key by.
    Harness,
    /// A probe session could not be run.
    Launch {
        probe: &'static str,
        cause: String,
    },
    Store(StoreError),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::Prepare(why) => {
                write!(f, "the probe worktree could not be prepared: {why}")
            }
            VerifyError::Plan(why) => write!(f, "the probe cannot be planned: {why}"),
            VerifyError::Harness => {
                write!(
                    f,
                    "the harness version is unknown, so a verification has no key"
                )
            }
            VerifyError::Launch { probe, cause } => {
                write!(f, "the {probe} probe session could not run: {cause}")
            }
            VerifyError::Store(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// What the two probe sessions showed.
#[derive(Debug)]
pub struct VerifyOutcome {
    pub sandbox: ProbeReport,
    pub allowlist: ProbeReport,
    /// The two sessions' cost, when both reported one.
    pub cost: Option<MicroUsd>,
    /// The configuration probed, and recorded when both passed.
    pub key: VerificationKey,
}

impl VerifyOutcome {
    pub fn passed(&self) -> bool {
        self.sandbox.passed && self.allowlist.passed
    }

    /// Both reports, the cost and whether a record was written.
    pub fn render(&self) -> String {
        let cost = match self.cost {
            Some(cost) => format!("cost: {cost}"),
            None => "cost: not reported".to_string(),
        };
        let standing = if self.passed() {
            format!("verified: recorded under {}", &self.key.as_str()[..12])
        } else {
            "NOT verified: no record written".to_string()
        };
        format!(
            "sandbox probe\n{}\nallowlist probe\n{}\n{cost}\n{standing}\n",
            self.sandbox.render(),
            self.allowlist.render()
        )
    }
}

/// The directories one attempt uses, under `<state_dir>/sandbox/probe/<nonce>`.
struct Prepared {
    dir: PathBuf,
    worktree: PathBuf,
    fixture: PathBuf,
    scratch: PathBuf,
}

/// A throwaway repository with one commit, a `git worktree add` checkout of
/// it (as `workspace::create_worktree` makes a task's), the planted
/// project settings in that worktree, the fixture credential and a scratch
/// directory. The fixture holds `relais-probe-<nonce>` and must exist
/// before the launch: `test -r` answers `denied` for a missing file too.
fn prepare(git: &dyn Git, state_dir: &Path, nonce: &str) -> Result<Prepared, VerifyError> {
    let io = |what: &str, e: std::io::Error| VerifyError::Prepare(format!("{what}: {e}"));
    let vcs = |e: workspace::WorkspaceError| VerifyError::Prepare(e.to_string());
    let dir = state_dir.join("sandbox").join("probe").join(nonce);
    let repo = dir.join("repo");
    let worktree = dir.join("worktree");
    let fixture_dir = dir.join("fixture");
    let scratch = dir.join("scratch");
    for created in [&repo, &fixture_dir, &scratch] {
        std::fs::create_dir_all(created).map_err(|e| io("creating the probe directories", e))?;
    }
    std::fs::write(repo.join("README"), "relais sandbox probe\n")
        .map_err(|e| io("writing the probe repository", e))?;
    git.run(&repo, &["init", "-q"]).map_err(vcs)?;
    git.run(&repo, &["add", "README"]).map_err(vcs)?;
    // No hooks and no signing: this repository belongs to the probe, not to
    // the user's git configuration.
    git.run(
        &repo,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "user.name=relais-probe",
            "-c",
            "user.email=probe@relais.invalid",
            "commit",
            "-q",
            "-m",
            "probe",
        ],
    )
    .map_err(vcs)?;
    let base = workspace::resolve_base(&repo, "HEAD").map_err(vcs)?;
    workspace::create_worktree(&repo, &base, &worktree).map_err(vcs)?;

    let settings_dir = worktree.join(".claude");
    std::fs::create_dir_all(&settings_dir).map_err(|e| io("creating .claude", e))?;
    std::fs::write(settings_dir.join("settings.json"), PLANTED_SETTINGS)
        .map_err(|e| io("planting the project settings", e))?;
    let fixture = fixture_dir.join("probe-credential");
    std::fs::write(&fixture, format!("relais-probe-{nonce}\n"))
        .map_err(|e| io("writing the fixture credential", e))?;
    Ok(Prepared {
        dir,
        worktree,
        fixture,
        scratch,
    })
}

/// Probe directories kept before a new attempt: failed ones stay for
/// diagnosis until this many newer ones exist.
const PROBE_DIRS_KEPT: usize = 3;

/// Removes all but the most recent [`PROBE_DIRS_KEPT`] directories (by
/// mtime) under `<state_dir>/sandbox/probe`.
fn prune_probe_dirs(state_dir: &Path) {
    let root = state_dir.join("sandbox").join("probe");
    let Ok(entries) = std::fs::read_dir(&root) else {
        // No probe directory yet, or none readable: nothing to prune.
        return;
    };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = entries
        // An entry that cannot be read is left alone, not counted.
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let modified = entry.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((modified, entry.path()))
        })
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, stale) in dirs.into_iter().skip(PROBE_DIRS_KEPT) {
        // Best effort: a probe directory that stays is litter, not a wrong answer.
        std::fs::remove_dir_all(&stale).ok();
    }
}

/// A probe launch: haiku, a $0.50 cap and the prompt for `steps`, in the
/// probe worktree.
fn probe_spec(
    nonce: &str,
    label: &str,
    steps: &[ProbeStep],
    worktree: &Path,
    env: LaunchEnv,
    allowed_tools: &[&str],
    sandbox: Option<SandboxLaunch>,
) -> LaunchSpec {
    LaunchSpec {
        dispatch_id: format!("sandbox-probe-{label}-{nonce}"),
        prompt: probe_prompt(steps),
        model: PROBE_MODEL.to_string(),
        effort: None,
        max_turns: None,
        budget_micros: Some(PROBE_BUDGET_MICROS),
        disallowed_tools: Vec::new(),
        allowed_tools: allowed_tools.iter().map(|tool| tool.to_string()).collect(),
        work_dir: worktree.to_path_buf(),
        env,
        wall_timeout: PROBE_WALL,
        cancel: None,
        pid_slot: None,
        sandbox,
        tools: crate::backend::ToolSet::ModeDefault,
    }
}

/// A worker's launch inputs for `settings`: the machine's paths and the
/// worker environment's names, whatever the scratch directory.
fn launch_inputs_for<'a>(
    inputs: &VerifyInputs<'a>,
    settings: &'a SandboxSettings,
    launch_env_names: &'a [String],
    scratch: &'a Path,
    tmp_link: &'a Path,
) -> LaunchInputs<'a> {
    LaunchInputs {
        settings,
        home: inputs.home,
        config_dir: inputs.config_dir,
        ledger_path: inputs.ledger_path,
        env: inputs.env,
        launch_env_names,
        scratch,
        tmp_link,
    }
}

/// The `system`/`init` record of a stream-json transcript.
fn init_record(stream: &str) -> Option<Value> {
    stream
        .lines()
        // A line that is not JSON is `evaluate`'s to reject, not ours to skip past.
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| {
            record.get("type").and_then(Value::as_str) == Some("system")
                && record.get("subtype").and_then(Value::as_str) == Some("init")
        })
}

/// What the session reported it cost, from its terminal `result` record.
fn session_cost(stream: &str) -> Option<MicroUsd> {
    stream
        .lines()
        // A line that is not JSON carries no cost; `evaluate` rejects it.
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|record| record.get("type").and_then(Value::as_str) == Some("result"))
        .find_map(|record| record.get("total_cost_usd").and_then(Value::as_f64))
        .map(MicroUsd::from_dollars)
}

/// Runs the two probes and, when both pass, records the configuration.
pub fn verify_sandbox(
    inputs: &VerifyInputs,
    git: &dyn Git,
    launcher: &dyn ProbeLauncher,
) -> Result<VerifyOutcome, VerifyError> {
    let harness_version = inputs
        .capabilities
        .version
        .as_deref()
        .ok_or(VerifyError::Harness)?;
    let nonce = inputs.nonce;
    // Loaded first, as a check only: a store that cannot be read must not
    // cost two paid sessions whose result could not be recorded. It is
    // loaded AGAIN just before the record is written, so a record another
    // verification wrote meanwhile is kept, not overwritten.
    VerificationStore::load(&store_path(inputs.state_dir)).map_err(VerifyError::Store)?;
    prune_probe_dirs(inputs.state_dir);
    let prepared = prepare(git, inputs.state_dir, nonce)?;
    let synthetic = format!("relais-probe-{nonce}");
    // The probe launches with the synthetic credential on top of the
    // worker env, so the floor's `credentials.envVars` must deny it too —
    // as a worker carrying that name would. Built from the env actually
    // launched; the recorded key keeps the base names a dispatch uses.
    let mut probe_env_names = inputs.base_env.names();
    if !probe_env_names.iter().any(|name| name == SYNTHETIC_ENV) {
        probe_env_names.push(SYNTHETIC_ENV.to_string());
    }
    let tmp_link = short_tmp_link(inputs.tmp_link_root, nonce, &prepared.scratch);
    let launch_inputs = |settings| {
        launch_inputs_for(
            inputs,
            settings,
            &probe_env_names,
            &prepared.scratch,
            &tmp_link,
        )
    };
    let stream = |probe: &'static str, spec: &LaunchSpec| {
        launcher
            .stream(spec)
            .map_err(|e: BackendError| VerifyError::Launch {
                probe,
                cause: e.to_string(),
            })
    };

    // The sandbox probe: a worker's launch, with the fixture added to the
    // floor.
    let mut with_fixture = inputs.settings.clone();
    with_fixture
        .deny_read
        .push(prepared.fixture.to_string_lossy().into_owned());
    let sandbox = worker_launch(&launch_inputs(&with_fixture));
    // Held across both sessions: the link the probe's `CLAUDE_CODE_TMPDIR` names.
    let _tmp_link = TmpLink::create(&sandbox)
        .map_err(|why| VerifyError::Prepare(format!("the temp-dir link: {why}")))?;
    let steps = probe_plan(&ProbePlanInputs {
        nonce,
        fixture: &prepared.fixture,
        home: inputs.home,
    })
    .map_err(VerifyError::Plan)?;
    let env =
        worker_launch_env(inputs.base_env, Some(&sandbox)).with_var(SYNTHETIC_ENV, &synthetic);
    let spec = probe_spec(
        nonce,
        "sandbox",
        &steps,
        &prepared.worktree,
        env,
        &[],
        Some(sandbox),
    );
    let sandbox_stream = stream("sandbox", &spec)?;
    let sandbox_report = evaluate(
        &steps,
        &sandbox_stream,
        init_record(&sandbox_stream).as_ref(),
        InitExpect::Confined {
            tools: &SANDBOX_TOOLS,
        },
    );

    // The allowlist probe: the launch relais uses with `[sandbox]` off.
    let steps = probe_plan_allowlist(nonce);
    let env = worker_launch_env(inputs.base_env, None).with_var(SYNTHETIC_ENV, &synthetic);
    let spec = probe_spec(
        nonce,
        "allowlist",
        &steps,
        &prepared.worktree,
        env,
        &ALLOWLIST_PROBE_TOOLS,
        None,
    );
    let allowlist_stream = stream("allowlist", &spec)?;
    // This probe is about the environment only: its launch has no
    // `--strict-mcp-config`, so the user's own MCP servers and plugins show
    // in the init record, and that record is not judged.
    let allowlist_report = evaluate(
        &steps,
        &allowlist_stream,
        init_record(&allowlist_stream).as_ref(),
        InitExpect::EnvOnly,
    );

    let cost = session_cost(&sandbox_stream)
        .zip(session_cost(&allowlist_stream))
        .map(|(a, b)| a + b);
    // The key a later DISPATCH computes: the base env names (no synthetic
    // credential) and no fixture, or no record would ever match.
    let base_env_names = inputs.base_env.names();
    let key = dispatch_key(&DispatchKeyInputs {
        harness_version,
        platform: inputs.platform,
        launch: &launch_inputs_for(
            inputs,
            inputs.settings,
            &base_env_names,
            &prepared.scratch,
            &tmp_link,
        ),
        managed: inputs.managed,
    });
    let outcome = VerifyOutcome {
        sandbox: sandbox_report,
        allowlist: allowlist_report,
        cost,
        key,
    };
    if outcome.passed() {
        let mut store =
            VerificationStore::load(&store_path(inputs.state_dir)).map_err(VerifyError::Store)?;
        store.record(VerificationRecord {
            key: outcome.key.as_str().to_string(),
            verified_at: inputs.verified_at.to_string(),
            harness_version: harness_version.to_string(),
            platform: inputs.platform.to_string(),
            report: outcome
                .sandbox
                .render()
                .lines()
                .chain(outcome.allowlist.render().lines())
                .map(str::to_string)
                .collect(),
        });
        store.save().map_err(VerifyError::Store)?;
        // Best effort: a failed removal leaves litter in the probe's own directory, not a wrong answer.
        std::fs::remove_dir_all(&prepared.dir).ok();
    }
    Ok(outcome)
}

// Unix only: the sandbox is, and the probe repository leans on `/dev/null`.
#[cfg(all(test, unix))]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use crate::sandbox::probe::Expect;

    use super::*;
    use crate::backend::CLAUDE_TMPDIR;
    use crate::sandbox::{ProbeTool, Verdict, VerificationKey};
    use crate::test_support::temp_dir;
    use crate::workspace::SystemGit;

    const NONCE: &str = "0123456789abcdef";
    const HOME: &str = "/nonexistent-home";

    /// The result S0 measured for each step, by id.
    fn measured(step: &ProbeStep) -> String {
        match step.id {
            "pipe" => "a".to_string(),
            "tmp-write" | "home-write" => format!(
                "Exit code 1\n(eval):1: operation not permitted: {}",
                step.input
            ),
            "network" => "Exit code 56\ncurl: (56) CONNECT tunnel failed, response 403\n000\n\
                <sandbox_violations>\ndeny network-outbound example.com:443 \
                (host is not on the allow list)\n</sandbox_violations>"
                .to_string(),
            "unix-socket" => "unix-socket-bound".to_string(),
            "log-redirect" => "x".to_string(),
            "python-write" => "y".to_string(),
            "fixture-bash" => "denied".to_string(),
            "synthetic-env" | "scrub-env" => "absent".to_string(),
            "auth-env" => "absent\nabsent".to_string(),
            "git" => "A  relais-probe-git".to_string(),
            "excluded-python" => "PermissionError: [Errno 1] Operation not permitted".to_string(),
            "fixture-read" => "<tool_use_error>File is in a directory that is denied by your \
                permission settings.</tool_use_error>"
                .to_string(),
            "fixture-grep" => "Permission to read the directory has been denied.".to_string(),
            other => panic!("no measured result for {other}"),
        }
    }

    fn call_input(step: &ProbeStep) -> Value {
        match step.tool {
            ProbeTool::Bash => json!({"command": step.input}),
            ProbeTool::Read => json!({"file_path": step.input}),
            ProbeTool::Grep => {
                let (pattern, path) = step.input.split_once('\t').expect("pattern and path");
                json!({"pattern": pattern, "path": path})
            }
        }
    }

    fn tool_name(step: &ProbeStep) -> &'static str {
        match step.tool {
            ProbeTool::Bash => "Bash",
            ProbeTool::Read => "Read",
            ProbeTool::Grep => "Grep",
        }
    }

    /// The stream-json a session that did `steps` prints: the init record,
    /// a call and a result per step, and the terminal result with its cost.
    fn stream_of(steps: &[ProbeStep], result: impl Fn(&ProbeStep) -> String) -> String {
        let mut lines = vec![json!({
            "type": "system", "subtype": "init",
            "tools": SANDBOX_TOOLS, "mcp_servers": [],
            "plugins": [{"name": "core", "source": "core@builtin"}],
        })];
        for step in steps {
            let id = format!("toolu_{}", step.id);
            lines.push(json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": id, "name": tool_name(step), "input": call_input(step)}
            ]}}));
            // As the harness records it (S0, 2.1.285): a denied or refused
            // call is an error result; a successful one is not.
            let denied = match step.expect {
                Expect::OsDenied | Expect::NetworkViolation | Expect::PermissionDenied => true,
                Expect::OutputLine(_) | Expect::OutputLineEndsWith(_) => false,
            };
            let mut item =
                json!({"type": "tool_result", "tool_use_id": id, "content": result(step)});
            if denied {
                item["is_error"] = json!(true);
            }
            lines.push(json!({"type": "user", "message": {"content": [item]}}));
        }
        lines.push(json!({"type": "result", "subtype": "success", "total_cost_usd": 0.02}));
        lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A launcher that answers from canned transcripts and keeps what it
    /// was asked to launch. `ignored_step` names a step whose result shows
    /// the sandbox was ignored.
    struct Canned {
        ignored_step: Option<&'static str>,
        launched: Mutex<Vec<LaunchSpec>>,
        /// The fixture as it was on disk at the sandbox launch.
        fixture_at_launch: Mutex<Option<String>>,
        /// Where the sandbox launch's `CLAUDE_CODE_TMPDIR` resolved to at launch.
        tmpdir_at_launch: Mutex<Option<PathBuf>>,
        /// Run once during the first launch: what another process does to
        /// the store while the paid sessions are running.
        meanwhile: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl Canned {
        fn new(ignored_step: Option<&'static str>) -> Self {
            Canned {
                ignored_step,
                launched: Mutex::new(Vec::new()),
                fixture_at_launch: Mutex::new(None),
                tmpdir_at_launch: Mutex::new(None),
                meanwhile: Mutex::new(None),
            }
        }
    }

    impl ProbeLauncher for Canned {
        fn stream(&self, spec: &LaunchSpec) -> Result<String, BackendError> {
            self.launched.lock().unwrap().push(spec.clone());
            if let Some(action) = self.meanwhile.lock().unwrap().take() {
                action();
            }
            let steps = if spec.sandbox.is_some() {
                *self.tmpdir_at_launch.lock().unwrap() = spec
                    .env
                    .vars()
                    .iter()
                    .find(|(name, _)| name == CLAUDE_TMPDIR)
                    .and_then(|(_, link)| std::fs::canonicalize(link).ok());
                let fixture = spec
                    .work_dir
                    .parent()
                    .expect("worktree has a parent")
                    .join("fixture")
                    .join("probe-credential");
                *self.fixture_at_launch.lock().unwrap() = std::fs::read_to_string(&fixture).ok();
                probe_plan(&ProbePlanInputs {
                    nonce: NONCE,
                    fixture: &fixture,
                    home: Path::new(HOME),
                })
                .expect("a plan")
            } else {
                probe_plan_allowlist(NONCE)
            };
            let ignored = self.ignored_step;
            Ok(stream_of(&steps, |step| {
                if Some(step.id) == ignored {
                    String::new()
                } else {
                    measured(step)
                }
            }))
        }
    }

    struct Machine {
        state: crate::test_support::TempDir,
        settings: SandboxSettings,
        capabilities: Capabilities,
        base_env: LaunchEnv,
        managed: Vec<(PathBuf, Vec<u8>)>,
    }

    impl Machine {
        fn new(tag: &str) -> Self {
            Machine {
                state: temp_dir(tag),
                settings: SandboxSettings {
                    enabled: true,
                    writable: vec!["/opt/out".to_string()],
                    network: vec!["api.anthropic.com".to_string()],
                    deny_read: vec!["~/private".to_string()],
                },
                capabilities: Capabilities {
                    version: Some("2.1.285".to_string()),
                    ..Capabilities::default()
                },
                base_env: LaunchEnv::from_ambient(&[
                    ("PATH".to_string(), "/usr/bin".to_string()),
                    ("ANTHROPIC_API_KEY".to_string(), "not-a-key".to_string()),
                ]),
                managed: vec![(
                    PathBuf::from("/etc/claude-code/managed-settings.json"),
                    b"{}".to_vec(),
                )],
            }
        }

        fn verify(&self, launcher: &Canned) -> Result<VerifyOutcome, VerifyError> {
            let no_env = |_: &str| None;
            verify_sandbox(
                &VerifyInputs {
                    settings: &self.settings,
                    state_dir: &self.state,
                    nonce: NONCE,
                    home: Path::new(HOME),
                    tmp_link_root: self.state.as_ref(),
                    config_dir: Path::new("/nonexistent-home/.config/relais"),
                    ledger_path: Path::new("/nonexistent-home/.local/state/relais/ledger.sqlite"),
                    env: &no_env,
                    base_env: &self.base_env,
                    capabilities: &self.capabilities,
                    platform: "macos",
                    managed: &self.managed,
                    verified_at: "2026-09-30T10:00:00Z",
                },
                &SystemGit,
                launcher,
            )
        }

        /// The key a later worker dispatch on this machine computes: its own
        /// scratch directory, and no probe fixture.
        fn dispatch_key(&self) -> VerificationKey {
            let no_env = |_: &str| None;
            let names = self.base_env.names();
            dispatch_key(&DispatchKeyInputs {
                harness_version: "2.1.285",
                platform: "macos",
                launch: &LaunchInputs {
                    settings: &self.settings,
                    home: Path::new(HOME),
                    config_dir: Path::new("/nonexistent-home/.config/relais"),
                    ledger_path: Path::new("/nonexistent-home/.local/state/relais/ledger.sqlite"),
                    env: &no_env,
                    launch_env_names: &names,
                    scratch: Path::new("/state/runs/run-1/attempts/1/scratch"),
                    tmp_link: Path::new("/tmp/rl-0a1b2c3d"),
                },
                managed: &self.managed,
            })
        }

        fn store(&self) -> VerificationStore {
            VerificationStore::load(&store_path(&self.state)).expect("a readable store")
        }
    }

    #[test]
    fn two_passing_probes_record_the_key_a_later_dispatch_computes() {
        let machine = Machine::new("verify-pass");
        let launcher = Canned::new(None);
        let outcome = machine.verify(&launcher).expect("verified");
        assert!(outcome.passed(), "{}", outcome.render());
        assert_eq!(outcome.cost, Some(MicroUsd::from_micros(40_000)));

        let key = machine.dispatch_key();
        assert_eq!(outcome.key, key, "the probe's key is the dispatch key");
        let record = machine.store().find(&key).cloned().expect("a record");
        assert_eq!(record.harness_version, "2.1.285");
        assert_eq!(record.platform, "macos");
        assert_eq!(record.verified_at, "2026-09-30T10:00:00Z");
        assert!(record.report.iter().any(|line| line == "✓ tmp-write: ok"));
        assert!(outcome.render().contains("verified: recorded under"));
        assert!(
            !machine.state.join("sandbox/probe").join(NONCE).exists(),
            "a pass cleans up after itself"
        );
    }

    #[test]
    fn a_sandbox_the_harness_ignored_records_nothing() {
        let machine = Machine::new("verify-fail");
        let launcher = Canned::new(Some("tmp-write"));
        let outcome = machine.verify(&launcher).expect("ran");
        assert!(!outcome.passed());
        let failed: Vec<_> = outcome
            .sandbox
            .steps
            .iter()
            .filter(|step| step.verdict != Verdict::Pass)
            .map(|step| step.id)
            .collect();
        assert_eq!(failed, ["tmp-write"]);
        assert!(outcome.allowlist.passed, "{}", outcome.allowlist.render());
        assert!(machine.store().find(&machine.dispatch_key()).is_none());
        assert!(!store_path(&machine.state).exists(), "no store was written");
        assert!(outcome.render().contains("NOT verified"));
        assert!(
            machine.state.join("sandbox/probe").join(NONCE).is_dir(),
            "a failure keeps its probe directory"
        );
    }

    #[test]
    fn a_failing_allowlist_probe_records_nothing_either() {
        let machine = Machine::new("verify-allowlist-fail");
        let launcher = Canned::new(Some("scrub-env"));
        let outcome = machine.verify(&launcher).expect("ran");
        assert!(outcome.sandbox.passed && !outcome.allowlist.passed);
        assert!(machine.store().find(&machine.dispatch_key()).is_none());
    }

    #[test]
    fn the_sandbox_probe_is_a_workers_launch_with_the_fixture_added_to_the_floor() {
        let machine = Machine::new("verify-launch");
        let launcher = Canned::new(None);
        machine.verify(&launcher).expect("verified");
        let launched = launcher.launched.lock().unwrap();
        assert_eq!(launched.len(), 2);

        let sandbox = &launched[0];
        assert_eq!(sandbox.model, "haiku");
        assert_eq!(sandbox.budget_micros, Some(500_000));
        assert!(sandbox.effort.is_none() && sandbox.max_turns.is_none());
        assert!(sandbox.disallowed_tools.is_empty() && sandbox.allowed_tools.is_empty());
        assert!(sandbox
            .work_dir
            .ends_with(format!("probe/{NONCE}/worktree")));
        assert!(sandbox.prompt.contains("Perform each numbered step"));

        let launch = sandbox.sandbox.as_ref().expect("sandboxed");
        let fixture = sandbox
            .work_dir
            .parent()
            .unwrap()
            .join("fixture/probe-credential");
        let fixture = fixture.to_string_lossy();
        let settings = &launch.settings;
        for key in ["denyRead", "denyWrite"] {
            let listed = settings["sandbox"]["filesystem"][key].to_string();
            assert!(listed.contains(&*fixture), "{key}: {listed}");
        }
        // The synthetic credential the probe launches with is denied like a
        // worker's own would be, although the base env does not carry it.
        let env_vars = settings["sandbox"]["credentials"]["envVars"].to_string();
        assert!(
            env_vars.contains(r#"{"mode":"deny","name":"AWS_SECRET_ACCESS_KEY"}"#)
                || env_vars.contains(r#"{"name":"AWS_SECRET_ACCESS_KEY","mode":"deny"}"#),
            "{env_vars}"
        );
        let allow_write = settings["sandbox"]["filesystem"]["allowWrite"].to_string();
        assert!(
            allow_write.contains("/opt/out"),
            "the machine's [sandbox]: {allow_write}"
        );
        assert!(
            launch
                .scratch_dir
                .ends_with(format!("probe/{NONCE}/scratch")),
            "{:?}",
            launch.scratch_dir
        );

        let var = |spec: &LaunchSpec, name: &str| {
            spec.env
                .vars()
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            var(sandbox, "AWS_SECRET_ACCESS_KEY").as_deref(),
            Some("relais-probe-0123456789abcdef")
        );
        assert_eq!(
            var(sandbox, CLAUDE_TMPDIR).as_deref(),
            Some(&*launch.tmp_link.to_string_lossy())
        );
        assert_eq!(
            var(sandbox, "TMPDIR"),
            None,
            "the base env has none, and the launch adds none"
        );
        // Suffix, not equality: the scratch is removed once the probe passes,
        // and the launch-time path is canonical (`/private/tmp` for `/tmp`).
        let resolved = launcher.tmpdir_at_launch.lock().unwrap().clone();
        assert!(
            resolved.is_some_and(|path| path.ends_with(format!("probe/{NONCE}/scratch"))),
            "the link resolved to the scratch while the probe ran"
        );
        assert!(
            std::fs::symlink_metadata(&launch.tmp_link).is_err(),
            "and is gone after it"
        );
        assert_eq!(var(sandbox, "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"), None);
        assert!(
            var(sandbox, "ANTHROPIC_API_KEY").is_some(),
            "the worker env, whole"
        );

        let allowlist = &launched[1];
        assert!(allowlist.sandbox.is_none());
        assert_eq!(
            var(allowlist, "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB").as_deref(),
            Some("1")
        );
        assert_eq!(
            var(allowlist, "AWS_SECRET_ACCESS_KEY").as_deref(),
            Some("relais-probe-0123456789abcdef")
        );
        assert_eq!(allowlist.model, "haiku");
        assert_eq!(allowlist.budget_micros, Some(500_000));
    }

    #[test]
    fn the_probe_worktree_is_a_git_checkout_with_the_settings_planted_and_the_fixture_present() {
        let machine = Machine::new("verify-prepared");
        let launcher = Canned::new(Some("tmp-write"));
        machine.verify(&launcher).expect("ran");
        let dir = machine.state.join("sandbox/probe").join(NONCE);

        let worktree = dir.join("worktree");
        assert!(
            worktree.join(".git").is_file(),
            "a `git worktree add` checkout"
        );
        let planted = std::fs::read_to_string(worktree.join(".claude/settings.json")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&planted).unwrap(),
            json!({"sandbox": {"excludedCommands": ["python3"]}})
        );
        assert_eq!(
            launcher.fixture_at_launch.lock().unwrap().as_deref(),
            Some("relais-probe-0123456789abcdef\n"),
            "the fixture exists, with the nonce, before the launch"
        );
        assert!(dir.join("scratch").is_dir());
        let log = SystemGit
            .run(&dir.join("repo"), &["log", "--format=%s"])
            .expect("a log");
        assert_eq!(log.trim(), "probe", "one commit");
    }

    #[test]
    fn a_launch_that_cannot_run_is_an_error_and_records_nothing() {
        struct Broken;
        impl ProbeLauncher for Broken {
            fn stream(&self, _spec: &LaunchSpec) -> Result<String, BackendError> {
                Err(BackendError::Launch("no such file".to_string()))
            }
        }
        let machine = Machine::new("verify-broken");
        let no_env = |_: &str| None;
        let err = verify_sandbox(
            &VerifyInputs {
                settings: &machine.settings,
                state_dir: &machine.state,
                nonce: NONCE,
                home: Path::new(HOME),
                tmp_link_root: &machine.state,
                config_dir: Path::new("/c"),
                ledger_path: Path::new("/l"),
                env: &no_env,
                base_env: &machine.base_env,
                capabilities: &machine.capabilities,
                platform: "macos",
                managed: &machine.managed,
                verified_at: "t",
            },
            &SystemGit,
            &Broken,
        )
        .expect_err("cannot launch");
        assert!(
            matches!(
                err,
                VerifyError::Launch {
                    probe: "sandbox",
                    ..
                }
            ),
            "{err}"
        );
        assert!(!store_path(&machine.state).exists());
    }

    #[test]
    fn an_unknown_harness_version_has_no_key() {
        let mut machine = Machine::new("verify-no-version");
        machine.capabilities.version = None;
        let err = machine.verify(&Canned::new(None)).expect_err("no version");
        assert!(matches!(err, VerifyError::Harness));
    }

    #[test]
    fn a_corrupt_store_is_an_error_before_anything_launches() {
        let machine = Machine::new("verify-corrupt-store");
        let path = store_path(&machine.state);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not a store").unwrap();
        let launcher = Canned::new(None);
        let err = machine.verify(&launcher).expect_err("corrupt store");
        assert!(
            matches!(err, VerifyError::Store(StoreError::Corrupt(_))),
            "{err}"
        );
        assert!(launcher.launched.lock().unwrap().is_empty(), "no launch");
        assert!(
            !machine.state.join("sandbox/probe").exists(),
            "no probe directory was prepared"
        );
    }

    /// A record another verification writes while this one's paid sessions
    /// run is kept: the store is loaded again just before the save.
    #[test]
    fn a_record_written_meanwhile_survives_the_save() {
        let machine = Machine::new("verify-meanwhile");
        let launcher = Canned::new(None);
        let path = store_path(&machine.state);
        *launcher.meanwhile.lock().unwrap() = Some(Box::new(move || {
            let mut other = VerificationStore::load(&path).expect("loads");
            other.record(VerificationRecord {
                key: "other-key".to_string(),
                verified_at: "2026-09-30T09:00:00Z".to_string(),
                harness_version: "2.1.285".to_string(),
                platform: "linux".to_string(),
                report: Vec::new(),
            });
            other.save().expect("saves");
        }));
        let outcome = machine.verify(&launcher).expect("verified");
        assert!(outcome.passed(), "{}", outcome.render());
        let store = machine.store();
        assert!(store.find(&machine.dispatch_key()).is_some(), "this record");
        assert!(
            store
                .records()
                .iter()
                .any(|record| record.key == "other-key"),
            "the other verification's record survived"
        );
    }

    #[test]
    fn only_the_three_most_recent_probe_dirs_survive_a_new_attempt() {
        let machine = Machine::new("verify-prune");
        let root = machine.state.join("sandbox/probe");
        let epoch = std::time::SystemTime::UNIX_EPOCH;
        for (index, name) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::File::open(&dir)
                .unwrap()
                .set_modified(epoch + Duration::from_secs(1_000 * (index as u64 + 1)))
                .unwrap();
        }
        machine
            .verify(&Canned::new(Some("tmp-write")))
            .expect("ran");
        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [NONCE, "c", "d", "e"],
            "the newest three, and this attempt's"
        );
    }

    #[test]
    fn the_session_cost_is_read_from_the_terminal_record() {
        assert_eq!(
            session_cost("{\"type\":\"system\"}\n{\"type\":\"result\",\"total_cost_usd\":0.125}"),
            Some(MicroUsd::from_micros(125_000))
        );
        assert_eq!(session_cost("{\"type\":\"result\"}"), None);
        assert_eq!(session_cost("not json"), None);
    }
}
