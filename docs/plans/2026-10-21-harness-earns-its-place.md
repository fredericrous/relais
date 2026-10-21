---
canonical: decisions:docs/plans/2026-10-21-harness-earns-its-place.md
phases: [4]
status: active
---
# A harness part earns its place on each model (pointer)

The plan lives in decisions, which owns ADR-0042. This repository carries
Phase 4: every pipeline step relais runs (verification, review, repair) names
the failure it prevents in SPEC, and `relais report` shows per step and model
how often review findings led to a revise and how often a repair turned a
failed check green. The decision that a trial cannot lower review stands.
