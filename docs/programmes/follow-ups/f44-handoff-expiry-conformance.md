+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# Conformance pins the handoff hold's expiry on a retry

## Question

Filed from the relais inbox (2026-11-25, session f138271b, an-t23 follow-up). Add a handoff expiry case to `conformance::run_all_with_clock`: reserve as me, advance past `HANDOFF_HOLD_SECS` from the first grant but not from my retry, then `other` must still get `Held`. The trait doc says a primary should move the hold end on a retry, and no check holds a client to it.

## Look at

- `crates/relais/src/` conformance `run_all_with_clock` and `HANDOFF_HOLD_SECS`.
- The shared-state trait doc for handoff holds.

## Answer looks like

The case added to the conformance suite and passing on every client it runs against.
