---
canonical: decisions:docs/plans/2026-10-25-relais-tests-follow-user-flows.md
phases: [2, 3, 4, 5, 6, 7]
status: done
---
# relais tests follow user flows (pointer)

The plan lives in the decisions repository, which owns the testing rules it
applies (ADR-0040). This repository carries phases [2, 3, 4, 5, 6, 7].

## Implementation review

- Phase 2: approve after a Delta.
  - Round 1, approve-with-changes (49k, 50 s). Delta, approve (52k, 18 s).
  - Fixed: the worker also backdates the cloned `dep/index.js`; the `relais.toml` note now dates its figure.
  - deliberate: the timings are recorded in the PR body, and the worker backdating the task tree is noted in Phase 8's decision log, because the canonical plan lives in decisions.
  - Still open: Windows is proven by the CI test matrix, and the full-suite before figure was not measured under the same load.
- Phase 3 (move): approve-with-changes after a Delta.
  - Round 1, approve-with-changes (70k, 137 s). Delta, approve-with-changes (74k, 22 s).
  - Fixed:
    - `WorldPermit::acquire` waits at most 600 s, then panics with the counts.
    - The README paths name `tests/it/`.
    - The measurements are recorded in the PR body: a peak of 4 live worlds, rebuild and relink times, nextest and `cargo test`.
  - deliberate, all moved to the Phase 7 PR:
    - The doc comment and panic wording on `acquire`.
    - The harness-test fold, which waits for Phase 4.
    - The unexplained nextest slowdown in the library's tests (code Phase 3 does not change).
- Phase 4 (check, part 1: the script and the workflow): approve after a Delta.
  - Round 1, approve-with-changes (75k, 125 s). Delta, approve (81k, 21 s).
  - Fixed:
    - A base timeout counts as a detection.
    - A manifest with helper entries but no test entry is refused.
    - Test entries and the deleted-test cover both match by exact module path.
    - The listing uses awk, BSD-safe.
    - A fixture covers the ambiguous outcome.
  - deliberate: entries are not checked to belong to the right package, because relais is one crate.
  - Part 2 is the proof dispatches, after this merges, because `workflow_dispatch` needs the file on main.
- Close-out: the plan is done (Phase 8 in decisions#99). Phases 5–7 were narrowed by the person to relais#312. This PR carries the one Phase 7 item that remained: the wording of `WorldPermit::acquire`'s doc comment and panic message.
