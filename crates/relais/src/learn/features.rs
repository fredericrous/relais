//! Pre-dispatch feature extraction (SPEC §16, §17, §21).
//!
//! Deterministic, shared byte-for-byte between training and inference.
//! ONLY information available at dispatch enters: actual patch size,
//! later failures and final outcomes are labels, never features. Free
//! text (the objective) is untrusted data, hashed through a frozen
//! tokenizer configuration — never an embedding API, never policy
//! instructions. Unavailable features are represented explicitly (the
//! caller records absence), not defaulted into silence.

use serde::{Deserialize, Serialize};

use crate::contract::{Kind, TaskContract};
use crate::money::MicroUsd;
use crate::policy::{RepoPolicy, Tier};

pub const FEATURE_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_HASHED_BUCKETS: usize = 256;

/// Frozen hashing configuration: the tokenizer and bucket count live in
/// the artifact and both train and inference read them from the same
/// place (SPEC §16: "the same frozen tokenizer and hashing configuration
/// at train and inference time").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureSchema {
    pub version: u32,
    pub hashed_buckets: usize,
}

impl FeatureSchema {
    pub fn standard() -> Self {
        Self {
            version: FEATURE_SCHEMA_VERSION,
            hashed_buckets: DEFAULT_HASHED_BUCKETS,
        }
    }
}

/// Dense task-level features, all observable before any dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TaskFeatures {
    pub kind_change: f64,
    pub kind_inspect: f64,
    pub scope_patterns: f64,
    pub scope_wildcards: f64,
    pub read_hints: f64,
    pub acceptance_count: f64,
    pub risk_hints: f64,
    pub verification_commands: f64,
    pub verification_amont_checks: f64,
    pub architecture_keys: f64,
    pub objective_len: f64,
}

pub const TASK_FEATURE_COUNT: usize = 11;
pub const TIER_COUNT: usize = 3;

impl TaskFeatures {
    pub fn extract(contract: &TaskContract, repo: &RepoPolicy) -> Self {
        let scope_patterns = contract
            .write_scope
            .as_deref()
            .is_some_and(|s| !s.is_empty()) as u64 as f64;
        let scope_wildcards = contract
            .write_scope
            .as_deref()
            .map(|patterns| patterns.iter().any(|pattern| pattern.contains('*')) as u64 as f64)
            .unwrap_or(0.0);
        let profile = repo
            .verification
            .profiles
            .get(&contract.verification_profile);
        TaskFeatures {
            kind_change: (contract.kind == Kind::Change) as u64 as f64,
            kind_inspect: (contract.kind == Kind::Inspect) as u64 as f64,
            scope_patterns,
            scope_wildcards,
            read_hints: contract.read_hints.len().min(16) as f64,
            acceptance_count: contract.acceptance.len().min(16) as f64,
            risk_hints: contract.risk_hints.len().min(16) as f64,
            verification_commands: profile
                .map(|profile| profile.commands.len().min(8) as f64)
                .unwrap_or(-1.0),
            verification_amont_checks: profile
                .map(|profile| profile.amont_checks.len().min(8) as f64)
                .unwrap_or(-1.0),
            architecture_keys: contract.architecture.keys.len().min(16) as f64,
            objective_len: contract.objective.len().min(512) as f64,
        }
    }

    /// The cost cohort this task belongs to. Cohorts are task KINDS — the
    /// unit the registry advertises in `cohorts` and the one a cohort mean
    /// is an honest baseline for — not tiers: a tier is what the router
    /// chooses, and pricing a cohort by the choice under test would make
    /// the baseline a function of the decision it is meant to check.
    pub fn cohort(&self) -> &'static str {
        if self.kind_inspect > 0.0 {
            "inspect"
        } else {
            "change"
        }
    }

    pub fn dense(&self) -> [f64; TASK_FEATURE_COUNT] {
        [
            self.kind_change,
            self.kind_inspect,
            self.scope_patterns,
            self.scope_wildcards,
            self.read_hints,
            self.acceptance_count,
            self.risk_hints,
            self.verification_commands,
            self.verification_amont_checks,
            self.architecture_keys,
            self.objective_len,
        ]
    }
}

