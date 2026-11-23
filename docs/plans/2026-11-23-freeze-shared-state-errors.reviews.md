# Full reviews: freeze the SharedState error and grant types

Round 1 (backend, rust, tui, unix): all approve-with-changes, applied; rust finding 1 (outcome.rs) rejected, the real callers are run, learn, decide, programme.
Round 2: unix, rust approve; tui approve-with-changes (carried into implementation: `node_error(what, e)` passes the same `what` into `Operational`, and the mapping test asserts the operation string); backend blocking late-write fence answered by a Decision-log statement (the ledger fences within one ledger; a grant token is an-t13's), then approve on the final body.
Architect (round 1, then carried): lapse check requires `HeldElsewhere` (applied), breaking CHANGELOG (applied), `NodeError` at `shared_state::node::NodeError` (applied).
Po (round 1): doc requirement not yet written (applied), breaking changelog (applied), existing-suite diff bounded (applied); carried into implementation: pin run, learn and decide error lines; an after-release measure (follow-up).
