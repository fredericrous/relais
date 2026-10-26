# relais seeds its own tree: full reviews

Plan: [2026-10-26-relais-seeds-its-own-tree.md](2026-10-26-relais-seeds-its-own-tree.md)

## Revision (trust the end signal)

- **backend (opus-5-5).** Rounds 1–3 on the first draft (a 3 s window): see the first draft's reviews sidecar. Delta round on this revision, approve-with-changes (29k, 42 s). Carried into implementation:
  - Name the setup-gap event, count its baseline over the recorded runs, and set the Phase 3 trigger as a threshold. If no event carries the gap, emit one.
  - Test the caller seed at the `seed_tree` level, with a short `Quiet` and a recording sleeper, not through the runner (where it would sleep for real).
  - Rollback is a release that turns `WritersEnded` back into `QuietFor(CALLER)` at the two call sites.
  - Phase 3 splits the `workspace_seed_unwaited` count by in-place vs fork.
- **codex (gpt-6-astra), first draft.** The high finding, that a bad seed can block verification, is kept in Decisions. The medium findings, best-effort wording and positive seed evidence, are folded into this revision.

## First draft (a 3 s window, superseded)


- **backend (opus-5-5).**
  - Round 1, approve-with-changes (29k, 60 s): pick the window by caller, not by path (blocking); measure the depth-2 gap first; count the wait locations; assert on decisions, not time; a before figure. All applied.
  - Round 2, approve-with-changes (24k, 55 s). Carried:
    - The Phase 2 file list needs the four caller files (`verification.rs`, `holdouts.rs`, `baseline.rs`, `engine.rs`).
    - Drop "takes under 10 s" from the test bullet.
    - The heading should say "chosen by the call site".
  - Re-run after Codex, approve-with-changes (22k). Carried:
    - Put the partial-seed release gate into Phase 2's checklist and Verification.
    - A setup-gap baseline. A first count found no `setup_gap` event in 373 recorded verification starts; confirm the event name.
- **codex (gpt-6-astra).** Approve-with-changes.
  - Applied (high): a bad seed can turn into a verification gap with no retry, so partial-seed tests gate the release and Phase 3 tracks setup gaps.
  - Carried (medium): describe the detection as best-effort, and calibrate including the age shortcut.
  - Carried (medium): the no-wait assertion also needs `workspace_seeded` and the contents seeded.
