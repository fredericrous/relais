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
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::contract::{Review, TaskContract};
use crate::ids::canonical_json_hash;
use crate::money::MicroUsd;

mod recipe;
pub use recipe::{
    select_highest_enabled_revision, validate_recipes, CoveringRecipe, RecipeError, RecipeRecord,
    RecipeSpec,
};

/// The highest `relais.toml` `schema_version` this relais understands.
/// `RepoPolicy::validate` accepts every version from
/// [`MIN_POLICY_SCHEMA_VERSION`] through this one — the format is
/// additive, so a repository's existing declaration keeps working
/// unchanged after an upgrade (SPEC §5). `schema_version` itself is
/// outside the authority hash ([`AUTHORITY_EXCLUSIONS`]), so bumping this
/// constant never invalidates a grant on its own.
pub const POLICY_SCHEMA_VERSION: u64 = 2;

/// The lowest `relais.toml` `schema_version` `RepoPolicy::validate`
/// accepts.
pub const MIN_POLICY_SCHEMA_VERSION: u64 = 1;

/// `machine.toml`'s own schema version, checked separately from
/// [`POLICY_SCHEMA_VERSION`]: machine-owned settings are a different
/// format from repo policy, and did not change when recipes gained
/// revisions, so this stays fixed rather than tracking the repo policy
/// version upward.
pub const MACHINE_SCHEMA_VERSION: u64 = 1;

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

/// An effort identifier: data, not a closed list. What levels exist is a
/// fact about a harness and a model (see [`crate::catalog`]), so this type
/// only guarantees the spelling, `^[a-z][a-z0-9_-]{0,31}$`, checked when
/// it is deserialized. It serializes as the plain string, so a policy
/// that already names `effort = "medium"` hashes exactly as before.
///
/// There is deliberately no ordering here: which level is above which is
/// the catalog's configured order, never a property of the name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EffortId(String);

/// The text is not an effort identifier; names the value that was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortIdError {
    pub value: String,
}

