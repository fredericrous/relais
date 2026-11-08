---
canonical: fredericrous/adr-lint docs/plans/2026-11-08-ci-fetches-release-assets-api.md
phases: [2]
status: done
branch: fix/gh-api-downloads
repos: [relais]
---

# CI fetches GitHub release assets through the API, fleet-wide (pointer)

This repository's slice of the fleet plan: Phase 2. The plan, its review
and its recipe are canonical in adr-lint (pilot: fredericrous/adr-lint#53).

## Code review

- relais: backend, platform

## Outcome

- input: release.yaml "git-attest 1.7.0" step script, GH_TOKEN from `gh auth token` → expected: fetched through gh, `sha256sum -c` OK, extracted → actual: as expected.
- input: `yq '.jobs[].steps[] | select(.name=="git-attest 1.7.0") | .env.GH_TOKEN' .github/workflows/release.yaml` at this tree → expected: `${{ github.token }}` (the step carries its own token; round 1 found it missing, from a transformer bug since fixed) → actual: `${{ github.token }}`.
- input: grep of release.yaml for the web route → expected: none → actual: none. `ci.yaml` is left to `fix/plugin-spawn-cwd`, which another session owns.
- To observe: the first tag's `platforms` job log, `gh release download` then `sha256sum: OK`.

## Implementation review

- relais implementation-review: approve-with-changes (blocking: GH_TOKEN missing on the step, fixed; high: a check that sees the step env, added; medium: outcome, done here), then approve-with-changes on the Delta, record only; 69k+25k, 44+55 s (opus).
- relais backend: approve-with-changes (blocking, the same GH_TOKEN, fixed), then approve; 51k+25k, 31+81 s (opus).
- relais platform: approve-with-changes (blocking, the same GH_TOKEN, fixed), then approve; 52k+30k, 33+61 s (opus).
