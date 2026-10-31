---
status: active
branch: build/ci-platform-schedule
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [C1]
adrs: [decisions:ADR-0041]
---
# One verdict per tree: C1, relais CI proves each platform once (pointer)

The canonical plan is decisions `docs/plans/2026-11-01-one-verdict-build.md`. This branch is phase **C1**:

- macOS is declared as relais's primary platform.
- A pull request runs `test` on macOS, plus Ubuntu and Windows when it touches platform code or carries `full-ci`.
- main and the weekly schedule run all three platforms.
- The release waits for all three `test` legs on main for the tagged commit.

Design: decisions `docs/programmes/one-verdict-per-tree/t06-relais-ci-platform-schedule.md` and `t08-relais-primary-platform.md`.