impl std::fmt::Display for EffortIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` is not an effort identifier (lowercase letter, then up to 31 of a-z, 0-9, `_` or `-`)",
            self.value
        )
    }
}

impl std::error::Error for EffortIdError {}

impl EffortId {
    pub fn parse(text: &str) -> Result<Self, EffortIdError> {
        let mut chars = text.chars();
        let well_formed = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && text.len() <= 32
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if well_formed {
            Ok(Self(text.to_string()))
        } else {
            Err(EffortIdError {
                value: text.to_string(),
            })
        }
    }

    /// The stored spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EffortId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EffortId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    /// Explicit model ID. Recorded in the effective model fields of every
    /// dispatch and report.
    pub id: String,
    /// Omitted for models without effort control.
    #[serde(default)]
    pub effort: Option<EffortId>,
    /// The highest effort this tier may ever be dispatched at: the
    /// AUTHORITY policy's ceiling for the tier (SPEC §6). Read from the
    /// repository policy only — a recipe's own copy is never consulted, so
    /// a recipe lowering or raising its start effort cannot move it. Absent,
    /// the ceiling is the top of the model's admissible set. Omitted from
    /// the serialized form when absent, so no existing authority hash moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_effort: Option<EffortId>,
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
    /// The name a declared acceptance criterion's `Evidence::Check`
    /// names. Omitted from the serialized form when absent, so the
    /// authority hash of a policy that never named a check is unchanged
    /// by this field existing at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub argv: Vec<String>,
    #[serde(default = "default_command_timeout")]
    pub timeout_seconds: u64,
    /// A JUnit XML report this command writes, relative to the worktree
    /// it runs in. After the command runs, verification reads it for
    /// per-test results, which is what lets a declared criterion name one
    /// test (SPEC §10, #51). The command's own pass/fail stays its exit
    /// status. Omitted from the serialized form when absent, so the
    /// authority hash of a policy that declares none is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub junit: Option<String>,
}

/// The command a profile defines under `name`, if any — what a declared
/// criterion's `Evidence::Check { name }` refers to.
pub fn named_command<'a>(profile: &'a VerificationProfile, name: &str) -> Option<&'a CommandSpec> {
    profile
        .commands
        .iter()
        .find(|command| command.name.as_deref() == Some(name))
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

impl VerificationProfile {
    /// A pure, content-derived hash of this profile alone: setup,
    /// commands, amont checks/waivers, inputs and `cache_baseline`.
    /// Distinct from [`RepoPolicy::authority_hash`], which moves whenever
    /// anything in the policy moves — another profile, a model, a recipe
    /// promotion — even when none of it touches verification. This hash
    /// answers "was this run judged by the same checks", which the
    /// whole-policy hash cannot, and is what [`crate::verify::Receipt`]
    /// binds as `verification_profile_hash`.
    pub fn hash(&self) -> String {
        canonical_json_hash(&serde_json::to_value(self).expect("VerificationProfile serializes"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskRule {
    pub paths: Vec<String>,
    pub minimum_tier: Tier,
    #[serde(default)]
    pub review: Option<Review>,
    /// The lowest effort a task touching these paths may be dispatched at.
    /// Which effort is above which is the catalog's configured order, so
    /// the router ranks it there. Omitted from the serialized form when
    /// absent, so no existing authority hash moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_effort: Option<EffortId>,
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
    /// NOT a worker permission. It declares the agent-tree limits the
    /// coordinator enforces for hook-admitted sessions (SPEC §23), and
    /// nothing else: a print-mode worker never spawns a subagent whatever
    /// this says, because the deny floor refuses `Agent` and `Task`
    /// ([`default_disallowed_tools`]). Do not read `true` as "workers may
    /// nest".
    #[serde(default = "default_allow_nested_agents")]
    pub allow_nested_agents: bool,
    #[serde(default = "default_max_agent_depth")]
    pub max_agent_depth: u32,
    #[serde(default = "default_max_agents_total")]
    pub max_agents_total: u32,
    /// Whether a repair climbs effort within its tier (`raise`, the
    /// default) or keeps the previous attempt's (`same`). Left out of the
    /// serialized declaration while it is the default, so a policy that
    /// never names it keeps its authority hash.
    #[serde(default, skip_serializing_if = "RepairEffort::is_default")]
    pub repair_effort: RepairEffort,
}

/// How a repair's effort follows the attempt it repairs (SPEC §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RepairEffort {
    /// The next admissible effort in the model's order, never above the
    /// tier's ceiling.
    #[default]
    Raise,
    /// The previous attempt's effort, unchanged.
    Same,
}

impl RepairEffort {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raise => "raise",
            Self::Same => "same",
        }
    }
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

impl RepoPolicy {
    pub fn from_toml_str(text: &str) -> Result<Self, PolicyError> {
        let policy: RepoPolicy =
            toml::from_str(text).map_err(|e| PolicyError::MalformedToml(e.to_string()))?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), PolicyError> {
        if !(MIN_POLICY_SCHEMA_VERSION..=POLICY_SCHEMA_VERSION).contains(&self.schema_version) {
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
        validate_recipes(&self.recipes).map_err(PolicyError::InvalidRecipe)?;
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
    /// A `[[recipes]]` table with two recipes sharing `(name, revision)`
    /// or two recipes sharing a [`RecipeSpec::recipe_id`].
    InvalidRecipe(RecipeError),
    /// A `[sandbox]` entry the worker sandbox could not honour or must
    /// not grant.
    InvalidSandbox {
        field: &'static str,
        detail: String,
    },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(v) => write!(
                f,
                "unsupported relais.toml schema_version {v} (this relais understands \
                 {MIN_POLICY_SCHEMA_VERSION}..={POLICY_SCHEMA_VERSION})"
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
            Self::InvalidRecipe(err) => write!(f, "{err}"),
            Self::InvalidSandbox { field, detail } => write!(f, "[sandbox] {field}: {detail}"),
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
    /// What a hook-admitted agent needs that the rest of this struct does
    /// not already carry. See [`HookAdmissionSettings`] for what is and
    /// is not here — the agent and depth caps stay in [`ConcurrencyLimits`]
    /// rather than being redeclared.
    #[serde(default)]
    pub admission: HookAdmissionSettings,
    /// Orchestration usage prices (SPEC §11), read from `[pricing]` and
    /// never from constants in the code — an Anthropic price change is a
    /// config edit. `None` when machine.toml has no `[pricing]` block;
    /// callers treat that the same as an empty table (every record prices
    /// `Unknown`). Machine policy, not repo policy: this field belongs to
    /// `MachineSettings`, not [`RepoPolicy`], so it never feeds
    /// [`RepoPolicy::authority_hash`] and never invalidates a trust
    /// grant.
    #[serde(default)]
    pub pricing: Option<crate::orchestration::PriceTable>,
    /// What efforts a model supports, and in what order. Machine-owned
    /// like `[pricing]`: it feeds no authority hash. Every key is optional
    /// and `doctor` reports what is missing.
    #[serde(default)]
    pub efforts: EffortSettings,
    /// Worker OS sandbox configuration (SPEC §8). Machine-owned like
    /// `[pricing]`: it feeds no authority hash.
    #[serde(default)]
    pub sandbox: SandboxSettings,
}

/// `[sandbox]` in machine.toml: what an OS-sandboxed worker may touch
/// beyond its own worktree. Disabled unless the machine says otherwise;
/// the credential floor is not configurable away (see `crate::sandbox`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SandboxSettings {
    pub enabled: bool,
    /// Extra writable directories, absolute or `~/`-relative.
    pub writable: Vec<String>,
    /// Domains the worker may reach, e.g. `api.anthropic.com`.
    pub network: Vec<String>,
    /// Extra paths the worker may not read, absolute or `~/`-relative.
    pub deny_read: Vec<String>,
}

/// A machine.toml path entry made absolute: `~` and `~/…` against `home`,
/// an absolute path as it is, and `None` for anything else. Pure, so the
/// validator and the settings builder cannot disagree about spelling.
pub fn expand_home(entry: &str, home: &Path) -> Option<PathBuf> {
    let path = if entry == "~" {
        home.to_path_buf()
    } else if let Some(rest) = entry.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(entry)
    };
    // `..` is refused in EVERY form: `~/..` joined as written became the
    // home's parent, and `Path::starts_with` compares components without
    // resolving `..`, so the ancestor guard below never saw it.
    let clean = path.is_absolute()
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir));
    clean.then_some(path)
}

/// Whether `name` is a plain domain: letters, digits, `-` and `.`, at
/// least one dot, no empty label, no scheme, path or wildcard.
fn is_plain_domain(name: &str) -> bool {
    name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// Credential directories under `$HOME`: the credential floor's defaults
/// (`crate::sandbox`), named here so `writable` can be checked against them
/// without this module depending on the sandbox.
pub const FLOOR_HOME_DIRS: &[&str] = &[".ssh", ".gnupg", ".aws", ".kube", "Library/Keychains"];
/// Credential directories under the config root (`~/.config`).
pub const FLOOR_CONFIG_DIRS: &[&str] = &["gh", "gcloud", "mtls"];
/// Credential files directly under `$HOME`.
pub const FLOOR_HOME_FILES: &[&str] = &[
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".claude.json",
];
/// Where the credential files of cargo, docker and Claude Code live under
/// `$HOME` by default, each with the files inside it.
pub const FLOOR_CARGO_DIR: (&str, &[&str]) = (".cargo", &["credentials", "credentials.toml"]);
pub const FLOOR_DOCKER_DIR: (&str, &[&str]) = (".docker", &["config.json"]);
pub const FLOOR_CLAUDE_DIR: (&str, &[&str]) = (".claude", &[".credentials.json"]);
/// relais's own default config dir and ledger, relative to `$HOME`.
pub const FLOOR_RELAIS_CONFIG: &str = ".config/relais";
pub const FLOOR_RELAIS_LEDGER: &str = ".local/state/relais/ledger.sqlite";

/// Every default credential-floor directory and file under `home`, and
/// nothing an environment variable relocated.
pub fn default_floor_paths(home: &Path) -> Vec<PathBuf> {
    let config = home.join(".config");
    let mut paths: Vec<PathBuf> = FLOOR_HOME_DIRS.iter().map(|d| home.join(d)).collect();
    paths.extend(FLOOR_CONFIG_DIRS.iter().map(|d| config.join(d)));
    paths.push(home.join(FLOOR_RELAIS_CONFIG));
    paths.push(home.join(FLOOR_RELAIS_LEDGER));
    paths.extend(FLOOR_HOME_FILES.iter().map(|f| home.join(f)));
    for (dir, files) in [FLOOR_CARGO_DIR, FLOOR_DOCKER_DIR, FLOOR_CLAUDE_DIR] {
        paths.extend(files.iter().map(|f| home.join(dir).join(f)));
    }
    paths
}

/// `path` and, when it resolves, its symlink-free form.
fn spellings(path: &Path) -> Vec<PathBuf> {
    // `Path::canonicalize` fails for a path that does not exist yet; such
    // a path has only its given form to compare.
    let mut forms = vec![path.to_path_buf()];
    forms.extend(path.canonicalize().ok().filter(|c| c != path));
    forms
}

impl SandboxSettings {
    /// Refuse what a sandbox could not honour or must not grant. `home`,
    /// `state_dir` and `config_dir` are parameters, so a caller that knows
    /// them gets the checks that need them; `home` absent falls back to a
    /// stand-in (`~` itself is still the home directory, whatever it is).
    /// `floor_defaults` is the default credential floor: a `writable`
    /// entry equal to, or containing, any of it is refused, in its given
    /// and its symlink-resolved form. `MachineSettings::validate` calls
    /// this with the REAL home, state and config directories (it reads
    /// `crate::paths`).
    pub fn check(
        &self,
        home: Option<&Path>,
        state_dir: Option<&Path>,
        config_dir: Option<&Path>,
        floor_defaults: &[PathBuf],
    ) -> Result<(), PolicyError> {
        let invalid =
            |field: &'static str, entry: &str, detail: &str| PolicyError::InvalidSandbox {
                field,
                detail: format!("{entry:?}: {detail}"),
            };
        // A stand-in home keeps `~` entries expandable, and comparable to
        // the guarded directories, when the real one is not known.
        let base = home.unwrap_or(Path::new(STAND_IN_HOME));
        let expand = |field: &'static str, entry: &str| -> Result<PathBuf, PolicyError> {
            expand_home(entry, base).ok_or_else(|| {
                invalid(
                    field,
                    entry,
                    "must be absolute (or start with `~/`), without `..`",
                )
            })
        };
        for entry in &self.deny_read {
            expand("deny_read", entry)?;
        }
        for entry in &self.writable {
            let path = expand("writable", entry)?;
            // The config dir holds machine.toml and its trust grants: a
            // worker able to write it could grant itself authority.
            let guarded = [Some(Path::new("/")), Some(base), state_dir, config_dir]
                .into_iter()
                .flatten()
                .chain(floor_defaults.iter().map(PathBuf::as_path));
            let written = spellings(&path);
            if guarded.into_iter().any(|dir| {
                spellings(dir)
                    .iter()
                    .any(|form| written.iter().any(|w| form.starts_with(w)))
            }) {
                return Err(invalid(
                    "writable",
                    entry,
                    "is, or contains, `/`, the home directory, the state directory, the \
                     config directory or a credential-floor path",
                ));
            }
        }
        for name in &self.network {
            if !is_plain_domain(name) {
                return Err(invalid(
                    "network",
                    name,
                    "is not a plain domain (letters, digits, `-`, `.`, at least one dot; \
                     no scheme, path or wildcard)",
                ));
            }
        }
        Ok(())
    }
}

/// `[efforts]` in machine.toml: the order and per-model support the
/// catalog ([`crate::catalog`]) resolves from. Nothing here has a built-in
/// default — an absent key is an unknown fact, never a guessed one.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EffortSettings {
    /// The machine-wide order, lowest first. A model's own `order`
    /// overrides it.
    pub order: Option<Vec<EffortId>>,
    pub models: Vec<EffortModelEntry>,
}

/// One `[[efforts.models]]` entry: the facts about one or more models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffortModelEntry {
    /// Model IDs this entry describes.
    pub ids: Vec<String>,
    /// The efforts these models support; `[]` means no effort control.
    /// Absent means the support is not stated.
    #[serde(default)]
    pub supported: Option<Vec<EffortId>>,
    /// The order for these models, lowest first.
    #[serde(default)]
    pub order: Option<Vec<EffortId>>,
}

impl EffortSettings {
    /// The entry naming `model`, if any. Case-insensitive and trimmed,
    /// like [`crate::backend::model_matches`]'s exact-id comparison.
    pub fn entry_for(&self, model: &str) -> Option<&EffortModelEntry> {
        self.models.iter().find(|entry| {
            entry
                .ids
                .iter()
                .any(|id| id.trim().eq_ignore_ascii_case(model.trim()))
        })
    }
}

/// One substitution `machine.toml` reviewed and will accept without
/// ending a run — see [`MachineSettings::approved_substitutions`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedSubstitution {
    /// The model ID or alias a route requested.
    pub requested: String,
    /// The model ID the harness is permitted to report running instead.
    pub effective: String,
    /// Why this substitution is accepted — required in spirit, not in
    /// type: left optional so an older entry still parses, but every
    /// entry a person writes should carry one.
    #[serde(default)]
    pub note: Option<String>,
}

impl ApprovedSubstitution {
    /// Whether this entry covers exactly this requested/effective pair.
    /// Case-insensitive, trimmed, like [`crate::backend::model_matches`] —
    /// but an exact pair, never an alias match: this list names concrete
    /// substitutions a person reviewed, not a pattern.
    pub fn approves(&self, requested: &str, effective: &str) -> bool {
        self.requested.trim().eq_ignore_ascii_case(requested.trim())
            && self.effective.trim().eq_ignore_ascii_case(effective.trim())
    }
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
///
/// `Agent` and `Task` (its legacy name) are denied for a different reason:
/// model choice belongs to the route. MEASURED 2026-09-28
/// (run-65c89ac482819-d958): in print mode the Agent tool needs no
/// permission, and a sonnet worker handed its whole task to a background
/// `general-purpose` subagent with `model: "opus"` — 124 Opus 5.5 turns
/// against 14 of its own. That routed around relais's model choice, and
/// relais noticed only after the spend, as an unapproved substitution.
/// Falsified: with `Agent` removed from this list the test
/// `the_floor_denies_subagent_spawning` failed; restored.
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
        // A subagent chooses its own model. Model choice belongs to the
        // route, so a worker may not hand its task to one.
        "Agent".into(),
        "Task".into(),
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

/// `max_active_agents_per_session` and `max_agent_depth` are the agent
/// and depth caps: the coordinator enforces both (`coordinator::DEFAULT_LIMITS`,
/// `coordinator::effective_limits`), whose defaults live in
/// `coordinator::DEFAULT_LIMITS` and are deliberately NOT repeated here:
/// a number copied into prose is a second record of it, and changing
/// the constant would silently falsify the copy.
/// Nothing else in this crate redeclares them — a second home for either
/// number would be one fact recorded twice, free to drift apart.
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

/// Settings a hook-admitted agent needs that no other machine-owned type
/// carries. Deliberately narrow: this struct states values, it does not
/// admit an agent, bind a dispatch, reserve money or contact the
/// coordinator — those actions belong to the admission package this
/// struct exists for. The agent and depth caps are NOT here; see the
/// comment on [`ConcurrencyLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HookAdmissionSettings {
    /// How long a binding may be held before it lapses, in seconds,
    /// measured from the moment the hook records it. A binding older
    /// than this is treated as gone whether or not anything freed it
    /// explicitly — the mechanism a crashed or forgotten hook needs so a
    /// stale binding does not block admission forever.
    pub binding_lease_secs: u64,
    /// What one dispatch reserves against its session's money before the
    /// provider reports real usage. Zero is not "no limit enforced by
    /// accident" — it is the stated default, and it means observation
    /// rather than refusal: a hook admits every agent the rest of policy
    /// allows and tracks spend as it is reported, without holding any of
    /// it back up front. A machine that wants dispatch itself throttled
    /// on money sets this above zero explicitly.
    pub dispatch_reserve_micros: MicroUsd,
    /// What a hook does when the coordinator cannot be reached.
    pub on_coordinator_unreachable: CoordinatorUnreachableBehavior,
    /// How long a `PreToolUse` spawn queued for a seat may wait before
    /// the hook gives up, in seconds; zero means give up at once. This is
    /// the raw number machine.toml stores — see [`HookAdmissionSettings::queue_behaviour`],
    /// the one place that decides what zero means. Nothing else in this
    /// crate reads the field directly.
    ///
    /// Measured on Claude Code 2.1.282 (SPEC §23): a `PreToolUse` hook
    /// holds its tool call open for as long as it runs, so a hook CAN
    /// wait for a freed seat rather than refusing at once — this is the
    /// budget for that wait. Short by default (two seconds): the case
    /// this serves is an agent finishing right now, and a long default
    /// would buy stalls and widen the window in which a hook killed
    /// mid-wait can strand a seat (bounded instead by
    /// `admission::UNCLAIMED_GRACE`).
    pub queue_wait_secs: u64,
}

