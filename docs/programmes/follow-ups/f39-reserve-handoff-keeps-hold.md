+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# reserve_handoff keeps a hold its claim file still keeps

## Question

Filed from the relais inbox (2026-11-24, session f138271b, an-t23 follow-up). `node::reserve_handoff` gives the shared lease back when the claim file refuses after a `Granted`. On a primary that re-grants its own holder (the an-t23 rule), this drops a hold the claim file still keeps. It is unreachable today, because callers ask only for unheld tickets under the `SlugLock`. Should it keep the shared lease when the claim file already holds a handoff of the same session, or assert the invariant?

## Look at

- `crates/relais/src/node/` `reserve_handoff`.
- an-t23's re-grant rule.

## Answer looks like

Either the lease is kept in that case, with a test, or a debug assertion of the invariant, with its reason.
