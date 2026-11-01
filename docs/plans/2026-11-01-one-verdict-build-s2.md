---
status: active
branch: feat/verify-through-amont
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [S2]
---
# One verdict per tree, build: S2 relais verifies through amont (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`. This branch builds phase **S2**:

- relais's own profile runs each gate as `amont run --stamp <gate>`.
- A missing stamp or a deferred gate is a verification gap, never a pass.
- A base that amont already proved green skips its baseline run.