impl Default for HookAdmissionSettings {
    fn default() -> Self {
        Self {
            binding_lease_secs: 120,
            dispatch_reserve_micros: MicroUsd::ZERO,
            on_coordinator_unreachable: CoordinatorUnreachableBehavior::CarryOn,
            queue_wait_secs: DEFAULT_QUEUE_WAIT_SECS,
        }
    }
}

/// The default `queue_wait_secs`, named so the constant is not repeated
/// inside its own default and readable at a glance rather than derived
/// from the doc comment above it.
pub const DEFAULT_QUEUE_WAIT_SECS: u64 = 2;

/// What a queued `PreToolUse` spawn does about the wait: give up at once
/// (`queue_wait_secs = 0`, exactly today's behaviour), or hold the tool
/// call open and poll for a seat up to a bound. A named choice, not a
/// bare integer read for its sign or its zero-ness at each call site —
/// see [`HookAdmissionSettings::queue_behaviour`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueBehaviour {
    RefuseImmediately,
    WaitUpTo(Duration),
}

impl HookAdmissionSettings {
    /// The one place that decides what `queue_wait_secs` means: zero is
    /// [`QueueBehaviour::RefuseImmediately`], exactly as before this
    /// field existed; anything else is a bound to wait up to. Every other
    /// reader of the wait consults this, never the raw integer.
    pub fn queue_behaviour(&self) -> QueueBehaviour {
        match self.queue_wait_secs {
            0 => QueueBehaviour::RefuseImmediately,
            secs => QueueBehaviour::WaitUpTo(Duration::from_secs(secs)),
        }
    }
}

