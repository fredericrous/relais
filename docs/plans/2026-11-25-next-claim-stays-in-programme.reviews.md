# Full reviews: Next claim stays in its programme

## Round 1 (body f8336b46fb6b, 1029 words)

- **backend** (opus-5-5, 34k, 50 s): approve-with-changes.
  1 high: a busy lock as exit 1 would stop the relay; retry once, then
  park. Accepted.
  2 medium: list only programmes in which nothing is held. Accepted.
  3 low: say what happens to an own handoff in another programme. Accepted.
  4 low: add a `--claim` pilot. Accepted.
  5 low: draw the failed-load row. Accepted.
- **lang:rust** (opus-5-5, 46k, 66 s): approve-with-changes.
  1 high: name the three tests that change. Accepted.
  2 high: test Behaviour 5. Accepted.
  3 medium: extend the pin phrases. Accepted.
  4 low: implementer note (delete the `LockTimeout => {}` arm, fix the doc comment). No edit.
  5 low: own handoff elsewhere. Covered by backend 3.
- **tui** (opus-5-5, 29k, 30 s): approve-with-changes.
  1 medium: `<repo>:<slug>` so a line pastes into `--from`. Accepted.
  2 medium: test Behaviour 5. Accepted.
  3 low: test the line order. Accepted.
- **unix** (opus-5-5, 41k, 48 s): approve-with-changes.
  1 medium: `elsewhere_partial` in JSON. Accepted.
  2 low: exit 1 means report and stop. Rejected in favour of backend 1.
  3 low: assert stream separation. Accepted.

## Round 2 (body 0e563e0a1a1a, 1230 words)

All four approve. Carried into implementation:

- lang:rust 6 (low): rename the failed-load test to say what it now asserts.
- tui (low): the lock-busy park names the retry command (above the line).
- backend 6 (low): Phase 3 also greps for the new text.
- backend 7 (low): the ADR names `parked: lock busy` (done in ADR-0084).
- unix (low): the busy-lock test asserts `stayed held for` on stderr.

## Codex (gpt-6-astra, high effort)

approve-with-changes.
1 high: Work step 6 still demands a `next:` line unconditionally
(`SKILL.md:189-198`). Applied, as one Skill text bullet.
2 medium: an error reading another programme's claims while listing must
be partial discovery, not a failure (`claim.rs:166-168`). Carried into
implementation.

## Delta on the Codex edit (body 009568a3d007, 1275 words)

backend approve, tui approve, unix approve, lang:rust approve-with-changes.
Carried: pin one phrase that appears only in Work step 6 (rust 7, medium;
backend and tui the same, low).
