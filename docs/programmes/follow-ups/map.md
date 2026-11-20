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
- [f03](f03-pane-placed-after-resize.md) (2026-11-20): no change. A
  narrowing resize moves a placed pane inline above the prompt, never
  hides it, and `$.ui.panes()` keeps `isPlaced` true; live in tmux on
  Claude Code 2.1.329.

## Handoff

2026-11-20, boundary 4 (a ticket closed): f03 closed in the same PR as
this handoff (docs only).

- Done: f03 by hand (no relais run). The premise did not reproduce: at 100
  columns the relais pane (programme window and `before /clear` tail) moved
  inline above the prompt and stayed drawn; a probe mod read
  `isPlaced: true` from `$.ui.panes()` through 180 → 100 → 180.
- Live state: no code changed, no release owed. Scratch probe, demo repo
  and tmux sessions removed.
- Next: no ready ticket; the map waits for inbox lines filed at a boundary.
- Lessons:
  - Reproduce a live observation in its exact state (here, a held
    programme plus a tail) before treating it as a bug.
  - A tmux repro of Claude Code needs `Down` then Enter on the trust dialog
    (its default is `No, exit`), and a typed prompt sometimes needs a second
    Enter.
  - A `/relais-status` in a test session is an asked open of id `relais`:
    the engine keeps it across sessions, so the unasked floor is 110, not
    144, until the pane is closed by hand.
- Your steps: none.

## Not yet specified

None.

## Out of scope

None.