/// A named choice, not a bare boolean, so a reader of a settings file
/// sees which behaviour is configured rather than which flag is true.
///
/// Default is [`Self::CarryOn`]: SPEC §23 says a coordinator outage "must
/// not silently turn a strict managed launch into an unmanaged launch",
/// but a hook is not queuing a managed dispatch behind a retry — it is
/// holding open a tool call a person or another agent is waiting on. A
/// hook that refused every agent whenever the daemon was mid-restart
/// would be worse than the gap it guards against, so the default lets
/// the agent through and records the outage rather than blocking on it;
/// `Refuse` is for a machine that has decided the alternative — an
/// unrecorded agent — is the greater risk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorUnreachableBehavior {
    #[default]
    CarryOn,
    Refuse,
}

/// Authorized experimentation envelope (SPEC §13, §17, §28). Live trial
/// assignment stays inside it; anything wider needs an explicit edit of
/// machine.toml. Risk floors and required checks are never trainable
/// parameters.
///
/// MACHINE-OWNED and OFF by default: this lives in [`MachineSettings`],
/// never in [`RepoPolicy`], so a repository can neither enable trials nor
/// widen the envelope. Every field has a safe default that runs nothing:
/// `enabled = false`, no seed, no eligible kind, no candidate, and caps
/// of zero — so a machine that sets only `enabled = true` still draws no
/// arm. `enabled = true` without a `seed` makes every task not eligible;
/// an unseeded draw is never made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TrialEnvelope {
    pub enabled: bool,
    /// The machine's own randomization seed. Together with the task id
    /// and the UTC day it fixes the draw, so an assignment is
    /// reproducible from what the ledger records. TOML integers are
    /// signed 64-bit, so the largest seed a file can spell is `i64::MAX`.
    pub seed: Option<u64>,
    /// Task kinds that may be drawn into a trial. Empty: none.
    pub eligible_kinds: Vec<crate::contract::Kind>,
    /// Trials that may be created per UTC day. 0: none.
    pub max_daily_trials: u32,
    /// Ceiling on the day's recorded trial cost, in micro-USD; a trial
    /// whose cost is unknown counts as this full amount. 0: none.
    pub max_trial_cost_micros: i64,
    /// Candidate `relais.toml` files, each admitted at run time against
    /// the repository's current policy and its own trust grant.
    pub candidates: Vec<std::path::PathBuf>,
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
    /// Substitutions reviewed and accepted in advance: a dispatch that
    /// requested `requested` and ran `effective` instead stops being an
    /// unapproved substitution that ends the run (SPEC §6) and becomes an
    /// accepted [`crate::backend::ModelVerification::Approved`] one.
    /// Machine-owned only, like `allowed_models` on [`MachineSettings`] —
    /// accepting a costlier model is a spending decision a repository
    /// must not be able to widen on its own.
    pub approved_substitutions: Vec<ApprovedSubstitution>,
    /// The highest effort this machine authorizes, named by its position
    /// in the configured order: that entry and every one below it. It is
    /// spend authority, never evidence of what a harness or model supports.
    pub max_effort: EffortId,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            max_effort: EffortId("high".into()),
            learned_enabled: true,
            quality_floor: Some(0.75),
            min_supported_test_records: 20,
            max_abstention_rate: 0.5,
            approved_substitutions: Vec::new(),
        }
    }
}

