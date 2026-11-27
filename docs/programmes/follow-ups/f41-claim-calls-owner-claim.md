+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# programme claim records a greenbelay owner claim

## Question

Filed from the relais inbox (2026-11-24, session a8957506). `relais programme claim` should call `greenbelay owner claim --ticket <repo>/<prog>/<id>` (greenbelay 6.13.0, record v=1), so greenbelay can delete its interim PostToolUse observer of `relais programme claim` (the holds-until in greenbelay `src/hook.rs`).

## Look at

- relais `programme claim`.
- greenbelay `src/hook.rs` holds-until, and `greenbelay owner claim --help`.

## Answer looks like

claim writes the owner record when greenbelay is present (and does nothing when it is absent: every Green Harness part is optional), with a test.
