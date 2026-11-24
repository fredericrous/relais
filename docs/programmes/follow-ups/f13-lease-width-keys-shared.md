+++
type = "task"
mode = "afk"
status = "closed"
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

## Resolution

2026-11-24. `width_env` shipped in greenharness-slots 4.13.0, which relais
already locks. `child_environment` now builds the lease env from
`greenharness_slots::width_env(jobs, |_| None)`: every key in
`WIDTH_KEYS`, whatever relais's own environment holds, with the check's
own `env` the only override (as before). The test
`the_lease_bounds_exactly_greengates_width_keys` pins that the lease's
keys are exactly `WIDTH_KEYS`.

The published 4.13.0 still lists the four keys; `VITEST_MAX_WORKERS` and
`GOMAXPROCS` (amont #514) are on amont main, unreleased. They reach relais
with the next greenharness-slots bump and no relais change.
