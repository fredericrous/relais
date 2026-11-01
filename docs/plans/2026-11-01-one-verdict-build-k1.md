---
status: active
branch: feat/prune-verify-and-runs
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [K1]
---
# One verdict per tree, build: K1 cleanup (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`. This branch is phase **K1**: relais removes the empty `verify/<run>/` directories it leaves, and `relais resume --prune-runs <days>` prunes old `runs/<run>/` directories, previewing first.
