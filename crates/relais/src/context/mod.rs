//! Context packages (SPEC §7, §18).
//!
//! The manifest carries contract hash, base SHA, policy hash, tool
//! versions, source paths and fingerprints, architecture evidence and
//! the verification plan. Workers get the objective, acceptance criteria,
//! necessary constraints, a small set of entry points and exact failure
//! evidence — large files and logs are referenced by path and range.
//! Required constraints that exceed the context budget are a sizing
//! problem, never silently truncated. aval verdicts (active, undecided,
//! contradiction, retired, unknown) and aval tool failures stay distinct;
//! only a contradiction blocks affected work, and a missing answer gates
//! only tasks that depend on it.

pub mod guidance;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

use crate::contract::TaskContract;
use crate::ids::sha256_hex;
use crate::policy::RepoPolicy;
use crate::workspace::Git;
pub use guidance::{
    Guidance, GuidanceGap, GuidanceSource, RankedKey, Relevant, RelevantQuery, RuleRef, Rules,
    GUIDANCE_BUDGET_BYTES,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvalVerdict {
    Active {
        adr: String,
        /// What was decided, in the record's words — the constraint the
        /// worker is handed (SPEC §7: "necessary constraints").
        #[serde(default)]
        choice: Option<String>,
        #[serde(default)]
        reason: Option<String>,
    },
    Undecided,
    Contradiction {
        heads: usize,
    },
    Retired {
        adr: String,
    },
    Unknown,
    /// Exit 1/2/3 from aval: a tool or corpus failure, NOT a verdict.
    /// Kept distinct so a broken corpus can never read as a decision
    /// (SPEC §7).
    ToolFailure {
        exit: i32,
        detail: String,
    },
}

/// Parse `aval resolve <key> --json` output (SEMANTICS.md v1.2.0 is the
/// normative contract): the payload carries `state` and `exit`; an error
/// envelope is `{"ok": false, "exit": N, "error": "..."}`.
pub fn parse_aval_output(exit_code: i32, stdout: &str) -> AvalVerdict {
    let value: serde_json::Value = match serde_json::from_str(stdout.trim()) {
        Ok(value) => value,
        Err(_) => {
            return AvalVerdict::ToolFailure {
                exit: exit_code,
                detail: format!("unparseable aval output: {stdout:?}"),
            }
        }
    };
    if value.get("ok") == Some(&serde_json::Value::Bool(false)) {
        let detail = value
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("aval reported a tool failure")
            .to_string();
        let exit = value
            .get("exit")
            .and_then(|e| e.as_i64())
            .unwrap_or(exit_code as i64) as i32;
        return AvalVerdict::ToolFailure { exit, detail };
    }
    match value.get("state").and_then(|s| s.as_str()) {
        Some("active") => AvalVerdict::Active {
            adr: value
                .get("adr")
                .and_then(|a| a.as_str())
                .unwrap_or("?")
                .to_string(),
            choice: value
                .get("choice")
                .and_then(|c| c.as_str())
                .map(str::to_string),
            reason: value
                .get("reason")
                .and_then(|r| r.as_str())
                .map(str::to_string),
        },
        Some("undecided") => AvalVerdict::Undecided,
        Some("contradiction") => AvalVerdict::Contradiction {
            heads: value
                .get("heads")
                .and_then(|h| h.as_array())
                .map(|heads| heads.len())
                .unwrap_or(0),
        },
        Some("retired") => AvalVerdict::Retired {
            adr: value
                .get("adr")
                .and_then(|a| a.as_str())
                .unwrap_or("?")
                .to_string(),
        },
        Some("unknown") => AvalVerdict::Unknown,
        other => AvalVerdict::ToolFailure {
            exit: exit_code,
            detail: format!("aval emitted an unrecognized state: {other:?}"),
        },
    }
}

/// What the decision corpus currently says. The interface belongs to
/// relais — the runner asks a `DecisionResolver`, never `aval` — so a
/// test supplies verdicts without a corpus on disk and a second corpus
/// tool would cost nobody a change of import (boundaries.own-the-interface).
pub trait DecisionResolver {
    /// Resolve one decision key, optionally within a scope.
    fn resolve(&self, key: &str, scope: Option<&str>) -> AvalVerdict;

    /// The active constraint rules, for the index and the reviewer. A
    /// failure is a gap, never a verdict: guidance does not block a run.
    /// The default knows none, so a resolver that only answers keys
    /// (a closure, a test) needs nothing more.
    fn constraint_rules(&self) -> Result<Rules, GuidanceGap> {
        Ok(Rules::default())
    }

    /// aval's advisory ranking for a query. Same failure policy.
    fn relevant(&self, _query: &RelevantQuery) -> Result<Relevant, GuidanceGap> {
        Ok(Relevant::default())
    }
}

/// A closure is a resolver: the shape tests and the CLI both already use.
impl<F> DecisionResolver for F
where
    F: Fn(&str, Option<&str>) -> AvalVerdict,
{
    fn resolve(&self, key: &str, scope: Option<&str>) -> AvalVerdict {
        self(key, scope)
    }
}

/// The `aval` binary on this machine — the one place it is spawned.
pub struct AvalCli {
    root: PathBuf,
    cancel: Option<Arc<AtomicBool>>,
    /// The constraint index is the same for every package of a run, so it
    /// is fetched once per `AvalCli` (one per run) and shared.
    rules: OnceLock<Result<Rules, GuidanceGap>>,
}

impl AvalCli {
    /// Resolve decisions for the corpus rooted at `root`.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            cancel: None,
            rules: OnceLock::new(),
        }
    }

    /// The same, with the run's cancel flag: a corpus tool that hangs
    /// stops when the run does (audit V6).
    pub fn watching(root: PathBuf, cancel: Arc<AtomicBool>) -> Self {
        Self {
            root,
            cancel: Some(cancel),
            rules: OnceLock::new(),
        }
    }
}

impl DecisionResolver for AvalCli {
    fn resolve(&self, key: &str, scope: Option<&str>) -> AvalVerdict {
        aval_resolve(&self.root, key, scope, self.cancel.as_deref())
    }

    fn constraint_rules(&self) -> Result<Rules, GuidanceGap> {
        self.rules
            .get_or_init(|| {
                guidance::run_aval(
                    "aval",
                    &guidance::rules_args(),
                    &self.root,
                    GuidanceSource::Index,
                    crate::tooling::GUIDANCE_TIMEOUT,
                    self.cancel.as_deref(),
                )
                .and_then(|stdout| guidance::parse_rules_output(&stdout))
            })
            .clone()
    }

