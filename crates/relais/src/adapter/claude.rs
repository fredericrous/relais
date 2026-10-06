//! The mandatory Claude Code adapter (SPEC §8, §15).
//!
//! Launches `claude -p` with explicit model, effort, turn limits and
//! budget controls, prompt via stdin, arguments as an argv array, output
//! as structured JSON. Every flag is capability-checked against the
//! installed version's `--help` output before use — nothing is assumed
//! from documentation, and the installed CLI is probed once per process,
//! not once per launch. The result JSON is parsed leniently: absent
//! fields are unknown, never zero — including the model, which stays
//! UNREPORTED when the harness names none, because reading silence as
//! agreement is what made an unapproved substitution undetectable
//! (SPEC §6).
//!
//! The worker's environment is the one `LaunchSpec::env` names and
//! nothing else: the process starts from a cleared environment, so an
//! ambient `GIT_DIR` or `GIT_INDEX_FILE` from a rebase shell cannot
//! point its commits at the user's repository.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;
use std::time::Duration;

use crate::backend::{
    claims_blockage, Backend, BackendError, Capabilities, Cost, LaunchResult, LaunchSpec,
    PermissionDenial, PermissionEnforcement, ProbeLauncher, SandboxCapability, SandboxLaunch,
    ToolSet, UsageReport,
};
use crate::money::MicroUsd;
use crate::procs::{run_with_timeout, Ended, ProcessEnd};
use crate::tooling::PROBE_TIMEOUT;

/// Why a capability probe produced no capabilities.
///
/// A probe that could not be run is not the same fact as a CLI that ran
/// and does not support a flag, and reading the first as the second is
/// how an unavailable harness reads as an unsupported one. Every
/// variant blocks the launch; which it was is what `doctor` prints
/// (`errors.never-swallowed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// The probe process could not be started, timed out, or was
    /// cancelled.
    NotRun { args: String, detail: String },
    /// It ran and did not exit 0.
    Refused { args: String, ended: String },
    /// It exited 0 with nothing on either stream.
    Silent { args: String },
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRun { args, detail } => {
                write!(f, "`claude {args}` did not run: {detail}")
            }
            Self::Refused { args, ended } => write!(f, "`claude {args}` {ended}"),
            Self::Silent { args } => {
                write!(f, "`claude {args}` exited 0 and printed nothing")
            }
        }
    }
}

impl std::error::Error for ProbeFailure {}

#[derive(Debug)]
pub struct ClaudeBackend {
    binary: PathBuf,
    /// Probed on first use and kept: `--version` and `--help` are facts
    /// about an installed CLI, and asking again before every dispatch
    /// spent two processes per launch to learn the same thing (audit V6).
    /// The failure is kept too, so a probe that errored is reported as
    /// an error rather than as a CLI without the flags.
    capabilities: OnceLock<Result<Capabilities, ProbeFailure>>,
}

impl ClaudeBackend {
    /// Discovery: `RELAIS_CLAUDE_BIN` wins, then PATH lookup. Missing
    /// binary is a blocked outcome, never a fallback (SPEC §6).
    pub fn discover() -> Result<Self, BackendError> {
        if let Some(explicit) = std::env::var_os("RELAIS_CLAUDE_BIN") {
            return Self::with_binary(PathBuf::from(explicit));
        }
        let found = crate::tooling::which("claude")
            .ok_or_else(|| BackendError::MissingBinary("claude".into()))?;
        Self::with_binary(found)
    }

    /// A backend at a named binary, resolved to an absolute path.
    ///
    /// The path is canonicalised because the launch runs with the task
    /// worktree as its working directory: a relative one is probed
    /// against relais's cwd and executed against the worker's, so a
    /// repository that ships `node_modules/.bin/claude` could have its
    /// own binary run in place of the operator's (audit V13). A relative
    /// `RELAIS_CLAUDE_BIN` is refused outright rather than resolved,
    /// since nothing can say which directory the operator meant.
    pub fn with_binary(binary: PathBuf) -> Result<Self, BackendError> {
        if binary.is_relative() {
            return Err(BackendError::MissingBinary(format!(
                "{}: the Claude Code binary must be named by an absolute path — a relative one \
                 resolves against relais's working directory and runs against the worker's",
                binary.display()
            )));
        }
        let resolved = binary
            .canonicalize()
            .map_err(|e| BackendError::MissingBinary(format!("{}: {e}", binary.display())))?;
        if !resolved.is_file() {
            return Err(BackendError::MissingBinary(
                resolved.to_string_lossy().into(),
            ));
        }
        Ok(Self {
            binary: resolved,
            capabilities: OnceLock::new(),
        })
    }

    /// The binary this backend launches.
    pub fn binary(&self) -> &Path {
        &self.binary
    }

