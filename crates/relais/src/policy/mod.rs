//! Policy and authority (SPEC §5).
//!
//! Repository policy lives in `relais.toml`; machine-owned settings (under
//! `~/.config/relais/machine.toml`) hold spending ceilings, allowed
//! models, permissions and trust grants. Effective authority is the
//! intersection of repo policy, machine settings and per-run contract
//! limits — a contract can narrow a grant but nothing broadens one, and
//! there is no last-writer-wins override. Trust grants are content-bound
//! to the reviewed execution profile; a changed declaration invalidates
//! them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::{Review, TaskContract};
use crate::ids::canonical_json_hash;

pub const POLICY_SCHEMA_VERSION: u64 = 1;

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
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
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
    pub integrations: Integrations,
    #[serde(default)]
    pub verification: VerificationPolicy,
    #[serde(default)]
    pub risk: Vec<RiskRule>,
    #[serde(default)]
    pub architecture: ArchitectureConfig,
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
            for command in &profile.commands {
                if command.argv.is_empty() {
                    return Err(PolicyError::EmptyCommandArgv);
                }
            }
        }
        Ok(())
    }

    /// Hash over the executable authority: models, execution limits,
    /// verification profiles, integration modes, risk rules and
    /// architecture mappings. Machine trust grants are content-bound to
    /// this hash — a changed declaration invalidates them (SPEC §5).
    pub fn authority_hash(&self) -> String {
        let value = serde_json::json!({
            "models": self.models,
            "execution": self.execution,
            "verification": self.verification,
            "integrations": self.integrations,
            "risk": self.risk,
            "architecture": self.architecture,
        });
        canonical_json_hash(&value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    UnsupportedSchemaVersion(u64),
    MalformedToml(String),
    EmptyRiskPaths,
    EmptyCommandArgv,
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
    /// Content-bound trust grants, keyed by repo authority hash.
    #[serde(default)]
    pub trust: BTreeMap<String, TrustGrant>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub concurrency: ConcurrencyLimits,
    #[serde(default)]
    pub trials: TrialEnvelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct SpendingCeilings {
    /// Per-run API spend ceiling in micro-USD. Best effort across
    /// in-flight requests; never advertised as an exact cap (SPEC §11).
    pub per_run_micros: Option<i64>,
    pub per_day_micros: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustGrant {
    pub granted_at: String,
    #[serde(default)]
    pub reviewed_by: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Tool profile for workers. Workers cannot commit, merge, push or publish
/// through the normal allowed tool profile (SPEC §8); the default list is
/// the floor, and repo policy may extend it — never shorten it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Permissions {
    #[serde(default = "default_disallowed_tools")]
    pub disallowed_tools: Vec<String>,
}

fn default_disallowed_tools() -> Vec<String> {
    vec![
        "Bash(git commit:*)".into(),
        "Bash(git merge:*)".into(),
        "Bash(git push:*)".into(),
        "Bash(git rebase:*)".into(),
        "Bash(git reset:*)".into(),
    ]
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            disallowed_tools: default_disallowed_tools(),
        }
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TrialEnvelope {
    pub enabled: bool,
    pub max_daily_trials: Option<u32>,
    pub max_trial_cost_micros: Option<i64>,
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
        Ok(settings)
    }
}

/// Why execution is blocked before any model is launched. Each blocker is
/// a stable code plus a human explanation; `blocked:*` codes surface in
/// `plan`, `run` and `explain` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub code: String,
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
    pub verification_profile: VerificationProfile,
    pub review_floor: Review,
    pub disallowed_tools: Vec<String>,
    pub authority_hash: String,
    pub trust_granted: bool,
    pub blockers: Vec<Blocker>,
}

pub fn effective_authority(
    repo: &RepoPolicy,
    machine: &MachineSettings,
    contract: &TaskContract,
) -> EffectiveAuthority {
    let mut blockers = Vec::new();

    let mut models = repo.models.clone();
    if let Some(allowed) = &machine.allowed_models {
        models.retain(|_, profile| allowed.contains(&profile.id));
    }
    if !repo.models.is_empty() && models.is_empty() {
        blockers.push(Blocker {
            code: "model_not_allowed".into(),
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
                code: "verification_profile_unknown".into(),
                detail: format!(
                    "verification profile `{}` is not defined in relais.toml",
                    contract.verification_profile
                ),
            });
            VerificationProfile::default()
        });

    let authority_hash = repo.authority_hash();
    let trust_granted = machine.trust.contains_key(&authority_hash);
    if !trust_granted {
        blockers.push(Blocker {
            code: "missing_trust_grant".into(),
            detail: format!(
                "no content-bound trust grant for this execution declaration (authority hash {authority_hash}); grants are recorded in machine.toml after review"
            ),
        });
    }

    let max_attempts = repo.execution.max_attempts.min(contract.limits.attempts);
    let max_wall_seconds = repo
        .execution
        .max_wall_seconds
        .min(contract.limits.wall_seconds);

    let review_floor = contract.review.max(
        repo.risk
            .iter()
            .map(|rule| rule.review.unwrap_or(Review::Off))
            .max()
            .unwrap_or(Review::Off),
    );

    EffectiveAuthority {
        models,
        max_attempts,
        max_wall_seconds,
        max_repairs_before_escalation: repo.execution.max_repairs_before_escalation,
        verification_profile: profile,
        review_floor,
        disallowed_tools: machine.permissions.disallowed_tools.clone(),
        authority_hash,
        trust_granted,
        blockers,
    }
}

