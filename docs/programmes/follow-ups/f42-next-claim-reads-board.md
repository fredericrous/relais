+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# programme next --claim skips a ticket with a live owner

## Question

Filed from the relais inbox (2026-11-24, session a8957506; greenbelay t02, f107). `relais programme next --claim` should consult `greenbelay board --json` (`worktrees[].owner`, state live or cleared) before it reserves a ticket, so that a live owner on a non-`prog/` branch is not handed over.

## Look at

- relais `programme next`.
- `greenbelay board --json`'s owner fields.

## Answer looks like

next skips a ticket whose worktree has a live owner, with a test, and falls back to today's rule without greenbelay.
