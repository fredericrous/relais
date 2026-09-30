//! Recipe specs (SPEC §6, step 3): deterministic recipes, versioned.
//!
//! A recipe is a versioned, hashed unit. `revision` and `enabled` default
//! to values that leave a `relais.toml` written before this module existed
//! behaving exactly as it did: every recipe implicitly at revision 0 and
//! always enabled. `recipe_id` is derived purely from a recipe's own
//! content — no address, no iteration order, no clock — so it is stable
//! across runs and process restarts, and two structurally identical
//! recipes always agree on it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::contract::{Kind, Review};
use crate::ids::canonical_json_hash;

use super::{ContextPolicy, ExecutionPolicy, ModelProfile, Tier};

fn default_recipe_revision() -> u32 {
    0
}

fn is_default_recipe_revision(revision: &u32) -> bool {
    *revision == default_recipe_revision()
}

fn default_recipe_enabled() -> bool {
    true
}

fn is_default_recipe_enabled(enabled: &bool) -> bool {
    *enabled == default_recipe_enabled()
}

/// Explicitly configured deterministic recipe (SPEC §6, step 3). Used only
/// when it fully covers the task; never inferred from prose.
///
/// Every field but `revision` and `enabled` participates in
/// [`RecipeSpec::recipe_id`]; both of those participate too, deliberately
/// — a revision bump is a content change, and a disabled recipe is a
/// different recipe from the same one enabled. Fields default so a recipe
/// written before this module existed serializes identically to before
/// (`skip_serializing_if`), which is what keeps
/// [`super::RepoPolicy::authority_hash`] unmoved for a policy shaped as
/// today's are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeSpec {
    pub name: String,
    #[serde(default)]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub scope_within: Vec<String>,
    pub tier: Tier,
    /// Which version of this recipe this is. Two recipes may share a
    /// `name` only across different revisions (SPEC §6): `validate`
    /// refuses two recipes sharing `(name, revision)`.
    #[serde(
        default = "default_recipe_revision",
        skip_serializing_if = "is_default_recipe_revision"
    )]
    pub revision: u32,
    /// Whether this revision is eligible for selection. A disabled
    /// revision stays in the file — for history, or to re-enable later —
    /// but [`crate::route::route`] never selects it, whatever its
    /// revision.
    #[serde(
        default = "default_recipe_enabled",
        skip_serializing_if = "is_default_recipe_enabled"
    )]
    pub enabled: bool,
    /// Per-recipe models: when this recipe covers a task, the profile for
    /// the routed tier (model and start effort) is read from here instead
    /// of the repository's `models` table. The model must still be in the
    /// machine's `allowed_models`, and the effort stays under the
    /// authority ceiling (SPEC §6).
    ///
    /// Omitted from the serialized form when absent, like every optional
    /// field here, so a recipe that never set one leaves the authority
    /// hash exactly where it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<BTreeMap<Tier, ModelProfile>>,
    /// Per-recipe execution limits — DECLARED AND HASHED, NOT YET READ.
    /// No routing or dispatch path consults this today. Setting it
    /// therefore moves the authority hash, and costs a trust re-grant,
    /// while changing nothing that runs. It is here so the shape and the
    /// hash settle in one release rather than two. The release that reads
    /// it must delete this paragraph, and `CHANGELOG.md` says the same
    /// thing — if these two ever disagree, the code is right and the prose
    /// is stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionPolicy>,
    /// Per-recipe context budget — DECLARED AND HASHED, NOT YET READ;
    /// see [`RecipeSpec::execution`] for what that costs and why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ContextPolicy>,
    /// Per-recipe review floor override — DECLARED AND HASHED, NOT YET
    /// READ; see [`RecipeSpec::execution`] for what that costs and why. A
    /// tunable knob a candidate revision may raise but never lower (SPEC
    /// §17): the same shape as [`super::RiskRule::review`], one level
    /// down, so a recipe can demand more caution than the risk rules its
    /// scope touches without a repository editing those rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<Review>,
}

