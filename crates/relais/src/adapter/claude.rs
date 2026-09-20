//! The mandatory Claude Code adapter (SPEC §8, §15).
//!
//! Launches `claude -p` with explicit model, effort, turn limits and
//! budget controls, prompt via stdin, arguments as an argv array, output
//! as structured JSON. Every flag is capability-checked against the
//! installed version's `--help` output before use — nothing is assumed
//! from documentation. The result JSON is parsed leniently: absent fields
//! are unknown, never zero, and the effective model is surfaced so an
//! unapproved substitution is detectable (SPEC §6).

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use super::{
    run_with_timeout, Backend, BackendError, Capabilities, LaunchResult, LaunchSpec,
    PermissionEnforcement, SandboxCapability, UsageReport,
};
use crate::money::{CostCompleteness, MicroUsd};

pub struct ClaudeBackend {
    binary: PathBuf,
}

impl ClaudeBackend {
    /// Discovery: `RELAIS_CLAUDE_BIN` wins, then PATH lookup. Missing
    /// binary is a blocked outcome, never a fallback (SPEC §6).
    pub fn discover() -> Result<Self, BackendError> {
        if let Some(explicit) = std::env::var_os("RELAIS_CLAUDE_BIN") {
            let path = PathBuf::from(explicit);
            if path.is_file() {
                return Ok(Self { binary: path });
            }
            return Err(BackendError::MissingBinary(path.to_string_lossy().into()));
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("claude");
            if candidate.is_file() {
                return Ok(Self { binary: candidate });
            }
        }
        Err(BackendError::MissingBinary("claude".into()))
    }

    pub fn with_binary(binary: PathBuf) -> Self {
        Self { binary }
    }

    fn run_version(&self) -> Option<String> {
        let output = Command::new(&self.binary).arg("--version").output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn run_help(&self) -> String {
        Command::new(&self.binary)
            .arg("--help")
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout).to_string()
                    + &String::from_utf8_lossy(&output.stderr)
            })
            .unwrap_or_default()
    }
}

