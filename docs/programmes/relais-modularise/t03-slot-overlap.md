+++
type = "research"
mode = "afk"
status = "closed"
blocked_by = []
repos = ["relais"]
+++
# Whether relais's host scheduling duplicates greengate's slot queue

## Question

Do `admission`, `coordinator` and `procs` in relais duplicate greengate's
(was amont) host slot queue (ADR-0014), and can one of them go?

## Look at

- `crates/relais/src/admission/mod.rs` (6.4k lines), `admission/native.rs`,
  `crates/relais/src/coordinator/mod.rs` (5.3k lines) and
  `crates/relais/src/procs.rs`.
- `greengate slots --json` and its documentation in the greengate repository
  (`amont`'s `docs/`), and decisions ADR-0014 (one host slot queue) and
  ADR-0013 (a busy host defers).
- The memory notes `amont_one_host_slot_queue_adr_0014` and
  `relais_settle_timeouts_adr_0012`, for what is already decided.

## Answer looks like

A table with one row per relais type or function that admits, queues, waits
or reaps: what it decides, the greengate surface that decides the same thing
(or none), and whether it is removed, kept, or moved. Then one line: what
relais still has to own after the overlap is gone, and how many lines that
removes from the crate.

## Resolution

Closed 2026-11-26. Read-only research (relais run `run-66128f3f91a09-873c`,
at `0d1b8c4db`), then checked by hand against the greengate repository.

**Short answer:** relais does not run its own host-slot queue. Its heavy
verification path already takes host slots through greengate's
`greenharness-slots` crate (`verify/heavy.rs`), as ADR-0014 decided. The
overlap is a hand-copied wait loop and a load-average helper, plus a
coordinator token queue for `HeavyLocal` work that stays for now. Neither
`admission`, `coordinator` nor `procs` can go as a module.

**Where the decisions live.** ADR-0013 and ADR-0014 are in the greengate
repository's own corpus (`adr/0013-a-busy-host-defers-to-ci.md`,
`adr/0014-every-heavy-job-shares-one-queue.md`), not in `decisions`, where
those numbers are other records. ADR-0014 replaces ADR-0013 for key
`hooks.host-concurrency`. ADR-0022 (waits end on a stalled line) governs the
wait loop. Line counts below are estimates from symbol ranges, not measured.

| relais type or function | What it decides | greengate surface | Verdict |
|---|---|---|---|
| `admission::AdmissionState::admit`, `admissible`, `drain`, `Queued` | Per-class caps and a FIFO queue for coordinator tokens | `greenharness_slots::Waiter` queue, host slots only | Kept for model, indexing, training and remote classes. For `HeavyLocal` it duplicates the slot queue when slots are on, and is the only queue when slots are off (`CoordinatorOnly`). Collapse only after the slots-off case is designed. |
| `AdmissionState::disk_hold`, `set_disk`, `volumes_for`, `DiskHold`, `Decision::WaitingForDisk` | Holds a lease until the volumes stay above the disk floor | None | Kept |
| `AdmissionState::request_training`, `end_training`, `live_agents_outside`, `TrainingDecision` | Defers training while other sessions' agents are live | None; ADR-0013's busy defer covers hook checks only | Kept |
| `AdmissionState::reap_unclaimed`, `reconcile`, `reconcile_groups`, `escalate`, `cancel_*` | Reaps dead dispatches; terminate then kill | None; slot holders are close-on-exec and rely on dead-pid reclaim | Kept |
| `AdmissionState::bind`, `bind_group`, `heartbeat`, `lease_is_stale` | Lease liveness and process-group binding | Dead-holder check in `greenharness_slots`, same idea, different record | Kept |
| `admission::native`, `admission::orphan` | Plugin-spawned native agents; orphaned worktree fences | None | Kept |
| `coordinator::Coordinator`, `elect`, `serve`, `reconcile_loop`, `group_loop`, `GroupStore`, `Client`, `RemoteGate` | One daemon owning admission state over a socket | None (greengate has no daemon) | Kept |
| `coordinator::seats` | Serialises admission answers | None | Not read; assumed kept |
| `verify/heavy.rs` slot-acquire loop (`Waiter::poll`, `Stall`, failure count) | Waits in the host-slot line until granted, stalled or unreadable | `greenharness_slots::wait_in_line`, "the one loop every caller uses" (ADR-0022) | **Moved**, about 80 lines. relais keeps its own loop because it needs cancellation and a heartbeat between polls. `wait_in_line` takes `WaitBounds` and a read-only `on_wait(&Queue)` callback, so it has no cancel seam: greengate gains one first, then relais calls it. |
| `verify/heavy.rs` `HostSlots`, `HostSlotsOff`, `plan` | Resolves cores, sizing and the off reasons | `greenharness_slots::host::resolve`, `Sizing`, `Machine` | Kept as a thin adapter |
| `procs::load_average` (three platform impls) | One-minute load average | `greenharness_slots::load1` | **Removed**, about 25 lines. Callers: `verify/mod.rs` (two) and `cli/replay.rs`. |
| `procs::LockFile` | Exclusive file lock with a pid file | `greenharness_slots::lock_within` | Kept: coordinator election, trust and prune use it, and none is a host-slot path |
| `procs::kill_group`, `stop_group`, `sample_group_cpu`, `group_cpu_time` | Signals a process group; samples CPU | None | Kept |

**What relais still owns once the overlap is gone:** the coordinator's token
classes for non-slot work (model, indexing, training, remote), the disk
floor, the training defer, native agents and orphan fences, process-group
reaping and signalling, and the daemon transport. The overlap removes about
105 lines of roughly 229,500 (about 80 from the wait loop, about 25 from
`load_average`): too few to be a crate boundary.

**Consequence for t02 and t04.** `admission`, `coordinator` and `procs` do
not shrink into greengate. The host-scheduling group is relais's own, so it
is a candidate crate on its merits (t02), not a deletion. Two follow-ups are
not yet tickets: add a cancel seam to `greenharness_slots::wait_in_line` (a
greengate change), then switch relais's `verify/heavy.rs` loop to it and
drop `procs::load_average`. Whether the `HeavyLocal` coordinator token path
can collapse into the slot queue depends on what slots-off means, which is
a separate decision.
