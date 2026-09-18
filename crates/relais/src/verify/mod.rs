//! Verification and acceptance (SPEC §10).
//!
//! Verification runs after all candidate-writing descendants have stopped
//! or relinquished their write leases, against an immutable copy of the
//! candidate, recording identity, base SHA, contract and policy hashes,
//! commands, exit statuses, timeouts and log hashes. Preflight captures
//! baseline failures; existing failures are not automatically waived. A
//! skipped, inert, unavailable or untrusted required check is a gap, not a
//! pass. An accepted receipt is bound to one candidate and does not
//! authorize merge or survive edits.
