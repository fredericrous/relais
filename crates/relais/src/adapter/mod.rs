//! Execution backends (SPEC §8, §15, §20).
//!
//! The mandatory Claude Code adapter launches `claude -p` child processes
//! with explicit model, effort, turn limits and budget controls, passing
//! prompts via stdin and arguments as an argv array. The installed version
//! is capability-checked against a compatibility matrix before any
//! launch; missing permissions produce a blocked result and no bypass
//! flags are introduced. The adapter contract covers launch, events,
//! cancellation, effective profile, permission capability, sandbox
//! capability and usage completeness; a backend interface admits
//! alternative providers without requiring every possible provider to
//! ship. No adapter may advertise guarantees its backend cannot enforce.

pub mod claude;
pub mod mock;

pub use mock::{MockBackend, MockOutcome};

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::money::{CostCompleteness, MicroUsd};
use crate::policy::Effort;

#[derive(Debug)]
pub enum BackendError {
    MissingBinary(String),
    Launch(String),
    Unsupported(&'static str),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBinary(name) => write!(f, "required binary `{name}` is not available"),
            Self::Launch(detail) => write!(f, "backend launch failed: {detail}"),
            Self::Unsupported(what) => write!(f, "backend does not support {what}"),
        }
    }
}

impl std::error::Error for BackendError {}

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

/// How a process run ended, beyond its exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEnd {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
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

/// Runs a command with a wall timeout, streaming stdout+stderr into one
/// captured buffer. Reader threads own the pipes (a blocking read in the
/// wait loop would stall the timeout until the child spoke again); on
/// timeout the whole process group is killed: the launch counts as
/// interrupted, never as a completed attempt.
pub fn run_with_timeout(
    mut command: Command,
    wall_timeout: Duration,
    stdin_bytes: Option<Vec<u8>>,
    cancel: Option<&AtomicBool>,
    pid_slot: Option<&AtomicU32>,
) -> Result<ProcessEnd, BackendError> {
    command.stdin(
        stdin_bytes
            .as_ref()
            .map_or(Stdio::null(), |_| Stdio::piped()),
    );
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    own_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| BackendError::Launch(format!("spawn: {e}")))?;
    if let Some(slot) = pid_slot {
        slot.store(child.id(), Ordering::SeqCst);
    }

    if let Some(bytes) = stdin_bytes {
        let mut stdin = child.stdin.take().expect("stdin is piped when bytes exist");
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }

    let stdout_pipe = child.stdout.take().expect("stdout piped");
    let stderr_pipe = child.stderr.take().expect("stderr piped");
    let stdout = std::thread::spawn(move || read_to_end(stdout_pipe));
    let stderr = std::thread::spawn(move || read_to_end(stderr_pipe));

    let (status, timed_out, cancelled) = wait_for_exit(&mut child, wall_timeout, cancel)
        .map_err(|e| BackendError::Launch(format!("wait: {e}")))?;
    let stdout = stdout
        .join()
        .map_err(|_| BackendError::Launch("stdout reader panicked".into()))?;
    let stderr = stderr
        .join()
        .map_err(|_| BackendError::Launch("stderr reader panicked".into()))?;
    Ok(ProcessEnd {
        // A killed process has no exit code: interrupted, never a
        // completed attempt.
        exit_code: if cancelled { None } else { status.code() },
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
        cancelled,
    })
}