    /// The probe, cached, with the reason it failed when it did.
    /// `cancel` aborts a probe that hangs; the timeout bounds one that
    /// merely takes its time.
    fn capability_report(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> &Result<Capabilities, ProbeFailure> {
        self.capabilities.get_or_init(|| self.probe_now(cancel))
    }

    /// What the installed CLI can be asked to do, or why that could not
    /// be established — the answer `doctor` reports.
    pub fn probe_report(&self) -> Result<Capabilities, ProbeFailure> {
        self.capability_report(None).clone()
    }

    fn capabilities(&self, cancel: Option<&AtomicBool>) -> Option<Capabilities> {
        // The failure is not dropped here: `launch` re-reads the report
        // below to put it in the `BackendError`, and `probe_report` is
        // what `doctor` prints. This is the `Backend::probe` shape.
        self.capability_report(cancel).clone().ok()
    }

    fn probe_now(&self, cancel: Option<&AtomicBool>) -> Result<Capabilities, ProbeFailure> {
        let version = self.ask(&["--version"], cancel)?;
        // A CLI that answers `--version` but not `--help` cannot have its
        // flags checked, and a launch may not assume them: no
        // capabilities means no backend, which blocks (SPEC §6).
        let help = self.ask(&["--help"], cancel)?;
        Ok(capabilities_from_help(
            version.trim().to_string(),
            help.trim(),
        ))
    }

    /// One probe call: bounded, cancellable, and an error unless the CLI
    /// exited successfully with something to say.
    fn ask(&self, args: &[&str], cancel: Option<&AtomicBool>) -> Result<String, ProbeFailure> {
        let mut command = Command::new(&self.binary);
        command.args(args);
        let named = args.join(" ");
        let end = run_with_timeout(command, PROBE_TIMEOUT, None, cancel, None).map_err(|e| {
            ProbeFailure::NotRun {
                args: named.clone(),
                detail: e.to_string(),
            }
        })?;
        if end.ended != Ended::Exited(0) {
            return Err(ProbeFailure::Refused {
                args: named,
                ended: end.ended.describe(),
            });
        }
        let answer = format!("{}{}", end.stdout, end.stderr);
        if answer.trim().is_empty() {
            return Err(ProbeFailure::Silent { args: named });
        }
        Ok(answer)
    }

    /// The capabilities a launch is checked against; a probe that could
    /// not run means no launch.
    fn launch_capabilities(&self, spec: &LaunchSpec) -> Result<Capabilities, BackendError> {
        self.capability_report(spec.cancel.as_deref())
            .clone()
            .map_err(|failure| {
                BackendError::MissingBinary(format!(
                    "{}: its launch controls cannot be checked — {failure} (probes are bounded \
                     at {PROBE_TIMEOUT:?})",
                    self.binary.display()
                ))
            })
    }

    /// Runs `argv` for `spec`: prompt on stdin, the spec's worktree as
    /// working directory and its environment, entire.
    fn run_argv(&self, spec: &LaunchSpec, argv: &[String]) -> Result<ProcessEnd, BackendError> {
        let mut command = Command::new(&self.binary);
        command
            .args(argv)
            .current_dir(&spec.work_dir)
            // The worker's environment is the spec's, entire. Clearing
            // first is what removes the ambient `GIT_*` variables that
            // override a working directory, the machine authority a
            // nested `relais` would read, and every credential the task
            // was never given (SPEC §8, audit V4).
            .env_clear();
        for (name, value) in spec.env.vars() {
            command.env(name, value);
        }
        let wall = spec.wall_timeout.max(Duration::from_secs(1));
        Ok(run_with_timeout(
            command,
            wall,
            Some(spec.prompt.clone().into_bytes()),
            spec.cancel.as_deref(),
            spec.pid_slot.as_deref(),
        )?)
    }
}

impl ProbeLauncher for ClaudeBackend {
    fn stream(&self, spec: &LaunchSpec) -> Result<String, BackendError> {
        let caps = self.launch_capabilities(spec)?;
        let argv = probe_argv(spec, &caps)?;
        let end = self.run_argv(spec, &argv)?;
        // What the session did is in its stdout whatever way it ended (a
        // budget stop still leaves the transcript); with none, the way it
        // ended is the only evidence there is.
        if end.stdout.trim().is_empty() {
            return Err(BackendError::Launch(failure_detail(
                &end,
                &parse_result_json(&end.stdout),
            )));
        }
        Ok(end.stdout)
    }
}

impl Backend for ClaudeBackend {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn probe(&self) -> Option<Capabilities> {
        self.capabilities(None)
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let caps = self.launch_capabilities(spec)?;
        let argv = build_argv(spec, &caps)?;
        let end = self.run_argv(spec, &argv)?;

        let parsed = parse_result_json(&end.stdout);
        let worker_claims_blockage = parsed.result_text.as_deref().is_some_and(claims_blockage);
        // A harness that exited non-zero, reported an error, or printed
        // something the adapter cannot read did not complete an attempt:
        // the result is missing, never an empty candidate (SPEC §9).
        let completed = end.ended.succeeded();
        let usable = completed && parsed.result_text.is_some() && !parsed.is_error;
        let failure_detail = if usable {
            None
        } else {
            Some(failure_detail(&end, &parsed))
        };
        Ok(LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            ended: end.ended,
            stdout: end.stdout,
            stderr: end.stderr,
            result_text: if usable { parsed.result_text } else { None },
            session_id: parsed.session_id,
            effective_model: parsed.effective_model,
            usage: parsed.usage,
            worker_claims_blockage,
            permission_denials: parsed.permission_denials,
            failure_detail,
            booked_message_ids: Vec::new(),
        })
    }
}

