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

- **`python3 scripts/test_ci_platform.py`:** first 13 tests, then 18 after review round 1, all passing with `-W error::ResourceWarning`. They cover:
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

## Review round 1: fixes (approve-with-changes)

- **The guard no longer swallows errors** (`errors.never-swallowed`). `gh` and `jq` failures print `::warning::check-runs query failed: <reason>` with the tool's own stderr, then count as "nothing passed yet".
  - Test input → expected → actual: a fake `gh` that exits 1 with "HTTP 403 rate limited" → exit 1, warning, the reason and the missing leg in stderr → as expected.
  - Unreadable JSON → exit 1 plus the warning → as expected.
- **The guard counts only push-to-main runs.** It lists workflow runs with `head_sha=<sha>&event=push&branch=main`, then reads those runs' jobs. A pull request's run of the same commit may have deferred legs, so it does not count.
  - Test: a fake `gh` logs its query → the log contains `head_sha=abc123&event=push&branch=main` → as expected.
  - The job needs `actions: read`, not `checks: read`; `release.yaml` is updated.
- **Greppable deferral marker** on the stopgap (`change.a-deferral-names-its-ceiling`): `holds-until: relais CI signs attestations (plan C2); then read git-attest, as ci-release-gates.sh does` is in `scripts/ci-release-platforms.sh` and in the `platforms` comment in `release.yaml`.
- **Classifier widened.** The pattern is now `cfg(_attr)?!?\([^)]*(windows|unix|target_os|target_family)`, so a keyword inside `cfg(`, `cfg_attr(` or `cfg!(`, however nested, counts. It matches the real `tooling.rs` (`cfg!(windows)`), `holdout.rs`, `ipc.rs` and `procs.rs`.
  - 30 product source files under `crates/*/src` match, so a PR touching any of them runs all three platforms. That is the safe side of ADR-0041's "an uncertain classification runs the platform".
  - Fixtures: `cfg!(windows)`, `any(…)`, `all(test, unix)`, `not(…)`, `target_os`, `cfg_attr` → `true`; `cfg(test)` and `cfg(feature = "x")` → `false`.
- **A deleted source file counts:** `crates/*/src/*` that no longer exists → `true`, reason "deleted, cannot inspect". Test passes.
- **Shape of the matrix:** one `test` job whose matrix comes from `plan` (macOS always; ubuntu and windows when `run_other`), not separate `test-macos` and `test-other` jobs. The behaviour is equivalent, and the check names stay `test (<os>)`.
- **Branch protection:** none on relais main (`gh api repos/fredericrous/relais/branches/main/protection` → 404), and squash, rebase and merge are all allowed. So a dropped matrix leg cannot block a PR, and nothing but the release guard stops an untested platform from shipping.
- **Decision (the person, via the parent):** the macOS leg keeps the exact platform `aarch64-macos`. The laptop's `x86_64-macos` notes do not cover it.
