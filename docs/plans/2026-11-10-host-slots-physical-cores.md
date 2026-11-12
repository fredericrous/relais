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
- `HeavyAdmission` carries a `Machine` (the cores the slots count) beside the coordinator `cap`. A command under a host slot (its own or an ancestor's) runs `parallelism(weight, machine.cores, sizing.cap_for(machine.cores))` wide, with the crate's own `parallelism` (relais's copy is deleted and re-exported), so a weight means the same width whoever holds it; a command queued nowhere (slots off, or a refused directory) keeps the coordinator's share. One pure rule, `verify::host_share`, picks the cap for the admission and the doctor alike. Before this, relais divided its logical count by its own coordinator cap: 4 wide on a 6-core, 12-thread laptop where greengate runs 2.
- The slot sizing and the core count come from `greenharness_slots::host::from_host(detect_cores())`, published in `greenharness-slots` 4.0.0 (the crate relais already links: its registry source carries `host::`, `Machine`, `new_in_on`, `detect_cores` and `WaitContext.cores`). Precedence: `GREENGATE_HOST_SLOTS` (an environment variable outranks a file), then relais's own `[resources] host_slots`, deprecated but still winning over git config, then `greengate.hostSlots` (`--global`, then `--system`), then detection. The doctor says which value applies. `greengate.hostSlots 0`, `GREENGATE_HOST_SLOTS=0` or `host_slots = 0` turns the host slots off for relais, and the run's note and the doctor name which one did. The mapping to the run's queue is the pure `runner::engine::host_queue`.
- Wait lines are written with `record_wait_named`: the width the command got (`jobs`), `cap` and `cores`, so a relais line and a greengate line for the same slot read alike.
- The doctor prints the width on a line of its own, in words and within 80 columns (`a weight-1 command runs 2 jobs at a time: 6 cores shared by 3 host slots`), the sizing warnings on one line with their `Fix:`, a legacy `amont.hostCores` / `amont.hostSlots` in `--global` config as ignored with the `greengate migrate --global` fix, and a `--global` config git cannot read as a warning (exit 1, no key, is no finding).
- Standalone stays (ADR-0047): relais links the crate, never the binary. With no greengate, no keys and no working `git`, `from_host` returns the detected sizing, which is relais's behaviour with defaults.

The interim `host_slots = 3` the canonical plan named is not needed: relais 0.56.0 (#448) already joined the `greenharness-slots` queue, which no old build shares.

## Verification

Unit tests (`cargo test -p relais --lib -- verify:: doctor:: runner::engine`, 222 pass; clippy `-D warnings` clean), each input → expected → actual:
- 8 cores, 4 fixed slots, coordinator cap 2, a weight-1 command granted a host slot → `CARGO_BUILD_JOBS=2` and a wait line `"jobs":2,"cap":4,"cores":8` → as expected (`a_queued_command_gets_the_slots_share_and_its_wait_line_names_it`).
- The same with a refused 0755 directory → `CARGO_BUILD_JOBS=4`, the coordinator's share → 4 (`a_refused_directory_runs_at_the_coordinators_width`).
- No host queue → 4 → 4 (`with_no_host_queue_the_coordinators_cap_shares_the_cores`).
- `GREENGATE_HOST_SLOTS=5` with `host_slots = 2` → Fixed(5) → Fixed(5); `greengate.hostSlots 0` → off, named `greengate.hostSlots 0 (git config --global)` → as expected; `GREENGATE_HOST_SLOTS=0` → `GREENGATE_HOST_SLOTS=0`; `host_slots = 0` → `[resources] host_slots = 0` (`the_plan_names_the_setting_that_turned_the_host_slots_off`).
- `host_queue` with the plan off → no directory, whatever `shared_host_slots` says → as expected (`the_host_queue_follows_the_plan_and_the_shared_setting`).
- git's answer for the legacy keys: 0 → rows, 1 → none, 3 → a warning naming the exit and git's stderr (`the_legacy_read_tells_no_keys_from_a_failed_git`).

The branch binary's `relais doctor`, run with HOME, `RELAIS_CONFIG_DIR`, `RELAIS_STATE_DIR`, `XDG_CONFIG_HOME`, `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM=1` and the cwd all scratch, from `env -i`, on this i7-9750H (6 physical, 12 logical); a listing of the live relais state was identical before and after:
| input | expected | actual |
|---|---|---|
| no keys | 3 slots, 2 wide | `a weight-1 command runs 2 jobs at a time: 6 cores shared by 3 host slots` |
| `GREENGATE_HOST_CORES=4` | 2 slots, 2 wide | `… 2 jobs at a time: 4 cores shared by 2 host slots` |
| `host_slots = 2`, `GREENGATE_HOST_SLOTS=5` | 5 fixed, the variable wins | `5 fixed`; `host_slots = 2 is deprecated (GREENGATE_HOST_SLOTS wins over it): relais uses 5, greengate uses 5` |
| `greengate.hostSlots 0` | off, named, coordinator width | `no shared host slots: greengate.hostSlots 0 (git config --global) turns them off`; `6 cores shared by 3 coordinator tokens` |
| `amont.hostSlots 3` | ignored, fix named | `amont.hostSlots = 3 in git config --global is ignored: … Fix: greengate migrate --global` |
| no `git` on PATH | detected sizing, one warning | `6 cores shared by 3 host slots`; `git cannot list config with scopes … Fix: check that git runs` |

## Implementation review

Filled at the close of the branch.