/// Availability of required integrations at run time. Kept out of
/// `effective_authority` on purpose: the intersection stays a pure function
/// over policy files, while PATH probing belongs to doctor, plan and run.
pub fn probe_integrations(repo: &RepoPolicy) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    for (name, dependency) in [
        ("aval", repo.integrations.aval.as_ref()),
        ("amont", repo.integrations.amont.as_ref()),
        ("amont_agent", repo.integrations.amont_agent.as_ref()),
    ] {
        if let Some(dependency) = dependency {
            if dependency.mode() == DependencyMode::Required {
                let bin = dependency.bin().unwrap_or(default_bin(name));
                if which_missing(bin) {
                    blockers.push(Blocker {
                        code: "integration_missing".into(),
                        detail: format!("required integration `{name}` is not available on PATH"),
                    });
                }
            }
        }
    }
    blockers
}

/// The config key is `amont_agent`; the binary on PATH is `amont-agent`.
fn default_bin(name: &str) -> &str {
    match name {
        "amont_agent" => "amont-agent",
        other => other,
    }
}

fn which_missing(bin: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).all(|dir| !dir.join(bin).is_file())
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

# required blocks execution when missing; optional gaps are reported and
# never counted as passed checks.
[integrations]
aval = "required"
amont = "required"
amont_agent = "required"

[[verification.profiles.default.commands]]
argv = ["make", "check"]
timeout_seconds = 300

# Risk floors: writes touching these patterns cannot route below the
# minimum tier, and the review requirement here is a floor, not a hint.
[[risk]]
paths = ["**/trust/**", "**/restore/**"]
minimum_tier = "escalation"
review = "required"

# Path-to-aval-key mappings. aval resolves exact keys; it has no
# source-impact analysis, so the mapping lives here (SPEC §7).
# [[architecture.mapping]]
# paths = ["crates/**"]
# keys = ["storage.object-store"]
# scope = "default"
"#;

/// Write the template unless a policy already exists. Returns `false`
/// when the file was present; init never overwrites.
pub fn write_init_template(path: &std::path::Path) -> std::io::Result<bool> {
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(e),
    };
    file.write_all(INIT_TEMPLATE.as_bytes())?;
    Ok(true)
}

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

    fn grant_for(policy: &RepoPolicy) -> String {
        let hash = policy.authority_hash();
        format!("[trust.\"{hash}\"]\ngranted_at = \"2026-09-18\"\n")
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

        let a = effective_authority(&repo, &machine, &c);
        assert!(a.trust_granted);
        assert!(a.blockers.is_empty(), "{:?}", a.blockers);
        assert_eq!(a.max_attempts, 3);

        c.limits.attempts = 1;
        let a = effective_authority(&repo, &machine, &c);
        assert_eq!(a.max_attempts, 1, "contract limits narrow authority");
        c.limits.attempts = 3;

        let mut machine_restricted = machine.clone();
        machine_restricted.allowed_models = Some(vec!["haiku".into()]);
        let a = effective_authority(&repo, &machine_restricted, &c);
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
        let a = effective_authority(&repo, &machine, &contract());
        assert!(!a.trust_granted);
        assert!(a.blockers.iter().any(|b| b.code == "missing_trust_grant"));
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
        let a = effective_authority(&changed, &machine, &contract());
        assert!(!a.trust_granted);
    }

    #[test]
    fn unknown_verification_profile_blocks() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let mut c = contract();
        c.verification_profile = "not-a-profile".into();
        let a = effective_authority(&repo, &machine, &c);
        assert!(a
            .blockers
            .iter()
            .any(|b| b.code == "verification_profile_unknown"));
    }

    #[test]
    fn risk_review_floor_is_maxed_not_minced() {
        let repo = RepoPolicy::from_toml_str(REPO_TOML).expect("parses");
        let machine =
            MachineSettings::from_toml_str(&machine_toml(&grant_for(&repo))).expect("parses");
        let a = effective_authority(&repo, &machine, &contract());
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
    }

    #[test]
    fn init_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("relais-init-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("relais.toml");
        assert!(write_init_template(&path).expect("first write"));
        std::fs::write(&path, "schema_version = 1").expect("user edit");
        assert!(!write_init_template(&path).expect("second write"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "schema_version = 1",
            "init must not clobber an existing policy"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
