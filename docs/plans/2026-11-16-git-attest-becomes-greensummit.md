---
status: active
canonical: greenharness/greensummit docs/plans/2026-11-16-git-attest-becomes-greensummit.md
plan-id: p-01M7JZ490DTGP50C29SRAYEMYB
branch: chore/greensummit-readers
repos: [relais]
adrs: [ADR-0073, ADR-0069, ADR-0037]
phases: [3]
---
# git-attest becomes greensummit (relais slice)

Pointer to the canonical plan in greenharness/greensummit. This branch is
the relais slice of its Phase 3 (readers). The sign pin, the release
verifier and `allowed_signers` change in this one PR, because the release
gate (`ATTEST_CHECK=1`) is not fail-open; no relais release is tagged
before main has re-signed under `greensummit`.

## Code review
- relais: backend, platform, lang:python (lang:rust, unix, tui, architect left out: CI files, one script and its Python test; Rust only in comments)

## This slice

- `.github/workflows/ci.yaml`: `fredericrous/attest@v1` →
  `greenharness/greensummit@v2`; `fredericrous/attest/sign@c5c6181… # v1.7.0`
  → `greenharness/greensummit/sign@9a54c1db… # v2.0.0` (same inputs).
- `.github/allowed_signers`: namespaces `amont-attest,greengate-attest` →
  `greensummit` on all three lines, the `cluster-signer@x86_64-linux` line
  included; provenance namespaces kept; line count unchanged.
- `.github/workflows/release.yaml`: download greensummit 2.0.0
  (`greensummit-2.0.0-x86_64-unknown-linux-musl`, tag `v2.0.0`,
  `greenharness/greensummit`, sha256 `bc0ea42f…e515eb`, checked against the
  release's SHA256SUMS and the downloaded archive), exported as
  `GREENSUMMIT_BIN`.
- `scripts/ci-release-platforms.sh`: `GIT_ATTEST` → `GREENSUMMIT_BIN`,
  default binary `greensummit`.
- `scripts/test_ci_platform.py`: the fake verifier and its env var follow.

## Verification

1. `python3 scripts/test_ci_platform.py` green.
2. Census in this repo outside history:
   `git grep -nE 'git-attest|git_attest|fredericrous/attest|greengate-attest|amont-attest|attest-local/|attest-inputs|GIT_ATTEST' -- . ':(exclude)CHANGELOG.md' ':(exclude)docs/plans' ':(exclude)docs/adr' ':(exclude)docs/programmes' ':(exclude).adr/packs'`
   lists only the allowlist (the key comment `amont-attest` on the
   laptop line, as in greensummit's own file; generated `HEADS.md` prose).
3. The release verifier against real notes: greensummit 2.0.0's
   `covered --platform <p>` run on a relais tree.
4. After merge (canonical Verification 4): main's CI signs under
   `refs/notes/greensummit`, a release dry run finds the signature on all
   three platforms, and the laptop's collect files a cluster-signed
   `greensummit-v2` block for relais.

## Decision log

## Implementation review