impl Backend for ClaudeBackend {
    fn name(&self) -> &'static str {
        "claude-code"
    }

    fn probe(&self) -> Option<Capabilities> {
        let version = self.run_version()?;
        let help = self.run_help();
        Some(capabilities_from_help(version, &help))
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let caps = self
            .probe()
            .ok_or_else(|| BackendError::MissingBinary(self.binary.to_string_lossy().into()))?;
        let argv = build_argv(spec, &caps)?;

        let mut command = Command::new(&self.binary);
        command
            .args(&argv)
            .current_dir(&spec.work_dir)
            .env_remove("CLAUDE_CODE_EXTRA_BUDGET")
            // The worker must not reach the machine authority a nested
            // `relais` would read; the defaults are the real ones.
            .env_remove("RELAIS_CONFIG_DIR")
            .env_remove("RELAIS_STATE_DIR");
        let wall = spec.wall_timeout.max(Duration::from_secs(1));
        let end = run_with_timeout(
            command,
            wall,
            Some(spec.prompt.clone().into_bytes()),
            spec.cancel.as_deref(),
            spec.pid_slot.as_deref(),
        )?;

        let parsed = parse_result_json(&end.stdout, &spec.model);
        let worker_claims_blockage = parsed
            .result_text
            .as_deref()
            .is_some_and(super::claims_blockage);
        // A harness that exited non-zero, reported an error, or printed
        // something the adapter cannot read did not complete an attempt:
        // the result is missing, never an empty candidate (SPEC §9).
        let completed = !end.timed_out && !end.cancelled && end.exit_code == Some(0);
        let usable = completed && parsed.result_text.is_some() && !parsed.is_error;
        let failure_detail = if usable {
            None
        } else {
            Some(failure_detail(&end, &parsed))
        };
        Ok(LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            exit_code: end.exit_code,
            stdout: end.stdout,
            stderr: end.stderr,
            timed_out: end.timed_out,
            result_text: if usable { parsed.result_text } else { None },
            session_id: parsed.session_id,
            effective_model: parsed.effective_model,
            usage: parsed.usage,
            worker_claims_blockage,
            cancelled: end.cancelled,
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
    // deliverable on this harness, and a receipt says so.
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

fn failure_detail(end: &super::ProcessEnd, parsed: &ParsedClaudeResult) -> String {
    let mut parts = Vec::new();
    match end.exit_code {
        Some(code) => parts.push(format!("exit {code}")),
        None => parts.push("no exit status".to_string()),
    }
    if parsed.is_error {
        parts.push("the harness reported an error".to_string());
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
    pub effective_model: Option<String>,
    pub usage: UsageReport,
    /// `is_error` as the harness reported it.
    pub is_error: bool,
    /// Tool names from `permission_denials`, deduplicated, in order.
    pub permission_denials: Vec<String>,
}

pub fn parse_result_json(stdout: &str, requested_model: &str) -> ParsedClaudeResult {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return ParsedClaudeResult {
            result_text: None,
            session_id: None,
            effective_model: Some(requested_model.to_string()),
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
        cost: value
            .get("total_cost_usd")
            .and_then(|cost| cost.as_f64())
            .map(MicroUsd::from_dollars),
        cost_completeness: if value.get("total_cost_usd").is_some() {
            CostCompleteness::Actual
        } else {
            CostCompleteness::Unknown
        },
        inclusive: spawned_subagents,
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
        effective_model: effective_model(&value).or_else(|| Some(requested_model.to_string())),
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
            wall_timeout: Duration::from_secs(10),
            cancel: None,
            pid_slot: None,
        }
    }

    #[test]
    fn parses_documented_result_fields() {
        let parsed = parse_result_json(SAMPLE, "sonnet");
        assert_eq!(
            parsed.result_text.as_deref(),
            Some("done: fixed the escaping")
        );
        assert_eq!(parsed.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(parsed.effective_model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(parsed.usage.input_tokens, Some(1200));
        assert_eq!(parsed.usage.output_tokens, Some(300));
        assert_eq!(parsed.usage.cache_read_tokens, Some(800));
        assert_eq!(parsed.usage.cost, Some(MicroUsd::from_micros(12_300)));
        assert_eq!(parsed.usage.cost_completeness, CostCompleteness::Actual);
        assert!(!parsed.usage.inclusive);
        assert!(!parsed.is_error);
        assert!(parsed.permission_denials.is_empty());
    }

    #[test]
    fn absent_usage_stays_unknown_never_zero() {
        let parsed = parse_result_json(r#"{"result": "no usage here"}"#, "sonnet");
        assert_eq!(parsed.usage.input_tokens, None);
        assert_eq!(parsed.usage.cost, None);
        assert_eq!(parsed.usage.cost_completeness, CostCompleteness::Unknown);
    }

    #[test]
    fn unparseable_output_falls_back_to_requested_model_and_unknown_usage() {
        let parsed = parse_result_json("not json", "sonnet");
        assert_eq!(parsed.result_text, None);
        assert_eq!(parsed.effective_model.as_deref(), Some("sonnet"));
        assert_eq!(parsed.usage.cost_completeness, CostCompleteness::Unknown);
    }

    #[test]
    fn substitution_is_detectable_because_the_effective_model_surfaces() {
        let parsed = parse_result_json(SAMPLE, "haiku");
        let effective = parsed.effective_model.expect("model surfaces");
        assert!(!super::super::model_matches("haiku", &effective));
        assert!(super::super::model_matches("sonnet", &effective));
    }

    #[test]
    fn the_worker_model_is_the_one_that_answered_and_subagents_make_usage_inclusive() {
        let json = r#"{"result":"ok","modelUsage":{
            "claude-haiku-4-5":{"outputTokens":5000,"canonicalModel":"claude-haiku-4-5"},
            "claude-sonnet-5":{"outputTokens":12000}},
            "subagent_stats":{"spawned":2},"total_cost_usd":0.5}"#;
        let parsed = parse_result_json(json, "sonnet");
        assert_eq!(parsed.effective_model.as_deref(), Some("claude-sonnet-5"));
        assert!(
            parsed.usage.inclusive,
            "a session total with subagents is inclusive"
        );
    }

    #[test]
    fn permission_denials_and_errors_are_read() {
        let json = r#"{"result":"I need permission","is_error":false,
            "permission_denials":[{"tool_name":"Edit","tool_use_id":"a"},
                                  {"tool_name":"Edit","tool_use_id":"b"},
                                  {"tool_name":"Bash","tool_use_id":"c","tool_input":{"command":"git diff --stat"}}]}"#;
        let parsed = parse_result_json(json, "sonnet");
        assert_eq!(
            parsed.permission_denials,
            vec!["Edit", "Bash(git diff --stat)"]
        );
        let parsed = parse_result_json(r#"{"result":"boom","is_error":true}"#, "sonnet");
        assert!(parsed.is_error);
    }

    #[test]
    fn blockage_claims_are_detected_from_the_result_text() {
        let parsed = parse_result_json(
            r#"{"result": "relais-blocked: required dependency missing"}"#,
            "sonnet",
        );
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
        use crate::adapter::TurnCeiling;
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
}
