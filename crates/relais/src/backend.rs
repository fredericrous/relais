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
use crate::procs::{Ended, RunError};

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
        match e {
            // The worker was handed something other than the prompt: the
            // launch failed, and the attempt is interrupted rather than
            // completed over a truncated task (audit V12).
            RunError::PromptWrite(io) => {
                Self::Launch(format!("the prompt did not reach the worker: {io}"))
            }
            other => Self::Process(other),
        }
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
    /// The value recorded in the context manifest, which is where a
    /// run says which of the two it was.
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

/// The variables a worker process keeps, by exact name. Everything else
/// is cleared, which is what removes `GIT_DIR`, `GIT_WORK_TREE`,
/// `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY` and
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES`: inherited from a rebase or a hook
/// shell they override the working directory, and the worker's `git
/// commit` lands in the user's repository instead of the owned worktree
/// (audit V4, the same variables `workspace::git_command` strips).
///
/// The list is what `claude -p` needs to start and to reach a provider,
/// read off Claude Code's own environment-variable and authentication
/// documentation (code.claude.com/docs/en/env-vars, /authentication,
/// /network-config) and checked against 2.1.278 with a cleared
/// environment. A variable that only tunes behaviour is deliberately
/// absent — the machine's policy decides those, not the operator's shell.
///
/// `USER` earns its place the hard way: on macOS, a session signed in
/// with `/login` keeps its credential in the Keychain, and without
/// `USER` the CLI reports "Not logged in · Please run /login" however
/// much of `HOME` and `PATH` it is given.
pub const WORKER_ENV_ALLOWLIST: &[&str] = &[
    // The machine, without which nothing runs.
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    // Windows spellings of the same thing. `SYSTEMROOT` is required for
    // Node's own crypto and socket startup.
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "USERPROFILE",
    "TEMP",
    "TMP",
    // Reaching the provider through a corporate network. Node does not
    // honour `SSL_CERT_FILE`; `NODE_EXTRA_CA_CERTS` is the documented
    // way to add a CA, and no variable that DISABLES verification is
    // passed through.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "NODE_EXTRA_CA_CERTS",
    // Google Cloud's own spellings, which carry no common prefix.
    "GCLOUD_PROJECT",
    "CLOUDSDK_CONFIG",
];

/// Credential families passed through by prefix: Anthropic's own
/// (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`,
/// `ANTHROPIC_CUSTOM_HEADERS`, the federation variables), Claude Code's
/// own configuration (`CLAUDE_CODE_USE_BEDROCK`,
/// `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CONFIG_DIR`), and the clouds
/// Claude Code can be pointed at (`AWS_*`, `GOOGLE_*`, `CLOUD_ML_REGION`,
/// `VERTEX_REGION_CLAUDE_*`). A prefix, because which member of a family
/// is set depends on how the operator signs in, and a worker that cannot
/// authenticate produces a blocked run nobody can act on.
pub const WORKER_ENV_PREFIXES: &[&str] = &[
    "ANTHROPIC_",
    "CLAUDE_CODE_",
    "CLAUDE_CONFIG_",
    "AWS_",
    "GOOGLE_",
    "CLOUD_ML_",
    "VERTEX_",
];

/// Names that match a passed-through prefix but are still removed: the
/// worker must not inherit relais's own authority or a budget override
/// the machine did not set.
pub const WORKER_ENV_DENIED: &[&str] = &[
    "CLAUDE_CODE_EXTRA_BUDGET",
    "RELAIS_CONFIG_DIR",
    "RELAIS_STATE_DIR",
    "RELAIS_CLAUDE_BIN",
];

/// The environment one dispatch runs with — the whole of it. A launch
/// clears the ambient environment and sets exactly these, so what the
/// worker inherits is a decision recorded in the context manifest rather
/// than whatever shell the operator happened to start relais from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchEnv {
    passed: Vec<(String, String)>,
}

impl LaunchEnv {
    /// Select the allowed variables out of an environment. Pure: the
    /// ambient environment is a parameter, so what a worker would
    /// inherit is testable without touching this process's own.
    pub fn from_ambient(ambient: &[(String, String)]) -> Self {
        let mut passed: Vec<(String, String)> = ambient
            .iter()
            .filter(|(name, _)| Self::is_allowed(name))
            .cloned()
            .collect();
        passed.sort();
        passed.dedup_by(|a, b| a.0 == b.0);
        Self { passed }
    }

    /// The same selection over this process's real environment: the one
    /// boundary call, made by the adapter at launch time.
    pub fn from_process_env() -> Self {
        Self::from_ambient(&std::env::vars().collect::<Vec<_>>())
    }

    /// Is this variable one a worker keeps?
    pub fn is_allowed(name: &str) -> bool {
        if WORKER_ENV_DENIED.contains(&name) {
            return false;
        }
        WORKER_ENV_ALLOWLIST.contains(&name)
            || WORKER_ENV_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
    }

    /// Name and value, for the launch itself.
    pub fn vars(&self) -> &[(String, String)] {
        &self.passed
    }

