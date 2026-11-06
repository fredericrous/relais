---
canonical: decisions:docs/plans/2026-11-06-harness-part-reports-its-cost.md
phases: [3]
status: active
branch: feat/harness-step-cost
---
# A harness part reports its cost beside its catches

Pointer. The plan and its review panel live in decisions. This repository
carries Phase 3: `relais report` puts each harness step's cost beside what
it caught (`harness.steps[].models[]`: cost, tokens, completeness, cost per
catch), report schema 16 → 17. Phase 1 shipped in amont-agent#212; Phase 2
was measured 2026-11-06 and skipped (guard firings cost 0.39–1.13% of
session cost against a 1% bar).

## Code review

- relais: backend, lang:rust, unix, tui, architect (po left out: report fields only)

## Verification

(filled at the close)
