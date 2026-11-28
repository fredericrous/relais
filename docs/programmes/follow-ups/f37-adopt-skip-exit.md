+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Whether programme adopt exits 1 when it skips a ticket

## Question

Filed from the relais inbox (2026-11-24, session bf3a0ae7, found by the an-t18 agent). `relais programme adopt` (#561) exits 0 when it skips a ticket whose repository is unreachable or belongs to another repository. A relayed session then reads success while its ticket stays unheld. Should that skip exit 1?

## Look at

- `crates/relais/src/programme/` adopt (#561) and its exit codes.
- The clear-relay's handling of the adopt exit.

## Answer looks like

A decision (exit 1, or 0 with a named stderr line the relay reads), a test pinning it, and the relay's reading of it.
