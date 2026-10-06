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

use crate::catalog::{Admission, EffortSet};
use crate::money::{CostCompleteness, CostKind, MicroUsd};
use crate::policy::EffortId;
use crate::procs::{Ended, RunError};

#[derive(Debug)]
pub enum BackendError {
    MissingBinary(String),
    Launch(String),
    /// The child could not be supervised: it would not spawn, the wait
    /// failed, or a pipe reader was lost. Never a worker's doing.
    Process(RunError),
    Unsupported(&'static str),
    /// The dispatch asked for an effort the installed harness does not
    /// accept. An effort is never dropped silently: the run is blocked.
    EffortUnsupported {
        effort: EffortId,
        model: String,
        harness_version: String,
    },
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBinary(name) => write!(f, "required binary `{name}` is not available"),
            Self::Launch(detail) => write!(f, "backend launch failed: {detail}"),
            Self::Process(e) => write!(f, "backend launch failed: {e}"),
            Self::Unsupported(what) => write!(f, "backend does not support {what}"),
            Self::EffortUnsupported {
                effort,
                model,
                harness_version,
            } => write!(
                f,
                "the harness (version {harness_version}) does not accept effort `{effort}` for \
                 model `{model}`; run `relais doctor` for what it accepts"
            ),
        }
    }
}

impl std::error::Error for BackendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Process(e) => Some(e),
            Self::MissingBinary(_)
            | Self::Launch(_)
            | Self::Unsupported(_)
            | Self::EffortUnsupported { .. } => None,
        }
    }
}

impl BackendError {
    /// Why the run is blocked, as the code `plan`, `run` and `explain`
    /// print: an effort the harness refuses is its own code, not a
    /// harness that is missing.
    pub fn block_code(&self) -> crate::policy::BlockCode {
        use crate::policy::BlockCode;
        match self {
            Self::EffortUnsupported { .. } => BlockCode::EffortUnsupported,
            Self::MissingBinary(_) | Self::Launch(_) | Self::Process(_) | Self::Unsupported(_) => {
                BlockCode::BackendUnavailable
            }
        }
    }
}

/// The adapter's rule for a requested effort: passed on when the harness
/// accepts it, refused (typed, naming effort, model and harness version)
/// when its CLI fact is `Unsupported` or `Known` without it, and passed on
/// when the CLI fact is `Unknown` — the carve-out for the effort the repo
/// policy explicitly configures (SPEC §20). Never dropped.
pub fn check_effort(
    accepted: &EffortSet,
    effort: &EffortId,
    model: &str,
    harness_version: Option<&str>,
) -> Result<(), BackendError> {
    match accepted.admission_of(effort) {
        Admission::Accepted => Ok(()),
        // holds-until: correctly scoped only while routing never changes the
        // effort, so what is passed on is always the one the repo policy
        // configured; it ends when P2/P3 route-time admissibility blocks
        // effort changes under an unknown catalog
        // (rule change.a-deferral-names-its-ceiling).
        Admission::Unverifiable => Ok(()),
        Admission::Refused => Err(BackendError::EffortUnsupported {
            effort: effort.clone(),
            model: model.to_string(),
            harness_version: harness_version.unwrap_or("unknown").to_string(),
        }),
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
    /// The CLI-accepted efforts, read from the harness (never assumed):
    /// a known set, established absent, or unknown.
    pub accepted_efforts: EffortSet,
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

    /// This environment with `name` set to `value`, replacing any earlier
    /// value.
    pub fn with_var(&self, name: &str, value: &str) -> Self {
        let mut passed = self.without_var(name).passed;
        passed.push((name.to_string(), value.to_string()));
        passed.sort();
        Self { passed }
    }

    /// This environment without `name`, whatever the ambient environment
    /// carried.
    pub fn without_var(&self, name: &str) -> Self {
        Self {
            passed: self
                .passed
                .iter()
                .filter(|(passed, _)| passed != name)
                .cloned()
                .collect(),
        }
    }

    /// The NAMES only — what the context manifest records. A value here
    /// is a credential; the manifest says which variables reached the
    /// worker, never what was in them.
    pub fn names(&self) -> Vec<String> {
        self.passed.iter().map(|(name, _)| name.clone()).collect()
    }
}

/// The tool set a launch asks the harness for (`--tools`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSet {
    /// What the launch mode has always asked for: the sandbox worker's
    /// tools, and nothing at all in allowlist mode.
    ModeDefault,
    /// `Read,Grep,Glob` in either mode: a launch that can look and never
    /// write or run anything (SPEC §10, the report review).
    ReadOnly,
}

