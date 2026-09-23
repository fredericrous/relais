//! Acceptance criteria and the evidence that settles them (SPEC §4, §10).
//!
//! A criterion has always been a bare string, and that is still a valid
//! criterion. A declared criterion adds a statement's identity, whether
//! it must be met, and which named check, test, review or sign-off
//! settles it — so a receipt can say which criteria were actually met
//! and by what, instead of leaving that to a worker's own claim.

use serde::{Deserialize, Serialize};

use crate::ids::sha256_hex;

/// One acceptance entry, parsed from a single untagged JSON value: a bare
/// string, exactly as every contract has always written one, or a
/// declared criterion naming its own evidence. Untagged so a contract
/// written entirely in bare strings is unchanged, byte for byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AcceptanceEntry {
    Bare(String),
    Declared(DeclaredCriterion),
}

/// A declared criterion: a statement, an identity, whether it is
/// mandatory, and the evidence that settles it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredCriterion {
    pub statement: String,
    /// The author's own id, when given. Absent, the id is derived from
    /// the statement's content (see [`AcceptanceEntry::id`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Absent means mandatory, exactly as a bare string always was.
    #[serde(default = "default_mandatory")]
    pub mandatory: bool,
    pub evidence: Evidence,
}

fn default_mandatory() -> bool {
    true
}

/// The evidence that settles a declared criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    /// A named command in the verification profile (`CommandSpec.name`).
    Check { name: String },
    /// A test, with who wrote it — a model writing the test that judges
    /// its own work is not evidence about the work.
    Test { authorship: TestAuthorship },
    /// A reviewing model's judgement: useful, never independent.
    LlmReview,
    /// A human's explicit sign-off.
    HumanSignOff,
}

/// Who wrote the test that is a criterion's evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestAuthorship {
    PreExisting,
    HumanAdded,
    ModelAdded,
}

/// Whether this evidence is independent ground truth about the work, or
/// only a claim about it. A check and a pre-existing or human-added test
/// answer a question nobody working on the change got to grade; a
/// model-added test and an LLM review do not, because a model writing
/// the test that judges its own work is not evidence about the work
/// (SPEC §10). A human sign-off is independent by definition — it is a
/// person's own judgement, not the implementing model's.
///
/// Pure and exhaustive: every [`Evidence`] variant is named here, so a
/// new one added later fails to compile until this says whether it
/// counts.
pub fn independent(evidence: &Evidence) -> bool {
    match evidence {
        Evidence::Check { .. } => true,
        Evidence::Test { authorship } => match authorship {
            TestAuthorship::PreExisting | TestAuthorship::HumanAdded => true,
            TestAuthorship::ModelAdded => false,
        },
        Evidence::LlmReview => false,
        Evidence::HumanSignOff => true,
    }
}

impl AcceptanceEntry {
    /// The text a prompt or a report quotes: the bare string, or the
    /// declared statement.
    pub fn statement(&self) -> &str {
        match self {
            Self::Bare(statement) => statement,
            Self::Declared(criterion) => &criterion.statement,
        }
    }

    /// Whether this criterion must have evidence for acceptance. A bare
    /// string is mandatory, exactly as it always was.
    pub fn mandatory(&self) -> bool {
        match self {
            Self::Bare(_) => true,
            Self::Declared(criterion) => criterion.mandatory,
        }
    }

    /// The evidence a declared criterion names. `None` for a bare
    /// string: it is settled the way it always was, by the verification
    /// profile as a whole.
    pub fn evidence(&self) -> Option<&Evidence> {
        match self {
            Self::Bare(_) => None,
            Self::Declared(criterion) => Some(&criterion.evidence),
        }
    }

    /// This criterion's identity: the author's own id, or one derived
    /// from the statement's content, so reordering the acceptance list
    /// never renumbers a criterion (SPEC §4).
    pub fn id(&self) -> String {
        match self {
            Self::Bare(statement) => derive_id(statement),
            Self::Declared(criterion) => criterion
                .id
                .clone()
                .unwrap_or_else(|| derive_id(&criterion.statement)),
        }
    }
}

impl From<String> for AcceptanceEntry {
    fn from(statement: String) -> Self {
        Self::Bare(statement)
    }
}

