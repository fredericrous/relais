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
