//! Owned learning (SPEC §16–§17): features, dataset, learner, predict,
//! evaluate, registry.
//!
//! No upstream pretrained router weights or scores are loaded in
//! inference, feature generation or label generation. Regularized
//! logistic regression for acceptance without escalation, a separate
//! regularized cost estimator for complete strategies, deterministic
//! pre-dispatch feature extraction shared between training and
//! inference, versioned artifacts with schema and finite-value
//! validation, and promotion that is evidence-gated. Risk floors and
//! required checks are never trainable parameters. The complete loop is:
//! execute → verify → label → build dataset → train → evaluate →
//! promote or reject → monitor.

pub mod dataset;
pub mod evaluate;
pub mod features;
pub mod learner;
pub mod predict;
pub mod registry;
