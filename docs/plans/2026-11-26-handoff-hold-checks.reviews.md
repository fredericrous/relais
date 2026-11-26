# Reviews: handoff hold checks

Round 1 (opus): backend approve-with-changes (adds=contract; kept lease outlives the file's hold; module doc; verification results), lang:rust approve-with-changes (same overrun; `reserve_in_empty_programme` has the same give-back; module doc), tui approve-with-changes (error arm through `compensated`), unix approve (the read is safe under the existing lock).

Round 2 (opus): backend blocking (the `run_all` case cannot assert a moved file end on `LocalState`), then approve on the final body; architect high (same), then approve; unix approve; lang:rust, po, tui, approve-with-changes with low/medium findings carried into implementation: name the two `node.rs` unit tests and the skipped-rewrite mutation in Verification, `Ok(None)` answer for the empty programme, LocalState note in Goal, a post-release check (relais-fabric runs `run_all_with_clock` at its next pin bump; one report of a ticket granted while the file keeps the handoff means revert), error arm at both call sites, clock skew between node and primary.

Codex: not run (programme ticket agent, ADR-0080).
