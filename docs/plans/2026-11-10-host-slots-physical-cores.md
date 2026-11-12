---
canonical: greengate:docs/plans/2026-11-10-host-slots-physical-cores.md
phases: [4]
status: active
---
Part of [host slots count physical cores](https://github.com/greenharness/greengate/blob/main/docs/plans/2026-11-10-host-slots-physical-cores.md) (greengate ADR-0021): greengate took Phases 1 to 3 (#459, #460, release 4.0.0, `greenharness-slots` 4.0.0 on crates.io). This repository takes Phase 4: relais sizes the host queue from the same physical core count and the same two keys as greengate, with nothing copied by hand.

## Code review
- relais: backend, lang:rust, unix, tui, architect

## What Phase 4 changes here

- `verify::cores()` becomes `logical_cores()` and keeps feeding only `heavy_cap` and `default_heavy_commands` (`runner/engine.rs`, `runner/preflight.rs`, `coordinator/mod.rs`, `doctor/mod.rs`): the coordinator cap stays a quarter of the logical cores (relais ADR-0010 unchanged), so preflight does not start refusing weight-2 commands.
- `HeavyAdmission` carries a `Machine` (the cores the slots count) beside the coordinator `cap`. A command's width is `parallelism(weight, machine.cores, sizing.cap_for(machine.cores))`, the formula greengate uses, so a weight means the same width whoever holds it. Today relais divides its logical count by its own coordinator cap: 4 wide on a 6-core, 12-thread laptop where greengate runs 2.
- The slot sizing and the core count come from `greenharness_slots::host::from_host(detect_cores())`: `GREENGATE_HOST_CORES` / `GREENGATE_HOST_SLOTS`, then `greengate.hostCores` / `greengate.hostSlots` (`--global`, then `--system`), then detection. relais's own `[resources] host_slots` stays as a deprecated override that still wins; the doctor says so and names the mismatch. `greengate.hostSlots 0` turns the host slots off for relais too, as it does for greengate.
- Wait lines are written with `record_wait_named`: the width the command got (`jobs`), `cap` and `cores`, so a relais line and a greengate line for the same slot read alike.
- The doctor prints `heavy: width W (N cores / slots cap C), coordinator cap K`, the sizing warnings with their `Fix:` lines, and a legacy `amont.hostCores` / `amont.hostSlots` in `--global` config as ignored, with the `greengate migrate --global` fix.
- Standalone stays (ADR-0047): relais links the crate, never the binary. With no greengate, no keys and no working `git`, `from_host` returns the detected sizing, which is relais's behaviour with defaults.

The interim `host_slots = 3` the canonical plan named is not needed: relais 0.56.0 (#448) already joined the `greenharness-slots` queue, which no old build shares.

## Verification

Filled at the close of the branch: input, expected, actual for each change above.

## Implementation review

Filled at the close of the branch.
