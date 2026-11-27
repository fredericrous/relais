+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["amont", "relais"]
+++
# relais waits for a host slot through greengate's one wait loop

## Question

t03 found relais's `verify/heavy.rs` re-implements
`greenharness_slots::wait_in_line` (about 80 lines), because it needs to be
cancelled and to heartbeat between polls, and `wait_in_line` takes only
`WaitBounds` and a read-only `on_wait(&Queue)` callback. Add a cancel seam to
`wait_in_line` in greengate, switch relais's loop to it, and replace
`procs::load_average` with `greenharness_slots::load1` (about 25 lines).

## Look at

- amont `crates/greenharness-slots/src/lib.rs`: `wait_in_line` (~954),
  `wait_in_line_with` (~973), `load1` (~278); `adr/0022-waits-end-on-a-stalled-line.md`
  (the one loop every caller uses).
- relais `crates/relais/src/verify/heavy.rs` slot-acquire loop (~1203-1330);
  `procs::load_average` and its callers in `verify/mod.rs` and `cli/replay.rs`.
- [t03](t03-slot-overlap.md)'s Resolution.

## Answer looks like

Two pull requests: greengate's slots crate gains a way for a caller to end
the wait (for example `on_wait` returning a continue/stop answer) with a
test, released; then relais links that release, its own loop and
`load_average` are gone, and its slot-wait tests still pass. The line count
removed from relais, measured.