/// What the installed CLI advertises, read from its `--help`. Flag
/// spellings are the ones Claude Code 2.x prints; a harness that spells
/// one differently is a harness without that capability, and the launch
/// leaves the flag out (or fails closed, for the deny list and the
/// allowlist, which are the controls that bound the worker).
pub fn capabilities_from_help(version: String, help: &str) -> Capabilities {
    let supports = |flag: &str| help.contains(flag);
    Capabilities {
        backend: "claude-code".into(),
        version: Some(version),
        supports_model: supports("--model"),
        accepted_efforts: crate::catalog::parse_cli_efforts(help),
        supports_max_turns: supports("--max-turns"),
        supports_output_format_json: supports("--output-format"),
        supports_budget: supports("--max-budget-usd"),
        supports_disallowed_tools: supports("--disallowed-tools"),
        supports_settings: supports("--settings"),
        // Permission lists are enforced by the harness itself; the
        // adapter reports observed enforcement until the
        // compatibility matrix verifies it per version.
        permission_enforcement: PermissionEnforcement::Observed,
        sandbox: SandboxCapability::WorktreeOnly,
    }
}

/// The argv for one launch, as a pure function of the spec and the
/// probed capabilities. No permission-bypass flag exists here, and none
/// may be added (SPEC §8): missing permissions produce a blocked result.
pub fn build_argv(spec: &LaunchSpec, caps: &Capabilities) -> Result<Vec<String>, BackendError> {
    if !caps.supports_model {
        return Err(BackendError::Unsupported("explicit model selection"));
    }
    if !spec.disallowed_tools.is_empty() && !caps.supports_disallowed_tools {
        return Err(BackendError::Unsupported("a tool deny list"));
    }
    if !spec.allowed_tools.is_empty() && !caps.supports_settings {
        return Err(BackendError::Unsupported("a permission allowlist"));
    }
    if spec.sandbox.is_some() && !caps.supports_settings {
        return Err(BackendError::Unsupported("sandbox settings"));
    }

    let mut argv: Vec<String> = vec!["-p".into(), "--model".into(), spec.model.clone()];
    // An effort is passed on or refused, never dropped: when the CLI fact
    // is `Unknown` the configured effort goes through (the compatibility
    // carve-out), and when the CLI is known to lack it the run is blocked.
    if let Some(effort) = &spec.effort {
        crate::backend::check_effort(
            &caps.accepted_efforts,
            effort,
            &spec.model,
            caps.version.as_deref(),
        )?;
        argv.push("--effort".into());
        argv.push(effort.as_str().to_string());
    }
    // A turn ceiling only reaches the harness when the harness has a flag
    // for it; Claude Code 2.1.x has none. The capability is reported
    // (`Capabilities::turn_ceiling`) so nothing downstream claims a
    // ceiling that was never applied — SPEC §11's turn ceiling is not
    // deliverable on this harness, and the context manifest says so.
    if let Some(max_turns) = spec.max_turns {
        if caps.supports_max_turns {
            argv.push("--max-turns".into());
            argv.push(max_turns.to_string());
        }
    }
    if caps.supports_output_format_json {
        argv.push("--output-format".into());
        argv.push("json".into());
    }
    if let Some(budget) = spec.budget_micros {
        if caps.supports_budget {
            argv.push("--max-budget-usd".into());
            argv.push(budget_dollars(budget));
        }
    }
    // One flag per rule: the CLI's variadic parser accumulates them
    // (verified on 2.1.278), and a rule is never split on whitespace.
    for tool in &spec.disallowed_tools {
        argv.push("--disallowed-tools".into());
        argv.push(tool.clone());
    }
    match &spec.sandbox {
        Some(launch) => argv.extend(sandbox_args(launch, sandbox_tools(spec.tools))),
        None => {
            match spec.tools {
                ToolSet::ReadOnly => {
                    argv.push("--tools".into());
                    argv.push(READ_ONLY_TOOLS.into());
                }
                // Allowlist mode has no tool flag of its own: the
                // permission rules below decide.
                ToolSet::ModeDefault => {}
            }
            if !spec.allowed_tools.is_empty() {
                argv.push("--settings".into());
                argv.push(
                    serde_json::json!({ "permissions": { "allow": spec.allowed_tools } })
                        .to_string(),
                );
            }
        }
    }
    Ok(argv)
}

/// [`build_argv`] for a probe session: the worker's argv with ONE
/// difference, `--output-format stream-json --verbose` in place of
/// `--output-format json`. The single JSON result of a worker's launch
/// carries neither the `system`/`init` record (tools, MCP servers, plugins)
/// nor the tool calls and results the probe is judged on; the stream does.
pub fn probe_argv(spec: &LaunchSpec, caps: &Capabilities) -> Result<Vec<String>, BackendError> {
    let mut argv = build_argv(spec, caps)?;
    let Some(at) = argv.iter().position(|arg| arg == "--output-format") else {
        return Err(BackendError::Unsupported("streamed output"));
    };
    // `build_argv` pushes the flag and its value together.
    argv[at + 1] = "stream-json".into();
    argv.insert(at + 2, "--verbose".into());
    Ok(argv)
}

/// The tools a sandboxed worker has. `MultiEdit` is not a tool on the
/// measured harness (S0), so it is not listed.
const SANDBOX_TOOLS: &str = "Bash,Read,Edit,Write,Grep,Glob";

/// What a launch that can only look asks for, in either mode.
const READ_ONLY_TOOLS: &str = "Read,Grep,Glob";

/// The `--tools` value a launch in sandbox mode passes: what it asked for,
/// or the sandbox worker's own set.
fn sandbox_tools(asked: ToolSet) -> &'static str {
    match asked {
        ToolSet::ModeDefault => SANDBOX_TOOLS,
        ToolSet::ReadOnly => READ_ONLY_TOOLS,
    }
}

