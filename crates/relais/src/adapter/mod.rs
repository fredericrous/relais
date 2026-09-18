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

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

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
    pub supports_budget: bool,
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
    pub work_dir: PathBuf,
    pub wall_timeout: Duration,
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
}

impl LaunchResult {
    /// A terminal result is missing when the process died without one:
    /// interrupted, not failed (SPEC §9).
    pub fn terminal_result_missing(&self) -> bool {
        self.timed_out || self.exit_code.is_none()
    }
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
) -> Result<(Option<i32>, String, String, bool), BackendError> {
    command.stdin(
        stdin_bytes
            .as_ref()
            .map_or(Stdio::null(), |_| Stdio::piped()),
    );
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| BackendError::Launch(format!("spawn: {e}")))?;

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

    let started = Instant::now();
    let timed_out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break false,
            Ok(None) => {}
            Err(e) => return Err(BackendError::Launch(format!("wait: {e}"))),
        }
        if started.elapsed() >= wall_timeout {
            let _ = kill_process_group(&mut child);
            break true;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let status = child
        .wait()
        .map_err(|e| BackendError::Launch(format!("final wait: {e}")))?;
    let stdout = stdout
        .join()
        .map_err(|_| BackendError::Launch("stdout reader panicked".into()))?;
    let stderr = stderr
        .join()
        .map_err(|_| BackendError::Launch("stderr reader panicked".into()))?;
    Ok((
        status.code(),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    ))
}

fn read_to_end(mut pipe: impl std::io::Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = std::io::Read::read_to_end(&mut pipe, &mut buffer);
    buffer
}

#[cfg(unix)]
fn kill_process_group(child: &mut std::process::Child) -> std::io::Result<()> {
    // The whole group: a harness that spawned its own children must not
    // survive the kill (SPEC §23 cancellation).
    let pgid = child.id() as i32;
    let result = unsafe { libc::kill(-pgid, libc::SIGKILL) };
    if result == -1 {
        child.kill()
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut std::process::Child) -> std::io::Result<()> {
    child.kill()
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
        let (exit, stdout, stderr, timed_out) = run_with_timeout(command, spec.wall_timeout, None)?;
        Ok(LaunchResult {
            dispatch_id: spec.dispatch_id.clone(),
            exit_code: exit,
            result_text: if timed_out {
                None
            } else {
                Some(stdout.clone())
            },
            stdout,
            stderr,
            timed_out,
            session_id: None,
            effective_model: Some(spec.model.clone()),
            usage: UsageReport::unknown(),
            worker_claims_blockage: false,
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
        let (exit, stdout, _, timed_out) =
            run_with_timeout(command, Duration::from_millis(300), None).expect("runs");
        assert!(timed_out, "must be marked interrupted by the wall clock");
        assert_eq!(exit, None);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            stdout.contains("start"),
            "output before the kill is kept: {stdout}"
        );
    }

    #[test]
    fn run_with_timeout_feeds_stdin_and_captures_output() {
        let mut command = Command::new("sh");
        command.args(["-c", "cat; echo processed"]);
        let (exit, stdout, stderr, timed_out) = run_with_timeout(
            command,
            Duration::from_secs(10),
            Some(b"prompt-bytes".to_vec()),
        )
        .expect("runs");
        assert_eq!(exit, Some(0));
        assert!(!timed_out);
        assert!(stdout.contains("prompt-bytes"));
        assert!(stdout.contains("processed"), "{stdout} {stderr}");
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
        };
        assert!(result.terminal_result_missing());
        let completed = LaunchResult {
            timed_out: false,
            exit_code: Some(0),
            ..result
        };
        assert!(!completed.terminal_result_missing());
    }
}
