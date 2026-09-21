//! Task contracts (SPEC §4): validated, frozen, hashed.
//!
//! A contract is checked against `schema_version` 1 and hashed before the
//! first worker; objective, scope, acceptance criteria and budget are
//! immutable afterwards. Changing any of them creates a new revision that
//! requires renewed verification. Unknown fields are rejected to catch
//! misspelled controls. `kind` is `inspect` (evidence criteria, no patch)
//! or `change` (bounded write scope required). `base_ref` resolves once to
//! a commit SHA. Empty architecture keys mean "no explicit mapping
//! supplied", not "architecture does not apply".

use serde::{Deserialize, Serialize};

pub mod scope;

use crate::ids::canonical_json_hash;

pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Change,
    Inspect,
}

/// Whether a semantic reviewer must look at candidates. Risk rules can
/// raise this to `Required` but nothing can lower a floor set by policy
/// (SPEC §4: worker-supplied hints may increase caution, never lower it).
/// Ordered so `max` picks the most caution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Review {
    /// No review; the least cautious value.
    Off,
    /// Review if the route's risk calls for it.
    #[default]
    Optional,
    /// A separate reviewer must pass; the most cautious value, and a
    /// floor nothing can lower.
    Required,
}

/// What is wrong with one declared write-scope pattern. A scope is
/// untrusted input — it arrives in a contract file, or from a planner
/// model under `"decomposition": "propose"` — so every pattern is
/// validated where the contract is parsed, not where a diff is checked
/// after a worker has already run and been paid for (P5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeProblem {
    /// Not a glob `globset` can compile; the detail is its message.
    NotAGlob(String),
    /// Absolute. A write scope names paths inside the repository, and an
    /// absolute pattern would be judged against repository-relative diff
    /// paths — matching nothing, silently.
    Absolute,
    /// Carries a `..` segment, which names something outside whatever it
    /// appears to bound.
    LeavesTheScope,
    /// Empty or whitespace only.
    Empty,
}

impl std::fmt::Display for ScopeProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAGlob(detail) => write!(f, "is not a valid glob: {detail}"),
            Self::Absolute => write!(
                f,
                "is absolute; a write scope names paths relative to the repository root"
            ),
            Self::LeavesTheScope => write!(f, "has a `..` segment, which leaves what it bounds"),
            Self::Empty => write!(f, "is empty"),
        }
    }
}

/// A validated, compiled write scope: the declared patterns and the
/// matcher built from them. Constructing one is the only way to get a
/// `Task::Change`, so a scope that reaches the runner has already been
/// judged — a bad glob is a contract error naming the pattern, not a
/// `WorkspaceError::Git` after the attempt (P5).
#[derive(Debug, Clone)]
pub struct WriteScope {
    patterns: Vec<String>,
    matcher: globset::GlobSet,
}

/// Two scopes are the same when they declare the same patterns; the
/// matcher is derived from them.
impl PartialEq for WriteScope {
    fn eq(&self, other: &Self) -> bool {
        self.patterns == other.patterns
    }
}

impl Eq for WriteScope {}

impl WriteScope {
    /// Validate and compile the declared patterns. Every pattern is
    /// checked, and the first problem names the pattern it is about.
    pub fn compile(patterns: Vec<String>) -> Result<Self, ContractError> {
        let mut builder = globset::GlobSetBuilder::new();
        for pattern in &patterns {
            let problem = Self::problem_with(pattern);
            if let Some(problem) = problem {
                return Err(ContractError::BadWriteScopePattern {
                    pattern: pattern.clone(),
                    problem,
                });
            }
            match globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
            {
                Ok(glob) => {
                    builder.add(glob);
                }
                Err(e) => {
                    return Err(ContractError::BadWriteScopePattern {
                        pattern: pattern.clone(),
                        problem: ScopeProblem::NotAGlob(e.to_string()),
                    })
                }
            }
        }
        let matcher = builder
            .build()
            .map_err(|e| ContractError::BadWriteScopePattern {
                pattern: patterns.join(", "),
                problem: ScopeProblem::NotAGlob(e.to_string()),
            })?;
        Ok(Self { patterns, matcher })
    }

