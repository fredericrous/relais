---
canonical: decisions:docs/plans/2026-10-29-widen-holdout-acceptance.md
phases: [1]
status: done
branch: feat/holdout-attach
---
# Pointer: widen holdout acceptance

The plan lives in decisions (ADR-0045's home). This repository carries Phase 1: the `attach` declaration, attachment at preflight into an effective acceptance list rebuilt from the ledger, per-holdout fetch with an unreachable attached store skipped, concurrent or shared-build holdouts with per-holdout durations, `source` in ledger schema 45, `relais report` `by_source` (schema 15), `relais plan` attachment, and SPEC §4 and §10.

## Implementation review

- Round 1: approve-with-changes, 6 findings. The preflight skip of an attached holdout (blocking), no definitions row for a skipped holdout, the plan record, `relais plan` resolving attached holdouts and a placeholder name were fixed. The overlay-removal test was confirmed in verify/mod.rs.
- Delta: approve-with-changes. A new high finding, a double resolve when an optional named holdout is also attached, was fixed in 77458fc3.
- Fresh round on 77458fc3: approve, no edits. A flaky store between the named and the attached resolution of one name is kept as a known edge in the canonical plan.