/// How a launch is presented to the person running relais (SPEC §23).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Presentation {
    /// A child process relais starts and supervises: every launch but a
    /// native worker's.
    #[default]
    Headless,
    /// A subagent the parent Claude Code session spawns on relais's
    /// behalf, so Claude Code renders it.
    Native,
}

/// One model dispatch. The prompt travels via stdin; arguments are an
/// argv array; the working directory is the owned task worktree.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub dispatch_id: String,
    pub prompt: String,
    pub model: String,
    pub effort: Option<EffortId>,
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
    /// The OS-sandbox launch, for a worker on a machine with `[sandbox]`
    /// on (SPEC §8). `None` is the allowlist launch, and every dispatch
    /// that is not a worker's.
    pub sandbox: Option<SandboxLaunch>,
    /// The tools the harness is asked to expose.
    pub tools: ToolSet,
    /// Who runs the launch: relais's own child process, or a native
    /// subagent of the parent session.
    pub presentation: Presentation,
}

/// What a sandboxed launch adds: the whole `--settings` JSON (sandbox,
/// credential floor and permissions in one document), the scratch
/// directory the worker writes its own output to, and the short path the
/// worker's `CLAUDE_CODE_TMPDIR` names: a symlink to the scratch on unix (a
/// Unix socket path is capped near 104 bytes, and the scratch path spends
/// most of it), the scratch itself elsewhere. Claude Code appends
/// `/claude-<uid>` to it, so sandboxed Bash's `TMPDIR` lands in the scratch.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxLaunch {
    pub settings: serde_json::Value,
    pub scratch_dir: PathBuf,
    pub tmp_link: PathBuf,
}

/// The variable Claude Code honours as the base of its own temp dir
/// (`<it>/claude-<uid>`, which sandboxed Bash gets as `TMPDIR`). It never
/// passes the launch's own `TMPDIR` on, and falls back to `/tmp/claude-<uid>`
/// without a word when the path is too long.
pub const CLAUDE_TMPDIR: &str = "CLAUDE_CODE_TMPDIR";

/// The environment variable that makes Claude Code strip provider
/// credentials from the subprocesses it starts. Allowlist mode sets it.
/// Sandbox mode must not: measured on 2.1.285, it disables the sandbox's
/// auto-allow, and the `credentials.envVars` deny protects the environment
/// there instead.
pub const SUBPROCESS_ENV_SCRUB: &str = "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB";

/// The environment one WORKER dispatch runs with: `base` plus what the
/// mode adds. Sandbox mode points `CLAUDE_CODE_TMPDIR` at the short link to
/// the scratch (and leaves `TMPDIR` as `base` has it) and carries no scrub
/// (even one the ambient environment brought); allowlist
/// mode sets the scrub.
pub fn worker_launch_env(base: &LaunchEnv, sandbox: Option<&SandboxLaunch>) -> LaunchEnv {
    worker_env_with(base, sandbox.map(|launch| launch.tmp_link.as_path()))
}

/// [`worker_launch_env`] from the `CLAUDE_CODE_TMPDIR` path alone:
/// `Some` is sandbox mode. The context manifest records the worker env by NAME
/// before any attempt has its own scratch directory, and building it
/// here, not from `base`, keeps the recorded names and the launched ones
/// one fact.
pub fn worker_env_with(base: &LaunchEnv, claude_tmpdir: Option<&std::path::Path>) -> LaunchEnv {
    match claude_tmpdir {
        Some(dir) => base
            .without_var(SUBPROCESS_ENV_SCRUB)
            .with_var(CLAUDE_TMPDIR, &dir.to_string_lossy()),
        None => base.with_var(SUBPROCESS_ENV_SCRUB, "1"),
    }
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
    /// A figure relais computed from the usage it read (tokens times the
    /// machine's price table), not one the harness reported: what the
    /// same usage would cost at API rates.
    Estimated { micros: MicroUsd },
    /// The harness reported no cost. Unknown, never zero (SPEC §11).
    #[default]
    Unknown,
}