impl RecipeSpec {
    /// A pure, content-derived identity: the canonical-JSON hash of
    /// everything about the recipe EXCEPT `name`. Stable across runs and
    /// process restarts because it reads only fields already on the
    /// struct — no address, no iteration order, no clock. `name` is a
    /// label, not content: excluding it is what lets two differently
    /// named recipes that declare the same executable behaviour be
    /// caught as the same recipe declared twice (`validate_recipes`'s
    /// `DuplicateId`), distinct from `DuplicateNameRevision`, which is
    /// about the `(name, revision)` pair alone and fires even when the
    /// rest of the content differs. Two recipes differing only in
    /// `revision` hash differently (a revision bump is a content
    /// change); two structurally identical recipes — same content
    /// whether or not each wrote its defaults explicitly, whatever their
    /// names — hash the same.
    pub fn recipe_id(&self) -> String {
        let mut value =
            serde_json::to_value(self).expect("RecipeSpec serializes: string map keys, no floats");
        if let serde_json::Value::Object(map) = &mut value {
            map.remove("name");
        }
        canonical_json_hash(&value)
    }
}

/// Why a `[[recipes]]` table failed validation, naming the offenders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeError {
    /// Two recipes share both `name` and `revision` — the pair that is
    /// supposed to identify one recipe uniquely.
    DuplicateNameRevision { name: String, revision: u32 },
    /// Two differently named recipes hash to the same [`RecipeSpec::recipe_id`]
    /// — the same executable content declared twice.
    DuplicateId {
        recipe_id: String,
        first: String,
        second: String,
    },
}

impl std::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateNameRevision { name, revision } => write!(
                f,
                "two recipes share name `{name}` and revision {revision}; \
                 revisions of one recipe must be numbered distinctly"
            ),
            Self::DuplicateId {
                recipe_id,
                first,
                second,
            } => write!(
                f,
                "recipes `{first}` and `{second}` are the same recipe (id {recipe_id}); \
                 declaring identical content twice is not a second recipe"
            ),
        }
    }
}

impl std::error::Error for RecipeError {}

/// Refuses a `[[recipes]]` table with two recipes sharing `(name,
/// revision)`, or two recipes sharing a [`RecipeSpec::recipe_id`]. Pure:
/// no disk, no clock, just the recipes as given.
pub fn validate_recipes(recipes: &[RecipeSpec]) -> Result<(), RecipeError> {
    let mut seen_name_revision = BTreeSet::new();
    for recipe in recipes {
        let key = (recipe.name.clone(), recipe.revision);
        if !seen_name_revision.insert(key.clone()) {
            let (name, revision) = key;
            return Err(RecipeError::DuplicateNameRevision { name, revision });
        }
    }
    let mut seen_ids: BTreeMap<String, String> = BTreeMap::new();
    for recipe in recipes {
        let recipe_id = recipe.recipe_id();
        if let Some(first) = seen_ids.get(&recipe_id) {
            return Err(RecipeError::DuplicateId {
                recipe_id,
                first: first.clone(),
                second: recipe.name.clone(),
            });
        }
        seen_ids.insert(recipe_id, recipe.name.clone());
    }
    Ok(())
}

impl RecipeSpec {
    /// A recipe with only the fields a v1 policy could set, every field
    /// added since at its default.
    ///
    /// Exists so a construction site does not have to name fields it has
    /// no opinion about: `RecipeSpec { name, tier, ..RecipeSpec::covering(name, tier) }`
    /// keeps compiling when this struct grows again. Adding a field to a
    /// public struct with public fields IS a breaking change for every
    /// struct literal — Rust offers no way around that — so the useful
    /// property is not "this change broke nothing" but "the next one
    /// need not".
    pub fn covering(name: impl Into<String>, tier: Tier) -> Self {
        Self {
            name: name.into(),
            kind: None,
            scope_within: Vec::new(),
            tier,
            revision: default_recipe_revision(),
            enabled: default_recipe_enabled(),
            models: None,
            execution: None,
            context: None,
            review: None,
        }
    }
}

/// The identity of the recipe that covered a run, named on its dispatch
/// intent and its receipt (SPEC §12, §17): id, name and revision together,
/// never separately, so a reader cannot see a name with no id or a
/// revision with no name. A run no recipe covers records the absence of
/// this type — `None` — not a placeholder built from empty/zero fields;
/// the two are different facts and only one of them was ever true of a
/// given run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoveringRecipe {
    pub id: String,
    pub name: String,
    pub revision: u32,
}

