//! Recipe promotion and rollback (SPEC §27): the two human-gated ways a
//! recipe revision enters or leaves a repository's policy.
//!
//! Both produce one [`Amendment`] — the recipe revisions a `relais.toml`
//! would gain — and an amendment is only ever APPENDED as text: existing
//! bytes, comments and ordering are never rewritten, and recipe history
//! stays append-only, as `route::validate_candidate` already demands of a
//! candidate. Adding a change is gated: [`Amendment::for_promotion`]
//! takes a [`Promotable`], which only [`ComparisonReport::promotable`]
//! can hand out. Removing one is not: [`Amendment::for_rollback`] appends
//! a new revision that repeats an older one, and needs no evaluation.
//!
//! Nothing here reads a machine file, issues a trust grant, or runs
//! anything. The policy an amendment produces has a new authority hash,
//! so its trust grant is still a person's to paste.

use std::fmt;

use crate::policy::{select_highest_enabled_revision, PolicyError, RecipeSpec, RepoPolicy};

use super::comparison::{ComparisonReport, GateFailure, Promotable};

/// Which command an error belongs to, so every refusal names the
/// operation it refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Promote,
    Rollback,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Promote => f.write_str("relais recipe promote"),
            Self::Rollback => f.write_str("relais recipe rollback"),
        }
    }
}

/// A comparison that did not clear every gate, with the gates it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionRefused {
    pub subject: String,
    pub failures: Vec<GateFailure>,
}

impl fmt::Display for PromotionRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: refused for {}: the comparison does not clear every gate:",
            Operation::Promote,
            self.subject
        )?;
        for failure in &self.failures {
            write!(f, "\n  gate: {failure}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PromotionRefused {}

/// Recompute the report's gates and hand back the proof they hold.
/// Never reads a cached verdict: [`ComparisonReport::promotable`]
/// recomputes from the report's own figures on every call.
pub fn admit(report: &ComparisonReport, subject: &str) -> Result<Promotable, PromotionRefused> {
    report.promotable().ok_or_else(|| PromotionRefused {
        subject: subject.to_string(),
        failures: report.failures(),
    })
}

/// Why an amendment could not be built or appended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmendError {
    /// The candidate declares no recipe revision the repository lacks.
    NothingAdded { subject: String },
    /// No recipe carries the name; `known` is every name that exists.
    UnknownRecipe { name: String, known: Vec<String> },
    /// Rollback needs a revision below the effective one to restore.
    TooFewRevisions { name: String, revisions: Vec<u32> },
    /// Every revision of the recipe is disabled, so none is effective.
    NoEffectiveRevision { name: String, revisions: Vec<u32> },
    /// The effective revision is the lowest there is.
    NothingBelow { name: String, effective: u32 },
    /// A fragment could not be written as TOML.
    Render {
        operation: Operation,
        subject: String,
        cause: String,
    },
    /// `existing` plus the fragment does not parse to the policy the
    /// amendment means to produce, so nothing is appended.
    NotAppendable {
        operation: Operation,
        subject: String,
        cause: String,
    },
}

impl AmendError {
    /// The operation this refusal belongs to.
    fn operation(&self) -> Operation {
        match self {
            Self::UnknownRecipe { .. }
            | Self::TooFewRevisions { .. }
            | Self::NoEffectiveRevision { .. }
            | Self::NothingBelow { .. } => Operation::Rollback,
            Self::NothingAdded { .. } => Operation::Promote,
            Self::Render { operation, .. } | Self::NotAppendable { operation, .. } => *operation,
        }
    }
}

fn revisions_label(revisions: &[u32]) -> String {
    revisions
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for AmendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let operation = self.operation();
        match self {
            Self::NothingAdded { subject } => write!(
                f,
                "{operation}: {subject} adds no recipe revision the repository does not already \
                 declare"
            ),
            Self::UnknownRecipe { name, known } if known.is_empty() => write!(
                f,
                "{operation}: no recipe named `{name}`; relais.toml declares no recipes"
            ),
            Self::UnknownRecipe { name, known } => write!(
                f,
                "{operation}: no recipe named `{name}`; declared recipes: {}",
                known.join(", ")
            ),
            Self::TooFewRevisions { name, revisions } => write!(
                f,
                "{operation}: recipe `{name}` has {} revision(s) ({}); rollback needs at least two",
                revisions.len(),
                revisions_label(revisions)
            ),
            Self::NoEffectiveRevision { name, revisions } => write!(
                f,
                "{operation}: every revision of recipe `{name}` ({}) is disabled, so none is \
                 effective to roll back from",
                revisions_label(revisions)
            ),
            Self::NothingBelow { name, effective } => write!(
                f,
                "{operation}: revision {effective} of recipe `{name}` is effective and no revision \
                 lies below it"
            ),
            Self::Render { subject, cause, .. } => write!(
                f,
                "{operation}: cannot write the fragment for {subject} as TOML: {cause}"
            ),
            Self::NotAppendable { subject, cause, .. } => write!(
                f,
                "{operation}: appending to {subject} would not yield the intended policy, so \
                 nothing was written: {cause}"
            ),
        }
    }
}

