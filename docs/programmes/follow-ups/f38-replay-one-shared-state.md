+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# relais replay builds one SharedState per run

## Question

Filed from the relais inbox (2026-11-24, session f138271b). `relais replay` builds three `SharedState` instances per run (`load_machine`, `ctx::shared_state()`, `load_price_table` in `cli/replay.rs:302-385`). Build one and pass it, as an-t22 did for `relais run`.

## Look at

- `crates/relais/src/cli/replay.rs` around lines 302-385.
- an-t22's change to `relais run`.

## Answer looks like

replay builds one `SharedState` and passes it down, and a test counts one construction per run.
