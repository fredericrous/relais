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
    PermissionEnforcement, SandboxCapability, UsageReport,
};
use crate::money::MicroUsd;
use crate::procs::{run_with_timeout, Ended, ProcessEnd};
use crate::tooling::PROBE_TIMEOUT;

#[derive(Debug)]
pub struct ClaudeBackend {
    binary: PathBuf,
    /// Probed on first use and kept: `--version` and `--help` are facts
    /// about an installed CLI, and asking again before every dispatch
    /// spent two processes per launch to learn the same thing (audit V6).
    capabilities: OnceLock<Option<Capabilities>>,
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

    /// The probe, cached. `cancel` aborts a probe that hangs; the timeout
    /// bounds one that merely takes its time.
    fn capabilities(&self, cancel: Option<&AtomicBool>) -> Option<Capabilities> {
        self.capabilities
            .get_or_init(|| self.probe_now(cancel))
            .clone()
    }

    fn probe_now(&self, cancel: Option<&AtomicBool>) -> Option<Capabilities> {
        let version = self.ask(&["--version"], cancel)?;
        // A CLI that answers `--version` but not `--help` cannot have its
        // flags checked, and a launch may not assume them: no
        // capabilities means no backend, which blocks (SPEC §6).
        let help = self.ask(&["--help"], cancel)?;
        Some(capabilities_from_help(
            version.trim().to_string(),
            help.trim(),
        ))
    }

    /// One probe call: bounded, cancellable, and `None` unless the CLI
    /// exited successfully with something to say.
    fn ask(&self, args: &[&str], cancel: Option<&AtomicBool>) -> Option<String> {
        let mut command = Command::new(&self.binary);
        command.args(args);
        let end = run_with_timeout(command, PROBE_TIMEOUT, None, cancel, None).ok()?;
        if end.ended != Ended::Exited(0) {
            return None;
        }
        let answer = format!("{}{}", end.stdout, end.stderr);
        (!answer.trim().is_empty()).then_some(answer)
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
        let caps = self
            .capabilities(spec.cancel.as_deref())
            .ok_or_else(|| BackendError::MissingBinary(self.binary.to_string_lossy().into()))?;
        let argv = build_argv(spec, &caps)?;

        let mut command = Command::new(&self.binary);
        command
            .args(&argv)
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
        let end = run_with_timeout(
            command,
            wall,
            Some(spec.prompt.clone().into_bytes()),
            spec.cancel.as_deref(),
            spec.pid_slot.as_deref(),
        )?;

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
        supports_effort: supports("--effort"),
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

    let mut argv: Vec<String> = vec!["-p".into(), "--model".into(), spec.model.clone()];
    if let Some(effort) = spec.effort {
        if caps.supports_effort {
            argv.push("--effort".into());
            argv.push(
                match effort {
                    crate::policy::Effort::Low => "low",
                    crate::policy::Effort::Medium => "medium",
                    crate::policy::Effort::High => "high",
                }
                .into(),
            );
        }
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
    if !spec.allowed_tools.is_empty() {
        argv.push("--settings".into());
        argv.push(
            serde_json::json!({ "permissions": { "allow": spec.allowed_tools } }).to_string(),
        );
    }
    Ok(argv)
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
    /// Tool names from `permission_denials`, deduplicated, in order.
    pub permission_denials: Vec<String>,
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
            let mut tools: Vec<String> = Vec::new();
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
                if !tools.contains(&entry) {
                    tools.push(entry);
                }
            }
            tools
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
            effort: Some(crate::policy::Effort::Medium),
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
            crate::backend::verify_model("sonnet", parsed.effective_model.as_deref()),
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
            vec!["Edit", "Bash(git diff --stat)"]
        );
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
        assert!(caps.supports_effort);
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
        let argv = build_argv(&no_deny, &caps).expect("nothing left to refuse");
        assert!(!argv.iter().any(|arg| arg == "--max-budget-usd"));
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

        let dir = std::env::temp_dir().join(format!(
            "relais-claude-bin-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&dir).ok();
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

    /// Unique fixture directories: the pid is shared by parallel test
    /// threads, the counter is not.
    static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
}
