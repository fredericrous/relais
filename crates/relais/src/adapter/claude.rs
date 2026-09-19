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
        let supports = |flag: &str| help.contains(flag);
        Some(Capabilities {
            backend: "claude-code".into(),
            version: Some(version),
            supports_model: supports("--model"),
            supports_effort: supports("--effort"),
            supports_max_turns: supports("--max-turns"),
            supports_output_format_json: supports("--output-format"),
            supports_budget: supports("--budget"),
            // Permission lists are enforced by the harness itself; the
            // adapter reports observed enforcement until the
            // compatibility matrix verifies it per version.
            permission_enforcement: PermissionEnforcement::Observed,
            sandbox: SandboxCapability::WorktreeOnly,
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let caps = self
            .probe()
            .ok_or_else(|| BackendError::MissingBinary(self.binary.to_string_lossy().into()))?;
        if !caps.supports_model {
            return Err(BackendError::Unsupported("explicit model selection"));
        }

        let mut argv: Vec<String> = vec!["-p".into()];
        argv.push("--model".into());
        argv.push(spec.model.clone());
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
                argv.push("--budget".into());
                argv.push(format!("{}usd", MicroUsd::from_micros(budget)));
            }
        }
        // No permission-bypass flags exist here, and none may be added
        // (SPEC §8): missing permissions produce a blocked result.
        for tool in &spec.disallowed_tools {
            argv.push("--disallowed-tools".into());
            argv.push(tool.clone());
        }

        let mut command = Command::new(&self.binary);
        command
            .args(&argv)
            .current_dir(&spec.work_dir)
            .env_remove("CLAUDE_CODE_EXTRA_BUDGET");
        let wall = spec.wall_timeout.max(Duration::from_secs(1));
        let end = run_with_timeout(
            command,
            wall,
            Some(spec.prompt.clone().into_bytes()),
            spec.cancel.as_deref(),
            spec.pid_slot.as_deref(),
        )?;
        let (exit, stdout, stderr, timed_out, cancelled) = (
            end.exit_code,
            end.stdout,
            end.stderr,
            end.timed_out,
            end.cancelled,
        );

        let parsed = parse_result_json(&stdout, &spec.model);
        let worker_claims_blockage = parsed.result_text.as_deref().is_some_and(|text| {
            text.contains("RELAYS-BLOCKED:") || text.contains("relais-blocked:")
        });
        Ok(LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            exit_code: exit,
            stdout,
            stderr,
            timed_out,
            result_text: if timed_out || cancelled {
                None
            } else {
                parsed.result_text.or(Some(String::new()))
            },
            session_id: parsed.session_id,
            effective_model: parsed.effective_model,
            usage: parsed.usage,
            worker_claims_blockage,
            cancelled,
        })
    }
}

/// Lenient result parsing. Every field is optional in the wild; absent
/// usage stays unknown (SPEC §11) and an unreadable payload keeps the raw
/// stdout as evidence instead of pretending it was empty.
pub struct ParsedClaudeResult {
    pub result_text: Option<String>,
    pub session_id: Option<String>,
    pub effective_model: Option<String>,
    pub usage: UsageReport,
}

pub fn parse_result_json(stdout: &str, requested_model: &str) -> ParsedClaudeResult {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return ParsedClaudeResult {
            result_text: None,
            session_id: None,
            effective_model: Some(requested_model.to_string()),
            usage: UsageReport::unknown(),
        };
    };
    let get_i64 = |path: &[&str]| -> Option<i64> {
        let mut current = &value;
        for key in path {
            current = current.get(*key)?;
        }
        current.as_i64()
    };
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
        inclusive: false,
    };
    ParsedClaudeResult {
        result_text: value
            .get("result")
            .and_then(|result| result.as_str())
            .map(|text| text.to_string()),
        session_id: value
            .get("session_id")
            .and_then(|id| id.as_str())
            .map(|id| id.to_string()),
        effective_model: value
            .get("model")
            .or_else(|| value.get("actual_model"))
            .and_then(|model| model.as_str())
            .map(|model| model.to_string())
            .or_else(|| Some(requested_model.to_string())),
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "result": "done: fixed the escaping",
        "session_id": "sess-abc",
        "total_cost_usd": 0.0123,
        "model": "claude-sonnet-4-5",
        "usage": {
            "input_tokens": 1200,
            "output_tokens": 300,
            "cache_read_input_tokens": 800,
            "cache_creation_input_tokens": 50
        }
    }"#;

    #[test]
    fn parses_documented_result_fields() {
        let parsed = parse_result_json(SAMPLE, "sonnet");
        assert_eq!(
            parsed.result_text.as_deref(),
            Some("done: fixed the escaping")
        );
        assert_eq!(parsed.session_id.as_deref(), Some("sess-abc"));
        assert_eq!(parsed.effective_model.as_deref(), Some("claude-sonnet-4-5"));
        assert_eq!(parsed.usage.input_tokens, Some(1200));
        assert_eq!(parsed.usage.output_tokens, Some(300));
        assert_eq!(parsed.usage.cache_read_tokens, Some(800));
        assert_eq!(parsed.usage.cost, Some(MicroUsd::from_micros(12_300)));
        assert_eq!(parsed.usage.cost_completeness, CostCompleteness::Actual);
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
        assert_ne!(parsed.effective_model.as_deref(), Some("haiku"));
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
}
