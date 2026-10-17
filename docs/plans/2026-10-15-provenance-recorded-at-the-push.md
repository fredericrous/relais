---
canonical: https://github.com/fredericrous/decisions/blob/main/docs/plans/2026-10-15-provenance-recorded-at-the-push.md
phases: [5, 6]
status: active
---
# Pointer: provenance is recorded at the push

This repository carries Phase 5 of the plan (decisions ADR-0036): a run's
landing is found once and recorded.

Phase 6 (added 2026-10-16): a landing search that finds nothing keeps the
last commit it checked, and the next pass resumes from there.
