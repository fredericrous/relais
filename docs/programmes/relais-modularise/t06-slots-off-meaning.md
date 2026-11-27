+++
type = "grilling"
mode = "hitl"
status = "open"
blocked_by = []
repos = ["relais", "amont"]
+++
# What relais does with heavy local work when host slots are off

## Question

relais's coordinator keeps a `HeavyLocal` token class. With host slots on it
duplicates greengate's slot queue; with slots off (`hostSlots 0`, relais's
`CoordinatorOnly`) it is the only queue heavy local work has. Can the
`HeavyLocal` path collapse into the slot queue, and what should slots-off
mean for relais then: no queue at all, or a queue relais keeps?

## Look at

- relais `crates/relais/src/admission/mod.rs` (`admit`, `admissible`,
  `drain`, the `HeavyLocal` class) and `verify/heavy.rs` (`HostSlotsOff`).
- amont `adr/0014-every-heavy-job-shares-one-queue.md` (`0` turns slots
  off) and `docs/host-slots.md`.
- [t03](t03-slot-overlap.md)'s Resolution, its first table row.

## Answer looks like

One decision: what relais does with heavy local work when slots are off,
and whether the `HeavyLocal` token class is removed. If it is removed, the
lines it takes from `admission`, and whether this lands before the
`admission` crate is cut (t02 row 3).
