---
status: done
branch: build/ci-sign
repos: [relais]
canonical: decisions:docs/plans/2026-11-01-one-verdict-build.md
phases: [C2]
adrs: [decisions:ADR-0041, decisions:ADR-0037]
---
# Relais CI signs attestations (C2)

Canonical plan: decisions `docs/plans/2026-11-01-one-verdict-build.md`, phase C2. Design: decisions `docs/programmes/one-verdict-per-tree/t10-relais-ci-signs.md`.

## What changed
- **Signer:** `.github/allowed_signers` gains `ci@relais` (namespaces `amont-attest,amont-provenance`). Its key is the repo secret `ATTEST_SIGNING_KEY`, set by the person.
- **Sign job** (`ci.yaml`, push to `main` only, after `test`): one leg per matrix platform. Each leg uploads a result file saying whether its own suite ran and passed. The sign job signs `pre-push-cargo-test` on a platform only when that leg's suite ran. A leg whose suite an attestation skipped signs nothing, so only real runs are signed.
- **Release guard** (`release.yaml`, `ci-release-platforms.sh`): the check-run guard stays and always runs. The attestation read (`git-attest covered --platform`, for `pre-push-cargo-test` on all three platforms) sits beside it, behind `ATTEST_CHECK`, which stays `0` in `release.yaml` until the first signed run is proven on `main`.

## Why the check-run guard stays
A tag must not depend on a signature that does not exist yet. Turning the attestation check on before main has a signed run would block every release. Once a signed run is proven, `release.yaml` sets `ATTEST_CHECK=1` and a tag needs both.

## Verification (input → expected → actual)
- **Signers file:** `ci@relais` line parses (principal and namespaces). Expected: a valid entry alongside `zougi.fr@gmail.com`. Actual: both principals present, format matches `ci@amont`.
- **Workflows:** `ci.yaml` and `release.yaml` parse as YAML. Expected: no parse error. Actual: parse ok; `sign` is a job after `test`.
- **Guard script:** `sh -n` and shellcheck on `ci-release-platforms.sh`. Expected: clean. Actual: clean.
- **Guard tests:** `scripts/test_ci_platform.py`, 21 tests including three new attestation cases (off ignores signatures; on requires every platform; on passes when all three are signed). Expected: all pass. Actual: 21 OK.

## Not yet proven (post-merge)
The real proof is that a push to `main` produces three `ci@relais` blocks: `git notes --ref amont-attest show <main tree>` on origin carries one block per platform for `pre-push-cargo-test`. Until that is seen, `ATTEST_CHECK` stays `0`.

## Implementation review
- Round 1 on d1b91ac0 (by the parent): approve-with-changes. Fixed:
  - `release.yaml`'s `platforms` job installs git-attest 1.7.0 checksum-verified and exports `GIT_ATTEST`, as amont does. It is inert while `ATTEST_CHECK` is 0, so turning it on is one token. The verifier's stderr is no longer hidden. The `holds-until:` markers name only the flag flip.
  - The sign job no longer requires every test leg to pass: each leg's result file decides, so passing legs are signed even when another fails. The comment and message match.
  - `continue-on-error: true` on the sign job's artifact download, so a missing file takes the no-sign path.
- After the fixes: 21 script tests OK, both workflows parse, shellcheck clean.

## Implementation review

- Approved after round 1 (on d1b91ac0) and a Delta (on a3678554).
- Round 1 was approve-with-changes: 1 medium finding (install the verifier now, so the switch is one token) and 2 low (sign each leg, as amont does; `continue-on-error` on the download). All were fixed in 648c1e07.
- Follow-up, low: add `&& needs.plan.result == 'success'` to the `sign` job's `if`, so a failed `plan` skips the job instead of erroring on an empty matrix.
- After merge: check that `git notes --ref amont-attest show <main tree>` on origin carries three `ci@relais` blocks. Then do one dry-run release with `ATTEST_CHECK=1` before committing the flip.
