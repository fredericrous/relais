+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

2026-11-24. The takeover rule was already right: a live `work` claim
refuses (exit 3), and a `handoff` is meant to be taken over. On 11-21 the
live session held no claim at all. The claim log and transcripts show
943bdf5e reserved t19 as a handoff at 05:21, and its relayed successor
1c6146ea started at 05:22 and never ran `claim`. The handoff lapsed at
07:21 (R = 2 h) while 1c6146ea worked t19 in another repository's
worktree with no `prog/` branch. At 08:36, 22c6b0fc's `next --claim`
reserved the now unheld t19, and its relay 0f2c38ac took it over.

So a live holder is now told from a dead one by its session, not by a
timer. The relayed session holds the ticket as `work` from its first
read, and that claim ends at its own `SessionEnd` (ADR-0083):

- `relais programme adopt --previous <old>` takes every live handoff of
  the session a `/clear` ended over as this session's `work`, through the
  same path as `claim`.
- The relais plugin runs it at the successor's first read, using the exact
  id it already records at `SessionEnd(reason=clear)`. It retries on later
  reads until the adopt succeeds, for up to 2 h, and the pane shows the
  claim command meanwhile.
- `programme_claims.rs` replays 11-21: after adopt and 3 h, another
  session's `next --claim` takes t20 and a direct `claim` on t19 exits 3.

Not built: the board clause. It would not have caught 11-21 (another
repository, a branch naming no ticket), and relais reading greenbelay
crosses a tool boundary. Accepted gap: a live session that was not relayed
and never claims. Plan:
[2026-11-24-relayed-session-holds-its-ticket](../../plans/2026-11-24-relayed-session-holds-its-ticket.md).
Follow-up: [f16](f16-adopt-week-count.md), a week of relays after the
plugin reinstall.
