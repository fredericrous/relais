---
status: active
branch: feat/per-check-timings
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [T1]
---
# One verdict per tree, build: T1 per-check timings (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`.
This branch is phase **T1**: relais records per-check timings (queued,
admitted, started, ended, slot wait, run time) per check and per retry, in
the receipt and in a `check_timings` ledger table, and `relais report` shows
per-label percentiles. Design: decisions
`docs/programmes/one-verdict-per-tree/t09-per-check-durations.md`.
