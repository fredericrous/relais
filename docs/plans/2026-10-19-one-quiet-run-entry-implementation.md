---
canonical: amont:docs/plans/2026-10-19-one-quiet-run-entry-implementation.md
phases: [3]
status: active
---
# One implementation of the quiet-run active entry (pointer)

The plan lives in amont, which owns the contract and the `amont-quiet`
crate. This repository carries Phase 3: `quiet.rs` `active_dir` delegates to
`amont_quiet::dir`, so the writers and this reader cannot name different
directories. The watcher's own liveness check stays relais's.
