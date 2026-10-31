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

## Verification record

- **`python3 scripts/test_ci_platform.py`:** 13 tests passed, with `-W error::ResourceWarning`. They cover:
  - the classifier:
    - docs-only → `false`
    - a portable source file → `false`
    - `#[cfg(unix)]` in `crates/relais/src/ipc.rs` → `true`, reason "compiles per platform"
    - a `cfg(unix)` test under `tests/` → `false`
    - each build input (workflow, script, Makefile, Cargo.lock, crate manifest, installer) → `true`
    - an unreadable base → `true`
  - the plan:
    - a portable PR → matrix `[macos]`, run_other `false`, and the summary names Ubuntu and Windows as deferred
    - platform code, `full-ci`, or an unset PLATFORM_CODE → all three
    - push and schedule → all three
    - each leg's attest platform
  - the release guard:
    - all three legs green → 0
    - a missing or failed leg → 1, naming it
    - an unreadable answer → 1
- **The classifier on real main history** (a scratch detached worktree, each commit against its parent):

  | Commit | Expected | Actual |
  |---|---|---|
  | #363 (relais.toml, policy/mod.rs) | false | false |
  | #362 (ledger) | false | false |
  | v0.44.0 release (Cargo.toml, Cargo.lock) | true | true |
- **Lint:** `shellcheck` is clean on the three scripts, and both workflows parse as YAML (ruby psych). `jobs`: ci = lint, plan, test, msrv, plugin, audit; release gains `platforms` and `publish` needs it.
- **Attest platforms:** each leg now names its attestation platform: macOS `aarch64-macos`, ubuntu `x86_64-linux`, windows `x86_64-windows`. These equal each runner's default, so the existing skip is unchanged (attest `action.yml`: the default is the runner's own platform).
  - The laptop's notes are `x86_64-macos`, so they still do not skip the macOS leg, as before.
  - Accepting `macos` (any arch) on that leg is a separate decision for the person.
- **This PR's own CI** touches workflows and scripts, so it runs all three legs (`run_other` true, reason "platform-specific changes"). That is expected. The deferral path (a docs-only PR running `test (macos-latest)` only, with the summary line) is first observed on the next docs-only PR, read by merge-when-green there.