/// The cost cohort a contract belongs to: its kind, so training and
/// inference name the same cohorts (SPEC §16 "supported cohorts").
pub fn cohort_of_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Change => "change",
        Kind::Inspect => "inspect",
    }
}

/// The execution profile's identity at dispatch (SPEC §16: "model/effort/
/// harness identity" are initial features). A new model or harness
/// version is a new identity; evidence is not blindly inherited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProfileIdentity {
    pub model: String,
    pub effort: Option<String>,
    pub harness: Option<String>,
}

impl ProfileIdentity {
    /// The tokens this identity hashes to. Prefixed so `sonnet` the model
    /// and `sonnet` in an objective never share a bucket.
    fn tokens(&self) -> Vec<String> {
        let mut tokens = vec![format!("model={}", self.model)];
        if let Some(effort) = &self.effort {
            tokens.push(format!("effort={effort}"));
        }
        if let Some(harness) = &self.harness {
            tokens.push(format!("harness={harness}"));
        }
        tokens
    }
}

/// One (index, value) sparse vector over the expanded layout:
/// [task features (11)] + [tier one-hot (3)] + [task×tier interactions
/// (11×3)] + [hashed buckets: objective tokens and profile identity].
/// Interactions exist so capability varies by task class rather than
/// only assigning a global strength to each model (SPEC §16).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparseVec(pub Vec<(usize, f64)>);

fn tier_offset(tier: Tier) -> usize {
    TASK_FEATURE_COUNT
        + match tier {
            Tier::Research => 0,
            Tier::Implementation => 1,
            Tier::Escalation => 2,
        }
}

/// Tokenizer: lowercase, split on non-alphanumeric, tokens capped at 24
/// chars. Frozen with FEATURE_SCHEMA_VERSION; changing it is a new
/// schema version, never a silent edit.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            current.push(ch.to_ascii_lowercase());
            if current.chars().count() >= 24 {
                tokens.push(std::mem::take(&mut current));
            }
        } else if !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub fn hash_bucket(token: &str, buckets: usize) -> usize {
    (fnv1a(token.as_bytes()) % buckets as u64) as usize
}

pub fn expand(
    task: &TaskFeatures,
    tier: Tier,
    objective: &str,
    identity: &ProfileIdentity,
    schema: &FeatureSchema,
) -> SparseVec {
    let mut features: Vec<(usize, f64)> = Vec::with_capacity(48);
    for (index, value) in task.dense().into_iter().enumerate() {
        features.push((index, value));
    }
    let tier_hot = tier_offset(tier);
    features.push((tier_hot, 1.0));
    // Interactions: each task feature gated by this tier.
    for (index, value) in task.dense().into_iter().enumerate() {
        features.push((
            TASK_FEATURE_COUNT + TIER_COUNT + index * TIER_COUNT + (tier_hot - TASK_FEATURE_COUNT),
            value,
        ));
    }
    let interaction_base = TASK_FEATURE_COUNT + TIER_COUNT + TASK_FEATURE_COUNT * TIER_COUNT;
    for token in tokenize(objective).into_iter().chain(identity.tokens()) {
        features.push((
            interaction_base + hash_bucket(&token, schema.hashed_buckets),
            1.0,
        ));
    }
    // Coalesce duplicate hashed buckets into counts.
    features.sort_by_key(|(index, _)| *index);
    let mut coalesced: Vec<(usize, f64)> = Vec::with_capacity(features.len());
    for (index, value) in features {
        match coalesced.last_mut() {
            Some((last_index, last_value)) if *last_index == index => {
                *last_value += value;
            }
            _ => coalesced.push((index, value)),
        }
    }
    SparseVec(coalesced)
}

pub fn feature_dim(schema: &FeatureSchema) -> usize {
    TASK_FEATURE_COUNT + TIER_COUNT + TASK_FEATURE_COUNT * TIER_COUNT + schema.hashed_buckets
}

