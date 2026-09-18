//! Owned learning (SPEC §16–§17): features, dataset, learner, predict,
//! evaluate, registry.
//!
//! No upstream pretrained router weights or scores are loaded in inference,
//! feature generation or label generation. Regularized logistic regression
//! for acceptance without escalation, a separate regularized cost estimator
//! for complete strategies, deterministic pre-dispatch feature extraction
//! shared between training and inference, versioned artifacts with schema
//! and finite-value validation, and promotion that is evidence-gated.
//! Risk floors and required checks are never trainable parameters.
