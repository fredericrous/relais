//! Policy and authority (SPEC §5).
//!
//! Repository policy lives in `relais.toml`; machine-owned settings (under
//! `~/.config/relais/machine.toml`) hold spending ceilings, allowed
//! models, permissions and trust grants. Effective authority is the
//! intersection of repo policy, machine settings and per-run contract
//! limits — a contract can narrow a grant but nothing broadens one, and
//! there is no last-writer-wins override. Trust grants are bound to the
//! reviewed execution profile AND to the repository it was reviewed for;
//! a changed declaration, or the same declaration in another repository,
//! invalidates them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::{Review, TaskContract};
use crate::ids::canonical_json_hash;
use crate::money::MicroUsd;

pub const POLICY_SCHEMA_VERSION: u64 = 1;

/// How much context a worker prompt may carry when `relais.toml` says
/// nothing. Repositories override it with `[context] budget_bytes`, which
/// is part of the hashed authority. Sizing problems are explicit;
/// nothing is silently truncated (SPEC §7).
pub const DEFAULT_CONTEXT_BUDGET_BYTES: usize = 64 * 1024;

/// Routing tiers, ordered: research < implementation < escalation. Also
/// the `[models.*]` table keys, so a profile's tier is its identity in
/// policy and reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Research,
    Implementation,
    Escalation,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Research => "research",
            Tier::Implementation => "implementation",
            Tier::Escalation => "escalation",
        }
    }

    /// The inverse of [`Tier::as_str`], for a tier read back from the
    /// ledger or a dataset. `None` is a name this relais does not know.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "research" => Tier::Research,
            "implementation" => Tier::Implementation,
            "escalation" => Tier::Escalation,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    /// Explicit model ID. Recorded in the effective model fields of every
    /// dispatch and report.
    pub id: String,
    /// Omitted for models without effort control.
    #[serde(default)]
    pub effort: Option<Effort>,
}

/// Integration dependency: required, optional or off. A missing required
/// integration blocks execution; an optional gap appears in the report and
/// is never reported as a passed check (SPEC §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyMode {
    Required,
    Optional,
    Off,
}

/// String (`aval = "required"`) or table form with an explicit binary
/// override for doctor/invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    Mode(DependencyMode),
    Full {
        mode: DependencyMode,
        #[serde(default)]
        bin: Option<String>,
    },
}

impl Dependency {
    pub fn mode(&self) -> DependencyMode {
        match self {
            Dependency::Mode(mode) => *mode,
            Dependency::Full { mode, .. } => *mode,
        }
    }

    pub fn bin(&self) -> Option<&str> {
        match self {
            Dependency::Mode(_) => None,
            Dependency::Full { bin, .. } => bin.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Integrations {
    pub aval: Option<Dependency>,
    pub amont: Option<Dependency>,
    pub amont_agent: Option<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    #[serde(default = "default_command_timeout")]
    pub timeout_seconds: u64,
}

fn default_command_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct VerificationPolicy {
    #[serde(default)]
    pub profiles: BTreeMap<String, VerificationProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct VerificationProfile {
    /// What the commands need installed inside a verification worktree
    /// before they can run: `npm ci`, `uv sync --frozen`, whatever puts
    /// the tree's own dependencies in place. A worktree is a checkout of
    /// one revision and nothing else, so an ecosystem that keeps its
    /// dependencies in the tree (node_modules, a virtualenv) has none
    /// there until this runs. It runs first, in every directory the
    /// commands run in, and stops at its first failure. It is executable
    /// authority like the commands themselves — hashed into the trust
    /// grant, never inferred from a lockfile (SPEC §5, §7). Its outcomes
    /// are evidence, never checks: a setup that succeeds passes nothing.
    #[serde(default)]
    pub setup: Vec<CommandSpec>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
    /// amont check IDs (from `amont list --json`) this profile requires to
    /// be in force. A skipped, inert, unavailable or untrusted required
    /// check is a gap, not a pass (SPEC §10).
    #[serde(default)]
    pub amont_checks: Vec<String>,
    /// amont check IDs whose bypass or severity downgrade this profile
    /// accepts. A bypassed or downgraded check cannot fail the gate, so
    /// it is a verification gap by default (SPEC §10); naming it here is
    /// the "waiver already in policy" the same section allows — a
    /// reviewed, content-bound decision, not something a run may reach.
    #[serde(default)]
    pub amont_waivers: Vec<String>,
    /// Extra globs (beyond the built-in build-manifest and test-tree
    /// defaults) naming files this profile's verdict depends on. A
    /// candidate touching one requires review (SPEC §10).
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Cache baseline results by base SHA, profile and toolchain (SPEC
    /// §18). Off by default: a profile with undeclared external
    /// dependencies or nondeterministic checks is not cacheable, and only
    /// the repository knows which it has.
    #[serde(default)]
    pub cache_baseline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRule {
    pub paths: Vec<String>,
    pub minimum_tier: Tier,
    #[serde(default)]
    pub review: Option<Review>,
}

/// Path-to-decision mappings live here, because aval resolves exact keys
/// and does not provide source-code impact analysis (SPEC §7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ArchitectureConfig {
    #[serde(default)]
    pub mapping: Vec<ArchitectureMapping>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchitectureMapping {
    pub paths: Vec<String>,
    pub keys: Vec<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ExecutionPolicy {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_max_repairs")]
    pub max_repairs_before_escalation: u32,
    #[serde(default = "default_max_wall_seconds")]
    pub max_wall_seconds: u64,
    #[serde(default = "default_allow_nested_agents")]
    pub allow_nested_agents: bool,
    #[serde(default = "default_max_agent_depth")]
    pub max_agent_depth: u32,
    #[serde(default = "default_max_agents_total")]
    pub max_agents_total: u32,
}

fn default_max_attempts() -> u32 {
    3
}
fn default_max_repairs() -> u32 {
    1
}
fn default_max_wall_seconds() -> u64 {
    1200
}
fn default_allow_nested_agents() -> bool {
    true
}
fn default_max_agent_depth() -> u32 {
    3
}
fn default_max_agents_total() -> u32 {
    24
}

/// How much context a worker prompt may carry (SPEC §7). Repository
/// policy, not a machine setting: what a task needs to be told is a
/// property of the repository's decisions and entry points, and it is
/// hashed into the authority so raising the budget needs the same review
/// as changing a verification command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ContextPolicy {
    pub budget_bytes: usize,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            budget_bytes: DEFAULT_CONTEXT_BUDGET_BYTES,
        }
    }
}

/// Repository policy, `relais.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoPolicy {
    pub schema_version: u64,
    #[serde(default)]
    pub models: BTreeMap<Tier, ModelProfile>,
    #[serde(default)]
    pub execution: ExecutionPolicy,
    #[serde(default)]
    pub context: ContextPolicy,
    #[serde(default)]
    pub integrations: Integrations,
    #[serde(default)]
    pub verification: VerificationPolicy,
    #[serde(default)]
    pub risk: Vec<RiskRule>,
    #[serde(default)]
    pub architecture: ArchitectureConfig,
    /// Explicitly configured deterministic recipes (SPEC §6, step 3). Used
    /// only
    /// when one fully covers the task; never inferred from prose.
    #[serde(default)]
    pub recipes: Vec<RecipeSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeSpec {
    pub name: String,
    #[serde(default)]
    pub kind: Option<crate::contract::Kind>,
    #[serde(default)]
    pub scope_within: Vec<String>,
    pub tier: Tier,
}

impl RepoPolicy {
    pub fn from_toml_str(text: &str) -> Result<Self, PolicyError> {
        let policy: RepoPolicy =
            toml::from_str(text).map_err(|e| PolicyError::MalformedToml(e.to_string()))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(PolicyError::UnsupportedSchemaVersion(self.schema_version));
        }
        for rule in &self.risk {
            if rule.paths.is_empty() {
                return Err(PolicyError::EmptyRiskPaths);
            }
        }
        for profile in self.verification.profiles.values() {
            for command in profile.setup.iter().chain(&profile.commands) {
                if command.argv.is_empty() {
                    return Err(PolicyError::EmptyCommandArgv);
                }
            }
        }
        Ok(())
    }

    /// The policy as the hash sees it: everything the serialized policy
    /// carries except [`AUTHORITY_EXCLUSIONS`].
    ///
    /// Derived from the struct rather than enumerated by hand, so a new
    /// `RepoPolicy` field is inside the hash by construction. The hand
    /// written list it replaced had the opposite property: a field added
    /// without touching this function was executable authority outside
    /// the hash, and a grant reviewed before it survived (P4).
    fn authority_value(&self) -> serde_json::Value {
        let mut value =
            serde_json::to_value(self).expect("RepoPolicy serializes: string map keys, no floats");
        if let serde_json::Value::Object(map) = &mut value {
            for excluded in AUTHORITY_EXCLUSIONS {
                map.remove(*excluded);
            }
        }
        value
    }

    /// Hash over the executable authority: models, execution limits, the
    /// context budget, verification profiles, integration modes, risk
    /// rules, recipes and architecture mappings — every field of the
    /// policy but the schema version. A trust grant is bound to this
    /// hash and to the repository (see [`grant_key`]), so a changed
    /// declaration invalidates it (SPEC §5). Recipes are executable
    /// authority: one that covers a task picks its tier outright, ahead
    /// of the learner, so a grant must not survive an edit to them. The
    /// context budget belongs here for the same reason: raising it is
    /// what turns a sizing problem into a dispatch, so it is reviewed,
    /// not slipped in. A profile's setup step is hashed with its
    /// commands: `npm ci` runs the repository's own lifecycle scripts,
    /// which is exactly the kind of execution a grant is a review of.
    pub fn authority_hash(&self) -> String {
        canonical_json_hash(&self.authority_value())
    }
}

/// Policy fields that are NOT executable authority, and so stay outside
/// the hash. The schema version identifies the format, not what runs;
/// bumping it would otherwise invalidate every grant on upgrade.
pub const AUTHORITY_EXCLUSIONS: &[&str] = &["schema_version"];

/// Which repository a trust grant was reviewed for.
///
/// A grant used to be bound to the policy's content alone, so any other
/// repository whose `relais.toml` hashed the same — the public `relais
/// init` template does — ran its `make check` under a grant nobody had
/// reviewed for it (P2). The identity is the REPOSITORY, never one
/// checkout of it: the `origin` remote URL when git reports one, else
/// the repository's common git directory. Every worktree of one
/// repository shares both, so a grant reviewed in the live checkout
/// covers the task worktrees `relais run` creates from it — the
/// checkout path used to be hashed in, and a run from a worktree
/// blocked on a grant the user had already issued. Built at a boundary
/// (`repo::identity`) and passed in: this module decides, it does not
/// look at disks or run git.
///
/// Serialized externally tagged — `{"origin": "…"}` or
/// `{"common_dir": "…"}` — and that shape is hashed into the grant key,
/// so it is a wire format: a renamed variant re-keys every grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoIdentity {
    /// The `origin` remote URL, exactly as git reports it.
    Origin(String),
    /// The canonical path of the git common directory — the one `.git`
    /// every worktree of the repository shares — for a repository with
    /// no `origin`; outside any repository, the policy root itself.
    CommonDir(String),
}

impl RepoIdentity {
    /// The identity of a repository whose `origin` remote is `url`.
    pub fn origin(url: impl Into<String>) -> Self {
        Self::Origin(url.into())
    }

    /// The identity of a repository with no `origin`, by its common
    /// git directory (or, outside any repository, its policy root).
    pub fn common_dir(path: &std::path::Path) -> Self {
        Self::CommonDir(path.to_string_lossy().into_owned())
    }

    /// What a human reads in a grant's `repo = "…"` field: the origin
    /// URL, or the common directory. Informational — the binding itself
    /// is in the key.
    pub fn label(&self) -> &str {
        match self {
            Self::Origin(url) | Self::CommonDir(url) => url,
        }
    }
}

/// How `relais plan` and `relais doctor` name the identity in use:
/// `<url>`, or `<common-dir> (no origin)` so a reader knows which of the
/// two bindings a grant will be keyed on.
impl std::fmt::Display for RepoIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Origin(url) => f.write_str(url),
            Self::CommonDir(dir) => write!(f, "{dir} (no origin)"),
        }
    }
}

