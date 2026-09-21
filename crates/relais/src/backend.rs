//! The execution-backend contract (SPEC §20) and its wire types.
//!
//! A backend launches one dispatch and reports what came back: the
//! terminal result text, the model that actually ran, what it cost, the
//! tools the harness refused, and how the process ended. The contract
//! covers launch, cancellation, effective profile, permission capability,
//! sandbox capability and usage completeness. No adapter may advertise
//! guarantees its backend cannot enforce, and nothing here assumes a
//! capability: they are probed on the machine.
//!
//! The trait and its types live here rather than inside `adapter`
//! because they are this crate's interface to a model harness, not the
//! Claude adapter's property: the runner depends on `Backend`, the
//! adapter implements it, and a second implementation costs nobody a
//! change of import. `adapter` keeps the concrete backends and the
//! process plumbing for talking to them.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::money::{CostCompleteness, MicroUsd};
use crate::policy::Effort;
use crate::procs::RunError;

#[derive(Debug)]
pub enum BackendError {
    MissingBinary(String),
    Launch(String),
    /// The child could not be supervised: it would not spawn, the wait
    /// failed, or a pipe reader was lost. Never a worker's doing.
    Process(RunError),
    Unsupported(&'static str),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBinary(name) => write!(f, "required binary `{name}` is not available"),
            Self::Launch(detail) => write!(f, "backend launch failed: {detail}"),
            Self::Process(e) => write!(f, "backend launch failed: {e}"),
            Self::Unsupported(what) => write!(f, "backend does not support {what}"),
        }
    }
}

impl std::error::Error for BackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Process(e) => Some(e),
            Self::MissingBinary(_) | Self::Launch(_) | Self::Unsupported(_) => None,
        }
    }
}

impl From<RunError> for BackendError {
    fn from(e: RunError) -> Self {
        Self::Process(e)
    }
}

/// What a backend reports about itself before any dispatch (SPEC §20).
/// Probed, not assumed: versions and flags are observed on the machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub backend: String,
    pub version: Option<String>,
    pub supports_model: bool,
    pub supports_effort: bool,
    pub supports_max_turns: bool,
    pub supports_output_format_json: bool,
    /// A per-launch API dollar ceiling (`--max-budget-usd` on Claude
    /// Code). Best effort across in-flight requests (SPEC §11).
    pub supports_budget: bool,
    /// A tool deny list. The one launch control that constrains the
    /// worker: when the harness cannot take it, the launch fails closed.
    #[serde(default)]
    pub supports_disallowed_tools: bool,
    /// An explicit settings document (`--settings`), which is how a
    /// machine-owned permission allowlist reaches the worker without any
    /// permission-mode flag (SPEC §8).
    #[serde(default)]
    pub supports_settings: bool,
    pub permission_enforcement: PermissionEnforcement,
    pub sandbox: SandboxCapability,
}

impl Capabilities {
    /// Who, if anyone, enforces a turn ceiling. SPEC §11 lists turns
    /// among the ceilings the runner enforces, and Claude Code 2.1.x has
    /// no flag for one: the honest answer is to report which it is, on
    /// every run, rather than let a receipt imply a ceiling nothing
    /// applied.
    pub fn turn_ceiling(&self) -> TurnCeiling {
        if self.supports_max_turns {
            TurnCeiling::Harness
        } else {
            TurnCeiling::Unavailable
        }
    }
}

/// Whether the installed harness can be given a turn ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TurnCeiling {
    /// The harness takes a turn limit and enforces it.
    Harness,
    /// This harness has no turn-limit flag; attempts and wall time are
    /// the ceilings that actually bound the run.
    #[default]
    Unavailable,
}

impl TurnCeiling {
    /// The value recorded in the context manifest, so a receipt says
    /// which it was for that run.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Harness => "harness",
            Self::Unavailable => "unavailable",
        }
    }

    /// The line `doctor` prints.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Harness => "turn ceiling: enforced by the harness",
            Self::Unavailable => "turn ceiling: not available on this Claude Code — attempts and wall time are enforced by the runner, turns are not",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEnforcement {
    /// The backend enforces tool permission lists server-side.
    Enforced,
    /// Permissions can be observed but not guaranteed.
    Observed,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxCapability {
    /// OS/container isolation configured for this backend.
    Strong,
    /// Worktree-only: an acceptance boundary, not isolation (SPEC §8).
    WorktreeOnly,
    #[default]
    Unknown,
}

/// One model dispatch. The prompt travels via stdin; arguments are an
/// argv array; the working directory is the owned task worktree.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub dispatch_id: String,
    pub prompt: String,
    pub model: String,
    pub effort: Option<Effort>,
    pub max_turns: Option<u32>,
    pub budget_micros: Option<i64>,
    pub disallowed_tools: Vec<String>,
    /// Machine-owned permission rules the worker may use without asking
    /// (a print-mode harness cannot ask). Explicit and reviewed, never a
    /// bypass: a tool outside this list is still denied (SPEC §8).
    pub allowed_tools: Vec<String>,
    pub work_dir: PathBuf,
    pub wall_timeout: Duration,
    /// Set by the runner when the coordinator cancels this dispatch; the
    /// adapter kills the process group and reports `cancelled`
    /// (SPEC §20: the adapter contract includes cancellation).
    pub cancel: Option<Arc<AtomicBool>>,
    /// Receives the child PID as soon as it exists, so the runner can
    /// bind it to the lease and the ledger while the worker runs
    /// (SPEC §12: persist the PID after the dispatch intent).
    pub pid_slot: Option<Arc<AtomicU32>>,
}