/// What a sandboxed launch adds: no user, project or local settings,
/// hooks or plugins (`--restricted`), no MCP servers but the ones named
/// here (none), the tools above, the scratch directory, and ONE
/// `--settings` document that replaces the allowlist-only one.
fn sandbox_args(launch: &SandboxLaunch, tools: &str) -> Vec<String> {
    vec![
        "--restricted".into(),
        "--tools".into(),
        tools.into(),
        "--strict-mcp-config".into(),
        "--add-dir".into(),
        launch.scratch_dir.to_string_lossy().into_owned(),
        "--settings".into(),
        launch.settings.to_string(),
    ]
}

/// Micro-USD as the plain decimal `--max-budget-usd` takes; never a
/// float on the way there.
fn budget_dollars(micros: i64) -> String {
    let micros = micros.max(0);
    let whole = micros / 1_000_000;
    let frac = micros % 1_000_000;
    if frac == 0 {
        whole.to_string()
    } else {
        let digits = format!("{frac:06}");
        format!("{whole}.{}", digits.trim_end_matches('0'))
    }
}

fn failure_detail(end: &ProcessEnd, parsed: &ParsedClaudeResult) -> String {
    let mut parts = vec![end.ended.describe()];
    if parsed.is_error {
        parts.push("the harness reported an error".to_string());
    }
    // What the supervision itself found: a descendant that outlived the
    // worker, or output that never finished arriving, both explain an
    // unreadable result (audit V1).
    if let crate::procs::GroupKill::Refused { .. } = &end.group {
        parts.push(end.group.describe());
    }
    if let crate::procs::Captured::Partial { detail } = &end.captured {
        parts.push(detail.clone());
    }
    if let Some(text) = parsed
        .result_text
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        parts.push(head(text));
    }
    let stderr = end.stderr.trim();
    if !stderr.is_empty() {
        parts.push(format!("stderr: {}", head(stderr)));
    } else if parsed.result_text.is_none() {
        let stdout = end.stdout.trim();
        if !stdout.is_empty() {
            parts.push(format!("stdout: {}", head(stdout)));
        }
    }
    parts.join("; ")
}

fn head(text: &str) -> String {
    let mut out: String = text.chars().take(400).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

/// Lenient result parsing. Every field is optional in the wild; absent
/// usage stays unknown (SPEC §11) and an unreadable payload keeps the raw
/// stdout as evidence instead of pretending it was empty.
pub struct ParsedClaudeResult {
    pub result_text: Option<String>,
    pub session_id: Option<String>,
    /// The model the harness named, and `None` when it named none. Not
    /// the requested model: a harness that reports nothing has not
    /// confirmed anything, and echoing the request back made
    /// `verify_model` structurally unable to see a substitution
    /// (audit V3).
    pub effective_model: Option<String>,
    pub usage: UsageReport,
    /// `is_error` as the harness reported it.
    pub is_error: bool,
    /// The harness's `permission_denials`, in order, one per refused call.
    pub permission_denials: Vec<PermissionDenial>,
}

pub fn parse_result_json(stdout: &str) -> ParsedClaudeResult {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return ParsedClaudeResult {
            result_text: None,
            session_id: None,
            effective_model: None,
            usage: UsageReport::unknown(),
            is_error: false,
            permission_denials: Vec::new(),
        };
    };
    let get_i64 = |path: &[&str]| -> Option<i64> {
        let mut current = &value;
        for key in path {
            current = current.get(*key)?;
        }
        current.as_i64()
    };
    // Claude Code's `total_cost_usd` is the session total, subagents
    // included; when any were spawned the figure is an inclusive parent
    // total and must not be summed with its descendants (SPEC §11).
    let spawned_subagents = get_i64(&["subagent_stats", "spawned"]).unwrap_or(0) > 0;
    let usage = UsageReport {
        input_tokens: get_i64(&["usage", "input_tokens"]),
        output_tokens: get_i64(&["usage", "output_tokens"]),
        cache_read_tokens: get_i64(&["usage", "cache_read_input_tokens"]),
        cache_write_tokens: get_i64(&["usage", "cache_creation_input_tokens"]),
        cost: match value
            .get("total_cost_usd")
            .and_then(|cost| cost.as_f64())
            .map(MicroUsd::from_dollars)
        {
            Some(micros) => Cost::Reported {
                micros,
                inclusive: spawned_subagents,
            },
            None => Cost::Unknown,
        },
    };
    let permission_denials = value
        .get("permission_denials")
        .and_then(|denials| denials.as_array())
        .map(|denials| {
            let mut refused: Vec<PermissionDenial> = Vec::new();
            for denial in denials {
                let name = denial
                    .get("tool_name")
                    .and_then(|name| name.as_str())
                    .unwrap_or("unknown tool");
                // A Bash denial is only actionable with the command: the
                // allowlist rule to add is `Bash(<command>:*)`.
                let entry = match denial
                    .get("tool_input")
                    .and_then(|input| input.get("command"))
                    .and_then(|command| command.as_str())
                {
                    Some(command) if name == "Bash" => {
                        format!("Bash({})", command.chars().take(120).collect::<String>())
                    }
                    _ => name.to_string(),
                };
                let tool_use_id = denial.get("tool_use_id").and_then(|id| id.as_str());
                refused.push(PermissionDenial::new(entry, tool_use_id));
            }
            refused
        })
        .unwrap_or_default();
    ParsedClaudeResult {
        result_text: value
            .get("result")
            .and_then(|result| result.as_str())
            .map(|text| text.to_string()),
        session_id: value
            .get("session_id")
            .and_then(|id| id.as_str())
            .map(|id| id.to_string()),
        effective_model: effective_model(&value),
        usage,
        is_error: value
            .get("is_error")
            .and_then(|flag| flag.as_bool())
            .unwrap_or(false),
        permission_denials,
    }
}