    fn problem_with(pattern: &str) -> Option<ScopeProblem> {
        if pattern.trim().is_empty() {
            return Some(ScopeProblem::Empty);
        }
        if pattern.starts_with('/') || std::path::Path::new(pattern).is_absolute() {
            return Some(ScopeProblem::Absolute);
        }
        if pattern.split(['/', '\\']).any(|segment| segment == "..") {
            return Some(ScopeProblem::LeavesTheScope);
        }
        None
    }

    /// The patterns as the contract declared them, in order. This is the
    /// text a prompt quotes and a report prints.
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    /// Is this repository-relative path inside the declared scope?
    pub fn is_match(&self, path: &str) -> bool {
        self.matcher.is_match(path)
    }
}

/// What a contract asks for, and what that implies about a write scope
/// (SPEC §4). A `change` carries a bounded scope; an `inspect` produces
/// evidence and no patch — and cannot be given a scope at all, because
/// there is no constructor that would take one (P11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Task {
    Change { write_scope: WriteScope },
    Inspect,
}

impl Task {
    /// A change bounded by these patterns. Refuses an empty scope and a
    /// pattern that is not a relative, compilable glob.
    pub fn change(patterns: Vec<String>) -> Result<Self, ContractError> {
        if patterns.is_empty() {
            return Err(ContractError::MissingWriteScope);
        }
        Ok(Task::Change {
            write_scope: WriteScope::compile(patterns)?,
        })
    }

    pub fn kind(&self) -> Kind {
        match self {
            Task::Change { .. } => Kind::Change,
            Task::Inspect => Kind::Inspect,
        }
    }

