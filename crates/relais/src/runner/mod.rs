//! Attempt lifecycle (SPEC §9, §19).
//!
//! States: prepared, running, verifying, repairing, escalating, accepted,
//! needs_review, needs_decision, blocked, failed, budget_exhausted,
//! cancelled, interrupted. Every transition carries a reason code,
//! timestamp and evidence references. Workers can propose completion or
//! blockage; only the runner assigns final state. Escalation uses a fresh
//! context that labels previous model claims unverified, never changes
//! acceptance criteria, and default ceilings are at most three attempts:
//! initial, one repair, one stronger attempt. A bounded planner may
//! propose work packages under explicit aggregate limits.
