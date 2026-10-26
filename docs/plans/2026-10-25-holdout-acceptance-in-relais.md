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
- 2026-10-26 — Phase 3 is split into three relais tasks: 3a runs and
  settles holdouts (`docs/design/holdout-phase-3a.md`); 3b covers the
  overlay collision, the protected verification inputs and the repair and
  escalation feedback rule; 3c covers successor inheritance and the
  missing-repository-name detail. The Forgejo `holdouts` repository is
  created at the phase 5 pilot, its first use; phase 3 tests use local
  stores.

## Verification

Phase 3a, observed 2026-10-26 on `feat/holdout-run`
(`cargo test -p relais --lib holdout`: every test listed below ok; the pre-commit gate ran the full suite green).
Each test drives a full `RunEngine` run against a local bare-repo store.
- Pass: the script writes a passing JUnit report → expected `accepted`, `holdouts[0].outcome == passed`, test `h::escapes` → observed as expected, and the run's holdout dir is gone afterwards (`a_passing_holdout_settles_its_criterion_and_is_listed_on_the_receipt`).
- Fail: a failing test → expected criterion unmet, a gap naming only the criterion, `escaping` and `h::escapes` → observed as expected; the gap text holds no script line, store, path or overlay (`a_failing_holdout_test_leaves_the_criterion_unmet_without_leaking_the_holdout`).
- Skipped with exit 0 → expected not met → observed not met (`a_skipped_holdout_test_is_not_a_pass_even_on_exit_zero`).
- No report → expected `did_not_build` → observed (`a_holdout_that_writes_no_report_did_not_build`).
- Store deleted after preflight → expected `blocked (holdout_unfetchable)`, candidate ref kept → observed (`a_store_deleted_after_preflight_blocks_the_run_and_keeps_the_candidate`).
- `{config}` → expected the store's absolute config path in argv → observed (`the_config_placeholder_reaches_the_holdout_command_as_the_stores_config_path`).
- No holdout log among the profile's `CheckLog` evidence rows → observed (checked inside the pass test).
- Fetch once, reused across a two-attempt run; the dir is removed at the end → observed (`the_store_is_fetched_once_and_reused_across_attempts`).
- Sweep: the dir of an ended run is removed at the next run's start and by the `--retire` sweep, while the dirs of a live run and of a run unknown to the ledger are kept → observed (`ended_run_holdout_dirs_are_swept_at_run_start_and_live_or_unknown_ones_kept`). A no-op mutation of `sweep_ended` turns it red.
- Not shown by 3a, left to 3b: the repair and escalation inputs holding nothing of the holdout, and the overlay collision.

## Implementation review (3a)

- Round 1: approve-with-changes (109k tokens, 112 s).
  - The blocking finding was the missing Verification record, written above.
  - Fixed the low finding: an empty failing list now reads "did not exit 0 (a non-zero exit, a timeout, a signal or a cancel)".
  - Candidates: `f` in `Display::fmt` is fine, and so is `_ =>` over a tuple.
- Delta: approve (113k tokens, 16 s). Carried to 3b: keep the kept `log.txt`/`report.xml` out of reach of `read_hints` and successor context; check the fetch-by-ref fallback against the real Forgejo store at the phase 5 pilot.
- Windows CI fixes after the delta. `{config}` is now rendered per platform through a `PathStyle` sum type: Unix keeps the path as is, Windows strips canonicalize's `\\?\` prefix and uses `/`.
  - The first fix took a `windows: bool`, which I had asked for in the contract. The review flagged it under `functions.no-flag-arguments`, and it was replaced.
  - Fresh round 1 on the final tree: approve (33k tokens, 47 s).