/// What a receipt records about the recipe that produced a run. THREE
/// states, not two, because `Option` cannot hold the difference and the
/// difference is load-bearing:
///
/// - `NotRecorded` — this receipt predates the field. Nobody wrote down
///   whether a recipe covered the run, either way.
/// - `NoneCovered` — recorded, and no configured recipe covered the task:
///   the learner or the conservative baseline decided.
/// - `Covered` — recorded, and this is the recipe.
///
/// An `Option<CoveringRecipe>` collapsed the first two into one `None`,
/// so a reader counting "runs produced by a recipe" would have scored the
/// five receipts already stored on this machine as "no recipe covered"
/// rather than as unknown — a positive claim about a fact those receipts
/// never recorded. `NotRecorded` is the `Default`, so a receipt written
/// before this field existed still parses into the honest answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecipeRecord {
    #[default]
    NotRecorded,
    NoneCovered,
    Covered(CoveringRecipe),
}

impl RecipeRecord {
    /// What a run whose route is known records: the covering recipe, or
    /// `NoneCovered` when nothing covered it. Never `NotRecorded` — a
    /// caller that knows the route has, by definition, recorded it.
    pub fn of(covering: Option<CoveringRecipe>) -> Self {
        match covering {
            Some(recipe) => Self::Covered(recipe),
            None => Self::NoneCovered,
        }
    }

    /// The covering recipe, if one was recorded AND covered the task.
    /// `NotRecorded` and `NoneCovered` both answer `None` here, which is
    /// correct for a caller that only wants the recipe; a caller that
    /// must tell them apart matches on the variants instead.
    pub fn covering(&self) -> Option<&CoveringRecipe> {
        match self {
            Self::Covered(recipe) => Some(recipe),
            Self::NotRecorded | Self::NoneCovered => None,
        }
    }
}

/// Among the recipes a task's coverage predicate accepts, the one
/// `route` selects: the highest `revision` among those that are
/// `enabled`. A disabled recipe is never selected even when it is the
/// highest revision; a recipe the predicate rejects is never selected
/// whatever its revision. Ties keep the earliest recipe in `recipes`
/// order — `validate_recipes` already refuses two enabled recipes
/// sharing `(name, revision)`, so a tie here is two differently named
/// recipes, and config order is the only deterministic tie-break that
/// does not depend on process state.
pub fn select_highest_enabled_revision(
    recipes: &[RecipeSpec],
    mut covers: impl FnMut(&RecipeSpec) -> bool,
) -> Option<&RecipeSpec> {
    let mut best: Option<&RecipeSpec> = None;
    for recipe in recipes {
        if !recipe.enabled || !covers(recipe) {
            continue;
        }
        let take = match best {
            Some(current) => recipe.revision > current.revision,
            None => true,
        };
        if take {
            best = Some(recipe);
        }
    }
    best
}

#[cfg(test)]
mod tests {

    /// The distinction an `Option<CoveringRecipe>` could not hold. All
    /// three states must be mutually distinguishable, and the one a
    /// missing JSON key yields must be the honest one.
    #[test]
    fn a_missing_record_is_not_the_same_as_a_recorded_absence() {
        use super::{CoveringRecipe, RecipeRecord};

        // What `serde(default)` gives a receipt that predates the field.
        let from_old_json: RecipeRecord = serde_json::from_str("null").unwrap_or_default();
        assert_eq!(RecipeRecord::default(), RecipeRecord::NotRecorded);
        assert_eq!(from_old_json, RecipeRecord::NotRecorded);

        // What a run whose route IS known records when nothing covered it.
        let recorded_absence = RecipeRecord::of(None);
        assert_eq!(recorded_absence, RecipeRecord::NoneCovered);

        // The three are distinct: collapsing any pair would let a reader
        // score "nobody wrote it down" as "no recipe covered this".
        let covered = RecipeRecord::of(Some(CoveringRecipe {
            id: "rid-1".into(),
            name: "docs".into(),
            revision: 2,
        }));
        assert_ne!(RecipeRecord::NotRecorded, recorded_absence);
        assert_ne!(RecipeRecord::NotRecorded, covered);
        assert_ne!(recorded_absence, covered);

        // `covering()` answers None for both non-covered states, which is
        // right for a caller that only wants the recipe.
        assert!(RecipeRecord::NotRecorded.covering().is_none());
        assert!(recorded_absence.covering().is_none());
        assert_eq!(covered.covering().map(|r| r.revision), Some(2));

        // Round-trips, so a stored receipt reads back as what was written.
        for state in [RecipeRecord::NotRecorded, recorded_absence, covered] {
            let json = serde_json::to_string(&state).expect("serializes");
            assert_eq!(
                serde_json::from_str::<RecipeRecord>(&json).expect("parses"),
                state,
                "{json} must round-trip"
            );
        }
    }
    use super::*;

