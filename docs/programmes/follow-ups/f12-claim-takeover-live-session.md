+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# A work claim held by a live session is never taken over

## Question

On 2026-11-21 a `/clear` relay resumed blue-factory t19 in a new session.
`relais programme claim` took the work claim over from 22c6b0fc while the
session carrying t19 (1c6146ea, worktree `greenbelay-wt-fingerprint`) was
still live and editing. Two sessions then worked the same worktree, and
one commit ended mid-gate. How should a claim tell a live holder from a
dead one, and refuse to take over from a live one?

## Look at

- `crates/relais/src/claim/` (the takeover rule and its liveness test).
- `greenbelay board` (live sessions per worktree and branch), which a
  claim could consult.
- The clear-relay hand-off: which session id the new claim carries.

## Answer looks like

A claim held by a live session refuses a takeover. So does a ticket whose
branch or worktree shows a live session on the board. A test reproduces
the 11-21 sequence: a live holder, then a relayed claim.