    fn relevant(&self, query: &RelevantQuery) -> Result<Relevant, GuidanceGap> {
        guidance::run_aval(
            "aval",
            &guidance::relevant_args(query),
            &self.root,
            GuidanceSource::Relevant,
            crate::tooling::GUIDANCE_TIMEOUT,
            self.cancel.as_deref(),
        )
        .and_then(|stdout| guidance::parse_relevant_output(&stdout))
    }
}

/// Invoke `aval resolve KEY --json` in a repository. The exit-code contract
/// is part of aval's stable interface: 0 active, 4 undecided,
/// 5 contradiction, 6 retired, 7 unknown, 1/2/3 tool or corpus failure.
///
/// Bounded and cancellable: a corpus tool that never answers is a tool
/// failure, not a run that waits out its wall clock (audit V6).
pub fn aval_resolve(
    repo_dir: &Path,
    key: &str,
    scope: Option<&str>,
    cancel: Option<&AtomicBool>,
) -> AvalVerdict {
    // aval takes no `--` separator, so a key that looks like a flag would
    // be parsed as one. Such a key is a mapping error, reported as the
    // tool failure it would otherwise become in a less legible form.
    if key.starts_with('-') || key.trim().is_empty() {
        return AvalVerdict::ToolFailure {
            exit: 2,
            detail: format!("decision key `{key}` is not a key aval can be asked for"),
        };
    }
    let mut command = std::process::Command::new("aval");
    command
        .arg("resolve")
        .arg(key)
        .arg("--json")
        .current_dir(repo_dir);
    if let Some(scope) = scope {
        command.arg("--scope").arg(scope);
    }
    match crate::procs::run_with_timeout(command, crate::tooling::PROBE_TIMEOUT, None, cancel) {
        Ok(end) => match end.ended {
            crate::procs::Ended::Exited(code) => parse_aval_output(code, &end.stdout),
            other => AvalVerdict::ToolFailure {
                exit: -1,
                detail: format!("aval did not answer: {}", other.describe()),
            },
        },
        Err(e) => AvalVerdict::ToolFailure {
            exit: -1,
            detail: format!("aval could not be launched: {e}"),
        },
    }
}

/// Architecture evidence assembled for a contract: explicit keys from the
/// contract plus repo path-to-key mappings whose paths the declared scope
/// could touch. Unlisted keys never block unrelated work (SPEC §7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchitectureEvidence {
    pub resolved: Vec<(String, AvalVerdict)>,
}

/// Why a context package could not be assembled. Sizing is a distinct
/// outcome from missing evidence (SPEC §7).
#[derive(Debug, Clone, PartialEq)]
pub enum ContextError {
    /// Required constraints exceed the configured budget. Never
    /// truncated, never silently dropped.
    SizingProblem {
        required_bytes: usize,
        budget_bytes: usize,
    },
    /// A contradiction on a key the task depends on blocks affected work.
    ContradictionBlocked {
        key: String,
        heads: usize,
    },
    /// The task depends on a key aval cannot currently answer.
    NeedsDecision {
        key: String,
        verdict: AvalVerdict,
    },
    ToolFailure {
        key: String,
        detail: String,
    },
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizingProblem {
                required_bytes,
                budget_bytes,
            } => write!(
                f,
                "sizing problem: required context is {required_bytes} bytes against a {budget_bytes}-byte budget; raise the budget or split the task"
            ),
            Self::ContradictionBlocked { key, heads } => write!(
                f,
                "aval reports a contradiction on `{key}` ({heads} heads); affected work is blocked until it is resolved"
            ),
            Self::NeedsDecision { key, verdict } => write!(
                f,
                "the task depends on `{key}` but aval's answer is {verdict:?}; needs_decision"
            ),
            Self::ToolFailure { key, detail } => {
                write!(f, "aval tool failure while resolving `{key}`: {detail}")
            }
        }
    }
}

impl std::error::Error for ContextError {}

/// A source file's identity at the base revision: git's own blob id,
/// which is a content hash the repository already computed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub path: String,
    pub blob: String,
}

/// How many files a context package fingerprints at most; beyond that
/// the manifest names the hint and says it was truncated rather than
/// growing without bound.
pub const FINGERPRINT_CAP: usize = 400;

/// One read hint's outcome: the blobs it resolved to, and whether the
/// cap cut the listing short. Truncation is a FLAG, not a fingerprint
/// row with a sentence where a path belongs — a consumer reading the
/// manifest had no way to tell that row from a file (style, audit §2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HintFingerprints {
    pub hint: String,
    pub files: Vec<FileFingerprint>,
    /// `FINGERPRINT_CAP` was reached: the hint points at more than the
    /// manifest records.
    pub truncated: bool,
}

/// A read hint the repository cannot resolve at the base revision — a
/// path that does not exist there, or a `git` that would not answer.
/// The worker would be pointed at nothing, so this is a preflight
/// problem, not an empty fingerprint list (audit V15).
#[derive(Debug, Clone, PartialEq)]
pub struct UnresolvableHint {
    pub hint: String,
    pub detail: String,
}

/// Every read hint that could not be resolved, named.
#[derive(Debug, Clone, PartialEq)]
pub struct UnresolvableHints(pub Vec<UnresolvableHint>);

impl std::fmt::Display for UnresolvableHints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let listed: Vec<String> = self
            .0
            .iter()
            .map(|hint| format!("`{}` ({})", hint.hint, hint.detail))
            .collect();
        write!(
            f,
            "the contract's read hints do not resolve at the base revision: {}",
            listed.join(", ")
        )
    }
}

impl std::error::Error for UnresolvableHints {}

/// What the read hints resolved to, hint by hint.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Fingerprinted {
    pub hints: Vec<HintFingerprints>,
}

impl Fingerprinted {
    /// Every fingerprint, in hint order.
    pub fn files(&self) -> Vec<FileFingerprint> {
        self.hints
            .iter()
            .flat_map(|hint| hint.files.iter().cloned())
            .collect()
    }

    /// The hints the cap cut short, by name.
    pub fn truncated_hints(&self) -> Vec<String> {
        self.hints
            .iter()
            .filter(|hint| hint.truncated)
            .map(|hint| hint.hint.clone())
            .collect()
    }
}