/// The key a trust grant is recorded under in machine.toml: the
/// execution declaration AND the repository it was reviewed for. The
/// same declaration in a second repository hashes to a different key,
/// so it needs its own review (P2).
pub fn grant_key(authority_hash: &str, repo: &RepoIdentity) -> String {
    canonical_json_hash(&serde_json::json!({
        "authority": authority_hash,
        "repo": repo,
    }))
}

/// The repository half of a task's identity, hashed the way [`grant_key`]
/// hashes it: the identity alone, so two repositories with the same
/// `origin` (or the same common directory) key their tasks alike and a
/// renamed `RepoIdentity` variant re-keys them the same way it re-keys a
/// trust grant.
pub fn repo_key(repo: &RepoIdentity) -> String {
    canonical_json_hash(&serde_json::json!({ "repo": repo }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    UnsupportedSchemaVersion(u64),
    MalformedToml(String),
    EmptyRiskPaths,
    EmptyCommandArgv,
    /// A spending ceiling below zero. A negative ceiling is not a tight
    /// one: every comparison against it is already past, and the
    /// remaining-budget subtraction would go somewhere nobody meant.
    NegativeCeiling {
        field: &'static str,
        micros: i64,
    },
    /// A `[trust."…"]` block that is not a reviewed grant: no reviewer
    /// named, or a `granted_at` that is not a date this relais can read.
    InvalidTrustGrant {
        key: String,
        detail: String,
    },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(v) => write!(
                f,
                "unsupported relais.toml schema_version {v} (this relais understands {POLICY_SCHEMA_VERSION})"
            ),
            Self::MalformedToml(detail) => write!(f, "policy file is not valid TOML: {detail}"),
            Self::EmptyRiskPaths => write!(f, "a [[risk]] rule needs at least one path pattern"),
            Self::EmptyCommandArgv => write!(f, "a verification command needs a non-empty argv"),
            Self::NegativeCeiling { field, micros } => write!(
                f,
                "[spending] {field} is {micros}; a ceiling is a number of micro-USD and cannot be negative"
            ),
            Self::InvalidTrustGrant { key, detail } => {
                write!(f, "trust grant [trust.\"{key}\"]: {detail}")
            }
        }
    }
}

impl std::error::Error for PolicyError {}