impl Cost {
    /// The figure, when one was reported.
    pub fn micros(self) -> Option<MicroUsd> {
        match self {
            Self::Reported { micros, .. } | Self::Estimated { micros } => Some(micros),
            Self::Unknown => None,
        }
    }

    /// What kind of money the figure is, as the ledger books it.
    pub fn kind(self) -> CostKind {
        match self {
            Self::Reported { .. } | Self::Unknown => CostKind::ApiSpend,
            Self::Estimated { .. } => CostKind::EstimatedApiEquivalent,
        }
    }

    /// How complete the figure is: what the harness reported is actual,
    /// and what it did not report is unknown rather than zero.
    pub fn completeness(self) -> CostCompleteness {
        match self {
            Self::Reported { .. } => CostCompleteness::Actual,
            Self::Estimated { .. } => CostCompleteness::Estimated,
            Self::Unknown => CostCompleteness::Unknown,
        }
    }

    /// Does the figure already cover descendants? An unreported cost
    /// covers nothing.
    pub fn inclusive(self) -> bool {
        match self {
            Self::Reported { inclusive, .. } => inclusive,
            Self::Estimated { .. } | Self::Unknown => false,
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

/// One tool call the harness refused the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionDenial {
    /// The tool, and for Bash the command: `Edit`, `Bash(git diff)`.
    pub entry: String,
    /// The refused call's id in the transcript, when the harness named it.
    pub tool_use_id: Option<String>,
}

impl PermissionDenial {
    pub fn new(entry: impl Into<String>, tool_use_id: Option<&str>) -> Self {
        Self {
            entry: entry.into(),
            tool_use_id: tool_use_id.map(str::to_string),
        }
    }

    /// The tool's name, without a Bash command.
    pub fn tool_name(&self) -> &str {
        self.entry.split('(').next().unwrap_or(&self.entry)
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
    pub permission_denials: Vec<PermissionDenial>,
    /// Why the harness ended without a usable result — its stderr, or
    /// the error it reported — for the interrupted transition's evidence.
    #[serde(default)]
    pub failure_detail: Option<String>,
    /// The transcript message ids this attempt's usage was booked from:
    /// only a native worker's, empty for every other launch. The runner
    /// records them so `usage import` does not count them again (SPEC §11).
    #[serde(default)]
    pub booked_message_ids: Vec<String>,
    /// Why some of those messages could not be priced, one line per model
    /// and reason (`orchestration::unpriced_reason`): only a native
    /// worker's, empty when every booked message priced.
    #[serde(default)]
    pub unpriced: Vec<String>,
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

/// One probe session (SPEC §8, `relais doctor --verify-sandbox`): `spec`
/// launched exactly as a worker is, except that the harness streams its
/// transcript. Returns that stream (one JSON record per line).
pub trait ProbeLauncher {
    fn stream(&self, spec: &LaunchSpec) -> Result<String, BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Fact;

    fn effort(name: &str) -> EffortId {
        EffortId::parse(name).expect("a valid effort identifier")
    }

    fn ambient() -> LaunchEnv {
        LaunchEnv::from_ambient(&[
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("TMPDIR".to_string(), "/var/tmp".to_string()),
            (SUBPROCESS_ENV_SCRUB.to_string(), "0".to_string()),
        ])
    }

    fn value_of(env: &LaunchEnv, name: &str) -> Option<String> {
        env.vars()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    /// The manifest names the env the worker is LAUNCHED with, per mode:
    /// the names `worker_env_with` gives before any attempt exists are the
    /// names `worker_launch_env` gives at dispatch.
    #[test]
    fn the_recorded_worker_env_names_are_the_launched_ones() {
        let base = ambient();
        assert_eq!(
            worker_env_with(&base, None).names(),
            worker_launch_env(&base, None).names()
        );
        let launch = SandboxLaunch {
            settings: serde_json::json!({}),
            scratch_dir: PathBuf::from("/state/runs/r/attempts/1/scratch"),
            tmp_link: PathBuf::from("/tmp/rl-0a1b2c3d"),
        };
        let recorded = worker_env_with(&base, Some(std::path::Path::new("/state/runs/r/attempts")));
        assert_eq!(
            recorded.names(),
            worker_launch_env(&base, Some(&launch)).names()
        );
        assert!(!recorded
            .names()
            .iter()
            .any(|name| name == SUBPROCESS_ENV_SCRUB));
        assert!(worker_env_with(&base, None)
            .names()
            .iter()
            .any(|name| name == SUBPROCESS_ENV_SCRUB));
    }

    #[test]
    fn allowlist_workers_run_with_the_credential_scrub() {
        let env = worker_launch_env(&ambient(), None);
        assert_eq!(value_of(&env, SUBPROCESS_ENV_SCRUB).as_deref(), Some("1"));
        assert_eq!(value_of(&env, "TMPDIR").as_deref(), Some("/var/tmp"));
    }

    #[test]
    fn sandboxed_workers_get_the_short_link_as_claude_tmpdir_and_no_scrub() {
        let launch = SandboxLaunch {
            settings: serde_json::json!({}),
            scratch_dir: PathBuf::from("/state/runs/r/attempts/1/scratch"),
            tmp_link: PathBuf::from("/tmp/rl-0a1b2c3d"),
        };
        let env = worker_launch_env(&ambient(), Some(&launch));
        assert_eq!(
            value_of(&env, CLAUDE_TMPDIR).as_deref(),
            Some("/tmp/rl-0a1b2c3d")
        );
        assert_eq!(
            value_of(&env, "TMPDIR").as_deref(),
            Some("/var/tmp"),
            "the launch's own TMPDIR is left as the base env has it"
        );
        assert_eq!(
            value_of(&env, SUBPROCESS_ENV_SCRUB),
            None,
            "the scrub disables the sandbox's auto-allow, even when the ambient env has it"
        );
        assert_eq!(value_of(&env, "PATH").as_deref(), Some("/usr/bin"));
    }

    #[test]
    fn an_effort_is_refused_by_name_unless_the_cli_fact_cannot_say() {
        let known = Fact::Known(vec![effort("low"), effort("high")]);
        assert!(check_effort(&known, &effort("high"), "sonnet", Some("2.1.284")).is_ok());
        assert!(check_effort(&Fact::Unknown, &effort("ultra"), "sonnet", None).is_ok());
        for refused in [known, Fact::Unsupported] {
            let err = check_effort(&refused, &effort("ultra"), "sonnet", Some("2.1.284"))
                .expect_err("not accepted");
            assert!(
                matches!(
                    &err,
                    BackendError::EffortUnsupported { effort, model, harness_version }
                        if effort.as_str() == "ultra"
                            && model == "sonnet"
                            && harness_version == "2.1.284"
                ),
                "{err:?}"
            );
            assert_eq!(
                err.block_code(),
                crate::policy::BlockCode::EffortUnsupported
            );
        }
        let unversioned =
            check_effort(&Fact::Unsupported, &effort("low"), "m", None).expect_err("refused");
        assert!(unversioned.to_string().contains("version unknown"));
        assert_eq!(
            BackendError::Launch("x".into()).block_code(),
            crate::policy::BlockCode::BackendUnavailable
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
            booked_message_ids: Vec::new(),
            unpriced: Vec::new(),
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

    #[test]
    fn a_stored_result_without_booked_message_ids_still_parses() {
        let mut stored = serde_json::to_value(LaunchResult {
            dispatch_id: "disp-1".into(),
            ended: Ended::Exited(0),
            stdout: String::new(),
            stderr: String::new(),
            result_text: None,
            session_id: None,
            effective_model: None,
            usage: UsageReport::unknown(),
            worker_claims_blockage: false,
            permission_denials: Vec::new(),
            failure_detail: None,
            booked_message_ids: vec!["m1".into()],
            unpriced: Vec::new(),
        })
        .expect("serializes");
        stored
            .as_object_mut()
            .expect("an object")
            .remove("booked_message_ids");
        let parsed: LaunchResult = serde_json::from_value(stored).expect("an old result parses");
        assert!(parsed.booked_message_ids.is_empty());
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

        let estimated = Cost::Estimated {
            micros: MicroUsd::from_micros(900),
        };
        assert_eq!(estimated.micros(), Some(MicroUsd::from_micros(900)));
        assert!(!estimated.inclusive());
        assert_eq!(estimated.completeness(), CostCompleteness::Estimated);
        assert_eq!(estimated.kind(), CostKind::EstimatedApiEquivalent);
        assert_eq!(reported.kind(), CostKind::ApiSpend);
        assert_eq!(Cost::Unknown.kind(), CostKind::ApiSpend);
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
