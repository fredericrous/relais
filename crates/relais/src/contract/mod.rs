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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContract {
    /// SPEC §4 calls the required field "version"; the example spells it
    /// `schema_version`. Both are accepted, one canonical name is written.
    #[serde(alias = "version")]
    pub schema_version: u64,
    pub kind: Kind,
    pub objective: String,
    pub base_ref: String,
    /// Bounded write scope. Required and non-empty for `change`, must be
    /// absent for `inspect`.
    #[serde(default)]
    pub write_scope: Option<Vec<String>>,
    #[serde(default)]
    pub read_hints: Vec<String>,
    /// Acceptance criteria; for `inspect` these are evidence criteria.
    pub acceptance: Vec<String>,
    pub verification_profile: String,
    #[serde(default)]
    pub architecture: Architecture,
    #[serde(default)]
    pub risk_hints: Vec<String>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub review: Review,
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
    DuplicateAcceptanceCriterion(String),
    BadAttempts(u32),
    BadWallSeconds(u64),
    EmptyBaseRef,
    EmptyVerificationProfile,
    MalformedJson(String),
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
            Self::DuplicateAcceptanceCriterion(c) => {
                write!(f, "duplicate acceptance criterion: {c}")
            }
            Self::BadAttempts(n) => write!(f, "limits.attempts must be >= 1, got {n}"),
            Self::BadWallSeconds(n) => write!(f, "limits.wall_seconds must be >= 1, got {n}"),
            Self::EmptyBaseRef => write!(f, "base_ref is empty"),
            Self::EmptyVerificationProfile => write!(f, "verification_profile is empty"),
            Self::MalformedJson(detail) => write!(f, "contract is not valid JSON: {detail}"),
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
        let contract: TaskContract = serde_json::from_value(value).map_err(|e| {
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
        contract.validate()?;
        Ok(contract)
    }

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
        match self.kind {
            Kind::Change => {
                if self.write_scope.as_deref().is_none_or(|s| s.is_empty()) {
                    return Err(ContractError::MissingWriteScope);
                }
            }
            Kind::Inspect => {
                if self.write_scope.as_deref().is_some_and(|s| !s.is_empty()) {
                    return Err(ContractError::WriteScopeOnInspect);
                }
            }
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
        scope: old.write_scope != new.write_scope,
        acceptance: old.acceptance != new.acceptance,
        budget: old.limits != new.limits,
        base: old.base_ref != new.base_ref,
        other: old.kind != new.kind
            || old.verification_profile != new.verification_profile
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
        assert_eq!(c.kind, Kind::Change);
        assert_eq!(c.review, Review::Required);
        assert_eq!(c.limits.attempts, 3);
        assert_eq!(
            c.write_scope.as_deref().unwrap(),
            ["crates/amont-runtime/**", "crates/amont/**"]
        );
        assert!(c.architecture.keys.is_empty());
        assert_eq!(c.architecture.scope, None);
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

    #[test]
    fn inspect_rejects_write_scope() {
        let mut c = TaskContract::from_json_str(EXAMPLE).expect("parses");
        c.kind = Kind::Inspect;
        assert_eq!(
            c.validate().unwrap_err(),
            ContractError::WriteScopeOnInspect
        );
        c.write_scope = None;
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
}