    pub fn write_scope(&self) -> Option<&WriteScope> {
        match self {
            Task::Change { write_scope } => Some(write_scope),
            Task::Inspect => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContract {
    /// SPEC §4 calls the required field "version"; the example spells it
    /// `schema_version`. Both are accepted, one canonical name is written.
    pub schema_version: u64,
    /// The kind and, for a change, its bounded write scope. One field,
    /// because an inspect with a scope and a change without one are both
    /// unrepresentable.
    pub task: Task,
    pub objective: String,
    pub base_ref: String,
    pub read_hints: Vec<String>,
    /// Acceptance criteria; for `inspect` these are evidence criteria.
    pub acceptance: Vec<String>,
    pub verification_profile: String,
    pub architecture: Architecture,
    pub risk_hints: Vec<String>,
    pub limits: Limits,
    pub review: Review,
    /// Bounded decomposition into work packages (SPEC §19): an explicit
    /// plan, or `"propose"` to let a bounded planner suggest one that
    /// deterministic validation then accepts or rejects. Absent for the
    /// ordinary single-worker run; omitted from the canonical form when
    /// absent so existing contract hashes are unchanged.
    pub decomposition: Option<Decomposition>,
}

/// The contract exactly as it is written and stored: flat `kind` and
/// `write_scope` fields, unknown fields refused (SPEC §4). It exists so
/// the in-memory model can be an enum without changing one byte of the
/// wire format — every stored contract hash stays what it was.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractWire {
    #[serde(alias = "version")]
    schema_version: u64,
    kind: Kind,
    objective: String,
    base_ref: String,
    #[serde(default)]
    write_scope: Option<Vec<String>>,
    #[serde(default)]
    read_hints: Vec<String>,
    acceptance: Vec<String>,
    verification_profile: String,
    #[serde(default)]
    architecture: Architecture,
    #[serde(default)]
    risk_hints: Vec<String>,
    #[serde(default)]
    limits: Limits,
    #[serde(default)]
    review: Review,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decomposition: Option<Decomposition>,
}

impl From<&TaskContract> for ContractWire {
    fn from(contract: &TaskContract) -> Self {
        Self {
            schema_version: contract.schema_version,
            kind: contract.task.kind(),
            objective: contract.objective.clone(),
            base_ref: contract.base_ref.clone(),
            // `null` for an inspect, exactly as it has always been
            // written, so the canonical form and its hash are unchanged.
            write_scope: contract
                .task
                .write_scope()
                .map(|scope| scope.patterns().to_vec()),
            read_hints: contract.read_hints.clone(),
            acceptance: contract.acceptance.clone(),
            verification_profile: contract.verification_profile.clone(),
            architecture: contract.architecture.clone(),
            risk_hints: contract.risk_hints.clone(),
            limits: contract.limits.clone(),
            review: contract.review,
            decomposition: contract.decomposition.clone(),
        }
    }
}

impl TryFrom<ContractWire> for TaskContract {
    type Error = ContractError;

    fn try_from(wire: ContractWire) -> Result<Self, ContractError> {
        let task = match wire.kind {
            Kind::Change => Task::change(wire.write_scope.unwrap_or_default())?,
            Kind::Inspect => {
                if wire.write_scope.is_some_and(|scope| !scope.is_empty()) {
                    return Err(ContractError::WriteScopeOnInspect);
                }
                Task::Inspect
            }
        };
        let contract = TaskContract {
            schema_version: wire.schema_version,
            task,
            objective: wire.objective,
            base_ref: wire.base_ref,
            read_hints: wire.read_hints,
            acceptance: wire.acceptance,
            verification_profile: wire.verification_profile,
            architecture: wire.architecture,
            risk_hints: wire.risk_hints,
            limits: wire.limits,
            review: wire.review,
            decomposition: wire.decomposition,
        };
        contract.validate()?;
        Ok(contract)
    }
}

impl Serialize for TaskContract {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ContractWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TaskContract {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ContractWire::deserialize(deserializer)?;
        TaskContract::try_from(wire).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Decomposition {
    /// `"propose"`: a planner model proposes a `WorkPlan`; planning
    /// overhead counts against the run.
    Mode(DecompositionMode),
    Plan(WorkPlan),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecompositionMode {
    Propose,
}

/// A dependency graph of work packages with per-package acceptance,
/// integration acceptance and aggregate limits (SPEC §19). Shape is
/// validated here; coverage against the contract scope and the run's
/// authority is checked by the scheduler with the effective authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPlan {
    pub packages: Vec<WorkPackage>,
    /// Acceptance for the assembled candidate; independent receipts do
    /// not constitute final acceptance.
    pub integration_acceptance: Vec<String>,
    #[serde(default)]
    pub limits: PlanLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkPackage {
    pub id: String,
    pub objective: String,
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub acceptance: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanLimits {
    #[serde(default = "default_max_packages")]
    pub max_packages: u32,
    #[serde(default = "default_attempts_per_package")]
    pub attempts_per_package: u32,
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_packages: default_max_packages(),
            attempts_per_package: default_attempts_per_package(),
        }
    }
}

fn default_max_packages() -> u32 {
    4
}

fn default_attempts_per_package() -> u32 {
    2
}

/// `[A-Za-z0-9][A-Za-z0-9_-]{0,63}`. A package id is not a label: the
/// scheduler uses it as a filesystem path component for the package's
/// worktree and artifacts, and with `"decomposition": "propose"` the ids
/// are model output. `../..`, an absolute path, a NUL or a name that is
/// merely long must never reach a `join`.
fn is_valid_package_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    if id.len() > 64 {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A package objective is one line of at most 2000 characters. Under
/// `"decomposition": "propose"` it is model output, and the scheduler
/// splices it into a child contract's objective, which the worker prompt
/// then quotes: a newline inside it would add lines to that quoted block
/// (SPEC §16 — free text is untrusted data). Rejecting the plan is the
/// bound; the prompt's fence is the second line of defence.
pub const MAX_PACKAGE_OBJECTIVE_CHARS: usize = 2000;

fn is_valid_package_objective(objective: &str) -> bool {
    objective.chars().count() <= MAX_PACKAGE_OBJECTIVE_CHARS && !objective.contains(['\n', '\r'])
}

impl WorkPlan {
    /// Shape validation: ids unique and non-empty, every package carries
    /// an objective, scope and acceptance, dependencies resolve, and the
    /// graph is acyclic. Returns the packages in a topological order
    /// with each package's wave (0 = no dependencies).
    pub fn validate(&self) -> Result<Vec<(usize, u32)>, ContractError> {
        if self.packages.len() < 2 {
            return Err(ContractError::PlanTooSmall(self.packages.len()));
        }
        if self.packages.len() as u32 > self.limits.max_packages {
            return Err(ContractError::PlanTooLarge(
                self.packages.len(),
                self.limits.max_packages,
            ));
        }
        if self.limits.attempts_per_package < 1 {
            return Err(ContractError::BadAttempts(self.limits.attempts_per_package));
        }
        if self.integration_acceptance.is_empty() {
            return Err(ContractError::PlanMissingIntegrationAcceptance);
        }
        let mut ids = std::collections::BTreeSet::new();
        for package in &self.packages {
            if package.id.trim().is_empty() || !ids.insert(package.id.as_str()) {
                return Err(ContractError::PlanDuplicatePackage(package.id.clone()));
            }
            if !is_valid_package_id(&package.id) {
                return Err(ContractError::PlanBadPackageId(package.id.clone()));
            }
            if package.objective.trim().is_empty() {
                return Err(ContractError::PlanPackageIncomplete(
                    package.id.clone(),
                    "objective",
                ));
            }
            if !is_valid_package_objective(&package.objective) {
                return Err(ContractError::PlanBadPackageObjective(package.id.clone()));
            }
            if package.write_scope.is_empty() {
                return Err(ContractError::PlanPackageIncomplete(
                    package.id.clone(),
                    "write_scope",
                ));
            }
            if package.acceptance.is_empty() {
                return Err(ContractError::PlanPackageIncomplete(
                    package.id.clone(),
                    "acceptance",
                ));
            }
        }
        for package in &self.packages {
            for dependency in &package.depends_on {
                if dependency == &package.id || !ids.contains(dependency.as_str()) {
                    return Err(ContractError::PlanBadDependency(
                        package.id.clone(),
                        dependency.clone(),
                    ));
                }
            }
        }
        // Kahn's algorithm, waves = longest path from a root.
        let index_of = |id: &str| {
            self.packages
                .iter()
                .position(|p| p.id == id)
                .expect("known")
        };
        let mut indegree: Vec<usize> = self.packages.iter().map(|p| p.depends_on.len()).collect();
        let mut wave: Vec<u32> = vec![0; self.packages.len()];
        let mut order = Vec::new();
        let mut ready: Vec<usize> = (0..self.packages.len())
            .filter(|&i| indegree[i] == 0)
            .collect();
        while let Some(current) = ready.first().copied() {
            ready.remove(0);
            order.push((current, wave[current]));
            for (i, package) in self.packages.iter().enumerate() {
                if package.depends_on.iter().any(|d| index_of(d) == current) {
                    indegree[i] -= 1;
                    wave[i] = wave[i].max(wave[current] + 1);
                    if indegree[i] == 0 {
                        ready.push(i);
                    }
                }
            }
        }
        if order.len() != self.packages.len() {
            return Err(ContractError::PlanCycle);
        }
        Ok(order)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Architecture {
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    #[serde(default = "default_wall_seconds")]
    pub wall_seconds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            attempts: default_attempts(),
            wall_seconds: default_wall_seconds(),
        }
    }
}

fn default_attempts() -> u32 {
    3
}

fn default_wall_seconds() -> u64 {
    1200
}

/// Why a contract is rejected. Errors are hard validation failures at
/// parse time; preflight-observable problems are separate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    UnsupportedSchemaVersion(u64),
    UnknownField(String),
    EmptyObjective,
    EmptyAcceptance,
    MissingWriteScope,
    WriteScopeOnInspect,
    /// One declared write-scope pattern cannot be used as a bound.
    BadWriteScopePattern {
        pattern: String,
        problem: ScopeProblem,
    },
    DuplicateAcceptanceCriterion(String),
    BadAttempts(u32),
    BadWallSeconds(u64),
    EmptyBaseRef,
    EmptyVerificationProfile,
    MalformedJson(String),
    DecompositionOnInspect,
    PlanTooSmall(usize),
    PlanTooLarge(usize, u32),
    PlanMissingIntegrationAcceptance,
    PlanDuplicatePackage(String),
    PlanBadPackageId(String),
    PlanBadPackageObjective(String),
    PlanPackageIncomplete(String, &'static str),
    PlanBadDependency(String, String),
    PlanCycle,
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion(v) => {
                write!(
                    f,
                    "unsupported schema_version {v} (this relais understands {SCHEMA_VERSION})"
                )
            }
            Self::UnknownField(name) => write!(
                f,
                "unknown field `{name}`: contracts reject misspelled controls"
            ),
            Self::EmptyObjective => write!(f, "objective is empty"),
            Self::EmptyAcceptance => write!(f, "acceptance needs at least one criterion"),
            Self::MissingWriteScope => write!(f, "kind=change requires a non-empty write_scope"),
            Self::WriteScopeOnInspect => write!(f, "kind=inspect cannot declare a write_scope"),
            Self::BadWriteScopePattern { pattern, problem } => {
                write!(f, "write_scope pattern `{pattern}` {problem}")
            }
            Self::DuplicateAcceptanceCriterion(c) => {
                write!(f, "duplicate acceptance criterion: {c}")
            }
            Self::BadAttempts(n) => write!(f, "limits.attempts must be >= 1, got {n}"),
            Self::BadWallSeconds(n) => write!(f, "limits.wall_seconds must be >= 1, got {n}"),
            Self::EmptyBaseRef => write!(f, "base_ref is empty"),
            Self::EmptyVerificationProfile => write!(f, "verification_profile is empty"),
            Self::MalformedJson(detail) => write!(f, "contract is not valid JSON: {detail}"),
            Self::DecompositionOnInspect => {
                write!(f, "kind=inspect cannot be decomposed into work packages")
            }
            Self::PlanTooSmall(n) => write!(
                f,
                "a work plan needs at least two packages, got {n}; simple tasks stay single-worker"
            ),
            Self::PlanTooLarge(n, max) => {
                write!(f, "work plan has {n} packages, more than its limit {max}")
            }
            Self::PlanMissingIntegrationAcceptance => write!(
                f,
                "work plan needs integration_acceptance: independent receipts are not final acceptance"
            ),
            Self::PlanDuplicatePackage(id) => write!(f, "package id `{id}` is empty or duplicated"),
            Self::PlanBadPackageId(id) => write!(
                f,
                "package id `{id}` is not [A-Za-z0-9][A-Za-z0-9_-]{{0,63}}: ids name directories on disk"
            ),
            Self::PlanBadPackageObjective(id) => write!(
                f,
                "package `{id}` objective must be a single line of at most \
                 {MAX_PACKAGE_OBJECTIVE_CHARS} characters: it is spliced into a child contract \
                 and quoted in a worker prompt"
            ),
            Self::PlanPackageIncomplete(id, field) => {
                write!(f, "package `{id}` has no {field}")
            }
            Self::PlanBadDependency(id, dep) => write!(
                f,
                "package `{id}` depends on `{dep}`, which is itself or not a package"
            ),
            Self::PlanCycle => write!(f, "work plan dependencies form a cycle"),
        }
    }
}

impl std::error::Error for ContractError {}

impl TaskContract {
    pub fn from_json_str(text: &str) -> Result<Self, ContractError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| ContractError::MalformedJson(e.to_string()))?;
        if let serde_json::Value::Object(map) = &value {
            let has_schema = map.contains_key("schema_version") || map.contains_key("version");
            if !has_schema {
                return Err(ContractError::UnsupportedSchemaVersion(0));
            }
        }
        // Through the wire struct rather than `TaskContract`'s own
        // `Deserialize`, so a bad write-scope pattern comes back as the
        // `ContractError` that names it instead of a serde message.
        let wire: ContractWire = serde_json::from_value(value).map_err(|e| {
            let msg = e.to_string();
            if let Some(field) = msg
                .split("unknown field `")
                .nth(1)
                .and_then(|rest| rest.split('`').next())
            {
                return ContractError::UnknownField(field.to_string());
            }
            ContractError::MalformedJson(msg)
        })?;
        TaskContract::try_from(wire)
    }

    /// What this contract asks for: `change` or `inspect`.
    pub fn kind(&self) -> Kind {
        self.task.kind()
    }

    /// The declared write scope, or `None` for an inspect contract.
    pub fn write_scope(&self) -> Option<&WriteScope> {
        self.task.write_scope()
    }

    /// The declared scope patterns, empty for an inspect contract — the
    /// reading most callers want, since "no scope" and "an empty scope"
    /// mean the same thing to them.
    pub fn scope_patterns(&self) -> &[String] {
        self.task
            .write_scope()
            .map_or(&[], |scope| scope.patterns())
    }

    /// Everything a contract must satisfy beyond what its types already
    /// guarantee. The kind/scope agreement is not checked here: it is
    /// unrepresentable (see [`Task`]).
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ContractError::UnsupportedSchemaVersion(self.schema_version));
        }
        if self.objective.trim().is_empty() {
            return Err(ContractError::EmptyObjective);
        }
        if self.acceptance.is_empty() {
            return Err(ContractError::EmptyAcceptance);
        }
        if self.base_ref.trim().is_empty() {
            return Err(ContractError::EmptyBaseRef);
        }
        if self.verification_profile.trim().is_empty() {
            return Err(ContractError::EmptyVerificationProfile);
        }
        if self.limits.attempts < 1 {
            return Err(ContractError::BadAttempts(self.limits.attempts));
        }
        if self.limits.wall_seconds < 1 {
            return Err(ContractError::BadWallSeconds(self.limits.wall_seconds));
        }
        let mut seen = std::collections::BTreeSet::new();
        for criterion in &self.acceptance {
            if !seen.insert(criterion.trim()) {
                return Err(ContractError::DuplicateAcceptanceCriterion(
                    criterion.clone(),
                ));
            }
        }
        match self.task {
            Task::Change { .. } => {}
            Task::Inspect => {
                if self.decomposition.is_some() {
                    return Err(ContractError::DecompositionOnInspect);
                }
            }
        }
        if let Some(Decomposition::Plan(plan)) = &self.decomposition {
            plan.validate()?;
        }
        Ok(())
    }

    /// Canonical form: the fully materialized struct, not the source text.
    /// Two contracts that differ only in omitted defaults or key order
    /// hash identically, because defaults apply identically.
    pub fn canonical_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("TaskContract serializes")
    }

    /// The frozen hash, taken before the first worker. Stored in the
    /// ledger and repeated in every receipt for this task.
    pub fn hash(&self) -> String {
        canonical_json_hash(&self.canonical_value())
    }
}

