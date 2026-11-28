+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

Done in two pull requests, with the plan
[`docs/plans/2026-11-27-relais-waits-through-greengate-loop.md`](../../plans/2026-11-27-relais-waits-through-greengate-loop.md).

- **greengate** ([greenharness/greengate#538](https://github.com/greenharness/greengate/pull/538),
  released in **5.5.0**, [#540](https://github.com/greenharness/greengate/pull/540)).
  `greenharness-slots` gains `wait_in_line_paced(waiter, bounds, on_wait, pause)`.
  It runs the same ADR-0022 loop, but the caller owns the pause between polls:
  `pause` gets `Pause::Waiting(&Queue)` or `Pause::Retrying { error, failures }`
  and answers `ControlFlow`, where `Break(b)` ends the wait as
  `Ended::Caller(b)`, and the line's own ends arrive as `Ended::Line(LineEnd)`.
  `wait_in_line` is that loop with a 200 ms sleep, unchanged for its callers.
  The design is a caller-owned pause, not the ticket's "`on_wait` returns
  continue or stop", because relais must also sleep through its own sleeper,
  see failed polls before it joins, and heartbeat on retries. Both enums are
  exhaustive (`types.exhaustive-matching`). The crates.io publish job
  concluded `success`.
- **relais** (branch `prog/relais-modularise/t05-wait-loop-cancel-seam`).
  `slot::take` waits through `wait_in_line_paced`, with
  `greenharness-slots = "5.5"` as the new minimum. relais keeps the pause:
  heartbeat, budget, cancel, announce and its sleeper. A lock that fails
  before the line is joined still turns the host slots off once, and the
  command runs unqueued. `procs::load_average` and its two platform arms are
  gone; their two callers (three call sites) read `greenharness_slots::load1`.
- **Lines removed, measured** (`git diff --numstat origin/main`):
  - `procs.rs`: −20.
  - `verify/heavy.rs`: +108/−100. The 80-line copy of the loop is gone, but
    mapping relais's ends (granted, unusable, stopped, stalled, unreadable,
    ceiling) back to its events and journal rows takes about as many lines.
  - Production code nets about −12, plus +89 lines of new tests (a cancel
    while queued; a lock that fails before joining). That is under the plan's
    20-line bar, so this reads as "one loop, little code saved". The gain is
    the single loop of ADR-0022, not size.
- **`Line(Ceiling)`**, which relais's `Duration::MAX` ceiling never reaches,
  maps to a new `SlotLineEnd::Ceiling`, so its event and its journal row both
  say "ceiling".
- **Tests:** `verify::heavy` passes 59 of 59 against the published crate,
  with the existing slot-wait tests unedited. The cancel test asserts one
  heartbeat per pause, and that the cancelled waiter leaves the line.
- **Not run:**
  - The plan's 360 s live pilot, a `relais run` behind a held slot. It was
    judged too costly for what it adds on this host (load ~19, 4 GB free):
    the heartbeat-per-pause assertion covers the seam this ticket changed,
    and the lease-expiry path is the coordinator's, which t05 did not touch.
  - `cargo check --target x86_64-pc-windows-msvc` stops in
    `libsqlite3-sys`'s C build script (no cross C compiler here). No
    `load_average` reference is left under any `cfg`.