/// Machine-owned settings, `~/.config/relais/machine.toml`. Never written
/// by a run; workers cannot update grants or policy during a run (SPEC §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineSettings {
    pub schema_version: u64,
    /// Allowed model IDs. `None` allows every repo-configured model; a
    /// list intersects with repo policy. Machine authority can only
    /// narrow, never broaden.
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub spending: SpendingCeilings,
    /// Trust grants, keyed by [`grant_key`]: the repository's authority
    /// hash together with the repository's own identity.
    #[serde(default)]
    pub trust: BTreeMap<String, TrustGrant>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub concurrency: ConcurrencyLimits,
    #[serde(default)]
    pub trials: TrialEnvelope,
    #[serde(default)]
    pub routing: RoutingSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct SpendingCeilings {
    /// Per-run API spend ceiling. Best effort across in-flight requests;
    /// never advertised as an exact cap (SPEC §11). Money, not a bare
    /// integer: the remaining-budget subtraction saturates (P7).
    pub per_run_micros: Option<MicroUsd>,
    /// Machine-wide ceiling for one UTC day, checked at the runner's
    /// loop top against every usage event recorded that day — this run's
    /// and every other run's on this machine. Best effort for the same
    /// reasons as the per-run ceiling, and a lower bound besides: usage
    /// the provider never reported is NULL in the ledger and no sum can
    /// include it.
    pub per_day_micros: Option<MicroUsd>,
}

/// One reviewed grant. The key it is recorded under carries the binding
/// (see [`grant_key`]); these fields are the audit trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustGrant {
    /// An RFC3339 timestamp or a plain `YYYY-MM-DD` date. Parsed at the
    /// boundary by [`MachineSettings::validate`], so a grant stamped
    /// `"yesterday"` is refused rather than counted as valid (P10).
    pub granted_at: String,
    /// Who reviewed the declaration. Required: a grant with nobody's
    /// name on it is not a reviewed grant.
    pub reviewed_by: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Which repository this grant was issued for, as `relais plan`
    /// prints it. Informational — the binding is in the key — but it is
    /// what makes a machine.toml readable.
    #[serde(default)]
    pub repo: Option<String>,
}

impl TrustGrant {
    /// When this grant was recorded. Both spellings machine.toml uses
    /// are accepted; a plain date is read as midnight UTC.
    pub fn granted_at(&self) -> Result<chrono::DateTime<chrono::FixedOffset>, String> {
        // Two accepted spellings, tried in turn: a full timestamp that
        // will not parse is not an error here, it is the plain-date
        // case below, which reports the failure for both.
        if let Ok(at) = chrono::DateTime::parse_from_rfc3339(&self.granted_at) {
            return Ok(at);
        }
        let date = chrono::NaiveDate::parse_from_str(&self.granted_at, "%Y-%m-%d")
            .map_err(|e| format!("granted_at `{}` is not a date: {e}", self.granted_at))?;
        date.and_hms_opt(0, 0, 0)
            .and_then(|at| {
                at.and_local_timezone(chrono::FixedOffset::east_opt(0)?)
                    .single()
            })
            .ok_or_else(|| format!("granted_at `{}` is not a date", self.granted_at))
    }

    fn validate(&self, key: &str) -> Result<(), PolicyError> {
        if self.reviewed_by.trim().is_empty() {
            return Err(PolicyError::InvalidTrustGrant {
                key: key.to_string(),
                detail: "reviewed_by is empty; name who reviewed the declaration".into(),
            });
        }
        self.granted_at()
            .map(|_| ())
            .map_err(|detail| PolicyError::InvalidTrustGrant {
                key: key.to_string(),
                detail,
            })
    }
}

/// Tool profile for workers. Workers cannot commit, merge, push or
/// publish through the normal allowed tool profile (SPEC §8).
///
/// Both lists are machine-owned; repository policy has no permissions
/// field at all, so a repository can neither widen nor narrow what its
/// own workers may do. What machine.toml writes here can only ADD to
/// the deny floor: [`effective_disallowed_tools`] unions the two, so
/// `disallowed_tools = []` still denies commit, merge, push, rebase,
/// reset and tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Permissions {
    /// Extra denials, on top of [`default_disallowed_tools`]. Never
    /// read on its own — read [`effective_disallowed_tools`].
    #[serde(default = "default_disallowed_tools")]
    pub disallowed_tools: Vec<String>,
    /// Claude Code permission rules the worker is granted, passed via
    /// `--settings` (`{"permissions":{"allow":[…]}}`) — "Edit", "Write",
    /// "Bash(cargo test:*)" and the like. Machine-owned and machine-owned
    /// only: repo policy never appears here, so a repository cannot widen
    /// what its own workers may do (SPEC §8). Empty by default: with no
    /// grant a `change` worker is denied `Edit`/`Write` by the harness and
    /// the attempt ends blocked rather than silently doing nothing.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// The deny floor (SPEC §8: "a worker cannot commit, merge, push or
/// publish through the normal allowed tool profile").
///
/// Claude Code matches a `Bash(…:*)` rule as a PREFIX of the literal
/// command line, so the publishing verbs are only half the list: `sh -c
/// 'git push'`, `git -C . commit` and `env git commit` all reach the
/// same operation without ever starting the command line `git push`.
/// The wrappers below are the evasions that can be expressed as
/// permission rules, and they are denied outright rather than
/// pattern-matched — a rule cannot look inside `-c` at what is about to
/// run.
///
/// This is a floor, NOT a sandbox. SPEC §8 is explicit: "this is an
/// acceptance boundary, not a claim of filesystem isolation… tools with
/// Bash access are not a security sandbox". Any worker that can run one
/// unlisted program can run `git` through it (`make push`, a script it
/// wrote, a language runtime's subprocess call), and no deny list
/// enumerates that. What actually holds is downstream: relais snapshots
/// the candidate itself, judges the diff against the declared scope,
/// and never integrates anything — a worker that did manage to commit
/// has changed nothing about what gets accepted. Strong confinement
/// needs the separately configured OS/container backend §8 describes.
pub fn default_disallowed_tools() -> Vec<String> {
    vec![
        // The operations themselves.
        "Bash(git commit:*)".into(),
        "Bash(git merge:*)".into(),
        "Bash(git push:*)".into(),
        "Bash(git rebase:*)".into(),
        "Bash(git reset:*)".into(),
        "Bash(git tag:*)".into(),
        "Bash(git am:*)".into(),
        "Bash(git cherry-pick:*)".into(),
        // Same operations, spelled so a prefix match misses them: git's
        // own pre-subcommand options move the verb off the front of the
        // command line.
        "Bash(git -C:*)".into(),
        "Bash(git -c:*)".into(),
        "Bash(git --git-dir:*)".into(),
        "Bash(git --work-tree:*)".into(),
        "Bash(git --exec-path:*)".into(),
        // A shell or an environment wrapper hides any command line at
        // all behind its own.
        "Bash(sh -c:*)".into(),
        "Bash(bash -c:*)".into(),
        "Bash(zsh -c:*)".into(),
        "Bash(dash -c:*)".into(),
        "Bash(env git:*)".into(),
        "Bash(eval:*)".into(),
    ]
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            disallowed_tools: default_disallowed_tools(),
            allowed_tools: Vec::new(),
        }
    }
}