    /// The NAMES only — what the context manifest records. A value here
    /// is a credential; the manifest says which variables reached the
    /// worker, never what was in them.
    pub fn names(&self) -> Vec<String> {
        self.passed.iter().map(|(name, _)| name.clone()).collect()
    }
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
    /// Everything the worker process's environment will contain. The
    /// adapter clears the ambient environment and sets these; an empty
    /// one is a worker with no environment at all, never an inherited one.
    pub env: LaunchEnv,
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

/// What the harness said one dispatch cost. `inclusive` describes a
/// figure, so it exists only where a figure does: the struct this
/// replaces could hold `cost: None, inclusive: true`, which says
/// nothing, and `cost: Some(..), cost_completeness: Unknown`, which the
/// settlement path silently read as no cost at all.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Cost {
    /// A total the harness reported for this dispatch.
    Reported {
        micros: MicroUsd,
        /// True when the total already includes descendants (an
        /// inclusive parent: SPEC §11), so it is never summed with them.
        inclusive: bool,
    },
    /// The harness reported no cost. Unknown, never zero (SPEC §11).
    #[default]
    Unknown,
}

impl Cost {
    /// The figure, when one was reported.
    pub fn micros(self) -> Option<MicroUsd> {
        match self {
            Self::Reported { micros, .. } => Some(micros),
            Self::Unknown => None,
        }
    }

    /// How complete the figure is: what the harness reported is actual,
    /// and what it did not report is unknown rather than zero.
    pub fn completeness(self) -> CostCompleteness {
        match self {
            Self::Reported { .. } => CostCompleteness::Actual,
            Self::Unknown => CostCompleteness::Unknown,
        }
    }

    /// Does the figure already cover descendants? An unreported cost
    /// covers nothing.
    pub fn inclusive(self) -> bool {
        match self {
            Self::Reported { inclusive, .. } => inclusive,
            Self::Unknown => false,
        }
    }
}

/// Provider-reported usage as extracted from one terminal result.
/// Everything absent is `None` — unknown, never zero (SPEC §11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UsageReport {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub cost: Cost,
}