impl MachineSettings {
    pub fn from_toml_str(text: &str) -> Result<Self, PolicyError> {
        let settings: MachineSettings =
            toml::from_str(text).map_err(|e| PolicyError::MalformedToml(e.to_string()))?;
        if settings.schema_version != MACHINE_SCHEMA_VERSION {
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
        // The REAL directories: checking against a stand-in let an entry
        // that is an ancestor of the actual home or state dir through. An
        // unresolvable one (no HOME) is left to the stand-in.
        let home = crate::paths::home_dir().ok();
        let state_dir = crate::paths::state_dir().ok();
        let config_dir = crate::paths::config_dir().ok();
        // With no resolvable home the floor is computed against the same
        // stand-in `check` expands `~` with, so `~/.ssh` is still refused
        // rather than the floor check being skipped.
        let floor_defaults =
            default_floor_paths(home.as_deref().unwrap_or(Path::new(STAND_IN_HOME)));
        self.sandbox.check(
            home.as_deref(),
            state_dir.as_deref(),
            config_dir.as_deref(),
            &floor_defaults,
        )?;
        Ok(())
    }
}

/// The home `[sandbox]` checks expand `~` against when the real one cannot
/// be resolved: one value, so the `writable` guard and the floor defaults
/// it compares against always agree.
const STAND_IN_HOME: &str = "/~home";

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
    /// The base revision's checks could not run to a verdict — a program
    /// the profile names was not found (exit 127), or the check was cut
    /// off before it finished (the wall clock, or a signal) — so the base
    /// has no verdict and no candidate can be compared to it. Not a
    /// baseline FAILURE, which is a check that RAN and said no: reporting
    /// a check that never finished as one sends a reader to look for a
    /// broken base that is not broken.
    BaselineUnrunnable,
    /// A declared setup command did not succeed in a worktree the run
    /// owns (the base or the task worktree), so the profile's commands
    /// never ran there.
    VerificationSetupFailed,
    WorktreeUnavailable,
    BackendUnavailable,
    /// The dispatch requested an effort the installed harness does not
    /// accept for the model. Never dropped silently (SPEC §20).
    EffortUnsupported,
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
    /// A declared acceptance criterion names a check the verification
    /// profile does not define. Refused before dispatch: a criterion
    /// nothing can settle is not a narrower contract, it is a broken one.
    AcceptanceCheckUnknown,
    /// The rung's effort is above the ceiling the authority policy (or the
    /// top of the model's admissible set) allows. Never clamped: a spend
    /// the policy did not authorize is refused, not lowered.
    EffortAboveCap,
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
            Self::EffortUnsupported => "effort_unsupported",
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
            Self::AcceptanceCheckUnknown => "acceptance_check_unknown",
            Self::EffortAboveCap => "effort_above_cap",
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

/// The policy's models narrowed by the machine's `allowed_models`
/// (`None`: any). The one narrowing both routing
/// ([`effective_authority`]) and candidate admission
/// (`route::default_tuning_bounds`) use, so what admission judges against
/// is what a run would dispatch.
pub fn allowed_models_of(
    models: &BTreeMap<Tier, ModelProfile>,
    allowed: Option<&[String]>,
) -> BTreeMap<Tier, ModelProfile> {
    let mut narrowed = models.clone();
    if let Some(allowed) = allowed {
        narrowed.retain(|_, profile| allowed.contains(&profile.id));
    }
    narrowed
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

    let models = allowed_models_of(&repo.models, machine.allowed_models.as_deref());
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

    // A declared criterion that names a check the profile does not
    // define would settle nothing, ever: refused here, before any
    // dispatch, naming both the criterion and the check (SPEC §10).
    for criterion in &contract.acceptance {
        if let Some(crate::acceptance::Evidence::Check { name }) = criterion.evidence() {
            if named_command(&profile, name).is_none() {
                blockers.push(Blocker {
                    code: BlockCode::AcceptanceCheckUnknown,
                    detail: format!(
                        "acceptance criterion `{}` names check `{name}`, which verification \
                         profile `{}` does not define",
                        criterion.id(),
                        contract.verification_profile
                    ),
                });
            }
        }
    }

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
            EffortId::parse("medium").ok()
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

    /// A machine.toml written before `[admission]` existed has no such
    /// table at all, and must still parse and mean what it meant: the
    /// new settings are optional, defaulted, and never asked for.
    #[test]
    fn a_machine_toml_without_admission_parses_with_defaults() {
        let machine = MachineSettings::from_toml_str(&machine_toml("")).expect("parses");
        assert_eq!(machine.admission.binding_lease_secs, 120);
        assert_eq!(machine.admission.dispatch_reserve_micros, MicroUsd::ZERO);
        assert_eq!(
            machine.admission.on_coordinator_unreachable,
            CoordinatorUnreachableBehavior::CarryOn
        );
        assert_eq!(machine.admission.queue_wait_secs, DEFAULT_QUEUE_WAIT_SECS);
        assert_eq!(
            machine.admission.queue_behaviour(),
            QueueBehaviour::WaitUpTo(Duration::from_secs(2))
        );
    }

    /// The single place that decides what `queue_wait_secs` means: zero
    /// is refuse-at-once, exactly today's behaviour before this field
    /// existed; anything else is a bound to wait up to. No other code
    /// path may read the raw integer and decide for itself.
    #[test]
    fn queue_behaviour_is_the_one_place_zero_means_refuse_immediately() {
        let zero = HookAdmissionSettings {
            queue_wait_secs: 0,
            ..HookAdmissionSettings::default()
        };
        assert_eq!(zero.queue_behaviour(), QueueBehaviour::RefuseImmediately);

        let five = HookAdmissionSettings {
            queue_wait_secs: 5,
            ..HookAdmissionSettings::default()
        };
        assert_eq!(
            five.queue_behaviour(),
            QueueBehaviour::WaitUpTo(Duration::from_secs(5))
        );
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

    /// `[pricing]` is machine policy, not repo policy (SPEC §11): adding
    /// or editing it in machine.toml must not move a repo's authority
    /// hash or invalidate a trust grant reviewed before it existed.
    #[test]
    fn a_pricing_block_in_machine_settings_does_not_move_the_authority_hash() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let without_pricing =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let with_pricing = MachineSettings::from_toml_str(&format!(
            "{}\n[pricing]\nversion = \"2026-09-01\"\n\n\
             [[pricing.models]]\nids = [\"claude-haiku-4-5\"]\ninput = 1000000\n\
             output = 5000000\ncache_read = 100000\ncache_write_5m = 1250000\n\
             cache_write_1h = 2000000\n",
            machine_toml(&grant_for(&repo))
        ))
        .expect("parses");
        assert!(without_pricing.pricing.is_none());
        assert!(with_pricing.pricing.is_some());
        assert_eq!(
            repo.authority_hash(),
            repo.authority_hash(),
            "a repo's own hash never reads machine.toml at all"
        );
        let a = effective_authority(&repo, &without_pricing, &contract(), &identity());
        let b = effective_authority(&repo, &with_pricing, &contract(), &identity());
        assert_eq!(
            a.trust_granted, b.trust_granted,
            "a pricing block must not affect whether the existing grant still holds"
        );
    }

    /// `[sandbox]` is machine policy like `[pricing]`: adding it must not
    /// move a repo's authority hash or invalidate a reviewed grant.
    #[test]
    fn a_sandbox_block_in_machine_settings_does_not_move_the_authority_hash() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let hash = repo.authority_hash();
        let without =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let with = MachineSettings::from_toml_str(&format!(
            "{}\n[sandbox]\nenabled = true\nwritable = [\"~/scratch\"]\n\
             network = [\"api.anthropic.com\"]\ndeny_read = [\"~/secrets\"]\n",
            machine_toml(&grant_for(&repo))
        ))
        .expect("parses");
        assert!(!without.sandbox.enabled, "disabled unless stated");
        assert!(with.sandbox.enabled);
        assert_eq!(repo.authority_hash(), hash);
        let a = effective_authority(&repo, &without, &contract(), &identity());
        let b = effective_authority(&repo, &with, &contract(), &identity());
        assert_eq!(a.trust_granted, b.trust_granted);
    }

