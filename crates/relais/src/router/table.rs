//! The capability table (plan §2): which tier a task's classification
//! requires. Data, versioned, served by `router-state` exactly as the wire
//! contract spells it; [`tier_for`] evaluates it the way the plugin does,
//! so the two sides can be held to the same cases. Pure.

use serde::Serialize;

use crate::policy::Tier;

/// The table's version on the wire. A change to the rules is a new
/// version, and a change to both sides (docs/router-protocol.md).
pub const CAPABILITY_TABLE_VERSION: u32 = 1;

/// One rule's condition. Every field present must hold; an empty
/// condition always holds.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct When {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty_min: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub difficulty_max: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<Vec<&'static str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<&'static str>>,
    /// The task is verifiable, or a question with no edits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifiable_or_question: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Rule {
    pub when: When,
    pub tier: Tier,
}

/// The served table: the first rule whose condition holds decides.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CapabilityTable {
    pub version: u32,
    pub rules: Vec<Rule>,
}

/// The defaults of plan §2:
/// - escalation: difficulty ≥ 4, or high uncertainty beyond `local`;
/// - research: difficulty ≤ 2, low uncertainty, and verifiable or a
///   question;
/// - implementation: everything else.
pub fn default_table() -> CapabilityTable {
    CapabilityTable {
        version: CAPABILITY_TABLE_VERSION,
        rules: vec![
            Rule {
                when: When {
                    difficulty_min: Some(4),
                    ..When::default()
                },
                tier: Tier::Escalation,
            },
            Rule {
                when: When {
                    uncertainty: Some(vec!["high"]),
                    scope: Some(vec!["module", "cross-cutting", "unknown"]),
                    ..When::default()
                },
                tier: Tier::Escalation,
            },
            Rule {
                when: When {
                    difficulty_max: Some(2),
                    uncertainty: Some(vec!["low"]),
                    verifiable_or_question: Some(true),
                    ..When::default()
                },
                tier: Tier::Research,
            },
            Rule {
                when: When::default(),
                tier: Tier::Implementation,
            },
        ],
    }
}

/// What the table reads from a classification.
#[derive(Debug, Clone, PartialEq)]
pub struct Classification<'a> {
    pub kind: &'a str,
    pub difficulty: u8,
    pub scope: &'a str,
    pub uncertainty: &'a str,
    pub verifiable: bool,
}

impl When {
    fn holds(&self, class: &Classification<'_>) -> bool {
        self.difficulty_min
            .is_none_or(|min| class.difficulty >= min)
            && self
                .difficulty_max
                .is_none_or(|max| class.difficulty <= max)
            && self
                .uncertainty
                .as_ref()
                .is_none_or(|set| set.contains(&class.uncertainty))
            && self
                .scope
                .as_ref()
                .is_none_or(|set| set.contains(&class.scope))
            && self
                .verifiable_or_question
                .is_none_or(|want| (class.verifiable || class.kind == "question") == want)
    }
}

/// The tier the first matching rule names; implementation when none
/// matches (the default table's last rule always does).
pub fn tier_for(table: &CapabilityTable, class: &Classification<'_>) -> Tier {
    table
        .rules
        .iter()
        .find(|rule| rule.when.holds(class))
        .map_or(Tier::Implementation, |rule| rule.tier)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class<'a>(
        kind: &'a str,
        difficulty: u8,
        scope: &'a str,
        uncertainty: &'a str,
        verifiable: bool,
    ) -> Classification<'a> {
        Classification {
            kind,
            difficulty,
            scope,
            uncertainty,
            verifiable,
        }
    }

    #[test]
    fn a_hard_investigation_goes_to_escalation_even_as_research() {
        let table = default_table();
        // Difficulty alone.
        assert_eq!(
            tier_for(&table, &class("question", 4, "local", "low", false)),
            Tier::Escalation
        );
        // A subtle concurrency bug: uncertain, beyond one file.
        assert_eq!(
            tier_for(&table, &class("debug", 3, "module", "high", false)),
            Tier::Escalation
        );
        // High uncertainty that stays local is not enough.
        assert_eq!(
            tier_for(&table, &class("debug", 3, "local", "high", true)),
            Tier::Implementation
        );
    }

    #[test]
    fn a_rename_with_tests_runs_cheap_and_a_hard_question_does_not() {
        let table = default_table();
        assert_eq!(
            tier_for(&table, &class("edit", 1, "module", "low", true)),
            Tier::Research
        );
        assert_eq!(
            tier_for(&table, &class("question", 2, "local", "low", false)),
            Tier::Research
        );
        // Not verifiable, and an edit: no evidence to back a cheap model.
        assert_eq!(
            tier_for(&table, &class("edit", 1, "local", "low", false)),
            Tier::Implementation
        );
        assert_eq!(
            tier_for(&table, &class("question", 2, "local", "medium", false)),
            Tier::Implementation
        );
    }

    #[test]
    fn an_unknown_scope_is_implementation_unless_uncertainty_is_high() {
        let table = default_table();
        assert_eq!(
            tier_for(&table, &class("edit", 3, "unknown", "medium", true)),
            Tier::Implementation
        );
        assert_eq!(
            tier_for(&table, &class("edit", 2, "unknown", "medium", true)),
            Tier::Implementation
        );
        assert_eq!(
            tier_for(&table, &class("edit", 2, "unknown", "high", true)),
            Tier::Escalation
        );
    }

    #[test]
    fn the_table_serializes_exactly_as_the_contract_spells_it() {
        let value = serde_json::to_value(default_table()).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "version": 1,
                "rules": [
                    { "when": { "difficulty_min": 4 }, "tier": "escalation" },
                    { "when": { "uncertainty": ["high"], "scope": ["module", "cross-cutting", "unknown"] }, "tier": "escalation" },
                    { "when": { "difficulty_max": 2, "uncertainty": ["low"], "verifiable_or_question": true }, "tier": "research" },
                    { "when": {}, "tier": "implementation" }
                ]
            })
        );
    }
}
