---
canonical: amont:docs/plans/2026-10-19-one-quiet-run-entry-implementation.md
phases: [3]
status: done
---
# One implementation of the quiet-run active entry (pointer)

The plan lives in amont, which owns the contract and the `amont-quiet`
crate. This repository carries Phase 3: `quiet.rs` `active_dir` delegates to
`amont_quiet::dir`, so the writers and this reader cannot name different
directories. The watcher's own liveness check stays relais's.

## Implementation review

approve-with-changes, then approve-with-changes on the Delta.
- relais had no `active_dir` test. `active_dir_keeps_the_rule_it_had` now pins every case, and it passes against both the delegated rule and the old body.
- The lock entry shows `amont-quiet` brings no dependencies.
- The canonical plan's Phase 3 row is corrected in amont's closing PR.