/// Fingerprint the files under each read hint at `base_sha`, from the
/// tree itself (`git ls-tree`), so what the worker was pointed at is
/// recorded exactly as it was — whatever the working tree does later.
/// Git is the injected port, which is also what keeps the `GIT_*`
/// scrubbing `workspace::git_command` does on this path (audit V7).
pub fn fingerprint_hints(
    git: &dyn Git,
    repo_dir: &Path,
    base_sha: &str,
    hints: &[String],
) -> Result<Fingerprinted, UnresolvableHints> {
    let mut resolved = Vec::new();
    let mut unresolvable = Vec::new();
    let mut counted = 0usize;
    for hint in hints {
        let listing = match git.run(
            repo_dir,
            &["ls-tree", "-r", base_sha, "--", hint.trim_end_matches('/')],
        ) {
            Ok(listing) => listing,
            Err(e) => {
                unresolvable.push(UnresolvableHint {
                    hint: hint.clone(),
                    detail: e.to_string(),
                });
                continue;
            }
        };
        let mut files = Vec::new();
        let mut truncated = false;
        for line in listing.lines() {
            // `<mode> <type> <object>\t<path>`
            let Some((meta, path)) = line.split_once('\t') else {
                continue;
            };
            let mut fields = meta.split_whitespace();
            let (_mode, kind, object) = (fields.next(), fields.next(), fields.next());
            if kind != Some("blob") {
                continue;
            }
            let Some(object) = object else { continue };
            files.push(FileFingerprint {
                path: path.to_string(),
                blob: object.to_string(),
            });
            counted += 1;
            if counted >= FINGERPRINT_CAP {
                truncated = true;
                break;
            }
        }
        // A hint that names nothing in the tree points the worker at
        // nothing: said so, rather than silently contributing no rows.
        if files.is_empty() && !truncated {
            unresolvable.push(UnresolvableHint {
                hint: hint.clone(),
                detail: format!("no file under it exists at {base_sha}"),
            });
            continue;
        }
        resolved.push(HintFingerprints {
            hint: hint.clone(),
            files,
            truncated,
        });
        if counted >= FINGERPRINT_CAP {
            break;
        }
    }
    if unresolvable.is_empty() {
        Ok(Fingerprinted { hints: resolved })
    } else {
        Err(UnresolvableHints(unresolvable))
    }
}

/// The program design a contract names, as it was at the base revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignInput {
    pub path: String,
    pub text: String,
}

/// Why a contract's design could not be read at preflight. Each is a
/// refusal of the contract, before any worker is paid for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesignError {
    /// No such file at the base revision, or git would not answer.
    Missing { path: String, detail: String },
    /// Over [`crate::contract::MAX_DESIGN_BYTES`].
    TooLarge { path: String, bytes: usize },
    /// Not UTF-8: it could not be put verbatim in a prompt.
    NotText { path: String },
}

impl std::fmt::Display for DesignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { path, detail } => {
                write!(
                    f,
                    "the design `{path}` cannot be read at the base revision: {detail}"
                )
            }
            Self::TooLarge { path, bytes } => write!(
                f,
                "the design `{path}` is {bytes} bytes, over the {} byte limit",
                crate::contract::MAX_DESIGN_BYTES
            ),
            Self::NotText { path } => write!(f, "the design `{path}` is not UTF-8 text"),
        }
    }
}

impl std::error::Error for DesignError {}