/// The model that actually ran. Claude Code 2.x reports it only under
/// `modelUsage`, keyed by the dated ID (with `canonicalModel` beside it);
/// older shapes carried a top-level `model`. With several models in one
/// session the one that answered the most output tokens is the worker's;
/// the others are subagents, observed through `inclusive` usage.
fn effective_model(value: &serde_json::Value) -> Option<String> {
    if let Some(model) = value
        .get("model")
        .or_else(|| value.get("actual_model"))
        .and_then(|model| model.as_str())
    {
        return Some(model.to_string());
    }
    let usage = value.get("modelUsage")?.as_object()?;
    usage
        .iter()
        .max_by_key(|(_, stats)| {
            stats
                .get("outputTokens")
                .and_then(|tokens| tokens.as_i64())
                .unwrap_or(0)
        })
        .map(|(id, stats)| {
            stats
                .get("canonicalModel")
                .and_then(|model| model.as_str())
                .unwrap_or(id)
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::CostCompleteness;

    /// A result document as Claude Code 2.1.278 prints it (trimmed).
    const SAMPLE: &str = r#"{
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "done: fixed the escaping",
        "session_id": "sess-abc",
        "total_cost_usd": 0.0123,
        "usage": {
            "input_tokens": 1200,
            "output_tokens": 300,
            "cache_read_input_tokens": 800,
            "cache_creation_input_tokens": 50
        },
        "modelUsage": {
            "claude-sonnet-5-20261001": {
                "inputTokens": 1200, "outputTokens": 300,
                "costUSD": 0.0123, "canonicalModel": "claude-sonnet-5"
            }
        },
        "permission_denials": [],
        "subagent_stats": {"spawned": 0}
    }"#;

    /// The real `--help` fragment of 2.1.278: `--max-budget-usd`, no
    /// `--max-turns`, no `--budget`.
    const HELP_2_1: &str = "  --max-budget-usd <amount>  Maximum dollar amount\n  \
        --model <model>  Model\n  --effort <level>  Effort\n  \
        --output-format <format>  Output\n  --disallowedTools, --disallowed-tools <tools...>\n  \
        --settings <file-or-json>  Path\n";

    fn spec(budget: Option<i64>) -> LaunchSpec {
        LaunchSpec {
            dispatch_id: "disp-1".into(),
            prompt: "do it".into(),
            model: "sonnet".into(),
            effort: crate::policy::EffortId::parse("medium").ok(),
            max_turns: Some(12),
            budget_micros: budget,
            disallowed_tools: vec!["Bash(git push:*)".into()],
            allowed_tools: vec!["Edit".into(), "Bash(cargo test:*)".into()],
            work_dir: PathBuf::from("."),
            env: crate::backend::LaunchEnv::from_ambient(&[(
                "PATH".to_string(),
                "/usr/bin".to_string(),
            )]),
            wall_timeout: Duration::from_secs(10),
            cancel: None,
            pid_slot: None,
            sandbox: None,
            tools: crate::backend::ToolSet::ModeDefault,
            presentation: crate::backend::Presentation::Headless,
        }
    }

    fn sandboxed_spec() -> LaunchSpec {
        LaunchSpec {
            sandbox: Some(SandboxLaunch {
                settings: serde_json::json!({"sandbox": {"enabled": true}}),
                scratch_dir: PathBuf::from("/state/runs/r/attempts/1/scratch"),
                tmp_link: PathBuf::from("/tmp/rl-0a1b2c3d"),
            }),
            ..spec(Some(500_000))
        }
    }

    fn count(argv: &[String], flag: &str) -> usize {
        argv.iter().filter(|arg| *arg == flag).count()
    }

    fn value_after<'a>(argv: &'a [String], flag: &str) -> &'a str {
        let at = argv.iter().position(|arg| arg == flag).expect(flag);
        &argv[at + 1]
    }

    #[test]
    fn a_read_only_launch_asks_for_exactly_three_tools_in_both_modes() {
        let caps = capabilities_from_help("2.1.278".into(), HELP_2_1);
        let read_only = |spec: LaunchSpec| LaunchSpec {
            tools: ToolSet::ReadOnly,
            allowed_tools: Vec::new(),
            ..spec
        };
        for spec in [read_only(spec(Some(500_000))), read_only(sandboxed_spec())] {
            let argv = build_argv(&spec, &caps).expect("argv");
            assert_eq!(count(&argv, "--tools"), 1, "{argv:?}");
            assert_eq!(value_after(&argv, "--tools"), "Read,Grep,Glob");
        }
        // The launches that never asked keep what they always had.
        let worker = build_argv(&spec(Some(500_000)), &caps).expect("argv");
        assert_eq!(count(&worker, "--tools"), 0, "{worker:?}");
        let sandboxed = build_argv(&sandboxed_spec(), &caps).expect("argv");
        assert_eq!(
            value_after(&sandboxed, "--tools"),
            "Bash,Read,Edit,Write,Grep,Glob"
        );
    }

    #[test]
    fn the_allowlist_argv_is_what_it_always_was() {
        let caps = capabilities_from_help("2.1.278".into(), HELP_2_1);
        let argv = build_argv(&spec(Some(500_000)), &caps).expect("argv");
        assert_eq!(
            argv,
            [
                "-p",
                "--model",
                "sonnet",
                "--effort",
                "medium",
                "--output-format",
                "json",
                "--max-budget-usd",
                "0.5",
                "--disallowed-tools",
                "Bash(git push:*)",
                "--settings",
                r#"{"permissions":{"allow":["Edit","Bash(cargo test:*)"]}}"#,
            ]
        );
    }

    #[test]
    fn the_sandbox_argv_carries_each_flag_once_and_the_exact_settings() {
        let caps = capabilities_from_help("2.1.285".into(), HELP_2_1);
        let argv = build_argv(&sandboxed_spec(), &caps).expect("argv");
        for flag in [
            "--restricted",
            "--tools",
            "--strict-mcp-config",
            "--add-dir",
            "--settings",
        ] {
            assert_eq!(count(&argv, flag), 1, "{flag} in {argv:?}");
        }
        assert_eq!(
            value_after(&argv, "--tools"),
            "Bash,Read,Edit,Write,Grep,Glob"
        );
        assert_eq!(
            value_after(&argv, "--add-dir"),
            "/state/runs/r/attempts/1/scratch"
        );
        assert_eq!(
            value_after(&argv, "--settings"),
            r#"{"sandbox":{"enabled":true}}"#,
            "the allowlist-only settings are replaced, not joined"
        );
        assert!(
            !argv.iter().any(|arg| arg.contains("Bash(cargo test")),
            "no permissions.allow Bash rule rides along"
        );
    }

    #[test]
    fn the_probe_argv_differs_from_the_workers_only_in_the_output_format() {
        let caps = capabilities_from_help("2.1.285".into(), HELP_2_1);
        for spec in [sandboxed_spec(), spec(Some(500_000))] {
            let worker = build_argv(&spec, &caps).expect("argv");
            let probe = probe_argv(&spec, &caps).expect("argv");
            let streamed: Vec<String> = probe
                .iter()
                .filter(|arg| *arg != "--verbose")
                .map(|arg| {
                    if arg == "stream-json" {
                        "json".to_string()
                    } else {
                        arg.clone()
                    }
                })
                .collect();
            assert_eq!(streamed, worker);
            assert_eq!(count(&probe, "--verbose"), 1);
            assert_eq!(value_after(&probe, "--output-format"), "stream-json");
        }
        let mut no_format = caps.clone();
        no_format.supports_output_format_json = false;
        assert!(matches!(
            probe_argv(&sandboxed_spec(), &no_format),
            Err(BackendError::Unsupported(_))
        ));
    }

    #[test]
    fn the_deny_floor_is_the_same_in_both_modes() {
        let caps = capabilities_from_help("2.1.285".into(), HELP_2_1);
        let plain = build_argv(&spec(Some(500_000)), &caps).expect("argv");
        let sandboxed = build_argv(&sandboxed_spec(), &caps).expect("argv");
        for argv in [&plain, &sandboxed] {
            assert_eq!(count(argv, "--disallowed-tools"), 1);
            assert_eq!(value_after(argv, "--disallowed-tools"), "Bash(git push:*)");
        }
    }

    #[test]
    fn parses_documented_result_fields() {
        let parsed = parse_result_json(SAMPLE);
        assert_eq!(
            parsed.result_text.as_deref(),
            Some("done: fixed the escaping")
        );
        assert_eq!(parsed.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(parsed.effective_model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(parsed.usage.input_tokens, Some(1200));
        assert_eq!(parsed.usage.output_tokens, Some(300));
        assert_eq!(parsed.usage.cache_read_tokens, Some(800));
        assert_eq!(
            parsed.usage.cost,
            Cost::Reported {
                micros: MicroUsd::from_micros(12_300),
                inclusive: false,
            }
        );
        assert_eq!(parsed.usage.cost.completeness(), CostCompleteness::Actual);
        assert!(!parsed.is_error);
        assert!(parsed.permission_denials.is_empty());
    }

    #[test]
    fn absent_usage_stays_unknown_never_zero() {
        let parsed = parse_result_json(r#"{"result": "no usage here"}"#);
        assert_eq!(parsed.usage.input_tokens, None);
        assert_eq!(parsed.usage.cost, Cost::Unknown);
        assert_eq!(parsed.usage.cost.micros(), None);
        assert!(!parsed.usage.cost.inclusive());
    }

    /// V3: output the adapter cannot read says nothing about which model
    /// ran, and must not be read as confirmation that it was the one
    /// asked for.
    #[test]
    fn unreadable_output_leaves_the_model_unreported_and_the_usage_unknown() {
        let parsed = parse_result_json("not json");
        assert_eq!(parsed.result_text, None);
        assert_eq!(parsed.effective_model, None);
        assert_eq!(parsed.usage.cost, Cost::Unknown);
        assert_eq!(
            crate::backend::verify_model("sonnet", parsed.effective_model.as_deref(), &[]),
            crate::backend::ModelVerification::Unverified {
                requested: "sonnet".into()
            },
            "an unreported model is a gap the runner must act on, not a match"
        );
    }

    /// A result document that names no model at all — the older shapes,
    /// and any harness that simply does not say.
    #[test]
    fn a_result_naming_no_model_reports_none() {
        let parsed = parse_result_json(r#"{"result":"done","session_id":"s"}"#);
        assert_eq!(parsed.effective_model, None);
    }

    #[test]
    fn substitution_is_detectable_because_the_effective_model_surfaces() {
        let parsed = parse_result_json(SAMPLE);
        let effective = parsed.effective_model.expect("model surfaces");
        assert!(!crate::backend::model_matches("haiku", &effective));
        assert!(crate::backend::model_matches("sonnet", &effective));
    }

    #[test]
    fn the_worker_model_is_the_one_that_answered_and_subagents_make_usage_inclusive() {
        let json = r#"{"result":"ok","modelUsage":{
            "claude-haiku-4-5":{"outputTokens":5000,"canonicalModel":"claude-haiku-4-5"},
            "claude-sonnet-5":{"outputTokens":12000}},
            "subagent_stats":{"spawned":2},"total_cost_usd":0.5}"#;
        let parsed = parse_result_json(json);
        assert_eq!(parsed.effective_model.as_deref(), Some("claude-sonnet-5"));
        assert!(
            parsed.usage.cost.inclusive(),
            "a session total with subagents is inclusive"
        );
    }

    #[test]
    fn permission_denials_and_errors_are_read() {
        let json = r#"{"result":"I need permission","is_error":false,
            "permission_denials":[{"tool_name":"Edit","tool_use_id":"a"},
                                  {"tool_name":"Edit","tool_use_id":"b"},
                                  {"tool_name":"Bash","tool_use_id":"c","tool_input":{"command":"git diff --stat"}}]}"#;
        let parsed = parse_result_json(json);
        assert_eq!(
            parsed.permission_denials,
            vec![
                PermissionDenial::new("Edit", Some("a")),
                PermissionDenial::new("Edit", Some("b")),
                PermissionDenial::new("Bash(git diff --stat)", Some("c")),
            ]
        );
        assert_eq!(parsed.permission_denials[2].tool_name(), "Bash");
        let parsed = parse_result_json(r#"{"result":"boom","is_error":true}"#);
        assert!(parsed.is_error);
    }

    #[test]
    fn blockage_claims_are_detected_from_the_result_text() {
        let parsed =
            parse_result_json(r#"{"result": "relais-blocked: required dependency missing"}"#);
        let claims = parsed
            .result_text
            .as_deref()
            .is_some_and(|text| text.contains("relais-blocked:"));
        assert!(claims);
    }

    #[test]
    fn capabilities_come_from_the_installed_help_text() {
        let caps = capabilities_from_help("2.1.278".into(), HELP_2_1);
        assert!(caps.supports_model);
        assert_eq!(
            caps.accepted_efforts,
            crate::catalog::Fact::Unknown,
            "the flag is there and no list of levels is"
        );
        assert!(caps.supports_budget, "--max-budget-usd is the budget flag");
        assert!(!caps.supports_max_turns, "2.1.278 has no turn ceiling");
        assert!(caps.supports_disallowed_tools);
        assert!(caps.supports_settings);
        let old = capabilities_from_help("1.0".into(), "--model --budget --max-turns");
        assert!(!old.supports_budget, "`--budget` is not the budget flag");
        assert!(old.supports_max_turns);
        assert!(!old.supports_disallowed_tools);
    }

    #[test]
    fn the_turn_ceiling_capability_is_reported_not_assumed() {
        use crate::backend::TurnCeiling;
        assert_eq!(
            capabilities_from_help("2.1.278".into(), HELP_2_1).turn_ceiling(),
            TurnCeiling::Unavailable,
            "SPEC §11 promises a turn ceiling this harness cannot take"
        );
        assert_eq!(
            capabilities_from_help("1.0".into(), "--model --max-turns").turn_ceiling(),
            TurnCeiling::Harness
        );
        assert_eq!(TurnCeiling::Unavailable.as_str(), "unavailable");
        assert_eq!(TurnCeiling::Harness.as_str(), "harness");
    }

    #[test]
    fn argv_carries_the_budget_in_dollars_and_the_allowlist_as_settings() {
        let caps = capabilities_from_help("2.1.278".into(), HELP_2_1);
        let argv = build_argv(&spec(Some(1_500_000)), &caps).expect("argv");
        let budget_at = argv
            .iter()
            .position(|arg| arg == "--max-budget-usd")
            .expect("budget flag");
        assert_eq!(argv[budget_at + 1], "1.5");
        assert!(!argv.iter().any(|arg| arg == "--budget"));
        assert!(!argv.iter().any(|arg| arg == "--max-turns"));
        assert!(!argv.iter().any(|arg| arg.contains("permission-mode")));
        assert!(!argv.iter().any(|arg| arg.contains("dangerously")));
        let deny_at = argv
            .iter()
            .position(|arg| arg == "--disallowed-tools")
            .expect("deny flag");
        assert_eq!(argv[deny_at + 1], "Bash(git push:*)");
        let settings_at = argv
            .iter()
            .position(|arg| arg == "--settings")
            .expect("settings flag");
        let settings: serde_json::Value =
            serde_json::from_str(&argv[settings_at + 1]).expect("settings json");
        assert_eq!(
            settings["permissions"]["allow"],
            serde_json::json!(["Edit", "Bash(cargo test:*)"])
        );
        assert_eq!(&argv[..3], &["-p", "--model", "sonnet"]);
    }

    #[test]
    fn a_default_permissions_worker_is_denied_subagents() {
        let caps = capabilities_from_help("2.1.278".into(), HELP_2_1);
        let mut worker = spec(None);
        worker.disallowed_tools =
            crate::policy::effective_disallowed_tools(&crate::policy::Permissions::default());
        let argv = build_argv(&worker, &caps).expect("argv");
        let denied: Vec<&str> = argv
            .windows(2)
            .filter(|pair| pair[0] == "--disallowed-tools")
            .map(|pair| pair[1].as_str())
            .collect();
        assert!(denied.contains(&"Agent"), "denied: {denied:?}");
        assert!(denied.contains(&"Task"), "denied: {denied:?}");
    }

    #[test]
    fn budget_dollars_is_a_plain_decimal() {
        assert_eq!(budget_dollars(1_500_000), "1.5");
        assert_eq!(budget_dollars(3_000_000), "3");
        assert_eq!(budget_dollars(250), "0.00025");
        assert_eq!(budget_dollars(-7), "0");
    }

    #[test]
    fn launch_controls_fail_closed_when_the_harness_lacks_them() {
        let caps = capabilities_from_help("x".into(), "--model --output-format");
        let err = build_argv(&spec(None), &caps).expect_err("deny list unsupported");
        assert!(matches!(err, BackendError::Unsupported(_)));
        let mut no_deny = spec(None);
        no_deny.disallowed_tools.clear();
        let err = build_argv(&no_deny, &caps).expect_err("allowlist unsupported");
        assert!(matches!(err, BackendError::Unsupported(_)));
        no_deny.allowed_tools.clear();
        no_deny.effort = None;
        let argv = build_argv(&no_deny, &caps).expect("nothing left to refuse");
        assert!(!argv.iter().any(|arg| arg == "--max-budget-usd"));
    }

    fn effort_value(argv: &[String]) -> Option<&str> {
        argv.iter()
            .position(|arg| arg == "--effort")
            .map(|at| argv[at + 1].as_str())
    }

    /// The real 2.1.284 help lists its levels, so a level outside that
    /// list is refused with all three facts named, and one inside it is
    /// passed through exactly as configured.
    #[test]
    fn a_requested_effort_is_passed_on_or_refused_never_dropped() {
        let help = include_str!("../../tests/fixtures/help/claude-2.1.284.txt");
        let caps = capabilities_from_help("2.1.284".into(), help);
        let argv = build_argv(&spec(None), &caps).expect("medium is listed");
        assert_eq!(effort_value(&argv), Some("medium"));

        let mut ultra = spec(None);
        ultra.effort = crate::policy::EffortId::parse("ultra").ok();
        let err = build_argv(&ultra, &caps).expect_err("ultra is not listed");
        let BackendError::EffortUnsupported {
            effort,
            model,
            harness_version,
        } = &err
        else {
            panic!("expected EffortUnsupported, got {err:?}");
        };
        assert_eq!(
            (effort.as_str(), model.as_str(), harness_version.as_str()),
            ("ultra", "sonnet", "2.1.284")
        );
        assert!(err.to_string().contains("ultra"), "{err}");

        let none = capabilities_from_help(
            "2.1.0".into(),
            include_str!("../../tests/fixtures/help/claude-no-effort.txt"),
        );
        let mut plain = spec(None);
        plain.disallowed_tools.clear();
        plain.allowed_tools.clear();
        assert!(
            matches!(
                build_argv(&plain, &none),
                Err(BackendError::EffortUnsupported { .. })
            ),
            "a CLI with no --effort flag refuses instead of dropping it"
        );
        plain.effort = None;
        assert!(build_argv(&plain, &none).is_ok(), "no request, no refusal");
    }

    /// The compatibility carve-out: the flag exists, its help lists no
    /// levels, and the effort the repo policy configured goes through.
    #[test]
    fn an_unknown_cli_fact_passes_the_configured_effort_through() {
        let caps = capabilities_from_help(
            "2.1.0".into(),
            include_str!("../../tests/fixtures/help/claude-effort-no-list.txt"),
        );
        assert_eq!(caps.accepted_efforts, crate::catalog::Fact::Unknown);
        let argv = build_argv(&spec(None), &caps).expect("passed through");
        assert_eq!(effort_value(&argv), Some("medium"));
    }

    /// V13: the launch runs with the worker's worktree as its working
    /// directory, so a binary named relatively is probed in one place and
    /// executed in another. It is refused, and an absolute one is
    /// canonicalised so both halves name the same file.
    #[test]
    fn the_backend_binary_is_absolute_or_refused() {
        let error = ClaudeBackend::with_binary(PathBuf::from("node_modules/.bin/claude"))
            .expect_err("a relative binary is refused");
        assert!(
            error.to_string().contains("absolute path"),
            "{error}: {error:?}"
        );

        let dir = crate::test_support::temp_dir("claude-bin");
        std::fs::create_dir_all(dir.join("bin")).expect("mkdir");
        let binary = dir.join("bin/claude");
        std::fs::write(&binary, "#!/bin/sh\nexit 0\n").expect("write");
        let backend = ClaudeBackend::with_binary(dir.join("bin/../bin/claude"))
            .expect("an absolute binary resolves");
        assert_eq!(
            backend.binary(),
            binary.canonicalize().expect("canonical").as_path(),
            "the path the probe used is the path the launch runs"
        );
        assert!(
            ClaudeBackend::with_binary(dir.join("bin/absent")).is_err(),
            "a binary that is not there is a missing binary, never a fallback"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
