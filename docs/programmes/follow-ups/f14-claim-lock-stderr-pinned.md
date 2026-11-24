+++
type = "task"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# The claim lock-timeout wording is pinned by a test

## Question

The programme driver retries a claim's exit 1 only when stderr says
`stayed held for` (`claim/store.rs`, about line 77). If the message is
reworded, every lock wait becomes a stop (relais backend and Rust review,
ADR-0080).

## Look at

- `crates/relais/src/claim/store.rs`, the lock-timeout message.
- The driver's retry match (the programme skill's Drive section).

## Answer looks like

A claim CLI test that triggers a lock timeout and asserts that stderr
contains `stayed held for`.

## Resolution

Closed on 2026-11-24 by #566. The claim CLI lock-timeout test now asserts
that stderr holds `stayed held for`, the wording the driver's retry
matches. (This Resolution was added with f15. #566 closed the ticket
without one, and `relais programme check` refused it.)
