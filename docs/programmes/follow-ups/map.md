# Follow-ups

## Destination

Every follow-up a session notices in relais, outside the task it works, is a
ticket here until it is resolved or moved into a programme of its own.

## Notes

- The standing follow-ups map of ADR-0077 (`work.programme-map`): inbox lines
  filed at a boundary land here when no programme owns them.

## Decisions so far

- [f01](f01-flaky-end-session-tombstone-test.md) (2026-11-20): the
  claim passes wait out a brief lock (30 ms). A concurrent fork's copy of a
  freed flock read as held (#508).
- [f02](f02-claims-record-common-dir.md) (2026-11-20): claims do not
  record the git common dir. Their `repo` is already the main checkout
  (#430), which a worktree removal leaves in place; a test pins it.

## Handoff

2026-11-20, boundary 3 (a ticket closed, its PR merged): f02 closed,
relais#509 merged (ad713f89).

- Done: f02 by hand (no relais run). The answer was "no new field": the
  premise did not hold. Its test
  (`a_claim_from_a_removed_worktree_still_finds_the_map`) passed on
  unchanged main.
- Live state: relais main ad713f89, live checkout fast-forwarded. The f02
  worktree (`relais-wt-claims-common-dir`) is removed. No release is owed (test and docs only).
- Next: f03 (the pre-clear status hint misses a resize), the only ready
  ticket.
- Lessons:
  - Check a ticket's premise with a failing-first test before designing the
    change.
  - The plan's Decision log line came from a test that rewrites a claim's
    `repo` (`record_repo`), not from a real claim.
  - The push gate rehearsal for #509 took 27m10s (land output) under a
    busy slot queue: run `greengate land --detach`, then wait on it.
- Your steps: none.

## Not yet specified

None.

## Out of scope

None.