    fn recipe(name: &str, revision: u32, enabled: bool) -> RecipeSpec {
        RecipeSpec {
            name: name.to_string(),
            kind: None,
            scope_within: vec!["docs/**".to_string()],
            tier: Tier::Research,
            revision,
            enabled,
            models: None,
            execution: None,
            context: None,
            review: None,
        }
    }

    #[test]
    fn recipe_id_is_stable_and_content_derived() {
        let a = recipe("docs-touchup", 0, true);
        let b = recipe("docs-touchup", 0, true);
        assert_eq!(a.recipe_id(), b.recipe_id());
        assert_eq!(a.recipe_id(), a.recipe_id(), "stable across calls");
    }

    #[test]
    fn recipe_id_differs_by_revision_alone() {
        let a = recipe("docs-touchup", 0, true);
        let b = recipe("docs-touchup", 1, true);
        assert_ne!(a.recipe_id(), b.recipe_id());
    }

    #[test]
    fn structurally_identical_recipes_share_an_id() {
        let a = recipe("docs-touchup", 2, true);
        let mut b = recipe("docs-touchup", 2, true);
        b.name.clone_from(&a.name);
        assert_eq!(a.recipe_id(), b.recipe_id());
    }

    #[test]
    fn validate_refuses_duplicate_name_revision() {
        let recipes = vec![
            recipe("docs-touchup", 0, true),
            recipe("docs-touchup", 0, false),
        ];
        let err = validate_recipes(&recipes).unwrap_err();
        assert_eq!(
            err,
            RecipeError::DuplicateNameRevision {
                name: "docs-touchup".into(),
                revision: 0,
            }
        );
    }

    #[test]
    fn validate_refuses_duplicate_recipe_id() {
        let recipes = vec![
            recipe("docs-touchup", 0, true),
            recipe("docs-alias", 0, true),
        ];
        let err = validate_recipes(&recipes).unwrap_err();
        match err {
            RecipeError::DuplicateId { first, second, .. } => {
                assert_eq!(first, "docs-touchup");
                assert_eq!(second, "docs-alias");
            }
            other => panic!("expected DuplicateId, got {other:?}"),
        }
    }

    #[test]
    fn validate_accepts_distinct_revisions_of_one_name() {
        let recipes = vec![
            recipe("docs-touchup", 0, true),
            recipe("docs-touchup", 1, true),
        ];
        assert!(validate_recipes(&recipes).is_ok());
    }

    #[test]
    fn selection_picks_highest_enabled_revision_among_covering() {
        let recipes = vec![
            recipe("docs-touchup", 0, true),
            recipe("docs-touchup", 2, true),
        ];
        let picked = select_highest_enabled_revision(&recipes, |_| true).unwrap();
        assert_eq!(picked.revision, 2);
    }

    #[test]
    fn selection_skips_a_disabled_higher_revision() {
        let recipes = vec![
            recipe("docs-touchup", 0, true),
            recipe("docs-touchup", 2, false),
        ];
        let picked = select_highest_enabled_revision(&recipes, |_| true).unwrap();
        assert_eq!(
            picked.revision, 0,
            "the disabled higher revision is never selected"
        );
    }

    #[test]
    fn selection_skips_a_recipe_the_predicate_rejects() {
        let recipes = vec![recipe("docs-touchup", 5, true)];
        let picked = select_highest_enabled_revision(&recipes, |_| false);
        assert!(
            picked.is_none(),
            "a recipe that does not cover the task is never selected whatever its revision"
        );
    }
}