    fn sandbox_refusal(body: &str) -> String {
        let text = machine_toml(&format!("[sandbox]\n{body}\n"));
        match MachineSettings::from_toml_str(&text) {
            Err(PolicyError::InvalidSandbox { field, .. }) => field.to_string(),
            other => panic!("expected a sandbox refusal for {body:?}, got {other:?}"),
        }
    }

    #[test]
    fn a_sandbox_block_refuses_what_it_cannot_honour() {
        assert_eq!(sandbox_refusal("writable = [\"~\"]"), "writable");
        assert_eq!(sandbox_refusal("writable = [\"/\"]"), "writable");
        assert_eq!(sandbox_refusal("writable = [\"scratch\"]"), "writable");
        assert_eq!(sandbox_refusal("writable = [\"/a/../b\"]"), "writable");
        assert_eq!(sandbox_refusal("deny_read = [\"secrets\"]"), "deny_read");
        for bad in ["localhost", "https://a.com", "a.com/x", "*.a.com", "a..com"] {
            assert_eq!(
                sandbox_refusal(&format!("network = [\"{bad}\"]")),
                "network"
            );
        }
    }

    /// `~/..` resolved to the home's parent and passed: `..` is refused in
    /// a `~/` entry as in an absolute one.
    #[test]
    fn a_home_relative_parent_escape_is_refused() {
        for entry in ["~/..", "~/../..", "~/.cache/../../x"] {
            let settings = SandboxSettings {
                writable: vec![entry.to_string()],
                ..SandboxSettings::default()
            };
            assert!(
                settings
                    .check(Some(Path::new("/Users/me")), None, None, &[])
                    .is_err(),
                "{entry} must be refused"
            );
        }
    }

    /// With no resolvable home, `~/.ssh` is still refused: the floor
    /// defaults are computed against the stand-in `~` expands with.
    #[test]
    fn a_floor_path_is_refused_even_without_a_resolvable_home() {
        let settings = SandboxSettings {
            writable: vec!["~/.ssh".to_string()],
            ..SandboxSettings::default()
        };
        let floor = default_floor_paths(Path::new(STAND_IN_HOME));
        assert!(
            settings.check(None, None, None, &floor).is_err(),
            "the floor defaults use the same stand-in `~` expands against"
        );
    }

    /// A `writable` entry containing relais's config dir (machine.toml, the
    /// trust grants) is refused, relocated or not.
    // Unix only: Unix absolute-path fixtures (see the symlink test).
    #[cfg(unix)]
    #[test]
    fn a_writable_entry_over_the_config_dir_is_refused() {
        let settings = SandboxSettings {
            writable: vec!["~/.config".to_string()],
            ..SandboxSettings::default()
        };
        let home = Path::new("/Users/me");
        assert!(settings
            .check(
                Some(home),
                None,
                Some(Path::new("/Users/me/.config/relais")),
                &[]
            )
            .is_err());
        assert!(settings
            .check(Some(home), None, Some(Path::new("/etc/relais")), &[])
            .is_ok());
    }

    // Unix only: Unix absolute-path fixtures (see the symlink test).
    #[cfg(unix)]
    #[test]
    fn a_sandbox_check_with_known_directories_refuses_their_ancestors() {
        let settings = SandboxSettings {
            writable: vec!["/Users".to_string()],
            ..SandboxSettings::default()
        };
        let home = Path::new("/Users/me");
        assert!(settings.check(Some(home), None, None, &[]).is_err());
        let state = SandboxSettings {
            writable: vec!["/var/lib".to_string()],
            ..SandboxSettings::default()
        };
        assert!(state
            .check(Some(home), Some(Path::new("/var/lib/relais")), None, &[])
            .is_err());
        assert!(state
            .check(Some(home), Some(Path::new("/srv/relais")), None, &[])
            .is_ok());
    }

    fn writable_check(entry: &str, home: &Path) -> Result<(), PolicyError> {
        let settings = SandboxSettings {
            writable: vec![entry.to_string()],
            ..SandboxSettings::default()
        };
        settings.check(Some(home), None, None, &default_floor_paths(home))
    }

    // Unix only: Unix absolute-path fixtures (see the symlink test).
    #[cfg(unix)]
    #[test]
    fn a_writable_entry_that_is_or_contains_a_floor_path_is_refused() {
        let home = Path::new("/Users/me");
        for entry in [
            "~/.ssh",
            "~/.aws",
            "~/.config",
            "~/.cargo",
            "~/.claude.json",
        ] {
            assert!(writable_check(entry, home).is_err(), "{entry}");
        }
        assert!(writable_check("~/work", home).is_ok());
    }

    // Unix only: it makes a symlink with `std::os::unix`, and the OS
    // sandbox these paths feed exists on macOS and Linux only.
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_floor_dir_is_refused_as_writable() {
        let scratch = crate::test_support::temp_dir("writable-alias");
        let home = scratch.join("home");
        std::fs::create_dir_all(home.join(".ssh")).expect("mkdir");
        let link = scratch.join("link");
        std::os::unix::fs::symlink(home.join(".ssh"), &link).expect("symlink");
        assert!(writable_check(&link.display().to_string(), &home).is_err());
        let sibling = scratch.join("plain");
        std::fs::create_dir(&sibling).expect("mkdir");
        assert!(writable_check(&sibling.display().to_string(), &home).is_ok());
    }

    /// `CommandSpec.name` is what a declared criterion's evidence names;
    /// absent, it must not appear at all in what the authority hash is
    /// taken over, so a policy written before this field existed keeps
    /// the hash its trust grant was reviewed against.
    #[test]
    fn an_absent_command_name_does_not_reach_the_authority_hash_input() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let value = serde_json::to_value(&repo).expect("a policy serializes");
        let commands = value["verification"]["profiles"]["rust-change"]["commands"]
            .as_array()
            .expect("commands array");
        assert!(
            !commands[0]
                .as_object()
                .expect("a command is an object")
                .contains_key("name"),
            "an absent command name must not appear in the hashed form: {value}"
        );

