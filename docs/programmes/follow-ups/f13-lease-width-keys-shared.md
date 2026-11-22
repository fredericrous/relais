+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# The lease's width variables come from one shared list

## Question

`crates/relais/src/verify/heavy.rs` (about line 361) lists the width keys
by hand: `MAKEFLAGS`, `CARGO_BUILD_JOBS`, `RUST_TEST_THREADS`,
`NEXTEST_TEST_THREADS`. greengate also sets `VITEST_MAX_WORKERS` and
`GOMAXPROCS` (amont `fix/slot-bounds-vitest`, f115). The two lists will
drift.

## Look at

- `crates/relais/src/verify/heavy.rs`, the lease env.
- greenharness-slots, whether `width_env` has shipped.

## Answer looks like

Once greenharness-slots ships `width_env`, relais builds the lease env
from `greenharness_slots::width_env`, and a test pins that the two key
sets match. Until then the ticket waits.