/// The deny list a worker actually runs under: the shipped floor, plus
/// whatever machine.toml added, in that order and without duplicates.
///
/// A union, never an override. The doc above `default_disallowed_tools`
/// has always called that list a floor; until this function existed it
/// was not one, because `disallowed_tools = []` in machine.toml replaced
/// it wholesale and nothing else re-added the commit/merge/push denials.
pub fn effective_disallowed_tools(permissions: &Permissions) -> Vec<String> {
    let mut tools = default_disallowed_tools();
    for tool in &permissions.disallowed_tools {
        if !tools.contains(tool) {
            tools.push(tool.clone());
        }
    }
    tools
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct ConcurrencyLimits {
    pub max_active_agents: Option<u32>,
    pub max_active_agents_per_session: Option<u32>,
    pub max_heavy_commands: Option<u32>,
    pub max_training_jobs: Option<u32>,
    pub max_agent_depth: Option<u32>,
    pub max_agents_per_run: Option<u32>,
    pub training_when_idle: bool,
}

/// Authorized experimentation envelope (SPEC §13, §17). Automatic trials
/// and promotion stay inside it; anything wider needs an explicit policy
/// edit. Risk floors and required checks are never trainable parameters.
///
/// NOT IMPLEMENTED IN THIS RELEASE. §17's comparative evidence needs a
/// replay command or randomized assignment with logged propensities, and
/// this release ships neither: nothing reads these fields, so
/// `enabled = true` changes no behaviour whatsoever. The struct stays
/// because `MachineSettings` denies unknown fields — deleting it would
/// turn a machine.toml that already sets `[trials]` into a parse error
/// on upgrade — and `doctor` prints a `!` line whenever the flag is on,
/// so nobody believes a trial is running.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TrialEnvelope {
    /// Inert: see the struct's note. Kept parseable, reported by doctor.
    pub enabled: bool,
    pub max_daily_trials: Option<u32>,
    pub max_trial_cost_micros: Option<i64>,
}

/// Learned-routing switch and the quality requirement estimates are
/// selected against (SPEC §6, §17: "expected complete-strategy cost
/// subject to the configured quality requirement"). Disabling learned
/// routing disables only the predictor — execution, verification and
/// accounting are unaffected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RoutingSettings {
    pub learned_enabled: bool,
    /// Minimum estimated acceptance a profile needs to be selected over
    /// the conservative baseline.
    pub quality_floor: Option<f64>,
    /// Promotion gate (SPEC §17): the minimum number of held-out TEST
    /// records observed at the tier the artifact selects. Below it, the
    /// measured acceptance rate is a handful of tasks and promotion is
    /// refused — one supported record used to be enough.
    pub min_supported_test_records: usize,
    /// Promotion gate: the largest share of held-out test tasks the
    /// artifact may abstain on and still be promoted. An artifact that
    /// abstains on most tasks is the baseline wearing a model's name.
    pub max_abstention_rate: f64,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            learned_enabled: true,
            quality_floor: Some(0.75),
            min_supported_test_records: 20,
            max_abstention_rate: 0.5,
        }
    }
}

impl MachineSettings {
    pub fn from_toml_str(text: &str) -> Result<Self, PolicyError> {
        let settings: MachineSettings =
            toml::from_str(text).map_err(|e| PolicyError::MalformedToml(e.to_string()))?;
        if settings.schema_version != POLICY_SCHEMA_VERSION {
            return Err(PolicyError::UnsupportedSchemaVersion(
                settings.schema_version,
            ));
        }
        settings.validate()?;
        Ok(settings)
    }

    /// Everything about machine.toml that serde's types cannot state:
    /// ceilings are non-negative amounts of money, and every trust grant
    /// names a reviewer and a date that parses. Run from
    /// [`MachineSettings::from_toml_str`], so nothing downstream has to
    /// re-check it, and `doctor` reports the failure by name.
    pub fn validate(&self) -> Result<(), PolicyError> {
        for (field, ceiling) in [
            ("per_run_micros", self.spending.per_run_micros),
            ("per_day_micros", self.spending.per_day_micros),
        ] {
            if let Some(ceiling) = ceiling {
                if ceiling.is_negative() {
                    return Err(PolicyError::NegativeCeiling {
                        field,
                        micros: ceiling.to_micros(),
                    });
                }
            }
        }
        for (key, grant) in &self.trust {
            grant.validate(key)?;
        }
        Ok(())
    }
}

/// Why execution is blocked before any model is launched. Each blocker is
/// a stable code plus a human explanation; `blocked:*` codes surface in
/// `plan`, `run` and `explain` output.
/// Why execution cannot proceed. Each code is stable — it is what
/// `plan`, `run` and `explain` print after `blocked:` — and an enum so a
/// misspelled code is a compile error rather than a silent new outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockCode {
    DirtyBase,
    MissingTrustGrant,
    VerificationProfileUnknown,
    ModelNotAllowed,
    ModelUnavailable,
    IntegrationMissing,
    BaseUnresolvable,
    AvalToolFailure,
    ContextSizing,
    BaselineVerificationFailed,
    /// The base revision's checks could not run at all — a program the
    /// profile names was not found (exit 127) — so the base has no
    /// verdict and no candidate can be compared to it. Not a baseline
    /// FAILURE, which is a check that ran and said no.
    BaselineUnrunnable,
    /// A declared setup command did not succeed in a worktree the run
    /// owns (the base or the task worktree), so the profile's commands
    /// never ran there.
    VerificationSetupFailed,
    WorktreeUnavailable,
    BackendUnavailable,
    AdmissionUnavailable,
    AdmissionRefused,
    SnapshotFailed,
    ScopeCheckFailed,
    VerificationUnavailable,
    EnvMissing,
    ArchitectureContradiction,
    DecompositionKind,
    /// The harness refused the worker a tool it needed: missing
    /// permissions are a blocked result, never a worker that chose to do
    /// nothing (SPEC §8).
    PermissionDenied,
    /// The harness reported no effective model, so nothing establishes
    /// that the routed model ran. Unverified is not approved (SPEC §6).
    ModelUnverified,
    /// A contract read hint names nothing at the base revision: the
    /// worker would be pointed at a path this tree does not have.
    ReadHintUnresolvable,
}

impl BlockCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirtyBase => "dirty_base",
            Self::MissingTrustGrant => "missing_trust_grant",
            Self::VerificationProfileUnknown => "verification_profile_unknown",
            Self::ModelNotAllowed => "model_not_allowed",
            Self::ModelUnavailable => "model_unavailable",
            Self::IntegrationMissing => "integration_missing",
            Self::BaseUnresolvable => "base_unresolvable",
            Self::AvalToolFailure => "aval_tool_failure",
            Self::ContextSizing => "context_sizing",
            Self::BaselineVerificationFailed => "baseline_verification_failed",
            Self::BaselineUnrunnable => "baseline_unrunnable",
            Self::VerificationSetupFailed => "verification_setup_failed",
            Self::WorktreeUnavailable => "worktree_unavailable",
            Self::BackendUnavailable => "backend_unavailable",
            Self::AdmissionUnavailable => "admission_unavailable",
            Self::AdmissionRefused => "admission_refused",
            Self::SnapshotFailed => "snapshot_failed",
            Self::ScopeCheckFailed => "scope_check_failed",
            Self::VerificationUnavailable => "verification_unavailable",
            Self::EnvMissing => "env_missing",
            Self::ArchitectureContradiction => "architecture_contradiction",
            Self::DecompositionKind => "decomposition_kind",
            Self::PermissionDenied => "permission_denied",
            Self::ModelUnverified => "model_unverified",
            Self::ReadHintUnresolvable => "read_hint_unresolvable",
        }
    }
}

impl std::fmt::Display for BlockCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub code: BlockCode,
    pub detail: String,
}

