---
canonical: decisions:docs/plans/2026-10-25-relais-tests-follow-user-flows.md
phases: [2, 3, 4, 5, 6, 7]
status: active
---
# relais tests follow user flows (pointer)

The plan lives in the decisions repository, which owns the testing rules it
applies (ADR-0040). This repository carries phases [2, 3, 4, 5, 6, 7].

## Implementation review

- Phase 2: approve after a Delta.
  - Round 1, approve-with-changes (49k, 50 s). Delta, approve (52k, 18 s).
  - Fixed: the worker also backdates the cloned `dep/index.js`; the `relais.toml` note now dates its figure.
  - deliberate: the timings are recorded in the PR body, and the worker backdating the task tree is noted in Phase 8's decision log, because the canonical plan lives in decisions.
  - Still open: Windows is proven by the CI test matrix, and the full-suite before figure was not measured under the same load.