pub fn dot(features: &SparseVec, weights: &[f64]) -> f64 {
    features
        .0
        .iter()
        .map(|(index, value)| {
            weights
                .get(*index)
                .copied()
                .map(|weight| weight * value)
                .unwrap_or(0.0)
        })
        .sum()
}

/// Scaling fitted ONLY on training data (SPEC §16), applied identically
/// at train and inference.
///
/// Scale only, no centering. The vectors are sparse: an absent entry IS
/// zero, and a transform that subtracted a mean would have to touch every
/// absent entry to stay a transform of the same space. Dividing by the
/// root-mean-square over ALL N samples (absent entries counted as zero)
/// keeps zero at zero and puts every coordinate on a comparable scale.
/// The earlier version took moments over present entries only — a
/// statistic of a different, per-coordinate population — in O(dim·N·nnz).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Standardization {
    /// Kept at zero; retained in the artifact so the transform's shape is
    /// explicit and a future centering scheme is a schema change, not a
    /// silent reinterpretation.
    pub means: Vec<f64>,
    pub stds: Vec<f64>,
}

impl Standardization {
    pub fn fit(feature_sets: &[SparseVec], dim: usize) -> Self {
        let n = feature_sets.len().max(1) as f64;
        let mut sum_squares = vec![0.0f64; dim];
        for features in feature_sets {
            for (index, value) in &features.0 {
                if let Some(slot) = sum_squares.get_mut(*index) {
                    *slot += value * value;
                }
            }
        }
        let stds = sum_squares
            .iter()
            .map(|sum| {
                let rms = (sum / n).sqrt();
                if rms > 1e-8 {
                    rms
                } else {
                    1.0
                }
            })
            .collect();
        Self {
            means: vec![0.0; dim],
            stds,
        }
    }

    pub fn apply(&self, features: &SparseVec) -> SparseVec {
        SparseVec(
            features
                .0
                .iter()
                .map(|(index, value)| {
                    let mean = self.means.get(*index).copied().unwrap_or(0.0);
                    let std = self.stds.get(*index).copied().unwrap_or(1.0);
                    (*index, (value - mean) / std)
                })
                .collect(),
        )
    }
}