/// The intersection of repo policy, machine settings and one contract's
/// limits. All narrowing, never broadening: attempts and wall seconds take
/// the minimum of every source, models take the intersection of the
/// allowed lists.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveAuthority {
    pub models: BTreeMap<Tier, ModelProfile>,
    pub max_attempts: u32,
    pub max_wall_seconds: u64,
    pub max_repairs_before_escalation: u32,
    /// Agent-tree limits: repo policy intersected with machine
    /// concurrency, the coordinator enforces them per run (SPEC §23).
    pub allow_nested_agents: bool,
    pub max_agent_depth: u32,
    pub max_agents_total: u32,
    pub verification_profile: VerificationProfile,
    pub review_floor: Review,
    pub disallowed_tools: Vec<String>,
    /// Machine-owned permission grant handed to the worker via
    /// `--settings`. Repo policy contributes nothing: authority here only
    /// narrows, and a repository cannot broaden its own workers' reach.
    pub allowed_tools: Vec<String>,
    /// The hash of the repository's executable declaration.
    pub authority_hash: String,
    /// The machine.toml key a grant for THIS declaration in THIS
    /// repository is recorded under; what `relais plan` prints to paste.
    pub grant_key: String,
    pub trust_granted: bool,
    pub blockers: Vec<Blocker>,
}

/// The authority a run may execute under.
///
/// `repo_identity` is which repository this is, built at a boundary
/// (`repo::identity`) and passed in. A grant is bound to the pair
/// (declaration, repository), so the public `relais init` template
/// hashing the same in two repositories no longer lets one borrow the
/// other's review (P2).
pub fn effective_authority(
    repo: &RepoPolicy,
    machine: &MachineSettings,
    contract: &TaskContract,
    repo_identity: &RepoIdentity,
) -> EffectiveAuthority {
    let mut blockers = Vec::new();

    let mut models = repo.models.clone();
    if let Some(allowed) = &machine.allowed_models {
        models.retain(|_, profile| allowed.contains(&profile.id));
    }
    if !repo.models.is_empty() && models.is_empty() {
        blockers.push(Blocker {
            code: BlockCode::ModelNotAllowed,
            detail: "the machine's allowed_models excludes every configured model".into(),
        });
    }

    let profile = repo
        .verification
        .profiles
        .get(&contract.verification_profile)
        .cloned()
        .unwrap_or_else(|| {
            blockers.push(Blocker {
                code: BlockCode::VerificationProfileUnknown,
                detail: format!(
                    "verification profile `{}` is not defined in relais.toml",
                    contract.verification_profile
                ),
            });
            VerificationProfile::default()
        });

    let authority_hash = repo.authority_hash();
    let grant_key = grant_key(&authority_hash, repo_identity);
    let trust_granted = machine.trust.contains_key(&grant_key);
    if !trust_granted {
        blockers.push(Blocker {
            code: BlockCode::MissingTrustGrant,
            detail: format!(
                "no trust grant for this execution declaration (authority hash \
                 {authority_hash}) in this repository ({repo_identity}); `relais plan` \
                 prints the [trust.\"{grant_key}\"] block to review and paste into \
                 machine.toml"
            ),
        });
    }

    // The contract narrows only what it actually says. An absent limit
    // is not a narrowing: it inherits the repository's, so raising a
    // repository ceiling reaches the runs, which a hard-coded contract
    // default silently prevented.
    let max_attempts = contract
        .limits
        .attempts
        .map_or(repo.execution.max_attempts, |attempts| {
            repo.execution.max_attempts.min(attempts)
        });
    let max_wall_seconds = contract
        .limits
        .wall_seconds
        .map_or(repo.execution.max_wall_seconds, |wall| {
            repo.execution.max_wall_seconds.min(wall)
        });

    // Only the rules the declared scope could touch raise the review
    // floor; a rule about `**/trust/**` says nothing about a docs change.
    let review_floor = contract.review.max(
        repo.risk
            .iter()
            .filter(|rule| {
                rule.paths.iter().any(|pattern| {
                    crate::contract::scope::write_scope_could_touch(contract, pattern)
                })
            })
            .map(|rule| rule.review.unwrap_or(Review::Off))
            .max()
            .unwrap_or(Review::Off),
    );

    let max_agent_depth = machine
        .concurrency
        .max_agent_depth
        .map_or(repo.execution.max_agent_depth, |cap| {
            cap.min(repo.execution.max_agent_depth)
        });
    let max_agents_total = machine
        .concurrency
        .max_agents_per_run
        .map_or(repo.execution.max_agents_total, |cap| {
            cap.min(repo.execution.max_agents_total)
        });

    EffectiveAuthority {
        models,
        max_attempts,
        max_wall_seconds,
        max_repairs_before_escalation: repo.execution.max_repairs_before_escalation,
        allow_nested_agents: repo.execution.allow_nested_agents,
        max_agent_depth,
        max_agents_total,
        verification_profile: profile,
        review_floor,
        disallowed_tools: effective_disallowed_tools(&machine.permissions),
        allowed_tools: machine.permissions.allowed_tools.clone(),
        authority_hash,
        grant_key,
        trust_granted,
        blockers,
    }
}

/// Starter `relais.toml` written by `relais init`. Models and profiles
/// are placeholders the user edits; init never writes machine-owned
/// settings.
pub const INIT_TEMPLATE: &str = r#"schema_version = 1

# Profiles the router selects among. Explicit model IDs keep policies
# reproducible; the effective model is recorded on every dispatch.
[models.research]
id = "haiku"

[models.implementation]
id = "sonnet"
effort = "medium"

[models.escalation]
id = "fable"
effort = "medium"

[execution]
max_attempts = 3
max_repairs_before_escalation = 1
max_wall_seconds = 1200
allow_nested_agents = true
max_agent_depth = 3
max_agents_total = 24

# How much context one worker prompt may carry: objective, acceptance
# criteria, architectural constraints and entry points together. A task
# whose package does not fit is a sizing problem — split it — and never a
# silently truncated prompt (SPEC §7). This is executable authority: it
# is hashed into the trust grant, so raising it is reviewed.
# [context]
# budget_bytes = 65536

# required blocks execution when missing; optional gaps are reported and
# never counted as passed checks. `off` is not checked at all, and
# `relais doctor` says which of the three each one is.
[integrations]
aval = "required"
amont = "required"
amont_agent = "required"

# What the commands need installed inside a verification worktree before
# they can run. A worktree is a checkout of one revision and nothing
# else: an ecosystem that keeps its dependencies in the tree (npm, pnpm,
# yarn, bun, uv, poetry, bundler, composer) has none there until this
# runs. Go and Rust need no step — their tools fetch into shared caches.
# It runs first, in EVERY worktree the commands run in (the base, the
# task worktree, each candidate — five `npm ci` in a three-attempt run),
# and it is executable authority like the commands: hashed into the
# trust grant, never inferred from a lockfile. `relais doctor` says when
# a lockfile is present and no setup is declared.
# [[verification.profiles.default.setup]]
# argv = ["npm", "ci"]
# timeout_seconds = 600

[[verification.profiles.default.commands]]
argv = ["make", "check"]
timeout_seconds = 300

# A candidate that touches what the profile's verdict depends on — build
# manifests, lockfiles, the test tree, fixtures, or a program the profile
# runs — is reviewed explicitly whatever the route said (SPEC §10). The
# built-in list covers the common ones; add this repository's own here.
# [verification.profiles.default]
# inputs = ["scripts/check.sh", "ci/**"]
# amont check IDs this profile requires to be in force. Left empty, every
# check the inventory reports as in force at `block` severity is required.
# amont_checks = ["pre-push-cargo-test"]
# Bypasses and severity downgrades this profile accepts. A bypassed or
# downgraded check cannot fail the gate, so by default it is a
# verification GAP and the run ends needs_decision (SPEC §10). Listing an
# ID here is a reviewed waiver in policy — the run can never grant one.
# amont_waivers = ["pre-push-cargo-test"]
# Cache baseline results by base SHA, profile and toolchain (SPEC §18).
# Off by default: only a profile with no undeclared external dependency
# and no nondeterministic check is safe to cache.
# cache_baseline = true