/// Provider-reported usage as extracted from one terminal result.
/// Everything absent is `None` — unknown, never zero (SPEC §11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UsageReport {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub cost: Option<MicroUsd>,
    pub cost_completeness: CostCompleteness,
    /// True when the reported total already includes descendants
    /// (an inclusive parent: SPEC §11).
    pub inclusive: bool,
}

impl UsageReport {
    pub fn unknown() -> Self {
        Self {
            cost_completeness: CostCompleteness::Unknown,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchResult {
    pub dispatch_id: String,
    /// Exit code of the backend process; None when killed by signal.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// The result payload the worker proposed (text). Evidence only; the
    /// runner owns state.
    pub result_text: Option<String>,
    pub session_id: Option<String>,
    /// The model the backend reports as having actually run. Mismatching
    /// the requested model is an unapproved substitution: the runner stops
    /// further dispatch (SPEC §6).
    pub effective_model: Option<String>,
    pub usage: UsageReport,
    pub worker_claims_blockage: bool,
    /// Killed on a cancellation request, not on the wall clock.
    #[serde(default)]
    pub cancelled: bool,
    /// Tools the harness refused the worker, as it reported them. A
    /// worker that could not act is not a worker that chose not to:
    /// missing permissions produce a blocked result (SPEC §8).
    #[serde(default)]
    pub permission_denials: Vec<String>,
    /// Why the harness ended without a usable result — its stderr, or
    /// the error it reported — for the interrupted transition's evidence.
    #[serde(default)]
    pub failure_detail: Option<String>,
}

impl LaunchResult {
    /// A terminal result is missing when the process died without one,
    /// or ended with a non-zero status, or produced nothing the adapter
    /// could read as a result: interrupted, not failed, and never a
    /// completed attempt with an empty candidate (SPEC §9).
    pub fn terminal_result_missing(&self) -> bool {
        self.timed_out || self.cancelled || self.exit_code != Some(0) || self.result_text.is_none()
    }
}

/// Does the model the harness ran satisfy the model the route requested?
/// An explicit ID must match exactly. A short alias (`sonnet`, `haiku`,
/// `fable`) is satisfied by any concrete ID that carries it, because the
/// harness resolves aliases to dated IDs and reports those (SPEC §5:
/// aliases are permitted, the effective model is recorded). Anything
/// else is a substitution.
pub fn model_matches(requested: &str, effective: &str) -> bool {
    let requested = requested.trim().to_ascii_lowercase();
    let effective = effective.trim().to_ascii_lowercase();
    if requested == effective {
        return true;
    }
    let is_alias = !requested.contains('-') && !requested.contains(':');
    is_alias && effective.contains(&requested)
}

/// Does a worker's terminal text PROPOSE blockage? The marker must open a
/// line: a worker that merely mentions the protocol ("do not write
/// relais-blocked: unless…") is not claiming it. The runner, not the
/// worker, assigns the blocked state from this proposal (SPEC §9).
pub fn claims_blockage(result_text: &str) -> bool {
    result_text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("relais-blocked:") || line.starts_with("RELAIS-BLOCKED:")
    })
}

/// The adapter contract (SPEC §20).
pub trait Backend {
    fn name(&self) -> &'static str;
    /// Probe the installed backend; capabilities are observed, not
    /// declared. `None` = backend not installed (blocked, not fallback).
    fn probe(&self) -> Option<Capabilities>;
    /// Launch one dispatch and wait for its terminal result.
    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blockage_claim_opens_a_line_and_a_mention_does_not() {
        assert!(claims_blockage("relais-blocked: the crate is not vendored"));
        assert!(claims_blockage(
            "done reading\n  RELAIS-BLOCKED: no network"
        ));
        assert!(!claims_blockage(
            "I will not write relais-blocked: here because the task is possible"
        ));
    }

    #[test]
    fn missing_terminal_result_is_interrupted_not_failed() {
        let result = LaunchResult {
            dispatch_id: "disp-1".into(),
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            result_text: None,
            session_id: None,
            effective_model: None,
            usage: UsageReport::unknown(),
            worker_claims_blockage: false,
            cancelled: false,
            permission_denials: Vec::new(),
            failure_detail: None,
        };
        assert!(result.terminal_result_missing());
        let completed = LaunchResult {
            timed_out: false,
            exit_code: Some(0),
            result_text: Some("DONE".into()),
            ..result.clone()
        };
        assert!(!completed.terminal_result_missing());
        // A non-zero exit or an unreadable result is not a completed
        // attempt with an empty candidate; it is a missing result.
        let usage_error = LaunchResult {
            timed_out: false,
            exit_code: Some(1),
            result_text: Some(String::new()),
            ..result.clone()
        };
        assert!(usage_error.terminal_result_missing());
        let unreadable = LaunchResult {
            timed_out: false,
            exit_code: Some(0),
            result_text: None,
            ..result
        };
        assert!(unreadable.terminal_result_missing());
    }

    #[test]
    fn aliases_are_satisfied_by_dated_ids_and_explicit_ids_must_match() {
        assert!(model_matches("sonnet", "claude-sonnet-5"));
        assert!(model_matches("haiku", "claude-haiku-4-5-20251001"));
        assert!(model_matches("fable", "claude-fable-5-1"));
        assert!(model_matches("claude-sonnet-5", "claude-sonnet-5"));
        assert!(!model_matches(
            "claude-sonnet-5",
            "claude-sonnet-5-20261001"
        ));
        assert!(!model_matches("haiku", "claude-sonnet-5"));
        assert!(!model_matches("sonnet", "claude-haiku-4-5"));
    }
}
