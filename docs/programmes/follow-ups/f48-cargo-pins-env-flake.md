+++
type = "task"
mode = "afk"
status = "open"
blocked_by = []
repos = ["relais"]
+++
# cargo_pins_the_host_slot_for_tests fails when run with its heavy siblings

## Question

Found while chasing the setup-slot flake (#645). `verify::heavy::tests::cargo_pins_the_host_slot_for_tests` failed in 2 of 2 local runs of the test binary filtered to `verify::heavy`, at `tests.rs:1090`: `GREENHARNESS_HOST_SLOT` read `Err(NotPresent)` where `Ok("held")` was expected. It passes in CI and in the `verify:: procs:: runner::` subset. Does a sibling test set or remove that environment variable in the process while this one reads it, and should the test read the child command's environment rather than the process's?

## Look at

- `crates/relais/src/verify/heavy/tests.rs` around line 1090: `cargo_pins_the_host_slot_for_tests`.
- Every `std::env::set_var` or `remove_var` of `GREENHARNESS_HOST_SLOT` in `crates/relais/src` (tests included).

## Answer looks like

A repro (the `verify::heavy` filter, run through `greengate slot run`) that goes red, then a fix with the test reading the spawned command's env (or serialised with its writers); or a Resolution that says why it cannot reproduce.