/// Wait for a child under a wall clock and an optional cancel flag,
/// killing its WHOLE process group when either fires. The one loop every
/// subprocess in relais waits in — the verification runner used to have
/// its own copy that killed only the direct child and left `cargo test`
/// grandchildren running in a worktree about to be removed.
pub fn wait_for_exit(
    child: &mut std::process::Child,
    wall_timeout: Duration,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<(std::process::ExitStatus, bool, bool)> {
    let started = Instant::now();
    let (timed_out, cancelled) = loop {
        if child.try_wait()?.is_some() {
            break (false, false);
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            let _ = crate::procs::kill_tree(child);
            break (false, true);
        }
        if started.elapsed() >= wall_timeout {
            let _ = crate::procs::kill_tree(child);
            break (true, false);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let status = child.wait()?;
    Ok((status, timed_out, cancelled))
}

/// Put a command in its own process group so a kill reaches everything it
/// spawned. Every subprocess relais waits on goes through here.
pub fn own_process_group(command: &mut Command) {
    crate::procs::own_process_group(command);
}

fn read_to_end(mut pipe: impl std::io::Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = std::io::Read::read_to_end(&mut pipe, &mut buffer);
    buffer
}

/// A stub backend for tests and `relais doctor --dry`: a shell command
/// whose stdout is the terminal result JSON. `enforcement` is reported
/// exactly as observed (it does nothing itself), because no adapter may
/// advertise guarantees its backend cannot enforce.
pub struct ScriptBackend {
    pub program: String,
    pub args: Vec<String>,
}

impl Backend for ScriptBackend {
    fn name(&self) -> &'static str {
        "script"
    }

    fn probe(&self) -> Option<Capabilities> {
        Some(Capabilities {
            backend: "script".into(),
            version: None,
            supports_model: false,
            supports_effort: false,
            supports_max_turns: false,
            supports_output_format_json: false,
            supports_budget: false,
            supports_disallowed_tools: false,
            supports_settings: false,
            permission_enforcement: PermissionEnforcement::Observed,
            sandbox: SandboxCapability::WorktreeOnly,
        })
    }

    fn launch(&self, spec: &LaunchSpec) -> Result<LaunchResult, BackendError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&spec.work_dir)
            .stdin(Stdio::null());
        let end = run_with_timeout(
            command,
            spec.wall_timeout,
            None,
            spec.cancel.as_deref(),
            spec.pid_slot.as_deref(),
        )?;
        Ok(LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            exit_code: end.exit_code,
            result_text: if end.timed_out || end.cancelled {
                None
            } else {
                Some(end.stdout.clone())
            },
            stdout: end.stdout,
            stderr: end.stderr,
            timed_out: end.timed_out,
            session_id: None,
            effective_model: Some(spec.model.clone()),
            usage: UsageReport::unknown(),
            worker_claims_blockage: false,
            cancelled: end.cancelled,
            permission_denials: Vec::new(),
            failure_detail: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_with_timeout_kills_slow_children() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo start; sleep 30; echo done"]);
        let started = Instant::now();
        let pid_slot = AtomicU32::new(0);
        let end = run_with_timeout(
            command,
            Duration::from_millis(300),
            None,
            None,
            Some(&pid_slot),
        )
        .expect("runs");
        assert_ne!(pid_slot.load(Ordering::SeqCst), 0, "the PID was published");
        assert!(
            end.timed_out,
            "must be marked interrupted by the wall clock"
        );
        assert!(!end.cancelled);
        assert_eq!(end.exit_code, None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            end.stdout.contains("start"),
            "output before the kill is kept: {}",
            end.stdout
        );
    }

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
    fn cancellation_kills_the_child_and_is_not_a_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo start; sleep 30; echo done"]);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let end = run_with_timeout(command, Duration::from_secs(30), None, Some(&cancel), None)
            .expect("runs");
        assert!(end.cancelled);
        assert!(!end.timed_out);
        assert_eq!(
            end.exit_code, None,
            "a cancelled run has no terminal result"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn run_with_timeout_feeds_stdin_and_captures_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "cat; echo processed"]);
        let end = run_with_timeout(
            command,
            Duration::from_secs(10),
            Some(b"prompt-bytes".to_vec()),
            None,
            None,
        )
        .expect("runs");
        assert_eq!(end.exit_code, Some(0));
        assert!(!end.timed_out);
        assert!(end.stdout.contains("prompt-bytes"));
        assert!(
            end.stdout.contains("processed"),
            "{} {}",
            end.stdout,
            end.stderr
        );
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
