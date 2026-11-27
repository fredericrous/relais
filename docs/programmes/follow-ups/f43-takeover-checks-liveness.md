+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# A handoff takeover checks the holder is alive

## Question

Filed from the relais inbox (2026-11-24, session a8957506; greenbelay t02). Before it takes a handoff claim over, relais should check the holder's liveness (`greenbelay owner show --ticket … --json` state): a cleared holder is not the session that did the work.

## Look at

- relais claim takeover of a `handoff` claim.
- `greenbelay owner show --json`.

## Answer looks like

A takeover reads the holder's state and acts on a cleared or live holder as decided, with a test, and keeps today's rule without greenbelay.