/// A complete training example: features, the label (evidence-backed),
/// and the complete-strategy cost. Attempt-level outcomes stay distinct
/// from complete-strategy outcomes (SPEC §17): `accepted_without_escalation`
/// is the attempt-level acceptance label; `complete_cost` belongs to the
/// whole strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingExample {
    pub family: String,
    pub tier: Tier,
    /// The dispatch-time inputs, kept so an evaluator can ask what the
    /// artifact would have chosen among OTHER tiers for this task —
    /// `sparse` alone is the expansion for the observed tier only.
    pub task: TaskFeatures,
    pub objective: String,
    pub identity: ProfileIdentity,
    /// `expand(task, tier, objective, identity)`, cached.
    pub sparse: SparseVec,
    pub accepted_without_escalation: bool,
    pub complete_cost: MicroUsd,
    pub cost_complete: bool,
    pub dispatched_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> TaskContract {
        TaskContract::from_json_str(
            &serde_json::json!({
                "schema_version": 1, "kind": "change",
                "objective": "Fix the JSON escaping defect in list output",
                "base_ref": "HEAD", "write_scope": ["crates/**"],
                "read_hints": ["crates"],
                "acceptance": ["one", "two"],
                "verification_profile": "default",
                "risk_hints": ["public-output-contract"],
            })
            .to_string(),
        )
        .expect("contract")
    }

    fn repo() -> RepoPolicy {
        RepoPolicy::from_toml_str(crate::policy::INIT_TEMPLATE).expect("policy")
    }

    #[test]
    fn extraction_is_deterministic_and_dispatch_time_only() {
        let task = TaskFeatures::extract(&contract(), &repo());
        assert_eq!(task.kind_change, 1.0);
        assert_eq!(task.kind_inspect, 0.0);
        assert_eq!(task.acceptance_count, 2.0);
        assert_eq!(task.risk_hints, 1.0);
        assert_eq!(task.verification_commands, 1.0);
        let again = TaskFeatures::extract(&contract(), &repo());
        assert_eq!(task, again);
    }

    #[test]
    fn expansion_is_shared_between_train_and_inference() {
        let schema = FeatureSchema::standard();
        let task = TaskFeatures::extract(&contract(), &repo());
        let identity = ProfileIdentity {
            model: "sonnet".into(),
            effort: Some("medium".into()),
            harness: Some("claude-code 2.1".into()),
        };
        let a = expand(
            &task,
            Tier::Implementation,
            &contract().objective,
            &identity,
            &schema,
        );
        let b = expand(
            &task,
            Tier::Implementation,
            &contract().objective,
            &identity,
            &schema,
        );
        assert_eq!(a, b, "the same inputs give the same features");
        assert!(a.0.iter().all(|(index, _)| *index < feature_dim(&schema)));
        let escalation = expand(
            &task,
            Tier::Escalation,
            &contract().objective,
            &identity,
            &schema,
        );
        let a_hot =
            a.0.iter()
                .find(|(index, _)| *index == tier_offset(Tier::Implementation));
        assert!(a_hot.is_some(), "tier one-hot is present");
        let escalation_hot = escalation
            .0
            .iter()
            .find(|(index, _)| *index == tier_offset(Tier::Escalation));
        assert!(escalation_hot.is_some());
        // A new harness version is a new identity (SPEC §17: evidence is
        // not blindly inherited).
        let newer = ProfileIdentity {
            harness: Some("claude-code 2.2".into()),
            ..identity.clone()
        };
        let c = expand(
            &task,
            Tier::Implementation,
            &contract().objective,
            &newer,
            &schema,
        );
        assert_ne!(a, c);
    }

    #[test]
    fn hashed_features_use_the_frozen_tokenizer() {
        let schema = FeatureSchema::standard();
        let task = TaskFeatures::extract(&contract(), &repo());
        let identity = ProfileIdentity::default();
        let a = expand(
            &task,
            Tier::Research,
            "Fix the JSON escaping defect",
            &identity,
            &schema,
        );
        let b = expand(
            &task,
            Tier::Research,
            "fix THE json ESCAPING defect",
            &identity,
            &schema,
        );
        assert_eq!(a, b, "case and punctuation do not move the hash");
        let c = expand(
            &task,
            Tier::Research,
            "completely different objective",
            &identity,
            &schema,
        );
        assert_ne!(a, c);
        assert_eq!(hash_bucket("json", 256), hash_bucket("json", 256));
    }

    #[test]
    fn scaling_is_fitted_on_the_given_data_and_keeps_zero_at_zero() {
        let dim = 3;
        let samples = vec![
            SparseVec(vec![(0, 3.0), (1, 1.0)]),
            SparseVec(vec![(0, 4.0)]),
        ];
        let standardization = Standardization::fit(&samples, dim);
        // Coordinate 0: rms over BOTH samples = sqrt((9+16)/2) = 3.5355…
        assert!((standardization.stds[0] - (12.5f64).sqrt()).abs() < 1e-12);
        // Coordinate 1: the absent entry counts as zero: sqrt((1+0)/2).
        assert!((standardization.stds[1] - (0.5f64).sqrt()).abs() < 1e-12);
        // Coordinate 2 was never seen: scale 1, never a division by zero.
        assert_eq!(standardization.stds[2], 1.0);
        let applied = standardization.apply(&SparseVec(vec![(1, 1.0), (7, 2.0)]));
        assert!(applied.0.iter().all(|(_, value)| value.is_finite()));
        assert_eq!(
            applied.0[1],
            (7, 2.0),
            "an index past the fit is passed through"
        );
        assert!(standardization.means.iter().all(|mean| *mean == 0.0));
    }

    #[test]
    fn dot_product_ignores_out_of_range_indices() {
        let weights = vec![1.0; 4];
        let features = SparseVec(vec![(0, 2.0), (9, 5.0)]);
        assert_eq!(dot(&features, &weights), 2.0);
    }
}