impl std::error::Error for AmendError {}

/// The recipe revisions a `relais.toml` would gain. The field is private
/// and the two constructors are the only ways in: one needs a
/// [`Promotable`], the other only removes a change.
#[derive(Debug, Clone, PartialEq)]
pub struct Amendment {
    operation: Operation,
    added: Vec<RecipeSpec>,
}

impl Amendment {
    /// Which command this amendment belongs to, so a failure appending it
    /// names that command rather than a generic one.
    pub fn operation(&self) -> Operation {
        self.operation
    }

    /// The revisions `candidate` adds after `incumbent`'s own, given the
    /// proof its comparison cleared every gate. `candidate` must already
    /// have been admitted by `route::validate_candidate`, which is what
    /// guarantees it starts with every recipe the incumbent declares.
    pub fn for_promotion(
        _proof: Promotable,
        incumbent: &RepoPolicy,
        candidate: &RepoPolicy,
        subject: &str,
    ) -> Result<Self, AmendError> {
        let added: Vec<RecipeSpec> = candidate
            .recipes
            .iter()
            .skip(incumbent.recipes.len())
            .cloned()
            .collect();
        if added.is_empty() {
            return Err(AmendError::NothingAdded {
                subject: subject.to_string(),
            });
        }
        Ok(Self {
            operation: Operation::Promote,
            added,
        })
    }

    /// A new revision N+1 of recipe `name` whose fields equal revision
    /// N-1's, N being the revision routing selects today (the highest
    /// enabled one). History is never edited or disabled.
    pub fn for_rollback(incumbent: &RepoPolicy, name: &str) -> Result<Self, AmendError> {
        let mut revisions: Vec<&RecipeSpec> = incumbent
            .recipes
            .iter()
            .filter(|recipe| recipe.name == name)
            .collect();
        revisions.sort_by_key(|recipe| recipe.revision);
        let numbers: Vec<u32> = revisions.iter().map(|recipe| recipe.revision).collect();
        if revisions.is_empty() {
            let mut known: Vec<String> = Vec::new();
            for recipe in &incumbent.recipes {
                if !known.contains(&recipe.name) {
                    known.push(recipe.name.clone());
                }
            }
            return Err(AmendError::UnknownRecipe {
                name: name.to_string(),
                known,
            });
        }
        if revisions.len() < 2 {
            return Err(AmendError::TooFewRevisions {
                name: name.to_string(),
                revisions: numbers,
            });
        }
        let effective =
            select_highest_enabled_revision(&incumbent.recipes, |recipe| recipe.name == name)
                .ok_or_else(|| AmendError::NoEffectiveRevision {
                    name: name.to_string(),
                    revisions: numbers.clone(),
                })?;
        let below = revisions
            .iter()
            .rev()
            .find(|recipe| recipe.revision < effective.revision)
            .ok_or_else(|| AmendError::NothingBelow {
                name: name.to_string(),
                effective: effective.revision,
            })?;
        let newest = revisions.last().map_or(0, |recipe| recipe.revision);
        let restored = RecipeSpec {
            revision: newest + 1,
            enabled: true,
            ..(*below).clone()
        };
        Ok(Self {
            operation: Operation::Rollback,
            added: vec![restored],
        })
    }

    /// The revisions this amendment appends, oldest first.
    pub fn added(&self) -> &[RecipeSpec] {
        &self.added
    }

    /// The policy the repository would have after the append.
    pub fn resulting_policy(&self, incumbent: &RepoPolicy) -> RepoPolicy {
        let mut policy = incumbent.clone();
        policy.recipes.extend(self.added.iter().cloned());
        policy
    }

    /// The `[[recipes]]` fragment(s), as TOML text.
    pub fn fragment(&self, subject: &str) -> Result<String, AmendError> {
        let mut out = String::new();
        for recipe in &self.added {
            let body = toml::to_string(recipe).map_err(|cause| AmendError::Render {
                operation: self.operation,
                subject: subject.to_string(),
                cause: cause.to_string(),
            })?;
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("[[recipes]]\n");
            out.push_str(&body);
        }
        Ok(out)
    }