/// Read the contract's design from `base_sha` itself (`git show`), never
/// from the working tree, so the worker is handed what the base revision
/// holds whatever the tree does later. The size is asked first so an
/// oversized file is never loaded.
pub fn read_design(
    git: &dyn Git,
    repo_dir: &Path,
    base_sha: &str,
    path: &str,
) -> Result<DesignInput, DesignError> {
    let object = format!("{base_sha}:{path}");
    let missing = |detail: String| DesignError::Missing {
        path: path.to_string(),
        detail,
    };
    let size_text = git
        .run(repo_dir, &["cat-file", "-s", &object])
        .map_err(|e| missing(e.to_string()))?;
    let bytes: usize = size_text
        .trim()
        .parse()
        .map_err(|_| missing(format!("git answered `{}` for its size", size_text.trim())))?;
    if bytes > crate::contract::MAX_DESIGN_BYTES {
        return Err(DesignError::TooLarge {
            path: path.to_string(),
            bytes,
        });
    }
    let text = git
        .run(repo_dir, &["show", &object])
        .map_err(|e| missing(e.to_string()))?;
    // `git` text comes back lossy: a length that differs from the blob's
    // means a byte was replaced.
    if text.len() != bytes {
        return Err(DesignError::NotText {
            path: path.to_string(),
        });
    }
    Ok(DesignInput {
        path: path.to_string(),
        text,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolVersions {
    pub relais: String,
    pub aval: Option<String>,
    pub amont: Option<String>,
    #[serde(default)]
    pub claude_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextManifest {
    pub contract_hash: String,
    pub base_sha: String,
    pub policy_hash: String,
    pub tool_versions: ToolVersions,
    pub fingerprints: Vec<FileFingerprint>,
    /// Read hints whose listing hit `FINGERPRINT_CAP`: the worker was
    /// pointed at more than this manifest records.
    #[serde(default)]
    pub truncated_hints: Vec<String>,
    /// The environment variables a relais-launched worker was given, by
    /// NAME. A new manifest records none: the agent inherits the session's
    /// environment. Kept so an older manifest, which lists names, parses.
    #[serde(default)]
    pub worker_env: Vec<String>,
    pub architecture: ArchitectureEvidence,
    pub verification_profile: String,
    /// Constraints included verbatim; the manifest reports their size so
    /// the sizing problem is observable before dispatch.
    pub constraints: Vec<String>,
    pub budget_bytes: usize,
    /// What the worker actually receives, measured: objective,
    /// acceptance criteria, constraints and entry points together. The
    /// budget is checked against this, not against the constraints
    /// alone, so a sizing problem is raised on the real prompt.
    #[serde(default)]
    pub package_bytes: usize,
    /// Whether a turn ceiling was enforceable on this run's harness
    /// ("harness" or "unavailable"). SPEC §11 lists turns among the
    /// ceilings; Claude Code 2.1.x takes no turn flag, so this manifest
    /// records which it was instead of implying one was applied.
    #[serde(default)]
    pub turn_ceiling: String,
    /// Always "none asked for": relais keeps no OS sandbox. The record stays
    /// so a reader of an older manifest, or an older reader of this one,
    /// still parses it.
    #[serde(default)]
    pub sandbox: SandboxRecord,
    /// What confines each way the worker can act on the machine.
    #[serde(default)]
    pub confinement: Confinement,
    /// How the worker's environment is kept from its subprocesses: `session`
    /// in a new manifest (the agent inherits the session's environment),
    /// `scrub` or `credentials-deny` in an older one, empty in one written
    /// before this was recorded.
    #[serde(default)]
    pub env_protection: String,
    /// Advisory guidance (SPEC §7): the constraint index, the full
    /// statements, aval's ranking and every gap. Evidence only; it is not
    /// compared and never counts toward `package_bytes`.
    #[serde(default)]
    pub guidance: Guidance,
    /// The contract's program design, verbatim as it was at the base
    /// revision. It is part of the package the worker receives, so its
    /// bytes count in `package_bytes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<String>,
    /// SHA-256 of `design`; the contract hash covers only the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_sha256: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxRecord {
    pub requested: bool,
    pub verified: Option<String>,
}

/// The mechanism that bounds each channel: `session` (the Claude Code
/// session's own sandbox and permissions, which a native agent runs
/// under). Older manifests hold `os` (relais's former OS sandbox),
/// `worktree` (the file tools' own path rule), `allowlist` (permission
/// rules relais once passed) or `none`; empty fields are a manifest from
/// before this was recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confinement {
    pub bash: String,
    pub file_tools: String,
    pub web: String,
    pub mcp: String,
}

impl Confinement {
    /// The session bounds every channel: relais applies no confinement of
    /// its own to a native agent.
    fn session() -> Self {
        let all = |mechanism: &str| Self {
            bash: mechanism.into(),
            file_tools: mechanism.into(),
            web: mechanism.into(),
            mcp: mechanism.into(),
        };
        all("session")
    }
}

pub struct ContextInputs<'a> {
    pub contract: &'a TaskContract,
    pub repo: &'a RepoPolicy,
    pub contract_hash: &'a str,
    pub base_sha: &'a str,
    pub policy_hash: &'a str,
    pub fingerprints: Fingerprinted,
    pub tool_versions: ToolVersions,
    /// Whether the harness this run will dispatch on can take a turn
    /// ceiling (`crate::backend::TurnCeiling`).
    pub turn_ceiling: crate::backend::TurnCeiling,
    /// The decision corpus, as a port, so tests supply verdicts without
    /// invoking the real binary.
    pub resolver: &'a dyn DecisionResolver,
    /// The contract's design as read from the base revision
    /// ([`read_design`]); `None` when the contract names none.
    pub design: Option<DesignInput>,
}

/// Assemble the context manifest. Fails on contradiction (blocked), on
/// an unanswered depended-upon key (needs_decision), on tool failure,
/// and on sizing problems — in that order of precedence, since a blocked
/// task should not pay for context it cannot use.
pub fn assemble(inputs: ContextInputs<'_>) -> Result<ContextManifest, ContextError> {
    let ContextInputs {
        contract,
        repo,
        contract_hash,
        base_sha,
        policy_hash,
        fingerprints,
        tool_versions,
        turn_ceiling,
        resolver,
        design,
    } = inputs;

    let manifest_aval_version = tool_versions.aval.clone();
    let mut keys: Vec<String> = contract.architecture.keys.clone();
    for mapping in &repo.architecture.mapping {
        let touched = contract
            .scope_patterns()
            .iter()
            .any(|scope| scope_could_touch(scope, &mapping.paths));
        if touched {
            keys.extend(mapping.keys.iter().cloned());
        }
    }
    keys.sort();
    keys.dedup();

    let mut resolved = Vec::new();
    for key in &keys {
        let verdict = resolver.resolve(key, contract.architecture.scope.as_deref());
        match &verdict {
            AvalVerdict::Contradiction { heads } => {
                return Err(ContextError::ContradictionBlocked {
                    key: key.clone(),
                    heads: *heads,
                })
            }
            AvalVerdict::Undecided | AvalVerdict::Unknown | AvalVerdict::Retired { .. } => {
                return Err(ContextError::NeedsDecision {
                    key: key.clone(),
                    verdict: verdict.clone(),
                })
            }
            AvalVerdict::ToolFailure { detail, .. } => {
                return Err(ContextError::ToolFailure {
                    key: key.clone(),
                    detail: detail.clone(),
                })
            }
            AvalVerdict::Active { .. } => {}
        }
        resolved.push((key.clone(), verdict));
    }

    let guidance = gather_guidance(
        resolver,
        &RelevantQuery {
            paths: guidance::query_paths(contract.scope_patterns(), &contract.read_hints),
            text: contract.objective.clone(),
        },
        &keys,
        manifest_aval_version,
    );

    // The constraint is the decision in the record's own words — what
    // `aval resolve --json` carries as `choice` and `reason`; the full
    // body stays behind `aval show ADR` for the worker to fetch on demand.
    let constraints: Vec<String> = resolved
        .iter()
        .filter_map(|(key, verdict)| match verdict {
            AvalVerdict::Active {
                adr,
                choice,
                reason,
            } => Some(match (choice, reason) {
                (Some(choice), Some(reason)) => {
                    format!("`{key}` ({adr}): {choice} — because {reason}")
                }
                (Some(choice), None) => format!("`{key}` ({adr}): {choice}"),
                (None, Some(_) | None) => {
                    format!("`{key}` is decided by {adr} (run `aval show {adr}` for the text)")
                }
            }),
            // Only an Active verdict carries a constraint: Undecided and
            // Unknown decide nothing, Retired decides nothing any more,
            // and Contradiction/ToolFailure were already refused above.
            AvalVerdict::Undecided
            | AvalVerdict::Unknown
            | AvalVerdict::Retired { .. }
            | AvalVerdict::Contradiction { .. }
            | AvalVerdict::ToolFailure { .. } => None,
        })
        .collect();

    // The budget bounds the package the worker receives (SPEC §7:
    // "the objective, acceptance criteria, necessary constraints, a
    // small set of entry points"), not the constraints alone — an
    // objective or an acceptance list that alone overruns the budget is
    // just as much a sizing problem, and counting a subset raised it on
    // the wrong number. Large files and logs are referenced by path, so
    // they are not part of this sum by design.
    let package_bytes = contract.objective.len()
        + contract
            .acceptance
            .iter()
            .map(|criterion| criterion.statement().len())
            .sum::<usize>()
        + constraints
            .iter()
            .map(|constraint| constraint.len())
            .sum::<usize>()
        + contract
            .read_hints
            .iter()
            .map(|hint| hint.len())
            .sum::<usize>()
        + design.as_ref().map_or(0, |design| design.text.len());

    let manifest = ContextManifest {
        contract_hash: contract_hash.to_string(),
        base_sha: base_sha.to_string(),
        policy_hash: policy_hash.to_string(),
        tool_versions,
        fingerprints: fingerprints.files(),
        truncated_hints: fingerprints.truncated_hints(),
        // The agent inherits the session's environment; relais names none.
        worker_env: Vec::new(),
        architecture: ArchitectureEvidence { resolved },
        verification_profile: contract.verification_profile.clone(),
        constraints: constraints.clone(),
        budget_bytes: repo.context.budget_bytes,
        package_bytes,
        turn_ceiling: turn_ceiling.as_str().to_string(),
        sandbox: SandboxRecord::default(),
        confinement: Confinement::session(),
        env_protection: "session".to_string(),
        guidance,
        design_sha256: design.as_ref().map(|d| sha256_hex(d.text.as_bytes())),
        design: design.map(|d| d.text),
    };

    if manifest.package_bytes > manifest.budget_bytes {
        return Err(ContextError::SizingProblem {
            required_bytes: manifest.package_bytes,
            budget_bytes: manifest.budget_bytes,
        });
    }
    Ok(manifest)
}

/// Ask the resolver for the advisory guidance. Nothing here can fail a
/// run: each failure becomes a gap, and guidance over its own budget is
/// trimmed (`guidance::fit_budget`). Keys already binding are not repeated
/// in the relevant block.
pub fn gather_guidance(
    resolver: &dyn DecisionResolver,
    query: &RelevantQuery,
    binding_keys: &[String],
    aval_version: Option<String>,
) -> Guidance {
    let mut guidance = Guidance {
        aval_version,
        ..Guidance::default()
    };
    let mut aval_missing = false;
    match resolver.constraint_rules() {
        Ok(rules) => {
            guidance.index = rules.rules.iter().map(|rule| rule.id.clone()).collect();
            guidance.rules = rules.rules;
            guidance.omitted = rules.omitted;
        }
        Err(gap) => {
            aval_missing = gap.is_not_launched();
            guidance.record(gap);
        }
    }
    // An aval that will not launch fails the second question the same
    // way: one gap, not two.
    if !aval_missing {
        match resolver.relevant(query) {
            Ok(relevant) => {
                guidance.relevant_keys = relevant
                    .keys
                    .into_iter()
                    .filter(|ranked| !binding_keys.contains(&ranked.key))
                    .collect();
                guidance.relevant_rules = relevant.rules;
            }
            Err(gap) => guidance.record(gap),
        }
    }
    guidance::fit_budget(&mut guidance, GUIDANCE_BUDGET_BYTES);
    guidance
}

/// Same conservative overlap logic as the router, applied between the
/// contract's declared scope and a repo architecture mapping's paths.
fn scope_could_touch(scope: &str, mapping_paths: &[String]) -> bool {
    mapping_paths
        .iter()
        .any(|pattern| crate::contract::scope::scope_could_touch(scope, pattern))
}

/// Hash a context manifest for the ledger: content-addressed evidence.
pub fn manifest_hash(manifest: &ContextManifest) -> String {
    sha256_hex(&serde_json::to_vec(manifest).expect("manifest serializes"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ArchitectureConfig;

    fn contract(architecture_keys: &[&str]) -> TaskContract {
        let mut c = TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1,
                "kind": "change",
                "objective": "Fix JSON escaping",
                "base_ref": "HEAD",
                "write_scope": ["crates/amont/**"],
                "acceptance": ["output parses"],
                "verification_profile": "rust-change",
            })
            .to_string(),
        )
        .expect("contract parses");
        c.architecture.keys = architecture_keys.iter().map(|k| k.to_string()).collect();
        c
    }

    fn repo() -> RepoPolicy {
        RepoPolicy::from_toml_str(crate::policy::INIT_TEMPLATE).expect("policy parses")
    }

    fn inputs<'a>(
        contract: &'a TaskContract,
        repo: &'a RepoPolicy,
        resolver: &'a dyn DecisionResolver,
    ) -> ContextInputs<'a> {
        ContextInputs {
            contract,
            repo,
            contract_hash: "chash",
            base_sha: "deadbeef",
            policy_hash: "phash",
            fingerprints: Fingerprinted {
                hints: vec![HintFingerprints {
                    hint: "crates/amont/src".into(),
                    files: vec![FileFingerprint {
                        path: "crates/amont/src/main.rs".into(),
                        blob: "abc".into(),
                    }],
                    truncated: false,
                }],
            },
            tool_versions: ToolVersions {
                relais: "0.1.0".into(),
                aval: Some("1.3.2".into()),
                amont: Some("1.36.0".into()),
                claude_code: None,
            },
            turn_ceiling: crate::backend::TurnCeiling::Unavailable,
            resolver,
            design: None,
        }
    }

    fn active(key: &str, _scope: Option<&str>) -> AvalVerdict {
        let _ = key;
        AvalVerdict::Active {
            adr: "ADR-0021".into(),
            choice: None,
            reason: None,
        }
    }

    #[test]
    fn parses_each_verdict_state_from_aval_json() {
        assert_eq!(
            parse_aval_output(
                0,
                r#"{"state":"active","exit":0,"key":"storage.object-store","adr":"ADR-0021",
                    "choice":"RGW behind Ceph","reason":"one storage plane"}"#
            ),
            AvalVerdict::Active {
                adr: "ADR-0021".into(),
                choice: Some("RGW behind Ceph".into()),
                reason: Some("one storage plane".into()),
            }
        );
        assert_eq!(
            parse_aval_output(4, r#"{"state":"undecided","exit":4,"key":"k"}"#),
            AvalVerdict::Undecided
        );
        assert_eq!(
            parse_aval_output(
                5,
                r#"{"state":"contradiction","exit":5,"heads":[{"adr":"ADR-0001"},{"adr":"ADR-0002"}]}"#
            ),
            AvalVerdict::Contradiction { heads: 2 }
        );
        assert_eq!(
            parse_aval_output(6, r#"{"state":"retired","exit":6,"adr":"ADR-0002"}"#),
            AvalVerdict::Retired {
                adr: "ADR-0002".into()
            }
        );
        assert_eq!(
            parse_aval_output(7, r#"{"state":"unknown","exit":7,"key":"k"}"#),
            AvalVerdict::Unknown
        );
    }

    #[test]
    fn tool_failures_stay_distinct_from_verdicts() {
        assert_eq!(
            parse_aval_output(3, r#"{"ok":false,"exit":3,"error":"unreadable corpus"}"#),
            AvalVerdict::ToolFailure {
                exit: 3,
                detail: "unreadable corpus".into()
            }
        );
        assert!(matches!(
            parse_aval_output(1, "not json at all"),
            AvalVerdict::ToolFailure { .. }
        ));
        let verdict = parse_aval_output(0, r#"{"state":"weird"}"#);
        assert!(
            matches!(verdict, AvalVerdict::ToolFailure { .. }),
            "{verdict:?}"
        );
    }

    #[test]
    fn active_decisions_assemble_into_constraints() {
        let c = contract(&["storage.object-store"]);
        let r = repo();
        let manifest = assemble(inputs(&c, &r, &active)).expect("assembles");
        assert_eq!(manifest.architecture.resolved.len(), 1);
        assert_eq!(manifest.constraints.len(), 1);
        assert_eq!(manifest.base_sha, "deadbeef");
        assert_eq!(manifest.fingerprints[0].path, "crates/amont/src/main.rs");
        let hash = manifest_hash(&manifest);
        assert_eq!(hash.len(), 64);
    }

    #[test]
    fn contradiction_blocks_affected_work() {
        let c = contract(&["api.gateway"]);
        let r = repo();
        let contradiction = |_: &str, _: Option<&str>| AvalVerdict::Contradiction { heads: 2 };
        let err = assemble(inputs(&c, &r, &contradiction)).unwrap_err();
        assert_eq!(
            err,
            ContextError::ContradictionBlocked {
                key: "api.gateway".into(),
                heads: 2
            }
        );
    }

    #[test]
    fn undecided_gates_only_tasks_that_depend_on_it() {
        let c = contract(&[]);
        let r = repo();
        // No keys: an unrelated undecided key cannot block anything.
        let undecided = |_: &str, _: Option<&str>| AvalVerdict::Undecided;
        assemble(inputs(&c, &r, &undecided)).expect("no keys means no dependency");

        let dependent = contract(&["api.gateway"]);
        let err = assemble(inputs(&dependent, &r, &undecided)).unwrap_err();
        assert!(matches!(err, ContextError::NeedsDecision { .. }), "{err:?}");
    }

    #[test]
    fn tool_failure_is_not_a_verdict() {
        let c = contract(&["k"]);
        let r = repo();
        let broken = |_: &str, _: Option<&str>| AvalVerdict::ToolFailure {
            exit: 3,
            detail: "unreadable".into(),
        };
        let err = assemble(inputs(&c, &r, &broken)).unwrap_err();
        assert!(matches!(err, ContextError::ToolFailure { .. }), "{err:?}");
    }

    #[test]
    fn repo_mapping_keys_are_pulled_in_when_scope_touches() {
        let mut r = repo();
        r.architecture = ArchitectureConfig {
            mapping: vec![crate::policy::ArchitectureMapping {
                paths: vec!["crates/amont/**".into()],
                keys: vec!["output.contract".into()],
                scope: None,
            }],
        };
        let c = contract(&[]);
        let manifest = assemble(inputs(&c, &r, &active)).expect("assembles");
        assert_eq!(
            manifest.architecture.resolved[0].0, "output.contract",
            "mapping keys are resolved when the declared scope could touch their paths"
        );
    }

    #[test]
    fn oversized_constraints_are_a_sizing_problem_never_truncated() {
        let keys: Vec<String> = (0..1500).map(|i| format!("key{i}.decision")).collect();
        let c = contract(&[]);
        let mut r = repo();
        r.architecture = ArchitectureConfig {
            mapping: vec![crate::policy::ArchitectureMapping {
                paths: vec!["crates/amont/**".into()],
                keys,
                scope: None,
            }],
        };
        let big = |_: &str, _: Option<&str>| AvalVerdict::Active {
            adr: "ADR-0001".into(),
            choice: None,
            reason: None,
        };
        let err = assemble(inputs(&c, &r, &big)).unwrap_err();
        assert!(
            matches!(err, ContextError::SizingProblem { required_bytes, budget_bytes }
                if required_bytes > budget_bytes),
            "{err:?}"
        );
    }

    #[test]
    fn the_budget_is_repo_policy_and_bounds_the_whole_package() {
        let mut c = contract(&[]);
        c.objective = "x".repeat(200);
        let mut r = repo();
        // Smaller than the objective alone: the old check counted only
        // constraints, of which this task has none, and passed.
        r.context.budget_bytes = 64;
        let err = assemble(inputs(&c, &r, &active)).unwrap_err();
        assert_eq!(
            err,
            ContextError::SizingProblem {
                required_bytes: c.objective.len()
                    + c.acceptance
                        .iter()
                        .map(|criterion| criterion.statement().len())
                        .sum::<usize>()
                    + c.read_hints.iter().map(String::len).sum::<usize>(),
                budget_bytes: 64,
            },
            "the objective alone overruns the configured budget"
        );

        // The same task fits under a budget the repository raised, and
        // the manifest carries both numbers as evidence.
        r.context.budget_bytes = 4096;
        let manifest = assemble(inputs(&c, &r, &active)).expect("assembles");
        assert_eq!(manifest.budget_bytes, 4096);
        assert!(manifest.package_bytes >= c.objective.len());
        assert_eq!(
            manifest.turn_ceiling, "unavailable",
            "a receipt says which turn ceiling the harness could take"
        );
    }

    fn manifest() -> ContextManifest {
        let c = contract(&[]);
        let r = repo();
        assemble(inputs(&c, &r, &active)).expect("assembles")
    }

    #[test]
    fn the_manifest_records_that_no_sandbox_was_asked_for() {
        let manifest = manifest();
        assert_eq!(
            manifest.sandbox,
            SandboxRecord {
                requested: false,
                verified: None
            }
        );
        for channel in [
            &manifest.confinement.bash,
            &manifest.confinement.file_tools,
            &manifest.confinement.web,
            &manifest.confinement.mcp,
        ] {
            assert_eq!(channel, "session");
        }
        assert_eq!(manifest.env_protection, "session");
    }

    /// A manifest a sandboxed run stored still parses, its record as written.
    #[test]
    fn a_manifest_written_under_the_os_sandbox_still_parses() {
        let mut stored = serde_json::to_value(manifest()).expect("json");
        stored["sandbox"] = serde_json::json!({"requested": true, "verified": "probe-7f3a"});
        stored["confinement"]["bash"] = serde_json::json!("os");
        stored["env_protection"] = serde_json::json!("credentials-deny");
        let parsed: ContextManifest = serde_json::from_value(stored).expect("still parses");
        assert_eq!(
            parsed.sandbox,
            SandboxRecord {
                requested: true,
                verified: Some("probe-7f3a".into())
            }
        );
        assert_eq!(parsed.confinement.bash, "os");
        assert_eq!(parsed.env_protection, "credentials-deny");
    }

    #[test]
    fn a_manifest_written_before_the_sandbox_fields_still_parses() {
        let mut older = serde_json::to_value(manifest()).expect("json");
        let fields = older.as_object_mut().expect("an object");
        for key in ["sandbox", "confinement", "env_protection"] {
            assert!(fields.remove(key).is_some(), "{key} was serialized");
        }
        let parsed: ContextManifest = serde_json::from_value(older).expect("still parses");
        assert_eq!(parsed.sandbox, SandboxRecord::default());
        assert_eq!(parsed.confinement, Confinement::default());
        assert_eq!(parsed.env_protection, "");
    }

    /// The agent inherits the session's environment, so a new manifest names
    /// no variable and records `session`.
    #[test]
    fn a_new_manifest_names_no_worker_environment() {
        let manifest = manifest();
        assert!(manifest.worker_env.is_empty());
        assert_eq!(manifest.env_protection, "session");
    }

    /// A manifest a relais-launched worker stored, with the names it was
    /// given and `scrub`, still parses as written.
    #[test]
    fn a_manifest_stored_with_worker_env_names_and_scrub_still_parses() {
        let mut stored = serde_json::to_value(manifest()).expect("json");
        stored["worker_env"] = serde_json::json!(["ANTHROPIC_API_KEY", "PATH"]);
        stored["env_protection"] = serde_json::json!("scrub");
        let parsed: ContextManifest = serde_json::from_value(stored).expect("still parses");
        assert_eq!(parsed.worker_env, ["ANTHROPIC_API_KEY", "PATH"]);
        assert_eq!(parsed.env_protection, "scrub");
    }

    /// V15: a read hint that resolves to nothing used to contribute no
    /// rows and no word about it, so the worker was pointed at a path
    /// the base revision does not have and nothing said so.
    #[test]
    fn an_unresolvable_read_hint_is_a_preflight_problem() {
        let git = crate::workspace::FakeGit::new(|_dir, args| {
            match args.last().copied() {
                Some("src") => Ok("100644 blob abc123\tsrc/main.rs\n".to_string()),
                // `ls-tree` on a path that is not in the tree prints
                // nothing and exits 0; a git that fails is an error.
                Some("gone") => Ok(String::new()),
                _ => Err(crate::workspace::WorkspaceError::Git(
                    "fatal: not a tree object".to_string(),
                )),
            }
        });
        let resolved = fingerprint_hints(&git, Path::new("."), "base", &["src".to_string()])
            .expect("the hint resolves");
        assert_eq!(resolved.hints.len(), 1);
        assert_eq!(resolved.files().len(), 1);
        assert_eq!(resolved.files()[0].blob, "abc123");
        assert!(resolved.truncated_hints().is_empty());

        let missing = fingerprint_hints(
            &git,
            Path::new("."),
            "base",
            &["src".to_string(), "gone".to_string(), "boom".to_string()],
        )
        .expect_err("hints that name nothing are reported");
        assert_eq!(missing.0.len(), 2, "{missing}");
        assert_eq!(missing.0[0].hint, "gone");
        assert!(missing.to_string().contains("`boom`"), "{missing}");
    }

    /// The cap is a flag on the hint, not a fingerprint row whose `path`
    /// is a sentence about truncation.
    #[test]
    fn a_hint_over_the_cap_is_flagged_not_faked() {
        let listing: String = (0..FINGERPRINT_CAP + 10)
            .map(|i| format!("100644 blob deadbeef{i}\tsrc/file{i}.rs\n"))
            .collect();
        let git = crate::workspace::FakeGit::new(move |_dir, _args| Ok(listing.clone()));
        let resolved = fingerprint_hints(&git, Path::new("."), "base", &["src".to_string()])
            .expect("resolves");
        assert_eq!(resolved.files().len(), FINGERPRINT_CAP);
        assert_eq!(resolved.truncated_hints(), vec!["src".to_string()]);
        assert!(
            resolved
                .files()
                .iter()
                .all(|file| !file.blob.is_empty() && file.path.starts_with("src/")),
            "every row is a real file"
        );
    }
    /// A resolver with canned guidance answers.
    struct Guided {
        rules: Result<Rules, GuidanceGap>,
        relevant: Result<Relevant, GuidanceGap>,
    }

    impl DecisionResolver for Guided {
        fn resolve(&self, key: &str, scope: Option<&str>) -> AvalVerdict {
            active(key, scope)
        }

        fn constraint_rules(&self) -> Result<Rules, GuidanceGap> {
            self.rules.clone()
        }

        fn relevant(&self, _query: &RelevantQuery) -> Result<Relevant, GuidanceGap> {
            self.relevant.clone()
        }
    }

    fn rule(id: &str) -> RuleRef {
        RuleRef {
            id: id.into(),
            level: "constraint".into(),
            statement: format!("statement of {id}"),
        }
    }

    fn ranked(key: &str, state: &str) -> RankedKey {
        RankedKey {
            key: key.into(),
            state: guidance::RankState::from(state.to_string()),
            adr: Some("ADR-0001".into()),
            choice: (state == "active").then(|| "the choice".to_string()),
        }
    }

    fn good() -> Guided {
        Guided {
            rules: Ok(Rules {
                rules: vec![rule("errors.never-swallowed"), rule("errors.typed-values")],
                omitted: guidance::Omitted::default(),
            }),
            relevant: Ok(Relevant {
                keys: vec![ranked("a.b", "active"), ranked("c.d", "undecided")],
                rules: vec![rule("effects.at-the-boundary")],
            }),
        }
    }

    fn gap(source: GuidanceSource, cause: guidance::GapCause) -> GuidanceGap {
        GuidanceGap::new(source, cause, "boom")
    }

    #[test]
    fn guidance_is_recorded_and_an_undecided_ranked_key_never_blocks() {
        let c = contract(&[]);
        let r = repo();
        let manifest = assemble(inputs(&c, &r, &good())).expect("assembles");
        let g = &manifest.guidance;
        assert_eq!(g.index, ["errors.never-swallowed", "errors.typed-values"]);
        assert_eq!(g.rules.len(), 2);
        assert_eq!(g.aval_version.as_deref(), Some("1.3.2"));
        assert_eq!(g.relevant_keys.len(), 2, "the undecided key is recorded");
        assert_eq!(
            g.relevant_entries(&[]).len(),
            2,
            "and left out of the prompt"
        );
        assert!(g.gaps.is_empty());
        assert!(manifest.architecture.resolved.is_empty());
    }

    #[test]
    fn a_binding_key_is_not_repeated_among_the_relevant_ones() {
        let c = contract(&["a.b"]);
        let r = repo();
        let manifest = assemble(inputs(&c, &r, &good())).expect("assembles");
        let keys: Vec<&str> = manifest
            .guidance
            .relevant_keys
            .iter()
            .map(|ranked| ranked.key.as_str())
            .collect();
        assert_eq!(keys, ["c.d"]);
    }

    #[test]
    fn each_guidance_failure_is_a_gap_and_the_run_continues() {
        let c = contract(&[]);
        let r = repo();
        for cause in [
            guidance::GapCause::Exited(1),
            guidance::GapCause::Exited(2),
            guidance::GapCause::NoAnswer,
        ] {
            let broken = Guided {
                rules: Err(gap(GuidanceSource::Index, cause.clone())),
                relevant: Err(gap(GuidanceSource::Relevant, cause)),
            };
            let manifest = assemble(inputs(&c, &r, &broken)).expect("never blocks");
            let sources: Vec<GuidanceSource> = manifest
                .guidance
                .gaps
                .iter()
                .map(|gap| gap.source)
                .collect();
            assert_eq!(sources, [GuidanceSource::Index, GuidanceSource::Relevant]);
            assert!(manifest.guidance.index.is_empty());
            assert!(manifest.guidance.relevant_entries(&[]).is_empty());
        }
    }

    #[test]
    fn malformed_json_from_the_spawn_layer_becomes_a_gap_too() {
        let c = contract(&[]);
        let r = repo();
        let malformed = Guided {
            rules: guidance::parse_rules_output("not json"),
            relevant: guidance::parse_relevant_output("{"),
        };
        let manifest = assemble(inputs(&c, &r, &malformed)).expect("never blocks");
        assert_eq!(manifest.guidance.gaps.len(), 2);
    }

    #[test]
    fn an_aval_that_will_not_launch_is_one_gap() {
        let c = contract(&[]);
        let r = repo();
        let absent = Guided {
            rules: Err(gap(GuidanceSource::Index, guidance::GapCause::NotLaunched)),
            relevant: Err(gap(GuidanceSource::Relevant, guidance::GapCause::NoAnswer)),
        };
        let manifest = assemble(inputs(&c, &r, &absent)).expect("never blocks");
        assert_eq!(manifest.guidance.gaps.len(), 1);
    }

    #[test]
    fn guidance_never_raises_a_sizing_problem() {
        let c = contract(&[]);
        let mut r = repo();
        // Smaller than the guidance, large enough for the package itself.
        let package = c.objective.len()
            + c.acceptance
                .iter()
                .map(|a| a.statement().len())
                .sum::<usize>()
            + c.read_hints.iter().map(String::len).sum::<usize>();
        r.context.budget_bytes = package;
        let huge = Guided {
            rules: Ok(Rules {
                rules: (0..400).map(|i| rule(&format!("g{i}.rule-{i}"))).collect(),
                omitted: guidance::Omitted::default(),
            }),
            relevant: Ok(Relevant::default()),
        };
        let manifest = assemble(inputs(&c, &r, &huge)).expect("guidance is not package bytes");
        assert_eq!(manifest.package_bytes, package);
    }

    #[test]
    fn over_the_guidance_budget_the_relevant_block_goes_first() {
        let c = contract(&[]);
        let r = repo();
        let big = Guided {
            rules: good().rules,
            relevant: Ok(Relevant {
                keys: vec![],
                rules: vec![RuleRef {
                    statement: "x".repeat(GUIDANCE_BUDGET_BYTES),
                    ..rule("big.rule")
                }],
            }),
        };
        let manifest = assemble(inputs(&c, &r, &big)).expect("assembles");
        assert_eq!(manifest.guidance.index.len(), 2);
        assert!(manifest.guidance.relevant_rules.is_empty());
        assert_eq!(manifest.guidance.gaps[0].source, GuidanceSource::Relevant);
    }

    #[test]
    fn a_manifest_written_before_guidance_still_parses() {
        let mut older = serde_json::to_value(manifest()).expect("json");
        assert!(older
            .as_object_mut()
            .expect("an object")
            .remove("guidance")
            .is_some());
        let parsed: ContextManifest = serde_json::from_value(older).expect("still parses");
        assert_eq!(parsed.guidance, Guidance::default());
    }

    fn git_in(dir: &Path, args: &[&str]) {
        crate::workspace::SystemGit
            .run(dir, args)
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    }

    /// A repository whose base commit holds `docs/design.md` with
    /// `committed`, and whose working tree then differs.
    fn repo_with_design(committed: &str) -> (crate::test_support::TempDir, PathBuf, String) {
        let dir = crate::test_support::temp_dir("design");
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("docs")).expect("mkdir");
        let no_hooks = dir.join("no-hooks");
        std::fs::create_dir_all(&no_hooks).expect("mkdir");
        git_in(&repo, &["init", "-q"]);
        git_in(
            &repo,
            &["config", "core.hooksPath", &no_hooks.to_string_lossy()],
        );
        git_in(&repo, &["config", "user.email", "relais@test"]);
        git_in(&repo, &["config", "user.name", "relais test"]);
        std::fs::write(repo.join("docs/design.md"), committed).expect("write");
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-q", "-m", "base"]);
        let sha = crate::workspace::SystemGit
            .run(&repo, &["rev-parse", "HEAD"])
            .expect("sha")
            .trim()
            .to_string();
        std::fs::write(repo.join("docs/design.md"), "edited after the base\n").expect("write");
        (dir, repo, sha)
    }

    #[test]
    fn a_design_is_read_from_the_base_sha_not_the_working_tree() {
        let committed = "# design\n".to_string() + &"x".repeat(10 * 1024) + "\n";
        let (_dir, repo, sha) = repo_with_design(&committed);
        let design = read_design(&crate::workspace::SystemGit, &repo, &sha, "docs/design.md")
            .expect("reads");
        assert_eq!(design.text, committed);
    }

    #[test]
    fn a_missing_or_oversized_design_is_refused() {
        let big = "y".repeat(20 * 1024);
        let (_dir, repo, sha) = repo_with_design(&big);
        let git = crate::workspace::SystemGit;
        assert!(matches!(
            read_design(&git, &repo, &sha, "docs/design.md"),
            Err(DesignError::TooLarge { bytes, .. }) if bytes == 20 * 1024
        ));
        assert!(matches!(
            read_design(&git, &repo, &sha, "docs/absent.md"),
            Err(DesignError::Missing { .. })
        ));
    }

    #[test]
    fn a_design_goes_verbatim_in_the_package_and_is_counted_and_hashed() {
        let c = contract(&[]);
        let mut r = repo();
        r.context.budget_bytes = 64 * 1024;
        let without = assemble(inputs(&c, &r, &active)).expect("assembles");
        assert_eq!(without.design, None);
        assert_eq!(without.design_sha256, None);

        let text = "# design\n".to_string() + &"z".repeat(10 * 1024);
        let mut with = inputs(&c, &r, &active);
        with.design = Some(DesignInput {
            path: "docs/design.md".into(),
            text: text.clone(),
        });
        let manifest = assemble(with).expect("assembles");
        assert_eq!(manifest.design.as_deref(), Some(text.as_str()));
        assert_eq!(manifest.package_bytes, without.package_bytes + text.len());
        assert_eq!(
            manifest.design_sha256,
            Some(sha256_hex(text.as_bytes())),
            "the manifest records the design's sha-256"
        );
    }
}
