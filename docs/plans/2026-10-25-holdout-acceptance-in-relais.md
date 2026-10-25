---
canonical: decisions:docs/plans/2026-10-25-holdout-acceptance-in-relais.md
phases: [2, 3, 4, 5]
status: active
---
# Holdout acceptance in relais (pointer)

The plan lives in the decisions repository, which owns ADR-0045
(`work.holdout-acceptance`). This repository carries phases 2–5.

## Decision log

- 2026-10-25 — Phase 2 is parse and preflight only. Successor inheritance
  of the frozen definition (resume, repair, `--widen-scope`, `review-parts`,
  `--rerun-verify`) moves to phase 3, because only phase 3 runs a holdout
  and the successor paths are part of verification. Phase 2 records the
  definition digest in the manifest so that phase 3 can bind to it.
- 2026-10-25 — Phase 3 also owns two things the canonical plan lists in
  phase 2's text: the holdout command's dependency files joining the
  verification inputs, and protecting the overlay path in the scope
  check. Phase 2 neither fetches nor runs a holdout, so neither has
  anything to guard yet.
- 2026-10-25 — `docs/SPEC.md` §4 describes parse and preflight only, and
  says so: "This relais does not fetch or run a holdout".

## Implementation review

- Phase 2: approve-with-changes (82k tokens, 69 s). relais verified the candidate first (run-65eb09e231ab9-11e50, accepted, its reviewer passed).
  - Fixed: the Verification row now holds what was observed. A refusal exits 3 (`CliOutcome::Blocked`, `cli/outcome.rs:115`), not the 2 the plan expected; the manifest leaves the overlay out by design. Phase 2 is ticked in the canonical plan.
  - deliberate: when the repository name is missing, preflight refuses as `holdout_unknown (declared: none)`. That fails closed; phase 3 gives it a clearer detail.
  - Candidate `functions.no-flag-arguments` at `runner/tests/holdouts.rs:39`: fine, because the bool is data passed into `DeclaredCriterion.mandatory`.
- Delta after the Windows CI fix (`store` written as a TOML literal string): approve (88k tokens, 35 s). No new finding. The missing-repository-name detail is still deliberate and goes to phase 3.