    /// The text to append to `existing` (the current `relais.toml`).
    /// Checked before it is returned: `existing` followed by it must
    /// parse to exactly [`Self::resulting_policy`], so a file the
    /// fragment cannot be appended to — recipes written inline, say —
    /// is refused rather than corrupted.
    pub fn appendix(&self, existing: &str, subject: &str) -> Result<String, AmendError> {
        let incumbent = RepoPolicy::from_toml_str(existing).map_err(|cause: PolicyError| {
            AmendError::NotAppendable {
                operation: self.operation,
                subject: subject.to_string(),
                cause: cause.to_string(),
            }
        })?;
        let mut appendix = String::new();
        if !existing.is_empty() && !existing.ends_with('\n') {
            appendix.push('\n');
        }
        appendix.push('\n');
        appendix.push_str(&self.fragment(subject)?);
        let expected = self.resulting_policy(&incumbent);
        let parsed =
            RepoPolicy::from_toml_str(&format!("{existing}{appendix}")).map_err(|cause| {
                AmendError::NotAppendable {
                    operation: self.operation,
                    subject: subject.to_string(),
                    cause: cause.to_string(),
                }
            })?;
        if parsed != expected {
            return Err(AmendError::NotAppendable {
                operation: self.operation,
                subject: subject.to_string(),
                cause: "the appended text does not parse back to the recipes it was written from"
                    .to_string(),
            });
        }
        Ok(appendix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Tier;

    const BASE: &str = "schema_version = 1\n\n# keep this comment\n[models.implementation]\nid = \"sonnet\"\n\n[verification.profiles.default]\ncommands = []\n";

    fn recipe(revision: u32, enabled: bool, tier: Tier) -> RecipeSpec {
        RecipeSpec {
            revision,
            enabled,
            ..RecipeSpec::covering("docs", tier)
        }
    }

    fn policy(recipes: Vec<RecipeSpec>) -> RepoPolicy {
        let mut policy = RepoPolicy::from_toml_str(BASE).expect("base parses");
        policy.recipes = recipes;
        policy
    }

    #[test]
    fn rollback_repeats_the_revision_below_the_effective_one_as_a_new_revision() {
        let incumbent = policy(vec![
            recipe(0, true, Tier::Research),
            recipe(1, true, Tier::Implementation),
        ]);
        let amendment = Amendment::for_rollback(&incumbent, "docs").expect("rolls back");
        let [added] = amendment.added() else {
            panic!("one revision: {:?}", amendment.added())
        };
        assert_eq!(added.revision, 2);
        assert!(added.enabled);
        assert_eq!(added.tier, Tier::Research);
    }

    #[test]
    fn rollback_of_one_revision_or_an_unknown_name_names_what_exists() {
        let single = policy(vec![recipe(0, true, Tier::Research)]);
        let too_few = Amendment::for_rollback(&single, "docs").unwrap_err();
        assert!(matches!(too_few, AmendError::TooFewRevisions { .. }));
        assert!(too_few.to_string().contains("`docs`"), "{too_few}");
        let unknown = Amendment::for_rollback(&single, "nope").unwrap_err();
        assert!(
            unknown.to_string().contains("declared recipes: docs"),
            "{unknown}"
        );
    }

    #[test]
    fn appendix_keeps_the_existing_bytes_and_parses_to_the_new_policy() {
        let incumbent = RepoPolicy::from_toml_str(BASE).expect("parses");
        let with_two = {
            let mut p = incumbent.clone();
            p.recipes = vec![
                recipe(0, true, Tier::Research),
                recipe(1, true, Tier::Implementation),
            ];
            p
        };
        let amendment = Amendment::for_rollback(&with_two, "docs").expect("rolls back");
        // The base text declares no recipes, so build the file that does.
        let base_two = format!(
            "{BASE}\n[[recipes]]\nname = \"docs\"\ntier = \"research\"\n\n[[recipes]]\nname = \"docs\"\ntier = \"implementation\"\nrevision = 1\n"
        );
        let appendix = amendment
            .appendix(&base_two, "relais.toml")
            .expect("appends");
        assert!(appendix.contains("[[recipes]]"), "{appendix}");
        let after = format!("{base_two}{appendix}");
        assert!(after.starts_with(&base_two));
        let parsed = RepoPolicy::from_toml_str(&after).expect("parses");
        assert_eq!(parsed.recipes.len(), 3);
    }
}