# Risk floors: writes touching these patterns cannot route below the
# minimum tier, and the review requirement here is a floor, not a hint.
# Floors apply to the DECLARED scope, over-approximated: a contract
# scoped `src/**` COULD write `src/trust/x`, so a `**/trust/**` rule
# floors it. That is why no rule ships enabled. A leading-`**` pattern
# matches every scope that ends in `**`, which is nearly every ordinary
# contract, so one such rule pins the whole repository to the escalation
# tier with mandatory review and the cheap tiers become unreachable.
#
# Write rules against the real directories instead of a `**` prefix
# (`crates/relais/src/trust/**`, not `**/trust/**`), and scope contracts
# as narrowly as the task allows.
# [[risk]]
# paths = ["crates/*/src/trust/**", "crates/*/src/restore/**"]
# minimum_tier = "escalation"
# review = "required"

# Path-to-aval-key mappings. aval resolves exact keys; it has no
# source-impact analysis, so the mapping lives here (SPEC §7).
# [[architecture.mapping]]
# paths = ["crates/**"]
# keys = ["storage.object-store"]
# scope = "default"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const REPO_TOML: &str = r#"
schema_version = 1

[models.research]
id = "haiku"

[models.implementation]
id = "sonnet"
effort = "medium"

[models.escalation]
id = "fable"
effort = "medium"

[execution]
max_attempts = 3
max_repairs_before_escalation = 1
max_wall_seconds = 1200

[integrations]
aval = "required"
amont = "required"
amont_agent = "required"

[[verification.profiles.rust-change.commands]]
argv = ["make", "check"]
timeout_seconds = 300

[[risk]]
paths = ["**/trust/**", "**/restore/**"]
minimum_tier = "escalation"
review = "required"

