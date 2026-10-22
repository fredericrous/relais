---
canonical: decisions:docs/plans/2026-10-21-harness-earns-its-place.md
phases: [4]
status: done
---
# A harness part earns its place on each model (pointer)

The plan lives in decisions, which owns ADR-0042. This repository carries
Phase 4: every pipeline step relais runs (verification, review, repair) names
the failure it prevents in SPEC, and `relais report` shows per step and model
how often review findings led to a revise and how often a repair turned a
failed check green. The decision that a trial cannot lower review stands.

## Implementation review

- Phase 4 (relais): approve after two reviewers. Fixed: the step is an enum; one review per run, credited to its last dispatch; reviews and repairs with no verdict reported apart; the unknown-model footnote, asserted; SPEC no longer points verification at a count that misses the initial attempt (holds-until). Carried to Phase 5: whether a clean review on a run that ends other than Accepted is misfiled as no-verdict (the live ledger has 26).