/// Which control groups differ between two contract revisions. The spec's
/// revision-forcing controls (objective, scope, acceptance, budget) are
/// distinguished from advisory fields so callers can require renewed
/// verification for the former (SPEC §4).
///
/// `scope` covers the declared write scope AND the decomposition: a work
/// plan partitions that scope among packages, so replacing the plan
/// changes what may be written and by whom (P13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangedControls {
    pub objective: bool,
    pub scope: bool,
    pub acceptance: bool,
    pub budget: bool,
    pub base: bool,
    pub other: bool,
}

impl ChangedControls {
    pub fn requires_new_revision(&self) -> bool {
        self.objective || self.scope || self.acceptance || self.budget || self.base
    }

    pub fn any(&self) -> bool {
        self.requires_new_revision() || self.other
    }
}

pub fn changed_controls(old: &TaskContract, new: &TaskContract) -> ChangedControls {
    ChangedControls {
        objective: old.objective != new.objective,
        scope: old.task != new.task || old.decomposition != new.decomposition,
        acceptance: old.acceptance != new.acceptance,
        budget: old.limits != new.limits,
        base: old.base_ref != new.base_ref,
        other: old.verification_profile != new.verification_profile
            || old.review != new.review
            || old.architecture != new.architecture
            || old.risk_hints != new.risk_hints
            || old.read_hints != new.read_hints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
      "schema_version": 1,
      "kind": "change",
      "objective": "Preserve quotes and backslashes in amont list JSON output",
      "base_ref": "HEAD",
      "write_scope": ["crates/amont-runtime/**", "crates/amont/**"],
      "read_hints": ["crates/amont-runtime", "crates/amont"],
      "acceptance": [
        "Output parses as JSON and preserves original string values",
        "Existing fields and exit semantics remain unchanged",
        "The commit path acquires no external dependencies"
      ],
      "verification_profile": "rust-change",
      "architecture": {"keys": [], "scope": null},
      "risk_hints": ["public-output-contract"],
      "limits": {"attempts": 3, "wall_seconds": 1200},
      "review": "required"
    }"#;

    #[test]
    fn parses_the_spec_example() {
        let c = TaskContract::from_json_str(EXAMPLE).expect("spec example parses");
        assert_eq!(c.kind(), Kind::Change);
        assert_eq!(c.review, Review::Required);
        assert_eq!(c.limits.attempts, 3);
        assert_eq!(
            c.scope_patterns(),
            ["crates/amont-runtime/**", "crates/amont/**"]
        );
        assert!(c.architecture.keys.is_empty());
        assert_eq!(c.architecture.scope, None);
    }

    /// P5: a scope is untrusted input. A pattern that cannot bound
    /// anything is refused where the contract is parsed, naming the
    /// pattern — not after a worker ran, as a git error.
    #[test]
    fn a_scope_pattern_that_cannot_bound_anything_is_refused_by_name() {
        let with_scope = |scope: &str| {
            EXAMPLE.replace(
                "[\"crates/amont-runtime/**\", \"crates/amont/**\"]",
                &format!("[{scope}]"),
            )
        };
        for (scope, problem) in [
            ("\"/etc/**\"", ScopeProblem::Absolute),
            ("\"../other-repo/**\"", ScopeProblem::LeavesTheScope),
            ("\"src/../../x\"", ScopeProblem::LeavesTheScope),
            ("\"   \"", ScopeProblem::Empty),
        ] {
            let error = TaskContract::from_json_str(&with_scope(scope)).unwrap_err();
            let ContractError::BadWriteScopePattern {
                pattern,
                problem: found,
            } = &error
            else {
                panic!("scope {scope} must be refused, got {error:?}");
            };
            assert_eq!(found, &problem);
            assert!(
                scope.contains(pattern.as_str()),
                "the error names the pattern: {error}"
            );
        }
        // A glob globset cannot compile.
        let error = TaskContract::from_json_str(&with_scope("\"src/[unclosed/**\"")).unwrap_err();
        assert!(
            matches!(
                &error,
                ContractError::BadWriteScopePattern {
                    problem: ScopeProblem::NotAGlob(_),
                    ..
                }
            ),
            "got {error:?}"
        );
        assert!(error.to_string().contains("src/[unclosed/**"), "{error}");
    }

    #[test]
    fn a_compiled_scope_matches_repository_relative_paths() {
        let scope =
            WriteScope::compile(vec!["crates/**".into(), "Cargo.toml".into()]).expect("compiles");
        assert!(scope.is_match("crates/relais/src/main.rs"));
        assert!(scope.is_match("Cargo.toml"));
        assert!(!scope.is_match("docs/x.md"));
        assert_eq!(scope.patterns(), ["crates/**", "Cargo.toml"]);
    }

    #[test]
    fn rejects_unknown_fields() {
        let bad = EXAMPLE.replace(
            "\"kind\": \"change\"",
            "\"kind\": \"change\", \"effort\": \"high\"",
        );
        assert_eq!(
            TaskContract::from_json_str(&bad).unwrap_err(),
            ContractError::UnknownField("effort".into())
        );
    }

    #[test]
    fn rejects_wrong_schema_version() {
        let bad = EXAMPLE.replace("schema_version\": 1", "schema_version\": 2");
        assert_eq!(
            TaskContract::from_json_str(&bad).unwrap_err(),
            ContractError::UnsupportedSchemaVersion(2)
        );
    }

    #[test]
    fn accepts_version_alias() {
        let aliased = EXAMPLE.replace("schema_version", "version");
        let c = TaskContract::from_json_str(&aliased).expect("version alias parses");
        assert_eq!(c.schema_version, 1);
    }

    #[test]
    fn change_requires_write_scope() {
        let bad = EXAMPLE.replace(
            "\"write_scope\": [\"crates/amont-runtime/**\", \"crates/amont/**\"],",
            "",
        );
        assert_eq!(
            TaskContract::from_json_str(&bad).unwrap_err(),
            ContractError::MissingWriteScope
        );
    }

    /// P11: an inspect with a scope is not something `validate` catches,
    /// it is something no constructor produces. The wire form is still
    /// refused by name, for a contract file that spells it.
    #[test]
    fn inspect_rejects_write_scope() {
        let bad = EXAMPLE.replace("\"kind\": \"change\"", "\"kind\": \"inspect\"");
        assert_eq!(
            TaskContract::from_json_str(&bad).unwrap_err(),
            ContractError::WriteScopeOnInspect
        );
        let mut c = TaskContract::from_json_str(EXAMPLE).expect("parses");
        c.task = Task::Inspect;
        assert_eq!(c.write_scope(), None);
        assert!(c.scope_patterns().is_empty());
        c.validate().expect("inspect without write scope validates");
    }

    #[test]
    fn rejects_empty_and_duplicate_acceptance() {
        let mut c = TaskContract::from_json_str(EXAMPLE).expect("parses");
        c.acceptance.clear();
        assert_eq!(c.validate().unwrap_err(), ContractError::EmptyAcceptance);
        c.acceptance = vec!["same".into(), "same".into()];
        assert_eq!(
            c.validate().unwrap_err(),
            ContractError::DuplicateAcceptanceCriterion("same".into())
        );
    }

    #[test]
    fn hash_is_stable_under_key_order_and_materialized_defaults() {
        let a = TaskContract::from_json_str(EXAMPLE).expect("parses");
        let reordered: serde_json::Value = serde_json::from_str(&EXAMPLE.replace(
            "{\n      \"schema_version\": 1,\n      \"kind\": \"change\",",
            "{\n      \"kind\": \"change\",\n      \"schema_version\": 1,",
        ))
        .expect("parses");
        let b: TaskContract = serde_json::from_value(reordered).expect("parses");
        assert_eq!(a.hash(), b.hash());

        let minimal = r#"{
          "schema_version": 1, "kind": "inspect",
          "objective": "o", "base_ref": "HEAD",
          "acceptance": ["evidence"], "verification_profile": "p"
        }"#;
        let m1 = TaskContract::from_json_str(minimal).expect("parses");
        let mut m2 = m1.clone();
        m2.limits = Limits::default();
        assert_eq!(m1.hash(), m2.hash(), "omitted limits hash as their default");
        assert_eq!(m1.review, Review::Optional);
    }

    fn plan_with_ids(first: &str, second: &str) -> WorkPlan {
        let package = |id: &str| WorkPackage {
            id: id.into(),
            objective: "do the thing".into(),
            write_scope: vec!["src/**".into()],
            depends_on: Vec::new(),
            acceptance: vec!["it builds".into()],
        };
        WorkPlan {
            packages: vec![package(first), package(second)],
            integration_acceptance: vec!["the whole thing builds".into()],
            limits: PlanLimits::default(),
        }
    }

    // SPEC §19 + the scheduler: a package id is a directory name on disk,
    // and under `"decomposition": "propose"` it is model output.
    #[test]
    fn plan_package_ids_are_safe_path_components() {
        plan_with_ids("api", "web-ui_2")
            .validate()
            .expect("ordinary ids validate");

        for bad in ["../x", "/abs", "a/b", ".hidden", "-lead", "x y", "é"] {
            assert_eq!(
                plan_with_ids(bad, "ok").validate().unwrap_err(),
                ContractError::PlanBadPackageId(bad.into()),
                "id `{bad}` must be refused"
            );
        }
        // Empty is caught by the earlier empty/duplicate rule.
        assert_eq!(
            plan_with_ids("", "ok").validate().unwrap_err(),
            ContractError::PlanDuplicatePackage(String::new())
        );
        // 64 characters is the ceiling; 65 is refused.
        let long = "a".repeat(64);
        plan_with_ids(&long, "ok")
            .validate()
            .expect("64 characters fit");
        let too_long = "a".repeat(65);
        assert_eq!(
            plan_with_ids(&too_long, "ok").validate().unwrap_err(),
            ContractError::PlanBadPackageId(too_long)
        );
    }

    #[test]
    fn revision_classification() {
        let base = TaskContract::from_json_str(EXAMPLE).expect("parses");
        let mut new = base.clone();
        assert!(!changed_controls(&base, &new).any());
        new.objective = "different".into();
        assert!(changed_controls(&base, &new).objective);
        new = base.clone();
        new.limits.attempts = 2;
        assert!(changed_controls(&base, &new).budget);
        new = base.clone();
        new.read_hints.push("extra".into());
        let changed = changed_controls(&base, &new);
        assert!(changed.other && !changed.requires_new_revision());
        new = base.clone();
        new.base_ref = "main".into();
        assert!(changed_controls(&base, &new).requires_new_revision());
    }

    /// P13: a work plan partitions the declared scope among packages, so
    /// replacing it changes who may write what. It used to land in no
    /// group at all, and the revision read as unchanged.
    #[test]
    fn a_changed_decomposition_is_a_scope_change() {
        let base = TaskContract::from_json_str(EXAMPLE).expect("parses");
        let mut new = base.clone();
        new.decomposition = Some(Decomposition::Mode(DecompositionMode::Propose));
        let changed = changed_controls(&base, &new);
        assert!(changed.scope, "a new decomposition is a scope change");
        assert!(changed.requires_new_revision());

        let mut narrowed = base.clone();
        narrowed.task = Task::change(vec!["crates/amont/**".into()]).expect("compiles");
        assert!(changed_controls(&base, &narrowed).scope);
    }
}