impl UsageReport {
    /// A dispatch whose usage the harness did not report at all.
    pub fn unknown() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchResult {
    pub dispatch_id: String,
    /// How the backend process ended: a status, the wall clock, a
    /// cancellation or a signal. Exactly one of them.
    pub ended: Ended,
    pub stdout: String,
    pub stderr: String,
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
        !self.ended.succeeded() || self.result_text.is_none()
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

/// What a dispatch established about the model that ran. Four answers,
/// not two: a harness that reports no model leaves the question OPEN,
/// and reading that as agreement is how an unreported substitution used
/// to pass (audit V3); a substitution the machine reviewed in advance is
/// distinct from one nobody approved, so accepting it cannot be mistaken
/// for the harness having matched the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelVerification {
    /// The harness named a model that satisfies the route's request.
    Matches,
    /// It named a different one, and `machine.toml` names this exact
    /// substitution in advance (SPEC §6): accepted, not refused. Still
    /// two distinct identities — the route's request and the model that
    /// actually ran — carried separately so both reach the dispatch, the
    /// usage event and the receipt.
    Approved {
        requested: String,
        effective: String,
    },
    /// It named a different one, and nothing approved it: an unapproved
    /// substitution (SPEC §6).
    Substituted {
        requested: String,
        effective: String,
    },
    /// It named none. The run cannot claim the requested route was
    /// tested, so dispatch stops here rather than on an assumption.
    Unverified { requested: String },
}

/// Check the model the harness reported against the one the route asked
/// for. `None` is the harness reporting nothing, which is a gap, never a
/// match. `approved_substitutions` is machine-owned (SPEC §6: accepting a
/// costlier model is a spending decision a repo must not be able to
/// widen) — see [`crate::policy::RoutingSettings::approved_substitutions`].
pub fn verify_model(
    requested: &str,
    effective: Option<&str>,
    approved_substitutions: &[crate::policy::ApprovedSubstitution],
) -> ModelVerification {
    match effective {
        Some(effective) if model_matches(requested, effective) => ModelVerification::Matches,
        Some(effective)
            if approved_substitutions
                .iter()
                .any(|approval| approval.approves(requested, effective)) =>
        {
            ModelVerification::Approved {
                requested: requested.to_string(),
                effective: effective.to_string(),
            }
        }
        Some(effective) => ModelVerification::Substituted {
            requested: requested.to_string(),
            effective: effective.to_string(),
        },
        None => ModelVerification::Unverified {
            requested: requested.to_string(),
        },
    }
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
            ended: Ended::TimedOut,
            stdout: String::new(),
            stderr: String::new(),
            result_text: None,
            session_id: None,
            effective_model: None,
            usage: UsageReport::unknown(),
            worker_claims_blockage: false,
            permission_denials: Vec::new(),
            failure_detail: None,
        };
        assert!(result.terminal_result_missing());
        let completed = LaunchResult {
            ended: Ended::Exited(0),
            result_text: Some("DONE".into()),
            ..result.clone()
        };
        assert!(!completed.terminal_result_missing());
        // A non-zero exit, a signal, a cancellation or an unreadable
        // result is not a completed attempt with an empty candidate; it
        // is a missing result.
        let usage_error = LaunchResult {
            ended: Ended::Exited(1),
            result_text: Some(String::new()),
            ..result.clone()
        };
        assert!(usage_error.terminal_result_missing());
        for ended in [Ended::Signalled, Ended::Cancelled] {
            let killed = LaunchResult {
                ended,
                result_text: Some("DONE".into()),
                ..result.clone()
            };
            assert!(killed.terminal_result_missing());
        }
        let unreadable = LaunchResult {
            ended: Ended::Exited(0),
            result_text: None,
            ..result
        };
        assert!(unreadable.terminal_result_missing());
    }

    /// `inclusive` described a cost, so it meant nothing without one,
    /// and a reported figure with `Unknown` completeness was dropped
    /// from the settlement without a word. Neither state exists now.
    #[test]
    fn an_unreported_cost_carries_no_inclusiveness_and_no_completeness() {
        assert_eq!(Cost::Unknown.micros(), None);
        assert!(!Cost::Unknown.inclusive());
        assert_eq!(Cost::Unknown.completeness(), CostCompleteness::Unknown);

        let reported = Cost::Reported {
            micros: MicroUsd::from_micros(12_300),
            inclusive: true,
        };
        assert_eq!(reported.micros(), Some(MicroUsd::from_micros(12_300)));
        assert!(reported.inclusive());
        assert_eq!(reported.completeness(), CostCompleteness::Actual);
        assert_eq!(UsageReport::unknown().cost, Cost::Unknown);
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

    /// V3: an unreported model is an open question. It used to be
    /// answered by echoing the request back, which made the substitution
    /// check unable to fail.
    #[test]
    fn an_unreported_model_is_unverified_not_a_match() {
        assert_eq!(
            verify_model("sonnet", Some("claude-sonnet-5"), &[]),
            ModelVerification::Matches
        );
        assert_eq!(
            verify_model("sonnet", Some("claude-haiku-4-5"), &[]),
            ModelVerification::Substituted {
                requested: "sonnet".into(),
                effective: "claude-haiku-4-5".into(),
            }
        );
        assert_eq!(
            verify_model("sonnet", None, &[]),
            ModelVerification::Unverified {
                requested: "sonnet".into()
            }
        );
    }

    /// A substitution `machine.toml` names in advance is accepted rather
    /// than refused, but stays a distinct answer from a genuine match —
    /// both identities survive. Anything not in the list is still an
    /// unapproved substitution, list or no list.
    #[test]
    fn a_listed_substitution_is_approved_and_an_unlisted_one_still_is_not() {
        let approved = vec![crate::policy::ApprovedSubstitution {
            requested: "sonnet".into(),
            effective: "claude-haiku-4-5".into(),
            note: None,
        }];
        assert_eq!(
            verify_model("sonnet", Some("claude-haiku-4-5"), &approved),
            ModelVerification::Approved {
                requested: "sonnet".into(),
                effective: "claude-haiku-4-5".into(),
            }
        );
        assert_eq!(
            verify_model("sonnet", Some("claude-fable-5-1"), &approved),
            ModelVerification::Substituted {
                requested: "sonnet".into(),
                effective: "claude-fable-5-1".into(),
            }
        );
    }

    /// V4: the worker's environment is chosen, not inherited. The git
    /// variables that redirect a commit into the user's repository are
    /// gone because nothing but the allowlist survives.
    #[test]
    fn a_worker_keeps_the_allowlist_and_nothing_else() {
        let ambient: Vec<(String, String)> = [
            ("PATH", "/usr/bin"),
            ("HOME", "/home/dev"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("CLAUDE_CODE_USE_BEDROCK", "1"),
            ("AWS_PROFILE", "work"),
            ("HTTPS_PROXY", "http://proxy:3128"),
            ("GIT_DIR", "/elsewhere/.git"),
            ("GIT_INDEX_FILE", "/elsewhere/.git/index"),
            ("GIT_WORK_TREE", "/elsewhere"),
            ("CLAUDE_CODE_EXTRA_BUDGET", "999"),
            ("RELAIS_STATE_DIR", "/run/relais"),
            ("MY_SECRET_TOKEN", "hunter2"),
        ]
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();

        let env = LaunchEnv::from_ambient(&ambient);
        let names = env.names();
        for kept in [
            "PATH",
            "HOME",
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_USE_BEDROCK",
            "AWS_PROFILE",
            "HTTPS_PROXY",
        ] {
            assert!(names.contains(&kept.to_string()), "{kept} in {names:?}");
        }
        for removed in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_WORK_TREE",
            "CLAUDE_CODE_EXTRA_BUDGET",
            "RELAIS_STATE_DIR",
            "MY_SECRET_TOKEN",
        ] {
            assert!(
                !names.contains(&removed.to_string()),
                "{removed} must not reach the worker: {names:?}"
            );
        }
        assert_eq!(
            env.vars().len(),
            names.len(),
            "every passed variable carries its value"
        );
        assert!(
            LaunchEnv::from_ambient(&[]).names().is_empty(),
            "an empty environment passes nothing, rather than falling back to the ambient one"
        );
    }
}