        let named = RepoPolicy::from_toml_str(&REPO_TOML.replace(
            "[[verification.profiles.rust-change.commands]]\nargv = [\"make\", \"check\"]",
            "[[verification.profiles.rust-change.commands]]\n\
             name = \"make check\"\nargv = [\"make\", \"check\"]",
        ))
        .expect("parses");
        assert_ne!(
            repo.authority_hash(),
            named.authority_hash(),
            "naming a command IS a change once it is written"
        );
    }

    /// `CommandSpec.junit` is a declared JUnit report path. Absent, it
    /// must not appear in the hashed form (the frozen-policy golden
    /// below, `authority_hash_of_a_policy_shaped_as_todays_does_not_move`,
    /// pins the hash of a policy that sets none); written, it is
    /// executable-adjacent authority and moves the hash.
    #[test]
    fn a_junit_path_reaches_the_authority_hash_only_when_written() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let value = serde_json::to_value(&repo).expect("a policy serializes");
        let commands = value["verification"]["profiles"]["rust-change"]["commands"]
            .as_array()
            .expect("commands array");
        assert!(
            !commands[0]
                .as_object()
                .expect("a command is an object")
                .contains_key("junit"),
            "an absent junit path must not appear in the hashed form: {value}"
        );

        let with_junit = RepoPolicy::from_toml_str(&REPO_TOML.replace(
            "[[verification.profiles.rust-change.commands]]\nargv = [\"make\", \"check\"]",
            "[[verification.profiles.rust-change.commands]]\n\
             argv = [\"make\", \"check\"]\njunit = \"target/junit.xml\"",
        ))
        .expect("parses");
        assert_eq!(
            with_junit.verification.profiles["rust-change"].commands[0]
                .junit
                .as_deref(),
            Some("target/junit.xml")
        );
        assert_ne!(repo.authority_hash(), with_junit.authority_hash());
    }

    /// SPEC §10: a declared criterion naming a check no profile defines
    /// would settle nothing, ever. Refused at preflight, before any
    /// dispatch, by a block code naming both the criterion and the check.
    #[test]
    fn a_declared_criterion_naming_an_unknown_check_is_refused_before_dispatch() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let mut c = contract();
        let entry =
            crate::acceptance::AcceptanceEntry::Declared(crate::acceptance::DeclaredCriterion {
                statement: "the api rejects malformed input".into(),
                id: None,
                mandatory: true,
                evidence: crate::acceptance::Evidence::Check {
                    name: "no-such-check".into(),
                },
            });
        let entry_id = entry.id();
        c.acceptance = vec![entry];
        let a = effective_authority(&repo, &machine, &c, &identity());
        let blocker = a
            .blockers
            .iter()
            .find(|b| b.code == BlockCode::AcceptanceCheckUnknown)
            .expect("a criterion naming an unknown check must be refused before dispatch");
        assert!(blocker.detail.contains("no-such-check"), "{blocker:?}");
        assert!(blocker.detail.contains(&entry_id), "{blocker:?}");
    }

    #[test]
    fn a_declared_criterion_naming_a_defined_check_is_not_blocked() {
        let named = RepoPolicy::from_toml_str(&REPO_TOML.replace(
            "[[verification.profiles.rust-change.commands]]\nargv = [\"make\", \"check\"]",
            "[[verification.profiles.rust-change.commands]]\n\
             name = \"make check\"\nargv = [\"make\", \"check\"]",
        ))
        .expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&named))).expect("parses");
        let mut c = contract();
        c.acceptance = vec![crate::acceptance::AcceptanceEntry::Declared(
            crate::acceptance::DeclaredCriterion {
                statement: "it builds".into(),
                id: None,
                mandatory: true,
                evidence: crate::acceptance::Evidence::Check {
                    name: "make check".into(),
                },
            },
        )];
        let a = effective_authority(&named, &machine, &c, &identity());
        assert!(
            !a.blockers
                .iter()
                .any(|b| b.code == BlockCode::AcceptanceCheckUnknown),
            "a check the profile defines must not be blocked: {:?}",
            a.blockers
        );
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

    #[test]
    fn the_floor_denies_subagent_spawning() {
        let floor = default_disallowed_tools();
        assert!(floor.contains(&"Agent".to_string()));
        assert!(floor.contains(&"Task".to_string()));
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

    /// A machine.toml that sets only today's three `[trials]` fields still
    /// parses, and everything it does not set is the safe default: no
    /// seed, no eligible kind, no candidate. One that sets none is OFF.
    #[test]
    fn a_machine_with_only_the_original_trial_fields_parses_with_safe_defaults() {
        let machine = MachineSettings::from_toml_str(
            "schema_version = 1\n[trials]\nenabled = true\nmax_daily_trials = 3\n\
             max_trial_cost_micros = 500\n",
        )
        .expect("the original three fields still parse");
        let trials = machine.trials;
        assert!(trials.enabled);
        assert_eq!(trials.max_daily_trials, 3);
        assert_eq!(trials.max_trial_cost_micros, 500);
        assert_eq!(trials.seed, None);
        assert!(trials.eligible_kinds.is_empty());
        assert!(trials.candidates.is_empty());

        let bare = MachineSettings::from_toml_str("schema_version = 1\n").expect("parses");
        assert_eq!(bare.trials, TrialEnvelope::default());
        assert!(!bare.trials.enabled);
        assert_eq!(bare.trials.max_daily_trials, 0);
        assert_eq!(bare.trials.max_trial_cost_micros, 0);
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
    fn validate_accepts_schema_version_1_and_2_but_not_others() {
        let v1 = RepoPolicy::from_toml_str(REPO_TOML).expect("schema_version 1 still parses");
        assert_eq!(v1.schema_version, 1);

        let v2 = RepoPolicy::from_toml_str(&REPO_TOML.replacen(
            "schema_version = 1",
            "schema_version = 2",
            1,
        ))
        .expect("schema_version 2 parses");
        assert_eq!(v2.schema_version, 2);

        let unsupported = REPO_TOML.replacen("schema_version = 1", "schema_version = 3", 1);
        assert_eq!(
            RepoPolicy::from_toml_str(&unsupported),
            Err(PolicyError::UnsupportedSchemaVersion(3))
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

    /// `policy_hash` (the authority hash) moves whenever anything in the
    /// policy moves, including a recipe promotion that touches no check —
    /// so it cannot answer "was this judged by the same checks".
    /// `VerificationProfile::hash` answers exactly that: it is unmoved by
    /// a change entirely outside verification.
    #[test]
    fn verification_profile_hash_is_unmoved_by_an_unrelated_policy_change() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let with_recipe = RepoPolicy::from_toml_str(&format!(
            "{REPO_TOML}\n[[recipes]]\nname = \"docs\"\nscope_within = [\"docs/**\"]\ntier = \"research\"\n"
        ))
        .expect("parses");
        assert_ne!(
            repo.authority_hash(),
            with_recipe.authority_hash(),
            "adding a recipe is an authority change"
        );
        assert_eq!(
            repo.verification.profiles["rust-change"].hash(),
            with_recipe.verification.profiles["rust-change"].hash(),
            "the verification profile itself did not change"
        );
    }

    /// A FROZEN policy fixture, deliberately not `include_str!` of this
    /// repository's live `relais.toml`.
    ///
    /// The hashes below were measured against this exact text with the
    /// PRE-CHANGE binary. Reading the live file instead would make the
    /// policy content and the pinned hash two copies of one fact with
    /// only one of them frozen: any ordinary edit to `relais.toml` — a
    /// raised wall clock, a new risk rule — would fail this test, and the
    /// obvious repair would be to update the literal, which is precisely
    /// what the pin exists to forbid. The fixture is minimal because what
    /// is under test is how a RecipeSpec serializes, not what this
    /// repository happens to configure.
    ///
    /// Change this text only together with a hash re-measured on a build
    /// that does NOT contain the change being tested.
    const FROZEN_V1_POLICY: &str = r#"schema_version = 1

[models.research]
id = "haiku"

[models.implementation]
id = "sonnet"

[models.escalation]
id = "opus"

[execution]
max_attempts = 3
max_repairs_before_escalation = 1
max_wall_seconds = 600
allow_nested_agents = false
max_agent_depth = 1
max_agents_total = 1

[integrations]
aval = "optional"
amont = "optional"

[[verification.profiles.default.commands]]
argv = ["true"]
timeout_seconds = 60
"#;

    #[test]
    fn authority_hash_of_a_policy_shaped_as_todays_does_not_move() {
        let repo = RepoPolicy::from_toml_str(FROZEN_V1_POLICY).expect("the frozen fixture parses");
        assert_eq!(
            repo.authority_hash(),
            "62accc6aa657334e14ec991ef466b22c3db7917bb3bb8654d85d111ecf3acf33",
            "the authority hash of a v1-shaped policy must not move; a moved hash is a DEAD \
             TRUST GRANT on every repository whose policy has this shape"
        );

        // The case the skip_serializing_if attributes exist for: a recipe
        // that sets only the fields a v1 policy could set must serialize
        // as it did before RecipeSpec grew revision/enabled/models/
        // execution/context.
        let with_recipe = RepoPolicy::from_toml_str(&format!(
            "{FROZEN_V1_POLICY}\n[[recipes]]\nname = \"docs-touchup\"\nkind = \"change\"\n\
             scope_within = [\"docs/**\"]\ntier = \"research\"\n"
        ))
        .expect("the frozen fixture plus one recipe parses");
        assert_eq!(
            with_recipe.authority_hash(),
            "ae3a0efe11b4cecfcdc95e527e05bba923967d62556767a91750c17cb12e75cb",
            "a recipe setting only name/kind/scope_within/tier must hash exactly as it did \
             before RecipeSpec grew its new fields"
        );
    }

    /// Effort became data (`EffortId`) and must serialize as the plain
    /// string the closed enum did: a moved hash is a dead trust grant.
    #[test]
    fn the_authority_hash_survives_effort_becoming_data() {
        let own = RepoPolicy::from_toml_str(include_str!("../../../../relais.toml"))
            .expect("this repository's own relais.toml parses");
        assert_eq!(
            own.authority_hash(),
            "afff01a0df657a98568c538fd07d0281e15b41d64073d277b4bf6e49f1f8880c",
            "the hash of this repository's own policy must not move"
        );
        let tiered = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        assert_eq!(
            tiered.authority_hash(),
            "d909b85bb81bf97cd2a2415b758cc9cc9d5d74eac087ef3746755ac1d27c74c7",
            "a policy with per-tier efforts must hash as it did with the enum"
        );
    }

    /// `minimum_effort` and `max_effort` are new keys. Absent, they do not
    /// appear in the serialized form, which is why the pinned hashes above
    /// (this repository's `relais.toml`, and REPO_TOML: a `[[risk]]` rule
    /// and per-tier efforts) did not move; present, they are authority and
    /// move the hash like any other.
    #[test]
    fn the_new_effort_keys_are_hashed_only_when_present() {
        let plain = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let serialized = serde_json::to_string(&plain).expect("serializes");
        assert!(
            !serialized.contains("minimum_effort") && !serialized.contains("max_effort"),
            "absent keys are omitted: {serialized}"
        );
        let floor = REPO_TOML.replace(
            "review = \"required\"\n",
            "review = \"required\"\nminimum_effort = \"high\"\n",
        );
        let ceiling = REPO_TOML.replace(
            "id = \"sonnet\"\neffort = \"medium\"\n",
            "id = \"sonnet\"\neffort = \"medium\"\nmax_effort = \"high\"\n",
        );
        for changed in [floor, ceiling] {
            assert_ne!(changed, REPO_TOML, "the replacement landed");
            assert_ne!(
                RepoPolicy::from_toml_str(&changed)
                    .expect("parses")
                    .authority_hash(),
                plain.authority_hash()
            );
        }
    }

    #[test]
    fn an_effort_identifier_is_validated_when_it_is_read() {
        for good in ["low", "xhigh", "ultra", "x", "a_b-c9"] {
            assert_eq!(EffortId::parse(good).expect(good).as_str(), good);
        }
        let too_long = "a".repeat(33);
        for bad in ["", "High", "1up", "-x", "a b", "é", too_long.as_str()] {
            assert_eq!(
                EffortId::parse(bad),
                Err(EffortIdError { value: bad.into() }),
                "`{bad}` is not an identifier"
            );
        }
        let err = RepoPolicy::from_toml_str(
            &REPO_TOML.replace("effort = \"medium\"", "effort = \"Ultra!\""),
        )
        .expect_err("a malformed effort is refused");
        assert!(
            err.to_string().contains("Ultra!"),
            "the error names the bad value: {err}"
        );
        assert_eq!(
            serde_json::to_string(&EffortId::parse("xhigh").expect("valid")).expect("serializes"),
            "\"xhigh\"",
            "it serializes as the plain string"
        );
    }

    #[test]
    fn machine_efforts_are_optional_and_strictly_shaped() {
        let bare = MachineSettings::from_toml_str(&machine_toml("")).expect("parses");
        assert_eq!(bare.efforts, EffortSettings::default());
        assert_eq!(bare.routing.max_effort.as_str(), "high");
        let set = MachineSettings::from_toml_str(&machine_toml(
            "[routing]\nmax_effort = \"max\"\n\n[efforts]\norder = [\"low\", \"high\"]\n\n\
             [[efforts.models]]\nids = [\"haiku\"]\nsupported = []\n",
        ))
        .expect("parses");
        assert_eq!(set.routing.max_effort.as_str(), "max");
        assert_eq!(set.efforts.order.as_ref().map(Vec::len), Some(2));
        let entry = set.efforts.entry_for("HAIKU").expect("an entry");
        assert_eq!(entry.supported, Some(vec![]));
        assert!(set.efforts.entry_for("sonnet").is_none());
        assert!(
            MachineSettings::from_toml_str(&machine_toml("[efforts]\nlevels = []\n")).is_err(),
            "an unknown key is still refused"
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