impl From<&str> for AcceptanceEntry {
    fn from(statement: &str) -> Self {
        Self::Bare(statement.to_string())
    }
}

/// A criterion id derived from its statement's content: stable across
/// reordering, and independent of everything else in the contract.
fn derive_id(statement: &str) -> String {
    format!("c-{}", &sha256_hex(statement.trim().as_bytes())[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_string_parses_and_is_mandatory_with_no_evidence() {
        let entry: AcceptanceEntry = serde_json::from_str(r#""it builds""#).expect("parses");
        assert_eq!(entry, AcceptanceEntry::Bare("it builds".into()));
        assert_eq!(entry.statement(), "it builds");
        assert!(entry.mandatory());
        assert!(entry.evidence().is_none());
    }

    #[test]
    fn a_declared_criterion_round_trips() {
        let json = r#"{
            "statement": "the api rejects malformed input",
            "mandatory": false,
            "evidence": {"kind": "check", "name": "make check"}
        }"#;
        let entry: AcceptanceEntry = serde_json::from_str(json).expect("parses");
        assert_eq!(entry.statement(), "the api rejects malformed input");
        assert!(!entry.mandatory());
        assert_eq!(
            entry.evidence(),
            Some(&Evidence::Check {
                name: "make check".into()
            })
        );
    }

    #[test]
    fn an_id_is_derived_from_statement_content_when_absent() {
        let a: AcceptanceEntry = serde_json::from_value(serde_json::json!({
            "statement": "same statement",
            "evidence": {"kind": "llm_review"}
        }))
        .expect("parses");
        let b: AcceptanceEntry = serde_json::from_value(serde_json::json!({
            "statement": "same statement",
            "evidence": {"kind": "human_sign_off"}
        }))
        .expect("parses");
        assert_eq!(
            a.id(),
            b.id(),
            "the id depends on the statement, not the rest of the criterion"
        );

        let different: AcceptanceEntry = serde_json::from_value(serde_json::json!({
            "statement": "a different statement",
            "evidence": {"kind": "llm_review"}
        }))
        .expect("parses");
        assert_ne!(a.id(), different.id());
    }

    #[test]
    fn reordering_the_list_does_not_renumber_the_criteria() {
        let a: AcceptanceEntry = "first".into();
        let b: AcceptanceEntry = "second".into();
        let forwards = [a.clone(), b.clone()];
        let backwards = [b, a];
        let mut forward_ids: Vec<String> = forwards.iter().map(AcceptanceEntry::id).collect();
        let mut backward_ids: Vec<String> = backwards.iter().map(AcceptanceEntry::id).collect();
        forward_ids.sort();
        backward_ids.sort();
        assert_eq!(forward_ids, backward_ids);
        // And each entry keeps its own id regardless of position.
        assert_eq!(forwards[0].id(), backwards[1].id());
        assert_eq!(forwards[1].id(), backwards[0].id());
    }

    #[test]
    fn an_author_given_id_is_kept_as_written() {
        let entry: AcceptanceEntry = serde_json::from_value(serde_json::json!({
            "statement": "it builds",
            "id": "builds",
            "evidence": {"kind": "check", "name": "make"}
        }))
        .expect("parses");
        assert_eq!(entry.id(), "builds");
    }

    #[test]
    fn independence_is_exhaustive_over_evidence_kinds() {
        assert!(independent(&Evidence::Check { name: "x".into() }));
        assert!(independent(&Evidence::Test {
            authorship: TestAuthorship::PreExisting
        }));
        assert!(independent(&Evidence::Test {
            authorship: TestAuthorship::HumanAdded
        }));
        assert!(!independent(&Evidence::Test {
            authorship: TestAuthorship::ModelAdded
        }));
        assert!(!independent(&Evidence::LlmReview));
        assert!(independent(&Evidence::HumanSignOff));
    }

    #[test]
    fn a_declared_criterion_refuses_unknown_fields() {
        serde_json::from_value::<AcceptanceEntry>(serde_json::json!({
            "statement": "it builds",
            "evidence": {"kind": "check", "name": "make"},
            "surprise": true
        }))
        .unwrap_err();
    }
}
