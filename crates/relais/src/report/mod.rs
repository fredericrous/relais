//! Cost and outcome reporting (SPEC §11).
//!
//! Requested and effective model, effort, harness version, token
//! categories, provider-reported cost, estimated cost, duration, attempt,
//! phase and final outcome are recorded per dispatch. Missing usage is
//! marked unknown, never zero. An inclusive parent total is never added to
//! its children, and stable request/event IDs dedupe. The primary metric
//! is total recorded cost — failed runs included — divided by accepted
//! tasks in the same cohort.