[[architecture.mapping]]
paths = ["crates/amont/**"]
keys = ["output.contract"]
"#;

    fn contract() -> TaskContract {
        TaskContract::from_json_str(
            r#"{
              "schema_version": 1, "kind": "change",
              "objective": "Fix JSON escaping",
              "base_ref": "HEAD",
              "write_scope": ["crates/amont/**"],
              "acceptance": ["output parses"],
              "verification_profile": "rust-change",
              "limits": {"attempts": 3, "wall_seconds": 1200},
              "review": "optional"
            }"#,
        )
        .expect("contract parses")
    }

    fn machine_toml(extra: &str) -> String {
        format!("schema_version = 1\n{}\n", extra)
    }

    fn identity() -> RepoIdentity {
        RepoIdentity::origin("git@example.invalid:me/relais.git")
    }

    fn grant_for(policy: &RepoPolicy) -> String {
        grant_for_repo(policy, &identity())
    }

    fn grant_for_repo(policy: &RepoPolicy, repo: &RepoIdentity) -> String {
        let key = grant_key(&policy.authority_hash(), repo);
        format!("[trust.\"{key}\"]\ngranted_at = \"2026-09-18\"\nreviewed_by = \"me\"\n")
    }

    fn authority(repo: &RepoPolicy, machine: &MachineSettings) -> EffectiveAuthority {
        effective_authority(repo, machine, &contract(), &identity())
    }

    #[test]
    fn parses_the_spec_example() {
        let policy = RepoPolicy::from_toml_str(REPO_TOML).expect("spec example parses");
        assert_eq!(policy.models[&Tier::Implementation].id, "sonnet");
        assert_eq!(
            policy.models[&Tier::Implementation].effort,
            Some(Effort::Medium)
        );
        assert_eq!(policy.risk[0].minimum_tier, Tier::Escalation);
        assert_eq!(policy.risk[0].review, Some(Review::Required));
        assert_eq!(
            policy.verification.profiles["rust-change"].commands[0].argv,
            ["make", "check"]
        );
        assert_eq!(
            policy.integrations.aval.as_ref().unwrap().mode(),
            DependencyMode::Required
        );
        assert_eq!(policy.architecture.mapping[0].keys, ["output.contract"]);
    }

    #[test]
    fn rejects_unknown_fields() {
        let bad = REPO_TOML.replace("schema_version = 1", "schema_version = 1\nsurprise = true");
        assert!(matches!(
            RepoPolicy::from_toml_str(&bad).unwrap_err(),
            PolicyError::MalformedToml(_)
        ));
    }

    #[test]
    fn tier_ordering() {
        assert!(Tier::Research < Tier::Implementation);
        assert!(Tier::Implementation < Tier::Escalation);
        assert_eq!(Tier::Escalation.as_str(), "escalation");
    }

    #[test]
    fn intersection_narrows_never_broadens() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine = MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo)))
            .expect("machine parses");
        let mut c = contract();

        let a = effective_authority(&repo, &machine, &c, &identity());
        assert!(a.trust_granted);
        assert!(a.blockers.is_empty(), "{:?}", a.blockers);
        assert_eq!(a.max_attempts, 3);

        c.limits.attempts = Some(1);
        let a = effective_authority(&repo, &machine, &c, &identity());
        assert_eq!(a.max_attempts, 1, "contract limits narrow authority");
        c.limits.attempts = Some(3);

        let mut machine_restricted = machine.clone();
        machine_restricted.allowed_models = Some(vec!["haiku".into()]);
        let a = effective_authority(&repo, &machine_restricted, &c, &identity());
        assert_eq!(
            a.models.keys().collect::<Vec<_>>(),
            [&Tier::Research],
            "machine allowed_models intersects repo models"
        );
    }

    #[test]
    fn missing_grant_blocks() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine = MachineSettings::from_toml_str(&machine_toml("")).expect("machine parses");
        let a = authority(&repo, &machine);
        assert!(!a.trust_granted);
        assert!(a
            .blockers
            .iter()
            .any(|b| b.code == BlockCode::MissingTrustGrant));
    }

    /// A setup step runs the repository's own lifecycle scripts inside a
    /// worktree relais owns: executable authority, so declaring one must
    /// invalidate a grant reviewed without it (SPEC §5).
    #[test]
    fn a_declared_setup_step_changes_the_authority_hash() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let with_setup = RepoPolicy::from_toml_str(&REPO_TOML.replace(
            "[[verification.profiles.rust-change.commands]]",
            "[[verification.profiles.rust-change.setup]]\nargv = [\"npm\", \"ci\"]\n\n\
             [[verification.profiles.rust-change.commands]]",
        ))
        .expect("parses");
        assert_eq!(
            with_setup.verification.profiles["rust-change"].setup[0].argv,
            vec!["npm", "ci"]
        );
        assert_ne!(
            repo.authority_hash(),
            with_setup.authority_hash(),
            "a setup step is executable authority; declaring one must change the hash"
        );
        let a = effective_authority(&with_setup, &machine, &contract(), &identity());
        assert!(!a.trust_granted, "the grant was reviewed without the setup");
    }

    #[test]
    fn an_empty_setup_argv_is_rejected() {
        let err = RepoPolicy::from_toml_str(&REPO_TOML.replace(
            "[[verification.profiles.rust-change.commands]]",
            "[[verification.profiles.rust-change.setup]]\nargv = []\n\n\
             [[verification.profiles.rust-change.commands]]",
        ))
        .expect_err("an empty setup argv is a policy mistake");
        assert!(matches!(err, PolicyError::EmptyCommandArgv), "{err}");
    }

    #[test]
    fn changed_declaration_invalidates_grant() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let mut changed = repo.clone();
        changed.models.get_mut(&Tier::Implementation).unwrap().id = "sonnet-2".into();
        assert_ne!(
            repo.authority_hash(),
            changed.authority_hash(),
            "changing executable authority must change the hash"
        );
        let a = effective_authority(&changed, &machine, &contract(), &identity());
        assert!(!a.trust_granted);
    }

    #[test]
    fn unknown_verification_profile_blocks() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let mut c = contract();
        c.verification_profile = "not-a-profile".into();
        let a = effective_authority(&repo, &machine, &c, &identity());
        assert!(a
            .blockers
            .iter()
            .any(|b| b.code == BlockCode::VerificationProfileUnknown));
    }

    #[test]
    fn risk_review_floor_is_maxed_not_minced() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let a = authority(&repo, &machine);
        assert_eq!(a.review_floor, Review::Required);
    }

    #[test]
    fn permissions_floor_cannot_be_shortened_by_repo() {
        let machine = MachineSettings::from_toml_str(&machine_toml("")).expect("parses");
        assert!(machine
            .permissions
            .disallowed_tools
            .contains(&"Bash(git push:*)".to_string()));
    }

    /// The deny floor is a floor. `disallowed_tools = []` in machine.toml
    /// used to replace the shipped list wholesale, and nothing else
    /// re-added the commit/merge/push denials the doc promised.
    #[test]
    fn an_empty_machine_deny_list_cannot_remove_the_floor() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine = MachineSettings::from_toml_str(&machine_toml(&format!(
            "{}\n[permissions]\ndisallowed_tools = []\n",
            grant_for(&repo)
        )))
        .expect("parses");
        assert!(
            machine.permissions.disallowed_tools.is_empty(),
            "machine.toml says what it says"
        );
        let a = authority(&repo, &machine);
        for denied in [
            "Bash(git commit:*)",
            "Bash(git merge:*)",
            "Bash(git push:*)",
            "Bash(git rebase:*)",
            "Bash(git reset:*)",
            "Bash(git tag:*)",
        ] {
            assert!(
                a.disallowed_tools.contains(&denied.to_string()),
                "{denied} survives an empty machine deny list"
            );
        }
    }

    #[test]
    fn a_machine_deny_list_adds_to_the_floor_without_duplicating_it() {
        let extra = Permissions {
            disallowed_tools: vec!["Bash(git push:*)".into(), "WebFetch".into()],
            allowed_tools: Vec::new(),
        };
        let effective = effective_disallowed_tools(&extra);
        assert_eq!(
            effective
                .iter()
                .filter(|t| *t == "Bash(git push:*)")
                .count(),
            1,
            "a rule already on the floor is not repeated"
        );
        assert!(effective.contains(&"WebFetch".to_string()));
        assert!(effective.len() > default_disallowed_tools().len());
    }

    /// P4: the hash is derived from the struct, so a field added to
    /// `RepoPolicy` without touching `authority_hash` is inside it.
    #[test]
    fn the_hashed_value_is_the_whole_policy_minus_the_exclusions() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let serialized = serde_json::to_value(&repo).expect("serializes");
        let all: std::collections::BTreeSet<&str> = serialized
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        let authority = repo.authority_value();
        let hashed: std::collections::BTreeSet<&str> = authority
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        let expected: std::collections::BTreeSet<&str> = all
            .iter()
            .copied()
            .filter(|key| !AUTHORITY_EXCLUSIONS.contains(key))
            .collect();
        assert_eq!(hashed, expected);
        assert!(all.contains("schema_version") && !hashed.contains("schema_version"));
    }

    /// P2: the same declaration in a second repository is a second
    /// review. A grant issued for one must not open the other.
    #[test]
    fn a_grant_is_bound_to_the_repository_as_well_as_the_declaration() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let elsewhere = RepoIdentity::origin("git@example.invalid:someone/else.git");
        assert_ne!(
            grant_key(&repo.authority_hash(), &identity()),
            grant_key(&repo.authority_hash(), &elsewhere),
            "identical policy text, different repository, different key"
        );
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        assert!(authority(&repo, &machine).trust_granted);
        assert!(
            !effective_authority(&repo, &machine, &contract(), &elsewhere).trust_granted,
            "the other repository runs unreviewed until it is reviewed"
        );
        // A repository with no origin is still identified, by its
        // common git directory.
        let no_origin = RepoIdentity::common_dir(std::path::Path::new("/repos/relais/.git"));
        assert_ne!(
            grant_key(&repo.authority_hash(), &no_origin),
            grant_key(&repo.authority_hash(), &identity())
        );
        assert_eq!(no_origin.label(), "/repos/relais/.git");
        assert_eq!(no_origin.to_string(), "/repos/relais/.git (no origin)");
        assert_eq!(identity().label(), "git@example.invalid:me/relais.git");
        assert_eq!(identity().to_string(), "git@example.invalid:me/relais.git");
    }

    /// The identity's serialized shape is hashed into every grant key,
    /// so it is a wire format: these two keys are what a `machine.toml`
    /// written by this version holds, and a change here re-keys every
    /// grant on every machine. Pinned so that is a deliberate release
    /// note, never a surprise.
    #[test]
    fn the_grant_key_shape_is_pinned() {
        assert_eq!(
            serde_json::to_value(identity()).expect("serializes"),
            serde_json::json!({ "origin": "git@example.invalid:me/relais.git" })
        );
        assert_eq!(
            serde_json::to_value(RepoIdentity::common_dir(std::path::Path::new("/r/.git")))
                .expect("serializes"),
            serde_json::json!({ "common_dir": "/r/.git" })
        );
        assert_eq!(
            grant_key("authority", &identity()),
            "ff25e7845522d451e49c35865572e3a48a295dcf806caecde55b3afa345dcaa5"
        );
    }

    /// `repo_key` hashes only the identity — no authority hash — so a
    /// task stays keyed to the same repository across a policy edit that
    /// would re-key a trust grant. It still moves when the identity's
    /// wire shape does, for the same reason `grant_key`'s does.
    #[test]
    fn repo_key_is_bound_to_the_identity_alone() {
        let elsewhere = RepoIdentity::origin("git@example.invalid:someone/else.git");
        assert_ne!(repo_key(&identity()), repo_key(&elsewhere));
        assert_eq!(
            repo_key(&identity()),
            repo_key(&identity()),
            "pure: the same identity always hashes the same"
        );
    }

    #[test]
    fn the_missing_grant_blocker_names_the_key_to_paste() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine = MachineSettings::from_toml_str(&machine_toml("")).expect("parses");
        let a = authority(&repo, &machine);
        let blocker = a
            .blockers
            .iter()
            .find(|b| b.code == BlockCode::MissingTrustGrant)
            .expect("blocked on the grant");
        assert!(blocker.detail.contains(&a.grant_key), "{}", blocker.detail);
        assert!(blocker.detail.contains("git@example.invalid:me/relais.git"));
    }

    /// P7 and P10 at the machine boundary: a ceiling is a non-negative
    /// amount of money and a grant names a reviewer and a real date.
    #[test]
    fn machine_settings_validate_ceilings_and_grants() {
        assert_eq!(
            MachineSettings::from_toml_str(&machine_toml("[spending]\nper_run_micros = -1\n"))
                .unwrap_err(),
            PolicyError::NegativeCeiling {
                field: "per_run_micros",
                micros: -1
            }
        );
        assert_eq!(
            MachineSettings::from_toml_str(&machine_toml("[spending]\nper_day_micros = -5\n"))
                .unwrap_err(),
            PolicyError::NegativeCeiling {
                field: "per_day_micros",
                micros: -5
            }
        );
        let ok = MachineSettings::from_toml_str(&machine_toml(
            "[spending]\nper_run_micros = 2500\nper_day_micros = 10000\n",
        ))
        .expect("non-negative ceilings parse");
        assert_eq!(
            ok.spending.per_run_micros,
            Some(MicroUsd::from_micros(2500))
        );

        // granted_at must be a date this relais can read.
        let bad = MachineSettings::from_toml_str(&machine_toml(
            "[trust.\"k\"]\ngranted_at = \"yesterday\"\nreviewed_by = \"me\"\n",
        ))
        .unwrap_err();
        assert!(
            matches!(&bad, PolicyError::InvalidTrustGrant { key, .. } if key == "k"),
            "{bad}"
        );
        assert!(bad.to_string().contains("yesterday"), "{bad}");
        // …and reviewed_by is required, not an optional nicety.
        let missing = MachineSettings::from_toml_str(&machine_toml(
            "[trust.\"k\"]\ngranted_at = \"2026-09-18\"\n",
        ))
        .unwrap_err();
        assert!(
            matches!(missing, PolicyError::MalformedToml(_)),
            "{missing}"
        );
        let empty = MachineSettings::from_toml_str(&machine_toml(
            "[trust.\"k\"]\ngranted_at = \"2026-09-18\"\nreviewed_by = \"  \"\n",
        ))
        .unwrap_err();
        assert!(matches!(empty, PolicyError::InvalidTrustGrant { .. }));

        // Both date spellings machine.toml uses are read.
        let both = MachineSettings::from_toml_str(&machine_toml(
            "[trust.\"a\"]\ngranted_at = \"2026-09-18\"\nreviewed_by = \"me\"\n\
             [trust.\"b\"]\ngranted_at = \"2026-09-18T10:00:00+02:00\"\nreviewed_by = \"me\"\n\
             repo = \"git@example.invalid:me/relais.git\"\n",
        ))
        .expect("parses");
        assert!(both.trust["a"].granted_at().is_ok());
        assert!(both.trust["b"].granted_at().is_ok());
        assert_eq!(
            both.trust["b"].repo.as_deref(),
            Some("git@example.invalid:me/relais.git")
        );
    }

    /// The rules match a command-line prefix, so denying the verb is not
    /// denying the operation: every wrapper that can be named as a rule
    /// is named (audit B7). The list is a floor, not a sandbox.
    #[test]
    fn the_deny_floor_names_the_prefix_match_evasions() {
        let machine = MachineSettings::from_toml_str(&machine_toml("")).expect("parses");
        let floor = machine.permissions.disallowed_tools;
        for rule in [
            "Bash(git commit:*)",
            "Bash(git -C:*)",
            "Bash(git -c:*)",
            "Bash(git --git-dir:*)",
            "Bash(sh -c:*)",
            "Bash(bash -c:*)",
            "Bash(zsh -c:*)",
            "Bash(env git:*)",
            "Bash(eval:*)",
        ] {
            assert!(floor.contains(&rule.to_string()), "{rule} is on the floor");
        }
    }

    #[test]
    fn allowed_tools_are_machine_owned_and_empty_by_default() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        assert!(machine.permissions.allowed_tools.is_empty());
        let a = authority(&repo, &machine);
        assert!(a.allowed_tools.is_empty(), "no grant, no tools");

        let granted = MachineSettings::from_toml_str(&machine_toml(&format!(
            "{}\n[permissions]\nallowed_tools = [\"Edit\", \"Write\", \"Bash(cargo test:*)\"]\n",
            grant_for(&repo)
        )))
        .expect("parses");
        let a = authority(&repo, &granted);
        assert_eq!(a.allowed_tools, ["Edit", "Write", "Bash(cargo test:*)"]);
        assert!(
            a.disallowed_tools.contains(&"Bash(git push:*)".to_string()),
            "naming allowed tools never shortens the deny floor"
        );
    }

    #[test]
    fn recipes_are_hashed_as_executable_authority() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let with_recipe = RepoPolicy::from_toml_str(&format!(
            "{REPO_TOML}\n[[recipes]]\nname = \"docs\"\nscope_within = [\"docs/**\"]\ntier = \"research\"\n"
        ))
        .expect("parses");
        assert_eq!(with_recipe.recipes.len(), 1);
        assert_ne!(
            repo.authority_hash(),
            with_recipe.authority_hash(),
            "a recipe picks a tier ahead of the learner: adding one is an authority change"
        );
        // A grant bound to the recipe-less declaration does not carry over.
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        assert!(
            !effective_authority(&with_recipe, &machine, &contract(), &identity()).trust_granted
        );
    }

    #[test]
    fn the_context_budget_is_repo_policy_and_part_of_the_authority() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        assert_eq!(
            repo.context.budget_bytes, DEFAULT_CONTEXT_BUDGET_BYTES,
            "a policy that says nothing keeps the shipped budget"
        );
        let wider =
            RepoPolicy::from_toml_str(&format!("{REPO_TOML}\n[context]\nbudget_bytes = 131072\n"))
                .expect("parses");
        assert_eq!(wider.context.budget_bytes, 131_072);
        assert_ne!(
            repo.authority_hash(),
            wider.authority_hash(),
            "raising the budget is what turns a sizing problem into a dispatch: an authority change"
        );
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        assert!(!effective_authority(&wider, &machine, &contract(), &identity()).trust_granted);
    }

    #[test]
    fn integration_table_form() {
        let repo = RepoPolicy::from_toml_str(
            r#"
schema_version = 1
[integrations]
amont = { mode = "optional", bin = "/opt/amont/bin/amont" }
"#,
        )
        .expect("table form parses");
        let dep = repo.integrations.amont.unwrap();
        assert_eq!(dep.mode(), DependencyMode::Optional);
        assert_eq!(dep.bin(), Some("/opt/amont/bin/amont"));
    }

    #[test]
    fn init_template_is_valid_policy() {
        let policy = RepoPolicy::from_toml_str(INIT_TEMPLATE).expect("template parses");
        assert_eq!(policy.models.len(), 3);
        assert!(policy.verification.profiles.contains_key("default"));
        assert!(
            policy.risk.is_empty(),
            "the template ships risk rules as commented examples: a live \
             leading-`**` rule floors every `…/**` scope to escalation and \
             makes the cheap tiers unreachable"
        );
    }
    /// A repository that raises its ceiling reaches the runs. The
    /// contract's limits used to DEFAULT to 3 attempts and 1200 seconds,
    /// and the intersection took them, so a repository that moved its
    /// wall clock to 2700 s kept watching workers killed at exactly
    /// 1200 — twice on this machine, both times read as the model
    /// running long. A limit the contract does not write is not a
    /// narrowing the author chose.
    #[test]
    fn an_unwritten_contract_limit_inherits_the_repositorys() {
        let mut repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        repo.execution.max_wall_seconds = 2700;
        repo.execution.max_attempts = 5;
        let machine = MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo)))
            .expect("machine parses");

        let mut contract = contract();
        contract.limits = crate::contract::Limits::default();
        let a = effective_authority(&repo, &machine, &contract, &identity());
        assert_eq!(
            a.max_wall_seconds, 2700,
            "an unwritten wall clock is the repository's"
        );
        assert_eq!(a.max_attempts, 5, "and so are its attempts");

        // What the contract DOES write still narrows, and still cannot
        // broaden: 9000 is past the repository's ceiling.
        contract.limits.wall_seconds = Some(600);
        contract.limits.attempts = Some(9000);
        let a = effective_authority(&repo, &machine, &contract, &identity());
        assert_eq!(a.max_wall_seconds, 600, "a written limit narrows");
        assert_eq!(
            a.max_attempts, 5,
            "and a written one past the ceiling does not broaden"
        );
    }
}
